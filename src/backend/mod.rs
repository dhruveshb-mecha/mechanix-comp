use smithay::backend::renderer::gles::GlesRenderer;
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;

pub mod udev;
pub mod winit;

/// Abstraction over the concrete rendering/presentation backend the compositor
/// runs on. Both backends render with the same `GlesRenderer`, so the render
/// element type stays uniform (`WaylandSurfaceRenderElement<GlesRenderer>`);
/// only the presentation path differs.
pub trait Backend {
    /// The renderer used to import client buffers and draw frames.
    fn renderer(&mut self) -> &mut GlesRenderer;

    /// The seat name advertised for this backend.
    fn seat_name(&self) -> String;

    /// Discard any cached swapchain/damage buffers for `output`, e.g. after a
    /// session resume (VT switch back) where the previous framebuffers are stale.
    fn reset_buffers(&mut self, output: &Output);

    /// Opportunity to pre-import a client buffer onto the render GPU before it is
    /// sampled. A no-op with a single-GPU `GlesRenderer`.
    fn early_import(&mut self, surface: &WlSurface) {
        let _ = surface;
    }
}
