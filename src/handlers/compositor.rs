use crate::backend::Backend;
use crate::handlers::{layer_shell, xdg_shell};
use crate::state::State;
use smithay::backend::renderer::utils::on_commit_buffer_handler;
use smithay::desktop::PopupKind;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::{
    CompositorHandler, CompositorState, get_parent, is_sync_subsurface,
};

impl<BackendData: Backend + 'static> CompositorHandler for State<BackendData> {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(
        &self,
        client: &'a smithay::reexports::wayland_server::Client,
    ) -> &'a smithay::wayland::compositor::CompositorClientState {
        &client
            .get_data::<crate::state::ClientState>()
            .unwrap()
            .compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        self.backend_data.early_import(surface);

        let mut child_to_center: Option<smithay::desktop::Window> = None;

        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            if let Some(window) = self
                .space
                .elements()
                .find(|w| w.toplevel().unwrap().wl_surface() == &root)
                .cloned()
            {
                window.on_commit();

                if window.toplevel().unwrap().parent().is_some() {
                    child_to_center = Some(window);
                }
            }
        }

        self.popups.commit(surface);
        if let Some(popup) = self.popups.find_popup(surface) {
            if let PopupKind::Xdg(ref xdg) = popup {
                eprintln!(
                    "[commit] xdg popup found, needs_configure={}",
                    !xdg.is_initial_configure_sent()
                );
                if !xdg.is_initial_configure_sent() {
                    xdg.send_configure().expect("initial configure failed");
                }
            }
        }

        if layer_shell::handle_commit(self, surface) {
            if let Some(window) = child_to_center {
                self.center_child_toplevel(&window);
            }
            return;
        }

        xdg_shell::handle_commit(self, surface);

        if let Some(window) = child_to_center {
            self.center_child_toplevel(&window);
        }
    }
}
