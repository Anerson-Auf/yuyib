//! Window abstractions for the Windows-first Yuyib runtime.
//!
//! The wrapper owns ergonomic configuration while still exposing Winit at a
//! deliberate low-level boundary for integrations that need it.

#![deny(unsafe_code)]

use std::sync::Arc;

pub use winit;
#[cfg(target_os = "windows")]
use winit::platform::windows::WindowAttributesExtWindows;
use winit::{
    dpi::{LogicalSize, PhysicalPosition, PhysicalSize},
    error::OsError,
    event_loop::ActiveEventLoop,
    raw_window_handle::{HasWindowHandle, RawWindowHandle},
    window::{CursorGrabMode, Fullscreen, Window as WinitWindow, WindowAttributes},
};

/// Desired mouse-cursor behaviour for an application window.
///
/// [`Self::LockedHidden`] is the usual mode for a first-person camera. The
/// platform first asks the operating system for a true lock and falls back to
/// keeping the cursor confined to the window when a true lock is unavailable.
/// [`Self::Released`] restores normal desktop behaviour.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CursorControl {
    /// The cursor is visible and can leave the window.
    #[default]
    Released,
    /// The cursor is hidden and kept in the game window.
    LockedHidden,
}

/// Actual cursor-grab method accepted by the operating system.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CursorGrab {
    /// The operating system provides relative locked-cursor input.
    Locked,
    /// The cursor is confined to the client area as a safe fallback.
    Confined,
}

/// Result of applying a [`CursorControl`] request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CursorControlOutcome {
    /// Normal desktop cursor behaviour was restored.
    Released,
    /// The cursor is hidden and the listed grab mode is active.
    LockedHidden(CursorGrab),
}

/// Native cursor operation failed.
#[derive(Debug)]
pub enum CursorControlError {
    /// Neither a locked nor confined cursor could be enabled.
    Grab(winit::error::ExternalError),
    /// The active grab could not be released.
    Release(winit::error::ExternalError),
}

impl std::fmt::Display for CursorControlError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Grab(error) => write!(formatter, "could not lock or confine the cursor: {error}"),
            Self::Release(error) => write!(formatter, "could not release the cursor: {error}"),
        }
    }
}

impl std::error::Error for CursorControlError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Grab(error) | Self::Release(error) => Some(error),
        }
    }
}

/// Режим начального окна.
///
/// В [`Self::Windowed`] используются поля [`WindowConfig::width`],
/// [`WindowConfig::height`] и [`WindowConfig::resizable`]. Другие режимы
/// намеренно их не читают: так конфигурация не содержит двух конкурирующих
/// способов задать размер окна.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WindowMode {
    /// Обычное окно с системной рамкой и заданным размером.
    #[default]
    Windowed,
    /// Окно без рамки и заголовка.
    ///
    /// При создании растягивается на весь основной монитор, но не переводит
    /// Windows в fullscreen-состояние. Изменить размер перетягиванием нельзя.
    Borderless,
    /// Полноэкранное окно без рамки на основном (текущем) мониторе.
    ///
    /// Это не эксклюзивный видеорежим: Windows не меняет разрешение рабочего
    /// стола, а приложение надёжнее переключается между окнами.
    Fullscreen,
}

/// Настройки главного окна приложения.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WindowConfig {
    /// Заголовок, который увидит пользователь.
    pub title: String,
    /// Начальная ширина клиентской области в логических пикселях.
    pub width: u32,
    /// Начальная высота клиентской области в логических пикселях.
    pub height: u32,
    /// Разрешено ли пользователю менять размер окна.
    pub resizable: bool,
    /// Показывать ли системную рамку и title bar.
    ///
    /// Имеет смысл только в [`WindowMode::Windowed`]. Borderless/Fullscreen
    /// всегда создаются без decorations.
    pub decorations: bool,
    /// Начальный режим окна.
    ///
    /// В [`WindowMode::Borderless`] и [`WindowMode::Fullscreen`] поля
    /// [`Self::width`], [`Self::height`] и [`Self::resizable`] игнорируются.
    pub mode: WindowMode,
}

/// Placement of a child window inside a parent client area, in physical pixels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChildWindowPlacement {
    /// Horizontal offset from the parent client origin.
    pub x: i32,
    /// Vertical offset from the parent client origin.
    pub y: i32,
    /// Child client width.
    pub width: u32,
    /// Child client height.
    pub height: u32,
}

impl ChildWindowPlacement {
    /// Creates a non-empty physical placement.
    ///
    /// # Errors
    ///
    /// Returns an error when width or height is zero.
    pub fn new(x: i32, y: i32, width: u32, height: u32) -> Result<Self, ChildWindowError> {
        if width == 0 || height == 0 {
            return Err(ChildWindowError::Empty);
        }
        Ok(Self {
            x,
            y,
            width,
            height,
        })
    }
}

/// Failed to create or update a child window.
#[derive(Debug)]
pub enum ChildWindowError {
    /// Width or height was zero.
    Empty,
    /// The parent window handle could not be borrowed.
    ParentHandle(String),
    /// The operating system rejected window creation.
    Create(OsError),
}

impl std::fmt::Display for ChildWindowError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => formatter.write_str("child window needs a positive size"),
            Self::ParentHandle(message) => {
                write!(
                    formatter,
                    "could not borrow parent window handle: {message}"
                )
            }
            Self::Create(error) => write!(formatter, "could not create child window: {error}"),
        }
    }
}

impl std::error::Error for ChildWindowError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Create(error) => Some(error),
            Self::Empty | Self::ParentHandle(_) => None,
        }
    }
}

impl WindowConfig {
    /// Создаёт настройки Winit, но не открывает настоящее окно.
    ///
    /// Это низкоуровневая граница для интеграций, которым нужно дополнить
    /// [`WindowAttributes`] собственными настройками.
    #[must_use]
    pub fn attributes(&self) -> WindowAttributes {
        let attributes = WinitWindow::default_attributes().with_title(&self.title);

        match self.mode {
            WindowMode::Windowed => attributes
                .with_inner_size(LogicalSize::new(
                    f64::from(self.width),
                    f64::from(self.height),
                ))
                .with_resizable(self.resizable)
                .with_decorations(self.decorations),
            WindowMode::Borderless => attributes.with_decorations(false).with_resizable(false),
            WindowMode::Fullscreen => attributes
                .with_decorations(false)
                .with_resizable(false)
                .with_fullscreen(Some(Fullscreen::Borderless(None))),
        }
    }
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            title: "Yuyib".to_owned(),
            width: 1280,
            height: 720,
            resizable: true,
            decorations: true,
            mode: WindowMode::Windowed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windowed_mode_uses_requested_size_and_resizability() {
        let attributes = WindowConfig {
            width: 960,
            height: 540,
            resizable: false,
            ..Default::default()
        }
        .attributes();

        assert_eq!(
            attributes.inner_size,
            Some(LogicalSize::new(960.0, 540.0).into())
        );
        assert!(!attributes.resizable);
        assert!(attributes.decorations);
        assert_eq!(attributes.fullscreen, None);
    }

    #[test]
    fn windowed_mode_can_disable_decorations() {
        let attributes = WindowConfig {
            decorations: false,
            ..Default::default()
        }
        .attributes();

        assert!(!attributes.decorations);
        assert_eq!(
            attributes.inner_size,
            Some(LogicalSize::new(1280.0, 720.0).into())
        );
    }

    #[test]
    fn borderless_mode_ignores_windowed_geometry() {
        let attributes = WindowConfig {
            width: 1,
            height: 1,
            resizable: true,
            mode: WindowMode::Borderless,
            ..Default::default()
        }
        .attributes();

        assert_eq!(attributes.inner_size, None);
        assert!(!attributes.resizable);
        assert!(!attributes.decorations);
        assert_eq!(attributes.fullscreen, None);
    }

    #[test]
    fn fullscreen_mode_uses_borderless_primary_monitor_and_ignores_geometry() {
        let attributes = WindowConfig {
            width: 1,
            height: 1,
            resizable: true,
            mode: WindowMode::Fullscreen,
            ..Default::default()
        }
        .attributes();

        assert_eq!(attributes.inner_size, None);
        assert!(!attributes.resizable);
        assert!(!attributes.decorations);
        assert_eq!(attributes.fullscreen, Some(Fullscreen::Borderless(None)));
    }
}

/// A shareable native window.
///
/// The `Arc` ownership model allows a GPU surface to keep its handle source
/// alive without unsafe lifetime extension.
#[derive(Clone, Debug)]
pub struct Window {
    inner: Arc<WinitWindow>,
}

impl Window {
    /// Creates a native window during the event loop's resumed phase.
    ///
    /// # Errors
    ///
    /// Returns [`OsError`] when the operating system rejects window creation.
    pub fn create(event_loop: &ActiveEventLoop, config: &WindowConfig) -> Result<Self, OsError> {
        let inner = event_loop.create_window(config.attributes())?;
        if config.mode == WindowMode::Borderless
            && let Some(monitor) = event_loop.primary_monitor()
        {
            // Borderless intentionally is not Winit fullscreen: it remains a
            // regular top-level window, but has the primary monitor's exact
            // physical placement and extent. Windowed width/height never
            // participate in this path.
            inner.set_outer_position(monitor.position());
            let _ = inner.request_inner_size(monitor.size());
        }
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Requests a redraw from the native event loop.
    pub fn request_redraw(&self) {
        self.inner.request_redraw();
    }

    /// Applies a safe high-level cursor request.
    ///
    /// A locked cursor is tried first. If Windows rejects it, a confined
    /// cursor is tried before this method reports an error. Applications that
    /// need a special platform policy can use [`Self::raw`] directly.
    ///
    /// # Errors
    ///
    /// Returns [`CursorControlError`] when the requested native grab change
    /// cannot be completed.
    pub fn set_cursor_control(
        &self,
        control: CursorControl,
    ) -> Result<CursorControlOutcome, CursorControlError> {
        match control {
            CursorControl::Released => {
                self.inner
                    .set_cursor_grab(CursorGrabMode::None)
                    .map_err(CursorControlError::Release)?;
                self.inner.set_cursor_visible(true);
                Ok(CursorControlOutcome::Released)
            }
            CursorControl::LockedHidden => {
                // On Windows, winit implements `Locked` with a one-pixel
                // `ClipCursor` rectangle at the current desktop cursor
                // position. The initial request can arrive before the newly
                // created window receives focus, leaving that position
                // outside the client area when the grab is restored. Move to
                // a known client-space point first so a successful lock can
                // never strand the hidden cursor outside the game window.
                let size = self.inner.inner_size();
                let center = PhysicalPosition::new(
                    f64::from(size.width) * 0.5,
                    f64::from(size.height) * 0.5,
                );
                let _ = self.inner.set_cursor_position(center);
                let grab = match self.inner.set_cursor_grab(CursorGrabMode::Locked) {
                    Ok(()) => CursorGrab::Locked,
                    Err(_) => self
                        .inner
                        .set_cursor_grab(CursorGrabMode::Confined)
                        .map(|()| CursorGrab::Confined)
                        .map_err(CursorControlError::Grab)?,
                };
                self.inner.set_cursor_visible(false);
                Ok(CursorControlOutcome::LockedHidden(grab))
            }
        }
    }

    /// Returns the current physical client-area size.
    #[must_use]
    pub fn physical_size(&self) -> winit::dpi::PhysicalSize<u32> {
        self.inner.inner_size()
    }

    /// Creates a `WS_CHILD` window hosted inside `parent`.
    ///
    /// Editors use this for a GPU viewport sibling that can sit above a
    /// windowed `WebView2` child: transparent `WebView` holes do not composite
    /// DXGI swapchains of the parent HWND, so the scene must own its own
    /// surface HWND.
    ///
    /// # Errors
    ///
    /// Returns [`ChildWindowError`] when the placement is empty, the parent
    /// handle cannot be borrowed, or the OS rejects creation.
    #[allow(unsafe_code)]
    pub fn create_child(
        event_loop: &ActiveEventLoop,
        parent: &Self,
        placement: ChildWindowPlacement,
    ) -> Result<Self, ChildWindowError> {
        let parent_handle = parent
            .inner
            .window_handle()
            .map_err(|error| ChildWindowError::ParentHandle(error.to_string()))?
            .as_raw();
        let attributes = WinitWindow::default_attributes()
            .with_title("")
            .with_decorations(false)
            .with_resizable(false)
            .with_visible(true)
            .with_position(PhysicalPosition::new(placement.x, placement.y))
            .with_inner_size(PhysicalSize::new(placement.width, placement.height));
        // SAFETY: `parent` outlives the child for the editor lifetime; the
        // handle is the live HWND of that same parent window.
        let attributes = unsafe { attributes.with_parent_window(Some(parent_handle)) };
        let inner = event_loop
            .create_window(attributes)
            .map_err(ChildWindowError::Create)?;
        let child = Self {
            inner: Arc::new(inner),
        };
        child.raise();
        Ok(child)
    }

    /// Creates a transparent, frameless top-level window owned by `owner`.
    ///
    /// Unlike a `WS_CHILD` surface, this is composited by DWM with the owned
    /// window below it. It is intended for a windowed `WebView2` HUD over a
    /// native WGPU game surface: transparent `WebView` pixels cannot reveal a
    /// sibling child HWND reliably, while an owned transparent top-level
    /// overlay can. Windows also keeps the overlay above its owner, hides it
    /// when the owner is minimized, and destroys it with the owner.
    ///
    /// # Errors
    ///
    /// Returns [`ChildWindowError`] when the requested size is empty, the
    /// owner handle cannot be borrowed, or Windows rejects creation.
    #[cfg(target_os = "windows")]
    #[allow(unsafe_code)]
    pub fn create_owned_overlay(
        event_loop: &ActiveEventLoop,
        owner: &Self,
        position: PhysicalPosition<i32>,
        size: PhysicalSize<u32>,
    ) -> Result<Self, ChildWindowError> {
        if size.width == 0 || size.height == 0 {
            return Err(ChildWindowError::Empty);
        }
        let owner_handle = owner
            .inner
            .window_handle()
            .map_err(|error| ChildWindowError::ParentHandle(error.to_string()))?
            .as_raw();
        let RawWindowHandle::Win32(owner) = owner_handle else {
            return Err(ChildWindowError::ParentHandle(
                "owner does not expose a Win32 HWND".to_owned(),
            ));
        };
        let attributes = WinitWindow::default_attributes()
            .with_title("")
            .with_decorations(false)
            .with_resizable(false)
            .with_transparent(true)
            .with_visible(true)
            .with_skip_taskbar(true)
            .with_position(position)
            .with_inner_size(size)
            .with_owner_window(owner.hwnd.get() as _);
        let inner = event_loop
            .create_window(attributes)
            .map_err(ChildWindowError::Create)?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Moves and resizes this child inside its parent client area.
    pub fn set_child_placement(&self, placement: ChildWindowPlacement) {
        self.inner
            .set_outer_position(PhysicalPosition::new(placement.x, placement.y));
        let _ = self
            .inner
            .request_inner_size(PhysicalSize::new(placement.width, placement.height));
        self.inner.set_visible(true);
        self.raise();
    }

    /// Hides a child viewport without destroying the HWND.
    pub fn hide(&self) {
        self.inner.set_visible(false);
    }

    /// Raises this window above sibling children (e.g. `WebView2`).
    #[allow(unsafe_code)]
    pub fn raise(&self) {
        #[cfg(target_os = "windows")]
        {
            let Ok(handle) = self.inner.window_handle() else {
                return;
            };
            let RawWindowHandle::Win32(win32) = handle.as_raw() else {
                return;
            };
            let hwnd = win32.hwnd.get() as windows_sys::Win32::Foundation::HWND;
            // SAFETY: HWND comes from a live winit window we own.
            unsafe {
                let _ = windows_sys::Win32::UI::WindowsAndMessaging::SetWindowPos(
                    hwnd,
                    windows_sys::Win32::UI::WindowsAndMessaging::HWND_TOP,
                    0,
                    0,
                    0,
                    0,
                    windows_sys::Win32::UI::WindowsAndMessaging::SWP_NOMOVE
                        | windows_sys::Win32::UI::WindowsAndMessaging::SWP_NOSIZE
                        | windows_sys::Win32::UI::WindowsAndMessaging::SWP_NOACTIVATE,
                );
            }
        }
    }

    /// Returns the native Winit window for deliberate low-level integrations.
    #[must_use]
    pub const fn raw(&self) -> &Arc<WinitWindow> {
        &self.inner
    }
}
