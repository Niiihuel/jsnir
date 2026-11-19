//! Single-line editor shared by the backends. Internal indices are UTF-8 byte offsets.
use crate::text::{METRICS, TextBitmap, TextEngine};
use cosmic_text::{
    Action, Affinity, Attrs, AttrsList, Buffer, BufferLine, BufferRef, Change, Cursor, Edit,
    Editor, LayoutGlyph, LayoutRun, LineEnding, Motion, Selection, Shaping, Wrap,
};
use std::{borrow::Cow, ops::Range};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug)]
pub enum EditorEvent {
    Insert(String),
    Move {
        direction: Direction,
        extend: bool,
    },
    Delete {
        backward: bool,
        word: bool,
    },
    Point {
        x: f32,
        select: PointSelection,
    },
    SelectAll,
    Copy,
    Cut,
    Undo,
    Redo,
    Preedit {
        text: String,
        active: Option<Range<usize>>,
    },
    Commit(String),
    DeleteSurrounding {
        before: usize,
        after: usize,
    },
    Cancel,
    Enter,
}

impl EditorEvent {
    pub(crate) fn involves_ime(&self) -> bool {
        matches!(
            self,
            Self::Preedit { .. } | Self::Commit(_) | Self::DeleteSurrounding { .. }
        )
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Direction {
    Left,
    Right,
    WordLeft,
    WordRight,
    Start,
    End,
}

#[derive(Clone, Copy, Debug)]
pub enum PointSelection {
    Place,
    Extend,
    Word,
    All,
}

#[derive(Default)]
pub struct EditorOutput {
    pub clipboard: Option<String>,
    pub interrupt_ime: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EditorBitmapKey {
    revision: u64,
    size: [f32; 2],
    scale: f32,
    max_side: usize,
    scroll_x: f32,
}

#[derive(Debug, Clone)]
pub struct EditorFrame {
    pub bitmap_key: EditorBitmapKey,
    pub bitmap: Option<TextBitmap>,
    pub caret_x: f32,
    pub caret_visible: bool,
    pub line_height: f32,
    pub selection: Vec<Range<f32>>,
    pub composition: Vec<Range<f32>>,
}

#[derive(Clone, Copy)]
struct Caret {
    cursor: Cursor,
    selection: Selection,
}

struct EditRecord {
    change: Change,
    before: Caret,
    after: Caret,
}

struct Preedit {
    text: String,
    active: Option<Range<usize>>,
}

pub struct LineEditor {
    inner: Editor<'static>,
    preedit: Option<Preedit>,
    preview: Option<Buffer>,
    undo: Vec<EditRecord>,
    redo: Vec<EditRecord>,
    scroll_x: f32,
    revision: u64,
}

impl Default for LineEditor {
    fn default() -> Self {
        Self {
            inner: empty_editor(""),
            preedit: None,
            preview: None,
            undo: Vec::new(),
            redo: Vec::new(),
            scroll_x: 0.,
            revision: 0,
        }
    }
}

impl LineEditor {
    fn buffer(&self) -> &Buffer {
        let BufferRef::Owned(buffer) = self.inner.buffer_ref() else {
            unreachable!()
        };
        buffer
    }

    pub fn text(&self) -> &str {
        self.buffer().lines[0].text()
    }

    pub fn selection(&self) -> [u32; 2] {
        let range = self.selected_bytes();
        let text = self.text();
        let start = text[..range.start].chars().count();
        [start as u32, (start + text[range].chars().count()) as u32]
    }

    pub fn reset(&mut self, text: &str, [anchor, head]: [u32; 2]) {
        let text = single_line(text);
        self.inner = empty_editor(&text);
        let anchor = Cursor::new(0, byte_at(&text, anchor as usize));
        let head = Cursor::new(0, byte_at(&text, head as usize));
        self.inner.set_cursor(head);
        if anchor != head {
            self.inner.set_selection(Selection::Normal(anchor));
        }
        self.preedit = None;
        self.scroll_x = 0.;
        self.undo.clear();
        self.redo.clear();
        self.invalidate();
    }

    pub fn composing(&self) -> bool {
        self.preedit.is_some()
    }

    fn selected_bytes(&self) -> Range<usize> {
        self.inner.selection_bounds().map_or_else(
            || {
                let i = self.inner.cursor().index;
                i..i
            },
            |(a, b)| a.index..b.index,
        )
    }

    fn invalidate(&mut self) {
        self.preview = None;
        self.revision = self.revision.wrapping_add(1);
    }

    fn caret(&self) -> Caret {
        Caret {
            cursor: self.inner.cursor(),
            selection: self.inner.selection(),
        }
    }

    fn change(&mut self, edit: impl FnOnce(&mut Editor<'static>)) {
        let before = self.caret();
        self.inner.start_change();
        edit(&mut self.inner);
        if let Some(change) = self
            .inner
            .finish_change()
            .filter(|change| !change.items.is_empty())
        {
            const UNDO_LIMIT: usize = 128;
            if self.undo.len() == UNDO_LIMIT {
                self.undo.remove(0);
            }
            self.undo.push(EditRecord {
                change,
                before,
                after: self.caret(),
            });
            self.redo.clear();
            self.invalidate();
        }
    }

    fn restore(&mut self, redo: bool) {
        let record = if redo {
            self.redo.pop()
        } else {
            self.undo.pop()
        };
        if let Some(mut record) = record {
            if !redo {
                record.change.reverse();
            }
            self.inner.apply_change(&record.change);
            let caret = if redo { record.after } else { record.before };
            self.inner.set_cursor(caret.cursor);
            self.inner.set_selection(caret.selection);
            if redo {
                self.undo.push(record);
            } else {
                record.change.reverse();
                self.redo.push(record);
            }
            self.invalidate();
        }
    }

    fn insert(&mut self, text: &str) {
        let text = single_line(text);
        if !text.is_empty() {
            self.change(|editor| {
                editor.insert_string(&text, None);
                let cursor = editor.cursor();
                let index = editor.with_buffer(|buffer| {
                    grapheme_edge(buffer.lines[0].text(), cursor.index, true)
                });
                editor.set_cursor(Cursor::new_with_affinity(0, index, Affinity::Before));
            });
        }
    }

    fn move_cursor(&mut self, engine: &mut TextEngine, direction: Direction, extend: bool) {
        let motion = match direction {
            Direction::Left => Motion::Left,
            Direction::Right => Motion::Right,
            Direction::WordLeft => Motion::LeftWord,
            Direction::WordRight => Motion::RightWord,
            Direction::Start => Motion::ParagraphStart,
            Direction::End => Motion::ParagraphEnd,
        };

        if extend && self.inner.selection() == Selection::None {
            self.inner
                .set_selection(Selection::Normal(self.inner.cursor()));
        }
        if !extend {
            let range = self.selected_bytes();
            self.inner.set_selection(Selection::None);
            if !range.is_empty() && matches!(direction, Direction::Left | Direction::Right) {
                self.inner.shape_as_needed(engine.fonts(), false);
                let start = Cursor::new_with_affinity(0, range.start, Affinity::After);
                let end = Cursor::new_with_affinity(0, range.end, Affinity::Before);
                let toward_left = matches!(direction, Direction::Left);
                let start_on_left = caret_x(self.buffer(), start) <= caret_x(self.buffer(), end);
                self.inner.set_cursor(if toward_left == start_on_left {
                    start
                } else {
                    end
                });
                return;
            }
        }
        if matches!(direction, Direction::Left | Direction::Right) {
            self.inner.shape_as_needed(engine.fonts(), false);
            if let Some(run) = self.buffer().layout_runs().next()
                && run
                    .glyphs
                    .iter()
                    .any(|glyph| glyph.level.is_rtl() != run.rtl)
            {
                let left = matches!(direction, Direction::Left);
                if let Some(cursor) =
                    visual_neighbor(&run, caret_x(self.buffer(), self.inner.cursor()), left)
                {
                    self.inner.set_cursor(cursor);
                }
                return;
            }
        }
        self.inner.action(engine.fonts(), Action::Motion(motion));
    }

    fn delete(&mut self, backward: bool, word: bool) {
        if !self.selected_bytes().is_empty() {
            self.change(|editor| {
                editor.delete_selection();
            });
            return;
        }
        let index = self.inner.cursor().index;
        let text = self.text();
        let boundary = match (backward, word) {
            (true, true) => text
                .unicode_word_indices()
                .rev()
                .find(|(i, _)| *i < index)
                .map_or(0, |(i, _)| i),
            (false, true) => text
                .unicode_word_indices()
                .map(|(i, word)| i + word.len())
                .find(|&end| end > index)
                .unwrap_or(text.len()),
            (true, false) => text
                .grapheme_indices(true)
                .take_while(|(i, _)| *i < index)
                .last()
                .map_or(0, |(i, _)| i),
            (false, false) => text
                .grapheme_indices(true)
                .map(|(i, g)| i + g.len())
                .find(|&end| end > index)
                .unwrap_or(text.len()),
        };
        let start = grapheme_edge(text, index.min(boundary), false);
        let end = grapheme_edge(text, index.max(boundary), true);
        self.replace_with_empty(start..end);
    }

    fn replace_with_empty(&mut self, bytes: Range<usize>) {
        if bytes.is_empty() {
            return;
        }
        self.change(|editor| {
            let start = Cursor::new(0, bytes.start);
            editor.delete_range(start, Cursor::new(0, bytes.end));
            editor.set_cursor(start);
            editor.set_selection(Selection::None);
        });
    }

    fn delete_surrounding(&mut self, before: usize, after: usize) {
        let selected = self.selected_bytes();
        let text = self.text();
        let start = before.checked_sub(1).map_or(selected.start, |n| {
            text[..selected.start]
                .char_indices()
                .rev()
                .nth(n)
                .map_or(0, |(i, _)| i)
        });
        let end = selected.end + byte_at(&text[selected.end..], after);
        let removed_before = selected.start - start;
        if removed_before == 0 && selected.end == end {
            return;
        }

        let caret = self.caret();
        let backwards = match caret.selection {
            Selection::Normal(anchor) | Selection::Line(anchor) | Selection::Word(anchor) => {
                caret.cursor.index < anchor.index
            }
            Selection::None => false,
        };
        let mut head = Cursor::new(0, selected.end - removed_before);
        let mut anchor = Cursor::new(0, start);
        if backwards {
            std::mem::swap(&mut head, &mut anchor);
        }
        self.change(|editor| {
            if selected.end < end {
                editor.delete_range(Cursor::new(0, selected.end), Cursor::new(0, end));
            }
            if start < selected.start {
                editor.delete_range(Cursor::new(0, start), Cursor::new(0, selected.start));
            }
            editor.set_cursor(head);
            editor.set_selection(if head == anchor {
                Selection::None
            } else {
                Selection::Normal(anchor)
            });
        });
    }

    pub fn handle(&mut self, engine: &mut TextEngine, event: &EditorEvent) -> EditorOutput {
        let mut output = EditorOutput::default();
        match event {
            EditorEvent::Preedit { text, active } => {
                let text = single_line(text);
                self.preedit = (!text.is_empty()).then(|| Preedit {
                    text: text.into_owned(),
                    active: active.clone(),
                });
                self.invalidate();
            }
            EditorEvent::Commit(text) => {
                if self.preedit.take().is_some() {
                    self.invalidate();
                }
                self.insert(text);
            }
            EditorEvent::DeleteSurrounding { before, after } => {
                self.delete_surrounding(*before, *after);
            }
            EditorEvent::Cancel | EditorEvent::Point { .. } => {
                if self.preedit.take().is_some() {
                    self.invalidate();
                    output.interrupt_ime = true;
                }
                if let EditorEvent::Point { x, select } = *event {
                    self.inner.shape_as_needed(engine.fonts(), false);
                    let x = x + self.scroll_x;
                    if let Some(mut cursor) = self.buffer().hit(x, METRICS.line_height / 2.) {
                        cursor.index = grapheme_edge(
                            self.text(),
                            cursor.index,
                            cursor.affinity == Affinity::Before,
                        );
                        let selection = match select {
                            PointSelection::Place => Selection::None,
                            PointSelection::Extend => match self.inner.selection() {
                                Selection::None => Selection::Normal(self.inner.cursor()),
                                selection => selection,
                            },
                            PointSelection::Word => Selection::Word(cursor),
                            PointSelection::All => Selection::Line(cursor),
                        };
                        self.inner.set_cursor(cursor);
                        self.inner.set_selection(selection);
                    }
                } else if !output.interrupt_ime {
                    self.inner.set_selection(Selection::None);
                }
            }
            _ if self.composing() => {}
            EditorEvent::Insert(text) => self.insert(text),
            EditorEvent::Move { direction, extend } => {
                self.move_cursor(engine, *direction, *extend)
            }
            EditorEvent::Delete { backward, word } => self.delete(*backward, *word),
            EditorEvent::SelectAll => {
                self.inner
                    .set_selection(Selection::Normal(Cursor::new(0, 0)));
                self.inner.set_cursor(Cursor::new(0, self.text().len()));
            }
            EditorEvent::Copy | EditorEvent::Cut => {
                let range = self.selected_bytes();
                if !range.is_empty() {
                    output.clipboard = Some(self.text()[range.clone()].to_owned());
                    if matches!(event, EditorEvent::Cut) {
                        self.replace_with_empty(range);
                    }
                }
            }
            EditorEvent::Undo => self.restore(false),
            EditorEvent::Redo => self.restore(true),
            EditorEvent::Enter => {}
        }
        output
    }

    pub fn frame(
        &mut self,
        engine: &mut TextEngine,
        size: [f32; 2],
        scale: f32,
        max_side: usize,
        cached: Option<EditorBitmapKey>,
    ) -> EditorFrame {
        let width = size[0].max(0.);
        let (cursor, selected, composition) = if let Some(preedit) = &self.preedit {
            let replace = self.selected_bytes();
            let start = replace.start;
            let end = start + preedit.text.len();
            if self.preview.is_none() {
                let mut display = self.text().to_owned();
                display.replace_range(replace, &preedit.text);
                self.preview = Some(engine.shape(&display, None, Wrap::None));
            }
            let selected = preedit
                .active
                .as_ref()
                .map(|range| {
                    start + byte_at(&preedit.text, range.start)
                        ..start + byte_at(&preedit.text, range.end)
                })
                .unwrap_or(end..end);
            (
                Cursor::new_with_affinity(0, selected.end, Affinity::Before),
                selected,
                start..end,
            )
        } else {
            if !self.text().is_empty() {
                self.inner.shape_as_needed(engine.fonts(), false);
            }
            (self.inner.cursor(), self.selected_bytes(), 0..0)
        };
        let buffer = self.preview.as_ref().unwrap_or_else(|| self.buffer());
        let caret = caret_x(buffer, cursor);
        let run = buffer.layout_runs().next();
        let line_width = run.as_ref().map_or(0., |run| run.line_w);
        let max_scroll = (line_width + 2. - width).max(0.);
        let left = self.scroll_x.clamp(0., max_scroll);
        let scroll_x = left.min(caret).max(caret + 2. - width).max(0.);
        let bitmap_key = EditorBitmapKey {
            revision: self.revision,
            size,
            scale,
            max_side,
            scroll_x,
        };
        let bitmap = if Some(bitmap_key) != cached
            && run.as_ref().is_some_and(|run| !run.glyphs.is_empty())
        {
            engine.rasterize_buffer(buffer, size, scale, max_side, scroll_x, 1)
        } else {
            None
        };
        let selection = run
            .as_ref()
            .map_or_else(Vec::new, |run| spans(run, selected, scroll_x, width));
        let composition = run
            .as_ref()
            .map_or_else(Vec::new, |run| spans(run, composition, scroll_x, width));
        self.scroll_x = scroll_x;
        EditorFrame {
            bitmap_key,
            bitmap,
            caret_x: caret - scroll_x,
            caret_visible: self
                .preedit
                .as_ref()
                .is_none_or(|preedit| preedit.active.is_some()),
            line_height: METRICS.line_height,
            selection,
            composition,
        }
    }
}

fn empty_editor(text: &str) -> Editor<'static> {
    let mut buffer = Buffer::new_empty(METRICS);
    buffer.lines.push(BufferLine::new(
        text,
        LineEnding::None,
        AttrsList::new(&Attrs::new()),
        Shaping::Advanced,
    ));
    Editor::new(buffer)
}

fn single_line(text: &str) -> Cow<'_, str> {
    if !text
        .chars()
        .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
    {
        return Cow::Borrowed(text);
    }
    Cow::Owned(
        text.replace("\r\n", "\n")
            .chars()
            .filter_map(|c| match c {
                '\r' | '\n' | '\t' | '\u{000B}' | '\u{000C}' | '\u{0085}' | '\u{2028}'
                | '\u{2029}' => Some(' '),
                c if c.is_control() => None,
                c => Some(c),
            })
            .collect(),
    )
}

fn byte_at(text: &str, index: usize) -> usize {
    text.char_indices()
        .nth(index)
        .map_or(text.len(), |(offset, _)| offset)
}

fn grapheme_edge(text: &str, index: usize, forward: bool) -> usize {
    if index == 0 || index == text.len() {
        return index;
    }
    text.grapheme_indices(true)
        .take_while(|(start, _)| *start < index)
        .find(|(start, cluster)| index < *start + cluster.len())
        .map_or(index, |(start, cluster)| {
            if forward {
                start + cluster.len()
            } else {
                start
            }
        })
}

fn visual_neighbor(run: &LayoutRun<'_>, x: f32, left: bool) -> Option<Cursor> {
    let mut nearest = None::<(f32, Cursor)>;
    let boundaries = run
        .text
        .grapheme_indices(true)
        .map(|(i, _)| i)
        .chain([run.text.len()])
        .collect::<Vec<_>>();
    for glyph in run.glyphs {
        let first = boundaries.partition_point(|&index| index < glyph.start);
        for &index in boundaries[first..]
            .iter()
            .take_while(|&&index| index <= glyph.end)
        {
            let at = cluster_x(run.text, glyph, index);
            let distance = if left { x - at } else { at - x };
            if distance > 0.01 && nearest.is_none_or(|(best, _)| distance < best) {
                let affinity = if index == glyph.end {
                    Affinity::Before
                } else {
                    Affinity::After
                };
                nearest = Some((distance, Cursor::new_with_affinity(0, index, affinity)));
            }
        }
    }
    nearest.map(|(_, cursor)| cursor)
}

fn cluster_x(text: &str, glyph: &LayoutGlyph, byte: usize) -> f32 {
    let cluster = &text[glyph.start..glyph.end];
    let total = cluster.graphemes(true).count().max(1);
    let offset = cluster
        .grapheme_indices(true)
        .take_while(|(i, _)| glyph.start + i < byte)
        .count();
    let t = offset as f32 / total as f32;
    glyph.x + glyph.w * if glyph.level.is_rtl() { 1. - t } else { t }
}

fn caret_x(buffer: &Buffer, cursor: Cursor) -> f32 {
    let Some(run) = buffer.layout_runs().next() else {
        return 0.;
    };
    let mut alternative = 0.;
    for glyph in run.glyphs {
        if cursor.index < glyph.start || cursor.index > glyph.end {
            continue;
        }
        let x = cluster_x(run.text, glyph, cursor.index);
        if (cursor.index > glyph.start && cursor.index < glyph.end)
            || (cursor.index == glyph.start && cursor.affinity == Affinity::After)
            || (cursor.index == glyph.end && cursor.affinity == Affinity::Before)
        {
            return x;
        }
        alternative = x;
    }
    alternative
}

fn spans(run: &LayoutRun<'_>, bytes: Range<usize>, scroll_x: f32, width: f32) -> Vec<Range<f32>> {
    if bytes.is_empty() || width <= 0. {
        return Vec::new();
    }
    let mut segments = Vec::<Range<f32>>::new();
    for glyph in run.glyphs {
        let start = bytes.start.max(glyph.start);
        let end = bytes.end.min(glyph.end);
        if start < end && glyph.x < scroll_x + width && glyph.x + glyph.w > scroll_x {
            let a = cluster_x(run.text, glyph, start) - scroll_x;
            let b = cluster_x(run.text, glyph, end) - scroll_x;
            segments.push(a.min(b).max(0.)..a.max(b).min(width));
        }
    }
    segments.sort_unstable_by(|a, b| a.start.total_cmp(&b.start));
    let mut merged = Vec::<Range<f32>>::new();
    for span in segments {
        if let Some(last) = merged.last_mut().filter(|last| span.start <= last.end) {
            last.end = last.end.max(span.end);
        } else {
            merged.push(span);
        }
    }
    merged
}
