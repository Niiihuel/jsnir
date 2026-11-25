//! Turns the host's drawing items into GPU draws, packing text and icon bitmaps into the
//! renderer's persistent texture atlas and caching them between frames without O(N²) eviction.
use crate::renderer::{AtlasAlloc, Draw, Image, Renderer};
use jsnir::{
    editor::EditorBitmapKey,
    host::{Host, ImageData, Item, Rect},
    icons,
    text::{TextBitmap, TextStyle, node_spans},
    tree::{Icon, Span, TextAlign},
};
use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
};

// Exact value key for rich-text spans comparing floats by their bits, avoiding `format!("{spans:?}")`.
#[derive(Clone)]
struct SpanList(Vec<Span>);

impl PartialEq for SpanList {
    fn eq(&self, other: &Self) -> bool {
        if self.0.len() != other.0.len() {
            return false;
        }
        self.0.iter().zip(&other.0).all(|(a, b)| {
            a.text == b.text
                && a.color == b.color
                && a.weight == b.weight
                && a.size.map(f32::to_bits) == b.size.map(f32::to_bits)
                && a.family == b.family
                && a.underline == b.underline
                && a.italic == b.italic
                && a.strike == b.strike
                && a.background == b.background
                && a.background_radius.to_bits() == b.background_radius.to_bits()
                && match (&a.image, &b.image) {
                    (None, None) => true,
                    (Some(x), Some(y)) => {
                        x.source == y.source
                            && x.width.to_bits() == y.width.to_bits()
                            && x.height.to_bits() == y.height.to_bits()
                    }
                    _ => false,
                }
        })
    }
}

impl Eq for SpanList {}

impl Hash for SpanList {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.len().hash(state);
        for s in &self.0 {
            s.text.hash(state);
            s.color.hash(state);
            s.weight.hash(state);
            s.size.map(f32::to_bits).hash(state);
            s.family.hash(state);
            s.underline.hash(state);
            s.italic.hash(state);
            s.strike.hash(state);
            s.background.hash(state);
            s.background_radius.to_bits().hash(state);
            if let Some(img) = &s.image {
                true.hash(state);
                img.source.hash(state);
                img.width.to_bits().hash(state);
                img.height.to_bits().hash(state);
            } else {
                false.hash(state);
            }
        }
    }
}

// Bitmaps depend on everything that changes their pixels; floats are compared by their bits.
#[derive(PartialEq, Eq, Hash)]
struct TextKey {
    text: String,
    spans: Option<SpanList>,
    size: [u32; 2],
    scale: u32,
    wrap: bool,
    style: [u32; 4],
    color: [u8; 4],
    family: Option<String>,
    align: TextAlign,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct IconKey {
    icon: Icon,
    size: u32,
    scale: u32,
    stroke: u32,
    draw: u32,
    color: [u8; 4],
}

struct CacheEntry {
    image: Image,
    last_used: u64,
    // `Some(epoch)` while packed in the renderer's texture atlas; `None` for standalone GPU textures.
    atlas_epoch: Option<u64>,
}

#[derive(Default)]
pub struct View {
    texts: HashMap<TextKey, CacheEntry>,
    icons: HashMap<IconKey, CacheEntry>,
    // App images and masks keyed directly by their generation (`(image, last_used_frame)`).
    images: HashMap<u64, (Image, u64)>,
    // The focused field's text bitmap and the key it was made for; no image while the field is empty.
    editor: Option<(EditorBitmapKey, Option<Image>)>,
    // The host's font epoch when the bitmaps were made.
    font_epoch: u64,
    atlas_epoch: u64,
    atlas_needs_clear: bool,
    standalone_count: usize,
    frame_id: u64,
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
    /// Forgets every cached image, for a new GPU device.
    pub fn reset(&mut self) {
        self.texts.clear();
        self.icons.clear();
        self.images.clear();
        self.editor = None;
        self.atlas_epoch = 0;
        self.atlas_needs_clear = false;
        self.standalone_count = 0;
    }

    // The GPU image of an app image, uploaded the first time a frame uses its generation.
    fn image(&mut self, renderer: &Renderer, image: &ImageData) -> Image {
        let frame_id = self.frame_id;
        if let Some((cached, last_used)) = self.images.get_mut(&image.generation) {
            *last_used = frame_id;
            return cached.clone();
        }
        let size = image.size.map(|side| side as usize);
        let uploaded = renderer.rgba_image(&image.rgba, size);
        self.images
            .insert(image.generation, (uploaded.clone(), frame_id));
        uploaded
    }

    fn mask(&mut self, renderer: &Renderer, mask: Option<(ImageData, Rect)>) -> Option<(Image, Rect)> {
        mask.map(|(image, rect)| (self.image(renderer, &image), rect))
    }

    fn upload_tile(&mut self, renderer: &mut Renderer, bitmap: TextBitmap) -> CacheEntry {
        match renderer.atlas_image(&bitmap) {
            AtlasAlloc::Packed(image) => CacheEntry {
                image,
                last_used: self.frame_id,
                atlas_epoch: Some(renderer.atlas_epoch()),
            },
            AtlasAlloc::Full => {
                // Keep previously emitted draws in this frame intact; reset the atlas at the start of the next frame.
                self.atlas_needs_clear = true;
                self.standalone_count += 1;
                CacheEntry {
                    image: renderer.image(bitmap),
                    last_used: self.frame_id,
                    atlas_epoch: None,
                }
            }
            AtlasAlloc::Oversized => {
                self.standalone_count += 1;
                CacheEntry {
                    image: renderer.image(bitmap),
                    last_used: self.frame_id,
                    atlas_epoch: None,
                }
            }
        }
    }

    pub fn draws(
        &mut self,
        host: &mut Host,
        renderer: &mut Renderer,
        items: Vec<Item>,
        scale: f32,
    ) -> Vec<Draw> {
        let max_side = renderer.device.limits().max_texture_dimension_2d as usize;
        self.frame_id = self.frame_id.wrapping_add(1).max(1);
        let frame_id = self.frame_id;

        if self.font_epoch != host.font_epoch() {
            // The fonts changed: every bitmap of text, and the focused field's, was made with the old ones.
            self.texts.clear();
            self.icons.clear();
            self.editor = None;
            renderer.clear_atlas();
            self.atlas_epoch = renderer.atlas_epoch();
            self.atlas_needs_clear = false;
            self.standalone_count = 0;
            self.font_epoch = host.font_epoch();
        } else if self.atlas_needs_clear {
            renderer.clear_atlas();
            self.texts.retain(|_, e| e.atlas_epoch.is_none());
            self.icons.retain(|_, e| e.atlas_epoch.is_none());
            self.atlas_epoch = renderer.atlas_epoch();
            self.atlas_needs_clear = false;
        } else if self.atlas_epoch != renderer.atlas_epoch() {
            let epoch = renderer.atlas_epoch();
            self.texts
                .retain(|_, e| e.atlas_epoch.is_none_or(|ep| ep == epoch));
            self.icons
                .retain(|_, e| e.atlas_epoch.is_none_or(|ep| ep == epoch));
            self.atlas_epoch = epoch;
        }

        let mut draws = Vec::with_capacity(items.len());
        let mut skip_surface_depth = 0usize;
        for item in items {
            if skip_surface_depth > 0 {
                match item {
                    Item::PushSurface { .. } => skip_surface_depth += 1,
                    Item::PopSurface => {
                        skip_surface_depth -= 1;
                        if skip_surface_depth == 0 {
                            draws.push(Draw::PopSurface);
                        }
                    }
                    _ => {}
                }
                continue;
            }
            match item {
                Item::PushLayer { opacity } => draws.push(Draw::PushLayer { opacity }),
                Item::PopLayer => draws.push(Draw::PopLayer),
                Item::PushSurface {
                    source,
                    dest,
                    radius,
                    opacity,
                    clip,
                    round_clip,
                    additive,
                    cache_key,
                } => {
                    let cached = cache_key.is_some_and(|key| {
                        let surf_px = [
                            (source[2] * scale).ceil().max(1.) as u32,
                            (source[3] * scale).ceil().max(1.) as u32,
                        ];
                        renderer.has_cached_surface(key, surf_px)
                    });
                    draws.push(Draw::PushSurface {
                        source,
                        dest,
                        radius,
                        opacity,
                        clip,
                        round_clip,
                        additive,
                        cache_key,
                    });
                    if cached {
                        skip_surface_depth = 1;
                    }
                }
                Item::PopSurface => draws.push(Draw::PopSurface),
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
                    let mask = self.mask(renderer, mask);
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
                    let mask = self.mask(renderer, mask);
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
                    mut style,
                    wrap,
                    zoom,
                    stretch,
                    clip,
                    round_clip,
                } => {
                    // Rasterized at its logical size and `zoom = min(kx, ky)`; stretched on GPU when `kx != ky`.
                    let stretched =
                        (stretch[0] - 1.).abs() > 1e-4 || (stretch[1] - 1.).abs() > 1e-4;
                    let size = [
                        rect[2] / (zoom * stretch[0]),
                        rect[3] / (zoom * stretch[1]),
                    ];
                    let pixels = scale * zoom;
                    let key = TextKey {
                        text,
                        spans: spans.map(SpanList),
                        size: size.map(f32::to_bits),
                        scale: pixels.to_bits(),
                        wrap,
                        style: style_bits(&style),
                        color: style.color,
                        family: style.family.take(),
                        align: style.align,
                    };
                    let push_text = |draws: &mut Vec<Draw>, img: &Image| {
                        if stretched {
                            let dst = [
                                rect[0],
                                rect[1],
                                (img.size[0] as f32 / scale) * stretch[0],
                                (img.size[1] as f32 / scale) * stretch[1],
                            ];
                            draws.push(
                                Draw::image_in(img, dst, [0.; 4], clip, [255; 4])
                                    .rounded_clip(round_clip),
                            );
                        } else {
                            draws.push(
                                Draw::image(img, snap([rect[0], rect[1]], scale), scale, clip)
                                    .rounded_clip(round_clip),
                            );
                        }
                    };
                    if let Some(entry) = self.texts.get_mut(&key) {
                        entry.last_used = frame_id;
                        push_text(&mut draws, &entry.image);
                    } else {
                        style.family = key.family.clone();
                        let runs = node_spans(
                            &key.text,
                            key.spans.as_ref().map(|spans| spans.0.as_slice()),
                        );
                        let Some(bitmap) =
                            host.raster_text(&runs, size, pixels, wrap, max_side, style)
                        else {
                            continue;
                        };
                        let entry = self.upload_tile(renderer, bitmap);
                        push_text(&mut draws, &entry.image);
                        self.texts.insert(key, entry);
                    }
                }
                Item::Icon {
                    rect,
                    icon,
                    stroke,
                    draw,
                    color,
                    zoom: _,
                    stretch,
                    clip,
                    round_clip,
                } => {
                    let stretched =
                        (stretch[0] - 1.).abs() > 1e-4 || (stretch[1] - 1.).abs() > 1e-4;
                    let base_size = if stretch[0] > 1e-4 {
                        rect[2] / stretch[0]
                    } else {
                        rect[2]
                    };
                    let key = IconKey {
                        icon,
                        size: base_size.to_bits(),
                        scale: scale.to_bits(),
                        stroke: stroke.to_bits(),
                        draw: draw.to_bits(),
                        color,
                    };
                    let push_icon = |draws: &mut Vec<Draw>, img: &Image| {
                        if stretched {
                            draws.push(
                                Draw::image_in(img, rect, [0.; 4], clip, [255; 4])
                                    .rounded_clip(round_clip),
                            );
                        } else {
                            draws.push(
                                Draw::image(img, snap([rect[0], rect[1]], scale), scale, clip)
                                    .rounded_clip(round_clip),
                            );
                        }
                    };
                    if let Some(entry) = self.icons.get_mut(&key) {
                        entry.last_used = frame_id;
                        push_icon(&mut draws, &entry.image);
                    } else {
                        let Some(bitmap) =
                            icons::rasterize(icon, base_size, stroke, draw, color, scale)
                        else {
                            continue;
                        };
                        let entry = self.upload_tile(renderer, bitmap);
                        push_icon(&mut draws, &entry.image);
                        self.icons.insert(key, entry);
                    }
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
                    let texture = self.image(renderer, &image);
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
                    let mask = self.mask(renderer, mask);
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
                    stretch,
                    clip,
                    round_clip,
                } => {
                    let stretched =
                        (stretch[0] - 1.).abs() > 1e-4 || (stretch[1] - 1.).abs() > 1e-4;
                    for [x, y, w, h] in &frame.selection {
                        draws.push(
                            Draw::fill(
                                [
                                    rect[0] + x * stretch[0],
                                    rect[1] + y * stretch[1],
                                    *w * stretch[0],
                                    *h * stretch[1],
                                ],
                                clip,
                                selection,
                            )
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
                        if stretched {
                            let dst = [
                                rect[0],
                                rect[1],
                                (image.size[0] as f32 / scale) * stretch[0],
                                (image.size[1] as f32 / scale) * stretch[1],
                            ];
                            draws.push(
                                Draw::image_in(image, dst, [0.; 4], clip, [255; 4])
                                    .rounded_clip(round_clip),
                            );
                        } else {
                            draws.push(
                                Draw::image(image, snap([rect[0], rect[1]], scale), scale, clip)
                                    .rounded_clip(round_clip),
                            );
                        }
                    }
                    for [x, y, w, h] in &frame.composition {
                        draws.push(
                            Draw::fill(
                                [
                                    rect[0] + x * stretch[0],
                                    rect[1] + y * stretch[1],
                                    *w * stretch[0],
                                    *h * stretch[1],
                                ],
                                clip,
                                caret_color,
                            )
                            .rounded_clip(round_clip),
                        );
                    }
                    if let Some(caret) = caret {
                        draws.push(Draw::fill(caret, clip, caret_color).rounded_clip(round_clip));
                    }
                }
            }
        }

        // O(1) steady-state cleanup: atlas tiles stay cached inside the bounded persistent atlas
        // (avoiding re-rasterization on scroll-back); standalone textures are reclaimed immediately.
        if !self.images.is_empty() {
            self.images.retain(|_, (_, last)| *last == frame_id);
        }
        if self.standalone_count > 0 {
            let mut standalone = 0usize;
            self.texts.retain(|_, e| {
                let keep = e.atlas_epoch.is_some() || e.last_used == frame_id;
                if keep && e.atlas_epoch.is_none() {
                    standalone += 1;
                }
                keep
            });
            self.icons.retain(|_, e| {
                let keep = e.atlas_epoch.is_some() || e.last_used == frame_id;
                if keep && e.atlas_epoch.is_none() {
                    standalone += 1;
                }
                keep
            });
            self.standalone_count = standalone;
        }
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
