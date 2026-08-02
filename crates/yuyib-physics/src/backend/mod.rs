//! Dynamics backend facade for M4 (mature solver adapters).
//!
//! Yuyib keeps static mesh queries ([`crate::TriangleMesh3d`]) and the
//! lightweight ECS prototypes in the crate root. Dynamic rigid bodies, CCD,
//! joints and sleeping belong behind [`DynamicsBackend3d`] / [`DynamicsBackend2d`]
//! so playable character paths are not rewritten onto an in-house solver.
//!
//! Enable the `rapier` feature for the 3D adapter and `rapier2d` for the 2D adapter.

use std::fmt;

/// Opaque body identity issued by a [`DynamicsBackend3d`] implementation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct BodyId3d {
    index: u32,
    generation: u32,
}

impl BodyId3d {
    /// Creates a backend-local body id from raw parts.
    #[must_use]
    pub const fn from_raw_parts(index: u32, generation: u32) -> Self {
        Self { index, generation }
    }

    /// Returns the backend-local index part.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.index
    }

    /// Returns the backend-local generation part.
    #[must_use]
    pub const fn generation(self) -> u32 {
        self.generation
    }
}

/// Opaque body identity issued by a [`DynamicsBackend2d`] implementation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct BodyId2d {
    index: u32,
    generation: u32,
}

impl BodyId2d {
    /// Creates a backend-local body id from raw parts.
    #[must_use]
    pub const fn from_raw_parts(index: u32, generation: u32) -> Self {
        Self { index, generation }
    }

    /// Returns the backend-local index part.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.index
    }

    /// Returns the backend-local generation part.
    #[must_use]
    pub const fn generation(self) -> u32 {
        self.generation
    }
}

/// Opaque impulse-joint identity issued by a dynamics backend.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct JointId3d {
    index: u32,
    generation: u32,
}

impl JointId3d {
    /// Creates a backend-local joint id from raw parts.
    #[must_use]
    pub const fn from_raw_parts(index: u32, generation: u32) -> Self {
        Self { index, generation }
    }

    /// Returns the backend-local index part.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.index
    }

    /// Returns the backend-local generation part.
    #[must_use]
    pub const fn generation(self) -> u32 {
        self.generation
    }
}

/// Opaque impulse-joint identity issued by a 2D dynamics backend.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct JointId2d {
    index: u32,
    generation: u32,
}

impl JointId2d {
    /// Creates a backend-local joint id from raw parts.
    #[must_use]
    pub const fn from_raw_parts(index: u32, generation: u32) -> Self {
        Self { index, generation }
    }

    /// Returns the backend-local index part.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.index
    }

    /// Returns the backend-local generation part.
    #[must_use]
    pub const fn generation(self) -> u32 {
        self.generation
    }
}

/// Collision membership/filter bitmasks (Rapier `InteractionGroups` mapping).
///
/// Shared by 2D and 3D facades. Two colliders interact when each one's
/// memberships intersect the other's filter (AND mode).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CollisionGroups3d {
    /// Bitmask of layers this collider belongs to.
    pub memberships: u32,
    /// Bitmask of layers this collider can interact with.
    pub filter: u32,
}

/// Alias for 2D call sites (same bitmasks as [`CollisionGroups3d`]).
pub type CollisionGroups2d = CollisionGroups3d;

/// Options for a single kinematic character move against a 2D dynamics world.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CharacterMoveConfig2d {
    /// Maximum climbable slope angle in radians (from floor up-vector).
    pub max_slope_climb_angle: f32,
    /// When `true`, snap the character to nearby ground after horizontal motion.
    pub snap_to_ground: bool,
    /// Desired translation Y used to filter one-way platforms (`<= 0` = landing).
    ///
    /// Callers typically pass the character's vertical velocity (or desired
    /// delta Y). Jumping through a one-way platform requires a positive value.
    pub vertical_filter: f32,
}

impl CharacterMoveConfig2d {
    /// Default platformer climb (~45°) with ground snap enabled.
    #[must_use]
    pub const fn platformer() -> Self {
        Self {
            max_slope_climb_angle: std::f32::consts::FRAC_PI_4,
            snap_to_ground: true,
            vertical_filter: 0.0,
        }
    }
}

impl Default for CharacterMoveConfig2d {
    fn default() -> Self {
        Self::platformer()
    }
}

/// Result of [`RapierDynamicsWorld2d::move_kinematic_character`] (feature `rapier2d`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CharacterMoveResult2d {
    /// World-space translation applied this move.
    pub translation: [f32; 2],
    /// Whether the character is considered grounded after the move.
    pub grounded: bool,
    /// Whether the character is sliding down a steep slope.
    pub sliding_down_slope: bool,
}

impl CollisionGroups3d {
    /// Belongs to all layers and interacts with all layers.
    #[must_use]
    pub const fn all() -> Self {
        Self {
            memberships: u32::MAX,
            filter: u32::MAX,
        }
    }

    /// Creates an explicit membership/filter pair.
    #[must_use]
    pub const fn new(memberships: u32, filter: u32) -> Self {
        Self {
            memberships,
            filter,
        }
    }
}

impl Default for CollisionGroups3d {
    fn default() -> Self {
        Self::all()
    }
}

/// One active non-sensor contact between two rigid bodies.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContactPair3d {
    /// Body with the lower `(index, generation)` identity.
    pub body_a: BodyId3d,
    /// Body with the higher identity.
    pub body_b: BodyId3d,
    /// Contact normal from `body_a` toward `body_b` (unit-ish Rapier normal).
    pub normal: [f32; 3],
    /// Peak impulse magnitude on the strongest manifold contact.
    pub impulse_magnitude: f32,
}

/// One active non-sensor contact between two 2D rigid bodies.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContactPair2d {
    /// Body with the lower `(index, generation)` identity.
    pub body_a: BodyId2d,
    /// Body with the higher identity.
    pub body_b: BodyId2d,
    /// Contact normal from `body_a` toward `body_b`.
    pub normal: [f32; 2],
    /// Peak impulse magnitude on the strongest manifold contact.
    pub impulse_magnitude: f32,
}

/// Frame-delta accumulator that emits fixed simulation ticks.
///
/// Matches the Engine DoD rule that dynamics advance only on a fixed step.
/// Presentation frames call [`Self::drain_steps`] with clamped `frame_dt`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DynamicsFixedStepper3d {
    fixed_dt: f32,
    max_steps_per_frame: u32,
    accumulator: f32,
}

impl DynamicsFixedStepper3d {
    /// Creates a stepper with a positive fixed dt and step budget.
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError3d`] when `fixed_dt` is non-positive or
    /// `max_steps_per_frame` is zero.
    pub fn new(
        fixed_dt: f32,
        max_steps_per_frame: u32,
    ) -> Result<Self, DynamicsBackendError3d> {
        let fixed_dt = require_positive(fixed_dt)?;
        if max_steps_per_frame == 0 {
            return Err(DynamicsBackendError3d::NonPositiveExtent);
        }
        Ok(Self {
            fixed_dt,
            max_steps_per_frame,
            accumulator: 0.0,
        })
    }

    /// 60 Hz stepper with at most 8 catch-up ticks per frame.
    #[must_use]
    pub const fn hz60() -> Self {
        Self {
            fixed_dt: 1.0 / 60.0,
            max_steps_per_frame: 8,
            accumulator: 0.0,
        }
    }

    /// Returns the fixed timestep in seconds.
    #[must_use]
    pub const fn fixed_dt(&self) -> f32 {
        self.fixed_dt
    }

    /// Accumulates `frame_dt` (clamped to ≥ 0) and returns how many fixed steps
    /// to run. Excess time beyond `max_steps_per_frame` is dropped.
    pub fn drain_steps(&mut self, frame_dt: f32) -> u32 {
        let frame_dt = if frame_dt.is_finite() && frame_dt > 0.0 {
            frame_dt
        } else {
            0.0
        };
        self.accumulator += frame_dt;
        let mut steps = 0_u32;
        while self.accumulator >= self.fixed_dt && steps < self.max_steps_per_frame {
            self.accumulator -= self.fixed_dt;
            steps += 1;
        }
        if steps == self.max_steps_per_frame {
            self.accumulator = 0.0;
        }
        steps
    }

    /// Runs `step(Some(fixed_dt))` on `backend` for each drained tick.
    ///
    /// # Errors
    ///
    /// Propagates [`DynamicsBackend3d::step`] failures.
    pub fn step_backend(
        &mut self,
        backend: &mut impl DynamicsBackend3d,
        frame_dt: f32,
    ) -> Result<u32, DynamicsBackendError3d> {
        let steps = self.drain_steps(frame_dt);
        for _ in 0..steps {
            backend.step(Some(self.fixed_dt))?;
        }
        Ok(steps)
    }
}

/// 2D fixed-step accumulator (same policy as [`DynamicsFixedStepper3d`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DynamicsFixedStepper2d {
    fixed_dt: f32,
    max_steps_per_frame: u32,
    accumulator: f32,
}

impl DynamicsFixedStepper2d {
    /// Creates a stepper with a positive fixed dt and step budget.
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError2d`] when inputs are invalid.
    pub fn new(
        fixed_dt: f32,
        max_steps_per_frame: u32,
    ) -> Result<Self, DynamicsBackendError2d> {
        let fixed_dt = require_positive_2d(fixed_dt)?;
        if max_steps_per_frame == 0 {
            return Err(DynamicsBackendError2d::NonPositiveExtent);
        }
        Ok(Self {
            fixed_dt,
            max_steps_per_frame,
            accumulator: 0.0,
        })
    }

    /// 60 Hz stepper with at most 8 catch-up ticks per frame.
    #[must_use]
    pub const fn hz60() -> Self {
        Self {
            fixed_dt: 1.0 / 60.0,
            max_steps_per_frame: 8,
            accumulator: 0.0,
        }
    }

    /// Returns the fixed timestep in seconds.
    #[must_use]
    pub const fn fixed_dt(&self) -> f32 {
        self.fixed_dt
    }

    /// Accumulates `frame_dt` and returns how many fixed steps to run.
    pub fn drain_steps(&mut self, frame_dt: f32) -> u32 {
        let frame_dt = if frame_dt.is_finite() && frame_dt > 0.0 {
            frame_dt
        } else {
            0.0
        };
        self.accumulator += frame_dt;
        let mut steps = 0_u32;
        while self.accumulator >= self.fixed_dt && steps < self.max_steps_per_frame {
            self.accumulator -= self.fixed_dt;
            steps += 1;
        }
        if steps == self.max_steps_per_frame {
            self.accumulator = 0.0;
        }
        steps
    }

    /// Runs `step(Some(fixed_dt))` on `backend` for each drained tick.
    ///
    /// # Errors
    ///
    /// Propagates [`DynamicsBackend2d::step`] failures.
    pub fn step_backend(
        &mut self,
        backend: &mut impl DynamicsBackend2d,
        frame_dt: f32,
    ) -> Result<u32, DynamicsBackendError2d> {
        let steps = self.drain_steps(frame_dt);
        for _ in 0..steps {
            backend.step(Some(self.fixed_dt))?;
        }
        Ok(steps)
    }
}

/// Gravity and default timestep for a 3D dynamics world.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DynamicsWorldConfig3d {
    /// World gravity in metres per second squared.
    pub gravity: [f32; 3],
    /// Default fixed step length in seconds when [`DynamicsBackend3d::step`] is
    /// called with `None`.
    pub default_dt: f32,
}

impl DynamicsWorldConfig3d {
    /// Earth-like downward gravity with a 60 Hz default step.
    #[must_use]
    pub const fn earth_60hz() -> Self {
        Self {
            gravity: [0.0, -9.81, 0.0],
            default_dt: 1.0 / 60.0,
        }
    }
}

impl Default for DynamicsWorldConfig3d {
    fn default() -> Self {
        Self::earth_60hz()
    }
}

/// Gravity and default timestep for a 2D dynamics world.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DynamicsWorldConfig2d {
    /// World gravity in metres per second squared (`[x, y]`).
    pub gravity: [f32; 2],
    /// Default fixed step length in seconds when [`DynamicsBackend2d::step`] is
    /// called with `None`.
    pub default_dt: f32,
}

impl DynamicsWorldConfig2d {
    /// Platformer-style downward gravity at 60 Hz.
    #[must_use]
    pub const fn earth_60hz() -> Self {
        Self {
            gravity: [0.0, -9.81],
            default_dt: 1.0 / 60.0,
        }
    }

    /// Top-down / zero-g default at 60 Hz.
    #[must_use]
    pub const fn top_down_60hz() -> Self {
        Self {
            gravity: [0.0, 0.0],
            default_dt: 1.0 / 60.0,
        }
    }
}

impl Default for DynamicsWorldConfig2d {
    fn default() -> Self {
        Self::earth_60hz()
    }
}

/// Failure while mutating or querying a 3D dynamics backend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DynamicsBackendError3d {
    /// The body id is unknown or was removed.
    UnknownBody(BodyId3d),
    /// The joint id is unknown or was removed.
    UnknownJoint(JointId3d),
    /// A numeric input was NaN or infinite.
    NonFiniteInput,
    /// A size/radius argument was non-positive.
    NonPositiveExtent,
    /// A direction/axis vector had near-zero length.
    DegenerateAxis,
    /// Convex hull construction failed (too few / degenerate points).
    ConvexHullFailed,
    /// Triangle mesh collider construction failed.
    TrimeshFailed,
}

impl fmt::Display for DynamicsBackendError3d {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownBody(id) => write!(
                f,
                "unknown dynamics body id {}/{}",
                id.index(),
                id.generation()
            ),
            Self::UnknownJoint(id) => write!(
                f,
                "unknown dynamics joint id {}/{}",
                id.index(),
                id.generation()
            ),
            Self::NonFiniteInput => f.write_str("dynamics input must be finite"),
            Self::NonPositiveExtent => f.write_str("dynamics extent must be positive"),
            Self::DegenerateAxis => f.write_str("dynamics axis must be non-zero"),
            Self::ConvexHullFailed => f.write_str("convex hull construction failed"),
            Self::TrimeshFailed => f.write_str("triangle mesh collider construction failed"),
        }
    }
}

impl std::error::Error for DynamicsBackendError3d {}

/// Failure while mutating or querying a 2D dynamics backend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DynamicsBackendError2d {
    /// The body id is unknown or was removed.
    UnknownBody(BodyId2d),
    /// The joint id is unknown or was removed.
    UnknownJoint(JointId2d),
    /// A numeric input was NaN or infinite.
    NonFiniteInput,
    /// A size/radius argument was non-positive.
    NonPositiveExtent,
    /// A direction/axis vector had near-zero length.
    DegenerateAxis,
}

impl fmt::Display for DynamicsBackendError2d {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownBody(id) => write!(
                f,
                "unknown 2d dynamics body id {}/{}",
                id.index(),
                id.generation()
            ),
            Self::UnknownJoint(id) => write!(
                f,
                "unknown 2d dynamics joint id {}/{}",
                id.index(),
                id.generation()
            ),
            Self::NonFiniteInput => f.write_str("2d dynamics input must be finite"),
            Self::NonPositiveExtent => f.write_str("2d dynamics extent must be positive"),
            Self::DegenerateAxis => f.write_str("2d dynamics axis must be non-zero"),
        }
    }
}

impl std::error::Error for DynamicsBackendError2d {}

/// Minimal 3D dynamics backend contract used by fixed steppers and examples.
pub trait DynamicsBackend3d {
    /// Advances the simulation by `dt` seconds, or the world default when `None`.
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError3d`] for invalid timesteps.
    fn step(&mut self, dt: Option<f32>) -> Result<(), DynamicsBackendError3d>;

    /// Returns the world-space translation of `body`, if it still exists.
    fn translation(&self, body: BodyId3d) -> Option<[f32; 3]>;

    /// Returns the world-space rotation of `body` as a unit quaternion `xyzw`.
    fn rotation_xyzw(&self, body: BodyId3d) -> Option<[f32; 4]>;
}

/// Minimal 2D dynamics backend contract used by fixed steppers and examples.
pub trait DynamicsBackend2d {
    /// Advances the simulation by `dt` seconds, or the world default when `None`.
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError2d`] for invalid timesteps.
    fn step(&mut self, dt: Option<f32>) -> Result<(), DynamicsBackendError2d>;

    /// Returns the world-space translation of `body`, if it still exists.
    fn translation(&self, body: BodyId2d) -> Option<[f32; 2]>;

    /// Returns the world-space rotation angle in radians.
    fn rotation(&self, body: BodyId2d) -> Option<f32>;
}

/// Validates a finite translation/gravity vector.
#[cfg(feature = "rapier")]
pub(crate) fn require_finite3(value: [f32; 3]) -> Result<[f32; 3], DynamicsBackendError3d> {
    if value.iter().any(|channel| !channel.is_finite()) {
        Err(DynamicsBackendError3d::NonFiniteInput)
    } else {
        Ok(value)
    }
}

/// Validates a finite 2D vector.
#[cfg(feature = "rapier2d")]
pub(crate) fn require_finite2(value: [f32; 2]) -> Result<[f32; 2], DynamicsBackendError2d> {
    if value.iter().any(|channel| !channel.is_finite()) {
        Err(DynamicsBackendError2d::NonFiniteInput)
    } else {
        Ok(value)
    }
}

/// Validates a strictly positive finite extent.
pub(crate) fn require_positive(value: f32) -> Result<f32, DynamicsBackendError3d> {
    if !value.is_finite() {
        return Err(DynamicsBackendError3d::NonFiniteInput);
    }
    if value <= 0.0 {
        return Err(DynamicsBackendError3d::NonPositiveExtent);
    }
    Ok(value)
}

pub(crate) fn require_positive_2d(value: f32) -> Result<f32, DynamicsBackendError2d> {
    if !value.is_finite() {
        return Err(DynamicsBackendError2d::NonFiniteInput);
    }
    if value <= 0.0 {
        return Err(DynamicsBackendError2d::NonPositiveExtent);
    }
    Ok(value)
}

/// Validates a finite non-zero axis and returns it unchanged (caller may normalize).
#[cfg(feature = "rapier")]
pub(crate) fn require_nonzero3(value: [f32; 3]) -> Result<[f32; 3], DynamicsBackendError3d> {
    let value = require_finite3(value)?;
    let len_sq = value[0] * value[0] + value[1] * value[1] + value[2] * value[2];
    if len_sq <= f32::EPSILON * f32::EPSILON {
        return Err(DynamicsBackendError3d::DegenerateAxis);
    }
    Ok(value)
}

#[cfg(feature = "rapier")]
mod rapier_backend;

#[cfg(feature = "rapier")]
pub use rapier_backend::RapierDynamicsWorld3d;

#[cfg(feature = "rapier2d")]
mod rapier2d_backend;

#[cfg(feature = "rapier2d")]
pub use rapier2d_backend::RapierDynamicsWorld2d;

#[cfg(test)]
mod tests {
    use super::{BodyId3d, DynamicsBackendError3d, DynamicsWorldConfig2d, DynamicsWorldConfig3d};

    #[test]
    fn body_id_round_trips_raw_parts() {
        let id = BodyId3d::from_raw_parts(7, 3);
        assert_eq!(id.index(), 7);
        assert_eq!(id.generation(), 3);
    }

    #[test]
    fn earth_config_is_downward() {
        let config = DynamicsWorldConfig3d::earth_60hz();
        assert!(config.gravity[1] < 0.0);
        assert!(config.default_dt > 0.0);
    }

    #[test]
    fn top_down_2d_config_has_zero_gravity() {
        let config = DynamicsWorldConfig2d::top_down_60hz();
        assert_eq!(config.gravity, [0.0, 0.0]);
    }

    #[test]
    fn fixed_stepper_drains_bounded_ticks() {
        use super::DynamicsFixedStepper3d;

        let mut stepper = DynamicsFixedStepper3d::new(0.05, 3).expect("stepper");
        assert_eq!(stepper.drain_steps(0.2), 3);
        assert_eq!(stepper.drain_steps(0.01), 0);
    }

    #[test]
    fn error_display_mentions_extent() {
        let message = DynamicsBackendError3d::NonPositiveExtent.to_string();
        assert!(message.contains("positive"));
    }
}
