#![cfg(feature = "ecs")]

use bevy_ecs::message::Messages;
use bevy_ecs::prelude::*;
use crate::high_level::{ClientEvent, NetClient, NetServer, ServerEvent};
use crate::WireFrame;

/// ECS resource wrapping the thread-safe `NetClient`.
#[derive(Resource)]
pub struct NetworkClient {
    /// Inner network client instance.
    pub client: NetClient,
}

impl NetworkClient {
    /// Creates a new resource wrapping the network client.
    #[must_use]
    pub fn new(client: NetClient) -> Self {
        Self { client }
    }
}

/// ECS resource wrapping the thread-safe `NetServer`.
#[derive(Resource)]
pub struct NetworkServer {
    /// Inner network server instance.
    pub server: NetServer,
}

impl NetworkServer {
    /// Creates a new resource wrapping the network server.
    #[must_use]
    pub fn new(server: NetServer) -> Self {
        Self { server }
    }
}

/// ECS message representing client connection state and message events.
#[derive(Message, Clone, Debug)]
pub enum EcsClientEvent {
    /// Client successfully connected to the server.
    Connected,
    /// Client disconnected from the server.
    Disconnected,
    /// Client received a message frame from the server.
    Message(WireFrame),
    /// An error occurred during client connection or I/O.
    Error(String),
}

/// ECS message representing server connection state and client message events.
#[derive(Message, Clone, Debug)]
pub enum EcsServerEvent {
    /// A new client session connected.
    ClientConnected(u64),
    /// A client session disconnected.
    ClientDisconnected(u64),
    /// A client sent a message frame to the server.
    ClientMessage(u64, WireFrame),
}

/// System that polls events from the background client task and publishes them to the ECS message queue.
pub fn poll_client_events_system(
    client: Option<ResMut<NetworkClient>>,
    messages: Option<ResMut<Messages<EcsClientEvent>>>,
) {
    if let (Some(mut client), Some(mut messages)) = (client, messages) {
        while let Some(event) = client.client.poll_event() {
            let ecs_event = match event {
                ClientEvent::Connected => EcsClientEvent::Connected,
                ClientEvent::Disconnected => EcsClientEvent::Disconnected,
                ClientEvent::Message(frame) => EcsClientEvent::Message(frame),
                ClientEvent::Error(error) => EcsClientEvent::Error(error),
            };
            messages.write(ecs_event);
        }
    }
}

/// System that polls events from the background server task and publishes them to the ECS message queue.
pub fn poll_server_events_system(
    server: Option<ResMut<NetworkServer>>,
    messages: Option<ResMut<Messages<EcsServerEvent>>>,
) {
    if let (Some(mut server), Some(mut messages)) = (server, messages) {
        while let Some(event) = server.server.poll_event() {
            let ecs_event = match event {
                ServerEvent::ClientConnected(client_id) => EcsServerEvent::ClientConnected(client_id),
                ServerEvent::ClientDisconnected(client_id) => EcsServerEvent::ClientDisconnected(client_id),
                ServerEvent::ClientMessage(client_id, frame) => EcsServerEvent::ClientMessage(client_id, frame),
            };
            messages.write(ecs_event);
        }
    }
}
