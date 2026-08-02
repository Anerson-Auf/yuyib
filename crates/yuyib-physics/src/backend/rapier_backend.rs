//! Rapier 3D adapter for [`super::DynamicsBackend3d`].

use rapier3d::prelude::{
    BroadPhaseBvh, CCDSolver, ColliderBuilder, ColliderSet, FixedJointBuilder, Group,
    ImpulseJointHandle, ImpulseJointSet, IntegrationParameters, InteractionGroups,
    InteractionTestMode, IslandManager, MultibodyJointSet, NarrowPhase, PhysicsPipeline,
    PrismaticJointBuilder, RevoluteJointBuilder, RigidBodyBuilder, RigidBodyHandle, RigidBodySet,
    RopeJointBuilder, SphericalJointBuilder, TriMeshFlags, Vector,
};

use super::{
    BodyId3d, CollisionGroups3d, ContactPair3d, DynamicsBackend3d, DynamicsBackendError3d,
    DynamicsWorldConfig3d, JointId3d, require_finite3, require_nonzero3, require_positive,
};

/// Rapier-backed dynamics world used by the M4 facade path.
pub struct RapierDynamicsWorld3d {
    gravity: [f32; 3],
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
}

impl RapierDynamicsWorld3d {
    /// Creates an empty Rapier world with the supplied gravity/default dt.
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError3d`] when gravity or `default_dt` is invalid.
    pub fn new(config: DynamicsWorldConfig3d) -> Result<Self, DynamicsBackendError3d> {
        let gravity = require_finite3(config.gravity)?;
        let default_dt = require_positive(config.default_dt)?;
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
        })
    }

    /// Inserts a fixed axis-aligned cuboid collider.
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError3d`] for non-finite centres or non-positive
    /// half extents.
    pub fn insert_fixed_cuboid(
        &mut self,
        center: [f32; 3],
        half_extents: [f32; 3],
    ) -> Result<BodyId3d, DynamicsBackendError3d> {
        let center = require_finite3(center)?;
        let hx = require_positive(half_extents[0])?;
        let hy = require_positive(half_extents[1])?;
        let hz = require_positive(half_extents[2])?;
        let body = self.bodies.insert(
            RigidBodyBuilder::fixed().translation(Vector::new(center[0], center[1], center[2])),
        );
        self.colliders.insert_with_parent(
            ColliderBuilder::cuboid(hx, hy, hz),
            body,
            &mut self.bodies,
        );
        Ok(body_id_from_handle(body))
    }

    /// Inserts a dynamic sphere with zero initial velocity.
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError3d`] for non-finite centres or non-positive
    /// radii.
    pub fn insert_dynamic_sphere(
        &mut self,
        center: [f32; 3],
        radius: f32,
    ) -> Result<BodyId3d, DynamicsBackendError3d> {
        let center = require_finite3(center)?;
        let radius = require_positive(radius)?;
        let body = self.bodies.insert(
            RigidBodyBuilder::dynamic().translation(Vector::new(center[0], center[1], center[2])),
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
    /// Returns [`DynamicsBackendError3d`] for non-finite centres or non-positive
    /// half extents.
    pub fn insert_dynamic_cuboid(
        &mut self,
        center: [f32; 3],
        half_extents: [f32; 3],
    ) -> Result<BodyId3d, DynamicsBackendError3d> {
        let center = require_finite3(center)?;
        let hx = require_positive(half_extents[0])?;
        let hy = require_positive(half_extents[1])?;
        let hz = require_positive(half_extents[2])?;
        let body = self.bodies.insert(
            RigidBodyBuilder::dynamic().translation(Vector::new(center[0], center[1], center[2])),
        );
        self.colliders.insert_with_parent(
            ColliderBuilder::cuboid(hx, hy, hz),
            body,
            &mut self.bodies,
        );
        Ok(body_id_from_handle(body))
    }

    /// Inserts a dynamic Y-axis capsule (`half_height` is the cylindrical half-length).
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError3d`] for non-finite centres or non-positive sizes.
    pub fn insert_dynamic_capsule(
        &mut self,
        center: [f32; 3],
        half_height: f32,
        radius: f32,
    ) -> Result<BodyId3d, DynamicsBackendError3d> {
        let center = require_finite3(center)?;
        let half_height = require_positive(half_height)?;
        let radius = require_positive(radius)?;
        let body = self.bodies.insert(
            RigidBodyBuilder::dynamic().translation(Vector::new(center[0], center[1], center[2])),
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
    /// Returns [`DynamicsBackendError3d`] for unknown bodies or non-finite velocity.
    pub fn set_linear_velocity(
        &mut self,
        body: BodyId3d,
        velocity: [f32; 3],
    ) -> Result<(), DynamicsBackendError3d> {
        let velocity = require_finite3(velocity)?;
        let handle =
            handle_from_body_id(body).ok_or(DynamicsBackendError3d::UnknownBody(body))?;
        let rigid = self
            .bodies
            .get_mut(handle)
            .ok_or(DynamicsBackendError3d::UnknownBody(body))?;
        rigid.set_linvel(Vector::new(velocity[0], velocity[1], velocity[2]), true);
        Ok(())
    }

    /// Inserts a position-based kinematic sphere (character proxies, moving sensors).
    ///
    /// Drive it with [`Self::set_next_kinematic_translation`] before each step.
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError3d`] for invalid centres or radii.
    pub fn insert_kinematic_position_sphere(
        &mut self,
        center: [f32; 3],
        radius: f32,
    ) -> Result<BodyId3d, DynamicsBackendError3d> {
        let center = require_finite3(center)?;
        let radius = require_positive(radius)?;
        let body = self.bodies.insert(
            RigidBodyBuilder::kinematic_position_based()
                .translation(Vector::new(center[0], center[1], center[2])),
        );
        self.colliders.insert_with_parent(
            ColliderBuilder::ball(radius).friction(0.8),
            body,
            &mut self.bodies,
        );
        Ok(body_id_from_handle(body))
    }

    /// Inserts a velocity-based kinematic cuboid (moving platforms).
    ///
    /// Drive it with [`Self::set_linear_velocity`]. Contact friction transfers
    /// motion to dynamic riders; tune speed/friction for conveyor-like carry.
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError3d`] for invalid centres or extents.
    pub fn insert_kinematic_cuboid(
        &mut self,
        center: [f32; 3],
        half_extents: [f32; 3],
    ) -> Result<BodyId3d, DynamicsBackendError3d> {
        self.insert_kinematic_cuboid_with_mode(center, half_extents, false)
    }

    /// Inserts a position-based kinematic cuboid for scripted pose updates.
    ///
    /// Drive it with [`Self::set_next_kinematic_translation`] before each step.
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError3d`] for invalid centres or extents.
    pub fn insert_kinematic_position_cuboid(
        &mut self,
        center: [f32; 3],
        half_extents: [f32; 3],
    ) -> Result<BodyId3d, DynamicsBackendError3d> {
        self.insert_kinematic_cuboid_with_mode(center, half_extents, true)
    }

    fn insert_kinematic_cuboid_with_mode(
        &mut self,
        center: [f32; 3],
        half_extents: [f32; 3],
        position_based: bool,
    ) -> Result<BodyId3d, DynamicsBackendError3d> {
        let center = require_finite3(center)?;
        let hx = require_positive(half_extents[0])?;
        let hy = require_positive(half_extents[1])?;
        let hz = require_positive(half_extents[2])?;
        let builder = if position_based {
            RigidBodyBuilder::kinematic_position_based()
        } else {
            RigidBodyBuilder::kinematic_velocity_based()
        };
        let body = self
            .bodies
            .insert(builder.translation(Vector::new(center[0], center[1], center[2])));
        self.colliders.insert_with_parent(
            ColliderBuilder::cuboid(hx, hy, hz).friction(1.2),
            body,
            &mut self.bodies,
        );
        Ok(body_id_from_handle(body))
    }

    /// Sets the next translation for a **position-based** kinematic body.
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError3d`] for unknown bodies or non-finite input.
    pub fn set_next_kinematic_translation(
        &mut self,
        body: BodyId3d,
        translation: [f32; 3],
    ) -> Result<(), DynamicsBackendError3d> {
        let translation = require_finite3(translation)?;
        let handle =
            handle_from_body_id(body).ok_or(DynamicsBackendError3d::UnknownBody(body))?;
        let rigid = self
            .bodies
            .get_mut(handle)
            .ok_or(DynamicsBackendError3d::UnknownBody(body))?;
        rigid.set_next_kinematic_translation(Vector::new(
            translation[0],
            translation[1],
            translation[2],
        ));
        Ok(())
    }

    /// Inserts a fixed sensor cuboid (trigger volume — no collision response).
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError3d`] for invalid centres or extents.
    pub fn insert_trigger_cuboid(
        &mut self,
        center: [f32; 3],
        half_extents: [f32; 3],
    ) -> Result<BodyId3d, DynamicsBackendError3d> {
        let center = require_finite3(center)?;
        let hx = require_positive(half_extents[0])?;
        let hy = require_positive(half_extents[1])?;
        let hz = require_positive(half_extents[2])?;
        let body = self.bodies.insert(
            RigidBodyBuilder::fixed().translation(Vector::new(center[0], center[1], center[2])),
        );
        self.colliders.insert_with_parent(
            ColliderBuilder::cuboid(hx, hy, hz).sensor(true),
            body,
            &mut self.bodies,
        );
        Ok(body_id_from_handle(body))
    }

    /// Inserts a fixed sensor sphere (trigger volume).
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError3d`] for invalid centres or radii.
    pub fn insert_trigger_sphere(
        &mut self,
        center: [f32; 3],
        radius: f32,
    ) -> Result<BodyId3d, DynamicsBackendError3d> {
        let center = require_finite3(center)?;
        let radius = require_positive(radius)?;
        let body = self.bodies.insert(
            RigidBodyBuilder::fixed().translation(Vector::new(center[0], center[1], center[2])),
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
    /// Returns [`DynamicsBackendError3d`] for unknown bodies.
    pub fn set_ccd_enabled(
        &mut self,
        body: BodyId3d,
        enabled: bool,
    ) -> Result<(), DynamicsBackendError3d> {
        let handle =
            handle_from_body_id(body).ok_or(DynamicsBackendError3d::UnknownBody(body))?;
        let rigid = self
            .bodies
            .get_mut(handle)
            .ok_or(DynamicsBackendError3d::UnknownBody(body))?;
        rigid.enable_ccd(enabled);
        Ok(())
    }

    /// Returns current sensor intersection pairs as `(trigger_body, other_body)`.
    ///
    /// Sorted by body id for deterministic tests. Pairs where both colliders are
    /// sensors are ordered by ascending `(index, generation)`.
    #[must_use]
    pub fn collect_trigger_overlaps(&self) -> Vec<(BodyId3d, BodyId3d)> {
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

    /// Sets Rapier collision membership/filter on every collider attached to `body`.
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError3d`] for unknown bodies.
    pub fn set_collision_groups(
        &mut self,
        body: BodyId3d,
        groups: CollisionGroups3d,
    ) -> Result<(), DynamicsBackendError3d> {
        let handle =
            handle_from_body_id(body).ok_or(DynamicsBackendError3d::UnknownBody(body))?;
        let rigid = self
            .bodies
            .get(handle)
            .ok_or(DynamicsBackendError3d::UnknownBody(body))?;
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
    /// Anchors are expressed in each body's local space.
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError3d`] for unknown bodies or non-finite anchors.
    pub fn insert_fixed_joint(
        &mut self,
        body_a: BodyId3d,
        body_b: BodyId3d,
        local_anchor_a: [f32; 3],
        local_anchor_b: [f32; 3],
    ) -> Result<JointId3d, DynamicsBackendError3d> {
        let local_anchor_a = require_finite3(local_anchor_a)?;
        let local_anchor_b = require_finite3(local_anchor_b)?;
        let handle_a =
            handle_from_body_id(body_a).ok_or(DynamicsBackendError3d::UnknownBody(body_a))?;
        let handle_b =
            handle_from_body_id(body_b).ok_or(DynamicsBackendError3d::UnknownBody(body_b))?;
        if self.bodies.get(handle_a).is_none() {
            return Err(DynamicsBackendError3d::UnknownBody(body_a));
        }
        if self.bodies.get(handle_b).is_none() {
            return Err(DynamicsBackendError3d::UnknownBody(body_b));
        }
        let joint = FixedJointBuilder::new()
            .local_anchor1(Vector::new(
                local_anchor_a[0],
                local_anchor_a[1],
                local_anchor_a[2],
            ))
            .local_anchor2(Vector::new(
                local_anchor_b[0],
                local_anchor_b[1],
                local_anchor_b[2],
            ))
            .build();
        let handle = self.impulse_joints.insert(handle_a, handle_b, joint, true);
        Ok(joint_id_from_handle(handle))
    }

    /// Inserts a revolute impulse joint (free rotation about `axis` in both local frames).
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError3d`] for unknown bodies, non-finite input, or a
    /// degenerate axis.
    pub fn insert_revolute_joint(
        &mut self,
        body_a: BodyId3d,
        body_b: BodyId3d,
        axis: [f32; 3],
        local_anchor_a: [f32; 3],
        local_anchor_b: [f32; 3],
    ) -> Result<JointId3d, DynamicsBackendError3d> {
        let axis = require_nonzero3(axis)?;
        let local_anchor_a = require_finite3(local_anchor_a)?;
        let local_anchor_b = require_finite3(local_anchor_b)?;
        let handle_a =
            handle_from_body_id(body_a).ok_or(DynamicsBackendError3d::UnknownBody(body_a))?;
        let handle_b =
            handle_from_body_id(body_b).ok_or(DynamicsBackendError3d::UnknownBody(body_b))?;
        if self.bodies.get(handle_a).is_none() {
            return Err(DynamicsBackendError3d::UnknownBody(body_a));
        }
        if self.bodies.get(handle_b).is_none() {
            return Err(DynamicsBackendError3d::UnknownBody(body_b));
        }
        let joint = RevoluteJointBuilder::new(Vector::new(axis[0], axis[1], axis[2]))
            .local_anchor1(Vector::new(
                local_anchor_a[0],
                local_anchor_a[1],
                local_anchor_a[2],
            ))
            .local_anchor2(Vector::new(
                local_anchor_b[0],
                local_anchor_b[1],
                local_anchor_b[2],
            ))
            .build();
        let handle = self.impulse_joints.insert(handle_a, handle_b, joint, true);
        Ok(joint_id_from_handle(handle))
    }

    /// Inserts a prismatic (slider) impulse joint along `axis`.
    ///
    /// When `limits` is `Some([min, max])`, translation along the axis is clamped.
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError3d`] for unknown bodies, non-finite input,
    /// a degenerate axis, or invalid limit ordering.
    pub fn insert_prismatic_joint(
        &mut self,
        body_a: BodyId3d,
        body_b: BodyId3d,
        axis: [f32; 3],
        local_anchor_a: [f32; 3],
        local_anchor_b: [f32; 3],
        limits: Option<[f32; 2]>,
    ) -> Result<JointId3d, DynamicsBackendError3d> {
        let axis = require_nonzero3(axis)?;
        let local_anchor_a = require_finite3(local_anchor_a)?;
        let local_anchor_b = require_finite3(local_anchor_b)?;
        if let Some([min, max]) = limits {
            if !min.is_finite() || !max.is_finite() {
                return Err(DynamicsBackendError3d::NonFiniteInput);
            }
            if min > max {
                return Err(DynamicsBackendError3d::NonPositiveExtent);
            }
        }
        let handle_a =
            handle_from_body_id(body_a).ok_or(DynamicsBackendError3d::UnknownBody(body_a))?;
        let handle_b =
            handle_from_body_id(body_b).ok_or(DynamicsBackendError3d::UnknownBody(body_b))?;
        if self.bodies.get(handle_a).is_none() {
            return Err(DynamicsBackendError3d::UnknownBody(body_a));
        }
        if self.bodies.get(handle_b).is_none() {
            return Err(DynamicsBackendError3d::UnknownBody(body_b));
        }
        let mut builder = PrismaticJointBuilder::new(Vector::new(axis[0], axis[1], axis[2]))
            .local_anchor1(Vector::new(
                local_anchor_a[0],
                local_anchor_a[1],
                local_anchor_a[2],
            ))
            .local_anchor2(Vector::new(
                local_anchor_b[0],
                local_anchor_b[1],
                local_anchor_b[2],
            ));
        if let Some(limits) = limits {
            builder = builder.limits(limits);
        }
        let handle = self
            .impulse_joints
            .insert(handle_a, handle_b, builder.build(), true);
        Ok(joint_id_from_handle(handle))
    }

    /// Inserts a dynamic convex hull collider from local-space points.
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError3d`] for non-finite centres/points or when
    /// Rapier cannot build a hull.
    pub fn insert_dynamic_convex_hull(
        &mut self,
        center: [f32; 3],
        points: &[[f32; 3]],
    ) -> Result<BodyId3d, DynamicsBackendError3d> {
        let center = require_finite3(center)?;
        let collider = convex_collider_from_points(points)?;
        let body = self.bodies.insert(
            RigidBodyBuilder::dynamic().translation(Vector::new(center[0], center[1], center[2])),
        );
        self.colliders
            .insert_with_parent(collider, body, &mut self.bodies);
        Ok(body_id_from_handle(body))
    }

    /// Inserts a fixed triangle-mesh collider from interleaved vertices + tri indices.
    ///
    /// Used to mirror a slice of [`crate::TriangleMesh3d`] into Rapier so dynamic
    /// props collide with map walls/floors. Character motion stays on the mesh
    /// query path.
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError3d`] for empty/invalid data or when Rapier
    /// rejects the mesh.
    pub fn insert_fixed_trimesh(
        &mut self,
        vertices: &[[f32; 3]],
        indices: &[[u32; 3]],
    ) -> Result<BodyId3d, DynamicsBackendError3d> {
        if vertices.is_empty() || indices.is_empty() {
            return Err(DynamicsBackendError3d::TrimeshFailed);
        }
        let mut points = Vec::with_capacity(vertices.len());
        for vertex in vertices {
            let vertex = require_finite3(*vertex)?;
            points.push(Vector::new(vertex[0], vertex[1], vertex[2]));
        }
        let mut tris = Vec::with_capacity(indices.len());
        for tri in indices {
            let max_index = tri[0].max(tri[1]).max(tri[2]) as usize;
            if max_index >= points.len() {
                return Err(DynamicsBackendError3d::TrimeshFailed);
            }
            tris.push(*tri);
        }
        let collider = ColliderBuilder::trimesh_with_flags(
            points,
            tris,
            TriMeshFlags::DELETE_DEGENERATE_TRIANGLES | TriMeshFlags::MERGE_DUPLICATE_VERTICES,
        )
        .map_err(|_| DynamicsBackendError3d::TrimeshFailed)?;
        let body = self.bodies.insert(RigidBodyBuilder::fixed());
        self.colliders
            .insert_with_parent(collider, body, &mut self.bodies);
        Ok(body_id_from_handle(body))
    }

    /// Inserts a fixed convex hull collider from local-space points.
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError3d`] for non-finite centres/points or when
    /// Rapier cannot build a hull.
    pub fn insert_fixed_convex_hull(
        &mut self,
        center: [f32; 3],
        points: &[[f32; 3]],
    ) -> Result<BodyId3d, DynamicsBackendError3d> {
        let center = require_finite3(center)?;
        let collider = convex_collider_from_points(points)?;
        let body = self.bodies.insert(
            RigidBodyBuilder::fixed().translation(Vector::new(center[0], center[1], center[2])),
        );
        self.colliders
            .insert_with_parent(collider, body, &mut self.bodies);
        Ok(body_id_from_handle(body))
    }

    /// Returns active non-sensor contact pairs, sorted by body id.
    #[must_use]
    pub fn collect_contact_pairs(&self) -> Vec<ContactPair3d> {
        let mut pairs = Vec::new();
        for pair in self.narrow_phase.contact_pairs() {
            if !pair.has_any_active_contact() {
                continue;
            }
            let Some(collider_a) = self.colliders.get(pair.collider1) else {
                continue;
            };
            let Some(collider_b) = self.colliders.get(pair.collider2) else {
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
            let (magnitude, normal) = pair.max_impulse();
            let mut normal = [normal.x, normal.y, normal.z];
            if (body_a.index(), body_a.generation()) > (body_b.index(), body_b.generation()) {
                core::mem::swap(&mut body_a, &mut body_b);
                normal = [-normal[0], -normal[1], -normal[2]];
            }
            pairs.push(ContactPair3d {
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

    /// Inserts a spherical (ball) joint — free rotation, locked anchors.
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError3d`] for unknown bodies or non-finite anchors.
    pub fn insert_spherical_joint(
        &mut self,
        body_a: BodyId3d,
        body_b: BodyId3d,
        local_anchor_a: [f32; 3],
        local_anchor_b: [f32; 3],
    ) -> Result<JointId3d, DynamicsBackendError3d> {
        let local_anchor_a = require_finite3(local_anchor_a)?;
        let local_anchor_b = require_finite3(local_anchor_b)?;
        let handle_a =
            handle_from_body_id(body_a).ok_or(DynamicsBackendError3d::UnknownBody(body_a))?;
        let handle_b =
            handle_from_body_id(body_b).ok_or(DynamicsBackendError3d::UnknownBody(body_b))?;
        if self.bodies.get(handle_a).is_none() {
            return Err(DynamicsBackendError3d::UnknownBody(body_a));
        }
        if self.bodies.get(handle_b).is_none() {
            return Err(DynamicsBackendError3d::UnknownBody(body_b));
        }
        let joint = SphericalJointBuilder::new()
            .local_anchor1(Vector::new(
                local_anchor_a[0],
                local_anchor_a[1],
                local_anchor_a[2],
            ))
            .local_anchor2(Vector::new(
                local_anchor_b[0],
                local_anchor_b[1],
                local_anchor_b[2],
            ))
            .build();
        let handle = self.impulse_joints.insert(handle_a, handle_b, joint, true);
        Ok(joint_id_from_handle(handle))
    }

    /// Inserts a rope joint that enforces a hard maximum distance between anchors.
    ///
    /// # Errors
    ///
    /// Returns [`DynamicsBackendError3d`] for unknown bodies, non-finite input, or
    /// a non-positive `max_distance`.
    pub fn insert_rope_joint(
        &mut self,
        body_a: BodyId3d,
        body_b: BodyId3d,
        max_distance: f32,
        local_anchor_a: [f32; 3],
        local_anchor_b: [f32; 3],
    ) -> Result<JointId3d, DynamicsBackendError3d> {
        let max_distance = require_positive(max_distance)?;
        let local_anchor_a = require_finite3(local_anchor_a)?;
        let local_anchor_b = require_finite3(local_anchor_b)?;
        let handle_a =
            handle_from_body_id(body_a).ok_or(DynamicsBackendError3d::UnknownBody(body_a))?;
        let handle_b =
            handle_from_body_id(body_b).ok_or(DynamicsBackendError3d::UnknownBody(body_b))?;
        if self.bodies.get(handle_a).is_none() {
            return Err(DynamicsBackendError3d::UnknownBody(body_a));
        }
        if self.bodies.get(handle_b).is_none() {
            return Err(DynamicsBackendError3d::UnknownBody(body_b));
        }
        let joint = RopeJointBuilder::new(max_distance)
            .local_anchor1(Vector::new(
                local_anchor_a[0],
                local_anchor_a[1],
                local_anchor_a[2],
            ))
            .local_anchor2(Vector::new(
                local_anchor_b[0],
                local_anchor_b[1],
                local_anchor_b[2],
            ))
            .build();
        let handle = self.impulse_joints.insert(handle_a, handle_b, joint, true);
        Ok(joint_id_from_handle(handle))
    }
}

impl DynamicsBackend3d for RapierDynamicsWorld3d {
    fn step(&mut self, dt: Option<f32>) -> Result<(), DynamicsBackendError3d> {
        let dt = match dt {
            Some(value) => require_positive(value)?,
            None => self.default_dt,
        };
        self.integration_parameters.dt = dt;
        self.pipeline.step(
            Vector::new(self.gravity[0], self.gravity[1], self.gravity[2]),
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

    fn translation(&self, body: BodyId3d) -> Option<[f32; 3]> {
        let handle = handle_from_body_id(body)?;
        let rigid = self.bodies.get(handle)?;
        let translation = rigid.translation();
        Some([translation.x, translation.y, translation.z])
    }

    fn rotation_xyzw(&self, body: BodyId3d) -> Option<[f32; 4]> {
        let handle = handle_from_body_id(body)?;
        let rigid = self.bodies.get(handle)?;
        let rotation = rigid.rotation();
        Some([rotation.x, rotation.y, rotation.z, rotation.w])
    }
}

fn body_id_from_handle(handle: RigidBodyHandle) -> BodyId3d {
    let (index, generation) = handle.into_raw_parts();
    BodyId3d::from_raw_parts(index, generation)
}

fn handle_from_body_id(id: BodyId3d) -> Option<RigidBodyHandle> {
    Some(RigidBodyHandle::from_raw_parts(id.index(), id.generation()))
}

fn joint_id_from_handle(handle: ImpulseJointHandle) -> JointId3d {
    let (index, generation) = handle.into_raw_parts();
    JointId3d::from_raw_parts(index, generation)
}

fn to_interaction_groups(groups: CollisionGroups3d) -> InteractionGroups {
    InteractionGroups::new(
        Group::from_bits_truncate(groups.memberships),
        Group::from_bits_truncate(groups.filter),
        InteractionTestMode::And,
    )
}

fn convex_collider_from_points(
    points: &[[f32; 3]],
) -> Result<ColliderBuilder, DynamicsBackendError3d> {
    if points.len() < 4 {
        return Err(DynamicsBackendError3d::ConvexHullFailed);
    }
    let mut vectors = Vec::with_capacity(points.len());
    for point in points {
        let point = require_finite3(*point)?;
        vectors.push(Vector::new(point[0], point[1], point[2]));
    }
    ColliderBuilder::convex_hull(&vectors).ok_or(DynamicsBackendError3d::ConvexHullFailed)
}

#[cfg(test)]
mod tests {
    use super::RapierDynamicsWorld3d;
    use crate::backend::{DynamicsBackend3d, DynamicsWorldConfig3d};

    #[test]
    fn dynamic_sphere_rests_on_fixed_cuboid() {
        let mut world =
            RapierDynamicsWorld3d::new(DynamicsWorldConfig3d::earth_60hz()).expect("world");
        let _ground = world
            .insert_fixed_cuboid([0.0, -0.5, 0.0], [5.0, 0.5, 5.0])
            .expect("ground");
        let ball = world
            .insert_dynamic_sphere([0.0, 3.0, 0.0], 0.5)
            .expect("ball");

        for _ in 0..180 {
            world.step(None).expect("step");
        }

        let translation = world.translation(ball).expect("ball translation");
        assert!(
            translation[1] > 0.4 && translation[1] < 0.7,
            "expected sphere resting near y=0.5, got {}",
            translation[1]
        );
        assert!(
            translation[0].abs() < 0.25 && translation[2].abs() < 0.25,
            "expected sphere near XZ origin, got {translation:?}"
        );
        let rotation = world.rotation_xyzw(ball).expect("ball rotation");
        assert!(rotation.iter().all(|channel| channel.is_finite()));
    }

    #[test]
    fn dynamic_cuboids_stack_without_falling_through() {
        let mut world =
            RapierDynamicsWorld3d::new(DynamicsWorldConfig3d::earth_60hz()).expect("world");
        let _ground = world
            .insert_fixed_cuboid([0.0, -0.25, 0.0], [3.0, 0.25, 3.0])
            .expect("ground");
        let lower = world
            .insert_dynamic_cuboid([0.0, 1.0, 0.0], [0.4, 0.4, 0.4])
            .expect("lower");
        let upper = world
            .insert_dynamic_cuboid([0.05, 2.2, -0.05], [0.4, 0.4, 0.4])
            .expect("upper");

        for _ in 0..240 {
            world.step(None).expect("step");
        }

        let lower_y = world.translation(lower).expect("lower")[1];
        let upper_y = world.translation(upper).expect("upper")[1];
        assert!(
            lower_y > 0.3 && lower_y < 0.7,
            "lower box should rest on ground, y={lower_y}"
        );
        assert!(
            upper_y > lower_y + 0.5,
            "upper box should rest above lower, lower={lower_y} upper={upper_y}"
        );
    }

    #[test]
    fn capsule_falls_onto_ground() {
        let mut world =
            RapierDynamicsWorld3d::new(DynamicsWorldConfig3d::earth_60hz()).expect("world");
        let _ground = world
            .insert_fixed_cuboid([0.0, -0.25, 0.0], [4.0, 0.25, 4.0])
            .expect("ground");
        let capsule = world
            .insert_dynamic_capsule([0.0, 3.0, 0.0], 0.4, 0.25)
            .expect("capsule");

        for _ in 0..240 {
            world.step(None).expect("step");
        }

        let translation = world.translation(capsule).expect("capsule");
        // Y-capsule resting upright: center ≈ half_height + radius = 0.65 above ground top (y=0).
        assert!(
            translation[1] > 0.55 && translation[1] < 0.85,
            "capsule should settle upright above ground, y={}",
            translation[1]
        );
    }

    #[test]
    fn set_linear_velocity_moves_dynamic_body() {
        let mut world =
            RapierDynamicsWorld3d::new(DynamicsWorldConfig3d::earth_60hz()).expect("world");
        let _ground = world
            .insert_fixed_cuboid([0.0, -0.25, 0.0], [8.0, 0.25, 8.0])
            .expect("ground");
        let ball = world
            .insert_dynamic_sphere([0.0, 1.0, 0.0], 0.4)
            .expect("ball");
        world
            .set_linear_velocity(ball, [3.0, 0.0, 0.0])
            .expect("kick");

        for _ in 0..30 {
            world.step(None).expect("step");
        }

        let translation = world.translation(ball).expect("ball");
        assert!(
            translation[0] > 0.5,
            "ball should drift +X after kick, got {translation:?}"
        );
        assert!(
            translation[1] > 0.2,
            "ball must stay above ground, y={}",
            translation[1]
        );
    }

    #[test]
    fn kinematic_platform_carries_dynamic_sphere() {
        let mut world =
            RapierDynamicsWorld3d::new(DynamicsWorldConfig3d::earth_60hz()).expect("world");
        let platform = world
            .insert_kinematic_cuboid([0.0, 0.5, 0.0], [2.0, 0.15, 2.0])
            .expect("platform");
        let ball = world
            .insert_dynamic_sphere([0.0, 1.5, 0.0], 0.35)
            .expect("ball");

        for _ in 0..120 {
            world.step(None).expect("settle");
        }
        let start = world.translation(ball).expect("ball start");
        assert!(
            start[1] > 0.5 && start[1] < 1.2,
            "ball should rest on platform, y={}",
            start[1]
        );

        world
            .set_linear_velocity(platform, [0.8, 0.0, 0.0])
            .expect("drive platform");
        for _ in 0..90 {
            world.step(None).expect("step");
        }

        let end = world.translation(ball).expect("ball end");
        let platform_end = world.translation(platform).expect("platform end");
        assert!(
            platform_end[0] > 1.0,
            "platform should have moved +X, got {}",
            platform_end[0]
        );
        // Contact coupling is lossy without per-material tuning; require a clear
        // +X delta while the rider stays seated.
        assert!(
            end[0] > start[0] + 0.25,
            "ball should gain +X from platform contact: start={start:?} end={end:?}"
        );
        assert!(
            end[1] > 0.7,
            "ball must stay on platform, y={}",
            end[1]
        );
    }

    #[test]
    fn position_kinematic_follows_set_next_translation() {
        let mut world =
            RapierDynamicsWorld3d::new(DynamicsWorldConfig3d::earth_60hz()).expect("world");
        let platform = world
            .insert_kinematic_position_cuboid([0.0, 1.0, 0.0], [1.0, 0.2, 1.0])
            .expect("platform");

        for i in 1..=30 {
            let x = i as f32 * 0.1;
            world
                .set_next_kinematic_translation(platform, [x, 1.0, 0.0])
                .expect("script");
            world.step(None).expect("step");
        }

        let end = world.translation(platform).expect("platform");
        assert!(
            (end[0] - 3.0).abs() < 0.05 && (end[1] - 1.0).abs() < 0.05,
            "position kinematic should track scripted pose, got {end:?}"
        );
    }

    #[test]
    fn trigger_reports_overlap_without_blocking() {
        let mut world =
            RapierDynamicsWorld3d::new(DynamicsWorldConfig3d::earth_60hz()).expect("world");
        let _ground = world
            .insert_fixed_cuboid([0.0, -0.25, 0.0], [3.0, 0.25, 3.0])
            .expect("ground");
        let trigger = world
            .insert_trigger_cuboid([0.0, 1.5, 0.0], [0.8, 0.8, 0.8])
            .expect("trigger");
        let ball = world
            .insert_dynamic_sphere([0.0, 4.0, 0.0], 0.3)
            .expect("ball");

        let mut saw_overlap = false;
        for _ in 0..240 {
            world.step(None).expect("step");
            let overlaps = world.collect_trigger_overlaps();
            if overlaps.iter().any(|(t, o)| *t == trigger && *o == ball) {
                saw_overlap = true;
            }
        }
        assert!(saw_overlap, "ball should intersect the trigger while falling");

        let end = world.translation(ball).expect("ball");
        assert!(
            end[1] < 0.8,
            "sensor must not support the ball; expected rest on ground, y={}",
            end[1]
        );
        assert!(
            world.collect_trigger_overlaps().is_empty(),
            "ball should leave the elevated trigger after landing"
        );
    }

    #[test]
    fn ccd_keeps_fast_sphere_from_tunnelling_thin_floor() {
        let mut world =
            RapierDynamicsWorld3d::new(DynamicsWorldConfig3d::earth_60hz()).expect("world");
        let _ground = world
            .insert_fixed_cuboid([0.0, 0.0, 0.0], [4.0, 0.05, 4.0])
            .expect("thin floor");
        let ball = world
            .insert_dynamic_sphere([0.0, 2.0, 0.0], 0.2)
            .expect("ball");
        world.set_ccd_enabled(ball, true).expect("ccd");
        world
            .set_linear_velocity(ball, [0.0, -40.0, 0.0])
            .expect("slam");

        for _ in 0..120 {
            world.step(None).expect("step");
        }

        let end = world.translation(ball).expect("ball");
        assert!(
            end[1] > 0.1,
            "CCD ball should not tunnel through thin floor, y={}",
            end[1]
        );
    }

    #[test]
    fn fixed_joint_holds_dynamic_body_in_air() {
        let mut world =
            RapierDynamicsWorld3d::new(DynamicsWorldConfig3d::earth_60hz()).expect("world");
        let anchor = world
            .insert_fixed_cuboid([0.0, 3.0, 0.0], [0.2, 0.2, 0.2])
            .expect("anchor");
        // Joint world point at y=2.5: anchor local (0,-0.5,0), bob local (0,0.25,0).
        let hanging = world
            .insert_dynamic_cuboid([0.0, 2.25, 0.0], [0.25, 0.25, 0.25])
            .expect("hanging");
        let _joint = world
            .insert_fixed_joint(anchor, hanging, [0.0, -0.5, 0.0], [0.0, 0.25, 0.0])
            .expect("fixed joint");

        for _ in 0..180 {
            world.step(None).expect("step");
        }

        let end = world.translation(hanging).expect("hanging");
        assert!(
            end[1] > 2.0 && end[1] < 2.6,
            "fixed joint should keep cuboid near y=2.25, got {end:?}"
        );
        assert!(
            end[0].abs() < 0.35 && end[2].abs() < 0.35,
            "fixed joint should keep cuboid near XZ origin, got {end:?}"
        );
    }

    #[test]
    fn revolute_joint_forms_a_pendulum() {
        let mut world =
            RapierDynamicsWorld3d::new(DynamicsWorldConfig3d::earth_60hz()).expect("world");
        let pivot = world
            .insert_fixed_cuboid([0.0, 3.0, 0.0], [0.15, 0.15, 0.15])
            .expect("pivot");
        let bob = world
            .insert_dynamic_sphere([1.5, 3.0, 0.0], 0.25)
            .expect("bob");
        let _joint = world
            .insert_revolute_joint(
                pivot,
                bob,
                [0.0, 0.0, 1.0],
                [0.0, 0.0, 0.0],
                [-1.5, 0.0, 0.0],
            )
            .expect("revolute");

        for _ in 0..240 {
            world.step(None).expect("step");
        }

        let end = world.translation(bob).expect("bob");
        // Pendulum should swing down toward lower hemisphere.
        assert!(
            end[1] < 2.6,
            "revolute bob should drop under gravity, y={}",
            end[1]
        );
        let dx = end[0];
        let dy = end[1] - 3.0;
        let radius = (dx * dx + dy * dy).sqrt();
        assert!(
            (radius - 1.5).abs() < 0.35,
            "bob should stay near 1.5m arm length, radius={radius}, end={end:?}"
        );
    }

    #[test]
    fn collision_groups_let_ghost_fall_through_ground() {
        use crate::backend::CollisionGroups3d;

        let mut world =
            RapierDynamicsWorld3d::new(DynamicsWorldConfig3d::earth_60hz()).expect("world");
        let ground = world
            .insert_fixed_cuboid([0.0, -0.25, 0.0], [4.0, 0.25, 4.0])
            .expect("ground");
        // World layer = bit0, solids = bit1, ghost = bit2.
        world
            .set_collision_groups(ground, CollisionGroups3d::new(1, 1 | 2))
            .expect("ground groups");

        let solid = world
            .insert_dynamic_sphere([-1.0, 2.0, 0.0], 0.3)
            .expect("solid");
        world
            .set_collision_groups(solid, CollisionGroups3d::new(2, 1))
            .expect("solid groups");

        let ghost = world
            .insert_dynamic_sphere([1.0, 2.0, 0.0], 0.3)
            .expect("ghost");
        world
            .set_collision_groups(ghost, CollisionGroups3d::new(4, 4))
            .expect("ghost groups");

        for _ in 0..180 {
            world.step(None).expect("step");
        }

        let solid_y = world.translation(solid).expect("solid")[1];
        let ghost_y = world.translation(ghost).expect("ghost")[1];
        assert!(
            solid_y > 0.2 && solid_y < 0.7,
            "filtered solid should rest on ground, y={solid_y}"
        );
        assert!(
            ghost_y < -1.0,
            "ghost with no world filter should fall through, y={ghost_y}"
        );
    }

    fn diamond_points() -> [[f32; 3]; 6] {
        [
            [0.5, 0.0, 0.0],
            [-0.5, 0.0, 0.0],
            [0.0, 0.6, 0.0],
            [0.0, -0.6, 0.0],
            [0.0, 0.0, 0.5],
            [0.0, 0.0, -0.5],
        ]
    }

    #[test]
    fn dynamic_convex_hull_rests_on_ground() {
        let mut world =
            RapierDynamicsWorld3d::new(DynamicsWorldConfig3d::earth_60hz()).expect("world");
        let _ground = world
            .insert_fixed_cuboid([0.0, -0.25, 0.0], [3.0, 0.25, 3.0])
            .expect("ground");
        let hull = world
            .insert_dynamic_convex_hull([0.0, 2.5, 0.0], &diamond_points())
            .expect("hull");

        for _ in 0..240 {
            world.step(None).expect("step");
        }

        let end = world.translation(hull).expect("hull");
        assert!(
            end[1] > 0.4 && end[1] < 1.2,
            "convex diamond should rest above ground, got {end:?}"
        );
    }

    #[test]
    fn prismatic_joint_respects_limits() {
        let mut world =
            RapierDynamicsWorld3d::new(DynamicsWorldConfig3d::earth_60hz()).expect("world");
        let rail = world
            .insert_fixed_cuboid([0.0, 1.0, 0.0], [0.2, 0.2, 0.2])
            .expect("rail");
        let slider = world
            .insert_dynamic_cuboid([0.0, 1.0, 0.0], [0.25, 0.25, 0.25])
            .expect("slider");
        let _joint = world
            .insert_prismatic_joint(
                rail,
                slider,
                [1.0, 0.0, 0.0],
                [0.0, 0.0, 0.0],
                [0.0, 0.0, 0.0],
                Some([-0.8, 0.8]),
            )
            .expect("prismatic");
        world
            .set_linear_velocity(slider, [6.0, 0.0, 0.0])
            .expect("kick");

        for _ in 0..180 {
            world.step(None).expect("step");
        }

        let end = world.translation(slider).expect("slider");
        assert!(
            end[0] > 0.5 && end[0] < 1.0,
            "slider should stop near +limit 0.8, got {end:?}"
        );
        assert!(
            (end[1] - 1.0).abs() < 0.25,
            "prismatic should lock Y, got {end:?}"
        );
    }

    #[test]
    fn contact_pairs_report_resting_sphere() {
        let mut world =
            RapierDynamicsWorld3d::new(DynamicsWorldConfig3d::earth_60hz()).expect("world");
        let ground = world
            .insert_fixed_cuboid([0.0, -0.25, 0.0], [3.0, 0.25, 3.0])
            .expect("ground");
        let ball = world
            .insert_dynamic_sphere([0.0, 2.0, 0.0], 0.4)
            .expect("ball");

        for _ in 0..180 {
            world.step(None).expect("step");
        }

        let contacts = world.collect_contact_pairs();
        assert!(
            contacts
                .iter()
                .any(|pair| pair.body_a == ground && pair.body_b == ball
                    || pair.body_a == ball && pair.body_b == ground),
            "expected ground/ball contact, got {contacts:?}"
        );
        let hit = contacts
            .iter()
            .find(|pair| {
                (pair.body_a == ground && pair.body_b == ball)
                    || (pair.body_a == ball && pair.body_b == ground)
            })
            .expect("contact");
        assert!(hit.normal[1].abs() > 0.5, "normal should be mostly vertical");
    }

    #[test]
    fn rope_joint_limits_separation() {
        let mut world =
            RapierDynamicsWorld3d::new(DynamicsWorldConfig3d::earth_60hz()).expect("world");
        let anchor = world
            .insert_fixed_cuboid([0.0, 3.0, 0.0], [0.15, 0.15, 0.15])
            .expect("anchor");
        let bob = world
            .insert_dynamic_sphere([0.0, 1.0, 0.0], 0.25)
            .expect("bob");
        let _rope = world
            .insert_rope_joint(anchor, bob, 1.2, [0.0, 0.0, 0.0], [0.0, 0.0, 0.0])
            .expect("rope");

        for _ in 0..180 {
            world.step(None).expect("step");
        }

        let end = world.translation(bob).expect("bob");
        let dx = end[0];
        let dy = end[1] - 3.0;
        let dz = end[2];
        let dist = (dx * dx + dy * dy + dz * dz).sqrt();
        assert!(
            dist < 1.35,
            "rope should keep bob within ~1.2m, dist={dist}, end={end:?}"
        );
        assert!(end[1] > 1.5, "bob should hang, not fall freely, y={}", end[1]);
    }

    #[test]
    fn fixed_trimesh_wall_stops_dynamic_sphere() {
        let mut world =
            RapierDynamicsWorld3d::new(DynamicsWorldConfig3d::earth_60hz()).expect("world");
        // Vertical wall on X=1 (two triangles in YZ).
        let vertices = [
            [1.0, 0.0, -2.0],
            [1.0, 3.0, -2.0],
            [1.0, 3.0, 2.0],
            [1.0, 0.0, 2.0],
        ];
        let indices = [[0_u32, 1, 2], [0, 2, 3]];
        let _wall = world
            .insert_fixed_trimesh(&vertices, &indices)
            .expect("wall");
        let _floor = world
            .insert_fixed_cuboid([0.0, -0.25, 0.0], [4.0, 0.25, 4.0])
            .expect("floor");
        let ball = world
            .insert_dynamic_sphere([-1.0, 1.0, 0.0], 0.3)
            .expect("ball");
        world
            .set_linear_velocity(ball, [8.0, 0.0, 0.0])
            .expect("kick");

        for _ in 0..180 {
            world.step(None).expect("step");
        }

        let end = world.translation(ball).expect("ball");
        assert!(
            end[0] < 0.85,
            "trimesh wall should stop the ball before x=1, got {end:?}"
        );
        assert!(end[1] > 0.2, "ball should stay above floor, y={}", end[1]);
    }
}
