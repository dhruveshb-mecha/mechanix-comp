use std::ffi::OsString;
use std::sync::Arc;
use std::time::Instant;

use smithay::backend::renderer::ImportDma;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::winit::WinitGraphicsBackend;
use smithay::desktop::{PopupManager, Space, Window};
use smithay::input::{Seat, SeatState};
use smithay::reexports::calloop::{
    EventLoop, Interest, LoopSignal, Mode, PostAction, generic::Generic,
};
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

pub struct State {
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
    pub shm_state: ShmState,
    pub output_manager_state: OutputManagerState,
    pub data_device_state: DataDeviceState,
    pub seat_state: SeatState<State>,
    pub popups: PopupManager,

    pub seat: Seat<Self>,

    // Rendering backend + dmabuf import
    pub backend: WinitGraphicsBackend<GlesRenderer>,
    pub dmabuf_state: DmabufState,
    pub dmabuf_global: DmabufGlobal,

    pub session_lock_state: SessionLockManagerState,
    pub is_locked: bool,
    pub lock_surfaces: Vec<LockSurface>,
    /// Held between `lock()` and the first submitted locked frame.
    /// Calling `.lock()` on this sends the `locked` event to the client.
    pub pending_lock: Option<SessionLocker>,
}

impl State {
    pub fn new(
        event_loop: &mut EventLoop<Self>,
        display: Display<Self>,
        mut backend: WinitGraphicsBackend<GlesRenderer>,
    ) -> Self {
        let start_time = Instant::now();
        let dh = display.handle();

        // Advertise zwp_linux_dmabuf_v1 (v3) with the formats the GLES renderer
        // can import. Dispatch is handled by the blanket `delegate_dispatch2!`.
        let dmabuf_formats = backend.renderer().dmabuf_formats();
        let mut dmabuf_state = DmabufState::new();
        let dmabuf_global = dmabuf_state.create_global::<Self>(&dh, dmabuf_formats);

        let compositor_state = CompositorState::new::<Self>(&dh);
        let xdg_shell_state = XdgShellState::new::<Self>(&dh);
        let xdg_decoration_state = XdgDecorationState::new::<Self>(&dh);
        let layer_shell_state = WlrLayerShellState::new::<Self>(&dh);
        let shm_state = ShmState::new::<Self>(&dh, vec![]);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&dh);
        let space = Space::default();
        let data_device_state = DataDeviceState::new::<Self>(&dh);
        let popups = PopupManager::default();
        let mut seat_state = SeatState::new();
        let mut seat: Seat<Self> = seat_state.new_wl_seat(&dh, "winit");
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
            shm_state,
            output_manager_state,
            data_device_state,
            seat_state,
            popups,
            seat,
            backend,
            dmabuf_state,
            dmabuf_global,
            session_lock_state,
            is_locked: false,
            lock_surfaces: Vec::new(),
            pending_lock: None,
        }
    }

    fn init_wayland_listener(
        display: Display<State>,
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
