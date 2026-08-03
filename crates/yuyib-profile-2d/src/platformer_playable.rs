//! Rapier platformer playable: input → fixed motor → sprite sync → camera.
//!
//! Physics space is **Y-up** (Rapier / [`PlatformerController2d`]). Sprite and
//! camera space stay **Y-down** (same as top-down tilemaps). Conversion is
//! `sprite = [x, -y] * pixels_per_unit`.
//!
//! Enable with crate feature `character-2d` (wired from `yuyib` via
//! `character-2d`).

use std::{error::Error, fmt, time::Duration};

use yuyib_character_2d::{
    PlatformerController2d, PlatformerControllerConfig2d, PlatformerControllerError2d,
    PlatformerInput2d, PlatformerStep2d,
};
use yuyib_ecs::prelude::{Entity, World};
use yuyib_game_2d::{Game2dSceneStats, Sprite2d};
use yuyib_physics::{
    DynamicsBackendError2d, DynamicsWorldConfig2d, RapierDynamicsWorld2d,
};
use yuyib_platform::winit::event::{ElementState, WindowEvent};
use yuyib_platform::winit::keyboard::{KeyCode, PhysicalKey};
use yuyib_render::RenderFrame;

use super::{CameraFollow2d, Game2dProfile, Game2dProfileError};

const MAX_FIXED_STEPS_PER_FRAME: usize = 8;

/// Construction knobs for [`PlatformerPlayable2d`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlatformerPlayableDesc2d {
    /// Sprite entity updated from the capsule translation each fixed step.
    pub actor: Entity,
    /// Platformer motor numbers.
    pub config: PlatformerControllerConfig2d,
    /// Capsule spawn in physics Y-up space.
    pub spawn: [f32; 2],
    /// Physics-metre → sprite-pixel scale (and Y flip).
    pub pixels_per_unit: f32,
    /// Soft frame-delta clamp.
    pub max_frame_delta: Duration,
    /// Camera follow in sprite/display space.
    pub camera: CameraFollow2d,
}

impl PlatformerPlayableDesc2d {
    /// Builds a desc with default motor numbers and 32 px / metre.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformerPlayableError2d::InvalidScale`] when
    /// `pixels_per_unit` is non-finite or ≤ 0.
    pub fn new(actor: Entity, spawn: [f32; 2]) -> Result<Self, PlatformerPlayableError2d> {
        Self {
            actor,
            config: PlatformerControllerConfig2d::default(),
            spawn,
            pixels_per_unit: 32.0,
            max_frame_delta: Duration::from_millis(50),
            camera: CameraFollow2d::new(),
        }
        .validated()
    }

    /// Sets motor configuration.
    #[must_use]
    pub const fn with_config(mut self, config: PlatformerControllerConfig2d) -> Self {
        self.config = config;
        self
    }

    /// Sets physics→sprite scale.
    #[must_use]
    pub const fn with_pixels_per_unit(mut self, pixels_per_unit: f32) -> Self {
        self.pixels_per_unit = pixels_per_unit;
        self
    }

    /// Sets the soft frame-delta clamp.
    #[must_use]
    pub const fn with_max_frame_delta(mut self, max_frame_delta: Duration) -> Self {
        self.max_frame_delta = max_frame_delta;
        self
    }

    /// Sets camera follow policy (applied in sprite/display space).
    #[must_use]
    pub const fn with_camera(mut self, camera: CameraFollow2d) -> Self {
        self.camera = camera;
        self
    }

    fn validated(self) -> Result<Self, PlatformerPlayableError2d> {
        if !self.pixels_per_unit.is_finite() || self.pixels_per_unit <= 0.0 {
            return Err(PlatformerPlayableError2d::InvalidScale);
        }
        if self.spawn.iter().any(|channel| !channel.is_finite()) {
            return Err(PlatformerPlayableError2d::InvalidSpawn);
        }
        Ok(self)
    }
}

/// Engine-owned Rapier platformer loop with sprite/camera sync.
pub struct PlatformerPlayable2d {
    dynamics: RapierDynamicsWorld2d,
    controller: PlatformerController2d,
    actor: Entity,
    input: HeldPlatformerInput2d,
    pixels_per_unit: f32,
    fixed_delta_seconds: f32,
    max_frame_delta: Duration,
    camera: CameraFollow2d,
    fixed_accumulator_seconds: f32,
}

impl PlatformerPlayable2d {
    /// Creates an empty Rapier world, spawns the capsule, and wires the loop.
    ///
    /// Insert solids through [`Self::dynamics_mut`] before the first
    /// [`Self::step`] (ground, walls, one-way platforms).
    ///
    /// # Errors
    ///
    /// Returns dynamics / controller / desc validation failures.
    pub fn spawn(desc: PlatformerPlayableDesc2d) -> Result<Self, PlatformerPlayableError2d> {
        let desc = desc.validated()?;
        let mut dynamics = RapierDynamicsWorld2d::new(DynamicsWorldConfig2d::earth_60hz())
            .map_err(PlatformerPlayableError2d::Dynamics)?;
        let fixed_delta_seconds = desc.config.fixed_delta_seconds;
        let controller =
            PlatformerController2d::spawn(&mut dynamics, desc.config, desc.spawn)?;
        Ok(Self {
            dynamics,
            controller,
            actor: desc.actor,
            input: HeldPlatformerInput2d::default(),
            pixels_per_unit: desc.pixels_per_unit,
            fixed_delta_seconds,
            max_frame_delta: desc.max_frame_delta,
            camera: desc.camera,
            fixed_accumulator_seconds: 0.0,
        })
    }

    /// Returns the driven sprite entity.
    #[must_use]
    pub const fn actor(&self) -> Entity {
        self.actor
    }

    /// Returns whether the capsule is grounded.
    #[must_use]
    pub const fn grounded(&self) -> bool {
        self.controller.grounded()
    }

    /// Escape hatch for inserting solids / kinematic platforms.
    pub const fn dynamics_mut(&mut self) -> &mut RapierDynamicsWorld2d {
        &mut self.dynamics
    }

    /// Returns the Rapier world.
    #[must_use]
    pub const fn dynamics(&self) -> &RapierDynamicsWorld2d {
        &self.dynamics
    }

    /// Escape hatch for the motor.
    pub const fn controller_mut(&mut self) -> &mut PlatformerController2d {
        &mut self.controller
    }

    /// Converts physics Y-up metres to sprite Y-down pixels.
    #[must_use]
    pub fn physics_to_sprite(&self, translation: [f32; 2]) -> [f32; 2] {
        physics_to_sprite(translation, self.pixels_per_unit)
    }

    /// Forwards window keyboard events (A/D or arrows + Space).
    pub fn handle_window_event(&mut self, event: &WindowEvent) {
        self.input.handle(event);
    }

    /// Accumulates frame time, runs fixed motor steps, syncs sprite + camera.
    ///
    /// # Errors
    ///
    /// Forwards motor / missing-sprite failures.
    pub fn step(
        &mut self,
        profile: &mut Game2dProfile,
        frame_delta: Duration,
    ) -> Result<Option<PlatformerStep2d>, PlatformerPlayableError2d> {
        let frame_delta = if self.max_frame_delta.is_zero() {
            frame_delta
        } else {
            frame_delta.min(self.max_frame_delta)
        };
        let frame_seconds = frame_delta.as_secs_f32();
        if !frame_seconds.is_finite() || frame_seconds < 0.0 {
            return Ok(None);
        }

        let fixed = self.fixed_delta_seconds;
        self.fixed_accumulator_seconds = (self.fixed_accumulator_seconds + frame_seconds)
            .min(fixed * MAX_FIXED_STEPS_PER_FRAME as f32);

        let mut last = None;
        let mut first = true;
        for _ in 0..MAX_FIXED_STEPS_PER_FRAME {
            if self.fixed_accumulator_seconds < fixed {
                break;
            }
            let input = self.input.platformer_input(first)?;
            first = false;
            let step = self.controller.step(&mut self.dynamics, input)?;
            sync_sprite(
                profile.world_mut(),
                self.actor,
                step.translation,
                self.pixels_per_unit,
            )?;
            last = Some(step);
            self.fixed_accumulator_seconds -= fixed;
        }

        profile.step_animations(frame_delta);
        self.sync_camera(profile)?;
        Ok(last)
    }

    /// Syncs camera from the actor sprite and renders through the profile.
    ///
    /// # Errors
    ///
    /// Returns missing-sprite or render failures.
    pub fn render(
        &mut self,
        profile: &mut Game2dProfile,
        frame: &mut RenderFrame<'_>,
    ) -> Result<Game2dSceneStats, PlatformerPlayableError2d> {
        self.sync_camera(profile)?;
        profile
            .render(frame)
            .map_err(PlatformerPlayableError2d::Profile)
    }

    fn sync_camera(&self, profile: &mut Game2dProfile) -> Result<(), PlatformerPlayableError2d> {
        let position = profile
            .world()
            .get::<Sprite2d>(self.actor)
            .map(|sprite| sprite.position)
            .ok_or(PlatformerPlayableError2d::MissingSprite(self.actor))?;
        self.camera
            .apply(profile.scene_mut().camera_mut(), position);
        Ok(())
    }
}

/// Converts physics Y-up metres to sprite Y-down pixels.
#[must_use]
pub fn physics_to_sprite(translation: [f32; 2], pixels_per_unit: f32) -> [f32; 2] {
    [
        translation[0] * pixels_per_unit,
        -translation[1] * pixels_per_unit,
    ]
}

fn sync_sprite(
    world: &mut World,
    actor: Entity,
    translation: [f32; 2],
    pixels_per_unit: f32,
) -> Result<(), PlatformerPlayableError2d> {
    let mut sprite = world
        .get_mut::<Sprite2d>(actor)
        .ok_or(PlatformerPlayableError2d::MissingSprite(actor))?;
    sprite.position = physics_to_sprite(translation, pixels_per_unit);
    Ok(())
}

#[derive(Clone, Copy, Debug, Default)]
struct HeldPlatformerInput2d {
    left: bool,
    right: bool,
    jump_held: bool,
    jump_pressed: bool,
}

impl HeldPlatformerInput2d {
    fn handle(&mut self, event: &WindowEvent) {
        match event {
            WindowEvent::Focused(false) => *self = Self::default(),
            WindowEvent::KeyboardInput { event, .. } => {
                let PhysicalKey::Code(key) = event.physical_key else {
                    return;
                };
                let held = event.state == ElementState::Pressed;
                match key {
                    KeyCode::KeyA | KeyCode::ArrowLeft => self.left = held,
                    KeyCode::KeyD | KeyCode::ArrowRight => self.right = held,
                    KeyCode::Space => {
                        if held && !event.repeat {
                            self.jump_pressed = true;
                        }
                        self.jump_held = held;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn platformer_input(
        &mut self,
        consume_jump_edge: bool,
    ) -> Result<PlatformerInput2d, PlatformerPlayableError2d> {
        let move_x = f32::from(i8::from(self.right) - i8::from(self.left));
        let jump = if consume_jump_edge {
            let pressed = self.jump_pressed;
            self.jump_pressed = false;
            pressed || self.jump_held
        } else {
            self.jump_held
        };
        PlatformerInput2d::new(move_x, jump).map_err(PlatformerPlayableError2d::Controller)
    }
}

/// Failure while constructing or driving [`PlatformerPlayable2d`].
#[derive(Debug)]
pub enum PlatformerPlayableError2d {
    /// `pixels_per_unit` invalid.
    InvalidScale,
    /// Spawn position non-finite.
    InvalidSpawn,
    /// Actor missing [`Sprite2d`].
    MissingSprite(Entity),
    /// Dynamics world construction / query failure.
    Dynamics(DynamicsBackendError2d),
    /// Platformer motor failure.
    Controller(PlatformerControllerError2d),
    /// Profile render failure.
    Profile(Game2dProfileError),
}

impl fmt::Display for PlatformerPlayableError2d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidScale => {
                formatter.write_str("platformer playable: pixels_per_unit must be finite and > 0")
            }
            Self::InvalidSpawn => {
                formatter.write_str("platformer playable: spawn must be finite")
            }
            Self::MissingSprite(entity) => {
                write!(
                    formatter,
                    "platformer playable: missing Sprite2d on {entity:?}"
                )
            }
            Self::Dynamics(error) => write!(formatter, "platformer playable dynamics: {error}"),
            Self::Controller(error) => {
                write!(formatter, "platformer playable controller: {error}")
            }
            Self::Profile(error) => write!(formatter, "platformer playable profile: {error}"),
        }
    }
}

impl Error for PlatformerPlayableError2d {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Dynamics(error) => Some(error),
            Self::Controller(error) => Some(error),
            Self::Profile(error) => Some(error),
            Self::InvalidScale | Self::InvalidSpawn | Self::MissingSprite(_) => None,
        }
    }
}

impl From<PlatformerControllerError2d> for PlatformerPlayableError2d {
    fn from(value: PlatformerControllerError2d) -> Self {
        Self::Controller(value)
    }
}
