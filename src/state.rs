use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::sync::Arc;
use std::time::{Duration, Instant};

use smithay::desktop::{PopupManager, Space, Window, layer_map_for_output};
use smithay::input::keyboard::Keysym;
use smithay::input::pointer::{CursorImageStatus, PointerHandle};
use smithay::input::{Seat, SeatState};
use smithay::output::Output;
use smithay::reexports::calloop::{
    EventLoop, Interest, LoopSignal, Mode, PostAction, generic::Generic,
};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::reexports::wayland_server::{Display, DisplayHandle};
use smithay::utils::{Clock, Logical, Monotonic, Point, SERIAL_COUNTER};
use smithay::wayland::compositor::{CompositorClientState, CompositorState, with_states};
use smithay::wayland::cursor_shape::CursorShapeManagerState;
use smithay::wayland::dmabuf::{DmabufGlobal, DmabufState};
use smithay::wayland::foreign_toplevel_list::ForeignToplevelListState;
use smithay::wayland::fractional_scale::{FractionalScaleManagerState, with_fractional_scale};
use smithay::wayland::idle_inhibit::IdleInhibitManagerState;
use smithay::wayland::idle_notify::IdleNotifierState;
use smithay::wayland::input_method::InputMethodManagerState;
use smithay::wayland::output::OutputManagerState;
use smithay::wayland::selection::data_device::DataDeviceState;
use smithay::wayland::selection::wlr_data_control::DataControlState;
use smithay::wayland::session_lock::{LockSurface, SessionLockManagerState, SessionLocker};
use smithay::wayland::shell::wlr_layer::{KeyboardInteractivity, Layer, WlrLayerShellState};
use smithay::wayland::shell::xdg::XdgShellState;
use smithay::wayland::shell::xdg::decoration::XdgDecorationState;
use smithay::wayland::shell::xdg::dialog::XdgDialogState;
use smithay::wayland::shm::ShmState;
use smithay::wayland::socket::ListeningSocketSource;
use smithay::wayland::text_input::TextInputManagerState;
use smithay::wayland::viewporter::ViewporterState;
use smithay::wayland::virtual_keyboard::VirtualKeyboardManagerState;
use smithay::wayland::xdg_activation::XdgActivationState;
use smithay::wayland::xdg_toplevel_icon::XdgToplevelIconManager;

use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;

use crate::backend::Backend;
use crate::handlers::foreign_toplevel::ForeignToplevelManagerState;

/// What a toplevel *is*, decided at the first commit from client-declared
/// facts (parent, title/app_id, dialog hint).
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum WindowKind {
    /// A regular toplevel: subject to the maximize/fullscreen policy.
    Normal,
    /// A dialog: never maximized/fullscreen, kept centered over its parent.
    Dialog,
    /// A parentless, untitled toplevel (GTK3 tooltip fallback that couldn't
    /// become an xdg_popup): rendered above windows, never focused, kept out
    /// of the `Space`; the compositor positions it.
    Transient(Point<i32, Logical>),
}

/// How a toplevel is arranged right now.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum WindowMode {
    /// The window keeps its own size, centered in the work zone.
    Floating,
    /// The window fills the work zone.
    Maximized,
    /// The window fills the output and renders above the top layer.
    Fullscreen,
}

/// Everything the compositor tracks about one xdg toplevel, keyed in
/// `State::toplevels` by its `wl_surface`.
pub struct WindowState {
    pub window: Window,
    pub kind: WindowKind,
    pub mode: WindowMode,
    /// True once first-commit handling ran; transients are marked too, though they never join the `Space`.
    pub mapped: bool,
    /// The xdg-dialog modal hint, cached so input handling needn't lock
    /// surface data on every click.
    pub modal: bool,
}

pub struct State<BackendData: Backend + 'static> {
    pub start_time: Instant,
    pub socket_name: OsString,
    pub display_handle: DisplayHandle,

    pub space: Space<Window>,
    pub loop_signal: LoopSignal,

    /// All xdg toplevels ever created, keyed by `wl_surface`, whether or not
    /// mapped into `space` yet (mapping happens on the first commit). Dead
    /// entries are pruned by `cleanup_toplevels`.
    pub toplevels: HashMap<WlSurface, WindowState>,

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
    pub suppressed_keys: Vec<Keysym>,
    pub cursor_status: CursorImageStatus,
    pub clock: Clock<Monotonic>,
    pub pointer: PointerHandle<State<BackendData>>,
    pub cursor_position_hint: Option<(WlSurface, Point<f64, Logical>)>,

    // Rendering backend + dmabuf import. The dmabuf global is created lazily by
    // each backend once its renderer (and thus its format list) exists.
    pub backend_data: BackendData,
    pub dmabuf_state: DmabufState,
    pub dmabuf_global: Option<DmabufGlobal>,

    pub session_lock_state: SessionLockManagerState,
    pub is_locked: bool,
    pub lock_surfaces: Vec<LockSurface>,
    pub viewporter_state: ViewporterState,
    pub foreign_toplevel: ForeignToplevelManagerState,
    pub foreign_toplevel_list: ForeignToplevelListState,
    pub xdg_toplevel_icon: XdgToplevelIconManager,
    pub xdg_dialog_state: XdgDialogState,
    pub idle_notifier_state: IdleNotifierState<State<BackendData>>,
    pub idle_inhibit_manager_state: IdleInhibitManagerState,
    pub data_control_state: DataControlState,
    pub fractional_scale_manager_state: FractionalScaleManagerState,
    /// Surfaces holding an active `zwp_idle_inhibitor_v1`; while non-empty the
    /// idle notifier is inhibited.
    pub idle_inhibiting_surfaces: HashSet<WlSurface>,
    /// The `OnDemand` layer surface last opened or clicked. `update_keyboard_focus`
    /// focuses it while it stays a mapped OnDemand layer, so launchers and
    /// panels take keyboard focus on open.
    pub layer_shell_on_demand_focus: Option<WlSurface>,
    /// The toplevel surface last focused; the fallback keyboard focus when no
    /// layer-shell surface holds it.
    pub active_window: Option<WlSurface>,
    /// Held between `lock()` and the first submitted locked frame.
    /// Calling `.lock()` on this sends the `locked` event to the client.
    pub pending_lock: Option<SessionLocker>,
}

impl<BackendData: Backend + 'static> State<BackendData> {
    pub fn new(
        event_loop: &mut EventLoop<'static, Self>,
        display: Display<Self>,
        backend_data: BackendData,
    ) -> Self {
        let start_time = Instant::now();
        let dh = display.handle();
        let clock = Clock::new();

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
        let pointer = seat.add_pointer();

        let session_lock_state = SessionLockManagerState::new::<Self, _>(&dh, |_| true);
        let viewporter_state = ViewporterState::new::<Self>(&dh);
        let foreign_toplevel = ForeignToplevelManagerState::new::<Self>(&dh);
        let foreign_toplevel_list = ForeignToplevelListState::new::<Self>(&dh);
        let mut xdg_toplevel_icon = XdgToplevelIconManager::new::<Self>(&dh);
        xdg_toplevel_icon.add_icon_size(64);
        let xdg_dialog_state = XdgDialogState::new::<Self>(&dh);
        let idle_notifier_state = IdleNotifierState::new(&dh, event_loop.handle());
        let idle_inhibit_manager_state = IdleInhibitManagerState::new::<Self>(&dh);
        let data_control_state = DataControlState::new::<Self, _>(&dh, None, |_| true);
        let fractional_scale_manager_state = FractionalScaleManagerState::new::<Self>(&dh);
        TextInputManagerState::new::<Self>(&dh);
        InputMethodManagerState::new::<Self, _>(&dh, |_client| true);
        VirtualKeyboardManagerState::new::<Self, _>(&dh, |_client| true);
        CursorShapeManagerState::new::<Self>(&dh);

        let socket_name = Self::init_wayland_listener(display, event_loop);
        let loop_signal = event_loop.get_signal();

        Self {
            start_time,
            socket_name,
            display_handle: dh,
            space,
            toplevels: HashMap::new(),
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
            suppressed_keys: Vec::new(),
            cursor_status: CursorImageStatus::default_named(),
            pointer,
            cursor_position_hint: None,
            clock,
            backend_data,
            dmabuf_state,
            dmabuf_global: None,
            session_lock_state,
            is_locked: false,
            lock_surfaces: Vec::new(),
            viewporter_state,
            foreign_toplevel,
            foreign_toplevel_list,
            xdg_toplevel_icon,
            xdg_dialog_state,
            idle_notifier_state,
            idle_inhibit_manager_state,
            data_control_state,
            fractional_scale_manager_state,
            idle_inhibiting_surfaces: HashSet::new(),
            layer_shell_on_demand_focus: None,
            active_window: None,
            pending_lock: None,
        }
    }

    fn init_wayland_listener(display: Display<Self>, event_loop: &mut EventLoop<Self>) -> OsString {
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
                    let _ = state.display_handle.flush_clients();
                    Ok(PostAction::Continue)
                },
            )
            .unwrap();

        socket_name
    }

    /// Send frame callbacks to every visible surface on `output`, once per
    /// presented frame. Lifecycle bookkeeping happens in the backends' idle
    /// callbacks instead, so client I/O isn't blocked on frame presentation.
    pub fn send_frame_callbacks(&mut self, output: &Output) {
        let now = self.start_time.elapsed();
        if self.is_locked {
            // Send frame callbacks only to live surfaces.
            for lock_surface in self.lock_surfaces.iter().filter(|s| s.alive()) {
                smithay::desktop::utils::send_frames_surface_tree(
                    lock_surface.wl_surface(),
                    output,
                    now,
                    Some(Duration::ZERO),
                    |_, _| Some(output.clone()),
                );
            }

            // Send `locked` once a live lock surface has been registered.
            let has_live_surface = self.lock_surfaces.iter().any(|s| s.alive());
            if has_live_surface && let Some(locker) = self.pending_lock.take() {
                locker.lock();
            }
        } else {
            let scale = output.current_scale().fractional_scale();
            for window in self.space.elements() {
                window.send_frame(output, now, Some(Duration::ZERO), |_, _| {
                    Some(output.clone())
                });
                self.push_fractional_scale(window.toplevel().unwrap().wl_surface(), scale);
            }
            for layer_surface in layer_map_for_output(output).layers() {
                layer_surface.send_frame(output, now, Some(Duration::ZERO), |_, _| {
                    Some(output.clone())
                });
                self.push_fractional_scale(layer_surface.wl_surface(), scale);
            }
            for (window, _) in self.transient_windows() {
                window.send_frame(output, now, Some(Duration::ZERO), |_, _| {
                    Some(output.clone())
                });
                self.push_fractional_scale(window.toplevel().unwrap().wl_surface(), scale);
            }
        }
    }

    /// Push the preferred fractional scale to `surface` (no-op for surfaces
    /// without the object); the module only sends the event when it changes.
    fn push_fractional_scale(&self, surface: &WlSurface, scale: f64) {
        if !surface.is_alive() {
            return;
        }
        with_states(surface, |states| {
            with_fractional_scale(states, |fractional_scale| {
                fractional_scale.set_preferred_scale(scale);
            });
        });
    }

    /// Recompute idle-notify inhibition from the active `zwp_idle_inhibitor_v1`
    /// surfaces.
    pub fn update_idle_inhibit(&mut self) {
        let inhibited = !self.idle_inhibiting_surfaces.is_empty();
        self.idle_notifier_state.set_is_inhibited(inhibited);
    }

    /// The tracked transient tooltip fallbacks and their positions.
    pub fn transient_windows(&self) -> impl Iterator<Item = (&Window, Point<i32, Logical>)> {
        self.toplevels.values().filter_map(|ws| match ws.kind {
            WindowKind::Transient(loc) => Some((&ws.window, loc)),
            _ => None,
        })
    }

    /// The topmost window currently in `Fullscreen` mode, if any. While one is
    /// active it is rendered above the top layer and keeps keyboard focus.
    pub fn active_fullscreen_window(&self) -> Option<Window> {
        self.space
            .elements()
            .rev()
            .find(|window| {
                window.toplevel().is_some_and(|toplevel| {
                    self.toplevels
                        .get(toplevel.wl_surface())
                        .is_some_and(|ws| ws.mode == WindowMode::Fullscreen)
                })
            })
            .cloned()
    }

    /// Prune bookkeeping for toplevels whose client went away without the
    /// `toplevel_destroyed` path (e.g. a crash); dropping the entry also drops
    /// its foreign-toplevel handle.
    pub fn cleanup_toplevels(&mut self) {
        self.toplevels
            .retain(|_, ws| ws.window.toplevel().unwrap().wl_surface().is_alive());
        // Prune dead idle-inhibitor surfaces and re-evaluate.
        self.idle_inhibiting_surfaces
            .retain(|surface| surface.is_alive());
        self.update_idle_inhibit();
    }

    /// Recompute keyboard focus from the layer-shell priority list and apply it
    /// if it changed. Called each frame from the backends' idle callbacks.
    pub fn update_keyboard_focus(&mut self) {
        if self.is_locked {
            return;
        }
        let keyboard = self.seat.get_keyboard().unwrap();
        if keyboard.is_grabbed() {
            return;
        }
        let focus = self.compute_keyboard_focus();
        if keyboard.current_focus().as_ref() != focus.as_ref() {
            keyboard.set_focus(self, focus, SERIAL_COUNTER.next_serial());
        }
    }

    /// The keyboard focus target: the top-most `Exclusive` layer (Overlay/Top),
    /// else the opened/clicked `OnDemand` layer, else the active window.
    fn compute_keyboard_focus(&self) -> Option<WlSurface> {
        for kind in [Layer::Overlay, Layer::Top] {
            if let Some(surface) = self.topmost_exclusive_layer(kind) {
                return Some(surface);
            }
        }
        if let Some(surface) = self
            .layer_shell_on_demand_focus
            .clone()
            .filter(|s| self.is_mapped_on_demand_layer(s))
        {
            return Some(surface);
        }
        self.active_window
            .clone()
            .filter(|surface| surface.is_alive())
    }

    /// The top-most mapped layer on `kind` with `Exclusive` keyboard interactivity.
    fn topmost_exclusive_layer(&self, kind: Layer) -> Option<WlSurface> {
        self.space.outputs().find_map(|output| {
            layer_map_for_output(output)
                .layers_on(kind)
                .rev()
                .find(|layer| {
                    layer.cached_state().keyboard_interactivity == KeyboardInteractivity::Exclusive
                })
                .map(|layer| layer.wl_surface().clone())
        })
    }

    /// Whether `surface` is still a mapped layer with `OnDemand` interactivity.
    fn is_mapped_on_demand_layer(&self, surface: &WlSurface) -> bool {
        if !surface.is_alive() {
            return false;
        }
        self.space.outputs().any(|output| {
            layer_map_for_output(output).layers().any(|layer| {
                layer.wl_surface() == surface
                    && layer.cached_state().keyboard_interactivity
                        == KeyboardInteractivity::OnDemand
            })
        })
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
