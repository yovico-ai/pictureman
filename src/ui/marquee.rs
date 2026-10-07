//! The current selection of a document: a mask that stays until it is
//! cleared, combined with new shapes (replace, add, subtract), inverted or
//! moved. Commands apply to it — or to the whole image when there is none.

use eframe::egui::{Pos2, pos2};

use crate::core::{Mask, Rect};
use crate::selection::{self as sel, AreaKind, Edge, Weights};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Combine {
    Replace,
    Add,
    Subtract,
}

#[derive(Clone)]
pub struct Selection {
    /// 255 = selected.
    pub mask: Mask,
    /// The tool that made it last; decides the soft-edge width.
    pub kind: AreaKind,
    /// Made with the ellipse tool (the radial gradient and Rubber follow it).
    pub elliptic: bool,
    pub bounds: Rect,
    /// Outline as pixel-edge segments, in image coordinates.
    pub outline: Vec<[Pos2; 2]>,
}

impl Selection {
    /// `None` when nothing is selected.
    pub fn new(mask: Mask, kind: AreaKind, elliptic: bool) -> Option<Self> {
        let bounds = mask.bounds();
        if bounds.is_empty() {
            return None;
        }
        let outline = outline_segments(&mask);
        Some(Selection {
            mask,
            kind,
            elliptic,
            bounds,
            outline,
        })
    }

    pub fn all(w: usize, h: usize) -> Option<Self> {
        Selection::new(Mask::full(w, h), AreaKind::Rect, false)
    }

    /// Combine a new shape with the current selection.
    pub fn combine(
        old: Option<&Selection>,
        shape: Mask,
        kind: AreaKind,
        elliptic: bool,
        op: Combine,
    ) -> Option<Self> {
        match (op, old) {
            (Combine::Replace, _) | (Combine::Add, None) => Selection::new(shape, kind, elliptic),
            (Combine::Subtract, None) => None,
            (Combine::Add, Some(o)) => {
                let mut m = o.mask.clone();
                for (a, b) in m.data.iter_mut().zip(&shape.data) {
                    *a = (*a).max(*b);
                }
                Selection::new(m, kind, elliptic && o.elliptic)
            }
            (Combine::Subtract, Some(o)) => {
                let mut m = o.mask.clone();
                for (a, b) in m.data.iter_mut().zip(&shape.data) {
                    if *b != 0 {
                        *a = 0;
                    }
                }
                Selection::new(m, o.kind, false)
            }
        }
    }

    /// Everything that wasn't selected.
    pub fn inverted(&self) -> Option<Self> {
        let mut m = self.mask.clone();
        m.invert();
        Selection::new(m, self.kind, false)
    }

    /// The same shape moved by (dx, dy) pixels.
    pub fn translated(&self, dx: i64, dy: i64) -> Option<Self> {
        let (w, h) = (self.mask.w, self.mask.h);
        let mut m = Mask::empty(w, h);
        for y in 0..h {
            let sy = y as i64 - dy;
            if sy < 0 || sy >= h as i64 {
                continue;
            }
            for x in 0..w {
                let sx = x as i64 - dx;
                if sx >= 0 && sx < w as i64 {
                    m.set(x, y, self.mask.get(sx as usize, sy as usize));
                }
            }
        }
        Selection::new(m, self.kind, self.elliptic)
    }

    pub fn contains(&self, p: Pos2) -> bool {
        p.x >= 0.0
            && p.y >= 0.0
            && (p.x as usize) < self.mask.w
            && (p.y as usize) < self.mask.h
            && self.mask.get(p.x as usize, p.y as usize) != 0
    }

    /// Soft-edge weights for committing an operation (the original's
    /// quarter-sine ramp inside the area).
    pub fn weights(&self, edge: Edge) -> Weights {
        let feather =
            sel::feather_width(self.kind, edge, self.bounds.w as i32, self.bounds.h as i32);
        sel::selection_weights(&self.mask, true, edge, feather)
    }
}

/// The boundary between selected and unselected pixels as horizontal and
/// vertical segments along pixel edges (runs merged).
pub fn outline_segments(m: &Mask) -> Vec<[Pos2; 2]> {
    let (w, h) = (m.w, m.h);
    let on = |x: usize, y: usize| m.data[y * w + x] != 0;
    let mut out = Vec::new();
    // Horizontal edges between rows y-1 and y.
    for y in 0..=h {
        let mut start: Option<usize> = None;
        for x in 0..=w {
            let edge = x < w && {
                let a = y > 0 && on(x, y - 1);
                let b = y < h && on(x, y);
                a != b
            };
            match (edge, start) {
                (true, None) => start = Some(x),
                (false, Some(s)) => {
                    out.push([pos2(s as f32, y as f32), pos2(x as f32, y as f32)]);
                    start = None;
                }
                _ => {}
            }
        }
    }
    // Vertical edges between columns x-1 and x.
    for x in 0..=w {
        let mut start: Option<usize> = None;
        for y in 0..=h {
            let edge = y < h && {
                let a = x > 0 && on(x - 1, y);
                let b = x < w && on(x, y);
                a != b
            };
            match (edge, start) {
                (true, None) => start = Some(y),
                (false, Some(s)) => {
                    out.push([pos2(x as f32, s as f32), pos2(x as f32, y as f32)]);
                    start = None;
                }
                _ => {}
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect_mask(w: usize, h: usize, r: Rect) -> Mask {
        let mut m = Mask::empty(w, h);
        for y in r.y..r.y + r.h {
            for x in r.x..r.x + r.w {
                m.set(x, y, 255);
            }
        }
        m
    }

    #[test]
    fn outline_of_a_rectangle_is_four_segments() {
        let m = rect_mask(
            10,
            8,
            Rect {
                x: 2,
                y: 3,
                w: 4,
                h: 2,
            },
        );
        let segs = outline_segments(&m);
        assert_eq!(segs.len(), 4);
        assert!(segs.contains(&[pos2(2.0, 3.0), pos2(6.0, 3.0)]));
        assert!(segs.contains(&[pos2(6.0, 3.0), pos2(6.0, 5.0)]));
    }

    #[test]
    fn combine_invert_translate() {
        let a = Selection::new(
            rect_mask(
                10,
                10,
                Rect {
                    x: 0,
                    y: 0,
                    w: 4,
                    h: 4,
                },
            ),
            AreaKind::Rect,
            false,
        );
        let b = rect_mask(
            10,
            10,
            Rect {
                x: 2,
                y: 2,
                w: 4,
                h: 4,
            },
        );
        let u =
            Selection::combine(a.as_ref(), b.clone(), AreaKind::Rect, false, Combine::Add).unwrap();
        assert_eq!(
            u.bounds,
            Rect {
                x: 0,
                y: 0,
                w: 6,
                h: 6
            }
        );
        let d =
            Selection::combine(a.as_ref(), b, AreaKind::Rect, false, Combine::Subtract).unwrap();
        assert!(d.contains(pos2(0.5, 0.5)) && !d.contains(pos2(3.5, 3.5)));
        let i = a.as_ref().unwrap().inverted().unwrap();
        assert!(!i.contains(pos2(1.5, 1.5)) && i.contains(pos2(8.5, 8.5)));
        let t = a.unwrap().translated(3, 1).unwrap();
        assert_eq!(
            t.bounds,
            Rect {
                x: 3,
                y: 1,
                w: 4,
                h: 4
            }
        );
        assert!(Selection::all(5, 5).unwrap().inverted().is_none());
    }
}
