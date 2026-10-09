# gpui-glass

Cached backdrop refraction, uniform pre-refraction frost, optional interior fog,
spectral dispersion, layered lighting/shadows, and exterior reflections for
GPUI-CE's **WGPU backend**. The shader is
embedded; there are no runtime assets or demo dependencies.

**Requires the tested custom-backdrop GPUI patch.** Merely adding this crate to an
unpatched GPUI project cannot give it access to the rendered scene. The demo
workspace selects patched sources with `[patch.crates-io]`; the consuming
application must do the same. See the repository's `REUSE.md` for setup and update
checks. No build script modifies Cargo's cache or third-party source files.

```rust,no_run
use gpui::{prelude::*, px};
use gpui_glass::{glass, GlassParams, GlassRenderer, Shape};
use std::sync::Arc;

// Store this in your view/application; do not recreate it each render.
let renderer = Arc::new(GlassRenderer::default());
let button = glass(renderer, 1, GlassParams::default(), Shape::Capsule)
    .id("save")
    .w(px(120.0)).h(px(44.0))
    .flex().items_center().justify_center()
    .child("Save")
    .on_click(|_, _, _| {});
```

`glass` returns a normal GPUI div and puts the backdrop behind its children.
`glass_layer` provides the canvas alone. Both read the current window scale during
paint. Keep each surface ID stable and unique within its shared renderer (including
across windows, if you share one renderer between windows). Device changes recreate
GPU resources. Cache retention and explicit release are described below.

The container supports normal GPUI layout, text, children, and listeners. Its
background, borders, shadows, clipping, and hit testing still use GPUI's own
rectangle/corner geometry. The glass contour shapes the effect, not its children.
Use `gpui-smooth` for an explicitly matching painted stroke or solid fallback.

## Light, fog, and reflections

All widths, radii, and blur sigma values use logical pixels. `LightParams` supplies
five independent layers: dark exterior outline, sharp highlight, soft highlight, inner shadow,
and outer shadow. Each has an opacity plus a width or radius. A zero layer opacity
skips its shader work; `GlassParams::light = 0.0` skips all light layers and removes
the expanded outer-shadow drawing area. `LightParams::disabled()` zeros all five
opacities without changing their dimensions. `LightBlend::Native27` uses equations
decoded from the installed macOS 27.0 (26A428) QuartzCore AIR and host parameter
packing, with a locally inspected `NSGlassEffectView` recipe. This is a standalone
shader adaptation, not a call into Apple's private renderer; provenance and
remaining calibration limits are in [RESEARCH.md](../../RESEARCH.md).

The outline sits outside the contour, over the shadow. Highlights default to vertical
top/bottom lobes. Sharp and soft highlights each have `*_angle`, `*_inset`,
`*_focus`, `*_spread`, and `*_blend` controls; the inner shadow has its own angle,
inset, width, and blend. Angles are degrees clockwise from up; insets move a band
inward. `opposite` adjusts the secondary highlight. The native path combines the
sharp and diffuse masks before applying the backdrop-dependent vibrant color
matrix once. It lifts dark material more than light material, instead of adding
white over both. The exterior outline uses a negative polynomial color bias over
the shadow; the inner shadow uses the difference of two blurred distance steps.

In Native27, `*_focus` is the raw rational-curve coefficient, not opacity;
`*_spread` is the cosine threshold, not an angle. Defaults reproduce the capsule
recipe's one-point sharp band and eight-point diffuse band: sharp threshold
`cos(80°)`, curve 0; diffuse threshold `cos(52°)`, curve `11⅓`. The native public
amount maps to this coefficient as `1 / amount - 2`. Independently adjustable
opacity/gain remains available and is skipped at zero. Screen, SoftLight,
LinearDodge, Multiply, LinearBurn, Normal, and Overlay remain available for
experimentation and older presets.

The native matrix's negative extended-range values are clamped to zero for this
SDR renderer. The offscreen native recipe did not establish visible-control
opacity, so the adjustable gain is not a calibrated pixel match. Native's
additional blur-fill min/max blend is not implemented as a separate stage here.
The geometry direction field and layered outer shadow also remain this crate's
implementations. Run `python3 scripts/check-native27.py` from the repository root
for frozen equation references and numeric boundary checks; it tests an
independent CPU f32 model, not GPU execution or native screenshot equality.

Boundary directions are extended continuously through the interior using a cached
harmonic field. Opposing sides cancel without normalizing that cancellation back
into a sharp direction. This avoids medial-axis wedges for oversized lighting
bands and sharp/concave corners. The same field stabilizes refraction and edge
reflection; coverage distances and exterior sampling masks remain unchanged.
The solve happens on geometry changes, not on radius/opacity updates or redraws.

The outer shadow combines contact and penumbra layers. `outer_shadow_angle` defaults
downward; translation is `0.65 * radius + outer_shadow_offset` logical pixels.
Increasing the radius therefore also increases the offset. Shadow calculations run
only outside the fully covered material.

`blur` uniformly frosts the backdrop before refracting it. Optional fog uses `fog`
as an additional Gaussian sigma, `fog_edge` as an independent rim fade width, and
`fog_opacity` as its interior blend amount and `fog_edge_opacity` at the rim. Zero fog blur or both effective opacities zero omits its extra textures
and passes. `fog_edge = 0.0` applies fog without the rim fade; none of these controls
changes the refraction edge width.

`ReflectionParams` controls:

| Field | Meaning |
| --- | --- |
| `edge_intensity`, `edge_width` | Amount and inward thickness of the narrow reflection |
| `diffuse_intensity`, `diffuse_radius` | Amount and blur radius of native diffuse; inward reach in exterior-only mode |
| `diffuse_size_scaling`, `diffuse_radius_limit` | Automatic growth with surface size and its ceiling; zero scaling preserves the explicit radius |
| `sampling_radius` | Maximum exterior distance for edge/legacy diffuse sampling |
| `sharpness` | Detail retained for nearby sources, from 0 to 1 |
| `shadow_intensity` | Exterior color diffused into an enabled outer shadow |
| `edge_blend`, `diffuse_blend`, `shadow_blend` | Independent `LightBlend` operators |

Each zero intensity skips that contribution. When all three are zero (or sampling
radius is zero), reflection capture/filtering textures and passes are omitted. A
shadow tint has independent opacity and retains the same radius, offset, and layered profile even with black-shadow opacity zero. The light master disables both. Use
`ReflectionParams::disabled()` for that configuration.

Edge sources are masked to the **outside** before filtering. The default diffuse
path (`native_diffuse = true`) reconstructs macOS 27's backdrop bleed: a full
resolution source, native 13-point half-precision mip filtering, size-derived
curved displacement, and one explicit fractional-mip sample per fragment.
`diffuse_radius` controls blur independently of the geometry-derived bleed height.
`sampling_radius` controls the exterior edge source; it does not limit native
diffuse to an artificial capture ring. Setting `native_diffuse = false` selects
the prior exterior-only diffuse source; the debug menu exposes both modes.

Native pyramids share storage and update conservative damage between consecutive
surfaces, preserving GPUI paint order. They refresh completely at the first use
of every frame or when reuse cannot be proved safe. No temporal filtering or
previous-frame source caching is used. Unknown backdrop callbacks conservatively
invalidate their content mask; precise `damage_bounds` is an optional optimization.
See `RESEARCH.md` for recovered stages, remaining native mismatches and pixel tests.

The default `Radiance` operator combines glow on dark surfaces with a small,
bounded chromatic absorption on light surfaces. It borrows macOS 27's complementary
fourth-power luminance gates, with adjustable dark bias. It is a customizable SDR
hybrid, not Apple's complete bleed pipeline. Legacy blend operators remain
available. Reflection intensity changes only uniform arithmetic, never sampling
counts or texture dimensions.

Size scaling grows native diffuse blur (or legacy inward spread) independently of edge sampling reach.
Automatic growth stops at `diffuse_radius_limit`; an explicit radius above that
ceiling is still respected. Refraction/frost captures have independent padding,
so increasing reflection reach no longer expands their full-resolution textures.
The distance field defines all reflection boundaries, including concave contours.

This is a screen-space effect over the already-painted scene, not physical ray
tracing. It cannot see offscreen content or children painted after the surface.
An effect painted earlier can appear in a later surface's source image; there is
no recursive environment calculation. Order nearby content before reflecting panels
and keep their labels/controls afterwards.

A sidebar can reflect surrounding content without refracting its background:

```rust,no_run
use gpui::{prelude::*, px};
use gpui_glass::{glass, GlassParams, GlassRenderer, Shape};
use std::sync::Arc;

let renderer = Arc::new(GlassRenderer::default());
let params = GlassParams::default().reflection_only([0.98, 0.984, 0.969]);
let sidebar = glass(renderer, 6, params, Shape::Rectangle)
    .w(px(304.0)).h_full()
    .child("Inspector");
```

`reflection_only([r, g, b])` supplies an opaque RGB base and preserves reflection
settings while disabling transmission/refraction, frost, fog, dispersion, and
lighting. `glass_layer` supports keeping this surface fixed behind a separately
scrolling foreground, as in the demo inspector.

## Cache and diagnostics

Keep the renderer alive between frames. Reuse stable IDs to reuse texture storage,
bind groups, and contour atlases. Most optical controls only upload parameters;
texture configuration changes can reallocate a surface, while resized geometry,
contour changes, or a larger required shadow margin can rebuild its atlas. Capture
extent grows as needed and retains its warmed maximum. With geometry and enabled
pipeline configuration unchanged, tuning the sampling radius down and back up within
that retained extent does not reallocate the surface.

`GlassRenderer::stats()` returns `GlassStats`: `cached_surfaces`, `slot_allocations`,
`atlas_uploads`, `backdrop_captures`, `blur_passes`, and `reflection_passes`. Except
for the current cached-surface count, these are cumulative encoded-work counters
for the current GPU state, not timing measurements. `reflection_passes` counts one
compute pass per reflecting surface, not its individual dispatches or mip levels.
Compare snapshots around a render to check that disabled features do not encode
their optional passes.

128 IDs is a **soft retention threshold**, not a hard limit. When inserting a new
ID at or above that threshold, the cache may evict one least-recently-used surface
only if it has been idle for more than two seconds. Active working sets may exceed
128 so they do not repeatedly rebuild. There is no automatic whole-cache flush and
no fixed memory-byte bound; large surfaces and retained sampling extents need more
texture storage.

`GlassRenderer::clear_cache()` explicitly releases cached surfaces while retaining
pipelines and cumulative counters. Call it when cached views/documents are no longer
needed.

## Lower-level custom contours

`GlassContour::new` accepts normalized vertices of a simple closed polygon,
including concave shapes. Flatten curves once and divide pixel coordinates by the
contour's width/height. Pass the resulting `Arc<GlassContour>` as `GlassDraw.contour`,
then call `draw.paint_in(window, bounds)` during a GPUI paint callback. `paint_in`
reads the current scale and registers the full shadow/outline extent for GPUI scene
ordering and culling while retaining the original contour bounds.

`GlassDraw::effect()` remains a lower-level renderer payload. Its caller is responsible
for registering expanded shadow/outline bounds without changing the contour's original shape
coordinates. Prefer `paint_in` unless implementing a custom scene adapter. The `glass`
and `glass_layer` helpers already handle this registration.

The `Shape` presets are retained for convenience. `Rectangle` has square corners.
`Capsule` uses `gpui-smooth`'s
Lisse geometry; `Rounded` is the original demo's superellipse study shape, not
Lisse's rounded rectangle. Use `SmoothShape::rounded_rect(...).outline(...)` and a
custom contour for the latter.

## Compatibility and limits

- Tested GPUI revision: `175ef66578817bf96b2e26b0cd568dd4f4793529` plus the
  repository's backdrop patch; package versions `gpui-ce = 0.2.2`,
  `gpui_ce_wgpu = 0.1.0` (WGPU 29).
- Select `macos-wgpu` on `gpui_ce_platform` for the tested macOS configuration.
  The native Metal renderer and other backend paths have not been validated for
  this callback. There is no automatic glass fallback on unsupported renderers.
- Respects rectangular content masks and inherited opacity. Content-mask edge
  fades are not implemented.
- No framebuffer readback or forced continuous redraw in the library.
- Renderer hook names are isolated in `src/backend.rs`; optics stay in this crate.
  An upstream stable backdrop extension API could remove the local patch. Until
  then, changing GPUI revisions requires compatibility checks; Cargo cannot make
  an internal renderer change automatically safe.

This crate and its sibling `gpui-smooth` can be used as local path dependencies.
They have not been published to crates.io. Copy both sibling directories together
when relocating them. GPU/GPUI dependencies are selected by the consuming
workspace; the crates contain no paths to the demo or vendored GPUI tree.

## Reusable controls and JSON presets

`controls::GlassControls` is an embeddable GPUI entity. It contains only the material
controls, presets, shape selector, and effect/preview switches. The host supplies
its own container, padding, width, and scrolling. It does not render a sidebar.

```rust,no_run
use gpui::{App, AppContext, Entity};
use gpui_glass::{GlassParams, controls::GlassControls};
fn material_controls(cx: &mut App) -> Entity<GlassControls> {
    cx.new(|_| GlassControls::new(GlassParams::default()))
}
```

Subscribe to `controls::ControlsChanged` to receive the current `params`, `shape`,
`enabled`, and `lighting_preview`; update your material and call `cx.notify()`.
Embed the entity with `.child(controls.clone())`. Set `controls.dark` and notify the
entity to match a dark host. `preset()` returns `GlassPreset`; `set_preset()` applies
one and emits a change. The demo's theme, animation, sidebar, and reflection-target
widgets remain outside this component.

Save JSON / Load JSON open the platform's native file dialogs. Presets contain
`version: 1`, `params`, and `shape`; they preserve every material field and blend
mode. `GlassPreset::to_json()` / `from_json()` also work without a window. Missing
material fields inherit defaults. Unknown fields, unsupported versions, reversed
brightness ranges, oversized files, and values outside the controls' bounds are
rejected. Failed loads leave the active material intact and report an inline error.

Fog now has two independent endpoints: `fog_edge_opacity` at the rim and
`fog_opacity` in the interior, interpolated across `fog_edge`. A zero edge width
uses the interior opacity everywhere. Edge-only fog works with interior opacity
zero; both effective opacities zero bypass the fog pipeline. Frost and Fog sliders
reach 128 logical pixels, using additional dense reduction levels for large blur.

Reflections default to `LightBlend::Overlay`. `brightness_min` and `brightness_max`
remap the reflected RGB range (defaults 0.5 to 1.0), making dark sources subtle and
bright sources luminous. `dark_bias` attenuates reflections on bright surfaces.
Radiance support and contrast against the material are tested **before** this
remapping: no support, or a matching uniform environment, leaves the surface
unchanged in every blend mode, including SoftLight and Add.

`diffuse_radius` independently spreads reflected light inward, up to 2048 px in
the controls, without increasing the source sampling radius. The full prefiltered
radiance mip chain supports this broad glow; it does not enlarge source captures.
`sharp_brightness`, `soft_brightness`, and `inner_shadow_brightness` set each
legacy lighting layer's source gray before blending, so Add/Burn are tunable
colors rather than fixed white/black endpoints. For Native27, sharp/soft
brightness scales the vibrant mask gain; inner-shadow brightness remains the
ring's tint gray.

Exterior source contributions fade with a quintic falloff over the entire sampling
radius, avoiding a narrow boundary where approaching objects suddenly brighten.

Shadow reflections use the selected diffuse source hue and saturation to tint the existing shadow.
They use an independent tint alpha with the same layered profile and offset;
achromatic sources do not wash out the shadow. Black-shadow opacity does not
scale the tint. Reflection brightness bounds apply to surface reflections.

Reflections have independent `saturation` (0–2, default 1) and `vibrance`
(-1–1, default 0). Vibrance preferentially changes muted colors. Chroma is
compressed into display gamut while retaining hue; Soft Light/Overlay and the
other finish operators bound their inputs before applying contrast, including
transmission colors driven outside gamut by material saturation.

The transmitted material also exposes `brightness` (-1–1 additive offset, default
0), `contrast` (0–2 around mid-gray, default 1), and `vibrance` (-1–1, default 0).
Vibrance weights the chroma boost by the square of inverse source saturation, so
already saturated colors are largely unchanged. These adjustments happen after
refraction and before tint, surface reflections, and lighting; opaque reflection-only
sidebar colors remain host-controlled. All controls are included in JSON presets.

For inexpensive affine shape animation, keep the glass canvas bounds fixed and
animate `GlassParams::shape_scale` (default `[1., 1.]`, each axis clamped to
0.05–1). Coverage, distance, and normals are transformed together on the GPU;
the cached atlas and backing textures remain reusable. This transforms the
existing contour; it does not recompute a new Lisse capsule for each aspect ratio.
The local SDF metric is approximate away from the boundary under nonuniform scale.
See the root `PRISM.md` demo notes and `--dynamic` benchmark.

For **recalculated** built-in contours rather than affine scaling, set
`dynamic_shape = true`. Each changed `shape_scale` evaluates the shape generator
at the new dimensions, then rebuilds its SDF through a compute pass. The maximum
canvas allocation stays fixed. Lisse outlines are simplified with a 0.05 logical
pixel error bound before upload; weighted continuous normals avoid medial-axis
flips in this path. `GlassStats::dynamic_atlas_builds` exposes the actual work.
Custom contours continue to use their existing static path.

`GlassParams::tint_color` controls the material fill used by `opacity`. Hosts should
supply an appropriate dark/light neutral tint; both bundled demos do so on every
paint, independently of the chosen opacity.

Edge and diffuse intensity sliders each span **0–10×**. Their brightness bounds
are independent: `edge_brightness_min/max` for the edge, and `brightness_min/max`
for diffuse (the latter retain their original serialized names). The UI labels
them explicitly. `GlassPreset::from_json` migrates an older shared brightness
range to both layers when the new edge fields are absent.

### Splay and native 27 lens profile

`GlassParams::splay` now uses the recovered macOS 27 gradient-ovalization law:
blend the contour direction toward an aspect-corrected center ray, then normalize.
The aspect follows the actual morphed dimensions. Zero skips this work; changing
splay does not rebuild the SDF. The effect remains confined to the refraction band.
`profile = 1` uses the native circular displacement curve, which joins the flat
interior with zero displacement and zero slope. Linear and superellipse profiles
remain available. Lisse/custom silhouettes and continuous medial-axis directions
are deliberate adaptations, not Apple's analytic SDF. See the native splay/SDF
section in the root `RESEARCH.md` and `scripts/check-splay27.py` for equations,
evidence, and fidelity limits.

### Scrolling and idle work

Unchanged per-surface uniform blocks reuse their GPU contents; changing source
pixels still refreshes the backdrop normally. Native diffuse-only surfaces omit
the unused exterior reflection pyramid. Neither optimization caches stale scene
pixels or reduces blur quality. `GlassStats::uniform_uploads` and
`uniform_reuses` expose the upload decisions. The demo benchmark supports
`GLASS_PERF_MOTION=scroll` and `idle` (forced unchanged-content redraws), and
`scripts/check-scroll-pyramid.py` compares the optimized and reference-work paths
pixel-for-pixel. See `REFLECTION_PERFORMANCE.md` for measurements and limitations.
