use std::time::Duration;

use smithay::backend::renderer::ImportDma;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::winit::{WinitEvent, WinitGraphicsBackend};
use smithay::desktop::layer_map_for_output;
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use smithay::utils::{Rectangle, Transform};

use crate::backend::Backend;
use crate::render::output_elements;
use crate::state::State;

/// Backend data for the nested winit window. Both the output and the damage
/// tracker are captured by the redraw closure rather than stored here, so this
/// only owns the graphics backend itself.
pub struct WinitData {
    pub backend: WinitGraphicsBackend<GlesRenderer>,
}

impl Backend for WinitData {
    fn renderer(&mut self) -> &mut GlesRenderer {
        self.backend.renderer()
    }

    fn seat_name(&self) -> String {
        "winit".to_string()
    }

    fn reset_buffers(&mut self, _output: &Output) {
        // The winit backend re-renders a full frame every time; there are no
        // scanout buffers to reset.
    }
}

/// Create the nested winit window, wire up the compositor state, and run the
/// event loop to completion.
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut event_loop: EventLoop<State<WinitData>> = EventLoop::try_new()?;
    let display: Display<State<WinitData>> = Display::new()?;

    let (backend, winit) = smithay::backend::winit::init::<GlesRenderer>()?;
    let mut state = State::new(&mut event_loop, display, WinitData { backend });

    // Advertise zwp_linux_dmabuf_v1 with the formats the GLES renderer can
    // import, now that the renderer exists.
    let dmabuf_formats = state.backend_data.renderer().dmabuf_formats();
    let dmabuf_global = state
        .dmabuf_state
        .create_global::<State<WinitData>>(&state.display_handle, dmabuf_formats);
    state.dmabuf_global = Some(dmabuf_global);

    let mode = Mode {
        size: state.backend_data.backend.window_size(),
        refresh: 60_000,
    };

    let output = Output::new(
        "winit".to_string(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "Smithay".into(),
            model: "Winit".into(),
            serial_number: "0".into(),
        },
    );
    let _global = output.create_global::<State<WinitData>>(&state.display_handle);
    output.change_current_state(
        Some(mode),
        Some(Transform::Flipped180),
        None,
        Some((0, 0).into()),
    );
    output.set_preferred(mode);

    state.space.map_output(&output, (0, 0));

    let mut damage_tracker = OutputDamageTracker::from_output(&output);

    event_loop
        .handle()
        .insert_source(winit, move |event, _, state| match event {
            WinitEvent::Resized { size, .. } => {
                output.change_current_state(
                    Some(Mode {
                        size,
                        refresh: 60_000,
                    }),
                    None,
                    None,
                    None,
                );

                // Re-arrange layer surfaces for the new output size, then keep
                // every open toplevel filling the (possibly changed)
                // non-exclusive zone.
                layer_map_for_output(&output).arrange();
                state.reflow_toplevels();

                if state.is_locked {
                    let logical_size = (size.w as u32, size.h as u32).into();
                    for surface in &state.lock_surfaces {
                        surface.with_pending_state(|pending| {
                            pending.size = Some(logical_size);
                        });
                        surface.send_configure();
                    }
                }
            }
            WinitEvent::Input(event) => state.process_input_event(event),
            WinitEvent::Redraw => {
                let size = state.backend_data.backend.window_size();
                let damage = Rectangle::from_size(size);

                {
                    let (renderer, mut framebuffer) = state.backend_data.backend.bind().unwrap();
                    let (elements, clear_color) = output_elements(
                        renderer,
                        &state.space,
                        &output,
                        state.is_locked,
                        &state.lock_surfaces,
                    );
                    damage_tracker
                        .render_output(renderer, &mut framebuffer, 0, &elements, clear_color)
                        .unwrap();
                }
                state.backend_data.backend.submit(Some(&[damage])).unwrap();

                if !state.is_locked {
                    state.space.elements().for_each(|window| {
                        window.send_frame(
                            &output,
                            state.start_time.elapsed(),
                            Some(Duration::ZERO),
                            |_, _| Some(output.clone()),
                        )
                    });

                    for layer_surface in layer_map_for_output(&output).layers() {
                        layer_surface.send_frame(
                            &output,
                            state.start_time.elapsed(),
                            Some(Duration::ZERO),
                            |_, _| Some(output.clone()),
                        );
                    }
                }

                if state.is_locked {
                    // Send frame callbacks only to live surfaces.
                    for lock_surface in state.lock_surfaces.iter().filter(|s| s.alive()) {
                        smithay::desktop::utils::send_frames_surface_tree(
                            lock_surface.wl_surface(),
                            &output,
                            state.start_time.elapsed(),
                            Some(Duration::ZERO),
                            |_, _| Some(output.clone()),
                        );
                    }

                    // Only send the `locked` event once at least one live lock
                    // surface has been registered.
                    let has_live_surface = state.lock_surfaces.iter().any(|s| s.alive());
                    if has_live_surface
                        && let Some(locker) = state.pending_lock.take()
                    {
                        locker.lock();
                    }
                }

                state.space.refresh();
                state.popups.cleanup();
                let _ = state.display_handle.flush_clients();

                state.backend_data.backend.window().request_redraw();
            }
            WinitEvent::CloseRequested => {
                state.loop_signal.stop();
            }
            _ => (),
        })?;

    println!(
        "Compositor listening on Wayland socket: {:?}",
        state.socket_name
    );

    event_loop.run(None, &mut state, move |_| {})?;

    Ok(())
}
