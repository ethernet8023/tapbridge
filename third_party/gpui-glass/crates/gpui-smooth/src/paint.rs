//! Optional GPUI adapter. This uses public path drawing; no GPUI patch is needed.
use crate::SmoothShape;
use gpui::{
    Background, Bounds, Canvas, Div, ParentElement, PathBuilder, Pixels, Styled, Window, canvas,
    div, point, px,
};

#[derive(Clone)]
pub struct ShapeStyle {
    pub fill: Background,
    /// A centered stroke, extending half its width outside the contour.
    pub stroke: Option<(Pixels, Background)>,
    /// Empty means solid. Nonfinite or sub-0.1px entries fall back to solid.
    pub dash: Vec<Pixels>,
}
impl ShapeStyle {
    pub fn new(fill: impl Into<Background>) -> Self {
        Self {
            fill: fill.into(),
            stroke: None,
            dash: Vec::new(),
        }
    }
    pub fn stroke(mut self, width: Pixels, color: impl Into<Background>) -> Self {
        self.stroke = Some((width, color.into()));
        self
    }
    pub fn dash(mut self, pattern: Vec<Pixels>) -> Self {
        self.dash = pattern;
        self
    }
}

/// Paint fill and optional centered stroke along the same smoothed outline.
/// GPUI applies its inherited opacity and rectangular content mask to each path.
pub fn paint_shape(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    shape: SmoothShape,
    style: &ShapeStyle,
) {
    let points = shape.outline(f32::from(bounds.size.width), f32::from(bounds.size.height));
    if points.len() < 3 {
        return;
    }
    let draw = |mut builder: PathBuilder, background: Background, window: &mut Window| {
        for (i, p) in points.iter().enumerate() {
            let p = bounds.origin + point(px(p[0]), px(p[1]));
            if i == 0 {
                builder.move_to(p);
            } else {
                builder.line_to(p);
            }
        }
        builder.close();
        if let Ok(path) = builder.build() {
            window.paint_path(path, background);
        }
    };
    draw(PathBuilder::fill(), style.fill, window);
    if let Some((width, background)) = style
        .stroke
        .filter(|(width, _)| f32::from(*width).is_finite() && *width > px(0.))
    {
        draw(stroke_builder(width, &style.dash), background, window);
    }
}

fn stroke_builder(width: Pixels, dash: &[Pixels]) -> PathBuilder {
    let builder = PathBuilder::stroke(width);
    // GPUI's dash tessellator expects a nonempty pattern with forward progress.
    // Passing an empty pattern panics; zero lengths can loop forever.
    if !dash.is_empty()
        && dash
            .iter()
            .all(|v| f32::from(*v).is_finite() && *v >= px(0.1))
    {
        builder.dash_array(dash)
    } else {
        builder
    }
}

/// A layoutable canvas without foreground children. Use ShapeStyle for its
/// contour's appearance; normal `.bg()`/`.border()` style the canvas rectangle.
pub fn shape_layer(shape: SmoothShape, style: ShapeStyle) -> Canvas<()> {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| paint_shape(window, bounds, shape, &style),
    )
}

/// A normal GPUI div with a shape layer behind the caller's children.
/// Supports ordinary sizing, padding, text, listeners and children. This is
/// composition, not a replacement for GPUI's clip/shadow/hit-test primitives.
pub fn smooth(shape: SmoothShape, style: ShapeStyle) -> Div {
    div()
        .relative()
        .child(shape_layer(shape, style).absolute().inset_0())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn solid_dashed_and_invalid_patterns_tessellate() {
        for dash in [
            vec![],
            vec![px(4.), px(2.)],
            vec![px(4.)],
            vec![px(0.)],
            vec![px(-1.)],
            vec![px(f32::NAN)],
        ] {
            let mut path = stroke_builder(px(1.), &dash);
            path.move_to(point(px(0.), px(0.)));
            path.line_to(point(px(100.), px(0.)));
            path.line_to(point(px(100.), px(40.)));
            path.line_to(point(px(0.), px(40.)));
            path.close();
            assert!(path.build().is_ok());
        }
    }
}
