//! Edit → Transformation: Size, Clip, Move, Flip, Rotate, Rubber and
//! Deformations, recovered from PMAN.EXE (see `re/specs/transform.md`).
//!
//! # Conventions of the original
//!
//! * **Coordinates are 1/16 pixel fixed point.** Every geometric operation is
//!   an *inverse* mapping `dst (x·16, y·16) → src (sx, sy)` (a far function
//!   pointer stored in DS:0x6e48), evaluated by a shared warp engine
//!   (`sub_15_0000` in place, `sub_15_0620` / `sub_15_1696` into a fragment).
//! * **Bilinear weights use a 1/15 step**: with `fx = sx & 15`, the weights
//!   are `(15-fx)` and `fx`, the four products are divided by 225
//!   (truncating). So a fraction of 15/16 already gives full weight to the
//!   next pixel. Neighbours outside the source take the background colour.
//! * **Images are stored bottom-up** (DIB order) in MWAREA: row 0 is the
//!   bottom of the picture (the selection mask is drawn at GDI `y = H - y`,
//!   and positive rotation angles are counter-clockwise as the manual says).
//!   All maths here runs in that bottom-up space ("WA" coordinates) so that
//!   rounding, phase origins and the CW/CCW sense match; our `Image` is
//!   top-down, so the functions flip on entry and exit.
//! * Integer arithmetic mirrors the MS C long helpers: `/` truncates toward
//!   zero (`_aFldiv`), `>>` on fixed point is arithmetic (floor), `_ftol`
//!   truncates. Intermediate products that could overflow 32 bits in the
//!   original (selections larger than ~500–700 px) are computed in 64 bits
//!   here; the original would wrap (see the spec).
//!
//! # Whole image vs. selected area
//!
//! The in-place functions (`flip_*`, `deform`, `rubber`) correspond to the
//! "whole image" / painting / rectangle path: the ROI is warped in place and
//! the caller commits through the selection mask. For other area types the
//! original *extracts a fragment* (pixels whose source falls outside the
//! selection become transparent), lets the user move/resize its rectangle,
//! and pastes it back with [`place_fragment`]; the `*_fragment` functions
//! produce that fragment.

use crate::core::{Image, Mask, Rect, Rgb};

// ---------------------------------------------------------------------------
// Fixed-point helpers mirroring the MS C runtime
// ---------------------------------------------------------------------------

/// `_ftol`: truncation toward zero.
#[inline]
fn ftol(v: f64) -> i64 {
    v.trunc() as i64
}

/// Bottom-up ("WA") copy of a top-down image.
fn to_wa(img: &Image) -> Image {
    let mut out = Image::new(img.w, img.h, [0; 3]);
    for y in 0..img.h {
        out.row_mut(img.h - 1 - y).copy_from_slice(img.row(y));
    }
    out
}

/// Inverse of [`to_wa`] (the flip is its own inverse).
fn from_wa(img: &Image) -> Image {
    to_wa(img)
}

fn mask_to_wa(m: &Mask) -> Mask {
    let mut out = Mask::empty(m.w, m.h);
    for y in 0..m.h {
        let (a, b) = (y * m.w, (m.h - 1 - y) * m.w);
        out.data[b..b + m.w].copy_from_slice(&m.data[a..a + m.w]);
    }
    out
}

/// Integer rectangle in WA (bottom-up) coordinates, the DS:0x9578 block
/// (x0, y0, w, h).
#[derive(Clone, Copy, Debug)]
struct WaRect {
    x0: i64,
    y0: i64,
    w: i64,
    h: i64,
}

fn rect_to_wa(r: Rect, img_h: usize) -> WaRect {
    WaRect {
        x0: r.x as i64,
        y0: img_h as i64 - (r.y + r.h) as i64,
        w: r.w as i64,
        h: r.h as i64,
    }
}

/// Clamp a rectangle to the image; returns `None` if empty.
fn clamp_rect(r: Rect, w: usize, h: usize) -> Option<Rect> {
    let x1 = (r.x + r.w).min(w);
    let y1 = (r.y + r.h).min(h);
    if r.x >= x1 || r.y >= y1 {
        return None;
    }
    Some(Rect {
        x: r.x,
        y: r.y,
        w: x1 - r.x,
        h: y1 - r.y,
    })
}

/// An inverse mapping in 1/16-pixel WA coordinates.
trait Map {
    fn map(&self, x16: i64, y16: i64) -> (i64, i64);
}

impl<F: Fn(i64, i64) -> (i64, i64)> Map for F {
    fn map(&self, x16: i64, y16: i64) -> (i64, i64) {
        self(x16, y16)
    }
}

/// 4-tap bilinear fetch with the original's 1/15 weights (`sub_15_0000`,
/// seg15:025e..0374). `get` returns `None` for pixels outside the source.
#[inline]
fn bilinear15(sx: i64, sy: i64, get: impl Fn(i64, i64) -> Option<Rgb>, bg: Rgb) -> Rgb {
    let (ix, iy) = (sx >> 4, sy >> 4);
    let (fx, fy) = ((sx & 15) as u32, (sy & 15) as u32);
    let p00 = get(ix, iy).unwrap_or(bg);
    let p10 = get(ix + 1, iy).unwrap_or(bg);
    let p11 = get(ix + 1, iy + 1).unwrap_or(bg);
    let p01 = get(ix, iy + 1).unwrap_or(bg);
    let w00 = (15 - fx) * (15 - fy);
    let w10 = fx * (15 - fy);
    let w01 = (15 - fx) * fy;
    let w11 = fx * fy;
    let mut out = [0u8; 3];
    for c in 0..3 {
        let s =
            w00 * p00[c] as u32 + w10 * p10[c] as u32 + w01 * p01[c] as u32 + w11 * p11[c] as u32;
        out[c] = (s / 225) as u8;
    }
    out
}

/// In-place warp engine `sub_15_0000` (seg15:0000): for every pixel of the
/// rectangle (clipped to the image) the inverse mapping is evaluated at
/// `(x·16, y·16)` and the source is sampled bilinearly; neighbours outside the
/// image take `bg`. Pixels outside the rectangle are copied unchanged.
fn warp_inplace(src: &Image, r: WaRect, bg: Rgb, m: &dyn Map) -> Image {
    let mut out = src.clone();
    let get = |x: i64, y: i64| {
        if x >= 0 && y >= 0 && (x as usize) < src.w && (y as usize) < src.h {
            Some(src.get(x as usize, y as usize))
        } else {
            None
        }
    };
    let x_end = (r.x0 + r.w).min(src.w as i64);
    let y_end = (r.y0 + r.h).min(src.h as i64);
    for y in r.y0.max(0)..y_end {
        for x in r.x0.max(0)..x_end {
            let (sx, sy) = m.map(x << 4, y << 4);
            out.set(x as usize, y as usize, bilinear15(sx, sy, get, bg));
        }
    }
    out
}

/// A transformed fragment waiting to be placed: the original keeps it in a
/// temporary image where "transparent" pixels carry the key colour
/// `(0xfb,0xfc,0xfd)`; here transparency is a separate 0/255 mask.
/// `x`, `y` are the fragment's top-left in (top-down) image coordinates and
/// may be negative (e.g. a rotated fragment larger than the image).
#[derive(Clone, Debug)]
pub struct Fragment {
    pub x: i64,
    pub y: i64,
    pub img: Image,
    pub alpha: Mask,
}

/// Fragment extraction `sub_15_0620` (seg15:0620) — used by Rotate, Rubber and
/// Deformations in area mode. For each pixel of the rectangle (not clipped)
/// the source position rounded to nearest (`(s+8)>>4`) must lie inside the
/// image and the selection, otherwise the pixel is transparent; inside, the
/// value is the 1/15 bilinear sample. (Out-of-image neighbours reuse stale
/// values in the original; we clamp to the edge.)
fn extract_bilinear(src: &Image, sel: &Mask, r: WaRect, m: &dyn Map) -> (Image, Mask) {
    let (w, h) = (r.w.max(0) as usize, r.h.max(0) as usize);
    let mut img = Image::new(w, h, [0; 3]);
    let mut alpha = Mask::empty(w, h);
    let clampget = |x: i64, y: i64| {
        let x = x.clamp(0, src.w as i64 - 1) as usize;
        let y = y.clamp(0, src.h as i64 - 1) as usize;
        Some(src.get(x, y))
    };
    for j in 0..h {
        let y = r.y0 + j as i64;
        for i in 0..w {
            let x = r.x0 + i as i64;
            let (sx, sy) = m.map(x << 4, y << 4);
            let (rx, ry) = ((sx + 8) >> 4, (sy + 8) >> 4);
            if rx < 0 || ry < 0 || rx >= src.w as i64 || ry >= src.h as i64 {
                continue;
            }
            if sel.get(rx as usize, ry as usize) == 0 {
                continue;
            }
            img.set(i, j, bilinear15(sx, sy, clampget, [0; 3]));
            alpha.set(i, j, 255);
        }
    }
    (img, alpha)
}

/// Fragment extraction `sub_15_1696` (seg15:1696) — Flip and Move in area
/// mode: nearest (floor) source pixel, clamped to the image, transparent if
/// outside the selection. The loop is clipped to the image.
fn extract_nearest(src: &Image, sel: &Mask, r: WaRect, m: &dyn Map) -> (Image, Mask) {
    let (w, h) = (r.w.max(0) as usize, r.h.max(0) as usize);
    let mut img = Image::new(w, h, [0; 3]);
    let mut alpha = Mask::empty(w, h);
    let x_end = (r.x0 + r.w).min(src.w as i64);
    let y_end = (r.y0 + r.h).min(src.h as i64);
    for y in r.y0.max(0)..y_end {
        for x in r.x0.max(0)..x_end {
            let (sx, sy) = m.map(x << 4, y << 4);
            let px = (sx >> 4).clamp(0, src.w as i64 - 1) as usize;
            let py = (sy >> 4).clamp(0, src.h as i64 - 1) as usize;
            if sel.get(px, py) == 0 {
                continue;
            }
            let (i, j) = ((x - r.x0) as usize, (y - r.y0) as usize);
            img.set(i, j, src.get(px, py));
            alpha.set(i, j, 255);
        }
    }
    (img, alpha)
}

/// Convert a WA fragment (rows bottom-up, rect in WA) to a top-down [`Fragment`].
fn wa_fragment(img: Image, alpha: Mask, r: WaRect, img_h: usize) -> Fragment {
    Fragment {
        x: r.x0,
        y: img_h as i64 - (r.y0 + r.h),
        img: from_wa(&img),
        alpha: mask_to_wa(&alpha),
    }
}

// ---------------------------------------------------------------------------
// Size (ID 102)
// ---------------------------------------------------------------------------

/// Edit → Transformation → Size: resample the whole image to `w × h`
/// (`sub_60_0916`, called from the SIZEDLG handler `sub_60_0f40`).
///
/// Bilinear with 4-bit fractions and **top-left aligned** sample positions,
/// no box filtering when shrinking:
/// `sy = r·sh/dh`, `fy = ((r·sh)<<4)/dh & 15` (same for x),
/// `out = ((16-fx)(16-fy)·p00 + fx(16-fy)·p10 + (16-fx)fy·p01 + fx·fy·p11) >> 8`.
/// The right/bottom neighbour past the edge replicates the last pixel.
/// Rows are processed in the bottom-up storage order (matters only for the
/// alignment of the vertical sampling grid).
pub fn resize(src: &Image, w: usize, h: usize) -> Image {
    if w == 0 || h == 0 || src.w == 0 || src.h == 0 {
        return Image::new(w, h, [0; 3]);
    }
    let s = to_wa(src);
    let (sw, sh, dw, dh) = (s.w as i64, s.h as i64, w as i64, h as i64);
    let mut out = Image::new(w, h, [0; 3]);
    for r in 0..dh {
        let sy = (sh * r / dh).min(sh - 1);
        let fy = ((((sh * r) << 4) / dh) & 15) as u32;
        let ra = s.row(sy as usize);
        let rb = s.row((sy + 1).min(sh - 1) as usize);
        for c in 0..dw {
            let sx = (c * sw / dw).min(sw - 1) as usize;
            let fx = ((((c * sw) << 4) / dw) & 15) as u32;
            let sx1 = (sx + 1).min(s.w - 1);
            let w00 = (16 - fx) * (16 - fy);
            let w10 = fx * (16 - fy);
            let w01 = (16 - fx) * fy;
            let w11 = fx * fy;
            let mut px = [0u8; 3];
            for k in 0..3 {
                let v = w00 * ra[sx][k] as u32
                    + w10 * ra[sx1][k] as u32
                    + w01 * rb[sx][k] as u32
                    + w11 * rb[sx1][k] as u32;
                px[k] = (v >> 8) as u8;
            }
            out.set(c as usize, r as usize, px);
        }
    }
    from_wa(&out)
}

// ---------------------------------------------------------------------------
// Clip (ID 108)
// ---------------------------------------------------------------------------

/// Edit → Transformation → Clip (`sub_56_02a8` → `sub_56_0000`): the image
/// becomes the selected rectangle, clamped to the image. Returns `None` if the
/// rectangle does not intersect the image.
pub fn clip(src: &Image, r: Rect) -> Option<Image> {
    clamp_rect(r, src.w, src.h).map(|r| src.crop(r))
}

// ---------------------------------------------------------------------------
// Flip (IDs 136 / 140)
// ---------------------------------------------------------------------------

fn flip_map(r: WaRect, vertical: bool) -> impl Fn(i64, i64) -> (i64, i64) {
    // sub_35_0050 (vertical) / sub_35_009a (horizontal):
    // s = ((2·x0 + w − 1) << 4) − x
    let cx = (2 * r.x0 + r.w - 1) << 4;
    let cy = (2 * r.y0 + r.h - 1) << 4;
    move |x, y| if vertical { (x, cy - y) } else { (cx - x, y) }
}

/// Flip Horizontal (ID 136, `sub_35_0126(0)`): mirror the ROI left↔right in
/// place. The mapping is integral, so the bilinear engine reproduces pixels
/// exactly.
pub fn flip_horizontal(src: &Image, roi: Rect) -> Image {
    flip_impl(src, roi, false)
}

/// Flip Vertical (ID 140, `sub_35_0126(1)`): mirror the ROI top↔bottom in place.
pub fn flip_vertical(src: &Image, roi: Rect) -> Image {
    flip_impl(src, roi, true)
}

fn flip_impl(src: &Image, roi: Rect, vertical: bool) -> Image {
    let Some(roi) = clamp_rect(roi, src.w, src.h) else {
        return src.clone();
    };
    let wa = to_wa(src);
    let r = rect_to_wa(roi, src.h);
    from_wa(&warp_inplace(&wa, r, [0; 3], &flip_map(r, vertical)))
}

/// Area-mode flip: nearest extraction (`sub_15_1696`) of the mirrored
/// selection, to be placed with [`place_fragment`] (initially at its own rect).
pub fn flip_fragment(src: &Image, sel: &Mask, roi: Rect, vertical: bool) -> Fragment {
    let wa = to_wa(src);
    let r = rect_to_wa(roi, src.h);
    let (img, alpha) = extract_nearest(&wa, &mask_to_wa(sel), r, &flip_map(r, vertical));
    wa_fragment(img, alpha, r, src.h)
}

// ---------------------------------------------------------------------------
// Move (ID 167)
// ---------------------------------------------------------------------------

/// Edit → Transformation → Move (`sub_5_007a`), area mode: copy the selected
/// pixels (identity mapping, `sub_15_1696`) into a fragment; the user then
/// drags/resizes its rectangle and [`place_fragment`] pastes it. The source
/// pixels are *not* erased (the manual calls it "Clone"). Not available for
/// the whole image.
pub fn move_fragment(src: &Image, sel: &Mask, roi: Rect) -> Fragment {
    let wa = to_wa(src);
    let r = rect_to_wa(roi, src.h);
    let (img, alpha) = extract_nearest(&wa, &mask_to_wa(sel), r, &|x, y| (x, y));
    wa_fragment(img, alpha, r, src.h)
}

/// Move in painting mode (`sub_5_0000` mapping via `sub_15_0000`): every
/// painted pixel takes the pixel `(dx, dy)` away from it, i.e. a clone brush
/// with offset `reference − brush`. `dx`, `dy` are in top-down pixels.
pub fn clone_offset(src: &Image, roi: Rect, dx: i64, dy: i64, bg: Rgb) -> Image {
    let Some(roi) = clamp_rect(roi, src.w, src.h) else {
        return src.clone();
    };
    let wa = to_wa(src);
    let r = rect_to_wa(roi, src.h);
    // WA y axis is flipped: a top-down offset dy is −dy in WA.
    let (ox, oy) = (dx << 4, (-dy) << 4);
    from_wa(&warp_inplace(&wa, r, bg, &move |x, y| (x - ox, y - oy)))
}

// ---------------------------------------------------------------------------
// Placement of a fragment (double-click to accept)
// ---------------------------------------------------------------------------

/// Paste a fragment scaled into the rectangle `(x, y, w, h)` (top-down image
/// coordinates, may extend past the image) — `sub_50_0000` (seg50:0000).
///
/// * Sampling is the Size resampler (4-bit bilinear, top-left aligned,
///   edge-replicated): fragment pixel `(c·fw/w, r·fh/h)`.
/// * A new selection mask is built from the bilinear *opacity* of the four
///   fragment neighbours (transparent = 0, opaque = 255, weights /256):
///   opacity ≥ 128 → selected. (With Edge "smooth" options the original then
///   blurs this mask by min(w,h)/20, /8 or /4 — not reproduced here.)
/// * Colours: transparent neighbours are replaced by the destination pixel
///   underneath before interpolating; if all four are transparent the
///   destination pixel is kept.
///
/// Returns the composited image (new pixels where the new mask is set) and
/// the new mask.
pub fn place_fragment(
    dst: &Image,
    frag: &Fragment,
    x: i64,
    y: i64,
    w: usize,
    h: usize,
) -> (Image, Mask) {
    let under = to_wa(dst);
    let fimg = to_wa(&frag.img);
    let falpha = mask_to_wa(&frag.alpha);
    let (fw, fh) = (fimg.w as i64, fimg.h as i64);
    let mut out = under.clone();
    let mut mask = Mask::empty(dst.w, dst.h);
    if fw == 0 || fh == 0 || w == 0 || h == 0 {
        return (dst.clone(), mask);
    }
    let (iw, ih) = (dst.w as i64, dst.h as i64);
    // Target rect in WA (L, T, R, B exclusive).
    let l = x;
    let rr = x + w as i64;
    let t = ih - (y + h as i64);
    let b = ih - y;
    let (cx0, cy0, cx1, cy1) = (l.max(0), t.max(0), rr.min(iw), b.min(ih));
    if cx0 >= cx1 || cy0 >= cy1 {
        return (dst.clone(), mask);
    }
    let rw = (rr - l).max(1);
    let rh = (b - t).max(1);
    let sxa = (cx0 - l) * fw / rw;
    let sxb = (cx1 - l) * fw / rw;
    let sya = (cy0 - t) * fh / rh;
    let syb = (cy1 - t) * fh / rh;
    let (spw, sph) = (sxb - sxa, syb - sya);
    let (dw, dh) = (cx1 - cx0, cy1 - cy0);
    let fget = |i: i64, j: i64| -> (Rgb, bool) {
        // Row buffers hold `spw` pixels starting at sxa plus one duplicated
        // pixel; the second row is not reloaded past `syb` (edge replicate).
        let i = (sxa + i.min(spw - 1).max(0)).clamp(0, fw - 1) as usize;
        let j = j.clamp(sya, (syb - 1).max(sya)).clamp(0, fh - 1) as usize;
        (fimg.get(i, j), falpha.get(i, j) != 0)
    };
    for yy in cy0..cy1 {
        let r = yy - cy0;
        let srow = r * sph / dh + sya;
        let fy = ((((r * sph) << 4) / dh) & 15) as u32;
        for xx in cx0..cx1 {
            let c = xx - cx0;
            let sx = c * spw / dw;
            let fx = ((((c * spw) << 4) / dw) & 15) as u32;
            let (a0, oa0) = fget(sx, srow);
            let (a1, oa1) = fget(sx + 1, srow);
            let (b0, ob0) = fget(sx, srow + 1);
            let (b1, ob1) = fget(sx + 1, srow + 1);
            let w00 = (16 - fx) * (16 - fy);
            let w10 = fx * (16 - fy);
            let w01 = (16 - fx) * fy;
            let w11 = fx * fy;
            let op = |o: bool| if o { 255u32 } else { 0 };
            let opacity = (op(oa0) * w00 + op(oa1) * w10 + op(ob0) * w01 + op(ob1) * w11) >> 8;
            let upx = under.get(xx as usize, yy as usize);
            let pick = |p: Rgb, o: bool| if o { p } else { upx };
            let (a0, a1, b0, b1) = (pick(a0, oa0), pick(a1, oa1), pick(b0, ob0), pick(b1, ob1));
            let mut px = [0u8; 3];
            for k in 0..3 {
                let v = w00 * a0[k] as u32
                    + w10 * a1[k] as u32
                    + w01 * b0[k] as u32
                    + w11 * b1[k] as u32;
                px[k] = (v >> 8) as u8;
            }
            if opacity >= 0x80 {
                out.set(xx as usize, yy as usize, px);
                mask.set(xx as usize, (ih - 1 - yy) as usize, 255);
            }
        }
    }
    (from_wa(&out), mask)
}

// ---------------------------------------------------------------------------
// Rotate (ID 137)
// ---------------------------------------------------------------------------

/// π/180 as stored in DS:0x28a8 (the program uses π ≈ 3.1415).
const DEG2RAD: f64 = 0.01745277777777778;

/// `(C, S) = (trunc(cos·4096.5), trunc(sin·4096.5))` (seg64:016a..01c1).
fn rot_coeffs(degrees: f64) -> (i64, i64) {
    let a = DEG2RAD * degrees;
    (ftol(a.cos() * 4096.5), ftol(a.sin() * 4096.5))
}

/// Rotation mapping `sub_64_0000`: with `u = x − (W'−1)·8`, `v = y − (H'−1)·8`
/// (1/16 px, relative to the destination centre),
/// `sx = (u·C + v·S)/4096 + (W−1)·8`, `sy = (v·C − u·S)/4096 + (H−1)·8`.
/// `dc` / `sc` are the doubled-centre values DS:0x34f4/34f6 and 0x34f0/34f2.
fn rot_map(
    c: i64,
    s: i64,
    dcx: i64,
    dcy: i64,
    scx: i64,
    scy: i64,
) -> impl Fn(i64, i64) -> (i64, i64) {
    move |x, y| {
        let u = x + ((1 - dcx) << 3);
        let v = y + ((1 - dcy) << 3);
        let sx = (u * c + v * s) / 4096 + ((scx - 1) << 3);
        let sy = (v * c - u * s) / 4096 + ((scy - 1) << 3);
        (sx, sy)
    }
}

/// Edit → Transformation → Rotate on the whole image (`sub_64_046a` →
/// `sub_64_011e`): counter-clockwise by `degrees` (ROTATE dialog, default
/// 90), bilinear (1/15 weights), uncovered area and out-of-image neighbours
/// filled with `bg` (the current background colour, DS:0x6e40).
///
/// Output size: for exact multiples of 90° the size is kept (×180) or
/// swapped (×90); otherwise `W' = (|C·W| + |S·H|)/4096`,
/// `H' = (|C·H| + |S·W|)/4096`, the bounding box of the rotated image.
/// Returns `None` for a non-finite angle or an output over
/// [`crate::core::MAX_PIXELS`].
///
/// Fixed: the original left out the absolute values (the area-mode
/// rotation has them), so angles outside 0..90 cropped the canvas or failed
/// with "not enough memory"; it also treated e.g. 90.5° as exactly 90°.
pub fn rotate(src: &Image, degrees: f64, bg: Rgb) -> Option<Image> {
    if !degrees.is_finite() {
        return None;
    }
    let degrees = degrees % 360.0;
    let (c, s) = rot_coeffs(degrees);
    let (w, h) = (src.w as i64, src.h as i64);
    let (nw, nh) = if degrees.fract() == 0.0 && (degrees as i64) % 90 == 0 {
        if (degrees as i64) % 180 == 0 {
            (w, h)
        } else {
            (h, w)
        }
    } else {
        (
            ((c * w).abs() + (s * h).abs()) / 4096,
            ((c * h).abs() + (s * w).abs()) / 4096,
        )
    };
    if nw <= 0 || nh <= 0 || crate::core::checked_area(nw as usize, nh as usize).is_none() {
        return None;
    }
    let wa = to_wa(src);
    let canvas = Image::new(nw as usize, nh as usize, bg);
    // sub_15_0000(src → new image) over the whole new canvas.
    let m = rot_map(c, s, nw, nh, w, h);
    let mut out = canvas;
    let get = |x: i64, y: i64| {
        if x >= 0 && y >= 0 && x < w && y < h {
            Some(wa.get(x as usize, y as usize))
        } else {
            None
        }
    };
    for y in 0..nh {
        for x in 0..nw {
            let (sx, sy) = m(x << 4, y << 4);
            out.set(x as usize, y as usize, bilinear15(sx, sy, get, bg));
        }
    }
    Some(from_wa(&out))
}

/// Rotate in area mode (`sub_64_0548`): the fragment's new size is
/// `W' = (|C·w| + |S·h|)/4096`, `H' = (|C·h| + |S·w|)/4096` (or kept/swapped
/// for multiples of 90°), centred on the selection's centre
/// (`x0' = (2·x0 + w − W')/2`, truncating). Pixels whose source lies outside
/// the selection are transparent. Place it with [`place_fragment`].
pub fn rotate_fragment(src: &Image, sel: &Mask, roi: Rect, degrees: f64) -> Fragment {
    let (c, s) = rot_coeffs(degrees);
    let r = rect_to_wa(roi, src.h);
    let ia = ftol(degrees);
    let (nw, nh) = if ia % 90 == 0 {
        if ia % 180 == 0 {
            (r.w, r.h)
        } else {
            (r.h, r.w)
        }
    } else {
        (
            ((c * r.w).abs() + (s * r.h).abs()) / 4096,
            ((c * r.h).abs() + (s * r.w).abs()) / 4096,
        )
    };
    let nr = WaRect {
        x0: (2 * r.x0 - nw + r.w) / 2,
        y0: (2 * r.y0 - nh + r.h) / 2,
        w: nw,
        h: nh,
    };
    // DS:0x34f0 = 0x34f4 = 2·x0 + w (both centres coincide).
    let (dcx, dcy) = (2 * r.x0 + r.w, 2 * r.y0 + r.h);
    let wa = to_wa(src);
    let (img, alpha) = extract_bilinear(
        &wa,
        &mask_to_wa(sel),
        nr,
        &rot_map(c, s, dcx, dcy, dcx, dcy),
    );
    wa_fragment(img, alpha, nr, src.h)
}

// ---------------------------------------------------------------------------
// Rubber (ID 228)
// ---------------------------------------------------------------------------

/// Tables built by `sub_14_0000` (seg14:0000).
struct Rubber {
    r: WaRect,
    /// X[i], i in 0..w: horizontal influence 0..4096 (X[0] = 0).
    xt: Vec<i64>,
    /// Y[j], j in 0..=h.
    yt: Vec<i64>,
    /// Ellipse chord tables (area type 2): E_row[j] = width of the ellipse at
    /// row j (scaled by w), E_col[i] = height at column i (scaled by h).
    e_row: Vec<i64>,
    e_col: Vec<i64>,
    dx: i64,
    dy: i64,
}

impl Rubber {
    /// `p16`: reference point (the *dragged-to* point, DS:0x6e4e/6e5c ·16),
    /// `d16`: displacement `from − to` (·16), all in WA coordinates.
    fn new(r: WaRect, p16: (i64, i64), d16: (i64, i64), elliptic: bool) -> Self {
        let (w, h) = (r.w, r.h);
        let px = (p16.0 - (r.x0 << 4)) >> 4;
        let py = (p16.1 - (r.y0 << 4)) >> 4;
        // influence(i): d_b² / (d_p² + d_b²), d_b = distance to the boundary on
        // the reference side, d_p = distance to the reference.
        let infl = |i: i64, p: i64, n: i64| -> i64 {
            let db = if p >= i { i } else { i - n };
            let dp = p - i;
            ((db * db) << 12) / (dp * dp + db * db)
        };
        let chord = |t: i64, n: i64, scale: i64| -> i64 {
            let v = (t * t) as f64 / (n * n) as f64 * -4.0 + 1.0;
            ftol(v.max(0.0).sqrt() * scale as f64)
        };
        let mut xt = vec![0i64; (w + 16) as usize];
        let mut e_col = vec![0i64; (w + 16) as usize];
        for i in 1..w {
            xt[i as usize] = infl(i, px, w);
            if elliptic {
                e_col[i as usize] = chord(i - w / 2, w, h);
            }
        }
        let mut yt = vec![0i64; (h + 16) as usize];
        let mut e_row = vec![0i64; (h + 16) as usize];
        for j in 1..=h {
            yt[j as usize] = infl(j, py, h);
            if elliptic {
                e_row[j as usize] = chord(j - h / 2, h, w);
            }
        }
        Rubber {
            r,
            xt,
            yt,
            e_row,
            e_col,
            dx: d16.0,
            dy: d16.1,
        }
    }

    fn inside(&self, x: i64, y: i64) -> bool {
        let r = self.r;
        !(r.x0 << 4 > x || (r.x0 + r.w) << 4 <= x || r.y0 << 4 > y || (r.y0 + r.h) << 4 <= y)
    }

    /// Rectangular rubber, `sub_14_0372`:
    /// `sx = x + (X[i]·dx/4096)·Y[j]/4096`, `sy = y + (Y[j]·dy/4096)·X[i]/4096`.
    fn map_rect(&self, x: i64, y: i64) -> (i64, i64) {
        if !self.inside(x, y) {
            return (x, y);
        }
        let i = ((x >> 4) - self.r.x0) as usize;
        let j = ((y >> 4) - self.r.y0) as usize;
        let sx = x + (self.xt[i] * self.dx / 4096) * self.yt[j] / 4096;
        let sy = y + (self.yt[j] * self.dy / 4096) * self.xt[i] / 4096;
        (sx, sy)
    }

    /// Elliptic rubber, `sub_14_04bc`: the position is first stretched from
    /// the ellipse chord to the full width/height, and the displacement is
    /// further scaled by the chord ratios.
    fn map_ellipse(&self, x: i64, y: i64) -> (i64, i64) {
        if !self.inside(x, y) {
            return (x, y);
        }
        let (w, h) = (self.r.w, self.r.h);
        let ix = (x >> 4) - self.r.x0;
        let iy = (y >> 4) - self.r.y0;
        let er = self.e_row[iy as usize];
        let ec = self.e_col[ix as usize];
        let ie = if er != 0 {
            ((x >> 4) - (w - er) / 2 - self.r.x0) * w / er
        } else {
            0
        };
        let je = if ec != 0 {
            ((y >> 4) - (h - ec) / 2 - self.r.y0) * h / ec
        } else {
            0
        };
        if ie < 0 || ie >= w || je < 0 || je >= h {
            return (x, y);
        }
        let (xe, ye) = (self.xt[ie as usize], self.yt[je as usize]);
        let sx = x + ((xe * self.dx / 4096) * ye / 4096) * er / h * ec / w;
        let sy = y + ((ye * self.dy / 4096) * xe / 4096) * er / h * ec / w;
        (sx, sy)
    }
}

fn rubber_setup(
    src: &Image,
    roi: Rect,
    from: (i64, i64),
    to: (i64, i64),
    elliptic: bool,
) -> (WaRect, Rubber) {
    let r = rect_to_wa(roi, src.h);
    let hh = src.h as i64;
    // WA y = H − 1 − y.
    let (fx, fy) = (from.0, hh - 1 - from.1);
    let (tx, ty) = (to.0, hh - 1 - to.1);
    let rb = Rubber::new(
        r,
        (tx << 4, ty << 4),
        ((fx - tx) << 4, (fy - ty) << 4),
        elliptic,
    );
    (r, rb)
}

/// Edit → Transformation → Rubber (`sub_14_0a68`), in place (whole image,
/// rectangle area, painting): the point `from` is dragged to `to`; the ROI
/// boundary stays fixed. Inverse mapping `src = dst + X(i)·Y(j)·(from − to)`
/// with the separable influence
/// `X(i) = i² / ((P−i)² + i²)` left of the reference column `P = to.x − x0`
/// and `(w−i)² / ((P−i)² + (w−i)²)` right of it (×4096, integer), likewise
/// `Y(j)`. `elliptic = true` selects the ellipse-area variant `sub_14_04bc`.
/// Coordinates are integer pixels (top-down); `bg` (DS:0x6e40) only matters
/// for neighbours outside the image.
pub fn rubber(
    src: &Image,
    roi: Rect,
    from: (i64, i64),
    to: (i64, i64),
    elliptic: bool,
    bg: Rgb,
) -> Image {
    let Some(roi) = clamp_rect(roi, src.w, src.h) else {
        return src.clone();
    };
    let (r, rb) = rubber_setup(src, roi, from, to, elliptic);
    let wa = to_wa(src);
    let out = if elliptic {
        warp_inplace(&wa, r, bg, &|x, y| rb.map_ellipse(x, y))
    } else {
        warp_inplace(&wa, r, bg, &|x, y| rb.map_rect(x, y))
    };
    from_wa(&out)
}

/// Rubber in area mode (polygon, freehand, magic wand, ellipse…): extracts a
/// fragment (`sub_15_0620`) to be placed with [`place_fragment`].
pub fn rubber_fragment(
    src: &Image,
    sel: &Mask,
    roi: Rect,
    from: (i64, i64),
    to: (i64, i64),
    elliptic: bool,
) -> Fragment {
    let (r, rb) = rubber_setup(src, roi, from, to, elliptic);
    let wa = to_wa(src);
    let (img, alpha) = if elliptic {
        extract_bilinear(&wa, &mask_to_wa(sel), r, &|x, y| rb.map_ellipse(x, y))
    } else {
        extract_bilinear(&wa, &mask_to_wa(sel), r, &|x, y| rb.map_rect(x, y))
    };
    wa_fragment(img, alpha, r, src.h)
}

// ---------------------------------------------------------------------------
// Deformations (ID 141)
// ---------------------------------------------------------------------------

/// "Type of mirror" combobox entries, in the original combobox order
/// (no CBS_SORT; index 0 is the default). String IDs in parentheses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MirrorKind {
    /// "Elliptic convex mirror" (182) — `sub_12_0486`. Default.
    EllipticConvex,
    /// "Elliptic concave mirror" (181) — `sub_12_02fe`.
    EllipticConcave,
    /// "Cylindric concave mirror, vertical" (183) — `sub_12_0646`.
    CylindricConcaveVertical,
    /// "Cylindric convexe mirror, vertical" (184) — `sub_12_06fc`.
    CylindricConvexVertical,
    /// "Cylindric concave mirror, horizontal" (191) — `sub_12_07ba`.
    CylindricConcaveHorizontal,
    /// "Cylindric convex mirror, horizontal" (192) — `sub_12_0870`.
    CylindricConvexHorizontal,
    /// "Horizontal wave" (193) — `sub_12_0b50`.
    HorizontalWave,
    /// "Vertical wave" (194) — `sub_12_0c2e`.
    VerticalWave,
    /// "Circular wave" (195) — `sub_12_0d1a`.
    CircularWave,
    /// "Whirlpool, clockwise" (198) — `sub_12_0f82`.
    WhirlpoolClockwise,
    /// "Whirlpool, counterclockwise" (199) — `sub_12_11bc`.
    WhirlpoolCounterclockwise,
}

impl MirrorKind {
    pub const ALL: [MirrorKind; 11] = [
        MirrorKind::EllipticConvex,
        MirrorKind::EllipticConcave,
        MirrorKind::CylindricConcaveVertical,
        MirrorKind::CylindricConvexVertical,
        MirrorKind::CylindricConcaveHorizontal,
        MirrorKind::CylindricConvexHorizontal,
        MirrorKind::HorizontalWave,
        MirrorKind::VerticalWave,
        MirrorKind::CircularWave,
        MirrorKind::WhirlpoolClockwise,
        MirrorKind::WhirlpoolCounterclockwise,
    ];

    /// Combobox text (strings.txt, original spelling).
    pub fn name(self) -> &'static str {
        match self {
            MirrorKind::EllipticConvex => "Elliptic convex mirror",
            MirrorKind::EllipticConcave => "Elliptic concave mirror",
            MirrorKind::CylindricConcaveVertical => "Cylindric concave mirror, vertical",
            MirrorKind::CylindricConvexVertical => "Cylindric convexe mirror, vertical",
            MirrorKind::CylindricConcaveHorizontal => "Cylindric concave mirror, horizontal",
            MirrorKind::CylindricConvexHorizontal => "Cylindric convex mirror, horizontal",
            MirrorKind::HorizontalWave => "Horizontal wave",
            MirrorKind::VerticalWave => "Vertical wave",
            MirrorKind::CircularWave => "Circular wave",
            MirrorKind::WhirlpoolClockwise => "Whirlpool, clockwise",
            MirrorKind::WhirlpoolCounterclockwise => "Whirlpool, counterclockwise",
        }
    }

    /// The "Size" scrollbar is enabled only for the three waves.
    pub fn uses_size(self) -> bool {
        matches!(
            self,
            MirrorKind::HorizontalWave | MirrorKind::VerticalWave | MirrorKind::CircularWave
        )
    }
}

/// Scrollbar ranges of the SETTING dialog: both 0..=50, default 0.
pub const DEFORM_PARAM_MAX: u32 = 50;

/// State set up by `sub_12_00da` / `sub_12_0926` for a rectangle, plus the
/// dialog parameters.
struct Deform {
    kind: MirrorKind,
    x0: i64,
    y0: i64,
    w: i64,
    h: i64,
    /// Centre ·16: x0·16 + w·8, y0·16 + h·8 (DS:0x306a / 0x306c).
    cx: i64,
    cy: i64,
    /// Half extents ·16: (w−1)·8, (h−1)·8 (DS:0x305e / 0x3060).
    a: i64,
    b: i64,
    /// R2 = 64·(w² + h²) (DS:0x305a).
    r2: i64,
    /// sqrt table T[k] = trunc(sqrt(R2·k/1024)), k = 0..=1024.
    sqrt_t: Vec<i64>,
    /// sin table S[k] = trunc(sin(k·3.1415/2048)·4096), k = 0..=1024.
    sin_t: Vec<i64>,
    /// K = 75·D/(p+2), D = trunc(sqrt(R2)) (DS:0x921a).
    k: i64,
    /// DS:0xa3a = distortion + 1, DS:0xa3c = size + 1.
    pa: i64,
    pc: i64,
    /// Apply the "stay inside the rectangle" checks that the original only
    /// performs for whole image / painting (area types 0 and 7).
    check: bool,
}

impl Deform {
    // The original's tables use π ≈ 3.1415, reproduced on purpose.
    #[allow(clippy::approx_constant)]
    fn new(kind: MirrorKind, r: WaRect, distortion: u32, size: u32, check: bool) -> Self {
        let (w, h) = (r.w, r.h);
        let r2 = (h * h + w * w) << 6;
        let d = ftol((r2 as f64).sqrt());
        let sqrt_t = (0..=1024i64)
            .map(|k| ftol(((r2 as f64) * k as f64 * 0.0009765625).sqrt()))
            .collect();
        let sin_t = (0..=1024i64)
            .map(|k| ftol((k as f64 * 3.1415 * 0.00048828125).sin() * 4096.0))
            .collect();
        let pa = distortion.min(DEFORM_PARAM_MAX) as i64 + 1;
        let pc = size.min(DEFORM_PARAM_MAX) as i64 + 1;
        Deform {
            kind,
            x0: r.x0,
            y0: r.y0,
            w,
            h,
            cx: (r.x0 << 4) + w * 8,
            cy: (r.y0 << 4) + h * 8,
            a: (w - 1) * 8,
            b: (h - 1) * 8,
            r2,
            sqrt_t,
            sin_t,
            k: (75 * d) / (pa + 1),
            pa,
            pc,
            check,
        }
    }

    /// Radius lookup `sub_12_0238`: idx = min(((a²+b²) & ~15)·64 / (R2>>4), 1023).
    fn radius(&self, a: i64, b: i64) -> i64 {
        let s = (a * a + b * b) as u64;
        let div = (self.r2 >> 4).max(1) as u64;
        let idx = (((s & !0xf) << 6) / div).min(0x3ff);
        self.sqrt_t[idx as usize]
    }

    /// `sub_12_09cc`: sine of a 16-bit phase where 25736 = 2π, result ·4096.
    fn sin16(&self, ph: i64) -> i64 {
        let q = ph / 0x6488;
        let r = ph % 0x6488;
        let idx = |v: i64| ((v << 11) / 0x3244) as usize;
        if (0..0x1922).contains(&r) {
            self.sin_t[idx(r)]
        } else if (0x1922..0x3244).contains(&r) {
            self.sin_t[idx(0x3244 - r)]
        } else if (0x3244..0x4b66).contains(&r) {
            -self.sin_t[idx(r - 0x3244)]
        } else if (0x4b66..0x6488).contains(&r) {
            -self.sin_t[idx(0x6488 - r)]
        } else {
            q // negative remainder: the original returns the quotient left in AX
        }
    }

    /// `sub_12_0a78`: cos = sin16(ph + π/2) with 16-bit wrap of the addition.
    fn cos16(&self, ph: i64) -> i64 {
        self.sin16(((ph + 0x1922) as i16) as i64)
    }

    /// Back from the circle space to the ellipse: `(s' − cx)·B/A + cy`.
    fn unsquash(&self, sy: i64) -> i64 {
        (sy - self.cx) * self.b / self.a.max(1) + self.cy
    }

    fn in_x(&self, v: i64) -> bool {
        v >= self.x0 << 4 && v < (self.x0 + self.w) << 4
    }

    fn map(&self, x: i64, y: i64) -> (i64, i64) {
        let (a, b, k, cx, cy) = (self.a, self.b, self.k, self.cx, self.cy);
        match self.kind {
            MirrorKind::EllipticConcave | MirrorKind::EllipticConvex => {
                let v = (y - cy) * a / b.max(1);
                let u = x - cx;
                let r = self.radius(u, v);
                if r > a {
                    return (x, y);
                }
                let (sx, sy) = if self.kind == MirrorKind::EllipticConcave {
                    // sub_12_02fe: m(r) = (K − r)/(K − A)
                    (u * (k - r) / (k - a) + cx, v * (k - r) / (k - a) + cx)
                } else {
                    // sub_12_0486: m(r) = (K/2 + r)/(K/2 + A), u/2 truncated first
                    let den = k / 2 + a;
                    (
                        ((u / 2) * k + r * u) / den + cx,
                        ((v / 2) * k + r * v) / den + cx,
                    )
                };
                let check = self.kind == MirrorKind::EllipticConvex || self.check;
                if check && !(self.in_x(sx) && self.in_x(sy)) {
                    return (x, y);
                }
                (sx, self.unsquash(sy))
            }
            MirrorKind::CylindricConcaveVertical => {
                let u = x - cx;
                let sx = u * (k - u.abs()) / (k - a) + cx;
                if self.check && !(sx > self.x0 << 4 && sx < (self.x0 + self.w) << 4) {
                    return (x, y);
                }
                (sx, y)
            }
            MirrorKind::CylindricConvexVertical => {
                let u = x - cx;
                ((u.abs() * u + (u / 2) * k) / (k / 2 + a) + cx, y)
            }
            MirrorKind::CylindricConcaveHorizontal => {
                let v = y - cy;
                let sy = v * (k - v.abs()) / (k - b) + cy;
                if !(sy > self.y0 << 4 && sy < (self.y0 + self.h) << 4) {
                    return (x, y);
                }
                (x, sy)
            }
            MirrorKind::CylindricConvexHorizontal => {
                let v = y - cy;
                (x, (v.abs() * v + (v / 2) * k) / (k / 2 + b) + cy)
            }
            MirrorKind::HorizontalWave | MirrorKind::VerticalWave => {
                let horiz = self.kind == MirrorKind::HorizontalWave;
                let (n, o, p) = if horiz {
                    (self.w, self.x0, x)
                } else {
                    (self.h, self.y0, y)
                };
                // amplitude (1/16 px) = ((n·(dist+1)/(size+1))/50)·8
                let amp = (((n * self.pa) / self.pc) / 50) << 3;
                let t = p - (o << 4);
                let phase = (((t >> 4) * 0x6488) / (n - 1).max(1) * self.pc) % 0x6488;
                let s = self.sin16(phase as i16 as i64);
                // Fixed: the original's vertical wave added x0·16 instead of
                // y0·16, shifting the wave for areas not at the image's top.
                let sp = t + (s * amp) / 4096 + (o << 4);
                let c = sp >> 4;
                if c >= o + n || c < o {
                    return (x, y);
                }
                if horiz { (sp, y) } else { (x, sp) }
            }
            MirrorKind::CircularWave => {
                let v = (y - cy) * a / b.max(1);
                let u = x - cx;
                let r = self.radius(u, v);
                if r == 0 || r > a {
                    return (x, y);
                }
                let amp = (((a * self.pa) / self.pc) / 50) >> 1;
                let amp16 = amp as i16 as i64;
                let phase = ((((r >> 4) * 0x6488) / (self.w - 1).max(1)) * self.pc) % 0x6488;
                let s = self.sin16(phase as i16 as i64);
                let d = s * amp16 / 4096;
                let sx = d * u / r + cx + u;
                let sy = d * v / r + cx + v;
                if !(self.in_x(sx) && self.in_x(sy)) {
                    return (x, y);
                }
                (sx, self.unsquash(sy))
            }
            MirrorKind::WhirlpoolClockwise | MirrorKind::WhirlpoolCounterclockwise => {
                let v = (y - cy) * a / b.max(1);
                let u = x - cx;
                let r = self.radius(u, v);
                if r == 0 || r > a {
                    return (x, y);
                }
                // twist = 257·(A−r)²/A²·(dist+1) in 25736-per-turn units
                let q = (((r - a) * 257) / a * (r - a)) / a;
                let ang = ((q as i16 as i64) * self.pa) as i16 as i64;
                let (c, s) = (self.cos16(ang), self.sin16(ang));
                let (sx, sy) = if self.kind == MirrorKind::WhirlpoolClockwise {
                    (
                        c * u / 4096 - s * v / 4096 + cx,
                        c * v / 4096 + s * u / 4096 + cx,
                    )
                } else {
                    (
                        c * u / 4096 + s * v / 4096 + cx,
                        c * v / 4096 - s * u / 4096 + cx,
                    )
                };
                if !(self.in_x(sx) && self.in_x(sy)) {
                    return (x, y);
                }
                (sx, self.unsquash(sy))
            }
        }
    }
}

/// Edit → Transformation → Deformations (`sub_37_0dcc`), in place over the
/// ROI (whole image / painting path, `sub_37_0c8c` → `sub_15_0000`, black
/// background). `distortion` and `size` are the SETTING dialog scrollbar
/// positions, 0..=50, default 0 ("Distortion (percents)" and "Size"; the
/// "256"/"145" texts in the resource are placeholders overwritten with "0").
///
/// Distortion enters as `K = 75·D/(distortion+2)` for the mirrors and
/// whirlpools (D = half-diagonal of the ROI in 1/16 px) or as amplitude
/// `(distortion+1)` % of the wavelength for the waves; Size gives the number
/// of wave periods `size+1` across the ROI. ROIs narrower than 2 px are
/// returned unchanged (the original divides by w−1 / h−1).
pub fn deform(src: &Image, roi: Rect, kind: MirrorKind, distortion: u32, size: u32) -> Image {
    let Some(roi) = clamp_rect(roi, src.w, src.h) else {
        return src.clone();
    };
    if roi.w < 2 || roi.h < 2 {
        return src.clone();
    }
    let r = rect_to_wa(roi, src.h);
    let d = Deform::new(kind, r, distortion, size, true);
    let wa = to_wa(src);
    from_wa(&warp_inplace(&wa, r, [0; 3], &|x, y| d.map(x, y)))
}

/// Deformations in area mode: fragment extraction (`sub_15_0620`) over the
/// ROI; place it with [`place_fragment`].
pub fn deform_fragment(
    src: &Image,
    sel: &Mask,
    roi: Rect,
    kind: MirrorKind,
    distortion: u32,
    size: u32,
) -> Fragment {
    let r = rect_to_wa(roi, src.h);
    if roi.w < 2 || roi.h < 2 {
        let (img, alpha) = extract_nearest(&to_wa(src), &mask_to_wa(sel), r, &|x, y| (x, y));
        return wa_fragment(img, alpha, r, src.h);
    }
    let d = Deform::new(kind, r, distortion, size, false);
    let wa = to_wa(src);
    let (img, alpha) = extract_bilinear(&wa, &mask_to_wa(sel), r, &|x, y| d.map(x, y));
    wa_fragment(img, alpha, r, src.h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(w: usize, h: usize) -> Image {
        let mut img = Image::new(w, h, [0; 3]);
        for y in 0..h {
            for x in 0..w {
                img.set(
                    x,
                    y,
                    [
                        (x * 7 % 256) as u8,
                        (y * 11 % 256) as u8,
                        ((x * 3 + y * 5) % 256) as u8,
                    ],
                );
            }
        }
        img
    }

    #[test]
    fn resize_identity_and_upscale() {
        let img = ramp(13, 9);
        assert_eq!(resize(&img, 13, 9), img);
        let up = resize(&img, 26, 18);
        assert_eq!((up.w, up.h), (26, 18));
        // Even columns of a 2× upscale sample source columns exactly (fx = 0).
        assert_eq!(up.get(4, 17)[0], img.get(2, 8)[0]);
    }

    #[test]
    fn clip_clamps() {
        let img = ramp(10, 10);
        let c = clip(
            &img,
            Rect {
                x: 8,
                y: 7,
                w: 5,
                h: 5,
            },
        )
        .unwrap();
        assert_eq!((c.w, c.h), (2, 3));
        assert_eq!(c.get(0, 0), img.get(8, 7));
        assert!(
            clip(
                &img,
                Rect {
                    x: 20,
                    y: 0,
                    w: 2,
                    h: 2
                }
            )
            .is_none()
        );
    }

    #[test]
    fn flips_are_exact_involutions() {
        let img = ramp(11, 7);
        let roi = Rect {
            x: 2,
            y: 1,
            w: 6,
            h: 5,
        };
        let f = flip_horizontal(&img, roi);
        assert_eq!(f.get(2, 1), img.get(7, 1));
        assert_eq!(f.get(0, 0), img.get(0, 0));
        assert_eq!(flip_horizontal(&f, roi), img);
        let v = flip_vertical(&img, roi);
        assert_eq!(v.get(3, 1), img.get(3, 5));
        assert_eq!(flip_vertical(&v, roi), img);
    }

    #[test]
    fn rotate_zero_is_identity() {
        let img = ramp(9, 6);
        assert_eq!(rotate(&img, 0.0, [1, 2, 3]).unwrap(), img);
    }

    #[test]
    fn rotate_90_is_counterclockwise_and_exact() {
        let img = ramp(5, 3);
        let r = rotate(&img, 90.0, [0; 3]).unwrap();
        assert_eq!((r.w, r.h), (3, 5));
        // CCW: dst(x, y) = src(W-1-y, x)
        for y in 0..5 {
            for x in 0..3 {
                assert_eq!(r.get(x, y), img.get(4 - y, x), "at {x},{y}");
            }
        }
    }

    #[test]
    fn rotate_size_formula_and_bg() {
        let img = Image::new(40, 20, [200, 200, 200]);
        let r = rotate(&img, 30.0, [0, 0, 255]).unwrap();
        let (c, s) = rot_coeffs(30.0);
        assert_eq!(r.w as i64, (c * 40 + s * 20) / 4096);
        assert_eq!(r.h as i64, (c * 20 + s * 40) / 4096);
        assert_eq!(r.get(0, 0), [0, 0, 255]);
        assert_eq!(r.get(r.w / 2, r.h / 2), [200, 200, 200]);
        // 150° and -30° rotate to the same bounding box as 30°.
        for a in [150.0, -30.0, 210.0] {
            let r2 = rotate(&img, a, [0; 3]).unwrap();
            assert_eq!((r2.w, r2.h), (r.w, r.h), "{a}");
        }
        assert!(rotate(&img, f64::NAN, [0; 3]).is_none());
    }

    #[test]
    fn rotate_fragment_is_centred() {
        let img = ramp(30, 30);
        let sel = Mask::full(30, 30);
        let f = rotate_fragment(
            &img,
            &sel,
            Rect {
                x: 10,
                y: 10,
                w: 10,
                h: 6,
            },
            90.0,
        );
        assert_eq!((f.img.w, f.img.h), (6, 10));
        assert_eq!((f.x, f.y), (12, 8));
    }

    #[test]
    fn rubber_no_drag_is_identity_and_boundary_fixed() {
        let img = ramp(20, 16);
        let roi = Rect {
            x: 2,
            y: 3,
            w: 14,
            h: 10,
        };
        assert_eq!(rubber(&img, roi, (8, 8), (8, 8), false, [0; 3]), img);
        let out = rubber(&img, roi, (6, 7), (10, 9), false, [0; 3]);
        // Outside the ROI and on its left/top edge (influence 0) nothing moves.
        assert_eq!(out.get(0, 0), img.get(0, 0));
        assert_eq!(out.get(2, 5), img.get(2, 5));
        // The dragged-to point now shows the dragged-from pixel.
        assert_eq!(out.get(10, 9), img.get(6, 7));
    }

    #[test]
    fn rubber_influence_table() {
        let r = WaRect {
            x0: 0,
            y0: 0,
            w: 10,
            h: 10,
        };
        let rb = Rubber::new(r, (4 << 4, 4 << 4), (0, 0), false);
        assert_eq!(rb.xt[0], 0);
        assert_eq!(rb.xt[4], 4096);
        assert_eq!(rb.xt[2], (4 << 12) / (4 + 4));
        assert_eq!(rb.xt[7], (9 << 12) / (9 + 9));
    }

    #[test]
    fn sine_table_and_lookup() {
        let d = Deform::new(
            MirrorKind::HorizontalWave,
            WaRect {
                x0: 0,
                y0: 0,
                w: 16,
                h: 16,
            },
            0,
            0,
            true,
        );
        assert_eq!(d.sin16(0), 0);
        assert_eq!(d.sin16(0x1922), 4095); // sin(1024·3.1415/2048)·4096 truncated
        assert_eq!(d.sin16(0x3244 + 0x1922), -4095);
        assert_eq!(d.cos16(0), 4095);
        assert_eq!(d.sin16(-5), 0);
    }

    #[test]
    fn deform_keeps_outside_and_centre() {
        let img = ramp(32, 24);
        let roi = Rect {
            x: 4,
            y: 2,
            w: 20,
            h: 18,
        };
        for kind in MirrorKind::ALL {
            let out = deform(&img, roi, kind, 30, 3);
            assert_eq!(out.get(0, 0), img.get(0, 0), "{kind:?}");
            assert_eq!(out.get(31, 23), img.get(31, 23), "{kind:?}");
        }
        // Mirrors with a radial law leave the ROI corner pixels (r > A) alone.
        let out = deform(&img, roi, MirrorKind::EllipticConvex, 50, 0);
        assert_eq!(out.get(4, 2), img.get(4, 2));
        assert_ne!(out, img);
    }

    #[test]
    fn deform_parameters() {
        let r = WaRect {
            x0: 0,
            y0: 0,
            w: 100,
            h: 100,
        };
        let d = Deform::new(MirrorKind::EllipticConvex, r, 0, 0, true);
        let dd = ftol(((100i64 * 100 + 100 * 100) as f64 * 64.0).sqrt());
        assert_eq!(d.k, 75 * dd / 2);
        assert_eq!((d.a, d.cx), (99 * 8, 100 * 8));
        assert_eq!(d.radius(0, 0), 0);
        let rr = d.radius(400, 300);
        assert!((rr - 500).abs() <= 8, "{rr}");
    }

    #[test]
    fn place_fragment_same_rect_is_copy() {
        let img = ramp(12, 12);
        let mut sel = Mask::empty(12, 12);
        for y in 3..8 {
            for x in 2..9 {
                sel.set(x, y, 255);
            }
        }
        let frag = move_fragment(
            &img,
            &sel,
            Rect {
                x: 2,
                y: 3,
                w: 7,
                h: 5,
            },
        );
        let dst = Image::new(12, 12, [9, 9, 9]);
        let (out, m) = place_fragment(&dst, &frag, 4, 5, 7, 5);
        assert_eq!(out.get(4, 5), img.get(2, 3));
        assert_eq!(out.get(10, 9), img.get(8, 7));
        assert_eq!(out.get(3, 5), [9, 9, 9]);
        assert_eq!(m.get(4, 5), 255);
        assert_eq!(m.get(11, 11), 0);
    }

    #[test]
    fn clone_offset_shifts() {
        let img = ramp(10, 10);
        let out = clone_offset(
            &img,
            Rect {
                x: 5,
                y: 5,
                w: 3,
                h: 3,
            },
            2,
            1,
            [0; 3],
        );
        assert_eq!(out.get(5, 5), img.get(3, 4));
    }
}
