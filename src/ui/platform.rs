//! What differs between the desktop and the browser: picking files (a
//! native dialog vs. the browser's file input), saving (a file vs. a
//! download), and running operations (a worker thread vs. inline).

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};

/// Why a file is being picked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    Open,
    Pattern,
    PasteFrom,
}

/// A file the user picked: its bytes, plus its path on the desktop.
pub struct Picked {
    pub purpose: Purpose,
    pub name: String,
    pub path: Option<PathBuf>,
    pub bytes: Vec<u8>,
}

pub struct Files {
    tx: Sender<Picked>,
    rx: Receiver<Picked>,
}

impl Default for Files {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Files { tx, rx }
    }
}

const IMAGE_EXTS: &[&str] = &[
    "bmp", "dib", "gif", "tif", "tiff", "jpg", "jpeg", "tga", "pcx", "png",
];

impl Files {
    /// Ask for a file (several for Open). The result arrives through
    /// [`Files::poll`]: on the desktop right away (the native dialog
    /// blocks), in the browser once the user has chosen.
    pub fn request(&self, purpose: Purpose, title: &str) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let d = rfd::FileDialog::new()
                .set_title(title)
                .add_filter("Images", IMAGE_EXTS);
            let paths = if purpose == Purpose::Open {
                d.pick_files().unwrap_or_default()
            } else {
                d.pick_file().into_iter().collect()
            };
            for path in paths {
                let name = path
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                match std::fs::read(&path) {
                    Ok(bytes) => {
                        let _ = self.tx.send(Picked {
                            purpose,
                            name,
                            path: Some(path),
                            bytes,
                        });
                    }
                    Err(e) => {
                        let _ = self.tx.send(Picked {
                            purpose,
                            name: format!("{name}: {e}"),
                            path: Some(path),
                            bytes: Vec::new(),
                        });
                    }
                }
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = title;
            if let Err(e) = pick_in_browser(purpose, self.tx.clone()) {
                let _ = self.tx.send(Picked {
                    purpose,
                    name: format!("(file picker: {e})"),
                    path: None,
                    bytes: Vec::new(),
                });
            }
        }
    }

    pub fn poll(&self) -> Vec<Picked> {
        self.rx.try_iter().collect()
    }
}

/// The browser's own file picker: a hidden `<input type=file>`. (rfd's web
/// backend panicked when its element was already gone.)
#[cfg(target_arch = "wasm32")]
fn pick_in_browser(purpose: Purpose, tx: Sender<Picked>) -> Result<(), String> {
    use eframe::wasm_bindgen::{JsCast, closure::Closure};
    let document = web_sys::window()
        .and_then(|w| w.document())
        .ok_or("no document")?;
    let input: web_sys::HtmlInputElement = document
        .create_element("input")
        .map_err(|e| format!("{e:?}"))?
        .dyn_into()
        .map_err(|_| "not an input")?;
    input.set_type("file");
    input.set_accept(
        &IMAGE_EXTS
            .iter()
            .map(|e| format!(".{e}"))
            .collect::<Vec<_>>()
            .join(","),
    );
    input.set_multiple(purpose == Purpose::Open);
    let el = input.clone();
    let on_change = Closure::<dyn FnMut()>::new(move || {
        let Some(files) = el.files() else { return };
        for i in 0..files.length() {
            let Some(file) = files.get(i) else { continue };
            let tx = tx.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let name = file.name();
                let bytes = match wasm_bindgen_futures::JsFuture::from(file.array_buffer()).await {
                    Ok(buf) => js_sys::Uint8Array::new(&buf).to_vec(),
                    Err(_) => Vec::new(),
                };
                let _ = tx.send(Picked {
                    purpose,
                    name,
                    path: None,
                    bytes,
                });
            });
        }
    });
    input.set_onchange(Some(on_change.as_ref().unchecked_ref()));
    // The input lives as long as the closure; both are tiny.
    on_change.forget();
    input.click();
    Ok(())
}

/// Offer `bytes` as a download named `name` (browser only).
#[cfg(target_arch = "wasm32")]
pub fn download(name: &str, bytes: &[u8]) -> Result<(), String> {
    use eframe::wasm_bindgen::JsCast;
    let err = |e: eframe::wasm_bindgen::JsValue| format!("{e:?}");
    let window = web_sys::window().ok_or("no window")?;
    let document = window.document().ok_or("no document")?;
    let array = js_sys::Uint8Array::from(bytes);
    let parts = js_sys::Array::of1(&array);
    let blob = web_sys::Blob::new_with_u8_array_sequence(&parts).map_err(err)?;
    let url = web_sys::Url::create_object_url_with_blob(&blob).map_err(err)?;
    let a: web_sys::HtmlAnchorElement = document
        .create_element("a")
        .map_err(err)?
        .dyn_into()
        .map_err(|_| "anchor")?;
    a.set_href(&url);
    a.set_download(name);
    a.click();
    web_sys::Url::revoke_object_url(&url).map_err(err)?;
    Ok(())
}

/// Run `f` off the UI thread where threads exist; in the browser it runs
/// inline (the result is delivered the same way).
pub fn spawn<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Receiver<T> {
    let (tx, rx) = mpsc::channel();
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    #[cfg(target_arch = "wasm32")]
    let _ = tx.send(f());
    rx
}
