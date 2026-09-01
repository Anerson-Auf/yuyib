//! ECS-facing retained vector render data.
//!
//! This module deliberately owns only simulation-visible data and deterministic
//! extraction. Tessellation, GPU residency and drawing remain in
//! `yuyib-render-2d`, so gameplay systems can be tested without a GPU.

use std::time::Duration;

use yuyib_ecs::{bevy_ecs::entity::Entity, prelude::*};
use yuyib_render_2d::{VectorDraw2d, VectorMeshId2d};

/// Explicit painter layer shared by vector shapes, decals and particles.
#[derive(Component, Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct Layer2d(pub i32);

/// An ECS entity rendered from one retained [`VectorMeshId2d`].
///
/// Mesh topology is owned by `RetainedVectorScene2d`; this component changes
/// only lightweight instance state on normal gameplay frames.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct VectorShape2d {
    /// Stable handle of the immutable mesh stored in the renderer scene.
    pub mesh: VectorMeshId2d,
    /// Per-frame transform/tint state.
    pub draw: VectorDraw2d,
}

impl VectorShape2d {
    /// Creates an identity-transformed vector shape.
    #[must_use]
    pub const fn new(mesh: VectorMeshId2d) -> Self {
        Self {
            mesh,
            draw: VectorDraw2d::new(),
        }
    }

    /// Replaces instance state while preserving the mesh handle.
    #[must_use]
    pub const fn with_draw(mut self, draw: VectorDraw2d) -> Self {
        self.draw = draw;
        self
    }
}

/// Marker for a vector shape that is world dressing rather than an actor.
///
/// Rendering is unchanged; gameplay can use this marker to keep decals out of
/// interaction/physics queries and to apply its own lifetime policy.
#[derive(Component, Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Decal2d;

/// Kinematic visual particle state for an entity that also has [`VectorShape2d`].
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct Particle2d {
    /// World-space velocity in units per second.
    pub velocity: [f32; 2],
    /// Constant world-space acceleration in units per second squared.
    pub acceleration: [f32; 2],
    /// Remaining life. The entity is despawned at or below zero.
    pub remaining: Duration,
}

impl Particle2d {
    /// Creates a particle with zero acceleration.
    #[must_use]
    pub const fn new(velocity: [f32; 2], remaining: Duration) -> Self {
        Self {
            velocity,
            acceleration: [0.0, 0.0],
            remaining,
        }
    }

    /// Sets constant acceleration.
    #[must_use]
    pub const fn with_acceleration(mut self, acceleration: [f32; 2]) -> Self {
        self.acceleration = acceleration;
        self
    }
}

/// Generic authored emission policy.
///
/// The engine stores the policy but intentionally does not choose a random
/// distribution, spawn timing source, or target entity: those are game rules.
/// A gameplay system can use this data to spawn [`VectorShape2d`] +
/// [`Particle2d`] entities deterministically.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct ParticleEmitter2d {
    /// Mesh that emitted particles should reference.
    pub mesh: VectorMeshId2d,
    /// Maximum live particles allowed by the owning gameplay system.
    pub max_live: u32,
    /// Requested emissions per second.
    pub rate_per_second: f32,
    /// Lifetime assigned to each emitted particle.
    pub lifetime: Duration,
}

impl ParticleEmitter2d {
    /// Creates an explicit bounded emission policy.
    #[must_use]
    pub const fn new(
        mesh: VectorMeshId2d,
        max_live: u32,
        rate_per_second: f32,
        lifetime: Duration,
    ) -> Self {
        Self {
            mesh,
            max_live,
            rate_per_second,
            lifetime,
        }
    }
}

/// A renderer-ready snapshot of ECS vector shapes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExtractedVectorShapes2d {
    draws: Vec<(VectorMeshId2d, VectorDraw2d)>,
}

impl ExtractedVectorShapes2d {
    /// Returns draws in stable layer/entity painter order.
    #[must_use]
    pub fn draws(&self) -> &[(VectorMeshId2d, VectorDraw2d)] {
        &self.draws
    }

    /// Returns the number of extracted shapes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.draws.len()
    }

    /// Returns whether no shape was extracted.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.draws.is_empty()
    }
}

/// Extracts all vector shapes in deterministic painter order.
///
/// Needs mutable world access only for Bevy's lazily initialized query state;
/// it never mutates a gameplay component.
#[must_use]
pub fn extract_vector_shapes_2d(world: &mut World) -> ExtractedVectorShapes2d {
    let mut draws: Vec<(u64, VectorMeshId2d, VectorDraw2d)> = world
        .query::<(Entity, &VectorShape2d)>()
        .iter(world)
        .map(|(entity, shape)| (entity.to_bits(), shape.mesh, shape.draw))
        .collect();
    draws.sort_by_key(|(entity, _, draw)| (draw.layer, *entity));
    ExtractedVectorShapes2d {
        draws: draws
            .into_iter()
            .map(|(_, mesh, draw)| (mesh, draw))
            .collect(),
    }
}

/// Advances vector-particle transforms and despawns expired particle entities.
///
/// Entities missing [`VectorShape2d`] are ignored rather than despawned, so a
/// partially constructed entity cannot be destroyed by a render-only system.
/// The returned entity IDs are in deterministic ascending order.
pub fn step_vector_particles_2d(world: &mut World, delta: Duration) -> Vec<Entity> {
    let seconds = delta.as_secs_f32();
    let mut expired = Vec::new();
    let mut query = world.query::<(Entity, &mut VectorShape2d, &mut Particle2d)>();
    for (entity, mut shape, mut particle) in query.iter_mut(world) {
        if !particle
            .velocity
            .iter()
            .chain(particle.acceleration.iter())
            .all(|value| value.is_finite())
        {
            expired.push(entity);
            continue;
        }
        let displacement = [
            particle.velocity[0] * seconds + 0.5 * particle.acceleration[0] * seconds * seconds,
            particle.velocity[1] * seconds + 0.5 * particle.acceleration[1] * seconds * seconds,
        ];
        shape.draw.position[0] += displacement[0];
        shape.draw.position[1] += displacement[1];
        particle.velocity[0] += particle.acceleration[0] * seconds;
        particle.velocity[1] += particle.acceleration[1] * seconds;
        particle.remaining = particle.remaining.saturating_sub(delta);
        if particle.remaining.is_zero() {
            expired.push(entity);
        }
    }
    expired.sort_by_key(|entity| entity.to_bits());
    for entity in &expired {
        let _ = world.despawn(*entity);
    }
    expired
}
