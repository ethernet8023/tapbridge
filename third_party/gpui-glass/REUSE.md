# Reusing the two crates

The demo is now a consumer of two standalone packages:

- [`gpui-smooth`](crates/gpui-smooth/README.md): dependency-free capsule and uniform
  rounded-rectangle geometry, plus an optional public-API GPUI paint adapter.
- [`gpui-glass`](crates/gpui-glass/README.md): the glass renderer, its embedded
  shader, cache, contour atlas, and GPUI container/layer helpers. It depends on
  `gpui-smooth` for capsule geometry, without enabling its GPUI adapter.

Each directory has its own manifest, documentation, tests, and license. There are
no inherited workspace fields or paths back into the demo. Copy both directories
as siblings when using glass. They have not been published to crates.io.

## Shapes without glass

```toml
[dependencies]
gpui-smooth = { path = "../gpui-glass/crates/gpui-smooth" }
```

This gives pure Rust geometry and has no normal dependencies. Enable
`features = ["gpui"]` for `smooth`, `shape_layer`, and `ShapeStyle`; that adapter
works with public, unpatched GPUI-CE. See the crate README for examples and a
styling support table. This is the capsule and uniform squircle subset of Lisse,
not a port of all its curve families and per-corner options.

## Glass with the existing tested source

In the **consuming application's root** Cargo.toml (adjust the relative paths):

```toml
[dependencies]
gpui = { package = "gpui-ce", version = "=0.2.2", default-features = false }
gpui_platform = { package = "gpui_ce_platform", version = "=0.1.0", features = ["font-kit", "macos-wgpu"] }
gpui-glass = { path = "../gpui-glass/crates/gpui-glass" }
gpui-smooth = { path = "../gpui-glass/crates/gpui-smooth", features = ["gpui"] }

[patch.crates-io]
gpui-ce = { path = "../gpui-glass/vendor/gpui-ce/crates/gpui" }
gpui_ce_wgpu = { path = "../gpui-glass/vendor/gpui-ce/crates/gpui_wgpu" }
gpui_ce_platform = { path = "../gpui-glass/vendor/gpui-ce/crates/gpui_platform" }
```

These declarations select a single coherent GPUI source graph for the application
and both libraries. Do not mix a git-sourced GPUI type graph with crates.io/path
versions of the same crates. If your project currently depends on GPUI through
Git, switch its GPUI declarations consistently, or override that Git source too.

Keep the vendored GPUI tree outside an explicit consuming Cargo workspace's
member tree (a sibling checkout is simplest). GPUI has its own workspace and
inherits its dependencies and package metadata there. This demo keeps the three
packages independent rather than enrolling GPUI's nested workspace.

`[patch]` **selects a replacement crate source**; it does not apply a `.patch` file.
Cargo reads overrides only from the root application/workspace, so putting them
inside `gpui-glass` would not configure downstream users. This follows Cargo's
[dependency override rules](https://doc.rust-lang.org/cargo/reference/overriding-dependencies.html#the-patch-section).

## Material controls and reflecting panels

`GlassParams` exposes `LightParams` and `ReflectionParams` independently. Light has
outline, sharp and soft highlight, inner-shadow, and outer-shadow opacities, each
paired with a width or radius in logical pixels. Set any opacity to zero to skip
that layer; `params.light = 0.0` skips all five. `LightParams::disabled()` gives an
all-zero-opacity configuration while preserving useful dimensions.

The dark outline is outside the contour, over the two-layer outer shadow. Highlights
default to top/bottom alignment. Their independent angle, inset, focus, opposite-lobe
weight, and `LightBlend` controls also work on custom contours. The inner shadow has
its own angle/inset/blend; the outer shadow has an angle and extra offset, added to
its radius-relative downward translation. See the crate README for field names.

Frost (`blur`) filters the backdrop uniformly before refraction. Fog has separate
`fog` (additional blur sigma), `fog_edge` (rim fade width), `fog_opacity` (interior), and `fog_edge_opacity` (rim) values.
Its width does not track the refraction band. A zero fog blur or both effective opacity endpoints zero omits its
blur pipeline; `fog_edge = 0.0` removes the rim fade.

Reflections have independent edge intensity/width and diffuse intensity/radius,
plus an exterior sampling radius and close-up sharpness. `shadow_intensity` adds
diffuse exterior color to an enabled outer shadow. `edge_blend`, `diffuse_blend`,
and `shadow_blend` independently select blend operators. `ReflectionParams::disabled()`
turns all three contributions off; no reflection textures or filtering passes are needed
in that configuration. Reflection sources are masked outside the contour before
prefiltering. Edge detail becomes softer with source distance; diffuse reflection
starts at the rim and fades inward using a spatial blur, avoiding nearest-edge
projection seams. This is a screen-space
approximation of the supplied visual references, not Apple's Liquid Glass code or
physical ray tracing. Only visible scene content painted before the effect is
available: offscreen objects and later children cannot reflect.

Use a solid reflecting surface when there is no useful backdrop to refract:

```rust,ignore
let panel = gpui_glass::glass_layer(
    shared_renderer.clone(),
    6, // stable, unique surface ID
    params.reflection_only([0.98, 0.984, 0.969]),
    gpui_glass::Shape::Rectangle,
)
.absolute()
.inset_0();
```

`reflection_only([r, g, b])` preserves reflection settings, supplies an opaque RGB
base, and disables transmission/refraction, fog, frost, dispersion, and light.
Paint the panel after the neighboring content it should reflect and place its
foreground controls afterwards. The demo inspector follows this pattern, with a
rectangular, fixed reflection layer behind independently scrolling controls. Its **Reflection
targets** toggle and **Animate backdrop** toggle provide moving exterior colors for
manual inspection.

Keep one `Arc<GlassRenderer>` alive for the relevant view or application. Its cache
uses a soft retention threshold of 128 surface IDs: inserting an ID may evict one
least-recently-used entry only if that entry has been idle for more than two seconds.
Active working sets can exceed 128 without repeated rebuilding. The capture extent
retains its warmed maximum, so tuning sampling radius down and back up within that
maximum does not reallocate an otherwise unchanged surface. There is no fixed
texture-memory budget. Use `renderer.clear_cache()` to explicitly release cached
surfaces, for example after closing a document; pipelines and counters are retained.

`renderer.stats()` reports cached surface count and cumulative allocations, atlas
uploads, captures, blur passes, and reflection passes. These are encoded-work counters,
not timings. `reflection_passes` counts one compute pass per reflecting surface;
`reflection.wgsl` generates the full radiance mip chain, four levels per dispatch
inside that pass. Individual levels do not increment the pass counter. All three
reflection intensities at zero omit the capture/filtering resources and compute pass.
Shadow reflection alone also schedules no work when the outer shadow is disabled.

For custom contours painted from a GPUI paint callback, use
`GlassDraw::paint_in(window, bounds)`. It registers the expanded shadow/outline extent
for scene ordering and culling while preserving the original contour bounds and
reading the current window scale. The lower-level `effect()` payload remains available,
but its caller must handle full shadow/outline registration and shape coordinates itself.

## Preparing another checkout

To use your own pristine GPUI checkout, pin it to
`175ef66578817bf96b2e26b0cd568dd4f4793529`, then run:

```sh
python3 scripts/gpui-compat.py --apply /path/to/gpui-ce
python3 scripts/gpui-compat.py --check /path/to/gpui-ce
```

The explicit setup script checks the compiled-source/manifests fingerprint,
applies the existing five-file hook patch only to the expected upstream tree,
and verifies the result. Applying it again is a no-op. It refuses modified or
newer source instead of guessing how to merge it. It never edits Cargo's cache.
The fingerprint excludes build output, lockfiles, and documentation; it is a
version guard, not proof that arbitrary backend/platform combinations work.

## Updating GPUI

There is **no declaration that makes an internal renderer patch update-proof**.
The current integration is reproducible because the source is pinned, not because
it tolerates arbitrary future versions. A stable upstream custom-backdrop hook
would remove the need for this patch; a public custom GPU surface alone cannot
sample already-painted GPUI content at the correct point in scene order.

For an intentional upgrade:

1. Review/rebase `patches/gpui-backdrop.patch` against the new GPUI source.
2. Adapt `crates/gpui-glass/src/backend.rs` if the hook API changes; review WGPU
   changes in the renderer as needed.
3. Run `scripts/check-crates.sh` and the release renderer's optical checks.
4. Update the exact dependency versions, lockfiles, and compatibility manifest
   only after validating the new source. Changing the manifest checksum alone is
   not a compatibility fix.

If maintaining a fork later, Cargo can select an immutable Git `rev` instead of a
path. The application must still make the override, and upgrades remain explicit.
No public fork, upstream PR, or registry publication was created by this split.

## Checks

```sh
./scripts/check-crates.sh
python3 scripts/check-reuse.py
cargo run --release --features bench --bin glass-bench -- --verify
```

The shell script tests each standalone manifest and its documentation examples,
runs Clippy, and verifies the pinned hook. The reuse smoke test copies the crates
into a temporary project, checks geometry without GPUI, compiles the adapter with
unpatched GPUI, checks explicit patch setup and rejection of changed source, and
compiles a separate glass consumer. It runs offline after dependencies have been
fetched. The final command renders the optical regressions on an available native
GPU. These checks do not validate signed distribution.
