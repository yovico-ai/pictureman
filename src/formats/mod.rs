//! File format readers and writers. The original shipped one converter DLL
//! per format (READ*.DLL / WRITE*.DLL, DJPG/CJPG, DTARGA/CTARGA); BMP, GIF,
//! TIFF, JPEG and TARGA now come from the `image` crate, while PCX, EPS
//! (the original "EPI" writer) and raw byte arrays (READBIN/WRITEBIN) are
//! implemented here. (The original's READBIN/WRITEBIN raw byte arrays are
//! not carried over.)

mod eps;
mod pcx;

use std::path::Path;

use crate::core::{Image, checked_area};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Bmp,
    Gif,
    Tiff,
    Jpeg,
    Targa,
    Pcx,
    Png,
    Eps,
}

impl Format {
    pub const ALL: [Format; 8] = [
        Format::Tiff,
        Format::Pcx,
        Format::Bmp,
        Format::Gif,
        Format::Jpeg,
        Format::Targa,
        Format::Png,
        Format::Eps,
    ];

    /// Description and extensions, in the style of the original converters'
    /// GETDESCRIPTION / GETEXTENSION exports.
    pub fn description(self) -> &'static str {
        match self {
            Format::Bmp => "Windows Bitmap",
            Format::Gif => "CompuServe GIF",
            Format::Tiff => "TIFF",
            Format::Jpeg => "JPEG",
            Format::Targa => "TARGA",
            Format::Pcx => "ZSoft PaintBrush PCX",
            Format::Png => "PNG",
            Format::Eps => "PostScript EPI",
        }
    }

    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            Format::Bmp => &["bmp", "dib"],
            Format::Gif => &["gif"],
            Format::Tiff => &["tif", "tiff"],
            Format::Jpeg => &["jpg", "jpeg"],
            Format::Targa => &["tga"],
            Format::Pcx => &["pcx"],
            Format::Png => &["png"],
            Format::Eps => &["eps", "epi"],
        }
    }

    pub fn can_read(self) -> bool {
        self != Format::Eps
    }

    pub fn from_path(path: &Path) -> Option<Format> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Format::ALL
            .into_iter()
            .find(|f| f.extensions().contains(&ext.as_str()))
    }

    fn image_format(self) -> Option<image::ImageFormat> {
        Some(match self {
            Format::Bmp => image::ImageFormat::Bmp,
            Format::Gif => image::ImageFormat::Gif,
            Format::Tiff => image::ImageFormat::Tiff,
            Format::Jpeg => image::ImageFormat::Jpeg,
            Format::Targa => image::ImageFormat::Tga,
            Format::Png => image::ImageFormat::Png,
            _ => return None,
        })
    }
}

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Image(image::ImageError),
    UnknownFormat,
    Unsupported(String),
    Corrupt(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Image(e) => write!(f, "{e}"),
            Error::UnknownFormat => write!(f, "Unknown file format"),
            Error::Unsupported(s) => write!(f, "Unsupported sub format: {s}"),
            Error::Corrupt(s) => write!(f, "Error in image file structure: {s}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<image::ImageError> for Error {
    fn from(e: image::ImageError) -> Self {
        Error::Image(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Load any supported image as 24-bit RGB.
pub fn load(path: &Path) -> Result<Image> {
    load_bytes(&std::fs::read(path)?, Format::from_path(path))
}

/// Decode an image from memory. The content is sniffed first, like the
/// converters' MAGIC export; `hint` (usually from the file name) is the
/// fallback.
pub fn load_bytes(bytes: &[u8], hint: Option<Format>) -> Result<Image> {
    if pcx::is_pcx(bytes) {
        return pcx::decode(bytes);
    }
    let fmt = image::guess_format(bytes)
        .ok()
        .or_else(|| hint.and_then(Format::image_format));
    let Some(fmt) = fmt else {
        return Err(Error::UnknownFormat);
    };
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), fmt);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(65_535);
    limits.max_image_height = Some(65_535);
    reader.limits(limits);
    let (w, h) = reader.into_dimensions()?;
    if checked_area(w as usize, h as usize).is_none() {
        return Err(Error::Unsupported(format!("{w}x{h} image is too large")));
    }
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), fmt);
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(1 << 30);
    reader.limits(limits);
    Ok(Image::from_rgb_image(&reader.decode()?.to_rgb8()))
}

/// Save in the format chosen by `format` (or the file extension).
pub fn save(path: &Path, img: &Image, format: Option<Format>) -> Result<()> {
    let format = format
        .or_else(|| Format::from_path(path))
        .ok_or(Error::UnknownFormat)?;
    std::fs::write(path, encode(img, format)?)?;
    Ok(())
}

/// Encode an image in `format`.
pub fn encode(img: &Image, format: Format) -> Result<Vec<u8>> {
    if img.w == 0 || img.h == 0 || img.px.len() != img.w * img.h {
        return Err(Error::Unsupported("empty image".into()));
    }
    if format == Format::Pcx && (img.w > 65_536 || img.h > 65_536) {
        return Err(Error::Unsupported(format!(
            "{}x{} is too large for PCX",
            img.w, img.h
        )));
    }
    Ok(match format {
        Format::Pcx => pcx::encode(img),
        Format::Eps => eps::encode(img, 72),
        Format::Jpeg => {
            let mut out = Vec::new();
            let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85);
            img.to_rgb_image().write_with_encoder(enc)?;
            out
        }
        other => {
            let mut out = std::io::Cursor::new(Vec::new());
            img.to_rgb_image()
                .write_to(&mut out, other.image_format().expect("handled above"))?;
            out.into_inner()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Image {
        let mut img = Image::new(37, 11, [0, 0, 0]);
        for y in 0..img.h {
            for x in 0..img.w {
                img.set(x, y, [(x * 7) as u8, (y * 23) as u8, ((x ^ y) * 5) as u8]);
            }
        }
        img
    }

    #[test]
    fn lossless_round_trips() {
        let dir = std::env::temp_dir().join(format!("pman-fmt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = sample();
        for ext in ["bmp", "tif", "tga", "pcx", "png"] {
            let p = dir.join(format!("t.{ext}"));
            save(&p, &img, None).unwrap();
            assert_eq!(load(&p).unwrap(), img, "{ext}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
