# Selection areas, smooth edges, magic wand, pen painting

Implementation: `src/selection.rs`. Addresses are `seg:off` in
the `nedis.py` listings (`segNNN.asm`); `[0xNNNN]` is a DGROUP global.

## Globals and INI keys (loaded in seg3:0c0a..0e53, 122b..)

| INI `[MODE]` | global | meaning |
|---|---|---|
| EDGE (0) | 0x6e34 | 0 Sharp, 1 low, 2 medium, 3 high (IDs 149..152, seg67:148e) |
| FRAGMENT | 0x519a | area: 0 whole, 1 rect, 2 ellipse, 3 polygon, 4 text, 5 freehand, 6 magic wand, 7 pen |
| PENSIZE (1) | 0x92f8 | 1,3,5,7,9,11 (IDs 212,213,214,215,249,216) |
| BRUSH (0) | 0x93f0 | 0 square (218), 1 circle (217); 2..5 INI-only, see below |
| TOLERANCE (code default 30) | 0x83ca | wand, 0..100 |
| UNIFOLD (1) | 0x709c | wand: 1 = contiguous |
| RGBMATCH (1) | 0x51a0 | wand: 1 RGB, 0 HSV |
| AIRBRUSH (16000) | 0x956a | hidden IDs 238/239/240 set 16000/8000/4000 |
| FADING (3) | 0x715c | fade step per mouse-move dab |
| RANDOM (0) | 0x6e32 | hidden ID 274 toggles; reseed fluctuated fill per dab |
| ANIMATE (1) | 0x51ca | marching ants (timer 0x521, 200 ms) |
| BACKUP (0) | 0xa164 | "Create backup" (138) |
| KEEPMASK (0) | 0xa1d8 | "Preserve mask" (232) |
| PAINTICON | 0xa400 | View/Paint on icon = draw image in the minimised icon. Unrelated to the pen. |

Other state: 0x12da = inside(1)/outside(0) choice; 0x81e8 = feather width W;
0x81e4 = stroke fading counter; 0x8fc8 = painting mode active; 0x51a2 =
"first dab, make backup"; 0x9578/0x957a/0x957c/0x957e = processing box
x/y/w/h; mask DIBs 0x61e4 / 0x7168 (1 bpp, drawn through a DIB.DRV DC).

## 1. Area masks (seg22 rasterisers; confidence high on primitives, ±1 px on borders)

The area is always a **binary** 1-bpp bitmap drawn with GDI. If inside was
chosen it is drawn as a white shape (white brush + white pen) on black.
Otherwise it is drawn as a black shape on white. White means selected.
- Rect `sub_22_00ca`: `Rectangle` → half-open `[x0,x1)×[y0,y1)`, outline included.
- Ellipse `sub_22_01ec`: `Ellipse(cx-rx, cy-ry, cx+rx, cy+ry)`.
- Polygon `sub_22_0322`: `SetPolyFillMode(ALTERNATE)` + `Polygon` with a 1-px outline (even-odd fill).
- Freehand `sub_10_1b20`: paint with the brush, one dab per mouse position
  with no interpolation (seg10:0f8e.. uses the same Rectangle/Ellipse dab
  as the pen). The right button erases. On entry, if a previous area exists
  (prev type 0x512e ≠ 0, 7), the previous mask is the starting point.
  If **KEEPMASK** is on, the previous geometry is re-rasterised
  (switch seg10:1cd8: rect, ellipse, polygon, text, line). If it is off,
  the current (possibly moved/transformed) mask bitmap is used.
- Text `sub_20_*`: GDI text glyphs. Not implemented. Supply a mask.
- Line/stroke `sub_22_0484`: wide pen (`CreatePen(PS_SOLID, PENSIZE, white)`) between two points.

The double-click (WM_LBUTTONDBLCLK 0x203) reads the mask bit under the
cursor (`sub_10_0e94`/`sub_10_0120`). If it is set, 0x12da = 1 (process
the interior); otherwise 0x12da = 0.

## 2. Edge softening (sub_57_3a32 + seg32 helpers; confidence high)

Every operation writes each processed row through `sub_57_3a32(out_row,
orig_row, y)`. `orig_row` is the operation's source row.

1. `sub_32_0000(y)` turns the mask row into a boundary list at 0x8fd0.
   This step is cached per row.
2. Sharp edge or inside mode: unselected spans are restored from `orig`. This is a binary mask.
3. If EDGE ≠ 0 (all area types except whole image and pen), for each x:
   ```
   a = T[ramp(dh)]          // sub_32_04c6: horizontal run of the *shape*
   b = T[ramp(dv)]          // sub_32_0414 (+ column cache sub_32_02c6/sub_31_1c70)
   e = ((a/4) * b) >> 8      // 0..64 "inside-ness"
   inside : out = (proc*e + orig*(64-e)) >> 6
   outside: out = (orig*e + proc*(64-e)) >> 6
   ramp(d) = W==0 ? 0 : d>=W ? 255 : (d<<8)/W
   dh = min(x - s', e' - x)   over the run [s,e] of shape pixels containing x
   ```
   - Inside mode: `s' = s`, `e' = e`. The exception is a run that reaches
     the right or bottom image border, which ends at W or H, one past the
     border.
   - Outside mode: the run is widened by 1 (`s-1`, `e+1`). Horizontally
     `s-1` is clamped to 0; vertically it is not.
   - `T` (DS:0xa1e0, `sub_3_1e2e`): `T[i] = (int)(256*sin(i*0.00613591796875))` (≈π/512), with `T[255] = 256`.
   - The result is a separable quarter-sine ramp, product of the horizontal and
     vertical ramps. It always lies **inside the drawn shape**, and the
     outermost shape pixels get e = 0.
   - **W = 0 with a smooth edge gives e = 0 everywhere, so the operation
     has no effect.** This is faithful to the original; it happens, for
     example, with a rectangle under 20 px and Smooth low.
4. Feather width W (0x81e8), with integer division:

| area | low | medium | high | where |
|---|---|---|---|---|
| Rect | min(w,h)/20 | /8 | /4 | seg57:3431 |
| Ellipse | 2·min(rx,ry)/20 | /8 | /4 | seg30:2108 |
| Polygon | min(bbox w,h)/20 | /8 | /4 | seg31:1b22 |
| Freehand | bbox **w**/20 | /8 | /4 | seg10:225b |
| Text | 1 | 2 | 4 (alt path 1,2,3) | seg20:1176 / 172a |
| Magic wand | 2 | 4 | 6 | seg9:1677 |
| Pen | 2 | 4 | 5 (only ≠0 matters) | seg67:1e3d |

## 3. Magic wand (sub_9_0474; dialog WANGDLGPROC seg9:0970; confidence high)

- The seed is the pixel under the click (`[0x81e2]`, `[0x81fc]`). Every pixel of the image is classified:
  - RGB: `|c_i - seed_i| < tol` for all 3 channels (8-bit).
  - HSV: inputs are `c>>2` (6-bit). Match if `|S-S0| < 2·tol && huedist(H0,H) < tol && |V-V0| < 2·tol`.
  - `sub_59_01e0`: `V = max*4`, `S = (max-min)*255/max`. The hue uses 256 units per sextant over 0..1536. If channel 0 is max, `H = bc-gc`; if channel 1 is max, `H = rc-bc+512`; otherwise `H = gc-rc+1024`, with `xc = (max-x)*255/(max-min)`. Negative values get +1536. Grey has `H = 0`.
  - `sub_9_0000`: `huedist = min(d, 1536-d)/6`.
  - The test is strict `<`. Channel order does not matter for any test.
- **Unifold = 1** (seg9:08a5) runs `FloodFill(seed, border=black)` on the
  1-bpp match bitmap, then `StretchDIBits(... SRCERASE)` against a copy.
  The result is the **4-connected** component of the seed. Unifold = 0
  selects every matching pixel in the image, connected or not.
- Tolerance: scroll range 0..100, line step 1, page step 15. OK clamps it to ≥ 1.
- `sub_9_0071` builds the contour for the marching ants. A selected pixel is
  on the contour if it has an unselected 8-neighbour or touches the image border.
- Each click re-runs the wand at the new seed. The double-click then decides inside or outside.

## 4. Painting with any operation (Pen, area 7; confidence high on structure, medium on 1-px details)

- When an operation is chosen with area = Pen, `sub_57_37be` sets 0x8fc8 = 1
  (painting mode) and 0x51a2 = 1. The operation's dialog is shown once.
- Every WM_MOUSEMOVE with the left button down (seg67:1d97):
  `fade -= FADING (min 1)`. The dab box is `[X - s/2, +s)²`, clipped to
  the image. The dab is drawn into the mask (`sub_22_062a`). Then
  **`SendMessage(WM_COMMAND, last_command)`** (seg67:1e87) re-runs the
  operation with the dab box as the processing rectangle. The operation is
  **recomputed per dab**; it is not revealed from a precomputed image.
  There is no interpolation between mouse positions.
- Button down (seg67:20ad): `[0x5196] = rand()` after `srand(time)`, and
  `fade = 256`. Then one dab.
- Source for each dab (`sub_22_0802`, painting branch):
  - Filters (callback flag = 1): the backup is taken at the first dab of
    the painting session. Every dab is then computed **from the backup**
    into the image, so effects do not compound.
  - In-place ops such as fills: these run on the image itself, so opacity accumulates.
- Row commit for the pen (`sub_57_3a32`, BRUSH switch seg57:40d5), applied in order:
  1. BRUSH effects over the row's first..last dab pixel:
     - 2 Airbrush: if `rand() > AIRBRUSH`, keep the source. Pixels are painted with probability (A+1)/32768, so ≈49 % at 16000.
     - 3 Fading opacity: `out = (out*t + src*(64-t))>>6`, where `t = max(3, (((fade/4)²/64)²)/64)`.
     - 5 Fading spray: if `rand()%64 > t`, keep the source.
     - 4 Fading size: the ellipse diameter is `max(1,(s*fade+128)>>8)` (seg22:0775). The box stays s×s.
  2. Pixels of the box outside the dab shape are reset to the source. With
     filters this also reverts earlier dabs in a circle brush's corners,
     an apparent original quirk.
  3. If EDGE ≠ 0, the edge becomes **transparency**:
     - Pen size ≤ 3: `e = 32/16/8` for low/medium/high.
     - Pen size > 3: `e = rim/{2,4,8}`, with rim = 32 on the dab's outer ring
       (`sub_32_0608`: top/bottom row; left/right column for the square;
       first/last pixel of the row for the circle) and 64 elsewhere.
     - Then `out = (out*e + src*(64-e))>>6`.
- Size 1: the original draws a 2×2 dab clipped by a 2×2 box offset by one pixel. It is treated as 1 px (uncertain).
- RANDOM: in painting mode the fluctuated fill (`sub_29_0ad0`) does `srand([0x5196])`
  before each dab when RANDOM = 1. Every dab then reuses the stroke's noise
  sequence. When RANDOM = 0 the sequence continues.
- Fluctuated ("natural") colour is the Fill/Color/Fluctuated op (282, seg29,
  INI `[RANDOM]` DEVX/DEVY/BAND/SCALE/DEPTH). It is painted like any op. That is
  outside this file.
- Right button while painting (seg67:21aa) requires BACKUP and an existing
  backup. It copies the backup back through the dab (`sub_19_00da` →
  `sub_57_3a32`), so it erases with the same brush.

## 5. Backup / Undo (sub_22_0802, sub_38_02ce, sub_38_0000)

- BACKUP on: the operation is run as `callback(src=image, dst=backup)` and
  the two are then swapped (`sub_38_0000`). The backup therefore holds the
  pre-command image.
  - Undo (121) is one level only.
  - Erase (162) restores the backup inside a newly selected area. It can be
    repeated, and the right button does it while painting.
  - Esc can cancel a running operation.
- BACKUP off: operations run in place, and Undo and Erase are disabled.
- Painting: one backup per painting session, taken at the first dab, so
  Undo reverts every stroke made since the operation was chosen.

## 6. UI interaction model

1. Choose the **area type** and **edge** first. Use the toolbox or Options/Area and
   Options/Edge; the area stays selected until changed.
2. Choose the **operation** (Edit menu), then fill in its parameter dialog if it has one.
3. Whole image: processing starts at once. Otherwise an area-specific
   cursor appears and the area is entered:
   - **Rect / Ellipse:**
     - Drag from corner to corner.
     - Drag corners or sides to resize.
     - Drag the interior to move (palm cursor).
   - **Polygon:**
     - Click each vertex. Press, drag and release places the vertex at the release point.
     - Shift+click removes the last vertex; removing the first one moves it to the cursor.
     - Double-click closes the polygon.
     - Edit by dragging a vertex (cross cursor), Ctrl+click on a side to add a vertex, or drag the interior to move.
   - **Freehand:** paint with the brush (pen size and shape). The right button erases.
   - **Text:** click the baseline-left point, fill in the font/text dialog and press OK, then drag the text into place.
   - **Magic wand:** click a reference point. Click again to re-pick.
4. **Double-click inside** the area to process the interior, or outside it
   to process the exterior. For text, the test uses the bounding rectangle.
   For transformations, the user then adjusts the geometry and clicks inside
   to start.
5. **Esc**, or a click outside the window and toolbox, cancels the area input.
6. Repeating a command with the same area type shows the previous contour.
   Double-click to accept it, or draw a new area. Shift+click edits the
   previous area (not for the magic wand).
7. **Pen**: after the operation is chosen, a pen cursor appears.
   - Left-drag paints the operation.
   - Right-drag erases (restores the backup).
   - Painting mode lasts until the area type changes or a new image is loaded.
   - Smooth edge = pen transparency.
8. Contours are drawn with an XOR-type raster op. `sub_57_37be` uses
   `SetROP2(10)` = R2_NOTXORPEN with a dotted pen; freehand and the magic
   wand use R2_NOT. With ANIMATE on, the contour is redrawn on a 200 ms timer
   to make the marching ants.

## Open questions

- Vertical ±1 px conventions: the image and mask DIBs are bottom-up and drawn with `h - y`.
- Whether choosing a new menu command while in painting mode ends the session or starts a new backup. (0x142 and `sub_67_0000` accumulate the stroke's dirty rectangle.)
- `sub_57_3520` (pen session start) uses a different W table by pen size: for size ≤ 5 with a low edge it is W = 0, so there is no rim on the first dab. The code uses the mouse-move rule.
- GDI `FloodFill` connectivity is assumed to be 4-connected, as in the Win3.x and Wine scanline fill.
- The exact GDI ellipse pixelisation for small dabs.
