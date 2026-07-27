use crate::state::State;
use smithay::desktop::{Space, Window, layer_map_for_output};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::protocol::{wl_seat, wl_surface::WlSurface};
use smithay::utils::{SERIAL_COUNTER, Serial};
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
    XdgToplevelSurfaceData,
};

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let window = Window::new_wayland_window(surface);

        // Fill the space left after layer-shell panels claim their exclusive
        // zones, rather than the whole output. Both the suggested size and the
        // map location come from the non-exclusive zone so the window neither
        // sits under a top/left bar nor overlaps it. The size rides along on the
        // initial configure sent from `handle_commit`.
        let loc = if let Some(output) = self.space.outputs().next().cloned() {
            let zone = layer_map_for_output(&output).non_exclusive_zone();
            window.toplevel().unwrap().with_pending_state(|state| {
                state.size = Some(zone.size);
            });
            zone.loc
        } else {
            (0, 0).into()
        };

        self.space.map_element(window.clone(), loc, true);
        self.focus_window(&window);
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let window = self
            .space
            .elements()
            .find(|w| w.toplevel() == Some(&surface))
            .cloned();
        if let Some(window) = window {
            self.space.unmap_elem(&window);
        }
        self.focus_topmost();
    }

    // Popups are out of scope for the toplevel-only shell; the following are
    // required by the trait, so they are provided as no-ops.
    fn new_popup(&mut self, _surface: PopupSurface, _positioner: PositionerState) {}

    fn grab(&mut self, _surface: PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {}

    fn reposition_request(
        &mut self,
        _surface: PopupSurface,
        _positioner: PositionerState,
        _token: u32,
    ) {
    }
}

impl State {
    /// Give keyboard focus to `window`, raise it to the top of the stack and
    /// mark it (and only it) as activated, redrawing every already-configured
    /// toplevel with its new activation state.
    pub fn focus_window(&mut self, window: &Window) {
        self.space.raise_element(window, true);

        let target = window.toplevel().unwrap().wl_surface().clone();
        let toplevels: Vec<ToplevelSurface> = self
            .space
            .elements()
            .map(|element| element.toplevel().unwrap().clone())
            .collect();
        for toplevel in toplevels {
            let activated = toplevel.wl_surface() == &target;
            toplevel.with_pending_state(|state| {
                if activated {
                    state.states.set(xdg_toplevel::State::Activated);
                } else {
                    state.states.unset(xdg_toplevel::State::Activated);
                }
            });
            // The freshly mapped toplevel gets its initial configure (carrying
            // both size and activation) from `handle_commit`; only nudge
            // windows that have already seen one.
            if toplevel.is_initial_configure_sent() {
                toplevel.send_pending_configure();
            }
        }

        let keyboard = self.seat.get_keyboard().unwrap();
        let serial = SERIAL_COUNTER.next_serial();
        keyboard.set_focus(self, Some(target), serial);
    }

    /// Resize and reposition every open toplevel to fill the output's current
    /// non-exclusive zone (the area left over once layer-shell panels have
    /// claimed their exclusive strips). Called when the zone changes: a new
    /// toplevel maps, the output resizes, or a layer surface commits/arranges.
    pub fn reflow_toplevels(&mut self) {
        let Some(output) = self.space.outputs().next().cloned() else {
            return;
        };
        let zone = layer_map_for_output(&output).non_exclusive_zone();

        let windows: Vec<Window> = self.space.elements().cloned().collect();
        for window in windows {
            let toplevel = window.toplevel().unwrap();
            toplevel.with_pending_state(|state| {
                state.size = Some(zone.size);
            });
            if toplevel.is_initial_configure_sent() {
                toplevel.send_pending_configure();
            }
            // Re-map at the zone origin without changing the stacking order.
            self.space.map_element(window, zone.loc, false);
        }
    }

    /// Focus the topmost remaining toplevel, if any.
    pub fn focus_topmost(&mut self) {
        let topmost = self.space.elements().next_back().cloned();
        if let Some(window) = topmost {
            self.focus_window(&window);
        } else {
            let keyboard = self.seat.get_keyboard().unwrap();
            let serial = SERIAL_COUNTER.next_serial();
            keyboard.set_focus(self, Option::<WlSurface>::None, serial);
        }
    }
}

/// Should be called on `WlSurface::commit`. Sends the initial configure to a
/// toplevel the first time it commits its role.
pub fn handle_commit(space: &Space<Window>, surface: &WlSurface) {
    if let Some(window) = space
        .elements()
        .find(|w| w.toplevel().unwrap().wl_surface() == surface)
    {
        let initial_configure_sent = with_states(surface, |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .unwrap()
                .lock()
                .unwrap()
                .initial_configure_sent
        });

        if !initial_configure_sent {
            window.toplevel().unwrap().send_configure();
        }
    }
}
