//! Layout with Taffy (flexbox). It runs on the UI thread, which owns the tree and the window size;
//! changed rectangles leave as a UI batch (`setrect`) and reach JS through the `x`, `y`, `width` and
//! `height` columns.
//!
//! List items do not go through Taffy: the list is a leaf that lays out its own rows, and the backend
//! draws only the visible ones. Per Taffy's benchmark, layout costs ≈ 0.25–0.47 µs per node; 10,000
//! rows would take more than half a frame at 120 Hz.
use crate::{
    text::{TextEngine, TextStyle, WRAP_SLACK},
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

fn taffy_style(node: &Node) -> taffy::Style {
    let mut out = taffy::Style::default();
    if let Some(s) = node.style() {
        let side = |v: Option<f32>| v.map_or(Dimension::auto(), length);
        out.size = Size {
            width: side(s.width),
            height: side(s.height),
        };
        if let Node::Icon { size, .. } = node {
            out.size = Size {
                width: side(s.width.or(Some(*size))),
                height: side(s.height.or(Some(*size))),
            };
        }
        out.flex_grow = s.grow;
        out.flex_shrink = s.shrink.unwrap_or(1.);
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
        if s.clip {
            // As CSS `overflow: hidden`: the node may shrink below its content.
            out.overflow = taffy::Point {
                x: taffy::Overflow::Hidden,
                y: taffy::Overflow::Hidden,
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
        let style = taffy_style(node);
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
            Some(p) => self.taffy.add_child(p, t).expect("taffy: child"),
            None => self.root = Some(id),
        }
    }
    /// Lays out a whole tree, for when its structure changed (for example, after an app reload).
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
    /// The node's style changed.
    pub fn restyled(&mut self, tree: &Tree, id: u16) {
        if let (Some(node), Some(t)) = (tree.nodes.get(&id), self.get(id)) {
            let mut style = taffy_style(node);
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
        let space = Size {
            width: AvailableSpace::Definite(width),
            height: AvailableSpace::Definite(height),
        };
        self.taffy
            .compute_layout_with_measure(root_node, space, |input, _, id, style| {
                taffy::compute_leaf_layout(
                    input,
                    style,
                    |_, _| 0.,
                    |known, available| {
                        let Some(Node::Text {
                            text: value, font, ..
                        }) = id.as_ref().and_then(|id| tree.nodes.get(id))
                        else {
                            return Size::ZERO;
                        };
                        let width = known.width.or(match available.width {
                            AvailableSpace::Definite(width) => Some(width),
                            AvailableSpace::MinContent => Some(1.),
                            AvailableSpace::MaxContent => None,
                        });
                        // A hair of slack, so a line laid out in exactly its measured width never wraps on rounding.
                        let [width, height] = text.measure_styled(
                            value,
                            width.map(|w| w + WRAP_SLACK),
                            TextStyle::resolve(font),
                        );
                        Size { width, height }
                    },
                )
            })
            .expect("taffy: layout");
        // Taffy gives each position relative to the parent: accumulate from the root.
        let mut rects: HashMap<u16, [f32; 4]> = HashMap::new();
        let mut follows = Vec::new();
        let mut stack = vec![(root, 0.0, 0.0)];
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
            if !matches!(tree.nodes.get(&id), Some(Node::List { .. })) {
                stack.extend(
                    tree.children(id)
                        .iter()
                        .filter(|c| self.get(**c).is_some())
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
        let mut changed: Vec<(u16, [f32; 4])> = rects
            .into_iter()
            .filter(|(id, rect)| tree.rect(*id) != *rect)
            .collect();
        changed.sort_unstable_by_key(|(id, _)| *id);
        changed
    }
}
