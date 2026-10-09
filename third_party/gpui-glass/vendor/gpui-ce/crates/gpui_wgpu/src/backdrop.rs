//! Opt-in backdrop callback. The application owns shader resources, GPUI owns scene ordering.
use std::sync::Arc;

/// GPU access for one backdrop draw. Source and target alias: sample the source only in a
/// separate pass into application-owned scratch storage before compositing to the target.
pub struct BackdropContext<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub encoder: &'a mut wgpu::CommandEncoder,
    pub scene: &'a wgpu::TextureView,
    pub format: wgpu::TextureFormat,
    pub viewport: [u32; 2],
    /// Unique command-frame identity; changes even when the target is reused.
    pub frame_id: u64,
    /// Monotonic backdrop position within this frame.
    pub sequence: u64,
    /// Conservative changed region since the previous backdrop: left/top/right/bottom.
    pub source_damage: [f32; 4],
    pub filter: &'a gpui::BackdropFilter,
}

/// Implementations must encode into the supplied encoder, never submit/wait/read back.
/// Resources must be recreated when `device` changes, and respect the filter content mask.
pub trait WgpuBackdrop: Send + Sync + 'static {
    fn paint(&self, context: BackdropContext<'_>);
    /// Conservative output bounds. Unknown effects invalidate the complete content mask.
    fn damage_bounds(&self, filter: &gpui::BackdropFilter) -> gpui::Bounds<gpui::ScaledPixels> {
        filter.content_mask.bounds
    }
}

/// Typed envelope used by the WGPU backend; other backends fall back to the ordinary chain.
#[derive(Clone)]
pub struct WgpuBackdropEffect(pub Arc<dyn WgpuBackdrop>);

impl WgpuBackdropEffect {
    pub fn into_gpui(self) -> gpui::CustomBackdrop {
        gpui::CustomBackdrop(Arc::new(self))
    }
}
