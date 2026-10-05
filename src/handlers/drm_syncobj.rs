use crate::backend::Backend;
use crate::state::State;
use smithay::wayland::drm_syncobj::{DrmSyncobjHandler, DrmSyncobjState};

impl<BackendData: Backend + 'static> DrmSyncobjHandler for State<BackendData> {
    fn drm_syncobj_state(&mut self) -> Option<&mut DrmSyncobjState> {
        self.drm_syncobj_state.as_mut()
    }
}
