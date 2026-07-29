use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent,
        KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent,
    },
    desktop::{WindowSurfaceType, layer_map_for_output},
    input::{
        keyboard::FilterResult,
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, SERIAL_COUNTER},
    wayland::shell::wlr_layer::Layer as WlrLayer,
};

use crate::state::State;

impl State {
    /// Find the surface (and its location) under `pos`, in z-order:
    /// Overlay/Top layer surfaces first, then `Space` toplevels, then
    /// Bottom/Background layer surfaces. Layer surfaces live in the output's
    /// `LayerMap`, so they are missed by `Space::element_under` alone.
    pub fn surface_under(
        &self,
        pos: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        let output = self.space.outputs().next()?;
        let output_geo = self.space.output_geometry(output)?;

        if self.is_locked {
            // Find if the pos is within any lock surface.
            for lock_surface in self.lock_surfaces.iter().filter(|s| s.alive()) {
                let surface = lock_surface.wl_surface();
                if let Some((s, p)) = smithay::desktop::utils::under_from_surface_tree(
                    surface,
                    pos - output_geo.loc.to_f64(),
                    (0, 0),
                    WindowSurfaceType::ALL,
                ) {
                    return Some((s.clone(), p.to_f64() + output_geo.loc.to_f64()));
                }
            }
            // If locked, don't fall through to other surfaces.
            return None;
        }

        let map = layer_map_for_output(output);

        let layer_surface_under = |layer: &smithay::desktop::LayerSurface| {
            let layer_loc = map.layer_geometry(layer).unwrap().loc;
            layer
                .surface_under(
                    pos - output_geo.loc.to_f64() - layer_loc.to_f64(),
                    WindowSurfaceType::ALL,
                )
                .map(|(surface, loc)| (surface, (loc + layer_loc + output_geo.loc).to_f64()))
        };

        if let Some(layer) = map
            .layer_under(WlrLayer::Overlay, pos - output_geo.loc.to_f64())
            .or_else(|| map.layer_under(WlrLayer::Top, pos - output_geo.loc.to_f64()))
            && let Some(found) = layer_surface_under(layer)
        {
            return Some(found);
        }

        if let Some(found) = self
            .space
            .element_under(pos)
            .and_then(|(window, location)| {
                window
                    .surface_under(pos - location.to_f64(), WindowSurfaceType::ALL)
                    .map(|(s, p)| (s, (p + location).to_f64()))
            })
        {
            return Some(found);
        }

        if let Some(layer) = map
            .layer_under(WlrLayer::Bottom, pos - output_geo.loc.to_f64())
            .or_else(|| map.layer_under(WlrLayer::Background, pos - output_geo.loc.to_f64()))
            && let Some(found) = layer_surface_under(layer)
        {
            return Some(found);
        }

        None
    }

    pub fn process_input_event<I: InputBackend>(&mut self, event: InputEvent<I>) {
        match event {
            InputEvent::Keyboard { event, .. } => {
                let serial = SERIAL_COUNTER.next_serial();
                let time = Event::time_msec(&event);

                self.seat.get_keyboard().unwrap().input::<(), _>(
                    self,
                    event.key_code(),
                    event.state(),
                    serial,
                    time,
                    |_, _, _| FilterResult::Forward,
                );
            }
            InputEvent::PointerMotion { .. } => {}
            InputEvent::PointerMotionAbsolute { event, .. } => {
                let output = self.space.outputs().next().unwrap();

                let output_geo = self.space.output_geometry(output).unwrap();

                let pos = event.position_transformed(output_geo.size) + output_geo.loc.to_f64();

                let serial = SERIAL_COUNTER.next_serial();

                let pointer = self.seat.get_pointer().unwrap();

                let under = self.surface_under(pos);

                pointer.motion(
                    self,
                    under,
                    &MotionEvent {
                        location: pos,
                        serial,
                        time: event.time_msec(),
                    },
                );
                pointer.frame(self);
            }
            InputEvent::PointerButton { event, .. } => {
                let pointer = self.seat.get_pointer().unwrap();
                let keyboard = self.seat.get_keyboard().unwrap();

                let serial = SERIAL_COUNTER.next_serial();

                let button = event.button_code();

                let button_state = event.state();

                if ButtonState::Pressed == button_state && !pointer.is_grabbed() {
                    let pos = pointer.current_location();
                    if self.is_locked {
                        if let Some((surface, _)) = self.surface_under(pos) {
                            keyboard.set_focus(self, Some(surface), serial);
                        }
                    } else {
                        // An Overlay/Top layer surface sits above every toplevel, so
                        // it wins the click. `Some(Some(surface))` = focusable layer
                        // (OnDemand/Exclusive), `Some(None)` = a layer that declines
                        // keyboard focus (leave focus where it is), `None` = no layer
                        // there, fall through to the toplevels.
                        let top_layer_focus: Option<Option<WlSurface>> =
                            self.space.outputs().next().cloned().and_then(|output| {
                                let output_geo = self.space.output_geometry(&output)?;
                                let map = layer_map_for_output(&output);
                                map.layer_under(WlrLayer::Overlay, pos - output_geo.loc.to_f64())
                                    .or_else(|| {
                                        map.layer_under(
                                            WlrLayer::Top,
                                            pos - output_geo.loc.to_f64(),
                                        )
                                    })
                                    .map(|layer| {
                                        layer
                                            .can_receive_keyboard_focus()
                                            .then(|| layer.wl_surface().clone())
                                    })
                            });

                        if let Some(focus) = top_layer_focus {
                            // Only a layer surface that requested keyboard input
                            // steals focus; a bare panel/wallpaper does not.
                            if let Some(surface) = focus {
                                self.space.elements().for_each(|window| {
                                    window.set_activated(false);
                                    window.toplevel().unwrap().send_pending_configure();
                                });
                                keyboard.set_focus(self, Some(surface), serial);
                            }
                        } else if let Some((window, _loc)) =
                            self.space.element_under(pos).map(|(w, l)| (w.clone(), l))
                        {
                            self.space.raise_element(&window, true);
                            keyboard.set_focus(
                                self,
                                Some(window.toplevel().unwrap().wl_surface().clone()),
                                serial,
                            );
                            self.space.elements().for_each(|window| {
                                window.toplevel().unwrap().send_pending_configure();
                            });
                        } else {
                            self.space.elements().for_each(|window| {
                                window.set_activated(false);
                                window.toplevel().unwrap().send_pending_configure();
                            });
                            keyboard.set_focus(self, Option::<WlSurface>::None, serial);
                        }
                    }
                };

                pointer.button(
                    self,
                    &ButtonEvent {
                        button,
                        state: button_state,
                        serial,
                        time: event.time_msec(),
                    },
                );
                pointer.frame(self);
            }
            InputEvent::PointerAxis { event, .. } => {
                let source = event.source();

                let horizontal_amount = event.amount(Axis::Horizontal).unwrap_or_else(|| {
                    event.amount_v120(Axis::Horizontal).unwrap_or(0.0) * 15.0 / 120.
                });
                let vertical_amount = event.amount(Axis::Vertical).unwrap_or_else(|| {
                    event.amount_v120(Axis::Vertical).unwrap_or(0.0) * 15.0 / 120.
                });
                let horizontal_amount_discrete = event.amount_v120(Axis::Horizontal);
                let vertical_amount_discrete = event.amount_v120(Axis::Vertical);

                let mut frame = AxisFrame::new(event.time_msec()).source(source);
                if horizontal_amount != 0.0 {
                    frame = frame.value(Axis::Horizontal, horizontal_amount);
                    if let Some(discrete) = horizontal_amount_discrete {
                        frame = frame.v120(Axis::Horizontal, discrete as i32);
                    }
                }
                if vertical_amount != 0.0 {
                    frame = frame.value(Axis::Vertical, vertical_amount);
                    if let Some(discrete) = vertical_amount_discrete {
                        frame = frame.v120(Axis::Vertical, discrete as i32);
                    }
                }

                if source == AxisSource::Finger {
                    if event.amount(Axis::Horizontal) == Some(0.0) {
                        frame = frame.stop(Axis::Horizontal);
                    }
                    if event.amount(Axis::Vertical) == Some(0.0) {
                        frame = frame.stop(Axis::Vertical);
                    }
                }

                let pointer = self.seat.get_pointer().unwrap();
                pointer.axis(self, frame);
                pointer.frame(self);
            }
            _ => {}
        }
    }
}
