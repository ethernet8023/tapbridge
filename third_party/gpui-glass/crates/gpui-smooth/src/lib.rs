#![doc = include_str!("../README.md")]

pub mod capsule;
#[cfg(feature = "gpui")]
pub mod paint;
pub mod rounded_rect;
#[cfg(feature = "gpui")]
pub use paint::{ShapeStyle, shape_layer, smooth};

/// Geometry only: no GPUI dependency or renderer patch is needed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SmoothShape {
    Capsule { smoothing: f32 },
    RoundedRect { radius: f32, smoothing: f32 },
}
impl SmoothShape {
    pub fn capsule(smoothing: f32) -> Self {
        Self::Capsule { smoothing }
    }
    pub fn rounded_rect(radius: f32, smoothing: f32) -> Self {
        Self::RoundedRect { radius, smoothing }
    }
    pub fn outline(self, width: f32, height: f32) -> Vec<[f32; 2]> {
        match self {
            Self::Capsule { smoothing } => capsule::outline(width, height, smoothing),
            Self::RoundedRect { radius, smoothing } => {
                rounded_rect::outline(width, height, radius, smoothing)
            }
        }
    }
}
