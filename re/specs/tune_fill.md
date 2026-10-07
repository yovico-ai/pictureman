# Tune & Fill — recovered algorithms (PMAN.EXE 1.55)

Port: `src/ops/tune.rs`, `src/ops/fill.rs`.

Conventions used by every op below:
* Work-area pixels are **BGR** (byte0 = B, byte2 = R).
* Work-area rows are **bottom-up** (DIB). The evidence: the Gradient dialog labels colour #1 "bottom" for Vertical, and colour #1 is the LUT entry for row offset 0. Every row-dependent formula below uses bottom-up row numbers `yb`. The Rust code converts with `yb = H-1-y`.
* ROI = DS:0x9578/0x957a/0x957c/0x957e (x, yb, w, h), the selection's bounding box. Results are committed through the selection mask (`sub_57_3a32` + `sub_44_0280`). The caller does that with `blend_through_mask`.
* Area type DS:0x519a: 0 Whole, 1 Rect, 2 Ellipse, 3 Polygon, 4 Text, 6 Magic wand, 7 Pen. Pen shape DS:0x93f0: 1 = Circle (cmd 217), 0 = Square (218).
* C integer division truncates toward zero. `sub_1_0b06` = `_aFldiv`, `sub_1_0ba0` = `_aFlmul`, `sub_1_0f0e` = ftol (truncation), `sub_1_0ec2` = `pow`, `sub_1_0eb6` = `sqrt`, `sub_1_0ed3` = `exp`, `sub_1_0f4c` = fmax(st0, st1), `sub_1_03f8` = `abs`.
* Several stats/apply loops run to `row <= min(top+h, H-1)`, which includes one row past the ROI. That row lies outside the mask, so it has no visible effect and the port ignores it.

## System colour (Options/Color)
* COLORREF DS:0x6e40 (low byte = R). INI `[COLOR] RED=0 GREEN=0 BLUE=255` gives a default of **blue** (`seg3:0cfc`).
* Select (cmd 103): COMMDLG `ChooseColor` with the custom CHOOSECOLOR template and hook `COLORHOOK` (`sub_2_007d`). DS:0x8f30 = 1 while it is open.
* Pick up (cmd 277) was not traced.
* Gradient colours come from INI `LEFTRED/LEFTGREEN/LEFTBLUE` → DS:0x9342/0x7200/0x451c (colour #1) and `RIGHT*` → DS:0x7166/0x51c0/0xa1d0 (colour #2). All default to 100. `RAINBOW` (DS:0x518e) is not used by these ops.

## RGB control — TV (104) and Linear (105) — confidence HIGH
* Entry: `sub_58_0258` (COMMON dialog, `COMMONDLGPROC` seg58:0000) and `sub_59_0b8c` (RGB dialog, `PROCRGBDLGPROC` seg59:0562).
* Tables: `sub_59_0000`. Apply callback: `seg49:0000` → `sub_59_040c` for each pixel.
* There are five linear tables `T(i) = clamp(i*k/50 + off, 0, 255)`:
  * halftone: k DS:0x81f6, off DS:0x6e46
  * color: DS:0x7162, DS:0x61dc
  * red: DS:0xa3e2, DS:0x61d2
  * green: DS:0x70a2, DS:0x5198
  * blue: DS:0x7174, DS:0x7220
* Defaults are k=50, off=0. Both dialogs reset to these defaults every time they open (`sub_59_0c2c`), and Cancel resets them too.
* Pixel routine (`sub_59_040c`):
  ```
  r6,g6,b6 = R>>2, G>>2, B>>2
  (H,S,V) = rgb2hsv(r6,g6,b6)        # sub_59_01e0
      V = max*4
      S = max ? (max-min)*255/max : 0
      H = 0 if S == 0, else
          rc,gc,bc = (max-c)*255/(max-min)
          H = (max==R)? bc-gc : (max==G)? rc-bc+0x200 : gc-rc+0x400
          H += 0x600 if H < 0
  V = Thalftone[V]; S = Tcolor[S]
  (r,g,b) = hsv2rgb(H,S,V)           # sub_59_02c4
      if S == 0: r = g = b = V
      else:
          H = 0 if H == 0x600
          v4 = V>>2; f = H&255
          p = (v4*(256-S)+128)>>8
          q = ((256-((f*S)>>8))*v4+128)>>8
          t = ((256-(((255-f)*S)>>8))*v4+128)>>8
          by sector H>>8:
              0 (V,4t,4p)   1 (4q,V,4p)   2 (4p,V,4t)
              3 (4p,4q,V)   4 (4t,4p,V)   5 (V,4p,4q)
  out = (Tr[r], Tg[g], Tb[b])
  ```
  An identity map still quantises the image to 6 bits. This is the original behaviour.
* TV dialog. It has three vertical scrollbars with line step 1, page step 10, and live preview through the display palette (`sub_69_0290`):
  * contrast (id 712): range 0..100, default 50, `k_halftone = 100 - pos`
  * brightness (id 710): range −255..255, default 0, `off_halftone = -pos`
  * "RGB" icon (id 711): range −255..255, default 0, `off_color = -pos` (saturation shift)
* Linear dialog. It has five `transfn` graph controls (Halftone, Color, Red, Green, Blue; window proc `TRANSFNWNDFN` seg40:00c4):
  * The user drags the end points of a line in a 255×255 box. Keyboard arrows move a point by 1/10 of the box.
  * The control sends WM_COMMAND with `lParam = (offset << 16) | slope%`. The dialog stores `k = slope%/2`.
  * Labels show `k*0.02` ("%3.1f") and the offset ("%5d").
  * The "Restore" button (616) resets the curves.
  * Preview is live through the palette.

## Gamma correction (143) — confidence HIGH
* Code: `sub_33_0a76`, dialog `PROCGAMMADLGPROC` seg33:07ba, apply `sub_33_0000`.
* Slider (id 1501, range 0..255):
  * `v = pos*0.023529412f`
  * if v > 3: `g = v-2`, otherwise `g = 1/(4-v)` (range 0.25..4.0)
  * Line step ±4, page step ±15. Label "%3.2f".
* The dialog opens with g = 1.0 exactly (thumb at 127).
* Check boxes R/G/B (603–605, DS:0xfac/0xfae/0xfb0) default to on and persist between runs.
* LUT: `trunc(pow(i/255, g)*255 + 0.49)`, saturating at 255. It is applied only to the checked channels.
* Hidden commands 251 and 252 (`sub_33_0bd0`) apply g = 0.7 and g = 1.4 to all channels and set all three boxes on.

## Expand (154) — confidence HIGH
* Code: `sub_28_07ea` → `sub_28_0072`. There is no dialog.
* Statistic over the area:
  * Whole: `L = (4G + 2B + R + 3)/7`
  * any selection, including Pen: `max(R,G,B)`, counted over mask pixels only. Pen counts all ROI pixels.
* LUT: `clamp((i-min)*255/(max-min))`, or identity when max == min. The same LUT is applied to R, G and B.

## Equalization (144) — confidence HIGH
* Code: `sub_34_073a` → `sub_34_0000`.
* Histogram of `L = (4G+2B+R+3)/7` over the area. The mask is used except for Whole and Pen.
* N = number of pixels, or 1 if there are none.
* LUT: `lut[0] = 0`, and `lut[i] = min(255, (Σ_{j<i} hist[j] << 8) / N)`, followed by a monotonic fix-up.
* The LUT is applied to each of R, G and B.

## Fill/Color/Plain (153) — confidence HIGH
* Code: `sub_29_060e` → `sub_29_02ea`. Every ROI pixel becomes the system colour.

## Fill/Color/Fluctuated (282) — confidence HIGH (formula), MEDIUM (rand state)
* Code: `sub_29_0ad0` → dialog `sub_86_0000` (EXITATION, `EXITATIONDLGPROC`) → apply `sub_29_0642`. The noise comes from seg87.
* Parameters:
  * Grain: scrollbar 1409, range 1..16, default 3, line 1 / page 4, label "%dx%d". It sets both DS:0x11e8 and 0x11ea.
  * Depth: scrollbar 1907, range 1..100, default 50 (DS:0x11ec).
* Set-up (`sub_87_0026(n = roi_w+10, A8 = depth*8, gx, gy)`):
  ```
  a = trunc(64*exp(-1/max(gx-1, 0.1)))     # likewise b from gy
  amp = trunc(sqrt((1-b²/4096)*(1-a²/4096)) * A8)
  ```
* Rows (`sub_87_02bc`) use 16-bit wrap-around arithmetic. Buffers start at zero.
  ```
  cur[0] = U
  (discard one U for the unused 2nd field)
  for i in 1..n:  cur[i] = cur[i-1]*a/64 + U
  for i in 1..n:  cur[i] += prev[i]*b/64
  swap(cur, prev)
  ```
  Here `U = rand() % (2*amp+1) - amp`, which is `sub_87_0000`.
* Four warm-up rows are generated first. Then one row per ROI row, bottom-up.
* Pixel at ROI column i:
  ```
  d = MulDiv(row[i+10], max(R,G,B of colour), 255) / 8
  channel = clamp(c + d)
  ```
* rand() call count: (4 + rows) × (w + 11).
* There is no srand on the normal path. The path where DS:0x8fc8 is already set and INI `[MODE] RANDOM` (DS:0x6e32, default 0) is set calls `srand(DS:0x5196)` (`seg29:0af8`; the same pattern appears in Gradient) — open question.

## Fill/Gradient Vertical/Horizontal/Radial (221–223) — confidence HIGH
* Code: `sub_29_1648` (dialog `GRADIENTDLGPROC`) → apply `sub_29_0b20`.
* Dialog colour wells: colour #1 is PWCOLOR 1602, colour #2 is 1601.
  * Clicking a well opens ChooseColor seeded with that colour.
  * If ChooseColor is already open (DS:0x8f30), clicking takes the current system colour instead.
  * Labels: Vertical "bottom"/"top", Horizontal "left"/"right", Radial "center"/"border".
* LUT: `((255-t)*c1 + t*c2)/255` per channel.
* t values (all relative to the ROI):
  * Vertical: `t = yb*255/h`
  * Horizontal: `t = x*255/w`
  * Radial: `t = clamp(trunc(dist)*255/R, 0, 255)` with `dy = yb - h/2`, `dx = x - w/2` (C halves):
    * Ellipse area: `dist = sqrt(dy²·k2 + dx²)` where `k2 = f32(f32(w/h)²)`, and `R = max(w/2,1)`
    * Pen + circle: Euclidean distance, `R = max(h/2, w/2, 1)`
    * otherwise: Euclidean distance, `R = max(trunc(sqrt((w²+h²)/4)), 1)`

## Fill/Pattern Tiled/Scaled/Fitted (224/233/234) — confidence HIGH (Tiled/Scaled), MEDIUM-HIGH (Fitted)
* The pattern is an image file chosen with the standard Open dialog (`sub_11_1624` → `sub_23_0151` loader). It is loaded fresh each time and freed after the fill.
* Tiled (`sub_11_0000`): `pat[(x mod pw), (yb mod ph)]`. The tiling is anchored at the image origin (bottom-left), not at the ROI.
* Scaled (`sub_11_0498`) stretches the pattern over the **whole image** W×H:
  ```
  sy = ph*yb/H
  fy = ((ph*yb)<<4)/H & 15
  sx = min(pw*x/W, pw)
  fx = ((pw*x)<<4)/W & 15
  out = (w00*r0[sx] + w10*r0[sx+1] + w01*r1[sx] + w11*r1[sx+1]) >> 8
  ```
  * Weights are products of (16−f) and f.
  * r0 is pattern row sy and r1 is row sy+1. Each row buffer gets its last pixel duplicated.
  * Row cache: on `sy == prev+1`, r0 = r1 and r1 is reloaded only if sy+1 < ph; otherwise both rows are loaded. Initial buffers are uninitialised in PMAN (zero in the port).
* Fitted (`sub_11_0d1c`): the same, applied to a centred crop window of the pattern that has the image's aspect ratio:
  * if `ph*W > pw*H`: `winh = pw*H/W`, `offy = (ph-winh)/2`
  * else: `winw = ph*W/H`, `offx = (pw-winw)/2`
  * Then `sy = winh*yb/H + offy` and `sx = winw*x/W + offx`.
* None of the three has parameters.

## Fill/Patch Full/Horizontal/Vertical (155–157) — confidence MEDIUM-HIGH
Code: `sub_27_0e46` → apply `sub_27_09a9` → per-pixel `sub_27_0000`. Columns are set up in `sub_27_0694` and rescanned in `sub_32_02c6`; row spans come from `sub_32_0000`. The input is the 1-bpp selection bitmap (DS:0x61e4) plus the ROI. With Area/Whole, PMAN temporarily builds a sharp rectangle selection over the ROI.

Row spans (`sub_32_0000`):
* Spans are the selected runs of row yb inside the ROI columns, `(xl, xr)` inclusive.
* A run still open at the ROI's right end gets xr = W.

Column cache, per ROI column:
* `T[x]` and `B[x]` are the first vertical run, scanning bottom-up from row 0.
  * The 0→1 transition gives T = yy.
  * The 1→0 transition gives B = yy−1.
  * If the scan ends at yy ≥ H, then B = H (and T = H if there was no transition). The scan loop increments yy after the second transition, so a run whose end transition is on row H−1 also gets B = H.
* `top[x]` and `bot[x]` are the source pixels at rows T and B.
* When `B[x] < yb`, the column is rescanned starting at row yb, and the colours are reloaded from the source image.

Per pixel (rows bottom-up, x ascending, over all ROI columns):
```
find the first span with xr >= x
    if xr == xl or xl >= x: keep pixel
    else: L = xl, R = xr
if no span is found: L = roi_x0, R = roi_x1 - 1
refresh the column if B[x] < yb
if B == T or T >= yb: keep pixel
a = src[L], b = src[R]                      (same row)
H = ((x-L)*b + (R-x)*a) / max(R-L, 1)
V = (top*(B-yb) + bot*(yb-T)) / max(B-T, 1)
Full:
    wh = max(R-L,1)/2 - |x - (L+R)/2|       (1 if <= 0)
    wv = max(B-T,1)/2 - |yb - (T+B)/2|      (1 if <= 0)
    out = (wv*H + wh*V) / (wh+wv)
```
Consequences:
* The run ends themselves (left end and bottom end) stay unchanged.
* A 1-row selection is a no-op.
* Each pixel's horizontal ends come from its own row run. Its vertical ends come from the column run, which for non-convex shapes may be stale or partial — that is faithful.

Why the confidence is not higher:
* Reads past the buffers when R = W or B = H. PMAN reads into the next buffer or a failed row read. The port clamps to the last column/row.
* The inverted-selection flag DS:0x12da (start state of the rescan) is assumed to be normal.
* It is unverified whether the ROI is exactly the bounding box. If it is, every run touching the right edge of the bbox gets R = W.

## Open questions
1. Is the ROI exactly the mask bounding box, or one pixel larger? This affects the Patch R = W case.
2. Patch with Area/Whole: what does the rectangle polygon from `sub_22_00ca` actually cover?
3. When is `srand(DS:0x5196)` used? It applies to the DS:0x8fc8 path plus INI `[MODE] RANDOM` (DS:0x6e32), probably for repeating a recorded command.
4. Pick up (cmd 277) was not traced.
