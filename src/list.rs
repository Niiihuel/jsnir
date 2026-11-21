//! Weighted AVL sequence for Rust-owned virtual rows. Internal order labels
//! are independent of row keys and never require relabeling. The key index and each
//! tree path cost O(log n); window selection prunes nonintersecting subtrees.
use std::collections::BTreeMap;

type Order = i128;
type Link = Option<Box<Row>>;

struct Row {
    key: u64,
    order: Order,
    pixels: f64,
    total: f64,
    depth: i32,
    // Left (earlier) and right (later), indexed by direction in the AVL helpers.
    child: [Link; 2],
}

fn total(link: &Link) -> f64 {
    link.as_ref().map_or(0., |row| row.total)
}

fn depth(link: &Link) -> i32 {
    link.as_ref().map_or(0, |row| row.depth)
}

impl Row {
    fn refresh(&mut self) {
        self.total = total(&self.child[0]) + self.pixels + total(&self.child[1]);
        self.depth = 1 + depth(&self.child[0]).max(depth(&self.child[1]));
    }

    fn balance(&self) -> i32 {
        depth(&self.child[1]) - depth(&self.child[0])
    }

    // Promote the child on `side`, keeping the in-order sequence unchanged.
    fn promote(mut self: Box<Self>, side: usize) -> Box<Self> {
        let mut pivot = self.child[side].take().unwrap();
        self.child[side] = pivot.child[1 - side].take();
        self.refresh();
        pivot.child[1 - side] = Some(self);
        pivot.refresh();
        pivot
    }

    fn rebalance(mut self: Box<Self>) -> Box<Self> {
        let balance = self.balance();
        if balance.abs() <= 1 {
            self.refresh();
            return self;
        }
        let side = usize::from(balance > 0);
        let inner = self.child[side].as_ref().unwrap().balance();
        if inner * balance < 0 {
            self.child[side] = Some(self.child[side].take().unwrap().promote(1 - side));
        }
        self.promote(side)
    }

    fn insert(root: Link, row: Box<Self>) -> Box<Self> {
        let Some(mut root) = root else { return row };
        let side = usize::from(row.order > root.order);
        root.child[side] = Some(Self::insert(root.child[side].take(), row));
        root.rebalance()
    }

    fn update(&mut self, order: Order, pixels: f64) -> bool {
        let changed = if order == self.order {
            let changed = self.pixels != pixels;
            self.pixels = pixels;
            changed
        } else {
            let side = usize::from(order > self.order);
            self.child[side].as_mut().unwrap().update(order, pixels)
        };
        if changed {
            self.refresh();
        }
        changed
    }

    fn take_first(mut self: Box<Self>) -> (Box<Self>, Link) {
        let Some(left) = self.child[0].take() else {
            let rest = self.child[1].take();
            return (self, rest);
        };
        let (first, rest) = left.take_first();
        self.child[0] = rest;
        (first, Some(self.rebalance()))
    }

    fn remove(mut self: Box<Self>, order: Order) -> Link {
        if order != self.order {
            let side = usize::from(order > self.order);
            self.child[side] = self.child[side].take().unwrap().remove(order);
            return Some(self.rebalance());
        }
        let Some(right) = self.child[1].take() else {
            return self.child[0].take();
        };
        let (mut replacement, rest) = right.take_first();
        replacement.child = [self.child[0].take(), rest];
        Some(replacement.rebalance())
    }

    fn extreme(&self, side: usize) -> Order {
        self.child[side]
            .as_ref()
            .map_or(self.order, |child| child.extreme(side))
    }

    fn neighbor(&self, order: Order, after: bool) -> Option<Order> {
        let qualifies = if after {
            self.order > order
        } else {
            self.order < order
        };
        let side = usize::from(after != qualifies);
        self.child[side]
            .as_ref()
            .and_then(|child| child.neighbor(order, after))
            .or(qualifies.then_some(self.order))
    }

    // All queries share the same partition of the parent interval. Passing both
    // edges down keeps adjacent rows contiguous even when sums round differently.
    fn edges(&self, (start, end): (f64, f64)) -> (f64, f64) {
        let top = (start + total(&self.child[0])).min(end);
        let bottom = if self.child[1].is_none() {
            end
        } else {
            (top + self.pixels).min(end)
        };
        (top, bottom)
    }

    fn anchor(&self, span: (f64, f64), offset: f64) -> (Order, f64) {
        let (top, bottom) = self.edges(span);
        if offset < top {
            self.child[0]
                .as_ref()
                .unwrap()
                .anchor((span.0, top), offset)
        } else if offset >= bottom {
            self.child[1]
                .as_ref()
                .unwrap()
                .anchor((bottom, span.1), offset)
        } else {
            (self.order, offset - top)
        }
    }

    fn span_at(&self, span: (f64, f64), order: Order) -> (f64, f64) {
        let (top, bottom) = self.edges(span);
        if order < self.order {
            self.child[0]
                .as_ref()
                .unwrap()
                .span_at((span.0, top), order)
        } else if order > self.order {
            self.child[1]
                .as_ref()
                .unwrap()
                .span_at((bottom, span.1), order)
        } else {
            (top, bottom)
        }
    }

    fn collect(&self, span: (f64, f64), window: (f64, f64), rows: &mut Vec<(u64, f64, f64)>) {
        if span.0 >= span.1 || span.0 >= window.1 || span.1 <= window.0 {
            return;
        }
        let (top, bottom) = self.edges(span);
        if let Some(left) = &self.child[0] {
            left.collect((span.0, top), window, rows);
        }
        if top < bottom && top < window.1 && bottom > window.0 {
            rows.push((self.key, top, self.pixels));
        }
        if let Some(right) = &self.child[1] {
            right.collect((bottom, span.1), window, rows);
        }
    }
}

#[derive(Default)]
pub(crate) struct List {
    root: Link,
    keys: BTreeMap<u64, Order>,
    offset: f64,
    extent: f64,
}

impl List {
    pub(crate) fn append(&mut self, key: u64, height: f32) -> bool {
        self.insert(key, height, 1)
    }

    pub(crate) fn prepend(&mut self, key: u64, height: f32) -> bool {
        self.insert(key, height, 0)
    }

    fn insert(&mut self, key: u64, height: f32, side: usize) -> bool {
        if !valid_height(height) || self.keys.contains_key(&key) {
            return false;
        }
        let order = self.root.as_ref().map_or(Some(0), |root| {
            root.extreme(side)
                .checked_add(if side == 0 { -1 } else { 1 })
        });
        let Some(order) = order else { return false };
        let anchor = self.anchor();
        let pixels = f64::from(height);
        self.root = Some(Row::insert(
            self.root.take(),
            Box::new(Row {
                key,
                order,
                pixels,
                total: pixels,
                depth: 1,
                child: [None, None],
            }),
        ));
        self.keys.insert(key, order);
        self.restore(anchor);
        true
    }

    /// `None` rejects the edit; otherwise returns whether the geometry changed.
    pub(crate) fn update_height(&mut self, key: u64, height: f32) -> Option<bool> {
        let &order = self.keys.get(&key)?;
        if !valid_height(height) {
            return None;
        }
        let anchor = self.anchor();
        let changed = self.root.as_mut().unwrap().update(order, f64::from(height));
        if changed {
            self.restore(anchor);
        }
        Some(changed)
    }

    pub(crate) fn remove(&mut self, key: u64) -> bool {
        let Some(order) = self.keys.remove(&key) else {
            return false;
        };
        let anchor = self.anchor();
        self.root = self.root.take().unwrap().remove(order);
        let anchor = anchor.and_then(|(saved, intra)| {
            if saved != order {
                return Some((saved, intra));
            }
            // Prefer the next row's start, then the previous row's start.
            let root = self.root.as_ref()?;
            root.neighbor(order, true)
                .or_else(|| root.neighbor(order, false))
                .map(|survivor| (survivor, 0.))
        });
        self.restore(anchor);
        true
    }

    pub(crate) fn total(&self) -> f64 {
        total(&self.root)
    }

    pub(crate) fn offset(&self) -> f64 {
        self.offset
    }

    pub(crate) fn extent(&self) -> f64 {
        self.extent
    }

    /// A finite offset is clamped into [0, max(content height - extent, 0)].
    pub(crate) fn set_viewport(&mut self, offset: f64, extent: f64) -> bool {
        if !offset.is_finite() || !extent.is_finite() || extent < 0. {
            return false;
        }
        self.extent = extent;
        self.offset = self.clamp(offset);
        true
    }

    /// (key, content-relative top, logical height) for rows intersecting the
    /// half-open viewport. A zero extent has no visible rows.
    pub(crate) fn visible(&self) -> Vec<(u64, f64, f64)> {
        self.visible_within(self.extent)
    }

    pub(crate) fn visible_within(&self, limit: f64) -> Vec<(u64, f64, f64)> {
        let mut rows = Vec::new();
        let end = self.offset + self.extent.min(limit);
        if end > self.offset
            && let Some(root) = &self.root
        {
            root.collect((0., root.total), (self.offset, end), &mut rows);
        }
        rows
    }

    pub(crate) fn contains(&self, key: u64) -> bool {
        self.keys.contains_key(&key)
    }

    pub(crate) fn intersects(&self, key: u64, limit: f64) -> bool {
        let end = self.offset + self.extent.min(limit);
        self.keys
            .get(&key)
            .zip(self.root.as_ref())
            .is_some_and(|(&order, root)| {
                let (top, bottom) = root.span_at((0., root.total), order);
                end > self.offset && top < bottom && top < end && bottom > self.offset
            })
    }

    fn clamp(&self, offset: f64) -> f64 {
        offset.max(0.).min((self.total() - self.extent).max(0.))
    }

    fn anchor(&self) -> Option<(Order, f64)> {
        let root = self.root.as_ref()?;
        (self.offset < root.total).then(|| root.anchor((0., root.total), self.offset))
    }

    fn restore(&mut self, anchor: Option<(Order, f64)>) {
        let offset = match (anchor, &self.root) {
            (Some((order, intra)), Some(root)) => {
                let (top, bottom) = root.span_at((0., root.total), order);
                if top + intra < bottom {
                    top + intra
                } else {
                    top
                }
            }
            _ => self.offset,
        };
        self.offset = self.clamp(offset);
    }
}

// Accumulate f32 logical heights in f64: even the largest possible collection
// of finite f32 heights on an addressable machine cannot overflow the sum.
fn valid_height(height: f32) -> bool {
    height.is_finite() && height > 0.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fractional_window_is_half_open_and_clamped() {
        let mut list = List::default();
        assert!(list.append(30, 1.25));
        assert!(list.prepend(40, 2.5));
        assert!(list.append(20, 4.));
        assert_eq!(list.total(), 7.75);
        assert!(list.set_viewport(2.5, 1.25));
        assert_eq!(list.visible(), [(30, 2.5, 1.25)]);
        assert!(list.set_viewport(2.25, 1.5));
        assert_eq!(list.visible(), [(40, 0., 2.5), (30, 2.5, 1.25)]);
        assert!(list.set_viewport(100., 2.));
        assert_eq!(list.offset, 5.75);
        assert_eq!(list.visible(), [(20, 3.75, 4.)]);
        assert!(list.set_viewport(-1., 0.));
        assert_eq!(list.offset, 0.);
        assert!(list.visible().is_empty());
    }

    #[test]
    fn keeps_variable_height_anchor_and_chooses_survivors() {
        let mut list = List::default();
        assert!(list.append(1, 2.));
        assert!(list.append(2, 4.));
        assert!(list.append(3, 5.));
        assert!(list.set_viewport(3.25, 1.));
        assert!(list.prepend(4, 1.5));
        assert_eq!(list.offset, 4.75);
        assert_eq!(list.update_height(1, 3.), Some(true));
        assert_eq!(list.offset, 5.75);
        assert!(list.remove(4));
        assert_eq!(list.offset, 4.25);
        assert_eq!(list.visible(), [(2, 3., 4.)]);
        assert!(list.remove(2));
        assert_eq!(list.offset, 3.);
        assert_eq!(list.visible(), [(3, 3., 5.)]);
        assert!(list.remove(3));
        assert_eq!(list.offset, 0.);
        assert_eq!(list.visible(), [(1, 0., 3.)]);
    }

    #[test]
    fn handles_empty_and_shrinking_content() {
        let mut list = List::default();
        assert!(list.set_viewport(20., 3.));
        assert!(list.visible().is_empty());
        assert_eq!(list.offset, 0.);
        assert!(list.append(1, 4.));
        assert!(list.append(2, 2.));
        assert!(list.append(3, 5.));
        assert!(list.set_viewport(5.5, 3.));
        assert_eq!(list.update_height(2, 1.), Some(true));
        assert_eq!(list.offset, 4.); // The original intra-row point no longer exists.
        assert!(list.remove(3));
        assert_eq!(list.offset, 2.);
        assert_eq!(list.visible(), [(1, 0., 4.), (2, 4., 1.)]);
        assert!(list.remove(1));
        assert_eq!(list.offset, 0.);
        assert!(list.remove(2));
        assert_eq!(list.total(), 0.);
        assert!(list.visible().is_empty());
    }

    #[test]
    fn invalid_inputs_do_not_change_the_view_or_index() {
        let mut list = List::default();
        assert!(list.append(1, 2.));
        assert!(list.set_viewport(0.5, 1.));
        for invalid in [0., -1., f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(!list.append(2, invalid));
            assert!(!list.prepend(2, invalid));
            assert_eq!(list.update_height(1, invalid), None);
        }
        assert!(!list.append(1, 10.));
        assert!(!list.prepend(1, 10.));
        assert_eq!(list.update_height(2, 10.), None);
        assert!(!list.remove(2));
        for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(!list.set_viewport(invalid, 1.));
            assert!(!list.set_viewport(0., invalid));
        }
        assert!(!list.set_viewport(0., -1.));
        assert_eq!(list.total(), 2.);
        assert_eq!((list.offset, list.visible()), (0.5, vec![(1, 0., 2.)]));
        assert!(list.append(2, 1.));
    }

    fn verify_tree(list: &List) {
        fn visit(
            link: &Link,
            keys: &BTreeMap<u64, Order>,
            bounds: (Order, Order),
        ) -> (i32, f64, usize) {
            let Some(row) = link else { return (0, 0., 0) };
            assert!(bounds.0 < row.order && row.order < bounds.1);
            assert_eq!(keys.get(&row.key), Some(&row.order));
            let (ld, lt, ln) = visit(&row.child[0], keys, (bounds.0, row.order));
            let (rd, rt, rn) = visit(&row.child[1], keys, (row.order, bounds.1));
            assert!((ld - rd).abs() <= 1);
            assert_eq!(row.depth, 1 + ld.max(rd));
            assert_eq!(row.total, lt + row.pixels + rt);
            (row.depth, row.total, 1 + ln + rn)
        }
        let (depth, _, count) = visit(&list.root, &list.keys, (Order::MIN, Order::MAX));
        assert_eq!(count, list.keys.len());
        assert!(depth <= 20, "unexpected AVL depth: {depth}");
    }

    #[test]
    fn deep_history_supports_key_edits_and_small_windows() {
        let mut list = List::default();
        for key in 0..5_000 {
            assert!(list.append(key, 1.));
            assert!(list.prepend(key + 5_000, 1.));
        }
        assert!(list.set_viewport(7_500.5, 3.));
        assert_eq!(list.update_height(10, 3.), Some(true));
        assert!(list.remove(9_000));
        assert!(list.prepend(20_000, 0.25));
        assert_eq!(list.total(), 10_001.25);
        assert_eq!(list.offset, 7_501.75);
        assert_eq!(
            list.visible(),
            [2500, 2501, 2502, 2503].map(|key| (key, 7501.25 + (key - 2500) as f64, 1.))
        );
        verify_tree(&list);
        for key in (0..10_000).step_by(3).filter(|&key| key != 9_000) {
            assert!(list.remove(key));
        }
        verify_tree(&list);
    }
}
