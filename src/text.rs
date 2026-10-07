//! Text areas: "Picture Man treats text as a kind of selected area". The
//! original rasterised the text with GDI into a 1-bpp mask; here a TrueType
//! font is rasterised with `ab_glyph` and thresholded the same way.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};

use crate::core::Mask;

#[derive(Clone)]
pub struct FontEntry {
    pub name: String,
    pub data: Arc<[u8]>,
}

impl std::fmt::Debug for FontEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name)
    }
}

/// Fonts bundled with the program, followed by TrueType/OpenType fonts found
/// in the usual system font folders.
pub fn available_fonts() -> Vec<FontEntry> {
    let mut fonts = vec![
        FontEntry {
            name: "Sans (Ubuntu Light)".into(),
            data: Arc::from(epaint_default_fonts::UBUNTU_LIGHT),
        },
        FontEntry {
            name: "Mono (Hack)".into(),
            data: Arc::from(epaint_default_fonts::HACK_REGULAR),
        },
    ];
    let mut files = Vec::new();
    for dir in system_font_dirs() {
        collect_font_files(&dir, 0, &mut files);
    }
    files.sort();
    files.dedup();
    for path in files.into_iter().take(400) {
        let Ok(data) = std::fs::read(&path) else {
            continue;
        };
        if FontRef::try_from_slice(&data).is_err() {
            continue;
        }
        let name = path
            .file_stem()
            .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
        fonts.push(FontEntry {
            name,
            data: Arc::from(data),
        });
    }
    fonts
}

fn system_font_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if cfg!(target_os = "windows") {
        let windir =
            std::env::var_os("WINDIR").map_or_else(|| PathBuf::from("C:\\Windows"), PathBuf::from);
        dirs.push(windir.join("Fonts"));
    } else if cfg!(target_os = "macos") {
        dirs.extend(["/System/Library/Fonts", "/Library/Fonts"].map(PathBuf::from));
    } else {
        dirs.extend(["/usr/share/fonts", "/usr/local/share/fonts"].map(PathBuf::from));
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        dirs.extend([
            home.join(".fonts"),
            home.join(".local/share/fonts"),
            home.join("Library/Fonts"),
        ]);
    }
    dirs
}

fn collect_font_files(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 4 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_font_files(&p, depth + 1, out);
        } else if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
            let ext = ext.to_ascii_lowercase();
            // Skip font collections and very large files (CJK fonts).
            let small = e.metadata().map(|m| m.len() < 8_000_000).unwrap_or(false);
            if (ext == "ttf" || ext == "otf") && small {
                out.push(p);
            }
        }
    }
}

/// Rasterise `text` (may contain newlines) at `px` pixels per em into a
/// tight binary mask. Returns `None` for empty text or an unusable font.
pub fn render_text_mask(text: &str, font_data: &[u8], px: f32) -> Option<Mask> {
    let font = FontRef::try_from_slice(font_data).ok()?;
    let px = px.clamp(4.0, 2000.0);
    let sf = font.as_scaled(PxScale::from(px));
    let line_h = sf.height() + sf.line_gap();

    // Lay out glyphs, remembering their pixel bounds.
    let mut outlined = Vec::new();
    for (row, line) in text.lines().enumerate() {
        let mut x = 0.0f32;
        let mut prev = None;
        for ch in line.chars() {
            let id = sf.glyph_id(ch);
            if let Some(p) = prev {
                x += sf.kern(p, id);
            }
            let g = id
                .with_scale_and_position(px, ab_glyph::point(x, sf.ascent() + row as f32 * line_h));
            x += sf.h_advance(id);
            prev = Some(id);
            if let Some(o) = font.outline_glyph(g) {
                outlined.push(o);
            }
        }
    }
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for o in &outlined {
        let b = o.px_bounds();
        x0 = x0.min(b.min.x);
        y0 = y0.min(b.min.y);
        x1 = x1.max(b.max.x);
        y1 = y1.max(b.max.y);
    }
    if outlined.is_empty() || x1 <= x0 || y1 <= y0 {
        return None;
    }
    let (w, h) = ((x1 - x0).ceil() as usize, (y1 - y0).ceil() as usize);
    // Keep masks within a sane size even for absurd inputs.
    if w == 0 || h == 0 || w.checked_mul(h)? > 64_000_000 {
        return None;
    }
    let mut m = Mask::empty(w, h);
    for o in &outlined {
        let b = o.px_bounds();
        let (ox, oy) = ((b.min.x - x0) as i64, (b.min.y - y0) as i64);
        o.draw(|gx, gy, cov| {
            let (x, y) = (ox + gx as i64, oy + gy as i64);
            if cov >= 0.5 && x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h {
                m.set(x as usize, y as usize, 255);
            }
        });
    }
    Some(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_text() {
        let m = render_text_mask("PM", epaint_default_fonts::UBUNTU_LIGHT, 40.0).unwrap();
        assert!(m.w > 20 && m.h > 20);
        assert!(m.data.contains(&255));
        assert!(render_text_mask("", epaint_default_fonts::UBUNTU_LIGHT, 40.0).is_none());
        assert!(render_text_mask("x", b"not a font", 40.0).is_none());
    }
}
