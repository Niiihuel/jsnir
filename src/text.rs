//! Measuring and rasterizing static text in logical points, independent of the backend.
use crate::tree::Font;
use cosmic_text::{
    Attrs, Buffer, Color, FontSystem, Metrics, Shaping, SwashCache, Weight, Wrap, fontdb,
};

pub(crate) const METRICS: Metrics = Metrics::new(14., 19.);
/// CSS `line-height: normal` for Inter: (ascender + descender) / units per em.
pub const NORMAL_LINE_HEIGHT: f32 = 1.21;

#[derive(Default)]
pub struct TextEngine {
    fonts: Option<FontSystem>,
    glyphs: Option<SwashCache>,
    /// Color of unstyled text, such as the line editor's.
    ink: Option<[u8; 3]>,
}

/// Resolved text style, in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextStyle {
    pub size: f32,
    pub line_height: f32,
    pub weight: u16,
    /// In em, as CSS `letter-spacing`.
    pub letter_spacing: f32,
    /// Straight (not premultiplied) RGBA.
    pub color: [u8; 4],
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            size: 14.,
            line_height: 19.,
            weight: 400,
            letter_spacing: 0.,
            color: [215, 225, 231, 255],
        }
    }
}

impl TextStyle {
    /// Style for laying out a node's text: a node without a font keeps the engine's default style.
    pub fn resolve(font: &Font) -> Self {
        if *font == Font::default() {
            Self::default()
        } else {
            Self::of(font, [255; 4])
        }
    }
    /// Style of a `Font` from the tree; sizes default to CSS's 16 px with `line-height: normal`.
    pub fn of(font: &Font, color: [u8; 4]) -> Self {
        let size = font.size.unwrap_or(16.);
        Self {
            size,
            line_height: size * font.line_height.unwrap_or(NORMAL_LINE_HEIGHT),
            weight: font.weight.unwrap_or(400),
            letter_spacing: font.letter_spacing,
            color,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TextBitmap {
    pub size: [usize; 2],
    /// RGBA pixels, left to right, with premultiplied alpha.
    pub rgba: Vec<u8>,
}

impl TextEngine {
    /// An engine that sees only these font files, instead of the system fonts. The first file's family
    /// becomes the default, so unstyled text (the line editor's) uses it too. `ink` colors that text.
    pub fn with_fonts(files: &[&[u8]], ink: [u8; 3]) -> Self {
        let mut db = fontdb::Database::new();
        for file in files {
            db.load_font_data(file.to_vec());
        }
        let family = db
            .faces()
            .next()
            .and_then(|face| face.families.first())
            .map(|f| f.0.clone());
        if let Some(family) = family {
            db.set_sans_serif_family(family);
        }
        Self {
            fonts: Some(FontSystem::new_with_locale_and_db("en-US".into(), db)),
            glyphs: None,
            ink: Some(ink),
        }
    }

    pub(crate) fn fonts(&mut self) -> &mut FontSystem {
        self.fonts.get_or_insert_with(FontSystem::new)
    }

    fn ink(&self) -> Color {
        let [r, g, b] = self.ink.unwrap_or([215, 225, 231]);
        Color::rgb(r, g, b)
    }

    pub(crate) fn shape(&mut self, text: &str, width: Option<f32>, wrap: Wrap) -> Buffer {
        self.shape_styled(text, width, wrap, TextStyle::default())
    }

    fn shape_styled(
        &mut self,
        text: &str,
        width: Option<f32>,
        wrap: Wrap,
        style: TextStyle,
    ) -> Buffer {
        let fonts = self.fonts();
        let mut buffer =
            Buffer::new_empty(Metrics::new(style.size.max(1.), style.line_height.max(1.)));
        buffer.set_wrap(fonts, wrap);
        buffer.set_size(fonts, width, None);
        buffer.set_text(
            fonts,
            text,
            &Attrs::new()
                .weight(Weight(style.weight))
                .letter_spacing(style.letter_spacing),
            Shaping::Advanced,
        );
        buffer
    }

    /// Size of `text` laid out in `width` (one line if `None`).
    pub fn measure_styled(&mut self, text: &str, width: Option<f32>, style: TextStyle) -> [f32; 2] {
        if text.is_empty() || width == Some(0.) {
            return [0.; 2];
        }
        self.shape_styled(text, width, Wrap::WordOrGlyph, style)
            .layout_runs()
            .fold([0.0_f32; 2], |[width, height], run| {
                [
                    width.max(run.line_w),
                    height.max(run.line_top + run.line_height),
                ]
            })
    }

    pub(crate) fn rasterize(
        &mut self,
        text: &str,
        [width, height]: [f32; 2],
        scale: f32,
        wrap: bool,
        max_side: usize,
    ) -> Option<TextBitmap> {
        self.rasterize_styled(
            text,
            [width, height],
            scale,
            wrap,
            max_side,
            TextStyle::default(),
        )
    }

    pub fn rasterize_styled(
        &mut self,
        text: &str,
        [width, height]: [f32; 2],
        scale: f32,
        wrap: bool,
        max_side: usize,
        style: TextStyle,
    ) -> Option<TextBitmap> {
        if text.is_empty()
            || !width.is_finite()
            || !height.is_finite()
            || width <= 0.
            || height <= 0.
            || !scale.is_finite()
            || scale <= 0.
            || max_side == 0
        {
            return None;
        }
        let buffer = if wrap {
            self.shape_styled(text, Some(width), Wrap::WordOrGlyph, style)
        } else {
            self.shape_styled(text, None, Wrap::None, style)
        };
        let width = if wrap {
            width
        } else {
            let line = buffer.layout_runs().next()?;
            // Leave room for glyphs that overhang the advance of the last letter.
            width.min(line.line_w + style.size)
        };
        let [r, g, b, a] = style.color;
        self.rasterize_buffer_colored(
            &buffer,
            [width, height],
            scale,
            max_side,
            0.,
            if wrap { usize::MAX } else { 1 },
            Color::rgba(r, g, b, a),
        )
    }

    pub(crate) fn rasterize_buffer(
        &mut self,
        buffer: &Buffer,
        [width, height]: [f32; 2],
        scale: f32,
        max_side: usize,
        scroll_x: f32,
        lines: usize,
    ) -> Option<TextBitmap> {
        let ink = self.ink();
        self.rasterize_buffer_colored(
            buffer,
            [width, height],
            scale,
            max_side,
            scroll_x,
            lines,
            ink,
        )
    }

    fn rasterize_buffer_colored(
        &mut self,
        buffer: &Buffer,
        [width, height]: [f32; 2],
        scale: f32,
        max_side: usize,
        scroll_x: f32,
        lines: usize,
        color: Color,
    ) -> Option<TextBitmap> {
        let fonts = self.fonts.get_or_insert_with(FontSystem::new);
        let glyphs = self.glyphs.get_or_insert_with(SwashCache::new);
        let pixel_size = |points: f32| ((points * scale).ceil() as usize).min(max_side);
        let (width, height) = (pixel_size(width), pixel_size(height));
        if width == 0 || height == 0 {
            return None;
        }
        let mut rgba = vec![0; width.checked_mul(height)?.checked_mul(4)?];
        for run in buffer
            .layout_runs()
            .take_while(|run| run.line_top * scale < height as f32)
            .take(lines)
        {
            for glyph in run.glyphs {
                if (glyph.x - scroll_x) * scale >= width as f32
                    || (glyph.x + glyph.w + glyph.font_size) < scroll_x
                {
                    continue;
                }
                let color = glyph.color_opt.unwrap_or(color);
                // swash reports only the glyph's coverage in alpha: apply the color's own alpha here.
                let base_alpha = u16::from(color.a());
                let glyph = glyph.physical((-scroll_x * scale, run.line_y * scale), scale);
                glyphs.with_pixels(fonts, glyph.cache_key, color, |x, y, color| {
                    let (x, y) = ((glyph.x + x) as usize, (glyph.y + y) as usize);
                    let alpha = (u16::from(color.a()) * base_alpha + 127) / 255;
                    if x < width && y < height && alpha != 0 {
                        let offset = (y * width + x) * 4;
                        let color = Color::rgba(color.r(), color.g(), color.b(), alpha as u8);
                        blend(&mut rgba[offset..offset + 4], color);
                    }
                });
            }
        }
        Some(TextBitmap {
            size: [width, height],
            rgba,
        })
    }
}

fn blend(pixel: &mut [u8], color: Color) {
    let alpha = u16::from(color.a());
    for (dst, src) in pixel.iter_mut().zip([color.r(), color.g(), color.b(), 255]) {
        *dst = ((u16::from(src) * alpha + u16::from(*dst) * (255 - alpha) + 127) / 255) as u8;
    }
}
