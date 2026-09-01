use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};

use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::mpsc;

use crate::{FrameCodec, MessageType, ProtocolVersion, WireFrame};

/// Unique identifier for a connected client session.
pub type ClientId = u64;

/// Connection lifecycle states of a network client.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientState {
    /// Client is not connected and is not attempting to connect.
    Disconnected = 0,
    /// Client is attempting to establish the initial connection.
    Connecting = 1,
    /// Client is actively connected to the server.
    Connected = 2,
    /// Client has lost connection and is trying to reconnect.
    Reconnecting = 3,
}

/// Events emitted by a high-level client.
#[derive(Clone, Debug)]
pub enum ClientEvent {
    /// Dispatched once connection is established.
    Connected,
    /// Dispatched when disconnected from the server.
    Disconnected,
    /// Disconnected with a detailed reason.
    DisconnectedWithReason(DisconnectReason),
    /// Dispatched when a custom application message frame is received.
    Message(WireFrame),
    /// Dispatched when an error occurs during connection or I/O.
    Error(String),
}

/// Events emitted by a high-level server.
#[derive(Clone, Debug)]
pub enum ServerEvent {
    /// Dispatched when a new client connects.
    ClientConnected(ClientId),
    /// Dispatched when a client session terminates.
    ClientDisconnected(ClientId),
    /// Dispatched when a client session terminates with a reason.
    ClientDisconnectedWithReason(ClientId, DisconnectReason),
    /// Dispatched when a client sends a custom application message frame.
    ClientMessage(ClientId, WireFrame),
}

/// Reason why a network session ended.
#[derive(Clone, Debug)]
pub enum DisconnectReason {
    /// Local shutdown requested.
    LocalShutdown,
    /// The connection was explicitly terminated by the server.
    AdminDisconnect,
    /// Remote side closed the transport socket.
    RemoteClosed,
    /// Heartbeat timeout while waiting for pong.
    HeartbeatTimeout,
    /// Retry budget exhausted while reconnecting (client-side only).
    ReconnectExhausted,
    /// Underlying transport error.
    TransportError(String),
    /// Unexpected protocol parsing or decoding error.
    ProtocolError(String),
}

/// Configuration settings for the network client.
#[derive(Clone, Copy, Debug)]
pub struct ClientConfig {
    /// Interval at which the client sends ping heartbeats.
    pub heartbeat_interval: Duration,
    /// Timeout duration to wait for a pong response before disconnecting.
    pub heartbeat_timeout: Duration,
    /// Delay between consecutive reconnection attempts.
    pub reconnect_interval: Duration,
    /// Maximum number of reconnect retries before giving up.
    pub max_reconnect_attempts: usize,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            heartbeat_interval: Duration::from_secs(4),
            heartbeat_timeout: Duration::from_secs(8),
            reconnect_interval: Duration::from_secs(2),
            max_reconnect_attempts: 5,
        }
    }
}

/// Configuration settings for the network server.
#[derive(Clone, Copy, Debug)]
pub struct ServerConfig {
    /// Interval at which the server sends ping heartbeats.
    pub heartbeat_interval: Duration,
    /// Timeout duration to wait for a client's pong response.
    pub heartbeat_timeout: Duration,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            heartbeat_interval: Duration::from_secs(4),
            heartbeat_timeout: Duration::from_secs(8),
        }
    }
}

fn heartbeat_miss_limit(heartbeat_timeout: Duration, heartbeat_interval: Duration) -> u64 {
    if heartbeat_timeout.is_zero() {
        return u64::MAX;
    }

    let interval = if heartbeat_interval.is_zero() {
        Duration::from_millis(1)
    } else {
        heartbeat_interval
    };

    let limit = heartbeat_timeout
        .as_nanos()
        .div_ceil(interval.as_nanos())
        .max(1);
    u64::try_from(limit).unwrap_or(u64::MAX)
}

/// Thread-safe client facade communicating with a background connection task.
pub struct NetClient {
    outbound_tx: mpsc::Sender<WireFrame>,
    events_rx: mpsc::Receiver<ClientEvent>,
    state: Arc<AtomicU8>,
}

impl NetClient {
    /// Connects to a remote server asynchronously in a spawned background task.
    #[must_use]
    pub fn connect(address: SocketAddr, codec: FrameCodec, config: ClientConfig) -> Self {
        let (outbound_tx, outbound_rx) = mpsc::channel(1024);
        let (events_tx, events_rx) = mpsc::channel(1024);
        let state = Arc::new(AtomicU8::new(ClientState::Connecting as u8));

        let state_clone = Arc::clone(&state);
        tokio::spawn(async move {
            client_worker_loop(address, codec, config, outbound_rx, events_tx, state_clone).await;
        });

        Self {
            outbound_tx,
            events_rx,
            state,
        }
    }

    /// Returns the current connection state of the client.
    #[must_use]
    pub fn state(&self) -> ClientState {
        match self.state.load(Ordering::Relaxed) {
            0 => ClientState::Disconnected,
            1 => ClientState::Connecting,
            2 => ClientState::Connected,
            3 => ClientState::Reconnecting,
            _ => ClientState::Disconnected,
        }
    }

    /// Polls for the next client event in a non-blocking manner.
    pub fn poll_event(&mut self) -> Option<ClientEvent> {
        self.events_rx.try_recv().ok()
    }

    /// Sends a JSON-serialized payload to the server.
    ///
    /// # Errors
    ///
    /// Returns an error if the message type name is invalid, serialization fails,
    /// or the background connection loop is shut down.
    pub fn send<T: Serialize>(&self, message_type: &str, payload: &T) -> Result<(), String> {
        let msg_type = MessageType::new(message_type).map_err(|error| error.to_string())?;
        let payload_bytes = serde_json::to_vec(payload).map_err(|error| error.to_string())?;
        let frame = WireFrame::new(ProtocolVersion::new(1), msg_type, payload_bytes);
        self.outbound_tx
            .try_send(frame)
            .map_err(|error| error.to_string())
    }
}

async fn client_worker_loop(
    address: SocketAddr,
    codec: FrameCodec,
    config: ClientConfig,
    mut outbound_rx: mpsc::Receiver<WireFrame>,
    events_tx: mpsc::Sender<ClientEvent>,
    state: Arc<AtomicU8>,
) {
    let mut reconnect_attempts = 0;

    let ping_type = MessageType::new("sys.ping").unwrap();
    let pong_type = MessageType::new("sys.pong").unwrap();

    loop {
        state.store(
            if reconnect_attempts > 0 {
                ClientState::Reconnecting
            } else {
                ClientState::Connecting
            } as u8,
            Ordering::Relaxed,
        );

        match crate::connect(address, codec).await {
            Ok(mut connection) => {
                reconnect_attempts = 0;
                state.store(ClientState::Connected as u8, Ordering::Relaxed);
                if events_tx.send(ClientEvent::Connected).await.is_err() {
                    return;
                }

                let mut ping_interval = tokio::time::interval(config.heartbeat_interval);
                ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                let mut missed_pongs = 0;
                let heartbeat_limit = heartbeat_miss_limit(config.heartbeat_timeout, config.heartbeat_interval);
                let disconnect_reason = 'session: loop {
                    tokio::select! {
                        frame_res = connection.read_frame() => {
                            match frame_res {
                                Ok(frame) => {
                                    if frame.message_type() == &ping_type {
                                        let reply = WireFrame::new(frame.version(), pong_type.clone(), Vec::new());
                                        if connection.write_frame(&reply).await.is_err() {
                                            break 'session Some(DisconnectReason::TransportError(
                                                "failed to send pong".to_owned(),
                                            ));
                                        }
                                    } else if frame.message_type() == &pong_type {
                                        missed_pongs = 0;
                                    } else {
                                        if events_tx.send(ClientEvent::Message(frame)).await.is_err() {
                                            return;
                                        }
                                    }
                                }
                                    Err(error) => {
                                        let _ = events_tx
                                            .send(ClientEvent::Error(error.to_string()))
                                            .await;
                                    break 'session Some(DisconnectReason::TransportError(error.to_string()));
                                }
                            }
                        }
                        outbound = outbound_rx.recv() => {
                            if let Some(frame) = outbound {
                                if connection.write_frame(&frame).await.is_err() {
                                    break 'session Some(DisconnectReason::TransportError(
                                        "failed to send application frame".to_owned(),
                                    ));
                                }
                            } else {
                                // outbound channel dropped -> shutdown client
                                break 'session Some(DisconnectReason::LocalShutdown);
                            }
                        }
                        _ = ping_interval.tick() => {
                            if heartbeat_limit != u64::MAX && missed_pongs >= heartbeat_limit {
                                break 'session Some(DisconnectReason::HeartbeatTimeout);
                            }
                            missed_pongs += 1;
                            let ping_frame = WireFrame::new(ProtocolVersion::new(1), ping_type.clone(), Vec::new());
                            if connection.write_frame(&ping_frame).await.is_err() {
                                break 'session Some(DisconnectReason::TransportError(
                                    "failed to send heartbeat".to_owned(),
                                ));
                            }
                        }
                    }
                }.unwrap_or(DisconnectReason::RemoteClosed);

                state.store(ClientState::Disconnected as u8, Ordering::Relaxed);
                let event = ClientEvent::DisconnectedWithReason(disconnect_reason);
                if events_tx.send(event).await.is_err() {
                    return;
                }
            }
            Err(error) => {
                reconnect_attempts += 1;
                let _ = events_tx
                    .send(ClientEvent::Error(format!("Connection failed: {error}")))
                    .await;
                if reconnect_attempts > config.max_reconnect_attempts {
                    state.store(ClientState::Disconnected as u8, Ordering::Relaxed);
                    let _ = events_tx
                        .send(ClientEvent::DisconnectedWithReason(
                            DisconnectReason::ReconnectExhausted,
                        ))
                        .await;
                    return;
                }
                tokio::time::sleep(config.reconnect_interval).await;
            }
        }
    }
}

/// Commands sent from the server facade to the server worker task.
enum ServerCommand {
    Send(ClientId, WireFrame),
    Broadcast(WireFrame),
    BroadcastExcept(WireFrame, ClientId),
    Disconnect(ClientId),
}

enum ClientCommand {
    Outbound(WireFrame),
    Disconnect,
}

/// Thread-safe server facade communicating with an accept loop and client session tasks.
pub struct NetServer {
    events_rx: mpsc::Receiver<ServerEvent>,
    commands_tx: mpsc::Sender<ServerCommand>,
    local_addr: SocketAddr,
}

impl NetServer {
    /// Binds to a network address and starts accepting connections asynchronously.
    ///
    /// # Errors
    ///
    /// Returns a `BindError` if the TCP address cannot be bound.
    pub fn bind(address: SocketAddr, codec: FrameCodec, config: ServerConfig) -> Result<Self, crate::BindError> {
        let (events_tx, events_rx) = mpsc::channel(1024);
        let (commands_tx, commands_rx) = mpsc::channel(1024);

        let listener = std::net::TcpListener::bind(address).map_err(|source| crate::BindError {
            address,
            source,
        })?;

        let local_addr = listener.local_addr().map_err(|source| crate::BindError {
            address,
            source,
        })?;

        tokio::spawn(async move {
            let tokio_listener = tokio::net::TcpListener::from_std(listener).expect("tokio listener");
            server_worker_loop(tokio_listener, codec, config, commands_rx, events_tx).await;
        });

        Ok(Self {
            events_rx,
            commands_tx,
            local_addr,
        })
    }

    /// Returns the local socket address this server is listening on.
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Polls for the next server event (new connection, message, disconnect).
    pub fn poll_event(&mut self) -> Option<ServerEvent> {
        self.events_rx.try_recv().ok()
    }

    /// Sends a JSON-serialized message to a specific client.
    ///
    /// # Errors
    ///
    /// Returns an error if the message type name is invalid, serialization fails,
    /// or the background server loop is shut down.
    pub fn send<T: Serialize>(&self, client_id: ClientId, message_type: &str, payload: &T) -> Result<(), String> {
        let msg_type = MessageType::new(message_type).map_err(|error| error.to_string())?;
        let payload_bytes = serde_json::to_vec(payload).map_err(|error| error.to_string())?;
        let frame = WireFrame::new(ProtocolVersion::new(1), msg_type, payload_bytes);
        self.commands_tx
            .try_send(ServerCommand::Send(client_id, frame))
            .map_err(|error| error.to_string())
    }

    /// Broadcasts a JSON-serialized message to all connected clients.
    ///
    /// # Errors
    ///
    /// Returns an error if the message type name is invalid, serialization fails,
    /// or the background server loop is shut down.
    pub fn broadcast<T: Serialize>(&self, message_type: &str, payload: &T) -> Result<(), String> {
        let msg_type = MessageType::new(message_type).map_err(|error| error.to_string())?;
        let payload_bytes = serde_json::to_vec(payload).map_err(|error| error.to_string())?;
        let frame = WireFrame::new(ProtocolVersion::new(1), msg_type, payload_bytes);
        self.commands_tx
            .try_send(ServerCommand::Broadcast(frame))
            .map_err(|error| error.to_string())
    }

    /// Broadcasts a JSON-serialized message to all clients except one.
    ///
    /// # Errors
    ///
    /// Returns an error if the message type name is invalid, serialization fails,
    /// or the background server loop is shut down.
    pub fn broadcast_except<T: Serialize>(&self, message_type: &str, payload: &T, except: ClientId) -> Result<(), String> {
        let msg_type = MessageType::new(message_type).map_err(|error| error.to_string())?;
        let payload_bytes = serde_json::to_vec(payload).map_err(|error| error.to_string())?;
        let frame = WireFrame::new(ProtocolVersion::new(1), msg_type, payload_bytes);
        self.commands_tx
            .try_send(ServerCommand::BroadcastExcept(frame, except))
            .map_err(|error| error.to_string())
    }

    /// Forcefully disconnects a specific client.
    ///
    /// # Errors
    ///
    /// Returns an error if the server worker loop is shut down.
    pub fn disconnect(&self, client_id: ClientId) -> Result<(), String> {
        self.commands_tx
            .try_send(ServerCommand::Disconnect(client_id))
            .map_err(|error| error.to_string())
    }
}

enum ClientTaskEvent {
    Message(WireFrame),
    Disconnected(DisconnectReason),
}

async fn server_worker_loop(
    listener: tokio::net::TcpListener,
    codec: FrameCodec,
    config: ServerConfig,
    mut commands_rx: mpsc::Receiver<ServerCommand>,
    events_tx: mpsc::Sender<ServerEvent>,
) {
    let mut clients: HashMap<ClientId, mpsc::Sender<ClientCommand>> = HashMap::new();
    let mut next_client_id = 1;

    let (client_events_tx, mut client_events_rx) = mpsc::channel::<(ClientId, ClientTaskEvent)>(1024);

    loop {
        tokio::select! {
            accept_res = listener.accept() => {
                if let Ok((stream, _peer_addr)) = accept_res {
                    let _ = stream.set_nodelay(true);
                    let client_id = next_client_id;
                    next_client_id += 1;

                    let (client_tx, client_rx) = mpsc::channel(128);
                    clients.insert(client_id, client_tx);

                    let client_events_tx_clone = client_events_tx.clone();
                    tokio::spawn(async move {
                        client_session_task(client_id, stream, codec, config, client_rx, client_events_tx_clone).await;
                    });

                    if events_tx.send(ServerEvent::ClientConnected(client_id)).await.is_err() {
                        return;
                    }
                }
            }
            client_event = client_events_rx.recv() => {
                if let Some((client_id, event)) = client_event {
                    match event {
                        ClientTaskEvent::Message(frame) => {
                            if events_tx.send(ServerEvent::ClientMessage(client_id, frame)).await.is_err() {
                                return;
                            }
                        }
                        ClientTaskEvent::Disconnected(reason) => {
                            if clients.remove(&client_id).is_some() {
                                if events_tx
                                    .send(ServerEvent::ClientDisconnectedWithReason(
                                        client_id,
                                        reason,
                                    ))
                                    .await
                                    .is_err()
                                {
                                    return;
                                }
                            }
                        }
                    }
                }
            }
            cmd = commands_rx.recv() => {
                if let Some(cmd) = cmd {
                    match cmd {
                        ServerCommand::Send(client_id, frame) => {
                            if let Some(tx) = clients.get(&client_id) {
                                let _ = tx.send(ClientCommand::Outbound(frame)).await;
                            }
                        }
                        ServerCommand::Broadcast(frame) => {
                            for tx in clients.values() {
                                let _ = tx
                                    .send(ClientCommand::Outbound(frame.clone()))
                                    .await;
                            }
                        }
                        ServerCommand::BroadcastExcept(frame, except) => {
                            for (&id, tx) in &clients {
                                if id != except {
                                    let _ = tx.send(ClientCommand::Outbound(frame.clone())).await;
                                }
                            }
                        }
                        ServerCommand::Disconnect(client_id) => {
                            if let Some(tx) = clients.get(&client_id) {
                                let _ = tx.send(ClientCommand::Disconnect).await;
                            }
                        }
                    }
                } else {
                    return;
                }
            }
        }
    }
}

async fn client_session_task(
    client_id: ClientId,
    mut stream: tokio::net::TcpStream,
    codec: FrameCodec,
    config: ServerConfig,
    mut outbound_rx: mpsc::Receiver<ClientCommand>,
    events_tx: mpsc::Sender<(ClientId, ClientTaskEvent)>,
) {
    let mut ping_interval = tokio::time::interval(config.heartbeat_interval);
    ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut missed_pongs = 0;
    let heartbeat_limit = heartbeat_miss_limit(config.heartbeat_timeout, config.heartbeat_interval);

    let ping_type = MessageType::new("sys.ping").unwrap();
    let pong_type = MessageType::new("sys.pong").unwrap();

    let disconnect_reason = loop {
        tokio::select! {
            frame_res = codec.read_frame(&mut stream) => {
                match frame_res {
                    Ok(frame) => {
                        if frame.message_type() == &ping_type {
                            let reply = WireFrame::new(frame.version(), pong_type.clone(), Vec::new());
                            if codec.write_frame(&mut stream, &reply).await.is_err() {
                                break Some(DisconnectReason::TransportError(
                                    "failed to send pong".to_owned(),
                                ));
                            }
                        } else if frame.message_type() == &pong_type {
                            missed_pongs = 0;
                        } else {
                            if events_tx.send((client_id, ClientTaskEvent::Message(frame))).await.is_err() {
                                return;
                            }
                        }
                    }
                    Err(_) => {
                        break Some(DisconnectReason::RemoteClosed);
                    }
                }
            }
            outbound = outbound_rx.recv() => {
                if let Some(command) = outbound {
                    match command {
                        ClientCommand::Outbound(frame) => {
                            if codec.write_frame(&mut stream, &frame).await.is_err() {
                                break Some(DisconnectReason::TransportError(
                                    "failed to send application frame".to_owned(),
                                ));
                            }
                        }
                        ClientCommand::Disconnect => {
                            break Some(DisconnectReason::AdminDisconnect);
                        }
                    }
                } else {
                    break Some(DisconnectReason::LocalShutdown);
                }
            }
            _ = ping_interval.tick() => {
                if heartbeat_limit != u64::MAX && missed_pongs >= heartbeat_limit {
                    break Some(DisconnectReason::HeartbeatTimeout);
                }
                missed_pongs += 1;
                let ping_frame = WireFrame::new(ProtocolVersion::new(1), ping_type.clone(), Vec::new());
                if codec.write_frame(&mut stream, &ping_frame).await.is_err() {
                    break Some(DisconnectReason::TransportError(
                        "failed to send heartbeat".to_owned(),
                    ));
                }
            }
        }
    }.unwrap_or(DisconnectReason::RemoteClosed);

    let _ = events_tx
        .send((
            client_id,
            ClientTaskEvent::Disconnected(disconnect_reason),
        ))
        .await;
}

/// Message router dispatcher to register type-safe JSON callback handlers.
pub struct MessageRouter<Ctx> {
    handlers: HashMap<
        String,
        Box<
            dyn FnMut(&mut Ctx, ClientId, &[u8]) -> Result<(), serde_json::Error>
                + Send
                + Sync
                + 'static,
        >,
    >,
}

impl<Ctx> Default for MessageRouter<Ctx> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Ctx> MessageRouter<Ctx> {
    /// Creates a new empty message router.
    #[must_use]
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
        }
    }

    /// Registers a handler for a given message type name.
    pub fn register<T, F>(&mut self, message_type: impl Into<String>, mut handler: F)
    where
        T: DeserializeOwned + 'static,
        F: FnMut(&mut Ctx, ClientId, T) + Send + Sync + 'static,
    {
        let wrapped = move |ctx: &mut Ctx, client_id: ClientId, payload: &[u8]| {
            let value: T = serde_json::from_slice(payload)?;
            handler(ctx, client_id, value);
            Ok(())
        };
        self.handlers.insert(message_type.into(), Box::new(wrapped));
    }

    /// Handles an incoming wire frame by routing it to its registered callback.
    ///
    /// Returns `Ok(true)` if the message was routed successfully, `Ok(false)` if
    /// no handler matches, or `Err` if deserialization fails.
    ///
    /// # Errors
    ///
    /// Returns a `serde_json::Error` if payload parsing fails.
    pub fn handle(&mut self, context: &mut Ctx, client_id: ClientId, frame: &WireFrame) -> Result<bool, serde_json::Error> {
        if let Some(handler) = self.handlers.get_mut(frame.message_type().as_str()) {
            handler(context, client_id, frame.payload())?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}
