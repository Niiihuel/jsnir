//! Measuring and rasterizing static text in logical points, independent of the backend.
use crate::tree::{Font, Span, TextAlign};
use cosmic_text::{
    Align, Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, Style, SwashCache, Weight,
    Wrap, fontdb,
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
/// Rich multi-span measurement cache key: spans debug repr, optional width bits, shaping fields, and family.
type RichMeasureKey = (String, Option<u32>, [u32; 4], Option<String>);
/// What stands in for an inline image while shaping: its advance is set to the image's width.
const PLACEHOLDER: &str = "\u{FFFC}";
/// Cut texts kept before the cache starts over.
const CUT_CACHE_MAX: usize = 1024;
/// Plain text measurement entries kept before resetting the cache.
const PLAIN_MEASURE_CACHE_MAX: usize = 4096;
/// Maximum shaped `Buffer` instances retained for zero-HarfBuzz re-wrapping during live resize.
const SHAPED_BUFFER_MAX: usize = 512;

/// Interned shaping-relevant fields of a `TextStyle` for plain text measurement caching.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PlainStyleKey {
    size: u32,
    line_height: u32,
    weight: u16,
    letter_spacing: u32,
    family: Option<String>,
}

/// Cached measurement and optional shaped `Buffer` for a single plain `(style, text)` entry.
struct PlainMeasureEntry {
    /// Unwrapped `[width, height]` with `width = None` (one line per `\n`-delimited paragraph).
    natural: [f32; 2],
    /// Most recent wrapped `(width_bits, [width, height])` for instant repeat hits (e.g. Taffy multi-pass).
    last_wrapped: Option<(u32, [f32; 2])>,
    /// Retained shaped `Buffer` so re-wrapping at a new width skips HarfBuzz shaping completely.
    shaped: Option<Buffer>,
}

#[derive(Default)]
pub struct TextEngine {
    fonts: Option<FontSystem>,
    glyphs: Option<SwashCache>,
    /// Color of unstyled text, such as the line editor's.
    ink: Option<[u8; 3]>,
    cuts: HashMap<CutKey, Vec<Span>>,
    /// Marks of rich text, keyed by its cut and alignment: a frame that draws unchanged rich text shapes nothing.
    marks_cache: HashMap<(CutKey, TextAlign), Vec<Mark>>,
    /// Interned plain text styles; index matches `plain_measure`.
    plain_styles: Vec<PlainStyleKey>,
    /// Per-style map of `text -> PlainMeasureEntry`; lookups take `&str` with zero heap allocation.
    plain_measure: Vec<HashMap<String, PlainMeasureEntry>>,
    plain_measure_count: usize,
    shaped_buffer_count: usize,
    /// Measurement cache for multi-span / styled rich text.
    rich_measure_cache: HashMap<RichMeasureKey, [f32; 2]>,
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
    // cosmic-text does not slant upright glyphs: without an italic face the family's upright one is used.
    if span.italic {
        attrs = attrs.style(Style::Italic);
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

/// An inline image, or a line or background of rich text, where it is laid out: logical pixels from the top left.
#[derive(Debug, Clone, PartialEq)]
pub enum Mark {
    /// The image of span `span`: `[x, y, width, height]`.
    Image { span: usize, rect: [f32; 4] },
    /// A line under the glyphs of span `span`: `[x, y, width, thickness]`.
    Underline { span: usize, rect: [f32; 4] },
    /// A line through the middle of the glyphs of span `span`: `[x, y, width, thickness]`.
    Strike { span: usize, rect: [f32; 4] },
    /// The glyphs of span `span` on one line, as `[x, y, width, line height]`.
    Background { span: usize, rect: [f32; 4] },
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

    // Cuts, marks, and measurements are laid out with the fonts they were made with: a font change drops them all.
    fn clear_caches(&mut self) {
        self.cuts.clear();
        self.marks_cache.clear();
        self.plain_styles.clear();
        self.plain_measure.clear();
        self.plain_measure_count = 0;
        self.shaped_buffer_count = 0;
        self.rich_measure_cache.clear();
    }

    fn plain_style_id(&mut self, style: &TextStyle) -> usize {
        let size = style.size.to_bits();
        let line_height = style.line_height.to_bits();
        let weight = style.weight;
        let letter_spacing = style.letter_spacing.to_bits();
        let family = style.family.as_deref();
        if let Some(pos) = self.plain_styles.iter().position(|s| {
            s.size == size
                && s.line_height == line_height
                && s.weight == weight
                && s.letter_spacing == letter_spacing
                && s.family.as_deref() == family
        }) {
            return pos;
        }
        let id = self.plain_styles.len();
        self.plain_styles.push(PlainStyleKey {
            size,
            line_height,
            weight,
            letter_spacing,
            family: style.family.clone(),
        });
        self.plain_measure.push(HashMap::new());
        id
    }

    fn shape_plain_buffer(&mut self, text: &str, width: Option<f32>, style: &TextStyle) -> Buffer {
        #[cfg(test)]
        {
            self.shapes += 1;
        }
        let fonts = self.fonts();
        let mut buffer = Buffer::new_empty(metrics(style));
        buffer.set_wrap(fonts, Wrap::WordOrGlyph);
        buffer.set_size(fonts, width, None);
        buffer.set_text(fonts, text, &attrs(style), Shaping::Advanced);
        buffer.shape_until_scroll(fonts, false);
        buffer
    }

    fn ink(&self) -> Color {
        let [r, g, b] = self.ink.unwrap_or([215, 225, 231]);
        Color::rgb(r, g, b)
    }

    /// `text` in `style` for the line editor's preedit: one line, or wrapped at `width` in a multi-line field.
    pub(crate) fn shape(&mut self, text: &str, style: &TextStyle, width: Option<f32>) -> Buffer {
        let wrap = if width.is_some() {
            Wrap::WordOrGlyph
        } else {
            Wrap::None
        };
        self.shape_styled(&node_spans(text, None), width, wrap, style)
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

    /// Size of plain `text` laid out in `width` (one line per paragraph if `None`).
    /// Cached per `(style, text)`: unwrapped hits return in O(1) with zero allocations, and wrapped resizes
    /// re-wrap the retained shaped `Buffer` without re-running HarfBuzz shaping.
    pub fn measure_plain(
        &mut self,
        text: &str,
        width: Option<f32>,
        style: &TextStyle,
    ) -> [f32; 2] {
        if text.is_empty() || width == Some(0.) {
            return [0.; 2];
        }
        let style_id = self.plain_style_id(style);
        if let Some(entry) = self.plain_measure[style_id].get_mut(text) {
            match width {
                None => return entry.natural,
                Some(w) if !w.is_finite() || w >= entry.natural[0] + 0.001 => return entry.natural,
                Some(w) => {
                    let bits = w.to_bits();
                    if let Some((cached_bits, size)) = entry.last_wrapped
                        && cached_bits == bits
                    {
                        return size;
                    }
                    if let Some(buffer) = entry.shaped.as_mut() {
                        let fonts = self.fonts.get_or_insert_with(FontSystem::new);
                        buffer.set_size(fonts, Some(w), None);
                        buffer.shape_until_scroll(fonts, false);
                        let size = measure_buffer(buffer);
                        entry.last_wrapped = Some((bits, size));
                        return size;
                    }
                }
            }
            let w = width.unwrap();
            let buffer = self.shape_plain_buffer(text, Some(w), style);
            let size = measure_buffer(&buffer);
            if let Some(entry) = self.plain_measure[style_id].get_mut(text) {
                entry.last_wrapped = Some((w.to_bits(), size));
            }
            return size;
        }

        // Cache miss: shape once unwrapped so we know `natural` AND can re-wrap without re-shaping.
        let mut buffer = self.shape_plain_buffer(text, None, style);
        let natural = measure_buffer(&buffer);
        let (result, last_wrapped) = match width {
            Some(w) if w.is_finite() && w < natural[0] + 0.001 => {
                let fonts = self.fonts.get_or_insert_with(FontSystem::new);
                buffer.set_size(fonts, Some(w), None);
                buffer.shape_until_scroll(fonts, false);
                let wrapped = measure_buffer(&buffer);
                (wrapped, Some((w.to_bits(), wrapped)))
            }
            _ => (natural, None),
        };

        if self.plain_measure_count >= PLAIN_MEASURE_CACHE_MAX {
            for map in &mut self.plain_measure {
                map.clear();
            }
            self.plain_measure_count = 0;
            self.shaped_buffer_count = 0;
        }
        let shaped = if self.shaped_buffer_count < SHAPED_BUFFER_MAX {
            self.shaped_buffer_count += 1;
            Some(buffer)
        } else {
            None
        };
        self.plain_measure[style_id].insert(
            text.to_owned(),
            PlainMeasureEntry {
                natural,
                last_wrapped,
                shaped,
            },
        );
        self.plain_measure_count += 1;
        result
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
        if spans.len() == 1 && is_plain_measure_span(&spans[0]) {
            return self.measure_plain(&spans[0].text, width, style);
        }
        let key = rich_measure_key(spans, width, style);
        if let Some(&size) = self.rich_measure_cache.get(&key) {
            return size;
        }
        let buffer = self.shape_styled(spans, width, Wrap::WordOrGlyph, style);
        let size = measure_buffer(&buffer);
        if self.rich_measure_cache.len() >= CUT_CACHE_MAX {
            self.rich_measure_cache.clear();
        }
        self.rich_measure_cache.insert(key, size);
        size
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

    /// Where the inline images, lines and backgrounds of `spans` are when laid out in `width` (one line if `None`).
    /// Backgrounds come first, so they are drawn under the glyphs' lines.
    pub fn marks(&mut self, spans: &[Span], width: Option<f32>, style: &TextStyle) -> Vec<Mark> {
        if blank(spans) {
            return Vec::new();
        }
        let wrap = width.map(|width| width + WRAP_SLACK);
        let buffer = self.shape_styled(spans, wrap, Wrap::WordOrGlyph, style);
        let mut backgrounds = Vec::new();
        let mut marks = Vec::new();
        for run in buffer.layout_runs() {
            // A background span's glyphs on this line are one fragment, from its first glyph to its last.
            let mut fragments: Vec<Mark> = Vec::new();
            for glyph in run.glyphs {
                let Some(span) = spans.get(glyph.metadata) else {
                    continue;
                };
                if span.background.is_some() {
                    match fragments.last_mut() {
                        Some(Mark::Background { span: index, rect })
                            if *index == glyph.metadata =>
                        {
                            rect[2] = glyph.x + glyph.w - rect[0];
                        }
                        _ => fragments.push(Mark::Background {
                            span: glyph.metadata,
                            rect: [glyph.x, run.line_top, glyph.w, run.line_height],
                        }),
                    }
                }
                if let Some(image) = &span.image {
                    let top = run.line_top + (run.line_height - image.height) / 2.;
                    marks.push(Mark::Image {
                        span: glyph.metadata,
                        rect: [glyph.x, top, image.width, image.height],
                    });
                    continue;
                }
                if span.underline {
                    let (offset, thickness) = self.underline(glyph.font_id, glyph.font_size);
                    marks.push(Mark::Underline {
                        span: glyph.metadata,
                        rect: [glyph.x, run.line_y - offset, glyph.w, thickness],
                    });
                }
                if span.strike {
                    let (offset, thickness) = self.strikeout(glyph.font_id, glyph.font_size);
                    marks.push(Mark::Strike {
                        span: glyph.metadata,
                        rect: [glyph.x, run.line_y - offset, glyph.w, thickness],
                    });
                }
            }
            backgrounds.extend(fragments);
        }
        backgrounds.extend(marks);
        backgrounds
    }

    /// The line through the middle of a glyph's font at `size`, as `underline`: its top is `offset` above the baseline
    /// and it is `thickness` tall, from the font's `OS/2` table. A font without one gets a line at 30% of the size.
    fn strikeout(&mut self, font: fontdb::ID, size: f32) -> (f32, f32) {
        let metrics = self
            .fonts()
            .get_font(font)
            .map(|f| f.as_swash().metrics(&[]).scale(size));
        match metrics {
            Some(m) => (m.strikeout_offset, m.stroke_size.max(1.)),
            None => (size * 0.3, 1.),
        }
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
        size: [f32; 2],
        scale: f32,
        wrap: bool,
        max_side: usize,
        style: &TextStyle,
    ) -> Option<TextBitmap> {
        self.rasterize_styled_impl(spans, size, scale, wrap, max_side, style, false)
    }

    /// Like [`Self::rasterize_styled`], trimming unused transparent space to the right of the
    /// rightmost glyph even when `wrap` is true (preserving left/center/end glyph x-offsets).
    pub fn rasterize_styled_tight(
        &mut self,
        spans: &[Span],
        size: [f32; 2],
        scale: f32,
        wrap: bool,
        max_side: usize,
        style: &TextStyle,
    ) -> Option<TextBitmap> {
        self.rasterize_styled_impl(spans, size, scale, wrap, max_side, style, true)
    }

    fn rasterize_styled_impl(
        &mut self,
        spans: &[Span],
        [width, height]: [f32; 2],
        scale: f32,
        wrap: bool,
        max_side: usize,
        style: &TextStyle,
        trim_wrapped_right: bool,
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
            if trim_wrapped_right {
                let max_right = buffer
                    .layout_runs()
                    .take_while(|run| run.line_top < height)
                    .map(|run| {
                        let glyph_right = run
                            .glyphs
                            .iter()
                            .map(|g| g.x + g.w + g.font_size)
                            .fold(0.0_f32, f32::max);
                        // For Start alignment `line_w` is already from x=0; for Center/End
                        // `glyph.x` carries the alignment offset so `glyph_right` bounds the ink.
                        if style.align == TextAlign::Start {
                            glyph_right.max(run.line_w + style.size)
                        } else {
                            glyph_right
                        }
                    })
                    .fold(0.0_f32, f32::max);
                if max_right <= 0. {
                    return None;
                }
                width.min(max_right)
            } else {
                width
            }
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
            [0.; 2],
            if wrap { usize::MAX } else { 1 },
            Color::rgba(r, g, b, a),
        )
    }

    /// Draws `buffer` from `scroll` (logical pixels: left, top) into a bitmap of `size`, at most `lines` runs.
    pub(crate) fn rasterize_buffer(
        &mut self,
        buffer: &Buffer,
        [width, height]: [f32; 2],
        scale: f32,
        max_side: usize,
        scroll: [f32; 2],
        lines: usize,
    ) -> Option<TextBitmap> {
        let ink = self.ink();
        self.rasterize_buffer_colored(buffer, [width, height], scale, max_side, scroll, lines, ink)
    }

    fn rasterize_buffer_colored(
        &mut self,
        buffer: &Buffer,
        [width, height]: [f32; 2],
        scale: f32,
        max_side: usize,
        [scroll_x, scroll_y]: [f32; 2],
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
            .filter(|run| run.line_top + run.line_height > scroll_y)
            .take_while(|run| (run.line_top - scroll_y) * scale < height as f32)
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
                let glyph =
                    glyph.physical((-scroll_x * scale, (run.line_y - scroll_y) * scale), scale);
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

fn measure_buffer(buffer: &Buffer) -> [f32; 2] {
    buffer
        .layout_runs()
        .fold([0.0_f32; 2], |[width, height], run| {
            [
                width.max(run.line_w),
                height.max(run.line_top + run.line_height),
            ]
        })
}

/// Whether `span` has no shaping-relevant overrides (size, weight, family, italic, inline image).
fn is_plain_measure_span(span: &Span) -> bool {
    span.image.is_none()
        && span.size.is_none()
        && span.weight.is_none()
        && span.family.is_none()
        && !span.italic
}

fn rich_measure_key(spans: &[Span], width: Option<f32>, style: &TextStyle) -> RichMeasureKey {
    (
        format!("{spans:?}"),
        width.map(f32::to_bits),
        [
            style.size.to_bits(),
            style.line_height.to_bits(),
            u32::from(style.weight),
            style.letter_spacing.to_bits(),
        ],
        style.family.clone(),
    )
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

    #[test]
    fn strikes_and_backgrounds_are_marked_once_and_then_shape_nothing() {
        let mut engine = engine();
        let style = TextStyle::default();
        let spans = [
            Span {
                strike: true,
                ..run("a")
            },
            Span {
                background: Some(0x2040_80ff),
                background_radius: 4.,
                ..run("b")
            },
        ];
        let marks = engine.marks(&spans, None, &style);
        assert!(
            marks
                .iter()
                .any(|m| matches!(m, Mark::Strike { span: 0, rect } if rect[3] >= 1.)),
            "{marks:?}"
        );
        assert!(
            matches!(marks[0], Mark::Background { span: 1, .. }),
            "backgrounds come first: {marks:?}"
        );
        let first = engine.cut_marks(&spans, 300., None, &style);
        engine.shapes = 0;
        let second = engine.cut_marks(&spans, 300., None, &style);
        assert_eq!(first, second);
        assert_eq!(engine.shapes, 0, "unchanged marks shape nothing");
    }

    #[test]
    fn a_background_wrapped_onto_two_lines_is_two_fragments() {
        let mut engine = engine();
        let style = TextStyle::default();
        let word = engine.measure_styled(&[run("xxxxxxxx")], None, &style)[0];
        let spans = [Span {
            background: Some(0xff0000ff),
            background_radius: 2.,
            ..run("xxxxxxxx yyyyyyyy")
        }];
        let marks = engine.marks(&spans, Some(word * 1.5), &style);
        let lines: Vec<[f32; 4]> = marks
            .iter()
            .filter_map(|m| match m {
                Mark::Background { span: 0, rect } => Some(*rect),
                _ => None,
            })
            .collect();
        assert_eq!(lines.len(), 2, "{marks:?}");
        assert!(lines[0][1] < lines[1][1], "{lines:?}");
        assert!(lines.iter().all(|r| r[2] > 0. && r[3] > 0.), "{lines:?}");
    }

    #[test]
    fn italic_picks_the_italic_face_of_a_family_that_has_one() {
        // The upright face is used when the family has no italic one: cosmic-text does not slant glyphs.
        let (Ok(regular), Ok(oblique)) = (
            std::fs::read("/usr/share/fonts/TTF/DejaVuSans.ttf"),
            std::fs::read("/usr/share/fonts/TTF/DejaVuSans-Oblique.ttf"),
        ) else {
            eprintln!("skipped: DejaVu Sans with its oblique face is not installed");
            return;
        };
        let mut engine =
            TextEngine::with_fonts(&[regular.as_slice(), oblique.as_slice()], [255; 3]);
        let family = Some("DejaVu Sans".to_owned());
        let spans = [
            Span {
                family: family.clone(),
                ..run("H")
            },
            Span {
                family,
                italic: true,
                ..run("H")
            },
        ];
        let buffer = engine.shape_styled(&spans, None, Wrap::None, &TextStyle::default());
        let glyphs = buffer.layout_runs().next().expect("a line").glyphs;
        let face = |span: usize| {
            glyphs
                .iter()
                .find(|g| g.metadata == span)
                .map(|g| g.font_id)
        };
        assert_ne!(face(0), face(1), "the italic run takes the oblique face");
    }

    #[test]
    fn measure_plain_matches_uncached_shaping_and_rewraps_without_reshaping() {
        let mut engine = engine();
        let style = TextStyle::default();
        let samples = [
            "Hello world",
            "Hello world with trailing spaces   ",
            "First line\nSecond longer line here\nThird",
            "Message payload 42 testing steady layout, scroll clipping, and hover walk.",
            "Supercalifragilisticexpialidocious",
        ];
        for text in samples {
            let natural_ref = measure_buffer(&engine.shape_styled(
                &[run(text)],
                None,
                Wrap::WordOrGlyph,
                &style,
            ));
            engine.shapes = 0;
            let got_nat = engine.measure_plain(text, None, &style);
            assert_eq!(got_nat, natural_ref, "natural mismatch for {text:?}");
            assert_eq!(engine.shapes, 1, "first measurement shapes once");

            // Unwrapped widths >= natural + slack hit O(1) natural cache with 0 shapes.
            for extra in [WRAP_SLACK, 10., 200., 800.] {
                let w = natural_ref[0] + extra;
                let expected = measure_buffer(&engine.shape_styled(
                    &[run(text)],
                    Some(w),
                    Wrap::WordOrGlyph,
                    &style,
                ));
                let before = engine.shapes;
                let got = engine.measure_plain(text, Some(w), &style);
                assert_eq!(got, expected, "unwrapped mismatch for {text:?} at {w}");
                assert_eq!(engine.shapes, before, "unwrapped width shapes nothing");
            }

            // Narrower widths re-wrap the retained Buffer with 0 additional HarfBuzz shapes!
            for frac in [0.85_f32, 0.55, 0.30, 0.15] {
                let w = (natural_ref[0] * frac).max(1.) + WRAP_SLACK;
                let expected = measure_buffer(&engine.shape_styled(
                    &[run(text)],
                    Some(w),
                    Wrap::WordOrGlyph,
                    &style,
                ));
                let before = engine.shapes;
                let got = engine.measure_plain(text, Some(w), &style);
                assert_eq!(got, expected, "wrapped mismatch for {text:?} at {w}");
                assert_eq!(engine.shapes, before, "re-wrapping retained buffer shapes nothing");
            }
        }
    }
}
