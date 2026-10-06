use crate::backend::Backend;
use crate::state::State;
use smithay::wayland::xdg_foreign::{XdgForeignHandler, XdgForeignState};

impl<BackendData: Backend + 'static> XdgForeignHandler for State<BackendData> {
    fn xdg_foreign_state(&mut self) -> &mut XdgForeignState {
        &mut self.xdg_foreign_state
    }
}
