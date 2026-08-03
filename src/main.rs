mod handlers;
mod input;
mod state;
mod winit;

use crate::state::State;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut event_loop: EventLoop<State> = EventLoop::try_new()?;
    let display: Display<State> = Display::new()?;

    // Create the winit backend first so its GLES renderer is available when the
    // dmabuf global is built inside `State::new`.
    let (backend, winit) = smithay::backend::winit::init::<GlesRenderer>()?;

    let mut state = State::new(&mut event_loop, display, backend);

    // Open a Wayland/X11 window for our nested compositor
    crate::winit::init_winit(&mut event_loop, &mut state, winit)?;

    println!(
        "Compositor listening on Wayland socket: {:?}",
        state.socket_name
    );

    event_loop.run(None, &mut state, move |_| {})?;

    Ok(())
}
