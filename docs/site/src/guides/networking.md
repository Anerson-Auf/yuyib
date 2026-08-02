# Networking Phase 1

> **Статус:** Experimental  
> **Crate:** `yuyib-net` (optional client/server foundation)

`yuyib-net` — explicit Tokio TCP layer для приложений, tools и будущей
client/server игры. Он не создаёт Tokio runtime, background task или global
queue: host выбирает runtime, `SocketAddr`, lifecycle connection и policy
reconnect самостоятельно. У `TcpServer::bind` нет default address — даже
`0.0.0.0` должен быть осознанно передан caller'ом.

## Два уровня API

Low-level `FrameCodec` работает без `serde`. `WireFrame` состоит из explicit
`ProtocolVersion`, non-empty `MessageType` и opaque `Vec<u8>`. `MessageType`
uses stable cross-language ASCII grammar `[A-Za-z0-9._-]+`; it intentionally
rejects spaces, controls and Unicode confusables. Wire layout
всегда big-endian:

```text
u32 body_length | u16 protocol_version | u16 type_length | UTF-8 type | payload
```

`FrameLimits` ограничивает body и type-name bytes. При receive префикс
проверяется **до** allocation body; clean EOF до нового prefix возвращает
`FrameReadError::EndOfStream`, а EOF посреди prefix/body —
`FrameReadError::Truncated`. `write_frame` awaits `write_all`; у connection
нет unbounded producer queue, поэтому slow peer naturally applies backpressure.

High-level `JsonConnection` делает typed JSON поверх той же connection.
Для каждого send/receive caller передаёт `JsonMessageSpec` с version и type
name. `JsonLimits::max_json_bytes` применяется при serialize в bounded writer
и перед deserialize; wrong version/type возвращаются до JSON parsing.

```rust,no_run
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use yuyib_net::{FrameCodec, FrameLimits, TcpServer};

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let codec = FrameCodec::new(FrameLimits::default())?;
let server = TcpServer::bind(
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 43000),
    codec,
)
.await?;
let accepted = server.accept().await?;
# let _ = accepted;
# Ok(())
# }
```

## Limits & Caveats

`FrameLimits` is the primary trust boundary. Set a smaller frame/type limit
for Internet-facing peers than for trusted local development. Payloads are
opaque in low-level API; a host must define message-specific validation after
decode. `JsonLimits` should not exceed the usable `FrameLimits` body after
version/type overhead; otherwise `FrameWriteError::Encode` still rejects it.

Connection ownership is intentionally mutable and sequential. Do not issue
concurrent reads or writes through the same wrapper without defining your own
ordering/synchronization policy. The crate performs no admission control, rate
limit, timeout, cancellation, authorization, encryption, observability or
reconnect policy.

UDP, custom reliability/ordering, ECS replication, snapshots/delta encoding,
authentication, TLS, discovery and matchmaking are **Planned** layers, not
implicit promises of this Phase 1 transport.

Full API: [yuyib-net](../api/yuyib_net/index.html).
