use crate::state::State;
use smithay::delegate_output;
use smithay::wayland::output::OutputHandler;

impl OutputHandler for State {}

delegate_output!(State);
