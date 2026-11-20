//! Layout with Taffy (flexbox). It runs on the UI thread, which owns the tree and the window size;
//! changed rectangles leave as a UI batch (`setrect`) and reach JS through the `x`, `y`, `width` and
//! `height` columns.
//!
//! List items do not go through Taffy: the list is a leaf that lays out its own rows, and the backend
//! draws only the visible ones. Per Taffy's benchmark, layout costs ≈ 0.25–0.47 µs per node; 10,000
//! rows would take more than half a frame at 120 Hz.
use crate::{
    text::{TextEngine, TextStyle, WRAP_SLACK, node_spans},
    tree::{Align, Axis, Justify, Node, Position, Tree},
};
use std::collections::HashMap;
use taffy::prelude::{
    AvailableSpace, Dimension, FlexDirection, LengthPercentage, LengthPercentageAuto, NodeId, Rect,
    Size, TaffyTree, auto, length,
};
use taffy::style::{AlignItems, JustifyContent};

pub struct Layout {
    taffy: TaffyTree<u16>,
    // Taffy node of each JSNIR id that takes part in the layout.
    nodes: Vec<Option<NodeId>>,
    root: Option<u16>,
    viewport: Option<(f32, f32)>,
    // Heights currently pinned by an animation, so they can be released when it ends.
    pinned: HashMap<u16, f32>,
    // Content height of each scroll node in the last computed layout.
    content: HashMap<u16, f32>,
}
impl Default for Layout {
    fn default() -> Self {
        let mut taffy = TaffyTree::new();
        // Fractional layout, as browsers do: rounding to whole pixels would make a text box narrower
        // than its measured text and wrap the last glyph. Backends snap bitmaps to device pixels.
        taffy.disable_rounding();
        Self {
            taffy,
            nodes: Vec::new(),
            root: None,
            viewport: None,
            pinned: HashMap::new(),
            content: HashMap::new(),
        }
    }
}

fn edges<T>(sides: [f32; 4], make: impl Fn(f32) -> T) -> Rect<T> {
    let [top, right, bottom, left] = sides.map(make);
    Rect {
        left,
        right,
        top,
        bottom,
    }
}

// Children of a scroll node keep their natural size: Taffy would shrink them to fit the box.
fn in_scroll(tree: &Tree, id: u16) -> bool {
    tree.parent(id)
        .and_then(|p| tree.nodes.get(&p))
        .and_then(Node::style)
        .is_some_and(|s| s.scroll)
}

// A fixed node is a Taffy root of its own: it is not a child of its parent's Taffy node, so the parent's
// layout ignores it, and it is laid out in `compute` at the size its own style and content give it.
fn is_fixed(node: &Node) -> bool {
    node.style().is_some_and(|s| s.position == Position::Fixed)
}
fn detached(tree: &Tree, id: u16) -> bool {
    tree.nodes.get(&id).is_some_and(is_fixed)
}

fn taffy_style(node: &Node, in_scroll: bool) -> taffy::Style {
    let mut out = taffy::Style::default();
    if let Some(s) = node.style() {
        let side = |v: Option<f32>| v.map_or(Dimension::auto(), length);
        out.size = Size {
            width: side(s.width),
            height: side(s.height),
        };
        let limit = |v: Option<f32>| v.map_or(auto(), LengthPercentageAuto::length);
        out.min_size = Size {
            width: limit(s.min_width),
            height: limit(s.min_height),
        };
        out.max_size = Size {
            width: limit(s.max_width),
            height: limit(s.max_height),
        };
        if let Node::Icon { size, .. } = node {
            out.size = Size {
                width: side(s.width.or(Some(*size))),
                height: side(s.height.or(Some(*size))),
            };
        }
        out.flex_grow = s.grow;
        out.flex_shrink = if in_scroll {
            0.
        } else {
            s.shrink.unwrap_or(1.)
        };
        out.flex_basis = side(s.basis);
        out.padding = edges(s.padding.sides(), LengthPercentage::length);
        out.margin = edges(s.margin.sides(), LengthPercentageAuto::length);
        out.gap = Size {
            width: length(s.gap),
            height: length(s.gap),
        };
        out.align_items = s.align.map_or(AlignItems::NORMAL, |a| match a {
            Align::Stretch => AlignItems::STRETCH,
            Align::Start => AlignItems::FLEX_START,
            Align::Center => AlignItems::CENTER,
            Align::End => AlignItems::FLEX_END,
        });
        out.justify_content = match s.justify {
            Justify::Start => JustifyContent::FLEX_START,
            Justify::Center => JustifyContent::CENTER,
            Justify::End => JustifyContent::FLEX_END,
            Justify::Between => JustifyContent::SPACE_BETWEEN,
        };
        if s.position == Position::Absolute {
            out.position = taffy::Position::Absolute;
            out.inset = Rect {
                left: s.left.map_or(auto(), LengthPercentageAuto::length),
                top: s.top.map_or(auto(), LengthPercentageAuto::length),
                right: auto(),
                bottom: auto(),
            };
        }
        if s.clip || s.scroll {
            // As CSS `overflow: hidden`: the node may shrink below its content. A scroll node scrolls vertically.
            out.overflow = taffy::Point {
                x: taffy::Overflow::Hidden,
                y: if s.scroll {
                    taffy::Overflow::Scroll
                } else {
                    taffy::Overflow::Hidden
                },
            };
        }
        out.flex_direction = match (node, s.direction) {
            (Node::Box { .. }, Axis::Row) => FlexDirection::Row,
            _ => FlexDirection::Column,
        };
    }
    out
}
impl Layout {
    fn get(&self, id: u16) -> Option<NodeId> {
        *self.nodes.get(id as usize)?
    }
    /// Adds a newly created node to the layout. Descendants of a list stay out.
    pub fn created(&mut self, tree: &Tree, id: u16, parent: Option<u16>) {
        let Some(node) = tree.nodes.get(&id) else {
            return;
        };
        let parent = match parent {
            None => None,
            Some(p) if matches!(tree.nodes.get(&p), Some(Node::List { .. })) => return,
            Some(p) => match self.get(p) {
                Some(t) => Some(t),
                None => return,
            },
        };
        let style = taffy_style(node, in_scroll(tree, id));
        let t = if matches!(node, Node::Text { .. }) {
            self.taffy.new_leaf_with_context(style, id)
        } else {
            self.taffy.new_leaf(style)
        }
        .expect("taffy: node");
        let i = id as usize;
        if self.nodes.len() <= i {
            self.nodes.resize(i + 1, None);
        }
        self.nodes[i] = Some(t);
        match parent {
            Some(_) if is_fixed(node) => {}
            Some(p) => self.taffy.add_child(p, t).expect("taffy: child"),
            None => self.root = Some(id),
        }
    }
    /// Lays out a whole tree, for when its structure changed (for example, after an app reload).
    #[cfg(feature = "examples")]
    pub fn from_tree(tree: &Tree) -> Self {
        let mut layout = Self::default();
        let mut stack: Vec<(u16, Option<u16>)> = tree.root.map(|r| (r, None)).into_iter().collect();
        while let Some((id, parent)) = stack.pop() {
            layout.created(tree, id, parent);
            if !matches!(tree.nodes.get(&id), Some(Node::List { .. })) {
                stack.extend(tree.children(id).iter().rev().map(|&c| (c, Some(id))));
            }
        }
        layout
    }
    /// Removes nodes that left the tree. Pass the whole removed subtree.
    pub fn removed(&mut self, ids: &[u16]) {
        for &id in ids {
            if let Some(node) = self.nodes.get_mut(id as usize).and_then(Option::take) {
                let _ = self.taffy.remove(node);
            }
            self.pinned.remove(&id);
            if self.root == Some(id) {
                self.root = None;
                self.viewport = None;
            }
        }
    }
    /// The children of `parent` changed order: Taffy takes the tree's order. Lists keep their rows out of Taffy.
    pub fn reordered(&mut self, tree: &Tree, parent: u16) {
        let Some(t) = self.get(parent) else {
            return;
        };
        if matches!(tree.nodes.get(&parent), Some(Node::List { .. })) {
            return;
        }
        let children: Vec<NodeId> = tree
            .children(parent)
            .iter()
            .filter(|c| !detached(tree, **c))
            .filter_map(|c| self.get(*c))
            .collect();
        self.taffy
            .set_children(t, &children)
            .expect("taffy: children");
    }
    /// The node's style changed. A scroll node's children change with it: their flex-shrink depends on it.
    pub fn restyled(&mut self, tree: &Tree, id: u16) {
        self.restyle(tree, id);
        if tree
            .nodes
            .get(&id)
            .and_then(Node::style)
            .is_some_and(|s| s.scroll)
        {
            for &child in tree.children(id) {
                self.restyle(tree, child);
            }
        }
    }
    fn restyle(&mut self, tree: &Tree, id: u16) {
        if let (Some(node), Some(t)) = (tree.nodes.get(&id), self.get(id)) {
            let mut style = taffy_style(node, in_scroll(tree, id));
            if Some(id) == self.root
                && let Some((width, height)) = self.viewport
            {
                style.size = Size {
                    width: length(width),
                    height: length(height),
                };
            }
            self.taffy.set_style(t, style).expect("taffy: style");
            self.pinned.remove(&id);
        }
    }
    pub(crate) fn text_changed(&mut self, id: u16) {
        if let Some(node) = self.get(id) {
            self.taffy.mark_dirty(node).expect("taffy: text node");
        }
    }
    /// Pins the heights of animating nodes; nodes missing from `heights` go back to their own style.
    pub fn pin_heights(&mut self, tree: &Tree, heights: &HashMap<u16, f32>) {
        let released: Vec<u16> = self
            .pinned
            .keys()
            .filter(|id| !heights.contains_key(id))
            .copied()
            .collect();
        for id in released {
            self.restyled(tree, id);
        }
        for (&id, &height) in heights {
            if self.pinned.get(&id) == Some(&height) {
                continue;
            }
            if let Some(t) = self.get(id) {
                let mut style = self.taffy.style(t).expect("taffy: style").clone();
                style.size.height = length(height);
                self.taffy.set_style(t, style).expect("taffy: style");
                self.pinned.insert(id, height);
            }
        }
    }
    /// Content height of a scroll node in the last layout, in logical pixels.
    pub fn content_height(&self, id: u16) -> f32 {
        self.content.get(&id).copied().unwrap_or(0.)
    }
    // The fixed nodes below `root`, at any depth: each one is a Taffy root. Lists keep their rows out of the layout.
    fn fixed_nodes(&self, tree: &Tree, root: u16) -> Vec<u16> {
        let mut out = Vec::new();
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            if id != root && detached(tree, id) && self.get(id).is_some() {
                out.push(id);
            }
            if !matches!(tree.nodes.get(&id), Some(Node::List { .. })) {
                stack.extend(tree.children(id));
            }
        }
        out
    }
    /// Computes the layout for the window size and returns the absolute rectangles that changed.
    /// Taffy caches per node: if nothing changed, the previous result is reused.
    pub fn compute(
        &mut self,
        tree: &Tree,
        text: &mut TextEngine,
        width: f32,
        height: f32,
    ) -> Vec<(u16, [f32; 4])> {
        let Some(root) = self.root else {
            return Vec::new();
        };
        let root_node = self.get(root).expect("root in the layout");
        if self.viewport != Some((width, height)) {
            // The root fills the window: its size is set here, not in the app's style.
            let mut style = self.taffy.style(root_node).expect("taffy: style").clone();
            style.size = Size {
                width: length(width),
                height: length(height),
            };
            self.taffy
                .set_style(root_node, style)
                .expect("taffy: style");
            self.viewport = Some((width, height));
        }
        let window = Size {
            width: AvailableSpace::Definite(width),
            height: AvailableSpace::Definite(height),
        };
        // A fixed node takes its own style's size, or its content's (max-content) where the style sets none.
        let fixed = self.fixed_nodes(tree, root);
        let mut roots = vec![(root_node, window)];
        for &id in &fixed {
            let style = tree.nodes.get(&id).and_then(Node::style);
            let side =
                |v: Option<f32>| v.map_or(AvailableSpace::MaxContent, AvailableSpace::Definite);
            let space = Size {
                width: side(style.and_then(|s| s.width)),
                height: side(style.and_then(|s| s.height)),
            };
            roots.push((self.get(id).expect("fixed node in the layout"), space));
        }
        for (node, space) in roots {
            self.taffy
                .compute_layout_with_measure(node, space, |input, _, id, style| {
                    taffy::compute_leaf_layout(
                        input,
                        style,
                        |_, _| 0.,
                        |known, available| {
                            let Some(Node::Text {
                                text: value,
                                font,
                                lines,
                                spans,
                                ..
                            }) = id.as_ref().and_then(|id| tree.nodes.get(id))
                            else {
                                return Size::ZERO;
                            };
                            let width = known.width.or(match available.width {
                                AvailableSpace::Definite(width) => Some(width),
                                AvailableSpace::MinContent => Some(1.),
                                AvailableSpace::MaxContent => None,
                            });
                            let style = TextStyle::resolve(font);
                            // Measures the text the node draws: cut to its line limit at this width.
                            let runs = node_spans(value, spans.as_deref());
                            let cut;
                            let runs = match (lines, width) {
                                (Some(lines), Some(width)) => {
                                    cut = text.limit_lines(&runs, width, *lines, &style);
                                    cut.as_slice()
                                }
                                _ => &runs[..],
                            };
                            // A hair of slack, so a line laid out in exactly its measured width never wraps on rounding.
                            let [width, height] =
                                text.measure_styled(runs, width.map(|w| w + WRAP_SLACK), &style);
                            Size { width, height }
                        },
                    )
                })
                .expect("taffy: layout");
        }
        // Taffy gives each position relative to the parent: accumulate from each root.
        let mut rects: HashMap<u16, [f32; 4]> = HashMap::new();
        let mut follows = Vec::new();
        // Scroll nodes, with their bottom padding: their content height is read below.
        let mut scrolls = Vec::new();
        let mut stack: Vec<(u16, f32, f32)> = std::iter::once(root)
            .chain(fixed)
            .map(|id| (id, 0., 0.))
            .collect();
        while let Some((id, px, py)) = stack.pop() {
            let l = self
                .taffy
                .layout(self.get(id).expect("node in the layout"))
                .expect("taffy: result");
            let rect = [
                px + l.location.x,
                py + l.location.y,
                l.size.width,
                l.size.height,
            ];
            rects.insert(id, rect);
            if let Some(target) = tree
                .nodes
                .get(&id)
                .and_then(Node::style)
                .and_then(|s| s.follow)
            {
                follows.push((id, target));
            }
            if let Some(s) = tree
                .nodes
                .get(&id)
                .and_then(Node::style)
                .filter(|s| s.scroll)
            {
                scrolls.push((id, s.padding.sides()[2]));
            }
            if !matches!(tree.nodes.get(&id), Some(Node::List { .. })) {
                stack.extend(
                    tree.children(id)
                        .iter()
                        .filter(|c| self.get(**c).is_some() && !detached(tree, **c))
                        .map(|&c| (c, rect[0], rect[1])),
                );
            }
        }
        // A following node takes its target's rectangle.
        for (id, target) in follows {
            if let Some(&rect) = rects.get(&target) {
                rects.insert(id, rect);
            }
        }
        // Content height: from the top of the scroll node to its lowest child, plus the bottom padding.
        self.content.clear();
        for (id, padding) in scrolls {
            let top = rects[&id][1];
            let bottom = tree
                .children(id)
                .iter()
                .filter(|c| !detached(tree, **c))
                .filter_map(|c| rects.get(c))
                .map(|r| r[1] + r[3])
                .fold(top, f32::max);
            self.content.insert(id, bottom - top + padding);
        }
        let mut changed: Vec<(u16, [f32; 4])> = rects
            .into_iter()
            .filter(|(id, rect)| tree.rect(*id) != *rect)
            .collect();
        changed.sort_unstable_by_key(|(id, _)| *id);
        changed
    }
}
