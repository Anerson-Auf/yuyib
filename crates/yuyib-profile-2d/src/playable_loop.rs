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

use super::{CameraFollow2d, Game2dProfile, Game2dProfileError};

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
    tile_limits: TileKinematicAabbLimits2d,
    max_delta: Duration,
    camera: CameraFollow2d,
}

impl PlayableLoop2d {
    /// Creates a loop from an authored desc.
    #[must_use]
    pub fn new(desc: PlayableLoopDesc2d) -> Self {
        Self {
            actor: desc.actor,
            input: HeldMoveInput2d::default(),
            tile_limits: desc.tile_limits,
            max_delta: desc.max_delta,
            camera: desc.camera,
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

    /// Forwards window keyboard events into held WASD / arrow axes.
    pub fn handle_window_event(&mut self, event: &WindowEvent) {
        self.input.handle(event);
    }

    /// Returns the current semantic movement input.
    #[must_use]
    pub fn movement(&self) -> SpriteMoveInput2d {
        self.input.movement()
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
        let input = self.input.movement();
        let movement = step_kinematic_sprite_controller_2d(
            profile.world_mut(),
            self.actor,
            input,
            delta,
            self.tile_limits,
        )?;
        profile.step_animations(delta);
        self.sync_camera(profile)?;
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
        self.sync_camera(profile)?;
        profile.render(frame).map_err(PlayableLoopError2d::Profile)
    }

    fn sync_camera(&self, profile: &mut Game2dProfile) -> Result<(), PlayableLoopError2d> {
        let position = actor_position(profile.world(), self.actor)?;
        self.camera
            .apply(profile.scene_mut().camera_mut(), position);
        Ok(())
    }
}

fn actor_position(world: &World, actor: Entity) -> Result<[f32; 2], PlayableLoopError2d> {
    world
        .get::<Sprite2d>(actor)
        .map(|sprite| sprite.position)
        .ok_or(PlayableLoopError2d::MissingSprite(actor))
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
