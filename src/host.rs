//! Runs any TSX app: the app thread sends batches, and this UI-side host keeps the tree, lays it out,
//! handles pointer, focus and text input, runs transitions and timers, and turns the tree into a list
//! of drawing items that any backend can paint. JS never runs on this thread.
use crate::{
    App, Bridge, Event, HOST_CALLBACK, Inbox, Log, MAX_PENDING, Vm,
    editor::{EditorBitmapKey, EditorEvent, EditorFrame, EditorOutput, LineEditor, PointSelection},
    layout::Layout,
    motion::{Channel, Motion, Value},
    or_log, spawn_app,
    text::{Mark, TextBitmap, TextEngine, TextStyle, WRAP_SLACK, node_spans, plain_text},
    tree::{
        Anchor, AnchorAlign, Batch, Easing, Icon, Node, Op, Paint, Position, Property, Replica,
        Scrollbar, Shadow, Side, Span, Transition, Tree, Variant,
    },
};
use rquickjs::{Ctx, Exception, Function, function::MutFn};
use std::{
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
    sync::{Arc, mpsc::TryRecvError},
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

/// Pointer shape over what is under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Cursor {
    #[default]
    Default,
    Pointer,
    Text,
    NotAllowed,
}

pub struct Frame {
    pub items: Vec<Item>,
    /// Keep drawing: a transition, or a keyframe animation on screen, is running.
    pub animating: bool,
    /// Caret of the focused field, for the platform's input method window.
    pub ime_area: Option<Rect>,
}

// The app in QuickJS: `jsnirMount` once, `jsnirEvent(callback, text)` per event.
struct HostApp(Vm);
impl App for HostApp {
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

// Something under the pointer that reacts to it, recorded while drawing: a pressable box, or the root of a
// layer (which covers what is below it even when it reacts to nothing).
struct Hit {
    rect: Rect,
    id: u16,
    layer: i32,
}

// A scroll node drawn in the last frame, in painting order (an outer one comes first).
struct Scroll {
    id: u16,
    layer: i32,
    // Visible part of its rectangle on screen.
    rect: Rect,
    // Scale of its content on screen, to turn screen pixels into logical ones.
    k: f32,
    // Largest offset: content height minus box height.
    max: f32,
    // Height of the box in logical pixels: a click on the track pages by this much.
    view: f32,
    // Track and thumb of its scrollbar on screen, if it has one.
    bar: Option<(Rect, Rect)>,
}

pub struct Host {
    pub tree: Tree,
    bridge: Bridge,
    pending: u16,
    layout: Layout,
    text: TextEngine,
    motion: Motion,
    clock: Instant,
    // Timers from `after` operations: deadline in ms on `clock`, and the callback.
    timers: Vec<(f64, u16)>,
    // Events that did not fit in the queue, sent on the next poll.
    backlog: Vec<Event>,
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
    scrolls: Vec<Scroll>,
    // Offset of each scroll node in logical pixels; a node without an entry is at its top.
    scroll: HashMap<u16, f32>,
    // A scrollbar thumb being dragged: its scroll node, and how far below the thumb's top the pointer grabbed it.
    drag: Option<(u16, f32)>,
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
        Self::start(script, args, text, |_| Ok(()))
    }

    /// Exposes `jsnirRequest(json)` to an embedded app. The handler runs on the app thread.
    pub fn with_request_handler(
        script: String,
        args: String,
        text: TextEngine,
        mut handler: impl FnMut(String) -> Result<String, String> + Send + 'static,
    ) -> Result<Self, String> {
        Self::start(script, args, text, move |ctx| {
            let request = Function::new(
                ctx.clone(),
                MutFn::new(move |ctx: Ctx<'_>, json: String| {
                    handler(json).map_err(|error| Exception::throw_message(&ctx, &error))
                }),
            )?;
            ctx.globals().set("jsnirRequest", request)
        })
    }

    fn start(
        script: String,
        args: String,
        text: TextEngine,
        configure: impl for<'js> FnOnce(&Ctx<'js>) -> rquickjs::Result<()> + Send + 'static,
    ) -> Result<Self, String> {
        let (mount, bridge) = spawn_app(
            Duration::ZERO,
            move |replica: Rc<RefCell<Replica>>| -> Result<Box<dyn App>, String> {
                let vm = Vm::try_load_with(&script, replica, None, |c| {
                    c.globals().set("__jsnirArgs", args)?;
                    configure(c)
                })
                .map_err(|error| format!("script failed to load: {error}"))?;
                Ok(Box::new(HostApp(vm)))
            },
        )?;
        let mut host = Self {
            tree: Tree::default(),
            bridge,
            pending: 0,
            layout: Layout::default(),
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
            scrolls: Vec::new(),
            scroll: HashMap::new(),
            drag: None,
            images: HashMap::new(),
            image_generation: 0,
            font_epoch: 0,
            hide_caret: false,
            keyframes_at: None,
            reduced_motion: false,
        };
        host.receive(mount);
        if host.tree.root.is_none() {
            return Err("the app did not mount a root node".into());
        }
        Ok(host)
    }

    fn now(&self) -> f64 {
        self.clock.elapsed().as_secs_f64() * 1e3
    }

    /// Whether events are waiting for an answer from the app, or timers are due: keep polling.
    pub fn busy(&self) -> bool {
        self.pending > 0 || !self.timers.is_empty() || !self.backlog.is_empty()
    }

    fn send(&mut self, event: Event) {
        if self.pending < MAX_PENDING {
            self.bridge
                .events
                .try_send(Inbox::Event(event))
                .expect("event queue");
            self.pending += 1;
        } else {
            self.backlog.push(event);
        }
    }

    /// Sends a message from the embedding program to the app's `onHost` handler, in order with UI events.
    pub fn push(&mut self, text: String) {
        self.send(Event {
            callback: HOST_CALLBACK,
            text,
        });
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
            if matches!(node, Node::Text { .. }) {
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

    /// Fires due timers and applies the batches that arrived.
    pub fn poll(&mut self) {
        let now = self.now();
        let (due, waiting): (Vec<_>, Vec<_>) =
            self.timers.drain(..).partition(|(at, _)| *at <= now);
        self.timers = waiting;
        let backlog = std::mem::take(&mut self.backlog);
        for event in backlog {
            self.send(event);
        }
        for (_, callback) in due {
            self.send(Event {
                callback,
                text: String::new(),
            });
        }
        while self.pending > 0 {
            match self.bridge.updates.try_recv() {
                Ok(batch) => {
                    self.pending -= 1;
                    self.receive(batch);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => panic!("app thread disconnected"),
            }
        }
    }

    // Applies one app batch and updates the layout, transitions, timers and focus that depend on it.
    fn receive(&mut self, batch: Batch) {
        let mut removed = Vec::new();
        let mut created = Vec::new();
        let mut restyled = Vec::new();
        let mut texts = Vec::new();
        let mut timers = Vec::new();
        // Parents whose children were placed out of order, and moved nodes (their parent is known after the apply).
        let mut reordered = Vec::new();
        let mut moved = Vec::new();
        for op in &batch.ops {
            match op {
                Op::Remove { id } => removed.extend(self.tree.subtree(*id)),
                Op::Create {
                    id, parent, before, ..
                } => {
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
                Op::SetText { id, .. } | Op::SetSpans { id, .. } => texts.push(*id),
                Op::After { ms, callback } => timers.push((*ms, *callback)),
                _ => {}
            }
        }
        let result = self.tree.apply(batch);
        // The answer goes first: whatever the UI publishes next depends on this batch.
        let _ = self.bridge.log.send(Log::Answer(result.is_ok()));
        if let Err(e) = result {
            eprintln!("batch rejected ({e})");
            return;
        }
        // Nodes created and removed in the same batch are gone too.
        removed.extend(
            created
                .iter()
                .map(|(id, _)| *id)
                .filter(|id| self.tree.nodes.get(id).is_none()),
        );
        self.layout.removed(&removed);
        for id in &removed {
            self.scroll.remove(id);
        }
        self.motion.forget(&removed);
        if self.focused.is_some_and(|f| removed.contains(&f)) {
            self.focused = None;
        }
        for (id, parent) in created {
            self.layout.created(&self.tree, id, parent);
        }
        let moved = moved.into_iter().filter_map(|id| self.tree.parent(id));
        for parent in reordered.into_iter().chain(moved) {
            self.layout.reordered(&self.tree, parent);
        }
        for id in restyled {
            self.layout.restyled(&self.tree, id);
        }
        for id in texts {
            self.layout.text_changed(id);
        }
        let now = self.now();
        self.timers.extend(
            timers
                .into_iter()
                .map(|(ms, callback)| (now + f64::from(ms), callback)),
        );
    }

    // ---------- Input ----------

    // The topmost thing under `point`: the last one drawn, which is the highest layer's and the latest in it.
    fn top_hit(&self, point: [f32; 2]) -> Option<(u16, i32)> {
        self.hits
            .iter()
            .rev()
            .find(|h| contains(h.rect, point))
            .map(|h| (h.id, h.layer))
    }

    fn hit(&self, point: [f32; 2]) -> Option<u16> {
        self.top_hit(point).map(|(id, _)| id)
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
    /// (positive moves the content up), at once. Returns whether an offset changed.
    pub fn wheel(&mut self, delta_y: f32) -> bool {
        let Some((id, next)) = self.scroll_step(delta_y, false) else {
            return false;
        };
        self.motion.release(id, Channel::Scroll);
        self.scroll.insert(id, next);
        self.hover_frozen_until = self.now() + SCROLL_HOVER_IDLE_MS;
        true
    }

    /// Like `wheel`, but the offset glides to its new target over 150 ms, and repeated calls add up. For the
    /// line deltas of a mouse wheel; exact pixel deltas from a touchpad go through `wheel`.
    pub fn wheel_smooth(&mut self, delta_y: f32) -> bool {
        let Some((id, next)) = self.scroll_step(delta_y, true) else {
            return false;
        };
        let now = self.now();
        self.hover_frozen_until = now + SCROLL_HOVER_IDLE_MS;
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

    // The innermost scroll node under the pointer that can still move by `delta_y`, and the offset it reaches.
    // A smooth wheel adds to a glide's target; an immediate one moves from the offset on screen.
    fn scroll_step(&self, delta_y: f32, smooth: bool) -> Option<(u16, f32)> {
        let point = self.pointer?;
        let top = self.top_hit(point);
        self.scrolls.iter().rev().find_map(|s| {
            if s.k <= 0. || !contains(s.rect, point) || self.covered(top, s.layer, s.id) {
                return None;
            }
            let base = match self.motion.target(s.id, Channel::Scroll) {
                Some(target) if smooth => target.number(),
                _ => self.scroll.get(&s.id).copied().unwrap_or(0.),
            };
            let next = (base + delta_y / s.k).clamp(0., s.max);
            (next != base).then_some((s.id, next))
        })
    }

    // The offset of a scroll node in this frame: where its glide is, or the stored one, clamped to `max`.
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

    // A press on a scrollbar: on the thumb it starts a drag, on the track above or below it pages by a viewport.
    fn press_scrollbar(&mut self, point: [f32; 2]) -> bool {
        let top = self.top_hit(point);
        let Some((id, view, max, thumb)) = self.scrolls.iter().rev().find_map(|s| {
            let (track, thumb) = s.bar?;
            (contains(s.rect, point) && contains(track, point) && !self.covered(top, s.layer, s.id))
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

    /// The pointer moved. Returns the cursor shape for what is under it.
    pub fn pointer_move(&mut self, point: Option<[f32; 2]>) -> Cursor {
        self.pointer = point;
        self.refresh_hover();
        if let (Some((id, grab)), Some(p)) = (self.drag, point)
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
        let hit = point.and_then(|p| self.hit(p));
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
        match hit.and_then(|id| self.tree.nodes.get(&id)) {
            Some(Node::Input { .. }) => Cursor::Text,
            Some(Node::Box { disabled: true, .. }) => Cursor::NotAllowed,
            Some(Node::Box { press, .. }) if *press != 0 => Cursor::Pointer,
            _ => Cursor::Default,
        }
    }

    /// Primary button pressed. `select` comes from the click count (word, all) and modifiers.
    pub fn pointer_down(&mut self, select: PointSelection) {
        let Some(point) = self.pointer else { return };
        self.dismiss_outside(point);
        self.pressed_groups = self.hovered_groups.clone();
        if self.press_scrollbar(point) {
            return;
        }
        let hit = self.hit(point);
        match hit.and_then(|id| self.tree.nodes.get(&id)) {
            Some(Node::Input { .. }) => {
                let id = hit.unwrap();
                self.focus(Some(id));
                let (x, y) = self.field_point(id, point);
                self.edit(&[EditorEvent::Point { x, y, select }]);
                self.editor_dragging = true;
            }
            _ => {
                self.pressed = hit.filter(|id| self.pressable(*id));
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
        let Some(id) = self.pressed.take() else {
            return;
        };
        if self.pointer.and_then(|p| self.hit(p)) == Some(id)
            && let Some(Node::Box { press, .. }) = self.tree.nodes.get(&id)
        {
            let callback = *press;
            self.send(Event {
                callback,
                text: String::new(),
            });
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
            let text = serde_json::json!({ "x": point[0], "y": point[1] }).to_string();
            self.send(Event {
                callback: *contextmenu,
                text,
            });
        }
    }

    /// Escape was pressed: every box that is drawn with a `dismiss` callback is asked to close.
    pub fn escape(&mut self) {
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
        if let Some(Node::Box { dismiss, .. }) = self.tree.nodes.get(&id)
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
        self.screen.get(id as usize).copied().flatten()
    }

    /// Moves keyboard focus to a field, or clears it.
    pub fn focus(&mut self, id: Option<u16>) {
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
            ..
        }) = id.and_then(|id| self.tree.nodes.get(&id))
        {
            let (text, selection, multiline) = (text.clone(), *selection, *multiline);
            self.editor.reset(&text, selection, multiline);
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
        self.focused.is_some()
    }

    pub fn composing(&self) -> bool {
        self.focused.is_some() && self.editor.composing()
    }

    /// Keyboard and input-method events for the focused field.
    pub fn edit(&mut self, events: &[EditorEvent]) -> EditorOutput {
        let mut output = EditorOutput::default();
        let Some(id) = self.focused else {
            return output;
        };
        let composing = self.editor.composing()
            || events.iter().any(|e| match e {
                EditorEvent::Commit(_) => true,
                EditorEvent::Preedit { text, .. } => !text.is_empty(),
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
                    ..
                }) = self.tree.nodes.get(&id)
                else {
                    continue;
                };
                let (submit, multiline, submit_on_enter) = (*submit, *multiline, *submit_on_enter);
                if composing {
                    continue;
                }
                if matches!(event, EditorEvent::LineBreak) || (multiline && !submit_on_enter) {
                    self.editor.handle(&mut self.text, &EditorEvent::LineBreak);
                } else if submit != 0 {
                    let text = self.editor.text().into_owned();
                    if multiline {
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
        self.tree.apply_ui(batch.clone()).expect("UI batch");
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

    /// Lays out the tree for the window and returns what to draw.
    pub fn frame(&mut self, width: f32, height: f32, scale: f32, max_side: usize) -> Frame {
        let now = self.now();
        let mut frame = self.render(width, height, scale, max_side, now);
        // A glide keeps hover frozen until it has been over for SCROLL_HOVER_IDLE_MS.
        if self.motion.animating_on(Channel::Scroll, now) {
            self.hover_frozen_until = now + SCROLL_HOVER_IDLE_MS;
        }
        if now < self.hover_frozen_until {
            // Keeps drawing until the idle time is over, so hover is then recomputed.
            frame.animating = true;
            return frame;
        }
        // A layout change may have moved what is under the pointer: hover follows it, and a changed hover is
        // drawn at once.
        if self.refresh_hover() {
            return self.render(width, height, scale, max_side, now);
        }
        frame
    }

    fn render(&mut self, width: f32, height: f32, scale: f32, max_side: usize, now: f64) -> Frame {
        self.lay_out(width, height, now);
        let mut items = Vec::new();
        let mut hits = std::mem::take(&mut self.hits);
        hits.clear();
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
            };
            let mut walker = Walker {
                items: &mut items,
                hits: &mut hits,
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
                queued: 0,
                looping: false,
            };
            walker.node(self, root, base, scale, max_side);
            walker.draw_deferred(self, scale, max_side);
            looping = walker.looping;
        }
        self.hits = hits;
        self.scrolls = scrolls;
        self.group_rects = groups;
        self.hover_rects = hovers;
        self.dismiss_rects = dismissers;
        Frame {
            items,
            animating: self.motion.animating(now) || looping,
            ime_area,
        }
    }

    // Whether the pointer is in `rect` and nothing on a higher layer covers it: what a group or a hover box reacts to.
    fn pointed(&self, top: Option<(u16, i32)>, rect: Rect, layer: i32, id: u16) -> bool {
        self.pointer.is_some_and(|p| contains(rect, p)) && !self.covered(top, layer, id)
    }

    // Recomputes what the pointer is over, from the last frame: the hovered node, the group nodes and the boxes with
    // a hover callback. Calls the hover callbacks of the boxes it entered or left. Returns whether any of it changed.
    fn refresh_hover(&mut self) -> bool {
        let top = self.pointer.and_then(|p| self.top_hit(p));
        let hovered = top.map(|(id, _)| id).filter(|id| self.pressable(*id));
        let groups: Vec<u16> = self
            .group_rects
            .iter()
            .filter(|(id, rect, layer)| self.pointed(top, *rect, *layer, *id))
            .map(|(id, _, _)| *id)
            .collect();
        let boxes: Vec<u16> = self
            .hover_rects
            .iter()
            .filter(|(id, rect, layer)| self.pointed(top, *rect, *layer, *id))
            .map(|(id, _, _)| *id)
            .collect();
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
        let own = (
            self.hovered == Some(id),
            self.pressed == Some(id) && self.hovered == Some(id),
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
        let (hover, active) = self.interaction(id);
        let applied = [
            (!disabled && hover, &paint.hover),
            (!disabled && active, &paint.active),
            (self.focused == Some(id), &paint.focus),
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
                };
            }
        }
        out
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
        // Nodes with a height transition, a height in a variant or a height keyframe: their heights are pinned while
        // they change.
        let sized: Vec<(u16, Option<Transition>)> = self
            .tree
            .nodes
            .iter()
            .filter_map(|(id, node)| {
                let paint = node.paint()?;
                let transition = transition(paint, HEIGHT_TRANSITION).cloned();
                let variant = [&paint.hover, &paint.active, &paint.focus]
                    .into_iter()
                    .flatten()
                    .any(|v| v.height.is_some());
                let keyed = paint
                    .animations
                    .iter()
                    .flatten()
                    .any(|a| a.property == Property::Height);
                (transition.is_some() || variant || keyed).then_some((id, transition))
            })
            .collect();
        let mut changed = if sized.is_empty() {
            self.layout
                .compute(&self.tree, &mut self.text, width, height)
        } else {
            self.layout.pin_heights(&self.tree, &HashMap::new());
            let natural = self
                .layout
                .compute(&self.tree, &mut self.text, width, height);
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
                self.layout
                    .compute(&self.tree, &mut self.text, width, height)
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
        self.tree.apply_ui(batch.clone()).expect("layout batch");
        let _ = self.bridge.log.send(Log::Ui(batch));
    }
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
fn place(anchor: &Anchor, [ax, ay, aw, ah]: Rect, [w, h]: [f32; 2], window: Rect) -> [f32; 2] {
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
    [
        x.min(wx + ww - WINDOW_MARGIN - w).max(wx + WINDOW_MARGIN),
        y.min(wy + wh - WINDOW_MARGIN - h).max(wy + WINDOW_MARGIN),
    ]
}

// What a node inherits from its ancestors: a uniform scale `k` and offset, group opacity, brightness and clip.
#[derive(Clone, Copy)]
struct Context {
    k: f32,
    offset: [f32; 2],
    opacity: f32,
    brightness: f32,
    clip: Rect,
    // Rounded rectangle and corner radius of the innermost rounded clip; None for none.
    round_clip: Option<(Rect, Corners)>,
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
    items: &'a mut Vec<Item>,
    hits: &'a mut Vec<Hit>,
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

impl Walker<'_> {
    fn shown(
        &mut self,
        host: &mut Host,
        id: u16,
        paint: &Paint,
        rect: Rect,
        disabled: bool,
    ) -> Shown {
        let v = host.variant(id, paint, disabled);
        let background = v.background.or(paint.background);
        let brightness = v.brightness.or(paint.brightness).unwrap_or(1.);
        let scale = v.scale.or(paint.scale).unwrap_or(1.);
        let edge = v.edge.or(paint.edge);
        let opacity = v.opacity.or(paint.opacity).unwrap_or(1.);
        let (x, y) = (v.x.unwrap_or(paint.x), v.y.unwrap_or(paint.y));
        let text_color = v.color.or(paint.color);
        let now = self.now;
        let reduced = host.reduced_motion;
        let motion = &mut host.motion;
        // First time on screen: properties with a starting value animate from it.
        if motion.target(id, Property::Opacity).is_none()
            && let Some(starting) = &paint.starting
        {
            let from = [
                (Property::Opacity, starting.opacity, opacity),
                (Property::X, starting.x, x),
                (Property::Y, starting.y, y),
                (Property::Scale, starting.scale, scale),
                (Property::Draw, starting.draw, paint.draw.unwrap_or(1.)),
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
        let frozen = host.keyframes_at;
        let keyed = motion.keyframes(
            id,
            paint.animations.as_deref().unwrap_or(&[]),
            reduced,
            frozen,
            now,
        );
        let overridden = |property: Property| {
            keyed
                .values
                .iter()
                .find(|(p, _)| *p == property)
                .map(|(_, v)| *v)
        };
        let mut number = |property, value: f32| {
            let shown = motion
                .show(
                    id,
                    property,
                    Value::Number(value),
                    transition(paint, property),
                    now,
                )
                .number();
            overridden(property).map_or(shown, Value::number)
        };
        let opacity = number(Property::Opacity, opacity);
        let x = number(Property::X, x);
        let y = number(Property::Y, y);
        let scale = number(Property::Scale, scale);
        let brightness = number(Property::Brightness, brightness);
        let draw = number(Property::Draw, paint.draw.unwrap_or(1.));
        let left = number(Property::Left, rect[0]);
        let width = number(Property::Width, v.width.unwrap_or(rect[2]));
        // A color animates from transparent when it appears.
        let mut color = |property, value: Option<u32>| {
            let target = value.map_or(Value::Color([0.; 4]), Value::color);
            let shown = motion
                .show(id, property, target, transition(paint, property), now)
                .rgba();
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
    fn node(&mut self, host: &mut Host, id: u16, ctx: Context, scale: f32, max_side: usize) {
        let style = host.tree.nodes.get(&id).and_then(Node::style);
        let fixed = style.is_some_and(|s| s.position == Position::Fixed);
        let layer = style.and_then(|s| s.layer.or(fixed.then_some(1)));
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
        self.draw(host, id, ctx, scale, max_side);
    }

    // Draws the queued layers, lowest first and in document order within a layer. A layer's own layered children are
    // queued in turn, so they are drawn after the lower layers and before the higher ones.
    fn draw_deferred(&mut self, host: &mut Host, scale: f32, max_side: usize) {
        while let Some(next) = (0..self.deferred.len())
            .min_by_key(|&i| (self.deferred[i].layer, self.deferred[i].order))
        {
            let Deferred { id, layer, ctx, .. } = self.deferred.swap_remove(next);
            let ctx = match ctx {
                Some(ctx) => ctx,
                None => match self.fixed_context(host, id) {
                    Some(ctx) => ctx,
                    None => continue,
                },
            };
            self.layer = layer;
            self.layer_root = Some(id);
            self.draw(host, id, ctx, scale, max_side);
        }
        self.layer = 0;
        self.layer_root = None;
    }

    // The context a fixed node is drawn in: its origin is where it is placed this frame. None while its anchor is not
    // on screen, which hides it.
    fn fixed_context(&self, host: &Host, id: u16) -> Option<Context> {
        let style = host.tree.nodes.get(&id)?.style()?;
        let [_, _, w, h] = host.tree.rect(id);
        let [x, y] = match &style.anchor {
            Some(anchor) => place(anchor, host.screen_rect(anchor.node)?, [w, h], self.window),
            None => [style.left.unwrap_or(0.), style.top.unwrap_or(0.)],
        };
        Some(Context {
            k: 1.,
            offset: [x, y],
            opacity: 1.,
            brightness: 1.,
            clip: self.window,
            round_clip: None,
        })
    }

    fn draw(&mut self, host: &mut Host, id: u16, ctx: Context, scale: f32, max_side: usize) {
        let Some(node) = host.tree.nodes.get(&id).cloned() else {
            return;
        };
        let layout_rect = host.tree.rect(id);
        let disabled = matches!(node, Node::Box { disabled: true, .. });
        let default_paint = Paint::default();
        let paint = node.paint().unwrap_or(&default_paint);
        let shown = self.shown(host, id, paint, layout_rect, disabled);
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
        host.screen[id as usize] = Some(screen);
        let empty = (screen[2] <= 0. || screen[3] <= 0.) && host.tree.children(id).is_empty();
        // A walked node has visible ancestors, so a loop that is on screen keeps the window drawing.
        let on_screen = intersect(screen, ctx.clip);
        if shown.looping && on_screen[2] > 0. && on_screen[3] > 0. {
            self.looping = true;
        }
        if shown.opacity <= 0.001 || empty {
            return;
        }
        let layer = shown.opacity < 0.999;
        if layer {
            self.items.push(Item::PushLayer {
                opacity: shown.opacity,
            });
        }
        let clip = ctx.clip;
        let round_clip = ctx.round_clip;
        // The root of a layer covers what is below it, even where it reacts to nothing.
        if self.layer_root == Some(id) {
            self.hits.push(Hit {
                rect: intersect(screen, clip),
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
            .map(|source| host.images.get(source).cloned());
        let hidden = matches!(mask, Some(None));
        let mask = mask.flatten().map(|image| (image, screen));
        if !hidden {
            for shadow in &paint.shadows {
                let Shadow {
                    y,
                    blur,
                    spread,
                    color,
                } = *shadow;
                let shadow = ShadowSpec {
                    y: y * inner.k,
                    blur: blur * inner.k,
                    spread: spread * inner.k,
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
            if let Some([top, bottom]) = shown.gradient {
                let [top, bottom] = [top, bottom].map(|c| inner.color(Value::color(c).rgba()));
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
                    // does not darken where two meet. With a radius, a band is the rounded ring clipped to its
                    // strip, so the outer corners stay round; without one, it is a plain rectangle.
                    let [top, right, bottom, left] = sides.map(|w| w * inner.k);
                    let [x, y, w, h] = screen;
                    let middle = (h - top - bottom).max(0.);
                    let bands = [
                        (top, [x, y, w, top]),
                        (bottom, [x, y + h - bottom, w, bottom]),
                        (left, [x, y + top, left, middle]),
                        (right, [x + w - right, y + top, right, middle]),
                    ];
                    for (width, strip) in bands {
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
        let color = shown.color.unwrap_or([244., 244., 245., 255.]);
        match &node {
            Node::Text {
                text,
                font,
                lines,
                align,
                spans,
                ..
            } if !text.is_empty() => {
                let style = TextStyle {
                    align: *align,
                    ..TextStyle::of(font, inner.color(color))
                };
                // Draws the text cut to its line limit at the width the layout measured.
                let mut runs = node_spans(text, spans.as_deref()).into_owned();
                if shown.underline {
                    runs.iter_mut().for_each(|run| run.underline = true);
                }
                let rich = spans.as_ref().is_some_and(|spans| !spans.is_empty());
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
                    host.text
                        .cut_marks(&runs, layout_rect[2], *lines, &cut_style)
                } else {
                    let runs = match lines {
                        Some(lines) => host.text.limit_lines(
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
                                .and_then(|image| host.images.get(&image.source))
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
                        Mark::Underline { span, rect } => {
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
            Node::Icon { icon, stroke, .. } => {
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
            Node::Image { source, .. } => {
                if let Some(image) = host.images.get(source).filter(|_| !hidden) {
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
                ..
            } => {
                self.hits.push(Hit {
                    rect: intersect(screen, clip),
                    id,
                    layer: self.layer,
                });
                let content = inner.rect(host.input_content(id));
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
                if host.focused == Some(id) {
                    let requested = if *multiline {
                        host.scroll_offset(id, f32::MAX, self.now)
                    } else {
                        0.
                    };
                    host.editor.scroll_to(requested);
                    let frame = host.editor.frame(
                        &mut host.text,
                        &style,
                        [width, view],
                        scale * inner.k,
                        max_side,
                        host.editor_key,
                    );
                    host.editor_key = Some(frame.bitmap_key);
                    if *multiline {
                        if frame.scroll_y != requested {
                            host.motion.release(id, Channel::Scroll);
                        }
                        host.scroll.insert(id, frame.scroll_y);
                        self.scrolls.push(Scroll {
                            id,
                            layer: self.layer,
                            rect: intersect(content, clip),
                            k: inner.k,
                            max: (frame.content_height - view).max(0.),
                            view,
                            bar: None,
                        });
                    }
                    if text.is_empty() && !host.editor.composing() {
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
                    *self.ime_area = Some(caret);
                    let caret = (frame.caret_visible && !host.hide_caret).then_some(caret);
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
                    // Unfocused, the wrapped text is drawn whole and shifted up by the scroll; the clip shows the visible part.
                    let height = host.text.measure_styled(
                        &node_spans(text, None),
                        Some(width + WRAP_SLACK),
                        &style,
                    )[1]
                    .max(style.line_height);
                    let max = (height - view).max(0.);
                    let offset = host.scroll_offset(id, max, self.now);
                    self.scrolls.push(Scroll {
                        id,
                        layer: self.layer,
                        rect: intersect(content, clip),
                        k: inner.k,
                        max,
                        view,
                        bar: None,
                    });
                    self.items.push(Item::Text {
                        rect: [
                            content[0],
                            content[1] - offset * inner.k,
                            content[2],
                            height * inner.k,
                        ],
                        text: text.clone(),
                        spans: None,
                        style,
                        wrap: true,
                        zoom: inner.k,
                        clip: intersect(content, clip),
                        round_clip,
                    });
                } else {
                    self.items.push(Item::Text {
                        rect: line,
                        text: text.clone(),
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
        if matches!(node, Node::Box { press, contextmenu, .. } if press != 0 || contextmenu != 0) {
            self.hits.push(Hit {
                rect: intersect(screen, clip),
                id,
                layer: self.layer,
            });
        }
        if node.style().is_some_and(|s| s.group) && !disabled {
            self.groups.push((id, intersect(screen, clip), self.layer));
        }
        if let Node::Box { hover, dismiss, .. } = &node {
            let rect = intersect(screen, clip);
            if *hover != 0 {
                self.hovers.push((id, rect, self.layer));
            }
            if *dismiss != 0 {
                self.dismissers.push((id, rect, self.layer));
            }
        }
        let mut child = inner;
        if node.style().is_some_and(|s| s.clip || s.scroll) {
            child.clip = intersect(clip, screen);
            // shortcut: a rounded clip inside another rounded clip replaces it, so the outer corners are lost.
            if radius.iter().any(|&r| r > 0.) {
                child.round_clip = Some((screen, radius));
            }
        }
        // A scrollbar and its opacity this frame, drawn over the content.
        let mut bar = None;
        if node.style().is_some_and(|s| s.scroll) {
            // The offset is clamped to this frame's content, so it stays valid when the content shrinks.
            let content = host.layout.content_height(id);
            let max = (content - layout_rect[3]).max(0.);
            let offset = host.scroll_offset(id, max, self.now);
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
                        let over = host
                            .pointer
                            .is_some_and(|p| contains(intersect(screen, clip), p));
                        let dragged = host.drag.is_some_and(|(drag, _)| drag == id);
                        let shown = Value::Number(f32::from(u8::from(over || dragged)));
                        host.motion
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
                    bar = Some((scrollbar.clone(), rects.0, rects.1, alpha));
                    rects
                });
            self.scrolls.push(Scroll {
                id,
                layer: self.layer,
                rect: intersect(screen, clip),
                k: inner.k,
                max,
                view: layout_rect[3],
                bar: rects,
            });
        }
        if !matches!(node, Node::List { .. }) {
            for &c in host.tree.children(id).to_vec().iter() {
                self.node(host, c, child, scale, max_side);
            }
        }
        if let Some((scrollbar, track, thumb, alpha)) = bar.filter(|(_, _, _, alpha)| *alpha > 0.) {
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
        for n in 0..10 {
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
    fn edge_sides_draw_one_band_per_side_and_a_rounded_one_as_a_ring_clipped_to_it() {
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
        // The rounded box's 2 px bottom is the ring clipped to a 2 px strip: one ring, clipped to that strip.
        let rings: Vec<Rect> = frame
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Inset {
                    color, clip, rect, ..
                } if *color == red && clip[3] == 2. => {
                    assert_eq!(rect[2], width);
                    Some(*clip)
                }
                _ => None,
            })
            .collect();
        assert_eq!(rings.len(), 1, "{rings:?}");
        assert!(
            rings[0][1] > y,
            "the ring's strip is at the bottom: {rings:?}"
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
        let fills = fills_of(&frame_of(&mut host), ROW_FILL);
        assert_eq!(fills.len(), 1);
        assert!((fills[0][1] - top).abs() < 0.01, "{fills:?}");

        // Scrolling 45 px moves row 1 under the same point, but hover keeps row 0 until the scroll is idle.
        assert!(host.wheel(45.));
        let fills = fills_of(&frame_of(&mut host), ROW_FILL);
        assert_eq!(fills.len(), 1);
        assert!((fills[0][1] - (top - 45.)).abs() < 0.01, "{fills:?}");

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
        frame_of(&mut host);
        assert!(host.wheel(45.));
        let fills = fills_of(&frame_of(&mut host), ROW_FILL);
        assert!((fills[0][1] - (top - 45.)).abs() < 0.01, "{fills:?}");

        // Row 1 is under the moved pointer: no idle time to wait for.
        host.pointer_move(Some([x + 4., top + 6.]));
        let fills = fills_of(&frame_of(&mut host), ROW_FILL);
        assert_eq!(fills.len(), 1);
        assert!((fills[0][1] - (top - 15.)).abs() < 0.01, "{fills:?}");
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
        host.render(640., 560., 1., 4096, now)
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
}
