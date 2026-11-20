//! Turns the host's drawing items into GPU draws, caching text and icon bitmaps between frames.
use crate::renderer::{Draw, Image, Renderer};
use jsnir::{
    editor::EditorBitmapKey,
    host::{Host, ImageData, Item, Rect},
    icons,
    text::{TextStyle, node_spans},
    tree::TextAlign,
};
use std::collections::HashMap;

// Bitmaps depend on everything that changes their pixels; floats are compared by their bits.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Key {
    Text {
        text: String,
        // The runs of a rich text, as their Debug text: the bitmap depends on each of them.
        spans: Option<String>,
        size: [u32; 2],
        scale: u32,
        wrap: bool,
        style: [u32; 4],
        color: [u8; 4],
        family: Option<String>,
        align: TextAlign,
    },
    Icon {
        icon: jsnir::tree::Icon,
        size: u32,
        scale: u32,
        stroke: u32,
        draw: u32,
        color: [u8; 4],
    },
    // Pixels are replaced by a new generation, so this key is never stale.
    Image {
        generation: u64,
    },
}

#[derive(Default)]
pub struct View {
    images: HashMap<Key, Image>,
    used: Vec<Key>,
    // The focused field's text bitmap and the key it was made for; no image while the field is empty.
    editor: Option<(EditorBitmapKey, Option<Image>)>,
    // The host's font epoch when the bitmaps were made.
    font_epoch: u64,
}

fn style_bits(style: &TextStyle) -> [u32; 4] {
    [
        style.size.to_bits(),
        style.line_height.to_bits(),
        u32::from(style.weight),
        style.letter_spacing.to_bits(),
    ]
}

impl View {
    /// Forgets every image, for a new GPU device.
    pub fn reset(&mut self) {
        self.images.clear();
        self.editor = None;
    }

    // The GPU image of an app image, uploaded the first time a frame uses its generation.
    fn image(&mut self, renderer: &Renderer, image: &ImageData, used: &mut Vec<Key>) -> Image {
        let key = Key::Image {
            generation: image.generation,
        };
        if !self.images.contains_key(&key) {
            let size = image.size.map(|side| side as usize);
            let uploaded = renderer.rgba_image(&image.rgba, size);
            self.images.insert(key.clone(), uploaded);
        }
        let shown = self.images[&key].clone();
        used.push(key);
        shown
    }

    fn mask(
        &mut self,
        renderer: &Renderer,
        mask: Option<(ImageData, Rect)>,
        used: &mut Vec<Key>,
    ) -> Option<(Image, Rect)> {
        mask.map(|(image, rect)| (self.image(renderer, &image, used), rect))
    }

    pub fn draws(
        &mut self,
        host: &mut Host,
        renderer: &Renderer,
        items: Vec<Item>,
        scale: f32,
    ) -> Vec<Draw> {
        let max_side = renderer.device.limits().max_texture_dimension_2d as usize;
        if self.font_epoch != host.font_epoch() {
            // The fonts changed: every bitmap of text, and the focused field's, was made with the old ones.
            self.images.clear();
            self.editor = None;
            self.font_epoch = host.font_epoch();
        }
        let mut used = std::mem::take(&mut self.used);
        used.clear();
        let mut draws = Vec::with_capacity(items.len());
        for item in items {
            match item {
                Item::PushLayer { opacity } => draws.push(Draw::PushLayer { opacity }),
                Item::PopLayer => draws.push(Draw::PopLayer),
                Item::Shadow {
                    rect,
                    radius,
                    shadow,
                    clip,
                    round_clip,
                } => {
                    draws.push(
                        Draw::shadow(
                            rect,
                            radius,
                            shadow.y,
                            shadow.blur,
                            shadow.spread,
                            shadow.color,
                            clip,
                        )
                        .rounded_clip(round_clip),
                    );
                }
                Item::Fill {
                    rect,
                    radius,
                    top,
                    bottom,
                    clip,
                    round_clip,
                    mask,
                } => {
                    let mask = self.mask(renderer, mask, &mut used);
                    draws.push(
                        Draw::gradient(rect, clip, radius, top, bottom)
                            .rounded_clip(round_clip)
                            .masked(mask),
                    );
                }
                Item::Inset {
                    rect,
                    radius,
                    spread,
                    offset,
                    color,
                    clip,
                    round_clip,
                    mask,
                } => {
                    let mask = self.mask(renderer, mask, &mut used);
                    draws.push(
                        Draw::inset(rect, clip, radius, spread, offset, color)
                            .rounded_clip(round_clip)
                            .masked(mask),
                    );
                }
                Item::Text {
                    rect,
                    text,
                    spans,
                    style,
                    wrap,
                    zoom,
                    clip,
                    round_clip,
                } => {
                    // Rasterized at its logical size and the zoomed scale, so it stays sharp while scaled.
                    let size = [rect[2] / zoom, rect[3] / zoom];
                    let pixels = scale * zoom;
                    let key = Key::Text {
                        text,
                        spans: spans.as_ref().map(|spans| format!("{spans:?}")),
                        size: size.map(f32::to_bits),
                        scale: pixels.to_bits(),
                        wrap,
                        style: style_bits(&style),
                        color: style.color,
                        family: style.family.clone(),
                        align: style.align,
                    };
                    if !self.images.contains_key(&key) {
                        let Key::Text { text, .. } = &key else {
                            unreachable!()
                        };
                        let runs = node_spans(text, spans.as_deref());
                        match host.raster_text(&runs, size, pixels, wrap, max_side, style) {
                            Some(bitmap) => {
                                let image = renderer.image(bitmap);
                                self.images.insert(key.clone(), image);
                            }
                            None => continue,
                        }
                    }
                    draws.push(
                        Draw::image(
                            &self.images[&key],
                            snap([rect[0], rect[1]], scale),
                            scale,
                            clip,
                        )
                        .rounded_clip(round_clip),
                    );
                    used.push(key);
                }
                Item::Icon {
                    rect,
                    icon,
                    stroke,
                    draw,
                    color,
                    zoom: _,
                    clip,
                    round_clip,
                } => {
                    let key = Key::Icon {
                        icon,
                        size: rect[2].to_bits(),
                        scale: scale.to_bits(),
                        stroke: stroke.to_bits(),
                        draw: draw.to_bits(),
                        color,
                    };
                    if !self.images.contains_key(&key) {
                        match icons::rasterize(icon, rect[2], stroke, draw, color, scale) {
                            Some(bitmap) => {
                                let image = renderer.image(bitmap);
                                self.images.insert(key.clone(), image);
                            }
                            None => continue,
                        }
                    }
                    draws.push(
                        Draw::image(
                            &self.images[&key],
                            snap([rect[0], rect[1]], scale),
                            scale,
                            clip,
                        )
                        .rounded_clip(round_clip),
                    );
                    used.push(key);
                }
                Item::Image {
                    rect,
                    radius,
                    image,
                    tint,
                    clip,
                    round_clip,
                    mask,
                } => {
                    let texture = self.image(renderer, &image, &mut used);
                    // The mask is snapped with the image, so it keeps the same pixels as the image it masks.
                    let fitted = one_to_one(rect, image.size, scale);
                    let mask = mask.map(|(pixels, mask_rect)| {
                        let mask_rect = if fitted.is_some() {
                            snap_rect(mask_rect, scale)
                        } else {
                            mask_rect
                        };
                        (pixels, mask_rect)
                    });
                    let mask = self.mask(renderer, mask, &mut used);
                    draws.push(
                        Draw::image_in(&texture, fitted.unwrap_or(rect), radius, clip, tint)
                            .rounded_clip(round_clip)
                            .masked(mask),
                    );
                }
                Item::Editor {
                    rect,
                    frame,
                    caret,
                    selection,
                    caret_color,
                    clip,
                    round_clip,
                } => {
                    for [x, y, w, h] in &frame.selection {
                        draws.push(
                            Draw::fill([rect[0] + x, rect[1] + y, *w, *h], clip, selection)
                                .rounded_clip(round_clip),
                        );
                    }
                    let key = frame.bitmap_key;
                    match frame.bitmap {
                        Some(bitmap) => self.editor = Some((key, Some(renderer.image(bitmap)))),
                        None if self.editor.as_ref().is_none_or(|(old, _)| *old != key) => {
                            self.editor = Some((key, None))
                        }
                        None => {}
                    }
                    if let Some((_, Some(image))) = &self.editor {
                        draws.push(
                            Draw::image(image, snap([rect[0], rect[1]], scale), scale, clip)
                                .rounded_clip(round_clip),
                        );
                    }
                    for [x, y, w, h] in &frame.composition {
                        draws.push(
                            Draw::fill([rect[0] + x, rect[1] + y, *w, *h], clip, caret_color)
                                .rounded_clip(round_clip),
                        );
                    }
                    if let Some(caret) = caret {
                        draws.push(Draw::fill(caret, clip, caret_color).rounded_clip(round_clip));
                    }
                }
            }
        }
        // Keep only what this frame used: animations would otherwise pile up one bitmap per step.
        self.images.retain(|key, _| used.contains(key));
        self.used = used;
        draws
    }
}

// Bitmaps are drawn 1:1 with device pixels: a fractional origin would blur them through filtering.
fn snap(point: [f32; 2], scale: f32) -> [f32; 2] {
    point.map(|v| (v * scale).round() / scale)
}

fn snap_rect(rect: Rect, scale: f32) -> Rect {
    rect.map(|v| (v * scale).round() / scale)
}

// An app image whose pixels exactly fill its rectangle is drawn 1:1, snapped like a bitmap; None: it is stretched.
fn one_to_one(rect: Rect, size: [u32; 2], scale: f32) -> Option<Rect> {
    let [x, y, w, h] = rect;
    let fits = [w, h]
        .iter()
        .zip(size)
        .all(|(&side, pixels)| (side * scale).round() == pixels as f32);
    if !fits {
        return None;
    }
    let [x, y] = snap([x, y], scale);
    Some([x, y, size[0] as f32 / scale, size[1] as f32 / scale])
}
