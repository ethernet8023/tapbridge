#[cfg(target_os = "macos")]
use objc2_metal::{
    MTLBlitCommandEncoder as _, MTLCommandBuffer as _, MTLCommandEncoder as _,
    MTLCommandQueue as _, MTLDevice as _,
};
use std::sync::Arc;

#[cfg(target_os = "macos")]
struct MetalFrameClock {
    queue: objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2_metal::MTLCommandQueue>>,
    buffer: objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2_metal::MTLBuffer>>,
}
#[cfg(target_os = "macos")]
impl MetalFrameClock {
    fn marker(
        &self,
    ) -> Option<
        objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2_metal::MTLCommandBuffer>>,
    > {
        let command = self.queue.commandBuffer()?;
        let blit = command.blitCommandEncoder()?;
        // A tiny real blit prevents empty marker command buffers being discarded.
        unsafe {
            blit.copyFromBuffer_sourceOffset_toBuffer_destinationOffset_size(
                &self.buffer,
                0,
                &self.buffer,
                4,
                4,
            );
        }
        blit.endEncoding();
        Some(command)
    }
}

use gpui::{DevicePixels, Scene, Size};

use crate::{WgpuAtlas, WgpuContext};

use super::{WgpuRenderer, WgpuSurfaceConfig};

struct OffscreenTarget {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    readback: wgpu::Buffer,
    padded_bytes_per_row: u32,
    size: Size<DevicePixels>,
}

impl WgpuRenderer {
    pub(super) fn new_headless(
        context: &WgpuContext,
        size: Size<DevicePixels>,
    ) -> anyhow::Result<Self> {
        Self::new_internal(
            None,
            context,
            None,
            WgpuSurfaceConfig {
                size,
                transparent: false,
                preferred_present_mode: None,
            },
            None,
            None,
            Arc::new(WgpuAtlas::from_context(context)),
        )
    }

    fn create_offscreen_target(&self) -> OffscreenTarget {
        let width = self.target.width();
        let height = self.target.height();
        let padded_bytes_per_row = (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let texture = self
            .resources()
            .device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("gpui_offscreen_target"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.target.format(),
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let readback = self
            .resources()
            .device
            .create_buffer(&wgpu::BufferDescriptor {
                label: Some("gpui_offscreen_readback"),
                size: u64::from(padded_bytes_per_row) * u64::from(height),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
        OffscreenTarget {
            texture,
            view,
            readback,
            padded_bytes_per_row,
            size: self.viewport_size(),
        }
    }

    fn read_offscreen_target(
        &self,
        target: &OffscreenTarget,
        submission: wgpu::SubmissionIndex,
    ) -> anyhow::Result<image::RgbaImage> {
        let width = target.size.width.0 as u32;
        let height = target.size.height.0 as u32;
        let bytes_per_row = width * 4;
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        target
            .readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        self.resources()
            .device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: None,
            })
            .map_err(|error| anyhow::anyhow!("failed to poll offscreen readback: {error}"))?;
        receiver
            .recv()
            .map_err(|error| anyhow::anyhow!("offscreen readback callback dropped: {error}"))?
            .map_err(|error| anyhow::anyhow!("failed to map offscreen readback: {error}"))?;

        let mapped = target.readback.slice(..).get_mapped_range();
        let mut pixels = Vec::with_capacity(bytes_per_row as usize * height as usize);
        for row in mapped.chunks_exact(target.padded_bytes_per_row as usize) {
            pixels.extend_from_slice(&row[..bytes_per_row as usize]);
        }
        drop(mapped);
        target.readback.unmap();
        if self.target.format() == wgpu::TextureFormat::Bgra8Unorm {
            for pixel in pixels.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
        }
        image::RgbaImage::from_raw(width, height, pixels)
            .ok_or_else(|| anyhow::anyhow!("offscreen readback dimensions did not match its data"))
    }

    /// Renders through the normal scene path and reads back without presenting.
    pub fn render_to_image(&mut self, scene: &Scene) -> anyhow::Result<image::RgbaImage> {
        let target = self.create_offscreen_target();
        let submission = self
            .render_to_view_with_readback(scene, &target.view, target.readback_copy())
            .ok_or_else(|| anyhow::anyhow!("failed to render scene into the offscreen target"))?;
        self.read_offscreen_target(&target, submission)
    }
}

impl OffscreenTarget {
    fn readback_copy(&self) -> super::frame::ReadbackCopy<'_> {
        super::frame::ReadbackCopy {
            texture: &self.texture,
            buffer: &self.readback,
            bytes_per_row: self.padded_bytes_per_row,
            width: self.size.width.0 as u32,
            height: self.size.height.0 as u32,
        }
    }
}

/// Surface-free renderer used by GPUI visual tests and benchmarks.
pub struct WgpuHeadlessRenderer {
    renderer: WgpuRenderer,
    target: Option<OffscreenTarget>,
    timestamps: Option<super::frame::FrameTimestamps>,
    #[cfg(target_os = "macos")]
    metal_clock: Option<MetalFrameClock>,
}

impl WgpuHeadlessRenderer {
    pub fn new() -> anyhow::Result<Self> {
        Self::from_context(WgpuContext::new_headless(None)?, false)
    }

    /// Benchmark-only GPU timing. Metal brackets the prepared frame with native
    /// queue markers; other backends request optional timestamp queries.
    /// Normal renderer construction does neither.
    pub fn new_with_timestamps() -> anyhow::Result<Self> {
        #[cfg(target_os = "macos")]
        {
            let mut headless = Self::new()?;
            let queue = unsafe {
                headless
                    .renderer
                    .resources()
                    .queue
                    .as_hal::<wgpu::hal::api::Metal>()
            };
            if let Some(ref queue) = queue {
                let raw = queue.as_raw();
                let buffer = raw.device().newBufferWithLength_options(
                    8,
                    objc2_metal::MTLResourceOptions::StorageModeShared,
                );
                if let Some(buffer) = buffer {
                    let queue =
                        unsafe { objc2::rc::Retained::retain(raw as *const _ as *mut _) }.unwrap();
                    headless.metal_clock = Some(MetalFrameClock { queue, buffer });
                }
            }
            drop(queue);
            Ok(headless)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let requirements = crate::WgpuDeviceRequirements {
                features: wgpu::Features::TIMESTAMP_QUERY
                    | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS,
                limits: None,
            };
            match WgpuContext::new_headless(Some(&requirements)) {
                Ok(context) => Self::from_context(context, true),
                Err(_) => Self::new(),
            }
        }
    }

    fn from_context(context: WgpuContext, timing: bool) -> anyhow::Result<Self> {
        let renderer = WgpuRenderer::new_headless(
            &context,
            Size {
                width: DevicePixels(1),
                height: DevicePixels(1),
            },
        )?;
        let timestamps = timing.then(|| {
            let device = &renderer.resources().device;
            super::frame::FrameTimestamps {
                marker: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("gpui_bench_timestamp_marker"),
                    size: 4,
                    usage: wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }),
                queries: device.create_query_set(&wgpu::QuerySetDescriptor {
                    label: Some("gpui_bench_timestamps"),
                    ty: wgpu::QueryType::Timestamp,
                    count: 2,
                }),
                resolved: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("gpui_bench_timestamp_resolve"),
                    size: 16,
                    usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                }),
                readback: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("gpui_bench_timestamp_readback"),
                    size: 16,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                }),
            }
        });
        Ok(Self {
            renderer,
            target: None,
            timestamps,
            #[cfg(target_os = "macos")]
            metal_clock: None,
        })
    }

    /// GPU duration in milliseconds. Metal returns the interval between two
    /// native queue markers around the prepared frame, including inter-buffer
    /// scheduling gaps and possible independent-stage overlap. It is a
    /// diagnostic queue interval, not isolated shader time or GPU occupancy.
    /// Other backends return timestamp-query duration without resolve/readback.
    /// CPU scene preparation and native presentation are excluded.
    pub fn render_scene_timed_and_wait(
        &mut self,
        scene: &Scene,
        size: Size<DevicePixels>,
    ) -> anyhow::Result<Option<f64>> {
        #[cfg(target_os = "macos")]
        if self.metal_clock.is_some() {
            self.ensure_target(size)?;
            let target = self.target.as_ref().unwrap();
            let encoded =
                super::frame::encode_for_headless_timing(&mut self.renderer, scene, &target.view)
                    .ok_or_else(|| anyhow::anyhow!("failed to encode timed frame"))?;
            let clock = self.metal_clock.as_ref().unwrap();
            let before = clock
                .marker()
                .ok_or_else(|| anyhow::anyhow!("failed to create Metal begin marker"))?;
            let after = clock
                .marker()
                .ok_or_else(|| anyhow::anyhow!("failed to create Metal end marker"))?;
            // Prepare all CPU work first, then submit the three buffers without
            // waiting between them. The measured queue interval includes gaps
            // between command buffers and may overlap independent GPU stages;
            // this is a queue diagnostic, not isolated execution or active time.
            before.commit();
            let submission = self.renderer.resources().queue.submit([encoded]);
            after.commit();
            after.waitUntilCompleted();
            self.renderer
                .resources()
                .device
                .poll(wgpu::PollType::Wait {
                    submission_index: Some(submission),
                    timeout: None,
                })
                .map_err(|error| anyhow::anyhow!("failed to wait for timed frame: {error}"))?;
            let start = before.GPUEndTime();
            let end = after.GPUStartTime();
            if start > 0. && end > start {
                return Ok(Some((end - start) * 1000.));
            }
            self.metal_clock = None;
            return Ok(None);
        }
        if self.timestamps.is_none() {
            self.render_scene_and_wait(scene, size)?;
            return Ok(None);
        }
        self.ensure_target(size)?;
        let timestamps = self.timestamps.as_ref().unwrap();
        let target = self.target.as_ref().unwrap();
        let submission =
            super::frame::render_to_view_timed(&mut self.renderer, scene, &target.view, timestamps)
                .ok_or_else(|| anyhow::anyhow!("failed to render timed headless scene"))?;
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        timestamps
            .readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        self.renderer
            .resources()
            .device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: None,
            })
            .map_err(|error| anyhow::anyhow!("failed to wait for timestamps: {error}"))?;
        receiver
            .recv()
            .map_err(|error| anyhow::anyhow!("timestamp callback dropped: {error}"))?
            .map_err(|error| anyhow::anyhow!("failed to map timestamps: {error}"))?;
        let mapped = timestamps.readback.slice(..).get_mapped_range();
        let start = u64::from_le_bytes(mapped[0..8].try_into().unwrap());
        let end = u64::from_le_bytes(mapped[8..16].try_into().unwrap());
        drop(mapped);
        timestamps.readback.unmap();
        // Some drivers advertise timestamp support but return an all-zero pair.
        // Do not turn unavailable instrumentation into a false zero-cost result.
        if start == 0 || end <= start {
            self.timestamps = None;
            return Ok(None);
        }
        let elapsed = end.wrapping_sub(start) as f64
            * self.renderer.resources().queue.get_timestamp_period() as f64
            / 1_000_000.;
        Ok(Some(elapsed))
    }

    pub fn gpu_timing_source(&self) -> &'static str {
        #[cfg(target_os = "macos")]
        if self.metal_clock.is_some() {
            return "Metal marker interval (diagnostic only; gaps and independent-stage overlap)";
        }
        if self.timestamps.is_some() {
            "GPU timestamp queries"
        } else {
            "unavailable"
        }
    }

    pub fn gpu_timing_enabled(&self) -> bool {
        #[cfg(target_os = "macos")]
        if self.metal_clock.is_some() {
            return true;
        }
        self.timestamps.is_some()
    }

    pub fn adapter_description(&self) -> String {
        format!("{:?}", self.renderer.adapter_info)
    }

    fn ensure_target(&mut self, size: Size<DevicePixels>) -> anyhow::Result<()> {
        anyhow::ensure!(
            size.width.0 > 0 && size.height.0 > 0,
            "headless render target must have positive dimensions"
        );
        if self
            .target
            .as_ref()
            .is_some_and(|target| target.size == size)
        {
            return Ok(());
        }
        self.renderer.update_drawable_size(size);
        self.target = Some(self.renderer.create_offscreen_target());
        Ok(())
    }

    /// Renders through the normal submission path and waits for that work to finish.
    pub fn render_scene_and_wait(
        &mut self,
        scene: &Scene,
        size: Size<DevicePixels>,
    ) -> anyhow::Result<()> {
        self.ensure_target(size)?;
        let target = self.target.as_ref().expect("target was just ensured");
        let submission =
            super::frame::render_to_view(&mut self.renderer, scene, &target.view, None)
                .ok_or_else(|| anyhow::anyhow!("failed to render headless scene"))?;
        self.renderer
            .resources()
            .device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: None,
            })
            .map_err(|error| anyhow::anyhow!("failed to wait for headless render: {error}"))?;
        Ok(())
    }
}

impl gpui::PlatformHeadlessRenderer for WgpuHeadlessRenderer {
    fn render_scene_to_image(
        &mut self,
        scene: &Scene,
        size: Size<DevicePixels>,
    ) -> anyhow::Result<image::RgbaImage> {
        self.ensure_target(size)?;
        let target = self.target.as_ref().expect("target was just ensured");
        let submission = self
            .renderer
            .render_to_view_with_readback(scene, &target.view, target.readback_copy())
            .ok_or_else(|| anyhow::anyhow!("failed to render headless scene"))?;
        self.renderer.read_offscreen_target(target, submission)
    }

    fn render_scene(&mut self, scene: &Scene, size: Size<DevicePixels>) -> anyhow::Result<()> {
        self.ensure_target(size)?;
        let target = self.target.as_ref().expect("target was just ensured");
        anyhow::ensure!(
            self.renderer.render_to_view(scene, &target.view),
            "failed to render headless scene"
        );
        Ok(())
    }

    fn sprite_atlas(&self) -> Arc<dyn gpui::PlatformAtlas> {
        self.renderer.sprite_atlas().clone()
    }
}
