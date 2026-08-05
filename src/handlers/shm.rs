use crate::backend::Backend;
use crate::state::State;
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::shm::{ShmHandler, ShmState};

impl<BackendData: Backend + 'static> BufferHandler for State<BackendData> {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

impl<BackendData: Backend + 'static> ShmHandler for State<BackendData> {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}
