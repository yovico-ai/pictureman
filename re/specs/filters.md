# Processing filters (Smoothing … Emboss, hidden 127 / 250)

Port: `src/ops/filters.rs`.

## Entry and parameters

`seg67:0f8f` → `sub_43_23fc(id)`:

- IDs 0x6b, 0x7c–0x81, 0x84–0x86 open the Filter size dialog (`sub_43_2394`,
  dialog `SETARRAYSIZE`, proc `SETMATRICSIZEDLGPROC` at `seg43:215c`).
  - The size is reset to **3×3** every time the dialog opens (`23a8/23b0`).
  - The `lParam` of `DialogBoxParam` is the **maximum**, not the default.
    It is 15×15, or W/5×H/5 for Mosaic, Faceted glass and Scatter (0x84–0x86).
  - Both scroll bars have the range **2..max**. The vertical bar is inverted
    (pos = max − y + 2).
- All other IDs (122, 123, 130, 131, 250) skip the dialog and use a fixed
  **3×3** size (`24b4`).
- The chosen size is stored in DS:0x9436 (N, horizontal) and DS:0x9566
  (M, vertical). The command ID goes to DS:0xaf5c. Then
  `sub_22_0802(sub_43_0be6, 1, 1)` runs.

## Framework: `sub_43_0be6` (row streaming)

Variables: `fx`=bp‑0x56, `fy`=bp‑0x58. The ROI is x0=DS:9578, y0=DS:957a,
w=DS:957c, h=DS:957e (w and h are pixel counts). Image W and H come from
WAGETINFO +2/+4.

- It keeps `fy` row buffers (`ptr[]`=bp‑4), each `(W+fx+2)*3` bytes.
  Buffer index `i` holds source column `x0 − fx/2 + i`.
- Column fetch (`0f67`, `1100`):
  - `x_start = max(0, x0−fx/2)`
  - `x_end = min(x0+w+(fx−fx/2)+1, W−1)`
  - `sub_44_0088` copies `x_end−x_start` pixels.
  - The buffer is then padded on both sides by replicating the first and last
    fetched pixel.
  - **Quirk:** `x_end` is exclusive but clamped to W−1, so image column W−1
    is never read and acts as a copy of column W−2. Effective clamp:
    x ∈ [0, W−2].
- Row fetch: `sub_44_0088` clamps the row to [0, H−1] (`seg44:00a3`).
- Window for output (x,y) is columns `x−fx/2 … x−fx/2+fx−1` and rows
  `y−fy/2 … y−fy/2+fy−1`. For even sizes it extends further up/left.
- Per output row the code does:
  1. Rotate `ptr[]`.
  2. Fetch row `y+fy−fy/2−1`.
  3. Switch on the ID at `12de`.
  4. Write the result held in **`ptr[0]`**.

  The helpers `sub_43_xxxx(rows, fx, fy, ch, 0, w+1, tmp)` compute `w`
  outputs into `tmp`, then copy them into `ptr[0][j]`.
- Masking is done by `sub_57_3a32(ptr[0], ptr[fy/2]+3*(fx/2), y, w)`, which
  copies the original back outside the selection span. Writing back is done
  by `sub_44_0280`.
- Minor quirk: the loop runs rows y0..=min(y0+h, H−1), one more row than
  the ROI. The mask normally hides it.
- **Pixel order is BGR.** Emboss adds COLORREF bytes `[0x6e42]` (B) to
  channel 0 and `[0x6e40]` (R) to channel 2.
- Every op is per channel.

Switch (`12de`): 0x6b→1340, 0x7a→15b2, 0x7b→1807, 0x7c→`08ee`, 0x7d→`074c`,
0x7e→`081e`, 0x80→`05fc`, 0x81→`0348`, 0x82→`04d4`, 0x83→`025c`, 0x84→`01a6`,
0x85→`00f2`, 0x86→`0000` (after `srand(y)` via `sub_1_058c`), 0x88→`0b6c`
(Flip H via this framework: dead code, since menu 136 goes to `sub_35_0126`),
0xfa→1340, **0x7f→default (nothing)**.

Runtime helpers:

| Address | Function |
|---|---|
| `sub_1_0b06` | signed long divide (truncating) |
| `sub_1_0ba0` | long multiply |
| `sub_1_0c7e` | long arithmetic shift right (`sar`/`rcr` loop) |
| `sub_1_03f8` | `abs(int)` |

## Ops

### Smoothing — 107, `seg43:1340` — confidence high

```
out = (Σ window) / (N*M)            // long division, truncating; 16-bit column sums
```
Uses the dialog. Implemented as `smoothing(src, roi, FilterSize)`.

### Hidden 250 — `1338` → `1340` — confidence high

The same code as Smoothing, but with no dialog, so it is always 3×3.
Implemented as `smoothing_3x3`.

### Sharpening — 122, `seg43:15b2` — confidence high

Fixed 3×3. S = 3×3 sum including the centre, c = centre.
```
out = min(255, abs((17*c − S) / 8))     // truncating division, then abs()
```
Negative overshoot is mirrored by `abs()`, not clamped to 0.

### Heavy sharpening — 123, `seg43:1807` — confidence high

```
out = min(255, abs((35*c − 3*S) >> 3))  // arithmetic shift (floor), then abs()
```

### Spot removing — 124, `sub_43_08ee` — confidence high

A sliding byte histogram per row and channel, reset at j=0. `half = (N*M)/2`.
```
result = smallest v with cum(≤v) ≥ half   // the half-th smallest value, 1-based
```
- For 3×3 this is the 4th of 9, one below the true median.
- The incremental search (up from the previous median when `below < half`,
  down otherwise) gives the same result as the global definition.
- `hist[255]` is never cleared (stack garbage), but it can never change the
  result.
- Counts never exceed 225, so the byte counters cannot wrap.
- 1×1 would read garbage, but the dialog minimum is 2.

### Minimum — 125, `sub_43_074c` / Maximum — 126, `sub_43_081e` — confidence high

Per-channel min or max over N×M. The initial values are 255 and 0.

### Contour outlining — 130, `sub_43_04d4` — confidence high

Fixed 3×3.
```
out = clamp(2*c − min3x3, 0, 255)
```

### Emboss — 131, `sub_43_025c` — confidence high (code); intent unclear

Fixed 3×3 framework, using only the centre row (`rows[fy/2]`).
```
out[j] = clamp(buf[min(j+1, …)] − buf[max(j−1, 0)] + sysColor[ch], 0, 255)
```
- Buffer index j+1 is x and j−1 is x−2, so the result is
  `p(x) − p(x−2) + color`.
- At the first ROI column the result is `p(x0) − p(x0−1) + color`.
- `sysColor` is the "system colour" (DS:0x6e40 COLORREF, set by the Colour
  dialog). It is read from PMAN.INI `[COLOR] RED/GREEN/BLUE`. The code
  defaults are 0/0/255; the shipped INI has 0/166/166.
- The bias table is DS:0x3312/14/16 = B,G,R, set at `seg43:10a1`.

### Hidden 127 — confidence high (behaviour), purpose unknown

- `sub_43_23fc` lists 0x7f among the IDs that show the Filter size dialog.
- `sub_43_0be6` has no case for it, so the row written is the untouched
  `ptr[0]`. The output is the source shifted right by N/2 and down by M/2
  (edge-replicated).
- This is a stub of a removed filter. Nothing references the
  EXITATION or DFRM dialogs from this path.

Implemented as `hidden_127(src, roi, FilterSize)`.

## Shared with other ops (Hand drawing, Cleaning bg, Mosaic, Faceted, Scatter)

- They use the same framework, the same column clamp quirk, `ptr[0]` output
  and BGR order.
- The window convention is the same: buffer index `j..j+fx−1`, centre
  `j+fx/2`.
- Scatter calls `srand(y)` (`sub_1_058c`, argument = current row) before each
  channel.
