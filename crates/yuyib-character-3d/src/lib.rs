//! Deterministic, renderer-neutral prototype character movement for Yuyib.
//!
//! [`CharacterMotor3d`] consumes caller-supplied [`CharacterInput3d`] at one
//! configured fixed time step. It is deliberately independent of Winit,
//! keyboard bindings, rendering and networking: an application maps actions
//! such as `move_forward` and `jump` to input before calling
//! [`CharacterMotor3d::step`].
//! [`step_character_motors_3d`] is an optional ECS adapter that writes movement
//! into [`yuyib_game_3d::LocalTransform3d`] for use with scene hierarchy
//! propagation.
//!
//! # Collision model
//!
//! [`CharacterMotor3d`] is the original small motor and resolves only against
//! one infinite horizontal ground plane. [`CharacterController3d`] is the
//! higher-level map controller: [`CharacterController3d::step_on_triangle_mesh`]
//! resolves its sphere against exact static map triangles, so it can walk a
//! corridor, stop at walls and jump from floors. For a custom collider, use
//! [`CharacterController3d::step_with_collision`] and supply the resolution
//! function directly. Neither controller is a general rigid-body solver:
//! there are no moving platforms, steps, slopes policy or dynamic bodies yet.
//! The triangle-map path divides one normal fixed movement into a small,
//! bounded number of sphere resolutions. This prevents ordinary jumping from
//! skipping a thin ceiling; it is deliberately not a replacement for
//! continuous collision detection at arbitrarily high speeds.
//!
//! ```
//! use yuyib_character_3d::{CharacterInput3d, CharacterMotor3d, CharacterMotorConfig3d};
//! use yuyib_physics::{Vec2, Vec3};
//!
//! let config = CharacterMotorConfig3d::default();
//! let mut motor = CharacterMotor3d::new(config, Vec3::new(0.0, 0.5, 0.0)).unwrap();
//! let input = CharacterInput3d::new(Vec2::new(0.0, 1.0), false).unwrap();
//! let _events = motor.step(input).unwrap();
//! ```

#![forbid(unsafe_code)]

mod locomotion;
mod placement;

pub use locomotion::{
    LocomotionAnimationSample8, LocomotionClipSet8, LocomotionController8,
    LocomotionControllerConfig8, LocomotionControllerError8, LocomotionDirection8,
    LocomotionFacingError, LocomotionFacingSmoother, LocomotionState8, LocomotionTransition8,
};
pub use placement::{CharacterModelPlacement3d, CharacterModelPlacementError3d};

use std::{cmp::Ordering, error::Error, fmt};

use yuyib_ecs::{bevy_ecs::entity::Entity, prelude::*};
use yuyib_game_3d::LocalTransform3d;
use yuyib_physics::{Ray3d, Sphere, TriangleMesh3d, TriangleMeshQueryError, Vec2, Vec3};

/// Immutable simulation parameters for [`CharacterMotor3d`].
///
/// The motor is deliberately kinematic: horizontal desired movement becomes
/// immediate velocity, while vertical motion uses explicit Euler gravity. This
/// makes first prototypes predictable and keeps acceleration/friction policy
/// in a future controller layer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CharacterMotorConfig3d {
    /// Fixed simulation interval in seconds. Every motor step uses this value.
    pub fixed_delta_seconds: f32,
    /// Non-positive world-space vertical acceleration in units per second².
    pub gravity_y: f32,
    /// Maximum horizontal speed in world units per second.
    pub move_speed: f32,
    /// Initial upward speed applied by a grounded jump.
    pub jump_speed: f32,
    /// Centre height of the infinite ground plane in world units.
    pub ground_y: f32,
    /// Radius of the character's ground-contact sphere.
    pub radius: f32,
}

impl CharacterMotorConfig3d {
    /// Validates this configuration for fixed-step simulation.
    ///
    /// # Errors
    ///
    /// Returns [`CharacterMotorError::InvalidConfig`] when any setting is
    /// non-finite, the step/radius is not positive, gravity is positive, or a
    /// speed is negative.
    pub fn validate(self) -> Result<(), CharacterMotorError> {
        validate_positive(self.fixed_delta_seconds, "fixed_delta_seconds")?;
        validate_finite(self.gravity_y, "gravity_y")?;
        if self.gravity_y > 0.0 {
            return Err(CharacterMotorError::InvalidConfig {
                field: "gravity_y",
                reason: "must be zero or negative for a downward ground-plane model",
            });
        }
        validate_non_negative(self.move_speed, "move_speed")?;
        validate_non_negative(self.jump_speed, "jump_speed")?;
        validate_finite(self.ground_y, "ground_y")?;
        validate_positive(self.radius, "radius")?;
        Ok(())
    }
}

impl Default for CharacterMotorConfig3d {
    fn default() -> Self {
        Self {
            fixed_delta_seconds: 1.0 / 60.0,
            gravity_y: -19.62,
            move_speed: 5.0,
            jump_speed: 7.0,
            ground_y: 0.0,
            radius: 0.5,
        }
    }
}

/// Validated caller-supplied desired movement for one fixed character step.
///
/// `movement.x` means world-space X/right and `movement.y` means world-space
/// Z/forward. The constructor normalizes values longer than one so diagonal or
/// analogue inputs cannot grant extra speed. It deliberately does not apply a
/// camera orientation; callers can map local controls to world axes however
/// their game requires.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CharacterInput3d {
    movement: Vec2,
    jump_pressed: bool,
}

impl CharacterInput3d {
    /// Creates one fixed-step input sample.
    ///
    /// # Errors
    ///
    /// Returns [`CharacterMotorError::InvalidInput`] when movement contains
    /// NaN or infinity.
    pub fn new(movement: Vec2, jump_pressed: bool) -> Result<Self, CharacterMotorError> {
        if !movement.x.is_finite() || !movement.y.is_finite() {
            return Err(CharacterMotorError::InvalidInput);
        }
        let normalized = if movement.length_squared() > 1.0 {
            movement.normalized_or_zero()
        } else {
            movement
        };
        Ok(Self {
            movement: normalized,
            jump_pressed,
        })
    }

    /// Returns a neutral movement/no-jump input.
    #[must_use]
    pub const fn idle() -> Self {
        Self {
            movement: Vec2::ZERO,
            jump_pressed: false,
        }
    }

    /// Returns the normalized desired horizontal movement.
    #[must_use]
    pub const fn movement(self) -> Vec2 {
        self.movement
    }

    /// Returns whether this step requests a jump.
    #[must_use]
    pub const fn jump_pressed(self) -> bool {
        self.jump_pressed
    }
}

/// Gameplay-relevant transition from one [`CharacterMotor3d::step`] call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CharacterMotorEvent3d {
    /// A grounded motor accepted a jump request.
    Jumped,
    /// A falling motor was clamped back onto the ground plane.
    Landed,
}

/// Result of one fixed motor update.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CharacterMotorStep3d {
    events: Vec<CharacterMotorEvent3d>,
}

impl CharacterMotorStep3d {
    /// Returns transition events in their deterministic emission order.
    #[must_use]
    pub fn events(&self) -> &[CharacterMotorEvent3d] {
        &self.events
    }

    /// Returns whether no transition happened in this step.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

/// Kinematic state for one ground-plane prototype character.
///
/// The component can be used standalone or in [`step_character_motors_3d`].
/// Its position is authoritative for this motor; the ECS adapter copies that
/// position into a [`LocalTransform3d`] after each successful fixed update.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct CharacterMotor3d {
    config: CharacterMotorConfig3d,
    collider: Sphere,
    position: Vec3,
    velocity: Vec3,
    grounded: bool,
}

impl CharacterMotor3d {
    /// Creates a character motor at `position`.
    ///
    /// The initial position is clamped upward to keep the contact sphere above
    /// the configured plane. An initially clamped motor starts grounded.
    ///
    /// # Errors
    ///
    /// Returns [`CharacterMotorError`] for invalid config or a non-finite
    /// position.
    pub fn new(
        config: CharacterMotorConfig3d,
        position: Vec3,
    ) -> Result<Self, CharacterMotorError> {
        config.validate()?;
        validate_position(position)?;
        let collider = Sphere::new(config.radius).map_err(CharacterMotorError::Physics)?;
        let contact_y = config.ground_y + config.radius;
        let grounded = position.y <= contact_y;
        Ok(Self {
            config,
            collider,
            position: Vec3::new(position.x, position.y.max(contact_y), position.z),
            velocity: Vec3::ZERO,
            grounded,
        })
    }

    /// Returns the fixed configuration that governs this motor.
    #[must_use]
    pub const fn config(self) -> CharacterMotorConfig3d {
        self.config
    }

    /// Replaces locomotion settings without resetting pose / velocity.
    ///
    /// Changing `radius` or `ground_y` rebuilds the contact sphere and may
    /// snap the body onto the plane if it would penetrate.
    ///
    /// # Errors
    ///
    /// Returns [`CharacterMotorError::InvalidConfig`] or a physics radius error.
    pub fn set_config(
        &mut self,
        config: CharacterMotorConfig3d,
    ) -> Result<(), CharacterMotorError> {
        config.validate()?;
        let collider = Sphere::new(config.radius).map_err(CharacterMotorError::Physics)?;
        self.config = config;
        self.collider = collider;
        let contact_y = config.ground_y + config.radius;
        if self.position.y < contact_y {
            self.position.y = contact_y;
            self.velocity.y = self.velocity.y.max(0.0);
            self.grounded = true;
        }
        Ok(())
    }

    /// Returns the ground-contact sphere shape.
    #[must_use]
    pub const fn collider(self) -> Sphere {
        self.collider
    }

    /// Returns the current finite world position.
    #[must_use]
    pub const fn position(self) -> Vec3 {
        self.position
    }

    /// Returns current linear velocity in world units per second.
    #[must_use]
    pub const fn velocity(self) -> Vec3 {
        self.velocity
    }

    /// Returns whether the contact sphere is resting on the ground plane.
    #[must_use]
    pub const fn is_grounded(self) -> bool {
        self.grounded
    }

    /// Teleports the motor and resets vertical contact state.
    ///
    /// The target is clamped to the ground plane as in [`Self::new`], while
    /// velocity is preserved so gameplay may intentionally teleport momentum.
    ///
    /// # Errors
    ///
    /// Returns [`CharacterMotorError::InvalidPosition`] for non-finite input.
    pub fn set_position(&mut self, position: Vec3) -> Result<(), CharacterMotorError> {
        validate_position(position)?;
        let contact_y = self.config.ground_y + self.config.radius;
        self.grounded = position.y <= contact_y;
        self.position = Vec3::new(position.x, position.y.max(contact_y), position.z);
        Ok(())
    }

    /// Simulates exactly one configured fixed time step.
    ///
    /// Horizontal motion uses the current desired input immediately. A jump is
    /// accepted only when grounded, then gravity is integrated with explicit
    /// Euler and the sphere is clamped to the infinite ground plane. Call this
    /// exactly once per fixed tick; variable frame time belongs to the caller's
    /// accumulator rather than this deterministic motor API.
    ///
    /// # Errors
    ///
    /// Returns [`CharacterMotorError::InvalidSimulation`] if arithmetic could
    /// no longer produce a finite state. The old state is retained on error.
    pub fn step(
        &mut self,
        input: CharacterInput3d,
    ) -> Result<CharacterMotorStep3d, CharacterMotorError> {
        let mut next = *self;
        let mut events = Vec::new();
        next.velocity.x = input.movement.x * next.config.move_speed;
        next.velocity.z = input.movement.y * next.config.move_speed;
        if next.grounded && input.jump_pressed {
            next.velocity.y = next.config.jump_speed;
            next.grounded = false;
            events.push(CharacterMotorEvent3d::Jumped);
        }
        if !next.grounded {
            next.velocity.y += next.config.gravity_y * next.config.fixed_delta_seconds;
        }
        next.position = next.position + next.velocity * next.config.fixed_delta_seconds;
        let contact_y = next.config.ground_y + next.config.radius;
        if next.position.y <= contact_y {
            if !next.grounded {
                events.push(CharacterMotorEvent3d::Landed);
            }
            next.position.y = contact_y;
            next.velocity.y = 0.0;
            next.grounded = true;
        }
        if !finite_vec3(next.position) || !finite_vec3(next.velocity) {
            return Err(CharacterMotorError::InvalidSimulation);
        }
        *self = next;
        Ok(CharacterMotorStep3d { events })
    }
}

/// Fixed parameters for [`CharacterController3d`].
///
/// Unlike [`CharacterMotorConfig3d`], this configuration has no artificial
/// infinite floor. Grounding comes from the collision result, normally from a
/// static [`TriangleMesh3d`] imported with a map.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CharacterControllerConfig3d {
    /// Fixed simulation interval in seconds.
    pub fixed_delta_seconds: f32,
    /// Non-positive vertical acceleration in world units per second².
    pub gravity_y: f32,
    /// Desired horizontal speed in world units per second.
    pub move_speed: f32,
    /// Initial upward velocity for a grounded jump.
    pub jump_speed: f32,
    /// Radius of the character contact sphere.
    pub radius: f32,
    /// Maximum passes used to push the sphere out of static map triangles.
    ///
    /// Four passes are usually enough for a corridor corner. Raise this only
    /// for unusually dense overlapping map geometry; it scales collision cost
    /// linearly.
    pub collision_iterations: usize,
}

impl CharacterControllerConfig3d {
    /// Validates this configuration for fixed-step map movement.
    ///
    /// # Errors
    ///
    /// Returns [`CharacterControllerError3d::InvalidConfig`] when a value is
    /// unsuitable for a finite, deterministic fixed step.
    pub fn validate(self) -> Result<(), CharacterControllerError3d> {
        validate_controller_positive(self.fixed_delta_seconds, "fixed_delta_seconds")?;
        validate_controller_finite(self.gravity_y, "gravity_y")?;
        if self.gravity_y > 0.0 {
            return Err(CharacterControllerError3d::InvalidConfig {
                field: "gravity_y",
                reason: "must be zero or negative",
            });
        }
        validate_controller_non_negative(self.move_speed, "move_speed")?;
        validate_controller_non_negative(self.jump_speed, "jump_speed")?;
        validate_controller_positive(self.radius, "radius")?;
        if self.collision_iterations == 0 {
            return Err(CharacterControllerError3d::InvalidConfig {
                field: "collision_iterations",
                reason: "must be positive",
            });
        }
        Ok(())
    }
}

impl Default for CharacterControllerConfig3d {
    fn default() -> Self {
        Self {
            fixed_delta_seconds: 1.0 / 60.0,
            gravity_y: -19.62,
            move_speed: 5.0,
            jump_speed: 7.0,
            radius: 0.35,
            collision_iterations: 4,
        }
    }
}

/// Search policy used by [`CharacterController3d::spawn_in_triangle_mesh`].
///
/// The default is deliberately suitable for enclosed corridors: a candidate
/// must have a near-horizontal supporting triangle, empty sphere clearance and
/// a ceiling high enough for the camera/player.  Open outdoor levels can turn
/// off [`Self::require_ceiling`] and explicitly select a lower surface. Merely
/// disabling the ceiling check retains the default nearest-to-anchor ordering
/// and may therefore select a roof in vertically layered city geometry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CharacterSpawnOptions3d {
    /// Small empty gap left between the controller sphere and its floor.
    pub floor_clearance: f32,
    /// Minimum vertical free space above the sphere centre.
    pub minimum_headroom: f32,
    /// Minimum absolute Y component of a floor candidate's geometric normal.
    pub minimum_floor_normal_y: f32,
    /// Reject an exposed surface when no ceiling is found above it.
    pub require_ceiling: bool,
    /// Optional vertical distance that must remain free for an outdoor spawn.
    ///
    /// A ceiling hit closer than this value rejects the candidate; no ceiling
    /// hit is accepted as open sky. Use this together with
    /// [`CharacterController3d::spawn_on_surface_mesh_with_options`] when the
    /// walkable surface layer deliberately excludes buildings.
    pub minimum_open_sky_clearance: Option<f32>,
    /// Horizontal point used to rank or constrain candidate surfaces.
    pub anchor: CharacterSpawnAnchor3d,
    /// Optional maximum horizontal distance from [`Self::anchor`].
    ///
    /// This localizes elevation-based policies in maps that contain unrelated
    /// underground or remote geometry. `None` searches the complete mesh.
    pub maximum_horizontal_distance: Option<f32>,
    /// Deterministic ordering used after floor-angle filtering.
    pub surface_selection: CharacterSpawnSurfaceSelection3d,
}

impl Default for CharacterSpawnOptions3d {
    fn default() -> Self {
        Self {
            floor_clearance: 0.02,
            minimum_headroom: 1.6,
            minimum_floor_normal_y: 0.55,
            require_ceiling: true,
            minimum_open_sky_clearance: None,
            anchor: CharacterSpawnAnchor3d::MeshCenter,
            maximum_horizontal_distance: None,
            surface_selection: CharacterSpawnSurfaceSelection3d::NearestToAnchor,
        }
    }
}

impl CharacterSpawnOptions3d {
    /// Creates an outdoor policy that selects the lowest valid surface near a
    /// caller-owned horizontal anchor.
    ///
    /// This is suitable for a road/terrain spawn in a city model with roofs
    /// stacked above the same XZ area. It validates geometric clearance but
    /// does not claim navigation-mesh reachability. Use
    /// [`Self::with_maximum_horizontal_distance`] to avoid selecting an
    /// unrelated lower level far from the intended spawn district.
    #[must_use]
    pub const fn outdoor_lowest(preferred_xz: Vec2) -> Self {
        Self {
            floor_clearance: 0.02,
            minimum_headroom: 1.6,
            minimum_floor_normal_y: 0.55,
            require_ceiling: false,
            minimum_open_sky_clearance: None,
            anchor: CharacterSpawnAnchor3d::PreferredXz(preferred_xz),
            maximum_horizontal_distance: None,
            surface_selection: CharacterSpawnSurfaceSelection3d::LowestElevation,
        }
    }

    /// Replaces the horizontal anchor used by the surface-selection policy.
    #[must_use]
    pub const fn with_anchor(mut self, anchor: CharacterSpawnAnchor3d) -> Self {
        self.anchor = anchor;
        self
    }

    /// Restricts candidates to a finite horizontal radius around the anchor.
    #[must_use]
    pub const fn with_maximum_horizontal_distance(mut self, distance: f32) -> Self {
        self.maximum_horizontal_distance = Some(distance);
        self
    }

    /// Replaces the deterministic vertical/horizontal candidate ordering.
    #[must_use]
    pub const fn with_surface_selection(
        mut self,
        selection: CharacterSpawnSurfaceSelection3d,
    ) -> Self {
        self.surface_selection = selection;
        self
    }

    /// Rejects outdoor candidates with overhead geometry closer than
    /// `distance` world units. Absence of an overhead hit counts as open sky.
    #[must_use]
    pub const fn with_minimum_open_sky_clearance(mut self, distance: f32) -> Self {
        self.minimum_open_sky_clearance = Some(distance);
        self
    }

    fn validate(self) -> Result<(), CharacterControllerError3d> {
        validate_controller_non_negative(self.floor_clearance, "floor_clearance")?;
        validate_controller_positive(self.minimum_headroom, "minimum_headroom")?;
        validate_controller_finite(self.minimum_floor_normal_y, "minimum_floor_normal_y")?;
        if !(0.0..=1.0).contains(&self.minimum_floor_normal_y) {
            return Err(CharacterControllerError3d::InvalidConfig {
                field: "minimum_floor_normal_y",
                reason: "must be in 0.0..=1.0",
            });
        }
        if let CharacterSpawnAnchor3d::PreferredXz(anchor) = self.anchor
            && (!anchor.x.is_finite() || !anchor.y.is_finite())
        {
            return Err(CharacterControllerError3d::InvalidConfig {
                field: "spawn_anchor",
                reason: "preferred XZ coordinates must be finite",
            });
        }
        if let Some(distance) = self.maximum_horizontal_distance {
            validate_controller_non_negative(distance, "maximum_horizontal_distance")?;
        }
        if let Some(distance) = self.minimum_open_sky_clearance {
            validate_controller_positive(distance, "minimum_open_sky_clearance")?;
            if distance < self.minimum_headroom {
                return Err(CharacterControllerError3d::InvalidConfig {
                    field: "minimum_open_sky_clearance",
                    reason: "must be at least minimum_headroom",
                });
            }
        }
        if let CharacterSpawnSurfaceSelection3d::ClosestToElevation(elevation) =
            self.surface_selection
        {
            validate_controller_finite(elevation, "spawn_elevation")?;
        }
        Ok(())
    }
}

/// Horizontal reference used to rank static-map spawn candidates.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum CharacterSpawnAnchor3d {
    /// Arithmetic centre of all triangle vertices, preserving the original
    /// high-level map-spawn behaviour.
    #[default]
    MeshCenter,
    /// Caller-owned preferred world-space XZ point (`Vec2::x` = X,
    /// `Vec2::y` = Z). Each candidate is placed at the closest point on its
    /// projected triangle instead of being forced to the triangle centroid.
    PreferredXz(Vec2),
}

/// Ordering policy for geometrically valid static-map spawn surfaces.
///
/// Every variant still applies floor-normal, sphere-clearance and headroom
/// checks. "Lowest" means the lowest valid candidate in the configured search
/// area; determining graph reachability requires a navigation system and is
/// intentionally not fabricated by this collision-only API.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum CharacterSpawnSurfaceSelection3d {
    /// Prefer the smallest horizontal distance to the anchor, then source
    /// triangle order. This is the backward-compatible corridor default.
    #[default]
    NearestToAnchor,
    /// Prefer the lowest world-space Y, then distance to the anchor.
    LowestElevation,
    /// Prefer the highest world-space Y, then distance to the anchor.
    HighestElevation,
    /// Prefer the smallest absolute distance to one world-space elevation,
    /// then horizontal distance to the anchor.
    ClosestToElevation(f32),
}

/// Why a surface triangle was rejected during high-level spawn selection.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CharacterSpawnRejectReason3d {
    /// Supporting triangle normal was too steep for the floor policy.
    SteepNormal,
    /// Candidate fell outside [`CharacterSpawnOptions3d::maximum_horizontal_distance`].
    OutsideHorizontalRadius,
    /// Spawn sphere overlapped the collision mesh at the candidate point.
    SphereContacts,
    /// [`CharacterSpawnOptions3d::require_ceiling`] was set and no ceiling hit existed.
    MissingCeiling,
    /// A ceiling existed but was closer than [`CharacterSpawnOptions3d::minimum_headroom`].
    LowHeadroom,
    /// A ceiling existed closer than [`CharacterSpawnOptions3d::minimum_open_sky_clearance`].
    InsufficientOpenSky,
}

/// Aggregate reject tallies produced by spawn selection.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CharacterSpawnRejectCounts3d {
    /// Triangles skipped for [`CharacterSpawnRejectReason3d::SteepNormal`].
    pub steep_normal: u32,
    /// Triangles skipped for [`CharacterSpawnRejectReason3d::OutsideHorizontalRadius`].
    pub outside_horizontal_radius: u32,
    /// Ranked candidates rejected for [`CharacterSpawnRejectReason3d::SphereContacts`].
    pub sphere_contacts: u32,
    /// Ranked candidates rejected for [`CharacterSpawnRejectReason3d::MissingCeiling`].
    pub missing_ceiling: u32,
    /// Ranked candidates rejected for [`CharacterSpawnRejectReason3d::LowHeadroom`].
    pub low_headroom: u32,
    /// Ranked candidates rejected for [`CharacterSpawnRejectReason3d::InsufficientOpenSky`].
    pub insufficient_open_sky: u32,
}

impl CharacterSpawnRejectCounts3d {
    /// Sum of all reject counters.
    #[must_use]
    pub const fn total(self) -> u32 {
        self.steep_normal
            + self.outside_horizontal_radius
            + self.sphere_contacts
            + self.missing_ceiling
            + self.low_headroom
            + self.insufficient_open_sky
    }

    fn bump(&mut self, reason: CharacterSpawnRejectReason3d) {
        match reason {
            CharacterSpawnRejectReason3d::SteepNormal => self.steep_normal += 1,
            CharacterSpawnRejectReason3d::OutsideHorizontalRadius => {
                self.outside_horizontal_radius += 1
            }
            CharacterSpawnRejectReason3d::SphereContacts => self.sphere_contacts += 1,
            CharacterSpawnRejectReason3d::MissingCeiling => self.missing_ceiling += 1,
            CharacterSpawnRejectReason3d::LowHeadroom => self.low_headroom += 1,
            CharacterSpawnRejectReason3d::InsufficientOpenSky => self.insufficient_open_sky += 1,
        }
    }
}

/// The surface triangle chosen by high-level spawn selection.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CharacterSpawnSelection3d {
    /// Index of the supporting triangle in the surface mesh.
    pub triangle: usize,
    /// Contact point on the supporting triangle (feet / floor).
    pub floor: Vec3,
    /// Resolved sphere centre after floor clearance.
    pub position: Vec3,
    /// Horizontal distance from the resolved spawn anchor to [`Self::floor`].
    pub horizontal_distance: f32,
}

/// Observability report for one high-level spawn attempt.
///
/// Always produced by
/// [`CharacterController3d::spawn_on_surface_mesh_with_report`], including the
/// failure path [`CharacterControllerError3d::NoPlayableSpawn`]. Soft metrics
/// for screenshots live elsewhere; this report explains *why* a map spawn
/// succeeded or exhausted candidates.
#[derive(Clone, Debug, PartialEq)]
pub struct CharacterSpawnReport3d {
    /// Horizontal XZ anchor used for ranking / radius filtering.
    pub anchor: Vec2,
    /// Number of triangles inspected in the surface mesh.
    pub surface_triangle_count: u32,
    /// Candidates that passed floor-angle and radius filters before clearance.
    pub ranked_candidate_count: u32,
    /// Per-reason reject tallies across the full search.
    pub reject_counts: CharacterSpawnRejectCounts3d,
    /// Winning candidate when spawn succeeded.
    pub selected: Option<CharacterSpawnSelection3d>,
}

#[derive(Clone, Copy, Debug)]
struct SpawnSurfaceCandidate3d {
    horizontal_distance: f32,
    triangle: usize,
    point: Vec3,
}

fn compare_spawn_surfaces(
    selection: CharacterSpawnSurfaceSelection3d,
    left: &SpawnSurfaceCandidate3d,
    right: &SpawnSurfaceCandidate3d,
) -> Ordering {
    let elevation_order = match selection {
        CharacterSpawnSurfaceSelection3d::NearestToAnchor => Ordering::Equal,
        CharacterSpawnSurfaceSelection3d::LowestElevation => left.point.y.total_cmp(&right.point.y),
        CharacterSpawnSurfaceSelection3d::HighestElevation => {
            right.point.y.total_cmp(&left.point.y)
        }
        CharacterSpawnSurfaceSelection3d::ClosestToElevation(elevation) => (left.point.y
            - elevation)
            .abs()
            .total_cmp(&(right.point.y - elevation).abs()),
    };
    elevation_order
        .then_with(|| {
            left.horizontal_distance
                .total_cmp(&right.horizontal_distance)
        })
        .then_with(|| left.triangle.cmp(&right.triangle))
}

fn closest_point_on_triangle_xz(anchor: Vec2, face: [Vec3; 3]) -> Vec3 {
    let points = [
        Vec2::new(face[0].x, face[0].z),
        Vec2::new(face[1].x, face[1].z),
        Vec2::new(face[2].x, face[2].z),
    ];
    let ab = points[1] - points[0];
    let ac = points[2] - points[0];
    let ap = anchor - points[0];
    let d1 = dot2(ab, ap);
    let d2 = dot2(ac, ap);
    if d1 <= 0.0 && d2 <= 0.0 {
        return face[0];
    }

    let bp = anchor - points[1];
    let d3 = dot2(ab, bp);
    let d4 = dot2(ac, bp);
    if d3 >= 0.0 && d4 <= d3 {
        return face[1];
    }
    let vc = d1.mul_add(d4, -(d3 * d2));
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return face[0] + (face[1] - face[0]) * (d1 / (d1 - d3));
    }

    let cp = anchor - points[2];
    let d5 = dot2(ab, cp);
    let d6 = dot2(ac, cp);
    if d6 >= 0.0 && d5 <= d6 {
        return face[2];
    }
    let vb = d5.mul_add(d2, -(d1 * d6));
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return face[0] + (face[2] - face[0]) * (d2 / (d2 - d6));
    }

    let va = d3.mul_add(d6, -(d5 * d4));
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        let weight = (d4 - d3) / ((d4 - d3) + (d5 - d6));
        return face[1] + (face[2] - face[1]) * weight;
    }

    let inverse = (va + vb + vc).recip();
    let b_weight = vb * inverse;
    let c_weight = vc * inverse;
    face[0] + (face[1] - face[0]) * b_weight + (face[2] - face[0]) * c_weight
}

fn dot2(left: Vec2, right: Vec2) -> f32 {
    left.x.mul_add(right.x, left.y * right.y)
}

/// A low-level collision result supplied to [`CharacterController3d`].
///
/// `position` must be the non-penetrating sphere centre. `grounded` means the
/// collision system found a walkable upward-facing contact. This intentionally
/// keeps custom physics integration small: a game can use its own collider
/// backend without the controller depending on it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CharacterCollisionResolution3d {
    /// Collision-free sphere centre.
    pub position: Vec3,
    /// Whether the result has a walkable floor contact.
    pub grounded: bool,
    /// Number of contacts considered while resolving this move.
    pub contacts: usize,
}

/// Failure produced by a caller-provided collision resolver.
#[derive(Clone, Debug, PartialEq)]
pub enum CharacterCollisionError3d {
    /// The built-in static triangle-mesh query could not be completed.
    TriangleMesh(TriangleMeshQueryError),
    /// A custom collision backend rejected the requested movement.
    Custom(String),
}

impl CharacterCollisionError3d {
    /// Wraps a stable message from a custom low-level collision backend.
    #[must_use]
    pub fn custom(message: impl Into<String>) -> Self {
        Self::Custom(message.into())
    }
}

impl From<TriangleMeshQueryError> for CharacterCollisionError3d {
    fn from(error: TriangleMeshQueryError) -> Self {
        Self::TriangleMesh(error)
    }
}

impl fmt::Display for CharacterCollisionError3d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TriangleMesh(error) => write!(formatter, "triangle collision failed: {error}"),
            Self::Custom(message) => {
                write!(formatter, "custom character collision failed: {message}")
            }
        }
    }
}

impl Error for CharacterCollisionError3d {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::TriangleMesh(error) => Some(error),
            Self::Custom(_) => None,
        }
    }
}

/// Gameplay-relevant transition from one [`CharacterController3d`] step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CharacterControllerEvent3d {
    /// A grounded controller accepted a jump request.
    Jumped,
    /// A falling controller gained a walkable floor contact.
    Landed,
    /// The requested movement was pushed out of one or more static surfaces.
    Collided,
}

/// Result of one collision-aware character step.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CharacterControllerStep3d {
    events: Vec<CharacterControllerEvent3d>,
    contacts: usize,
}

impl CharacterControllerStep3d {
    /// Returns transition events in deterministic emission order.
    #[must_use]
    pub fn events(&self) -> &[CharacterControllerEvent3d] {
        &self.events
    }

    /// Returns the number of static contacts reported by the resolver.
    #[must_use]
    pub const fn contacts(&self) -> usize {
        self.contacts
    }
}

/// Fixed-step character controller for a static map.
///
/// Use [`Self::step_on_triangle_mesh`] for the normal case. It is the compact
/// high-level API for imported maps. It splits a normal fixed displacement
/// into bounded sphere-resolution increments, so a jump cannot pass through
/// an ordinary thin ceiling. [`Self::step_with_collision`] is the explicit
/// low-level escape hatch for a custom physics world. This is not general
/// continuous collision detection: keep speed and fixed step bounded.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CharacterController3d {
    config: CharacterControllerConfig3d,
    collider: Sphere,
    position: Vec3,
    velocity: Vec3,
    grounded: bool,
}

// A controller normally moves far less than half its radius during a 60 Hz
// tick. Keeping each triangle-map increment at or below that distance catches
// floor, wall and ceiling faces without making the common case more expensive.
// The cap is an explicit work bound for malformed input/configuration; games
// that need projectile-grade CCD should use `step_with_collision` with their
// own physics backend.
const MAX_TRIANGLE_MESH_SUBSTEPS: u8 = 32;

impl CharacterController3d {
    /// Creates a controller at a finite world-space sphere centre.
    ///
    /// The controller starts airborne until its first collision step or an
    /// explicit [`Self::place_on_triangle_mesh`] call establishes ground.
    ///
    /// # Errors
    ///
    /// Returns [`CharacterControllerError3d`] for invalid settings or a
    /// non-finite starting point.
    pub fn new(
        config: CharacterControllerConfig3d,
        position: Vec3,
    ) -> Result<Self, CharacterControllerError3d> {
        config.validate()?;
        validate_controller_position(position)?;
        let collider = Sphere::new(config.radius).map_err(CharacterControllerError3d::Physics)?;
        Ok(Self {
            config,
            collider,
            position,
            velocity: Vec3::ZERO,
            grounded: false,
        })
    }

    /// Creates a player at a deterministic, usable location in a static map.
    ///
    /// This high-level map-spawn helper inspects horizontal map triangles using
    /// the configured anchor and surface ordering. A selected point must fit
    /// the collision sphere and (by default) have a ceiling above it, so a
    /// map's exterior roof is not accidentally chosen instead of its corridor.
    /// Triangle winding does not decide which side is usable: the empty-sphere
    /// check does.
    ///
    /// For a game that owns player starts or needs editor-specific rules, use
    /// [`Self::new`] plus [`Self::place_on_triangle_mesh`] instead.  The raw
    /// [`TriangleMesh3d::raycast`] query remains available for custom spawn
    /// policies.
    ///
    /// # Errors
    ///
    /// Returns [`CharacterControllerError3d::NoPlayableSpawn`] when no map
    /// face satisfies the requested clearance policy.
    pub fn spawn_in_triangle_mesh(
        config: CharacterControllerConfig3d,
        mesh: &TriangleMesh3d,
    ) -> Result<Self, CharacterControllerError3d> {
        Self::spawn_in_triangle_mesh_with_options(config, mesh, CharacterSpawnOptions3d::default())
    }

    /// Configurable variant of [`Self::spawn_in_triangle_mesh`].
    ///
    /// This is still a high-level placement policy.  It only exposes the four
    /// choices that commonly differ between an indoor corridor and an open
    /// level; it does not expose triangle iteration or ray maths.
    ///
    /// # Errors
    ///
    /// Returns [`CharacterControllerError3d::NoPlayableSpawn`] when no map
    /// face meets `options`, or an ordinary controller/collision error when a
    /// setting or triangle query is invalid.
    pub fn spawn_in_triangle_mesh_with_options(
        config: CharacterControllerConfig3d,
        mesh: &TriangleMesh3d,
        options: CharacterSpawnOptions3d,
    ) -> Result<Self, CharacterControllerError3d> {
        Self::spawn_on_surface_mesh_with_options(config, mesh, mesh, options)
    }

    /// Selects a floor from `surface_mesh` and validates the complete spawn
    /// volume against a separate `collision_mesh`.
    ///
    /// This is the intended high-level path for semantic map layers: roads or
    /// navigation geometry choose where spawning is allowed, while the full
    /// static collider still rejects walls, props, low ceilings and indoor
    /// candidates. Passing the same mesh twice is equivalent to
    /// [`Self::spawn_in_triangle_mesh_with_options`].
    ///
    /// # Errors
    ///
    /// Returns [`CharacterControllerError3d::NoPlayableSpawn`] when no surface
    /// candidate also satisfies the full collision/open-sky policy.
    pub fn spawn_on_surface_mesh_with_options(
        config: CharacterControllerConfig3d,
        surface_mesh: &TriangleMesh3d,
        collision_mesh: &TriangleMesh3d,
        options: CharacterSpawnOptions3d,
    ) -> Result<Self, CharacterControllerError3d> {
        Self::spawn_on_surface_mesh_with_report(config, surface_mesh, collision_mesh, options)
            .map(|(controller, _)| controller)
    }

    /// Configurable surface spawn that always returns selection diagnostics.
    ///
    /// On success the report's [`CharacterSpawnReport3d::selected`] names the
    /// winning triangle. On exhaustion the error is
    /// [`CharacterControllerError3d::NoPlayableSpawn`] carrying the same report
    /// shape (with `selected = None`) so callers can log reject tallies without
    /// a second search.
    ///
    /// # Errors
    ///
    /// Propagates config/collision/physics failures unchanged. Exhausted search
    /// returns [`CharacterControllerError3d::NoPlayableSpawn`].
    pub fn spawn_on_surface_mesh_with_report(
        config: CharacterControllerConfig3d,
        surface_mesh: &TriangleMesh3d,
        collision_mesh: &TriangleMesh3d,
        options: CharacterSpawnOptions3d,
    ) -> Result<(Self, CharacterSpawnReport3d), CharacterControllerError3d> {
        config.validate()?;
        options.validate()?;

        let preferred_anchor = matches!(options.anchor, CharacterSpawnAnchor3d::PreferredXz(_));
        let empty_report = |anchor: Vec2| CharacterSpawnReport3d {
            anchor,
            surface_triangle_count: 0,
            ranked_candidate_count: 0,
            reject_counts: CharacterSpawnRejectCounts3d::default(),
            selected: None,
        };
        let anchor = match options.anchor {
            CharacterSpawnAnchor3d::MeshCenter => {
                let mut horizontal_centre = Vec2::ZERO;
                let mut vertex_count = 0.0_f32;
                for face in surface_mesh.triangles() {
                    for vertex in face {
                        horizontal_centre.x += vertex.x;
                        horizontal_centre.y += vertex.z;
                        vertex_count += 1.0;
                    }
                }
                if vertex_count <= 0.0 {
                    return Err(CharacterControllerError3d::NoPlayableSpawn(empty_report(
                        Vec2::ZERO,
                    )));
                }
                let anchor = horizontal_centre * vertex_count.recip();
                if !anchor.x.is_finite() || !anchor.y.is_finite() {
                    return Err(CharacterControllerError3d::NoPlayableSpawn(empty_report(
                        Vec2::ZERO,
                    )));
                }
                anchor
            }
            CharacterSpawnAnchor3d::PreferredXz(anchor) => anchor,
        };

        let mut reject_counts = CharacterSpawnRejectCounts3d::default();
        let mut surface_triangle_count = 0_u32;
        let mut candidates = Vec::new();
        for (triangle, face) in surface_mesh.triangles().iter().copied().enumerate() {
            surface_triangle_count = surface_triangle_count.saturating_add(1);
            let normal = cross3(face[1] - face[0], face[2] - face[0]).normalized_or_zero();
            if normal.y.abs() < options.minimum_floor_normal_y {
                reject_counts.bump(CharacterSpawnRejectReason3d::SteepNormal);
                continue;
            }
            let point = if preferred_anchor {
                closest_point_on_triangle_xz(anchor, face)
            } else {
                (face[0] + face[1] + face[2]) * (1.0 / 3.0)
            };
            let horizontal_distance = (point.x - anchor.x).hypot(point.z - anchor.y);
            if !horizontal_distance.is_finite()
                || options
                    .maximum_horizontal_distance
                    .is_some_and(|maximum| horizontal_distance > maximum)
            {
                reject_counts.bump(CharacterSpawnRejectReason3d::OutsideHorizontalRadius);
                continue;
            }
            candidates.push(SpawnSurfaceCandidate3d {
                horizontal_distance,
                triangle,
                point,
            });
        }
        let ranked_candidate_count = u32::try_from(candidates.len()).unwrap_or(u32::MAX);
        candidates
            .sort_by(|left, right| compare_spawn_surfaces(options.surface_selection, left, right));

        for candidate in candidates {
            let floor = candidate.point;
            let position = Vec3::new(
                floor.x,
                floor.y + config.radius + options.floor_clearance,
                floor.z,
            );
            let clearance = collision_mesh
                .resolve_sphere(position, config.radius, config.collision_iterations)
                .map_err(CharacterCollisionError3d::from)
                .map_err(CharacterControllerError3d::Collision)?;
            if clearance.contacts != 0 {
                reject_counts.bump(CharacterSpawnRejectReason3d::SphereContacts);
                continue;
            }

            let upward = Ray3d::new(position, Vec3::new(0.0, 1.0, 0.0))
                .map_err(CharacterControllerError3d::Physics)?;
            let ceiling = collision_mesh
                .raycast(upward, f32::MAX)
                .map_err(CharacterControllerError3d::Physics)?;
            if ceiling.is_some_and(|hit| hit.distance < options.minimum_headroom) {
                reject_counts.bump(CharacterSpawnRejectReason3d::LowHeadroom);
                continue;
            }
            if options.require_ceiling && ceiling.is_none() {
                reject_counts.bump(CharacterSpawnRejectReason3d::MissingCeiling);
                continue;
            }
            if options
                .minimum_open_sky_clearance
                .is_some_and(|minimum| ceiling.is_some_and(|hit| hit.distance < minimum))
            {
                reject_counts.bump(CharacterSpawnRejectReason3d::InsufficientOpenSky);
                continue;
            }

            let selection = CharacterSpawnSelection3d {
                triangle: candidate.triangle,
                floor,
                position,
                horizontal_distance: candidate.horizontal_distance,
            };
            let report = CharacterSpawnReport3d {
                anchor,
                surface_triangle_count,
                ranked_candidate_count,
                reject_counts,
                selected: Some(selection),
            };
            return Ok((
                Self {
                    config,
                    collider: Sphere::new(config.radius)
                        .map_err(CharacterControllerError3d::Physics)?,
                    position,
                    velocity: Vec3::ZERO,
                    grounded: true,
                },
                report,
            ));
        }
        Err(CharacterControllerError3d::NoPlayableSpawn(
            CharacterSpawnReport3d {
                anchor,
                surface_triangle_count,
                ranked_candidate_count,
                reject_counts,
                selected: None,
            },
        ))
    }

    /// Returns the fixed settings of this controller.
    #[must_use]
    pub const fn config(self) -> CharacterControllerConfig3d {
        self.config
    }

    /// Replaces locomotion / collision settings without resetting pose.
    ///
    /// Changing `radius` rebuilds the contact sphere. Pose and velocity are
    /// preserved so hosts can retune `move_speed` for sprint without a respawn.
    ///
    /// # Errors
    ///
    /// Returns [`CharacterControllerError3d`] for invalid settings or radius.
    pub fn set_config(
        &mut self,
        config: CharacterControllerConfig3d,
    ) -> Result<(), CharacterControllerError3d> {
        config.validate()?;
        let collider = Sphere::new(config.radius).map_err(CharacterControllerError3d::Physics)?;
        self.config = config;
        self.collider = collider;
        Ok(())
    }

    /// Returns the collision sphere shape.
    #[must_use]
    pub const fn collider(self) -> Sphere {
        self.collider
    }

    /// Returns the current sphere centre.
    #[must_use]
    pub const fn position(self) -> Vec3 {
        self.position
    }

    /// Returns current velocity in world units per second.
    #[must_use]
    pub const fn velocity(self) -> Vec3 {
        self.velocity
    }

    /// Returns whether the last collision result contained walkable ground.
    #[must_use]
    pub const fn is_grounded(self) -> bool {
        self.grounded
    }

    /// Teleports the controller and clears velocity/contact state.
    ///
    /// For a map-aware spawn, prefer [`Self::place_on_triangle_mesh`].
    ///
    /// # Errors
    ///
    /// Returns [`CharacterControllerError3d::InvalidPosition`] for NaN or
    /// infinite coordinates.
    pub fn set_position(&mut self, position: Vec3) -> Result<(), CharacterControllerError3d> {
        validate_controller_position(position)?;
        self.position = position;
        self.velocity = Vec3::ZERO;
        self.grounded = false;
        Ok(())
    }

    /// Resolves a spawn or teleport immediately against a static triangle map.
    ///
    /// This is normally called once after loading a map, before the first
    /// player frame. It does not fabricate a floor below an empty scene.
    ///
    /// # Errors
    ///
    /// Returns an error if the mesh query fails or produces a non-finite
    /// resolved position.
    pub fn place_on_triangle_mesh(
        &mut self,
        mesh: &TriangleMesh3d,
    ) -> Result<(), CharacterControllerError3d> {
        let result = mesh
            .resolve_sphere(
                self.position,
                self.config.radius,
                self.config.collision_iterations,
            )
            .map_err(CharacterCollisionError3d::from)
            .map_err(CharacterControllerError3d::Collision)?;
        self.apply_resolution(result)?;
        self.velocity = Vec3::ZERO;
        Ok(())
    }

    /// Simulates one fixed step against the supplied static triangle map.
    ///
    /// This is the usual high-level API for a player in an imported corridor
    /// or level. It uses the controller's configured sphere and iteration
    /// budget, and emits jump, landing and collision events.
    ///
    /// # Errors
    ///
    /// Returns an error if the static mesh cannot resolve the sphere or if
    /// fixed-step arithmetic would become non-finite.
    pub fn step_on_triangle_mesh(
        &mut self,
        input: CharacterInput3d,
        mesh: &TriangleMesh3d,
    ) -> Result<CharacterControllerStep3d, CharacterControllerError3d> {
        let radius = self.config.radius;
        let iterations = self.config.collision_iterations;
        let start = self.position;
        // `resolve_sphere` deliberately reports only penetration. A sphere
        // exactly tangent to a floor therefore needs this tiny bounded probe
        // to retain grounded state while walking, without turning a step into
        // a permanently invisible infinite floor.
        let probe_ground = self.grounded && !input.jump_pressed;
        self.step_with_collision(input, |desired, _| {
            let delta = desired - start;
            let max_increment = radius * 0.5;
            let distance = delta.length_squared().sqrt();
            let mut substeps = 1_u8;
            let mut covered_distance = max_increment;
            while distance > covered_distance && substeps < MAX_TRIANGLE_MESH_SUBSTEPS {
                substeps += 1;
                covered_distance += max_increment;
            }
            let increment = delta * f32::from(substeps).recip();
            let mut position = start;
            let mut contacts = 0;
            let mut ground_contact = false;

            for _ in 0..substeps {
                let result = mesh
                    .resolve_sphere(position + increment, radius, iterations)
                    .map_err(CharacterCollisionError3d::from)?;
                position = result.position;
                contacts += result.contacts;
                ground_contact |= result.ground_contact;
            }
            let mut result = yuyib_physics::SphereMeshResolution3d {
                position,
                ground_contact,
                contacts,
            };
            if probe_ground && !result.ground_contact {
                let probe = Vec3::new(
                    result.position.x,
                    result.position.y - 0.001,
                    result.position.z,
                );
                let ground = mesh
                    .resolve_sphere(probe, radius, iterations)
                    .map_err(CharacterCollisionError3d::from)?;
                if ground.ground_contact {
                    result = ground;
                }
            }
            Ok(CharacterCollisionResolution3d {
                position: result.position,
                grounded: result.ground_contact,
                contacts: result.contacts,
            })
        })
    }

    /// Simulates one fixed step through a custom low-level collision resolver.
    ///
    /// The callback receives the desired sphere centre and radius, and must
    /// return the final non-penetrating centre plus a grounded flag. This
    /// hook is for a custom broad phase, moving geometry or another physics
    /// backend while retaining Yuyib's input, jump and fixed-step policy.
    ///
    /// # Errors
    ///
    /// Returns the custom collision error unchanged in
    /// [`CharacterControllerError3d::Collision`], or fails when its returned
    /// centre is non-finite.
    pub fn step_with_collision<F>(
        &mut self,
        input: CharacterInput3d,
        mut resolve: F,
    ) -> Result<CharacterControllerStep3d, CharacterControllerError3d>
    where
        F: FnMut(Vec3, f32) -> Result<CharacterCollisionResolution3d, CharacterCollisionError3d>,
    {
        let mut next = *self;
        let mut events = Vec::new();
        next.velocity.x = input.movement.x * next.config.move_speed;
        next.velocity.z = input.movement.y * next.config.move_speed;
        if next.grounded && input.jump_pressed {
            next.velocity.y = next.config.jump_speed;
            next.grounded = false;
            events.push(CharacterControllerEvent3d::Jumped);
        }
        if !next.grounded {
            next.velocity.y += next.config.gravity_y * next.config.fixed_delta_seconds;
        }
        let desired = next.position + next.velocity * next.config.fixed_delta_seconds;
        let resolution =
            resolve(desired, next.config.radius).map_err(CharacterControllerError3d::Collision)?;
        validate_controller_position(resolution.position)
            .map_err(|_| CharacterControllerError3d::InvalidCollisionResult)?;
        let correction = resolution.position - desired;
        cancel_velocity_against_correction(&mut next.velocity, correction);
        let was_grounded = self.grounded;
        next.position = resolution.position;
        next.grounded = resolution.grounded;
        if next.grounded && next.velocity.y < 0.0 {
            next.velocity.y = 0.0;
        }
        if !was_grounded && next.grounded {
            events.push(CharacterControllerEvent3d::Landed);
        }
        if resolution.contacts > 0 {
            events.push(CharacterControllerEvent3d::Collided);
        }
        if !finite_vec3(next.position) || !finite_vec3(next.velocity) {
            return Err(CharacterControllerError3d::InvalidSimulation);
        }
        *self = next;
        Ok(CharacterControllerStep3d {
            events,
            contacts: resolution.contacts,
        })
    }

    fn apply_resolution(
        &mut self,
        resolution: yuyib_physics::SphereMeshResolution3d,
    ) -> Result<(), CharacterControllerError3d> {
        validate_controller_position(resolution.position)
            .map_err(|_| CharacterControllerError3d::InvalidCollisionResult)?;
        self.position = resolution.position;
        self.grounded = resolution.ground_contact;
        Ok(())
    }
}

/// An ECS-addressable event from [`step_character_motors_3d`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CharacterMotorEntityEvent3d {
    /// Entity whose motor emitted the event.
    pub entity: Entity,
    /// Transition generated by its fixed simulation step.
    pub event: CharacterMotorEvent3d,
}

/// Steps motors with inputs supplied by `input_for` and syncs local positions.
///
/// Query order is sorted by full generational entity ID before invoking the
/// caller callback, so input sampling and output events are deterministic for
/// a fixed world. Only entities carrying both [`CharacterMotor3d`] and
/// [`LocalTransform3d`] participate; this keeps transform ownership explicit.
/// After this function, call [`yuyib_game_3d::propagate_world_transforms`] to
/// update hierarchy/world rendering snapshots.
///
/// # Errors
///
/// Returns [`CharacterMotorError`] if any motor cannot complete its step. No
/// later entities are stepped after the first error; already-stepped entities
/// retain their completed state.
pub fn step_character_motors_3d<F>(
    world: &mut World,
    mut input_for: F,
) -> Result<Vec<CharacterMotorEntityEvent3d>, CharacterMotorError>
where
    F: FnMut(Entity) -> CharacterInput3d,
{
    let mut entities: Vec<Entity> = world
        .query::<(Entity, &CharacterMotor3d, &LocalTransform3d)>()
        .iter(world)
        .map(|(entity, _, _)| entity)
        .collect();
    entities.sort_by_key(|entity| entity.to_bits());

    let mut events = Vec::new();
    for entity in entities {
        let input = input_for(entity);
        let mut entity_world = world.entity_mut(entity);
        let (step, position) = {
            let mut motor = entity_world.get_mut::<CharacterMotor3d>().ok_or(
                CharacterMotorError::MissingEcsComponent {
                    entity,
                    component: "CharacterMotor3d",
                },
            )?;
            let step = motor.step(input)?;
            (step, motor.position())
        };
        let mut transform = entity_world.get_mut::<LocalTransform3d>().ok_or(
            CharacterMotorError::MissingEcsComponent {
                entity,
                component: "LocalTransform3d",
            },
        )?;
        transform.translation = [position.x, position.y, position.z];
        events.extend(
            step.events()
                .iter()
                .copied()
                .map(|event| CharacterMotorEntityEvent3d { entity, event }),
        );
    }
    Ok(events)
}

/// Failure while configuring or stepping a prototype character motor.
#[derive(Debug)]
pub enum CharacterMotorError {
    /// A configuration field is not compatible with this fixed ground-plane motor.
    InvalidConfig {
        /// Invalid configuration field.
        field: &'static str,
        /// Stable human-readable constraint.
        reason: &'static str,
    },
    /// An initial or teleported character position was non-finite.
    InvalidPosition,
    /// Caller-supplied desired movement was non-finite.
    InvalidInput,
    /// Fixed-step arithmetic could not retain a finite state.
    InvalidSimulation,
    /// The internal collision sphere could not be configured.
    Physics(yuyib_physics::PhysicsConfigError),
    /// An ECS entity changed after discovery and lost a required component.
    MissingEcsComponent {
        /// Entity selected by the adapter's deterministic discovery pass.
        entity: Entity,
        /// Required component absent at update time.
        component: &'static str,
    },
}

impl fmt::Display for CharacterMotorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig { field, reason } => {
                write!(
                    formatter,
                    "invalid character motor config {field}: {reason}"
                )
            }
            Self::InvalidPosition => formatter.write_str("character position must be finite"),
            Self::InvalidInput => formatter.write_str("character input movement must be finite"),
            Self::InvalidSimulation => {
                formatter.write_str("character fixed-step simulation produced non-finite state")
            }
            Self::Physics(source) => {
                write!(formatter, "invalid character collision sphere: {source}")
            }
            Self::MissingEcsComponent { entity, component } => write!(
                formatter,
                "character entity {entity:?} lost required component {component} during update"
            ),
        }
    }
}

impl Error for CharacterMotorError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Physics(source) => Some(source),
            Self::InvalidConfig { .. }
            | Self::InvalidPosition
            | Self::InvalidInput
            | Self::InvalidSimulation
            | Self::MissingEcsComponent { .. } => None,
        }
    }
}

/// Failure while configuring or stepping [`CharacterController3d`].
#[derive(Debug)]
pub enum CharacterControllerError3d {
    /// A setting is incompatible with the finite fixed-step controller.
    InvalidConfig {
        /// Invalid configuration field.
        field: &'static str,
        /// Stable human-readable constraint.
        reason: &'static str,
    },
    /// A starting, teleported or resolved position was non-finite.
    InvalidPosition,
    /// A collision resolver returned a non-finite position.
    InvalidCollisionResult,
    /// The internal contact sphere could not be configured.
    Physics(yuyib_physics::PhysicsConfigError),
    /// The configured or custom collision query failed.
    Collision(CharacterCollisionError3d),
    /// No map surface met the requested floor, sphere-clearance and headroom policy.
    ///
    /// The embedded report explains which filters exhausted the candidate set.
    NoPlayableSpawn(CharacterSpawnReport3d),
    /// Fixed-step arithmetic could not retain a finite state.
    InvalidSimulation,
}

impl fmt::Display for CharacterControllerError3d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig { field, reason } => {
                write!(
                    formatter,
                    "invalid character controller config {field}: {reason}"
                )
            }
            Self::InvalidPosition => {
                formatter.write_str("character controller position must be finite")
            }
            Self::InvalidCollisionResult => {
                formatter.write_str("character collision resolver returned a non-finite position")
            }
            Self::Physics(source) => {
                write!(formatter, "invalid character collision sphere: {source}")
            }
            Self::Collision(source) => write!(formatter, "character collision failed: {source}"),
            Self::NoPlayableSpawn(report) => write!(
                formatter,
                "static triangle map has no playable spawn with the requested clearance policy \
                 (surface_triangles={}, ranked={}, rejects={})",
                report.surface_triangle_count,
                report.ranked_candidate_count,
                report.reject_counts.total(),
            ),
            Self::InvalidSimulation => formatter
                .write_str("character controller fixed-step simulation produced non-finite state"),
        }
    }
}

impl Error for CharacterControllerError3d {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Physics(source) => Some(source),
            Self::Collision(source) => Some(source),
            Self::InvalidConfig { .. }
            | Self::InvalidPosition
            | Self::InvalidCollisionResult
            | Self::NoPlayableSpawn(_)
            | Self::InvalidSimulation => None,
        }
    }
}

fn cancel_velocity_against_correction(velocity: &mut Vec3, correction: Vec3) {
    // A correction opposite to velocity means the sphere reached a blocking
    // face on that axis. Cancelling only that component retains ordinary
    // corner sliding without requiring a normal from every custom backend.
    if correction.x * velocity.x < 0.0 {
        velocity.x = 0.0;
    }
    if correction.y * velocity.y < 0.0 {
        velocity.y = 0.0;
    }
    if correction.z * velocity.z < 0.0 {
        velocity.z = 0.0;
    }
}

fn cross3(left: Vec3, right: Vec3) -> Vec3 {
    Vec3::new(
        left.y * right.z - left.z * right.y,
        left.z * right.x - left.x * right.z,
        left.x * right.y - left.y * right.x,
    )
}

fn validate_controller_finite(
    value: f32,
    field: &'static str,
) -> Result<(), CharacterControllerError3d> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(CharacterControllerError3d::InvalidConfig {
            field,
            reason: "must be finite",
        })
    }
}

fn validate_controller_positive(
    value: f32,
    field: &'static str,
) -> Result<(), CharacterControllerError3d> {
    validate_controller_finite(value, field)?;
    if value > 0.0 {
        Ok(())
    } else {
        Err(CharacterControllerError3d::InvalidConfig {
            field,
            reason: "must be positive",
        })
    }
}

fn validate_controller_non_negative(
    value: f32,
    field: &'static str,
) -> Result<(), CharacterControllerError3d> {
    validate_controller_finite(value, field)?;
    if value >= 0.0 {
        Ok(())
    } else {
        Err(CharacterControllerError3d::InvalidConfig {
            field,
            reason: "must be non-negative",
        })
    }
}

fn validate_controller_position(value: Vec3) -> Result<(), CharacterControllerError3d> {
    finite_vec3(value)
        .then_some(())
        .ok_or(CharacterControllerError3d::InvalidPosition)
}

fn validate_finite(value: f32, field: &'static str) -> Result<(), CharacterMotorError> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(CharacterMotorError::InvalidConfig {
            field,
            reason: "must be finite",
        })
    }
}

fn validate_positive(value: f32, field: &'static str) -> Result<(), CharacterMotorError> {
    validate_finite(value, field)?;
    if value > 0.0 {
        Ok(())
    } else {
        Err(CharacterMotorError::InvalidConfig {
            field,
            reason: "must be positive",
        })
    }
}

fn validate_non_negative(value: f32, field: &'static str) -> Result<(), CharacterMotorError> {
    validate_finite(value, field)?;
    if value >= 0.0 {
        Ok(())
    } else {
        Err(CharacterMotorError::InvalidConfig {
            field,
            reason: "must be non-negative",
        })
    }
}

fn validate_position(position: Vec3) -> Result<(), CharacterMotorError> {
    finite_vec3(position)
        .then_some(())
        .ok_or(CharacterMotorError::InvalidPosition)
}

fn finite_vec3(value: Vec3) -> bool {
    value.x.is_finite() && value.y.is_finite() && value.z.is_finite()
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // Inputs are chosen for exact fixed-step results.
mod tests {
    use super::*;

    #[test]
    fn motor_moves_at_fixed_speed_and_normalizes_diagonal_input() {
        let config = CharacterMotorConfig3d {
            fixed_delta_seconds: 0.5,
            gravity_y: 0.0,
            move_speed: 4.0,
            jump_speed: 1.0,
            ground_y: 0.0,
            radius: 0.5,
        };
        let mut motor =
            CharacterMotor3d::new(config, Vec3::new(0.0, 0.5, 0.0)).expect("valid motor");
        motor
            .step(CharacterInput3d::new(Vec2::new(3.0, 0.0), false).expect("valid input"))
            .expect("fixed step");
        assert_eq!(motor.position(), Vec3::new(2.0, 0.5, 0.0));
        assert!(motor.is_grounded());
    }

    #[test]
    fn jump_and_landing_emit_ordered_events() {
        let config = CharacterMotorConfig3d {
            fixed_delta_seconds: 1.0,
            gravity_y: -2.0,
            move_speed: 0.0,
            jump_speed: 4.0,
            ground_y: 0.0,
            radius: 0.5,
        };
        let mut motor =
            CharacterMotor3d::new(config, Vec3::new(0.0, 0.5, 0.0)).expect("valid motor");
        let jumped = motor
            .step(CharacterInput3d::new(Vec2::ZERO, true).expect("valid input"))
            .expect("jump step");
        assert_eq!(jumped.events(), &[CharacterMotorEvent3d::Jumped]);
        assert_eq!(motor.position().y, 2.5);
        let landed = motor.step(CharacterInput3d::idle()).expect("falling step");
        assert!(landed.is_empty());
        let landed = motor.step(CharacterInput3d::idle()).expect("landing step");
        assert_eq!(landed.events(), &[CharacterMotorEvent3d::Landed]);
        assert_eq!(motor.position().y, 0.5);
    }

    #[test]
    fn ecs_adapter_orders_callback_and_writes_local_transform() {
        let config = CharacterMotorConfig3d {
            fixed_delta_seconds: 1.0,
            gravity_y: 0.0,
            move_speed: 1.0,
            jump_speed: 0.0,
            ground_y: 0.0,
            radius: 0.5,
        };
        let mut world = World::new();
        let first = world
            .spawn((
                CharacterMotor3d::new(config, Vec3::new(0.0, 0.5, 0.0)).expect("motor"),
                LocalTransform3d::IDENTITY,
            ))
            .id();
        let second = world
            .spawn((
                CharacterMotor3d::new(config, Vec3::new(0.0, 0.5, 0.0)).expect("motor"),
                LocalTransform3d::IDENTITY,
            ))
            .id();
        let mut called = Vec::new();
        step_character_motors_3d(&mut world, |entity| {
            called.push(entity);
            CharacterInput3d::new(Vec2::new(1.0, 0.0), false).expect("valid input")
        })
        .expect("valid ECS step");
        let mut expected = [first, second];
        expected.sort_by_key(|entity| entity.to_bits());
        assert_eq!(called, expected);
        assert_eq!(
            world
                .get::<LocalTransform3d>(first)
                .expect("local transform")
                .translation,
            [1.0, 0.5, 0.0]
        );
    }

    #[test]
    fn invalid_config_and_input_are_rejected() {
        assert!(
            CharacterMotorConfig3d {
                gravity_y: 1.0,
                ..CharacterMotorConfig3d::default()
            }
            .validate()
            .is_err()
        );
        assert!(CharacterInput3d::new(Vec2::new(f32::NAN, 0.0), false).is_err());
    }

    #[test]
    fn triangle_controller_lands_and_stops_at_a_wall() {
        let vertices = [
            // Floor, wound upward.
            Vec3::new(-2.0, 0.0, -2.0),
            Vec3::new(-2.0, 0.0, 2.0),
            Vec3::new(2.0, 0.0, -2.0),
            Vec3::new(2.0, 0.0, 2.0),
            // Wall at X = 1, wound towards the player at the origin.
            Vec3::new(1.0, 0.0, -2.0),
            Vec3::new(1.0, 2.0, 2.0),
            Vec3::new(1.0, 2.0, -2.0),
            Vec3::new(1.0, 0.0, 2.0),
        ];
        let mesh = TriangleMesh3d::from_indexed(&vertices, &[0, 1, 2, 2, 1, 3, 4, 5, 6, 4, 7, 5])
            .expect("valid static floor and wall");
        let config = CharacterControllerConfig3d {
            fixed_delta_seconds: 0.1,
            gravity_y: -1.0,
            move_speed: 10.0,
            jump_speed: 3.0,
            radius: 0.25,
            collision_iterations: 4,
        };
        let mut controller =
            CharacterController3d::new(config, Vec3::new(0.0, 0.2, 0.0)).expect("valid controller");

        let landed = controller
            .step_on_triangle_mesh(CharacterInput3d::idle(), &mesh)
            .expect("fall onto floor");
        assert!(controller.is_grounded());
        assert!(
            landed
                .events()
                .contains(&CharacterControllerEvent3d::Landed)
        );

        let hit_wall = controller
            .step_on_triangle_mesh(
                CharacterInput3d::new(Vec2::new(1.0, 0.0), false).expect("valid input"),
                &mesh,
            )
            .expect("move into wall");
        assert!(hit_wall.contacts() > 0);
        assert!(
            hit_wall
                .events()
                .contains(&CharacterControllerEvent3d::Collided)
        );
        assert!(controller.position().x < 0.76);
        assert_eq!(controller.velocity().x, 0.0);
    }

    #[test]
    fn triangle_controller_jump_does_not_skip_a_thin_ceiling() {
        let vertices = [
            // Floor at y=0 and ceiling at y=1. Their winding is deliberately
            // mixed: sphere contact must not depend on authoring winding.
            Vec3::new(-2.0, 0.0, -2.0),
            Vec3::new(-2.0, 0.0, 2.0),
            Vec3::new(2.0, 0.0, -2.0),
            Vec3::new(2.0, 0.0, 2.0),
            Vec3::new(-2.0, 1.0, -2.0),
            Vec3::new(-2.0, 1.0, 2.0),
            Vec3::new(2.0, 1.0, -2.0),
            Vec3::new(2.0, 1.0, 2.0),
        ];
        let mesh = TriangleMesh3d::from_indexed(&vertices, &[0, 1, 2, 2, 1, 3, 4, 6, 5, 5, 6, 7])
            .expect("valid floor and ceiling");
        let config = CharacterControllerConfig3d {
            fixed_delta_seconds: 0.1,
            gravity_y: -1.0,
            move_speed: 0.0,
            jump_speed: 10.0,
            radius: 0.25,
            collision_iterations: 4,
        };
        let mut controller =
            CharacterController3d::new(config, Vec3::new(0.0, 0.25, 0.0)).expect("controller");

        controller
            .step_on_triangle_mesh(CharacterInput3d::idle(), &mesh)
            .expect("land on floor");
        assert!(controller.is_grounded());

        let hit_ceiling = controller
            .step_on_triangle_mesh(
                CharacterInput3d::new(Vec2::ZERO, true).expect("jump input"),
                &mesh,
            )
            .expect("jump resolves against ceiling");
        assert!(
            hit_ceiling
                .events()
                .contains(&CharacterControllerEvent3d::Collided)
        );
        assert!(controller.position().y <= 0.7506);
        assert_eq!(controller.velocity().y, 0.0);
        assert!(!controller.is_grounded());
    }

    #[test]
    fn controller_accepts_jump_and_exposes_custom_collision_hook() {
        let config = CharacterControllerConfig3d {
            fixed_delta_seconds: 0.25,
            gravity_y: -2.0,
            move_speed: 0.0,
            jump_speed: 4.0,
            radius: 0.5,
            collision_iterations: 1,
        };
        let mut controller =
            CharacterController3d::new(config, Vec3::ZERO).expect("valid controller");

        controller
            .step_with_collision(CharacterInput3d::idle(), |position, _| {
                Ok(CharacterCollisionResolution3d {
                    position,
                    grounded: true,
                    contacts: 1,
                })
            })
            .expect("custom ground resolver");
        let jumped = controller
            .step_with_collision(
                CharacterInput3d::new(Vec2::ZERO, true).expect("valid input"),
                |position, _| {
                    Ok(CharacterCollisionResolution3d {
                        position,
                        grounded: false,
                        contacts: 0,
                    })
                },
            )
            .expect("custom air resolver");
        assert!(
            jumped
                .events()
                .contains(&CharacterControllerEvent3d::Jumped)
        );
        assert!(controller.position().y > 0.0);
        assert!(!controller.is_grounded());
    }

    #[test]
    fn map_spawn_selects_enclosed_floor_and_rejects_exposed_surface() {
        let vertices = [
            // A 4x4 floor at y=0.
            Vec3::new(-2.0, 0.0, -2.0),
            Vec3::new(-2.0, 0.0, 2.0),
            Vec3::new(2.0, 0.0, -2.0),
            Vec3::new(2.0, 0.0, 2.0),
            // The matching ceiling at y=3.
            Vec3::new(-2.0, 3.0, -2.0),
            Vec3::new(-2.0, 3.0, 2.0),
            Vec3::new(2.0, 3.0, -2.0),
            Vec3::new(2.0, 3.0, 2.0),
            // An exposed, otherwise valid horizontal triangle at y=5.
            Vec3::new(8.0, 5.0, -1.0),
            Vec3::new(10.0, 5.0, -1.0),
            Vec3::new(9.0, 5.0, 1.0),
        ];
        let mesh = TriangleMesh3d::from_indexed(
            &vertices,
            &[0, 1, 2, 2, 1, 3, 4, 6, 5, 5, 6, 7, 8, 9, 10],
        )
        .expect("valid enclosed floor and exposed roof");
        let config = CharacterControllerConfig3d {
            radius: 0.35,
            ..CharacterControllerConfig3d::default()
        };
        let player = CharacterController3d::spawn_in_triangle_mesh(config, &mesh)
            .expect("the enclosed floor must be chosen");
        assert!(player.is_grounded());
        assert!(player.position().y < 1.0);
        assert!(player.position().x.abs() <= 2.0);
    }

    #[test]
    fn outdoor_lowest_policy_selects_street_below_overlapping_roof() {
        let vertices = [
            // Roof first: nearest-to-anchor ties intentionally retain source order.
            Vec3::new(-2.0, 6.0, -2.0),
            Vec3::new(2.0, 6.0, -2.0),
            Vec3::new(0.0, 6.0, 2.0),
            // Street directly below it at the same horizontal anchor.
            Vec3::new(-2.0, 0.0, -2.0),
            Vec3::new(2.0, 0.0, -2.0),
            Vec3::new(0.0, 0.0, 2.0),
        ];
        let mesh = TriangleMesh3d::from_indexed(&vertices, &[0, 1, 2, 3, 4, 5])
            .expect("valid vertically layered outdoor mesh");
        let config = CharacterControllerConfig3d::default();

        let nearest = CharacterController3d::spawn_in_triangle_mesh_with_options(
            config,
            &mesh,
            CharacterSpawnOptions3d {
                require_ceiling: false,
                ..CharacterSpawnOptions3d::default()
            },
        )
        .expect("backward-compatible nearest policy accepts the first tied surface");
        assert!(nearest.position().y > 6.0);

        let street = CharacterController3d::spawn_in_triangle_mesh_with_options(
            config,
            &mesh,
            CharacterSpawnOptions3d::outdoor_lowest(Vec2::ZERO),
        )
        .expect("outdoor lowest policy finds the valid street");
        assert!(street.position().y < 1.0);

        let roof = CharacterController3d::spawn_in_triangle_mesh_with_options(
            config,
            &mesh,
            CharacterSpawnOptions3d::outdoor_lowest(Vec2::ZERO)
                .with_surface_selection(CharacterSpawnSurfaceSelection3d::ClosestToElevation(6.0)),
        )
        .expect("explicit elevation target selects the roof");
        assert!(roof.position().y > 6.0);
    }

    #[test]
    fn semantic_surface_spawn_uses_full_map_for_open_sky_clearance() {
        let surfaces = TriangleMesh3d::from_indexed(
            &[
                Vec3::new(-1.0, 0.0, -1.0),
                Vec3::new(1.0, 0.0, -1.0),
                Vec3::new(0.0, 0.0, 1.0),
                Vec3::new(4.0, 0.0, -1.0),
                Vec3::new(6.0, 0.0, -1.0),
                Vec3::new(5.0, 0.0, 1.0),
            ],
            &[0, 1, 2, 3, 4, 5],
        )
        .expect("two semantic spawn surfaces");
        let full_map = TriangleMesh3d::from_indexed(
            &[
                Vec3::new(-1.0, 0.0, -1.0),
                Vec3::new(1.0, 0.0, -1.0),
                Vec3::new(0.0, 0.0, 1.0),
                Vec3::new(4.0, 0.0, -1.0),
                Vec3::new(6.0, 0.0, -1.0),
                Vec3::new(5.0, 0.0, 1.0),
                Vec3::new(-2.0, 3.0, -2.0),
                Vec3::new(2.0, 3.0, -2.0),
                Vec3::new(0.0, 3.0, 2.0),
            ],
            &[0, 1, 2, 3, 4, 5, 6, 7, 8],
        )
        .expect("full map with one indoor surface");

        let spawn = CharacterController3d::spawn_on_surface_mesh_with_options(
            CharacterControllerConfig3d::default(),
            &surfaces,
            &full_map,
            CharacterSpawnOptions3d::outdoor_lowest(Vec2::ZERO)
                .with_maximum_horizontal_distance(8.0)
                .with_minimum_open_sky_clearance(10.0),
        )
        .expect("outdoor semantic surface remains available");

        assert!(spawn.position().x > 3.0);
    }

    #[test]
    fn preferred_anchor_and_search_radius_localize_spawn_selection() {
        let vertices = [
            Vec3::new(-11.0, 0.0, -1.0),
            Vec3::new(-9.0, 0.0, -1.0),
            Vec3::new(-10.0, 0.0, 2.0),
            Vec3::new(9.0, 0.0, -1.0),
            Vec3::new(11.0, 0.0, -1.0),
            Vec3::new(10.0, 0.0, 2.0),
        ];
        let mesh = TriangleMesh3d::from_indexed(&vertices, &[0, 1, 2, 3, 4, 5])
            .expect("valid separated platforms");
        let config = CharacterControllerConfig3d::default();
        let right = CharacterController3d::spawn_in_triangle_mesh_with_options(
            config,
            &mesh,
            CharacterSpawnOptions3d::outdoor_lowest(Vec2::new(10.0, 0.0))
                .with_maximum_horizontal_distance(2.0),
        )
        .expect("preferred district contains one valid platform");
        assert!(right.position().x > 9.0);

        let missing = CharacterController3d::spawn_in_triangle_mesh_with_options(
            config,
            &mesh,
            CharacterSpawnOptions3d::outdoor_lowest(Vec2::ZERO)
                .with_maximum_horizontal_distance(2.0),
        );
        match missing {
            Err(CharacterControllerError3d::NoPlayableSpawn(report)) => {
                assert!(report.selected.is_none());
                assert_eq!(report.surface_triangle_count, 2);
                assert_eq!(report.ranked_candidate_count, 0);
                assert!(report.reject_counts.outside_horizontal_radius >= 1);
            }
            other => panic!("expected NoPlayableSpawn with report, got {other:?}"),
        }
    }

    #[test]
    fn spawn_report_names_selected_triangle_and_reject_tallies() {
        let vertices = [
            Vec3::new(-1.0, 0.0, -1.0),
            Vec3::new(1.0, 0.0, -1.0),
            Vec3::new(0.0, 0.0, 1.0),
            Vec3::new(-1.0, 0.0, 4.0),
            Vec3::new(1.0, 0.0, 4.0),
            Vec3::new(0.0, 0.0, 6.0),
            // Steep wall — must be counted as SteepNormal, not ranked.
            Vec3::new(-1.0, 0.0, 8.0),
            Vec3::new(1.0, 0.0, 8.0),
            Vec3::new(0.0, 2.0, 8.0),
        ];
        let mesh = TriangleMesh3d::from_indexed(&vertices, &[0, 1, 2, 3, 4, 5, 6, 7, 8])
            .expect("platform plus steep face");
        let (controller, report) = CharacterController3d::spawn_on_surface_mesh_with_report(
            CharacterControllerConfig3d::default(),
            &mesh,
            &mesh,
            CharacterSpawnOptions3d::outdoor_lowest(Vec2::ZERO),
        )
        .expect("flat platforms remain spawnable");

        assert!(report.selected.is_some());
        let selected = report.selected.expect("selection recorded");
        assert_eq!(selected.position, controller.position());
        assert_eq!(report.surface_triangle_count, 3);
        assert_eq!(report.ranked_candidate_count, 2);
        assert_eq!(report.reject_counts.steep_normal, 1);
        assert!(selected.floor.y < 0.1);
    }

    #[test]
    fn spawn_anchor_radius_and_elevation_must_be_finite() {
        let vertices = [
            Vec3::new(-1.0, 0.0, -1.0),
            Vec3::new(1.0, 0.0, -1.0),
            Vec3::new(0.0, 0.0, 1.0),
        ];
        let mesh =
            TriangleMesh3d::from_indexed(&vertices, &[0, 1, 2]).expect("valid test platform");
        let config = CharacterControllerConfig3d::default();

        for options in [
            CharacterSpawnOptions3d::outdoor_lowest(Vec2::new(f32::NAN, 0.0)),
            CharacterSpawnOptions3d::outdoor_lowest(Vec2::ZERO)
                .with_maximum_horizontal_distance(f32::INFINITY),
            CharacterSpawnOptions3d::outdoor_lowest(Vec2::ZERO).with_surface_selection(
                CharacterSpawnSurfaceSelection3d::ClosestToElevation(f32::NAN),
            ),
            CharacterSpawnOptions3d::outdoor_lowest(Vec2::ZERO)
                .with_minimum_open_sky_clearance(0.5),
        ] {
            assert!(matches!(
                CharacterController3d::spawn_in_triangle_mesh_with_options(config, &mesh, options,),
                Err(CharacterControllerError3d::InvalidConfig { .. })
            ));
        }
    }
}
