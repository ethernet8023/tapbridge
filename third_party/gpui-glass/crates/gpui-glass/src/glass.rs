use crate::backend::{BackdropContext, WgpuBackdrop, WgpuBackdropEffect};
use crate::shape::{self, Shape};
use gpui_wgpu::wgpu;
use serde::{Deserialize, Serialize};
use std::hash::{Hash, Hasher};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use wgpu::util::DeviceExt;

/// A closed outline in normalized [0,1] coordinates. Concave outlines are supported.
/// Curves can be flattened to this representation once by the caller.
pub struct GlassContour {
    points: Vec<[f32; 2]>,
    key: u64,
}
impl GlassContour {
    pub fn new(points: Vec<[f32; 2]>) -> anyhow::Result<Self> {
        anyhow::ensure!(
            points.len() >= 3
                && points
                    .iter()
                    .flatten()
                    .all(|x| x.is_finite() && (0.0..=1.0).contains(x)),
            "a contour needs at least 3 finite normalized vertices"
        );
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        for p in &points {
            p[0].to_bits().hash(&mut hash);
            p[1].to_bits().hash(&mut hash);
        }
        Ok(Self {
            points,
            key: hash.finish(),
        })
    }
}

/// Blend operators for the directional finish. Defaults use Screen for the
/// narrow glint, SoftLight for the broad shoulder, and Multiply for shadows.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LightBlend {
    #[default]
    Screen = 0,
    SoftLight = 1,
    LinearDodge = 2,
    Multiply = 3,
    LinearBurn = 4,
    Normal = 5,
    Overlay = 6,
    /// Screen-like radiance on dark surfaces, bounded chromatic tint on light ones.
    Radiance = 7,
    /// Reconstructed macOS 27 key/fill distance lobes and material color bias.
    Native27 = 8,
}
impl LightBlend {
    pub fn label(self) -> &'static str {
        match self {
            Self::Screen => "Screen",
            Self::SoftLight => "Soft light",
            Self::LinearDodge => "Add",
            Self::Multiply => "Multiply",
            Self::LinearBurn => "Burn",
            Self::Normal => "Normal",
            Self::Overlay => "Overlay",
            Self::Radiance => "Radiance",
            Self::Native27 => "27 key/fill",
        }
    }
}

/// Independent lighting layers. Widths and radii are logical pixels.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LightParams {
    pub outline_opacity: f32,
    pub outline_width: f32,
    pub sharp_opacity: f32,
    pub sharp_width: f32,
    pub soft_opacity: f32,
    pub soft_width: f32,
    pub inner_shadow_opacity: f32,
    pub inner_shadow_radius: f32,
    pub inner_shadow_width: f32,
    pub outer_shadow_opacity: f32,
    pub outer_shadow_radius: f32,
    /// Degrees clockwise from the upward screen direction.
    pub sharp_angle: f32,
    /// Highlight band center measured inward from the contour, in logical pixels.
    pub sharp_inset: f32,
    pub sharp_focus: f32,
    pub sharp_spread: f32,
    pub sharp_blend: LightBlend,
    pub sharp_brightness: f32,
    pub soft_angle: f32,
    pub soft_inset: f32,
    pub soft_focus: f32,
    pub soft_spread: f32,
    pub soft_blend: LightBlend,
    pub soft_brightness: f32,
    pub inner_shadow_angle: f32,
    pub inner_shadow_inset: f32,
    pub inner_shadow_blend: LightBlend,
    pub inner_shadow_brightness: f32,
    pub opposite: f32,
    pub outer_shadow_angle: f32,
    pub outer_shadow_offset: f32,
}
impl Default for LightParams {
    fn default() -> Self {
        Self {
            outline_opacity: 0.1875,
            outline_width: 1.,
            sharp_opacity: 1.,
            sharp_width: 1.,
            soft_opacity: 1.,
            soft_width: 8.,
            inner_shadow_opacity: 0.12,
            inner_shadow_radius: 5.,
            inner_shadow_width: 2.,
            outer_shadow_opacity: 0.07,
            outer_shadow_radius: 4.,
            sharp_angle: 0.,
            sharp_inset: 0.,
            sharp_focus: 0.,
            sharp_spread: 0.17364818,
            sharp_blend: LightBlend::Native27,
            sharp_brightness: 1.,
            soft_angle: 0.,
            soft_inset: 0.,
            soft_focus: 11.333333,
            soft_spread: 0.6156615,
            soft_blend: LightBlend::Native27,
            soft_brightness: 1.,
            inner_shadow_angle: 0.,
            inner_shadow_inset: 3.,
            inner_shadow_blend: LightBlend::Native27,
            inner_shadow_brightness: 0.,
            opposite: 1.,
            outer_shadow_angle: 180.,
            outer_shadow_offset: 1.4,
        }
    }
}
impl LightParams {
    pub fn disabled() -> Self {
        Self {
            outline_opacity: 0.,
            sharp_opacity: 0.,
            soft_opacity: 0.,
            inner_shadow_opacity: 0.,
            outer_shadow_opacity: 0.,
            ..Self::default()
        }
    }
}

/// Screen-space reflections: exterior-only edges and a native backdrop diffuse path.
/// Near edge sources remain sharp; diffuse blur is independently controlled.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReflectionParams {
    pub edge_intensity: f32,
    pub edge_width: f32,
    pub diffuse_intensity: f32,
    pub diffuse_radius: f32,
    /// Native backdrop + blur pyramid. False preserves the exterior-only experiment.
    pub native_diffuse: bool,
    /// Grow the diffuse radius with surface area; zero retains the explicit radius.
    pub diffuse_size_scaling: f32,
    /// Ceiling for automatic growth; an explicitly larger radius is preserved.
    pub diffuse_radius_limit: f32,
    pub sampling_radius: f32,
    pub sharpness: f32,
    pub edge_blend: LightBlend,
    pub diffuse_blend: LightBlend,
    pub shadow_blend: LightBlend,
    pub shadow_intensity: f32,
    pub saturation: f32,
    pub vibrance: f32,
    pub brightness: f32,
    pub contrast: f32,
    pub edge_brightness_min: f32,
    pub edge_brightness_max: f32,
    /// Diffuse reflection brightness range (retained names for preset compatibility).
    pub brightness_min: f32,
    pub brightness_max: f32,
    pub dark_bias: f32,
}
impl Default for ReflectionParams {
    fn default() -> Self {
        Self {
            edge_intensity: 0.8,
            edge_width: 3.,
            diffuse_intensity: 0.3,
            diffuse_radius: 34.,
            native_diffuse: true,
            diffuse_size_scaling: 0.7,
            diffuse_radius_limit: 240.,
            sampling_radius: 100.,
            sharpness: 0.75,
            edge_blend: LightBlend::Radiance,
            diffuse_blend: LightBlend::Radiance,
            shadow_blend: LightBlend::Overlay,
            shadow_intensity: 0.4,
            saturation: 1.,
            vibrance: 0.,
            brightness: 0.,
            contrast: 1.,
            edge_brightness_min: 0.,
            edge_brightness_max: 1.,
            brightness_min: 0.,
            brightness_max: 1.,
            dark_bias: 0.65,
        }
    }
}
impl ReflectionParams {
    /// Resolve size-aware spread in logical pixels without changing sampling reach.
    pub fn diffuse_radius_for_size(self, width: f32, height: f32) -> f32 {
        let base = self.diffuse_radius.max(0.);
        let growth = ((width.max(0.) * height.max(0.)).sqrt() / 96. - 1.).max(0.);
        let scaled = base * (1. + self.diffuse_size_scaling.clamp(0., 1.) * growth);
        scaled.min(base.max(self.diffuse_radius_limit.max(0.)))
    }
    pub fn disabled() -> Self {
        Self {
            edge_intensity: 0.,
            diffuse_intensity: 0.,
            shadow_intensity: 0.,
            ..Self::default()
        }
    }
    fn enabled(self) -> bool {
        self.sampling_radius > 0.
            && ((self.edge_intensity > 0. && self.edge_width > 0.)
                || (self.diffuse_intensity > 0. && self.diffuse_radius > 0.))
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GlassParams {
    pub blur: f32,
    pub fog: f32,
    pub fog_edge: f32,
    pub fog_opacity: f32,
    pub fog_edge_opacity: f32,
    pub opacity: f32,
    pub tint_color: [f32; 3],
    pub brightness: f32,
    pub contrast: f32,
    pub vibrance: f32,
    pub saturation: f32,
    pub edge: f32,
    pub distortion: f32,
    pub profile: f32,
    pub dispersion: f32,
    /// Native 27 gradient ovalization: blends the contour direction toward an
    /// aspect-corrected center ray within the refraction band. Zero disables it.
    pub splay: f32,
    pub light: f32,
    pub lighting: LightParams,
    pub reflection: ReflectionParams,
    /// Opaque RGB base instead of backdrop refraction, useful for reflecting sidebars.
    pub surface: Option<[f32; 3]>,
    pub smoothing: f32,
    /// Affine contour scale within fixed bounds; animated without atlas regeneration.
    pub shape_scale: [f32; 2],
    /// Re-evaluate the built-in contour at shape_scale dimensions and rebuild its SDF on GPU.
    pub dynamic_shape: bool,
}
impl Default for GlassParams {
    fn default() -> Self {
        Self {
            blur: 3.,
            fog: 0.,
            fog_edge: 14.,
            fog_opacity: 1.,
            fog_edge_opacity: 0.,
            opacity: 0.07,
            tint_color: [0.97; 3],
            brightness: 0.,
            contrast: 1.,
            vibrance: 0.,
            saturation: 1.2,
            edge: 14.,
            distortion: 22.,
            profile: 1.,
            dispersion: 0.65,
            splay: 0.12,
            light: 0.65,
            lighting: LightParams::default(),
            reflection: ReflectionParams::default(),
            surface: None,
            smoothing: 0.65,
            shape_scale: [1., 1.],
            dynamic_shape: false,
        }
    }
}
impl GlassParams {
    /// Preserve reflection settings while disabling all transmission and lighting.
    pub fn reflection_only(mut self, color: [f32; 3]) -> Self {
        self.surface = Some(color);
        self.blur = 0.;
        self.fog = 0.;
        self.light = 0.;
        self.distortion = 0.;
        self.dispersion = 0.;
        self.opacity = 0.;
        self.saturation = 1.;
        self
    }
    pub fn shadow_outset(self) -> f32 {
        if self.light > 0.
            && (self.lighting.outer_shadow_opacity > 0.
                || (self.reflection.shadow_intensity > 0. && self.reflection.sampling_radius > 0.))
        {
            (self.lighting.outer_shadow_radius.clamp(0.5, 64.) * 3. + self.shadow_translation())
                .ceil()
        } else {
            0.
        }
    }
    fn shadow_translation(self) -> f32 {
        self.lighting.outer_shadow_radius.clamp(0.5, 64.) * 0.65
            + self.lighting.outer_shadow_offset.abs().min(64.)
    }
    /// Extent of the shadow and the exterior dark outline together.
    pub fn paint_outset(self) -> f32 {
        let outline = if self.light > 0. && self.lighting.outline_opacity > 0. {
            self.lighting.outline_width.max(0.) + 1.
        } else {
            0.
        };
        self.shadow_outset().max(outline)
    }
    fn reflections_enabled(self) -> bool {
        self.reflection.enabled()
            || (self.reflection.sampling_radius > 0.
                && self.reflection.shadow_intensity > 0.
                && self.shadow_outset() > 0.)
    }
    fn fog_enabled(self) -> bool {
        self.surface.is_none()
            && self.fog >= 0.05
            && (self.fog_opacity > 0. || (self.fog_edge > 0. && self.fog_edge_opacity > 0.))
    }
}
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    viewport: [f32; 4],
    bounds: [f32; 4],
    crop: [f32; 4],
    transmission_crop: [f32; 4],
    bleed_crop: [f32; 4],
    bleed_damage: [f32; 4],
    optics: [f32; 4],
    lens: [f32; 4],
    finish: [f32; 4],
    fog_params: [f32; 4],
    outline_sharp: [f32; 4],
    soft_inner: [f32; 4],
    outer: [f32; 4],
    reflection: [f32; 4],
    reflection_info: [f32; 4],
    reflection_modes: [f32; 4],
    reflection_tone: [f32; 4],
    reflection_edge_tone: [f32; 4],
    light_color: [f32; 4],
    light_profiles: [f32; 4],
    reflection_color: [f32; 4],
    material_color: [f32; 4],
    shape_transform: [f32; 4],
    tint_color: [f32; 4],
    surface: [f32; 4],
    sharp_position: [f32; 4],
    soft_position: [f32; 4],
    shadow_position: [f32; 4],
    light_modes: [f32; 4],
    shadow_offset: [f32; 4],
    blur_info: [f32; 4],
    kernel: [[f32; 4]; 8],
}

#[derive(Default)]
pub struct GlassRenderer {
    gpu: Mutex<Option<Gpu>>,
}
/// Cumulative encoded work, for checking disabled paths without GPU timing noise.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GlassStats {
    pub cached_surfaces: usize,
    /// Cached effect textures only; excludes GPUI targets, driver overhead and buffers.
    pub cached_texture_bytes: u64,
    pub reflection_capture_texels: u64,
    pub diffuse_prefilter_texels: u64,
    pub slot_allocations: u64,
    pub atlas_uploads: u64,
    pub dynamic_atlas_builds: u64,
    pub backdrop_captures: u64,
    pub blur_passes: u64,
    pub uniform_uploads: u64,
    pub uniform_reuses: u64,
    pub reflection_passes: u64,
}
impl GlassRenderer {
    pub fn stats(&self) -> GlassStats {
        self.gpu
            .lock()
            .unwrap()
            .as_ref()
            .map_or(GlassStats::default(), |g| GlassStats {
                cached_surfaces: g.slots.len(),
                cached_texture_bytes: g.slots.values().map(Slot::texture_bytes).sum::<u64>()
                    + g.bleed_pool
                        .lock()
                        .unwrap()
                        .values()
                        .filter_map(std::sync::Weak::upgrade)
                        .map(|b| {
                            let t = &b.texture._texture;
                            (0..t.mip_level_count())
                                .map(|m| {
                                    (t.width() >> m).max(1) as u64
                                        * (t.height() >> m).max(1) as u64
                                        * 8
                                })
                                .sum::<u64>()
                        })
                        .sum::<u64>(),
                ..g.stats
            })
    }
    /// Release cached surface resources, for example after closing a document.
    pub fn clear_cache(&self) {
        if let Some(g) = self.gpu.lock().unwrap().as_mut() {
            g.slots.clear();
            g.bleed_history = None;
            g.bleed_pool.lock().unwrap().clear();
        }
    }
}
#[derive(Clone)]
pub struct GlassDraw {
    pub renderer: Arc<GlassRenderer>,
    pub id: u64,
    pub params: GlassParams,
    pub shape: Shape,
    pub scale: f32,
    pub contour: Option<Arc<GlassContour>>,
}
impl GlassDraw {
    /// Paint a custom contour/surface during a GPUI paint callback, registering
    /// its complete shadow extent while retaining the original shape bounds.
    pub fn paint_in(mut self, window: &mut gpui::Window, bounds: gpui::Bounds<gpui::Pixels>) {
        self.scale = window.scale_factor();
        crate::backend::paint(window, bounds, self);
    }

    /// Low-level payload. Prefer `paint_in` so outer shadows participate in
    /// GPUI scene ordering and culling with their full extent.
    pub fn effect(self) -> gpui::CustomBackdrop {
        WgpuBackdropEffect(Arc::new(self)).into_gpui()
    }
}
#[derive(Clone)]
struct Texture {
    view: wgpu::TextureView,
    _texture: wgpu::Texture,
}
struct BlurChain {
    levels: u32,
    pyramid: Vec<Texture>,
    reductions: Vec<wgpu::BindGroup>,
    ping: Texture,
    pong: Texture,
    uniform: wgpu::Buffer,
    horizontal: wgpu::BindGroup,
    vertical: wgpu::BindGroup,
}
struct DiffuseField {
    texture: Texture,
    bind: wgpu::BindGroup,
}
struct BleedPyramid {
    texture: Texture,
    levels: Vec<wgpu::TextureView>,
}
struct ReflectionChain {
    bleed: Option<Arc<BleedPyramid>>,
    bleed_downsample: Vec<wgpu::BindGroup>,
    diffuse: Option<DiffuseField>,
    edge: Option<Texture>,
    texture: Texture,
    levels: Vec<wgpu::TextureView>,
    downsample: Vec<wgpu::BindGroup>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct Geometry {
    width: u32,
    height: u32,
    shape: Shape,
    smoothing: u32,
    scale: u32,
    halo: u32,
    transmission_halo: u32,
    contour: u64,
    frost: Option<u32>,
    fog: Option<u32>,
    reflection: bool,
    diffuse: bool,
    edge_reflection: bool,
    diffuse_step: u32,
    native_diffuse: bool,
    bleed_size: [u32; 2],
    transmission: bool,
    atlas_pad: u32,
    dynamic: bool,
}
impl Geometry {
    fn same_atlas(self, other: Self) -> bool {
        (
            self.dynamic,
            self.width,
            self.height,
            self.shape,
            self.smoothing,
            self.scale,
            self.contour,
            self.atlas_pad,
        ) == (
            other.dynamic,
            other.width,
            other.height,
            other.shape,
            other.smoothing,
            other.scale,
            other.contour,
            other.atlas_pad,
        )
    }
}
struct Slot {
    geometry: Geometry,
    sharp: Texture,
    atlas: Texture,
    uniform: wgpu::Buffer,
    last_uniform: Option<Uniforms>,
    frost: Option<BlurChain>,
    fog: Option<BlurChain>,
    reflection: Option<ReflectionChain>,
    last_used: u64,
    last_seen: std::time::Instant,
    composite: wgpu::BindGroup,
    capture: Option<(wgpu::TextureView, wgpu::BindGroup)>,
    reflection_capture_bind: Option<wgpu::BindGroup>,
    bleed_capture_bind: Option<wgpu::BindGroup>,
    reflection_edge_bind: Option<wgpu::BindGroup>,
    crop_size: [u32; 2],
    dynamic: Option<crate::dynamic::State>,
}
impl Slot {
    fn texture_bytes(&self) -> u64 {
        let bytes = |t: &Texture| -> u64 {
            let pixel = t._texture.format().block_copy_size(None).unwrap_or(4) as u64;
            (0..t._texture.mip_level_count())
                .map(|level| {
                    (t._texture.width() >> level).max(1) as u64
                        * (t._texture.height() >> level).max(1) as u64
                        * pixel
                })
                .sum()
        };
        let mut total = bytes(&self.sharp) + bytes(&self.atlas);
        for chain in [&self.frost, &self.fog].into_iter().flatten() {
            total += bytes(&chain.ping) + bytes(&chain.pong);
            total += chain.pyramid.iter().map(bytes).sum::<u64>();
        }
        if let Some(r) = &self.reflection {
            total += bytes(&r.texture);
            if let Some(edge) = &r.edge {
                total += bytes(edge);
            }
            if let Some(d) = &r.diffuse {
                total += bytes(&d.texture);
            }
        }
        total
    }
}
struct Gpu {
    device: wgpu::Device,
    format: wgpu::TextureFormat,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    capture: wgpu::RenderPipeline,
    reduce: wgpu::RenderPipeline,
    horizontal: wgpu::RenderPipeline,
    vertical: wgpu::RenderPipeline,
    composite: wgpu::RenderPipeline,
    dynamic: crate::dynamic::Pipeline,
    reflection_layout: wgpu::BindGroupLayout,
    reflection_capture: wgpu::ComputePipeline,
    bleed_capture: wgpu::ComputePipeline,
    bleed_reduce: wgpu::ComputePipeline,
    bleed_history: Option<(u64, u64, wgpu::TextureView, wgpu::TextureView)>,
    reference_pyramid: bool,
    bleed_pool: Mutex<std::collections::HashMap<[u32; 2], std::sync::Weak<BleedPyramid>>>,
    reflection_reduce: wgpu::ComputePipeline,
    reflection_diffuse: wgpu::ComputePipeline,
    reflection_edge: wgpu::ComputePipeline,
    reflection_dummy: Vec<wgpu::TextureView>,
    tick: u64,
    stats: GlassStats,
    slots: HashMap<u64, Slot>,
}
// Keep reflection crop origins aligned to the useful radiance mip grid as
// reach changes; shifting coarse texel phase otherwise modulates thin sources.
// The native stencil reaches four parent pixels plus the bilinear footprint.
// Expand cumulatively before mapping damage into each mip's coordinate system.
fn bleed_region(damage: [f32; 4], size: [u32; 2], level: u32) -> [u32; 4] {
    let step = (1u32 << level) as f32;
    let halo = 5. * (step - 1.);
    let width = (size[0] >> level).max(1);
    let height = (size[1] >> level).max(1);
    let x = ((damage[0] - halo) / step).floor().clamp(0., width as f32) as u32;
    let y = ((damage[1] - halo) / step).floor().clamp(0., height as f32) as u32;
    let right = ((damage[2] + halo) / step)
        .ceil()
        .clamp(x as f32, width as f32) as u32;
    let bottom = ((damage[3] + halo) / step)
        .ceil()
        .clamp(y as f32, height as f32) as u32;
    [x, y, right - x, bottom - y]
}
fn transmission_halo(p: GlassParams) -> f32 {
    let refracted = if p.surface.is_none() {
        p.distortion.abs()
            + p.dispersion * (2. + p.distortion.abs() * 0.12)
            + p.blur.hypot(if p.fog_enabled() { p.fog } else { 0. }) * 3.
    } else {
        0.
    };
    ((refracted + 4.) / 16.).ceil().max(1.) * 16.
}
fn sampling_halo(p: GlassParams) -> f32 {
    // The reflection pyramid needs space for the widest distant cone filter too.
    let reflected = if p.reflections_enabled() {
        (p.reflection.sampling_radius.clamp(0., 320.) + 8.).max(p.paint_outset() + 16.)
    } else {
        0.
    };
    let quantum = if p.reflections_enabled() { 64. } else { 16. };
    ((reflected + 4.) / quantum).ceil().max(1.) * quantum
}
fn blur_levels(blur: f32, scale: f32) -> u32 {
    let mut levels = 0;
    while blur * scale / (1u32 << levels) as f32 > 3.5 && levels < 7 {
        levels += 1;
    }
    levels
}
// Exact Gaussian weights grouped into adjacent bilinear pairs. No stretched sparse taps.
fn gaussian_kernel(sigma: f32) -> ([[f32; 4]; 8], u32) {
    let sigma = sigma.max(0.05);
    let radius = (sigma * 3.).ceil().min(13.) as usize;
    let weights: Vec<f32> = (0..=radius)
        .map(|x| (-0.5 * (x as f32 / sigma).powi(2)).exp())
        .collect();
    let total = weights[0] + weights[1..].iter().sum::<f32>() * 2.;
    let mut kernel = [[0.; 4]; 8];
    kernel[0][1] = weights[0] / total;
    let mut count = 0;
    for i in (1..=radius).step_by(2) {
        let a = weights[i];
        let b = weights.get(i + 1).copied().unwrap_or(0.);
        count += 1;
        kernel[count] = [i as f32 + b / (a + b).max(1e-30), (a + b) / total, 0., 0.];
    }
    (kernel, count as u32)
}

impl Gpu {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("liquid-glass"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/glass.wgsl").into()),
        });
        let texture_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("glass-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                texture_entry(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                texture_entry(3),
                texture_entry(4),
                texture_entry(5),
                texture_entry(6),
                texture_entry(7),
                texture_entry(8),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("glass-pipeline-layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = |entry: &str, composite: bool, target_format| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(entry),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(if composite {
                        "glass_vertex"
                    } else {
                        "fullscreen"
                    }),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: target_format,
                        blend: if composite {
                            Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING)
                        } else {
                            None
                        },
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let capture = pipeline("capture", false, format);
        let reduce = pipeline("reduce", false, format);
        let horizontal = pipeline("blur_h", false, format);
        let vertical = pipeline("blur_v", false, format);
        let composite = pipeline("glass", true, format);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("glass-linear-clamp"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            anisotropy_clamp: 8,
            ..Default::default()
        });
        let reflection_source = include_str!("../shaders/reflection.wgsl");
        let reflection_source = if device.features().contains(wgpu::Features::SHADER_F16) {
            format!(
                "enable f16;\n{}",
                reflection_source
                    .replace(
                        "alias BleedValue = vec4<f32>;",
                        "alias BleedValue = vec4<f16>;"
                    )
                    .replace("return quantizeToF16(v);", "return v;")
            )
        } else {
            reflection_source.to_owned()
        };
        let reflection_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("glass-exterior-compute"),
            source: wgpu::ShaderSource::Wgsl(reflection_source.into()),
        });
        let mut entries = vec![
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                visibility: wgpu::ShaderStages::COMPUTE,
                ..texture_entry(1)
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                visibility: wgpu::ShaderStages::COMPUTE,
                ..texture_entry(3)
            },
        ];
        for binding in 4..8 {
            entries.push(wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::StorageTexture {
                    access: wgpu::StorageTextureAccess::WriteOnly,
                    format: wgpu::TextureFormat::Rgba16Float,
                    view_dimension: wgpu::TextureViewDimension::D2,
                },
                count: None,
            });
        }
        entries.push(wgpu::BindGroupLayoutEntry {
            visibility: wgpu::ShaderStages::COMPUTE,
            ..texture_entry(8)
        });
        let reflection_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("glass-reflection-compute-layout"),
            entries: &entries,
        });
        let compute_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("glass-reflection-compute-pipeline"),
            bind_group_layouts: &[Some(&reflection_layout)],
            immediate_size: 0,
        });
        let compute = |entry| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&compute_layout),
                module: &reflection_shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let reflection_dummy = (0..3)
            .map(|_| {
                device
                    .create_texture(&wgpu::TextureDescriptor {
                        label: Some("glass-unused-mip"),
                        size: wgpu::Extent3d {
                            width: 1,
                            height: 1,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: wgpu::TextureFormat::Rgba16Float,
                        usage: wgpu::TextureUsages::STORAGE_BINDING,
                        view_formats: &[],
                    })
                    .create_view(&Default::default())
            })
            .collect();
        Self {
            device: device.clone(),
            format,
            layout,
            sampler,
            capture,
            reduce,
            horizontal,
            vertical,
            composite,
            reflection_capture: compute("capture"),
            bleed_capture: compute("bleed_capture"),
            bleed_reduce: compute("bleed_mip"),
            bleed_history: None,
            reference_pyramid: std::env::var_os("GLASS_REFERENCE_PYRAMID").is_some(),
            bleed_pool: Mutex::new(std::collections::HashMap::new()),
            reflection_reduce: compute("gaussian_mip"),
            reflection_diffuse: compute("diffuse"),
            reflection_edge: compute("edge"),
            reflection_layout,
            reflection_dummy,
            tick: 0,
            dynamic: crate::dynamic::Pipeline::new(device),
            stats: GlassStats::default(),
            slots: HashMap::new(),
        }
    }
    fn texture(&self, label: &str, w: u32, h: u32, format: wgpu::TextureFormat) -> Texture {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_DST
                | if format == wgpu::TextureFormat::Rgba16Float {
                    wgpu::TextureUsages::STORAGE_BINDING
                } else {
                    wgpu::TextureUsages::empty()
                },
            view_formats: &[],
        });
        Texture {
            view: texture.create_view(&Default::default()),
            _texture: texture,
        }
    }
    fn bind(
        &self,
        uniform: &wgpu::Buffer,
        source: &wgpu::TextureView,
        frost: &wgpu::TextureView,
        shape: &wgpu::TextureView,
        fog: &wgpu::TextureView,
        reflections: [Option<&wgpu::TextureView>; 3],
    ) -> wgpu::BindGroup {
        let [reflection, diffuse, edge] = reflections;
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("glass-bindings"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(frost),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(shape),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(fog),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::TextureView(reflection.unwrap_or(source)),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: wgpu::BindingResource::TextureView(diffuse.unwrap_or(source)),
                },
                wgpu::BindGroupEntry {
                    binding: 8,
                    resource: wgpu::BindingResource::TextureView(edge.unwrap_or(source)),
                },
            ],
        })
    }
    fn uniform(&self) -> wgpu::Buffer {
        self.device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("glass-uniforms"),
                contents: bytemuck::bytes_of(&Uniforms::zeroed()),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            })
    }
    fn blur_chain(
        &self,
        sharp: &Texture,
        atlas: &Texture,
        crop_size: [u32; 2],
        levels: u32,
    ) -> BlurChain {
        let mut pyramid = Vec::new();
        let mut blur_size = crop_size;
        for _ in 0..levels {
            blur_size = [blur_size[0].div_ceil(2), blur_size[1].div_ceil(2)];
            pyramid.push(self.texture(
                "glass-filtered-reduction",
                blur_size[0],
                blur_size[1],
                self.format,
            ));
        }
        let ping = self.texture("glass-blur-x", blur_size[0], blur_size[1], self.format);
        let pong = self.texture("glass-blur-y", blur_size[0], blur_size[1], self.format);
        let uniform = self.uniform();
        let mut source = &sharp.view;
        let mut reductions = Vec::new();
        for target in &pyramid {
            reductions.push(self.bind(&uniform, source, source, &atlas.view, source, [None; 3]));
            source = &target.view;
        }
        let horizontal = self.bind(&uniform, source, source, &atlas.view, source, [None; 3]);
        let vertical = self.bind(
            &uniform,
            &ping.view,
            &ping.view,
            &atlas.view,
            &ping.view,
            [None; 3],
        );
        BlurChain {
            levels,
            pyramid,
            reductions,
            ping,
            pong,
            uniform,
            horizontal,
            vertical,
        }
    }
    fn reflection_bind(
        &self,
        uniform: &wgpu::Buffer,
        source: &wgpu::TextureView,
        atlas: &wgpu::TextureView,
        levels: &[wgpu::TextureView],
        radiance: Option<&wgpu::TextureView>,
    ) -> wgpu::BindGroup {
        let mut entries = vec![
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(source),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(&self.sampler),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(atlas),
            },
        ];
        for i in 0..4 {
            entries.push(wgpu::BindGroupEntry {
                binding: 4 + i as u32,
                resource: wgpu::BindingResource::TextureView(
                    levels
                        .get(i)
                        .unwrap_or_else(|| &self.reflection_dummy[(i - levels.len()) % 3]),
                ),
            });
        }
        entries.push(wgpu::BindGroupEntry {
            binding: 8,
            resource: wgpu::BindingResource::TextureView(radiance.unwrap_or(source)),
        });
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("glass-reflection-compute-bind"),
            layout: &self.reflection_layout,
            entries: &entries,
        })
    }
    fn reflection_chain(
        &self,
        crop_size: [u32; 2],
        uniform: &wgpu::Buffer,
        atlas: &Texture,
        geometry: Geometry,
        scale: f32,
    ) -> ReflectionChain {
        // Four-logical-pixel radiance uses hardware trilinear mip sampling for a
        // bounded-cost reflection composite regardless of the sampling radius.
        // The edge field reads original scene detail near the rim instead of
        // forcing this whole exterior rectangle to retain physical-pixel resolution.
        let step = (4. * scale).ceil().max(4.) as u32;
        let width = crop_size[0].div_ceil(step);
        let height = crop_size[1].div_ceil(step);
        let mip_count = 32 - width.max(height).leading_zeros();
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("glass-exterior-radiance"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: mip_count,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
            view_formats: &[],
        });
        let levels: Vec<_> = (0..mip_count)
            .map(|i| {
                texture.create_view(&wgpu::TextureViewDescriptor {
                    base_mip_level: i,
                    mip_level_count: Some(1),
                    ..Default::default()
                })
            })
            .collect();
        let downsample = (1..levels.len())
            .map(|i| {
                self.reflection_bind(
                    uniform,
                    &levels[i - 1],
                    &atlas.view,
                    std::slice::from_ref(&levels[i]),
                    None,
                )
            })
            .collect();
        let bleed = (geometry.diffuse && geometry.native_diffuse).then(|| {
            // Preserve native level zero; pre-box-reducing it changes the impulse response.
            let mut pool = self.bleed_pool.lock().unwrap();
            if let Some(cached) = pool
                .get(&geometry.bleed_size)
                .and_then(std::sync::Weak::upgrade)
            {
                return cached;
            }
            if pool.len() > 128 {
                pool.retain(|_, v| v.strong_count() > 0);
            }
            let width = geometry.bleed_size[0];
            let height = geometry.bleed_size[1];
            let mip_count = 32 - width.max(height).leading_zeros();
            // Native bleed samples the backdrop, not the exterior-masked edge field.
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("glass-native-bleed-pyramid"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: mip_count,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
                view_formats: &[],
            });
            let levels: Vec<_> = (0..mip_count)
                .map(|i| {
                    texture.create_view(&wgpu::TextureViewDescriptor {
                        base_mip_level: i,
                        mip_level_count: Some(1),
                        ..Default::default()
                    })
                })
                .collect();
            let result = Arc::new(BleedPyramid {
                texture: Texture {
                    view: texture.create_view(&Default::default()),
                    _texture: texture,
                },
                levels,
            });
            pool.insert(geometry.bleed_size, Arc::downgrade(&result));
            result
        });
        // Bind per-surface uniforms to shared storage; another surface must
        // never change this draw's damage rectangle through a shared bind group.
        let bleed_downsample = bleed.as_ref().map_or_else(Vec::new, |b| {
            (1..b.levels.len())
                .map(|i| {
                    self.reflection_bind(
                        uniform,
                        &b.levels[i - 1],
                        &atlas.view,
                        std::slice::from_ref(&b.levels[i]),
                        None,
                    )
                })
                .collect()
        });
        let diffuse = (geometry.diffuse && !geometry.native_diffuse).then(|| {
            let step = geometry.diffuse_step;
            let target = self.texture(
                "glass-diffuse-field",
                crop_size[0].div_ceil(step),
                crop_size[1].div_ceil(step),
                wgpu::TextureFormat::Rgba16Float,
            );
            let source = texture.create_view(&Default::default());
            let bind = self.reflection_bind(
                uniform,
                &source,
                &atlas.view,
                std::slice::from_ref(&target.view),
                None,
            );
            DiffuseField {
                texture: target,
                bind,
            }
        });
        let edge = geometry.edge_reflection.then(|| {
            self.texture(
                "glass-edge-field",
                (geometry.width as f32 / scale).ceil() as u32,
                (geometry.height as f32 / scale).ceil() as u32,
                wgpu::TextureFormat::Rgba16Float,
            )
        });
        ReflectionChain {
            bleed,
            bleed_downsample,
            diffuse,
            edge,
            texture: Texture {
                view: texture.create_view(&Default::default()),
                _texture: texture,
            },
            levels,
            downsample,
        }
    }
    fn slot(
        &self,
        geometry: Geometry,
        scale: f32,
        contour: Option<&GlassContour>,
        queue: &wgpu::Queue,
        reusable_atlas: Option<Texture>,
    ) -> Slot {
        let g = geometry;
        let crop_size = [
            g.width + (g.halo as f32 * 2. * scale).ceil() as u32,
            g.height + (g.halo as f32 * 2. * scale).ceil() as u32,
        ];
        let sharp_size = if g.transmission {
            [
                g.width + (g.transmission_halo as f32 * 2. * scale).ceil() as u32,
                g.height + (g.transmission_halo as f32 * 2. * scale).ceil() as u32,
            ]
        } else {
            [1, 1]
        };
        let sharp = self.texture("glass-crop", sharp_size[0], sharp_size[1], self.format);
        let atlas = reusable_atlas.unwrap_or_else(|| {
            if g.dynamic {
                return self.texture(
                    "dynamic-distance-normal",
                    ((g.width as f32 / scale + 2. * g.atlas_pad as f32) * 2.).ceil() as u32,
                    ((g.height as f32 / scale + 2. * g.atlas_pad as f32) * 2.).ceil() as u32,
                    wgpu::TextureFormat::Rgba16Float,
                );
            }
            let logical = [g.width as f32 / scale, g.height as f32 / scale];
            let outline = if let Some(contour) = contour {
                contour
                    .points
                    .iter()
                    .map(|p| [p[0] * logical[0], p[1] * logical[1]])
                    .collect()
            } else {
                shape::outline(g.shape, logical[0], logical[1], f32::from_bits(g.smoothing))
            };
            let sdf =
                shape::distance_atlas_padded(&outline, logical[0], logical[1], g.atlas_pad as f32);
            let atlas = self.texture(
                "glass-distance-normal",
                sdf.width,
                sdf.height,
                wgpu::TextureFormat::Rgba16Float,
            );
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &atlas._texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                bytemuck::cast_slice(&sdf.data),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(sdf.width * 8),
                    rows_per_image: Some(sdf.height),
                },
                wgpu::Extent3d {
                    width: sdf.width,
                    height: sdf.height,
                    depth_or_array_layers: 1,
                },
            );
            atlas
        });
        let uniform = self.uniform();
        let frost = g
            .frost
            .map(|levels| self.blur_chain(&sharp, &atlas, sharp_size, levels));
        let fog = g
            .fog
            .map(|levels| self.blur_chain(&sharp, &atlas, sharp_size, levels));
        let reflection = g
            .reflection
            .then(|| self.reflection_chain(crop_size, &uniform, &atlas, g, scale));
        let composite = self.bind(
            &uniform,
            &sharp.view,
            frost.as_ref().map_or(&sharp.view, |f| &f.pong.view),
            &atlas.view,
            fog.as_ref().map_or(&sharp.view, |f| &f.pong.view),
            [
                reflection.as_ref().map(|r| {
                    r.bleed
                        .as_ref()
                        .map_or(&r.texture.view, |b| &b.texture.view)
                }),
                reflection
                    .as_ref()
                    .and_then(|r| r.diffuse.as_ref().map(|d| &d.texture.view)),
                reflection
                    .as_ref()
                    .and_then(|r| r.edge.as_ref().map(|e| &e.view)),
            ],
        );
        let dynamic = g
            .dynamic
            .then(|| self.dynamic.state(&self.device, &atlas.view));
        Slot {
            dynamic,
            geometry,
            sharp,
            atlas,
            uniform,
            last_uniform: None,
            frost,
            fog,
            reflection,
            composite,
            capture: None,
            reflection_capture_bind: None,
            bleed_capture_bind: None,
            reflection_edge_bind: None,
            crop_size,
            last_used: 0,
            last_seen: std::time::Instant::now(),
        }
    }
}
use bytemuck::Zeroable;
fn pass(
    encoder: &mut wgpu::CommandEncoder,
    label: &str,
    target: &wgpu::TextureView,
    pipeline: &wgpu::RenderPipeline,
    bind: &wgpu::BindGroup,
    scissor: Option<[u32; 4]>,
) {
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(label),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: target,
            resolve_target: None,
            ops: wgpu::Operations {
                load: if scissor.is_some() {
                    wgpu::LoadOp::Load
                } else {
                    wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
                },
                store: wgpu::StoreOp::Store,
            },
            depth_slice: None,
        })],
        ..Default::default()
    });
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, bind, &[]);
    if let Some([x, y, w, h]) = scissor {
        pass.set_scissor_rect(x, y, w, h);
    }
    pass.draw(0..3, 0..1);
}
impl BlurChain {
    fn encode(&self, gpu: &Gpu, c: &mut BackdropContext<'_>, sigma: f32) {
        let (kernel, taps) = gaussian_kernel(sigma / (1u32 << self.levels) as f32);
        let uniforms = Uniforms {
            blur_info: [
                1. / self.ping._texture.width() as f32,
                1. / self.ping._texture.height() as f32,
                taps as f32,
                0.,
            ],
            kernel,
            ..Uniforms::zeroed()
        };
        c.queue
            .write_buffer(&self.uniform, 0, bytemuck::bytes_of(&uniforms));
        for (target, bind) in self.pyramid.iter().zip(&self.reductions) {
            pass(
                c.encoder,
                "glass-filtered-downsample",
                &target.view,
                &gpu.reduce,
                bind,
                None,
            );
        }
        pass(
            c.encoder,
            "glass-gaussian-horizontal",
            &self.ping.view,
            &gpu.horizontal,
            &self.horizontal,
            None,
        );
        pass(
            c.encoder,
            "glass-gaussian-vertical",
            &self.pong.view,
            &gpu.vertical,
            &self.vertical,
            None,
        );
    }
}
impl WgpuBackdrop for GlassDraw {
    fn damage_bounds(&self, filter: &gpui::BackdropFilter) -> gpui::Bounds<gpui::ScaledPixels> {
        filter
            .bounds
            .dilate(gpui::ScaledPixels(self.params.paint_outset() * self.scale))
    }

    fn paint(&self, mut c: BackdropContext<'_>) {
        let b = c.filter.bounds;
        let p = self.params;
        let outset = p.paint_outset() * self.scale;
        let m = c.filter.content_mask.bounds;
        let x = (b.origin.x.0 - outset).max(m.origin.x.0).max(0.).floor() as u32;
        let y = (b.origin.y.0 - outset).max(m.origin.y.0).max(0.).floor() as u32;
        let right = (b.origin.x.0 + b.size.width.0 + outset)
            .min(m.origin.x.0 + m.size.width.0)
            .ceil()
            .max(0.)
            .min(c.viewport[0] as f32) as u32;
        let bottom = (b.origin.y.0 + b.size.height.0 + outset)
            .min(m.origin.y.0 + m.size.height.0)
            .ceil()
            .max(0.)
            .min(c.viewport[1] as f32) as u32;
        if right <= x || bottom <= y || c.filter.opacity <= 0. {
            return;
        }
        let mut state = self.renderer.gpu.lock().unwrap();
        if state
            .as_ref()
            .is_none_or(|g| g.device != *c.device || g.format != c.format)
        {
            *state = Some(Gpu::new(c.device, c.format));
        }
        let g = state.as_mut().unwrap();
        // One world-anchored pyramid preserves native level zero and mip phase.
        // Later surfaces update conservative scene damage, not a stale snapshot.
        let bleed_crop = [0., 0., c.viewport[0] as f32, c.viewport[1] as f32];
        let mut geometry = Geometry {
            dynamic: p.dynamic_shape && self.contour.is_none(),
            width: b.size.width.0.ceil() as u32,
            height: b.size.height.0.ceil() as u32,
            shape: self.shape,
            smoothing: p.smoothing.to_bits(),
            scale: self.scale.to_bits(),
            halo: sampling_halo(p) as u32,
            transmission_halo: transmission_halo(p) as u32,
            contour: self.contour.as_ref().map_or(0, |c| c.key),
            frost: (p.surface.is_none() && p.blur >= 0.05).then(|| blur_levels(p.blur, self.scale)),
            fog: p
                .fog_enabled()
                .then(|| blur_levels(p.blur.hypot(p.fog), self.scale)),
            reflection: p.reflections_enabled(),
            edge_reflection: p.reflections_enabled()
                && p.reflection.edge_intensity > 0.
                && p.reflection.edge_width > 0.,
            native_diffuse: p.reflection.native_diffuse,
            bleed_size: if p.reflections_enabled() && p.reflection.native_diffuse {
                [bleed_crop[2] as u32, bleed_crop[3] as u32]
            } else {
                [1, 1]
            },
            diffuse_step: ((if p.reflection.diffuse_radius < 8. {
                1.
            } else {
                4.
            }) * self.scale)
                .ceil()
                .max(1.) as u32,
            diffuse: p.reflections_enabled()
                && ((p.reflection.diffuse_intensity > 0. && p.reflection.diffuse_radius > 0.)
                    || (p.reflection.shadow_intensity > 0. && p.shadow_outset() > 0.)),
            transmission: p.surface.is_none(),
            atlas_pad: (((p.paint_outset()
                + if p.shadow_outset() > 0. {
                    p.shadow_translation()
                } else {
                    0.
                })
                / 16.)
                .ceil()
                * 16.)
                .max(shape::PAD) as u32,
        };
        // A closing material can drop its shadow without rebuilding the distance
        // atlas. Retain padding for the lifetime of otherwise identical geometry.
        if let Some(old) = g.slots.get(&self.id).map(|s| s.geometry) {
            let mut same_shape = geometry;
            same_shape.atlas_pad = old.atlas_pad;
            if old.same_atlas(same_shape) {
                geometry.atlas_pad = geometry.atlas_pad.max(old.atlas_pad);
            }
        }
        // Retain the maximum sampled extent while tuning radius. Uniforms use
        // the retained crop; decreasing radius never reallocates the same surface.
        if let Some(old) = g.slots.get(&self.id).map(|s| s.geometry)
            && old.same_atlas(geometry)
            && old.reflection == geometry.reflection
            && old.diffuse == geometry.diffuse
            && old.transmission == geometry.transmission
            && old.frost == geometry.frost
            && old.fog == geometry.fog
        {
            geometry.halo = geometry.halo.max(old.halo);
            geometry.transmission_halo = geometry.transmission_halo.max(old.transmission_halo);
        }
        if g.slots.get(&self.id).is_none_or(|s| s.geometry != geometry) {
            // Soft retention limit: only evict IDs idle for two seconds. A cyclic
            // working set larger than the limit must never rebuild every frame.
            // Callers can explicitly clear caches when closing a document.
            if g.slots.len() >= 128
                && !g.slots.contains_key(&self.id)
                && let Some(id) = g
                    .slots
                    .iter()
                    .filter(|(_, s)| s.last_seen.elapsed().as_secs_f32() > 2.)
                    .min_by_key(|(_, s)| s.last_used)
                    .map(|(&id, _)| id)
            {
                g.slots.remove(&id);
            }
            let atlas = g
                .slots
                .get(&self.id)
                .filter(|s| s.geometry.same_atlas(geometry))
                .map(|s| s.atlas.clone());
            g.stats.slot_allocations += 1;
            g.stats.atlas_uploads += u64::from(atlas.is_none());
            let slot = g.slot(
                geometry,
                self.scale,
                self.contour.as_deref(),
                c.queue,
                atlas,
            );
            g.slots.insert(self.id, slot);
        }
        g.tick = g.tick.wrapping_add(1);
        let active = g.slots.get_mut(&self.id).unwrap();
        active.last_used = g.tick;
        active.last_seen = std::time::Instant::now();
        if g.slots[&self.id]
            .capture
            .as_ref()
            .is_none_or(|(v, _)| v != c.scene)
        {
            let s = &g.slots[&self.id];
            let bind = g.bind(
                &s.uniform,
                c.scene,
                &s.atlas.view,
                &s.atlas.view,
                &s.atlas.view,
                [None; 3],
            );
            let reflection_bind = s.reflection.as_ref().map(|r| {
                g.reflection_bind(
                    &s.uniform,
                    c.scene,
                    &s.atlas.view,
                    std::slice::from_ref(&r.levels[0]),
                    None,
                )
            });
            let bleed_bind = s.reflection.as_ref().and_then(|r| {
                r.bleed.as_ref().map(|b| {
                    g.reflection_bind(
                        &s.uniform,
                        c.scene,
                        &s.atlas.view,
                        std::slice::from_ref(&b.levels[0]),
                        None,
                    )
                })
            });
            g.slots.get_mut(&self.id).unwrap().bleed_capture_bind = bleed_bind;
            let s = &g.slots[&self.id];
            let edge_bind = s.reflection.as_ref().and_then(|r| {
                r.edge.as_ref().map(|edge| {
                    g.reflection_bind(
                        &s.uniform,
                        c.scene,
                        &s.atlas.view,
                        std::slice::from_ref(&edge.view),
                        Some(&r.texture.view),
                    )
                })
            });
            g.slots.get_mut(&self.id).unwrap().reflection_edge_bind = edge_bind;
            g.slots.get_mut(&self.id).unwrap().reflection_capture_bind = reflection_bind;
            g.slots.get_mut(&self.id).unwrap().capture = Some((c.scene.clone(), bind));
        }
        if geometry.dynamic {
            let Gpu {
                slots,
                dynamic,
                stats,
                ..
            } = g;
            let slot = slots.get_mut(&self.id).unwrap();
            if dynamic.update(
                slot.dynamic.as_mut().unwrap(),
                c.queue,
                c.encoder,
                self.shape,
                p.smoothing,
                [
                    geometry.width as f32 / self.scale,
                    geometry.height as f32 / self.scale,
                ],
                p.shape_scale,
                geometry.atlas_pad as f32,
                [slot.atlas._texture.width(), slot.atlas._texture.height()],
            ) {
                stats.dynamic_atlas_builds += 1;
            }
        }
        let mut bleed_damage = bleed_crop;
        if let Some(bleed) = g.slots[&self.id]
            .reflection
            .as_ref()
            .and_then(|r| r.bleed.as_ref())
        {
            if let Some((frame, sequence, scene, pyramid)) = &g.bleed_history
                && *frame == c.frame_id
                && *sequence + 1 == c.sequence
                && scene == c.scene
                && *pyramid == bleed.texture.view
            {
                bleed_damage = c.source_damage;
            }
            g.bleed_history = Some((
                c.frame_id,
                c.sequence,
                c.scene.clone(),
                bleed.texture.view.clone(),
            ));
        }
        let s = &g.slots[&self.id];
        let halo = geometry.halo as f32;
        let l = p.lighting;
        let r = p.reflection;
        let light = p.light.max(0.);
        let uniforms = Uniforms {
            viewport: [
                c.viewport[0] as f32,
                c.viewport[1] as f32,
                self.scale,
                c.filter.opacity,
            ],
            bounds: [b.origin.x.0, b.origin.y.0, b.size.width.0, b.size.height.0],
            crop: [
                b.origin.x.0 - halo * self.scale,
                b.origin.y.0 - halo * self.scale,
                s.crop_size[0] as f32,
                s.crop_size[1] as f32,
            ],
            bleed_crop,
            bleed_damage,
            transmission_crop: [
                b.origin.x.0 - geometry.transmission_halo as f32 * self.scale,
                b.origin.y.0 - geometry.transmission_halo as f32 * self.scale,
                s.sharp._texture.width() as f32,
                s.sharp._texture.height() as f32,
            ],
            optics: [p.blur, p.opacity, p.saturation, p.edge],
            lens: [p.distortion, p.profile, p.dispersion, p.splay],
            finish: [
                light,
                geometry.atlas_pad as f32,
                if p.fog_enabled() { p.fog } else { 0. },
                0.35 * (b.size.width.0 * p.shape_scale[0].clamp(0.05, 1.))
                    .min(b.size.height.0 * p.shape_scale[1].clamp(0.05, 1.))
                    / self.scale,
            ],
            fog_params: [
                p.fog_edge.max(0.),
                p.fog_opacity.clamp(0., 1.),
                p.fog_edge_opacity.clamp(0., 1.),
                0.,
            ],
            reflection_edge_tone: [
                r.edge_brightness_min.clamp(0., 2.),
                r.edge_brightness_max.clamp(0., 2.),
                0.,
                0.,
            ],
            reflection_tone: [
                r.brightness_min.clamp(0., 2.),
                r.brightness_max.clamp(0., 2.),
                r.dark_bias.clamp(0., 1.),
                f32::from(r.native_diffuse),
            ],
            tint_color: [
                p.tint_color[0].clamp(0., 1.),
                p.tint_color[1].clamp(0., 1.),
                p.tint_color[2].clamp(0., 1.),
                0.,
            ],
            shape_transform: [
                if p.dynamic_shape {
                    1.
                } else {
                    p.shape_scale[0].clamp(0.05, 1.)
                },
                if p.dynamic_shape {
                    1.
                } else {
                    p.shape_scale[1].clamp(0.05, 1.)
                },
                if self.shape == Shape::Rectangle
                    && self.contour.is_none()
                    && !p.dynamic_shape
                    && p.shape_scale == [1., 1.]
                {
                    1.
                } else {
                    0.
                },
                (b.size.width.0 * p.shape_scale[0].clamp(0.05, 1.))
                    / (b.size.height.0 * p.shape_scale[1].clamp(0.05, 1.)).max(0.00001),
            ],
            material_color: [
                p.brightness.clamp(-1., 1.),
                p.contrast.clamp(0., 2.),
                p.vibrance.clamp(-1., 1.),
                0.,
            ],
            reflection_color: [
                r.saturation.clamp(0., 2.),
                r.vibrance.clamp(-1., 1.),
                r.brightness.clamp(-1., 1.),
                r.contrast.clamp(0., 3.),
            ],
            light_color: [
                l.sharp_brightness.clamp(0., 1.),
                l.soft_brightness.clamp(0., 1.),
                l.inner_shadow_brightness.clamp(0., 1.),
                0.,
            ],
            light_profiles: [
                l.sharp_spread.clamp(-1., 0.99),
                l.soft_spread.clamp(-1., 0.99),
                l.inner_shadow_width.max(0.),
                0.,
            ],
            sharp_position: [
                l.sharp_angle.to_radians().sin(),
                -l.sharp_angle.to_radians().cos(),
                l.sharp_inset.max(0.),
                l.sharp_focus.max(0.),
            ],
            soft_position: [
                l.soft_angle.to_radians().sin(),
                -l.soft_angle.to_radians().cos(),
                l.soft_inset.max(0.),
                l.soft_focus.max(0.),
            ],
            shadow_position: [
                l.inner_shadow_angle.to_radians().sin(),
                -l.inner_shadow_angle.to_radians().cos(),
                l.inner_shadow_inset.max(0.),
                l.opposite.clamp(0., 1.),
            ],
            light_modes: [
                l.sharp_blend as u32 as f32,
                l.soft_blend as u32 as f32,
                l.inner_shadow_blend as u32 as f32,
                0.,
            ],
            shadow_offset: [
                l.outer_shadow_angle.to_radians().sin() * p.shadow_translation(),
                -l.outer_shadow_angle.to_radians().cos() * p.shadow_translation(),
                0.,
                0.,
            ],
            outline_sharp: [
                l.outline_opacity * light,
                l.outline_width.max(0.1),
                l.sharp_opacity * light,
                l.sharp_width.max(0.1),
            ],
            soft_inner: [
                l.soft_opacity * light,
                l.soft_width.max(0.1),
                l.inner_shadow_opacity * light,
                l.inner_shadow_radius.max(0.1),
            ],
            outer: [
                l.outer_shadow_opacity * light,
                l.outer_shadow_radius.clamp(0.5, 64.),
                p.paint_outset(),
                0.,
            ],
            reflection: [
                if geometry.reflection {
                    r.edge_intensity
                } else {
                    0.
                },
                r.edge_width.max(0.),
                if geometry.reflection {
                    r.diffuse_intensity
                } else {
                    0.
                },
                r.diffuse_radius_for_size(
                    b.size.width.0 / self.scale * p.shape_scale[0].clamp(0.05, 1.),
                    b.size.height.0 / self.scale * p.shape_scale[1].clamp(0.05, 1.),
                ),
            ],
            reflection_modes: [
                r.edge_blend as u32 as f32,
                r.diffuse_blend as u32 as f32,
                r.shadow_blend as u32 as f32,
                if geometry.reflection && p.shadow_outset() > 0. {
                    r.shadow_intensity.max(0.)
                } else {
                    0.
                },
            ],
            reflection_info: [
                r.sampling_radius.clamp(0., 320.),
                r.sharpness.clamp(0., 1.),
                s.reflection
                    .as_ref()
                    .map_or(0., |r| (r.levels.len() - 1) as f32),
                (4. * self.scale).ceil().max(4.),
            ],
            surface: p
                .surface
                .map_or([0.; 4], |rgb| [rgb[0], rgb[1], rgb[2], 1.]),
            ..Uniforms::zeroed()
        };
        let slot = g.slots.get_mut(&self.id).unwrap();
        if !g.reference_pyramid
            && slot
                .last_uniform
                .as_ref()
                .is_some_and(|old| bytemuck::bytes_of(old) == bytemuck::bytes_of(&uniforms))
        {
            g.stats.uniform_reuses += 1;
        } else {
            c.queue
                .write_buffer(&slot.uniform, 0, bytemuck::bytes_of(&uniforms));
            slot.last_uniform = Some(uniforms);
            g.stats.uniform_uploads += 1;
        }
        let s = &g.slots[&self.id];
        if geometry.transmission {
            g.stats.backdrop_captures += 1;
            pass(
                c.encoder,
                "glass-capture",
                &s.sharp.view,
                &g.capture,
                &s.capture.as_ref().unwrap().1,
                None,
            );
        }
        if let Some(frost) = &s.frost {
            g.stats.blur_passes += frost.levels as u64 + 2;
            frost.encode(g, &mut c, p.blur * self.scale);
        }
        if let Some(fog) = &s.fog {
            g.stats.blur_passes += fog.levels as u64 + 2;
            fog.encode(g, &mut c, p.blur.hypot(p.fog) * self.scale);
        }
        if let Some(reflection) = &s.reflection {
            g.stats.reflection_passes += 1;
            let mut pass = c.encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("glass-exterior-pyramid"),
                timestamp_writes: None,
            });
            if g.reference_pyramid || geometry.edge_reflection || !geometry.native_diffuse {
                g.stats.reflection_capture_texels += reflection.texture._texture.width() as u64
                    * reflection.texture._texture.height() as u64;
                pass.set_pipeline(&g.reflection_capture);
                pass.set_bind_group(0, s.reflection_capture_bind.as_ref().unwrap(), &[]);
                pass.dispatch_workgroups(
                    reflection.texture._texture.width().div_ceil(8),
                    reflection.texture._texture.height().div_ceil(8),
                    1,
                );
                for (index, bind) in reflection.downsample.iter().enumerate() {
                    pass.set_pipeline(&g.reflection_reduce);
                    pass.set_bind_group(0, bind, &[]);
                    pass.dispatch_workgroups(
                        (reflection.texture._texture.width() >> (index + 1))
                            .max(1)
                            .div_ceil(8),
                        (reflection.texture._texture.height() >> (index + 1))
                            .max(1)
                            .div_ceil(8),
                        1,
                    );
                }
            }
            if let Some(bleed) = &reflection.bleed {
                let size = [
                    bleed.texture._texture.width(),
                    bleed.texture._texture.height(),
                ];
                let [_, _, w, h] = bleed_region(bleed_damage, size, 0);
                g.stats.reflection_capture_texels += u64::from(w) * u64::from(h);
                pass.set_pipeline(&g.bleed_capture);
                pass.set_bind_group(0, s.bleed_capture_bind.as_ref().unwrap(), &[]);
                if w > 0 && h > 0 {
                    pass.dispatch_workgroups(w.div_ceil(8), h.div_ceil(8), 1);
                }
                for (index, bind) in reflection.bleed_downsample.iter().enumerate() {
                    let [_, _, w, h] = bleed_region(bleed_damage, size, index as u32 + 1);
                    pass.set_pipeline(&g.bleed_reduce);
                    pass.set_bind_group(0, bind, &[]);
                    if w > 0 && h > 0 {
                        pass.dispatch_workgroups(w.div_ceil(8), h.div_ceil(8), 1);
                    }
                }
            }
            if let Some(edge) = &reflection.edge {
                pass.set_pipeline(&g.reflection_edge);
                pass.set_bind_group(0, s.reflection_edge_bind.as_ref().unwrap(), &[]);
                pass.dispatch_workgroups(
                    edge._texture.width().div_ceil(8),
                    edge._texture.height().div_ceil(8),
                    1,
                );
            }
            if let Some(diffuse) = &reflection.diffuse {
                g.stats.diffuse_prefilter_texels += diffuse.texture._texture.width() as u64
                    * diffuse.texture._texture.height() as u64;
                pass.set_pipeline(&g.reflection_diffuse);
                pass.set_bind_group(0, &diffuse.bind, &[]);
                pass.dispatch_workgroups(
                    diffuse.texture._texture.width().div_ceil(8),
                    diffuse.texture._texture.height().div_ceil(8),
                    1,
                );
            }
        }
        pass(
            c.encoder,
            "glass-composite",
            c.scene,
            &g.composite,
            &s.composite,
            Some([x, y, right - x, bottom - y]),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gaussian_preserves_constant_color_and_has_dense_coverage() {
        for sigma in [0.05, 0.3, 0.8, 1., 2., 3.5] {
            let (k, n) = gaussian_kernel(sigma);
            let sum = k[0][1] + 2. * k[1..=n as usize].iter().map(|v| v[1]).sum::<f32>();
            assert!((sum - 1.).abs() < 1e-5);
            for (i, v) in k[1..=n as usize].iter().enumerate() {
                assert!(v[0] >= (i * 2 + 1) as f32 && v[0] <= (i * 2 + 2) as f32);
                assert!(v[1] >= 0.);
            }
        }
    }
    #[test]
    fn disabled_layers_do_not_expand_the_capture_or_shadow() {
        let base = GlassParams {
            reflection: ReflectionParams::disabled(),
            fog: 0.,
            light: 0.,
            ..Default::default()
        };
        let dormant = GlassParams {
            fog: 100.,
            fog_opacity: 0.,
            reflection: ReflectionParams {
                sampling_radius: 320.,
                ..ReflectionParams::disabled()
            },
            lighting: LightParams {
                outer_shadow_radius: 64.,
                ..LightParams::default()
            },
            ..base
        };
        assert_eq!(sampling_halo(base), sampling_halo(dormant));
        assert_eq!(dormant.shadow_outset(), 0.);
        assert!(!dormant.fog_enabled());
        assert!(!dormant.reflection.enabled());
        let individual = GlassParams {
            light: 1.,
            lighting: LightParams::disabled(),
            ..dormant
        };
        assert_eq!(individual.shadow_outset(), 0.);
        let sidebar = dormant.reflection_only([0.1, 0.2, 0.3]);
        assert!(!sidebar.fog_enabled());
        assert_eq!(sidebar.light, 0.);
        assert_eq!(sidebar.distortion, 0.);
    }
    #[test]
    fn reflection_capture_work_has_a_bounded_radius() {
        let base = GlassParams::default().reflection_only([0.; 3]);
        let large = GlassParams {
            reflection: ReflectionParams {
                sampling_radius: 320.,
                ..base.reflection
            },
            ..base
        };
        let larger = GlassParams {
            reflection: ReflectionParams {
                sampling_radius: 10000.,
                ..base.reflection
            },
            ..base
        };
        assert_eq!(sampling_halo(large), sampling_halo(larger));
    }
    #[test]
    fn shader_validates_without_a_gpu() {
        for source in [
            include_str!("../shaders/glass.wgsl"),
            include_str!("../shaders/reflection.wgsl"),
            include_str!("../shaders/dynamic_shape.wgsl"),
        ] {
            let module = naga::front::wgsl::parse_str(source).unwrap();
            naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::SHADER_FLOAT16_IN_FLOAT32,
            )
            .validate(&module)
            .unwrap();
        }
    }
}

#[cfg(all(test, not(target_family = "wasm")))]
#[path = "reflection_tests.rs"]
mod reflection_tests;

#[cfg(test)]
#[path = "diffuse_size_tests.rs"]
mod diffuse_size_tests;
