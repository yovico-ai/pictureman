//! The image view: zoomed display, area selection, pen strokes, the Rubber
//! drag, fragment placement and the color pick-up tool.
//!
//! Workflow, as in the original: choose a command; with an area tool other
//! than Whole image, outline the area, then double-click inside it to process
//! the interior or outside it to process the exterior. Esc cancels.

use std::sync::Arc;

use eframe::egui::{
    self, Color32, Pos2, Rect, Sense, Shape as EShape, Stroke, TextureId, Vec2, pos2, vec2,
};

use super::doc::Doc;
use super::tools::{Area, Brush, Tools};
use crate::core::{Mask, Rgb};

/// Selection outlines move like the original's: the four 8×8 TILE brushes
/// (45° stripes, 4 px black / 4 px white, each shifted 2 rows from the last)
/// selected in turn by a 200 ms timer (`seg30:041b`, `SetTimer(…, 200)`), and
/// blitted through the outline with ROP DSPDxax.
pub const MARCH_MS: u64 = 200;

/// Pattern pixel of TILE`phase` at (x, y): black when `(x + y − 2·phase) mod 8 < 4`.
pub fn tile_black(x: i64, y: i64, phase: usize) -> bool {
    (x + y - 2 * phase as i64).rem_euclid(8) < 4
}

/// Current pattern phase (0..=3), or 0 when "Animate selection" is off.
pub fn march_phase(time: f64, animate: bool) -> usize {
    if animate {
        ((time * 1000.0 / MARCH_MS as f64) as usize) & 3
    } else {
        0
    }
}

/// An outlined area, in image coordinates.
#[derive(Clone, Debug)]
pub enum Shape {
    Rect(Pos2, Pos2),
    Ellipse(Pos2, Pos2),
    Polygon(Vec<Pos2>),
    Freehand(Vec<Pos2>),
    /// A rendered text mask with its top-left corner.
    Text(Arc<Mask>, Pos2),
}

impl Shape {
    /// Rasterize with the original's rules (see `crate::selection`).
    pub fn to_mask(&self, w: usize, h: usize, pen: usize) -> Mask {
        use crate::selection as s;
        let ip = |p: Pos2| (p.x.round() as i32, p.y.round() as i32);
        match self {
            Shape::Rect(a, b) => s::rasterize_rect(w, h, ip(*a), ip(*b)),
            Shape::Ellipse(a, b) => {
                let r = Rect::from_two_pos(*a, *b);
                let c = ip(r.center());
                s::rasterize_ellipse(
                    w,
                    h,
                    c.0,
                    c.1,
                    (r.width() / 2.0) as i32,
                    (r.height() / 2.0) as i32,
                )
            }
            Shape::Polygon(v) => {
                s::rasterize_polygon(w, h, &v.iter().map(|p| ip(*p)).collect::<Vec<_>>())
            }
            Shape::Freehand(v) => {
                // The original paints the outline with the pen and fills it.
                let pts: Vec<(i32, i32)> = v.iter().map(|p| ip(*p)).collect();
                let mut m = s::rasterize_polygon(w, h, &pts);
                let brush = s::Brush {
                    size: pen.max(1),
                    kind: s::BrushKind::Circle,
                };
                for p in &pts {
                    s::freehand_dab(&mut m, *p, &brush, false);
                }
                m
            }
            Shape::Text(tm, at) => {
                let mut m = Mask::empty(w, h);
                let (ox, oy) = (at.x.round() as i64, at.y.round() as i64);
                for y in 0..tm.h {
                    for x in 0..tm.w {
                        let (ix, iy) = (ox + x as i64, oy + y as i64);
                        if tm.get(x, y) != 0
                            && ix >= 0
                            && iy >= 0
                            && (ix as usize) < w
                            && (iy as usize) < h
                        {
                            m.set(ix as usize, iy as usize, 255);
                        }
                    }
                }
                m
            }
        }
    }

    /// Bounding box in image coordinates (for the feather width).
    pub fn bounds(&self) -> Rect {
        match self {
            Shape::Rect(a, b) | Shape::Ellipse(a, b) => Rect::from_two_pos(*a, *b),
            Shape::Polygon(v) | Shape::Freehand(v) => Rect::from_points(v),
            Shape::Text(m, at) => Rect::from_min_size(*at, vec2(m.w as f32, m.h as f32)),
        }
    }

    /// Outline points for vector display.
    fn outline(&self) -> Vec<Pos2> {
        let rect_pts = |r: Rect| {
            vec![
                r.left_top(),
                r.right_top(),
                r.right_bottom(),
                r.left_bottom(),
                r.left_top(),
            ]
        };
        match self {
            Shape::Rect(a, b) => rect_pts(Rect::from_two_pos(*a, *b)),
            Shape::Text(..) => rect_pts(self.bounds()),
            Shape::Ellipse(a, b) => {
                let r = Rect::from_two_pos(*a, *b);
                (0..=64)
                    .map(|i| {
                        let t = i as f32 / 64.0 * std::f32::consts::TAU;
                        r.center() + vec2(t.cos(), t.sin()) * r.size() / 2.0
                    })
                    .collect()
            }
            Shape::Polygon(v) | Shape::Freehand(v) => {
                let mut v = v.clone();
                if let Some(&f) = v.first() {
                    v.push(f);
                }
                v
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Handle {
    Move,
    L,
    R,
    T,
    B,
    LT,
    RT,
    LB,
    RB,
}

/// Interaction state that lives across frames.
#[derive(Default)]
pub struct Selecting {
    pub shape: Option<Shape>,
    /// Polygon still being built (clicks add vertices).
    building: bool,
    drag_from: Option<Pos2>,
    /// Text box being dragged: offset of the pointer inside it.
    text_grab: Option<Vec2>,
    /// Fragment placement: active handle and the rect when the drag began.
    place_drag: Option<(Handle, Rect, Pos2)>,
    rubber_from: Option<Pos2>,
    pen_erasing: bool,
}

impl Selecting {
    /// A complete area is outlined (a polygon must be closed).
    pub fn ready(&self) -> bool {
        self.shape.is_some() && !self.building
    }

    pub fn clear(&mut self) {
        *self = Selecting::default();
    }
}

/// What the canvas is doing.
#[derive(Clone, Copy)]
pub enum Mode {
    Idle,
    Select(Area),
    Pen,
    /// Rubber: drag a reference point inside `roi` (image coords).
    Rubber {
        roi: Rect,
        from: Option<Pos2>,
        to: Option<Pos2>,
    },
    /// Place a fragment: `rect` in image coords, `orig` = original size.
    Place {
        rect: Rect,
        tex: TextureId,
        orig: Vec2,
    },
}

pub enum Event {
    /// Double-click on the outlined area: the app decides from the area's
    /// mask whether the interior or the exterior is processed.
    ApplyAt(Pos2),
    Cancel,
    Picked(Rgb),
    WandSeed(Pos2),
    /// First click of the Text area: where the text goes (left-bottom corner).
    TextAt(Pos2),
    PenDown {
        erase: bool,
    },
    PenDab(Pos2),
    PenUp,
    /// Shift-click in pen mode: the clone reference point (Move/"Clone").
    CloneRef(Pos2),
    RubberSet(Pos2, Pos2),
    RubberApply,
    PlaceSet(Rect),
    PlaceAccept,
    Hover(Option<(usize, usize)>),
}

/// Overlays drawn above the image (texture covering the whole image).
pub struct Overlay {
    pub tex: Option<TextureId>,
    /// Text-area outline: texture and its rect in image coordinates.
    pub text: Option<(TextureId, Rect)>,
}

pub fn canvas(
    ui: &mut egui::Ui,
    doc: &mut Doc,
    sel: &mut Selecting,
    mode: Mode,
    tools: &Tools,
    overlay: Overlay,
) -> Vec<Event> {
    let mut ev = Vec::new();
    let zoom = doc.zoom;
    let size = vec2(doc.img.w as f32, doc.img.h as f32) * zoom;
    let tex = doc.texture(ui.ctx()).id();
    let (iw, ih) = (doc.img.w, doc.img.h);

    egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
        // Center small images in the view.
        let avail = ui.available_size();
        let pad = ((avail - size) / 2.0).max(Vec2::ZERO);
        ui.add_space(pad.y);
        ui.horizontal(|ui| {
            ui.add_space(pad.x);
            let (resp, painter) = ui.allocate_painter(size, Sense::click_and_drag());
            let origin = resp.rect.min;
            let to_img = |p: Pos2| pos2((p.x - origin.x) / zoom, (p.y - origin.y) / zoom);
            let to_scr = |p: Pos2| origin + p.to_vec2() * zoom;
            let full_uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));

            painter.rect_filled(resp.rect.expand(1.0), 0.0, Color32::from_gray(20));
            painter.image(tex, resp.rect, full_uv, Color32::WHITE);
            if let Some(t) = overlay.tex {
                painter.image(t, resp.rect, full_uv, Color32::WHITE);
            }
            if let Some((t, r)) = overlay.text {
                painter.image(
                    t,
                    Rect::from_min_max(to_scr(r.min), to_scr(r.max)),
                    full_uv,
                    Color32::WHITE,
                );
            }

            let hover = resp
                .hover_pos()
                .map(to_img)
                .filter(|p| p.x >= 0.0 && p.y >= 0.0 && (p.x as usize) < iw && (p.y as usize) < ih);
            ev.push(Event::Hover(hover.map(|p| (p.x as usize, p.y as usize))));

            if ui.input(|i| i.key_pressed(egui::Key::Escape)) && !matches!(mode, Mode::Idle) {
                ev.push(Event::Cancel);
            }

            if tools.picking {
                if resp.clicked()
                    && let Some(p) = hover
                {
                    ev.push(Event::Picked(doc.img.get(p.x as usize, p.y as usize)));
                }
                resp.on_hover_cursor(egui::CursorIcon::Crosshair);
                return;
            }

            let ptr = resp.interact_pointer_pos().map(to_img);
            // Where the button went down: drags are reported only after the
            // pointer has moved a few pixels.
            let press = ui.input(|i| i.pointer.press_origin()).map(to_img).or(ptr);
            let mods = ui.input(|i| i.modifiers);
            let phase = march_phase(ui.input(|i| i.time), tools.animate);
            let ppp = ui.ctx().pixels_per_point();
            // The original's selection outline: 1-pixel line painted with the
            // TILE pattern, aligned to the screen.
            let ants = |pts: &[Pos2]| {
                let mut mesh = egui::Mesh::default();
                let mut last = (i64::MIN, i64::MIN);
                for w in pts.windows(2) {
                    let (a, b) = (to_scr(w[0]) * ppp, to_scr(w[1]) * ppp);
                    let n = (a.distance(b).ceil() as usize).max(1);
                    for k in 0..=n {
                        let p = a.lerp(b, k as f32 / n as f32);
                        let px = (p.x.floor() as i64, p.y.floor() as i64);
                        if px == last {
                            continue;
                        }
                        last = px;
                        let c = if tile_black(px.0, px.1, phase) {
                            Color32::BLACK
                        } else {
                            Color32::WHITE
                        };
                        let r = Rect::from_min_size(
                            pos2(px.0 as f32, px.1 as f32) / ppp,
                            Vec2::splat(1.0 / ppp),
                        );
                        mesh.add_colored_rect(r, c);
                    }
                }
                painter.add(EShape::mesh(mesh));
            };
            let mut animate = false;

            match mode {
                Mode::Idle => {}
                Mode::Pen => {
                    let secondary = ui.input(|i| i.pointer.secondary_down());
                    if mods.shift && resp.clicked() {
                        if let Some(p) = ptr {
                            ev.push(Event::CloneRef(p));
                        }
                    } else {
                        if resp.drag_started() || (resp.clicked() || resp.secondary_clicked()) {
                            sel.pen_erasing = secondary || resp.secondary_clicked();
                            ev.push(Event::PenDown {
                                erase: sel.pen_erasing,
                            });
                        }
                        if (resp.dragged()
                            || resp.drag_started()
                            || resp.clicked()
                            || resp.secondary_clicked())
                            && let Some(p) = ptr.or_else(|| resp.hover_pos().map(to_img))
                        {
                            ev.push(Event::PenDab(p));
                        }
                        if resp.drag_stopped() || resp.clicked() || resp.secondary_clicked() {
                            ev.push(Event::PenUp);
                        }
                    }
                    if let Some(p) = resp.hover_pos() {
                        let r = tools.pen_size as f32 * zoom / 2.0;
                        let stroke = Stroke::new(1.0, Color32::WHITE);
                        match tools.brush {
                            Brush::Circle => painter.circle_stroke(p, r, stroke),
                            Brush::Square => painter.rect_stroke(
                                Rect::from_center_size(p, Vec2::splat(2.0 * r)),
                                0.0,
                                stroke,
                                egui::StrokeKind::Middle,
                            ),
                        };
                        painter.circle_stroke(p, r + 1.0, Stroke::new(1.0, Color32::BLACK));
                    }
                    resp.clone().on_hover_cursor(egui::CursorIcon::None);
                }
                Mode::Select(area) => {
                    select_interaction(&resp, sel, area, ptr, press, &mut ev);
                    if let Some(shape) = &sel.shape {
                        let mut pts = shape.outline();
                        if sel.building {
                            pts.pop();
                            if let Some(h) = resp.hover_pos() {
                                pts.push(to_img(h));
                            }
                        }
                        ants(&pts);
                        animate = true;
                    }
                    resp.clone().on_hover_cursor(match area {
                        Area::Text if sel.shape.is_some() => egui::CursorIcon::Move,
                        Area::Text => egui::CursorIcon::Text,
                        _ => egui::CursorIcon::Crosshair,
                    });
                }
                Mode::Rubber { roi, from, to } => {
                    if resp.drag_started() {
                        sel.rubber_from = press;
                    }
                    if let (Some(a), Some(b)) = (sel.rubber_from, ptr)
                        && resp.dragged()
                    {
                        ev.push(Event::RubberSet(a, b));
                    }
                    if resp.double_clicked() {
                        ev.push(Event::RubberApply);
                    }
                    // Grid deformed by the drag: each node moves by the
                    // displacement weighted like the original's influence.
                    let n = 10;
                    let (a, b) = (from.unwrap_or(roi.center()), to.unwrap_or(roi.center()));
                    let d = b - a;
                    let warp = |p: Pos2| -> Pos2 {
                        let infl = |i: f32, pv: f32, lo: f32, hi: f32| -> f32 {
                            if i <= lo || i >= hi {
                                return 0.0;
                            }
                            if i <= pv {
                                let (u, v) = (i - lo, pv - i);
                                u * u / (u * u + v * v).max(1e-6)
                            } else {
                                let (u, v) = (hi - i, i - pv);
                                u * u / (u * u + v * v).max(1e-6)
                            }
                        };
                        let wx = infl(p.x, a.x, roi.left(), roi.right());
                        let wy = infl(p.y, a.y, roi.top(), roi.bottom());
                        p + d * (wx * wy)
                    };
                    let line = Stroke::new(1.0, Color32::from_white_alpha(170));
                    for i in 0..=n {
                        let f = i as f32 / n as f32;
                        let hl: Vec<Pos2> = (0..=40)
                            .map(|k| {
                                to_scr(warp(pos2(
                                    roi.left() + roi.width() * k as f32 / 40.0,
                                    roi.top() + roi.height() * f,
                                )))
                            })
                            .collect();
                        let vl: Vec<Pos2> = (0..=40)
                            .map(|k| {
                                to_scr(warp(pos2(
                                    roi.left() + roi.width() * f,
                                    roi.top() + roi.height() * k as f32 / 40.0,
                                )))
                            })
                            .collect();
                        painter.add(EShape::line(hl, line));
                        painter.add(EShape::line(vl, line));
                    }
                    if let (Some(a), Some(b)) = (from, to) {
                        painter.arrow(
                            to_scr(a),
                            (b - a) * zoom,
                            Stroke::new(2.0, Color32::from_rgb(255, 210, 0)),
                        );
                    }
                    resp.clone().on_hover_cursor(egui::CursorIcon::Grab);
                }
                Mode::Place {
                    rect,
                    tex: ftex,
                    orig,
                } => {
                    let srect = Rect::from_min_max(to_scr(rect.min), to_scr(rect.max));
                    painter.image(ftex, srect, full_uv, Color32::WHITE);
                    let corners = [
                        rect.left_top(),
                        rect.right_top(),
                        rect.right_bottom(),
                        rect.left_bottom(),
                        rect.left_top(),
                    ];
                    ants(&corners);
                    animate = true;
                    for c in &corners[..4] {
                        painter.rect_filled(
                            Rect::from_center_size(to_scr(*c), Vec2::splat(7.0)),
                            0.0,
                            Color32::WHITE,
                        );
                        painter.rect_stroke(
                            Rect::from_center_size(to_scr(*c), Vec2::splat(7.0)),
                            0.0,
                            Stroke::new(1.0, Color32::BLACK),
                            egui::StrokeKind::Middle,
                        );
                    }
                    let handle_at = |p: Pos2| -> Option<Handle> {
                        let tol = 6.0 / zoom;
                        let (l, r, t, b) = (
                            (p.x - rect.left()).abs() < tol,
                            (p.x - rect.right()).abs() < tol,
                            (p.y - rect.top()).abs() < tol,
                            (p.y - rect.bottom()).abs() < tol,
                        );
                        let inside_x = p.x > rect.left() - tol && p.x < rect.right() + tol;
                        let inside_y = p.y > rect.top() - tol && p.y < rect.bottom() + tol;
                        Some(match (l, r, t, b) {
                            (true, _, true, _) => Handle::LT,
                            (_, true, true, _) => Handle::RT,
                            (true, _, _, true) => Handle::LB,
                            (_, true, _, true) => Handle::RB,
                            (true, ..) if inside_y => Handle::L,
                            (_, true, ..) if inside_y => Handle::R,
                            (_, _, true, _) if inside_x => Handle::T,
                            (_, _, _, true) if inside_x => Handle::B,
                            _ if rect.contains(p) => Handle::Move,
                            _ => return None,
                        })
                    };
                    let hover_h = resp.hover_pos().map(to_img).and_then(handle_at);
                    resp.clone().on_hover_cursor(match hover_h {
                        Some(Handle::Move) => egui::CursorIcon::Move,
                        Some(Handle::L | Handle::R) => egui::CursorIcon::ResizeHorizontal,
                        Some(Handle::T | Handle::B) => egui::CursorIcon::ResizeVertical,
                        Some(Handle::LT | Handle::RB) => egui::CursorIcon::ResizeNwSe,
                        Some(Handle::RT | Handle::LB) => egui::CursorIcon::ResizeNeSw,
                        None => egui::CursorIcon::Default,
                    });
                    if resp.double_clicked() {
                        if ptr.is_some_and(|p| rect.contains(p)) {
                            ev.push(Event::PlaceAccept);
                        }
                    } else if resp.clicked() && mods.shift {
                        // Shift-click: back to the fragment's original size.
                        ev.push(Event::PlaceSet(Rect::from_min_size(rect.min, orig)));
                    } else if resp.drag_started() {
                        if let Some(p) = press {
                            sel.place_drag = handle_at(p).map(|h| (h, rect, p));
                        }
                    } else if resp.dragged()
                        && let (Some((h, r0, p0)), Some(p)) = (sel.place_drag, ptr)
                    {
                        ev.push(Event::PlaceSet(drag_rect(
                            h,
                            r0,
                            p - p0,
                            mods.command,
                            orig,
                        )));
                    }
                    if resp.drag_stopped() {
                        sel.place_drag = None;
                    }
                }
            }
            if animate && tools.animate {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(MARCH_MS));
            }
        });
    });
    ev
}

/// New placement rect for a handle drag; Ctrl keeps proportions.
fn drag_rect(h: Handle, r: Rect, d: Vec2, proportional: bool, orig: Vec2) -> Rect {
    let (mut l, mut t, mut rr, mut b) = (r.left(), r.top(), r.right(), r.bottom());
    match h {
        Handle::Move => return r.translate(d),
        Handle::L => l += d.x,
        Handle::R => rr += d.x,
        Handle::T => t += d.y,
        Handle::B => b += d.y,
        Handle::LT => {
            l += d.x;
            t += d.y;
        }
        Handle::RT => {
            rr += d.x;
            t += d.y;
        }
        Handle::LB => {
            l += d.x;
            b += d.y;
        }
        Handle::RB => {
            rr += d.x;
            b += d.y;
        }
    }
    if rr - l < 1.0 {
        rr = l + 1.0;
    }
    if b - t < 1.0 {
        b = t + 1.0;
    }
    let mut out = Rect::from_min_max(pos2(l, t), pos2(rr, b));
    if proportional && orig.x > 0.0 && orig.y > 0.0 {
        let s = (out.width() / orig.x).max(out.height() / orig.y);
        out = Rect::from_min_size(out.min, orig * s);
    }
    out
}

fn select_interaction(
    resp: &egui::Response,
    sel: &mut Selecting,
    area: Area,
    ptr: Option<Pos2>,
    press: Option<Pos2>,
    ev: &mut Vec<Event>,
) {
    // Double-click inside/outside a finished area starts processing.
    if resp.double_clicked() {
        if sel.building {
            // Double-click also closes a polygon (drop the vertex the first click added).
            if let Some(Shape::Polygon(v)) = &mut sel.shape {
                v.pop();
            }
            sel.building = false;
            return;
        }
        if let Some(p) = ptr
            && (sel.shape.is_some() || area == Area::MagicWand)
        {
            ev.push(Event::ApplyAt(p));
        }
        return;
    }

    match area {
        Area::Rect | Area::Ellipse => {
            if resp.drag_started() {
                sel.drag_from = press;
            }
            if let (Some(a), Some(b)) = (sel.drag_from, ptr)
                && resp.dragged()
            {
                sel.shape = Some(if area == Area::Rect {
                    Shape::Rect(a, b)
                } else {
                    Shape::Ellipse(a, b)
                });
            }
            if resp.drag_stopped() {
                sel.drag_from = None;
            }
        }
        Area::Freehand => {
            if resp.drag_started() {
                sel.shape = press.map(|p| Shape::Freehand(vec![p]));
            }
            if resp.dragged()
                && let (Some(Shape::Freehand(v)), Some(p)) = (&mut sel.shape, ptr)
                && v.last().is_none_or(|l| l.distance(p) >= 1.0)
            {
                v.push(p);
            }
            // Right button erases the last part of the outline.
            if resp.secondary_clicked()
                && let Some(Shape::Freehand(v)) = &mut sel.shape
            {
                let keep = v.len().saturating_sub(20);
                v.truncate(keep);
            }
        }
        Area::Polygon => {
            if resp.clicked()
                && let Some(p) = ptr
            {
                match (&mut sel.shape, sel.building) {
                    (Some(Shape::Polygon(v)), true) => v.push(p),
                    _ => {
                        sel.shape = Some(Shape::Polygon(vec![p]));
                        sel.building = true;
                    }
                }
            }
        }
        Area::MagicWand => {
            if resp.clicked()
                && let Some(p) = ptr
            {
                ev.push(Event::WandSeed(p));
            }
        }
        Area::Text => match &mut sel.shape {
            None => {
                if resp.clicked()
                    && let Some(p) = ptr
                {
                    ev.push(Event::TextAt(p));
                }
            }
            Some(Shape::Text(_, at)) => {
                if resp.drag_started() {
                    sel.text_grab = press.map(|p| p - *at);
                }
                if let (Some(g), Some(p)) = (sel.text_grab, ptr)
                    && resp.dragged()
                {
                    *at = p - g;
                }
                if resp.drag_stopped() {
                    sel.text_grab = None;
                }
            }
            Some(_) => {}
        },
        Area::Whole | Area::Pen => {}
    }
}
