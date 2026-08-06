use crate::backend::Backend;
use crate::state::State;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::with_states;
use smithay::wayland::fractional_scale::{FractionalScaleHandler, with_fractional_scale};

impl<BackendData: Backend + 'static> FractionalScaleHandler for State<BackendData> {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        // Seed the current scale so the first `preferred_scale` event is correct.
        let scale = self
            .space
            .outputs()
            .next()
            .map(|output| output.current_scale().fractional_scale())
            .unwrap_or(1.0);
        with_states(&surface, |states| {
            with_fractional_scale(states, |fractional_scale| {
                fractional_scale.set_preferred_scale(scale);
            });
        });
    }
}
