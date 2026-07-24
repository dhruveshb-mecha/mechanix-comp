pub mod compositor;
pub mod data_device;
pub mod output;
pub mod seat;
pub mod shm;

smithay::delegate_dispatch2!(crate::state::State);
