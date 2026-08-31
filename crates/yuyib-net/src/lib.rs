//! Bounded versioned wire frames and optional Tokio TCP transport for Yuyib.
//!
//! The low-level layer is [`FrameCodec`] plus [`WireFrame`]: a big-endian
//! length-prefixed body containing protocol version, UTF-8 message type and
//! opaque bytes. It has no `serde` requirement. [`JsonConnection`] adds an
//! ergonomic typed JSON layer with an explicit [`JsonMessageSpec`] and size
//! limit.
//!
//! Networking is deliberately opt-in. Callers provide and own the Tokio
//! runtime, choose every bind/connect [`SocketAddr`], and drive the async
//! operations themselves. This phase has no UDP, reliability protocol, ECS
//! replication, authentication, TLS, discovery, reconnect policy, or global
//! runtime.

#![forbid(unsafe_code)]

/// High-level client, server, configuration, and JSON routing layers.
pub mod high_level;
/// Bevy ECS resources and message polling systems.
#[cfg(feature = "ecs")]
pub mod ecs;

pub use high_level::{
    ClientConfig, ClientEvent, ClientId, ClientState, MessageRouter, NetClient, NetServer,
    ServerConfig, ServerEvent,
};

#[cfg(feature = "ecs")]
pub use ecs::{
    EcsClientEvent, EcsServerEvent, NetworkClient, NetworkServer, poll_client_events_system,
    poll_server_events_system,
};

use std::{
    error::Error,
    fmt,
    io::{self, Write},
    net::SocketAddr,
};

use serde::{Serialize, de::DeserializeOwned};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

const LENGTH_PREFIX_BYTES: usize = 4;
const FRAME_FIXED_BYTES: usize = 4;

/// One explicit protocol version carried by every wire frame.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ProtocolVersion(u16);

impl ProtocolVersion {
    /// Creates an explicit protocol version.
    #[must_use]
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    /// Returns the encoded numeric protocol version.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// A validated non-empty ASCII application message type name.
///
/// The grammar is `[A-Za-z0-9._-]+`. A deliberately narrow cross-language
/// grammar prevents Unicode canonicalization ambiguity, control characters,
/// and log/header injection through protocol metadata.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MessageType(String);

impl MessageType {
    /// Validates a non-empty message type against `[A-Za-z0-9._-]+`.
    ///
    /// The frame codec separately applies its configured byte limit before
    /// serializing or allocating a frame body.
    ///
    /// # Errors
    ///
    /// Returns [`MessageTypeError::Empty`] when `value` is empty.
    pub fn new(value: impl Into<String>) -> Result<Self, MessageTypeError> {
        let value = value.into();
        if value.is_empty() {
            return Err(MessageTypeError::Empty);
        }
        if let Some(character) = value.chars().find(|character| {
            !character.is_ascii_alphanumeric() && !matches!(character, '.' | '_' | '-')
        }) {
            return Err(MessageTypeError::InvalidCharacter { character });
        }
        Ok(Self(value))
    }

    /// Returns the UTF-8 type name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MessageType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Invalid application message type name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MessageTypeError {
    /// The wire format reserves an empty type name as invalid.
    Empty,
    /// The type name contains a character outside `[A-Za-z0-9._-]`.
    InvalidCharacter {
        /// First invalid Unicode scalar value observed in caller input.
        character: char,
    },
}

impl fmt::Display for MessageTypeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("message type name must not be empty"),
            Self::InvalidCharacter { character } => write!(
                formatter,
                "message type contains invalid character {character:?}; expected [A-Za-z0-9._-]"
            ),
        }
    }
}

impl Error for MessageTypeError {}

/// Opaque low-level frame data independent of serialization formats.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WireFrame {
    version: ProtocolVersion,
    message_type: MessageType,
    payload: Vec<u8>,
}

impl WireFrame {
    /// Creates one versioned opaque frame.
    #[must_use]
    pub fn new(version: ProtocolVersion, message_type: MessageType, payload: Vec<u8>) -> Self {
        Self {
            version,
            message_type,
            payload,
        }
    }

    /// Returns the explicit protocol version.
    #[must_use]
    pub const fn version(&self) -> ProtocolVersion {
        self.version
    }

    /// Returns the validated application message type.
    #[must_use]
    pub const fn message_type(&self) -> &MessageType {
        &self.message_type
    }

    /// Returns opaque application payload bytes.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// Deconstructs the frame without copying its payload.
    #[must_use]
    pub fn into_parts(self) -> (ProtocolVersion, MessageType, Vec<u8>) {
        (self.version, self.message_type, self.payload)
    }
}

/// Hard work limits applied before frame-body allocation or serialization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameLimits {
    /// Maximum body bytes after the four-byte length prefix.
    pub max_frame_bytes: usize,
    /// Maximum UTF-8 bytes in one message type name.
    pub max_type_name_bytes: usize,
}

impl Default for FrameLimits {
    fn default() -> Self {
        Self {
            max_frame_bytes: 1024 * 1024,
            max_type_name_bytes: 256,
        }
    }
}

impl FrameLimits {
    /// Validates limits required by the fixed wire header and `u16` type field.
    ///
    /// # Errors
    ///
    /// Returns [`FrameLimitsError`] for unusable or unrepresentable bounds.
    pub fn validate(self) -> Result<(), FrameLimitsError> {
        if self.max_type_name_bytes == 0 || self.max_type_name_bytes > usize::from(u16::MAX) {
            return Err(FrameLimitsError::InvalidTypeNameLimit {
                limit: self.max_type_name_bytes,
            });
        }
        if self.max_frame_bytes > usize::try_from(u32::MAX).unwrap_or(usize::MAX) {
            return Err(FrameLimitsError::FrameLimitUnrepresentable {
                limit: self.max_frame_bytes,
            });
        }
        let minimum = FRAME_FIXED_BYTES.saturating_add(1);
        if self.max_frame_bytes < minimum
            || self.max_type_name_bytes.saturating_add(FRAME_FIXED_BYTES) > self.max_frame_bytes
        {
            return Err(FrameLimitsError::InvalidFrameLimit {
                frame_limit: self.max_frame_bytes,
                type_name_limit: self.max_type_name_bytes,
            });
        }
        Ok(())
    }
}

/// Invalid [`FrameLimits`] configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameLimitsError {
    /// Type-name bound is zero or cannot fit the protocol's `u16` field.
    InvalidTypeNameLimit {
        /// Requested byte limit.
        limit: usize,
    },
    /// Frame body bound cannot fit the protocol's `u32` prefix.
    FrameLimitUnrepresentable {
        /// Requested byte limit.
        limit: usize,
    },
    /// Frame body cannot hold the fixed fields and an allowed type name.
    InvalidFrameLimit {
        /// Requested body byte limit.
        frame_limit: usize,
        /// Requested type-name byte limit.
        type_name_limit: usize,
    },
}

impl fmt::Display for FrameLimitsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTypeNameLimit { limit } => {
                write!(formatter, "invalid message type byte limit {limit}")
            }
            Self::FrameLimitUnrepresentable { limit } => {
                write!(
                    formatter,
                    "frame byte limit {limit} exceeds u32 wire prefix"
                )
            }
            Self::InvalidFrameLimit {
                frame_limit,
                type_name_limit,
            } => write!(
                formatter,
                "frame byte limit {frame_limit} cannot hold fixed fields and type limit {type_name_limit}"
            ),
        }
    }
}

impl Error for FrameLimitsError {}

/// Failure while encoding a [`WireFrame`] into its complete wire representation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameEncodeError {
    /// Type bytes exceed [`FrameLimits::max_type_name_bytes`].
    TypeNameTooLarge {
        /// Observed type-name byte count.
        actual: usize,
        /// Configured maximum.
        limit: usize,
    },
    /// Fixed fields, type bytes, and payload exceed the frame body limit.
    FrameTooLarge {
        /// Required body byte count.
        actual: usize,
        /// Configured maximum.
        limit: usize,
    },
}

impl fmt::Display for FrameEncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TypeNameTooLarge { actual, limit } => {
                write!(
                    formatter,
                    "message type has {actual} bytes, limit is {limit}"
                )
            }
            Self::FrameTooLarge { actual, limit } => {
                write!(formatter, "frame body has {actual} bytes, limit is {limit}")
            }
        }
    }
}

impl Error for FrameEncodeError {}

/// Failure while decoding raw frame bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FrameDecodeError {
    /// Input omitted the four-byte length prefix.
    MissingLengthPrefix {
        /// Supplied byte count.
        actual: usize,
    },
    /// Declared frame body exceeds the configured bound.
    FrameTooLarge {
        /// Declared body byte count.
        actual: usize,
        /// Configured maximum.
        limit: usize,
    },
    /// Prefix does not describe exactly the supplied frame body bytes.
    LengthMismatch {
        /// Declared body byte count.
        declared: usize,
        /// Actual bytes after the prefix.
        actual: usize,
    },
    /// Body does not contain the two-byte protocol version.
    MissingVersion {
        /// Supplied body byte count.
        actual: usize,
    },
    /// Body does not contain the two-byte type-name length field.
    MissingTypeNameLength {
        /// Supplied body byte count.
        actual: usize,
    },
    /// Declared type-name bytes exceed the configured bound.
    TypeNameTooLarge {
        /// Declared type-name byte count.
        actual: usize,
        /// Configured maximum.
        limit: usize,
    },
    /// Body ended inside its declared type-name bytes.
    TruncatedTypeName {
        /// Declared type-name byte count.
        declared: usize,
        /// Available type-name byte count.
        actual: usize,
    },
    /// Type-name bytes were not valid UTF-8.
    InvalidTypeNameUtf8,
    /// UTF-8 type name violated the stable ASCII type-name grammar.
    InvalidTypeNameCharacter,
    /// Type name is empty.
    EmptyTypeName,
}

impl fmt::Display for FrameDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingLengthPrefix { actual } => {
                write!(
                    formatter,
                    "frame has {actual} bytes but needs a four-byte length prefix"
                )
            }
            Self::FrameTooLarge { actual, limit } => {
                write!(
                    formatter,
                    "declared frame body is {actual} bytes, limit is {limit}"
                )
            }
            Self::LengthMismatch { declared, actual } => write!(
                formatter,
                "frame declares {declared} body bytes but contains {actual}"
            ),
            Self::MissingVersion { actual } => {
                write!(
                    formatter,
                    "frame body has {actual} bytes but needs a protocol version"
                )
            }
            Self::MissingTypeNameLength { actual } => {
                write!(
                    formatter,
                    "frame body has {actual} bytes but needs a type length"
                )
            }
            Self::TypeNameTooLarge { actual, limit } => {
                write!(
                    formatter,
                    "declared type name has {actual} bytes, limit is {limit}"
                )
            }
            Self::TruncatedTypeName { declared, actual } => write!(
                formatter,
                "frame declares {declared} type-name bytes but contains {actual}"
            ),
            Self::InvalidTypeNameUtf8 => formatter.write_str("frame type name is not valid UTF-8"),
            Self::InvalidTypeNameCharacter => {
                formatter.write_str("frame type name must match [A-Za-z0-9._-]+")
            }
            Self::EmptyTypeName => formatter.write_str("frame type name is empty"),
        }
    }
}

impl Error for FrameDecodeError {}

/// Stateless bounded codec for complete wire frames and Tokio streams.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameCodec {
    limits: FrameLimits,
}

impl FrameCodec {
    /// Creates a codec after validating its mandatory bounds.
    ///
    /// # Errors
    ///
    /// Returns [`FrameLimitsError`] for unusable limits.
    pub fn new(limits: FrameLimits) -> Result<Self, FrameLimitsError> {
        limits.validate()?;
        Ok(Self { limits })
    }

    /// Returns the codec's hard body/type bounds.
    #[must_use]
    pub const fn limits(self) -> FrameLimits {
        self.limits
    }

    /// Encodes one complete wire frame, including its four-byte length prefix.
    ///
    /// The body is `version:u16`, `type_bytes:u16`, UTF-8 type bytes, then
    /// opaque payload. Bounds are checked before allocating the output vector.
    ///
    /// # Errors
    ///
    /// Returns [`FrameEncodeError`] when configured bounds would be exceeded.
    pub fn encode(&self, frame: &WireFrame) -> Result<Vec<u8>, FrameEncodeError> {
        let type_bytes = frame.message_type.as_str().as_bytes();
        if type_bytes.len() > self.limits.max_type_name_bytes {
            return Err(FrameEncodeError::TypeNameTooLarge {
                actual: type_bytes.len(),
                limit: self.limits.max_type_name_bytes,
            });
        }
        let body_len = FRAME_FIXED_BYTES
            .checked_add(type_bytes.len())
            .and_then(|value| value.checked_add(frame.payload.len()))
            .ok_or(FrameEncodeError::FrameTooLarge {
                actual: usize::MAX,
                limit: self.limits.max_frame_bytes,
            })?;
        if body_len > self.limits.max_frame_bytes {
            return Err(FrameEncodeError::FrameTooLarge {
                actual: body_len,
                limit: self.limits.max_frame_bytes,
            });
        }
        let body_len_u32 =
            u32::try_from(body_len).map_err(|_| FrameEncodeError::FrameTooLarge {
                actual: body_len,
                limit: self.limits.max_frame_bytes,
            })?;
        let type_len_u16 =
            u16::try_from(type_bytes.len()).map_err(|_| FrameEncodeError::TypeNameTooLarge {
                actual: type_bytes.len(),
                limit: self.limits.max_type_name_bytes,
            })?;
        let total_len =
            LENGTH_PREFIX_BYTES
                .checked_add(body_len)
                .ok_or(FrameEncodeError::FrameTooLarge {
                    actual: usize::MAX,
                    limit: self.limits.max_frame_bytes,
                })?;
        let mut encoded = Vec::with_capacity(total_len);
        encoded.extend_from_slice(&body_len_u32.to_be_bytes());
        encoded.extend_from_slice(&frame.version.get().to_be_bytes());
        encoded.extend_from_slice(&type_len_u16.to_be_bytes());
        encoded.extend_from_slice(type_bytes);
        encoded.extend_from_slice(&frame.payload);
        Ok(encoded)
    }

    /// Decodes exactly one complete wire frame including its length prefix.
    ///
    /// # Errors
    ///
    /// Returns [`FrameDecodeError`] for malformed, truncated, oversized, or
    /// trailing input. It never allocates from a declared network length.
    pub fn decode(&self, bytes: &[u8]) -> Result<WireFrame, FrameDecodeError> {
        if bytes.len() < LENGTH_PREFIX_BYTES {
            return Err(FrameDecodeError::MissingLengthPrefix {
                actual: bytes.len(),
            });
        }
        let declared =
            usize::try_from(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
                .unwrap_or(usize::MAX);
        if declared > self.limits.max_frame_bytes {
            return Err(FrameDecodeError::FrameTooLarge {
                actual: declared,
                limit: self.limits.max_frame_bytes,
            });
        }
        let actual = bytes.len().saturating_sub(LENGTH_PREFIX_BYTES);
        if declared != actual {
            return Err(FrameDecodeError::LengthMismatch { declared, actual });
        }
        self.decode_body(&bytes[LENGTH_PREFIX_BYTES..])
    }

    /// Reads exactly one framed message from a Tokio async stream.
    ///
    /// A clean EOF before the next prefix is [`FrameReadError::EndOfStream`];
    /// EOF after any prefix/body byte is [`FrameReadError::Truncated`]. The
    /// declared length is checked before allocating the body vector.
    ///
    /// # Errors
    ///
    /// Returns structured EOF/truncation, I/O, oversized-length, or wire
    /// decoding errors.
    pub async fn read_frame<R>(&self, reader: &mut R) -> Result<WireFrame, FrameReadError>
    where
        R: AsyncRead + Unpin,
    {
        let mut prefix = [0_u8; LENGTH_PREFIX_BYTES];
        read_exact_stage(reader, &mut prefix, ReadStage::LengthPrefix, true).await?;
        let declared = usize::try_from(u32::from_be_bytes(prefix)).unwrap_or(usize::MAX);
        if declared > self.limits.max_frame_bytes {
            return Err(FrameReadError::FrameTooLarge {
                actual: declared,
                limit: self.limits.max_frame_bytes,
            });
        }
        let mut body = vec![0_u8; declared];
        read_exact_stage(reader, &mut body, ReadStage::Body, false).await?;
        self.decode_body(&body).map_err(FrameReadError::Decode)
    }

    /// Writes one encoded frame and waits for Tokio stream backpressure.
    ///
    /// This API owns no queue: `write_all` is awaited before returning, so a
    /// slow connection naturally applies per-connection backpressure.
    ///
    /// # Errors
    ///
    /// Returns an encode error before I/O or a structured write failure.
    pub async fn write_frame<W>(
        &self,
        writer: &mut W,
        frame: &WireFrame,
    ) -> Result<(), FrameWriteError>
    where
        W: AsyncWrite + Unpin,
    {
        let encoded = self.encode(frame).map_err(FrameWriteError::Encode)?;
        writer
            .write_all(&encoded)
            .await
            .map_err(FrameWriteError::Io)
    }

    fn decode_body(&self, body: &[u8]) -> Result<WireFrame, FrameDecodeError> {
        if body.len() < 2 {
            return Err(FrameDecodeError::MissingVersion { actual: body.len() });
        }
        if body.len() < FRAME_FIXED_BYTES {
            return Err(FrameDecodeError::MissingTypeNameLength { actual: body.len() });
        }
        let version = ProtocolVersion::new(u16::from_be_bytes([body[0], body[1]]));
        let type_len = usize::from(u16::from_be_bytes([body[2], body[3]]));
        if type_len > self.limits.max_type_name_bytes {
            return Err(FrameDecodeError::TypeNameTooLarge {
                actual: type_len,
                limit: self.limits.max_type_name_bytes,
            });
        }
        let available = body.len().saturating_sub(FRAME_FIXED_BYTES);
        if type_len > available {
            return Err(FrameDecodeError::TruncatedTypeName {
                declared: type_len,
                actual: available,
            });
        }
        let type_bytes = &body[FRAME_FIXED_BYTES..FRAME_FIXED_BYTES + type_len];
        let type_name =
            std::str::from_utf8(type_bytes).map_err(|_| FrameDecodeError::InvalidTypeNameUtf8)?;
        let message_type = MessageType::new(type_name).map_err(|error| match error {
            MessageTypeError::Empty => FrameDecodeError::EmptyTypeName,
            MessageTypeError::InvalidCharacter { .. } => FrameDecodeError::InvalidTypeNameCharacter,
        })?;
        let payload = body[FRAME_FIXED_BYTES + type_len..].to_vec();
        Ok(WireFrame::new(version, message_type, payload))
    }
}

/// TCP read stage associated with a structured [`FrameReadError`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadStage {
    /// The four-byte body length prefix.
    LengthPrefix,
    /// The already-validated declared body.
    Body,
}

impl fmt::Display for ReadStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LengthPrefix => formatter.write_str("length prefix"),
            Self::Body => formatter.write_str("frame body"),
        }
    }
}

/// Failure while reading one frame from an async stream.
#[derive(Debug)]
pub enum FrameReadError {
    /// EOF occurred before any byte of the next length prefix.
    EndOfStream,
    /// EOF occurred after a partial prefix/body.
    Truncated {
        /// Incomplete wire stage.
        stage: ReadStage,
        /// Bytes required for this stage.
        expected: usize,
        /// Bytes actually received before EOF.
        received: usize,
    },
    /// Declared body length exceeds the configured limit before allocation.
    FrameTooLarge {
        /// Declared body byte count.
        actual: usize,
        /// Configured maximum.
        limit: usize,
    },
    /// Stream operation failed for a reason other than EOF.
    Io {
        /// Wire stage being read.
        stage: ReadStage,
        /// Tokio/operating-system error.
        source: io::Error,
    },
    /// A complete body failed wire validation.
    Decode(FrameDecodeError),
}

impl fmt::Display for FrameReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EndOfStream => formatter.write_str("stream ended before the next frame"),
            Self::Truncated {
                stage,
                expected,
                received,
            } => write!(
                formatter,
                "stream ended during {stage}: received {received} of {expected} bytes"
            ),
            Self::FrameTooLarge { actual, limit } => {
                write!(
                    formatter,
                    "declared frame body is {actual} bytes, limit is {limit}"
                )
            }
            Self::Io { stage, source } => write!(formatter, "failed to read {stage}: {source}"),
            Self::Decode(source) => write!(formatter, "invalid complete frame: {source}"),
        }
    }
}

impl Error for FrameReadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Decode(source) => Some(source),
            _ => None,
        }
    }
}

/// Failure while writing one frame to an async stream.
#[derive(Debug)]
pub enum FrameWriteError {
    /// Frame violated configured bounds before any I/O.
    Encode(FrameEncodeError),
    /// Tokio/operating-system write failure.
    Io(io::Error),
}

impl fmt::Display for FrameWriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Encode(source) => write!(formatter, "cannot encode frame: {source}"),
            Self::Io(source) => write!(formatter, "failed to write frame: {source}"),
        }
    }
}

impl Error for FrameWriteError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Encode(source) => Some(source),
            Self::Io(source) => Some(source),
        }
    }
}

async fn read_exact_stage<R>(
    reader: &mut R,
    destination: &mut [u8],
    stage: ReadStage,
    clean_eof_allowed: bool,
) -> Result<(), FrameReadError>
where
    R: AsyncRead + Unpin,
{
    let mut received = 0_usize;
    while received < destination.len() {
        let count = reader
            .read(&mut destination[received..])
            .await
            .map_err(|source| FrameReadError::Io { stage, source })?;
        if count == 0 {
            if received == 0 && clean_eof_allowed {
                return Err(FrameReadError::EndOfStream);
            }
            return Err(FrameReadError::Truncated {
                stage,
                expected: destination.len(),
                received,
            });
        }
        received = received.saturating_add(count);
    }
    Ok(())
}

/// One connected TCP stream carrying a caller-selected [`FrameCodec`].
pub struct TcpConnection {
    stream: TcpStream,
    codec: FrameCodec,
}

impl TcpConnection {
    /// Returns the per-connection bounded codec.
    #[must_use]
    pub const fn codec(&self) -> FrameCodec {
        self.codec
    }

    /// Returns the remote TCP endpoint.
    ///
    /// # Errors
    ///
    /// Returns an operating-system socket query error.
    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.stream.peer_addr()
    }

    /// Reads one bounded wire frame from this connection.
    ///
    /// # Errors
    ///
    /// Returns structured EOF/truncation, I/O, oversized-length, or wire
    /// decoding errors from the underlying stream.
    pub async fn read_frame(&mut self) -> Result<WireFrame, FrameReadError> {
        self.codec.read_frame(&mut self.stream).await
    }

    /// Writes one bounded frame, awaiting this connection's backpressure.
    ///
    /// # Errors
    ///
    /// Returns a bounded frame encoding or TCP write error.
    pub async fn write_frame(&mut self, frame: &WireFrame) -> Result<(), FrameWriteError> {
        self.codec.write_frame(&mut self.stream, frame).await
    }

    /// Transfers ownership of the underlying Tokio TCP stream.
    #[must_use]
    pub fn into_inner(self) -> TcpStream {
        self.stream
    }
}

/// Listener with explicit bind address and per-accepted-connection codec.
pub struct TcpServer {
    listener: TcpListener,
    codec: FrameCodec,
}

impl TcpServer {
    /// Binds an explicit caller-selected TCP address.
    ///
    /// No default bind address (including `0.0.0.0`). The caller owns exposure
    /// policy and Tokio runtime lifecycle.
    ///
    /// # Errors
    ///
    /// Returns [`BindError`] when the operating system rejects the address.
    pub async fn bind(address: SocketAddr, codec: FrameCodec) -> Result<Self, BindError> {
        let listener = TcpListener::bind(address)
            .await
            .map_err(|source| BindError { address, source })?;
        Ok(Self { listener, codec })
    }

    /// Returns the actual local address, useful after binding port zero.
    ///
    /// # Errors
    ///
    /// Returns an operating-system socket query error.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Accepts one connection without spawning a task or creating a queue.
    ///
    /// # Errors
    ///
    /// Returns [`AcceptError`] when Tokio or the operating system rejects the
    /// next accept operation.
    pub async fn accept(&self) -> Result<AcceptedConnection, AcceptError> {
        let (stream, peer_addr) = self.listener.accept().await.map_err(AcceptError::Io)?;
        let _ = stream.set_nodelay(true);
        Ok(AcceptedConnection {
            connection: TcpConnection {
                stream,
                codec: self.codec,
            },
            peer_addr,
        })
    }
}

/// Result of one explicit [`TcpServer::accept`] call.
pub struct AcceptedConnection {
    /// Newly accepted bounded TCP connection.
    pub connection: TcpConnection,
    /// Remote endpoint observed by the listener.
    pub peer_addr: SocketAddr,
}

/// Failure while explicitly binding a TCP listener.
#[derive(Debug)]
pub struct BindError {
    /// Requested bind endpoint.
    pub address: SocketAddr,
    /// Tokio/operating-system error.
    pub source: io::Error,
}

impl fmt::Display for BindError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "failed to bind TCP listener at {}: {}",
            self.address, self.source
        )
    }
}

impl Error for BindError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}

/// Failure while accepting a TCP connection.
#[derive(Debug)]
pub enum AcceptError {
    /// Tokio/operating-system accept failure.
    Io(io::Error),
}

impl fmt::Display for AcceptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(source) => write!(formatter, "failed to accept TCP connection: {source}"),
        }
    }
}

impl Error for AcceptError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(source) => Some(source),
        }
    }
}

/// Connects to one explicit caller-selected TCP address.
///
/// No runtime is created and no reconnect task is spawned. Invoke this future
/// inside the application's Tokio runtime and retain the returned connection.
///
/// # Errors
///
/// Returns [`ConnectError`] when Tokio or the operating system cannot connect.
pub async fn connect(
    address: SocketAddr,
    codec: FrameCodec,
) -> Result<TcpConnection, ConnectError> {
    let stream = TcpStream::connect(address)
        .await
        .map_err(|source| ConnectError { address, source })?;
    let _ = stream.set_nodelay(true);
    Ok(TcpConnection { stream, codec })
}

/// Failure while connecting to a caller-selected endpoint.
#[derive(Debug)]
pub struct ConnectError {
    /// Requested remote endpoint.
    pub address: SocketAddr,
    /// Tokio/operating-system error.
    pub source: io::Error,
}

impl fmt::Display for ConnectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "failed to connect to TCP {}: {}",
            self.address, self.source
        )
    }
}

impl Error for ConnectError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}

/// One explicit version/type contract for JSON messages.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JsonMessageSpec {
    version: ProtocolVersion,
    message_type: MessageType,
}

impl JsonMessageSpec {
    /// Creates an explicit JSON protocol version and type-name contract.
    #[must_use]
    pub const fn new(version: ProtocolVersion, message_type: MessageType) -> Self {
        Self {
            version,
            message_type,
        }
    }

    /// Returns the expected protocol version.
    #[must_use]
    pub const fn version(&self) -> ProtocolVersion {
        self.version
    }

    /// Returns the expected application type name.
    #[must_use]
    pub const fn message_type(&self) -> &MessageType {
        &self.message_type
    }
}

/// Bounded JSON payload policy layered on top of [`FrameLimits`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JsonLimits {
    /// Maximum serialized JSON bytes per typed message.
    pub max_json_bytes: usize,
}

impl Default for JsonLimits {
    fn default() -> Self {
        Self {
            max_json_bytes: 512 * 1024,
        }
    }
}

impl JsonLimits {
    /// Validates a required non-zero JSON byte bound.
    ///
    /// # Errors
    ///
    /// Returns [`JsonLimitsError::ZeroPayloadLimit`] for zero.
    pub const fn validate(self) -> Result<(), JsonLimitsError> {
        if self.max_json_bytes == 0 {
            return Err(JsonLimitsError::ZeroPayloadLimit);
        }
        Ok(())
    }
}

/// Invalid [`JsonLimits`] configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JsonLimitsError {
    /// A zero limit would prohibit every JSON value.
    ZeroPayloadLimit,
}

impl fmt::Display for JsonLimitsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroPayloadLimit => {
                formatter.write_str("JSON payload byte limit must be positive")
            }
        }
    }
}

impl Error for JsonLimitsError {}

/// Typed JSON view over one bounded [`TcpConnection`].
///
/// It owns no task, channel, or runtime. Every send awaits TCP backpressure;
/// every receive waits directly on the caller-provided Tokio runtime.
pub struct JsonConnection {
    connection: TcpConnection,
    limits: JsonLimits,
}

impl JsonConnection {
    /// Wraps one connection with a separately validated JSON payload bound.
    ///
    /// # Errors
    ///
    /// Returns [`JsonLimitsError`] when the JSON byte limit is zero.
    pub fn new(connection: TcpConnection, limits: JsonLimits) -> Result<Self, JsonLimitsError> {
        limits.validate()?;
        Ok(Self { connection, limits })
    }

    /// Returns the JSON payload limit.
    #[must_use]
    pub const fn limits(&self) -> JsonLimits {
        self.limits
    }

    /// Returns the underlying low-level connection.
    #[must_use]
    pub const fn connection(&self) -> &TcpConnection {
        &self.connection
    }

    /// Sends a typed JSON message with an explicit version/type contract.
    ///
    /// JSON serialization writes into a bounded sink instead of creating an
    /// unbounded intermediate payload. TCP `write_all` is awaited, so this
    /// method provides per-connection backpressure rather than an unbounded
    /// producer queue.
    ///
    /// # Errors
    ///
    /// Returns a bounded JSON serialization, frame encoding, or TCP write
    /// error.
    pub async fn send<T>(&mut self, spec: &JsonMessageSpec, value: &T) -> Result<(), JsonSendError>
    where
        T: Serialize,
    {
        let payload = encode_json_bounded(value, self.limits.max_json_bytes)?;
        let frame = WireFrame::new(spec.version, spec.message_type.clone(), payload);
        self.connection
            .write_frame(&frame)
            .await
            .map_err(JsonSendError::Write)
    }

    /// Receives and deserializes one JSON message matching an explicit contract.
    ///
    /// Version and type-name mismatches are returned before JSON parsing. A
    /// payload exceeding [`JsonLimits::max_json_bytes`] is rejected before the
    /// deserializer reads it.
    ///
    /// # Errors
    ///
    /// Returns a structured frame read, version/type mismatch, JSON size, or
    /// deserialization error.
    pub async fn receive<T>(&mut self, spec: &JsonMessageSpec) -> Result<T, JsonReceiveError>
    where
        T: DeserializeOwned,
    {
        let frame = self
            .connection
            .read_frame()
            .await
            .map_err(JsonReceiveError::Read)?;
        if frame.version != spec.version {
            return Err(JsonReceiveError::VersionMismatch {
                expected: spec.version,
                received: frame.version,
            });
        }
        if frame.message_type != spec.message_type {
            return Err(JsonReceiveError::TypeMismatch {
                expected: spec.message_type.clone(),
                received: frame.message_type,
            });
        }
        if frame.payload.len() > self.limits.max_json_bytes {
            return Err(JsonReceiveError::PayloadTooLarge {
                actual: frame.payload.len(),
                limit: self.limits.max_json_bytes,
            });
        }
        serde_json::from_slice(&frame.payload).map_err(JsonReceiveError::Deserialize)
    }

    /// Releases the typed wrapper and returns the low-level connection.
    #[must_use]
    pub fn into_inner(self) -> TcpConnection {
        self.connection
    }
}

/// Failure while serializing/sending one typed JSON message.
#[derive(Debug)]
pub enum JsonSendError {
    /// Serialization attempted to exceed the configured JSON payload bound.
    PayloadTooLarge {
        /// Lower bound for serialized bytes; exact count is intentionally not accumulated.
        actual_at_least: usize,
        /// Configured maximum.
        limit: usize,
    },
    /// Serde JSON serialization failed for another reason.
    Serialize(serde_json::Error),
    /// Low-level frame encoding or TCP write failed.
    Write(FrameWriteError),
}

impl fmt::Display for JsonSendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PayloadTooLarge {
                actual_at_least,
                limit,
            } => write!(
                formatter,
                "JSON payload is at least {actual_at_least} bytes, limit is {limit}"
            ),
            Self::Serialize(source) => write!(formatter, "failed to serialize JSON: {source}"),
            Self::Write(source) => write!(formatter, "failed to send JSON frame: {source}"),
        }
    }
}

impl Error for JsonSendError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Serialize(source) => Some(source),
            Self::Write(source) => Some(source),
            Self::PayloadTooLarge { .. } => None,
        }
    }
}

/// Failure while receiving/deserializing one typed JSON message.
#[derive(Debug)]
pub enum JsonReceiveError {
    /// Low-level TCP/frame read failure.
    Read(FrameReadError),
    /// Frame protocol version differs from the explicit expected version.
    VersionMismatch {
        /// Expected protocol version.
        expected: ProtocolVersion,
        /// Received protocol version.
        received: ProtocolVersion,
    },
    /// Frame type name differs from the explicit expected type.
    TypeMismatch {
        /// Expected type name.
        expected: MessageType,
        /// Received type name.
        received: MessageType,
    },
    /// Frame payload exceeds the JSON payload bound before parsing.
    PayloadTooLarge {
        /// Observed payload byte count.
        actual: usize,
        /// Configured maximum.
        limit: usize,
    },
    /// JSON syntax or target-type conversion failed.
    Deserialize(serde_json::Error),
}

impl fmt::Display for JsonReceiveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(source) => write!(formatter, "failed to receive JSON frame: {source}"),
            Self::VersionMismatch { expected, received } => write!(
                formatter,
                "JSON protocol version mismatch: expected {}, received {}",
                expected.get(),
                received.get()
            ),
            Self::TypeMismatch { expected, received } => write!(
                formatter,
                "JSON message type mismatch: expected {expected}, received {received}"
            ),
            Self::PayloadTooLarge { actual, limit } => {
                write!(
                    formatter,
                    "JSON payload has {actual} bytes, limit is {limit}"
                )
            }
            Self::Deserialize(source) => write!(formatter, "failed to deserialize JSON: {source}"),
        }
    }
}

impl Error for JsonReceiveError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Read(source) => Some(source),
            Self::Deserialize(source) => Some(source),
            Self::VersionMismatch { .. }
            | Self::TypeMismatch { .. }
            | Self::PayloadTooLarge { .. } => None,
        }
    }
}

struct BoundedJsonWriter {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}

impl BoundedJsonWriter {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
            exceeded: false,
        }
    }

    fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

impl Write for BoundedJsonWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let remaining = self.limit.saturating_sub(self.bytes.len());
        if bytes.len() > remaining {
            self.exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "bounded JSON payload limit exceeded",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn encode_json_bounded<T>(value: &T, limit: usize) -> Result<Vec<u8>, JsonSendError>
where
    T: Serialize,
{
    let mut writer = BoundedJsonWriter::new(limit);
    let result = {
        let mut serializer = serde_json::Serializer::new(&mut writer);
        value.serialize(&mut serializer)
    };
    if writer.exceeded {
        return Err(JsonSendError::PayloadTooLarge {
            actual_at_least: limit.saturating_add(1),
            limit,
        });
    }
    result.map_err(JsonSendError::Serialize)?;
    Ok(writer.into_bytes())
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use serde::{Deserialize, Serialize};
    use tokio::io::AsyncWriteExt;

    use super::*;

    fn codec() -> FrameCodec {
        FrameCodec::new(FrameLimits {
            max_frame_bytes: 1024,
            max_type_name_bytes: 32,
        })
        .expect("valid codec")
    }

    fn message_type() -> MessageType {
        MessageType::new("demo.ping").expect("type")
    }

    #[test]
    fn message_type_requires_stable_ascii_protocol_grammar() {
        assert!(matches!(
            MessageType::new("demo ping"),
            Err(MessageTypeError::InvalidCharacter { character: ' ' })
        ));
        assert!(matches!(
            MessageType::new("demo.пинг"),
            Err(MessageTypeError::InvalidCharacter { character: 'п' })
        ));
        assert!(MessageType::new("demo.v1_ping-2").is_ok());
    }

    #[test]
    fn codec_round_trips_version_type_and_opaque_payload() {
        let frame = WireFrame::new(ProtocolVersion::new(7), message_type(), vec![1, 2, 3]);
        let encoded = codec().encode(&frame).expect("encode");
        assert_eq!(&encoded[..4], &[0, 0, 0, 16]);
        assert_eq!(codec().decode(&encoded).expect("decode"), frame);
    }

    #[test]
    fn codec_rejects_declared_size_before_body_and_invalid_type_boundaries() {
        let oversized = [0, 0, 4, 1];
        assert!(matches!(
            codec().decode(&oversized),
            Err(FrameDecodeError::FrameTooLarge { actual: 1025, .. })
        ));
        let truncated_type = [0, 0, 0, 6, 0, 1, 0, 3, b'a', b'b'];
        assert!(matches!(
            codec().decode(&truncated_type),
            Err(FrameDecodeError::TruncatedTypeName {
                declared: 3,
                actual: 2
            })
        ));
        let invalid_type = [
            0, 0, 0, 12, 0, 1, 0, 8, b'b', b'a', b'd', b' ', b'n', b'a', b'm', b'e',
        ];
        assert!(matches!(
            codec().decode(&invalid_type),
            Err(FrameDecodeError::InvalidTypeNameCharacter)
        ));
    }

    #[tokio::test]
    async fn read_distinguishes_clean_eof_from_truncated_body() {
        let (mut writer, mut reader) = tokio::io::duplex(32);
        writer.write_all(&[0, 0, 0, 5, 0, 1]).await.expect("write");
        drop(writer);
        assert!(matches!(
            codec().read_frame(&mut reader).await,
            Err(FrameReadError::Truncated {
                stage: ReadStage::Body,
                expected: 5,
                received: 2
            })
        ));

        let (writer, mut reader) = tokio::io::duplex(32);
        drop(writer);
        assert!(matches!(
            codec().read_frame(&mut reader).await,
            Err(FrameReadError::EndOfStream)
        ));
    }

    #[tokio::test]
    async fn explicit_loopback_server_and_client_exchange_low_level_frame() {
        let server = TcpServer::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0), codec())
            .await
            .expect("bind");
        let address = server.local_addr().expect("address");
        let (client, accepted) = tokio::join!(connect(address, codec()), server.accept());
        let mut client = client.expect("connect");
        let mut server_connection = accepted.expect("accept").connection;
        let outbound = WireFrame::new(ProtocolVersion::new(1), message_type(), b"hello".to_vec());
        let (written, received) = tokio::join!(
            client.write_frame(&outbound),
            server_connection.read_frame()
        );
        written.expect("client write");
        assert_eq!(received.expect("server read"), outbound);
    }

    #[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
    struct Ping {
        id: u32,
        text: String,
    }

    #[tokio::test]
    async fn typed_json_wrapper_checks_explicit_version_and_type_on_loopback() {
        let server = TcpServer::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0), codec())
            .await
            .expect("bind");
        let address = server.local_addr().expect("address");
        let (client, accepted) = tokio::join!(connect(address, codec()), server.accept());
        let mut client = JsonConnection::new(client.expect("connect"), JsonLimits::default())
            .expect("json client");
        let mut accepted =
            JsonConnection::new(accepted.expect("accept").connection, JsonLimits::default())
                .expect("json server");
        let spec = JsonMessageSpec::new(ProtocolVersion::new(3), message_type());
        let ping = Ping {
            id: 42,
            text: String::from("hello"),
        };
        let (sent, received) =
            tokio::join!(client.send(&spec, &ping), accepted.receive::<Ping>(&spec));
        sent.expect("json send");
        assert_eq!(received.expect("json receive"), ping);
    }

    #[test]
    fn typed_json_serialization_stops_at_the_configured_bound() {
        let value = "abcdefgh";
        assert!(matches!(
            encode_json_bounded(&value, 4),
            Err(JsonSendError::PayloadTooLarge {
                actual_at_least: 5,
                limit: 4
            })
        ));
    }
}
