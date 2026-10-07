//! ZSoft PaintBrush PCX (READPCX.DLL / WRITEPCX.DLL).
//! Reads 1-bit, 4-plane 16-color, 8-bit paletted and 24-bit (3 planes);
//! writes 24-bit, 3-plane version 5 files.

use super::{Error, Result};
use crate::core::{Image, checked_area};

const HEADER: usize = 128;

pub fn is_pcx(b: &[u8]) -> bool {
    b.len() > HEADER && b[0] == 0x0a && b[1] <= 5 && b[2] == 1
}

fn u16le(b: &[u8], o: usize) -> usize {
    u16::from_le_bytes([b[o], b[o + 1]]) as usize
}

pub fn decode(b: &[u8]) -> Result<Image> {
    if b.len() <= HEADER {
        return Err(Error::Corrupt("file too short".into()));
    }
    let bpp = b[3] as usize;
    let (x0, y0, x1, y1) = (u16le(b, 4), u16le(b, 6), u16le(b, 8), u16le(b, 10));
    if x1 < x0 || y1 < y0 {
        return Err(Error::Corrupt("bad window".into()));
    }
    let (w, h) = (x1 - x0 + 1, y1 - y0 + 1);
    if checked_area(w, h).is_none() {
        return Err(Error::Unsupported(format!("{w}x{h} image is too large")));
    }
    let planes = b[65] as usize;
    if !matches!((bpp, planes), (8, 3) | (8, 1) | (1, 1..=4) | (4, 1)) {
        return Err(Error::Unsupported(format!("{bpp} bits x {planes} planes")));
    }
    // Bytes per plane line must hold a full row: a smaller value made the
    // original READPCX overrun its row buffer.
    let bpl = u16le(b, 66);
    let need = (w * bpp).div_ceil(8);
    if bpl < need {
        return Err(Error::Corrupt(format!(
            "{bpl} bytes per line for {w} pixels"
        )));
    }
    let line = planes * bpl;
    let total = line
        .checked_mul(h)
        .ok_or_else(|| Error::Corrupt("size overflow".into()))?;

    // RLE-decode all scanlines.
    let mut data = Vec::with_capacity(total);
    let mut i = HEADER;
    while data.len() < total {
        let Some(&c) = b.get(i) else {
            return Err(Error::Corrupt("truncated data".into()));
        };
        i += 1;
        if c & 0xc0 == 0xc0 {
            let v = *b
                .get(i)
                .ok_or_else(|| Error::Corrupt("truncated run".into()))?;
            i += 1;
            data.extend(std::iter::repeat_n(v, (c & 0x3f) as usize));
        } else {
            data.push(c);
        }
    }

    let ega_pal = |k: usize| [b[16 + 3 * k], b[17 + 3 * k], b[18 + 3 * k]];
    let vga_pal = || -> Result<&[u8]> {
        if b.len() >= 769 && b[b.len() - 769] == 0x0c {
            Ok(&b[b.len() - 768..])
        } else {
            Err(Error::Corrupt("missing 256-color palette".into()))
        }
    };

    let mut img = Image::new(w, h, [0; 3]);
    match (bpp, planes) {
        (8, 3) => {
            for y in 0..h {
                let s = &data[y * line..];
                for x in 0..w {
                    img.set(x, y, [s[x], s[bpl + x], s[2 * bpl + x]]);
                }
            }
        }
        (8, 1) => {
            let pal = vga_pal()?;
            for y in 0..h {
                for x in 0..w {
                    let k = data[y * line + x] as usize;
                    img.set(x, y, [pal[3 * k], pal[3 * k + 1], pal[3 * k + 2]]);
                }
            }
        }
        (1, n @ 1..=4) => {
            for y in 0..h {
                let s = &data[y * line..];
                for x in 0..w {
                    let mut k = 0;
                    for p in 0..n {
                        k |= (((s[p * bpl + x / 8] >> (7 - x % 8)) & 1) as usize) << p;
                    }
                    let c = if n == 1 {
                        [k as u8 * 255; 3]
                    } else {
                        ega_pal(k)
                    };
                    img.set(x, y, c);
                }
            }
        }
        (4, 1) => {
            for y in 0..h {
                for x in 0..w {
                    let v = data[y * line + x / 2];
                    img.set(x, y, ega_pal(((v >> (4 * (1 - x % 2))) & 15) as usize));
                }
            }
        }
        _ => unreachable!("validated above"),
    }
    Ok(img)
}

pub fn encode(img: &Image) -> Vec<u8> {
    if img.w == 0 || img.h == 0 || img.w > 65_536 || img.h > 65_536 {
        return Vec::new();
    }
    let bpl = (img.w + 1) & !1; // bytes per plane line must be even
    let mut out = vec![0u8; HEADER];
    out[0] = 0x0a;
    out[1] = 5;
    out[2] = 1;
    out[3] = 8;
    out[8..10].copy_from_slice(&((img.w - 1) as u16).to_le_bytes());
    out[10..12].copy_from_slice(&((img.h - 1) as u16).to_le_bytes());
    out[12..14].copy_from_slice(&72u16.to_le_bytes());
    out[14..16].copy_from_slice(&72u16.to_le_bytes());
    out[65] = 3;
    out[66..68].copy_from_slice(&(bpl as u16).to_le_bytes());
    out[68] = 1;

    let mut plane = vec![0u8; bpl];
    for y in 0..img.h {
        for c in 0..3 {
            for (x, p) in img.row(y).iter().enumerate() {
                plane[x] = p[c];
            }
            // Runs never cross a plane line.
            let mut i = 0;
            while i < bpl {
                let v = plane[i];
                let mut n = 1;
                while i + n < bpl && n < 63 && plane[i + n] == v {
                    n += 1;
                }
                if n > 1 || v & 0xc0 == 0xc0 {
                    out.push(0xc0 | n as u8);
                }
                out.push(v);
                i += n;
            }
        }
    }
    out
}
