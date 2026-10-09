//! Explicit native-GPU regression for the overlapping Gaussian radiance pyramid.
use super::*;
use gpui_wgpu::WgpuContext;
use std::sync::mpsc;

fn reduce_reference(input: &[[f32; 4]], width: u32, height: u32) -> Vec<[f32; 4]> {
    let mut output = Vec::new();
    for y in 0..(height / 2).max(1) {
        for x in 0..(width / 2).max(1) {
            let mut pixel = [0.; 4];
            // Four bilinear shader taps integrate this overlapping 4x4 kernel.
            // Clamp each source coordinate independently, including one-pixel axes.
            for (dy, wy) in [1., 3., 3., 1.].into_iter().enumerate() {
                for (dx, wx) in [1., 3., 3., 1.].into_iter().enumerate() {
                    let sx = (x as i32 * 2 + dx as i32 - 1).clamp(0, width as i32 - 1);
                    let sy = (y as i32 * 2 + dy as i32 - 1).clamp(0, height as i32 - 1);
                    let p = input[(sy as u32 * width + sx as u32) as usize];
                    for (value, sample) in pixel.iter_mut().zip(p) {
                        *value += sample * (wx * wy / 64.);
                    }
                }
            }
            // Each actual GPU level is stored in rgba16float before the next read.
            output.push(pixel.map(|value| half::f16::from_f32(value).to_f32()));
        }
    }
    output
}

// Independent CPU model of the decoded Metal compute stencil and f16 rounding.
fn native_reduce_reference(input: &[[f32; 4]], width: u32, height: u32) -> Vec<[f32; 4]> {
    native_reduce_stage(input, width, height, false)
}
fn native_reduce_stage(input: &[[f32; 4]], width: u32, height: u32, base: bool) -> Vec<[f32; 4]> {
    let half = |v: f32| half::f16::from_f32(v).to_f32();
    let mut output = Vec::new();
    for y in 0..(height / 2).max(1) {
        for x in 0..(width / 2).max(1) {
            let sample = |dx: i32, dy: i32| -> [f32; 4] {
                let sx = x as i32 * 2 + dx;
                let sy = y as i32 * 2 + dy;
                let mut value = [0.; 4];
                for ox in 0..2 {
                    for oy in 0..2 {
                        let at = ((sy + oy).clamp(0, height as i32 - 1) as u32 * width
                            + (sx + ox).clamp(0, width as i32 - 1) as u32)
                            as usize;
                        for (v, s) in value.iter_mut().zip(input[at]) {
                            *v += s * 0.25;
                        }
                    }
                }
                if base {
                    let get = |ox: i32, oy: i32| {
                        input[((sy + oy).clamp(0, height as i32 - 1) as u32 * width
                            + (sx + ox).clamp(0, width as i32 - 1) as u32)
                            as usize]
                    };
                    let [bl, br, tr, tl] = [get(0, 1), get(1, 1), get(1, 0), get(0, 0)];
                    for c in 0..4 {
                        value[c] = half(half(half(half(bl[c] + br[c]) + tr[c]) + tl[c]) * 0.25);
                    }
                }
                value.map(half)
            };
            let taps = [
                sample(0, 0),
                sample(-2, -2),
                sample(2, -2),
                sample(-2, 2),
                sample(2, 2),
                sample(0, -2),
                sample(-2, 0),
                sample(2, 0),
                sample(0, 2),
                sample(0, -4),
                sample(-4, 0),
                sample(4, 0),
                sample(0, 4),
            ];
            let mut value = [0.; 4];
            for c in 0..4 {
                let four = |at: usize| {
                    half(
                        half(half(taps[at][c] + taps[at + 1][c]) + taps[at + 2][c])
                            + taps[at + 3][c],
                    )
                };
                let center = half(taps[0][c] * 0.10546875);
                let diagonal = half(four(1) * 0.0770874);
                let axis = half(four(5) * 0.09020996);
                let outer = half(four(9) * 0.05633545);
                value[c] = half(half(half(diagonal + center) + axis) + outer);
                if base && c < 3 && value[c].abs() < 0.000100016594 {
                    value[c] = 0.;
                }
            }
            output.push(value);
        }
    }
    output
}

#[test]
#[ignore = "requires native GPU; run explicitly"]
fn gaussian_radiance_mips_match_odd_and_skinny_reference() -> anyhow::Result<()> {
    check_mips(false)
}

#[test]
#[ignore = "requires native GPU; run explicitly"]
fn native_bleed_mips_match_odd_and_skinny_reference() -> anyhow::Result<()> {
    check_mips(true)
}

fn check_mips(native: bool) -> anyhow::Result<()> {
    let reduce_reference = if native {
        native_reduce_reference
    } else {
        reduce_reference
    };
    let context = WgpuContext::new_headless(None)?;
    let device = &context.device;
    let queue = &context.queue;
    device.on_uncaptured_error(Arc::new(|error| {
        eprintln!("Gaussian regression GPU error: {error}");
    }));
    let gpu = Gpu::new(device, wgpu::TextureFormat::Bgra8Unorm);
    for (source_width, source_height) in [
        (35u32, 7u32),
        (7, 35),
        (1, 67),
        (67, 1),
        (27, 23),
        (23, 27),
        (1, 1),
        (2, 1),
        (11, 2),
        (47, 39),
        (2, 2),
        (3, 3),
    ] {
        // Mip dimensions floor odd sizes, but the overlapping footprint may
        // still read their last row/column. Skinny axes remain one texel wide.
        let width = (source_width / 2).max(1);
        let height = (source_height / 2).max(1);
        let mut input = Vec::new();
        let mut encoded = Vec::new();
        for y in 0..source_height {
            for x in 0..source_width {
                let alpha = ((x + y) % 4 + 1) as f32 / 4.;
                let pixel = [
                    ((x * 7 + y * 3) % 64) as f32 / 64. * alpha,
                    ((x * 11 + y * 5) % 64) as f32 / 64. * alpha,
                    ((x * 3 + y * 13) % 64) as f32 / 64. * alpha,
                    alpha,
                ];
                input.push(pixel);
                encoded.extend(pixel.map(|x| half::f16::from_f32(x).to_bits()));
            }
        }
        let source = gpu.texture(
            "mip-test-source",
            source_width,
            source_height,
            wgpu::TextureFormat::Rgba16Float,
        );
        queue.write_texture(
            source._texture.as_image_copy(),
            bytemuck::cast_slice(&encoded),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(source_width * 8),
                rows_per_image: Some(source_height),
            },
            source._texture.size(),
        );
        let mip_count = 32 - width.max(height).leading_zeros();
        let output = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("mip-test-output"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: mip_count,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let views: Vec<_> = (0..mip_count)
            .map(|mip| {
                output.create_view(&wgpu::TextureViewDescriptor {
                    base_mip_level: mip,
                    mip_level_count: Some(1),
                    ..Default::default()
                })
            })
            .collect();
        let uniform = gpu.uniform();
        queue.write_buffer(
            &uniform,
            0,
            bytemuck::bytes_of(&Uniforms {
                bleed_crop: [0., 0., source_width as f32, source_height as f32],
                bleed_damage: [0., 0., source_width as f32, source_height as f32],
                ..Uniforms::zeroed()
            }),
        );
        // Match production: one overlapping Gaussian mip per dispatch,
        // sampling the previous level. Readback validates the entire chain,
        // including odd dimensions and axes that reach one texel first.
        let bindings: Vec<_> = (0..views.len())
            .map(|mip| {
                let input = if mip == 0 {
                    &source.view
                } else {
                    &views[mip - 1]
                };
                gpu.reflection_bind(&uniform, input, &source.view, &views[mip..mip + 1], None)
            })
            .collect();
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(if native {
                &gpu.bleed_reduce
            } else {
                &gpu.reflection_reduce
            });
            for (mip, bindings) in bindings.iter().enumerate() {
                pass.set_bind_group(0, bindings, &[]);
                pass.dispatch_workgroups(
                    (width >> mip).max(1).div_ceil(8),
                    (height >> mip).max(1).div_ceil(8),
                    1,
                );
            }
        }
        let mut layouts = Vec::new();
        let mut bytes = 0u64;
        for mip in 0..mip_count {
            let w = (width >> mip).max(1);
            let h = (height >> mip).max(1);
            let row_bytes = (w * 8).div_ceil(256) * 256;
            layouts.push((bytes, row_bytes, w, h));
            bytes += u64::from(row_bytes * h);
        }
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mip-test-readback"),
            size: bytes,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        for (mip, &(offset, row_bytes, w, h)) in layouts.iter().enumerate() {
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: &output,
                    mip_level: mip as u32,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &readback,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset,
                        bytes_per_row: Some(row_bytes),
                        rows_per_image: Some(h),
                    },
                },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );
        }
        let submission = queue.submit([encoder.finish()]);
        let (send, receive) = mpsc::sync_channel(1);
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = send.send(result);
            });
        device.poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: None,
        })?;
        receive.recv()??;
        let mapped = readback.slice(..).get_mapped_range();
        let mut reference = if native {
            native_reduce_stage(&input, source_width, source_height, true)
        } else {
            reduce_reference(&input, source_width, source_height)
        };
        for (mip, &(offset, row_bytes, w, h)) in layouts.iter().enumerate() {
            let mut actual_level = vec![[0.; 4]; (w * h) as usize];
            for y in 0..h {
                for x in 0..w {
                    let start = (offset + u64::from(y * row_bytes + x * 8)) as usize;
                    let values: &[u16] = bytemuck::cast_slice(&mapped[start..start + 8]);
                    for (channel, &bits) in values.iter().enumerate() {
                        let actual = half::f16::from_bits(bits).to_f32();
                        actual_level[(y * w + x) as usize][channel] = actual;
                        let expected = reference[(y * w + x) as usize][channel];
                        assert!(
                            (actual - expected).abs() <= 0.001,
                            "{width}x{height} mip{mip} at({x},{y}) channel{channel}: {actual} != {expected}"
                        );
                    }
                }
            }
            // Isolate each half-arithmetic stage from accumulated hardware rounding.
            reference = reduce_reference(if native { &actual_level } else { &reference }, w, h);
        }
        drop(mapped);
        readback.unmap();
    }
    Ok(())
}
