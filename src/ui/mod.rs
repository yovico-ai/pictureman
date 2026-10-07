//! The Picture Man window: menus, toolbox, tool options, tabs, canvas,
//! dialogs and status bar.
//!
//! Interaction model: pick a selection tool and draw — the selection stays
//! until cleared (Shift adds, Alt subtracts, drag inside to move); every
//! command applies to the selection, or to the whole image when there is
//! none. The brush paints with an effect: while it is the current tool,
//! choosing an Adjust, Fill or Filter command makes it the brush's effect
//! (the original's "paint with any operation").

mod apply;
mod assets;
mod canvas;
mod commands;
mod doc;
mod marquee;
mod platform;
mod settings;
mod tools;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};

use eframe::egui::{
    self, Color32, ColorImage, RichText, TextureHandle, TextureOptions, pos2, vec2,
};

use self::apply::{Ctx, DialogEnv, OpParams, Scope};
pub use self::assets::Assets as AppAssets;
use self::assets::Assets;
use self::canvas::{Event, Interaction, Mode, View};
use self::commands::{Cmd, MenuItem};
use self::doc::Doc;
use self::marquee::{Combine, Selection};
use self::tools::{Tool, ToolboxAction, Tools};
use crate::core::{Image, MAX_PIXELS, Mask, MsRand, Rect};
use crate::formats::{self, Format};
use crate::ops::transform::{self, Fragment};
use crate::selection::{self as sel, AreaKind};
use crate::text::FontEntry;

/// The View menu's fixed zoom steps (1:8 … 8:1), as in the original.
const ZOOMS: [f32; 9] = [0.125, 1.0 / 6.0, 0.25, 0.5, 1.0, 2.0, 4.0, 6.0, 8.0];

#[derive(Clone)]
struct Pending {
    cmd: Cmd,
    params: OpParams,
}

/// What an operation works on: the selected pixels, and the selection (for
/// the soft-edge weights, computed off the UI thread) unless it is the
/// whole image.
#[derive(Clone)]
struct Chosen {
    selected: Mask,
    selection: Option<(Selection, sel::Edge)>,
    scope: Scope,
}

/// Painting with the brush. Operations that allow it are computed once for
/// the whole image when the session starts (see `apply::pen_once`); dabs
/// reveal that result.
struct BrushSession {
    doc: u64,
    version: u64,
    effect: u64,
    /// The image when the session started: filters read from it, so strokes
    /// don't compound; the right button restores it.
    backup: Image,
    processed: Option<Image>,
    preparing: Option<Receiver<Image>>,
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
    Rubber {
        pending: Pending,
        roi: Rect,
        from: Option<egui::Pos2>,
        to: Option<egui::Pos2>,
    },
    Place(Placing),
}

enum Dialog {
    Op(Cmd, OpParams),
    Text(egui::Pos2, Combine, OpParams),
    WandOptions,
    /// Save As in the browser: the download's file name.
    #[cfg(target_arch = "wasm32")]
    SaveName(String),
}

/// An operation running on a worker thread.
struct Job {
    doc: u64,
    title: &'static str,
    rx: Receiver<Result<(Image, MsRand), String>>,
}

/// Live preview of the open dialog's operation, shown on the canvas.
#[derive(Default)]
struct Preview {
    /// Parameters of the result in `img`.
    key: String,
    img: Option<Image>,
    tex: Option<TextureHandle>,
    computing: Option<(String, Receiver<Image>)>,
    doc: u64,
    version: u64,
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
    interaction: Interaction,
    job: Option<Job>,
    rng: MsRand,
    fonts: Option<Vec<FontEntry>>,
    preview: Preview,
    show_preview: bool,
    hover: Option<(usize, usize)>,
    message: Option<String>,
    about: bool,
    splash_until: f64,
    recent: Vec<PathBuf>,
    files: platform::Files,
    /// Paste-from parameters while the file picker is open.
    paste_params: Option<OpParams>,
    brush_effect: Pending,
    effect_gen: u64,
    brush: Option<BrushSession>,
    /// A menu or popup was open when the frame started (Esc closes it
    /// before the canvas sees the key).
    popup_was_open: bool,
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

fn image_texture(ctx: &egui::Context, name: &str, img: &Image) -> TextureHandle {
    let raw: Vec<u8> = img.px.iter().flatten().copied().collect();
    ctx.load_texture(
        name,
        ColorImage::from_rgb([img.w, img.h], &raw),
        TextureOptions::NEAREST,
    )
}

/// Run `cmd` on `src` and commit it through the selection's soft edge.
fn compute(
    cmd: Cmd,
    params: &OpParams,
    src: &Image,
    chosen: &Chosen,
    backup: Option<&Image>,
    rng: &mut MsRand,
) -> Image {
    let roi = chosen.selected.bounds();
    let processed = {
        let mut ctx = Ctx {
            scope: chosen.scope,
            mask: &chosen.selected,
            rng,
            backup,
        };
        apply::apply(cmd, params, src, roi, &mut ctx)
    };
    match &chosen.selection {
        Some((s, e)) => {
            let mut out = src.clone();
            sel::commit(&mut out, &processed, &s.weights(*e));
            out
        }
        None => processed,
    }
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, files: Vec<PathBuf>) -> Self {
        let mut tools = Tools::default();
        let mut params = OpParams::default();
        let loaded = settings::load(&mut tools, &mut params);
        let brush_effect = Pending {
            cmd: Cmd::FillPlain,
            params: params.clone(),
        };
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
            interaction: Interaction::default(),
            job: None,
            rng: MsRand::default(),
            fonts: None,
            preview: Preview::default(),
            show_preview: true,
            hover: None,
            message: None,
            about: false,
            splash_until: 2.5,
            recent: loaded.recent,
            files: platform::Files::default(),
            paste_params: None,
            brush_effect,
            effect_gen: 0,
            brush: None,
            popup_was_open: false,
            saved_settings: String::new(),
        };
        app.saved_settings = app.settings_text();
        for f in files {
            app.open_path(f);
        }
        app
    }

    fn doc(&mut self) -> Option<&mut Doc> {
        self.docs.get_mut(self.active)
    }

    fn selection(&self) -> Option<&Selection> {
        self.docs.get(self.active)?.selection.as_ref()
    }

    fn set_selection(&mut self, s: Option<Selection>) {
        if let Some(d) = self.doc() {
            d.selection = s;
        }
    }

    fn add_doc(&mut self, name: String, path: Option<PathBuf>, img: Image) {
        let id = self.next_id;
        self.next_id += 1;
        self.docs.push(Doc::new(id, name, path, img));
        self.switch_to(self.docs.len() - 1);
    }

    fn switch_to(&mut self, i: usize) {
        self.active = i;
        self.state = State::Idle;
        self.interaction.clear();
        self.dialog = None;
        self.preview = Preview::default();
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

    /// Files arrive from the file picker (on the desktop at once, in the
    /// browser asynchronously) or dropped on the window.
    fn handle_picked(&mut self, p: platform::Picked) {
        if p.bytes.is_empty() {
            self.message = Some(format!("Can't open file {}", p.name));
            return;
        }
        let hint = Format::from_path(std::path::Path::new(&p.name));
        let img = match formats::load_bytes(&p.bytes, hint) {
            Ok(img) => img,
            Err(e) => {
                self.message = Some(format!("Can't open file {}: {e}", p.name));
                return;
            }
        };
        match p.purpose {
            platform::Purpose::Open => {
                if let Some(path) = &p.path {
                    self.remember(path);
                }
                self.add_doc(p.name, p.path, img);
            }
            platform::Purpose::Pattern => {
                if let Some(Dialog::Op(_, params)) = &mut self.dialog {
                    params.pattern = Some(Arc::new(img));
                    params.pattern_name = p.name;
                }
            }
            platform::Purpose::PasteFrom => {
                if let Some(params) = self.paste_params.take() {
                    self.start_place(apply::paste_fragment(img, &params));
                }
            }
        }
    }

    /// In the browser, saving downloads the image (Save As asks for a name).
    #[cfg(target_arch = "wasm32")]
    fn save(&mut self, ask: bool) {
        let Some(doc) = self.docs.get(self.active) else {
            return;
        };
        if ask {
            self.dialog = Some(Dialog::SaveName(doc.name.clone()));
        } else {
            let name = doc.name.clone();
            self.download(name);
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn download(&mut self, mut name: String) {
        let Some(doc) = self.docs.get_mut(self.active) else {
            return;
        };
        let format = match Format::from_path(std::path::Path::new(&name)) {
            Some(f) => f,
            None => {
                name.push_str(".png");
                Format::Png
            }
        };
        let r = formats::encode(&doc.img, format)
            .map_err(|e| e.to_string())
            .and_then(|bytes| platform::download(&name, &bytes));
        match r {
            Ok(()) => {
                doc.name = name;
                doc.modified = false;
            }
            Err(e) => self.message = Some(format!("Can't save file: {e}")),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn save(&mut self, ask: bool) {
        let Some(doc) = self.docs.get_mut(self.active) else {
            return;
        };
        let path = match (&doc.path, ask) {
            (Some(p), false) => Some(p.clone()),
            _ => {
                let mut d = rfd::FileDialog::new()
                    .set_title("Save As")
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

    /// A menu command or shortcut.
    fn invoke(&mut self, cmd: Cmd, ctx: &egui::Context) {
        if cmd.needs_image() && (self.docs.is_empty() || self.job.is_some()) {
            return;
        }
        self.message = None;
        match cmd {
            Cmd::New | Cmd::Size | Cmd::Paste | Cmd::PasteFrom => self.open_op_dialog(cmd),
            Cmd::Open => self.files.request(platform::Purpose::Open, "Open"),
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
                self.state = State::Idle;
            }
            Cmd::Exit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Cmd::Undo => {
                self.state = State::Idle;
                if let Some(d) = self.doc() {
                    d.undo();
                }
            }
            Cmd::Redo => {
                self.state = State::Idle;
                if let Some(d) = self.doc() {
                    d.redo();
                }
            }
            Cmd::SelectAll => {
                let s = self
                    .docs
                    .get(self.active)
                    .and_then(|d| Selection::all(d.img.w, d.img.h));
                self.set_selection(s);
            }
            Cmd::SelectNone => self.set_selection(None),
            Cmd::InvertSelection => {
                let s = match self.selection() {
                    Some(s) => s.inverted(),
                    None => self
                        .docs
                        .get(self.active)
                        .and_then(|d| Selection::all(d.img.w, d.img.h)),
                };
                self.set_selection(s);
            }
            Cmd::Zoom(z) => self.set_zoom(ZOOMS[z as usize]),
            Cmd::ZoomIn => self.zoom_by(1.25),
            Cmd::ZoomOut => self.zoom_by(0.8),
            Cmd::ZoomFit => {
                if let Some(d) = self.doc() {
                    d.fit_pending = true;
                }
            }
            Cmd::Toolbox => self.show_toolbox = !self.show_toolbox,
            Cmd::AnimateSelection => self.tools.animate = !self.tools.animate,
            Cmd::CreateBackup => self.tools.backup = !self.tools.backup,
            Cmd::MagicWandOptions => self.dialog = Some(Dialog::WandOptions),
            Cmd::About => self.about = true,
            Cmd::Copy => {
                let p = self.params.clone();
                self.execute(Cmd::Copy, p);
            }
            c if c.is_image_op() => self.open_op_dialog(c),
            _ => {}
        }
    }

    fn set_zoom(&mut self, z: f32) {
        if let Some(d) = self.doc() {
            let crossed = (d.zoom >= 1.0) != (z >= 1.0);
            d.zoom = z.clamp(1.0 / 32.0, 32.0);
            if crossed {
                d.mark_stale(); // texture filtering changes at 1:1
            }
        }
    }

    fn zoom_by(&mut self, f: f32) {
        if let Some(z) = self.docs.get(self.active).map(|d| d.zoom) {
            self.set_zoom(z * f);
        }
    }

    fn open_op_dialog(&mut self, cmd: Cmd) {
        let mut params = self.params.clone();
        params.color = self.tools.color;
        apply::prepare(cmd, &mut params, self.docs.get(self.active).map(|d| &d.img));
        if apply::needs_dialog(cmd) {
            self.preview = Preview::default();
            self.dialog = Some(Dialog::Op(cmd, params));
        } else {
            self.execute(cmd, params);
        }
    }

    /// Parameters are known: set the brush's effect, or apply to the
    /// selection (or the whole image).
    fn execute(&mut self, cmd: Cmd, params: OpParams) {
        self.params = params.clone();
        if self.tools.tool == Tool::Brush && cmd.paintable() {
            self.brush_effect = Pending { cmd, params };
            self.effect_gen += 1;
            self.brush = None;
            return;
        }
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
            #[cfg(target_arch = "wasm32")]
            Cmd::Paste => {
                self.message = Some(
                    "Paste from the clipboard isn't available in the browser; use Paste from…"
                        .into(),
                );
                return;
            }
            #[cfg(not(target_arch = "wasm32"))]
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
                self.paste_params = Some(params);
                self.files
                    .request(platform::Purpose::PasteFrom, "Paste from");
                return;
            }
            _ => {}
        }
        self.apply_to_selection(cmd, params);
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

    /// The selection, or the whole image.
    fn chosen(&self) -> Option<Chosen> {
        let d = self.docs.get(self.active)?;
        Some(match &d.selection {
            Some(s) => Chosen {
                selected: s.mask.clone(),
                selection: Some((s.clone(), edge(self.tools.edge))),
                scope: Scope::Selection {
                    elliptic: s.elliptic,
                },
            },
            None => Chosen {
                selected: Mask::full(d.img.w, d.img.h),
                selection: None,
                scope: Scope::Whole,
            },
        })
    }

    fn apply_to_selection(&mut self, cmd: Cmd, params: OpParams) {
        let Some(chosen) = self.chosen() else { return };
        let Some(d) = self.docs.get(self.active) else {
            return;
        };
        let roi = chosen.selected.bounds();
        let selected = d.selection.as_ref();
        match cmd {
            #[cfg(target_arch = "wasm32")]
            Cmd::Copy => {
                let _ = (d, roi, selected);
                self.message =
                    Some("Copy to the clipboard isn't available in the browser; use Save".into());
            }
            #[cfg(not(target_arch = "wasm32"))]
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
            Cmd::Clip if selected.is_none() => {
                self.message = Some("Select the area to crop to first.".into())
            }
            Cmd::Clip => self.run_job(cmd, move |src, rng| Ok((src.crop(roi), rng))),
            Cmd::Rotate if selected.is_none() => {
                let (angle, bg) = (params.angle, params.color);
                self.run_job(cmd, move |src, rng| {
                    transform::rotate(&src, angle, bg)
                        .map(|i| (i, rng))
                        .ok_or_else(|| "Incorrect parameters.".to_string())
                });
            }
            Cmd::Move if selected.is_none() => {
                self.message = Some("Select the area to move first.".into())
            }
            Cmd::Rubber => {
                self.state = State::Rubber {
                    pending: Pending { cmd, params },
                    roi,
                    from: None,
                    to: None,
                };
            }
            c if apply::is_fragment_op(c) && selected.is_some() => {
                let elliptic = selected.is_some_and(|s| s.elliptic);
                if let Some(frag) =
                    apply::make_fragment(c, &params, &d.img, &chosen.selected, roi, elliptic)
                {
                    self.start_place(frag);
                }
            }
            _ => self.run_pixel(cmd, params, chosen),
        }
    }

    /// Run a pixel operation on a worker thread.
    fn run_pixel(&mut self, cmd: Cmd, params: OpParams, chosen: Chosen) {
        let Some(d) = self.docs.get(self.active) else {
            return;
        };
        let backup = d.last_backup().cloned();
        self.run_job(cmd, move |src, mut rng| {
            let out = compute(cmd, &params, &src, &chosen, backup.as_ref(), &mut rng);
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
        let rx = platform::spawn(move || {
            // A panic in an operation must not take the program down.
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(src, rng)))
                .unwrap_or_else(|_| Err("Unknown error.".to_string()))
        });
        let title = if cmd.title().is_empty() {
            "Processing"
        } else {
            cmd.title()
        };
        self.job = Some(Job {
            doc: d.id,
            title,
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
                                d.fit_pending = true;
                            }
                        }
                    }
                    Err(e) => self.message = Some(e),
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
        self.interaction.clear();
        let rect = egui::Rect::from_min_size(
            pos2(frag.x as f32, frag.y as f32),
            vec2(frag.img.w as f32, frag.img.h as f32),
        );
        let tex = fragment_texture(&self.egui, &frag);
        self.state = State::Place(Placing { frag, rect, tex });
    }

    fn accept_place(&mut self) {
        let State::Place(pl) = std::mem::replace(&mut self.state, State::Idle) else {
            return;
        };
        let backup = self.tools.backup;
        let Some(d) = self.docs.get_mut(self.active) else {
            return;
        };
        let r = pl.rect;
        let (w, h) = (
            r.width().round().max(1.0) as usize,
            r.height().round().max(1.0) as usize,
        );
        if w.saturating_mul(h) > MAX_PIXELS {
            self.message = Some("Fragment too large.".into());
            return;
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
        // The placed fragment becomes the selection.
        d.selection = Selection::new(mask, AreaKind::Rect, false);
    }

    fn apply_rubber(&mut self) {
        let state = std::mem::replace(&mut self.state, State::Idle);
        let State::Rubber {
            mut pending,
            roi,
            from: Some(a),
            to: Some(b),
        } = state
        else {
            self.state = state;
            return;
        };
        let ip = |p: egui::Pos2| (p.x.round() as i64, p.y.round() as i64);
        pending.params.rubber = (ip(a), ip(b));
        let Some(chosen) = self.chosen() else { return };
        match (&chosen.selection, self.docs.get(self.active)) {
            (Some((s, _)), Some(d)) => {
                if let Some(f) = apply::make_fragment(
                    Cmd::Rubber,
                    &pending.params,
                    &d.img,
                    &s.mask,
                    roi,
                    s.elliptic,
                ) {
                    self.start_place(f);
                }
            }
            _ => self.run_pixel(pending.cmd, pending.params, chosen),
        }
    }

    fn handle_canvas(&mut self, events: Vec<Event>) {
        for e in events {
            match e {
                Event::Hover(h) => self.hover = h,
                // Esc closes menus and dialogs first; only then does it act
                // on the canvas.
                Event::Escape if self.dialog.is_some() || self.popup_was_open => {}
                Event::Escape => match self.state {
                    State::Idle => self.set_selection(None),
                    _ => self.state = State::Idle,
                },
                Event::Picked(c) => self.tools.color = c,
                Event::ShapeDone(shape, op) => {
                    let Some(d) = self.docs.get(self.active) else {
                        continue;
                    };
                    if shape.is_degenerate() {
                        continue;
                    }
                    let mask = shape.to_mask(d.img.w, d.img.h);
                    let (kind, elliptic) = match shape {
                        canvas::Shape::Rect(..) => (AreaKind::Rect, false),
                        canvas::Shape::Ellipse(..) => (AreaKind::Ellipse, true),
                        canvas::Shape::Polygon(_) => (AreaKind::Polygon, false),
                        canvas::Shape::Lasso(_) => (AreaKind::Freehand, false),
                    };
                    let s = Selection::combine(d.selection.as_ref(), mask, kind, elliptic, op);
                    self.set_selection(s);
                }
                Event::WandClick(p, op) => {
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
                    let s =
                        Selection::combine(d.selection.as_ref(), m, AreaKind::MagicWand, false, op);
                    self.set_selection(s);
                }
                Event::TextAt(p, op) => {
                    if self.fonts.is_none() {
                        self.fonts = Some(crate::text::available_fonts());
                    }
                    self.dialog = Some(Dialog::Text(p, op, self.params.clone()));
                }
                Event::MoveSelection(dx, dy) => {
                    let s = self.selection().and_then(|s| s.translated(dx, dy));
                    self.set_selection(s);
                }
                Event::Deselect => self.set_selection(None),
                Event::BrushDown { erase } => self.brush_down(erase),
                Event::BrushDab(p) => self.brush_dab(p),
                Event::BrushUp => {
                    if let Some(s) = &mut self.brush {
                        s.stroke = None;
                        s.last = None;
                    }
                }
                Event::CloneRef(p) => {
                    self.ensure_brush();
                    if let Some(s) = &mut self.brush {
                        s.clone_ref = Some(p);
                    }
                }
                Event::RubberSet(a, b) => {
                    if let State::Rubber { from, to, .. } = &mut self.state {
                        *from = Some(a);
                        *to = Some(b);
                    }
                }
                Event::RubberApply => self.apply_rubber(),
                Event::PlaceSet(r) => {
                    if let State::Place(pl) = &mut self.state {
                        pl.rect = r;
                    }
                }
                Event::PlaceAccept => self.accept_place(),
            }
        }
    }

    /// Keep a painting session for the current image and effect, preparing
    /// the effect in the background when it can be computed once.
    fn ensure_brush(&mut self) {
        if self.tools.tool != Tool::Brush {
            self.brush = None;
            return;
        }
        let Some(d) = self.docs.get(self.active) else {
            return;
        };
        if self
            .brush
            .as_ref()
            .is_some_and(|s| s.doc == d.id && s.version == d.version && s.effect == self.effect_gen)
        {
            return;
        }
        let backup = d.img.clone();
        let fx = &self.brush_effect;
        let preparing = apply::pen_once(fx.cmd).then(|| {
            let (cmd, params, src) = (fx.cmd, fx.params.clone(), backup.clone());
            let (circle, mut rng) = (self.tools.brush == tools::Brush::Circle, self.rng.clone());
            platform::spawn(move || {
                let full = Mask::full(src.w, src.h);
                let mut ctx = Ctx {
                    scope: Scope::Brush { circle },
                    mask: &full,
                    rng: &mut rng,
                    backup: Some(&src),
                };
                apply::apply(cmd, &params, &src, src.rect(), &mut ctx)
            })
        });
        self.brush = Some(BrushSession {
            doc: d.id,
            version: d.version,
            effect: self.effect_gen,
            backup,
            processed: None,
            preparing,
            stroke: None,
            last: None,
            erase: false,
            clone_ref: None,
            clone_offset: None,
        });
    }

    fn poll_brush(&mut self, ctx: &egui::Context) {
        if let Some(s) = &mut self.brush
            && let Some(rx) = &s.preparing
        {
            match rx.try_recv() {
                Ok(img) => {
                    s.processed = Some(img);
                    s.preparing = None;
                }
                Err(mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(30))
                }
                Err(mpsc::TryRecvError::Disconnected) => s.preparing = None,
            }
        }
    }

    fn brush_down(&mut self, erase: bool) {
        self.ensure_brush();
        let backup = self.tools.backup;
        if let (Some(s), Some(d)) = (&mut self.brush, self.docs.get_mut(self.active)) {
            // One undo step per stroke.
            if backup {
                d.push_undo();
            }
            s.stroke = Some(sel::Stroke::begin(3));
            s.last = None;
            s.erase = erase;
            s.clone_offset = None;
        }
    }

    fn brush_dab(&mut self, p: egui::Pos2) {
        let (Some(s), Some(d)) = (&mut self.brush, self.docs.get_mut(self.active)) else {
            return;
        };
        let Some(stroke) = s.stroke.as_mut() else {
            return;
        };
        let fx = &self.brush_effect;
        let cmd = fx.cmd;
        if apply::pen_once(cmd) && !s.erase && s.processed.is_none() {
            return; // still preparing; the status bar says so
        }
        let size = self.tools.pen_size as usize;
        let kind = if self.tools.brush == tools::Brush::Circle {
            sel::BrushKind::Circle
        } else {
            sel::BrushKind::Square
        };
        let pen = sel::PenParams {
            brush: sel::Brush { size, kind },
            edge: edge(self.tools.brush_edge),
            airbrush: 16000,
        };
        if cmd == Cmd::Move && !s.erase && s.clone_offset.is_none() {
            let Some(r) = s.clone_ref else {
                self.message = Some("Shift-click the spot to clone from first".into());
                return;
            };
            s.clone_offset = Some(((r.x - p.x).round() as i64, (r.y - p.y).round() as i64));
        }
        // Dabs along the drag (the original only dabbed at mouse-move events,
        // which leaves gaps on fast strokes).
        let from = s.last.unwrap_or(p);
        let spacing = (size as f32 / 2.0).max(1.0);
        let n = ((from.distance(p) / spacing).ceil() as usize).max(1);
        for i in 1..=n {
            let c = if s.last.is_none() {
                p
            } else {
                from.lerp(p, i as f32 / n as f32)
            };
            let ci = (c.x.floor() as i32, c.y.floor() as i32);
            // Everything happens on the dab's square only.
            let r = sel::dab_rect(ci, size, d.img.w, d.img.h);
            if r.is_empty() {
                stroke.advance();
                continue;
            }
            let local = (ci.0 - r.x as i32, ci.1 - r.y as i32);
            let before = d.img.crop(r);
            let mut out = before.clone();
            if s.erase {
                sel::paint_dab(
                    &mut out,
                    &s.backup.crop(r),
                    &before,
                    local,
                    &pen,
                    stroke,
                    &mut self.rng,
                );
            } else {
                // Fills accumulate on the current image, the rest reads the
                // image as it was when painting started.
                let source = if apply::is_fill(cmd) {
                    before.clone()
                } else {
                    s.backup.crop(r)
                };
                let processed = if cmd == Cmd::Move {
                    let (dx, dy) = s.clone_offset.unwrap_or((0, 0));
                    let mut img = Image::new(r.w, r.h, fx.params.color);
                    for y in 0..r.h {
                        for x in 0..r.w {
                            let (sx, sy) = ((r.x + x) as i64 + dx, (r.y + y) as i64 + dy);
                            if sx >= 0
                                && sy >= 0
                                && (sx as usize) < s.backup.w
                                && (sy as usize) < s.backup.h
                            {
                                img.set(x, y, s.backup.get(sx as usize, sy as usize));
                            }
                        }
                    }
                    img
                } else if let Some(pre) = &s.processed {
                    pre.crop(r)
                } else {
                    let full = Mask::full(r.w, r.h);
                    let mut ctx = Ctx {
                        scope: Scope::Brush {
                            circle: kind == sel::BrushKind::Circle,
                        },
                        mask: &full,
                        rng: &mut self.rng,
                        backup: Some(&s.backup),
                    };
                    apply::apply(cmd, &fx.params, &source, source.rect(), &mut ctx)
                };
                sel::paint_dab(
                    &mut out,
                    &processed,
                    &source,
                    local,
                    &pen,
                    stroke,
                    &mut self.rng,
                );
            }
            // Painting stays inside the selection.
            if let Some(selm) = &d.selection {
                for y in 0..r.h {
                    for x in 0..r.w {
                        if selm.mask.get(r.x + x, r.y + y) == 0 {
                            out.set(x, y, before.get(x, y));
                        }
                    }
                }
            }
            for y in 0..r.h {
                d.img.row_mut(r.y + y)[r.x..r.x + r.w].copy_from_slice(out.row(y));
            }
            d.touch_rect(r);
            stroke.advance();
            if s.last.is_none() {
                break;
            }
        }
        s.last = Some(p);
    }

    /// Keep the open dialog's live preview up to date (computed off the UI
    /// thread; the latest parameters win).
    fn update_preview(&mut self, ctx: &egui::Context) {
        let Some(Dialog::Op(cmd, params)) = &self.dialog else {
            return;
        };
        if !self.show_preview || !apply::has_preview(*cmd) || self.tools.tool == Tool::Brush {
            return;
        }
        let Some(d) = self.docs.get(self.active) else {
            return;
        };
        let key = apply::preview_key(*cmd, params);
        if let Some((k, rx)) = &self.preview.computing {
            match rx.try_recv() {
                Ok(img) => {
                    let k = k.clone();
                    match &mut self.preview.tex {
                        Some(t) => {
                            let raw: Vec<u8> = img.px.iter().flatten().copied().collect();
                            t.set(
                                ColorImage::from_rgb([img.w, img.h], &raw),
                                TextureOptions::NEAREST,
                            );
                        }
                        None => self.preview.tex = Some(image_texture(ctx, "preview", &img)),
                    }
                    self.preview.img = Some(img);
                    self.preview.key = k;
                    self.preview.computing = None;
                }
                Err(mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(30));
                    return;
                }
                Err(mpsc::TryRecvError::Disconnected) => self.preview.computing = None,
            }
        }
        let stale = self.preview.doc != d.id || self.preview.version != d.version;
        if (self.preview.key != key || stale || self.preview.img.is_none())
            && self.preview.computing.is_none()
        {
            let Some(chosen) = self.chosen() else { return };
            let (cmd, params, src) = (*cmd, params.clone(), d.img.clone());
            let backup = d.last_backup().cloned();
            let mut rng = self.rng.clone();
            self.preview.doc = d.id;
            self.preview.version = d.version;
            if stale {
                self.preview.img = None;
            }
            let rx = platform::spawn(move || {
                compute(cmd, &params, &src, &chosen, backup.as_ref(), &mut rng)
            });
            self.preview.computing = Some((key, rx));
            ctx.request_repaint_after(std::time::Duration::from_millis(30));
        }
    }

    /// OK in a dialog: use the finished preview if it matches, else run it.
    fn accept_dialog(&mut self, cmd: Cmd, params: OpParams) {
        let key = apply::preview_key(cmd, &params);
        let ready = self.preview.img.is_some()
            && self.preview.key == key
            && self.tools.tool != Tool::Brush
            && self
                .docs
                .get(self.active)
                .is_some_and(|d| d.id == self.preview.doc && d.version == self.preview.version)
            && apply::has_preview(cmd)
            && !(apply::is_fragment_op(cmd) && self.selection().is_some());
        if ready {
            let img = self.preview.img.take().unwrap();
            self.params = params;
            let backup = self.tools.backup;
            if let Some(d) = self.doc() {
                if backup {
                    d.push_undo();
                }
                d.set_image(img);
            }
        } else {
            self.execute(cmd, params);
        }
        self.preview = Preview::default();
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        let mut chosen = None;
        let mut open_recent: Option<PathBuf> = None;
        let has_doc = !self.docs.is_empty();
        let d = self.docs.get(self.active);
        let (can_undo, can_redo) = (
            d.is_some_and(|d| d.can_undo()),
            d.is_some_and(|d| d.can_redo()),
        );
        let has_sel = d.is_some_and(|d| d.selection.is_some());
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                item(ui, "New…", Some("Ctrl+N"), Cmd::New, true, &mut chosen);
                item(ui, "Open…", Some("Ctrl+O"), Cmd::Open, true, &mut chosen);
                ui.add_enabled_ui(!self.recent.is_empty(), |ui| {
                    ui.menu_button("Open recent", |ui| {
                        for p in &self.recent {
                            let name = p.file_name().map_or_else(
                                || p.display().to_string(),
                                |n| n.to_string_lossy().into_owned(),
                            );
                            if ui
                                .button(name)
                                .on_hover_text(p.display().to_string())
                                .clicked()
                            {
                                open_recent = Some(p.clone());
                                ui.close();
                            }
                        }
                    });
                });
                item(ui, "Reload", None, Cmd::Reload, has_doc, &mut chosen);
                ui.separator();
                item(ui, "Save", Some("Ctrl+S"), Cmd::Save, has_doc, &mut chosen);
                item(
                    ui,
                    "Save As…",
                    Some("Ctrl+Shift+S"),
                    Cmd::SaveAs,
                    has_doc,
                    &mut chosen,
                );
                #[cfg(not(target_arch = "wasm32"))]
                {
                    ui.separator();
                    item(ui, "Exit", None, Cmd::Exit, true, &mut chosen);
                }
            });
            for (title, items) in commands::image_menus() {
                ui.menu_button(title, |ui| {
                    ui.add_enabled_ui(has_doc && self.job.is_none(), |ui| {
                        for m in &items {
                            let enabled = |c: Cmd| match c {
                                Cmd::Undo => can_undo,
                                Cmd::Redo => can_redo,
                                Cmd::SelectNone | Cmd::Clip | Cmd::Move => has_sel,
                                _ => true,
                            };
                            menu_item(ui, m, &enabled, &mut chosen);
                        }
                        if title == "Edit" {
                            ui.separator();
                            check(
                                ui,
                                "Undo history",
                                self.tools.backup,
                                Cmd::CreateBackup,
                                &mut chosen,
                            );
                        }
                    });
                });
            }
            ui.menu_button("View", |ui| {
                ui.add_enabled_ui(has_doc, |ui| {
                    item(
                        ui,
                        "Zoom in",
                        Some("Ctrl++"),
                        Cmd::ZoomIn,
                        true,
                        &mut chosen,
                    );
                    item(
                        ui,
                        "Zoom out",
                        Some("Ctrl+-"),
                        Cmd::ZoomOut,
                        true,
                        &mut chosen,
                    );
                    item(
                        ui,
                        "Fit in window",
                        Some("Ctrl+0"),
                        Cmd::ZoomFit,
                        true,
                        &mut chosen,
                    );
                    item(
                        ui,
                        "Original size [1:1]",
                        Some("Ctrl+1"),
                        Cmd::Zoom(4),
                        true,
                        &mut chosen,
                    );
                    ui.menu_button("Zoom", |ui| {
                        for (i, l) in [
                            (0, "1:8"),
                            (1, "1:6"),
                            (2, "1:4"),
                            (3, "1:2"),
                            (5, "2:1"),
                            (6, "4:1"),
                            (7, "6:1"),
                            (8, "8:1"),
                        ] {
                            item(ui, l, None, Cmd::Zoom(i), true, &mut chosen);
                        }
                    });
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
            ui.menu_button("Help", |ui| {
                item(
                    ui,
                    "About Picture Man…",
                    None,
                    Cmd::About,
                    true,
                    &mut chosen,
                );
            });
        });
        if let Some(c) = chosen {
            self.invoke(c, ui.ctx());
        }
        if let Some(p) = open_recent {
            self.open_path(p);
        }
    }

    /// The tool options bar under the menus.
    fn options_bar(&mut self, ui: &mut egui::Ui) {
        let mut cmd = None;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            match &mut self.state {
                State::Place(_) => {
                    ui.label(RichText::new("Place").strong());
                    ui.label("Drag to move, drag the corners to resize (Ctrl keeps proportions, Shift-click = original size)");
                    if ui.button("✔ Apply").clicked() {
                        self.accept_place();
                    }
                    if ui.button("Cancel").clicked() {
                        self.state = State::Idle;
                    }
                    return;
                }
                State::Rubber { from, .. } => {
                    let ready = from.is_some();
                    ui.label(RichText::new("Rubber").strong());
                    ui.label("Drag a point to where it should go");
                    if ui.add_enabled(ready, egui::Button::new("✔ Apply")).clicked() {
                        self.apply_rubber();
                    }
                    if ui.button("Cancel").clicked() {
                        self.state = State::Idle;
                    }
                    return;
                }
                State::Idle => {}
            }
            let t = &mut self.tools;
            ui.label(RichText::new(t.tool.name()).strong());
            ui.separator();
            match t.tool {
                Tool::Brush => {
                    let fx = &self.brush_effect;
                    ui.label("Paints with");
                    let name = if fx.cmd == Cmd::Move { "Clone" } else { fx.cmd.title() };
                    ui.label(RichText::new(name).strong().color(ui.visuals().hyperlink_color));
                    if fx.cmd == Cmd::FillPlain {
                        let [r, g, b] = t.color;
                        let mut c = Color32::from_rgb(r, g, b);
                        if egui::color_picker::color_edit_button_srgba(ui, &mut c, egui::color_picker::Alpha::Opaque).changed() {
                            t.color = [c.r(), c.g(), c.b()];
                            self.brush_effect.params.color = t.color;
                            self.effect_gen += 1;
                        }
                    }
                    if ui.small_button("Color").on_hover_text("Paint with the current color").clicked() {
                        cmd = Some(Cmd::FillPlain);
                    }
                    ui.separator();
                    ui.label("Size");
                    egui::ComboBox::from_id_salt("opt-size").width(60.0).selected_text(format!("{0}×{0}", t.pen_size)).show_ui(
                        ui,
                        |ui| {
                            for s in tools::PEN_SIZES {
                                ui.selectable_value(&mut t.pen_size, s, format!("{s}×{s}"));
                            }
                        },
                    );
                    ui.selectable_value(&mut t.brush, tools::Brush::Circle, "Round");
                    ui.selectable_value(&mut t.brush, tools::Brush::Square, "Square");
                    edge_combo(ui, &mut t.brush_edge, "Softness");
                }
                Tool::Eyedropper => {
                    let [r, g, b] = t.color;
                    let (resp, p) = ui.allocate_painter(vec2(28.0, 16.0), egui::Sense::hover());
                    p.rect_filled(resp.rect, 3.0, Color32::from_rgb(r, g, b));
                    ui.label(format!("{r} {g} {b}"));
                }
                _ => {
                    edge_combo(ui, &mut t.edge, "Edge");
                    if t.tool == Tool::Wand {
                        ui.separator();
                        let mut tol = t.wand_tolerance as i32;
                        ui.add(egui::Slider::new(&mut tol, 1..=100).text("Tolerance"));
                        t.wand_tolerance = tol as u8;
                        ui.selectable_value(&mut t.wand_hsv, false, "RGB");
                        ui.selectable_value(&mut t.wand_hsv, true, "HSV");
                        ui.checkbox(&mut t.wand_unifold, "Contiguous");
                    }
                }
            }
            let (has_sel, info) = match self.docs.get(self.active).and_then(|d| d.selection.as_ref()) {
                Some(s) => (true, format!("Selection {} × {}", s.bounds.w, s.bounds.h)),
                None => (false, "Whole image".to_string()),
            };
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add_enabled(has_sel, egui::Button::new("Invert")).clicked() {
                    cmd = Some(Cmd::InvertSelection);
                }
                if ui.add_enabled(has_sel, egui::Button::new("None")).clicked() {
                    cmd = Some(Cmd::SelectNone);
                }
                if ui.button("All").clicked() {
                    cmd = Some(Cmd::SelectAll);
                }
                ui.label(RichText::new(info).weak());
            });
        });
        if let Some(c) = cmd {
            let ctx = ui.ctx().clone();
            if c == Cmd::FillPlain {
                let p = self.params.clone();
                self.brush_effect = Pending {
                    cmd: c,
                    params: OpParams {
                        color: self.tools.color,
                        ..p
                    },
                };
                self.effect_gen += 1;
            } else {
                self.invoke(c, &ctx);
            }
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if let Some(job) = &self.job {
                ui.spinner();
                ui.label(format!("{}…", job.title));
            } else if self.brush.as_ref().is_some_and(|s| s.preparing.is_some()) {
                ui.spinner();
                ui.label(format!("Preparing {}…", self.brush_effect.cmd.title()));
            } else if let Some(m) = &self.message {
                ui.label(RichText::new(m).color(Color32::from_rgb(220, 90, 60)));
            } else if self.docs.is_empty() {
                ui.label("Open an image to start (Ctrl+O), or drop files on the window");
            } else {
                let what = if self.selection().is_some() { "the selection" } else { "the whole image" };
                let hint = match (&self.state, self.tools.tool) {
                    (State::Place(_), _) => "Drag to move, drag corners to resize (Ctrl keeps proportions); double-click or Apply".to_string(),
                    (State::Rubber { .. }, _) => "Drag a point to where it should go, then Apply".to_string(),
                    (_, Tool::Brush) => "Paint · right-drag restores · any Adjust, Fill or Filters command sets the effect · Image > Move then Shift-click a source clones".to_string(),
                    (_, Tool::Eyedropper) => "Click the image to pick the current color".to_string(),
                    (_, t) => {
                        let how = match t {
                            Tool::Polygon => "Click the corners, double-click to close",
                            Tool::Text => "Click where the text starts",
                            Tool::Wand => "Click a color",
                            _ => "Drag to select",
                        };
                        format!("{how} · Shift adds · Alt subtracts · drag inside to move · commands apply to {what}")
                    }
                };
                ui.label(RichText::new(hint).weak());
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
                let brush = self.tools.tool == Tool::Brush && cmd.paintable();
                let previewing = apply::has_preview(cmd) && !brush;
                let busy = self.preview.computing.is_some();
                let mut open = true;
                egui::Window::new(cmd.title())
                    .id(egui::Id::new("op-dialog"))
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::RIGHT_TOP, vec2(-16.0, 80.0))
                    .show(ctx, |ui| {
                        apply::dialog_ui(ui, cmd, &mut params, &mut env);
                        ui.add_space(8.0);
                        if previewing {
                            ui.horizontal(|ui| {
                                ui.checkbox(&mut self.show_preview, "Preview");
                                if busy && self.show_preview {
                                    ui.spinner();
                                }
                            });
                        }
                        if brush {
                            ui.label(RichText::new("OK sets the brush's effect").weak());
                        }
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
                if !open {
                    result = Some(false);
                }
                let choose_pattern = env.choose_pattern;
                let pattern_missing = matches!(
                    cmd,
                    Cmd::PatternTiled | Cmd::PatternScaled | Cmd::PatternFitted
                ) && params.pattern.is_none();
                match result {
                    Some(true) if pattern_missing => {
                        self.message = Some("Choose a pattern image first.".into());
                        self.dialog = Some(Dialog::Op(cmd, params));
                    }
                    Some(true) => self.accept_dialog(cmd, params),
                    Some(false) => self.preview = Preview::default(),
                    None => self.dialog = Some(Dialog::Op(cmd, params)),
                }
                // The chosen pattern is put into the open dialog when it arrives.
                if choose_pattern {
                    self.files.request(platform::Purpose::Pattern, "Pattern");
                }
            }
            #[cfg(target_arch = "wasm32")]
            Some(Dialog::SaveName(mut name)) => {
                let mut result = None;
                egui::Modal::new(egui::Id::new("save-name")).show(ctx, |ui| {
                    ui.heading("Save As");
                    ui.label("File name (the extension picks the format)");
                    ui.text_edit_singleline(&mut name);
                    ui.horizontal(|ui| {
                        if ui.button("Download").clicked()
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
                match result {
                    Some(true) => self.download(name),
                    Some(false) => {}
                    None => self.dialog = Some(Dialog::SaveName(name)),
                }
            }
            Some(Dialog::Text(at, op, mut params)) => {
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
                        match (mask, self.docs.get(self.active)) {
                            (Some(m), Some(d)) => {
                                // Bottom-left corner at the click.
                                let (ox, oy) =
                                    (at.x.round() as i64, at.y.round() as i64 - m.h as i64);
                                let mut full = Mask::empty(d.img.w, d.img.h);
                                for y in 0..m.h {
                                    for x in 0..m.w {
                                        let (ix, iy) = (ox + x as i64, oy + y as i64);
                                        if m.get(x, y) != 0
                                            && ix >= 0
                                            && iy >= 0
                                            && (ix as usize) < full.w
                                            && (iy as usize) < full.h
                                        {
                                            full.set(ix as usize, iy as usize, 255);
                                        }
                                    }
                                }
                                let s = Selection::combine(
                                    d.selection.as_ref(),
                                    full,
                                    AreaKind::Text,
                                    false,
                                    op,
                                );
                                self.set_selection(s);
                            }
                            _ => self.message = Some("Can't render this text.".into()),
                        }
                        self.params.font = params.font;
                        self.params.font_px = params.font_px;
                        self.params.text = params.text;
                    }
                    Some(false) => {}
                    None => self.dialog = Some(Dialog::Text(at, op, params)),
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
                    ui.checkbox(&mut self.tools.wand_unifold, "Contiguous (\"Unifold\")");
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
        let typing = ctx.egui_wants_keyboard_input();
        if !matches!(self.dialog, None | Some(Dialog::Op(..))) || typing {
            return;
        }
        let cmd = ctx.input_mut(|i| {
            use egui::{Key, KeyboardShortcut as K, Modifiers as M};
            let cs = M::COMMAND | M::SHIFT;
            [
                (K::new(cs, Key::Z), Cmd::Redo),
                (K::new(cs, Key::I), Cmd::InvertSelection),
                (K::new(cs, Key::S), Cmd::SaveAs),
                (K::new(M::COMMAND, Key::O), Cmd::Open),
                (K::new(M::COMMAND, Key::N), Cmd::New),
                (K::new(M::COMMAND, Key::S), Cmd::Save),
                (K::new(M::COMMAND, Key::Z), Cmd::Undo),
                (K::new(M::COMMAND, Key::Y), Cmd::Redo),
                (K::new(M::COMMAND, Key::A), Cmd::SelectAll),
                (K::new(M::COMMAND, Key::D), Cmd::SelectNone),
                (K::new(M::COMMAND, Key::C), Cmd::Copy),
                (K::new(M::COMMAND, Key::V), Cmd::Paste),
                (K::new(M::COMMAND, Key::Num0), Cmd::ZoomFit),
                (K::new(M::COMMAND, Key::Num1), Cmd::Zoom(4)),
                (K::new(M::COMMAND, Key::Equals), Cmd::ZoomIn),
                (K::new(M::COMMAND, Key::Plus), Cmd::ZoomIn),
                (K::new(M::COMMAND, Key::Minus), Cmd::ZoomOut),
            ]
            .into_iter()
            .find(|(k, _)| i.consume_shortcut(k))
            .map(|(_, c)| c)
        });
        if let Some(c) = cmd {
            self.invoke(c, ctx);
        }
        // Single-key tool shortcuts.
        if self.dialog.is_none() {
            let tool = ctx.input(|i| {
                if i.modifiers.any() {
                    return None;
                }
                Tool::ALL.into_iter().find(|t| i.key_pressed(t.key()))
            });
            if let Some(t) = tool {
                self.tools.tool = t;
            }
        }
        // Ctrl + wheel (and pinch) zoom smoothly.
        let zd = ctx.input(|i| i.zoom_delta());
        if zd != 1.0 {
            self.zoom_by(zd);
        }
        let dropped: Vec<egui::DroppedFile> = ctx.input(|i| i.raw.dropped_files.clone());
        for f in dropped {
            match (f.path, f.bytes) {
                (Some(p), _) => self.open_path(p),
                (None, Some(bytes)) => self.handle_picked(platform::Picked {
                    purpose: platform::Purpose::Open,
                    name: f.name,
                    path: None,
                    bytes: bytes.to_vec(),
                }),
                _ => {}
            }
        }
        for p in self.files.poll() {
            self.handle_picked(p);
        }
    }

    fn tabs(&mut self, ui: &mut egui::Ui) {
        let (mut close, mut switch) = (None, None);
        ui.horizontal(|ui| {
            for (i, d) in self.docs.iter().enumerate() {
                let active = i == self.active;
                egui::Frame::new()
                    .fill(if active {
                        ui.visuals().selection.bg_fill.gamma_multiply(0.35)
                    } else {
                        Color32::TRANSPARENT
                    })
                    .corner_radius(4.0)
                    .inner_margin(egui::Margin::symmetric(6, 2))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui.selectable_label(active, d.title()).clicked() && !active {
                                switch = Some(i);
                            }
                            if ui.small_button("×").on_hover_text("Close").clicked() {
                                close = Some(i);
                            }
                        });
                    });
            }
        });
        if let Some(i) = switch {
            self.switch_to(i);
        }
        if let Some(i) = close {
            self.docs.remove(i);
            let a = if i < self.active {
                self.active - 1
            } else {
                self.active
            };
            self.switch_to(a.min(self.docs.len().saturating_sub(1)));
        }
    }
}

fn edge_combo(ui: &mut egui::Ui, e: &mut tools::Edge, label: &str) {
    ui.label(label);
    egui::ComboBox::from_id_salt(label)
        .width(80.0)
        .selected_text(e.name())
        .show_ui(ui, |ui| {
            for v in [
                tools::Edge::Sharp,
                tools::Edge::Low,
                tools::Edge::Medium,
                tools::Edge::High,
            ] {
                ui.selectable_value(e, v, v.name());
            }
        });
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

fn menu_item(
    ui: &mut egui::Ui,
    m: &MenuItem,
    enabled: &dyn Fn(Cmd) -> bool,
    chosen: &mut Option<Cmd>,
) {
    match m {
        MenuItem::Cmd(l, c, k) => item(ui, l, *k, *c, enabled(*c), chosen),
        MenuItem::Sub(l, items) => {
            ui.menu_button(*l, |ui| {
                for i in items {
                    menu_item(ui, i, enabled, chosen);
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
        self.popup_was_open = ctx.any_popup_open();
        self.poll_job(&ctx);
        self.ensure_brush();
        self.poll_brush(&ctx);
        self.shortcuts(&ctx);
        self.update_preview(&ctx);

        let title = match self.docs.get(self.active) {
            Some(d) => format!("Picture Man — {}", d.title()),
            None => "Picture Man".into(),
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));

        egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui));
        if !self.docs.is_empty() {
            egui::Panel::top("options").show(ui, |ui| {
                ui.add_space(3.0);
                self.options_bar(ui);
                ui.add_space(3.0);
            });
        }
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        if self.show_toolbox {
            egui::Panel::left("toolbox")
                .resizable(false)
                .show(ui, |ui| {
                    ui.add_space(4.0);
                    let has_sel = self.selection().is_some();
                    if let ToolboxAction::Deselect =
                        tools::toolbox(ui, &self.assets, &mut self.tools, has_sel)
                    {
                        self.set_selection(None);
                    }
                });
        }
        egui::CentralPanel::default().show(ui, |ui| {
            if self.docs.is_empty() {
                ui.centered_and_justified(|ui| {
                    ui.label(
                        RichText::new("Open an image (Ctrl+O) or drop files here")
                            .weak()
                            .size(16.0),
                    );
                });
                return;
            }
            self.tabs(ui);
            ui.separator();
            let mode = match &self.state {
                State::Idle => Mode::Tool(self.tools.tool),
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
            let previewing = matches!(self.dialog, Some(Dialog::Op(..))) && self.show_preview;
            let display = if previewing {
                self.preview.tex.as_ref().map(|t| t.id())
            } else {
                None
            };
            let tools = &self.tools;
            let it = &mut self.interaction;
            let events = match self.docs.get_mut(self.active) {
                Some(d) => {
                    let selection = d.selection.take();
                    let ev = canvas::canvas(
                        ui,
                        d,
                        it,
                        mode,
                        tools,
                        View {
                            selection: selection.as_ref(),
                            display,
                        },
                    );
                    d.selection = selection;
                    ev
                }
                None => Vec::new(),
            };
            self.handle_canvas(events);
        });

        self.dialogs(&ctx);
        if self.job.is_none() {
            self.save_settings_if_changed();
        }
    }
}
