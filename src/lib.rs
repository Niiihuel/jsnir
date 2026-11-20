//! JSNIR: a TypeScript app runs in an embedded QuickJS VM on its own thread, and Rust owns the native UI.
//!
//! The app and the UI share a tree of nodes with stable ids. The app sends versioned batches; the UI
//! applies each batch whole or rejects it, and never waits for JS to draw a frame.
#[macro_use]
mod contract;
#[cfg(feature = "examples")]
pub mod chat;
pub mod design;
pub mod editor;
#[cfg(any(feature = "examples", test))]
pub mod history;
pub mod host;
pub mod icons;
mod layout;
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
}

use contract::Columns;
use rquickjs::{CatchResultExt, Ctx, Exception, Function, function::IntoArgs};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::mpsc::{Receiver, Sender, SyncSender, channel, sync_channel},
    thread,
    time::Duration,
};
#[cfg(feature = "examples")]
use std::{
    collections::VecDeque,
    fs::File,
    io::{BufWriter, Write},
    path::Path,
    sync::mpsc::{RecvTimeoutError, TryRecvError},
    time::Instant,
};
#[cfg(feature = "examples")]
use tree::Node;
use tree::{Batch, Op, Replica, Tree};

/// Registers a native function on a JS object and records its TypeScript signature from the same declaration.
/// A leading `Ctx` parameter is the VM context and does not appear in the JS signature.
macro_rules! def {
    ($obj:ident, $c:ident, $sigs:ident, $name:literal, || -> $ret:ty $body:block) => {{
        $sigs.push(format!("{}(): {}", $name, <$ret as $crate::contract::Ts>::ts()));
        $obj.set($name, Function::new($c.clone(), move || -> $ret { $body })?)?;
    }};
    ($obj:ident, $c:ident, $sigs:ident, $name:literal, |$ctx:ident: Ctx, $($arg:ident: $ty:ty),*| -> $ret:ty $body:block) => {{
        let args: Vec<String> = vec![$(format!("{}: {}", stringify!($arg), <$ty as $crate::contract::Ts>::ts())),*];
        $sigs.push(format!("{}({}): {}", $name, args.join(", "), <$ret as $crate::contract::Ts>::ts()));
        $obj.set($name, Function::new($c.clone(), move |$ctx: Ctx, $($arg: $ty),*| -> $ret { $body })?)?;
    }};
    ($obj:ident, $c:ident, $sigs:ident, $name:literal, |$($arg:ident: $ty:ty),*| -> $ret:ty $body:block) => {{
        let args: Vec<String> = vec![$(format!("{}: {}", stringify!($arg), <$ty as $crate::contract::Ts>::ts())),*];
        $sigs.push(format!("{}({}): {}", $name, args.join(", "), <$ret as $crate::contract::Ts>::ts()));
        $obj.set($name, Function::new($c.clone(), move |$($arg: $ty),*| -> $ret { $body })?)?;
    }};
}

/// A QuickJS VM. It is created and used on the app thread only; the UI thread never touches it.
struct Vm {
    ctx: rquickjs::Context,
    // Native `jsnir` functions push operations here; the call that triggered them collects the batch.
    ops: Rc<RefCell<Vec<Op>>>,
    // Numeric view data that JS reads without crossing the boundary (`jsnir.columns`).
    columns: Columns,
    signatures: Vec<String>,
    _rt: rquickjs::Runtime,
}
impl Vm {
    /// The counter app (app/counter.tsx).
    #[cfg(feature = "examples")]
    fn counter(replica: Rc<RefCell<Replica>>) -> Self {
        Self::load(
            include_str!(concat!(env!("OUT_DIR"), "/counter.js")),
            replica,
        )
    }
    /// Text reads go to `replica`'s view, which lives on the app thread. Numbers reach JS through the columns (see `sync`).
    fn load(js: &str, replica: Rc<RefCell<Replica>>) -> Self {
        Self::try_load(js, replica, None).unwrap_or_else(|e| panic!("script failed to load: {e}"))
    }
    /// Like `load`, but returns the script error. On a live reload, `previous` is the signal state of the
    /// old VM; the app finds it in `__jsnirPreviousState` before it runs.
    fn try_load(
        js: &str,
        replica: Rc<RefCell<Replica>>,
        previous: Option<&str>,
    ) -> Result<Self, String> {
        Self::try_load_with(js, replica, previous, |_| Ok(()))
    }
    fn try_load_with(
        js: &str,
        replica: Rc<RefCell<Replica>>,
        previous: Option<&str>,
        configure: impl for<'js> FnOnce(&Ctx<'js>) -> rquickjs::Result<()>,
    ) -> Result<Self, String> {
        let rt = rquickjs::Runtime::new().expect("VM out of memory");
        let ctx = rquickjs::Context::full(&rt).expect("VM out of memory");
        let ops: Rc<RefCell<Vec<Op>>> = Rc::default();
        let mut sigs = Vec::new();
        let columns = ctx.with(|c| {
            let r = (|| {
                let jsnir = rquickjs::Object::new(c.clone())?;
                // Frequent writes are plain calls with numbers, no JSON. rquickjs rejects numbers outside
                // u16; the tree validates the rest when it applies the batch.
                let sink = ops.clone();
                def!(jsnir, c, sigs, "set", |id: u16, value: u16| -> () {
                    sink.borrow_mut().push(Op::Set { id, value })
                });
                // Structure: a whole subtree crosses in one call. Creating nodes is rare and the JSON
                // cost is amortized over the subtree.
                let sink = ops.clone();
                def!(
                    jsnir,
                    c,
                    sigs,
                    "batch",
                    |ctx: Ctx, json: String| -> rquickjs::Result<()> {
                        let batch: Batch = serde_json::from_str(&json).map_err(|e| {
                            Exception::throw_type(&ctx, &format!("invalid batch: {e}"))
                        })?;
                        sink.borrow_mut().extend(batch.ops);
                        Ok(())
                    }
                );
                let sink = ops.clone();
                def!(jsnir, c, sigs, "setText", |id: u16, text: String| -> () {
                    sink.borrow_mut().push(Op::SetText { id, text })
                });
                // Text is transcoded on the way (UTF-8 in Rust, 8 or 16 bits in JS), so it goes per call and per id.
                let read = replica.clone();
                def!(jsnir, c, sigs, "readText", |id: u16| -> Option<String> {
                    read.borrow().view.text(id).map(str::to_owned)
                });
                c.globals().set("jsnir", jsnir.clone())?;
                if let Some(state) = previous {
                    c.globals().set("__jsnirPreviousState", state)?;
                }
                let columns = Columns::install(&c, &jsnir)?;
                configure(&c)?;
                c.eval::<(), _>(js)?;
                Ok(columns)
            })();
            r.catch(&c).map_err(|e| e.to_string())
        })?;
        Ok(Self {
            ctx,
            ops,
            columns,
            signatures: sigs,
            _rt: rt,
        })
    }
    /// Copies the touched nodes of the view (or all of them with `None`) into the shared columns.
    fn sync(&mut self, view: &Tree, ids: Option<&[u16]>) {
        let Self { ctx, columns, .. } = self;
        ctx.with(|c| columns.sync(&c, view, ids));
    }
    fn try_invoke<R: for<'js> rquickjs::FromJs<'js>>(
        &self,
        name: &str,
        args: impl for<'js> IntoArgs<'js>,
    ) -> Result<R, String> {
        self.ctx.with(|c| {
            let r: rquickjs::Result<R> = c
                .globals()
                .get::<_, Function>(name)
                .and_then(|f| f.call(args));
            r.catch(&c).map_err(|e| format!("{name}: {e}"))
        })
    }
    /// Calls a global app function and collects the operations it emitted through `jsnir`.
    /// If the function throws, the operations it already emitted are dropped: a batch is all or nothing.
    fn try_call(&self, name: &str, args: impl for<'js> IntoArgs<'js>) -> Result<Vec<Op>, String> {
        self.ops.borrow_mut().clear();
        let result = self.try_invoke::<()>(name, args);
        let ops = std::mem::take(&mut *self.ops.borrow_mut());
        result.map(|()| ops)
    }
    #[cfg(all(test, feature = "examples"))]
    fn call(&self, name: &str, args: impl for<'js> IntoArgs<'js>) -> Vec<Op> {
        self.try_call(name, args).unwrap_or_else(|e| panic!("{e}"))
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
            create(
                1,
                None,
                Node::Column {
                    children: vec![],
                    style: Default::default(),
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
/// Message from the UI to the app thread: an event, or new code for a live reload.
enum Inbox {
    Event(Event),
    #[cfg(feature = "examples")]
    Reload(String),
}
/// Ordered log from the UI to the app thread's replica.
enum Log {
    Answer(bool),
    Ui(Batch),
}
struct Bridge {
    events: SyncSender<Inbox>,
    updates: Receiver<Batch>,
    // Unbounded so the UI never blocks when it writes to the log.
    log: Sender<Log>,
}
/// Most events the UI keeps in flight; the event queue has the same capacity, so `try_send` cannot fail below it.
const MAX_PENDING: u16 = 64;

/// App logic running on its own thread: JS in a VM, or Rust in reference mode.
trait App {
    fn mount(&mut self) -> Result<Vec<Op>, String>;
    fn handle(&mut self, event: Event, view: &Tree) -> Vec<Op>;
    /// The view changed in these nodes (or in all of them with `None`). JS apps copy it to their columns.
    fn sync(&mut self, _view: &Tree, _ids: Option<&[u16]>) {}
    /// Replaces the app code and returns the new mount. On failure the previous app keeps running.
    #[cfg(feature = "examples")]
    fn reload(&mut self, _js: &str) -> Result<Vec<Op>, String> {
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
) -> Result<(Batch, Bridge), String> {
    let (init, boot) = sync_channel::<Result<Batch, String>>(1);
    let (events, rx) = sync_channel::<Inbox>(MAX_PENDING.into());
    let (tx, updates) = sync_channel(MAX_PENDING.into());
    let (log, entries) = channel();
    thread::spawn(move || {
        let replica: Rc<RefCell<Replica>> = Rc::default();
        let start = || {
            let mut app = make(replica.clone())?;
            let mount = Batch {
                base: 0,
                ops: app.mount()?,
            };
            replica.borrow_mut().produced(&mount)?;
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
            if matches!(message, Inbox::Event(_)) {
                thread::sleep(delay);
            }
            // Catch up with the log. JS is not running, so the columns can be written.
            let (mut ids, mut all) = (Vec::new(), false);
            for entry in entries.try_iter() {
                match entry {
                    Log::Answer(accepted) => all |= replica.borrow_mut().answered(accepted),
                    Log::Ui(batch) => {
                        ids.extend(touched(&batch.ops));
                        replica.borrow_mut().foreign(batch)
                    }
                }
            }
            if all || !ids.is_empty() {
                app.sync(&replica.borrow().view, (!all).then_some(&ids));
            }
            let ops = match message {
                Inbox::Event(event) => app.handle(event, &replica.borrow().view),
                // The new app replaces the old one in a single batch: remove the root, then mount.
                #[cfg(feature = "examples")]
                Inbox::Reload(js) => match app.reload(&js) {
                    Ok(mount) => {
                        let root = replica.borrow().view.root;
                        root.map(|id| Op::Remove { id })
                            .into_iter()
                            .chain(mount)
                            .collect()
                    }
                    Err(e) => {
                        eprintln!("reload rejected, the previous version keeps running: {e}");
                        Vec::new()
                    }
                },
            };
            let batch = Batch {
                base: replica.borrow().view.app_seq,
                ops,
            };
            let produced = replica.borrow_mut().produced(&batch);
            // Removing nodes changes many ids at once: resync all columns.
            let removed = batch.ops.iter().any(|op| matches!(op, Op::Remove { .. }));
            match produced {
                Ok(()) if removed => app.sync(&replica.borrow().view, None),
                Ok(()) => app.sync(&replica.borrow().view, Some(&touched(&batch.ops))),
                Err(e) => eprintln!("invalid batch from the app ({e}): the UI will reject it"),
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
            Mode::Rust => (rust_mount(), None),
            Mode::Js => {
                let (mount, bridge) = spawn_app(delay, |replica| {
                    Ok(Box::new(CounterJs(Vm::counter(replica))))
                })
                .expect("counter failed to mount");
                (mount, Some(bridge))
            }
        };
        let mut tree = Tree::default();
        tree.apply(mount).expect("invalid mount");
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
            self.apply(Batch {
                base: self.tree.app_seq,
                ops: vec![Op::Set {
                    id: self.view.display,
                    value,
                }],
            });
        }
        true
    }
    fn apply(&mut self, batch: Batch) {
        let (base, ops) = (batch.base, batch.ops.len());
        let result = self.tree.apply(batch);
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
                "states",
                "masks",
                "text",
                "fields",
                "spans",
                "borders",
                "scrollbar",
                "overlay",
                "fill",
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
}
