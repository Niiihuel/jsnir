//! Tree of nodes with stable identity. A batch is validated whole before it touches the tree.
//! Two writers own separate nodes: the app (JS) and the UI. Their batches never touch the same
//! nodes, so they commute: they apply in any order, without locks or rebasing.
use serde::Deserialize;
use std::{
    collections::{HashMap, VecDeque},
    ops::Index,
};

contract_strings! {
    /// Main axis of a container.
    pub enum Axis as "Axis" { Column, Row }
}
contract_strings! {
    /// Cross-axis alignment of a container's children (CSS `align-items`).
    pub enum Align as "Align" { Stretch, Start, Center, End }
}
contract_strings! {
    /// Main-axis distribution of a container's children (CSS `justify-content`).
    pub enum Justify as "Justify" { Start, Center, End, Between }
}
contract_strings! {
    /// `absolute` takes the node out of the flow; `left` and `top` place it inside its parent.
    pub enum Position as "Position" { Relative, Absolute }
}

/// Space around the four sides: one number for all of them, or `[top, right, bottom, left]` as in CSS.
#[derive(Debug, Deserialize, Clone, Copy, PartialEq)]
#[serde(untagged)]
pub enum Edges {
    All(f32),
    Sides([f32; 4]),
}
impl Default for Edges {
    fn default() -> Self {
        Edges::All(0.)
    }
}
impl Edges {
    /// `[top, right, bottom, left]`.
    pub fn sides(self) -> [f32; 4] {
        match self {
            Edges::All(v) => [v; 4],
            Edges::Sides(sides) => sides,
        }
    }
}
impl crate::contract::Ts for Edges {
    fn ts() -> String {
        "number | [number, number, number, number]".into()
    }
}

contract_struct! {
    /// Flexbox layout style of a node, in logical pixels. No value means automatic.
    pub struct Style as "Style" {
        width: Option<f32>,
        height: Option<f32>,
        /// Share of the parent's free space the node takes (flex-grow).
        grow: f32,
        /// How much the node gives up when space is short (flex-shrink, default 1).
        shrink: Option<f32>,
        /// Main-axis size before growing and shrinking (flex-basis).
        basis: Option<f32>,
        padding: Edges,
        margin: Edges,
        /// Gap between children.
        gap: f32,
        /// Main axis of a `box` (default column).
        direction: Axis,
        align: Option<Align>,
        justify: Justify,
        position: Position,
        left: Option<f32>,
        top: Option<f32>,
        /// Clips the children to the node's rectangle (CSS `overflow: hidden`).
        clip: bool,
        /// An absolute node that takes the rectangle of another node (relative to its own parent).
        /// Moving it from one target to another animates with its `x` and `width` transitions.
        follow: Option<u16>,
    }
}
impl Style {
    fn valid(&self) -> bool {
        let finite = |v: f32| v.is_finite();
        let positive = |v: f32| v.is_finite() && v >= 0.0;
        let [p, m] = [self.padding.sides(), self.margin.sides()];
        [self.grow, self.gap]
            .into_iter()
            .chain(self.width)
            .chain(self.height)
            .chain(self.shrink)
            .chain(self.basis)
            .chain(p)
            .all(positive)
            && m.into_iter().chain(self.left).chain(self.top).all(finite)
    }
}

contract_strings! {
    /// Properties that a transition can animate. `left`, `width` and `height` animate layout changes:
    /// `left` and `width` move the node on screen only, `height` also moves what follows it.
    pub enum Property as "Property" {
        Opacity, X, Y, Scale, Brightness, Background, Edge, Color, Draw, Left, Width, Height
    }
}
contract_strings! {
    /// Named timing curves. `spring` is the React Bits `linear()` spring; `bezier` uses `Transition.bezier`.
    pub enum Easing as "Easing" { Linear, Ease, EaseOut, Spring, Bezier }
}
contract_struct! {
    /// Animates changes of one property, as a CSS transition. Durations in milliseconds.
    pub struct Transition as "Transition" {
        property: Property,
        duration: f32,
        delay: f32,
        easing: Easing,
        /// Control points for `easing: "bezier"`, as in CSS `cubic-bezier(x1, y1, x2, y2)`.
        bezier: Option<[f32; 4]>,
        /// Fraction of the eased progress during which the value holds its start (a keyframe at that offset).
        hold: f32,
    }
}
contract_struct! {
    /// Outer shadow (CSS `box-shadow` without `inset`). Colors are `0xRRGGBBAA`.
    pub struct Shadow as "Shadow" {
        y: f32,
        blur: f32,
        spread: f32,
        color: u32,
    }
}
contract_struct! {
    /// Paint overrides for an interaction state. Rust applies them without asking JS.
    pub struct Variant as "Variant" {
        background: Option<u32>,
        brightness: Option<f32>,
        scale: Option<f32>,
        edge: Option<u32>,
    }
}
contract_struct! {
    /// Values a node shows when it first appears, before animating to its paint (CSS `@starting-style`).
    pub struct Starting as "Starting" {
        opacity: Option<f32>,
        x: Option<f32>,
        y: Option<f32>,
        scale: Option<f32>,
        draw: Option<f32>,
    }
}
contract_struct! {
    /// How a node looks. Colors are `0xRRGGBBAA`. Changes animate with the matching `transitions`.
    pub struct Paint as "Paint" {
        background: Option<u32>,
        /// Vertical gradient, top to bottom (overrides `background`).
        gradient: Option<[u32; 2]>,
        radius: f32,
        /// Inner ring (CSS `inset 0 0 0 <edgeWidth> <edge>`).
        edge: Option<u32>,
        edge_width: Option<f32>,
        /// Inner top highlight (CSS `inset 0 1px 0 <highlight>`).
        highlight: Option<u32>,
        shadows: Vec<Shadow>,
        opacity: Option<f32>,
        /// Visual offset, which does not move the layout.
        x: f32,
        y: f32,
        scale: Option<f32>,
        /// Multiplies the node's colors, as CSS `filter: brightness()`.
        brightness: Option<f32>,
        /// Text and icon color.
        color: Option<u32>,
        /// Drawn fraction of an icon's strokes (1 by default).
        draw: Option<f32>,
        transitions: Vec<Transition>,
        starting: Option<Starting>,
        hover: Option<Variant>,
        active: Option<Variant>,
        focus: Option<Variant>,
    }
}
impl Paint {
    fn valid(&self) -> bool {
        let finite = |v: f32| v.is_finite();
        let numbers = [self.radius, self.x, self.y]
            .into_iter()
            .chain(self.edge_width)
            .chain(self.opacity)
            .chain(self.scale)
            .chain(self.brightness)
            .chain(self.draw)
            .chain(
                self.starting
                    .iter()
                    .flat_map(|s| [s.opacity, s.x, s.y, s.scale, s.draw])
                    .flatten(),
            )
            .chain(self.shadows.iter().flat_map(|s| [s.y, s.blur, s.spread]))
            .chain(self.transitions.iter().flat_map(|t| {
                [t.duration, t.delay, t.hold]
                    .into_iter()
                    .chain(t.bezier.into_iter().flatten())
            }));
        numbers.into_iter().all(finite)
            && self
                .transitions
                .iter()
                .all(|t| t.duration >= 0. && t.delay >= 0.)
    }
}
contract_struct! {
    /// Text style. `lineHeight` multiplies the size (CSS unitless line-height); `letterSpacing` is in em.
    pub struct Font as "Font" {
        size: Option<f32>,
        weight: Option<u16>,
        line_height: Option<f32>,
        letter_spacing: f32,
    }
}
contract_strings! {
    /// Built-in stroke icons (Hugeicons free, MIT).
    pub enum Icon as "IconName" { ArrowLeft, ArrowRight, Tick, CheckCircle }
}

contract_enum! {
    /// Tree nodes. The app creates them on mount or in a batch.
    pub enum Node as "Node", tag "kind" {
        /// Container that stacks its children vertically.
        Column {
            /// Rust derives the children from each `create`; JS cannot send them.
            (skip) children: Vec<u16>,
            (default) style: Style,
        },
        /// Scrolling list: the backend draws only the visible rows.
        /// For layout it is a leaf: its items do not go through Taffy.
        List { (skip) children: Vec<u16>, (default) style: Style },
        /// Counter label, drawn as three seven-segment digits.
        Label { value: u16 },
        Button { callback: u16 },
        Text { text: String, (default) style: Style, (default) font: Font, (default) paint: Box<Paint> },
        /// Text field. The app declares it on mount; afterwards only the UI writes it.
        Input {
            (default) text: String,
            (default) selection: [u32; 2],
            /// Callback that receives the submitted text. 0: no submit.
            (default) submit: u16,
            /// Callback that receives the text after each edit. 0: none.
            (default) change: u16,
            (default) placeholder: String,
            (default) placeholder_color: Option<u32>,
            (default) style: Style,
            (default) font: Font,
            (default) paint: Box<Paint>,
        },
        /// General container: layout, paint and an optional press callback.
        Box {
            (skip) children: Vec<u16>,
            (default) style: Style,
            (default) paint: Box<Paint>,
            /// Callback for a click or tap. 0: not pressable.
            (default) press: u16,
            (default) disabled: bool,
        },
        /// A stroke icon of `size` × `size` logical pixels.
        Icon { icon: Icon, size: f32, stroke: f32, (default) style: Style, (default) paint: Box<Paint> },
    }
}
contract_enum! {
    /// Batch operations.
    pub enum Op as "Operation", tag "op" {
        Create { id: u16, (default) parent: Option<u16>, node: Node },
        Set { id: u16, value: u16 },
        SetText { id: u16, text: String },
        /// Replaces the paint of a node. Properties with a transition animate from what is on screen.
        SetPaint { id: u16, paint: Box<Paint> },
        SetStyle { id: u16, style: Style },
        SetDisabled { id: u16, disabled: bool },
        /// Removes the node and all its descendants. The node must exist before the batch.
        Remove { id: u16 },
        /// Calls `callback` once after `ms` milliseconds. It touches no node.
        After { ms: f32, callback: u16 },
        /// UI only: an app batch that contains it is rejected.
        (ui) SetInput { id: u16, text: String, selection: [u32; 2] },
        /// UI only: rectangle computed by the layout (x, y, width, height).
        (ui) SetRect { id: u16, rect: [f32; 4] },
    }
}
/// TypeScript declarations of every contract type, in dependency order.
pub(crate) fn ts_decls() -> String {
    [
        Axis::ts_decl(),
        Align::ts_decl(),
        Justify::ts_decl(),
        Position::ts_decl(),
        Style::ts_decl(),
        Property::ts_decl(),
        Easing::ts_decl(),
        Transition::ts_decl(),
        Shadow::ts_decl(),
        Variant::ts_decl(),
        Starting::ts_decl(),
        Paint::ts_decl(),
        Font::ts_decl(),
        Icon::ts_decl(),
        Node::ts_decl(),
        Op::ts_decl(),
    ]
    .concat()
}
impl Node {
    /// Layout style, if the node kind has one.
    pub fn style(&self) -> Option<&Style> {
        match self {
            Node::Column { style, .. }
            | Node::List { style, .. }
            | Node::Text { style, .. }
            | Node::Input { style, .. }
            | Node::Box { style, .. }
            | Node::Icon { style, .. } => Some(style),
            Node::Label { .. } | Node::Button { .. } => None,
        }
    }
    fn style_mut(&mut self) -> Option<&mut Style> {
        match self {
            Node::Column { style, .. }
            | Node::List { style, .. }
            | Node::Text { style, .. }
            | Node::Input { style, .. }
            | Node::Box { style, .. }
            | Node::Icon { style, .. } => Some(style),
            Node::Label { .. } | Node::Button { .. } => None,
        }
    }
    /// Paint, if the node kind has one.
    pub fn paint(&self) -> Option<&Paint> {
        match self {
            Node::Text { paint, .. }
            | Node::Input { paint, .. }
            | Node::Box { paint, .. }
            | Node::Icon { paint, .. } => Some(paint),
            _ => None,
        }
    }
    fn paint_mut(&mut self) -> Option<&mut Paint> {
        match self {
            Node::Text { paint, .. }
            | Node::Input { paint, .. }
            | Node::Box { paint, .. }
            | Node::Icon { paint, .. } => Some(paint),
            _ => None,
        }
    }
    /// Whether the node holds children: a column, a list or a box.
    pub fn is_container(&self) -> bool {
        matches!(
            self,
            Node::Column { .. } | Node::List { .. } | Node::Box { .. }
        )
    }
    fn children_mut(&mut self) -> Option<&mut Vec<u16>> {
        match self {
            Node::Column { children, .. }
            | Node::List { children, .. }
            | Node::Box { children, .. } => Some(children),
            _ => None,
        }
    }
}
impl Op {
    /// Nodes whose content changes: the node and, on create, also its parent.
    pub fn touched(&self) -> [Option<u16>; 2] {
        match self {
            Op::Create { id, parent, .. } => [Some(*id), *parent],
            Op::Set { id, .. }
            | Op::SetText { id, .. }
            | Op::SetPaint { id, .. }
            | Op::SetStyle { id, .. }
            | Op::SetDisabled { id, .. }
            | Op::SetInput { id, .. }
            | Op::SetRect { id, .. }
            | Op::Remove { id } => [Some(*id), None],
            Op::After { .. } => [None, None],
        }
    }
}
#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct Batch {
    /// App batches applied before this one. The VM thread fills it in; JS does not send it.
    #[serde(default)]
    pub base: u64,
    pub ops: Vec<Op>,
}
/// Nodes indexed by id: O(1) reads.
/// shortcut: a Vec of up to 65,536 slots indexed by id; sparse, large ids would want a map.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Nodes(Vec<Option<Node>>);
impl Nodes {
    pub fn get(&self, id: &u16) -> Option<&Node> {
        self.0.get(*id as usize)?.as_ref()
    }
    pub fn get_mut(&mut self, id: &u16) -> Option<&mut Node> {
        self.0.get_mut(*id as usize)?.as_mut()
    }
    fn insert(&mut self, id: u16, node: Node) {
        let i = id as usize;
        if self.0.len() <= i {
            self.0.resize(i + 1, None);
        }
        self.0[i] = Some(node);
    }
    /// One past the largest id ever created.
    pub fn end(&self) -> usize {
        self.0.len()
    }
    fn remove(&mut self, id: u16) {
        if let Some(slot) = self.0.get_mut(id as usize) {
            *slot = None;
        }
    }
    pub fn iter(&self) -> impl Iterator<Item = (u16, &Node)> {
        self.0
            .iter()
            .enumerate()
            .filter_map(|(i, n)| Some((i as u16, n.as_ref()?)))
    }
}
impl Index<&u16> for Nodes {
    type Output = Node;
    fn index(&self, id: &u16) -> &Node {
        self.get(id).expect("no such node")
    }
}
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Tree {
    pub nodes: Nodes,
    /// Batches applied from both writers.
    pub version: u64,
    /// App batches applied. The next app batch must carry this as its base.
    pub app_seq: u64,
    // Absolute layout rectangles, by id. Only the UI writes them.
    rects: Vec<[f32; 4]>,
    /// Tree root, if there is one.
    pub root: Option<u16>,
    // Parent of each id, to detach a removed node without walking the tree.
    parents: Vec<Option<u16>>,
}
// State of an id during validation: what the batch already created or removed overrides the tree.
// A map rather than a list: validating a batch of n creates is O(n), not O(n²).
fn find<'a>(
    nodes: &'a Nodes,
    overlay: &HashMap<u16, Option<&'a Node>>,
    id: u16,
) -> Option<&'a Node> {
    match overlay.get(&id) {
        Some(node) => *node,
        None => nodes.get(&id),
    }
}
impl Tree {
    /// Applies a whole app batch or no operation at all. On rejection the tree and version stay intact.
    pub fn apply(&mut self, batch: Batch) -> Result<(), &'static str> {
        if batch.base != self.app_seq {
            return Err("stale_base");
        }
        self.validate(&batch.ops, false)?;
        self.write(batch.ops);
        self.app_seq += 1;
        Ok(())
    }
    /// Applies a UI batch. It may only contain UI operations and carries no base: the UI is the authority.
    pub fn apply_ui(&mut self, batch: Batch) -> Result<(), &'static str> {
        self.validate(&batch.ops, true)?;
        self.write(batch.ops);
        Ok(())
    }
    fn validate(&self, ops: &[Op], ui: bool) -> Result<(), &'static str> {
        let mut created: HashMap<u16, Option<&Node>> = HashMap::new();
        // Parent of each node this batch created, to remove them with an ancestor removed later in the batch.
        let mut created_parent: HashMap<u16, u16> = HashMap::new();
        let mut has_root = self.root.is_some();
        for op in ops {
            match (op, ui) {
                (Op::SetInput { id, .. }, true) => {
                    if !matches!(self.nodes.get(id), Some(Node::Input { .. })) {
                        return Err("invalid_node");
                    }
                }
                (Op::SetRect { id, rect }, true) => {
                    if self.nodes.get(id).is_none() || !rect.iter().all(|v| v.is_finite()) {
                        return Err("invalid_node");
                    }
                }
                (Op::SetInput { .. } | Op::SetRect { .. }, false) => return Err("ui_only_op"),
                (Op::Remove { id }, false) => {
                    // Only nodes that existed before the batch and that the batch has not removed yet.
                    if self.nodes.get(id).is_none() || created.contains_key(id) {
                        return Err("invalid_node");
                    }
                    if self.root == Some(*id) {
                        has_root = false;
                    }
                    created.extend(self.subtree(*id).into_iter().map(|i| (i, None)));
                    // Nodes this batch created under the removed subtree go with it.
                    loop {
                        let doomed: Vec<u16> = created_parent
                            .iter()
                            .filter(|(c, p)| {
                                matches!(created.get(*c), Some(Some(_)))
                                    && matches!(created.get(*p), Some(None))
                            })
                            .map(|(c, _)| *c)
                            .collect();
                        if doomed.is_empty() {
                            break;
                        }
                        created.extend(doomed.into_iter().map(|c| (c, None)));
                    }
                }
                (_, true) => return Err("app_only_op"),
                (Op::Create { id, parent, node }, false) => {
                    if find(&self.nodes, &created, *id).is_some() {
                        return Err("duplicate_id");
                    }
                    match parent {
                        None if !has_root => has_root = true,
                        Some(p)
                            if find(&self.nodes, &created, *p).is_some_and(Node::is_container) =>
                        {
                            created_parent.insert(*id, *p);
                        }
                        _ => return Err("invalid_parent"),
                    }
                    if matches!(node, Node::Label { value } if *value >= 1000) {
                        return Err("value_out_of_range");
                    }
                    if node.style().is_some_and(|s| !s.valid()) {
                        return Err("invalid_style");
                    }
                    if node.paint().is_some_and(|p| !p.valid()) {
                        return Err("invalid_paint");
                    }
                    if matches!(node, Node::Icon { size, stroke, .. } if !(size.is_finite() && *size >= 0. && stroke.is_finite() && *stroke >= 0.))
                    {
                        return Err("invalid_icon");
                    }
                    created.insert(*id, Some(node));
                }
                (Op::Set { id, value }, false) => {
                    if !matches!(find(&self.nodes, &created, *id), Some(Node::Label { .. })) {
                        return Err("invalid_node");
                    }
                    if *value >= 1000 {
                        return Err("value_out_of_range");
                    }
                }
                (Op::SetText { id, .. }, false) => {
                    if !matches!(find(&self.nodes, &created, *id), Some(Node::Text { .. })) {
                        return Err("invalid_node");
                    }
                }
                (Op::SetPaint { id, paint }, false) => {
                    if find(&self.nodes, &created, *id)
                        .and_then(Node::paint)
                        .is_none()
                    {
                        return Err("invalid_node");
                    }
                    if !paint.valid() {
                        return Err("invalid_paint");
                    }
                }
                (Op::SetStyle { id, style }, false) => {
                    if find(&self.nodes, &created, *id)
                        .and_then(Node::style)
                        .is_none()
                    {
                        return Err("invalid_node");
                    }
                    if !style.valid() {
                        return Err("invalid_style");
                    }
                }
                (Op::SetDisabled { id, .. }, false) => {
                    if !matches!(find(&self.nodes, &created, *id), Some(Node::Box { .. })) {
                        return Err("invalid_node");
                    }
                }
                (Op::After { ms, .. }, false) => {
                    if !(ms.is_finite() && *ms >= 0.) {
                        return Err("invalid_timer");
                    }
                }
            }
        }
        Ok(())
    }
    fn write(&mut self, ops: Vec<Op>) {
        for op in ops {
            match op {
                Op::Create { id, parent, node } => {
                    if let Some(children) = parent
                        .and_then(|p| self.nodes.get_mut(&p))
                        .and_then(Node::children_mut)
                    {
                        children.push(id);
                    }
                    if parent.is_none() {
                        self.root = Some(id);
                    }
                    let i = id as usize;
                    if self.parents.len() <= i {
                        self.parents.resize(i + 1, None);
                    }
                    self.parents[i] = parent;
                    self.nodes.insert(id, node);
                }
                Op::Remove { id } => {
                    if let Some(parent) = self.parent(id)
                        && let Some(children) =
                            self.nodes.get_mut(&parent).and_then(Node::children_mut)
                    {
                        children.retain(|c| *c != id);
                    }
                    if self.root == Some(id) {
                        self.root = None;
                    }
                    for i in self.subtree(id) {
                        self.nodes.remove(i);
                        if let Some(rect) = self.rects.get_mut(i as usize) {
                            *rect = [0.0; 4];
                        }
                        if let Some(p) = self.parents.get_mut(i as usize) {
                            *p = None;
                        }
                    }
                }
                Op::Set { id, value: new } => {
                    if let Some(Node::Label { value }) = self.nodes.get_mut(&id) {
                        *value = new;
                    }
                }
                Op::SetText { id, text: new } => {
                    if let Some(Node::Text { text, .. }) = self.nodes.get_mut(&id) {
                        *text = new;
                    }
                }
                Op::SetPaint { id, paint } => {
                    if let Some(slot) = self.nodes.get_mut(&id).and_then(Node::paint_mut) {
                        *slot = *paint;
                    }
                }
                Op::SetStyle { id, style } => {
                    if let Some(slot) = self.nodes.get_mut(&id).and_then(Node::style_mut) {
                        *slot = style;
                    }
                }
                Op::SetDisabled { id, disabled: new } => {
                    if let Some(Node::Box { disabled, .. }) = self.nodes.get_mut(&id) {
                        *disabled = new;
                    }
                }
                Op::After { .. } => {}
                Op::SetRect { id, rect } => {
                    let i = id as usize;
                    if self.rects.len() <= i {
                        self.rects.resize(i + 1, [0.0; 4]);
                    }
                    self.rects[i] = rect;
                }
                Op::SetInput {
                    id,
                    text: new,
                    selection: sel,
                } => {
                    if let Some(Node::Input {
                        text, selection, ..
                    }) = self.nodes.get_mut(&id)
                    {
                        (*text, *selection) = (new, sel);
                    }
                }
            }
        }
        self.version += 1;
    }
    /// The node and all its descendants, in preorder.
    pub fn subtree(&self, id: u16) -> Vec<u16> {
        let (mut out, mut stack) = (Vec::new(), vec![id]);
        while let Some(i) = stack.pop() {
            out.push(i);
            stack.extend(self.children(i).iter().rev());
        }
        out
    }
    /// Children of a container, or none if the id is not one.
    pub fn children(&self, id: u16) -> &[u16] {
        match self.nodes.get(&id) {
            Some(
                Node::Column { children, .. }
                | Node::List { children, .. }
                | Node::Box { children, .. },
            ) => children,
            _ => &[],
        }
    }
    /// Parent of a node, if it has one.
    pub fn parent(&self, id: u16) -> Option<u16> {
        self.parents.get(id as usize).copied().flatten()
    }
    /// Absolute layout rectangle (x, y, width, height); zeros until computed.
    pub fn rect(&self, id: u16) -> [f32; 4] {
        self.rects.get(id as usize).copied().unwrap_or_default()
    }
    /// Text of a `Text` or `Input` node.
    pub fn text(&self, id: u16) -> Option<&str> {
        match self.nodes.get(&id)? {
            Node::Text { text, .. } | Node::Input { text, .. } => Some(text),
            _ => None,
        }
    }
}
/// Replica of the tree on the JS thread. `view` is what the UI confirmed plus the app's own batches
/// still awaiting an answer, so JS reads its own writes without waiting. The UI answers in order.
#[derive(Debug, Default)]
pub struct Replica {
    pub confirmed: Tree,
    pub view: Tree,
    // Each pending batch remembers whether it was applied to the view.
    pending: VecDeque<(Batch, bool)>,
}
impl Replica {
    /// Records a batch on its way to the UI and applies it to the view if it is valid here.
    /// If it is not, the local version does not advance and the next batch leaves with the right base.
    pub fn produced(&mut self, batch: &Batch) -> Result<(), &'static str> {
        let result = self.view.apply(batch.clone());
        self.pending.push_back((batch.clone(), result.is_ok()));
        result
    }
    /// The UI's answer to the oldest pending batch. Returns whether the view was rebuilt entirely.
    pub fn answered(&mut self, accepted: bool) -> bool {
        let (batch, in_view) = self
            .pending
            .pop_front()
            .expect("answer without a pending batch");
        if accepted {
            self.confirmed
                .apply(batch)
                .expect("the replica cannot apply a batch the UI accepted");
        } else if in_view {
            // The view counted on that batch: rebuild it from what is confirmed plus the pending batches that are still valid.
            self.view = self.confirmed.clone();
            for (b, applied) in &mut self.pending {
                *applied = self.view.apply(b.clone()).is_ok();
            }
            return true;
        }
        false
    }
    /// A batch the UI already applied. It touches only UI nodes, so it commutes with the pending ones.
    pub fn foreign(&mut self, batch: Batch) {
        self.confirmed
            .apply_ui(batch.clone())
            .expect("invalid UI batch in the replica");
        self.view
            .apply_ui(batch)
            .expect("invalid UI batch in the replica");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn mounted() -> Tree {
        let mut t = Tree::default();
        let ops = serde_json::from_str(
            r#"[{"op":"create","id":1,"node":{"kind":"column"}},
                {"op":"create","id":2,"parent":1,"node":{"kind":"label","value":0}},
                {"op":"create","id":3,"parent":1,"node":{"kind":"button","callback":1}}]"#,
        )
        .unwrap();
        t.apply(Batch { base: 0, ops }).unwrap();
        t
    }
    #[test]
    fn replica_reads_own_writes_and_follows_the_ui_answers() {
        let set = |base, id, value| Batch {
            base,
            ops: vec![Op::Set { id, value }],
        };
        let mount = Batch {
            base: 0,
            ops: mounted()
                .nodes
                .iter()
                .map(|(id, node)| (id, node.clone()))
                .map(|(id, node)| {
                    let parent = (id != 1).then_some(1);
                    Op::Create {
                        id,
                        parent,
                        node: if id == 1 {
                            Node::Column {
                                children: vec![],
                                style: Style::default(),
                            }
                        } else {
                            node
                        },
                    }
                })
                .collect(),
        };
        let (mut ui, mut r) = (Tree::default(), Replica::default());
        // b2 targets a missing node: the replica does not apply it and b3 leaves with the same base.
        let batches = [mount, set(1, 2, 1), set(2, 99, 1), set(2, 2, 2)];
        let local: Vec<_> = batches.iter().map(|b| r.produced(b).is_ok()).collect();
        assert_eq!(local, [true, true, false, true]);
        assert_eq!(
            (r.view.version, &r.view.nodes[&2]),
            (3, &Node::Label { value: 2 })
        );
        for b in batches {
            let ok = ui.apply(b).is_ok();
            r.answered(ok);
        }
        assert_eq!((&r.confirmed, &r.view), (&ui, &ui));
        // If the UI rejects a batch the view counted on, the view returns to what is confirmed.
        r.produced(&set(3, 2, 7)).unwrap();
        r.produced(&set(4, 2, 8)).unwrap();
        r.answered(false);
        assert_eq!(
            (r.view.version, &r.view.nodes[&2]),
            (3, &Node::Label { value: 2 })
        );
        r.answered(false);
        assert_eq!(r.view, ui);
    }
    #[test]
    fn app_and_ui_write_separate_nodes_and_commute() {
        let mut t = Tree::default();
        let ops = serde_json::from_str(
            r#"[{"op":"create","id":1,"node":{"kind":"column"}},
                {"op":"create","id":2,"parent":1,"node":{"kind":"input"}},
                {"op":"create","id":3,"parent":1,"node":{"kind":"text","text":""}}]"#,
        )
        .unwrap();
        t.apply(Batch { base: 0, ops }).unwrap();
        let input = |text: &str| Op::SetInput {
            id: 2,
            text: text.into(),
            selection: [0, 1],
        };
        let app = Batch {
            base: 1,
            ops: vec![Op::SetText {
                id: 3,
                text: "ready".into(),
            }],
        };
        let ui = Batch {
            base: 0,
            ops: vec![input("hello")],
        };
        let before = t.clone();
        // The app cannot write the field, and the UI cannot write the app's nodes.
        for (ops, err) in [
            (vec![input("overwritten")], "ui_only_op"),
            (
                vec![Op::SetText {
                    id: 2,
                    text: "overwritten".into(),
                }],
                "invalid_node",
            ),
        ] {
            assert_eq!(t.clone().apply(Batch { base: 1, ops }), Err(err));
        }
        assert_eq!(t.clone().apply_ui(app.clone()), Err("app_only_op"));
        assert_eq!(
            t.clone().apply_ui(Batch {
                base: 0,
                ops: vec![Op::SetInput {
                    id: 3,
                    text: "x".into(),
                    selection: [0, 0]
                }]
            }),
            Err("invalid_node")
        );
        // UI batches do not move the app's base: both orders produce the same tree.
        let (mut a, mut b) = (before.clone(), before);
        a.apply(app.clone()).unwrap();
        a.apply_ui(ui.clone()).unwrap();
        b.apply_ui(ui).unwrap();
        b.apply(app).unwrap();
        assert_eq!(a, b);
        assert_eq!(
            (a.text(2), a.text(3), a.version, a.app_seq),
            (Some("hello"), Some("ready"), 3, 2)
        );
    }
    #[test]
    fn remove_replaces_the_root_atomically_and_reuses_ids() {
        let mut t = mounted();
        let label = |value| Node::Label { value };
        let create = |id, parent, node| Op::Create { id, parent, node };
        // Removing the label detaches it from its parent and deletes it.
        t.apply(Batch {
            base: 1,
            ops: vec![Op::Remove { id: 2 }],
        })
        .unwrap();
        assert_eq!((t.nodes.get(&2), t.children(1)), (None, &[3][..]));
        let before = t.clone();
        for (ops, err) in [
            (vec![Op::Remove { id: 2 }], "invalid_node"),
            (
                vec![Op::Remove { id: 1 }, Op::Remove { id: 3 }],
                "invalid_node",
            ),
            (
                vec![Op::Remove { id: 1 }, create(4, Some(3), label(1))],
                "invalid_parent",
            ),
            (
                vec![create(
                    9,
                    None,
                    Node::Column {
                        children: vec![],
                        style: Style::default(),
                    },
                )],
                "invalid_parent",
            ),
        ] {
            assert_eq!(t.clone().apply(Batch { base: 2, ops }), Err(err));
        }
        assert_eq!(
            t.clone().apply_ui(Batch {
                base: 0,
                ops: vec![Op::Remove { id: 1 }]
            }),
            Err("app_only_op")
        );
        assert_eq!(t, before);
        // Replacing the whole app in one batch: removed ids can be created again.
        let column = Node::Column {
            children: vec![],
            style: Style::default(),
        };
        t.apply(Batch {
            base: 2,
            ops: vec![
                Op::Remove { id: 1 },
                create(1, None, column),
                create(2, Some(1), label(7)),
            ],
        })
        .unwrap();
        assert_eq!(
            (t.root, t.children(1), t.nodes.get(&3)),
            (Some(1), &[2][..], None)
        );
        assert_eq!(t.nodes[&2], label(7));
    }
    #[test]
    fn removing_a_parent_also_removes_what_the_batch_created_under_it() {
        let column = || Node::Column {
            children: vec![],
            style: Style::default(),
        };
        let mut t = Tree::default();
        let create = |id, parent, node| Op::Create { id, parent, node };
        t.apply(Batch {
            base: 0,
            ops: vec![create(1, None, column()), create(2, Some(1), column())],
        })
        .unwrap();
        // 3 is created under 2, then 2 is removed: 3 goes too, so 4 cannot hang from it.
        let text = Node::Text {
            text: "orphan".into(),
            style: Style::default(),
            font: Font::default(),
            paint: Box::default(),
        };
        let before = t.clone();
        assert_eq!(
            t.apply(Batch {
                base: 1,
                ops: vec![
                    create(3, Some(2), column()),
                    Op::Remove { id: 2 },
                    create(4, Some(3), text)
                ]
            }),
            Err("invalid_parent")
        );
        assert_eq!(t, before);
        t.apply(Batch {
            base: 1,
            ops: vec![create(3, Some(2), column()), Op::Remove { id: 2 }],
        })
        .unwrap();
        let reachable = t.subtree(1);
        assert!(
            t.nodes.iter().all(|(id, _)| reachable.contains(&id)),
            "no orphans"
        );
    }
    #[test]
    fn invalid_styles_are_rejected() {
        let column = |style: &str| {
            serde_json::from_str::<Op>(&format!(
                r#"{{"op":"create","id":1,"node":{{"kind":"column","style":{style}}}}}"#
            ))
        };
        // A misspelled field fails serde; a negative value fails tree validation.
        assert!(column(r#"{"paddng":3}"#).is_err());
        let negative = column(r#"{"padding":-1}"#).unwrap();
        assert_eq!(
            Tree::default().apply(Batch {
                base: 0,
                ops: vec![negative]
            }),
            Err("invalid_style")
        );
        assert!(
            Tree::default()
                .apply(Batch {
                    base: 0,
                    ops: vec![column(r#"{"padding":4,"gap":2,"grow":1,"height":30}"#).unwrap()]
                })
                .is_ok()
        );
    }
    #[test]
    fn invalid_batches_leave_tree_and_version_intact() {
        let before = mounted();
        assert_eq!(
            before.nodes[&1],
            Node::Column {
                children: vec![2, 3],
                style: Style::default()
            }
        );
        let set = |id, value| Op::Set { id, value };
        let label = Node::Label { value: 0 };
        for (base, ops, err) in [
            (1, vec![set(2, 5), set(99, 1)], "invalid_node"),
            (1, vec![set(2, 5), set(3, 1)], "invalid_node"),
            (1, vec![set(2, 1000)], "value_out_of_range"),
            (0, vec![set(2, 5)], "stale_base"),
            (
                1,
                vec![Op::Create {
                    id: 4,
                    parent: None,
                    node: label.clone(),
                }],
                "invalid_parent",
            ),
            (
                1,
                vec![Op::Create {
                    id: 4,
                    parent: Some(2),
                    node: label.clone(),
                }],
                "invalid_parent",
            ),
            (
                1,
                vec![
                    set(2, 5),
                    Op::Create {
                        id: 3,
                        parent: Some(1),
                        node: label,
                    },
                ],
                "duplicate_id",
            ),
        ] {
            let mut t = before.clone();
            assert_eq!(t.apply(Batch { base, ops }), Err(err));
            assert_eq!(t, before);
        }
        let mut t = before;
        t.apply(Batch {
            base: 1,
            ops: vec![set(2, 7)],
        })
        .unwrap();
        assert_eq!(
            (t.nodes[&2].clone(), t.version),
            (Node::Label { value: 7 }, 2)
        );
    }
}
