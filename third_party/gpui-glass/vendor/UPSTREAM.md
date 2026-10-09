# GPUI-CE snapshot

Source: https://github.com/gpui-ce/gpui-ce
Revision: `175ef66578817bf96b2e26b0cd568dd4f4793529`
License: Apache-2.0, preserved in `gpui-ce/LICENSE.md` and individual crate notices.

This is a local source snapshot, not a published fork. `../patches/gpui-backdrop.patch`
records every change to upstream. The extension consists of a type-erased backdrop
payload, one paint method, a WGPU callback interface, and its dispatch at the existing
backdrop stage. All glass optics, shape generation, scratch textures, and UI live in
this application.

The callback also supplies frame/sequence identity and conservative damage since
the previous backdrop. Effects may report tighter output bounds through an
optional method; unknown effects invalidate their full content mask. This lets
applications reuse source filtering while preserving paint order. The WGPU device
enables optional SHADER_F16 only when supported; glass retains an f32 fallback.
The patch also includes headless timing/readback support used by the benchmarks.

The existing custom GPU surface API cannot sample already-painted GPUI text/images.
The callback is necessary to get the current scene color at the correct draw order.

Apply the patch to a fresh checkout of the pinned revision using `git apply`.
The nested checkout's original Git metadata is retained locally in the ignored
`artifacts/gpui-ce.git` directory, so the source snapshot can be committed normally.

The input engine in `src/editor.rs` is adapted from upstream
`crates/gpui/examples/view_example/example_editor.rs`, with the current TextRun
field, a demo placeholder, and unused multiline action removed.
