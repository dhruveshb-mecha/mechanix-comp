use crate::state::State;
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::renderer::ImportDma;
use smithay::wayland::dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier};

impl DmabufHandler for State {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        // Import eagerly to validate the buffer at attach time. The resulting
        // texture is discarded here — `GlesRenderer` weak-ref-caches dmabuf
        // imports, so `render_output` reuses it when sampling the surface.
        match self.backend.renderer().import_dmabuf(&dmabuf, None) {
            Ok(_texture) => {
                let _ = notifier.successful::<State>();
            }
            Err(_) => notifier.failed(),
        }
    }
}
