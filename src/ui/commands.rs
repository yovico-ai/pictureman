//! The command set and menu tree, mirroring PMAN.EXE's MENU resource.
//! Discriminants are the original WM_COMMAND IDs.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum Cmd {
    // File
    New = 120,
    Open = 101,
    Reload = 139,
    Save = 118,
    SaveAs = 114,
    Exit = 116,
    // Edit
    Undo = 121,
    Erase = 162,
    Size = 102,
    Clip = 108,
    Move = 167,
    FlipH = 136,
    FlipV = 140,
    Rubber = 228,
    Deformations = 141,
    Rotate = 137,
    RgbTv = 104,
    RgbLinear = 105,
    Gamma = 143,
    Expand = 154,
    Equalization = 144,
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
    Copy = 106,
    Paste = 119,
    PasteFrom = 117,
    // View
    Zoom(i8) = 200,
    AnimateSelection = 168,
    Toolbox = 219,
    // Options
    PickColor = 277,
    MagicWandOptions = 231,
    CreateBackup = 138,
    PreserveMask = 232,
    // Help
    About = 113,
}

impl Cmd {
    /// Needs an open image and a free worker.
    pub fn needs_image(self) -> bool {
        self.is_image_op() || matches!(self, Cmd::Copy | Cmd::Paste | Cmd::PasteFrom | Cmd::Size)
    }
}

impl Cmd {
    /// Does this command operate on (a selected area of) the image?
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

    pub fn title(self) -> &'static str {
        use Cmd::*;
        match self {
            Size => "Size",
            Clip => "Clip",
            Move => "Move",
            FlipH => "Flip horizontal",
            FlipV => "Flip vertical",
            Rubber => "Rubber",
            Deformations => "Deformations",
            Rotate => "Rotate",
            RgbTv => "RGB control (TV)",
            RgbLinear => "Linear grey/color map",
            Gamma => "Gamma correction",
            Expand => "Expand",
            Equalization => "Equalization",
            FillPlain => "Fill with color",
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
            Erase => "Erase",
            Copy => "Copy",
            Paste => "Paste",
            PasteFrom => "Paste from file",
            New => "New image parameters",
            MagicWandOptions => "Magic wand options",
            _ => "",
        }
    }
}

pub enum MenuItem {
    Cmd(&'static str, Cmd),
    Sub(&'static str, Vec<MenuItem>),
    Sep,
}

use MenuItem::{Cmd as C, Sep, Sub};

/// The Edit menu, as in the original (the © marks the authors' own
/// algorithms in 1.55's menu).
pub fn edit_menu() -> Vec<MenuItem> {
    use Cmd::*;
    vec![
        C("Undo", Undo),
        C("Erase", Erase),
        Sep,
        Sub(
            "Transformation",
            vec![
                C("Size…", Size),
                C("Clip", Clip),
                C("Move", Move),
                Sub("Flip", vec![C("Horizontal", FlipH), C("Vertical", FlipV)]),
                C("Rubber", Rubber),
                C("Deformations…", Deformations),
                C("Rotate…", Rotate),
            ],
        ),
        Sub(
            "Tune",
            vec![
                Sub(
                    "RGB control",
                    vec![C("TV…", RgbTv), C("Linear…", RgbLinear)],
                ),
                C("Gamma correction…", Gamma),
                C("Expand", Expand),
                C("Equalization", Equalization),
            ],
        ),
        Sub(
            "Fill area",
            vec![
                Sub(
                    "Color",
                    vec![C("Plain", FillPlain), C("Fluctuated…", FillFluctuated)],
                ),
                Sub(
                    "Gradient",
                    vec![
                        C("Vertical…", GradientV),
                        C("Horizontal…", GradientH),
                        C("Radial…", GradientRadial),
                    ],
                ),
                Sub(
                    "Pattern",
                    vec![
                        C("Tiled…", PatternTiled),
                        C("Scaled…", PatternScaled),
                        C("Fitted…", PatternFitted),
                    ],
                ),
                Sub(
                    "Patch ©",
                    vec![
                        C("Full", PatchFull),
                        C("Horizontal", PatchH),
                        C("Vertical", PatchV),
                    ],
                ),
            ],
        ),
        Sub(
            "Processing",
            vec![
                C("Smoothing…", Smoothing),
                C("Sharpening", Sharpening),
                C("Heavy sharpening", HeavySharpening),
                C("Spot removing…", SpotRemoving),
                C("Minimum…", Minimum),
                C("Maximum…", Maximum),
                C("Hand drawing ©…", HandDrawing),
                C("Cleaning background…", CleaningBackground),
                C("Contour outlining", ContourOutlining),
                C("Emboss", Emboss),
                C("Mosaic…", Mosaic),
                C("Faceted glass ©…", FacetedGlass),
                C("Scatter…", Scatter),
            ],
        ),
        Sep,
        C("Copy", Copy),
        C("Paste", Paste),
        C("Paste from…", PasteFrom),
    ]
}
