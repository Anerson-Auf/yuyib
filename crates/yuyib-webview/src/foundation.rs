//! Pure local-asset and message-schema foundations for a later WebView bridge.
//!
//! No type in this module reads from disk, starts a process, evaluates
//! JavaScript, or dispatches a browser message. A future backend must consume
//! these validated values and still enforce origin/capability checks.

#![allow(clippy::doc_markdown)]

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
    num::{NonZeroU64, NonZeroU128},
    sync::Arc,
};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::DeserializeOwned};
use serde_json::Value;
use url::Url;

use crate::ControlledUrl;

const MAX_LOGICAL_PATH_BYTES: usize = 512;
const MAX_ENDPOINT_NAME_BYTES: usize = 96;
const LOCAL_PROTOCOL_HOST: &str = "app.localhost";
const LOCAL_PROTOCOL_SOURCE_SCHEME: &str = "app";
const LOCAL_PROTOCOL_SOURCE_HOST: &str = "localhost";

/// A safe, logical path inside an in-memory WebView asset bundle.
///
/// It is never an operating-system path. Parsing rejects absolute paths,
/// backslashes, empty segments, dot segments, drive separators, controls, and
/// percent signs so a future custom-protocol adapter cannot accidentally turn
/// an asset identifier into a disk traversal.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AssetPath(String);

impl AssetPath {
    /// Validates a slash-separated logical asset path.
    ///
    /// # Errors
    ///
    /// Returns an asset-path error for an empty, oversized, absolute, or unsafe
    /// path.
    pub fn parse(value: impl AsRef<str>) -> Result<Self, AssetPathError> {
        let value = value.as_ref();
        if value.is_empty() {
            return Err(AssetPathError::Empty);
        }
        if value.len() > MAX_LOGICAL_PATH_BYTES {
            return Err(AssetPathError::TooLong {
                limit: MAX_LOGICAL_PATH_BYTES,
            });
        }
        if value.starts_with('/') || value.starts_with('\\') {
            return Err(AssetPathError::Absolute);
        }
        if value.contains('\\') || value.contains(':') || value.contains('%') {
            return Err(AssetPathError::ForbiddenCharacter);
        }
        for segment in value.split('/') {
            if segment.is_empty() {
                return Err(AssetPathError::EmptySegment);
            }
            if matches!(segment, "." | "..") {
                return Err(AssetPathError::DotSegment);
            }
            if segment.chars().any(char::is_control) {
                return Err(AssetPathError::ForbiddenCharacter);
            }
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the canonical logical path.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn extension(&self) -> Option<&str> {
        self.0.rsplit_once('.').map(|(_, extension)| extension)
    }
}

impl fmt::Display for AssetPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Reason a logical asset path was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssetPathError {
    /// The path was empty.
    Empty,
    /// The path exceeded the bounded logical-path length.
    TooLong {
        /// Maximum accepted UTF-8 byte length.
        limit: usize,
    },
    /// The path began with an operating-system root marker.
    Absolute,
    /// The path had an empty segment.
    EmptySegment,
    /// The path contained a dot or parent traversal segment.
    DotSegment,
    /// The path contained a backslash, colon, percent sign, or control code.
    ForbiddenCharacter,
}

impl fmt::Display for AssetPathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("asset path must not be empty"),
            Self::TooLong { limit } => write!(formatter, "asset path exceeds {limit} bytes"),
            Self::Absolute => formatter.write_str("asset path must be relative"),
            Self::EmptySegment => formatter.write_str("asset path contains an empty segment"),
            Self::DotSegment => formatter.write_str("asset path contains a dot segment"),
            Self::ForbiddenCharacter => {
                formatter.write_str("asset path contains a forbidden character")
            }
        }
    }
}

impl Error for AssetPathError {}

/// MIME classes allowed by the local asset policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssetMime {
    /// HTML served as UTF-8.
    Html,
    /// CSS served as UTF-8.
    Css,
    /// JavaScript served as UTF-8.
    JavaScript,
    /// JSON.
    Json,
    /// Plain UTF-8 text.
    Text,
    /// SVG image.
    Svg,
    /// PNG image.
    Png,
    /// JPEG image.
    Jpeg,
    /// GIF image.
    Gif,
    /// WebP image.
    Webp,
    /// ICO image.
    Ico,
    /// WOFF2 font.
    Woff2,
    /// WebAssembly module.
    Wasm,
}

impl AssetMime {
    /// Returns the response Content-Type value.
    #[must_use]
    pub const fn content_type(self) -> &'static str {
        match self {
            Self::Html => "text/html; charset=utf-8",
            Self::Css => "text/css; charset=utf-8",
            Self::JavaScript => "text/javascript; charset=utf-8",
            Self::Json => "application/json",
            Self::Text => "text/plain; charset=utf-8",
            Self::Svg => "image/svg+xml",
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
            Self::Ico => "image/x-icon",
            Self::Woff2 => "font/woff2",
            Self::Wasm => "application/wasm",
        }
    }
}

/// Strict mapping from known file extensions to MIME classes.
///
/// Unknown extensions are rejected instead of falling back to browser sniffing.
/// WebAssembly is disabled unless explicitly enabled.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MimePolicy {
    allow_wasm: bool,
}

impl MimePolicy {
    /// Starts with known web types and WebAssembly disabled.
    #[must_use]
    pub const fn strict() -> Self {
        Self { allow_wasm: false }
    }

    /// Enables the WebAssembly MIME type for this explicit asset bundle.
    #[must_use]
    pub const fn with_web_assembly(mut self, enabled: bool) -> Self {
        self.allow_wasm = enabled;
        self
    }

    /// Resolves the safe MIME type for one logical asset path.
    ///
    /// # Errors
    ///
    /// Returns an error for unknown, extensionless, or disabled asset types.
    pub fn resolve(&self, path: &AssetPath) -> Result<AssetMime, MimePolicyError> {
        let extension = path.extension().ok_or(MimePolicyError::MissingExtension)?;
        let mime = match extension.to_ascii_lowercase().as_str() {
            "html" | "htm" => AssetMime::Html,
            "css" => AssetMime::Css,
            "js" | "mjs" => AssetMime::JavaScript,
            "json" => AssetMime::Json,
            "txt" => AssetMime::Text,
            "svg" => AssetMime::Svg,
            "png" => AssetMime::Png,
            "jpg" | "jpeg" => AssetMime::Jpeg,
            "gif" => AssetMime::Gif,
            "webp" => AssetMime::Webp,
            "ico" => AssetMime::Ico,
            "woff2" => AssetMime::Woff2,
            "wasm" if self.allow_wasm => AssetMime::Wasm,
            "wasm" => return Err(MimePolicyError::WasmDisabled),
            _ => return Err(MimePolicyError::UnknownExtension),
        };
        Ok(mime)
    }
}

/// A MIME-policy rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MimePolicyError {
    /// The logical path did not have an extension.
    MissingExtension,
    /// The extension is not on the strict allow-list.
    UnknownExtension,
    /// A WebAssembly asset needs explicit policy opt-in.
    WasmDisabled,
}

impl fmt::Display for MimePolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingExtension => formatter.write_str("asset has no file extension"),
            Self::UnknownExtension => formatter.write_str("asset extension is not allowed"),
            Self::WasmDisabled => formatter.write_str("WebAssembly is disabled by MIME policy"),
        }
    }
}

impl Error for MimePolicyError {}

/// Upper limits for an in-memory local asset bundle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AssetLimits {
    /// Maximum assets held by one bundle.
    pub max_assets: usize,
    /// Maximum bytes in an individual asset.
    pub max_asset_bytes: usize,
    /// Maximum bytes across all assets.
    pub max_bundle_bytes: usize,
}

impl Default for AssetLimits {
    fn default() -> Self {
        Self {
            max_assets: 1_024,
            max_asset_bytes: 8 * 1024 * 1024,
            max_bundle_bytes: 64 * 1024 * 1024,
        }
    }
}

/// An immutable response asset held solely in memory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalAsset {
    mime: AssetMime,
    bytes: Arc<[u8]>,
}

impl LocalAsset {
    /// Returns the policy-approved MIME type.
    #[must_use]
    pub const fn mime(&self) -> AssetMime {
        self.mime
    }

    /// Returns the policy-approved response Content-Type.
    #[must_use]
    pub const fn content_type(&self) -> &'static str {
        self.mime.content_type()
    }

    /// Returns immutable response bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// An in-memory safe-path local asset bundle.
///
/// Callers supply bytes themselves; this type deliberately has no directory,
/// path, file, or symlink API.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetBundle {
    limits: AssetLimits,
    mime_policy: MimePolicy,
    total_bytes: usize,
    assets: BTreeMap<AssetPath, LocalAsset>,
}

impl AssetBundle {
    /// Creates an empty bundle with the selected MIME policy and limits.
    #[must_use]
    pub fn new(mime_policy: MimePolicy, limits: AssetLimits) -> Self {
        Self {
            limits,
            mime_policy,
            total_bytes: 0,
            assets: BTreeMap::new(),
        }
    }

    /// Adds one in-memory asset after MIME and size validation.
    ///
    /// # Errors
    ///
    /// Returns an error for duplicate paths, MIME-policy rejections, or
    /// configured resource limits.
    pub fn insert(
        &mut self,
        path: AssetPath,
        bytes: impl Into<Vec<u8>>,
    ) -> Result<(), AssetBundleError> {
        if self.assets.contains_key(&path) {
            return Err(AssetBundleError::DuplicatePath);
        }
        if self.assets.len() >= self.limits.max_assets {
            return Err(AssetBundleError::TooManyAssets {
                limit: self.limits.max_assets,
            });
        }
        let bytes = bytes.into();
        if bytes.len() > self.limits.max_asset_bytes {
            return Err(AssetBundleError::AssetTooLarge {
                limit: self.limits.max_asset_bytes,
            });
        }
        let total_bytes =
            self.total_bytes
                .checked_add(bytes.len())
                .ok_or(AssetBundleError::BundleTooLarge {
                    limit: self.limits.max_bundle_bytes,
                })?;
        if total_bytes > self.limits.max_bundle_bytes {
            return Err(AssetBundleError::BundleTooLarge {
                limit: self.limits.max_bundle_bytes,
            });
        }
        let mime = self
            .mime_policy
            .resolve(&path)
            .map_err(AssetBundleError::Mime)?;
        self.assets.insert(
            path,
            LocalAsset {
                mime,
                bytes: Arc::from(bytes),
            },
        );
        self.total_bytes = total_bytes;
        Ok(())
    }

    /// Resolves a validated logical path without touching the filesystem.
    #[must_use]
    pub fn get(&self, path: &AssetPath) -> Option<&LocalAsset> {
        self.assets.get(path)
    }

    /// Returns how many bytes the bundle owns.
    #[must_use]
    pub const fn total_bytes(&self) -> usize {
        self.total_bytes
    }
}

/// Asset-bundle insertion failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssetBundleError {
    /// The path has already been registered.
    DuplicatePath,
    /// Asset-count policy was exceeded.
    TooManyAssets {
        /// Configured maximum asset count.
        limit: usize,
    },
    /// One asset exceeded its byte limit.
    AssetTooLarge {
        /// Configured individual asset-byte limit.
        limit: usize,
    },
    /// Bundle bytes exceeded their aggregate limit.
    BundleTooLarge {
        /// Configured total byte limit.
        limit: usize,
    },
    /// The asset extension violated the MIME policy.
    Mime(MimePolicyError),
}

impl fmt::Display for AssetBundleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicatePath => formatter.write_str("asset path is already registered"),
            Self::TooManyAssets { limit } => write!(formatter, "asset limit {limit} exceeded"),
            Self::AssetTooLarge { limit } => write!(formatter, "asset exceeds {limit} bytes"),
            Self::BundleTooLarge { limit } => {
                write!(formatter, "asset bundle exceeds {limit} bytes")
            }
            Self::Mime(error) => write!(formatter, "asset MIME policy rejected asset: {error}"),
        }
    }
}

impl Error for AssetBundleError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Mime(error) => Some(error),
            _ => None,
        }
    }
}

/// A restrictive CSP policy for a future local custom-protocol response.
///
/// The default permits only same-origin resources, denies objects, frames, and
/// base URLs, and permits connections only to same-origin. Each remote
/// connection origin is an explicit HTTPS ControlledUrl opt-in.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LocalCsp {
    connect_origins: BTreeSet<String>,
    blob_workers: bool,
    inline_styles: bool,
}

impl LocalCsp {
    /// Returns the strict default CSP.
    #[must_use]
    pub fn strict() -> Self {
        Self::default()
    }

    /// Allows fetch/XHR connections to the HTTPS origin of one controlled URL.
    ///
    /// This changes only the CSP connection allow-list. It does not enable
    /// browser navigation, bridge privileges, filesystem access, or process
    /// access.
    #[must_use]
    pub fn with_connect_origin(mut self, url: &ControlledUrl) -> Self {
        self.connect_origins.insert(url.origin());
        self
    }

    /// Allows same-origin `blob:` workers (Monaco and similar editor bundles).
    ///
    /// Without this, a page meta CSP that permits `blob:` is still intersected
    /// with the response header's `default-src 'self'` and workers fail.
    #[must_use]
    pub fn with_blob_workers(mut self) -> Self {
        self.blob_workers = true;
        self
    }

    /// Permits `'unsafe-inline'` styles for CSS-in-JS hosts such as Monaco.
    #[must_use]
    pub fn with_inline_styles(mut self) -> Self {
        self.inline_styles = true;
        self
    }

    /// Produces one safe Content-Security-Policy response header value.
    #[must_use]
    pub fn header_value(&self) -> String {
        let mut value =
            "default-src 'self'; object-src 'none'; base-uri 'none'; frame-src 'none'; script-src 'self'; connect-src 'self'"
                .to_owned();
        for origin in &self.connect_origins {
            value.push(' ');
            value.push_str(origin);
        }
        value.push_str("; worker-src 'self'");
        if self.blob_workers {
            value.push_str(" blob:");
        }
        if self.inline_styles {
            value.push_str(
                "; style-src 'self' 'unsafe-inline'; font-src 'self' data:; img-src 'self' data:",
            );
        } else {
            value.push_str("; style-src 'self'; font-src 'self' data:; img-src 'self' data:");
        }
        value.push(';');
        value
    }
}

/// Stable opaque identifier for one page lifetime.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PageSessionId(NonZeroU128);

impl PageSessionId {
    /// Creates a non-zero page session identifier.
    #[must_use]
    pub const fn new(value: NonZeroU128) -> Self {
        Self(value)
    }

    /// Parses exactly 32 lowercase-or-uppercase hexadecimal characters.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid, zero, or non-canonical sized value.
    pub fn parse(value: &str) -> Result<Self, PageSessionIdError> {
        if value.len() != 32 {
            return Err(PageSessionIdError::WrongLength);
        }
        let value = u128::from_str_radix(value, 16).map_err(|_| PageSessionIdError::InvalidHex)?;
        let value = NonZeroU128::new(value).ok_or(PageSessionIdError::Zero)?;
        Ok(Self(value))
    }

    /// Returns a fixed-width lowercase hexadecimal representation.
    #[must_use]
    pub fn to_hex(self) -> String {
        format!("{:032x}", self.0)
    }
}

impl fmt::Display for PageSessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

impl Serialize for PageSessionId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for PageSessionId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

/// Reason a page session ID was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PageSessionIdError {
    /// The string did not contain exactly 32 hexadecimal characters.
    WrongLength,
    /// The string was not hexadecimal.
    InvalidHex,
    /// The all-zero value is reserved and rejected.
    Zero,
}

impl fmt::Display for PageSessionIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongLength => {
                formatter.write_str("page session ID must contain 32 hex characters")
            }
            Self::InvalidHex => formatter.write_str("page session ID is not hexadecimal"),
            Self::Zero => formatter.write_str("page session ID must be non-zero"),
        }
    }
}

impl Error for PageSessionIdError {}

/// Non-zero correlation identifier for one browser message.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MessageId(NonZeroU64);

impl MessageId {
    /// Validates and creates a non-zero correlation identifier.
    #[must_use]
    pub const fn new(value: NonZeroU64) -> Self {
        Self(value)
    }

    /// Returns the numeric correlation identifier.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Validated endpoint name in the message schema.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EndpointName(String);

impl EndpointName {
    /// Validates an ASCII endpoint name such as document.save.
    ///
    /// # Errors
    ///
    /// Returns an error if the name is empty, too long, starts with a
    /// non-letter, or contains an unapproved character.
    pub fn parse(value: impl AsRef<str>) -> Result<Self, EndpointNameError> {
        let value = value.as_ref();
        if value.is_empty() {
            return Err(EndpointNameError::Empty);
        }
        if value.len() > MAX_ENDPOINT_NAME_BYTES {
            return Err(EndpointNameError::TooLong {
                limit: MAX_ENDPOINT_NAME_BYTES,
            });
        }
        let mut bytes = value.bytes();
        let Some(first) = bytes.next() else {
            return Err(EndpointNameError::Empty);
        };
        if !first.is_ascii_alphabetic() {
            return Err(EndpointNameError::InvalidCharacter);
        }
        if !bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')) {
            return Err(EndpointNameError::InvalidCharacter);
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the validated endpoint name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EndpointName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Reason an endpoint name was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EndpointNameError {
    /// The name was empty.
    Empty,
    /// The name exceeded the fixed endpoint-name limit.
    TooLong {
        /// Maximum endpoint UTF-8 byte length.
        limit: usize,
    },
    /// The name used an invalid start or character.
    InvalidCharacter,
}

impl fmt::Display for EndpointNameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("endpoint name must not be empty"),
            Self::TooLong { limit } => write!(formatter, "endpoint name exceeds {limit} bytes"),
            Self::InvalidCharacter => formatter.write_str("endpoint name has invalid characters"),
        }
    }
}

impl Error for EndpointNameError {}

/// Limits used before a browser message reaches any application handler.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BridgeLimits {
    protocol_version: u16,
    max_message_bytes: usize,
    max_payload_bytes: usize,
    max_endpoint_bytes: usize,
}

impl BridgeLimits {
    /// Validates explicit message-schema limits.
    ///
    /// # Errors
    ///
    /// Returns an error for a zero version or zero/inconsistent byte limits.
    pub fn new(
        protocol_version: u16,
        max_message_bytes: usize,
        max_payload_bytes: usize,
        max_endpoint_bytes: usize,
    ) -> Result<Self, BridgeLimitsError> {
        if protocol_version == 0 {
            return Err(BridgeLimitsError::ZeroProtocolVersion);
        }
        if max_message_bytes == 0 || max_payload_bytes == 0 || max_endpoint_bytes == 0 {
            return Err(BridgeLimitsError::ZeroByteLimit);
        }
        if max_payload_bytes > max_message_bytes {
            return Err(BridgeLimitsError::PayloadExceedsMessage);
        }
        if max_endpoint_bytes > MAX_ENDPOINT_NAME_BYTES {
            return Err(BridgeLimitsError::EndpointLimitTooLarge {
                maximum: MAX_ENDPOINT_NAME_BYTES,
            });
        }
        Ok(Self {
            protocol_version,
            max_message_bytes,
            max_payload_bytes,
            max_endpoint_bytes,
        })
    }

    /// Returns the accepted protocol version.
    #[must_use]
    pub const fn protocol_version(self) -> u16 {
        self.protocol_version
    }

    /// Returns the total encoded JSON byte ceiling.
    #[must_use]
    pub const fn max_message_bytes(self) -> usize {
        self.max_message_bytes
    }

    /// Returns the canonical JSON payload byte ceiling.
    #[must_use]
    pub const fn max_payload_bytes(self) -> usize {
        self.max_payload_bytes
    }

    /// Returns the endpoint-name byte ceiling.
    #[must_use]
    pub const fn max_endpoint_bytes(self) -> usize {
        self.max_endpoint_bytes
    }
}

impl Default for BridgeLimits {
    fn default() -> Self {
        Self {
            protocol_version: 1,
            max_message_bytes: 64 * 1024,
            max_payload_bytes: 48 * 1024,
            max_endpoint_bytes: MAX_ENDPOINT_NAME_BYTES,
        }
    }
}

/// Invalid bridge-limit configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BridgeLimitsError {
    /// Version zero is reserved.
    ZeroProtocolVersion,
    /// A byte limit was zero.
    ZeroByteLimit,
    /// Payload limit must fit inside total message limit.
    PayloadExceedsMessage,
    /// Endpoint-name limit cannot exceed the implementation hard limit.
    EndpointLimitTooLarge {
        /// Maximum endpoint-name byte count.
        maximum: usize,
    },
}

impl fmt::Display for BridgeLimitsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroProtocolVersion => formatter.write_str("protocol version must be non-zero"),
            Self::ZeroByteLimit => formatter.write_str("bridge byte limits must be non-zero"),
            Self::PayloadExceedsMessage => {
                formatter.write_str("payload limit must not exceed message limit")
            }
            Self::EndpointLimitTooLarge { maximum } => {
                write!(formatter, "endpoint limit must not exceed {maximum} bytes")
            }
        }
    }
}

impl Error for BridgeLimitsError {}

/// A bounded validated JSON message ready for future endpoint routing.
#[derive(Clone, Debug, PartialEq)]
pub struct BridgeEnvelope {
    version: u16,
    session: PageSessionId,
    id: MessageId,
    endpoint: EndpointName,
    payload: Value,
}

impl BridgeEnvelope {
    /// Creates a validated message envelope.
    ///
    /// # Errors
    ///
    /// Returns a bridge error when supplied values exceed selected limits or
    /// the configured protocol version.
    pub fn new(
        version: u16,
        session: PageSessionId,
        id: MessageId,
        endpoint: EndpointName,
        payload: Value,
        limits: BridgeLimits,
    ) -> Result<Self, BridgeError> {
        let value = Self {
            version,
            session,
            id,
            endpoint,
            payload,
        };
        value.validate(limits)?;
        Ok(value)
    }

    /// Parses and validates one untrusted JSON message.
    ///
    /// # Errors
    ///
    /// Returns a structured bridge error before any future endpoint handler
    /// can observe malformed, oversized, stale-version, or invalid messages.
    pub fn parse_json(input: &[u8], limits: BridgeLimits) -> Result<Self, BridgeError> {
        if input.len() > limits.max_message_bytes {
            return Err(BridgeError::MessageTooLarge {
                limit: limits.max_message_bytes,
            });
        }
        let wire: WireEnvelope =
            serde_json::from_slice(input).map_err(|_| BridgeError::InvalidJson)?;
        let session = PageSessionId::parse(&wire.session).map_err(BridgeError::Session)?;
        let id = NonZeroU64::new(wire.id)
            .map(MessageId::new)
            .ok_or(BridgeError::ZeroMessageId)?;
        let endpoint = EndpointName::parse(wire.endpoint).map_err(BridgeError::Endpoint)?;
        Self::new(wire.version, session, id, endpoint, wire.payload, limits)
    }

    /// Serializes the message only after applying all configured limits.
    ///
    /// # Errors
    ///
    /// Returns a bridge error when the envelope no longer satisfies configured
    /// protocol limits.
    pub fn to_json(&self, limits: BridgeLimits) -> Result<Vec<u8>, BridgeError> {
        self.validate(limits)?;
        let output = serde_json::to_vec(&WireEnvelope {
            version: self.version,
            session: self.session.to_hex(),
            id: self.id.get(),
            endpoint: self.endpoint.as_str().to_owned(),
            payload: self.payload.clone(),
        })
        .map_err(|_| BridgeError::Serialization)?;
        if output.len() > limits.max_message_bytes {
            return Err(BridgeError::MessageTooLarge {
                limit: limits.max_message_bytes,
            });
        }
        Ok(output)
    }

    /// Returns the protocol version.
    #[must_use]
    pub const fn version(&self) -> u16 {
        self.version
    }

    /// Returns the page session that must match the currently loaded page.
    #[must_use]
    pub const fn session(&self) -> PageSessionId {
        self.session
    }

    /// Returns the non-zero correlation identifier.
    #[must_use]
    pub const fn id(&self) -> MessageId {
        self.id
    }

    /// Returns the validated endpoint name.
    #[must_use]
    pub fn endpoint(&self) -> &EndpointName {
        &self.endpoint
    }

    /// Returns JSON payload data without granting dispatch privileges.
    #[must_use]
    pub const fn payload(&self) -> &Value {
        &self.payload
    }

    fn validate(&self, limits: BridgeLimits) -> Result<(), BridgeError> {
        if self.version != limits.protocol_version {
            return Err(BridgeError::UnsupportedVersion {
                expected: limits.protocol_version,
                actual: self.version,
            });
        }
        if self.endpoint.as_str().len() > limits.max_endpoint_bytes {
            return Err(BridgeError::EndpointTooLong {
                limit: limits.max_endpoint_bytes,
            });
        }
        let payload_bytes = serde_json::to_vec(&self.payload)
            .map_err(|_| BridgeError::Serialization)?
            .len();
        if payload_bytes > limits.max_payload_bytes {
            return Err(BridgeError::PayloadTooLarge {
                limit: limits.max_payload_bytes,
            });
        }
        Ok(())
    }
}

/// A bounded host-to-page event for the current local page session.
///
/// Event names use the same strict ASCII rule as endpoint names. The event is
/// one-way and grants no host capability to page code.
#[derive(Clone, Debug, PartialEq)]
pub struct PageEvent {
    version: u16,
    session: PageSessionId,
    event: EndpointName,
    payload: Value,
}

impl PageEvent {
    /// Creates a validated host-to-page event from JSON payload data.
    ///
    /// # Errors
    ///
    /// Returns an event error when version, event name, payload, or total JSON
    /// bytes violate the supplied bridge limits.
    pub fn new(
        version: u16,
        session: PageSessionId,
        event: EndpointName,
        payload: Value,
        limits: BridgeLimits,
    ) -> Result<Self, PageEventError> {
        let value = Self {
            version,
            session,
            event,
            payload,
        };
        value.validate(limits)?;
        Ok(value)
    }

    /// Serializes an application type as the event payload before validation.
    ///
    /// # Errors
    ///
    /// Returns an event error when the value cannot become JSON or violates
    /// the supplied bridge limits.
    pub fn from_typed<T: Serialize>(
        version: u16,
        session: PageSessionId,
        event: EndpointName,
        payload: T,
        limits: BridgeLimits,
    ) -> Result<Self, PageEventError> {
        let payload = serde_json::to_value(payload).map_err(|_| PageEventError::Serialization)?;
        Self::new(version, session, event, payload, limits)
    }

    /// Serializes the event as bounded JSON.
    ///
    /// # Errors
    ///
    /// Returns an event error if the event violates the supplied limits.
    pub fn to_json(&self, limits: BridgeLimits) -> Result<Vec<u8>, PageEventError> {
        self.validate(limits)?;
        let output = serde_json::to_vec(&WirePageEvent {
            version: self.version,
            session: self.session.to_hex(),
            event: self.event.as_str().to_owned(),
            payload: self.payload.clone(),
        })
        .map_err(|_| PageEventError::Serialization)?;
        if output.len() > limits.max_message_bytes {
            return Err(PageEventError::MessageTooLarge {
                limit: limits.max_message_bytes,
            });
        }
        Ok(output)
    }

    /// Returns the protocol version.
    #[must_use]
    pub const fn version(&self) -> u16 {
        self.version
    }

    /// Returns the page session bound to this event.
    #[must_use]
    pub const fn session(&self) -> PageSessionId {
        self.session
    }

    /// Returns the validated event name.
    #[must_use]
    pub fn event(&self) -> &EndpointName {
        &self.event
    }

    /// Returns immutable JSON payload data.
    #[must_use]
    pub const fn payload(&self) -> &Value {
        &self.payload
    }

    fn validate(&self, limits: BridgeLimits) -> Result<(), PageEventError> {
        if self.version != limits.protocol_version {
            return Err(PageEventError::UnsupportedVersion {
                expected: limits.protocol_version,
                actual: self.version,
            });
        }
        if self.event.as_str().len() > limits.max_endpoint_bytes {
            return Err(PageEventError::EventTooLong {
                limit: limits.max_endpoint_bytes,
            });
        }
        let payload_bytes = serde_json::to_vec(&self.payload)
            .map_err(|_| PageEventError::Serialization)?
            .len();
        if payload_bytes > limits.max_payload_bytes {
            return Err(PageEventError::PayloadTooLarge {
                limit: limits.max_payload_bytes,
            });
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct WirePageEvent {
    version: u16,
    session: String,
    event: String,
    payload: Value,
}

/// Host-to-page event validation or serialization failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PageEventError {
    /// Event version did not match the selected bridge version.
    UnsupportedVersion {
        /// Expected protocol version.
        expected: u16,
        /// Event protocol version.
        actual: u16,
    },
    /// Event name exceeded the configured endpoint/event byte limit.
    EventTooLong {
        /// Configured byte limit.
        limit: usize,
    },
    /// Canonical JSON payload bytes exceeded their configured limit.
    PayloadTooLarge {
        /// Configured byte limit.
        limit: usize,
    },
    /// Total serialized JSON bytes exceeded their configured limit.
    MessageTooLarge {
        /// Configured byte limit.
        limit: usize,
    },
    /// JSON conversion or serialization failed.
    Serialization,
}

impl fmt::Display for PageEventError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion { expected, actual } => {
                write!(
                    formatter,
                    "event protocol version {actual} does not match {expected}"
                )
            }
            Self::EventTooLong { limit } => write!(formatter, "event name exceeds {limit} bytes"),
            Self::PayloadTooLarge { limit } => {
                write!(formatter, "event payload exceeds {limit} bytes")
            }
            Self::MessageTooLarge { limit } => {
                write!(formatter, "event message exceeds {limit} bytes")
            }
            Self::Serialization => formatter.write_str("event JSON serialization failed"),
        }
    }
}

impl Error for PageEventError {}

#[derive(Deserialize, Serialize)]
struct WireEnvelope {
    version: u16,
    session: String,
    id: u64,
    endpoint: String,
    payload: Value,
}

/// A message-envelope parse or validation failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BridgeError {
    /// The complete untrusted JSON input exceeded its byte limit.
    MessageTooLarge {
        /// Configured byte limit.
        limit: usize,
    },
    /// The input was not the expected JSON object shape.
    InvalidJson,
    /// The page-session string was invalid.
    Session(PageSessionIdError),
    /// Correlation ID zero is invalid.
    ZeroMessageId,
    /// The endpoint name was invalid.
    Endpoint(EndpointNameError),
    /// The message version differed from the accepted version.
    UnsupportedVersion {
        /// Accepted version.
        expected: u16,
        /// Received version.
        actual: u16,
    },
    /// A valid endpoint name exceeded the caller configured limit.
    EndpointTooLong {
        /// Configured byte limit.
        limit: usize,
    },
    /// Canonical JSON payload bytes exceeded their configured limit.
    PayloadTooLarge {
        /// Configured byte limit.
        limit: usize,
    },
    /// JSON serialization failed.
    Serialization,
}

impl fmt::Display for BridgeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MessageTooLarge { limit } => {
                write!(formatter, "bridge message exceeds {limit} bytes")
            }
            Self::InvalidJson => formatter.write_str("bridge message is invalid JSON"),
            Self::Session(error) => write!(formatter, "bridge session is invalid: {error}"),
            Self::ZeroMessageId => formatter.write_str("bridge message ID must be non-zero"),
            Self::Endpoint(error) => write!(formatter, "bridge endpoint is invalid: {error}"),
            Self::UnsupportedVersion { expected, actual } => {
                write!(
                    formatter,
                    "bridge protocol version {actual} does not match {expected}"
                )
            }
            Self::EndpointTooLong { limit } => {
                write!(formatter, "bridge endpoint exceeds {limit} bytes")
            }
            Self::PayloadTooLarge { limit } => {
                write!(formatter, "bridge payload exceeds {limit} bytes")
            }
            Self::Serialization => formatter.write_str("bridge JSON serialization failed"),
        }
    }
}

impl Error for BridgeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Session(error) => Some(error),
            Self::Endpoint(error) => Some(error),
            _ => None,
        }
    }
}

/// Status code produced by the safe local-asset protocol.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalProtocolStatus {
    /// A local asset was resolved.
    Ok,
    /// The request syntax, method, or body was invalid.
    BadRequest,
    /// The request did not target the local application origin.
    Forbidden,
    /// The safe logical path did not exist in the bundle.
    NotFound,
    /// The request method is not supported.
    MethodNotAllowed,
}

impl LocalProtocolStatus {
    /// Returns the HTTP-compatible status code expected by the Wry adapter.
    #[must_use]
    pub const fn code(self) -> u16 {
        match self {
            Self::Ok => 200,
            Self::BadRequest => 400,
            Self::Forbidden => 403,
            Self::NotFound => 404,
            Self::MethodNotAllowed => 405,
        }
    }
}

/// Response data produced without an operating-system or Wry dependency.
#[derive(Clone, Debug)]
pub struct LocalProtocolResponse {
    status: LocalProtocolStatus,
    asset: Option<LocalAsset>,
    csp: String,
    head_only: bool,
}

impl LocalProtocolResponse {
    /// Returns the response status.
    #[must_use]
    pub const fn status(&self) -> LocalProtocolStatus {
        self.status
    }

    /// Returns the asset content type only for successful lookups.
    #[must_use]
    pub fn content_type(&self) -> Option<&'static str> {
        self.asset.as_ref().map(LocalAsset::content_type)
    }

    /// Returns immutable response bytes, or an empty error body.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        if self.head_only {
            &[]
        } else {
            self.asset.as_ref().map_or(&[], LocalAsset::bytes)
        }
    }

    /// Returns the restrictive CSP header applied by the future Wry adapter.
    #[must_use]
    pub fn csp_header(&self) -> &str {
        &self.csp
    }
}

/// Pure request resolver for the fixed local application origin.
///
/// This type owns only the provided in-memory AssetBundle. It cannot access
/// disk or network resources. On Windows Wry maps the registered app scheme to
/// the HTTPS app.localhost origin when HTTPS custom protocols are enabled.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalAssetProtocol {
    assets: AssetBundle,
    csp: LocalCsp,
}

impl LocalAssetProtocol {
    /// Creates a local protocol from a bounded in-memory asset bundle.
    #[must_use]
    pub fn new(assets: AssetBundle, csp: LocalCsp) -> Self {
        Self { assets, csp }
    }

    /// Resolves one Wry-style custom-protocol request without I/O.
    ///
    /// Only empty-body GET and HEAD requests to HTTPS app.localhost or Wry's
    /// internal restored `app://localhost` source are accepted. Wry maps the
    /// latter to the former for WebView2 navigation, then restores it before
    /// invoking the protocol handler. URL query strings, fragment-like percent
    /// encoding, unsafe logical paths, and every other origin fail closed.
    #[must_use]
    pub fn handle(&self, method: &str, request_url: &str, has_body: bool) -> LocalProtocolResponse {
        let csp = self.csp.header_value();
        if has_body {
            return LocalProtocolResponse {
                status: LocalProtocolStatus::BadRequest,
                asset: None,
                csp,
                head_only: false,
            };
        }
        if !matches!(method, "GET" | "HEAD") {
            return LocalProtocolResponse {
                status: LocalProtocolStatus::MethodNotAllowed,
                asset: None,
                csp,
                head_only: false,
            };
        }
        if request_url.contains('%') {
            return LocalProtocolResponse {
                status: LocalProtocolStatus::BadRequest,
                asset: None,
                csp,
                head_only: false,
            };
        }
        let Ok(url) = Url::parse(request_url) else {
            return LocalProtocolResponse {
                status: LocalProtocolStatus::BadRequest,
                asset: None,
                csp,
                head_only: false,
            };
        };
        if !is_local_app_url(&url) && !is_local_protocol_source_url(&url) {
            return LocalProtocolResponse {
                status: LocalProtocolStatus::Forbidden,
                asset: None,
                csp,
                head_only: false,
            };
        }
        if url.query().is_some() {
            return LocalProtocolResponse {
                status: LocalProtocolStatus::BadRequest,
                asset: None,
                csp,
                head_only: false,
            };
        }
        let Some(path) = url.path().strip_prefix('/') else {
            return LocalProtocolResponse {
                status: LocalProtocolStatus::BadRequest,
                asset: None,
                csp,
                head_only: false,
            };
        };
        let Ok(path) = AssetPath::parse(path) else {
            return LocalProtocolResponse {
                status: LocalProtocolStatus::BadRequest,
                asset: None,
                csp,
                head_only: false,
            };
        };
        let asset = self.assets.get(&path).cloned();
        LocalProtocolResponse {
            status: if asset.is_some() {
                LocalProtocolStatus::Ok
            } else {
                LocalProtocolStatus::NotFound
            },
            asset,
            csp,
            head_only: method == "HEAD",
        }
    }

    /// Returns the asset bundle used by this resolver.
    #[must_use]
    pub const fn assets(&self) -> &AssetBundle {
        &self.assets
    }
}

/// A structured typed endpoint used by a local bridge router.
///
/// The trait is intentionally one-way. It receives already bounded and
/// validated JSON and exposes no JavaScript evaluator, browser handle,
/// filesystem, process, or network capability.
pub trait BridgeEndpoint {
    /// Returns the explicitly registered endpoint name.
    fn name(&self) -> &EndpointName;

    /// Decodes and handles one already authorized payload.
    ///
    /// # Errors
    ///
    /// Returns a structured error when the JSON payload cannot be decoded as
    /// the endpoint request type.
    fn dispatch(&mut self, payload: &Value) -> Result<(), EndpointDispatchError>;
}

/// A typed endpoint implemented by an application closure.
pub struct TypedEndpoint<T, F> {
    name: EndpointName,
    handler: F,
    marker: std::marker::PhantomData<fn(T)>,
}

impl<T, F> TypedEndpoint<T, F> {
    /// Registers a closure for one typed endpoint name.
    #[must_use]
    pub fn new(name: EndpointName, handler: F) -> Self {
        Self {
            name,
            handler,
            marker: std::marker::PhantomData,
        }
    }
}

impl<T, F> BridgeEndpoint for TypedEndpoint<T, F>
where
    T: DeserializeOwned,
    F: FnMut(T),
{
    fn name(&self) -> &EndpointName {
        &self.name
    }

    fn dispatch(&mut self, payload: &Value) -> Result<(), EndpointDispatchError> {
        let request = serde_json::from_value(payload.clone())
            .map_err(|_| EndpointDispatchError::InvalidPayload)?;
        (self.handler)(request);
        Ok(())
    }
}

/// Failure while decoding the request type of one registered endpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EndpointDispatchError {
    /// Payload JSON did not match the endpoint request type.
    InvalidPayload,
}

impl fmt::Display for EndpointDispatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPayload => {
                formatter.write_str("bridge payload did not match endpoint type")
            }
        }
    }
}

impl Error for EndpointDispatchError {}

/// Explicit router for a single local page session.
///
/// It accepts messages only from the fixed local application origin, checks
/// bounds and protocol format through BridgeEnvelope, rejects stale sessions,
/// and dispatches only pre-registered endpoint names.
pub struct BridgeRouter {
    session: PageSessionId,
    limits: BridgeLimits,
    endpoints: BTreeMap<EndpointName, Box<dyn BridgeEndpoint>>,
}

impl fmt::Debug for BridgeRouter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BridgeRouter")
            .field("session", &self.session)
            .field("limits", &self.limits)
            .field("endpoints", &self.endpoints.keys())
            .finish()
    }
}

impl BridgeRouter {
    /// Creates an empty router for one current local page session.
    #[must_use]
    pub fn new(session: PageSessionId, limits: BridgeLimits) -> Self {
        Self {
            session,
            limits,
            endpoints: BTreeMap::new(),
        }
    }

    /// Returns the only page session accepted by this router.
    #[must_use]
    pub const fn session(&self) -> PageSessionId {
        self.session
    }

    /// Returns the bounds enforced for both incoming and outgoing page data.
    #[must_use]
    pub const fn limits(&self) -> BridgeLimits {
        self.limits
    }

    /// Registers one typed endpoint.
    ///
    /// # Errors
    ///
    /// Returns an error when an endpoint name is registered more than once.
    pub fn register(
        &mut self,
        endpoint: impl BridgeEndpoint + 'static,
    ) -> Result<(), BridgeRouterError> {
        let name = endpoint.name().clone();
        if self.endpoints.contains_key(&name) {
            return Err(BridgeRouterError::DuplicateEndpoint(name));
        }
        self.endpoints.insert(name, Box::new(endpoint));
        Ok(())
    }

    /// Validates origin/session/schema and invokes one explicitly registered endpoint.
    ///
    /// # Errors
    ///
    /// Returns a structured error for invalid origins, bridge envelopes, stale
    /// sessions, unknown endpoints, or endpoint payload-type mismatches.
    pub fn dispatch(&mut self, origin_url: &str, json: &[u8]) -> Result<(), BridgeRouterError> {
        let Ok(origin) = Url::parse(origin_url) else {
            return Err(BridgeRouterError::InvalidOrigin);
        };
        // WebView2 may report either the workaround HTTPS origin or the raw
        // custom-protocol source URL depending on OS build / wry mapping.
        if !is_local_app_url(&origin) && !is_local_protocol_source_url(&origin) {
            return Err(BridgeRouterError::InvalidOrigin);
        }
        let message =
            BridgeEnvelope::parse_json(json, self.limits).map_err(BridgeRouterError::Envelope)?;
        if message.session() != self.session {
            return Err(BridgeRouterError::StaleSession {
                expected: self.session,
                actual: message.session(),
            });
        }
        let name = message.endpoint().clone();
        let endpoint = self
            .endpoints
            .get_mut(&name)
            .ok_or(BridgeRouterError::UnknownEndpoint(name))?;
        endpoint
            .dispatch(message.payload())
            .map_err(BridgeRouterError::Dispatch)
    }
}

/// Router rejection that is safe to record without exposing an application handler.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BridgeRouterError {
    /// The IPC request URL was not the fixed local application origin.
    InvalidOrigin,
    /// The bounded bridge envelope was invalid.
    Envelope(BridgeError),
    /// The message belongs to an earlier or different local page session.
    StaleSession {
        /// Current host page session.
        expected: PageSessionId,
        /// Session claimed by the incoming message.
        actual: PageSessionId,
    },
    /// No endpoint was registered under this validated name.
    UnknownEndpoint(EndpointName),
    /// The application tried to register an endpoint twice.
    DuplicateEndpoint(EndpointName),
    /// The endpoint request type did not match the JSON payload.
    Dispatch(EndpointDispatchError),
}

impl fmt::Display for BridgeRouterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidOrigin => {
                formatter.write_str("bridge request came from an invalid origin")
            }
            Self::Envelope(error) => write!(formatter, "bridge envelope rejected: {error}"),
            Self::StaleSession { .. } => {
                formatter.write_str("bridge request used a stale page session")
            }
            Self::UnknownEndpoint(name) => {
                write!(formatter, "bridge endpoint is not registered: {name}")
            }
            Self::DuplicateEndpoint(name) => {
                write!(formatter, "bridge endpoint is already registered: {name}")
            }
            Self::Dispatch(error) => write!(formatter, "bridge endpoint rejected payload: {error}"),
        }
    }
}

impl Error for BridgeRouterError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Envelope(error) => Some(error),
            Self::Dispatch(error) => Some(error),
            _ => None,
        }
    }
}

pub(crate) fn is_local_app_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url
            .host_str()
            .is_some_and(|host| host.eq_ignore_ascii_case(LOCAL_PROTOCOL_HOST))
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
}

fn is_local_protocol_source_url(url: &Url) -> bool {
    url.scheme() == LOCAL_PROTOCOL_SOURCE_SCHEME
        && url
            .host_str()
            .is_some_and(|host| host.eq_ignore_ascii_case(LOCAL_PROTOCOL_SOURCE_HOST))
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use super::*;

    fn session() -> PageSessionId {
        PageSessionId::parse("1234567890abcdef1234567890abcdef").expect("session")
    }

    #[test]
    fn logical_assets_reject_filesystem_escape_and_mime_sniffing() {
        for invalid in [
            "",
            "/index.html",
            "C:/index.html",
            "ui/../secret.html",
            "ui\\\\secret.html",
            "ui/%2e%2e/secret.html",
            "ui//index.html",
        ] {
            assert!(AssetPath::parse(invalid).is_err(), "{invalid}");
        }
        let path = AssetPath::parse("ui/index.html").expect("safe logical path");
        let mut bundle = AssetBundle::new(MimePolicy::strict(), AssetLimits::default());
        bundle
            .insert(path.clone(), "<h1>local</h1>")
            .expect("asset");
        let asset = bundle.get(&path).expect("asset lookup");
        assert_eq!(asset.content_type(), "text/html; charset=utf-8");
        assert!(matches!(
            MimePolicy::strict().resolve(&AssetPath::parse("ui/app.wasm").expect("path")),
            Err(MimePolicyError::WasmDisabled)
        ));
    }

    #[test]
    fn csp_requires_explicit_remote_connection_opt_in() {
        let strict = LocalCsp::strict().header_value();
        assert_eq!(
            strict,
            "default-src 'self'; object-src 'none'; base-uri 'none'; frame-src 'none'; script-src 'self'; connect-src 'self'; worker-src 'self'; style-src 'self'; font-src 'self' data:; img-src 'self' data:;"
        );
        let endpoint = ControlledUrl::parse("https://api.example.test/v1").expect("controlled");
        assert!(
            LocalCsp::strict()
                .with_connect_origin(&endpoint)
                .header_value()
                .contains("'self' https://api.example.test;")
        );
        let monaco = LocalCsp::strict()
            .with_blob_workers()
            .with_inline_styles()
            .header_value();
        assert!(monaco.contains("worker-src 'self' blob:"));
        assert!(monaco.contains("style-src 'self' 'unsafe-inline'"));
    }

    #[test]
    fn bridge_round_trip_is_bounded_and_structured() {
        let limits = BridgeLimits::new(1, 256, 96, 32).expect("limits");
        let message = BridgeEnvelope::new(
            1,
            session(),
            MessageId::new(NonZeroU64::new(7).expect("non-zero")),
            EndpointName::parse("document.save").expect("endpoint"),
            serde_json::json!({ "slot": 3 }),
            limits,
        )
        .expect("message");
        let encoded = message.to_json(limits).expect("serialize");
        let decoded = BridgeEnvelope::parse_json(&encoded, limits).expect("parse");
        assert_eq!(decoded, message);

        let oversized = br#"{"version":1,"session":"1234567890abcdef1234567890abcdef","id":1,"endpoint":"document.save","payload":{"text":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}"#;
        assert!(matches!(
            BridgeEnvelope::parse_json(oversized, limits),
            Err(BridgeError::PayloadTooLarge { .. })
        ));
        assert!(matches!(
            BridgeEnvelope::parse_json(
                br#"{"version":2,"session":"1234567890abcdef1234567890abcdef","id":1,"endpoint":"document.save","payload":{}}"#,
                limits
            ),
            Err(BridgeError::UnsupportedVersion { .. })
        ));
    }

    #[test]
    fn typed_page_event_is_bounded_and_serializes_a_session() {
        #[derive(Serialize)]
        struct Notice {
            text: &'static str,
        }

        let limits = BridgeLimits::new(1, 256, 64, 32).expect("limits");
        let event = PageEvent::from_typed(
            1,
            session(),
            EndpointName::parse("ui.notice").expect("event"),
            Notice { text: "saved" },
            limits,
        )
        .expect("event");
        let encoded = event.to_json(limits).expect("json");
        let value: Value = serde_json::from_slice(&encoded).expect("event json");
        assert_eq!(value["session"], session().to_hex());
        assert_eq!(value["event"], "ui.notice");
        assert!(matches!(
            PageEvent::new(
                1,
                session(),
                EndpointName::parse("ui.notice").expect("event"),
                serde_json::json!({ "text": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" }),
                limits,
            ),
            Err(PageEventError::PayloadTooLarge { .. })
        ));
    }

    #[test]
    fn local_protocol_allows_only_safe_local_gets() {
        let path = AssetPath::parse("index.html").expect("path");
        let mut bundle = AssetBundle::new(MimePolicy::strict(), AssetLimits::default());
        bundle.insert(path, "<main>safe</main>").expect("asset");
        let protocol = LocalAssetProtocol::new(bundle, LocalCsp::strict());

        let response = protocol.handle("GET", "https://app.localhost/index.html", false);
        assert_eq!(response.status(), LocalProtocolStatus::Ok);
        assert_eq!(response.bytes(), b"<main>safe</main>");
        assert_eq!(response.content_type(), Some("text/html; charset=utf-8"));
        assert!(response.csp_header().contains("object-src 'none'"));

        let wry_source = protocol.handle("GET", "app://localhost/index.html", false);
        assert_eq!(wry_source.status(), LocalProtocolStatus::Ok);
        assert_eq!(wry_source.bytes(), b"<main>safe</main>");

        let head = protocol.handle("HEAD", "https://app.localhost/index.html", false);
        assert_eq!(head.status(), LocalProtocolStatus::Ok);
        assert!(head.bytes().is_empty());
        assert_eq!(
            protocol
                .handle("GET", "https://app.localhost/%2e%2e/secret", false)
                .status(),
            LocalProtocolStatus::BadRequest
        );
        assert_eq!(
            protocol
                .handle("GET", "https://remote.example/index.html", false)
                .status(),
            LocalProtocolStatus::Forbidden
        );
        assert_eq!(
            protocol
                .handle("GET", "app://remote.example/index.html", false)
                .status(),
            LocalProtocolStatus::Forbidden
        );
        assert_eq!(
            protocol
                .handle("POST", "https://app.localhost/index.html", false)
                .status(),
            LocalProtocolStatus::MethodNotAllowed
        );
    }

    #[test]
    fn typed_router_rejects_origin_stale_session_unknown_and_oversize() {
        #[derive(Deserialize)]
        struct SaveRequest {
            slot: u32,
        }

        let limits = BridgeLimits::new(1, 256, 96, 32).expect("limits");
        let seen = Rc::new(RefCell::new(Vec::new()));
        let mut router = BridgeRouter::new(session(), limits);
        let seen_by_handler = Rc::clone(&seen);
        router
            .register(TypedEndpoint::new(
                EndpointName::parse("document.save").expect("endpoint"),
                move |request: SaveRequest| seen_by_handler.borrow_mut().push(request.slot),
            ))
            .expect("register");

        let message = BridgeEnvelope::new(
            1,
            session(),
            MessageId::new(NonZeroU64::new(1).expect("id")),
            EndpointName::parse("document.save").expect("endpoint"),
            serde_json::json!({ "slot": 9 }),
            limits,
        )
        .expect("message")
        .to_json(limits)
        .expect("json");
        router
            .dispatch("https://app.localhost/index.html", &message)
            .expect("local typed dispatch");
        assert_eq!(*seen.borrow(), vec![9]);
        assert!(matches!(
            router.dispatch("https://remote.example/index.html", &message),
            Err(BridgeRouterError::InvalidOrigin)
        ));
        router
            .dispatch("app://localhost/index.html", &message)
            .expect("custom-protocol source origin must be accepted");

        let stale = br#"{"version":1,"session":"1234567890abcdef1234567890abcdee","id":1,"endpoint":"document.save","payload":{"slot":1}}"#;
        assert!(matches!(
            router.dispatch("https://app.localhost/index.html", stale),
            Err(BridgeRouterError::StaleSession { .. })
        ));
        let unknown = br#"{"version":1,"session":"1234567890abcdef1234567890abcdef","id":1,"endpoint":"document.delete","payload":{}}"#;
        assert!(matches!(
            router.dispatch("https://app.localhost/index.html", unknown),
            Err(BridgeRouterError::UnknownEndpoint(_))
        ));
        assert!(matches!(
            router.dispatch("https://app.localhost/index.html", &[b'x'; 257]),
            Err(BridgeRouterError::Envelope(
                BridgeError::MessageTooLarge { .. }
            ))
        ));
    }
}
