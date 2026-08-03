//! Lightweight deterministic physics primitives for 2D and 3D prototypes.
//!
//! This is intentionally a small, inspectable foundation rather than a hidden
//! general-purpose solver. It integrates velocity, detects circle/sphere
//! overlap and supplies ECS components for games which need a first playable
//! slice. It has no rotation, joints, continuous collision detection, spatial
//! acceleration structure, or network authority model yet.
//! The 3D ECS slice adds validated sphere bodies, deterministic overlap events,
//! and finite ray/sphere queries suitable for a gameplay interaction adapter.
//! It also provides axis-aligned box query colliders. Boxes are detection-only:
//! there is no collision response, OBB rotation, mesh collision, CCD or broad
//! phase.
//! The 2D query slice mirrors those AABB guarantees for tile and top-down
//! gameplay: validated boxes, strict contacts, bounded deterministic ECS
//! overlap queries, and finite 2D ray/AABB tests.
//! It remains detection-only: response and trigger semantics belong to the
//! caller.
//!
//! Dynamic rigid bodies ship behind the M4 [`backend`] facade (`rapier` / `rapier2d`
//! features). Playable 3D character motion continues to use [`TriangleMesh3d`]
//! static queries.
//!
//! ```
//! use yuyib_physics::{Body2d, Circle, Vec2, collide_circles};
//!
//! let left = Body2d::new(Vec2::ZERO, Vec2::new(1.0, 0.0));
//! let right = Body2d::stationary(Vec2::new(1.5, 0.0));
//! assert!(collide_circles(left.position, Circle::new(1.0).unwrap(),
//!                         right.position, Circle::new(1.0).unwrap()).is_some());
//! ```

#![forbid(unsafe_code)]

mod backend;

pub use backend::{
    BodyId2d, BodyId3d, CharacterMoveConfig2d, CharacterMoveResult2d, CollisionGroups2d,
    CollisionGroups3d, ContactPair2d, ContactPair3d, DynamicsBackend2d, DynamicsBackend3d,
    DynamicsBackendError2d, DynamicsBackendError3d, DynamicsFixedStepper2d, DynamicsFixedStepper3d,
    DynamicsWorldConfig2d, DynamicsWorldConfig3d, JointId2d, JointId3d,
};

#[cfg(feature = "rapier")]
pub use backend::RapierDynamicsWorld3d;

#[cfg(feature = "rapier2d")]
pub use backend::RapierDynamicsWorld2d;

use std::fmt;

use yuyib_ecs::{bevy_ecs::entity::Entity, prelude::*};

/// A finite two-dimensional vector in world units.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec2 {
    /// Horizontal component.
    pub x: f32,
    /// Vertical component.
    pub y: f32,
}

impl Vec2 {
    /// The zero vector.
    pub const ZERO: Self = Self { x: 0.0, y: 0.0 };

    /// Creates a vector.
    #[must_use]
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    /// Returns the squared length.
    #[must_use]
    pub fn length_squared(self) -> f32 {
        self.x.mul_add(self.x, self.y * self.y)
    }

    /// Returns a unit vector, or [`Self::ZERO`] for a zero/non-finite vector.
    #[must_use]
    pub fn normalized_or_zero(self) -> Self {
        let length_squared = self.length_squared();
        if !length_squared.is_finite() || length_squared <= f32::EPSILON {
            return Self::ZERO;
        }
        let inverse = length_squared.sqrt().recip();
        Self::new(self.x * inverse, self.y * inverse)
    }
}

impl std::ops::Add for Vec2 {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self::new(self.x + other.x, self.y + other.y)
    }
}

impl std::ops::Sub for Vec2 {
    type Output = Self;

    fn sub(self, other: Self) -> Self {
        Self::new(self.x - other.x, self.y - other.y)
    }
}

impl std::ops::Mul<f32> for Vec2 {
    type Output = Self;

    fn mul(self, scalar: f32) -> Self {
        Self::new(self.x * scalar, self.y * scalar)
    }
}

/// A finite three-dimensional vector in world units.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec3 {
    /// Horizontal component.
    pub x: f32,
    /// Vertical component.
    pub y: f32,
    /// Depth component.
    pub z: f32,
}

impl Vec3 {
    /// The zero vector.
    pub const ZERO: Self = Self {
        x: 0.0,
        y: 0.0,
        z: 0.0,
    };

    /// Creates a vector.
    #[must_use]
    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }

    /// Returns the squared length.
    #[must_use]
    pub fn length_squared(self) -> f32 {
        self.x
            .mul_add(self.x, self.y.mul_add(self.y, self.z * self.z))
    }

    /// Returns a unit vector, or [`Self::ZERO`] for a zero/non-finite vector.
    #[must_use]
    pub fn normalized_or_zero(self) -> Self {
        let length_squared = self.length_squared();
        if !length_squared.is_finite() || length_squared <= f32::EPSILON {
            return Self::ZERO;
        }
        let inverse = length_squared.sqrt().recip();
        Self::new(self.x * inverse, self.y * inverse, self.z * inverse)
    }
}

impl std::ops::Add for Vec3 {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self::new(self.x + other.x, self.y + other.y, self.z + other.z)
    }
}

impl std::ops::Sub for Vec3 {
    type Output = Self;

    fn sub(self, other: Self) -> Self {
        Self::new(self.x - other.x, self.y - other.y, self.z - other.z)
    }
}

impl std::ops::Mul<f32> for Vec3 {
    type Output = Self;

    fn mul(self, scalar: f32) -> Self {
        Self::new(self.x * scalar, self.y * scalar, self.z * scalar)
    }
}

/// Static collision geometry made of explicit triangles.
///
/// This is the low-level building block for imported maps: it retains exact
/// triangle faces instead of approximating a corridor with one large box.
/// Build it directly from vertices/indices or let a higher-level scene adapter
/// collect triangles from imported models. Queries retain deterministic source
/// order and allocate nothing. A deterministic static BVH over contiguous
/// source ranges prunes unrelated map regions before their per-triangle bounds
/// or exact geometry are tested. Keeping leaves in source order is important:
/// sequential penetration resolution therefore produces the same result as a
/// linear source scan while large maps avoid paying its full cost.
#[derive(Clone, Debug, PartialEq)]
pub struct TriangleMesh3d {
    triangles: Vec<[Vec3; 3]>,
    bounds: Vec<TriangleAabb3d>,
    bvh: Vec<TriangleBvhNode3d>,
    acceleration: TriangleMeshAccelerationStats3d,
}

const TRIANGLE_BVH_LEAF_CAPACITY: usize = 8;
const TRIANGLE_BVH_STACK_CAPACITY: usize = 64;

/// Conservative bounds cached for a source triangle.
#[derive(Clone, Copy, Debug, PartialEq)]
struct TriangleAabb3d {
    minimum: Vec3,
    maximum: Vec3,
}

/// One node in a balanced source-range BVH.
///
/// Branch children and leaf triangle ranges use compact `u32` indices. This
/// keeps the flat tree near 11 MiB for a 1.25-million-triangle map instead of
/// duplicating vertices or allocating pointer-heavy nodes. Tree depth is
/// bounded logarithmically by construction rather than by input shape.
#[derive(Clone, Copy, Debug, PartialEq)]
struct TriangleBvhNode3d {
    bounds: TriangleAabb3d,
    kind: TriangleBvhNodeKind3d,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TriangleBvhNodeKind3d {
    Leaf { first: u32, count: u32 },
    Branch { left: u32, right: u32 },
}

impl TriangleMesh3d {
    /// Creates a triangle mesh from an indexed triangle list.
    ///
    /// # Errors
    ///
    /// Returns [`TriangleMeshError`] for empty/non-triangle index data,
    /// invalid indices, non-finite vertices or degenerate faces.
    pub fn from_indexed(vertices: &[Vec3], indices: &[u32]) -> Result<Self, TriangleMeshError> {
        if indices.is_empty() || !indices.len().is_multiple_of(3) {
            return Err(TriangleMeshError::InvalidIndexCount {
                actual: indices.len(),
            });
        }
        let triangle_count = indices.len() / 3;
        let leaf_count = triangle_count.div_ceil(TRIANGLE_BVH_LEAF_CAPACITY);
        let maximum_nodes = leaf_count.saturating_mul(2).saturating_sub(1);
        if triangle_count > u32::MAX as usize || maximum_nodes > u32::MAX as usize {
            return Err(TriangleMeshError::TooManyTriangles {
                actual: triangle_count,
                maximum: u32::MAX as usize,
            });
        }
        let mut triangles = Vec::with_capacity(triangle_count);
        let mut bounds = Vec::with_capacity(triangle_count);
        let (chunks, []) = indices.as_chunks::<3>() else {
            return Err(TriangleMeshError::InvalidIndexCount {
                actual: indices.len(),
            });
        };
        for (triangle, chunk) in chunks.iter().enumerate() {
            let mut face = [Vec3::ZERO; 3];
            for (corner, index) in chunk.iter().copied().enumerate() {
                face[corner] = *vertices
                    .get(index as usize)
                    .ok_or(TriangleMeshError::IndexOutOfBounds { triangle, index })?;
                if !finite_vec3(face[corner]) {
                    return Err(TriangleMeshError::NonFiniteVertex { triangle, corner });
                }
            }
            if cross3(face[1] - face[0], face[2] - face[0]).length_squared() <= f32::EPSILON {
                return Err(TriangleMeshError::DegenerateTriangle { triangle });
            }
            bounds.push(TriangleAabb3d::from_face(face));
            triangles.push(face);
        }
        let mut bvh = Vec::with_capacity(maximum_nodes);
        let mut acceleration = TriangleMeshAccelerationStats3d {
            triangles: triangles.len(),
            leaf_capacity: TRIANGLE_BVH_LEAF_CAPACITY,
            ..TriangleMeshAccelerationStats3d::default()
        };
        build_triangle_bvh(&bounds, 0, bounds.len(), 1, &mut bvh, &mut acceleration);
        acceleration.nodes = bvh.len();
        acceleration.bvh_bytes = bvh
            .len()
            .saturating_mul(std::mem::size_of::<TriangleBvhNode3d>());
        Ok(Self {
            triangles,
            bounds,
            bvh,
            acceleration,
        })
    }

    /// Returns immutable faces in source order.
    #[must_use]
    pub fn triangles(&self) -> &[[Vec3; 3]] {
        &self.triangles
    }

    /// Returns immutable construction-time acceleration metrics.
    #[must_use]
    pub const fn acceleration_stats(&self) -> TriangleMeshAccelerationStats3d {
        self.acceleration
    }

    /// Intersects a finite ray with this static triangle mesh.
    ///
    /// This is the low-level exact query used by editor picking, placement and
    /// custom game physics.  It checks both sides of a face, keeps the source
    /// triangle order as its tie-breaker, and deliberately allocates nothing.
    /// The mesh's immutable BVH is traversed near-first and pruned by both the
    /// caller's maximum distance and the nearest exact hit found so far.
    ///
    /// # Errors
    ///
    /// Returns [`PhysicsConfigError::InvalidRaycastDistance`] when
    /// `max_distance` is negative, infinite or NaN.
    pub fn raycast(
        &self,
        ray: Ray3d,
        max_distance: f32,
    ) -> Result<Option<TriangleMeshRayHit3d>, PhysicsConfigError> {
        Ok(self.raycast_with_stats(ray, max_distance)?.hit)
    }

    /// Intersects a finite ray and returns broad-phase work counters.
    ///
    /// This performs the same query as [`Self::raycast`]. The extra counters
    /// are intended for profiling, regression tests and runtime diagnostics;
    /// collecting them does not allocate.
    ///
    /// # Errors
    ///
    /// Returns [`PhysicsConfigError::InvalidRaycastDistance`] under the same
    /// conditions as [`Self::raycast`].
    pub fn raycast_with_stats(
        &self,
        ray: Ray3d,
        max_distance: f32,
    ) -> Result<TriangleMeshRaycastResult3d, PhysicsConfigError> {
        validate_raycast_distance(max_distance)?;
        let mut nearest = None;
        let mut stats = TriangleMeshQueryStats3d::default();
        let mut stack = [0_u32; TRIANGLE_BVH_STACK_CAPACITY];
        let mut stack_len = 1;
        while stack_len > 0 {
            stack_len -= 1;
            let node_index = triangle_bvh_index(stack[stack_len]);
            let node = self.bvh[node_index];
            stats.nodes_visited += 1;
            let nearest_distance = nearest
                .as_ref()
                .map_or(max_distance, |hit: &TriangleMeshRayHit3d| hit.distance);
            let Some(_node_entry) = node.bounds.ray_entry_distance(ray, nearest_distance) else {
                stats.nodes_pruned += 1;
                continue;
            };
            match node.kind {
                TriangleBvhNodeKind3d::Leaf { first, count } => {
                    stats.leaves_visited += 1;
                    let first = triangle_bvh_index(first);
                    let count = triangle_bvh_index(count);
                    for triangle in first..first + count {
                        stats.triangle_bounds_tested += 1;
                        let nearest_distance = nearest
                            .as_ref()
                            .map_or(max_distance, |hit: &TriangleMeshRayHit3d| hit.distance);
                        if self.bounds[triangle]
                            .ray_entry_distance(ray, nearest_distance)
                            .is_none()
                        {
                            continue;
                        }
                        stats.exact_triangles_tested += 1;
                        let face = self.triangles[triangle];
                        let Some(distance) = ray_triangle_distance(ray, face) else {
                            continue;
                        };
                        if distance > max_distance {
                            continue;
                        }
                        let normal =
                            cross3(face[1] - face[0], face[2] - face[0]).normalized_or_zero();
                        let hit = TriangleMeshRayHit3d {
                            triangle,
                            distance,
                            position: ray.point_at(distance),
                            normal,
                        };
                        if nearest.as_ref().is_none_or(|current| {
                            match distance.total_cmp(&current.distance) {
                                std::cmp::Ordering::Less => true,
                                std::cmp::Ordering::Equal => triangle < current.triangle,
                                std::cmp::Ordering::Greater => false,
                            }
                        }) {
                            nearest = Some(hit);
                        }
                    }
                }
                TriangleBvhNodeKind3d::Branch { left, right } => {
                    let left_index = triangle_bvh_index(left);
                    let right_index = triangle_bvh_index(right);
                    let limit = nearest
                        .as_ref()
                        .map_or(max_distance, |hit: &TriangleMeshRayHit3d| hit.distance);
                    let left_entry = self.bvh[left_index].bounds.ray_entry_distance(ray, limit);
                    let right_entry = self.bvh[right_index].bounds.ray_entry_distance(ray, limit);
                    match (left_entry, right_entry) {
                        (Some(left_distance), Some(right_distance)) => {
                            let (near, far) = if left_distance <= right_distance {
                                (left, right)
                            } else {
                                (right, left)
                            };
                            push_triangle_bvh_stack(&mut stack, &mut stack_len, far);
                            push_triangle_bvh_stack(&mut stack, &mut stack_len, near);
                        }
                        (Some(_), None) => {
                            stats.nodes_pruned += 1;
                            push_triangle_bvh_stack(&mut stack, &mut stack_len, left);
                        }
                        (None, Some(_)) => {
                            stats.nodes_pruned += 1;
                            push_triangle_bvh_stack(&mut stack, &mut stack_len, right);
                        }
                        (None, None) => {
                            stats.nodes_pruned += 2;
                        }
                    }
                }
            }
        }
        Ok(TriangleMeshRaycastResult3d {
            hit: nearest,
            stats,
        })
    }

    /// Resolves a sphere already moved to `position` out of this static mesh.
    ///
    /// Contacts are iterated in source order, yielding stable prototype
    /// movement. This is discrete penetration resolution, not CCD: callers
    /// must keep a character's fixed time step and speed bounded.
    ///
    /// Walkable ground uses [`default_max_walkable_slope_radians`]. Prefer
    /// [`Self::resolve_sphere_with_slope`] when the game needs a custom max
    /// slope.
    ///
    /// # Errors
    ///
    /// Returns [`TriangleMeshQueryError`] for a non-finite position, invalid
    /// radius, or an invalid iteration budget.
    pub fn resolve_sphere(
        &self,
        position: Vec3,
        radius: f32,
        iterations: usize,
    ) -> Result<SphereMeshResolution3d, TriangleMeshQueryError> {
        self.resolve_sphere_with_slope(
            position,
            radius,
            iterations,
            default_max_walkable_slope_radians(),
        )
    }

    /// Like [`Self::resolve_sphere`], but treats contacts as walkable ground
    /// only when the contact normal is within `max_slope_radians` of world up
    /// (`normal.y >= cos(max_slope_radians)`).
    ///
    /// # Errors
    ///
    /// Returns [`TriangleMeshQueryError`] for invalid sphere input, a zero
    /// iteration budget, or an invalid slope angle.
    pub fn resolve_sphere_with_slope(
        &self,
        position: Vec3,
        radius: f32,
        iterations: usize,
        max_slope_radians: f32,
    ) -> Result<SphereMeshResolution3d, TriangleMeshQueryError> {
        Ok(self
            .resolve_sphere_with_slope_and_stats(position, radius, iterations, max_slope_radians)?
            .resolution)
    }

    /// Resolves a sphere and returns deterministic BVH traversal counters.
    ///
    /// This has identical contact order and output to [`Self::resolve_sphere`]
    /// and performs no per-query allocation.
    ///
    /// # Errors
    ///
    /// Returns [`TriangleMeshQueryError`] under the same conditions as
    /// [`Self::resolve_sphere`].
    pub fn resolve_sphere_with_stats(
        &self,
        position: Vec3,
        radius: f32,
        iterations: usize,
    ) -> Result<TriangleMeshSphereResolutionResult3d, TriangleMeshQueryError> {
        self.resolve_sphere_with_slope_and_stats(
            position,
            radius,
            iterations,
            default_max_walkable_slope_radians(),
        )
    }

    /// Like [`Self::resolve_sphere_with_stats`] with an explicit max walkable slope.
    ///
    /// # Errors
    ///
    /// Returns [`TriangleMeshQueryError`] under the same conditions as
    /// [`Self::resolve_sphere_with_slope`].
    pub fn resolve_sphere_with_slope_and_stats(
        &self,
        position: Vec3,
        radius: f32,
        iterations: usize,
        max_slope_radians: f32,
    ) -> Result<TriangleMeshSphereResolutionResult3d, TriangleMeshQueryError> {
        if !finite_vec3(position) {
            return Err(TriangleMeshQueryError::NonFinitePosition);
        }
        if !radius.is_finite() || radius <= 0.0 {
            return Err(TriangleMeshQueryError::InvalidRadius(radius));
        }
        if iterations == 0 {
            return Err(TriangleMeshQueryError::InvalidIterations);
        }
        let min_walkable_normal_y = min_walkable_normal_y_from_slope(max_slope_radians)?;
        let mut resolved = position;
        let mut ground_contact = false;
        let mut contacts = 0;
        let mut stats = TriangleMeshQueryStats3d::default();
        let mut completed_iterations = 0;
        for _ in 0..iterations {
            completed_iterations += 1;
            let mut changed = false;
            let mut stack = [0_u32; TRIANGLE_BVH_STACK_CAPACITY];
            let mut stack_len = 1;
            while stack_len > 0 {
                stack_len -= 1;
                let node = self.bvh[triangle_bvh_index(stack[stack_len])];
                stats.nodes_visited += 1;
                if !node.bounds.overlaps_sphere(resolved, radius) {
                    stats.nodes_pruned += 1;
                    continue;
                }
                match node.kind {
                    TriangleBvhNodeKind3d::Leaf { first, count } => {
                        stats.leaves_visited += 1;
                        let first = triangle_bvh_index(first);
                        let count = triangle_bvh_index(count);
                        for triangle in first..first + count {
                            stats.triangle_bounds_tested += 1;
                            if !self.bounds[triangle].overlaps_sphere(resolved, radius) {
                                continue;
                            }
                            stats.exact_triangles_tested += 1;
                            let face = self.triangles[triangle];
                            let closest = closest_point_on_triangle(resolved, face);
                            let offset = resolved - closest;
                            let distance_squared = offset.length_squared();
                            if distance_squared >= radius * radius {
                                continue;
                            }
                            let normal = if distance_squared > f32::EPSILON {
                                offset * distance_squared.sqrt().recip()
                            } else {
                                cross3(face[1] - face[0], face[2] - face[0]).normalized_or_zero()
                            };
                            let distance = distance_squared.sqrt();
                            resolved = resolved + normal * (radius - distance + 0.0005);
                            ground_contact |= normal.y >= min_walkable_normal_y;
                            contacts += 1;
                            changed = true;
                        }
                    }
                    TriangleBvhNodeKind3d::Branch { left, right } => {
                        // Right is pushed first so contiguous leaves are always
                        // processed in exact ascending source-triangle order.
                        push_triangle_bvh_stack(&mut stack, &mut stack_len, right);
                        push_triangle_bvh_stack(&mut stack, &mut stack_len, left);
                    }
                }
            }
            if !changed {
                break;
            }
        }
        Ok(TriangleMeshSphereResolutionResult3d {
            resolution: SphereMeshResolution3d {
                position: resolved,
                ground_contact,
                contacts,
            },
            stats,
            completed_iterations,
        })
    }
}

/// Exact hit returned by [`TriangleMesh3d::raycast`].
///
/// The normal follows the source triangle winding.  The ray itself is
/// two-sided, therefore games that need a walkable floor should explicitly
/// require `normal.y` to be positive enough rather than assuming every hit is
/// ground.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TriangleMeshRayHit3d {
    /// Zero-based source triangle index.
    pub triangle: usize,
    /// Distance from the normalized ray origin.
    pub distance: f32,
    /// World-space hit position.
    pub position: Vec3,
    /// Unit geometric normal in source-winding direction.
    pub normal: Vec3,
}

/// Immutable size and shape metrics for a triangle mesh's static BVH.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TriangleMeshAccelerationStats3d {
    /// Source triangles indexed by the BVH.
    pub triangles: usize,
    /// Total branch and leaf nodes.
    pub nodes: usize,
    /// Leaf nodes containing contiguous source ranges.
    pub leaves: usize,
    /// Maximum root-to-leaf depth, where the root has depth one.
    pub maximum_depth: usize,
    /// Maximum number of source triangles stored in one leaf.
    pub leaf_capacity: usize,
    /// Resident bytes occupied by flat BVH nodes, excluding existing faces and
    /// per-triangle AABBs.
    pub bvh_bytes: usize,
}

/// Work counters collected by one allocation-free BVH query.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TriangleMeshQueryStats3d {
    /// Nodes popped from the traversal stack.
    pub nodes_visited: usize,
    /// Nodes or branch children rejected by conservative bounds.
    pub nodes_pruned: usize,
    /// Leaves whose source ranges were inspected.
    pub leaves_visited: usize,
    /// Per-triangle AABBs tested inside accepted leaves.
    pub triangle_bounds_tested: usize,
    /// Triangles reaching ray/triangle or closest-point geometry math.
    pub exact_triangles_tested: usize,
}

/// Raycast output paired with deterministic acceleration telemetry.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TriangleMeshRaycastResult3d {
    /// Nearest exact hit, if any.
    pub hit: Option<TriangleMeshRayHit3d>,
    /// BVH and exact-test work performed by the query.
    pub stats: TriangleMeshQueryStats3d,
}

/// Sphere resolution output paired with acceleration telemetry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TriangleMeshSphereResolutionResult3d {
    /// Exact source-ordered resolution result.
    pub resolution: SphereMeshResolution3d,
    /// Aggregated BVH and exact-test work across completed iterations.
    pub stats: TriangleMeshQueryStats3d,
    /// Iterations entered before stability or the caller's limit.
    pub completed_iterations: usize,
}

/// Construction failure for [`TriangleMesh3d`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TriangleMeshError {
    /// Indices were empty or not arranged in triples.
    InvalidIndexCount {
        /// Observed index count.
        actual: usize,
    },
    /// One source index did not name a supplied vertex.
    IndexOutOfBounds {
        /// Source triangle ordinal.
        triangle: usize,
        /// Missing source vertex index.
        index: u32,
    },
    /// A source position was NaN or infinite.
    NonFiniteVertex {
        /// Source triangle ordinal.
        triangle: usize,
        /// Corner within the source triangle.
        corner: usize,
    },
    /// A face had zero area.
    DegenerateTriangle {
        /// Source triangle ordinal.
        triangle: usize,
    },
    /// The compact BVH cannot represent the requested source triangle count.
    TooManyTriangles {
        /// Requested triangle count.
        actual: usize,
        /// Maximum count supported by compact node indices.
        maximum: usize,
    },
}

impl fmt::Display for TriangleMeshError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidIndexCount { actual } => write!(
                f,
                "triangle mesh needs a non-empty index count divisible by 3, got {actual}"
            ),
            Self::IndexOutOfBounds { triangle, index } => {
                write!(f, "triangle {triangle} references missing vertex {index}")
            }
            Self::NonFiniteVertex { triangle, corner } => {
                write!(f, "triangle {triangle} corner {corner} is not finite")
            }
            Self::DegenerateTriangle { triangle } => write!(f, "triangle {triangle} has zero area"),
            Self::TooManyTriangles { actual, maximum } => write!(
                f,
                "triangle mesh has {actual} faces; compact acceleration supports at most {maximum}"
            ),
        }
    }
}
impl std::error::Error for TriangleMeshError {}

/// Historical walkable normal threshold (`normal.y`) used before configurable slope.
///
/// Corresponds to a maximum walkable slope of about 56.6° from world up
/// (`acos(0.55)`). Kept as the default so existing corridor playables stay
/// behaviour-compatible.
pub const DEFAULT_MIN_WALKABLE_NORMAL_Y: f32 = 0.55;

/// Default max walkable slope matching [`DEFAULT_MIN_WALKABLE_NORMAL_Y`].
#[must_use]
pub fn default_max_walkable_slope_radians() -> f32 {
    DEFAULT_MIN_WALKABLE_NORMAL_Y.acos()
}

/// Result of resolving a character-sized sphere against [`TriangleMesh3d`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SphereMeshResolution3d {
    /// Collision-free centre after the requested iterations.
    pub position: Vec3,
    /// True when a contact normal supports walking upward.
    pub ground_contact: bool,
    /// Number of penetration contacts processed.
    pub contacts: usize,
}

/// Input validation failure for [`TriangleMesh3d::resolve_sphere`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TriangleMeshQueryError {
    /// Requested sphere centre was NaN or infinite.
    NonFinitePosition,
    /// Requested sphere radius was invalid.
    InvalidRadius(f32),
    /// Requested contact-resolution iteration count was zero.
    InvalidIterations,
    /// Max walkable slope was non-finite, non-positive, or greater than `π/2`.
    InvalidMaxWalkableSlope(f32),
}
impl fmt::Display for TriangleMeshQueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFinitePosition => f.write_str("sphere position must be finite"),
            Self::InvalidRadius(radius) => {
                write!(f, "sphere radius must be finite and positive, got {radius}")
            }
            Self::InvalidIterations => f.write_str("collision iterations must be positive"),
            Self::InvalidMaxWalkableSlope(slope) => write!(
                f,
                "max walkable slope must be finite and in (0, π/2], got {slope}"
            ),
        }
    }
}
impl std::error::Error for TriangleMeshQueryError {}

/// Error from a collider or physics-time configuration.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PhysicsConfigError {
    /// A radius was zero, negative, NaN, or infinite.
    InvalidRadius(f32),
    /// A simulation delta was negative, NaN, or infinite.
    InvalidDeltaSeconds(f32),
    /// A named 3D position or velocity contained NaN or infinity.
    NonFiniteVec3 {
        /// Component or query field that failed validation.
        field: &'static str,
    },
    /// A named 2D position or query vector contained NaN or infinity.
    NonFiniteVec2 {
        /// Component or query field that failed validation.
        field: &'static str,
    },
    /// A ray direction was zero, NaN, or infinite.
    InvalidRayDirection,
    /// A ray-query distance was negative, NaN, or infinite.
    InvalidRaycastDistance(f32),
    /// One axis-aligned box half extent was zero, negative, NaN or infinite.
    InvalidAabbHalfExtents(Vec3),
    /// One 2D axis-aligned box half extent was zero, negative, NaN or infinite.
    InvalidAabb2dHalfExtents(Vec2),
    /// An overlap-result budget was zero.
    InvalidQueryResultLimit(usize),
    /// A kinematic static-collider budget was zero.
    InvalidKinematicColliderLimit(usize),
    /// A static-AABB broadphase collider budget was zero.
    InvalidBroadphaseColliderLimit(usize),
    /// A static-AABB broadphase query-result budget was zero.
    InvalidBroadphaseCandidateLimit(usize),
}

impl fmt::Display for PhysicsConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRadius(radius) => write!(
                formatter,
                "radius must be finite and positive, got {radius}"
            ),
            Self::InvalidDeltaSeconds(delta) => write!(
                formatter,
                "delta seconds must be finite and non-negative, got {delta}"
            ),
            Self::NonFiniteVec3 { field } | Self::NonFiniteVec2 { field } => {
                write!(formatter, "{field} must be finite")
            }
            Self::InvalidRayDirection => {
                formatter.write_str("ray direction must be finite and non-zero")
            }
            Self::InvalidRaycastDistance(distance) => write!(
                formatter,
                "raycast distance must be finite and non-negative, got {distance}"
            ),
            Self::InvalidAabbHalfExtents(extents) => write!(
                formatter,
                "AABB half extents must be finite and positive, got ({}, {}, {})",
                extents.x, extents.y, extents.z
            ),
            Self::InvalidAabb2dHalfExtents(extents) => write!(
                formatter,
                "2D AABB half extents must be finite, positive, and safely bounded, got ({}, {})",
                extents.x, extents.y
            ),
            Self::InvalidQueryResultLimit(limit) => {
                write!(
                    formatter,
                    "query result limit must be positive, got {limit}"
                )
            }
            Self::InvalidKinematicColliderLimit(limit) => {
                write!(
                    formatter,
                    "kinematic collider limit must be positive, got {limit}"
                )
            }
            Self::InvalidBroadphaseColliderLimit(limit) => write!(
                formatter,
                "broadphase collider limit must be positive, got {limit}"
            ),
            Self::InvalidBroadphaseCandidateLimit(limit) => write!(
                formatter,
                "broadphase candidate limit must be positive, got {limit}"
            ),
        }
    }
}

impl std::error::Error for PhysicsConfigError {}

/// A 2D circular collider.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Circle {
    radius: f32,
}

impl Circle {
    /// Creates a collider with a finite positive radius.
    ///
    /// # Errors
    ///
    /// Returns [`PhysicsConfigError::InvalidRadius`] for zero, negative, NaN,
    /// or infinite radii.
    pub fn new(radius: f32) -> Result<Self, PhysicsConfigError> {
        if radius.is_finite() && radius > 0.0 {
            Ok(Self { radius })
        } else {
            Err(PhysicsConfigError::InvalidRadius(radius))
        }
    }

    /// Returns the collider radius.
    #[must_use]
    pub const fn radius(self) -> f32 {
        self.radius
    }
}

/// A 3D spherical collider.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sphere {
    radius: f32,
}

impl Sphere {
    /// Creates a collider with a finite positive radius.
    ///
    /// # Errors
    ///
    /// Returns [`PhysicsConfigError::InvalidRadius`] for invalid radii.
    pub fn new(radius: f32) -> Result<Self, PhysicsConfigError> {
        Circle::new(radius).map(|circle| Self {
            radius: circle.radius,
        })
    }

    /// Returns the collider radius.
    #[must_use]
    pub const fn radius(self) -> f32 {
        self.radius
    }
}

/// A point-mass 2D body for standalone simulations.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Body2d {
    /// World position.
    pub position: Vec2,
    /// Linear velocity in world units per second.
    pub velocity: Vec2,
}

impl Body2d {
    /// Creates a moving body.
    #[must_use]
    pub const fn new(position: Vec2, velocity: Vec2) -> Self {
        Self { position, velocity }
    }

    /// Creates a stationary body.
    #[must_use]
    pub const fn stationary(position: Vec2) -> Self {
        Self::new(position, Vec2::ZERO)
    }

    /// Integrates this body with an explicit Euler step.
    ///
    /// # Errors
    ///
    /// Returns [`PhysicsConfigError::InvalidDeltaSeconds`] for invalid time.
    pub fn step(&mut self, delta_seconds: f32) -> Result<(), PhysicsConfigError> {
        validate_delta(delta_seconds)?;
        self.position = self.position + self.velocity * delta_seconds;
        Ok(())
    }
}

/// A point-mass 3D body for standalone simulations.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Body3d {
    /// World position.
    pub position: Vec3,
    /// Linear velocity in world units per second.
    pub velocity: Vec3,
}

impl Body3d {
    /// Creates a moving body.
    #[must_use]
    pub const fn new(position: Vec3, velocity: Vec3) -> Self {
        Self { position, velocity }
    }

    /// Integrates this body with an explicit Euler step.
    ///
    /// # Errors
    ///
    /// Returns [`PhysicsConfigError::InvalidDeltaSeconds`] for invalid time.
    pub fn step(&mut self, delta_seconds: f32) -> Result<(), PhysicsConfigError> {
        validate_delta(delta_seconds)?;
        self.position = self.position + self.velocity * delta_seconds;
        Ok(())
    }
}

/// Contact information returned by shape-overlap queries.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Contact<V> {
    /// Unit normal directed from the first shape toward the second shape.
    pub normal: V,
    /// Positive amount by which shapes overlap.
    pub penetration: f32,
}

/// Tests two circles for overlap.
///
/// Touching exactly is not reported as a collision. Coincident centres use a
/// deterministic positive X normal instead of producing NaN.
#[must_use]
pub fn collide_circles(
    first: Vec2,
    first_shape: Circle,
    second: Vec2,
    second_shape: Circle,
) -> Option<Contact<Vec2>> {
    let separation = second - first;
    let distance_squared = separation.length_squared();
    let radii = first_shape.radius + second_shape.radius;
    if !distance_squared.is_finite() || distance_squared >= radii * radii {
        return None;
    }
    let distance = distance_squared.sqrt();
    let normal = if distance <= f32::EPSILON {
        Vec2::new(1.0, 0.0)
    } else {
        separation * distance.recip()
    };
    Some(Contact {
        normal,
        penetration: radii - distance,
    })
}

/// Tests two spheres for overlap.
///
/// Touching exactly is not reported as a collision. Coincident centres use a
/// deterministic positive X normal instead of producing NaN.
#[must_use]
pub fn collide_spheres(
    first: Vec3,
    first_shape: Sphere,
    second: Vec3,
    second_shape: Sphere,
) -> Option<Contact<Vec3>> {
    let separation = second - first;
    let distance_squared = separation.length_squared();
    let radii = first_shape.radius + second_shape.radius;
    if !distance_squared.is_finite() || distance_squared >= radii * radii {
        return None;
    }
    let distance = distance_squared.sqrt();
    let normal = if distance <= f32::EPSILON {
        Vec3::new(1.0, 0.0, 0.0)
    } else {
        separation * distance.recip()
    };
    Some(Contact {
        normal,
        penetration: radii - distance,
    })
}

/// ECS position component for a 2D dynamic body.
#[derive(Clone, Copy, Component, Debug, PartialEq)]
pub struct Position2d(pub Vec2);

/// ECS linear-velocity component for a 2D dynamic body.
#[derive(Clone, Copy, Component, Debug, PartialEq)]
pub struct Velocity2d(pub Vec2);

/// ECS circle collider component.
#[derive(Clone, Copy, Component, Debug, PartialEq)]
pub struct CircleCollider(pub Circle);

/// Collision discovered by [`step_ecs_2d`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Collision2d {
    /// First entity in deterministic entity-ID order.
    pub first: Entity,
    /// Second entity in deterministic entity-ID order.
    pub second: Entity,
    /// Contact information from first to second.
    pub contact: Contact<Vec2>,
}

/// Integrates all 2D bodies and returns pairwise circle overlaps.
///
/// Collision response is intentionally left to gameplay for this initial
/// slice. Pairs are sorted by Bevy's full generational entity ID. This is
/// quadratic in collider count and intended for prototypes and small gameplay
/// volumes; use a broad phase before large scenes.
///
/// # Errors
///
/// Returns [`PhysicsConfigError::InvalidDeltaSeconds`] for invalid time.
pub fn step_ecs_2d(
    world: &mut World,
    delta_seconds: f32,
) -> Result<Vec<Collision2d>, PhysicsConfigError> {
    validate_delta(delta_seconds)?;
    let mut moving_bodies = world.query::<(&mut Position2d, &Velocity2d)>();
    for (mut position, velocity) in moving_bodies.iter_mut(world) {
        position.0 = position.0 + velocity.0 * delta_seconds;
    }

    let mut colliders: Vec<(Entity, Vec2, Circle)> = world
        .query::<(Entity, &Position2d, &CircleCollider)>()
        .iter(world)
        .map(|(entity, position, collider)| (entity, position.0, collider.0))
        .collect();
    colliders.sort_by_key(|(entity, _, _)| entity.to_bits());

    let mut collisions = Vec::new();
    for first_index in 0..colliders.len() {
        for second_index in first_index + 1..colliders.len() {
            let (first, first_position, first_shape) = colliders[first_index];
            let (second, second_position, second_shape) = colliders[second_index];
            if let Some(contact) =
                collide_circles(first_position, first_shape, second_position, second_shape)
            {
                collisions.push(Collision2d {
                    first,
                    second,
                    contact,
                });
            }
        }
    }
    Ok(collisions)
}

/// Validated ECS position component for a 3D sphere body.
///
/// The field is private so query and simulation code never receives a NaN or
/// infinity authored through this component. This is intentionally separate
/// from render transforms: an application can synchronize the two at its own
/// frame boundary without coupling physics to a renderer.
#[derive(Clone, Copy, Component, Debug, PartialEq)]
pub struct Position3d(Vec3);

impl Position3d {
    /// Creates a finite world position.
    ///
    /// # Errors
    ///
    /// Returns [`PhysicsConfigError::NonFiniteVec3`] when `position` contains
    /// NaN or infinity.
    pub fn new(position: Vec3) -> Result<Self, PhysicsConfigError> {
        validate_vec3(position, "3D position")?;
        Ok(Self(position))
    }

    /// Returns the finite world position.
    #[must_use]
    pub const fn get(self) -> Vec3 {
        self.0
    }

    /// Replaces the world position after validation.
    ///
    /// # Errors
    ///
    /// Returns [`PhysicsConfigError::NonFiniteVec3`] when `position` contains
    /// NaN or infinity.
    pub fn set(&mut self, position: Vec3) -> Result<(), PhysicsConfigError> {
        validate_vec3(position, "3D position")?;
        self.0 = position;
        Ok(())
    }
}

/// Validated ECS linear-velocity component for a 3D sphere body.
#[derive(Clone, Copy, Component, Debug, PartialEq)]
pub struct Velocity3d(Vec3);

impl Velocity3d {
    /// Creates a finite linear velocity in world units per second.
    ///
    /// # Errors
    ///
    /// Returns [`PhysicsConfigError::NonFiniteVec3`] when `velocity` contains
    /// NaN or infinity.
    pub fn new(velocity: Vec3) -> Result<Self, PhysicsConfigError> {
        validate_vec3(velocity, "3D velocity")?;
        Ok(Self(velocity))
    }

    /// Returns the finite linear velocity.
    #[must_use]
    pub const fn get(self) -> Vec3 {
        self.0
    }

    /// Replaces the linear velocity after validation.
    ///
    /// # Errors
    ///
    /// Returns [`PhysicsConfigError::NonFiniteVec3`] when `velocity` contains
    /// NaN or infinity.
    pub fn set(&mut self, velocity: Vec3) -> Result<(), PhysicsConfigError> {
        validate_vec3(velocity, "3D velocity")?;
        self.0 = velocity;
        Ok(())
    }
}

/// ECS spherical collider component for the lightweight 3D query layer.
#[derive(Clone, Copy, Component, Debug, PartialEq)]
pub struct SphereCollider3d(Sphere);

impl SphereCollider3d {
    /// Creates a spherical collider with a finite positive radius.
    ///
    /// # Errors
    ///
    /// Returns [`PhysicsConfigError::InvalidRadius`] for invalid radii.
    pub fn new(radius: f32) -> Result<Self, PhysicsConfigError> {
        Sphere::new(radius).map(Self)
    }

    /// Returns the collider shape.
    #[must_use]
    pub const fn sphere(self) -> Sphere {
        self.0
    }
}

/// Collision event emitted by [`step_ecs_3d`].
///
/// This is detection-only data. No impulses, position correction, trigger
/// routing or gameplay effects are applied by the physics crate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Collision3d {
    /// First entity in deterministic full generational entity-ID order.
    pub first: Entity,
    /// Second entity in deterministic full generational entity-ID order.
    pub second: Entity,
    /// Contact information from `first` toward `second`.
    pub contact: Contact<Vec3>,
}

/// Integrates finite 3D body components and returns pairwise sphere overlaps.
///
/// Events are ordered by full generational entity ID. The algorithm is an
/// O(n²) narrow-phase-only prototype for small gameplay volumes. It has no
/// broad phase, collision response, rotation, continuous collision detection,
/// sleeping, forces or network authority model.
///
/// # Errors
///
/// Returns [`PhysicsConfigError`] for an invalid time step or an integration
/// result that cannot remain finite. On a failed integration the offending
/// position is left unchanged.
pub fn step_ecs_3d(
    world: &mut World,
    delta_seconds: f32,
) -> Result<Vec<Collision3d>, PhysicsConfigError> {
    validate_delta(delta_seconds)?;
    let mut moving_bodies = world.query::<(&mut Position3d, &Velocity3d)>();
    for (mut position, velocity) in moving_bodies.iter_mut(world) {
        let next = position.get() + velocity.get() * delta_seconds;
        position.set(next)?;
    }

    let mut colliders: Vec<(Entity, Vec3, Sphere)> = world
        .query::<(Entity, &Position3d, &SphereCollider3d)>()
        .iter(world)
        .map(|(entity, position, collider)| (entity, position.get(), collider.sphere()))
        .collect();
    colliders.sort_by_key(|(entity, _, _)| entity.to_bits());

    let mut collisions = Vec::new();
    for first_index in 0..colliders.len() {
        for second_index in first_index + 1..colliders.len() {
            let (first, first_position, first_shape) = colliders[first_index];
            let (second, second_position, second_shape) = colliders[second_index];
            if let Some(contact) =
                collide_spheres(first_position, first_shape, second_position, second_shape)
            {
                collisions.push(Collision3d {
                    first,
                    second,
                    contact,
                });
            }
        }
    }
    Ok(collisions)
}

/// A finite axis-aligned 2D box represented by strictly positive half extents.
///
/// Combine this local shape with a world-space centre at query time. It is a
/// detection primitive for tile/world rectangles, not an OBB or rigid body.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aabb2d {
    half_extents: Vec2,
}

impl Aabb2d {
    /// Creates an axis-aligned box with finite, strictly positive safe half extents.
    ///
    /// # Errors
    ///
    /// Returns [`PhysicsConfigError::InvalidAabb2dHalfExtents`] for zero,
    /// negative, NaN, infinite, or too large to combine safely with another
    /// accepted AABB.
    pub fn new(half_extents: Vec2) -> Result<Self, PhysicsConfigError> {
        if half_extents.x.is_finite()
            && half_extents.y.is_finite()
            && half_extents.x > 0.0
            && half_extents.y > 0.0
            && half_extents.x <= f32::MAX / 2.0
            && half_extents.y <= f32::MAX / 2.0
        {
            Ok(Self { half_extents })
        } else {
            Err(PhysicsConfigError::InvalidAabb2dHalfExtents(half_extents))
        }
    }

    /// Returns the positive local half extents.
    #[must_use]
    pub const fn half_extents(self) -> Vec2 {
        self.half_extents
    }
}

/// ECS 2D AABB collider for detection and query-only gameplay.
///
/// Pair it with [`Position2d`] in the ECS world. `Position2d` predates the
/// validated query layer and has a public field, so each query validates its
/// value before using it.
#[derive(Clone, Copy, Component, Debug, PartialEq)]
pub struct AabbCollider2d(Aabb2d);

impl AabbCollider2d {
    /// Creates an ECS AABB collider with validated half extents.
    ///
    /// # Errors
    ///
    /// Returns [`PhysicsConfigError::InvalidAabb2dHalfExtents`] for invalid extents.
    pub fn new(half_extents: Vec2) -> Result<Self, PhysicsConfigError> {
        Aabb2d::new(half_extents).map(Self)
    }

    /// Returns the local AABB shape.
    #[must_use]
    pub const fn aabb(self) -> Aabb2d {
        self.0
    }
}

/// Returns whether `point` is inside or on the boundary of an AABB.
///
/// # Errors
///
/// Returns [`PhysicsConfigError::NonFiniteVec2`] for a non-finite point or centre.
pub fn point_in_aabb_2d(
    point: Vec2,
    center: Vec2,
    aabb: Aabb2d,
) -> Result<bool, PhysicsConfigError> {
    validate_vec2(point, "AABB point")?;
    validate_vec2(center, "AABB centre")?;
    let offset = point - center;
    Ok(offset.x.abs() <= aabb.half_extents.x && offset.y.abs() <= aabb.half_extents.y)
}

/// Tests two AABBs for strict overlap and returns minimal-separation contact data.
///
/// Exact face, edge, or corner touching is not an overlap. For equal
/// penetrations the X axis wins; coincident centres consequently use positive
/// X as a stable normal.
///
/// # Errors
///
/// Returns [`PhysicsConfigError::NonFiniteVec2`] when either centre is non-finite.
pub fn collide_aabbs_2d(
    first_center: Vec2,
    first: Aabb2d,
    second_center: Vec2,
    second: Aabb2d,
) -> Result<Option<Contact<Vec2>>, PhysicsConfigError> {
    validate_vec2(first_center, "first AABB centre")?;
    validate_vec2(second_center, "second AABB centre")?;
    let delta = second_center - first_center;
    let overlap_x = first.half_extents.x + second.half_extents.x - delta.x.abs();
    let overlap_y = first.half_extents.y + second.half_extents.y - delta.y.abs();
    if overlap_x <= 0.0 || overlap_y <= 0.0 {
        return Ok(None);
    }
    let (normal, penetration) = if overlap_x <= overlap_y {
        (
            Vec2::new(if delta.x < 0.0 { -1.0 } else { 1.0 }, 0.0),
            overlap_x,
        )
    } else {
        (
            Vec2::new(0.0, if delta.y < 0.0 { -1.0 } else { 1.0 }),
            overlap_y,
        )
    };
    Ok(Some(Contact {
        normal,
        penetration,
    }))
}

/// A finite ray with a normalized direction used by 2D AABB queries.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ray2d {
    origin: Vec2,
    direction: Vec2,
}

impl Ray2d {
    /// Creates a ray and normalizes `direction`.
    ///
    /// # Errors
    ///
    /// Returns [`PhysicsConfigError::NonFiniteVec2`] for a non-finite origin,
    /// or [`PhysicsConfigError::InvalidRayDirection`] for a zero/non-finite direction.
    pub fn new(origin: Vec2, direction: Vec2) -> Result<Self, PhysicsConfigError> {
        validate_vec2(origin, "ray origin")?;
        let direction = normalize_ray_direction_2d(direction)?;
        Ok(Self { origin, direction })
    }

    /// Returns the finite world-space ray origin.
    #[must_use]
    pub const fn origin(self) -> Vec2 {
        self.origin
    }

    /// Returns the normalized ray direction.
    #[must_use]
    pub const fn direction(self) -> Vec2 {
        self.direction
    }

    /// Returns the point `distance` world units along this ray.
    #[must_use]
    pub fn point_at(self, distance: f32) -> Vec2 {
        self.origin + self.direction * distance
    }
}

/// Geometry hit from a ray/AABB query.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RayAabbHit2d {
    /// Distance along the normalized ray in world units.
    pub distance: f32,
    /// World-space hit point.
    pub position: Vec2,
    /// Outward face normal. Rays originating inside use the stable opposite-ray normal.
    pub normal: Vec2,
}

/// Intersects `ray` with an AABB up to `max_distance` world units.
///
/// A ray starting inside or on the boundary reports distance zero and uses a
/// stable normal opposite its direction. Parallel slab cases are handled
/// without division by zero.
///
/// # Errors
///
/// Returns [`PhysicsConfigError`] for a non-finite centre or invalid distance.
pub fn raycast_aabb_2d(
    ray: Ray2d,
    center: Vec2,
    aabb: Aabb2d,
    max_distance: f32,
) -> Result<Option<RayAabbHit2d>, PhysicsConfigError> {
    validate_vec2(center, "AABB centre")?;
    validate_raycast_distance(max_distance)?;
    if point_in_aabb_2d(ray.origin, center, aabb)? {
        return Ok(Some(RayAabbHit2d {
            distance: 0.0,
            position: ray.origin,
            normal: ray.direction * -1.0,
        }));
    }
    let minimum = center - aabb.half_extents;
    let maximum = center + aabb.half_extents;
    validate_vec2(minimum, "AABB minimum world bound")?;
    validate_vec2(maximum, "AABB maximum world bound")?;
    let origins = [ray.origin.x, ray.origin.y];
    let directions = [ray.direction.x, ray.direction.y];
    let minima = [minimum.x, minimum.y];
    let maxima = [maximum.x, maximum.y];
    let mut entry = f32::NEG_INFINITY;
    let mut exit = f32::INFINITY;
    let mut entry_normal = Vec2::ZERO;
    for axis in 0..2 {
        if directions[axis].abs() <= f32::EPSILON {
            if origins[axis] < minima[axis] || origins[axis] > maxima[axis] {
                return Ok(None);
            }
            continue;
        }
        let first = (minima[axis] - origins[axis]) / directions[axis];
        let second = (maxima[axis] - origins[axis]) / directions[axis];
        let (near, far, normal) = if first <= second {
            (first, second, axis_normal_2d(axis, -1.0))
        } else {
            (second, first, axis_normal_2d(axis, 1.0))
        };
        if near > entry {
            entry = near;
            entry_normal = normal;
        }
        exit = exit.min(far);
        if entry > exit {
            return Ok(None);
        }
    }
    if entry < 0.0 || entry > max_distance || !entry.is_finite() {
        return Ok(None);
    }
    Ok(Some(RayAabbHit2d {
        distance: entry,
        position: ray.point_at(entry),
        normal: entry_normal,
    }))
}

/// ECS-owned nearest hit from [`raycast_aabbs_2d`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RaycastAabbHit2d {
    /// Entity that owns the hit [`AabbCollider2d`].
    pub entity: Entity,
    /// Geometry hit information.
    pub hit: RayAabbHit2d,
}

/// Returns the nearest ECS AABB hit with deterministic entity-ID tie breaking.
///
/// `ignored` excludes one entity. The query is O(n) and performs no broad
/// phase, mesh test, collision response, rotation, OBB handling or CCD.
///
/// # Errors
///
/// Returns [`PhysicsConfigError`] for an invalid maximum distance or a
/// non-finite [`Position2d`] component.
pub fn raycast_aabbs_2d(
    world: &mut World,
    ray: Ray2d,
    max_distance: f32,
    ignored: Option<Entity>,
) -> Result<Option<RaycastAabbHit2d>, PhysicsConfigError> {
    validate_raycast_distance(max_distance)?;
    let mut hits = Vec::new();
    let mut colliders = world.query::<(Entity, &Position2d, &AabbCollider2d)>();
    for (entity, position, collider) in colliders.iter(world) {
        if Some(entity) == ignored {
            continue;
        }
        validate_vec2(position.0, "AABB collider position")?;
        if let Some(hit) = raycast_aabb_2d(ray, position.0, collider.aabb(), max_distance)? {
            hits.push(RaycastAabbHit2d { entity, hit });
        }
    }
    hits.sort_by(|left, right| {
        left.hit
            .distance
            .total_cmp(&right.hit.distance)
            .then_with(|| left.entity.to_bits().cmp(&right.entity.to_bits()))
    });
    Ok(hits.into_iter().next())
}

/// Validated result budget for [`overlap_aabbs_2d`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AabbQueryLimits2d {
    max_overlaps: usize,
}

impl AabbQueryLimits2d {
    /// Creates a non-zero maximum count for returned overlap records.
    ///
    /// # Errors
    ///
    /// Returns [`PhysicsConfigError::InvalidQueryResultLimit`] when `max_overlaps` is zero.
    pub const fn new(max_overlaps: usize) -> Result<Self, PhysicsConfigError> {
        if max_overlaps == 0 {
            Err(PhysicsConfigError::InvalidQueryResultLimit(max_overlaps))
        } else {
            Ok(Self { max_overlaps })
        }
    }

    /// Returns the maximum number of overlap records produced by one query.
    #[must_use]
    pub const fn max_overlaps(self) -> usize {
        self.max_overlaps
    }
}

impl Default for AabbQueryLimits2d {
    fn default() -> Self {
        Self {
            max_overlaps: 65_536,
        }
    }
}

/// ECS AABB overlap result with strict contact data and no response.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AabbOverlap2d {
    /// Entity owning the overlapping AABB collider.
    pub entity: Entity,
    /// Contact normal and penetration from the query AABB toward `entity`.
    pub contact: Contact<Vec2>,
}

/// Failure while collecting a bounded ECS AABB overlap result.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AabbQueryError2d {
    /// Query centre or a participating collider position was non-finite.
    InvalidInput(PhysicsConfigError),
    /// A matching collider would exceed the configured result budget.
    ResultLimitExceeded {
        /// Configured maximum number of overlap records.
        maximum: usize,
    },
}

impl fmt::Display for AabbQueryError2d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(error) => write!(formatter, "2D AABB query input failed: {error}"),
            Self::ResultLimitExceeded { maximum } => {
                write!(formatter, "2D AABB query exceeded result limit {maximum}")
            }
        }
    }
}

impl std::error::Error for AabbQueryError2d {}

/// Returns strictly overlapping ECS AABBs in deterministic entity-ID order.
///
/// Exact face, edge, or corner touching is not an overlap. The result is
/// bounded by `limits`; this protects callers materialising dense tile-map
/// collision snapshots, but this narrow-phase query still scans every matching
/// ECS collider. It has no broad phase, response, rotation, OBB support,
/// mesh collision or CCD.
///
/// # Errors
///
/// Returns [`AabbQueryError2d::InvalidInput`] for a non-finite query or
/// collider centre, and [`AabbQueryError2d::ResultLimitExceeded`] if the
/// result budget would be exceeded.
pub fn overlap_aabbs_2d(
    world: &mut World,
    center: Vec2,
    aabb: Aabb2d,
    ignored: Option<Entity>,
    limits: AabbQueryLimits2d,
) -> Result<Vec<AabbOverlap2d>, AabbQueryError2d> {
    validate_vec2(center, "overlap AABB centre").map_err(AabbQueryError2d::InvalidInput)?;
    let mut overlaps = Vec::new();
    let mut colliders = world.query::<(Entity, &Position2d, &AabbCollider2d)>();
    for (entity, position, collider) in colliders.iter(world) {
        if Some(entity) == ignored {
            continue;
        }
        let contact = collide_aabbs_2d(center, aabb, position.0, collider.aabb())
            .map_err(AabbQueryError2d::InvalidInput)?;
        let Some(contact) = contact else {
            continue;
        };
        if overlaps.len() == limits.max_overlaps {
            return Err(AabbQueryError2d::ResultLimitExceeded {
                maximum: limits.max_overlaps,
            });
        }
        overlaps.push(AabbOverlap2d { entity, contact });
    }
    overlaps.sort_by_key(|overlap| overlap.entity.to_bits());
    Ok(overlaps)
}

/// Validated immutable AABB used as an obstacle by kinematic movement queries.
///
/// `key` is caller-owned and must be unique within one movement call. It gives
/// tile collision snapshots a stable ordering without coupling this crate to a
/// renderer, map format, or gameplay ECS entity layout.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StaticAabb2d {
    key: u64,
    center: Vec2,
    aabb: Aabb2d,
}

impl StaticAabb2d {
    /// Creates one finite static obstacle with a caller-owned stable `key`.
    ///
    /// # Errors
    ///
    /// Returns [`PhysicsConfigError::NonFiniteVec2`] when `center` is invalid
    /// or its AABB bounds cannot remain finite.
    pub fn new(key: u64, center: Vec2, aabb: Aabb2d) -> Result<Self, PhysicsConfigError> {
        validate_vec2(center, "static AABB centre")?;
        let _ = aabb_bounds_2d(center, aabb, "static AABB world bounds")?;
        Ok(Self { key, center, aabb })
    }

    /// Returns the caller-owned stable ordering key.
    #[must_use]
    pub const fn key(self) -> u64 {
        self.key
    }

    /// Returns the finite world-space centre.
    #[must_use]
    pub const fn center(self) -> Vec2 {
        self.center
    }

    /// Returns the local static AABB shape.
    #[must_use]
    pub const fn aabb(self) -> Aabb2d {
        self.aabb
    }
}

/// Ограничения памяти и одного запроса для [`StaticAabbBroadphase2d`].
///
/// Лимиты обязательны: индекс предназначен для карт и наборов препятствий,
/// которые могут быть получены из файлов или сети. Он не выделяет память без
/// заранее заданной верхней границы.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StaticAabbBroadphaseLimits2d {
    max_colliders: usize,
    max_candidates: usize,
}

impl StaticAabbBroadphaseLimits2d {
    /// Создаёт ограничения для числа статичных коробок и кандидатов запроса.
    ///
    /// # Errors
    ///
    /// Возвращает [`PhysicsConfigError`] при нулевом лимите.
    pub const fn new(
        max_colliders: usize,
        max_candidates: usize,
    ) -> Result<Self, PhysicsConfigError> {
        if max_colliders == 0 {
            return Err(PhysicsConfigError::InvalidBroadphaseColliderLimit(
                max_colliders,
            ));
        }
        if max_candidates == 0 {
            return Err(PhysicsConfigError::InvalidBroadphaseCandidateLimit(
                max_candidates,
            ));
        }
        Ok(Self {
            max_colliders,
            max_candidates,
        })
    }

    /// Максимальное число коробок, которое можно хранить в индексе.
    #[must_use]
    pub const fn max_colliders(self) -> usize {
        self.max_colliders
    }

    /// Максимальное число ключей, которое может вернуть один запрос.
    #[must_use]
    pub const fn max_candidates(self) -> usize {
        self.max_candidates
    }
}

impl Default for StaticAabbBroadphaseLimits2d {
    fn default() -> Self {
        Self {
            max_colliders: 65_536,
            max_candidates: 16_384,
        }
    }
}

/// Ошибка построения, изменения или запроса [`StaticAabbBroadphase2d`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StaticAabbBroadphaseError2d {
    /// Входная геометрия не имеет конечных координат.
    InvalidInput(PhysicsConfigError),
    /// Набор коробок превышает выделенный бюджет индекса.
    ColliderLimitExceeded {
        /// Разрешённое число коробок.
        maximum: usize,
        /// Фактическое число коробок.
        actual: usize,
    },
    /// В одном индексе два препятствия используют одинаковый ключ.
    DuplicateStaticColliderKey {
        /// Повторённый ключ.
        key: u64,
    },
    /// Обновляется или удаляется ключ, которого нет в индексе.
    MissingStaticColliderKey {
        /// Запрошенный ключ.
        key: u64,
    },
    /// Результат запроса превысил заданный бюджет кандидатов.
    CandidateLimitExceeded {
        /// Разрешённое число кандидатов.
        maximum: usize,
    },
}

impl fmt::Display for StaticAabbBroadphaseError2d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(error) => {
                write!(formatter, "invalid static AABB broadphase input: {error}")
            }
            Self::ColliderLimitExceeded { maximum, actual } => write!(
                formatter,
                "static AABB broadphase collider budget {maximum} exceeded by {actual} colliders"
            ),
            Self::DuplicateStaticColliderKey { key } => {
                write!(formatter, "duplicate static AABB broadphase key {key}")
            }
            Self::MissingStaticColliderKey { key } => {
                write!(formatter, "static AABB broadphase has no key {key}")
            }
            Self::CandidateLimitExceeded { maximum } => write!(
                formatter,
                "static AABB broadphase candidate budget {maximum} exceeded"
            ),
        }
    }
}

impl std::error::Error for StaticAabbBroadphaseError2d {}

/// Точный результат высокоуровневого лучевого запроса к статичным AABB.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StaticRaycastAabbHit2d {
    /// Стабильный ключ препятствия.
    pub collider_key: u64,
    /// Пересечённое препятствие.
    pub collider: StaticAabb2d,
    /// Точка, нормаль и расстояние пересечения.
    pub hit: RayAabbHit2d,
}

/// Индекс статичных 2D AABB для карт, тайлов и простых миров.
///
/// Это компактный sweep-and-prune по левой X-границе. Он строится один раз
/// для неизменяемой части карты, а затем быстро отбрасывает коробки, которые
/// не пересекают прямоугольник области по X. По Y выполняется консервативная
/// проверка границ. Луч сначала получает кандидатов по ограничивающему
/// прямоугольнику своего отрезка, после чего [`Self::raycast`] делает точный
/// тест. Поэтому API кандидатов безопасен для собственного narrow phase, но
/// сам по себе не является попаданием луча.
///
/// Порядок всегда детерминирован: методы кандидатов возвращают ключи по
/// возрастанию, а при равном расстоянии [`Self::raycast`] выбирает меньший
/// ключ. Индекс статичный: изменения через [`Self::insert`],
/// [`Self::update`], [`Self::remove`] и [`Self::rebuild`] перестраивают
/// упорядоченный массив. Для постоянно движущихся тел нужен отдельный
/// динамический broadphase.
#[derive(Clone, Debug)]
pub struct StaticAabbBroadphase2d {
    limits: StaticAabbBroadphaseLimits2d,
    /// Sorted by minimum X, then caller key. Never exposed as index positions.
    colliders: Vec<StaticAabb2d>,
}

impl StaticAabbBroadphase2d {
    /// Создаёт пустой индекс с явными ограничениями.
    #[must_use]
    pub const fn new(limits: StaticAabbBroadphaseLimits2d) -> Self {
        Self {
            limits,
            colliders: Vec::new(),
        }
    }

    /// Строит индекс из набора статичных препятствий.
    ///
    /// При ошибке частично построенный индекс не возвращается.
    ///
    /// # Errors
    ///
    /// Возвращает ошибку повторяющегося ключа или превышения лимита коробок.
    pub fn build(
        colliders: impl IntoIterator<Item = StaticAabb2d>,
        limits: StaticAabbBroadphaseLimits2d,
    ) -> Result<Self, StaticAabbBroadphaseError2d> {
        let mut index = Self::new(limits);
        for collider in colliders {
            index.insert(collider)?;
        }
        Ok(index)
    }

    /// Возвращает ограничения этого индекса.
    #[must_use]
    pub const fn limits(&self) -> StaticAabbBroadphaseLimits2d {
        self.limits
    }

    /// Возвращает число сохранённых препятствий.
    #[must_use]
    pub fn len(&self) -> usize {
        self.colliders.len()
    }

    /// Возвращает `true`, если индекс пуст.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.colliders.is_empty()
    }

    /// Находит препятствие по стабильному ключу.
    #[must_use]
    pub fn get(&self, key: u64) -> Option<StaticAabb2d> {
        self.colliders
            .iter()
            .copied()
            .find(|collider| collider.key == key)
    }

    /// Добавляет одно препятствие и поддерживает индекс упорядоченным.
    ///
    /// # Errors
    ///
    /// Возвращает ошибку повторяющегося ключа или превышения лимита коробок.
    pub fn insert(&mut self, collider: StaticAabb2d) -> Result<(), StaticAabbBroadphaseError2d> {
        if self.colliders.len() == self.limits.max_colliders {
            return Err(StaticAabbBroadphaseError2d::ColliderLimitExceeded {
                maximum: self.limits.max_colliders,
                actual: self.colliders.len() + 1,
            });
        }
        if self.get(collider.key).is_some() {
            return Err(StaticAabbBroadphaseError2d::DuplicateStaticColliderKey {
                key: collider.key,
            });
        }
        self.colliders.push(collider);
        self.sort_for_queries();
        Ok(())
    }

    /// Заменяет существующее препятствие с тем же ключом.
    ///
    /// # Errors
    ///
    /// Возвращает [`StaticAabbBroadphaseError2d::MissingStaticColliderKey`],
    /// если этого ключа ещё нет в индексе.
    pub fn update(&mut self, collider: StaticAabb2d) -> Result<(), StaticAabbBroadphaseError2d> {
        let Some(slot) = self
            .colliders
            .iter_mut()
            .find(|existing| existing.key == collider.key)
        else {
            return Err(StaticAabbBroadphaseError2d::MissingStaticColliderKey {
                key: collider.key,
            });
        };
        *slot = collider;
        self.sort_for_queries();
        Ok(())
    }

    /// Удаляет препятствие по ключу и возвращает его прежнее значение.
    ///
    /// # Errors
    ///
    /// Возвращает [`StaticAabbBroadphaseError2d::MissingStaticColliderKey`],
    /// если этого ключа нет в индексе.
    pub fn remove(&mut self, key: u64) -> Result<StaticAabb2d, StaticAabbBroadphaseError2d> {
        let Some(index) = self
            .colliders
            .iter()
            .position(|collider| collider.key == key)
        else {
            return Err(StaticAabbBroadphaseError2d::MissingStaticColliderKey { key });
        };
        Ok(self.colliders.remove(index))
    }

    /// Атомарно заменяет все препятствия новым снимком карты.
    ///
    /// При ошибке прежний индекс остаётся нетронутым.
    ///
    /// # Errors
    ///
    /// Возвращает ошибку повторяющегося ключа или превышения лимита коробок;
    /// прежний снимок при этом сохраняется.
    pub fn rebuild(
        &mut self,
        colliders: impl IntoIterator<Item = StaticAabb2d>,
    ) -> Result<(), StaticAabbBroadphaseError2d> {
        let rebuilt = Self::build(colliders, self.limits)?;
        *self = rebuilt;
        Ok(())
    }

    /// Возвращает ключи возможных пересечений области в порядке возрастания.
    ///
    /// Касание границы включается намеренно: это безопасный набор кандидатов
    /// для собственного narrow phase. Для строгого игрового overlap используйте
    /// [`Self::overlaps_in_region`].
    ///
    /// # Errors
    ///
    /// Возвращает ошибку неконечной области или превышения лимита кандидатов.
    pub fn candidate_keys_in_region(
        &self,
        center: Vec2,
        aabb: Aabb2d,
    ) -> Result<Vec<u64>, StaticAabbBroadphaseError2d> {
        let (minimum, maximum) = aabb_bounds_2d(center, aabb, "broadphase region bounds")
            .map_err(StaticAabbBroadphaseError2d::InvalidInput)?;
        self.candidate_keys_in_bounds(minimum, maximum)
    }

    /// Возвращает ключи возможных пересечений конечного отрезка луча.
    ///
    /// Это низкоуровневый вариант: его результат консервативен и может
    /// содержать коробки, в которые сам луч не попадает.
    ///
    /// # Errors
    ///
    /// Возвращает ошибку некорректной длины луча, переполнения его конечной
    /// точки или превышения лимита кандидатов.
    pub fn candidate_keys_for_ray(
        &self,
        ray: Ray2d,
        max_distance: f32,
    ) -> Result<Vec<u64>, StaticAabbBroadphaseError2d> {
        validate_raycast_distance(max_distance)
            .map_err(StaticAabbBroadphaseError2d::InvalidInput)?;
        let end = ray.point_at(max_distance);
        validate_vec2(end, "broadphase ray end")
            .map_err(StaticAabbBroadphaseError2d::InvalidInput)?;
        let minimum = Vec2::new(ray.origin.x.min(end.x), ray.origin.y.min(end.y));
        let maximum = Vec2::new(ray.origin.x.max(end.x), ray.origin.y.max(end.y));
        self.candidate_keys_in_bounds(minimum, maximum)
    }

    /// Высокоуровневый строгий overlap-запрос статичных коробок.
    ///
    /// Результат отсортирован по ключу. Касание граней, рёбер и углов не
    /// считается overlap — точно как в [`collide_aabbs_2d`].
    ///
    /// # Errors
    ///
    /// Возвращает ошибку неконечной области или превышения лимита кандидатов.
    pub fn overlaps_in_region(
        &self,
        center: Vec2,
        aabb: Aabb2d,
    ) -> Result<Vec<StaticAabb2d>, StaticAabbBroadphaseError2d> {
        let candidate_keys = self.candidate_keys_in_region(center, aabb)?;
        let mut overlaps = Vec::with_capacity(candidate_keys.len());
        for key in candidate_keys {
            let Some(collider) = self.get(key) else {
                continue;
            };
            if collide_aabbs_2d(center, aabb, collider.center, collider.aabb)
                .map_err(StaticAabbBroadphaseError2d::InvalidInput)?
                .is_some()
            {
                overlaps.push(collider);
            }
        }
        Ok(overlaps)
    }

    /// Высокоуровневый точный лучевой запрос к статичным коробкам.
    ///
    /// При равном расстоянии побеждает меньший ключ. Можно использовать его
    /// напрямую для взаимодействия с миром без отдельного ручного narrow phase.
    ///
    /// # Errors
    ///
    /// Возвращает ошибку некорректной длины луча, переполнения его конечной
    /// точки или превышения лимита кандидатов.
    pub fn raycast(
        &self,
        ray: Ray2d,
        max_distance: f32,
    ) -> Result<Option<StaticRaycastAabbHit2d>, StaticAabbBroadphaseError2d> {
        let candidate_keys = self.candidate_keys_for_ray(ray, max_distance)?;
        let mut nearest: Option<StaticRaycastAabbHit2d> = None;
        for key in candidate_keys {
            let Some(collider) = self.get(key) else {
                continue;
            };
            let Some(hit) = raycast_aabb_2d(ray, collider.center, collider.aabb, max_distance)
                .map_err(StaticAabbBroadphaseError2d::InvalidInput)?
            else {
                continue;
            };
            let candidate = StaticRaycastAabbHit2d {
                collider_key: key,
                collider,
                hit,
            };
            if nearest.as_ref().is_none_or(|current| {
                candidate
                    .hit
                    .distance
                    .total_cmp(&current.hit.distance)
                    .then_with(|| candidate.collider_key.cmp(&current.collider_key))
                    .is_lt()
            }) {
                nearest = Some(candidate);
            }
        }
        Ok(nearest)
    }

    fn candidate_keys_in_bounds(
        &self,
        minimum: Vec2,
        maximum: Vec2,
    ) -> Result<Vec<u64>, StaticAabbBroadphaseError2d> {
        validate_vec2(minimum, "broadphase query minimum")
            .map_err(StaticAabbBroadphaseError2d::InvalidInput)?;
        validate_vec2(maximum, "broadphase query maximum")
            .map_err(StaticAabbBroadphaseError2d::InvalidInput)?;
        let mut keys = Vec::new();
        for collider in &self.colliders {
            let (collider_minimum, collider_maximum) =
                aabb_bounds_2d(collider.center, collider.aabb, "broadphase collider bounds")
                    .map_err(StaticAabbBroadphaseError2d::InvalidInput)?;
            if collider_minimum.x > maximum.x {
                break;
            }
            if collider_maximum.x < minimum.x
                || collider_maximum.y < minimum.y
                || collider_minimum.y > maximum.y
            {
                continue;
            }
            if keys.len() == self.limits.max_candidates {
                return Err(StaticAabbBroadphaseError2d::CandidateLimitExceeded {
                    maximum: self.limits.max_candidates,
                });
            }
            keys.push(collider.key);
        }
        keys.sort_unstable();
        Ok(keys)
    }

    fn sort_for_queries(&mut self) {
        self.colliders.sort_by(|left, right| {
            let left_minimum = left.center.x - left.aabb.half_extents.x;
            let right_minimum = right.center.x - right.aabb.half_extents.x;
            left_minimum
                .total_cmp(&right_minimum)
                .then_with(|| left.key.cmp(&right.key))
        });
    }
}

/// Validated work limit for [`resolve_kinematic_aabb_2d`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KinematicAabbMoveLimits2d {
    max_static_colliders: usize,
}

impl KinematicAabbMoveLimits2d {
    /// Creates a non-zero static-collider budget for one movement query.
    ///
    /// # Errors
    ///
    /// Returns [`PhysicsConfigError::InvalidKinematicColliderLimit`] when the
    /// budget is zero.
    pub const fn new(max_static_colliders: usize) -> Result<Self, PhysicsConfigError> {
        if max_static_colliders == 0 {
            Err(PhysicsConfigError::InvalidKinematicColliderLimit(
                max_static_colliders,
            ))
        } else {
            Ok(Self {
                max_static_colliders,
            })
        }
    }

    /// Returns the maximum accepted static-obstacle count.
    #[must_use]
    pub const fn max_static_colliders(self) -> usize {
        self.max_static_colliders
    }
}

impl Default for KinematicAabbMoveLimits2d {
    fn default() -> Self {
        Self {
            max_static_colliders: 65_536,
        }
    }
}

/// One resolved static-obstacle contact from a kinematic movement query.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KinematicAabbContact2d {
    /// Stable key of the static obstacle that stopped this axis sweep.
    pub collider_key: u64,
    /// Outward normal of the obstacle face toward the moving AABB.
    ///
    /// This is the direction a caller would use to push the moving box away.
    pub normal: Vec2,
}

/// Completed kinematic AABB movement, including ordered blocking contacts.
#[derive(Clone, Debug, PartialEq)]
pub struct KinematicAabbMove2d {
    /// Final finite world-space centre after X then Y axis sweeps.
    pub final_center: Vec2,
    /// Actual finite displacement after static-obstacle blocking.
    pub applied_delta: Vec2,
    contacts: Vec<KinematicAabbContact2d>,
}

impl KinematicAabbMove2d {
    /// Returns contacts in deterministic X-sweep then Y-sweep order.
    #[must_use]
    pub fn contacts(&self) -> &[KinematicAabbContact2d] {
        &self.contacts
    }
}

/// Failure while resolving kinematic AABB motion.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum KinematicAabbMoveError {
    /// Moving input or its required world-space bounds were not finite.
    InvalidInput(PhysicsConfigError),
    /// Supplied static obstacles exceed the configured work budget.
    StaticColliderLimitExceeded {
        /// Maximum accepted static-obstacle count.
        maximum: usize,
        /// Number of supplied static obstacles.
        actual: usize,
    },
    /// Two supplied static obstacles used the same stable key.
    DuplicateStaticColliderKey {
        /// Duplicate caller-owned key.
        key: u64,
    },
    /// The moving AABB started in strict overlap with an obstacle.
    ///
    /// No automatic depenetration policy is applied.
    InitialOverlap {
        /// Stable key of the first overlapping obstacle in deterministic order.
        collider_key: u64,
    },
}

impl fmt::Display for KinematicAabbMoveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(error) => write!(formatter, "invalid kinematic AABB input: {error}"),
            Self::StaticColliderLimitExceeded { maximum, actual } => write!(
                formatter,
                "kinematic AABB static collider budget {maximum} exceeded by {actual} colliders"
            ),
            Self::DuplicateStaticColliderKey { key } => {
                write!(
                    formatter,
                    "duplicate kinematic AABB static collider key {key}"
                )
            }
            Self::InitialOverlap { collider_key } => write!(
                formatter,
                "kinematic AABB starts overlapping static collider {collider_key}"
            ),
        }
    }
}

impl std::error::Error for KinematicAabbMoveError {}

#[derive(Clone, Copy, Debug)]
struct AxisSweepHit2d {
    distance: f32,
    collider_key: u64,
    normal: Vec2,
}

/// Resolves a moving AABB against bounded immutable AABBs with deterministic X/Y sliding.
///
/// The resolver sweeps the X axis first, then sweeps Y from the X-resolved
/// position. It chooses the nearest blocking obstacle; equal-distance ties use
/// the smallest [`StaticAabb2d::key`]. This prevents tunnelling for finite
/// axis-aligned movement while naturally allowing a diagonal move to slide
/// along a vertical or horizontal wall.
///
/// Static boxes only: this does not move obstacles, process dynamic bodies,
/// apply forces/impulses, rotate shapes, use OBBs, broad phase, CCD for
/// arbitrary trajectories, or solve an initial penetration.
///
/// # Errors
///
/// Returns [`KinematicAabbMoveError`] for non-finite input, a static-budget
/// overflow, duplicate static keys, or an initial strict overlap.
pub fn resolve_kinematic_aabb_2d(
    center: Vec2,
    aabb: Aabb2d,
    desired_delta: Vec2,
    static_colliders: &[StaticAabb2d],
    limits: KinematicAabbMoveLimits2d,
) -> Result<KinematicAabbMove2d, KinematicAabbMoveError> {
    validate_vec2(center, "kinematic AABB centre").map_err(KinematicAabbMoveError::InvalidInput)?;
    validate_vec2(desired_delta, "kinematic AABB desired delta")
        .map_err(KinematicAabbMoveError::InvalidInput)?;
    let desired_end = center + desired_delta;
    validate_vec2(desired_end, "kinematic AABB desired end")
        .map_err(KinematicAabbMoveError::InvalidInput)?;
    let _ = aabb_bounds_2d(center, aabb, "kinematic AABB world bounds")
        .map_err(KinematicAabbMoveError::InvalidInput)?;
    if static_colliders.len() > limits.max_static_colliders {
        return Err(KinematicAabbMoveError::StaticColliderLimitExceeded {
            maximum: limits.max_static_colliders,
            actual: static_colliders.len(),
        });
    }

    let mut ordered = static_colliders.to_vec();
    ordered.sort_by_key(|collider| collider.key);
    for pair in ordered.windows(2) {
        if pair[0].key == pair[1].key {
            return Err(KinematicAabbMoveError::DuplicateStaticColliderKey { key: pair[0].key });
        }
    }
    for collider in &ordered {
        if collide_aabbs_2d(center, aabb, collider.center, collider.aabb)
            .map_err(KinematicAabbMoveError::InvalidInput)?
            .is_some()
        {
            return Err(KinematicAabbMoveError::InitialOverlap {
                collider_key: collider.key,
            });
        }
    }

    let mut final_center = center;
    let mut contacts = Vec::new();
    let resolved_x = sweep_aabb_axis_2d(
        final_center,
        aabb,
        desired_delta.x,
        true,
        &ordered,
        KINEMATIC_AABB_CONTACT_SKIN_2D,
    )
    .map_err(KinematicAabbMoveError::InvalidInput)?;
    if let Some(hit) = resolved_x {
        final_center.x += hit.distance.copysign(desired_delta.x);
        contacts.push(KinematicAabbContact2d {
            collider_key: hit.collider_key,
            normal: hit.normal,
        });
    } else {
        final_center.x += desired_delta.x;
    }
    let resolved_y = sweep_aabb_axis_2d(
        final_center,
        aabb,
        desired_delta.y,
        false,
        &ordered,
        KINEMATIC_AABB_CONTACT_SKIN_2D,
    )
    .map_err(KinematicAabbMoveError::InvalidInput)?;
    if let Some(hit) = resolved_y {
        final_center.y += hit.distance.copysign(desired_delta.y);
        contacts.push(KinematicAabbContact2d {
            collider_key: hit.collider_key,
            normal: hit.normal,
        });
    } else {
        final_center.y += desired_delta.y;
    }
    Ok(KinematicAabbMove2d {
        final_center,
        applied_delta: final_center - center,
        contacts,
    })
}

/// Keep movers a hair outside static AABBs so float error cannot nest into a wall.
const KINEMATIC_AABB_CONTACT_SKIN_2D: f32 = 0.5;

fn sweep_aabb_axis_2d(
    center: Vec2,
    moving: Aabb2d,
    delta: f32,
    horizontal: bool,
    static_colliders: &[StaticAabb2d],
    contact_skin: f32,
) -> Result<Option<AxisSweepHit2d>, PhysicsConfigError> {
    if delta == 0.0 {
        return Ok(None);
    }
    let (moving_min, moving_max) = aabb_bounds_2d(center, moving, "moving AABB world bounds")?;
    let mut nearest: Option<AxisSweepHit2d> = None;
    for collider in static_colliders {
        let (static_min, static_max) =
            aabb_bounds_2d(collider.center, collider.aabb, "static AABB world bounds")?;
        let (cross_min, cross_max, static_cross_min, static_cross_max) = if horizontal {
            (moving_min.y, moving_max.y, static_min.y, static_max.y)
        } else {
            (moving_min.x, moving_max.x, static_min.x, static_max.x)
        };
        if cross_min >= static_cross_max || cross_max <= static_cross_min {
            continue;
        }
        let (separation, normal) = if horizontal {
            if delta > 0.0 {
                (static_min.x - moving_max.x, Vec2::new(-1.0, 0.0))
            } else {
                (moving_min.x - static_max.x, Vec2::new(1.0, 0.0))
            }
        } else if delta > 0.0 {
            (static_min.y - moving_max.y, Vec2::new(0.0, -1.0))
        } else {
            (moving_min.y - static_max.y, Vec2::new(0.0, 1.0))
        };
        if !separation.is_finite() {
            continue;
        }
        // Negative gap = collider is already behind the sweep axis. Ignoring it
        // is required in bordered rooms: otherwise every opposite wall shares the
        // cross-axis slab and freezes travel at 0 (skin treated "behind" as hit).
        if separation < 0.0 {
            continue;
        }
        // Within skin of an ahead collider: stop on this axis.
        let travel = if separation <= contact_skin {
            0.0
        } else {
            let allowed = separation - contact_skin;
            if allowed > delta.abs() {
                continue;
            }
            allowed
        };
        let candidate = AxisSweepHit2d {
            distance: travel,
            collider_key: collider.key,
            normal,
        };
        let replace = match nearest {
            None => true,
            Some(current) => candidate
                .distance
                .total_cmp(&current.distance)
                .then_with(|| candidate.collider_key.cmp(&current.collider_key))
                .is_lt(),
        };
        if replace {
            nearest = Some(candidate);
        }
    }
    Ok(nearest)
}

/// A finite ray with a normalized direction used by 3D interaction queries.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ray3d {
    origin: Vec3,
    direction: Vec3,
}

impl Ray3d {
    /// Creates a ray and normalizes `direction`.
    ///
    /// # Errors
    ///
    /// Returns [`PhysicsConfigError::NonFiniteVec3`] for a non-finite origin,
    /// or [`PhysicsConfigError::InvalidRayDirection`] for a zero/non-finite
    /// direction.
    pub fn new(origin: Vec3, direction: Vec3) -> Result<Self, PhysicsConfigError> {
        validate_vec3(origin, "ray origin")?;
        let direction = normalize_ray_direction(direction)?;
        Ok(Self { origin, direction })
    }

    /// Returns the finite world-space ray origin.
    #[must_use]
    pub const fn origin(self) -> Vec3 {
        self.origin
    }

    /// Returns the normalized ray direction.
    #[must_use]
    pub const fn direction(self) -> Vec3 {
        self.direction
    }

    /// Returns the point `distance` world units along this ray.
    #[must_use]
    pub fn point_at(self, distance: f32) -> Vec3 {
        self.origin + self.direction * distance
    }
}

/// Hit information from a ray/sphere query without ECS entity ownership.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RaySphereHit {
    /// Distance along the normalized query ray, in world units.
    pub distance: f32,
    /// World-space impact point.
    pub position: Vec3,
    /// Unit normal facing away from the sphere centre at the impact point.
    pub normal: Vec3,
}

/// Intersects `ray` with one sphere up to `max_distance` world units.
///
/// A ray that starts inside the sphere reports distance zero and a stable
/// fallback normal opposite the ray direction. Tangential contact is a hit.
///
/// # Errors
///
/// Returns [`PhysicsConfigError`] if `center` is non-finite or
/// `max_distance` is invalid.
pub fn raycast_sphere(
    ray: Ray3d,
    center: Vec3,
    sphere: Sphere,
    max_distance: f32,
) -> Result<Option<RaySphereHit>, PhysicsConfigError> {
    validate_vec3(center, "sphere centre")?;
    validate_raycast_distance(max_distance)?;
    let to_origin = ray.origin - center;
    let projection = dot3(to_origin, ray.direction);
    let centre_distance_squared = to_origin.length_squared();
    let discriminant =
        projection.mul_add(projection, sphere.radius() * sphere.radius()) - centre_distance_squared;
    if discriminant < 0.0 || !discriminant.is_finite() {
        return Ok(None);
    }
    let root = discriminant.sqrt();
    let near = -projection - root;
    let far = -projection + root;
    let distance = if near >= 0.0 { near } else { far.max(0.0) };
    if distance > max_distance {
        return Ok(None);
    }
    let position = ray.point_at(distance);
    let normal = if distance <= f32::EPSILON
        && to_origin.length_squared() < sphere.radius() * sphere.radius()
    {
        ray.direction * -1.0
    } else {
        (position - center).normalized_or_zero()
    };
    Ok(Some(RaySphereHit {
        distance,
        position,
        normal,
    }))
}

/// ECS-owned nearest hit from [`raycast_spheres_3d`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RaycastHit3d {
    /// Entity that owns the hit [`SphereCollider3d`].
    pub entity: Entity,
    /// Geometry hit information.
    pub hit: RaySphereHit,
}

/// Returns the nearest ECS sphere ray hit, with deterministic tie breaking.
///
/// Ties at the same finite hit distance resolve to the smallest full
/// generational entity ID. Entities in `ignored` are skipped; this lets a
/// player avoid selecting its own interaction collider without encoding actor
/// policy into the physics layer.
///
/// This is a linear scan intended for prototypes and interaction reach tests.
/// It intentionally has no broad phase, mesh raycast, penetration filtering or
/// network-authority decision.
///
/// # Errors
///
/// Returns [`PhysicsConfigError`] for an invalid max distance. ECS components
/// are validated at construction, so no per-entity invalid-data error exists.
pub fn raycast_spheres_3d(
    world: &mut World,
    ray: Ray3d,
    max_distance: f32,
    ignored: Option<Entity>,
) -> Result<Option<RaycastHit3d>, PhysicsConfigError> {
    validate_raycast_distance(max_distance)?;
    let mut hits = Vec::new();
    let mut colliders = world.query::<(Entity, &Position3d, &SphereCollider3d)>();
    for (entity, position, collider) in colliders.iter(world) {
        if Some(entity) == ignored {
            continue;
        }
        if let Some(hit) = raycast_sphere(ray, position.get(), collider.sphere(), max_distance)? {
            hits.push(RaycastHit3d { entity, hit });
        }
    }
    hits.sort_by(|left, right| {
        left.hit
            .distance
            .total_cmp(&right.hit.distance)
            .then_with(|| left.entity.to_bits().cmp(&right.entity.to_bits()))
    });
    Ok(hits.into_iter().next())
}

/// ECS-owned overlap hit returned by [`overlap_spheres_3d`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SphereOverlap3d {
    /// Entity that owns the overlapping collider.
    pub entity: Entity,
    /// Contact normal and penetration from the query sphere toward `entity`.
    pub contact: Contact<Vec3>,
}

/// Returns all ECS spheres strictly overlapping a query sphere.
///
/// Results are sorted by full generational entity ID. Exact touching is not an
/// overlap, consistent with [`collide_spheres`]. Pass an entity in `ignored`
/// to omit a query actor's own collider.
///
/// # Errors
///
/// Returns [`PhysicsConfigError::NonFiniteVec3`] when `center` is non-finite.
pub fn overlap_spheres_3d(
    world: &mut World,
    center: Vec3,
    sphere: Sphere,
    ignored: Option<Entity>,
) -> Result<Vec<SphereOverlap3d>, PhysicsConfigError> {
    validate_vec3(center, "overlap sphere centre")?;
    let mut overlaps = Vec::new();
    let mut colliders = world.query::<(Entity, &Position3d, &SphereCollider3d)>();
    for (entity, position, collider) in colliders.iter(world) {
        if Some(entity) == ignored {
            continue;
        }
        if let Some(contact) = collide_spheres(center, sphere, position.get(), collider.sphere()) {
            overlaps.push(SphereOverlap3d { entity, contact });
        }
    }
    overlaps.sort_by_key(|overlap| overlap.entity.to_bits());
    Ok(overlaps)
}

/// A finite axis-aligned box represented by strictly positive half extents.
///
/// The box has no rotation; query functions combine it with a world-space
/// centre. This is a detection/query primitive, not an OBB or rigid body.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aabb3d {
    half_extents: Vec3,
}

impl Aabb3d {
    /// Creates an axis-aligned box with finite, strictly positive half extents.
    ///
    /// # Errors
    ///
    /// Returns [`PhysicsConfigError::InvalidAabbHalfExtents`] for zero,
    /// negative, NaN or infinite components.
    pub fn new(half_extents: Vec3) -> Result<Self, PhysicsConfigError> {
        if half_extents.x.is_finite()
            && half_extents.y.is_finite()
            && half_extents.z.is_finite()
            && half_extents.x > 0.0
            && half_extents.y > 0.0
            && half_extents.z > 0.0
        {
            Ok(Self { half_extents })
        } else {
            Err(PhysicsConfigError::InvalidAabbHalfExtents(half_extents))
        }
    }

    /// Returns the positive local half extents.
    #[must_use]
    pub const fn half_extents(self) -> Vec3 {
        self.half_extents
    }
}

/// ECS axis-aligned box collider for the lightweight 3D query layer.
#[derive(Clone, Copy, Component, Debug, PartialEq)]
pub struct AabbCollider3d(Aabb3d);

impl AabbCollider3d {
    /// Creates an ECS AABB collider with validated half extents.
    ///
    /// # Errors
    ///
    /// Returns [`PhysicsConfigError::InvalidAabbHalfExtents`] for invalid extents.
    pub fn new(half_extents: Vec3) -> Result<Self, PhysicsConfigError> {
        Aabb3d::new(half_extents).map(Self)
    }

    /// Returns the local AABB shape.
    #[must_use]
    pub const fn aabb(self) -> Aabb3d {
        self.0
    }
}

/// Returns whether `point` is inside or on the boundary of an AABB.
///
/// # Errors
///
/// Returns [`PhysicsConfigError::NonFiniteVec3`] for a non-finite point or centre.
pub fn point_in_aabb_3d(
    point: Vec3,
    center: Vec3,
    aabb: Aabb3d,
) -> Result<bool, PhysicsConfigError> {
    validate_vec3(point, "AABB point")?;
    validate_vec3(center, "AABB centre")?;
    let offset = point - center;
    Ok(offset.x.abs() <= aabb.half_extents.x
        && offset.y.abs() <= aabb.half_extents.y
        && offset.z.abs() <= aabb.half_extents.z)
}

/// Geometry hit from a ray/AABB query.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RayAabbHit3d {
    /// Distance along the normalized ray in world units.
    pub distance: f32,
    /// World-space hit point.
    pub position: Vec3,
    /// Outward face normal. Rays originating inside use the stable opposite-ray normal.
    pub normal: Vec3,
}

/// Intersects `ray` with an AABB up to `max_distance` world units.
///
/// A ray starting inside or on the boundary reports distance zero and uses a
/// stable normal opposite its direction. Parallel slab cases are handled
/// without division by zero.
///
/// # Errors
///
/// Returns [`PhysicsConfigError`] for a non-finite centre or invalid distance.
pub fn raycast_aabb_3d(
    ray: Ray3d,
    center: Vec3,
    aabb: Aabb3d,
    max_distance: f32,
) -> Result<Option<RayAabbHit3d>, PhysicsConfigError> {
    validate_vec3(center, "AABB centre")?;
    validate_raycast_distance(max_distance)?;
    if point_in_aabb_3d(ray.origin, center, aabb)? {
        return Ok(Some(RayAabbHit3d {
            distance: 0.0,
            position: ray.origin,
            normal: ray.direction * -1.0,
        }));
    }
    let minimum = center - aabb.half_extents;
    let maximum = center + aabb.half_extents;
    let origins = [ray.origin.x, ray.origin.y, ray.origin.z];
    let directions = [ray.direction.x, ray.direction.y, ray.direction.z];
    let minima = [minimum.x, minimum.y, minimum.z];
    let maxima = [maximum.x, maximum.y, maximum.z];
    let mut entry = f32::NEG_INFINITY;
    let mut exit = f32::INFINITY;
    let mut entry_normal = Vec3::ZERO;
    for axis in 0..3 {
        if directions[axis].abs() <= f32::EPSILON {
            if origins[axis] < minima[axis] || origins[axis] > maxima[axis] {
                return Ok(None);
            }
            continue;
        }
        let first = (minima[axis] - origins[axis]) / directions[axis];
        let second = (maxima[axis] - origins[axis]) / directions[axis];
        let (near, far, normal) = if first <= second {
            (first, second, axis_normal(axis, -1.0))
        } else {
            (second, first, axis_normal(axis, 1.0))
        };
        if near > entry {
            entry = near;
            entry_normal = normal;
        }
        exit = exit.min(far);
        if entry > exit {
            return Ok(None);
        }
    }
    if entry < 0.0 || entry > max_distance || !entry.is_finite() {
        return Ok(None);
    }
    Ok(Some(RayAabbHit3d {
        distance: entry,
        position: ray.point_at(entry),
        normal: entry_normal,
    }))
}

/// ECS-owned nearest hit from [`raycast_aabbs_3d`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RaycastAabbHit3d {
    /// Entity that owns the hit [`AabbCollider3d`].
    pub entity: Entity,
    /// Geometry hit information.
    pub hit: RayAabbHit3d,
}

/// Returns the nearest ECS AABB hit with deterministic entity-ID tie breaking.
///
/// `ignored` excludes one entity (normally the querying actor). This is an
/// O(n) narrow-phase query: it has no broad phase, mesh test, collision
/// response, OBB rotation or CCD.
///
/// # Errors
///
/// Returns [`PhysicsConfigError`] for an invalid maximum distance.
pub fn raycast_aabbs_3d(
    world: &mut World,
    ray: Ray3d,
    max_distance: f32,
    ignored: Option<Entity>,
) -> Result<Option<RaycastAabbHit3d>, PhysicsConfigError> {
    validate_raycast_distance(max_distance)?;
    let mut hits = Vec::new();
    let mut colliders = world.query::<(Entity, &Position3d, &AabbCollider3d)>();
    for (entity, position, collider) in colliders.iter(world) {
        if Some(entity) == ignored {
            continue;
        }
        if let Some(hit) = raycast_aabb_3d(ray, position.get(), collider.aabb(), max_distance)? {
            hits.push(RaycastAabbHit3d { entity, hit });
        }
    }
    hits.sort_by(|left, right| {
        left.hit
            .distance
            .total_cmp(&right.hit.distance)
            .then_with(|| left.entity.to_bits().cmp(&right.entity.to_bits()))
    });
    Ok(hits.into_iter().next())
}

/// ECS AABB overlap result. It contains no response/contact resolution data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AabbOverlap3d {
    /// Entity owning the overlapping AABB collider.
    pub entity: Entity,
}

/// Returns strictly overlapping ECS AABBs in deterministic entity-ID order.
///
/// Exact face/edge/corner touching is not an overlap. This query is O(n) and
/// has no broad phase, response, OBB support, mesh collision or CCD.
///
/// # Errors
///
/// Returns [`PhysicsConfigError::NonFiniteVec3`] when `center` is non-finite.
pub fn overlap_aabbs_3d(
    world: &mut World,
    center: Vec3,
    aabb: Aabb3d,
    ignored: Option<Entity>,
) -> Result<Vec<AabbOverlap3d>, PhysicsConfigError> {
    validate_vec3(center, "overlap AABB centre")?;
    let mut overlaps = Vec::new();
    let mut colliders = world.query::<(Entity, &Position3d, &AabbCollider3d)>();
    for (entity, position, collider) in colliders.iter(world) {
        if Some(entity) != ignored && aabbs_overlap(center, aabb, position.get(), collider.aabb()) {
            overlaps.push(AabbOverlap3d { entity });
        }
    }
    overlaps.sort_by_key(|overlap| overlap.entity.to_bits());
    Ok(overlaps)
}

fn aabbs_overlap(left_center: Vec3, left: Aabb3d, right_center: Vec3, right: Aabb3d) -> bool {
    let delta = left_center - right_center;
    delta.x.abs() < left.half_extents.x + right.half_extents.x
        && delta.y.abs() < left.half_extents.y + right.half_extents.y
        && delta.z.abs() < left.half_extents.z + right.half_extents.z
}

fn axis_normal(axis: usize, sign: f32) -> Vec3 {
    match axis {
        0 => Vec3::new(sign, 0.0, 0.0),
        1 => Vec3::new(0.0, sign, 0.0),
        _ => Vec3::new(0.0, 0.0, sign),
    }
}

fn axis_normal_2d(axis: usize, sign: f32) -> Vec2 {
    match axis {
        0 => Vec2::new(sign, 0.0),
        _ => Vec2::new(0.0, sign),
    }
}

fn validate_delta(delta_seconds: f32) -> Result<(), PhysicsConfigError> {
    if delta_seconds.is_finite() && delta_seconds >= 0.0 {
        Ok(())
    } else {
        Err(PhysicsConfigError::InvalidDeltaSeconds(delta_seconds))
    }
}

fn validate_vec3(value: Vec3, field: &'static str) -> Result<(), PhysicsConfigError> {
    if value.x.is_finite() && value.y.is_finite() && value.z.is_finite() {
        Ok(())
    } else {
        Err(PhysicsConfigError::NonFiniteVec3 { field })
    }
}

fn validate_vec2(value: Vec2, field: &'static str) -> Result<(), PhysicsConfigError> {
    if value.x.is_finite() && value.y.is_finite() {
        Ok(())
    } else {
        Err(PhysicsConfigError::NonFiniteVec2 { field })
    }
}

fn aabb_bounds_2d(
    center: Vec2,
    aabb: Aabb2d,
    field: &'static str,
) -> Result<(Vec2, Vec2), PhysicsConfigError> {
    let minimum = center - aabb.half_extents;
    let maximum = center + aabb.half_extents;
    validate_vec2(minimum, field)?;
    validate_vec2(maximum, field)?;
    Ok((minimum, maximum))
}

fn normalize_ray_direction_2d(direction: Vec2) -> Result<Vec2, PhysicsConfigError> {
    let normalized = direction.normalized_or_zero();
    if normalized == Vec2::ZERO {
        Err(PhysicsConfigError::InvalidRayDirection)
    } else {
        Ok(normalized)
    }
}

fn normalize_ray_direction(direction: Vec3) -> Result<Vec3, PhysicsConfigError> {
    let normalized = direction.normalized_or_zero();
    if normalized == Vec3::ZERO {
        Err(PhysicsConfigError::InvalidRayDirection)
    } else {
        Ok(normalized)
    }
}

fn validate_raycast_distance(max_distance: f32) -> Result<(), PhysicsConfigError> {
    if max_distance.is_finite() && max_distance >= 0.0 {
        Ok(())
    } else {
        Err(PhysicsConfigError::InvalidRaycastDistance(max_distance))
    }
}

fn dot3(left: Vec3, right: Vec3) -> f32 {
    left.x
        .mul_add(right.x, left.y.mul_add(right.y, left.z * right.z))
}

fn cross3(left: Vec3, right: Vec3) -> Vec3 {
    Vec3::new(
        left.y * right.z - left.z * right.y,
        left.z * right.x - left.x * right.z,
        left.x * right.y - left.y * right.x,
    )
}

/// Returns the finite distance of a two-sided ray/triangle intersection.
///
/// This is the Moller-Trumbore test.  Keeping it private avoids exposing an
/// unvalidated raw-triangle API; callers normally own a [`TriangleMesh3d`]
/// and receive a stable source-triangle index from [`TriangleMesh3d::raycast`].
fn ray_triangle_distance(ray: Ray3d, [first, second, third]: [Vec3; 3]) -> Option<f32> {
    let first_edge = second - first;
    let second_edge = third - first;
    let direction_cross_second_edge = cross3(ray.direction(), second_edge);
    let determinant = dot3(first_edge, direction_cross_second_edge);
    if determinant.abs() <= f32::EPSILON {
        return None;
    }
    let inverse_determinant = determinant.recip();
    let origin_offset = ray.origin() - first;
    let first_barycentric = dot3(origin_offset, direction_cross_second_edge) * inverse_determinant;
    if !(0.0..=1.0).contains(&first_barycentric) {
        return None;
    }
    let offset_cross_first_edge = cross3(origin_offset, first_edge);
    let second_barycentric = dot3(ray.direction(), offset_cross_first_edge) * inverse_determinant;
    if second_barycentric < 0.0 || first_barycentric + second_barycentric > 1.0 {
        return None;
    }
    let distance = dot3(second_edge, offset_cross_first_edge) * inverse_determinant;
    (distance >= 0.0).then_some(distance.max(0.0))
}

fn finite_vec3(value: Vec3) -> bool {
    value.x.is_finite() && value.y.is_finite() && value.z.is_finite()
}

fn min_walkable_normal_y_from_slope(
    max_slope_radians: f32,
) -> Result<f32, TriangleMeshQueryError> {
    if !max_slope_radians.is_finite()
        || max_slope_radians <= 0.0
        || max_slope_radians > std::f32::consts::FRAC_PI_2
    {
        return Err(TriangleMeshQueryError::InvalidMaxWalkableSlope(
            max_slope_radians,
        ));
    }
    Ok(max_slope_radians.cos())
}

impl TriangleAabb3d {
    fn from_face([first, second, third]: [Vec3; 3]) -> Self {
        Self {
            minimum: Vec3::new(
                first.x.min(second.x).min(third.x),
                first.y.min(second.y).min(third.y),
                first.z.min(second.z).min(third.z),
            ),
            maximum: Vec3::new(
                first.x.max(second.x).max(third.x),
                first.y.max(second.y).max(third.y),
                first.z.max(second.z).max(third.z),
            ),
        }
    }

    /// Squared distance from a point to this box is compared with a sphere
    /// radius. This is intentionally inclusive: a tangent box proceeds to the
    /// exact test, matching `resolve_sphere`'s strict contact semantics.
    fn overlaps_sphere(self, point: Vec3, radius: f32) -> bool {
        let axis_distance = |value: f32, minimum: f32, maximum: f32| {
            if value < minimum {
                minimum - value
            } else if value > maximum {
                value - maximum
            } else {
                0.0
            }
        };
        let dx = axis_distance(point.x, self.minimum.x, self.maximum.x);
        let dy = axis_distance(point.y, self.minimum.y, self.maximum.y);
        let dz = axis_distance(point.z, self.minimum.z, self.maximum.z);
        dx.mul_add(dx, dy.mul_add(dy, dz * dz)) <= radius * radius
    }

    fn union(self, other: Self) -> Self {
        Self {
            minimum: Vec3::new(
                self.minimum.x.min(other.minimum.x),
                self.minimum.y.min(other.minimum.y),
                self.minimum.z.min(other.minimum.z),
            ),
            maximum: Vec3::new(
                self.maximum.x.max(other.maximum.x),
                self.maximum.y.max(other.maximum.y),
                self.maximum.z.max(other.maximum.z),
            ),
        }
    }

    /// Returns the conservative entry distance of a finite ray segment.
    fn ray_entry_distance(self, ray: Ray3d, max_distance: f32) -> Option<f32> {
        let origin = ray.origin();
        let direction = ray.direction();
        let mut entry: f32 = 0.0;
        let mut exit = max_distance;
        for (origin, direction, minimum, maximum) in [
            (origin.x, direction.x, self.minimum.x, self.maximum.x),
            (origin.y, direction.y, self.minimum.y, self.maximum.y),
            (origin.z, direction.z, self.minimum.z, self.maximum.z),
        ] {
            if direction == 0.0 {
                if origin < minimum || origin > maximum {
                    return None;
                }
                continue;
            }
            let first = (minimum - origin) / direction;
            let second = (maximum - origin) / direction;
            let near = first.min(second);
            let far = first.max(second);
            entry = entry.max(near);
            exit = exit.min(far);
            if entry > exit {
                return None;
            }
        }
        (exit >= 0.0 && entry <= max_distance).then_some(entry.max(0.0))
    }
}

fn build_triangle_bvh(
    bounds: &[TriangleAabb3d],
    first: usize,
    count: usize,
    depth: usize,
    nodes: &mut Vec<TriangleBvhNode3d>,
    stats: &mut TriangleMeshAccelerationStats3d,
) -> u32 {
    let node_index = compact_triangle_bvh_index(nodes.len());
    nodes.push(TriangleBvhNode3d {
        bounds: bounds[first],
        kind: TriangleBvhNodeKind3d::Leaf {
            first: compact_triangle_bvh_index(first),
            count: compact_triangle_bvh_index(count),
        },
    });
    stats.maximum_depth = stats.maximum_depth.max(depth);

    let leaf_count = count.div_ceil(TRIANGLE_BVH_LEAF_CAPACITY);
    if leaf_count == 1 {
        let mut leaf_bounds = bounds[first];
        for bound in &bounds[first + 1..first + count] {
            leaf_bounds = leaf_bounds.union(*bound);
        }
        nodes[triangle_bvh_index(node_index)].bounds = leaf_bounds;
        stats.leaves += 1;
        return node_index;
    }
    let left_leaf_count = leaf_count / 2;
    let left_count = left_leaf_count * TRIANGLE_BVH_LEAF_CAPACITY;
    let right_count = count - left_count;
    let left = build_triangle_bvh(bounds, first, left_count, depth + 1, nodes, stats);
    let right = build_triangle_bvh(
        bounds,
        first + left_count,
        right_count,
        depth + 1,
        nodes,
        stats,
    );
    let left_bounds = nodes[triangle_bvh_index(left)].bounds;
    let right_bounds = nodes[triangle_bvh_index(right)].bounds;
    nodes[triangle_bvh_index(node_index)] = TriangleBvhNode3d {
        bounds: left_bounds.union(right_bounds),
        kind: TriangleBvhNodeKind3d::Branch { left, right },
    };
    node_index
}

fn compact_triangle_bvh_index(index: usize) -> u32 {
    u32::try_from(index).expect("triangle BVH size is validated before construction")
}

fn triangle_bvh_index(index: u32) -> usize {
    usize::try_from(index).expect("u32 indices fit every supported Rust target")
}

fn push_triangle_bvh_stack(
    stack: &mut [u32; TRIANGLE_BVH_STACK_CAPACITY],
    stack_len: &mut usize,
    node: u32,
) {
    debug_assert!(*stack_len < stack.len());
    stack[*stack_len] = node;
    *stack_len += 1;
}

fn closest_point_on_triangle(point: Vec3, triangle: [Vec3; 3]) -> Vec3 {
    let [a, b, c] = triangle;
    let ab = b - a;
    let ac = c - a;
    let ap = point - a;
    let d1 = dot3(ab, ap);
    let d2 = dot3(ac, ap);
    if d1 <= 0.0 && d2 <= 0.0 {
        return a;
    }
    let bp = point - b;
    let d3 = dot3(ab, bp);
    let d4 = dot3(ac, bp);
    if d3 >= 0.0 && d4 <= d3 {
        return b;
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return a + ab * (d1 / (d1 - d3));
    }
    let cp = point - c;
    let d5 = dot3(ab, cp);
    let d6 = dot3(ac, cp);
    if d6 >= 0.0 && d5 <= d6 {
        return c;
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return a + ac * (d2 / (d2 - d6));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        let bc = c - b;
        return b + bc * ((d4 - d3) / ((d4 - d3) + (d5 - d6)));
    }
    let denominator = (va + vb + vc).recip();
    a + ab * (vb * denominator) + ac * (vc * denominator)
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // Values chosen in these tests are exactly representable.
mod tests {
    use super::*;

    #[test]
    fn body_integrates_explicit_euler() {
        let mut body = Body2d::new(Vec2::new(1.0, 2.0), Vec2::new(4.0, -2.0));
        body.step(0.5).expect("valid step");
        assert_eq!(body.position, Vec2::new(3.0, 1.0));
    }

    #[test]
    fn overlap_has_expected_normal_and_penetration() {
        let contact = collide_circles(
            Vec2::ZERO,
            Circle::new(1.0).expect("valid circle"),
            Vec2::new(1.5, 0.0),
            Circle::new(1.0).expect("valid circle"),
        )
        .expect("circles overlap");
        assert_eq!(contact.normal, Vec2::new(1.0, 0.0));
        assert_eq!(contact.penetration, 0.5);
    }

    #[test]
    fn ecs_step_is_ordered_and_does_not_resolve_collisions_implicitly() {
        let mut world = World::new();
        let first = world
            .spawn((
                Position2d(Vec2::ZERO),
                Velocity2d(Vec2::new(1.0, 0.0)),
                CircleCollider(Circle::new(1.0).expect("valid")),
            ))
            .id();
        let second = world
            .spawn((
                Position2d(Vec2::new(2.5, 0.0)),
                CircleCollider(Circle::new(1.0).expect("valid")),
            ))
            .id();
        let collisions = step_ecs_2d(&mut world, 1.0).expect("valid step");
        assert_eq!(
            world.get::<Position2d>(first).expect("position").0,
            Vec2::new(1.0, 0.0)
        );
        assert_eq!(collisions.len(), 1);
        let mut expected = [first.to_bits(), second.to_bits()];
        expected.sort_unstable();
        assert_eq!(collisions[0].first.to_bits(), expected[0]);
        assert_eq!(collisions[0].second.to_bits(), expected[1]);
    }

    #[test]
    fn invalid_configuration_is_rejected() {
        assert!(Circle::new(0.0).is_err());
        assert!(Sphere::new(f32::INFINITY).is_err());
        assert!(Body3d::new(Vec3::ZERO, Vec3::ZERO).step(-1.0).is_err());
    }

    #[test]
    fn ecs_3d_step_reports_ordered_detection_events() {
        let mut world = World::new();
        let first = world
            .spawn((
                Position3d::new(Vec3::ZERO).expect("finite position"),
                Velocity3d::new(Vec3::new(1.0, 0.0, 0.0)).expect("finite velocity"),
                SphereCollider3d::new(1.0).expect("positive radius"),
            ))
            .id();
        let second = world
            .spawn((
                Position3d::new(Vec3::new(2.5, 0.0, 0.0)).expect("finite position"),
                SphereCollider3d::new(1.0).expect("positive radius"),
            ))
            .id();

        let events = step_ecs_3d(&mut world, 1.0).expect("valid step");
        assert_eq!(
            world.get::<Position3d>(first).expect("position").get(),
            Vec3::new(1.0, 0.0, 0.0)
        );
        assert_eq!(events.len(), 1);
        let mut expected = [first.to_bits(), second.to_bits()];
        expected.sort_unstable();
        assert_eq!(events[0].first.to_bits(), expected[0]);
        assert_eq!(events[0].second.to_bits(), expected[1]);
    }

    #[test]
    fn raycast_selects_nearest_then_entity_id_and_can_ignore_self() {
        let mut world = World::new();
        let first = world
            .spawn((
                Position3d::new(Vec3::new(0.0, 0.0, -3.0)).expect("finite position"),
                SphereCollider3d::new(1.0).expect("positive radius"),
            ))
            .id();
        let second = world
            .spawn((
                Position3d::new(Vec3::new(0.0, 0.0, -3.0)).expect("finite position"),
                SphereCollider3d::new(1.0).expect("positive radius"),
            ))
            .id();
        let ray = Ray3d::new(Vec3::ZERO, Vec3::new(0.0, 0.0, -2.0)).expect("valid ray");

        let hit = raycast_spheres_3d(&mut world, ray, 10.0, None)
            .expect("valid query")
            .expect("sphere should be hit");
        let expected = if first.to_bits() < second.to_bits() {
            first
        } else {
            second
        };
        assert_eq!(hit.entity, expected);
        assert_eq!(hit.hit.distance, 2.0);

        let after_ignoring = raycast_spheres_3d(&mut world, ray, 10.0, Some(expected))
            .expect("valid query")
            .expect("other sphere should be hit");
        assert_ne!(after_ignoring.entity, expected);
    }

    #[test]
    fn overlap_query_is_entity_ordered_and_rejects_non_finite_center() {
        let mut world = World::new();
        let first = world
            .spawn((
                Position3d::new(Vec3::new(0.5, 0.0, 0.0)).expect("finite position"),
                SphereCollider3d::new(1.0).expect("positive radius"),
            ))
            .id();
        let second = world
            .spawn((
                Position3d::new(Vec3::new(-0.5, 0.0, 0.0)).expect("finite position"),
                SphereCollider3d::new(1.0).expect("positive radius"),
            ))
            .id();
        let overlaps = overlap_spheres_3d(
            &mut world,
            Vec3::ZERO,
            Sphere::new(1.0).expect("positive radius"),
            None,
        )
        .expect("finite query");
        assert_eq!(overlaps.len(), 2);
        let mut expected = [first.to_bits(), second.to_bits()];
        expected.sort_unstable();
        assert_eq!(overlaps[0].entity.to_bits(), expected[0]);
        assert_eq!(overlaps[1].entity.to_bits(), expected[1]);
        assert!(matches!(
            overlap_spheres_3d(
                &mut world,
                Vec3::new(f32::NAN, 0.0, 0.0),
                Sphere::new(1.0).expect("positive radius"),
                None,
            ),
            Err(PhysicsConfigError::NonFiniteVec3 { .. })
        ));
    }

    #[test]
    fn aabb_raycast_reports_entry_face_and_inside_origin() {
        let box_shape = Aabb3d::new(Vec3::new(1.0, 1.0, 1.0)).expect("positive extents");
        let ray =
            Ray3d::new(Vec3::new(0.0, 0.0, 3.0), Vec3::new(0.0, 0.0, -2.0)).expect("valid ray");
        let hit = raycast_aabb_3d(ray, Vec3::ZERO, box_shape, 10.0)
            .expect("query")
            .expect("entry hit");
        assert_eq!(hit.distance, 2.0);
        assert_eq!(hit.position, Vec3::new(0.0, 0.0, 1.0));
        assert_eq!(hit.normal, Vec3::new(0.0, 0.0, 1.0));

        let inside = Ray3d::new(Vec3::ZERO, Vec3::new(1.0, 0.0, 0.0)).expect("valid ray");
        let inside_hit = raycast_aabb_3d(inside, Vec3::ZERO, box_shape, 10.0)
            .expect("query")
            .expect("inside hit");
        assert_eq!(inside_hit.distance, 0.0);
        assert_eq!(inside_hit.normal, Vec3::new(-1.0, 0.0, 0.0));
    }

    #[test]
    fn aabb_ecs_raycast_ties_by_entity_and_supports_self_exclusion() {
        let mut world = World::new();
        let first = world
            .spawn((
                Position3d::new(Vec3::new(0.0, 0.0, -3.0)).expect("finite"),
                AabbCollider3d::new(Vec3::new(1.0, 1.0, 1.0)).expect("valid"),
            ))
            .id();
        let second = world
            .spawn((
                Position3d::new(Vec3::new(0.0, 0.0, -3.0)).expect("finite"),
                AabbCollider3d::new(Vec3::new(1.0, 1.0, 1.0)).expect("valid"),
            ))
            .id();
        let ray = Ray3d::new(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0)).expect("valid ray");
        let hit = raycast_aabbs_3d(&mut world, ray, 10.0, None)
            .expect("query")
            .expect("hit");
        let expected = if first.to_bits() < second.to_bits() {
            first
        } else {
            second
        };
        assert_eq!(hit.entity, expected);
        let next = raycast_aabbs_3d(&mut world, ray, 10.0, Some(expected))
            .expect("query")
            .expect("other hit");
        assert_ne!(next.entity, expected);
    }

    #[test]
    fn aabb_rejects_invalid_extents_and_reports_strict_overlaps() {
        assert!(matches!(
            Aabb3d::new(Vec3::new(0.0, 1.0, 1.0)),
            Err(PhysicsConfigError::InvalidAabbHalfExtents(_))
        ));
        let mut world = World::new();
        let overlapping = world
            .spawn((
                Position3d::new(Vec3::new(1.5, 0.0, 0.0)).expect("finite"),
                AabbCollider3d::new(Vec3::new(1.0, 1.0, 1.0)).expect("valid"),
            ))
            .id();
        let _touching = world
            .spawn((
                Position3d::new(Vec3::new(2.0, 0.0, 0.0)).expect("finite"),
                AabbCollider3d::new(Vec3::new(1.0, 1.0, 1.0)).expect("valid"),
            ))
            .id();
        let overlaps = overlap_aabbs_3d(
            &mut world,
            Vec3::ZERO,
            Aabb3d::new(Vec3::new(1.0, 1.0, 1.0)).expect("valid"),
            None,
        )
        .expect("query");
        assert_eq!(overlaps.len(), 1);
        assert_eq!(overlaps[0].entity, overlapping);
    }

    #[test]
    fn aabb_2d_rejects_invalid_extents_and_uses_strict_touching_semantics() {
        assert!(matches!(
            Aabb2d::new(Vec2::new(0.0, 1.0)),
            Err(PhysicsConfigError::InvalidAabb2dHalfExtents(_))
        ));
        assert!(matches!(
            Aabb2d::new(Vec2::new(f32::MAX, 1.0)),
            Err(PhysicsConfigError::InvalidAabb2dHalfExtents(_))
        ));
        let shape = Aabb2d::new(Vec2::new(1.0, 1.0)).expect("positive extents");
        assert!(point_in_aabb_2d(Vec2::new(1.0, 0.0), Vec2::ZERO, shape).expect("finite point"));
        assert_eq!(
            collide_aabbs_2d(Vec2::ZERO, shape, Vec2::new(2.0, 0.0), shape)
                .expect("finite centres"),
            None
        );
        let contact = collide_aabbs_2d(Vec2::ZERO, shape, Vec2::new(1.5, 0.0), shape)
            .expect("finite centres")
            .expect("strict overlap");
        assert_eq!(contact.normal, Vec2::new(1.0, 0.0));
        assert_eq!(contact.penetration, 0.5);
    }

    #[test]
    fn aabb_2d_ecs_overlap_is_ordered_and_bounded() {
        let shape = Aabb2d::new(Vec2::new(1.0, 1.0)).expect("positive extents");
        let mut world = World::new();
        let first = world
            .spawn((
                Position2d(Vec2::new(0.5, 0.0)),
                AabbCollider2d::new(Vec2::new(1.0, 1.0)).expect("valid"),
            ))
            .id();
        let second = world
            .spawn((
                Position2d(Vec2::new(-0.5, 0.0)),
                AabbCollider2d::new(Vec2::new(1.0, 1.0)).expect("valid"),
            ))
            .id();
        let overlaps = overlap_aabbs_2d(
            &mut world,
            Vec2::ZERO,
            shape,
            None,
            AabbQueryLimits2d::new(2).expect("positive result budget"),
        )
        .expect("finite positions");
        let mut expected = [first.to_bits(), second.to_bits()];
        expected.sort_unstable();
        assert_eq!(overlaps.len(), 2);
        assert_eq!(overlaps[0].entity.to_bits(), expected[0]);
        assert_eq!(overlaps[1].entity.to_bits(), expected[1]);

        assert!(matches!(
            overlap_aabbs_2d(
                &mut world,
                Vec2::ZERO,
                shape,
                None,
                AabbQueryLimits2d::new(1).expect("positive result budget"),
            ),
            Err(AabbQueryError2d::ResultLimitExceeded { maximum: 1 })
        ));
    }

    #[test]
    fn aabb_2d_raycast_reports_entry_face_inside_origin_and_entity_ties() {
        let shape = Aabb2d::new(Vec2::new(1.0, 1.0)).expect("positive extents");
        let ray = Ray2d::new(Vec2::new(3.0, 0.0), Vec2::new(-2.0, 0.0)).expect("valid ray");
        let hit = raycast_aabb_2d(ray, Vec2::ZERO, shape, 10.0)
            .expect("query")
            .expect("entry hit");
        assert_eq!(hit.distance, 2.0);
        assert_eq!(hit.position, Vec2::new(1.0, 0.0));
        assert_eq!(hit.normal, Vec2::new(1.0, 0.0));
        let inside = Ray2d::new(Vec2::ZERO, Vec2::new(1.0, 0.0)).expect("valid ray");
        assert_eq!(
            raycast_aabb_2d(inside, Vec2::ZERO, shape, 10.0)
                .expect("query")
                .expect("inside hit")
                .normal,
            Vec2::new(-1.0, 0.0)
        );

        let mut world = World::new();
        let first = world
            .spawn((
                Position2d(Vec2::new(-3.0, 0.0)),
                AabbCollider2d::new(Vec2::new(1.0, 1.0)).expect("valid"),
            ))
            .id();
        let second = world
            .spawn((
                Position2d(Vec2::new(-3.0, 0.0)),
                AabbCollider2d::new(Vec2::new(1.0, 1.0)).expect("valid"),
            ))
            .id();
        let query_ray = Ray2d::new(Vec2::ZERO, Vec2::new(-1.0, 0.0)).expect("valid ray");
        let hit = raycast_aabbs_2d(&mut world, query_ray, 10.0, None)
            .expect("query")
            .expect("hit");
        let expected = if first.to_bits() < second.to_bits() {
            first
        } else {
            second
        };
        assert_eq!(hit.entity, expected);
    }

    fn static_aabb(key: u64, center: Vec2, half_extents: Vec2) -> StaticAabb2d {
        StaticAabb2d::new(
            key,
            center,
            Aabb2d::new(half_extents).expect("positive safe extents"),
        )
        .expect("finite static bounds")
    }

    #[test]
    fn static_aabb_broadphase_returns_sorted_candidates_and_exact_overlaps() {
        let limits = StaticAabbBroadphaseLimits2d::new(8, 8).expect("valid limits");
        let index = StaticAabbBroadphase2d::build(
            [
                static_aabb(40, Vec2::new(8.0, 0.0), Vec2::new(1.0, 1.0)),
                static_aabb(9, Vec2::new(0.5, 0.0), Vec2::new(1.0, 1.0)),
                static_aabb(2, Vec2::new(-0.5, 0.0), Vec2::new(1.0, 1.0)),
                // Касается границы области: кандидат, но не строгий overlap.
                static_aabb(20, Vec2::new(3.0, 0.0), Vec2::new(1.0, 1.0)),
            ],
            limits,
        )
        .expect("unique bounded colliders");
        let query = Aabb2d::new(Vec2::new(2.0, 1.0)).expect("valid query");
        assert_eq!(
            index
                .candidate_keys_in_region(Vec2::ZERO, query)
                .expect("valid query"),
            [2, 9, 20]
        );
        assert_eq!(
            index
                .overlaps_in_region(Vec2::ZERO, query)
                .expect("valid query")
                .iter()
                .map(|collider| collider.key())
                .collect::<Vec<_>>(),
            [2, 9]
        );
    }

    #[test]
    fn static_aabb_broadphase_raycast_is_precise_and_ties_by_key() {
        let limits = StaticAabbBroadphaseLimits2d::new(8, 8).expect("valid limits");
        let index = StaticAabbBroadphase2d::build(
            [
                static_aabb(10, Vec2::new(4.0, 0.0), Vec2::new(1.0, 1.0)),
                static_aabb(3, Vec2::new(4.0, 0.0), Vec2::new(1.0, 1.0)),
                // Попадает в ограничивающий прямоугольник луча, но не в сам луч.
                static_aabb(20, Vec2::new(2.0, 1.5), Vec2::new(0.5, 0.5)),
            ],
            limits,
        )
        .expect("unique bounded colliders");
        let ray = Ray2d::new(Vec2::ZERO, Vec2::new(1.0, 0.0)).expect("valid ray");
        assert_eq!(
            index
                .candidate_keys_for_ray(ray, 10.0)
                .expect("valid ray query"),
            [3, 10]
        );
        let hit = index
            .raycast(ray, 10.0)
            .expect("valid ray query")
            .expect("precise hit");
        assert_eq!(hit.collider_key, 3);
        assert_eq!(hit.hit.distance, 3.0);
    }

    #[test]
    fn static_aabb_broadphase_updates_atomically_and_reports_limits() {
        assert!(matches!(
            StaticAabbBroadphaseLimits2d::new(0, 1),
            Err(PhysicsConfigError::InvalidBroadphaseColliderLimit(0))
        ));
        assert!(matches!(
            StaticAabbBroadphaseLimits2d::new(1, 0),
            Err(PhysicsConfigError::InvalidBroadphaseCandidateLimit(0))
        ));

        let limits = StaticAabbBroadphaseLimits2d::new(2, 1).expect("valid limits");
        let mut index = StaticAabbBroadphase2d::build(
            [static_aabb(1, Vec2::ZERO, Vec2::new(1.0, 1.0))],
            limits,
        )
        .expect("one collider");
        index
            .update(static_aabb(1, Vec2::new(5.0, 0.0), Vec2::new(1.0, 1.0)))
            .expect("known key");
        assert_eq!(
            index.get(1).expect("known key").center(),
            Vec2::new(5.0, 0.0)
        );
        assert!(matches!(
            index.update(static_aabb(99, Vec2::ZERO, Vec2::new(1.0, 1.0))),
            Err(StaticAabbBroadphaseError2d::MissingStaticColliderKey { key: 99 })
        ));
        index
            .insert(static_aabb(2, Vec2::new(5.0, 0.0), Vec2::new(1.0, 1.0)))
            .expect("within collider limit");
        assert!(matches!(
            index.candidate_keys_in_region(
                Vec2::new(5.0, 0.0),
                Aabb2d::new(Vec2::new(2.0, 2.0)).expect("valid query"),
            ),
            Err(StaticAabbBroadphaseError2d::CandidateLimitExceeded { maximum: 1 })
        ));
        assert!(matches!(
            index.rebuild([
                static_aabb(7, Vec2::ZERO, Vec2::new(1.0, 1.0)),
                static_aabb(7, Vec2::new(5.0, 0.0), Vec2::new(1.0, 1.0)),
            ]),
            Err(StaticAabbBroadphaseError2d::DuplicateStaticColliderKey { key: 7 })
        ));
        // Ошибочная перестройка не заменяет работоспособный предыдущий снимок.
        assert_eq!(index.len(), 2);
        assert!(index.get(1).is_some());
    }

    #[test]
    fn triangle_mesh_resolves_a_sphere_above_a_floor() {
        let vertices = [
            Vec3::new(-2.0, 0.0, -2.0),
            Vec3::new(2.0, 0.0, -2.0),
            Vec3::new(2.0, 0.0, 2.0),
            Vec3::new(-2.0, 0.0, 2.0),
        ];
        let mesh =
            TriangleMesh3d::from_indexed(&vertices, &[0, 2, 1, 0, 3, 2]).expect("valid floor");
        let result = mesh
            .resolve_sphere(Vec3::new(0.0, 0.1, 0.0), 0.5, 4)
            .expect("valid query");
        assert!(result.ground_contact);
        assert!(result.position.y >= 0.5);
        assert!(result.contacts > 0);
    }

    fn separated_floor_triangles(count: usize) -> (Vec<Vec3>, Vec<u32>) {
        let mut vertices = Vec::with_capacity(count * 3);
        let mut indices = Vec::with_capacity(count * 3);
        for triangle in 0..count {
            let x = triangle as f32 * 10.0;
            let first = u32::try_from(vertices.len()).expect("focused test fits u32 indices");
            vertices.extend([
                Vec3::new(x - 1.0, 0.0, -1.0),
                Vec3::new(x + 1.0, 0.0, -1.0),
                Vec3::new(x, 0.0, 1.0),
            ]);
            indices.extend([first, first + 1, first + 2]);
        }
        (vertices, indices)
    }

    fn linear_sphere_resolution_reference(
        mesh: &TriangleMesh3d,
        position: Vec3,
        radius: f32,
        iterations: usize,
    ) -> SphereMeshResolution3d {
        let min_walkable_normal_y = DEFAULT_MIN_WALKABLE_NORMAL_Y;
        let mut resolved = position;
        let mut ground_contact = false;
        let mut contacts = 0;
        for _ in 0..iterations {
            let mut changed = false;
            for (face, bounds) in mesh.triangles.iter().zip(&mesh.bounds) {
                if !bounds.overlaps_sphere(resolved, radius) {
                    continue;
                }
                let closest = closest_point_on_triangle(resolved, *face);
                let offset = resolved - closest;
                let distance_squared = offset.length_squared();
                if distance_squared >= radius * radius {
                    continue;
                }
                let normal = if distance_squared > f32::EPSILON {
                    offset * distance_squared.sqrt().recip()
                } else {
                    cross3(face[1] - face[0], face[2] - face[0]).normalized_or_zero()
                };
                let distance = distance_squared.sqrt();
                resolved = resolved + normal * (radius - distance + 0.0005);
                ground_contact |= normal.y >= min_walkable_normal_y;
                contacts += 1;
                changed = true;
            }
            if !changed {
                break;
            }
        }
        SphereMeshResolution3d {
            position: resolved,
            ground_contact,
            contacts,
        }
    }

    #[test]
    fn resolve_sphere_slope_floor_wall_and_ramp() {
        let floor = TriangleMesh3d::from_indexed(
            &[
                Vec3::new(-2.0, 0.0, -2.0),
                Vec3::new(2.0, 0.0, -2.0),
                Vec3::new(2.0, 0.0, 2.0),
                Vec3::new(-2.0, 0.0, 2.0),
            ],
            &[0, 2, 1, 0, 3, 2],
        )
        .expect("floor");
        let wall = TriangleMesh3d::from_indexed(
            &[
                Vec3::new(1.0, 0.0, -2.0),
                Vec3::new(1.0, 3.0, -2.0),
                Vec3::new(1.0, 3.0, 2.0),
                Vec3::new(1.0, 0.0, 2.0),
            ],
            &[0, 1, 2, 0, 2, 3],
        )
        .expect("wall");
        // 30° ramp: rise/run = tan(30°), normal.y = cos(30°) ≈ 0.866.
        let ramp_angle = 30.0_f32.to_radians();
        let run = 4.0_f32;
        let rise = run * ramp_angle.tan();
        let ramp = TriangleMesh3d::from_indexed(
            &[
                Vec3::new(-2.0, 0.0, 0.0),
                Vec3::new(2.0, 0.0, 0.0),
                Vec3::new(2.0, rise, run),
                Vec3::new(-2.0, rise, run),
            ],
            // Winding must face upward so contact normals count as walkable.
            &[0, 2, 1, 0, 3, 2],
        )
        .expect("ramp");

        let on_floor = floor
            .resolve_sphere_with_slope(Vec3::new(0.0, 0.1, 0.0), 0.5, 4, 45.0_f32.to_radians())
            .expect("floor query");
        assert!(on_floor.ground_contact, "flat floor must be walkable");

        let against_wall = wall
            .resolve_sphere_with_slope(Vec3::new(0.7, 1.0, 0.0), 0.5, 4, 60.0_f32.to_radians())
            .expect("wall query");
        assert!(against_wall.contacts > 0, "wall should push the sphere");
        assert!(
            !against_wall.ground_contact,
            "vertical wall must not count as ground"
        );

        let steep_limit = 20.0_f32.to_radians();
        let walkable_limit = 35.0_f32.to_radians();
        // Centre slightly above the ramp surface at z=1 so contact normals face up.
        let surface_y = 1.0 * ramp_angle.tan();
        let ramp_probe = Vec3::new(0.0, surface_y + 0.15, 1.0);
        let on_ramp_steep = ramp
            .resolve_sphere_with_slope(ramp_probe, 0.4, 6, steep_limit)
            .expect("ramp steep");
        let on_ramp_ok = ramp
            .resolve_sphere_with_slope(ramp_probe, 0.4, 6, walkable_limit)
            .expect("ramp ok");
        assert!(
            on_ramp_steep.contacts > 0 && !on_ramp_steep.ground_contact,
            "30° ramp must not be walkable at 20° max slope (contacts={}, grounded={})",
            on_ramp_steep.contacts,
            on_ramp_steep.ground_contact
        );
        assert!(
            on_ramp_ok.ground_contact,
            "30° ramp must be walkable at 35° max slope"
        );
        assert!(
            matches!(
                floor.resolve_sphere_with_slope(Vec3::ZERO, 0.5, 4, 0.0),
                Err(TriangleMeshQueryError::InvalidMaxWalkableSlope(_))
            ),
            "zero slope must be rejected"
        );
    }

    #[test]
    fn triangle_mesh_bvh_is_compact_balanced_and_prunes_distant_candidates() {
        const TRIANGLES: usize = 4_096;
        let (vertices, indices) = separated_floor_triangles(TRIANGLES);
        let mesh = TriangleMesh3d::from_indexed(&vertices, &indices).expect("valid sparse map");
        let acceleration = mesh.acceleration_stats();
        let expected_leaves = TRIANGLES.div_ceil(TRIANGLE_BVH_LEAF_CAPACITY);
        assert_eq!(acceleration.triangles, TRIANGLES);
        assert_eq!(acceleration.leaves, expected_leaves);
        assert_eq!(acceleration.nodes, expected_leaves * 2 - 1);
        assert_eq!(acceleration.leaf_capacity, TRIANGLE_BVH_LEAF_CAPACITY);
        assert_eq!(
            acceleration.bvh_bytes,
            acceleration.nodes * std::mem::size_of::<TriangleBvhNode3d>()
        );
        assert!(
            std::mem::size_of::<TriangleBvhNode3d>() <= 40,
            "compact flat node unexpectedly grew"
        );
        assert!(acceleration.maximum_depth <= 11);

        let ray = Ray3d::new(Vec3::new(0.0, 3.0, 0.0), Vec3::new(0.0, -1.0, 0.0))
            .expect("valid downward ray");
        let ray_result = mesh
            .raycast_with_stats(ray, 10.0)
            .expect("valid profiled raycast");
        assert_eq!(ray_result.hit.expect("first floor triangle").triangle, 0);
        assert!(
            ray_result.stats.triangle_bounds_tested < TRIANGLES / 100,
            "BVH should reject over 99% of sparse map triangles: {:?}",
            ray_result.stats
        );
        assert!(ray_result.stats.exact_triangles_tested <= TRIANGLE_BVH_LEAF_CAPACITY);

        let sphere_result = mesh
            .resolve_sphere_with_stats(Vec3::new(0.0, 0.1, 0.0), 0.5, 4)
            .expect("valid profiled sphere query");
        assert!(sphere_result.resolution.ground_contact);
        assert!(
            sphere_result.stats.triangle_bounds_tested < TRIANGLES / 100,
            "sphere BVH should reject over 99% of sparse map triangles: {:?}",
            sphere_result.stats
        );
        assert!(
            sphere_result.stats.exact_triangles_tested
                <= TRIANGLE_BVH_LEAF_CAPACITY * sphere_result.completed_iterations
        );
    }

    #[test]
    fn triangle_mesh_bvh_preserves_linear_source_order_resolution_exactly() {
        let vertices = [
            Vec3::new(-2.0, 0.0, -2.0),
            Vec3::new(2.0, 0.0, -2.0),
            Vec3::new(2.0, 0.0, 2.0),
            Vec3::new(-2.0, 0.0, 2.0),
            Vec3::new(0.3, -1.0, -2.0),
            Vec3::new(0.3, 2.0, -2.0),
            Vec3::new(0.3, 2.0, 2.0),
            Vec3::new(0.3, -1.0, 2.0),
        ];
        let mesh = TriangleMesh3d::from_indexed(&vertices, &[0, 2, 1, 0, 3, 2, 4, 5, 6, 4, 6, 7])
            .expect("valid floor and wall");
        let position = Vec3::new(0.1, 0.1, 0.0);
        let expected = linear_sphere_resolution_reference(&mesh, position, 0.5, 4);
        let accelerated = mesh
            .resolve_sphere(position, 0.5, 4)
            .expect("valid accelerated query");
        assert_eq!(accelerated, expected);
    }

    #[test]
    fn kinematic_aabb_stops_at_wall_and_floor_without_axis_tunnelling() {
        let mover = Aabb2d::new(Vec2::new(1.0, 1.0)).expect("valid mover");
        let limits = KinematicAabbMoveLimits2d::new(4).expect("positive budget");
        let wall = static_aabb(10, Vec2::new(5.0, 0.0), Vec2::new(1.0, 10.0));
        let wall_hit =
            resolve_kinematic_aabb_2d(Vec2::ZERO, mover, Vec2::new(100.0, 0.0), &[wall], limits)
                .expect("valid movement");
        assert_eq!(wall_hit.final_center, Vec2::new(2.5, 0.0));
        assert_eq!(wall_hit.applied_delta, Vec2::new(2.5, 0.0));
        assert_eq!(wall_hit.contacts()[0].normal, Vec2::new(-1.0, 0.0));

        let floor = static_aabb(11, Vec2::new(0.0, 5.0), Vec2::new(10.0, 1.0));
        let floor_hit =
            resolve_kinematic_aabb_2d(Vec2::ZERO, mover, Vec2::new(0.0, 100.0), &[floor], limits)
                .expect("valid movement");
        assert_eq!(floor_hit.final_center, Vec2::new(0.0, 2.5));
        assert_eq!(floor_hit.contacts()[0].normal, Vec2::new(0.0, -1.0));
    }

    #[test]
    fn kinematic_aabb_slides_on_wall_and_reports_corner_in_axis_order() {
        let mover = Aabb2d::new(Vec2::new(1.0, 1.0)).expect("valid mover");
        let limits = KinematicAabbMoveLimits2d::new(4).expect("positive budget");
        let wall = static_aabb(20, Vec2::new(5.0, 0.0), Vec2::new(1.0, 10.0));
        let slide =
            resolve_kinematic_aabb_2d(Vec2::ZERO, mover, Vec2::new(10.0, 4.0), &[wall], limits)
                .expect("valid movement");
        assert_eq!(slide.final_center, Vec2::new(2.5, 4.0));
        assert_eq!(slide.contacts().len(), 1);
        assert_eq!(slide.contacts()[0].collider_key, 20);

        let floor = static_aabb(30, Vec2::new(0.0, 5.0), Vec2::new(10.0, 1.0));
        let corner = resolve_kinematic_aabb_2d(
            Vec2::ZERO,
            mover,
            Vec2::new(10.0, 10.0),
            &[floor, wall],
            limits,
        )
        .expect("valid movement");
        assert_eq!(corner.final_center, Vec2::new(2.5, 2.5));
        assert_eq!(
            corner.contacts(),
            [
                KinematicAabbContact2d {
                    collider_key: 20,
                    normal: Vec2::new(-1.0, 0.0),
                },
                KinematicAabbContact2d {
                    collider_key: 30,
                    normal: Vec2::new(0.0, -1.0),
                },
            ]
        );
    }

    #[test]
    fn kinematic_aabb_ignores_walls_behind_sweep_in_bordered_room() {
        // Mover in open space with solid borders on all four sides (farm-map shape).
        // A skin that treats negative separation as a hit freezes every axis.
        let mover = Aabb2d::new(Vec2::new(5.0, 4.0)).expect("valid mover");
        let limits = KinematicAabbMoveLimits2d::new(8).expect("positive budget");
        let left = static_aabb(1, Vec2::new(-50.0, 0.0), Vec2::new(10.0, 80.0));
        let right = static_aabb(2, Vec2::new(50.0, 0.0), Vec2::new(10.0, 80.0));
        let bottom = static_aabb(3, Vec2::new(0.0, -50.0), Vec2::new(80.0, 10.0));
        let top = static_aabb(4, Vec2::new(0.0, 50.0), Vec2::new(80.0, 10.0));
        let walls = [left, right, bottom, top];

        let rightward = resolve_kinematic_aabb_2d(
            Vec2::ZERO,
            mover,
            Vec2::new(12.0, 0.0),
            &walls,
            limits,
        )
        .expect("open move right");
        assert_eq!(rightward.final_center, Vec2::new(12.0, 0.0));
        assert!(rightward.contacts().is_empty());

        let up = resolve_kinematic_aabb_2d(
            Vec2::ZERO,
            mover,
            Vec2::new(0.0, 9.0),
            &walls,
            limits,
        )
        .expect("open move up");
        assert_eq!(up.final_center, Vec2::new(0.0, 9.0));
        assert!(up.contacts().is_empty());
    }

    #[test]
    fn kinematic_aabb_ties_by_key_and_rejects_invalid_work() {
        let mover = Aabb2d::new(Vec2::new(1.0, 1.0)).expect("valid mover");
        let lower_key = static_aabb(2, Vec2::new(5.0, 0.0), Vec2::new(1.0, 10.0));
        let higher_key = static_aabb(9, Vec2::new(5.0, 0.0), Vec2::new(1.0, 10.0));
        let limits = KinematicAabbMoveLimits2d::new(2).expect("positive budget");
        let tied = resolve_kinematic_aabb_2d(
            Vec2::ZERO,
            mover,
            Vec2::new(10.0, 0.0),
            &[higher_key, lower_key],
            limits,
        )
        .expect("valid movement");
        assert_eq!(tied.contacts()[0].collider_key, 2);

        assert!(matches!(
            KinematicAabbMoveLimits2d::new(0),
            Err(PhysicsConfigError::InvalidKinematicColliderLimit(0))
        ));
        assert!(matches!(
            resolve_kinematic_aabb_2d(Vec2::ZERO, mover, Vec2::new(f32::NAN, 0.0), &[], limits,),
            Err(KinematicAabbMoveError::InvalidInput(
                PhysicsConfigError::NonFiniteVec2 { .. }
            ))
        ));
        assert!(matches!(
            resolve_kinematic_aabb_2d(
                Vec2::ZERO,
                mover,
                Vec2::ZERO,
                &[
                    higher_key,
                    lower_key,
                    static_aabb(17, Vec2::new(9.0, 0.0), Vec2::new(1.0, 1.0))
                ],
                limits,
            ),
            Err(KinematicAabbMoveError::StaticColliderLimitExceeded {
                maximum: 2,
                actual: 3,
            })
        ));
        assert!(matches!(
            resolve_kinematic_aabb_2d(
                Vec2::ZERO,
                mover,
                Vec2::ZERO,
                &[
                    lower_key,
                    static_aabb(2, Vec2::new(9.0, 0.0), Vec2::new(1.0, 1.0))
                ],
                limits,
            ),
            Err(KinematicAabbMoveError::DuplicateStaticColliderKey { key: 2 })
        ));
        assert!(matches!(
            resolve_kinematic_aabb_2d(Vec2::new(5.0, 0.0), mover, Vec2::ZERO, &[lower_key], limits,),
            Err(KinematicAabbMoveError::InitialOverlap { collider_key: 2 })
        ));
    }

    #[test]
    fn triangle_mesh_raycast_is_two_sided_and_keeps_source_order_for_ties() {
        let mesh = TriangleMesh3d::from_indexed(
            &[
                Vec3::new(-1.0, 0.0, -1.0),
                Vec3::new(1.0, 0.0, -1.0),
                Vec3::new(0.0, 0.0, 1.0),
                Vec3::new(-1.0, 0.0, -1.0),
                Vec3::new(1.0, 0.0, -1.0),
                Vec3::new(0.0, 0.0, 1.0),
            ],
            &[0, 1, 2, 3, 4, 5],
        )
        .expect("valid duplicated floor");
        let ray = Ray3d::new(Vec3::new(0.0, 3.0, 0.0), Vec3::new(0.0, -1.0, 0.0))
            .expect("valid down ray");
        let hit = mesh
            .raycast(ray, 5.0)
            .expect("valid query")
            .expect("floor hit");
        assert_eq!(hit.triangle, 0);
        assert_eq!(hit.distance, 3.0);
        assert_eq!(hit.position, Vec3::new(0.0, 0.0, 0.0));
        assert_eq!(hit.normal, Vec3::new(0.0, -1.0, 0.0));

        let from_below =
            Ray3d::new(Vec3::new(0.0, -3.0, 0.0), Vec3::new(0.0, 1.0, 0.0)).expect("valid up ray");
        assert!(
            mesh.raycast(from_below, 5.0)
                .expect("valid query")
                .is_some()
        );
    }
}
