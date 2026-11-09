//! Split layout as a binary tree, generic over the leaf so it's tested without a GPU.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    /// Children side by side (vertical divider).
    Row,
    /// Children stacked (horizontal divider).
    Column,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const UNIT: Rect = Rect { x: 0.0, y: 0.0, w: 1.0, h: 1.0 };

    fn split(self, axis: Axis, ratio: f32) -> (Rect, Rect) {
        match axis {
            Axis::Row => {
                let w = self.w * ratio;
                (Rect { w, ..self }, Rect { x: self.x + w, w: self.w - w, ..self })
            }
            Axis::Column => {
                let h = self.h * ratio;
                (Rect { h, ..self }, Rect { y: self.y + h, h: self.h - h, ..self })
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Node<T> {
    Leaf(T),
    Split { axis: Axis, ratio: f32, a: Box<Node<T>>, b: Box<Node<T>> },
}

const MIN_RATIO: f32 = 0.1;

impl<T: Copy + PartialEq> Node<T> {
    pub fn leaves(&self) -> Vec<T> {
        let mut v = Vec::new();
        self.collect(&mut v);
        v
    }

    fn collect(&self, v: &mut Vec<T>) {
        match self {
            Node::Leaf(t) => v.push(*t),
            Node::Split { a, b, .. } => {
                a.collect(v);
                b.collect(v);
            }
        }
    }

    pub fn contains(&self, t: T) -> bool {
        match self {
            Node::Leaf(x) => *x == t,
            Node::Split { a, b, .. } => a.contains(t) || b.contains(t),
        }
    }

    /// Split `target` in two, putting `new` after it. Returns false if not found.
    pub fn split(&mut self, target: T, new: T, axis: Axis) -> bool {
        match self {
            Node::Leaf(x) if *x == target => {
                *self = Node::Split { axis, ratio: 0.5, a: Box::new(Node::Leaf(target)), b: Box::new(Node::Leaf(new)) };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { a, b, .. } => a.split(target, new, axis) || b.split(target, new, axis),
        }
    }

    /// Remove `target`; its sibling takes the space. `None` if the tree is now empty.
    pub fn remove(self, target: T) -> Option<Node<T>> {
        match self {
            Node::Leaf(x) if x == target => None,
            leaf @ Node::Leaf(_) => Some(leaf),
            Node::Split { axis, ratio, a, b } => match (a.remove(target), b.remove(target)) {
                (Some(a), Some(b)) => Some(Node::Split { axis, ratio, a: Box::new(a), b: Box::new(b) }),
                (Some(only), None) | (None, Some(only)) => Some(only),
                (None, None) => None,
            },
        }
    }

    /// Leaf rectangles inside `area`.
    pub fn layout(&self, area: Rect) -> Vec<(T, Rect)> {
        let mut v = Vec::new();
        self.layout_into(area, &mut v);
        v
    }

    fn layout_into(&self, area: Rect, v: &mut Vec<(T, Rect)>) {
        match self {
            Node::Leaf(t) => v.push((*t, area)),
            Node::Split { axis, ratio, a, b } => {
                let (ra, rb) = area.split(*axis, *ratio);
                a.layout_into(ra, v);
                b.layout_into(rb, v);
            }
        }
    }

    /// The closest pane in direction `dir` from `from` that overlaps it on the other axis.
    pub fn neighbor(&self, from: T, dir: Dir) -> Option<T> {
        let rects = self.layout(Rect::UNIT);
        let (_, r) = *rects.iter().find(|(t, _)| *t == from)?;
        const EPS: f32 = 1e-4;
        let overlap = |a0: f32, a1: f32, b0: f32, b1: f32| a0.max(b0) < a1.min(b1) - EPS;
        rects
            .iter()
            .filter(|(t, _)| *t != from)
            .filter_map(|(t, o)| {
                let (ok, dist) = match dir {
                    Dir::Left => (o.x + o.w <= r.x + EPS && overlap(r.y, r.y + r.h, o.y, o.y + o.h), r.x - (o.x + o.w)),
                    Dir::Right => {
                        (o.x >= r.x + r.w - EPS && overlap(r.y, r.y + r.h, o.y, o.y + o.h), o.x - (r.x + r.w))
                    }
                    Dir::Up => (o.y + o.h <= r.y + EPS && overlap(r.x, r.x + r.w, o.x, o.x + o.w), r.y - (o.y + o.h)),
                    Dir::Down => (o.y >= r.y + r.h - EPS && overlap(r.x, r.x + r.w, o.x, o.x + o.w), o.y - (r.y + r.h)),
                };
                // Prefer the nearest; among equals, the one most aligned with our top/left.
                let align = match dir {
                    Dir::Left | Dir::Right => (o.y - r.y).abs(),
                    Dir::Up | Dir::Down => (o.x - r.x).abs(),
                };
                ok.then_some((*t, dist, align))
            })
            .min_by(|a, b| (a.1, a.2).partial_cmp(&(b.1, b.2)).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(t, _, _)| t)
    }

    /// The split at `path` (false = first child, true = second): its axis, ratio and
    /// the rectangle it occupies inside `area`.
    pub fn split_at(&mut self, path: &[bool], area: Rect) -> Option<(Axis, &mut f32, Rect)> {
        match self {
            Node::Leaf(_) => None,
            Node::Split { axis, ratio, a, b } => match path.split_first() {
                None => Some((*axis, ratio, area)),
                Some((&second, rest)) => {
                    let (ra, rb) = area.split(*axis, *ratio);
                    if second { b.split_at(rest, rb) } else { a.split_at(rest, ra) }
                }
            },
        }
    }

    /// Set the ratio of the split at `path` so its divider sits at `pos` (same units as `area`).
    pub fn drag_divider(&mut self, path: &[bool], area: Rect, pos: (f32, f32)) -> bool {
        let Some((axis, ratio, r)) = self.split_at(path, area) else { return false };
        let frac = match axis {
            Axis::Row => (pos.0 - r.x) / r.w,
            Axis::Column => (pos.1 - r.y) / r.h,
        };
        if !frac.is_finite() {
            return false;
        }
        *ratio = frac.clamp(MIN_RATIO, 1.0 - MIN_RATIO);
        true
    }

    /// Grow (`delta > 0`) or shrink the pane `target` along the nearest split of `axis`
    /// that contains it. Returns false if there's no such split.
    pub fn resize(&mut self, target: T, axis: Axis, delta: f32) -> bool {
        match self {
            Node::Leaf(_) => false,
            Node::Split { axis: ax, ratio, a, b } => {
                let in_a = a.contains(target);
                if !in_a && !b.contains(target) {
                    return false;
                }
                // Deepest matching split wins.
                let child = if in_a { a.as_mut() } else { b.as_mut() };
                if child.resize(target, axis, delta) {
                    return true;
                }
                if *ax != axis {
                    return false;
                }
                let d = if in_a { delta } else { -delta };
                *ratio = (*ratio + d).clamp(MIN_RATIO, 1.0 - MIN_RATIO);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> Node<u32> {
        // ┌───┬───┐
        // │ 1 │ 2 │
        // │   ├───┤
        // │   │ 3 │
        // └───┴───┘
        let mut t = Node::Leaf(1);
        assert!(t.split(1, 2, Axis::Row));
        assert!(t.split(2, 3, Axis::Column));
        t
    }

    #[test]
    fn split_and_order() {
        let t = tree();
        assert_eq!(t.leaves(), [1, 2, 3]);
        assert!(!tree().split(9, 4, Axis::Row));
    }

    #[test]
    fn layout_fills_area() {
        let rects = tree().layout(Rect { x: 0.0, y: 0.0, w: 100.0, h: 50.0 });
        assert_eq!(rects[0], (1, Rect { x: 0.0, y: 0.0, w: 50.0, h: 50.0 }));
        assert_eq!(rects[1], (2, Rect { x: 50.0, y: 0.0, w: 50.0, h: 25.0 }));
        assert_eq!(rects[2], (3, Rect { x: 50.0, y: 25.0, w: 50.0, h: 25.0 }));
        let area: f32 = rects.iter().map(|(_, r)| r.w * r.h).sum();
        assert_eq!(area, 5000.0);
    }

    #[test]
    fn remove_collapses_to_sibling() {
        let t = tree().remove(2).unwrap();
        assert_eq!(t.leaves(), [1, 3]);
        assert_eq!(t.layout(Rect::UNIT)[1].1, Rect { x: 0.5, y: 0.0, w: 0.5, h: 1.0 });
        let t = t.remove(1).unwrap();
        assert_eq!(t, Node::Leaf(3));
        assert_eq!(t.remove(3), None);
        assert_eq!(Node::Leaf(1).remove(7), Some(Node::Leaf(1)));
    }

    #[test]
    fn directional_neighbors() {
        let t = tree();
        assert_eq!(t.neighbor(1, Dir::Right), Some(2));
        assert_eq!(t.neighbor(2, Dir::Down), Some(3));
        assert_eq!(t.neighbor(3, Dir::Up), Some(2));
        assert_eq!(t.neighbor(3, Dir::Left), Some(1));
        assert_eq!(t.neighbor(2, Dir::Left), Some(1));
        assert_eq!(t.neighbor(1, Dir::Left), None);
        assert_eq!(t.neighbor(1, Dir::Up), None);
        assert_eq!(t.neighbor(9, Dir::Up), None);
    }

    #[test]
    fn dragging_dividers() {
        let mut t = tree();
        let area = Rect { x: 0.0, y: 0.0, w: 200.0, h: 100.0 };
        // Root divider (vertical) to x=50.
        assert!(t.drag_divider(&[], area, (50.0, 0.0)));
        assert_eq!(t.layout(area)[0].1.w, 50.0);
        // Nested divider (horizontal, inside the right half) to y=80.
        assert!(t.drag_divider(&[true], area, (0.0, 80.0)));
        assert_eq!(t.layout(area)[1].1.h, 80.0);
        // Clamped, and invalid paths are rejected.
        assert!(t.drag_divider(&[], area, (-500.0, 0.0)));
        assert!((t.layout(area)[0].1.w - 20.0).abs() < 1e-4);
        assert!(!t.drag_divider(&[false], area, (0.0, 0.0)));
        assert!(!t.drag_divider(&[true, true], area, (0.0, 0.0)));
    }

    #[test]
    fn resize_uses_nearest_split_on_axis() {
        let mut t = tree();
        assert!(t.resize(3, Axis::Column, 0.2));
        assert_eq!(t.layout(Rect::UNIT)[2].1.h, 0.7);
        assert!(t.resize(3, Axis::Row, 0.1));
        assert!((t.layout(Rect::UNIT)[2].1.w - 0.6).abs() < 1e-6);
        // Clamped so no pane can vanish.
        assert!(t.resize(1, Axis::Row, 5.0));
        assert!((t.layout(Rect::UNIT)[0].1.w - 0.9).abs() < 1e-6);
        assert!(!Node::Leaf(1).resize(1, Axis::Row, 0.1));
    }
}
