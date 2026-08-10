pub mod decoration;

use crate::backend::Backend;
use crate::state::{State, WindowKind, WindowMode, WindowState};
use smithay::desktop::{
    PopupKeyboardGrab, PopupKind, PopupPointerGrab, PopupUngrabStrategy, Window, WindowSurfaceType,
    find_popup_root_surface, get_popup_toplevel_coords, layer_map_for_output,
};
use smithay::input::{Seat, pointer::Focus};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::protocol::{wl_output, wl_seat, wl_surface::WlSurface};
use smithay::utils::{Logical, Point, Rectangle, SERIAL_COUNTER, Serial, Size};
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, SurfaceCachedState, ToplevelSurface, XdgShellHandler,
    XdgShellState, XdgToplevelSurfaceData,
};

impl<BackendData: Backend + 'static> XdgShellHandler for State<BackendData> {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let window = Window::new_wayland_window(surface.clone());

        // `set_parent` hasn't arrived yet, so only set bounds here; the first
        // commit decides sizing/mapping in `handle_commit`.
        if let Some(output) = self.space.outputs().next().cloned() {
            let zone = layer_map_for_output(&output).non_exclusive_zone();
            surface.with_pending_state(|state| {
                state.bounds = Some(zone.size);
            });
        }

        self.toplevels.insert(
            surface.wl_surface().clone(),
            WindowState {
                window: window.clone(),
                kind: WindowKind::Normal,
                mode: WindowMode::Floating,
                mapped: false,
                modal: false,
            },
        );
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let was_transient = matches!(
            self.toplevels.get(surface.wl_surface()).map(|ws| ws.kind),
            Some(WindowKind::Transient(_))
        );
        // The foreign-toplevel `closed` event is sent by
        // `foreign_toplevel_refresh` on the next idle callback.
        self.toplevels.remove(surface.wl_surface());
        if was_transient {
            // Transients never joined the `Space`; nothing to restore.
            return;
        }
        let window = self
            .space
            .elements()
            .find(|w| w.toplevel() == Some(&surface))
            .cloned();
        if let Some(window) = window {
            self.space.unmap_elem(&window);
            self.focus_topmost();
        }
    }

    fn title_changed(&mut self, surface: ToplevelSurface) {
        self.promote_transient(&surface);
        // Title is picked up by `foreign_toplevel_refresh` on the next idle.
    }

    fn app_id_changed(&mut self, surface: ToplevelSurface) {
        self.promote_transient(&surface);
    }

    fn parent_changed(&mut self, surface: ToplevelSurface) {
        self.promote_transient(&surface);
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        // Layer popups get their parent patched in later; they're tracked by
        // `WlrLayerShellHandler::new_popup`, so skip them here.
        if surface.get_parent_surface().is_none() {
            return;
        }
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
        // Dialogs are never maximized.
        if surface.parent().is_some() {
            return;
        }
        // A fullscreen window keeps its mode; the stale-flag re-check would
        // otherwise bounce it to a maximized look.
        if matches!(
            self.toplevels.get(surface.wl_surface()).map(|ws| ws.mode),
            Some(WindowMode::Fullscreen)
        ) {
            return;
        }
        let zone = self
            .space
            .outputs()
            .next()
            .map(|o| layer_map_for_output(o).non_exclusive_zone());
        if let Some(zone) = zone {
            // Fixed-size windows can't be maximized.
            if !fills_zone(surface.wl_surface(), zone.size) {
                return;
            }
            surface.with_pending_state(|state| {
                state.size = Some(zone.size);
                state.states.set(xdg_toplevel::State::Maximized);
            });
            self.toplevels.get_mut(surface.wl_surface()).unwrap().mode = WindowMode::Maximized;
        }
        surface.send_configure();
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        // Dialogs are never maximized, so ignore restore requests.
        if surface.parent().is_some() {
            return;
        }
        // Not maximized (e.g. the spurious restore GTK sends on a floating
        // window): nothing to restore.
        if !matches!(
            self.toplevels.get(surface.wl_surface()).map(|ws| ws.mode),
            Some(WindowMode::Maximized)
        ) {
            return;
        }
        let zone = self
            .space
            .outputs()
            .next()
            .map(|o| layer_map_for_output(o).non_exclusive_zone());
        if let Some(zone) = zone {
            // Fixed-size windows are never maximized.
            if !fills_zone(surface.wl_surface(), zone.size) {
                return;
            }
            // Windows cannot be un-maximized. Re-confirm maximized state.
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
        // Dialogs are never fullscreen.
        if surface.parent().is_some() {
            return;
        }
        if matches!(
            self.toplevels.get(surface.wl_surface()).map(|ws| ws.mode),
            Some(WindowMode::Fullscreen)
        ) {
            return;
        }
        let output_geo = self
            .space
            .outputs()
            .next()
            .and_then(|o| self.space.output_geometry(o));
        if let Some(geo) = output_geo {
            // Fixed-size windows can't be fullscreened.
            if !fills_zone(surface.wl_surface(), geo.size) {
                return;
            }
            surface.with_pending_state(|state| {
                state.size = Some(geo.size);
                state.states.set(xdg_toplevel::State::Fullscreen);
            });
            self.toplevels.get_mut(surface.wl_surface()).unwrap().mode = WindowMode::Fullscreen;
        }
        surface.send_configure();

        // Fullscreen windows stack above every other toplevel and keep the
        // keyboard focus.
        let window = self
            .toplevels
            .get(surface.wl_surface())
            .map(|ws| ws.window.clone());
        if let Some(window) = window {
            self.space.raise_element(&window, false);
            self.focus_window(&window, SERIAL_COUNTER.next_serial());
        }
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        // Not fullscreen (e.g. the `unset_fullscreen` GTK sends on every
        // headerbar drag): nothing to restore.
        if !matches!(
            self.toplevels.get(surface.wl_surface()).map(|ws| ws.mode),
            Some(WindowMode::Fullscreen)
        ) {
            return;
        }
        let zone = self
            .space
            .outputs()
            .next()
            .map(|o| layer_map_for_output(o).non_exclusive_zone());
        if let Some(zone) = zone {
            // Return to maximized state.
            surface.with_pending_state(|state| {
                state.size = Some(zone.size);
                state.states.unset(xdg_toplevel::State::Fullscreen);
                state.states.set(xdg_toplevel::State::Maximized);
            });
            self.toplevels.get_mut(surface.wl_surface()).unwrap().mode = WindowMode::Maximized;
        }
        surface.send_configure();
    }
}

impl<BackendData: Backend + 'static> State<BackendData> {
    pub fn focus_window(&mut self, window: &Window, serial: Serial) {
        // A fullscreen window keeps keyboard focus; other windows can't steal it.
        if let Some(active) = self.active_fullscreen_window()
            && &active != window
        {
            return;
        }
        // Raise + activate (the pending Activated state), then flush to clients.
        self.space.raise_element(window, true);
        for toplevel in self
            .space
            .elements()
            .map(|element| element.toplevel().unwrap().clone())
        {
            if toplevel.is_initial_configure_sent() {
                toplevel.send_pending_configure();
            }
        }

        // Remember the active window so focus can be restored when no layer
        // holds it; focusing a window also drops any on-demand layer focus.
        self.active_window = Some(window.toplevel().unwrap().wl_surface().clone());
        self.layer_shell_on_demand_focus = None;

        self.seat.get_keyboard().unwrap().set_focus(
            self,
            Some(window.toplevel().unwrap().wl_surface().clone()),
            serial,
        );
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
                // Keep dialogs centered over (and stacked above) their parent.
                self.center_child_toplevel(&window);
                continue;
            }
            if fills_zone(toplevel.wl_surface(), zone.size) {
                toplevel.with_pending_state(|state| {
                    state.size = Some(zone.size);
                });
                if toplevel.is_initial_configure_sent() {
                    toplevel.send_pending_configure();
                }
                self.space.relocate_element(&window, zone.loc);
            } else {
                // Fixed-size window: keep it centered, don't re-size it.
                self.space
                    .relocate_element(&window, centered_loc(zone, window.geometry()));
            }
        }
    }

    pub fn focus_topmost(&mut self) {
        let topmost = self.space.elements().next_back().cloned();
        if let Some(window) = topmost {
            self.focus_window(&window, SERIAL_COUNTER.next_serial());
        } else {
            self.active_window = None;
            self.layer_shell_on_demand_focus = None;
            let keyboard = self.seat.get_keyboard().unwrap();
            let serial = SERIAL_COUNTER.next_serial();
            keyboard.set_focus(self, Option::<WlSurface>::None, serial);
        }
    }

    /// Map a toplevel on its first commit, or promote a transient that gained a title/app_id/parent. Returns true if kept as a transient.
    fn handle_toplevel_first_commit(&mut self, surface: &WlSurface, window: &Window) -> bool {
        let toplevel = window.toplevel().unwrap();
        let (title, app_id) = toplevel_title_app_id(surface);

        if toplevel.parent().is_none() && title.is_empty() && app_id.is_empty() {
            // GTK3 tooltip fallback: float at the pointer, never focus/maximize (0x0 configure = client picks its size).
            toplevel.with_pending_state(|pending| {
                pending.size = None;
                pending.states.unset(xdg_toplevel::State::Maximized);
            });
            toplevel.send_configure();
            let loc = self
                .seat
                .get_pointer()
                .map(|p| p.current_location().to_i32_round() + Point::from((12, 24)))
                .unwrap_or_default();
            let ws = self.toplevels.get_mut(surface).unwrap();
            ws.kind = WindowKind::Transient(loc);
            ws.mapped = true;
            return true;
        }

        let zone = self
            .space
            .outputs()
            .next()
            .map(|o| layer_map_for_output(o).non_exclusive_zone());
        let is_dialog = toplevel.parent().is_some();
        let fills = !is_dialog
            && zone
                .as_ref()
                .map(|z| fills_zone(surface, z.size))
                .unwrap_or(true);
        let ws = self.toplevels.get_mut(surface).unwrap();
        ws.kind = if is_dialog {
            WindowKind::Dialog
        } else {
            WindowKind::Normal
        };
        ws.mode = if !is_dialog && fills {
            WindowMode::Maximized
        } else {
            WindowMode::Floating
        };
        if !is_dialog && let Some(zone) = zone {
            toplevel.with_pending_state(|pending| {
                if fills {
                    pending.size = Some(zone.size);
                    pending.states.set(xdg_toplevel::State::Maximized);
                } else {
                    pending.size = None;
                    pending.bounds = Some(zone.size);
                    pending.states.unset(xdg_toplevel::State::Maximized);
                }
            });
        }

        let loc = if is_dialog {
            (0, 0).into()
        } else if let Some(zone) = zone {
            if fills {
                zone.loc
            } else {
                centered_loc(zone, window.geometry())
            }
        } else {
            (0, 0).into()
        };
        // Don't activate/raise a new window over an active fullscreen one.
        let activate = self.active_fullscreen_window().is_none();
        self.space.map_element(window.clone(), loc, activate);
        self.focus_window(window, SERIAL_COUNTER.next_serial());
        self.toplevels.get_mut(surface).unwrap().mapped = true;
        toplevel.send_configure();
        if is_dialog {
            self.center_child_toplevel(window);
        }
        false
    }

    fn promote_transient(&mut self, surface: &ToplevelSurface) {
        let wl_surface = surface.wl_surface();
        let Some(window) = self
            .toplevels
            .get(wl_surface)
            .filter(|ws| matches!(ws.kind, WindowKind::Transient(_)))
            .map(|ws| ws.window.clone())
        else {
            return;
        };
        self.handle_toplevel_first_commit(wl_surface, &window);
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
            .cloned()
        else {
            return;
        };

        let Some(parent_geo) = self.space.element_geometry(&parent_window) else {
            return;
        };
        let child_geo = window.geometry();

        let new_pos = Point::from((
            parent_geo.loc.x + (parent_geo.size.w - child_geo.size.w) / 2,
            parent_geo.loc.y + (parent_geo.size.h - child_geo.size.h) / 2,
        ));
        self.space.relocate_element(window, new_pos);
        // Keep the dialog stacked above its parent (idempotent).
        self.space
            .raise_element_above(window, &parent_window, false);
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
            return;
        };
        let Some(output) = self.space.outputs().find(|o| {
            layer_map_for_output(o)
                .layer_for_surface(&root, WindowSurfaceType::TOPLEVEL)
                .is_some()
        }) else {
            return;
        };
        let Some(output_geo) = self.space.output_geometry(output) else {
            return;
        };

        let map = layer_map_for_output(output);
        let Some(layer) = map.layer_for_surface(&root, WindowSurfaceType::TOPLEVEL) else {
            return;
        };
        let Some(layer_geo) = map.layer_geometry(layer) else {
            return;
        };

        let mut target = output_geo;
        target.loc -= get_popup_toplevel_coords(&PopupKind::Xdg(popup.clone()));
        target.loc -= layer_geo.loc;

        popup.with_pending_state(|state| {
            state.geometry = state.positioner.get_unconstrained_geometry(target);
        });
    }
}

/// The toplevel's current title and app_id.
fn toplevel_title_app_id(surface: &WlSurface) -> (String, String) {
    with_states(surface, |states| {
        let attrs = states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .unwrap()
            .lock()
            .unwrap();
        (
            attrs.title.clone().unwrap_or_default(),
            attrs.app_id.clone().unwrap_or_default(),
        )
    })
}

/// Whether the toplevel's declared max size can fill `zone`. Fixed-size
/// windows (`set_max_size` smaller than the zone) are never maximized.
fn fills_zone(surface: &WlSurface, zone: Size<i32, Logical>) -> bool {
    let max_size = with_states(surface, |states| {
        states
            .cached_state
            .get::<SurfaceCachedState>()
            .current()
            .max_size
    });
    (max_size.w == 0 || max_size.w >= zone.w) && (max_size.h == 0 || max_size.h >= zone.h)
}

/// Center `geo` within `zone`, clamped to keep its top-left inside the zone.
fn centered_loc(
    zone: Rectangle<i32, Logical>,
    geo: Rectangle<i32, Logical>,
) -> Point<i32, Logical> {
    Point::from((
        zone.loc.x + (zone.size.w - geo.size.w).max(0) / 2,
        zone.loc.y + (zone.size.h - geo.size.h).max(0) / 2,
    ))
}

/// Toplevel commit handler: first-commit map, keeps transients in-bounds and dialogs centered. Returns true for toplevels (skip popup path).
pub fn handle_commit<BackendData: Backend + 'static>(
    state: &mut State<BackendData>,
    surface: &WlSurface,
) -> bool {
    let Some((window, mapped)) = state
        .toplevels
        .get(surface)
        .map(|ws| (ws.window.clone(), ws.mapped))
    else {
        return false;
    };

    if !mapped {
        if state.handle_toplevel_first_commit(surface, &window) {
            return true;
        }
    } else if let Some(ws) = state.toplevels.get_mut(surface) {
        if let WindowKind::Transient(loc) = &mut ws.kind
            && let Some(geo) = state
                .space
                .outputs()
                .next()
                .and_then(|o| state.space.output_geometry(o))
        {
            let size = window.geometry().size;
            if size.w <= geo.size.w && size.h <= geo.size.h {
                loc.x = loc.x.clamp(geo.loc.x, geo.loc.x + geo.size.w - size.w);
                loc.y = loc.y.clamp(geo.loc.y, geo.loc.y + geo.size.h - size.h);
            }
        }
    }

    // Re-check the maximize decision once size hints arrive (GTK sends
    // `set_max_size` only after the initial configure); un-maximize and
    // re-center fixed-size windows.
    let toplevel = window.toplevel().unwrap();
    let zone = state
        .space
        .outputs()
        .next()
        .map(|o| layer_map_for_output(o).non_exclusive_zone());
    if toplevel.parent().is_none()
        && toplevel.is_initial_configure_sent()
        && !matches!(
            state.toplevels.get(surface).map(|ws| ws.mode),
            Some(WindowMode::Fullscreen)
        )
        && let Some(zone) = zone
    {
        let fills = fills_zone(surface, zone.size);
        let maximized = toplevel.with_committed_state(|s| {
            s.is_some_and(|s| s.states.contains(xdg_toplevel::State::Maximized))
        });
        if fills != maximized {
            state.toplevels.get_mut(surface).unwrap().mode = if fills {
                WindowMode::Maximized
            } else {
                WindowMode::Floating
            };
            toplevel.with_pending_state(|pending| {
                if fills {
                    pending.size = Some(zone.size);
                    pending.states.set(xdg_toplevel::State::Maximized);
                } else {
                    pending.size = None;
                    pending.bounds = Some(zone.size);
                    pending.states.unset(xdg_toplevel::State::Maximized);
                }
            });
            toplevel.send_pending_configure();
            if !fills {
                state
                    .space
                    .relocate_element(&window, centered_loc(zone, window.geometry()));
            }
        }
    }

    if window.toplevel().unwrap().parent().is_some() {
        // Keep dialogs centered over (and stacked above) their parent.
        state.center_child_toplevel(&window);
    }
    true
}
