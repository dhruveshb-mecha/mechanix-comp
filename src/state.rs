use std::ffi::OsString;
use std::sync::Arc;
use std::time::Instant;

use smithay::desktop::{PopupManager, Space, Window};
use smithay::input::{Seat, SeatState};
use smithay::reexports::calloop::{
    EventLoop, Interest, LoopSignal, Mode, PostAction, generic::Generic,
};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::reexports::wayland_server::{Display, DisplayHandle};
use smithay::wayland::compositor::{CompositorClientState, CompositorState};
use smithay::wayland::dmabuf::{DmabufGlobal, DmabufState};
use smithay::wayland::output::OutputManagerState;
use smithay::wayland::selection::data_device::DataDeviceState;
use smithay::wayland::session_lock::{LockSurface, SessionLockManagerState, SessionLocker};
use smithay::wayland::shell::wlr_layer::WlrLayerShellState;
use smithay::wayland::shell::xdg::XdgShellState;
use smithay::wayland::shell::xdg::decoration::XdgDecorationState;
use smithay::wayland::shm::ShmState;
use smithay::wayland::socket::ListeningSocketSource;
use smithay::wayland::xdg_activation::XdgActivationState;

use crate::backend::Backend;

pub struct State<BackendData: Backend + 'static> {
    pub start_time: Instant,
    pub socket_name: OsString,
    pub display_handle: DisplayHandle,

    pub space: Space<Window>,
    pub loop_signal: LoopSignal,

    // Smithay State
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub xdg_decoration_state: XdgDecorationState,
    pub layer_shell_state: WlrLayerShellState,
    pub xdg_activation_state: XdgActivationState,
    pub shm_state: ShmState,
    pub output_manager_state: OutputManagerState,
    pub data_device_state: DataDeviceState,
    pub seat_state: SeatState<State<BackendData>>,
    pub popups: PopupManager,

    pub seat: Seat<Self>,

    // Rendering backend + dmabuf import. The dmabuf global is created lazily by
    // each backend once its renderer (and thus its format list) exists.
    pub backend_data: BackendData,
    pub dmabuf_state: DmabufState,
    pub dmabuf_global: Option<DmabufGlobal>,

    pub session_lock_state: SessionLockManagerState,
    pub is_locked: bool,
    pub lock_surfaces: Vec<LockSurface>,
    /// Held between `lock()` and the first submitted locked frame.
    /// Calling `.lock()` on this sends the `locked` event to the client.
    pub pending_lock: Option<SessionLocker>,
}

impl<BackendData: Backend + 'static> State<BackendData> {
    pub fn new(
        event_loop: &mut EventLoop<Self>,
        display: Display<Self>,
        backend_data: BackendData,
    ) -> Self {
        let start_time = Instant::now();
        let dh = display.handle();

        // The zwp_linux_dmabuf_v1 global is created lazily by each backend once
        // its renderer's format list is available. Dispatch is handled by the
        // blanket `delegate_dispatch2!`.
        let dmabuf_state = DmabufState::new();

        let seat_name = backend_data.seat_name();

        let compositor_state = CompositorState::new::<Self>(&dh);
        let xdg_shell_state = XdgShellState::new_with_capabilities::<Self>(
            &dh,
            [
                xdg_toplevel::WmCapabilities::Maximize,
                xdg_toplevel::WmCapabilities::Fullscreen,
            ],
        );
        let xdg_decoration_state = XdgDecorationState::new::<Self>(&dh);
        let layer_shell_state = WlrLayerShellState::new::<Self>(&dh);
        let xdg_activation_state = XdgActivationState::new::<Self>(&dh);
        let shm_state = ShmState::new::<Self>(&dh, vec![]);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&dh);
        let space = Space::default();
        let data_device_state = DataDeviceState::new::<Self>(&dh);
        let popups = PopupManager::default();
        let mut seat_state = SeatState::new();
        let mut seat: Seat<Self> = seat_state.new_wl_seat(&dh, seat_name);
        seat.add_keyboard(Default::default(), 200, 25).unwrap();
        seat.add_pointer();

        let session_lock_state = SessionLockManagerState::new::<Self, _>(&dh, |_| true);

        let socket_name = Self::init_wayland_listener(display, event_loop);
        let loop_signal = event_loop.get_signal();

        Self {
            start_time,
            socket_name,
            display_handle: dh,
            space,
            loop_signal,
            compositor_state,
            xdg_shell_state,
            xdg_decoration_state,
            layer_shell_state,
            xdg_activation_state,
            shm_state,
            output_manager_state,
            data_device_state,
            seat_state,
            popups,
            seat,
            backend_data,
            dmabuf_state,
            dmabuf_global: None,
            session_lock_state,
            is_locked: false,
            lock_surfaces: Vec::new(),
            pending_lock: None,
        }
    }

    fn init_wayland_listener(
        display: Display<Self>,
        event_loop: &mut EventLoop<Self>,
    ) -> OsString {
        let listening_socket = ListeningSocketSource::new_auto().unwrap();
        let socket_name = listening_socket.socket_name().to_os_string();

        let loop_handle = event_loop.handle();

        loop_handle
            .insert_source(listening_socket, move |client_stream, _, state| {
                state
                    .display_handle
                    .insert_client(client_stream, Arc::new(ClientState::default()))
                    .unwrap();
            })
            .expect("Failed to init the wayland event source.");

        loop_handle
            .insert_source(
                Generic::new(display, Interest::READ, Mode::Level),
                |_, display, state| {
                    unsafe {
                        display.get_mut().dispatch_clients(state).unwrap();
                    }
                    Ok(PostAction::Continue)
                },
            )
            .unwrap();

        socket_name
    }
}

#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}
