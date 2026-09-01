//! High-level networking example: client-server TCP lobby synchronization.
//!
//! This example binds a server to a dynamically allocated loopback port,
//! connects a client to it, registers a type-safe `MessageRouter` to handle
//! lobby joins, and uses ECS systems and events to poll and update state.
//!
//! Run from the workspace root:
//!
//! ```text
//! cargo run -p yuyib --example networking_lobby
//! ```

use std::collections::HashMap;
use std::error::Error;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use serde::{Deserialize, Serialize};
use yuyib::{
    ecs::bevy_ecs::message::{MessageCursor, MessageRegistry, Messages},
    ecs::prelude::*,
    game::{Game, GamePlugin, GameSchedule},
    net::*,
    platform::WindowConfig,
};

// --- Serializable Messages ---

#[derive(Serialize, Deserialize, Debug, Clone)]
struct JoinLobby {
    username: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct LobbyStatus {
    players: Vec<String>,
}

// --- ECS Resources ---

#[derive(Resource)]
struct ServerLobby {
    players: HashMap<ClientId, String>,
}

#[derive(Resource)]
struct ClientLobby {
    players: Vec<String>,
}

#[derive(Resource)]
struct ServerRouter {
    router: MessageRouter<ServerLobby>,
}

// --- ECS Plugin ---

struct LobbyPlugin;

impl GamePlugin for LobbyPlugin {
    fn build(self, game: &mut Game) {
        // Initialize state resources
        game.world_mut().insert_resource(ServerLobby {
            players: HashMap::new(),
        });
        game.world_mut().insert_resource(ClientLobby {
            players: Vec::new(),
        });

        // Initialize and configure type-safe server message router
        let mut router = MessageRouter::new();
        router.register("lobby.join", |lobby: &mut ServerLobby, client_id, msg: JoinLobby| {
            println!("Server Router: Client {} requested to join as '{}'", client_id, msg.username);
            lobby.players.insert(client_id, msg.username);
        });
        game.world_mut().insert_resource(ServerRouter { router });

        // Register custom networking message queues in the ECS world
        MessageRegistry::register_message::<EcsClientEvent>(game.world_mut());
        MessageRegistry::register_message::<EcsServerEvent>(game.world_mut());

        // Add startup systems
        game.schedule_mut(GameSchedule::Startup)
            .add_systems(setup_networking);

        // Add update systems
        game.schedule_mut(GameSchedule::Update).add_systems((
            poll_client_events_system,
            poll_server_events_system,
            server_handle_events,
            client_handle_events,
        ));
    }
}

// --- ECS Systems ---

/// Set up server and client networking resources.
fn setup_networking(mut commands: Commands) {
    println!("System: Setting up networking...");

    let limits = FrameLimits::default();
    let codec = FrameCodec::new(limits).expect("valid frame codec");

    // Bind server to a dynamic loopback port
    let bind_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
    let server = NetServer::bind(bind_addr, codec, ServerConfig::default())
        .expect("failed to bind server");

    let bound_addr = server.local_addr();
    println!("System: Server listening on {}", bound_addr);

    // Connect client to the server's dynamically bound address
    let client = NetClient::connect(bound_addr, codec, ClientConfig::default());
    println!("System: Client initiated connection to {}", bound_addr);

    commands.insert_resource(NetworkServer::new(server));
    commands.insert_resource(NetworkClient::new(client));
}

/// Server system to process events and route messages.
#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy ECS systems receive parameters by value"
)]
fn server_handle_events(
    messages: Option<Res<Messages<EcsServerEvent>>>,
    mut cursor: Local<MessageCursor<EcsServerEvent>>,
    mut lobby: ResMut<ServerLobby>,
    mut router: ResMut<ServerRouter>,
    server: Option<Res<NetworkServer>>,
) {
    let (Some(messages), Some(server)) = (messages, server) else { return };
    let mut state_changed = false;

    for event in cursor.read(&messages) {
        match event {
            EcsServerEvent::ClientConnected(client_id) => {
                println!("Server ECS: Client {} connected", client_id);
            }
            EcsServerEvent::ClientDisconnected(client_id) => {
                println!("Server ECS: Client {} disconnected", client_id);
                if lobby.players.remove(&client_id).is_some() {
                    state_changed = true;
                }
            }
            EcsServerEvent::ClientDisconnectedWithReason(client_id, reason) => {
                println!(
                    "Server ECS: Client {} disconnected ({:?})",
                    client_id,
                    reason
                );
                if lobby.players.remove(&client_id).is_some() {
                    state_changed = true;
                }
            }
            EcsServerEvent::ClientMessage(client_id, frame) => {
                match router.router.handle(&mut lobby, *client_id, &frame) {
                    Ok(true) => {
                        state_changed = true;
                    }
                    Ok(false) => {
                        println!(
                            "Server ECS: Received unhandled message type: {}",
                            frame.message_type()
                        );
                    }
                    Err(error) => {
                        println!("Server ECS: Failed to deserialize message: {}", error);
                    }
                }
            }
        }
    }

    if state_changed {
        let player_names: Vec<String> = lobby.players.values().cloned().collect();
        println!("Server ECS: Broadcasting lobby status: {:?}", player_names);
        let _ = server.server.broadcast("lobby.status", &LobbyStatus {
            players: player_names,
        });
    }
}

/// Client system to handle connection, send joins, and receive lobby status.
#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy ECS systems receive parameters by value"
)]
fn client_handle_events(
    messages: Option<Res<Messages<EcsClientEvent>>>,
    mut cursor: Local<MessageCursor<EcsClientEvent>>,
    mut lobby: ResMut<ClientLobby>,
    client: Option<Res<NetworkClient>>,
) {
    let (Some(messages), Some(client)) = (messages, client) else { return };

    for event in cursor.read(&messages) {
        match event {
            EcsClientEvent::Connected => {
                println!("Client ECS: Connected to server. Joining lobby...");
                let join_msg = JoinLobby {
                    username: "GamerPro_9000".to_owned(),
                };
                if let Err(error) = client.client.send("lobby.join", &join_msg) {
                    println!("Client ECS: Failed to send join request: {}", error);
                }
            }
            EcsClientEvent::Disconnected => {
                println!("Client ECS: Disconnected from server");
            }
            EcsClientEvent::DisconnectedWithReason(reason) => {
                println!("Client ECS: Disconnected from server ({:?})", reason);
            }
            EcsClientEvent::Message(frame) => {
                if frame.message_type().as_str() == "lobby.status" {
                    match serde_json::from_slice::<LobbyStatus>(frame.payload()) {
                        Ok(status) => {
                            println!("Client ECS: Received Lobby Status: {:?}", status.players);
                            lobby.players = status.players;
                        }
                        Err(error) => {
                            println!("Client ECS: Failed to parse lobby status: {}", error);
                        }
                    }
                }
            }
            EcsClientEvent::Error(error) => {
                println!("Client ECS: Error: {}", error);
            }
        }
    }
}

// --- Main Application ---

fn main() -> Result<(), Box<dyn Error>> {
    println!("Starting Yuyib High-Level Networking Demo...");

    // Create and enter a multi-threaded Tokio runtime context.
    // This allows background tasks spawned by systems to run successfully.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let _guard = rt.enter();

    Game::new()
        .window(WindowConfig {
            title: "Yuyib — Lobby Networking Example".to_owned(),
            ..Default::default()
        })
        .add_plugin(LobbyPlugin)
        .on_frame(|frame| {
            let mut success = false;
            if let Some(lobby) = frame.world().get_resource::<ClientLobby>() {
                if !lobby.players.is_empty() {
                    println!("Client ECS: Lobby synchronized successfully with players: {:?}", lobby.players);
                    success = true;
                }
            }

            if success {
                println!("Lobby exchange test complete! Exiting example successfully.");
                frame.request_exit();
            } else if frame.frame().index >= 1000 {
                println!("Error: Timeout waiting for lobby synchronization. Exiting.");
                frame.request_exit();
            }
        })
        .run()?;

    Ok(())
}
