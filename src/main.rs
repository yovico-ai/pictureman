#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use pictureman::ui::{App, AppAssets};

fn main() -> eframe::Result {
    // Like the original, an image file can be passed on the command line.
    let files = std::env::args_os().skip(1).map(Into::into).collect();
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Picture Man")
            .with_inner_size([1100.0, 760.0])
            .with_icon(AppAssets::window_icon()),
        ..Default::default()
    };
    eframe::run_native(
        "Picture Man",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc, files)))),
    )
}
