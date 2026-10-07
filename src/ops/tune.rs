//! Tune menu: RGB control (TV / Linear), Gamma correction, Expand,
//! Equalization — recovered from PMAN.EXE 1.55.
//!
//! Pixel order in the original work areas is BGR (DIB); everything here is
//! expressed in RGB.  All operations are pure per-pixel lookups except the
//! statistics passes of Expand / Equalization, so row order (the original
//! stores rows bottom-up) does not matter here.
//!
//! See `re/specs/tune_fill.md` for addresses, pseudocode and confidence.

use crate::core::{Image, Mask, Rect, Rgb};

/// Which kind of selection is active (PMAN global DS:0x519a).  Expand and
/// Equalization gather their statistics differently depending on it.
#[derive(Clone, Copy, Debug)]
pub enum AreaKind<'a> {
    /// Area/Whole image (0x519a == 0): every pixel of the ROI counts.
    Whole,
    /// Area/Pen (0x519a == 7): every pixel of the ROI counts, but Expand
    /// uses the max(R,G,B) statistic like a selection.
    Pen,
    /// Any other selection (rect, ellipse, polygon, text, wand, freehand):
    /// only pixels with `mask != 0` count.
    Mask(&'a Mask),
}

fn apply_lut3(src: &Image, roi: Rect, lut: &[[u8; 256]; 3]) -> Image {
    let mut out = src.clone();
    for y in roi.y..(roi.y + roi.h).min(src.h) {
        for x in roi.x..(roi.x + roi.w).min(src.w) {
            let p = src.get(x, y);
            out.set(
                x,
                y,
                [
                    lut[0][p[0] as usize],
                    lut[1][p[1] as usize],
                    lut[2][p[2] as usize],
                ],
            );
        }
    }
    out
}

fn identity_lut() -> [u8; 256] {
    let mut t = [0u8; 256];
    for (i, v) in t.iter_mut().enumerate() {
        *v = i as u8;
    }
    t
}

// ---------------------------------------------------------------------------
// RGB control (TV = COMMON dialog, Linear = RGB dialog)
// ---------------------------------------------------------------------------

/// One linear transfer function `out = clamp(i*k/50 + offset, 0, 255)`
/// (`k/50` is the slope; C integer division truncating toward zero).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Linear {
    /// Slope in 1/50 units (50 = 1.0).  Dialogs show `k*0.02` as "%3.1f".
    pub k: i32,
    /// Additive offset, shown as "%5d".
    pub offset: i32,
}

impl Default for Linear {
    fn default() -> Self {
        Linear { k: 50, offset: 0 }
    }
}

impl Linear {
    /// `seg59:0000` table builder.
    pub fn table(&self) -> [u8; 256] {
        let mut t = [0u8; 256];
        for (i, v) in t.iter_mut().enumerate() {
            let x = (i as i32 * self.k) / 50 + self.offset;
            *v = x.clamp(0, 255) as u8;
        }
        t
    }

    /// From the value a `transfn` control reports (WM_COMMAND lParam): slope
    /// in percent and the y-intercept.  The dialog halves the slope with C
    /// truncation (`seg59:0745`).
    pub fn from_transfn(slope_percent: i32, offset: i32) -> Self {
        Linear {
            k: slope_percent / 2,
            offset,
        }
    }
}

/// The five curves of "Tune/RGB control".  `halftone` acts on HSV value
/// (DS:0x81f6/0x6e46), `color` on HSV saturation (DS:0x7162/0x61dc), then
/// `red`/`green`/`blue` on the resulting channels (DS:0xa3e2/0x61d2,
/// 0x70a2/0x5198, 0x7174/0x7220).  Default = identity (k=50, offset=0),
/// which is also what both dialogs reset to every time they open.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ColorMap {
    pub halftone: Linear,
    pub color: Linear,
    pub red: Linear,
    pub green: Linear,
    pub blue: Linear,
}

impl ColorMap {
    /// Map the three vertical scrollbars of the COMMON ("Color/contrast",
    /// RGB/TV) dialog (`COMMONDLGPROC`, seg58:0000):
    /// * `contrast_pos` (id 712, range 0..=100, default 50): halftone k = 100 - pos
    /// * `brightness_pos` (id 710, range -255..=255, default 0): halftone offset = -pos
    /// * `color_pos` (id 711 under the "RGB" icon, -255..=255, default 0):
    ///   saturation offset = -pos (saturation slope stays 1.0)
    ///
    /// The scrollbars are vertical, so dragging up (smaller pos) increases.
    pub fn from_tv(contrast_pos: i32, brightness_pos: i32, color_pos: i32) -> Self {
        ColorMap {
            halftone: Linear {
                k: 100 - contrast_pos,
                offset: -brightness_pos,
            },
            color: Linear {
                k: 50,
                offset: -color_pos,
            },
            ..Default::default()
        }
    }
}

struct MapTables {
    v: [u8; 256],
    s: [u8; 256],
    r: [u8; 256],
    g: [u8; 256],
    b: [u8; 256],
}

/// Transfer table evaluated between its integer entries, so an identity
/// table is an exact identity.
fn lut_f(t: &[u8; 256], x: f64) -> f64 {
    let x = x.clamp(0.0, 255.0);
    let i = x.floor() as usize;
    if i >= 255 {
        return t[255] as f64;
    }
    let f = x - i as f64;
    t[i] as f64 * (1.0 - f) + t[i + 1] as f64 * f
}

/// One pixel through the colour map (`seg59:040c`): RGB → HSV, V through the
/// halftone table, S through the color table, back to RGB, then the
/// per-channel tables.
fn map_pixel(p: Rgb, t: &MapTables) -> Rgb {
    let (r, g, b) = (p[0] as f64, p[1] as f64, p[2] as f64);
    let mx = r.max(g).max(b);
    let mn = r.min(g).min(b);
    let d = mx - mn;
    let s = if mx > 0.0 { d * 255.0 / mx } else { 0.0 };
    // Hue in sextants, 0..6.
    let h = if d == 0.0 {
        0.0
    } else if mx == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if mx == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    let v2 = lut_f(&t.v, mx);
    // Greys have no hue: they stay grey whatever the color table does.
    let s2 = if d == 0.0 {
        0.0
    } else {
        lut_f(&t.s, s) / 255.0
    };
    let c = v2 * s2;
    let x = c * (1.0 - ((h % 2.0) - 1.0).abs());
    let m = v2 - c;
    let (r1, g1, b1) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let q = |v: f64| (v + m).round().clamp(0.0, 255.0) as usize;
    [t.r[q(r1)], t.g[q(g1)], t.b[q(b1)]]
}

/// Tune/RGB control/TV (cmd 104, `sub_58_0258`) and Tune/RGB control/Linear
/// (cmd 105, `sub_59_0b8c`).  Both commit through the same per-pixel routine
/// `sub_59_040c` (callback `seg49:0000`):
///
/// 1. convert to HSV (S = (max-min)*255/max, V = max);
/// 2. V <- halftone table, S <- color table;
/// 3. back to RGB;
/// 4. R/G/B <- red/green/blue tables.
///
/// Fixed: the original worked on 6-bit channels (`>>2`) with integer hue, so
/// even the identity map lost the low 2 bits of every colour; and greys
/// (hue 0 = red) turned red when the color table raised saturation.
pub fn rgb_control(src: &Image, roi: Rect, map: &ColorMap) -> Image {
    let t = MapTables {
        v: map.halftone.table(),
        s: map.color.table(),
        r: map.red.table(),
        g: map.green.table(),
        b: map.blue.table(),
    };
    let mut out = src.clone();
    for y in roi.y..(roi.y + roi.h).min(src.h) {
        for x in roi.x..(roi.x + roi.w).min(src.w) {
            out.set(x, y, map_pixel(src.get(x, y), &t));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Gamma correction
// ---------------------------------------------------------------------------

/// Gamma from the GAMMA dialog scrollbar position (id 1501, 0..=255;
/// PROCGAMMADLGPROC seg33:0939): `v = pos*0.023529412f` (6/255 as f32);
/// `v > 3 ? v-2 : 1/(4-v)` -> 0.25..4.0.  The dialog opens at gamma 1.0
/// exactly (thumb at 127, label "1.0"); moving the thumb snaps to this curve.
/// Line step 4, page step 15.
pub fn gamma_from_slider(pos: i32) -> f64 {
    let c = 0.023_529_412_f32;
    let ext = pos as f64 * c as f64; // compared in extended precision
    let v32 = ext as f32;
    if ext > 3.0 {
        v32 as f64 - 2.0
    } else {
        1.0 / (4.0 - v32 as f64)
    }
}

/// Initial thumb position for a gamma of 1.0: `trunc(42.5*(4-1/1.0))` = 127.
pub const GAMMA_SLIDER_DEFAULT: i32 = 127;

/// `sub_33_0a76` LUT: `trunc(pow(i*(1/255), g)*255 + 0.49)`, 255 if > 255.
pub fn gamma_lut(g: f64) -> [u8; 256] {
    let mut t = [0u8; 256];
    for (i, v) in t.iter_mut().enumerate() {
        let x = i as f64 * 0.00392156862745098;
        let y = (x.powf(g) * 255.0 + 0.49) as i32;
        *v = if y > 255 { 255 } else { y as u8 };
    }
    t
}

/// Tune/Gamma correction (cmd 143, `sub_33_0a76`, apply callback
/// `sub_33_0000`).  The LUT is applied to the checked channels only (R/G/B
/// check boxes 603/604/605, globals DS:0xfac/0xfae/0xfb0, default all on,
/// remembered between invocations).  Hidden commands 251/252 (`sub_33_0bd0`)
/// apply gamma 0.7 / 1.4 to all channels and force all three boxes on.
pub fn gamma(src: &Image, roi: Rect, g: f64, red: bool, green: bool, blue: bool) -> Image {
    let lut = gamma_lut(g);
    let id = identity_lut();
    apply_lut3(
        src,
        roi,
        &[
            if red { lut } else { id },
            if green { lut } else { id },
            if blue { lut } else { id },
        ],
    )
}

// ---------------------------------------------------------------------------
// Expand / Equalization
// ---------------------------------------------------------------------------

/// Luminance `(2R + 4G + B + 3) / 7` (seg28:034b / seg34:022c).
///
/// Fixed: the original computed `(4G + 2B + R + 3) / 7`, weighting blue
/// above red — red and blue swapped by the BGR pixel order.
#[inline]
pub fn pman_luma(p: Rgb) -> u32 {
    let (r, g, b) = (p[0] as u32, p[1] as u32, p[2] as u32);
    (2 * r + 4 * g + b + 3) / 7
}

fn roi_pixels<'a>(src: &'a Image, roi: Rect, area: AreaKind<'a>) -> impl Iterator<Item = Rgb> + 'a {
    let (x0, x1) = (roi.x, (roi.x + roi.w).min(src.w));
    let (y0, y1) = (roi.y, (roi.y + roi.h).min(src.h));
    (y0..y1)
        .flat_map(move |y| (x0..x1).map(move |x| (x, y)))
        .filter_map(move |(x, y)| match area {
            AreaKind::Mask(m) if m.get(x, y) == 0 => None,
            _ => Some(src.get(x, y)),
        })
}

/// Tune/Expand (cmd 154, `sub_28_07ea` -> `sub_28_0072`): linear contrast
/// stretch.  With min/max of luma ([`pman_luma`]) over the area,
/// `lut[i] = clamp((i-min)*255/(max-min))`
/// (C division, truncation toward zero), identity if max == min; the same
/// LUT is applied to R, G and B.  No dialog.
///
/// Fixed: the original used max(R,G,B) instead of luma for selections and
/// the pen, so the same pixels stretched differently depending on the area.
pub fn expand(src: &Image, roi: Rect, area: AreaKind) -> Image {
    let mut mx = 0i32;
    let mut mn = 255i32;
    for p in roi_pixels(src, roi, area) {
        let v = pman_luma(p).min(255) as i32;
        mx = mx.max(v);
        mn = mn.min(v);
    }
    let lut = expand_lut(mn, mx);
    apply_lut3(src, roi, &[lut, lut, lut])
}

/// The Expand LUT for a given statistic range.
pub fn expand_lut(mn: i32, mx: i32) -> [u8; 256] {
    if mx == mn {
        return identity_lut();
    }
    let mut t = [0u8; 256];
    for (i, v) in t.iter_mut().enumerate() {
        *v = ((i as i32 - mn) * 255 / (mx - mn)).clamp(0, 255) as u8;
    }
    t
}

/// Tune/Equalization (cmd 144, `sub_34_073a` -> `sub_34_0000`): histogram
/// of luma ([`pman_luma`]) over the area (Whole/Pen: all ROI pixels,
/// selection: mask != 0); `lut[0] = 0`,
/// `lut[i] = min(255, (sum(hist[0..i]) << 8) / N)` (exclusive cumulative
/// sum, N = pixel count or 1); the LUT is applied to R, G and B separately.
/// No dialog.
pub fn equalize(src: &Image, roi: Rect, area: AreaKind) -> Image {
    let mut hist = [0u32; 256];
    for p in roi_pixels(src, roi, area) {
        hist[pman_luma(p).min(255) as usize] += 1;
    }
    let lut = equalize_lut(&hist);
    apply_lut3(src, roi, &[lut, lut, lut])
}

/// The Equalization LUT from a luma histogram.
pub fn equalize_lut(hist: &[u32; 256]) -> [u8; 256] {
    let mut n: u64 = hist.iter().map(|&v| v as u64).sum();
    if n == 0 {
        n = 1;
    }
    let mut t = [0u8; 256];
    let mut cum: u64 = 0;
    for i in 1..256 {
        cum += hist[i - 1] as u64;
        let v = ((cum << 8) & 0xffff_ffff) / n;
        t[i] = v.min(255) as u8;
    }
    // monotonic fix-up present in the original (no-op for this LUT)
    for i in 1..256 {
        if t[i - 1] > t[i] {
            t[i] = t[i - 1];
        }
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(px: &[Rgb], w: usize) -> Image {
        Image {
            w,
            h: px.len() / w,
            px: px.to_vec(),
        }
    }

    #[test]
    fn identity_color_map_is_exact() {
        let mut px = Vec::new();
        for i in 0..4096u32 {
            px.push([
                (i * 37 % 256) as u8,
                (i * 101 % 256) as u8,
                (i * 7 % 256) as u8,
            ]);
        }
        let src = img(&px, 64);
        assert_eq!(rgb_control(&src, src.rect(), &ColorMap::default()), src);
    }

    #[test]
    fn saturation_keeps_greys_and_hues() {
        let m = ColorMap {
            color: Linear { k: 100, offset: 0 },
            ..Default::default()
        }; // ×2
        let src = img(&[[128, 128, 128], [200, 150, 150]], 2);
        let out = rgb_control(&src, src.rect(), &m);
        assert_eq!(out.px[0], [128, 128, 128]); // grey stays grey
        assert_eq!(out.px[1], [200, 100, 100]); // same hue and value, s 0.25 -> 0.5
    }

    #[test]
    fn tv_brightness_and_desaturate() {
        let m = ColorMap::from_tv(50, -20, 255); // brighter, saturation -255
        let src = img(&[[200, 40, 40]], 1);
        let out = rgb_control(&src, src.rect(), &m);
        // grey with V = 200 + 20
        assert_eq!(out.px[0], [220, 220, 220]);
    }

    #[test]
    fn linear_table() {
        let l = Linear::from_transfn(200, -10); // slope 2.0
        let t = l.table();
        assert_eq!(t[0], 0);
        assert_eq!(t[10], 10);
        assert_eq!(t[200], 255);
    }

    #[test]
    fn gamma_slider_and_lut() {
        assert!((gamma_from_slider(0) - 0.25).abs() < 1e-9);
        assert!((gamma_from_slider(255) - 4.0).abs() < 1e-5);
        assert!((gamma_from_slider(128) - 1.0117647).abs() < 1e-5);
        let id = gamma_lut(1.0);
        for i in 0..256 {
            assert_eq!(id[i] as usize, i);
        }
        let g = gamma_lut(0.5);
        assert_eq!(g[64], 128); // sqrt(64/255)*255+0.49 = 127.75+0.49
        let src = img(&[[64, 64, 64]], 1);
        let out = gamma(&src, src.rect(), 0.5, true, false, true);
        assert_eq!(out.px[0], [128, 64, 128]);
    }

    #[test]
    fn expand_stretches() {
        let src = img(&[[50, 50, 50], [150, 150, 150]], 2);
        let out = expand(&src, src.rect(), AreaKind::Whole);
        assert_eq!(out.px, vec![[0, 0, 0], [255, 255, 255]]);
        // Selections use luma too: 60 and 29 stretch to 255 and 0.
        let src = img(&[[10, 100, 0], [0, 0, 200]], 2);
        let m = Mask::full(2, 1);
        let out = expand(&src, src.rect(), AreaKind::Mask(&m));
        assert_eq!(out.px[0], [0, 255, 0]);
        assert_eq!(out.px[1], [0, 0, 255]);
    }

    #[test]
    fn equalize_two_levels() {
        let src = img(
            &[[10, 10, 10], [10, 10, 10], [200, 200, 200], [200, 200, 200]],
            4,
        );
        let out = equalize(&src, src.rect(), AreaKind::Whole);
        // lut[10] = 0 (nothing below), lut[200] = (2<<8)/4 = 128
        assert_eq!(out.px[0], [0, 0, 0]);
        assert_eq!(out.px[2], [128, 128, 128]);
    }

    #[test]
    fn roi_untouched_outside() {
        let src = img(&[[64, 64, 64], [64, 64, 64]], 2);
        let out = gamma(
            &src,
            Rect {
                x: 1,
                y: 0,
                w: 1,
                h: 1,
            },
            0.5,
            true,
            true,
            true,
        );
        assert_eq!(out.px[0], [64, 64, 64]);
        assert_eq!(out.px[1], [128, 128, 128]);
    }
}
