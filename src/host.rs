//! Runs any TSX app: the app thread sends batches, and this UI-side host keeps the tree, lays it out,
//! handles pointer, focus and text input, runs transitions and timers, and turns the tree into a list
//! of drawing items that any backend can paint. JS never runs on this thread.
use crate::{
    App, Bridge, Event, Inbox, Log, MAX_PENDING, Vm,
    editor::{EditorBitmapKey, EditorEvent, EditorFrame, EditorOutput, LineEditor, PointSelection},
    layout::Layout,
    motion::{Motion, Value},
    or_log, spawn_app,
    text::{TextBitmap, TextEngine, TextStyle},
    tree::{Batch, Icon, Node, Op, Paint, Property, Replica, Shadow, Transition, Tree},
};
use std::{
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
    sync::mpsc::TryRecvError,
    time::{Duration, Instant},
};

/// A rectangle in logical pixels: x, y, width, height.
pub type Rect = [f32; 4];
/// Straight (not premultiplied) RGBA.
pub type Rgba = [u8; 4];

/// What to draw, in painting order. Rectangles are already transformed and clipped by `clip`.
#[derive(Debug, Clone)]
pub enum Item {
    /// Starts a group drawn into its own layer and composited with `opacity`, as CSS `opacity` does.
    PushLayer {
        opacity: f32,
    },
    PopLayer,
    /// Outer shadow of a rounded rectangle; it is not drawn under the rectangle itself.
    Shadow {
        rect: Rect,
        radius: f32,
        shadow: ShadowSpec,
        clip: Rect,
    },
    /// Rounded rectangle filled with a vertical gradient (`top` = `bottom` for a solid color).
    Fill {
        rect: Rect,
        radius: f32,
        top: Rgba,
        bottom: Rgba,
        clip: Rect,
    },
    /// Inner shadow without blur: the part of the shape not covered by itself inset by `spread` and
    /// moved by `offset` (a ring with spread, a top highlight with offset).
    Inset {
        rect: Rect,
        radius: f32,
        spread: f32,
        offset: [f32; 2],
        color: Rgba,
        clip: Rect,
    },
    /// Text laid out in `rect` at `zoom` times the device scale.
    Text {
        rect: Rect,
        text: String,
        style: TextStyle,
        wrap: bool,
        zoom: f32,
        clip: Rect,
    },
    Icon {
        rect: Rect,
        icon: Icon,
        stroke: f32,
        draw: f32,
        color: Rgba,
        zoom: f32,
        clip: Rect,
    },
    /// The focused field's text, selection and caret.
    Editor {
        rect: Rect,
        frame: Box<EditorFrame>,
        caret: Option<Rect>,
        selection: Rgba,
        caret_color: Rgba,
        clip: Rect,
    },
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
    /// Keep drawing: a transition is running.
    pub animating: bool,
    /// Caret of the focused field, for the platform's input method window.
    pub ime_area: Option<Rect>,
}

// The app in QuickJS: `jsnirMount` once, `jsnirEvent(callback, text)` per event.
struct HostApp(Vm);
impl App for HostApp {
    fn mount(&mut self) -> Vec<Op> {
        self.0.call("jsnirMount", ())
    }
    fn handle(&mut self, e: Event, _: &Tree) -> Vec<Op> {
        or_log(self.0.try_call("jsnirEvent", (e.callback, e.text)))
    }
    fn sync(&mut self, view: &Tree, ids: Option<&[u16]>) {
        self.0.sync(view, ids)
    }
}

// Something under the pointer that reacts to it, recorded while drawing.
struct Hit {
    rect: Rect,
    id: u16,
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
    editor: LineEditor,
    editor_key: Option<EditorBitmapKey>,
    // A press inside the focused field extends the selection while the pointer moves.
    editor_dragging: bool,
    published: (String, [u32; 2]),
    hits: Vec<Hit>,
    /// Hides the caret, for reproducible captures.
    pub hide_caret: bool,
}

const HEIGHT_TRANSITION: Property = Property::Height;

impl Host {
    /// Starts `script` (SDK + app) on its own thread. `args` is a JSON value the app reads with `args()`.
    pub fn new(script: String, args: String, text: TextEngine) -> Self {
        let (mount, bridge) = spawn_app(
            Duration::ZERO,
            move |replica: Rc<RefCell<Replica>>| -> Box<dyn App> {
                let vm = Vm::try_load_with(&script, replica, None, |c| {
                    c.globals().set("__jsnirArgs", args)
                })
                .unwrap_or_else(|e| panic!("script failed to load: {e}"));
                Box::new(HostApp(vm))
            },
        );
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
            editor: LineEditor::default(),
            editor_key: None,
            editor_dragging: false,
            published: Default::default(),
            hits: Vec::new(),
            hide_caret: false,
        };
        host.receive(mount);
        host
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
        for op in &batch.ops {
            match op {
                Op::Remove { id } => removed.extend(self.tree.subtree(*id)),
                Op::Create { id, parent, .. } => created.push((*id, *parent)),
                Op::SetStyle { id, .. } => restyled.push(*id),
                Op::SetText { id, .. } => texts.push(*id),
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
        self.motion.forget(&removed);
        if self.focused.is_some_and(|f| removed.contains(&f)) {
            self.focused = None;
        }
        for (id, parent) in created {
            self.layout.created(&self.tree, id, parent);
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

    fn hit(&self, point: [f32; 2]) -> Option<u16> {
        self.hits
            .iter()
            .rev()
            .find(|h| contains(h.rect, point))
            .map(|h| h.id)
    }

    fn pressable(&self, id: u16) -> bool {
        matches!(self.tree.nodes.get(&id), Some(Node::Box { press, disabled: false, .. }) if *press != 0)
    }

    /// The pointer moved. Returns the cursor shape for what is under it.
    pub fn pointer_move(&mut self, point: Option<[f32; 2]>) -> Cursor {
        self.pointer = point;
        let hit = point.and_then(|p| self.hit(p));
        self.hovered = hit.filter(|id| self.pressable(*id));
        if let (Some(id), Some(p)) = (self.focused, point)
            && self.pressed.is_none()
            && self.editor_dragging
        {
            let x = p[0] - self.input_content(id)[0];
            self.edit(&[EditorEvent::Point {
                x,
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
        let hit = self.hit(point);
        match hit.and_then(|id| self.tree.nodes.get(&id)) {
            Some(Node::Input { .. }) => {
                let id = hit.unwrap();
                self.focus(Some(id));
                let x = point[0] - self.input_content(id)[0];
                self.edit(&[EditorEvent::Point { x, select }]);
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
            text, selection, ..
        }) = id.and_then(|id| self.tree.nodes.get(&id))
        {
            let (text, selection) = (text.clone(), *selection);
            self.editor.reset(&text, selection);
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
        for event in events {
            if output.interrupt_ime && event.involves_ime() {
                continue;
            }
            if matches!(event, EditorEvent::Enter) {
                if !composing
                    && let Some(Node::Input { submit, .. }) = self.tree.nodes.get(&id)
                    && *submit != 0
                {
                    let (callback, text) = (*submit, self.editor.text().to_owned());
                    self.send(Event { callback, text });
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
        let state = (self.editor.text().to_owned(), self.editor.selection());
        if state == self.published {
            return;
        }
        let changed = state.0 != self.published.0;
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
        text: &str,
        size: [f32; 2],
        scale: f32,
        wrap: bool,
        max_side: usize,
        style: TextStyle,
    ) -> Option<TextBitmap> {
        self.text
            .rasterize_styled(text, size, scale, wrap, max_side, style)
    }

    /// Forget uploaded images (the backend lost its surface).
    pub fn reset_images(&mut self) {
        self.editor_key = None;
    }

    // ---------- Frame ----------

    /// Lays out the tree for the window and returns what to draw.
    pub fn frame(&mut self, width: f32, height: f32, scale: f32, max_side: usize) -> Frame {
        let now = self.now();
        self.lay_out(width, height, now);
        let mut items = Vec::new();
        let mut hits = std::mem::take(&mut self.hits);
        hits.clear();
        let mut ime_area = None;
        if let Some(root) = self.tree.root {
            let base = Context {
                k: 1.,
                offset: [0., 0.],
                opacity: 1.,
                brightness: 1.,
                clip: [0., 0., width, height],
            };
            let mut walker = Walker {
                items: &mut items,
                hits: &mut hits,
                now,
                ime_area: &mut ime_area,
            };
            walker.node(self, root, base, scale, max_side);
        }
        self.hits = hits;
        Frame {
            items,
            animating: self.motion.animating(),
            ime_area,
        }
    }

    // Taffy layout with animated heights: a node with a `height` transition animates from its height on
    // screen to its new natural height, and pinning that height each frame moves what follows it.
    fn lay_out(&mut self, width: f32, height: f32, now: f64) {
        let animated: Vec<(u16, Transition)> = self
            .tree
            .nodes
            .iter()
            .filter_map(|(id, node)| {
                Some((id, transition(node.paint()?, HEIGHT_TRANSITION)?.clone()))
            })
            .collect();
        let mut changed = if animated.is_empty() {
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
            for (id, transition) in &animated {
                let target = natural_height(*id);
                let shown = self
                    .motion
                    .show(
                        *id,
                        HEIGHT_TRANSITION,
                        Value::Number(target),
                        Some(transition),
                        now,
                    )
                    .number();
                if (shown - target).abs() > 0.01 {
                    pins.insert(*id, shown);
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

// What a node inherits from its ancestors: a uniform scale `k` and offset, group opacity, brightness and clip.
#[derive(Clone, Copy)]
struct Context {
    k: f32,
    offset: [f32; 2],
    opacity: f32,
    brightness: f32,
    clip: Rect,
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
    now: f64,
    ime_area: &'a mut Option<Rect>,
}

// Values a node shows this frame, after its interaction state and its transitions.
struct Shown {
    opacity: f32,
    x: f32,
    y: f32,
    scale: f32,
    brightness: f32,
    background: Option<[f32; 4]>,
    edge: Option<[f32; 4]>,
    color: Option<[f32; 4]>,
    draw: f32,
    left: f32,
    width: f32,
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
        let state = |variant: &Option<crate::tree::Variant>, on: bool| {
            if on { variant.clone() } else { None }
        };
        let interactive = !disabled;
        let variants = [
            state(&paint.hover, interactive && host.hovered == Some(id)),
            state(
                &paint.active,
                interactive && host.pressed == Some(id) && host.hovered == Some(id),
            ),
            state(&paint.focus, host.focused == Some(id)),
        ];
        let mut background = paint.background;
        let mut brightness = paint.brightness.unwrap_or(1.);
        let mut scale = paint.scale.unwrap_or(1.);
        let mut edge = paint.edge;
        for variant in variants.into_iter().flatten() {
            background = variant.background.or(background);
            brightness = variant.brightness.unwrap_or(brightness);
            scale = variant.scale.unwrap_or(scale);
            edge = variant.edge.or(edge);
        }
        let now = self.now;
        let motion = &mut host.motion;
        // First time on screen: properties with a starting value animate from it.
        if motion.target(id, Property::Opacity).is_none()
            && let Some(starting) = &paint.starting
        {
            let from = [
                (
                    Property::Opacity,
                    starting.opacity,
                    paint.opacity.unwrap_or(1.),
                ),
                (Property::X, starting.x, paint.x),
                (Property::Y, starting.y, paint.y),
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
        let mut number = |property, value: f32| {
            motion
                .show(
                    id,
                    property,
                    Value::Number(value),
                    transition(paint, property),
                    now,
                )
                .number()
        };
        let opacity = number(Property::Opacity, paint.opacity.unwrap_or(1.));
        let x = number(Property::X, paint.x);
        let y = number(Property::Y, paint.y);
        let scale = number(Property::Scale, scale);
        let brightness = number(Property::Brightness, brightness);
        let draw = number(Property::Draw, paint.draw.unwrap_or(1.));
        let left = number(Property::Left, rect[0]);
        let width = number(Property::Width, rect[2]);
        // A color animates from transparent when it appears.
        let mut color = |property, value: Option<u32>| {
            let target = value.map_or(Value::Color([0.; 4]), Value::color);
            let shown = motion
                .show(id, property, target, transition(paint, property), now)
                .rgba();
            (value.is_some() || shown[3] > 0.).then_some(shown)
        };
        Shown {
            opacity,
            x,
            y,
            scale,
            brightness,
            background: color(Property::Background, background),
            edge: color(Property::Edge, edge),
            color: color(Property::Color, paint.color),
            draw,
            left,
            width,
        }
    }

    fn node(&mut self, host: &mut Host, id: u16, ctx: Context, scale: f32, max_side: usize) {
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
        let empty = (screen[2] <= 0. || screen[3] <= 0.) && host.tree.children(id).is_empty();
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
        let radius = paint.radius * inner.k;
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
                radius,
                shadow,
                clip,
            });
        }
        if let Some([top, bottom]) = paint.gradient {
            let [top, bottom] = [top, bottom].map(|c| inner.color(Value::color(c).rgba()));
            self.items.push(Item::Fill {
                rect: screen,
                radius,
                top,
                bottom,
                clip,
            });
        } else if let Some(background) = shown.background {
            let color = inner.color(background);
            self.items.push(Item::Fill {
                rect: screen,
                radius,
                top: color,
                bottom: color,
                clip,
            });
        }
        if let Some(edge) = shown.edge {
            let spread = paint.edge_width.unwrap_or(1.) * inner.k;
            let color = inner.color(edge);
            self.items.push(Item::Inset {
                rect: screen,
                radius,
                spread,
                offset: [0., 0.],
                color,
                clip,
            });
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
            });
        }
        let color = shown.color.unwrap_or([244., 244., 245., 255.]);
        match &node {
            Node::Text { text, font, .. } if !text.is_empty() => {
                let style = TextStyle::of(font, inner.color(color));
                self.items.push(Item::Text {
                    rect: screen,
                    text: text.clone(),
                    style,
                    wrap: true,
                    zoom: inner.k,
                    clip,
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
                });
            }
            Node::Input {
                text,
                font,
                placeholder,
                placeholder_color,
                ..
            } => {
                self.hits.push(Hit {
                    rect: intersect(screen, clip),
                    id,
                });
                let content = inner.rect(host.input_content(id));
                let style = TextStyle::of(font, inner.color(color));
                let line = [
                    content[0],
                    content[1] + (content[3] - style.line_height * inner.k) / 2.,
                    content[2],
                    style.line_height * inner.k,
                ];
                if host.focused == Some(id) {
                    let frame = host.editor.frame(
                        &mut host.text,
                        [content[2] / inner.k, line[3] / inner.k],
                        scale * inner.k,
                        max_side,
                        host.editor_key,
                    );
                    host.editor_key = Some(frame.bitmap_key);
                    if text.is_empty() && !host.editor.composing() {
                        self.placeholder(placeholder, *placeholder_color, font, &inner, line, clip);
                    }
                    let caret = [line[0] + frame.caret_x * inner.k, line[1], inner.k, line[3]];
                    *self.ime_area = Some(caret);
                    let caret = (frame.caret_visible && !host.hide_caret).then_some(caret);
                    self.items.push(Item::Editor {
                        rect: line,
                        frame: Box::new(frame),
                        caret,
                        selection: [70, 120, 220, 110],
                        caret_color: inner.color(color),
                        clip: intersect(content, clip),
                    });
                } else if text.is_empty() {
                    self.placeholder(placeholder, *placeholder_color, font, &inner, line, clip);
                } else {
                    self.items.push(Item::Text {
                        rect: line,
                        text: text.clone(),
                        style,
                        wrap: false,
                        zoom: inner.k,
                        clip: intersect(content, clip),
                    });
                }
            }
            _ => {}
        }
        if matches!(node, Node::Box { press, .. } if press != 0) {
            self.hits.push(Hit {
                rect: intersect(screen, clip),
                id,
            });
        }
        let mut child = inner;
        if node.style().is_some_and(|s| s.clip) {
            child.clip = intersect(clip, screen);
        }
        if !matches!(node, Node::List { .. }) {
            for &c in host.tree.children(id).to_vec().iter() {
                self.node(host, c, child, scale, max_side);
            }
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
        clip: Rect,
    ) {
        if text.is_empty() {
            return;
        }
        let color = ctx.color(Value::color(color.unwrap_or(0x757575ff)).rgba());
        self.items.push(Item::Text {
            rect: line,
            text: text.into(),
            style: TextStyle::of(font, color),
            wrap: false,
            zoom: ctx.k,
            clip,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::EditorEvent;

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
}
