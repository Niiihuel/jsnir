//! Chat on top of the tree. The UI writes the field (draft and selection); the app writes the history and the status.
//! The JS app is app/chat.tsx with the SDK. Rust does not know its ids: after mounting it finds the nodes by kind.
use crate::{
    App, Bridge, Event, Inbox, Log, MAX_PENDING, Mode, Vm,
    editor::{EditorBitmapKey, EditorEvent, EditorFrame, EditorOutput, LineEditor},
    history::{HistoryScroll, HistoryView, ScrollEvent},
    layout::Layout,
    live, or_log, spawn_app,
    text::{TextBitmap, TextEngine, TextStyle},
    tree::{Batch, Edges, Font, Node, Op, Replica, Style, Tree},
};
use std::{
    borrow::Cow,
    cell::RefCell,
    rc::Rc,
    sync::mpsc::{Receiver, TryRecvError},
    time::{Duration, Instant},
};

const CHAT_JS: &str = include_str!(concat!(env!("OUT_DIR"), "/chat.js"));

// Ids of the Rust reference app. The TSX app assigns its own, which may differ.
const HISTORY: u16 = 3;
const INPUT: u16 = 4;
const STATUS: u16 = 5;
const SEND: u16 = 1;

// Both apps produce the same tree and the same changes: the Rust one is the reference without a VM.
struct ChatJs(Vm, Rc<RefCell<Replica>>);
impl App for ChatJs {
    fn mount(&mut self) -> Result<Vec<Op>, String> {
        self.0.try_call("jsnirMount", ())
    }
    fn handle(&mut self, e: Event, _: &Tree) -> Vec<Op> {
        or_log(self.0.try_call("jsnirEvent", (e.callback, e.text)))
    }
    fn reload(&mut self, js: &str, preserve_state: bool) -> Result<Vec<Op>, String> {
        // The old VM's signals carry over to the new one when preserving state. If the new script fails to load, the old one keeps running.
        let state = preserve_state
            .then(|| self.0.try_invoke::<String>("jsnirReloadState", ()).ok())
            .flatten();
        let vm = Vm::try_load(js, self.1.clone(), state.as_deref())?;
        let mount = vm.try_call("jsnirMount", ())?;
        self.0 = vm;
        Ok(mount)
    }
    fn sync(&mut self, view: &Tree, ids: Option<&[u16]>) {
        self.0.sync(view, ids)
    }
}
struct ChatRust;
impl App for ChatRust {
    fn mount(&mut self) -> Result<Vec<Op>, String> {
        // Same tree and styles as app/chat.tsx.
        let create = |id, parent, node| Op::Create {
            id,
            parent,
            before: None,
            node,
        };
        let style = |padding, gap, grow, height| Style {
            padding: Edges::All(padding),
            gap,
            grow,
            height,
            ..Default::default()
        };
        Ok(vec![
            create(
                1,
                None,
                Node::Column {
                    children: vec![],
                    style: Style::default(),
                },
            ),
            create(
                2,
                Some(1),
                Node::Column {
                    children: vec![],
                    style: style(16., 16., 1., None),
                },
            ),
            create(
                HISTORY,
                Some(2),
                Node::List {
                    scroll: 0,
                    children: vec![],
                    style: style(0., 0., 1., None),
                },
            ),
            create(
                INPUT,
                Some(2),
                Node::Input {
                    text: String::new(),
                    selection: [0, 0],
                    submit: SEND,
                    change: 0,
                    placeholder: String::new(),
                    placeholder_color: None,
                    multiline: false,
                    submit_on_enter: false,
                    retain_on_submit: false,
                    purpose: Default::default(),
                    style: style(0., 0., 0., Some(34.)),
                    font: Font::default(),
                    paint: Default::default(),
                },
            ),
            create(
                STATUS,
                Some(1),
                Node::Text {
                    lines: None,
                    align: Default::default(),
                    spans: None,
                    text: String::new(),
                    style: Style::default(),
                    font: Font::default(),
                    paint: Default::default(),
                },
            ),
        ])
    }
    fn handle(&mut self, e: Event, view: &Tree) -> Vec<Op> {
        assert_eq!(e.callback, SEND, "unknown callback");
        let id = view.nodes.end() as u16;
        let Some(Node::Input {
            text,
            selection: [a, b],
            ..
        }) = view.nodes.get(&INPUT)
        else {
            unreachable!()
        };
        vec![
            Op::Create {
                id,
                parent: Some(HISTORY),
                before: None,
                node: Node::Text {
                    lines: None,
                    align: Default::default(),
                    spans: None,
                    text: e.text,
                    style: Style::default(),
                    font: Font::default(),
                    paint: Default::default(),
                },
            },
            Op::SetText {
                id: STATUS,
                text: format!("draft «{text}», selection {a}–{b}"),
            },
        ]
    }
}
pub struct Chat {
    editor: LineEditor,
    pub tree: Tree,
    published: (String, [u32; 2]),
    bridge: Bridge,
    pending: u16,
    // Nodes found by kind after mounting: the list, the field with its submit callback, and the status.
    history: u16,
    history_scroll: HistoryScroll,
    input: u16,
    submit: u16,
    status: u16,
    layout: Layout,
    text: TextEngine,
    // Live reload: new scripts from the watcher and when the last reload was requested.
    live: Option<Receiver<String>>,
    reload_started: Option<Instant>,
}
/// Chat nodes found by kind: the list, the field with its submit callback, and the status.
fn discover(tree: &Tree) -> Result<(u16, u16, u16, u16), &'static str> {
    let history = tree
        .nodes
        .iter()
        .find(|(_, n)| matches!(n, Node::List { .. }))
        .map(|(id, _)| id)
        .ok_or("the chat needs a list")?;
    let (input, submit) = tree
        .nodes
        .iter()
        .find_map(|(id, n)| match n {
            Node::Input { submit, .. } if *submit != 0 => Some((id, *submit)),
            _ => None,
        })
        .ok_or("the chat needs a field with a submit callback")?;
    let root = tree.root.ok_or("the mount needs a root")?;
    let status = *tree
        .children(root)
        .iter()
        .find(|id| matches!(tree.nodes.get(id), Some(Node::Text { .. })))
        .ok_or("the chat needs a status text under the root")?;
    Ok((history, input, submit, status))
}
fn creates(ops: &[Op]) -> Vec<(u16, Option<u16>)> {
    ops.iter()
        .filter_map(|op| match op {
            Op::Create { id, parent, .. } => Some((*id, *parent)),
            _ => None,
        })
        .collect()
}
impl Chat {
    pub fn new(mode: Mode, delay: Duration) -> Self {
        let (mount, bridge) = spawn_app(
            delay,
            move |replica: Rc<RefCell<Replica>>| -> Result<Box<dyn App>, String> {
                Ok(match mode {
                    Mode::Rust => Box::new(ChatRust),
                    Mode::Js => Box::new(ChatJs(
                        Vm::try_load(CHAT_JS, replica.clone(), None)?,
                        replica,
                    )),
                })
            },
        )
        .expect("chat failed to mount");
        let created = creates(&mount.ops);
        let mut tree = Tree::default();
        tree.apply_shared(mount).expect("invalid chat mount");
        let mut layout = Layout::default();
        for (id, parent) in created {
            layout.created(&tree, id, parent);
        }
        let _ = bridge.log.send(Log::Answer(true));
        let (history, input, submit, status) = discover(&tree).unwrap_or_else(|e| panic!("{e}"));
        Self {
            editor: LineEditor::default(),
            tree,
            published: Default::default(),
            bridge,
            pending: 0,
            history,
            history_scroll: HistoryScroll::default(),
            input,
            submit,
            status,
            layout,
            text: TextEngine::default(),
            live: None,
            reload_started: None,
        }
    }
    /// Live reload: watches app/chat.tsx and app/sdk.ts and reloads the app on save if it compiles cleanly.
    pub fn watch(&mut self) {
        self.live = Some(live::watch("chat"));
    }
    /// Whether live reload is on. The backend must redraw now and then to pick reloads up.
    pub fn watching(&self) -> bool {
        self.live.is_some()
    }
    /// Replaces the app code without restarting. The draft, selection and scroll belong to the UI and are
    /// left alone; JS signals carry over to the new app. Returns false when the queue is full.
    pub fn reload(&mut self, js: String) -> bool {
        if self.pending == MAX_PENDING {
            return false;
        }
        self.bridge
            .events
            .try_send(Inbox::Reload(js, true))
            .expect("event queue");
        self.pending += 1;
        self.reload_started = Some(Instant::now());
        true
    }
    /// Computes the layout for the window size. Changed rectangles go into the tree and the replica's log
    /// as a UI batch, so JS reads them from the columns without crossing.
    pub fn layout(&mut self, width: f32, height: f32) {
        let changed = self
            .layout
            .compute(&self.tree, &mut self.text, width, height);
        if changed.is_empty() {
            return;
        }
        let ops = changed
            .into_iter()
            .map(|(id, rect)| Op::SetRect { id, rect })
            .collect();
        let batch = Batch { base: 0, ops };
        self.tree.apply_ui_ref(&batch.ops).expect("layout batch");
        let _ = self.bridge.log.send(Log::Ui(batch));
    }
    /// List rectangle (x, y, width, height).
    pub fn history_rect(&self) -> [f32; 4] {
        self.tree.rect(self.history)
    }
    pub fn history_view(&mut self) -> HistoryView {
        self.history_scroll
            .view(self.history_rect(), self.history_len())
    }
    pub fn scroll_history(&mut self, event: ScrollEvent) {
        let rect = self.history_rect();
        let count = self.history_len();
        self.history_scroll.input(rect, count, event);
    }
    /// Field rectangle, including its send button.
    pub fn input_rect(&self) -> [f32; 4] {
        self.tree.rect(self.input)
    }
    /// Rectangles of the editor and the send button inside the field.
    pub fn controls(&self) -> [[f32; 4]; 2] {
        let [x, y, width, height] = self.input_rect();
        let button = width.clamp(0., 92.);
        [
            [x, y, (width - button - 12.).max(0.), height],
            [x + width - button, y, button, height],
        ]
    }
    /// Content rectangle of the status text, without the node's padding.
    pub fn status_rect(&self) -> [f32; 4] {
        let [x, y, width, height] = self.tree.rect(self.status);
        let padding = self
            .tree
            .nodes
            .get(&self.status)
            .and_then(Node::style)
            .map_or(0., |style| style.padding.sides()[0]);
        [
            x + padding,
            y + padding,
            (width - 2. * padding).max(0.),
            (height - 2. * padding).max(0.),
        ]
    }
    /// Static text pixels for the backend to present on its surface.
    pub fn raster_text(
        &mut self,
        text: &str,
        size: [f32; 2],
        scale: f32,
        wrap: bool,
        max_side: usize,
    ) -> Option<TextBitmap> {
        self.text.rasterize(text, size, scale, wrap, max_side)
    }
    pub fn draft(&self) -> Cow<'_, str> {
        self.editor.text()
    }
    /// Ordered selection in characters, as the tree and the SDK see it.
    pub fn selection(&self) -> [u32; 2] {
        self.editor.selection()
    }
    /// Replaces the document and resets the edit history.
    pub fn set_draft(&mut self, text: &str, selection: [u32; 2]) {
        self.editor.reset(text, selection, false);
    }
    pub fn composing(&self) -> bool {
        self.editor.composing()
    }

    pub fn surrounding_text(&self) -> crate::editor::SurroundingText {
        self.editor.surrounding_text(4000)
    }
    /// Applies the events of one platform delivery. Enter may arrive before or after the Commit.
    pub fn edit(&mut self, events: &[EditorEvent]) -> EditorOutput {
        if events.is_empty() {
            return EditorOutput::default();
        }
        let ime = self.composing()
            || events.iter().any(|event| match event {
                EditorEvent::Commit(_) | EditorEvent::DeleteSurrounding { .. } => true,
                EditorEvent::Preedit { text, .. } => !text.is_empty(),
                _ => false,
            });
        let mut output = EditorOutput::default();
        for event in events {
            if output.interrupt_ime && event.involves_ime() {
                continue;
            }
            if matches!(event, EditorEvent::Enter) {
                if !ime {
                    self.submit();
                }
                continue;
            }
            let effect = self.editor.handle(&mut self.text, event);
            if effect.clipboard.is_some() {
                output.clipboard = effect.clipboard;
            }
            output.interrupt_ime |= effect.interrupt_ime;
        }
        self.publish_input();
        output
    }
    pub fn editor_frame(
        &mut self,
        size: [f32; 2],
        scale: f32,
        max_side: usize,
        cached: Option<EditorBitmapKey>,
    ) -> EditorFrame {
        // The chat field keeps its own 14 px text.
        let style = TextStyle {
            size: 14.,
            line_height: 19.,
            ..TextStyle::default()
        };
        self.editor
            .frame(&mut self.text, &style, size, scale, max_side, cached)
    }
    /// If the draft or selection changed, applies them to the tree and appends them to the replica's log.
    pub fn publish_input(&mut self) {
        let selection = self.selection();
        if (self.draft().as_ref(), selection) == (self.published.0.as_str(), self.published.1) {
            return;
        }
        let draft = self.draft().into_owned();
        let op = Op::SetInput {
            id: self.input,
            text: draft.clone(),
            selection,
        };
        let batch = Batch {
            base: 0,
            ops: vec![op],
        };
        self.tree.apply_ui_ref(&batch.ops).expect("UI batch");
        let _ = self.bridge.log.send(Log::Ui(batch));
        self.published = (draft, selection);
    }
    /// Captures the text and clears the field in the same operation. With a full queue it keeps the draft.
    pub fn submit(&mut self) -> bool {
        if self.composing() || self.draft().trim().is_empty() || self.pending == MAX_PENDING {
            return false;
        }
        let text = self.draft().into_owned();
        self.set_draft("", [0, 0]);
        // The cleared field enters the log before the event: the app never sees already-sent text as a draft.
        self.publish_input();
        // Below MAX_PENDING in flight, the event queue has room.
        self.bridge
            .events
            .try_send(Inbox::Event(Event {
                callback: self.submit,
                text,
            }))
            .expect("event queue");
        self.pending += 1;
        true
    }
    pub fn pending(&self) -> bool {
        self.pending > 0
    }
    pub fn poll(&mut self) {
        let fresh: Vec<String> = self
            .live
            .as_ref()
            .map(|rx| rx.try_iter().collect())
            .unwrap_or_default();
        for js in fresh {
            self.reload(js);
        }
        while let Ok(report) = self.bridge.reports.try_recv() {
            if let Err(e) = report {
                eprintln!("reload rejected, the previous version keeps running: {e}");
            }
        }
        while self.pending > 0 {
            match self.bridge.updates.try_recv() {
                Ok(batch) => {
                    self.pending -= 1;
                    let created = creates(&batch.ops);
                    let text_changed = batch
                        .ops
                        .iter()
                        .filter_map(|op| match op {
                            Op::SetText { id, text } if self.tree.text(*id) != Some(text) => {
                                Some(*id)
                            }
                            _ => None,
                        })
                        .collect::<Vec<_>>();
                    let remount = batch.ops.iter().any(|op| matches!(op, Op::Remove { .. }));
                    let result = self.tree.apply_shared(batch);
                    // The answer goes first: whatever the UI publishes because of this batch (for example, the
                    // draft in a new field) depends on it, and the replica must apply it first.
                    let _ = self.bridge.log.send(Log::Answer(result.is_ok()));
                    match result {
                        Ok(()) if remount => self.remounted(),
                        Ok(()) => {
                            for (id, parent) in created {
                                self.layout.created(&self.tree, id, parent);
                            }
                            for id in text_changed {
                                self.layout.text_changed(id);
                            }
                        }
                        Err(e) => eprintln!("chat batch rejected ({e})"),
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => panic!("chat app thread disconnected"),
            }
        }
    }
    // The app changed structure (live reload): find its nodes again, redo the layout, and publish the
    // draft and selection, which belong to the UI, into the new field.
    fn remounted(&mut self) {
        match discover(&self.tree) {
            Ok((history, input, submit, status)) => {
                (self.history, self.input, self.submit, self.status) =
                    (history, input, submit, status);
                self.layout = Layout::from_tree(&self.tree);
                self.published = Default::default();
                self.publish_input();
                if let Some(start) = self.reload_started.take() {
                    eprintln!(
                        "live: app reloaded in {:.2} ms",
                        start.elapsed().as_secs_f64() * 1e3
                    );
                }
            }
            Err(e) => eprintln!("live: the reloaded app is not shaped like a chat ({e})"),
        }
    }
    pub fn history_len(&self) -> usize {
        self.tree.children(self.history).len()
    }
    pub fn message(&self, i: usize) -> &str {
        self.tree
            .text(self.tree.children(self.history)[i])
            .unwrap_or_default()
    }
    pub fn status(&self) -> &str {
        self.tree.text(self.status).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn settle(chat: &mut Chat) {
        let start = std::time::Instant::now();
        while chat.pending() {
            assert!(start.elapsed() < Duration::from_secs(3));
            chat.poll();
            std::thread::yield_now();
        }
    }
    #[test]
    fn live_reload_keeps_ui_state_and_signals_and_runs_the_new_code() {
        let mut chat = Chat::new(Mode::Js, Duration::ZERO);
        for text in ["one", "two"] {
            chat.set_draft(text, [0, 0]);
            assert!(chat.submit());
        }
        settle(&mut chat);
        chat.layout(640., 480.);
        chat.set_draft("editing…", [3, 8]);
        chat.publish_input();
        // New code: the status is written in upper case.
        assert!(chat.reload(CHAT_JS.replace("draft «", "DRAFT «")));
        settle(&mut chat);
        chat.layout(640., 480.);
        // The list lives in a JS signal and moved to the new app; the draft belongs to the UI and stayed in Rust.
        assert_eq!((chat.history_len(), chat.message(1)), (2, "two"));
        assert_eq!(
            (chat.tree.text(chat.input), chat.draft()),
            (Some("editing…"), Cow::from("editing…"))
        );
        assert_eq!(
            chat.input_rect(),
            [16., 411., 608., 34.],
            "the layout was redone with room for the restored status"
        );
        chat.set_draft("three", [0, 0]);
        assert!(chat.submit());
        settle(&mut chat);
        assert_eq!(
            chat.status(),
            "DRAFT «», selection 0–0",
            "the new code runs"
        );
        // A broken script is rejected and the previous app keeps answering.
        assert!(chat.reload("function (".into()));
        settle(&mut chat);
        chat.set_draft("four", [0, 0]);
        assert!(chat.submit());
        settle(&mut chat);
        assert_eq!((chat.history_len(), chat.message(3)), (4, "four"));
        // A throwing callback does not bring the app down: that submit produces no changes.
        assert!(chat.reload(CHAT_JS.replace(
            "messages.push(text);",
            "throw new Error('deliberate failure');"
        )));
        settle(&mut chat);
        chat.set_draft("five", [0, 0]);
        assert!(chat.submit());
        settle(&mut chat);
        assert_eq!(chat.history_len(), 4);
        assert!(chat.reload(CHAT_JS.to_owned()));
        settle(&mut chat);
        chat.set_draft("six", [0, 0]);
        assert!(chat.submit());
        settle(&mut chat);
        assert_eq!((chat.history_len(), chat.message(4)), (5, "six"));
    }
    #[test]
    fn taffy_reproduces_the_hand_written_egui_layout() {
        // The sizes the egui backend used to hard-code: list 16..624 × 16..414, field 16..624 × 430..464.
        for mode in [Mode::Rust, Mode::Js] {
            let mut chat = Chat::new(mode, Duration::ZERO);
            chat.layout(640., 480.);
            assert_eq!(chat.history_rect(), [16., 16., 608., 398.], "{mode:?}");
            assert_eq!(chat.input_rect(), [16., 430., 608., 34.], "{mode:?}");
            // With no changes, the second pass publishes nothing.
            let version = chat.tree.version;
            chat.layout(640., 480.);
            assert_eq!(chat.tree.version, version);
            // Another window size: the list takes the height and the field moves down.
            chat.layout(800., 600.);
            assert_eq!(
                (chat.history_rect(), chat.input_rect()),
                ([16., 16., 768., 518.], [16., 550., 768., 34.])
            );
        }
    }
    #[test]
    fn status_is_measured_after_an_update_and_rasterized_at_display_scale() {
        let mut chat = Chat::new(Mode::Js, Duration::ZERO);
        chat.layout(640., 480.);
        assert_eq!(chat.status_rect()[3], 0.);
        chat.set_draft("Hello, Córdoba", [0, 0]);
        assert!(chat.submit());
        settle(&mut chat);

        chat.layout(640., 480.);
        let single_line = chat.status_rect()[3];
        assert_eq!(single_line, 19.);
        assert_eq!(chat.input_rect()[1], 430. - single_line);
        chat.layout(140., 480.);
        let [_, _, width, height] = chat.status_rect();
        assert!(
            height > single_line,
            "the status wraps to the available width"
        );
        let version = chat.tree.version;
        chat.layout(140., 480.);
        assert_eq!(chat.tree.version, version);

        let text = chat.status().to_owned();
        let bitmap = chat
            .raster_text(&text, [width, height], 2., true, 2048)
            .unwrap();
        assert_eq!(bitmap.size, [(width * 2.) as usize, (height * 2.) as usize]);
        let first_line_end = bitmap.size[0] * (single_line * 2.) as usize * 4;
        assert!(
            bitmap.rgba[..first_line_end]
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[3] != 0)
        );
        assert!(
            bitmap.rgba[first_line_end..]
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[3] != 0)
        );
    }
    #[test]
    fn ime_candidate_is_local_and_enter_only_sends_after_commit() {
        use EditorEvent::{Commit, Enter, Preedit};
        for commit_first in [false, true] {
            let mut chat = Chat::new(Mode::Js, Duration::ZERO);
            chat.set_draft("Hello, té", [7, 9]);
            chat.publish_input();
            chat.edit(&[
                Preedit {
                    text: "cha".into(),
                    active: Some(0..2),
                },
                Enter,
            ]);
            assert_eq!(chat.tree.text(chat.input), Some("Hello, té"));
            assert_eq!(chat.selection(), [7, 9]);
            assert!(chat.composing() && !chat.pending());
            let frame = chat.editor_frame([120., 19.], 2., 2048, None);
            assert!(frame.bitmap.is_some() && !frame.composition.is_empty());
            assert!(!frame.selection.is_empty());

            let commit = Commit("茶".into());
            let events = if commit_first {
                [commit, Enter]
            } else {
                [Enter, commit]
            };
            chat.edit(&events);
            assert!(!chat.composing() && !chat.pending());
            assert_eq!(chat.tree.text(chat.input), Some("Hello, 茶"));
            chat.edit(&[Enter]);
            settle(&mut chat);
            assert_eq!((chat.history_len(), chat.message(0)), (1, "Hello, 茶"));
            assert!(chat.draft().is_empty());
        }
    }

    #[test]
    fn editor_cuts_and_undoes_graphemes_and_normalizes_paste() {
        use EditorEvent::*;
        let mut chat = Chat::new(Mode::Rust, Duration::ZERO);
        chat.edit(&[Insert("e\u{301}👨‍👩‍👧‍👦".into())]);
        chat.edit(&[Delete {
            backward: true,
            word: false,
        }]);
        assert_eq!(chat.draft(), "e\u{301}");
        chat.edit(&[Delete {
            backward: true,
            word: false,
        }]);
        assert_eq!(chat.draft(), "");
        chat.edit(&[Undo]);
        assert_eq!(chat.draft(), "e\u{301}");
        chat.edit(&[Undo]);
        assert_eq!(chat.draft(), "e\u{301}👨‍👩‍👧‍👦");
        chat.edit(&[Redo]);
        assert_eq!(chat.draft(), "e\u{301}");
        assert_eq!(
            chat.edit(&[SelectAll, Cut]).clipboard.as_deref(),
            Some("e\u{301}")
        );
        chat.edit(&[Insert("one\r\ntwo\u{2028}three".into())]);
        assert_eq!(chat.draft(), "one two three");
    }

    #[test]
    fn reload_keeps_active_composition_and_cancel_preserves_draft() {
        use EditorEvent::*;
        let mut chat = Chat::new(Mode::Js, Duration::ZERO);
        chat.set_draft("my draft", [0, 8]);
        chat.publish_input();
        chat.edit(&[Preedit {
            text: "other".into(),
            active: None,
        }]);
        assert!(chat.reload(CHAT_JS.to_owned()));
        settle(&mut chat);
        assert!(chat.composing());
        assert_eq!(chat.tree.text(chat.input), Some("my draft"));
        assert!(chat.edit(&[Cancel, Commit("other".into())]).interrupt_ime);
        assert_eq!(chat.selection(), [0, 8]);
        assert_eq!(chat.draft(), "my draft");
        assert!(!chat.composing());
        assert!(!chat.pending());
    }

    #[test]
    fn ime_deletes_surrounding_characters_without_erasing_the_selection() {
        use EditorEvent::*;
        let mut chat = Chat::new(Mode::Rust, Duration::ZERO);
        chat.set_draft("é[中]y", [2, 3]);
        chat.edit(&[DeleteSurrounding {
            before: 1,
            after: 1,
        }]);
        assert_eq!(chat.draft(), "é中y");
        assert_eq!(chat.selection(), [1, 2]);
        chat.edit(&[Commit("文".into())]);
        assert_eq!(chat.draft(), "é文y");
        chat.edit(&[Undo, Undo]);
        assert_eq!(chat.draft(), "é[中]y");
        assert_eq!(chat.selection(), [2, 3]);
    }

    #[test]
    fn long_draft_scrolls_to_the_caret_and_reuses_its_bitmap() {
        use crate::editor::Direction;
        let mut chat = Chat::new(Mode::Rust, Duration::ZERO);
        let draft = "a long sentence that is wider than the field";
        let end = draft.chars().count() as u32;
        chat.set_draft(draft, [end, end]);
        let size = [100., 24.];
        let end = chat.editor_frame(size, 2., 2048, None);
        assert!(end.bitmap.is_some() && end.caret_x > 80. && end.caret_x < 100.);
        let cached = chat.editor_frame(size, 2., 2048, Some(end.bitmap_key));
        assert!(cached.bitmap.is_none());
        chat.edit(&[EditorEvent::Move {
            direction: Direction::Start,
            extend: true,
        }]);
        let start = chat.editor_frame(size, 2., 2048, Some(end.bitmap_key));
        assert!(start.bitmap.is_some() && start.caret_x < 1.);
        assert!(
            start
                .selection
                .iter()
                .all(|[x, _, w, _]| 0. <= *x && x + w <= 100.)
        );
    }
    #[test]
    fn app_reads_the_draft_typed_while_it_was_busy() {
        for mode in [Mode::Rust, Mode::Js] {
            let mut chat = Chat::new(mode, Duration::from_millis(80));
            for text in ["Hello, Córdoba", "🧉 with \"limón\""] {
                chat.set_draft(text, [0, 0]);
                assert!(chat.submit());
            }
            chat.set_draft("editing…", [3, 8]);
            chat.publish_input();
            let start = std::time::Instant::now();
            while chat.pending() {
                assert!(start.elapsed() < Duration::from_secs(3));
                chat.poll();
                std::thread::yield_now();
            }
            assert_eq!(
                (chat.history_len(), chat.message(1)),
                (2, "🧉 with \"limón\""),
                "{mode:?}"
            );
            assert_eq!(chat.draft(), "editing…");
            assert_eq!(chat.status(), "draft «editing…», selection 3–8", "{mode:?}");
        }
    }
}
