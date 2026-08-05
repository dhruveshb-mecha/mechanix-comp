use crate::backend::Backend;
use crate::state::State;
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
use smithay::wayland::shell::xdg::ToplevelSurface;
use smithay::wayland::shell::xdg::decoration::XdgDecorationHandler;

impl<BackendData: Backend + 'static> XdgDecorationHandler for State<BackendData> {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        self.set_decoration_mode(toplevel, Mode::ServerSide);
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: Mode) {
        self.set_decoration_mode(toplevel, mode);
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        self.set_decoration_mode(toplevel, Mode::ServerSide);
    }
}

impl<BackendData: Backend + 'static> State<BackendData> {
    fn set_decoration_mode(&self, toplevel: ToplevelSurface, mode: Mode) {
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(mode);
        });
        if toplevel.is_initial_configure_sent() {
            toplevel.send_configure();
        }
    }
}
