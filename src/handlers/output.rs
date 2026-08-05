use crate::backend::Backend;
use crate::state::State;
use smithay::wayland::output::OutputHandler;

impl<BackendData: Backend + 'static> OutputHandler for State<BackendData> {}
