//! GPU SDF regeneration for a fixed allocation containing a changing real contour.
use gpui_wgpu::wgpu;
pub(crate) struct Pipeline {
    pub pipeline: wgpu::ComputePipeline,
}
pub(crate) struct State {
    buffer: wgpu::Buffer,
    bind: wgpu::BindGroup,
    last: [u32; 2],
}
impl Pipeline {
    pub fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("dynamic-lisse-sdf"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/dynamic_shape.wgsl").into()),
        });
        Self {
            pipeline: device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("dynamic-lisse-sdf"),
                layout: None,
                module: &shader,
                entry_point: Some("build"),
                compilation_options: Default::default(),
                cache: None,
            }),
        }
    }
    pub fn state(&self, device: &wgpu::Device, view: &wgpu::TextureView) -> State {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dynamic-contour"),
            size: 16 + 1024 * 16,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("dynamic-contour"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(view),
                },
            ],
        });
        State {
            buffer,
            bind,
            last: [u32::MAX; 2],
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &self,
        state: &mut State,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        shape: crate::Shape,
        smoothing: f32,
        logical: [f32; 2],
        fraction: [f32; 2],
        pad: f32,
        extent: [u32; 2],
    ) -> bool {
        let key = fraction.map(f32::to_bits);
        if key == state.last {
            return false;
        }
        state.last = key;
        let dimensions = [
            logical[0] * fraction[0].clamp(0.05, 1.),
            logical[1] * fraction[1].clamp(0.05, 1.),
        ];
        let points = crate::shape::outline(shape, dimensions[0], dimensions[1], smoothing);
        let points = simplify(&points, 0.05);
        assert!(
            points.len() <= 1024,
            "dynamic contour exceeds segment buffer"
        );
        let offset = [
            (logical[0] - dimensions[0]) * 0.5,
            (logical[1] - dimensions[1]) * 0.5,
        ];
        let mut data = Vec::<[f32; 4]>::with_capacity(points.len() + 1);
        data.push([points.len() as f32, pad, 0., 0.]);
        for (i, a) in points.iter().enumerate() {
            let b = points[(i + 1) % points.len()];
            data.push([
                a[0] + offset[0],
                a[1] + offset[1],
                b[0] + offset[0],
                b[1] + offset[1],
            ]);
        }
        queue.write_buffer(&state.buffer, 0, bytemuck::cast_slice(&data));
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("regenerate-lisse-distance"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &state.bind, &[]);
        pass.dispatch_workgroups(extent[0].div_ceil(8), extent[1].div_ceil(8), 1);
        true
    }
}
// Ramer-Douglas-Peucker on each half of a closed contour. The maximum deviation
// from Lisse's flattened outline is bounded in logical pixels, not vertex count.
fn simplify(points: &[[f32; 2]], epsilon: f32) -> Vec<[f32; 2]> {
    fn visit(p: &[[f32; 2]], out: &mut Vec<[f32; 2]>, e: f32) {
        if p.len() < 3 {
            out.push(p[0]);
            return;
        }
        let a = p[0];
        let b = p[p.len() - 1];
        let d = [b[0] - a[0], b[1] - a[1]];
        let len = d[0] * d[0] + d[1] * d[1];
        let (mut index, mut maximum) = (0, 0.);
        for (i, v) in p.iter().enumerate().take(p.len() - 1).skip(1) {
            let q = [v[0] - a[0], v[1] - a[1]];
            let t = ((q[0] * d[0] + q[1] * d[1]) / len.max(1e-12)).clamp(0., 1.);
            let ds = (q[0] - t * d[0]).hypot(q[1] - t * d[1]);
            if ds > maximum {
                maximum = ds;
                index = i;
            }
        }
        if maximum > e {
            visit(&p[..=index], out, e);
            visit(&p[index..], out, e);
        } else {
            out.push(a);
        }
    }
    if points.len() < 4 {
        return points.to_vec();
    }
    let mut closed = points.to_vec();
    closed.push(points[0]);
    let mid = points.len() / 2;
    let mut out = Vec::new();
    visit(&closed[..=mid], &mut out, epsilon);
    visit(&closed[mid..], &mut out, epsilon);
    out
}
