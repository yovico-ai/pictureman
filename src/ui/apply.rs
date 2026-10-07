//! Bridge between commands and the operations recovered from PMAN.EXE
//! (`crate::ops`): parameters, the parameter dialogs, and execution.

use std::sync::Arc;

use eframe::egui::{self, Color32};

use super::commands::Cmd;
use super::widgets;
use crate::core::{Image, Mask, MsRand, Rect, Rgb};
use crate::ops::fill::{Fluctuation, GradientKind, PatchMode, RadialShape};
use crate::ops::filters::FilterSize;
use crate::ops::transform::{DEFORM_PARAM_MAX, Fragment, MirrorKind};
use crate::ops::tune::{ColorMap, Linear};
use crate::ops::{effects, fill, filters, transform, tune};
use crate::text::FontEntry;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasteLogic {
    Replace,
    BlackTransparent,
    WhiteTransparent,
}

/// Parameters of every operation, kept between invocations like the
/// original's globals (some dialogs reset theirs when they open, exactly as
/// PMAN did — see `prepare`).
#[derive(Clone)]
pub struct OpParams {
    /// The current ("system") color: plain fill, emboss, background.
    pub color: Rgb,
    pub filter: FilterSize,
    /// Keep the filter window square.
    pub square_filter: bool,
    /// TV dialog in user terms: contrast −50..50, brightness and color −255..255.
    pub tv: (i32, i32, i32),
    pub linear: ColorMap,
    pub gamma: f64,
    pub gamma_rgb: [bool; 3],
    pub fluct: Fluctuation,
    pub grad: (Rgb, Rgb),
    pub pattern: Option<Arc<Image>>,
    pub pattern_name: String,
    pub mirror: MirrorKind,
    pub distortion: u32,
    pub dsize: u32,
    pub angle: f64,
    pub new_size: (usize, usize),
    pub keep_aspect: bool,
    pub paste_logic: PasteLogic,
    pub paste_level: u8,
    pub font: usize,
    pub font_px: f32,
    pub text: String,
    /// Rubber deformation: reference point and its new place (image coords).
    pub rubber: ((i64, i64), (i64, i64)),
}

impl Default for OpParams {
    fn default() -> Self {
        OpParams {
            color: [0, 166, 166],
            filter: FilterSize::default(),
            square_filter: true,
            tv: (0, 0, 0),
            linear: ColorMap::default(),
            gamma: 1.0,
            gamma_rgb: [true; 3],
            fluct: Fluctuation::default(),
            // PMAN.INI [COLOR] LEFT*/RIGHT* as shipped.
            grad: ([0, 255, 0], [255, 0, 0]),
            pattern: None,
            pattern_name: String::new(),
            mirror: MirrorKind::EllipticConvex,
            distortion: 0,
            dsize: 0,
            angle: 90.0,
            new_size: (640, 480),
            keep_aspect: true,
            paste_logic: PasteLogic::Replace,
            paste_level: 32,
            font: 0,
            font_px: 48.0,
            text: "Picture Man".into(),
            rubber: ((0, 0), (0, 0)),
        }
    }
}

/// What an operation is applied to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Whole,
    /// A selection; `elliptic` when it was made with the ellipse tool (the
    /// radial gradient and Rubber follow its shape).
    Selection {
        elliptic: bool,
    },
    /// The brush's square, painting.
    Brush {
        circle: bool,
    },
}

/// Context an operation may need besides its parameters.
pub struct Ctx<'a> {
    pub scope: Scope,
    /// Selected pixels (non-zero), full image size.
    pub mask: &'a Mask,
    pub rng: &'a mut MsRand,
    /// Last backup (for Erase).
    pub backup: Option<&'a Image>,
}

pub fn needs_dialog(cmd: Cmd) -> bool {
    use Cmd::*;
    matches!(
        cmd,
        Smoothing
            | SpotRemoving
            | Minimum
            | Maximum
            | HandDrawing
            | CleaningBackground
            | Mosaic
            | FacetedGlass
            | Scatter
            | RgbTv
            | RgbLinear
            | Gamma
            | FillFluctuated
            | GradientV
            | GradientH
            | GradientRadial
            | PatternTiled
            | PatternScaled
            | PatternFitted
            | Deformations
            | Rotate
            | Size
            | New
            | Paste
            | PasteFrom
            | MagicWandOptions
    )
}

/// Ops whose result is shown live in the dialog.
pub fn has_preview(cmd: Cmd) -> bool {
    use Cmd::*;
    needs_dialog(cmd)
        && !matches!(
            cmd,
            Size | New | Paste | PasteFrom | Rotate | MagicWandOptions
        )
}

/// Identifies the parameters that matter for a preview, so it is only
/// recomputed when something changed.
pub fn preview_key(cmd: Cmd, p: &OpParams) -> String {
    format!(
        "{cmd:?}|{:?}|{:?}|{:?}|{}|{:?}|{:?}|{:?}|{:?}|{:?}|{}|{}|{}|{:?}",
        p.color,
        p.filter,
        p.tv,
        p.gamma,
        p.gamma_rgb,
        p.fluct,
        p.grad,
        p.linear,
        p.mirror,
        p.distortion,
        p.dsize,
        p.pattern
            .as_ref()
            .map_or(0, |a| std::sync::Arc::as_ptr(a) as usize),
        p.rubber,
    )
}

fn uses_tile_size(cmd: Cmd) -> bool {
    matches!(cmd, Cmd::Mosaic | Cmd::FacetedGlass | Cmd::Scatter)
}

/// Reset what the original's dialogs reset every time they open.
pub fn prepare(cmd: Cmd, p: &mut OpParams, img: Option<&Image>) {
    use Cmd::*;
    match cmd {
        Smoothing | SpotRemoving | Minimum | Maximum | HandDrawing | CleaningBackground
        | Mosaic | FacetedGlass | Scatter => p.filter = FilterSize::default(),
        RgbTv => p.tv = (0, 0, 0),
        RgbLinear => p.linear = ColorMap::default(),
        Gamma => p.gamma = 1.0,
        Rotate => p.angle = 90.0,
        Size => {
            if let Some(i) = img {
                p.new_size = (i.w, i.h);
            }
        }
        _ => {}
    }
}

/// One line about what the command does (after the 1993 manual).
pub fn description(cmd: Cmd) -> &'static str {
    use Cmd::*;
    match cmd {
        Smoothing => "Averages each pixel with its neighbours.",
        SpotRemoving => "Median filter: removes specks and scratches, keeps edges.",
        Minimum => "Spreads the dark parts of the image.",
        Maximum => "Spreads the bright parts of the image.",
        HandDrawing => "Turns the picture into a pencil-like drawing.",
        CleaningBackground => "Smooths flat areas while keeping contrasty details.",
        Mosaic => "Divides the picture into flat tiles.",
        FacetedGlass => "Looks at the picture through faceted glass.",
        Scatter => "Scatters pixels randomly within the window.",
        RgbTv => "Adjusts contrast, brightness and color like a TV set.",
        RgbLinear => "Linear transfer curves for brightness, saturation and each channel.",
        Gamma => "Lightens or darkens the midtones.",
        FillFluctuated => "Fills with a natural, fluctuating color.",
        GradientV | GradientH | GradientRadial => {
            "Fills with a smooth transition between two colors."
        }
        PatternTiled | PatternScaled | PatternFitted => "Fills with a picture used as a pattern.",
        Deformations => "Reflections in curved mirrors, waves and whirlpools.",
        Rotate => "Rotates the selection or the whole image.",
        Size => "Changes the size of the whole image.",
        New => "Creates an empty image.",
        Paste | PasteFrom => "How the pasted picture is combined with the image.",
        _ => "",
    }
}

fn linear_rows(ui: &mut egui::Ui, rows: [(&str, &mut Linear, Color32); 5]) {
    ui.spacing_mut().slider_width = 110.0;
    egui::Grid::new("linear")
        .num_columns(3)
        .spacing([10.0, 6.0])
        .show(ui, |ui| {
            ui.label("");
            ui.label(egui::RichText::new("Slope").small().weak());
            ui.label(egui::RichText::new("Offset").small().weak());
            ui.end_row();
            for (label, l, col) in rows {
                ui.horizontal(|ui| {
                    let (r, _) =
                        ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                    ui.painter().circle_filled(r.center(), 4.5, col);
                    ui.label(label);
                });
                let mut pct = l.k * 2;
                ui.add(egui::Slider::new(&mut pct, 0..=400).suffix("%"));
                l.k = pct / 2;
                ui.add(egui::Slider::new(&mut l.offset, -255..=255));
                ui.end_row();
            }
        });
}

/// Things a dialog needs from the app that aren't parameters.
pub struct DialogEnv {
    pub img_size: (usize, usize),
    /// Set by the Pattern dialog's "Choose…" button.
    pub choose_pattern: bool,
}

/// Restore the parameters `cmd`'s dialog shows to their defaults.
pub fn reset(cmd: Cmd, p: &mut OpParams, img_size: (usize, usize)) {
    use Cmd::*;
    let d = OpParams::default();
    match cmd {
        RgbTv => p.tv = d.tv,
        RgbLinear => p.linear = d.linear,
        Gamma => {
            p.gamma = d.gamma;
            p.gamma_rgb = d.gamma_rgb;
        }
        FillFluctuated => p.fluct = d.fluct,
        GradientV | GradientH | GradientRadial => p.grad = d.grad,
        Deformations => {
            p.mirror = d.mirror;
            p.distortion = d.distortion;
            p.dsize = d.dsize;
        }
        Rotate => p.angle = d.angle,
        Size => p.new_size = img_size,
        New => p.new_size = d.new_size,
        Paste | PasteFrom => {
            p.paste_logic = d.paste_logic;
            p.paste_level = d.paste_level;
        }
        _ => p.filter = d.filter,
    }
}

/// The body of `cmd`'s parameter dialog.
pub fn dialog_ui(ui: &mut egui::Ui, cmd: Cmd, p: &mut OpParams, env: &mut DialogEnv) {
    use Cmd::*;
    let grid = |id: &str| egui::Grid::new(id).num_columns(2).spacing([12.0, 8.0]);
    match cmd {
        _ if matches!(
            cmd,
            Smoothing | SpotRemoving | Minimum | Maximum | HandDrawing | CleaningBackground
        ) || uses_tile_size(cmd) =>
        {
            let tiles = uses_tile_size(cmd);
            let (mx, my) = if tiles {
                let (a, b) = effects::filter_size_max_tiles(env.img_size.0, env.img_size.1);
                (a.max(FilterSize::MIN), b.max(FilterSize::MIN))
            } else {
                (FilterSize::MAX, FilterSize::MAX)
            };
            widgets::section(ui, if tiles { "Tile size" } else { "Window" });
            ui.horizontal(|ui| {
                grid("fsize").show(ui, |ui| {
                    ui.label("Width");
                    let cw = ui
                        .add(egui::Slider::new(&mut p.filter.w, FilterSize::MIN..=mx).suffix(" px"))
                        .changed();
                    ui.end_row();
                    ui.label("Height");
                    let ch = ui
                        .add(egui::Slider::new(&mut p.filter.h, FilterSize::MIN..=my).suffix(" px"))
                        .changed();
                    ui.end_row();
                    ui.label("");
                    ui.checkbox(&mut p.square_filter, "Square");
                    ui.end_row();
                    if p.square_filter {
                        if cw {
                            p.filter.h = p.filter.w.min(my);
                        } else if ch {
                            p.filter.w = p.filter.h.min(mx);
                        }
                    }
                });
                if !tiles {
                    ui.add_space(8.0);
                    widgets::window_grid(ui, p.filter.w, p.filter.h);
                }
            });
        }
        RgbTv => {
            let (c, b, col) = p.tv;
            let map = ColorMap::from_tv(50 - c, -b, -col);
            ui.horizontal(|ui| {
                widgets::curve_plot(
                    ui,
                    &[(&map.halftone.table(), ui.visuals().text_color())],
                    110.0,
                );
                grid("tv").show(ui, |ui| {
                    ui.label("Contrast");
                    ui.add(egui::Slider::new(&mut p.tv.0, -50..=50));
                    ui.end_row();
                    ui.label("Brightness");
                    ui.add(egui::Slider::new(&mut p.tv.1, -255..=255));
                    ui.end_row();
                    ui.label("Color");
                    ui.add(egui::Slider::new(&mut p.tv.2, -255..=255));
                    ui.end_row();
                });
            });
        }
        RgbLinear => {
            let m = p.linear;
            let tabs = [
                (m.halftone.table(), ui.visuals().text_color()),
                (m.color.table(), Color32::from_rgb(230, 190, 40)),
                (m.red.table(), Color32::from_rgb(230, 70, 70)),
                (m.green.table(), Color32::from_rgb(70, 200, 90)),
                (m.blue.table(), Color32::from_rgb(80, 130, 240)),
            ];
            let curves: Vec<(&[u8; 256], Color32)> = tabs.iter().map(|(t, c)| (t, *c)).collect();
            ui.horizontal(|ui| {
                widgets::curve_plot(ui, &curves, 150.0);
                ui.vertical(|ui| {
                    let [h, c, r, g, b] = [tabs[0].1, tabs[1].1, tabs[2].1, tabs[3].1, tabs[4].1];
                    let l = &mut p.linear;
                    linear_rows(
                        ui,
                        [
                            ("Brightness", &mut l.halftone, h),
                            ("Saturation", &mut l.color, c),
                            ("Red", &mut l.red, r),
                            ("Green", &mut l.green, g),
                            ("Blue", &mut l.blue, b),
                        ],
                    );
                });
            });
        }
        Gamma => {
            let lut = tune::gamma_lut(p.gamma);
            let col = ui.visuals().text_color();
            ui.horizontal(|ui| {
                widgets::curve_plot(ui, &[(&lut, col)], 110.0);
                ui.vertical(|ui| {
                    widgets::section(ui, "Gamma");
                    ui.add(
                        egui::Slider::new(&mut p.gamma, 0.25..=4.0)
                            .logarithmic(true)
                            .fixed_decimals(2),
                    );
                    ui.horizontal(|ui| {
                        for (i, (l, c)) in [
                            ("R", (230, 70, 70)),
                            ("G", (70, 200, 90)),
                            ("B", (80, 130, 240)),
                        ]
                        .into_iter()
                        .enumerate()
                        {
                            // Colored letters on checkboxes: readable on or off.
                            let text = egui::RichText::new(l)
                                .strong()
                                .color(Color32::from_rgb(c.0, c.1, c.2));
                            ui.checkbox(&mut p.gamma_rgb[i], text);
                        }
                        ui.label(egui::RichText::new("channels").weak());
                    });
                });
            });
        }
        FillFluctuated => {
            grid("fluct").show(ui, |ui| {
                ui.label("Color");
                widgets::swatch(ui, &mut p.color, 28.0, false);
                ui.end_row();
                ui.label("Grain size");
                ui.add(egui::Slider::new(&mut p.fluct.grain, 1..=16).suffix(" px"));
                ui.end_row();
                ui.label("Depth");
                ui.add(egui::Slider::new(&mut p.fluct.depth, 1..=100).suffix(" %"));
                ui.end_row();
            });
        }
        GradientV | GradientH | GradientRadial => {
            let (a, b) = match cmd {
                GradientV => ("Bottom", "Top"),
                GradientH => ("Left", "Right"),
                _ => ("Center", "Border"),
            };
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new(a).weak());
                    widgets::swatch(ui, &mut p.grad.0, 32.0, false);
                });
                ui.vertical(|ui| {
                    ui.label("");
                    widgets::gradient_bar(
                        ui,
                        p.grad.0,
                        p.grad.1,
                        cmd == GradientRadial,
                        egui::vec2(170.0, 32.0),
                    );
                });
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new(b).weak());
                    widgets::swatch(ui, &mut p.grad.1, 32.0, false);
                });
            });
            if ui.small_button("⇄ Swap colors").clicked() {
                p.grad = (p.grad.1, p.grad.0);
            }
        }
        PatternTiled | PatternScaled | PatternFitted => {
            widgets::section(ui, "Pattern");
            ui.horizontal(|ui| {
                match &p.pattern {
                    Some(img) => ui.label(format!("{}  ({} × {})", p.pattern_name, img.w, img.h)),
                    None => ui.label(egui::RichText::new("No pattern chosen").weak()),
                };
                if ui.button("Choose image…").clicked() {
                    env.choose_pattern = true;
                }
            });
        }
        Deformations => {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    for (title, kinds) in [
                        ("Mirrors", &MirrorKind::ALL[0..6]),
                        ("Waves", &MirrorKind::ALL[6..9]),
                        ("Whirlpools", &MirrorKind::ALL[9..11]),
                    ] {
                        widgets::section(ui, title);
                        for k in kinds {
                            ui.selectable_value(&mut p.mirror, *k, k.name());
                        }
                    }
                });
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    widgets::section(ui, "Strength");
                    ui.add(
                        egui::Slider::new(&mut p.distortion, 0..=DEFORM_PARAM_MAX)
                            .text("distortion"),
                    );
                    ui.add_enabled(
                        p.mirror.uses_size(),
                        egui::Slider::new(&mut p.dsize, 0..=DEFORM_PARAM_MAX).text("waves"),
                    );
                });
            });
        }
        Rotate => {
            ui.horizontal(|ui| {
                widgets::angle_dial(ui, p.angle, 64.0);
                ui.vertical(|ui| {
                    ui.add(
                        egui::Slider::new(&mut p.angle, -180.0..=180.0)
                            .suffix("°")
                            .fixed_decimals(1),
                    );
                    ui.horizontal(|ui| {
                        for a in [-90.0, 90.0, 180.0] {
                            if ui.small_button(format!("{a:+}°")).clicked() {
                                p.angle = a;
                            }
                        }
                    });
                    ui.label(
                        egui::RichText::new("Counter-clockwise; negative turns clockwise").weak(),
                    );
                });
            });
        }
        Size | New => {
            let (w0, h0) = env.img_size;
            let (mut w, mut h) = p.new_size;
            if cmd == New {
                ui.horizontal(|ui| {
                    for (pw, ph, label) in [
                        (640, 480, "640×480"),
                        (1024, 768, "1024×768"),
                        (1920, 1080, "1920×1080"),
                        (1000, 1000, "Square"),
                    ] {
                        if ui.selectable_label((w, h) == (pw, ph), label).clicked() {
                            (w, h) = (pw, ph);
                        }
                    }
                });
            }
            grid("size").show(ui, |ui| {
                ui.label("Width");
                let cw = ui
                    .add(egui::DragValue::new(&mut w).range(1..=30000).suffix(" px"))
                    .changed();
                ui.end_row();
                ui.label("Height");
                let ch = ui
                    .add(egui::DragValue::new(&mut h).range(1..=30000).suffix(" px"))
                    .changed();
                ui.end_row();
                if cmd == Size {
                    ui.label("");
                    ui.checkbox(&mut p.keep_aspect, "Keep proportions");
                    ui.end_row();
                    if p.keep_aspect && w0 > 0 && h0 > 0 {
                        if cw {
                            h = ((w as f64 * h0 as f64 / w0 as f64).round() as usize).max(1);
                        } else if ch {
                            w = ((h as f64 * w0 as f64 / h0 as f64).round() as usize).max(1);
                        }
                    }
                }
            });
            if cmd == Size {
                ui.horizontal(|ui| {
                    for pct in [25, 50, 200, 400] {
                        if ui.small_button(format!("{pct}%")).clicked() {
                            w = (w0 * pct / 100).max(1);
                            h = (h0 * pct / 100).max(1);
                        }
                    }
                    ui.label(egui::RichText::new(format!("now {w0} × {h0}")).weak());
                });
            }
            p.new_size = (w, h);
            if w.saturating_mul(h) > crate::core::MAX_PIXELS {
                ui.colored_label(
                    Color32::from_rgb(220, 80, 60),
                    "Too large (100 megapixels at most)",
                );
            }
        }
        Paste | PasteFrom => {
            widgets::section(ui, "Transparent");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut p.paste_logic, PasteLogic::Replace, "Nothing");
                ui.selectable_value(&mut p.paste_logic, PasteLogic::BlackTransparent, "Black");
                ui.selectable_value(&mut p.paste_logic, PasteLogic::WhiteTransparent, "White");
            });
            ui.add_enabled(
                p.paste_logic != PasteLogic::Replace,
                egui::Slider::new(&mut p.paste_level, 0..=255).text("tolerance"),
            );
        }
        _ => {}
    }
}

pub fn text_dialog_ui(ui: &mut egui::Ui, p: &mut OpParams, fonts: &[FontEntry]) {
    let name = fonts.get(p.font).map_or("?", |f| f.name.as_str());
    egui::Grid::new("text")
        .num_columns(2)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            ui.label("Font");
            egui::ComboBox::from_id_salt("font")
                .selected_text(name)
                .width(240.0)
                .show_ui(ui, |ui| {
                    for (i, f) in fonts.iter().enumerate() {
                        ui.selectable_value(&mut p.font, i, &f.name);
                    }
                });
            ui.end_row();
            ui.label("Size");
            ui.add(
                egui::Slider::new(&mut p.font_px, 8.0..=400.0)
                    .logarithmic(true)
                    .suffix(" px")
                    .fixed_decimals(0),
            );
            ui.end_row();
        });
    ui.add(
        egui::TextEdit::multiline(&mut p.text)
            .desired_rows(3)
            .desired_width(340.0)
            .hint_text("Text"),
    );
}

fn area_kind(scope: Scope, mask: &Mask) -> tune::AreaKind<'_> {
    match scope {
        Scope::Whole => tune::AreaKind::Whole,
        Scope::Brush { .. } => tune::AreaKind::Pen,
        Scope::Selection { .. } => tune::AreaKind::Mask(mask),
    }
}

/// Fill operations run in place when painting (they accumulate), all other
/// operations read the pre-painting backup.
pub fn is_fill(cmd: Cmd) -> bool {
    use Cmd::*;
    matches!(
        cmd,
        FillPlain
            | FillFluctuated
            | GradientV
            | GradientH
            | GradientRadial
            | PatternTiled
            | PatternScaled
            | PatternFitted
            | PatchFull
            | PatchH
            | PatchV
    )
}

/// When painting, can the operation be computed once for the whole image
/// at the start of the session? True when its result at a pixel doesn't
/// depend on the brush square: local filters and effects, colour tuning and
/// source-independent fills. The others (statistics, gradients, patches and
/// geometry relative to the square) are computed per dab on the square.
pub fn pen_once(cmd: Cmd) -> bool {
    use Cmd::*;
    !matches!(
        cmd,
        Expand
            | Equalization
            | GradientV
            | GradientH
            | GradientRadial
            | PatchFull
            | PatchH
            | PatchV
            | Deformations
            | Rubber
            | FlipH
            | FlipV
            | Move
    )
}

/// Commands that produce a movable fragment when used with a selected area.
pub fn is_fragment_op(cmd: Cmd) -> bool {
    matches!(
        cmd,
        Cmd::FlipH | Cmd::FlipV | Cmd::Move | Cmd::Rotate | Cmd::Deformations | Cmd::Rubber
    )
}

/// Run a pixel operation on `src` over `roi`; returns the full processed
/// image (the caller commits it through the selection weights).
pub fn apply(cmd: Cmd, p: &OpParams, src: &Image, roi: Rect, ctx: &mut Ctx) -> Image {
    use Cmd::*;
    let f = p.filter;
    match cmd {
        Smoothing => filters::smoothing(src, roi, f),
        Sharpening => filters::sharpening(src, roi),
        HeavySharpening => filters::heavy_sharpening(src, roi),
        SpotRemoving => filters::spot_removing(src, roi, f),
        Minimum => filters::minimum(src, roi, f),
        Maximum => filters::maximum(src, roi, f),
        ContourOutlining => filters::contour_outlining(src, roi),
        Emboss => filters::emboss(src, roi, p.color),
        HandDrawing => effects::hand_drawing(src, roi, f.w, f.h),
        CleaningBackground => effects::cleaning_background(src, roi, f.w, f.h),
        Mosaic => effects::mosaic(src, roi, f.w, f.h),
        FacetedGlass => effects::faceted_glass(src, roi, f.w, f.h),
        Scatter => effects::scatter(src, roi, f.w, f.h, ctx.rng),
        RgbTv => {
            let (c, b, col) = p.tv;
            tune::rgb_control(src, roi, &ColorMap::from_tv(50 - c, -b, -col))
        }
        RgbLinear => tune::rgb_control(src, roi, &p.linear),
        Gamma => {
            let [r, g, b] = p.gamma_rgb;
            tune::gamma(src, roi, p.gamma, r, g, b)
        }
        Expand => tune::expand(src, roi, area_kind(ctx.scope, ctx.mask)),
        Equalization => tune::equalize(src, roi, area_kind(ctx.scope, ctx.mask)),
        FillPlain => fill::fill_plain(src, roi, p.color),
        FillFluctuated => fill::fill_fluctuated(src, roi, p.color, p.fluct, ctx.rng),
        GradientV => fill::gradient(src, roi, GradientKind::Vertical, p.grad.0, p.grad.1),
        GradientH => fill::gradient(src, roi, GradientKind::Horizontal, p.grad.0, p.grad.1),
        GradientRadial => {
            let shape = match ctx.scope {
                Scope::Selection { elliptic: true } => RadialShape::Ellipse,
                Scope::Brush { circle: true } => RadialShape::Circle,
                _ => RadialShape::Diagonal,
            };
            fill::gradient(src, roi, GradientKind::Radial(shape), p.grad.0, p.grad.1)
        }
        PatternTiled | PatternScaled | PatternFitted => match &p.pattern {
            Some(pat) if pat.w > 0 && pat.h > 0 => match cmd {
                PatternTiled => fill::pattern_tiled(src, roi, pat),
                PatternScaled => fill::pattern_scaled(src, roi, pat),
                _ => fill::pattern_fitted(src, roi, pat),
            },
            _ => src.clone(),
        },
        PatchFull | PatchH | PatchV => {
            let mode = match cmd {
                PatchFull => PatchMode::Full,
                PatchH => PatchMode::Horizontal,
                _ => PatchMode::Vertical,
            };
            fill::patch(src, roi, ctx.mask, mode)
        }
        FlipH => transform::flip_horizontal(src, roi),
        FlipV => transform::flip_vertical(src, roi),
        Deformations => transform::deform(src, roi, p.mirror, p.distortion, p.dsize),
        Rubber => {
            let (a, b) = p.rubber;
            transform::rubber(
                src,
                roi,
                a,
                b,
                ctx.scope == Scope::Selection { elliptic: true },
                p.color,
            )
        }
        Erase => match ctx.backup {
            Some(b) if b.w == src.w && b.h == src.h => b.clone(),
            _ => src.clone(),
        },
        _ => src.clone(),
    }
}

/// Area mode of the geometric commands: cut the transformed fragment.
pub fn make_fragment(
    cmd: Cmd,
    p: &OpParams,
    src: &Image,
    sel: &Mask,
    roi: Rect,
    elliptic: bool,
) -> Option<Fragment> {
    Some(match cmd {
        Cmd::FlipH => transform::flip_fragment(src, sel, roi, false),
        Cmd::FlipV => transform::flip_fragment(src, sel, roi, true),
        Cmd::Move => transform::move_fragment(src, sel, roi),
        Cmd::Rotate => transform::rotate_fragment(src, sel, roi, p.angle),
        Cmd::Deformations => {
            transform::deform_fragment(src, sel, roi, p.mirror, p.distortion, p.dsize)
        }
        Cmd::Rubber => {
            let (a, b) = p.rubber;
            transform::rubber_fragment(src, sel, roi, a, b, elliptic)
        }
        _ => return None,
    })
}

/// Build a pasted fragment with the Paste dialog's transparency logic.
pub fn paste_fragment(img: Image, p: &OpParams) -> Fragment {
    let mut alpha = Mask::full(img.w, img.h);
    let lvl = p.paste_level as u32;
    for (i, px) in img.px.iter().enumerate() {
        let l = (px[0] as u32 + px[1] as u32 + px[2] as u32) / 3;
        let transparent = match p.paste_logic {
            PasteLogic::Replace => false,
            PasteLogic::BlackTransparent => l <= lvl,
            PasteLogic::WhiteTransparent => l >= 255 - lvl,
        };
        if transparent {
            alpha.data[i] = 0;
        }
    }
    Fragment {
        x: 0,
        y: 0,
        img,
        alpha,
    }
}
