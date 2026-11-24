//! Line editor shared by the backends: one line, or several lines wrapped at the field's width. Internal indices
//! are UTF-8 byte offsets; in a text with several lines each line break counts as one byte, `\n`.
use crate::text::{TextBitmap, TextEngine, TextStyle, WRAP_SLACK, attrs, metrics};
use cosmic_text::{
    Action, Affinity, AttrsList, Buffer, BufferLine, BufferRef, Change, Cursor, Edit, Editor,
    FontSystem, LayoutGlyph, LayoutRun, LineEnding, Motion, Selection, Shaping,
};
use std::{borrow::Cow, ops::Range};
use unicode_segmentation::UnicodeSegmentation;

/// Window of text surrounding the caret/selection for cross-platform IME protocols:
/// - Linux Wayland `zwp_text_input_v3` consumes `text` (<= 4000 UTF-8 bytes), `cursor_byte`, and `anchor_byte`.
/// - Windows TSF and macOS `NSTextInputClient` consume `cursor_utf16` and `anchor_utf16`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SurroundingText {
    pub text: String,
    /// Caret (head) offset inside `text`, in UTF-8 bytes.
    pub cursor_byte: usize,
    /// Selection anchor offset inside `text`, in UTF-8 bytes.
    pub anchor_byte: usize,
    /// Caret (head) offset inside `text`, in UTF-16 code units.
    pub cursor_utf16: usize,
    /// Selection anchor offset inside `text`, in UTF-16 code units.
    pub anchor_utf16: usize,
    /// Start of `text` within the full field text, in UTF-8 bytes.
    pub window_byte_offset: usize,
}

/// Visual style of a preedit clause during CJK / dead-key composition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreeditStyle {
    /// Unconverted / inactive clause: 1 px underline.
    Underline,
    /// Target / active conversion clause: 2 px thick underline.
    ThickUnderline,
    /// Highlighted range within the preedit string.
    Selection,
}

/// One styled range within a preedit string. `range` uses character indices in the preedit text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreeditSegment {
    pub range: Range<usize>,
    pub style: PreeditStyle,
}

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
    /// A press or drag at `x` and `y`, where `y` is counted from the top of the text (its scroll included).
    Point {
        x: f32,
        y: f32,
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
    /// Multi-clause preedit (Japanese / Chinese / Korean IMEs with active conversion segments).
    PreeditSegments {
        text: String,
        segments: Vec<PreeditSegment>,
        cursor: Option<usize>,
    },
    Commit(String),
    /// Deletes `before` and `after` Unicode scalar values around the selection.
    DeleteSurrounding {
        before: usize,
        after: usize,
    },
    /// Deletes `before_bytes` and `after_bytes` UTF-8 bytes around the selection (Wayland `zwp_text_input_v3`).
    /// Unaligned byte lengths are clamped safely to UTF-8 character boundaries.
    DeleteSurroundingBytes {
        before_bytes: usize,
        after_bytes: usize,
    },
    Cancel,
    /// Enter: the host submits a field with it, or breaks the line of a multi-line field that does not submit.
    Enter,
    /// Shift+Enter: breaks the line of a multi-line field. Nothing in a single-line one.
    LineBreak,
}

impl EditorEvent {
    pub(crate) fn involves_ime(&self) -> bool {
        matches!(
            self,
            Self::Preedit { .. }
                | Self::PreeditSegments { .. }
                | Self::Commit(_)
                | Self::DeleteSurrounding { .. }
                | Self::DeleteSurroundingBytes { .. }
        )
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Direction {
    Left,
    Right,
    WordLeft,
    WordRight,
    /// The start and the end of the visual line (Home and End).
    Start,
    End,
    /// The start and the end of the text (Ctrl+Home and Ctrl+End).
    TextStart,
    TextEnd,
    Up,
    Down,
    /// Up or down by the height of the field.
    PageUp,
    PageDown,
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
    scroll_y: f32,
}

#[derive(Debug, Clone)]
pub struct EditorFrame {
    pub bitmap_key: EditorBitmapKey,
    pub bitmap: Option<TextBitmap>,
    pub caret_x: f32,
    /// Top of the caret's line, below the scroll.
    pub caret_y: f32,
    pub caret_visible: bool,
    pub line_height: f32,
    /// Selected rectangles `[x, y, width, height]`, below the scroll.
    pub selection: Vec<[f32; 4]>,
    /// Preedit underlines `[x, y, width, 1]`, below the scroll.
    pub composition: Vec<[f32; 4]>,
    /// The vertical scroll the frame shows: the one asked for, kept within the text and with the caret in view.
    pub scroll_y: f32,
    /// Height of the whole text laid out at the field's width.
    pub content_height: f32,
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
    segments: Vec<PreeditSegment>,
    cursor: Option<usize>,
}

pub struct LineEditor {
    inner: Editor<'static>,
    // Style the text is shaped in: the field's font.
    style: TextStyle,
    multiline: bool,
    // Password fields mask displayed glyphs with '•', block Copy/Cut, and suppress IME surrounding text.
    password: bool,
    // Height of the visible text, which Page Up and Page Down move by.
    page: f32,
    // Set by each event: the next frame keeps the caret in view.
    follow: bool,
    preedit: Option<Preedit>,
    preview: Option<Buffer>,
    undo: Vec<EditRecord>,
    redo: Vec<EditRecord>,
    scroll_x: f32,
    // Vertical scroll of a multi-line field, as the field asks for it: the next frame may move it to keep the caret in view.
    scroll_y: f32,
    revision: u64,
}

impl Default for LineEditor {
    fn default() -> Self {
        Self {
            inner: empty_editor("", &TextStyle::default()),
            style: TextStyle::default(),
            multiline: false,
            password: false,
            page: 0.,
            follow: false,
            preedit: None,
            preview: None,
            undo: Vec::new(),
            redo: Vec::new(),
            scroll_x: 0.,
            scroll_y: 0.,
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

    fn buffer_mut(&mut self) -> &mut Buffer {
        let BufferRef::Owned(buffer) = self.inner.buffer_ref_mut() else {
            unreachable!()
        };
        buffer
    }

    /// Shapes the text again: the fonts changed, so the glyphs and the bitmap are stale.
    pub fn fonts_changed(&mut self) {
        for line in &mut self.buffer_mut().lines {
            line.reset();
        }
        self.invalidate();
    }

    // Shapes the text in `style`, which the field's font names.
    fn set_style(&mut self, fonts: &mut FontSystem, style: &TextStyle) {
        self.style = style.clone();
        let list = AttrsList::new(&attrs(style));
        let buffer = self.buffer_mut();
        buffer.set_metrics(fonts, metrics(style));
        for line in &mut buffer.lines {
            line.set_attrs_list(list.clone());
        }
        self.fonts_changed();
    }

    /// Wraps the text at `width`, or leaves it on one line (None). The field sets it before it handles events, so
    /// motions see the lines the field shows.
    pub fn set_width(&mut self, engine: &mut TextEngine, width: Option<f32>) {
        self.set_wrap(engine.fonts(), width);
    }

    // Wraps the text at `width`, or leaves it on one line when None. A change lays the text out again.
    fn set_wrap(&mut self, fonts: &mut FontSystem, width: Option<f32>) {
        if self.buffer().size().0 != width {
            self.buffer_mut().set_size(fonts, width, None);
            self.invalidate();
        }
    }

    /// The text, its lines joined by `\n`.
    pub fn text(&self) -> Cow<'_, str> {
        match self.buffer().lines.as_slice() {
            [line] => Cow::Borrowed(line.text()),
            lines => Cow::Owned(
                lines
                    .iter()
                    .map(|line| line.text())
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
        }
    }

    pub fn selection(&self) -> [u32; 2] {
        let range = self.selected_bytes();
        let text = self.text();
        let start = text[..range.start].chars().count();
        [start as u32, (start + text[range].chars().count()) as u32]
    }

    /// Directed selection `[anchor, head]` in character offsets (preserves backwards selections for AccessKit).
    pub fn directed_selection(&self) -> [u32; 2] {
        let selected = self.selected_bytes();
        let caret = self.caret();
        let buffer = self.buffer();
        let backwards = match caret.selection {
            Selection::Normal(anchor) | Selection::Line(anchor) | Selection::Word(anchor) => {
                offset_of(buffer, caret.cursor) < offset_of(buffer, anchor)
            }
            Selection::None => false,
        };
        let text = self.text();
        let start = text[..selected.start].chars().count() as u32;
        let end = start + text[selected].chars().count() as u32;
        if backwards { [end, start] } else { [start, end] }
    }

    /// Sets the caret or selection to `[anchor, head]` in character offsets, cancelling any active preedit.
    pub fn set_selection(&mut self, [anchor, head]: [u32; 2]) {
        if self.preedit.is_some() {
            self.preedit = None;
        }
        let text = self.text().into_owned();
        let anchor_cur = cursor_at(self.buffer(), byte_at(&text, anchor as usize));
        let head_cur = cursor_at(self.buffer(), byte_at(&text, head as usize));
        self.inner.set_cursor(head_cur);
        if anchor_cur != head_cur {
            self.inner.set_selection(Selection::Normal(anchor_cur));
        } else {
            self.inner.set_selection(Selection::None);
        }
        self.follow = true;
        self.invalidate();
    }

    /// Replaces the text and selects `[anchor, head]` (in characters). A multi-line field keeps the line breaks.
    pub fn reset(&mut self, text: &str, [anchor, head]: [u32; 2], multiline: bool) {
        self.multiline = multiline;
        let text = clean(text, multiline);
        self.inner = empty_editor(&text, &self.style);
        let anchor = cursor_at(self.buffer(), byte_at(&text, anchor as usize));
        let head = cursor_at(self.buffer(), byte_at(&text, head as usize));
        self.inner.set_cursor(head);
        if anchor != head {
            self.inner.set_selection(Selection::Normal(anchor));
        }
        self.preedit = None;
        self.scroll_x = 0.;
        self.follow = true;
        self.undo.clear();
        self.redo.clear();
        self.invalidate();
    }

    pub fn composing(&self) -> bool {
        self.preedit.is_some()
    }

    /// Enables or disables password masking (`•` glyphs, Copy/Cut blocked, surrounding text withheld).
    pub fn set_password(&mut self, password: bool) {
        if self.password != password {
            self.password = password;
            if password {
                self.preedit = None;
            }
            self.invalidate();
        }
    }

    pub fn is_password(&self) -> bool {
        self.password
    }

    /// Returns a UTF-8 char-boundary-safe window of text surrounding the caret/selection (at most `max_bytes`
    /// UTF-8 bytes), with both UTF-8 byte offsets (Wayland `zwp_text_input_v3`) and UTF-16 code-unit offsets
    /// (Windows TSF / macOS). Returns an empty window when `password` is enabled.
    pub fn surrounding_text(&self, max_bytes: usize) -> SurroundingText {
        if self.password || max_bytes == 0 {
            return SurroundingText::default();
        }
        let full = self.text();
        let selected = self.selected_bytes();
        let caret = self.caret();
        let buffer = self.buffer();
        let backwards = match caret.selection {
            Selection::Normal(anchor) | Selection::Line(anchor) | Selection::Word(anchor) => {
                offset_of(buffer, caret.cursor) < offset_of(buffer, anchor)
            }
            Selection::None => false,
        };
        let (head, anchor) = if backwards {
            (selected.start, selected.end)
        } else {
            (selected.end, selected.start)
        };

        let (win_start, win_end) = if full.len() <= max_bytes {
            (0, full.len())
        } else {
            let sel_len = selected.end - selected.start;
            let mid = if sel_len <= max_bytes {
                (selected.start + selected.end) / 2
            } else {
                head
            };
            let mut raw_start = mid.saturating_sub(max_bytes / 2);
            let mut raw_end = (raw_start + max_bytes).min(full.len());
            raw_start = raw_end.saturating_sub(max_bytes);
            if sel_len <= max_bytes {
                if raw_start > selected.start {
                    raw_start = selected.start;
                    raw_end = (raw_start + max_bytes).min(full.len());
                } else if raw_end < selected.end {
                    raw_end = selected.end;
                    raw_start = raw_end.saturating_sub(max_bytes);
                }
            }
            let mut start = raw_start;
            while start < full.len() && !full.is_char_boundary(start) {
                start += 1;
            }
            let mut end = raw_end;
            while end > start && !full.is_char_boundary(end) {
                end -= 1;
            }
            if start > end {
                (end, end)
            } else {
                (start, end)
            }
        };

        let slice = &full[win_start..win_end];
        let cursor_byte = head.clamp(win_start, win_end) - win_start;
        let anchor_byte = anchor.clamp(win_start, win_end) - win_start;
        let cursor_utf16 = slice[..cursor_byte].encode_utf16().count();
        let anchor_utf16 = slice[..anchor_byte].encode_utf16().count();
        SurroundingText {
            text: slice.to_owned(),
            cursor_byte,
            anchor_byte,
            cursor_utf16,
            anchor_utf16,
            window_byte_offset: win_start,
        }
    }

    fn selected_bytes(&self) -> Range<usize> {
        let buffer = self.buffer();
        self.inner.selection_bounds().map_or_else(
            || {
                let i = offset_of(buffer, self.inner.cursor());
                i..i
            },
            |(a, b)| offset_of(buffer, a)..offset_of(buffer, b),
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
        let text = clean(text, self.multiline);
        if !text.is_empty() {
            self.change(|editor| {
                editor.insert_string(&text, None);
                let cursor = editor.cursor();
                let index = editor.with_buffer(|buffer| {
                    grapheme_edge(buffer.lines[cursor.line].text(), cursor.index, true)
                });
                editor.set_cursor(Cursor::new_with_affinity(
                    cursor.line,
                    index,
                    Affinity::Before,
                ));
            });
        }
    }

    fn move_cursor(&mut self, engine: &mut TextEngine, direction: Direction, extend: bool) {
        // A single-line field has no other line to move to.
        if !self.multiline
            && matches!(
                direction,
                Direction::Up | Direction::Down | Direction::PageUp | Direction::PageDown
            )
        {
            return;
        }
        let page = self.page as i32;
        let motion = match direction {
            Direction::Left => Motion::Left,
            Direction::Right => Motion::Right,
            Direction::WordLeft => Motion::LeftWord,
            Direction::WordRight => Motion::RightWord,
            Direction::Start => Motion::Home,
            Direction::End => Motion::End,
            Direction::TextStart => Motion::BufferStart,
            Direction::TextEnd => Motion::BufferEnd,
            Direction::Up => Motion::Up,
            Direction::Down => Motion::Down,
            Direction::PageUp => Motion::Vertical(-page),
            Direction::PageDown => Motion::Vertical(page),
        };

        if extend && self.inner.selection() == Selection::None {
            self.inner
                .set_selection(Selection::Normal(self.inner.cursor()));
        }
        if !extend {
            let bounds = self.inner.selection_bounds();
            self.inner.set_selection(Selection::None);
            if let Some((a, b)) = bounds
                .filter(|(a, b)| a != b && matches!(direction, Direction::Left | Direction::Right))
            {
                self.inner.shape_as_needed(engine.fonts(), false);
                let start = Cursor::new_with_affinity(a.line, a.index, Affinity::After);
                let end = Cursor::new_with_affinity(b.line, b.index, Affinity::Before);
                let toward_left = matches!(direction, Direction::Left);
                let (start_at, end_at) =
                    (caret_at(self.buffer(), start), caret_at(self.buffer(), end));
                let start_on_left =
                    start_at.1 < end_at.1 || (start_at.1 == end_at.1 && start_at.0 <= end_at.0);
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
            let cursor = self.inner.cursor();
            if let Some(run) = self
                .buffer()
                .layout_runs()
                .find(|run| run.line_i == cursor.line && caret_in_run(run, cursor).is_some())
                && run
                    .glyphs
                    .iter()
                    .any(|glyph| glyph.level.is_rtl() != run.rtl)
            {
                let left = matches!(direction, Direction::Left);
                let x = caret_at(self.buffer(), cursor).0;
                if let Some(cursor) = visual_neighbor(&run, x, left) {
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
        let index = offset_of(self.buffer(), self.inner.cursor());
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
        let start = grapheme_edge(&text, index.min(boundary), false);
        let end = grapheme_edge(&text, index.max(boundary), true);
        self.replace_with_empty(start..end);
    }

    fn replace_with_empty(&mut self, bytes: Range<usize>) {
        if bytes.is_empty() {
            return;
        }
        let buffer = self.buffer();
        let (start, end) = (cursor_at(buffer, bytes.start), cursor_at(buffer, bytes.end));
        self.change(|editor| {
            editor.delete_range(start, end);
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
        self.delete_surrounding_byte_range(selected, start, end);
    }

    fn delete_surrounding_bytes(&mut self, before_bytes: usize, after_bytes: usize) {
        let selected = self.selected_bytes();
        let text = self.text();
        let mut start = selected.start.saturating_sub(before_bytes);
        while start > 0 && !text.is_char_boundary(start) {
            start -= 1;
        }
        let mut end = selected.end.saturating_add(after_bytes).min(text.len());
        while end < text.len() && !text.is_char_boundary(end) {
            end += 1;
        }
        self.delete_surrounding_byte_range(selected, start, end);
    }

    fn delete_surrounding_byte_range(
        &mut self,
        selected: Range<usize>,
        start: usize,
        end: usize,
    ) {
        let removed_before = selected.start - start;
        if removed_before == 0 && selected.end == end {
            return;
        }

        let caret = self.caret();
        let buffer = self.buffer();
        let backwards = match caret.selection {
            Selection::Normal(anchor) | Selection::Line(anchor) | Selection::Word(anchor) => {
                offset_of(buffer, caret.cursor) < offset_of(buffer, anchor)
            }
            Selection::None => false,
        };
        // Offsets after the deletion: the head is where the selection's end was, less what was cut before it.
        let (mut head, mut anchor) = (selected.end - removed_before, start);
        if backwards {
            std::mem::swap(&mut head, &mut anchor);
        }
        let cut = (
            cursor_at(buffer, selected.end),
            cursor_at(buffer, end),
            cursor_at(buffer, start),
            cursor_at(buffer, selected.start),
        );
        self.change(|editor| {
            if selected.end < end {
                editor.delete_range(cut.0, cut.1);
            }
            if start < selected.start {
                editor.delete_range(cut.2, cut.3);
            }
            let (head, anchor) =
                editor.with_buffer(|buffer| (cursor_at(buffer, head), cursor_at(buffer, anchor)));
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
        self.follow = true;
        match event {
            EditorEvent::Preedit { text, active } => {
                if self.password {
                    return output;
                }
                let text = clean(text, self.multiline);
                let char_len = text.chars().count();
                let (active, cursor) = match active {
                    Some(range) => {
                        let start = range.start.min(char_len);
                        let end = range.end.min(char_len);
                        if start == end {
                            (None, Some(start))
                        } else {
                            let (lo, hi) = (start.min(end), start.max(end));
                            (Some(lo..hi), Some(hi))
                        }
                    }
                    None => (None, None),
                };
                self.preedit = (!text.is_empty()).then(|| Preedit {
                    text: text.into_owned(),
                    active,
                    segments: Vec::new(),
                    cursor,
                });
                self.invalidate();
            }
            EditorEvent::PreeditSegments {
                text,
                segments,
                cursor,
            } => {
                if self.password {
                    return output;
                }
                let text = clean(text, self.multiline);
                self.preedit = (!text.is_empty()).then(|| Preedit {
                    text: text.into_owned(),
                    active: None,
                    segments: segments.clone(),
                    cursor: *cursor,
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
                if !self.password {
                    self.delete_surrounding(*before, *after);
                }
            }
            EditorEvent::DeleteSurroundingBytes {
                before_bytes,
                after_bytes,
            } => {
                if !self.password {
                    self.delete_surrounding_bytes(*before_bytes, *after_bytes);
                }
            }
            EditorEvent::Cancel | EditorEvent::Point { .. } => {
                if self.preedit.take().is_some() {
                    self.invalidate();
                    output.interrupt_ime = true;
                }
                if let EditorEvent::Point { x, y, select } = *event {
                    let x = x + self.scroll_x;
                    // A single-line field takes the middle of its line, whatever the height of the press.
                    let y = if self.multiline {
                        y
                    } else {
                        self.style.line_height / 2.
                    };
                    if self.password {
                        let real = self.text().into_owned();
                        let masked = mask_text(&real);
                        let wrap = self.buffer().size().0;
                        if self.preview.is_none() {
                            self.preview = Some(engine.shape(&masked, &self.style, wrap));
                        }
                        let preview = self.preview.as_ref().expect("masked preview is shaped");
                        if let Some(mut hit) = preview.hit(x, y) {
                            hit.index = grapheme_edge(
                                preview.lines[hit.line].text(),
                                hit.index,
                                hit.affinity == Affinity::Before,
                            );
                            let masked_byte = offset_of(preview, hit);
                            let char_idx = masked[..masked_byte].chars().count();
                            let real_byte = byte_at(&real, char_idx);
                            let cursor = cursor_at(self.buffer(), real_byte);
                            let selection = match select {
                                PointSelection::Place => Selection::None,
                                PointSelection::Extend => match self.inner.selection() {
                                    Selection::None => Selection::Normal(self.inner.cursor()),
                                    selection => selection,
                                },
                                // Do not expose word boundaries inside password fields.
                                PointSelection::Word | PointSelection::All => {
                                    Selection::Line(cursor)
                                }
                            };
                            self.inner.set_cursor(cursor);
                            self.inner.set_selection(selection);
                        }
                    } else {
                        self.inner.shape_as_needed(engine.fonts(), false);
                        if let Some(mut cursor) = self.buffer().hit(x, y) {
                            cursor.index = grapheme_edge(
                                self.buffer().lines[cursor.line].text(),
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
                let last = self.buffer().lines.len() - 1;
                let end = Cursor::new(last, self.buffer().lines[last].text().len());
                self.inner.set_cursor(end);
            }
            EditorEvent::Copy | EditorEvent::Cut => {
                if self.password {
                    return output;
                }
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
            EditorEvent::LineBreak => {
                if self.multiline {
                    self.insert("\n");
                }
            }
            EditorEvent::Enter => {}
        }
        output
    }

    /// Asks a multi-line field to show the text from `y` down. The next frame clamps it.
    pub fn scroll_to(&mut self, y: f32) {
        self.scroll_y = y;
    }

    /// Draws the field in a box of `size` (width and visible height).
    pub fn frame(
        &mut self,
        engine: &mut TextEngine,
        style: &TextStyle,
        size: [f32; 2],
        scale: f32,
        max_side: usize,
        cached: Option<EditorBitmapKey>,
    ) -> EditorFrame {
        // Color and alignment do not change the shaping: only the fields that do re-shape the text.
        let shaping = |s: &TextStyle| {
            (
                s.size,
                s.line_height,
                s.weight,
                s.letter_spacing,
                s.family.clone(),
            )
        };
        if shaping(&self.style) != shaping(style) {
            self.set_style(engine.fonts(), style);
        }
        let width = size[0].max(0.);
        // A multi-line field wraps where the layout measures it; a single line never wraps.
        let wrap = self.multiline.then_some(width + WRAP_SLACK);
        self.set_wrap(engine.fonts(), wrap);
        self.page = size[1];
        let follow = std::mem::take(&mut self.follow);
        let mut selection_ranges: Vec<Range<usize>> = Vec::new();
        let mut underline_ranges: Vec<(Range<usize>, f32)> = Vec::new();
        let (cursor, caret_visible) = if let Some(preedit) = &self.preedit {
            let replace = self.selected_bytes();
            let start = replace.start;
            let end = start + preedit.text.len();
            if self.preview.is_none() {
                let mut display = self.text().into_owned();
                display.replace_range(replace, &preedit.text);
                self.preview = Some(engine.shape(&display, &self.style, wrap));
            }
            if preedit.segments.is_empty() {
                if let Some(range) = preedit.active.as_ref() {
                    selection_ranges.push(
                        start + byte_at(&preedit.text, range.start)
                            ..start + byte_at(&preedit.text, range.end),
                    );
                }
                underline_ranges.push((start..end, 1.));
            } else {
                for seg in &preedit.segments {
                    let s = start + byte_at(&preedit.text, seg.range.start);
                    let e = start + byte_at(&preedit.text, seg.range.end);
                    if s >= e {
                        continue;
                    }
                    match seg.style {
                        PreeditStyle::Underline => underline_ranges.push((s..e, 1.)),
                        PreeditStyle::ThickUnderline => underline_ranges.push((s..e, 2.)),
                        PreeditStyle::Selection => selection_ranges.push(s..e),
                    }
                }
            }
            let preview = self.preview.as_ref().expect("the preview is shaped");
            let caret_byte = preedit
                .cursor
                .map_or(end, |c| start + byte_at(&preedit.text, c));
            let at = cursor_at(preview, caret_byte);
            let visible = preedit.cursor.is_some() || preedit.active.is_some();
            (
                Cursor::new_with_affinity(at.line, at.index, Affinity::Before),
                visible,
            )
        } else if self.password {
            let real = self.text().into_owned();
            let masked = mask_text(&real);
            if self.preview.is_none() {
                self.preview = Some(engine.shape(&masked, &self.style, wrap));
            }
            let preview = self.preview.as_ref().expect("masked preview is shaped");
            let caret_char = real[..offset_of(self.buffer(), self.inner.cursor())]
                .chars()
                .count();
            let sel = self.selected_bytes();
            let sel_start = byte_at(&masked, real[..sel.start].chars().count());
            let sel_end = byte_at(&masked, real[..sel.end].chars().count());
            if sel_start < sel_end {
                selection_ranges.push(sel_start..sel_end);
            }
            let at = cursor_at(preview, byte_at(&masked, caret_char));
            (
                Cursor::new_with_affinity(at.line, at.index, Affinity::Before),
                true,
            )
        } else {
            if self.multiline {
                self.buffer_mut().shape_until_scroll(engine.fonts(), false);
            } else if !self.text().is_empty() {
                self.inner.shape_as_needed(engine.fonts(), false);
            }
            let sel = self.selected_bytes();
            if !sel.is_empty() {
                selection_ranges.push(sel);
            }
            (self.inner.cursor(), true)
        };
        let buffer = self.preview.as_ref().unwrap_or_else(|| self.buffer());
        let (caret, caret_top) = caret_at(buffer, cursor);
        let line_height = self.style.line_height;
        let content_height = buffer
            .layout_runs()
            .fold(0_f32, |height, run| {
                height.max(run.line_top + run.line_height)
            })
            .max(line_height);
        let view = size[1].max(0.);
        let top = if self.multiline {
            let max = (content_height - view).max(0.);
            let mut top = self.scroll_y.clamp(0., max);
            if follow {
                top = if caret_top < top {
                    caret_top
                } else {
                    top.max(caret_top + line_height - view)
                };
                top = top.clamp(0., max);
            }
            top
        } else {
            0.
        };
        let scroll_x = if self.multiline {
            0.
        } else {
            let line_width = buffer.layout_runs().next().map_or(0., |run| run.line_w);
            let max_scroll = (line_width + 2. - width).max(0.);
            let left = self.scroll_x.clamp(0., max_scroll);
            left.min(caret).max(caret + 2. - width).max(0.)
        };
        let bitmap_key = EditorBitmapKey {
            revision: self.revision,
            size,
            scale,
            max_side,
            scroll_x,
            scroll_y: top,
        };
        let has_glyphs = buffer.layout_runs().any(|run| !run.glyphs.is_empty());
        let bitmap = if Some(bitmap_key) != cached && has_glyphs {
            let lines = if self.multiline { usize::MAX } else { 1 };
            engine.rasterize_buffer(buffer, size, scale, max_side, [scroll_x, top], lines)
        } else {
            None
        };
        let mut selection = Vec::new();
        for range in selection_ranges {
            selection.extend(rows(buffer, range, [scroll_x, top], width));
        }
        let mut composition = Vec::new();
        for (range, thickness) in underline_ranges {
            for [x, y, w, h] in rows(buffer, range, [scroll_x, top], width) {
                composition.push([x, y + h - thickness, w, thickness]);
            }
        }
        self.scroll_x = scroll_x;
        self.scroll_y = top;
        EditorFrame {
            bitmap_key,
            bitmap,
            caret_x: caret - scroll_x,
            caret_y: caret_top - top,
            caret_visible,
            line_height,
            selection,
            composition,
            scroll_y: top,
            content_height,
        }
    }
}

pub(crate) fn mask_text(text: &str) -> String {
    text.chars()
        .map(|c| if c == '\n' { '\n' } else { '•' })
        .collect()
}

fn empty_editor(text: &str, style: &TextStyle) -> Editor<'static> {
    let mut buffer = Buffer::new_empty(metrics(style));
    let last = text.matches('\n').count();
    for (i, line) in text.split('\n').enumerate() {
        let ending = if i < last {
            LineEnding::Lf
        } else {
            LineEnding::None
        };
        buffer.lines.push(BufferLine::new(
            line,
            ending,
            AttrsList::new(&attrs(style)),
            Shaping::Advanced,
        ));
    }
    Editor::new(buffer)
}

// The text an editor holds: one line, or lines broken at `\n` in a multi-line field.
fn clean(text: &str, multiline: bool) -> Cow<'_, str> {
    if !multiline {
        return single_line(text);
    }
    Cow::Owned(
        text.replace("\r\n", "\n")
            .replace('\r', "\n")
            .chars()
            .filter_map(|c| match c {
                '\n' => Some('\n'),
                '\t' | '\u{000B}' | '\u{000C}' | '\u{0085}' | '\u{2028}' | '\u{2029}' => Some(' '),
                c if c.is_control() => None,
                c => Some(c),
            })
            .collect(),
    )
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

/// Byte offset of `cursor` in the text of `buffer`, whose lines are joined by one `\n` each.
fn offset_of(buffer: &Buffer, cursor: Cursor) -> usize {
    buffer.lines[..cursor.line]
        .iter()
        .map(|line| line.text().len() + 1)
        .sum::<usize>()
        + cursor.index
}

/// The cursor at byte `offset` of the text of `buffer`. An offset at a line break is the end of the line before it.
fn cursor_at(buffer: &Buffer, offset: usize) -> Cursor {
    let last = buffer.lines.len() - 1;
    let mut start = 0;
    for (line, text) in buffer.lines.iter().map(|line| line.text()).enumerate() {
        let end = start + text.len();
        if offset <= end || line == last {
            return Cursor::new(line, offset.saturating_sub(start).min(text.len()));
        }
        start = end + 1;
    }
    Cursor::new(last, 0)
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
                nearest = Some((
                    distance,
                    Cursor::new_with_affinity(run.line_i, index, affinity),
                ));
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

// Where `cursor` is drawn in `buffer`: its x and the top of its line.
fn caret_at(buffer: &Buffer, cursor: Cursor) -> (f32, f32) {
    let mut fallback = None;
    for run in buffer.layout_runs().filter(|run| run.line_i == cursor.line) {
        match caret_in_run(&run, cursor) {
            Some((x, true)) => return (x, run.line_top),
            Some((x, false)) => fallback = Some((x, run.line_top)),
            None => {}
        }
    }
    fallback.unwrap_or((0., 0.))
}

// The x of `cursor` in `run`, if the cursor is in it. The flag says whether the cursor's affinity picks this run's
// edge, which matters at a wrap: the cursor sits at the end of the line it ends, or the start of the next.
fn caret_in_run(run: &LayoutRun<'_>, cursor: Cursor) -> Option<(f32, bool)> {
    if run.glyphs.is_empty() {
        return (cursor.index == 0).then_some((0., true));
    }
    let mut alternative = None;
    for glyph in run.glyphs {
        if cursor.index < glyph.start || cursor.index > glyph.end {
            continue;
        }
        let x = cluster_x(run.text, glyph, cursor.index);
        if (cursor.index > glyph.start && cursor.index < glyph.end)
            || (cursor.index == glyph.start && cursor.affinity == Affinity::After)
            || (cursor.index == glyph.end && cursor.affinity == Affinity::Before)
        {
            return Some((x, true));
        }
        alternative = Some(x);
    }
    alternative.map(|x| (x, false))
}

// The rectangles `[x, y, width, height]` of the text in the byte range `bytes` (of the whole text), below `scroll`.
fn rows(
    buffer: &Buffer,
    bytes: Range<usize>,
    [scroll_x, scroll_y]: [f32; 2],
    width: f32,
) -> Vec<[f32; 4]> {
    let mut out = Vec::new();
    let mut line_start = 0;
    for (line, text) in buffer.lines.iter().map(|line| line.text()).enumerate() {
        let end = line_start + text.len();
        let (from, to) = (bytes.start.max(line_start), bytes.end.min(end));
        if from < to {
            let local = from - line_start..to - line_start;
            for run in buffer.layout_runs().filter(|run| run.line_i == line) {
                for span in spans(&run, local.clone(), scroll_x, width) {
                    out.push([
                        span.start,
                        run.line_top - scroll_y,
                        span.end - span.start,
                        run.line_height,
                    ]);
                }
            }
        }
        line_start = end + 1;
    }
    out
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surrounding_text_clamps_utf8_boundaries_and_reports_utf16_offsets() {
        let mut engine = TextEngine::default();
        let mut editor = LineEditor::default();
        // 'a' (1B/1u16), '茶' (3B/1u16), '😀' (4B/2u16), 'z' (1B/1u16) => total 9 UTF-8 bytes, 5 UTF-16 units.
        editor.reset("a茶😀z", [1, 3], false);
        let full = editor.surrounding_text(4000);
        assert_eq!(full.text, "a茶😀z");
        assert_eq!((full.anchor_byte, full.cursor_byte), (1, 8));
        assert_eq!((full.anchor_utf16, full.cursor_utf16), (1, 4));
        assert_eq!(full.window_byte_offset, 0);

        // Tight max_bytes window must never split multi-byte UTF-8 codepoints.
        editor.reset("hello 茶😀 world", [7, 7], false);
        let small = editor.surrounding_text(6);
        assert!(small.text.len() <= 6, "{small:?}");
        assert!(small.text.is_char_boundary(small.cursor_byte));
        assert!(small.text.is_char_boundary(small.anchor_byte));

        // Backward selection swaps anchor and cursor offsets properly.
        editor.handle(
            &mut engine,
            &EditorEvent::Move {
                direction: Direction::Left,
                extend: true,
            },
        );
        let back = editor.surrounding_text(4000);
        assert!(back.cursor_byte < back.anchor_byte);
        assert!(back.cursor_utf16 < back.anchor_utf16);
    }

    #[test]
    fn delete_surrounding_bytes_handles_multibyte_and_unaligned_lengths() {
        let mut engine = TextEngine::default();
        let mut editor = LineEditor::default();
        // Place caret right after '茶' (char index 2, byte 4 in "a茶😀z").
        editor.reset("a茶😀z", [2, 2], false);
        // Wayland deletes exact 3 bytes before ('茶') and 4 bytes after ('😀').
        editor.handle(
            &mut engine,
            &EditorEvent::DeleteSurroundingBytes {
                before_bytes: 3,
                after_bytes: 4,
            },
        );
        assert_eq!(editor.text(), "az");
        assert_eq!(editor.selection(), [1, 1]);

        // Unaligned byte length inside a multi-byte char clamps safely without panicking.
        editor.reset("a茶😀z", [2, 2], false);
        editor.handle(
            &mut engine,
            &EditorEvent::DeleteSurroundingBytes {
                before_bytes: 2,
                after_bytes: 2,
            },
        );
        assert_eq!(editor.text(), "az");
    }

    #[test]
    fn preedit_segments_draws_thin_thick_underlines_and_selection() {
        let mut engine = TextEngine::default();
        let mut editor = LineEditor::default();
        editor.reset("x", [1, 1], false);
        editor.handle(
            &mut engine,
            &EditorEvent::PreeditSegments {
                text: "nihongo".into(),
                segments: vec![
                    PreeditSegment {
                        range: 0..2,
                        style: PreeditStyle::ThickUnderline,
                    },
                    PreeditSegment {
                        range: 2..5,
                        style: PreeditStyle::Underline,
                    },
                    PreeditSegment {
                        range: 5..7,
                        style: PreeditStyle::Selection,
                    },
                ],
                cursor: Some(2),
            },
        );
        assert!(editor.composing());
        let frame = editor.frame(
            &mut engine,
            &TextStyle::default(),
            [300., 30.],
            1.,
            2048,
            None,
        );
        assert!(frame.caret_visible);
        assert!(
            frame.composition.iter().any(|r| (r[3] - 2.).abs() < 0.01),
            "expected 2px thick underline: {:?}",
            frame.composition
        );
        assert!(
            frame.composition.iter().any(|r| (r[3] - 1.).abs() < 0.01),
            "expected 1px thin underline: {:?}",
            frame.composition
        );
        assert!(
            !frame.selection.is_empty(),
            "expected selection highlight for PreeditStyle::Selection"
        );
    }

    #[test]
    fn password_mode_masks_glyphs_blocks_clipboard_and_hides_surrounding_text() {
        let mut engine = TextEngine::default();
        let mut editor = LineEditor::default();
        editor.reset("secret", [0, 6], false);
        editor.set_password(true);
        assert!(editor.is_password());
        assert_eq!(editor.text(), "secret");
        assert_eq!(editor.surrounding_text(4000), SurroundingText::default());

        // Copy and Cut are blocked in password mode.
        let copy = editor.handle(&mut engine, &EditorEvent::Copy);
        assert!(copy.clipboard.is_none());
        let cut = editor.handle(&mut engine, &EditorEvent::Cut);
        assert!(cut.clipboard.is_none());
        assert_eq!(editor.text(), "secret");

        // Preedit is suppressed in password mode.
        editor.handle(
            &mut engine,
            &EditorEvent::Preedit {
                text: "abc".into(),
                active: Some(0..3),
            },
        );
        assert!(!editor.composing());

        // Frame renders without panicking and keeps selection rects aligned with masked bullets.
        let frame = editor.frame(
            &mut engine,
            &TextStyle::default(),
            [200., 30.],
            1.,
            2048,
            None,
        );
        assert!(!frame.selection.is_empty());
        assert!(frame.caret_x > 0.);
    }

    #[test]
    fn preedit_cursor_position_and_hidden_cursor_follow_wayland_spec() {
        let mut engine = TextEngine::default();
        let mut editor = LineEditor::default();
        editor.reset("", [0, 0], false);

        // Cursor in the middle of preedit (`2..2`): visible caret, no selection highlight, caret_x < end.
        editor.handle(
            &mut engine,
            &EditorEvent::Preedit {
                text: "にほんご".into(),
                active: Some(2..2),
            },
        );
        let mid = editor.frame(&mut engine, &TextStyle::default(), [300., 30.], 1., 2048, None);
        assert!(mid.caret_visible);
        assert!(mid.selection.is_empty(), "zero-width active cursor must not draw selection");

        // Cursor at end (`4..4`): caret_x must be strictly to the right of mid.caret_x.
        editor.handle(
            &mut engine,
            &EditorEvent::Preedit {
                text: "にほんご".into(),
                active: Some(4..4),
            },
        );
        let end = editor.frame(&mut engine, &TextStyle::default(), [300., 30.], 1., 2048, None);
        assert!(end.caret_visible);
        assert!(
            end.caret_x > mid.caret_x,
            "expected end caret ({}) > mid caret ({})",
            end.caret_x,
            mid.caret_x
        );

        // Cursor hidden (`None`, Wayland -1/-1): caret_visible must be false.
        editor.handle(
            &mut engine,
            &EditorEvent::Preedit {
                text: "にほんご".into(),
                active: None,
            },
        );
        let hidden = editor.frame(&mut engine, &TextStyle::default(), [300., 30.], 1., 2048, None);
        assert!(!hidden.caret_visible);
        assert!(!hidden.composition.is_empty());
    }
}
