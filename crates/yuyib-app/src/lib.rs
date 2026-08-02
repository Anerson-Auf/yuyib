//! High-level native application facade for Yuyib.
//!
//! [`Application`] provides the simplest path to a Windows window and GPU
//! surface. Lower-level crates remain available for custom scheduling.

#![forbid(unsafe_code)]

#[cfg(feature = "webview")]
use std::{cell::RefCell, collections::VecDeque, rc::Rc};
use std::{error::Error, fmt};

use yuyib_core::{FrameInfo, Runtime};
#[cfg(feature = "ui")]
use yuyib_input::{WinitUiAdapter, WinitUiError, WinitUiUpdate};
#[cfg(feature = "webview")]
use yuyib_platform::winit::event_loop::EventLoopProxy;
use yuyib_platform::{
    CursorControl, CursorControlError, Window, WindowConfig,
    winit::{
        application::ApplicationHandler,
        error::{EventLoopError, OsError},
        event::{DeviceEvent, DeviceId, WindowEvent},
        event_loop::{ActiveEventLoop, EventLoop},
        window::WindowId,
    },
};
use yuyib_render::{
    ClearColor, ColorPostProcess, RenderFrame, RenderGraph, RenderGraphExecutionError,
    RenderStatus, Renderer, RendererInitError, SurfaceValidationError,
};
#[cfg(feature = "ui-text")]
use yuyib_ui::{
    Color as UiColor, ColorToken, LayoutWithMeasureError, UiMeasurer, Widget,
    layout_with_measurer_and_input_state,
};
#[cfg(feature = "ui")]
use yuyib_ui::{Size as UiSize, UiError, UiLayout, UiTokens, UiTree, layout_with_input_state};
#[cfg(feature = "ui")]
use yuyib_ui::{UiInputState, UiResponse};
#[cfg(feature = "ui")]
use yuyib_ui_render::{UiRectangle, UiRenderError, UiRenderLimits, UiRenderStats, UiRenderer};
#[cfg(feature = "ui-text")]
use yuyib_ui_text::{FontSource, TextEngine, TextError, TextLayoutOptions, TextLimits};
#[cfg(feature = "ui-text")]
use yuyib_ui_text_render::{
    GlyphAtlasConfig, GpuGlyphAtlas, TextColor, TextDrawList, TextGlyphDrawOptions,
    TextGlyphRenderLimits, TextGlyphRenderer, TextGpuRenderError, TextRasterizer, TextRenderError,
    TextViewport,
};
#[cfg(feature = "webview")]
use yuyib_webview::{
    HostEventError, PageEvent, PageSessionId, WebViewBounds, WebViewBoundsError, WebViewBuilder,
    WebViewError, WebViewHost,
};

/// Render scheduling policy for a high-level application.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RenderLoop {
    /// Draw only when the window requires it; best for normal native applications.
    #[default]
    OnDemand,
    /// Request a redraw after each event-loop wait; suited to games and live views.
    Continuous,
}

/// Context passed to a high-level frame callback.
pub struct FrameContext<'runtime> {
    frame: FrameInfo,
    runtime: &'runtime mut Runtime,
    cursor_control: &'runtime mut Option<CursorControl>,
}

impl FrameContext<'_> {
    /// Returns the timing snapshot of this frame.
    #[must_use]
    pub const fn frame(&self) -> FrameInfo {
        self.frame
    }

    /// Returns the shared runtime for lifecycle events and future resources.
    pub fn runtime(&mut self) -> &mut Runtime {
        self.runtime
    }

    /// Requests an orderly shutdown after the current callback returns.
    pub fn request_exit(&mut self) {
        self.runtime.request_exit();
    }

    /// Requests cursor behaviour after this frame callback returns.
    ///
    /// This is the frame-time counterpart of
    /// [`WindowEventContext::set_cursor_control`]. It is useful when a game
    /// changes mode asynchronously: for example, an initial loading screen
    /// can keep the cursor free, then lock and hide it in the same frame that
    /// a first-person scene becomes playable.
    pub fn set_cursor_control(&mut self, control: CursorControl) {
        *self.cursor_control = Some(control);
    }
}

type FrameCallback = Box<dyn FnMut(&mut FrameContext<'_>)>;
type RenderCallback = Box<dyn for<'frame> FnMut(&mut RenderFrame<'frame>)>;
type WindowEventCallback = Box<dyn FnMut(&WindowEvent, &mut WindowEventContext)>;
type DeviceEventCallback = Box<dyn FnMut(&DeviceEvent, &mut WindowEventContext)>;

/// Explicit control requests available while observing a native window event.
///
/// [`Application`] creates this context for each [`Application::on_window_event`]
/// callback. It intentionally exposes neither the Winit event loop nor mutable
/// window/GPU objects, so a callback cannot create a second loop, reconfigure a
/// surface, or present outside the application's render lifecycle.
#[derive(Default)]
pub struct WindowEventContext {
    exit_requested: bool,
    redraw_requested: bool,
    cursor_control: Option<CursorControl>,
}

impl WindowEventContext {
    /// Requests orderly event-loop exit after the event callback returns.
    pub fn request_exit(&mut self) {
        self.exit_requested = true;
    }

    /// Requests a later native redraw without exposing the window object.
    pub fn request_redraw(&mut self) {
        self.redraw_requested = true;
    }

    /// Requests cursor behaviour to be applied after this event callback.
    ///
    /// [`CursorControl::LockedHidden`] is useful for a first-person camera.
    /// The runtime attempts a true operating-system lock and safely falls back
    /// to confining the hidden cursor to the window. Call this again with
    /// [`CursorControl::Released`] before displaying normal UI.
    pub fn set_cursor_control(&mut self, control: CursorControl) {
        self.cursor_control = Some(control);
    }

    /// Returns whether this event callback requested exit.
    #[must_use]
    pub const fn exit_requested(&self) -> bool {
        self.exit_requested
    }

    /// Returns whether this event callback requested a later redraw.
    #[must_use]
    pub const fn redraw_requested(&self) -> bool {
        self.redraw_requested
    }

    fn requested_cursor_control(&self) -> Option<CursorControl> {
        self.cursor_control
    }
}

/// Optional child `WebView` configuration owned by [`Application`].
///
/// This type is available with the `webview` feature. It accepts a safe Yuyib
/// [`WebViewBuilder`], not `Wry`'s raw builder or controller. The high-level
/// lifecycle always places the child over the complete native client area and
/// updates that rectangle after resize and DPI-scale changes; use
/// `yuyib-webview` directly when an application needs a custom child rectangle.
#[cfg(feature = "webview")]
pub struct ApplicationWebView {
    builder: WebViewBuilder,
    visible: bool,
    events: Option<ApplicationWebViewEventQueue>,
}

#[cfg(feature = "webview")]
impl ApplicationWebView {
    /// Wraps a prepared page/bridge builder with a visible child by default.
    #[must_use]
    pub fn new(builder: WebViewBuilder) -> Self {
        Self {
            builder,
            visible: true,
            events: None,
        }
    }

    /// Sets the desired child visibility when the native window is not occluded.
    ///
    /// The host also hides a visible child while Windows reports the parent as
    /// occluded and restores this desired value when occlusion ends.
    #[must_use]
    pub fn with_visible(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    /// Adds a bounded UI-thread outbound-event queue and returns its safe handle.
    ///
    /// The returned [`ApplicationWebViewHandle`] deliberately uses `Rc`, so it is
    /// not `Send` or `Sync`; clone and use it only from application callbacks
    /// running on the native UI thread. Events are queued, never dispatched
    /// directly from a callback. A successful enqueue wakes the Winit event
    /// loop; multiple pending events coalesce onto one outstanding wake.
    /// [`Application`] drains FIFO events after `on_frame` returns and before it
    /// acquires the GPU frame for that redraw.
    ///
    /// The handle becomes ready only after the native child is created with a
    /// local page and a typed bridge. It rejects calls made earlier, after
    /// close, with a stale session, or when capacity events are pending.
    ///
    /// # Errors
    ///
    /// Returns an error for zero capacity or when this configuration already
    /// has an outbound-event queue.
    pub fn with_event_queue(
        mut self,
        capacity: usize,
    ) -> Result<(Self, ApplicationWebViewHandle), ApplicationWebViewQueueConfigError> {
        if capacity == 0 {
            return Err(ApplicationWebViewQueueConfigError::ZeroCapacity);
        }
        if self.events.is_some() {
            return Err(ApplicationWebViewQueueConfigError::AlreadyConfigured);
        }
        let events = ApplicationWebViewEventQueue::new(capacity);
        let handle = ApplicationWebViewHandle {
            events: events.clone(),
        };
        self.events = Some(events);
        Ok((self, handle))
    }

    fn build(self, window: &Window) -> Result<LiveApplicationWebView, ApplicationWebViewError> {
        let Self {
            builder,
            visible,
            events,
        } = self;
        let bounds =
            match webview_client_bounds(window.physical_size(), window.raw().scale_factor()) {
                Ok(bounds) => bounds,
                Err(error) => {
                    ApplicationWebViewEventQueue::close_optional(events.as_ref());
                    return Err(error);
                }
            };
        let host = builder
            .with_bounds(bounds)
            .with_visible(visible)
            .build(window)
            .map_err(ApplicationWebViewError::Host);
        let host = match host {
            Ok(host) => host,
            Err(error) => {
                ApplicationWebViewEventQueue::close_optional(events.as_ref());
                return Err(error);
            }
        };
        if let Some(events) = &events {
            match host.page_session() {
                Some(session) => events.ready(session),
                None => events.no_local_bridge(),
            }
        }
        Ok(LiveApplicationWebView {
            host,
            desired_visible: visible,
            events,
        })
    }

    fn close_events(&self) {
        ApplicationWebViewEventQueue::close_optional(self.events.as_ref());
    }

    fn install_event_loop_proxy(&self, proxy: EventLoopProxy<()>) {
        if let Some(events) = &self.events {
            events.install_event_loop_proxy(proxy);
        }
    }
}

#[cfg(feature = "webview")]
impl From<WebViewBuilder> for ApplicationWebView {
    /// Wraps a builder with the default visible child policy.
    fn from(builder: WebViewBuilder) -> Self {
        Self::new(builder)
    }
}

/// `WebView` lifecycle failure in a high-level [`Application`].
#[cfg(feature = "webview")]
#[derive(Debug)]
pub enum ApplicationWebViewError {
    /// Winit supplied a non-finite or non-positive native window scale factor.
    InvalidScaleFactor(f64),
    /// Current parent client dimensions could not produce valid child bounds.
    Bounds(WebViewBoundsError),
    /// `WebView2` or its native child controller rejected a lifecycle operation.
    Host(WebViewError),
    /// A bounded fixed-format event could not be dispatched to the local page.
    Event(HostEventError),
}

#[cfg(feature = "webview")]
impl fmt::Display for ApplicationWebViewError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidScaleFactor(scale_factor) => {
                write!(formatter, "invalid WebView scale factor {scale_factor}")
            }
            Self::Bounds(source) => {
                write!(formatter, "application WebView bounds failed: {source}")
            }
            Self::Host(source) => write!(formatter, "application WebView host failed: {source}"),
            Self::Event(source) => write!(formatter, "application WebView event failed: {source}"),
        }
    }
}

#[cfg(feature = "webview")]
impl Error for ApplicationWebViewError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidScaleFactor(_) => None,
            Self::Bounds(source) => Some(source),
            Self::Host(source) => Some(source),
            Self::Event(source) => Some(source),
        }
    }
}

/// Configuration error for an [`ApplicationWebView`] outbound-event queue.
#[cfg(feature = "webview")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplicationWebViewQueueConfigError {
    /// The queue cannot preserve a bounded delivery contract with zero slots.
    ZeroCapacity,
    /// This webview configuration already owns one event queue.
    AlreadyConfigured,
}

#[cfg(feature = "webview")]
impl fmt::Display for ApplicationWebViewQueueConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroCapacity => {
                formatter.write_str("WebView event queue capacity must be non-zero")
            }
            Self::AlreadyConfigured => {
                formatter.write_str("WebView configuration already has an event queue")
            }
        }
    }
}

#[cfg(feature = "webview")]
impl Error for ApplicationWebViewQueueConfigError {}

/// Non-blocking result of enqueueing an outbound [`PageEvent`].
///
/// This type never exposes `WebViewHost`, `Wry`, arbitrary script execution, or
/// a cross-thread synchronization primitive.
#[cfg(feature = "webview")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplicationWebViewCommandError {
    /// The native child has not yet established its current page session.
    NotReady,
    /// The configured page has no local typed bridge.
    NoLocalBridge,
    /// The application lifecycle has closed its native child.
    Closed,
    /// The bounded queue already contains its configured maximum event count.
    Full {
        /// Configured maximum number of pending events.
        capacity: usize,
    },
    /// The caller supplied an event for an earlier or different page session.
    StaleSession {
        /// Session accepted by the live local page.
        expected: PageSessionId,
        /// Session carried by the attempted event.
        actual: PageSessionId,
    },
}

#[cfg(feature = "webview")]
impl fmt::Display for ApplicationWebViewCommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotReady => formatter.write_str("WebView event queue is not ready"),
            Self::NoLocalBridge => {
                formatter.write_str("WebView event queue requires a local typed bridge")
            }
            Self::Closed => formatter.write_str("WebView event queue is closed"),
            Self::Full { capacity } => {
                write!(
                    formatter,
                    "WebView event queue is full (capacity {capacity})"
                )
            }
            Self::StaleSession { .. } => {
                formatter.write_str("WebView event belongs to a stale page session")
            }
        }
    }
}

#[cfg(feature = "webview")]
impl Error for ApplicationWebViewCommandError {}

/// Cloneable UI-thread handle for bounded host-to-page event commands.
///
/// Create it with [`ApplicationWebView::with_event_queue`] before
/// [`Application::run`]. The handle is intentionally neither `Send` nor `Sync`; an
/// application must hand work from background threads to its own UI-thread
/// scheduling boundary before enqueueing an event.
#[cfg(feature = "webview")]
#[derive(Clone, Debug)]
pub struct ApplicationWebViewHandle {
    events: ApplicationWebViewEventQueue,
}

#[cfg(feature = "webview")]
impl ApplicationWebViewHandle {
    /// Returns the session accepted by the live local typed bridge.
    ///
    /// # Errors
    ///
    /// Returns [`ApplicationWebViewCommandError::NotReady`] before the child is
    /// created, [`ApplicationWebViewCommandError::NoLocalBridge`] for a page
    /// without a bridge, or [`ApplicationWebViewCommandError::Closed`] after
    /// shutdown.
    pub fn page_session(&self) -> Result<PageSessionId, ApplicationWebViewCommandError> {
        self.events.page_session()
    }

    /// Enqueues one pre-validated, session-bound event without touching Wry.
    ///
    /// Use [`PageEvent::from_typed`] to serialize a Rust payload before calling
    /// this method. Enqueueing wakes the application event loop; at the next
    /// redraw, Yuyib drains events in FIFO order after `on_frame` and before GPU
    /// rendering. An event queued by `on_render` therefore waits for the
    /// automatically requested following redraw.
    ///
    /// # Errors
    ///
    /// Returns an explicit lifecycle, capacity, or stale-session error. A
    /// later native dispatch failure terminates [`Application::run`] with
    /// [`ApplicationError::WebView`].
    pub fn enqueue(&self, event: PageEvent) -> Result<(), ApplicationWebViewCommandError> {
        self.events.enqueue(event)
    }
}

#[cfg(feature = "webview")]
#[derive(Clone, Debug)]
struct ApplicationWebViewEventQueue {
    state: Rc<RefCell<ApplicationWebViewEventQueueState>>,
}

#[cfg(feature = "webview")]
#[derive(Debug)]
struct ApplicationWebViewEventQueueState {
    capacity: usize,
    lifecycle: ApplicationWebViewEventQueueLifecycle,
    pending: VecDeque<PageEvent>,
    event_loop_proxy: Option<EventLoopProxy<()>>,
    wake_requested: bool,
}

#[cfg(feature = "webview")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ApplicationWebViewEventQueueLifecycle {
    Pending,
    Ready(PageSessionId),
    NoLocalBridge,
    Closed,
}

#[cfg(feature = "webview")]
impl ApplicationWebViewEventQueue {
    fn new(capacity: usize) -> Self {
        Self {
            state: Rc::new(RefCell::new(ApplicationWebViewEventQueueState {
                capacity,
                lifecycle: ApplicationWebViewEventQueueLifecycle::Pending,
                pending: VecDeque::with_capacity(capacity),
                event_loop_proxy: None,
                wake_requested: false,
            })),
        }
    }

    fn install_event_loop_proxy(&self, proxy: EventLoopProxy<()>) {
        let mut state = self.state.borrow_mut();
        if !matches!(
            state.lifecycle,
            ApplicationWebViewEventQueueLifecycle::Closed
        ) {
            state.event_loop_proxy = Some(proxy);
        }
    }

    fn close_optional(queue: Option<&Self>) {
        if let Some(queue) = queue {
            queue.close();
        }
    }

    fn ready(&self, session: PageSessionId) {
        let mut state = self.state.borrow_mut();
        if !matches!(
            state.lifecycle,
            ApplicationWebViewEventQueueLifecycle::Closed
        ) {
            state.lifecycle = ApplicationWebViewEventQueueLifecycle::Ready(session);
        }
    }

    fn no_local_bridge(&self) {
        let mut state = self.state.borrow_mut();
        if !matches!(
            state.lifecycle,
            ApplicationWebViewEventQueueLifecycle::Closed
        ) {
            state.lifecycle = ApplicationWebViewEventQueueLifecycle::NoLocalBridge;
        }
    }

    fn close(&self) {
        let mut state = self.state.borrow_mut();
        state.lifecycle = ApplicationWebViewEventQueueLifecycle::Closed;
        state.pending.clear();
        state.event_loop_proxy = None;
        state.wake_requested = false;
    }

    fn page_session(&self) -> Result<PageSessionId, ApplicationWebViewCommandError> {
        match self.state.borrow().lifecycle {
            ApplicationWebViewEventQueueLifecycle::Pending => {
                Err(ApplicationWebViewCommandError::NotReady)
            }
            ApplicationWebViewEventQueueLifecycle::Ready(session) => Ok(session),
            ApplicationWebViewEventQueueLifecycle::NoLocalBridge => {
                Err(ApplicationWebViewCommandError::NoLocalBridge)
            }
            ApplicationWebViewEventQueueLifecycle::Closed => {
                Err(ApplicationWebViewCommandError::Closed)
            }
        }
    }

    fn enqueue(&self, event: PageEvent) -> Result<(), ApplicationWebViewCommandError> {
        let mut state = self.state.borrow_mut();
        match state.lifecycle {
            ApplicationWebViewEventQueueLifecycle::Pending => {
                return Err(ApplicationWebViewCommandError::NotReady);
            }
            ApplicationWebViewEventQueueLifecycle::NoLocalBridge => {
                return Err(ApplicationWebViewCommandError::NoLocalBridge);
            }
            ApplicationWebViewEventQueueLifecycle::Closed => {
                return Err(ApplicationWebViewCommandError::Closed);
            }
            ApplicationWebViewEventQueueLifecycle::Ready(session) => {
                if event.session() != session {
                    return Err(ApplicationWebViewCommandError::StaleSession {
                        expected: session,
                        actual: event.session(),
                    });
                }
            }
        }
        if state.pending.len() == state.capacity {
            return Err(ApplicationWebViewCommandError::Full {
                capacity: state.capacity,
            });
        }
        state.pending.push_back(event);
        if !state.wake_requested
            && let Some(proxy) = &state.event_loop_proxy
        {
            if proxy.send_event(()).is_err() {
                state.pending.clear();
                state.lifecycle = ApplicationWebViewEventQueueLifecycle::Closed;
                state.event_loop_proxy = None;
                return Err(ApplicationWebViewCommandError::Closed);
            }
            state.wake_requested = true;
        }
        Ok(())
    }

    fn dequeue(&self) -> Option<PageEvent> {
        let mut state = self.state.borrow_mut();
        if !matches!(
            state.lifecycle,
            ApplicationWebViewEventQueueLifecycle::Ready(_)
        ) {
            return None;
        }
        let event = state.pending.pop_front();
        if state.pending.is_empty() {
            state.wake_requested = false;
        }
        event
    }
}

#[cfg(feature = "webview")]
struct LiveApplicationWebView {
    host: WebViewHost,
    desired_visible: bool,
    events: Option<ApplicationWebViewEventQueue>,
}

#[cfg(feature = "webview")]
impl LiveApplicationWebView {
    fn resize(
        &self,
        physical_size: yuyib_platform::winit::dpi::PhysicalSize<u32>,
        scale_factor: f64,
    ) -> Result<(), ApplicationWebViewError> {
        self.host
            .set_bounds(webview_client_bounds(physical_size, scale_factor)?)
            .map_err(ApplicationWebViewError::Host)
    }

    fn set_occluded(&self, occluded: bool) -> Result<(), ApplicationWebViewError> {
        self.host
            .set_visible(self.desired_visible && !occluded)
            .map_err(ApplicationWebViewError::Host)
    }

    fn flush_events(&self) -> Result<(), ApplicationWebViewError> {
        let Some(events) = &self.events else {
            return Ok(());
        };
        while let Some(event) = events.dequeue() {
            self.host
                .emit_event(&event)
                .map_err(ApplicationWebViewError::Event)?;
        }
        Ok(())
    }

    fn close_events(&self) {
        ApplicationWebViewEventQueue::close_optional(self.events.as_ref());
    }
}

#[cfg(feature = "webview")]
fn webview_client_bounds(
    physical_size: yuyib_platform::winit::dpi::PhysicalSize<u32>,
    scale_factor: f64,
) -> Result<WebViewBounds, ApplicationWebViewError> {
    if !scale_factor.is_finite() || scale_factor <= 0.0 {
        return Err(ApplicationWebViewError::InvalidScaleFactor(scale_factor));
    }
    WebViewBounds::new(
        0.0,
        0.0,
        (f64::from(physical_size.width) / scale_factor).max(1.0),
        (f64::from(physical_size.height) / scale_factor).max(1.0),
    )
    .map_err(ApplicationWebViewError::Bounds)
}

/// Явная настройка нативного текста для [`ApplicationUi`].
///
/// Шрифт всегда задаёт приложение: bytes из его assets либо точный путь.
/// Высокоуровневый UI не сканирует системные шрифты, поэтому размер labels и
/// buttons одинаков на машинах с одинаковым пакетом приложения.
#[cfg(feature = "ui-text")]
#[derive(Clone, Debug)]
pub struct NativeUiTextConfig {
    source: FontSource,
    font_size: f32,
    line_height: f32,
    text_limits: TextLimits,
    atlas: GlyphAtlasConfig,
    gpu_limits: TextGlyphRenderLimits,
}

#[cfg(feature = "ui-text")]
impl NativeUiTextConfig {
    /// Создаёт настройки с выбранным приложением шрифтом и читаемыми значениями.
    #[must_use]
    pub fn new(source: FontSource) -> Self {
        Self {
            source,
            font_size: 16.0,
            line_height: 20.0,
            text_limits: TextLimits::default(),
            atlas: GlyphAtlasConfig::default(),
            gpu_limits: TextGlyphRenderLimits::default(),
        }
    }

    /// Задаёт размер шрифта и расстояние между базовыми линиями в логических пикселях.
    #[must_use]
    pub const fn with_metrics(mut self, font_size: f32, line_height: f32) -> Self {
        self.font_size = font_size;
        self.line_height = line_height;
        self
    }

    /// Заменяет лимиты CPU-подготовки и формирования текста.
    #[must_use]
    pub const fn with_text_limits(mut self, limits: TextLimits) -> Self {
        self.text_limits = limits;
        self
    }

    /// Заменяет лимиты постоянного атласа глифов.
    #[must_use]
    pub const fn with_atlas(mut self, atlas: GlyphAtlasConfig) -> Self {
        self.atlas = atlas;
        self
    }

    /// Заменяет лимиты загрузки в GPU и геометрии глифов.
    #[must_use]
    pub const fn with_gpu_limits(mut self, limits: TextGlyphRenderLimits) -> Self {
        self.gpu_limits = limits;
        self
    }
}

/// Ошибка при создании состояния [`NativeUiTextConfig`].
#[cfg(feature = "ui-text")]
#[derive(Debug)]
pub enum NativeUiTextInitError {
    /// Шрифт или лимиты его формирования недопустимы.
    Shape(TextError),
    /// Не удалось создать совместимый rasterizer либо атлас.
    Rasterizer(TextRenderError),
}

#[cfg(feature = "ui-text")]
impl fmt::Display for NativeUiTextInitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Shape(error) => write!(formatter, "native UI text shaping setup failed: {error}"),
            Self::Rasterizer(error) => {
                write!(formatter, "native UI text rasterizer setup failed: {error}")
            }
        }
    }
}

#[cfg(feature = "ui-text")]
impl Error for NativeUiTextInitError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Shape(error) => Some(error),
            Self::Rasterizer(error) => Some(error),
        }
    }
}

#[cfg(feature = "ui-text")]
#[derive(Debug)]
/// Ошибка измерения, rasterization или рисования нативного текста готового UI.
pub enum NativeUiTextError {
    /// Формирование текста отклонило подпись, шрифт или настройки разметки.
    Shape(TextError),
    /// Не удалась rasterization глифов либо вставка в атлас.
    Rasterize(TextRenderError),
    /// Не удалась ограниченная загрузка атласа либо GPU-проход текста.
    Gpu(TextGpuRenderError),
    /// Не удался проход прямоугольников, нужный для сохранения порядка виджетов.
    Ui(UiRenderError),
    /// Метрики текста не помещаются в размер UI в логических пикселях.
    MetricsOutsideUiRange,
}

#[cfg(feature = "ui-text")]
impl fmt::Display for NativeUiTextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Shape(error) => write!(formatter, "native UI text shaping failed: {error}"),
            Self::Rasterize(error) => {
                write!(formatter, "native UI text rasterization failed: {error}")
            }
            Self::Gpu(error) => write!(formatter, "native UI text GPU pass failed: {error}"),
            Self::Ui(error) => write!(formatter, "native UI rectangle pass failed: {error}"),
            Self::MetricsOutsideUiRange => {
                formatter.write_str("native UI text size is outside UI range")
            }
        }
    }
}

#[cfg(feature = "ui-text")]
impl Error for NativeUiTextError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Shape(error) => Some(error),
            Self::Rasterize(error) => Some(error),
            Self::Gpu(error) => Some(error),
            Self::Ui(error) => Some(error),
            Self::MetricsOutsideUiRange => None,
        }
    }
}

/// Optional retained native UI lifecycle owned by [`Application`].
///
/// This type is available with the `ui` feature. It owns an existing
/// [`UiTree`] and recomputes its [`UiLayout`] when the presentation size
/// changes. [`Application`] creates the WGPU backend only after it owns a
/// [`Renderer`], then renders this UI after the application's custom
/// [`Application::on_render`] callback so native UI is a foreground overlay.
///
/// With [`Self::with_winit_input`], it also owns a caller-configured
/// [`WinitUiAdapter`]. [`Application`] forwards native events into that adapter
/// and flushes ordered [`UiResponse`] values before `on_render` and this UI's
/// overlay pass. The adapter keeps its explicit [`yuyib_input::UiDpiPolicy`].
/// With the `ui-text` feature, [`Self::with_text`] adds labels and button
/// captions from one explicitly configured font. IME, accessibility, scrolling
/// and custom render ordering remain lower-level host responsibilities.
#[cfg(feature = "ui")]
pub struct ApplicationUi {
    tree: UiTree,
    tokens: UiTokens,
    limits: UiRenderLimits,
    layout: Option<(UiSize, UiLayout)>,
    renderer: Option<UiRenderer>,
    #[cfg(feature = "ui-text")]
    text: Option<ApplicationUiText>,
    input: Option<ApplicationUiInput>,
}

#[cfg(feature = "ui")]
type UiResponseCallback = Box<dyn FnMut(&UiResponse)>;

#[cfg(feature = "ui")]
struct ApplicationUiInput {
    adapter: WinitUiAdapter,
    state: yuyib_ui::UiInputState,
    on_response: UiResponseCallback,
    pending_error: Option<WinitUiError>,
}

/// Private bridge that keeps high-level UI text coherent with one explicit font.
#[cfg(feature = "ui-text")]
struct ApplicationUiText {
    config: NativeUiTextConfig,
    engine: TextEngine,
    rasterizer: TextRasterizer,
    glyph_renderer: Option<TextGlyphRenderer>,
    gpu_atlas: Option<GpuGlyphAtlas>,
}

#[cfg(feature = "ui-text")]
impl ApplicationUiText {
    fn new(config: NativeUiTextConfig) -> Result<Self, NativeUiTextInitError> {
        let engine = TextEngine::from_source(config.source.clone(), config.text_limits)
            .map_err(NativeUiTextInitError::Shape)?;
        let rasterizer = TextRasterizer::from_source(config.source.clone(), config.atlas)
            .map_err(NativeUiTextInitError::Rasterizer)?;
        Ok(Self {
            config,
            engine,
            rasterizer,
            glyph_renderer: None,
            gpu_atlas: None,
        })
    }

    fn layout_options(&self, widget: &Widget, available: UiSize) -> TextLayoutOptions {
        let horizontal_padding = widget
            .style()
            .padding
            .left
            .saturating_add(widget.style().padding.right);
        let width = available.width.saturating_sub(horizontal_padding);
        TextLayoutOptions {
            font_size: self.config.font_size,
            line_height: self.config.line_height,
            max_width: (width != 0).then(|| to_f32(width)),
            ..TextLayoutOptions::default()
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "One private frame operation keeps the UI renderer, layout, text and input state explicit"
    )]
    fn render(
        &mut self,
        frame: &mut RenderFrame<'_>,
        rectangles: &UiRenderer,
        root: &Widget,
        layout: &UiLayout,
        tokens: UiTokens,
        limits: UiRenderLimits,
        input: Option<&UiInputState>,
    ) -> Result<UiRenderStats, NativeUiTextError> {
        let stats = rectangles
            .draw(frame, root, layout, tokens, limits)
            .map_err(NativeUiTextError::Ui)?;
        let mut draw_list = TextDrawList::default();
        self.collect_text(root, layout, tokens, &mut draw_list)?;
        if draw_list.quads().is_empty() {
            return Ok(stats);
        }
        let glyph_renderer = self
            .glyph_renderer
            .get_or_insert_with(|| TextGlyphRenderer::new_for_frame(frame));
        if self.gpu_atlas.is_none() {
            self.gpu_atlas = Some(
                glyph_renderer
                    .upload_atlas(frame, self.rasterizer.atlas(), self.config.gpu_limits)
                    .map_err(NativeUiTextError::Gpu)?,
            );
        }
        let Some(gpu_atlas) = self.gpu_atlas.as_mut() else {
            return Ok(stats);
        };
        let viewport = TextViewport::new(0, 0, frame.surface_size()[0], frame.surface_size()[1]);
        let options = TextGlyphDrawOptions::new(viewport).with_limits(self.config.gpu_limits);
        glyph_renderer
            .update_atlas(
                frame,
                gpu_atlas,
                self.rasterizer.atlas(),
                self.config.gpu_limits,
            )
            .map_err(NativeUiTextError::Gpu)?;
        let _ = glyph_renderer
            .draw(frame, gpu_atlas, &draw_list, options)
            .map_err(NativeUiTextError::Gpu)?;
        let overlays = interaction_overlays(input, layout);
        if !overlays.is_empty() {
            let _ = rectangles
                .draw_rectangles(frame, &overlays)
                .map_err(NativeUiTextError::Ui)?;
        }
        Ok(stats)
    }

    fn collect_text(
        &mut self,
        widget: &Widget,
        layout: &UiLayout,
        tokens: UiTokens,
        draw_list: &mut TextDrawList,
    ) -> Result<(), NativeUiTextError> {
        let bounds = layout
            .bounds(widget.id())
            .ok_or(NativeUiTextError::MetricsOutsideUiRange)?;
        if let Some(content) = widget.text() {
            self.collect_widget_text(widget, bounds, tokens, content, draw_list)?;
        }
        for child in widget.children() {
            self.collect_text(child, layout, tokens, draw_list)?;
        }
        Ok(())
    }

    fn collect_widget_text(
        &mut self,
        widget: &Widget,
        bounds: yuyib_ui::Rect,
        tokens: UiTokens,
        content: &str,
        draw_list: &mut TextDrawList,
    ) -> Result<(), NativeUiTextError> {
        let padding = widget.style().padding;
        let inner = yuyib_ui::Rect {
            origin: yuyib_ui::Point::new(
                bounds
                    .origin
                    .x
                    .saturating_add(i32::try_from(padding.left).unwrap_or(i32::MAX)),
                bounds
                    .origin
                    .y
                    .saturating_add(i32::try_from(padding.top).unwrap_or(i32::MAX)),
            ),
            size: UiSize::new(
                bounds
                    .size
                    .width
                    .saturating_sub(padding.left.saturating_add(padding.right)),
                bounds
                    .size
                    .height
                    .saturating_sub(padding.top.saturating_add(padding.bottom)),
            ),
        };
        if inner.size.width == 0 || inner.size.height == 0 {
            return Ok(());
        }
        let shaped = self
            .engine
            .shape(content, self.layout_options(widget, bounds.size))
            .map_err(NativeUiTextError::Shape)?;
        let color = text_color(
            tokens
                .colors
                .resolve(widget.style().foreground.unwrap_or(ColorToken::Text)),
        );
        let text_frame = self
            .rasterizer
            .rasterize(&shaped, color)
            .map_err(NativeUiTextError::Rasterize)?;
        let translated = text_frame
            .draw_list()
            .translated(to_f32_i32(inner.origin.x), to_f32_i32(inner.origin.y));
        draw_list
            .append(&translated)
            .map_err(|_| NativeUiTextError::MetricsOutsideUiRange)?;
        Ok(())
    }
}

#[cfg(feature = "ui-text")]
impl UiMeasurer for ApplicationUiText {
    type Error = NativeUiTextError;

    fn measure(&mut self, widget: &Widget, available: UiSize) -> Result<UiSize, Self::Error> {
        let Some(content) = widget.text() else {
            return Ok(UiSize::default());
        };
        let shaped = self
            .engine
            .shape(content, self.layout_options(widget, available))
            .map_err(NativeUiTextError::Shape)?;
        Ok(UiSize::new(
            metric_to_ui(shaped.metrics().width)?,
            metric_to_ui(shaped.metrics().height)?,
        ))
    }
}

#[cfg(feature = "ui-text")]
fn text_color(color: UiColor) -> TextColor {
    TextColor {
        red: f32::from(color.red) / f32::from(u8::MAX),
        green: f32::from(color.green) / f32::from(u8::MAX),
        blue: f32::from(color.blue) / f32::from(u8::MAX),
        alpha: f32::from(color.alpha) / f32::from(u8::MAX),
    }
}

/// Builds the small high-level interaction layer after text has been drawn.
///
/// It intentionally uses translucent fills and a thin focus frame rather than
/// inventing a second button style system. Applications wanting a different
/// look can still consume `UiInputState` and render their own low-level list.
#[cfg(feature = "ui-text")]
fn interaction_overlays(input: Option<&UiInputState>, layout: &UiLayout) -> Vec<UiRectangle> {
    let Some(input) = input else {
        return Vec::new();
    };
    let mut overlays = Vec::new();
    for (widget, color) in [
        (
            input.hovered(),
            UiColor {
                red: u8::MAX,
                green: u8::MAX,
                blue: u8::MAX,
                alpha: 24,
            },
        ),
        (
            input.pressed(),
            UiColor {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 56,
            },
        ),
    ] {
        if let Some(widget) = widget
            && let Some(bounds) = layout.bounds(widget)
        {
            overlays.push(UiRectangle {
                widget,
                bounds,
                color,
                clip: None,
            });
        }
    }
    let Some(widget) = input.focused() else {
        return overlays;
    };
    let Some(bounds) = layout.bounds(widget) else {
        return overlays;
    };
    let thickness = 2_u32.min(bounds.size.width / 2).min(bounds.size.height / 2);
    if thickness == 0 {
        return overlays;
    }
    let color = UiColor::rgb(250, 204, 21);
    let right = bounds.origin.x.saturating_add(
        i32::try_from(bounds.size.width.saturating_sub(thickness)).unwrap_or(i32::MAX),
    );
    let bottom = bounds.origin.y.saturating_add(
        i32::try_from(bounds.size.height.saturating_sub(thickness)).unwrap_or(i32::MAX),
    );
    overlays.extend([
        UiRectangle {
            widget,
            bounds: yuyib_ui::Rect {
                origin: bounds.origin,
                size: UiSize::new(bounds.size.width, thickness),
            },
            color,
            clip: None,
        },
        UiRectangle {
            widget,
            bounds: yuyib_ui::Rect {
                origin: yuyib_ui::Point::new(bounds.origin.x, bottom),
                size: UiSize::new(bounds.size.width, thickness),
            },
            color,
            clip: None,
        },
        UiRectangle {
            widget,
            bounds: yuyib_ui::Rect {
                origin: bounds.origin,
                size: UiSize::new(thickness, bounds.size.height),
            },
            color,
            clip: None,
        },
        UiRectangle {
            widget,
            bounds: yuyib_ui::Rect {
                origin: yuyib_ui::Point::new(right, bounds.origin.y),
                size: UiSize::new(thickness, bounds.size.height),
            },
            color,
            clip: None,
        },
    ]);
    overlays
}

#[cfg(feature = "ui-text")]
fn metric_to_ui(value: f32) -> Result<u32, NativeUiTextError> {
    if !value.is_finite() || value < 0.0 || value > to_f32(u32::MAX) {
        return Err(NativeUiTextError::MetricsOutsideUiRange);
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "range is checked before converting the non-negative finite metric"
    )]
    Ok(value.ceil() as u32)
}

#[cfg(feature = "ui-text")]
#[allow(
    clippy::cast_precision_loss,
    reason = "UI coordinates are bounded by u32 pixels"
)]
fn to_f32(value: u32) -> f32 {
    value as f32
}

#[cfg(feature = "ui-text")]
#[allow(
    clippy::cast_precision_loss,
    reason = "UI coordinates are bounded by i32 pixels"
)]
fn to_f32_i32(value: i32) -> f32 {
    value as f32
}

#[cfg(feature = "ui-text")]
#[cfg(feature = "ui")]
impl ApplicationUi {
    /// Creates optional UI lifecycle state from a validated retained tree.
    #[must_use]
    pub fn new(tree: UiTree) -> Self {
        Self {
            tree,
            tokens: UiTokens::default(),
            limits: UiRenderLimits::default(),
            layout: None,
            renderer: None,
            #[cfg(feature = "ui-text")]
            text: None,
            input: None,
        }
    }

    /// Replaces resolved native UI colour and spacing tokens.
    #[must_use]
    pub const fn tokens(mut self, tokens: UiTokens) -> Self {
        self.tokens = tokens;
        self
    }

    /// Replaces the bounded rectangle-fill upload limit.
    #[must_use]
    pub const fn render_limits(mut self, limits: UiRenderLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Подключает готовое нативное рисование подписей и текста кнопок.
    ///
    /// Этот путь владеет формированием текста, атласом глифов и GPU-проходом,
    /// но шрифт приложение всё равно передаёт явно через
    /// [`NativeUiTextConfig`]. При первом кадре измеряются все подписи и
    /// кнопки с `Auto` размером.
    ///
    /// # Errors
    ///
    /// Возвращает ошибку, если из переданного шрифта нельзя создать
    /// совместимое состояние формирования текста и rasterizer.
    #[cfg(feature = "ui-text")]
    pub fn with_text(mut self, config: NativeUiTextConfig) -> Result<Self, NativeUiTextInitError> {
        self.text = Some(ApplicationUiText::new(config)?);
        Ok(self)
    }

    /// Integrates a caller-configured Winit UI adapter and response callback.
    ///
    /// [`Application`] routes each [`WindowEvent`] to `adapter` after the
    /// application's optional [`Application::on_window_event`] observer. At
    /// the next render frame, buffered input is applied in arrival order before
    /// `on_render`; each resulting [`UiResponse`] is synchronously passed to
    /// `on_response`, then this retained UI is rendered as the final overlay.
    ///
    /// Construct `adapter` with the [`yuyib_input::UiDpiPolicy`] matching this
    /// UI's layout viewport. The callback intentionally receives only a shared
    /// response reference: it cannot borrow window or GPU internals.
    #[must_use]
    pub fn with_winit_input(
        mut self,
        adapter: WinitUiAdapter,
        on_response: impl FnMut(&UiResponse) + 'static,
    ) -> Self {
        self.input = Some(ApplicationUiInput {
            adapter,
            state: yuyib_ui::UiInputState::default(),
            on_response: Box::new(on_response),
            pending_error: None,
        });
        self
    }

    /// Returns the retained UI tree.
    #[must_use]
    pub const fn tree(&self) -> &UiTree {
        &self.tree
    }

    /// Returns the current cached layout, if one has been requested.
    #[must_use]
    pub fn cached_layout(&self) -> Option<&UiLayout> {
        self.layout.as_ref().map(|(_, layout)| layout)
    }

    /// Computes or reuses a layout for an explicit presentation size.
    ///
    /// This CPU-only method is useful for hosts that want to inspect layout
    /// before a native application starts.
    ///
    /// # Errors
    ///
    /// Returns [`ApplicationUiError`] when the retained layout constraints are
    /// invalid.
    pub fn layout_for(&mut self, size: UiSize) -> Result<&UiLayout, ApplicationUiError> {
        let should_rebuild = self.input.is_some()
            || self
                .layout
                .as_ref()
                .is_none_or(|(cached, _)| *cached != size);
        if should_rebuild {
            let default_input = yuyib_ui::UiInputState::default();
            let input_state = self
                .input
                .as_ref()
                .map_or(&default_input, |input| &input.state);
            let layout = {
                #[cfg(feature = "ui-text")]
                if let Some(text) = &mut self.text {
                    layout_with_measurer_and_input_state(&self.tree, size, text, input_state)
                        .map_err(ApplicationUiError::TextLayout)?
                } else {
                    layout_with_input_state(&self.tree, size, input_state)
                        .map_err(ApplicationUiError::Layout)?
                }
                #[cfg(not(feature = "ui-text"))]
                layout_with_input_state(&self.tree, size, input_state)
                    .map_err(ApplicationUiError::Layout)?
            };
            self.layout = Some((size, layout));
        }
        self.layout
            .as_ref()
            .map(|(_, layout)| layout)
            .ok_or(ApplicationUiError::LayoutCacheUnavailable)
    }

    fn initialize(&mut self, renderer: &Renderer) {
        self.renderer = Some(UiRenderer::new(renderer));
    }

    fn handle_window_event(&mut self, event: &WindowEvent) -> bool {
        let Some(input) = &mut self.input else {
            return false;
        };
        match input.adapter.handle_window_event(event) {
            Ok(WinitUiUpdate::Buffered | WinitUiUpdate::FocusLost) => true,
            Ok(WinitUiUpdate::Ignored | WinitUiUpdate::ModifiersChanged) => false,
            Err(error) => {
                input.pending_error = Some(error);
                true
            }
        }
    }

    fn emit_input(&mut self, size: UiSize) -> Result<(), ApplicationUiError> {
        if let Some(input) = &mut self.input
            && let Some(error) = input.pending_error.take()
        {
            return Err(ApplicationUiError::Input(error));
        }
        self.layout_for(size)?;
        let Self {
            tree,
            layout,
            input,
            ..
        } = self;
        let Some(input) = input else {
            return Ok(());
        };
        let layout = layout
            .as_ref()
            .map(|(_, layout)| layout)
            .ok_or(ApplicationUiError::LayoutCacheUnavailable)?;
        let responses = input
            .adapter
            .emit_frame(tree, layout, &mut input.state)
            .map_err(ApplicationUiError::Input)?;
        for response in &responses {
            (input.on_response)(response);
        }
        Ok(())
    }

    fn render(&mut self, frame: &mut RenderFrame<'_>) -> Result<UiRenderStats, ApplicationUiError> {
        let [width, height] = frame.surface_size();
        self.layout_for(UiSize::new(width, height))?;
        let layout = self
            .cached_layout()
            .ok_or(ApplicationUiError::LayoutCacheUnavailable)?
            .clone();
        let renderer = self
            .renderer
            .as_ref()
            .ok_or(ApplicationUiError::NotInitialized)?;
        #[cfg(feature = "ui-text")]
        let input = self.input.as_ref().map(|value| &value.state);
        #[cfg(feature = "ui-text")]
        if let Some(text) = &mut self.text {
            return text
                .render(
                    frame,
                    renderer,
                    self.tree.root(),
                    &layout,
                    self.tokens,
                    self.limits,
                    input,
                )
                .map_err(ApplicationUiError::TextRender);
        }
        renderer
            .draw(frame, self.tree.root(), &layout, self.tokens, self.limits)
            .map_err(ApplicationUiError::Render)
    }
}

/// Failure while an [`ApplicationUi`] lifecycle phase is active.
#[cfg(feature = "ui")]
#[derive(Debug)]
pub enum ApplicationUiError {
    /// Render lifecycle ran before the application created its WGPU backend.
    NotInitialized,
    /// Retained UI layout failed.
    Layout(UiError),
    /// Text-aware intrinsic measurement failed.
    #[cfg(feature = "ui-text")]
    TextLayout(LayoutWithMeasureError<NativeUiTextError>),
    /// Layout completed without producing the required cache entry.
    ///
    /// This signals an internal lifecycle invariant failure and is retained as
    /// an error instead of allowing the high-level API to panic.
    LayoutCacheUnavailable,
    /// Winit event conversion or retained UI input dispatch failed.
    Input(WinitUiError),
    /// Rectangle extraction, upload, or WGPU recording failed.
    Render(UiRenderError),
    /// Native text rasterization or its ordered UI pass failed.
    #[cfg(feature = "ui-text")]
    TextRender(NativeUiTextError),
}

#[cfg(feature = "ui")]
impl fmt::Display for ApplicationUiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotInitialized => {
                formatter.write_str("application UI renderer is not initialized")
            }
            Self::Layout(source) => write!(formatter, "application UI layout failed: {source}"),
            #[cfg(feature = "ui-text")]
            Self::TextLayout(source) => {
                write!(formatter, "application UI text layout failed: {source}")
            }
            Self::LayoutCacheUnavailable => {
                formatter.write_str("application UI layout cache is unavailable")
            }
            Self::Input(source) => write!(formatter, "application UI input failed: {source}"),
            Self::Render(source) => write!(formatter, "application UI render failed: {source}"),
            #[cfg(feature = "ui-text")]
            Self::TextRender(source) => {
                write!(formatter, "application UI text render failed: {source}")
            }
        }
    }
}

#[cfg(feature = "ui")]
impl Error for ApplicationUiError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Layout(source) => Some(source),
            #[cfg(feature = "ui-text")]
            Self::TextLayout(source) => Some(source),
            Self::Input(source) => Some(source),
            Self::Render(source) => Some(source),
            #[cfg(feature = "ui-text")]
            Self::TextRender(source) => Some(source),
            Self::NotInitialized | Self::LayoutCacheUnavailable => None,
        }
    }
}

/// High-level configuration and entry point for a native Windows application.
pub struct Application {
    window: WindowConfig,
    clear_color: ClearColor,
    color_post_process: Option<ColorPostProcess>,
    render_loop: RenderLoop,
    cursor_control: CursorControl,
    on_frame: Option<FrameCallback>,
    on_window_event: Option<WindowEventCallback>,
    on_device_event: Option<DeviceEventCallback>,
    render_graph: Option<RenderGraph>,
    on_render: Option<RenderCallback>,
    #[cfg(feature = "webview")]
    webview: Option<ApplicationWebView>,
    #[cfg(feature = "ui")]
    ui: Option<ApplicationUi>,
}

impl Application {
    /// Creates a 1280×720 resizable application with an on-demand render loop.
    #[must_use]
    pub fn new() -> Self {
        Self {
            window: WindowConfig::default(),
            clear_color: ClearColor::default(),
            color_post_process: None,
            render_loop: RenderLoop::default(),
            cursor_control: CursorControl::Released,
            on_frame: None,
            on_window_event: None,
            on_device_event: None,
            render_graph: None,
            on_render: None,
            #[cfg(feature = "webview")]
            webview: None,
            #[cfg(feature = "ui")]
            ui: None,
        }
    }

    /// Replaces the native window configuration.
    #[must_use]
    pub fn window(mut self, config: WindowConfig) -> Self {
        self.window = config;
        self
    }

    /// Sets the color written by the foundation render pass.
    #[must_use]
    pub fn clear_color(mut self, color: ClearColor) -> Self {
        self.clear_color = color;
        self
    }

    /// Enables renderer-owned HDR exposure and display tone mapping.
    ///
    /// This is opt-in so existing applications retain byte-for-byte rendering
    /// policy. Pass [`ColorPostProcess::filmic`] for the recommended real-time
    /// 3D starting point.
    #[must_use]
    pub fn color_post_process(mut self, config: ColorPostProcess) -> Self {
        self.color_post_process = Some(config);
        self
    }

    /// Selects on-demand or continuous rendering.
    #[must_use]
    pub fn render_loop(mut self, render_loop: RenderLoop) -> Self {
        self.render_loop = render_loop;
        self
    }

    /// Sets the initial cursor behaviour after the native window is created.
    ///
    /// This is normally paired with a 3D controller. Runtime changes remain
    /// available through [`WindowEventContext::set_cursor_control`] in either
    /// native-event callback.
    #[must_use]
    pub fn cursor_control(mut self, control: CursorControl) -> Self {
        self.cursor_control = control;
        self
    }

    /// Registers one callback that runs before each foundation render pass.
    #[must_use]
    pub fn on_frame(mut self, callback: impl FnMut(&mut FrameContext<'_>) + 'static) -> Self {
        self.on_frame = Some(Box::new(callback));
        self
    }

    /// Registers one native-event observer that runs before built-in handling.
    ///
    /// The callback receives the original [`WindowEvent`] by shared reference
    /// and a restricted [`WindowEventContext`]. It may buffer input such as
    /// `WinitUiAdapter::handle_window_event`, request a redraw,
    /// or request exit, but cannot access a mutable window, renderer, surface,
    /// or Winit event loop. If it requests exit, built-in processing for that
    /// event is skipped and the application exits after the callback returns.
    /// Otherwise close, resize, redraw, `on_frame`, and `on_render` behavior is
    /// unchanged. A redraw request received with `CloseRequested` is ignored.
    #[must_use]
    pub fn on_window_event(
        mut self,
        callback: impl FnMut(&WindowEvent, &mut WindowEventContext) + 'static,
    ) -> Self {
        self.on_window_event = Some(Box::new(callback));
        self
    }

    /// Registers a low-level native device-event observer.
    ///
    /// Unlike [`Self::on_window_event`], this receives relative mouse motion
    /// while a cursor is locked, including motion that has no useful absolute
    /// client position. It is intended for cameras, raw input and similar
    /// game controls. The callback can request cursor changes, redraw or exit
    /// through the same restricted [`WindowEventContext`].
    #[must_use]
    pub fn on_device_event(
        mut self,
        callback: impl FnMut(&DeviceEvent, &mut WindowEventContext) + 'static,
    ) -> Self {
        self.on_device_event = Some(Box::new(callback));
        self
    }

    /// Registers one GPU callback that runs after the foundation clear pass.
    ///
    /// The callback receives the exclusive [`RenderFrame`] for the current
    /// presentation texture. It can record one or more WGPU passes through
    /// [`RenderFrame::with_surface_pass`]; the first custom pass normally uses
    /// [`yuyib_render::wgpu::LoadOp::Load`] to compose over the configured
    /// clear color. The renderer retains texture acquisition, submission and
    /// presentation, so a callback cannot accidentally present twice or retain
    /// a surface texture beyond this frame.
    ///
    /// `on_frame` is still invoked first and retains its lifecycle-only
    /// semantics. If it requests exit, no GPU frame is acquired and this
    /// callback does not run.
    #[must_use]
    pub fn on_render(
        mut self,
        callback: impl for<'frame> FnMut(&mut RenderFrame<'frame>) + 'static,
    ) -> Self {
        self.on_render = Some(Box::new(callback));
        self
    }

    /// Installs a declared render graph before the low-level render callback.
    ///
    /// The graph executes after the foundation clear and before `on_render` and
    /// native UI. Standard phases, dependencies, resource access and per-pass
    /// CPU recording time remain observable through the graph API.
    #[must_use]
    pub fn render_graph(mut self, graph: RenderGraph) -> Self {
        self.render_graph = Some(graph);
        self
    }

    /// Registers one optional `WebView2` child for the application's client area.
    ///
    /// Available with the `webview` feature. `builder` can be a prepared
    /// [`WebViewBuilder`] or [`ApplicationWebView`]. The host is created only
    /// during `resumed`, on the thread that created the native window. Its
    /// child bounds follow the complete client area through resize and DPI
    /// changes, and it hides while the parent is occluded.
    ///
    /// This does not relax `yuyib-webview` page, navigation, IPC, or script
    /// policies. The child is a native overlay rather than a GPU render layer,
    /// so it visually composes above `on_render` and optional retained UI.
    /// When configured through
    /// [`ApplicationWebView::with_event_queue`], bounded page events queued by
    /// a UI-thread callback wake the event loop automatically and are dispatched
    /// in FIFO order before the next redraw's GPU frame.
    #[cfg(feature = "webview")]
    #[must_use]
    pub fn webview(mut self, builder: impl Into<ApplicationWebView>) -> Self {
        self.webview = Some(builder.into());
        self
    }

    /// Registers optional retained native UI as the final foreground phase.
    ///
    /// Available with the `ui` Cargo feature. During each successfully
    /// acquired frame, `on_render` runs first and this UI pass runs second with
    /// the same exclusive [`RenderFrame`]. This explicit phase ordering avoids
    /// hidden window or renderer ownership while ensuring UI overlays game and
    /// application rendering.
    #[cfg(feature = "ui")]
    #[must_use]
    pub fn ui(mut self, ui: ApplicationUi) -> Self {
        self.ui = Some(ui);
        self
    }

    /// Creates the window, initialises the GPU surface and enters the event loop.
    ///
    /// # Errors
    ///
    /// Returns [`ApplicationError`] if Winit, native window creation, WGPU
    /// initialisation, surface validation or surface lifetime fails.
    pub fn run(self) -> Result<(), ApplicationError> {
        let event_loop = EventLoop::new().map_err(ApplicationError::EventLoop)?;
        #[cfg(feature = "webview")]
        if let Some(webview) = &self.webview {
            webview.install_event_loop_proxy(event_loop.create_proxy());
        }
        let mut host = ApplicationHost::new(self);
        let event_loop_result = event_loop.run_app(&mut host);
        #[cfg(feature = "webview")]
        host.close_webview();
        event_loop_result.map_err(ApplicationError::EventLoop)?;
        host.failure.map_or(Ok(()), Err)
    }
}

impl Default for Application {
    fn default() -> Self {
        Self::new()
    }
}

/// Failure that prevents a high-level application from running correctly.
#[derive(Debug)]
#[non_exhaustive]
pub enum ApplicationError {
    /// Winit failed to initialise or drive the native event loop.
    EventLoop(EventLoopError),
    /// The native window could not be created.
    Window(OsError),
    /// The GPU surface, adapter or device could not be initialised.
    Renderer(RendererInitError),
    /// WGPU rejected the active surface configuration.
    SurfaceValidation(SurfaceValidationError),
    /// A requested cursor grab or release was rejected by the platform.
    CursorControl(CursorControlError),
    /// A declared render-graph pass failed while recording the frame.
    RenderGraph(RenderGraphExecutionError),
    /// The WGPU surface was lost; automatic device recovery is not available yet.
    SurfaceLost,
    /// Optional `WebView2` child creation, resize, or visibility update failed.
    #[cfg(feature = "webview")]
    WebView(ApplicationWebViewError),
    /// Optional retained native UI failed during layout, upload, or recording.
    #[cfg(feature = "ui")]
    Ui(ApplicationUiError),
}

impl fmt::Display for ApplicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EventLoop(error) => write!(formatter, "native event-loop failure: {error}"),
            Self::Window(error) => write!(formatter, "native window creation failed: {error}"),
            Self::Renderer(error) => write!(formatter, "renderer initialisation failed: {error}"),
            Self::SurfaceValidation(error) => {
                write!(formatter, "surface validation failed: {error}")
            }
            Self::CursorControl(error) => write!(formatter, "cursor control failed: {error}"),
            Self::RenderGraph(error) => write!(formatter, "render graph failed: {error}"),
            Self::SurfaceLost => formatter.write_str("GPU surface was lost"),
            #[cfg(feature = "webview")]
            Self::WebView(error) => write!(formatter, "WebView lifecycle failed: {error}"),
            #[cfg(feature = "ui")]
            Self::Ui(error) => write!(formatter, "native UI lifecycle failed: {error}"),
        }
    }
}

impl Error for ApplicationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::EventLoop(error) => Some(error),
            Self::Window(error) => Some(error),
            Self::Renderer(error) => Some(error),
            Self::SurfaceValidation(error) => Some(error),
            Self::CursorControl(error) => Some(error),
            Self::RenderGraph(error) => Some(error),
            Self::SurfaceLost => None,
            #[cfg(feature = "webview")]
            Self::WebView(error) => Some(error),
            #[cfg(feature = "ui")]
            Self::Ui(error) => Some(error),
        }
    }
}

struct ApplicationHost {
    application: Application,
    runtime: Runtime,
    // Declared before `window` so unexpected host drop tears down WebView2 first.
    #[cfg(feature = "webview")]
    webview: Option<LiveApplicationWebView>,
    window: Option<Window>,
    renderer: Option<Renderer>,
    failure: Option<ApplicationError>,
}

impl ApplicationHost {
    fn new(application: Application) -> Self {
        Self {
            application,
            runtime: Runtime::new(),
            #[cfg(feature = "webview")]
            webview: None,
            window: None,
            renderer: None,
            failure: None,
        }
    }

    fn fail(&mut self, event_loop: &ActiveEventLoop, error: ApplicationError) {
        #[cfg(feature = "webview")]
        {
            self.close_webview();
        }
        self.failure = Some(error);
        event_loop.exit();
    }

    #[cfg(feature = "webview")]
    fn close_webview(&mut self) {
        if let Some(webview) = self.webview.take() {
            webview.close_events();
        }
        if let Some(configuration) = &self.application.webview {
            configuration.close_events();
        }
    }

    fn dispatch_window_event_hook(&mut self, event: &WindowEvent) -> WindowEventContext {
        let mut context = WindowEventContext::default();
        if let Some(callback) = &mut self.application.on_window_event {
            callback(event, &mut context);
        }
        context
    }

    fn dispatch_device_event_hook(&mut self, event: &DeviceEvent) -> WindowEventContext {
        let mut context = WindowEventContext::default();
        if let Some(callback) = &mut self.application.on_device_event {
            callback(event, &mut context);
        }
        context
    }

    fn apply_cursor_request(
        &mut self,
        event_loop: &ActiveEventLoop,
        context: &WindowEventContext,
    ) -> bool {
        self.apply_cursor_control(event_loop, context.requested_cursor_control())
    }

    fn apply_cursor_control(
        &mut self,
        event_loop: &ActiveEventLoop,
        control: Option<CursorControl>,
    ) -> bool {
        let Some(control) = control else {
            return true;
        };
        let Some(window) = &self.window else {
            return true;
        };
        if let Err(error) = window.set_cursor_control(control) {
            self.fail(event_loop, ApplicationError::CursorControl(error));
            return false;
        }
        true
    }
}

impl ApplicationHandler for ApplicationHost {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() || self.failure.is_some() {
            return;
        }
        let window = match Window::create(event_loop, &self.application.window) {
            Ok(window) => window,
            Err(error) => {
                self.fail(event_loop, ApplicationError::Window(error));
                return;
            }
        };
        let mut renderer = match Renderer::new(&window) {
            Ok(renderer) => renderer,
            Err(error) => {
                self.fail(event_loop, ApplicationError::Renderer(error));
                return;
            }
        };
        renderer.set_color_post_process(self.application.color_post_process);

        if let Err(error) = window.set_cursor_control(self.application.cursor_control) {
            self.fail(event_loop, ApplicationError::CursorControl(error));
            return;
        }

        #[cfg(feature = "webview")]
        let webview = match self.application.webview.take() {
            Some(configuration) => match configuration.build(&window) {
                Ok(webview) => Some(webview),
                Err(error) => {
                    self.fail(event_loop, ApplicationError::WebView(error));
                    return;
                }
            },
            None => None,
        };

        #[cfg(feature = "ui")]
        if let Some(ui) = &mut self.application.ui {
            ui.initialize(&renderer);
        }

        window.request_redraw();
        #[cfg(feature = "webview")]
        {
            self.webview = webview;
        }
        self.window = Some(window);
        self.renderer = Some(renderer);
    }

    #[allow(clippy::too_many_lines)] // Event lifecycle branches remain adjacent for ordering review.
    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        let context = self.dispatch_window_event_hook(&event);
        if !self.apply_cursor_request(event_loop, &context) {
            return;
        }
        if context.exit_requested() {
            #[cfg(feature = "webview")]
            self.close_webview();
            event_loop.exit();
            return;
        }
        #[cfg(feature = "ui")]
        let context = {
            let mut context = context;
            if let Some(ui) = &mut self.application.ui
                && ui.handle_window_event(&event)
            {
                context.request_redraw();
            }
            context
        };
        let close_requested = matches!(event, WindowEvent::CloseRequested);
        match event {
            WindowEvent::CloseRequested => {
                #[cfg(feature = "webview")]
                self.close_webview();
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                #[cfg(feature = "webview")]
                {
                    let webview_result = self.webview.as_ref().map(|webview| {
                        webview.resize(
                            size,
                            self.window
                                .as_ref()
                                .map_or(1.0, |window| window.raw().scale_factor()),
                        )
                    });
                    if let Some(Err(error)) = webview_result {
                        self.fail(event_loop, ApplicationError::WebView(error));
                        return;
                    }
                }
                if let Some(renderer) = &mut self.renderer {
                    renderer.resize(size.width, size.height);
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                #[cfg(not(feature = "webview"))]
                let _ = scale_factor;
                #[cfg(feature = "webview")]
                {
                    let webview_result =
                        self.webview
                            .as_ref()
                            .zip(self.window.as_ref())
                            .map(|(webview, window)| {
                                webview.resize(window.physical_size(), scale_factor)
                            });
                    if let Some(Err(error)) = webview_result {
                        self.fail(event_loop, ApplicationError::WebView(error));
                        return;
                    }
                }
            }
            WindowEvent::Occluded(occluded) => {
                #[cfg(not(feature = "webview"))]
                let _ = occluded;
                #[cfg(feature = "webview")]
                {
                    let webview_result = self
                        .webview
                        .as_ref()
                        .map(|webview| webview.set_occluded(occluded));
                    if let Some(Err(error)) = webview_result {
                        self.fail(event_loop, ApplicationError::WebView(error));
                        return;
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                let frame = self.runtime.begin_frame();
                let mut frame_cursor_control = None;
                if let Some(callback) = &mut self.application.on_frame {
                    callback(&mut FrameContext {
                        frame,
                        runtime: &mut self.runtime,
                        cursor_control: &mut frame_cursor_control,
                    });
                }
                if !self.apply_cursor_control(event_loop, frame_cursor_control) {
                    return;
                }
                if self.runtime.exit_requested() {
                    #[cfg(feature = "webview")]
                    self.close_webview();
                    event_loop.exit();
                    return;
                }
                #[cfg(feature = "webview")]
                {
                    let webview_result = self
                        .webview
                        .as_ref()
                        .map(LiveApplicationWebView::flush_events);
                    if let Some(Err(error)) = webview_result {
                        self.fail(event_loop, ApplicationError::WebView(error));
                        return;
                    }
                }
                let Some(renderer) = &mut self.renderer else {
                    return;
                };
                let render_graph = &mut self.application.render_graph;
                let on_render = &mut self.application.on_render;
                let mut render_graph_error = None;
                #[cfg(feature = "ui")]
                let ui = &mut self.application.ui;
                #[cfg(feature = "ui")]
                let mut ui_error = None;
                let render_result = renderer.render_frame(self.application.clear_color, |frame| {
                    #[cfg(feature = "ui")]
                    if let Some(ui) = ui
                        && let Err(error) = ui.emit_input(UiSize::new(
                            frame.surface_size()[0],
                            frame.surface_size()[1],
                        ))
                    {
                        ui_error = Some(error);
                        return;
                    }
                    if let Some(graph) = render_graph {
                        match graph.execute(frame) {
                            Ok(execution) => {
                                if std::env::var_os("YUYIB_RENDER_GRAPH_TIMINGS").is_some() {
                                    for timing in &execution.timings {
                                        eprintln!(
                                            "yuyib render-graph: phase={:?} label={} cpu_us={}",
                                            timing.phase,
                                            timing.label,
                                            timing.cpu_duration.as_micros()
                                        );
                                    }
                                }
                            }
                            Err(error) => {
                                render_graph_error = Some(error);
                                return;
                            }
                        }
                    }
                    if let Some(callback) = on_render {
                        callback(frame);
                    }
                    #[cfg(feature = "ui")]
                    if let Some(ui) = ui
                        && let Err(error) = ui.render(frame)
                    {
                        ui_error = Some(error);
                    }
                });
                if let Some(error) = render_graph_error {
                    self.fail(event_loop, ApplicationError::RenderGraph(error));
                    return;
                }
                #[cfg(feature = "ui")]
                if let Some(error) = ui_error {
                    self.fail(event_loop, ApplicationError::Ui(error));
                    return;
                }
                match render_result {
                    Ok(RenderStatus::SurfaceLost) => {
                        self.fail(event_loop, ApplicationError::SurfaceLost);
                    }
                    Ok(RenderStatus::Reconfigured | RenderStatus::SurfaceRecreated) => {
                        if let Some(window) = &self.window {
                            window.request_redraw();
                        }
                    }
                    Ok(_) => {}
                    Err(error) => self.fail(event_loop, ApplicationError::SurfaceValidation(error)),
                }
            }
            _ => {}
        }
        if context.redraw_requested()
            && !close_requested
            && let Some(window) = &self.window
        {
            window.request_redraw();
        }
    }

    fn device_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _device_id: DeviceId,
        event: DeviceEvent,
    ) {
        let context = self.dispatch_device_event_hook(&event);
        if !self.apply_cursor_request(event_loop, &context) {
            return;
        }
        if context.exit_requested() {
            #[cfg(feature = "webview")]
            self.close_webview();
            event_loop.exit();
            return;
        }
        if context.redraw_requested()
            && let Some(window) = &self.window
        {
            window.request_redraw();
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if self.application.render_loop == RenderLoop::Continuous
            && let Some(window) = &self.window
        {
            window.request_redraw();
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, (): ()) {
        if self.failure.is_none()
            && let Some(window) = &self.window
        {
            window.request_redraw();
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "ui")]
    use super::ApplicationUi;
    use super::{Application, ApplicationHost, CursorControl, RenderLoop};
    #[cfg(feature = "webview")]
    use super::{
        ApplicationWebView, ApplicationWebViewCommandError, ApplicationWebViewError,
        ApplicationWebViewEventQueue, ApplicationWebViewQueueConfigError, webview_client_bounds,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    #[cfg(feature = "ui")]
    use std::{cell::RefCell, rc::Rc};
    #[cfg(feature = "ui")]
    use yuyib_input::{UiDpiPolicy, WinitUiAdapter, WinitUiUpdate};
    #[cfg(feature = "webview")]
    use yuyib_platform::winit::dpi::PhysicalSize;
    use yuyib_platform::winit::event::{DeviceEvent, WindowEvent};
    #[cfg(feature = "ui")]
    use yuyib_platform::winit::{
        event::ElementState,
        keyboard::{KeyCode, PhysicalKey},
    };
    #[cfg(feature = "ui")]
    use yuyib_ui::{LayoutKind, Size, UiAction, UiBuilder, Widget, WidgetId};
    #[cfg(feature = "webview")]
    use yuyib_webview::{BridgeLimits, EndpointName, PageEvent, PageSessionId, WebViewBuilder};

    #[test]
    fn application_defaults_to_an_on_demand_render_loop() {
        let application = Application::new();
        assert_eq!(application.render_loop, RenderLoop::OnDemand);
        assert_eq!(application.color_post_process, None);
    }

    #[test]
    fn application_accepts_opt_in_filmic_post_processing() {
        let config = yuyib_render::ColorPostProcess::filmic();
        let application = Application::new().color_post_process(config);

        assert_eq!(application.color_post_process, Some(config));
    }

    #[test]
    fn application_accepts_a_render_callback_for_each_render_frame_lifetime() {
        let application = Application::new().on_render(|frame| {
            let _surface_size = frame.surface_size();
        });

        assert!(application.on_render.is_some());
    }

    #[test]
    fn frame_context_can_request_cursor_control_after_async_state_change() {
        let mut runtime = yuyib_core::Runtime::new();
        let frame = runtime.begin_frame();
        let mut cursor_control = None;
        let mut context = super::FrameContext {
            frame,
            runtime: &mut runtime,
            cursor_control: &mut cursor_control,
        };

        context.set_cursor_control(CursorControl::LockedHidden);

        assert_eq!(cursor_control, Some(CursorControl::LockedHidden));
    }

    #[test]
    fn window_event_hook_observes_event_and_returns_explicit_requests() {
        let calls = Arc::new(AtomicUsize::new(0));
        let callback_calls = Arc::clone(&calls);
        let application = Application::new().on_window_event(move |event, context| {
            assert!(matches!(event, WindowEvent::RedrawRequested));
            callback_calls.fetch_add(1, Ordering::Relaxed);
            context.request_redraw();
        });
        let mut host = ApplicationHost::new(application);

        let context = host.dispatch_window_event_hook(&WindowEvent::RedrawRequested);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert!(!context.exit_requested());
        assert!(context.redraw_requested());
    }

    #[test]
    fn device_event_hook_can_request_cursor_control() {
        let application = Application::new()
            .cursor_control(CursorControl::LockedHidden)
            .on_device_event(|event, context| {
                assert!(matches!(event, DeviceEvent::Added));
                context.set_cursor_control(CursorControl::Released);
            });
        let mut host = ApplicationHost::new(application);

        let context = host.dispatch_device_event_hook(&DeviceEvent::Added);
        assert_eq!(
            context.requested_cursor_control(),
            Some(CursorControl::Released)
        );
        assert_eq!(host.application.cursor_control, CursorControl::LockedHidden);
    }

    #[cfg(feature = "webview")]
    #[test]
    fn webview_configuration_uses_full_client_bounds_and_validates_scale() {
        let bounds =
            webview_client_bounds(PhysicalSize::new(800, 450), 2.0).expect("valid client bounds");
        assert!((bounds.x() - 0.0).abs() < f64::EPSILON);
        assert!((bounds.y() - 0.0).abs() < f64::EPSILON);
        assert!((bounds.width() - 400.0).abs() < f64::EPSILON);
        assert!((bounds.height() - 225.0).abs() < f64::EPSILON);
        assert!(matches!(
            webview_client_bounds(PhysicalSize::new(800, 450), f64::NAN),
            Err(ApplicationWebViewError::InvalidScaleFactor(scale_factor)) if scale_factor.is_nan()
        ));

        let configuration = ApplicationWebView::new(WebViewBuilder::new()).with_visible(false);
        assert!(!configuration.visible);
        let application = Application::new().webview(configuration);
        assert!(application.webview.is_some());
    }

    #[cfg(feature = "webview")]
    fn page_session(value: &str) -> PageSessionId {
        PageSessionId::parse(value).expect("test session must be valid")
    }

    #[cfg(feature = "webview")]
    fn page_event(session: PageSessionId, event: &str) -> PageEvent {
        let limits = BridgeLimits::default();
        PageEvent::from_typed(
            limits.protocol_version(),
            session,
            EndpointName::parse(event).expect("test event name must be valid"),
            event,
            limits,
        )
        .expect("test event must be valid")
    }

    #[cfg(feature = "webview")]
    #[test]
    fn webview_event_queue_reports_lifecycle_states_without_native_host() {
        let queue = ApplicationWebViewEventQueue::new(2);
        let session = page_session("1234567890abcdef1234567890abcdef");

        assert_eq!(
            queue.page_session(),
            Err(ApplicationWebViewCommandError::NotReady)
        );
        assert_eq!(
            queue.enqueue(page_event(session, "demo.pending")),
            Err(ApplicationWebViewCommandError::NotReady)
        );

        queue.ready(session);
        assert_eq!(queue.page_session(), Ok(session));

        queue.close();
        assert_eq!(
            queue.page_session(),
            Err(ApplicationWebViewCommandError::Closed)
        );
        assert_eq!(
            queue.enqueue(page_event(session, "demo.closed")),
            Err(ApplicationWebViewCommandError::Closed)
        );

        let no_bridge = ApplicationWebViewEventQueue::new(1);
        no_bridge.no_local_bridge();
        assert_eq!(
            no_bridge.page_session(),
            Err(ApplicationWebViewCommandError::NoLocalBridge)
        );
        assert_eq!(
            no_bridge.enqueue(page_event(session, "demo.no_bridge")),
            Err(ApplicationWebViewCommandError::NoLocalBridge)
        );
    }

    #[cfg(feature = "webview")]
    #[test]
    fn webview_event_queue_preserves_fifo_and_rejects_stale_or_full_commands() {
        let queue = ApplicationWebViewEventQueue::new(2);
        let session = page_session("1234567890abcdef1234567890abcdef");
        let stale = page_session("1234567890abcdef1234567890abcdee");
        let first = page_event(session, "demo.first");
        let second = page_event(session, "demo.second");
        queue.ready(session);

        assert_eq!(
            queue.enqueue(page_event(stale, "demo.stale")),
            Err(ApplicationWebViewCommandError::StaleSession {
                expected: session,
                actual: stale,
            })
        );
        queue.enqueue(first.clone()).expect("first event must fit");
        queue
            .enqueue(second.clone())
            .expect("second event must fit");
        assert_eq!(
            queue.enqueue(page_event(session, "demo.full")),
            Err(ApplicationWebViewCommandError::Full { capacity: 2 })
        );
        assert_eq!(queue.dequeue(), Some(first));
        assert_eq!(queue.dequeue(), Some(second));
        assert_eq!(queue.dequeue(), None);
    }

    #[cfg(feature = "webview")]
    #[test]
    fn webview_event_queue_configuration_rejects_invalid_capacity_and_duplicates() {
        assert!(matches!(
            ApplicationWebView::new(WebViewBuilder::new()).with_event_queue(0),
            Err(ApplicationWebViewQueueConfigError::ZeroCapacity)
        ));

        let (configuration, _) = ApplicationWebView::new(WebViewBuilder::new())
            .with_event_queue(1)
            .expect("positive event queue capacity must be accepted");
        assert!(matches!(
            configuration.with_event_queue(1),
            Err(ApplicationWebViewQueueConfigError::AlreadyConfigured)
        ));
    }

    #[cfg(feature = "ui")]
    #[test]
    fn optional_ui_caches_layout_and_can_be_registered() {
        let tree = UiBuilder::new(WidgetId::from_key("application-root"), LayoutKind::Column)
            .build()
            .expect("test tree must be valid");
        let mut ui = ApplicationUi::new(tree);

        let first_layout = std::ptr::from_ref(
            ui.layout_for(Size::new(640, 360))
                .expect("layout must be valid"),
        );
        let second_layout = std::ptr::from_ref(
            ui.layout_for(Size::new(640, 360))
                .expect("cached layout must be valid"),
        );
        assert_eq!(first_layout, second_layout);
        assert!(ui.cached_layout().is_some());

        let application = Application::new().ui(ui);
        assert!(application.ui.is_some());
    }

    #[cfg(feature = "ui")]
    #[test]
    fn optional_ui_emits_buffered_input_before_render_and_clears_on_focus_loss() {
        let button = WidgetId::from_key("action");
        let tree = UiBuilder::new(WidgetId::from_key("input-root"), LayoutKind::Column)
            .child(Widget::button(button, "Action"))
            .build()
            .expect("test tree must be valid");
        let received = Rc::new(RefCell::new(Vec::new()));
        let callback_received = Rc::clone(&received);
        let adapter = WinitUiAdapter::new(UiDpiPolicy::PhysicalPixels).expect("valid DPI policy");
        let mut ui = ApplicationUi::new(tree).with_winit_input(adapter, move |response| {
            callback_received
                .borrow_mut()
                .extend_from_slice(response.actions());
        });

        let input = ui.input.as_mut().expect("registered input");
        assert_eq!(
            input
                .adapter
                .handle_key_code(PhysicalKey::Code(KeyCode::Tab), ElementState::Pressed),
            WinitUiUpdate::Buffered
        );
        ui.emit_input(Size::new(320, 180))
            .expect("input frame must succeed");
        assert_eq!(received.borrow().as_slice(), &[UiAction::Focused(button)]);
        assert_eq!(
            ui.input.as_ref().expect("registered input").state.focused(),
            Some(button)
        );

        assert!(ui.handle_window_event(&WindowEvent::Focused(false)));
        ui.emit_input(Size::new(320, 180))
            .expect("focus-loss frame must succeed");
        assert_eq!(
            ui.input.as_ref().expect("registered input").state.focused(),
            None
        );
    }
}
