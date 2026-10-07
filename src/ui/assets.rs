//! The original Picture Man artwork (toolbox buttons, logo, icons, cursors),
//! extracted from PMAN.EXE's resources and embedded in the binary.

use std::collections::HashMap;

use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};

macro_rules! embed {
    ($($name:literal),* $(,)?) => {
        &[$(($name, include_bytes!(concat!("../../assets/", $name, ".png")) as &[u8])),*]
    };
}

const FILES: &[(&str, &[u8])] = embed!(
    "bmp_ACTION",
    "bmp_AREA",
    "bmp_EDGE",
    "bmp_PENSIZE",
    "bmp_COLOR",
    "bmp_LOGO",
    "bmp_WHOLEUP",
    "bmp_WHOLEDOWN",
    "bmp_RECTUP",
    "bmp_RECTDOWN",
    "bmp_ELLIPSEUP",
    "bmp_ELLIPSEDOWN",
    "bmp_POLYUP",
    "bmp_POLYDOWN",
    "bmp_TEXTUP",
    "bmp_TEXTDOWN",
    "bmp_WANGUP",
    "bmp_WANGDOWN",
    "bmp_BRUSHUP",
    "bmp_BRUSHDOWN",
    "bmp_PENUP",
    "bmp_PENDOWN",
    "bmp_SHARPUP",
    "bmp_SHARPDOWN",
    "bmp_LOWUP",
    "bmp_LOWDOWN",
    "bmp_MEDUP",
    "bmp_MEDDOWN",
    "bmp_HIGHUP",
    "bmp_HIGHDOWN",
    "bmp_SQPEN0",
    "bmp_SQPEN1",
    "bmp_CIRPEN0",
    "bmp_CIRPEN1",
    "bmp_PICKUP",
    "bmp_PICKDOWN",
    "icon_34",
    "icon_35",
    "icon_36",
);

pub struct Assets {
    tex: HashMap<&'static str, TextureHandle>,
}

impl Assets {
    pub fn load(ctx: &egui::Context) -> Self {
        let tex = FILES
            .iter()
            .map(|(name, bytes)| {
                let img = image::load_from_memory(bytes)
                    .expect("embedded png")
                    .to_rgba8();
                let size = [img.width() as usize, img.height() as usize];
                let ci = ColorImage::from_rgba_unmultiplied(size, img.as_raw());
                (*name, ctx.load_texture(*name, ci, TextureOptions::NEAREST))
            })
            .collect();
        Assets { tex }
    }

    pub fn get(&self, name: &str) -> &TextureHandle {
        &self.tex[name]
    }

    /// The application icon (the color-wheel monitor), for the window.
    pub fn window_icon() -> egui::IconData {
        let bytes = FILES.iter().find(|(n, _)| *n == "icon_35").unwrap().1;
        let img = image::load_from_memory(bytes).unwrap().to_rgba8();
        egui::IconData {
            width: img.width(),
            height: img.height(),
            rgba: img.into_raw(),
        }
    }
}
