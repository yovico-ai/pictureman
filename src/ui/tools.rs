//! Tools and their settings, and the toolbox panel drawn with the original
//! button bitmaps.

use eframe::egui::{self, Color32, RichText};

use super::assets::Assets;
use crate::core::Rgb;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Rect,
    Ellipse,
    Polygon,
    Lasso,
    Wand,
    Text,
    Brush,
    Eyedropper,
}

impl Tool {
    pub fn is_selection(self) -> bool {
        !matches!(self, Tool::Brush | Tool::Eyedropper)
    }

    pub fn name(self) -> &'static str {
        match self {
            Tool::Rect => "Rectangle",
            Tool::Ellipse => "Ellipse",
            Tool::Polygon => "Polygon",
            Tool::Lasso => "Lasso",
            Tool::Wand => "Magic wand",
            Tool::Text => "Text",
            Tool::Brush => "Brush",
            Tool::Eyedropper => "Eyedropper",
        }
    }

    /// Single-key shortcut.
    pub fn key(self) -> egui::Key {
        use egui::Key;
        match self {
            Tool::Rect => Key::M,
            Tool::Ellipse => Key::E,
            Tool::Polygon => Key::P,
            Tool::Lasso => Key::L,
            Tool::Wand => Key::W,
            Tool::Text => Key::T,
            Tool::Brush => Key::B,
            Tool::Eyedropper => Key::I,
        }
    }

    pub const ALL: [Tool; 8] = [
        Tool::Rect,
        Tool::Ellipse,
        Tool::Polygon,
        Tool::Lasso,
        Tool::Wand,
        Tool::Text,
        Tool::Brush,
        Tool::Eyedropper,
    ];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Edge {
    Sharp,
    Low,
    Medium,
    High,
}

impl Edge {
    pub fn name(self) -> &'static str {
        match self {
            Edge::Sharp => "Sharp",
            Edge::Low => "Soft",
            Edge::Medium => "Softer",
            Edge::High => "Softest",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Brush {
    Square,
    Circle,
}

pub struct Tools {
    pub tool: Tool,
    /// Soft edge of selections.
    pub edge: Edge,
    /// Brush softness (the original used the selection edge as the pen's
    /// transparency; here they are separate).
    pub brush_edge: Edge,
    pub pen_size: u32,
    pub brush: Brush,
    pub color: Rgb,
    pub wand_tolerance: u8,
    pub wand_hsv: bool,
    pub wand_unifold: bool,
    pub animate: bool,
    pub backup: bool,
}

impl Default for Tools {
    fn default() -> Self {
        // Defaults from the shipped PMAN.INI [MODE] / [COLOR].
        Tools {
            tool: Tool::Rect,
            edge: Edge::Sharp,
            brush_edge: Edge::Sharp,
            pen_size: 11,
            brush: Brush::Circle,
            color: [0, 166, 166],
            wand_tolerance: 51,
            wand_hsv: false,
            wand_unifold: true,
            animate: true,
            backup: true,
        }
    }
}

pub const PEN_SIZES: [u32; 6] = [1, 3, 5, 7, 9, 11];

/// Tool buttons, in the original toolbox's order and bitmaps.
const TOOL_BUTTONS: [(Tool, &str); 7] = [
    (Tool::Rect, "RECT"),
    (Tool::Ellipse, "ELLIPSE"),
    (Tool::Polygon, "POLY"),
    (Tool::Text, "TEXT"),
    (Tool::Wand, "WANG"),
    (Tool::Lasso, "BRUSH"),
    (Tool::Brush, "PEN"),
];

const EDGES: [(Edge, &str); 4] = [
    (Edge::Sharp, "SHARP"),
    (Edge::Low, "LOW"),
    (Edge::Medium, "MED"),
    (Edge::High, "HIGH"),
];

/// One of the original 3D bitmap buttons: UP normally, DOWN when selected.
fn bitmap_button(ui: &mut egui::Ui, assets: &Assets, bmp: &str, on: bool, tip: &str) -> bool {
    let tex = assets.get(&format!("bmp_{bmp}{}", if on { "DOWN" } else { "UP" }));
    ui.add(egui::Button::image(egui::Image::new(tex)).frame(false))
        .on_hover_text(tip)
        .clicked()
}

fn pair_button(
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
    ui.add_space(8.0);
    ui.add(egui::Image::new(assets.get(label_bitmap)));
}

/// What the toolbox asks the app to do besides changing settings.
pub enum ToolboxAction {
    None,
    /// The "whole image" button: drop the selection.
    Deselect,
}

pub fn toolbox(
    ui: &mut egui::Ui,
    assets: &Assets,
    t: &mut Tools,
    has_selection: bool,
) -> ToolboxAction {
    let mut action = ToolboxAction::None;
    ui.spacing_mut().item_spacing = egui::vec2(2.0, 2.0);
    ui.add(egui::Image::new(assets.get("bmp_ACTION")));

    section(ui, assets, "bmp_AREA");
    egui::Grid::new("tools").spacing([2.0, 2.0]).show(ui, |ui| {
        // The original "whole image" button: no selection = whole image.
        if bitmap_button(
            ui,
            assets,
            "WHOLE",
            !has_selection,
            "Whole image — clear the selection (Ctrl+D)",
        ) {
            action = ToolboxAction::Deselect;
        }
        for (i, (tool, bmp)) in TOOL_BUTTONS.iter().enumerate() {
            let tip = format!("{} ({})", tool.name(), tool.key().name());
            if bitmap_button(ui, assets, bmp, t.tool == *tool, &tip) {
                t.tool = *tool;
            }
            if i % 2 == 0 {
                ui.end_row();
            }
        }
    });

    section(ui, assets, "bmp_EDGE");
    egui::Grid::new("edges").spacing([2.0, 2.0]).show(ui, |ui| {
        for (i, (e, bmp)) in EDGES.iter().enumerate() {
            let tip = format!("Edge: {}", e.name());
            if bitmap_button(ui, assets, bmp, t.edge == *e, &tip) {
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
        if pair_button(ui, assets, "bmp_SQPEN0", "bmp_SQPEN1", sq, "Square brush") {
            t.brush = Brush::Square;
        }
        if pair_button(ui, assets, "bmp_CIRPEN0", "bmp_CIRPEN1", !sq, "Round brush") {
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
        let on = t.tool == Tool::Eyedropper;
        if pair_button(
            ui,
            assets,
            "bmp_PICKUP",
            "bmp_PICKDOWN",
            on,
            "Eyedropper (I)",
        ) {
            t.tool = Tool::Eyedropper;
        }
    });
    ui.label(
        RichText::new(format!("{} {} {}", t.color[0], t.color[1], t.color[2]))
            .small()
            .weak(),
    );
    action
}
