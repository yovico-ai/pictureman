# Edit → Transformation (Size, Clip, Move, Flip, Rotate, Rubber, Deformations)

Rust port: `src/ops/transform.rs`. Handlers: README dispatch table
(seg67). Notation: `x0,y0,w,h` = DS:0x9578/957a/957c/957e (current rect),
`/` = MS C long division (truncates toward 0), `>>` = arithmetic shift.

## Common machinery

**Fixed point.** All geometric ops are *inverse* mappings
`(x·16, y·16) → (sx, sy)` in 1/16 px, held in the far pointer DS:0x6e48.
Engines (seg15):

| engine | used by | sampling | outside |
|---|---|---|---|
| `sub_15_0000` in place (`0c30` = masked variant) | whole image / painting / rect | bilinear, weights `(15-f)`,`f` with `f = s & 15`, sum/225 truncated | neighbours outside image = bg colour |
| `sub_15_0620` → temp fragment | Rotate, Rubber, Deform in area mode | rounded `(s+8)>>4` must be inside image **and** selection (`sub_10_0efa`), else key colour; inside: same 1/15 bilinear; a real pixel equal to the key is decremented by 1 | key `(fb,fc,fd)` = transparent |
| `sub_15_1696` → temp fragment | Flip, Move in area mode | nearest `s>>4`, clamped to image; selection test | key |

Confidence high (code read fully; weight algebra checked).

**Storage is bottom-up** (DIB order): the selection mask is drawn at GDI
`y = H − y` (`sub_10_0f2e`), and with that convention rotation by a positive
angle is counter-clockwise, as the manual says. All formulas below are in this
bottom-up space; the port flips on entry/exit. Consequences: CW/CCW sense,
phase origin of the vertical wave, and grid alignment of Size are measured
from the bottom row. Confidence medium-high (two independent indications).

**Placing a fragment** (`sub_50_0000`, after the interactive rect editor
`sub_57_2810`): the fragment is scaled into the user's rectangle with the Size
resampler (below). New selection = bilinear opacity of the 4 fragment
neighbours (0/255, weights /256) `≥ 128`; with Edge smooth low/medium/high the
mask is then blurred with radius `min(w,h)/20`, `/8`, `/4` (`0x6e34`; not
ported). Colours: transparent neighbours are replaced by the destination pixel
underneath; all four transparent → destination unchanged. The source area is
not erased (Move = clone). If the rectangle was not changed, fits the image and
the area type is whole/rect/ellipse, a fast path (`0x7158 = 1`) keeps the
original mask. Confidence high for sampling/threshold, medium for the final
write (assumed hard mask commit).

**UI semantics.** Transformation in a selected area: after the dialog (if
any), the fragment is computed and its rectangle is shown (for Rotate: the
enlarged bounding box centred on the selection); the user drags/resizes it,
double-click accepts, Esc / click outside cancels. Unregistered copies only
preview (string 212). Whole image: applied directly. Painting mode (area 7)
applies the in-place mapping under the brush.

## Size (102) — `sub_60_0f40` dialog, `sub_60_0916` resampler
Whole image only. For dst row `r`, col `c` (src `sw×sh` → `dw×dh`):
```
sy = sh·r/dh   fy = ((sh·r)<<4)/dh & 15     (same for x)
out = ((16-fx)(16-fy)·p00 + fx(16-fy)·p10 + (16-fx)fy·p01 + fx·fy·p11) >> 8
```
Top-left aligned, no area averaging when shrinking (2× shrink = every 2nd
row/column, counted from the bottom row); right/bottom neighbour past the edge
= last pixel. Dialog: Width/Height, Pels/Cm/In, DPI screen/printer/custom,
Reset. Confidence high.

## Clip (108) — `sub_56_02a8`
Rect selected with `sub_57_23d4`, clamped to the image, cropped
(`sub_56_0000`). Confidence high.

## Flip (136 H / 140 V) — `sub_35_0126(0|1)`
`sx = ((2·x0 + w − 1)<<4) − x` (H, `sub_35_009a`); `sy = ((2·y0 + h − 1)<<4) − y`
(V, `sub_35_0050`). Whole image: `sub_15_0000`, bg 0. Area: `sub_15_1696` +
placement. Exact. Confidence high.

## Move (167) — `sub_5_007a`
Not allowed for the whole image. Area: identity extraction (`sub_15_1696`) +
placement (copy). Painting mode: mapping `sub_5_0000`
`s = p − 16·(Δx, Δy)` with Δ = brush − reference (Shift+click sets the
reference) = clone brush, bg = 0x6e40. Confidence high (painting offsets medium).

## Rotate (137) — `sub_64_0548` (area) / `sub_64_011e` (whole)
Dialog ROTATE: angle (double via atof), reset to `90.` each time; bg = current
background colour DS:0x6e40.
```
k = 0.01745277777777778 (π≈3.1415 /180)
C = trunc(cos(a·k)·4096.5)   S = trunc(sin(a·k)·4096.5)
u = x − (W'−1)·8   v = y − (H'−1)·8            (1/16 px, dst centre)
sx = (u·C + v·S)/4096 + (W−1)·8
sy = (v·C − u·S)/4096 + (H−1)·8
```
Bilinear 1/15. Output size: if `trunc(a) % 90 == 0` → same (×180) or swapped;
else whole image `W' = (C·W + S·H)/4096`, `H' = (C·H + S·W)/4096` **no abs**
(angles outside 0..90 crop or fail — likely an original bug, kept); area mode
uses `labs` per product and centres the new rect:
`x0' = (2x0 − W' + w)/2`, `y0' = (2y0 − H' + h)/2`, both centres = `2x0+w`.
Confidence high (formulas), medium (CCW sense depends on bottom-up storage).

## Rubber (228) — `sub_14_0a68`, tables `sub_14_0000`, maps `sub_14_0372` (rect) / `sub_14_04bc` (ellipse area)
User drags from `F` (button down, DS:0x7170/7216) to `T` (DS:0x6e4e/6e5c);
reference `P = T − (x0,y0)`, displacement `d = 16·(F − T)`. Tables (×4096):
```
X[0]=0;  i=1..w-1:  X[i] = (i² <<12)/((P−i)² + i²)          if P ≥ i
                          ((i−w)² <<12)/((P−i)² + (i−w)²)    otherwise
Y[j], j=1..h: same with P_y, h
sx = x + (X[i]·dx/4096)·Y[j]/4096     sy = y + (Y[j]·dy/4096)·X[i]/4096
```
(i, j = pixel offsets in the rect; outside rect identity). So `T` shows the
pixel from `F`, the rect border is fixed, influence is separable rational.
Ellipse area adds chord tables `Ecol[i] = trunc(h·sqrt(max(0,1−4t²/w²)))`,
`t=i−w/2`, `Erow[j] = trunc(w·sqrt(max(0,1−4t²/h²)))`, maps `i' = (ix −
(w−Erow[j])/2)·w/Erow[j]` (likewise `j'`), identity if out of range, and
multiplies the displacement by `·Erow[j]/h·Ecol[i]/w`. Painting mode uses the
grid-relative displacement scaled by the image size (not ported). Area types
0,1,7 in place (bg 0x6e40); others extract a fragment. `((i−w)²<<12)` overflows
32 bits for selections wider than ~724 px in the original; port uses 64-bit.
Confidence high (rect), medium-high (ellipse).

## Deformations (141) — `sub_37_0dcc`, dialog SETTINGDLGPROC `sub_37_00d4`, maps in seg12
Dialog: combobox (no sort) in this order, default index 0; two scrollbars
range 0..50, default 0 (the "256"/"145" resource texts are placeholders,
replaced by "0"). `p` = Distortion, `s` = Size; `pa = p+1` (DS:0xa3a),
`pc = s+1` (DS:0xa3c). Size is enabled only for the waves. The `deformchild`
preview runs the same mapping on a thumbnail.

Setup `sub_12_00da(x0,y0,w,h)`: `R2 = 64(w²+h²)`, `D = trunc(sqrt R2)`
(half-diagonal ·16), `cx = 16x0 + 8w`, `cy = 16y0 + 8h`, `A = 8(w−1)`,
`B = 8(h−1)`, `T[k] = trunc(sqrt(R2·k/1024))`; `K = 75·D/(p+2)` (DS:0x921a).
Radius `R(u,v) = T[min(((u²+v²)&~15)·64/(R2>>4), 1023)]` (`sub_12_0238`).
Ellipse→circle: `v = (y−cy)·A/B`, `u = x−cx`, `r = R(u,v)`; results in circle
space are mapped back by `sy = (sy'−cx)·B/A + cy` (note `cx` is the circle
offset for both axes). "check" = result must satisfy
`16x0 ≤ sx, sy' < 16(x0+w)` else identity.

| # | name (string id) | map | formula (identity if `r > A`, or `r = 0` for waves/whirl) |
|---|---|---|---|
|0| Elliptic convex mirror (182) | `0486` | `s' = ((u/2)·K + r·u)/(K/2 + A)` per axis; check always |
|1| Elliptic concave mirror (181) | `02fe` | `s' = u·(K − r)/(K − A)`; check only whole image/painting |
|2| Cylindric concave, vertical (183) | `0646` | `sx = u(K−|u|)/(K−A) + cx`; check (whole/paint) `16x0 < sx < 16(x0+w)` |
|3| Cylindric convexe, vertical (184) | `06fc` | `sx = (|u|·u + (u/2)·K)/(K/2 + A) + cx` |
|4| Cylindric concave, horizontal (191) | `07ba` | `sy = v(K−|v|)/(K−B) + cy`, `v=y−cy`; check always |
|5| Cylindric convex, horizontal (192) | `0870` | `sy = (|v|·v + (v/2)·K)/(K/2 + B) + cy` |
|6| Horizontal wave (193) | `0b50` | `amp = ((w·pa/pc)/50)<<3`; `t = x−16x0`; `ph = ((t>>4)·25736/(w−1)·pc) % 25736`; `sx = x + amp·sin(ph)/4096`; identity unless `x0 ≤ sx>>4 < x0+w` |
|7| Vertical wave (194) | `0c2e` | same on y with h; **adds 16·x0 instead of 16·y0** (bug, kept; harmless for whole image) |
|8| Circular wave (195) | `0d1a` | `amp = ((A·pa/pc)/50)>>1`; `ph = ((r>>4)·25736/(w−1)·pc) % 25736`; `δ = sin(ph)·amp/4096`; `s' = δ·u/r + u + cx`; check |
|9| Whirlpool, clockwise (198) | `0f82` | `θ = (((r−A)·257/A)·(r−A)/A)·pa` (16-bit); `sx = cosθ·u/4096 − sinθ·v/4096 + cx`, `sy' = cosθ·v/4096 + sinθ·u/4096 + cx`; check |
|10| Whirlpool, counterclockwise (199) | `11bc` | signs of the sin terms swapped |

Trig (`sub_12_0926` table, `09cc` sin, `0a78` cos): phase unit 25736 = 2π;
`S[k] = trunc(sin(k·3.1415/2048)·4096)`, k=0..1024, quadrant folding at
6434/12868/19302; a negative remainder returns the quotient (0). Max whirl at
the centre = `257·pa/25736·360° ≈ 3.6°·(p+1)`; wave amplitude = `(p+1)%` of
the wavelength `w/(s+1)` (circular: half that, wavelength `(w−1)/(s+1)`).
Whole image: `sub_15_0000`, bg black. Area: `sub_15_0620` + placement.
32-bit overflow in `R()` for radii > 512 px in the original; port is 64-bit.
Confidence: high for formulas and parameter plumbing; medium for which
"check" applies in area mode (port: none for concave/cyl-concave-V there).

## Open questions
- Final pixel write in `sub_50_0000` (assumed: new hard mask, then the
  smoothed-edge blend); edge-smoothing blur kernel not decoded.
- Rubber/Move painting-mode scaling of the drag vector (`sub_14_086c`).
- `DFRM` "Deformation parameters" (Correlation/Depth) dialog is not part of
  Edit → Transformation (likely an effects filter).
