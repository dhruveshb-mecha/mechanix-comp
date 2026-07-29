pub mod compositor;
pub mod data_device;
pub mod dmabuf;
pub mod layer_shell;
pub mod output;
pub mod seat;
pub mod shm;
pub mod xdg_shell;
pub mod session_lock;

smithay::delegate_dispatch2!(crate::state::State);
