# Lisse capsule geometry

`src/capsule.rs` and `src/rounded_rect.rs` port geometry from
[JaceThings/Lisse](https://github.com/JaceThings/Lisse), revision
`173846978ed806e14c78154ec914a9645a991932`:

- `packages/core/src/curves/capsule.ts`
- `packages/core/src/corner-params.ts`
- `packages/core/src/curves/blend.ts`
- Reference test anchors from `packages/core/__tests__/capsule.test.ts`

Copyright (c) 2026 Jace Attard. Licensed under the MIT license, reproduced in
[`LICENSE`](LICENSE).

The Rust port handles uniform rounded rectangles and fully rounded capsules. It retains Lisse's
flat-side smoothing budget, cubic shoulder controls, circular end arcs, and
near-square clamp. It flattens the curves for painting or distance/normal atlases
instead of emitting SVG. It does not import Lisse's other curve families or
per-corner distribution code.

The optional GPUI-CE dependency retains its own license and third-party notices.
