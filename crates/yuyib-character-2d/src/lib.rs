//! Rapier-backed 2D platformer character controller.
//!
//! Separate from the top-down [`yuyib_game_2d::KinematicSpriteController2d`]:
//! this motor applies gravity, jump (with coyote time / jump buffer), and moves a
//! kinematic capsule through [`yuyib_physics::RapierDynamicsWorld2d`] using
//! Rapier's character controller. One-way platforms are supported via
//! [`RapierDynamicsWorld2d::insert_one_way_platform_cuboid`].
//!
//! ```
//! use yuyib_character_2d::{PlatformerController2d, PlatformerControllerConfig2d, PlatformerInput2d};
//! use yuyib_physics::{DynamicsWorldConfig2d, RapierDynamicsWorld2d};
//!
//! let mut world = RapierDynamicsWorld2d::new(DynamicsWorldConfig2d::earth_60hz()).unwrap();
//! let _ground = world.insert_fixed_cuboid([0.0, -0.5], [4.0, 0.5]).unwrap();
//! let mut controller = PlatformerController2d::spawn(
//!     &mut world,
//!     PlatformerControllerConfig2d::default(),
//!     [0.0, 2.0],
//! ).unwrap();
//! let _ = controller.step(&mut world, PlatformerInput2d::neutral()).unwrap();
//! ```

#![forbid(unsafe_code)]

use std::{error::Error, fmt};

use yuyib_physics::{
    BodyId2d, CharacterMoveConfig2d, DynamicsBackend2d, DynamicsBackendError2d,
    RapierDynamicsWorld2d,
};

/// Immutable simulation parameters for [`PlatformerController2d`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlatformerControllerConfig2d {
    /// Fixed simulation interval in seconds.
    pub fixed_delta_seconds: f32,
    /// Non-positive world-space vertical acceleration (units / s²).
    pub gravity_y: f32,
    /// Maximum horizontal speed.
    pub move_speed: f32,
    /// Initial upward speed applied by a jump.
    pub jump_speed: f32,
    /// Coyote-time window after leaving ground (seconds).
    pub coyote_time: f32,
    /// Jump-buffer window before landing (seconds).
    pub jump_buffer: f32,
    /// Maximum climbable slope angle in radians.
    pub max_slope_climb_angle: f32,
    /// Capsule cylindrical half-height.
    pub half_height: f32,
    /// Capsule radius.
    pub radius: f32,
}

impl PlatformerControllerConfig2d {
    /// Validates this configuration.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformerControllerError2d::InvalidConfig`] when values are invalid.
    pub fn validate(self) -> Result<(), PlatformerControllerError2d> {
        validate_positive(self.fixed_delta_seconds, "fixed_delta_seconds")?;
        validate_finite(self.gravity_y, "gravity_y")?;
        if self.gravity_y > 0.0 {
            return Err(PlatformerControllerError2d::InvalidConfig {
                field: "gravity_y",
                reason: "must be zero or negative for a downward platformer model",
            });
        }
        validate_non_negative(self.move_speed, "move_speed")?;
        validate_non_negative(self.jump_speed, "jump_speed")?;
        validate_non_negative(self.coyote_time, "coyote_time")?;
        validate_non_negative(self.jump_buffer, "jump_buffer")?;
        validate_finite(self.max_slope_climb_angle, "max_slope_climb_angle")?;
        if self.max_slope_climb_angle < 0.0 {
            return Err(PlatformerControllerError2d::InvalidConfig {
                field: "max_slope_climb_angle",
                reason: "must be non-negative",
            });
        }
        validate_positive(self.half_height, "half_height")?;
        validate_positive(self.radius, "radius")?;
        Ok(())
    }
}

impl Default for PlatformerControllerConfig2d {
    fn default() -> Self {
        Self {
            fixed_delta_seconds: 1.0 / 60.0,
            gravity_y: -40.0,
            move_speed: 7.0,
            jump_speed: 14.0,
            coyote_time: 0.08,
            jump_buffer: 0.1,
            max_slope_climb_angle: std::f32::consts::FRAC_PI_4,
            half_height: 0.35,
            radius: 0.25,
        }
    }
}

/// Validated caller-supplied input for one fixed platformer step.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlatformerInput2d {
    /// Desired horizontal axis in `[-1, 1]`.
    pub move_x: f32,
    /// Jump requested this tick (held or pressed — buffering is handled by the controller).
    pub jump: bool,
}

impl PlatformerInput2d {
    /// No horizontal motion and no jump.
    #[must_use]
    pub const fn neutral() -> Self {
        Self {
            move_x: 0.0,
            jump: false,
        }
    }

    /// Creates validated input.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformerControllerError2d::InvalidInput`] when `move_x` is non-finite
    /// or outside `[-1, 1]`.
    pub fn new(move_x: f32, jump: bool) -> Result<Self, PlatformerControllerError2d> {
        if !move_x.is_finite() {
            return Err(PlatformerControllerError2d::InvalidInput {
                field: "move_x",
                reason: "must be finite",
            });
        }
        if !(-1.0..=1.0).contains(&move_x) {
            return Err(PlatformerControllerError2d::InvalidInput {
                field: "move_x",
                reason: "must be within [-1, 1]",
            });
        }
        Ok(Self { move_x, jump })
    }
}

/// Events emitted by one [`PlatformerController2d::step`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlatformerControllerEvent2d {
    /// Character became grounded this step.
    Landed,
    /// Character left the ground this step.
    LeftGround,
    /// A jump impulse was applied this step.
    Jumped,
}

/// Result of one platformer step.
#[derive(Clone, Debug, PartialEq)]
pub struct PlatformerStep2d {
    /// World translation after the step.
    pub translation: [f32; 2],
    /// Velocity after the step.
    pub velocity: [f32; 2],
    /// Grounded state after the step.
    pub grounded: bool,
    /// Events raised this step (stable order: LeftGround, Jumped, Landed).
    pub events: Vec<PlatformerControllerEvent2d>,
}

/// Kinematic capsule platformer driven through a Rapier 2D world.
#[derive(Clone, Debug, PartialEq)]
pub struct PlatformerController2d {
    config: PlatformerControllerConfig2d,
    body: BodyId2d,
    velocity: [f32; 2],
    grounded: bool,
    coyote_timer: f32,
    jump_buffer_timer: f32,
}

impl PlatformerController2d {
    /// Spawns a kinematic capsule at `position` and returns a controller bound to it.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformerControllerError2d`] for invalid config/position or backend failures.
    pub fn spawn(
        world: &mut RapierDynamicsWorld2d,
        config: PlatformerControllerConfig2d,
        position: [f32; 2],
    ) -> Result<Self, PlatformerControllerError2d> {
        config.validate()?;
        if position.iter().any(|channel| !channel.is_finite()) {
            return Err(PlatformerControllerError2d::InvalidInput {
                field: "position",
                reason: "must be finite",
            });
        }
        let body = world
            .insert_kinematic_position_capsule(position, config.half_height, config.radius)
            .map_err(PlatformerControllerError2d::Backend)?;
        Ok(Self {
            config,
            body,
            velocity: [0.0, 0.0],
            grounded: false,
            coyote_timer: 0.0,
            jump_buffer_timer: 0.0,
        })
    }

    /// Returns the rigid-body id owned by this controller.
    #[must_use]
    pub const fn body(&self) -> BodyId2d {
        self.body
    }

    /// Returns whether the controller considers itself grounded.
    #[must_use]
    pub const fn grounded(&self) -> bool {
        self.grounded
    }

    /// Returns the current velocity.
    #[must_use]
    pub const fn velocity(&self) -> [f32; 2] {
        self.velocity
    }

    /// Advances one fixed step against `world`.
    ///
    /// Call after moving kinematic platforms (`world.step`) when carry is needed so
    /// query geometry reflects platform motion.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformerControllerError2d`] when the backend move fails.
    pub fn step(
        &mut self,
        world: &mut RapierDynamicsWorld2d,
        input: PlatformerInput2d,
    ) -> Result<PlatformerStep2d, PlatformerControllerError2d> {
        if !input.move_x.is_finite() || !(-1.0..=1.0).contains(&input.move_x) {
            return Err(PlatformerControllerError2d::InvalidInput {
                field: "move_x",
                reason: "must be finite and within [-1, 1]",
            });
        }

        let dt = self.config.fixed_delta_seconds;
        let was_grounded = self.grounded;
        let mut events = Vec::new();

        if input.jump {
            self.jump_buffer_timer = self.config.jump_buffer;
        } else {
            self.jump_buffer_timer = (self.jump_buffer_timer - dt).max(0.0);
        }

        if self.grounded {
            self.coyote_timer = self.config.coyote_time;
        } else {
            self.coyote_timer = (self.coyote_timer - dt).max(0.0);
        }

        self.velocity[0] = input.move_x * self.config.move_speed;
        if !self.grounded {
            self.velocity[1] += self.config.gravity_y * dt;
        }

        let can_jump = self.grounded || self.coyote_timer > 0.0;
        let mut jumped = false;
        if self.jump_buffer_timer > 0.0 && can_jump {
            self.velocity[1] = self.config.jump_speed;
            self.jump_buffer_timer = 0.0;
            self.coyote_timer = 0.0;
            self.grounded = false;
            jumped = true;
            events.push(PlatformerControllerEvent2d::Jumped);
        }

        let desired = [self.velocity[0] * dt, self.velocity[1] * dt];
        let move_config = CharacterMoveConfig2d {
            max_slope_climb_angle: self.config.max_slope_climb_angle,
            snap_to_ground: self.velocity[1] <= 0.0,
            vertical_filter: self.velocity[1],
        };
        let movement = world
            .move_kinematic_character(self.body, desired, dt, move_config)
            .map_err(PlatformerControllerError2d::Backend)?;

        self.grounded = movement.grounded;
        if self.grounded && self.velocity[1] < 0.0 {
            self.velocity[1] = 0.0;
        }
        // If we hit a ceiling while rising, kill upward speed.
        if !self.grounded && self.velocity[1] > 0.0 && movement.translation[1] < desired[1] * 0.5 {
            self.velocity[1] = 0.0;
        }

        if was_grounded && !self.grounded && !jumped {
            events.insert(0, PlatformerControllerEvent2d::LeftGround);
        }
        if !was_grounded && self.grounded {
            events.push(PlatformerControllerEvent2d::Landed);
        }

        let translation = world.translation(self.body).ok_or(
            PlatformerControllerError2d::Backend(DynamicsBackendError2d::UnknownBody(self.body)),
        )?;

        Ok(PlatformerStep2d {
            translation,
            velocity: self.velocity,
            grounded: self.grounded,
            events,
        })
    }
}

/// Failure while configuring or stepping a platformer controller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlatformerControllerError2d {
    /// A configuration field failed validation.
    InvalidConfig {
        /// Field name.
        field: &'static str,
        /// Human-readable reason.
        reason: &'static str,
    },
    /// An input field failed validation.
    InvalidInput {
        /// Field name.
        field: &'static str,
        /// Human-readable reason.
        reason: &'static str,
    },
    /// Underlying dynamics backend failure.
    Backend(DynamicsBackendError2d),
}

impl fmt::Display for PlatformerControllerError2d {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig { field, reason } => {
                write!(f, "invalid platformer config `{field}`: {reason}")
            }
            Self::InvalidInput { field, reason } => {
                write!(f, "invalid platformer input `{field}`: {reason}")
            }
            Self::Backend(error) => write!(f, "platformer backend error: {error}"),
        }
    }
}

impl Error for PlatformerControllerError2d {}

fn validate_finite(value: f32, field: &'static str) -> Result<(), PlatformerControllerError2d> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(PlatformerControllerError2d::InvalidConfig {
            field,
            reason: "must be finite",
        })
    }
}

fn validate_positive(value: f32, field: &'static str) -> Result<(), PlatformerControllerError2d> {
    validate_finite(value, field)?;
    if value > 0.0 {
        Ok(())
    } else {
        Err(PlatformerControllerError2d::InvalidConfig {
            field,
            reason: "must be positive",
        })
    }
}

fn validate_non_negative(
    value: f32,
    field: &'static str,
) -> Result<(), PlatformerControllerError2d> {
    validate_finite(value, field)?;
    if value >= 0.0 {
        Ok(())
    } else {
        Err(PlatformerControllerError2d::InvalidConfig {
            field,
            reason: "must be non-negative",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PlatformerController2d, PlatformerControllerConfig2d, PlatformerControllerEvent2d,
        PlatformerInput2d,
    };
    use yuyib_physics::{DynamicsBackend2d, DynamicsWorldConfig2d, RapierDynamicsWorld2d};

    #[test]
    fn falls_lands_and_jumps() {
        let mut world =
            RapierDynamicsWorld2d::new(DynamicsWorldConfig2d::earth_60hz()).expect("world");
        let _ground = world
            .insert_fixed_cuboid([0.0, -0.5], [6.0, 0.5])
            .expect("ground");
        let mut controller = PlatformerController2d::spawn(
            &mut world,
            PlatformerControllerConfig2d::default(),
            [0.0, 3.0],
        )
        .expect("spawn");

        let mut landed = false;
        for _ in 0..180 {
            let step = controller
                .step(&mut world, PlatformerInput2d::neutral())
                .expect("fall");
            if step.events.contains(&PlatformerControllerEvent2d::Landed) {
                landed = true;
                break;
            }
        }
        assert!(landed, "should land");
        assert!(controller.grounded());

        let jump = controller
            .step(
                &mut world,
                PlatformerInput2d::new(0.0, true).expect("jump input"),
            )
            .expect("jump");
        assert!(jump.events.contains(&PlatformerControllerEvent2d::Jumped));
        assert!(!controller.grounded());
        assert!(controller.velocity()[1] > 0.0);
    }

    #[test]
    fn walks_into_wall_without_tunneling_through() {
        let mut world =
            RapierDynamicsWorld2d::new(DynamicsWorldConfig2d::earth_60hz()).expect("world");
        let _ground = world
            .insert_fixed_cuboid([0.0, -0.5], [8.0, 0.5])
            .expect("ground");
        let _wall = world
            .insert_fixed_cuboid([2.0, 1.0], [0.25, 1.0])
            .expect("wall");
        let mut controller = PlatformerController2d::spawn(
            &mut world,
            PlatformerControllerConfig2d::default(),
            [0.0, 1.0],
        )
        .expect("spawn");

        for _ in 0..60 {
            let _ = controller
                .step(&mut world, PlatformerInput2d::neutral())
                .expect("settle");
        }
        for _ in 0..120 {
            let _ = controller
                .step(
                    &mut world,
                    PlatformerInput2d::new(1.0, false).expect("run"),
                )
                .expect("run");
        }
        let x = world.translation(controller.body()).expect("pos")[0];
        assert!(x < 1.8, "should be stopped by wall, x={x}");
    }

    #[test]
    fn one_way_platform_jump_through_and_land() {
        let mut world =
            RapierDynamicsWorld2d::new(DynamicsWorldConfig2d::earth_60hz()).expect("world");
        let _ground = world
            .insert_fixed_cuboid([0.0, -0.5], [6.0, 0.5])
            .expect("ground");
        let _platform = world
            .insert_one_way_platform_cuboid([0.0, 2.5], [1.5, 0.1])
            .expect("one-way");
        let mut controller = PlatformerController2d::spawn(
            &mut world,
            PlatformerControllerConfig2d {
                jump_speed: 16.0,
                ..PlatformerControllerConfig2d::default()
            },
            [0.0, 1.0],
        )
        .expect("spawn");

        for _ in 0..90 {
            let _ = controller
                .step(&mut world, PlatformerInput2d::neutral())
                .expect("settle");
        }
        assert!(controller.grounded());

        // Jump through.
        let _ = controller
            .step(
                &mut world,
                PlatformerInput2d::new(0.0, true).expect("jump"),
            )
            .expect("jump");
        let mut above = false;
        for _ in 0..90 {
            let step = controller
                .step(&mut world, PlatformerInput2d::neutral())
                .expect("air");
            if step.translation[1] > 2.8 {
                above = true;
            }
            if above && step.grounded && step.translation[1] > 2.4 {
                return;
            }
        }
        panic!("expected to land on one-way platform after jumping through");
    }
}
