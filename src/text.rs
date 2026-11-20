//! Measuring and rasterizing static text in logical points, independent of the backend.
use crate::tree::{Font, Span, TextAlign};
use cosmic_text::{
    Align, Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache, Weight, Wrap,
    fontdb,
};
use std::{borrow::Cow, collections::HashMap};

/// Width added when wrapping text in the width it was measured at, so float rounding never pushes its
/// last glyph to a new line. Layout and rasterization must use the same slack.
pub(crate) const WRAP_SLACK: f32 = 0.01;
/// CSS `line-height: normal` for Inter: (ascender + descender) / units per em.
pub const NORMAL_LINE_HEIGHT: f32 = 1.21;
/// Size of text without a `font.size`, in logical pixels: what is drawn, and CSS's default.
pub const DEFAULT_FONT_SIZE: f32 = 16.;

/// Spans (as their Debug text), width, lines, the shaping fields and the family of a style, as `limit_lines`
/// and `cut_marks` key their caches.
type CutKey = (String, u32, Option<u16>, [u32; 4], Option<String>);
/// What stands in for an inline image while shaping: its advance is set to the image's width.
const PLACEHOLDER: &str = "\u{FFFC}";
/// Cut texts kept before the cache starts over.
const CUT_CACHE_MAX: usize = 1024;

#[derive(Default)]
pub struct TextEngine {
    fonts: Option<FontSystem>,
    glyphs: Option<SwashCache>,
    /// Color of unstyled text, such as the line editor's.
    ink: Option<[u8; 3]>,
    cuts: HashMap<CutKey, Vec<Span>>,
    /// Marks of rich text, keyed by its cut and alignment: a frame that draws unchanged rich text shapes nothing.
    marks_cache: HashMap<(CutKey, TextAlign), Vec<Mark>>,
    /// Shaping calls so far, which tests count.
    #[cfg(test)]
    pub(crate) shapes: usize,
}

/// Resolved text style, in logical pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct TextStyle {
    pub size: f32,
    pub line_height: f32,
    pub weight: u16,
    /// In em, as CSS `letter-spacing`.
    pub letter_spacing: f32,
    /// Straight (not premultiplied) RGBA.
    pub color: [u8; 4],
    /// Font family; None: the engine's default family.
    pub family: Option<String>,
    /// Alignment of each line inside the width the text is laid out in.
    pub align: TextAlign,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            size: DEFAULT_FONT_SIZE,
            line_height: 19.,
            weight: 400,
            letter_spacing: 0.,
            color: [215, 225, 231, 255],
            family: None,
            align: TextAlign::Start,
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
        let size = font.size.unwrap_or(DEFAULT_FONT_SIZE);
        Self {
            size,
            line_height: size * font.line_height.unwrap_or(NORMAL_LINE_HEIGHT),
            weight: font.weight.unwrap_or(400),
            letter_spacing: font.letter_spacing,
            color,
            family: font.family.clone(),
            align: TextAlign::Start,
        }
    }
}

/// The spans a text node draws: its `spans` when it has some, else its plain `text` as one span.
pub fn node_spans<'a>(text: &str, spans: Option<&'a [Span]>) -> Cow<'a, [Span]> {
    match spans.filter(|spans| !spans.is_empty()) {
        Some(spans) => Cow::Borrowed(spans),
        None => Cow::Owned(vec![Span {
            text: text.to_owned(),
            ..Span::default()
        }]),
    }
}

/// The plain text of spans, as `Node::Text.text` holds it: images have none.
pub fn plain_text(spans: &[Span]) -> String {
    spans
        .iter()
        .filter(|span| span.image.is_none())
        .map(|span| span.text.as_str())
        .collect()
}

/// Whether spans draw nothing: no text and no image.
fn blank(spans: &[Span]) -> bool {
    spans
        .iter()
        .all(|span| span.image.is_none() && span.text.is_empty())
}

/// Attributes of unstyled text in `style`'s family, weight and letter spacing.
pub(crate) fn attrs(style: &TextStyle) -> Attrs<'_> {
    Attrs::new()
        .family(
            style
                .family
                .as_deref()
                .map_or(Family::SansSerif, Family::Name),
        )
        .weight(Weight(style.weight))
        .letter_spacing(style.letter_spacing)
}

/// Metrics of text in `style`, with sizes kept positive for cosmic-text.
pub(crate) fn metrics(style: &TextStyle) -> Metrics {
    Metrics::new(style.size.max(1.), style.line_height.max(1.))
}

/// Attributes of one run: its style, its color (transparent for an image, so it draws nothing), its
/// metrics when it sets a size, and its index as metadata, which marks its glyphs.
fn run_attrs<'a>(run: &'a TextStyle, span: &Span, index: usize) -> Attrs<'a> {
    let mut attrs = attrs(run).metadata(index);
    let color = if span.image.is_some() {
        Some([0; 4])
    } else {
        span.color.map(u32::to_be_bytes)
    };
    if let Some([r, g, b, a]) = color {
        attrs = attrs.color(Color::rgba(r, g, b, a));
    }
    if span.size.is_some() {
        attrs = attrs.metrics(metrics(run));
    }
    attrs
}

/// The spans with "…" after them: in the style of the last run, or of `first` when none is left.
fn with_ellipsis(mut spans: Vec<Span>, first: &Span) -> Vec<Span> {
    if let Some(last) = spans.last_mut().filter(|last| last.image.is_none()) {
        last.text.push('…');
    } else {
        let template = spans.last().unwrap_or(first).clone();
        spans.push(Span {
            text: "…".to_owned(),
            image: None,
            ..template
        });
    }
    spans
}

/// The spans up to byte `end` of their shaped text, without the whitespace that ends them. An image is
/// kept whole or not at all.
fn cut_spans(spans: &[Span], end: usize) -> Vec<Span> {
    let mut out = Vec::new();
    let mut offset = 0;
    for span in spans {
        let len = if span.image.is_some() {
            PLACEHOLDER.len()
        } else {
            span.text.len()
        };
        if offset + len > end {
            if span.image.is_none() && offset < end {
                out.push(Span {
                    text: span.text[..end - offset].to_owned(),
                    ..span.clone()
                });
            }
            break;
        }
        out.push(span.clone());
        offset += len;
    }
    while let Some(last) = out.last_mut().filter(|last| last.image.is_none()) {
        let kept = last.text.trim_end().len();
        last.text.truncate(kept);
        if !last.text.is_empty() {
            break;
        }
        out.pop();
    }
    out
}

/// An inline image or an underline of rich text, where it is laid out: logical pixels from the top left.
#[derive(Debug, Clone, PartialEq)]
pub enum Mark {
    /// The image of span `span`: `[x, y, width, height]`.
    Image { span: usize, rect: [f32; 4] },
    /// A 1 px line under the glyphs of span `span`: `[x, y, width, 1]`.
    Underline { span: usize, rect: [f32; 4] },
}

/// cosmic-text's alignment of a line; None keeps the paragraph's own (start) alignment.
fn align(align: TextAlign) -> Option<Align> {
    match align {
        TextAlign::Start => None,
        TextAlign::Center => Some(Align::Center),
        TextAlign::End => Some(Align::End),
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
            ..Self::default()
        }
    }

    pub(crate) fn fonts(&mut self) -> &mut FontSystem {
        self.fonts.get_or_insert_with(FontSystem::new)
    }

    /// Adds a TTF or OTF font file to the database. Callers drop what they measured with the old fonts.
    pub(crate) fn load_font(&mut self, bytes: Vec<u8>) {
        self.fonts().db_mut().load_font_data(bytes);
        self.clear_caches();
    }

    /// Sets the family of text without one: the generic sans-serif family the default attributes name.
    pub(crate) fn set_default_family(&mut self, family: String) {
        self.fonts().db_mut().set_sans_serif_family(family);
        self.clear_caches();
    }

    // Cuts and marks are laid out with the fonts they were made with: a font change drops them all.
    fn clear_caches(&mut self) {
        self.cuts.clear();
        self.marks_cache.clear();
    }

    fn ink(&self) -> Color {
        let [r, g, b] = self.ink.unwrap_or([215, 225, 231]);
        Color::rgb(r, g, b)
    }

    /// `text` on one line in `style`, for the line editor's preedit.
    pub(crate) fn shape(&mut self, text: &str, style: &TextStyle) -> Buffer {
        self.shape_styled(&node_spans(text, None), None, Wrap::None, style)
    }

    /// Width of `text` on one line in `style`: the advance of the placeholder of an image.
    fn line_width(&mut self, text: &str, style: &TextStyle) -> f32 {
        let span = Span {
            text: text.to_owned(),
            ..Span::default()
        };
        self.shape_styled(&[span], None, Wrap::None, style)
            .layout_runs()
            .next()
            .map_or(0., |run| run.line_w)
    }

    fn shape_styled(
        &mut self,
        spans: &[Span],
        width: Option<f32>,
        wrap: Wrap,
        style: &TextStyle,
    ) -> Buffer {
        #[cfg(test)]
        {
            self.shapes += 1;
        }
        // Each run's style: its size, weight and family, or the node's. An image run is a placeholder
        // whose advance is the image's width, so its letter spacing makes up the difference.
        let mut runs = Vec::with_capacity(spans.len());
        for span in spans {
            let mut run = TextStyle {
                size: span.size.unwrap_or(style.size),
                line_height: span.size.map_or(style.line_height, |size| {
                    size * style.line_height / style.size
                }),
                weight: span.weight.unwrap_or(style.weight),
                family: span.family.clone().or_else(|| style.family.clone()),
                ..style.clone()
            };
            let text = match &span.image {
                Some(image) => {
                    let natural = TextStyle {
                        letter_spacing: 0.,
                        ..run.clone()
                    };
                    let advance = self.line_width(PLACEHOLDER, &natural);
                    run.letter_spacing = (image.width - advance) / run.size.max(1.);
                    PLACEHOLDER
                }
                None => span.text.as_str(),
            };
            runs.push((span, text, run));
        }
        let fonts = self.fonts();
        let mut buffer = Buffer::new_empty(metrics(style));
        buffer.set_wrap(fonts, wrap);
        buffer.set_size(fonts, width, None);
        buffer.set_rich_text(
            fonts,
            runs.iter()
                .enumerate()
                .map(|(i, (span, text, run))| (*text, run_attrs(run, span, i))),
            &attrs(style),
            Shaping::Advanced,
            None,
        );
        let align = align(style.align);
        for line in &mut buffer.lines {
            line.set_align(align);
        }
        // Setting the alignment drops the layout of the lines it changed: lay them out again.
        buffer.shape_until_scroll(fonts, false);
        buffer
    }

    /// Size of `spans` laid out in `width` (one line if `None`).
    pub fn measure_styled(
        &mut self,
        spans: &[Span],
        width: Option<f32>,
        style: &TextStyle,
    ) -> [f32; 2] {
        if blank(spans) || width == Some(0.) {
            return [0.; 2];
        }
        self.shape_styled(spans, width, Wrap::WordOrGlyph, style)
            .layout_runs()
            .fold([0.0_f32; 2], |[width, height], run| {
                [
                    width.max(run.line_w),
                    height.max(run.line_top + run.line_height),
                ]
            })
    }

    /// `spans` cut to at most `lines` lines when laid out at `width`. If they wrap onto more, the last line
    /// ends in "…" and fits the width. Layout and drawing both cut with this, so they agree.
    pub fn limit_lines(
        &mut self,
        spans: &[Span],
        width: f32,
        lines: u16,
        style: &TextStyle,
    ) -> Vec<Span> {
        if blank(spans) || !(width.is_finite() && width > 0.) {
            return spans.to_vec();
        }
        let key = cut_key(spans, width, Some(lines), style);
        if let Some(cut) = self.cuts.get(&key) {
            return cut.clone();
        }
        let cut = self.cut(spans, width, usize::from(lines.max(1)), style);
        // shortcut: clears the whole cache when full, not LRU; upgrade if streamed text keeps it thrashing.
        if self.cuts.len() >= CUT_CACHE_MAX {
            self.cuts.clear();
        }
        self.cuts.insert(key, cut.clone());
        cut
    }

    /// `spans` cut as `limit_lines` cuts them (all of them if `lines` is None), and where their marks are at
    /// `width`. Both come from the caches when the same text was laid out before.
    pub fn cut_marks(
        &mut self,
        spans: &[Span],
        width: f32,
        lines: Option<u16>,
        style: &TextStyle,
    ) -> (Vec<Span>, Vec<Mark>) {
        let cut = match lines {
            Some(lines) => self.limit_lines(spans, width, lines, style),
            None => spans.to_vec(),
        };
        let key = (cut_key(spans, width, lines, style), style.align);
        if let Some(marks) = self.marks_cache.get(&key) {
            return (cut, marks.clone());
        }
        let marks = self.marks(&cut, Some(width), style);
        if self.marks_cache.len() >= CUT_CACHE_MAX {
            self.marks_cache.clear();
        }
        self.marks_cache.insert(key, marks.clone());
        (cut, marks)
    }

    fn cut(&mut self, spans: &[Span], width: f32, lines: usize, style: &TextStyle) -> Vec<Span> {
        let wrap = Some(width + WRAP_SLACK);
        let buffer = self.shape_styled(spans, wrap, Wrap::WordOrGlyph, style);
        let mut end = 0;
        let mut count = 0;
        for run in buffer.layout_runs() {
            count += 1;
            if count <= lines
                && let Some(glyph) = run.glyphs.last()
            {
                // Glyph offsets count from the start of their paragraph, not of the whole text.
                let start: usize = buffer.lines[..run.line_i]
                    .iter()
                    .map(|l| l.text().len() + l.ending().as_str().len())
                    .sum();
                end = start + glyph.end;
            }
        }
        if count <= lines {
            return spans.to_vec();
        }
        // Offsets count the shaped text: each span's text, and one placeholder per image.
        let shaped: String = spans
            .iter()
            .map(|span| match span.image {
                Some(_) => PLACEHOLDER,
                None => span.text.as_str(),
            })
            .collect();
        let mut cut = end;
        loop {
            let candidate = with_ellipsis(cut_spans(spans, cut), &spans[0]);
            let runs = self
                .shape_styled(&candidate, wrap, Wrap::WordOrGlyph, style)
                .layout_runs()
                .count();
            if runs <= lines || cut == 0 {
                return candidate;
            }
            cut = shaped[..cut].char_indices().last().map_or(0, |(i, _)| i);
        }
    }

    /// Where the inline images and underlines of `spans` are when laid out in `width` (one line if `None`).
    pub fn marks(&mut self, spans: &[Span], width: Option<f32>, style: &TextStyle) -> Vec<Mark> {
        if blank(spans) {
            return Vec::new();
        }
        let wrap = width.map(|width| width + WRAP_SLACK);
        let buffer = self.shape_styled(spans, wrap, Wrap::WordOrGlyph, style);
        let mut marks = Vec::new();
        for run in buffer.layout_runs() {
            for glyph in run.glyphs {
                let Some(span) = spans.get(glyph.metadata) else {
                    continue;
                };
                if let Some(image) = &span.image {
                    let top = run.line_top + (run.line_height - image.height) / 2.;
                    marks.push(Mark::Image {
                        span: glyph.metadata,
                        rect: [glyph.x, top, image.width, image.height],
                    });
                } else if span.underline {
                    let (offset, thickness) = self.underline(glyph.font_id, glyph.font_size);
                    marks.push(Mark::Underline {
                        span: glyph.metadata,
                        rect: [glyph.x, run.line_y - offset, glyph.w, thickness],
                    });
                }
            }
        }
        marks
    }

    /// The underline of a glyph's font at `size`: the top of the stroke is `offset` above the baseline (negative
    /// below it), and it is `thickness` tall. Both come from the font's `post` table. The thickness is at least
    /// 1 px, as browsers draw it: Inter's stroke is 0.4 px at 16 px. A font without a table gets 2 px below.
    fn underline(&mut self, font: fontdb::ID, size: f32) -> (f32, f32) {
        let metrics = self
            .fonts()
            .get_font(font)
            .map(|f| f.as_swash().metrics(&[]).scale(size));
        match metrics {
            Some(m) => (m.underline_offset, m.stroke_size.max(1.)),
            None => (-2., 1.),
        }
    }

    #[cfg(feature = "examples")]
    pub(crate) fn rasterize(
        &mut self,
        text: &str,
        [width, height]: [f32; 2],
        scale: f32,
        wrap: bool,
        max_side: usize,
    ) -> Option<TextBitmap> {
        self.rasterize_styled(
            &node_spans(text, None),
            [width, height],
            scale,
            wrap,
            max_side,
            &TextStyle::default(),
        )
    }

    pub fn rasterize_styled(
        &mut self,
        spans: &[Span],
        [width, height]: [f32; 2],
        scale: f32,
        wrap: bool,
        max_side: usize,
        style: &TextStyle,
    ) -> Option<TextBitmap> {
        if blank(spans)
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
            self.shape_styled(spans, Some(width + WRAP_SLACK), Wrap::WordOrGlyph, style)
        } else {
            self.shape_styled(spans, None, Wrap::None, style)
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

/// The key of a cut of `spans` at `width` and `lines`, with the style fields that shaping reads.
fn cut_key(spans: &[Span], width: f32, lines: Option<u16>, style: &TextStyle) -> CutKey {
    (
        format!("{spans:?}"),
        width.to_bits(),
        lines,
        [
            style.size.to_bits(),
            style.line_height.to_bits(),
            u32::from(style.weight),
            style.letter_spacing.to_bits(),
        ],
        style.family.clone(),
    )
}

fn blend(pixel: &mut [u8], color: Color) {
    let alpha = u16::from(color.a());
    for (dst, src) in pixel.iter_mut().zip([color.r(), color.g(), color.b(), 255]) {
        *dst = ((u16::from(src) * alpha + u16::from(*dst) * (255 - alpha) + 127) / 255) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::InlineImage;

    const REGULAR: &[u8] = include_bytes!("../assets/fonts/Inter-Regular.ttf");
    const SEMIBOLD: &[u8] = include_bytes!("../assets/fonts/Inter-SemiBold.ttf");

    fn engine() -> TextEngine {
        TextEngine::with_fonts(&[REGULAR, SEMIBOLD], [255; 3])
    }

    fn run(text: &str) -> Span {
        Span {
            text: text.into(),
            ..Span::default()
        }
    }

    #[test]
    fn each_span_keeps_its_color_weight_and_underline() {
        let mut engine = engine();
        let style = TextStyle::default();
        let spans = [
            Span {
                color: Some(0xff0000ff),
                ..run("a")
            },
            Span {
                weight: Some(600),
                ..run("b")
            },
            Span {
                underline: true,
                ..run("c")
            },
        ];
        let buffer = engine.shape_styled(&spans, None, Wrap::None, &style);
        let line = buffer.layout_runs().next().expect("a line");
        let glyphs = line.glyphs;
        assert_eq!(
            glyphs.iter().map(|g| g.metadata).collect::<Vec<_>>(),
            [0, 1, 2]
        );
        assert_eq!(glyphs[0].color_opt, Some(Color::rgba(255, 0, 0, 255)));
        assert_eq!(glyphs[1].color_opt, None, "unset colors take the node's");
        let regular = engine.measure_styled(&[run("b")], None, &style)[0];
        let bold = engine.measure_styled(
            &[Span {
                weight: Some(600),
                ..run("b")
            }],
            None,
            &style,
        )[0];
        assert!(bold > regular, "{bold} vs {regular}");
        // The underline is the font's own: below the baseline, as thick as its stroke.
        let font = engine
            .fonts()
            .get_font(glyphs[2].font_id)
            .expect("the font");
        let post = font.as_swash().metrics(&[]).scale(glyphs[2].font_size);
        assert!(post.underline_offset < 0. && post.stroke_size > 0.);
        assert_eq!(
            engine.marks(&spans, None, &style),
            [Mark::Underline {
                span: 2,
                rect: [
                    glyphs[2].x,
                    line.line_y - post.underline_offset,
                    glyphs[2].w,
                    post.stroke_size.max(1.)
                ],
            }]
        );
    }

    #[test]
    fn an_inline_image_takes_its_width_and_marks_its_place() {
        let mut engine = engine();
        let style = TextStyle::default();
        let image = InlineImage {
            source: "tag".into(),
            width: 20.,
            height: 20.,
        };
        let spans = [
            run("x"),
            Span {
                image: Some(image),
                ..Span::default()
            },
            run("y"),
        ];
        let x = engine.measure_styled(&[run("x")], None, &style)[0];
        let y = engine.measure_styled(&[run("y")], None, &style)[0];
        let total = engine.measure_styled(&spans, None, &style)[0];
        assert!(
            (total - (x + 20. + y)).abs() <= 1.,
            "{total} vs {}",
            x + 20. + y
        );
        let Mark::Image { span, rect } = engine.marks(&spans, None, &style)[0].clone() else {
            panic!("the image mark")
        };
        assert_eq!(span, 1);
        assert!((rect[0] - x).abs() <= 1., "{rect:?} after {x}");
        assert_eq!(rect[2..], [20., 20.]);
        assert!(
            (rect[1] - (style.line_height - 20.) / 2.).abs() < 0.01,
            "{rect:?}"
        );
    }

    #[test]
    fn spans_cut_to_one_line_end_in_an_ellipsis() {
        let mut engine = engine();
        let style = TextStyle::default();
        let spans = [
            run("The quick "),
            Span {
                weight: Some(600),
                ..run("brown")
            },
            run(" fox jumps over the lazy dog"),
        ];
        let cut = engine.limit_lines(&spans, 60., 1, &style);
        assert!(
            cut.last().is_some_and(|last| last.text.ends_with('…')),
            "{cut:?}"
        );
        assert!(cut[0].text.starts_with("The"), "{cut:?}");
        let [_, height] = engine.measure_styled(&cut, Some(60. + WRAP_SLACK), &style);
        assert!((height - style.line_height).abs() < 0.01, "{height}");
    }
}
