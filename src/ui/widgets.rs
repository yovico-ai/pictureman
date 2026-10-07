//! Small widgets shared by the toolbox and the dialogs, and the app's look.

use eframe::egui::{
    self, Color32, CornerRadius, Pos2, Rect, Sense, Shape, Stroke, Vec2, pos2, vec2,
};

use crate::core::Rgb;

/// The accent color: Picture Man's default "system color" (PMAN.INI
/// RED=0 GREEN=166 BLUE=166).
pub const ACCENT: Color32 = Color32::from_rgb(0, 150, 150);

/// A slightly more contemporary look: rounder corners, more air, the teal
/// accent for selections and the default button.
pub fn apply_style(ctx: &egui::Context) {
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = vec2(8.0, 6.0);
        s.spacing.button_padding = vec2(8.0, 3.0);
        s.spacing.slider_width = 160.0;
        s.visuals.window_corner_radius = CornerRadius::same(10);
        s.visuals.menu_corner_radius = CornerRadius::same(8);
        for w in [
            &mut s.visuals.widgets.noninteractive,
            &mut s.visuals.widgets.inactive,
            &mut s.visuals.widgets.hovered,
            &mut s.visuals.widgets.active,
            &mut s.visuals.widgets.open,
        ] {
            w.corner_radius = CornerRadius::same(5);
        }
        s.visuals.selection.bg_fill =
            ACCENT.gamma_multiply(if s.visuals.dark_mode { 0.75 } else { 0.55 });
        s.visuals.selection.stroke = Stroke::new(
            1.0,
            if s.visuals.dark_mode {
                Color32::WHITE
            } else {
                Color32::BLACK
            },
        );
        s.visuals.hyperlink_color = if s.visuals.dark_mode {
            Color32::from_rgb(80, 210, 210)
        } else {
            ACCENT
        };
        s.visuals.slider_trailing_fill = true;
    });
}

pub fn to_color(c: Rgb) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

/// A square color well; click to edit. With `bevel` it is drawn like the
/// original toolbox's 3D bitmap buttons.
pub fn swatch(ui: &mut egui::Ui, c: &mut Rgb, side: f32, bevel: bool) -> egui::Response {
    let (rect, mut resp) = ui.allocate_exact_size(Vec2::splat(side), Sense::click());
    let p = ui.painter();
    if bevel {
        // Light top-left, dark bottom-right, black outer line: the 1993 look.
        p.rect_filled(rect, 0.0, Color32::BLACK);
        let r = rect.shrink(1.0);
        p.rect_filled(r, 0.0, Color32::from_gray(128));
        p.rect_filled(
            Rect::from_min_max(r.min, r.max - vec2(1.0, 1.0)),
            0.0,
            Color32::WHITE,
        );
        p.rect_filled(r.shrink(1.0), 0.0, Color32::from_gray(192));
        p.rect_filled(r.shrink(3.0), 0.0, to_color(*c));
    } else {
        p.rect_filled(rect, 5.0, to_color(*c));
        p.rect_stroke(
            rect,
            5.0,
            ui.visuals().widgets.inactive.bg_stroke,
            egui::StrokeKind::Inside,
        );
    }
    if resp.hovered() {
        p.rect_stroke(
            rect.expand(1.0),
            2.0,
            Stroke::new(1.0, ui.visuals().hyperlink_color),
            egui::StrokeKind::Outside,
        );
    }
    let popup = egui::Popup::from_toggle_button_response(&resp);
    let mut changed = false;
    popup.show(|ui| {
        let mut col = to_color(*c);
        if egui::color_picker::color_picker_color32(ui, &mut col, egui::color_picker::Alpha::Opaque)
        {
            *c = [col.r(), col.g(), col.b()];
            changed = true;
        }
    });
    if changed {
        resp.mark_changed();
    }
    resp
}

/// A labelled section inside a dialog.
pub fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(title.to_uppercase())
            .small()
            .strong()
            .color(ui.visuals().weak_text_color()),
    );
}

/// A plot of transfer curves (input → output, 0..255) with a grid and the
/// identity diagonal.
pub fn curve_plot(ui: &mut egui::Ui, curves: &[(&[u8; 256], Color32)], size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    let p = ui.painter();
    let v = ui.visuals();
    p.rect_filled(rect, 6.0, v.extreme_bg_color);
    let grid = Stroke::new(
        1.0,
        v.widgets.noninteractive.bg_stroke.color.gamma_multiply(0.6),
    );
    for i in 1..4 {
        let t = i as f32 / 4.0;
        p.line_segment(
            [
                pos2(rect.left() + rect.width() * t, rect.top()),
                pos2(rect.left() + rect.width() * t, rect.bottom()),
            ],
            grid,
        );
        p.line_segment(
            [
                pos2(rect.left(), rect.top() + rect.height() * t),
                pos2(rect.right(), rect.top() + rect.height() * t),
            ],
            grid,
        );
    }
    let at = |i: usize, o: u8| {
        pos2(
            rect.left() + rect.width() * i as f32 / 255.0,
            rect.bottom() - rect.height() * o as f32 / 255.0,
        )
    };
    p.extend(Shape::dashed_line(
        &[rect.left_bottom(), rect.right_top()],
        Stroke::new(1.0, v.weak_text_color()),
        4.0,
        4.0,
    ));
    for (lut, col) in curves {
        let pts: Vec<Pos2> = (0..256).map(|i| at(i, lut[i])).collect();
        p.add(Shape::line(pts, Stroke::new(2.0, *col)));
    }
    p.rect_stroke(
        rect,
        6.0,
        v.widgets.noninteractive.bg_stroke,
        egui::StrokeKind::Inside,
    );
}

/// A preview bar of a two-color gradient (linear, or radial from the
/// middle).
pub fn gradient_bar(ui: &mut egui::Ui, a: Rgb, b: Rgb, radial: bool, size: Vec2) {
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let p = ui.painter();
    let n = 64;
    for i in 0..n {
        let t0 = i as f32 / n as f32;
        let t1 = (i + 1) as f32 / n as f32;
        let t = (t0 + t1) / 2.0;
        let k = if radial { (2.0 * t - 1.0).abs() } else { t };
        let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * k).round() as u8;
        let c = Color32::from_rgb(mix(a[0], b[0]), mix(a[1], b[1]), mix(a[2], b[2]));
        let r = Rect::from_min_max(
            pos2(rect.left() + rect.width() * t0, rect.top()),
            pos2(rect.left() + rect.width() * t1 + 0.5, rect.bottom()),
        );
        p.rect_filled(r, 0.0, c);
    }
    p.rect_stroke(
        rect,
        4.0,
        ui.visuals().widgets.noninteractive.bg_stroke,
        egui::StrokeKind::Inside,
    );
}

/// A tiny diagram of an N×M filter window.
pub fn window_grid(ui: &mut egui::Ui, w: usize, h: usize) {
    let cell = (64.0 / w.max(h) as f32).clamp(3.0, 12.0);
    let (rect, _) = ui.allocate_exact_size(vec2(cell * w as f32, cell * h as f32), Sense::hover());
    let p = ui.painter();
    let v = ui.visuals();
    for y in 0..h {
        for x in 0..w {
            let r = Rect::from_min_size(
                rect.min + vec2(x as f32 * cell, y as f32 * cell),
                Vec2::splat(cell),
            )
            .shrink(0.5);
            let centre = x == w / 2 && y == h / 2;
            p.rect_filled(
                r,
                1.0,
                if centre {
                    v.hyperlink_color
                } else {
                    v.widgets.inactive.bg_fill
                },
            );
        }
    }
}

/// A dial showing an angle (degrees, counter-clockwise).
pub fn angle_dial(ui: &mut egui::Ui, degrees: f64, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    let p = ui.painter();
    let v = ui.visuals();
    let c = rect.center();
    let r = size / 2.0 - 2.0;
    p.circle_filled(c, r, v.extreme_bg_color);
    p.circle_stroke(c, r, v.widgets.noninteractive.bg_stroke);
    p.line_segment([c, c + vec2(r, 0.0)], Stroke::new(1.0, v.weak_text_color()));
    let a = (degrees as f32).to_radians();
    p.line_segment(
        [c, c + vec2(a.cos(), -a.sin()) * r],
        Stroke::new(2.5, v.hyperlink_color),
    );
    p.circle_filled(c, 3.0, v.hyperlink_color);
}
