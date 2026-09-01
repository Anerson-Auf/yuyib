//! Optional Windows `WebView2` hosting for a Yuyib platform window.
//!
//! This crate hosts developer-owned inline HTML or one explicitly controlled
//! HTTPS page. A local in-memory asset protocol and an explicit typed one-way
//! IPC router are available for application pages. The crate has no filesystem
//! asset protocol, host objects, generic browser API, or unrestricted bridge.
//! The asset and schema foundations expose no filesystem, process, or browser
//! capability by themselves.
//!
//! Build a host on the UI/event-loop thread that created the platform window.
//! The host retains an internal parent-window clone and drops its native child
//! controller first.
//!
//! Inline HTML is the default and browser navigation is rejected. A controlled
//! URL permits only its exact initial HTTPS URL. Developer script evaluation
//! accepts static developer-owned code only, never interpolated external data.
//!
//! This backend requires the Microsoft Edge `WebView2` Runtime. See
//! <https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/distribution>.

#![forbid(unsafe_code)]

#[cfg(not(all(target_os = "windows", feature = "webview2")))]
compile_error!("yuyib-webview currently requires Windows with the webview2 feature enabled");

#[cfg(all(target_os = "windows", feature = "webview2"))]
mod windows;

#[cfg(all(target_os = "windows", feature = "webview2"))]
mod foundation;

#[cfg(all(target_os = "windows", feature = "webview2"))]
pub use windows::{
    ControlledUrl, ControlledUrlError, HostEventError, LocalPage, LocalPageError, WebViewBounds,
    WebViewBoundsError, WebViewBuilder, WebViewError, WebViewHost, WebViewPage,
};

#[cfg(all(target_os = "windows", feature = "webview2"))]
pub use foundation::{
    AssetBundle, AssetBundleError, AssetLimits, AssetMime, AssetPath, AssetPathError,
    BinaryPayload, BridgeEndpoint, BridgeEnvelope, BridgeError, BridgeLimits, BridgeLimitsError,
    BridgeRouter, BridgeRouterError, EndpointDispatchError, EndpointName, EndpointNameError,
    LocalAsset, LocalAssetProtocol, LocalCsp, LocalProtocolResponse, LocalProtocolStatus,
    MessageId, MimePolicy, MimePolicyError, PageEvent, PageEventError, PageSessionId,
    PageSessionIdError, TypedEndpoint, WebSocketOrigin, WebSocketOriginError,
};
