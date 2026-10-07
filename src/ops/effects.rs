//! Edit → Processing effects that take the "Filter size" N×M parameter:
//! Hand drawing (128), Cleaning background (129), Mosaic (132),
//! Faceted glass (133) and Scatter (134).
//!
//! Recovered from PMAN.EXE segment 43 (see `re/specs/effects.md`).
//!
//! All five run inside the shared sliding-window driver `sub_43_0be6`, which
//! keeps `sy` source rows of the selection's bounding rectangle in memory.
//! Window row `k` holds image row `y - sy/2 + k` (clamped to the image), and
//! window column `b` holds image column `x0 - sx/2 + b`, where `x0` is the
//! left edge of the processed rectangle. Columns are loaded only for
//! `[max(0, x0 - sx/2), min(w-1, x0 + W + sx - sx/2 + 1))` (note the
//! exclusive end clamped to `w-1`), and the rest is padded by replicating the
//! first/last loaded pixel. A consequence reproduced here: whenever the
//! window reaches the right image border, the last image column `w-1` is
//! never read — it is replaced by column `w-2`.
//!
//! Each per-op row routine is called once per channel (pixels are stored
//! BGR in the original; every op here treats the channels identically, so
//! the channel order does not matter for results).

use crate::core::{Image, MsRand, Rect, Rgb};

/// Filter-size dialog (`SETARRAYSIZE`, `sub_43_2394` / dialog proc
/// `sub_43_215c`): the size always starts at 3×3.
pub const FILTER_SIZE_INITIAL: (usize, usize) = (3, 3);
/// Minimum of both scroll bars.
pub const FILTER_SIZE_MIN: usize = 2;
/// Maximum for Hand drawing / Cleaning background (and Smoothing etc.).
pub const FILTER_SIZE_MAX: usize = 15;

/// Maximum filter size for Mosaic, Faceted glass and Scatter:
/// `(width/5, height/5)` of the whole image (`sub_43_23fc` @ 2480).
pub fn filter_size_max_tiles(img_w: usize, img_h: usize) -> (usize, usize) {
    (img_w / 5, img_h / 5)
}

/// Column/row addressing of the sliding window of `sub_43_0be6`, with the
/// image edges replicated.
///
/// Fixed: the original clamped columns to the span it had loaded for the
/// area, `[max(0, x0 - sx/2), min(W-1, …))` with W-1 exclusive, so the last
/// image column was never read and results near an area's left edge
/// depended on where the area started.
struct Window {
    w: isize,
    h: isize,
}

impl Window {
    fn new(src: &Image, _roi: Rect, _sx: usize) -> Self {
        Window {
            w: src.w as isize,
            h: src.h as isize,
        }
    }
    #[inline]
    fn col(&self, c: isize) -> usize {
        c.clamp(0, self.w - 1) as usize
    }
    /// Image row for wanted row `r` (sub_44_0088 clamps to the image).
    #[inline]
    fn row(&self, r: isize) -> usize {
        r.clamp(0, self.h - 1) as usize
    }
}

fn check(src: &Image, roi: Rect, sx: usize, sy: usize) -> bool {
    !(roi.is_empty() || src.w == 0 || src.h == 0 || sx == 0 || sy == 0)
}

/// Hand drawing (command 128, `sub_43_05fc`, per channel).
///
/// For every pixel and channel: `mn`/`mx` = min/max over the `sx`×`sy`
/// window (columns `x - sx/2 ..`, rows `y - sy/2 ..`), `c` = centre,
/// `half = mn >> 1`;
/// `out = 255` if `mx == half` (only for an all-zero window), else
/// `min(255, (c - half) * 255 / (mx - half))` (unsigned 16-bit, truncating).
/// Confidence: high.
pub fn hand_drawing(src: &Image, roi: Rect, sx: usize, sy: usize) -> Image {
    let mut out = src.clone();
    if !check(src, roi, sx, sy) {
        return out;
    }
    let win = Window::new(src, roi, sx);
    let (hx, hy) = ((sx / 2) as isize, (sy / 2) as isize);
    for y in roi.y..roi.y + roi.h {
        for x in roi.x..roi.x + roi.w {
            let (xi, yi) = (x as isize, y as isize);
            let mut mn = [255u16; 3];
            let mut mx = [0u16; 3];
            for k in 0..sy as isize {
                let r = win.row(yi - hy + k);
                for j in 0..sx as isize {
                    let p = src.get(win.col(xi - hx + j), r);
                    for ch in 0..3 {
                        mn[ch] = mn[ch].min(p[ch] as u16);
                        mx[ch] = mx[ch].max(p[ch] as u16);
                    }
                }
            }
            let c = src.get(win.col(xi), win.row(yi));
            let mut o: Rgb = [0; 3];
            for ch in 0..3 {
                let half = mn[ch] >> 1;
                o[ch] = if half == mx[ch] {
                    255
                } else {
                    let v = (c[ch] as u16 - half).wrapping_mul(255) / (mx[ch] - half);
                    v.min(255) as u8
                };
            }
            out.set(x, y, o);
        }
    }
    out
}

/// Cleaning background (command 129, `sub_43_0348`, per channel): a sigma
/// filter. Averages the window pixels `v` with `c-40 < v < c+40` (`c` = the
/// centre; the centre itself always qualifies).
///
/// The sum is divided (truncating) by the count. Confidence: high.
///
/// Fixed: the original summed in a 16-bit signed integer, which wrapped for
/// bright areas with large windows (15×15 of 200 came out as 165).
pub fn cleaning_background(src: &Image, roi: Rect, sx: usize, sy: usize) -> Image {
    let mut out = src.clone();
    if !check(src, roi, sx, sy) {
        return out;
    }
    let win = Window::new(src, roi, sx);
    let (hx, hy) = ((sx / 2) as isize, (sy / 2) as isize);
    for y in roi.y..roi.y + roi.h {
        for x in roi.x..roi.x + roi.w {
            let (xi, yi) = (x as isize, y as isize);
            let c = src.get(win.col(xi), win.row(yi));
            let mut sum = [0i32; 3];
            let mut cnt = [0i32; 3];
            for k in 0..sy as isize {
                let r = win.row(yi - hy + k);
                for j in 0..sx as isize {
                    let p = src.get(win.col(xi - hx + j), r);
                    for ch in 0..3 {
                        let (v, cc) = (p[ch] as i32, c[ch] as i32);
                        if cc - 40 < v && cc + 40 > v {
                            sum[ch] += v;
                            cnt[ch] += 1;
                        }
                    }
                }
            }
            let mut o: Rgb = c;
            for ch in 0..3 {
                if cnt[ch] != 0 {
                    o[ch] = (sum[ch] / cnt[ch]) as u8;
                }
            }
            out.set(x, y, o);
        }
    }
    out
}

/// Mosaic (command 132, `sub_43_01a6`): every pixel copies one sample of
/// its tile. Tiles are anchored to absolute image coordinates:
/// horizontally `[X - X%sx, +sx)`, vertically tiles start where
/// `(y + sy/2) % sy == 0`; both sampled at `tile + (s-1)/2`, the centre.
/// Confidence: high.
///
/// Fixed: the original sampled horizontally at `tile_x + sx - sx/2`, one
/// pixel right of the vertical analogue (3×3 took the right-middle pixel).
pub fn mosaic(src: &Image, roi: Rect, sx: usize, sy: usize) -> Image {
    let mut out = src.clone();
    if !check(src, roi, sx, sy) {
        return out;
    }
    let win = Window::new(src, roi, sx);
    let (sxi, syi) = (sx as isize, sy as isize);
    let hy = syi / 2;
    for y in roi.y..roi.y + roi.h {
        let yi = y as isize;
        // window row k = sy - 1 - (sy/2 + y) % sy
        let k = syi - 1 - (hy + yi) % syi;
        let r = win.row(yi - hy + k);
        for x in roi.x..roi.x + roi.w {
            let xi = x as isize;
            let c = xi - xi % sxi + (sxi - 1) / 2;
            out.set(x, y, src.get(win.col(c), r));
        }
    }
    out
}

/// Faceted glass (command 133, `sub_43_00f2`): same tile grid as
/// [`mosaic`], but each tile shows its neighbourhood minified 2×: for tile
/// offsets `(tx, ty)` the source is `(tile_x + 2*tx - sx/2,
/// tile_y + 2*ty - sy/2)` (i.e. `X + X%sx - sx/2`, `y + (y+sy/2)%sy - sy/2`).
/// Confidence: high.
pub fn faceted_glass(src: &Image, roi: Rect, sx: usize, sy: usize) -> Image {
    let mut out = src.clone();
    if !check(src, roi, sx, sy) {
        return out;
    }
    let win = Window::new(src, roi, sx);
    let (sxi, syi) = (sx as isize, sy as isize);
    let (hx, hy) = (sxi / 2, syi / 2);
    for y in roi.y..roi.y + roi.h {
        let yi = y as isize;
        let k = (hy + yi) % syi;
        let r = win.row(yi - hy + k);
        for x in roi.x..roi.x + roi.w {
            let xi = x as isize;
            let c = xi - hx + xi % sxi;
            out.set(x, y, src.get(win.col(c), r));
        }
    }
    out
}

/// Scatter (command 134, `sub_43_0000`): each pixel takes a random pixel of
/// its `sx`×`sy` window: column `x - sx/2 + rand()%sx`, row
/// `y - sy/2 + rand()%sy`.
///
/// Randomness exactly as the original: before each channel of each image
/// row `y`, `srand(y)` (`sub_1_058c`), so all three channels pick the same
/// pixel. Per pixel, left to right, `rand()` is called 5 times — the
/// column clamp is a double-evaluating `max(min(rand()%sx+i, lim), 0)`
/// macro, so calls 1–3 are discarded, call 4 gives the column and call 5
/// the row. `rng` is reseeded by this function (its final state matches the
/// original after the last processed row). Confidence: high.
pub fn scatter(src: &Image, roi: Rect, sx: usize, sy: usize, rng: &mut MsRand) -> Image {
    let mut out = src.clone();
    if !check(src, roi, sx, sy) {
        return out;
    }
    let win = Window::new(src, roi, sx);
    let (sxi, syi) = (sx as i32, sy as i32);
    let (hx, hy) = ((sx / 2) as isize, (sy / 2) as isize);
    for y in roi.y..roi.y + roi.h {
        let yi = y as isize;
        let mut picks = Vec::with_capacity(roi.w);
        // The original runs this loop three times (B, G, R), each preceded by
        // srand(y); the sequence is identical each time.
        for _ch in 0..3 {
            rng.srand(y as u16 as u32);
            picks.clear();
            for _ in 0..roi.w {
                rng.rand(); // t1 (compared with the limit, always below it)
                rng.rand(); // t2 (checked for < 0)
                rng.rand(); // t3 (compared with the limit again)
                let dx = rng.rand() % sxi; // t4
                let dy = rng.rand() % syi; // t5
                picks.push((dx as isize, dy as isize));
            }
        }
        for (i, x) in (roi.x..roi.x + roi.w).enumerate() {
            let (dx, dy) = picks[i];
            let p = src.get(win.col(x as isize - hx + dx), win.row(yi - hy + dy));
            out.set(x, y, p);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(w: usize, h: usize) -> Image {
        let mut im = Image::new(w, h, [0; 3]);
        for y in 0..h {
            for x in 0..w {
                im.set(x, y, [(x * 7 + y) as u8, (y * 5) as u8, (x ^ y) as u8]);
            }
        }
        im
    }

    #[test]
    fn mosaic_tiles_are_flat_and_anchored() {
        let im = gradient(40, 30);
        let out = mosaic(&im, im.rect(), 4, 4);
        // Horizontal tiles start at multiples of 4, vertical tiles at y ≡ 2 (mod 4).
        for y in 2..6 {
            for x in 4..8 {
                assert_eq!(out.get(x, y), im.get(5, 3)); // tile centres: 4+(4-1)/2, 2+4-2-1
            }
        }
        assert_ne!(out.get(8, 2), out.get(7, 2));
        assert_ne!(out.get(4, 6), out.get(4, 5));
    }

    #[test]
    fn faceted_glass_minifies() {
        let im = gradient(40, 30);
        let out = faceted_glass(&im, im.rect(), 4, 4);
        // x = 9: X%4 = 1 -> col 9 + 1 - 2 = 8; y = 7: (7+2)%4 = 1 -> row 7 - 2 + 1 = 6
        assert_eq!(out.get(9, 7), im.get(8, 6));
    }

    #[test]
    fn scatter_is_deterministic_and_local() {
        let im = gradient(20, 20);
        let mut r1 = MsRand::default();
        let mut r2 = MsRand::new(12345);
        let a = scatter(&im, im.rect(), 3, 3, &mut r1);
        let b = scatter(&im, im.rect(), 3, 3, &mut r2);
        assert_eq!(a, b); // reseeded per row: initial state is irrelevant
        // Row 5, pixel 0: srand(5), 5 calls.
        let mut r = MsRand::new(5);
        for _ in 0..3 {
            r.rand();
        }
        let dx = (r.rand() % 3) as usize;
        let dy = (r.rand() % 3) as usize;
        let want = im.get((dx as isize - 1).max(0) as usize, 5 - 1 + dy);
        assert_eq!(a.get(0, 5), want);
    }

    #[test]
    fn hand_drawing_flat_is_white() {
        let im = Image::new(8, 8, [0, 100, 255]);
        let out = hand_drawing(&im, im.rect(), 3, 3);
        assert_eq!(out.get(4, 4), [255, 255, 255]);
        // Step edge: centre 100 next to 200 -> (100-50)*255/(200-50) = 85
        let mut im = Image::new(8, 1, [100; 3]);
        for x in 4..8 {
            im.set(x, 0, [200; 3]);
        }
        let out = hand_drawing(&im, im.rect(), 3, 3);
        assert_eq!(out.get(3, 0), [85; 3]);
    }

    #[test]
    fn cleaning_background_sigma_and_overflow() {
        let mut im = Image::new(9, 1, [100; 3]);
        im.set(4, 0, [130; 3]);
        im.set(5, 0, [250; 3]);
        let out = cleaning_background(&im, im.rect(), 3, 1);
        assert_eq!(out.get(4, 0), [115; 3]); // (100+130)/2, 250 rejected
        // No 16-bit overflow (the original gave 165 here).
        let im = Image::new(20, 20, [200; 3]);
        let out = cleaning_background(&im, im.rect(), 15, 15);
        assert_eq!(out.get(10, 10), [200; 3]);
    }

    #[test]
    fn right_column_is_read() {
        let mut im = Image::new(6, 1, [0; 3]);
        im.set(5, 0, [255; 3]);
        let out = cleaning_background(&im, im.rect(), 3, 3);
        assert_eq!(out.get(5, 0), [255; 3]);
        // Interior ROI that does not reach the border sees the true pixel.
        let mut im = Image::new(20, 1, [0; 3]);
        im.set(5, 0, [255; 3]);
        let out = cleaning_background(
            &im,
            Rect {
                x: 2,
                y: 0,
                w: 4,
                h: 1,
            },
            3,
            1,
        );
        assert_eq!(out.get(5, 0), [255; 3]);
    }

    #[test]
    fn roi_only() {
        let im = gradient(16, 16);
        let roi = Rect {
            x: 4,
            y: 4,
            w: 4,
            h: 4,
        };
        let out = mosaic(&im, roi, 3, 3);
        assert_eq!(out.get(0, 0), im.get(0, 0));
        assert_eq!(out.get(12, 12), im.get(12, 12));
    }
}
