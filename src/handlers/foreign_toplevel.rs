//! Unified `ext-foreign-toplevel-list-v1` + `zwlr-foreign-toplevel-management-v1`
//! implementation.
//!
//! Both protocols are served from a single map of toplevels keyed by
//! `wl_surface`, so identifiers and state stay consistent across them. The map
//! is (re)built by [`State::foreign_toplevel_refresh`], called from the
//! backends' idle callbacks, which diffs the current windows against the
//! announced ones and emits the appropriate events.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};

use smithay::output::Output;
use smithay::reexports::wayland_protocols::ext::foreign_toplevel_list::v1::server::{
    ext_foreign_toplevel_handle_v1::{self, ExtForeignToplevelHandleV1},
    ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
};
use smithay::reexports::wayland_protocols_wlr::foreign_toplevel::v1::server::{
    zwlr_foreign_toplevel_handle_v1::{self, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1},
};
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use smithay::utils::SERIAL_COUNTER;
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;
use smithay::wayland::{Dispatch2, GlobalDispatch2};

use crate::backend::Backend;
use crate::state::{State, WindowKind, WindowMode};

const EXT_LIST_VERSION: u32 = 1;
const WLR_MANAGEMENT_VERSION: u32 = 3;

/// Global user data for both manager globals. Currently carries no state; it
/// exists so the globals get a distinct `GlobalDispatch2` impl.
#[derive(Default)]
pub struct ForeignToplevelGlobalData;

/// Resource user data for the manager and handle resources. The per-client
/// announcement state lives in [`ForeignToplevelManagerState`], so this is a
/// marker type only.
#[derive(Default)]
pub struct ForeignToplevelUdata;

/// What the foreign-toplevel protocols need to know about one toplevel.
#[derive(Debug)]
pub struct ToplevelData {
    /// Monotonic, never-reused identifier (ext-list sends it as a string).
    identifier: u64,
    title: String,
    app_id: String,
    /// The wlr `state` event payload: maximized/fullscreen/activated flags.
    states: Vec<u8>,
    /// The output the toplevel is currently on, for `output_enter`/`leave`.
    output: Option<Output>,
    /// Per-client ext-list handles announcing this toplevel.
    ext_list_instances: HashSet<ExtForeignToplevelHandleV1>,
    /// Per-client wlr handles, each with the outputs entered so far.
    wlr_management_instances: HashMap<ZwlrForeignToplevelHandleV1, Vec<WlOutput>>,
}

impl ToplevelData {
    fn add_ext_instance<D>(
        &mut self,
        handle: &DisplayHandle,
        client: &Client,
        manager: &ExtForeignToplevelListV1,
    ) where
        D: Dispatch<ExtForeignToplevelHandleV1, ForeignToplevelUdata> + 'static,
    {
        let Ok(toplevel) = client.create_resource::<ExtForeignToplevelHandleV1, _, D>(
            handle,
            manager.version(),
            ForeignToplevelUdata,
        ) else {
            return;
        };
        manager.toplevel(&toplevel);

        toplevel.identifier(self.identifier.to_string());
        toplevel.title(self.title.clone());
        toplevel.app_id(self.app_id.clone());
        toplevel.done();

        self.ext_list_instances.insert(toplevel);
    }

    fn add_wlr_instance<D>(
        &mut self,
        handle: &DisplayHandle,
        client: &Client,
        manager: &ZwlrForeignToplevelManagerV1,
    ) where
        D: Dispatch<ZwlrForeignToplevelHandleV1, ForeignToplevelUdata> + 'static,
    {
        let Ok(toplevel) = client.create_resource::<ZwlrForeignToplevelHandleV1, _, D>(
            handle,
            manager.version(),
            ForeignToplevelUdata,
        ) else {
            return;
        };
        manager.toplevel(&toplevel);

        toplevel.title(self.title.clone());
        toplevel.app_id(self.app_id.clone());
        toplevel.state(self.states.clone());

        let mut outputs = Vec::new();
        if let Some(output) = &self.output {
            for wl_output in output.client_outputs(client) {
                toplevel.output_enter(&wl_output);
                outputs.push(wl_output);
            }
        }

        toplevel.done();

        self.wlr_management_instances.insert(toplevel, outputs);
    }
}

/// One toplevel as seen at refresh time, decoupled from the live state so the
/// refresh can diff against the already-announced data.
struct ToplevelSnapshot {
    surface: WlSurface,
    title: String,
    app_id: String,
    states: Vec<u8>,
    output: Option<Output>,
}

/// The foreign-toplevel module state, held by [`State`].
#[derive(Default)]
pub struct ForeignToplevelManagerState {
    ext_list_instances: HashSet<ExtForeignToplevelListV1>,
    wlr_management_instances: HashSet<ZwlrForeignToplevelManagerV1>,
    toplevels: HashMap<WlSurface, ToplevelData>,
    next_identifier: u64,
}

impl ForeignToplevelManagerState {
    /// Register both globals.
    pub fn new<D>(dh: &DisplayHandle) -> Self
    where
        D: GlobalDispatch<ExtForeignToplevelListV1, ForeignToplevelGlobalData>,
        D: GlobalDispatch<ZwlrForeignToplevelManagerV1, ForeignToplevelGlobalData>,
        D: Dispatch<ExtForeignToplevelListV1, ForeignToplevelUdata>,
        D: Dispatch<ExtForeignToplevelHandleV1, ForeignToplevelUdata>,
        D: Dispatch<ZwlrForeignToplevelManagerV1, ForeignToplevelUdata>,
        D: Dispatch<ZwlrForeignToplevelHandleV1, ForeignToplevelUdata>,
        D: 'static,
    {
        dh.create_global::<D, ExtForeignToplevelListV1, _>(
            EXT_LIST_VERSION,
            ForeignToplevelGlobalData,
        );
        dh.create_global::<D, ZwlrForeignToplevelManagerV1, _>(
            WLR_MANAGEMENT_VERSION,
            ForeignToplevelGlobalData,
        );
        Self::default()
    }

    /// Diff `snapshots` against the announced toplevels and emit the protocol
    /// events for removed, new and changed toplevels.
    fn apply<D>(&mut self, dh: &DisplayHandle, snapshots: Vec<ToplevelSnapshot>)
    where
        D: Dispatch<ExtForeignToplevelHandleV1, ForeignToplevelUdata>
            + Dispatch<ZwlrForeignToplevelHandleV1, ForeignToplevelUdata>
            + 'static,
    {
        // 1. Close toplevels that are gone (destroyed, crashed or transient).
        self.toplevels.retain(|surface, data| {
            let keep = snapshots.iter().any(|snap| snap.surface == *surface);
            if !keep {
                for instance in data.ext_list_instances.iter() {
                    instance.closed();
                }
                for instance in data.wlr_management_instances.keys() {
                    instance.closed();
                }
            }
            keep
        });

        // 2. Create or update the remaining toplevels.
        for snap in snapshots {
            match self.toplevels.entry(snap.surface) {
                Entry::Occupied(mut entry) => {
                    let data = entry.get_mut();

                    let mut new_title = None;
                    if data.title != snap.title {
                        data.title = snap.title.clone();
                        new_title = Some(snap.title.clone());
                    }

                    let mut new_app_id = None;
                    if data.app_id != snap.app_id {
                        data.app_id = snap.app_id.clone();
                        new_app_id = Some(snap.app_id.clone());
                    }

                    let mut states_changed = false;
                    if data.states != snap.states {
                        data.states = snap.states.clone();
                        states_changed = true;
                    }

                    let mut output_changed = false;
                    if data.output.as_ref() != snap.output.as_ref() {
                        data.output = snap.output.clone();
                        output_changed = true;
                    }

                    if new_title.is_some() || new_app_id.is_some() {
                        for instance in &data.ext_list_instances {
                            if let Some(title) = &new_title {
                                instance.title(title.clone());
                            }
                            if let Some(app_id) = &new_app_id {
                                instance.app_id(app_id.clone());
                            }
                            instance.done();
                        }
                    }

                    if new_title.is_some()
                        || new_app_id.is_some()
                        || states_changed
                        || output_changed
                    {
                        for (instance, outputs) in &mut data.wlr_management_instances {
                            if let Some(title) = &new_title {
                                instance.title(title.clone());
                            }
                            if let Some(app_id) = &new_app_id {
                                instance.app_id(app_id.clone());
                            }
                            if states_changed {
                                instance.state(snap.states.clone());
                            }
                            if output_changed {
                                for wl_output in outputs.drain(..) {
                                    instance.output_leave(&wl_output);
                                }
                                if let (Some(output), Some(client)) =
                                    (&data.output, instance.client())
                                {
                                    for wl_output in output.client_outputs(&client) {
                                        instance.output_enter(&wl_output);
                                        outputs.push(wl_output);
                                    }
                                }
                            }
                            instance.done();
                        }
                    }

                    // Clean up dead wl_outputs.
                    for outputs in data.wlr_management_instances.values_mut() {
                        outputs.retain(|wl_output| wl_output.is_alive());
                    }
                }
                Entry::Vacant(entry) => {
                    let mut data = ToplevelData {
                        identifier: self.next_identifier,
                        title: snap.title,
                        app_id: snap.app_id,
                        states: snap.states,
                        output: snap.output,
                        ext_list_instances: HashSet::new(),
                        wlr_management_instances: HashMap::new(),
                    };
                    self.next_identifier += 1;

                    for manager in &self.ext_list_instances {
                        if let Some(client) = manager.client() {
                            data.add_ext_instance::<D>(dh, &client, manager);
                        }
                    }
                    for manager in &self.wlr_management_instances {
                        if let Some(client) = manager.client() {
                            data.add_wlr_instance::<D>(dh, &client, manager);
                        }
                    }

                    entry.insert(data);
                }
            }
        }
    }

    fn surface_for_wlr_handle(&self, resource: &ZwlrForeignToplevelHandleV1) -> Option<WlSurface> {
        self.toplevels
            .iter()
            .find(|(_, data)| data.wlr_management_instances.contains_key(resource))
            .map(|(surface, _)| surface.clone())
    }

    fn remove_ext_instance(&mut self, resource: &ExtForeignToplevelHandleV1) {
        for data in self.toplevels.values_mut() {
            data.ext_list_instances.remove(resource);
        }
    }

    fn remove_wlr_instance(&mut self, resource: &ZwlrForeignToplevelHandleV1) {
        for data in self.toplevels.values_mut() {
            data.wlr_management_instances.remove(resource);
        }
    }
}

/// The wlr `state` event payload for a toplevel in `mode` with `focused`
/// keyboard focus.
fn to_state_vec(mode: WindowMode, focused: bool) -> Vec<u8> {
    let mut states = Vec::new();
    if matches!(mode, WindowMode::Maximized) {
        states.extend((zwlr_foreign_toplevel_handle_v1::State::Maximized as u32).to_ne_bytes());
    }
    if matches!(mode, WindowMode::Fullscreen) {
        states.extend((zwlr_foreign_toplevel_handle_v1::State::Fullscreen as u32).to_ne_bytes());
    }
    if focused {
        states.extend((zwlr_foreign_toplevel_handle_v1::State::Activated as u32).to_ne_bytes());
    }
    states
}

/// The toplevel's current title and app_id.
fn foreign_toplevel_title_app_id(surface: &WlSurface) -> (String, String) {
    with_states(surface, |states| {
        let Some(attrs) = states.data_map.get::<XdgToplevelSurfaceData>() else {
            return (String::new(), String::new());
        };
        let attrs = attrs.lock().unwrap();
        (
            attrs.title.clone().unwrap_or_default(),
            attrs.app_id.clone().unwrap_or_default(),
        )
    })
}

impl<BackendData: Backend + 'static> State<BackendData> {
    /// Reconcile the foreign-toplevel protocols with the current windows.
    /// Called from the backends' idle callbacks. The focused toplevel is
    /// processed last so clients see the old window deactivate first.
    pub fn foreign_toplevel_refresh(&mut self) {
        let focused = self.seat.get_keyboard().unwrap().current_focus();

        let mut snapshots: Vec<ToplevelSnapshot> = Vec::new();
        for (surface, ws) in &self.toplevels {
            if matches!(ws.kind, WindowKind::Transient(_)) || !surface.is_alive() {
                continue;
            }
            let (title, app_id) = foreign_toplevel_title_app_id(surface);
            let states = to_state_vec(ws.mode, focused.as_ref() == Some(surface));
            let output = self
                .space
                .outputs_for_element(&ws.window)
                .into_iter()
                .next();
            snapshots.push(ToplevelSnapshot {
                surface: surface.clone(),
                title,
                app_id,
                states,
                output,
            });
        }

        // Focused last.
        if let Some(focused) = focused {
            snapshots.sort_by_key(|snap| (snap.surface == focused) as u8);
        }

        self.foreign_toplevel
            .apply::<State<BackendData>>(&self.display_handle, snapshots);
    }

    /// wlr `activate` request: raise the toplevel and give it keyboard focus.
    fn foreign_activate(&mut self, surface: &WlSurface) {
        let Some(ws) = self.toplevels.get(surface) else {
            return;
        };
        let window = ws.window.clone();
        self.focus_window(&window, SERIAL_COUNTER.next_serial());
    }

    /// wlr `close` request: ask the toplevel to close.
    fn foreign_close(&mut self, surface: &WlSurface) {
        let Some(ws) = self.toplevels.get(surface) else {
            return;
        };
        if let Some(toplevel) = ws.window.toplevel() {
            toplevel.send_close();
        }
    }
}

impl<BackendData: Backend + 'static> GlobalDispatch2<ExtForeignToplevelListV1, State<BackendData>>
    for ForeignToplevelGlobalData
{
    fn bind(
        &self,
        state: &mut State<BackendData>,
        dh: &DisplayHandle,
        client: &Client,
        resource: New<ExtForeignToplevelListV1>,
        data_init: &mut DataInit<'_, State<BackendData>>,
    ) {
        let manager = data_init.init(resource, ForeignToplevelUdata);

        for data in state.foreign_toplevel.toplevels.values_mut() {
            data.add_ext_instance::<State<BackendData>>(dh, client, &manager);
        }

        state.foreign_toplevel.ext_list_instances.insert(manager);
    }
}

impl<BackendData: Backend + 'static>
    GlobalDispatch2<ZwlrForeignToplevelManagerV1, State<BackendData>>
    for ForeignToplevelGlobalData
{
    fn bind(
        &self,
        state: &mut State<BackendData>,
        dh: &DisplayHandle,
        client: &Client,
        resource: New<ZwlrForeignToplevelManagerV1>,
        data_init: &mut DataInit<'_, State<BackendData>>,
    ) {
        let manager = data_init.init(resource, ForeignToplevelUdata);

        for data in state.foreign_toplevel.toplevels.values_mut() {
            data.add_wlr_instance::<State<BackendData>>(dh, client, &manager);
        }

        state
            .foreign_toplevel
            .wlr_management_instances
            .insert(manager);
    }
}

impl<BackendData: Backend + 'static> Dispatch2<ExtForeignToplevelListV1, State<BackendData>>
    for ForeignToplevelUdata
{
    fn request(
        &self,
        state: &mut State<BackendData>,
        _client: &Client,
        resource: &ExtForeignToplevelListV1,
        request: ext_foreign_toplevel_list_v1::Request,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, State<BackendData>>,
    ) {
        match request {
            ext_foreign_toplevel_list_v1::Request::Stop => {
                resource.finished();
                // Remove the instance so no further events are sent.
                state.foreign_toplevel.ext_list_instances.remove(resource);
            }
            ext_foreign_toplevel_list_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(
        &self,
        state: &mut State<BackendData>,
        _client: ClientId,
        resource: &ExtForeignToplevelListV1,
    ) {
        // Also remove the instance here, in case `stop` was never sent
        // (e.g. sudden disconnect).
        state.foreign_toplevel.ext_list_instances.remove(resource);
    }
}

impl<BackendData: Backend + 'static> Dispatch2<ExtForeignToplevelHandleV1, State<BackendData>>
    for ForeignToplevelUdata
{
    fn request(
        &self,
        _state: &mut State<BackendData>,
        _client: &Client,
        _resource: &ExtForeignToplevelHandleV1,
        request: ext_foreign_toplevel_handle_v1::Request,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, State<BackendData>>,
    ) {
        match request {
            ext_foreign_toplevel_handle_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(
        &self,
        state: &mut State<BackendData>,
        _client: ClientId,
        resource: &ExtForeignToplevelHandleV1,
    ) {
        state.foreign_toplevel.remove_ext_instance(resource);
    }
}

impl<BackendData: Backend + 'static> Dispatch2<ZwlrForeignToplevelManagerV1, State<BackendData>>
    for ForeignToplevelUdata
{
    fn request(
        &self,
        state: &mut State<BackendData>,
        _client: &Client,
        resource: &ZwlrForeignToplevelManagerV1,
        request: zwlr_foreign_toplevel_manager_v1::Request,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, State<BackendData>>,
    ) {
        match request {
            zwlr_foreign_toplevel_manager_v1::Request::Stop => {
                resource.finished();
                state
                    .foreign_toplevel
                    .wlr_management_instances
                    .remove(resource);
            }
            _ => unreachable!(),
        }
    }

    fn destroyed(
        &self,
        state: &mut State<BackendData>,
        _client: ClientId,
        resource: &ZwlrForeignToplevelManagerV1,
    ) {
        state
            .foreign_toplevel
            .wlr_management_instances
            .remove(resource);
    }
}

impl<BackendData: Backend + 'static> Dispatch2<ZwlrForeignToplevelHandleV1, State<BackendData>>
    for ForeignToplevelUdata
{
    fn request(
        &self,
        state: &mut State<BackendData>,
        _client: &Client,
        resource: &ZwlrForeignToplevelHandleV1,
        request: zwlr_foreign_toplevel_handle_v1::Request,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, State<BackendData>>,
    ) {
        let surface = state.foreign_toplevel.surface_for_wlr_handle(resource);
        let Some(surface) = surface else {
            return;
        };

        match request {
            zwlr_foreign_toplevel_handle_v1::Request::SetMaximized
            | zwlr_foreign_toplevel_handle_v1::Request::UnsetMaximized
            | zwlr_foreign_toplevel_handle_v1::Request::SetMinimized
            | zwlr_foreign_toplevel_handle_v1::Request::UnsetMinimized
            | zwlr_foreign_toplevel_handle_v1::Request::SetRectangle { .. }
            | zwlr_foreign_toplevel_handle_v1::Request::SetFullscreen { .. }
            | zwlr_foreign_toplevel_handle_v1::Request::UnsetFullscreen => {
                // State changes are driven by the compositor's own policy
                // (reflected back via the `state` event on the next refresh);
                // there is no client-requested maximize/minimize here.
            }
            zwlr_foreign_toplevel_handle_v1::Request::Activate { .. } => {
                state.foreign_activate(&surface);
            }
            zwlr_foreign_toplevel_handle_v1::Request::Close => {
                state.foreign_close(&surface);
            }
            zwlr_foreign_toplevel_handle_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(
        &self,
        state: &mut State<BackendData>,
        _client: ClientId,
        resource: &ZwlrForeignToplevelHandleV1,
    ) {
        state.foreign_toplevel.remove_wlr_instance(resource);
    }
}
