use crate::backend::Backend;
use crate::state::State;
use smithay::desktop::Window;
use smithay::wayland::shell::xdg::dialog::{ToplevelDialogHint, XdgDialogHandler};

impl<BackendData: Backend + 'static> XdgDialogHandler for State<BackendData> {
    fn dialog_hint_changed(
        &mut self,
        toplevel: smithay::wayland::shell::xdg::ToplevelSurface,
        hint: ToplevelDialogHint,
    ) {
        // Cache the hint so input handling doesn't lock surface data per click.
        if let Some(ws) = self.toplevels.get_mut(toplevel.wl_surface()) {
            ws.modal = hint == ToplevelDialogHint::Modal;
        }
    }
}

impl<BackendData: Backend + 'static> State<BackendData> {
    /// The topmost window currently marked modal, if any. While open, input to
    /// every other window is blocked.
    pub fn active_modal_window(&self) -> Option<Window> {
        self.space
            .elements()
            .rev()
            .find(|w| {
                w.toplevel().is_some_and(|toplevel| {
                    self.toplevels
                        .get(toplevel.wl_surface())
                        .is_some_and(|ws| ws.modal)
                })
            })
            .cloned()
    }
}
