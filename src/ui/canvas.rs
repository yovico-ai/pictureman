//! The image view: display and zoom, drawing selections, the brush, the
//! eyedropper, the Rubber drag and fragment placement.

use eframe::egui::{
    self, Color32, Pos2, Rect, Sense, Shape as EShape, Stroke, TextureId, Vec2, pos2, vec2,
};

use super::doc::Doc;
use super::marquee::{Combine, Selection};
use super::tools::{Brush, Tool, Tools};
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

/// A shape being drawn, in image coordinates.
#[derive(Clone, Debug)]
pub enum Shape {
    Rect(Pos2, Pos2),
    Ellipse(Pos2, Pos2),
    Polygon(Vec<Pos2>),
    Lasso(Vec<Pos2>),
}

impl Shape {
    /// Rasterize with the original's rules (see `crate::selection`).
    pub fn to_mask(&self, w: usize, h: usize) -> Mask {
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
            Shape::Polygon(v) | Shape::Lasso(v) => {
                s::rasterize_polygon(w, h, &v.iter().map(|p| ip(*p)).collect::<Vec<_>>())
            }
        }
    }

    pub fn is_degenerate(&self) -> bool {
        match self {
            Shape::Rect(a, b) | Shape::Ellipse(a, b) => {
                (a.x - b.x).abs() < 1.0 || (a.y - b.y).abs() < 1.0
            }
            Shape::Polygon(v) | Shape::Lasso(v) => v.len() < 3,
        }
    }

    /// Outline points for display while drawing.
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
            Shape::Ellipse(a, b) => {
                let r = Rect::from_two_pos(*a, *b);
                (0..=96)
                    .map(|i| {
                        let t = i as f32 / 96.0 * std::f32::consts::TAU;
                        r.center() + vec2(t.cos(), t.sin()) * r.size() / 2.0
                    })
                    .collect()
            }
            Shape::Polygon(v) | Shape::Lasso(v) => {
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
pub struct Interaction {
    shape: Option<Shape>,
    combine: Option<Combine>,
    /// Polygon still being built (clicks add vertices).
    building: bool,
    drag_from: Option<Pos2>,
    /// Moving the selection: where the drag began.
    moving_from: Option<Pos2>,
    place_drag: Option<(Handle, Rect, Pos2)>,
    rubber_from: Option<Pos2>,
}

impl Interaction {
    pub fn clear(&mut self) {
        *self = Interaction::default();
    }

    pub fn drawing(&self) -> bool {
        self.shape.is_some()
    }
}

/// What the canvas is doing.
#[derive(Clone, Copy)]
pub enum Mode {
    /// The current tool.
    Tool(Tool),
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
    ShapeDone(Shape, Combine),
    WandClick(Pos2, Combine),
    TextAt(Pos2, Combine),
    MoveSelection(i64, i64),
    Deselect,
    Picked(Rgb),
    BrushDown {
        erase: bool,
    },
    BrushDab(Pos2),
    BrushUp,
    /// Shift-click with the brush: the clone reference point (Move/"Clone").
    CloneRef(Pos2),
    RubberSet(Pos2, Pos2),
    RubberApply,
    PlaceSet(Rect),
    PlaceAccept,
    Escape,
    Hover(Option<(usize, usize)>),
}

pub struct View<'a> {
    pub selection: Option<&'a Selection>,
    /// Shown instead of the image (live preview of a dialog).
    pub display: Option<TextureId>,
}

fn combine_of(m: egui::Modifiers) -> Combine {
    if m.shift {
        Combine::Add
    } else if m.alt {
        Combine::Subtract
    } else {
        Combine::Replace
    }
}

pub fn canvas(
    ui: &mut egui::Ui,
    doc: &mut Doc,
    it: &mut Interaction,
    mode: Mode,
    tools: &Tools,
    view: View,
) -> Vec<Event> {
    let mut ev = Vec::new();
    let avail = ui.available_size();
    if doc.fit_pending {
        doc.fit_pending = false;
        let fit = ((avail.x - 16.0) / doc.img.w as f32).min((avail.y - 16.0) / doc.img.h as f32);
        doc.zoom = fit.clamp(1.0 / 32.0, 1.0);
        doc.mark_stale();
    }
    let zoom = doc.zoom;
    let size = vec2(doc.img.w as f32, doc.img.h as f32) * zoom;
    let tex = view.display.unwrap_or_else(|| doc.texture(ui.ctx()).id());
    let (iw, ih) = (doc.img.w, doc.img.h);

    egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
        // Center small images in the view.
        let pad = ((avail - size) / 2.0).max(Vec2::ZERO);
        ui.add_space(pad.y);
        ui.horizontal(|ui| {
            ui.add_space(pad.x);
            let (resp, painter) = ui.allocate_painter(size, Sense::click_and_drag());
            let origin = resp.rect.min;
            let to_img = |p: Pos2| pos2((p.x - origin.x) / zoom, (p.y - origin.y) / zoom);
            let to_scr = |p: Pos2| origin + p.to_vec2() * zoom;
            let full_uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
            let clip = ui.clip_rect();

            painter.rect_filled(resp.rect.expand(1.0), 0.0, Color32::from_gray(20));
            painter.image(tex, resp.rect, full_uv, Color32::WHITE);

            let hover = resp
                .hover_pos()
                .map(to_img)
                .filter(|p| p.x >= 0.0 && p.y >= 0.0 && (p.x as usize) < iw && (p.y as usize) < ih);
            ev.push(Event::Hover(hover.map(|p| (p.x as usize, p.y as usize))));
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                if it.drawing() {
                    it.clear();
                } else {
                    ev.push(Event::Escape);
                }
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
            let ants = |segs: &mut dyn Iterator<Item = (Pos2, Pos2)>| {
                let mut mesh = egui::Mesh::default();
                for (a, b) in segs {
                    let (a, b) = (to_scr(a), to_scr(b));
                    if !clip.intersects(Rect::from_two_pos(a, b).expand(1.0)) {
                        continue;
                    }
                    let (a, b) = (a * ppp, b * ppp);
                    let n = (a.distance(b).ceil() as usize).max(1);
                    let mut last = (i64::MIN, i64::MIN);
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
            let polyline = |pts: &[Pos2]| pts.windows(2).map(|w| (w[0], w[1])).collect::<Vec<_>>();

            // The current selection (shifted while it is being moved).
            if let Some(s) = view.selection {
                let off = match (it.moving_from, resp.interact_pointer_pos().map(to_img)) {
                    (Some(a), Some(b)) => vec2((b.x - a.x).round(), (b.y - a.y).round()),
                    _ => Vec2::ZERO,
                };
                ants(&mut s.outline.iter().map(|[a, b]| (*a + off, *b + off)));
            }

            match mode {
                Mode::Tool(tool) => {
                    tool_interaction(&resp, it, tool, ptr, press, mods, view.selection, &mut ev);
                    if let Some(shape) = &it.shape {
                        let mut pts = shape.outline();
                        if it.building {
                            pts.pop();
                            if let Some(h) = resp.hover_pos() {
                                pts.push(to_img(h));
                            }
                        }
                        ants(&mut polyline(&pts).into_iter());
                    }
                    match tool {
                        Tool::Brush => {
                            if let Some(p) = resp.hover_pos() {
                                let r = tools.pen_size as f32 * zoom / 2.0;
                                let (w, b) = (
                                    Stroke::new(1.0, Color32::WHITE),
                                    Stroke::new(1.0, Color32::BLACK),
                                );
                                match tools.brush {
                                    Brush::Circle => {
                                        painter.circle_stroke(p, r.max(2.0), w);
                                        painter.circle_stroke(p, r.max(2.0) + 1.0, b);
                                    }
                                    Brush::Square => {
                                        let q = Rect::from_center_size(
                                            p,
                                            Vec2::splat(2.0 * r.max(2.0)),
                                        );
                                        painter.rect_stroke(q, 0.0, w, egui::StrokeKind::Inside);
                                        painter.rect_stroke(q, 0.0, b, egui::StrokeKind::Outside);
                                    }
                                }
                            }
                            resp.clone().on_hover_cursor(egui::CursorIcon::None);
                        }
                        Tool::Eyedropper => {
                            if resp.clicked()
                                && let Some(p) = hover
                            {
                                ev.push(Event::Picked(doc.img.get(p.x as usize, p.y as usize)));
                            }
                            resp.clone().on_hover_cursor(egui::CursorIcon::Crosshair);
                        }
                        Tool::Text => {
                            resp.clone().on_hover_cursor(egui::CursorIcon::Text);
                        }
                        _ => {
                            let over_sel = resp
                                .hover_pos()
                                .map(to_img)
                                .is_some_and(|p| view.selection.is_some_and(|s| s.contains(p)));
                            let moving = it.moving_from.is_some();
                            resp.clone().on_hover_cursor(if moving {
                                egui::CursorIcon::Grabbing
                            } else if over_sel
                                && combine_of(mods) == Combine::Replace
                                && !it.building
                            {
                                egui::CursorIcon::Move
                            } else {
                                egui::CursorIcon::Crosshair
                            });
                        }
                    }
                }
                Mode::Rubber { roi, from, to } => {
                    if resp.drag_started() {
                        it.rubber_from = press;
                    }
                    if let (Some(a), Some(b)) = (it.rubber_from, ptr)
                        && resp.dragged()
                    {
                        ev.push(Event::RubberSet(a, b));
                    }
                    if resp.double_clicked() {
                        ev.push(Event::RubberApply);
                    }
                    draw_rubber_grid(&painter, roi, from, to, &to_scr, zoom);
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
                    ants(&mut polyline(&corners).into_iter());
                    for c in &corners[..4] {
                        let h = Rect::from_center_size(to_scr(*c), Vec2::splat(8.0));
                        painter.rect_filled(h, 1.0, Color32::WHITE);
                        painter.rect_stroke(
                            h,
                            1.0,
                            Stroke::new(1.0, Color32::BLACK),
                            egui::StrokeKind::Middle,
                        );
                    }
                    let handle_at = |p: Pos2| -> Option<Handle> {
                        let tol = 7.0 / zoom;
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
                            it.place_drag = handle_at(p).map(|h| (h, rect, p));
                        }
                    } else if resp.dragged()
                        && let (Some((h, r0, p0)), Some(p)) = (it.place_drag, ptr)
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
                        it.place_drag = None;
                    }
                }
            }
            if tools.animate
                && (view.selection.is_some()
                    || it.shape.is_some()
                    || matches!(mode, Mode::Place { .. }))
            {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(MARCH_MS));
            }
        });
    });
    ev
}

#[allow(clippy::too_many_arguments)]
fn tool_interaction(
    resp: &egui::Response,
    it: &mut Interaction,
    tool: Tool,
    ptr: Option<Pos2>,
    press: Option<Pos2>,
    mods: egui::Modifiers,
    selection: Option<&Selection>,
    ev: &mut Vec<Event>,
) {
    let combine = combine_of(mods);
    // Dragging inside the selection (no modifier) moves it.
    let starts_move = |p: Option<Pos2>| {
        tool.is_selection()
            && tool != Tool::Polygon
            && combine == Combine::Replace
            && p.is_some_and(|p| selection.is_some_and(|s| s.contains(p)))
    };
    if it.moving_from.is_some() {
        if resp.drag_stopped() {
            if let (Some(a), Some(b)) = (it.moving_from, ptr) {
                ev.push(Event::MoveSelection(
                    (b.x - a.x).round() as i64,
                    (b.y - a.y).round() as i64,
                ));
            }
            it.moving_from = None;
        }
        return;
    }
    match tool {
        Tool::Rect | Tool::Ellipse => {
            if resp.drag_started() {
                if starts_move(press) {
                    it.moving_from = press;
                    return;
                }
                it.drag_from = press;
                it.combine = Some(combine);
            }
            if let (Some(a), Some(b)) = (it.drag_from, ptr)
                && resp.dragged()
            {
                // Shift while dragging (with Replace) keeps it square/round.
                let b = if mods.shift && it.combine == Some(Combine::Replace) {
                    let d = b - a;
                    let s = d.x.abs().max(d.y.abs());
                    a + vec2(s * d.x.signum(), s * d.y.signum())
                } else {
                    b
                };
                it.shape = Some(if tool == Tool::Rect {
                    Shape::Rect(a, b)
                } else {
                    Shape::Ellipse(a, b)
                });
            }
            if resp.drag_stopped() {
                if let Some(s) = it.shape.take() {
                    ev.push(Event::ShapeDone(s, it.combine.unwrap_or(Combine::Replace)));
                }
                it.drag_from = None;
            } else if resp.clicked() && combine == Combine::Replace {
                ev.push(Event::Deselect);
            }
        }
        Tool::Lasso => {
            if resp.drag_started() {
                if starts_move(press) {
                    it.moving_from = press;
                    return;
                }
                it.shape = press.map(|p| Shape::Lasso(vec![p]));
                it.combine = Some(combine);
            }
            if resp.dragged()
                && let (Some(Shape::Lasso(v)), Some(p)) = (&mut it.shape, ptr)
                && v.last().is_none_or(|l| l.distance(p) >= 1.0)
            {
                v.push(p);
            }
            if resp.drag_stopped() {
                if let Some(s) = it.shape.take() {
                    ev.push(Event::ShapeDone(s, it.combine.unwrap_or(Combine::Replace)));
                }
            } else if resp.clicked() && combine == Combine::Replace {
                ev.push(Event::Deselect);
            }
        }
        Tool::Polygon => {
            if resp.double_clicked() {
                if it.building
                    && let Some(Shape::Polygon(mut v)) = it.shape.take()
                {
                    v.pop(); // the vertex the double-click's first click added
                    it.building = false;
                    ev.push(Event::ShapeDone(
                        Shape::Polygon(v),
                        it.combine.unwrap_or(Combine::Replace),
                    ));
                }
                return;
            }
            if resp.clicked()
                && let Some(p) = ptr
            {
                match (&mut it.shape, it.building) {
                    (Some(Shape::Polygon(v)), true) => v.push(p),
                    _ => {
                        it.shape = Some(Shape::Polygon(vec![p]));
                        it.building = true;
                        it.combine = Some(combine);
                    }
                }
            }
        }
        Tool::Wand => {
            if resp.drag_started() && starts_move(press) {
                it.moving_from = press;
            } else if resp.clicked()
                && let Some(p) = ptr
            {
                ev.push(Event::WandClick(p, combine));
            }
        }
        Tool::Text => {
            if resp.drag_started() && starts_move(press) {
                it.moving_from = press;
            } else if resp.clicked()
                && let Some(p) = ptr
            {
                ev.push(Event::TextAt(p, combine));
            }
        }
        Tool::Brush => {
            let secondary =
                resp.secondary_clicked() || resp.dragged_by(egui::PointerButton::Secondary);
            if mods.shift && resp.clicked() {
                if let Some(p) = ptr {
                    ev.push(Event::CloneRef(p));
                }
                return;
            }
            if resp.drag_started() || resp.clicked() || resp.secondary_clicked() {
                ev.push(Event::BrushDown { erase: secondary });
            }
            if (resp.dragged() || resp.drag_started() || resp.clicked() || resp.secondary_clicked())
                && let Some(p) = ptr
            {
                ev.push(Event::BrushDab(p));
            }
            if resp.drag_stopped() || resp.clicked() || resp.secondary_clicked() {
                ev.push(Event::BrushUp);
            }
        }
        Tool::Eyedropper => {}
    }
}

fn draw_rubber_grid(
    painter: &egui::Painter,
    roi: Rect,
    from: Option<Pos2>,
    to: Option<Pos2>,
    to_scr: &dyn Fn(Pos2) -> Pos2,
    zoom: f32,
) {
    // Grid deformed by the drag: each node moves by the displacement
    // weighted like the original's influence.
    let n = 10;
    let (a, b) = (from.unwrap_or(roi.center()), to.unwrap_or(roi.center()));
    let d = b - a;
    let infl = |i: f32, pv: f32, lo: f32, hi: f32| -> f32 {
        if i <= lo || i >= hi {
            return 0.0;
        }
        let (u, v) = if i <= pv {
            (i - lo, pv - i)
        } else {
            (hi - i, i - pv)
        };
        u * u / (u * u + v * v).max(1e-6)
    };
    let warp = |p: Pos2| {
        p + d * (infl(p.x, a.x, roi.left(), roi.right()) * infl(p.y, a.y, roi.top(), roi.bottom()))
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
