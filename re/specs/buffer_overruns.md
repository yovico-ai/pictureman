# Picture Man 1.55: buffer-overrun audit

This audit covers PMAN.EXE and the plug-in DLLs. It is static only, based on the `nedis.py` disassembly of PMAN.EXE and the DLLs. Nothing was run under Windows 3.x. Addresses are `seg:offset` in the NE file. `DS:xxxx` is DGROUP. Global sizes are inferred from the next DS address that any code references. Stack buffer sizes come from the frame layout.

## C runtime and helpers identified (PMAN seg 1)

| addr | routine | addr | routine |
|---|---|---|---|
| 1:00bc | `strcat` (near) | 1:02bc | `strchr` |
| 1:00fc | `strcpy` (near) | 1:02e6 | `strrchr` |
| 1:012e | `strcmp` | 1:030e | `strtok` |
| 1:015a | `strlen` | 1:039e / 03ca | `memcpy` / `memset` |
| 1:01ca | `sprintf` (count 0x7fff, so **unbounded**) | 1:07bc | `_fullpath(buf, path, maxlen)` (bounded) |
| 1:0a98 | `remove` (DOS 41h) | 1:1cb4 | `_getcwd(buf, maxlen)` (bounded) |
| 1:0ddc / 0e0e / 0e32 | `_fstrchr` / `_fstrupr` / `_fstrcpy` | 1:1ac8 | `strncpy` |
| 1:45b0 | `strupr` | 1:0d7e | `_fmemcpy` |

There is no `_splitpath`/`_makepath`. File names are built with `GetModuleFileName`, `strrchr('\\')`, then `lstrcpy`/`lstrcat`. No `SHELL.DragQueryFile`, no DDE, and no call to `CommDlgExtendedError`.

## Key path buffers

| Buffer | Size | Contents | Filled by |
|---|---|---|---|
| DS:9300 | **64** | current image file name | the Open/Save dialog (`nMaxFile` = 64), `_fullpath(...,0x40)` from the command line, `lstrcpy` from the grabber |
| DS:a120 | 66 | converter DLL name | the format table entry (≤63 characters, from the INI with `nSize` 0x40) |
| DS:70ca | 64 | directory of PMAN.EXE (with trailing `\`) | `GetModuleFileName(...,0x40)` at seg3:0b32 |
| DS:93b0 | **64** | path of PMAN.INI | dir + `"pman.ini"` (seg3:0b52/0b5f) |
| DS:3566 | **60** | path of PMAN.INI (print settings) | `GetModuleFileName(...,0x40)` + `"PMAN.INI"` (seg62:0e46..0ec9) |
| DS:838a | 64 | TMPDIR, later WORKDIR | `GetPrivateProfileString(...,0x40)`, then `_getcwd(...,0x40)` |
| DS:821c | **130** | general scratch: window title, message text, sprintf | many callers |
| DS:92ae / 9366 | 66 / 64 | undo copies of 9300 / a120 | `strcpy` |
| DS:91d8 | 64 | GRABBER DLL name | INI, `nSize` 0x40 |

The OPENFILENAME struct is built at seg25:14f6 at bp-0x96. `lpstrFile` = bp-0xf2 (64 bytes), `nMaxFile` = 0x40, `lpstrFileTitle` = NULL. `Flags` = 0x64 (HIDEREADONLY, ENABLEHOOK, ENABLETEMPLATE). `lCustData` = the format table. After a successful call, `lpstrFile` is copied with `lstrcpy` into the caller's buffer: DS:9300 from seg67:0da0/141c/1b8f, or a 128-byte stack buffer from seg11:1641 and seg50:1f30.

**Image path limit:** a file whose full path is 64 characters or more cannot be opened through the dialog. COMMDLG returns FALSE (`FNERR_BUFFERTOOSMALL`), and PMAN never calls `CommDlgExtendedError`, so the Open command does nothing and shows no message. In 1.55 the dialog itself does not overflow (but see F11).

The converter DLLs never receive the path. PMAN calls `_lopen` itself and passes only the handle (info+0x88), so converter `ReadHeader` cannot overflow on a path.

## Findings

Severity: **H** = stack smash or GP fault is likely; **M** = memory corruption or misbehaviour; **L** = latent, or reachable only through a third-party plug-in or a hostile INI or resource. "d" = length of the PMAN.EXE directory including the trailing `\`.

| # | Location | Buffer (where, size) | Source of data | Max safe input | Overflows with long path? | Effect | Sev | Conf |
|---|---|---|---|---|---|---|---|---|
| F1 | seg24:07e0 (build the reader table, called at the start of every File/Open dialog seg25:159a and from Save seg48:02ff) at 24:0930..095b | stack bp-0x54, **64** bytes. Then bp-0x14 hLib, **bp-0x12/-0x10 far pointer to the format table**, bp-0xe key list pointer, bp-4 index, saved DS/BP/return | `GetModuleFileName(..,0x40)` truncated at the last `\`, then `lstrcat` of the `[Sources]` DLL name (e.g. `readtif.dll`, 11 characters) | d + len(dll) ≤ 63 | **Yes, with a deep install directory.** d ≥ 55 writes NUL/`l` over the table pointer offset, and d ≥ 57 overwrites its selector. d > 55 only happens when the PMAN.EXE path is 64 characters or more and is truncated, which leaves the `\` cut wrong. | Description and extension are written through a corrupted far pointer, giving a GP fault **the moment File/Open is chosen**. This is the best candidate for "crashes when loading an image" from a deep directory: the deep **program** directory, not the image directory. | H | High |
| F2 | seg18:04c2 (build the writer table: Save As via seg25:15ac, Save via seg48:0029) at 18:0607..062e | same layout as F1 (bp-0x54 64 bytes, table pointer at bp-0x12/-0x10) | exe directory + `[Destinations]` DLL name | d + len(dll) ≤ 63 | Yes, same thresholds as F1 | GP fault when Save or Save As is chosen | H | High |
| F3 | seg24:09b6 (open converter for load or Info) at 24:09d9..09fd | stack bp-0x54, 64 bytes. Locals above it are all assigned **after** the copy. | exe directory + DS:a120 | 63 | With 11-character names this overflows only into dead locals. Reaching saved DS needs a string of 82+ characters (only possible with long relative DLL names in the INI). | Normally harmless. A crash needs exe directory + DLL name ≥ 82. | L–M | High |
| F4 | seg3:0b4a..0b5f (WinMain init) | DS:93b0 INI path, **64** bytes. Next global DS:93f0 is an int set from the INI at seg3:0ca1. | exe directory + `"pman.ini"` | d ≤ 55 | Yes, d ≥ 56 | The string runs into DS:93f0. When 93f0 is then set from the INI, the **INI file name is corrupted**, and every later Get/WritePrivateProfile* call (converter lists, settings) uses a garbage file name. The result is an empty format list or settings that are lost. | M | High |
| F5 | seg62:0e34 (print setup init, called from WinMain at seg3:0be7) | DS:3566, **60** bytes. DS:35a2/35a4 are print variables (seg62:2371). | `GetModuleFileName(..,0x40)`: the size passed (64) is **larger than the buffer** (60). Then `lstrcat "PMAN.INI"` is done 3 times. | exe path ≤ 59, d ≤ 51 | Yes, d ≥ 52 | Overwrites the print offset/size globals and corrupts the INI name used for the `[print]` keys | M | High |
| F6 | seg3:13a2..13b2 (WinMain) | stack bp-0x62, **64** bytes. Above it: MSG at bp-0x22, locals, saved DS at bp-2, return at bp+2. | `lstrcpy(buf, lpCmdLine)`, **unbounded**. The command line comes from File Manager association, Run, or a Program Manager item. | 63 | 64..95 characters damage only the MSG struct and locals, which are rewritten. **96 or more characters overwrite saved DS, BP and the return address, giving a GP fault when WinMain returns (on exit).** The DOS path limit (~79) keeps a plain path below 96, but extra arguments do not. | Then `_fullpath(DS:9300, buf, 0x40)` (seg3:13c0) is bounded. A 64+ character path fails and leaves 9300 partial or invalid ("Can't open file"). | M | High |
| F7 | seg3:2798 (command-line option parser, seg3:0f5e) | stack bp-0x86, 128 bytes. DS:8f40, 128 bytes. | rest of the command line, or exe directory + a relative name | 127 | No (the Win16 command line is ≤127). exe directory + a relative token > 64 characters would overflow DS:8f40. | none in practice | L | High |
| F8 | seg48:0000 (File/Save) and seg54:062e | stack bp-0x54, **4 bytes** (`char ext[4]`), directly below `dll[64]` at bp-0x50 | `strcpy(ext, strupr(strrchr(copy_of_9300,'.')+1))` | a 3-character extension | A dot in a **directory** name plus an extension-less file (`C:\WORK.93\PHOTOS\FACE`) or a 4+ character extension overflows. Because 9300 ≤ 63, the overrun stays inside `dll[]` and the frame is reached only at 68+. | The extension does not match, so it falls through to Save As. Latent but real; it becomes a stack smash as soon as the name buffer grows. seg48:02ba/seg54 use a 68-byte buffer here and are safe. | L–M | High |
| F9 | seg25:14f6 at 25:153f (Open/Save dialog default extension) | stack bp-0x106, **20** bytes, followed by the `lpstrFile` buffer at bp-0xf2 | `lstrcpy` of the caller's "default extension" = `strrchr(DS:9300,'.')+1` (seg67:13f8/1b6b, seg50:1f17) | 19 | Same dot-in-directory case: the extension can be about 60 characters | Overwrites the initial text of the file name box (bp-0xf2). No frame damage while 9300 ≤ 63. | L | High |
| F10 | seg25:0000 (the dialog's **Info** button, hook seg25:12b6 → `GetDlgItemText(edt1, buf, 0x3f)`) at 25:0176 | **DS:821c, 130 bytes**. Next is DS:829e/82a0/82e0 (globals of seg77/81). | `wsprintf("File: %s\n Format: %s\nWidth: %d ... Raw data size: %ld")`: 72 fixed characters + typed name (≤63) + format description (≤28) + numbers (≤36) = up to **~200** | name ≤ ~20 characters with typical values | Yes, if the user types a path in the File name box and presses Info. An 8.3 name alone fits. | DGROUP overrun of up to ~70 bytes into the seg77/81 state | M | High |
| F11 | seg25:14f6, OPENFILENAME | `lpstrFile` at bp-0xf2, 64 bytes, `nMaxFile` = 64 | COMMDLG | 63 | No overflow when COMMDLG respects `nMaxFile`. With a COMMDLG that does not (an assumption to test), the overflow would hit the OPENFILENAME struct itself at bp-0x96. | Paths of 64+ characters are silently refused (see above) | L (functional: **M**) | Med |
| F12 | seg67:19a7..1a0a (Acquire through the GRABBER DLL, DS:91d8) | **DS:9300, 64** bytes | `lstrcpy(DS:9300, name returned by the external Grab())` (stack bp-0xda, 210 bytes) | 63 | Yes, if the grabber returns a long temp path (e.g. a deep TEMP directory) | DGROUP overrun into DS:9340.. | M | High |
| F13 | seg3:0b7a..0b97 (TMPDIR) | DS:838a, 64 bytes | INI TMPDIR (`nSize` 64). A `\` is appended at len and NUL at len+1. | 62 | A TMPDIR of exactly 63 characters without a trailing `\` writes NUL to DS:83ca (1 byte). Longer values are silently truncated, giving the wrong directory. | 1-byte overrun / wrong directory. TMPDIR is only checked with `access()` and saved back. MWAREA uses `%TEMP%` (see D3). | L | High |
| F14 | seg24:015e at 24:01d6..023a (palette fix-up after ReadHeader) | info struct palette planes +0xa4/+0x1a4/+0x2a4 (256 bytes each). The struct is on the stack (bp-0x3fc in seg23:0151, bp-0x400 in seg25:0000). | Inversion loop for flags `2 and 4`: the bound `[bp-8]` is **never initialised** (it is only assigned at 24:0283) | – | n/a | Inverts up to 3×(garbage+1) bytes, smashing the stack. No shipped converter sets flags 2 and 4 together (READTIF sets 4 only without a ColorMap, READTIF seg1:0926). **Latent; third-party plug-ins could trigger it.** | L (H if reached) | High |
| F15 | seg24:015e at 24:0276..02e4 (grey ramp) | same planes | count = `1 << bpp` from the converter header (+0x8e), any bpp except 24 | bpp ≤ 8 | n/a | bpp 9..15 gives 512..32768 entries and smashes the stack. Every shipped reader restricts bpp to {1,4,8,24} (READTIF seg1:09bf, READBMP). Latent for plug-ins. | L | High |
| F16 | seg24:05ac / 065e (de-planarise 3×8-bit and 4×1-bit PCX-style rows) | scratch `GlobalAlloc(planes*BPL)` at seg24:0db2 | writes `width*3` bytes (05ac) or `width/2` bytes (065e). width (+0x8a) and BPL (+0x94) come from the file header via READPCX, with no cross-check. | 3·width ≤ planes·BPL | n/a (file content) | **Heap overrun** with a crafted PCX (BytesPerLine < width) | H | High |
| F17 | seg25:14f6 at 25:1608..1757 (building the filter string) | `GlobalAlloc(count*0x88+2)` | description (≤128, from the DLL's `GetDescription`) + `*.` + extension per entry | description + extension ≤ ~130 | n/a | Shipped descriptions are ≤28 characters, so this is safe. A plug-in with a long description overruns. The format table slots (+0x40 description 128 bytes, +0xc0 extension 4 bytes, then flags at +0xc4) are filled by the DLL with **no size argument**. | L | High |
| F18 | various `sprintf` (1:01ca) with `%f` into DS:821c (130 bytes) or DS:3a48 | | doubles/floats from dialogs or INI (`[print] width` read with `atof`) | < 130 characters | n/a | Float arguments are ≤ ~45 characters and are safe. A double from an unchecked edit field would print up to ~310 characters. All sites checked use scrollbar/clamped values or `float`, so none was found exploitable. | L | Med |
| F19 | seg3:25ca (window title) | DS:821c | `wsprintf(821c, "%s (%s:%dx%dx%d)", 821c, basename(9300), ...)` (**source and destination overlap**) | – | No, only the base name is used | Works only because `%s` copies 821c onto itself first. Undefined behaviour; avoid in the port. | L | High |

Checked and safe: the message-box helper seg3:2506 (`wvsprintf` into a 400-byte stack buffer). The "File %s exists" text at seg25:18f2 (256 bytes). The `lstrcpy` of the 64-byte DLL name into DS:a120 (66 bytes). Undo copies into DS:92ae (66) and DS:9366 (64). Exe directory + `pman.exe` into a 128-byte buffer at seg47:01d6. Windows directory + `TWAIN.DLL` into 160 bytes at seg90:006a (the Windows directory is ≤144). Text tool `GetDlgItemText(...,0x100)` into DS:30b0 (256). Every other `GetDlgItemText` size matches its buffer. `GetProfileString` for the printer reads 0x50 into DS:3462 (80). The DIB reader seg53:09c4 sizes its allocation and its read from the same `biClrUsed`. Temp names (`0987.tmp`, `$$TMP$.BMP`) are relative and use `OpenFile` with a 136-byte OFSTRUCT.

### DLL side (from the two DLL sub-audits, re-checked where noted)

| # | Location | Buffer | Source | Problem | Sev | Conf |
|---|---|---|---|---|---|---|
| D1 | READPCX header → PMAN seg24:05ac | see F16 | BytesPerLine, Xmax-Xmin | BPL is never compared with the width | H | High |
| D2 | READBMP / PMAN seg23:01b4 (row buffer = max((w+24)·bpp/8, planes·BPL)+12, signed 16-bit `imul`) | row buffer | `biWidth` | A huge width wraps, so the row buffer can be smaller than the BPL that READROW writes | M | Med |
| D3 | MWAREA seg3:008d (swap file) | `GetTempFileName` output into a 150-byte header field, `_fstrncpy` to DS:2e8 (64), `_fullpath(...,0x40)` | `%TEMP%` | Truncated, not overflowed. A TEMP path longer than ~50 characters gives a wrong or orphaned swap file. | M (functional) | High |
| D4 | READBMP RLE4/RLE8 (seg1:0b28, 0e81), DTARGA RLE (seg1:0f28) | row | run lengths | Can write **one byte or one packet past the row end**. This is absorbed by PMAN's 12-byte slack. | L | High/Med |
| D5 | READGIF LZW (seg1:0708/07c8) | private 0x6025-byte block | code stream | Code size is capped at 12 and chains at 0xfff. Not verified: whether `next_code` can pass 4095 without a clear code. | L | Med |
| D6 | READTIF ColorMap fill (seg1:0a5a..0be8) | palette planes | `1<<bpp` | One sub-audit flagged BitsPerSample > 8 here. **Refuted:** bpp (+0x8e) is limited to {1,4,8,24} at seg1:093e→09bf before the loop, and 24 gives a count of 0. | – | High |
| D7 | READTIF LZW (seg1:16e4..1edf), strip array (seg1:0cb5) | tables in the private block, `LocalAlloc(strips*4)` | codes ≤ 12 bits, output clipped to the row; allocation equals the count read | OK | – | High |
| D8 | DTARGA palette (seg1:0214) | 3×256 | always reads 256 entries (the TGA colour-map length is ignored) | Safe, but a colour map with more than 256 entries is misread | L | High |
| D9 | DJPG (STOIK JPEG library) | Huffman, quant and component tables | DHT counts, DQT index, SOF Nf, sampling factors | **Not audited.** The `cmp ax,3` at READHEADER seg1:0126 is an error-code test, not a component limit. Treat it as unbounded. | ? | Low |
| D10 | WRITE*, CJPG, CTARGA | numeric `wsprintf` (EPS BoundingBox) and line buffers sized from the image | – | Only skimmed. No path buffers found. | L | Low-Med |

## Checks the Rust port must have

1. **No fixed-length path buffers.** Use `PathBuf`/`String` for the program directory, INI path, image path, converter DLL path, TMPDIR/WORKDIR, temp and swap names and grabber output (F1–F6, F12, F13, D3). Test the port with an install directory and an image directory longer than 260 characters, with non-ASCII names, and with dots in directory names.
2. **Find extensions with `Path::extension()`**, not by "after the last `.` in the full path" (F8, F9). Compare extensions without regard to case and without length assumptions (`.tiff`, `.jpeg`).
3. **Never truncate silently.** If a path or name cannot be used, show an error. Do not drop it the way the `nMaxFile=64` Open dialog does (F11), and do not cut TMPDIR (F13).
4. **Command-line arguments** are parsed into owned strings of any length (F6, F7).
5. **Build message, title and info text with `format!`.** Never write into a shared scratch buffer, and never let source and destination overlap (F10, F19). Cap or elide very long names only for display.
6. **The palette is a `[Rgb; 256]` with an explicit, validated count** ≤ 256. Initialise every loop bound. Reject bpp not in {1,4,8,24} (or support more depths with properly sized storage) before building grey ramps or inverting (F14, F15, D6).
7. **Validate converter headers before allocating:** width and height > 0 and within limits. PCX: `BytesPerLine*8 ≥ width*bits_per_plane`, `Xmax ≥ Xmin`, planes ∈ {1,3,4}. BMP: `biClrUsed ≤ 2^bpp`. Use checked arithmetic (`checked_mul`) for every row, strip and band size. Size de-planarise buffers from the width actually written (F16, D1, D2).
8. **Decoders write only through bounds-checked slices.** RLE runs (BMP RLE4/8, Targa, PCX) must stay inside the row, with no "one past the end" write (D4). GIF/TIFF LZW: code < 4096, prefix chains terminate, output clipped to the row (D5, D7). Targa colour map: honour the header's length and entry size (D8).
9. **JPEG** (if not using a vetted crate): DHT counts sum to ≤ 256, DQT/DHT table id ≤ 3, SOF component count ≤ 4, sampling factors 1..4, and restart interval and MCU sizes validated (D9).
10. **Plug-in boundary (if one is kept):** pass the buffer length with every string or array a plug-in fills (description, extension, palette), and never trust plug-in header fields (F14, F15, F17).
