//! Dynamics backend facade for M4 (mature solver adapters).
//!
//! Yuyib keeps static mesh queries ([`crate::TriangleMesh3d`]) and the
//! lightweight ECS prototypes in the crate root. Dynamic rigid bodies, CCD,
//! joints and sleeping belong behind [`DynamicsBackend3d`] so the playable
//! character path is not rewritten onto an in-house solver.
//!
//! Enable the `rapier` feature for the first production adapter.

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

/// Collision membership/filter bitmasks (Rapier `InteractionGroups` mapping).
///
/// Two colliders interact when each one's memberships intersect the other's
/// filter (AND mode). Use [`Self::all`] for the default “collide with everything”.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CollisionGroups3d {
    /// Bitmask of layers this collider belongs to.
    pub memberships: u32,
    /// Bitmask of layers this collider can interact with.
    pub filter: u32,
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

/// Gravity and default timestep for a dynamics world.
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

/// Failure while mutating or querying a dynamics backend.
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
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownBody(id) => write!(
                formatter,
                "unknown dynamics body id {}/{}",
                id.index, id.generation
            ),
            Self::UnknownJoint(id) => write!(
                formatter,
                "unknown dynamics joint id {}/{}",
                id.index, id.generation
            ),
            Self::NonFiniteInput => formatter.write_str("dynamics input must be finite"),
            Self::NonPositiveExtent => {
                formatter.write_str("dynamics extent/radius must be strictly positive")
            }
            Self::DegenerateAxis => formatter.write_str("dynamics axis must be non-zero"),
            Self::ConvexHullFailed => {
                formatter.write_str("convex hull construction failed for the given points")
            }
            Self::TrimeshFailed => {
                formatter.write_str("triangle mesh collider construction failed")
            }
        }
    }
}

impl std::error::Error for DynamicsBackendError3d {}

/// Replaceable 3D dynamics backend (Rapier today; Avian later).
///
/// Static triangle-mesh character collision stays on [`crate::TriangleMesh3d`].
/// This trait owns **dynamic/kinematic rigid-body** simulation only.
pub trait DynamicsBackend3d {
    /// Advances the simulation by `dt` seconds, or the backend default when
    /// `dt` is `None`.
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError3d`] for invalid timesteps.
    fn step(&mut self, dt: Option<f32>) -> Result<(), DynamicsBackendError3d>;

    /// Returns the world-space translation of a body.
    fn translation(&self, body: BodyId3d) -> Option<[f32; 3]>;

    /// Returns the world-space rotation as a unit quaternion `[x, y, z, w]`.
    fn rotation_xyzw(&self, body: BodyId3d) -> Option<[f32; 4]>;
}

/// Validates a finite translation/gravity vector.
fn require_finite3(value: [f32; 3]) -> Result<[f32; 3], DynamicsBackendError3d> {
    if value.iter().any(|channel| !channel.is_finite()) {
        Err(DynamicsBackendError3d::NonFiniteInput)
    } else {
        Ok(value)
    }
}

/// Validates a strictly positive finite extent.
fn require_positive(value: f32) -> Result<f32, DynamicsBackendError3d> {
    if !value.is_finite() {
        return Err(DynamicsBackendError3d::NonFiniteInput);
    }
    if value <= 0.0 {
        return Err(DynamicsBackendError3d::NonPositiveExtent);
    }
    Ok(value)
}

/// Validates a finite non-zero axis and returns it unchanged (caller may normalize).
fn require_nonzero3(value: [f32; 3]) -> Result<[f32; 3], DynamicsBackendError3d> {
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

#[cfg(test)]
mod tests {
    use super::{BodyId3d, DynamicsBackendError3d, DynamicsWorldConfig3d};

    #[test]
    fn earth_config_is_finite() {
        let config = DynamicsWorldConfig3d::earth_60hz();
        assert!(config.gravity[1] < 0.0);
        assert!(config.default_dt > 0.0);
    }

    #[test]
    fn body_id_roundtrips_parts() {
        let id = BodyId3d::from_raw_parts(7, 3);
        assert_eq!(id.index(), 7);
        assert_eq!(id.generation(), 3);
    }

    #[test]
    fn error_display_is_stable() {
        let message = DynamicsBackendError3d::NonPositiveExtent.to_string();
        assert!(message.contains("positive"));
    }

    #[test]
    fn fixed_stepper_drains_bounded_ticks() {
        use super::DynamicsFixedStepper3d;

        let mut stepper = DynamicsFixedStepper3d::new(0.05, 3).expect("stepper");
        assert_eq!(stepper.drain_steps(0.12), 2);
        assert_eq!(stepper.drain_steps(0.01), 0);
        assert_eq!(stepper.drain_steps(1.0), 3); // clamped; remainder dropped
        assert_eq!(stepper.drain_steps(0.05), 1);
    }
}
