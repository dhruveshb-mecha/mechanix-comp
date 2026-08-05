pub mod compositor;
pub mod data_device;
pub mod dmabuf;
pub mod layer_shell;
pub mod output;
pub mod seat;
pub mod session_lock;
pub mod shm;
pub mod xdg_activation;
pub mod xdg_shell;

use crate::backend::Backend;

smithay::delegate_dispatch2!(@<BackendData: Backend + 'static> crate::state::State<BackendData>);
