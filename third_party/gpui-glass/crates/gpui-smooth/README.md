# gpui-smooth

Lisse-style capsules and uniform rounded rectangles. Geometry has **zero runtime
dependencies** and does not require GPUI or any renderer patch. This is a focused
Rust port, not the whole Lisse library: per-corner radii and the other Lisse curve
families (clothoid and superellipse) are not included.

```rust
use gpui_smooth::SmoothShape;
let shape = SmoothShape::capsule(0.65);
let points = shape.outline(160.0, 50.0);
let card = SmoothShape::rounded_rect(24.0, 0.6).outline(320.0, 180.0);
assert!(points.len() > 4 && card.len() > 4);
```

Points are clockwise logical-pixel coordinates without a repeated closing vertex.
Radius and smoothing are clamped to available space. Invalid dimensions return an
empty contour. Rounded rectangles blend smoothly into capsules; fully rounded
squares remain circles. Curves are flattened to 0.01 logical-pixel tolerance.

## GPUI adapter

Enable the optional `gpui` feature to paint using public GPUI-CE APIs:

```toml
[dependencies]
gpui-smooth = { path = "../gpui-glass/crates/gpui-smooth", features = ["gpui"] }
```

```rust
# #[cfg(feature = "gpui")] {
use gpui::{prelude::*, px, rgb};
use gpui_smooth::{smooth, SmoothShape, ShapeStyle};
let button = smooth(
    SmoothShape::capsule(0.65),
    ShapeStyle::new(rgb(0x396958)).stroke(px(1.0), rgb(0x203e30)),
)
.id("save")
.w(px(120.0)).h(px(44.0))
.flex().items_center().justify_center()
.text_color(rgb(0xffffff))
.child("Save")
.on_click(|_, _, _| {});
# }
```

`smooth` returns a normal GPUI `Div`. `shape_layer` returns a `Canvas<()>` for
manual layering. `paint::paint_shape` can be used inside another paint callback.
These helpers compile against unpatched GPUI-CE; their only rendering API is
`PathBuilder`/`Window::paint_path`.

## Styling contract

| Feature | Support |
| --- | --- |
| Size, layout, margins, padding, foreground text/children, interaction | Normal GPUI container behavior |
| Solid and GPUI gradient fills | `ShapeStyle::new(background)` |
| Uniform centered stroke, gradient stroke, dashes | `ShapeStyle::stroke` / `dash` |
| Inherited opacity and rectangular ancestor clipping | Applied by GPUI |
| Hover/active state | Normal container events/styles; update `ShapeStyle` from state to change the contour's fill |
| Per-side borders, inset/outer stroke alignment | Not implemented |
| Shadows following the custom contour | Not implemented |
| Clipping children/images to the custom contour | Not implemented; `.overflow_hidden()` clips using GPUI's own bounds/corners |
| Shape-aware pointer hit testing | Not automatic; the div's hit box is rectangular |

Use `ShapeStyle` for the contour's paint. Calling `.bg()`, `.border()`, `.shadow()`,
or `.rounded()` styles the **ordinary container**, not the Lisse path. Full styling
parity would require upstream custom-outline support for clipping, shadows, and
hit testing. The helper does not silently approximate those features.

Source attribution and pinned upstream revision: `THIRD_PARTY_NOTICES.md`.
BSD-3-Clause license: `LICENSE`. The crate can be copied and built independently, including
without its GPUI feature. It has not been published to crates.io.
