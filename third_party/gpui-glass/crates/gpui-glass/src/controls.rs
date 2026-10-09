//! Reusable material controls. The host owns padding, scrolling and the container.
use crate::{GlassParams, LightBlend, Shape};
use gpui::{prelude::*, *};
use serde::{Deserialize, Serialize};
use std::{cell::Cell, rc::Rc};
const GREEN: u32 = 0x396958;
const MUTED: u32 = 0x89978e;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlassPreset {
    pub version: u32,
    pub params: GlassParams,
    pub shape: Shape,
}
impl Default for GlassPreset {
    fn default() -> Self {
        Self {
            version: 1,
            params: GlassParams::default(),
            shape: Shape::Rounded,
        }
    }
}
impl GlassPreset {
    pub fn to_json(&self) -> anyhow::Result<String> {
        let json = serde_json::to_string_pretty(self)?;
        Self::from_json(&json)?;
        Ok(json)
    }
    pub fn from_json(json: &str) -> anyhow::Result<Self> {
        anyhow::ensure!(json.len() <= 65536, "Preset exceeds 64 KiB");
        let mut value: serde_json::Value = serde_json::from_str(json)?;
        if let Some(r) = value
            .pointer_mut("/params/reflection")
            .and_then(|v| v.as_object_mut())
        {
            for (old, new) in [
                ("brightness_min", "edge_brightness_min"),
                ("brightness_max", "edge_brightness_max"),
            ] {
                if !r.contains_key(new)
                    && let Some(v) = r.get(old).cloned()
                {
                    r.insert(new.into(), v);
                }
            }
        }
        let p: Self = serde_json::from_value(value)?;
        anyhow::ensure!(p.version == 1, "Unsupported preset version {}", p.version);
        let value = serde_json::to_value(&p)?;
        fn check(v: &serde_json::Value) -> bool {
            match v {
                serde_json::Value::Number(n) => n
                    .as_f64()
                    .is_some_and(|x| x.is_finite() && x.abs() <= 4096.),
                serde_json::Value::Null => false,
                serde_json::Value::Array(a) => a.iter().all(check),
                serde_json::Value::Object(o) => o
                    .iter()
                    .all(|(k, v)| k == "surface" && v.is_null() || check(v)),
                _ => true,
            }
        }
        anyhow::ensure!(
            check(&value),
            "Preset has invalid or excessive numeric values"
        );
        for (value, (label, min, max, _)) in GlassControls::new(p.params)
            .values()
            .into_iter()
            .zip(SLIDERS)
        {
            anyhow::ensure!(
                value.is_finite() && value >= min.min(0.) && value <= max,
                "Invalid {label}: {value}"
            );
        }
        anyhow::ensure!(
            p.params
                .surface
                .is_none_or(|rgb| rgb.into_iter().all(|v| (0.0..=1.0).contains(&v))),
            "Invalid surface color"
        );
        anyhow::ensure!(
            p.params.reflection.brightness_min <= p.params.reflection.brightness_max,
            "Brightness range is reversed"
        );
        anyhow::ensure!(
            p.params.reflection.edge_brightness_min <= p.params.reflection.edge_brightness_max,
            "Edge brightness range is reversed"
        );
        Ok(p)
    }
}
#[derive(Clone, Copy)]
pub struct ControlsChanged {
    pub params: GlassParams,
    pub shape: Shape,
    pub enabled: bool,
    pub lighting_preview: u8,
}
impl EventEmitter<ControlsChanged> for GlassControls {}
/// Embed this entity in any div; it supplies no sidebar, background, or scroll view.
pub struct GlassControls {
    pub params: GlassParams,
    pub shape: Shape,
    pub enabled: bool,
    pub lighting_preview: u8,
    pub dark: bool,
    sliders: Vec<Rc<Cell<Bounds<Pixels>>>>,
    dragging: Option<usize>,
    status: String,
}
const SLIDERS: [(&str, f32, f32, &str); 62] = [
    ("Frost", 0., 128., "px"),
    ("Tint opacity", 0., 0.6, "%"),
    ("Saturation", 0., 2., "×"),
    ("Refraction edge width", 2., 36., "px"),
    ("Refraction", -40., 40., "px"),
    ("Edge profile", 0., 2., ""),
    ("Dispersion", 0., 3., ""),
    ("Splay", 0., 1., ""),
    ("Light master", 0., 1.5, "×"),
    ("Corner smoothing", 0., 1., "%"),
    ("Fog blur", 0., 128., "px"),
    ("Fog edge width", 0., 100., "px"),
    ("Fog opacity", 0., 1., "%"),
    ("Dark outline opacity", 0., 1., "%"),
    ("Outline width", 0.25, 4., "px"),
    ("Sharp highlight opacity", 0., 1., "%"),
    ("Sharp highlight width", 0.5, 12., "px"),
    ("Soft highlight opacity", 0., 1., "%"),
    ("Soft highlight width", 1., 48., "px"),
    ("Inner shadow opacity", 0., 1., "%"),
    ("Inner shadow radius", 1., 64., "px"),
    ("Outer shadow opacity", 0., 1., "%"),
    ("Outer shadow radius", 1., 64., "px"),
    ("Edge intensity", 0., 10., "×"),
    ("Edge thickness", 0.5, 12., "px"),
    ("Diffuse intensity", 0., 10., "×"),
    ("Diffuse radius", 1., 2048., "px"),
    ("Sampling radius", 8., 240., "px"),
    ("Close-up sharpness", 0., 1., "%"),
    ("Sharp angle", 0., 360., "°"),
    ("Sharp inset", 0., 16., "px"),
    ("Sharp lobe shaping", 0., 32., "×"),
    ("Soft angle", 0., 360., "°"),
    ("Soft inset", 0., 40., "px"),
    ("Soft lobe shaping", 0., 32., "×"),
    ("Inner shadow angle", 0., 360., "°"),
    ("Inner shadow inset", 0., 40., "px"),
    ("Opposite highlight", 0., 1., "%"),
    ("Outer shadow angle", 0., 360., "°"),
    ("Extra shadow offset", 0., 32., "px"),
    ("Shadow reflection", 0., 2., "×"),
    ("Fog edge opacity", 0., 1., "%"),
    ("Diffuse brightness min", 0., 1., ""),
    ("Diffuse brightness max", 0., 2., ""),
    ("Dark surface bias", 0., 1., "%"),
    ("Sharp source brightness", 0., 1., "%"),
    ("Soft source brightness", 0., 1., "%"),
    ("Shadow source brightness", 0., 1., "%"),
    ("Reflection saturation", 0., 2., "×"),
    ("Reflection vibrance", -1., 1., "%"),
    ("Material brightness", -1., 1., "%"),
    ("Material contrast", 0., 2., "×"),
    ("Material vibrance", -1., 1., "%"),
    ("Edge brightness min", 0., 1., ""),
    ("Edge brightness max", 0., 2., ""),
    ("Reflection brightness", -1., 1., "%"),
    ("Reflection contrast", 0., 3., "×"),
    ("Size-based diffuse spread", 0., 1., "%"),
    ("Auto spread limit", 0., 1600., "px"),
    ("Sharp angular spread", -1., 0.99, ""),
    ("Soft angular spread", -1., 0.99, ""),
    ("Inner ring thickness", 0., 40., "px"),
];
impl GlassControls {
    pub fn new(params: GlassParams) -> Self {
        Self {
            params,
            shape: Shape::Rounded,
            enabled: true,
            lighting_preview: 0,
            dark: false,
            sliders: (0..SLIDERS.len())
                .map(|_| Rc::new(Cell::new(Bounds::default())))
                .collect(),
            dragging: None,
            status: String::new(),
        }
    }
    pub fn preset(&self) -> GlassPreset {
        GlassPreset {
            version: 1,
            params: self.params,
            shape: self.shape,
        }
    }
    pub fn set_preset(&mut self, preset: GlassPreset, cx: &mut Context<Self>) {
        self.params = preset.params;
        self.shape = preset.shape;
        self.changed(cx);
    }
    fn changed(&self, cx: &mut Context<Self>) {
        cx.emit(ControlsChanged {
            params: self.params,
            shape: self.shape,
            enabled: self.enabled,
            lighting_preview: self.lighting_preview,
        });
        cx.notify();
    }
    fn preset_button(&self, save: bool, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id(if save {
                "save-material"
            } else {
                "load-material"
            })
            .flex_1()
            .px_2()
            .py_2()
            .rounded_md()
            .cursor_pointer()
            .bg(rgb(if self.dark { 0x35403c } else { 0xe1e9dc }))
            .text_size(px(12.))
            .child(if save { "Save JSON…" } else { "Load JSON…" })
            .on_click(cx.listener(move |this, _, _, cx| this.file_prompt(save, cx)))
            .into_any_element()
    }
    fn file_prompt(&mut self, save: bool, cx: &mut Context<Self>) {
        let preset = self.preset();
        if save {
            let prompt = cx.prompt_for_new_path(
                &std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                    .join("Documents"),
                Some("glass-preset.json"),
            );
            cx.spawn(async move |this, cx| {
                let result: anyhow::Result<Option<String>> = async {
                    let Some(path) = prompt.await?? else {
                        return Ok(None);
                    };
                    let json = preset.to_json()?;
                    std::fs::write(&path, json)?;
                    Ok(Some(format!(
                        "Saved {}",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    )))
                }
                .await;
                let _ = this.update(cx, |this, cx| {
                    match result {
                        Ok(Some(message)) => this.status = message,
                        Err(e) => this.status = format!("Save failed: {e}"),
                        _ => {}
                    }
                    cx.notify();
                });
            })
            .detach();
        } else {
            let prompt = cx.prompt_for_paths(PathPromptOptions {
                files: true,
                directories: false,
                multiple: false,
                prompt: Some("Load glass preset".into()),
            });
            cx.spawn(async move |this, cx| {
                let result: anyhow::Result<Option<GlassPreset>> = async {
                    let Some(paths) = prompt.await?? else {
                        return Ok(None);
                    };
                    let Some(path) = paths.first() else {
                        return Ok(None);
                    };
                    anyhow::ensure!(
                        std::fs::metadata(path)?.len() <= 65536,
                        "Preset exceeds 64 KiB"
                    );
                    Ok(Some(GlassPreset::from_json(&std::fs::read_to_string(
                        path,
                    )?)?))
                }
                .await;
                let _ = this.update(cx, |this, cx| {
                    match result {
                        Ok(Some(p)) => {
                            this.set_preset(p, cx);
                            this.status = "Preset loaded".into()
                        }
                        Err(e) => this.status = format!("Load failed: {e}"),
                        _ => {}
                    }
                    cx.notify();
                });
            })
            .detach();
        }
    }
    fn values(&self) -> [f32; SLIDERS.len()] {
        let p = self.params;
        [
            p.blur,
            p.opacity,
            p.saturation,
            p.edge,
            p.distortion,
            p.profile,
            p.dispersion,
            p.splay,
            p.light,
            p.smoothing,
            p.fog,
            p.fog_edge,
            p.fog_opacity,
            p.lighting.outline_opacity,
            p.lighting.outline_width,
            p.lighting.sharp_opacity,
            p.lighting.sharp_width,
            p.lighting.soft_opacity,
            p.lighting.soft_width,
            p.lighting.inner_shadow_opacity,
            p.lighting.inner_shadow_radius,
            p.lighting.outer_shadow_opacity,
            p.lighting.outer_shadow_radius,
            p.reflection.edge_intensity,
            p.reflection.edge_width,
            p.reflection.diffuse_intensity,
            p.reflection.diffuse_radius,
            p.reflection.sampling_radius,
            p.reflection.sharpness,
            p.lighting.sharp_angle,
            p.lighting.sharp_inset,
            p.lighting.sharp_focus,
            p.lighting.soft_angle,
            p.lighting.soft_inset,
            p.lighting.soft_focus,
            p.lighting.inner_shadow_angle,
            p.lighting.inner_shadow_inset,
            p.lighting.opposite,
            p.lighting.outer_shadow_angle,
            p.lighting.outer_shadow_offset,
            p.reflection.shadow_intensity,
            p.fog_edge_opacity,
            p.reflection.brightness_min,
            p.reflection.brightness_max,
            p.reflection.dark_bias,
            p.lighting.sharp_brightness,
            p.lighting.soft_brightness,
            p.lighting.inner_shadow_brightness,
            p.reflection.saturation,
            p.reflection.vibrance,
            p.brightness,
            p.contrast,
            p.vibrance,
            p.reflection.edge_brightness_min,
            p.reflection.edge_brightness_max,
            p.reflection.brightness,
            p.reflection.contrast,
            p.reflection.diffuse_size_scaling,
            p.reflection.diffuse_radius_limit,
            p.lighting.sharp_spread,
            p.lighting.soft_spread,
            p.lighting.inner_shadow_width,
        ]
    }
    fn set_value(&mut self, index: usize, x: Pixels, cx: &mut Context<Self>) {
        let b = self.sliders[index].get();
        let t = ((x - b.origin.x) / b.size.width).clamp(0., 1.);
        let (_, min, max, _) = SLIDERS[index];
        let value = min + t * (max - min);
        let p = &mut self.params;
        match index {
            0 => p.blur = value,
            1 => p.opacity = value,
            2 => p.saturation = value,
            3 => p.edge = value,
            4 => p.distortion = value,
            5 => p.profile = value,
            6 => p.dispersion = value,
            7 => p.splay = value,
            8 => p.light = value,
            9 => p.smoothing = value,
            10 => p.fog = value,
            11 => p.fog_edge = value,
            12 => p.fog_opacity = value,
            13 => p.lighting.outline_opacity = value,
            14 => p.lighting.outline_width = value,
            15 => p.lighting.sharp_opacity = value,
            16 => p.lighting.sharp_width = value,
            17 => p.lighting.soft_opacity = value,
            18 => p.lighting.soft_width = value,
            19 => p.lighting.inner_shadow_opacity = value,
            20 => p.lighting.inner_shadow_radius = value,
            21 => p.lighting.outer_shadow_opacity = value,
            22 => p.lighting.outer_shadow_radius = value,
            23 => p.reflection.edge_intensity = value,
            24 => p.reflection.edge_width = value,
            25 => p.reflection.diffuse_intensity = value,
            26 => p.reflection.diffuse_radius = value,
            27 => p.reflection.sampling_radius = value,
            28 => p.reflection.sharpness = value,
            29 => p.lighting.sharp_angle = value,
            30 => p.lighting.sharp_inset = value,
            31 => p.lighting.sharp_focus = value,
            32 => p.lighting.soft_angle = value,
            33 => p.lighting.soft_inset = value,
            34 => p.lighting.soft_focus = value,
            35 => p.lighting.inner_shadow_angle = value,
            36 => p.lighting.inner_shadow_inset = value,
            37 => p.lighting.opposite = value,
            38 => p.lighting.outer_shadow_angle = value,
            39 => p.lighting.outer_shadow_offset = value,
            40 => p.reflection.shadow_intensity = value,
            41 => p.fog_edge_opacity = value,
            42 => {
                p.reflection.brightness_min = value;
                p.reflection.brightness_max = p.reflection.brightness_max.max(value);
            }
            43 => {
                p.reflection.brightness_max = value;
                p.reflection.brightness_min = p.reflection.brightness_min.min(value);
            }
            44 => p.reflection.dark_bias = value,
            45 => p.lighting.sharp_brightness = value,
            46 => p.lighting.soft_brightness = value,
            47 => p.lighting.inner_shadow_brightness = value,
            48 => p.reflection.saturation = value,
            49 => p.reflection.vibrance = value,
            50 => p.brightness = value,
            51 => p.contrast = value,
            52 => p.vibrance = value,
            53 => {
                p.reflection.edge_brightness_min = value;
                p.reflection.edge_brightness_max = p.reflection.edge_brightness_max.max(value);
            }
            54 => {
                p.reflection.edge_brightness_max = value;
                p.reflection.edge_brightness_min = p.reflection.edge_brightness_min.min(value);
            }
            55 => p.reflection.brightness = value,
            56 => p.reflection.contrast = value,
            57 => p.reflection.diffuse_size_scaling = value,
            58 => p.reflection.diffuse_radius_limit = value,
            59 => p.lighting.sharp_spread = value,
            60 => p.lighting.soft_spread = value,
            61 => p.lighting.inner_shadow_width = value,
            _ => {}
        }
        self.changed(cx);
    }
    fn slider(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let (label, min, max, unit) = SLIDERS[index];
        let label = match (index, self.params.reflection.native_diffuse) {
            (26, true) => "Diffuse blur radius",
            (27, true) => "Edge sampling radius",
            (57, true) => "Size-based diffuse blur",
            (58, true) => "Auto blur limit",
            _ => label,
        };
        let value = self.values()[index];
        let t = ((value - min) / (max - min)).clamp(0., 1.);
        let display = if unit == "%" {
            format!("{:.0}%", value * 100.)
        } else if index == 5 {
            if value < 0.5 {
                "Linear".into()
            } else if value < 1.5 {
                "Circular".into()
            } else {
                "Superellipse".into()
            }
        } else {
            format!("{value:.1}{unit}")
        };
        let bounds = self.sliders[index].clone();
        div()
            .flex()
            .flex_col()
            .gap(px(5.))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .text_size(px(12.))
                    .child(label)
                    .child(
                        div()
                            .text_color(rgb(MUTED))
                            .font_family("SF Mono")
                            .text_size(px(10.))
                            .child(display),
                    ),
            )
            .child(
                div()
                    .id(ElementId::Name(format!("slider-{index}").into()))
                    .h(px(18.))
                    .relative()
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            this.dragging = Some(index);
                            this.set_value(index, event.position.x, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .child(
                        canvas(move |b, _, _| bounds.set(b), |_, _, _, _| {})
                            .absolute()
                            .inset_0(),
                    )
                    .child(
                        div()
                            .absolute()
                            .top(px(7.))
                            .w_full()
                            .h(px(3.))
                            .rounded_full()
                            .bg(rgb(0xe1e6df)),
                    )
                    .child(
                        div()
                            .absolute()
                            .top(px(7.))
                            .w(relative(t))
                            .h(px(3.))
                            .rounded_full()
                            .bg(rgb(GREEN)),
                    )
                    .child(
                        div()
                            .absolute()
                            .top(px(1.))
                            .left(relative(t))
                            .ml(px(-7.))
                            .size(px(14.))
                            .rounded_full()
                            .bg(rgb(0xffffff))
                            .border_1()
                            .border_color(rgb(0x9daf9e))
                            .shadow_sm(),
                    ),
            )
            .into_any_element()
    }
    fn slider_section(
        &self,
        title: &'static str,
        indices: &[usize],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .mt(px(22.))
            .flex()
            .flex_col()
            .gap(px(7.))
            .child(div().mb(px(5.)).child(eyebrow(title)))
            .children(indices.iter().map(|&i| self.slider(i, cx)))
            .into_any_element()
    }
    fn blend_selector(&self, layer: u8, cx: &mut Context<Self>) -> AnyElement {
        let l = self.params.lighting;
        let (current, modes) = match layer {
            0 => (
                l.sharp_blend,
                [
                    LightBlend::Native27,
                    LightBlend::Screen,
                    LightBlend::SoftLight,
                    LightBlend::LinearDodge,
                ],
            ),
            1 => (
                l.soft_blend,
                [
                    LightBlend::Native27,
                    LightBlend::Screen,
                    LightBlend::SoftLight,
                    LightBlend::LinearDodge,
                ],
            ),
            _ => (
                l.inner_shadow_blend,
                [
                    LightBlend::Native27,
                    LightBlend::Multiply,
                    LightBlend::SoftLight,
                    LightBlend::LinearBurn,
                ],
            ),
        };
        div()
            .mt(px(8.))
            .flex()
            .gap_1()
            .children(modes.into_iter().map(|mode| {
                div()
                    .id(ElementId::Name(
                        format!("blend-{layer}-{}", mode as u32).into(),
                    ))
                    .flex_1()
                    .px(px(5.))
                    .py(px(6.))
                    .rounded(px(7.))
                    .text_size(px(10.))
                    .bg(rgb(if mode == current {
                        if self.dark { 0x435e4e } else { 0xdde8d8 }
                    } else {
                        if self.dark { 0x303833 } else { 0xebeee6 }
                    }))
                    .cursor_pointer()
                    .child(mode.label())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        match layer {
                            0 => this.params.lighting.sharp_blend = mode,
                            1 => this.params.lighting.soft_blend = mode,
                            _ => this.params.lighting.inner_shadow_blend = mode,
                        };
                        this.changed(cx);
                    }))
            }))
            .into_any_element()
    }
    fn reflection_blend_selector(&self, layer: u8, cx: &mut Context<Self>) -> AnyElement {
        let r = self.params.reflection;
        let current = match layer {
            0 => r.edge_blend,
            1 => r.diffuse_blend,
            _ => r.shadow_blend,
        };
        let label = match layer {
            0 => "EDGE BLEND",
            1 => "DIFFUSE BLEND",
            _ => "SHADOW REFLECTION BLEND",
        };
        div()
            .mt(px(12.))
            .child(eyebrow(label))
            .child(
                div().mt(px(6.)).flex().flex_wrap().gap_1().children(
                    [
                        LightBlend::Radiance,
                        LightBlend::Normal,
                        LightBlend::Screen,
                        LightBlend::SoftLight,
                        LightBlend::Overlay,
                        LightBlend::Multiply,
                        LightBlend::LinearDodge,
                    ]
                    .into_iter()
                    .map(|mode| {
                        div()
                            .id(ElementId::Name(
                                format!("reflection-blend-{layer}-{}", mode as u32).into(),
                            ))
                            .w(px(78.))
                            .py(px(6.))
                            .px(px(5.))
                            .rounded(px(7.))
                            .text_size(px(10.))
                            .bg(rgb(if mode == current {
                                if self.dark { 0x435e4e } else { 0xdde8d8 }
                            } else {
                                if self.dark { 0x303833 } else { 0xebeee6 }
                            }))
                            .cursor_pointer()
                            .child(mode.label())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                match layer {
                                    0 => this.params.reflection.edge_blend = mode,
                                    1 => this.params.reflection.diffuse_blend = mode,
                                    _ => this.params.reflection.shadow_blend = mode,
                                };
                                this.changed(cx);
                            }))
                    }),
                ),
            )
            .into_any_element()
    }
    fn lighting_preview_selector(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .mt(px(20.))
            .child(eyebrow("LIGHTING PREVIEW · STUDY LENS"))
            .child(
                div().mt(px(8.)).flex().gap_1().children(
                    ["Backdrop", "Light", "Dark"]
                        .into_iter()
                        .enumerate()
                        .map(|(i, label)| {
                            div()
                                .id(ElementId::Name(format!("lighting-preview-{i}").into()))
                                .flex_1()
                                .py(px(6.))
                                .px(px(8.))
                                .rounded(px(7.))
                                .text_size(px(10.))
                                .bg(rgb(if self.lighting_preview == i as u8 {
                                    if self.dark { 0x435e4e } else { 0xdde8d8 }
                                } else {
                                    if self.dark { 0x303833 } else { 0xebeee6 }
                                }))
                                .cursor_pointer()
                                .child(label)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.lighting_preview = i as u8;
                                    this.changed(cx);
                                }))
                        }),
                ),
            )
            .child(
                div()
                    .mt(px(6.))
                    .text_size(px(10.))
                    .text_color(rgb(MUTED))
                    .child("Angles: 0° up, 90° right. Inset moves the band inward."),
            )
            .into_any_element()
    }
}
impl Render for GlassControls {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("glass-controls")
            .flex()
            .flex_col()
            .text_color(rgb(if self.dark { 0xe2e9e4 } else { 0x202c2a }))
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if let Some(i) = this.dragging {
                    if event.dragging() {
                        this.set_value(i, event.position.x, cx);
                    } else {
                        this.dragging = None;
                    }
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.dragging = None),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(self.preset_button(true, cx))
                    .child(self.preset_button(false, cx)),
            )
            .child(div().mt_1().text_size(px(11.)).child(self.status.clone()))
            .child(
                div().mt(px(22.)).flex().gap_2().children(
                    [("Clear", 0), ("Frost", 1), ("Prism", 2)]
                        .into_iter()
                        .map(|(label, index)| {
                            div()
                                .id(ElementId::Name(format!("preset-{index}").into()))
                                .flex_1()
                                .h(px(32.))
                                .rounded(px(10.))
                                .rounded_smoothing(0.7)
                                .border_1()
                                .border_color(rgb(0xdce3d9))
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(11.))
                                .cursor_pointer()
                                .hover(|s| s.bg(rgb(0xe8eee4)))
                                .child(label)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.params = GlassParams::default();
                                    match index {
                                        0 => this.params.blur = 0.,
                                        1 => {
                                            this.params.blur = 12.;
                                            this.params.opacity = 0.2;
                                            this.params.distortion = 14.;
                                        }
                                        _ => {
                                            this.params.dispersion = 2.5;
                                            this.params.distortion = 32.;
                                            this.params.blur = 1.;
                                        }
                                    }
                                    this.changed(cx);
                                }))
                        }),
                ),
            )
            .child(self.slider_section("OPTICS", &[0, 1, 2, 50, 51, 52, 3, 4, 5, 6, 7, 9], cx))
            .child(self.slider_section("FOG", &[10, 11, 12, 41], cx))
            .child(self.lighting_preview_selector(cx))
            .child(self.slider_section("LIGHT / DARK OUTLINE", &[8, 13, 14, 37], cx))
            .child(self.slider_section("SHARP HIGHLIGHT", &[15, 16, 29, 30, 31, 59, 45], cx))
            .child(self.blend_selector(0, cx))
            .child(self.slider_section("SOFT HIGHLIGHT", &[17, 18, 32, 33, 34, 60, 46], cx))
            .child(self.blend_selector(1, cx))
            .child(self.slider_section("INNER SHADOW", &[19, 20, 35, 36, 61, 47], cx))
            .child(self.blend_selector(2, cx))
            .child(self.slider_section("OUTER SHADOW", &[21, 22, 38, 39], cx))
            .child(self.slider_section(
                "REFLECTIONS",
                &[
                    23, 24, 53, 54, 25, 26, 57, 58, 42, 43, 27, 28, 40, 44, 55, 56, 48, 49,
                ],
                cx,
            ))
            .child(
                div()
                    .id("diffuse-source-mode")
                    .mt_2()
                    .p_2()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(rgb(if self.dark { 0x435e4e } else { 0xc5cdc1 }))
                    .cursor_pointer()
                    .text_size(px(11.))
                    .child(if self.params.reflection.native_diffuse {
                        "Diffuse source: 27 backdrop"
                    } else {
                        "Diffuse source: exterior only"
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.params.reflection.native_diffuse =
                            !this.params.reflection.native_diffuse;
                        this.changed(cx);
                    })),
            )
            .child(self.reflection_blend_selector(0, cx))
            .child(self.reflection_blend_selector(1, cx))
            .child(self.reflection_blend_selector(2, cx))
            .child(div().mt(px(18.)).child(eyebrow("SHAPE STUDY")))
            .child(
                div()
                    .mt(px(10.))
                    .flex()
                    .gap_1()
                    .children(Shape::ALL.into_iter().map(|shape| {
                        div()
                            .id(ElementId::Name(format!("shape-{}", shape.name()).into()))
                            .flex_1()
                            .h(px(29.))
                            .rounded(px(8.))
                            .rounded_smoothing(0.7)
                            .bg(rgb(if shape == self.shape {
                                if self.dark { 0x435e4e } else { 0xe2ebde }
                            } else {
                                if self.dark { 0x303833 } else { 0xf0f3ec }
                            }))
                            .text_size(px(10.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .child(shape.name())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.shape = shape;
                                this.changed(cx);
                            }))
                    })),
            )
            .child(
                div()
                    .mt(px(16.))
                    .flex()
                    .justify_between()
                    .items_center()
                    .text_size(px(12.))
                    .child("Glass effect")
                    .child(
                        div()
                            .id("toggle-glass")
                            .w(px(34.))
                            .h(px(20.))
                            .p(px(3.))
                            .rounded_full()
                            .bg(rgb(if self.enabled { GREEN } else { 0xc5cdc1 }))
                            .cursor_pointer()
                            .flex()
                            .when(self.enabled, |d| d.justify_end())
                            .child(div().size(px(14.)).rounded_full().bg(rgb(0xffffff)))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.enabled = !this.enabled;
                                this.changed(cx);
                            })),
                    ),
            )
    }
}
fn eyebrow(label: &'static str) -> impl IntoElement {
    div()
        .text_size(px(10.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(MUTED))
        .child(label)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[::core::prelude::v1::test]
    fn presets_round_trip_all_material_fields_and_shape() {
        let mut preset = GlassPreset::default();
        preset.params.fog = 100.;
        preset.params.brightness = -0.2;
        preset.params.contrast = 1.3;
        preset.params.vibrance = 0.7;
        preset.params.reflection.saturation = 1.5;
        preset.params.reflection.vibrance = -0.3;
        preset.params.reflection.brightness = 0.15;
        preset.params.reflection.contrast = 1.4;
        preset.params.fog_edge_opacity = 0.4;
        preset.params.reflection.diffuse_radius = 1500.;
        preset.params.reflection.brightness_min = 0.4;
        preset.params.reflection.edge_brightness_min = 0.1;
        preset.params.reflection.edge_brightness_max = 1.6;
        preset.params.reflection.edge_intensity = 8.;
        preset.params.reflection.diffuse_intensity = 9.;
        preset.params.lighting.inner_shadow_brightness = 0.3;
        preset.shape = Shape::Flower;
        let encoded = preset.to_json().unwrap();
        assert_eq!(
            encoded,
            GlassPreset::from_json(&encoded).unwrap().to_json().unwrap()
        );
    }
    #[::core::prelude::v1::test]
    fn invalid_presets_do_not_parse_and_partial_materials_use_defaults() {
        assert!(GlassPreset::from_json(r#"{"version":2,"params":{},"shape":"rounded"}"#).is_err());
        assert!(
            GlassPreset::from_json(r#"{"version":1,"params":{"blur":10000},"shape":"rounded"}"#)
                .is_err()
        );
        assert!(
            GlassPreset::from_json(r#"{"version":1,"params":{"fog":null},"shape":"rounded"}"#)
                .is_err()
        );
        let legacy=GlassPreset::from_json(r#"{"version":1,"params":{"reflection":{"brightness_min":0.2,"brightness_max":0.8}},"shape":"capsule"}"#).unwrap();
        assert_eq!(legacy.params.reflection.edge_brightness_min, 0.2);
        assert_eq!(legacy.params.reflection.edge_brightness_max, 0.8);
        let p = GlassPreset::from_json(r#"{"version":1,"params":{},"shape":"rectangle"}"#).unwrap();
        assert_eq!(p.params.reflection.diffuse_blend, LightBlend::Radiance);
        assert_eq!(p.params.fog_edge_opacity, 0.);
    }
}
