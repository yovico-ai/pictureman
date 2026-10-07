//! Robustness: nothing a user or a file can supply may crash the program.
//! The original had fixed, small buffers (64-byte paths, unchecked PCX
//! bytes-per-line, …); these tests feed hostile sizes, regions, parameters,
//! files and paths to every module and collect any panic.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;

use pictureman::core::{Image, Mask, MsRand, Rect};
use pictureman::formats::{self, Format};
use pictureman::ops::fill::{Fluctuation, GradientKind, PatchMode, RadialShape};
use pictureman::ops::filters::FilterSize;
use pictureman::ops::transform::{self, MirrorKind};
use pictureman::ops::tune::{self, AreaKind, ColorMap};
use pictureman::ops::{effects, fill, filters};
use pictureman::selection as sel;

const SIZES: [(usize, usize); 8] = [
    (1, 1),
    (1, 7),
    (7, 1),
    (2, 2),
    (3, 5),
    (16, 9),
    (33, 17),
    (5, 40),
];

fn test_image(w: usize, h: usize) -> Image {
    let mut img = Image::new(w, h, [0; 3]);
    let mut r = MsRand::new(7);
    for p in img.px.iter_mut() {
        *p = [r.rand() as u8, r.rand() as u8, r.rand() as u8];
    }
    img
}

fn rois(w: usize, h: usize) -> Vec<Rect> {
    let mut v = vec![
        Rect { x: 0, y: 0, w, h },
        Rect {
            x: 0,
            y: 0,
            w: 1,
            h: 1,
        },
        Rect {
            x: w - 1,
            y: h - 1,
            w: 1,
            h: 1,
        },
        Rect {
            x: w - 1,
            y: 0,
            w: 1,
            h,
        },
        Rect {
            x: 0,
            y: h - 1,
            w,
            h: 1,
        },
    ];
    if w > 2 && h > 2 {
        v.push(Rect {
            x: 1,
            y: 1,
            w: w - 2,
            h: h - 2,
        });
    }
    v
}

fn random_mask(w: usize, h: usize, seed: u32) -> Mask {
    let mut r = MsRand::new(seed);
    let mut m = Mask::empty(w, h);
    for v in m.data.iter_mut() {
        *v = if r.rand() % 3 == 0 { 0 } else { 255 };
    }
    m
}

struct Failures(Vec<String>);

impl Failures {
    fn check(&mut self, what: impl Fn() -> String, f: impl FnOnce()) {
        if catch_unwind(AssertUnwindSafe(f)).is_err() {
            self.0.push(what());
        }
    }
    fn assert_none(self) {
        assert!(
            self.0.is_empty(),
            "{} panics:\n{}",
            self.0.len(),
            self.0.join("\n")
        );
    }
}

fn quiet() {
    std::panic::set_hook(Box::new(|_| {}));
}

#[test]
fn filters_and_effects_never_panic() {
    quiet();
    let mut f = Failures(Vec::new());
    for (w, h) in SIZES {
        let img = test_image(w, h);
        let (tw, th) = effects::filter_size_max_tiles(w, h);
        for roi in rois(w, h) {
            for s in [
                (2, 2),
                (3, 3),
                (15, 15),
                (2, 15),
                (tw.max(2), th.max(2)),
                (40, 40),
            ] {
                let fs = FilterSize::new(s.0, s.1);
                let ctx = || format!("{w}x{h} roi {roi:?} size {s:?}");
                f.check(
                    || format!("smoothing {}", ctx()),
                    || drop(filters::smoothing(&img, roi, fs)),
                );
                f.check(
                    || format!("spot_removing {}", ctx()),
                    || drop(filters::spot_removing(&img, roi, fs)),
                );
                f.check(
                    || format!("minimum {}", ctx()),
                    || drop(filters::minimum(&img, roi, fs)),
                );
                f.check(
                    || format!("maximum {}", ctx()),
                    || drop(filters::maximum(&img, roi, fs)),
                );
                f.check(
                    || format!("hidden_127 {}", ctx()),
                    || drop(filters::hidden_127(&img, roi, fs)),
                );
                f.check(
                    || format!("hand_drawing {}", ctx()),
                    || drop(effects::hand_drawing(&img, roi, s.0, s.1)),
                );
                f.check(
                    || format!("cleaning {}", ctx()),
                    || drop(effects::cleaning_background(&img, roi, s.0, s.1)),
                );
                f.check(
                    || format!("mosaic {}", ctx()),
                    || drop(effects::mosaic(&img, roi, s.0, s.1)),
                );
                f.check(
                    || format!("faceted {}", ctx()),
                    || drop(effects::faceted_glass(&img, roi, s.0, s.1)),
                );
                f.check(
                    || format!("scatter {}", ctx()),
                    || {
                        drop(effects::scatter(
                            &img,
                            roi,
                            s.0,
                            s.1,
                            &mut MsRand::default(),
                        ))
                    },
                );
            }
            let ctx = || format!("{w}x{h} roi {roi:?}");
            f.check(
                || format!("smoothing_3x3 {}", ctx()),
                || drop(filters::smoothing_3x3(&img, roi)),
            );
            f.check(
                || format!("sharpening {}", ctx()),
                || drop(filters::sharpening(&img, roi)),
            );
            f.check(
                || format!("heavy_sharpening {}", ctx()),
                || drop(filters::heavy_sharpening(&img, roi)),
            );
            f.check(
                || format!("contour {}", ctx()),
                || drop(filters::contour_outlining(&img, roi)),
            );
            f.check(
                || format!("emboss {}", ctx()),
                || drop(filters::emboss(&img, roi, [0, 166, 166])),
            );
        }
    }
    f.assert_none();
}

#[test]
fn tune_and_fill_never_panic() {
    quiet();
    let mut f = Failures(Vec::new());
    let patterns = [test_image(1, 1), test_image(3, 2), test_image(50, 70)];
    for (w, h) in SIZES {
        let img = test_image(w, h);
        let mask = random_mask(w, h, 3);
        for roi in rois(w, h) {
            let ctx = || format!("{w}x{h} roi {roi:?}");
            for map in [
                ColorMap::default(),
                ColorMap::from_tv(0, 255, -255),
                ColorMap::from_tv(100, -255, 255),
            ] {
                f.check(
                    || format!("rgb_control {}", ctx()),
                    || drop(tune::rgb_control(&img, roi, &map)),
                );
            }
            for pos in [0, 127, 255] {
                f.check(
                    || format!("gamma {pos} {}", ctx()),
                    || {
                        drop(tune::gamma(
                            &img,
                            roi,
                            tune::gamma_from_slider(pos),
                            true,
                            false,
                            true,
                        ))
                    },
                );
            }
            for area in [
                AreaKind::Whole,
                AreaKind::Pen,
                AreaKind::Mask(&mask),
                AreaKind::Mask(&Mask::empty(w, h)),
            ] {
                f.check(
                    || format!("expand {}", ctx()),
                    || drop(tune::expand(&img, roi, area)),
                );
                f.check(
                    || format!("equalize {}", ctx()),
                    || drop(tune::equalize(&img, roi, area)),
                );
            }
            f.check(
                || format!("plain {}", ctx()),
                || drop(fill::fill_plain(&img, roi, [1, 2, 3])),
            );
            for (grain, depth) in [(1, 1), (3, 50), (16, 100)] {
                f.check(
                    || format!("fluctuated {grain} {depth} {}", ctx()),
                    || {
                        drop(fill::fill_fluctuated(
                            &img,
                            roi,
                            [200, 10, 30],
                            Fluctuation { grain, depth },
                            &mut MsRand::default(),
                        ))
                    },
                );
            }
            for kind in [
                GradientKind::Vertical,
                GradientKind::Horizontal,
                GradientKind::Radial(RadialShape::Diagonal),
                GradientKind::Radial(RadialShape::Ellipse),
                GradientKind::Radial(RadialShape::Circle),
            ] {
                f.check(
                    || format!("gradient {kind:?} {}", ctx()),
                    || drop(fill::gradient(&img, roi, kind, [0; 3], [255; 3])),
                );
            }
            for pat in &patterns {
                f.check(
                    || format!("tiled {}", ctx()),
                    || drop(fill::pattern_tiled(&img, roi, pat)),
                );
                f.check(
                    || format!("scaled {}", ctx()),
                    || drop(fill::pattern_scaled(&img, roi, pat)),
                );
                f.check(
                    || format!("fitted {}", ctx()),
                    || drop(fill::pattern_fitted(&img, roi, pat)),
                );
            }
            for mode in [PatchMode::Full, PatchMode::Horizontal, PatchMode::Vertical] {
                for m in [&mask, &Mask::full(w, h), &Mask::empty(w, h)] {
                    f.check(
                        || format!("patch {mode:?} {}", ctx()),
                        || drop(fill::patch(&img, roi, m, mode)),
                    );
                }
            }
        }
    }
    f.assert_none();
}

#[test]
fn transforms_never_panic() {
    quiet();
    let mut f = Failures(Vec::new());
    for (w, h) in SIZES {
        let img = test_image(w, h);
        let sel = random_mask(w, h, 5);
        for (nw, nh) in [(1, 1), (w * 3, h), (w, 1), (97, 61)] {
            f.check(
                || format!("resize {w}x{h} -> {nw}x{nh}"),
                || drop(transform::resize(&img, nw, nh)),
            );
        }
        for r in [
            Rect { x: 0, y: 0, w, h },
            Rect {
                x: w - 1,
                y: h - 1,
                w: 10,
                h: 10,
            },
            Rect {
                x: w + 5,
                y: 0,
                w: 3,
                h: 3,
            },
        ] {
            f.check(
                || format!("clip {w}x{h} {r:?}"),
                || drop(transform::clip(&img, r)),
            );
        }
        for a in [
            0.0,
            1.0,
            30.0,
            45.0,
            89.9,
            90.0,
            135.0,
            180.0,
            270.0,
            -30.0,
            -359.0,
            360.0,
            1e9,
            f64::NAN,
        ] {
            f.check(
                || format!("rotate {w}x{h} {a}"),
                || drop(transform::rotate(&img, a, [0; 3])),
            );
        }
        for roi in rois(w, h) {
            let ctx = || format!("{w}x{h} roi {roi:?}");
            f.check(
                || format!("flip_h {}", ctx()),
                || drop(transform::flip_horizontal(&img, roi)),
            );
            f.check(
                || format!("flip_v {}", ctx()),
                || drop(transform::flip_vertical(&img, roi)),
            );
            for (dx, dy) in [(0, 0), (5, -3), (-1000, 1000)] {
                f.check(
                    || format!("clone {dx},{dy} {}", ctx()),
                    || drop(transform::clone_offset(&img, roi, dx, dy, [0; 3])),
                );
            }
            for (a, b) in [
                ((0, 0), (0, 0)),
                ((1, 1), (w as i64 - 1, h as i64 - 1)),
                ((-50, -50), (500, 500)),
                ((2, 3), (2, 3)),
            ] {
                for elliptic in [false, true] {
                    f.check(
                        || format!("rubber {a:?}->{b:?} {elliptic} {}", ctx()),
                        || drop(transform::rubber(&img, roi, a, b, elliptic, [0; 3])),
                    );
                    f.check(
                        || format!("rubber_fragment {a:?}->{b:?} {}", ctx()),
                        || drop(transform::rubber_fragment(&img, &sel, roi, a, b, elliptic)),
                    );
                }
            }
            for k in MirrorKind::ALL {
                for (d, s) in [(0, 0), (25, 10), (50, 50)] {
                    f.check(
                        || format!("deform {k:?} {d} {s} {}", ctx()),
                        || drop(transform::deform(&img, roi, k, d, s)),
                    );
                    f.check(
                        || format!("deform_fragment {k:?} {}", ctx()),
                        || drop(transform::deform_fragment(&img, &sel, roi, k, d, s)),
                    );
                }
            }
            for a in [0.0, 30.0, 90.0, -45.0, 180.0] {
                f.check(
                    || format!("rotate_fragment {a} {}", ctx()),
                    || drop(transform::rotate_fragment(&img, &sel, roi, a)),
                );
            }
            f.check(
                || format!("move_fragment {}", ctx()),
                || drop(transform::move_fragment(&img, &sel, roi)),
            );
            for v in [false, true] {
                f.check(
                    || format!("flip_fragment {}", ctx()),
                    || drop(transform::flip_fragment(&img, &sel, roi, v)),
                );
            }
            let frag = transform::move_fragment(&img, &sel, roi);
            for (x, y, pw, ph) in [
                (0, 0, 1, 1),
                (-30, -30, 5, 5),
                (w as i64 - 1, h as i64 - 1, 40, 40),
                (1000, 1000, 2, 2),
                (0, 0, w * 4, h * 4),
            ] {
                f.check(
                    || format!("place_fragment {x},{y} {pw}x{ph} {}", ctx()),
                    || drop(transform::place_fragment(&img, &frag, x, y, pw, ph)),
                );
            }
        }
    }
    f.assert_none();
}

#[test]
fn selection_and_pen_never_panic() {
    quiet();
    let mut f = Failures(Vec::new());
    let far = 1_000_000;
    for (w, h) in SIZES {
        let img = test_image(w, h);
        let (wi, hi) = (w as i32, h as i32);
        let ctx = || format!("{w}x{h}");
        for (a, b) in [
            ((0, 0), (wi, hi)),
            ((-far, -far), (far, far)),
            ((5, 5), (5, 5)),
            ((wi, hi), (-3, -3)),
        ] {
            f.check(
                || format!("rect {a:?} {b:?} {}", ctx()),
                || drop(sel::rasterize_rect(w, h, a, b)),
            );
        }
        for (cx, cy, rx, ry) in [
            (0, 0, 0, 0),
            (wi / 2, hi / 2, wi, hi),
            (-far, far, far, 1),
            (3, 3, -5, 2),
        ] {
            f.check(
                || format!("ellipse {cx},{cy} {rx}x{ry} {}", ctx()),
                || drop(sel::rasterize_ellipse(w, h, cx, cy, rx, ry)),
            );
        }
        for pts in [
            vec![],
            vec![(1, 1)],
            vec![(0, 0), (wi, 0)],
            vec![(-far, -far), (far, 0), (0, far)],
            vec![(0, 0), (wi, hi), (wi, 0), (0, hi)],
        ] {
            f.check(
                || format!("polygon {pts:?} {}", ctx()),
                || drop(sel::rasterize_polygon(w, h, &pts)),
            );
            let brush = sel::Brush {
                size: 11,
                kind: sel::BrushKind::Circle,
            };
            f.check(
                || format!("freehand {pts:?} {}", ctx()),
                || drop(sel::rasterize_freehand(w, h, &pts, &brush)),
            );
        }
        let masks = [Mask::full(w, h), Mask::empty(w, h), random_mask(w, h, 9)];
        for m in &masks {
            for inside in [true, false] {
                for edge in [
                    sel::Edge::Sharp,
                    sel::Edge::SmoothLow,
                    sel::Edge::SmoothMedium,
                    sel::Edge::SmoothHigh,
                ] {
                    for feather in [0, 1, 3, 1000, -5] {
                        f.check(
                            || format!("weights {inside} {edge:?} {feather} {}", ctx()),
                            || {
                                let wts = sel::selection_weights(m, inside, edge, feather);
                                let mut d = img.clone();
                                sel::commit(&mut d, &img, &wts);
                            },
                        );
                    }
                }
            }
            f.check(|| format!("outline {}", ctx()), || drop(sel::outline(m)));
            f.check(
                || format!("double_click {}", ctx()),
                || {
                    let _ = sel::double_click_inside(m, w + 10, h + 10);
                },
            );
        }
        for (x, y) in [(0, 0), (w - 1, h - 1), (w / 2, h / 2)] {
            for mode in [sel::Match::Rgb, sel::Match::Hsv] {
                for tol in [0, 1, 51, 100, 255] {
                    for unifold in [true, false] {
                        f.check(
                            || format!("wand ({x},{y}) {mode:?} {tol} {unifold} {}", ctx()),
                            || drop(sel::magic_wand(&img, (x, y), tol, mode, unifold)),
                        );
                    }
                }
            }
        }
        let processed = test_image(w, h);
        for kind in [
            sel::BrushKind::Square,
            sel::BrushKind::Circle,
            sel::BrushKind::Airbrush,
            sel::BrushKind::FadingOpacity,
            sel::BrushKind::FadingSize,
            sel::BrushKind::FadingSpray,
        ] {
            for size in [1, 2, 3, 5, 11, 200] {
                for edge in [sel::Edge::Sharp, sel::Edge::SmoothHigh] {
                    let p = sel::PenParams {
                        brush: sel::Brush { size, kind },
                        edge,
                        airbrush: 16000,
                    };
                    let mut stroke = sel::Stroke::begin(3);
                    for c in [
                        (0, 0),
                        (wi - 1, hi - 1),
                        (-far, -far),
                        (far, 3),
                        (-5, hi + 2),
                    ] {
                        f.check(
                            || format!("paint_dab {kind:?} {size} {c:?} {}", ctx()),
                            || {
                                let mut d = img.clone();
                                sel::paint_dab(
                                    &mut d,
                                    &processed,
                                    &img,
                                    c,
                                    &p,
                                    &stroke,
                                    &mut MsRand::default(),
                                );
                                sel::erase_dab(
                                    &mut d,
                                    &img,
                                    c,
                                    &p,
                                    &stroke,
                                    &mut MsRand::default(),
                                );
                            },
                        );
                        for _ in 0..200 {
                            stroke.advance();
                        }
                    }
                }
            }
        }
    }
    f.assert_none();
}

/// Corrupt and hostile files must produce an error, never a crash.
#[test]
fn hostile_files_are_rejected() {
    quiet();
    let dir = std::env::temp_dir().join(format!("pman-hostile-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut f = Failures(Vec::new());
    let img = test_image(37, 11);

    // Every format, truncated at many lengths and with flipped bytes.
    for fmt in [
        Format::Bmp,
        Format::Gif,
        Format::Tiff,
        Format::Jpeg,
        Format::Targa,
        Format::Pcx,
        Format::Png,
    ] {
        let ext = fmt.extensions()[0];
        let good = dir.join(format!("good.{ext}"));
        formats::save(&good, &img, None).unwrap();
        let bytes = std::fs::read(&good).unwrap();
        let mut r = MsRand::new(11);
        for cut in (0..bytes.len()).step_by((bytes.len() / 40).max(1)) {
            let p = dir.join(format!("cut.{ext}"));
            std::fs::write(&p, &bytes[..cut]).unwrap();
            f.check(
                || format!("{ext} truncated at {cut}"),
                || drop(formats::load(&p)),
            );
        }
        for round in 0..60 {
            let mut b = bytes.clone();
            for _ in 0..4 {
                let i = r.rand() as usize % b.len();
                b[i] = r.rand() as u8;
            }
            let p = dir.join(format!("fuzz.{ext}"));
            std::fs::write(&p, &b).unwrap();
            f.check(
                || format!("{ext} fuzz round {round}"),
                || drop(formats::load(&p)),
            );
        }
    }

    // The original READPCX overrun: bytes-per-line smaller than the width,
    // plus absurd dimensions.
    let mut pcx = vec![0u8; 128];
    pcx[0] = 10;
    pcx[1] = 5;
    pcx[2] = 1;
    pcx[3] = 8;
    pcx[8..10].copy_from_slice(&199u16.to_le_bytes());
    pcx[10..12].copy_from_slice(&9u16.to_le_bytes());
    pcx[65] = 3;
    pcx[66..68].copy_from_slice(&4u16.to_le_bytes());
    pcx.extend(std::iter::repeat_n(7u8, 5000));
    let p = dir.join("short_bpl.pcx");
    std::fs::write(&p, &pcx).unwrap();
    f.check(
        || "pcx short bytes-per-line".into(),
        || assert!(formats::load(&p).is_err()),
    );
    pcx[8..12].copy_from_slice(&[0xff, 0xff, 0xff, 0xff]);
    pcx[66..68].copy_from_slice(&0xffffu16.to_le_bytes());
    std::fs::write(&p, &pcx).unwrap();
    f.check(
        || "pcx 65536x65536".into(),
        || assert!(formats::load(&p).is_err()),
    );

    // Empty and garbage files.
    for (name, data) in [
        ("empty.png", vec![]),
        ("garbage.tif", vec![0x49u8; 3]),
        ("noext", b"hello".to_vec()),
    ] {
        let p = dir.join(name);
        std::fs::write(&p, &data).unwrap();
        f.check(|| name.to_string(), || assert!(formats::load(&p).is_err()));
    }
    std::fs::remove_dir_all(&dir).ok();
    f.assert_none();
}

/// The original kept paths in 64-byte buffers; deep folders, long and
/// non-ASCII names and dots in directory names must all just work.
#[test]
fn deep_and_long_paths_work() {
    let mut dir: PathBuf = std::env::temp_dir().join(format!("pman-deep-{}", std::process::id()));
    let root = dir.clone();
    for i in 0..30 {
        dir.push(format!("Уровень {i}.v1.55 — dir with spaces and dots"));
    }
    std::fs::create_dir_all(&dir).unwrap();
    let img = test_image(13, 7);
    for ext in ["bmp", "tif", "pcx", "png", "tga", "gif", "jpg", "eps"] {
        let p = dir.join(format!(
            "Picture Man – тест {}.image.{ext}",
            "x".repeat(150)
        ));
        assert!(
            p.as_os_str().len() > 1500,
            "path is long: {}",
            p.as_os_str().len()
        );
        formats::save(&p, &img, None).unwrap_or_else(|e| panic!("save {ext}: {e}"));
        match ext {
            "eps" => {}
            "jpg" | "gif" => assert_eq!(formats::load(&p).unwrap().w, 13),
            _ => assert_eq!(formats::load(&p).unwrap(), img, "{ext}"),
        }
    }
    std::fs::remove_dir_all(&root).ok();
}

/// Fixed-point coordinate maths must not overflow on big images (the
/// original's 32-bit arithmetic broke on selections over ~500–700 px).
#[test]
fn large_images_do_not_overflow() {
    quiet();
    let mut f = Failures(Vec::new());
    let (w, h) = (4000, 2600);
    let img = test_image(w, h);
    let roi = Rect { x: 0, y: 0, w, h };
    for k in MirrorKind::ALL {
        f.check(
            || format!("deform {k:?}"),
            || drop(transform::deform(&img, roi, k, 50, 50)),
        );
    }
    for elliptic in [false, true] {
        f.check(
            || format!("rubber {elliptic}"),
            || {
                drop(transform::rubber(
                    &img,
                    roi,
                    (100, 2500),
                    (3900, 10),
                    elliptic,
                    [0; 3],
                ))
            },
        );
    }
    f.check(
        || "rotate".into(),
        || drop(transform::rotate(&img, 33.0, [0; 3])),
    );
    f.check(
        || "resize".into(),
        || drop(transform::resize(&img, 9000, 3)),
    );
    f.check(
        || "gradient".into(),
        || {
            drop(fill::gradient(
                &img,
                roi,
                GradientKind::Radial(RadialShape::Diagonal),
                [0; 3],
                [255; 3],
            ))
        },
    );
    f.check(
        || "patch".into(),
        || drop(fill::patch(&img, roi, &Mask::full(w, h), PatchMode::Full)),
    );
    f.check(
        || "weights".into(),
        || {
            drop(sel::selection_weights(
                &Mask::full(w, h),
                true,
                sel::Edge::SmoothHigh,
                650,
            ))
        },
    );
    f.check(
        || "ellipse".into(),
        || drop(sel::rasterize_ellipse(w, h, 2000, 1300, 1999, 1299)),
    );
    f.assert_none();
}
