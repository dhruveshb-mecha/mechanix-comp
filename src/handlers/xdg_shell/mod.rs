pub mod decoration;

use crate::backend::Backend;
use crate::state::State;
use smithay::desktop::{
    PopupKeyboardGrab, PopupKind, PopupPointerGrab, PopupUngrabStrategy, Window, WindowSurfaceType,
    find_popup_root_surface, get_popup_toplevel_coords, layer_map_for_output,
};
use smithay::input::{Seat, pointer::Focus};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::protocol::{wl_output, wl_seat, wl_surface::WlSurface};
use smithay::utils::{Point, SERIAL_COUNTER, Serial};
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
    XdgToplevelSurfaceData,
};

impl<BackendData: Backend + 'static> XdgShellHandler for State<BackendData> {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let window = Window::new_wayland_window(surface.clone());

        let loc = if let Some(output) = self.space.outputs().next().cloned() {
            let zone = layer_map_for_output(&output).non_exclusive_zone();
            if surface.parent().is_some() {
                // Dialog with a parent — constrain to the zone but don't fill
                // it. The dialog will be centered once it commits its buffer.
                surface.with_pending_state(|state| {
                    state.bounds = Some(zone.size);
                });
                (0, 0).into()
            } else {
                surface.with_pending_state(|state| {
                    state.size = Some(zone.size);
                });
                zone.loc
            }
        } else {
            (0, 0).into()
        };

        surface.send_configure();

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

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        self.unconstrain_popup(&surface);
        let _ = self.popups.track_popup(PopupKind::Xdg(surface));
    }

    fn grab(&mut self, surface: PopupSurface, seat: wl_seat::WlSeat, serial: Serial) {
        let seat = Seat::from_resource(&seat).unwrap();
        let kind = PopupKind::Xdg(surface);
        let Ok(root) = find_popup_root_surface(&kind) else {
            return;
        };

        let ret = self.popups.grab_popup(root, kind, &seat, serial);

        if let Ok(mut grab) = ret {
            if let Some(keyboard) = seat.get_keyboard() {
                if keyboard.is_grabbed()
                    && !(keyboard.has_grab(serial)
                        || grab.previous_serial().is_none_or(|s| keyboard.has_grab(s)))
                {
                    grab.ungrab(PopupUngrabStrategy::All);
                    return;
                }
                keyboard.set_focus(self, grab.current_grab(), serial);
                keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
            }
            if let Some(pointer) = seat.get_pointer() {
                if pointer.is_grabbed()
                    && !(pointer.has_grab(serial)
                        || grab.previous_serial().is_none_or(|s| pointer.has_grab(s)))
                {
                    grab.ungrab(PopupUngrabStrategy::All);
                    return;
                }
                pointer.set_grab(self, PopupPointerGrab::new(&grab), serial, Focus::Keep);
            }
        }
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        self.unconstrain_popup(&surface);
        surface.send_repositioned(token);
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        if surface.parent().is_some() {
            return;
        }
        let zone = self
            .space
            .outputs()
            .next()
            .map(|o| layer_map_for_output(o).non_exclusive_zone());
        if let Some(zone) = zone {
            surface.with_pending_state(|state| {
                state.size = Some(zone.size);
                state.states.set(xdg_toplevel::State::Maximized);
            });
        }
        surface.send_configure();
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        // Windows cannot be un-maximized. Re-confirm maximized state.
        let zone = self
            .space
            .outputs()
            .next()
            .map(|o| layer_map_for_output(o).non_exclusive_zone());
        if let Some(zone) = zone {
            surface.with_pending_state(|state| {
                state.size = Some(zone.size);
                state.states.set(xdg_toplevel::State::Maximized);
            });
        }
        surface.send_configure();
    }

    fn fullscreen_request(
        &mut self,
        surface: ToplevelSurface,
        _output: Option<wl_output::WlOutput>,
    ) {
        if surface.parent().is_some() {
            return;
        }
        let output_geo = self
            .space
            .outputs()
            .next()
            .and_then(|o| self.space.output_geometry(o));
        if let Some(geo) = output_geo {
            surface.with_pending_state(|state| {
                state.size = Some(geo.size);
                state.states.set(xdg_toplevel::State::Fullscreen);
            });
        }
        surface.send_configure();
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        // Return to maximized state.
        let zone = self
            .space
            .outputs()
            .next()
            .map(|o| layer_map_for_output(o).non_exclusive_zone());
        if let Some(zone) = zone {
            surface.with_pending_state(|state| {
                state.size = Some(zone.size);
                state.states.unset(xdg_toplevel::State::Fullscreen);
                state.states.set(xdg_toplevel::State::Maximized);
            });
        }
        surface.send_configure();
    }
}

impl<BackendData: Backend + 'static> State<BackendData> {
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
            if toplevel.is_initial_configure_sent() {
                toplevel.send_pending_configure();
            }
        }

        let keyboard = self.seat.get_keyboard().unwrap();
        let serial = SERIAL_COUNTER.next_serial();
        keyboard.set_focus(self, Some(target), serial);
    }

    pub fn reflow_toplevels(&mut self) {
        let Some(output) = self.space.outputs().next().cloned() else {
            return;
        };
        let zone = layer_map_for_output(&output).non_exclusive_zone();

        let windows: Vec<Window> = self.space.elements().cloned().collect();
        for window in windows {
            let toplevel = window.toplevel().unwrap();
            if toplevel.parent().is_some() {
                continue;
            }
            toplevel.with_pending_state(|state| {
                state.size = Some(zone.size);
            });
            if toplevel.is_initial_configure_sent() {
                toplevel.send_pending_configure();
            }
            self.space.map_element(window, zone.loc, false);
        }
    }

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

    pub fn center_child_toplevel(&mut self, window: &Window) {
        let Some(toplevel) = window.toplevel() else {
            return;
        };
        let Some(parent_surface) = toplevel.parent() else {
            return;
        };

        let Some(parent_window) = self
            .space
            .elements()
            .find(|w| w.toplevel().unwrap().wl_surface() == &parent_surface)
        else {
            return;
        };

        let Some(parent_geo) = self.space.element_geometry(parent_window) else {
            return;
        };
        let child_geo = window.geometry();

        let new_pos = Point::from((
            parent_geo.loc.x + (parent_geo.size.w - child_geo.size.w) / 2,
            parent_geo.loc.y + (parent_geo.size.h - child_geo.size.h) / 2,
        ));
        self.space.map_element(window.clone(), new_pos, false);
    }

    fn unconstrain_popup(&self, popup: &PopupSurface) {
        let Ok(root) = find_popup_root_surface(&PopupKind::Xdg(popup.clone())) else {
            return;
        };
        let Some(window) = self
            .space
            .elements()
            .find(|w| w.toplevel().unwrap().wl_surface() == &root)
        else {
            return;
        };

        let Some(output) = self.space.outputs().next() else {
            return;
        };
        let Some(output_geo) = self.space.output_geometry(output) else {
            return;
        };
        let Some(window_geo) = self.space.element_geometry(window) else {
            return;
        };

        let mut target = output_geo;
        target.loc -= get_popup_toplevel_coords(&PopupKind::Xdg(popup.clone()));
        target.loc -= window_geo.loc;

        popup.with_pending_state(|state| {
            state.geometry = state.positioner.get_unconstrained_geometry(target);
        });
    }

    pub fn unconstrain_layer_popup(&self, popup: &PopupSurface) {
        let Ok(root) = find_popup_root_surface(&PopupKind::Xdg(popup.clone())) else {
            eprintln!("[unconstrain_layer] root not found");
            return;
        };
        eprintln!("[unconstrain_layer] root={:?}", root);
        let Some(output) = self.space.outputs().find(|o| {
            layer_map_for_output(o)
                .layer_for_surface(&root, WindowSurfaceType::TOPLEVEL)
                .is_some()
        }) else {
            eprintln!("[unconstrain_layer] output not found for root");
            return;
        };
        let Some(output_geo) = self.space.output_geometry(output) else {
            return;
        };

        let map = layer_map_for_output(output);
        let Some(layer) = map.layer_for_surface(&root, WindowSurfaceType::TOPLEVEL) else {
            eprintln!("[unconstrain_layer] layer not found in map");
            return;
        };
        let Some(layer_geo) = map.layer_geometry(layer) else {
            eprintln!("[unconstrain_layer] layer_geo not found");
            return;
        };
        eprintln!(
            "[unconstrain_layer] layer_geo={:?} output_geo={:?}",
            layer_geo, output_geo
        );

        let mut target = output_geo;
        target.loc -= get_popup_toplevel_coords(&PopupKind::Xdg(popup.clone()));
        target.loc -= layer_geo.loc;

        popup.with_pending_state(|state| {
            state.geometry = state.positioner.get_unconstrained_geometry(target);
            eprintln!("[unconstrain_layer] final geometry={:?}", state.geometry);
        });
    }
}

pub fn handle_commit<BackendData: Backend + 'static>(
    state: &mut State<BackendData>,
    surface: &WlSurface,
) {
    if let Some(window) = state
        .space
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
            let toplevel = window.toplevel().unwrap();
            if toplevel.parent().is_none() {
                // Auto-maximize non-dialog toplevels on first commit.
                // By now set_parent() has been called if this is a dialog.
                let zone = state
                    .space
                    .outputs()
                    .next()
                    .map(|o| layer_map_for_output(o).non_exclusive_zone());
                if let Some(zone) = zone {
                    toplevel.with_pending_state(|pending| {
                        pending.size = Some(zone.size);
                        pending.states.set(xdg_toplevel::State::Maximized);
                    });
                }
            }
            toplevel.send_configure();
        }
    }
}
