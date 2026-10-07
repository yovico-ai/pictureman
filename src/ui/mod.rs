//! The Picture Man window: menu bar, toolbox, documents and dialogs, and the
//! interaction state machine (outline an area → double-click inside or
//! outside → process; paint with the pen; drag the Rubber grid; place a
//! transformed or pasted fragment).

mod apply;
mod assets;
mod canvas;
mod commands;
mod doc;
mod settings;
mod tools;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};

use eframe::egui::{
    self, Color32, ColorImage, RichText, TextureHandle, TextureOptions, pos2, vec2,
};

use self::apply::{Ctx, DialogEnv, OpParams};
pub use self::assets::Assets as AppAssets;
use self::assets::Assets;
use self::canvas::{Event, Mode, Overlay, Selecting, Shape};
use self::commands::{Cmd, MenuItem};
use self::doc::Doc;
use self::tools::{Area, Tools};
use crate::core::{Image, MAX_PIXELS, Mask, MsRand, Rect};
use crate::formats::{self, Format};
use crate::ops::transform::{self, Fragment};
use crate::selection::{self as sel, Weights};
use crate::text::FontEntry;

/// Zoom steps of the View menu: 1:8 … 8:1.
const ZOOMS: [f32; 9] = [0.125, 1.0 / 6.0, 0.25, 0.5, 1.0, 2.0, 4.0, 6.0, 8.0];

#[derive(Clone)]
struct Pending {
    cmd: Cmd,
    params: OpParams,
}

/// The chosen part of the image: binary selection plus edge weights.
struct Chosen {
    selected: Mask,
    weights: Option<Weights>,
}

struct PenSession {
    /// Image when painting started: filters read from it, so repeated dabs
    /// don't compound; right-drag restores it.
    backup: Image,
    stroke: Option<sel::Stroke>,
    last: Option<egui::Pos2>,
    erase: bool,
    clone_ref: Option<egui::Pos2>,
    clone_offset: Option<(i64, i64)>,
}

struct Placing {
    frag: Fragment,
    rect: egui::Rect,
    tex: TextureHandle,
}

enum State {
    Idle,
    Select(Pending),
    Pen(Pending, Option<PenSession>),
    Rubber {
        pending: Pending,
        chosen: Chosen,
        roi: Rect,
        from: Option<egui::Pos2>,
        to: Option<egui::Pos2>,
    },
    Place(Placing),
}

enum Dialog {
    Op(Cmd, OpParams),
    Text(egui::Pos2, OpParams),
    WandOptions,
}

/// An operation running on a worker thread.
struct Job {
    doc: u64,
    title: &'static str,
    rx: Receiver<Result<(Image, MsRand), String>>,
}

pub struct App {
    egui: egui::Context,
    assets: Assets,
    docs: Vec<Doc>,
    active: usize,
    next_id: u64,
    tools: Tools,
    params: OpParams,
    show_toolbox: bool,
    state: State,
    dialog: Option<Dialog>,
    sel: Selecting,
    /// Mask of the current area for mask-based areas (magic wand).
    area_mask: Option<Mask>,
    /// Outline textures (the four TILE phases) for mask-based areas.
    ants: Option<[TextureHandle; 4]>,
    text_ants: Option<[TextureHandle; 4]>,
    job: Option<Job>,
    rng: MsRand,
    fonts: Option<Vec<FontEntry>>,
    preview: Option<(TextureHandle, Image)>,
    hover: Option<(usize, usize)>,
    message: Option<String>,
    about: bool,
    splash_until: f64,
    recent: Vec<PathBuf>,
    zoom_accum: f32,
    last_area: Area,
    zoom_latched: bool,
    /// Last settings text written, to save only on change.
    saved_settings: String,
}

fn edge(e: tools::Edge) -> sel::Edge {
    match e {
        tools::Edge::Sharp => sel::Edge::Sharp,
        tools::Edge::Low => sel::Edge::SmoothLow,
        tools::Edge::Medium => sel::Edge::SmoothMedium,
        tools::Edge::High => sel::Edge::SmoothHigh,
    }
}

fn area_kind(a: Area) -> sel::AreaKind {
    match a {
        Area::Whole => sel::AreaKind::Whole,
        Area::Rect => sel::AreaKind::Rect,
        Area::Ellipse => sel::AreaKind::Ellipse,
        Area::Polygon => sel::AreaKind::Polygon,
        Area::Text => sel::AreaKind::Text,
        Area::MagicWand => sel::AreaKind::MagicWand,
        Area::Freehand => sel::AreaKind::Freehand,
        Area::Pen => sel::AreaKind::Pen,
    }
}

/// A mask's outline in the original's moving TILE pattern, one texture per
/// phase.
fn ants_textures(ctx: &egui::Context, name: &str, mask: &Mask) -> [TextureHandle; 4] {
    let outline = sel::outline(mask);
    [0, 1, 2, 3].map(|phase| {
        let mut rgba = vec![0u8; mask.w * mask.h * 4];
        for y in 0..mask.h {
            for x in 0..mask.w {
                if outline.get(x, y) != 0 {
                    let v = if !canvas::tile_black(x as i64, y as i64, phase) {
                        255
                    } else {
                        0
                    };
                    rgba[(y * mask.w + x) * 4..][..4].copy_from_slice(&[v, v, v, 255]);
                }
            }
        }
        let ci = ColorImage::from_rgba_unmultiplied([mask.w, mask.h], &rgba);
        ctx.load_texture(format!("{name}{phase}"), ci, TextureOptions::NEAREST)
    })
}

fn fragment_texture(ctx: &egui::Context, f: &Fragment) -> TextureHandle {
    let rgba: Vec<u8> = f
        .img
        .px
        .iter()
        .zip(&f.alpha.data)
        .flat_map(|(p, a)| [p[0], p[1], p[2], if *a != 0 { 230 } else { 0 }])
        .collect();
    ctx.load_texture(
        "fragment",
        ColorImage::from_rgba_unmultiplied([f.img.w, f.img.h], &rgba),
        TextureOptions::LINEAR,
    )
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, files: Vec<PathBuf>) -> Self {
        let mut tools = Tools::default();
        let mut params = OpParams::default();
        let loaded = settings::load(&mut tools, &mut params);
        let mut app = App {
            egui: cc.egui_ctx.clone(),
            assets: Assets::load(&cc.egui_ctx),
            docs: Vec::new(),
            active: 0,
            next_id: 1,
            tools,
            params,
            show_toolbox: loaded.show_toolbox,
            state: State::Idle,
            dialog: None,
            sel: Selecting::default(),
            area_mask: None,
            ants: None,
            text_ants: None,
            job: None,
            rng: MsRand::default(),
            fonts: None,
            preview: None,
            hover: None,
            message: None,
            about: false,
            splash_until: 2.5,
            recent: loaded.recent,
            zoom_accum: 0.0,
            last_area: Area::Whole,
            zoom_latched: false,
            saved_settings: String::new(),
        };
        app.saved_settings = app.settings_text();
        app.last_area = app.tools.area;
        for f in files {
            app.open_path(f);
        }
        app
    }

    fn doc(&mut self) -> Option<&mut Doc> {
        self.docs.get_mut(self.active)
    }

    fn add_doc(&mut self, name: String, path: Option<PathBuf>, img: Image) {
        let id = self.next_id;
        self.next_id += 1;
        self.docs.push(Doc::new(id, name, path, img));
        self.active = self.docs.len() - 1;
        self.reset_area();
    }

    fn open_path(&mut self, path: PathBuf) {
        match formats::load(&path) {
            Ok(img) => {
                let name = path
                    .file_name()
                    .map_or("image".into(), |n| n.to_string_lossy().into_owned());
                self.remember(&path);
                self.add_doc(name, Some(path), img);
            }
            Err(e) => self.message = Some(format!("Can't open file {}: {e}", path.display())),
        }
    }

    /// Put `path` at the top of the recent-files list.
    fn remember(&mut self, path: &std::path::Path) {
        let p = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.recent.retain(|r| *r != p);
        self.recent.insert(0, p);
        self.recent.truncate(settings::MAX_RECENT);
    }

    fn settings_text(&self) -> String {
        settings::serialize(&self.tools, &self.params, self.show_toolbox, &self.recent)
    }

    fn save_settings_if_changed(&mut self) {
        let text = self.settings_text();
        if text != self.saved_settings {
            if let Err(e) = settings::save(&text) {
                self.message = Some(format!("Can't save settings: {e}"));
            }
            self.saved_settings = text;
        }
    }

    fn image_filters(d: rfd::FileDialog) -> rfd::FileDialog {
        let all: Vec<&str> = Format::ALL
            .iter()
            .filter(|f| f.can_read())
            .flat_map(|f| f.extensions().iter().copied())
            .collect();
        let mut d = d.add_filter("All images", &all);
        for f in Format::ALL.iter().filter(|f| f.can_read()) {
            d = d.add_filter(f.description(), f.extensions());
        }
        d
    }

    fn open_dialog(&mut self) {
        if let Some(paths) =
            Self::image_filters(rfd::FileDialog::new().set_title("Open Source")).pick_files()
        {
            for p in paths {
                self.open_path(p);
            }
        }
    }

    fn pick_image(&mut self, title: &str) -> Option<(Image, String)> {
        let path = Self::image_filters(rfd::FileDialog::new().set_title(title)).pick_file()?;
        match formats::load(&path) {
            Ok(img) => Some((
                img,
                path.file_name()
                    .map_or(String::new(), |n| n.to_string_lossy().into_owned()),
            )),
            Err(e) => {
                self.message = Some(format!("Can't open file {}: {e}", path.display()));
                None
            }
        }
    }

    fn save(&mut self, ask: bool) {
        let Some(doc) = self.docs.get_mut(self.active) else {
            return;
        };
        let path = match (&doc.path, ask) {
            (Some(p), false) => Some(p.clone()),
            _ => {
                let mut d = rfd::FileDialog::new()
                    .set_title("Select Target")
                    .set_file_name(&doc.name);
                for f in Format::ALL {
                    d = d.add_filter(f.description(), f.extensions());
                }
                d.save_file()
            }
        };
        let Some(path) = path else { return };
        match formats::save(&path, &doc.img, None) {
            Ok(()) => {
                let p = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
                self.recent.retain(|r| *r != p);
                self.recent.insert(0, p);
                self.recent.truncate(settings::MAX_RECENT);
                doc.name = path
                    .file_name()
                    .map_or(doc.name.clone(), |n| n.to_string_lossy().into_owned());
                doc.path = Some(path);
                doc.saved = Some(doc.img.clone());
                doc.modified = false;
            }
            Err(e) => self.message = Some(format!("Can't save file: {e}")),
        }
    }

    /// Forget the outlined area (unless "Preserve mask" keeps it).
    fn reset_area(&mut self) {
        self.state = State::Idle;
        self.sel.clear();
        self.area_mask = None;
        self.ants = None;
        self.text_ants = None;
    }

    fn cancel(&mut self) {
        if let State::Pen(_, Some(_)) = self.state {
            // Leaving painting mode ends the session; the backup stays in Undo.
        }
        self.reset_area();
    }

    /// A menu command was chosen.
    fn invoke(&mut self, cmd: Cmd, ctx: &egui::Context) {
        if cmd.needs_image() && (self.docs.is_empty() || self.job.is_some()) {
            return;
        }
        match cmd {
            Cmd::New | Cmd::Size | Cmd::Paste | Cmd::PasteFrom => self.open_op_dialog(cmd),
            Cmd::Open => self.open_dialog(),
            Cmd::Save => self.save(false),
            Cmd::SaveAs => self.save(true),
            Cmd::Reload => {
                if let Some(d) = self.doc()
                    && let Some(img) = d.saved.clone()
                {
                    d.push_undo();
                    d.set_image(img);
                    d.modified = false;
                }
                self.reset_area();
            }
            Cmd::Exit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Cmd::Undo => {
                self.reset_area();
                if let Some(d) = self.doc() {
                    d.undo();
                }
            }
            Cmd::Zoom(z) => {
                if let Some(d) = self.doc() {
                    d.zoom = ZOOMS[z as usize];
                    d.mark_stale();
                }
            }
            Cmd::Toolbox => self.show_toolbox = !self.show_toolbox,
            Cmd::AnimateSelection => self.tools.animate = !self.tools.animate,
            Cmd::CreateBackup => self.tools.backup = !self.tools.backup,
            Cmd::PreserveMask => self.tools.preserve_mask = !self.tools.preserve_mask,
            Cmd::PickColor => self.tools.picking = true,
            Cmd::MagicWandOptions => self.dialog = Some(Dialog::WandOptions),
            Cmd::About => self.about = true,
            Cmd::Copy => {
                let p = self.params.clone();
                self.begin(Cmd::Copy, p);
            }
            c if c.is_image_op() => self.open_op_dialog(c),
            _ => {}
        }
    }

    fn open_op_dialog(&mut self, cmd: Cmd) {
        let mut params = self.params.clone();
        params.color = self.tools.color;
        apply::prepare(cmd, &mut params, self.docs.get(self.active).map(|d| &d.img));
        if apply::needs_dialog(cmd) {
            self.preview = None;
            self.dialog = Some(Dialog::Op(cmd, params));
        } else {
            self.begin(cmd, params);
        }
    }

    /// Parameters are known; process the whole image or wait for an area.
    fn begin(&mut self, cmd: Cmd, params: OpParams) {
        self.params = params.clone();
        // An area outlined before choosing the command is used directly.
        let keep = matches!(
            self.tools.area,
            Area::Rect
                | Area::Ellipse
                | Area::Polygon
                | Area::Text
                | Area::MagicWand
                | Area::Freehand
        ) && (self.sel.ready() || self.area_mask.is_some());
        if !keep {
            self.reset_area();
        }
        self.state = State::Idle;
        let Some(d) = self.docs.get(self.active) else {
            if cmd == Cmd::New {
                self.new_image(&params);
            }
            return;
        };
        let (w, h) = (d.img.w, d.img.h);
        match cmd {
            Cmd::New => return self.new_image(&params),
            Cmd::Size => {
                let (nw, nh) = params.new_size;
                if nw == 0 || nh == 0 || nw.saturating_mul(nh) > MAX_PIXELS {
                    self.message = Some("Incorrect parameters.".into());
                    return;
                }
                return self.run_job(cmd, move |src, rng| {
                    Ok((transform::resize(&src, nw, nh), rng))
                });
            }
            Cmd::Paste => {
                let img = arboard::Clipboard::new()
                    .and_then(|mut c| c.get_image())
                    .ok()
                    .and_then(|im| {
                        let (w, h) = (im.width, im.height);
                        (w > 0
                            && h > 0
                            && im.bytes.len() >= w * h * 4
                            && w.saturating_mul(h) <= MAX_PIXELS)
                            .then(|| Image {
                                w,
                                h,
                                px: im
                                    .bytes
                                    .as_chunks::<4>()
                                    .0
                                    .iter()
                                    .take(w * h)
                                    .map(|c| [c[0], c[1], c[2]])
                                    .collect(),
                            })
                    });
                match img {
                    Some(img) => self.start_place(apply::paste_fragment(img, &params)),
                    None => self.message = Some("The clipboard holds no picture.".into()),
                }
                return;
            }
            Cmd::PasteFrom => {
                if let Some((img, _)) = self.pick_image("Paste from") {
                    self.start_place(apply::paste_fragment(img, &params));
                }
                return;
            }
            _ => {}
        }
        let pending = Pending { cmd, params };
        match self.tools.area {
            Area::Pen => {
                if cmd == Cmd::Rotate || cmd == Cmd::Clip || cmd == Cmd::Copy {
                    self.message = Some(format!("{} can't be used with the pen.", cmd.title()));
                    return;
                }
                self.state = State::Pen(pending, None);
            }
            Area::Whole => {
                let chosen = Chosen {
                    selected: Mask::full(w, h),
                    weights: None,
                };
                self.process(pending, chosen);
            }
            _ if keep => {
                if let Some(chosen) = self.choose_area(None) {
                    self.process(pending, chosen);
                }
            }
            _ => self.state = State::Select(pending),
        }
    }

    fn new_image(&mut self, p: &OpParams) {
        let (w, h) = p.new_size;
        if w == 0 || h == 0 || w.saturating_mul(h) > MAX_PIXELS {
            self.message = Some("Incorrect parameters.".into());
            return;
        }
        self.add_doc(
            format!("Untitled{}", self.next_id),
            None,
            Image::new(w, h, [255; 3]),
        );
    }

    /// The area is chosen; do what the command does with it.
    fn process(&mut self, pending: Pending, chosen: Chosen) {
        let Pending { cmd, params } = pending;
        let Some(d) = self.docs.get(self.active) else {
            return;
        };
        let roi = chosen.selected.bounds();
        if roi.is_empty() {
            return;
        }
        let whole = self.tools.area == Area::Whole;
        match cmd {
            Cmd::Copy => {
                let img = d.img.crop(roi);
                let raw: Vec<u8> = img
                    .px
                    .iter()
                    .flat_map(|p| [p[0], p[1], p[2], 255])
                    .collect();
                let r = arboard::Clipboard::new().and_then(|mut c| {
                    c.set_image(arboard::ImageData {
                        width: img.w,
                        height: img.h,
                        bytes: raw.into(),
                    })
                });
                if let Err(e) = r {
                    self.message = Some(format!("Can't copy to the clipboard: {e}"));
                }
            }
            Cmd::Clip => {
                if whole {
                    self.message = Some("Select the area to clip first.".into());
                    return;
                }
                self.run_job(cmd, move |src, rng| Ok((src.crop(roi), rng)));
            }
            Cmd::Rotate if whole => {
                let (angle, bg) = (params.angle, params.color);
                self.run_job(cmd, move |src, rng| {
                    transform::rotate(&src, angle, bg)
                        .map(|i| (i, rng))
                        .ok_or_else(|| "Incorrect parameters.".to_string())
                });
            }
            Cmd::Move if whole => self.message = Some("Move needs a selected area.".into()),
            Cmd::Rubber => {
                self.state = State::Rubber {
                    pending: Pending { cmd, params },
                    chosen,
                    roi,
                    from: None,
                    to: None,
                }
            }
            c if apply::is_fragment_op(c) && !whole => {
                if let Some(frag) =
                    apply::make_fragment(c, &params, &d.img, &chosen.selected, roi, self.tools.area)
                {
                    self.start_place(frag);
                }
            }
            _ => self.run_pixel(cmd, params, chosen),
        }
    }

    /// Run a pixel operation on a worker thread and commit it through the
    /// selection's edge weights.
    fn run_pixel(&mut self, cmd: Cmd, params: OpParams, chosen: Chosen) {
        let Some(d) = self.docs.get(self.active) else {
            return;
        };
        let backup = d.last_backup().cloned();
        let (area, circle) = (self.tools.area, self.tools.brush == tools::Brush::Circle);
        self.run_job(cmd, move |src, mut rng| {
            let roi = chosen.selected.bounds();
            let processed = {
                let mut ctx = Ctx {
                    area,
                    mask: &chosen.selected,
                    circle_pen: circle,
                    rng: &mut rng,
                    backup: backup.as_ref(),
                };
                apply::apply(cmd, &params, &src, roi, &mut ctx)
            };
            let out = match &chosen.weights {
                Some(w) => {
                    let mut out = src;
                    sel::commit(&mut out, &processed, w);
                    out
                }
                None => processed,
            };
            Ok((out, rng))
        });
    }

    fn run_job(
        &mut self,
        cmd: Cmd,
        f: impl FnOnce(Image, MsRand) -> Result<(Image, MsRand), String> + Send + 'static,
    ) {
        let backup = self.tools.backup;
        let Some(d) = self.docs.get_mut(self.active) else {
            return;
        };
        if backup {
            d.push_undo();
        }
        let src = d.img.clone();
        let rng = self.rng.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            // A panic in an operation must not take the program down.
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(src, rng)))
                .unwrap_or_else(|_| Err("Unknown error.".to_string()));
            let _ = tx.send(r);
        });
        self.job = Some(Job {
            doc: d.id,
            title: if cmd.title().is_empty() {
                "Processing"
            } else {
                cmd.title()
            },
            rx,
        });
    }

    fn poll_job(&mut self, ctx: &egui::Context) {
        let Some(job) = &self.job else { return };
        match job.rx.try_recv() {
            Ok(r) => {
                let id = job.doc;
                self.job = None;
                match r {
                    Ok((img, rng)) => {
                        self.rng = rng;
                        if let Some(d) = self.docs.iter_mut().find(|d| d.id == id) {
                            let resized = img.w != d.img.w || img.h != d.img.h;
                            d.set_image(img);
                            if resized {
                                self.reset_area();
                            }
                        }
                    }
                    Err(e) => self.message = Some(e),
                }
                if !self.tools.preserve_mask {
                    self.reset_area();
                }
            }
            Err(mpsc::TryRecvError::Empty) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(30))
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.job = None;
                self.message = Some("Operation failed.".into());
            }
        }
    }

    fn start_place(&mut self, frag: Fragment) {
        if frag.img.w == 0 || frag.img.h == 0 {
            return;
        }
        self.sel.clear();
        self.area_mask = None;
        self.ants = None;
        self.text_ants = None;
        let rect = egui::Rect::from_min_size(
            pos2(frag.x as f32, frag.y as f32),
            vec2(frag.img.w as f32, frag.img.h as f32),
        );
        let tex = fragment_texture(&self.egui, &frag);
        self.state = State::Place(Placing { frag, rect, tex });
    }

    /// The area the user double-clicked in: inside or outside of the shape,
    /// with the edge weights of the current Edge mode.
    /// `p` = the double-click point (it decides inside or outside); `None`
    /// takes the inside.
    fn choose_area(&self, p: Option<egui::Pos2>) -> Option<Chosen> {
        let d = self.docs.get(self.active)?;
        let (w, h) = (d.img.w, d.img.h);
        let (shape_mask, bw, bh) = match (&self.area_mask, &self.sel.shape) {
            (Some(m), _) => (m.clone(), 0, 0),
            (None, Some(s)) => {
                let b = s.bounds();
                (
                    s.to_mask(w, h, self.tools.pen_size as usize),
                    b.width().round() as i32,
                    b.height().round() as i32,
                )
            }
            _ => return None,
        };
        let inside = match p {
            Some(p) => {
                sel::double_click_inside(&shape_mask, p.x.max(0.0) as usize, p.y.max(0.0) as usize)
            }
            None => true,
        };
        let selected = sel::selected_mask(&shape_mask, inside);
        let e = edge(self.tools.edge);
        let feather = sel::feather_width(area_kind(self.tools.area), e, bw, bh);
        let weights = sel::selection_weights(&shape_mask, inside, e, feather);
        Some(Chosen {
            selected,
            weights: Some(weights),
        })
    }

    fn handle_canvas(&mut self, events: Vec<Event>) {
        for e in events {
            match e {
                Event::Hover(h) => self.hover = h,
                Event::Cancel => self.cancel(),
                Event::Picked(c) => {
                    self.tools.color = c;
                    self.tools.picking = false;
                }
                Event::ApplyAt(p) => {
                    let State::Select(pending) = &self.state else {
                        continue;
                    };
                    let pending = pending.clone();
                    if let Some(chosen) = self.choose_area(Some(p)) {
                        self.state = State::Idle;
                        self.message = None;
                        self.process(pending, chosen);
                    }
                }
                Event::WandSeed(p) => {
                    let Some(d) = self.docs.get(self.active) else {
                        continue;
                    };
                    let (x, y) = (p.x.max(0.0) as usize, p.y.max(0.0) as usize);
                    if x >= d.img.w || y >= d.img.h {
                        continue;
                    }
                    let mode = if self.tools.wand_hsv {
                        sel::Match::Hsv
                    } else {
                        sel::Match::Rgb
                    };
                    let m = sel::magic_wand(
                        &d.img,
                        (x, y),
                        self.tools.wand_tolerance as i32,
                        mode,
                        self.tools.wand_unifold,
                    );
                    self.ants = Some(ants_textures(&self.egui, "wand", &m));
                    self.area_mask = Some(m);
                }
                Event::TextAt(p) => {
                    if self.fonts.is_none() {
                        self.fonts = Some(crate::text::available_fonts());
                    }
                    self.dialog = Some(Dialog::Text(p, self.params.clone()));
                }
                Event::PenDown { erase } => self.pen_down(erase),
                Event::PenDab(p) => self.pen_dab(p),
                Event::PenUp => {
                    if let State::Pen(_, Some(s)) = &mut self.state {
                        s.stroke = None;
                        s.last = None;
                    }
                }
                Event::CloneRef(p) => {
                    self.pen_session();
                    if let State::Pen(_, Some(s)) = &mut self.state {
                        s.clone_ref = Some(p);
                    }
                }
                Event::RubberSet(a, b) => {
                    if let State::Rubber { from, to, .. } = &mut self.state {
                        *from = Some(a);
                        *to = Some(b);
                    }
                }
                Event::RubberApply => {
                    let state = std::mem::replace(&mut self.state, State::Idle);
                    let State::Rubber {
                        mut pending,
                        chosen,
                        roi,
                        from: Some(a),
                        to: Some(b),
                    } = state
                    else {
                        self.state = state;
                        continue;
                    };
                    let ip = |p: egui::Pos2| (p.x.round() as i64, p.y.round() as i64);
                    pending.params.rubber = (ip(a), ip(b));
                    if self.tools.area == Area::Whole {
                        self.run_pixel(pending.cmd, pending.params, chosen);
                    } else if let Some(d) = self.docs.get(self.active) {
                        let frag = apply::make_fragment(
                            Cmd::Rubber,
                            &pending.params,
                            &d.img,
                            &chosen.selected,
                            roi,
                            self.tools.area,
                        );
                        if let Some(f) = frag {
                            self.start_place(f);
                        }
                    }
                }
                Event::PlaceSet(r) => {
                    if let State::Place(pl) = &mut self.state {
                        pl.rect = r;
                    }
                }
                Event::PlaceAccept => {
                    let State::Place(pl) = std::mem::replace(&mut self.state, State::Idle) else {
                        continue;
                    };
                    let backup = self.tools.backup;
                    let Some(d) = self.docs.get_mut(self.active) else {
                        continue;
                    };
                    let r = pl.rect;
                    let (w, h) = (
                        r.width().round().max(1.0) as usize,
                        r.height().round().max(1.0) as usize,
                    );
                    if w.saturating_mul(h) > MAX_PIXELS {
                        self.message = Some("Fragment too large.".into());
                        continue;
                    }
                    let (img, mask) = transform::place_fragment(
                        &d.img,
                        &pl.frag,
                        r.min.x.round() as i64,
                        r.min.y.round() as i64,
                        w,
                        h,
                    );
                    if backup {
                        d.push_undo();
                    }
                    d.set_image(img);
                    if self.tools.preserve_mask {
                        self.ants = Some(ants_textures(&self.egui, "wand", &mask));
                        self.area_mask = Some(mask);
                    }
                }
            }
        }
    }

    fn pen_session(&mut self) {
        let backup = self.tools.backup;
        let (State::Pen(_, sess), Some(d)) = (&mut self.state, self.docs.get_mut(self.active))
        else {
            return;
        };
        if sess.is_none() {
            if backup {
                d.push_undo();
            }
            *sess = Some(PenSession {
                backup: d.img.clone(),
                stroke: None,
                last: None,
                erase: false,
                clone_ref: None,
                clone_offset: None,
            });
        }
    }

    fn pen_down(&mut self, erase: bool) {
        self.pen_session();
        if let State::Pen(_, Some(s)) = &mut self.state {
            s.stroke = Some(sel::Stroke::begin(3));
            s.last = None;
            s.erase = erase;
            s.clone_offset = None;
        }
    }

    fn pen_dab(&mut self, p: egui::Pos2) {
        let (State::Pen(pending, Some(s)), Some(d)) =
            (&mut self.state, self.docs.get_mut(self.active))
        else {
            return;
        };
        let Some(stroke) = s.stroke.as_mut() else {
            return;
        };
        let cmd = pending.cmd;
        let size = self.tools.pen_size as usize;
        let kind = if self.tools.brush == tools::Brush::Circle {
            sel::BrushKind::Circle
        } else {
            sel::BrushKind::Square
        };
        let pen = sel::PenParams {
            brush: sel::Brush { size, kind },
            edge: edge(self.tools.edge),
            airbrush: 16000,
        };
        if cmd == Cmd::Move && !s.erase && s.clone_offset.is_none() {
            let Some(r) = s.clone_ref else {
                self.message = Some("Shift-click the reference point first".into());
                return;
            };
            s.clone_offset = Some(((r.x - p.x).round() as i64, (r.y - p.y).round() as i64));
        }
        // Dabs along the drag (the original only dabbed at mouse-move events,
        // which leaves gaps on fast strokes).
        let from = s.last.unwrap_or(p);
        let spacing = (size as f32 / 2.0).max(1.0);
        let n = ((from.distance(p) / spacing).ceil() as usize).max(1);
        let full = Mask::full(d.img.w, d.img.h);
        for i in 1..=n {
            let c = if s.last.is_none() {
                p
            } else {
                from.lerp(p, i as f32 / n as f32)
            };
            let ci = (c.x.floor() as i32, c.y.floor() as i32);
            if s.erase {
                sel::erase_dab(&mut d.img, &s.backup, ci, &pen, stroke, &mut self.rng);
            } else {
                let r = sel::dab_rect(ci, size, d.img.w, d.img.h);
                if !r.is_empty() {
                    let fill_copy = apply::is_fill(cmd).then(|| d.img.clone());
                    let source = fill_copy.as_ref().unwrap_or(&s.backup);
                    let processed = if cmd == Cmd::Move {
                        let (dx, dy) = s.clone_offset.unwrap_or((0, 0));
                        transform::clone_offset(source, r, dx, dy, pending.params.color)
                    } else {
                        let mut ctx = Ctx {
                            area: Area::Pen,
                            mask: &full,
                            circle_pen: kind == sel::BrushKind::Circle,
                            rng: &mut self.rng,
                            backup: Some(&s.backup),
                        };
                        apply::apply(cmd, &pending.params, source, r, &mut ctx)
                    };
                    sel::paint_dab(
                        &mut d.img,
                        &processed,
                        source,
                        ci,
                        &pen,
                        stroke,
                        &mut self.rng,
                    );
                }
            }
            stroke.advance();
            if s.last.is_none() {
                break;
            }
        }
        s.last = Some(p);
        d.touch();
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        let mut chosen = None;
        let mut open_recent: Option<PathBuf> = None;
        let has_doc = !self.docs.is_empty();
        let can_undo = self
            .docs
            .get(self.active)
            .is_some_and(|d| d.last_backup().is_some());
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                item(ui, "New…", Some("Ctrl+N"), Cmd::New, true, &mut chosen);
                item(ui, "Open…", Some("Ctrl+O"), Cmd::Open, true, &mut chosen);
                item(ui, "Reload", None, Cmd::Reload, has_doc, &mut chosen);
                ui.add_enabled_ui(!self.recent.is_empty(), |ui| {
                    ui.menu_button("Recent", |ui| {
                        for (i, p) in self.recent.iter().enumerate() {
                            let name = p.file_name().map_or_else(
                                || p.display().to_string(),
                                |n| n.to_string_lossy().into_owned(),
                            );
                            if ui
                                .button(format!("{} {name}", i + 1))
                                .on_hover_text(p.display().to_string())
                                .clicked()
                            {
                                open_recent = Some(p.clone());
                                ui.close();
                            }
                        }
                    });
                });
                ui.separator();
                item(ui, "Save", Some("Ctrl+S"), Cmd::Save, has_doc, &mut chosen);
                item(ui, "Save As…", None, Cmd::SaveAs, has_doc, &mut chosen);
                ui.separator();
                item(ui, "Exit", None, Cmd::Exit, true, &mut chosen);
            });
            ui.menu_button("Edit", |ui| {
                ui.add_enabled_ui(has_doc && self.job.is_none(), |ui| {
                    for m in commands::edit_menu() {
                        match m {
                            MenuItem::Cmd(l, Cmd::Undo) => {
                                item(ui, l, Some("Ctrl+Z"), Cmd::Undo, can_undo, &mut chosen)
                            }
                            m => menu_item(ui, &m, &mut chosen),
                        }
                    }
                });
            });
            ui.menu_button("View", |ui| {
                ui.add_enabled_ui(has_doc, |ui| {
                    ui.menu_button("Zoom out", |ui| {
                        for (i, l) in [(3, "1:2"), (2, "1:4"), (1, "1:6"), (0, "1:8")] {
                            item(ui, l, None, Cmd::Zoom(i), true, &mut chosen);
                        }
                    });
                    ui.menu_button("Zoom in", |ui| {
                        for (i, l) in [(5, "2:1"), (6, "4:1"), (7, "6:1"), (8, "8:1")] {
                            item(ui, l, None, Cmd::Zoom(i), true, &mut chosen);
                        }
                    });
                    item(
                        ui,
                        "Original size [1:1]",
                        Some("Ctrl+1"),
                        Cmd::Zoom(4),
                        true,
                        &mut chosen,
                    );
                });
                ui.separator();
                check(
                    ui,
                    "Animate selection",
                    self.tools.animate,
                    Cmd::AnimateSelection,
                    &mut chosen,
                );
                check(ui, "Toolbox", self.show_toolbox, Cmd::Toolbox, &mut chosen);
            });
            ui.menu_button("Options", |ui| {
                ui.menu_button("Area", |ui| {
                    for (a, l) in [
                        (Area::Whole, "Whole image"),
                        (Area::Rect, "Rectangle"),
                        (Area::Ellipse, "Ellipse"),
                        (Area::Polygon, "Polygon"),
                        (Area::Text, "Text"),
                        (Area::MagicWand, "Magic Wand"),
                        (Area::Freehand, "Freehand"),
                        (Area::Pen, "Pen"),
                    ] {
                        ui.radio_value(&mut self.tools.area, a, l);
                    }
                });
                ui.menu_button("Edge", |ui| {
                    use tools::Edge::*;
                    for (e, l) in [
                        (Sharp, "Sharp"),
                        (Low, "Smooth low"),
                        (Medium, "Smooth medium"),
                        (High, "Smooth high"),
                    ] {
                        ui.radio_value(&mut self.tools.edge, e, l);
                    }
                });
                ui.menu_button("Pen/brush/line size", |ui| {
                    for s in tools::PEN_SIZES {
                        ui.radio_value(&mut self.tools.pen_size, s, format!("{s} (×{s})"));
                    }
                });
                ui.menu_button("Pen/brush type", |ui| {
                    ui.radio_value(&mut self.tools.brush, tools::Brush::Square, "Square");
                    ui.radio_value(&mut self.tools.brush, tools::Brush::Circle, "Circle");
                });
                ui.menu_button("Color", |ui| {
                    item(ui, "Pick up…", None, Cmd::PickColor, has_doc, &mut chosen);
                });
                item(
                    ui,
                    "Magic wand…",
                    None,
                    Cmd::MagicWandOptions,
                    true,
                    &mut chosen,
                );
                ui.separator();
                check(
                    ui,
                    "Create backup",
                    self.tools.backup,
                    Cmd::CreateBackup,
                    &mut chosen,
                );
                check(
                    ui,
                    "Preserve mask",
                    self.tools.preserve_mask,
                    Cmd::PreserveMask,
                    &mut chosen,
                );
            });
            ui.menu_button("Help", |ui| {
                item(ui, "About…", None, Cmd::About, true, &mut chosen);
            });
        });
        if let Some(c) = chosen {
            self.invoke(c, ui.ctx());
        }
        if let Some(p) = open_recent {
            self.open_path(p);
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let hint = match &self.state {
                State::Idle => match self.tools.area {
                    _ if self.docs.is_empty() => None,
                    Area::Whole => None,
                    Area::Pen => Some("Pen: choose a command, then paint with it".into()),
                    Area::Polygon if !self.sel.ready() => Some(
                        "Polygon: click vertices, double-click to close, then choose a command (it applies inside)".into(),
                    ),
                    Area::MagicWand if self.area_mask.is_none() => {
                        Some("Magic wand: click a color, then choose a command (it applies inside)".into())
                    }
                    Area::Text if self.sel.shape.is_none() => {
                        Some("Text: click where the text starts (bottom-left), then choose a command".into())
                    }
                    _ if self.sel.ready() || self.area_mask.is_some() => Some(
                        "Choose a command to process this area (Esc clears it)".into(),
                    ),
                    _ => Some("Outline an area, then choose a command (it applies inside)".into()),
                },
                State::Select(p) => Some(format!(
                    "{}: {}",
                    p.cmd.title(),
                    match self.tools.area {
                        Area::Polygon => "click vertices, double-click to close; then double-click inside or outside",
                        Area::MagicWand => "click a color, then double-click inside or outside",
                        Area::Text => "click where the text starts (bottom-left), drag it, then double-click inside or outside",
                        Area::Freehand => "draw the outline (right-click erases), then double-click inside or outside",
                        _ => "outline the area, then double-click inside or outside",
                    }
                )),
                State::Pen(p, _) if p.cmd == Cmd::Move => Some("Clone: Shift-click the reference point, then paint".into()),
                State::Pen(p, _) => Some(format!("{}: paint with the pen (right button restores)", p.cmd.title())),
                State::Rubber { .. } => Some("Rubber: drag a point to its new place, then double-click".into()),
                State::Place(_) => Some("Drag or resize the fragment (Ctrl = proportional, Shift-click = original size); double-click to accept".into()),
            };
            if let Some(job) = &self.job {
                ui.spinner();
                ui.label(format!("{}…", job.title));
            } else if let Some(h) = hint {
                let esc = if matches!(self.state, State::Idle) { "" } else { "   (Esc cancels)" };
                ui.label(RichText::new(format!("{h}{esc}")).strong());
            } else if let Some(m) = &self.message {
                ui.label(RichText::new(m).color(Color32::from_rgb(220, 80, 60)));
            } else {
                ui.label("Ready");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(d) = self.docs.get(self.active) {
                    ui.label(format!("{:.0}%", d.zoom * 100.0));
                    ui.separator();
                    ui.label(format!("{} × {}", d.img.w, d.img.h));
                    if let Some((x, y)) = self.hover.filter(|(x, y)| *x < d.img.w && *y < d.img.h) {
                        let [r, g, b] = d.img.get(x, y);
                        ui.separator();
                        ui.label(format!("{x}, {y}   R{r} G{g} B{b}"));
                    }
                }
            });
        });
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        match self.dialog.take() {
            Some(Dialog::Op(cmd, mut params)) => {
                let img_size = self
                    .docs
                    .get(self.active)
                    .map_or((640, 480), |d| (d.img.w, d.img.h));
                let mut env = DialogEnv {
                    img_size,
                    choose_pattern: false,
                };
                let mut result = None;
                if apply::has_preview(cmd)
                    && self.preview.is_none()
                    && let Some(d) = self.docs.get(self.active)
                {
                    let thumb = apply::thumbnail(&d.img, 200);
                    let tex = ctx.load_texture(
                        "preview",
                        ColorImage::from_rgb([1, 1], &[0, 0, 0]),
                        TextureOptions::LINEAR,
                    );
                    self.preview = Some((tex, thumb));
                }
                egui::Modal::new(egui::Id::new("op-dialog")).show(ctx, |ui| {
                    ui.heading(cmd.title());
                    ui.add_space(6.0);
                    ui.horizontal_top(|ui| {
                        if let Some((tex, thumb)) = &mut self.preview {
                            let mut rng = self.rng.clone();
                            let full = Mask::full(thumb.w, thumb.h);
                            let mut c = Ctx {
                                area: Area::Whole,
                                mask: &full,
                                circle_pen: false,
                                rng: &mut rng,
                                backup: None,
                            };
                            let out = apply::apply(cmd, &params, thumb, thumb.rect(), &mut c);
                            let raw: Vec<u8> = out.px.iter().flatten().copied().collect();
                            tex.set(
                                ColorImage::from_rgb([out.w, out.h], &raw),
                                TextureOptions::LINEAR,
                            );
                            ui.add(egui::Image::new(&*tex).fit_to_original_size(1.0));
                        }
                        ui.vertical(|ui| apply::dialog_ui(ui, cmd, &mut params, &mut env));
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("OK").clicked()
                            || ui.input(|i| i.key_pressed(egui::Key::Enter))
                        {
                            result = Some(true);
                        }
                        if ui.button("Cancel").clicked()
                            || ui.input(|i| i.key_pressed(egui::Key::Escape))
                        {
                            result = Some(false);
                        }
                    });
                });
                if env.choose_pattern
                    && let Some((img, name)) = self.pick_image("Pattern")
                {
                    params.pattern = Some(Arc::new(img));
                    params.pattern_name = name;
                }
                let pattern_missing = matches!(
                    cmd,
                    Cmd::PatternTiled | Cmd::PatternScaled | Cmd::PatternFitted
                ) && params.pattern.is_none();
                match result {
                    Some(true) if pattern_missing => {
                        self.message = Some("Choose a pattern image first.".into());
                        self.dialog = Some(Dialog::Op(cmd, params));
                    }
                    Some(true) => {
                        self.preview = None;
                        self.begin(cmd, params);
                    }
                    Some(false) => self.preview = None,
                    None => self.dialog = Some(Dialog::Op(cmd, params)),
                }
            }
            Some(Dialog::Text(at, mut params)) => {
                let fonts = self.fonts.get_or_insert_with(crate::text::available_fonts);
                let mut result = None;
                egui::Modal::new(egui::Id::new("text-dialog")).show(ctx, |ui| {
                    ui.heading("Text");
                    apply::text_dialog_ui(ui, &mut params, fonts);
                    ui.horizontal(|ui| {
                        if ui.button("OK").clicked() {
                            result = Some(true);
                        }
                        if ui.button("Cancel").clicked()
                            || ui.input(|i| i.key_pressed(egui::Key::Escape))
                        {
                            result = Some(false);
                        }
                    });
                });
                match result {
                    Some(true) => {
                        let font = fonts
                            .get(params.font)
                            .or(fonts.first())
                            .map(|f| f.data.clone());
                        let mask = font.and_then(|f| {
                            crate::text::render_text_mask(&params.text, &f, params.font_px)
                        });
                        match mask {
                            Some(m) => {
                                self.text_ants = Some(ants_textures(&self.egui, "text", &m));
                                self.sel.shape = Some(Shape::Text(
                                    Arc::new(m.clone()),
                                    pos2(at.x, at.y - m.h as f32),
                                ));
                            }
                            None => self.message = Some("Can't render this text.".into()),
                        }
                        let (f, px, t) = (params.font, params.font_px, params.text.clone());
                        self.params.font = f;
                        self.params.font_px = px;
                        self.params.text = t;
                    }
                    Some(false) => {}
                    None => self.dialog = Some(Dialog::Text(at, params)),
                }
            }
            Some(Dialog::WandOptions) => {
                let mut close = false;
                egui::Modal::new(egui::Id::new("wand")).show(ctx, |ui| {
                    ui.heading("Magic wand options");
                    let mut tol = self.tools.wand_tolerance as i32;
                    ui.add(egui::Slider::new(&mut tol, 1..=100).text("Tolerance"));
                    self.tools.wand_tolerance = tol as u8;
                    ui.horizontal(|ui| {
                        ui.label("Matching");
                        ui.radio_value(&mut self.tools.wand_hsv, false, "RGB");
                        ui.radio_value(&mut self.tools.wand_hsv, true, "HSV");
                    });
                    ui.checkbox(
                        &mut self.tools.wand_unifold,
                        "Unifold (connected region only)",
                    );
                    close =
                        ui.button("OK").clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter));
                });
                if !close {
                    self.dialog = Some(Dialog::WandOptions);
                }
            }
            None => {}
        }

        let splash = ctx.input(|i| i.time) < self.splash_until;
        if self.about || splash {
            let r = egui::Modal::new(egui::Id::new("about")).show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.add(egui::Image::new(self.assets.get("bmp_LOGO")).max_width(220.0));
                    ui.vertical(|ui| {
                        ui.heading("Picture Man");
                        ui.label("True color image processing tool");
                        ui.label("Version 1.55 (1991–1993), rebuilt in Rust");
                        ui.add_space(8.0);
                        ui.label("Authors: Igor 'Potapov' Plotnikov,");
                        ui.label("Mike Kuznetsov, Alex Bobkov");
                        ui.add_space(8.0);
                        ui.label(RichText::new("© Potapov WORKS, STOIK Ltd.").weak());
                        if self.about && ui.button("OK").clicked() {
                            self.about = false;
                        }
                    });
                });
            });
            if r.should_close() || (splash && ctx.input(|i| i.pointer.any_click())) {
                self.about = false;
                self.splash_until = 0.0;
            }
            if splash {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        if self.dialog.is_some() {
            return;
        }
        let cmd = ctx.input_mut(|i| {
            use egui::{Key, KeyboardShortcut as K, Modifiers as M};
            [
                (K::new(M::COMMAND, Key::O), Cmd::Open),
                (K::new(M::COMMAND, Key::N), Cmd::New),
                (K::new(M::COMMAND, Key::S), Cmd::Save),
                (K::new(M::COMMAND, Key::Z), Cmd::Undo),
                (K::new(M::COMMAND, Key::Num1), Cmd::Zoom(4)),
            ]
            .into_iter()
            .find(|(k, _)| i.consume_shortcut(k))
            .map(|(_, c)| c)
        });
        if let Some(c) = cmd {
            self.invoke(c, ctx);
        }
        // Ctrl + wheel (and pinch) step through the zoom levels. egui reports
        // them as a zoom factor spread over several frames: take one step per
        // gesture, then wait until it ends.
        let zd = ctx.input(|i| i.zoom_delta());
        let mut scroll: f32 = 0.0;
        if zd == 1.0 {
            self.zoom_latched = false;
            self.zoom_accum = 0.0;
        } else if !self.zoom_latched {
            self.zoom_accum += zd.ln();
            if self.zoom_accum.abs() > 0.05 {
                scroll = self.zoom_accum.signum();
                self.zoom_latched = true;
            }
        }
        if scroll != 0.0
            && let Some(d) = self.doc()
        {
            let k = ZOOMS.iter().position(|z| *z >= d.zoom).unwrap_or(4) as i32;
            let k = (k + scroll.signum() as i32).clamp(0, ZOOMS.len() as i32 - 1);
            d.zoom = ZOOMS[k as usize];
            d.mark_stale();
        }
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        for p in dropped {
            self.open_path(p);
        }
    }
}

fn item(
    ui: &mut egui::Ui,
    label: &str,
    shortcut: Option<&str>,
    cmd: Cmd,
    enabled: bool,
    chosen: &mut Option<Cmd>,
) {
    let mut b = egui::Button::new(label);
    if let Some(s) = shortcut {
        b = b.shortcut_text(s);
    }
    if ui.add_enabled(enabled, b).clicked() {
        *chosen = Some(cmd);
        ui.close();
    }
}

fn check(ui: &mut egui::Ui, label: &str, on: bool, cmd: Cmd, chosen: &mut Option<Cmd>) {
    let mut v = on;
    if ui.checkbox(&mut v, label).clicked() {
        *chosen = Some(cmd);
    }
}

fn menu_item(ui: &mut egui::Ui, m: &MenuItem, chosen: &mut Option<Cmd>) {
    match m {
        MenuItem::Cmd(l, c) => item(ui, l, None, *c, true, chosen),
        MenuItem::Sub(l, items) => {
            ui.menu_button(*l, |ui| {
                for i in items {
                    menu_item(ui, i, chosen);
                }
            });
        }
        MenuItem::Sep => {
            ui.separator();
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_job(&ctx);
        // Changing the area type ends area entry or painting; a pending
        // command restarts with the new type.
        if self.tools.area != self.last_area {
            self.last_area = self.tools.area;
            match &self.state {
                State::Select(p) | State::Pen(p, _) => {
                    let p = p.clone();
                    self.reset_area();
                    self.begin(p.cmd, p.params);
                }
                State::Idle => self.reset_area(),
                _ => {}
            }
        }
        self.shortcuts(&ctx);

        let title = match self.docs.get(self.active) {
            Some(d) => format!("Picture Man — {}", d.title()),
            None => "Picture Man".into(),
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));

        egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        if self.show_toolbox {
            egui::Panel::left("toolbox")
                .resizable(false)
                .show(ui, |ui| {
                    tools::toolbox(ui, &self.assets, &mut self.tools);
                    ui.add_space(10.0);
                    if ui.button("Magic wand…").clicked() {
                        self.dialog = Some(Dialog::WandOptions);
                    }
                });
        }
        egui::CentralPanel::default().show(ui, |ui| {
            if self.docs.len() > 1 {
                ui.horizontal(|ui| {
                    let (mut close, mut switch) = (None, None);
                    for (i, d) in self.docs.iter().enumerate() {
                        if ui.selectable_label(i == self.active, d.title()).clicked()
                            && i != self.active
                        {
                            switch = Some(i);
                        }
                        if i == self.active && ui.small_button("✕").clicked() {
                            close = Some(i);
                        }
                    }
                    if let Some(i) = switch {
                        self.active = i;
                        self.reset_area();
                    }
                    if let Some(i) = close {
                        self.docs.remove(i);
                        self.active = self.active.min(self.docs.len().saturating_sub(1));
                        self.reset_area();
                    }
                });
                ui.separator();
            }
            let mode = match &self.state {
                // With an area tool, an area can be outlined before choosing
                // the command.
                State::Idle if !matches!(self.tools.area, Area::Whole | Area::Pen) => {
                    Mode::Select(self.tools.area)
                }
                State::Idle => Mode::Idle,
                State::Select(_) => Mode::Select(self.tools.area),
                State::Pen(..) => Mode::Pen,
                State::Rubber { roi, from, to, .. } => Mode::Rubber {
                    roi: egui::Rect::from_min_size(
                        pos2(roi.x as f32, roi.y as f32),
                        vec2(roi.w as f32, roi.h as f32),
                    ),
                    from: *from,
                    to: *to,
                },
                State::Place(pl) => Mode::Place {
                    rect: pl.rect,
                    tex: pl.tex.id(),
                    orig: vec2(pl.frag.img.w as f32, pl.frag.img.h as f32),
                },
            };
            let phase = canvas::march_phase(ctx.input(|i| i.time), self.tools.animate);
            let overlay = Overlay {
                tex: self.ants.as_ref().map(|t| t[phase].id()),
                text: match (&self.text_ants, &self.sel.shape) {
                    (Some(t), Some(Shape::Text(m, at))) => Some((
                        t[phase].id(),
                        egui::Rect::from_min_size(*at, vec2(m.w as f32, m.h as f32)),
                    )),
                    _ => None,
                },
            };
            if (self.ants.is_some() || self.text_ants.is_some()) && self.tools.animate {
                ctx.request_repaint_after(std::time::Duration::from_millis(canvas::MARCH_MS));
            }
            let tools = &self.tools;
            let sel = &mut self.sel;
            let events = match self.docs.get_mut(self.active) {
                Some(d) => canvas::canvas(ui, d, sel, mode, tools, overlay),
                None => {
                    ui.centered_and_justified(|ui| {
                        ui.label(RichText::new("Open an image (Ctrl+O) or drop files here").weak());
                    });
                    Vec::new()
                }
            };
            self.handle_canvas(events);
        });

        self.dialogs(&ctx);
        if self.job.is_none() {
            self.save_settings_if_changed();
        }
    }
}
