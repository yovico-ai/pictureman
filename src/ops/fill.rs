//! Fill area: plain / fluctuated colour, gradients, patterns, patches —
//! recovered from PMAN.EXE 1.55.
//!
//! IMPORTANT — row order.  PMAN's work areas are DIBs whose row 0 is the
//! *bottom* of the picture, and every geometric formula below (gradient
//! direction, radial centre rounding, tile anchoring, pattern scaling, patch
//! scan order, noise row order) is defined on those bottom-up row numbers.
//! The functions take ordinary top-down `Image`s and convert internally with
//! `yb = h - 1 - y`.  (Evidence: the Gradient dialog labels colour #1
//! "bottom" for Vertical, and colour #1 is the value at row offset 0.)
//!
//! See `re/specs/tune_fill.md` for addresses, pseudocode and confidence.

use crate::core::{Image, Mask, MsRand, Rect, Rgb};

/// Default system colour from PMAN.INI `[COLOR] RED=0 GREEN=0 BLUE=255`
/// (`seg3:0cfc`); changed with Options/Color/Select (ChooseColor common
/// dialog with the CHOOSECOLOR template) or Options/Color/Pick up.
pub const DEFAULT_SYSTEM_COLOR: Rgb = [0, 0, 255];

/// Default gradient border colours: INI `[COLOR] LEFTRED/LEFTGREEN/LEFTBLUE`
/// and `RIGHTRED/...`, all defaulting to 100 (`seg3:0d63..0de4`).
pub const DEFAULT_GRADIENT_COLORS: (Rgb, Rgb) = ([100, 100, 100], [100, 100, 100]);

#[inline]
fn bu(h: usize, y: usize) -> usize {
    h - 1 - y
}

/// ROI rows in bottom-up numbering: (first bu row, count).
fn roi_bu(src: &Image, roi: Rect) -> (usize, usize) {
    let y1 = (roi.y + roi.h).min(src.h);
    let h = y1.saturating_sub(roi.y);
    (src.h - y1, h)
}

fn roi_x(src: &Image, roi: Rect) -> (usize, usize) {
    let x1 = (roi.x + roi.w).min(src.w);
    (roi.x, x1.saturating_sub(roi.x))
}

// ---------------------------------------------------------------------------
// Plain / Fluctuated colour
// ---------------------------------------------------------------------------

/// Fill/Color/Plain (cmd 153, `sub_29_060e`, callback `sub_29_02ea`): every
/// ROI pixel = the system colour (COLORREF DS:0x6e40).
pub fn fill_plain(src: &Image, roi: Rect, color: Rgb) -> Image {
    let mut out = src.clone();
    let (x0, w) = roi_x(src, roi);
    for y in roi.y..(roi.y + roi.h).min(src.h) {
        for x in x0..x0 + w {
            out.set(x, y, color);
        }
    }
    out
}

/// Parameters of the EXITATION ("Exitation parameters") dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fluctuation {
    /// "Grain size" scrollbar 1409: 1..=16, shown as "%dx%d" (DS:0x11e8 and
    /// 0x11ea, always equal).  Line step 1, page step 4.  Default 3.
    pub grain: i32,
    /// "Depth" scrollbar 1907: 1..=100 (DS:0x11ec), line 1 / page 4.  Default 50.
    pub depth: i32,
}

impl Default for Fluctuation {
    fn default() -> Self {
        Fluctuation {
            grain: 3,
            depth: 50,
        }
    }
}

/// `seg87` 2-D autoregressive noise generator, one row per call.
struct Noise {
    a: i32,
    b: i32,
    amp: i32,
    cur: Vec<i16>,
    prev: Vec<i16>,
}

impl Noise {
    /// `sub_87_0026(n, depth*8, grain_x, grain_y)`.
    fn new(n: usize, amp8: i32, gx: i32, gy: i32) -> Self {
        let coef = |g: i32| -> i32 {
            let m = ((g as f64) - 1.0).max(0.1);
            ((-1.0 / m).exp() * 64.0) as i32
        };
        let a = coef(gx);
        let b = coef(gy);
        let f = |c: i32| (c * c) as f64 * -0.000244140625 + 1.0;
        let amp = ((f(b) * f(a)).sqrt() * amp8 as f64) as i32;
        Noise {
            a,
            b,
            amp,
            cur: vec![0; n],
            prev: vec![0; n],
        }
    }

    /// `sub_87_0000(-amp, amp)` = rand() % (2*amp+1) - amp.
    fn uni(&self, rng: &mut MsRand) -> i16 {
        let span = 2 * self.amp + 1;
        (rng.rand() % span - self.amp) as i16
    }

    /// `sub_87_02bc(.., n, 0)`: returns the new row (second field disabled,
    /// but its first sample is still drawn).
    fn next_row(&mut self, rng: &mut MsRand) -> &[i16] {
        let n = self.cur.len();
        self.cur[0] = self.uni(rng);
        let _second_field = self.uni(rng);
        for i in 1..n {
            let r = self.uni(rng);
            let h = ((self.cur[i - 1] as i32 * self.a) / 64) as i16;
            self.cur[i] = h.wrapping_add(r);
        }
        for i in 1..n {
            let v = ((self.prev[i] as i32 * self.b) / 64) as i16;
            self.cur[i] = self.cur[i].wrapping_add(v);
        }
        std::mem::swap(&mut self.cur, &mut self.prev);
        &self.prev
    }
}

/// Win16 `MulDiv` (rounds to nearest, halves away from zero).
fn mul_div(a: i32, b: i32, c: i32) -> i32 {
    let p = a as i64 * b as i64;
    let neg = (p < 0) != (c < 0);
    let q = (p.abs() + (c.abs() as i64) / 2) / c.abs() as i64;
    (if neg { -q } else { q }) as i32
}

/// Fill/Color/Fluctuated (cmd 282, `sub_29_0ad0`, callback `sub_29_0642`):
/// the system colour plus correlated noise.
///
/// Noise: AR(1) in x and y with coefficients `a = trunc(64*exp(-1/max(g-1,0.1)))`
/// (/64), amplitude `trunc(sqrt((1-a²/4096)(1-a²/4096)) * depth*8)`, uniform
/// innovations `rand() % (2A+1) - A`.  Rows are ROI width + 10 wide; 4 warm-up
/// rows are generated first; the pixel at ROI column i uses sample i+10.
/// Each row consumes width+11 rand() calls.  Per pixel:
/// `d = MulDiv(n, max(R,G,B of colour), 255) / 8` (C truncation), and each
/// channel = clamp(c + d).  Rows are produced bottom-up.
///
/// The original does not reseed (`rand()` continues from the global state);
/// pass the session's `MsRand`.
pub fn fill_fluctuated(
    src: &Image,
    roi: Rect,
    color: Rgb,
    p: Fluctuation,
    rng: &mut MsRand,
) -> Image {
    let mut out = src.clone();
    let (x0, w) = roi_x(src, roi);
    let (yb0, h) = roi_bu(src, roi);
    if w == 0 || h == 0 {
        return out;
    }
    let maxc = color[0].max(color[1]).max(color[2]) as i32;
    let mut noise = Noise::new(w + 10, p.depth << 3, p.grain, p.grain);
    for _ in 0..4 {
        noise.next_row(rng);
    }
    for yb in yb0..yb0 + h {
        let row = noise.next_row(rng).to_vec();
        let y = bu(src.h, yb);
        for i in 0..w {
            let d = mul_div(row[i + 10] as i32, maxc, 255);
            let d = d / 8;
            let px = [
                (color[0] as i32 + d).clamp(0, 255) as u8,
                (color[1] as i32 + d).clamp(0, 255) as u8,
                (color[2] as i32 + d).clamp(0, 255) as u8,
            ];
            out.set(x0 + i, y, px);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Gradient
// ---------------------------------------------------------------------------

/// How the radial gradient measures distance; depends on the current area
/// tool in the original (DS:0x519a / pen shape DS:0x93f0).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RadialShape {
    /// Default (rectangle, polygon, whole, text, wand, freehand, square pen):
    /// Euclidean distance, radius = half the ROI diagonal.
    Diagonal,
    /// Ellipse area: y distance scaled by w/h, radius = w/2.
    Ellipse,
    /// Pen area with the circle pen: Euclidean, radius = max(w/2, h/2).
    Circle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GradientKind {
    /// cmd 221: colour #1 at the bottom row, towards colour #2 at the top.
    Vertical,
    /// cmd 222: colour #1 at the left, colour #2 at the right.
    Horizontal,
    /// cmd 223: colour #1 at the centre, colour #2 at the border.
    Radial(RadialShape),
}

/// `sub_29_1648` LUT: `((255-t)*c1 + t*c2) / 255` per channel.
pub fn gradient_lut(c1: Rgb, c2: Rgb) -> [Rgb; 256] {
    let mut t = [[0u8; 3]; 256];
    for (i, e) in t.iter_mut().enumerate() {
        for c in 0..3 {
            e[c] = (((255 - i as i32) * c1[c] as i32 + i as i32 * c2[c] as i32) / 255) as u8;
        }
    }
    t
}

/// Fill/Gradient (cmds 221–223, `sub_29_1648`, callback `sub_29_0b20`).
/// Always relative to the ROI (the selection's bounding box), committed
/// through the selection mask by the caller.  `c1`/`c2` are the GRADIENT
/// dialog's colour #1 (control 1602, INI LEFT*) and #2 (1601, INI RIGHT*).
///
/// Index t (0..255): Vertical `t = yb*255/h` (yb = bottom-up row offset),
/// Horizontal `t = x*255/w`, Radial `t = clamp(trunc(dist)*255/R)` with
/// `dist` from (w/2, h/2) (C-truncated halves).
pub fn gradient(src: &Image, roi: Rect, kind: GradientKind, c1: Rgb, c2: Rgb) -> Image {
    let lut = gradient_lut(c1, c2);
    let mut out = src.clone();
    let (x0, w) = roi_x(src, roi);
    let (yb0, h) = roi_bu(src, roi);
    if w == 0 || h == 0 {
        return out;
    }
    let (wi, hi) = (w as i32, h as i32);
    // radial set-up (seg29:0d3c..)
    let (radius, k2) = match kind {
        GradientKind::Radial(RadialShape::Ellipse) => {
            let r = (wi / 2).max(1);
            let k = (wi as f64 / hi as f64) as f32;
            let k2 = (k as f64 * k as f64) as f32;
            (r, k2 as f64)
        }
        GradientKind::Radial(RadialShape::Circle) => ((hi / 2).max(wi / 2).max(1), 1.0),
        _ => {
            let r = (((wi as f64 * wi as f64) + (hi as f64 * hi as f64)) * 0.25).sqrt() as i32;
            (r.max(1), 1.0)
        }
    };
    for j in 0..h {
        let yb = yb0 + j;
        let y = bu(src.h, yb);
        let dyr = j as i32 - hi / 2;
        for i in 0..w {
            let t = match kind {
                GradientKind::Vertical => (j as i32 * 255 / hi) as usize,
                GradientKind::Horizontal => (i as i32 * 255 / wi) as usize,
                GradientKind::Radial(_) => {
                    let dx = i as i32 - wi / 2;
                    let d2 = (dyr as f64 * dyr as f64) * k2 + (dx as f64 * dx as f64);
                    let dist = d2.sqrt() as i64;
                    (dist * 255 / radius as i64).clamp(0, 255) as usize
                }
            };
            out.set(x0 + i, y, lut[t]);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Pattern
// ---------------------------------------------------------------------------

/// Fill/Pattern/Tiled (cmd 224, `sub_11_174a` -> `sub_11_0000`).  The pattern
/// is an image file chosen with the standard Open dialog (`sub_11_1624`,
/// any readable format) and freed afterwards.  Tiles are anchored at the
/// image origin (bottom-left in DIB terms): pixel (x, yb) <- pattern
/// (x mod pw, yb mod ph), not at the ROI.
pub fn pattern_tiled(src: &Image, roi: Rect, pat: &Image) -> Image {
    let mut out = src.clone();
    if pat.w == 0 || pat.h == 0 {
        return out;
    }
    let (x0, w) = roi_x(src, roi);
    let (yb0, h) = roi_bu(src, roi);
    for yb in yb0..yb0 + h {
        let pyb = yb % pat.h;
        for x in x0..x0 + w {
            out.set(x, bu(src.h, yb), pat.get(x % pat.w, bu(pat.h, pyb)));
        }
    }
    out
}

/// Shared bilinear resampler of `sub_11_0498` (Scaled) / `sub_11_0d1c`
/// (Fitted): pattern window (offx, offy, winw, winh) stretched over the
/// whole image W×H, 4-bit fractional weights, `>>8` truncation; only the
/// ROI is written.  Two cached pattern rows with PMAN's exact reload rules.
fn pattern_resample(
    src: &Image,
    roi: Rect,
    pat: &Image,
    offx: i64,
    offy: i64,
    winw: i64,
    winh: i64,
) -> Image {
    let mut out = src.clone();
    let (pw, ph) = (pat.w as i64, pat.h as i64);
    let (big_w, big_h) = (src.w as i64, src.h as i64);
    let (x0, w) = roi_x(src, roi);
    let (yb0, h) = roi_bu(src, roi);
    if pw == 0 || ph == 0 || w == 0 || h == 0 {
        return out;
    }
    let pat_row = |syb: i64| -> Vec<Rgb> {
        let mut v = pat.row(bu(pat.h, syb as usize)).to_vec();
        v.push(*v.last().unwrap()); // buf[pw] = buf[pw-1]
        v
    };
    // GlobalAlloc without ZEROINIT in the original: initial contents undefined.
    let mut buf1 = vec![[0u8; 3]; pat.w + 1];
    let mut buf2 = vec![[0u8; 3]; pat.w + 1];
    let mut prev: i64 = -2;
    for yb in yb0..yb0 + h {
        let ybi = yb as i64;
        let sy = winh * ybi / big_h + offy;
        if sy != prev {
            if sy < ph {
                if sy == prev + 1 {
                    buf1.copy_from_slice(&buf2);
                } else {
                    buf1 = pat_row(sy);
                }
                if sy + 1 < ph {
                    buf2 = pat_row(sy + 1);
                }
            }
            prev = sy;
        }
        let fy = ((((winh * ybi) << 4) / big_h) & 15) as u32;
        let y = bu(src.h, yb);
        for x in x0..x0 + w {
            let xi = x as i64;
            let sx = (winw * xi / big_w + offx).min(pw) as usize;
            let fx = ((((winw * xi) << 4) / big_w) & 15) as u32;
            let w00 = (16 - fx) * (16 - fy);
            let w10 = fx * (16 - fy);
            let w01 = (16 - fx) * fy;
            let w11 = fx * fy;
            let sx1 = (sx + 1).min(pat.w); // sx < pw always in practice
            let mut px = [0u8; 3];
            for c in 0..3 {
                let s = w00 * buf1[sx][c] as u32
                    + w10 * buf1[sx1][c] as u32
                    + w01 * buf2[sx][c] as u32
                    + w11 * buf2[sx1][c] as u32;
                px[c] = ((s >> 8) & 0xff) as u8;
            }
            out.set(x, y, px);
        }
    }
    out
}

/// Fill/Pattern/Scaled (cmd 233, `sub_11_0498`): the pattern stretched
/// (bilinear, 1/16 steps) to the size of the whole image; the ROI shows the
/// corresponding part.
pub fn pattern_scaled(src: &Image, roi: Rect, pat: &Image) -> Image {
    pattern_resample(src, roi, pat, 0, 0, pat.w as i64, pat.h as i64)
}

/// Fill/Pattern/Fitted (cmd 234, `sub_11_0d1c`): like Scaled but the
/// pattern is first centre-cropped to the image's aspect ratio
/// ("cover", aspect preserved): if ph*W > pw*H the window is pw × pw*H/W
/// at y offset (ph-winh)/2, else ph*W/H × ph at x offset (pw-winw)/2.
pub fn pattern_fitted(src: &Image, roi: Rect, pat: &Image) -> Image {
    let (pw, ph) = (pat.w as i64, pat.h as i64);
    let (big_w, big_h) = (src.w as i64, src.h as i64);
    if pw == 0 || ph == 0 || big_w == 0 || big_h == 0 {
        return src.clone();
    }
    let (offx, offy, winw, winh) = if ph * big_w > pw * big_h {
        let winh = pw * big_h / big_w;
        (0, (ph - winh) / 2, pw, winh)
    } else {
        let winw = ph * big_w / big_h;
        ((pw - winw) / 2, 0, winw, ph)
    };
    pattern_resample(src, roi, pat, offx, offy, winw, winh)
}

// ---------------------------------------------------------------------------
// Patch
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PatchMode {
    /// cmd 155: blend of horizontal and vertical interpolation.
    Full,
    /// cmd 156: linear interpolation between the left/right ends of the run.
    Horizontal,
    /// cmd 157: linear interpolation between the top/bottom ends of the run.
    Vertical,
}

/// Fill/Patch (cmds 155–157, `sub_27_0e46`, callback `sub_27_09a9`,
/// per-pixel `sub_27_0000`, column set-up `sub_27_0694`, column rescan
/// `sub_32_02c6`).  Fills the selection by interpolating the colours found
/// at the ends of the selection's runs — the boundary pixels themselves are
/// the first/last selected pixels of each run (they are kept unchanged).
///
/// `mask` (non-zero = selected) gives the shape; with Area/Whole PMAN
/// substitutes a sharp rectangle covering the ROI (pass `Mask::full`).
///
/// Horizontal runs come from the current row restricted to the ROI columns;
/// for each column PMAN caches the *first* vertical run (scanning bottom-up
/// from row 0) and rescans from the current row once the row passes that
/// run's end.  Pixel rules, rows bottom-up, x ascending:
/// * x not strictly inside a run (left end, or 1-pixel run) -> unchanged;
/// * column run with T == B, or y <= T -> unchanged;
/// * H: `((x-L)*src[R] + (R-x)*src[L]) / max(R-L,1)`
/// * V: `(top*(B-y) + bot*(y-T)) / max(B-T,1)`
/// * Full: `wh = max(R-L,1)/2 - |x-(L+R)/2|`, `wv = max(B-T,1)/2 - |y-(T+B)/2|`
///   (each forced to 1 if <= 0), result `(wv*H + wh*V) / (wh+wv)`.
///
/// Fixed relative to the original: runs reaching the area's right edge or
/// the image's last row end on their last pixel (PMAN used one past the end
/// and read out of range), single-row selections are patched horizontally,
/// and results are clamped instead of wrapped.
pub fn patch(src: &Image, roi: Rect, mask: &Mask, mode: PatchMode) -> Image {
    let mut out = src.clone();
    let (big_w, big_h) = (src.w as i32, src.h as i32);
    let (x0, w) = roi_x(src, roi);
    let (yb0, h) = roi_bu(src, roi);
    if w == 0 || h == 0 {
        return out;
    }
    let inside = |x: i32, yb: i32| -> bool { mask.get(x as usize, bu(src.h, yb as usize)) != 0 };
    let px_bu = |x: i32, yb: i32| -> Rgb {
        let x = x.clamp(0, big_w - 1) as usize;
        let yb = yb.clamp(0, big_h - 1) as usize;
        src.get(x, bu(src.h, yb))
    };

    // column caches (sub_27_0694)
    let mut col_t = vec![big_h; src.w];
    let mut col_b = vec![big_h; src.w];
    let mut col_top = vec![[0u8; 3]; src.w];
    let mut col_bot = vec![[0u8; 3]; src.w];
    let scan = |x: i32, from: i32| -> (i32, i32) {
        let (mut t, mut b) = (big_h, big_h);
        let mut prev = false;
        let mut cnt = 0;
        let mut yy = from;
        while cnt < 2 && yy < big_h {
            let bit = inside(x, yy);
            if bit != prev {
                let v = if bit { yy } else { yy - 1 };
                if cnt == 0 {
                    t = v;
                } else {
                    b = v;
                }
                prev = bit;
                cnt += 1;
            }
            yy += 1;
        }
        // A run still open at the last row ends there. (The original set the
        // bottom to the image height — one past the end — also when the run
        // closed exactly on the last row.)
        match cnt {
            0 => (big_h, big_h),
            1 => (t, big_h - 1),
            _ => (t, b),
        }
    };
    for x in x0..x0 + w {
        let (t, b) = scan(x as i32, 0);
        col_t[x] = t;
        col_b[x] = b;
        col_top[x] = px_bu(x as i32, t);
        col_bot[x] = px_bu(x as i32, b);
    }

    let (rx0, rx1) = (x0 as i32, (x0 + w) as i32);
    for ybu in yb0..yb0 + h {
        let y = ybu as i32;
        // spans of this row (sub_32_0000): inclusive, open run -> xr = W
        let mut spans: Vec<(i32, i32)> = Vec::new();
        let mut start: Option<i32> = None;
        for x in rx0..rx1 {
            match (inside(x, y), start) {
                (true, None) => start = Some(x),
                (false, Some(s)) => {
                    spans.push((s, x - 1));
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(s) = start {
            // Open at the area's right edge: ends on the last selected pixel
            // (the original used the image width, one past the end).
            spans.push((s, rx1 - 1));
        }
        let yd = bu(src.h, ybu);
        for x in rx0..rx1 {
            let xu = x as usize;
            let mut l = rx0;
            let mut r = rx1 - 1;
            let mut tv = yb0 as i32;
            let mut bv = 2 * h as i32 - 1;
            let mut copy = false;
            for &(xl, xr) in &spans {
                if xr < x {
                    continue;
                }
                if xr == xl || xl >= x {
                    copy = true;
                    break;
                }
                l = xl;
                r = xr;
                break;
            }
            if copy {
                continue; // out already holds src
            }
            if col_b[xu] < y {
                let (t, b) = scan(x, y);
                col_t[xu] = t;
                col_b[xu] = b;
                col_top[xu] = px_bu(x, t);
                col_bot[xu] = px_bu(x, b);
            }
            let (t, b) = (col_t[xu], col_b[xu]);
            // A one-pixel column run has nothing to interpolate vertically;
            // the horizontal interpolation still applies. (The original left
            // such pixels unchanged, so one-row selections were never patched.)
            let flat = b == t;
            if flat && mode == PatchMode::Vertical {
                continue;
            }
            if !flat && t >= y {
                continue;
            }
            if t <= y && b >= y {
                tv = t;
                bv = b;
            }
            let a = px_bu(l, y);
            let bb = px_bu(r, y);
            let top = col_top[xu];
            let bot = col_bot[xu];
            let dh = (r - l).max(1) as i64;
            let dv = (bv - tv).max(1) as i64;
            let (xi, yi, li, ri, ti, bi) =
                (x as i64, y as i64, l as i64, r as i64, tv as i64, bv as i64);
            let mut px = [0u8; 3];
            for c in 0..3 {
                let hval = ((xi - li) * bb[c] as i64 + (ri - xi) * a[c] as i64) / dh;
                let vval = (top[c] as i64 * (bi - yi) + bot[c] as i64 * (yi - ti)) / dv;
                let v = match mode {
                    _ if flat => hval,
                    PatchMode::Horizontal => hval,
                    PatchMode::Vertical => vval,
                    PatchMode::Full => {
                        let mut wh = dh / 2 - (xi - (li + ri) / 2).abs();
                        let mut wv = dv / 2 - (yi - (ti + bi) / 2).abs();
                        if wh <= 0 {
                            wh = 1;
                        }
                        if wv <= 0 {
                            wv = 1;
                        }
                        (wv * hval + wh * vval) / (wh + wv)
                    }
                };
                px[c] = v.clamp(0, 255) as u8;
            }
            out.set(xu, yd, px);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_fills_roi_only() {
        let src = Image::new(3, 2, [1, 2, 3]);
        let out = fill_plain(
            &src,
            Rect {
                x: 1,
                y: 0,
                w: 2,
                h: 1,
            },
            [9, 8, 7],
        );
        assert_eq!(out.get(0, 0), [1, 2, 3]);
        assert_eq!(out.get(1, 0), [9, 8, 7]);
        assert_eq!(out.get(2, 1), [1, 2, 3]);
    }

    #[test]
    fn noise_coefficients() {
        let n = Noise::new(10, 400, 3, 3);
        assert_eq!(n.a, 38); // 64*exp(-0.5) = 38.8
        let n1 = Noise::new(10, 400, 1, 1);
        assert_eq!(n1.a, 0);
        assert_eq!(n1.amp, 400);
        // sqrt((1-38²/4096)²)*400 = (1-0.352539)*400 = 258.98
        assert_eq!(n.amp, 258);
    }

    #[test]
    fn fluctuated_is_deterministic_and_bounded() {
        let src = Image::new(20, 10, [0, 0, 0]);
        let mut r1 = MsRand::new(1);
        let mut r2 = MsRand::new(1);
        let a = fill_fluctuated(
            &src,
            src.rect(),
            [100, 150, 200],
            Fluctuation::default(),
            &mut r1,
        );
        let b = fill_fluctuated(
            &src,
            src.rect(),
            [100, 150, 200],
            Fluctuation::default(),
            &mut r2,
        );
        assert_eq!(a, b);
        // rand() consumption: (4 + rows) * (w + 11)
        let mut r3 = MsRand::new(1);
        for _ in 0..(4 + 10) * (20 + 11) {
            r3.rand();
        }
        assert_eq!(r1.rand(), r3.rand());
        // the same offset d is added to all channels
        for p in &a.px {
            let d = p[0] as i32 - 100;
            assert_eq!(p[1] as i32 - 150, d);
            assert_eq!(p[2] as i32 - 200, d);
        }
        assert!(a.px.iter().any(|p| p[0] != 100));
    }

    #[test]
    fn mul_div_rounds() {
        assert_eq!(mul_div(3, 1, 2), 2);
        assert_eq!(mul_div(-3, 1, 2), -2);
        assert_eq!(mul_div(100, 255, 255), 100);
    }

    #[test]
    fn gradient_vertical_starts_at_bottom() {
        let src = Image::new(1, 4, [0, 0, 0]);
        let out = gradient(
            &src,
            src.rect(),
            GradientKind::Vertical,
            [0, 0, 0],
            [255, 255, 255],
        );
        // bottom row t=0, top row t=3*255/4=191
        assert_eq!(out.get(0, 3), [0, 0, 0]);
        assert_eq!(out.get(0, 0), [191, 191, 191]);
        let out = gradient(
            &src,
            src.rect(),
            GradientKind::Horizontal,
            [10, 10, 10],
            [255, 255, 255],
        );
        assert_eq!(out.get(0, 0), [10, 10, 10]);
    }

    #[test]
    fn gradient_radial_centre_and_corner() {
        let src = Image::new(5, 5, [0, 0, 0]);
        let out = gradient(
            &src,
            src.rect(),
            GradientKind::Radial(RadialShape::Diagonal),
            [0, 0, 0],
            [255, 0, 0],
        );
        assert_eq!(out.get(2, 2), [0, 0, 0]);
        // R = trunc(sqrt(50*0.25)) = 3; corner dist trunc(sqrt(8)) = 2 -> 170
        assert_eq!(out.get(0, 0)[0], 170);
        let out = gradient(
            &src,
            src.rect(),
            GradientKind::Radial(RadialShape::Ellipse),
            [0, 0, 0],
            [255, 0, 0],
        );
        assert_eq!(out.get(0, 2)[0], 255); // dist 2, R = 2
    }

    #[test]
    fn tiled_anchor_bottom_left() {
        let mut pat = Image::new(2, 2, [0, 0, 0]);
        pat.set(0, 1, [1, 1, 1]); // bottom-left of the pattern
        let src = Image::new(4, 3, [9, 9, 9]);
        let out = pattern_tiled(&src, src.rect(), &pat);
        assert_eq!(out.get(0, 2), [1, 1, 1]); // image bottom-left
        assert_eq!(out.get(2, 2), [1, 1, 1]);
        assert_eq!(out.get(0, 0), [1, 1, 1]); // yb = 2 -> pattern bu row 0
        assert_eq!(out.get(1, 2), [0, 0, 0]);
    }

    #[test]
    fn scaled_identity_size() {
        let mut pat = Image::new(3, 3, [0, 0, 0]);
        for (i, p) in pat.px.iter_mut().enumerate() {
            *p = [i as u8 * 10, 0, 0];
        }
        let src = Image::new(3, 3, [0, 0, 0]);
        let out = pattern_scaled(&src, src.rect(), &pat);
        assert_eq!(out, pat);
        let out = pattern_fitted(&src, src.rect(), &pat);
        assert_eq!(out, pat);
    }

    #[test]
    fn fitted_crops_centre() {
        // pattern 4 wide x 2 high, image 2x2 -> window 2x2 at offx 1
        let mut pat = Image::new(4, 2, [0, 0, 0]);
        for y in 0..2 {
            for x in 0..4 {
                pat.set(x, y, [x as u8 * 50, 0, 0]);
            }
        }
        let src = Image::new(2, 2, [0, 0, 0]);
        let out = pattern_fitted(&src, src.rect(), &pat);
        assert_eq!(out.get(0, 0)[0], 50);
        assert_eq!(out.get(1, 1)[0], 100);
    }

    #[test]
    fn patch_horizontal_interpolates_run() {
        // 7x6 image, selection = rows 1..=3, x 1..=5; defect in row 2
        let mut src = Image::new(7, 6, [0, 0, 0]);
        src.set(5, 2, [200, 100, 40]);
        for x in 2..5 {
            src.set(x, 2, [255, 255, 255]);
        }
        let mut m = Mask::empty(7, 6);
        for y in 1..=3 {
            for x in 1..=5 {
                m.set(x, y, 255);
            }
        }
        let roi = m.bounds();
        // extend the ROI one column so the run closes inside it
        let roi = Rect {
            w: roi.w + 1,
            ..roi
        };
        let out = patch(&src, roi, &m, PatchMode::Horizontal);
        assert_eq!(out.get(1, 2), [0, 0, 0]);
        assert_eq!(out.get(3, 2), [100, 50, 20]);
        assert_eq!(out.get(5, 2), [200, 100, 40]);
        // first (bottom) row of each column run is kept
        assert_eq!(out.get(3, 3), src.get(3, 3));
    }

    #[test]
    fn patch_single_row_selection_is_patched() {
        let mut src = Image::new(5, 6, [0, 0, 0]);
        src.set(2, 2, [255, 255, 255]);
        let mut m = Mask::empty(5, 6);
        for x in 0..5 {
            m.set(x, 2, 255);
        }
        // The spot is interpolated away (the original left one-row
        // selections unchanged); vertical mode has nothing to interpolate.
        for mode in [PatchMode::Horizontal, PatchMode::Full] {
            let out = patch(
                &src,
                Rect {
                    x: 0,
                    y: 2,
                    w: 5,
                    h: 1,
                },
                &m,
                mode,
            );
            assert_eq!(out, Image::new(5, 6, [0, 0, 0]), "{mode:?}");
        }
        let out = patch(
            &src,
            Rect {
                x: 0,
                y: 2,
                w: 5,
                h: 1,
            },
            &m,
            PatchMode::Vertical,
        );
        assert_eq!(out, src);
    }

    #[test]
    fn patch_vertical_and_full() {
        // 3x8; columns 0..3 selected on rows 2..=5 (bu 5..2)
        let mut src = Image::new(3, 8, [0, 0, 0]);
        src.set(1, 5, [99, 99, 99]); // display-bottom end of the run (bu 2 = T)
        src.set(1, 2, [0, 0, 0]); // display-top end (bu 5 = B)
        for y in 3..5 {
            src.set(1, y, [255, 0, 0]);
        }
        let mut m = Mask::empty(3, 8);
        for y in 2..=5 {
            for x in 0..3 {
                m.set(x, y, 255);
            }
        }
        let roi = Rect {
            x: 0,
            y: 2,
            w: 3,
            h: 4,
        };
        let out = patch(&src, roi, &m, PatchMode::Vertical);
        assert_eq!(out.get(1, 4), [66, 66, 66]); // bu 3: 99*2/3
        assert_eq!(out.get(1, 3), [33, 33, 33]); // bu 4: 99*1/3
        assert_eq!(out.get(1, 5), [99, 99, 99]); // run start kept
        let full = patch(&src, roi, &m, PatchMode::Full);
        assert_eq!(full.get(0, 3), src.get(0, 3)); // left run end kept
    }
}
