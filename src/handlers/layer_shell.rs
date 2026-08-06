use crate::backend::Backend;
use crate::state::State;
use smithay::desktop::{LayerSurface, PopupKind, WindowSurfaceType, layer_map_for_output};
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::wlr_layer::{
    KeyboardInteractivity, Layer, LayerSurface as WlrLayerSurface, LayerSurfaceData,
    WlrLayerShellHandler, WlrLayerShellState,
};
use smithay::wayland::shell::xdg::PopupSurface;

impl<BackendData: Backend + 'static> WlrLayerShellHandler for State<BackendData> {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: WlrLayerSurface,
        wl_output: Option<WlOutput>,
        _layer: Layer,
        namespace: String,
    ) {
        // Map into the output's `LayerMap`; keyboard focus and the initial
        // configure are deferred to `handle_commit`, once the double-buffered
        // interactivity state is applied.
        let output = wl_output
            .as_ref()
            .and_then(Output::from_resource)
            .or_else(|| self.space.outputs().next().cloned());
        if let Some(output) = output {
            let mut map = layer_map_for_output(&output);
            map.map_layer(&LayerSurface::new(surface, namespace))
                .unwrap();
        }
    }

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        // Just unmap; toplevel geometry is left as-is.
        if let Some((mut map, layer)) = self.space.outputs().find_map(|o| {
            let map = layer_map_for_output(o);
            let layer = map
                .layers()
                .find(|&layer| layer.layer_surface() == &surface)
                .cloned();
            layer.map(|layer| (map, layer))
        }) {
            map.unmap_layer(&layer);
        }
        if self.layer_shell_on_demand_focus.as_ref() == Some(surface.wl_surface()) {
            self.layer_shell_on_demand_focus = None;
        }
    }

    fn new_popup(&mut self, _parent: WlrLayerSurface, popup: PopupSurface) {
        self.unconstrain_layer_popup(&popup);
        let _ = self.popups.track_popup(PopupKind::Xdg(popup));
    }
}

/// Layer-surface side of a commit: arrange the map, send the initial configure,
/// and mark newly-mapped `OnDemand` layers (Overlay/Top) so
/// `update_keyboard_focus` gives them keyboard focus. Returns `true` if
/// `surface` was a layer surface (so the caller can skip the toplevel path).
pub fn handle_commit<BackendData: Backend + 'static>(
    state: &mut State<BackendData>,
    surface: &WlSurface,
) -> bool {
    let Some(output) = state
        .space
        .outputs()
        .find(|o| {
            layer_map_for_output(o)
                .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
                .is_some()
        })
        .cloned()
    else {
        return false;
    };

    let initial_configure_sent = with_states(surface, |states| {
        states
            .data_map
            .get::<LayerSurfaceData>()
            .unwrap()
            .lock()
            .unwrap()
            .initial_configure_sent
    });

    // Arrange before configuring so the client gets its real size. Only reflow
    // toplevels when the layout actually moved.
    let (layout_changed, needs_configure, on_demand) = {
        let mut map = layer_map_for_output(&output);
        let layout_changed = map.arrange();
        let layer = map
            .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
            .unwrap();
        // Newly-mapped `OnDemand` layers take keyboard focus, but only on
        // Overlay/Top; Bottom/Background are click-to-focus.
        let cached = layer.cached_state();
        let on_demand = matches!(cached.layer, Layer::Overlay | Layer::Top)
            && cached.keyboard_interactivity == KeyboardInteractivity::OnDemand;
        (layout_changed, !initial_configure_sent, on_demand)
    };

    if needs_configure {
        let map = layer_map_for_output(&output);
        let layer = map
            .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
            .unwrap();
        layer.layer_surface().send_configure();
    }

    // Reflow the toplevels when the layout changed.
    if layout_changed {
        state.reflow_toplevels();
    }

    // Panels/launchers take keyboard focus on open; applied next frame.
    if needs_configure && on_demand {
        state.layer_shell_on_demand_focus = Some(surface.clone());
    }

    true
}
