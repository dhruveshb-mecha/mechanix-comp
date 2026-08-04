use std::time::Duration;

use smithay::{
    backend::{
        renderer::{
            damage::OutputDamageTracker, element::surface::WaylandSurfaceRenderElement,
            gles::GlesRenderer,
        },
        winit::{WinitEvent, WinitEventLoop},
    },
    desktop::layer_map_for_output,
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::calloop::EventLoop,
    utils::{Rectangle, Transform},
};

use crate::state::State;

pub fn init_winit(
    event_loop: &mut EventLoop<State>,
    state: &mut State,
    winit: WinitEventLoop,
) -> Result<(), Box<dyn std::error::Error>> {
    let mode = Mode {
        size: state.backend.window_size(),
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
    let _global = output.create_global::<State>(&state.display_handle);
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
                let size = state.backend.window_size();
                let damage = Rectangle::from_size(size);

                {
                    let (renderer, mut framebuffer) = state.backend.bind().unwrap();
                    let mut custom_elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> = Vec::new();
                    if state.is_locked {
                        // Only render live lock surfaces. Dead surfaces (client crashed)
                        // produce no elements and fall back to solid red color
                        for lock_surface in state.lock_surfaces.iter().filter(|s| s.alive()) {
                            let tree = smithay::backend::renderer::element::surface::render_elements_from_surface_tree::<GlesRenderer, WaylandSurfaceRenderElement<GlesRenderer>>(
                                renderer,
                                lock_surface.wl_surface(),
                                (0, 0),
                                1.0,
                                1.0,
                                smithay::backend::renderer::element::Kind::Unspecified
                            );
                            custom_elements.extend(tree);
                        }

                        damage_tracker
                            .render_output(renderer, &mut framebuffer, 0, &custom_elements, [1.0, 0.0, 0.0, 1.0])
                            .unwrap();
                    } else {
                        smithay::desktop::space::render_output::<
                            _,
                            WaylandSurfaceRenderElement<GlesRenderer>,
                            _,
                            _,
                        >(
                            &output,
                            renderer,
                            &mut framebuffer,
                            1.0,
                            0,
                            Some(&state.space),
                            &custom_elements,
                            &mut damage_tracker,
                            [0.1, 0.1, 0.1, 1.0],
                        )
                        .unwrap();
                    }
                }
                state.backend.submit(Some(&[damage])).unwrap();

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

                    // Only send the `locked` event once at least one live lock surface has been registered.
                    let has_live_surface = state.lock_surfaces.iter().any(|s| s.alive());
                    if has_live_surface
                        && let Some(locker) = state.pending_lock.take() {
                            locker.lock();
                        }
                }

                state.space.refresh();
                state.popups.cleanup();
                let _ = state.display_handle.flush_clients();

                state.backend.window().request_redraw();
            }
            WinitEvent::CloseRequested => {
                state.loop_signal.stop();
            }
            _ => (),
        })?;

    Ok(())
}
