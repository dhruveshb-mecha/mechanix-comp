use std::sync::Arc;

use smithay::reexports::{
    calloop::{EventLoop, Interest, Mode, PostAction, generic::Generic},
    wayland_server::{
        Display, ListeningSocket,
        backend::{ClientData, ClientId, DisconnectReason},
    },
};

struct State;

struct ClientState;

impl ClientData for ClientState {
    fn initialized(&self, _: ClientId) {}
    fn disconnected(&self, _: ClientId, _: DisconnectReason) {}
}

fn main() {
    let mut event_loop: EventLoop<State> = EventLoop::try_new().unwrap();
    let mut display: Display<State> = Display::new().unwrap();
    let dh = display.handle();

    let socket = ListeningSocket::bind_auto("wayland", 1..33).unwrap();
    println!("Listening on {:?}", socket.socket_name());

    let mut dh_clone = dh.clone();
    event_loop
        .handle()
        .insert_source(
            Generic::new(socket, Interest::READ, Mode::Level),
            move |_, socket, _| {
                while let Some(client) = socket.accept().unwrap() {
                    dh_clone.insert_client(client, Arc::new(ClientState)).unwrap();
                }
                Ok(PostAction::Continue)
            },
        )
        .unwrap();

    let mut state = State;

    loop {
        event_loop
            .dispatch(std::time::Duration::from_millis(16), &mut state)
            .unwrap();
        display.dispatch_clients(&mut state).unwrap();
        display.flush_clients().unwrap();
    }
}
