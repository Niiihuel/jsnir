//! Turns the host's drawing items into GPU draws, caching text and icon bitmaps between frames.
use crate::renderer::{Draw, Image, Renderer};
use jsnir::{
    editor::EditorBitmapKey,
    host::{Host, Item},
    icons,
    text::TextStyle,
};
use std::collections::HashMap;

// Bitmaps depend on everything that changes their pixels; floats are compared by their bits.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Key {
    Text {
        text: String,
        size: [u32; 2],
        scale: u32,
        wrap: bool,
        style: [u32; 4],
        color: [u8; 4],
    },
    Icon {
        icon: jsnir::tree::Icon,
        size: u32,
        scale: u32,
        stroke: u32,
        draw: u32,
        color: [u8; 4],
    },
}

#[derive(Default)]
pub struct View {
    images: HashMap<Key, Image>,
    used: Vec<Key>,
    // The focused field's text bitmap and the key it was made for; no image while the field is empty.
    editor: Option<(EditorBitmapKey, Option<Image>)>,
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

    pub fn draws(
        &mut self,
        host: &mut Host,
        renderer: &Renderer,
        items: Vec<Item>,
        scale: f32,
    ) -> Vec<Draw> {
        let max_side = renderer.device.limits().max_texture_dimension_2d as usize;
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
                } => {
                    draws.push(Draw::shadow(
                        rect,
                        radius,
                        shadow.y,
                        shadow.blur,
                        shadow.spread,
                        shadow.color,
                        clip,
                    ));
                }
                Item::Fill {
                    rect,
                    radius,
                    top,
                    bottom,
                    clip,
                } => draws.push(Draw::gradient(rect, clip, radius, top, bottom)),
                Item::Inset {
                    rect,
                    radius,
                    spread,
                    offset,
                    color,
                    clip,
                } => {
                    draws.push(Draw::inset(rect, clip, radius, spread, offset, color));
                }
                Item::Text {
                    rect,
                    text,
                    style,
                    wrap,
                    zoom,
                    clip,
                } => {
                    // Rasterized at its logical size and the zoomed scale, so it stays sharp while scaled.
                    let size = [rect[2] / zoom, rect[3] / zoom];
                    let pixels = scale * zoom;
                    let key = Key::Text {
                        text,
                        size: size.map(f32::to_bits),
                        scale: pixels.to_bits(),
                        wrap,
                        style: style_bits(&style),
                        color: style.color,
                    };
                    if !self.images.contains_key(&key) {
                        let Key::Text { text, .. } = &key else {
                            unreachable!()
                        };
                        match host.raster_text(text, size, pixels, wrap, max_side, style) {
                            Some(bitmap) => {
                                let image = renderer.image(bitmap);
                                self.images.insert(key.clone(), image);
                            }
                            None => continue,
                        }
                    }
                    draws.push(Draw::image(
                        &self.images[&key],
                        [rect[0], rect[1]],
                        scale,
                        clip,
                    ));
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
                    draws.push(Draw::image(
                        &self.images[&key],
                        [rect[0], rect[1]],
                        scale,
                        clip,
                    ));
                    used.push(key);
                }
                Item::Editor {
                    rect,
                    frame,
                    caret,
                    selection,
                    caret_color,
                    clip,
                } => {
                    for span in &frame.selection {
                        draws.push(Draw::fill(
                            [
                                rect[0] + span.start,
                                rect[1],
                                span.end - span.start,
                                rect[3],
                            ],
                            clip,
                            selection,
                        ));
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
                        draws.push(Draw::image(image, [rect[0], rect[1]], scale, clip));
                    }
                    for span in &frame.composition {
                        draws.push(Draw::fill(
                            [
                                rect[0] + span.start,
                                rect[1] + rect[3] - 1.,
                                span.end - span.start,
                                1.,
                            ],
                            clip,
                            caret_color,
                        ));
                    }
                    if let Some(caret) = caret {
                        draws.push(Draw::fill(caret, clip, caret_color));
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
