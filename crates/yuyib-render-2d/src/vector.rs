//! Retained GPU vector geometry for dense procedural 2D worlds.
//!
//! A [`VectorMesh2d`] is immutable, already-tessellated triangle geometry.
//! Upload it once as [`GpuVectorMesh2d`], then submit lightweight
//! [`VectorDraw2d`] transforms every frame. This deliberately avoids a
//! Canvas-style command stream, per-frame path tessellation, and CPU-side
//! rasterisation. It is intended for authored SVG/path importers, procedural
//! character parts, static world chunks, decals, and other geometry whose
//! topology changes much less often than its transform.

use std::{
    collections::{BTreeMap, HashMap},
    error::Error,
    fmt,
    mem::size_of,
};

use bytemuck::{Pod, Zeroable};
use lyon_path::{Path, math::point};
use lyon_tessellation::{BuffersBuilder, FillOptions, FillTessellator, FillVertex, VertexBuffers};
use wgpu::util::DeviceExt;
use yuyib_render::{RenderFrame, RenderViewport, wgpu};

use crate::Camera2d;

/// One premultiplied-or-straight vertex colour supplied by the application.
///
/// The renderer performs ordinary straight-alpha blending. Colours are not
/// gamma-converted: callers should use the same colour convention as their
/// existing 2D material pipeline.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct VectorVertex2d {
    /// Mesh-local world coordinate.
    pub position: [f32; 2],
    /// Per-vertex RGBA colour; gradients are represented by interpolation.
    pub color: [f32; 4],
}

impl VectorVertex2d {
    /// Creates one coloured mesh vertex.
    #[must_use]
    pub const fn new(position: [f32; 2], color: [f32; 4]) -> Self {
        Self { position, color }
    }
}

/// Validated, immutable triangle geometry in mesh-local coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct VectorMesh2d {
    vertices: Vec<VectorVertex2d>,
    indices: Vec<u32>,
    bounds: VectorAabb2d,
}

/// Axis-aligned bounds in vector mesh-local world coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VectorAabb2d {
    /// Inclusive minimum X/Y corner.
    pub min: [f32; 2],
    /// Inclusive maximum X/Y corner.
    pub max: [f32; 2],
}

impl VectorAabb2d {
    fn from_vertices(vertices: &[VectorVertex2d]) -> Self {
        let mut min = vertices[0].position;
        let mut max = min;
        for vertex in &vertices[1..] {
            min[0] = min[0].min(vertex.position[0]);
            min[1] = min[1].min(vertex.position[1]);
            max[0] = max[0].max(vertex.position[0]);
            max[1] = max[1].max(vertex.position[1]);
        }
        Self { min, max }
    }
}

impl VectorMesh2d {
    /// Validates and creates one indexed triangle mesh.
    ///
    /// Tessellate curves at asset-import/content-change time, then keep this
    /// object unchanged through normal render frames.
    ///
    /// # Errors
    ///
    /// Returns [`VectorMeshError2d`] when geometry is empty, non-finite, not a
    /// triangle list, or contains an index outside the vertex array.
    pub fn new(
        vertices: impl Into<Vec<VectorVertex2d>>,
        indices: impl Into<Vec<u32>>,
    ) -> Result<Self, VectorMeshError2d> {
        let vertices = vertices.into();
        let indices = indices.into();
        if vertices.is_empty() {
            return Err(VectorMeshError2d::EmptyVertices);
        }
        if indices.is_empty() || indices.len() % 3 != 0 {
            return Err(VectorMeshError2d::NonTriangleIndexCount {
                actual: indices.len(),
            });
        }
        for (index, vertex) in vertices.iter().enumerate() {
            if !vertex
                .position
                .iter()
                .chain(vertex.color.iter())
                .all(|value| value.is_finite())
            {
                return Err(VectorMeshError2d::NonFiniteVertex { index });
            }
        }
        for (index, &vertex) in indices.iter().enumerate() {
            if usize::try_from(vertex).map_or(true, |vertex| vertex >= vertices.len()) {
                return Err(VectorMeshError2d::IndexOutOfBounds {
                    index,
                    vertex,
                    vertices: vertices.len(),
                });
            }
        }
        let bounds = VectorAabb2d::from_vertices(&vertices);
        Ok(Self {
            vertices,
            indices,
            bounds,
        })
    }

    /// Returns the immutable vertex data for asset tools and diagnostics.
    #[must_use]
    pub fn vertices(&self) -> &[VectorVertex2d] {
        &self.vertices
    }

    /// Returns the immutable triangle index data.
    #[must_use]
    pub fn indices(&self) -> &[u32] {
        &self.indices
    }

    /// Returns mesh-local bounds used for conservative camera culling.
    #[must_use]
    pub const fn bounds(&self) -> VectorAabb2d {
        self.bounds
    }

    /// Returns the number of indexed triangles.
    #[must_use]
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }
}

/// Invalid immutable vector geometry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VectorMeshError2d {
    /// A mesh must contain at least one vertex.
    EmptyVertices,
    /// Indexed triangle lists contain exactly three indices per triangle.
    NonTriangleIndexCount {
        /// Observed number of indices.
        actual: usize,
    },
    /// Positions and colours must never contain NaN or infinity.
    NonFiniteVertex {
        /// Zero-based position in the vertex array.
        index: usize,
    },
    /// An index did not select an existing vertex.
    IndexOutOfBounds {
        /// Zero-based position in the index array.
        index: usize,
        /// Invalid vertex index supplied by the mesh.
        vertex: u32,
        /// Number of vertices that were available for selection.
        vertices: usize,
    },
}

impl fmt::Display for VectorMeshError2d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyVertices => formatter.write_str("vector mesh has no vertices"),
            Self::NonTriangleIndexCount { actual } => {
                write!(
                    formatter,
                    "vector mesh has {actual} indices; a triangle list needs a multiple of three"
                )
            }
            Self::NonFiniteVertex { index } => {
                write!(formatter, "vector vertex {index} is non-finite")
            }
            Self::IndexOutOfBounds {
                index,
                vertex,
                vertices,
            } => write!(
                formatter,
                "vector index {index} selects vertex {vertex}, but the mesh has {vertices} vertices"
            ),
        }
    }
}

impl Error for VectorMeshError2d {}

/// A validated stop in a CPU-evaluated linear gradient.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VectorGradientStop2d {
    /// Normalised offset in the inclusive `0..=1` range.
    pub offset: f32,
    /// RGBA colour at this offset.
    pub color: [f32; 4],
}

impl VectorGradientStop2d {
    /// Creates one finite gradient stop.
    ///
    /// # Errors
    ///
    /// Returns [`VectorPathError2d::InvalidGradientStop`] for a non-finite or
    /// out-of-range offset, or a non-finite colour channel.
    pub fn new(offset: f32, color: [f32; 4]) -> Result<Self, VectorPathError2d> {
        if !offset.is_finite()
            || !(0.0..=1.0).contains(&offset)
            || !color.iter().all(|v| v.is_finite())
        {
            return Err(VectorPathError2d::InvalidGradientStop);
        }
        Ok(Self { offset, color })
    }
}

/// Fill paint baked into mesh vertex colours during tessellation.
#[derive(Clone, Debug, PartialEq)]
pub enum VectorFill2d {
    /// A single RGBA colour for every generated vertex.
    Solid([f32; 4]),
    /// A linear gradient evaluated in mesh-local coordinates.
    LinearGradient {
        /// Gradient start point.
        from: [f32; 2],
        /// Gradient end point; it must differ from `from`.
        to: [f32; 2],
        /// Sorted stops used for interpolation.
        stops: Vec<VectorGradientStop2d>,
    },
    /// A radial gradient evaluated from `center` to `radius` in mesh-local space.
    RadialGradient {
        /// Centre of the gradient.
        center: [f32; 2],
        /// Radius at which the last stop is reached.
        radius: f32,
        /// Sorted stops used for interpolation.
        stops: Vec<VectorGradientStop2d>,
    },
}

impl VectorFill2d {
    /// Creates a finite solid fill.
    ///
    /// # Errors
    ///
    /// Returns [`VectorPathError2d::NonFiniteColor`] when a channel is NaN or infinite.
    pub fn solid(color: [f32; 4]) -> Result<Self, VectorPathError2d> {
        if !color.iter().all(|value| value.is_finite()) {
            return Err(VectorPathError2d::NonFiniteColor);
        }
        Ok(Self::Solid(color))
    }

    /// Creates a sorted, finite linear gradient.
    ///
    /// # Errors
    ///
    /// Returns [`VectorPathError2d`] when endpoints are invalid/equal or fewer
    /// than two valid stops are supplied.
    pub fn linear_gradient(
        from: [f32; 2],
        to: [f32; 2],
        stops: impl Into<Vec<VectorGradientStop2d>>,
    ) -> Result<Self, VectorPathError2d> {
        if !from.iter().chain(to.iter()).all(|value| value.is_finite()) {
            return Err(VectorPathError2d::NonFinitePoint);
        }
        if from == to {
            return Err(VectorPathError2d::DegenerateGradient);
        }
        let mut stops = stops.into();
        if stops.len() < 2 {
            return Err(VectorPathError2d::TooFewGradientStops);
        }
        stops.sort_by(|left, right| left.offset.total_cmp(&right.offset));
        Ok(Self::LinearGradient { from, to, stops })
    }

    /// Creates a sorted, finite radial gradient.
    ///
    /// # Errors
    ///
    /// Returns [`VectorPathError2d`] when the centre/radius is invalid or fewer
    /// than two valid stops are supplied.
    pub fn radial_gradient(
        center: [f32; 2],
        radius: f32,
        stops: impl Into<Vec<VectorGradientStop2d>>,
    ) -> Result<Self, VectorPathError2d> {
        if !center.iter().all(|value| value.is_finite()) {
            return Err(VectorPathError2d::NonFinitePoint);
        }
        if !radius.is_finite() || radius <= 0.0 {
            return Err(VectorPathError2d::InvalidGradientRadius);
        }
        let mut stops = stops.into();
        if stops.len() < 2 {
            return Err(VectorPathError2d::TooFewGradientStops);
        }
        stops.sort_by(|left, right| left.offset.total_cmp(&right.offset));
        Ok(Self::RadialGradient {
            center,
            radius,
            stops,
        })
    }

    fn color_at(&self, position: [f32; 2]) -> [f32; 4] {
        match self {
            Self::Solid(color) => *color,
            Self::LinearGradient { from, to, stops } => {
                let direction = [to[0] - from[0], to[1] - from[1]];
                let length_squared = direction[0] * direction[0] + direction[1] * direction[1];
                let offset = (((position[0] - from[0]) * direction[0]
                    + (position[1] - from[1]) * direction[1])
                    / length_squared)
                    .clamp(0.0, 1.0);
                Self::interpolate_stops(stops, offset)
            }
            Self::RadialGradient {
                center,
                radius,
                stops,
            } => {
                let dx = position[0] - center[0];
                let dy = position[1] - center[1];
                Self::interpolate_stops(stops, (dx.mul_add(dx, dy * dy)).sqrt() / radius)
            }
        }
    }

    fn interpolate_stops(stops: &[VectorGradientStop2d], offset: f32) -> [f32; 4] {
        if offset <= stops[0].offset {
            return stops[0].color;
        }
        for pair in stops.windows(2) {
            let [left, right] = pair else { continue };
            if offset <= right.offset {
                let blend = ((offset - left.offset)
                    / (right.offset - left.offset).max(f32::EPSILON))
                .clamp(0.0, 1.0);
                return std::array::from_fn(|index| {
                    left.color[index] + (right.color[index] - left.color[index]) * blend
                });
            }
        }
        stops.last().expect("validated gradient has stops").color
    }
}

/// A backend-neutral Bézier path that may be tessellated once into [`VectorMesh2d`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VectorPath2d {
    commands: Vec<VectorPathCommand2d>,
}

/// One command in a [`VectorPath2d`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VectorPathCommand2d {
    /// Starts a new subpath.
    MoveTo([f32; 2]),
    /// Appends a straight segment.
    LineTo([f32; 2]),
    /// Appends a quadratic Bézier segment.
    QuadraticTo {
        /// Quadratic Bézier control point.
        control: [f32; 2],
        /// Segment endpoint.
        to: [f32; 2],
    },
    /// Appends a cubic Bézier segment.
    CubicTo {
        /// First cubic Bézier control point.
        control1: [f32; 2],
        /// Second cubic Bézier control point.
        control2: [f32; 2],
        /// Segment endpoint.
        to: [f32; 2],
    },
    /// Closes the current subpath.
    Close,
}

impl VectorPath2d {
    /// Creates an empty path.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            commands: Vec::new(),
        }
    }

    /// Appends one validated command.
    ///
    /// # Errors
    ///
    /// Returns [`VectorPathError2d::NonFinitePoint`] if any command point is
    /// NaN or infinite.
    pub fn push(&mut self, command: VectorPathCommand2d) -> Result<(), VectorPathError2d> {
        let finite = match &command {
            VectorPathCommand2d::MoveTo(point) | VectorPathCommand2d::LineTo(point) => {
                point.iter().all(|value| value.is_finite())
            }
            VectorPathCommand2d::QuadraticTo { control, to } => control
                .iter()
                .chain(to.iter())
                .all(|value| value.is_finite()),
            VectorPathCommand2d::CubicTo {
                control1,
                control2,
                to,
            } => control1
                .iter()
                .chain(control2.iter())
                .chain(to.iter())
                .all(|value| value.is_finite()),
            VectorPathCommand2d::Close => true,
        };
        if !finite {
            return Err(VectorPathError2d::NonFinitePoint);
        }
        self.commands.push(command);
        Ok(())
    }

    /// Returns the recorded path commands.
    #[must_use]
    pub fn commands(&self) -> &[VectorPathCommand2d] {
        &self.commands
    }

    /// Tessellates the path into immutable GPU-ready triangles.
    ///
    /// # Errors
    ///
    /// Returns [`VectorPathError2d`] when the path is empty or cannot become a
    /// valid filled triangle mesh.
    pub fn tessellate_fill(&self, fill: &VectorFill2d) -> Result<VectorMesh2d, VectorPathError2d> {
        if self.commands.is_empty() {
            return Err(VectorPathError2d::EmptyPath);
        }
        let mut builder = Path::builder();
        for command in &self.commands {
            match command {
                VectorPathCommand2d::MoveTo(to) => {
                    builder.begin(point(to[0], to[1]));
                }
                VectorPathCommand2d::LineTo(to) => {
                    builder.line_to(point(to[0], to[1]));
                }
                VectorPathCommand2d::QuadraticTo { control, to } => {
                    builder.quadratic_bezier_to(point(control[0], control[1]), point(to[0], to[1]));
                }
                VectorPathCommand2d::CubicTo {
                    control1,
                    control2,
                    to,
                } => {
                    builder.cubic_bezier_to(
                        point(control1[0], control1[1]),
                        point(control2[0], control2[1]),
                        point(to[0], to[1]),
                    );
                }
                VectorPathCommand2d::Close => {
                    builder.close();
                }
            }
        }
        let path = builder.build();
        let mut buffers: VertexBuffers<VectorVertex2d, u32> = VertexBuffers::new();
        FillTessellator::new()
            .tessellate_path(
                &path,
                &FillOptions::default(),
                &mut BuffersBuilder::new(&mut buffers, |vertex: FillVertex<'_>| {
                    let position = vertex.position();
                    VectorVertex2d::new(
                        [position.x, position.y],
                        fill.color_at([position.x, position.y]),
                    )
                }),
            )
            .map_err(|_| VectorPathError2d::TessellationFailed)?;
        VectorMesh2d::new(buffers.vertices, buffers.indices).map_err(VectorPathError2d::Mesh)
    }
}

/// Failed path/gradient authoring or tessellation.
#[derive(Clone, Debug, PartialEq)]
pub enum VectorPathError2d {
    /// A command point was NaN or infinite.
    NonFinitePoint,
    /// A solid-fill channel was NaN or infinite.
    NonFiniteColor,
    /// A gradient stop was non-finite or outside `0..=1`.
    InvalidGradientStop,
    /// A linear gradient needs at least two stops.
    TooFewGradientStops,
    /// Gradient endpoints must be distinct.
    DegenerateGradient,
    /// A radial gradient radius must be finite and greater than zero.
    InvalidGradientRadius,
    /// Tessellation requires at least one path command.
    EmptyPath,
    /// The tessellator rejected the authored path.
    TessellationFailed,
    /// The tessellator produced invalid mesh data.
    Mesh(VectorMeshError2d),
}

impl fmt::Display for VectorPathError2d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid vector path: {self:?}")
    }
}

impl Error for VectorPathError2d {}

/// An immutable vector mesh uploaded to GPU memory.
pub struct GpuVectorMesh2d {
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
    bounds: VectorAabb2d,
}

/// One rectangular clip in physical presentation pixels.
///
/// The rectangle is intersected with the active [`RenderViewport`] immediately
/// before recording. A zero-sized or fully outside clip simply skips that draw;
/// it never expands a nested viewport or produces an invalid WGPU scissor.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct VectorClipRect2d {
    /// Physical horizontal origin, allowed to be negative before intersection.
    pub x: i32,
    /// Physical vertical origin, allowed to be negative before intersection.
    pub y: i32,
    /// Physical width in pixels.
    pub width: u32,
    /// Physical height in pixels.
    pub height: u32,
}

impl VectorClipRect2d {
    /// Creates a physical-pixel clip rectangle.
    #[must_use]
    pub const fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    fn intersect(self, viewport: RenderViewport) -> Option<ResolvedVectorClip> {
        let left = i64::from(self.x).max(i64::from(viewport.x()));
        let top = i64::from(self.y).max(i64::from(viewport.y()));
        let right = i64::from(self.x)
            .saturating_add(i64::from(self.width))
            .min(i64::from(viewport.x()) + i64::from(viewport.width()));
        let bottom = i64::from(self.y)
            .saturating_add(i64::from(self.height))
            .min(i64::from(viewport.y()) + i64::from(viewport.height()));
        let width = u32::try_from(right - left).ok()?;
        let height = u32::try_from(bottom - top).ok()?;
        if width == 0 || height == 0 {
            return None;
        }
        Some(ResolvedVectorClip {
            x: u32::try_from(left).ok()?,
            y: u32::try_from(top).ok()?,
            width,
            height,
        })
    }
}

/// Colour-composition rule for one vector draw.
///
/// Pipeline selection is explicit because blend modes cannot be switched as a
/// dynamic WGPU draw-state. Adjacent draws only batch when mesh, blend and clip
/// state match, preserving painter order.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum VectorBlendMode2d {
    /// Ordinary straight-alpha source-over blending.
    #[default]
    Alpha,
    /// Source-over blending for premultiplied vertex colours.
    PremultipliedAlpha,
    /// Adds straight-alpha source colour to the destination.
    Additive,
    /// Multiplies destination colour by source colour.
    Multiply,
}

impl GpuVectorMesh2d {
    /// Uploads an immutable mesh using the GPU device of the current frame.
    ///
    /// Retain the return value. Recreating it per frame defeats the feature's
    /// purpose and uploads all path topology again.
    ///
    /// # Errors
    ///
    /// Returns [`VectorSceneError2d::TooManyIndices`] when the mesh exceeds
    /// WGPU's `u32` indexed-draw range.
    pub fn upload_for_frame(
        frame: &RenderFrame<'_>,
        mesh: &VectorMesh2d,
    ) -> Result<Self, VectorSceneError2d> {
        let index_count =
            u32::try_from(mesh.indices.len()).map_err(|_| VectorSceneError2d::TooManyIndices {
                actual: mesh.indices.len(),
            })?;
        Ok(Self {
            vertex_buffer: frame
                .device()
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("yuyib retained vector vertices"),
                    contents: bytemuck::cast_slice(mesh.vertices()),
                    usage: wgpu::BufferUsages::VERTEX,
                }),
            index_buffer: frame
                .device()
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("yuyib retained vector indices"),
                    contents: bytemuck::cast_slice(mesh.indices()),
                    usage: wgpu::BufferUsages::INDEX,
                }),
            index_count,
            bounds: mesh.bounds(),
        })
    }

    /// Returns the number of indices in this immutable GPU mesh.
    #[must_use]
    pub const fn index_count(&self) -> u32 {
        self.index_count
    }

    /// Returns the immutable mesh-local bounds used during draw extraction.
    #[must_use]
    pub const fn bounds(&self) -> VectorAabb2d {
        self.bounds
    }
}

/// A cheap per-frame instance of one retained vector mesh.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VectorDraw2d {
    /// World-space origin added after scale and rotation.
    pub position: [f32; 2],
    /// Mesh-local scale; negative axes mirror the mesh.
    pub scale: [f32; 2],
    /// Clockwise rotation in radians, following [`Camera2d`]'s Y-down space.
    pub rotation_radians: f32,
    /// Multiplies interpolated vertex colour.
    pub tint: [f32; 4],
    /// Stable painter order. Higher layers render later.
    pub layer: i32,
    /// Colour-composition pipeline selected for this draw.
    pub blend: VectorBlendMode2d,
    /// Optional physical-pixel scissor clip.
    pub clip: Option<VectorClipRect2d>,
}

impl VectorDraw2d {
    /// Creates an identity-transformed vector instance.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            position: [0.0, 0.0],
            scale: [1.0, 1.0],
            rotation_radians: 0.0,
            tint: [1.0; 4],
            layer: 0,
            blend: VectorBlendMode2d::Alpha,
            clip: None,
        }
    }

    /// Sets the world-space origin.
    #[must_use]
    pub const fn with_position(mut self, position: [f32; 2]) -> Self {
        self.position = position;
        self
    }

    /// Sets mesh-local scale.
    #[must_use]
    pub const fn with_scale(mut self, scale: [f32; 2]) -> Self {
        self.scale = scale;
        self
    }

    /// Sets clockwise rotation in radians.
    #[must_use]
    pub const fn with_rotation(mut self, rotation_radians: f32) -> Self {
        self.rotation_radians = rotation_radians;
        self
    }

    /// Sets the colour multiplier.
    #[must_use]
    pub const fn with_tint(mut self, tint: [f32; 4]) -> Self {
        self.tint = tint;
        self
    }

    /// Sets stable painter order.
    #[must_use]
    pub const fn with_layer(mut self, layer: i32) -> Self {
        self.layer = layer;
        self
    }

    /// Sets the colour-composition pipeline.
    #[must_use]
    pub const fn with_blend(mut self, blend: VectorBlendMode2d) -> Self {
        self.blend = blend;
        self
    }

    /// Sets an optional physical-pixel scissor clip.
    #[must_use]
    pub const fn with_clip(mut self, clip: Option<VectorClipRect2d>) -> Self {
        self.clip = clip;
        self
    }
}

impl Default for VectorDraw2d {
    fn default() -> Self {
        Self::new()
    }
}

/// Observable work submitted by [`VectorRenderer2d`] or [`RetainedVectorScene2d`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VectorDrawStats2d {
    /// Number of transform/tint instances uploaded this frame.
    pub instances: u32,
    /// Number of indexed GPU calls required to preserve painter order.
    pub draw_calls: u32,
    /// Number of indexed triangles submitted after instancing.
    pub triangles: u64,
    /// Instances rejected by conservative camera culling before GPU upload.
    pub culled_instances: u32,
}

/// Explicit per-frame work limit for retained vector rendering.
///
/// A budget makes pathological content visible at the integration boundary
/// instead of silently allocating an unbounded instance buffer. `None` keeps
/// the corresponding dimension unlimited.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VectorRenderBudget2d {
    max_instances: Option<u32>,
    max_draw_calls: Option<u32>,
}

impl VectorRenderBudget2d {
    /// Creates a budget with no artificial limits.
    #[must_use]
    pub const fn unlimited() -> Self {
        Self {
            max_instances: None,
            max_draw_calls: None,
        }
    }

    /// Limits the number of submitted instances in a frame.
    #[must_use]
    pub const fn with_max_instances(mut self, max_instances: u32) -> Self {
        self.max_instances = Some(max_instances);
        self
    }

    /// Limits mesh-switch draw calls in a frame.
    #[must_use]
    pub const fn with_max_draw_calls(mut self, max_draw_calls: u32) -> Self {
        self.max_draw_calls = Some(max_draw_calls);
        self
    }

    /// Returns the optional instance limit.
    #[must_use]
    pub const fn max_instances(self) -> Option<u32> {
        self.max_instances
    }

    /// Returns the optional draw-call limit.
    #[must_use]
    pub const fn max_draw_calls(self) -> Option<u32> {
        self.max_draw_calls
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ResolvedVectorClip {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

struct VectorDrawRange<'mesh> {
    mesh: &'mesh GpuVectorMesh2d,
    start: u32,
    count: u32,
    blend: VectorBlendMode2d,
    clip: Option<ResolvedVectorClip>,
}

struct VectorPipelines2d {
    alpha: wgpu::RenderPipeline,
    premultiplied_alpha: wgpu::RenderPipeline,
    additive: wgpu::RenderPipeline,
    multiply: wgpu::RenderPipeline,
}

impl VectorPipelines2d {
    fn create(
        device: &wgpu::Device,
        layout: &wgpu::PipelineLayout,
        shader: &wgpu::ShaderModule,
        format: wgpu::TextureFormat,
    ) -> Self {
        Self {
            alpha: create_vector_pipeline(
                device,
                layout,
                shader,
                format,
                "yuyib retained vector alpha pipeline",
                wgpu::BlendState::ALPHA_BLENDING,
            ),
            premultiplied_alpha: create_vector_pipeline(
                device,
                layout,
                shader,
                format,
                "yuyib retained vector premultiplied-alpha pipeline",
                wgpu::BlendState {
                    color: wgpu::BlendComponent {
                        src_factor: wgpu::BlendFactor::One,
                        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                        operation: wgpu::BlendOperation::Add,
                    },
                    alpha: wgpu::BlendComponent::OVER,
                },
            ),
            additive: create_vector_pipeline(
                device,
                layout,
                shader,
                format,
                "yuyib retained vector additive pipeline",
                wgpu::BlendState {
                    color: wgpu::BlendComponent {
                        src_factor: wgpu::BlendFactor::SrcAlpha,
                        dst_factor: wgpu::BlendFactor::One,
                        operation: wgpu::BlendOperation::Add,
                    },
                    alpha: wgpu::BlendComponent {
                        src_factor: wgpu::BlendFactor::One,
                        dst_factor: wgpu::BlendFactor::One,
                        operation: wgpu::BlendOperation::Add,
                    },
                },
            ),
            multiply: create_vector_pipeline(
                device,
                layout,
                shader,
                format,
                "yuyib retained vector multiply pipeline",
                wgpu::BlendState {
                    color: wgpu::BlendComponent {
                        src_factor: wgpu::BlendFactor::Dst,
                        dst_factor: wgpu::BlendFactor::Zero,
                        operation: wgpu::BlendOperation::Add,
                    },
                    alpha: wgpu::BlendComponent::OVER,
                },
            ),
        }
    }

    const fn get(&self, mode: VectorBlendMode2d) -> &wgpu::RenderPipeline {
        match mode {
            VectorBlendMode2d::Alpha => &self.alpha,
            VectorBlendMode2d::PremultipliedAlpha => &self.premultiplied_alpha,
            VectorBlendMode2d::Additive => &self.additive,
            VectorBlendMode2d::Multiply => &self.multiply,
        }
    }
}

fn create_vector_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
    label: &'static str,
    blend: wgpu::BlendState,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[Some(VECTOR_VERTEX_LAYOUT), Some(VECTOR_INSTANCE_LAYOUT)],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(blend),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

/// GPU renderer for ordered retained vector mesh instances.
pub struct VectorRenderer2d {
    pipelines: VectorPipelines2d,
    camera_bind_group: wgpu::BindGroup,
    camera_buffer: wgpu::Buffer,
    instance_buffer: Option<wgpu::Buffer>,
    instance_capacity: u32,
}

impl VectorRenderer2d {
    /// Creates the GPU pipeline from the current frame and retains it for future frames.
    #[must_use]
    pub fn new_for_frame(frame: &RenderFrame<'_>) -> Self {
        Self::create(frame.device(), frame.surface_format())
    }

    /// Draws an ordered stream of retained meshes in one surface pass.
    ///
    /// The input is stably sorted by [`VectorDraw2d::layer`]. Consecutive
    /// instances of the same mesh are instanced together; a mesh switch only
    /// costs an indexed draw, not new path tessellation or vertex uploads.
    ///
    /// # Errors
    ///
    /// Returns [`VectorSceneError2d`] for invalid transforms, an invalid
    /// camera, or more instances than WGPU can address.
    ///
    /// # Panics
    ///
    /// Panics only when the internal instance-buffer invariant is violated
    /// after a non-empty, validated instance stream has allocated the buffer.
    pub fn draw_ordered<'mesh>(
        &mut self,
        frame: &mut RenderFrame<'_>,
        camera: Camera2d,
        draws: impl IntoIterator<Item = (&'mesh GpuVectorMesh2d, VectorDraw2d)>,
    ) -> Result<VectorDrawStats2d, VectorSceneError2d> {
        self.draw_ordered_with_budget(frame, camera, draws, VectorRenderBudget2d::unlimited())
    }

    /// Draws retained meshes while enforcing a caller-provided work budget.
    ///
    /// This is useful for an engine-owned render stage that needs predictable
    /// frame time in the presence of user-authored or streamed content.
    ///
    /// # Errors
    ///
    /// Returns [`VectorSceneError2d::InstanceBudgetExceeded`] or
    /// [`VectorSceneError2d::DrawCallBudgetExceeded`] before a surface pass is
    /// opened when the supplied stream exceeds `budget`.
    #[allow(clippy::too_many_lines)]
    pub fn draw_ordered_with_budget<'mesh>(
        &mut self,
        frame: &mut RenderFrame<'_>,
        camera: Camera2d,
        draws: impl IntoIterator<Item = (&'mesh GpuVectorMesh2d, VectorDraw2d)>,
        budget: VectorRenderBudget2d,
    ) -> Result<VectorDrawStats2d, VectorSceneError2d> {
        let mut ordered: Vec<(&GpuVectorMesh2d, VectorDraw2d)> = draws.into_iter().collect();
        for (_, draw) in &ordered {
            validate_draw(*draw)?;
        }
        ordered.sort_by_key(|(_, draw)| draw.layer);
        if ordered.is_empty() {
            return Ok(VectorDrawStats2d::default());
        }
        let camera_viewport = camera
            .viewport(frame.draw_size())
            .map_err(VectorSceneError2d::InvalidCamera)?;
        let viewport = frame.viewport();
        let mut culled_instances = 0_u32;
        let ordered: Vec<_> = ordered
            .into_iter()
            .filter_map(|(mesh, draw)| {
                if !is_draw_visible(mesh.bounds(), draw, camera_viewport) {
                    culled_instances = culled_instances.saturating_add(1);
                    return None;
                }
                match draw.clip {
                    Some(clip) => clip
                        .intersect(viewport)
                        .map(|clip| (mesh, draw, Some(clip))),
                    None => Some((mesh, draw, None)),
                }
            })
            .collect();
        if ordered.is_empty() {
            return Ok(VectorDrawStats2d {
                culled_instances,
                ..VectorDrawStats2d::default()
            });
        }
        let total =
            u32::try_from(ordered.len()).map_err(|_| VectorSceneError2d::TooManyInstances {
                actual: ordered.len(),
            })?;
        if let Some(maximum) = budget.max_instances
            && total > maximum
        {
            return Err(VectorSceneError2d::InstanceBudgetExceeded { total, maximum });
        }
        let projection = camera
            .projection(frame.draw_size())
            .map_err(VectorSceneError2d::InvalidCamera)?;
        frame
            .queue()
            .write_buffer(&self.camera_buffer, 0, bytemuck::cast_slice(&projection));
        self.ensure_instance_capacity(frame.device(), total);
        let packed: Vec<GpuVectorInstance> = ordered
            .iter()
            .map(|(_, draw, _)| GpuVectorInstance::from_draw(*draw))
            .collect();
        let instance_buffer = self
            .instance_buffer
            .as_ref()
            .expect("non-empty vector stream creates an instance buffer");
        frame
            .queue()
            .write_buffer(instance_buffer, 0, bytemuck::cast_slice(&packed));

        let mut ranges: Vec<VectorDrawRange<'_>> = Vec::new();
        for (offset, (mesh, draw, clip)) in ordered.iter().enumerate() {
            let offset = u32::try_from(offset).expect("total was checked as u32");
            if let Some(last) = ranges.last_mut()
                && std::ptr::eq(last.mesh, *mesh)
                && last.blend == draw.blend
                && last.clip == *clip
            {
                last.count += 1;
            } else {
                ranges.push(VectorDrawRange {
                    mesh,
                    start: offset,
                    count: 1,
                    blend: draw.blend,
                    clip: *clip,
                });
            }
        }
        let draw_calls = u32::try_from(ranges.len()).unwrap_or(u32::MAX);
        if let Some(maximum) = budget.max_draw_calls
            && draw_calls > maximum
        {
            return Err(VectorSceneError2d::DrawCallBudgetExceeded {
                total: draw_calls,
                maximum,
            });
        }
        let triangles = ranges
            .iter()
            .map(|range| u64::from(range.mesh.index_count / 3) * u64::from(range.count))
            .sum();
        frame.with_surface_pass(wgpu::LoadOp::Load, |pass| {
            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            pass.set_vertex_buffer(1, instance_buffer.slice(..));
            for range in &ranges {
                pass.set_pipeline(self.pipelines.get(range.blend));
                if let Some(clip) = range.clip {
                    pass.set_scissor_rect(clip.x, clip.y, clip.width, clip.height);
                } else {
                    pass.set_scissor_rect(
                        viewport.x(),
                        viewport.y(),
                        viewport.width(),
                        viewport.height(),
                    );
                }
                pass.set_vertex_buffer(0, range.mesh.vertex_buffer.slice(..));
                pass.set_index_buffer(range.mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(
                    0..range.mesh.index_count,
                    0,
                    range.start..range.start + range.count,
                );
            }
        });
        Ok(VectorDrawStats2d {
            instances: total,
            draw_calls,
            triangles,
            culled_instances,
        })
    }

    #[allow(clippy::too_many_lines)]
    fn create(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let camera_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("yuyib vector camera layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });
        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("yuyib vector camera"),
            size: size_of::<[f32; 16]>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("yuyib vector camera bind group"),
            layout: &camera_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("yuyib retained vector WGSL"),
            source: wgpu::ShaderSource::Wgsl(VECTOR_WGSL.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("yuyib vector pipeline layout"),
            bind_group_layouts: &[Some(&camera_bind_group_layout)],
            immediate_size: 0,
        });
        Self {
            pipelines: VectorPipelines2d::create(device, &pipeline_layout, &shader, format),
            camera_bind_group,
            camera_buffer,
            instance_buffer: None,
            instance_capacity: 0,
        }
    }

    fn ensure_instance_capacity(&mut self, device: &wgpu::Device, required: u32) {
        if required <= self.instance_capacity {
            return;
        }
        let capacity = required.checked_next_power_of_two().unwrap_or(required);
        self.instance_buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("yuyib retained vector instances"),
            size: u64::from(capacity) * size_of::<GpuVectorInstance>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        self.instance_capacity = capacity;
    }
}

/// Stable handle owned by [`RetainedVectorScene2d`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct VectorMeshId2d(u32);

/// High-level retained scene for immutable vector assets and dynamic instances.
///
/// This facade owns CPU mesh assets and their lazy GPU upload. Call
/// [`Self::set_draws`] only with current transforms; unchanged geometry stays
/// resident on the GPU. Replacing a mesh invalidates only that mesh's upload.
#[derive(Default)]
pub struct RetainedVectorScene2d {
    renderer: Option<VectorRenderer2d>,
    meshes: HashMap<VectorMeshId2d, RetainedMesh>,
    static_layers: BTreeMap<i32, Vec<(VectorMeshId2d, VectorDraw2d)>>,
    draws: Vec<(VectorMeshId2d, VectorDraw2d)>,
    budget: VectorRenderBudget2d,
    next_mesh_id: u32,
}

struct RetainedMesh {
    cpu: VectorMesh2d,
    gpu: Option<GpuVectorMesh2d>,
}

impl RetainedVectorScene2d {
    /// Configures the maximum dynamic and static work accepted per frame.
    ///
    /// Existing content remains registered; rendering returns a clear error
    /// until the budget is raised or the submitted scene becomes smaller.
    pub fn set_render_budget(&mut self, budget: VectorRenderBudget2d) {
        self.budget = budget;
    }

    /// Returns the currently configured render budget.
    #[must_use]
    pub const fn render_budget(&self) -> VectorRenderBudget2d {
        self.budget
    }

    /// Registers immutable mesh topology and returns its stable scene handle.
    ///
    /// # Errors
    ///
    /// Returns [`VectorSceneError2d::MeshIdExhausted`] only after exhausting
    /// the scene's monotonically assigned `u32` handles.
    pub fn insert_mesh(
        &mut self,
        mesh: VectorMesh2d,
    ) -> Result<VectorMeshId2d, VectorSceneError2d> {
        let next = self
            .next_mesh_id
            .checked_add(1)
            .ok_or(VectorSceneError2d::MeshIdExhausted)?;
        self.next_mesh_id = next;
        let id = VectorMeshId2d(next);
        self.meshes.insert(
            id,
            RetainedMesh {
                cpu: mesh,
                gpu: None,
            },
        );
        Ok(id)
    }

    /// Replaces topology for one retained mesh and invalidates only its GPU allocation.
    ///
    /// # Errors
    ///
    /// Returns [`VectorSceneError2d::UnknownMesh`] when `id` is not registered.
    pub fn replace_mesh(
        &mut self,
        id: VectorMeshId2d,
        mesh: VectorMesh2d,
    ) -> Result<(), VectorSceneError2d> {
        let retained = self
            .meshes
            .get_mut(&id)
            .ok_or(VectorSceneError2d::UnknownMesh { id })?;
        retained.cpu = mesh;
        retained.gpu = None;
        Ok(())
    }

    /// Replaces the dynamic per-frame instance list without touching mesh topology.
    ///
    /// # Errors
    ///
    /// Returns [`VectorSceneError2d`] when a handle is unknown or a draw has
    /// a non-finite/zero transform. Existing draws remain unchanged on error.
    pub fn set_draws(
        &mut self,
        draws: impl IntoIterator<Item = (VectorMeshId2d, VectorDraw2d)>,
    ) -> Result<(), VectorSceneError2d> {
        let draws: Vec<_> = draws.into_iter().collect();
        for &(id, draw) in &draws {
            if !self.meshes.contains_key(&id) {
                return Err(VectorSceneError2d::UnknownMesh { id });
            }
            validate_draw(draw)?;
        }
        self.draws = draws;
        Ok(())
    }

    /// Replaces an immutable painter layer without touching dynamic draws.
    ///
    /// This is designed for map chunks, background dressing and other content
    /// whose membership changes at load/unload time rather than every frame.
    /// Every supplied draw is assigned `layer`, which prevents accidental
    /// cross-layer ordering bugs. Mesh topology remains resident and is still
    /// uploaded only once.
    ///
    /// # Errors
    ///
    /// Returns an error without changing the previous layer when a mesh handle
    /// is unknown or a draw transform is invalid.
    pub fn set_static_layer_draws(
        &mut self,
        layer: i32,
        draws: impl IntoIterator<Item = (VectorMeshId2d, VectorDraw2d)>,
    ) -> Result<(), VectorSceneError2d> {
        let mut draws: Vec<_> = draws.into_iter().collect();
        for (id, draw) in &mut draws {
            if !self.meshes.contains_key(id) {
                return Err(VectorSceneError2d::UnknownMesh { id: *id });
            }
            draw.layer = layer;
            validate_draw(*draw)?;
        }
        if draws.is_empty() {
            self.static_layers.remove(&layer);
        } else {
            self.static_layers.insert(layer, draws);
        }
        Ok(())
    }

    /// Drops one retained static layer and returns whether it existed.
    pub fn remove_static_layer(&mut self, layer: i32) -> bool {
        self.static_layers.remove(&layer).is_some()
    }

    /// Returns the number of registered static painter layers.
    #[must_use]
    pub fn static_layer_count(&self) -> usize {
        self.static_layers.len()
    }

    /// Renders all current instances while lazily uploading new/replaced meshes.
    ///
    /// # Errors
    ///
    /// Returns upload, draw validation, or camera-projection failures.
    ///
    /// # Panics
    ///
    /// Panics only if the renderer initialization invariant is violated after
    /// this method successfully created its internal renderer.
    pub fn render(
        &mut self,
        frame: &mut RenderFrame<'_>,
        camera: Camera2d,
    ) -> Result<VectorDrawStats2d, VectorSceneError2d> {
        if self.renderer.is_none() {
            self.renderer = Some(VectorRenderer2d::new_for_frame(frame));
        }
        for retained in self.meshes.values_mut() {
            if retained.gpu.is_none() {
                retained.gpu = Some(GpuVectorMesh2d::upload_for_frame(frame, &retained.cpu)?);
            }
        }
        let Self {
            renderer,
            meshes,
            static_layers,
            draws,
            budget,
            ..
        } = self;
        let static_draw_count: usize = static_layers.values().map(Vec::len).sum();
        let mut ordered = Vec::with_capacity(static_draw_count + draws.len());
        for layer_draws in static_layers.values() {
            Self::append_gpu_draws(meshes, layer_draws, &mut ordered)?;
        }
        Self::append_gpu_draws(meshes, draws, &mut ordered)?;
        renderer
            .as_mut()
            .expect("renderer is initialized above")
            .draw_ordered_with_budget(frame, camera, ordered, *budget)
    }

    fn append_gpu_draws<'mesh>(
        meshes: &'mesh HashMap<VectorMeshId2d, RetainedMesh>,
        draws: &[(VectorMeshId2d, VectorDraw2d)],
        target: &mut Vec<(&'mesh GpuVectorMesh2d, VectorDraw2d)>,
    ) -> Result<(), VectorSceneError2d> {
        for &(id, draw) in draws {
            let mesh = meshes
                .get(&id)
                .and_then(|retained| retained.gpu.as_ref())
                .ok_or(VectorSceneError2d::UnknownMesh { id })?;
            target.push((mesh, draw));
        }
        Ok(())
    }
}

/// Errors while staging or rendering retained vector geometry.
#[derive(Clone, Debug, PartialEq)]
pub enum VectorSceneError2d {
    /// A per-frame transform or tint was non-finite.
    NonFiniteDraw,
    /// A mesh instance with a zero scale is ambiguous and invisible.
    ZeroScaleDraw,
    /// A configured mesh handle is absent from this retained scene.
    UnknownMesh {
        /// Handle that is absent from the retained scene.
        id: VectorMeshId2d,
    },
    /// The scene exhausted its monotonically assigned mesh IDs.
    MeshIdExhausted,
    /// WGPU indexed drawing accepts at most `u32::MAX` indices.
    TooManyIndices {
        /// Observed number of indices.
        actual: usize,
    },
    /// WGPU instanced drawing accepts at most `u32::MAX` instances.
    TooManyInstances {
        /// Observed number of instances.
        actual: usize,
    },
    /// The submitted scene exceeded the configured per-frame instance budget.
    InstanceBudgetExceeded {
        /// Number of instances submitted.
        total: u32,
        /// Configured maximum number of instances.
        maximum: u32,
    },
    /// The submitted scene exceeded the configured mesh-switch draw-call budget.
    DrawCallBudgetExceeded {
        /// Number of draw calls required to preserve painter order.
        total: u32,
        /// Configured maximum number of draw calls.
        maximum: u32,
    },
    /// The shared 2D camera cannot create a finite projection.
    InvalidCamera(crate::SpriteRenderError),
}

impl fmt::Display for VectorSceneError2d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFiniteDraw => {
                formatter.write_str("vector draw contains non-finite transform or tint data")
            }
            Self::ZeroScaleDraw => formatter.write_str("vector draw scale must be non-zero"),
            Self::UnknownMesh { .. } => {
                formatter.write_str("vector draw refers to an unknown retained mesh")
            }
            Self::MeshIdExhausted => {
                formatter.write_str("retained vector scene exhausted mesh IDs")
            }
            Self::TooManyIndices { actual } => write!(
                formatter,
                "vector mesh has {actual} indices; maximum is u32::MAX"
            ),
            Self::TooManyInstances { actual } => write!(
                formatter,
                "vector scene has {actual} instances; maximum is u32::MAX"
            ),
            Self::InstanceBudgetExceeded { total, maximum } => write!(
                formatter,
                "vector scene submitted {total} instances; budget is {maximum}"
            ),
            Self::DrawCallBudgetExceeded { total, maximum } => write!(
                formatter,
                "vector scene requires {total} draw calls; budget is {maximum}"
            ),
            Self::InvalidCamera(error) => write!(formatter, "invalid vector camera: {error}"),
        }
    }
}

impl Error for VectorSceneError2d {}

fn validate_draw(draw: VectorDraw2d) -> Result<(), VectorSceneError2d> {
    if !draw
        .position
        .iter()
        .chain(draw.scale.iter())
        .chain(std::iter::once(&draw.rotation_radians))
        .chain(draw.tint.iter())
        .all(|value| value.is_finite())
    {
        return Err(VectorSceneError2d::NonFiniteDraw);
    }
    if draw.scale[0] == 0.0 || draw.scale[1] == 0.0 {
        return Err(VectorSceneError2d::ZeroScaleDraw);
    }
    Ok(())
}

fn is_draw_visible(
    bounds: VectorAabb2d,
    draw: VectorDraw2d,
    camera_viewport: ([f32; 2], [f32; 2]),
) -> bool {
    let local_center = [
        (bounds.min[0] + bounds.max[0]) * 0.5,
        (bounds.min[1] + bounds.max[1]) * 0.5,
    ];
    let local_half = [
        (bounds.max[0] - bounds.min[0]) * 0.5,
        (bounds.max[1] - bounds.min[1]) * 0.5,
    ];
    let cosine = draw.rotation_radians.cos();
    let sine = draw.rotation_radians.sin();
    let scaled_center = [
        local_center[0] * draw.scale[0],
        local_center[1] * draw.scale[1],
    ];
    let center = [
        draw.position[0] + scaled_center[0] * cosine - scaled_center[1] * sine,
        draw.position[1] + scaled_center[0] * sine + scaled_center[1] * cosine,
    ];
    let scaled_half = [
        local_half[0] * draw.scale[0].abs(),
        local_half[1] * draw.scale[1].abs(),
    ];
    let half = [
        cosine.abs() * scaled_half[0] + sine.abs() * scaled_half[1],
        sine.abs() * scaled_half[0] + cosine.abs() * scaled_half[1],
    ];
    let right = camera_viewport.0[0] + camera_viewport.1[0];
    let bottom = camera_viewport.0[1] + camera_viewport.1[1];
    center[0] + half[0] > camera_viewport.0[0]
        && center[0] - half[0] < right
        && center[1] + half[1] > camera_viewport.0[1]
        && center[1] - half[1] < bottom
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct GpuVectorInstance {
    position: [f32; 2],
    scale: [f32; 2],
    tint: [f32; 4],
    rotation_radians: f32,
}

impl GpuVectorInstance {
    fn from_draw(draw: VectorDraw2d) -> Self {
        Self {
            position: draw.position,
            scale: draw.scale,
            tint: draw.tint,
            rotation_radians: draw.rotation_radians,
        }
    }
}

const VECTOR_VERTEX_LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
    array_stride: size_of::<VectorVertex2d>() as wgpu::BufferAddress,
    step_mode: wgpu::VertexStepMode::Vertex,
    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x4],
};
const VECTOR_INSTANCE_LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
    array_stride: size_of::<GpuVectorInstance>() as wgpu::BufferAddress,
    step_mode: wgpu::VertexStepMode::Instance,
    attributes: &wgpu::vertex_attr_array![2 => Float32x2, 3 => Float32x2, 4 => Float32x4, 5 => Float32],
};

const VECTOR_WGSL: &str = r"
struct Camera { projection: mat4x4<f32>, };
@group(0) @binding(0) var<uniform> camera: Camera;

struct VertexInput {
    @location(0) local_position: vec2<f32>,
    @location(1) local_color: vec4<f32>,
    @location(2) position: vec2<f32>,
    @location(3) scale: vec2<f32>,
    @location(4) tint: vec4<f32>,
    @location(5) rotation_radians: f32,
};
struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
};
@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    let c = cos(input.rotation_radians);
    let s = sin(input.rotation_radians);
    let scaled = input.local_position * input.scale;
    let rotated = vec2<f32>(scaled.x * c - scaled.y * s, scaled.x * s + scaled.y * c);
    var output: VertexOutput;
    output.clip_position = camera.projection * vec4<f32>(input.position + rotated, 0.0, 1.0);
    output.color = input.local_color * input.tint;
    return output;
}
@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> { return input.color; }
";

#[cfg(test)]
mod tests {
    use super::*;

    fn mesh() -> VectorMesh2d {
        VectorMesh2d::new(
            vec![
                VectorVertex2d::new([0.0, 0.0], [1.0; 4]),
                VectorVertex2d::new([1.0, 0.0], [1.0; 4]),
                VectorVertex2d::new([0.0, 1.0], [1.0; 4]),
            ],
            vec![0, 1, 2],
        )
        .expect("triangle is valid")
    }

    #[test]
    fn mesh_rejects_out_of_bounds_indices() {
        let error = VectorMesh2d::new(
            vec![VectorVertex2d::new([0.0, 0.0], [1.0; 4])],
            vec![0, 1, 0],
        )
        .expect_err("index one does not exist");
        assert!(matches!(
            error,
            VectorMeshError2d::IndexOutOfBounds { index: 1, .. }
        ));
    }

    #[test]
    fn retained_scene_rejects_unknown_draw_mesh_without_changing_draws() {
        let mut scene = RetainedVectorScene2d::default();
        let known = scene.insert_mesh(mesh()).expect("mesh ID is available");
        scene
            .set_draws([(known, VectorDraw2d::new())])
            .expect("known mesh is accepted");
        let error = scene
            .set_draws([(VectorMeshId2d(999), VectorDraw2d::new())])
            .expect_err("unknown mesh is rejected");
        assert!(matches!(error, VectorSceneError2d::UnknownMesh { .. }));
        assert_eq!(scene.draws.len(), 1);
    }

    #[test]
    fn static_layers_normalise_painter_order_and_can_be_removed() {
        let mut scene = RetainedVectorScene2d::default();
        let known = scene.insert_mesh(mesh()).expect("mesh ID is available");
        scene
            .set_static_layer_draws(42, [(known, VectorDraw2d::new().with_layer(-5))])
            .expect("known mesh is accepted");
        assert_eq!(scene.static_layers[&42][0].1.layer, 42);
        assert_eq!(scene.static_layer_count(), 1);
        assert!(scene.remove_static_layer(42));
        assert_eq!(scene.static_layer_count(), 0);
    }

    #[test]
    fn vector_draw_rejects_non_finite_values() {
        let error = validate_draw(VectorDraw2d::new().with_rotation(f32::NAN))
            .expect_err("NaN is never sent to the GPU");
        assert_eq!(error, VectorSceneError2d::NonFiniteDraw);
    }

    #[test]
    fn physical_clip_is_intersected_with_the_active_viewport() {
        let viewport = RenderViewport::new(20, 10, 100, 80).expect("valid viewport");
        let clip = VectorClipRect2d::new(-5, 60, 60, 80)
            .intersect(viewport)
            .expect("clip overlaps viewport");
        assert_eq!(clip.x, 20);
        assert_eq!(clip.y, 60);
        assert_eq!(clip.width, 35);
        assert_eq!(clip.height, 30);
        assert!(
            VectorClipRect2d::new(200, 200, 5, 5)
                .intersect(viewport)
                .is_none()
        );
    }

    #[test]
    fn vector_draw_carries_explicit_blend_and_clip_state() {
        let draw = VectorDraw2d::new()
            .with_blend(VectorBlendMode2d::Additive)
            .with_clip(Some(VectorClipRect2d::new(1, 2, 3, 4)));
        assert_eq!(draw.blend, VectorBlendMode2d::Additive);
        assert_eq!(draw.clip, Some(VectorClipRect2d::new(1, 2, 3, 4)));
    }

    #[test]
    fn conservative_culling_keeps_rotated_overlap_and_rejects_offscreen_draws() {
        let bounds = VectorAabb2d {
            min: [-1.0, -1.0],
            max: [1.0, 1.0],
        };
        let viewport = ([0.0, 0.0], [10.0, 10.0]);
        assert!(is_draw_visible(
            bounds,
            VectorDraw2d::new()
                .with_position([-0.5, 5.0])
                .with_rotation(std::f32::consts::FRAC_PI_4),
            viewport,
        ));
        assert!(!is_draw_visible(
            bounds,
            VectorDraw2d::new().with_position([20.0, 5.0]),
            viewport,
        ));
    }

    #[test]
    fn path_tessellation_bakes_a_linear_gradient() {
        let mut path = VectorPath2d::new();
        path.push(VectorPathCommand2d::MoveTo([0.0, 0.0]))
            .expect("finite point");
        path.push(VectorPathCommand2d::QuadraticTo {
            control: [0.5, 1.0],
            to: [1.0, 0.0],
        })
        .expect("finite curve");
        path.push(VectorPathCommand2d::LineTo([0.0, 0.0]))
            .expect("finite point");
        path.push(VectorPathCommand2d::Close)
            .expect("close is always finite");
        let fill = VectorFill2d::linear_gradient(
            [0.0, 0.0],
            [1.0, 0.0],
            [
                VectorGradientStop2d::new(0.0, [1.0, 0.0, 0.0, 1.0]).expect("valid stop"),
                VectorGradientStop2d::new(1.0, [0.0, 0.0, 1.0, 1.0]).expect("valid stop"),
            ],
        )
        .expect("valid gradient");
        let mesh = path
            .tessellate_fill(&fill)
            .expect("closed curve is fillable");
        assert!(mesh.triangle_count() > 0);
        assert!(
            mesh.vertices()
                .iter()
                .any(|vertex| vertex.color[0] != vertex.color[2])
        );
    }

    #[test]
    fn radial_gradient_rejects_a_zero_radius() {
        let error = VectorFill2d::radial_gradient(
            [0.0, 0.0],
            0.0,
            [
                VectorGradientStop2d::new(0.0, [1.0; 4]).expect("valid stop"),
                VectorGradientStop2d::new(1.0, [0.0; 4]).expect("valid stop"),
            ],
        )
        .expect_err("zero radius cannot define a radial gradient");
        assert_eq!(error, VectorPathError2d::InvalidGradientRadius);
    }
}
