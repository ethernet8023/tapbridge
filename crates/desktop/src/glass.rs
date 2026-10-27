//! Liquid Glass surfaces for tapbridge's windows, built on `gpui-glass`
//! (refraction, frost and lighting over the already-painted scene).
//!
//! Glass only looks like glass over something to bend, so every window paints
//! [`backdrop`] first: the brand gradient plus a few soft colour orbs. Panels
//! and buttons painted on top refract it.
//!
//! Windows draws with GPUI's DirectX renderer, which can't run the glass
//! shader (a wgpu backdrop hook), so there the same surfaces fall back to a
//! plain frosted look: a native backdrop blur, the tint, and a hairline rim
//! standing in for the glass edge light.
#[cfg(not(windows))]
use gpui::canvas;
use gpui::{
    AnyElement, App, Div, Global, IntoElement, ParentElement, Styled, div, linear_color_stop,
    linear_gradient, px, rgb, rgba,
};
#[cfg(not(windows))]
use gpui_glass::{GlassContour, GlassDraw, Shape};
use gpui_glass::{GlassParams, GlassRenderer};
use std::sync::Arc;

/// One renderer for the whole app: it caches per-surface GPU state by ID.
struct SharedGlass(Arc<GlassRenderer>);
impl Global for SharedGlass {}

fn renderer(cx: &mut App) -> Arc<GlassRenderer> {
    if !cx.has_global::<SharedGlass>() {
        cx.set_global(SharedGlass(Arc::new(GlassRenderer::default())));
    }
    cx.global::<SharedGlass>().0.clone()
}

/// Stable surface ID from a string key; must be unique per visible surface.
#[cfg(not(windows))]
fn surface_id(key: &str) -> u64 {
    use std::{
        collections::hash_map::DefaultHasher,
        hash::{Hash, Hasher},
    };
    let mut hash = DefaultHasher::new();
    key.hash(&mut hash);
    hash.finish()
}

/// Dark, slightly lavender material that keeps light text readable.
fn panel_params() -> GlassParams {
    GlassParams {
        tint_color: [0.09, 0.07, 0.13],
        opacity: 0.42,
        blur: 10.0,
        edge: 12.0,
        distortion: 16.0,
        dispersion: 0.4,
        saturation: 1.35,
        light: 0.7,
        smoothing: 0.6,
        ..Default::default()
    }
}

/// Buttons are clearer and more refractive than panels.
fn button_params(tint: [f32; 3], opacity: f32) -> GlassParams {
    GlassParams {
        tint_color: tint,
        opacity,
        blur: 4.0,
        edge: 10.0,
        distortion: 20.0,
        dispersion: 0.6,
        ..panel_params()
    }
}

fn rgb_f(hex: u32) -> [f32; 3] {
    [
        ((hex >> 16) & 0xFF) as f32 / 255.0,
        ((hex >> 8) & 0xFF) as f32 / 255.0,
        (hex & 0xFF) as f32 / 255.0,
    ]
}

/// The edge light's stand-in on Windows: a white hairline.
#[cfg(windows)]
const RIM: u32 = 0xFFFFFF2E;

/// Windows: a frosted surface with the glass's tint, blur and a rim, since
/// DirectX can't run the glass shader. Same shape and placement as the glass.
#[cfg(windows)]
fn frost(params: &GlassParams) -> Div {
    let [r, g, b] = params.tint_color;
    div()
        .absolute()
        .inset_0()
        .backdrop_blur(px(params.blur))
        .bg(gpui::Rgba::new(r, g, b, params.opacity))
}

#[cfg(windows)]
fn layer(
    _renderer: Arc<GlassRenderer>,
    _key: &str,
    params: GlassParams,
    radius: Option<f32>,
) -> AnyElement {
    let surface = frost(&params).border_1().border_color(rgba(RIM));
    match radius {
        None => surface.rounded_full(),
        Some(radius) => surface.rounded(px(radius)),
    }
    .into_any_element()
}

/// Glass behind whatever is laid out in the parent: a squircle with `radius`,
/// or a capsule. Absolute and full-size, so add it as the parent's first child.
#[cfg(not(windows))]
fn layer(
    renderer: Arc<GlassRenderer>,
    key: &str,
    params: GlassParams,
    radius: Option<f32>,
) -> AnyElement {
    let id = surface_id(key);
    // Build each surface's distance field on the GPU: the CPU path stalls
    // the main thread for ~0.5 s per large surface (dialogs, panels).
    let params = GlassParams {
        dynamic_shape: true,
        ..params
    };
    match radius {
        None => gpui_glass::glass_layer(renderer, id, params, Shape::Capsule)
            .absolute()
            .inset_0()
            .into_any_element(),
        Some(radius) => canvas(
            |_, _, _| {},
            move |bounds, (), window, _| {
                let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
                if w < 1.0 || h < 1.0 {
                    return;
                }
                let points = gpui_smooth::SmoothShape::rounded_rect(radius, params.smoothing)
                    .outline(w, h)
                    .into_iter()
                    .map(|[x, y]| [(x / w).clamp(0.0, 1.0), (y / h).clamp(0.0, 1.0)])
                    .collect();
                let Ok(contour) = GlassContour::new(points) else {
                    return;
                };
                GlassDraw {
                    renderer: renderer.clone(),
                    id,
                    params,
                    shape: Shape::Rectangle,
                    scale: window.scale_factor(),
                    contour: Some(Arc::new(contour)),
                }
                .paint_in(window, bounds);
            },
        )
        .absolute()
        .inset_0()
        .into_any_element(),
    }
}

/// A rounded glass panel. Lay it out like any div; its children sit on the glass.
pub fn panel(key: &str, radius: f32, cx: &mut App) -> Div {
    div().relative().rounded(px(radius)).child(layer(
        renderer(cx),
        key,
        panel_params(),
        Some(radius),
    ))
}

/// Dialog material: heavier frost than a panel, so the window behind
/// blurs away and the dialog reads as a sheet above it.
fn dialog_params() -> GlassParams {
    GlassParams {
        tint_color: [0.08, 0.06, 0.12],
        opacity: 0.55,
        blur: 28.0,
        edge: 14.0,
        distortion: 10.0,
        ..panel_params()
    }
}

/// A frosted glass dialog surface (see [`panel`]).
pub fn dialog(key: &str, radius: f32, cx: &mut App) -> Div {
    div().relative().rounded(px(radius)).child(layer(
        renderer(cx),
        key,
        dialog_params(),
        Some(radius),
    ))
}

fn scrim_params() -> GlassParams {
    GlassParams {
        tint_color: [0.04, 0.03, 0.07],
        opacity: 0.35,
        blur: 18.0,
        edge: 0.0,
        distortion: 0.0,
        dispersion: 0.0,
        light: 0.0,
        ..panel_params()
    }
}

/// Full-window glass scrim under a dialog: blurs and dims the window.
/// Absolute and full-size.
#[cfg(not(windows))]
pub fn scrim(key: &str, cx: &mut App) -> Div {
    div().absolute().inset_0().child(
        gpui_glass::glass_layer(
            renderer(cx),
            surface_id(key),
            GlassParams {
                dynamic_shape: true,
                ..scrim_params()
            },
            Shape::Rectangle,
        )
        .absolute()
        .inset_0(),
    )
}

/// Full-window frosted scrim under a dialog (no rim: it has no edge).
#[cfg(windows)]
pub fn scrim(_key: &str, _cx: &mut App) -> Div {
    div().absolute().inset_0().child(frost(&scrim_params()))
}

/// A capsule glass button tinted `tint` (a 0xRRGGBB brand colour).
pub fn capsule(key: &str, tint: u32, opacity: f32, cx: &mut App) -> Div {
    div().relative().rounded_full().child(layer(
        renderer(cx),
        key,
        button_params(rgb_f(tint), opacity),
        None,
    ))
}

/// The window background that glass refracts: brand gradient plus soft orbs.
/// Absolute and full-size; add it as the window root's first child.
pub fn backdrop() -> Div {
    let orb = |left: f32, top: f32, size: f32, from: u32, to: u32| {
        div()
            .absolute()
            .left(px(left))
            .top(px(top))
            .size(px(size))
            .rounded_full()
            .bg(linear_gradient(
                160.0,
                linear_color_stop(rgba(from), 0.0),
                linear_color_stop(rgba(to), 1.0),
            ))
    };
    div()
        .absolute()
        .inset_0()
        .overflow_hidden()
        .bg(linear_gradient(
            165.0,
            linear_color_stop(rgb(0x1F1533), 0.0),
            linear_color_stop(rgb(0x0E0B16), 1.0),
        ))
        .child(orb(-90.0, -70.0, 300.0, 0x7C5CFFCC, 0x7C5CFF00))
        .child(orb(260.0, 120.0, 260.0, 0xC4B5FF66, 0x3DDC9700))
        .child(orb(-40.0, 340.0, 240.0, 0x3DDC9755, 0x7C5CFF00))
        .child(orb(300.0, 520.0, 320.0, 0xFF6FB155, 0x7C5CFF00))
}
