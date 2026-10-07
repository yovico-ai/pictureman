//! Image operations recovered from PMAN.EXE. Each function takes the source
//! image and the region of interest and returns a full-size image whose
//! pixels inside the ROI are processed; the caller commits it through the
//! selection mask (`core::blend_through_mask`).

pub mod effects;
pub mod fill;
pub mod filters;
pub mod transform;
pub mod tune;
