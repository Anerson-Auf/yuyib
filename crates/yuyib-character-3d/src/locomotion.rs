//! Deterministic renderer/importer-neutral eight-way locomotion selection.
//!
//! The controller consumes camera-relative movement where positive X is camera
//! right and positive Y is camera forward. It does not read input devices,
//! rotate a character transform, advance animation time, or depend on a glTF
//! importer. Clip keys are generic, so a game can use a glTF clip index, an
//! asset handle, or its own animation-state identifier.

use std::{error::Error, fmt};

use yuyib_physics::Vec2;

const OCTANT_RADIANS: f32 = std::f32::consts::FRAC_PI_4;
const FULL_TURN_RADIANS: f32 = std::f32::consts::TAU;
const DIAGONAL_COMPONENT: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// One of eight camera-relative horizontal movement directions.
///
/// Discriminants follow clockwise octants starting at camera forward. This
/// stable order is also used by [`LocomotionClipSet8`].
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum LocomotionDirection8 {
    /// Positive camera-forward input.
    #[default]
    Forward = 0,
    /// Equal camera-forward and camera-right input.
    ForwardRight = 1,
    /// Positive camera-right input.
    Right = 2,
    /// Equal camera-backward and camera-right input.
    BackwardRight = 3,
    /// Negative camera-forward input.
    Backward = 4,
    /// Equal camera-backward and camera-left input.
    BackwardLeft = 5,
    /// Negative camera-right input.
    Left = 6,
    /// Equal camera-forward and camera-left input.
    ForwardLeft = 7,
}

impl LocomotionDirection8 {
    /// Returns the normalized camera-relative direction vector.
    #[must_use]
    pub const fn unit_vector(self) -> Vec2 {
        match self {
            Self::Forward => Vec2::new(0.0, 1.0),
            Self::ForwardRight => Vec2::new(DIAGONAL_COMPONENT, DIAGONAL_COMPONENT),
            Self::Right => Vec2::new(1.0, 0.0),
            Self::BackwardRight => Vec2::new(DIAGONAL_COMPONENT, -DIAGONAL_COMPONENT),
            Self::Backward => Vec2::new(0.0, -1.0),
            Self::BackwardLeft => Vec2::new(-DIAGONAL_COMPONENT, -DIAGONAL_COMPONENT),
            Self::Left => Vec2::new(-1.0, 0.0),
            Self::ForwardLeft => Vec2::new(-DIAGONAL_COMPONENT, DIAGONAL_COMPONENT),
        }
    }

    /// Returns clockwise yaw in radians from camera forward.
    #[must_use]
    pub const fn radians_from_forward(self) -> f32 {
        self as u8 as f32 * OCTANT_RADIANS
    }

    const fn from_octant(octant: u8) -> Self {
        match octant & 7 {
            0 => Self::Forward,
            1 => Self::ForwardRight,
            2 => Self::Right,
            3 => Self::BackwardRight,
            4 => Self::Backward,
            5 => Self::BackwardLeft,
            6 => Self::Left,
            _ => Self::ForwardLeft,
        }
    }

    const fn index(self) -> usize {
        self as usize
    }
}

/// Optional directional clips with one required walking fallback.
///
/// Every absent moving-direction clip resolves to `fallback_walk`. This makes
/// a one-animation prototype valid while allowing cardinal and diagonal clips
/// to be added independently. Idle is optional: `None` tells the application
/// to pause or retain its own idle policy rather than pretending the walk clip
/// is an idle animation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocomotionClipSet8<C> {
    fallback_walk: C,
    idle: Option<C>,
    directional: [Option<C>; 8],
}

impl<C: Copy> LocomotionClipSet8<C> {
    /// Creates a clip set where every moving direction uses `fallback_walk`.
    #[must_use]
    pub fn new(fallback_walk: C) -> Self {
        Self {
            fallback_walk,
            idle: None,
            directional: std::array::from_fn(|_| None),
        }
    }

    /// Selects an optional dedicated idle clip.
    #[must_use]
    pub const fn with_idle(mut self, clip: C) -> Self {
        self.idle = Some(clip);
        self
    }

    /// Selects a dedicated clip for one direction.
    #[must_use]
    pub const fn with_direction(mut self, direction: LocomotionDirection8, clip: C) -> Self {
        self.directional[direction.index()] = Some(clip);
        self
    }

    /// Selects a dedicated forward clip.
    #[must_use]
    pub const fn with_forward(self, clip: C) -> Self {
        self.with_direction(LocomotionDirection8::Forward, clip)
    }

    /// Selects a dedicated forward-right clip.
    #[must_use]
    pub const fn with_forward_right(self, clip: C) -> Self {
        self.with_direction(LocomotionDirection8::ForwardRight, clip)
    }

    /// Selects a dedicated right clip.
    #[must_use]
    pub const fn with_right(self, clip: C) -> Self {
        self.with_direction(LocomotionDirection8::Right, clip)
    }

    /// Selects a dedicated backward-right clip.
    #[must_use]
    pub const fn with_backward_right(self, clip: C) -> Self {
        self.with_direction(LocomotionDirection8::BackwardRight, clip)
    }

    /// Selects a dedicated backward clip.
    #[must_use]
    pub const fn with_backward(self, clip: C) -> Self {
        self.with_direction(LocomotionDirection8::Backward, clip)
    }

    /// Selects a dedicated backward-left clip.
    #[must_use]
    pub const fn with_backward_left(self, clip: C) -> Self {
        self.with_direction(LocomotionDirection8::BackwardLeft, clip)
    }

    /// Selects a dedicated left clip.
    #[must_use]
    pub const fn with_left(self, clip: C) -> Self {
        self.with_direction(LocomotionDirection8::Left, clip)
    }

    /// Selects a dedicated forward-left clip.
    #[must_use]
    pub const fn with_forward_left(self, clip: C) -> Self {
        self.with_direction(LocomotionDirection8::ForwardLeft, clip)
    }

    /// Returns the required fallback walking clip.
    #[must_use]
    pub const fn fallback_walk(&self) -> C {
        self.fallback_walk
    }

    /// Returns the dedicated idle clip, if configured.
    #[must_use]
    pub const fn idle(&self) -> Option<C> {
        self.idle
    }

    /// Returns the optional clip explicitly assigned to `direction`.
    #[must_use]
    pub const fn directional(&self, direction: LocomotionDirection8) -> Option<C> {
        self.directional[direction.index()]
    }

    const fn resolve_moving(&self, direction: LocomotionDirection8) -> C {
        match self.directional(direction) {
            Some(clip) => clip,
            None => self.fallback_walk,
        }
    }
}

/// Validated parameters for [`LocomotionController8`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LocomotionControllerConfig8 {
    dead_zone: f32,
}

impl LocomotionControllerConfig8 {
    /// Creates a camera-relative movement configuration.
    ///
    /// # Errors
    ///
    /// Returns [`LocomotionControllerError8::InvalidDeadZone`] unless
    /// `dead_zone` is finite and in `0.0..1.0`.
    pub fn new(dead_zone: f32) -> Result<Self, LocomotionControllerError8> {
        if !dead_zone.is_finite() || !(0.0..1.0).contains(&dead_zone) {
            return Err(LocomotionControllerError8::InvalidDeadZone);
        }
        Ok(Self { dead_zone })
    }

    /// Returns the radial input threshold below which the state is idle.
    #[must_use]
    pub const fn dead_zone(self) -> f32 {
        self.dead_zone
    }
}

impl Default for LocomotionControllerConfig8 {
    fn default() -> Self {
        Self { dead_zone: 0.15 }
    }
}

/// Coarse locomotion phase retained by [`LocomotionController8`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LocomotionState8 {
    /// Input is inside the configured radial dead zone.
    #[default]
    Idle,
    /// Input requests horizontal movement.
    Moving,
}

/// Deterministic state/facing transition produced by one update.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LocomotionTransition8 {
    /// State and quantized facing did not change.
    #[default]
    Unchanged,
    /// Input left the dead zone.
    StartedMoving,
    /// Input entered the dead zone; the previous facing is retained.
    StoppedMoving,
    /// Moving input crossed a nearest-octant boundary.
    ChangedDirection {
        /// Previous nearest direction.
        from: LocomotionDirection8,
        /// New nearest direction.
        to: LocomotionDirection8,
    },
}

/// One importer-neutral animation selection produced by the controller.
///
/// While moving, `secondary_weight` blends clockwise from `primary_direction`
/// to `secondary_direction`. It is zero when the input lies exactly on one
/// octant or both directions resolve to the same fallback clip. At idle,
/// `primary_clip` is the optional idle clip and the secondary fields are empty.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LocomotionAnimationSample8<C> {
    state: LocomotionState8,
    transition: LocomotionTransition8,
    facing: LocomotionDirection8,
    movement_amount: f32,
    primary_direction: LocomotionDirection8,
    primary_clip: Option<C>,
    secondary_direction: Option<LocomotionDirection8>,
    secondary_clip: Option<C>,
    secondary_weight: f32,
}

impl<C: Copy> LocomotionAnimationSample8<C> {
    /// Returns the coarse idle/moving state.
    #[must_use]
    pub const fn state(self) -> LocomotionState8 {
        self.state
    }

    /// Returns the transition observed by this update.
    #[must_use]
    pub const fn transition(self) -> LocomotionTransition8 {
        self.transition
    }

    /// Returns nearest-octant facing, retained while idle.
    #[must_use]
    pub const fn facing(self) -> LocomotionDirection8 {
        self.facing
    }

    /// Returns dead-zone-adjusted movement strength in `0.0..=1.0`.
    #[must_use]
    pub const fn movement_amount(self) -> f32 {
        self.movement_amount
    }

    /// Returns the counter-clockwise/lower octant of the blend interval.
    #[must_use]
    pub const fn primary_direction(self) -> LocomotionDirection8 {
        self.primary_direction
    }

    /// Returns the primary moving clip or optional idle clip.
    #[must_use]
    pub const fn primary_clip(self) -> Option<C> {
        self.primary_clip
    }

    /// Returns the clockwise/upper octant when two distinct clips are blended.
    #[must_use]
    pub const fn secondary_direction(self) -> Option<LocomotionDirection8> {
        self.secondary_direction
    }

    /// Returns the second distinct moving clip, if one is needed.
    #[must_use]
    pub const fn secondary_clip(self) -> Option<C> {
        self.secondary_clip
    }

    /// Returns the contribution of `secondary_clip` in `0.0..1.0`.
    #[must_use]
    pub const fn secondary_weight(self) -> f32 {
        self.secondary_weight
    }
}

/// Stateful deterministic eight-way locomotion animation selector.
///
/// The controller owns only phase and facing memory. Animation playback time,
/// cross-fade duration and root motion remain caller policy. This separation
/// lets the same controller drive glTF, sprite, custom skeletal, or headless
/// gameplay state. Clip storage is a fixed eight-element array and
/// [`Self::update`] performs no allocation.
///
/// ```
/// use yuyib_character_3d::{
///     LocomotionClipSet8, LocomotionController8, LocomotionDirection8,
/// };
/// use yuyib_physics::Vec2;
///
/// // The clip key can also be a glTF AnimationClipIndex or an asset handle.
/// let clips = LocomotionClipSet8::new(10_u32).with_right(11);
/// let mut locomotion = LocomotionController8::default();
/// let sample = locomotion.update(Vec2::new(1.0, 0.0), &clips)?;
/// assert_eq!(sample.facing(), LocomotionDirection8::Right);
/// assert_eq!(sample.primary_clip(), Some(11));
/// # Ok::<(), yuyib_character_3d::LocomotionControllerError8>(())
/// ```
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LocomotionController8 {
    config: LocomotionControllerConfig8,
    state: LocomotionState8,
    facing: LocomotionDirection8,
}

impl LocomotionController8 {
    /// Creates an idle controller facing camera-forward.
    #[must_use]
    pub const fn new(config: LocomotionControllerConfig8) -> Self {
        Self {
            config,
            state: LocomotionState8::Idle,
            facing: LocomotionDirection8::Forward,
        }
    }

    /// Creates an idle controller with an explicit initial facing.
    #[must_use]
    pub const fn with_initial_facing(
        config: LocomotionControllerConfig8,
        facing: LocomotionDirection8,
    ) -> Self {
        Self {
            config,
            state: LocomotionState8::Idle,
            facing,
        }
    }

    /// Returns the current idle/moving state.
    #[must_use]
    pub const fn state(self) -> LocomotionState8 {
        self.state
    }

    /// Returns the nearest movement direction, retained while idle.
    #[must_use]
    pub const fn facing(self) -> LocomotionDirection8 {
        self.facing
    }

    /// Resets phase and facing without touching any external animation player.
    pub fn reset(&mut self, facing: LocomotionDirection8) {
        self.state = LocomotionState8::Idle;
        self.facing = facing;
    }

    /// Resolves one camera-relative movement sample into animation clips.
    ///
    /// Values longer than one are normalized and report full movement amount.
    /// Missing direction clips independently resolve to the required fallback
    /// walk clip. If both blend endpoints resolve to that same clip, the output
    /// is collapsed to one clip with zero secondary weight.
    ///
    /// # Errors
    ///
    /// Returns [`LocomotionControllerError8::InvalidMovement`] for NaN,
    /// infinity, or a finite vector whose squared length overflows.
    pub fn update<C: Copy + Eq>(
        &mut self,
        movement: Vec2,
        clips: &LocomotionClipSet8<C>,
    ) -> Result<LocomotionAnimationSample8<C>, LocomotionControllerError8> {
        if !movement.x.is_finite() || !movement.y.is_finite() {
            return Err(LocomotionControllerError8::InvalidMovement);
        }
        let length_squared = movement.length_squared();
        if !length_squared.is_finite() {
            return Err(LocomotionControllerError8::InvalidMovement);
        }
        let magnitude = length_squared.sqrt();
        if magnitude <= self.config.dead_zone {
            let transition = if self.state == LocomotionState8::Moving {
                LocomotionTransition8::StoppedMoving
            } else {
                LocomotionTransition8::Unchanged
            };
            self.state = LocomotionState8::Idle;
            return Ok(LocomotionAnimationSample8 {
                state: self.state,
                transition,
                facing: self.facing,
                movement_amount: 0.0,
                primary_direction: self.facing,
                primary_clip: clips.idle(),
                secondary_direction: None,
                secondary_clip: None,
                secondary_weight: 0.0,
            });
        }

        let inverse_magnitude = magnitude.recip();
        let normalized = Vec2::new(
            movement.x * inverse_magnitude,
            movement.y * inverse_magnitude,
        );
        let angle = normalized
            .x
            .atan2(normalized.y)
            .rem_euclid(FULL_TURN_RADIANS);
        let octant_position = (angle / OCTANT_RADIANS).rem_euclid(8.0);
        let lower_octant = floor_octant(octant_position);
        let blend = octant_position - f32::from(lower_octant);
        let upper_octant = lower_octant.wrapping_add(1) & 7;
        let lower_direction = LocomotionDirection8::from_octant(lower_octant);
        let upper_direction = LocomotionDirection8::from_octant(upper_octant);
        let facing = LocomotionDirection8::from_octant(floor_octant(
            (octant_position + 0.5).rem_euclid(8.0),
        ));
        let transition = match self.state {
            LocomotionState8::Idle => LocomotionTransition8::StartedMoving,
            LocomotionState8::Moving if facing != self.facing => {
                LocomotionTransition8::ChangedDirection {
                    from: self.facing,
                    to: facing,
                }
            }
            LocomotionState8::Moving => LocomotionTransition8::Unchanged,
        };
        self.state = LocomotionState8::Moving;
        self.facing = facing;

        let primary_clip = clips.resolve_moving(lower_direction);
        let secondary_clip = clips.resolve_moving(upper_direction);
        let distinct_blend = blend > f32::EPSILON && primary_clip != secondary_clip;
        let movement_amount = ((magnitude.min(1.0) - self.config.dead_zone)
            / (1.0 - self.config.dead_zone))
            .clamp(0.0, 1.0);
        Ok(LocomotionAnimationSample8 {
            state: self.state,
            transition,
            facing,
            movement_amount,
            primary_direction: lower_direction,
            primary_clip: Some(primary_clip),
            secondary_direction: distinct_blend.then_some(upper_direction),
            secondary_clip: distinct_blend.then_some(secondary_clip),
            secondary_weight: if distinct_blend { blend } else { 0.0 },
        })
    }
}

impl Default for LocomotionController8 {
    fn default() -> Self {
        Self::new(LocomotionControllerConfig8::default())
    }
}

/// Smooth world-space horizontal facing used by locomotion-driven models.
///
/// The direction uses [`Vec2::x`] as world X and [`Vec2::y`] as world Z. Each
/// update follows the shortest angular arc and advances by at most
/// `turn_speed_radians_per_second * delta_seconds`. Keeping smoothing separate
/// from [`LocomotionController8`] is intentional: clip selection is
/// camera-relative, while a rendered model normally retains world-space yaw.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LocomotionFacingSmoother {
    direction: Vec2,
    turn_speed_radians_per_second: f32,
}

impl LocomotionFacingSmoother {
    /// Creates a facing state from a finite non-zero direction and positive
    /// angular speed.
    ///
    /// # Errors
    ///
    /// Returns [`LocomotionFacingError`] for invalid or unsafe values.
    pub fn new(
        initial_direction: Vec2,
        turn_speed_radians_per_second: f32,
    ) -> Result<Self, LocomotionFacingError> {
        if !turn_speed_radians_per_second.is_finite() || turn_speed_radians_per_second <= 0.0 {
            return Err(LocomotionFacingError::InvalidTurnSpeed);
        }
        Ok(Self {
            direction: normalize_facing(initial_direction)?,
            turn_speed_radians_per_second,
        })
    }

    /// Returns the current normalized world XZ direction.
    #[must_use]
    pub const fn direction(self) -> Vec2 {
        self.direction
    }

    /// Returns the maximum angular speed in radians per second.
    #[must_use]
    pub const fn turn_speed_radians_per_second(self) -> f32 {
        self.turn_speed_radians_per_second
    }

    /// Advances toward `target_direction` along the shortest yaw arc.
    ///
    /// A large but finite frame delta snaps at most one half-turn and cannot
    /// overshoot the target. Exactly opposite directions deterministically
    /// choose the negative half-turn.
    ///
    /// # Errors
    ///
    /// Returns [`LocomotionFacingError::InvalidDirection`] for a non-finite or
    /// zero target, and [`LocomotionFacingError::InvalidDeltaSeconds`] for a
    /// negative or non-finite delta.
    pub fn update(
        &mut self,
        target_direction: Vec2,
        delta_seconds: f32,
    ) -> Result<Vec2, LocomotionFacingError> {
        if !delta_seconds.is_finite() || delta_seconds < 0.0 {
            return Err(LocomotionFacingError::InvalidDeltaSeconds);
        }
        let target = normalize_facing(target_direction)?;
        let current_yaw = self.direction.x.atan2(self.direction.y);
        let target_yaw = target.x.atan2(target.y);
        let delta = (target_yaw - current_yaw + std::f32::consts::PI).rem_euclid(FULL_TURN_RADIANS)
            - std::f32::consts::PI;
        let turn_budget = self.turn_speed_radians_per_second * delta_seconds;
        let maximum_step = if turn_budget.is_finite() {
            turn_budget.min(std::f32::consts::PI)
        } else {
            std::f32::consts::PI
        };
        if delta.abs() <= maximum_step {
            self.direction = target;
        } else {
            let yaw = current_yaw + delta.signum() * maximum_step;
            self.direction = Vec2::new(yaw.sin(), yaw.cos());
        }
        Ok(self.direction)
    }
}

/// Invalid smooth-facing configuration or frame input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocomotionFacingError {
    /// Angular speed was non-finite, zero, or negative.
    InvalidTurnSpeed,
    /// A direction was non-finite, zero, or too large to normalize safely.
    InvalidDirection,
    /// Frame delta was negative or non-finite.
    InvalidDeltaSeconds,
}

impl fmt::Display for LocomotionFacingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTurnSpeed => {
                formatter.write_str("locomotion turn speed must be finite and positive")
            }
            Self::InvalidDirection => formatter.write_str(
                "locomotion facing direction must be finite, non-zero and safely normalizable",
            ),
            Self::InvalidDeltaSeconds => {
                formatter.write_str("locomotion facing delta must be finite and non-negative")
            }
        }
    }
}

impl Error for LocomotionFacingError {}

fn normalize_facing(direction: Vec2) -> Result<Vec2, LocomotionFacingError> {
    if !direction.x.is_finite() || !direction.y.is_finite() {
        return Err(LocomotionFacingError::InvalidDirection);
    }
    let length_squared = direction.length_squared();
    if !length_squared.is_finite() || length_squared <= f32::EPSILON {
        return Err(LocomotionFacingError::InvalidDirection);
    }
    Ok(direction * length_squared.sqrt().recip())
}

/// Invalid eight-way locomotion configuration or input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocomotionControllerError8 {
    /// The radial dead zone was non-finite, negative, or at least one.
    InvalidDeadZone,
    /// Camera-relative movement was non-finite or too large to normalize safely.
    InvalidMovement,
}

impl fmt::Display for LocomotionControllerError8 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDeadZone => {
                formatter.write_str("locomotion dead zone must be finite and in 0.0..1.0")
            }
            Self::InvalidMovement => formatter.write_str(
                "camera-relative locomotion movement must be finite and safely normalizable",
            ),
        }
    }
}

impl Error for LocomotionControllerError8 {}

fn floor_octant(position: f32) -> u8 {
    debug_assert!(position.is_finite() && (0.0..8.0).contains(&position));
    if position < 1.0 {
        0
    } else if position < 2.0 {
        1
    } else if position < 3.0 {
        2
    } else if position < 4.0 {
        3
    } else if position < 5.0 {
        4
    } else if position < 6.0 {
        5
    } else if position < 7.0 {
        6
    } else {
        7
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Clip {
        Idle,
        Walk,
        Forward,
        Right,
        Backward,
    }

    fn clips() -> LocomotionClipSet8<Clip> {
        LocomotionClipSet8::new(Clip::Walk)
            .with_idle(Clip::Idle)
            .with_forward(Clip::Forward)
            .with_right(Clip::Right)
            .with_backward(Clip::Backward)
    }

    #[test]
    fn all_octants_quantize_in_clockwise_order() {
        let mut controller = LocomotionController8::default();
        for direction in [
            LocomotionDirection8::Forward,
            LocomotionDirection8::ForwardRight,
            LocomotionDirection8::Right,
            LocomotionDirection8::BackwardRight,
            LocomotionDirection8::Backward,
            LocomotionDirection8::BackwardLeft,
            LocomotionDirection8::Left,
            LocomotionDirection8::ForwardLeft,
        ] {
            let sample = controller
                .update(direction.unit_vector(), &clips())
                .expect("unit input is valid");
            assert_eq!(sample.state(), LocomotionState8::Moving);
            assert_eq!(sample.facing(), direction);
            assert_eq!(sample.primary_direction(), direction);
            assert_eq!(sample.secondary_clip(), None);
            assert_eq!(sample.secondary_weight(), 0.0);
        }
    }

    #[test]
    fn angle_between_octants_blends_adjacent_distinct_clips() {
        let mut controller = LocomotionController8::default();
        let angle = OCTANT_RADIANS * 0.5;
        let movement = Vec2::new(angle.sin(), angle.cos());
        let sample = controller
            .update(movement, &clips())
            .expect("finite unit input");

        assert_eq!(sample.primary_direction(), LocomotionDirection8::Forward);
        assert_eq!(sample.primary_clip(), Some(Clip::Forward));
        assert_eq!(
            sample.secondary_direction(),
            Some(LocomotionDirection8::ForwardRight)
        );
        assert_eq!(sample.secondary_clip(), Some(Clip::Walk));
        assert!((sample.secondary_weight() - 0.5).abs() < 0.000_01);
    }

    #[test]
    fn absent_direction_clips_collapse_to_one_walk_fallback() {
        let fallback_only = LocomotionClipSet8::new(Clip::Walk);
        let mut controller = LocomotionController8::default();
        let sample = controller
            .update(Vec2::new(-0.3, -0.8), &fallback_only)
            .expect("finite input");

        assert_eq!(sample.primary_clip(), Some(Clip::Walk));
        assert_eq!(sample.secondary_clip(), None);
        assert_eq!(sample.secondary_weight(), 0.0);
    }

    #[test]
    fn idle_retains_last_moving_facing_and_selects_idle_clip() {
        let mut controller = LocomotionController8::default();
        let moving = controller
            .update(Vec2::new(-1.0, 0.0), &clips())
            .expect("left input is valid");
        assert_eq!(moving.transition(), LocomotionTransition8::StartedMoving);
        assert_eq!(moving.facing(), LocomotionDirection8::Left);

        let idle = controller
            .update(Vec2::ZERO, &clips())
            .expect("idle input is valid");
        assert_eq!(idle.state(), LocomotionState8::Idle);
        assert_eq!(idle.transition(), LocomotionTransition8::StoppedMoving);
        assert_eq!(idle.facing(), LocomotionDirection8::Left);
        assert_eq!(idle.primary_direction(), LocomotionDirection8::Left);
        assert_eq!(idle.primary_clip(), Some(Clip::Idle));
    }

    #[test]
    fn direction_changes_are_reported_only_across_nearest_octants() {
        let mut controller = LocomotionController8::default();
        controller
            .update(Vec2::new(0.0, 1.0), &clips())
            .expect("forward input");
        let same_octant = controller
            .update(Vec2::new(0.1, 1.0), &clips())
            .expect("near-forward input");
        assert_eq!(same_octant.transition(), LocomotionTransition8::Unchanged);

        let changed = controller
            .update(Vec2::new(1.0, 0.0), &clips())
            .expect("right input");
        assert_eq!(
            changed.transition(),
            LocomotionTransition8::ChangedDirection {
                from: LocomotionDirection8::Forward,
                to: LocomotionDirection8::Right,
            }
        );
    }

    #[test]
    fn dead_zone_rescales_analogue_strength() {
        let config = LocomotionControllerConfig8::new(0.2).expect("valid dead zone");
        let mut controller = LocomotionController8::new(config);
        let idle = controller
            .update(Vec2::new(0.2, 0.0), &clips())
            .expect("threshold input");
        assert_eq!(idle.state(), LocomotionState8::Idle);

        let half = controller
            .update(Vec2::new(0.6, 0.0), &clips())
            .expect("analogue input");
        assert!((half.movement_amount() - 0.5).abs() < 0.000_01);
        let full = controller
            .update(Vec2::new(10.0, 0.0), &clips())
            .expect("long finite input is normalized");
        assert_eq!(full.movement_amount(), 1.0);
    }

    #[test]
    fn invalid_configuration_and_movement_are_rejected() {
        assert_eq!(
            LocomotionControllerConfig8::new(1.0),
            Err(LocomotionControllerError8::InvalidDeadZone)
        );
        let mut controller = LocomotionController8::default();
        assert_eq!(
            controller.update(Vec2::new(f32::NAN, 0.0), &clips()),
            Err(LocomotionControllerError8::InvalidMovement)
        );
        assert_eq!(
            controller.update(Vec2::new(f32::MAX, f32::MAX), &clips()),
            Err(LocomotionControllerError8::InvalidMovement)
        );
    }

    #[test]
    fn smooth_facing_turns_at_a_bounded_angular_speed_without_overshoot() {
        let mut facing = LocomotionFacingSmoother::new(Vec2::new(0.0, 1.0), std::f32::consts::PI)
            .expect("forward direction and half-turn per second are valid");

        let halfway = facing
            .update(Vec2::new(1.0, 0.0), 0.25)
            .expect("quarter-second frame is valid");
        assert!((halfway.x - DIAGONAL_COMPONENT).abs() < 0.000_01);
        assert!((halfway.y - DIAGONAL_COMPONENT).abs() < 0.000_01);

        let target = facing
            .update(Vec2::new(1.0, 0.0), 1.0)
            .expect("a large frame snaps without overshooting");
        assert_eq!(target, Vec2::new(1.0, 0.0));
    }

    #[test]
    fn smooth_facing_rejects_invalid_configuration_and_frame_input() {
        assert_eq!(
            LocomotionFacingSmoother::new(Vec2::new(0.0, 1.0), 0.0),
            Err(LocomotionFacingError::InvalidTurnSpeed)
        );
        assert_eq!(
            LocomotionFacingSmoother::new(Vec2::ZERO, 1.0),
            Err(LocomotionFacingError::InvalidDirection)
        );
        let mut facing =
            LocomotionFacingSmoother::new(Vec2::new(0.0, 1.0), 1.0).expect("valid facing");
        assert_eq!(
            facing.update(Vec2::new(f32::NAN, 0.0), 0.1),
            Err(LocomotionFacingError::InvalidDirection)
        );
        assert_eq!(
            facing.update(Vec2::new(1.0, 0.0), -0.1),
            Err(LocomotionFacingError::InvalidDeltaSeconds)
        );
    }
}
