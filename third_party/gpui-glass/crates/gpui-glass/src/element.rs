use crate::{GlassDraw, GlassParams, GlassRenderer, Shape, backend};
use gpui::{Canvas, Div, ParentElement, Styled, canvas, div};
use std::sync::Arc;

/// A backdrop canvas. Put it before foreground children; it follows its laid-out
/// bounds and reads the window's scale factor at paint time.
pub fn glass_layer(
    renderer: Arc<GlassRenderer>,
    id: u64,
    params: GlassParams,
    shape: Shape,
) -> Canvas<()> {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let scale = window.scale_factor();
            backend::paint(
                window,
                bounds,
                GlassDraw {
                    renderer,
                    id,
                    params,
                    shape,
                    scale,
                    contour: None,
                },
            );
        },
    )
}

/// A normal GPUI div with a glass layer behind any children added by the caller.
/// Give it dimensions, padding, text, listeners, and children using normal GPUI
/// traits. Its background remains transparent unless you explicitly set one.
/// IDs must be stable and unique among surfaces sharing the renderer.
pub fn glass(renderer: Arc<GlassRenderer>, id: u64, params: GlassParams, shape: Shape) -> Div {
    div().relative().child(
        glass_layer(renderer, id, params, shape)
            .absolute()
            .inset_0(),
    )
}
