//! Configurable grounded player controls for Play Mode / playable hosts.
//!
//! Separates **semantic actions** ([`PlayerCharacterBindings3d`] →
//! [`KeyboardActionMap`]) from **locomotion numbers**
//! ([`PlayerCharacterControlConfig3d`]) and **mouse-look** (via
//! [`FreeCameraConfig3d`] / [`CharacterFollowCamera3d`]). Defaults match
//! common FPS/third-person expectations: `W` forward, `Space` jump, mouse look.

use winit::{
    event::WindowEvent,
    keyboard::KeyCode,
};

use yuyib_gameplay::ActionId;
use yuyib_physics::Vec2;

use crate::{
    FreeCameraBindings3d, FreeCameraConfig3d, FreeCameraController3d, KeyboardActionMap,
    KeyboardBindingError, ThirdPersonCameraConfig3d, WinitInputUpdate, WinitKeyboardAdapter,
};

/// Stable action ids for grounded player locomotion.
pub mod actions {
    /// Camera-relative forward.
    pub const MOVE_FORWARD: &str = "player.move_forward";
    /// Camera-relative backward.
    pub const MOVE_BACKWARD: &str = "player.move_backward";
    /// Camera-relative strafe left.
    pub const MOVE_LEFT: &str = "player.move_left";
    /// Camera-relative strafe right.
    pub const MOVE_RIGHT: &str = "player.move_right";
    /// Jump (edge-triggered while grounded — host applies motor rules).
    pub const JUMP: &str = "player.jump";
    /// Hold to multiply move speed by [`super::PlayerCharacterControlConfig3d::sprint_multiplier`].
    pub const SPRINT: &str = "player.sprint";
    /// Toggle first/third-person follow camera when the host wires it.
    pub const TOGGLE_VIEW: &str = "player.toggle_view";
}

/// Physical-key defaults for [`actions`]. Remap in code, then
/// [`PlayerCharacterBindings3d::keyboard_map`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlayerCharacterBindings3d {
    /// [`actions::MOVE_FORWARD`] — default `W`.
    pub forward: KeyCode,
    /// [`actions::MOVE_BACKWARD`] — default `S`.
    pub backward: KeyCode,
    /// [`actions::MOVE_LEFT`] — default `A`.
    pub left: KeyCode,
    /// [`actions::MOVE_RIGHT`] — default `D`.
    pub right: KeyCode,
    /// [`actions::JUMP`] — default `Space`.
    pub jump: KeyCode,
    /// [`actions::SPRINT`] — default left Shift.
    pub sprint: KeyCode,
    /// [`actions::TOGGLE_VIEW`] — default `V`. `None` disables the action.
    pub toggle_view: Option<KeyCode>,
}

impl Default for PlayerCharacterBindings3d {
    fn default() -> Self {
        Self {
            forward: KeyCode::KeyW,
            backward: KeyCode::KeyS,
            left: KeyCode::KeyA,
            right: KeyCode::KeyD,
            jump: KeyCode::Space,
            sprint: KeyCode::ShiftLeft,
            toggle_view: Some(KeyCode::KeyV),
        }
    }
}

impl PlayerCharacterBindings3d {
    /// Builds a [`KeyboardActionMap`] with unique physical-key ownership.
    ///
    /// # Errors
    ///
    /// Returns [`KeyboardBindingError`] when two fields share the same key.
    pub fn keyboard_map(self) -> Result<KeyboardActionMap, KeyboardBindingError> {
        let mut map = KeyboardActionMap::new();
        map.bind(self.forward, actions::MOVE_FORWARD)?;
        map.bind(self.backward, actions::MOVE_BACKWARD)?;
        map.bind(self.left, actions::MOVE_LEFT)?;
        map.bind(self.right, actions::MOVE_RIGHT)?;
        map.bind(self.jump, actions::JUMP)?;
        map.bind(self.sprint, actions::SPRINT)?;
        if let Some(toggle_view) = self.toggle_view {
            map.bind(toggle_view, actions::TOGGLE_VIEW)?;
        }
        Ok(map)
    }
}

/// Tunables for grounded Play locomotion + follow-camera look.
///
/// Copy `move_speed` / `jump_speed` / `radius` / `gravity_y` into
/// `CharacterControllerConfig3d` / `CharacterMotorConfig3d` at spawn (or after
/// [`PlayerCharacterControls3d::set_config`]). Mouse look uses
/// [`Self::look_config`] with the follow camera.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayerCharacterControlConfig3d {
    /// Horizontal speed in world units per second (fed to the character motor).
    pub move_speed: f32,
    /// Grounded jump impulse speed (fed to the character motor).
    pub jump_speed: f32,
    /// Multiplier applied to movement axes while sprint is held (host may also
    /// scale motor speed; default path scales the input vector).
    pub sprint_multiplier: f32,
    /// Character contact sphere radius.
    pub radius: f32,
    /// Vertical gravity for the motor (`≤ 0`).
    pub gravity_y: f32,
    /// Mouse look sensitivity in radians per physical pixel.
    pub mouse_sensitivity: f32,
    /// Invert horizontal look.
    pub invert_x: bool,
    /// Invert vertical look.
    pub invert_y: bool,
    /// Hide and lock the cursor while the window is focused.
    pub lock_cursor: bool,
    /// Third-person boom length.
    pub chase_distance: f32,
    /// Chase focus height above feet (added to body position).
    pub eye_height: f32,
    /// Extra chase target height offset on the boom focus.
    pub chase_target_height: f32,
    /// Camera near plane.
    pub near: f32,
    /// Camera far plane.
    pub far: f32,
    /// Keyboard remapping.
    pub bindings: PlayerCharacterBindings3d,
}

impl Default for PlayerCharacterControlConfig3d {
    fn default() -> Self {
        Self {
            move_speed: 6.0,
            jump_speed: 7.5,
            sprint_multiplier: 1.65,
            radius: 0.45,
            gravity_y: -20.0,
            mouse_sensitivity: 0.0025,
            invert_x: false,
            invert_y: false,
            lock_cursor: true,
            chase_distance: 5.5,
            eye_height: 1.4,
            chase_target_height: 0.0,
            near: 0.08,
            far: 2_000.0,
            bindings: PlayerCharacterBindings3d::default(),
        }
    }
}

impl PlayerCharacterControlConfig3d {
    /// Validates locomotion and camera numbers before constructing controls.
    ///
    /// # Errors
    ///
    /// Returns [`PlayerCharacterControlError3d`] when a field is non-finite or
    /// outside a usable range.
    pub fn validate(self) -> Result<(), PlayerCharacterControlError3d> {
        positive_finite(self.move_speed, "move_speed")?;
        positive_finite(self.jump_speed, "jump_speed")?;
        positive_finite(self.sprint_multiplier, "sprint_multiplier")?;
        positive_finite(self.radius, "radius")?;
        if !self.gravity_y.is_finite() || self.gravity_y > 0.0 {
            return Err(PlayerCharacterControlError3d::InvalidConfig {
                field: "gravity_y",
                reason: "must be finite and ≤ 0",
            });
        }
        positive_finite(self.mouse_sensitivity, "mouse_sensitivity")?;
        positive_finite(self.chase_distance, "chase_distance")?;
        finite_non_negative(self.eye_height, "eye_height")?;
        finite_non_negative(self.chase_target_height, "chase_target_height")?;
        positive_finite(self.near, "near")?;
        positive_finite(self.far, "far")?;
        if self.far <= self.near {
            return Err(PlayerCharacterControlError3d::InvalidConfig {
                field: "far",
                reason: "must be greater than near",
            });
        }
        Ok(())
    }

    /// Free-look config for [`crate::CharacterFollowCamera3d`].
    ///
    /// Movement keys on the free-look controller are unbound (look-only); Esc
    /// still requests exit. Locomotion keys live on [`Self::bindings`].
    #[must_use]
    pub fn look_config(self) -> FreeCameraConfig3d {
        FreeCameraConfig3d {
            move_speed: self.move_speed,
            sprint_multiplier: self.sprint_multiplier,
            mouse_sensitivity: self.mouse_sensitivity,
            invert_x: self.invert_x,
            invert_y: self.invert_y,
            lock_cursor: self.lock_cursor,
            near: self.near,
            far: self.far,
            bindings: FreeCameraBindings3d {
                // Look-only: avoid stealing WASD/Space from player actions.
                forward: KeyCode::F13,
                backward: KeyCode::F14,
                left: KeyCode::F15,
                right: KeyCode::F16,
                up: KeyCode::F17,
                down: KeyCode::F18,
                sprint: KeyCode::F19,
                exit: Some(KeyCode::Escape),
            },
            ..FreeCameraConfig3d::default()
        }
    }

    /// Third-person chase boom config for the follow camera.
    #[must_use]
    pub fn chase_config(self) -> ThirdPersonCameraConfig3d {
        ThirdPersonCameraConfig3d {
            distance: self.chase_distance,
            target_height: self.chase_target_height,
            near: self.near,
            far: self.far,
            ..ThirdPersonCameraConfig3d::default()
        }
    }
}

/// Validation / construction failure for player controls.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlayerCharacterControlError3d {
    /// A numeric field failed validation.
    InvalidConfig {
        /// Field name.
        field: &'static str,
        /// Human-readable reason.
        reason: &'static str,
    },
    /// Keyboard map could not be built (duplicate physical keys).
    Bindings(KeyboardBindingError),
}

impl std::fmt::Display for PlayerCharacterControlError3d {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidConfig { field, reason } => {
                write!(formatter, "player control `{field}` {reason}")
            }
            Self::Bindings(error) => write!(formatter, "player control bindings: {error}"),
        }
    }
}

impl std::error::Error for PlayerCharacterControlError3d {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Bindings(error) => Some(error),
            Self::InvalidConfig { .. } => None,
        }
    }
}

impl From<KeyboardBindingError> for PlayerCharacterControlError3d {
    fn from(value: KeyboardBindingError) -> Self {
        Self::Bindings(value)
    }
}

/// Frame-facing player keyboard state: remappable actions + jump/view edges.
pub struct PlayerCharacterControls3d {
    config: PlayerCharacterControlConfig3d,
    adapter: WinitKeyboardAdapter,
    jump_held: bool,
    jump_queued: bool,
    view_held: bool,
    view_toggle_queued: bool,
    forward_id: ActionId,
    backward_id: ActionId,
    left_id: ActionId,
    right_id: ActionId,
    jump_id: ActionId,
    sprint_id: ActionId,
    toggle_view_id: ActionId,
}

impl PlayerCharacterControls3d {
    /// Builds controls from validated config and default action ids.
    ///
    /// # Errors
    ///
    /// Returns config or binding map failures.
    pub fn new(config: PlayerCharacterControlConfig3d) -> Result<Self, PlayerCharacterControlError3d> {
        config.validate()?;
        let map = config.bindings.keyboard_map()?;
        Ok(Self {
            config,
            adapter: WinitKeyboardAdapter::new(map),
            jump_held: false,
            jump_queued: false,
            view_held: false,
            view_toggle_queued: false,
            forward_id: ActionId::new(actions::MOVE_FORWARD),
            backward_id: ActionId::new(actions::MOVE_BACKWARD),
            left_id: ActionId::new(actions::MOVE_LEFT),
            right_id: ActionId::new(actions::MOVE_RIGHT),
            jump_id: ActionId::new(actions::JUMP),
            sprint_id: ActionId::new(actions::SPRINT),
            toggle_view_id: ActionId::new(actions::TOGGLE_VIEW),
        })
    }

    /// Current control config (locomotion + look + bindings).
    #[must_use]
    pub const fn config(&self) -> PlayerCharacterControlConfig3d {
        self.config
    }

    /// Replaces config and rebuilds the keyboard map.
    ///
    /// Held keys are cleared (Winit does not replay them). Callers that need
    /// live motor retune should also rebuild / `set_config` the character body.
    ///
    /// # Errors
    ///
    /// Returns config or binding map failures.
    pub fn set_config(
        &mut self,
        config: PlayerCharacterControlConfig3d,
    ) -> Result<(), PlayerCharacterControlError3d> {
        config.validate()?;
        let map = config.bindings.keyboard_map()?;
        self.adapter.replace_map(map);
        self.config = config;
        self.jump_held = false;
        self.jump_queued = false;
        self.view_held = false;
        self.view_toggle_queued = false;
        Ok(())
    }

    /// Forwards a window event and updates jump / view edge queues.
    pub fn handle_window_event(&mut self, event: &WindowEvent) -> WinitInputUpdate {
        let update = self.adapter.handle_window_event(event);
        if matches!(
            update,
            WinitInputUpdate::KeyChanged | WinitInputUpdate::FocusLost
        ) {
            self.refresh_edges();
        }
        if matches!(update, WinitInputUpdate::FocusLost) {
            self.jump_queued = false;
            self.view_toggle_queued = false;
        }
        update
    }

    /// Forwards one decoded physical key (tests / non-Winit bridges).
    pub fn handle_key_code(
        &mut self,
        key: KeyCode,
        state: winit::event::ElementState,
    ) -> WinitInputUpdate {
        let update = self.adapter.handle_key_code(key, state);
        if matches!(
            update,
            WinitInputUpdate::KeyChanged | WinitInputUpdate::FocusLost
        ) {
            self.refresh_edges();
        }
        update
    }

    /// Local movement axes before camera projection: `x` = strafe, `y` = forward.
    ///
    /// Length is at most `1` (before host motor speed). Sprint does **not**
    /// inflate axes — use [`Self::is_sprinting`] and raise motor `move_speed`.
    #[must_use]
    pub fn movement_axes(&self) -> Vec2 {
        Vec2::new(
            f32::from(i8::from(self.adapter.is_active(&self.right_id)))
                - f32::from(i8::from(self.adapter.is_active(&self.left_id))),
            f32::from(i8::from(self.adapter.is_active(&self.forward_id)))
                - f32::from(i8::from(self.adapter.is_active(&self.backward_id))),
        )
    }

    /// Whether the sprint action is held.
    #[must_use]
    pub fn is_sprinting(&self) -> bool {
        self.adapter.is_active(&self.sprint_id)
    }

    /// Effective locomotion speed including sprint multiplier.
    #[must_use]
    pub fn effective_move_speed(&self) -> f32 {
        if self.is_sprinting() {
            self.config.move_speed * self.config.sprint_multiplier
        } else {
            self.config.move_speed
        }
    }

    /// Projects held movement into world XZ using the free-look yaw (pitch ignored).
    #[must_use]
    pub fn movement_in_camera_space(&self, look: &FreeCameraController3d) -> Vec2 {
        let view = look.camera();
        let mut forward = [
            view.target[0] - view.position[0],
            0.0,
            view.target[2] - view.position[2],
        ];
        let length = forward[0].hypot(forward[2]);
        if length <= f32::EPSILON {
            return Vec2::ZERO;
        }
        forward[0] /= length;
        forward[2] /= length;
        let right = [-forward[2], forward[0]];
        let axes = self.movement_axes();
        Vec2::new(
            right[0].mul_add(axes.x, forward[0] * axes.y),
            right[1].mul_add(axes.x, forward[2] * axes.y),
        )
    }

    /// Consumes a one-shot jump request.
    pub fn take_jump(&mut self) -> bool {
        let queued = self.jump_queued;
        self.jump_queued = false;
        queued
    }

    /// Consumes a one-shot first/third-person toggle request.
    pub fn take_view_toggle(&mut self) -> bool {
        let queued = self.view_toggle_queued;
        self.view_toggle_queued = false;
        queued
    }

    fn refresh_edges(&mut self) {
        let jump_now = self.adapter.is_active(&self.jump_id);
        if jump_now && !self.jump_held {
            self.jump_queued = true;
        }
        self.jump_held = jump_now;

        let view_now = self.adapter.is_active(&self.toggle_view_id);
        if view_now && !self.view_held {
            self.view_toggle_queued = true;
        }
        self.view_held = view_now;
    }
}

fn positive_finite(value: f32, field: &'static str) -> Result<(), PlayerCharacterControlError3d> {
    if value.is_finite() && value > 0.0 {
        Ok(())
    } else {
        Err(PlayerCharacterControlError3d::InvalidConfig {
            field,
            reason: "must be finite and > 0",
        })
    }
}

fn finite_non_negative(
    value: f32,
    field: &'static str,
) -> Result<(), PlayerCharacterControlError3d> {
    if value.is_finite() && value >= 0.0 {
        Ok(())
    } else {
        Err(PlayerCharacterControlError3d::InvalidConfig {
            field,
            reason: "must be finite and ≥ 0",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::event::ElementState;
    use winit::keyboard::KeyCode;

    #[test]
    fn default_bindings_map_wasd_space() {
        let map = PlayerCharacterBindings3d::default()
            .keyboard_map()
            .expect("defaults are unique");
        assert_eq!(map.bindings().len(), 7);
    }

    #[test]
    fn controls_queue_jump_on_space_edge() {
        let mut controls =
            PlayerCharacterControls3d::new(PlayerCharacterControlConfig3d::default())
                .expect("default config");
        controls.handle_key_code(KeyCode::Space, ElementState::Pressed);
        assert!(controls.take_jump());
        assert!(!controls.take_jump());
    }

    #[test]
    fn remapped_forward_key_drives_axes() {
        let config = PlayerCharacterControlConfig3d {
            bindings: PlayerCharacterBindings3d {
                forward: KeyCode::ArrowUp,
                ..PlayerCharacterBindings3d::default()
            },
            ..PlayerCharacterControlConfig3d::default()
        };
        let mut controls = PlayerCharacterControls3d::new(config).expect("config");
        controls.handle_key_code(KeyCode::ArrowUp, ElementState::Pressed);
        let axes = controls.movement_axes();
        assert!(axes.y > 0.0);
        assert_eq!(axes.x, 0.0);
    }

    #[test]
    fn sprint_raises_effective_move_speed_not_axes() {
        let mut controls =
            PlayerCharacterControls3d::new(PlayerCharacterControlConfig3d::default())
                .expect("default config");
        controls.handle_key_code(KeyCode::KeyW, ElementState::Pressed);
        controls.handle_key_code(KeyCode::ShiftLeft, ElementState::Pressed);
        assert!(controls.is_sprinting());
        assert!((controls.movement_axes().y - 1.0).abs() < f32::EPSILON);
        assert!(
            (controls.effective_move_speed()
                - PlayerCharacterControlConfig3d::default().move_speed
                    * PlayerCharacterControlConfig3d::default().sprint_multiplier)
                .abs()
                < 1.0e-5
        );
    }
}
