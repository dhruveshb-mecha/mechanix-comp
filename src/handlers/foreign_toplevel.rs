//! Our zwlr-foreign-toplevel-management-v1 + smithay's ext-foreign-toplevel-list-v1, driven by [`State::foreign_toplevel_refresh`].

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};

use smithay::output::Output;
use smithay::reexports::wayland_protocols::ext::foreign_toplevel_list::v1::server::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1;
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
use smithay::wayland::foreign_toplevel_list::{
    ForeignToplevelHandle, ForeignToplevelListHandler, ForeignToplevelListState,
};
use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;
use smithay::wayland::{Dispatch2, GlobalDispatch2};

use crate::backend::Backend;
use crate::state::{State, WindowMode};

const WLR_MANAGEMENT_VERSION: u32 = 3;

/// Marker so the wlr global and resources dispatch here.
#[derive(Default)]
pub struct ForeignToplevelUdata;

/// What the foreign-toplevel protocols need to know about one toplevel.
#[derive(Debug)]
pub struct ToplevelData {
    title: String,
    app_id: String,
    /// The wlr `state` event payload: maximized/fullscreen/activated flags.
    states: Vec<u8>,
    /// The output the toplevel is currently on, for `output_enter`/`leave`.
    output: Option<Output>,
    /// Per-client wlr handles, each with the outputs entered so far.
    wlr_management_instances: HashMap<ZwlrForeignToplevelHandleV1, Vec<WlOutput>>,
    /// smithay's ext-list handle; owns all ext instance bookkeeping.
    ext_handle: Option<ForeignToplevelHandle>,
}

impl ToplevelData {
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

/// A toplevel snapshot at refresh time, for diffing against announced data.
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
    wlr_management_instances: HashSet<ZwlrForeignToplevelManagerV1>,
    toplevels: HashMap<WlSurface, ToplevelData>,
}

impl ForeignToplevelManagerState {
    /// Register the wlr management global; ext-list lives in `State::foreign_toplevel_list`.
    pub fn new<D>(dh: &DisplayHandle) -> Self
    where
        D: GlobalDispatch<ZwlrForeignToplevelManagerV1, ForeignToplevelUdata>,
        D: Dispatch<ZwlrForeignToplevelManagerV1, ForeignToplevelUdata>,
        D: Dispatch<ZwlrForeignToplevelHandleV1, ForeignToplevelUdata>,
        D: 'static,
    {
        dh.create_global::<D, ZwlrForeignToplevelManagerV1, _>(
            WLR_MANAGEMENT_VERSION,
            ForeignToplevelUdata,
        );
        Self::default()
    }

    /// Diff snapshots against announced toplevels, emitting events; also drives ext-list in the same pass.
    fn apply<D>(
        &mut self,
        dh: &DisplayHandle,
        list: &mut ForeignToplevelListState,
        snapshots: Vec<ToplevelSnapshot>,
    ) where
        D: ForeignToplevelListHandler
            + Dispatch<ExtForeignToplevelHandleV1, ForeignToplevelHandle>
            + Dispatch<ZwlrForeignToplevelHandleV1, ForeignToplevelUdata>
            + 'static,
    {
        // 1. Close toplevels that are gone (destroyed, crashed or transient).
        self.toplevels.retain(|surface, data| {
            let keep = snapshots.iter().any(|snap| snap.surface == *surface);
            if !keep {
                if let Some(handle) = data.ext_handle.take() {
                    list.remove_toplevel(&handle);
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

                    // ext handle stays in sync with the diff above, so only touch it on changes.
                    if (new_title.is_some() || new_app_id.is_some())
                        && let Some(handle) = &data.ext_handle
                    {
                        if let Some(title) = &new_title {
                            handle.send_title(title);
                        }
                        if let Some(app_id) = &new_app_id {
                            handle.send_app_id(app_id);
                        }
                        handle.send_done();
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
                        title: snap.title,
                        app_id: snap.app_id,
                        states: snap.states,
                        output: snap.output,
                        wlr_management_instances: HashMap::new(),
                        ext_handle: None,
                    };
                    data.ext_handle =
                        Some(list.new_toplevel::<D>(data.title.clone(), data.app_id.clone()));
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

    fn remove_wlr_instance(&mut self, resource: &ZwlrForeignToplevelHandleV1) {
        for data in self.toplevels.values_mut() {
            data.wlr_management_instances.remove(resource);
        }
    }
}

/// The wlr `state` payload for a toplevel in `mode` with `focused` focus.
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
    /// Reconcile both protocols with the current windows; focused last so old windows deactivate first.
    pub fn foreign_toplevel_refresh(&mut self) {
        let focused = self.seat.get_keyboard().unwrap().current_focus();

        let mut snapshots: Vec<ToplevelSnapshot> = Vec::new();
        for (surface, ws) in &self.toplevels {
            if !surface.is_alive() {
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

        self.foreign_toplevel.apply::<State<BackendData>>(
            &self.display_handle,
            &mut self.foreign_toplevel_list,
            snapshots,
        );
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

impl<BackendData: Backend + 'static> ForeignToplevelListHandler for State<BackendData> {
    fn foreign_toplevel_list_state(&mut self) -> &mut ForeignToplevelListState {
        &mut self.foreign_toplevel_list
    }
}

impl<BackendData: Backend + 'static>
    GlobalDispatch2<ZwlrForeignToplevelManagerV1, State<BackendData>> for ForeignToplevelUdata
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
                // State is compositor-driven and reflected back on the next refresh.
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
