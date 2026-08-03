//! ECS-facing 3D scene extraction.
//!
//! [`Model3d`], [`Transform3d`] and [`DirectionalLight3d`] belong on gameplay
//! entities. At the frame boundary [`extract_models`] and
//! [`extract_directional_lights`] create owned, renderer-neutral snapshots.
//! Does not create GPU resources or prescribe a 3D rendering backend; a renderer
//! can consume [`ExtractedModels`] and
//! [`ExtractedDirectionalLights`] without borrowing the ECS [`World`].
//! [`filter_extracted_models_by_frustum_3d`] can then produce a second owned
//! snapshot using validated, caller-owned local bounds. This keeps visibility
//! policy independent from WGPU, a concrete camera type and the asset importer.
//!
//! # Stable ordering
//!
//! Extraction sorts by ascending [`Model3d::render_order`] and then the full
//! generational entity ID. Equal orders are therefore deterministic across
//! repeated extractions of the same world. Batches only join adjacent draws
//! referring to the same model handle. This avoids silently changing an
//! explicit order, which is important for future transparent material phases.
//! A backend may make a separate, documented opaque-depth batching decision.
//!
//! ```
//! use yuyib_assets::Assets;
//! use yuyib_ecs::prelude::*;
//! use yuyib_game_3d::{extract_models, Model3d, Transform3d};
//! use yuyib_model::Model;
//!
//! let mut models = Assets::new();
//! let cube = models.insert(Model::cube(0.5).expect("valid cube"));
//! let mut world = World::new();
//! world.spawn((Model3d::new(cube), Transform3d::from_translation([0.0, 1.0, 0.0])));
//!
//! let extracted = extract_models(&mut world);
//! assert_eq!(extracted.model_count(), 1);
//! ```

#![forbid(unsafe_code)]

mod navigation;

pub use navigation::*;

use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fmt,
};

use yuyib_assets::Assets;
use yuyib_ecs::{bevy_ecs::entity::Entity, prelude::*};
use yuyib_model::{Model, ModelHandle};
use yuyib_physics::{TriangleMesh3d, TriangleMeshError, Vec3 as PhysicsVec3};

/// An affine transform for a 3D scene entity.
///
/// Rotation is a quaternion in `[x, y, z, w]` order. This CPU-facing type
/// purposefully stores authoring data rather than a matrix so a renderer can
/// choose its own matrix layout and coordinate upload convention.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct Transform3d {
    /// World-space translation in engine units.
    pub translation: [f32; 3],
    /// Unit quaternion in `[x, y, z, w]` order.
    pub rotation: [f32; 4],
    /// Per-axis scale. Negative values mirror geometry on that axis.
    pub scale: [f32; 3],
}

impl Transform3d {
    /// The identity transform at the world origin.
    pub const IDENTITY: Self = Self {
        translation: [0.0; 3],
        rotation: [0.0, 0.0, 0.0, 1.0],
        scale: [1.0; 3],
    };

    /// Creates an identity transform translated to `translation`.
    #[must_use]
    pub const fn from_translation(translation: [f32; 3]) -> Self {
        Self {
            translation,
            ..Self::IDENTITY
        }
    }

    /// Replaces the world-space translation.
    #[must_use]
    pub const fn with_translation(mut self, translation: [f32; 3]) -> Self {
        self.translation = translation;
        self
    }

    /// Replaces the quaternion rotation in `[x, y, z, w]` order.
    ///
    /// The quaternion is not normalized here. This is intentional for a
    /// lightweight component; importers and animation systems should maintain
    /// unit quaternions before render extraction.
    #[must_use]
    pub const fn with_rotation(mut self, rotation: [f32; 4]) -> Self {
        self.rotation = rotation;
        self
    }

    /// Replaces the per-axis scale.
    #[must_use]
    pub const fn with_scale(mut self, scale: [f32; 3]) -> Self {
        self.scale = scale;
        self
    }

    /// Replaces all three scale axes with one uniform factor.
    ///
    /// Values below one shrink the object and values above one enlarge it.
    /// Zero remains invalid for lit rendering and hierarchy propagation.
    #[must_use]
    pub const fn with_uniform_scale(mut self, scale: f32) -> Self {
        self.scale = [scale; 3];
        self
    }
}

impl Default for Transform3d {
    fn default() -> Self {
        Self::IDENTITY
    }
}

/// Parent relationship for a [`LocalTransform3d`] entity.
///
/// Use [`set_parent_3d`] for authoring because it validates both entities and
/// rejects an immediate cycle. Importers that add this component directly are
/// still protected by [`propagate_world_transforms`], which validates the
/// complete graph before mutating world transforms.
#[derive(Component, Clone, Copy, Debug, Eq, PartialEq)]
pub struct Parent3d(Entity);

impl Parent3d {
    /// Creates a parent reference for import adapters and serialized scenes.
    ///
    /// Prefer [`set_parent_3d`] when an ECS world is available, because this
    /// constructor cannot establish that `parent` exists or that it is acyclic.
    #[must_use]
    pub const fn new(parent: Entity) -> Self {
        Self(parent)
    }

    /// Returns the referenced parent entity.
    #[must_use]
    pub const fn entity(self) -> Entity {
        self.0
    }
}

/// Local authoring transform used by the 3D hierarchy system.
///
/// Fields intentionally remain public for scene import adapters. They are
/// validated, normalized and composed only at
/// [`propagate_world_transforms`]; invalid imported values therefore produce a
/// structured error instead of silently reaching rendering. Rotation uses the
/// same `[x, y, z, w]` quaternion convention as [`Transform3d`].
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct LocalTransform3d {
    /// Translation relative to the parent, in engine units.
    pub translation: [f32; 3],
    /// Local quaternion rotation in `[x, y, z, w]` order.
    pub rotation: [f32; 4],
    /// Local per-axis scale. Zero is invalid when propagation runs.
    pub scale: [f32; 3],
}

impl LocalTransform3d {
    /// The identity local transform.
    pub const IDENTITY: Self = Self {
        translation: [0.0; 3],
        rotation: [0.0, 0.0, 0.0, 1.0],
        scale: [1.0; 3],
    };

    /// Creates an identity local transform translated to `translation`.
    #[must_use]
    pub const fn from_translation(translation: [f32; 3]) -> Self {
        Self {
            translation,
            ..Self::IDENTITY
        }
    }

    /// Replaces the local translation.
    #[must_use]
    pub const fn with_translation(mut self, translation: [f32; 3]) -> Self {
        self.translation = translation;
        self
    }

    /// Replaces the local quaternion rotation.
    #[must_use]
    pub const fn with_rotation(mut self, rotation: [f32; 4]) -> Self {
        self.rotation = rotation;
        self
    }

    /// Replaces the local scale.
    #[must_use]
    pub const fn with_scale(mut self, scale: [f32; 3]) -> Self {
        self.scale = scale;
        self
    }

    /// Replaces all three local scale axes with one uniform factor.
    #[must_use]
    pub const fn with_uniform_scale(mut self, scale: f32) -> Self {
        self.scale = [scale; 3];
        self
    }
}

impl Default for LocalTransform3d {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl From<Transform3d> for LocalTransform3d {
    fn from(transform: Transform3d) -> Self {
        Self {
            translation: transform.translation,
            rotation: transform.rotation,
            scale: transform.scale,
        }
    }
}

/// Exact local affine transform stored in glTF/WGPU column-major order.
///
/// This component is for sources which author a node with an explicit matrix
/// instead of TRS.  It is deliberately separate from [`LocalTransform3d`]:
/// changing the latter into an enum would break ergonomic gameplay mutation,
/// while decomposing an affine matrix would lose shear or transform order.
///
/// An entity must carry exactly one of `LocalTransform3d` or
/// `LocalMatrixTransform3d` when it participates in a [`Parent3d`] hierarchy.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct LocalMatrixTransform3d {
    column_major: [f32; 16],
}

impl LocalMatrixTransform3d {
    /// Creates a local matrix.
    ///
    /// The matrix is validated during [`propagate_world_transforms`] so scene
    /// import can remain transactional alongside the rest of the hierarchy.
    #[must_use]
    pub const fn new(column_major: [f32; 16]) -> Self {
        Self { column_major }
    }

    /// Returns the authored matrix without decomposition or conversion.
    #[must_use]
    pub const fn column_major(self) -> [f32; 16] {
        self.column_major
    }
}

/// Validated world transform produced by [`propagate_world_transforms`].
///
/// The fields are intentionally exposed through getters only, because this is
/// a derived snapshot. To change hierarchy authoring data, update
/// [`LocalTransform3d`] and propagate again.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct WorldTransform3d {
    column_major: [f32; 16],
    trs: Option<Transform3d>,
}

impl WorldTransform3d {
    /// Returns world-space translation.
    #[must_use]
    pub const fn translation(self) -> [f32; 3] {
        [
            self.column_major[12],
            self.column_major[13],
            self.column_major[14],
        ]
    }

    /// Returns the exact world transform in column-major order.
    #[must_use]
    pub const fn column_major(self) -> [f32; 16] {
        self.column_major
    }

    /// Returns a TRS view when the exact affine matrix has no shear.
    ///
    /// Matrix-authored hierarchies may compose into shear even when individual
    /// source nodes do not. In that case this returns `None` rather than a
    /// lossy approximation.
    #[must_use]
    pub const fn as_trs(self) -> Option<Transform3d> {
        self.trs
    }

    /// Returns the normalized world quaternion when a lossless TRS view exists.
    #[must_use]
    pub const fn rotation(self) -> Option<[f32; 4]> {
        match self.trs {
            Some(transform) => Some(transform.rotation),
            None => None,
        }
    }

    /// Returns world-space per-axis scale when a lossless TRS view exists.
    #[must_use]
    pub const fn scale(self) -> Option<[f32; 3]> {
        match self.trs {
            Some(transform) => Some(transform.scale),
            None => None,
        }
    }

    /// Produces a legacy render component only when the exact world matrix is
    /// representable as TRS.
    ///
    /// [`propagate_world_transforms`] performs this synchronization itself for
    /// every entity with a [`LocalTransform3d`]. This method is available to a
    /// custom renderer or scene adapter that needs the same data explicitly.
    #[must_use]
    pub const fn as_render_transform(self) -> Option<Transform3d> {
        self.trs
    }
}

/// Failure while authoring or resolving a 3D transform hierarchy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransformHierarchyError {
    /// A requested entity does not exist in the target ECS world.
    MissingEntity {
        /// Entity that was absent.
        entity: Entity,
    },
    /// An entity was assigned as its own parent.
    SelfParent {
        /// Child that referenced itself.
        child: Entity,
    },
    /// A child references a despawned or otherwise absent parent.
    MissingParent {
        /// Child holding the stale reference.
        child: Entity,
        /// Missing parent entity.
        parent: Entity,
    },
    /// A parent exists but has no [`LocalTransform3d`] to propagate.
    MissingParentTransform {
        /// Child that needs the parent's world transform.
        child: Entity,
        /// Parent without a local transform.
        parent: Entity,
    },
    /// An entity has both local transform component types.
    ConflictingLocalTransforms {
        /// Entity with ambiguous local transform authoring.
        entity: Entity,
    },
    /// Traversal reached an ancestor already on its active resolution path.
    Cycle {
        /// Entity at which the cycle was detected.
        entity: Entity,
    },
    /// A local transform cannot produce a finite, non-degenerate world value.
    InvalidLocalTransform {
        /// Entity owning the invalid local transform.
        entity: Entity,
        /// Stable reason suitable for diagnostics/UI.
        reason: &'static str,
    },
    /// Composition produced a non-finite or zero-scale world transform.
    InvalidWorldTransform {
        /// Entity whose derived world transform failed validation.
        entity: Entity,
    },
}

impl fmt::Display for TransformHierarchyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingEntity { entity } => {
                write!(formatter, "hierarchy entity is missing: {entity:?}")
            }
            Self::SelfParent { child } => {
                write!(formatter, "entity cannot parent itself: {child:?}")
            }
            Self::MissingParent { child, parent } => {
                write!(
                    formatter,
                    "entity {child:?} references missing parent {parent:?}"
                )
            }
            Self::MissingParentTransform { child, parent } => write!(
                formatter,
                "entity {child:?} references parent {parent:?} without LocalTransform3d"
            ),
            Self::ConflictingLocalTransforms { entity } => write!(
                formatter,
                "entity {entity:?} has both LocalTransform3d and LocalMatrixTransform3d"
            ),
            Self::Cycle { entity } => write!(
                formatter,
                "transform hierarchy contains a cycle at {entity:?}"
            ),
            Self::InvalidLocalTransform { entity, reason } => {
                write!(formatter, "invalid local transform on {entity:?}: {reason}")
            }
            Self::InvalidWorldTransform { entity } => {
                write!(formatter, "invalid derived world transform on {entity:?}")
            }
        }
    }
}

impl Error for TransformHierarchyError {}

/// Sets `parent` as the parent of `child` after validating the current graph.
///
/// This performs a bounded parent-chain walk and rejects an immediate cycle.
/// It does not run world-transform propagation; call
/// [`propagate_world_transforms`] at a defined frame boundary after all scene
/// mutations have completed.
///
/// # Errors
///
/// Returns [`TransformHierarchyError`] if either entity is missing, `child`
/// equals `parent`, or the proposed edge would introduce a cycle.
pub fn set_parent_3d(
    world: &mut World,
    child: Entity,
    parent: Entity,
) -> Result<(), TransformHierarchyError> {
    if world.get_entity(child).is_err() {
        return Err(TransformHierarchyError::MissingEntity { entity: child });
    }
    if world.get_entity(parent).is_err() {
        return Err(TransformHierarchyError::MissingEntity { entity: parent });
    }
    if child == parent {
        return Err(TransformHierarchyError::SelfParent { child });
    }

    let mut ancestor = parent;
    loop {
        if ancestor == child {
            return Err(TransformHierarchyError::Cycle { entity: child });
        }
        let Some(next_parent) = world.get::<Parent3d>(ancestor).copied() else {
            break;
        };
        ancestor = next_parent.entity();
        if world.get_entity(ancestor).is_err() {
            return Err(TransformHierarchyError::MissingParent {
                child: parent,
                parent: ancestor,
            });
        }
    }
    // Propagation only walks LocalTransform3d / LocalMatrixTransform3d.
    // Authoring roots often carry world Transform3d only — promote both ends
    // so parenting a glTF hierarchy under a scene entity cannot leave a
    // Parent3d edge that every render pass rejects.
    ensure_hierarchy_local(world, parent, Some(child))?;
    ensure_hierarchy_local(world, child, None)?;
    world.entity_mut(child).insert(Parent3d::new(parent));
    Ok(())
}

fn ensure_hierarchy_local(
    world: &mut World,
    entity: Entity,
    missing_as_parent_of: Option<Entity>,
) -> Result<(), TransformHierarchyError> {
    if world.get::<LocalTransform3d>(entity).is_some()
        || world.get::<LocalMatrixTransform3d>(entity).is_some()
    {
        return Ok(());
    }
    if let Some(transform) = world.get::<Transform3d>(entity).copied() {
        world
            .entity_mut(entity)
            .insert(LocalTransform3d::from(transform));
        return Ok(());
    }
    if let Some(child) = missing_as_parent_of {
        return Err(TransformHierarchyError::MissingParentTransform {
            child,
            parent: entity,
        });
    }
    world.entity_mut(entity).insert(LocalTransform3d::IDENTITY);
    Ok(())
}

/// Removes the parent edge from `child`.
///
/// Returns `true` when an edge existed. It does not change the child's local
/// transform, so its next propagated world transform becomes rooted at that
/// same local value.
///
/// # Errors
///
/// Returns [`TransformHierarchyError::MissingEntity`] if `child` is absent.
pub fn clear_parent_3d(world: &mut World, child: Entity) -> Result<bool, TransformHierarchyError> {
    if world.get_entity(child).is_err() {
        return Err(TransformHierarchyError::MissingEntity { entity: child });
    }
    let had_parent = world.get::<Parent3d>(child).is_some();
    if had_parent {
        world.entity_mut(child).remove::<Parent3d>();
    }
    Ok(had_parent)
}

/// Counts successful world-transform propagation work.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TransformPropagationStats {
    /// Number of entities with a [`LocalTransform3d`] updated this pass.
    pub updated: usize,
    /// Number of such entities without a [`Parent3d`].
    pub roots: usize,
}

#[derive(Clone, Copy)]
struct HierarchyNode {
    local: HierarchyLocalTransform,
    parent: Option<Entity>,
}

#[derive(Clone, Copy)]
enum HierarchyLocalTransform {
    Trs(LocalTransform3d),
    Matrix(LocalMatrixTransform3d),
}

/// Resolves local transforms through parent edges and writes world snapshots.
///
/// The system validates the entire relevant hierarchy before mutating any
/// [`WorldTransform3d`] or legacy [`Transform3d`] component. This gives scene
/// importers transactional failure semantics: a cycle, stale parent, missing
/// parent transform or invalid local value leaves previously propagated output
/// untouched. Entities are resolved in ascending full generational entity ID
/// order for deterministic diagnostics and output writes.
///
/// Every entity with a local transform component receives a
/// [`WorldTransform3d`]. A legacy [`Transform3d`] is synchronized only when
/// the exact matrix remains losslessly representable as TRS. This prevents a
/// matrix-authored glTF node from being silently decomposed into a visibly
/// different transform. [`extract_models`] always uses the exact matrix.
///
/// # Errors
///
/// Returns [`TransformHierarchyError`] if hierarchy validation or transform
/// composition fails. No derived components are changed in that case.
pub fn propagate_world_transforms(
    world: &mut World,
) -> Result<TransformPropagationStats, TransformHierarchyError> {
    let mut nodes = HashMap::new();
    let mut entities = Vec::new();
    for (entity, local, matrix, parent) in world
        .query::<(
            Entity,
            Option<&LocalTransform3d>,
            Option<&LocalMatrixTransform3d>,
            Option<&Parent3d>,
        )>()
        .iter(world)
    {
        let local = match (local, matrix) {
            (Some(local), None) => HierarchyLocalTransform::Trs(*local),
            (None, Some(matrix)) => HierarchyLocalTransform::Matrix(*matrix),
            (Some(_), Some(_)) => {
                return Err(TransformHierarchyError::ConflictingLocalTransforms { entity });
            }
            (None, None) => continue,
        };
        nodes.insert(
            entity,
            HierarchyNode {
                local,
                parent: parent.copied().map(Parent3d::entity),
            },
        );
        entities.push(entity);
    }
    entities.sort_by_key(|entity| entity.to_bits());

    let mut roots = 0;
    for entity in &entities {
        let node = nodes
            .get(entity)
            .ok_or(TransformHierarchyError::MissingEntity { entity: *entity })?;
        if let Some(parent) = node.parent {
            if parent == *entity {
                return Err(TransformHierarchyError::SelfParent { child: *entity });
            }
            if world.get_entity(parent).is_err() {
                return Err(TransformHierarchyError::MissingParent {
                    child: *entity,
                    parent,
                });
            }
            if !nodes.contains_key(&parent) {
                return Err(TransformHierarchyError::MissingParentTransform {
                    child: *entity,
                    parent,
                });
            }
        } else {
            roots += 1;
        }
    }

    let mut resolved = HashMap::new();
    let mut resolving = HashSet::new();
    for entity in &entities {
        resolve_world_transform(*entity, &nodes, &mut resolved, &mut resolving)?;
    }

    for entity in entities {
        let transform = *resolved
            .get(&entity)
            .ok_or(TransformHierarchyError::MissingEntity { entity })?;
        let mut entity_world = world.entity_mut(entity);
        entity_world.insert(transform);
        if let Some(render_transform) = transform.as_render_transform() {
            entity_world.insert(render_transform);
        } else {
            entity_world.remove::<Transform3d>();
        }
    }
    Ok(TransformPropagationStats {
        updated: resolved.len(),
        roots,
    })
}

fn resolve_world_transform(
    entity: Entity,
    nodes: &HashMap<Entity, HierarchyNode>,
    resolved: &mut HashMap<Entity, WorldTransform3d>,
    resolving: &mut HashSet<Entity>,
) -> Result<WorldTransform3d, TransformHierarchyError> {
    if let Some(transform) = resolved.get(&entity) {
        return Ok(*transform);
    }
    if !resolving.insert(entity) {
        return Err(TransformHierarchyError::Cycle { entity });
    }
    let node = nodes
        .get(&entity)
        .ok_or(TransformHierarchyError::MissingEntity { entity })?;
    let local = validate_local_transform(entity, node.local)?;
    let world_transform = if let Some(parent) = node.parent {
        let parent_transform = resolve_world_transform(parent, nodes, resolved, resolving)?;
        compose_world_transform(entity, parent_transform, local)?
    } else {
        local
    };
    resolving.remove(&entity);
    resolved.insert(entity, world_transform);
    Ok(world_transform)
}

fn validate_local_transform(
    entity: Entity,
    local: HierarchyLocalTransform,
) -> Result<WorldTransform3d, TransformHierarchyError> {
    match local {
        HierarchyLocalTransform::Trs(local) => validate_trs_local_transform(entity, local),
        HierarchyLocalTransform::Matrix(local) => {
            let column_major = local.column_major();
            if !all_finite(&column_major) {
                return Err(TransformHierarchyError::InvalidLocalTransform {
                    entity,
                    reason: "matrix components must be finite",
                });
            }
            if !is_affine_matrix(column_major) {
                return Err(TransformHierarchyError::InvalidLocalTransform {
                    entity,
                    reason: "matrix must be affine with final row [0, 0, 0, 1]",
                });
            }
            Ok(WorldTransform3d {
                column_major,
                trs: decompose_trs(column_major),
            })
        }
    }
}

fn validate_trs_local_transform(
    entity: Entity,
    local: LocalTransform3d,
) -> Result<WorldTransform3d, TransformHierarchyError> {
    if !all_finite(&local.translation) || !all_finite(&local.rotation) || !all_finite(&local.scale)
    {
        return Err(TransformHierarchyError::InvalidLocalTransform {
            entity,
            reason: "translation, rotation and scale must be finite",
        });
    }
    if local.scale.contains(&0.0) {
        return Err(TransformHierarchyError::InvalidLocalTransform {
            entity,
            reason: "scale components must not be zero",
        });
    }
    let rotation = normalize_quaternion(local.rotation).ok_or(
        TransformHierarchyError::InvalidLocalTransform {
            entity,
            reason: "quaternion rotation must have non-zero finite length",
        },
    )?;
    let transform = Transform3d {
        translation: local.translation,
        rotation,
        scale: local.scale,
    };
    Ok(WorldTransform3d {
        column_major: transform_matrix(transform),
        trs: Some(transform),
    })
}

fn compose_world_transform(
    entity: Entity,
    parent: WorldTransform3d,
    local: WorldTransform3d,
) -> Result<WorldTransform3d, TransformHierarchyError> {
    let column_major = multiply_matrix(parent.column_major, local.column_major);
    if !all_finite(&column_major) || !is_affine_matrix(column_major) {
        return Err(TransformHierarchyError::InvalidWorldTransform { entity });
    }
    Ok(WorldTransform3d {
        column_major,
        trs: decompose_trs(column_major),
    })
}

fn normalize_quaternion(quaternion: [f32; 4]) -> Option<[f32; 4]> {
    let length_squared = quaternion.iter().map(|value| value * value).sum::<f32>();
    if !length_squared.is_finite() || length_squared <= f32::EPSILON {
        return None;
    }
    let inverse_length = length_squared.sqrt().recip();
    let normalized = quaternion.map(|value| value * inverse_length);
    all_finite(&normalized).then_some(normalized)
}

fn transform_matrix(transform: Transform3d) -> [f32; 16] {
    let [x, y, z, w] = transform.rotation;
    let xx = x * x;
    let yy = y * y;
    let zz = z * z;
    let xy = x * y;
    let xz = x * z;
    let yz = y * z;
    let wx = w * x;
    let wy = w * y;
    let wz = w * z;
    let rotation = [
        1.0 - 2.0 * (yy + zz),
        2.0 * (xy + wz),
        2.0 * (xz - wy),
        0.0,
        2.0 * (xy - wz),
        1.0 - 2.0 * (xx + zz),
        2.0 * (yz + wx),
        0.0,
        2.0 * (xz + wy),
        2.0 * (yz - wx),
        1.0 - 2.0 * (xx + yy),
        0.0,
        0.0,
        0.0,
        0.0,
        1.0,
    ];
    [
        rotation[0] * transform.scale[0],
        rotation[1] * transform.scale[0],
        rotation[2] * transform.scale[0],
        0.0,
        rotation[4] * transform.scale[1],
        rotation[5] * transform.scale[1],
        rotation[6] * transform.scale[1],
        0.0,
        rotation[8] * transform.scale[2],
        rotation[9] * transform.scale[2],
        rotation[10] * transform.scale[2],
        0.0,
        transform.translation[0],
        transform.translation[1],
        transform.translation[2],
        1.0,
    ]
}

fn multiply_matrix(left: [f32; 16], right: [f32; 16]) -> [f32; 16] {
    let mut result = [0.0; 16];
    for column in 0..4 {
        for row in 0..4 {
            result[column * 4 + row] = (0..4)
                .map(|index| left[index * 4 + row] * right[column * 4 + index])
                .sum();
        }
    }
    result
}

fn is_affine_matrix(matrix: [f32; 16]) -> bool {
    const AFFINE_EPSILON: f32 = 32.0 * f32::EPSILON;
    matrix[3].abs() <= AFFINE_EPSILON
        && matrix[7].abs() <= AFFINE_EPSILON
        && matrix[11].abs() <= AFFINE_EPSILON
        && (matrix[15] - 1.0).abs() <= AFFINE_EPSILON
}

fn decompose_trs(column_major: [f32; 16]) -> Option<Transform3d> {
    let basis = [
        [column_major[0], column_major[1], column_major[2]],
        [column_major[4], column_major[5], column_major[6]],
        [column_major[8], column_major[9], column_major[10]],
    ];
    let unsigned_scales = basis.map(length3);
    if unsigned_scales
        .iter()
        .any(|scale| !scale.is_finite() || *scale <= f32::EPSILON)
    {
        return None;
    }

    // An affine matrix can encode reflection in any basis column. Try every
    // signed-scale assignment and keep only a proper orthonormal rotation that
    // reconstructs the input; this avoids losing negative scale information.
    for signs in [
        [-1.0_f32, -1.0, -1.0],
        [-1.0, -1.0, 1.0],
        [-1.0, 1.0, -1.0],
        [-1.0, 1.0, 1.0],
        [1.0, -1.0, -1.0],
        [1.0, -1.0, 1.0],
        [1.0, 1.0, -1.0],
        [1.0, 1.0, 1.0],
    ] {
        let scale = [
            unsigned_scales[0] * signs[0],
            unsigned_scales[1] * signs[1],
            unsigned_scales[2] * signs[2],
        ];
        let rotation_columns = [
            scale3(basis[0], scale[0].recip()),
            scale3(basis[1], scale[1].recip()),
            scale3(basis[2], scale[2].recip()),
        ];
        if !is_proper_rotation(rotation_columns) {
            continue;
        }
        let rotation = quaternion_from_rotation_columns(rotation_columns)?;
        let candidate = Transform3d {
            translation: [column_major[12], column_major[13], column_major[14]],
            rotation,
            scale,
        };
        if matrices_approximately_equal(transform_matrix(candidate), column_major) {
            return Some(candidate);
        }
    }
    None
}

fn is_proper_rotation(columns: [[f32; 3]; 3]) -> bool {
    const EPSILON: f32 = 1.0e-4;
    (length3(columns[0]) - 1.0).abs() <= EPSILON
        && (length3(columns[1]) - 1.0).abs() <= EPSILON
        && (length3(columns[2]) - 1.0).abs() <= EPSILON
        && dot3(columns[0], columns[1]).abs() <= EPSILON
        && dot3(columns[0], columns[2]).abs() <= EPSILON
        && dot3(columns[1], columns[2]).abs() <= EPSILON
        && (dot3(cross3(columns[0], columns[1]), columns[2]) - 1.0).abs() <= EPSILON
}

fn quaternion_from_rotation_columns(columns: [[f32; 3]; 3]) -> Option<[f32; 4]> {
    let m00 = columns[0][0];
    let m01 = columns[1][0];
    let m02 = columns[2][0];
    let m10 = columns[0][1];
    let m11 = columns[1][1];
    let m12 = columns[2][1];
    let m20 = columns[0][2];
    let m21 = columns[1][2];
    let m22 = columns[2][2];
    let trace = m00 + m11 + m22;
    let quaternion = if trace > 0.0 {
        let scale = (trace + 1.0).sqrt() * 2.0;
        [
            (m21 - m12) / scale,
            (m02 - m20) / scale,
            (m10 - m01) / scale,
            scale * 0.25,
        ]
    } else if m00 > m11 && m00 > m22 {
        let scale = (1.0 + m00 - m11 - m22).sqrt() * 2.0;
        [
            scale * 0.25,
            (m01 + m10) / scale,
            (m02 + m20) / scale,
            (m21 - m12) / scale,
        ]
    } else if m11 > m22 {
        let scale = (1.0 + m11 - m00 - m22).sqrt() * 2.0;
        [
            (m01 + m10) / scale,
            scale * 0.25,
            (m12 + m21) / scale,
            (m02 - m20) / scale,
        ]
    } else {
        let scale = (1.0 + m22 - m00 - m11).sqrt() * 2.0;
        [
            (m02 + m20) / scale,
            (m12 + m21) / scale,
            scale * 0.25,
            (m10 - m01) / scale,
        ]
    };
    normalize_quaternion(quaternion)
}

fn matrices_approximately_equal(left: [f32; 16], right: [f32; 16]) -> bool {
    const EPSILON: f32 = 2.0e-4;
    left.iter()
        .zip(right)
        .all(|(left, right)| (left - right).abs() <= EPSILON)
}

fn dot3(left: [f32; 3], right: [f32; 3]) -> f32 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

fn cross3(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [
        left[1] * right[2] - left[2] * right[1],
        left[2] * right[0] - left[0] * right[2],
        left[0] * right[1] - left[1] * right[0],
    ]
}

fn length3(value: [f32; 3]) -> f32 {
    dot3(value, value).sqrt()
}

fn scale3(value: [f32; 3], factor: f32) -> [f32; 3] {
    [value[0] * factor, value[1] * factor, value[2] * factor]
}

fn all_finite<const N: usize>(values: &[f32; N]) -> bool {
    values.iter().all(|value| value.is_finite())
}

/// A renderable model assignment for a 3D entity.
///
/// Model residency and model validity are owned by the asset/render layers;
/// this component only retains a typed [`ModelHandle`].
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct Model3d {
    /// Typed handle to the CPU model asset.
    pub model: ModelHandle,
    /// Optional source mesh index. `None` draws every mesh in the model and
    /// is suitable for procedural single-entity models. Scene importers set
    /// this to preserve one glTF node-to-mesh instance relationship.
    pub mesh: Option<usize>,
    /// Whether extraction should include this entity.
    pub visible: bool,
    /// Stable frame order used as a future render-phase input.
    ///
    /// Lower values are extracted first. It is not a depth replacement: opaque
    /// and transparent sorting remains a renderer policy.
    pub render_order: i32,
    /// When true, the high-level scene draws this entity in a second pass after
    /// clearing depth so editor overlays stay visible through world geometry.
    pub overlay: bool,
}

impl Model3d {
    /// Assigns `model` as a visible renderable with default order zero.
    #[must_use]
    pub const fn new(model: ModelHandle) -> Self {
        Self {
            model,
            mesh: None,
            visible: true,
            render_order: 0,
            overlay: false,
        }
    }

    /// Sets whether this entity is included in the extracted scene.
    #[must_use]
    pub const fn with_visible(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    /// Restricts this entity to one source mesh in its referenced model.
    #[must_use]
    pub const fn with_mesh(mut self, mesh: usize) -> Self {
        self.mesh = Some(mesh);
        self
    }

    /// Sets the explicit stable extraction order.
    #[must_use]
    pub const fn with_render_order(mut self, render_order: i32) -> Self {
        self.render_order = render_order;
        self
    }

    /// Marks this draw as a depth-cleared overlay (editor gizmos, helpers).
    #[must_use]
    pub const fn with_overlay(mut self, overlay: bool) -> Self {
        self.overlay = overlay;
        self
    }
}

/// Independent draw gate (**nodraw** when `draw == false`).
///
/// Separate from collision: a nodraw entity can still contribute to the static
/// player mesh when [`CollisionFlags3d`] allows it. When absent, render uses
/// [`Model3d::visible`] only.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderFlags3d {
    /// When false, extraction skips this entity (nodraw).
    pub draw: bool,
}

impl RenderFlags3d {
    /// Visible / drawn.
    pub const DRAW: Self = Self { draw: true };
    /// Hidden from render (nodraw).
    pub const NODRAW: Self = Self { draw: false };

    /// Creates a draw flag.
    #[must_use]
    pub const fn new(draw: bool) -> Self {
        Self { draw }
    }
}

impl Default for RenderFlags3d {
    fn default() -> Self {
        Self::DRAW
    }
}

/// Independent solid-collision gate for CharacterController mesh builds.
///
/// **nocollide** = `enabled == false`. Selective filter: non-empty
/// [`collide_with`](Self::collide_with) must include `"player"` to stay in the
/// default Play locomotion mesh. Prop↔prop filtering is out of scope for the
/// single trimesh path (Rapier overlay later).
#[derive(Component, Clone, Debug, PartialEq, Eq)]
pub struct CollisionFlags3d {
    /// When false, excluded from static player mesh (full nocollide).
    pub enabled: bool,
    /// Empty = collide with all mesh consumers. Non-empty = only listed tags.
    pub collide_with: Vec<String>,
    /// Optional semantic layer tag for this entity (`door`, `prop`, …).
    pub layer: String,
}

impl CollisionFlags3d {
    /// Solid collision vs every consumer (default when component absent).
    #[must_use]
    pub fn solid() -> Self {
        Self {
            enabled: true,
            collide_with: Vec::new(),
            layer: String::new(),
        }
    }

    /// No solid mesh contribution.
    #[must_use]
    pub fn nocollide() -> Self {
        Self {
            enabled: false,
            collide_with: Vec::new(),
            layer: String::new(),
        }
    }

    /// Whether this entity should be included in the Play player locomotion mesh.
    #[must_use]
    pub fn contributes_to_player_mesh(&self) -> bool {
        if !self.enabled {
            return false;
        }
        self.collide_with.is_empty()
            || self
                .collide_with
                .iter()
                .any(|tag| tag.eq_ignore_ascii_case("player") || tag.eq_ignore_ascii_case("all"))
    }
}

impl Default for CollisionFlags3d {
    fn default() -> Self {
        Self::solid()
    }
}

/// Границы видимой 3D-сцены в мировых координатах.
///
/// Это готовый результат [`scene_bounds_3d`]. Он уже учитывает иерархию,
/// точные матрицы импортированных glTF-узлов и выбор одного mesh через
/// [`Model3d::with_mesh`]. Поэтому его можно сразу использовать для стартовой
/// позиции камеры, маркера центра или настройки дальности прорисовки.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SceneBounds3d {
    minimum: [f32; 3],
    maximum: [f32; 3],
    entity_count: usize,
    vertex_count: usize,
}

impl SceneBounds3d {
    /// Наименьшая точка границ по каждой оси.
    #[must_use]
    pub const fn minimum(self) -> [f32; 3] {
        self.minimum
    }

    /// Наибольшая точка границ по каждой оси.
    #[must_use]
    pub const fn maximum(self) -> [f32; 3] {
        self.maximum
    }

    /// Центральная точка сцены.
    #[must_use]
    pub fn centre(self) -> [f32; 3] {
        [
            self.minimum[0].midpoint(self.maximum[0]),
            self.minimum[1].midpoint(self.maximum[1]),
            self.minimum[2].midpoint(self.maximum[2]),
        ]
    }

    /// Полный размер сцены по осям `X`, `Y`, `Z`.
    #[must_use]
    pub fn size(self) -> [f32; 3] {
        [
            self.maximum[0] - self.minimum[0],
            self.maximum[1] - self.minimum[1],
            self.maximum[2] - self.minimum[2],
        ]
    }

    /// Радиус сферы от центра до самого дальнего угла границ.
    ///
    /// В отличие от максимального размера стороны, это безопасное значение
    /// для камеры: в сферу целиком помещается прямоугольник границ.
    #[must_use]
    pub fn radius(self) -> f32 {
        let size = self.size();
        (size[0] * size[0] + size[1] * size[1] + size[2] * size[2]).sqrt() * 0.5
    }

    /// Число ECS-сущностей, внёсших геометрию в границы.
    #[must_use]
    pub const fn entity_count(self) -> usize {
        self.entity_count
    }

    /// Число проверенных вершин с учётом индексов примитивов.
    #[must_use]
    pub const fn vertex_count(self) -> usize {
        self.vertex_count
    }
}

/// Результат расчёта [`scene_bounds_3d`].
///
/// `Empty` — нормальный результат для ещё не загруженной или действительно
/// пустой сцены, а не ошибка. Например, во время фоновой загрузки его можно
/// показать как состояние "ожидание геометрии" и повторить расчёт после
/// добавления моделей в [`Assets`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SceneBoundsResult3d {
    /// В мире пока нет ни одной видимой модели с доступной геометрией.
    Empty,
    /// Границы одной или нескольких видимых моделей.
    Bounds(SceneBounds3d),
}

impl SceneBoundsResult3d {
    /// Возвращает границы, если сцена не пуста.
    #[must_use]
    pub const fn bounds(self) -> Option<SceneBounds3d> {
        match self {
            Self::Empty => None,
            Self::Bounds(bounds) => Some(bounds),
        }
    }
}

/// Ошибка согласования ECS-сцены и геометрии при расчёте границ.
#[derive(Debug)]
pub enum SceneBoundsError3d {
    /// Иерархия не смогла построить актуальные мировые трансформации.
    Hierarchy(TransformHierarchyError),
    /// `Model3d::with_mesh` ссылается на mesh, которого нет в модели.
    MissingMesh {
        /// ECS-сущность с ошибочной настройкой.
        entity: Entity,
        /// Запрошенный номер mesh.
        mesh: usize,
        /// Число доступных mesh в модели.
        mesh_count: usize,
    },
    /// Индекс вершины нельзя представить на текущей платформе.
    VertexIndexNotRepresentable {
        /// ECS-сущность, которой принадлежит модель.
        entity: Entity,
        /// Индекс вершины из модели.
        index: u32,
    },
    /// Геометрия содержит нечисловую координату и не может дать надёжные границы.
    NonFiniteVertex {
        /// ECS-сущность, которой принадлежит модель.
        entity: Entity,
        /// Номер mesh в модели.
        mesh: usize,
        /// Номер примитива в mesh.
        primitive: usize,
        /// Номер вершины в потоке positions.
        vertex: usize,
    },
    /// Преобразование вершины дало нечисловую мировую координату.
    NonFiniteWorldPoint {
        /// ECS-сущность, которой принадлежит модель.
        entity: Entity,
    },
}

impl fmt::Display for SceneBoundsError3d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hierarchy(error) => write!(formatter, "cannot resolve scene hierarchy: {error}"),
            Self::MissingMesh {
                entity,
                mesh,
                mesh_count,
            } => write!(
                formatter,
                "entity {entity:?} selects mesh {mesh}, but its model has {mesh_count} meshes"
            ),
            Self::VertexIndexNotRepresentable { entity, index } => write!(
                formatter,
                "entity {entity:?} has a vertex index {index} unsupported by this platform"
            ),
            Self::NonFiniteVertex {
                entity,
                mesh,
                primitive,
                vertex,
            } => write!(
                formatter,
                "entity {entity:?} has a non-finite vertex at mesh {mesh}, primitive {primitive}, vertex {vertex}"
            ),
            Self::NonFiniteWorldPoint { entity } => {
                write!(
                    formatter,
                    "entity {entity:?} produced a non-finite world-space vertex"
                )
            }
        }
    }
}

impl Error for SceneBoundsError3d {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Hierarchy(error) => Some(error),
            Self::MissingMesh { .. }
            | Self::VertexIndexNotRepresentable { .. }
            | Self::NonFiniteVertex { .. }
            | Self::NonFiniteWorldPoint { .. } => None,
        }
    }
}

/// Рассчитывает границы всех видимых моделей в ECS-сцене.
///
/// Это высокоуровневый путь: сначала обновляется 3D-иерархия, затем берутся
/// только доступные сейчас модели и их выбранные mesh. Отсутствующая модель в
/// [`Assets`] пропускается — это позволяет безопасно вызывать функцию во время
/// асинхронной загрузки и пересчитывать границы при появлении очередного
/// ресурса. Ошибкой остаются только повреждённые данные или неверный номер
/// mesh.
///
/// Низкоуровневые системы, которые сами уже вызвали
/// [`propagate_world_transforms`], могут использовать
/// [`scene_bounds_3d_from_current_transforms`], чтобы не выполнять обход
/// иерархии повторно.
///
/// # Errors
///
/// Возвращает [`SceneBoundsError3d`] при ошибке иерархии, несуществующем mesh
/// или нечисловой геометрии. Пустая сцена возвращается как
/// [`SceneBoundsResult3d::Empty`].
pub fn scene_bounds_3d(
    world: &mut World,
    models: &Assets<Model>,
) -> Result<SceneBoundsResult3d, SceneBoundsError3d> {
    propagate_world_transforms(world).map_err(SceneBoundsError3d::Hierarchy)?;
    scene_bounds_3d_from_current_transforms(world, models)
}

/// Рассчитывает границы, используя уже актуальные мировые трансформации.
///
/// Это низкоуровневый вариант [`scene_bounds_3d`]. Он не меняет ECS-мир и не
/// запускает [`propagate_world_transforms`]. Сначала вызовите её сами в своём
/// порядке систем, если это необходимо.
///
/// Модели без [`WorldTransform3d`] используют обычный [`Transform3d`], как и
/// [`extract_models`]. Модель без доступного ресурса или без трансформации
/// пропускается; пустой результат возвращается как [`SceneBoundsResult3d::Empty`].
///
/// # Errors
///
/// Возвращает [`SceneBoundsError3d`] для неверного номера mesh или нечисловой
/// геометрии.
pub fn scene_bounds_3d_from_current_transforms(
    world: &mut World,
    models: &Assets<Model>,
) -> Result<SceneBoundsResult3d, SceneBoundsError3d> {
    let mut minimum = [f32::INFINITY; 3];
    let mut maximum = [f32::NEG_INFINITY; 3];
    let mut entity_count = 0;
    let mut vertex_count = 0;

    for (entity, assignment, world_transform, transform) in world
        .query::<(
            Entity,
            &Model3d,
            Option<&WorldTransform3d>,
            Option<&Transform3d>,
        )>()
        .iter(world)
    {
        if !assignment.visible {
            continue;
        }
        let Some(matrix) = world_transform
            .map(|transform| transform.column_major())
            .or_else(|| transform.copied().map(transform_matrix))
        else {
            continue;
        };
        let Some(model) = models.get(assignment.model) else {
            continue;
        };
        if let Some(mesh) = assignment.mesh
            && mesh >= model.meshes().len()
        {
            return Err(SceneBoundsError3d::MissingMesh {
                entity,
                mesh,
                mesh_count: model.meshes().len(),
            });
        }

        let mut entity_has_geometry = false;
        for (mesh_index, mesh) in model.meshes().iter().enumerate() {
            if assignment
                .mesh
                .is_some_and(|selected| selected != mesh_index)
            {
                continue;
            }
            for (primitive_index, primitive) in mesh.primitives().iter().enumerate() {
                for index in primitive.indices() {
                    let vertex = usize::try_from(*index).map_err(|_| {
                        SceneBoundsError3d::VertexIndexNotRepresentable {
                            entity,
                            index: *index,
                        }
                    })?;
                    let position = primitive.positions()[vertex];
                    if !position.iter().all(|value| value.is_finite()) {
                        return Err(SceneBoundsError3d::NonFiniteVertex {
                            entity,
                            mesh: mesh_index,
                            primitive: primitive_index,
                            vertex,
                        });
                    }
                    let point = transform_scene_bounds_point(matrix, position);
                    if !point.iter().all(|value| value.is_finite()) {
                        return Err(SceneBoundsError3d::NonFiniteWorldPoint { entity });
                    }
                    for axis in 0..3 {
                        minimum[axis] = minimum[axis].min(point[axis]);
                        maximum[axis] = maximum[axis].max(point[axis]);
                    }
                    vertex_count += 1;
                    entity_has_geometry = true;
                }
            }
        }
        entity_count += usize::from(entity_has_geometry);
    }

    if entity_count == 0 {
        return Ok(SceneBoundsResult3d::Empty);
    }
    Ok(SceneBoundsResult3d::Bounds(SceneBounds3d {
        minimum,
        maximum,
        entity_count,
        vertex_count,
    }))
}

fn transform_scene_bounds_point(matrix: [f32; 16], point: [f32; 3]) -> [f32; 3] {
    [
        matrix[0] * point[0] + matrix[4] * point[1] + matrix[8] * point[2] + matrix[12],
        matrix[1] * point[0] + matrix[5] * point[1] + matrix[9] * point[2] + matrix[13],
        matrix[2] * point[0] + matrix[6] * point[1] + matrix[10] * point[2] + matrix[14],
    ]
}

/// One renderer-neutral 3D model draw extracted from ECS state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModelDraw {
    /// Typed handle to the selected model asset.
    pub model: ModelHandle,
    /// Optional source mesh selection copied from [`Model3d`].
    pub mesh: Option<usize>,
    /// Exact column-major model-to-world matrix at the extraction boundary.
    ///
    /// This is the renderer contract. It preserves affine glTF hierarchy
    /// composition and must not be reconstructed from a lossy TRS view.
    pub model_matrix: [f32; 16],
    /// Explicit ordering value copied from [`Model3d`].
    pub render_order: i32,
    /// Copied from [`Model3d::overlay`].
    pub overlay: bool,
}

/// An adjacent ordered group referring to one model asset.
///
/// Do not coalesce non-adjacent batches without defining a render phase that
/// permits reordering. A transparent rendering path needs this exact order.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelDrawBatch {
    model: ModelHandle,
    draws: Vec<ModelDraw>,
}

impl ModelDrawBatch {
    /// Returns the model used by every draw in this batch.
    #[must_use]
    pub const fn model(&self) -> ModelHandle {
        self.model
    }

    /// Returns draws in deterministic extraction order.
    #[must_use]
    pub fn draws(&self) -> &[ModelDraw] {
        &self.draws
    }

    /// Returns the number of draws in this batch.
    #[must_use]
    pub fn len(&self) -> usize {
        self.draws.len()
    }

    /// Returns whether the batch contains no draws.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.draws.is_empty()
    }
}

/// A renderer-neutral 3D scene snapshot extracted from an ECS world.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExtractedModels {
    batches: Vec<ModelDrawBatch>,
    model_count: usize,
}

impl ExtractedModels {
    /// Returns model batches in global deterministic order.
    #[must_use]
    pub fn batches(&self) -> &[ModelDrawBatch] {
        &self.batches
    }

    /// Returns how many visible entities were extracted.
    #[must_use]
    pub const fn model_count(&self) -> usize {
        self.model_count
    }

    /// Returns whether no visible model entities were extracted.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.model_count == 0
    }

    /// Splits ordinary scene draws from depth-cleared overlay draws.
    #[must_use]
    pub fn partition_overlay(self) -> (Self, Self) {
        let mut scene = Vec::new();
        let mut overlay = Vec::new();
        for batch in self.batches {
            for draw in batch.draws {
                if draw.overlay {
                    overlay.push(draw);
                } else {
                    scene.push(draw);
                }
            }
        }
        (
            Self::from_ordered_draws(scene),
            Self::from_ordered_draws(overlay),
        )
    }

    fn from_ordered_draws(draws: Vec<ModelDraw>) -> Self {
        let model_count = draws.len();
        let mut batches: Vec<ModelDrawBatch> = Vec::new();
        for draw in draws {
            match batches.last_mut() {
                Some(batch) if batch.model == draw.model => batch.draws.push(draw),
                _ => batches.push(ModelDrawBatch {
                    model: draw.model,
                    draws: vec![draw],
                }),
            }
        }
        Self {
            batches,
            model_count,
        }
    }
}

/// A normalized world-space plane used by [`Frustum3d`].
///
/// Points whose signed distance is greater than or equal to zero are on the
/// inside of the plane. Construction normalizes the plane, so distance tests
/// and projected bound radii use the same units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Plane3d {
    normal: [f32; 3],
    distance: f32,
}

impl Plane3d {
    /// Creates and normalizes a plane represented by `normal · point + distance`.
    ///
    /// # Errors
    ///
    /// Returns [`Plane3dError`] for non-finite coefficients or a zero-length
    /// normal.
    pub fn new(normal: [f32; 3], distance: f32) -> Result<Self, Plane3dError> {
        if !all_finite(&normal) || !distance.is_finite() {
            return Err(Plane3dError::NonFinite);
        }
        let length = length3(normal);
        if length == 0.0 {
            return Err(Plane3dError::ZeroNormal);
        }
        if !length.is_finite() {
            return Err(Plane3dError::NonFinite);
        }
        let inverse_length = length.recip();
        Ok(Self {
            normal: scale3(normal, inverse_length),
            distance: distance * inverse_length,
        })
    }

    /// Returns the normalized inward-facing plane normal.
    #[must_use]
    pub const fn normal(self) -> [f32; 3] {
        self.normal
    }

    /// Returns the normalized constant in `normal · point + distance`.
    #[must_use]
    pub const fn distance(self) -> f32 {
        self.distance
    }

    /// Returns the signed distance from `point` to the plane.
    #[must_use]
    pub fn signed_distance(self, point: [f32; 3]) -> f32 {
        dot3(self.normal, point) + self.distance
    }
}

/// Validation failure while constructing a [`Plane3d`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Plane3dError {
    /// At least one coefficient is NaN or infinite.
    NonFinite,
    /// The normal has zero length and does not define a half-space.
    ZeroNormal,
}

impl fmt::Display for Plane3dError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFinite => formatter.write_str("plane coefficients must be finite"),
            Self::ZeroNormal => formatter.write_str("plane normal must have non-zero length"),
        }
    }
}

impl Error for Plane3dError {}

/// Names a plane when reporting clip-matrix construction failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrustumPlane3d {
    /// Left side plane.
    Left,
    /// Right side plane.
    Right,
    /// Bottom side plane.
    Bottom,
    /// Top side plane.
    Top,
    /// Near depth plane.
    Near,
    /// Far depth plane.
    Far,
}

/// Clip-space depth convention used to extract a [`Frustum3d`] from a matrix.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClipDepthRange3d {
    /// Near and far clip depths are `-w` and `w`, as in OpenGL.
    NegativeOneToOne,
    /// Near and far clip depths are `0` and `w`, as in WGPU/Direct3D/Vulkan.
    ZeroToOne,
}

/// Six inward-facing planes defining a convex camera frustum.
///
/// The type is renderer-neutral. A backend may construct it directly from
/// validated planes, or use [`Frustum3d::from_clip_matrix`] with an explicit
/// depth convention. The matrix is column-major and transforms world points
/// into clip space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frustum3d {
    planes: [Plane3d; 6],
}

impl Frustum3d {
    /// Creates a frustum from six inward-facing planes.
    ///
    /// Plane order has no effect on visibility; the conventional order used by
    /// [`Self::from_clip_matrix`] is left, right, bottom, top, near, far.
    #[must_use]
    pub const fn from_planes(planes: [Plane3d; 6]) -> Self {
        Self { planes }
    }

    /// Extracts inward-facing planes from a column-major world-to-clip matrix.
    ///
    /// # Errors
    ///
    /// Returns [`Frustum3dError`] if the matrix is non-finite or a derived
    /// plane is degenerate.
    pub fn from_clip_matrix(
        matrix: [f32; 16],
        depth_range: ClipDepthRange3d,
    ) -> Result<Self, Frustum3dError> {
        if !all_finite(&matrix) {
            return Err(Frustum3dError::NonFiniteMatrix);
        }
        let row_x = [matrix[0], matrix[4], matrix[8], matrix[12]];
        let row_y = [matrix[1], matrix[5], matrix[9], matrix[13]];
        let row_z = [matrix[2], matrix[6], matrix[10], matrix[14]];
        let row_w = [matrix[3], matrix[7], matrix[11], matrix[15]];
        let near = match depth_range {
            ClipDepthRange3d::NegativeOneToOne => add4(row_w, row_z),
            ClipDepthRange3d::ZeroToOne => row_z,
        };
        let coefficients = [
            (FrustumPlane3d::Left, add4(row_w, row_x)),
            (FrustumPlane3d::Right, subtract4(row_w, row_x)),
            (FrustumPlane3d::Bottom, add4(row_w, row_y)),
            (FrustumPlane3d::Top, subtract4(row_w, row_y)),
            (FrustumPlane3d::Near, near),
            (FrustumPlane3d::Far, subtract4(row_w, row_z)),
        ];
        let mut planes = [Plane3d {
            normal: [1.0, 0.0, 0.0],
            distance: 0.0,
        }; 6];
        for (index, (kind, plane)) in coefficients.into_iter().enumerate() {
            planes[index] =
                Plane3d::new([plane[0], plane[1], plane[2]], plane[3]).map_err(|source| {
                    Frustum3dError::InvalidPlane {
                        plane: kind,
                        source,
                    }
                })?;
        }
        Ok(Self { planes })
    }

    /// Returns all inward-facing planes.
    #[must_use]
    pub const fn planes(&self) -> &[Plane3d; 6] {
        &self.planes
    }

    /// Tests local bounds transformed by a column-major affine model matrix.
    ///
    /// Sphere and AABB support radii are projected directly onto each frustum
    /// plane. This remains conservative under rotation, non-uniform scale and
    /// shear without expanding a sphere by an unnecessarily large global
    /// scale factor.
    ///
    /// # Errors
    ///
    /// Returns [`FrustumIntersectionError3d`] when the transform or projected
    /// world-space bounds are non-finite.
    pub fn intersects_local_bounds(
        &self,
        bounds: LocalBounds3d,
        model_matrix: [f32; 16],
    ) -> Result<bool, FrustumIntersectionError3d> {
        if !all_finite(&model_matrix) {
            return Err(FrustumIntersectionError3d::NonFiniteModelMatrix);
        }
        let (centre, extents) = match bounds {
            LocalBounds3d::Sphere(sphere) => {
                (sphere.centre, ProjectedExtents::Sphere(sphere.radius))
            }
            LocalBounds3d::Aabb(aabb) => {
                (aabb.centre(), ProjectedExtents::Aabb(aabb.half_extents()))
            }
        };
        let world_centre = transform_scene_bounds_point(model_matrix, centre);
        if !all_finite(&world_centre) {
            return Err(FrustumIntersectionError3d::NonFiniteProjectedBounds);
        }
        for plane in self.planes {
            let normal = plane.normal;
            let local_plane_normal = [
                model_matrix[0] * normal[0]
                    + model_matrix[1] * normal[1]
                    + model_matrix[2] * normal[2],
                model_matrix[4] * normal[0]
                    + model_matrix[5] * normal[1]
                    + model_matrix[6] * normal[2],
                model_matrix[8] * normal[0]
                    + model_matrix[9] * normal[1]
                    + model_matrix[10] * normal[2],
            ];
            let projected_radius = match extents {
                ProjectedExtents::Sphere(radius) => radius * length3(local_plane_normal),
                ProjectedExtents::Aabb(half_extents) => {
                    dot3(half_extents, local_plane_normal.map(f32::abs))
                }
            };
            let distance = plane.signed_distance(world_centre);
            if !projected_radius.is_finite() || !distance.is_finite() {
                return Err(FrustumIntersectionError3d::NonFiniteProjectedBounds);
            }
            if distance < -projected_radius {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

fn add4(left: [f32; 4], right: [f32; 4]) -> [f32; 4] {
    [
        left[0] + right[0],
        left[1] + right[1],
        left[2] + right[2],
        left[3] + right[3],
    ]
}

fn subtract4(left: [f32; 4], right: [f32; 4]) -> [f32; 4] {
    [
        left[0] - right[0],
        left[1] - right[1],
        left[2] - right[2],
        left[3] - right[3],
    ]
}

/// Failure while deriving frustum planes from a clip matrix.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Frustum3dError {
    /// At least one matrix coefficient is NaN or infinite.
    NonFiniteMatrix,
    /// One matrix half-space does not define a valid plane.
    InvalidPlane {
        /// Semantic plane that failed validation.
        plane: FrustumPlane3d,
        /// Underlying plane validation failure.
        source: Plane3dError,
    },
}

impl fmt::Display for Frustum3dError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFiniteMatrix => formatter.write_str("clip matrix must be finite"),
            Self::InvalidPlane { plane, source } => {
                write!(formatter, "invalid {plane:?} frustum plane: {source}")
            }
        }
    }
}

impl Error for Frustum3dError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidPlane { source, .. } => Some(source),
            Self::NonFiniteMatrix => None,
        }
    }
}

/// A validated bounding sphere in model-local coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LocalBoundingSphere3d {
    centre: [f32; 3],
    radius: f32,
}

impl LocalBoundingSphere3d {
    /// Creates local sphere bounds. A zero radius is a valid point bound.
    ///
    /// # Errors
    ///
    /// Returns [`LocalBounds3dError`] for non-finite values or a negative radius.
    pub fn new(centre: [f32; 3], radius: f32) -> Result<Self, LocalBounds3dError> {
        if !all_finite(&centre) || !radius.is_finite() {
            return Err(LocalBounds3dError::NonFinite);
        }
        if radius < 0.0 {
            return Err(LocalBounds3dError::NegativeRadius);
        }
        Ok(Self { centre, radius })
    }

    /// Returns the local-space centre.
    #[must_use]
    pub const fn centre(self) -> [f32; 3] {
        self.centre
    }

    /// Returns the local-space radius.
    #[must_use]
    pub const fn radius(self) -> f32 {
        self.radius
    }
}

/// A validated axis-aligned box in model-local coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LocalAabb3d {
    minimum: [f32; 3],
    maximum: [f32; 3],
}

impl LocalAabb3d {
    /// Creates local AABB bounds. Zero-sized axes are valid.
    ///
    /// # Errors
    ///
    /// Returns [`LocalBounds3dError`] for non-finite values or an axis whose
    /// minimum is greater than its maximum.
    pub fn new(minimum: [f32; 3], maximum: [f32; 3]) -> Result<Self, LocalBounds3dError> {
        if !all_finite(&minimum) || !all_finite(&maximum) {
            return Err(LocalBounds3dError::NonFinite);
        }
        for axis in 0..3 {
            if minimum[axis] > maximum[axis] {
                return Err(LocalBounds3dError::InvertedAxis { axis });
            }
        }
        Ok(Self { minimum, maximum })
    }

    /// Returns the local minimum corner.
    #[must_use]
    pub const fn minimum(self) -> [f32; 3] {
        self.minimum
    }

    /// Returns the local maximum corner.
    #[must_use]
    pub const fn maximum(self) -> [f32; 3] {
        self.maximum
    }

    /// Returns the local box centre.
    #[must_use]
    pub fn centre(self) -> [f32; 3] {
        [
            self.minimum[0].midpoint(self.maximum[0]),
            self.minimum[1].midpoint(self.maximum[1]),
            self.minimum[2].midpoint(self.maximum[2]),
        ]
    }

    /// Returns local half-extents.
    #[must_use]
    pub fn half_extents(self) -> [f32; 3] {
        let centre = self.centre();
        [
            self.maximum[0] - centre[0],
            self.maximum[1] - centre[1],
            self.maximum[2] - centre[2],
        ]
    }
}

/// Valid model-local geometry accepted by frustum filtering.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LocalBounds3d {
    /// Bounding sphere, projected exactly for every frustum plane.
    Sphere(LocalBoundingSphere3d),
    /// Axis-aligned local box, transformed as an oriented parallelepiped.
    Aabb(LocalAabb3d),
}

impl From<LocalBoundingSphere3d> for LocalBounds3d {
    fn from(value: LocalBoundingSphere3d) -> Self {
        Self::Sphere(value)
    }
}

impl From<LocalAabb3d> for LocalBounds3d {
    fn from(value: LocalAabb3d) -> Self {
        Self::Aabb(value)
    }
}

#[derive(Clone, Copy)]
enum ProjectedExtents {
    Sphere(f32),
    Aabb([f32; 3]),
}

/// Validation failure for model-local bounds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalBounds3dError {
    /// At least one coordinate or the radius is NaN or infinite.
    NonFinite,
    /// A sphere radius is less than zero.
    NegativeRadius,
    /// An AABB minimum is greater than its maximum on this zero-based axis.
    InvertedAxis {
        /// Zero-based X/Y/Z axis index.
        axis: usize,
    },
}

impl fmt::Display for LocalBounds3dError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFinite => formatter.write_str("local bounds must be finite"),
            Self::NegativeRadius => {
                formatter.write_str("bounding sphere radius cannot be negative")
            }
            Self::InvertedAxis { axis } => {
                write!(formatter, "AABB minimum exceeds maximum on axis {axis}")
            }
        }
    }
}

impl Error for LocalBounds3dError {}

/// Typed lookup key for model-wide or mesh-specific local bounds.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ModelBoundsKey3d {
    /// Model asset whose authoring coordinates define the bounds.
    pub model: ModelHandle,
    /// Selected mesh, or `None` for bounds covering the complete model.
    pub mesh: Option<usize>,
}

impl ModelBoundsKey3d {
    /// Creates a complete-model bounds key.
    #[must_use]
    pub const fn model(model: ModelHandle) -> Self {
        Self { model, mesh: None }
    }

    /// Creates a mesh-specific bounds key.
    #[must_use]
    pub const fn mesh(model: ModelHandle, mesh: usize) -> Self {
        Self {
            model,
            mesh: Some(mesh),
        }
    }
}

/// Caller-owned local bounds with mesh-to-model fallback.
///
/// An exact mesh entry wins. When it is absent, [`Self::get`] falls back to
/// complete-model bounds for the same handle. This is conservative and lets an
/// importer publish useful model bounds before optional per-mesh metadata is
/// cooked.
#[derive(Clone, Debug, Default)]
pub struct ModelBoundsRegistry3d {
    entries: HashMap<ModelBoundsKey3d, LocalBounds3d>,
}

impl ModelBoundsRegistry3d {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts or replaces complete-model bounds.
    pub fn insert_model(
        &mut self,
        model: ModelHandle,
        bounds: impl Into<LocalBounds3d>,
    ) -> Option<LocalBounds3d> {
        self.entries
            .insert(ModelBoundsKey3d::model(model), bounds.into())
    }

    /// Inserts or replaces bounds for one source mesh.
    pub fn insert_mesh(
        &mut self,
        model: ModelHandle,
        mesh: usize,
        bounds: impl Into<LocalBounds3d>,
    ) -> Option<LocalBounds3d> {
        self.entries
            .insert(ModelBoundsKey3d::mesh(model, mesh), bounds.into())
    }

    /// Returns exact mesh bounds, falling back to complete-model bounds.
    #[must_use]
    pub fn get(&self, key: ModelBoundsKey3d) -> Option<LocalBounds3d> {
        self.entries.get(&key).copied().or_else(|| {
            key.mesh.and_then(|_| {
                self.entries
                    .get(&ModelBoundsKey3d::model(key.model))
                    .copied()
            })
        })
    }

    /// Returns only bounds authored for this exact model/mesh key.
    ///
    /// Unlike [`Self::get`], a mesh key does not fall back to complete-model
    /// bounds. This is useful for visibility telemetry that must distinguish
    /// precise per-mesh culling from a conservative model-wide fallback.
    #[must_use]
    pub fn get_exact(&self, key: ModelBoundsKey3d) -> Option<LocalBounds3d> {
        self.entries.get(&key).copied()
    }

    /// Returns the number of authored entries, excluding implicit fallbacks.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns whether the registry has no authored entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns whether any complete-model or mesh entry belongs to `model`.
    #[must_use]
    pub fn contains_model(&self, model: ModelHandle) -> bool {
        self.entries.keys().any(|key| key.model == model)
    }

    /// Removes every complete-model and mesh entry for `model`.
    pub fn remove_model(&mut self, model: ModelHandle) -> usize {
        let before = self.entries.len();
        self.entries.retain(|key, _| key.model != model);
        before.saturating_sub(self.entries.len())
    }

    /// Merges validated entries, replacing identical model/mesh keys.
    pub fn extend(&mut self, other: Self) {
        self.entries.extend(other.entries);
    }
}

/// Validated local AABBs derived from one CPU [`Model`] asset.
///
/// Bounds are computed from every authored position, including positions not
/// referenced by the index stream. This is deliberately conservative: stale or
/// unusual source indices cannot make culling discard geometry covered by the
/// position accessor.
#[derive(Clone, Debug, PartialEq)]
pub struct ComputedModelBounds3d {
    model: LocalAabb3d,
    meshes: Vec<LocalAabb3d>,
    position_count: usize,
}

impl ComputedModelBounds3d {
    /// Returns bounds covering all meshes in the model.
    #[must_use]
    pub const fn model(&self) -> LocalAabb3d {
        self.model
    }

    /// Returns mesh-local bounds in source mesh index order.
    #[must_use]
    pub fn meshes(&self) -> &[LocalAabb3d] {
        &self.meshes
    }

    /// Returns the number of source positions examined.
    #[must_use]
    pub const fn position_count(&self) -> usize {
        self.position_count
    }
}

/// CPU bounds generation failure for one typed model asset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComputeModelBoundsError3d {
    /// The handle is stale, not resident or belongs to no slot in `models`.
    MissingModel {
        /// Requested typed model handle.
        model: ModelHandle,
    },
    /// The model contains no mesh geometry.
    EmptyModel {
        /// Empty model asset.
        model: ModelHandle,
    },
    /// One source mesh contains no position geometry.
    EmptyMesh {
        /// Model containing the empty mesh.
        model: ModelHandle,
        /// Zero-based source mesh index.
        mesh: usize,
    },
    /// A source position contains NaN or infinity.
    NonFinitePosition {
        /// Model containing the invalid position.
        model: ModelHandle,
        /// Zero-based source mesh index.
        mesh: usize,
        /// Zero-based primitive index within the mesh.
        primitive: usize,
        /// Zero-based position index within the primitive.
        position: usize,
    },
}

impl fmt::Display for ComputeModelBoundsError3d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingModel { model } => {
                write!(
                    formatter,
                    "model asset is missing or not resident: {model:?}"
                )
            }
            Self::EmptyModel { model } => {
                write!(formatter, "model asset has no mesh geometry: {model:?}")
            }
            Self::EmptyMesh { model, mesh } => {
                write!(
                    formatter,
                    "model asset {model:?} has no positions in mesh {mesh}"
                )
            }
            Self::NonFinitePosition {
                model,
                mesh,
                primitive,
                position,
            } => write!(
                formatter,
                "model asset {model:?} has a non-finite position at mesh {mesh}, primitive {primitive}, position {position}"
            ),
        }
    }
}

impl Error for ComputeModelBoundsError3d {}

/// Computes complete-model and per-mesh local AABBs from a resident CPU model.
///
/// The operation only reads [`Assets`] and allocates one bounds entry per mesh.
/// It does not inspect GPU residency and does not mutate a bounds registry.
/// Use [`register_computed_model_bounds_3d`] for the transactional convenience
/// path that also publishes the result.
///
/// # Errors
///
/// Returns [`ComputeModelBoundsError3d`] when the handle is unavailable, the
/// model or one of its meshes has no positions, or a position is non-finite.
pub fn compute_model_bounds_3d(
    models: &Assets<Model>,
    model: ModelHandle,
) -> Result<ComputedModelBounds3d, ComputeModelBoundsError3d> {
    let source = models
        .get(model)
        .ok_or(ComputeModelBoundsError3d::MissingModel { model })?;
    if source.meshes().is_empty() {
        return Err(ComputeModelBoundsError3d::EmptyModel { model });
    }

    let mut model_minimum = [f32::INFINITY; 3];
    let mut model_maximum = [f32::NEG_INFINITY; 3];
    let mut mesh_bounds = Vec::with_capacity(source.meshes().len());
    let mut position_count = 0;
    for (mesh_index, mesh) in source.meshes().iter().enumerate() {
        let mut mesh_minimum = [f32::INFINITY; 3];
        let mut mesh_maximum = [f32::NEG_INFINITY; 3];
        let mut mesh_position_count = 0;
        for (primitive_index, primitive) in mesh.primitives().iter().enumerate() {
            for (position_index, position) in primitive.positions().iter().copied().enumerate() {
                if !all_finite(&position) {
                    return Err(ComputeModelBoundsError3d::NonFinitePosition {
                        model,
                        mesh: mesh_index,
                        primitive: primitive_index,
                        position: position_index,
                    });
                }
                for axis in 0..3 {
                    mesh_minimum[axis] = mesh_minimum[axis].min(position[axis]);
                    mesh_maximum[axis] = mesh_maximum[axis].max(position[axis]);
                }
                mesh_position_count += 1;
            }
        }
        if mesh_position_count == 0 {
            return Err(ComputeModelBoundsError3d::EmptyMesh {
                model,
                mesh: mesh_index,
            });
        }
        for axis in 0..3 {
            model_minimum[axis] = model_minimum[axis].min(mesh_minimum[axis]);
            model_maximum[axis] = model_maximum[axis].max(mesh_maximum[axis]);
        }
        mesh_bounds.push(LocalAabb3d {
            minimum: mesh_minimum,
            maximum: mesh_maximum,
        });
        position_count += mesh_position_count;
    }

    Ok(ComputedModelBounds3d {
        model: LocalAabb3d {
            minimum: model_minimum,
            maximum: model_maximum,
        },
        meshes: mesh_bounds,
        position_count,
    })
}

/// Computes and atomically replaces registry entries for one resident model.
///
/// Existing entries for `model` are left untouched if computation fails. On
/// success, stale mesh entries are removed before complete-model and current
/// per-mesh bounds are inserted.
///
/// # Errors
///
/// Returns [`ComputeModelBoundsError3d`] under the same conditions as
/// [`compute_model_bounds_3d`].
pub fn register_computed_model_bounds_3d(
    registry: &mut ModelBoundsRegistry3d,
    models: &Assets<Model>,
    model: ModelHandle,
) -> Result<ComputedModelBounds3d, ComputeModelBoundsError3d> {
    let computed = compute_model_bounds_3d(models, model)?;
    registry.entries.retain(|key, _| key.model != model);
    registry.insert_model(model, computed.model());
    for (mesh, bounds) in computed.meshes().iter().copied().enumerate() {
        registry.insert_mesh(model, mesh, bounds);
    }
    Ok(computed)
}

/// Failure while projecting validated local bounds into world space.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrustumIntersectionError3d {
    /// A model matrix coefficient is NaN or infinite.
    NonFiniteModelMatrix,
    /// Finite inputs overflowed while projecting world-space bounds.
    NonFiniteProjectedBounds,
}

impl fmt::Display for FrustumIntersectionError3d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFiniteModelMatrix => formatter.write_str("model matrix must be finite"),
            Self::NonFiniteProjectedBounds => {
                formatter.write_str("projected world-space bounds are non-finite")
            }
        }
    }
}

impl Error for FrustumIntersectionError3d {}

/// Deterministic visibility counters for one filtered snapshot.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FrustumCullingStats3d {
    /// Draws in the input [`ExtractedModels`] snapshot.
    pub input_draws: usize,
    /// Draws tested using bounds.
    pub tested_draws: usize,
    /// Tested draws rejected by at least one plane.
    pub culled_draws: usize,
    /// Draws kept in the output, including conservative unbounded draws.
    pub visible_draws: usize,
    /// Draws for which the caller supplied no bounds; these are always kept.
    pub unbounded_draws: usize,
}

/// Owned visible snapshot and telemetry produced by frustum filtering.
#[derive(Clone, Debug, PartialEq)]
pub struct FrustumCullingResult3d {
    visible: ExtractedModels,
    stats: FrustumCullingStats3d,
}

impl FrustumCullingResult3d {
    /// Returns the renderer-ready visible snapshot.
    #[must_use]
    pub const fn visible(&self) -> &ExtractedModels {
        &self.visible
    }

    /// Consumes the result and returns the renderer-ready snapshot.
    #[must_use]
    pub fn into_visible(self) -> ExtractedModels {
        self.visible
    }

    /// Returns counters for visibility diagnostics and tuning.
    #[must_use]
    pub const fn stats(&self) -> FrustumCullingStats3d {
        self.stats
    }
}

/// Transactional failure while filtering one extracted draw.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrustumCullingError3d {
    /// Zero-based draw index in deterministic snapshot order.
    pub draw_index: usize,
    /// Model whose projected bounds failed.
    pub model: ModelHandle,
    /// Optional selected mesh copied from the draw.
    pub mesh: Option<usize>,
    /// Projection failure.
    pub source: FrustumIntersectionError3d,
}

impl fmt::Display for FrustumCullingError3d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "cannot test frustum visibility for draw {} ({:?}, mesh {:?}): {}",
            self.draw_index, self.model, self.mesh, self.source
        )
    }
}

impl Error for FrustumCullingError3d {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}

/// Filters a renderer-neutral snapshot using a standard bounds registry.
///
/// The input is never mutated. Missing bounds are kept conservatively and
/// counted in [`FrustumCullingStats3d::unbounded_draws`]. Draw order is
/// preserved exactly; adjacent visible draws using the same model may share a
/// rebuilt batch.
///
/// # Errors
///
/// Returns [`FrustumCullingError3d`] if a model matrix or projected bounds are
/// non-finite. No partial output is returned.
pub fn filter_extracted_models_by_frustum_3d(
    extracted: &ExtractedModels,
    frustum: &Frustum3d,
    bounds: &ModelBoundsRegistry3d,
) -> Result<FrustumCullingResult3d, FrustumCullingError3d> {
    filter_extracted_models_by_frustum_3d_with(extracted, frustum, |key| bounds.get(key))
}

/// Filters a renderer-neutral snapshot with a custom typed bounds lookup.
///
/// This is the low-level extension point for importer caches, streaming asset
/// tables or procedural geometry. The lookup is invoked once per draw in
/// deterministic input order. Returning `None` keeps that draw conservatively.
///
/// # Errors
///
/// Returns [`FrustumCullingError3d`] if a model matrix or projected bounds are
/// non-finite. No partial output is returned.
pub fn filter_extracted_models_by_frustum_3d_with(
    extracted: &ExtractedModels,
    frustum: &Frustum3d,
    mut bounds_for: impl FnMut(ModelBoundsKey3d) -> Option<LocalBounds3d>,
) -> Result<FrustumCullingResult3d, FrustumCullingError3d> {
    let mut batches: Vec<ModelDrawBatch> = Vec::new();
    let mut stats = FrustumCullingStats3d {
        input_draws: extracted.model_count,
        ..FrustumCullingStats3d::default()
    };
    let mut draw_index = 0;
    for batch in &extracted.batches {
        for draw in &batch.draws {
            let key = ModelBoundsKey3d {
                model: draw.model,
                mesh: draw.mesh,
            };
            let visible = if let Some(bounds) = bounds_for(key) {
                stats.tested_draws += 1;
                frustum
                    .intersects_local_bounds(bounds, draw.model_matrix)
                    .map_err(|source| FrustumCullingError3d {
                        draw_index,
                        model: draw.model,
                        mesh: draw.mesh,
                        source,
                    })?
            } else {
                stats.unbounded_draws += 1;
                true
            };
            if visible {
                stats.visible_draws += 1;
                match batches.last_mut() {
                    Some(output) if output.model == draw.model => output.draws.push(*draw),
                    _ => batches.push(ModelDrawBatch {
                        model: draw.model,
                        draws: vec![*draw],
                    }),
                }
            } else {
                stats.culled_draws += 1;
            }
            draw_index += 1;
        }
    }
    Ok(FrustumCullingResult3d {
        visible: ExtractedModels {
            batches,
            model_count: stats.visible_draws,
        },
        stats,
    })
}

/// Per-mesh visibility counters for one renderer-neutral snapshot.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MeshFrustumCullingStats3d {
    /// Source meshes represented by all input draws.
    pub input_meshes: usize,
    /// Meshes tested against either exact or fallback bounds.
    pub tested_meshes: usize,
    /// Meshes tested with an exact per-mesh bounds entry.
    pub exact_bound_meshes: usize,
    /// Meshes conservatively tested with complete-model bounds.
    pub model_fallback_meshes: usize,
    /// Meshes with no bounds, which are always retained.
    pub unbounded_meshes: usize,
    /// Meshes rejected by at least one frustum plane.
    pub culled_meshes: usize,
    /// Meshes retained in the output snapshot.
    pub visible_meshes: usize,
    /// Explicit visible mesh draws emitted from whole-model input draws.
    pub expanded_mesh_draws: usize,
}

/// Owned mesh-filtered snapshot with logical-draw and physical-mesh telemetry.
#[derive(Clone, Debug, PartialEq)]
pub struct MeshFrustumCullingResult3d {
    visible: ExtractedModels,
    draws: FrustumCullingStats3d,
    meshes: MeshFrustumCullingStats3d,
}

impl MeshFrustumCullingResult3d {
    /// Returns the renderer-ready snapshot with explicit mesh selections.
    #[must_use]
    pub const fn visible(&self) -> &ExtractedModels {
        &self.visible
    }

    /// Consumes the result and returns the renderer-ready snapshot.
    #[must_use]
    pub fn into_visible(self) -> ExtractedModels {
        self.visible
    }

    /// Returns logical input-draw visibility counters.
    #[must_use]
    pub const fn draw_stats(&self) -> FrustumCullingStats3d {
        self.draws
    }

    /// Returns physical source-mesh visibility counters.
    #[must_use]
    pub const fn mesh_stats(&self) -> MeshFrustumCullingStats3d {
        self.meshes
    }

    /// Splits telemetry and the owned renderer snapshot in one move.
    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        ExtractedModels,
        FrustumCullingStats3d,
        MeshFrustumCullingStats3d,
    ) {
        (self.visible, self.draws, self.meshes)
    }
}

/// Failure while expanding and filtering extracted model meshes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MeshFrustumCullingError3d {
    /// An extracted handle is absent from the supplied CPU model store.
    MissingModel {
        /// Zero-based draw index in deterministic snapshot order.
        draw_index: usize,
        /// Missing typed model handle.
        model: ModelHandle,
    },
    /// A draw selected a source mesh outside its model.
    MissingMesh {
        /// Zero-based draw index in deterministic snapshot order.
        draw_index: usize,
        /// Model owning the invalid selection.
        model: ModelHandle,
        /// Selected mesh index.
        mesh: usize,
        /// Number of source meshes available in the model.
        mesh_count: usize,
    },
    /// Valid bounds could not be projected through a draw transform.
    Intersection(FrustumCullingError3d),
}

impl fmt::Display for MeshFrustumCullingError3d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingModel { draw_index, model } => write!(
                formatter,
                "mesh frustum draw {draw_index} references missing model {model:?}"
            ),
            Self::MissingMesh {
                draw_index,
                model,
                mesh,
                mesh_count,
            } => write!(
                formatter,
                "mesh frustum draw {draw_index} selects model {model:?} mesh {mesh}, but only {mesh_count} meshes exist"
            ),
            Self::Intersection(error) => error.fmt(formatter),
        }
    }
}

impl Error for MeshFrustumCullingError3d {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Intersection(error) => Some(error),
            Self::MissingModel { .. } | Self::MissingMesh { .. } => None,
        }
    }
}

/// Filters every physical source mesh represented by an extracted snapshot.
///
/// A draw with `mesh: Some(_)` remains one draw. A whole-model draw is expanded
/// into deterministic ascending mesh selections, allowing meshes outside the
/// camera to be omitted before backend submission. Exact per-mesh bounds are
/// preferred; complete-model bounds are a conservative fallback, and missing
/// bounds retain the mesh. Primitive and transparent ordering are unchanged
/// because expansion follows the original draw and source mesh order exactly.
///
/// # Errors
///
/// Returns [`MeshFrustumCullingError3d`] for missing model/mesh data or invalid
/// projected bounds. No partial output is returned.
#[allow(
    clippy::too_many_lines,
    reason = "mesh expansion, conservative fallback telemetry and transactional output share one ordered pass"
)]
pub fn filter_extracted_model_meshes_by_frustum_3d(
    extracted: &ExtractedModels,
    frustum: &Frustum3d,
    models: &Assets<Model>,
    bounds: &ModelBoundsRegistry3d,
) -> Result<MeshFrustumCullingResult3d, MeshFrustumCullingError3d> {
    let mut batches: Vec<ModelDrawBatch> = Vec::new();
    let mut draw_stats = FrustumCullingStats3d {
        input_draws: extracted.model_count,
        ..FrustumCullingStats3d::default()
    };
    let mut mesh_stats = MeshFrustumCullingStats3d::default();
    let mut draw_index = 0;
    for batch in &extracted.batches {
        let model = models
            .get(batch.model)
            .ok_or(MeshFrustumCullingError3d::MissingModel {
                draw_index,
                model: batch.model,
            })?;
        for draw in &batch.draws {
            if let Some(mesh) = draw.mesh
                && mesh >= model.meshes().len()
            {
                return Err(MeshFrustumCullingError3d::MissingMesh {
                    draw_index,
                    model: draw.model,
                    mesh,
                    mesh_count: model.meshes().len(),
                });
            }
            let selected_meshes = draw
                .mesh
                .map_or(0..model.meshes().len(), |mesh| mesh..mesh + 1);
            let mut draw_had_bounds = false;
            let mut draw_had_unbounded_mesh = false;
            let visible_before = mesh_stats.visible_meshes;
            for mesh in selected_meshes {
                mesh_stats.input_meshes += 1;
                let mesh_key = ModelBoundsKey3d::mesh(draw.model, mesh);
                let (local_bounds, exact) = match bounds.get_exact(mesh_key) {
                    Some(local_bounds) => (Some(local_bounds), true),
                    None => (bounds.get_exact(ModelBoundsKey3d::model(draw.model)), false),
                };
                let visible = if let Some(local_bounds) = local_bounds {
                    draw_had_bounds = true;
                    mesh_stats.tested_meshes += 1;
                    if exact {
                        mesh_stats.exact_bound_meshes += 1;
                    } else {
                        mesh_stats.model_fallback_meshes += 1;
                    }
                    frustum
                        .intersects_local_bounds(local_bounds, draw.model_matrix)
                        .map_err(|source| {
                            MeshFrustumCullingError3d::Intersection(FrustumCullingError3d {
                                draw_index,
                                model: draw.model,
                                mesh: Some(mesh),
                                source,
                            })
                        })?
                } else {
                    draw_had_unbounded_mesh = true;
                    mesh_stats.unbounded_meshes += 1;
                    true
                };
                if visible {
                    mesh_stats.visible_meshes += 1;
                    let output_draw = ModelDraw {
                        mesh: Some(mesh),
                        ..*draw
                    };
                    match batches.last_mut() {
                        Some(output) if output.model == output_draw.model => {
                            output.draws.push(output_draw);
                        }
                        _ => batches.push(ModelDrawBatch {
                            model: output_draw.model,
                            draws: vec![output_draw],
                        }),
                    }
                    if draw.mesh.is_none() {
                        mesh_stats.expanded_mesh_draws += 1;
                    }
                } else {
                    mesh_stats.culled_meshes += 1;
                }
            }
            if draw_had_bounds {
                draw_stats.tested_draws += 1;
            }
            if draw_had_unbounded_mesh {
                draw_stats.unbounded_draws += 1;
            }
            if mesh_stats.visible_meshes > visible_before {
                draw_stats.visible_draws += 1;
            } else {
                draw_stats.culled_draws += 1;
            }
            draw_index += 1;
        }
    }
    Ok(MeshFrustumCullingResult3d {
        visible: ExtractedModels {
            model_count: draw_stats.visible_draws,
            batches,
        },
        draws: draw_stats,
        meshes: mesh_stats,
    })
}

/// Expands extracted draws to physical meshes and keeps those whose local-bound
/// centre (world space) satisfies `keep`.
///
/// Same expansion rules as [`filter_extracted_model_meshes_by_frustum_3d`], but
/// the visibility predicate is supplied by the caller (shadow coverage, light
/// volume, etc.). Missing bounds are retained, matching the frustum path.
///
/// # Errors
///
/// Returns [`MeshFrustumCullingError3d`] for missing model/mesh data or a
/// non-finite model matrix / projected centre.
pub fn filter_extracted_model_meshes_where_3d(
    extracted: &ExtractedModels,
    models: &Assets<Model>,
    bounds: &ModelBoundsRegistry3d,
    mut keep: impl FnMut([f32; 3]) -> bool,
) -> Result<MeshFrustumCullingResult3d, MeshFrustumCullingError3d> {
    let mut batches: Vec<ModelDrawBatch> = Vec::new();
    let mut draw_stats = FrustumCullingStats3d {
        input_draws: extracted.model_count,
        ..FrustumCullingStats3d::default()
    };
    let mut mesh_stats = MeshFrustumCullingStats3d::default();
    let mut draw_index = 0;
    for batch in &extracted.batches {
        let model = models
            .get(batch.model)
            .ok_or(MeshFrustumCullingError3d::MissingModel {
                draw_index,
                model: batch.model,
            })?;
        for draw in &batch.draws {
            if let Some(mesh) = draw.mesh
                && mesh >= model.meshes().len()
            {
                return Err(MeshFrustumCullingError3d::MissingMesh {
                    draw_index,
                    model: draw.model,
                    mesh,
                    mesh_count: model.meshes().len(),
                });
            }
            if !all_finite(&draw.model_matrix) {
                return Err(MeshFrustumCullingError3d::Intersection(FrustumCullingError3d {
                    draw_index,
                    model: draw.model,
                    mesh: draw.mesh,
                    source: FrustumIntersectionError3d::NonFiniteModelMatrix,
                }));
            }
            let selected_meshes = draw
                .mesh
                .map_or(0..model.meshes().len(), |mesh| mesh..mesh + 1);
            let mut draw_had_bounds = false;
            let mut draw_had_unbounded_mesh = false;
            let visible_before = mesh_stats.visible_meshes;
            for mesh in selected_meshes {
                mesh_stats.input_meshes += 1;
                let mesh_key = ModelBoundsKey3d::mesh(draw.model, mesh);
                let (local_bounds, exact) = match bounds.get_exact(mesh_key) {
                    Some(local_bounds) => (Some(local_bounds), true),
                    None => (bounds.get_exact(ModelBoundsKey3d::model(draw.model)), false),
                };
                let visible = if let Some(local_bounds) = local_bounds {
                    draw_had_bounds = true;
                    mesh_stats.tested_meshes += 1;
                    if exact {
                        mesh_stats.exact_bound_meshes += 1;
                    } else {
                        mesh_stats.model_fallback_meshes += 1;
                    }
                    let centre = match local_bounds {
                        LocalBounds3d::Sphere(sphere) => sphere.centre(),
                        LocalBounds3d::Aabb(aabb) => aabb.centre(),
                    };
                    let world_centre = transform_scene_bounds_point(draw.model_matrix, centre);
                    if !all_finite(&world_centre) {
                        return Err(MeshFrustumCullingError3d::Intersection(
                            FrustumCullingError3d {
                                draw_index,
                                model: draw.model,
                                mesh: Some(mesh),
                                source: FrustumIntersectionError3d::NonFiniteProjectedBounds,
                            },
                        ));
                    }
                    keep(world_centre)
                } else {
                    draw_had_unbounded_mesh = true;
                    mesh_stats.unbounded_meshes += 1;
                    true
                };
                if visible {
                    mesh_stats.visible_meshes += 1;
                    let output_draw = ModelDraw {
                        mesh: Some(mesh),
                        ..*draw
                    };
                    match batches.last_mut() {
                        Some(output) if output.model == output_draw.model => {
                            output.draws.push(output_draw);
                        }
                        _ => batches.push(ModelDrawBatch {
                            model: output_draw.model,
                            draws: vec![output_draw],
                        }),
                    }
                    if draw.mesh.is_none() {
                        mesh_stats.expanded_mesh_draws += 1;
                    }
                } else {
                    mesh_stats.culled_meshes += 1;
                }
            }
            if draw_had_bounds {
                draw_stats.tested_draws += 1;
            }
            if draw_had_unbounded_mesh {
                draw_stats.unbounded_draws += 1;
            }
            if mesh_stats.visible_meshes > visible_before {
                draw_stats.visible_draws += 1;
            } else {
                draw_stats.culled_draws += 1;
            }
            draw_index += 1;
        }
    }
    Ok(MeshFrustumCullingResult3d {
        visible: ExtractedModels {
            model_count: draw_stats.visible_draws,
            batches,
        },
        draws: draw_stats,
        meshes: mesh_stats,
    })
}

/// Extracts visible [`Model3d`] transforms from `world`.
///
/// A resolved [`WorldTransform3d`] takes precedence, so imported matrix nodes
/// reach the renderer exactly. Standalone gameplay entities can still use the
/// legacy [`Transform3d`]. Entities without either transform are skipped.
/// [`RenderFlags3d::draw`] false forces a skip even when [`Model3d::visible`].
#[must_use]
pub fn extract_models(world: &mut World) -> ExtractedModels {
    let mut extracted: Vec<(u64, ModelDraw)> = world
        .query::<(
            Entity,
            &Model3d,
            Option<&RenderFlags3d>,
            Option<&WorldTransform3d>,
            Option<&Transform3d>,
        )>()
        .iter(world)
        .filter_map(|(entity, model, render_flags, world_transform, transform)| {
            if !model_draw_enabled(model, render_flags) {
                return None;
            }
            let model_matrix = world_transform
                .map(|transform| transform.column_major())
                .or_else(|| transform.map(|transform| transform_matrix(*transform)))?;
            Some((
                entity.to_bits(),
                ModelDraw {
                    model: model.model,
                    mesh: model.mesh,
                    model_matrix,
                    render_order: model.render_order,
                    overlay: model.overlay,
                },
            ))
        })
        .collect();

    extracted.sort_by_key(|(entity_bits, draw)| (draw.render_order, *entity_bits));
    let model_count = extracted.len();
    let mut batches: Vec<ModelDrawBatch> = Vec::new();

    for (_, draw) in extracted {
        match batches.last_mut() {
            Some(batch) if batch.model == draw.model => batch.draws.push(draw),
            _ => batches.push(ModelDrawBatch {
                model: draw.model,
                draws: vec![draw],
            }),
        }
    }

    ExtractedModels {
        batches,
        model_count,
    }
}

/// Extracts models for **static player-mesh collision**, independent of nodraw.
///
/// Includes invisible / [`RenderFlags3d::NODRAW`] entities when
/// [`CollisionFlags3d`] allows player mesh contribution (or the component is
/// absent → solid by default). Skips entities that fail the filter.
#[must_use]
pub fn extract_models_for_static_collision(world: &mut World) -> ExtractedModels {
    let mut extracted: Vec<(u64, ModelDraw)> = world
        .query::<(
            Entity,
            &Model3d,
            Option<&CollisionFlags3d>,
            Option<&WorldTransform3d>,
            Option<&Transform3d>,
        )>()
        .iter(world)
        .filter_map(|(entity, model, collision, world_transform, transform)| {
            if let Some(flags) = collision
                && !flags.contributes_to_player_mesh()
            {
                return None;
            }
            let model_matrix = world_transform
                .map(|transform| transform.column_major())
                .or_else(|| transform.map(|transform| transform_matrix(*transform)))?;
            Some((
                entity.to_bits(),
                ModelDraw {
                    model: model.model,
                    mesh: model.mesh,
                    model_matrix,
                    render_order: model.render_order,
                    overlay: model.overlay,
                },
            ))
        })
        .collect();

    extracted.sort_by_key(|(entity_bits, draw)| (draw.render_order, *entity_bits));
    let model_count = extracted.len();
    let mut batches: Vec<ModelDrawBatch> = Vec::new();
    for (_, draw) in extracted {
        match batches.last_mut() {
            Some(batch) if batch.model == draw.model => batch.draws.push(draw),
            _ => batches.push(ModelDrawBatch {
                model: draw.model,
                draws: vec![draw],
            }),
        }
    }
    ExtractedModels {
        batches,
        model_count,
    }
}

/// Неподвижная коллизия, собранная из видимой 3D-сцены.
///
/// Это готовый результат [`build_static_scene_collider_3d`]. Все вершины уже
/// переведены из координат моделей в мировые координаты, поэтому персонаж или
/// другой kinematic controller может сразу передать [`mesh`](Self::mesh) в
/// [`TriangleMesh3d::resolve_sphere`]. Коллайдер намеренно не наблюдает за ECS
/// миром: после перемещения уровня его нужно собрать заново в понятной точке
/// смены сцены, а не оставлять устаревшие стены незаметно активными.
#[derive(Clone, Debug, PartialEq)]
pub struct StaticSceneCollider3d {
    mesh: TriangleMesh3d,
    source_draw_count: usize,
    triangle_count: usize,
    skipped_degenerate_triangle_count: usize,
}

/// One explicit static-collision model instance.
///
/// This snapshot is collision-specific and intentionally separate from
/// [`ModelDraw`]: importers can retain a stable source identifier for semantic
/// node selection without coupling renderer extraction to importer metadata.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StaticSceneCollisionDraw3d {
    /// Caller-owned stable identifier, such as an imported glTF node index.
    pub source_id: usize,
    /// Typed model asset containing the selected mesh geometry.
    pub model: ModelHandle,
    /// Optional source mesh selection; `None` includes every model mesh.
    pub mesh: Option<usize>,
    /// Exact column-major model-to-world matrix.
    pub model_matrix: [f32; 16],
}

impl StaticSceneCollisionDraw3d {
    /// Creates one explicit collision instance.
    #[must_use]
    pub const fn new(
        source_id: usize,
        model: ModelHandle,
        mesh: Option<usize>,
        model_matrix: [f32; 16],
    ) -> Self {
        Self {
            source_id,
            model,
            mesh,
            model_matrix,
        }
    }
}

/// Immutable source metadata presented to a collision primitive selector.
#[derive(Clone, Copy, Debug)]
pub struct StaticSceneCollisionPrimitive3d<'a> {
    /// Caller-owned source identifier copied from the collision draw.
    pub source_id: usize,
    /// Model asset handle used by the draw.
    pub model: ModelHandle,
    /// Selected model mesh index.
    pub mesh: usize,
    /// Optional imported/debug mesh name.
    pub mesh_name: Option<&'a str>,
    /// Primitive index inside [`Self::mesh`].
    pub primitive: usize,
    /// Optional material index referenced by the primitive.
    pub material: Option<usize>,
    /// Optional imported/debug material name.
    pub material_name: Option<&'a str>,
}

/// Resource dimension bounded during static collision construction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SceneCollisionLimitResource3d {
    /// Draw instances which contributed non-degenerate geometry.
    SourceDraws,
    /// Selected source mesh primitives.
    Primitives,
    /// Copied source vertices.
    Vertices,
    /// Retained non-degenerate triangles.
    Triangles,
}

/// Explicit memory/work limits for one static collider build.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SceneCollisionBuildLimits3d {
    /// Maximum contributing draw instances.
    pub maximum_source_draws: usize,
    /// Maximum selected mesh primitives.
    pub maximum_primitives: usize,
    /// Maximum copied source vertices.
    pub maximum_vertices: usize,
    /// Maximum retained non-degenerate triangles.
    pub maximum_triangles: usize,
}

impl Default for SceneCollisionBuildLimits3d {
    fn default() -> Self {
        Self {
            maximum_source_draws: usize::MAX,
            maximum_primitives: usize::MAX,
            maximum_vertices: usize::try_from(u32::MAX).unwrap_or(usize::MAX),
            maximum_triangles: usize::MAX,
        }
    }
}

impl StaticSceneCollider3d {
    /// Возвращает низкоуровневую геометрию для точных запросов физики.
    #[must_use]
    pub const fn mesh(&self) -> &TriangleMesh3d {
        &self.mesh
    }

    /// Число видимых model draw, внёсших хотя бы один треугольник.
    #[must_use]
    pub const fn source_draw_count(&self) -> usize {
        self.source_draw_count
    }

    /// Число мировых треугольников в коллайдере.
    #[must_use]
    pub const fn triangle_count(&self) -> usize {
        self.triangle_count
    }

    /// Число вырожденных лиц рендер-моделей, исключённых при сборке.
    ///
    /// Импортированные карты иногда содержат повторённую вершину или нулевую
    /// площадь у декоративного треугольника. Высокий API пропускает только
    /// такие лица: это не ослабляет строгую проверку
    /// [`TriangleMesh3d::from_indexed`], доступную в низком API, и не скрывает
    /// битые индексы или нечисловые координаты.
    #[must_use]
    pub const fn skipped_degenerate_triangle_count(&self) -> usize {
        self.skipped_degenerate_triangle_count
    }
}

/// Ошибка подготовки неподвижной коллизии уровня.
///
/// Поля с номерами batch/draw/mesh/primitive указывают на исходный
/// [`ExtractedModels`], а не на случайный индекс в объединённом буфере. Это
/// позволяет показать ошибку автору карты и не создавать уровень с частично
/// отсутствующими стенами.
#[derive(Debug)]
pub enum SceneCollisionError3d {
    /// Иерархию сцены не удалось обновить перед извлечением геометрии.
    Hierarchy(TransformHierarchyError),
    /// Видимая сущность ссылается на модель, которой ещё нет в `Assets`.
    MissingModel {
        /// Номер batch в [`ExtractedModels::batches`].
        batch: usize,
        /// Запрошенный resource handle.
        model: ModelHandle,
    },
    /// Draw выбрал отсутствующий mesh.
    MissingMesh {
        /// Номер batch в [`ExtractedModels::batches`].
        batch: usize,
        /// Номер draw внутри batch.
        draw: usize,
        /// Запрошенный mesh.
        mesh: usize,
        /// Доступное число mesh в модели.
        mesh_count: usize,
    },
    /// Индекс примитива не указывает на его поток позиций.
    SourceIndexOutOfBounds {
        /// Номер batch в [`ExtractedModels::batches`].
        batch: usize,
        /// Номер draw внутри batch.
        draw: usize,
        /// Номер mesh в модели.
        mesh: usize,
        /// Номер примитива в mesh.
        primitive: usize,
        /// Индекс треугольника в исходном index stream.
        index: u32,
    },
    /// Авторская вершина не является конечным числом.
    NonFiniteVertex {
        /// Номер batch в [`ExtractedModels::batches`].
        batch: usize,
        /// Номер draw внутри batch.
        draw: usize,
        /// Номер mesh в модели.
        mesh: usize,
        /// Номер примитива в mesh.
        primitive: usize,
        /// Номер вершины в позиции primitive.
        vertex: usize,
    },
    /// Матрица draw дала нечисловую мировую точку.
    NonFiniteWorldPoint {
        /// Номер batch в [`ExtractedModels::batches`].
        batch: usize,
        /// Номер draw внутри batch.
        draw: usize,
    },
    /// Объединённая геометрия не помещается в индексы формата `u32`.
    TooManyVertices,
    /// Явный предел работы/памяти исчерпан до публикации коллайдера.
    LimitExceeded {
        /// Ограниченный тип ресурса.
        resource: SceneCollisionLimitResource3d,
        /// Настроенное максимальное количество.
        limit: usize,
    },
    /// После фильтрации видимой сцены не осталось треугольников.
    EmptyScene,
    /// Физическое ядро отвергло уже собранные треугольники.
    TriangleMesh(TriangleMeshError),
}

impl fmt::Display for SceneCollisionError3d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hierarchy(error) => write!(formatter, "cannot resolve scene hierarchy: {error}"),
            Self::MissingModel { batch, model } => write!(
                formatter,
                "collision batch {batch} references unavailable model {model:?}"
            ),
            Self::MissingMesh {
                batch,
                draw,
                mesh,
                mesh_count,
            } => write!(
                formatter,
                "collision batch {batch}, draw {draw} selects mesh {mesh}, but its model has {mesh_count} meshes"
            ),
            Self::SourceIndexOutOfBounds {
                batch,
                draw,
                mesh,
                primitive,
                index,
            } => write!(
                formatter,
                "collision batch {batch}, draw {draw}, mesh {mesh}, primitive {primitive} references absent vertex {index}"
            ),
            Self::NonFiniteVertex {
                batch,
                draw,
                mesh,
                primitive,
                vertex,
            } => write!(
                formatter,
                "collision batch {batch}, draw {draw}, mesh {mesh}, primitive {primitive} has non-finite vertex {vertex}"
            ),
            Self::NonFiniteWorldPoint { batch, draw } => write!(
                formatter,
                "collision batch {batch}, draw {draw} produces a non-finite world point"
            ),
            Self::TooManyVertices => formatter
                .write_str("combined collision geometry exceeds the u32 vertex-index limit"),
            Self::LimitExceeded { resource, limit } => write!(
                formatter,
                "static collision {resource:?} exceeds configured limit {limit}"
            ),
            Self::EmptyScene => {
                formatter.write_str("visible scene has no triangle geometry for static collision")
            }
            Self::TriangleMesh(error) => {
                write!(formatter, "static collision geometry is invalid: {error}")
            }
        }
    }
}

impl Error for SceneCollisionError3d {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Hierarchy(error) => Some(error),
            Self::TriangleMesh(error) => Some(error),
            Self::MissingModel { .. }
            | Self::MissingMesh { .. }
            | Self::SourceIndexOutOfBounds { .. }
            | Self::NonFiniteVertex { .. }
            | Self::NonFiniteWorldPoint { .. }
            | Self::TooManyVertices
            | Self::LimitExceeded { .. }
            | Self::EmptyScene => None,
        }
    }
}

/// Собирает коллизию из текущего ECS-мира одной операцией.
///
/// Это высокий API для загрузки карты: функция сначала обновляет иерархию,
/// затем извлекает видимые модели и превращает их треугольники в одну
/// неподвижную [`StaticSceneCollider3d`]. Она не пропускает отсутствующие
/// модели, поскольку тихая неполная коллизия уровня хуже явной ошибки
/// загрузки. Нулевые или схлопнутые треугольники, не способные образовать
/// стену либо пол, пропускаются и учитываются в
/// [`StaticSceneCollider3d::skipped_degenerate_triangle_count`]. Вызывайте
/// после завершения загрузки всех ресурсов уровня.
///
/// Низкоуровневый путь —
/// [`build_static_scene_collider_3d_from_extracted`], когда extraction уже
/// выполнен в собственном расписании рендера/физики, и
/// [`TriangleMesh3d::from_indexed`] для процедурной геометрии.
///
/// # Errors
///
/// Возвращает [`SceneCollisionError3d`] для повреждённой геометрии, устаревшей
/// ссылки на модель или пустой видимой сцены. До успешного результата ECS-мир
/// изменяется только обычным обновлением производных world transforms.
pub fn build_static_scene_collider_3d(
    world: &mut World,
    models: &Assets<Model>,
) -> Result<StaticSceneCollider3d, SceneCollisionError3d> {
    propagate_world_transforms(world).map_err(SceneCollisionError3d::Hierarchy)?;
    let extracted = extract_models_for_static_collision(world);
    build_static_scene_collider_3d_from_extracted(&extracted, models)
}

/// Собирает коллизию из уже извлечённой сцены без изменения ECS-мира.
///
/// Это низкоуровневый вариант [`build_static_scene_collider_3d`]. Он подходит
/// для собственного порядка систем: вызовите `propagate_world_transforms` и
/// `extract_models` сами, сохраните snapshot на границе кадра и передайте его
/// сюда. Матрица каждого [`ModelDraw`] применяется к каждой позиции; один
/// model asset, размещённый несколько раз, корректно создаёт несколько групп
/// стен.
///
/// # Errors
///
/// Возвращает [`SceneCollisionError3d`] вместо создания частичной геометрии.
#[allow(clippy::too_many_lines)] // Validation, transformation and filtering form one atomic scene-build step.
pub fn build_static_scene_collider_3d_from_extracted(
    extracted: &ExtractedModels,
    models: &Assets<Model>,
) -> Result<StaticSceneCollider3d, SceneCollisionError3d> {
    let draws = extracted
        .batches()
        .iter()
        .enumerate()
        .flat_map(|(batch, model_batch)| {
            model_batch
                .draws()
                .iter()
                .copied()
                .enumerate()
                .map(move |(draw, source)| CollisionBuildDraw3d {
                    batch,
                    draw,
                    source: StaticSceneCollisionDraw3d::new(
                        0,
                        source.model,
                        source.mesh,
                        source.model_matrix,
                    ),
                })
        });
    build_static_scene_collider_3d_internal(
        draws,
        models,
        SceneCollisionBuildLimits3d::default(),
        |_| true,
    )
}

/// Builds one filtered static collider from importer/application-owned draws.
///
/// `select` runs before any vertex copy and receives stable source, mesh and
/// material metadata. Returning `false` excludes the complete primitive.
/// Source names are borrowed from `models`; the resulting collider owns only
/// transformed physics geometry.
///
/// # Errors
///
/// Returns [`SceneCollisionError3d`] for invalid model references, geometry,
/// an empty filtered result or an exhausted explicit build limit.
pub fn build_static_scene_collider_3d_from_draws_with<F>(
    draws: &[StaticSceneCollisionDraw3d],
    models: &Assets<Model>,
    limits: SceneCollisionBuildLimits3d,
    select: F,
) -> Result<StaticSceneCollider3d, SceneCollisionError3d>
where
    F: FnMut(StaticSceneCollisionPrimitive3d<'_>) -> bool,
{
    build_static_scene_collider_3d_internal(
        draws
            .iter()
            .copied()
            .enumerate()
            .map(|(draw, source)| CollisionBuildDraw3d {
                batch: 0,
                draw,
                source,
            }),
        models,
        limits,
        select,
    )
}

#[derive(Clone, Copy)]
struct CollisionBuildDraw3d {
    batch: usize,
    draw: usize,
    source: StaticSceneCollisionDraw3d,
}

#[allow(
    clippy::too_many_lines,
    reason = "validation, selection, transformation and bounded assembly form one atomic build"
)]
fn build_static_scene_collider_3d_internal<I, F>(
    draws: I,
    models: &Assets<Model>,
    limits: SceneCollisionBuildLimits3d,
    mut select: F,
) -> Result<StaticSceneCollider3d, SceneCollisionError3d>
where
    I: IntoIterator<Item = CollisionBuildDraw3d>,
    F: FnMut(StaticSceneCollisionPrimitive3d<'_>) -> bool,
{
    let mut vertices = Vec::<PhysicsVec3>::new();
    let mut indices = Vec::<u32>::new();
    let mut source_draw_count = 0;
    let mut primitive_count = 0;
    let mut skipped_degenerate_triangle_count = 0;

    for build_draw in draws {
        let batch_index = build_draw.batch;
        let draw_index = build_draw.draw;
        let draw = build_draw.source;
        let model = models
            .get(draw.model)
            .ok_or(SceneCollisionError3d::MissingModel {
                batch: batch_index,
                model: draw.model,
            })?;
        if let Some(mesh) = draw.mesh
            && mesh >= model.meshes().len()
        {
            return Err(SceneCollisionError3d::MissingMesh {
                batch: batch_index,
                draw: draw_index,
                mesh,
                mesh_count: model.meshes().len(),
            });
        }

        let mut draw_has_triangles = false;
        for (mesh_index, mesh) in model.meshes().iter().enumerate() {
            if draw.mesh.is_some_and(|selected| selected != mesh_index) {
                continue;
            }
            for (primitive_index, primitive) in mesh.primitives().iter().enumerate() {
                let material = primitive.material().map(yuyib_model::MaterialIndex::get);
                let material_name = material
                    .and_then(|material| model.materials().get(material))
                    .and_then(yuyib_model::Material::name);
                if !select(StaticSceneCollisionPrimitive3d {
                    source_id: draw.source_id,
                    model: draw.model,
                    mesh: mesh_index,
                    mesh_name: mesh.name(),
                    primitive: primitive_index,
                    material,
                    material_name,
                }) {
                    continue;
                }
                if primitive_count >= limits.maximum_primitives {
                    return Err(SceneCollisionError3d::LimitExceeded {
                        resource: SceneCollisionLimitResource3d::Primitives,
                        limit: limits.maximum_primitives,
                    });
                }
                primitive_count += 1;
                let required_vertices = vertices
                    .len()
                    .checked_add(primitive.positions().len())
                    .ok_or(SceneCollisionError3d::TooManyVertices)?;
                if required_vertices > limits.maximum_vertices {
                    return Err(SceneCollisionError3d::LimitExceeded {
                        resource: SceneCollisionLimitResource3d::Vertices,
                        limit: limits.maximum_vertices,
                    });
                }
                let vertex_base = u32::try_from(vertices.len())
                    .map_err(|_| SceneCollisionError3d::TooManyVertices)?;
                for (vertex_index, position) in primitive.positions().iter().copied().enumerate() {
                    if !position.iter().all(|value| value.is_finite()) {
                        return Err(SceneCollisionError3d::NonFiniteVertex {
                            batch: batch_index,
                            draw: draw_index,
                            mesh: mesh_index,
                            primitive: primitive_index,
                            vertex: vertex_index,
                        });
                    }
                    let point = transform_collision_point(draw.model_matrix, position);
                    if !point.iter().all(|value| value.is_finite()) {
                        return Err(SceneCollisionError3d::NonFiniteWorldPoint {
                            batch: batch_index,
                            draw: draw_index,
                        });
                    }
                    vertices.push(PhysicsVec3::new(point[0], point[1], point[2]));
                }
                for source_index in primitive.indices() {
                    if usize::try_from(*source_index)
                        .ok()
                        .is_none_or(|index| index >= primitive.positions().len())
                    {
                        return Err(SceneCollisionError3d::SourceIndexOutOfBounds {
                            batch: batch_index,
                            draw: draw_index,
                            mesh: mesh_index,
                            primitive: primitive_index,
                            index: *source_index,
                        });
                    }
                }
                let (source_triangles, []) = primitive.indices().as_chunks::<3>() else {
                    return Err(SceneCollisionError3d::TriangleMesh(
                        TriangleMeshError::InvalidIndexCount {
                            actual: primitive.indices().len(),
                        },
                    ));
                };
                for source_triangle in source_triangles {
                    let first = vertex_base
                        .checked_add(source_triangle[0])
                        .ok_or(SceneCollisionError3d::TooManyVertices)?;
                    let second = vertex_base
                        .checked_add(source_triangle[1])
                        .ok_or(SceneCollisionError3d::TooManyVertices)?;
                    let third = vertex_base
                        .checked_add(source_triangle[2])
                        .ok_or(SceneCollisionError3d::TooManyVertices)?;
                    let face = [
                        vertices[first as usize],
                        vertices[second as usize],
                        vertices[third as usize],
                    ];
                    if collision_triangle_is_degenerate(face) {
                        skipped_degenerate_triangle_count += 1;
                        continue;
                    }
                    let triangle_count = indices.len() / 3;
                    if triangle_count >= limits.maximum_triangles {
                        return Err(SceneCollisionError3d::LimitExceeded {
                            resource: SceneCollisionLimitResource3d::Triangles,
                            limit: limits.maximum_triangles,
                        });
                    }
                    indices.extend([first, second, third]);
                    draw_has_triangles = true;
                }
            }
        }
        if draw_has_triangles {
            if source_draw_count >= limits.maximum_source_draws {
                return Err(SceneCollisionError3d::LimitExceeded {
                    resource: SceneCollisionLimitResource3d::SourceDraws,
                    limit: limits.maximum_source_draws,
                });
            }
            source_draw_count += 1;
        }
    }

    if indices.is_empty() {
        return Err(SceneCollisionError3d::EmptyScene);
    }
    let triangle_count = indices.len() / 3;
    let mesh = TriangleMesh3d::from_indexed(&vertices, &indices)
        .map_err(SceneCollisionError3d::TriangleMesh)?;
    Ok(StaticSceneCollider3d {
        mesh,
        source_draw_count,
        triangle_count,
        skipped_degenerate_triangle_count,
    })
}

fn collision_triangle_is_degenerate(face: [PhysicsVec3; 3]) -> bool {
    let first_edge = face[1] - face[0];
    let second_edge = face[2] - face[0];
    let cross = PhysicsVec3::new(
        first_edge.y * second_edge.z - first_edge.z * second_edge.y,
        first_edge.z * second_edge.x - first_edge.x * second_edge.z,
        first_edge.x * second_edge.y - first_edge.y * second_edge.x,
    );
    cross.length_squared() <= f32::EPSILON
}

fn transform_collision_point(matrix: [f32; 16], point: [f32; 3]) -> [f32; 3] {
    [
        matrix[0] * point[0] + matrix[4] * point[1] + matrix[8] * point[2] + matrix[12],
        matrix[1] * point[0] + matrix[5] * point[1] + matrix[9] * point[2] + matrix[13],
        matrix[2] * point[0] + matrix[6] * point[1] + matrix[10] * point[2] + matrix[14],
    ]
}

/// One authored model choice valid up to `max_distance` world units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LodLevel3d {
    /// Model selected for this distance band.
    pub model: ModelHandle,
    /// Inclusive upper bound of this band in world units.
    pub max_distance: f32,
}

/// Renderer-neutral authored LOD bands for a [`Model3d`] entity.
///
/// Bands must have strictly increasing finite upper bounds. Beyond the final
/// bound the final model remains selected; this layer does not hide entities
/// or perform GPU/CPU frustum culling. Model-handle residency remains a
/// renderer/asset responsibility, so a selected stale/missing handle is passed
/// through explicitly rather than guessed or silently replaced.
#[derive(Component, Clone, Debug, PartialEq)]
pub struct LodGroup3d {
    levels: Vec<LodLevel3d>,
}

impl LodGroup3d {
    /// Creates validated increasing distance bands.
    ///
    /// # Errors
    ///
    /// Returns [`LodConfigError`] for no levels, an invalid bound, or bounds
    /// which are not strictly increasing.
    pub fn new(levels: Vec<LodLevel3d>) -> Result<Self, LodConfigError> {
        if levels.is_empty() {
            return Err(LodConfigError::NoLevels);
        }
        let mut previous = None;
        for (index, level) in levels.iter().enumerate() {
            if !level.max_distance.is_finite() || level.max_distance < 0.0 {
                return Err(LodConfigError::InvalidDistance { index });
            }
            if previous.is_some_and(|value: f32| level.max_distance <= value) {
                return Err(LodConfigError::NonIncreasingDistance { index });
            }
            previous = Some(level.max_distance);
        }
        Ok(Self { levels })
    }

    /// Returns levels in authored near-to-far order.
    #[must_use]
    pub fn levels(&self) -> &[LodLevel3d] {
        &self.levels
    }

    /// Selects one model for a finite non-negative camera distance.
    ///
    /// # Errors
    ///
    /// Returns [`LodSelectionError::InvalidDistance`] for negative, NaN or
    /// infinite distance.
    pub fn select(&self, distance: f32) -> Result<ModelHandle, LodSelectionError> {
        if !distance.is_finite() || distance < 0.0 {
            return Err(LodSelectionError::InvalidDistance(distance));
        }
        let Some(last) = self.levels.last() else {
            return Err(LodSelectionError::EmptyGroup);
        };
        Ok(self
            .levels
            .iter()
            .find(|level| distance <= level.max_distance)
            .unwrap_or(last)
            .model)
    }
}

/// Authored LOD configuration failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LodConfigError {
    /// No levels were supplied.
    NoLevels,
    /// A distance was negative, NaN or infinite.
    InvalidDistance {
        /// Level index.
        index: usize,
    },
    /// A bound was equal to or below the preceding bound.
    NonIncreasingDistance {
        /// Level index.
        index: usize,
    },
}

impl fmt::Display for LodConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoLevels => formatter.write_str("LOD group needs at least one level"),
            Self::InvalidDistance { index } => {
                write!(formatter, "LOD level {index} has an invalid distance")
            }
            Self::NonIncreasingDistance { index } => write!(
                formatter,
                "LOD level {index} must have a strictly increasing distance"
            ),
        }
    }
}
impl Error for LodConfigError {}

/// Runtime LOD selection failure.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LodSelectionError {
    /// Distance was negative, NaN or infinite.
    InvalidDistance(f32),
    /// Camera position contained NaN or infinity.
    InvalidCameraPosition,
    /// The group was constructed outside its validated constructor.
    EmptyGroup,
}
impl fmt::Display for LodSelectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDistance(value) => write!(
                formatter,
                "LOD distance must be finite and non-negative, got {value}"
            ),
            Self::InvalidCameraPosition => {
                formatter.write_str("LOD camera position must be finite")
            }
            Self::EmptyGroup => formatter.write_str("LOD group has no levels"),
        }
    }
}
impl Error for LodSelectionError {}

/// One selected model draw for a renderer-owned LOD phase.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LodModelDraw3d {
    /// ECS owner, retained for renderer telemetry/caches.
    pub entity: Entity,
    /// Selected model handle; this crate does not test asset residency.
    pub model: ModelHandle,
    /// Optional mesh selection retained when no authored LOD replacement ran.
    ///
    /// A selected [`LodGroup3d`] replaces the complete model and therefore
    /// clears this field. Ordinary imported glTF nodes keep their one-node to
    /// one-mesh relationship through the high-level LOD extraction path.
    pub mesh: Option<usize>,
    /// Exact column-major model-to-world matrix at the extraction boundary.
    pub model_matrix: [f32; 16],
    /// Distance used for this selection.
    pub distance: f32,
    /// Explicit render ordering inherited from [`Model3d`].
    pub render_order: i32,
    /// Overlay flag inherited from [`Model3d`].
    pub overlay: bool,
}

/// Deterministically ordered renderer-neutral LOD snapshot.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExtractedLodModels3d {
    draws: Vec<LodModelDraw3d>,
}
impl ExtractedLodModels3d {
    /// Returns selected draws ordered by render order then full entity ID.
    #[must_use]
    pub fn draws(&self) -> &[LodModelDraw3d] {
        &self.draws
    }
    /// Returns the number of visible extracted draws.
    #[must_use]
    pub fn len(&self) -> usize {
        self.draws.len()
    }
    /// Returns whether no visible model entities were extracted.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.draws.is_empty()
    }
}

/// Extracts visible models with optional authored LOD selection.
///
/// Entities without a resolved [`WorldTransform3d`] or [`Transform3d`] are
/// skipped, matching [`extract_models`].
/// A model without [`LodGroup3d`] keeps its [`Model3d::model`] handle. This is
/// selection only: no culling, hysteresis state, asset existence check or GPU
/// work happens here. Hysteresis is intentionally absent so selection is a
/// pure deterministic function of this snapshot's camera position.
/// Honours [`RenderFlags3d`] the same way as [`extract_models`].
///
/// # Errors
///
/// Returns [`LodSelectionError::InvalidCameraPosition`] for non-finite camera data.
pub fn extract_lod_models_3d(
    world: &mut World,
    camera_position: [f32; 3],
) -> Result<ExtractedLodModels3d, LodSelectionError> {
    if !camera_position.iter().all(|value| value.is_finite()) {
        return Err(LodSelectionError::InvalidCameraPosition);
    }
    let mut draws: Vec<LodModelDraw3d> = world
        .query::<(
            Entity,
            &Model3d,
            Option<&RenderFlags3d>,
            Option<&WorldTransform3d>,
            Option<&Transform3d>,
            Option<&LodGroup3d>,
        )>()
        .iter(world)
        .filter_map(
            |(entity, model, render_flags, world_transform, transform, lod)| {
                if !model_draw_enabled(model, render_flags) {
                    return None;
                }
                let model_matrix = world_transform
                    .map(|transform| transform.column_major())
                    .or_else(|| transform.map(|transform| transform_matrix(*transform)))?;
                let dx = model_matrix[12] - camera_position[0];
                let dy = model_matrix[13] - camera_position[1];
                let dz = model_matrix[14] - camera_position[2];
                let distance = dx.mul_add(dx, dy.mul_add(dy, dz * dz)).sqrt();
                let (selected, mesh) = match lod {
                    Some(group) => (group.select(distance).ok()?, None),
                    None => (model.model, model.mesh),
                };
                Some(LodModelDraw3d {
                    entity,
                    model: selected,
                    mesh,
                    model_matrix,
                    distance,
                    render_order: model.render_order,
                    overlay: model.overlay,
                })
            },
        )
        .collect();
    draws.sort_by_key(|draw| (draw.render_order, draw.entity.to_bits()));
    Ok(ExtractedLodModels3d { draws })
}

/// Extracts the ordinary renderer snapshot while replacing authored models by
/// their camera-distance LOD selections.
///
/// This is the high-level bridge used by standard scene renderers. Entities
/// without [`LodGroup3d`] keep their original model and source
/// `Model3d::mesh` sub-selection. An actual LOD selection targets the complete
/// replacement model, so only that path intentionally clears the mesh.
/// Visibility matches [`extract_models`]: [`RenderFlags3d::draw`] false skips
/// the entity even when [`Model3d::visible`] remains true.
///
/// # Errors
///
/// Returns [`LodSelectionError`] for invalid camera data or LOD configuration.
pub fn extract_models_with_lod_3d(
    world: &mut World,
    camera_position: [f32; 3],
) -> Result<ExtractedModels, LodSelectionError> {
    let lod = extract_lod_models_3d(world, camera_position)?;
    let model_count = lod.draws.len();
    let mut batches: Vec<ModelDrawBatch> = Vec::new();
    for selected in lod.draws {
        let draw = ModelDraw {
            model: selected.model,
            mesh: selected.mesh,
            model_matrix: selected.model_matrix,
            render_order: selected.render_order,
            overlay: selected.overlay,
        };
        match batches.last_mut() {
            Some(batch) if batch.model == draw.model => batch.draws.push(draw),
            _ => batches.push(ModelDrawBatch {
                model: draw.model,
                draws: vec![draw],
            }),
        }
    }
    Ok(ExtractedModels {
        batches,
        model_count,
    })
}

/// Shared draw gate for ordinary and LOD extraction.
#[inline]
fn model_draw_enabled(model: &Model3d, render_flags: Option<&RenderFlags3d>) -> bool {
    render_flags.map_or(model.visible, |flags| flags.draw && model.visible)
}

/// A renderer-neutral infinitely distant light with parallel rays.
///
/// `direction` points **from the light towards the scene**. For example,
/// `[0.0, -1.0, 0.0]` models a sun directly overhead. The constructor
/// normalizes it, so renderers can rely on a unit direction. Directional
/// lights have no position or finite range; point, spot, shadow and cookie
/// lights belong to future, separate components rather than overloading this
/// physically distinct type.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct DirectionalLight3d {
    direction: [f32; 3],
    color: [f32; 3],
    illuminance_lux: f32,
    enabled: bool,
}

impl DirectionalLight3d {
    /// Creates an enabled directional light.
    ///
    /// `color` is linear RGB and every component must be finite and
    /// non-negative. `illuminance_lux` is a non-negative illuminance in lux;
    /// zero is valid for a temporarily black light. A real-world noon sun is
    /// roughly 100,000 lux, while artist-friendly scenes often intentionally
    /// use much smaller values depending on their exposure policy.
    ///
    /// # Errors
    ///
    /// Returns [`DirectionalLightError`] if the direction is non-finite or
    /// zero, or if colour/illuminance is non-finite or negative.
    pub fn new(
        direction: [f32; 3],
        color: [f32; 3],
        illuminance_lux: f32,
    ) -> Result<Self, DirectionalLightError> {
        let direction = normalize_direction(direction)?;
        validate_color(color)?;
        validate_illuminance(illuminance_lux)?;
        Ok(Self {
            direction,
            color,
            illuminance_lux,
            enabled: true,
        })
    }

    /// Creates a white directional light using an approximate noon-sun level.
    ///
    /// # Errors
    ///
    /// Returns [`DirectionalLightError`] if `direction` is non-finite or zero.
    pub fn sun(direction: [f32; 3]) -> Result<Self, DirectionalLightError> {
        Self::new(direction, [1.0, 1.0, 1.0], 100_000.0)
    }

    /// Returns the normalized light-to-scene ray direction.
    #[must_use]
    pub const fn direction(&self) -> [f32; 3] {
        self.direction
    }

    /// Returns the non-negative linear RGB multiplier.
    #[must_use]
    pub const fn color(&self) -> [f32; 3] {
        self.color
    }

    /// Returns the non-negative incident illuminance in lux.
    #[must_use]
    pub const fn illuminance_lux(&self) -> f32 {
        self.illuminance_lux
    }

    /// Returns whether extraction includes this light.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Replaces the normalized direction.
    ///
    /// # Errors
    ///
    /// Returns [`DirectionalLightError`] if `direction` is non-finite or zero.
    pub fn with_direction(mut self, direction: [f32; 3]) -> Result<Self, DirectionalLightError> {
        self.direction = normalize_direction(direction)?;
        Ok(self)
    }

    /// Replaces the linear RGB multiplier.
    ///
    /// # Errors
    ///
    /// Returns [`DirectionalLightError`] if any component is non-finite or
    /// negative.
    pub fn with_color(mut self, color: [f32; 3]) -> Result<Self, DirectionalLightError> {
        validate_color(color)?;
        self.color = color;
        Ok(self)
    }

    /// Replaces the incident illuminance in lux.
    ///
    /// # Errors
    ///
    /// Returns [`DirectionalLightError`] if `illuminance_lux` is non-finite or
    /// negative.
    pub fn with_illuminance_lux(
        mut self,
        illuminance_lux: f32,
    ) -> Result<Self, DirectionalLightError> {
        validate_illuminance(illuminance_lux)?;
        self.illuminance_lux = illuminance_lux;
        Ok(self)
    }

    /// Sets whether extraction includes this light.
    #[must_use]
    pub const fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

/// Validation failure for a [`DirectionalLight3d`] value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DirectionalLightError {
    /// A direction component was NaN or infinity.
    NonFiniteDirection,
    /// A finite direction had zero or numerically degenerate length.
    ZeroDirection,
    /// A colour component was NaN or infinity.
    NonFiniteColor,
    /// A colour component was negative.
    NegativeColor,
    /// The illuminance was NaN or infinity.
    NonFiniteIlluminance,
    /// The illuminance was negative.
    NegativeIlluminance,
}

impl fmt::Display for DirectionalLightError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFiniteDirection => formatter.write_str("light direction must be finite"),
            Self::ZeroDirection => formatter.write_str("light direction must have non-zero length"),
            Self::NonFiniteColor => formatter.write_str("light color must be finite"),
            Self::NegativeColor => formatter.write_str("light color must be non-negative"),
            Self::NonFiniteIlluminance => formatter.write_str("light illuminance must be finite"),
            Self::NegativeIlluminance => {
                formatter.write_str("light illuminance must be non-negative")
            }
        }
    }
}

impl Error for DirectionalLightError {}

/// One directional light copied out of ECS state at a frame boundary.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DirectionalLightDraw {
    /// Normalized light-to-scene ray direction.
    pub direction: [f32; 3],
    /// Linear non-negative RGB multiplier.
    pub color: [f32; 3],
    /// Non-negative incident illuminance in lux.
    pub illuminance_lux: f32,
}

/// A renderer-neutral, deterministically ordered directional-light snapshot.
///
/// Lights are sorted by ascending full generational entity ID. This order does
/// not imply a lighting priority; it merely makes CPU snapshots stable for
/// reproducibility, testing and any future renderer-side light budget policy.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExtractedDirectionalLights {
    lights: Vec<DirectionalLightDraw>,
}

impl ExtractedDirectionalLights {
    /// Returns enabled directional lights in deterministic entity order.
    #[must_use]
    pub fn lights(&self) -> &[DirectionalLightDraw] {
        &self.lights
    }

    /// Returns the number of enabled directional lights.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lights.len()
    }

    /// Returns whether no enabled directional lights were extracted.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lights.is_empty()
    }
}

/// Extracts enabled [`DirectionalLight3d`] components from `world`.
///
/// Construction and mutation methods keep the component valid, so this
/// renderer-neutral boundary does not repeat validation work every frame. ECS
/// query creation needs mutable world access because Bevy initializes its
/// query state lazily.
#[must_use]
pub fn extract_directional_lights(world: &mut World) -> ExtractedDirectionalLights {
    let mut lights: Vec<(u64, DirectionalLightDraw)> = world
        .query::<(Entity, &DirectionalLight3d)>()
        .iter(world)
        .filter_map(|(entity, light)| {
            light.enabled.then_some((
                entity.to_bits(),
                DirectionalLightDraw {
                    direction: light.direction,
                    color: light.color,
                    illuminance_lux: light.illuminance_lux,
                },
            ))
        })
        .collect();
    lights.sort_by_key(|(entity_bits, _)| *entity_bits);
    ExtractedDirectionalLights {
        lights: lights.into_iter().map(|(_, light)| light).collect(),
    }
}

fn normalize_direction(direction: [f32; 3]) -> Result<[f32; 3], DirectionalLightError> {
    if !all_finite3(direction) {
        return Err(DirectionalLightError::NonFiniteDirection);
    }
    let length_squared = direction.iter().map(|value| value * value).sum::<f32>();
    if !length_squared.is_finite() || length_squared <= f32::EPSILON {
        return Err(DirectionalLightError::ZeroDirection);
    }
    let inverse_length = length_squared.sqrt().recip();
    let normalized = direction.map(|value| value * inverse_length);
    all_finite3(normalized)
        .then_some(normalized)
        .ok_or(DirectionalLightError::NonFiniteDirection)
}

fn validate_color(color: [f32; 3]) -> Result<(), DirectionalLightError> {
    if !all_finite3(color) {
        return Err(DirectionalLightError::NonFiniteColor);
    }
    if color.iter().any(|value| *value < 0.0) {
        return Err(DirectionalLightError::NegativeColor);
    }
    Ok(())
}

fn validate_illuminance(illuminance_lux: f32) -> Result<(), DirectionalLightError> {
    if !illuminance_lux.is_finite() {
        return Err(DirectionalLightError::NonFiniteIlluminance);
    }
    if illuminance_lux < 0.0 {
        return Err(DirectionalLightError::NegativeIlluminance);
    }
    Ok(())
}

fn all_finite3(values: [f32; 3]) -> bool {
    values.iter().all(|value| value.is_finite())
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // Values are copied exactly; tests assert extraction order.
mod tests {
    use yuyib_assets::Assets;
    use yuyib_model::{Material, MaterialIndex, Mesh, MeshPrimitive, Model};

    use super::*;

    fn model(models: &mut Assets<Model>) -> ModelHandle {
        models.insert(Model::cube(0.5).expect("valid model"))
    }

    fn identity_zero_to_one_frustum() -> Frustum3d {
        Frustum3d::from_clip_matrix(
            [
                1.0, 0.0, 0.0, 0.0, // X column
                0.0, 1.0, 0.0, 0.0, // Y column
                0.0, 0.0, 1.0, 0.0, // Z column
                0.0, 0.0, 0.0, 1.0, // W column
            ],
            ClipDepthRange3d::ZeroToOne,
        )
        .expect("identity clip volume is valid")
    }

    #[test]
    fn uniform_scale_builder_updates_every_axis_without_moving_the_object() {
        let transform = Transform3d::from_translation([2.0, 3.0, 4.0]).with_uniform_scale(0.5);
        assert_eq!(transform.translation, [2.0, 3.0, 4.0]);
        assert_eq!(transform.scale, [0.5; 3]);

        let local = LocalTransform3d::from_translation([-1.0, 0.0, 1.0]).with_uniform_scale(2.0);
        assert_eq!(local.translation, [-1.0, 0.0, 1.0]);
        assert_eq!(local.scale, [2.0; 3]);
    }

    #[test]
    fn frustum_extracts_explicit_depth_conventions() {
        let zero_to_one = identity_zero_to_one_frustum();
        let point = LocalBoundingSphere3d::new([0.0, 0.0, 0.0], 0.0).expect("point");
        assert!(
            zero_to_one
                .intersects_local_bounds(
                    point.into(),
                    transform_matrix(Transform3d::from_translation([0.0, 0.0, 0.5]))
                )
                .expect("finite test")
        );
        assert!(
            !zero_to_one
                .intersects_local_bounds(
                    point.into(),
                    transform_matrix(Transform3d::from_translation([0.0, 0.0, -0.1]))
                )
                .expect("finite test")
        );

        let negative_one_to_one = Frustum3d::from_clip_matrix(
            [
                1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
            ],
            ClipDepthRange3d::NegativeOneToOne,
        )
        .expect("OpenGL-style identity clip volume is valid");
        assert!(
            negative_one_to_one
                .intersects_local_bounds(
                    point.into(),
                    transform_matrix(Transform3d::from_translation([0.0, 0.0, -0.5]))
                )
                .expect("finite test")
        );
    }

    /// Same look-at × ZeroToOne perspective as `Camera3d::view_projection`.
    fn perspective_look_at(
        eye: [f32; 3],
        target: [f32; 3],
        near: f32,
        far: f32,
        fov_y: f32,
        aspect: f32,
    ) -> [f32; 16] {
        let forward = {
            let d = [
                target[0] - eye[0],
                target[1] - eye[1],
                target[2] - eye[2],
            ];
            let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            [d[0] / len, d[1] / len, d[2] / len]
        };
        let up = [0.0_f32, 1.0, 0.0];
        let side = {
            let c = [
                forward[1] * up[2] - forward[2] * up[1],
                forward[2] * up[0] - forward[0] * up[2],
                forward[0] * up[1] - forward[1] * up[0],
            ];
            let len = (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt();
            [c[0] / len, c[1] / len, c[2] / len]
        };
        let actual_up = [
            side[1] * forward[2] - side[2] * forward[1],
            side[2] * forward[0] - side[0] * forward[2],
            side[0] * forward[1] - side[1] * forward[0],
        ];
        let view = [
            side[0],
            actual_up[0],
            -forward[0],
            0.0,
            side[1],
            actual_up[1],
            -forward[1],
            0.0,
            side[2],
            actual_up[2],
            -forward[2],
            0.0,
            -(side[0] * eye[0] + side[1] * eye[1] + side[2] * eye[2]),
            -(actual_up[0] * eye[0] + actual_up[1] * eye[1] + actual_up[2] * eye[2]),
            forward[0] * eye[0] + forward[1] * eye[1] + forward[2] * eye[2],
            1.0,
        ];
        let focal = 1.0 / (fov_y * 0.5).tan();
        let projection = [
            focal / aspect,
            0.0,
            0.0,
            0.0,
            0.0,
            focal,
            0.0,
            0.0,
            0.0,
            0.0,
            far / (near - far),
            -1.0,
            0.0,
            0.0,
            (near * far) / (near - far),
            0.0,
        ];
        let mut out = [0.0_f32; 16];
        for col in 0..4 {
            for row in 0..4 {
                out[col * 4 + row] = (0..4)
                    .map(|k| projection[k * 4 + row] * view[col * 4 + k])
                    .sum();
            }
        }
        out
    }

    #[test]
    fn perspective_frustum_rejects_cube_clearly_behind_camera() {
        // Camera at +Z looking at origin (−Z forward). A cube further +Z is
        // behind the camera. Plane-only frustums often keep such boxes (infinite
        // pyramid) — that shows up as huge cubes popping in under yaw.
        let eye = [0.0_f32, 0.0, 3.0];
        let vp = perspective_look_at(eye, [0.0, 0.0, 0.0], 0.1, 100.0, 1.0, 16.0 / 9.0);
        let frustum =
            Frustum3d::from_clip_matrix(vp, ClipDepthRange3d::ZeroToOne).expect("frustum");
        let bounds = LocalAabb3d::new([-0.7; 3], [0.7; 3]).expect("cube");
        let behind = transform_matrix(Transform3d::from_translation([0.0, 0.0, 8.0]));
        let in_front = transform_matrix(Transform3d::from_translation([0.0, 0.0, 0.0]));
        assert!(
            frustum
                .intersects_local_bounds(bounds.into(), in_front)
                .expect("finite"),
            "cube at look-at must stay visible"
        );
        assert!(
            !frustum
                .intersects_local_bounds(bounds.into(), behind)
                .expect("finite"),
            "cube behind camera must be culled (yaw pop source)"
        );
    }

    #[test]
    fn frustum_projects_aabb_through_non_uniform_affine_transform() {
        let frustum = identity_zero_to_one_frustum();
        let bounds = LocalAabb3d::new([-0.5; 3], [0.5; 3]).expect("valid local box");
        let transform = Transform3d::from_translation([1.6, 0.0, 0.5]).with_scale([2.0, 0.5, 1.0]);
        assert!(
            frustum
                .intersects_local_bounds(bounds.into(), transform_matrix(transform))
                .expect("finite transformed box"),
            "scaled box reaches back into the right frustum plane"
        );
        assert!(
            !frustum
                .intersects_local_bounds(
                    bounds.into(),
                    transform_matrix(transform.with_translation([2.1, 0.0, 0.5])),
                )
                .expect("finite transformed box")
        );
    }

    #[test]
    fn bounds_registry_prefers_mesh_bounds_then_falls_back_to_model() {
        let mut models = Assets::new();
        let handle = model(&mut models);
        let model_bounds = LocalBoundingSphere3d::new([0.0; 3], 2.0).expect("model bounds");
        let mesh_bounds = LocalAabb3d::new([-0.25; 3], [0.25; 3]).expect("mesh bounds");
        let mut registry = ModelBoundsRegistry3d::new();
        registry.insert_model(handle, model_bounds);
        assert_eq!(
            registry.get(ModelBoundsKey3d::mesh(handle, 7)),
            Some(model_bounds.into())
        );
        registry.insert_mesh(handle, 7, mesh_bounds);
        assert_eq!(
            registry.get(ModelBoundsKey3d::mesh(handle, 7)),
            Some(mesh_bounds.into())
        );
        assert_eq!(registry.len(), 2);
    }

    #[test]
    fn computed_model_bounds_include_model_and_each_source_mesh() {
        let left = MeshPrimitive::new(
            vec![[-3.0, -1.0, 0.0], [-1.0, -1.0, 0.0], [-2.0, 2.0, 1.0]],
            vec![0, 1, 2],
        )
        .expect("valid left triangle");
        let right = MeshPrimitive::new(
            vec![[2.0, 0.0, -2.0], [5.0, 0.0, -2.0], [2.0, 4.0, 3.0]],
            vec![0, 1, 2],
        )
        .expect("valid right triangle");
        let source = Model::new(
            vec![
                Mesh::new(Some("left".to_owned()), vec![left]).expect("non-empty mesh"),
                Mesh::new(Some("right".to_owned()), vec![right]).expect("non-empty mesh"),
            ],
            Vec::new(),
            Vec::new(),
        )
        .expect("valid model");
        let mut models = Assets::new();
        let handle = models.insert(source);

        let computed = compute_model_bounds_3d(&models, handle).expect("finite model bounds");
        assert_eq!(computed.position_count(), 6);
        assert_eq!(computed.model().minimum(), [-3.0, -1.0, -2.0]);
        assert_eq!(computed.model().maximum(), [5.0, 4.0, 3.0]);
        assert_eq!(computed.meshes().len(), 2);
        assert_eq!(computed.meshes()[0].minimum(), [-3.0, -1.0, 0.0]);
        assert_eq!(computed.meshes()[0].maximum(), [-1.0, 2.0, 1.0]);
        assert_eq!(computed.meshes()[1].minimum(), [2.0, 0.0, -2.0]);
        assert_eq!(computed.meshes()[1].maximum(), [5.0, 4.0, 3.0]);
    }

    #[test]
    fn registering_computed_bounds_replaces_stale_entries_transactionally() {
        let mut models = Assets::new();
        let handle = model(&mut models);
        let stale = LocalBoundingSphere3d::new([100.0; 3], 50.0).expect("stale bounds");
        let mut registry = ModelBoundsRegistry3d::new();
        registry.insert_model(handle, stale);
        registry.insert_mesh(handle, 99, stale);

        let computed = register_computed_model_bounds_3d(&mut registry, &models, handle)
            .expect("cube has finite positions");
        assert_eq!(computed.model().minimum(), [-0.5; 3]);
        assert_eq!(computed.model().maximum(), [0.5; 3]);
        assert_eq!(registry.len(), 2, "model entry plus its only mesh");
        assert_eq!(
            registry.get(ModelBoundsKey3d::mesh(handle, 0)),
            Some(computed.meshes()[0].into())
        );
        assert_eq!(
            registry.get(ModelBoundsKey3d::mesh(handle, 99)),
            Some(computed.model().into()),
            "removed stale mesh key falls back to current model bounds"
        );

        let invalid = models.insert(Model::default());
        registry.insert_model(invalid, stale);
        assert!(matches!(
            register_computed_model_bounds_3d(&mut registry, &models, invalid),
            Err(ComputeModelBoundsError3d::EmptyModel { model }) if model == invalid
        ));
        assert_eq!(
            registry.get(ModelBoundsKey3d::model(invalid)),
            Some(stale.into()),
            "failed computation does not mutate existing bounds"
        );
    }

    #[test]
    fn computed_bounds_report_missing_empty_and_non_finite_geometry() {
        let mut models = Assets::new();
        let missing = model(&mut models);
        models.remove(missing).expect("model was resident");
        assert!(matches!(
            compute_model_bounds_3d(&models, missing),
            Err(ComputeModelBoundsError3d::MissingModel { model }) if model == missing
        ));

        let empty = models.insert(Model::default());
        assert!(matches!(
            compute_model_bounds_3d(&models, empty),
            Err(ComputeModelBoundsError3d::EmptyModel { model }) if model == empty
        ));

        let primitive = MeshPrimitive::new(
            vec![[f32::NAN, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            vec![0, 1, 2],
        )
        .expect("model validation intentionally permits non-finite source positions");
        let invalid_source = Model::new(
            vec![Mesh::new(None, vec![primitive]).expect("non-empty mesh")],
            Vec::new(),
            Vec::new(),
        )
        .expect("cross-resource model validation succeeds");
        let invalid = models.insert(invalid_source);
        assert!(matches!(
            compute_model_bounds_3d(&models, invalid),
            Err(ComputeModelBoundsError3d::NonFinitePosition {
                model,
                mesh: 0,
                primitive: 0,
                position: 0,
            }) if model == invalid
        ));
    }

    #[test]
    fn frustum_filter_preserves_order_and_keeps_missing_bounds() {
        let mut models = Assets::new();
        let bounded = model(&mut models);
        let unbounded = model(&mut models);
        let mut world = World::new();
        world.spawn((
            Model3d::new(bounded).with_render_order(0),
            Transform3d::from_translation([0.0, 0.0, 0.5]),
        ));
        world.spawn((
            Model3d::new(bounded).with_render_order(1),
            Transform3d::from_translation([4.0, 0.0, 0.5]),
        ));
        world.spawn((
            Model3d::new(unbounded).with_render_order(2),
            Transform3d::from_translation([100.0, 0.0, 0.5]),
        ));
        let extracted = extract_models(&mut world);
        let mut registry = ModelBoundsRegistry3d::new();
        registry.insert_model(
            bounded,
            LocalBoundingSphere3d::new([0.0; 3], 0.25).expect("valid sphere"),
        );

        let first = filter_extracted_models_by_frustum_3d(
            &extracted,
            &identity_zero_to_one_frustum(),
            &registry,
        )
        .expect("finite snapshot");
        let second = filter_extracted_models_by_frustum_3d(
            &extracted,
            &identity_zero_to_one_frustum(),
            &registry,
        )
        .expect("repeat is deterministic");
        assert_eq!(first, second);
        assert_eq!(
            first.stats(),
            FrustumCullingStats3d {
                input_draws: 3,
                tested_draws: 2,
                culled_draws: 1,
                visible_draws: 2,
                unbounded_draws: 1,
            }
        );
        assert_eq!(first.visible().model_count(), 2);
        assert_eq!(first.visible().batches()[0].model(), bounded);
        assert_eq!(first.visible().batches()[1].model(), unbounded);
        assert_eq!(first.visible().batches()[0].draws()[0].render_order, 0);
        assert_eq!(first.visible().batches()[1].draws()[0].render_order, 2);
    }

    #[test]
    fn mesh_frustum_filter_expands_whole_model_and_prunes_individual_meshes() {
        let near = MeshPrimitive::new(
            vec![[-0.5, -0.5, 0.5], [0.5, -0.5, 0.5], [0.0, 0.5, 0.5]],
            vec![0, 1, 2],
        )
        .expect("valid near triangle");
        let far = MeshPrimitive::new(
            vec![[9.5, -0.5, 0.5], [10.5, -0.5, 0.5], [10.0, 0.5, 0.5]],
            vec![0, 1, 2],
        )
        .expect("valid far triangle");
        let source = Model::new(
            vec![
                Mesh::new(Some("near".to_owned()), vec![near]).expect("near mesh"),
                Mesh::new(Some("far".to_owned()), vec![far]).expect("far mesh"),
            ],
            Vec::new(),
            Vec::new(),
        )
        .expect("valid two-mesh model");
        let mut models = Assets::new();
        let handle = models.insert(source);
        let mut world = World::new();
        world.spawn((Model3d::new(handle), Transform3d::IDENTITY));
        let extracted = extract_models(&mut world);
        let mut bounds = ModelBoundsRegistry3d::new();
        register_computed_model_bounds_3d(&mut bounds, &models, handle)
            .expect("finite per-mesh bounds");

        let filtered = filter_extracted_model_meshes_by_frustum_3d(
            &extracted,
            &identity_zero_to_one_frustum(),
            &models,
            &bounds,
        )
        .expect("valid mesh filtering");
        assert_eq!(
            filtered.draw_stats(),
            FrustumCullingStats3d {
                input_draws: 1,
                tested_draws: 1,
                culled_draws: 0,
                visible_draws: 1,
                unbounded_draws: 0,
            }
        );
        assert_eq!(
            filtered.mesh_stats(),
            MeshFrustumCullingStats3d {
                input_meshes: 2,
                tested_meshes: 2,
                exact_bound_meshes: 2,
                model_fallback_meshes: 0,
                unbounded_meshes: 0,
                culled_meshes: 1,
                visible_meshes: 1,
                expanded_mesh_draws: 1,
            }
        );
        assert_eq!(filtered.visible().model_count(), 1);
        assert_eq!(filtered.visible().batches().len(), 1);
        assert_eq!(filtered.visible().batches()[0].draws()[0].mesh, Some(0));
    }

    #[test]
    fn mesh_frustum_filter_reports_model_fallback_without_false_culling() {
        let mut models = Assets::new();
        let handle = model(&mut models);
        let mut world = World::new();
        world.spawn((Model3d::new(handle), Transform3d::IDENTITY));
        let extracted = extract_models(&mut world);
        let mut bounds = ModelBoundsRegistry3d::new();
        bounds.insert_model(
            handle,
            LocalAabb3d::new([-0.5; 3], [0.5; 3]).expect("valid model bounds"),
        );

        let filtered = filter_extracted_model_meshes_by_frustum_3d(
            &extracted,
            &identity_zero_to_one_frustum(),
            &models,
            &bounds,
        )
        .expect("fallback remains conservative");
        assert_eq!(filtered.mesh_stats().exact_bound_meshes, 0);
        assert_eq!(filtered.mesh_stats().model_fallback_meshes, 1);
        assert_eq!(filtered.mesh_stats().visible_meshes, 1);
        assert_eq!(filtered.visible().batches()[0].draws()[0].mesh, Some(0));
    }

    #[test]
    fn custom_bounds_lookup_runs_once_per_draw_in_snapshot_order() {
        let mut models = Assets::new();
        let handle = model(&mut models);
        let mut world = World::new();
        world.spawn((
            Model3d::new(handle).with_mesh(2).with_render_order(1),
            Transform3d::from_translation([0.0, 0.0, 0.5]),
        ));
        world.spawn((
            Model3d::new(handle).with_mesh(1).with_render_order(0),
            Transform3d::from_translation([0.0, 0.0, 0.5]),
        ));
        let extracted = extract_models(&mut world);
        let mut requested = Vec::new();
        filter_extracted_models_by_frustum_3d_with(
            &extracted,
            &identity_zero_to_one_frustum(),
            |key| {
                requested.push(key);
                Some(
                    LocalBoundingSphere3d::new([0.0; 3], 0.5)
                        .expect("static valid bounds")
                        .into(),
                )
            },
        )
        .expect("finite snapshot");
        assert_eq!(requested.len(), 2);
        assert_eq!(requested[0].mesh, Some(1));
        assert_eq!(requested[1].mesh, Some(2));
    }

    #[test]
    fn culling_reports_non_finite_draw_without_partial_output() {
        let mut models = Assets::new();
        let handle = model(&mut models);
        let extracted = ExtractedModels {
            batches: vec![ModelDrawBatch {
                model: handle,
                draws: vec![ModelDraw {
                    model: handle,
                    mesh: None,
                    model_matrix: [f32::NAN; 16],
                    render_order: 0,
                    overlay: false,
                }],
            }],
            model_count: 1,
        };
        let mut registry = ModelBoundsRegistry3d::new();
        registry.insert_model(
            handle,
            LocalBoundingSphere3d::new([0.0; 3], 1.0).expect("valid sphere"),
        );
        assert!(matches!(
            filter_extracted_models_by_frustum_3d(
                &extracted,
                &identity_zero_to_one_frustum(),
                &registry,
            ),
            Err(FrustumCullingError3d {
                draw_index: 0,
                source: FrustumIntersectionError3d::NonFiniteModelMatrix,
                ..
            })
        ));
    }

    #[test]
    fn spatial_primitives_reject_invalid_authoring_values() {
        assert_eq!(Plane3d::new([0.0; 3], 0.0), Err(Plane3dError::ZeroNormal));
        assert_eq!(
            LocalBoundingSphere3d::new([0.0; 3], -1.0),
            Err(LocalBounds3dError::NegativeRadius)
        );
        assert_eq!(
            LocalAabb3d::new([1.0, 0.0, 0.0], [0.0, 1.0, 1.0]),
            Err(LocalBounds3dError::InvertedAxis { axis: 0 })
        );
    }

    #[test]
    fn scene_bounds_resolve_exact_matrix_transform_and_return_camera_ready_values() {
        let mut models = Assets::new();
        let cube = model(&mut models);
        let mut world = World::new();
        world.spawn((
            Model3d::new(cube),
            LocalMatrixTransform3d::new([
                1.0, 0.0, 0.0, 0.0, // X
                0.0, 1.0, 0.0, 0.0, // Y
                0.0, 0.0, 1.0, 0.0, // Z
                5.0, 2.0, -3.0, 1.0, // translation
            ]),
        ));

        let result = scene_bounds_3d(&mut world, &models).expect("valid scene");
        let bounds = result.bounds().expect("cube is visible");
        assert_eq!(bounds.minimum(), [4.5, 1.5, -3.5]);
        assert_eq!(bounds.maximum(), [5.5, 2.5, -2.5]);
        assert_eq!(bounds.centre(), [5.0, 2.0, -3.0]);
        assert_eq!(bounds.size(), [1.0, 1.0, 1.0]);
        assert_eq!(bounds.entity_count(), 1);
        assert_eq!(bounds.vertex_count(), 36);
    }

    #[test]
    fn scene_bounds_distinguish_empty_scene_and_invalid_mesh_selection() {
        let mut models = Assets::new();
        let cube = model(&mut models);
        let mut empty_world = World::new();
        assert_eq!(
            scene_bounds_3d(&mut empty_world, &models).expect("empty is valid"),
            SceneBoundsResult3d::Empty
        );

        let mut world = World::new();
        world.spawn((Model3d::new(cube).with_mesh(1), Transform3d::IDENTITY));
        assert!(matches!(
            scene_bounds_3d(&mut world, &models),
            Err(SceneBoundsError3d::MissingMesh {
                mesh: 1,
                mesh_count: 1,
                ..
            })
        ));
    }

    #[test]
    fn static_scene_collision_applies_each_draw_world_transform() {
        let mut models = Assets::new();
        let cube = model(&mut models);
        let mut world = World::new();
        world.spawn((
            Model3d::new(cube),
            LocalTransform3d::from_translation([4.0, 2.0, -3.0]),
        ));

        let collider = build_static_scene_collider_3d(&mut world, &models)
            .expect("one transformed cube is valid static geometry");
        assert_eq!(collider.source_draw_count(), 1);
        assert_eq!(collider.triangle_count(), 12);
        assert!(
            collider
                .mesh()
                .triangles()
                .iter()
                .flatten()
                .all(|point| (3.5..=4.5).contains(&point.x))
        );
        assert!(
            collider
                .mesh()
                .triangles()
                .iter()
                .flatten()
                .all(|point| (1.5..=2.5).contains(&point.y))
        );
    }

    #[test]
    fn static_scene_collision_refuses_a_missing_model_instead_of_skipping_walls() {
        let mut loaded_models = Assets::new();
        let cube = model(&mut loaded_models);
        let mut world = World::new();
        world.spawn((Model3d::new(cube), Transform3d::IDENTITY));
        let extracted = extract_models(&mut world);

        assert!(matches!(
            build_static_scene_collider_3d_from_extracted(&extracted, &Assets::new()),
            Err(SceneCollisionError3d::MissingModel { model, .. }) if model == cube
        ));
    }

    #[test]
    fn static_scene_collision_keeps_multiple_instances_as_multiple_wall_groups() {
        let mut models = Assets::new();
        let cube = model(&mut models);
        let mut world = World::new();
        world.spawn((
            Model3d::new(cube),
            Transform3d::from_translation([-3.0, 0.0, 0.0]),
        ));
        world.spawn((
            Model3d::new(cube),
            Transform3d::from_translation([3.0, 0.0, 0.0]),
        ));
        let extracted = extract_models(&mut world);

        let collider = build_static_scene_collider_3d_from_extracted(&extracted, &models)
            .expect("reused model instances stay separate in world geometry");
        assert_eq!(collider.source_draw_count(), 2);
        assert_eq!(collider.triangle_count(), 24);
    }

    #[test]
    fn static_scene_collision_skips_degenerate_import_faces_but_keeps_valid_geometry() {
        let primitive = MeshPrimitive::new(
            vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0],
            ],
            vec![0, 1, 2, 0, 3, 1],
        )
        .expect("render geometry permits a degenerate face");
        let model = Model::new(
            vec![
                Mesh::new(Some("map".to_owned()), vec![primitive])
                    .expect("one primitive is a mesh"),
            ],
            Vec::new(),
            Vec::new(),
        )
        .expect("valid model resources");
        let mut models = Assets::new();
        let map = models.insert(model);
        let mut world = World::new();
        world.spawn((Model3d::new(map), Transform3d::IDENTITY));

        let collider = build_static_scene_collider_3d(&mut world, &models)
            .expect("a harmless degenerate render face must not reject the map");
        assert_eq!(collider.triangle_count(), 1);
        assert_eq!(collider.skipped_degenerate_triangle_count(), 1);
        assert_eq!(collider.source_draw_count(), 1);
    }

    #[test]
    fn filtered_static_collision_exposes_source_mesh_and_material_metadata() {
        let road = MeshPrimitive::new(
            vec![[-1.0, 0.0, -1.0], [1.0, 0.0, -1.0], [0.0, 0.0, 1.0]],
            vec![0, 1, 2],
        )
        .expect("road triangle")
        .with_material(MaterialIndex::new(0));
        let wall = MeshPrimitive::new(
            vec![[-1.0, 0.0, 0.0], [-1.0, 2.0, 0.0], [1.0, 0.0, 0.0]],
            vec![0, 1, 2],
        )
        .expect("wall triangle")
        .with_material(MaterialIndex::new(1));
        let model = Model::new(
            vec![
                Mesh::new(Some("street_mesh".to_owned()), vec![road]).expect("road mesh"),
                Mesh::new(Some("building_mesh".to_owned()), vec![wall]).expect("wall mesh"),
            ],
            vec![
                Material::new().with_name("road_surface"),
                Material::new().with_name("wall_surface"),
            ],
            Vec::new(),
        )
        .expect("semantic collision model");
        let mut models = Assets::new();
        let handle = models.insert(model);
        let draws = [StaticSceneCollisionDraw3d::new(
            7,
            handle,
            None,
            transform_matrix(Transform3d::IDENTITY),
        )];
        let mut seen = Vec::new();
        let collider = build_static_scene_collider_3d_from_draws_with(
            &draws,
            &models,
            SceneCollisionBuildLimits3d::default(),
            |source| {
                seen.push((
                    source.source_id,
                    source.mesh_name.map(str::to_owned),
                    source.material_name.map(str::to_owned),
                ));
                source.material_name == Some("road_surface")
            },
        )
        .expect("road layer contains one selected triangle");

        assert_eq!(collider.triangle_count(), 1);
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].0, 7);
        assert_eq!(seen[0].1.as_deref(), Some("street_mesh"));
        assert_eq!(seen[0].2.as_deref(), Some("road_surface"));
    }

    #[test]
    fn filtered_static_collision_enforces_explicit_triangle_limit() {
        let mut models = Assets::new();
        let cube = model(&mut models);
        let draws = [StaticSceneCollisionDraw3d::new(
            0,
            cube,
            None,
            transform_matrix(Transform3d::IDENTITY),
        )];
        let limits = SceneCollisionBuildLimits3d {
            maximum_triangles: 1,
            ..SceneCollisionBuildLimits3d::default()
        };
        assert!(matches!(
            build_static_scene_collider_3d_from_draws_with(&draws, &models, limits, |_| true,),
            Err(SceneCollisionError3d::LimitExceeded {
                resource: SceneCollisionLimitResource3d::Triangles,
                limit: 1,
            })
        ));
    }

    #[test]
    fn lod_selects_distance_bands_and_keeps_last_beyond_range() {
        let mut models = Assets::new();
        let near = model(&mut models);
        let far = model(&mut models);
        let lod = LodGroup3d::new(vec![
            LodLevel3d {
                model: near,
                max_distance: 5.0,
            },
            LodLevel3d {
                model: far,
                max_distance: 20.0,
            },
        ])
        .expect("valid bands");
        assert_eq!(lod.select(5.0).expect("valid distance"), near);
        assert_eq!(lod.select(5.1).expect("valid distance"), far);
        assert_eq!(lod.select(999.0).expect("valid distance"), far);
    }

    #[test]
    fn lod_rejects_invalid_ranges() {
        let mut models = Assets::new();
        let handle = model(&mut models);
        assert!(matches!(
            LodGroup3d::new(vec![]),
            Err(LodConfigError::NoLevels)
        ));
        assert!(matches!(
            LodGroup3d::new(vec![LodLevel3d {
                model: handle,
                max_distance: -1.0
            }]),
            Err(LodConfigError::InvalidDistance { .. })
        ));
        assert!(matches!(
            LodGroup3d::new(vec![
                LodLevel3d {
                    model: handle,
                    max_distance: 2.0
                },
                LodLevel3d {
                    model: handle,
                    max_distance: 2.0
                }
            ]),
            Err(LodConfigError::NonIncreasingDistance { .. })
        ));
    }

    #[test]
    fn lod_extraction_is_deterministic_by_render_order_then_entity() {
        let mut models = Assets::new();
        let near = model(&mut models);
        let far = model(&mut models);
        let lod = LodGroup3d::new(vec![LodLevel3d {
            model: far,
            max_distance: 100.0,
        }])
        .expect("valid");
        let mut world = World::new();
        let first = world
            .spawn((
                Model3d::new(near).with_render_order(1),
                Transform3d::from_translation([1.0, 0.0, 0.0]),
                lod.clone(),
            ))
            .id();
        let before = world
            .spawn((
                Model3d::new(near).with_render_order(0),
                Transform3d::IDENTITY,
            ))
            .id();
        let second = world
            .spawn((
                Model3d::new(near).with_render_order(1),
                Transform3d::from_translation([2.0, 0.0, 0.0]),
                lod,
            ))
            .id();
        let extracted = extract_lod_models_3d(&mut world, [0.0; 3]).expect("finite camera");
        assert_eq!(extracted.draws()[0].entity, before);
        let mut expected = [first.to_bits(), second.to_bits()];
        expected.sort_unstable();
        assert_eq!(extracted.draws()[1].entity.to_bits(), expected[0]);
        assert_eq!(extracted.draws()[2].entity.to_bits(), expected[1]);
        assert_eq!(extracted.draws()[1].model, far);
    }

    #[test]
    fn high_level_lod_extraction_returns_renderer_batches() {
        let mut models = Assets::new();
        let source = model(&mut models);
        let replacement = model(&mut models);
        let lod = LodGroup3d::new(vec![LodLevel3d {
            model: replacement,
            max_distance: 10.0,
        }])
        .expect("valid LOD");
        let mut world = World::new();
        world.spawn((
            Model3d::new(source).with_mesh(0),
            Transform3d::from_translation([2.0, 0.0, 0.0]),
            lod,
        ));

        let extracted = extract_models_with_lod_3d(&mut world, [0.0; 3])
            .expect("finite camera and authored LOD");
        assert_eq!(extracted.model_count(), 1);
        assert_eq!(extracted.batches()[0].model(), replacement);
        assert_eq!(extracted.batches()[0].draws()[0].mesh, None);
    }

    #[test]
    fn high_level_lod_extraction_preserves_mesh_without_lod_group() {
        let mut models = Assets::new();
        let source = model(&mut models);
        let mut world = World::new();
        world.spawn((Model3d::new(source).with_mesh(7), Transform3d::IDENTITY));

        let extracted = extract_models_with_lod_3d(&mut world, [0.0; 3])
            .expect("finite camera without authored LOD");

        assert_eq!(extracted.model_count(), 1);
        assert_eq!(extracted.batches()[0].model(), source);
        assert_eq!(extracted.batches()[0].draws()[0].mesh, Some(7));
    }

    #[test]
    fn extraction_sorts_by_order_then_entity() {
        let mut models = Assets::new();
        let model = model(&mut models);
        let mut world = World::new();
        let first = world
            .spawn((
                Model3d::new(model).with_render_order(1),
                Transform3d::from_translation([20.0, 0.0, 0.0]),
            ))
            .id();
        world.spawn((
            Model3d::new(model).with_render_order(0),
            Transform3d::from_translation([10.0, 0.0, 0.0]),
        ));
        let last = world
            .spawn((
                Model3d::new(model).with_render_order(1),
                Transform3d::from_translation([30.0, 0.0, 0.0]),
            ))
            .id();

        let extracted = extract_models(&mut world);
        assert_eq!(extracted.model_count(), 3);
        assert_eq!(extracted.batches().len(), 1);
        let draws = extracted.batches()[0].draws();
        let mut expected = [
            (first.to_bits(), [20.0, 0.0, 0.0]),
            (last.to_bits(), [30.0, 0.0, 0.0]),
        ];
        expected.sort_by_key(|(entity_bits, _)| *entity_bits);

        assert_eq!(&draws[0].model_matrix[12..15], [10.0, 0.0, 0.0]);
        assert_eq!(&draws[1].model_matrix[12..15], expected[0].1);
        assert_eq!(&draws[2].model_matrix[12..15], expected[1].1);
    }

    #[test]
    fn extraction_skips_hidden_or_transformless_entities() {
        let mut models = Assets::new();
        let model = model(&mut models);
        let mut world = World::new();
        world.spawn((
            Model3d::new(model).with_visible(false),
            Transform3d::IDENTITY,
        ));
        world.spawn(Model3d::new(model));

        let extracted = extract_models(&mut world);
        assert!(extracted.is_empty());
        assert!(extracted.batches().is_empty());
    }

    #[test]
    fn extraction_and_lod_skip_render_flags_nodraw_even_when_visible() {
        let mut models = Assets::new();
        let model = model(&mut models);
        let mut world = World::new();
        world.spawn((
            Model3d::new(model),
            RenderFlags3d::NODRAW,
            Transform3d::IDENTITY,
        ));
        world.spawn((Model3d::new(model), Transform3d::IDENTITY));

        assert_eq!(extract_models(&mut world).model_count(), 1);
        let lod = extract_models_with_lod_3d(&mut world, [0.0; 3]).expect("lod extract");
        assert_eq!(lod.model_count(), 1);
    }

    #[test]
    fn extraction_keeps_non_adjacent_model_batches_separate() {
        let mut models = Assets::new();
        let first = model(&mut models);
        let second = model(&mut models);
        let mut world = World::new();
        world.spawn((
            Model3d::new(first).with_render_order(0),
            Transform3d::IDENTITY,
        ));
        world.spawn((
            Model3d::new(second).with_render_order(1),
            Transform3d::IDENTITY,
        ));
        world.spawn((
            Model3d::new(first).with_render_order(2),
            Transform3d::IDENTITY,
        ));

        let extracted = extract_models(&mut world);
        let batches = extracted.batches();
        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0].model(), first);
        assert_eq!(batches[1].model(), second);
        assert_eq!(batches[2].model(), first);
    }

    #[test]
    fn directional_light_normalizes_and_validates_authoring_values() {
        let light = DirectionalLight3d::new([0.0, -2.0, 0.0], [1.0, 0.5, 0.25], 10.0)
            .expect("finite non-zero light is valid");
        assert_eq!(light.direction(), [0.0, -1.0, 0.0]);
        assert!(matches!(
            DirectionalLight3d::new([0.0; 3], [1.0; 3], 1.0),
            Err(DirectionalLightError::ZeroDirection)
        ));
        assert!(matches!(
            DirectionalLight3d::new([0.0, -1.0, 0.0], [-1.0, 1.0, 1.0], 1.0),
            Err(DirectionalLightError::NegativeColor)
        ));
        assert!(matches!(
            DirectionalLight3d::new([0.0, -1.0, 0.0], [1.0; 3], f32::NAN),
            Err(DirectionalLightError::NonFiniteIlluminance)
        ));
    }

    #[test]
    fn directional_light_extraction_is_stable_and_skips_disabled_lights() {
        let mut world = World::new();
        let red = world
            .spawn(
                DirectionalLight3d::new([0.0, -1.0, 0.0], [1.0, 0.0, 0.0], 3.0)
                    .expect("valid red light"),
            )
            .id();
        world.spawn(
            DirectionalLight3d::new([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], 4.0)
                .expect("valid green light")
                .with_enabled(false),
        );
        let blue = world
            .spawn(
                DirectionalLight3d::new([0.0, 0.0, -1.0], [0.0, 0.0, 1.0], 5.0)
                    .expect("valid blue light"),
            )
            .id();

        let first = extract_directional_lights(&mut world);
        let second = extract_directional_lights(&mut world);
        let mut expected = [
            (red.to_bits(), [1.0, 0.0, 0.0]),
            (blue.to_bits(), [0.0, 0.0, 1.0]),
        ];
        expected.sort_by_key(|(entity_bits, _)| *entity_bits);
        assert_eq!(first, second);
        assert_eq!(first.len(), 2);
        assert_eq!(first.lights()[0].color, expected[0].1);
        assert_eq!(first.lights()[1].color, expected[1].1);
    }

    #[test]
    fn set_parent_promotes_world_transform3d_into_local_for_propagation() {
        let mut world = World::new();
        let parent = world
            .spawn(Transform3d {
                translation: [10.0, 0.0, 0.0],
                rotation: [0.0, 0.0, 0.0, 1.0],
                scale: [1.0, 1.0, 1.0],
            })
            .id();
        let child = world
            .spawn(LocalTransform3d::from_translation([1.0, 0.0, 0.0]))
            .id();
        set_parent_3d(&mut world, child, parent).expect("promotes parent Transform3d");
        assert!(world.get::<LocalTransform3d>(parent).is_some());
        propagate_world_transforms(&mut world).expect("hierarchy propagates");
        assert_eq!(
            world
                .get::<WorldTransform3d>(child)
                .expect("child world")
                .translation(),
            [11.0, 0.0, 0.0]
        );
    }

    #[test]
    fn hierarchy_propagation_syncs_world_and_legacy_render_transform() {
        let mut world = World::new();
        let root = world
            .spawn(LocalTransform3d::from_translation([2.0, 0.0, 0.0]))
            .id();
        let child = world
            .spawn(LocalTransform3d::from_translation([1.0, 3.0, 0.0]))
            .id();
        set_parent_3d(&mut world, child, root).expect("valid rooted hierarchy");

        let stats = propagate_world_transforms(&mut world).expect("valid propagation");
        assert_eq!(stats.updated, 2);
        assert_eq!(stats.roots, 1);
        let world_transform = *world
            .get::<WorldTransform3d>(child)
            .expect("world transform is written");
        assert_eq!(world_transform.translation(), [3.0, 3.0, 0.0]);
        assert_eq!(
            world
                .get::<Transform3d>(child)
                .expect("legacy renderer transform is synchronized")
                .translation,
            [3.0, 3.0, 0.0]
        );
    }

    #[test]
    fn matrix_hierarchy_multiplies_exactly_and_extracts_the_result() {
        let mut models = Assets::new();
        let model = model(&mut models);
        let mut world = World::new();
        let parent = world
            .spawn(LocalMatrixTransform3d::new([
                1.0, 0.0, 0.0, 0.0, // X basis
                0.5, 1.0, 0.0, 0.0, // shear X by Y
                0.0, 0.0, 1.0, 0.0, // Z basis
                2.0, 0.0, 0.0, 1.0, // translation
            ]))
            .id();
        let child = world
            .spawn((
                LocalMatrixTransform3d::new([
                    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 4.0, 0.0, 1.0,
                ]),
                Model3d::new(model),
            ))
            .id();
        set_parent_3d(&mut world, child, parent).expect("valid matrix hierarchy");

        propagate_world_transforms(&mut world).expect("finite affine propagation");
        let world_transform = *world
            .get::<WorldTransform3d>(child)
            .expect("exact world transform");
        assert_eq!(world_transform.translation(), [4.0, 4.0, 0.0]);
        assert!(
            world_transform.as_trs().is_none(),
            "shear is not approximated"
        );
        assert!(world.get::<Transform3d>(child).is_none());

        let draws = extract_models(&mut world);
        assert_eq!(draws.model_count(), 1);
        assert_eq!(
            draws.batches()[0].draws()[0].model_matrix,
            world_transform.column_major()
        );
    }

    #[test]
    fn propagation_rejects_conflicting_local_component_kinds_transactionally() {
        let mut world = World::new();
        let entity = world
            .spawn((
                LocalTransform3d::IDENTITY,
                LocalMatrixTransform3d::new([
                    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
                ]),
            ))
            .id();
        assert!(matches!(
            propagate_world_transforms(&mut world),
            Err(TransformHierarchyError::ConflictingLocalTransforms { entity: actual }) if actual == entity
        ));
        assert!(world.get::<WorldTransform3d>(entity).is_none());
    }

    #[test]
    fn propagation_rejects_cycles_without_partially_overwriting_outputs() {
        let mut world = World::new();
        let root = world.spawn(LocalTransform3d::IDENTITY).id();
        propagate_world_transforms(&mut world).expect("root propagates first");
        let original = *world
            .get::<WorldTransform3d>(root)
            .expect("initial output exists");

        let child = world.spawn(LocalTransform3d::IDENTITY).id();
        world.entity_mut(root).insert(Parent3d::new(child));
        world.entity_mut(child).insert(Parent3d::new(root));
        assert!(matches!(
            propagate_world_transforms(&mut world),
            Err(TransformHierarchyError::Cycle { .. })
        ));
        assert_eq!(world.get::<WorldTransform3d>(root), Some(&original));
        assert!(world.get::<WorldTransform3d>(child).is_none());
    }

    #[test]
    fn parent_authoring_rejects_self_and_clear_is_explicit() {
        let mut world = World::new();
        let parent = world.spawn(LocalTransform3d::IDENTITY).id();
        let child = world.spawn(LocalTransform3d::IDENTITY).id();
        assert!(matches!(
            set_parent_3d(&mut world, child, child),
            Err(TransformHierarchyError::SelfParent { .. })
        ));
        set_parent_3d(&mut world, child, parent).expect("valid parent edge");
        assert!(clear_parent_3d(&mut world, child).expect("child exists"));
        assert!(!clear_parent_3d(&mut world, child).expect("child still exists"));
    }
}
