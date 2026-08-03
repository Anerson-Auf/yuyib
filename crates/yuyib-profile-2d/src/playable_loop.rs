//! Composed top-down playable loop: held move input + kinematic step + camera.
//!
//! Owns the repeated playground wiring that used to live in
//! `two_d_tile_playground`. Map/atlas spawn stays with the host; this type
//! drives input → motor → animation → camera → render against a
//! [`super::Game2dProfile`].

use std::{error::Error, fmt, time::Duration};

use yuyib_ecs::prelude::{Entity, World};
use yuyib_game_2d::{
    Game2dSceneStats, KinematicSpriteControllerError2d, KinematicSpriteMove2d, Sprite2d,
    SpriteMoveInput2d, TileKinematicAabbLimits2d, step_kinematic_sprite_controller_2d,
};
use yuyib_platform::winit::event::{ElementState, WindowEvent};
use yuyib_platform::winit::keyboard::{KeyCode, PhysicalKey};
use yuyib_render::RenderFrame;

use super::{CameraFollow2d, CameraFollowRuntime2d, Game2dProfile, Game2dProfileError};

/// Construction knobs for [`PlayableLoop2d`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayableLoopDesc2d {
    /// Actor that owns [`Sprite2d`] + kinematic controller.
    pub actor: Entity,
    /// Tile AABB query budget passed to the kinematic step.
    pub tile_limits: TileKinematicAabbLimits2d,
    /// Soft frame-delta clamp (playground default: 50 ms).
    pub max_delta: Duration,
    /// Camera follow policy applied after each step / before render.
    pub camera: CameraFollow2d,
}

impl PlayableLoopDesc2d {
    /// Builds a desc with the required actor and default clamp / follow.
    ///
    /// # Errors
    ///
    /// Returns [`PlayableLoopError2d::InvalidTileLimits`] when `max_tiles` is 0.
    pub fn new(actor: Entity, max_tiles: usize) -> Result<Self, PlayableLoopError2d> {
        Ok(Self {
            actor,
            tile_limits: TileKinematicAabbLimits2d::new(max_tiles)
                .map_err(|_| PlayableLoopError2d::InvalidTileLimits)?,
            max_delta: Duration::from_millis(50),
            camera: CameraFollow2d::new(),
        })
    }

    /// Sets the soft frame-delta clamp.
    #[must_use]
    pub const fn with_max_delta(mut self, max_delta: Duration) -> Self {
        self.max_delta = max_delta;
        self
    }

    /// Sets the camera follow policy.
    #[must_use]
    pub const fn with_camera(mut self, camera: CameraFollow2d) -> Self {
        self.camera = camera;
        self
    }

    /// Replaces the tile query budget.
    #[must_use]
    pub const fn with_tile_limits(mut self, tile_limits: TileKinematicAabbLimits2d) -> Self {
        self.tile_limits = tile_limits;
        self
    }
}

/// Engine-owned top-down playable loop (input → motor → anim → camera → draw).
pub struct PlayableLoop2d {
    actor: Entity,
    input: HeldMoveInput2d,
    /// Optional host-injected stick / network axis (right/down, typically [-1, 1]).
    external_axis: Option<[f32; 2]>,
    tile_limits: TileKinematicAabbLimits2d,
    max_delta: Duration,
    camera: CameraFollow2d,
    camera_runtime: CameraFollowRuntime2d,
    /// Last estimated actor velocity (wu/s) for look-ahead.
    look_velocity: [f32; 2],
    /// Last render surface size for viewport-aware camera bounds.
    surface_size: Option<[u32; 2]>,
}

impl PlayableLoop2d {
    /// Creates a loop from an authored desc.
    #[must_use]
    pub fn new(desc: PlayableLoopDesc2d) -> Self {
        Self {
            actor: desc.actor,
            input: HeldMoveInput2d::default(),
            external_axis: None,
            tile_limits: desc.tile_limits,
            max_delta: desc.max_delta,
            camera: desc.camera,
            camera_runtime: CameraFollowRuntime2d::new(),
            look_velocity: [0.0, 0.0],
            surface_size: None,
        }
    }

    /// Returns the driven actor entity.
    #[must_use]
    pub const fn actor(&self) -> Entity {
        self.actor
    }

    /// Returns the camera follow policy.
    #[must_use]
    pub const fn camera_follow(&self) -> CameraFollow2d {
        self.camera
    }

    /// Mutable camera follow (zoom / pan / shake trauma).
    pub fn camera_follow_mut(&mut self) -> &mut CameraFollow2d {
        &mut self.camera
    }

    /// Replaces the camera follow policy (e.g. after entering a new location).
    pub fn set_camera_follow(&mut self, camera: CameraFollow2d) {
        self.camera = camera;
        self.camera_runtime.reset();
    }

    /// Hard-cuts cinematic smoothing (next frame snaps to ideal).
    pub fn reset_camera_runtime(&mut self) {
        self.camera_runtime.reset();
    }

    /// Forwards window keyboard events into held WASD / arrow axes.
    pub fn handle_window_event(&mut self, event: &WindowEvent) {
        self.input.handle(event);
    }

    /// Injects a host-filtered move axis (gamepad stick, touch pad, network).
    ///
    /// Non-finite values are ignored. Components are clamped to `[-1, 1]`.
    /// Merged with keyboard via max-abs per axis each [`Self::step`].
    pub fn set_external_move_axis(&mut self, axis: [f32; 2]) {
        if !axis.iter().all(|v| v.is_finite()) {
            return;
        }
        self.external_axis = Some([axis[0].clamp(-1.0, 1.0), axis[1].clamp(-1.0, 1.0)]);
    }

    /// Clears any host-injected move axis (keyboard-only again).
    pub fn clear_external_move_axis(&mut self) {
        self.external_axis = None;
    }

    /// Returns the current semantic movement input (keyboard ∪ external).
    #[must_use]
    pub fn movement(&self) -> SpriteMoveInput2d {
        merge_move_input(self.input.movement(), self.external_axis)
    }

    /// Steps kinematic motion, sprite animations, and camera follow.
    ///
    /// # Errors
    ///
    /// Forwards kinematic step failures or a missing actor sprite.
    pub fn step(
        &mut self,
        profile: &mut Game2dProfile,
        delta: Duration,
    ) -> Result<KinematicSpriteMove2d, PlayableLoopError2d> {
        let delta = if self.max_delta.is_zero() {
            delta
        } else {
            delta.min(self.max_delta)
        };
        let dt = delta.as_secs_f32();
        let input = self.movement();
        let movement = step_kinematic_sprite_controller_2d(
            profile.world_mut(),
            self.actor,
            input,
            delta,
            self.tile_limits,
        )?;
        if dt.is_finite() && dt > 0.0 {
            let applied = movement.movement.applied_delta;
            self.look_velocity = [applied.x / dt, applied.y / dt];
        }
        profile.step_animations(delta);
        let _ = self.camera.tick(dt);
        self.sync_camera(profile, dt)?;
        Ok(movement)
    }

    /// Syncs the camera to the actor and renders through the profile.
    ///
    /// # Errors
    ///
    /// Returns missing-sprite or render failures.
    pub fn render(
        &mut self,
        profile: &mut Game2dProfile,
        frame: &mut RenderFrame<'_>,
    ) -> Result<Game2dSceneStats, PlayableLoopError2d> {
        self.surface_size = Some(frame.surface_size());
        self.sync_camera(profile, 0.0)?;
        profile.render(frame).map_err(PlayableLoopError2d::Profile)
    }

    fn sync_camera(
        &mut self,
        profile: &mut Game2dProfile,
        dt_seconds: f32,
    ) -> Result<(), PlayableLoopError2d> {
        let position = actor_position(profile.world(), self.actor)?;
        let camera = profile.scene_mut().camera_mut();
        self.camera.apply_cinematic(
            &mut self.camera_runtime,
            camera,
            position,
            self.look_velocity,
            dt_seconds,
            self.surface_size,
        );
        Ok(())
    }
}

fn actor_position(world: &World, actor: Entity) -> Result<[f32; 2], PlayableLoopError2d> {
    world
        .get::<Sprite2d>(actor)
        .map(|sprite| sprite.position)
        .ok_or(PlayableLoopError2d::MissingSprite(actor))
}

fn merge_move_input(
    keyboard: SpriteMoveInput2d,
    external: Option<[f32; 2]>,
) -> SpriteMoveInput2d {
    let Some(ext) = external else {
        return keyboard;
    };
    let k = keyboard.axis();
    SpriteMoveInput2d::new([max_abs(k.x, ext[0]), max_abs(k.y, ext[1])])
        .unwrap_or_else(|_| SpriteMoveInput2d::idle())
}

fn max_abs(a: f32, b: f32) -> f32 {
    if b.abs() >= a.abs() { b } else { a }
}

#[derive(Clone, Copy, Debug, Default)]
struct HeldMoveInput2d {
    horizontal: HeldAxis2d,
    vertical: HeldAxis2d,
}

#[derive(Clone, Copy, Debug, Default)]
struct HeldAxis2d {
    negative: bool,
    positive: bool,
}

impl HeldMoveInput2d {
    fn handle(&mut self, event: &WindowEvent) {
        match event {
            WindowEvent::Focused(false) => *self = Self::default(),
            WindowEvent::KeyboardInput { event, .. } => {
                let PhysicalKey::Code(key) = event.physical_key else {
                    return;
                };
                let held = event.state == ElementState::Pressed;
                match key {
                    KeyCode::KeyA | KeyCode::ArrowLeft => self.horizontal.negative = held,
                    KeyCode::KeyD | KeyCode::ArrowRight => self.horizontal.positive = held,
                    KeyCode::KeyW | KeyCode::ArrowUp => self.vertical.negative = held,
                    KeyCode::KeyS | KeyCode::ArrowDown => self.vertical.positive = held,
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn movement(self) -> SpriteMoveInput2d {
        SpriteMoveInput2d::new([
            f32::from(i8::from(self.horizontal.positive) - i8::from(self.horizontal.negative)),
            f32::from(i8::from(self.vertical.positive) - i8::from(self.vertical.negative)),
        ])
        .expect("boolean axes always produce finite SpriteMoveInput2d")
    }
}

/// Failure while constructing or driving [`PlayableLoop2d`].
#[derive(Debug)]
pub enum PlayableLoopError2d {
    /// `TileKinematicAabbLimits2d` rejected the configured budget.
    InvalidTileLimits,
    /// Actor is missing [`Sprite2d`].
    MissingSprite(Entity),
    /// Kinematic controller step failed.
    Kinematic(KinematicSpriteControllerError2d),
    /// Profile texture/render failure.
    Profile(Game2dProfileError),
}

impl fmt::Display for PlayableLoopError2d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTileLimits => {
                formatter.write_str("playable loop 2d: tile limits must be positive")
            }
            Self::MissingSprite(entity) => {
                write!(formatter, "playable loop 2d: missing Sprite2d on {entity:?}")
            }
            Self::Kinematic(error) => write!(formatter, "playable loop 2d kinematic: {error}"),
            Self::Profile(error) => write!(formatter, "playable loop 2d profile: {error}"),
        }
    }
}

impl Error for PlayableLoopError2d {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Kinematic(error) => Some(error),
            Self::Profile(error) => Some(error),
            Self::InvalidTileLimits | Self::MissingSprite(_) => None,
        }
    }
}

impl From<KinematicSpriteControllerError2d> for PlayableLoopError2d {
    fn from(value: KinematicSpriteControllerError2d) -> Self {
        Self::Kinematic(value)
    }
}

#[cfg(test)]
mod tests {
    use super::merge_move_input;
    use yuyib_game_2d::SpriteMoveInput2d;

    #[test]
    fn external_axis_wins_on_larger_abs() {
        let keyboard = SpriteMoveInput2d::new([0.2, 0.0]).expect("k");
        let merged = merge_move_input(keyboard, Some([-0.8, 0.0]));
        assert!((merged.axis().x - (-0.8)).abs() < 1e-5);
    }

    #[test]
    fn keyboard_wins_when_stick_weaker() {
        let keyboard = SpriteMoveInput2d::new([1.0, 0.0]).expect("k");
        let merged = merge_move_input(keyboard, Some([0.25, 0.0]));
        assert!((merged.axis().x - 1.0).abs() < 1e-5);
    }
}
