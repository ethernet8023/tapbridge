//! The only names that require our GPUI backdrop-hook patch.
//! Keep this adapter small so an eventual upstream hook is easy to adopt.
pub(crate) use gpui_wgpu::{BackdropContext, WgpuBackdrop, WgpuBackdropEffect};
use std::sync::Arc;

/// The scene needs the complete effect extent for ordering and culling, while the
/// shader needs the original shape bounds for its distance-field coordinates.
struct OutsetBackdrop {
    draw: super::GlassDraw,
    shape_bounds: gpui::Bounds<gpui::ScaledPixels>,
}

impl WgpuBackdrop for OutsetBackdrop {
    fn damage_bounds(&self, filter: &gpui::BackdropFilter) -> gpui::Bounds<gpui::ScaledPixels> {
        filter.bounds
    }

    fn paint(&self, context: BackdropContext<'_>) {
        let mut filter = context.filter.clone();
        filter.bounds = self.shape_bounds;
        self.draw.paint(BackdropContext {
            filter: &filter,
            ..context
        });
    }
}

pub(crate) fn paint(
    window: &mut gpui::Window,
    bounds: gpui::Bounds<gpui::Pixels>,
    draw: super::GlassDraw,
) {
    let outset = draw.params.paint_outset();
    if outset > 0. {
        // Match Window's normal backdrop snapping: snap the two corners, not
        // origin and size independently, so toggling a shadow cannot move a rim.
        let top_left = window.pixel_snap_point(bounds.origin);
        let bottom_right = window.pixel_snap_point(bounds.bottom_right());
        let effect = OutsetBackdrop {
            shape_bounds: gpui::Bounds::from_corners(top_left, bottom_right)
                .scale(window.scale_factor()),
            draw,
        };
        window.paint_custom_backdrop(
            bounds.dilate(gpui::px(outset)),
            WgpuBackdropEffect(Arc::new(effect)).into_gpui(),
        );
    } else {
        window.paint_custom_backdrop(bounds, draw.effect());
    }
}
