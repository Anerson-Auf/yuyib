//! Rapier 2D adapter for [`super::DynamicsBackend2d`].

use std::collections::HashSet;

use rapier2d::control::{CharacterLength, KinematicCharacterController};
use rapier2d::prelude::{
    BroadPhaseBvh, CCDSolver, ColliderBuilder, ColliderHandle, ColliderSet, FixedJointBuilder,
    Group, ImpulseJointHandle, ImpulseJointSet, IntegrationParameters, InteractionGroups,
    InteractionTestMode, IslandManager, MultibodyJointSet, NarrowPhase, PhysicsPipeline,
    PrismaticJointBuilder, QueryFilter, QueryFilterFlags, RevoluteJointBuilder, RigidBodyBuilder,
    RigidBodyHandle, RigidBodySet, RopeJointBuilder, Vector,
};

use super::{
    BodyId2d, CharacterMoveConfig2d, CharacterMoveResult2d, CollisionGroups2d, ContactPair2d,
    DynamicsBackend2d, DynamicsBackendError2d, DynamicsWorldConfig2d, JointId2d, require_finite2,
    require_positive_2d,
};

/// Rapier-backed 2D dynamics world used by the M4.13 facade path.
pub struct RapierDynamicsWorld2d {
    gravity: [f32; 2],
    default_dt: f32,
    pipeline: PhysicsPipeline,
    integration_parameters: IntegrationParameters,
    islands: IslandManager,
    broad_phase: BroadPhaseBvh,
    narrow_phase: NarrowPhase,
    bodies: RigidBodySet,
    colliders: ColliderSet,
    impulse_joints: ImpulseJointSet,
    multibody_joints: MultibodyJointSet,
    ccd_solver: CCDSolver,
    one_way_bodies: HashSet<BodyId2d>,
}

impl RapierDynamicsWorld2d {
    /// Creates an empty Rapier 2D world with the supplied gravity/default dt.
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError2d`] when gravity or `default_dt` is invalid.
    pub fn new(config: DynamicsWorldConfig2d) -> Result<Self, DynamicsBackendError2d> {
        let gravity = require_finite2(config.gravity)?;
        let default_dt = require_positive_2d(config.default_dt)?;
        let mut integration_parameters = IntegrationParameters::default();
        integration_parameters.dt = default_dt;
        Ok(Self {
            gravity,
            default_dt,
            pipeline: PhysicsPipeline::new(),
            integration_parameters,
            islands: IslandManager::new(),
            broad_phase: BroadPhaseBvh::new(),
            narrow_phase: NarrowPhase::new(),
            bodies: RigidBodySet::new(),
            colliders: ColliderSet::new(),
            impulse_joints: ImpulseJointSet::new(),
            multibody_joints: MultibodyJointSet::new(),
            ccd_solver: CCDSolver::new(),
            one_way_bodies: HashSet::new(),
        })
    }

    /// Inserts a fixed axis-aligned cuboid collider.
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError2d`] for non-finite centres or non-positive
    /// half extents.
    pub fn insert_fixed_cuboid(
        &mut self,
        center: [f32; 2],
        half_extents: [f32; 2],
    ) -> Result<BodyId2d, DynamicsBackendError2d> {
        let center = require_finite2(center)?;
        let hx = require_positive_2d(half_extents[0])?;
        let hy = require_positive_2d(half_extents[1])?;
        let body = self.bodies.insert(
            RigidBodyBuilder::fixed().translation(Vector::new(center[0], center[1])),
        );
        self.colliders.insert_with_parent(
            ColliderBuilder::cuboid(hx, hy),
            body,
            &mut self.bodies,
        );
        Ok(body_id_from_handle(body))
    }

    /// Inserts a dynamic ball with zero initial velocity.
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError2d`] for non-finite centres or non-positive radii.
    pub fn insert_dynamic_ball(
        &mut self,
        center: [f32; 2],
        radius: f32,
    ) -> Result<BodyId2d, DynamicsBackendError2d> {
        let center = require_finite2(center)?;
        let radius = require_positive_2d(radius)?;
        let body = self.bodies.insert(
            RigidBodyBuilder::dynamic().translation(Vector::new(center[0], center[1])),
        );
        self.colliders.insert_with_parent(
            ColliderBuilder::ball(radius),
            body,
            &mut self.bodies,
        );
        Ok(body_id_from_handle(body))
    }

    /// Inserts a dynamic axis-aligned cuboid with zero initial velocity.
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError2d`] for non-finite centres or non-positive
    /// half extents.
    pub fn insert_dynamic_cuboid(
        &mut self,
        center: [f32; 2],
        half_extents: [f32; 2],
    ) -> Result<BodyId2d, DynamicsBackendError2d> {
        let center = require_finite2(center)?;
        let hx = require_positive_2d(half_extents[0])?;
        let hy = require_positive_2d(half_extents[1])?;
        let body = self.bodies.insert(
            RigidBodyBuilder::dynamic().translation(Vector::new(center[0], center[1])),
        );
        self.colliders.insert_with_parent(
            ColliderBuilder::cuboid(hx, hy),
            body,
            &mut self.bodies,
        );
        Ok(body_id_from_handle(body))
    }

    /// Inserts a dynamic Y-axis capsule (`half_height` is the cylindrical half-length).
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError2d`] for non-finite centres or non-positive sizes.
    pub fn insert_dynamic_capsule(
        &mut self,
        center: [f32; 2],
        half_height: f32,
        radius: f32,
    ) -> Result<BodyId2d, DynamicsBackendError2d> {
        let center = require_finite2(center)?;
        let half_height = require_positive_2d(half_height)?;
        let radius = require_positive_2d(radius)?;
        let body = self.bodies.insert(
            RigidBodyBuilder::dynamic().translation(Vector::new(center[0], center[1])),
        );
        self.colliders.insert_with_parent(
            ColliderBuilder::capsule_y(half_height, radius),
            body,
            &mut self.bodies,
        );
        Ok(body_id_from_handle(body))
    }

    /// Sets the linear velocity of a dynamic or kinematic body.
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError2d`] for unknown bodies or non-finite velocity.
    pub fn set_linear_velocity(
        &mut self,
        body: BodyId2d,
        velocity: [f32; 2],
    ) -> Result<(), DynamicsBackendError2d> {
        let velocity = require_finite2(velocity)?;
        let handle =
            handle_from_body_id(body).ok_or(DynamicsBackendError2d::UnknownBody(body))?;
        let rigid = self
            .bodies
            .get_mut(handle)
            .ok_or(DynamicsBackendError2d::UnknownBody(body))?;
        rigid.set_linvel(Vector::new(velocity[0], velocity[1]), true);
        Ok(())
    }

    /// Inserts a velocity-based kinematic cuboid (moving platforms).
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError2d`] for invalid centres or extents.
    pub fn insert_kinematic_cuboid(
        &mut self,
        center: [f32; 2],
        half_extents: [f32; 2],
    ) -> Result<BodyId2d, DynamicsBackendError2d> {
        self.insert_kinematic_cuboid_with_mode(center, half_extents, false)
    }

    /// Inserts a position-based kinematic cuboid for scripted pose updates.
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError2d`] for invalid centres or extents.
    pub fn insert_kinematic_position_cuboid(
        &mut self,
        center: [f32; 2],
        half_extents: [f32; 2],
    ) -> Result<BodyId2d, DynamicsBackendError2d> {
        self.insert_kinematic_cuboid_with_mode(center, half_extents, true)
    }

    fn insert_kinematic_cuboid_with_mode(
        &mut self,
        center: [f32; 2],
        half_extents: [f32; 2],
        position_based: bool,
    ) -> Result<BodyId2d, DynamicsBackendError2d> {
        let center = require_finite2(center)?;
        let hx = require_positive_2d(half_extents[0])?;
        let hy = require_positive_2d(half_extents[1])?;
        let builder = if position_based {
            RigidBodyBuilder::kinematic_position_based()
        } else {
            RigidBodyBuilder::kinematic_velocity_based()
        };
        let body = self
            .bodies
            .insert(builder.translation(Vector::new(center[0], center[1])));
        self.colliders.insert_with_parent(
            ColliderBuilder::cuboid(hx, hy).friction(1.2),
            body,
            &mut self.bodies,
        );
        Ok(body_id_from_handle(body))
    }

    /// Sets the next translation for a **position-based** kinematic body.
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError2d`] for unknown bodies or non-finite input.
    pub fn set_next_kinematic_translation(
        &mut self,
        body: BodyId2d,
        translation: [f32; 2],
    ) -> Result<(), DynamicsBackendError2d> {
        let translation = require_finite2(translation)?;
        let handle =
            handle_from_body_id(body).ok_or(DynamicsBackendError2d::UnknownBody(body))?;
        let rigid = self
            .bodies
            .get_mut(handle)
            .ok_or(DynamicsBackendError2d::UnknownBody(body))?;
        rigid.set_next_kinematic_translation(Vector::new(translation[0], translation[1]));
        Ok(())
    }

    /// Immediately sets the world-space translation of a body (kinematic character sync).
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError2d`] for unknown bodies or non-finite input.
    pub fn set_translation(
        &mut self,
        body: BodyId2d,
        translation: [f32; 2],
    ) -> Result<(), DynamicsBackendError2d> {
        let translation = require_finite2(translation)?;
        let handle =
            handle_from_body_id(body).ok_or(DynamicsBackendError2d::UnknownBody(body))?;
        let rigid = self
            .bodies
            .get_mut(handle)
            .ok_or(DynamicsBackendError2d::UnknownBody(body))?;
        rigid.set_translation(Vector::new(translation[0], translation[1]), true);
        Ok(())
    }

    /// Inserts a position-based kinematic Y-capsule for character controllers.
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError2d`] for invalid centres or sizes.
    pub fn insert_kinematic_position_capsule(
        &mut self,
        center: [f32; 2],
        half_height: f32,
        radius: f32,
    ) -> Result<BodyId2d, DynamicsBackendError2d> {
        let center = require_finite2(center)?;
        let half_height = require_positive_2d(half_height)?;
        let radius = require_positive_2d(radius)?;
        let body = self.bodies.insert(
            RigidBodyBuilder::kinematic_position_based()
                .translation(Vector::new(center[0], center[1]))
                .lock_rotations(),
        );
        self.colliders.insert_with_parent(
            ColliderBuilder::capsule_y(half_height, radius).friction(0.0),
            body,
            &mut self.bodies,
        );
        Ok(body_id_from_handle(body))
    }

    /// Inserts a fixed one-way platform cuboid (land from above; jump through from below).
    ///
    /// Filtering is applied by [`Self::move_kinematic_character`] using
    /// [`CharacterMoveConfig2d::vertical_filter`].
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError2d`] for invalid centres or extents.
    pub fn insert_one_way_platform_cuboid(
        &mut self,
        center: [f32; 2],
        half_extents: [f32; 2],
    ) -> Result<BodyId2d, DynamicsBackendError2d> {
        let id = self.insert_fixed_cuboid(center, half_extents)?;
        self.one_way_bodies.insert(id);
        Ok(id)
    }

    /// Returns whether `body` is registered as a one-way platform.
    #[must_use]
    pub fn is_one_way_platform(&self, body: BodyId2d) -> bool {
        self.one_way_bodies.contains(&body)
    }

    /// Moves a kinematic character capsule/shape with Rapier's character controller.
    ///
    /// Refreshes query geometry, excludes the character rigid body and sensors,
    /// and filters one-way platforms when `config.vertical_filter > 0` (going up)
    /// or when the character centre is clearly below the platform.
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError2d`] for unknown bodies, missing colliders,
    /// or non-finite input.
    pub fn move_kinematic_character(
        &mut self,
        body: BodyId2d,
        desired_translation: [f32; 2],
        dt: f32,
        config: CharacterMoveConfig2d,
    ) -> Result<CharacterMoveResult2d, DynamicsBackendError2d> {
        let desired_translation = require_finite2(desired_translation)?;
        let dt = require_positive_2d(dt)?;
        if !config.max_slope_climb_angle.is_finite() || !config.vertical_filter.is_finite() {
            return Err(DynamicsBackendError2d::NonFiniteInput);
        }
        let handle =
            handle_from_body_id(body).ok_or(DynamicsBackendError2d::UnknownBody(body))?;
        if self.bodies.get(handle).is_none() {
            return Err(DynamicsBackendError2d::UnknownBody(body));
        }

        self.refresh_broad_phase();

        let (character_pos, shape_handle) = {
            let rigid = self
                .bodies
                .get(handle)
                .ok_or(DynamicsBackendError2d::UnknownBody(body))?;
            let collider_handle = *rigid
                .colliders()
                .first()
                .ok_or(DynamicsBackendError2d::UnknownBody(body))?;
            (*rigid.position(), collider_handle)
        };

        let character_center = [character_pos.translation.x, character_pos.translation.y];
        let going_up = config.vertical_filter > 0.0;
        let one_way = self.one_way_bodies.clone();

        let filter = QueryFilter {
            flags: QueryFilterFlags::EXCLUDE_SENSORS,
            exclude_rigid_body: Some(handle),
            predicate: Some(&|_: ColliderHandle, collider| {
                let Some(parent) = collider.parent() else {
                    return true;
                };
                let parent_id = body_id_from_handle(parent);
                if !one_way.contains(&parent_id) {
                    return true;
                }
                if going_up {
                    return false;
                }
                let platform_y = collider.position().translation.y;
                let platform_top = collider
                    .shape()
                    .as_cuboid()
                    .map(|cuboid| platform_y + cuboid.half_extents.y)
                    .unwrap_or(platform_y);
                character_center[1] >= platform_top - 0.05
            }),
            ..QueryFilter::default()
        };

        let controller = KinematicCharacterController {
            up: Vector::Y,
            offset: CharacterLength::Relative(0.01),
            slide: true,
            autostep: None,
            max_slope_climb_angle: config.max_slope_climb_angle,
            min_slope_slide_angle: config.max_slope_climb_angle,
            snap_to_ground: if config.snap_to_ground {
                Some(CharacterLength::Relative(0.2))
            } else {
                None
            },
            normal_nudge_factor: 1.0e-4,
        };

        let movement = {
            let queries = self.broad_phase.as_query_pipeline(
                self.narrow_phase.query_dispatcher(),
                &self.bodies,
                &self.colliders,
                filter,
            );
            let shape = self
                .colliders
                .get(shape_handle)
                .ok_or(DynamicsBackendError2d::UnknownBody(body))?
                .shape();
            controller.move_shape(
                dt,
                &queries,
                shape,
                &character_pos,
                Vector::new(desired_translation[0], desired_translation[1]),
                |_| {},
            )
        };

        let new_translation = [
            character_pos.translation.x + movement.translation.x,
            character_pos.translation.y + movement.translation.y,
        ];
        self.set_translation(body, new_translation)?;

        Ok(CharacterMoveResult2d {
            translation: [movement.translation.x, movement.translation.y],
            grounded: movement.grounded,
            sliding_down_slope: movement.is_sliding_down_slope,
        })
    }

    fn refresh_broad_phase(&mut self) {
        let modified = self.colliders.take_modified();
        if modified.is_empty() {
            return;
        }
        let mut events = Vec::new();
        self.broad_phase.update(
            &self.integration_parameters,
            &self.colliders,
            &self.bodies,
            &modified,
            &[],
            &mut events,
        );
    }

    /// Inserts a fixed sensor cuboid (trigger volume — no collision response).
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError2d`] for invalid centres or extents.
    pub fn insert_trigger_cuboid(
        &mut self,
        center: [f32; 2],
        half_extents: [f32; 2],
    ) -> Result<BodyId2d, DynamicsBackendError2d> {
        let center = require_finite2(center)?;
        let hx = require_positive_2d(half_extents[0])?;
        let hy = require_positive_2d(half_extents[1])?;
        let body = self.bodies.insert(
            RigidBodyBuilder::fixed().translation(Vector::new(center[0], center[1])),
        );
        self.colliders.insert_with_parent(
            ColliderBuilder::cuboid(hx, hy).sensor(true),
            body,
            &mut self.bodies,
        );
        Ok(body_id_from_handle(body))
    }

    /// Inserts a fixed sensor ball (trigger volume).
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError2d`] for invalid centres or radii.
    pub fn insert_trigger_ball(
        &mut self,
        center: [f32; 2],
        radius: f32,
    ) -> Result<BodyId2d, DynamicsBackendError2d> {
        let center = require_finite2(center)?;
        let radius = require_positive_2d(radius)?;
        let body = self.bodies.insert(
            RigidBodyBuilder::fixed().translation(Vector::new(center[0], center[1])),
        );
        self.colliders.insert_with_parent(
            ColliderBuilder::ball(radius).sensor(true),
            body,
            &mut self.bodies,
        );
        Ok(body_id_from_handle(body))
    }

    /// Enables or disables continuous collision detection on a body.
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError2d`] for unknown bodies.
    pub fn set_ccd_enabled(
        &mut self,
        body: BodyId2d,
        enabled: bool,
    ) -> Result<(), DynamicsBackendError2d> {
        let handle =
            handle_from_body_id(body).ok_or(DynamicsBackendError2d::UnknownBody(body))?;
        let rigid = self
            .bodies
            .get_mut(handle)
            .ok_or(DynamicsBackendError2d::UnknownBody(body))?;
        rigid.enable_ccd(enabled);
        Ok(())
    }

    /// Returns current sensor intersection pairs as `(trigger_body, other_body)`.
    #[must_use]
    pub fn collect_trigger_overlaps(&self) -> Vec<(BodyId2d, BodyId2d)> {
        let mut pairs = Vec::new();
        for (handle_a, handle_b, intersecting) in self.narrow_phase.intersection_pairs() {
            if !intersecting {
                continue;
            }
            let Some(collider_a) = self.colliders.get(handle_a) else {
                continue;
            };
            let Some(collider_b) = self.colliders.get(handle_b) else {
                continue;
            };
            if !collider_a.is_sensor() && !collider_b.is_sensor() {
                continue;
            }
            let Some(parent_a) = collider_a.parent() else {
                continue;
            };
            let Some(parent_b) = collider_b.parent() else {
                continue;
            };
            let body_a = body_id_from_handle(parent_a);
            let body_b = body_id_from_handle(parent_b);
            let ordered = match (collider_a.is_sensor(), collider_b.is_sensor()) {
                (true, false) => (body_a, body_b),
                (false, true) => (body_b, body_a),
                _ => {
                    if (body_a.index(), body_a.generation())
                        <= (body_b.index(), body_b.generation())
                    {
                        (body_a, body_b)
                    } else {
                        (body_b, body_a)
                    }
                }
            };
            pairs.push(ordered);
        }
        pairs.sort_by_key(|(trigger, other)| {
            (
                trigger.index(),
                trigger.generation(),
                other.index(),
                other.generation(),
            )
        });
        pairs.dedup();
        pairs
    }

    /// Wakes a sleeping rigid body so upcoming contacts are simulated.
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError2d`] for unknown bodies.
    pub fn wake_up(&mut self, body: BodyId2d) -> Result<(), DynamicsBackendError2d> {
        let handle =
            handle_from_body_id(body).ok_or(DynamicsBackendError2d::UnknownBody(body))?;
        let rigid = self
            .bodies
            .get_mut(handle)
            .ok_or(DynamicsBackendError2d::UnknownBody(body))?;
        rigid.wake_up(true);
        Ok(())
    }

    /// Sets Rapier collision membership/filter on every collider attached to `body`.
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError2d`] for unknown bodies.
    pub fn set_collision_groups(
        &mut self,
        body: BodyId2d,
        groups: CollisionGroups2d,
    ) -> Result<(), DynamicsBackendError2d> {
        let handle =
            handle_from_body_id(body).ok_or(DynamicsBackendError2d::UnknownBody(body))?;
        let rigid = self
            .bodies
            .get(handle)
            .ok_or(DynamicsBackendError2d::UnknownBody(body))?;
        let collider_handles: Vec<_> = rigid.colliders().to_vec();
        let interaction = to_interaction_groups(groups);
        for collider_handle in collider_handles {
            if let Some(collider) = self.colliders.get_mut(collider_handle) {
                collider.set_collision_groups(interaction);
            }
        }
        Ok(())
    }

    /// Inserts a fixed impulse joint locking relative pose between two bodies.
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError2d`] for unknown bodies or non-finite anchors.
    pub fn insert_fixed_joint(
        &mut self,
        body_a: BodyId2d,
        body_b: BodyId2d,
        local_anchor_a: [f32; 2],
        local_anchor_b: [f32; 2],
    ) -> Result<JointId2d, DynamicsBackendError2d> {
        let local_anchor_a = require_finite2(local_anchor_a)?;
        let local_anchor_b = require_finite2(local_anchor_b)?;
        let handle_a =
            handle_from_body_id(body_a).ok_or(DynamicsBackendError2d::UnknownBody(body_a))?;
        let handle_b =
            handle_from_body_id(body_b).ok_or(DynamicsBackendError2d::UnknownBody(body_b))?;
        if self.bodies.get(handle_a).is_none() {
            return Err(DynamicsBackendError2d::UnknownBody(body_a));
        }
        if self.bodies.get(handle_b).is_none() {
            return Err(DynamicsBackendError2d::UnknownBody(body_b));
        }
        let joint = FixedJointBuilder::new()
            .local_anchor1(Vector::new(local_anchor_a[0], local_anchor_a[1]))
            .local_anchor2(Vector::new(local_anchor_b[0], local_anchor_b[1]))
            .build();
        let handle = self.impulse_joints.insert(handle_a, handle_b, joint, true);
        Ok(joint_id_from_handle(handle))
    }

    /// Inserts a revolute impulse joint (free rotation about the Z axis).
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError2d`] for unknown bodies or non-finite anchors.
    pub fn insert_revolute_joint(
        &mut self,
        body_a: BodyId2d,
        body_b: BodyId2d,
        local_anchor_a: [f32; 2],
        local_anchor_b: [f32; 2],
    ) -> Result<JointId2d, DynamicsBackendError2d> {
        let local_anchor_a = require_finite2(local_anchor_a)?;
        let local_anchor_b = require_finite2(local_anchor_b)?;
        let handle_a =
            handle_from_body_id(body_a).ok_or(DynamicsBackendError2d::UnknownBody(body_a))?;
        let handle_b =
            handle_from_body_id(body_b).ok_or(DynamicsBackendError2d::UnknownBody(body_b))?;
        if self.bodies.get(handle_a).is_none() {
            return Err(DynamicsBackendError2d::UnknownBody(body_a));
        }
        if self.bodies.get(handle_b).is_none() {
            return Err(DynamicsBackendError2d::UnknownBody(body_b));
        }
        let joint = RevoluteJointBuilder::new()
            .local_anchor1(Vector::new(local_anchor_a[0], local_anchor_a[1]))
            .local_anchor2(Vector::new(local_anchor_b[0], local_anchor_b[1]))
            .build();
        let handle = self.impulse_joints.insert(handle_a, handle_b, joint, true);
        Ok(joint_id_from_handle(handle))
    }

    /// Inserts a prismatic (slider) impulse joint along `axis`.
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError2d`] for unknown bodies, non-finite input,
    /// a degenerate axis, or invalid limit ordering.
    pub fn insert_prismatic_joint(
        &mut self,
        body_a: BodyId2d,
        body_b: BodyId2d,
        axis: [f32; 2],
        local_anchor_a: [f32; 2],
        local_anchor_b: [f32; 2],
        limits: Option<[f32; 2]>,
    ) -> Result<JointId2d, DynamicsBackendError2d> {
        let axis = require_nonzero2(axis)?;
        let local_anchor_a = require_finite2(local_anchor_a)?;
        let local_anchor_b = require_finite2(local_anchor_b)?;
        if let Some([min, max]) = limits {
            if !min.is_finite() || !max.is_finite() {
                return Err(DynamicsBackendError2d::NonFiniteInput);
            }
            if min > max {
                return Err(DynamicsBackendError2d::NonPositiveExtent);
            }
        }
        let handle_a =
            handle_from_body_id(body_a).ok_or(DynamicsBackendError2d::UnknownBody(body_a))?;
        let handle_b =
            handle_from_body_id(body_b).ok_or(DynamicsBackendError2d::UnknownBody(body_b))?;
        if self.bodies.get(handle_a).is_none() {
            return Err(DynamicsBackendError2d::UnknownBody(body_a));
        }
        if self.bodies.get(handle_b).is_none() {
            return Err(DynamicsBackendError2d::UnknownBody(body_b));
        }
        let mut builder = PrismaticJointBuilder::new(Vector::new(axis[0], axis[1]))
            .local_anchor1(Vector::new(local_anchor_a[0], local_anchor_a[1]))
            .local_anchor2(Vector::new(local_anchor_b[0], local_anchor_b[1]));
        if let Some(limits) = limits {
            builder = builder.limits(limits);
        }
        let handle = self
            .impulse_joints
            .insert(handle_a, handle_b, builder.build(), true);
        Ok(joint_id_from_handle(handle))
    }

    /// Inserts a rope joint that enforces a hard maximum distance between anchors.
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError2d`] for unknown bodies, non-finite input, or
    /// a non-positive `max_distance`.
    pub fn insert_rope_joint(
        &mut self,
        body_a: BodyId2d,
        body_b: BodyId2d,
        max_distance: f32,
        local_anchor_a: [f32; 2],
        local_anchor_b: [f32; 2],
    ) -> Result<JointId2d, DynamicsBackendError2d> {
        let max_distance = require_positive_2d(max_distance)?;
        let local_anchor_a = require_finite2(local_anchor_a)?;
        let local_anchor_b = require_finite2(local_anchor_b)?;
        let handle_a =
            handle_from_body_id(body_a).ok_or(DynamicsBackendError2d::UnknownBody(body_a))?;
        let handle_b =
            handle_from_body_id(body_b).ok_or(DynamicsBackendError2d::UnknownBody(body_b))?;
        if self.bodies.get(handle_a).is_none() {
            return Err(DynamicsBackendError2d::UnknownBody(body_a));
        }
        if self.bodies.get(handle_b).is_none() {
            return Err(DynamicsBackendError2d::UnknownBody(body_b));
        }
        let joint = RopeJointBuilder::new(max_distance)
            .local_anchor1(Vector::new(local_anchor_a[0], local_anchor_a[1]))
            .local_anchor2(Vector::new(local_anchor_b[0], local_anchor_b[1]))
            .build();
        let handle = self.impulse_joints.insert(handle_a, handle_b, joint, true);
        Ok(joint_id_from_handle(handle))
    }

    /// Collects active non-sensor contact pairs (deterministic sort by body id).
    #[must_use]
    pub fn collect_contact_pairs(&self) -> Vec<ContactPair2d> {
        let mut pairs = Vec::new();
        for contact_pair in self.narrow_phase.contact_pairs() {
            let Some(collider_a) = self.colliders.get(contact_pair.collider1) else {
                continue;
            };
            let Some(collider_b) = self.colliders.get(contact_pair.collider2) else {
                continue;
            };
            if collider_a.is_sensor() || collider_b.is_sensor() {
                continue;
            }
            let Some(parent_a) = collider_a.parent() else {
                continue;
            };
            let Some(parent_b) = collider_b.parent() else {
                continue;
            };
            let mut body_a = body_id_from_handle(parent_a);
            let mut body_b = body_id_from_handle(parent_b);
            let (magnitude, normal) = contact_pair.max_impulse();
            let mut normal = [normal.x, normal.y];
            if (body_a.index(), body_a.generation()) > (body_b.index(), body_b.generation()) {
                core::mem::swap(&mut body_a, &mut body_b);
                normal = [-normal[0], -normal[1]];
            }
            pairs.push(ContactPair2d {
                body_a,
                body_b,
                normal,
                impulse_magnitude: magnitude,
            });
        }
        pairs.sort_by_key(|pair| {
            (
                pair.body_a.index(),
                pair.body_a.generation(),
                pair.body_b.index(),
                pair.body_b.generation(),
            )
        });
        pairs.dedup_by_key(|pair| (pair.body_a, pair.body_b));
        pairs
    }
}

impl DynamicsBackend2d for RapierDynamicsWorld2d {
    fn step(&mut self, dt: Option<f32>) -> Result<(), DynamicsBackendError2d> {
        let dt = match dt {
            Some(value) => require_positive_2d(value)?,
            None => self.default_dt,
        };
        self.integration_parameters.dt = dt;
        self.pipeline.step(
            Vector::new(self.gravity[0], self.gravity[1]),
            &self.integration_parameters,
            &mut self.islands,
            &mut self.broad_phase,
            &mut self.narrow_phase,
            &mut self.bodies,
            &mut self.colliders,
            &mut self.impulse_joints,
            &mut self.multibody_joints,
            &mut self.ccd_solver,
            &(),
            &(),
        );
        Ok(())
    }

    fn translation(&self, body: BodyId2d) -> Option<[f32; 2]> {
        let handle = handle_from_body_id(body)?;
        let rigid = self.bodies.get(handle)?;
        let translation = rigid.translation();
        Some([translation.x, translation.y])
    }

    fn rotation(&self, body: BodyId2d) -> Option<f32> {
        let handle = handle_from_body_id(body)?;
        let rigid = self.bodies.get(handle)?;
        Some(rigid.rotation().angle())
    }
}

fn body_id_from_handle(handle: RigidBodyHandle) -> BodyId2d {
    let (index, generation) = handle.into_raw_parts();
    BodyId2d::from_raw_parts(index, generation)
}

fn handle_from_body_id(id: BodyId2d) -> Option<RigidBodyHandle> {
    Some(RigidBodyHandle::from_raw_parts(id.index(), id.generation()))
}

fn joint_id_from_handle(handle: ImpulseJointHandle) -> JointId2d {
    let (index, generation) = handle.into_raw_parts();
    JointId2d::from_raw_parts(index, generation)
}

fn to_interaction_groups(groups: CollisionGroups2d) -> InteractionGroups {
    InteractionGroups::new(
        Group::from_bits_truncate(groups.memberships),
        Group::from_bits_truncate(groups.filter),
        InteractionTestMode::And,
    )
}

fn require_nonzero2(value: [f32; 2]) -> Result<[f32; 2], DynamicsBackendError2d> {
    let value = require_finite2(value)?;
    let len_sq = value[0] * value[0] + value[1] * value[1];
    if len_sq <= f32::EPSILON * f32::EPSILON {
        return Err(DynamicsBackendError2d::DegenerateAxis);
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::RapierDynamicsWorld2d;
    use crate::backend::{
        CollisionGroups2d, DynamicsBackend2d, DynamicsFixedStepper2d, DynamicsWorldConfig2d,
    };

    #[test]
    fn dynamic_ball_rests_on_fixed_cuboid() {
        let mut world =
            RapierDynamicsWorld2d::new(DynamicsWorldConfig2d::earth_60hz()).expect("world");
        let _ground = world
            .insert_fixed_cuboid([0.0, -0.5], [5.0, 0.5])
            .expect("ground");
        let ball = world
            .insert_dynamic_ball([0.0, 3.0], 0.5)
            .expect("ball");

        for _ in 0..180 {
            world.step(None).expect("step");
        }

        let translation = world.translation(ball).expect("ball translation");
        assert!(
            translation[1] > 0.4 && translation[1] < 0.7,
            "expected ball resting near y=0.5, got {}",
            translation[1]
        );
        assert!(
            translation[0].abs() < 0.25,
            "expected ball near X origin, got {translation:?}"
        );
        let rotation = world.rotation(ball).expect("ball rotation");
        assert!(rotation.is_finite());
    }

    #[test]
    fn top_down_impulse_moves_without_gravity() {
        let mut world =
            RapierDynamicsWorld2d::new(DynamicsWorldConfig2d::top_down_60hz()).expect("world");
        let ball = world
            .insert_dynamic_ball([0.0, 0.0], 0.4)
            .expect("ball");
        world
            .set_linear_velocity(ball, [2.0, 0.0])
            .expect("kick");

        for _ in 0..30 {
            world.step(None).expect("step");
        }

        let translation = world.translation(ball).expect("ball");
        assert!(
            translation[0] > 0.5,
            "ball should drift +X without gravity, got {translation:?}"
        );
        assert!(
            translation[1].abs() < 0.15,
            "ball should stay near Y=0, got {translation:?}"
        );
    }

    #[test]
    fn trigger_detects_overlap_without_blocking() {
        let mut world =
            RapierDynamicsWorld2d::new(DynamicsWorldConfig2d::earth_60hz()).expect("world");
        let _ground = world
            .insert_fixed_cuboid([0.0, -0.25], [3.0, 0.25])
            .expect("ground");
        let trigger = world
            .insert_trigger_cuboid([0.0, 1.5], [0.8, 0.8])
            .expect("trigger");
        let probe = world
            .insert_dynamic_ball([0.0, 4.0], 0.3)
            .expect("probe");

        let mut saw_trigger = false;
        for _ in 0..240 {
            world.step(None).expect("step");
            if world
                .collect_trigger_overlaps()
                .iter()
                .any(|(t, o)| *t == trigger && *o == probe)
            {
                saw_trigger = true;
            }
        }
        assert!(saw_trigger, "probe should overlap the trigger");
        let probe_end = world.translation(probe).expect("probe");
        assert!(
            probe_end[1] < 0.8,
            "sensor must not block the probe, y={}",
            probe_end[1]
        );
    }

    #[test]
    fn collision_groups_can_disable_pair() {
        let mut world =
            RapierDynamicsWorld2d::new(DynamicsWorldConfig2d::earth_60hz()).expect("world");
        let ground = world
            .insert_fixed_cuboid([0.0, -0.25], [3.0, 0.25])
            .expect("ground");
        let ball = world
            .insert_dynamic_ball([0.0, 2.0], 0.4)
            .expect("ball");
        // Layer 1 vs layer 2 only — no mutual filter overlap with ground.
        world
            .set_collision_groups(ground, CollisionGroups2d::new(0b01, 0b01))
            .expect("ground groups");
        world
            .set_collision_groups(ball, CollisionGroups2d::new(0b10, 0b10))
            .expect("ball groups");

        for _ in 0..120 {
            world.step(None).expect("step");
        }

        let y = world.translation(ball).expect("ball")[1];
        assert!(
            y < -1.0,
            "ball should fall through filtered ground, y={y}"
        );
    }

    #[test]
    fn fixed_stepper_advances_backend() {
        let mut world =
            RapierDynamicsWorld2d::new(DynamicsWorldConfig2d::earth_60hz()).expect("world");
        let _ground = world
            .insert_fixed_cuboid([0.0, -0.5], [4.0, 0.5])
            .expect("ground");
        let ball = world
            .insert_dynamic_ball([0.0, 3.0], 0.4)
            .expect("ball");
        let mut stepper = DynamicsFixedStepper2d::hz60();
        let mut steps = 0_u32;
        for _ in 0..60 {
            steps += stepper
                .step_backend(&mut world, 1.0 / 60.0)
                .expect("stepper");
        }
        assert!(steps >= 50);
        let y = world.translation(ball).expect("ball")[1];
        assert!(y > 0.3 && y < 0.8, "ball should settle, y={y}");
    }

    #[test]
    fn kinematic_character_lands_on_ground() {
        use crate::backend::CharacterMoveConfig2d;

        let mut world =
            RapierDynamicsWorld2d::new(DynamicsWorldConfig2d::earth_60hz()).expect("world");
        let _ground = world
            .insert_fixed_cuboid([0.0, -0.5], [5.0, 0.5])
            .expect("ground");
        let character = world
            .insert_kinematic_position_capsule([0.0, 3.0], 0.4, 0.25)
            .expect("character");

        let dt = 1.0 / 60.0;
        let mut grounded = false;
        let mut y = 3.0_f32;
        for _ in 0..180 {
            let move_config = CharacterMoveConfig2d {
                vertical_filter: -1.0,
                ..CharacterMoveConfig2d::platformer()
            };
            let result = world
                .move_kinematic_character(character, [0.0, -12.0 * dt], dt, move_config)
                .expect("move");
            grounded = result.grounded;
            y = world.translation(character).expect("pos")[1];
            if grounded {
                break;
            }
        }
        assert!(grounded, "character should land, y={y}");
        assert!(y > 0.4 && y < 1.2, "expected capsule resting above ground, y={y}");
    }

    #[test]
    fn one_way_platform_allows_upward_pass() {
        use crate::backend::CharacterMoveConfig2d;

        let mut world =
            RapierDynamicsWorld2d::new(DynamicsWorldConfig2d::top_down_60hz()).expect("world");
        let platform = world
            .insert_one_way_platform_cuboid([0.0, 2.0], [1.5, 0.1])
            .expect("platform");
        assert!(world.is_one_way_platform(platform));
        let character = world
            .insert_kinematic_position_capsule([0.0, 0.5], 0.35, 0.2)
            .expect("character");

        let dt = 1.0 / 60.0;
        // Jump upward through the platform.
        for _ in 0..40 {
            let move_config = CharacterMoveConfig2d {
                vertical_filter: 1.0,
                snap_to_ground: false,
                ..CharacterMoveConfig2d::platformer()
            };
            world
                .move_kinematic_character(character, [0.0, 8.0 * dt], dt, move_config)
                .expect("ascend");
        }
        let mid_y = world.translation(character).expect("mid")[1];
        assert!(mid_y > 2.0, "should pass through one-way from below, y={mid_y}");

        // Fall back onto it.
        let mut landed = false;
        for _ in 0..90 {
            let move_config = CharacterMoveConfig2d {
                vertical_filter: -1.0,
                ..CharacterMoveConfig2d::platformer()
            };
            let result = world
                .move_kinematic_character(character, [0.0, -12.0 * dt], dt, move_config)
                .expect("descend");
            if result.grounded {
                landed = true;
                break;
            }
        }
        assert!(landed, "should land on one-way from above");
        let end_y = world.translation(character).expect("end")[1];
        assert!(end_y > 2.0, "should rest above platform, y={end_y}");
    }

    #[test]
    fn contact_pairs_report_ground_collision() {
        let mut world =
            RapierDynamicsWorld2d::new(DynamicsWorldConfig2d::earth_60hz()).expect("world");
        let ground = world
            .insert_fixed_cuboid([0.0, -0.5], [4.0, 0.5])
            .expect("ground");
        let ball = world
            .insert_dynamic_ball([0.0, 2.0], 0.4)
            .expect("ball");

        for _ in 0..180 {
            world.step(None).expect("step");
        }

        let contacts = world.collect_contact_pairs();
        assert!(
            contacts.iter().any(|pair| {
                (pair.body_a == ground && pair.body_b == ball)
                    || (pair.body_a == ball && pair.body_b == ground)
            }),
            "expected ground-ball contact, got {contacts:?}"
        );
    }
}
