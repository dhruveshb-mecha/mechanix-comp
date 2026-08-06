use crate::backend::Backend;
use crate::state::State;
use smithay::wayland::xdg_toplevel_icon::XdgToplevelIconHandler;

impl<BackendData: Backend + 'static> XdgToplevelIconHandler for State<BackendData> {}
