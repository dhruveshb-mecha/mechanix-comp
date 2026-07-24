pub mod compositor;
pub mod output;
pub mod shm;

smithay::delegate_dispatch2!(crate::state::State);
