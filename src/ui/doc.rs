//! An open image: pixels, undo history, view state and the GPU texture.

use std::path::PathBuf;

use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};

use crate::core::Image;

const UNDO_DEPTH: usize = 16;

pub struct Doc {
    pub id: u64,
    pub name: String,
    pub path: Option<PathBuf>,
    pub img: Image,
    undo: Vec<Image>,
    /// The image as last loaded or saved, for File/Reload.
    pub saved: Option<Image>,
    pub modified: bool,
    pub zoom: f32,
    tex: Option<TextureHandle>,
    stale: bool,
}

impl Doc {
    pub fn new(id: u64, name: String, path: Option<PathBuf>, img: Image) -> Self {
        Doc {
            id,
            name,
            saved: path.as_ref().map(|_| img.clone()),
            path,
            img,
            undo: Vec::new(),
            modified: false,
            zoom: 1.0,
            tex: None,
            stale: true,
        }
    }

    /// Record the current state before a change ("Create backup").
    pub fn push_undo(&mut self) {
        if self.undo.len() == UNDO_DEPTH {
            self.undo.remove(0);
        }
        self.undo.push(self.img.clone());
    }

    /// The most recent backup (what Undo would restore; used by Erase).
    pub fn last_backup(&self) -> Option<&Image> {
        self.undo.last()
    }

    pub fn undo(&mut self) {
        if let Some(prev) = self.undo.pop() {
            self.img = prev;
            self.touch();
        }
    }

    /// Replace the pixels (after an operation) and mark the view stale.
    pub fn set_image(&mut self, img: Image) {
        self.img = img;
        self.touch();
    }

    pub fn touch(&mut self) {
        self.modified = true;
        self.stale = true;
    }

    pub fn mark_stale(&mut self) {
        self.stale = true;
    }

    pub fn texture(&mut self, ctx: &egui::Context) -> &TextureHandle {
        let opts = if self.zoom >= 1.0 {
            TextureOptions::NEAREST
        } else {
            TextureOptions::LINEAR
        };
        if self.stale || self.tex.is_none() {
            let raw: Vec<u8> = self.img.px.iter().flatten().copied().collect();
            let ci = ColorImage::from_rgb([self.img.w, self.img.h], &raw);
            match &mut self.tex {
                Some(t) => t.set(ci, opts),
                None => self.tex = Some(ctx.load_texture(format!("doc{}", self.id), ci, opts)),
            }
            self.stale = false;
        }
        self.tex.as_ref().unwrap()
    }

    pub fn title(&self) -> String {
        format!("{}{}", self.name, if self.modified { " *" } else { "" })
    }
}
