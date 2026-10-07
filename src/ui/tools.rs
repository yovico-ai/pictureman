//! Toolbox state (Options menu / "Processing tools" window) and the
//! toolbox panel itself, drawn with the original button bitmaps.

use eframe::egui::{self, Color32, RichText};

use super::assets::Assets;
use crate::core::Rgb;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Area {
    Whole,
    Rect,
    Ellipse,
    Polygon,
    Text,
    MagicWand,
    Freehand,
    Pen,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Sharp,
    Low,
    Medium,
    High,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Brush {
    Square,
    Circle,
}

pub struct Tools {
    pub area: Area,
    pub edge: Edge,
    pub pen_size: u32,
    pub brush: Brush,
    pub color: Rgb,
    pub wand_tolerance: u8,
    pub wand_hsv: bool,
    pub wand_unifold: bool,
    pub preserve_mask: bool,
    pub animate: bool,
    pub backup: bool,
    pub picking: bool,
}

impl Default for Tools {
    fn default() -> Self {
        // Defaults from the shipped PMAN.INI [MODE] / [COLOR].
        Tools {
            area: Area::Whole,
            edge: Edge::Sharp,
            pen_size: 11,
            brush: Brush::Square,
            color: [0, 166, 166],
            wand_tolerance: 51,
            wand_hsv: false,
            wand_unifold: true,
            preserve_mask: false,
            animate: true,
            backup: true,
            picking: false,
        }
    }
}

pub const PEN_SIZES: [u32; 6] = [1, 3, 5, 7, 9, 11];

const AREAS: [(Area, &str, &str); 8] = [
    (Area::Whole, "WHOLE", "Whole image"),
    (Area::Rect, "RECT", "Rectangle"),
    (Area::Ellipse, "ELLIPSE", "Ellipse"),
    (Area::Polygon, "POLY", "Polygon"),
    (Area::Text, "TEXT", "Text"),
    (Area::MagicWand, "WANG", "Magic wand"),
    (Area::Freehand, "BRUSH", "Freehand"),
    (Area::Pen, "PEN", "Pen — paint with the operation"),
];

const EDGES: [(Edge, &str, &str); 4] = [
    (Edge::Sharp, "SHARP", "Sharp edge"),
    (Edge::Low, "LOW", "Smooth edge — low"),
    (Edge::Medium, "MED", "Smooth edge — medium"),
    (Edge::High, "HIGH", "Smooth edge — high"),
];

/// One of the original 3D bitmap buttons: the UP bitmap normally, DOWN when
/// selected.
fn bitmap_button(
    ui: &mut egui::Ui,
    assets: &Assets,
    up: &str,
    down: &str,
    on: bool,
    tip: &str,
) -> bool {
    let tex = assets.get(if on { down } else { up });
    ui.add(egui::Button::image(egui::Image::new(tex)).frame(false))
        .on_hover_text(tip)
        .clicked()
}

fn section(ui: &mut egui::Ui, assets: &Assets, label_bitmap: &str) {
    ui.add_space(6.0);
    ui.add(egui::Image::new(assets.get(label_bitmap)));
}

pub fn toolbox(ui: &mut egui::Ui, assets: &Assets, t: &mut Tools) {
    ui.spacing_mut().item_spacing = egui::vec2(2.0, 2.0);
    ui.add(egui::Image::new(assets.get("bmp_ACTION")));

    section(ui, assets, "bmp_AREA");
    egui::Grid::new("areas").spacing([2.0, 2.0]).show(ui, |ui| {
        for (i, (a, bmp, tip)) in AREAS.iter().enumerate() {
            let (up, down) = (format!("bmp_{bmp}UP"), format!("bmp_{bmp}DOWN"));
            if bitmap_button(ui, assets, &up, &down, t.area == *a, tip) {
                t.area = *a;
            }
            if i % 2 == 1 {
                ui.end_row();
            }
        }
    });

    section(ui, assets, "bmp_EDGE");
    egui::Grid::new("edges").spacing([2.0, 2.0]).show(ui, |ui| {
        for (i, (e, bmp, tip)) in EDGES.iter().enumerate() {
            let (up, down) = (format!("bmp_{bmp}UP"), format!("bmp_{bmp}DOWN"));
            if bitmap_button(ui, assets, &up, &down, t.edge == *e, tip) {
                t.edge = *e;
            }
            if i % 2 == 1 {
                ui.end_row();
            }
        }
    });

    section(ui, assets, "bmp_PENSIZE");
    ui.horizontal(|ui| {
        let sq = t.brush == Brush::Square;
        if bitmap_button(ui, assets, "bmp_SQPEN0", "bmp_SQPEN1", sq, "Square brush") {
            t.brush = Brush::Square;
        }
        if bitmap_button(
            ui,
            assets,
            "bmp_CIRPEN0",
            "bmp_CIRPEN1",
            !sq,
            "Circular brush",
        ) {
            t.brush = Brush::Circle;
        }
    });
    egui::ComboBox::from_id_salt("pensize")
        .width(56.0)
        .selected_text(format!("{0}×{0}", t.pen_size))
        .show_ui(ui, |ui| {
            for s in PEN_SIZES {
                ui.selectable_value(&mut t.pen_size, s, format!("{s}×{s}"));
            }
        });

    section(ui, assets, "bmp_COLOR");
    ui.horizontal(|ui| {
        let [r, g, b] = t.color;
        let mut c = Color32::from_rgb(r, g, b);
        if egui::color_picker::color_edit_button_srgba(
            ui,
            &mut c,
            egui::color_picker::Alpha::Opaque,
        )
        .changed()
        {
            t.color = [c.r(), c.g(), c.b()];
        }
        if bitmap_button(
            ui,
            assets,
            "bmp_PICKUP",
            "bmp_PICKDOWN",
            t.picking,
            "Pick up color from image",
        ) {
            t.picking = !t.picking;
        }
    });
    ui.label(
        RichText::new(format!(
            "{r} {g} {b}",
            r = t.color[0],
            g = t.color[1],
            b = t.color[2]
        ))
        .small()
        .weak(),
    );
}
