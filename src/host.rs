//! Runs any TSX app: the app thread sends batches, and this UI-side host keeps the tree, lays it out,
//! handles pointer, focus and text input, runs transitions and timers, and turns the tree into a list
//! of drawing items that any backend can paint. JS never runs on this thread.
use crate::{
    App, Bridge, Event, HOST_CALLBACK, Inbox, JsEngine, ListBindings, Log, MAX_PENDING,
    ScriptBundle, Vm,
    editor::{
        EditorBitmapKey, EditorEvent, EditorFrame, EditorOutput, LineEditor, PointSelection,
        SurroundingText, mask_text,
    },
    layout::Layout,
    list::List,
    motion::{Channel, Motion, Value},
    or_log, spawn_app,
    text::{Mark, TextBitmap, TextEngine, TextStyle, WRAP_SLACK, node_spans, plain_text},
    tree::{
        Anchor, AnchorAlign, Batch, Easing, Icon, InputPurpose, Node, Op, Paint,
        PlacementSubscription, Position, Property, Replica, ScrollAnchor, ScrollEdge, Scrollbar,
        Shadow, Side, Span, Transition, Tree, Variant,
    },
};
use rquickjs::{Ctx, Exception, Function, function::MutFn};
use std::{
    cell::RefCell,
    collections::{BTreeSet, HashMap, HashSet, VecDeque},
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::{RecvTimeoutError, TryRecvError},
    },
    time::{Duration, Instant},
};

/// A rectangle in logical pixels: x, y, width, height.
pub type Rect = [f32; 4];
/// Straight (not premultiplied) RGBA.
pub type Rgba = [u8; 4];
/// Corner radii of a rectangle in logical pixels: top-left, top-right, bottom-right, bottom-left (CSS order).
pub type Corners = [f32; 4];

/// `rect`, which is in a text's logical pixels, placed at `origin` in device pixels scaled by `zoom`.
fn scaled(origin: Rect, rect: Rect, zoom: f32) -> Rect {
    [
        origin[0] + rect[0] * zoom,
        origin[1] + rect[1] * zoom,
        rect[2] * zoom,
        rect[3] * zoom,
    ]
}

/// What to draw, in painting order. Rectangles are already transformed and clipped by `clip`, which is
/// axis-aligned. `round_clip` is the rounded rectangle and corner radius of the innermost ancestor clip
/// with a radius: the item is also cut to its corners.
#[derive(Debug, Clone)]
pub enum Item {
    /// Starts a group drawn into its own layer and composited with `opacity`, as CSS `opacity` does.
    PushLayer {
        opacity: f32,
    },
    PopLayer,
    /// Outer shadow of a rounded rectangle; it is not drawn under the rectangle itself. Not masked. Its blur uses
    /// the largest corner radius: a shadow of unequal corners is approximated.
    Shadow {
        rect: Rect,
        radius: f32,
        shadow: ShadowSpec,
        clip: Rect,
        round_clip: Option<(Rect, Corners)>,
    },
    /// Rounded rectangle filled with a vertical gradient (`top` = `bottom` for a solid color).
    /// `mask` is the image and the rectangle it is stretched to.
    Fill {
        rect: Rect,
        radius: Corners,
        top: Rgba,
        bottom: Rgba,
        clip: Rect,
        round_clip: Option<(Rect, Corners)>,
        mask: Option<(ImageData, Rect)>,
    },
    /// Inner shadow without blur: the part of the shape not covered by itself inset by `spread` and
    /// moved by `offset` (a ring with spread, a top highlight with offset).
    Inset {
        rect: Rect,
        radius: Corners,
        spread: f32,
        offset: [f32; 2],
        color: Rgba,
        clip: Rect,
        round_clip: Option<(Rect, Corners)>,
        mask: Option<(ImageData, Rect)>,
    },
    /// Text laid out in `rect` at `zoom` times the device scale. `spans` are the rich text's runs, already
    /// cut to the node's line limit; `text` is their plain text.
    Text {
        rect: Rect,
        text: String,
        spans: Option<Vec<Span>>,
        style: TextStyle,
        wrap: bool,
        zoom: f32,
        clip: Rect,
        round_clip: Option<(Rect, Corners)>,
    },
    Icon {
        rect: Rect,
        icon: Icon,
        stroke: f32,
        draw: f32,
        color: Rgba,
        zoom: f32,
        clip: Rect,
        round_clip: Option<(Rect, Corners)>,
    },
    /// An image the app named, stretched to `rect`, rounded by `radius` and multiplied by `tint`.
    Image {
        rect: Rect,
        radius: Corners,
        image: ImageData,
        tint: Rgba,
        clip: Rect,
        round_clip: Option<(Rect, Corners)>,
        mask: Option<(ImageData, Rect)>,
    },
    /// The focused field's text, selection and caret.
    Editor {
        rect: Rect,
        frame: Box<EditorFrame>,
        caret: Option<Rect>,
        selection: Rgba,
        caret_color: Rgba,
        clip: Rect,
        round_clip: Option<(Rect, Corners)>,
    },
}

/// Pixels set with `Host::set_image`. Straight RGBA8, shared, so a frame copies no pixels.
#[derive(Debug, Clone)]
pub struct ImageData {
    pub rgba: Arc<[u8]>,
    pub size: [u32; 2],
    /// Changes with every `set_image`, so a backend that caches by it uploads a replaced image again.
    pub generation: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct ShadowSpec {
    pub y: f32,
    pub blur: f32,
    pub spread: f32,
    pub color: Rgba,
}

pub use crate::tree::Cursor;

/// Error shown in the native development overlay while the previous valid app stays running underneath.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevError {
    pub title: String,
    pub message: String,
    pub location: Option<String>,
    pub frame: Option<String>,
}

/// Cross-platform IME state for the currently focused field (Linux Wayland/X11, Windows TSF, macOS).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImeState {
    /// Whether platform IME composition should be enabled (false when unfocused or focused on a password field).
    pub allowed: bool,
    /// Semantic purpose of the focused input.
    pub purpose: InputPurpose,
    /// Window of text surrounding the caret/selection (at most 4000 UTF-8 bytes) when `allowed` is true.
    pub surrounding: Option<SurroundingText>,
}

pub struct Frame {
    pub items: Vec<Item>,
    /// Keep drawing: a transition, or a keyframe animation on screen, is running.
    pub animating: bool,
    /// Caret of the focused field, for the platform's input method window.
    pub ime_area: Option<Rect>,
}

// Adds the host's globals to a VM before its script runs (`jsnirRequest`).
type Configure = Box<dyn for<'js> Fn(&Ctx<'js>) -> rquickjs::Result<()> + Send>;

// The bridge carries placement on the host-message channel with a prefix that cannot start a JSON message.
// It must not enter the ordinary callback dispatcher before its mount token has been checked.
const PLACEMENT_PREFIX: &str = "\0placement:";

// The app in QuickJS (or another `JsEngine`): `jsnirMount` once, `jsnirEvent` / typed numeric handlers per event.
// A reload makes a new VM with the same replica, startup arguments and globals.
struct HostApp {
    vm: Box<dyn JsEngine>,
    replica: Rc<RefCell<Replica>>,
    args: String,
    configure: Configure,
    placement_epoch: u64,
}
impl App for HostApp {
    fn mount(&mut self) -> Result<Vec<Op>, String> {
        self.vm.call_mount()
    }
    fn handle(&mut self, e: Event, _: &Tree) -> Vec<Op> {
        if e.callback == HOST_CALLBACK
            && let Some(text) = e.text.strip_prefix(PLACEMENT_PREFIX)
        {
            return or_log(self.vm.call_placement(text));
        }
        or_log(self.vm.call_event(e.callback, &e.text, None))
    }
    fn guarded(&mut self, e: Event, guard: String) -> Vec<Op> {
        or_log(self.vm.call_event(e.callback, &e.text, Some(&guard)))
    }
    fn handle_drag(
        &mut self,
        callback: u16,
        guard: Option<String>,
        phase: u8,
        x: f32,
        y: f32,
        sx: f32,
        sy: f32,
        dx: f32,
        dy: f32,
    ) -> Vec<Op> {
        or_log(
            self.vm
                .call_drag(callback, phase, x, y, sx, sy, dx, dy, guard.as_deref()),
        )
    }
    fn handle_scroll(
        &mut self,
        callback: u16,
        guard: Option<String>,
        top: f32,
        height: f32,
        client: f32,
    ) -> Vec<Op> {
        or_log(
            self.vm
                .call_scroll(callback, top, height, client, guard.as_deref()),
        )
    }
    fn handle_context_menu(
        &mut self,
        callback: u16,
        guard: Option<String>,
        x: f32,
        y: f32,
    ) -> Vec<Op> {
        or_log(self.vm.call_context_menu(callback, x, y, guard.as_deref()))
    }
    fn list(&mut self, delta: String) -> Result<(ListBindings, Vec<Op>), String> {
        let (reply, ops) = self.vm.call_virtual_list(&delta)?;
        let pairs =
            serde_json::from_str::<Vec<(String, u16)>>(&reply).map_err(|e| e.to_string())?;
        let bindings = pairs
            .into_iter()
            .map(|(key, row)| {
                let number = key.parse::<u64>().map_err(|_| "invalid virtual row key")?;
                if number.to_string() != key {
                    return Err("invalid virtual row key");
                }
                Ok((number, row))
            })
            .collect::<Result<_, _>>()?;
        Ok((bindings, ops))
    }
    fn sync(&mut self, view: &Tree, ids: Option<&[u16]>) {
        self.vm.sync(view, ids)
    }
    fn reload(&mut self, js: &str, preserve_state: bool) -> Result<Vec<Op>, String> {
        // The old VM's signals carry over to the new one unless this is a hard reload. If the new script fails to load, the old one keeps running.
        let state = preserve_state.then(|| self.vm.reload_state()).flatten();
        let placement_epoch = self.placement_epoch + 1;
        let vm = Vm::try_load_with(js, self.replica.clone(), state.as_deref(), |c| {
            c.globals().set("__jsnirArgs", self.args.as_str())?;
            c.globals()
                .set("__jsnirPlacementEpoch", placement_epoch.to_string())?;
            (self.configure)(c)
        })?;
        let mount = vm.call_mount()?;
        self.vm = Box::new(vm);
        self.placement_epoch = placement_epoch;
        Ok(mount)
    }
}

// Where a node is in the tree: its child index at each level from the root, and its kind. A reload finds the
// nodes it keeps again by this place.
type Place = (Vec<usize>, std::mem::Discriminant<Node>);

// Native state of the tree that a reload keeps, by place.
struct Kept {
    // The focused node, with its field's text and selection.
    focused: Option<(Place, String, [u32; 2])>,
    // Scroll offsets, including the fields' own.
    scroll: Vec<(Place, f32)>,
}

// Something under the pointer that reacts to it, recorded while drawing: a pressable box, or the root of a
// layer (which covers what is below it even when it reacts to nothing).
struct Hit {
    rect: Rect,
    id: u16,
    layer: i32,
}

// A node with an explicit cursor override, recorded while drawing alongside its position after `hits`.
struct CursorHit {
    rect: Rect,
    id: u16,
    layer: i32,
    after_hits: usize,
}

// An active primary-button drag on a `Node::Box` with a `drag` callback.
#[derive(Clone, Copy)]
struct NodeDrag {
    id: u16,
    start: [f32; 2],
    last: [f32; 2],
    moved: bool,
}

// Kind of each in-flight message sent to the app thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Inflight {
    Event,
    DragMove,
    Reload(bool),
}

// A `scrollTo` op, applied by the frame that draws its node.
struct ScrollRequest {
    offset: Option<f32>,
    to: Option<ScrollEdge>,
    smooth: bool,
}

// Where an end-anchored scroll node was in its last frame. `child` is the first child whose bottom is in view, and
// `gap` is its top minus the offset: the offset that keeps it where it was is its top minus `gap`.
struct EndAnchor {
    child: Option<u16>,
    gap: f32,
    offset: f32,
    bottom: bool,
}

// A scroll node drawn in the last frame, in painting order (an outer one comes first).
struct Scroll {
    id: u16,
    layer: i32,
    // Visible part of its rectangle on screen.
    rect: Rect,
    // Scale of its content on screen, to turn screen pixels into logical ones.
    k: f32,
    // Offset in logical pixels.
    top: f32,
    // Largest offset: content height minus box height.
    max: f32,
    // Height of the box in logical pixels: a click on the track pages by this much.
    view: f32,
    // Track and thumb of its scrollbar on screen, if it has one.
    bar: Option<(Rect, Rect)>,
    // Automatic Lists use indexed f64 coordinates rather than the legacy columns above.
    list_generation: Option<u64>,
}

enum WheelOffset {
    Legacy(f32),
    Virtual(ListOffset),
}

/// Identifies one input sequence on the Host that issued it. Replacement invalidates the old handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtualListId {
    issuer: u64,
    node: u16,
    generation: u64,
}

impl VirtualListId {
    fn matches(self, issuer: u64, generation: u64) -> bool {
        self.issuer == issuer && self.generation == generation
    }
}

/// A requested scroll position in logical pixels.
#[derive(Debug, Clone, Copy)]
pub enum ListOffset {
    /// Clamp an absolute offset and preserve its row anchor on future edits.
    Pixels(f64),
    /// Stay at the end until the next explicit pixel offset.
    Tail,
}

/// Keyed edits to a Rust-owned list. Heights are positive, finite logical pixels.
#[derive(Debug, Clone, Copy)]
pub enum ListMutation {
    Append(u64, f32),
    Prepend(u64, f32),
    /// Invalidate the row's content even if its new height equals its old height.
    Update(u64, f32),
    Remove(u64),
}

/// A delta relative to the last demand taken by the consumer, not a resident snapshot.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ListDemand {
    /// Newly visible keys in list order.
    pub entering: Vec<u64>,
    /// Edited keys that remain visible, in key order.
    pub changed: Vec<u64>,
    /// Keys that left the window, in key order.
    pub evicted: Vec<u64>,
    /// Drop all residency for this previous generation before processing the delta.
    pub retired: Option<VirtualListId>,
}

type RowProvider = Box<dyn FnMut(u64) -> Result<serde_json::Value, String> + Send>;

#[derive(Default)]
struct VirtualList {
    provider: Option<RowProvider>,
    measured: bool,
    rows: List,
    bindings: HashMap<u64, u16>,
    follow_tail: bool,
    auto: Option<AutoViewport>,
    window_dirty: bool,
    delivered: Option<VirtualListId>,
    resident: HashSet<u64>,
    edited: BTreeSet<u64>,
    reply: ListReplyGate,
}

// The existing window cursor is speculative only while `pending` is set. No
// subsequent proposal can consume it until the batch is accepted. On failure
// it is never reused: the caller must rebuild the VM and mount a fresh list.
#[derive(Default)]
struct ListReplyGate {
    accepted: u64,
    pending: Option<PendingListReply>,
    failed: bool,
    removed: BTreeSet<u64>,
}

impl ListReplyGate {
    fn active(&self) -> bool {
        self.accepted != 0 || self.pending.is_some() || self.failed
    }
}

struct PendingListReply {
    id: ListReplyId,
    delta: ListDemand,
    invalidated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ListReplyId {
    // Generations are Host-unique, including when the same mount ID is recycled.
    list: VirtualListId,
    vm: u64,
    serial: u64,
}

#[derive(Debug, PartialEq, Eq)]
enum ListReplyError {
    Stale,
    Rebuild,
}

#[derive(Default)]
struct AutoViewport {
    ready: bool,
    requested: Option<ListOffset>,
    // Scratch geometry for this render pass; commit only after the final pass.
    preview: Option<(f64, f64)>,
}

impl VirtualList {
    fn max_offset(&self, extent: f64) -> f64 {
        (self.rows.total() - extent).max(0.)
    }

    fn offset_for(&self, requested: Option<ListOffset>, extent: f64) -> f64 {
        let max = self.max_offset(extent);
        match requested {
            Some(ListOffset::Tail) => max,
            Some(ListOffset::Pixels(pixels)) => pixels.clamp(0., max),
            None if self.follow_tail => max,
            None => self.rows.offset().min(max),
        }
    }

    fn preview_layout(&mut self, height: f32) -> (f64, f64) {
        let extent = f64::from(height);
        let preview = self
            .auto
            .as_ref()
            .map(|auto| (self.offset_for(auto.requested, extent), extent));
        if let Some(auto) = &mut self.auto {
            auto.preview = preview;
        }
        self.frame_viewport(height)
    }

    fn frame_viewport(&self, height: f32) -> (f64, f64) {
        self.auto.as_ref().and_then(|auto| auto.preview).unwrap_or((
            self.rows.offset(),
            self.rows.extent().min(f64::from(height)),
        ))
    }

    fn apply_viewport(&mut self, pixels: f64, extent: f64) -> bool {
        let before = (self.rows.offset(), self.rows.extent());
        if !self.rows.set_viewport(pixels, extent) {
            return false;
        }
        self.window_dirty |= before != (self.rows.offset(), self.rows.extent());
        true
    }
}

enum EventPayload {
    Text(String),
    Drag {
        phase: u8,
        x: f32,
        y: f32,
        sx: f32,
        sy: f32,
        dx: f32,
        dy: f32,
    },
    Scroll {
        top: f32,
        height: f32,
        client: f32,
    },
    ContextMenu {
        x: f32,
        y: f32,
    },
}

struct QueuedEvent {
    callback: u16,
    payload: EventPayload,
    guard: Option<String>,
}

impl QueuedEvent {
    fn into_inbox(self) -> Inbox {
        match self.payload {
            EventPayload::Text(text) => {
                let event = Event {
                    callback: self.callback,
                    text,
                };
                match self.guard {
                    Some(guard) => Inbox::Guarded(event, guard),
                    None => Inbox::Event(event),
                }
            }
            EventPayload::Drag {
                phase,
                x,
                y,
                sx,
                sy,
                dx,
                dy,
            } => Inbox::Drag {
                callback: self.callback,
                guard: self.guard,
                phase,
                x,
                y,
                sx,
                sy,
                dx,
                dy,
            },
            EventPayload::Scroll {
                top,
                height,
                client,
            } => Inbox::Scroll {
                callback: self.callback,
                guard: self.guard,
                top,
                height,
                client,
            },
            EventPayload::ContextMenu { x, y } => Inbox::ContextMenu {
                callback: self.callback,
                guard: self.guard,
                x,
                y,
            },
        }
    }
}

pub struct Host {
    pub tree: Tree,
    list_mounts: HashMap<u16, String>,
    list_ticket: Option<ListReplyId>,
    list_error: Option<String>,
    callback_guards: HashMap<u16, String>,
    virtual_lists: HashMap<u16, (u64, VirtualList)>,
    bound_rows: HashMap<u16, (u16, u64)>,
    list_issuer: u64,
    list_generation: u64,
    list_vm: u64,
    bridge: Bridge,
    pending: u16,
    // Bounded wait budget for `poll()`: lets fast JS replies land in the same frame without holding slow frames.
    wait_budget: Duration,
    sent_at: Option<Instant>,
    layout: Layout,
    // Nodes with a height transition, height variant, or height keyframe animation, tracked incrementally.
    sized_nodes: HashSet<u16>,
    // Cached per-node layout subtree bounds and conservative "cannot cull" flags, rebuilt only on tree/layout changes.
    cull_cache_dirty: bool,
    cull_bounds: Vec<[f32; 4]>,
    cull_special: Vec<bool>,
    // Previous frame's `Frame.items.len()`, used to pre-size `items` in one allocation.
    last_items_len: usize,
    text: TextEngine,
    motion: Motion,
    clock: Instant,
    // Timers from `after` operations: deadline in ms on `clock`, and the callback.
    timers: Vec<(f64, QueuedEvent)>,
    // Events that did not fit in the queue, sent on the next poll.
    backlog: Vec<QueuedEvent>,
    pointer: Option<[f32; 2]>,
    hovered: Option<u16>,
    pressed: Option<u16>,
    focused: Option<u16>,
    // Group nodes drawn in the last frame with their visible rectangles and layers, and the ones the pointer is in.
    group_rects: Vec<(u16, Rect, i32)>,
    hovered_groups: Vec<u16>,
    // Boxes with a `hover` or `dismiss` callback drawn in the last frame, the ones the pointer is in, and the
    // boxes it entered or left since (their callbacks have been told).
    hover_rects: Vec<(u16, Rect, i32)>,
    hovered_boxes: Vec<u16>,
    // Hover is kept as it is until this time on `clock`: the page is scrolling, or has been until then.
    hover_frozen_until: f64,
    dismiss_rects: Vec<(u16, Rect, i32)>,
    // On-screen rectangle of each node in the last frame, by id, unclipped: what a fixed node anchors to.
    screen: Vec<Option<Rect>>,
    // Group nodes the pointer was in when the primary button went down.
    pressed_groups: Vec<u16>,
    editor: LineEditor,
    editor_key: Option<EditorBitmapKey>,
    // A press inside the focused field extends the selection while the pointer moves.
    editor_dragging: bool,
    published: (String, [u32; 2]),
    hits: Vec<Hit>,
    cursor_rects: Vec<CursorHit>,
    scrolls: Vec<Scroll>,
    placements: HashMap<u16, Placement>,
    viewport_reporting: bool,
    reported_viewport: Option<[f32; 2]>,
    // Offset of each scroll node in logical pixels; a node without an entry is at its top.
    scroll: HashMap<u16, f32>,
    // `scrollTo` ops that the next frame drawing their node applies.
    scroll_requests: HashMap<u16, ScrollRequest>,
    anchors: HashMap<u16, EndAnchor>,
    // A scrollbar thumb being dragged: its scroll node, and how far below the thumb's top the pointer grabbed it.
    drag: Option<(u16, f32)>,
    // A `Node::Box` with a `drag` callback currently being dragged by the primary button.
    node_drag: Option<NodeDrag>,
    // Latest pointer position waiting to be sent as a drag `"move"` once the previous one is answered.
    pending_drag_move: Option<[f32; 2]>,
    drag_moves_inflight: u16,
    images: HashMap<String, ImageData>,
    image_generation: u64,
    // Changes with every font change: a backend drops the text bitmaps it made with the old fonts.
    font_epoch: u64,
    /// Hides the caret, for reproducible captures.
    pub hide_caret: bool,
    /// Captures only: keyframe animations show their value this many ms after their own start, and none keeps
    /// running. None: they run live.
    pub keyframes_at: Option<f32>,
    // Reduced motion: infinite keyframe animations do not run, and their nodes show their base values.
    reduced_motion: bool,
    // Kind of each message sent to the app and not answered yet.
    inflight: VecDeque<Inflight>,
    // Outcomes of the reloads that were applied, oldest first, until `take_reload_result`.
    reload_results: VecDeque<Result<(), String>>,
    // Native development error overlay shown above the app tree when compilation or reload fails.
    dev_error: Option<DevError>,
    dev_overlay_enabled: bool,
}

const HEIGHT_TRANSITION: Property = Property::Height;
// Hover is not updated while the page scrolls: Chrome keeps the old hover state during a scroll and dispatches a
// synthetic mouse move once the scroll has been idle for about 100 ms.
const SCROLL_HOVER_IDLE_MS: f64 = 100.;

impl Host {
    /// Starts `script` (SDK + app) on its own thread. `args` is a JSON value the app reads with `args()`.
    pub fn new(script: String, args: String, text: TextEngine) -> Self {
        Self::try_new(script, args, text)
            .unwrap_or_else(|error| panic!("app failed to mount: {error}"))
    }

    /// Starts an app and returns an error if the script fails to load or mount.
    pub fn try_new(script: String, args: String, text: TextEngine) -> Result<Self, String> {
        Self::start(ScriptBundle::Source(script), args, text, |_| Ok(()))
    }

    /// Starts an app from precompiled QuickJS bytecode (`JSNRBC01`, built with [`crate::compile_bytecode`]).
    pub fn try_new_bytecode(
        bytecode: Vec<u8>,
        args: String,
        text: TextEngine,
    ) -> Result<Self, String> {
        Self::start(ScriptBundle::Bytecode(bytecode), args, text, |_| Ok(()))
    }

    /// Starts an app from raw bytes, automatically detecting precompiled QuickJS bytecode (`JSNRBC01`)
    /// or UTF-8 JavaScript source.
    pub fn try_new_auto(bytes: Vec<u8>, args: String, text: TextEngine) -> Result<Self, String> {
        Self::start(ScriptBundle::from_bytes(bytes)?, args, text, |_| Ok(()))
    }

    /// Configures how long `poll()` may wait for a just-sent event's reply before returning to draw the frame.
    /// Defaults to 800 µs (or `JSNIR_WAIT_US`). Pass `Duration::ZERO` to disable bounded waiting.
    pub fn set_wait_budget(&mut self, budget: Duration) {
        self.wait_budget = budget;
    }

    /// Exposes `jsnirRequest(json)` to an embedded app. The handler runs on the app thread.
    pub fn with_request_handler(
        script: String,
        args: String,
        text: TextEngine,
        handler: impl FnMut(String) -> Result<String, String> + Send + 'static,
    ) -> Result<Self, String> {
        // Shared by every VM the app runs in, so a reloaded app keeps the same handler.
        let handler = Arc::new(Mutex::new(handler));
        Self::start(ScriptBundle::Source(script), args, text, move |ctx| {
            let handler = handler.clone();
            let request = Function::new(
                ctx.clone(),
                MutFn::new(move |ctx: Ctx<'_>, json: String| {
                    let mut handler = handler.lock().expect("request handler");
                    (*handler)(json).map_err(|error| Exception::throw_message(&ctx, &error))
                }),
            )?;
            ctx.globals().set("jsnirRequest", request)
        })
    }

    fn start(
        bundle: ScriptBundle,
        args: String,
        text: TextEngine,
        configure: impl for<'js> Fn(&Ctx<'js>) -> rquickjs::Result<()> + Send + 'static,
    ) -> Result<Self, String> {
        // Never recycle issuers: a handle may outlive its Host. Only uniqueness needs synchronization.
        static NEXT_LIST_ISSUER: AtomicU64 = AtomicU64::new(0);
        let list_issuer = NEXT_LIST_ISSUER
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| "virtual list issuer IDs exhausted".to_owned())?;
        let wait_budget = std::env::var("JSNIR_WAIT_US")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_micros)
            .unwrap_or(Duration::from_micros(800));
        let (mount, bridge) = spawn_app(
            Duration::ZERO,
            move |replica: Rc<RefCell<Replica>>| -> Result<Box<dyn App>, String> {
                let vm = Vm::try_load_bundle(&bundle, replica.clone(), None, |c| {
                    c.globals().set("__jsnirArgs", args.as_str())?;
                    configure(c)
                })
                .map_err(|error| format!("script failed to load: {error}"))?;
                Ok(Box::new(HostApp {
                    vm: Box::new(vm),
                    replica,
                    args,
                    configure: Box::new(configure),
                    placement_epoch: 0,
                }))
            },
        )?;
        let mut host = Self {
            tree: Tree::default(),
            list_mounts: HashMap::new(),
            list_ticket: None,
            list_error: None,
            callback_guards: HashMap::new(),
            virtual_lists: HashMap::new(),
            bound_rows: HashMap::new(),
            list_issuer,
            list_generation: 0,
            list_vm: 0,
            bridge,
            pending: 0,
            wait_budget,
            sent_at: None,
            layout: Layout::default(),
            sized_nodes: HashSet::new(),
            cull_cache_dirty: true,
            cull_bounds: Vec::new(),
            cull_special: Vec::new(),
            last_items_len: 0,
            text,
            motion: Motion::default(),
            clock: Instant::now(),
            timers: Vec::new(),
            backlog: Vec::new(),
            pointer: None,
            hovered: None,
            pressed: None,
            focused: None,
            group_rects: Vec::new(),
            hovered_groups: Vec::new(),
            hover_rects: Vec::new(),
            hovered_boxes: Vec::new(),
            hover_frozen_until: 0.,
            dismiss_rects: Vec::new(),
            screen: Vec::new(),
            pressed_groups: Vec::new(),
            editor: LineEditor::default(),
            editor_key: None,
            editor_dragging: false,
            published: Default::default(),
            hits: Vec::new(),
            cursor_rects: Vec::new(),
            scrolls: Vec::new(),
            placements: HashMap::new(),
            viewport_reporting: false,
            reported_viewport: None,
            scroll: HashMap::new(),
            scroll_requests: HashMap::new(),
            anchors: HashMap::new(),
            drag: None,
            node_drag: None,
            pending_drag_move: None,
            drag_moves_inflight: 0,
            images: HashMap::new(),
            image_generation: 0,
            font_epoch: 0,
            hide_caret: false,
            keyframes_at: None,
            reduced_motion: false,
            inflight: VecDeque::new(),
            reload_results: VecDeque::new(),
            dev_error: None,
            dev_overlay_enabled: false,
        };
        host.receive(mount);
        if host.tree.root.is_none() {
            return Err("the app did not mount a root node".into());
        }
        Ok(host)
    }

    /// Connects a uniquely named SDK `VirtualList` mount to an indexed Rust history.
    /// `provide` runs on the Host thread and must be cheap and nonblocking. It is called only
    /// for visible entering or edited keys. Reconnect with the current index after a remount.
    pub fn connect_virtual_list(
        &mut self,
        name: &str,
        rows: impl IntoIterator<Item = (u64, f32)>,
        provide: impl FnMut(u64) -> Result<serde_json::Value, String> + Send + 'static,
    ) -> Result<VirtualListId, String> {
        if let Some(error) = &self.list_error {
            return Err(error.clone());
        }
        let mut nodes = self
            .list_mounts
            .iter()
            .filter(|(_, mounted)| *mounted == name);
        let node = *nodes
            .next()
            .ok_or_else(|| format!("no virtual List mounted as {name:?}"))?
            .0;
        if nodes.next().is_some() || self.virtual_lists.contains_key(&node) {
            return Err(format!(
                "virtual List {name:?} is ambiguous or already connected"
            ));
        }
        let id = self
            .replace_auto_virtual_list(node, rows)
            .ok_or_else(|| format!("invalid index for virtual List {name:?}"))?;
        self.virtual_list_mut(id).unwrap().provider = Some(Box::new(provide));
        Ok(id)
    }

    /// A failed wire reply freezes app events. Recover with a successful reload and reconnect
    /// the new mounts. An unsuccessful reload does not clear the error.
    pub fn virtual_list_error(&self) -> Option<&str> {
        self.list_error.as_deref()
    }

    /// True while the SDK mount for this handle remains active.
    pub fn virtual_list_mounted(&self, id: VirtualListId) -> bool {
        self.virtual_lists.get(&id.node).is_some_and(|(generation, _)| id.matches(self.list_issuer, *generation))
    }

    /// Like `connect_virtual_list`, with the SDK row's natural height measured at the current width.
    pub fn connect_measured_virtual_list(
        &mut self,
        name: &str,
        rows: impl IntoIterator<Item = (u64, f32)>,
        provide: impl FnMut(u64) -> Result<serde_json::Value, String> + Send + 'static,
    ) -> Result<VirtualListId, String> {
        let id = self.connect_virtual_list(name, rows, provide)?;
        self.virtual_list_mut(id).unwrap().measured = true;
        self.set_list_offset(id, ListOffset::Tail);
        Ok(id)
    }

    /// Update the index and mark changed payloads while retaining mounted rows that belong to the new window.
    /// Returns false if the list reply lane is in flight, allowing the embedder to retry on the next frame.
    pub fn refresh_virtual_list(&mut self, id: VirtualListId, index: Option<&[(u64, f32)]>, changed: &[u64]) -> bool {
        let Some(list) = self.virtual_list_mut(id) else { return false };
        if list.reply.pending.is_some() { return false; }
        let mut removed = Vec::new();
        if let Some(index) = index {
            let Some(rows) = list.rows.reordered(index) else { return false };
            removed.extend(list.bindings.keys().copied().filter(|key| !rows.contains(*key)));
            list.rows = rows;
            if list.follow_tail { list.rows.set_viewport(list.rows.total(), list.rows.extent()); }
            list.window_dirty = true;
            if let Some(auto) = &mut list.auto { auto.ready = false; }
        }
        list.edited.extend(changed.iter().filter(|key| list.resident.contains(key)).copied());
        for key in removed { self.unbind_virtual_row(id, key); }
        true
    }

    /// Opts an existing List node into Rust-side demand, or replaces its input sequence.
    /// Duplicate keys or invalid heights reject the entire replacement. The new viewport starts
    /// empty and old row bindings are dropped. Only bound children contribute to its frame.
    /// Removing the node (including by reload) ends its residency and invalidates the handle.
    pub fn replace_virtual_list(
        &mut self,
        node: u16,
        rows: impl IntoIterator<Item = (u64, f32)>,
    ) -> Option<VirtualListId> {
        if !matches!(self.tree.nodes.get(&node), Some(Node::List { .. }))
            || self
                .virtual_lists
                .get(&node)
                .is_some_and(|(_, list)| list.provider.is_some())
        {
            return None;
        }
        let mut list = List::default();
        for (key, height) in rows {
            if !list.append(key, height) {
                return None;
            }
        }
        let generation = self.list_generation.checked_add(1)?;
        self.list_generation = generation;
        if let Some((_, list)) = self.virtual_lists.get_mut(&node) {
            for row in std::mem::take(&mut list.bindings).into_values() {
                self.detach_virtual_row(row);
            }
        }
        let entry = self.virtual_lists.entry(node).or_default();
        *entry = (
            generation,
            VirtualList {
                rows: list,
                delivered: entry.1.delivered.filter(|_| !entry.1.reply.active()),
                window_dirty: true,
                ..Default::default()
            },
        );
        Some(VirtualListId {
            issuer: self.list_issuer,
            node,
            generation,
        })
    }

    /// Like `replace_virtual_list`, but the completed frame supplies the List box height.
    /// Starts at the top. Wheel input affects the next frame; reaching the end by wheel pins it.
    /// Both pixel and line wheels move this mode directly, without a legacy scroll glide.
    pub fn replace_auto_virtual_list(
        &mut self,
        node: u16,
        rows: impl IntoIterator<Item = (u64, f32)>,
    ) -> Option<VirtualListId> {
        let id = self.replace_virtual_list(node, rows)?;
        self.virtual_list_mut(id)?.auto = Some(AutoViewport::default());
        Some(id)
    }

    /// Queues an offset for the next laid-out frame of an automatic list. A pixel request
    /// unpins the tail even if it clamps to the end. Invalid values and other handles are ignored.
    pub fn set_list_offset(&mut self, id: VirtualListId, offset: ListOffset) -> bool {
        if matches!(offset, ListOffset::Pixels(pixels) if !pixels.is_finite()) {
            return false;
        }
        let Some(auto) = self
            .virtual_list_mut(id)
            .and_then(|list| list.auto.as_mut())
        else {
            return false;
        };
        auto.requested = Some(offset);
        true
    }

    /// Bring a keyed row to the top of the viewport, clamped by remaining content.
    pub fn scroll_list_to_row(&mut self, id: VirtualListId, key: u64) -> bool {
        let Some(offset) = self.virtual_list_mut(id).and_then(|list| list.rows.top(key)) else { return false };
        self.set_list_offset(id, ListOffset::Pixels(offset))
    }

    /// Binds an existing direct child to an indexed key, one child per key and one key per child.
    /// Repeating the same binding is a no-op. Binding does not change demand or tree ownership.
    pub fn bind_virtual_row(&mut self, id: VirtualListId, key: u64, row: u16) -> bool {
        if self.tree.parent(row) != Some(id.node) {
            return false;
        }
        let already_bound = self.bound_rows.contains_key(&row);
        let Some(list) = self.virtual_list_mut(id) else {
            return false;
        };
        if let Some(&bound) = list.bindings.get(&key) {
            return bound == row;
        }
        if already_bound || !list.rows.contains(key) {
            return false;
        }
        list.bindings.insert(key, row);
        self.bound_rows.insert(row, (id.node, key));
        self.layout.attach_row(&self.tree, row);
        true
    }

    /// Drops the binding without removing the child from the app's tree.
    pub fn unbind_virtual_row(&mut self, id: VirtualListId, key: u64) -> bool {
        let Some(row) = self
            .virtual_list_mut(id)
            .and_then(|list| list.bindings.remove(&key))
        else {
            return false;
        };
        self.detach_virtual_row(row);
        true
    }

    fn detach_virtual_row(&mut self, row: u16) {
        self.bound_rows.remove(&row);
        self.layout.detach_row(&self.tree, row);
        self.forget_row_input(&self.tree.subtree(row));
    }

    // A new binding must not inherit the previous binding's last-frame pointer geometry.
    fn forget_row_input(&mut self, ids: &[u16]) {
        let ids: HashSet<_> = ids.iter().copied().collect();
        for &id in &ids {
            if let Some(rect) = self.screen.get_mut(id as usize) {
                *rect = None;
            }
        }
        self.hits.retain(|hit| !ids.contains(&hit.id));
        self.cursor_rects.retain(|cursor| !ids.contains(&cursor.id));
        self.scrolls.retain(|scroll| !ids.contains(&scroll.id));
        for rects in [
            &mut self.group_rects,
            &mut self.hover_rects,
            &mut self.dismiss_rects,
        ] {
            rects.retain(|(id, _, _)| !ids.contains(id));
        }
        for active in [&mut self.focused, &mut self.pressed, &mut self.hovered] {
            if active.is_some_and(|id| ids.contains(&id)) {
                *active = None;
            }
        }
        self.hovered_groups.retain(|id| !ids.contains(id));
        self.pressed_groups.retain(|id| !ids.contains(id));
        self.hovered_boxes.retain(|id| !ids.contains(id));
        if self.drag.is_some_and(|(id, _)| ids.contains(&id)) {
            self.drag = None;
        }
        if self.node_drag.is_some_and(|drag| ids.contains(&drag.id)) {
            self.node_drag = None;
            self.pending_drag_move = None;
        }
    }

    fn row_can_receive(&self, child: u16) -> bool {
        self.row_input_at(child, None)
    }

    fn row_input_at(&self, mut child: u16, point: Option<[f32; 2]>) -> bool {
        if self.virtual_lists.is_empty() {
            return true;
        }
        while let Some(parent) = self.tree.parent(child) {
            if let Some((_, list)) = self.virtual_lists.get(&parent) {
                let height = self.tree.rect(parent)[3];
                let (offset, extent) = list.frame_viewport(height);
                if !self.bound_rows.get(&child).is_some_and(|&(_, key)| {
                    list.rows.visible_row_at(key, offset, extent).is_some()
                }) {
                    return false;
                }
                // Old hit rectangles retain all last-frame clips. Also apply the current
                // viewport in the last-drawn list coordinates, even before another frame.
                if let Some(point) = point {
                    let Some(mut clip) = self.screen.get(parent as usize).copied().flatten() else {
                        return false;
                    };
                    clip[3] *= (extent / f64::from(height)) as f32;
                    if !contains(clip, point) {
                        return false;
                    }
                }
            }
            child = parent;
        }
        true
    }

    fn virtual_list_mut(&mut self, id: VirtualListId) -> Option<&mut VirtualList> {
        let (generation, list) = self.virtual_lists.get_mut(&id.node)?;
        id.matches(self.list_issuer, *generation).then_some(list)
    }

    /// Supplies the viewport of a manual list in logical pixels. Invalid values and other handles are ignored.
    pub fn set_list_viewport(
        &mut self,
        id: VirtualListId,
        offset: ListOffset,
        extent: f64,
    ) -> bool {
        let Some(list) = self.virtual_list_mut(id) else {
            return false;
        };
        if list.auto.is_some() {
            return false;
        }
        let pixels = match offset {
            ListOffset::Pixels(pixels) => pixels,
            ListOffset::Tail => list.rows.total(),
        };
        if !list.apply_viewport(pixels, extent) {
            return false;
        }
        list.follow_tail = matches!(offset, ListOffset::Tail);
        true
    }

    /// The clamped or anchored logical offset of the current generation.
    pub fn list_offset(&self, id: VirtualListId) -> Option<f64> {
        let (generation, list) = self.virtual_lists.get(&id.node)?;
        id.matches(self.list_issuer, *generation)
            .then(|| list.rows.offset())
    }

    /// Applies a keyed edit. Invalid edits and stale handles leave the list and demand unchanged.
    pub fn mutate_list(&mut self, id: VirtualListId, edit: ListMutation) -> bool {
        let Some(list) = self.virtual_list_mut(id) else {
            return false;
        };
        let (key, geometry_changed) = match edit {
            ListMutation::Append(key, height) if list.rows.append(key, height) => (key, true),
            ListMutation::Prepend(key, height) if list.rows.prepend(key, height) => (key, true),
            ListMutation::Remove(key) if list.rows.remove(key) => (key, true),
            ListMutation::Update(key, height) => match list.rows.update_height(key, height) {
                Some(changed) => (key, changed),
                None => return false,
            },
            _ => return false,
        };
        if list.follow_tail && geometry_changed {
            list.rows
                .set_viewport(list.rows.total(), list.rows.extent());
        }
        list.window_dirty |= geometry_changed;
        if geometry_changed && let Some(auto) = &mut list.auto {
            auto.ready = false;
        }
        // A removal followed by reinsertion of the same resident key before delivery
        // also invalidates that row. Never accumulate edits for offscreen keys.
        if list.resident.contains(&key) {
            list.edited.insert(key);
        }
        if matches!(edit, ListMutation::Remove(_)) {
            if list.reply.active() && list.resident.contains(&key) {
                list.reply.removed.insert(key);
                if let Some(pending) = &mut list.reply.pending {
                    // A reply can refer to any resident subtree, not just those it creates.
                    pending.invalidated = true;
                }
            }
            self.unbind_virtual_row(id, key);
        }
        true
    }

    /// Takes the initial demand (even if empty), then only visible-key changes. No overscan.
    /// A clean viewport is not traversed, and content-only edits do not reselect the window.
    /// Automatic lists first report demand after a frame lays them out.
    pub fn take_list_demand(&mut self, id: VirtualListId) -> Option<ListDemand> {
        let list = self.virtual_list_mut(id)?;
        if list.reply.active() || list.provider.is_some() {
            return None;
        }
        Self::list_delta(list, id)
    }

    fn list_delta(list: &mut VirtualList, id: VirtualListId) -> Option<ListDemand> {
        if list.auto.as_ref().is_some_and(|auto| !auto.ready) {
            return None;
        }
        let initial = list.delivered != Some(id);
        let mut demand = ListDemand {
            retired: list.delivered.filter(|_| initial),
            ..Default::default()
        };
        if std::mem::take(&mut list.window_dirty) {
            let mut resident = HashSet::new();
            for (key, _, _) in list.rows.visible() {
                resident.insert(key);
                if !list.resident.contains(&key) || list.reply.removed.contains(&key) {
                    demand.entering.push(key);
                }
            }
            demand.evicted.extend(
                list.resident
                    .iter()
                    .copied()
                    .filter(|key| !resident.contains(key) || list.reply.removed.contains(key)),
            );
            demand.evicted.sort_unstable();
            list.resident = resident;
        }
        demand.changed.extend(
            std::mem::take(&mut list.edited)
                .into_iter()
                .filter(|key| list.resident.contains(key) && !list.reply.removed.contains(key)),
        );
        list.reply.removed.clear();
        list.delivered = Some(id);
        (initial || demand != ListDemand::default()).then_some(demand)
    }

    // Only a fresh automatic list can join the accepted-reply path.
    fn propose_list_reply(&mut self, id: VirtualListId) -> Option<(ListReplyId, ListDemand)> {
        let vm = self.list_vm;
        let list = self.virtual_list_mut(id)?;
        if list.auto.is_none()
            || list.reply.pending.is_some()
            || list.reply.failed
            || !list.reply.active() && (list.delivered.is_some() || !list.bindings.is_empty())
        {
            return None;
        }
        let serial = list.reply.accepted.checked_add(1)?;
        let delta = Self::list_delta(list, id)?;
        let ticket = ListReplyId {
            list: id,
            vm,
            serial,
        };
        // Only the delta, never the whole window, is copied into the pending reply.
        let pending = PendingListReply {
            id: ticket,
            delta: ListDemand {
                entering: delta.entering.clone(),
                changed: delta.changed.clone(),
                evicted: delta.evicted.clone(),
                retired: delta.retired,
            },
            invalidated: false,
        };
        list.reply.pending = Some(pending);
        Some((ticket, delta))
    }

    // The caller owns transport correlation and must stop dispatching to this VM
    // on Rebuild. Stale tickets cannot consume or poison the current proposal.
    fn accept_list_reply(
        &mut self,
        ticket: ListReplyId,
        batch: impl Into<Arc<Batch>>,
        binds: &[(u64, u16)],
    ) -> Result<(), ListReplyError> {
        let batch = batch.into();
        let id = ticket.list;
        let Some((generation, list)) = self.virtual_lists.get(&id.node) else {
            return Err(ListReplyError::Stale);
        };
        let Some(pending) = list.reply.pending.as_ref().filter(|p| p.id == ticket) else {
            return Err(ListReplyError::Stale);
        };
        if ticket.vm != self.list_vm || !id.matches(self.list_issuer, *generation) {
            return Err(ListReplyError::Stale);
        }
        let delta = &pending.delta;
        let evicted: HashSet<_> = delta.evicted.iter().copied().collect();
        let entering: HashSet<_> = delta.entering.iter().copied().collect();
        let mut keys = HashSet::new();
        let mut rows = HashSet::new();
        let mut creates = HashSet::new();
        let mut removes = HashSet::new();
        let mut valid = !list.reply.failed && !pending.invalidated && entering.len() == binds.len();
        let mut ancestors = HashSet::from([id.node]);
        let mut parent = self.tree.parent(id.node);
        while let Some(node) = parent {
            ancestors.insert(node);
            parent = self.tree.parent(node);
        }
        for op in &batch.ops {
            match *op {
                Op::Create {
                    id: row,
                    parent: Some(node),
                    ..
                } if node == id.node => {
                    creates.insert(row);
                }
                Op::Remove { id: row } => {
                    removes.insert(row);
                    valid &= !ancestors.contains(&row)
                        && self
                            .bound_rows
                            .get(&row)
                            .is_none_or(|&(node, key)| node != id.node || evicted.contains(&key));
                }
                _ => {}
            }
        }
        valid &= binds.iter().all(|&(key, row)| {
            entering.contains(&key)
                && keys.insert(key)
                && rows.insert(row)
                && creates.contains(&row)
                && list.rows.contains(key)
                && (!list.bindings.contains_key(&key) || evicted.contains(&key))
                && (!self.bound_rows.contains_key(&row) || removes.contains(&row))
        });
        let gate = &mut self.virtual_list_mut(id).unwrap().reply;
        let pending = gate.pending.take().unwrap();
        if !valid {
            let _ = self.bridge.log.send(Log::Answer(false));
        }
        if !valid || !self.receive(batch) {
            self.virtual_list_mut(id).unwrap().reply.failed = true;
            return Err(ListReplyError::Rebuild);
        }
        for key in pending.delta.evicted {
            self.unbind_virtual_row(id, key);
        }
        for &(key, row) in binds {
            assert!(self.bind_virtual_row(id, key, row));
        }
        self.virtual_list_mut(id).unwrap().reply.accepted = ticket.serial;
        Ok(())
    }

    fn now(&self) -> f64 {
        self.clock.elapsed().as_secs_f64() * 1e3
    }

    /// Whether events are waiting for an answer from the app, or timers are due: keep polling.
    pub fn busy(&self) -> bool {
        self.pending > 0
            || !self.timers.is_empty()
            || !self.backlog.is_empty()
            || self.pending_drag_move.is_some()
    }

    fn has_inflight_reload(&self) -> bool {
        self.inflight
            .iter()
            .any(|kind| matches!(kind, Inflight::Reload(_)))
    }

    fn can_send_now(&self) -> bool {
        self.list_error.is_none()
            && self.pending < MAX_PENDING
            && self.list_ticket.is_none()
            && (self.list_mounts.is_empty() || !self.has_inflight_reload())
    }

    fn queued_payload(&self, callback: u16, payload: EventPayload) -> QueuedEvent {
        QueuedEvent {
            guard: self.callback_guards.get(&callback).cloned(),
            callback,
            payload,
        }
    }

    fn queued_event(&self, event: Event) -> QueuedEvent {
        self.queued_payload(event.callback, EventPayload::Text(event.text))
    }

    fn send(&mut self, event: Event) {
        self.send_queued(self.queued_event(event));
    }

    fn send_payload(&mut self, callback: u16, payload: EventPayload) {
        self.send_queued(self.queued_payload(callback, payload));
    }

    fn send_queued(&mut self, queued: QueuedEvent) {
        if self.list_error.is_some()
            || self.callback_guards.get(&queued.callback) != queued.guard.as_ref()
        {
            return;
        }
        if self.can_send_now() {
            self.bridge
                .events
                .try_send(queued.into_inbox())
                .expect("event queue");
            self.pending += 1;
            self.inflight.push_back(Inflight::Event);
            self.sent_at = Some(Instant::now());
        } else {
            self.backlog.push(queued);
        }
    }

    /// Sends a message from the embedding program to the app's `onHost` handler, in order with UI events.
    pub fn push(&mut self, text: String) {
        self.send(Event {
            callback: HOST_CALLBACK,
            text,
        });
    }

    /// Replaces the app with `script` (SDK + app) without restarting: the window, the program's connection and
    /// the native state of the tree stay. Signals keep their values where the new code creates them in the same
    /// order with the same initial value (or matching key). Focus, a field's text and selection, and scroll offsets
    /// stay on the nodes that keep their place in the tree. Returns false when the event queue is full. The outcome
    /// is reported by `take_reload_result` once `poll` has applied it; on failure the previous app keeps running.
    pub fn reload(&mut self, script: String) -> bool {
        self.reload_inner(script, true)
    }

    /// Like `reload`, but resets all JS signals and native focus/scroll state instead of preserving them.
    pub fn hard_reload(&mut self, script: String) -> bool {
        self.reload_inner(script, false)
    }

    fn reload_inner(&mut self, script: String, preserve_state: bool) -> bool {
        if self.pending == MAX_PENDING
            || self.list_ticket.is_some()
            || (!self.list_mounts.is_empty()
                && (!self.backlog.is_empty() || self.has_inflight_reload()))
        {
            return false;
        }
        self.bridge
            .events
            .try_send(Inbox::Reload(script, preserve_state))
            .expect("event queue");
        self.pending += 1;
        self.inflight.push_back(Inflight::Reload(preserve_state));
        true
    }

    /// Enables showing reload errors automatically in the native error overlay.
    pub fn enable_dev_overlay(&mut self, enabled: bool) {
        self.dev_overlay_enabled = enabled;
        if !enabled {
            self.dev_error = None;
        }
    }

    /// Shows or clears the native development error overlay above the app.
    pub fn set_dev_error(&mut self, error: Option<DevError>) {
        self.dev_error = error;
    }

    /// The currently displayed development error overlay, if any.
    pub fn dev_error(&self) -> Option<&DevError> {
        self.dev_error.as_ref()
    }

    /// The outcome of the next reload that `poll` has applied: `Err` when the new code failed to load or mount.
    pub fn take_reload_result(&mut self) -> Option<Result<(), String>> {
        self.reload_results.pop_front()
    }

    /// Turns reduced motion on or off. While it is on, infinite keyframe animations do not run.
    pub fn set_reduced_motion(&mut self, reduced: bool) {
        self.reduced_motion = reduced;
    }

    /// Sets the pixels that a `Node::Image` with this `source` draws. `rgba` is straight RGBA8, `size` in pixels.
    pub fn set_image(&mut self, source: String, rgba: Vec<u8>, size: [u32; 2]) {
        let pixels = size.iter().map(|&side| side as usize).product::<usize>();
        assert_eq!(
            rgba.len(),
            4 * pixels,
            "rgba must hold {size:?} RGBA8 pixels"
        );
        self.image_generation += 1;
        let image = ImageData {
            rgba: rgba.into(),
            size,
            generation: self.image_generation,
        };
        self.images.insert(source, image);
    }

    /// Loads a TTF or OTF font, which text can then name with `Font.family`. Text on screen switches to it.
    pub fn add_font(&mut self, bytes: Vec<u8>) {
        self.text.load_font(bytes);
        self.fonts_changed();
    }

    /// Sets the family of text whose `Font` names none. The default is the bundled Inter.
    pub fn set_default_family(&mut self, family: String) {
        self.text.set_default_family(family);
        self.fonts_changed();
    }

    // Text measured or drawn with the old fonts is stale: measure it again, and let backends drop their bitmaps.
    fn fonts_changed(&mut self) {
        self.font_epoch += 1;
        self.editor.fonts_changed();
        self.editor_key = None;
        for (id, node) in self.tree.nodes.iter() {
            if matches!(
                node,
                Node::Text { .. }
                    | Node::Input {
                        multiline: true,
                        ..
                    }
            ) {
                self.layout.text_changed(id);
            }
        }
    }

    /// Changes with every font change, so a backend compares it with the last frame's to drop its text bitmaps.
    pub fn font_epoch(&self) -> u64 {
        self.font_epoch
    }

    /// Stops drawing the image with this `source`; nodes that name it draw nothing.
    pub fn remove_image(&mut self, source: &str) {
        self.images.remove(source);
    }

    /// Fires due timers and applies the batches that arrived. When an event was just sent within
    /// `wait_budget` (default 800 µs), waits up to that deadline so fast JS answers land in the current frame
    /// without delaying frames when JS is slow.
    pub fn poll(&mut self) {
        let now = self.now();
        let guards = &self.callback_guards;
        let (due, waiting): (Vec<_>, Vec<_>) = self
            .timers
            .drain(..)
            .filter(|(_, event)| guards.get(&event.callback) == event.guard.as_ref())
            .partition(|(at, _)| *at <= now);
        self.timers = waiting;
        let backlog = std::mem::take(&mut self.backlog);
        for event in backlog
            .into_iter()
            .chain(due.into_iter().map(|(_, event)| event))
        {
            self.send_queued(event);
        }
        let deadline = self
            .sent_at
            .filter(|_| !self.wait_budget.is_zero())
            .map(|t| t + self.wait_budget);
        while self.pending > 0 {
            let wait = deadline.map_or(Duration::ZERO, |d| {
                d.saturating_duration_since(Instant::now())
            });
            let received = if wait.is_zero() {
                self.bridge.updates.try_recv().map_err(|e| match e {
                    TryRecvError::Empty => RecvTimeoutError::Timeout,
                    TryRecvError::Disconnected => RecvTimeoutError::Disconnected,
                })
            } else {
                self.bridge.updates.recv_timeout(wait)
            };
            match received {
                Ok(batch) => {
                    self.pending -= 1;
                    match self.inflight.pop_front() {
                        Some(Inflight::Reload(preserve_state)) => {
                            self.receive_reload(batch, preserve_state)
                        }
                        Some(Inflight::DragMove) => {
                            self.drag_moves_inflight = self.drag_moves_inflight.saturating_sub(1);
                            self.receive(batch);
                        }
                        _ => {
                            if let Some(ticket) = self.list_ticket.take() {
                                self.receive_list_reply(ticket, batch);
                            } else {
                                self.receive(batch);
                            }
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    // Past the budget: slow answers never hold later frames.
                    self.sent_at = None;
                    break;
                }
                Err(RecvTimeoutError::Disconnected) => panic!("app thread disconnected"),
            }
        }
        if self.pending == 0 {
            self.sent_at = None;
        }
        self.flush_drag_move();
    }

    // Applies the batch of a reload. The old tree's native state is kept only if the new code loaded and `preserve_state` is true.
    fn receive_reload(&mut self, batch: Arc<Batch>, preserve_state: bool) {
        // The app sends the outcome before the batch that carries it.
        let outcome = self
            .bridge
            .reports
            .try_recv()
            .unwrap_or_else(|_| Err("the app did not report the reload".into()));
        let kept = outcome.is_ok().then(|| self.keep());
        if kept.is_some() {
            // The app has already installed the new VM, even if its tree batch fails.
            self.list_vm = self.list_vm.checked_add(1).expect("VM epochs exhausted");
            self.callback_guards.clear();
            self.reported_viewport = None;
            self.timers.retain(|(_, event)| event.guard.is_none());
        } else if self.dev_overlay_enabled
            && let Err(error) = &outcome
        {
            self.dev_error = Some(DevError {
                title: "Reload failed".into(),
                message: error.clone(),
                location: None,
                frame: None,
            });
        }
        self.reload_results.push_back(outcome);
        let accepted = self.receive(batch);
        if let Some(kept) = kept {
            if accepted {
                self.list_error = None;
                self.dev_error = None;
            } else {
                if self.dev_overlay_enabled {
                    self.dev_error = Some(DevError {
                        title: "Mount batch rejected".into(),
                        message: "The reloaded app mounted an invalid UI batch.".into(),
                        location: None,
                        frame: None,
                    });
                }
                if !self.list_mounts.is_empty() {
                    self.fail_list(
                        "reload replaced the VM but its mount batch was rejected".into(),
                    );
                }
                for (_, list) in self.virtual_lists.values_mut() {
                    if list.reply.active() {
                        // The surviving tree belongs to the retired VM. Do not
                        // use its baseline to propose to the replacement VM.
                        list.reply.pending = None;
                        list.reply.failed = true;
                    }
                }
            }
            if preserve_state {
                self.restore(kept);
            } else {
                self.pressed = None;
                self.editor_dragging = false;
                self.drag = None;
                self.node_drag = None;
                self.pending_drag_move = None;
                self.focused = None;
            }
        }
    }

    // The native state of the current tree, by place.
    fn keep(&self) -> Kept {
        Kept {
            // The editor holds the focused field's text; for other nodes it is ignored.
            focused: self.focused.and_then(|id| {
                Some((
                    self.place(id)?,
                    self.editor.text().into_owned(),
                    self.editor.selection(),
                ))
            }),
            scroll: self
                .scroll
                .iter()
                .filter_map(|(&id, &offset)| Some((self.place(id)?, offset)))
                .collect(),
        }
    }

    // Puts back what `keep` saved on the nodes that now have the same place. A field's draft goes back into its
    // text, which the app hears of through `onChange`.
    fn restore(&mut self, kept: Kept) {
        // A press or drag that began on the old tree ends with it.
        self.pressed = None;
        self.editor_dragging = false;
        self.drag = None;
        self.node_drag = None;
        self.pending_drag_move = None;
        for (place, offset) in kept.scroll {
            if let Some(id) = self.find(&place) {
                self.scroll.insert(id, offset);
            }
        }
        if let Some((place, text, selection)) = kept.focused
            && let Some(id) = self.find(&place)
        {
            self.focus(Some(id));
            if let Some(Node::Input {
                multiline, purpose, ..
            }) = self.tree.nodes.get(&id)
            {
                let (multiline, password) = (*multiline, *purpose == InputPurpose::Password);
                self.editor.reset(&text, selection, multiline);
                self.editor.set_password(password);
                self.publish(id);
            }
        }
    }

    fn place(&self, id: u16) -> Option<Place> {
        let kind = std::mem::discriminant(self.tree.nodes.get(&id)?);
        let mut path = Vec::new();
        let mut node = id;
        while let Some(parent) = self.tree.parent(node) {
            path.push(self.tree.children(parent).iter().position(|&c| c == node)?);
            node = parent;
        }
        (self.tree.root == Some(node)).then(|| {
            path.reverse();
            (path, kind)
        })
    }

    fn find(&self, (path, kind): &Place) -> Option<u16> {
        let mut id = self.tree.root?;
        for &index in path {
            id = *self.tree.children(id).get(index)?;
        }
        (std::mem::discriminant(self.tree.nodes.get(&id)?) == *kind).then_some(id)
    }

    // Applies one app batch and updates the layout, transitions, timers and focus that depend on it.
    fn receive(&mut self, batch: impl Into<Arc<Batch>>) -> bool {
        let batch = batch.into();
        let mut removed = Vec::new();
        let mut affected_lists = HashSet::new();
        let mut created = Vec::new();
        let mut restyled = Vec::new();
        let mut repainted = Vec::new();
        let mut texts = Vec::new();
        let mut list_metadata = Vec::new();
        let mut requests = Vec::new();
        let mut focus_request = None;
        // Parents whose children were placed out of order, and moved nodes (their parent is known after the apply).
        let mut reordered = Vec::new();
        let mut moved = Vec::new();
        // Where in the batch each ID was last created and last mounted as a list: a recycled ID's mount counts only
        // when it came after the ID's last creation (one pass, no per-create scan).
        let mut last_create: HashMap<u16, usize> = HashMap::new();
        let mut last_listed: HashMap<u16, usize> = HashMap::new();
        for (index, op) in batch.ops.iter().enumerate() {
            match op {
                Op::Remove { id } => {
                    removed.extend(self.tree.subtree(*id));
                    if !self.bound_rows.is_empty() {
                        // Read ownership before the batch can recycle the ID, walking
                        // only the removed subtree root's old ancestor chain.
                        let mut parent = self.tree.parent(*id);
                        while let Some(node) = parent {
                            if let Some(&(list, _)) = self.bound_rows.get(&node) {
                                affected_lists.insert(list);
                            }
                            parent = self.tree.parent(node);
                        }
                    }
                }
                Op::Create {
                    id, parent, before, ..
                } => {
                    last_create.insert(*id, index);
                    created.push((*id, *parent));
                    if before.is_some() {
                        reordered.extend(*parent);
                    }
                }
                Op::Move { id, .. } => {
                    moved.push(*id);
                    restyled.push(*id);
                }
                Op::SetStyle { id, .. } => restyled.push(*id),
                Op::SetPaint { id, paint } => {
                    // If a previously static already-drawn node gains transitions or starting values via SetPaint,
                    // seed Motion with its current values before applying the batch so transitions start cleanly.
                    if (!paint.transitions.is_empty() || paint.starting.is_some())
                        && self.screen.get(*id as usize).is_some_and(Option::is_some)
                        && let Some(old_node) = self.tree.nodes.get(id)
                        && let Some(old_paint) = old_node.paint()
                        && old_paint.transitions.is_empty()
                        && old_paint.starting.is_none()
                    {
                        let disabled = matches!(old_node, Node::Box { disabled: true, .. });
                        let old_v = self.variant(*id, old_paint, disabled);
                        let rect = self.tree.rect(*id);
                        seed_motion_from_paint(&mut self.motion, *id, old_paint, &old_v, rect);
                    }
                    repainted.push(*id);
                }
                Op::SetText { id, .. } | Op::SetSpans { id, .. } | Op::SetField { id, .. } => texts.push(*id),
                Op::Focus { id } => focus_request = Some(*id),
                Op::After { .. } | Op::VirtualList { .. } | Op::CallbackGuard { .. } => {
                    if let Op::VirtualList { id, .. } = op {
                        last_listed.insert(*id, index);
                    }
                    list_metadata.push(op.clone());
                }
                Op::ScrollTo {
                    id,
                    offset,
                    to,
                    smooth,
                } => requests.push((
                    *id,
                    ScrollRequest {
                        offset: *offset,
                        to: *to,
                        smooth: *smooth,
                    },
                )),
                _ => {}
            }
        }
        let result = self.tree.apply_shared(batch);
        // The answer goes first: whatever the UI publishes next depends on this batch.
        let _ = self.bridge.log.send(Log::Answer(result.is_ok()));
        if let Err(e) = result {
            eprintln!("batch rejected ({e})");
            return false;
        }
        for id in affected_lists {
            if let Some((_, list)) = self.virtual_lists.get_mut(&id)
                && let Some(reply) = &mut list.reply.pending
            {
                reply.invalidated = true;
            }
        }
        for (id, request) in requests {
            if let Some((_, list)) = self.virtual_lists.get_mut(&id)
                && let Some(auto) = &mut list.auto
            {
                auto.requested = Some(if matches!(request.to, Some(ScrollEdge::End)) {
                    ListOffset::Tail
                } else { ListOffset::Pixels(f64::from(request.offset.unwrap_or(0.))) });
            } else {
                self.scroll_requests.insert(id, request);
            }
        }
        // Nodes created and removed in the same batch are gone too.
        removed.extend(
            created
                .iter()
                .map(|(id, _)| *id)
                .filter(|id| self.tree.nodes.get(id).is_none()),
        );
        self.layout.removed(&removed);
        if !removed.is_empty() && !self.bound_rows.is_empty() {
            self.forget_row_input(&removed);
        }
        for id in &removed {
            if let Some((list, key)) = self.bound_rows.remove(id)
                && let Some((_, list)) = self.virtual_lists.get_mut(&list)
            {
                list.bindings.remove(&key);
                if let Some(reply) = &mut list.reply.pending {
                    // A separate batch may recycle an ID before its reply is applied.
                    reply.invalidated = true;
                }
            }
            self.virtual_lists.remove(id);
            self.list_mounts.remove(id);
            self.scroll.remove(id);
            self.scroll_requests.remove(id);
            self.anchors.remove(id);
            self.placements.remove(id);
            self.sized_nodes.remove(id);
        }
        self.motion.forget(&removed);
        if self.focused.is_some_and(|f| removed.contains(&f)) {
            self.focused = None;
        }
        if self.pressed.is_some_and(|p| removed.contains(&p) || !self.pressable(p)) {
            self.pressed = None;
        }
        if let Some(drag) = self.node_drag {
            if removed.contains(&drag.id) {
                self.node_drag = None;
                self.pending_drag_move = None;
            } else if matches!(
                self.tree.nodes.get(&drag.id),
                Some(Node::Box { disabled: true, .. })
            ) {
                self.finish_node_drag(true);
            }
        }
        // An ID the batch recycled was created more than once, and one created then removed is gone: layout gets only
        // the last creation of each ID that is still in the tree, so it never hangs a node from a stale parent.
        let mut seen = HashSet::new();
        let mut last: Vec<(u16, Option<u16>)> = created
            .into_iter()
            .rev()
            .filter(|(id, _)| seen.insert(*id) && self.tree.nodes.get(id).is_some())
            .collect();
        last.reverse();
        let moved_any = !moved.is_empty();
        let reordered_any = !reordered.is_empty();
        let restyled_any = !restyled.is_empty();
        let repainted_any = !repainted.is_empty();
        let created_any = !last.is_empty();
        for (id, parent) in last {
            self.layout.created(&self.tree, id, parent);
            if self.tree.nodes.get(&id).is_some_and(is_height_animated) {
                self.sized_nodes.insert(id);
            } else {
                self.sized_nodes.remove(&id);
            }
        }
        for id in repainted {
            if self.tree.nodes.get(&id).is_some_and(is_height_animated) {
                self.sized_nodes.insert(id);
            } else {
                self.sized_nodes.remove(&id);
            }
        }
        let moved = moved.into_iter().filter_map(|id| self.tree.parent(id));
        for parent in reordered.into_iter().chain(moved) {
            self.layout.reordered(&self.tree, parent);
        }
        for id in restyled {
            self.layout.restyled(&self.tree, id);
        }
        if !removed.is_empty()
            || created_any
            || reordered_any
            || moved_any
            || restyled_any
            || repainted_any
        {
            self.cull_cache_dirty = true;
        }
        for id in texts {
            self.layout.text_changed(id);
            if self.focused == Some(id)
                && let Some(Node::Input {
                    text,
                    selection,
                    multiline,
                    purpose,
                    ..
                }) = self.tree.nodes.get(&id)
                && (&self.published.0 != text || self.published.1 != *selection)
            {
                self.editor.reset(text, *selection, *multiline);
                self.editor
                    .set_password(*purpose == InputPurpose::Password);
                self.published = (text.clone(), *selection);
                self.editor_key = None;
            }
        }
        if let Some(id) = focus_request {
            self.focus(Some(id));
        }
        let now = self.now();
        for op in list_metadata {
            match op {
                Op::After { ms, callback } => {
                    let event = self.queued_event(Event {
                        callback,
                        text: String::new(),
                    });
                    self.timers.push((now + f64::from(ms), event));
                }
                Op::VirtualList { id, name }
                    if matches!(self.tree.nodes.get(&id), Some(Node::List { .. }))
                        && last_create.get(&id) < last_listed.get(&id) =>
                {
                    self.list_mounts.insert(id, name);
                }
                Op::CallbackGuard {
                    callback,
                    token: Some(token),
                } => {
                    self.callback_guards.insert(callback, token);
                }
                Op::CallbackGuard {
                    callback,
                    token: None,
                } => {
                    self.callback_guards.remove(&callback);
                }
                _ => {}
            }
        }
        true
    }

    // ---------- Input ----------

    // The topmost thing under `point` and its index in `hits`: the last one drawn, which is the highest layer's and
    // the latest in it.
    fn top_hit_indexed(&self, point: [f32; 2]) -> Option<(usize, u16, i32)> {
        self.hits
            .iter()
            .enumerate()
            .rev()
            .find(|(_, h)| contains(h.rect, point) && self.row_input_at(h.id, Some(point)))
            .map(|(index, h)| (index, h.id, h.layer))
    }

    // The topmost thing under `point`: the last one drawn, which is the highest layer's and the latest in it.
    fn top_hit(&self, point: [f32; 2]) -> Option<(u16, i32)> {
        self.top_hit_indexed(point).map(|(_, id, layer)| (id, layer))
    }

    fn hit(&self, point: [f32; 2]) -> Option<u16> {
        self.top_hit(point).map(|(id, _)| id)
    }

    // The topmost primary-button target under `point`: an Input, a disabled Box, or a Box with `press` or `drag`.
    // Walks up through non-primary boxes (such as a `contextmenu`-only child) without crossing a layer boundary.
    fn primary_target(&self, point: [f32; 2]) -> Option<u16> {
        let top = self.hit(point)?;
        let mut node = top;
        loop {
            let is_primary = match self.tree.nodes.get(&node)? {
                Node::Input { .. } | Node::Box { disabled: true, .. } => true,
                Node::Box { press, drag, .. } => *press != 0 || *drag != 0,
                _ => false,
            };
            if is_primary
                && (node == top
                    || self.hits.iter().any(|h| {
                        h.id == node
                            && contains(h.rect, point)
                            && self.row_input_at(node, Some(point))
                    }))
            {
                return Some(node);
            }
            let layered = self
                .tree
                .nodes
                .get(&node)
                .and_then(Node::style)
                .is_some_and(|s| s.position == Position::Fixed || s.layer.is_some());
            if layered {
                return None;
            }
            node = self.tree.parent(node)?;
        }
    }

    // Whether a node of `layer` is covered at the pointer by a higher layer: the topmost thing under it is above
    // `layer` and is not the node itself or one of its descendants.
    fn covered(&self, top: Option<(u16, i32)>, layer: i32, id: u16) -> bool {
        higher(top, layer) && top.is_some_and(|(top, _)| !self.within(top, id))
    }

    // Whether `node` is `ancestor` or one of its descendants in the tree.
    fn within(&self, mut node: u16, ancestor: u16) -> bool {
        loop {
            if node == ancestor {
                return true;
            }
            match self.tree.parent(node) {
                Some(parent) => node = parent,
                None => return false,
            }
        }
    }

    /// Scrolls the innermost scroll node under the pointer that can still move, by `delta_y` logical pixels
    /// (positive moves the content up). Returns whether the input moved or queued an offset.
    pub fn wheel(&mut self, delta_y: f32) -> bool {
        let Some((id, next)) = self.scroll_step(delta_y, false) else {
            return false;
        };
        match next {
            WheelOffset::Legacy(next) => {
                self.motion.release(id, Channel::Scroll);
                self.scroll.insert(id, next);
            }
            WheelOffset::Virtual(next) => self.queue_list_wheel(id, next),
        }
        self.hover_frozen_until = self.now() + SCROLL_HOVER_IDLE_MS;
        true
    }

    /// Like `wheel`, but the offset glides to its new target over 150 ms, and repeated calls add up. For the
    /// line deltas of a mouse wheel; exact pixel deltas from a touchpad go through `wheel`.
    /// Automatic Lists instead queue a direct offset for the next frame.
    pub fn wheel_smooth(&mut self, delta_y: f32) -> bool {
        let Some((id, next)) = self.scroll_step(delta_y, true) else {
            return false;
        };
        let now = self.now();
        self.hover_frozen_until = now + SCROLL_HOVER_IDLE_MS;
        let next = match next {
            WheelOffset::Legacy(next) => next,
            WheelOffset::Virtual(next) => {
                self.queue_list_wheel(id, next);
                return true;
            }
        };
        let transition = ease_out(Property::Y);
        // A glide in progress continues from where it is on screen.
        let from = match self.motion.target(id, Channel::Scroll) {
            Some(target) => self
                .motion
                .show(id, Channel::Scroll, target, Some(&transition), now)
                .number(),
            None => self.scroll.get(&id).copied().unwrap_or(0.),
        };
        self.motion.start(
            id,
            Channel::Scroll,
            Value::Number(from),
            Value::Number(next),
            &transition,
            now,
        );
        true
    }

    fn queue_list_wheel(&mut self, id: u16, offset: ListOffset) {
        if let Some(auto) = self
            .virtual_lists
            .get_mut(&id)
            .and_then(|(_, list)| list.auto.as_mut())
        {
            auto.requested = Some(offset);
        }
    }

    // The innermost scroll node under the pointer that can still move by `delta_y`, and the offset it reaches.
    // A smooth legacy wheel adds to a glide's target; an automatic List wheel adds to its queued offset.
    fn scroll_step(&self, delta_y: f32, smooth: bool) -> Option<(u16, WheelOffset)> {
        let point = self.pointer?;
        let top = self.top_hit(point);
        self.scrolls.iter().rev().find_map(|s| {
            if s.k <= 0.
                || !contains(s.rect, point)
                || !self.row_input_at(s.id, Some(point))
                || self.covered(top, s.layer, s.id)
            {
                return None;
            }
            if let Some(generation) = s.list_generation {
                let (current, list) = self.virtual_lists.get(&s.id)?;
                let auto = list.auto.as_ref()?;
                if *current != generation || !delta_y.is_finite() {
                    return None;
                }
                let extent = list.rows.extent();
                let max = list.max_offset(extent);
                let base = list.offset_for(auto.requested, extent);
                let next = (base + f64::from(delta_y) / f64::from(s.k)).clamp(0., max);
                return (next != base).then_some((
                    s.id,
                    WheelOffset::Virtual(if next == max {
                        ListOffset::Tail
                    } else {
                        ListOffset::Pixels(next)
                    }),
                ));
            }
            if self
                .virtual_lists
                .get(&s.id)
                .is_some_and(|(_, list)| list.auto.is_some())
            {
                return None;
            }
            let base = match self.motion.target(s.id, Channel::Scroll) {
                Some(target) if smooth => target.number(),
                _ => self.scroll.get(&s.id).copied().unwrap_or(0.),
            };
            let next = (base + delta_y / s.k).clamp(0., s.max);
            (next != base).then_some((s.id, WheelOffset::Legacy(next)))
        })
    }

    // A press on a scrollbar: on the thumb it starts a drag, on the track above or below it pages by a viewport.
    fn press_scrollbar(&mut self, point: [f32; 2]) -> bool {
        let top = self.top_hit(point);
        let Some((id, view, max, thumb)) = self.scrolls.iter().rev().find_map(|s| {
            let (track, thumb) = s.bar?;
            (contains(s.rect, point)
                && contains(track, point)
                && self.row_input_at(s.id, Some(point))
                && !self.covered(top, s.layer, s.id))
            .then_some((s.id, s.view, s.max, thumb))
        }) else {
            return false;
        };
        if contains(thumb, point) {
            self.drag = Some((id, point[1] - thumb[1]));
        } else {
            let page = if point[1] < thumb[1] { -view } else { view };
            let offset = self.scroll.get(&id).copied().unwrap_or(0.) + page;
            self.motion.release(id, Channel::Scroll);
            self.scroll.insert(id, offset.clamp(0., max));
        }
        true
    }

    fn pressable(&self, id: u16) -> bool {
        matches!(self.tree.nodes.get(&id), Some(Node::Box { press, disabled: false, .. }) if *press != 0)
    }

    fn draggable(&self, id: u16) -> bool {
        matches!(self.tree.nodes.get(&id), Some(Node::Box { drag, disabled: false, .. }) if *drag != 0)
    }

    fn interactive(&self, id: u16) -> bool {
        self.pressable(id) || self.draggable(id)
    }

    fn drag_callback(&self, id: u16, allow_disabled: bool) -> Option<u16> {
        if !self.row_can_receive(id) {
            return None;
        }
        match self.tree.nodes.get(&id) {
            Some(Node::Box { drag, disabled, .. })
                if *drag != 0 && (allow_disabled || !disabled) =>
            {
                Some(*drag)
            }
            _ => None,
        }
    }

    fn flush_drag_move(&mut self) {
        if self.drag_moves_inflight > 0 || !self.backlog.is_empty() || !self.can_send_now() {
            return;
        }
        let Some(point) = self.pending_drag_move.take() else {
            return;
        };
        let Some(drag) = self.node_drag else {
            return;
        };
        let Some(callback) = self.drag_callback(drag.id, false) else {
            self.node_drag = None;
            return;
        };
        let queued = self.queued_payload(
            callback,
            EventPayload::Drag {
                phase: 1,
                x: point[0],
                y: point[1],
                sx: drag.start[0],
                sy: drag.start[1],
                dx: point[0] - drag.start[0],
                dy: point[1] - drag.start[1],
            },
        );
        if self.callback_guards.get(&queued.callback) != queued.guard.as_ref() {
            return;
        }
        self.bridge
            .events
            .try_send(queued.into_inbox())
            .expect("event queue");
        self.pending += 1;
        self.drag_moves_inflight += 1;
        self.inflight.push_back(Inflight::DragMove);
        self.sent_at = Some(Instant::now());
    }

    // Ends the active node drag, flushing any coalesced move before sending `"end"`.
    // Returns `(had_drag, moved)`.
    fn finish_node_drag(&mut self, allow_disabled: bool) -> (bool, bool) {
        let Some(drag) = self.node_drag.take() else {
            self.pending_drag_move = None;
            return (false, false);
        };
        let pending_move = self.pending_drag_move.take();
        if let Some(callback) = self.drag_callback(drag.id, allow_disabled) {
            if let Some(point) = pending_move {
                self.send_payload(
                    callback,
                    EventPayload::Drag {
                        phase: 1,
                        x: point[0],
                        y: point[1],
                        sx: drag.start[0],
                        sy: drag.start[1],
                        dx: point[0] - drag.start[0],
                        dy: point[1] - drag.start[1],
                    },
                );
            }
            self.send_payload(
                callback,
                EventPayload::Drag {
                    phase: 2,
                    x: drag.last[0],
                    y: drag.last[1],
                    sx: drag.start[0],
                    sy: drag.start[1],
                    dx: drag.last[0] - drag.start[0],
                    dy: drag.last[1] - drag.start[1],
                },
            );
        }
        (true, drag.moved)
    }

    fn node_cursor(&self, id: u16) -> Option<Cursor> {
        let node = self.tree.nodes.get(&id)?;
        if matches!(node, Node::Box { disabled: true, .. }) {
            return Some(Cursor::NotAllowed);
        }
        if let Some(paint) = node.paint()
            && let Some(cursor) = self.variant(id, paint, false).cursor.or(paint.cursor)
        {
            return Some(cursor);
        }
        match node {
            Node::Input { .. } => Some(Cursor::Text),
            Node::Box { press, .. } if *press != 0 => Some(Cursor::Pointer),
            _ => None,
        }
    }

    /// Pointer shape for what is under the pointer, or for the node currently being dragged.
    pub fn cursor(&self) -> Cursor {
        if let Some(drag) = self.node_drag
            && self.row_can_receive(drag.id)
        {
            return self
                .tree
                .nodes
                .get(&drag.id)
                .and_then(Node::paint)
                .and_then(|paint| self.variant(drag.id, paint, false).cursor.or(paint.cursor))
                .unwrap_or(Cursor::Default);
        }
        let Some(point) = self.pointer else {
            return Cursor::Default;
        };
        let top = self.top_hit_indexed(point);
        let top_pair = top.map(|(_, id, layer)| (id, layer));
        if let Some(cursor) = self.cursor_rects.iter().rev().find_map(|c| {
            if !contains(c.rect, point)
                || !self.row_input_at(c.id, Some(point))
                || self.covered(top_pair, c.layer, c.id)
            {
                return None;
            }
            if let Some((top_idx, _, top_layer)) = top
                && (c.layer < top_layer || (c.layer == top_layer && c.after_hits <= top_idx))
            {
                return None;
            }
            self.node_cursor(c.id)
        }) {
            return cursor;
        }
        self.primary_target(point)
            .and_then(|id| self.node_cursor(id))
            .unwrap_or(Cursor::Default)
    }

    /// The pointer moved. Returns the cursor shape for what is under it.
    pub fn pointer_move(&mut self, point: Option<[f32; 2]>) -> Cursor {
        let point = point.filter(|p| p.iter().all(|v| v.is_finite()));
        self.pointer = point;
        if let Some(mut drag) = self.node_drag {
            if let Some(p) = point {
                if self.drag_callback(drag.id, false).is_some() {
                    if p != drag.last {
                        drag.last = p;
                        drag.moved = true;
                        self.node_drag = Some(drag);
                        self.pending_drag_move = Some(p);
                        self.flush_drag_move();
                    }
                } else {
                    self.node_drag = None;
                    self.pending_drag_move = None;
                    self.refresh_hover();
                }
            }
        } else {
            self.refresh_hover();
        }
        if let (Some((id, grab)), Some(p)) = (self.drag, point)
            && self.row_can_receive(id)
            && let Some(s) = self.scrolls.iter().find(|s| s.id == id)
            && let Some((track, thumb)) = s.bar
            && thumb[3] < track[3]
        {
            // The thumb follows the pointer along its travel, and the offset is the same share of the content.
            let share = ((p[1] - grab - track[1]) / (track[3] - thumb[3])).clamp(0., 1.);
            let offset = share * s.max;
            self.motion.release(id, Channel::Scroll);
            self.scroll.insert(id, offset);
        }
        if let (Some(id), Some(p)) = (self.focused, point)
            && self.pressed.is_none()
            && self.editor_dragging
        {
            let (x, y) = self.field_point(id, p);
            self.edit(&[EditorEvent::Point {
                x,
                y,
                select: PointSelection::Extend,
            }]);
        }
        self.cursor()
    }

    /// Primary button pressed. `select` comes from the click count (word, all) and modifiers.
    pub fn pointer_down(&mut self, select: PointSelection) {
        let Some(point) = self.pointer else { return };
        self.finish_node_drag(false);
        self.dismiss_outside(point);
        self.pressed_groups = self.hovered_groups.clone();
        if self.press_scrollbar(point) {
            return;
        }
        let target = self.primary_target(point);
        match target.and_then(|id| self.tree.nodes.get(&id)) {
            Some(Node::Input { .. }) => {
                let id = target.unwrap();
                self.focus(Some(id));
                let (x, y) = self.field_point(id, point);
                self.edit(&[EditorEvent::Point { x, y, select }]);
                self.editor_dragging = true;
            }
            _ => {
                self.pressed = target.filter(|id| self.pressable(*id));
                if let Some(id) = target
                    && let Some(callback) = self.drag_callback(id, false)
                {
                    self.node_drag = Some(NodeDrag {
                        id,
                        start: point,
                        last: point,
                        moved: false,
                    });
                    self.send_payload(
                        callback,
                        EventPayload::Drag {
                            phase: 0,
                            x: point[0],
                            y: point[1],
                            sx: point[0],
                            sy: point[1],
                            dx: 0.,
                            dy: 0.,
                        },
                    );
                }
                if self.pressed.is_none() {
                    self.focus(None);
                }
            }
        }
    }

    /// Primary button released: a press that ends on the same node is a click.
    pub fn pointer_up(&mut self) {
        self.editor_dragging = false;
        self.drag = None;
        self.pressed_groups.clear();
        let (had_drag, moved) = self.finish_node_drag(false);
        let pressed = self.pressed.take();
        if let Some(id) = pressed
            && !moved
            && self.pointer.and_then(|p| self.primary_target(p)) == Some(id)
            && let Some(Node::Box { press, .. }) = self.tree.nodes.get(&id)
        {
            let callback = *press;
            self.send(Event {
                callback,
                text: String::new(),
            });
        }
        if had_drag {
            self.refresh_hover();
        }
    }

    /// Cancels any active pointer press or drag (e.g. when the window loses focus).
    pub fn pointer_cancel(&mut self) {
        self.editor_dragging = false;
        self.drag = None;
        self.pressed = None;
        self.pressed_groups.clear();
        let (had_drag, _) = self.finish_node_drag(false);
        if had_drag {
            self.refresh_hover();
        }
    }

    /// Secondary button pressed at the pointer. The nearest box with a `contextmenu` callback, from the topmost thing
    /// under the pointer up to its layer's root, receives the window point. A layer does not pass it to what it covers.
    pub fn context_press(&mut self) {
        let Some(point) = self.pointer else { return };
        self.dismiss_outside(point);
        let top = self.hit(point);
        let target = top.and_then(|mut node| {
            loop {
                if matches!(self.tree.nodes.get(&node), Some(Node::Box { contextmenu, .. }) if *contextmenu != 0) {
                    return Some(node);
                }
                let layered = self.tree.nodes.get(&node).and_then(Node::style).is_some_and(|s| {
                    s.position == Position::Fixed || s.layer.is_some()
                });
                if layered {
                    return None;
                }
                node = self.tree.parent(node)?;
            }
        });
        if let Some(id) = target
            && let Some(Node::Box { contextmenu, .. }) = self.tree.nodes.get(&id)
        {
            self.send_payload(
                *contextmenu,
                EventPayload::ContextMenu {
                    x: point[0],
                    y: point[1],
                },
            );
        }
    }

    /// Escape was pressed: dismisses the development error overlay if shown, otherwise asks every box with a `dismiss` callback to close.
    pub fn escape(&mut self) {
        if self.dev_error.take().is_some() {
            return;
        }
        let ids: Vec<u16> = self.dismiss_rects.iter().map(|(id, _, _)| *id).collect();
        for id in ids {
            self.dismiss(id);
        }
    }

    // A press outside a dismissing box, and not on a higher layer, dismisses it. A press on the node it is anchored
    // to is not outside: that press is the anchor's own, which toggles the box.
    fn dismiss_outside(&mut self, point: [f32; 2]) {
        let top = self.top_hit(point);
        let ids: Vec<u16> = self
            .dismiss_rects
            .iter()
            .filter(|(id, rect, layer)| {
                !contains(*rect, point) && !higher(top, *layer) && !self.on_anchor(*id, point)
            })
            .map(|(id, _, _)| *id)
            .collect();
        for id in ids {
            self.dismiss(id);
        }
    }

    // Whether `point` is on the node that `id`, or its nearest anchored ancestor, is anchored to.
    fn on_anchor(&self, id: u16, point: [f32; 2]) -> bool {
        let mut node = Some(id);
        while let Some(current) = node {
            let anchor = self
                .tree
                .nodes
                .get(&current)
                .and_then(Node::style)
                .and_then(|s| s.anchor.as_ref());
            if let Some(anchor) = anchor {
                return self
                    .screen_rect(anchor.node)
                    .is_some_and(|rect| contains(rect, point));
            }
            node = self.tree.parent(current);
        }
        false
    }

    fn dismiss(&mut self, id: u16) {
        if self.row_can_receive(id)
            && let Some(Node::Box { dismiss, .. }) = self.tree.nodes.get(&id)
            && *dismiss != 0
        {
            let callback = *dismiss;
            self.send(Event {
                callback,
                text: String::new(),
            });
        }
    }

    /// Where a node was drawn in the last frame, unclipped, in window logical pixels. None if it was not drawn.
    pub fn screen_rect(&self, id: u16) -> Option<Rect> {
        self.screen
            .get(id as usize)
            .copied()
            .flatten()
            .filter(|_| self.row_can_receive(id))
    }

    /// Moves keyboard focus to a field, or clears it.
    pub fn focus(&mut self, id: Option<u16>) {
        let id = id.filter(|&id| self.row_can_receive(id));
        if self.focused == id {
            return;
        }
        if self.focused.is_some() {
            self.edit(&[EditorEvent::Preedit {
                text: String::new(),
                active: None,
            }]);
        }
        self.focused = id;
        if let Some(Node::Input {
            text,
            selection,
            multiline,
            purpose,
            ..
        }) = id.and_then(|id| self.tree.nodes.get(&id))
        {
            let (text, selection, multiline, password) = (
                text.clone(),
                *selection,
                *multiline,
                *purpose == InputPurpose::Password,
            );
            self.editor.reset(&text, selection, multiline);
            self.editor.set_password(password);
            self.published = (text, selection);
            self.editor_key = None;
        }
    }

    /// Where the pointer is, if it is over the window.
    pub fn pointer(&self) -> Option<[f32; 2]> {
        self.pointer
    }

    /// Whether a field has focus: the backend enables the platform's input method only then.
    pub fn focused(&self) -> bool {
        self.focused.is_some_and(|id| self.row_can_receive(id))
    }

    /// Cross-platform IME state for the focused field (purpose, whether IME is allowed, and surrounding text).
    pub fn ime_state(&self) -> ImeState {
        let Some(id) = self.focused.filter(|&id| self.row_can_receive(id)) else {
            return ImeState {
                allowed: false,
                purpose: InputPurpose::Normal,
                surrounding: None,
            };
        };
        let purpose = match self.tree.nodes.get(&id) {
            Some(Node::Input { purpose, .. }) => *purpose,
            _ => InputPurpose::Normal,
        };
        let allowed = purpose != InputPurpose::Password;
        let surrounding = allowed.then(|| self.editor.surrounding_text(4000));
        ImeState {
            allowed,
            purpose,
            surrounding,
        }
    }

    pub fn composing(&self) -> bool {
        self.focused() && self.editor.composing()
    }

    /// Keyboard and input-method events for the focused field.
    pub fn edit(&mut self, events: &[EditorEvent]) -> EditorOutput {
        let mut output = EditorOutput::default();
        let Some(id) = self.focused.filter(|&id| self.row_can_receive(id)) else {
            return output;
        };
        let composing = self.editor.composing()
            || events.iter().any(|e| match e {
                EditorEvent::Commit(_) => true,
                EditorEvent::Preedit { text, .. } | EditorEvent::PreeditSegments { text, .. } => {
                    !text.is_empty()
                }
                _ => false,
            });
        // Motions wrap where the field is laid out, which may be before the next frame draws it.
        if let Some(Node::Input {
            multiline: true, ..
        }) = self.tree.nodes.get(&id)
        {
            let width = self.input_content(id)[2];
            if width > 0. {
                self.editor
                    .set_width(&mut self.text, Some(width + WRAP_SLACK));
            }
        }
        for event in events {
            if output.interrupt_ime && event.involves_ime() {
                continue;
            }
            if matches!(event, EditorEvent::Enter | EditorEvent::LineBreak) {
                let Some(Node::Input {
                    submit,
                    multiline,
                    submit_on_enter,
                    retain_on_submit,
                    ..
                }) = self.tree.nodes.get(&id)
                else {
                    continue;
                };
                let (submit, multiline, submit_on_enter, retain_on_submit) = (*submit, *multiline, *submit_on_enter, *retain_on_submit);
                if composing {
                    continue;
                }
                if matches!(event, EditorEvent::LineBreak) || (multiline && !submit_on_enter) {
                    self.editor.handle(&mut self.text, &EditorEvent::LineBreak);
                } else if submit != 0 {
                    let text = self.editor.text().into_owned();
                    if multiline && !retain_on_submit {
                        // A multi-line field clears itself as it submits, as the chat does: the app hears of the cleared field first.
                        self.editor.reset("", [0, 0], true);
                        self.publish(id);
                    }
                    self.send(Event {
                        callback: submit,
                        text,
                    });
                }
                continue;
            }
            let effect = self.editor.handle(&mut self.text, event);
            if effect.clipboard.is_some() {
                output.clipboard = effect.clipboard;
            }
            output.interrupt_ime |= effect.interrupt_ime;
        }
        self.publish(id);
        output
    }

    // Writes the field's text and selection to the tree, and tells the app if the text changed.
    fn publish(&mut self, id: u16) {
        let state = (self.editor.text().into_owned(), self.editor.selection());
        if state == self.published {
            return;
        }
        let changed = state.0 != self.published.0;
        if changed {
            // A multi-line field's height follows its text.
            self.layout.text_changed(id);
        }
        let batch = Batch {
            base: 0,
            ops: vec![Op::SetInput {
                id,
                text: state.0.clone(),
                selection: state.1,
            }],
        };
        self.tree.apply_ui_ref(&batch.ops).expect("UI batch");
        let _ = self.bridge.log.send(Log::Ui(batch));
        if changed
            && let Some(Node::Input { change, .. }) = self.tree.nodes.get(&id)
            && *change != 0
        {
            let callback = *change;
            self.send(Event {
                callback,
                text: state.0.clone(),
            });
        }
        self.published = state;
    }

    // A window point as the field's editor counts it: x from the content's left, y from the top of the text, which
    // the field's scroll moves.
    fn field_point(&self, id: u16, point: [f32; 2]) -> (f32, f32) {
        let content = self.input_content(id);
        let scroll = self.scroll.get(&id).copied().unwrap_or(0.);
        (point[0] - content[0], point[1] - content[1] + scroll)
    }

    // The field's text area: its rectangle minus its padding.
    fn input_content(&self, id: u16) -> Rect {
        let [x, y, w, h] = self.tree.rect(id);
        let [top, right, bottom, left] = self
            .tree
            .nodes
            .get(&id)
            .and_then(Node::style)
            .map_or([0.; 4], |s| s.padding.sides());
        [
            x + left,
            y + top,
            (w - left - right).max(0.),
            (h - top - bottom).max(0.),
        ]
    }

    /// Focuses the first text field in the tree, as an `autofocus`.
    pub fn focus_first_input(&mut self) {
        let Some(root) = self.tree.root else { return };
        let first = self
            .tree
            .subtree(root)
            .into_iter()
            .find(|id| matches!(self.tree.nodes.get(id), Some(Node::Input { .. })));
        self.focus(first);
    }

    /// Rasterizes text for a backend, at `scale` device pixels per logical pixel.
    pub fn raster_text(
        &mut self,
        spans: &[Span],
        size: [f32; 2],
        scale: f32,
        wrap: bool,
        max_side: usize,
        style: TextStyle,
    ) -> Option<TextBitmap> {
        self.text
            .rasterize_styled(spans, size, scale, wrap, max_side, &style)
    }

    /// Forget uploaded images (the backend lost its surface).
    pub fn reset_images(&mut self) {
        self.editor_key = None;
    }

    // ---------- Frame ----------

    /// Sends `{"jsnirViewport":[width,height]}` to `onHost` on the first frame, on
    /// logical size changes, and after VM replacement. Offscreen frames use the same units.
    pub fn enable_viewport_reporting(&mut self) {
        self.viewport_reporting = true;
    }

    /// Lays out the tree for the window and returns what to draw.
    pub fn frame(&mut self, width: f32, height: f32, scale: f32, max_side: usize) -> Frame {
        if self.viewport_reporting && self.reported_viewport != Some([width, height]) {
            self.reported_viewport = Some([width, height]);
            self.push(serde_json::json!({"jsnirViewport": [width, height]}).to_string());
        }
        let now = self.now();
        let (mut frame, mut placements) = self.render(width, height, scale, max_side, now);
        // A glide keeps hover frozen until it has been over for SCROLL_HOVER_IDLE_MS.
        if self.motion.animating_on(Channel::Scroll, now) {
            self.hover_frozen_until = now + SCROLL_HOVER_IDLE_MS;
        }
        if now < self.hover_frozen_until {
            // Keeps drawing until the idle time is over, so hover is then recomputed.
            frame.animating = true;
        } else if self.node_drag.is_none() && self.refresh_hover() {
            // A layout change may have moved what is under the pointer. Draw changed hover at once and discard
            // placements from the provisional pass.
            (frame, placements) = self.render(width, height, scale, max_side, now);
        }
        self.commit_list_viewports();
        frame.animating |= self.measure_virtual_rows();
        self.publish_placements(placements);
        self.publish_list_delta();
        frame
    }

    fn publish_list_delta(&mut self) {
        // Start only at the head of the ordered lane. Until the answer is applied,
        // later callbacks stay on the UI side and cannot observe speculative state.
        if self.pending != 0
            || !self.backlog.is_empty()
            || self.pending_drag_move.is_some()
            || self.list_error.is_some()
        {
            return;
        }
        let ids: Vec<_> = self
            .virtual_lists
            .iter()
            .filter_map(|(&node, &(generation, ref list))| {
                list.provider.is_some().then_some(VirtualListId {
                    issuer: self.list_issuer,
                    node,
                    generation,
                })
            })
            .collect();
        for id in ids {
            let Some((ticket, delta)) = self.propose_list_reply(id) else {
                continue;
            };
            let provide = self
                .virtual_list_mut(id)
                .unwrap()
                .provider
                .as_mut()
                .unwrap();
            let mut rows = |keys: Vec<u64>| -> Result<Vec<_>, String> {
                keys.into_iter()
                    .map(|key| Ok((key.to_string(), provide(key)?)))
                    .collect()
            };
            let payload = (|| -> Result<_, String> {
                Ok(serde_json::json!({
                    "node": id.node,
                    "mount": id.generation.to_string(),
                    "entering": rows(delta.entering)?,
                    "changed": rows(delta.changed)?,
                    "evicted": delta.evicted.into_iter().map(|key| key.to_string()).collect::<Vec<_>>(),
                }).to_string())
            })();
            match payload {
                Ok(payload) => {
                    self.bridge
                        .events
                        .try_send(Inbox::List(payload))
                        .expect("event queue");
                    self.pending += 1;
                    self.inflight.push_back(Inflight::Event);
                    self.list_ticket = Some(ticket);
                }
                Err(error) => self.fail_list(format!("virtual List payload: {error}")),
            }
            break;
        }
    }

    fn receive_list_reply(&mut self, ticket: ListReplyId, batch: Arc<Batch>) {
        match self
            .bridge
            .list_replies
            .try_recv()
            .expect("list reply precedes batch")
        {
            Ok(binds) => {
                if let Err(error) = self.accept_list_reply(ticket, batch, &binds) {
                    if error == ListReplyError::Stale {
                        let _ = self.bridge.log.send(Log::Answer(false));
                    }
                    self.fail_list(format!("virtual List reply rejected: {error:?}"));
                }
            }
            Err(error) => {
                let _ = self.bridge.log.send(Log::Answer(false));
                self.fail_list(format!("virtual List callback: {error}"));
            }
        }
    }

    fn fail_list(&mut self, error: String) {
        self.list_error = Some(error);
        self.backlog.clear();
        self.timers.clear();
        self.pending_drag_move = None;
    }

    fn commit_list_viewports(&mut self) {
        for (_, list) in self.virtual_lists.values_mut() {
            let Some(auto) = &mut list.auto else { continue };
            let Some((offset, extent)) = auto.preview.take() else {
                continue;
            };
            let geometry_edited = !std::mem::replace(&mut auto.ready, true);
            if let Some(requested) = auto.requested.take() {
                list.follow_tail = matches!(requested, ListOffset::Tail);
            }
            let moved = (offset, extent) != (list.rows.offset(), list.rows.extent());
            if moved {
                list.apply_viewport(offset, extent);
            }
            if (moved || geometry_edited)
                && let Some(reply) = &mut list.reply.pending
            {
                // Observe every committed viewport, so leaving and reentering
                // before the reply arrives cannot revive a residency grant.
                let mut granted = reply.delta.entering.iter().chain(&reply.delta.changed);
                reply.invalidated |=
                    granted.any(|&key| list.rows.visible_row_at(key, offset, extent).is_none());
            }
        }
    }

    fn measure_virtual_rows(&mut self) -> bool {
        let mut any = false;
        for (&node, (_, list)) in &mut self.virtual_lists {
            if !list.measured { continue; }
            let mut changed = false;
            for row in self.layout.visible_rows(node) {
                let (_, key) = self.bound_rows[row];
                let height = self.tree.rect(*row)[3].max(1.);
                if list.rows.height(key).is_some_and(|before| (before - height).abs() > 0.01) {
                    changed |= list.rows.update_height(key, height) == Some(true);
                }
            }
            if changed {
                if list.follow_tail { list.rows.set_viewport(list.rows.total(), list.rows.extent()); }
                list.window_dirty = true;
                if let Some(auto) = &mut list.auto { auto.ready = false; }
                any = true;
            }
        }
        any
    }

    fn rebuild_cull_cache(&mut self) {
        let end = self.tree.nodes.end();
        self.cull_bounds.clear();
        self.cull_bounds.resize(end, [0.; 4]);
        // Default `true` so any node not visited from `root` (e.g. detached virtual list rows) is never culled.
        self.cull_special.clear();
        self.cull_special.resize(end, true);
        let Some(root) = self.tree.root else {
            self.cull_cache_dirty = false;
            return;
        };
        let mut order = Vec::with_capacity(end);
        let mut stack = vec![root];
        let mut anchor_targets = Vec::new();
        while let Some(id) = stack.pop() {
            let Some(node) = self.tree.nodes.get(&id) else {
                continue;
            };
            order.push(id);
            if let Some(anchor) = node.style().and_then(|s| s.anchor.as_ref()) {
                anchor_targets.push(anchor.node);
            }
            if !matches!(node, Node::List { .. }) {
                stack.extend(self.tree.children(id).iter().copied());
            }
        }
        for &id in order.iter().rev() {
            let Some(node) = self.tree.nodes.get(&id) else {
                continue;
            };
            let rect = self.tree.rect(id);
            let mut min_x = rect[0];
            let mut min_y = rect[1];
            let mut max_x = rect[0] + rect[2];
            let mut max_y = rect[1] + rect[3];
            let mut special = node_is_cull_special(node, self.motion.has_active(id));
            let clips = node.style().is_some_and(|s| s.clip);
            if !matches!(node, Node::List { .. }) {
                for &c in self.tree.children(id) {
                    let ci = c as usize;
                    if ci >= end {
                        continue;
                    }
                    special |= self.cull_special[ci];
                    let child_fixed = self
                        .tree
                        .nodes
                        .get(&c)
                        .and_then(Node::style)
                        .is_some_and(|s| s.position == Position::Fixed);
                    if !clips && !child_fixed {
                        let b = self.cull_bounds[ci];
                        if b[2] > 0. || b[3] > 0. {
                            min_x = min_x.min(b[0]);
                            min_y = min_y.min(b[1]);
                            max_x = max_x.max(b[0] + b[2]);
                            max_y = max_y.max(b[1] + b[3]);
                        }
                    }
                }
            }
            self.cull_bounds[id as usize] =
                [min_x, min_y, (max_x - min_x).max(0.), (max_y - min_y).max(0.)];
            self.cull_special[id as usize] = special;
        }
        // Any node targeted by a Fixed overlay's anchor (and its ancestor chain) must always populate `screen`.
        for mut target in anchor_targets {
            while (target as usize) < end {
                if self.cull_special[target as usize] {
                    break;
                }
                self.cull_special[target as usize] = true;
                match self.tree.parent(target) {
                    Some(p) => target = p,
                    None => break,
                }
            }
        }
        self.cull_cache_dirty = false;
    }

    fn render(
        &mut self,
        width: f32,
        height: f32,
        scale: f32,
        max_side: usize,
        now: f64,
    ) -> (Frame, Vec<(u16, Placement)>) {
        self.lay_out(width, height, now);
        if self.focused.is_some() && !self.focused() {
            self.focused = None;
            self.editor_dragging = false;
        }
        if self.cull_cache_dirty {
            self.rebuild_cull_cache();
        }
        let mut items = Vec::with_capacity(self.last_items_len);
        let mut hits = std::mem::take(&mut self.hits);
        hits.clear();
        let mut cursors = std::mem::take(&mut self.cursor_rects);
        cursors.clear();
        let mut scrolls = std::mem::take(&mut self.scrolls);
        scrolls.clear();
        let mut groups = std::mem::take(&mut self.group_rects);
        groups.clear();
        let mut hovers = std::mem::take(&mut self.hover_rects);
        hovers.clear();
        let mut dismissers = std::mem::take(&mut self.dismiss_rects);
        dismissers.clear();
        let mut ime_area = None;
        let mut looping = false;
        let mut placements = Vec::new();
        self.screen.clear();
        self.screen.resize(self.tree.nodes.end(), None);
        if let Some(root) = self.tree.root {
            let window = [0., 0., width, height];
            let base = Context {
                k: 1.,
                offset: [0., 0.],
                opacity: 1.,
                brightness: 1.,
                clip: window,
                round_clip: None,
                in_row: false,
                group_hover: false,
                group_active: false,
            };
            let mut walker = Walker {
                tree: &self.tree,
                layout: &self.layout,
                virtual_lists: &self.virtual_lists,
                bound_rows: &self.bound_rows,
                images: &self.images,
                cull_bounds: &self.cull_bounds,
                cull_special: &self.cull_special,
                hovered: self.hovered,
                pressed: self.pressed,
                focused: self.focused,
                hovered_groups: &self.hovered_groups,
                pressed_groups: &self.pressed_groups,
                node_drag: self.node_drag,
                pointer: self.pointer,
                drag: self.drag,
                reduced_motion: self.reduced_motion,
                keyframes_at: self.keyframes_at,
                hide_caret: self.hide_caret,
                screen: &mut self.screen,
                motion: &mut self.motion,
                text: &mut self.text,
                editor: &mut self.editor,
                editor_key: &mut self.editor_key,
                scroll: &mut self.scroll,
                scroll_requests: &mut self.scroll_requests,
                anchors: &mut self.anchors,
                items: &mut items,
                hits: &mut hits,
                cursors: &mut cursors,
                scrolls: &mut scrolls,
                groups: &mut groups,
                hovers: &mut hovers,
                dismissers: &mut dismissers,
                now,
                ime_area: &mut ime_area,
                window,
                layer: 0,
                layer_root: None,
                deferred: Vec::new(),
                placements: &mut placements,
                queued: 0,
                looping: false,
            };
            walker.node(root, base, scale, max_side);
            walker.draw_deferred(scale, max_side);
            looping = walker.looping;
        }
        self.hits = hits;
        self.cursor_rects = cursors;
        self.scrolls = scrolls;
        self.publish_scrolls();
        self.group_rects = groups;
        self.hover_rects = hovers;
        self.dismiss_rects = dismissers;
        if self.dev_error.is_some() {
            self.render_dev_error(&mut items, [0., 0., width, height]);
        }
        self.last_items_len = items.len();
        (
            Frame {
                items,
                animating: self.motion.animating(now) || looping,
                ime_area,
            },
            placements,
        )
    }

    fn render_dev_error(&mut self, items: &mut Vec<Item>, window: Rect) {
        let Some(error) = self.dev_error.clone() else {
            return;
        };
        let [_, _, width, height] = window;
        // Translucent dark backdrop covering the whole window.
        items.push(Item::Fill {
            rect: window,
            radius: [0.; 4],
            top: [10, 12, 18, 215],
            bottom: [10, 12, 18, 215],
            clip: window,
            round_clip: None,
            mask: None,
        });

        let pad = 20.;
        let card_w = (width - 64.).clamp(240., 760.).min(width - 16.).max(120.);
        let content_w = (card_w - pad * 2.).max(80.);
        let title_style = TextStyle::of(
            &crate::tree::Font {
                size: Some(16.),
                weight: Some(700),
                line_height: Some(1.35),
                ..Default::default()
            },
            [248, 113, 113, 255],
        );
        let loc_style = TextStyle::of(
            &crate::tree::Font {
                size: Some(13.),
                weight: Some(600),
                line_height: Some(1.4),
                ..Default::default()
            },
            [251, 191, 36, 255],
        );
        let body_style = TextStyle::of(
            &crate::tree::Font {
                size: Some(13.5),
                weight: Some(400),
                line_height: Some(1.45),
                ..Default::default()
            },
            [241, 245, 249, 255],
        );
        let frame_style = TextStyle::of(
            &crate::tree::Font {
                size: Some(12.5),
                weight: Some(400),
                line_height: Some(1.45),
                ..Default::default()
            },
            [203, 213, 225, 255],
        );
        let hint_style = TextStyle::of(
            &crate::tree::Font {
                size: Some(12.),
                weight: Some(400),
                line_height: Some(1.35),
                ..Default::default()
            },
            [148, 163, 184, 255],
        );

        let mut measure = |text: &str, max_w: f32, style: &TextStyle| -> f32 {
            self.text
                .measure_plain(text, Some(max_w), style)[1]
                .max(style.line_height)
        };

        let title_h = measure(&error.title, content_w, &title_style);
        let loc_h = error
            .location
            .as_deref()
            .map(|l| measure(l, content_w, &loc_style));
        let msg_h = measure(&error.message, content_w, &body_style);
        let frame_pad = 12.;
        let frame_h = error
            .frame
            .as_deref()
            .map(|f| measure(f, (content_w - frame_pad * 2.).max(60.), &frame_style));
        let hint = "Press Esc to dismiss · save a file to reload";
        let hint_h = measure(hint, content_w, &hint_style);

        let mut inner_h = title_h + 10. + msg_h + 14. + hint_h;
        if let Some(h) = loc_h {
            inner_h += h + 6.;
        }
        if let Some(h) = frame_h {
            inner_h += h + frame_pad * 2. + 12.;
        }
        let card_h = (inner_h + pad * 2. + 4.).min((height - 48.).max(140.));
        let card_x = ((width - card_w) / 2.).max(8.);
        let card_y = ((height - card_h) / 2.).clamp(16., 72.);
        let card = [card_x, card_y, card_w, card_h];
        let card_radius = [10.; 4];
        let card_clip = intersect(card, window);

        items.push(Item::Shadow {
            rect: card,
            radius: 10.,
            shadow: ShadowSpec {
                y: 12.,
                blur: 32.,
                spread: 0.,
                color: [0, 0, 0, 160],
            },
            clip: window,
            round_clip: None,
        });
        items.push(Item::Fill {
            rect: card,
            radius: card_radius,
            top: [24, 27, 36, 255],
            bottom: [20, 23, 31, 255],
            clip: window,
            round_clip: None,
            mask: None,
        });
        // Red top bar clipped to the rounded card corners.
        items.push(Item::Fill {
            rect: [card_x, card_y, card_w, 4.],
            radius: [0.; 4],
            top: [239, 68, 68, 255],
            bottom: [239, 68, 68, 255],
            clip: card_clip,
            round_clip: Some((card, card_radius)),
            mask: None,
        });
        items.push(Item::Inset {
            rect: card,
            radius: card_radius,
            spread: 1.,
            offset: [0., 0.],
            color: [255, 255, 255, 24],
            clip: window,
            round_clip: None,
            mask: None,
        });

        let mut y = card_y + pad + 4.;
        let x = card_x + pad;
        items.push(Item::Text {
            rect: [x, y, content_w, title_h],
            text: error.title,
            spans: None,
            style: title_style,
            wrap: true,
            zoom: 1.,
            clip: card_clip,
            round_clip: Some((card, card_radius)),
        });
        y += title_h + 6.;

        if let (Some(loc), Some(h)) = (error.location, loc_h) {
            items.push(Item::Text {
                rect: [x, y, content_w, h],
                text: loc,
                spans: None,
                style: loc_style,
                wrap: true,
                zoom: 1.,
                clip: card_clip,
                round_clip: Some((card, card_radius)),
            });
            y += h + 6.;
        }

        items.push(Item::Text {
            rect: [x, y, content_w, msg_h],
            text: error.message,
            spans: None,
            style: body_style,
            wrap: true,
            zoom: 1.,
            clip: card_clip,
            round_clip: Some((card, card_radius)),
        });
        y += msg_h + 12.;

        if let (Some(frame), Some(h)) = (error.frame, frame_h) {
            let well = [x, y, content_w, h + frame_pad * 2.];
            items.push(Item::Fill {
                rect: well,
                radius: [6.; 4],
                top: [14, 16, 23, 255],
                bottom: [14, 16, 23, 255],
                clip: card_clip,
                round_clip: Some((card, card_radius)),
                mask: None,
            });
            items.push(Item::Text {
                rect: [
                    x + frame_pad,
                    y + frame_pad,
                    (content_w - frame_pad * 2.).max(60.),
                    h,
                ],
                text: frame,
                spans: None,
                style: frame_style,
                wrap: true,
                zoom: 1.,
                clip: intersect(well, card_clip),
                round_clip: Some((well, [6.; 4])),
            });
            y += well[3] + 12.;
        }

        items.push(Item::Text {
            rect: [x, y, content_w, hint_h],
            text: hint.into(),
            spans: None,
            style: hint_style,
            wrap: true,
            zoom: 1.,
            clip: card_clip,
            round_clip: Some((card, card_radius)),
        });
    }

    fn publish_placements(&mut self, placements: Vec<(u16, Placement)>) {
        let mut before = std::mem::take(&mut self.placements);
        // The walker collected these in layer and document order. Missing panels forget their last placement.
        for (id, placement) in placements {
            if before.remove(&id).as_ref() != Some(&placement) {
                let side = match placement.side {
                    Side::Top => "top",
                    Side::Right => "right",
                    Side::Bottom => "bottom",
                    Side::Left => "left",
                };
                let payload = serde_json::json!({
                    "callback": placement.subscription.callback,
                    "token": placement.subscription.token,
                    "side": side,
                    "anchorOffset": placement.anchor_offset,
                });
                let event = Event {
                    callback: HOST_CALLBACK,
                    text: format!("{PLACEMENT_PREFIX}{payload}"),
                };
                // A poll can free slots after trying the backlog. Do not jump ahead of older placements.
                if self.backlog.is_empty() {
                    self.send(event);
                } else {
                    self.backlog.push(self.queued_event(event));
                }
            }
            self.placements.insert(id, placement);
        }
    }

    // Sends the scroll state that changed since the last frame to JS: the `scroll` columns, and the `scroll` callbacks
    // of the boxes whose offset changed. Once per frame, as the columns are read between callbacks.
    fn publish_scrolls(&mut self) {
        let mut ops = Vec::new();
        let mut events = Vec::new();
        for s in &self.scrolls {
            let scroll = [s.top, s.max + s.view, s.view];
            let before = self.tree.scroll(s.id);
            if before == scroll {
                continue;
            }
            ops.push(Op::SetScroll { id: s.id, scroll });
            if (s.list_generation.is_some() || before[0] != s.top)
                && let Some(Node::Box { scroll: callback, .. } | Node::List { scroll: callback, .. }) = self.tree.nodes.get(&s.id)
                && *callback != 0
            {
                events.push((*callback, scroll));
            }
        }
        if ops.is_empty() {
            return;
        }
        let batch = Batch { base: 0, ops };
        self.tree.apply_ui_ref(&batch.ops).expect("scroll batch");
        let _ = self.bridge.log.send(Log::Ui(batch));
        for (callback, [top, height, client]) in events {
            self.send_payload(
                callback,
                EventPayload::Scroll {
                    top,
                    height,
                    client,
                },
            );
        }
    }

    // Whether the pointer is in `rect` and nothing on a higher layer covers it: what a group or a hover box reacts to.
    fn pointed(&self, top: Option<(u16, i32)>, rect: Rect, layer: i32, id: u16) -> bool {
        self.pointer
            .is_some_and(|p| contains(rect, p) && self.row_input_at(id, Some(p)))
            && !self.covered(top, layer, id)
    }

    // Recomputes what the pointer is over, from the last frame: the hovered node, the group nodes and the boxes with
    // a hover callback. Calls the hover callbacks of the boxes it entered or left. Returns whether any of it changed.
    fn refresh_hover(&mut self) -> bool {
        if self.pointer.is_none()
            && self.hovered.is_none()
            && self.hovered_groups.is_empty()
            && self.hovered_boxes.is_empty()
        {
            return false;
        }
        let top = self.pointer.and_then(|p| self.top_hit(p));
        let hovered = self
            .pointer
            .and_then(|p| self.primary_target(p))
            .filter(|id| self.interactive(*id));
        let groups: Vec<u16> = if self.pointer.is_none() || self.group_rects.is_empty() {
            Vec::new()
        } else {
            self.group_rects
                .iter()
                .filter(|(id, rect, layer)| self.pointed(top, *rect, *layer, *id))
                .map(|(id, _, _)| *id)
                .collect()
        };
        if self.hover_rects.is_empty() && self.hovered_boxes.is_empty() {
            let changed = hovered != self.hovered || groups != self.hovered_groups;
            self.hovered = hovered;
            self.hovered_groups = groups;
            return changed;
        }
        let boxes: Vec<u16> = if self.pointer.is_none() {
            Vec::new()
        } else {
            self.hover_rects
                .iter()
                .filter(|(id, rect, layer)| self.pointed(top, *rect, *layer, *id))
                .map(|(id, _, _)| *id)
                .collect()
        };
        let entered: Vec<u16> = boxes
            .iter()
            .filter(|id| !self.hovered_boxes.contains(id))
            .copied()
            .collect();
        let left: Vec<u16> = self
            .hovered_boxes
            .iter()
            .filter(|id| !boxes.contains(id))
            .copied()
            .collect();
        let changed = hovered != self.hovered
            || groups != self.hovered_groups
            || !entered.is_empty()
            || !left.is_empty();
        self.hovered = hovered;
        self.hovered_groups = groups;
        self.hovered_boxes = boxes;
        for id in entered {
            self.hover_event(id, "1");
        }
        for id in left {
            self.hover_event(id, "0");
        }
        changed
    }

    // Tells a box's hover callback that the pointer entered or left it. A box that was removed has nothing to tell.
    fn hover_event(&mut self, id: u16, text: &str) {
        if let Some(Node::Box { hover, .. }) = self.tree.nodes.get(&id)
            && *hover != 0
        {
            let callback = *hover;
            self.send(Event {
                callback,
                text: text.into(),
            });
        }
    }

    // Hover and active states of a node: its own, or those of its nearest ancestor with `group`.
    fn interaction(&self, id: u16) -> (bool, bool) {
        let group = |g: u16| {
            let hovered = self.hovered_groups.contains(&g);
            (hovered, hovered && self.pressed_groups.contains(&g))
        };
        let dragging = self.node_drag.is_some_and(|d| d.id == id);
        let own = (
            self.hovered == Some(id) || dragging,
            (self.pressed == Some(id) && self.hovered == Some(id)) || dragging,
        );
        let (ancestor_hover, ancestor_active) =
            self.nearest_group(id).map_or((false, false), group);
        let (self_hover, self_active) = group(id);
        (
            own.0 || ancestor_hover || self_hover,
            own.1 || ancestor_active || self_active,
        )
    }

    // The nearest strict ancestor whose style sets `group`.
    fn nearest_group(&self, id: u16) -> Option<u16> {
        let mut ancestor = self.tree.parent(id);
        while let Some(a) = ancestor {
            if self
                .tree
                .nodes
                .get(&a)
                .and_then(Node::style)
                .is_some_and(|s| s.group)
            {
                return Some(a);
            }
            ancestor = self.tree.parent(a);
        }
        None
    }

    // The variant a node shows now: hover, then active, then focus, each overriding the one before.
    fn variant(&self, id: u16, paint: &Paint, disabled: bool) -> Variant {
        if paint.hover.is_none() && paint.active.is_none() && paint.focus.is_none() {
            return Variant::default();
        }
        let (hover, active) = self.interaction(id);
        resolve_variant(paint, disabled, hover, active, self.focused == Some(id))
    }

    // The height a variant gives a node now, if any.
    fn height_variant(&self, id: u16) -> Option<f32> {
        let node = self.tree.nodes.get(&id)?;
        let paint = node.paint()?;
        let disabled = matches!(node, Node::Box { disabled: true, .. });
        self.variant(id, paint, disabled).height
    }

    // Taffy layout with animated heights: a node with a `height` transition animates from its height on
    // screen to its new natural height, and pinning that height each frame moves what follows it.
    fn lay_out(&mut self, width: f32, height: f32, now: f64) {
        if self.sized_nodes.is_empty() && self.virtual_lists.is_empty() {
            if !self.layout.is_dirty(width, height) {
                return;
            }
            if self.layout.has_pinned() {
                self.layout.pin_heights(&self.tree, &HashMap::new());
            }
        }
        // Nodes with a height transition, a height in a variant or a height keyframe: their heights are pinned while
        // they change.
        let sized: Vec<(u16, Option<Transition>)> = self
            .sized_nodes
            .iter()
            .filter_map(|&id| {
                let paint = self.tree.nodes.get(&id)?.paint()?;
                let transition = transition(paint, HEIGHT_TRANSITION).cloned();
                Some((id, transition))
            })
            .collect();
        let compute = |layout: &mut Layout,
                       tree: &Tree,
                       text: &mut TextEngine,
                       lists: &mut HashMap<u16, (u64, VirtualList)>| {
            if lists.is_empty() {
                layout.compute(tree, text, width, height)
            } else {
                for (_, list) in lists.values_mut() {
                    if let Some(auto) = &mut list.auto {
                        auto.preview = None;
                    }
                }
                layout.compute_with_rows(tree, text, width, height, |id, height| {
                    let (_, list) = lists.get_mut(&id)?;
                    let (offset, extent) = list.preview_layout(height);
                    let mut rows: Vec<_> = list
                        .bindings
                        .iter()
                        .filter_map(|(&key, &row)| {
                            let (top, pixels) = list.rows.visible_row_at(key, offset, extent)?;
                            Some((row, top, pixels))
                        })
                        .collect();
                    // Restore visual order before narrowing offsets to f32.
                    rows.sort_unstable_by(|a, b| a.1.total_cmp(&b.1));
                    Some(
                        rows.into_iter()
                            .map(|(row, top, pixels)| (row, (top - offset) as f32, (!list.measured).then_some(pixels as f32)))
                            .collect(),
                    )
                })
            }
        };
        let mut changed = if sized.is_empty() {
            compute(
                &mut self.layout,
                &self.tree,
                &mut self.text,
                &mut self.virtual_lists,
            )
        } else {
            self.layout.pin_heights(&self.tree, &HashMap::new());
            let natural = compute(
                &mut self.layout,
                &self.tree,
                &mut self.text,
                &mut self.virtual_lists,
            );
            let natural_height = |id: u16| {
                natural
                    .iter()
                    .find(|(n, _)| *n == id)
                    .map_or(self.tree.rect(id)[3], |(_, r)| r[3])
            };
            let mut pins = HashMap::new();
            for (id, transition) in &sized {
                let id = *id;
                let rest = natural_height(id);
                let target = self.height_variant(id).unwrap_or(rest);
                let mut shown = self
                    .motion
                    .show(
                        id,
                        HEIGHT_TRANSITION,
                        Value::Number(target),
                        transition.as_ref(),
                        now,
                    )
                    .number();
                let animations = self
                    .tree
                    .nodes
                    .get(&id)
                    .and_then(Node::paint)
                    .and_then(|p| p.animations.as_deref());
                let keyed = self.motion.keyframes(
                    id,
                    animations.unwrap_or(&[]),
                    self.reduced_motion,
                    self.keyframes_at,
                    now,
                );
                if let Some((_, height)) = keyed.values.iter().find(|(p, _)| *p == Property::Height)
                {
                    shown = height.number();
                }
                if (shown - rest).abs() > 0.01 {
                    pins.insert(id, shown);
                }
            }
            if pins.is_empty() {
                natural
            } else {
                self.layout.pin_heights(&self.tree, &pins);
                // Relative to the tree's rectangles, which still hold the previous frame.
                compute(
                    &mut self.layout,
                    &self.tree,
                    &mut self.text,
                    &mut self.virtual_lists,
                )
            }
        };
        changed.retain(|(id, rect)| self.tree.rect(*id) != *rect);
        if changed.is_empty() {
            return;
        }
        let batch = Batch {
            base: 0,
            ops: changed
                .into_iter()
                .map(|(id, rect)| Op::SetRect { id, rect })
                .collect(),
        };
        self.tree.apply_ui_ref(&batch.ops).expect("layout batch");
        self.cull_cache_dirty = true;
        let _ = self.bridge.log.send(Log::Ui(batch));
    }
}

fn resolve_variant(
    paint: &Paint,
    disabled: bool,
    hover: bool,
    active: bool,
    focused: bool,
) -> Variant {
    let applied = [
        (!disabled && hover, &paint.hover),
        (!disabled && active, &paint.active),
        (focused, &paint.focus),
    ];
    let mut out = Variant::default();
    for (on, variant) in applied {
        if on && let Some(v) = variant {
            out = Variant {
                background: v.background.or(out.background),
                gradient: v.gradient.or(out.gradient),
                brightness: v.brightness.or(out.brightness),
                scale: v.scale.or(out.scale),
                opacity: v.opacity.or(out.opacity),
                x: v.x.or(out.x),
                y: v.y.or(out.y),
                color: v.color.or(out.color),
                edge: v.edge.or(out.edge),
                height: v.height.or(out.height),
                width: v.width.or(out.width),
                underline: v.underline.or(out.underline),
                cursor: v.cursor.or(out.cursor),
            };
        }
    }
    out
}

fn seed_motion_from_paint(motion: &mut Motion, id: u16, paint: &Paint, v: &Variant, rect: Rect) {
    let opacity = v.opacity.or(paint.opacity).unwrap_or(1.);
    let (x, y) = (v.x.unwrap_or(paint.x), v.y.unwrap_or(paint.y));
    let scale = v.scale.or(paint.scale).unwrap_or(1.);
    let brightness = v.brightness.or(paint.brightness).unwrap_or(1.);
    let draw = paint.draw.unwrap_or(1.);
    let width = v.width.unwrap_or(rect[2]);
    let background = v
        .background
        .or(paint.background)
        .map_or(Value::Color([0.; 4]), Value::color);
    let edge = v
        .edge
        .or(paint.edge)
        .map_or(Value::Color([0.; 4]), Value::color);
    let color = v
        .color
        .or(paint.color)
        .map_or(Value::Color([0.; 4]), Value::color);
    motion.seed(id, Property::Opacity, Value::Number(opacity));
    motion.seed(id, Property::X, Value::Number(x));
    motion.seed(id, Property::Y, Value::Number(y));
    motion.seed(id, Property::Scale, Value::Number(scale));
    motion.seed(id, Property::Brightness, Value::Number(brightness));
    motion.seed(id, Property::Draw, Value::Number(draw));
    motion.seed(id, Property::Left, Value::Number(rect[0]));
    motion.seed(id, Property::Width, Value::Number(width));
    motion.seed(id, Property::Background, background);
    motion.seed(id, Property::Edge, edge);
    motion.seed(id, Property::Color, color);
}

fn variant_affects_geometry(paint: &Paint) -> bool {
    [&paint.hover, &paint.active, &paint.focus]
        .into_iter()
        .flatten()
        .any(|v| {
            v.x.is_some()
                || v.y.is_some()
                || v.scale.is_some()
                || v.width.is_some()
                || v.height.is_some()
        })
}

fn node_is_cull_special(node: &Node, has_active_motion: bool) -> bool {
    if has_active_motion || matches!(node, Node::Input { .. } | Node::List { .. }) {
        return true;
    }
    if let Some(s) = node.style()
        && (s.position == Position::Fixed
            || s.layer.is_some()
            || s.scroll
            || s.follow.is_some())
    {
        return true;
    }
    if let Some(p) = node.paint()
        && (p.x != 0.
            || p.y != 0.
            || p.scale.is_some_and(|s| s != 1.)
            || !p.shadows.is_empty()
            || !p.transitions.is_empty()
            || p.animations.as_ref().is_some_and(|a| !a.is_empty())
            || p.starting.is_some()
            || variant_affects_geometry(p))
    {
        return true;
    }
    false
}

fn within_tree(tree: &Tree, mut node: u16, ancestor: u16) -> bool {
    loop {
        if node == ancestor {
            return true;
        }
        match tree.parent(node) {
            Some(parent) => node = parent,
            None => return false,
        }
    }
}

fn input_content_of(tree: &Tree, id: u16) -> Rect {
    let [x, y, w, h] = tree.rect(id);
    let [top, right, bottom, left] = tree
        .nodes
        .get(&id)
        .and_then(Node::style)
        .map_or([0.; 4], |s| s.padding.sides());
    [
        x + left,
        y + top,
        (w - left - right).max(0.),
        (h - top - bottom).max(0.),
    ]
}

fn nearest_group_of(tree: &Tree, id: u16) -> Option<u16> {
    let mut ancestor = tree.parent(id);
    while let Some(a) = ancestor {
        if tree
            .nodes
            .get(&a)
            .and_then(Node::style)
            .is_some_and(|s| s.group)
        {
            return Some(a);
        }
        ancestor = tree.parent(a);
    }
    None
}

fn is_height_animated(node: &Node) -> bool {
    let Some(paint) = node.paint() else {
        return false;
    };
    transition(paint, HEIGHT_TRANSITION).is_some()
        || [&paint.hover, &paint.active, &paint.focus]
            .into_iter()
            .flatten()
            .any(|v| v.height.is_some())
        || paint
            .animations
            .iter()
            .flatten()
            .any(|a| a.property == Property::Height)
}

fn transition(paint: &Paint, property: Property) -> Option<&Transition> {
    paint.transitions.iter().find(|t| t.property == property)
}

// The wheel's glide and the scrollbar's fade: 150 ms, ease-out.
fn ease_out(property: Property) -> Transition {
    Transition {
        property,
        duration: 150.,
        easing: Easing::EaseOut,
        ..Default::default()
    }
}

// Track and thumb of a scrollbar, in screen pixels, of a scroll node drawn at `screen` (scaled by `k`). It
// shows `view` of `content` logical pixels, scrolled to `offset` of at most `max`. The track is `inset` from the
// right, top and bottom edges. The thumb is the visible share of the content, at least `min_thumb` long, and
// moves along the track with the offset.
fn scrollbar_rects(
    bar: &Scrollbar,
    screen: Rect,
    k: f32,
    view: f32,
    content: f32,
    offset: f32,
    max: f32,
) -> (Rect, Rect) {
    let [x, y, w, h] = screen;
    let inset = bar.inset * k;
    let track = [
        x + w - inset - bar.width * k,
        y + inset,
        bar.width * k,
        (h - 2. * inset).max(0.),
    ];
    let length = ((view * view / content).max(bar.min_thumb) * k).min(track[3]);
    let top = track[1] + (track[3] - length) * (offset / max);
    (track, [track[0], top, track[2], length])
}

fn contains([x, y, w, h]: Rect, [px, py]: [f32; 2]) -> bool {
    px >= x && px < x + w && py >= y && py < y + h
}

fn intersect(a: Rect, b: Rect) -> Rect {
    let x0 = a[0].max(b[0]);
    let y0 = a[1].max(b[1]);
    let x1 = (a[0] + a[2]).min(b[0] + b[2]);
    let y1 = (a[1] + a[3]).min(b[1] + b[3]);
    [x0, y0, (x1 - x0).max(0.), (y1 - y0).max(0.)]
}

// Whether the topmost thing under the pointer is on a layer above `layer`.
fn higher(top: Option<(u16, i32)>, layer: i32) -> bool {
    top.is_some_and(|(_, l)| l > layer)
}

// Margin kept between a fixed node and the window's edges.
const WINDOW_MARGIN: f32 = 8.;

// Where a node of `size` goes next to the on-screen rectangle of its anchor, in the window: on the anchor's side at
// its gap, aligned along the other axis. It flips to the opposite side when its side overflows the window and the
// opposite side fits. Then it is kept inside the window, `WINDOW_MARGIN` from its edges (top-left first).
fn place(
    anchor: &Anchor,
    [ax, ay, aw, ah]: Rect,
    [w, h]: [f32; 2],
    window: Rect,
) -> ([f32; 2], Side) {
    let [wx, wy, ww, wh] = window;
    let gap = anchor.gap;
    let fits = |side: Side| match side {
        Side::Top => ay - gap - h >= wy,
        Side::Bottom => ay + ah + gap + h <= wy + wh,
        Side::Left => ax - gap - w >= wx,
        Side::Right => ax + aw + gap + w <= wx + ww,
    };
    let opposite = |side: Side| match side {
        Side::Top => Side::Bottom,
        Side::Bottom => Side::Top,
        Side::Left => Side::Right,
        Side::Right => Side::Left,
    };
    let side = if anchor.flip && !fits(anchor.side) && fits(opposite(anchor.side)) {
        opposite(anchor.side)
    } else {
        anchor.side
    };
    let along = |start: f32, len: f32, size: f32| match anchor.align {
        AnchorAlign::Start => start,
        AnchorAlign::Center => start + (len - size) / 2.,
        AnchorAlign::End => start + len - size,
    };
    let [x, y] = match side {
        Side::Top => [along(ax, aw, w), ay - gap - h],
        Side::Bottom => [along(ax, aw, w), ay + ah + gap],
        Side::Left => [ax - gap - w, along(ay, ah, h)],
        Side::Right => [ax + aw + gap, along(ay, ah, h)],
    };
    (
        [
            x.min(wx + ww - WINDOW_MARGIN - w).max(wx + WINDOW_MARGIN),
            y.min(wy + wh - WINDOW_MARGIN - h).max(wy + WINDOW_MARGIN),
        ],
        side,
    )
}

#[derive(PartialEq)]
struct Placement {
    subscription: PlacementSubscription,
    side: Side,
    anchor_offset: f32,
}

// What a node inherits from its ancestors: a uniform scale `k` and offset, group opacity, brightness, clip,
// and the nearest ancestor `group`'s hover/active state (propagated in O(1) instead of walking parents).
#[derive(Clone, Copy)]
struct Context {
    k: f32,
    offset: [f32; 2],
    opacity: f32,
    brightness: f32,
    clip: Rect,
    // Rounded rectangle and corner radius of the innermost rounded clip; None for none.
    round_clip: Option<(Rect, Corners)>,
    in_row: bool,
    group_hover: bool,
    group_active: bool,
}
impl Context {
    fn rect(&self, [x, y, w, h]: Rect) -> Rect {
        [
            x * self.k + self.offset[0],
            y * self.k + self.offset[1],
            w * self.k,
            h * self.k,
        ]
    }
    fn color(&self, rgba: [f32; 4]) -> Rgba {
        let [r, g, b, a] = rgba;
        let lit = |c: f32| (c * self.brightness).round().clamp(0., 255.) as u8;
        [
            lit(r),
            lit(g),
            lit(b),
            (a * self.opacity).round().clamp(0., 255.) as u8,
        ]
    }
}

struct Walker<'a> {
    tree: &'a Tree,
    layout: &'a Layout,
    virtual_lists: &'a HashMap<u16, (u64, VirtualList)>,
    bound_rows: &'a HashMap<u16, (u16, u64)>,
    images: &'a HashMap<String, ImageData>,
    cull_bounds: &'a [[f32; 4]],
    cull_special: &'a [bool],
    hovered: Option<u16>,
    pressed: Option<u16>,
    focused: Option<u16>,
    hovered_groups: &'a [u16],
    pressed_groups: &'a [u16],
    node_drag: Option<NodeDrag>,
    pointer: Option<[f32; 2]>,
    drag: Option<(u16, f32)>,
    reduced_motion: bool,
    keyframes_at: Option<f32>,
    hide_caret: bool,
    screen: &'a mut Vec<Option<Rect>>,
    motion: &'a mut Motion,
    text: &'a mut TextEngine,
    editor: &'a mut LineEditor,
    editor_key: &'a mut Option<EditorBitmapKey>,
    scroll: &'a mut HashMap<u16, f32>,
    scroll_requests: &'a mut HashMap<u16, ScrollRequest>,
    anchors: &'a mut HashMap<u16, EndAnchor>,
    items: &'a mut Vec<Item>,
    hits: &'a mut Vec<Hit>,
    cursors: &'a mut Vec<CursorHit>,
    scrolls: &'a mut Vec<Scroll>,
    groups: &'a mut Vec<(u16, Rect, i32)>,
    hovers: &'a mut Vec<(u16, Rect, i32)>,
    dismissers: &'a mut Vec<(u16, Rect, i32)>,
    now: f64,
    ime_area: &'a mut Option<Rect>,
    // The window: what a fixed node is placed in and clipped to.
    window: Rect,
    // Layer of the node being drawn, and the node that starts that layer (its rectangle covers what is below it).
    layer: i32,
    layer_root: Option<u16>,
    // Layered nodes waiting for their turn, and how many were ever queued: their order within a layer.
    deferred: Vec<Deferred>,
    placements: &'a mut Vec<(u16, Placement)>,
    queued: usize,
    // Set when a drawn node runs a keyframe animation: the window keeps drawing.
    looping: bool,
}

// A node drawn after the layers below it. A fixed node is placed when it is drawn; any other keeps the context of
// the place it was met.
struct Deferred {
    id: u16,
    layer: i32,
    order: usize,
    ctx: Option<Context>,
}

// Values a node shows this frame, after its interaction state and its transitions.
struct Shown {
    opacity: f32,
    x: f32,
    y: f32,
    scale: f32,
    brightness: f32,
    background: Option<[f32; 4]>,
    gradient: Option<[u32; 2]>,
    edge: Option<[f32; 4]>,
    color: Option<[f32; 4]>,
    draw: f32,
    left: f32,
    width: f32,
    underline: bool,
    looping: bool,
}

impl<'a> Walker<'a> {
    #[inline]
    fn group_state(&self, id: u16) -> (bool, bool) {
        if self.hovered_groups.is_empty() {
            return (false, false);
        }
        let hovered = self.hovered_groups.contains(&id);
        (hovered, hovered && self.pressed_groups.contains(&id))
    }

    #[inline]
    fn variant(&self, id: u16, paint: &Paint, disabled: bool, ctx: &Context, is_group: bool) -> Variant {
        if paint.hover.is_none() && paint.active.is_none() && paint.focus.is_none() {
            return Variant::default();
        }
        let dragging = self.node_drag.is_some_and(|d| d.id == id);
        let own_hover = self.hovered == Some(id) || dragging;
        let own_active = (self.pressed == Some(id) && self.hovered == Some(id)) || dragging;
        let (self_hover, self_active) = if is_group {
            self.group_state(id)
        } else {
            (false, false)
        };
        let hover = own_hover || ctx.group_hover || self_hover;
        let active = own_active || ctx.group_active || self_active;
        resolve_variant(paint, disabled, hover, active, self.focused == Some(id))
    }

    fn row_can_receive(&self, mut child: u16) -> bool {
        if self.virtual_lists.is_empty() {
            return true;
        }
        while let Some(parent) = self.tree.parent(child) {
            if let Some((_, list)) = self.virtual_lists.get(&parent) {
                let height = self.tree.rect(parent)[3];
                let (offset, extent) = list.frame_viewport(height);
                if !self.bound_rows.get(&child).is_some_and(|&(_, key)| {
                    list.rows.visible_row_at(key, offset, extent).is_some()
                }) {
                    return false;
                }
            }
            child = parent;
        }
        true
    }

    fn screen_rect(&self, id: u16) -> Option<Rect> {
        self.screen
            .get(id as usize)
            .copied()
            .flatten()
            .filter(|_| self.row_can_receive(id))
    }

    fn child_top(&self, id: u16, node: u16) -> Option<f32> {
        within_tree(self.tree, node, id).then(|| self.tree.rect(node)[1] - self.tree.rect(id)[1])
    }

    fn anchor_at(&self, id: u16, offset: f32, max: f32) -> EndAnchor {
        let mut child = None;
        let mut parent = id;
        while let Some(next) = self.tree.children(parent).iter().copied().find(|&c| {
            self.child_top(id, c)
                .is_some_and(|top| top + self.tree.rect(c)[3] > offset)
        }) {
            child = Some(next);
            let straddles = self.child_top(id, next).is_some_and(|top| top < offset);
            if straddles && matches!(self.tree.nodes.get(&next), Some(Node::Box { .. })) {
                parent = next;
            } else {
                break;
            }
        }
        let gap = child
            .and_then(|child| self.child_top(id, child))
            .map_or(0., |top| top - offset);
        EndAnchor {
            child,
            gap,
            offset,
            bottom: offset >= max - 0.5,
        }
    }

    fn request(&mut self, id: u16, request: ScrollRequest, max: f32, now: f64) {
        let current = self.scroll.get(&id).copied().unwrap_or(0.);
        let target = request
            .offset
            .unwrap_or(match request.to {
                Some(ScrollEdge::End) => max,
                _ => 0.,
            })
            .clamp(0., max);
        if request.smooth {
            let transition = ease_out(Property::Y);
            self.motion.start(
                id,
                Channel::Scroll,
                Value::Number(current),
                Value::Number(target),
                &transition,
                now,
            );
        } else {
            self.motion.release(id, Channel::Scroll);
            self.scroll.insert(id, target);
        }
    }

    fn scroll_offset(&mut self, id: u16, max: f32, now: f64) -> f32 {
        let offset = match self.motion.target(id, Channel::Scroll) {
            Some(target) => {
                let target = Value::Number(target.number().clamp(0., max));
                self.motion
                    .show(
                        id,
                        Channel::Scroll,
                        target,
                        Some(&ease_out(Property::Y)),
                        now,
                    )
                    .number()
            }
            None => self.scroll.get(&id).copied().unwrap_or(0.).clamp(0., max),
        };
        self.scroll.insert(id, offset);
        offset
    }

    fn frame_offset(&mut self, id: u16, max: f32, now: f64, end: bool) -> f32 {
        let requested = match self.scroll_requests.remove(&id) {
            Some(request) => {
                self.request(id, request, max, now);
                true
            }
            None => false,
        };
        let base = self.scroll_offset(id, max, now);
        if !end {
            return base;
        }
        // An offset moved by a glide, a wheel or a `scrollTo` since the last frame is kept as it is.
        let kept = if requested {
            base
        } else {
            match self.anchors.get(&id) {
                None => max,
                Some(anchor) if (base - anchor.offset).abs() > 0.5 => base,
                Some(anchor) if anchor.bottom => max,
                Some(anchor) => anchor
                    .child
                    .and_then(|child| self.child_top(id, child))
                    .map_or(base, |top| top - anchor.gap),
            }
        }
        .clamp(0., max);
        if kept != base {
            // The target of a finished glide stays set: it would move the offset back.
            self.motion.release(id, Channel::Scroll);
        }
        let anchor = self.anchor_at(id, kept, max);
        self.anchors.insert(id, anchor);
        self.scroll.insert(id, kept);
        kept
    }

    fn shown(
        &mut self,
        id: u16,
        paint: &Paint,
        rect: Rect,
        disabled: bool,
        ctx: &Context,
        is_group: bool,
    ) -> Shown {
        let v = self.variant(id, paint, disabled, ctx, is_group);
        let background = v.background.or(paint.background);
        let brightness = v.brightness.or(paint.brightness).unwrap_or(1.);
        let scale = v.scale.or(paint.scale).unwrap_or(1.);
        let edge = v.edge.or(paint.edge);
        let opacity = v.opacity.or(paint.opacity).unwrap_or(1.);
        let (x, y) = (v.x.unwrap_or(paint.x), v.y.unwrap_or(paint.y));
        let text_color = v.color.or(paint.color);
        let draw_base = paint.draw.unwrap_or(1.);
        let width_base = v.width.unwrap_or(rect[2]);

        let has_transitions = !paint.transitions.is_empty();
        let has_animations = paint.animations.as_ref().is_some_and(|a| !a.is_empty());
        let has_starting = paint.starting.is_some();
        let has_active = self.motion.has_active(id);

        // Fast path: static node with no transitions, animations, starting state, or active motion.
        // Skips all 11 Motion HashMap operations completely.
        if !has_transitions && !has_animations && !has_starting && !has_active {
            return Shown {
                opacity,
                x,
                y,
                scale,
                brightness,
                background: background.map(|c| Value::color(c).rgba()),
                gradient: v.gradient.or(paint.gradient),
                edge: edge.map(|c| Value::color(c).rgba()),
                color: text_color.map(|c| Value::color(c).rgba()),
                draw: draw_base,
                left: rect[0],
                width: width_base,
                underline: v.underline.unwrap_or(false),
                looping: false,
            };
        }

        let now = self.now;
        let reduced = self.reduced_motion;
        let motion = &mut *self.motion;
        // First time on screen: properties with a starting value animate from it.
        if let Some(starting) = &paint.starting
            && motion.target(id, Property::Opacity).is_none()
        {
            let from = [
                (Property::Opacity, starting.opacity, opacity),
                (Property::X, starting.x, x),
                (Property::Y, starting.y, y),
                (Property::Scale, starting.scale, scale),
                (Property::Draw, starting.draw, draw_base),
            ];
            for (property, start, target) in from {
                if let (Some(start), Some(t)) = (start, transition(paint, property)) {
                    motion.start(
                        id,
                        property,
                        Value::Number(start),
                        Value::Number(target),
                        t,
                        now,
                    );
                }
            }
        }
        // Keyframe values override the base values while their animations run.
        let keyed = if has_animations || has_active {
            motion.keyframes(
                id,
                paint.animations.as_deref().unwrap_or(&[]),
                reduced,
                self.keyframes_at,
                now,
            )
        } else {
            Default::default()
        };
        let overridden = |property: Property| {
            keyed
                .values
                .iter()
                .find(|(p, _)| *p == property)
                .map(|(_, v)| *v)
        };
        let mut number = |property, value: f32| {
            let tr = transition(paint, property);
            let shown = if tr.is_some()
                || has_active
                || (has_starting && property == Property::Opacity)
            {
                motion
                    .show(id, property, Value::Number(value), tr, now)
                    .number()
            } else {
                value
            };
            overridden(property).map_or(shown, Value::number)
        };
        let opacity = number(Property::Opacity, opacity);
        let x = number(Property::X, x);
        let y = number(Property::Y, y);
        let scale = number(Property::Scale, scale);
        let brightness = number(Property::Brightness, brightness);
        let draw = number(Property::Draw, draw_base);
        let left = number(Property::Left, rect[0]);
        let width = number(Property::Width, width_base);
        // A color animates from transparent when it appears.
        let mut color = |property, value: Option<u32>| {
            let tr = transition(paint, property);
            if tr.is_none() && !has_active {
                return match overridden(property) {
                    Some(v) => Some(v.rgba()),
                    None => value.map(|c| Value::color(c).rgba()),
                };
            }
            let target = value.map_or(Value::Color([0.; 4]), Value::color);
            let shown = motion.show(id, property, target, tr, now).rgba();
            match overridden(property) {
                Some(v) => Some(v.rgba()),
                None => (value.is_some() || shown[3] > 0.).then_some(shown),
            }
        };
        // A fade keeps running while the node is at 0, so the window keeps drawing until the fade moves on.
        let fades = paint
            .animations
            .iter()
            .flatten()
            .any(|a| a.property == Property::Opacity);
        Shown {
            looping: keyed.running && (fades || opacity > 0.001),
            opacity,
            x,
            y,
            scale,
            brightness,
            background: color(Property::Background, background),
            gradient: v.gradient.or(paint.gradient),
            edge: color(Property::Edge, edge),
            color: color(Property::Color, text_color),
            draw,
            left,
            width,
            underline: v.underline.unwrap_or(false),
        }
    }

    // Draws a node in its place, or queues it when it starts a layer of its own: a fixed node, or one on another layer.
    fn node(&mut self, id: u16, ctx: Context, scale: f32, max_side: usize) {
        // Sound O(1) subtree clip cull: when neither `id` nor any descendant has layers, fixed overlays,
        // transforms, shadows, inputs, scroll containers, anchors, or animations, `cull_bounds[id]` strictly
        // bounds the entire subtree in `ctx` coordinates.
        if !ctx.in_row && !self.cull_special.get(id as usize).copied().unwrap_or(true) {
            let screen_bounds = ctx.rect(self.cull_bounds[id as usize]);
            let on = intersect(screen_bounds, ctx.clip);
            if on[2] <= 0. || on[3] <= 0. {
                return;
            }
        }
        let style = self.tree.nodes.get(&id).and_then(Node::style);
        let fixed = style.is_some_and(|s| s.position == Position::Fixed);
        let layer = style.and_then(|s| s.layer.or(fixed.then_some(1)));
        let fixed = fixed && !ctx.in_row;
        if let Some(layer) = layer
            && (fixed || layer != self.layer)
        {
            let ctx = (!fixed).then_some(ctx);
            self.deferred.push(Deferred {
                id,
                layer,
                order: self.queued,
                ctx,
            });
            self.queued += 1;
            return;
        }
        self.draw(id, ctx, scale, max_side);
    }

    // Draws the queued layers, lowest first and in document order within a layer. A layer's own layered children are
    // queued in turn, so they are drawn after the lower layers and before the higher ones.
    fn draw_deferred(&mut self, scale: f32, max_side: usize) {
        while let Some(next) = (0..self.deferred.len())
            .min_by_key(|&i| (self.deferred[i].layer, self.deferred[i].order))
        {
            let Deferred { id, layer, ctx, .. } = self.deferred.swap_remove(next);
            let ctx = match ctx {
                Some(ctx) => ctx,
                None => match self.fixed_context(id) {
                    Some(ctx) => ctx,
                    None => continue,
                },
            };
            self.layer = layer;
            self.layer_root = Some(id);
            self.draw(id, ctx, scale, max_side);
        }
        self.layer = 0;
        self.layer_root = None;
    }

    // The context a fixed node is drawn in: its origin is where it is placed this frame. None while its anchor is not
    // on screen, which hides it.
    fn fixed_context(&mut self, id: u16) -> Option<Context> {
        let style = self.tree.nodes.get(&id)?.style()?;
        let [_, _, w, h] = self.tree.rect(id);
        let [x, y] = match &style.anchor {
            Some(anchor) => {
                let rect = self.screen_rect(anchor.node)?;
                let (offset, side) = place(anchor, rect, [w, h], self.window);
                if let Some(subscription) = &anchor.placement
                    && subscription.callback != 0
                {
                    let axis = match side {
                        Side::Top | Side::Bottom => 0,
                        Side::Left | Side::Right => 1,
                    };
                    let anchor_offset = rect[axis] + rect[axis + 2] / 2. - offset[axis];
                    if anchor_offset.is_finite() {
                        self.placements.push((
                            id,
                            Placement {
                                subscription: subscription.clone(),
                                side,
                                anchor_offset,
                            },
                        ));
                    }
                }
                offset
            }
            None => [style.left.unwrap_or(0.), style.top.unwrap_or(0.)],
        };
        let (group_hover, group_active) =
            nearest_group_of(self.tree, id).map_or((false, false), |g| self.group_state(g));
        Some(Context {
            k: 1.,
            offset: [x, y],
            opacity: 1.,
            brightness: 1.,
            clip: self.window,
            round_clip: None,
            in_row: false,
            group_hover,
            group_active,
        })
    }

    fn draw(&mut self, id: u16, ctx: Context, scale: f32, max_side: usize) {
        let tree = self.tree;
        let layout = self.layout;
        let Some(node) = tree.nodes.get(&id) else {
            return;
        };
        let layout_rect = tree.rect(id);
        let disabled = matches!(node, Node::Box { disabled: true, .. });
        let is_group = node.style().is_some_and(|s| s.group);
        let default_paint = Paint::default();
        let paint = node.paint().unwrap_or(&default_paint);
        let shown = self.shown(id, paint, layout_rect, disabled, &ctx, is_group);
        // On-screen rectangle: layout transitions of `left` and `width`, then the node's own transform.
        let rect = [shown.left, layout_rect[1], shown.width, layout_rect[3]];
        let mut inner = ctx;
        let center = [rect[0] + rect[2] / 2., rect[1] + rect[3] / 2.];
        let s = shown.scale;
        let local = [
            center[0] - s * center[0] + shown.x,
            center[1] - s * center[1] + shown.y,
        ];
        inner.offset = [
            ctx.k * local[0] + ctx.offset[0],
            ctx.k * local[1] + ctx.offset[1],
        ];
        inner.k = ctx.k * s;
        inner.brightness = ctx.brightness * shown.brightness;
        let screen = inner.rect(rect);
        self.screen[id as usize] = Some(screen);
        let has_children = !tree.children(id).is_empty();
        let empty = (screen[2] <= 0. || screen[3] <= 0.) && !has_children;
        // A walked node has visible ancestors, so a loop that is on screen keeps the window drawing.
        let on_screen = intersect(screen, ctx.clip);
        let visible = on_screen[2] > 0. && on_screen[3] > 0.;
        if shown.looping && visible {
            self.looping = true;
        }
        if shown.opacity <= 0.001 || empty {
            return;
        }
        let layer = shown.opacity < 0.999
            && (visible || has_children || !paint.shadows.is_empty());
        if layer {
            self.items.push(Item::PushLayer {
                opacity: shown.opacity,
            });
        }
        let clip = ctx.clip;
        let round_clip = ctx.round_clip;
        // The root of a layer covers what is below it, even where it reacts to nothing.
        if visible && self.layer_root == Some(id) {
            self.hits.push(Hit {
                rect: on_screen,
                id,
                layer: self.layer,
            });
        }
        let radius = paint.radius.corners().map(|r| r * inner.k);
        let shadow_radius = radius.into_iter().fold(0., f32::max);
        // A mask names an image: while that image is missing, the node draws none of its own paint.
        let mask = paint
            .mask
            .as_ref()
            .map(|source| self.images.get(source).cloned());
        let hidden = matches!(mask, Some(None));
        let mask = mask.flatten().map(|image| (image, screen));
        if !hidden {
            if !paint.shadows.is_empty() {
                for shadow in &paint.shadows {
                    let Shadow {
                        y,
                        blur,
                        spread,
                        color,
                    } = *shadow;
                    let sy = y * inner.k;
                    let sblur = blur * inner.k;
                    let sspread = spread * inner.k;
                    let reach = sblur + sspread.max(0.);
                    let shadow_box = [
                        screen[0] - reach,
                        screen[1] + sy - reach,
                        screen[2] + 2. * reach,
                        screen[3] + 2. * reach,
                    ];
                    let shadow_vis = intersect(shadow_box, clip);
                    if shadow_vis[2] <= 0. || shadow_vis[3] <= 0. {
                        continue;
                    }
                    let shadow = ShadowSpec {
                        y: sy,
                        blur: sblur,
                        spread: sspread,
                        color: inner.color(Value::color(color).rgba()),
                    };
                    self.items.push(Item::Shadow {
                        rect: screen,
                        radius: shadow_radius,
                        shadow,
                        clip,
                        round_clip,
                    });
                }
            }
            if visible {
                if let Some([top, bottom]) = shown.gradient {
                    let [top, bottom] =
                        [top, bottom].map(|c| inner.color(Value::color(c).rgba()));
                    self.items.push(Item::Fill {
                        rect: screen,
                        radius,
                        top,
                        bottom,
                        clip,
                        round_clip,
                        mask: mask.clone(),
                    });
                } else if let Some(background) = shown.background {
                    let color = inner.color(background);
                    self.items.push(Item::Fill {
                        rect: screen,
                        radius,
                        top: color,
                        bottom: color,
                        clip,
                        round_clip,
                        mask: mask.clone(),
                    });
                }
                if let Some(edge) = shown.edge {
                    let color = inner.color(edge);
                    if let Some(sides) = paint.edge_sides {
                        // Each side is a band inside the rectangle. The bands do not overlap, so a translucent edge
                        // does not darken where two meet. Without a radius, a band is a plain rectangle. With one, each
                        // piece is the rounded ring clipped to it: the straight strips, and each corner square (radius
                        // by radius) drawn once, by the top or bottom side if it has a width, else by the left or right.
                        // So a corner keeps its arc even when only one of its sides has an edge.
                        let [top, right, bottom, left] = sides.map(|w| w * inner.k);
                        let [x, y, w, h] = screen;
                        let pieces: Vec<(f32, Rect)> = if radius.iter().any(|&r| r > 0.) {
                            let [tl, tr, br, bl] = radius;
                            let corner = |r: f32, across: f32, along: f32| {
                                [r.max(across).min(w / 2.), r.max(along).min(h / 2.)]
                            };
                            let [tl_w, tl_h] = corner(tl, left, top);
                            let [tr_w, tr_h] = corner(tr, right, top);
                            let [br_w, br_h] = corner(br, right, bottom);
                            let [bl_w, bl_h] = corner(bl, left, bottom);
                            let owner = |vertical: f32, horizontal: f32| {
                                if vertical > 0. { vertical } else { horizontal }
                            };
                            vec![
                                (top, [x + tl_w, y, w - tl_w - tr_w, top]),
                                (bottom, [x + bl_w, y + h - bottom, w - bl_w - br_w, bottom]),
                                (left, [x, y + tl_h, left, h - tl_h - bl_h]),
                                (right, [x + w - right, y + tr_h, right, h - tr_h - br_h]),
                                (owner(top, left), [x, y, tl_w, tl_h]),
                                (owner(top, right), [x + w - tr_w, y, tr_w, tr_h]),
                                (
                                    owner(bottom, right),
                                    [x + w - br_w, y + h - br_h, br_w, br_h],
                                ),
                                (owner(bottom, left), [x, y + h - bl_h, bl_w, bl_h]),
                            ]
                        } else {
                            let middle = (h - top - bottom).max(0.);
                            vec![
                                (top, [x, y, w, top]),
                                (bottom, [x, y + h - bottom, w, bottom]),
                                (left, [x, y + top, left, middle]),
                                (right, [x + w - right, y + top, right, middle]),
                            ]
                        };
                        for (width, strip) in pieces {
                            if width <= 0. || strip[2] <= 0. || strip[3] <= 0. {
                                continue;
                            }
                            if radius.iter().any(|&r| r > 0.) {
                                self.items.push(Item::Inset {
                                    rect: screen,
                                    radius,
                                    spread: width,
                                    offset: [0., 0.],
                                    color,
                                    clip: intersect(strip, clip),
                                    round_clip,
                                    mask: mask.clone(),
                                });
                            } else {
                                self.items.push(Item::Fill {
                                    rect: strip,
                                    radius: [0.; 4],
                                    top: color,
                                    bottom: color,
                                    clip,
                                    round_clip,
                                    mask: mask.clone(),
                                });
                            }
                        }
                    } else {
                        let spread = paint.edge_width.unwrap_or(1.) * inner.k;
                        self.items.push(Item::Inset {
                            rect: screen,
                            radius,
                            spread,
                            offset: [0., 0.],
                            color,
                            clip,
                            round_clip,
                            mask: mask.clone(),
                        });
                    }
                }
                if let Some(highlight) = paint.highlight {
                    let color = inner.color(Value::color(highlight).rgba());
                    self.items.push(Item::Inset {
                        rect: screen,
                        radius,
                        spread: 0.,
                        offset: [0., inner.k],
                        color,
                        clip,
                        round_clip,
                        mask: mask.clone(),
                    });
                }
            }
        }
        let color = shown.color.unwrap_or([244., 244., 245., 255.]);
        match node {
            Node::Text {
                text,
                font,
                lines,
                align,
                spans,
                ..
            } if visible && !text.is_empty() => {
                let style = TextStyle {
                    align: *align,
                    ..TextStyle::of(font, inner.color(color))
                };
                let rich = spans.as_ref().is_some_and(|spans| !spans.is_empty());
                if !rich && !shown.underline && lines.is_none() {
                    // Fast path: plain, non-underlined, unclamped text skips `node_spans` and `plain_text`.
                    self.items.push(Item::Text {
                        rect: screen,
                        text: text.clone(),
                        spans: None,
                        style,
                        wrap: true,
                        zoom: inner.k,
                        clip,
                        round_clip,
                    });
                } else {
                    // Draws the text cut to its line limit at the width the layout measured.
                    let mut runs = node_spans(text, spans.as_deref()).into_owned();
                    if shown.underline {
                        runs.iter_mut().for_each(|run| run.underline = true);
                    }
                    // Rich or underlined text cuts and places its marks in one cached step, so unchanged text shapes
                    // nothing. The underline is in the spans, so the cached marks are keyed by it.
                    let (runs, marks) = if rich || shown.underline {
                        // Plain text is cut in the style layout measured it in.
                        let cut_style = if rich {
                            style.clone()
                        } else {
                            TextStyle {
                                align: *align,
                                ..TextStyle::resolve(font)
                            }
                        };
                        self.text
                            .cut_marks(&runs, layout_rect[2], *lines, &cut_style)
                    } else {
                        let runs = match lines {
                            Some(lines) => self.text.limit_lines(
                                &runs,
                                layout_rect[2],
                                *lines,
                                &TextStyle::resolve(font),
                            ),
                            None => runs,
                        };
                        (runs, Vec::new())
                    };
                    let zoom = inner.k;
                    for mark in marks {
                        match mark {
                            Mark::Image { span, rect } => {
                                let Some(image) = runs[span]
                                    .image
                                    .as_ref()
                                    .and_then(|image| self.images.get(&image.source))
                                    .filter(|_| !hidden)
                                else {
                                    continue;
                                };
                                self.items.push(Item::Image {
                                    rect: scaled(screen, rect, zoom),
                                    radius: [0.; 4],
                                    image: image.clone(),
                                    tint: [255; 4],
                                    clip,
                                    round_clip,
                                    mask: None,
                                });
                            }
                            Mark::Underline { span, rect } | Mark::Strike { span, rect } => {
                                let color = runs[span]
                                    .color
                                    .map_or(color, |c| c.to_be_bytes().map(f32::from));
                                let color = inner.color(color);
                                self.items.push(Item::Fill {
                                    rect: scaled(screen, rect, zoom),
                                    radius: [0.; 4],
                                    top: color,
                                    bottom: color,
                                    clip,
                                    round_clip,
                                    mask: None,
                                });
                            }
                            Mark::Background { span, rect } => {
                                let Some(background) = runs[span].background else {
                                    continue;
                                };
                                let color = inner.color(background.to_be_bytes().map(f32::from));
                                self.items.push(Item::Fill {
                                    rect: scaled(screen, rect, zoom),
                                    radius: [runs[span].background_radius * zoom; 4],
                                    top: color,
                                    bottom: color,
                                    clip,
                                    round_clip,
                                    mask: None,
                                });
                            }
                        }
                    }
                    self.items.push(Item::Text {
                        rect: screen,
                        text: plain_text(&runs),
                        spans: rich.then_some(runs),
                        style,
                        wrap: true,
                        zoom,
                        clip,
                        round_clip,
                    });
                }
            }
            Node::Icon { icon, stroke, .. } if visible => {
                self.items.push(Item::Icon {
                    rect: screen,
                    icon: *icon,
                    stroke: *stroke,
                    draw: shown.draw,
                    color: inner.color(color),
                    zoom: inner.k,
                    clip,
                    round_clip,
                });
            }
            Node::Image { source, .. } if visible => {
                if let Some(image) = self.images.get(source).filter(|_| !hidden) {
                    let tint = shown.color.unwrap_or([255., 255., 255., 255.]);
                    self.items.push(Item::Image {
                        rect: screen,
                        radius,
                        image: image.clone(),
                        tint: inner.color(tint),
                        clip,
                        round_clip,
                        mask: mask.clone(),
                    });
                }
            }
            Node::Input {
                text,
                font,
                placeholder,
                placeholder_color,
                multiline,
                purpose,
                ..
            } if visible || self.focused == Some(id) => {
                if visible {
                    self.hits.push(Hit {
                        rect: on_screen,
                        id,
                        layer: self.layer,
                    });
                }
                let content = inner.rect(input_content_of(tree, id));
                let style = TextStyle::of(font, inner.color(color));
                // A line sits in the middle of a single-line field, and at the top of a multi-line one.
                let line = [
                    content[0],
                    if *multiline {
                        content[1]
                    } else {
                        content[1] + (content[3] - style.line_height * inner.k) / 2.
                    },
                    content[2],
                    style.line_height * inner.k,
                ];
                // The visible text's height, in logical pixels: a multi-line field scrolls when its text is taller.
                let view = if *multiline {
                    content[3] / inner.k
                } else {
                    line[3] / inner.k
                };
                let width = content[2] / inner.k;
                if self.focused == Some(id) {
                    let requested = if *multiline {
                        self.scroll_offset(id, f32::MAX, self.now)
                    } else {
                        0.
                    };
                    self.editor.scroll_to(requested);
                    let frame = self.editor.frame(
                        self.text,
                        &style,
                        [width, view],
                        scale * inner.k,
                        max_side,
                        *self.editor_key,
                    );
                    *self.editor_key = Some(frame.bitmap_key);
                    if *multiline {
                        if frame.scroll_y != requested {
                            self.motion.release(id, Channel::Scroll);
                        }
                        self.scroll.insert(id, frame.scroll_y);
                        self.scrolls.push(Scroll {
                            id,
                            layer: self.layer,
                            rect: intersect(content, clip),
                            k: inner.k,
                            top: frame.scroll_y,
                            max: (frame.content_height - view).max(0.),
                            view,
                            bar: None,
                            list_generation: None,
                        });
                    }
                    if text.is_empty() && !self.editor.composing() {
                        self.placeholder(
                            placeholder,
                            *placeholder_color,
                            font,
                            &inner,
                            line,
                            (clip, round_clip),
                        );
                    }
                    let caret = [
                        line[0] + frame.caret_x * inner.k,
                        line[1] + frame.caret_y * inner.k,
                        inner.k,
                        line[3],
                    ];
                    if *purpose != InputPurpose::Password {
                        *self.ime_area = Some(caret);
                    }
                    let caret = (frame.caret_visible && !self.hide_caret).then_some(caret);
                    self.items.push(Item::Editor {
                        rect: if *multiline { content } else { line },
                        frame: Box::new(frame),
                        caret,
                        selection: [70, 120, 220, 110],
                        caret_color: inner.color(color),
                        clip: intersect(content, clip),
                        round_clip,
                    });
                } else if text.is_empty() {
                    self.placeholder(
                        placeholder,
                        *placeholder_color,
                        font,
                        &inner,
                        line,
                        (clip, round_clip),
                    );
                } else if *multiline {
                    let display = if *purpose == InputPurpose::Password {
                        mask_text(text)
                    } else {
                        text.clone()
                    };
                    // Unfocused, the wrapped text is drawn whole and shifted up by the scroll; the clip shows the visible part.
                    let height = self
                        .text
                        .measure_plain(&display, Some(width + WRAP_SLACK), &style)[1]
                    .max(style.line_height);
                    let max = (height - view).max(0.);
                    let offset = self.scroll_offset(id, max, self.now);
                    self.scrolls.push(Scroll {
                        id,
                        layer: self.layer,
                        rect: intersect(content, clip),
                        k: inner.k,
                        top: offset,
                        max,
                        view,
                        bar: None,
                        list_generation: None,
                    });
                    self.items.push(Item::Text {
                        rect: [
                            content[0],
                            content[1] - offset * inner.k,
                            content[2],
                            height * inner.k,
                        ],
                        text: display,
                        spans: None,
                        style,
                        wrap: true,
                        zoom: inner.k,
                        clip: intersect(content, clip),
                        round_clip,
                    });
                } else {
                    let display = if *purpose == InputPurpose::Password {
                        mask_text(text)
                    } else {
                        text.clone()
                    };
                    self.items.push(Item::Text {
                        rect: line,
                        text: display,
                        spans: None,
                        style,
                        wrap: false,
                        zoom: inner.k,
                        clip: intersect(content, clip),
                        round_clip,
                    });
                }
            }
            _ => {}
        }
        if visible {
            if matches!(node, Node::Box { press, contextmenu, drag, .. } if *press != 0 || *contextmenu != 0 || *drag != 0)
            {
                self.hits.push(Hit {
                    rect: on_screen,
                    id,
                    layer: self.layer,
                });
            }
            if is_group && !disabled {
                self.groups.push((id, on_screen, self.layer));
            }
            if let Node::Box { hover, dismiss, .. } = node {
                if *hover != 0 {
                    self.hovers.push((id, on_screen, self.layer));
                }
                if *dismiss != 0 {
                    self.dismissers.push((id, on_screen, self.layer));
                }
            }
            let has_cursor = paint.cursor.is_some()
                || [&paint.hover, &paint.active, &paint.focus]
                    .into_iter()
                    .flatten()
                    .any(|v| v.cursor.is_some());
            if has_cursor {
                self.cursors.push(CursorHit {
                    rect: on_screen,
                    id,
                    layer: self.layer,
                    after_hits: self.hits.len(),
                });
            }
        }
        let mut child = inner;
        if is_group {
            let (gh, ga) = self.group_state(id);
            child.group_hover = gh;
            child.group_active = ga;
        }
        if node.style().is_some_and(|s| s.clip || s.scroll) {
            child.clip = on_screen;
            // shortcut: a rounded clip inside another rounded clip replaces it, so the outer corners are lost.
            if radius.iter().any(|&r| r > 0.) {
                child.round_clip = Some((screen, radius));
            }
        }
        let virtual_list = self.virtual_lists.get(&id).map(|&(generation, ref list)| {
            if list.auto.is_some() {
                let (offset, extent) = list.frame_viewport(layout_rect[3]);
                self.scrolls.push(Scroll {
                    id,
                    layer: self.layer,
                    rect: on_screen,
                    k: inner.k,
                    top: offset as f32,
                    max: list.max_offset(extent) as f32,
                    view: extent as f32,
                    bar: None,
                    list_generation: Some(generation),
                });
            }
            list.frame_viewport(layout_rect[3]).1
        });
        // A scrollbar and its opacity this frame, drawn over the content.
        let mut bar: Option<(&Scrollbar, Rect, Rect, f32)> = None;
        if virtual_list.is_none() && node.style().is_some_and(|s| s.scroll) {
            // The offset is clamped to this frame's content, so it stays valid when the content shrinks.
            let content = layout.content_height(id);
            let max = (content - layout_rect[3]).max(0.);
            let end = node
                .style()
                .is_some_and(|s| s.scroll_anchor == ScrollAnchor::End);
            let offset = self.frame_offset(id, max, self.now, end);
            child.offset[1] -= offset * inner.k;
            let rects = paint
                .scrollbar
                .as_ref()
                .filter(|_| max > 0.)
                .map(|scrollbar| {
                    let rects = scrollbar_rects(
                        scrollbar,
                        screen,
                        inner.k,
                        layout_rect[3],
                        content,
                        offset,
                        max,
                    );
                    // Auto-hide fades the bar in while the pointer is over the node, or while its thumb is dragged.
                    let alpha = if scrollbar.auto_hide {
                        let over = self
                            .pointer
                            .is_some_and(|p| contains(on_screen, p));
                        let dragged = self.drag.is_some_and(|(drag, _)| drag == id);
                        let shown = Value::Number(f32::from(u8::from(over || dragged)));
                        self.motion
                            .show(
                                id,
                                Channel::Thumb,
                                shown,
                                Some(&ease_out(Property::Opacity)),
                                self.now,
                            )
                            .number()
                    } else {
                        1.
                    };
                    bar = Some((scrollbar, rects.0, rects.1, alpha));
                    rects
                });
            self.scrolls.push(Scroll {
                id,
                layer: self.layer,
                rect: on_screen,
                k: inner.k,
                top: offset,
                max,
                view: layout_rect[3],
                bar: rects,
                list_generation: None,
            });
        }
        if let Some(extent) = virtual_list {
            let view = [
                screen[0],
                screen[1],
                screen[2],
                screen[3].min(extent as f32 * inner.k),
            ];
            child.clip = intersect(child.clip, view);
            child.in_row = true;
            if child.clip[2] > 0. && child.clip[3] > 0. {
                for &row in layout.visible_rows(id) {
                    self.node(row, child, scale, max_side);
                }
            }
        } else if !matches!(node, Node::List { .. }) {
            for &c in tree.children(id) {
                self.node(c, child, scale, max_side);
            }
        }
        if visible
            && let Some((scrollbar, track, thumb, alpha)) =
                bar.filter(|(_, _, _, alpha)| *alpha > 0.)
        {
            // The bar is clipped like the node's content, and shaded by its auto-hide opacity.
            let shade = |rgba: u32| {
                let mut color = inner.color(Value::color(rgba).rgba());
                color[3] = (f32::from(color[3]) * alpha).round() as u8;
                color
            };
            let radius = [scrollbar.radius * inner.k; 4];
            if let Some(track_color) = scrollbar.track {
                let color = shade(track_color);
                self.items.push(Item::Fill {
                    rect: track,
                    radius,
                    top: color,
                    bottom: color,
                    clip: child.clip,
                    round_clip: child.round_clip,
                    mask: None,
                });
            }
            let color = shade(scrollbar.thumb);
            self.items.push(Item::Fill {
                rect: thumb,
                radius,
                top: color,
                bottom: color,
                clip: child.clip,
                round_clip: child.round_clip,
                mask: None,
            });
        }
        if layer {
            self.items.push(Item::PopLayer);
        }
    }

    fn placeholder(
        &mut self,
        text: &str,
        color: Option<u32>,
        font: &crate::tree::Font,
        ctx: &Context,
        line: Rect,
        (clip, round_clip): (Rect, Option<(Rect, Corners)>),
    ) {
        if text.is_empty() {
            return;
        }
        let color = ctx.color(Value::color(color.unwrap_or(0x757575ff)).rgba());
        self.items.push(Item::Text {
            rect: line,
            text: text.into(),
            spans: None,
            style: TextStyle::of(font, color),
            wrap: false,
            zoom: ctx.k,
            clip,
            round_clip,
        });
    }
}

#[cfg(all(test, feature = "stepper"))]
mod tests {
    use super::*;
    use crate::editor::{Direction, EditorEvent, EditorFrame};
    use crate::text::{NORMAL_LINE_HEIGHT, WRAP_SLACK};
    use crate::tree::TextAlign;

    const INTER: &[u8] = include_bytes!("../assets/fonts/Inter-Regular.ttf");

    fn settle(host: &mut Host) {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut quiet = 0;
        while quiet < 3 {
            assert!(Instant::now() < deadline, "the app did not settle");
            host.poll();
            let frame = host.frame(640., 560., 1., 4096);
            quiet = if frame.animating || host.busy() {
                0
            } else {
                quiet + 1
            };
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn texts(host: &Host) -> Vec<String> {
        host.tree
            .nodes
            .iter()
            .filter_map(|(_, n)| match n {
                Node::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }
    // Clicks the pressable box that contains the text `label`.
    fn click(host: &mut Host, label: &str) {
        let text = host
            .tree
            .nodes
            .iter()
            .find(|(_, n)| matches!(n, Node::Text { text, .. } if text == label))
            .map(|(id, _)| id);
        let id = text.unwrap_or_else(|| panic!("no text {label:?}"));
        let [x, y, w, h] = host.tree.rect(id);
        host.pointer_move(Some([x + w / 2., y + h / 2.]));
        host.pointer_down(PointSelection::Place);
        host.pointer_up();
        settle(host);
    }

    #[test]
    fn the_stepper_form_works_end_to_end_with_js_off_the_ui_thread() {
        let mut host = Host::new(
            crate::apps::STEPPER.into(),
            "{}".into(),
            TextEngine::default(),
        );
        settle(&mut host);
        assert!(texts(&host).contains(&"Welcome aboard".into()));
        click(&mut host, "Continue");
        let shown = texts(&host);
        assert!(shown.contains(&"Choose a plan".into()));
        assert!(
            !shown.contains(&"Welcome aboard".into()),
            "the leaving pane is removed after its timer"
        );
        click(&mut host, "Starter");
        click(&mut host, "Continue");
        // Profile: Continue stays disabled while the name is empty.
        click(&mut host, "Continue");
        assert!(texts(&host).contains(&"What should we call you?".into()));
        host.focus_first_input();
        host.edit(&[EditorEvent::Insert("Ada".into())]);
        settle(&mut host);
        click(&mut host, "Continue");
        let shown = texts(&host);
        assert!(shown.contains(&"Looking good, Ada".into()), "{shown:?}");
        assert!(
            shown
                .iter()
                .any(|t| t.starts_with("You picked the Starter plan"))
        );
        click(&mut host, "Complete");
        assert!(texts(&host).contains(&"Welcome in, Ada.".into()));
        // Back through a header segment: the review again.
        click(&mut host, "Review");
        assert!(texts(&host).contains(&"Looking good, Ada".into()));
    }

    #[test]
    fn host_push_reaches_on_host_in_order() {
        let mut host = Host::new(
            crate::apps::HOSTED.into(),
            r#"{"listen":true}"#.into(),
            TextEngine::default(),
        );
        settle(&mut host);
        assert_eq!(texts(&host), vec!["waiting".to_string()]);
        host.push(r#"{"text":"pushed"}"#.into());
        settle(&mut host);
        assert!(
            texts(&host).contains(&"pushed".into()),
            "{:?}",
            texts(&host)
        );
    }

    #[test]
    fn host_push_without_on_host_changes_nothing() {
        let mut host = Host::new(
            crate::apps::HOSTED.into(),
            r#"{"listen":false}"#.into(),
            TextEngine::default(),
        );
        settle(&mut host);
        host.push(r#"{"text":"pushed"}"#.into());
        settle(&mut host);
        assert_eq!(texts(&host), vec!["waiting".to_string()]);
    }

    // The text of the field, its selection and the id of the field.
    fn field(host: &Host) -> (u16, String, [u32; 2]) {
        host.tree
            .nodes
            .iter()
            .find_map(|(id, n)| match n {
                Node::Input {
                    text, selection, ..
                } => Some((id, text.clone(), *selection)),
                _ => None,
            })
            .expect("a field")
    }

    #[test]
    fn reload_keeps_signals_and_native_state_and_reports_failures() {
        let v1 = crate::apps::RELOAD;
        let mut host = Host::new(v1.into(), "{}".into(), TextEngine::default());
        host.set_image("a".into(), vec![255; 4 * 2 * 2], [2, 2]);
        settle(&mut host);
        click(&mut host, "five");
        assert!(texts(&host).contains(&"v1:5".into()));
        host.focus_first_input();
        host.edit(&[EditorEvent::Insert("draft".into())]);
        settle(&mut host);
        let row = host
            .tree
            .nodes
            .iter()
            .find(|(_, n)| matches!(n, Node::Text { text, .. } if text == "row 0"))
            .map(|(id, _)| id)
            .expect("row 0");
        let [x, y, w, h] = host.tree.rect(row);
        host.pointer_move(Some([x + w / 2., y + h / 2.]));
        host.wheel(40.);
        settle(&mut host);
        let (input, draft, selection) = field(&host);
        assert_eq!(host.focused, Some(input));
        assert_eq!(draft, "draft");

        assert!(host.reload(v1.replace("v1:", "v2:")));
        settle(&mut host);
        assert_eq!(host.take_reload_result(), Some(Ok(())));
        let shown = texts(&host);
        assert!(shown.contains(&"v2:5".into()), "{shown:?}");
        let (input, text, kept) = field(&host);
        assert_eq!((text.as_str(), kept), ("draft", selection));
        assert_eq!(host.focused, Some(input), "the field keeps focus");
        assert!(host.scroll.values().any(|&offset| offset == 40.));
        assert!(
            !images(&mut host).is_empty(),
            "the image set before the reload still draws"
        );

        // A script that does not load: reported, and the app keeps running.
        assert!(host.reload(format!("{v1}\nfunction (")));
        settle(&mut host);
        assert!(matches!(host.take_reload_result(), Some(Err(_))));
        assert!(texts(&host).contains(&"v2:5".into()));
        click(&mut host, "plus");
        assert!(texts(&host).contains(&"v2:6".into()), "{:?}", texts(&host));
    }

    // The image items of the next frame.
    fn images(host: &mut Host) -> Vec<Item> {
        host.frame(640., 560., 1., 4096)
            .items
            .into_iter()
            .filter(|item| matches!(item, Item::Image { .. }))
            .collect()
    }

    #[test]
    fn images_follow_their_source_and_replacement() {
        let mut host = Host::new(
            crate::apps::IMAGES.into(),
            "{}".into(),
            TextEngine::default(),
        );
        host.set_image("a".into(), vec![255; 4 * 2 * 2], [2, 2]);
        settle(&mut host);
        // "missing" has no pixels, so only "a" draws.
        let items = images(&mut host);
        let [
            Item::Image {
                rect,
                radius,
                image,
                tint,
                ..
            },
        ] = &items[..]
        else {
            panic!("expected one image item")
        };
        assert_eq!((rect[2], rect[3]), (40., 40.));
        assert_eq!(*radius, [20.; 4]);
        assert_eq!(*tint, [255, 0, 0, 255]);
        let generation = image.generation;
        host.set_image("a".into(), vec![0; 4 * 2 * 2], [2, 2]);
        let items = images(&mut host);
        let [Item::Image { image, .. }] = &items[..] else {
            panic!("expected one image item")
        };
        assert!(image.generation > generation);
        host.remove_image("a");
        assert!(images(&mut host).is_empty());
    }

    #[test]
    fn text_lines_end_in_an_ellipsis_and_measure_what_they_draw() {
        let mut host = Host::new(
            crate::apps::LINES.into(),
            "{}".into(),
            TextEngine::default(),
        );
        settle(&mut host);
        let column = host.tree.children(host.tree.root.unwrap())[0];
        let &[one, two] = host.tree.children(column) else {
            panic!("two texts")
        };
        let line = 16. * NORMAL_LINE_HEIGHT;
        assert!((host.tree.rect(one)[3] - line).abs() < 0.01);
        assert!((host.tree.rect(two)[3] - 2. * line).abs() < 0.01);
        let style = TextStyle::of(
            &crate::tree::Font {
                size: Some(16.),
                ..Default::default()
            },
            [0; 4],
        );
        let frame = host.frame(640., 560., 1., 4096);
        for (id, lines) in [(one, 1), (two, 2)] {
            let [x, y, width, _] = host.tree.rect(id);
            let shown = frame
                .items
                .iter()
                .find_map(|item| match item {
                    Item::Text { rect, text, .. } if rect[..2] == [x, y] => Some(text.clone()),
                    _ => None,
                })
                .expect("drawn text");
            assert!(shown.ends_with('…'), "{shown:?}");
            let [_, height] = host.text.measure_styled(
                &node_spans(&shown, None),
                Some(width + WRAP_SLACK),
                &style,
            );
            assert_eq!((height / line).round() as usize, lines, "{shown:?}");
        }
    }

    // The text and node id of each child of the keyed list, in tree order.
    fn rows(host: &Host) -> Vec<(String, u16)> {
        let list = host.tree.children(host.tree.root.unwrap())[0];
        host.tree
            .children(list)
            .iter()
            .map(|&id| (host.tree.text(id).unwrap().to_owned(), id))
            .collect()
    }

    #[test]
    fn for_keeps_children_by_key_and_moves_only_what_changed() {
        let mut host = Host::new(
            crate::apps::KEYED.into(),
            r#"{"items":[{"key":"a","name":"A"},{"key":"b","name":"B"},{"key":"c","name":"C"}]}"#
                .into(),
            TextEngine::default(),
        );
        settle(&mut host);
        let start = rows(&host);
        let names =
            |rows: &[(String, u16)]| rows.iter().map(|(t, _)| t.clone()).collect::<Vec<_>>();
        let id = |rows: &[(String, u16)], name: &str| {
            rows.iter()
                .find(|(t, _)| t == name)
                .map(|(_, id)| *id)
                .unwrap()
        };
        assert_eq!(names(&start), ["A", "B", "C"]);

        host.push(
            r#"{"items":[{"key":"c","name":"C"},{"key":"a","name":"A"},{"key":"b","name":"B"}]}"#
                .into(),
        );
        settle(&mut host);
        let moved = rows(&host);
        assert_eq!(names(&moved), ["C", "A", "B"]);
        for name in ["A", "B", "C"] {
            assert_eq!(id(&moved, name), id(&start, name), "{name} was remounted");
        }
        // The layout follows the new order: each child sits below the one before it.
        let tops: Vec<f32> = moved.iter().map(|(_, id)| host.tree.rect(*id)[1]).collect();
        assert!(tops.windows(2).all(|w| w[0] < w[1]), "{tops:?}");

        host.push(r#"{"items":[{"key":"c","name":"C"},{"key":"b","name":"B"}]}"#.into());
        settle(&mut host);
        assert_eq!(names(&rows(&host)), ["C", "B"]);
        assert!(host.tree.nodes.get(&id(&start, "A")).is_none());

        host.push(
            r#"{"items":[{"key":"c","name":"C"},{"key":"b","name":"Bee"},{"key":"d","name":"D"}]}"#
                .into(),
        );
        settle(&mut host);
        let grown = rows(&host);
        assert_eq!(names(&grown), ["C", "Bee", "D"]);
        assert_eq!(id(&grown, "Bee"), id(&start, "B"), "b was remounted");
        assert_eq!(id(&grown, "C"), id(&start, "C"), "c was remounted");
    }

    // Where a text is drawn on screen, and the clip it is drawn with.
    fn drawn(frame: &Frame, label: &str) -> (Rect, Rect) {
        frame
            .items
            .iter()
            .find_map(|item| match item {
                Item::Text {
                    rect, text, clip, ..
                } if text == label => Some((*rect, *clip)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no text {label:?} drawn"))
    }

    #[test]
    fn scroll_moves_clips_and_clamps_its_content() {
        let mut host = Host::new(
            crate::apps::SCROLL.into(),
            "{}".into(),
            TextEngine::default(),
        );
        settle(&mut host);
        let container = host
            .tree
            .nodes
            .iter()
            .find(|(_, n)| n.style().is_some_and(|s| s.scroll))
            .map(|(id, _)| id)
            .expect("a scroll node");
        let [x, top, width, height] = host.tree.rect(container);
        assert_eq!(height, 100.);
        let near = |a: Rect, b: Rect| a.iter().zip(b).all(|(a, b)| (a - b).abs() < 0.01);
        let inside = [x + width / 2., top + 5.];

        let (rect, _) = drawn(&host.frame(640., 560., 1., 4096), "row 0");
        assert!((rect[1] - top).abs() < 0.01, "{rect:?}");

        host.pointer_move(Some(inside));
        assert!(host.wheel(50.));
        let frame = host.frame(640., 560., 1., 4096);
        // At offset 50 inside a 100 px box (30 px rows with 19 px top-aligned text):
        // row 1's box overlaps [top, top + 10] (so clicks hit it below), while its text ends at top - 1.
        // Texts of rows 2..=4 overlap the clip; off-screen texts (0, 1, 5..10) are culled.
        for n in 2..=4 {
            let (rect, clip) = drawn(&frame, &format!("row {n}"));
            assert!(
                (rect[1] - (top + 30. * n as f32 - 50.)).abs() < 0.01,
                "row {n}: {rect:?}"
            );
            assert!(
                near(clip, [x, top, width, height]),
                "row {n} is clipped to the box: {clip:?}"
            );
        }
        for n in [0, 1, 5, 6, 7, 8, 9] {
            let label = format!("row {n}");
            assert!(
                !frame
                    .items
                    .iter()
                    .any(|item| matches!(item, Item::Text { text, .. } if text == &label)),
                "off-screen {label:?} should be culled from frame items"
            );
        }

        // The row under the box's top edge is row 1 now, so that is what a click picks.
        host.pointer_down(PointSelection::Place);
        host.pointer_up();
        settle(&mut host);
        assert!(
            texts(&host).contains(&"picked 1".into()),
            "{:?}",
            texts(&host)
        );

        assert!(host.wheel(10_000.));
        let frame = host.frame(640., 560., 1., 4096);
        assert_eq!(host.layout.content_height(container), 300.);
        assert_eq!(host.scroll[&container], 200.);
        let (rect, _) = drawn(&frame, "row 9");
        assert!(
            (rect[1] + 30. - (top + height)).abs() < 0.01,
            "row 9 ends at the box: {rect:?}"
        );

        assert!(host.wheel(-10_000.));
        let frame = host.frame(640., 560., 1., 4096);
        assert_eq!(host.scroll[&container], 0.);
        let (rect, _) = drawn(&frame, "row 0");
        assert!((rect[1] - top).abs() < 0.01, "{rect:?}");

        // Outside every scroll node the wheel changes nothing.
        host.pointer_move(Some([x + width / 2., top - 10.]));
        assert!(!host.wheel(50.));
        assert_eq!(host.scroll[&container], 0.);
    }

    #[test]
    fn edge_sides_draw_one_band_per_side_and_a_rounded_one_as_ring_pieces_that_keep_the_corners() {
        let mut host = Host::new(
            crate::apps::BORDERS.into(),
            "{}".into(),
            TextEngine::default(),
        );
        settle(&mut host);
        let flat = find(&host, |n| {
            n.paint()
                .is_some_and(|p| p.edge_sides == Some([0., 0., 1., 0.]))
        });
        let [x, y, width, height] = host.tree.rect(flat);
        assert_eq!((width, height), (100., 40.));
        let red = [255, 0, 0, 255];
        let frame = frame_of(&mut host);
        // A square box with a 1 px bottom side: exactly one 100 by 1 band at the bottom, and nothing else in red.
        let bands: Vec<Rect> = frame
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Fill { rect, top, .. } if *top == red => Some(*rect),
                _ => None,
            })
            .collect();
        assert_eq!(bands, [[x, y + height - 1., width, 1.]]);
        // The rounded box's 2 px bottom is the ring clipped to the straight strip between the corners, and to
        // each bottom corner square, so the line keeps its arcs: three pieces, nothing on the top corners.
        let round = find(&host, |n| {
            n.paint()
                .is_some_and(|p| p.edge_sides == Some([0., 0., 2., 0.]))
        });
        let [rx, ry, _, rh] = host.tree.rect(round);
        let rings: Vec<Rect> = frame
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Inset {
                    color,
                    clip,
                    rect,
                    spread,
                    ..
                } if *color == red => {
                    assert_eq!((rect[2], *spread), (width, 2.));
                    Some(*clip)
                }
                _ => None,
            })
            .collect();
        let bottom = ry + rh;
        assert_eq!(
            rings,
            [
                [rx + 8., bottom - 2., width - 16., 2.],
                [rx + width - 8., bottom - 8., 8., 8.],
                [rx, bottom - 8., 8., 8.],
            ]
        );
    }

    // The thumbs drawn: 8 px wide fills in the thumb color of the scrollbar app, with their opacity.
    const THUMB: [u8; 3] = [0x5f, 0x60, 0x6a];
    fn thumbs(frame: &Frame) -> Vec<(Rect, Rgba)> {
        frame
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Fill { rect, top, .. } if rect[2] == 8. && top[..3] == THUMB => {
                    Some((*rect, *top))
                }
                _ => None,
            })
            .collect()
    }
    // The thumbs drawn inside a scroll node's rectangle.
    fn thumbs_in(frame: &Frame, [x, _, width, _]: Rect) -> Vec<(Rect, Rgba)> {
        thumbs(frame)
            .into_iter()
            .filter(|(rect, _)| rect[0] >= x && rect[0] + rect[2] <= x + width)
            .collect()
    }
    fn bar_node(host: &Host, auto_hide: bool, min_thumb: f32) -> u16 {
        find(host, |n| {
            n.paint()
                .and_then(|p| p.scrollbar.as_ref())
                .is_some_and(|b| b.auto_hide == auto_hide && b.min_thumb == min_thumb)
        })
    }

    // Containers take the props of a Box: a Column paints its background, a For shows a thumb, and a tooltip's text
    // takes its color and wraps at its maximum width.
    #[test]
    fn containers_take_the_props_of_a_box() {
        let mut host = Host::new(
            crate::apps::CONTAINERS.into(),
            "{}".into(),
            TextEngine::default(),
        );
        settle(&mut host);
        let frame = frame_of(&mut host);
        let background = frame
            .items
            .iter()
            .find_map(|item| match item {
                Item::Fill { rect, top, .. } if *top == [0x30, 0x60, 0xc0, 0xff] => Some(*rect),
                _ => None,
            })
            .expect("the column's background");
        assert_eq!((background[2], background[3]), (60., 60.));
        let thumb = thumbs(&frame);
        assert_eq!(thumb.len(), 1, "{thumb:?}");
        assert!(thumb[0].0[0] > background[0] + background[2]);

        let target = find(&host, |n| {
            n.style()
                .is_some_and(|s| s.width == Some(20.) && s.height == Some(20.))
        });
        host.pointer_move(Some(centre(host.screen_rect(target).unwrap())));
        settle(&mut host);
        let (rect, color) = frame_of(&mut host)
            .items
            .iter()
            .find_map(|item| match item {
                Item::Text {
                    rect, text, style, ..
                } if text == "A tooltip that wraps onto several lines" => {
                    Some((*rect, style.color))
                }
                _ => None,
            })
            .expect("the tooltip's text");
        assert_eq!(color, [255, 128, 0, 255]);
        assert!(rect[2] <= 60. + 0.01, "{rect:?}");
        assert!(rect[3] > 30., "the text wraps onto several lines: {rect:?}");
    }

    #[test]
    fn a_scrollbar_thumb_is_its_share_of_the_track_and_follows_the_offset() {
        let mut host = Host::new(
            crate::apps::SCROLLBAR.into(),
            "{}".into(),
            TextEngine::default(),
        );
        settle(&mut host);
        let node = bar_node(&host, false, 40.);
        let [x, top, width, height] = host.tree.rect(node);
        assert_eq!((height, host.layout.content_height(node)), (100., 300.));
        let near = |a: Rect, b: Rect| a.iter().zip(b).all(|(a, b)| (a - b).abs() < 0.01);
        let (rect, _) = thumbs_in(&frame_of(&mut host), [x, top, width, height])[0];
        // A 100 px view of 300 px of content is 33 px of thumb, so the 40 px minimum applies.
        assert!(near(rect, [x + width - 8., top, 8., 40.]), "{rect:?}");

        host.pointer_move(Some([x + width / 2., top + 50.]));
        assert!(host.wheel(10_000.));
        let (rect, _) = thumbs_in(&frame_of(&mut host), [x, top, width, height])[0];
        assert!(near(rect, [x + width - 8., top + 60., 8., 40.]), "{rect:?}");
        assert!(
            (rect[1] + rect[3] - (top + height)).abs() < 0.01,
            "{rect:?}"
        );
    }

    #[test]
    fn an_auto_hiding_thumb_shows_only_while_the_pointer_is_over_its_node() {
        let mut host = Host::new(
            crate::apps::SCROLLBAR.into(),
            "{}".into(),
            TextEngine::default(),
        );
        settle(&mut host);
        let node = bar_node(&host, true, 40.);
        let rect = host.tree.rect(node);
        assert!(
            thumbs_in(&frame_of(&mut host), rect).is_empty(),
            "the pointer is outside"
        );
        // The two other bars keep their thumbs.
        assert_eq!(thumbs(&frame_of(&mut host)).len(), 2);

        host.pointer_move(Some([rect[0] + rect[2] / 2., rect[1] + 5.]));
        settle(&mut host);
        let shown = thumbs_in(&frame_of(&mut host), rect);
        assert_eq!(shown.len(), 1, "{shown:?}");
        assert_eq!(shown[0].1[3], 255, "fully shown once faded in");

        host.pointer_move(None);
        settle(&mut host);
        assert!(thumbs_in(&frame_of(&mut host), rect).is_empty());
    }

    #[test]
    fn dragging_the_thumb_scrolls_by_the_content_over_the_viewport() {
        let mut host = Host::new(
            crate::apps::SCROLLBAR.into(),
            "{}".into(),
            TextEngine::default(),
        );
        settle(&mut host);
        // The third bar has no minimum, so its thumb is 100 * 100 / 300 long and the ratio is exact.
        let node = bar_node(&host, false, 0.);
        let rect = host.tree.rect(node);
        let (thumb, _) = thumbs_in(&frame_of(&mut host), rect)[0];
        let grab = [rect[0] + rect[2] - 4., thumb[1] + thumb[3] / 2.];
        host.pointer_move(Some(grab));
        host.pointer_down(PointSelection::Place);
        host.pointer_move(Some([grab[0], grab[1] + 30.]));
        host.pointer_up();
        let offset = host.scroll[&node];
        assert!((offset - 30. * 300. / 100.).abs() <= 1., "{offset}");
        let (moved, _) = thumbs_in(&frame_of(&mut host), rect)[0];
        assert!(
            (moved[1] - thumb[1] - 30.).abs() < 1.,
            "{moved:?} after {thumb:?}"
        );
    }

    #[test]
    fn a_wheel_notch_glides_to_its_target_and_repeated_notches_add_up() {
        let mut host = Host::new(
            crate::apps::SCROLLBAR.into(),
            "{}".into(),
            TextEngine::default(),
        );
        settle(&mut host);
        let node = bar_node(&host, false, 40.);
        let rect = host.tree.rect(node);
        host.pointer_move(Some([rect[0] + rect[2] / 2., rect[1] + 50.]));
        settle(&mut host);

        assert!(host.wheel_smooth(40.));
        std::thread::sleep(Duration::from_millis(40));
        let frame = frame_of(&mut host);
        let mid = host.scroll[&node];
        assert!(frame.animating);
        assert!(mid > 0. && mid < 40., "{mid}");

        settle(&mut host);
        assert_eq!(host.scroll[&node], 40.);
        // Two more notches, sent at once, add to the target of the first: 40 + 40 + 40.
        host.wheel_smooth(40.);
        host.wheel_smooth(40.);
        settle(&mut host);
        assert_eq!(host.scroll[&node], 120.);
    }

    const REST: [u8; 4] = [0x10, 0x20, 0x30, 0xff];
    const HOT: [u8; 4] = [0xf0, 0xa0, 0x50, 0xff];
    const PRESSED: [u8; 4] = [0, 0xff, 0, 0xff];
    const ROW_HOVER: Rgba = [0x80, 0x80, 0x80, 0x80];
    const ROW_FILL: Rgba = [0x40, 0x80, 0xc0, 0xff];

    // The states app with an image for each of its two sources.
    fn states() -> Host {
        let mut host = Host::new(
            crate::apps::STATES.into(),
            "{}".into(),
            TextEngine::default(),
        );
        host.set_image("a".into(), vec![255; 4 * 2 * 2], [2, 2]);
        host.set_image("b".into(), [0, 0, 0, 255].repeat(4), [2, 2]);
        settle(&mut host);
        host
    }
    fn find(host: &Host, pick: impl Fn(&Node) -> bool) -> u16 {
        host.tree
            .nodes
            .iter()
            .find(|(_, n)| pick(n))
            .map(|(id, _)| id)
            .expect("node")
    }
    fn text_id(host: &Host, label: &str) -> u16 {
        find(
            host,
            |n| matches!(n, Node::Text { text, .. } if text == label),
        )
    }
    fn frame_of(host: &mut Host) -> Frame {
        host.frame(640., 560., 1., 4096)
    }
    fn color_of(frame: &Frame, label: &str) -> [u8; 4] {
        frame
            .items
            .iter()
            .find_map(|item| match item {
                Item::Text { text, style, .. } if text == label => Some(style.color),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no text {label:?} drawn"))
    }
    // The rectangles of the fills of one color.
    fn fills_of(frame: &Frame, color: Rgba) -> Vec<Rect> {
        frame
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Fill { rect, top, .. } if *top == color => Some(*rect),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn group_hover_reaches_the_children_of_the_hovered_row() {
        let mut host = states();
        let row = host.tree.parent(text_id(&host, "Label")).unwrap();
        let pill = find(
            &host,
            |n| matches!(n, Node::Box { style, .. } if style.height == Some(8.)),
        );
        let [x, y, w, h] = host.tree.rect(row);
        assert_eq!(color_of(&frame_of(&mut host), "Label"), REST);
        assert_eq!(host.tree.rect(pill)[3], 8.);

        // Left of the texts, inside the row: the row and its children take their hover look.
        host.pointer_move(Some([x + 2., y + h / 2.]));
        let frame = frame_of(&mut host);
        assert_eq!(color_of(&frame, "Label"), HOT);
        assert_eq!(fills_of(&frame, ROW_HOVER), vec![[x, y, w, h]]);
        assert_eq!(host.tree.rect(pill)[3], 20.);

        host.pointer_move(Some([600., 500.]));
        let frame = frame_of(&mut host);
        assert_eq!(color_of(&frame, "Label"), REST);
        assert!(fills_of(&frame, ROW_HOVER).is_empty());
        assert_eq!(host.tree.rect(pill)[3], 8.);
    }

    #[test]
    fn a_group_hover_underlines_the_texts_inside_until_it_leaves() {
        let mut host = states();
        let row = host.tree.parent(text_id(&host, "Title")).unwrap();
        let [x, y, _, h] = host.tree.rect(row);
        // Underlines are 1 px tall fills.
        let underlines = |frame: &Frame| {
            frame
                .items
                .iter()
                .filter(|item| matches!(item, Item::Fill { rect, .. } if rect[3] == 1.))
                .count()
        };
        assert_eq!(underlines(&frame_of(&mut host)), 0);

        // One mark per glyph of the title and the subtitle.
        host.pointer_move(Some([x + 2., y + h / 2.]));
        assert!(underlines(&frame_of(&mut host)) > 0);
        host.text.shapes = 0;
        assert!(underlines(&frame_of(&mut host)) > 0);
        assert_eq!(host.text.shapes, 0, "unchanged underlines shape nothing");

        host.pointer_move(Some([600., 500.]));
        assert_eq!(underlines(&frame_of(&mut host)), 0);
    }

    #[test]
    fn a_transition_on_a_group_child_animates_then_settles() {
        let mut host = states();
        let row = host.tree.parent(text_id(&host, "Fading")).unwrap();
        let [x, y, _, h] = host.tree.rect(row);
        host.pointer_move(Some([x + 2., y + h / 2.]));
        // The transition starts with the first frame that sees the hover.
        frame_of(&mut host);
        std::thread::sleep(Duration::from_millis(30));
        let [red, ..] = color_of(&frame_of(&mut host), "Fading");
        assert!(red > REST[0] && red < HOT[0], "{red}");
        settle(&mut host);
        assert_eq!(color_of(&frame_of(&mut host), "Fading"), HOT);
    }

    // The scroll column of the states app, and the pointer five pixels into it: row 0 is under it.
    fn scroll_column(host: &mut Host) -> (u16, Rect) {
        let scroll = host
            .tree
            .parent(host.tree.parent(text_id(host, "row 0")).unwrap())
            .unwrap();
        let [x, top, width, height] = host.tree.rect(scroll);
        host.pointer_move(Some([x + 4., top + 5.]));
        (scroll, [x, top, width, height])
    }

    #[test]
    fn hover_follows_a_scroll_once_the_scroll_is_idle() {
        let mut host = states();
        let (_, [_, top, width, _]) = scroll_column(&mut host);
        let row0 = host.tree.parent(text_id(&host, "row 0")).unwrap();
        let fills = fills_of(&frame_of(&mut host), ROW_FILL);
        assert_eq!(fills.len(), 1);
        assert!((fills[0][1] - top).abs() < 0.01, "{fills:?}");

        // Scrolling 45 px moves row 1 under the same point, but hover keeps row 0 until the scroll is idle.
        // Row 0 is scrolled completely above the clip (culled from drawn fills), and row 1 is not hovered yet.
        assert!(host.wheel(45.));
        let fills = fills_of(&frame_of(&mut host), ROW_FILL);
        assert!(fills.is_empty(), "row 0 is culled off-screen and row 1 is not hovered yet: {fills:?}");
        assert_eq!(host.hovered_groups, [row0]);

        std::thread::sleep(Duration::from_millis(120));
        let frame = frame_of(&mut host);
        let fills = fills_of(&frame, ROW_FILL);
        assert_eq!(fills.len(), 1);
        assert!((fills[0][1] - (top - 15.)).abs() < 0.01, "{fills:?}");
        assert!((fills[0][2] - width).abs() < 0.01, "{fills:?}");
    }

    #[test]
    fn hover_stays_put_during_a_glide_and_follows_once_it_is_idle() {
        let mut host = states();
        let (scroll, [_, top, _, _]) = scroll_column(&mut host);
        frame_of(&mut host);

        assert!(host.wheel_smooth(45.));
        std::thread::sleep(Duration::from_millis(40));
        let frame = frame_of(&mut host);
        let mid = host.scroll[&scroll];
        assert!(frame.animating && mid > 0. && mid < 45., "{mid}");
        // Row 0 moves up with the glide and stays the hovered row.
        let fills = fills_of(&frame, ROW_FILL);
        assert_eq!(fills.len(), 1);
        assert!((fills[0][1] - (top - mid)).abs() < 0.01, "{fills:?}");

        // Settling ends the glide, then the idle time passes: row 1 is under the pointer and hovered.
        settle(&mut host);
        assert_eq!(host.scroll[&scroll], 45.);
        let fills = fills_of(&frame_of(&mut host), ROW_FILL);
        assert_eq!(fills.len(), 1);
        assert!((fills[0][1] - (top - 15.)).abs() < 0.01, "{fills:?}");
    }

    #[test]
    fn a_pointer_move_during_a_scroll_updates_hover_at_once() {
        let mut host = states();
        let (_, [x, top, _, _]) = scroll_column(&mut host);
        let row0 = host.tree.parent(text_id(&host, "row 0")).unwrap();
        frame_of(&mut host);
        assert!(host.wheel(45.));
        let fills = fills_of(&frame_of(&mut host), ROW_FILL);
        assert!(fills.is_empty(), "row 0 is culled off-screen and row 1 is not hovered yet: {fills:?}");
        assert_eq!(host.hovered_groups, [row0]);

        // Row 1 is under the moved pointer: no idle time to wait for.
        host.pointer_move(Some([x + 4., top + 6.]));
        let fills = fills_of(&frame_of(&mut host), ROW_FILL);
        assert_eq!(fills.len(), 1);
        assert!((fills[0][1] - (top - 15.)).abs() < 0.01, "{fills:?}");
    }

    #[test]
    fn clip_culling_preserves_casting_shadows_and_dynamic_transitions() {
        const APP: &str = r#"
            function jsnirMount() {
                jsnir.batch(JSON.stringify({ ops: [
                    { op: 'create', id: 1, node: { kind: 'box', style: { width: 400, height: 200 } } },
                    { op: 'create', id: 2, parent: 1, node: { kind: 'box', style: { width: 200, height: 100, clip: true } } },
                    // Positioned just below the 100 px clip, but casts a 24 px blur shadow upward into the clip.
                    { op: 'create', id: 3, parent: 2, node: { kind: 'box', style: { position: 'absolute', left: 10, top: 105, width: 80, height: 20 }, paint: { background: 0xff0000ff, shadows: [{ y: 0, blur: 24, spread: 0, color: 0x000000ff }] } } },
                    // Initially static node (no transitions) that dynamically receives a transition + new color later.
                    { op: 'create', id: 4, parent: 1, node: { kind: 'box', style: { width: 80, height: 40 }, paint: { background: 0x000000ff } } },
                ] }));
            }
            function jsnirEvent(_, text) { jsnir.batch(text); }
        "#;
        let mut host = Host::new(APP.into(), "{}".into(), TextEngine::default());
        settle(&mut host);
        let frame = host.frame(400., 200., 1., 4096);
        assert!(
            frame.items.iter().any(|item| matches!(item, Item::Shadow { .. })),
            "an off-screen box whose shadow reaches into the clip must still emit its shadow"
        );
        // Dynamically attach a 400 ms background transition while switching color 0x000000ff -> 0xffffffff.
        host.push(r#"{"ops":[{"op":"setpaint","id":4,"paint":{"background":4294967295,"transitions":[{"property":"background","duration":400,"easing":"linear"}]}}]}"#.into());
        while host.busy() {
            host.poll();
            std::thread::yield_now();
        }
        let first = host.frame(400., 200., 1., 4096);
        assert!(first.animating, "dynamically attached transition must animate from old static paint");
        std::thread::sleep(Duration::from_millis(40));
        let mid = host.frame(400., 200., 1., 4096);
        let box4_fill = mid.items.iter().find_map(|item| match item {
            Item::Fill { rect, top, .. } if (rect[2] - 80.).abs() < 0.1 && (rect[3] - 40.).abs() < 0.1 => Some(*top),
            _ => None,
        }).expect("box 4 fill");
        assert!(box4_fill[0] > 0 && box4_fill[0] < 255, "mid-transition color should interpolate: {box4_fill:?}");
    }

    #[test]
    fn pressing_the_group_switches_the_image_source() {
        let mut host = states();
        let image = find(&host, |n| matches!(n, Node::Image { .. }));
        let row = host.tree.parent(image).unwrap();
        let source = |host: &Host| match &host.tree.nodes[&image] {
            Node::Image { source, .. } => source.clone(),
            _ => unreachable!(),
        };
        assert_eq!(source(&host), "a");
        let [x, y, _, h] = host.tree.rect(row);
        host.pointer_move(Some([x + 2., y + h / 2.]));
        host.pointer_down(PointSelection::Place);
        host.pointer_up();
        settle(&mut host);
        assert_eq!(source(&host), "b");
        let items = images(&mut host);
        let [Item::Image { image: drawn, .. }] = &items[..] else {
            panic!("expected one image item")
        };
        assert_eq!(drawn.rgba[..4], [0, 0, 0, 255]);
    }

    #[test]
    fn active_applies_to_group_children_while_the_button_is_down() {
        let mut host = states();
        let row = host.tree.parent(text_id(&host, "Label")).unwrap();
        let [x, y, _, h] = host.tree.rect(row);
        host.pointer_move(Some([x + 2., y + h / 2.]));
        host.pointer_down(PointSelection::Place);
        assert_eq!(color_of(&frame_of(&mut host), "Label"), PRESSED);
        host.pointer_up();
        settle(&mut host);
        assert_eq!(color_of(&frame_of(&mut host), "Label"), HOT);
    }

    // The fill of one color in the frame: its rectangle, its mask and its rounded clip.
    type Fill = (Rect, Option<(ImageData, Rect)>, Option<(Rect, Corners)>);
    fn fill_of(frame: &Frame, color: Rgba) -> Option<Fill> {
        frame.items.iter().find_map(|item| match item {
            Item::Fill {
                rect,
                top,
                mask,
                round_clip,
                ..
            } if *top == color => Some((*rect, mask.clone(), *round_clip)),
            _ => None,
        })
    }

    #[test]
    fn masks_and_rounded_clips_follow_their_nodes() {
        const RED: Rgba = [255, 0, 0, 255];
        const GREEN: Rgba = [0, 255, 0, 255];
        const BLUE: Rgba = [0, 0, 255, 255];
        let mut host = Host::new(
            crate::apps::MASKS.into(),
            "{}".into(),
            TextEngine::default(),
        );
        host.set_image("squircle".into(), vec![255; 4 * 2 * 2], [2, 2]);
        host.set_image("cutout".into(), vec![255; 4 * 2 * 2], [2, 2]);
        host.set_image("a".into(), vec![255; 4 * 2 * 2], [2, 2]);
        settle(&mut host);
        let squircle = host.images["squircle"].generation;
        let cutout = host.images["cutout"].generation;

        let frame = frame_of(&mut host);
        let (rect, mask, _) = fill_of(&frame, RED).expect("the masked box");
        let (mask_image, mask_rect) = mask.expect("the box carries its mask");
        assert_eq!((mask_image.generation, mask_rect), (squircle, rect));
        assert_eq!((rect[2], rect[3]), (40., 40.));

        let image = frame
            .items
            .iter()
            .find_map(|item| match item {
                Item::Image { rect, mask, .. } => Some((*rect, mask.clone())),
                _ => None,
            })
            .expect("the image");
        let (mask_image, mask_rect) = image.1.expect("the image carries its mask");
        assert_eq!((mask_image.generation, mask_rect), (cutout, image.0));

        // The child of the rounded clip is cut to its corners; the clip itself draws nothing.
        let (child, mask, round) = fill_of(&frame, GREEN).expect("the clipped child");
        assert!(mask.is_none());
        // The child fills its clip, so the clip's rounded box is the child's rectangle.
        assert_eq!(round, Some((child, [16.; 4])), "the clip's own rounded box");
        assert_eq!(fill_of(&frame, RED).unwrap().2, None);

        // A missing mask hides the node's own fill, and only that.
        assert!(fill_of(&frame, BLUE).is_none());
        host.remove_image("squircle");
        assert!(fill_of(&frame_of(&mut host), RED).is_none());
        assert!(fill_of(&frame_of(&mut host), GREEN).is_some());
    }

    #[test]
    fn centered_text_sits_in_the_middle_of_its_width() {
        let mut host = Host::new(
            crate::apps::TEXT.into(),
            "{}".into(),
            TextEngine::with_fonts(&[INTER], [255; 3]),
        );
        settle(&mut host);
        let frame = frame_of(&mut host);
        let texts: Vec<(Rect, TextStyle)> = frame
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Text {
                    rect, text, style, ..
                } if text == "Hi" => Some((*rect, style.clone())),
                _ => None,
            })
            .collect();
        let [(centered, style), (start, plain)] = texts.try_into().expect("two texts");
        assert_eq!(style.align, TextAlign::Center);
        assert_eq!(plain.align, TextAlign::Start);
        // Where the first ink column is: the text is rasterized as the view draws it.
        let mut first_ink = |rect: Rect, style: &TextStyle| {
            let bitmap = host
                .raster_text(
                    &node_spans("Hi", None),
                    [rect[2], rect[3]],
                    1.,
                    true,
                    4096,
                    style.clone(),
                )
                .expect("the text bitmap");
            let [width, height] = bitmap.size;
            (0..width)
                .find(|&x| (0..height).any(|y| bitmap.rgba[(y * width + x) * 4 + 3] > 0))
                .expect("ink") as f32
        };
        let (left, shifted) = (first_ink(start, &plain), first_ink(centered, &style));
        let width = host
            .text
            .measure_styled(&node_spans("Hi", None), None, &plain)[0];
        assert!(
            left < 2.,
            "the start-aligned text begins at the left: {left}"
        );
        let expected = (200. - width) / 2.;
        assert!(
            (shifted - left - expected).abs() <= 1.,
            "centered by {}, expected {expected}",
            shifted - left
        );
    }

    #[test]
    fn a_loaded_font_is_the_family_text_is_measured_with() {
        // A system font with its own metrics. Without it the test is skipped.
        const SECOND: &str = "/usr/share/fonts/TTF/DejaVuSans.ttf";
        let Ok(bytes) = std::fs::read(SECOND) else {
            eprintln!("skipped: {SECOND} is not installed");
            return;
        };
        let mut host = Host::new(
            crate::apps::MASKS.into(),
            "{}".into(),
            TextEngine::with_fonts(&[INTER], [255; 3]),
        );
        let text = "The quick brown fox";
        let plain = TextStyle::default();
        let named = TextStyle {
            family: Some("DejaVu Sans".into()),
            ..TextStyle::default()
        };
        let inter = host
            .text
            .measure_styled(&node_spans(text, None), None, &plain)[0];
        let epoch = host.font_epoch();
        host.add_font(bytes);
        assert_eq!(host.font_epoch(), epoch + 1);
        let dejavu = host
            .text
            .measure_styled(&node_spans(text, None), None, &named)[0];
        assert_ne!(
            dejavu, inter,
            "the named family is measured in its own font"
        );
        assert_eq!(
            host.text
                .measure_styled(&node_spans(text, None), None, &plain)[0],
            inter
        );

        // The default family is what text without one is measured with.
        host.set_default_family("DejaVu Sans".into());
        assert_eq!(
            host.text
                .measure_styled(&node_spans(text, None), None, &plain)[0],
            dejavu
        );

        // The focused field is shaped in its family at the size it is given: its caret sits at the width of
        // the text at that size.
        host.editor.reset(text, [19, 19], false);
        let style = |family: Option<&str>| TextStyle {
            size: 14.,
            family: family.map(str::to_owned),
            ..TextStyle::default()
        };
        let caret = |host: &mut Host, family: Option<&str>| {
            host.editor
                .frame(&mut host.text, &style(family), [400., 20.], 1., 4096, None)
                .caret_x
        };
        let field = |host: &mut Host, family: Option<&str>| {
            host.text
                .measure_styled(&node_spans(text, None), None, &style(family))[0]
        };
        let (none, inter14) = (caret(&mut host, None), caret(&mut host, Some("Inter")));
        assert!(
            (none - field(&mut host, Some("DejaVu Sans"))).abs() < 0.01,
            "{none}"
        );
        assert!(
            (inter14 - field(&mut host, Some("Inter"))).abs() < 0.01,
            "{inter14}"
        );
        assert_ne!(none, inter14);
    }

    #[test]
    fn rich_text_draws_its_runs_an_underline_and_an_inline_image() {
        let mut host = Host::new(
            crate::apps::SPANS.into(),
            "{}".into(),
            TextEngine::default(),
        );
        host.set_image("tag".into(), [255, 0, 0, 255].repeat(20 * 20), [20, 20]);
        settle(&mut host);
        let frame = host.frame(640., 560., 1., 4096);
        let text = |prefix: &str| {
            frame
                .items
                .iter()
                .find_map(|item| match item {
                    Item::Text {
                        text,
                        spans: Some(spans),
                        rect,
                        ..
                    } if text.starts_with(prefix) => Some((text.clone(), *rect, spans.clone())),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("rich text {prefix:?}"))
        };
        let style = TextStyle::default();

        // Each run keeps its own color, weight and underline.
        let (_, abc, runs) = text("abc");
        assert_eq!(
            runs.iter().map(|r| r.color).collect::<Vec<_>>(),
            [Some(0xff0000ff), None, None]
        );
        assert_eq!(runs[1].weight, Some(600));
        assert!(runs[2].underline);
        // The underline is a line under the glyphs of its run: at least 1 px thick, below the text's middle.
        let underline = frame
            .items
            .iter()
            .find_map(|item| match item {
                Item::Fill { rect, .. } if rect[3] >= 1. && rect[3] < 2. => Some(*rect),
                _ => None,
            })
            .expect("the underline");
        assert!(
            underline[1] > abc[1] + abc[3] / 2.,
            "{underline:?} in {abc:?}"
        );
        assert!(underline[0] > abc[0] && underline[0] + underline[2] <= abc[0] + abc[2] + 1.);

        // "x", an image 20 by 20, then "y": the text is as wide as the three, and the image sits after "x".
        let (_, xy, runs) = text("xy");
        assert!(runs[1].image.is_some());
        let x = host
            .text
            .measure_styled(&node_spans("x", None), None, &style)[0];
        let y = host
            .text
            .measure_styled(&node_spans("y", None), None, &style)[0];
        let width = host.text.measure_styled(&runs, None, &style)[0];
        assert!(
            (width - (x + 20. + y)).abs() <= 1.,
            "{width} vs {}",
            x + 20. + y
        );
        let image = frame
            .items
            .iter()
            .find_map(|item| match item {
                Item::Image { rect, .. } if rect[2] == 20. => Some(*rect),
                _ => None,
            })
            .expect("the inline image");
        assert_eq!(image[3], 20.);
        assert!(
            (image[0] - (xy[0] + x)).abs() <= 1.,
            "{image:?} after {xy:?}"
        );
        assert_eq!(
            frame
                .items
                .iter()
                .filter(|item| matches!(item, Item::Image { .. }))
                .count(),
            1
        );

        // A line limit cuts the spans, ellipsis included, and the box is one line high.
        let (shown, box_rect, _) = text("The");
        assert!(shown.ends_with('…'), "{shown:?}");
        assert!(box_rect[3] < 2. * style.line_height, "{box_rect:?}");
    }

    #[test]
    fn a_background_span_wrapped_onto_two_lines_is_two_fills_drawn_under_its_text() {
        let mut host = Host::new(
            crate::apps::SPANS.into(),
            "{}".into(),
            TextEngine::default(),
        );
        settle(&mut host);
        let frame = host.frame(640., 560., 1., 4096);
        let background = [0x20, 0x40, 0x80, 0xff];
        let fills = fills_of(&frame, background);
        assert_eq!(fills.len(), 2, "{fills:?}");
        assert!(fills[0][1] < fills[1][1], "one fill per line: {fills:?}");
        let first = frame
            .items
            .iter()
            .position(|item| matches!(item, Item::Fill { top, .. } if *top == background));
        let text = frame
            .items
            .iter()
            .position(
                |item| matches!(item, Item::Text { text, .. } if text.starts_with("xxxxxxxx")),
            )
            .expect("the wrapped text");
        assert!(
            first.is_some_and(|first| first < text),
            "drawn under the text"
        );
    }

    #[test]
    fn rich_text_places_its_marks_once_and_then_shapes_nothing() {
        let mut host = Host::new(
            crate::apps::SPANS.into(),
            "{}".into(),
            TextEngine::default(),
        );
        host.set_image("tag".into(), [255, 0, 0, 255].repeat(20 * 20), [20, 20]);
        let first = frame_of(&mut host);
        assert!(host.text.shapes > 0, "the first frame lays the text out");
        host.text.shapes = 0;
        let second = frame_of(&mut host);
        assert_eq!(host.text.shapes, 0, "unchanged rich text shapes nothing");
        assert_eq!(format!("{:?}", first.items), format!("{:?}", second.items));
    }

    #[test]
    fn a_focused_field_is_shaped_and_drawn_in_its_own_font_size() {
        let mut host = Host::new(
            crate::apps::FIELDS.into(),
            "{}".into(),
            TextEngine::default(),
        );
        settle(&mut host);
        host.focus_first_input();
        host.edit(&[EditorEvent::Move {
            direction: crate::editor::Direction::End,
            extend: false,
        }]);
        let frame = host.frame(640., 560., 1., 4096);
        let (editor, caret) = frame
            .items
            .iter()
            .find_map(|item| match item {
                Item::Editor { frame, caret, .. } => Some((frame.clone(), *caret)),
                _ => None,
            })
            .expect("the focused field");
        // The unfocused field draws "Hello" in its own font: the focused one must match its width.
        let plain = frame
            .items
            .iter()
            .find_map(|item| match item {
                Item::Text { text, style, .. } if text == "Hello" => Some(style.clone()),
                _ => None,
            })
            .expect("the unfocused field");
        assert_eq!(plain.size, 20.);
        let width = host
            .text
            .measure_styled(&node_spans("Hello", None), None, &plain)[0];
        assert!(
            (editor.caret_x - width).abs() <= 1.,
            "focused width {} vs unfocused {width}",
            editor.caret_x
        );
        let line_height = 20. * NORMAL_LINE_HEIGHT;
        assert!(
            (editor.line_height - line_height).abs() < 0.01,
            "{editor:?}"
        );
        let caret = caret.expect("the caret is shown");
        assert!((caret[3] - line_height).abs() < 0.01, "{caret:?}");
    }

    // A field can take the focus in the same batch that creates it, as a popout's search does.
    #[test]
    fn a_focus_operation_takes_the_field_before_the_first_edit() {
        let script = r#"function jsnirMount() { jsnir.batch(JSON.stringify({ ops: [
            { op: 'create', id: 1, node: { kind: 'column' } },
            { op: 'create', id: 2, parent: 1, node: { kind: 'input', text: 'x' } },
            { op: 'focus', id: 2 },
        ] })); }"#;
        let mut host = Host::new(script.into(), "{}".into(), TextEngine::default());
        settle(&mut host);
        assert_eq!(host.focused, Some(2));
        host.edit(&[EditorEvent::Insert("!".into())]);
        settle(&mut host);
        assert_eq!(field(&host).1, "!x");
    }

    // The overlay app: its panels are the fixed nodes anchored to a node, found by that anchor.
    // A focused multi-line field 200 px wide, with 20 px lines, no padding and room for four lines (`apps::COMPOSER`).
    fn composer() -> (Host, u16) {
        let mut host = Host::new(
            crate::apps::COMPOSER.into(),
            "{}".into(),
            TextEngine::with_fonts(&[INTER], [255; 3]),
        );
        settle(&mut host);
        host.focus_first_input();
        let id = find(&host, |n| matches!(n, Node::Input { .. }));
        (host, id)
    }

    // The text and the selection of a field, as the tree holds them.
    fn value(host: &Host, id: u16) -> (String, [u32; 2]) {
        match host.tree.nodes.get(&id) {
            Some(Node::Input {
                text, selection, ..
            }) => (text.clone(), *selection),
            _ => panic!("no field {id}"),
        }
    }

    // Draws the frame and returns the focused field's editor frame and caret.
    fn field_frame(host: &mut Host) -> (EditorFrame, Option<Rect>) {
        let frame = host.frame(640., 560., 1., 4096);
        frame
            .items
            .iter()
            .find_map(|item| match item {
                Item::Editor { frame, caret, .. } => Some((frame.as_ref().clone(), *caret)),
                _ => None,
            })
            .expect("the focused field")
    }

    #[test]
    fn a_multi_line_field_wraps_grows_by_lines_and_then_scrolls_to_keep_the_caret() {
        let (mut host, id) = composer();
        let mut heights = Vec::new();
        for _ in 0..8 {
            host.edit(&[EditorEvent::Insert(
                "The quick brown fox jumps over the lazy dog. ".into(),
            )]);
            host.frame(640., 560., 1., 4096);
            heights.push(host.tree.rect(id)[3]);
        }
        assert!(heights[0] > 20., "the first sentence wraps: {heights:?}");
        assert!(heights.windows(2).all(|w| w[0] <= w[1]), "{heights:?}");
        assert!(
            heights
                .iter()
                .all(|h| (h / 20. - (h / 20.).round()).abs() < 0.01),
            "whole lines: {heights:?}"
        );
        assert_eq!(
            heights.last(),
            Some(&80.),
            "four lines at most: {heights:?}"
        );
        let (frame, caret) = field_frame(&mut host);
        assert!(frame.content_height > 80., "{frame:?}");
        assert!(host.scroll.get(&id).is_some_and(|offset| *offset > 0.));
        assert_eq!(frame.scroll_y, host.scroll[&id]);
        assert!(
            frame.caret_y >= 0. && frame.caret_y + frame.line_height <= 80.,
            "the caret line is in view: {frame:?}"
        );
        assert!(caret.is_some_and(|c| c[1] >= host.tree.rect(id)[1]));
    }

    #[test]
    fn shift_enter_breaks_a_line_and_enter_submits_the_whole_text_and_clears_the_field() {
        let (mut host, id) = composer();
        host.edit(&[EditorEvent::Insert("first".into())]);
        host.edit(&[EditorEvent::LineBreak]);
        host.edit(&[EditorEvent::Insert("second".into())]);
        assert_eq!(value(&host, id).0, "first\nsecond");
        host.edit(&[EditorEvent::Enter]);
        settle(&mut host);
        assert!(
            texts(&host).contains(&"first\nsecond".into()),
            "{:?}",
            texts(&host)
        );
        assert_eq!(value(&host, id).0, "", "submitting clears the field");
    }

    #[test]
    fn up_and_down_keep_the_column_and_shift_extends_the_selection() {
        let (mut host, id) = composer();
        host.edit(&[EditorEvent::Insert("abcdefgh\nab\nabcdefgh".into())]);
        let move_to = |host: &mut Host, direction: Direction, extend: bool| {
            host.edit(&[EditorEvent::Move { direction, extend }]);
        };
        move_to(&mut host, Direction::TextStart, false);
        for _ in 0..6 {
            move_to(&mut host, Direction::Right, false);
        }
        assert_eq!(value(&host, id).1, [6, 6]);
        // The short line ends before column 6, so the caret goes to its end; the next line is back at column 6.
        move_to(&mut host, Direction::Down, false);
        assert_eq!(value(&host, id).1, [11, 11]);
        move_to(&mut host, Direction::Down, false);
        assert_eq!(value(&host, id).1, [18, 18]);
        // Shift+Up from there selects back to the short line's end.
        move_to(&mut host, Direction::Up, true);
        assert_eq!(value(&host, id).1, [11, 18]);

        move_to(&mut host, Direction::TextStart, false);
        for _ in 0..6 {
            move_to(&mut host, Direction::Right, false);
        }
        move_to(&mut host, Direction::Down, true);
        assert_eq!(value(&host, id).1, [6, 11]);
    }

    #[test]
    fn home_and_end_stop_at_the_edges_of_a_wrapped_visual_line() {
        let (mut host, id) = composer();
        let long = "one two three four five six seven eight nine ten eleven twelve thirteen";
        host.edit(&[EditorEvent::Insert(long.into())]);
        host.edit(&[EditorEvent::Move {
            direction: Direction::Start,
            extend: false,
        }]);
        let (frame, _) = field_frame(&mut host);
        let start = value(&host, id).1[0] as usize;
        // The last visual line starts after a space that wrapped, not at the start of the text.
        assert!(frame.caret_y > 0. && frame.caret_x.abs() < 0.5, "{frame:?}");
        assert!(start > 0 && long[..start].ends_with(' '), "{start}");
        host.edit(&[EditorEvent::Move {
            direction: Direction::TextStart,
            extend: false,
        }]);
        host.edit(&[EditorEvent::Move {
            direction: Direction::End,
            extend: false,
        }]);
        let (frame, _) = field_frame(&mut host);
        let end = value(&host, id).1[0] as usize;
        assert!(end > 0 && end < long.chars().count(), "{end}");
        assert_eq!(frame.caret_y, 0., "End stays on the first visual line");
    }

    #[test]
    fn a_click_on_the_second_line_places_the_caret_there_and_a_drag_selects_across_lines() {
        let (mut host, id) = composer();
        host.edit(&[EditorEvent::Insert(
            "one two three four five six seven eight nine ten eleven twelve".into(),
        )]);
        host.frame(640., 560., 1., 4096);
        let [x, y, _, _] = host.tree.rect(id);
        host.pointer_move(Some([x + 4., y + 30.]));
        host.pointer_down(PointSelection::Place);
        host.pointer_up();
        let (frame, _) = field_frame(&mut host);
        assert!((frame.caret_y - 20.).abs() < 0.5, "{frame:?}");

        host.pointer_move(Some([x + 2., y + 10.]));
        host.pointer_down(PointSelection::Place);
        host.frame(640., 560., 1., 4096);
        host.pointer_move(Some([x + 150., y + 50.]));
        host.frame(640., 560., 1., 4096);
        host.pointer_up();
        let (frame, _) = field_frame(&mut host);
        let (_, [start, end]) = value(&host, id);
        assert!(end > start, "the drag selects");
        let mut rows: Vec<f32> = frame.selection.iter().map(|r| r[1]).collect();
        rows.dedup();
        assert_eq!(rows, [0., 20., 40.], "{frame:?}");
    }

    #[test]
    fn pasted_lines_are_inserted_as_lines() {
        let (mut host, id) = composer();
        host.edit(&[EditorEvent::Insert("a\nb\nc".into())]);
        assert_eq!(value(&host, id).0, "a\nb\nc");
        let copied = host.edit(&[EditorEvent::SelectAll, EditorEvent::Copy]);
        assert_eq!(copied.clipboard.as_deref(), Some("a\nb\nc"));
    }

    #[test]
    fn the_wheel_scrolls_a_multi_line_field_while_the_pointer_is_over_it() {
        let (mut host, id) = composer();
        for _ in 0..8 {
            host.edit(&[EditorEvent::Insert(
                "The quick brown fox jumps over the lazy dog. ".into(),
            )]);
        }
        host.frame(640., 560., 1., 4096);
        let [x, y, _, _] = host.tree.rect(id);
        host.pointer_move(Some([x + 10., y + 10.]));
        let (bottom, _) = field_frame(&mut host);
        assert!(
            bottom.scroll_y > 0.,
            "the caret follows the end: {bottom:?}"
        );
        assert!(host.wheel(-40.), "the field can scroll up");
        let (scrolled, _) = field_frame(&mut host);
        assert!(scrolled.scroll_y < bottom.scroll_y && scrolled.scroll_y >= 0.);
        assert_eq!(host.scroll[&id], scrolled.scroll_y);
    }

    fn overlay() -> Host {
        let mut host = Host::new(
            crate::apps::OVERLAY.into(),
            "{}".into(),
            TextEngine::default(),
        );
        settle(&mut host);
        host
    }
    fn panel_for(host: &Host, anchor: u16) -> Option<u16> {
        host.tree
            .nodes
            .iter()
            .find(|(_, n)| {
                n.style().is_some_and(|s| {
                    s.position == Position::Fixed
                        && s.anchor.as_ref().is_some_and(|a| a.node == anchor)
                })
            })
            .map(|(id, _)| id)
    }
    // The box around the text `label`, and the wrapper a tooltip puts around the row of that text.
    fn box_of(host: &Host, label: &str) -> u16 {
        host.tree.parent(text_id(host, label)).unwrap()
    }
    fn wrapper_of(host: &Host, label: &str) -> u16 {
        host.tree
            .parent(box_of(host, label))
            .expect("a row in its tooltip wrapper")
    }
    fn centre(rect: Rect) -> [f32; 2] {
        [rect[0] + rect[2] / 2., rect[1] + rect[3] / 2.]
    }
    fn near_rect(a: Rect, b: Rect) -> bool {
        a.iter().zip(b).all(|(a, b)| (a - b).abs() < 0.01)
    }

    #[test]
    fn a_tooltip_sits_beside_its_row_at_a_gap_of_8_and_goes_when_the_pointer_leaves() {
        let mut host = overlay();
        let row = wrapper_of(&host, "row 0");
        let rect = host.screen_rect(row).unwrap();
        host.pointer_move(Some(centre(rect)));
        settle(&mut host);
        let tip = panel_for(&host, row).expect("the tooltip");
        let [x, y, w, h] = rect;
        let [tx, ty, _, th] = host.screen_rect(tip).unwrap();
        assert!((tx - (x + w + 8.)).abs() < 0.01, "{tx} after {x} + {w}");
        assert!(
            (ty + th / 2. - (y + h / 2.)).abs() < 0.01,
            "{ty} {th} on {y} {h}"
        );

        host.pointer_move(Some([600., 500.]));
        settle(&mut host);
        assert!(panel_for(&host, row).is_none(), "the tooltip is removed");
    }

    #[test]
    fn a_tooltip_follows_its_row_when_the_column_scrolls() {
        let mut host = overlay();
        let row = wrapper_of(&host, "row 0");
        let [x, y, w, _] = host.screen_rect(row).unwrap();
        // Five pixels into the visible part of the row: it stays under the pointer after the scroll.
        host.pointer_move(Some([x + 10., y + 5.]));
        settle(&mut host);
        let tip = panel_for(&host, row).expect("the tooltip");
        assert!(host.wheel(10.));
        settle(&mut host);
        let rect = host.screen_rect(row).unwrap();
        assert!((rect[1] - (y - 10.)).abs() < 0.01, "{rect:?} after {y}");
        let [tx, ty, _, th] = host.screen_rect(tip).unwrap();
        assert!((ty + th / 2. - (rect[1] + rect[3] / 2.)).abs() < 0.01);
        assert!((tx - (x + w + 8.)).abs() < 0.01, "{tx}");
    }

    #[test]
    fn a_tooltip_is_drawn_outside_the_scroll_column_that_holds_its_row() {
        let mut host = overlay();
        let row = wrapper_of(&host, "row 0");
        host.pointer_move(Some(centre(host.screen_rect(row).unwrap())));
        settle(&mut host);
        let column = find(&host, |n| n.style().is_some_and(|s| s.scroll));
        let column_rect = host.screen_rect(column).unwrap();
        let (rect, clip) = drawn(&frame_of(&mut host), "tip 0");
        assert!(
            rect[0] > column_rect[0] + column_rect[2],
            "{rect:?} is right of {column_rect:?}"
        );
        assert!(near_rect(clip, [0., 0., 640., 560.]), "clip {clip:?}");
    }

    fn placement_fixture(args: &str) -> (Host, Arc<Mutex<Vec<String>>>) {
        let script = format!(
            "{}\n{}",
            crate::apps::OVERLAY,
            r#"
            mount(function () {
                var open = signal(true);
                var anchor;
                onHost(function (message) {
                    open.value = message.open;
                });
                return Box({children: [
                    Box({ref: function (id) { anchor = id; }, onPress: function () {},
                         paint: {hover: {y: 5}, transitions: []},
                         style: {position: "absolute", left: 80, top: 80, width: 20, height: 20}}),
                    Popover({open: open, anchor: function () { return anchor; }, side: "right", layer: 2,
                             align: "center", style: {width: 60, height: 50}, children: h(Text, null, "placement panel"),
                             onPlacement: function (p) { jsnirRequest(JSON.stringify(p)); }}),
                    args().multiple ? Popover({open: open, anchor: function () { return anchor; }, side: "right",
                             align: "center", style: {width: 60, height: 40}, children: h(Text, null, "second panel"),
                             onPlacement: function (p) { jsnirRequest(JSON.stringify(p)); }}) : null
                ]});
            });
            "#,
        );
        let events = Arc::new(Mutex::new(Vec::new()));
        let received = events.clone();
        let host =
            Host::with_request_handler(script, args.into(), TextEngine::default(), move |text| {
                received.lock().unwrap().push(text);
                Ok("null".into())
            })
            .unwrap();
        (host, events)
    }

    fn poll_placement_fixture(host: &mut Host) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while host.busy() {
            assert!(Instant::now() < deadline, "the app did not answer");
            host.poll();
            std::thread::yield_now();
        }
    }

    #[test]
    fn popover_placement_reports_center_flip_and_cross_axis_clamp_in_logical_pixels() {
        let (mut host, events) = placement_fixture("{}");
        for (width, height, scale, rect) in [
            (200., 180., 1., [108., 65., 60., 50.]),
            (200., 180., 2., [108., 65., 60., 50.]),
            (130., 180., 2., [12., 65., 60., 50.]),
            (130., 110., 2., [12., 52., 60., 50.]),
        ] {
            assert!(!host.frame(width, height, scale, 4096).animating);
            assert_eq!(
                host.screen_rect(box_of(&host, "placement panel")),
                Some(rect)
            );
            poll_placement_fixture(&mut host);
        }
        assert_eq!(
            *events.lock().unwrap(),
            [
                r#"{"side":"right","anchorOffset":25}"#,
                r#"{"side":"left","anchorOffset":25}"#,
                r#"{"side":"left","anchorOffset":38}"#,
            ]
        );
        host.push(r#"{"open":false}"#.into());
        host.push(r#"{"open":true}"#.into());
        poll_placement_fixture(&mut host);
        assert!(!host.frame(130., 110., 2., 4096).animating);
        poll_placement_fixture(&mut host);
        assert_eq!(
            &events.lock().unwrap()[3..],
            [r#"{"side":"left","anchorOffset":38}"#]
        );
    }

    #[test]
    fn placements_use_the_final_hover_pass_in_layer_order() {
        let (mut host, events) = placement_fixture(r#"{"multiple":true}"#);
        // No hit exists until the first render. The second pass draws the anchor 5 px lower.
        host.pointer_move(Some([90., 90.]));
        assert!(!host.frame(130., 110., 1., 4096).animating);
        poll_placement_fixture(&mut host);
        assert_eq!(
            *events.lock().unwrap(),
            [
                r#"{"side":"left","anchorOffset":33}"#,
                r#"{"side":"left","anchorOffset":43}"#,
            ]
        );
    }

    #[test]
    fn a_placement_does_not_overtake_an_older_placement_in_the_backlog() {
        let (mut host, events) = placement_fixture("{}");
        for _ in 0..MAX_PENDING {
            host.push(r#"{"open":true}"#.into());
        }
        host.frame(200., 180., 1., 4096);

        // The first poll with an available reply tries the backlog before freeing an event slot.
        let deadline = Instant::now() + Duration::from_secs(5);
        while host.pending == MAX_PENDING {
            assert!(Instant::now() < deadline, "the app did not answer");
            host.poll();
            std::thread::yield_now();
        }
        host.frame(130., 110., 1., 4096);
        poll_placement_fixture(&mut host);
        assert_eq!(
            *events.lock().unwrap(),
            [
                r#"{"side":"right","anchorOffset":25}"#,
                r#"{"side":"left","anchorOffset":38}"#,
            ]
        );
    }

    #[test]
    fn a_queued_placement_cannot_reach_a_recycled_callback() {
        let (mut host, events) = placement_fixture("{}");
        // JS handles the remount before the queued placement for the original panel.
        host.push(r#"{"open":false}"#.into());
        host.push(r#"{"open":true}"#.into());
        host.frame(200., 180., 1., 4096);
        poll_placement_fixture(&mut host);
        assert!(events.lock().unwrap().is_empty());
        host.frame(200., 180., 1., 4096);
        poll_placement_fixture(&mut host);
        assert_eq!(
            *events.lock().unwrap(),
            [r#"{"side":"right","anchorOffset":25}"#]
        );
    }

    #[test]
    fn a_popover_flips_at_the_window_edge_and_clamps_at_the_bottom() {
        let mut host = overlay();
        let edge = box_of(&host, "edge");
        let [ex, ey, _, _] = host.screen_rect(edge).unwrap();
        click(&mut host, "edge");
        let panel = panel_for(&host, edge).expect("the edge popover");
        let [px, py, pw, _] = host.screen_rect(panel).unwrap();
        // Right of the button there is no room for 200 px: the panel goes left, 8 px from the button.
        assert!((px + pw + 8. - ex).abs() < 0.01, "{px} + {pw} against {ex}");
        assert!((py - ey).abs() < 0.01, "{py} against {ey}");

        click(&mut host, "low");
        let low = box_of(&host, "low");
        let panel = panel_for(&host, low).expect("the low popover");
        let [_, py, _, ph] = host.screen_rect(panel).unwrap();
        // Its side and the one above it both overflow: it stays on its side, 8 px inside the window's bottom.
        assert!((py + ph - (560. - 8.)).abs() < 0.01, "{py} + {ph}");
    }

    #[test]
    fn a_popover_covers_the_rows_under_it_for_hover_clicks_and_wheel() {
        let mut host = overlay();
        let row = host.tree.parent(text_id(&host, "row 2")).unwrap();
        let rect = host.screen_rect(row).unwrap();
        host.pointer_move(Some(centre(rect)));
        settle(&mut host);
        assert_eq!(fills_of(&frame_of(&mut host), ROW_FILL).len(), 1);

        click(&mut host, "menu");
        let menu = box_of(&host, "menu");
        assert!(panel_for(&host, menu).is_some());
        // The popover spans y 56 to 176, and row 2 is under it at y 124 to 154.
        host.pointer_move(Some(centre(rect)));
        settle(&mut host);
        assert!(fills_of(&frame_of(&mut host), ROW_FILL).is_empty());
        let column = find(&host, |n| n.style().is_some_and(|s| s.scroll));
        let offset = host.scroll.get(&column).copied().unwrap_or(0.);
        assert!(
            !host.wheel(30.),
            "the wheel goes to the popover, not the column"
        );
        assert_eq!(host.scroll.get(&column).copied().unwrap_or(0.), offset);

        // The item of the popover lies over row 0: the press reaches the item only.
        let item = box_of(&host, "item");
        host.pointer_move(Some(centre(host.screen_rect(item).unwrap())));
        host.pointer_down(PointSelection::Place);
        host.pointer_up();
        settle(&mut host);
        assert!(
            texts(&host).contains(&"picked item".into()),
            "{:?}",
            texts(&host)
        );
        assert!(
            panel_for(&host, menu).is_some(),
            "a press inside keeps it open"
        );
    }

    #[test]
    fn escape_and_a_press_outside_dismiss_a_popover_and_a_press_inside_does_not() {
        let mut host = overlay();
        click(&mut host, "menu");
        let menu = box_of(&host, "menu");
        assert!(panel_for(&host, menu).is_some());

        let item = box_of(&host, "item");
        host.pointer_move(Some(centre(host.screen_rect(item).unwrap())));
        host.pointer_down(PointSelection::Place);
        host.pointer_up();
        settle(&mut host);
        assert!(
            panel_for(&host, menu).is_some(),
            "a press inside does not dismiss"
        );

        host.escape();
        settle(&mut host);
        assert!(panel_for(&host, menu).is_none(), "Escape dismisses");

        click(&mut host, "menu");
        assert!(panel_for(&host, menu).is_some());
        host.pointer_move(Some([600., 500.]));
        host.pointer_down(PointSelection::Place);
        host.pointer_up();
        settle(&mut host);
        assert!(
            panel_for(&host, menu).is_none(),
            "a press outside dismisses"
        );
    }

    #[test]
    fn a_press_on_the_anchor_closes_its_popover_once_and_the_next_opens_it() {
        let mut host = overlay();
        let menu = box_of(&host, "menu");
        click(&mut host, "menu");
        assert!(panel_for(&host, menu).is_some());

        // The anchor is not outside the popover: the press sends no dismiss, and the anchor's own press closes it.
        let rect = host.screen_rect(menu).unwrap();
        host.pointer_move(Some(centre(rect)));
        host.pointer_down(PointSelection::Place);
        assert_eq!(
            host.pending, 0,
            "no dismiss is sent for a press on the anchor"
        );
        host.pointer_up();
        settle(&mut host);
        assert!(panel_for(&host, menu).is_none(), "the anchor closes it");

        click(&mut host, "menu");
        assert!(
            panel_for(&host, menu).is_some(),
            "and the next press opens it"
        );
    }

    #[test]
    fn a_right_press_reports_its_window_point_to_the_box_under_it() {
        let mut host = overlay();
        let picker = find(
            &host,
            |n| matches!(n, Node::Box { contextmenu, .. } if *contextmenu != 0),
        );
        let [x, y, _, _] = host.screen_rect(picker).unwrap();
        let (px, py) = (x + 76., y + 16.);
        host.pointer_move(Some([px, py]));
        host.context_press();
        settle(&mut host);
        let expected = format!("picked context {px},{py}");
        assert!(texts(&host).contains(&expected), "{:?}", texts(&host));
    }

    #[test]
    fn a_for_list_in_a_scroll_box_follows_its_content_and_reclamps_its_offset() {
        let mut host = Host::new(
            crate::apps::KEYED.into(),
            r#"{"items":[{"key":"a","name":"A"}]}"#.into(),
            TextEngine::default(),
        );
        settle(&mut host);
        let list = host.tree.children(host.tree.root.unwrap())[0];
        let items = |n: usize| {
            let rows: Vec<String> = (0..n)
                .map(|i| format!(r#"{{"key":"k{i}","name":"Row {i}"}}"#))
                .collect();
            format!(r#"{{"items":[{}]}}"#, rows.join(","))
        };
        host.push(items(12));
        settle(&mut host);
        let content = host.layout.content_height(list);
        assert!(content > 60., "{content}");
        host.pointer_move(Some(centre(host.tree.rect(list))));
        assert!(host.wheel(10_000.));
        settle(&mut host);
        assert_eq!(host.scroll[&list], content - 60.);

        host.push(items(2));
        settle(&mut host);
        let shorter = host.layout.content_height(list);
        assert!(shorter < content, "{shorter} after {content}");
        let max = (shorter - 60.).max(0.);
        assert_eq!(host.scroll[&list], max, "re-clamped to the new content");
    }

    // The fill app laid out in a 400 by 300 window, after its first frame at the test's usual size.
    fn fill(shrink: bool, direct: bool) -> Host {
        let mut host = Host::new(
            crate::apps::FILL.into(),
            format!(r#"{{"shrink":{shrink},"direct":{direct}}}"#),
            TextEngine::default(),
        );
        settle(&mut host);
        host.frame(400., 300., 1., 4096);
        host
    }
    fn footer_and_column(host: &Host) -> (Rect, Rect) {
        let footer = find(host, |n| n.style().is_some_and(|s| s.height == Some(56.)));
        let column = find(host, |n| n.style().is_some_and(|s| s.scroll));
        (host.tree.rect(footer), host.tree.rect(column))
    }

    #[test]
    fn a_grow_row_with_min_height_zero_lets_its_scroll_column_shrink_above_the_footer() {
        let host = fill(true, false);
        let (footer, column) = footer_and_column(&host);
        assert!((footer[1] + footer[3] - 300.).abs() < 0.01, "{footer:?}");
        assert!((column[3] - (300. - 56.)).abs() < 0.01, "{column:?}");
    }

    #[test]
    fn a_scroll_column_in_a_grow_column_shrinks_without_any_minimum_set() {
        // Taffy gives a scroll node an automatic minimum of 0, as CSS does: no `minHeight` is needed.
        let host = fill(false, true);
        let (footer, column) = footer_and_column(&host);
        assert!((footer[1] + footer[3] - 300.).abs() < 0.01, "{footer:?}");
        assert!((column[3] - (300. - 56.)).abs() < 0.01, "{column:?}");
    }

    #[test]
    fn without_min_height_zero_the_row_keeps_its_content_height_as_css_does() {
        let host = fill(false, false);
        let (footer, column) = footer_and_column(&host);
        // CSS: the row's automatic minimum is its content, 50 rows of 30 px, so the footer is pushed below the window.
        assert!((footer[1] - 1500.).abs() < 0.01, "{footer:?}");
        assert!((column[3] - 1500.).abs() < 0.01, "{column:?}");
    }

    const LIST_APP: &str = r#"
        function jsnirMount() {
            jsnir.batch(JSON.stringify({ ops: [
                { op: 'create', id: 1, node: { kind: 'column' } },
                { op: 'create', id: 2, parent: 1, node: { kind: 'list', style: { height: 20 } } },
                { op: 'create', id: 3, parent: 2, node: { kind: 'text', text: 'legacy row' } }
            ] }));
        }
        function jsnirEvent(_, text) { jsnir.batch(text); }
    "#;

    #[test]
    fn a_recycled_id_is_laid_out_under_its_new_parent() {
        const APP: &str = r#"
            function jsnirMount() {
                jsnir.batch(JSON.stringify({ ops: [
                    { op: 'create', id: 1, node: { kind: 'column' } },
                    { op: 'create', id: 2, parent: 1, node: { kind: 'column', style: { height: 20 } } },
                    { op: 'create', id: 4, parent: 2, node: { kind: 'column', style: { height: 5 } } },
                    { op: 'create', id: 3, parent: 1, node: { kind: 'column', style: { height: 30 } } }
                ] }));
            }
            function jsnirEvent(_, text) { jsnir.batch(text); }
        "#;
        let mut host = Host::new(APP.into(), "{}".into(), TextEngine::default());
        settle(&mut host);
        // 2 goes with 4; 4 comes back under 3, and 5 is created and removed in the same batch.
        host.push(r#"{"ops":[{"op":"remove","id":2},
            {"op":"create","id":4,"parent":3,"node":{"kind":"column","style":{"height":7}}},
            {"op":"create","id":5,"parent":1,"node":{"kind":"column","style":{"height":40}}},
            {"op":"remove","id":5}]}"#.into());
        settle(&mut host);
        assert_eq!(host.tree.parent(4), Some(3));
        let three = host.screen_rect(3).unwrap();
        let four = host.screen_rect(4).unwrap();
        assert_eq!((four[1], four[3]), (three[1], 7.), "4 sits at the top of 3: {three:?} {four:?}");
        assert_eq!(three[1], 0., "2 and 5 left no room above 3");
    }

    #[test]
    fn a_recycled_id_does_not_inherit_a_removed_lists_mount() {
        const APP: &str = r#"
            function jsnirMount() {
                jsnir.batch(JSON.stringify({ ops: [{ op: 'create', id: 1, node: { kind: 'column' } }] }));
            }
            function jsnirEvent(_, text) { jsnir.batch(text); }
        "#;
        let mut host = Host::new(APP.into(), "{}".into(), TextEngine::default());
        settle(&mut host);
        // 2 is mounted as a virtual list, removed, and its ID comes back as a plain column in the same batch.
        host.push(r#"{"ops":[{"op":"create","id":2,"parent":1,"node":{"kind":"list","style":{"height":20}}},
            {"op":"virtuallist","id":2,"name":"messages"},
            {"op":"remove","id":2},
            {"op":"create","id":2,"parent":1,"node":{"kind":"column"}}]}"#.into());
        settle(&mut host);
        assert!(matches!(host.tree.nodes.get(&2), Some(Node::Column { .. })));
        assert!(!host.list_mounts.contains_key(&2));
    }

    fn list_host() -> Host {
        Host::new(LIST_APP.into(), "{}".into(), TextEngine::default())
    }

    fn virtual_list_script() -> String {
        fixture_script("virtual-list")
    }

    fn fixture_script(app: &str) -> String {
        struct Scratch(std::path::PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                std::fs::remove_dir_all(&self.0).expect("clean compiled fixture");
            }
        }
        let out = std::process::Command::new("mktemp")
            .arg("-d")
            .output()
            .unwrap();
        assert!(out.status.success());
        let dir = Scratch(String::from_utf8(out.stdout).unwrap().trim().into());
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let output = std::process::Command::new(crate::typescript::compiler(root))
            .current_dir(root)
            .args([
                "--ignoreConfig",
                "--strict",
                "--target",
                "es2020",
                "--lib",
                "es2020",
                "--types",
                "./app/jsnir.d.ts",
                "--jsx",
                "react",
                "--jsxFactory",
                "h",
                "app/sdk.ts",
                &format!("app/{app}.tsx"),
                "--outDir",
            ])
            .arg(&dir.0)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        std::fs::read_to_string(dir.0.join("sdk.js")).unwrap()
            + &std::fs::read_to_string(dir.0.join(format!("{app}.js"))).unwrap()
    }

    #[test]
    fn sdk_removes_a_for_row_whose_cleanup_throws() {
        let mut host = Host::new(fixture_script("cleanup-throw"), "{}".into(), TextEngine::default());
        settle(&mut host);
        assert!(texts(&host).contains(&"row drop".into()));
        host.push("{}".into());
        settle(&mut host);
        assert!(!texts(&host).contains(&"row drop".into()));
        assert!(texts(&host).contains(&"row keep".into()));
    }

    #[test]
    fn sdk_frees_the_ids_of_mounts_nested_in_a_removed_one() {
        let mut host = Host::new(fixture_script("remount"), "{}".into(), TextEngine::default());
        settle(&mut host);
        for _ in 0..10 {
            host.push("{}".into());
            settle(&mut host);
        }
        assert!(texts(&host).contains(&"nested 10".into()));
    }

    #[test]
    fn sdk_virtual_list_delivers_only_visible_rows_and_keeps_resident_controls() {
        const KEY: u64 = 9_007_199_254_740_993;
        let script = virtual_list_script();
        let mut host = Host::new(script.clone(), "{}".into(), TextEngine::default());
        let lookups = Arc::new(Mutex::new(Vec::new()));
        let content = Arc::new(Mutex::new(HashMap::from([(KEY + 1, "before")])));
        let index = (KEY..KEY + 10_000).map(|key| (key, 24.));
        let id = host.connect_virtual_list("messages", index, {
            let (lookups, content) = (lookups.clone(), content.clone());
            move |key| {
                lookups.lock().unwrap().push(key);
                let content = content.lock().unwrap();
                Ok(serde_json::json!({"author":"Ada", "body":content.get(&key).unwrap_or(&"message")}))
            }
        }).unwrap();
        host.set_image("attachment".into(), vec![255; 4], [1, 1]);
        assert!(lookups.lock().unwrap().is_empty());
        settle(&mut host);
        assert_eq!(*lookups.lock().unwrap(), [KEY, KEY + 1]);
        let label = format!("react {}: 0", KEY + 1);
        let control = |host: &Host, label: &str| {
            host.tree
                .nodes
                .iter()
                .find_map(|(id, _)| (host.tree.text(id) == Some(label)).then_some(id))
                .unwrap()
        };
        let control_id = control(&host, &label);
        click(&mut host, &label);
        let label = format!("react {}: 1", KEY + 1);
        assert_eq!(control(&host, &label), control_id);
        assert!(host.set_list_offset(id, ListOffset::Pixels(24.)));
        settle(&mut host);
        assert_eq!(control(&host, &label), control_id);
        assert_eq!(*lookups.lock().unwrap(), [KEY, KEY + 1, KEY + 2]);
        assert!(host.mutate_list(id, ListMutation::Prepend(KEY - 1, 24.)));
        settle(&mut host);
        assert_eq!(host.list_offset(id), Some(48.));
        assert_eq!(*lookups.lock().unwrap(), [KEY, KEY + 1, KEY + 2]);
        content.lock().unwrap().insert(KEY + 1, "edited");
        assert!(host.mutate_list(id, ListMutation::Update(KEY + 9999, 24.)));
        assert!(host.mutate_list(id, ListMutation::Update(KEY + 1, 24.)));
        settle(&mut host);
        assert!(texts(&host).contains(&"Ada edited".into()));
        assert_eq!(control(&host, &label), control_id);
        assert_eq!(*lookups.lock().unwrap(), [KEY, KEY + 1, KEY + 2, KEY + 1]);
        assert!(host.mutate_list(id, ListMutation::Remove(KEY + 1)));
        settle(&mut host);
        assert!(!texts(&host).contains(&label));
        assert_eq!(host.virtual_list_error(), None);
        assert!(host.reload(script));
        settle(&mut host);
        assert_eq!(host.take_reload_result(), Some(Ok(())));
        assert!(!host.set_list_offset(id, ListOffset::Tail));
    }

    fn demand(entering: &[u64], changed: &[u64], evicted: &[u64]) -> Option<ListDemand> {
        Some(ListDemand {
            entering: entering.to_vec(),
            changed: changed.to_vec(),
            evicted: evicted.to_vec(),
            retired: None,
        })
    }

    // Test the real batch/answer boundary without introducing a delivery protocol.
    fn reply_batch(host: &mut Host, ops: &str) -> Arc<Batch> {
        host.push(format!(r#"{{"ops":{ops}}}"#));
        let updates = &host.bridge.updates;
        let batch = updates.recv_timeout(Duration::from_secs(1)).unwrap();
        host.pending -= 1;
        assert_eq!(host.inflight.pop_front(), Some(Inflight::Event));
        batch
    }

    #[test]
    fn list_reply_gate_commits_only_delta_batches() {
        let mut host = list_host();
        let keys = (0..10_000).map(|n| (n, 10.));
        let id = host.replace_auto_virtual_list(2, keys).unwrap();
        assert!(host.propose_list_reply(id).is_none());
        host.frame(100., 100., 1., 4096);
        let (first, delta) = host.propose_list_reply(id).unwrap();
        assert_eq!(Some(delta), demand(&[0, 1], &[], &[]));
        assert!(host.propose_list_reply(id).is_none());
        assert_eq!(host.virtual_list_mut(id).unwrap().reply.accepted, 0);
        let ops = r#"[
            {"op":"create","id":4,"parent":2,"node":{"kind":"text","text":"a"}},
            {"op":"create","id":5,"parent":2,"node":{"kind":"text","text":"b"}}]"#;
        let batch = reply_batch(&mut host, ops);
        let result = host.accept_list_reply(first, batch, &[(0, 4), (1, 5)]);
        assert_eq!(result, Ok(()));
        assert_eq!(accepted(&mut host, id), first.serial);
        assert!(host.propose_list_reply(id).is_none());
        assert!(host.mutate_list(id, ListMutation::Update(1, 10.)));
        let (edit, delta) = host.propose_list_reply(id).unwrap();
        assert_eq!(Some(delta), demand(&[], &[1], &[]));
        let batch = reply_batch(&mut host, r#"[{"op":"settext","id":5,"text":"edited"}]"#);
        assert_eq!(host.accept_list_reply(edit, batch, &[]), Ok(()));
        assert_eq!(host.tree.text(5), Some("edited"));
        assert_eq!(host.bound_rows.get(&5), Some(&(2, 1)));
        host.set_list_offset(id, ListOffset::Pixels(10.));
        host.frame(100., 100., 1., 4096);
        let (shift, delta) = host.propose_list_reply(id).unwrap();
        assert_eq!(Some(delta), demand(&[2], &[], &[0]));
        let ops = r#"[{"op":"remove","id":4},
            {"op":"create","id":4,"parent":2,"node":{"kind":"text","text":"c"}}]"#;
        let batch = reply_batch(&mut host, ops);
        assert_eq!(host.accept_list_reply(shift, batch, &[(2, 4)]), Ok(()));
        assert_eq!(host.bound_rows.get(&4), Some(&(2, 2)));
        assert!(host.mutate_list(id, ListMutation::Remove(2)));
        assert!(host.mutate_list(id, ListMutation::Append(2, 10.)));
        host.set_list_offset(id, ListOffset::Tail);
        host.frame(100., 100., 1., 4096);
        let (bad, delta) = host.propose_list_reply(id).unwrap();
        assert_eq!(Some(delta), demand(&[9999, 2], &[], &[1, 2]));
        let batch = reply_batch(&mut host, "[]");
        let result = host.accept_list_reply(bad, batch, &[]);
        assert_eq!(result, Err(ListReplyError::Rebuild));
        assert_eq!(accepted(&mut host, id), shift.serial);
        assert!(host.propose_list_reply(id).is_none());
        assert!(host.reload(LIST_APP.into()));
        settle(&mut host);
        assert_eq!(host.take_reload_result(), Some(Ok(())));
        assert!(host.virtual_list_mut(id).is_none());
    }

    fn accepted(host: &mut Host, id: VirtualListId) -> u64 {
        host.virtual_list_mut(id).unwrap().reply.accepted
    }

    #[test]
    fn list_reply_gate_rejects_invalid_batches_and_a_recycled_scope() {
        let invalid = r#"[{"op":"create","id":4,"parent":2,"node":{"kind":"label","value":1000}}]"#;
        let wrong = r#"[{"op":"create","id":4,"parent":1,"node":{"kind":"label","value":1}}]"#;
        for (ops, row) in [(invalid, 4), ("[]", 3), (wrong, 4)] {
            let mut host = list_host();
            let id = host.replace_auto_virtual_list(2, [(7, 10.)]).unwrap();
            host.frame(100., 100., 1., 4096);
            let (reply, _) = host.propose_list_reply(id).unwrap();
            let base = host.tree.app_seq;
            let batch = reply_batch(&mut host, ops);
            let result = host.accept_list_reply(reply, batch, &[(7, row)]);
            assert_eq!(result, Err(ListReplyError::Rebuild));
            assert_eq!(host.tree.app_seq, base);
            assert_eq!(host.tree.children(2), [3]);
            assert!(host.bound_rows.is_empty());
            assert_eq!(host.virtual_list_mut(id).unwrap().reply.accepted, 0);
            assert!(host.propose_list_reply(id).is_none());
        }
        // This Host actually reloads and recycles node IDs in a fresh VM.
        let mut host = list_host();
        let id = host.replace_auto_virtual_list(2, [(7, 10.)]).unwrap();
        host.frame(100., 100., 1., 4096);
        let (old, _) = host.propose_list_reply(id).unwrap();
        assert!(host.reload(LIST_APP.into()));
        settle(&mut host);
        assert_eq!(host.take_reload_result(), Some(Ok(())));
        let id = host.replace_auto_virtual_list(2, []).unwrap();
        host.frame(100., 100., 1., 4096);
        let (fresh, delta) = host.propose_list_reply(id).unwrap();
        assert_eq!(delta, ListDemand::default());
        let wrong_vm = ListReplyId {
            vm: old.vm,
            ..fresh
        };
        let wrong_list = ListReplyId {
            list: old.list,
            ..fresh
        };
        for stale in [old, wrong_vm, wrong_list] {
            let batch = Batch {
                base: host.tree.app_seq,
                ops: Vec::new(),
            };
            let result = host.accept_list_reply(stale, batch, &[]);
            assert_eq!(result, Err(ListReplyError::Stale));
        }
        let batch = reply_batch(&mut host, "[]");
        assert_eq!(host.accept_list_reply(fresh, batch, &[]), Ok(()));
    }

    #[test]
    fn list_reply_gate_fences_vm_swap_even_when_reload_batch_is_rejected() {
        let mut host = list_host();
        let id = host.replace_auto_virtual_list(2, []).unwrap();
        host.frame(100., 100., 1., 4096);
        let (old, _) = host.propose_list_reply(id).unwrap();
        let base = host.tree.app_seq;
        let script = r#"
            function jsnirMount() {
                jsnir.batch('{"ops":[{"op":"create","id":1,"node":{"kind":"label","value":1000}}]}');
            }
        "#;
        assert!(host.reload(script.into()));
        settle(&mut host);
        assert_eq!(host.take_reload_result(), Some(Ok(())));
        assert_eq!(host.tree.app_seq, base);
        assert_eq!(host.tree.children(2), [3]);
        let batch = Batch { base, ops: vec![] };
        let result = host.accept_list_reply(old, batch, &[]);
        assert_eq!(result, Err(ListReplyError::Stale));
        assert_eq!(host.tree.app_seq, base);
        assert_eq!(accepted(&mut host, id), 0);
        assert!(host.propose_list_reply(id).is_none());
    }

    #[test]
    fn list_reply_gate_preserves_ticket_when_reload_never_swaps_vm() {
        let mut host = list_host();
        let id = host.replace_auto_virtual_list(2, []).unwrap();
        host.frame(100., 100., 1., 4096);
        let (ticket, _) = host.propose_list_reply(id).unwrap();
        for script in [
            "function (",
            "function jsnirMount() { throw Error('mount'); }",
        ] {
            assert!(host.reload(script.into()));
            settle(&mut host);
            assert!(host.take_reload_result().unwrap().is_err());
        }
        let batch = reply_batch(&mut host, "[]");
        assert_eq!(host.accept_list_reply(ticket, batch, &[]), Ok(()));
    }

    #[test]
    fn list_reply_gate_fences_residency_aba() {
        for remove_key in [true, false] {
            let mut host = list_host();
            let keys = (0..6).map(|n| (n, 10.));
            let id = host.replace_auto_virtual_list(2, keys).unwrap();
            host.frame(100., 100., 1., 4096);
            let (reply, _) = host.propose_list_reply(id).unwrap();
            if remove_key {
                assert!(host.mutate_list(id, ListMutation::Remove(0)));
                assert!(host.mutate_list(id, ListMutation::Prepend(0, 10.)));
                host.set_list_offset(id, ListOffset::Pixels(0.));
            } else {
                host.set_list_offset(id, ListOffset::Pixels(30.));
                host.frame(100., 100., 1., 4096);
                host.set_list_offset(id, ListOffset::Pixels(0.));
            }
            host.frame(100., 100., 1., 4096);
            let ops = r#"[
                {"op":"create","id":4,"parent":2,"node":{"kind":"label","value":1}},
                {"op":"create","id":5,"parent":2,"node":{"kind":"label","value":2}}]"#;
            let batch = reply_batch(&mut host, ops);
            let result = host.accept_list_reply(reply, batch, &[(0, 4), (1, 5)]);
            assert_eq!(result, Err(ListReplyError::Rebuild));
            assert_eq!(host.tree.children(2), [3]);
            assert!(host.bound_rows.is_empty());
            assert_eq!(host.virtual_list_mut(id).unwrap().reply.accepted, 0);
            assert!(host.propose_list_reply(id).is_none());
        }
    }

    #[test]
    fn list_reply_gate_does_not_apply_a_change_to_a_recycled_row() {
        let mut host = list_host();
        let id = host.replace_auto_virtual_list(2, [(7, 10.)]).unwrap();
        host.frame(100., 100., 1., 4096);
        let (first, _) = host.propose_list_reply(id).unwrap();
        let create = r#"[{"op":"create","id":4,"parent":2,"node":{"kind":"text","text":"old"}}]"#;
        let batch = reply_batch(&mut host, create);
        assert_eq!(host.accept_list_reply(first, batch, &[(7, 4)]), Ok(()));
        assert!(host.mutate_list(id, ListMutation::Update(7, 10.)));
        let (edit, delta) = host.propose_list_reply(id).unwrap();
        assert_eq!(Some(delta), demand(&[], &[7], &[]));
        let recycle = r#"[{"op":"remove","id":4},
            {"op":"create","id":4,"parent":2,"node":{"kind":"text","text":"new"}}]"#;
        let batch = reply_batch(&mut host, recycle);
        assert!(host.receive(batch));
        let edit_ops = r#"[{"op":"settext","id":4,"text":"stale"}]"#;
        let batch = reply_batch(&mut host, edit_ops);
        let result = host.accept_list_reply(edit, batch, &[]);
        assert_eq!(result, Err(ListReplyError::Rebuild));
        assert_eq!(host.tree.text(4), Some("new"));
        assert_eq!(accepted(&mut host, id), first.serial);
    }

    #[test]
    fn list_reply_gate_fences_recycled_descendants_only_in_the_bound_scope() {
        for (child, parent, expected) in [(3, 2, Ok(())), (6, 5, Err(ListReplyError::Rebuild))] {
            let mut host = list_host();
            let id = host.replace_auto_virtual_list(2, [(7, 10.)]).unwrap();
            host.frame(100., 100., 1., 4096);
            let (first, _) = host.propose_list_reply(id).unwrap();
            let create = r#"[
                {"op":"create","id":4,"parent":2,"node":{"kind":"column"}},
                {"op":"create","id":5,"parent":4,"node":{"kind":"column"}},
                {"op":"create","id":6,"parent":5,"node":{"kind":"text","text":"old"}}]"#;
            let batch = reply_batch(&mut host, create);
            assert_eq!(host.accept_list_reply(first, batch, &[(7, 4)]), Ok(()));
            assert!(host.mutate_list(id, ListMutation::Update(7, 10.)));
            let (edit, delta) = host.propose_list_reply(id).unwrap();
            assert_eq!(Some(delta), demand(&[], &[7], &[]));
            let recycle = format!(
                r#"[{{"op":"remove","id":{child}}},
                {{"op":"create","id":{child},"parent":{parent},"node":{{"kind":"text","text":"new"}}}}]"#
            );
            let batch = reply_batch(&mut host, &recycle);
            assert!(host.receive(batch));
            let base = host.tree.app_seq;
            let batch = reply_batch(&mut host, r#"[{"op":"settext","id":6,"text":"edited"}]"#);
            assert_eq!(batch.base, base);
            assert_eq!(host.accept_list_reply(edit, batch, &[]), expected);
            assert_eq!(host.bound_rows.get(&4), Some(&(2, 7)));
            if expected.is_err() {
                assert_eq!(host.tree.app_seq, base);
                assert_eq!(host.tree.text(6), Some("new"));
                assert_eq!(accepted(&mut host, id), first.serial);
            } else {
                assert_eq!(host.tree.text(6), Some("edited"));
                assert_eq!(accepted(&mut host, id), edit.serial);
            }
        }
    }

    #[test]
    fn list_host_handle_is_confined_to_issuing_host() {
        let mut a = list_host();
        let mut b = list_host();
        let foreign = a.replace_virtual_list(2, [(1, 4.)]).unwrap();
        let local = b.replace_virtual_list(2, [(1, 4.), (2, 4.)]).unwrap();
        assert!(b.set_list_viewport(local, ListOffset::Pixels(1.), 1.));
        assert_eq!(b.list_offset(local), Some(1.));

        assert_eq!(
            (
                b.list_offset(foreign),
                b.set_list_viewport(foreign, ListOffset::Tail, 1.),
                b.mutate_list(foreign, ListMutation::Remove(1)),
                b.take_list_demand(foreign),
            ),
            (None, false, false, None)
        );
        assert_eq!(b.list_offset(local), Some(1.));
        assert_eq!(b.take_list_demand(local), demand(&[1], &[], &[]));
        assert!(b.mutate_list(local, ListMutation::Update(1, 4.)));
        assert_eq!(b.take_list_demand(local), demand(&[], &[1], &[]));
        assert_eq!(a.list_offset(foreign), Some(0.));
    }

    #[test]
    fn virtual_list_host_preserves_anchor_and_delivers_only_key_deltas() {
        use ListMutation::*;
        use ListOffset::*;
        let mut host = list_host();
        assert!(host.replace_virtual_list(1, []).is_none());
        let list = host
            .replace_virtual_list(2, [(1, 2.), (2, 4.), (3, 5.)])
            .unwrap();
        assert!(host.set_list_viewport(list, Pixels(3.25), 1.));
        assert_eq!(host.take_list_demand(list), demand(&[2], &[], &[]));
        assert!(host.set_list_viewport(list, Pixels(3.5), 1.));
        assert_eq!(host.take_list_demand(list), None);
        assert!(host.set_list_viewport(list, Pixels(3.25), 1.));
        assert!(host.mutate_list(list, Append(8, 10.)));
        assert!(host.mutate_list(list, Prepend(4, 1.5)));
        assert_eq!(host.list_offset(list), Some(4.75));
        assert_eq!(host.take_list_demand(list), None);
        assert!(host.mutate_list(list, Update(1, 3.)));
        assert_eq!(host.list_offset(list), Some(5.75));
        assert_eq!(host.take_list_demand(list), None);
        assert!(host.mutate_list(list, Update(2, 4.))); // A content edit may leave height unchanged.
        assert_eq!(host.take_list_demand(list), demand(&[], &[2], &[]));
        assert!(!host.mutate_list(list, Append(2, 99.)));
        assert!(!host.mutate_list(list, Update(2, 0.)));
        assert!(!host.set_list_viewport(list, Pixels(f64::NAN), 1.));
        assert_eq!(host.list_offset(list), Some(5.75));
        assert_eq!(host.take_list_demand(list), None);
        assert!(host.mutate_list(list, Remove(2)));
        assert_eq!(host.list_offset(list), Some(4.5));
        assert_eq!(host.take_list_demand(list), demand(&[3], &[], &[2]));
        assert_eq!(host.take_list_demand(list), None);
        assert!(host.set_list_viewport(list, Tail, 2.));
        assert_eq!(host.list_offset(list), Some(17.5));
        assert_eq!(host.take_list_demand(list), demand(&[8], &[], &[3]));
        assert!(host.mutate_list(list, Append(9, 3.)));
        assert_eq!(host.list_offset(list), Some(20.5));
        assert_eq!(host.take_list_demand(list), demand(&[9], &[], &[8]));
        assert!(host.set_list_viewport(list, Pixels(20.5), 2.));
        assert!(host.mutate_list(list, Append(10, 3.)));
        assert_eq!(host.list_offset(list), Some(20.5));
        assert_eq!(host.take_list_demand(list), None);
    }

    #[test]
    fn virtual_list_host_bounds_history_demand_and_fences_replacement() {
        let mut host = list_host();
        let old = host
            .replace_virtual_list(2, (0..10_000).map(|key| (key, 1.)))
            .unwrap();
        assert!(host.set_list_viewport(old, ListOffset::Pixels(6_000.), 3.));
        assert_eq!(
            host.take_list_demand(old),
            demand(&[6000, 6001, 6002], &[], &[])
        );
        for _ in 0..3 {
            host.poll();
            host.frame(640., 560., 1., 4096);
            assert!(host.set_list_viewport(old, ListOffset::Pixels(6_000.), 3.));
            assert_eq!(host.take_list_demand(old), None);
        }
        assert!(host.set_list_viewport(old, ListOffset::Pixels(6_001.), 3.));
        assert_eq!(host.take_list_demand(old), demand(&[6003], &[], &[6000]));
        assert!(host.replace_virtual_list(2, [(1, 1.), (1, 2.)]).is_none());
        assert_eq!(host.list_offset(old), Some(6_001.));
        let new = host.replace_virtual_list(2, [(6001, 2.)]).unwrap();
        assert!(!host.mutate_list(old, ListMutation::Remove(6001)));
        assert!(!host.set_list_viewport(old, ListOffset::Tail, 1.));
        assert_eq!(host.take_list_demand(old), None);
        assert_eq!(host.list_offset(old), None);
        assert!(host.set_list_viewport(new, ListOffset::Pixels(0.), 1.));
        assert_eq!(
            host.take_list_demand(new),
            Some(ListDemand {
                entering: vec![6001],
                retired: Some(old),
                ..Default::default()
            })
        );
        let empty = host.replace_virtual_list(2, []).unwrap();
        assert_eq!(
            host.take_list_demand(empty),
            Some(ListDemand {
                retired: Some(new),
                ..Default::default()
            })
        );
        assert_eq!(host.take_list_demand(empty), None);
        assert!(host.reload(LIST_APP.into()));
        settle(&mut host);
        assert_eq!(host.take_reload_result(), Some(Ok(())));
        assert_eq!(host.list_offset(empty), None);
        let fresh = host.replace_virtual_list(2, []).unwrap();
        assert_ne!(fresh, empty);
        assert_eq!(host.take_list_demand(fresh), Some(ListDemand::default()));
        assert_eq!(host.take_list_demand(fresh), None);
    }

    #[test]
    fn legacy_list_host_keeps_tree_rows_out_of_layout_and_paint() {
        let mut host = list_host();
        let frame = host.frame(640., 560., 1., 4096);
        assert_eq!(host.tree.children(2), [3]);
        assert_eq!(host.tree.rect(2)[3], 20.);
        assert_eq!(host.tree.rect(3), [0.; 4]);
        host.pointer_move(Some([5., 5.]));
        assert!(!host.wheel(10.));
        assert!(!host.wheel_smooth(10.));
        assert!(
            !frame
                .items
                .iter()
                .any(|item| matches!(item, Item::Text { .. }))
        );
    }

    #[test]
    fn auto_virtual_list_uses_completed_hover_layout_and_is_clean_between_frames() {
        use crate::list::INDEX_VISITS;
        let mut host = column_host(true);
        add(
            &mut host,
            r#"[
                {"op":"create","id":2,"parent":1,"node":{"kind":"box","press":12,
                    "style":{"height":10},"paint":{"hover":{"height":30}}}},
                {"op":"create","id":3,"parent":1,"node":{"kind":"list","style":{"grow":1}}}
            ]"#,
        );
        let list = host
            .replace_auto_virtual_list(3, (0..10_000).map(|key| (key, 10.)))
            .unwrap();
        assert_eq!(host.take_list_demand(list), None);
        assert!(!host.set_list_viewport(list, ListOffset::Tail, 20.));
        host.pointer_move(Some([5., 5.]));
        host.frame(100., 60., 1., 4096);
        assert_eq!(host.tree.rect(3)[3], 30.);
        assert_eq!(host.take_list_demand(list), demand(&[0, 1, 2], &[], &[]));
        INDEX_VISITS.with(|n| n.set(0));
        host.frame(100., 61., 1., 4096);
        assert_eq!(host.tree.rect(3)[3], 31.);
        assert_eq!(host.take_list_demand(list), demand(&[3], &[], &[]));
        host.frame(100., 62., 1., 4096);
        assert_eq!(host.take_list_demand(list), None);
        INDEX_VISITS.with(|n| n.set(0));
        host.frame(100., 62., 1., 4096);
        assert_eq!(host.take_list_demand(list), None);
        assert_eq!(INDEX_VISITS.with(|n| n.get()), 0);
    }

    #[test]
    fn auto_virtual_list_offsets_anchor_and_wheel_can_repin_tail() {
        let mut host = column_host(true);
        add(
            &mut host,
            r#"[
                {"op":"create","id":2,"parent":1,"node":{"kind":"list","style":{"width":50,"grow":1}}},
                {"op":"create","id":3,"parent":2,"node":{"kind":"box","press":12}}
            ]"#,
        );
        let list = host
            .replace_auto_virtual_list(2, (0..10).map(|key| (key, 10.)))
            .unwrap();
        assert!(host.set_list_offset(list, ListOffset::Tail));
        assert!(host.bind_virtual_row(list, 9, 3));
        host.frame(100., 20., 1., 4096);
        assert_eq!(host.list_offset(list), Some(80.));
        assert_eq!(host.take_list_demand(list), demand(&[8, 9], &[], &[]));
        assert_eq!(host.pointer_move(Some([5., 15.])), Cursor::Pointer);
        host.pointer_move(Some([55., 15.]));
        assert!(!host.wheel(-1.));
        host.pointer_move(Some([5., 15.]));
        assert!(host.wheel(-25.));
        assert!(!host.wheel(f32::NAN));
        assert_eq!(host.take_list_demand(list), None);
        host.frame(100., 20., 1., 4096);
        assert_eq!(host.list_offset(list), Some(55.));
        assert_eq!(
            host.take_list_demand(list),
            demand(&[5, 6, 7], &[], &[8, 9])
        );
        assert_eq!(host.pointer_move(Some([5., 15.])), Cursor::Default);
        assert!(host.mutate_list(list, ListMutation::Prepend(100, 10.)));
        host.frame(100., 25., 1., 4096);
        assert_eq!(host.list_offset(list), Some(65.));
        assert_eq!(host.take_list_demand(list), None);
        assert!(host.set_list_offset(list, ListOffset::Pixels(65.5)));
        assert!(!host.set_list_offset(list, ListOffset::Pixels(f64::NAN)));
        host.frame(100., 25., 1., 4096);
        assert_eq!(host.list_offset(list), Some(65.5));
        assert_eq!(host.take_list_demand(list), demand(&[8], &[], &[]));
        assert!(host.wheel_smooth(100.));
        host.frame(100., 25., 1., 4096);
        assert_eq!(host.list_offset(list), Some(85.));
        assert_eq!(host.take_list_demand(list), demand(&[9], &[], &[5, 6]));
        assert!(host.mutate_list(list, ListMutation::Append(10, 10.)));
        assert_eq!(host.take_list_demand(list), None);
        host.frame(100., 20., 1., 4096);
        assert_eq!(host.list_offset(list), Some(100.));
        assert_eq!(host.take_list_demand(list), demand(&[10], &[], &[7, 8]));
        assert!(host.set_list_offset(list, ListOffset::Pixels(100.)));
        host.frame(100., 20., 1., 4096);
        assert_eq!(host.take_list_demand(list), None);
        assert!(host.mutate_list(list, ListMutation::Append(11, 10.)));
        host.frame(100., 20., 1., 4096);
        assert_eq!(host.list_offset(list), Some(100.));
        assert_eq!(host.take_list_demand(list), None);
    }

    #[test]
    fn virtual_list_host_frame_work_scales_with_bindings_not_visible_keys() {
        use crate::list::INDEX_VISITS;
        let mut host = list_host();
        let list = host
            .replace_virtual_list(2, (0..60_000).map(|key| (key, 0.01)))
            .unwrap();
        assert!(host.set_list_viewport(list, ListOffset::Pixels(0.), 560.));
        assert!(host.bind_virtual_row(list, 1234, 3));
        INDEX_VISITS.with(|n| n.set(0));
        let frame = host.frame(640., 560., 1., 4096);
        let visits = INDEX_VISITS.with(|n| n.get());
        assert_eq!(host.layout.visible_rows(2), [3]);
        assert!((host.tree.rect(3)[1] - 12.34).abs() < 0.001);
        assert!(matches!(&frame.items[..], [Item::Text { text, .. }] if text == "legacy row"));
        assert!(
            (1..=32).contains(&visits),
            "one binding visited {visits} index nodes"
        );
    }

    #[test]
    fn virtual_list_host_shrinking_viewport_clips_hits_before_the_next_frame() {
        let mut host = column_host(true);
        add(
            &mut host,
            r#"[
            {"op":"create","id":2,"parent":1,"node":{"kind":"list","style":{"width":100,"height":40}}},
            {"op":"create","id":3,"parent":2,"node":{"kind":"box","press":12}}
        ]"#,
        );
        let list = host.replace_virtual_list(2, [(1, 20.), (2, 20.)]).unwrap();
        assert!(host.bind_virtual_row(list, 1, 3));
        assert!(host.set_list_viewport(list, ListOffset::Pixels(0.), 40.));
        host.frame(640., 560., 1., 4096);
        assert_eq!(host.pointer_move(Some([5., 15.])), Cursor::Pointer);
        assert!(host.set_list_viewport(list, ListOffset::Pixels(0.), 10.));
        assert_eq!(host.pointer_move(Some([5., 15.])), Cursor::Default);
        assert_eq!(host.pointer_move(Some([5., 10.])), Cursor::Default);
        assert_eq!(host.pointer_move(Some([5., 5.])), Cursor::Pointer);
    }

    #[test]
    fn virtual_list_host_lays_out_and_clips_only_bound_visible_subtrees() {
        let mut host = Host::new(r#"
            function jsnirMount() {
                jsnir.batch(JSON.stringify({ops: [
                    {op:'create', id:1, node:{kind:'column'}},
                    {op:'create', id:2, parent:1, node:{kind:'list', style:{width:100, height:40}}},
                    {op:'create', id:3, parent:2, node:{kind:'box'}},
                    {op:'create', id:4, parent:3, node:{kind:'box', press:12,
                        style:{width:100, height:40, position:'fixed', top:0}, paint:{background:4294967295}}},
                    {op:'create', id:5, parent:4, node:{kind:'text', text:'rich row', spans:[{text:'rich row'}]}},
                    {op:'create', id:6, parent:2, node:{kind:'text', text:'offscreen'}},
                    {op:'create', id:7, parent:2, node:{kind:'text', text:'unbound'}}
                ]}));
            }
            function jsnirEvent(callback) {
                if (callback === 12) jsnir.batch(JSON.stringify({ops:[{op:'settext', id:7, text:'clicked'}]}));
            }
        "#.into(), "{}".into(), TextEngine::default());
        let id = host
            .replace_virtual_list(2, (0..10_000).map(|key| (key, 40.)))
            .unwrap();
        host.set_list_viewport(id, ListOffset::Pixels(240_010.), 20.);
        let frame = render_at(&mut host, 0.);
        assert!(frame.items.is_empty());
        assert!(host.bind_virtual_row(id, 6000, 3));
        assert!(host.bind_virtual_row(id, 6010, 6));
        assert!(!host.bind_virtual_row(id, 6000, 7));
        assert!(!host.bind_virtual_row(id, 6001, 3));
        assert!(!host.bind_virtual_row(id, 10000, 7));
        assert!(!host.bind_virtual_row(id, 6001, 5));
        let frame = render_at(&mut host, 0.);
        assert_eq!(host.tree.rect(3), [0., -10., 100., 40.]);
        assert!(host.tree.rect(5)[3] > 0.);
        assert_eq!(host.tree.rect(6), [0.; 4]);
        assert_eq!(host.tree.rect(7), [0.; 4]);
        assert_eq!(host.layout.visible_rows(2), [3]);
        assert_eq!(frame.items.len(), 2);
        assert!(
            matches!(&frame.items[1], Item::Text { spans: Some(spans), clip, .. }
            if spans[0].text == "rich row" && *clip == [0., 0., 100., 20.])
        );
        assert_eq!(host.pointer_move(Some([5., 5.])), Cursor::Pointer);
        for point in [[5., 20.], [101., 5.], [5., -1.]] {
            assert_eq!(host.pointer_move(Some(point)), Cursor::Default);
            host.pointer_down(PointSelection::Place);
            host.pointer_up();
        }
        assert!(!host.busy());
        host.pointer_move(Some([5., 5.]));
        host.pointer_down(PointSelection::Place);
        host.pointer_up();
        settle(&mut host);
        assert!(texts(&host).contains(&"clicked".to_owned()));
        // Even a large requested extent cannot lay out the prefetched row outside the list box.
        host.set_list_viewport(id, ListOffset::Pixels(240_015.), 100_000.);
        render_at(&mut host, 0.);
        assert_eq!(host.tree.rect(3)[1], -15.);
        assert_eq!(host.tree.rect(6), [0.; 4]);
        assert_eq!(host.hit([5., 5.]), Some(4));
        assert_eq!(host.hit([5., 40.]), None);
        host.set_list_viewport(id, ListOffset::Pixels(240_040.), 20.);
        assert_eq!(host.hit([5., 5.]), None);
        assert!(render_at(&mut host, 0.).items.is_empty());
        host.set_list_viewport(id, ListOffset::Pixels(240_015.), 20.);
        render_at(&mut host, 0.);
        assert!(host.unbind_virtual_row(id, 6000));
        assert_eq!(host.hit([5., 5.]), None);
        assert!(render_at(&mut host, 0.).items.is_empty());
        assert!(host.bind_virtual_row(id, 6000, 3));
        render_at(&mut host, 0.);
        let next = host.replace_virtual_list(2, [(6000, 40.)]).unwrap();
        assert!(!host.bind_virtual_row(id, 6000, 3));
        host.set_list_viewport(next, ListOffset::Pixels(0.), 20.);
        assert_eq!(host.hit([5., 5.]), None);
        assert!(render_at(&mut host, 0.).items.is_empty());
        assert!(host.bind_virtual_row(next, 6000, 3));
        render_at(&mut host, 0.);
        assert!(host.mutate_list(next, ListMutation::Remove(6000)));
        assert!(host.mutate_list(next, ListMutation::Append(6000, 40.)));
        assert_eq!(host.hit([5., 5.]), None);
        assert!(render_at(&mut host, 0.).items.is_empty());
        assert!(host.bind_virtual_row(next, 6000, 3));
        render_at(&mut host, 0.);
        add(&mut host, r#"[{"op":"remove","id":3}]"#);
        assert_eq!(host.hit([5., 5.]), None);
        assert!(render_at(&mut host, 0.).items.is_empty());
    }

    // A host whose app mounts a root column (id 1); tests add nodes with batches.
    fn column_host(reduced_motion: bool) -> Host {
        let mut host = Host::new(
            "function jsnirMount() { jsnir.batch(JSON.stringify({ ops: [{ op: 'create', id: 1, node: { kind: 'column' } }] })); }".into(),
            "{}".into(),
            TextEngine::default(),
        );
        host.set_reduced_motion(reduced_motion);
        host
    }
    fn add(host: &mut Host, ops: &str) {
        let base = host.tree.app_seq;
        let ops = serde_json::from_str(ops).unwrap();
        host.receive(Batch { base, ops });
    }
    fn render_at(host: &mut Host, now: f64) -> Frame {
        host.render(640., 560., 1., 4096, now).0
    }
    fn layers(frame: &Frame) -> Vec<f32> {
        frame
            .items
            .iter()
            .filter_map(|item| match item {
                Item::PushLayer { opacity } => Some(*opacity),
                _ => None,
            })
            .collect()
    }
    // A 100 × 20 box that pulses its opacity between 1 and 0.5 every 3 s, as the skeletons do.
    const PULSE: &str = r#"{"kind":"box","style":{"width":100,"height":20},"paint":{"animations":[{"property":"opacity","keyframes":[{"offset":0,"value":1},{"offset":0.5,"value":0.5},{"offset":1,"value":1}],"duration":3000,"easing":"easeInOut"}]}}"#;

    #[test]
    fn a_loop_keeps_the_window_drawing_only_while_it_is_visible() {
        let mut shown = column_host(false);
        add(
            &mut shown,
            &format!(r#"[{{"op":"create","id":2,"parent":1,"node":{PULSE}}}]"#),
        );
        assert!(render_at(&mut shown, 0.).animating);

        // Under an opacity-0 ancestor the loop is not walked, so nothing asks for frames.
        let mut hidden = column_host(false);
        add(
            &mut hidden,
            &format!(
                r#"[{{"op":"create","id":2,"parent":1,"node":{{"kind":"box","paint":{{"opacity":0}}}}}},
                    {{"op":"create","id":3,"parent":2,"node":{PULSE}}}]"#
            ),
        );
        assert!(!render_at(&mut hidden, 0.).animating);

        // A removed node is not walked either.
        add(&mut shown, r#"[{"op":"remove","id":2}]"#);
        assert!(!render_at(&mut shown, 0.).animating);
    }

    #[test]
    fn reduced_motion_shows_the_base_value_and_stops_the_loop() {
        let mut calm = column_host(true);
        add(
            &mut calm,
            &format!(r#"[{{"op":"create","id":2,"parent":1,"node":{PULSE}}}]"#),
        );
        render_at(&mut calm, 0.);
        // Halfway through the pulse the base opacity is 1: no layer, and nothing keeps drawing.
        let frame = render_at(&mut calm, 750.);
        assert!(layers(&frame).is_empty());
        assert!(!frame.animating);

        let mut moving = column_host(false);
        add(
            &mut moving,
            &format!(r#"[{{"op":"create","id":2,"parent":1,"node":{PULSE}}}]"#),
        );
        render_at(&mut moving, 0.);
        let frame = render_at(&mut moving, 750.);
        assert!((layers(&frame)[0] - 0.75).abs() < 1e-3);
        assert!(frame.animating);
    }

    #[test]
    fn a_finite_animation_ends_on_the_base_value() {
        let mut host = column_host(false);
        add(
            &mut host,
            r#"[{"op":"create","id":2,"parent":1,"node":{"kind":"box","style":{"width":100,"height":20},
                "paint":{"animations":[{"property":"opacity","keyframes":[{"offset":0,"value":0.5},{"offset":1,"value":1}],
                "duration":1000,"easing":"linear","iterations":1}]}}}]"#,
        );
        render_at(&mut host, 0.);
        let frame = render_at(&mut host, 500.);
        assert!((layers(&frame)[0] - 0.75).abs() < 1e-3);
        assert!(frame.animating);
        let frame = render_at(&mut host, 1000.);
        assert!(layers(&frame).is_empty(), "the base opacity 1 is back");
        assert!(!frame.animating);
    }

    // The chat of the scrollchat app: `rows` rows in a 300 px box anchored at its end.
    fn chat(rows: u32) -> Host {
        Host::new(
            crate::apps::SCROLLCHAT.into(),
            format!(r#"{{"rows":{rows}}}"#),
            TextEngine::default(),
        )
    }

    // The chat's box, and its largest offset.
    fn chat_box(host: &Host) -> (u16, f32) {
        let scroll = find(host, |n| n.style().is_some_and(|s| s.scroll));
        (
            scroll,
            host.layout.content_height(scroll) - host.tree.rect(scroll)[3],
        )
    }

    // Where a row is in the box on screen, relative to the box's top: its content position less the offset.
    fn row_in_view(host: &Host, scroll: u16, label: &str) -> f32 {
        host.tree.rect(text_id(host, label))[1] - host.scroll[&scroll]
    }

    #[test]
    fn a_chat_box_anchored_at_its_end_starts_at_the_bottom() {
        let mut host = chat(50);
        settle(&mut host);
        let (scroll, max) = chat_box(&host);
        assert!(max > 0., "the rows overflow the box");
        assert_eq!(host.scroll[&scroll], max);
    }

    #[test]
    fn rows_added_while_at_the_bottom_keep_the_bottom_in_view() {
        let mut host = chat(50);
        settle(&mut host);
        host.push(r#"{"rows":53}"#.into());
        settle(&mut host);
        let (scroll, max) = chat_box(&host);
        assert_eq!(host.scroll[&scroll], max);
        assert!(texts(&host).contains(&"row 52".into()));
    }

    #[test]
    fn a_chat_scrolled_up_keeps_its_rows_still_when_rows_come_in_below_or_above() {
        let mut host = chat(50);
        settle(&mut host);
        let (scroll, _) = chat_box(&host);
        let [x, top, _, _] = host.tree.rect(scroll);
        host.pointer_move(Some([x + 4., top + 5.]));
        assert!(host.wheel(-200.));
        settle(&mut host);
        let (_, max) = chat_box(&host);
        let before = row_in_view(&host, scroll, "row 30");
        assert!(host.scroll[&scroll] < max - 150., "scrolled up");

        // Rows added below the view: the view does not move.
        host.push(r#"{"rows":53}"#.into());
        settle(&mut host);
        assert_eq!(row_in_view(&host, scroll, "row 30"), before);

        // Content added above the rows: the view keeps them where they were.
        host.push(r#"{"above":100}"#.into());
        settle(&mut host);
        assert!(
            (row_in_view(&host, scroll, "row 30") - before).abs() < 0.01,
            "{}",
            row_in_view(&host, scroll, "row 30")
        );
    }

    #[test]
    fn scroll_to_moves_a_box_to_an_edge_or_an_offset_at_once_or_as_a_glide() {
        let mut host = chat(50);
        settle(&mut host);
        let (scroll, max) = chat_box(&host);
        host.push(r#"{"to":"start"}"#.into());
        settle(&mut host);
        assert_eq!(host.scroll[&scroll], 0.);
        host.push(r#"{"offset":100}"#.into());
        settle(&mut host);
        assert_eq!(host.scroll[&scroll], 100.);
        host.push(r#"{"to":"end","smooth":true}"#.into());
        settle(&mut host);
        assert_eq!(host.scroll[&scroll], max);
        // After the glide the box is still anchored at the bottom.
        host.push(r#"{"rows":53}"#.into());
        settle(&mut host);
        let (_, max) = chat_box(&host);
        assert_eq!(host.scroll[&scroll], max);
    }

    #[test]
    fn a_scroll_box_calls_js_once_per_frame_and_its_columns_hold_the_offset_there() {
        let mut host = chat(50);
        settle(&mut host);
        let (scroll, _) = chat_box(&host);
        let [x, top, _, _] = host.tree.rect(scroll);
        host.pointer_move(Some([x + 4., top + 5.]));
        let calls = |host: &Host| -> u32 {
            let line = texts(host)
                .into_iter()
                .find(|t| t.starts_with("calls "))
                .expect("the status line");
            line.split(' ').nth(1).unwrap().parse().unwrap()
        };
        let before = calls(&host);
        assert!(host.wheel(-30.));
        assert!(host.wheel(-30.));
        settle(&mut host);
        assert_eq!(
            calls(&host),
            before + 1,
            "two steps before a frame make one call"
        );
        let offset = host.scroll[&scroll];
        let status = texts(&host)
            .into_iter()
            .find(|t| t.starts_with("calls "))
            .unwrap();
        assert!(
            status.ends_with(&format!("columns {}", offset as i32)),
            "{status} against {offset}"
        );
    }

    #[test]
    fn dragging_a_resize_handle_delivers_start_move_end_and_resizes_a_pane() {
        let script = r#"
            var log = [];
            var base = 100;
            function jsnirMount() {
                jsnir.batch(JSON.stringify({ ops: [
                    { op: 'create', id: 1, node: { kind: 'column', style: { width: 400, height: 200 } } },
                    { op: 'create', id: 5, parent: 1, node: { kind: 'box', style: { direction: 'row', width: 400, height: 100 } } },
                    { op: 'create', id: 2, parent: 5, node: { kind: 'box', style: { width: 100, height: 100, shrink: 0 } } },
                    { op: 'create', id: 3, parent: 5, node: { kind: 'box', style: { width: 6, height: 100, shrink: 0 },
                        paint: { cursor: 'ewResize' }, drag: 1 } },
                    { op: 'create', id: 4, parent: 1, node: { kind: 'text', text: '[]' } },
                ] }));
            }
            function jsnirEvent(cb, text) {
                if (cb !== 1) return;
                var e = JSON.parse(text);
                log.push(e);
                if (e.phase === 'start') {
                    base = jsnir.columns.width[2];
                } else if (e.phase === 'move') {
                    var w = Math.max(40, Math.min(300, base + e.dx));
                    jsnir.batch(JSON.stringify({ ops: [{ op: 'setstyle', id: 2, style: { width: w, height: 100, shrink: 0 } }] }));
                }
                jsnir.setText(4, JSON.stringify(log));
            }
        "#;
        let mut host = Host::new(script.into(), "{}".into(), TextEngine::default());
        settle(&mut host);

        assert_eq!(host.pointer_move(Some([50., 50.])), Cursor::Default);
        assert_eq!(host.pointer_move(Some([103., 50.])), Cursor::EwResize);

        host.pointer_down(PointSelection::Place);
        assert_eq!(host.cursor(), Cursor::EwResize);
        settle(&mut host);

        // Move far outside the 6 px handle: cursor stays locked and pane resizes.
        assert_eq!(host.pointer_move(Some([180., 75.])), Cursor::EwResize);
        settle(&mut host);
        assert_eq!(host.tree.rect(2)[2], 177.);

        // Leaving the window mid-drag keeps the drag alive until release.
        assert_eq!(host.pointer_move(None), Cursor::EwResize);
        host.pointer_up();
        assert_eq!(host.cursor(), Cursor::Default);
        settle(&mut host);

        let log: Vec<serde_json::Value> = serde_json::from_str(&texts(&host)[0]).unwrap();
        assert_eq!(log.len(), 3, "{log:?}");
        assert_eq!(
            log[0],
            serde_json::json!({"phase":"start","x":103,"y":50,"startX":103,"startY":50,"dx":0,"dy":0})
        );
        assert_eq!(
            log[1],
            serde_json::json!({"phase":"move","x":180,"y":75,"startX":103,"startY":50,"dx":77,"dy":25})
        );
        assert_eq!(
            log[2],
            serde_json::json!({"phase":"end","x":180,"y":75,"startX":103,"startY":50,"dx":77,"dy":25})
        );
    }

    #[test]
    fn rapid_drag_moves_coalesce_and_deliver_the_final_position_before_end() {
        let script = r#"
            var log = [];
            function jsnirMount() {
                jsnir.batch(JSON.stringify({ ops: [
                    { op: 'create', id: 1, node: { kind: 'box', style: { width: 100, height: 100 }, drag: 1 } },
                    { op: 'create', id: 2, parent: 1, node: { kind: 'text', text: '[]' } },
                ] }));
            }
            function jsnirEvent(cb, text) {
                if (cb !== 1) return;
                log.push(JSON.parse(text));
                jsnir.setText(2, JSON.stringify(log));
            }
        "#;
        let mut host = Host::new(script.into(), "{}".into(), TextEngine::default());
        settle(&mut host);

        host.pointer_move(Some([10., 10.]));
        host.pointer_down(PointSelection::Place);
        // Many rapid moves without polling in between: only the first goes in flight immediately,
        // and the rest coalesce down to the latest position before `end`.
        for step in 1..=20 {
            host.pointer_move(Some([10. + step as f32 * 5., 10.]));
        }
        host.pointer_up();
        settle(&mut host);

        let log: Vec<serde_json::Value> = serde_json::from_str(&texts(&host)[0]).unwrap();
        assert_eq!(log.first().unwrap()["phase"], "start");
        assert_eq!(log.last().unwrap()["phase"], "end");
        assert_eq!(log.last().unwrap()["x"], 110.0);
        assert_eq!(log.last().unwrap()["dx"], 100.0);
        let before_end = &log[log.len() - 2];
        assert_eq!(before_end["phase"], "move");
        assert_eq!(before_end["x"], 110.0);
        assert!(log.len() <= 4, "expected coalescing, got {} events", log.len());
    }

    #[test]
    fn drag_and_press_on_the_same_box_clicks_without_motion_and_suppresses_click_after_drag() {
        let script = r#"
            var log = [];
            function jsnirMount() {
                jsnir.batch(JSON.stringify({ ops: [
                    { op: 'create', id: 1, node: { kind: 'box', style: { width: 100, height: 100 }, press: 1, drag: 2 } },
                    { op: 'create', id: 2, parent: 1, node: { kind: 'text', text: '[]' } },
                ] }));
            }
            function jsnirEvent(cb, text) {
                log.push(cb === 1 ? 'press' : JSON.parse(text).phase);
                jsnir.setText(2, JSON.stringify(log));
            }
        "#;
        let mut host = Host::new(script.into(), "{}".into(), TextEngine::default());
        settle(&mut host);

        // Press and release without moving: drag start+end plus press.
        host.pointer_move(Some([20., 20.]));
        host.pointer_down(PointSelection::Place);
        host.pointer_up();
        settle(&mut host);
        assert_eq!(texts(&host)[0], r#"["start","end","press"]"#);

        // Press, drag away, return inside the box, release: press is suppressed.
        host.pointer_down(PointSelection::Place);
        host.pointer_move(Some([50., 20.]));
        host.pointer_move(Some([20., 20.]));
        host.pointer_up();
        settle(&mut host);
        let log: Vec<String> = serde_json::from_str(&texts(&host)[0]).unwrap();
        assert_eq!(
            log.iter().filter(|e| e.as_str() == "press").count(),
            1,
            "{log:?}"
        );
        assert_eq!(log.last().unwrap(), "end");
    }

    #[test]
    fn disabled_cancelled_and_removed_drags_end_cleanly() {
        let script = r#"
            var log = [];
            function jsnirMount() {
                jsnir.batch(JSON.stringify({ ops: [
                    { op: 'create', id: 1, node: { kind: 'column', style: { width: 200, height: 200 } } },
                    { op: 'create', id: 2, parent: 1, node: { kind: 'box', style: { width: 100, height: 40 },
                        paint: { cursor: 'ewResize' }, drag: 1 } },
                    { op: 'create', id: 3, parent: 1, node: { kind: 'box', style: { width: 100, height: 40 },
                        paint: { cursor: 'ewResize' }, drag: 2, disabled: true } },
                    { op: 'create', id: 4, parent: 1, node: { kind: 'text', text: '[]' } },
                ] }));
            }
            function jsnirEvent(cb, text) {
                if (cb === 0) {
                    var msg = JSON.parse(text);
                    jsnir.batch(JSON.stringify({ ops: msg.ops }));
                    return;
                }
                log.push([cb, JSON.parse(text).phase]);
                jsnir.setText(4, JSON.stringify(log));
            }
        "#;
        let mut host = Host::new(script.into(), "{}".into(), TextEngine::default());
        settle(&mut host);

        // Disabled box rejects drag and shows NotAllowed.
        assert_eq!(host.pointer_move(Some([20., 60.])), Cursor::NotAllowed);
        host.pointer_down(PointSelection::Place);
        host.pointer_move(Some([60., 60.]));
        host.pointer_up();
        settle(&mut host);
        assert_eq!(texts(&host)[0], "[]");

        // pointer_cancel sends "end" and releases cursor lock.
        assert_eq!(host.pointer_move(Some([20., 20.])), Cursor::EwResize);
        host.pointer_down(PointSelection::Place);
        host.pointer_move(Some([80., 20.]));
        host.pointer_cancel();
        assert_eq!(host.cursor(), Cursor::EwResize);
        settle(&mut host);
        assert_eq!(texts(&host)[0], r#"[[1,"start"],[1,"move"],[1,"end"]]"#);

        // Disabling mid-drag sends "end" and stops further moves.
        host.pointer_move(Some([20., 20.]));
        host.pointer_down(PointSelection::Place);
        settle(&mut host);
        host.push(r#"{"ops":[{"op":"setdisabled","id":2,"disabled":true}]}"#.into());
        settle(&mut host);
        host.pointer_move(Some([90., 20.]));
        host.pointer_up();
        settle(&mut host);
        assert_eq!(
            texts(&host)[0],
            r#"[[1,"start"],[1,"move"],[1,"end"],[1,"start"],[1,"end"]]"#
        );

        // Re-enable, start dragging, then remove mid-drag: no further moves or crashes.
        host.push(r#"{"ops":[{"op":"setdisabled","id":2,"disabled":false}]}"#.into());
        settle(&mut host);
        host.pointer_move(Some([20., 20.]));
        host.pointer_down(PointSelection::Place);
        settle(&mut host);
        host.push(r#"{"ops":[{"op":"remove","id":2}]}"#.into());
        settle(&mut host);
        // After removing id 2, the disabled id 3 shifted up into y 0..40; moving outside it yields Default.
        assert_eq!(host.pointer_move(Some([50., 20.])), Cursor::NotAllowed);
        assert_eq!(host.pointer_move(Some([150., 20.])), Cursor::Default);
        host.pointer_up();
        settle(&mut host);
        assert_eq!(
            texts(&host)[0],
            r#"[[1,"start"],[1,"move"],[1,"end"],[1,"start"],[1,"end"],[1,"start"]]"#
        );
    }

    #[test]
    fn drag_respects_layers_contextmenu_children_and_variant_cursors() {
        let script = r#"
            var log = [];
            function jsnirMount() {
                jsnir.batch(JSON.stringify({ ops: [
                    { op: 'create', id: 1, node: { kind: 'box', style: { width: 200, height: 200 } } },
                    { op: 'create', id: 2, parent: 1, node: { kind: 'box', style: { width: 120, height: 80 },
                        paint: { cursor: 'grab', active: { cursor: 'grabbing' } }, drag: 1 } },
                    { op: 'create', id: 3, parent: 2, node: { kind: 'box', style: { width: 60, height: 40 },
                        contextmenu: 2 } },
                    { op: 'create', id: 4, parent: 1, node: { kind: 'box',
                        style: { position: 'fixed', left: 80, top: 0, width: 80, height: 80, layer: 1 } } },
                    { op: 'create', id: 5, parent: 1, node: { kind: 'box', style: { width: 100, height: 40 }, press: 3 } },
                    { op: 'create', id: 6, parent: 5, node: { kind: 'box', style: { width: 40, height: 20 },
                        paint: { cursor: 'nsResize' } } },
                    { op: 'create', id: 7, parent: 1, node: { kind: 'text', text: '[]' } },
                ] }));
            }
            function jsnirEvent(cb, text) {
                log.push([cb, text]);
                jsnir.setText(7, JSON.stringify(log));
            }
        "#;
        let mut host = Host::new(script.into(), "{}".into(), TextEngine::default());
        settle(&mut host);

        // Hovering over the contextmenu-only child inside the draggable box shows the parent's grab cursor,
        // and dragging starts on the parent with the active grabbing cursor.
        assert_eq!(host.pointer_move(Some([20., 20.])), Cursor::Grab);
        host.pointer_down(PointSelection::Place);
        assert_eq!(host.cursor(), Cursor::Grabbing);
        assert_eq!(host.pointer_move(Some([50., 30.])), Cursor::Grabbing);
        host.pointer_up();
        assert_eq!(host.cursor(), Cursor::Grab);
        settle(&mut host);
        let log: Vec<(u16, String)> = serde_json::from_str(&texts(&host)[0]).unwrap();
        assert_eq!(log.len(), 3, "{log:?}");
        assert!(log.iter().all(|(cb, _)| *cb == 1));

        // The higher layer (id 4, x 80..160) covers the right part of the draggable box (id 2, x 0..120).
        assert_eq!(host.pointer_move(Some([100., 20.])), Cursor::Default);
        host.pointer_down(PointSelection::Place);
        host.pointer_move(Some([110., 20.]));
        host.pointer_up();
        settle(&mut host);
        let log: Vec<(u16, String)> = serde_json::from_str(&texts(&host)[0]).unwrap();
        assert_eq!(log.len(), 3, "covered region must not start a drag");

        // Nested cursor override inside a pressable box (id 5 at y 80..120, child id 6 at x 0..40, y 80..100).
        assert_eq!(host.pointer_move(Some([10., 90.])), Cursor::NsResize);
        assert_eq!(host.pointer_move(Some([60., 90.])), Cursor::Pointer);
    }

    #[test]
    fn sdk_on_drag_resizes_a_box_through_tsx() {
        struct Scratch(std::path::PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let out = std::process::Command::new("mktemp")
            .arg("-d")
            .output()
            .unwrap();
        assert!(out.status.success());
        let dir = Scratch(String::from_utf8(out.stdout).unwrap().trim().into());
        let app = dir.0.join("drag.tsx");
        std::fs::write(
            &app,
            r#"
function ResizeApp(): Element {
    var width = signal(120);
    var status = signal("idle");
    var base = 120;
    return (
        <Column style={{ width: 400, height: 200 }}>
            <Row style={{ width: 400, height: 100 }}>
                <Box style={() => ({ width: width.value, height: 100, shrink: 0 })} />
                <Box
                    style={{ width: 6, height: 100, shrink: 0 }}
                    paint={{ cursor: "colResize" }}
                    onDrag={(e: DragEvent) => {
                        if (e.phase === "start") {
                            base = width.value;
                            status.value = "dragging";
                        } else if (e.phase === "move") {
                            width.value = Math.max(60, Math.min(300, base + e.dx));
                        } else {
                            status.value = "done " + width.value;
                        }
                    }}
                />
            </Row>
            <Text>{status}</Text>
        </Column>
    );
}
mount(ResizeApp);
"#,
        )
        .unwrap();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        std::fs::copy(root.join("app/jsnir.d.ts"), dir.0.join("jsnir.d.ts")).unwrap();
        std::fs::copy(root.join("app/sdk.ts"), dir.0.join("sdk.ts")).unwrap();
        let out_dir = dir.0.join("out");
        let output = std::process::Command::new(crate::typescript::compiler(root))
            .current_dir(&dir.0)
            .args([
                "--ignoreConfig",
                "--strict",
                "--target",
                "es2020",
                "--lib",
                "es2020",
                "--types",
                "./jsnir.d.ts",
                "--jsx",
                "react",
                "--jsxFactory",
                "h",
                "sdk.ts",
                "drag.tsx",
                "--outDir",
            ])
            .arg(&out_dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let script = std::fs::read_to_string(out_dir.join("sdk.js")).unwrap()
            + &std::fs::read_to_string(out_dir.join("drag.js")).unwrap();
        let mut host = Host::new(script, "{}".into(), TextEngine::default());
        settle(&mut host);

        assert_eq!(host.pointer_move(Some([123., 50.])), Cursor::ColResize);
        host.pointer_down(PointSelection::Place);
        settle(&mut host);
        assert!(texts(&host).contains(&"dragging".into()));

        assert_eq!(host.pointer_move(Some([203., 50.])), Cursor::ColResize);
        host.pointer_up();
        settle(&mut host);
        assert!(texts(&host).contains(&"done 200".into()), "{:?}", texts(&host));
    }

    #[test]
    fn dev_overlay_hard_reload_and_keyed_signals_work_together() {
        let sdk = include_str!("../app/sdk.ts");
        let _ = sdk;
        // Build a minimal script using keyed reload state to verify soft vs hard reload + DevError overlay.
        let v1 = r#"
            var saved = typeof __jsnirPreviousState === 'string' ? JSON.parse(__jsnirPreviousState) : [];
            var byKey = {};
            for (var i = 0; i < saved.length; i++) if (saved[i].key) byKey[saved[i].key] = saved[i];
            var count = byKey['count'] && byKey['count'].initial === '0' ? byKey['count'].value : 0;
            function jsnirReloadState() { return JSON.stringify([{ key: 'count', initial: '0', value: count }]); }
            function jsnirMount() {
                jsnir.batch(JSON.stringify({ ops: [
                    { op: 'create', id: 1, node: { kind: 'box', style: { width: 200, height: 80 }, press: 1 } },
                    { op: 'create', id: 2, parent: 1, node: { kind: 'text', text: 'v1:' + count } },
                ] }));
            }
            function jsnirEvent(cb) {
                if (cb === 1) { count += 1; jsnir.setText(2, 'v1:' + count); }
            }
        "#;
        let v2 = r#"
            var saved = typeof __jsnirPreviousState === 'string' ? JSON.parse(__jsnirPreviousState) : [];
            var byKey = {};
            for (var i = 0; i < saved.length; i++) if (saved[i].key) byKey[saved[i].key] = saved[i];
            var extra = byKey['extra'] && byKey['extra'].initial === '9' ? byKey['extra'].value : 9;
            var count = byKey['count'] && byKey['count'].initial === '0' ? byKey['count'].value : 0;
            function jsnirReloadState() { return JSON.stringify([
                { key: 'extra', initial: '9', value: extra },
                { key: 'count', initial: '0', value: count },
            ]); }
            function jsnirMount() {
                jsnir.batch(JSON.stringify({ ops: [
                    { op: 'create', id: 1, node: { kind: 'box', style: { width: 200, height: 80 }, press: 1 } },
                    { op: 'create', id: 2, parent: 1, node: { kind: 'text', text: 'v2:' + extra + ':' + count } },
                ] }));
            }
            function jsnirEvent(cb) {
                if (cb === 1) { count += 1; jsnir.setText(2, 'v2:' + extra + ':' + count); }
            }
        "#;

        let mut host = Host::new(v1.into(), "{}".into(), TextEngine::default());
        host.enable_dev_overlay(true);
        settle(&mut host);
        assert!(texts(&host).contains(&"v1:0".into()));

        // Click twice -> count = 2.
        for _ in 0..2 {
            host.pointer_move(Some([20., 20.]));
            host.pointer_down(PointSelection::Place);
            host.pointer_up();
            settle(&mut host);
        }
        assert!(texts(&host).contains(&"v1:2".into()));

        // Failed reload populates the native DevError overlay and keeps v1 alive.
        assert!(host.reload("function jsnirMount() { throw new Error('boom'); }".into()));
        settle(&mut host);
        assert!(host.dev_error().is_some());
        let frame = host.frame(640., 480., 1., 4096);
        assert!(
            frame
                .items
                .iter()
                .any(|item| matches!(item, Item::Text { text, .. } if text.contains("boom"))),
            "overlay must draw the error text"
        );
        // Escape dismisses the error overlay.
        host.escape();
        assert!(host.dev_error().is_none());

        // Set a synthetic compile error, then soft-reload v2: keyed signal preserves count=2 AND clears overlay!
        host.set_dev_error(Some(DevError {
            title: "Syntax error".into(),
            message: "Unexpected token".into(),
            location: Some("src/app.tsx:4:10".into()),
            frame: Some("> 4 | const x = ;".into()),
        }));
        assert!(host.reload(v2.into()));
        settle(&mut host);
        assert!(host.dev_error().is_none());
        assert!(texts(&host).contains(&"v2:9:2".into()), "{:?}", texts(&host));

        // Hard reload resets state back to count=0.
        assert!(host.hard_reload(v2.into()));
        settle(&mut host);
        assert!(texts(&host).contains(&"v2:9:0".into()), "{:?}", texts(&host));
    }

    #[test]
    fn bounded_wait_lands_fast_reply_in_first_poll_and_never_stalls_slow_handler() {
        let fast_script = r#"
            var n = 0;
            function jsnirMount() {
                jsnir.batch(JSON.stringify({ ops: [
                    { op: 'create', id: 1, node: { kind: 'box', style: { width: 100, height: 40 }, press: 1 } },
                    { op: 'create', id: 2, parent: 1, node: { kind: 'text', text: '0' } },
                ] }));
            }
            function jsnirEvent(cb) {
                if (cb === 1) { n += 1; jsnir.setText(2, '' + n); }
            }
        "#;
        let mut host = Host::new(fast_script.into(), "{}".into(), TextEngine::default());
        host.set_wait_budget(Duration::from_millis(5));
        settle(&mut host);
        // Warm up the VM thread, then assert every click lands on the very first `poll()`.
        host.pointer_move(Some([20., 20.]));
        for expected in 1..=20 {
            host.pointer_down(PointSelection::Place);
            host.pointer_up();
            host.poll();
            assert!(!host.busy(), "fast click #{expected} should land in the same poll");
            assert_eq!(host.tree.text(2), Some(expected.to_string().as_str()));
        }

        // Slow request handler: bounded wait returns promptly and subsequent polls don't block at all.
        let slow_script = r#"
            function jsnirMount() {
                jsnir.batch(JSON.stringify({ ops: [
                    { op: 'create', id: 1, node: { kind: 'box', style: { width: 100, height: 40 }, press: 1 } },
                    { op: 'create', id: 2, parent: 1, node: { kind: 'text', text: 'idle' } },
                ] }));
            }
            function jsnirEvent(cb) {
                if (cb === 1) {
                    var res = jsnirRequest('slow');
                    jsnir.setText(2, res);
                }
            }
        "#;
        let mut slow = Host::with_request_handler(
            slow_script.into(),
            "{}".into(),
            TextEngine::default(),
            |_| {
                std::thread::sleep(Duration::from_millis(120));
                Ok("done".into())
            },
        )
        .unwrap();
        slow.set_wait_budget(Duration::from_micros(800));
        settle(&mut slow);
        slow.pointer_move(Some([20., 20.]));
        slow.pointer_down(PointSelection::Place);
        slow.pointer_up();
        let t0 = Instant::now();
        slow.poll();
        assert!(
            slow.busy() && t0.elapsed() < Duration::from_millis(40),
            "first poll waited {:?}",
            t0.elapsed()
        );
        let t1 = Instant::now();
        slow.poll();
        assert!(
            t1.elapsed() < Duration::from_millis(5),
            "second poll past budget waited {:?}",
            t1.elapsed()
        );
        settle(&mut slow);
        assert_eq!(slow.tree.text(2), Some("done"));
    }

    #[test]
    fn password_input_disables_ime_masks_unfocused_text_and_blocks_copy() {
        let script = r#"
            function jsnirMount() {
                jsnir.batch(JSON.stringify({ ops: [
                    { op: 'create', id: 1, node: { kind: 'column', style: { width: 300, height: 120 } } },
                    { op: 'create', id: 2, parent: 1, node: { kind: 'input', text: 'hunter2', purpose: 'password', style: { width: 200, height: 32 } } },
                    { op: 'create', id: 3, parent: 1, node: { kind: 'input', text: 'hello 茶', purpose: 'search', style: { width: 200, height: 32 } } },
                ] }));
            }
        "#;
        let mut host = Host::new(script.into(), "{}".into(), TextEngine::default());
        settle(&mut host);

        // Unfocused password input draws masked bullets, never plaintext.
        let frame = host.frame(300., 120., 1., 4096);
        assert!(
            frame
                .items
                .iter()
                .any(|item| matches!(item, Item::Text { text, .. } if text == "•••••••")),
            "expected masked password bullets"
        );
        assert!(
            !frame
                .items
                .iter()
                .any(|item| matches!(item, Item::Text { text, .. } if text == "hunter2")),
            "password plaintext must never be drawn"
        );

        // Focusing password field sets purpose=Password, disables IME, omits ime_area, and blocks Copy.
        host.focus(Some(2));
        let state = host.ime_state();
        assert!(!state.allowed);
        assert_eq!(state.purpose, InputPurpose::Password);
        assert!(state.surrounding.is_none());
        let focused_frame = host.frame(300., 120., 1., 4096);
        assert!(focused_frame.ime_area.is_none());
        let out = host.edit(&[EditorEvent::SelectAll, EditorEvent::Copy]);
        assert!(out.clipboard.is_none());

        // Focusing search field enables IME, exposes surrounding text, and sets ime_area.
        host.focus(Some(3));
        let state = host.ime_state();
        assert!(state.allowed);
        assert_eq!(state.purpose, InputPurpose::Search);
        let surrounding = state.surrounding.expect("surrounding text");
        assert_eq!(surrounding.text, "hello 茶");
        let search_frame = host.frame(300., 120., 1., 4096);
        assert!(search_frame.ime_area.is_some());
    }
}
