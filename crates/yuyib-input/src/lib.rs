//! Semantic keyboard action maps and Winit adapters for Yuyib.
//!
//! [`KeyboardActionMap`] maps stable physical [`KeyCode`] values to
//! [`ActionId`]s. [`WinitKeyboardAdapter`] consumes `winit` window events,
//! buffers changes, then emits deterministic [`ActionEvent`] values once per
//! caller-defined frame through [`ActionStates`]. This separates potentially
//! noisy OS event delivery from gameplay's semantic frame boundary.
//!
//! # Supported input
//!
//! This initial Windows/Winit adapter handles only keyboard `KeyCode` events
//! with an identified physical code. It deliberately does **not** claim mouse,
//! text input, touch, gamepad HID, rebinding persistence, IME, or controller
//! vibration support. Hosts can still feed filtered analog axes through
//! [`VirtualStick2d`] into 2D playable loops. Focus loss clears every held
//! mapped key and emits normal action cancellation on the next
//! [`WinitKeyboardAdapter::emit_frame`] call.
//!
//! [`WinitUiAdapter`] separately buffers Winit cursor, primary-mouse, and
//! navigation-key input for a retained [`UiTree`]. It has no event loop or
//! renderer ownership: the host chooses when to call
//! [`WinitUiAdapter::emit_frame`] and supplies an explicit [`UiDpiPolicy`].
//!
//! # Свободная 3D-камера
//!
//! [`FreeCameraController3d`] связывает обычные `WASD`, подъём, ускорение и
//! относительное движение мыши с [`Camera3d`], но не забирает у приложения
//! окно или игровой цикл. Передайте ему оконные и device-события, затем раз в
//! кадр вызовите `step`. Настройки, раскладка и низкоуровневые методы ввода
//! остаются открытыми для игры с собственной системой действий.
//!
//! [`CollisionAwareThirdPersonCamera3d`] — отдельный renderer-neutral chase
//! controller. Он принимает абсолютный semantic orbit и gameplay target,
//! сокращает boom через ray/sphere queries к static triangle map и плавно
//! восстанавливает distance после выхода из-за стены. Он намеренно не знает о
//! Winit events или конкретной character implementation.
//!
//! [`CharacterFollowCamera3d`] склеивает free-look и chase boom в один
//! first/third-person follow rig для playable vertical slice.

#![forbid(unsafe_code)]

mod follow_camera;
mod player_character;
mod virtual_stick;
#[cfg(feature = "gamepad")]
mod gamepad;

pub use follow_camera::{
    CharacterCameraMode3d, CharacterFollowCamera3d, CharacterFollowCameraError3d,
};
pub use player_character::{
    PlayerCharacterBindings3d, PlayerCharacterControlConfig3d, PlayerCharacterControlError3d,
    PlayerCharacterControls3d, actions as player_actions,
};
pub use virtual_stick::{VirtualStick2d, VirtualStickError2d};
#[cfg(feature = "gamepad")]
pub use gamepad::{GilrsGamepadAdapter2d, GilrsGamepadError2d};

use std::{
    collections::{BTreeSet, VecDeque},
    error::Error,
    fmt,
};

use winit::{
    dpi::PhysicalPosition,
    event::{DeviceEvent, ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    keyboard::{KeyCode, ModifiersState, PhysicalKey},
};
use yuyib_gameplay::{ActionEvent, ActionId, ActionStates, ActionValue};
use yuyib_physics::{
    PhysicsConfigError, Ray3d, TriangleMesh3d, TriangleMeshQueryError, Vec3 as PhysicsVec3,
};
use yuyib_platform::CursorControl;
use yuyib_render_3d::Camera3d;
use yuyib_ui::{
    KeyboardInput, Point, PointerInput, UiError, UiInputState, UiLayout, UiResponse, UiTree,
    handle_input, handle_keyboard_input, handle_scroll_input,
};

/// One physical keyboard binding for a semantic action.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeyBinding {
    /// Stable physical keyboard position/code supplied by Winit.
    pub key: KeyCode,
    /// Semantic action activated while `key` is held.
    pub action: ActionId,
}

impl KeyBinding {
    /// Creates one physical-key semantic action binding.
    #[must_use]
    pub const fn new(key: KeyCode, action: ActionId) -> Self {
        Self { key, action }
    }
}

/// Keyboard mapping with one owner action per physical key.
///
/// Multiple keys may target one action (for example `KeyW` and `ArrowUp`), but
/// binding the same key twice is rejected so OS events never have ambiguous
/// ownership. Emission order is sorted by [`ActionId`], not insertion order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KeyboardActionMap {
    bindings: Vec<KeyBinding>,
}

impl KeyboardActionMap {
    /// Creates an empty keyboard action map.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            bindings: Vec::new(),
        }
    }

    /// Adds a unique physical-key binding.
    ///
    /// # Errors
    ///
    /// Returns [`KeyboardBindingError::KeyAlreadyBound`] if `key` already
    /// targets any action in this map.
    pub fn bind(
        &mut self,
        key: KeyCode,
        action: impl Into<ActionId>,
    ) -> Result<(), KeyboardBindingError> {
        if let Some(existing) = self.bindings.iter().find(|binding| binding.key == key) {
            return Err(KeyboardBindingError::KeyAlreadyBound {
                key,
                existing: existing.action.clone(),
            });
        }
        self.bindings.push(KeyBinding::new(key, action.into()));
        Ok(())
    }

    /// Removes the binding for `key` and returns it if present.
    pub fn unbind(&mut self, key: KeyCode) -> Option<KeyBinding> {
        self.bindings
            .iter()
            .position(|binding| binding.key == key)
            .map(|index| self.bindings.remove(index))
    }

    /// Returns every binding in insertion order for configuration UI.
    #[must_use]
    pub fn bindings(&self) -> &[KeyBinding] {
        &self.bindings
    }

    fn action_for(&self, key: KeyCode) -> Option<&ActionId> {
        self.bindings
            .iter()
            .find(|binding| binding.key == key)
            .map(|binding| &binding.action)
    }

    fn active_for(&self, action: &ActionId, pressed: &[KeyCode]) -> bool {
        self.bindings
            .iter()
            .any(|binding| &binding.action == action && pressed.contains(&binding.key))
    }
}

/// Keyboard map mutation failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KeyboardBindingError {
    /// A physical key already has an unambiguous owner action.
    KeyAlreadyBound {
        /// Attempted duplicate key.
        key: KeyCode,
        /// Existing action owner.
        existing: ActionId,
    },
}

impl fmt::Display for KeyboardBindingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::KeyAlreadyBound { key, existing } => {
                write!(
                    formatter,
                    "physical key {key:?} is already bound to {existing}"
                )
            }
        }
    }
}

impl Error for KeyboardBindingError {}

/// Result of feeding one Winit event into [`WinitKeyboardAdapter`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WinitInputUpdate {
    /// Event does not affect this keyboard adapter.
    Ignored,
    /// An identified mapped key changed pressed state.
    KeyChanged,
    /// Window focus was lost and all mapped held keys were released.
    FocusLost,
}

/// Frame-buffered Winit keyboard adapter.
///
/// Feed every relevant [`WindowEvent`] to [`Self::handle_window_event`], then
/// call [`Self::emit_frame`] exactly once at the application's selected
/// gameplay-frame boundary. It emits each changed semantic action at most once
/// in sorted `ActionId` order. A frame may therefore contain a `Started`,
/// `Performed`, or `Canceled` event per changed action, independent of Winit
/// key-repeat event frequency.
pub struct WinitKeyboardAdapter {
    map: KeyboardActionMap,
    pressed: Vec<KeyCode>,
    dirty_actions: BTreeSet<ActionId>,
}

impl WinitKeyboardAdapter {
    /// Creates an adapter from the supplied keyboard map.
    #[must_use]
    pub fn new(map: KeyboardActionMap) -> Self {
        Self {
            map,
            pressed: Vec::new(),
            dirty_actions: BTreeSet::new(),
        }
    }

    /// Returns the currently configured action map.
    #[must_use]
    pub const fn map(&self) -> &KeyboardActionMap {
        &self.map
    }

    /// Replaces the map and cancels any actions held through its old bindings.
    ///
    /// Call [`Self::emit_frame`] after replacement to deliver those
    /// cancellations. The new map starts with no pressed keys because Winit
    /// does not guarantee a replay of currently-held physical keys.
    pub fn replace_map(&mut self, map: KeyboardActionMap) {
        for binding in &self.map.bindings {
            if self.pressed.contains(&binding.key) {
                self.dirty_actions.insert(binding.action.clone());
            }
        }
        self.map = map;
        self.pressed.clear();
    }

    /// Processes a Winit window event without mutating [`ActionStates`].
    pub fn handle_window_event(&mut self, event: &WindowEvent) -> WinitInputUpdate {
        match event {
            WindowEvent::KeyboardInput { event, .. } => {
                let PhysicalKey::Code(key) = event.physical_key else {
                    return WinitInputUpdate::Ignored;
                };
                self.handle_key_code(key, event.state)
            }
            WindowEvent::Focused(false) => {
                if self.pressed.is_empty() {
                    return WinitInputUpdate::Ignored;
                }
                for binding in &self.map.bindings {
                    if self.pressed.contains(&binding.key) {
                        self.dirty_actions.insert(binding.action.clone());
                    }
                }
                self.pressed.clear();
                WinitInputUpdate::FocusLost
            }
            _ => WinitInputUpdate::Ignored,
        }
    }

    /// Processes one identified Winit physical key state transition.
    ///
    /// This is exposed for platform bridges that already decoded a Winit event;
    /// normal applications can pass whole events to [`Self::handle_window_event`].
    pub fn handle_key_code(&mut self, key: KeyCode, state: ElementState) -> WinitInputUpdate {
        let Some(action) = self.map.action_for(key).cloned() else {
            return WinitInputUpdate::Ignored;
        };
        let was_pressed = self.pressed.contains(&key);
        let pressed = state == ElementState::Pressed;
        if was_pressed == pressed {
            return WinitInputUpdate::Ignored;
        }
        let was_active = self.map.active_for(&action, &self.pressed);
        if pressed {
            self.pressed.push(key);
        } else if let Some(index) = self.pressed.iter().position(|held| *held == key) {
            self.pressed.remove(index);
        }
        if self.map.active_for(&action, &self.pressed) != was_active {
            self.dirty_actions.insert(action);
        }
        WinitInputUpdate::KeyChanged
    }

    /// Returns whether any bound key for `action` is currently held.
    #[must_use]
    pub fn is_active(&self, action: &ActionId) -> bool {
        self.map.active_for(action, &self.pressed)
    }

    /// Emits sorted semantic action transitions for changed bindings.
    ///
    /// The returned list is deterministic for equal map/state/frame inputs.
    /// It can be empty if a physical event did not change the action state, for
    /// example when releasing one of two keys mapped to the same action while
    /// the other key remains held.
    pub fn emit_frame(&mut self, actions: &mut ActionStates, frame: u64) -> Vec<ActionEvent> {
        let dirty = std::mem::take(&mut self.dirty_actions);
        dirty
            .into_iter()
            .filter_map(|action| {
                let value = ActionValue::digital(self.map.active_for(&action, &self.pressed));
                actions.submit(action, value, frame)
            })
            .collect()
    }
}

/// Explicit conversion from Winit physical cursor coordinates to retained UI coordinates.
///
/// [`yuyib_ui`] stores integer logical points. [`Self::PhysicalPixels`] rounds
/// Winit's physical `f64` cursor position to the nearest physical pixel, while
/// [`Self::LogicalPixels`] first divides by the caller-supplied Winit window
/// scale factor and then rounds to the nearest logical pixel. The choice must
/// match the viewport used to create [`UiLayout`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum UiDpiPolicy {
    /// Keep Winit physical pixels, for layouts built from a physical surface size.
    #[default]
    PhysicalPixels,
    /// Convert physical cursor coordinates to logical pixels with this scale factor.
    LogicalPixels {
        /// Current native window scale factor, which must be finite and positive.
        scale_factor: f64,
    },
}

/// Winit UI adapter setup, coordinate conversion, or UI handling failure.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WinitUiError {
    /// Logical coordinate conversion received a non-finite or non-positive scale factor.
    InvalidScaleFactor(f64),
    /// A Winit cursor coordinate cannot be represented by retained integer points.
    InvalidCursorPosition {
        /// Physical horizontal coordinate supplied by Winit.
        x: f64,
        /// Physical vertical coordinate supplied by Winit.
        y: f64,
    },
    /// Retained-tree layout or input validation failed while flushing a frame.
    Ui(UiError),
}

impl fmt::Display for WinitUiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidScaleFactor(scale_factor) => {
                write!(formatter, "invalid UI scale factor {scale_factor}")
            }
            Self::InvalidCursorPosition { x, y } => {
                write!(formatter, "UI cursor position ({x}, {y}) is out of range")
            }
            Self::Ui(source) => write!(formatter, "retained UI input failed: {source}"),
        }
    }
}

impl Error for WinitUiError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Ui(source) => Some(source),
            Self::InvalidScaleFactor(_) | Self::InvalidCursorPosition { .. } => None,
        }
    }
}

/// Result of one [`WinitUiAdapter::handle_window_event`] call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WinitUiUpdate {
    /// Event is outside this adapter's mouse, focus, modifier, and UI-key scope.
    Ignored,
    /// A pointer or keyboard UI command was buffered for a later frame boundary.
    Buffered,
    /// Modifier state changed for a possible later Shift+Tab command.
    ModifiersChanged,
    /// Native focus was lost and a retained-input clear was buffered.
    FocusLost,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BufferedUiInput {
    Pointer(PointerInput),
    Keyboard(KeyboardInput),
    Scroll { point: Point, vertical_delta: i32 },
    ClearPointer,
    ClearAll,
}

/// Frame-buffered Winit adapter for the retained [`UiTree`] input API.
///
/// Feed window events through [`Self::handle_window_event`] from the host's
/// existing Winit handler. At a deliberate boundary such as
/// `WindowEvent::RedrawRequested`, call [`Self::emit_frame`] with the same
/// tree/layout/input state used for rendering. Responses are returned in event
/// arrival order, independently of Winit's callback batching.
///
/// This adapter does not own a window, event loop, renderer, text input, IME,
/// clipboard, accessibility bridge, or key-repeat policy. It maps primary
/// mouse input plus Tab/Shift+Tab/Enter/Space commands only. `KeyboardInput`
/// press repeats from Winit are intentionally buffered like regular presses;
/// a host that wants a different repeat policy should filter events before
/// forwarding them.
pub struct WinitUiAdapter {
    dpi_policy: UiDpiPolicy,
    cursor: Option<Point>,
    modifiers: ModifiersState,
    buffered: VecDeque<BufferedUiInput>,
}

impl WinitUiAdapter {
    /// Creates an adapter with an explicit UI coordinate conversion policy.
    ///
    /// # Errors
    ///
    /// Returns [`WinitUiError::InvalidScaleFactor`] for an invalid logical DPI scale.
    pub fn new(dpi_policy: UiDpiPolicy) -> Result<Self, WinitUiError> {
        validate_dpi_policy(dpi_policy)?;
        Ok(Self {
            dpi_policy,
            cursor: None,
            modifiers: ModifiersState::default(),
            buffered: VecDeque::new(),
        })
    }

    /// Returns the coordinate policy selected when this adapter was created.
    #[must_use]
    pub const fn dpi_policy(&self) -> UiDpiPolicy {
        self.dpi_policy
    }

    /// Returns the number of platform commands awaiting [`Self::emit_frame`].
    #[must_use]
    pub fn pending_len(&self) -> usize {
        self.buffered.len()
    }

    /// Buffers supported Winit events without mutating retained [`UiInputState`].
    ///
    /// The adapter retains the latest cursor point so a subsequent primary
    /// mouse button event can use it. Primary mouse events before any cursor
    /// event are ignored because they have no deterministic retained-UI target.
    /// On a Winit `Focused(false)` event, it discards cursor/modifier state and
    /// queues a complete [`UiInputState::clear`] at the next frame boundary.
    ///
    /// # Errors
    ///
    /// Returns coordinate conversion errors for `CursorMoved` events.
    pub fn handle_window_event(
        &mut self,
        event: &WindowEvent,
    ) -> Result<WinitUiUpdate, WinitUiError> {
        match event {
            WindowEvent::CursorMoved { position, .. } => self.handle_cursor_position(*position),
            WindowEvent::CursorLeft { .. } => Ok(self.handle_cursor_left()),
            WindowEvent::MouseInput { state, button, .. } => {
                Ok(self.handle_mouse_button(*state, *button))
            }
            WindowEvent::MouseWheel { delta, .. } => Ok(self.handle_mouse_wheel(*delta)),
            WindowEvent::ModifiersChanged(modifiers) => {
                Ok(self.handle_modifiers(modifiers.state()))
            }
            WindowEvent::KeyboardInput { event, .. } => {
                Ok(self.handle_key_code(event.physical_key, event.state))
            }
            WindowEvent::Focused(false) => Ok(self.handle_focus_lost()),
            _ => Ok(WinitUiUpdate::Ignored),
        }
    }

    /// Converts and buffers one Winit physical cursor position.
    ///
    /// This lower-level method is useful for hosts that dispatch Winit events
    /// before they reach a shared window handler.
    ///
    /// # Errors
    ///
    /// Returns [`WinitUiError::InvalidCursorPosition`] if conversion is not representable.
    pub fn handle_cursor_position(
        &mut self,
        position: PhysicalPosition<f64>,
    ) -> Result<WinitUiUpdate, WinitUiError> {
        let point = self.convert_cursor(position)?;
        self.cursor = Some(point);
        self.buffered
            .push_back(BufferedUiInput::Pointer(PointerInput::Move(point)));
        Ok(WinitUiUpdate::Buffered)
    }

    /// Buffers a pointer-state clear after the cursor leaves the native window.
    ///
    /// Keyboard focus remains intact, matching normal desktop UI behavior.
    #[must_use]
    pub fn handle_cursor_left(&mut self) -> WinitUiUpdate {
        self.cursor = None;
        self.buffered.push_back(BufferedUiInput::ClearPointer);
        WinitUiUpdate::Buffered
    }

    /// Stores current Winit modifiers for future keyboard commands.
    #[must_use]
    pub fn handle_modifiers(&mut self, modifiers: ModifiersState) -> WinitUiUpdate {
        self.modifiers = modifiers;
        WinitUiUpdate::ModifiersChanged
    }

    /// Buffers a complete retained-state clear after native window focus loss.
    ///
    /// Cursor location and modifiers are discarded immediately; hover, press,
    /// and keyboard focus are cleared only when [`Self::emit_frame`] applies
    /// this transition in sequence with other buffered input.
    #[must_use]
    pub fn handle_focus_lost(&mut self) -> WinitUiUpdate {
        self.cursor = None;
        self.modifiers = ModifiersState::default();
        self.buffered.push_back(BufferedUiInput::ClearAll);
        WinitUiUpdate::FocusLost
    }

    /// Buffers one primary mouse transition at the latest known cursor point.
    #[must_use]
    pub fn handle_mouse_button(
        &mut self,
        state: ElementState,
        button: MouseButton,
    ) -> WinitUiUpdate {
        if button != MouseButton::Left {
            return WinitUiUpdate::Ignored;
        }
        let Some(point) = self.cursor else {
            return WinitUiUpdate::Ignored;
        };
        let input = match state {
            ElementState::Pressed => PointerInput::PrimaryDown(point),
            ElementState::Released => PointerInput::PrimaryUp(point),
        };
        self.buffered.push_back(BufferedUiInput::Pointer(input));
        WinitUiUpdate::Buffered
    }

    /// Buffers a vertical wheel delta at the latest known cursor position.
    ///
    /// One Winit line equals 24 retained logical pixels. Positive Winit values
    /// move content towards the top; pixel deltas are rounded to the nearest
    /// retained pixel and non-finite values are ignored.
    #[must_use]
    pub fn handle_mouse_wheel(&mut self, delta: MouseScrollDelta) -> WinitUiUpdate {
        let Some(point) = self.cursor else {
            return WinitUiUpdate::Ignored;
        };
        let vertical = match delta {
            MouseScrollDelta::LineDelta(_, y) => f64::from(y) * 24.0,
            MouseScrollDelta::PixelDelta(position) => position.y,
        };
        let Some(vertical_delta) = rounded_coordinate(vertical) else {
            return WinitUiUpdate::Ignored;
        };
        if vertical_delta == 0 {
            return WinitUiUpdate::Ignored;
        }
        self.buffered.push_back(BufferedUiInput::Scroll {
            point,
            vertical_delta,
        });
        WinitUiUpdate::Buffered
    }

    /// Buffers one supported physical UI key transition.
    ///
    /// Only press events become retained commands; releases are ignored. The
    /// current modifier state, provided by [`WindowEvent::ModifiersChanged`],
    /// selects Shift+Tab.
    #[must_use]
    pub fn handle_key_code(
        &mut self,
        physical_key: PhysicalKey,
        state: ElementState,
    ) -> WinitUiUpdate {
        if state != ElementState::Pressed {
            return WinitUiUpdate::Ignored;
        }
        let PhysicalKey::Code(key) = physical_key else {
            return WinitUiUpdate::Ignored;
        };
        let input = match key {
            KeyCode::Tab if self.modifiers.shift_key() => KeyboardInput::ShiftTab,
            KeyCode::Tab => KeyboardInput::Tab,
            KeyCode::Enter | KeyCode::NumpadEnter => KeyboardInput::Enter,
            KeyCode::Space => KeyboardInput::Space,
            _ => return WinitUiUpdate::Ignored,
        };
        self.buffered.push_back(BufferedUiInput::Keyboard(input));
        WinitUiUpdate::Buffered
    }

    /// Flushes buffered input at a caller-chosen UI frame boundary.
    ///
    /// Pointer and keyboard responses preserve platform arrival order. Focus
    /// loss and cursor-leave clear retained state without inventing a response.
    ///
    /// # Errors
    ///
    /// Returns [`WinitUiError::Ui`] when the supplied layout is incompatible
    /// with `tree`. Commands after the failing command remain buffered.
    pub fn emit_frame(
        &mut self,
        tree: &UiTree,
        layout: &UiLayout,
        state: &mut UiInputState,
    ) -> Result<Vec<UiResponse>, WinitUiError> {
        let mut responses = Vec::new();
        while let Some(input) = self.buffered.pop_front() {
            match input {
                BufferedUiInput::Pointer(input) => responses
                    .push(handle_input(tree, layout, state, input).map_err(WinitUiError::Ui)?),
                BufferedUiInput::Keyboard(input) => responses.push(
                    handle_keyboard_input(tree, layout, state, input).map_err(WinitUiError::Ui)?,
                ),
                BufferedUiInput::Scroll {
                    point,
                    vertical_delta,
                } => responses.push(
                    handle_scroll_input(tree, layout, state, point, vertical_delta)
                        .map_err(WinitUiError::Ui)?,
                ),
                BufferedUiInput::ClearPointer => state.clear_pointer(),
                BufferedUiInput::ClearAll => state.clear(),
            }
        }
        Ok(responses)
    }

    fn convert_cursor(&self, position: PhysicalPosition<f64>) -> Result<Point, WinitUiError> {
        let (x, y) = match self.dpi_policy {
            UiDpiPolicy::PhysicalPixels => (position.x, position.y),
            UiDpiPolicy::LogicalPixels { scale_factor } => {
                (position.x / scale_factor, position.y / scale_factor)
            }
        };
        let x = rounded_coordinate(x).ok_or(WinitUiError::InvalidCursorPosition {
            x: position.x,
            y: position.y,
        })?;
        let y = rounded_coordinate(y).ok_or(WinitUiError::InvalidCursorPosition {
            x: position.x,
            y: position.y,
        })?;
        Ok(Point::new(x, y))
    }
}

fn validate_dpi_policy(dpi_policy: UiDpiPolicy) -> Result<(), WinitUiError> {
    if let UiDpiPolicy::LogicalPixels { scale_factor } = dpi_policy
        && (!scale_factor.is_finite() || scale_factor <= 0.0)
    {
        return Err(WinitUiError::InvalidScaleFactor(scale_factor));
    }
    Ok(())
}

fn rounded_coordinate(value: f64) -> Option<i32> {
    if !value.is_finite() {
        return None;
    }
    let rounded = value.round();
    if rounded < f64::from(i32::MIN) || rounded > f64::from(i32::MAX) {
        return None;
    }
    // Bounds above prove this conversion cannot truncate or wrap.
    #[allow(clippy::cast_possible_truncation)]
    let coordinate = rounded as i32;
    Some(coordinate)
}

/// Hard upper work bounds accepted by [`ThirdPersonCameraConfig3d`].
pub const MAX_THIRD_PERSON_CAMERA_PROBE_STEPS: usize = 64;
/// Hard upper penetration-resolution bound accepted by the chase camera.
pub const MAX_THIRD_PERSON_CAMERA_COLLISION_ITERATIONS: usize = 32;

/// Validated presentation and collision policy for a chase/orbit camera.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThirdPersonCameraConfig3d {
    /// Nominal boom distance behind the focus point.
    pub distance: f32,
    /// Smallest non-degenerate focus-to-camera distance retained under collision.
    ///
    /// This is a fail-safe for a focus point touching or starting inside map
    /// geometry. It must be at least [`Self::near`] and no greater than
    /// [`Self::distance`].
    pub minimum_distance: f32,
    /// Vertical focus offset above the supplied gameplay target.
    pub target_height: f32,
    /// Horizontal camera offset; positive values move towards camera-right.
    pub shoulder_offset: f32,
    /// Radius of the camera collision probe.
    pub probe_radius: f32,
    /// Additional gap retained before an exact centre-ray wall hit.
    pub wall_clearance: f32,
    /// Maximum arm-length recovery speed after an obstruction disappears.
    pub recovery_speed: f32,
    /// Largest frame delta used by recovery.
    pub maximum_delta_seconds: f32,
    /// Minimum accepted orbit pitch in radians.
    pub minimum_pitch_radians: f32,
    /// Maximum accepted orbit pitch in radians.
    pub maximum_pitch_radians: f32,
    /// Maximum deterministic sphere samples along the boom.
    pub maximum_probe_steps: usize,
    /// Penetration iterations used by each sphere sample.
    pub collision_iterations: usize,
    /// Vertical perspective field of view in radians.
    pub vertical_fov_radians: f32,
    /// Positive near clip distance.
    pub near: f32,
    /// Far clip distance greater than [`Self::near`].
    pub far: f32,
}

impl ThirdPersonCameraConfig3d {
    /// Validates finite camera values and hard query-work bounds.
    ///
    /// # Errors
    ///
    /// Returns [`ThirdPersonCameraError3d::InvalidConfig`] without constructing
    /// partially valid camera state.
    pub fn validate(self) -> Result<(), ThirdPersonCameraError3d> {
        third_person_positive(self.distance, "distance")?;
        third_person_positive(self.minimum_distance, "minimum_distance")?;
        third_person_finite(self.target_height, "target_height")?;
        third_person_finite(self.shoulder_offset, "shoulder_offset")?;
        third_person_positive(self.probe_radius, "probe_radius")?;
        third_person_non_negative(self.wall_clearance, "wall_clearance")?;
        third_person_positive(self.recovery_speed, "recovery_speed")?;
        third_person_positive(self.maximum_delta_seconds, "maximum_delta_seconds")?;
        third_person_finite(self.minimum_pitch_radians, "minimum_pitch_radians")?;
        third_person_finite(self.maximum_pitch_radians, "maximum_pitch_radians")?;
        let pitch_limit = std::f32::consts::FRAC_PI_2;
        if self.minimum_pitch_radians <= -pitch_limit
            || self.maximum_pitch_radians >= pitch_limit
            || self.minimum_pitch_radians >= self.maximum_pitch_radians
        {
            return Err(ThirdPersonCameraError3d::InvalidConfig {
                field: "pitch_range",
                reason: "must satisfy -pi/2 < minimum < maximum < pi/2",
            });
        }
        if !(1..=MAX_THIRD_PERSON_CAMERA_PROBE_STEPS).contains(&self.maximum_probe_steps) {
            return Err(ThirdPersonCameraError3d::InvalidConfig {
                field: "maximum_probe_steps",
                reason: "must be in 1..=64",
            });
        }
        #[allow(
            clippy::cast_precision_loss,
            reason = "probe steps are hard-bounded to 64 and exactly represented by f32"
        )]
        let covered_distance = self.probe_radius * self.maximum_probe_steps as f32;
        let maximum_arm_length = self.distance.hypot(self.shoulder_offset);
        if maximum_arm_length > covered_distance {
            return Err(ThirdPersonCameraError3d::InvalidConfig {
                field: "probe_coverage",
                reason: "probe_radius * maximum_probe_steps must cover distance and shoulder offset",
            });
        }
        if !(1..=MAX_THIRD_PERSON_CAMERA_COLLISION_ITERATIONS).contains(&self.collision_iterations)
        {
            return Err(ThirdPersonCameraError3d::InvalidConfig {
                field: "collision_iterations",
                reason: "must be in 1..=32",
            });
        }
        if !self.vertical_fov_radians.is_finite()
            || self.vertical_fov_radians <= 0.0
            || self.vertical_fov_radians >= std::f32::consts::PI
        {
            return Err(ThirdPersonCameraError3d::InvalidConfig {
                field: "vertical_fov_radians",
                reason: "must be finite and between zero and pi",
            });
        }
        third_person_positive(self.near, "near")?;
        third_person_positive(self.far, "far")?;
        if self.minimum_distance < self.near || self.minimum_distance > self.distance {
            return Err(ThirdPersonCameraError3d::InvalidConfig {
                field: "minimum_distance",
                reason: "must be at least near and no greater than distance",
            });
        }
        if self.far <= self.near {
            return Err(ThirdPersonCameraError3d::InvalidConfig {
                field: "far",
                reason: "must be greater than near",
            });
        }
        Ok(())
    }
}

impl Default for ThirdPersonCameraConfig3d {
    fn default() -> Self {
        Self {
            distance: 4.0,
            minimum_distance: 0.1,
            target_height: 1.5,
            shoulder_offset: 0.35,
            probe_radius: 0.2,
            wall_clearance: 0.05,
            recovery_speed: 8.0,
            maximum_delta_seconds: 0.1,
            minimum_pitch_radians: -70.0_f32.to_radians(),
            maximum_pitch_radians: 70.0_f32.to_radians(),
            maximum_probe_steps: 32,
            collision_iterations: 4,
            vertical_fov_radians: 70.0_f32.to_radians(),
            near: 0.05,
            far: 2_000.0,
        }
    }
}

/// Absolute semantic orbit supplied independently of mouse/gamepad events.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ThirdPersonOrbit3d {
    /// World-up yaw in radians; zero places the camera behind a -Z-facing target.
    pub yaw_radians: f32,
    /// Vertical boom pitch in radians; positive values place the camera higher.
    pub pitch_radians: f32,
}

impl ThirdPersonOrbit3d {
    /// Creates an absolute orbit. Values are validated by camera construction/update.
    #[must_use]
    pub const fn new(yaw_radians: f32, pitch_radians: f32) -> Self {
        Self {
            yaw_radians,
            pitch_radians,
        }
    }
}

/// Observable work and obstruction state from one chase-camera update.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ThirdPersonCameraUpdate3d {
    /// Whether collision shortened or displaced the nominal camera boom.
    pub obstructed: bool,
    /// Whether the obstruction is gone but the boom is still extending at the recovery limit.
    pub recovering: bool,
    /// Full desired focus-to-camera arm length, including shoulder offset.
    pub desired_arm_length: f32,
    /// Collision-safe maximum arm length found this update.
    pub collision_arm_length: f32,
    /// Final focus-to-camera distance after recovery and sphere resolution.
    pub actual_arm_length: f32,
    /// Collision could not retain the configured minimum boom without overlap.
    ///
    /// The camera used the minimum non-degenerate fallback distance so a
    /// renderer never receives `position == target`.
    pub minimum_distance_forced: bool,
    /// Source triangle hit by the exact centre ray, if any.
    pub ray_hit_triangle: Option<usize>,
    /// Sphere samples executed along the shortened boom.
    pub sphere_probe_steps: usize,
    /// Penetration contacts processed by probe and final recovery queries.
    pub sphere_contacts: usize,
}

/// Deterministic collision-aware third-person camera state.
///
/// The controller accepts absolute semantic orbit values and a gameplay target;
/// it has no Winit, mouse or gamepad ownership. Collision combines an exact
/// target-to-camera ray with bounded sphere samples and a final penetration
/// recovery against immutable [`TriangleMesh3d`]. Obstruction contracts the
/// boom immediately; returning to the requested distance is speed-limited.
#[derive(Clone, Debug, PartialEq)]
pub struct CollisionAwareThirdPersonCamera3d {
    config: ThirdPersonCameraConfig3d,
    orbit: ThirdPersonOrbit3d,
    focus: [f32; 3],
    position: [f32; 3],
    current_arm_length: f32,
    initialized: bool,
}

impl CollisionAwareThirdPersonCamera3d {
    /// Creates camera state without performing a collision query.
    ///
    /// The first [`Self::update`] snaps to a collision-safe distance rather
    /// than recovering from an arbitrary constructor position.
    ///
    /// # Errors
    ///
    /// Returns a typed configuration or finite target/orbit error.
    pub fn new(
        config: ThirdPersonCameraConfig3d,
        target: [f32; 3],
        orbit: ThirdPersonOrbit3d,
    ) -> Result<Self, ThirdPersonCameraError3d> {
        config.validate()?;
        validate_third_person_target(target)?;
        let orbit = validate_third_person_orbit(orbit, config)?;
        let focus = add3(target, [0.0, config.target_height, 0.0]);
        let (direction, arm_length) = third_person_arm(config, orbit);
        let position = add3(focus, scale3(direction, arm_length));
        Ok(Self {
            config,
            orbit,
            focus,
            position,
            current_arm_length: arm_length,
            initialized: false,
        })
    }

    /// Returns the validated immutable camera policy.
    #[must_use]
    pub const fn config(&self) -> ThirdPersonCameraConfig3d {
        self.config
    }

    /// Returns the most recently accepted absolute orbit.
    #[must_use]
    pub const fn orbit(&self) -> ThirdPersonOrbit3d {
        self.orbit
    }

    /// Returns the current collision-resolved world position.
    #[must_use]
    pub const fn position(&self) -> [f32; 3] {
        self.position
    }

    /// Returns the current focus point after target-height offset.
    #[must_use]
    pub const fn focus(&self) -> [f32; 3] {
        self.focus
    }

    /// Returns a renderer camera looking from the resolved position to focus.
    #[must_use]
    pub const fn camera(&self) -> Camera3d {
        Camera3d::new(
            self.position,
            self.focus,
            [0.0, 1.0, 0.0],
            self.config.vertical_fov_radians,
            self.config.near,
            self.config.far,
        )
    }

    /// Replaces the policy transactionally and resets distance recovery.
    ///
    /// # Errors
    ///
    /// Invalid configuration leaves the current policy/state unchanged.
    pub fn set_config(
        &mut self,
        config: ThirdPersonCameraConfig3d,
    ) -> Result<(), ThirdPersonCameraError3d> {
        config.validate()?;
        self.orbit = validate_third_person_orbit(self.orbit, config)?;
        self.config = config;
        self.initialized = false;
        Ok(())
    }

    /// Resolves one target/orbit snapshot against a static triangle map.
    ///
    /// Query work is bounded by `maximum_probe_steps * collision_iterations`
    /// plus one exact ray and one final sphere resolution. A large frame delta
    /// is clamped only for smooth recovery; collision contraction is immediate.
    ///
    /// # Errors
    ///
    /// Returns a typed finite input or underlying physics-query error without
    /// partially committing camera state.
    pub fn update(
        &mut self,
        target: [f32; 3],
        orbit: ThirdPersonOrbit3d,
        delta_seconds: f32,
        mesh: &TriangleMesh3d,
    ) -> Result<ThirdPersonCameraUpdate3d, ThirdPersonCameraError3d> {
        validate_third_person_target(target)?;
        let orbit = validate_third_person_orbit(orbit, self.config)?;
        if !delta_seconds.is_finite() || delta_seconds < 0.0 {
            return Err(ThirdPersonCameraError3d::InvalidDeltaSeconds(delta_seconds));
        }
        let focus = add3(target, [0.0, self.config.target_height, 0.0]);
        let (arm_direction, desired_arm_length) = third_person_arm(self.config, orbit);
        let ray = Ray3d::new(
            PhysicsVec3::new(focus[0], focus[1], focus[2]),
            PhysicsVec3::new(arm_direction[0], arm_direction[1], arm_direction[2]),
        )
        .map_err(ThirdPersonCameraError3d::RayQuery)?;
        let ray_hit = mesh
            .raycast(ray, desired_arm_length)
            .map_err(ThirdPersonCameraError3d::RayQuery)?;
        let (collision_arm_length, mut minimum_distance_forced) = third_person_collision_arm_length(
            ray_hit.as_ref().map(|hit| hit.distance),
            desired_arm_length,
            self.config,
        );
        let (collision_arm_length, probe_steps, mut sphere_contacts, probe_forced) =
            third_person_probe_boom(
                mesh,
                focus,
                arm_direction,
                collision_arm_length,
                self.config,
            )?;
        minimum_distance_forced |= probe_forced;

        let recovery_delta =
            self.config.recovery_speed * delta_seconds.min(self.config.maximum_delta_seconds);
        let arm_length = if !self.initialized || collision_arm_length < self.current_arm_length {
            collision_arm_length
        } else {
            (self.current_arm_length + recovery_delta).min(collision_arm_length)
        };
        let candidate = add3(focus, scale3(arm_direction, arm_length));
        let resolved = mesh
            .resolve_sphere(
                PhysicsVec3::new(candidate[0], candidate[1], candidate[2]),
                self.config.probe_radius,
                self.config.collision_iterations,
            )
            .map_err(ThirdPersonCameraError3d::SphereQuery)?;
        sphere_contacts = sphere_contacts.saturating_add(resolved.contacts);
        let resolved_position = [
            resolved.position.x,
            resolved.position.y,
            resolved.position.z,
        ];
        let (position, actual_arm_length, forced_after_resolution) =
            third_person_non_degenerate_position(
                resolved_position,
                focus,
                arm_direction,
                self.config.minimum_distance,
            );
        minimum_distance_forced |= forced_after_resolution;
        let projected_arm_length = dot3(subtract3(position, focus), arm_direction)
            .clamp(self.config.minimum_distance, collision_arm_length);
        let obstructed =
            collision_arm_length + f32::EPSILON < desired_arm_length || resolved.contacts != 0;
        let recovering = !obstructed && actual_arm_length + f32::EPSILON < collision_arm_length;

        self.orbit = orbit;
        self.focus = focus;
        self.position = position;
        self.current_arm_length = projected_arm_length;
        self.initialized = true;
        Ok(ThirdPersonCameraUpdate3d {
            obstructed,
            recovering,
            desired_arm_length,
            collision_arm_length,
            actual_arm_length,
            minimum_distance_forced,
            ray_hit_triangle: ray_hit.map(|hit| hit.triangle),
            sphere_probe_steps: probe_steps,
            sphere_contacts,
        })
    }
}

/// Failure while configuring or querying a collision-aware chase camera.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ThirdPersonCameraError3d {
    /// One policy field is outside its stable finite/work bounds.
    InvalidConfig {
        /// Invalid field or related field group.
        field: &'static str,
        /// Stable validation constraint.
        reason: &'static str,
    },
    /// Gameplay target contained NaN or infinity.
    InvalidTarget,
    /// Absolute orbit contained NaN or infinity.
    InvalidOrbit,
    /// Frame delta was negative, NaN or infinite.
    InvalidDeltaSeconds(f32),
    /// Ray construction/query rejected an invariant.
    RayQuery(PhysicsConfigError),
    /// Sphere query rejected an invariant.
    SphereQuery(TriangleMeshQueryError),
}

impl fmt::Display for ThirdPersonCameraError3d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig { field, reason } => {
                write!(formatter, "invalid third-person camera {field}: {reason}")
            }
            Self::InvalidTarget => formatter.write_str("third-person camera target must be finite"),
            Self::InvalidOrbit => formatter.write_str("third-person camera orbit must be finite"),
            Self::InvalidDeltaSeconds(delta) => {
                write!(formatter, "invalid third-person camera frame delta {delta}")
            }
            Self::RayQuery(error) => write!(formatter, "third-person camera ray failed: {error}"),
            Self::SphereQuery(error) => {
                write!(
                    formatter,
                    "third-person camera sphere query failed: {error}"
                )
            }
        }
    }
}

impl Error for ThirdPersonCameraError3d {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::RayQuery(error) => Some(error),
            Self::SphereQuery(error) => Some(error),
            Self::InvalidConfig { .. }
            | Self::InvalidTarget
            | Self::InvalidOrbit
            | Self::InvalidDeltaSeconds(_) => None,
        }
    }
}

fn validate_third_person_target(target: [f32; 3]) -> Result<(), ThirdPersonCameraError3d> {
    if target.iter().all(|value| value.is_finite()) {
        Ok(())
    } else {
        Err(ThirdPersonCameraError3d::InvalidTarget)
    }
}

fn validate_third_person_orbit(
    orbit: ThirdPersonOrbit3d,
    config: ThirdPersonCameraConfig3d,
) -> Result<ThirdPersonOrbit3d, ThirdPersonCameraError3d> {
    if !orbit.yaw_radians.is_finite() || !orbit.pitch_radians.is_finite() {
        return Err(ThirdPersonCameraError3d::InvalidOrbit);
    }
    Ok(ThirdPersonOrbit3d {
        yaw_radians: orbit.yaw_radians.rem_euclid(std::f32::consts::TAU),
        pitch_radians: orbit
            .pitch_radians
            .clamp(config.minimum_pitch_radians, config.maximum_pitch_radians),
    })
}

fn third_person_arm(
    config: ThirdPersonCameraConfig3d,
    orbit: ThirdPersonOrbit3d,
) -> ([f32; 3], f32) {
    let (yaw_sin, yaw_cos) = orbit.yaw_radians.sin_cos();
    let (pitch_sin, pitch_cos) = orbit.pitch_radians.sin_cos();
    let backward = [-yaw_sin * pitch_cos, pitch_sin, yaw_cos * pitch_cos];
    let right = [yaw_cos, 0.0, yaw_sin];
    let offset = add3(
        scale3(backward, config.distance),
        scale3(right, config.shoulder_offset),
    );
    let arm_length = length3(offset);
    (scale3(offset, arm_length.recip()), arm_length)
}

fn third_person_collision_arm_length(
    ray_hit_distance: Option<f32>,
    desired_arm_length: f32,
    config: ThirdPersonCameraConfig3d,
) -> (f32, bool) {
    let Some(hit_distance) = ray_hit_distance else {
        return (desired_arm_length, false);
    };
    let available = hit_distance - config.probe_radius - config.wall_clearance;
    (
        available.clamp(config.minimum_distance, desired_arm_length),
        available < config.minimum_distance,
    )
}

fn third_person_probe_boom(
    mesh: &TriangleMesh3d,
    focus: [f32; 3],
    arm_direction: [f32; 3],
    mut arm_length: f32,
    config: ThirdPersonCameraConfig3d,
) -> Result<(f32, usize, usize, bool), ThirdPersonCameraError3d> {
    let mut steps = 1_usize;
    while steps < config.maximum_probe_steps {
        #[allow(
            clippy::cast_precision_loss,
            reason = "probe count is hard-bounded to 64 and exactly representable by f32"
        )]
        let covered_distance = config.probe_radius * steps as f32;
        if covered_distance >= arm_length {
            break;
        }
        steps += 1;
    }
    let mut contacts = 0_usize;
    let mut minimum_forced = false;
    for step in 1..=steps {
        #[allow(
            clippy::cast_precision_loss,
            reason = "probe count is hard-bounded to 64 and exactly representable by f32"
        )]
        let fraction = step as f32 / steps as f32;
        let point = add3(focus, scale3(arm_direction, arm_length * fraction));
        let resolved = mesh
            .resolve_sphere(
                PhysicsVec3::new(point[0], point[1], point[2]),
                config.probe_radius,
                config.collision_iterations,
            )
            .map_err(ThirdPersonCameraError3d::SphereQuery)?;
        contacts = contacts.saturating_add(resolved.contacts);
        if resolved.contacts != 0 {
            #[allow(
                clippy::cast_precision_loss,
                reason = "probe count is hard-bounded to 64 and exactly representable by f32"
            )]
            let previous_fraction = (step - 1) as f32 / steps as f32;
            let probed_arm_length = arm_length * previous_fraction;
            minimum_forced = probed_arm_length < config.minimum_distance;
            arm_length = probed_arm_length.max(config.minimum_distance);
            break;
        }
    }
    Ok((arm_length, steps, contacts, minimum_forced))
}

fn third_person_non_degenerate_position(
    position: [f32; 3],
    focus: [f32; 3],
    arm_direction: [f32; 3],
    minimum_distance: f32,
) -> ([f32; 3], f32, bool) {
    let actual_distance = length3(subtract3(position, focus));
    if actual_distance >= minimum_distance {
        (position, actual_distance, false)
    } else {
        (
            add3(focus, scale3(arm_direction, minimum_distance)),
            minimum_distance,
            true,
        )
    }
}

fn third_person_finite(value: f32, field: &'static str) -> Result<(), ThirdPersonCameraError3d> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(ThirdPersonCameraError3d::InvalidConfig {
            field,
            reason: "must be finite",
        })
    }
}

fn third_person_positive(value: f32, field: &'static str) -> Result<(), ThirdPersonCameraError3d> {
    if value.is_finite() && value > 0.0 {
        Ok(())
    } else {
        Err(ThirdPersonCameraError3d::InvalidConfig {
            field,
            reason: "must be finite and positive",
        })
    }
}

fn third_person_non_negative(
    value: f32,
    field: &'static str,
) -> Result<(), ThirdPersonCameraError3d> {
    if value.is_finite() && value >= 0.0 {
        Ok(())
    } else {
        Err(ThirdPersonCameraError3d::InvalidConfig {
            field,
            reason: "must be finite and non-negative",
        })
    }
}

/// Раскладка клавиш для готового свободного перемещения камеры.
///
/// Это именно физические коды клавиш, поэтому `W` означает привычное место
/// клавиши на клавиатуре, а не зависящий от раскладки текстовый символ.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FreeCameraBindings3d {
    /// Движение вперёд.
    pub forward: KeyCode,
    /// Движение назад.
    pub backward: KeyCode,
    /// Движение влево.
    pub left: KeyCode,
    /// Движение вправо.
    pub right: KeyCode,
    /// Подъём.
    pub up: KeyCode,
    /// Спуск.
    pub down: KeyCode,
    /// Временное ускорение.
    pub sprint: KeyCode,
    /// Выход из приложения. `None` оставляет решение приложению.
    pub exit: Option<KeyCode>,
}

impl Default for FreeCameraBindings3d {
    fn default() -> Self {
        Self {
            forward: KeyCode::KeyW,
            backward: KeyCode::KeyS,
            left: KeyCode::KeyA,
            right: KeyCode::KeyD,
            up: KeyCode::Space,
            down: KeyCode::ControlLeft,
            sprint: KeyCode::ShiftLeft,
            exit: Some(KeyCode::Escape),
        }
    }
}

/// Настройки высокоуровневого свободного управления 3D-камерой.
///
/// Значения измеряются в единицах мира в секунду и радианах на физический
/// пиксель мыши. По умолчанию курсор скрывается и удерживается в окне, а
/// горизонтальная ось намеренно инвертирована по текущему соглашению Yuyib.
/// Отключите [`Self::lock_cursor`], если в сцене одновременно нужна обычная
/// мышь для интерфейса.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FreeCameraConfig3d {
    /// Обычная скорость в единицах мира в секунду.
    pub move_speed: f32,
    /// Множитель скорости во время удержания [`FreeCameraBindings3d::sprint`].
    pub sprint_multiplier: f32,
    /// Чувствительность мыши в радианах на физический пиксель.
    pub mouse_sensitivity: f32,
    /// Меняет знак горизонтального поворота. По умолчанию включено.
    pub invert_x: bool,
    /// Меняет знак вертикального поворота.
    pub invert_y: bool,
    /// Скрывать и удерживать курсор, пока окно в фокусе.
    pub lock_cursor: bool,
    /// Наибольший допустимый шаг кадра. Больший шаг ограничивается этим числом.
    pub max_delta_seconds: f32,
    /// Вертикальный угол обзора создаваемой [`Camera3d`], в радианах.
    pub vertical_fov_radians: f32,
    /// Ближняя плоскость отсечения создаваемой камеры.
    pub near: f32,
    /// Дальняя плоскость отсечения создаваемой камеры.
    pub far: f32,
    /// Раскладка клавиш.
    pub bindings: FreeCameraBindings3d,
}

impl FreeCameraConfig3d {
    /// Проверяет настройки до запуска окна.
    ///
    /// # Errors
    ///
    /// Возвращает [`FreeCameraError3d::InvalidConfig`], если число не подходит
    /// для устойчивого движения или перспективной камеры.
    pub fn validate(self) -> Result<(), FreeCameraError3d> {
        positive_finite(self.move_speed, "move_speed")?;
        positive_finite(self.sprint_multiplier, "sprint_multiplier")?;
        positive_finite(self.mouse_sensitivity, "mouse_sensitivity")?;
        positive_finite(self.max_delta_seconds, "max_delta_seconds")?;
        if !self.vertical_fov_radians.is_finite()
            || self.vertical_fov_radians <= 0.0
            || self.vertical_fov_radians >= std::f32::consts::PI
        {
            return Err(FreeCameraError3d::InvalidConfig {
                field: "vertical_fov_radians",
                reason: "must be finite and between zero and pi",
            });
        }
        positive_finite(self.near, "near")?;
        positive_finite(self.far, "far")?;
        if self.far <= self.near {
            return Err(FreeCameraError3d::InvalidConfig {
                field: "far",
                reason: "must be greater than near",
            });
        }
        Ok(())
    }
}

impl Default for FreeCameraConfig3d {
    fn default() -> Self {
        Self {
            move_speed: 6.0,
            sprint_multiplier: 3.0,
            mouse_sensitivity: 0.0025,
            invert_x: false,
            invert_y: false,
            lock_cursor: true,
            max_delta_seconds: 0.1,
            vertical_fov_radians: 70.0_f32.to_radians(),
            near: 0.05,
            far: 2_000.0,
            bindings: FreeCameraBindings3d::default(),
        }
    }
}

/// Семантическое действие свободной камеры для низкоуровневого ввода.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FreeCameraAction3d {
    /// Вперёд относительно направления взгляда.
    Forward,
    /// Назад относительно направления взгляда.
    Backward,
    /// Влево относительно направления взгляда.
    Left,
    /// Вправо относительно направления взгляда.
    Right,
    /// Вверх по мировой оси Y.
    Up,
    /// Вниз по мировой оси Y.
    Down,
    /// Ускорение.
    Sprint,
}

/// Итог обработки одного события платформы камерой.
///
/// Высокоуровневый путь передаёт [`Self::cursor_control`] в
/// `WindowEventContext::set_cursor_control` и вызывает `request_exit`, если
/// [`Self::exit_requested`] равно `true`. Низкоуровневый хост может вместо
/// этого проигнорировать оба поля и управлять окном самостоятельно.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FreeCameraEvent3d {
    /// Требуемый режим курсора, если событие изменило фокус окна.
    pub cursor_control: Option<CursorControl>,
    /// Было нажато назначенное действие выхода.
    pub exit_requested: bool,
}

/// Готовое управление камерой свободного полёта.
///
/// Высокоуровневое использование принимает Winit-события через
/// [`Self::handle_window_event`] и [`Self::handle_device_event`], затем один
/// раз за кадр вызывает [`Self::step`]. При удерживаемом курсоре поворот надо
/// брать из `DeviceEvent::MouseMotion`: это относительное движение, которое
/// продолжает приходить у границы окна.
///
/// Для своего ввода доступны низкоуровневые [`Self::set_action`],
/// [`Self::add_mouse_delta`] и [`Self::step`]. Они не зависят от событий Winit.
#[derive(Clone, Debug, PartialEq)]
pub struct FreeCameraController3d {
    config: FreeCameraConfig3d,
    position: [f32; 3],
    yaw: f32,
    pitch: f32,
    held: [bool; 7],
    mouse_delta: [f32; 2],
}

impl FreeCameraController3d {
    /// Создаёт камеру в начале координат, смотрящую вдоль отрицательной Z.
    ///
    /// # Errors
    ///
    /// Возвращает ошибку при некорректных настройках.
    pub fn new(config: FreeCameraConfig3d) -> Result<Self, FreeCameraError3d> {
        config.validate()?;
        Ok(Self {
            config,
            position: [0.0, 0.0, 0.0],
            yaw: 0.0,
            pitch: 0.0,
            held: [false; 7],
            mouse_delta: [0.0, 0.0],
        })
    }

    /// Создаёт камеру из положения и точки, находящейся в центре экрана.
    ///
    /// # Errors
    ///
    /// Возвращает ошибку при неверной конфигурации, нечисловых координатах или
    /// совпадающих `position` и `target`.
    pub fn looking_at(
        config: FreeCameraConfig3d,
        position: [f32; 3],
        target: [f32; 3],
    ) -> Result<Self, FreeCameraError3d> {
        let mut controller = Self::new(config)?;
        controller.set_look_at(position, target)?;
        Ok(controller)
    }

    /// Возвращает текущие настройки.
    #[must_use]
    pub const fn config(&self) -> FreeCameraConfig3d {
        self.config
    }

    /// Заменяет настройки после проверки.
    ///
    /// # Errors
    ///
    /// Возвращает ошибку и оставляет прежние настройки без изменений.
    pub fn set_config(&mut self, config: FreeCameraConfig3d) -> Result<(), FreeCameraError3d> {
        config.validate()?;
        self.config = config;
        Ok(())
    }

    /// Возвращает текущее положение камеры.
    #[must_use]
    pub const fn position(&self) -> [f32; 3] {
        self.position
    }

    /// Устанавливает положение без изменения направления взгляда.
    ///
    /// # Errors
    ///
    /// Возвращает ошибку для NaN или бесконечных координат.
    pub fn set_position(&mut self, position: [f32; 3]) -> Result<(), FreeCameraError3d> {
        finite_vec3(position, "position")?;
        self.position = position;
        Ok(())
    }

    /// Устанавливает положение и направление через точку перед камерой.
    ///
    /// # Errors
    ///
    /// Возвращает ошибку для нечисловых данных или нулевого направления.
    pub fn set_look_at(
        &mut self,
        position: [f32; 3],
        target: [f32; 3],
    ) -> Result<(), FreeCameraError3d> {
        finite_vec3(position, "position")?;
        finite_vec3(target, "target")?;
        let direction = subtract3(target, position);
        let length = length3(direction);
        if !length.is_finite() || length <= f32::EPSILON {
            return Err(FreeCameraError3d::InvalidLookAt);
        }
        let normalized = scale3(direction, 1.0 / length);
        self.position = position;
        self.yaw = normalized[0].atan2(-normalized[2]);
        self.pitch = normalized[1].asin();
        Ok(())
    }

    /// Возвращает готовую камеру рендерера.
    #[must_use]
    pub fn camera(&self) -> Camera3d {
        Camera3d::new(
            self.position,
            add3(self.position, self.forward()),
            [0.0, 1.0, 0.0],
            self.config.vertical_fov_radians,
            self.config.near,
            self.config.far,
        )
    }

    /// Возвращает режим курсора, который следует включить при старте окна.
    #[must_use]
    pub const fn initial_cursor_control(&self) -> CursorControl {
        if self.config.lock_cursor {
            CursorControl::LockedHidden
        } else {
            CursorControl::Released
        }
    }

    /// Обрабатывает обычное оконное событие Winit.
    ///
    /// Для полного высокоуровневого управления передавайте сюда каждое событие
    /// из `Application::on_window_event`, а относительное движение мыши — в
    /// [`Self::handle_device_event`].
    #[must_use]
    pub fn handle_window_event(&mut self, event: &WindowEvent) -> FreeCameraEvent3d {
        match event {
            WindowEvent::KeyboardInput { event, .. } => {
                let PhysicalKey::Code(key) = event.physical_key else {
                    return FreeCameraEvent3d::default();
                };
                if event.state == ElementState::Pressed && self.config.bindings.exit == Some(key) {
                    return FreeCameraEvent3d {
                        cursor_control: None,
                        exit_requested: true,
                    };
                }
                if let Some(action) = self.action_for_key(key) {
                    self.set_action(action, event.state == ElementState::Pressed);
                }
                FreeCameraEvent3d::default()
            }
            WindowEvent::Focused(false) => {
                self.clear_input();
                FreeCameraEvent3d {
                    cursor_control: Some(CursorControl::Released),
                    exit_requested: false,
                }
            }
            WindowEvent::Focused(true) => FreeCameraEvent3d {
                cursor_control: Some(self.initial_cursor_control()),
                exit_requested: false,
            },
            _ => FreeCameraEvent3d::default(),
        }
    }

    /// Обрабатывает относительное движение мыши, полученное от Winit.
    #[must_use]
    pub fn handle_device_event(&mut self, event: &DeviceEvent) -> FreeCameraEvent3d {
        if let DeviceEvent::MouseMotion { delta } = event {
            self.add_mouse_delta(delta.0, delta.1);
        }
        FreeCameraEvent3d::default()
    }

    /// Низкоуровнево удерживает или отпускает действие камеры.
    pub fn set_action(&mut self, action: FreeCameraAction3d, held: bool) {
        self.held[action_index(action)] = held;
    }

    /// Низкоуровнево добавляет относительное движение мыши в физических пикселях.
    ///
    /// Нечисловые значения игнорируются, чтобы повреждённый ввод не делал
    /// камеру неиспользуемой.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "Camera and GPU contracts use f32; finite f64 deltas are clamped before conversion"
    )]
    pub fn add_mouse_delta(&mut self, x: f64, y: f64) {
        if !x.is_finite() || !y.is_finite() {
            return;
        }
        let limit = f64::from(f32::MAX);
        let x = x.clamp(-limit, limit) as f32;
        let y = y.clamp(-limit, limit) as f32;
        self.mouse_delta[0] += x;
        self.mouse_delta[1] += y;
    }

    /// Сбрасывает удерживаемые клавиши и накопленный поворот.
    pub fn clear_input(&mut self) {
        self.held = [false; 7];
        self.mouse_delta = [0.0, 0.0];
    }

    /// Применяет накопленный ввод за один кадр.
    ///
    /// Большое `delta_seconds` автоматически ограничивается настройкой камеры,
    /// чтобы возвращение после паузы окна не телепортировало игрока.
    ///
    /// # Errors
    ///
    /// Возвращает ошибку для отрицательного, NaN или бесконечного времени.
    pub fn step(&mut self, delta_seconds: f32) -> Result<(), FreeCameraError3d> {
        if !delta_seconds.is_finite() || delta_seconds < 0.0 {
            return Err(FreeCameraError3d::InvalidDeltaSeconds(delta_seconds));
        }
        let x_sign = if self.config.invert_x { -1.0 } else { 1.0 };
        let y_sign = if self.config.invert_y { 1.0 } else { -1.0 };
        self.yaw += self.mouse_delta[0] * self.config.mouse_sensitivity * x_sign;
        self.pitch += self.mouse_delta[1] * self.config.mouse_sensitivity * y_sign;
        let pitch_limit = std::f32::consts::FRAC_PI_2 - 0.001;
        self.pitch = self.pitch.clamp(-pitch_limit, pitch_limit);
        self.mouse_delta = [0.0, 0.0];

        let mut movement = [
            axis(
                self.held[action_index(FreeCameraAction3d::Right)],
                self.held[action_index(FreeCameraAction3d::Left)],
            ),
            axis(
                self.held[action_index(FreeCameraAction3d::Up)],
                self.held[action_index(FreeCameraAction3d::Down)],
            ),
            axis(
                self.held[action_index(FreeCameraAction3d::Forward)],
                self.held[action_index(FreeCameraAction3d::Backward)],
            ),
        ];
        let horizontal_forward = normalize_or_zero([self.forward()[0], 0.0, self.forward()[2]]);
        let right = [
            horizontal_forward[2].mul_add(-1.0, 0.0),
            0.0,
            horizontal_forward[0],
        ];
        let world_delta = add3(
            add3(scale3(right, movement[0]), [0.0, movement[1], 0.0]),
            scale3(horizontal_forward, movement[2]),
        );
        movement = normalize_or_zero(world_delta);
        let speed = self.config.move_speed
            * if self.held[action_index(FreeCameraAction3d::Sprint)] {
                self.config.sprint_multiplier
            } else {
                1.0
            };
        self.position = add3(
            self.position,
            scale3(
                movement,
                speed * delta_seconds.min(self.config.max_delta_seconds),
            ),
        );
        Ok(())
    }

    fn action_for_key(&self, key: KeyCode) -> Option<FreeCameraAction3d> {
        let bindings = self.config.bindings;
        if key == bindings.forward {
            Some(FreeCameraAction3d::Forward)
        } else if key == bindings.backward {
            Some(FreeCameraAction3d::Backward)
        } else if key == bindings.left {
            Some(FreeCameraAction3d::Left)
        } else if key == bindings.right {
            Some(FreeCameraAction3d::Right)
        } else if key == bindings.up {
            Some(FreeCameraAction3d::Up)
        } else if key == bindings.down {
            Some(FreeCameraAction3d::Down)
        } else if key == bindings.sprint {
            Some(FreeCameraAction3d::Sprint)
        } else {
            None
        }
    }

    fn forward(&self) -> [f32; 3] {
        let (yaw_sin, yaw_cos) = self.yaw.sin_cos();
        let (pitch_sin, pitch_cos) = self.pitch.sin_cos();
        [yaw_sin * pitch_cos, pitch_sin, -yaw_cos * pitch_cos]
    }
}

/// Ошибки настройки и шага [`FreeCameraController3d`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FreeCameraError3d {
    /// Одно из полей конфигурации не подходит для камеры.
    InvalidConfig {
        /// Имя ошибочного поля.
        field: &'static str,
        /// Краткая причина.
        reason: &'static str,
    },
    /// Положение или цель камеры содержит нечисловое значение.
    InvalidPosition {
        /// Имя ошибочного аргумента.
        field: &'static str,
    },
    /// Точка взгляда совпадает с положением камеры.
    InvalidLookAt,
    /// Время кадра отрицательно или не является числом.
    InvalidDeltaSeconds(f32),
}

impl fmt::Display for FreeCameraError3d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig { field, reason } => {
                write!(formatter, "invalid free camera setting {field}: {reason}")
            }
            Self::InvalidPosition { field } => write!(formatter, "invalid free camera {field}"),
            Self::InvalidLookAt => {
                formatter.write_str("free camera position and target must differ")
            }
            Self::InvalidDeltaSeconds(value) => write!(formatter, "invalid frame delta {value}"),
        }
    }
}

impl Error for FreeCameraError3d {}

fn positive_finite(value: f32, field: &'static str) -> Result<(), FreeCameraError3d> {
    if value.is_finite() && value > 0.0 {
        Ok(())
    } else {
        Err(FreeCameraError3d::InvalidConfig {
            field,
            reason: "must be finite and positive",
        })
    }
}

fn finite_vec3(value: [f32; 3], field: &'static str) -> Result<(), FreeCameraError3d> {
    if value.iter().all(|coordinate| coordinate.is_finite()) {
        Ok(())
    } else {
        Err(FreeCameraError3d::InvalidPosition { field })
    }
}

const fn action_index(action: FreeCameraAction3d) -> usize {
    match action {
        FreeCameraAction3d::Forward => 0,
        FreeCameraAction3d::Backward => 1,
        FreeCameraAction3d::Left => 2,
        FreeCameraAction3d::Right => 3,
        FreeCameraAction3d::Up => 4,
        FreeCameraAction3d::Down => 5,
        FreeCameraAction3d::Sprint => 6,
    }
}

fn axis(positive: bool, negative: bool) -> f32 {
    match (positive, negative) {
        (true, false) => 1.0,
        (false, true) => -1.0,
        _ => 0.0,
    }
}

fn add3(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] + right[0], left[1] + right[1], left[2] + right[2]]
}

fn subtract3(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
}

fn scale3(value: [f32; 3], scalar: f32) -> [f32; 3] {
    [value[0] * scalar, value[1] * scalar, value[2] * scalar]
}

fn length3(value: [f32; 3]) -> f32 {
    dot3(value, value).sqrt()
}

fn dot3(left: [f32; 3], right: [f32; 3]) -> f32 {
    left[0].mul_add(right[0], left[1].mul_add(right[1], left[2] * right[2]))
}

fn normalize_or_zero(value: [f32; 3]) -> [f32; 3] {
    let length = length3(value);
    if length.is_finite() && length > f32::EPSILON {
        scale3(value, 1.0 / length)
    } else {
        [0.0, 0.0, 0.0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yuyib_ui::{LayoutKind, Size, UiAction, UiBuilder, Widget, WidgetId, layout};

    fn camera_wall(z: f32) -> TriangleMesh3d {
        TriangleMesh3d::from_indexed(
            &[
                PhysicsVec3::new(-5.0, -5.0, z),
                PhysicsVec3::new(5.0, -5.0, z),
                PhysicsVec3::new(-5.0, 5.0, z),
                PhysicsVec3::new(5.0, 5.0, z),
            ],
            &[0, 1, 2, 2, 1, 3],
        )
        .expect("valid camera wall")
    }

    #[test]
    fn third_person_camera_retracts_before_a_wall() {
        let mesh = camera_wall(2.0);
        let mut camera = CollisionAwareThirdPersonCamera3d::new(
            ThirdPersonCameraConfig3d::default(),
            [0.0; 3],
            ThirdPersonOrbit3d::default(),
        )
        .expect("default chase camera");
        let update = camera
            .update([0.0; 3], ThirdPersonOrbit3d::default(), 1.0 / 60.0, &mesh)
            .expect("bounded wall query");

        assert!(update.obstructed);
        assert!(update.ray_hit_triangle.is_some());
        assert!(camera.position()[2] < 2.0 - camera.config().probe_radius);
        assert_eq!(camera.camera().target, [0.0, 1.5, 0.0]);
        assert!(update.sphere_probe_steps <= camera.config().maximum_probe_steps);
    }

    #[test]
    fn third_person_camera_never_collapses_onto_its_focus() {
        let mesh = camera_wall(0.02);
        let mut camera = CollisionAwareThirdPersonCamera3d::new(
            ThirdPersonCameraConfig3d::default(),
            [0.0; 3],
            ThirdPersonOrbit3d::default(),
        )
        .expect("default chase camera");
        let update = camera
            .update([0.0; 3], ThirdPersonOrbit3d::default(), 0.016, &mesh)
            .expect("minimum-distance fallback");

        assert!(update.minimum_distance_forced);
        assert!(update.actual_arm_length >= camera.config().minimum_distance);
        assert_ne!(camera.camera().position, camera.camera().target);
    }

    #[test]
    fn third_person_camera_recovers_distance_at_bounded_speed() {
        let blocking = camera_wall(2.0);
        let clear = camera_wall(100.0);
        let mut camera = CollisionAwareThirdPersonCamera3d::new(
            ThirdPersonCameraConfig3d::default(),
            [0.0; 3],
            ThirdPersonOrbit3d::default(),
        )
        .expect("default chase camera");
        let blocked = camera
            .update([0.0; 3], ThirdPersonOrbit3d::default(), 0.016, &blocking)
            .expect("blocked update");
        let recovering = camera
            .update([0.0; 3], ThirdPersonOrbit3d::default(), 0.1, &clear)
            .expect("clear update");

        assert!(!recovering.obstructed);
        assert!(recovering.recovering);
        assert!(recovering.actual_arm_length > blocked.actual_arm_length);
        assert!(
            recovering.actual_arm_length
                <= blocked.actual_arm_length + camera.config().recovery_speed * 0.1 + 0.001
        );
        assert!(recovering.actual_arm_length < recovering.desired_arm_length);
    }

    #[test]
    fn third_person_camera_update_is_deterministic_and_transactional() {
        let mesh = camera_wall(3.0);
        let orbit = ThirdPersonOrbit3d::new(0.4, 0.2);
        let camera = CollisionAwareThirdPersonCamera3d::new(
            ThirdPersonCameraConfig3d::default(),
            [1.0, 2.0, 3.0],
            orbit,
        )
        .expect("valid chase camera");
        let mut first = camera.clone();
        let mut second = camera;
        let first_update = first
            .update([1.5, 2.0, 3.0], orbit, 0.016, &mesh)
            .expect("first deterministic query");
        let second_update = second
            .update([1.5, 2.0, 3.0], orbit, 0.016, &mesh)
            .expect("second deterministic query");
        assert_eq!(first_update, second_update);
        assert_eq!(first, second);

        let before = first.clone();
        assert_eq!(
            first.update([f32::NAN, 0.0, 0.0], orbit, 0.016, &mesh),
            Err(ThirdPersonCameraError3d::InvalidTarget)
        );
        assert_eq!(first, before);
    }

    #[test]
    fn third_person_camera_rejects_unbounded_probe_work() {
        let invalid = ThirdPersonCameraConfig3d {
            maximum_probe_steps: MAX_THIRD_PERSON_CAMERA_PROBE_STEPS + 1,
            ..ThirdPersonCameraConfig3d::default()
        };
        assert!(matches!(
            CollisionAwareThirdPersonCamera3d::new(
                invalid,
                [0.0; 3],
                ThirdPersonOrbit3d::default(),
            ),
            Err(ThirdPersonCameraError3d::InvalidConfig {
                field: "maximum_probe_steps",
                ..
            })
        ));

        let uncovered = ThirdPersonCameraConfig3d {
            distance: 20.0,
            maximum_probe_steps: 8,
            ..ThirdPersonCameraConfig3d::default()
        };
        assert!(matches!(
            uncovered.validate(),
            Err(ThirdPersonCameraError3d::InvalidConfig {
                field: "probe_coverage",
                ..
            })
        ));

        let degenerate = ThirdPersonCameraConfig3d {
            minimum_distance: 0.01,
            near: 0.05,
            ..ThirdPersonCameraConfig3d::default()
        };
        assert!(matches!(
            degenerate.validate(),
            Err(ThirdPersonCameraError3d::InvalidConfig {
                field: "minimum_distance",
                ..
            })
        ));
    }

    #[test]
    fn free_camera_moves_at_configured_speed_without_diagonal_bonus() {
        let mut camera =
            FreeCameraController3d::new(FreeCameraConfig3d::default()).expect("default camera");
        camera.set_action(FreeCameraAction3d::Forward, true);
        camera.set_action(FreeCameraAction3d::Right, true);
        camera.step(1.0).expect("clamped camera step");

        let position = camera.position();
        let distance = (position[0].mul_add(position[0], position[2] * position[2])).sqrt();
        assert!((distance - 0.6).abs() < 0.0001);
        assert!(position[0] > 0.0);
        assert!(position[2] < 0.0);
    }

    #[test]
    fn free_camera_keeps_horizontal_mouse_axis_by_default() {
        let mut camera =
            FreeCameraController3d::new(FreeCameraConfig3d::default()).expect("default camera");
        camera.add_mouse_delta(40.0, 0.0);
        camera.step(0.0).expect("rotation step");

        assert!(camera.camera().target[0] > 0.0);
    }

    #[test]
    fn free_camera_focus_loss_releases_cursor_and_clears_movement() {
        let mut camera =
            FreeCameraController3d::new(FreeCameraConfig3d::default()).expect("default camera");
        camera.set_action(FreeCameraAction3d::Forward, true);
        let event = camera.handle_window_event(&WindowEvent::Focused(false));
        assert_eq!(event.cursor_control, Some(CursorControl::Released));

        camera.step(0.1).expect("idle step");
        assert!(
            camera
                .position()
                .iter()
                .all(|coordinate| coordinate.abs() < f32::EPSILON)
        );
    }

    #[test]
    fn free_camera_rejects_invalid_clip_planes_without_mutating_state() {
        let mut camera =
            FreeCameraController3d::new(FreeCameraConfig3d::default()).expect("default camera");
        let before = camera.config();
        let invalid = FreeCameraConfig3d {
            far: before.near,
            ..before
        };
        assert!(camera.set_config(invalid).is_err());
        assert_eq!(camera.config(), before);
    }

    fn ui_id(value: &str) -> WidgetId {
        WidgetId::from_key(value)
    }

    fn button_tree() -> UiTree {
        UiBuilder::new(ui_id("root"), LayoutKind::Column)
            .child(Widget::button(ui_id("first"), "First"))
            .child(Widget::button(ui_id("last"), "Last"))
            .build()
            .expect("test UI tree")
    }

    fn mapped_adapter() -> WinitKeyboardAdapter {
        let mut map = KeyboardActionMap::new();
        map.bind(KeyCode::KeyE, "game.use").expect("unique key");
        WinitKeyboardAdapter::new(map)
    }

    #[test]
    fn press_and_focus_loss_emit_semantic_lifecycle() {
        let mut adapter = mapped_adapter();
        let mut states = ActionStates::default();
        assert_eq!(
            adapter.handle_key_code(KeyCode::KeyE, ElementState::Pressed),
            WinitInputUpdate::KeyChanged
        );
        let pressed = adapter.emit_frame(&mut states, 10);
        assert_eq!(pressed.len(), 1);
        assert_eq!(pressed[0].action.as_str(), "game.use");
        assert_eq!(pressed[0].phase, yuyib_gameplay::ActionPhase::Started);
        assert_eq!(
            adapter.handle_window_event(&WindowEvent::Focused(false)),
            WinitInputUpdate::FocusLost
        );
        let released = adapter.emit_frame(&mut states, 11);
        assert_eq!(released.len(), 1);
        assert_eq!(released[0].phase, yuyib_gameplay::ActionPhase::Canceled);
    }

    #[test]
    fn same_action_two_keys_does_not_cancel_until_both_are_released() {
        let mut map = KeyboardActionMap::new();
        map.bind(KeyCode::KeyW, "game.forward").expect("unique");
        map.bind(KeyCode::ArrowUp, "game.forward").expect("unique");
        let mut adapter = WinitKeyboardAdapter::new(map);
        let mut states = ActionStates::default();
        let _ = adapter.handle_key_code(KeyCode::KeyW, ElementState::Pressed);
        assert_eq!(adapter.emit_frame(&mut states, 1).len(), 1);
        let _ = adapter.handle_key_code(KeyCode::ArrowUp, ElementState::Pressed);
        assert!(adapter.emit_frame(&mut states, 2).is_empty());
        let _ = adapter.handle_key_code(KeyCode::KeyW, ElementState::Released);
        assert!(adapter.emit_frame(&mut states, 3).is_empty());
        let _ = adapter.handle_key_code(KeyCode::ArrowUp, ElementState::Released);
        let released = adapter.emit_frame(&mut states, 4);
        assert_eq!(released.len(), 1);
        assert_eq!(released[0].phase, yuyib_gameplay::ActionPhase::Canceled);
    }

    #[test]
    fn emission_order_is_sorted_by_action_id() {
        let mut map = KeyboardActionMap::new();
        map.bind(KeyCode::KeyZ, "z.action").expect("unique");
        map.bind(KeyCode::KeyA, "a.action").expect("unique");
        let mut adapter = WinitKeyboardAdapter::new(map);
        let mut states = ActionStates::default();
        let _ = adapter.handle_key_code(KeyCode::KeyZ, ElementState::Pressed);
        let _ = adapter.handle_key_code(KeyCode::KeyA, ElementState::Pressed);
        let events = adapter.emit_frame(&mut states, 1);
        assert_eq!(
            events
                .iter()
                .map(|event| event.action.as_str())
                .collect::<Vec<_>>(),
            ["a.action", "z.action"]
        );
    }

    #[test]
    fn ui_adapter_buffers_pointer_events_and_emits_responses_in_arrival_order() {
        let tree = button_tree();
        let computed = layout(&tree, Size::new(160, 100)).expect("layout");
        let mut adapter = WinitUiAdapter::new(UiDpiPolicy::PhysicalPixels).expect("adapter");
        let mut state = UiInputState::default();

        assert_eq!(
            adapter
                .handle_cursor_position(PhysicalPosition::new(2.0, 2.0))
                .expect("cursor"),
            WinitUiUpdate::Buffered
        );
        assert_eq!(
            adapter.handle_mouse_button(ElementState::Pressed, MouseButton::Left),
            WinitUiUpdate::Buffered
        );
        assert_eq!(
            adapter.handle_mouse_button(ElementState::Released, MouseButton::Left),
            WinitUiUpdate::Buffered
        );

        let responses = adapter
            .emit_frame(&tree, &computed, &mut state)
            .expect("frame input");
        assert_eq!(responses.len(), 3);
        assert_eq!(responses[0].actions(), &[UiAction::Hovered(ui_id("first"))]);
        assert_eq!(responses[1].actions(), &[UiAction::Pressed(ui_id("first"))]);
        assert_eq!(responses[2].actions(), &[UiAction::Clicked(ui_id("first"))]);
        assert_eq!(adapter.pending_len(), 0);
    }

    #[test]
    fn ui_adapter_maps_shift_tab_and_activation_at_frame_boundaries() {
        let tree = button_tree();
        let computed = layout(&tree, Size::new(160, 100)).expect("layout");
        let mut adapter = WinitUiAdapter::new(UiDpiPolicy::PhysicalPixels).expect("adapter");
        let mut state = UiInputState::default();

        let _ = adapter.handle_key_code(PhysicalKey::Code(KeyCode::Tab), ElementState::Pressed);
        let first = adapter
            .emit_frame(&tree, &computed, &mut state)
            .expect("tab frame");
        assert_eq!(first[0].actions(), &[UiAction::Focused(ui_id("first"))]);

        let _ = adapter.handle_modifiers(ModifiersState::SHIFT);
        let _ = adapter.handle_key_code(PhysicalKey::Code(KeyCode::Tab), ElementState::Pressed);
        let reverse = adapter
            .emit_frame(&tree, &computed, &mut state)
            .expect("shift-tab frame");
        assert_eq!(reverse[0].actions(), &[UiAction::Focused(ui_id("last"))]);

        let _ = adapter.handle_key_code(PhysicalKey::Code(KeyCode::Enter), ElementState::Pressed);
        let activated = adapter
            .emit_frame(&tree, &computed, &mut state)
            .expect("activation frame");
        assert_eq!(
            activated[0].actions(),
            &[UiAction::Activated(ui_id("last"))]
        );
    }

    #[test]
    fn ui_adapter_focus_loss_clears_retained_state_at_next_frame_boundary() {
        let tree = button_tree();
        let computed = layout(&tree, Size::new(160, 100)).expect("layout");
        let mut adapter = WinitUiAdapter::new(UiDpiPolicy::PhysicalPixels).expect("adapter");
        let mut state = UiInputState::default();
        let _ = adapter.handle_cursor_position(PhysicalPosition::new(2.0, 2.0));
        let _ = adapter.handle_mouse_button(ElementState::Pressed, MouseButton::Left);
        let _ = adapter
            .emit_frame(&tree, &computed, &mut state)
            .expect("press");
        let _ = adapter.handle_key_code(PhysicalKey::Code(KeyCode::Tab), ElementState::Pressed);
        let _ = adapter
            .emit_frame(&tree, &computed, &mut state)
            .expect("focus");
        assert!(state.pressed().is_some());
        assert!(state.focused().is_some());

        assert_eq!(adapter.handle_focus_lost(), WinitUiUpdate::FocusLost);
        assert!(
            adapter
                .emit_frame(&tree, &computed, &mut state)
                .expect("focus loss")
                .is_empty()
        );
        assert_eq!(state.hovered(), None);
        assert_eq!(state.pressed(), None);
        assert_eq!(state.focused(), None);
    }

    #[test]
    fn ui_adapter_uses_explicit_logical_dpi_rounding_and_rejects_bad_scales() {
        assert!(matches!(
            WinitUiAdapter::new(UiDpiPolicy::LogicalPixels { scale_factor: 0.0 }),
            Err(WinitUiError::InvalidScaleFactor(0.0))
        ));
        let mut adapter = WinitUiAdapter::new(UiDpiPolicy::LogicalPixels { scale_factor: 2.0 })
            .expect("logical adapter");
        let _ = adapter
            .handle_cursor_position(PhysicalPosition::new(5.0, 7.0))
            .expect("logical cursor");
        assert_eq!(adapter.cursor, Some(Point::new(3, 4)));
    }

    #[test]
    fn ui_adapter_buffers_wheel_only_after_cursor_position() {
        let mut adapter = WinitUiAdapter::new(UiDpiPolicy::PhysicalPixels).expect("adapter");
        assert_eq!(
            adapter.handle_mouse_wheel(MouseScrollDelta::LineDelta(0.0, -1.0)),
            WinitUiUpdate::Ignored
        );
        let _ = adapter
            .handle_cursor_position(PhysicalPosition::new(2.0, 2.0))
            .expect("cursor");
        assert_eq!(
            adapter.handle_mouse_wheel(MouseScrollDelta::LineDelta(0.0, -1.0)),
            WinitUiUpdate::Buffered
        );
        assert_eq!(adapter.pending_len(), 2);
    }
}
