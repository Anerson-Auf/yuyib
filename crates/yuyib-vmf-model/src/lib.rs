//! Renderer-neutral compilation of convex Source 1 VMF brushes into models.
//!
//! Owns only normalized geometry input. A VMF parser
//! can convert each `solid`/`side` into [`BrushSolid`] and [`BrushSide`]
//! without forcing this compiler to duplicate parsing or `KeyValues` semantics.
//! [`compile_solid`] and [`compile_brushes`] intersect side planes, construct
//! convex face windings and emit indexed [`yuyib_model::Model`] meshes.
//!
//! # Source coordinate convention
//!
//! Source 1 brush points use `X`/`Y` in the map plane with `Z` up. Yuyib's 3D
//! convention is right-handed with `Y` up and camera forward along `-Z`. Every
//! source point is converted as **`[x, y, z] -> [x, z, -y]`** before plane
//! intersection. This map has determinant `+1`, so it preserves winding and
//! outward normals. Material strings are copied verbatim into
//! [`yuyib_model::Material::name`]; they are Source material/VMT identifiers,
//! not resolved texture URIs.
//!
//! # Deliberate scope
//!
//! Only finite convex brush solids made from planar sides are supported. The
//! compiler emits geometric normals but no VMF texture-axis UVs yet. It does
//! not read VMT/VTF, lightmaps, displacements, entities, props, areaportals,
//! BSP data, Hammer editor state, Source 2 VMAP/VPK content, or perform a
//! runtime visibility/physics conversion.

#![forbid(unsafe_code)]

use std::{collections::HashMap, error::Error, fmt};

use yuyib_model::{
    Material, MaterialIndex, Mesh, MeshError, MeshPrimitive, MeshValidationError, Model,
    ModelValidationError,
};

const PLANE_EPSILON: f32 = 0.000_5;
const DEDUPLICATION_EPSILON_SQUARED: f32 = 0.000_001;

/// Three Source-coordinate points defining one VMF side plane.
///
/// The Source 1 parser adapter should copy the three values from a VMF
/// `plane` string exactly. Values are converted to Yuyib coordinates only when
/// compilation starts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlanePoints {
    /// First source-coordinate point `[x, y, z]`.
    pub first: [f32; 3],
    /// Second source-coordinate point `[x, y, z]`.
    pub second: [f32; 3],
    /// Third source-coordinate point `[x, y, z]`.
    pub third: [f32; 3],
}

impl PlanePoints {
    /// Creates one source-coordinate plane definition.
    #[must_use]
    pub const fn new(first: [f32; 3], second: [f32; 3], third: [f32; 3]) -> Self {
        Self {
            first,
            second,
            third,
        }
    }
}

/// One material-bearing planar side of a convex VMF brush.
#[derive(Clone, Debug, PartialEq)]
pub struct BrushSide {
    /// Source plane points for this side.
    pub plane: PlanePoints,
    /// Source 1 material/VMT identifier, preserved exactly in model metadata.
    pub material: String,
}

impl BrushSide {
    /// Creates a side with its source material identifier.
    #[must_use]
    pub fn new(plane: PlanePoints, material: impl Into<String>) -> Self {
        Self {
            plane,
            material: material.into(),
        }
    }
}

/// One convex VMF `solid` normalized for compilation.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BrushSolid {
    /// Optional source/debug label used as the resulting mesh name.
    pub name: Option<String>,
    /// Plane sides in source order.
    pub sides: Vec<BrushSide>,
}

impl BrushSolid {
    /// Creates a solid from source-order sides.
    #[must_use]
    pub fn new(name: Option<String>, sides: Vec<BrushSide>) -> Self {
        Self { name, sides }
    }
}

/// Explicit bounded-work limits for VMF brush compilation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BrushCompileLimits {
    /// Maximum brush solids accepted by [`compile_brushes`].
    pub max_solids: usize,
    /// Maximum plane sides in one solid.
    pub max_sides_per_solid: usize,
    /// Maximum unique intersection vertices retained for one solid.
    pub max_vertices_per_solid: usize,
    /// Maximum vertices retained by one output face primitive.
    pub max_vertices_per_face: usize,
    /// Maximum generated triangle indices across the complete output model.
    pub max_indices: usize,
}

impl Default for BrushCompileLimits {
    fn default() -> Self {
        Self {
            max_solids: 4_096,
            max_sides_per_solid: 128,
            max_vertices_per_solid: 8_192,
            max_vertices_per_face: 1_024,
            max_indices: 16_000_000,
        }
    }
}

/// Failure while validating or compiling Source 1 brush geometry.
#[derive(Debug)]
pub enum BrushCompileError {
    /// The supplied solid list exceeded the configured bounded-work limit.
    TooManySolids {
        /// Observed solid count.
        actual: usize,
        /// Configured maximum.
        limit: usize,
    },
    /// A solid contained fewer than four planes and cannot bound a volume.
    TooFewSides {
        /// Solid index in the supplied slice.
        solid: usize,
        /// Observed side count.
        actual: usize,
    },
    /// A solid exceeded the configured side limit.
    TooManySides {
        /// Solid index in the supplied slice.
        solid: usize,
        /// Observed side count.
        actual: usize,
        /// Configured maximum.
        limit: usize,
    },
    /// A source plane point contained NaN or infinity.
    NonFinitePlanePoint {
        /// Solid index in the supplied slice.
        solid: usize,
        /// Side index in source order.
        side: usize,
    },
    /// Three plane points were collinear or numerically degenerate.
    DegeneratePlane {
        /// Solid index in the supplied slice.
        solid: usize,
        /// Side index in source order.
        side: usize,
    },
    /// The plane set has no finite convex interior for either global winding.
    NoConvexVolume {
        /// Solid index in the supplied slice.
        solid: usize,
    },
    /// An output face had fewer than three unique vertices.
    DegenerateFace {
        /// Solid index in the supplied slice.
        solid: usize,
        /// Side index in source order.
        side: usize,
    },
    /// Generated solid vertices exceeded the configured bound.
    TooManySolidVertices {
        /// Solid index in the supplied slice.
        solid: usize,
        /// Configured maximum.
        limit: usize,
    },
    /// Generated face vertices exceeded the configured bound.
    TooManyFaceVertices {
        /// Solid index in the supplied slice.
        solid: usize,
        /// Side index in source order.
        side: usize,
        /// Configured maximum.
        limit: usize,
    },
    /// Generated indices exceeded the configured global bound.
    TooManyIndices {
        /// Configured maximum.
        limit: usize,
    },
    /// A generated primitive violated the renderer-neutral mesh contract.
    MeshValidation {
        /// Solid index in the supplied slice.
        solid: usize,
        /// Side index in source order.
        side: usize,
        /// Underlying model validation failure.
        source: MeshValidationError,
    },
    /// A generated solid had no renderable primitives.
    Mesh {
        /// Solid index in the supplied slice.
        solid: usize,
        /// Underlying model mesh failure.
        source: MeshError,
    },
    /// Complete generated model did not meet cross-resource invariants.
    Model(ModelValidationError),
}

impl fmt::Display for BrushCompileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManySolids { actual, limit } => {
                write!(formatter, "VMF brush count {actual} exceeds limit {limit}")
            }
            Self::TooFewSides { solid, actual } => write!(
                formatter,
                "VMF solid {solid} has {actual} sides; at least four are required"
            ),
            Self::TooManySides {
                solid,
                actual,
                limit,
            } => write!(
                formatter,
                "VMF solid {solid} has {actual} sides; limit is {limit}"
            ),
            Self::NonFinitePlanePoint { solid, side } => {
                write!(
                    formatter,
                    "VMF solid {solid}, side {side} has non-finite plane point"
                )
            }
            Self::DegeneratePlane { solid, side } => {
                write!(
                    formatter,
                    "VMF solid {solid}, side {side} plane is degenerate"
                )
            }
            Self::NoConvexVolume { solid } => {
                write!(
                    formatter,
                    "VMF solid {solid} does not form a finite convex volume"
                )
            }
            Self::DegenerateFace { solid, side } => {
                write!(
                    formatter,
                    "VMF solid {solid}, side {side} does not form a polygon"
                )
            }
            Self::TooManySolidVertices { solid, limit } => write!(
                formatter,
                "VMF solid {solid} exceeded generated vertex limit {limit}"
            ),
            Self::TooManyFaceVertices { solid, side, limit } => write!(
                formatter,
                "VMF solid {solid}, side {side} exceeded face vertex limit {limit}"
            ),
            Self::TooManyIndices { limit } => {
                write!(formatter, "VMF compilation exceeded index limit {limit}")
            }
            Self::MeshValidation {
                solid,
                side,
                source,
            } => write!(
                formatter,
                "VMF solid {solid}, side {side} mesh is invalid: {source}"
            ),
            Self::Mesh { solid, source } => {
                write!(formatter, "VMF solid {solid} mesh is invalid: {source}")
            }
            Self::Model(source) => write!(formatter, "compiled VMF model is invalid: {source}"),
        }
    }
}

impl Error for BrushCompileError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::MeshValidation { source, .. } => Some(source),
            Self::Mesh { source, .. } => Some(source),
            Self::Model(source) => Some(source),
            Self::TooManySolids { .. }
            | Self::TooFewSides { .. }
            | Self::TooManySides { .. }
            | Self::NonFinitePlanePoint { .. }
            | Self::DegeneratePlane { .. }
            | Self::NoConvexVolume { .. }
            | Self::DegenerateFace { .. }
            | Self::TooManySolidVertices { .. }
            | Self::TooManyFaceVertices { .. }
            | Self::TooManyIndices { .. } => None,
        }
    }
}

/// Compiles one convex Source 1 brush into a [`Model`].
///
/// This is equivalent to calling [`compile_brushes`] with one element. Output
/// uses one mesh and one triangle-list primitive per source side, retaining the
/// side's material assignment and flat geometric normal.
///
/// # Errors
///
/// Returns [`BrushCompileError`] for invalid, non-convex, degenerate or
/// limit-exceeding brush input.
pub fn compile_solid(
    solid: &BrushSolid,
    limits: BrushCompileLimits,
) -> Result<Model, BrushCompileError> {
    compile_brushes(std::slice::from_ref(solid), limits)
}

/// Compiles convex Source 1 brush solids into one renderer-neutral model.
///
/// Materials are deduplicated by exact source string in first-seen source
/// order. Each `BrushSide` becomes a primitive; UVs are deliberately absent
/// because VMF texture-axis interpretation and VMT/VTF resolution are not part
/// of this geometry compiler.
///
/// Plane point winding is expected to use normal Source 1 side convention. To
/// tolerate parser-side global winding reversal, the compiler deterministically
/// tries the opposite orientation only when the expected orientation yields no
/// valid convex vertices. Mixed per-side winding is rejected.
///
/// # Errors
///
/// Returns [`BrushCompileError`] for validation, topology, bounded-work or
/// model-contract failure.
pub fn compile_brushes(
    solids: &[BrushSolid],
    limits: BrushCompileLimits,
) -> Result<Model, BrushCompileError> {
    if solids.len() > limits.max_solids {
        return Err(BrushCompileError::TooManySolids {
            actual: solids.len(),
            limit: limits.max_solids,
        });
    }
    let mut materials = Vec::new();
    let mut material_indices = HashMap::new();
    let mut meshes = Vec::new();
    let mut total_indices = 0_usize;

    for (solid_index, solid) in solids.iter().enumerate() {
        let primitives = compile_one_solid(
            solid,
            solid_index,
            limits,
            &mut materials,
            &mut material_indices,
            &mut total_indices,
        )?;
        let name = solid
            .name
            .clone()
            .or_else(|| Some(format!("vmf_solid_{solid_index}")));
        meshes.push(
            Mesh::new(name, primitives).map_err(|source| BrushCompileError::Mesh {
                solid: solid_index,
                source,
            })?,
        );
    }
    Model::new(meshes, materials, Vec::new()).map_err(BrushCompileError::Model)
}

#[derive(Clone, Copy)]
struct Plane {
    normal: [f32; 3],
    distance: f32,
}

fn compile_one_solid(
    solid: &BrushSolid,
    solid_index: usize,
    limits: BrushCompileLimits,
    materials: &mut Vec<Material>,
    material_indices: &mut HashMap<String, MaterialIndex>,
    total_indices: &mut usize,
) -> Result<Vec<MeshPrimitive>, BrushCompileError> {
    if solid.sides.len() < 4 {
        return Err(BrushCompileError::TooFewSides {
            solid: solid_index,
            actual: solid.sides.len(),
        });
    }
    if solid.sides.len() > limits.max_sides_per_solid {
        return Err(BrushCompileError::TooManySides {
            solid: solid_index,
            actual: solid.sides.len(),
            limit: limits.max_sides_per_solid,
        });
    }
    let planes: Vec<Plane> = solid
        .sides
        .iter()
        .enumerate()
        .map(|(side, source)| plane_from_points(source.plane, solid_index, side))
        .collect::<Result<_, _>>()?;

    let vertices = collect_convex_vertices(&planes, false, solid_index, limits).or_else(
        |error| match error {
            BrushCompileError::NoConvexVolume { .. } => {
                collect_convex_vertices(&planes, true, solid_index, limits)
            }
            _ => Err(error),
        },
    )?;
    let inward_is_positive = orientation_is_positive(&planes, &vertices);
    let mut primitives = Vec::new();
    for (side_index, plane) in planes.iter().copied().enumerate() {
        let mut face = vertices
            .iter()
            .copied()
            .filter(|vertex| (dot3(plane.normal, *vertex) - plane.distance).abs() <= PLANE_EPSILON)
            .collect::<Vec<_>>();
        if face.len() < 3 {
            return Err(BrushCompileError::DegenerateFace {
                solid: solid_index,
                side: side_index,
            });
        }
        if face.len() > limits.max_vertices_per_face {
            return Err(BrushCompileError::TooManyFaceVertices {
                solid: solid_index,
                side: side_index,
                limit: limits.max_vertices_per_face,
            });
        }
        let outward = if inward_is_positive {
            scale3(plane.normal, -1.0)
        } else {
            plane.normal
        };
        order_face_vertices(&mut face, outward);
        let triangle_count = face.len() - 2;
        let indices = triangulate_fan(face.len(), outward, &face).ok_or(
            BrushCompileError::TooManyFaceVertices {
                solid: solid_index,
                side: side_index,
                limit: limits.max_vertices_per_face,
            },
        )?;
        *total_indices = total_indices.checked_add(triangle_count * 3).ok_or(
            BrushCompileError::TooManyIndices {
                limit: limits.max_indices,
            },
        )?;
        if *total_indices > limits.max_indices {
            return Err(BrushCompileError::TooManyIndices {
                limit: limits.max_indices,
            });
        }
        let material = material_index(
            &solid.sides[side_index].material,
            materials,
            material_indices,
        );
        let normals = vec![outward; face.len()];
        let primitive = MeshPrimitive::new(face, indices)
            .map_err(|source| BrushCompileError::MeshValidation {
                solid: solid_index,
                side: side_index,
                source,
            })?
            .with_normals(normals)
            .map_err(|source| BrushCompileError::MeshValidation {
                solid: solid_index,
                side: side_index,
                source,
            })?
            .with_material(material);
        primitives.push(primitive);
    }
    Ok(primitives)
}

fn material_index(
    material_path: &str,
    materials: &mut Vec<Material>,
    indices: &mut HashMap<String, MaterialIndex>,
) -> MaterialIndex {
    if let Some(index) = indices.get(material_path) {
        return *index;
    }
    let index = MaterialIndex::new(materials.len());
    materials.push(Material::new().with_name(material_path));
    indices.insert(material_path.to_owned(), index);
    index
}

fn plane_from_points(
    points: PlanePoints,
    solid: usize,
    side: usize,
) -> Result<Plane, BrushCompileError> {
    if !all_finite(&points.first) || !all_finite(&points.second) || !all_finite(&points.third) {
        return Err(BrushCompileError::NonFinitePlanePoint { solid, side });
    }
    let first = source_to_yuyib(points.first);
    let second = source_to_yuyib(points.second);
    let third = source_to_yuyib(points.third);
    let normal = normalize3(cross3(sub3(second, first), sub3(third, first)))
        .ok_or(BrushCompileError::DegeneratePlane { solid, side })?;
    Ok(Plane {
        normal,
        distance: dot3(normal, first),
    })
}

fn collect_convex_vertices(
    planes: &[Plane],
    reverse_half_space: bool,
    solid: usize,
    limits: BrushCompileLimits,
) -> Result<Vec<[f32; 3]>, BrushCompileError> {
    let mut vertices = Vec::new();
    for first in 0..planes.len() {
        for second in first + 1..planes.len() {
            for third in second + 1..planes.len() {
                let Some(vertex) =
                    intersect_three_planes(planes[first], planes[second], planes[third])
                else {
                    continue;
                };
                let inside = planes.iter().all(|plane| {
                    let signed_distance = dot3(plane.normal, vertex) - plane.distance;
                    if reverse_half_space {
                        signed_distance >= -PLANE_EPSILON
                    } else {
                        signed_distance <= PLANE_EPSILON
                    }
                });
                if inside
                    && !vertices.iter().any(|known| {
                        squared_distance(*known, vertex) <= DEDUPLICATION_EPSILON_SQUARED
                    })
                {
                    vertices.push(vertex);
                    if vertices.len() > limits.max_vertices_per_solid {
                        return Err(BrushCompileError::TooManySolidVertices {
                            solid,
                            limit: limits.max_vertices_per_solid,
                        });
                    }
                }
            }
        }
    }
    if vertices.len() < 4 {
        Err(BrushCompileError::NoConvexVolume { solid })
    } else {
        Ok(vertices)
    }
}

#[allow(clippy::cast_precision_loss)] // Vertex count is bounded by the compiler's configured limit.
fn orientation_is_positive(planes: &[Plane], vertices: &[[f32; 3]]) -> bool {
    let centroid = scale3(
        vertices.iter().copied().fold([0.0; 3], add3),
        (vertices.len() as f32).recip(),
    );
    planes
        .iter()
        .map(|plane| dot3(plane.normal, centroid) - plane.distance)
        .sum::<f32>()
        > 0.0
}

fn intersect_three_planes(first: Plane, second: Plane, third: Plane) -> Option<[f32; 3]> {
    let second_cross_third = cross3(second.normal, third.normal);
    let denominator = dot3(first.normal, second_cross_third);
    if !denominator.is_finite() || denominator.abs() <= PLANE_EPSILON {
        return None;
    }
    let numerator = add3(
        add3(
            scale3(second_cross_third, first.distance),
            scale3(cross3(third.normal, first.normal), second.distance),
        ),
        scale3(cross3(first.normal, second.normal), third.distance),
    );
    let vertex = scale3(numerator, denominator.recip());
    all_finite(&vertex).then_some(vertex)
}

#[allow(clippy::cast_precision_loss)] // Face count is bounded by the compiler's configured limit.
fn order_face_vertices(vertices: &mut [[f32; 3]], outward_normal: [f32; 3]) {
    let centre = scale3(
        vertices.iter().copied().fold([0.0; 3], add3),
        (vertices.len() as f32).recip(),
    );
    let reference = if outward_normal[0].abs() < 0.8 {
        [1.0, 0.0, 0.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    let u =
        normalize3(cross3(reference, outward_normal)).expect("axis chosen non-parallel to normal");
    let v = cross3(outward_normal, u);
    vertices.sort_by(|left, right| {
        let left_offset = sub3(*left, centre);
        let right_offset = sub3(*right, centre);
        dot3(left_offset, v)
            .atan2(dot3(left_offset, u))
            .total_cmp(&dot3(right_offset, v).atan2(dot3(right_offset, u)))
    });
}

fn triangulate_fan(
    vertex_count: usize,
    outward: [f32; 3],
    vertices: &[[f32; 3]],
) -> Option<Vec<u32>> {
    let mut indices = Vec::with_capacity((vertex_count - 2) * 3);
    for index in 1..vertex_count - 1 {
        indices.extend([
            0,
            u32::try_from(index).ok()?,
            u32::try_from(index + 1).ok()?,
        ]);
    }
    if vertex_count >= 3 {
        let first = vertices[0];
        let second = vertices[1];
        let third = vertices[2];
        if dot3(cross3(sub3(second, first), sub3(third, first)), outward) < 0.0 {
            for triangle in indices.as_chunks_mut::<3>().0 {
                triangle.swap(1, 2);
            }
        }
    }
    Some(indices)
}

fn source_to_yuyib(source: [f32; 3]) -> [f32; 3] {
    [source[0], source[2], -source[1]]
}

fn all_finite<const N: usize>(values: &[f32; N]) -> bool {
    values.iter().all(|value| value.is_finite())
}

fn squared_distance(left: [f32; 3], right: [f32; 3]) -> f32 {
    let delta = sub3(left, right);
    dot3(delta, delta)
}

fn normalize3(value: [f32; 3]) -> Option<[f32; 3]> {
    let length_squared = dot3(value, value);
    if !length_squared.is_finite() || length_squared <= f32::EPSILON {
        return None;
    }
    let inverse_length = length_squared.sqrt().recip();
    let normalized = scale3(value, inverse_length);
    all_finite(&normalized).then_some(normalized)
}

fn dot3(left: [f32; 3], right: [f32; 3]) -> f32 {
    left[0].mul_add(right[0], left[1].mul_add(right[1], left[2] * right[2]))
}

fn cross3(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [
        left[1] * right[2] - left[2] * right[1],
        left[2] * right[0] - left[0] * right[2],
        left[0] * right[1] - left[1] * right[0],
    ]
}

fn add3(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] + right[0], left[1] + right[1], left[2] + right[2]]
}

fn sub3(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
}

fn scale3(value: [f32; 3], factor: f32) -> [f32; 3] {
    [value[0] * factor, value[1] * factor, value[2] * factor]
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // Axis-aligned test brushes use exact coordinates.
mod tests {
    use super::*;

    fn cube() -> BrushSolid {
        let point = |x, y, z| [x, y, z];
        BrushSolid::new(
            Some("test_cube".to_owned()),
            vec![
                BrushSide::new(
                    PlanePoints::new(
                        point(-1.0, -1.0, -1.0),
                        point(-1.0, -1.0, 1.0),
                        point(-1.0, 1.0, 1.0),
                    ),
                    "brick/wall",
                ),
                BrushSide::new(
                    PlanePoints::new(
                        point(1.0, -1.0, -1.0),
                        point(1.0, 1.0, 1.0),
                        point(1.0, -1.0, 1.0),
                    ),
                    "brick/wall",
                ),
                BrushSide::new(
                    PlanePoints::new(
                        point(-1.0, -1.0, -1.0),
                        point(1.0, -1.0, -1.0),
                        point(1.0, -1.0, 1.0),
                    ),
                    "brick/floor",
                ),
                BrushSide::new(
                    PlanePoints::new(
                        point(-1.0, 1.0, -1.0),
                        point(1.0, 1.0, 1.0),
                        point(1.0, 1.0, -1.0),
                    ),
                    "brick/ceiling",
                ),
                BrushSide::new(
                    PlanePoints::new(
                        point(-1.0, -1.0, -1.0),
                        point(-1.0, 1.0, -1.0),
                        point(1.0, 1.0, -1.0),
                    ),
                    "brick/wall",
                ),
                BrushSide::new(
                    PlanePoints::new(
                        point(-1.0, -1.0, 1.0),
                        point(1.0, 1.0, 1.0),
                        point(-1.0, 1.0, 1.0),
                    ),
                    "brick/wall",
                ),
            ],
        )
    }

    #[test]
    fn cube_brush_emits_valid_materialized_meshes() {
        let model = compile_solid(&cube(), BrushCompileLimits::default()).expect("cube compiles");
        model.validate().expect("model contract");
        assert_eq!(model.meshes().len(), 1);
        assert_eq!(model.meshes()[0].primitives().len(), 6);
        assert_eq!(model.materials().len(), 3);
        for primitive in model.meshes()[0].primitives() {
            assert_eq!(primitive.positions().len(), 4);
            assert_eq!(primitive.indices().len(), 6);
            assert!(primitive.normals().is_some());
            assert!(primitive.tex_coords_0().is_none());
        }
    }

    #[test]
    fn compiler_rejects_degenerate_side_plane() {
        let solid = BrushSolid::new(
            None,
            vec![
                BrushSide::new(PlanePoints::new([0.0; 3], [1.0; 3], [2.0; 3]), "x"),
                BrushSide::new(
                    PlanePoints::new([0.0; 3], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]),
                    "x",
                ),
                BrushSide::new(
                    PlanePoints::new([0.0; 3], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]),
                    "x",
                ),
                BrushSide::new(
                    PlanePoints::new([1.0; 3], [1.0, 0.0, 1.0], [1.0, 1.0, 0.0]),
                    "x",
                ),
            ],
        );
        assert!(matches!(
            compile_solid(&solid, BrushCompileLimits::default()),
            Err(BrushCompileError::DegeneratePlane { side: 0, .. })
        ));
    }

    #[test]
    fn compilation_is_deterministic() {
        let first = compile_solid(&cube(), BrushCompileLimits::default()).expect("cube compiles");
        let second = compile_solid(&cube(), BrushCompileLimits::default()).expect("cube compiles");
        assert_eq!(first, second);
    }
}
