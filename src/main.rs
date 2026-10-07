#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use pictureman::ui::{App, AppAssets};

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result {
    // Like the original, an image file can be passed on the command line.
    let files = std::env::args_os().skip(1).map(Into::into).collect();
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Picture Man")
            .with_inner_size([1100.0, 760.0])
            .with_icon(AppAssets::window_icon()),
        // Show pixels exactly as stored (egui dithers by default).
        dithering: false,
        ..Default::default()
    };
    eframe::run_native(
        "Picture Man",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc, files)))),
    )
}

/// In the browser: run in the page's canvas.
#[cfg(target_arch = "wasm32")]
fn main() {
    use eframe::wasm_bindgen::JsCast;
    let _ = AppAssets::window_icon;
    wasm_bindgen_futures::spawn_local(async {
        let document = web_sys::window()
            .and_then(|w| w.document())
            .expect("no document");
        let canvas = document
            .get_element_by_id("picture_man")
            .expect("no canvas")
            .dyn_into::<web_sys::HtmlCanvasElement>()
            .expect("not a canvas");
        let start = eframe::WebRunner::new()
            .start(
                canvas,
                eframe::WebOptions {
                    dithering: false,
                    ..Default::default()
                },
                Box::new(|cc| Ok(Box::new(App::new(cc, Vec::new())))),
            )
            .await;
        if let Err(e) = start {
            if let Some(el) = document.get_element_by_id("loading") {
                el.set_text_content(Some(&format!("Picture Man could not start: {e:?}")));
            }
        } else if let Some(el) = document.get_element_by_id("loading") {
            el.remove();
        }
    });
}
