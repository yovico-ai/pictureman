//! Selection areas, soft ("smooth") edges, the magic wand and painting with
//! any operation through the pen — recovered from PMAN.EXE 1.55.
//!
//! See `re/specs/selection_painting.md` for the addresses, pseudocode and
//! the UI interaction model. Summary of the original's design:
//!
//! * Every area is first rasterised as a **binary** 1-bpp mask (GDI drawing
//!   into a DIB.DRV memory DC: `Rectangle`, `Ellipse`, `Polygon`, brush dabs,
//!   `FloodFill`). The shape is drawn white on black when the user
//!   double-clicks *inside* the shape (DS:0x12da = 1) and black on white when
//!   the exterior is chosen, so "selected" = white bit.
//! * Smooth edges are **not** a blur: each processed row is committed by
//!   `sub_57_3a32` with a per-pixel weight `e = (T[h] / 4) * T[v] >> 8`
//!   (0..=64), where `h`/`v` are the distances to the end of the horizontal
//!   and vertical *run* of the shape that contains the pixel, normalised by a
//!   feather width that depends on the area type and edge level, and `T` is a
//!   quarter-sine table (`sub_3_1e2e`). Result
//!   `out = (proc*e + orig*(64-e)) >> 6` (inside) — the ramp always lies
//!   *within* the drawn shape.
//! * The magic wand (`sub_9_0474`) classifies every pixel against the seed
//!   colour (RGB box or HSV), then optionally ("Unifold") keeps only the
//!   4-connected component of the seed using GDI `FloodFill`.
//! * The pen (area 7) re-sends the last WM_COMMAND for every mouse-move, so
//!   the operation is recomputed for each dab over the dab's bounding square,
//!   reading from the pre-painting backup (filters) or in place (fills); the
//!   edge level becomes a pen opacity (32/16/8 of 64).
//!
//! Coordinates are image coordinates, y down, (0,0) top-left. The original
//! stores rows bottom-up (DIB order); where that could make a ±1 pixel
//! difference it is noted.

use crate::core::{Image, Mask, MsRand, Rect, Rgb};

// ---------------------------------------------------------------------------
// Edge mode
// ---------------------------------------------------------------------------

/// Options/Edge (menu IDs 149..152, INI `[MODE] EDGE=0..3`, DS:0x6e34).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Edge {
    #[default]
    Sharp = 0,
    SmoothLow = 1,
    SmoothMedium = 2,
    SmoothHigh = 3,
}

impl Edge {
    pub fn from_level(level: i32) -> Edge {
        match level {
            1 => Edge::SmoothLow,
            2 => Edge::SmoothMedium,
            3 => Edge::SmoothHigh,
            _ => Edge::Sharp,
        }
    }
    /// Menu command ID → edge (149 Sharp, 150 low, 151 medium, 152 high).
    pub fn from_command(id: u16) -> Option<Edge> {
        match id {
            149..=152 => Some(Edge::from_level(id as i32 - 149)),
            _ => None,
        }
    }
    pub fn level(self) -> i32 {
        self as i32
    }
}

/// Area kinds (DS:0x519a "FRAGMENT"; dispatcher `sub_57_37be` switch at
/// seg57:3909).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AreaKind {
    Whole = 0,     // ID 145
    Rect = 1,      // ID 146, sub_57_2b28
    Ellipse = 2,   // ID 147, sub_30_1788
    Polygon = 3,   // ID 148, sub_31_13e6
    Text = 4,      // ID 158, sub_20_1923
    Freehand = 5,  // ID 159, sub_10_1b20
    MagicWand = 6, // ID 160, sub_9_1066
    Pen = 7,       // ID 163, sub_57_3520
}

/// Feather width W (DS:0x81e8) in pixels, set when the area is confirmed.
///
/// * Rect (seg57:3431): `min(w, h) / {20, 8, 4}`.
/// * Ellipse (seg30:2108): `2*min(rx, ry) / {20, 8, 4}` (pass `w = 2rx, h = 2ry`).
/// * Polygon (seg31:1b22): `min(bbox w, bbox h) / {20, 8, 4}`.
/// * Freehand (seg10:225b): like Polygon. (The original used the bounding
///   box's width only, so tall thin shapes got huge feathers.)
/// * Text (seg20:1176): `{1, 2, 4}` (a second text path, seg20:172a, uses `{1, 2, 3}`).
/// * Magic wand (seg9:1677): `{2, 4, 6}`.
/// * Pen (seg67:1e3d): `{2, 4, 5}` — only tested for non-zero (enables the
///   dab rim, see [`pen_weight`]).
///
/// Integer division truncates, with a minimum of 1 for the smooth levels.
/// (In the original a width of 0 made every weight 0, so e.g. Smooth low on
/// a rectangle narrower than 20 px did nothing at all.)
pub fn feather_width(kind: AreaKind, edge: Edge, w: i32, h: i32) -> i32 {
    let l = edge.level();
    if l == 0 {
        return 0;
    }
    let div = [20, 8, 4][(l - 1) as usize];
    let (w, h) = (w.abs(), h.abs());
    match kind {
        AreaKind::Whole => 0,
        AreaKind::Rect | AreaKind::Ellipse | AreaKind::Polygon | AreaKind::Freehand => {
            (w.min(h) / div).max(1)
        }
        AreaKind::Text => [1, 2, 4][(l - 1) as usize],
        AreaKind::MagicWand => [2, 4, 6][(l - 1) as usize],
        AreaKind::Pen => [2, 4, 5][(l - 1) as usize],
    }
}

// ---------------------------------------------------------------------------
// Rasterisation of shapes (GDI semantics, 1-px pen of the fill colour)
// ---------------------------------------------------------------------------

fn mask_from_fn(w: usize, h: usize, f: impl Fn(i32, i32) -> bool) -> Mask {
    let mut m = Mask::empty(w, h);
    for y in 0..h {
        for x in 0..w {
            if f(x as i32, y as i32) {
                m.data[y * w + x] = 255;
            }
        }
    }
    m
}

/// Rectangle shape (`sub_22_00ca`, GDI `Rectangle` with a 1-px pen of the
/// same colour): covers `[min(x0,x1), max(x0,x1)) × [min(y0,y1), max(y0,y1))`.
/// Half-open per GDI; the vertical side may be shifted by one pixel because
/// the original draws into a bottom-up DIB with `y' = h - y`.
pub fn rasterize_rect(w: usize, h: usize, a: (i32, i32), b: (i32, i32)) -> Mask {
    let (x0, x1) = (a.0.min(b.0), a.0.max(b.0));
    let (y0, y1) = (a.1.min(b.1), a.1.max(b.1));
    mask_from_fn(w, h, |x, y| x >= x0 && x < x1 && y >= y0 && y < y1)
}

/// Ellipse shape (`sub_22_01ec`: `Ellipse(cx-rx, cy-ry, cx+rx, cy+ry)`).
/// Pixel centres `(x+0.5, y+0.5)` inside the ellipse centred on the grid
/// point `(cx, cy)` with radii `rx, ry`.
pub fn rasterize_ellipse(w: usize, h: usize, cx: i32, cy: i32, rx: i32, ry: i32) -> Mask {
    let (rx, ry) = (rx.abs().max(1) as f64, ry.abs().max(1) as f64);
    mask_from_fn(w, h, |x, y| {
        let dx = (x as f64 + 0.5 - cx as f64) / rx;
        let dy = (y as f64 + 0.5 - cy as f64) / ry;
        dx * dx + dy * dy <= 1.0
    })
}

fn draw_line(m: &mut Mask, a: (i32, i32), b: (i32, i32)) {
    let (mut x, mut y) = a;
    let dx = (b.0 - a.0).abs();
    let dy = -(b.1 - a.1).abs();
    let sx = if a.0 < b.0 { 1 } else { -1 };
    let sy = if a.1 < b.1 { 1 } else { -1 };
    let mut err = dx + dy;
    loop {
        if x >= 0 && y >= 0 && (x as usize) < m.w && (y as usize) < m.h {
            m.set(x as usize, y as usize, 255);
        }
        if x == b.0 && y == b.1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
    }
}

/// Polygon shape (`sub_22_0322`): `SetPolyFillMode(ALTERNATE)` + `Polygon`
/// with a 1-px outline of the same colour, i.e. even-odd fill of pixel
/// centres plus the Bresenham outline. The closing edge is implicit.
pub fn rasterize_polygon(w: usize, h: usize, pts: &[(i32, i32)]) -> Mask {
    let n = pts.len();
    let mut m = mask_from_fn(w, h, |x, y| {
        if n < 3 {
            return false;
        }
        let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
        let mut inside = false;
        let mut j = n - 1;
        for i in 0..n {
            let (xi, yi) = (pts[i].0 as f64, pts[i].1 as f64);
            let (xj, yj) = (pts[j].0 as f64, pts[j].1 as f64);
            if (yi > py) != (yj > py) && px < (xj - xi) * (py - yi) / (yj - yi) + xi {
                inside = !inside;
            }
            j = i;
        }
        inside
    });
    for i in 0..n {
        draw_line(&mut m, pts[i], pts[(i + 1) % n]);
    }
    m
}

/// Freehand shape (`sub_10_1b20`): the area is *painted* with the current
/// brush (one dab per mouse position, no interpolation between them; see
/// [`dab_mask`]); the right mouse button paints with "erase". Use
/// [`freehand_dab`] repeatedly, starting from an empty mask or — Preserve
/// mask / previous area — from the previous area's mask.
pub fn rasterize_freehand(w: usize, h: usize, dabs: &[(i32, i32)], brush: &Brush) -> Mask {
    let mut m = Mask::empty(w, h);
    for &c in dabs {
        freehand_dab(&mut m, c, brush, false);
    }
    m
}

/// One freehand dab: left button adds (`erase == false`), right button erases.
pub fn freehand_dab(m: &mut Mask, center: (i32, i32), brush: &Brush, erase: bool) {
    let d = dab_mask(brush.size, brush.kind == BrushKind::Square);
    let (ox, oy) = dab_origin(center, d.w);
    for dy in 0..d.h {
        for dx in 0..d.w {
            if d.get(dx, dy) == 0 {
                continue;
            }
            let (x, y) = (ox + dx as i32, oy + dy as i32);
            if x >= 0 && y >= 0 && (x as usize) < m.w && (y as usize) < m.h {
                m.set(x as usize, y as usize, if erase { 0 } else { 255 });
            }
        }
    }
}

/// Double-click decision (`sub_10_0e94` → `sub_10_0120` reads the mask bit
/// under the cursor): `true` = process the interior (DS:0x12da = 1).
pub fn double_click_inside(shape: &Mask, x: usize, y: usize) -> bool {
    x < shape.w && y < shape.h && shape.get(x, y) != 0
}

/// The selected pixels as a binary mask: the shape itself, or its
/// complement when the exterior was chosen.
pub fn selected_mask(shape: &Mask, inside: bool) -> Mask {
    let mut m = shape.clone();
    for v in m.data.iter_mut() {
        let s = *v != 0;
        *v = if s == inside { 255 } else { 0 };
    }
    m
}

/// Contour for the animated ("marching ants") display, `sub_9_0071`: a
/// selected pixel is on the outline if it touches the image border or has an
/// unselected pixel in its 3×3 neighbourhood.
pub fn outline(mask: &Mask) -> Mask {
    let (w, h) = (mask.w, mask.h);
    let mut out = Mask::empty(w, h);
    for y in 0..h {
        for x in 0..w {
            if mask.get(x, y) == 0 {
                continue;
            }
            let border = x == 0 || y == 0 || x + 1 == w || y + 1 == h;
            let mut edge = border;
            if !edge {
                'n: for ny in y - 1..=y + 1 {
                    for nx in x - 1..=x + 1 {
                        if mask.get(nx, ny) == 0 {
                            edge = true;
                            break 'n;
                        }
                    }
                }
            }
            if edge {
                out.set(x, y, 255);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Smooth edges
// ---------------------------------------------------------------------------

/// Quarter-sine table at DS:0xa1e0, built by `sub_3_1e2e`:
/// `T[i] = (int)(sin(i * 0.00613591796875) * 256.0)` (≈ π/512, truncated
/// toward zero by `_ftol`), then `T[255] = 256`.
pub fn sine_table() -> [i32; 256] {
    let mut t = [0i32; 256];
    for (i, v) in t.iter_mut().enumerate() {
        *v = ((i as f64 * 0.00613591796875).sin() * 256.0) as i32;
    }
    t[255] = 256;
    t
}

/// Per-pixel weight of the processed image, 0..=64 (64 = fully processed).
#[derive(Clone, Debug)]
pub struct Weights {
    pub w: usize,
    pub h: usize,
    pub data: Vec<u8>,
}

impl Weights {
    pub fn get(&self, x: usize, y: usize) -> u8 {
        self.data[y * self.w + x]
    }
    /// Convert to a 0..255 coverage [`Mask`] for `core::blend_through_mask`
    /// (approximate: the original blends with `>> 6`, see [`commit`]).
    pub fn to_mask(&self) -> Mask {
        Mask {
            w: self.w,
            h: self.h,
            data: self
                .data
                .iter()
                .map(|&e| ((e as u32 * 255 + 32) / 64) as u8)
                .collect(),
        }
    }
}

/// Distance index (`sub_32_04c6` / `sub_32_0414`): 255 when `d >= W`, else
/// `(d << 8) / W`; `W == 0` yields 0.
fn ramp_index(d: i32, feather: i32) -> usize {
    if feather <= 0 {
        0
    } else if d >= feather {
        255
    } else {
        ((d.max(0) << 8) / feather) as usize
    }
}

/// Weights of the processed image for a selection (`sub_57_3a32` with the
/// helpers `sub_32_0000`, `sub_32_04c6`, `sub_32_0414`, `sub_32_02c6`,
/// column tables from `sub_31_1c70`).
///
/// `shape` is the drawn shape (non-zero = inside the rectangle/ellipse/…),
/// `inside` the double-click choice. With `Edge::Sharp` the result is the
/// binary selection. Otherwise, for each pixel, the *runs of the shape* that
/// contain it horizontally (`[s, e]`) and vertically (`[t, b]`) give
/// `dh = min(x - s', e' - x)` and `dv = min(y - t', b' - y)` where
///
/// * inside mode: `s' = s`, `e' = e` — except a run reaching the right /
///   bottom image border ends one past it (`e' = W` / `b' = H`);
/// * outside mode: the run is widened by one pixel (`s' = s-1` clamped to 0
///   horizontally, unclamped vertically; `e' = e+1`).
///
/// `e = (T[ramp(dh)] / 4) * T[ramp(dv)] >> 8` is the "inside-ness" of the
/// pixel; the processed weight is `e` (inside) or `64 - e` (outside).
/// Hence the feather always lies inside the drawn shape, the outermost shape
/// pixels get `e = 0`, and the ramp is the product of a horizontal and a
/// vertical quarter-sine (not a Euclidean distance).
pub fn selection_weights(shape: &Mask, inside: bool, edge: Edge, feather: i32) -> Weights {
    // A smooth edge always softens by at least one pixel.
    let feather = if edge == Edge::Sharp {
        feather
    } else {
        feather.max(1)
    };
    let (w, h) = (shape.w, shape.h);
    let s = |x: usize, y: usize| shape.data[y * w + x] != 0;
    let mut out = Weights {
        w,
        h,
        data: vec![0; w * h],
    };
    if edge == Edge::Sharp {
        for i in 0..w * h {
            out.data[i] = if (shape.data[i] != 0) == inside {
                64
            } else {
                0
            };
        }
        return out;
    }
    let t = sine_table();
    // Horizontal ramp index per pixel.
    let mut hidx = vec![0usize; w * h];
    for y in 0..h {
        let mut x = 0;
        while x < w {
            if !s(x, y) {
                x += 1;
                continue;
            }
            let start = x;
            while x < w && s(x, y) {
                x += 1;
            }
            let end = x - 1;
            let (s_eff, e_eff) = if inside {
                (
                    start as i32,
                    if end + 1 == w { w as i32 } else { end as i32 },
                )
            } else {
                ((start as i32 - 1).max(0), end as i32 + 1)
            };
            for px in start..=end {
                let d = (px as i32 - s_eff).min(e_eff - px as i32);
                hidx[y * w + px] = ramp_index(d, feather);
            }
        }
    }
    // Vertical.
    for x in 0..w {
        let mut y = 0;
        while y < h {
            if !s(x, y) {
                y += 1;
                continue;
            }
            let start = y;
            while y < h && s(x, y) {
                y += 1;
            }
            let end = y - 1;
            let (t_eff, b_eff) = if inside {
                (
                    start as i32,
                    if end + 1 == h { h as i32 } else { end as i32 },
                )
            } else {
                (start as i32 - 1, end as i32 + 1)
            };
            for py in start..=end {
                let dv = (py as i32 - t_eff).min(b_eff - py as i32);
                let a = t[hidx[py * w + x]];
                let b = t[ramp_index(dv, feather)];
                let e = ((a / 4) * b) >> 8;
                out.data[py * w + x] = if inside { e as u8 } else { (64 - e) as u8 };
            }
        }
    }
    if !inside {
        // Pixels outside the shape: e = 0 → fully processed.
        for i in 0..w * h {
            if shape.data[i] == 0 {
                out.data[i] = 64;
            }
        }
    }
    out
}

/// Commit a processed image through 0..=64 weights exactly like
/// `sub_57_3a32`: `dst = (processed*e + dst*(64-e)) >> 6` (`dst` holds the
/// original on entry).
pub fn commit(dst: &mut Image, processed: &Image, weights: &Weights) {
    for i in 0..dst.px.len() {
        let e = weights.data[i] as i32;
        if e == 0 {
            continue;
        }
        if e == 64 {
            dst.px[i] = processed.px[i];
            continue;
        }
        let (o, p) = (dst.px[i], processed.px[i]);
        for c in 0..3 {
            dst.px[i][c] = ((p[c] as i32 * e + o[c] as i32 * (64 - e)) >> 6) as u8;
        }
    }
}

// ---------------------------------------------------------------------------
// Magic wand
// ---------------------------------------------------------------------------

/// Colour matching model (INI `RGBMATCH`, DS:0x51a0; dialog MAGICWANG
/// radio buttons 1712 RGB / 1714 HSV).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Match {
    Rgb,
    Hsv,
}

/// The original's HSV (`sub_59_01e0`), fed with 6-bit channels (`c >> 2`).
/// Returns `(h, s, v)`: `h` in 0..1536 (256 per sextant; 0 = channel 0
/// dominant), `s = (max-min)*255/max`, `v = max*4` (0..252). Grey → h = 0.
pub fn hsv_pm(c0: i32, c1: i32, c2: i32) -> (i32, i32, i32) {
    let mn = c0.min(c1).min(c2);
    let mx = c0.max(c1).max(c2);
    let v = mx * 4;
    let s = if mx != 0 { (mx - mn) * 255 / mx } else { 0 };
    if s == 0 {
        return (0, 0, v);
    }
    let d = mx - mn;
    let r = (mx - c0) * 255 / d;
    let g = (mx - c1) * 255 / d;
    let b = (mx - c2) * 255 / d;
    let mut hh = if mx == c0 {
        b - g
    } else if mx == c1 {
        r - b + 512
    } else {
        g - r + 1024
    };
    if hh < 0 {
        hh += 1536;
    }
    (hh, s, v)
}

/// Circular hue distance (`sub_9_0000`): `min(|Δ|, 1536-|Δ|) / 6`, 0..=128.
pub fn hue_distance(a: i32, b: i32) -> i32 {
    let (mx, mn) = (a.max(b), a.min(b));
    let d1 = (mx - mn).rem_euclid(0x600);
    let d2 = (mn - mx + 0x600).rem_euclid(0x600);
    d1.min(d2) / 6
}

/// Does `px` match the seed colour? (`sub_9_0474`, seg9:0656..0793.)
///
/// * RGB: `|c - seed| < tol` for each of the three 8-bit channels.
/// * HSV (on `c >> 2`): `|S - S0| < 2·tol && hue_distance < tol &&
///   |V - V0| < 2·tol`.
pub fn color_matches(px: Rgb, seed: Rgb, tolerance: i32, mode: Match) -> bool {
    match mode {
        Match::Rgb => (0..3).all(|c| (px[c] as i32 - seed[c] as i32).abs() < tolerance),
        Match::Hsv => {
            let (h0, s0, v0) = hsv_pm(
                seed[0] as i32 >> 2,
                seed[1] as i32 >> 2,
                seed[2] as i32 >> 2,
            );
            let (h, s, v) = hsv_pm(px[0] as i32 >> 2, px[1] as i32 >> 2, px[2] as i32 >> 2);
            (s - s0).abs() < 2 * tolerance
                && hue_distance(h0, h) < tolerance
                && (v - v0).abs() < 2 * tolerance
        }
    }
}

/// Magic wand (`sub_9_0474`): binary shape mask (255 = matched).
///
/// `tolerance` is the dialog value 0..=100 (scroll bar, line ±1, page ±15;
/// OK clamps it to ≥ 1; built-in default 30, shipped INI 51). With
/// `unifold == false` every matching pixel of the image is selected; with
/// `unifold == true` only the **4-connected** component containing the seed
/// (GDI `FloodFill` on the 1-bpp match bitmap, then `SRCERASE` against a
/// copy). A seed that does not match itself (tolerance 0) gives an empty
/// mask. The seed colour is the pixel under the click.
pub fn magic_wand(
    img: &Image,
    seed: (usize, usize),
    tolerance: i32,
    mode: Match,
    unifold: bool,
) -> Mask {
    let (w, h) = (img.w, img.h);
    let sc = img.get(seed.0, seed.1);
    let mut m = Mask::empty(w, h);
    // Precompute seed HSV once (as the original does).
    let seed_hsv = hsv_pm(sc[0] as i32 >> 2, sc[1] as i32 >> 2, sc[2] as i32 >> 2);
    for i in 0..w * h {
        let p = img.px[i];
        let ok = match mode {
            Match::Rgb => (0..3).all(|c| (p[c] as i32 - sc[c] as i32).abs() < tolerance),
            Match::Hsv => {
                let (h, s, v) = hsv_pm(p[0] as i32 >> 2, p[1] as i32 >> 2, p[2] as i32 >> 2);
                (s - seed_hsv.1).abs() < 2 * tolerance
                    && hue_distance(seed_hsv.0, h) < tolerance
                    && (v - seed_hsv.2).abs() < 2 * tolerance
            }
        };
        if ok {
            m.data[i] = 255;
        }
    }
    if !unifold {
        return m;
    }
    let mut out = Mask::empty(w, h);
    if m.get(seed.0, seed.1) == 0 {
        return out;
    }
    let mut stack = vec![seed];
    out.set(seed.0, seed.1, 255);
    while let Some((x, y)) = stack.pop() {
        let mut push = |nx: usize, ny: usize, out: &mut Mask| {
            if m.get(nx, ny) != 0 && out.get(nx, ny) == 0 {
                out.set(nx, ny, 255);
                stack.push((nx, ny));
            }
        };
        if x > 0 {
            push(x - 1, y, &mut out);
        }
        if x + 1 < w {
            push(x + 1, y, &mut out);
        }
        if y > 0 {
            push(x, y - 1, &mut out);
        }
        if y + 1 < h {
            push(x, y + 1, &mut out);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Pen / brush
// ---------------------------------------------------------------------------

/// Brush type (INI `BRUSH`, DS:0x93f0). The menu/toolbox only offer 0 and 1;
/// 2..5 are reachable through PMAN.INI only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BrushKind {
    #[default]
    Square = 0, // ID 218
    Circle = 1, // ID 217
    /// Circle; each pixel painted with probability (AIRBRUSH+1)/32768.
    Airbrush = 2,
    /// Circle; opacity fades along the stroke (FADING).
    FadingOpacity = 3,
    /// Circle whose diameter shrinks along the stroke (FADING).
    FadingSize = 4,
    /// Circle; random spray whose density fades along the stroke.
    FadingSpray = 5,
}

impl BrushKind {
    pub fn from_ini(v: i32) -> BrushKind {
        match v {
            1 => BrushKind::Circle,
            2 => BrushKind::Airbrush,
            3 => BrushKind::FadingOpacity,
            4 => BrushKind::FadingSize,
            5 => BrushKind::FadingSpray,
            _ => BrushKind::Square,
        }
    }
}

/// Pen size (1,3,5,7,9,11 — IDs 212,213,214,215,249,216; INI `PENSIZE`) and type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Brush {
    pub size: usize,
    pub kind: BrushKind,
}

/// Top-left of a dab of `size` centred on `c` (GDI box `[c - s/2, c + (s+1)/2)`).
fn dab_origin(c: (i32, i32), size: usize) -> (i32, i32) {
    (c.0 - size as i32 / 2, c.1 - size as i32 / 2)
}

/// Dab shape, `size × size` (`sub_22_062a`): square = full box; circle =
/// GDI `Ellipse` in the box, i.e. offsets with `dx² + dy² <= (size/2)²`
/// (size/2 as a real number; size 3 → full 3×3). Size 1 is always one pixel
/// (the original draws a 2×2 `Rectangle` that is then clipped by the 1-px
/// processing box — uncertain by one pixel).
pub fn dab_mask(size: usize, square: bool) -> Mask {
    let size = size.max(1);
    let r = size as f64 / 2.0;
    let c = (size / 2) as f64;
    mask_from_fn(size, size, |x, y| {
        if square || size <= 2 {
            return true;
        }
        let (dx, dy) = (x as f64 - c, y as f64 - c);
        dx * dx + dy * dy <= r * r
    })
}

/// The dab's processing box clipped to the image (`[0x9578..]`, seg67:1ddd
/// then `sub_10_0894`).
pub fn dab_rect(center: (i32, i32), size: usize, w: usize, h: usize) -> Rect {
    let (x0, y0) = dab_origin(center, size);
    let (x1, y1) = (x0 + size as i32, y0 + size as i32);
    let (cx0, cy0) = (x0.max(0), y0.max(0));
    let (cx1, cy1) = (x1.min(w as i32), y1.min(h as i32));
    if cx1 <= cx0 || cy1 <= cy0 {
        return Rect::default();
    }
    Rect {
        x: cx0 as usize,
        y: cy0 as usize,
        w: (cx1 - cx0) as usize,
        h: (cy1 - cy0) as usize,
    }
}

/// Per-stroke state: the fading counter DS:0x81e4. Reset to 256 on button
/// down; every WM_MOUSEMOVE dab first subtracts `FADING` (INI, default 3)
/// and clamps to ≥ 1.
#[derive(Clone, Debug)]
pub struct Stroke {
    pub fade: i32,
    pub fading_step: i32,
}

impl Stroke {
    pub fn begin(fading_step: i32) -> Stroke {
        Stroke {
            fade: 256,
            fading_step,
        }
    }
    /// Call before each mouse-move dab (not before the button-down dab).
    pub fn advance(&mut self) {
        self.fade -= self.fading_step;
        if self.fade <= 0 {
            self.fade = 1;
        }
    }
    /// Fading opacity t (0..=64, min 3): `((((fade/4)²)/64)²)/64` (seg57:3cf8).
    pub fn fade_t(&self) -> i32 {
        let q = self.fade / 4;
        let a = (q * q) / 64;
        ((a * a) / 64).max(3)
    }
    /// Shrunk dab diameter for [`BrushKind::FadingSize`]:
    /// `max(1, (size*fade + 128) >> 8)` (seg22:0775).
    pub fn fade_size(&self, size: usize) -> usize {
        (((size as i32 * self.fade + 128) >> 8).max(1)) as usize
    }
}

/// Pen parameters.
#[derive(Clone, Copy, Debug)]
pub struct PenParams {
    pub brush: Brush,
    pub edge: Edge,
    /// INI `AIRBRUSH` (DS:0x956a, default 16000; hidden commands 238/239/240
    /// set 16000/8000/4000). A pixel keeps the source if `rand() > airbrush`.
    pub airbrush: i32,
}

/// Pen opacity in 1/64 for a dab pixel (`sub_57_3a32`, pen branch
/// seg57:4325): Sharp → 64; pen size ≤ 3 → 32/16/8 (low/medium/high);
/// size > 3 → `rim / {2,4,8}` where `rim` (`sub_32_0608`) is 32 on the dab's
/// outer ring (top/bottom row of the box, left/right column for the square,
/// first/last pixel of the row for the circle) and 64 elsewhere.
pub fn pen_weight(edge: Edge, size: usize, on_rim: bool) -> i32 {
    let l = edge.level();
    if l == 0 {
        return 64;
    }
    if size <= 3 {
        return [32, 16, 8][(l - 1) as usize];
    }
    let rim = if on_rim { 32 } else { 64 };
    rim / [2, 4, 8][(l - 1) as usize]
}

/// Commit one pen dab (one WM_MOUSEMOVE). Faithful to the per-row pipeline
/// of `sub_57_3a32` for area 7:
///
/// 1. `out = processed` over the dab box (`processed` = the operation applied
///    to `source`; the original recomputes the operation for the box on
///    every dab by re-sending WM_COMMAND).
/// 2. Brush-type effects over the row's first..last dab pixel: Airbrush
///    (`rand() > airbrush` → source), FadingOpacity (`out = (out*t +
///    src*(64-t)) >> 6`), FadingSpray (`rand() % 64 > t` → source).
/// 3. Pixels of the box outside the dab shape keep their current value.
/// 4. Edge opacity: `out = (out*e + src*(64-e)) >> 6`, `e = pen_weight()`.
///
/// `dst` receives `out` for the whole box. For in-place operations (fills)
/// pass `source = dst` (copy) so step 3 is a no-op and opacity accumulates;
/// for filters pass the backup so repeated dabs do not compound.
pub fn paint_dab(
    dst: &mut Image,
    processed: &Image,
    source: &Image,
    center: (i32, i32),
    p: &PenParams,
    stroke: &Stroke,
    rng: &mut MsRand,
) {
    let size = p.brush.size.max(1);
    let r = dab_rect(center, size, dst.w, dst.h);
    if r.is_empty() {
        return;
    }
    // Dab shape (FadingSize shrinks the drawn ellipse, not the box).
    let shape_size = if p.brush.kind == BrushKind::FadingSize {
        stroke.fade_size(size)
    } else {
        size
    };
    let square = p.brush.kind == BrushKind::Square;
    let shape = dab_mask(shape_size, square);
    let (sox, soy) = dab_origin(center, shape_size);
    let in_shape = |x: i32, y: i32| -> bool {
        let (dx, dy) = (x - sox, y - soy);
        dx >= 0
            && dy >= 0
            && (dx as usize) < shape.w
            && (dy as usize) < shape.h
            && shape.get(dx as usize, dy as usize) != 0
    };
    let t = stroke.fade_t();
    let mut row: Vec<Rgb> = vec![[0; 3]; r.w];
    for y in r.y..r.y + r.h {
        let yi = y as i32;
        // First/last selected pixel of the row (X[2], X[3]).
        let xs: Vec<usize> = (r.x..r.x + r.w)
            .filter(|&x| in_shape(x as i32, yi))
            .collect();
        for (i, x) in (r.x..r.x + r.w).enumerate() {
            row[i] = processed.get(x, y);
        }
        if let (Some(&first), Some(&last)) = (xs.first(), xs.last()) {
            for x in first..=last {
                let i = x - r.x;
                let src = source.get(x, y);
                match p.brush.kind {
                    BrushKind::Airbrush => {
                        if rng.rand() > p.airbrush {
                            row[i] = src;
                        }
                    }
                    BrushKind::FadingOpacity => {
                        for c in 0..3 {
                            row[i][c] =
                                ((row[i][c] as i32 * t + src[c] as i32 * (64 - t)) >> 6) as u8;
                        }
                    }
                    BrushKind::FadingSpray => {
                        if rng.rand() % 64 > t {
                            row[i] = src;
                        }
                    }
                    _ => {}
                }
            }
        }
        // Pixels of the box outside the dab shape keep what is already
        // painted. (The original restored them from `source`, which for
        // filters is the backup, so a circular brush un-painted earlier dabs
        // in the corners of its box.)
        for (i, x) in (r.x..r.x + r.w).enumerate() {
            if !in_shape(x as i32, yi) {
                row[i] = dst.get(x, y);
            }
        }
        if p.edge != Edge::Sharp {
            let first = xs.first().copied();
            let last = xs.last().copied();
            for (i, x) in (r.x..r.x + r.w).enumerate() {
                if !in_shape(x as i32, yi) {
                    continue;
                }
                let rim = y == r.y
                    || y + 1 == r.y + r.h
                    || if square {
                        x == r.x || x + 1 == r.x + r.w
                    } else {
                        Some(x) == first || Some(x) == last
                    };
                let e = pen_weight(p.edge, size, rim);
                let src = source.get(x, y);
                for c in 0..3 {
                    row[i][c] = ((row[i][c] as i32 * e + src[c] as i32 * (64 - e)) >> 6) as u8;
                }
            }
        }
        for (i, x) in (r.x..r.x + r.w).enumerate() {
            dst.set(x, y, row[i]);
        }
    }
}

/// Right-button erase while painting (seg67:21aa → `sub_19_00da`): the
/// backup is copied back through the dab exactly like a paint dab whose
/// "processed" image is the backup.
pub fn erase_dab(
    dst: &mut Image,
    backup: &Image,
    center: (i32, i32),
    p: &PenParams,
    stroke: &Stroke,
    rng: &mut MsRand,
) {
    let cur = dst.clone();
    paint_dab(dst, backup, &cur, center, p, stroke, rng);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sine_table_shape() {
        let t = sine_table();
        assert_eq!(t[0], 0);
        assert_eq!(t[255], 256);
        assert_eq!(t[128], 181); // 256*sin(π/4) = 181.02
        assert!(t.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn feather_widths() {
        assert_eq!(feather_width(AreaKind::Rect, Edge::SmoothLow, 100, 60), 3);
        assert_eq!(
            feather_width(AreaKind::Rect, Edge::SmoothMedium, 100, 60),
            7
        );
        assert_eq!(feather_width(AreaKind::Rect, Edge::SmoothHigh, 100, 60), 15);
        assert_eq!(
            feather_width(AreaKind::Freehand, Edge::SmoothHigh, 100, 10),
            2
        );
        assert_eq!(feather_width(AreaKind::Rect, Edge::SmoothLow, 15, 40), 1);
        assert_eq!(
            feather_width(AreaKind::MagicWand, Edge::SmoothHigh, 0, 0),
            6
        );
        assert_eq!(feather_width(AreaKind::Rect, Edge::Sharp, 100, 60), 0);
        assert_eq!(Edge::from_command(151), Some(Edge::SmoothMedium));
    }

    #[test]
    fn rect_is_half_open() {
        let m = rasterize_rect(10, 10, (2, 3), (5, 7));
        assert_eq!(
            m.bounds(),
            Rect {
                x: 2,
                y: 3,
                w: 3,
                h: 4
            }
        );
    }

    #[test]
    fn polygon_and_ellipse() {
        let m = rasterize_polygon(10, 10, &[(1, 1), (8, 1), (8, 8), (1, 8)]);
        assert_eq!(m.get(4, 4), 255);
        assert_eq!(m.get(8, 8), 255); // outline included
        assert_eq!(m.get(9, 9), 0);
        let e = rasterize_ellipse(20, 20, 10, 10, 5, 3);
        assert_eq!(e.get(10, 10), 255);
        assert_eq!(e.get(10, 14), 0);
        assert_eq!(
            e.bounds(),
            Rect {
                x: 5,
                y: 7,
                w: 10,
                h: 6
            }
        );
    }

    #[test]
    fn hsv_matches_original_scale() {
        assert_eq!(hsv_pm(63, 0, 0), (0, 255, 252));
        assert_eq!(hsv_pm(0, 63, 0), (512, 255, 252));
        assert_eq!(hsv_pm(0, 0, 63), (1024, 255, 252));
        assert_eq!(hsv_pm(20, 20, 20), (0, 0, 80));
        assert_eq!(hue_distance(10, 1530), 16 / 6);
        assert_eq!(hue_distance(0, 768), 128);
    }

    #[test]
    fn wand_rgb_unifold_vs_global_and_4_connectivity() {
        let mut img = Image::new(5, 5, [0, 0, 0]);
        // Two red blobs: (0,0)-(1,1) and a diagonal neighbour at (2,2), plus (4,4).
        for &(x, y) in &[(0, 0), (1, 0), (0, 1), (1, 1), (2, 2), (4, 4)] {
            img.set(x, y, [200, 10, 10]);
        }
        img.set(1, 1, [210, 20, 5]);
        let g = magic_wand(&img, (0, 0), 30, Match::Rgb, false);
        assert_eq!(g.data.iter().filter(|&&v| v != 0).count(), 6);
        let u = magic_wand(&img, (0, 0), 30, Match::Rgb, true);
        assert_eq!(u.data.iter().filter(|&&v| v != 0).count(), 4); // (2,2) only diagonal → excluded
        // Strict "<": a difference equal to the tolerance does not match.
        let t = magic_wand(&img, (0, 0), 10, Match::Rgb, false);
        assert_eq!(t.get(1, 1), 0);
        assert!(
            magic_wand(&img, (0, 0), 0, Match::Rgb, true)
                .data
                .iter()
                .all(|&v| v == 0)
        );
    }

    #[test]
    fn wand_hsv_ignores_brightness_more() {
        let mut img = Image::new(2, 1, [200, 40, 40]);
        img.set(1, 0, [150, 30, 30]); // darker red, same hue
        let m = magic_wand(&img, (0, 0), 30, Match::Hsv, false);
        assert_eq!(m.get(1, 0), 255);
        let r = magic_wand(&img, (0, 0), 30, Match::Rgb, false);
        assert_eq!(r.get(1, 0), 0);
    }

    #[test]
    fn sharp_weights_are_binary() {
        let s = rasterize_rect(8, 8, (2, 2), (6, 6));
        let wi = selection_weights(&s, true, Edge::Sharp, 0);
        assert_eq!(wi.get(3, 3), 64);
        assert_eq!(wi.get(0, 0), 0);
        let wo = selection_weights(&s, false, Edge::Sharp, 0);
        assert_eq!(wo.get(3, 3), 0);
        assert_eq!(wo.get(0, 0), 64);
    }

    #[test]
    fn smooth_weights_ramp_inside_shape() {
        let s = rasterize_rect(60, 60, (10, 10), (50, 50));
        let wi = selection_weights(&s, true, Edge::SmoothHigh, 10);
        assert_eq!(wi.get(10, 30), 0); // boundary pixel of the shape
        assert_eq!(wi.get(30, 30), 64); // deep inside
        assert_eq!(wi.get(5, 30), 0); // outside
        let a = wi.get(12, 30);
        let b = wi.get(16, 30);
        assert!(a < b && b < 64);
        // Outside mode: exterior fully processed, ramp toward the interior.
        let wo = selection_weights(&s, false, Edge::SmoothHigh, 10);
        assert_eq!(wo.get(5, 30), 64);
        assert_eq!(wo.get(30, 30), 0);
        assert!(wo.get(10, 30) > 0 && wo.get(10, 30) < 64);
        // Zero feather with a smooth edge still processes the area (the
        // original's result was no effect at all).
        let wz = selection_weights(&s, true, Edge::SmoothLow, 0);
        assert_eq!(wz.get(30, 30), 64);
    }

    #[test]
    fn commit_uses_shift6() {
        let mut dst = Image::new(1, 1, [0, 0, 0]);
        let p = Image::new(1, 1, [255, 255, 255]);
        let w = Weights {
            w: 1,
            h: 1,
            data: vec![32],
        };
        commit(&mut dst, &p, &w);
        assert_eq!(dst.px[0], [127, 127, 127]);
    }

    #[test]
    fn dab_shapes() {
        assert_eq!(dab_mask(1, false).data, vec![255]);
        assert!(dab_mask(3, false).data.iter().all(|&v| v == 255));
        let c = dab_mask(5, false);
        assert_eq!(c.get(0, 0), 0);
        assert_eq!(c.get(2, 0), 255);
        assert_eq!(c.data.iter().filter(|&&v| v != 0).count(), 21);
        assert_eq!(
            dab_rect((0, 0), 5, 10, 10),
            Rect {
                x: 0,
                y: 0,
                w: 3,
                h: 3
            }
        );
    }

    #[test]
    fn pen_weights_and_fading() {
        assert_eq!(pen_weight(Edge::Sharp, 11, true), 64);
        assert_eq!(pen_weight(Edge::SmoothLow, 3, false), 32);
        assert_eq!(pen_weight(Edge::SmoothHigh, 11, false), 8);
        assert_eq!(pen_weight(Edge::SmoothMedium, 11, true), 8);
        let mut s = Stroke::begin(3);
        assert_eq!(s.fade_t(), 64);
        assert_eq!(s.fade_size(11), 11);
        for _ in 0..200 {
            s.advance();
        }
        assert_eq!(s.fade, 1);
        assert_eq!(s.fade_t(), 3);
        assert_eq!(s.fade_size(11), 1);
    }

    #[test]
    fn paint_dab_sharp_and_soft() {
        let src = Image::new(9, 9, [0, 0, 0]);
        let proc_ = Image::new(9, 9, [200, 200, 200]);
        let mut dst = src.clone();
        let p = PenParams {
            brush: Brush {
                size: 5,
                kind: BrushKind::Circle,
            },
            edge: Edge::Sharp,
            airbrush: 16000,
        };
        let st = Stroke::begin(3);
        let mut rng = MsRand::new(1);
        paint_dab(&mut dst, &proc_, &src, (4, 4), &p, &st, &mut rng);
        assert_eq!(dst.get(4, 4), [200; 3]);
        assert_eq!(dst.get(2, 2), [0; 3]); // circle corner restored
        assert_eq!(dst.get(0, 0), [0; 3]);
        let mut dst2 = src.clone();
        let p2 = PenParams {
            edge: Edge::SmoothLow,
            ..p
        };
        paint_dab(&mut dst2, &proc_, &src, (4, 4), &p2, &st, &mut rng);
        assert_eq!(dst2.get(4, 4), [100; 3]); // 200*32/64
        assert_eq!(dst2.get(4, 2), [50; 3]); // rim: 16/64
        // Earlier paint in the corners of a circular dab's box survives
        // (the original reset it to the backup).
        let mut dst3 = Image::new(9, 9, [77, 77, 77]);
        paint_dab(&mut dst3, &proc_, &src, (4, 4), &p, &st, &mut rng);
        assert_eq!(dst3.get(2, 2), [77; 3]);
        assert_eq!(dst3.get(4, 4), [200; 3]);
    }

    #[test]
    fn airbrush_density() {
        let src = Image::new(64, 64, [0, 0, 0]);
        let proc_ = Image::new(64, 64, [255, 255, 255]);
        let p = PenParams {
            brush: Brush {
                size: 11,
                kind: BrushKind::Airbrush,
            },
            edge: Edge::Sharp,
            airbrush: 16000,
        };
        let st = Stroke::begin(3);
        let mut rng = MsRand::new(7);
        let mut painted = 0;
        let mut total = 0;
        for cy in (6..60).step_by(11) {
            for cx in (6..60).step_by(11) {
                let mut d = src.clone();
                paint_dab(&mut d, &proc_, &src, (cx, cy), &p, &st, &mut rng);
                let m = dab_mask(11, false);
                for y in 0..11 {
                    for x in 0..11 {
                        if m.get(x, y) != 0 {
                            total += 1;
                            if d.get(cx as usize - 5 + x, cy as usize - 5 + y)[0] == 255 {
                                painted += 1;
                            }
                        }
                    }
                }
            }
        }
        let frac = painted as f64 / total as f64;
        assert!((frac - 16001.0 / 32768.0).abs() < 0.05, "{frac}");
    }

    #[test]
    fn outline_and_double_click() {
        let s = rasterize_rect(10, 10, (2, 2), (7, 7));
        let o = outline(&s);
        assert_eq!(o.get(2, 2), 255);
        assert_eq!(o.get(4, 4), 0);
        assert!(double_click_inside(&s, 4, 4));
        assert!(!double_click_inside(&s, 0, 0));
        let sel = selected_mask(&s, false);
        assert_eq!(sel.get(0, 0), 255);
        assert_eq!(sel.get(4, 4), 0);
    }

    #[test]
    fn freehand_paint_and_erase() {
        let b = Brush {
            size: 3,
            kind: BrushKind::Square,
        };
        let mut m = rasterize_freehand(10, 10, &[(3, 3), (4, 3)], &b);
        assert_eq!(m.get(5, 4), 255);
        freehand_dab(&mut m, (3, 3), &b, true);
        assert_eq!(m.get(3, 3), 0);
        assert_eq!(m.get(5, 3), 255);
    }
}
