//! Edit → Processing neighbourhood filters (Smoothing, Sharpening, Heavy
//! sharpening, Spot removing, Minimum, Maximum, Contour outlining, Emboss)
//! plus the two hidden command IDs 127 and 250 that share their dispatcher.
//!
//! All of them run inside one row-streaming framework in PMAN.EXE,
//! `sub_43_0be6` (callback of `sub_22_0802`, entered from `sub_43_23fc`).
//! See `re/specs/filters.md` for the recovered pseudocode. The framework
//! details that matter for a bit-exact port:
//!
//! * For output pixel `(x, y)` the window is the `fx × fy` block of columns
//!   `x - fx/2 ..= x - fx/2 + fx - 1` and rows `y - fy/2 ..= y - fy/2 + fy - 1`
//!   (for even sizes the window extends one more pixel up/left than
//!   down/right).
//! * Borders use edge replication. Rows are clamped to `[0, H-1]`
//!   (`sub_44_0088`). Columns are clamped to `[0, W-2]`, not `[0, W-1]`:
//!   the column fetch treats `W-1` as an exclusive bound, so the last image
//!   column is never read and is replaced by a copy of column `W-2`. This is a
//!   quirk of the original and is reproduced.
//! * Every op works per channel on the stored bytes. PMAN keeps pixels in BGR
//!   order (DIB layout); that only matters for Emboss, whose per-channel
//!   offset is the system colour.
//! * All the integer arithmetic (truncation versus floor) is reproduced
//!   exactly.

use crate::core::{Image, Rect, Rgb};
#[cfg(not(target_arch = "wasm32"))]
use rayon::prelude::*;

/// "Filter size" (dialog `SETARRAYSIZE`, proc `SETMATRICSIZEDLGPROC` at
/// `seg43:215c`). The dialog always opens at 3×3. Both scroll bars range
/// from 2 to 15, so `FilterSize::MIN` and `FilterSize::MAX` give the UI its
/// limits. (Mosaic, Faceted glass and Scatter use `W/5`, `H/5` as the
/// maximum; those ops live elsewhere.)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FilterSize {
    /// Horizontal size `N` (columns).
    pub w: usize,
    /// Vertical size `M` (rows).
    pub h: usize,
}

impl FilterSize {
    pub const MIN: usize = 2;
    pub const MAX: usize = 15;
    pub const fn new(w: usize, h: usize) -> Self {
        FilterSize { w, h }
    }
}

impl Default for FilterSize {
    /// `sub_43_2394` resets the size to 3×3 every time the dialog opens.
    fn default() -> Self {
        FilterSize { w: 3, h: 3 }
    }
}

/// Default system colour used by Emboss when PMAN.INI has no `[COLOR]`
/// section (`seg3:0cfc`: RED=0, GREEN=0, BLUE=255). The shipped PMAN.INI
/// sets RED=0, GREEN=166, BLUE=166 instead.
pub const DEFAULT_SYSTEM_COLOR: Rgb = [0, 0, 255];

// ---------------------------------------------------------------------------
// Framework (seg43:0be6 row loop)
// ---------------------------------------------------------------------------

fn clip_roi(src: &Image, roi: Rect) -> Rect {
    let x = roi.x.min(src.w);
    let y = roi.y.min(src.h);
    Rect {
        x,
        y,
        w: roi.w.min(src.w - x),
        h: roi.h.min(src.h - y),
    }
}

/// Runs `kernel` once per ROI row, the way `sub_43_0be6` does. The kernel gets
/// `fy` window rows, each a gathered buffer of `roi.w + fx` pixels where
/// buffer index `i` holds source column `clamp(roi.x - fx/2 + i, 0, W-2)`.
/// The kernel writes `roi.w` output pixels. Output pixel `j` corresponds to
/// `x = roi.x + j`, and its window is buffer indices `j .. j+fx`.
fn run_rows<K>(src: &Image, roi: Rect, fx: usize, fy: usize, kernel: K) -> Image
where
    K: Fn(&[Vec<Rgb>], &mut [Rgb]) + Sync,
{
    let mut out = src.clone();
    let roi = clip_roi(src, roi);
    if roi.is_empty() {
        return out;
    }
    let (fx, fy) = (fx.max(1), fy.max(1));
    let half_x = (fx / 2) as isize;
    let half_y = (fy / 2) as isize;
    // Edge replication. (The original treated W-1 as an exclusive bound at
    // seg43:0f80/1100, so the last column was never read and acted as a copy
    // of the one before it; fixed.)
    let xmax = src.w.saturating_sub(1) as isize;
    let ymax = src.h as isize - 1;
    let xs: Vec<usize> = (0..roi.w + fx)
        .map(|i| (roi.x as isize - half_x + i as isize).clamp(0, xmax) as usize)
        .collect();

    // Rows in parallel on the desktop; the browser has one thread.
    #[cfg(not(target_arch = "wasm32"))]
    let rows_iter = (0..roi.h).into_par_iter();
    #[cfg(target_arch = "wasm32")]
    let rows_iter = 0..roi.h;
    let rows: Vec<Vec<Rgb>> = rows_iter
        .map(|dy| {
            let y = (roi.y + dy) as isize;
            let bufs: Vec<Vec<Rgb>> = (0..fy)
                .map(|r| {
                    let sy = (y - half_y + r as isize).clamp(0, ymax) as usize;
                    let row = src.row(sy);
                    xs.iter().map(|&x| row[x]).collect()
                })
                .collect();
            let mut o = vec![[0u8; 3]; roi.w];
            kernel(&bufs, &mut o);
            o
        })
        .collect();

    for (dy, r) in rows.into_iter().enumerate() {
        out.row_mut(roi.y + dy)[roi.x..roi.x + roi.w].copy_from_slice(&r);
    }
    out
}

/// Per-channel sums of the window columns, used by the box-filter style ops.
#[inline]
fn column_sum(rows: &[Vec<Rgb>], i: usize, c: usize) -> i32 {
    rows.iter().map(|r| r[i][c] as i32).sum()
}

// ---------------------------------------------------------------------------
// Smoothing (107) and hidden 250
// ---------------------------------------------------------------------------

/// **Smoothing** (command 107, case at `seg43:1340`). Box mean over an
/// `N×M` window with edge replication, computed as
/// `sum / (N*M)` with truncating integer division. The original keeps
/// running 16-bit column sums, which cannot overflow for sizes up to 15.
/// Shows the Filter size dialog (default 3×3, range 2..=15).
/// Confidence: high.
pub fn smoothing(src: &Image, roi: Rect, size: FilterSize) -> Image {
    let (fx, fy) = (size.w.max(1), size.h.max(1));
    let area = (fx * fy) as i32;
    run_rows(src, roi, fx, fy, |rows, out| {
        let colsum: Vec<[i32; 3]> = (0..rows[0].len())
            .map(|i| [0, 1, 2].map(|c| column_sum(rows, i, c)))
            .collect();
        // Running window sum across the row.
        let mut acc = [0i32; 3];
        for i in 0..fx {
            for c in 0..3 {
                acc[c] += colsum[i][c];
            }
        }
        for (j, o) in out.iter_mut().enumerate() {
            if j > 0 {
                for c in 0..3 {
                    acc[c] += colsum[j + fx - 1][c] - colsum[j - 1][c];
                }
            }
            *o = [0, 1, 2].map(|c| (acc[c] / area) as u8);
        }
    })
}

/// **Hidden command 250** (no menu entry, handled at `seg43:1338` → `1340`).
/// It is the Smoothing code path. Because 250 is not in `sub_43_23fc`'s list
/// of IDs that ask for a filter size, the size is the fixed default 3×3 and no
/// dialog is shown. Confidence: high.
pub fn smoothing_3x3(src: &Image, roi: Rect) -> Image {
    smoothing(src, roi, FilterSize::new(3, 3))
}

// ---------------------------------------------------------------------------
// Sharpening (122) / Heavy sharpening (123)
// ---------------------------------------------------------------------------

/// **Sharpening** (command 122, `seg43:15b2`). Fixed 3×3, with no dialog.
/// With `S` the 3×3 sum (centre included) and `c` the centre:
/// `out = clamp((17*c - S) / 8, 0, 255)`, with C `long` division
/// (`_aFldiv`, truncating toward zero). Confidence: high.
///
/// Fixed: the original passed the result through `abs()`, so a strong
/// negative overshoot (the dark side of an edge) came out bright.
pub fn sharpening(src: &Image, roi: Rect) -> Image {
    run_rows(src, roi, 3, 3, |rows, out| {
        for (j, o) in out.iter_mut().enumerate() {
            *o = [0, 1, 2].map(|c| {
                let s: i32 = (j..j + 3).map(|i| column_sum(rows, i, c)).sum();
                let ctr = rows[1][j + 1][c] as i32;
                ((17 * ctr - s) / 8).clamp(0, 255) as u8
            });
        }
    })
}

/// **Heavy sharpening** (command 123, `seg43:1807`). Fixed 3×3, with no
/// dialog. `out = min(255, |(35*c - 3*S) >> 3|)`, where the shift is an
/// arithmetic `long` shift (`seg1:0c7e`, floor). Confidence: high.
///
/// Fixed like Sharpening: the original mirrored negative results with
/// `abs()`; they are clamped to 0 now.
pub fn heavy_sharpening(src: &Image, roi: Rect) -> Image {
    run_rows(src, roi, 3, 3, |rows, out| {
        for (j, o) in out.iter_mut().enumerate() {
            *o = [0, 1, 2].map(|c| {
                let s: i32 = (j..j + 3).map(|i| column_sum(rows, i, c)).sum();
                let ctr = rows[1][j + 1][c] as i32;
                ((35 * ctr - 3 * s) >> 3).clamp(0, 255) as u8
            });
        }
    })
}

// ---------------------------------------------------------------------------
// Spot removing (124), Minimum (125), Maximum (126)
// ---------------------------------------------------------------------------

/// **Spot removing** (command 124, `sub_43_08ee`). Median over the `N×M`
/// window, per channel, with a sliding 256-bin histogram: the smallest `v`
/// with `count(<= v) >= (N*M+1)/2` (the lower median for even counts).
/// Shows the Filter size dialog. Confidence: high.
///
/// Fixed: the original used rank `(N*M)/2`, one below the true median for
/// odd sizes (the 4th of 9 for 3×3), which darkened slightly.
///
/// The dialog never allows a 1×1 window, but the function accepts one. In
/// that case the original reads uninitialised stack memory; here the pixel
/// is returned unchanged.
pub fn spot_removing(src: &Image, roi: Rect, size: FilterSize) -> Image {
    let (fx, fy) = (size.w.max(1), size.h.max(1));
    let half = (fx * fy).div_ceil(2);
    if half == 0 {
        return src.clone();
    }
    run_rows(src, roi, fx, fy, |rows, out| {
        for c in 0..3 {
            let mut hist = [0u16; 256];
            for r in rows {
                for px in &r[0..fx] {
                    hist[px[c] as usize] += 1;
                }
            }
            for (j, o) in out.iter_mut().enumerate() {
                if j > 0 {
                    for r in rows {
                        hist[r[j - 1][c] as usize] -= 1;
                        hist[r[j + fx - 1][c] as usize] += 1;
                    }
                }
                let mut cum = 0usize;
                let mut v = 0usize;
                while v < 256 {
                    cum += hist[v] as usize;
                    if cum >= half {
                        break;
                    }
                    v += 1;
                }
                o[c] = v.min(255) as u8;
            }
        }
    })
}

/// Separable min/max over the window. This is equivalent to the
/// original's brute-force double loop.
fn rank_extreme(src: &Image, roi: Rect, size: FilterSize, take_max: bool) -> Image {
    let (fx, fy) = (size.w.max(1), size.h.max(1));
    run_rows(src, roi, fx, fy, move |rows, out| {
        let n = rows[0].len();
        let mut col = vec![[if take_max { 0u8 } else { 255u8 }; 3]; n];
        for r in rows {
            for (acc, px) in col.iter_mut().zip(r) {
                for c in 0..3 {
                    acc[c] = if take_max {
                        acc[c].max(px[c])
                    } else {
                        acc[c].min(px[c])
                    };
                }
            }
        }
        for (j, o) in out.iter_mut().enumerate() {
            *o = col[j..j + fx]
                .iter()
                .fold([if take_max { 0u8 } else { 255u8 }; 3], |a, p| {
                    [0, 1, 2].map(|c| {
                        if take_max {
                            a[c].max(p[c])
                        } else {
                            a[c].min(p[c])
                        }
                    })
                });
        }
    })
}

/// **Minimum** (command 125, `sub_43_074c`). Per-channel minimum over the
/// `N×M` window. Shows the Filter size dialog. Confidence: high.
pub fn minimum(src: &Image, roi: Rect, size: FilterSize) -> Image {
    rank_extreme(src, roi, size, false)
}

/// **Maximum** (command 126, `sub_43_081e`). Per-channel maximum over the
/// `N×M` window. Shows the Filter size dialog. Confidence: high.
pub fn maximum(src: &Image, roi: Rect, size: FilterSize) -> Image {
    rank_extreme(src, roi, size, true)
}

// ---------------------------------------------------------------------------
// Contour outlining (130), Emboss (131)
// ---------------------------------------------------------------------------

/// **Contour outlining** (command 130, `sub_43_04d4`). Fixed 3×3, with no
/// dialog. `out = clamp(2*c - min3x3, 0, 255)`, per channel, where `min3x3`
/// includes the centre. Confidence: high.
pub fn contour_outlining(src: &Image, roi: Rect) -> Image {
    run_rows(src, roi, 3, 3, |rows, out| {
        for (j, o) in out.iter_mut().enumerate() {
            *o = [0, 1, 2].map(|c| {
                let m = rows
                    .iter()
                    .flat_map(|r| r[j..j + 3].iter().map(move |p| p[c]))
                    .min()
                    .unwrap_or(255) as i32;
                let ctr = rows[1][j + 1][c] as i32;
                (2 * ctr - m).clamp(0, 255) as u8
            });
        }
    })
}

/// **Emboss** (command 131, `sub_43_025c`). Fixed 3×3 framework, with no
/// dialog. It uses only the centre row:
/// `out = clamp(buf[j+1] - buf[max(j-1, 0)] + color, 0, 255)` per channel,
/// where `color` is the current system colour (`DS:0x6e40`, a COLORREF; its
/// bytes are applied B,G,R to the stored BGR channels).
///
/// Centred difference `p(x+1) - p(x-1) + color`, edges replicated.
///
/// Fixed: the original indexed the buffer one place too far left, computing
/// `p(x) - p(x-2)`, and in the first ROI column `p(x0) - p(x0-1)`, so the
/// relief was shifted by a pixel and inconsistent at the area's left edge.
pub fn emboss(src: &Image, roi: Rect, color: Rgb) -> Image {
    run_rows(src, roi, 3, 3, move |rows, out| {
        let row = &rows[1];
        for (j, o) in out.iter_mut().enumerate() {
            let a = row[j];
            let b = row[j + 2];
            *o = [0, 1, 2]
                .map(|c| (b[c] as i32 - a[c] as i32 + color[c] as i32).clamp(0, 255) as u8);
        }
    })
}

// ---------------------------------------------------------------------------
// Hidden 127
// ---------------------------------------------------------------------------

/// **Hidden command 127** (no menu entry). `sub_43_23fc` shows the Filter
/// size dialog for it, but `sub_43_0be6` has no case for 127, so no
/// processing is done. The framework then writes out the oldest buffered
/// row (`ptr[0]`), which holds the window's top-left origin. The visible
/// effect is a copy of the image shifted right by `N/2` and down by `M/2`
/// inside the ROI, with edge replication (including the `W-2` column clamp).
/// It is almost certainly a removed or unfinished filter stub. The unused
/// dialogs EXITATION ("Grain size/Depth") and DFRM ("Correlation/Depth") are
/// not referenced by this path. Confidence: high for the behaviour; the
/// intended purpose is unknown.
pub fn hidden_127(src: &Image, roi: Rect, size: FilterSize) -> Image {
    let (fx, fy) = (size.w.max(1), size.h.max(1));
    run_rows(src, roi, fx, fy, |rows, out| {
        out.copy_from_slice(&rows[0][..out.len()]);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gray(w: usize, h: usize, f: impl Fn(usize, usize) -> u8) -> Image {
        let mut im = Image::new(w, h, [0; 3]);
        for y in 0..h {
            for x in 0..w {
                let v = f(x, y);
                im.set(x, y, [v, v, v]);
            }
        }
        im
    }

    #[test]
    fn flat_image_is_fixed_point() {
        let im = Image::new(8, 6, [10, 120, 250]);
        let r = im.rect();
        assert_eq!(smoothing(&im, r, FilterSize::new(5, 3)), im);
        assert_eq!(sharpening(&im, r), im);
        assert_eq!(heavy_sharpening(&im, r), im);
        assert_eq!(spot_removing(&im, r, FilterSize::default()), im);
        assert_eq!(minimum(&im, r, FilterSize::default()), im);
        assert_eq!(maximum(&im, r, FilterSize::default()), im);
        assert_eq!(contour_outlining(&im, r), im);
        // Emboss of a flat image is just the system colour.
        let e = emboss(&im, r, [1, 2, 3]);
        assert!(e.px.iter().all(|p| *p == [1, 2, 3]));
    }

    #[test]
    fn smoothing_truncates() {
        // A single 255 in a 3x3 window: 255/9 = 28.33 -> 28.
        let im = gray(5, 5, |x, y| if x == 2 && y == 2 { 255 } else { 0 });
        let s = smoothing(&im, im.rect(), FilterSize::default());
        assert_eq!(s.get(2, 2)[0], 28);
        assert_eq!(s.get(1, 1)[0], 28);
        assert_eq!(s.get(0, 0)[0], 0);
    }

    #[test]
    fn sharpening_clamps_negative() {
        // A dark centre pixel surrounded by 255: (17*0 - 8*255)/8 = -255 -> 0.
        let im = gray(5, 5, |x, y| if x == 2 && y == 2 { 0 } else { 255 });
        assert_eq!(sharpening(&im, im.rect()).get(2, 2)[0], 0);
        // Bright centre on black: (3400 - 200)/8 = 400 -> 255; a neighbour:
        // (0 - 200)/8 = -25 -> 0 (the original mirrored it to 25).
        let im = gray(5, 5, |x, y| if x == 2 && y == 2 { 200 } else { 0 });
        let s = sharpening(&im, im.rect());
        assert_eq!(s.get(2, 2)[0], 255);
        assert_eq!(s.get(1, 2)[0], 0);
        assert_eq!(heavy_sharpening(&im, im.rect()).get(1, 2)[0], 0);
    }

    #[test]
    fn spot_removing_is_true_median() {
        // 3x3 window values 1..=9 -> median 5.
        let mut big = Image::new(6, 6, [0; 3]);
        for y in 0..3 {
            for x in 0..3 {
                big.set(x + 1, y + 1, [(y * 3 + x + 1) as u8; 3]);
            }
        }
        let r = spot_removing(&big, big.rect(), FilterSize::default());
        assert_eq!(r.get(2, 2)[0], 5);
        // A single bright spot disappears.
        let im = gray(5, 5, |x, y| if x == 2 && y == 2 { 255 } else { 40 });
        assert_eq!(
            spot_removing(&im, im.rect(), FilterSize::default()).get(2, 2)[0],
            40
        );
    }

    #[test]
    fn last_column_is_read() {
        // Column W-1 = 255, others 0: the maximum reaches its neighbour.
        let im = gray(6, 3, |x, _| if x == 5 { 255 } else { 0 });
        let m = maximum(&im, im.rect(), FilterSize::default());
        assert_eq!(m.get(5, 1)[0], 255);
        assert_eq!(m.get(4, 1)[0], 255);
        assert_eq!(m.get(3, 1)[0], 0);
    }

    #[test]
    fn hidden_127_shifts() {
        let im = gray(8, 8, |x, y| (x * 10 + y) as u8);
        let r = hidden_127(&im, im.rect(), FilterSize::new(3, 5));
        // shift right by 1, down by 2
        assert_eq!(r.get(4, 4)[0], im.get(3, 2)[0]);
        assert_eq!(r.get(0, 0)[0], im.get(0, 0)[0]);
    }

    #[test]
    fn emboss_is_centred() {
        let im = gray(8, 1, |x, _| (x * 10) as u8);
        let roi = Rect {
            x: 3,
            y: 0,
            w: 3,
            h: 1,
        };
        let e = emboss(&im, roi, [0, 0, 0]);
        assert_eq!(e.get(3, 0)[0], 20); // p(4) - p(2)
        assert_eq!(e.get(4, 0)[0], 20); // p(5) - p(3)
        assert_eq!(e.get(2, 0)[0], 20); // outside ROI: untouched
        // Edges replicate: at x = 7, p(7) - p(6).
        let e = emboss(&im, im.rect(), [0, 0, 0]);
        assert_eq!(e.get(7, 0)[0], 10);
    }

    #[test]
    fn roi_only() {
        let im = gray(6, 6, |x, y| ((x + y) * 20) as u8);
        let roi = Rect {
            x: 1,
            y: 1,
            w: 2,
            h: 2,
        };
        let s = minimum(&im, roi, FilterSize::default());
        assert_eq!(s.get(0, 0), im.get(0, 0));
        assert_eq!(s.get(4, 4), im.get(4, 4));
        assert_eq!(s.get(1, 1)[0], 0);
    }
}
