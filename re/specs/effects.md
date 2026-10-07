# Processing effects with "Filter size": Hand drawing, Cleaning background, Mosaic, Faceted glass, Scatter

Rust: `src/ops/effects.rs`.

## Entry, dialog, parameters

- `sub_43_23fc(id)` (seg43:23fc). For IDs 0x6b, 0x7c–0x81, 0x84–0x86 it calls
  `sub_43_2394(&size, maxX, maxY)`. If `[0x8fc8]` (painting-with-processing
  mode) is set, it skips the dialog and reuses the previous size.
- `maxX, maxY` = 15, 15. For Mosaic/Faceted glass/Scatter it is `(imgW/5, imgH/5)`
  (seg43:2480, uses WAGETINFO fields 1 and 2 = width, height).
  **These are the scroll-bar maxima, not the defaults.** `sub_43_2394` always
  starts the size at **3×3** (seg43:23a8).
- Dialog `SETARRAYSIZE` "Filter size" (proc `sub_43_215c`): static text 1001 shows
  `"%dx%d"`. Horizontal scroll bar 1002 = width, range [2, maxX]. Vertical scroll bar 1003 =
  height, range [2, maxY], shown inverted (pos = maxY − h + 2). Line/page up/down = ±1, thumb
  sets the value directly. OK = 1, Cancel = 2.
- The result goes to `[0x9436]`=sx and `[0x9566]`=sy, the command ID goes to `[0xaf5c]`, then
  `sub_22_0802(sub_43_0be6, 1, 1)`.

## Shared driver `sub_43_0be6` (sliding window)

- ROI: x0=`[0x9578]`, y0=`[0x957a]`, W=`[0x957c]`, H=`[0x957e]` (the selection bounding rectangle
  intersected with the image, `seg22:062a`). Rows y = y0..=min(y0+H, h−1).
- It keeps `sy` row buffers (each `(sx + imgW + 2)*3` bytes). Window row k is image row
  `y − sy/2 + k`. `sub_44_0088` clamps the row index to [0, h−1].
- Window column b is image column `x0 − sx/2 + b`. Loaded span:
  `srcStart = max(0, x0 − sx/2)` .. `srcEnd = min(w−1, x0 + W + (sx − sx/2) + 1)`, exclusive
  (`sub_44_0088` copies `count` pixels). Pixels outside the span replicate the first or last
  loaded pixel.
  **Quirk:** when the span reaches the right border, image column w−1 is never loaded.
  Column w−2 replaces it everywhere: in the filters and also in the "original" pixel that
  `sub_57_3a32` uses to restore unselected pixels inside the bounding rectangle. Reproduced by
  `Window::col`.
- Pixel bytes are **BGR**: channel 0 = B (the emboss coat colour table at DS:3312/3314/3316 is
  filled B, G, R from COLORREF `[0x6e40]`).
- Per-op routines have the signature `f(rows, sx, sy, ch, 0, W+1, tmp, [y])` and are called for
  ch = 0, 1, 2. Each computes `tmp[i]` for i in 0..W, then stores it into `rows[0][i]`. Writing
  to rows[0] is safe because that buffer is recycled for the next row. `sub_57_3a32` applies the
  selection mask (it takes `rows[sy/2]+sx/2` as the original) and `sub_44_0280` writes the row.
- Switch at seg43:12de: 0x80→1ba1→`sub_43_05fc`, 0x81→1c01→`sub_43_0348`,
  0x84→1d21→`sub_43_01a6`, 0x85→1d8d→`sub_43_00f2`, 0x86→1df5→`sub_43_0000`.
  (Others in the same switch: 0x6b/0xfa smoothing @1340, 0x7a @15b2, 0x7b @1807, 0x7c @1a81,
  0x7d/0x7e → 074c (min)/081e (max), 0x82 → 04d4, 0x83 → 025c, 0x88 → 0b6c.)

In the formulas below, X and y are absolute image coordinates, and every column and row goes
through the window clamp above.

## Hand drawing (128) — `sub_43_05fc` — confidence high
```
mn = min, mx = max over window cols X-sx/2 .. +sx-1, rows y-sy/2 .. +sy-1 (this channel)
c = centre (X, y);  half = mn >> 1
out = (half == mx) ? 255 : min(255, (u16)((c-half)*255) / (u16)(mx-half))
```
`half == mx` happens only for an all-zero window. Flat areas become white, and edges become
dark lines whose width grows with the size.

## Cleaning background (129) — `sub_43_0348` — confidence high
A sigma filter with a fixed threshold of 40:
```
sum:i16 = 0; n:i16 = 0
for v in window:  if (c-40 < v && c+40 > v) { sum += v (wrapping 16-bit); n++ }
out = n ? low_byte(sum / n  /* signed idiv, truncating */) : c
```
**The original has a 16-bit overflow:** if sum > 32767, it wraps. For example, a flat 15×15
area of 200 gives 165. This is reproduced. The centre always counts, so n ≥ 1.

## Mosaic (132) — `sub_43_01a6` — confidence high
```
row k = sy-1 - (sy/2 + y) % sy          -> image row y - sy/2 + k
col   = X - X%sx + sx - sx/2            (buffer: i - X%sx + sx, clamped to [0, W+sx])
out[X,y] = src[col, row]  (all channels)
```
- Tiles are anchored to absolute image coordinates. Horizontally, a tile starts where
  X % sx == 0. Vertically, a tile starts where (y + sy/2) % sy == 0.
- Sample position inside a tile: horizontal offset `sx − sx/2`, vertical offset
  `sy − sy/2 − 1`. The horizontal offset is one more than the vertical one; this asymmetry is in
  the code. With sx = 1, Mosaic shifts the image left by one pixel.
- The value is not an average, despite the manual's "various brightness".

## Faceted glass (133) — `sub_43_00f2` — confidence high
```
row k = (sy/2 + y) % sy                 -> image row y - sy/2 + k
col   = X + X%sx - sx/2                 (buffer: i + X%sx, clamped)
```
It uses the same tile grid as Mosaic. Inside each tile at offset t, the source is
`tile_start + 2t − half`: each tile shows its surroundings minified 2×, like a facet lens.

## Scatter (134) — `sub_43_0000` — confidence high
```
for each row y: for ch in B,G,R: srand(y)   (sub_1_058c; seed = (u16)y)
  for i in 0..W:
     t1=rand()%sx+i  (compared with lim=W+sx: always <=)  -> t2=rand()%sx+i (checked >= 0)
     -> t3=rand()%sx+i (compared with lim again) -> xx = t4 = rand()%sx+i
     yy = rand()%sy
     out = rows[yy][xx]  -> image (X - sx/2 + t4%sx, y - sy/2 + yy)
```
- The 5 rand() calls per pixel come from a double-evaluating `max(min(...),0)` macro.
- Calling srand(y) before each channel makes all channels pick the same source pixel.
- The result depends only on the row number. The global rand state afterwards is the state
  after the last row's third pass.

## Open questions
- `[0x957c]`/`[0x957e]`: the driver loops rows y0..=y0+H (clamped to h−1), so H may be the
  height − 1 or there may be one extra row (masked out anyway). The Rust code processes exactly
  `roi`.
- Single-column images (w = 1): srcEnd = 0, so nothing is loaded and the original would read
  zero-initialised memory. The port clamps to column 0 instead.
- The painting-mode path (`[0x8fc8]` != 0, output through `-0x24` buffer / `sub_57_3a32`) is not
  analysed.
