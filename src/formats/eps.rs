//! Encapsulated PostScript writer, reproducing the layout of WRITEEPI.DLL:
//! separate red/green/blue hex streams per row fed to `colorimage`
//! (or a single grey stream to `image` for greyscale pictures).

use std::fmt::Write;

use crate::core::Image;

pub fn encode(img: &Image, dpi: u32) -> Vec<u8> {
    let (w, h) = (img.w, img.h);
    // Image size in points at the given resolution.
    let pw = (w as u32 * 72).div_ceil(dpi);
    let ph = (h as u32 * 72).div_ceil(dpi);
    let grey = img.px.iter().all(|p| p[0] == p[1] && p[1] == p[2]);

    let mut s = String::new();
    s.push_str("%!PS-Adobe-2.0 EPSF-2.0\n");
    let _ = writeln!(s, "%%BoundingBox: 0 0 {pw} {ph}");
    s.push_str("%%EndComments\n%%EndProlog\n");
    if grey {
        let _ = writeln!(s, "/grstr {w} string def");
    } else {
        let _ = writeln!(
            s,
            "/rstr {w} string def\n/gstr {w} string def\n/bstr {w} string def"
        );
    }
    let _ = writeln!(s, "{pw} {ph} scale");
    let _ = writeln!(s, "{w} {h} 8");
    let _ = writeln!(s, "[{w} 0 0 -{h} 0 {h}]");
    if grey {
        s.push_str("{currentfile grstr readhexstring pop}\nimage\n");
    } else {
        s.push_str("{currentfile rstr readhexstring pop}\n");
        s.push_str("{currentfile gstr readhexstring pop}\n");
        s.push_str("{currentfile bstr readhexstring pop}\n");
        s.push_str("true 3\ncolorimage\n");
    }
    let channels: &[usize] = if grey { &[0] } else { &[0, 1, 2] };
    for y in 0..h {
        for &c in channels {
            for (i, p) in img.row(y).iter().enumerate() {
                let _ = write!(s, "{:02x}", p[c]);
                if i % 36 == 35 {
                    s.push('\n');
                }
            }
            s.push('\n');
        }
    }
    s.push_str("showpage\n%%EOF\n");
    s.into_bytes()
}
