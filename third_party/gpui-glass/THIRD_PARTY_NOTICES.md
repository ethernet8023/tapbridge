# Lisse capsule geometry

`crates/gpui-smooth/src/capsule.rs` and `rounded_rect.rs` port geometry from
[JaceThings/Lisse](https://github.com/JaceThings/Lisse), revision
`173846978ed806e14c78154ec914a9645a991932`:

- `packages/core/src/curves/capsule.ts`
- `packages/core/src/corner-params.ts`
- `packages/core/src/curves/blend.ts`
- Reference test anchors from `packages/core/__tests__/capsule.test.ts`

Copyright (c) 2026 Jace Attard. Licensed under the MIT license, reproduced in
[`LICENSES/Lisse.txt`](LICENSES/Lisse.txt).

The Rust port handles uniform rounded rectangles and fully rounded capsules. It retains Lisse's
flat-side smoothing budget, cubic shoulder controls, circular end arcs, and
near-square clamp. It flattens the curves to a cached distance/normal atlas
instead of emitting SVG, and also supports direct GPUI path painting. It does not import Lisse's other curve families or
per-corner distribution code.

The vendored GPUI-CE source retains its own license and third-party notices;
see `vendor/UPSTREAM.md` for its revision and local patch.

# GPUI Kit scroll bounce

`src/scroll_bounce.rs` is adapted from
[longbridge/gpui-kit](https://github.com/longbridge/gpui-kit), revision
`0790ad3876ebe6b72ca0bf599db7f7d1718c6b61`:

- `crates/base/src/scroll_bounce.rs` (behavior, spring physics, and tests)
- `crates/base/src/scrollbar.rs` (ScrollHandle and ListState adapters)
- `crates/base/src/event.rs` (native axis-lock call)
- `crates/base/src/reduce_motion.rs` (macOS system preference lookup at launch)

Copyright 2024–2026 Longbridge. Licensed under Apache-2.0; see
[`LICENSES/GPUI-Kit-Apache-2.0.txt`](LICENSES/GPUI-Kit-Apache-2.0.txt).

The kit version uses `gpui-pre`; this demo retains GPUI-CE and adapts the component
locally. Changes add GPUI-CE's content-mask defaults and `AppContext` import, use
the native axis-lock API, and include the two necessary scroll-handle adapters.
The spring/resistance behavior and its upstream interaction tests are retained.
No extra change to GPUI-CE or the reusable glass/smoothing crates is required.
