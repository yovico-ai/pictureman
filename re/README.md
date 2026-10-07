# Picture Man 1.55 — reverse-engineering notes

PMAN.EXE is a 16-bit Windows 3.x NE executable, compiled with Microsoft C
(large model, plain C, no C++). No source code survives; everything in the
rebuild was recovered from the binaries with the tools here.

The specs describe the *original* behaviour, including its bugs. The
rebuild fixes those; see "Fixed bugs of the original" in `../README.md`.

## Layout

- `specs/` – the recovered algorithms, one file per area, with original
  addresses, pseudocode and confidence levels. `buffer_overruns.md` audits
  the original for fixed-size buffer overflows.
- `resources/` – the menu tree with command IDs (`menu.txt`) and the dialog
  and string resources (`dialogs.txt`, `strings.txt`), extracted from
  PMAN.EXE. The bitmaps, icons and cursors are in `../assets/`.
- `manual-1.55.txt` – text of the original user's manual (PMAN.WRI).
- `tools/` – the Python tools used (needs `pip install capstone pillow`):
  - `nedis.py PMAN.EXE out/` – NE disassembler: one listing per code
    segment, relocations resolved (imports by name, internal far calls as
    `sub_<seg>_<off>`), WIN87EM floating-point emulator calls turned back
    into x87 instructions, switch tables decoded.
  - `xref.py out/` – callee ← callers and imported API ← callers.
  - `res.py PMAN.EXE out/` – resources: bitmaps/icons/cursors as PNG,
    dialogs, strings. `menu.py PMAN.EXE` – the menu tree with IDs.
  - `ne.py` – NE header, segment and import summary.

  For import names `nedis.py` reads Wine's Win16 `.spec` files from
  `tools/spec/` (not included; get `krnl386.exe16.spec`, `gdi.exe16.spec`,
  `user.exe16.spec`, `commdlg.dll16.spec` and `win87em.dll16.spec` from
  Wine's `dlls/*/` directories). Exports of other DLLs are read from the
  DLL files next to the input.

The original binaries and the full listings are not part of this
repository. Addresses in the notes refer to `segNNN.asm` listings that
`nedis.py` produces from the PMAN.EXE of the 1.55 shareware release.

## Reading the disassembly

- Far calls are shown as `lcall sub_<seg>_<off>` (target = segment `seg`,
  offset `off`; open `pman/seg<seg>.asm` and search `^<off>:`), or
  `lcall MODULE.Function` for imports.
- Exported far functions start with `mov ax, ds; nop; inc bp; push bp;
  mov bp, sp; push ds; mov ds, ax` and end with `pop ds; pop bp; dec bp;
  retf`. Internal far functions: `inc bp; push bp; mov bp, sp; push ds`.
- Args: Pascal convention for exported/Windows callbacks (args pushed
  left-to-right, callee pops); cdecl for internal C functions (args pushed
  right-to-left, first arg at `[bp+6]` for far functions, caller pops
  with `add sp, N`).
- `push 0xNNNN ; SEGnn (far ptr segment...)` followed by `push 0xOOOO`
  is a far function pointer to `sub_nn_OOOO` (used for callbacks).
- Floating point: the compiler emitted WIN87EM emulator calls
  (`int 34h`–`3Dh`). The disassembler rewrites them back to real x87
  instructions (`fld`, `fmul`, ...), preceded by a `wait`.
- Long arithmetic helpers from the C runtime live in segment 1:
  `sub_1_0ba0` = 32-bit multiply (`_aFlmul`), `sub_1_05a4` = `rand()`.
  Identify others by their bodies (`_aFldiv`, `_aFlshl`, `_aFulrem`, ...).
- `rand()` is the MS C LCG: `seed = seed*214013 + 2531011; return
  (seed>>16)&0x7fff` (seed in DS:0x211a). `MsRand` in `src/core.rs`
  reproduces it.
- Image memory is accessed through MWAREA (`WAGETINFO`, `WABANDLOCK`,
  `WABANDADDRESS`, `WAGETPIXEL`, ...): images are stored as bands of rows;
  each pixel is 3 bytes (check the order — BGR like a DIB, or RGB).

## Command dispatch

`MAINWNDPROC` (seg 67) handles WM_COMMAND at `seg67:08b3`, with the main
switch at `seg67:0bc7` (table at `0bcc`, IDs 0x65..0x11a). Command → handler:

| ID | Menu | Handler |
|----|------|---------|
| 102 | Transformation/Size | seg67:0df3 |
| 104 | Tune/RGB control/TV | seg67:0f20 |
| 105 | Tune/RGB control/Linear | seg67:0f7b |
| 107 | Processing/Smoothing | seg67:0f8f → `sub_43_23fc(id)` |
| 108 | Transformation/Clip | seg67:0f9a |
| 122–134 | Processing/* (Sharpening 122, Heavy sharpening 123, Spot removing 124, Minimum 125, Maximum 126, (hidden 127), Hand drawing 128, Cleaning background 129, Contour outlining 130, Emboss 131, Mosaic 132, Faceted glass 133, Scatter 134) | seg67:0f8f → `sub_43_23fc(id)` |
| 250 | (hidden, not in menu) | seg67:0f8f → `sub_43_23fc(id)` |
| 136 / 140 | Flip Horizontal / Vertical | seg67:12b4 / 13ad |
| 137 | Rotate | seg67:12be |
| 141 | Deformations | seg67:13b2 |
| 143 | Gamma correction | seg67:1430 |
| 144 | Equalization | seg67:1438 |
| 145–148,158,160,163 | Area: Whole/Rect/Ellipse/Polygon/Text/Magic wand/Pen | seg67:1440.. |
| 149–152 | Edge: Sharp / Smooth low / medium / high | seg67:148e.. |
| 153 | Fill/Color/Plain | seg67:14c4 |
| 154 | Tune/Expand | seg67:14cc |
| 155–157 | Fill/Patch Full/Horizontal/Vertical | seg67:14d4 |
| 167 | Transformation/Move | seg67:1550 |
| 221–223 | Fill/Gradient Vertical/Horizontal/Radial | seg67:1927 |
| 224, 233, 234 | Fill/Pattern Tiled/Scaled/Fitted | seg67:1932 |
| 228 | Transformation/Rubber | seg67:1951 |
| 231 | Options/Magic wand... | seg67:1959 |
| 282 | Fill/Color/Fluctuated | seg67:1b25 |

`sub_43_23fc(id)` (processing entry): for IDs 0x6b, 0x7c–0x81, 0x84–0x86
it first asks for a filter size via `sub_43_2394` (the SETARRAYSIZE "Filter
size" dialog; always opens at 3x3, range 2..15 — or 2..width/5,height/5 for
Mosaic/Faceted glass/Scatter), stores size in DS:0x9436 (x) / DS:0x9566 (y) and the command
ID in DS:0xaf5c, then calls `sub_22_0802(callback=sub_43_0be6, 1, 1)` which
runs the operation over the selected area. The per-operation code is reached
from `sub_43_0be6` by switching on DS:0xaf5c.
