use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::{ImportAll, ImportMem};
use smithay::desktop::space::{SpaceRenderElements, space_render_elements};
use smithay::desktop::{Space, Window};
use smithay::output::Output;
use smithay::wayland::session_lock::LockSurface;

// Background shown behind normal desktop contents.
pub const CLEAR_COLOR: [f32; 4] = [0.1, 0.1, 0.1, 1.0];
// Solid red fallback drawn while the session is locked but no lock surface has
// produced content yet (or the client crashed).
pub const CLEAR_COLOR_LOCKED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];

smithay::backend::renderer::element::render_elements! {
    pub OutputElements<R, E> where R: ImportAll + ImportMem;
    Space = SpaceRenderElements<R, E>,
    Surface = WaylandSurfaceRenderElement<R>,
}

/// Concrete element type used by both backends: the compositor always renders
/// with a `GlesRenderer`, and every element ultimately resolves to a
/// `WaylandSurfaceRenderElement`.
pub type Element = OutputElements<GlesRenderer, WaylandSurfaceRenderElement<GlesRenderer>>;

/// Build the full list of render elements for `output` plus the clear color,
/// shared by every backend so the "what to draw" policy lives in one place.
/// Each backend owns "how to present" (winit binds a framebuffer, udev drives a
/// DRM swapchain).
pub fn output_elements(
    renderer: &mut GlesRenderer,
    space: &Space<Window>,
    output: &Output,
    is_locked: bool,
    lock_surfaces: &[LockSurface],
) -> (Vec<Element>, [f32; 4]) {
    if is_locked {
        // Only render live lock surfaces. Dead surfaces (client crashed) produce
        // no elements and fall back to the solid red clear color.
        let mut elements = Vec::new();
        for lock_surface in lock_surfaces.iter().filter(|s| s.alive()) {
            elements.extend(
                render_elements_from_surface_tree::<GlesRenderer, WaylandSurfaceRenderElement<GlesRenderer>>(
                    renderer,
                    lock_surface.wl_surface(),
                    (0, 0),
                    1.0,
                    1.0,
                    Kind::Unspecified,
                )
                .into_iter()
                .map(OutputElements::Surface),
            );
        }
        (elements, CLEAR_COLOR_LOCKED)
    } else {
        let elements = space_render_elements(renderer, [space], output, 1.0)
            .unwrap_or_default()
            .into_iter()
            .map(OutputElements::Space)
            .collect();
        (elements, CLEAR_COLOR)
    }
}
