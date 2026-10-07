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
            let d = rfd::AsyncFileDialog::new()
                .set_title(title)
                .add_filter("Images", IMAGE_EXTS);
            let tx = self.tx.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let handles = if purpose == Purpose::Open {
                    d.pick_files().await.unwrap_or_default()
                } else {
                    d.pick_file().await.into_iter().collect()
                };
                for h in handles {
                    let bytes = h.read().await;
                    let _ = tx.send(Picked {
                        purpose,
                        name: h.file_name(),
                        path: None,
                        bytes,
                    });
                }
            });
        }
    }

    pub fn poll(&self) -> Vec<Picked> {
        self.rx.try_iter().collect()
    }
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
