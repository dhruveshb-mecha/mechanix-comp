use crate::state::State;
use smithay::desktop::{LayerSurface, WindowSurfaceType, layer_map_for_output};
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::SERIAL_COUNTER;
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::wlr_layer::{
    Layer, LayerSurface as WlrLayerSurface, LayerSurfaceData, WlrLayerShellHandler,
    WlrLayerShellState,
};

impl WlrLayerShellHandler for State {
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
        // Layer surfaces belong to an output's `LayerMap`, not the `Space`.
        // Honour the client's requested output, falling back to the single
        // winit output. Keyboard focus and the initial configure are deferred
        // to the commit path (`handle_commit`), where the double-buffered
        // interactivity state has actually been applied.
        let output = wl_output
            .as_ref()
            .and_then(Output::from_resource)
            .or_else(|| self.space.outputs().next().cloned());
        if let Some(output) = output {
            let mut map = layer_map_for_output(&output);
            map.map_layer(&LayerSurface::new(surface, namespace)).unwrap();
        }
    }

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        // Just unmap; keyboard focus and toplevel geometry are left as-is.
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
    }
}

impl State {
    /// Give keyboard focus to a layer surface. Layer surfaces are their own
    /// keyboard-focus target (`KeyboardFocus = WlSurface`), so this is a plain
    /// `set_focus` on the surface.
    fn focus_layer_surface(&mut self, surface: WlSurface) {
        let keyboard = self.seat.get_keyboard().unwrap();
        let serial = SERIAL_COUNTER.next_serial();
        keyboard.set_focus(self, Some(surface), serial);
    }
}

/// Should be called on `WlSurface::commit`. Handles the layer-surface side of a
/// commit: arranges the map, sends the initial configure the first time, and —
/// once the surface is configured and its interactivity is known — grabs
/// keyboard focus if the surface requested it. Returns `true` if `surface` was
/// a layer surface (so the caller can skip the toplevel path).
pub fn handle_commit(state: &mut State, surface: &WlSurface) -> bool {
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

    // Arrange the layers (recomputing exclusive zones) before configuring, so
    // the client is told the size it will actually get. `arrange` reports
    // whether the layout actually changed, so per-frame content commits that
    // leave the zone untouched don't trigger a needless toplevel reflow.
    let (layout_changed, needs_configure, wants_focus) = {
        let mut map = layer_map_for_output(&output);
        let layout_changed = map.arrange();
        let layer = map
            .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
            .unwrap();
        (
            layout_changed,
            !initial_configure_sent,
            layer.can_receive_keyboard_focus(),
        )
    };

    if needs_configure {
        let map = layer_map_for_output(&output);
        let layer = map
            .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
            .unwrap();
        layer.layer_surface().send_configure();
    }

    // Reflow the toplevels into the new non-exclusive zone only when the layer
    // arrangement actually moved (a panel mapped, resized, or changed its
    // exclusive zone).
    if layout_changed {
        state.reflow_toplevels();
    }

    // Grab keyboard focus for launchers/panels that asked for it (Exclusive or
    // OnDemand). Only meaningful once the surface has been configured.
    if !initial_configure_sent && wants_focus {
        state.focus_layer_surface(surface.clone());
    }

    true
}
