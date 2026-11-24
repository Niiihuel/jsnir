//! JSNIR: a TypeScript app runs in a pluggable embedded JavaScript VM (QuickJS-ng by default,
//! optional JIT backends via [`engine::JsEngine`]) on its own thread, and Rust owns the native UI.
//!
//! The app and the UI share a tree of nodes with stable ids. The app sends versioned batches; the UI
//! applies each batch whole or rejects it, and never waits for JS to draw a frame.
#[macro_use]
mod contract;
#[cfg(feature = "examples")]
pub mod chat;
pub mod design;
pub mod editor;
pub mod engine;
#[cfg(any(feature = "examples", test))]
pub mod history;
pub mod host;
pub mod icons;
mod layout;
mod list;
#[cfg(feature = "examples")]
mod live;
#[cfg(feature = "examples")]
pub mod measure;
pub mod motion;
pub mod text;
pub mod tree;
#[cfg(any(feature = "examples", all(test, feature = "stepper")))]
mod typescript;

/// Test app for image sampling (SDK + app), for the GPU backend's tests.
#[cfg(feature = "examples")]
pub const SHARP_APP: &str = include_str!(concat!(env!("OUT_DIR"), "/sharp.js"));

/// The Stepper comparison script compiled at build time (SDK + app in one script).
#[cfg(feature = "stepper")]
pub mod apps {
    /// Compiled JSNIR TSX SDK (`app/sdk.ts`) alone, for embedding custom scripts and benchmarks.
    pub const SDK: &str = include_str!(concat!(env!("OUT_DIR"), "/sdk.js"));
    pub const STEPPER: &str = include_str!(concat!(env!("OUT_DIR"), "/stepper.js"));
    /// Test app for `Host::push` (SDK + app).
    pub const HOSTED: &str = include_str!(concat!(env!("OUT_DIR"), "/hosted.js"));
    /// Test app for `Host::set_image` (SDK + app).
    pub const IMAGES: &str = include_str!(concat!(env!("OUT_DIR"), "/images.js"));
    /// Test app for `Text` with `lines` (SDK + app).
    pub const LINES: &str = include_str!(concat!(env!("OUT_DIR"), "/lines.js"));
    /// Test app for `For` (SDK + app).
    pub const KEYED: &str = include_str!(concat!(env!("OUT_DIR"), "/keyed.js"));
    /// Test app for `scroll` (SDK + app).
    pub const SCROLL: &str = include_str!(concat!(env!("OUT_DIR"), "/scroll.js"));
    /// Test app for chat scrolling: `scrollAnchor: "end"`, `scrollTo` and `onScroll` (SDK + app).
    pub const SCROLLCHAT: &str = include_str!(concat!(env!("OUT_DIR"), "/scrollchat.js"));
    /// Test app for interaction states: group hover and active, transitions, `SetSource` (SDK + app).
    pub const STATES: &str = include_str!(concat!(env!("OUT_DIR"), "/states.js"));
    /// Test app for masks and rounded clips (SDK + app).
    pub const MASKS: &str = include_str!(concat!(env!("OUT_DIR"), "/masks.js"));
    /// Test app for `Text` alignment (SDK + app).
    pub const TEXT: &str = include_str!(concat!(env!("OUT_DIR"), "/text.js"));
    /// Test app for `TextInput` with a font, focused and unfocused (SDK + app).
    pub const FIELDS: &str = include_str!(concat!(env!("OUT_DIR"), "/fields.js"));
    /// Test app for rich `Text`: spans with colors, weights, underlines and an inline image (SDK + app).
    pub const SPANS: &str = include_str!(concat!(env!("OUT_DIR"), "/spans.js"));
    /// Test app for per-side borders (`edgeSides`) (SDK + app).
    pub const BORDERS: &str = include_str!(concat!(env!("OUT_DIR"), "/borders.js"));
    /// Test app for scrollbars: thumbs, auto-hide and dragging (SDK + app).
    pub const SCROLLBAR: &str = include_str!(concat!(env!("OUT_DIR"), "/scrollbar.js"));
    /// Test app for overlays: tooltips, popovers anchored, flipped and clamped, context presses and dismiss (SDK + app).
    pub const OVERLAY: &str = include_str!(concat!(env!("OUT_DIR"), "/overlay.js"));
    /// Test app for flex sizing: a grow row with a scroll column above a footer, with and without `minHeight: 0` (SDK + app).
    pub const FILL: &str = include_str!(concat!(env!("OUT_DIR"), "/fill.js"));
    /// Test app for containers: a Column's background, a For's scrollbar and a tooltip's text paint and width (SDK + app).
    pub const CONTAINERS: &str = include_str!(concat!(env!("OUT_DIR"), "/containers.js"));
    /// Test app for a multi-line `TextInput`: wrapping, growing to its max height, Enter and Shift+Enter (SDK + app).
    pub const COMPOSER: &str = include_str!(concat!(env!("OUT_DIR"), "/composer.js"));
    /// Test app for `Host::reload`: a signal, a field, an image and a scroll column (SDK + app).
    pub const RELOAD: &str = include_str!(concat!(env!("OUT_DIR"), "/reload.js"));
}

#[cfg(feature = "engine-quickjs")]
pub use engine::quickjs::compile_bytecode;
#[cfg(feature = "engine-quickjs")]
use engine::quickjs::QuickJsEngine as Vm;
use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    rc::Rc,
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, SyncSender, channel, sync_channel},
    },
    thread,
    time::Duration,
};
#[cfg(feature = "examples")]
use std::{
    fs::File,
    io::{BufWriter, Write},
    path::Path,
    sync::mpsc::{RecvTimeoutError, TryRecvError},
    time::Instant,
};
#[cfg(feature = "examples")]
use tree::Node;
use tree::{Batch, Op, Replica, Tree};

/// Magic header for precompiled JSNIR QuickJS bytecode bundles (`.jsbc`).
pub const BYTECODE_MAGIC: &[u8; 8] = b"JSNRBC01";

/// Input script for a VM: UTF-8 JavaScript source or precompiled bytecode (`JSNRBC01` for QuickJS).
#[derive(Debug, Clone)]
pub enum ScriptBundle {
    Source(String),
    Bytecode(Vec<u8>),
}

impl ScriptBundle {
    /// Detects whether `bytes` starts with [`BYTECODE_MAGIC`]; otherwise interprets it as UTF-8 JS source.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, String> {
        if bytes.starts_with(BYTECODE_MAGIC) {
            Ok(Self::Bytecode(bytes))
        } else {
            String::from_utf8(bytes)
                .map(Self::Source)
                .map_err(|e| format!("script is neither JSNIR bytecode nor valid UTF-8: {e}"))
        }
    }
}

/// The counter as the backends see it. The ids come from the mounted tree.
#[cfg(feature = "examples")]
#[derive(Debug, Clone)]
pub struct Counter {
    pub display: u16,
    pub button: u16,
    pub callback: u16,
    pub value: u16,
}

/// Where the app logic runs.
#[cfg(feature = "examples")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Reference app in Rust, without a VM.
    Rust,
    /// The TSX app in QuickJS.
    Js,
}
#[cfg(feature = "examples")]
impl Mode {
    /// Reads the mode from the first command-line argument (`rust` or `js`, default `js`).
    pub fn from_args() -> Self {
        match std::env::args().nth(1).as_deref() {
            None | Some("js") => Self::Js,
            Some("rust") => Self::Rust,
            Some(other) => panic!("unknown mode {other:?}: expected rust or js"),
        }
    }
}

// Same tree as `Counter` in app/counter.tsx, for the mode without a VM.
#[cfg(feature = "examples")]
fn rust_mount() -> Batch {
    let create = |id, parent, node| Op::Create {
        id,
        parent,
        before: None,
        node,
    };
    Batch {
        base: 0,
        ops: vec![
            // The SDK's `Column` is a box that lays out its children in a column.
            create(
                1,
                None,
                Node::Box {
                    children: vec![],
                    style: Default::default(),
                    paint: Default::default(),
                    press: 0,
                    disabled: false,
                    hover: 0,
                    contextmenu: 0,
                    dismiss: 0,
                    scroll: 0,
                    drag: 0,
                },
            ),
            create(2, Some(1), Node::Label { value: 0 }),
            create(3, Some(1), Node::Button { callback: 1 }),
        ],
    }
}

/// Callback id of messages from the embedding program (`Host::push`). SDK callbacks start at 1.
pub const HOST_CALLBACK: u16 = 0;

/// An event from the UI to the app. `text` carries the submitted text of a field.
#[derive(Debug, Clone, Default)]
pub struct Event {
    pub callback: u16,
    pub text: String,
}
/// Message from the UI to the app thread: an event, a zero-JSON numeric event, or new code for a live reload.
enum Inbox {
    Event(Event),
    Guarded(Event, String),
    Drag {
        callback: u16,
        guard: Option<String>,
        phase: u8,
        x: f32,
        y: f32,
        sx: f32,
        sy: f32,
        dx: f32,
        dy: f32,
    },
    Scroll {
        callback: u16,
        guard: Option<String>,
        top: f32,
        height: f32,
        client: f32,
    },
    ContextMenu {
        callback: u16,
        guard: Option<String>,
        x: f32,
        y: f32,
    },
    List(String),
    Reload(String, bool),
}
/// Ordered log from the UI to the app thread's replica.
enum Log {
    Answer(bool),
    Ui(Batch),
}

#[derive(Default)]
struct UiCoalesced {
    rects: HashMap<u16, [f32; 4]>,
    scrolls: HashMap<u16, [f32; 3]>,
    inputs: HashMap<u16, (String, [u32; 2])>,
    other: Vec<Op>,
}
impl UiCoalesced {
    fn merge(&mut self, ops: Vec<Op>) {
        for op in ops {
            match op {
                Op::SetRect { id, rect } => {
                    self.rects.insert(id, rect);
                }
                Op::SetScroll { id, scroll } => {
                    self.scrolls.insert(id, scroll);
                }
                Op::SetInput {
                    id,
                    text,
                    selection,
                } => {
                    self.inputs.insert(id, (text, selection));
                }
                other => self.other.push(other),
            }
        }
    }
    fn into_batch(self) -> Batch {
        let mut ops = Vec::with_capacity(
            self.rects.len() + self.scrolls.len() + self.inputs.len() + self.other.len(),
        );
        for (id, rect) in self.rects {
            ops.push(Op::SetRect { id, rect });
        }
        for (id, scroll) in self.scrolls {
            ops.push(Op::SetScroll { id, scroll });
        }
        for (id, (text, selection)) in self.inputs {
            ops.push(Op::SetInput {
                id,
                text,
                selection,
            });
        }
        ops.extend(self.other);
        Batch { base: 0, ops }
    }
}

enum LogEntry {
    Answer(bool),
    Ui(UiCoalesced),
}

/// Ordered UI→App log that coalesces repeated `SetRect`, `SetScroll`, and `SetInput` updates per node
/// while preserving strict causal barriers around every `Log::Answer`.
#[derive(Clone, Default)]
struct CoalescingLog(Arc<Mutex<VecDeque<LogEntry>>>);
impl CoalescingLog {
    fn send(&self, entry: Log) -> Result<(), ()> {
        let mut queue = self.0.lock().expect("log lock");
        match entry {
            Log::Answer(accepted) => queue.push_back(LogEntry::Answer(accepted)),
            Log::Ui(batch) => {
                if batch.ops.is_empty() {
                    return Ok(());
                }
                if let Some(LogEntry::Ui(pending)) = queue.back_mut() {
                    pending.merge(batch.ops);
                } else {
                    let mut coalesced = UiCoalesced::default();
                    coalesced.merge(batch.ops);
                    queue.push_back(LogEntry::Ui(coalesced));
                }
            }
        }
        Ok(())
    }
    fn drain(&self) -> VecDeque<LogEntry> {
        std::mem::take(&mut *self.0.lock().expect("log lock"))
    }
}

type ListBindings = Vec<(u64, u16)>;

struct Bridge {
    events: SyncSender<Inbox>,
    updates: Receiver<Arc<Batch>>,
    // Outcome of each reload, sent before the batch that carries it.
    reports: Receiver<Result<(), String>>,
    list_replies: Receiver<Result<ListBindings, String>>,
    // Coalescing ordered log: never blocks the UI thread and stays bounded per node between events.
    log: CoalescingLog,
}
/// Most events the UI keeps in flight; the event queue has the same capacity, so `try_send` cannot fail below it.
const MAX_PENDING: u16 = 64;

/// App logic running on its own thread: JS in a VM, or Rust in reference mode.
trait App {
    fn mount(&mut self) -> Result<Vec<Op>, String>;
    fn handle(&mut self, event: Event, view: &Tree) -> Vec<Op>;
    fn guarded(&mut self, _event: Event, _guard: String) -> Vec<Op> {
        Vec::new()
    }
    fn handle_drag(
        &mut self,
        _callback: u16,
        _guard: Option<String>,
        _phase: u8,
        _x: f32,
        _y: f32,
        _sx: f32,
        _sy: f32,
        _dx: f32,
        _dy: f32,
    ) -> Vec<Op> {
        Vec::new()
    }
    fn handle_scroll(
        &mut self,
        _callback: u16,
        _guard: Option<String>,
        _top: f32,
        _height: f32,
        _client: f32,
    ) -> Vec<Op> {
        Vec::new()
    }
    fn handle_context_menu(
        &mut self,
        _callback: u16,
        _guard: Option<String>,
        _x: f32,
        _y: f32,
    ) -> Vec<Op> {
        Vec::new()
    }
    fn list(&mut self, _delta: String) -> Result<(ListBindings, Vec<Op>), String> {
        Err("this app does not implement virtual List".into())
    }
    /// The view changed in these nodes (or in all of them with `None`). JS apps copy it to their columns.
    fn sync(&mut self, _view: &Tree, _ids: Option<&[u16]>) {}
    /// Replaces the app code and returns the new mount. On failure the previous app keeps running.
    fn reload(&mut self, _js: &str, _preserve_state: bool) -> Result<Vec<Op>, String> {
        Err("this app does not support live reload".into())
    }
}
/// Result of a JS callback: if it threw, the event produces no changes and the app keeps running.
fn or_log(result: Result<Vec<Op>, String>) -> Vec<Op> {
    result.unwrap_or_else(|e| {
        eprintln!("app error, the event produces no changes: {e}");
        Vec::new()
    })
}
fn touched(ops: &[Op]) -> Vec<u16> {
    ops.iter()
        .flat_map(|op| op.touched().into_iter().flatten())
        .collect()
}
/// App thread: mounts the view, keeps the replica up to date with the log, and answers each event with a batch.
fn spawn_app(
    delay: Duration,
    make: impl FnOnce(Rc<RefCell<Replica>>) -> Result<Box<dyn App>, String> + Send + 'static,
) -> Result<(Arc<Batch>, Bridge), String> {
    let (init, boot) = sync_channel::<Result<Arc<Batch>, String>>(1);
    let (events, rx) = sync_channel::<Inbox>(MAX_PENDING.into());
    let (tx, updates) = sync_channel(MAX_PENDING.into());
    let log = CoalescingLog::default();
    let entries = log.clone();
    let (report, reports) = channel();
    let (list_reply, list_replies) = channel();
    thread::spawn(move || {
        let replica: Rc<RefCell<Replica>> = Rc::default();
        let start = || {
            let mut app = make(replica.clone())?;
            let mount = Arc::new(Batch {
                base: 0,
                ops: app.mount()?,
            });
            replica.borrow_mut().produced_shared(Arc::clone(&mount))?;
            app.sync(&replica.borrow().view, None);
            Ok((app, mount))
        };
        let (mut app, mount) = match start() {
            Ok(mounted) => mounted,
            Err(error) => {
                let _ = init.send(Err(error));
                return;
            }
        };
        if init.send(Ok(mount)).is_err() {
            return;
        }
        for message in rx {
            if matches!(
                message,
                Inbox::Event(_)
                    | Inbox::Guarded(..)
                    | Inbox::Drag { .. }
                    | Inbox::Scroll { .. }
                    | Inbox::ContextMenu { .. }
            ) {
                thread::sleep(delay);
            }
            // Catch up with the log. JS is not running, so the columns can be written.
            let (mut ids, mut all) = (Vec::new(), false);
            for entry in entries.drain() {
                match entry {
                    LogEntry::Answer(accepted) => all |= replica.borrow_mut().answered(accepted),
                    LogEntry::Ui(coalesced) => {
                        let batch = coalesced.into_batch();
                        ids.extend(touched(&batch.ops));
                        replica.borrow_mut().foreign(batch);
                    }
                }
            }
            if all || !ids.is_empty() {
                app.sync(&replica.borrow().view, (!all).then_some(&ids));
            }
            let list_delivery = matches!(message, Inbox::List(_));
            let ops = match message {
                Inbox::Event(event) => app.handle(event, &replica.borrow().view),
                Inbox::Guarded(event, guard) => app.guarded(event, guard),
                Inbox::Drag {
                    callback,
                    guard,
                    phase,
                    x,
                    y,
                    sx,
                    sy,
                    dx,
                    dy,
                } => app.handle_drag(callback, guard, phase, x, y, sx, sy, dx, dy),
                Inbox::Scroll {
                    callback,
                    guard,
                    top,
                    height,
                    client,
                } => app.handle_scroll(callback, guard, top, height, client),
                Inbox::ContextMenu {
                    callback,
                    guard,
                    x,
                    y,
                } => app.handle_context_menu(callback, guard, x, y),
                Inbox::List(delta) => match app.list(delta) {
                    Ok((binds, ops)) => {
                        let _ = list_reply.send(Ok(binds));
                        ops
                    }
                    Err(error) => {
                        let _ = list_reply.send(Err(error));
                        Vec::new()
                    }
                },
                // The new app replaces the old one in a single batch: remove the root, then mount.
                Inbox::Reload(js, preserve_state) => match app.reload(&js, preserve_state) {
                    Ok(mount) => {
                        let _ = report.send(Ok(()));
                        let root = replica.borrow().view.root;
                        root.map(|id| Op::Remove { id })
                            .into_iter()
                            .chain(mount)
                            .collect()
                    }
                    Err(e) => {
                        let _ = report.send(Err(e));
                        Vec::new()
                    }
                },
            };
            let batch = Arc::new(Batch {
                base: replica.borrow().view.app_seq,
                ops,
            });
            // The list lane knows the exact evicted subtrees, so it need not republish
            // columns for untouched resident rows on every scroll.
            let removed = batch.ops.iter().any(|op| matches!(op, Op::Remove { .. }));
            let list_ids = list_delivery.then(|| {
                let mut ids = touched(&batch.ops);
                let state = replica.borrow();
                for op in &batch.ops {
                    if let Op::Remove { id } = op {
                        ids.extend(state.view.subtree(*id));
                    }
                }
                ids
            });
            let produced = replica.borrow_mut().produced_shared(Arc::clone(&batch));
            match produced {
                Ok(()) if let Some(ids) = list_ids => app.sync(&replica.borrow().view, Some(&ids)),
                Ok(()) if removed => app.sync(&replica.borrow().view, None),
                Ok(()) => app.sync(&replica.borrow().view, Some(&touched(&batch.ops))),
                Err(e) => { eprintln!("invalid batch from the app ({e}): the UI will reject it"); let st = replica.borrow(); for (i,op) in batch.ops.iter().enumerate() { if let tree::Op::Create{id,parent:Some(p),..} = op { if st.view.nodes.get(p).is_none() { eprintln!("DBGBAD #{i} create {id} under missing {p}; earlier ops on {p}: {:?}", batch.ops[..i].iter().filter(|o| format!("{o:?}").contains(&format!("id: {p},")) || format!("{o:?}").contains(&format!("id: {p} "))).map(|o| format!("{o:?}").chars().take(80).collect::<String>()).collect::<Vec<_>>()); break; } } } }
            }
            if tx.send(batch).is_err() {
                break;
            }
        }
    });
    let mount = boot.recv().map_err(|_| "app thread failed to start")??;
    Ok((
        mount,
        Bridge {
            events,
            updates,
            reports,
            list_replies,
            log,
        },
    ))
}

#[cfg(feature = "examples")]
struct CounterJs(Vm);
#[cfg(feature = "examples")]
impl App for CounterJs {
    fn mount(&mut self) -> Result<Vec<Op>, String> {
        self.0.try_call("jsnirMount", ())
    }
    fn handle(&mut self, e: Event, _: &Tree) -> Vec<Op> {
        or_log(self.0.try_call("jsnirEvent", (e.callback, e.text)))
    }
    fn sync(&mut self, view: &Tree, ids: Option<&[u16]>) {
        self.0.sync(view, ids)
    }
}

/// TypeScript declarations for `jsnir`, generated from the contract and the registered functions.
#[cfg(feature = "engine-quickjs")]
pub fn dts() -> String {
    contract::dts(&Vm::load("", Rc::default()).signatures)
}

// One row per event: event → node → batch → version → first frame drawn after the batch.
#[cfg(feature = "examples")]
struct Trace {
    out: BufWriter<File>,
    start: Instant,
    frame: u64,
    events: u64,
    waiting: VecDeque<(u64, u128, u64)>,
    unframed: Vec<String>,
}
#[cfg(feature = "examples")]
impl Trace {
    fn us(&self) -> u128 {
        self.start.elapsed().as_micros()
    }
    fn event(&mut self) {
        self.events += 1;
        self.waiting.push_back((self.events, self.us(), self.frame));
    }
    fn batch(&mut self, view: &Counter, base: u64, ops: usize, version: u64, status: &str) {
        let Some((event, event_us, frame_at)) = self.waiting.pop_front() else {
            return;
        };
        let (button, callback, waited, now) =
            (view.button, view.callback, self.frame - frame_at, self.us());
        self.unframed.push(format!(
            "{event},{button},{callback},{base},{ops},{version},{status},{waited},{event_us},{now}"
        ));
    }
    fn drawn(&mut self) {
        self.frame += 1;
        let now = self.us();
        for row in self.unframed.drain(..) {
            writeln!(self.out, "{row},{},{now}", self.frame).expect("write trace");
        }
        self.out.flush().expect("write trace");
    }
}

/// The counter runtime used by the benchmark backends.
#[cfg(feature = "examples")]
pub struct Runtime {
    pub view: Counter,
    tree: Tree,
    bridge: Option<Bridge>,
    pending: u16,
    trace: Option<Trace>,
    /// How long `poll` may wait for an answer, counted from the last activation.
    budget: Duration,
    last_activation: Option<Instant>,
}
#[cfg(feature = "examples")]
impl Runtime {
    /// Reads `JSNIR_DELAY_MS` (JS callback delay), `JSNIR_WAIT_US` (poll budget) and `JSNIR_TRACE` (new trace CSV).
    pub fn new(mode: Mode) -> Self {
        let delay = std::env::var("JSNIR_DELAY_MS")
            .map_or(0, |ms| ms.parse().expect("JSNIR_DELAY_MS in milliseconds"));
        let mut rt = Self::with_delay(mode, Duration::from_millis(delay));
        if let Ok(us) = std::env::var("JSNIR_WAIT_US") {
            rt.budget = Duration::from_micros(us.parse().expect("JSNIR_WAIT_US in microseconds"));
        }
        if let Some(path) = std::env::var_os("JSNIR_TRACE") {
            rt.trace_to(path.as_ref())
                .expect("JSNIR_TRACE must be a new file");
        }
        rt
    }
    /// The delay holds each event on the VM thread. Rust mode has no VM and ignores it.
    pub fn with_delay(mode: Mode, delay: Duration) -> Self {
        let (mount, bridge) = match mode {
            Mode::Rust => (Arc::new(rust_mount()), None),
            Mode::Js => {
                let (mount, bridge) = spawn_app(delay, |replica| {
                    Ok(Box::new(CounterJs(Vm::counter(replica))))
                })
                .expect("counter failed to mount");
                (mount, Some(bridge))
            }
        };
        let mut tree = Tree::default();
        tree.apply_shared(mount).expect("invalid mount");
        if let Some(b) = &bridge {
            let _ = b.log.send(Log::Answer(true));
        }
        let display = tree
            .nodes
            .iter()
            .find_map(|(id, n)| matches!(n, Node::Label { .. }).then_some(id));
        let button = tree.nodes.iter().find_map(|(id, n)| match n {
            Node::Button { callback } => Some((id, *callback)),
            _ => None,
        });
        let (Some(display), Some((button, callback))) = (display, button) else {
            panic!("the mount must create a label and a button")
        };
        let Node::Label { value } = tree.nodes[&display] else {
            unreachable!()
        };
        Self {
            view: Counter {
                display,
                button,
                callback,
                value,
            },
            tree,
            bridge,
            pending: 0,
            trace: None,
            // 1 ms: the measured round trip is ≈ 3 µs and a frame at 120 Hz lasts 8.3 ms.
            budget: Duration::from_millis(1),
            last_activation: None,
        }
    }
    pub fn trace_to(&mut self, path: &Path) -> std::io::Result<()> {
        let mut out = BufWriter::new(File::create_new(path)?);
        writeln!(
            out,
            "event,node,callback,base,ops,version,status,frames_waiting,event_us,batch_us,frame,frame_us"
        )?;
        self.trace = Some(Trace {
            out,
            start: Instant::now(),
            frame: 0,
            events: 0,
            waiting: VecDeque::new(),
            unframed: Vec::new(),
        });
        Ok(())
    }
    /// Backends call this after each frame so the trace can number frames.
    pub fn drawn(&mut self) {
        if let Some(t) = &mut self.trace {
            t.drawn()
        }
    }
    pub fn version(&self) -> u64 {
        self.tree.version
    }
    pub fn pending(&self) -> bool {
        self.pending > 0
    }
    /// A button click. Returns false when the event queue is full.
    pub fn activate(&mut self) -> bool {
        if let Some(b) = &self.bridge {
            if self.pending == MAX_PENDING {
                return false;
            }
            let event = Event {
                callback: self.view.callback,
                ..Default::default()
            };
            b.events.try_send(Inbox::Event(event)).expect("event queue");
            self.pending += 1;
            self.last_activation = Some(Instant::now());
            if let Some(t) = &mut self.trace {
                t.event()
            }
        } else {
            if let Some(t) = &mut self.trace {
                t.event()
            }
            let value = (self.view.value + 1) % 1000;
            self.apply(Arc::new(Batch {
                base: self.tree.app_seq,
                ops: vec![Op::Set {
                    id: self.view.display,
                    value,
                }],
            }));
        }
        true
    }
    fn apply(&mut self, batch: Arc<Batch>) {
        let (base, ops) = (batch.base, batch.ops.len());
        let result = self.tree.apply_shared(batch);
        if let Some(b) = &self.bridge {
            let _ = b.log.send(Log::Answer(result.is_ok()));
        }
        let status = match result {
            Ok(()) => {
                if let Node::Label { value } = self.tree.nodes[&self.view.display] {
                    self.view.value = value
                }
                "ok"
            }
            Err(e) => {
                eprintln!(
                    "batch rejected ({e}): base {base}, version {}",
                    self.tree.version
                );
                e
            }
        };
        if let Some(t) = &mut self.trace {
            t.batch(&self.view, base, ops, self.tree.version, status)
        }
    }
    /// Applies the answers that arrived. If an event is younger than `budget`, blocks on the channel until
    /// then, so a fast answer lands in the current frame. A slow answer never holds later frames, because
    /// the deadline counts from the activation.
    pub fn poll(&mut self) {
        let deadline = self.last_activation.map(|t| t + self.budget);
        while self.pending > 0 {
            let updates = &self.bridge.as_ref().unwrap().updates;
            let wait = deadline.map_or(Duration::ZERO, |d| {
                d.saturating_duration_since(Instant::now())
            });
            let received = if wait.is_zero() {
                updates.try_recv().map_err(|e| match e {
                    TryRecvError::Empty => RecvTimeoutError::Timeout,
                    TryRecvError::Disconnected => RecvTimeoutError::Disconnected,
                })
            } else {
                updates.recv_timeout(wait)
            };
            match received {
                Ok(batch) => {
                    self.pending -= 1;
                    self.apply(batch)
                }
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => panic!("app thread disconnected"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "engine-quickjs")]
    use crate::engine::JsEngine;
    #[cfg(all(feature = "engine-quickjs", feature = "examples"))]
    use rquickjs::CatchResultExt;
    #[cfg(feature = "examples")]
    #[test]
    fn ordered_burst_and_wrap() {
        for mode in [Mode::Js, Mode::Rust] {
            let mut r = Runtime::new(mode);
            for i in 0..1024 {
                while !r.activate() {
                    r.poll();
                    thread::yield_now()
                }
                if i == 63 && mode != Mode::Rust {
                    assert!(!r.activate())
                }
            }
            while r.pending() {
                r.poll();
                thread::yield_now()
            }
            // The mount is the batch of version 1.
            assert_eq!((r.version(), r.view.value), (1025, 24));
        }
    }
    #[cfg(feature = "engine-quickjs")]
    #[test]
    fn contract_dts_is_generated_from_rust() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/app/jsnir.d.ts");
        if std::env::var_os("JSNIR_UPDATE").is_some() {
            std::fs::write(path, dts()).unwrap();
        }
        let current = std::fs::read_to_string(path).unwrap_or_default();
        assert_eq!(
            current,
            dts(),
            "app/jsnir.d.ts is stale: JSNIR_UPDATE=1 cargo test contract"
        );
    }
    #[cfg(any(feature = "examples", feature = "stepper"))]
    #[test]
    fn contract_typescript_apps_typecheck() {
        // Apps are checked against the generated .d.ts: using a contract that Rust does not offer fails to compile.
        let root = env!("CARGO_MANIFEST_DIR");
        let tsc = typescript::compiler(std::path::Path::new(root));
        assert!(
            tsc.is_file(),
            "TypeScript compiler not found at {}. Set JSNIR_TSC or run `npm ci`.",
            tsc.display()
        );
        let mut apps = Vec::new();
        if cfg!(feature = "examples") {
            apps.extend(["chat", "counter"]);
        }
        if cfg!(feature = "stepper") {
            apps.extend([
                "stepper",
                "hosted",
                "images",
                "lines",
                "keyed",
                "scroll",
                "scrollchat",
                "states",
                "masks",
                "text",
                "fields",
                "spans",
                "borders",
                "scrollbar",
                "overlay",
                "fill",
                "composer",
                "reload",
            ]);
        }
        for app in apps {
            let out = std::process::Command::new(&tsc)
                .args(["-p", &format!("{root}/app/tsconfig.{app}.json")])
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{app}.tsx:\n{}",
                String::from_utf8_lossy(&out.stdout)
            );
        }
    }
    #[cfg(feature = "examples")]
    fn mount(vm: &Vm) -> Vec<Op> {
        vm.call("jsnirMount", ())
    }
    #[cfg(feature = "examples")]
    fn event(vm: &Vm, callback: u16, text: &str) -> Vec<Op> {
        vm.call("jsnirEvent", (callback, text.to_owned()))
    }
    #[cfg(feature = "examples")]
    #[test]
    fn counter_emits_one_set_per_click_and_keeps_the_button() {
        let vm = Vm::counter(Rc::default());
        assert_eq!(
            mount(&vm),
            rust_mount().ops,
            "the TSX counter mounts the same tree as the Rust reference"
        );
        for value in 1..=3 {
            assert_eq!(event(&vm, 1, ""), vec![Op::Set { id: 2, value }]);
        }
    }
    #[cfg(feature = "examples")]
    #[test]
    fn chat_emits_exactly_one_create_and_one_status_update() {
        let replica: Rc<RefCell<Replica>> = Rc::default();
        let mut vm = Vm::load(
            include_str!(concat!(env!("OUT_DIR"), "/chat.js")),
            replica.clone(),
        );
        let mount = Batch {
            base: 0,
            ops: mount(&vm),
        };
        replica.borrow_mut().produced(&mount).unwrap();
        // As in the real flow: the UI confirms the mount before it publishes the field.
        replica.borrow_mut().answered(true);
        // Ids the SDK assigns in chat.tsx: root 1, column 2, list 3, field 4, status 5.
        let input = Batch {
            base: 0,
            ops: vec![Op::SetInput {
                id: 4,
                text: "other".into(),
                selection: [1, 3],
            }],
        };
        replica.borrow_mut().foreign(input);
        vm.sync(&replica.borrow().view, None);
        let ops = event(&vm, 1, "Hello");
        assert_eq!(
            ops,
            vec![
                Op::Create {
                    id: 6,
                    parent: Some(3),
                    before: None,
                    node: Node::Text {
                        lines: None,
                        align: Default::default(),
                        spans: None,
                        text: "Hello".into(),
                        style: Default::default(),
                        font: Default::default(),
                        paint: Default::default()
                    }
                },
                Op::SetText {
                    id: 5,
                    text: "draft «other», selection 1–3".into()
                },
            ]
        );
    }
    #[cfg(feature = "examples")]
    #[test]
    fn js_reads_the_layout_from_columns_without_crossing() {
        let replica: Rc<RefCell<Replica>> = Rc::default();
        let mut vm = Vm::load(
            include_str!(concat!(env!("OUT_DIR"), "/chat.js")),
            replica.clone(),
        );
        let mount = Batch {
            base: 0,
            ops: mount(&vm),
        };
        let created: Vec<_> = mount
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::Create { id, parent, .. } => Some((*id, *parent)),
                _ => None,
            })
            .collect();
        replica.borrow_mut().produced(&mount).unwrap();
        replica.borrow_mut().answered(true);
        // The UI computes the layout and publishes it as its own batch; the replica applies it and syncs the columns.
        let mut layout = layout::Layout::default();
        for (id, parent) in created {
            layout.created(&replica.borrow().confirmed, id, parent);
        }
        let rects = layout.compute(
            &replica.borrow().confirmed,
            &mut text::TextEngine::default(),
            640.,
            480.,
        );
        replica.borrow_mut().foreign(Batch {
            base: 0,
            ops: rects
                .into_iter()
                .map(|(id, rect)| Op::SetRect { id, rect })
                .collect(),
        });
        vm.sync(&replica.borrow().view, None);
        let read = vm.ctx.with(|c| {
            c.eval::<Vec<i32>, _>(
                "var c = jsnir.columns; [c.x[4], c.y[4], c.width[4], c.height[4], c.height[3]]",
            )
            .unwrap()
        });
        assert_eq!(read, vec![16, 430, 608, 34, 398]);
    }
    #[cfg(feature = "examples")]
    #[test]
    fn signal_writes_within_an_event_are_batched_and_bad_batches_rejected() {
        let sdk = include_str!(concat!(env!("OUT_DIR"), "/counter/sdk.js"));
        let app = "var a = signal(1);
            function App() { return h(Column, null, h(Text, null, function () { return 'v' + a.value; }),
                h(Button, { onClick: function () { a.value = 2; a.value = 3; a.update(function (v) { return v + 1; }); } })); }
            mount(App);";
        let vm = Vm::load(&format!("{sdk}\n{app}"), Rc::default());
        mount(&vm);
        // Three writes in the same event: a single change with the final value.
        assert_eq!(
            event(&vm, 1, ""),
            vec![Op::SetText {
                id: 2,
                text: "v4".into()
            }]
        );
        let err = vm.ctx.with(|c| {
            c.eval::<(), _>("jsnir.batch('{\"ops\": [{\"op\": \"nothing\"}]}')")
                .catch(&c)
                .map_err(|e| e.to_string())
        });
        assert!(err.unwrap_err().contains("invalid batch"));
        assert!(
            vm.ops.borrow().is_empty(),
            "an invalid batch leaves no operations"
        );
    }
    #[cfg(feature = "examples")]
    #[test]
    fn fast_answer_lands_in_the_same_poll_and_slow_one_does_not_block() {
        let mut r = Runtime::with_delay(Mode::Js, Duration::ZERO);
        // Warm up the VM thread so its first wake-up is not measured.
        r.activate();
        while r.pending() {
            r.poll();
        }
        for _ in 0..100 {
            assert!(r.activate());
            r.poll();
            assert!(!r.pending(), "a fast answer must land in the same poll");
        }
        let mut slow = Runtime::with_delay(Mode::Js, Duration::from_millis(200));
        slow.activate();
        let start = Instant::now();
        slow.poll();
        assert!(
            slow.pending() && start.elapsed() < Duration::from_millis(20),
            "waited {:?}",
            start.elapsed()
        );
        let start = Instant::now();
        slow.poll();
        assert!(
            start.elapsed() < Duration::from_millis(1),
            "past the deadline, poll does not wait"
        );
    }
    #[cfg(feature = "examples")]
    #[test]
    fn slow_callback_keeps_drawing_and_traces_each_click() {
        let path = std::env::temp_dir().join(format!("jsnir-trace-{}.csv", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut r = Runtime::with_delay(Mode::Js, Duration::from_millis(150));
        r.trace_to(&path).unwrap();
        r.drawn();
        let (start, mut frames) = (Instant::now(), 0);
        assert!(r.activate());
        while r.pending() {
            assert!(start.elapsed() < Duration::from_secs(3));
            if frames == 10 {
                assert!(r.activate(), "a second click during the wait is queued");
            }
            if r.view.value == 0 {
                frames += 1;
            }
            r.poll();
            r.drawn();
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!((r.view.value, r.version()), (2, 3));
        assert!(
            frames > 50,
            "the window kept drawing: {frames} frames before the first batch"
        );
        drop(r);
        let csv = std::fs::read_to_string(&path).unwrap();
        let rows: Vec<Vec<&str>> = csv
            .lines()
            .skip(1)
            .map(|l| l.split(',').collect())
            .collect();
        assert_eq!(rows.len(), 2);
        for (row, (event, base, version)) in rows.iter().zip([("1", "1", "2"), ("2", "2", "3")]) {
            assert_eq!(
                (row[0], row[1], row[3], row[5], row[6]),
                (event, "3", base, version, "ok")
            );
            assert!(
                row[7].parse::<u64>().unwrap() > 50,
                "frames waiting: {}",
                row[7]
            );
        }
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(feature = "engine-quickjs")]
    #[test]
    fn binary_command_buffer_decodes_all_ops_without_json() {
        let mut vm = Vm::load(
            r#"
            function jsnirMount() {
                var u = jsnir.cmd;
                var f = jsnir.cmdF32;
                var p = 0;
                // OP_SET(1, id=2, value=42)
                u[p++] = 1; u[p++] = 2; u[p++] = 42;
                // OP_SET_DISABLED(2, id=3, disabled=1)
                u[p++] = 2; u[p++] = 3; u[p++] = 1;
                // OP_REMOVE(3, id=4)
                u[p++] = 3; u[p++] = 4;
                // OP_MOVE(4, id=5, before=None)
                u[p++] = 4; u[p++] = 5; u[p++] = 0xffffffff;
                // OP_FOCUS(5, id=6)
                u[p++] = 5; u[p++] = 6;
                // OP_AFTER(6, cb=7, ms=125.5)
                u[p++] = 6; u[p++] = 7; f[p++] = 125.5;
                // OP_SCROLL_TO(7, id=8, flags=offset|smooth=9, offset=320)
                u[p++] = 7; u[p++] = 8; u[p++] = 9; f[p++] = 320.0;
                // OP_SET_STYLE(8, id=9, mask=width(1)|height(2)|layer(1<<11), flags=row(1)|clip(1<<8))
                u[p++] = 8; u[p++] = 9; u[p++] = 1 | 2 | (1 << 11); u[p++] = 1 | (1 << 8);
                f[p++] = 240.0; f[p++] = 80.0; u[p++] = (-2 | 0) >>> 0;
                // OP_SET_PAINT(9, id=10, mask=bg(1)|opacity(1<<7)|cursor(1<<14))
                u[p++] = 9; u[p++] = 10; u[p++] = 1 | (1 << 7) | (1 << 14);
                u[p++] = 0x112233ff >>> 0; f[p++] = 0.75; u[p++] = 7; // ColResize
                jsnir.commit(p);
            }
            "#,
            Rc::default(),
        );
        let ops = vm.call_mount().unwrap();
        assert_eq!(ops.len(), 9);
        assert_eq!(ops[0], Op::Set { id: 2, value: 42 });
        assert_eq!(
            ops[1],
            Op::SetDisabled {
                id: 3,
                disabled: true
            }
        );
        assert_eq!(ops[2], Op::Remove { id: 4 });
        assert_eq!(
            ops[3],
            Op::Move {
                id: 5,
                before: None
            }
        );
        assert_eq!(ops[4], Op::Focus { id: 6 });
        assert_eq!(
            ops[5],
            Op::After {
                ms: 125.5,
                callback: 7
            }
        );
        assert_eq!(
            ops[6],
            Op::ScrollTo {
                id: 8,
                offset: Some(320.0),
                to: None,
                smooth: true
            }
        );
        assert_eq!(
            ops[7],
            Op::SetStyle {
                id: 9,
                style: tree::Style {
                    direction: tree::Axis::Row,
                    clip: true,
                    width: Some(240.0),
                    height: Some(80.0),
                    layer: Some(-2),
                    ..Default::default()
                }
            }
        );
        assert_eq!(
            ops[8],
            Op::SetPaint {
                id: 10,
                paint: Arc::new(tree::Paint {
                    background: Some(0x112233ff),
                    opacity: Some(0.75),
                    cursor: Some(tree::Cursor::ColResize),
                    ..Default::default()
                })
            }
        );
    }

    #[test]
    fn coalescing_log_merges_repeated_ui_updates_and_preserves_answer_barriers() {
        let log = CoalescingLog::default();
        for i in 0..100u32 {
            let _ = log.send(Log::Ui(Batch {
                base: 0,
                ops: vec![
                    Op::SetRect {
                        id: 2,
                        rect: [0., 0., i as f32, 20.],
                    },
                    Op::SetScroll {
                        id: 2,
                        scroll: [i as f32, 500., 100.],
                    },
                    Op::SetInput {
                        id: 3,
                        text: format!("v{i}"),
                        selection: [i, i],
                    },
                ],
            }));
        }
        let _ = log.send(Log::Answer(true));
        for i in 0..50u32 {
            let _ = log.send(Log::Ui(Batch {
                base: 0,
                ops: vec![Op::SetRect {
                    id: 2,
                    rect: [1., 2., (100 + i) as f32, 30.],
                }],
            }));
        }
        let mut drained = log.drain();
        assert_eq!(drained.len(), 3, "expected [Ui, Answer, Ui]");
        let first = match drained.pop_front().unwrap() {
            LogEntry::Ui(u) => u.into_batch(),
            _ => panic!("expected Ui entry"),
        };
        assert_eq!(first.ops.len(), 3);
        assert!(first.ops.contains(&Op::SetRect {
            id: 2,
            rect: [0., 0., 99., 20.]
        }));
        assert!(first.ops.contains(&Op::SetScroll {
            id: 2,
            scroll: [99., 500., 100.]
        }));
        assert!(first.ops.contains(&Op::SetInput {
            id: 3,
            text: "v99".into(),
            selection: [99, 99]
        }));
        assert!(matches!(drained.pop_front().unwrap(), LogEntry::Answer(true)));
        let second = match drained.pop_front().unwrap() {
            LogEntry::Ui(u) => u.into_batch(),
            _ => panic!("expected second Ui entry"),
        };
        assert_eq!(
            second.ops,
            vec![Op::SetRect {
                id: 2,
                rect: [1., 2., 149., 30.]
            }]
        );
    }

    #[cfg(feature = "engine-quickjs")]
    #[test]
    fn bytecode_aot_roundtrip_mounts_and_handles_events() {
        let js = r#"
            var n = 0;
            function jsnirMount() {
                jsnir.batch(JSON.stringify({ ops: [
                    { op: 'create', id: 1, node: { kind: 'box', style: { width: 100, height: 40 }, press: 1 } },
                    { op: 'create', id: 2, parent: 1, node: { kind: 'text', text: 'count:0' } },
                ] }));
            }
            function jsnirEvent(cb) {
                if (cb === 1) { n += 1; jsnir.setText(2, 'count:' + n); }
            }
        "#;
        let bytecode = compile_bytecode(js).expect("compile bytecode");
        assert!(bytecode.starts_with(BYTECODE_MAGIC));

        let mut host =
            host::Host::try_new_auto(bytecode, "{}".into(), text::TextEngine::default())
                .expect("mount bytecode");
        host.frame(200., 100., 1., 4096);
        assert_eq!(host.tree.text(2), Some("count:0"));

        host.pointer_move(Some([20., 20.]));
        host.pointer_down(editor::PointSelection::Place);
        host.pointer_up();
        host.poll();
        assert_eq!(host.tree.text(2), Some("count:1"));
    }

    #[cfg(feature = "engine-quickjs")]
    #[test]
    fn binary_create_and_string_pool_decode_nodes_variants_and_text() {
        let mut vm = Vm::load(
            r#"
            function jsnirMount() {
                var u = jsnir.cmd;
                var f = jsnir.cmdF32;
                var strs = ["Hola 茶😀", "Inter", "avatar.png", "0:1", "updated!"];
                var p = 0;
                // OP_CREATE_COLUMN(13, id=1, parent=None, before=None, flags=has_style(1), mask=width(1), flags=0, width=640)
                u[p++] = 13; u[p++] = 1; u[p++] = 0xffffffff; u[p++] = 0xffffffff; u[p++] = 1;
                u[p++] = 1; u[p++] = 0; f[p++] = 640.0;
                // OP_CREATE_BOX(11, id=2, parent=1, before=None, flags=press(1<<1)|paint(1<<8))
                u[p++] = 11; u[p++] = 2; u[p++] = 1; u[p++] = 0xffffffff; u[p++] = (1 << 1) | (1 << 8);
                u[p++] = 7; // press=7
                // paint: bg(1) | hover(1<<15) -> hover variant: brightness(1<<2) | cursor(1<<12)
                u[p++] = 1 | (1 << 15);
                u[p++] = 0x223344ff >>> 0;
                u[p++] = (1 << 2) | (1 << 12); f[p++] = 1.15; u[p++] = 1; // Cursor::Pointer
                // OP_CREATE_TEXT(12, id=3, parent=2, before=None, text=0, flags=align=Center(1)|lines(1<<2)|font(1<<3))
                u[p++] = 12; u[p++] = 3; u[p++] = 2; u[p++] = 0xffffffff; u[p++] = 0; u[p++] = 1 | (1 << 2) | (1 << 3);
                u[p++] = 2; // lines=2
                // font: size(1) | weight(1<<1) | family(1<<4)
                u[p++] = 1 | (1 << 1) | (1 << 4); f[p++] = 15.0; u[p++] = 600; u[p++] = 1;
                // OP_CREATE_IMAGE(17, id=4, parent=2, before=None, source=2, flags=0)
                u[p++] = 17; u[p++] = 4; u[p++] = 2; u[p++] = 0xffffffff; u[p++] = 2; u[p++] = 0;
                // OP_CREATE_ICON(16, id=5, parent=2, before=None, icon=Tick(2), size=16, stroke=2, flags=0)
                u[p++] = 16; u[p++] = 5; u[p++] = 2; u[p++] = 0xffffffff; u[p++] = 2; f[p++] = 16.0; f[p++] = 2.0; u[p++] = 0;
                // OP_CALLBACK_GUARD(19, cb=7, token=3)
                u[p++] = 19; u[p++] = 7; u[p++] = 3;
                // OP_SET_TEXT(10, id=3, text=4)
                u[p++] = 10; u[p++] = 3; u[p++] = 4;
                jsnir.commitStrings(p, strs);
            }
            "#,
            Rc::default(),
        );
        let ops = vm.call_mount().unwrap();
        assert_eq!(ops.len(), 7);
        let mut tree = Tree::default();
        tree.apply(Batch { base: 0, ops }).expect("valid binary batch");
        assert_eq!(tree.root, Some(1));
        assert_eq!(tree.text(3), Some("updated!"));
        match &tree.nodes[&2] {
            tree::Node::Box { press, paint, .. } => {
                assert_eq!(*press, 7);
                assert_eq!(paint.background, Some(0x223344ff));
                let hover = paint.hover.as_ref().expect("hover variant");
                assert_eq!(hover.brightness, Some(1.15));
                assert_eq!(hover.cursor, Some(tree::Cursor::Pointer));
            }
            other => panic!("expected Box, got {other:?}"),
        }
        match &tree.nodes[&3] {
            tree::Node::Text {
                align, lines, font, ..
            } => {
                assert_eq!(*align, tree::TextAlign::Center);
                assert_eq!(*lines, Some(2));
                assert_eq!(font.size, Some(15.0));
                assert_eq!(font.weight, Some(600));
                assert_eq!(font.family.as_deref(), Some("Inter"));
            }
            other => panic!("expected Text, got {other:?}"),
        }
        match &tree.nodes[&4] {
            tree::Node::Image { source, .. } => assert_eq!(source, "avatar.png"),
            other => panic!("expected Image, got {other:?}"),
        }
    }

    #[cfg(all(feature = "stepper", feature = "engine-quickjs"))]
    #[test]
    fn binary_sdk_roundtrip_covers_all_opcodes_rich_paint_spans_and_overflow_flush() {
        let app = r#"
            var jsonCalls = 0;
            var binCalls = 0;
            var origBatch = jsnir.batch;
            var origCommit = jsnir.commit;
            var origCommitStrings = jsnir.commitStrings;
            jsnir.batch = function (j) { jsonCalls++; return origBatch(j); };
            jsnir.commit = function (n) { binCalls++; return origCommit(n); };
            jsnir.commitStrings = function (n, s) { binCalls++; return origCommitStrings(n, s); };

            var spansSig = signal([
                { text: "hello ", color: 0xff0000ff, weight: 700, size: 15, family: "Inter", underline: true, italic: true, strike: true, background: 0x112233ff, backgroundRadius: 4 },
                { text: "", image: { source: "emoji:wave", width: 18, height: 18 } }
            ]);
            var fieldRef = null;
            var listItems = list(["row-a"]);

            function App() {
                return h(Column, {
                    style: { width: 800, height: 600, padding: [4, 8, 12, 16], margin: 6, gap: 4, scroll: true, scrollAnchor: "end", group: true },
                    paint: {
                        background: 0x10141dff,
                        radius: [2, 4, 6, 8],
                        edge: 0x334455ff,
                        edgeWidth: 1.5,
                        edgeSides: [1, 0, 1, 0],
                        mask: "fade-mask",
                        shadows: [
                            { y: 4, blur: 12, spread: -1, color: 0x00000080 },
                            { y: 1, blur: 3, spread: 0, color: 0x00000040 }
                        ],
                        transitions: [
                            { property: "opacity", duration: 180, delay: 20, hold: 10, easing: "bezier", bezier: [0.2, 0.8, 0.2, 1.0] },
                            { property: "radius", duration: 240, easing: "spring" }
                        ],
                        animations: [
                            {
                                property: "scale",
                                duration: 400,
                                delay: 50,
                                easing: "spring",
                                iterations: 2,
                                alternate: true,
                                keyframes: [
                                    { offset: 0, value: 0.9, color: 0x111111ff },
                                    { offset: 1, value: 1.0 }
                                ]
                            }
                        ],
                        starting: { opacity: 0, x: -4, y: 8, scale: 0.95, draw: 0.5 },
                        exiting: { opacity: 0, x: -120, y: 12, scale: 0.92 },
                        scrollbar: { width: 6, thumb: 0x64748bff, track: 0x1e293bff, radius: 3, minThumb: 24, inset: 2, autoHide: true }
                    }
                },
                    h(Box, {
                        style: {
                            position: "fixed",
                            layer: 50,
                            anchor: { node: 1, side: "right", align: "end", gap: 10, flip: true, placement: { callback: 9, token: "0:tok" } }
                        }
                    }),
                    h(Text, {
                        font: { size: 14, weight: 600, lineHeight: 20, letterSpacing: 0.5, family: "Inter" },
                        spans: spansSig,
                        children: "hello "
                    }),
                    h(TextInput, {
                        value: "secret",
                        placeholder: "Enter password",
                        placeholderColor: 0x64748bff,
                        multiline: true,
                        submitOnEnter: true,
                        retainOnSubmit: true,
                        purpose: "password",
                        onSubmit: function () {},
                        onChange: function () {},
                        ref: function (f) { fieldRef = f; },
                        style: { height: 42 },
                        font: { size: 13 },
                        paint: { color: 0xffffffff }
                    }),
                    h(List, {
                        items: listItems,
                        style: { grow: 1 },
                        children: function (item) { return h(Text, null, item); }
                    }),
                    h(VirtualList, {
                        name: "feed",
                        style: { grow: 1 },
                        children: function (_k, item) { return h(Text, null, function () { return String(item()); }); }
                    }),
                    // Trigger mid-mount binary command buffer flush (-2 overflow retry path) across >16,384 words.
                    h(Column, null, Array.from({ length: 700 }, function (_, i) {
                        return h(Box, {
                            style: { width: 100 + i, height: 24, padding: 4 },
                            paint: {
                                background: 0x1a1d24ff,
                                radius: 4,
                                shadows: [{ y: 2, blur: 6, spread: 0, color: 0x00000044 }],
                                transitions: [{ property: "background", duration: 120, easing: "easeOut" }]
                            }
                        });
                    })),
                    h(Button, {
                        onClick: function () {
                            spansSig.value = [{ text: "updated", color: 0x00ff00ff, italic: true }];
                            fieldRef.replace("next-secret", "secret");
                        }
                    })
                );
            }
            mount(App);
        "#;
        let mut vm = Vm::load(&format!("{}\n{app}", crate::apps::SDK), Rc::default());
        let mount_ops = vm.call_mount().expect("mount binary sdk");
        let (json_after_mount, bin_after_mount): (u32, u32) = vm.ctx.with(|c| {
            let j: u32 = c.eval("jsonCalls").unwrap();
            let b: u32 = c.eval("binCalls").unwrap();
            (j, b)
        });
        assert_eq!(json_after_mount, 0, "expected 0 JSON fallbacks on mount");
        assert!(
            bin_after_mount >= 2,
            "expected >16384 words to trigger binary overflow flush & retry, got {bin_after_mount} commits"
        );
        assert!(
            mount_ops
                .iter()
                .any(|op| matches!(op, Op::VirtualList { id: 7, name } if name == "feed")),
            "expected Op::VirtualList for node 7"
        );
        let button_cb = mount_ops
            .iter()
            .find_map(|op| match op {
                Op::Create {
                    node: tree::Node::Button { callback },
                    ..
                } => Some(*callback),
                _ => None,
            })
            .expect("button node");

        let mut tree = Tree::default();
        tree.apply(Batch {
            base: 0,
            ops: mount_ops,
        })
        .expect("mount batch must be valid");

        // Verify root Column (id 1): Style + extended Paint fields
        match &tree.nodes[&1] {
            tree::Node::Box { style, paint, .. } => {
                assert_eq!(style.direction, tree::Axis::Column);
                assert_eq!(style.padding, tree::Edges::Sides([4., 8., 12., 16.]));
                assert_eq!(style.margin, tree::Edges::Sides([6., 6., 6., 6.]));
                assert!(style.scroll && style.group);
                assert_eq!(style.scroll_anchor, tree::ScrollAnchor::End);
                assert_eq!(paint.mask.as_deref(), Some("fade-mask"));
                assert_eq!(paint.shadows.len(), 2);
                assert_eq!(paint.shadows[0].blur, 12.);
                assert_eq!(paint.shadows[0].spread, -1.);
                assert_eq!(paint.transitions.len(), 2);
                assert_eq!(paint.transitions[0].property, tree::Property::Opacity);
                assert_eq!(paint.transitions[0].easing, tree::Easing::Bezier);
                assert_eq!(paint.transitions[0].bezier, Some([0.2, 0.8, 0.2, 1.0]));
                assert_eq!(paint.transitions[0].delay, 20.);
                assert_eq!(paint.transitions[0].hold, 10.);
                assert_eq!(paint.transitions[1].property, tree::Property::Radius);
                assert_eq!(paint.transitions[1].duration, 240.);
                assert_eq!(paint.transitions[1].easing, tree::Easing::Spring);
                let anims = paint.animations.as_ref().expect("animations");
                assert_eq!(anims.len(), 1);
                assert_eq!(anims[0].property, tree::Property::Scale);
                assert_eq!(anims[0].easing, tree::Easing::Spring);
                assert_eq!(anims[0].iterations, Some(2.));
                assert!(anims[0].alternate);
                assert_eq!(anims[0].keyframes.len(), 2);
                assert_eq!(anims[0].keyframes[0].color, Some(0x111111ff));
                let st = paint.starting.as_ref().expect("starting");
                assert_eq!(st.opacity, Some(0.));
                assert_eq!(st.x, Some(-4.));
                assert_eq!(st.y, Some(8.));
                assert_eq!(st.scale, Some(0.95));
                assert_eq!(st.draw, Some(0.5));
                let ex = paint.exiting.as_ref().expect("exiting");
                assert_eq!(ex.opacity, Some(0.));
                assert_eq!(ex.x, Some(-120.));
                assert_eq!(ex.y, Some(12.));
                assert_eq!(ex.scale, Some(0.92));
                let sb = paint.scrollbar.as_ref().expect("scrollbar");
                assert_eq!(sb.width, 6.);
                assert_eq!(sb.thumb, 0x64748bff);
                assert_eq!(sb.track, Some(0x1e293bff));
                assert_eq!(sb.min_thumb, 24.);
                assert!(sb.auto_hide);
            }
            other => panic!("expected root Box, got {other:?}"),
        }

        // Verify anchored fixed Box (id 2)
        match &tree.nodes[&2] {
            tree::Node::Box { style, .. } => {
                assert_eq!(style.position, tree::Position::Fixed);
                assert_eq!(style.layer, Some(50));
                let anchor = style.anchor.as_ref().expect("anchor");
                assert_eq!(anchor.node, 1);
                assert_eq!(anchor.side, tree::Side::Right);
                assert_eq!(anchor.align, tree::AnchorAlign::End);
                assert_eq!(anchor.gap, 10.);
                assert!(anchor.flip);
                let pl = anchor.placement.as_ref().expect("placement");
                assert_eq!(pl.callback, 9);
                assert_eq!(pl.token, "0:tok");
            }
            other => panic!("expected anchored Box, got {other:?}"),
        }

        // Verify rich Text with spans (id 3)
        match &tree.nodes[&3] {
            tree::Node::Text { font, spans, .. } => {
                assert_eq!(font.line_height, Some(20.));
                assert_eq!(font.letter_spacing, 0.5);
                let spans = spans.as_ref().expect("spans");
                assert_eq!(spans.len(), 2);
                assert_eq!(spans[0].text, "hello ");
                assert_eq!(spans[0].color, Some(0xff0000ff));
                assert_eq!(spans[0].weight, Some(700));
                assert_eq!(spans[0].size, Some(15.));
                assert_eq!(spans[0].family.as_deref(), Some("Inter"));
                assert!(spans[0].underline && spans[0].italic && spans[0].strike);
                assert_eq!(spans[0].background, Some(0x112233ff));
                assert_eq!(spans[0].background_radius, 4.);
                let img = spans[1].image.as_ref().expect("inline image");
                assert_eq!(img.source, "emoji:wave");
                assert_eq!((img.width, img.height), (18., 18.));
            }
            other => panic!("expected Text with spans, got {other:?}"),
        }

        // Verify TextInput (id 4)
        match &tree.nodes[&4] {
            tree::Node::Input {
                text,
                selection,
                placeholder,
                placeholder_color,
                multiline,
                submit_on_enter,
                retain_on_submit,
                purpose,
                ..
            } => {
                assert_eq!(text, "secret");
                assert_eq!(*selection, [6, 6]);
                assert_eq!(placeholder, "Enter password");
                assert_eq!(*placeholder_color, Some(0x64748bff));
                assert!(*multiline && *submit_on_enter && *retain_on_submit);
                assert_eq!(*purpose, tree::InputPurpose::Password);
            }
            other => panic!("expected Input, got {other:?}"),
        }

        // Verify List (id 5) + VirtualList (id 7)
        assert!(matches!(tree.nodes[&5], tree::Node::List { .. }));
        assert!(matches!(tree.nodes[&7], tree::Node::List { .. }));

        // Trigger OP_SET_SPANS + OP_SET_FIELD via the Button click handler
        let ev_ops = event(&vm, button_cb, "");
        let json_after_ev: u32 = vm.ctx.with(|c| c.eval("jsonCalls").unwrap());
        assert_eq!(json_after_ev, 0, "expected 0 JSON fallbacks on SetSpans/SetField");
        assert!(
            ev_ops.iter().any(|op| matches!(op, Op::SetSpans { id: 3, spans } if spans.len() == 1 && spans[0].text == "updated" && spans[0].italic)),
            "expected Op::SetSpans in {ev_ops:?}"
        );
        assert!(
            ev_ops.iter().any(|op| matches!(op, Op::SetField { id: 4, expected, text } if expected == "secret" && text == "next-secret")),
            "expected Op::SetField in {ev_ops:?}"
        );
        tree.apply(Batch {
            base: 1,
            ops: ev_ops,
        })
        .expect("event batch must apply cleanly");
    }

    #[cfg(feature = "engine-quickjs")]
    #[test]
    fn vm_limits_interrupt_infinite_loop_and_enforce_memory_ceiling() {
        use crate::engine::{JsEngineFactory, VmConfig, VmLimits, quickjs::QuickJsFactory};

        let script = r#"
            var n = 0;
            function jsnirMount() {
                jsnir.batch(JSON.stringify({ ops: [
                    { op: 'create', id: 1, node: { kind: 'text', text: 'ok:0' } }
                ] }));
            }
            function jsnirEvent(cb) {
                if (cb === 1) {
                    jsnir.setText(1, 'partial');
                    while (true) {}
                }
                if (cb === 2) {
                    var junk = [];
                    for (var i = 0; i < 100000; i++) junk.push('x'.repeat(1024));
                }
                if (cb === 3) {
                    n += 1;
                    jsnir.setText(1, 'ok:' + n);
                }
            }
        "#;
        let factory = QuickJsFactory;
        let config = VmConfig {
            args: "{}".into(),
            limits: VmLimits {
                max_memory_bytes: Some(8 * 1024 * 1024),
                max_call_duration: Some(Duration::from_millis(25)),
            },
            ..Default::default()
        };
        let mut engine = factory
            .create(&ScriptBundle::Source(script.into()), Rc::default(), &config)
            .expect("load vm with limits");
        assert_eq!(engine.name(), "quickjs-ng");
        let mount = engine.call_mount().expect("mount");
        assert_eq!(mount.len(), 1);
        let mem = engine.memory_usage().expect("memory usage");
        assert!(mem.used_bytes > 0 && mem.total_bytes == 8 * 1024 * 1024);

        // Infinite loop is interrupted without hanging and discards partial ops.
        let hang = engine.call_event(1, "", None);
        assert!(hang.is_err(), "expected interrupt error, got {hang:?}");

        // Runaway allocation hits memory ceiling cleanly.
        let oom = engine.call_event(2, "", None);
        assert!(oom.is_err(), "expected OOM error, got {oom:?}");

        // VM remains healthy for subsequent events.
        let ok = engine.call_event(3, "", None).expect("recovery event");
        assert_eq!(
            ok,
            vec![Op::SetText {
                id: 1,
                text: "ok:1".into()
            }]
        );
    }
}
