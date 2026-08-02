use std::{borrow::Cow, cell::RefCell, error::Error, fmt, rc::Rc};

use url::Url;
use wry::{
    NewWindowResponse, Rect, WebView, WebViewBuilder as WryWebViewBuilder,
    WebViewBuilderExtWindows,
    dpi::{LogicalPosition, LogicalSize},
    http::{HeaderValue, Response, StatusCode, header},
};
use yuyib_platform::Window;

use crate::{
    AssetBundle, AssetMime, AssetPath, BridgeLimits, BridgeRouter, LocalAssetProtocol, LocalCsp,
    LocalProtocolResponse, PageEvent, PageEventError, PageSessionId, foundation::is_local_app_url,
};

const LOCAL_PROTOCOL_NAME: &str = "app";
const LOCAL_PROTOCOL_SOURCE_HOST: &str = "localhost";

/// A logical-pixel rectangle for a child `WebView`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WebViewBounds {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl WebViewBounds {
    /// Validates and creates a logical-pixel child rectangle.
    ///
    /// Negative origins are valid, while width and height must be finite and
    /// strictly positive.
    ///
    /// # Errors
    ///
    /// Returns an error for non-finite coordinates or an empty size.
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Result<Self, WebViewBoundsError> {
        if !x.is_finite() || !y.is_finite() || !width.is_finite() || !height.is_finite() {
            return Err(WebViewBoundsError::NonFinite);
        }
        if width <= 0.0 || height <= 0.0 {
            return Err(WebViewBoundsError::Empty);
        }
        Ok(Self {
            x,
            y,
            width,
            height,
        })
    }

    /// Returns the logical horizontal origin.
    #[must_use]
    pub const fn x(self) -> f64 {
        self.x
    }

    /// Returns the logical vertical origin.
    #[must_use]
    pub const fn y(self) -> f64 {
        self.y
    }

    /// Returns the logical width.
    #[must_use]
    pub const fn width(self) -> f64 {
        self.width
    }

    /// Returns the logical height.
    #[must_use]
    pub const fn height(self) -> f64 {
        self.height
    }

    fn from_window(window: &Window) -> Self {
        let logical = window
            .physical_size()
            .to_logical::<f64>(window.raw().scale_factor());
        Self {
            x: 0.0,
            y: 0.0,
            width: logical.width.max(1.0),
            height: logical.height.max(1.0),
        }
    }

    fn into_wry(self) -> Rect {
        Rect {
            position: LogicalPosition::new(self.x, self.y).into(),
            size: LogicalSize::new(self.width, self.height).into(),
        }
    }
}

/// An invalid child `WebView` rectangle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WebViewBoundsError {
    /// At least one coordinate was infinite or NaN.
    NonFinite,
    /// Width or height was zero or negative.
    Empty,
}

impl fmt::Display for WebViewBoundsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFinite => formatter.write_str("WebView bounds must be finite"),
            Self::Empty => formatter.write_str("WebView bounds need positive width and height"),
        }
    }
}

impl Error for WebViewBoundsError {}

/// An explicitly approved remote starting URL.
///
/// It accepts HTTPS only, rejects embedded credentials, and permits no later
/// navigation except the exact normalized initial URL.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControlledUrl(Url);

impl ControlledUrl {
    /// Parses one exact HTTPS URL that may be used as the initial page.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid URLs, non-HTTPS schemes, missing hosts, or
    /// embedded credentials.
    pub fn parse(value: impl AsRef<str>) -> Result<Self, ControlledUrlError> {
        let url = Url::parse(value.as_ref()).map_err(|_| ControlledUrlError::Invalid)?;
        if url.scheme() != "https" {
            return Err(ControlledUrlError::NotHttps);
        }
        if url.host_str().is_none() {
            return Err(ControlledUrlError::MissingHost);
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(ControlledUrlError::EmbeddedCredentials);
        }
        Ok(Self(url))
    }

    /// Returns the normalized, developer-approved URL.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub(crate) fn origin(&self) -> String {
        self.0.origin().ascii_serialization()
    }
}

/// A rejected controlled URL input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlledUrlError {
    /// The input was not an absolute URL.
    Invalid,
    /// The URL was not HTTPS.
    NotHttps,
    /// The URL did not contain a host.
    MissingHost,
    /// The URL contained a username or password.
    EmbeddedCredentials,
}

impl fmt::Display for ControlledUrlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid => formatter.write_str("controlled WebView URL is invalid"),
            Self::NotHttps => formatter.write_str("controlled WebView URL must use HTTPS"),
            Self::MissingHost => formatter.write_str("controlled WebView URL must include a host"),
            Self::EmbeddedCredentials => {
                formatter.write_str("controlled WebView URL must not include credentials")
            }
        }
    }
}

impl Error for ControlledUrlError {}

/// A local application page served exclusively from an in-memory asset bundle.
///
/// The entry asset must be HTML. Its resource requests are resolved by the
/// fixed Wry app protocol; no OS path, directory, or network lookup is
/// performed by this type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalPage {
    entry: AssetPath,
    protocol: LocalAssetProtocol,
}

impl LocalPage {
    /// Creates one local application page from a bounded asset bundle.
    ///
    /// # Errors
    ///
    /// Returns an error if the entry asset is absent or is not HTML.
    pub fn new(
        entry: AssetPath,
        assets: AssetBundle,
        csp: LocalCsp,
    ) -> Result<Self, LocalPageError> {
        let Some(asset) = assets.get(&entry) else {
            return Err(LocalPageError::MissingEntry);
        };
        if asset.mime() != AssetMime::Html {
            return Err(LocalPageError::EntryIsNotHtml);
        }
        Ok(Self {
            entry,
            protocol: LocalAssetProtocol::new(assets, csp),
        })
    }

    /// Returns the safe logical HTML entry path.
    #[must_use]
    pub const fn entry(&self) -> &AssetPath {
        &self.entry
    }

    fn source_url(&self) -> String {
        format!(
            "{LOCAL_PROTOCOL_NAME}://{LOCAL_PROTOCOL_SOURCE_HOST}/{}",
            self.entry.as_str()
        )
    }

    fn navigation_allowed(&self, candidate: &str) -> bool {
        let Ok(url) = Url::parse(candidate) else {
            return false;
        };
        let is_wry_local_origin = is_local_app_url(&url);
        let is_source_origin = url.scheme() == LOCAL_PROTOCOL_NAME
            && url
                .host_str()
                .is_some_and(|host| host.eq_ignore_ascii_case(LOCAL_PROTOCOL_SOURCE_HOST))
            && url.port().is_none()
            && url.username().is_empty()
            && url.password().is_none();
        (is_wry_local_origin || is_source_origin)
            && url.query().is_none()
            && url.path() == format!("/{}", self.entry.as_str())
    }
}

/// Invalid local application-page configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalPageError {
    /// The configured entry path was not in the provided asset bundle.
    MissingEntry,
    /// The configured entry asset was not served with the HTML MIME type.
    EntryIsNotHtml,
}

impl fmt::Display for LocalPageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingEntry => formatter.write_str("local WebView entry asset is missing"),
            Self::EntryIsNotHtml => formatter.write_str("local WebView entry asset must be HTML"),
        }
    }
}

impl Error for LocalPageError {}

/// Initial content for a `WebView` host.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WebViewPage {
    /// Developer-owned inline HTML.
    ///
    /// Windows `WebView2` limits inline HTML to 2 MiB and assigns it a null
    /// origin. Phase 2 will add a controlled local asset protocol.
    InlineHtml(String),
    /// One explicitly approved HTTPS page.
    ///
    /// The host blocks redirects, links, popups, and all other navigation.
    Remote(ControlledUrl),
    /// A fixed entry page resolved only from an in-memory local asset bundle.
    ///
    /// Local resource loads use the internal app protocol. Top-level
    /// navigation remains locked to this exact entry page so a configured
    /// page session cannot become stale.
    Local(LocalPage),
}

impl Default for WebViewPage {
    fn default() -> Self {
        Self::InlineHtml("<!doctype html><title>Yuyib</title>".to_owned())
    }
}

/// Configures one child `WebView` before the `WebView2` controller exists.
#[derive(Clone, Debug)]
pub struct WebViewBuilder {
    page: WebViewPage,
    bounds: Option<WebViewBounds>,
    visible: bool,
    transparent: bool,
    bridge_router: Option<Rc<RefCell<BridgeRouter>>>,
    bridge_failures: Option<Rc<RefCell<Vec<String>>>>,
    devtools: bool,
}

impl WebViewBuilder {
    /// Starts with a developer-owned inline page and no remote navigation.
    #[must_use]
    pub fn new() -> Self {
        Self {
            page: WebViewPage::default(),
            bounds: None,
            visible: true,
            transparent: false,
            bridge_router: None,
            bridge_failures: None,
            devtools: false,
        }
    }

    /// Sets developer-owned inline HTML.
    ///
    /// This replaces any controlled URL selected previously. Do not pass
    /// untrusted HTML.
    #[must_use]
    pub fn with_inline_html(mut self, html: impl Into<String>) -> Self {
        self.page = WebViewPage::InlineHtml(html.into());
        self
    }

    /// Selects one explicitly approved HTTPS starting page.
    ///
    /// This is an opt-in to remote content. All redirects and later navigation
    /// remain blocked.
    #[must_use]
    pub fn with_controlled_url(mut self, url: ControlledUrl) -> Self {
        self.page = WebViewPage::Remote(url);
        self
    }

    /// Selects a fixed local page served only by the provided memory bundle.
    ///
    /// This replaces inline or remote page content. It does not expose a
    /// filesystem resolver or permit later top-level navigation.
    #[must_use]
    pub fn with_local_page(mut self, page: LocalPage) -> Self {
        self.page = WebViewPage::Local(page);
        self
    }

    /// Registers an explicit bounded endpoint router for a local page.
    ///
    /// The router is rejected at build time unless the selected page is local.
    /// Wry IPC is one-way: errors are discarded at the browser boundary but
    /// remain structured when using `BridgeRouter` directly in tests or host code.
    #[must_use]
    pub fn with_bridge_router(mut self, router: BridgeRouter) -> Self {
        self.bridge_router = Some(Rc::new(RefCell::new(router)));
        self
    }

    /// Records IPC dispatch failures for the host to surface as diagnostics.
    #[must_use]
    pub fn with_bridge_failures(mut self, failures: Rc<RefCell<Vec<String>>>) -> Self {
        self.bridge_failures = Some(failures);
        self
    }

    /// Enables the WebView2 DevTools surface (F12 / context menu / open_devtools).
    #[must_use]
    pub const fn with_devtools(mut self, enabled: bool) -> Self {
        self.devtools = enabled;
        self
    }

    /// Sets the initial logical-pixel rectangle.
    ///
    /// If omitted, the current client area of the parent window is used.
    #[must_use]
    pub const fn with_bounds(mut self, bounds: WebViewBounds) -> Self {
        self.bounds = Some(bounds);
        self
    }

    /// Sets initial visibility.
    #[must_use]
    pub const fn with_visible(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    /// Makes the child `WebView` background transparent.
    ///
    /// This is useful for native renderer viewports placed beneath explicit
    /// transparent holes in a local UI. HTML content still owns input routing.
    #[must_use]
    pub const fn with_transparent(mut self, transparent: bool) -> Self {
        self.transparent = transparent;
        self
    }

    /// Creates the native child `WebView` on the window UI thread.
    ///
    /// # Errors
    ///
    /// Returns an error if `WebView2` or the native child controller cannot be
    /// created.
    ///
    /// # Panics
    ///
    /// Wry can panic for an invalid native window handle. Call this only after
    /// the platform window has been created and while its event loop owns it.
    pub fn build(self, window: &Window) -> Result<WebViewHost, WebViewError> {
        let bounds = self
            .bounds
            .unwrap_or_else(|| WebViewBounds::from_window(window));
        if self.bridge_router.is_some() && !matches!(self.page, WebViewPage::Local(_)) {
            return Err(WebViewError::BridgeRequiresLocalPage);
        }

        let base = WryWebViewBuilder::new()
            .with_visible(self.visible)
            .with_transparent(self.transparent)
            .with_devtools(self.devtools)
            .with_bounds(bounds.into_wry())
            .with_download_started_handler(|_, _| false)
            .with_new_window_req_handler(|_, _| NewWindowResponse::Deny);

        let (builder, event_binding) = match self.page {
            WebViewPage::InlineHtml(html) => (
                base.with_navigation_handler(|_| false).with_html(html),
                None,
            ),
            WebViewPage::Remote(url) => {
                let allowed_navigation = url.as_str().to_owned();
                (
                    base.with_navigation_handler(move |candidate| candidate == allowed_navigation)
                        .with_url(String::from(url.0)),
                    None,
                )
            }
            WebViewPage::Local(page) => {
                let source_url = page.source_url();
                let protocol = page.protocol.clone();
                let navigation_page = page.clone();
                let builder = base
                    .with_https_scheme(true)
                    .with_default_context_menus(false)
                    .with_browser_accelerator_keys(false)
                    .with_custom_protocol(LOCAL_PROTOCOL_NAME.to_owned(), move |_, request| {
                        protocol_response(&protocol.handle(
                            request.method().as_str(),
                            &request.uri().to_string(),
                            !request.body().is_empty(),
                        ))
                    })
                    .with_navigation_handler(move |candidate| {
                        navigation_page.navigation_allowed(&candidate)
                    });
                if let Some(router) = self.bridge_router {
                    let router_state = router.borrow();
                    let session_id = router_state.session();
                    let limits = router_state.limits();
                    let session = session_id.to_hex();
                    drop(router_state);
                    let callback_router = Rc::clone(&router);
                    let bridge_failures = self.bridge_failures.clone();
                    (
                        builder
                            .with_initialization_script(bridge_bootstrap(&session))
                            .with_ipc_handler(move |request| {
                                let origin = request.uri().to_string();
                                let result = match callback_router.try_borrow_mut() {
                                    Ok(mut router) => router
                                        .dispatch(&origin, request.body().as_bytes())
                                        .map_err(|error| error.to_string()),
                                    Err(_) => {
                                        Err("bridge router busy; dropped IPC message".to_owned())
                                    }
                                };
                                if let Err(error) = result {
                                    eprintln!(
                                        "yuyib-webview: bridge dispatch failed ({origin}): {error}"
                                    );
                                    if let Some(failures) = &bridge_failures
                                        && let Ok(mut shared) = failures.try_borrow_mut()
                                    {
                                        shared.push(format!("{origin}: {error}"));
                                        if shared.len() > 32 {
                                            let drain = shared.len() - 32;
                                            shared.drain(0..drain);
                                        }
                                    }
                                }
                            })
                            .with_url(source_url),
                        Some((session_id, limits)),
                    )
                } else {
                    (builder.with_url(source_url), None)
                }
            }
        };

        let webview = builder
            .build_as_child(window.raw())
            .map_err(WebViewError::Backend)?;
        Ok(WebViewHost {
            webview,
            event_binding,
            _window: window.clone(),
        })
    }
}

fn protocol_response(response: &LocalProtocolResponse) -> Response<Cow<'static, [u8]>> {
    let status =
        StatusCode::from_u16(response.status().code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut output = Response::new(Cow::Owned(response.bytes().to_vec()));
    *output.status_mut() = status;
    output.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if let Some(content_type) = response.content_type() {
        output
            .headers_mut()
            .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    }
    if let Ok(csp) = HeaderValue::from_str(response.csp_header()) {
        output
            .headers_mut()
            .insert(header::CONTENT_SECURITY_POLICY, csp);
    }
    output
}

fn bridge_bootstrap(session: &str) -> String {
    format!(
        "Object.defineProperty(window, 'yuyib', {{ value: Object.freeze({{ pageSession: '{session}', post(message) {{ window.ipc.postMessage(JSON.stringify({{ ...message, session: '{session}' }})); }} }}), configurable: false }});"
    )
}

const PAGE_EVENT_NAME: &str = "yuyib:event";

fn host_event_script(event: &PageEvent, limits: BridgeLimits) -> Result<String, PageEventError> {
    let json = event.to_json(limits)?;
    let json = String::from_utf8(json).map_err(|_| PageEventError::Serialization)?;
    let json_literal = serde_json::to_string(&json).map_err(|_| PageEventError::Serialization)?;
    Ok(format!(
        "window.dispatchEvent(new CustomEvent('{PAGE_EVENT_NAME}', {{ detail: JSON.parse({json_literal}) }}));"
    ))
}

fn bound_event_limits(
    binding: Option<(PageSessionId, BridgeLimits)>,
    event: &PageEvent,
) -> Result<BridgeLimits, HostEventError> {
    let Some((session, limits)) = binding else {
        return Err(HostEventError::NoLocalBridge);
    };
    if event.session() != session {
        return Err(HostEventError::StaleSession {
            expected: session,
            actual: event.session(),
        });
    }
    Ok(limits)
}

impl Default for WebViewBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// A live Windows `WebView2` child surface.
///
/// It is UI-thread-affine and an overlay surface, not a GPU texture or a
/// renderer layer.
pub struct WebViewHost {
    webview: WebView,
    event_binding: Option<(PageSessionId, BridgeLimits)>,
    _window: Window,
}

impl WebViewHost {
    /// Changes the child rectangle in logical pixels.
    ///
    /// # Errors
    ///
    /// Returns an error if the native controller rejects the update.
    pub fn set_bounds(&self, bounds: WebViewBounds) -> Result<(), WebViewError> {
        self.webview
            .set_bounds(bounds.into_wry())
            .map_err(WebViewError::Backend)
    }

    /// Shows or hides the native child surface.
    ///
    /// # Errors
    ///
    /// Returns an error if the native controller rejects the update.
    pub fn set_visible(&self, visible: bool) -> Result<(), WebViewError> {
        self.webview
            .set_visible(visible)
            .map_err(WebViewError::Backend)
    }

    /// Returns the page session accepted by local typed IPC and outbound events.
    ///
    /// A host has a session only when it was built from `LocalPage` with an
    /// explicit `BridgeRouter`.
    #[must_use]
    pub fn page_session(&self) -> Option<PageSessionId> {
        self.event_binding.map(|(session, _)| session)
    }

    /// Sends one validated event to the current local page.
    ///
    /// Wry exposes script evaluation rather than a native host-to-page message
    /// API. This method never interpolates payload text as JavaScript: it
    /// serializes the entire validated envelope to JSON, serializes that JSON
    /// once more as a JavaScript string literal, and dispatches a fixed
    /// `CustomEvent` named yuyib:event. Page code can subscribe with
    /// window.addEventListener.
    ///
    /// # Errors
    ///
    /// Returns a structured error when no local typed bridge exists, the event
    /// session is stale, validation exceeds bounds, or the native backend
    /// rejects the fixed dispatch script.
    pub fn emit_event(&self, event: &PageEvent) -> Result<(), HostEventError> {
        let limits = bound_event_limits(self.event_binding, event)?;
        let script = host_event_script(event, limits).map_err(HostEventError::Event)?;
        self.webview
            .evaluate_script(&script)
            .map_err(HostEventError::Backend)
    }

    /// Opens the native WebView DevTools window when built with `with_devtools(true)`.
    pub fn open_devtools(&self) {
        self.webview.open_devtools();
    }

    /// Evaluates static JavaScript supplied by the application developer.
    ///
    /// Never construct the script from user input, a URL, or an untrusted
    /// browser message. Use the bounded local typed router for page-to-host
    /// data instead.
    ///
    /// # Errors
    ///
    /// Returns an error if `WebView2` rejects script evaluation.
    pub fn evaluate_developer_script(&self, script: &str) -> Result<(), WebViewError> {
        self.webview
            .evaluate_script(script)
            .map_err(WebViewError::Backend)
    }
}

/// Failure while sending a bounded host-to-page event.
#[derive(Debug)]
pub enum HostEventError {
    /// Outbound events require a `LocalPage` with an explicit `BridgeRouter`.
    NoLocalBridge,
    /// The event belongs to an earlier or different local page session.
    StaleSession {
        /// Current host page session.
        expected: PageSessionId,
        /// Session claimed by the outgoing event.
        actual: PageSessionId,
    },
    /// The outbound event did not satisfy its bounded JSON contract.
    Event(PageEventError),
    /// Wry or `WebView2` rejected the fixed dispatch script.
    Backend(wry::Error),
}

impl fmt::Display for HostEventError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoLocalBridge => {
                formatter.write_str("host-to-page events require a local typed bridge")
            }
            Self::StaleSession { .. } => {
                formatter.write_str("host-to-page event used a stale page session")
            }
            Self::Event(error) => write!(formatter, "host-to-page event rejected: {error}"),
            Self::Backend(error) => write!(formatter, "WebView2 event dispatch failed: {error}"),
        }
    }
}

impl Error for HostEventError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Event(error) => Some(error),
            Self::Backend(error) => Some(error),
            _ => None,
        }
    }
}

/// Failure reported by the underlying Wry/WebView2 backend.
#[derive(Debug)]
pub enum WebViewError {
    /// A typed IPC router is allowed only for a local in-memory application page.
    BridgeRequiresLocalPage,
    /// The native `WebView2` backend returned an error.
    Backend(wry::Error),
}

impl fmt::Display for WebViewError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BridgeRequiresLocalPage => {
                formatter.write_str("a bridge router requires a local WebView page")
            }
            Self::Backend(error) => write!(formatter, "WebView2 backend error: {error}"),
        }
    }
}

impl Error for WebViewError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::BridgeRequiresLocalPage => None,
            Self::Backend(error) => Some(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controlled_url_is_https_without_credentials() {
        let url = ControlledUrl::parse("https://example.com/page").expect("valid HTTPS URL");
        assert_eq!(url.as_str(), "https://example.com/page");
        assert_eq!(
            ControlledUrl::parse("http://example.com").expect_err("HTTP denied"),
            ControlledUrlError::NotHttps
        );
        assert_eq!(
            ControlledUrl::parse("https://user@example.com").expect_err("credentials denied"),
            ControlledUrlError::EmbeddedCredentials
        );
    }

    #[test]
    fn bounds_require_a_visible_size() {
        assert!(matches!(
            WebViewBounds::new(0.0, 0.0, 0.0, 40.0),
            Err(WebViewBoundsError::Empty)
        ));
        assert!(matches!(
            WebViewBounds::new(f64::NAN, 0.0, 10.0, 10.0),
            Err(WebViewBoundsError::NonFinite)
        ));
        assert!(WebViewBounds::new(-2.0, 3.0, 20.0, 40.0).is_ok());
    }

    #[test]
    fn outbound_event_requires_current_session_and_json_only_bootstrap() {
        let session =
            crate::PageSessionId::parse("1234567890abcdef1234567890abcdef").expect("session");
        let stale =
            crate::PageSessionId::parse("1234567890abcdef1234567890abcdee").expect("stale session");
        let limits = crate::BridgeLimits::new(1, 512, 256, 32).expect("limits");
        let event = PageEvent::new(
            1,
            session,
            crate::EndpointName::parse("ui.notice").expect("event"),
            serde_json::json!({ "text": "quote: \" and </script>" }),
            limits,
        )
        .expect("outbound event");

        assert_eq!(
            bound_event_limits(Some((session, limits)), &event).expect("binding"),
            limits
        );
        let stale_event = PageEvent::new(
            1,
            stale,
            crate::EndpointName::parse("ui.notice").expect("event"),
            serde_json::json!({}),
            limits,
        )
        .expect("stale event");
        assert!(matches!(
            bound_event_limits(Some((session, limits)), &stale_event),
            Err(HostEventError::StaleSession { .. })
        ));

        let encoded = String::from_utf8(event.to_json(limits).expect("event JSON")).expect("UTF-8");
        let literal = serde_json::to_string(&encoded).expect("escaped JS string");
        let script = host_event_script(&event, limits).expect("fixed script");
        assert!(script.contains(&format!("JSON.parse({literal})")));
        assert!(script.starts_with("window.dispatchEvent(new CustomEvent('yuyib:event'"));
    }
}
