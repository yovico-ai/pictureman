//! Core data types shared by every operation: a 24-bit image, a coverage
//! mask for selections (smooth edges are mask values between 0 and 255),
//! and the Microsoft C runtime `rand()` the original relied on.

pub type Rgb = [u8; 3];

/// Largest image (in pixels) the program creates, loads or resizes to:
/// 100 megapixels, 300 MB of pixel data. Everything that sizes a buffer
/// from user input or file contents checks against it first.
pub const MAX_PIXELS: usize = 100_000_000;

/// `w × h` if it is a usable image size.
pub fn checked_area(w: usize, h: usize) -> Option<usize> {
    w.checked_mul(h).filter(|&n| n > 0 && n <= MAX_PIXELS)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub w: usize,
    pub h: usize,
    pub px: Vec<Rgb>,
}

impl Image {
    pub fn new(w: usize, h: usize, fill: Rgb) -> Self {
        Image {
            w,
            h,
            px: vec![fill; w * h],
        }
    }

    #[inline]
    pub fn get(&self, x: usize, y: usize) -> Rgb {
        self.px[y * self.w + x]
    }

    #[inline]
    pub fn set(&mut self, x: usize, y: usize, c: Rgb) {
        self.px[y * self.w + x] = c;
    }

    /// Pixel access with coordinates clamped to the image (edge replication).
    #[inline]
    pub fn get_clamped(&self, x: isize, y: isize) -> Rgb {
        let x = x.clamp(0, self.w as isize - 1) as usize;
        let y = y.clamp(0, self.h as isize - 1) as usize;
        self.px[y * self.w + x]
    }

    pub fn row(&self, y: usize) -> &[Rgb] {
        &self.px[y * self.w..(y + 1) * self.w]
    }

    pub fn row_mut(&mut self, y: usize) -> &mut [Rgb] {
        &mut self.px[y * self.w..(y + 1) * self.w]
    }

    pub fn rect(&self) -> Rect {
        Rect {
            x: 0,
            y: 0,
            w: self.w,
            h: self.h,
        }
    }

    pub fn from_rgb_image(img: &image::RgbImage) -> Self {
        let (w, h) = img.dimensions();
        let px = img.pixels().map(|p| p.0).collect();
        Image {
            w: w as usize,
            h: h as usize,
            px,
        }
    }

    pub fn to_rgb_image(&self) -> image::RgbImage {
        let raw: Vec<u8> = self.px.iter().flatten().copied().collect();
        image::RgbImage::from_raw(self.w as u32, self.h as u32, raw).expect("size matches")
    }

    /// Copy out a sub-rectangle.
    pub fn crop(&self, r: Rect) -> Image {
        let mut out = Image::new(r.w, r.h, [0; 3]);
        for y in 0..r.h {
            out.row_mut(y)
                .copy_from_slice(&self.row(r.y + y)[r.x..r.x + r.w]);
        }
        out
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

impl Rect {
    pub fn contains(&self, x: usize, y: usize) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }
}

/// Per-pixel selection coverage: 0 = untouched, 255 = fully processed.
#[derive(Clone, Debug)]
pub struct Mask {
    pub w: usize,
    pub h: usize,
    pub data: Vec<u8>,
}

impl Mask {
    pub fn empty(w: usize, h: usize) -> Self {
        Mask {
            w,
            h,
            data: vec![0; w * h],
        }
    }
    pub fn full(w: usize, h: usize) -> Self {
        Mask {
            w,
            h,
            data: vec![255; w * h],
        }
    }
    #[inline]
    pub fn get(&self, x: usize, y: usize) -> u8 {
        self.data[y * self.w + x]
    }
    #[inline]
    pub fn set(&mut self, x: usize, y: usize, v: u8) {
        self.data[y * self.w + x] = v;
    }
    pub fn invert(&mut self) {
        self.data.iter_mut().for_each(|v| *v = 255 - *v);
    }
    /// Smallest rectangle containing every non-zero pixel.
    pub fn bounds(&self) -> Rect {
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
        for y in 0..self.h {
            for x in 0..self.w {
                if self.get(x, y) != 0 {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x + 1);
                    y1 = y1.max(y + 1);
                }
            }
        }
        if x0 == usize::MAX {
            Rect::default()
        } else {
            Rect {
                x: x0,
                y: y0,
                w: x1 - x0,
                h: y1 - y0,
            }
        }
    }
}

/// `dst = lerp(dst, processed, mask)` — how a processed image is committed
/// through a (possibly soft-edged) selection.
pub fn blend_through_mask(dst: &mut Image, processed: &Image, mask: &Mask) {
    for i in 0..dst.px.len() {
        let m = mask.data[i] as u32;
        if m == 0 {
            continue;
        }
        if m == 255 {
            dst.px[i] = processed.px[i];
            continue;
        }
        let (a, b) = (dst.px[i], processed.px[i]);
        for c in 0..3 {
            dst.px[i][c] = ((a[c] as u32 * (255 - m) + b[c] as u32 * m + 127) / 255) as u8;
        }
    }
}

/// Microsoft C runtime `rand()`/`srand()`: the same LCG PMAN.EXE linked, so
/// random-driven effects (scatter, fluctuated fill, ...) can match the original.
#[derive(Clone, Debug)]
pub struct MsRand {
    state: u32,
}

impl Default for MsRand {
    fn default() -> Self {
        MsRand { state: 1 }
    }
}

impl MsRand {
    pub fn new(seed: u32) -> Self {
        MsRand { state: seed }
    }
    pub fn srand(&mut self, seed: u32) {
        self.state = seed;
    }
    /// Returns 0..=0x7fff.
    pub fn rand(&mut self) -> i32 {
        self.state = self.state.wrapping_mul(214013).wrapping_add(2531011);
        ((self.state >> 16) & 0x7fff) as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ms_rand_matches_crt() {
        // First values of MSVC/MS C rand() after srand(1).
        let mut r = MsRand::default();
        assert_eq!([r.rand(), r.rand(), r.rand()], [41, 18467, 6334]);
    }

    #[test]
    fn soft_mask_blends() {
        let mut a = Image::new(1, 1, [0, 0, 0]);
        let b = Image::new(1, 1, [255, 255, 255]);
        let m = Mask {
            w: 1,
            h: 1,
            data: vec![128],
        };
        blend_through_mask(&mut a, &b, &m);
        assert_eq!(a.px[0], [128, 128, 128]);
    }
}
