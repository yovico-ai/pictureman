//! Settings kept between runs in `pictureman.ini`, using the sections and
//! keys of the original PMAN.INI where they exist ([MODE], [COLOR]).

use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::PathBuf;

use super::apply::OpParams;
use super::tools::{Brush, Edge, Tool, Tools};

pub const MAX_RECENT: usize = 8;

/// Per-user settings file: `~/.config/pictureman/pictureman.ini` (Linux),
/// `~/Library/Application Support/Picture Man/pictureman.ini` (macOS),
/// `%APPDATA%\Picture Man\pictureman.ini` (Windows).
pub fn path() -> Option<PathBuf> {
    let env = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let dir = if cfg!(target_os = "windows") {
        env("APPDATA")?.join("Picture Man")
    } else if cfg!(target_os = "macos") {
        env("HOME")?.join("Library/Application Support/Picture Man")
    } else {
        env("XDG_CONFIG_HOME")
            .or_else(|| env("HOME").map(|h| h.join(".config")))?
            .join("pictureman")
    };
    Some(dir.join("pictureman.ini"))
}

type Ini = BTreeMap<String, BTreeMap<String, String>>;

fn parse(text: &str) -> Ini {
    let mut ini = Ini::new();
    let mut section = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        if let Some(s) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = s.trim().to_ascii_uppercase();
        } else if let Some((k, v)) = line.split_once('=') {
            ini.entry(section.clone())
                .or_default()
                .insert(k.trim().to_ascii_uppercase(), v.trim().to_string());
        }
    }
    ini
}

pub struct Loaded {
    pub show_toolbox: bool,
    pub recent: Vec<PathBuf>,
}

/// Apply the saved settings; unknown or malformed values keep the defaults.
pub fn load(tools: &mut Tools, params: &mut OpParams) -> Loaded {
    let mut out = Loaded {
        show_toolbox: true,
        recent: Vec::new(),
    };
    let Some(text) = path().and_then(|p| std::fs::read_to_string(p).ok()) else {
        return out;
    };
    let ini = parse(&text);
    let get = |s: &str, k: &str| ini.get(s).and_then(|m| m.get(k));
    let num = |s: &str, k: &str| get(s, k).and_then(|v| v.parse::<i64>().ok());
    let byte = |s: &str, k: &str| num(s, k).map(|v| v.clamp(0, 255) as u8);
    let flag = |s: &str, k: &str| num(s, k).map(|v| v != 0);

    // FRAGMENT keeps the original's numbering; 0 (whole image) has no tool.
    if let Some(v) = num("MODE", "FRAGMENT") {
        tools.tool = match v {
            2 => Tool::Ellipse,
            3 => Tool::Polygon,
            4 => Tool::Text,
            5 => Tool::Lasso,
            6 => Tool::Wand,
            7 => Tool::Brush,
            8 => Tool::Eyedropper,
            _ => Tool::Rect,
        };
    }
    if let Some(v) = num("MODE", "EDGE") {
        tools.edge = match v {
            1 => Edge::Low,
            2 => Edge::Medium,
            3 => Edge::High,
            _ => Edge::Sharp,
        };
    }
    if let Some(v) = num("VIEW", "THEME") {
        tools.theme = match v {
            1 => eframe::egui::ThemePreference::Light,
            2 => eframe::egui::ThemePreference::System,
            _ => eframe::egui::ThemePreference::Dark,
        };
    }
    if let Some(v) = num("MODE", "BRUSHEDGE") {
        tools.brush_edge = match v {
            1 => Edge::Low,
            2 => Edge::Medium,
            3 => Edge::High,
            _ => Edge::Sharp,
        };
    }
    if let Some(v) =
        num("MODE", "PENSIZE").filter(|v| super::tools::PEN_SIZES.contains(&(*v as u32)))
    {
        tools.pen_size = v as u32;
    }
    if let Some(v) = num("MODE", "BRUSH") {
        tools.brush = if v == 1 { Brush::Circle } else { Brush::Square };
    }
    if let Some(v) = num("MODE", "TOLERANCE") {
        tools.wand_tolerance = v.clamp(1, 100) as u8;
    }
    tools.wand_unifold = flag("MODE", "UNIFOLD").unwrap_or(tools.wand_unifold);
    tools.wand_hsv = flag("MODE", "RGBMATCH").map_or(tools.wand_hsv, |rgb| !rgb);
    tools.animate = flag("MODE", "ANIMATE").unwrap_or(tools.animate);
    tools.backup = flag("MODE", "BACKUP").unwrap_or(tools.backup);
    out.show_toolbox = flag("MODE", "TOOLBOX").unwrap_or(true);

    if let (Some(r), Some(g), Some(b)) = (
        byte("COLOR", "RED"),
        byte("COLOR", "GREEN"),
        byte("COLOR", "BLUE"),
    ) {
        tools.color = [r, g, b];
    }
    if let (Some(r), Some(g), Some(b)) = (
        byte("COLOR", "LEFTRED"),
        byte("COLOR", "LEFTGREEN"),
        byte("COLOR", "LEFTBLUE"),
    ) {
        params.grad.0 = [r, g, b];
    }
    if let (Some(r), Some(g), Some(b)) = (
        byte("COLOR", "RIGHTRED"),
        byte("COLOR", "RIGHTGREEN"),
        byte("COLOR", "RIGHTBLUE"),
    ) {
        params.grad.1 = [r, g, b];
    }

    if let Some(v) = num("GAMMA", "CHANNELS") {
        params.gamma_rgb = [v & 4 != 0, v & 2 != 0, v & 1 != 0];
    }
    if let Some(v) = num("FLUCTUATION", "GRAIN") {
        params.fluct.grain = v.clamp(1, 16) as i32;
    }
    if let Some(v) = num("FLUCTUATION", "DEPTH") {
        params.fluct.depth = v.clamp(1, 100) as i32;
    }
    if let Some(v) = num("TEXT", "FONT") {
        params.font = v.max(0) as usize;
    }
    if let Some(v) = num("TEXT", "SIZE") {
        params.font_px = (v as f32).clamp(8.0, 400.0);
    }
    if let Some(v) = get("TEXT", "STRING") {
        params.text = v.replace("\\n", "\n");
    }
    for i in 1..=MAX_RECENT {
        if let Some(p) = get("FILES", &format!("RECENT{i}")) {
            out.recent.push(PathBuf::from(p));
        }
    }
    out
}

/// The settings as INI text (also used to detect changes).
pub fn serialize(
    tools: &Tools,
    params: &OpParams,
    show_toolbox: bool,
    recent: &[PathBuf],
) -> String {
    let mut s = String::from("; Picture Man settings\n[MODE]\n");
    let area = match tools.tool {
        Tool::Rect => 1,
        Tool::Ellipse => 2,
        Tool::Polygon => 3,
        Tool::Text => 4,
        Tool::Lasso => 5,
        Tool::Wand => 6,
        Tool::Brush => 7,
        Tool::Eyedropper => 8,
    };
    let edge = match tools.edge {
        Edge::Sharp => 0,
        Edge::Low => 1,
        Edge::Medium => 2,
        Edge::High => 3,
    };
    let b = |v: bool| v as u8;
    let _ = writeln!(
        s,
        "FRAGMENT={area}\nEDGE={edge}\nBRUSHEDGE={}\nPENSIZE={}",
        tools.brush_edge as u8, tools.pen_size
    );
    let _ = writeln!(
        s,
        "BRUSH={}\nTOLERANCE={}",
        (tools.brush == Brush::Circle) as u8,
        tools.wand_tolerance
    );
    let _ = writeln!(
        s,
        "UNIFOLD={}\nRGBMATCH={}",
        b(tools.wand_unifold),
        b(!tools.wand_hsv)
    );
    let _ = writeln!(
        s,
        "ANIMATE={}\nBACKUP={}\nTOOLBOX={}",
        b(tools.animate),
        b(tools.backup),
        b(show_toolbox)
    );
    let [r, g, bl] = tools.color;
    let ([lr, lg, lb], [rr, rg, rb]) = params.grad;
    let _ = writeln!(s, "\n[COLOR]\nRED={r}\nGREEN={g}\nBLUE={bl}");
    let _ = writeln!(
        s,
        "LEFTRED={lr}\nLEFTGREEN={lg}\nLEFTBLUE={lb}\nRIGHTRED={rr}\nRIGHTGREEN={rg}\nRIGHTBLUE={rb}"
    );
    let [gr, gg, gb] = params.gamma_rgb;
    let _ = writeln!(
        s,
        "\n[GAMMA]\nCHANNELS={}",
        (gr as u8) << 2 | (gg as u8) << 1 | gb as u8
    );
    let _ = writeln!(
        s,
        "\n[FLUCTUATION]\nGRAIN={}\nDEPTH={}",
        params.fluct.grain, params.fluct.depth
    );
    let _ = writeln!(
        s,
        "\n[TEXT]\nFONT={}\nSIZE={}\nSTRING={}",
        params.font,
        params.font_px.round(),
        params.text.replace('\n', "\\n")
    );
    let theme = match tools.theme {
        eframe::egui::ThemePreference::Dark => 0,
        eframe::egui::ThemePreference::Light => 1,
        eframe::egui::ThemePreference::System => 2,
    };
    let _ = writeln!(s, "\n[VIEW]\nTHEME={theme}");
    s.push_str("\n[FILES]\n");
    for (i, p) in recent.iter().take(MAX_RECENT).enumerate() {
        let _ = writeln!(s, "RECENT{}={}", i + 1, p.display());
    }
    s
}

pub fn save(text: &str) -> std::io::Result<()> {
    let Some(p) = path() else { return Ok(()) };
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Write-then-rename so a crash never leaves a truncated file.
    let tmp = p.with_extension("ini.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let t = Tools {
            tool: Tool::Brush,
            edge: Edge::High,
            color: [1, 2, 3],
            wand_hsv: true,
            ..Default::default()
        };
        let p = OpParams {
            text: "two\nlines".into(),
            gamma_rgb: [true, false, true],
            ..Default::default()
        };
        let text = serialize(&t, &p, false, &[PathBuf::from("/a b/ü.png")]);
        let ini = parse(&text);
        assert_eq!(ini["MODE"]["FRAGMENT"], "7");
        assert_eq!(ini["COLOR"]["BLUE"], "3");
        assert_eq!(ini["FILES"]["RECENT1"], "/a b/ü.png");
        assert_eq!(ini["TEXT"]["STRING"], "two\\nlines");
        assert_eq!(ini["GAMMA"]["CHANNELS"], "5");
        // Garbage is ignored rather than trusted.
        let bad =
            parse("[MODE]\nPENSIZE=999999999999999999999\nEDGE=x\nno equals sign\n[COLOR\nRED=-5");
        assert!(bad["MODE"].contains_key("PENSIZE"));
    }
}
