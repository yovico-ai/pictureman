//! The command set and the menus. Discriminants are the original
//! WM_COMMAND IDs where the command existed in 1.55 (300+ are new).

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum Cmd {
    // File
    New = 120,
    Open = 101,
    Reload = 139,
    Save = 118,
    SaveAs = 114,
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    Exit = 116,
    // Edit
    Undo = 121,
    Redo = 300,
    Erase = 162,
    Copy = 106,
    Paste = 119,
    PasteFrom = 117,
    // Select
    SelectAll = 301,
    SelectNone = 302,
    InvertSelection = 303,
    MagicWandOptions = 231,
    // Image (Transformation)
    Size = 102,
    Clip = 108,
    Move = 167,
    FlipH = 136,
    FlipV = 140,
    Rubber = 228,
    Deformations = 141,
    Rotate = 137,
    // Adjust (Tune)
    RgbTv = 104,
    RgbLinear = 105,
    Gamma = 143,
    Expand = 154,
    Equalization = 144,
    // Fill area
    FillPlain = 153,
    FillFluctuated = 282,
    GradientV = 221,
    GradientH = 222,
    GradientRadial = 223,
    PatternTiled = 224,
    PatternScaled = 233,
    PatternFitted = 234,
    PatchFull = 155,
    PatchH = 156,
    PatchV = 157,
    // Filters (Processing)
    Smoothing = 107,
    Sharpening = 122,
    HeavySharpening = 123,
    SpotRemoving = 124,
    Minimum = 125,
    Maximum = 126,
    HandDrawing = 128,
    CleaningBackground = 129,
    ContourOutlining = 130,
    Emboss = 131,
    Mosaic = 132,
    FacetedGlass = 133,
    Scatter = 134,
    // View
    Zoom(i8) = 200,
    ZoomIn = 304,
    ZoomOut = 305,
    ZoomFit = 306,
    AnimateSelection = 168,
    Toolbox = 219,
    CreateBackup = 138,
    // Help
    About = 113,
}

impl Cmd {
    /// Needs an open image and a free worker.
    pub fn needs_image(self) -> bool {
        self.is_image_op() || matches!(self, Cmd::Copy | Cmd::Paste | Cmd::PasteFrom | Cmd::Size)
    }

    /// Does this command change the image (in the selection)?
    pub fn is_image_op(self) -> bool {
        use Cmd::*;
        matches!(
            self,
            Clip | Move
                | Size
                | Rubber
                | Deformations
                | Rotate
                | FlipH
                | FlipV
                | RgbTv
                | RgbLinear
                | Gamma
                | Expand
                | Equalization
                | FillPlain
                | FillFluctuated
                | GradientV
                | GradientH
                | GradientRadial
                | PatternTiled
                | PatternScaled
                | PatternFitted
                | PatchFull
                | PatchH
                | PatchV
                | Smoothing
                | Sharpening
                | HeavySharpening
                | SpotRemoving
                | Minimum
                | Maximum
                | HandDrawing
                | CleaningBackground
                | ContourOutlining
                | Emboss
                | Mosaic
                | FacetedGlass
                | Scatter
                | Erase
        )
    }

    /// Can the brush paint with it?
    pub fn paintable(self) -> bool {
        self.is_image_op() && !matches!(self, Cmd::Size | Cmd::Clip | Cmd::Rotate)
    }

    pub fn title(self) -> &'static str {
        use Cmd::*;
        match self {
            Size => "Size",
            Clip => "Crop to selection",
            Move => "Move",
            FlipH => "Flip horizontal",
            FlipV => "Flip vertical",
            Rubber => "Rubber",
            Deformations => "Deformations",
            Rotate => "Rotate",
            RgbTv => "Brightness / contrast (TV)",
            RgbLinear => "Linear grey/color map",
            Gamma => "Gamma correction",
            Expand => "Expand",
            Equalization => "Equalization",
            FillPlain => "Color",
            FillFluctuated => "Fluctuated color",
            GradientV => "Vertical gradient",
            GradientH => "Horizontal gradient",
            GradientRadial => "Radial gradient",
            PatternTiled => "Tiled pattern",
            PatternScaled => "Scaled pattern",
            PatternFitted => "Fitted pattern",
            PatchFull => "Full patch",
            PatchH => "Horizontal patch",
            PatchV => "Vertical patch",
            Smoothing => "Smoothing",
            Sharpening => "Sharpening",
            HeavySharpening => "Heavy sharpening",
            SpotRemoving => "Spot removing",
            Minimum => "Minimum",
            Maximum => "Maximum",
            HandDrawing => "Hand drawing",
            CleaningBackground => "Cleaning background",
            ContourOutlining => "Contour outlining",
            Emboss => "Emboss",
            Mosaic => "Mosaic",
            FacetedGlass => "Faceted glass",
            Scatter => "Scatter",
            Erase => "Revert area",
            Copy => "Copy",
            Paste => "Paste",
            PasteFrom => "Paste from file",
            New => "New image",
            MagicWandOptions => "Magic wand options",
            _ => "",
        }
    }
}

pub enum MenuItem {
    /// Label, command, keyboard shortcut text.
    Cmd(&'static str, Cmd, Option<&'static str>),
    Sub(&'static str, Vec<MenuItem>),
    Sep,
}

use MenuItem::{Sep, Sub};

fn c(label: &'static str, cmd: Cmd) -> MenuItem {
    MenuItem::Cmd(label, cmd, None)
}

fn k(label: &'static str, cmd: Cmd, key: &'static str) -> MenuItem {
    MenuItem::Cmd(label, cmd, Some(key))
}

/// The image menus. The original's Edit menu held everything; it is split
/// the usual way here, keeping the 1.55 command names (© marks the authors'
/// own algorithms, as in 1.55's menu).
pub fn image_menus() -> Vec<(&'static str, Vec<MenuItem>)> {
    use Cmd::*;
    vec![
        (
            "Edit",
            vec![
                k("Undo", Undo, "Ctrl+Z"),
                k("Redo", Redo, "Ctrl+Y"),
                Sep,
                k("Copy", Copy, "Ctrl+C"),
                k("Paste", Paste, "Ctrl+V"),
                c("Paste from…", PasteFrom),
                Sep,
                c("Revert area", Erase),
            ],
        ),
        (
            "Select",
            vec![
                k("All", SelectAll, "Ctrl+A"),
                k("None", SelectNone, "Ctrl+D"),
                k("Invert", InvertSelection, "Ctrl+Shift+I"),
                Sep,
                c("Magic wand options…", MagicWandOptions),
            ],
        ),
        (
            "Image",
            vec![
                c("Size…", Size),
                c("Crop to selection", Clip),
                Sep,
                c("Rotate…", Rotate),
                c("Flip horizontal", FlipH),
                c("Flip vertical", FlipV),
                c("Move", Move),
                Sep,
                c("Deformations…", Deformations),
                c("Rubber", Rubber),
            ],
        ),
        (
            "Adjust",
            vec![
                c("Brightness / contrast (TV)…", RgbTv),
                c("Linear grey/color map…", RgbLinear),
                c("Gamma correction…", Gamma),
                Sep,
                c("Expand", Expand),
                c("Equalization", Equalization),
            ],
        ),
        (
            "Fill",
            vec![
                c("Color", FillPlain),
                c("Fluctuated color…", FillFluctuated),
                Sub(
                    "Gradient",
                    vec![
                        c("Vertical…", GradientV),
                        c("Horizontal…", GradientH),
                        c("Radial…", GradientRadial),
                    ],
                ),
                Sub(
                    "Pattern",
                    vec![
                        c("Tiled…", PatternTiled),
                        c("Scaled…", PatternScaled),
                        c("Fitted…", PatternFitted),
                    ],
                ),
                Sub(
                    "Patch ©",
                    vec![
                        c("Full", PatchFull),
                        c("Horizontal", PatchH),
                        c("Vertical", PatchV),
                    ],
                ),
            ],
        ),
        (
            "Filters",
            vec![
                c("Smoothing…", Smoothing),
                c("Sharpening", Sharpening),
                c("Heavy sharpening", HeavySharpening),
                c("Spot removing…", SpotRemoving),
                c("Minimum…", Minimum),
                c("Maximum…", Maximum),
                Sep,
                c("Hand drawing ©…", HandDrawing),
                c("Cleaning background…", CleaningBackground),
                c("Contour outlining", ContourOutlining),
                c("Emboss", Emboss),
                Sep,
                c("Mosaic…", Mosaic),
                c("Faceted glass ©…", FacetedGlass),
                c("Scatter…", Scatter),
            ],
        ),
    ]
}
