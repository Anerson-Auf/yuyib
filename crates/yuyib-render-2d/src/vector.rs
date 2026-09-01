//! Retained GPU vector geometry for dense procedural 2D worlds.
//!
//! A [`VectorMesh2d`] is immutable, already-tessellated triangle geometry.
//! Upload it once as [`GpuVectorMesh2d`], then submit lightweight
//! [`VectorDraw2d`] transforms every frame. This deliberately avoids a
//! Canvas-style command stream, per-frame path tessellation, and CPU-side
//! rasterisation. It is intended for authored SVG/path importers, procedural
//! character parts, static world chunks, decals, and other geometry whose
//! topology changes much less often than its transform.

use std::{collections::HashMap, error::Error, fmt, mem::size_of};

use bytemuck::{Pod, Zeroable};
use lyon_path::{Path, math::point};
use lyon_tessellation::{BuffersBuilder, FillOptions, FillTessellator, FillVertex, VertexBuffers};
use wgpu::util::DeviceExt;
use yuyib_render::{RenderFrame, wgpu};

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
        Ok(Self { vertices, indices })
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
        })
    }

    /// Returns the number of indices in this immutable GPU mesh.
    #[must_use]
    pub const fn index_count(&self) -> u32 {
        self.index_count
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
}

/// GPU renderer for ordered retained vector mesh instances.
pub struct VectorRenderer2d {
    pipeline: wgpu::RenderPipeline,
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
        let mut ordered: Vec<(&GpuVectorMesh2d, VectorDraw2d)> = draws.into_iter().collect();
        for (_, draw) in &ordered {
            validate_draw(*draw)?;
        }
        ordered.sort_by_key(|(_, draw)| draw.layer);
        if ordered.is_empty() {
            return Ok(VectorDrawStats2d::default());
        }
        let total =
            u32::try_from(ordered.len()).map_err(|_| VectorSceneError2d::TooManyInstances {
                actual: ordered.len(),
            })?;
        let projection = camera
            .projection(frame.draw_size())
            .map_err(VectorSceneError2d::InvalidCamera)?;
        frame
            .queue()
            .write_buffer(&self.camera_buffer, 0, bytemuck::cast_slice(&projection));
        self.ensure_instance_capacity(frame.device(), total);
        let packed: Vec<GpuVectorInstance> = ordered
            .iter()
            .map(|(_, draw)| GpuVectorInstance::from_draw(*draw))
            .collect();
        let instance_buffer = self
            .instance_buffer
            .as_ref()
            .expect("non-empty vector stream creates an instance buffer");
        frame
            .queue()
            .write_buffer(instance_buffer, 0, bytemuck::cast_slice(&packed));

        let mut ranges: Vec<(&GpuVectorMesh2d, u32, u32)> = Vec::new();
        for (offset, (mesh, _)) in ordered.iter().enumerate() {
            let offset = u32::try_from(offset).expect("total was checked as u32");
            if let Some((last_mesh, _, count)) = ranges.last_mut()
                && std::ptr::eq(*last_mesh, *mesh)
            {
                *count += 1;
            } else {
                ranges.push((*mesh, offset, 1));
            }
        }
        frame.with_surface_pass(wgpu::LoadOp::Load, |pass| {
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            pass.set_vertex_buffer(1, instance_buffer.slice(..));
            for (mesh, start, count) in &ranges {
                pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                pass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..mesh.index_count, 0, *start..*start + *count);
            }
        });
        Ok(VectorDrawStats2d {
            instances: total,
            draw_calls: u32::try_from(ranges.len()).unwrap_or(u32::MAX),
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
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("yuyib retained vector pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
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
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        Self {
            pipeline,
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
    draws: Vec<(VectorMeshId2d, VectorDraw2d)>,
    next_mesh_id: u32,
}

struct RetainedMesh {
    cpu: VectorMesh2d,
    gpu: Option<GpuVectorMesh2d>,
}

impl RetainedVectorScene2d {
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
        let mut ordered = Vec::with_capacity(self.draws.len());
        for &(id, draw) in &self.draws {
            let mesh = self
                .meshes
                .get(&id)
                .and_then(|retained| retained.gpu.as_ref())
                .ok_or(VectorSceneError2d::UnknownMesh { id })?;
            ordered.push((mesh, draw));
        }
        self.renderer
            .as_mut()
            .expect("renderer is initialized above")
            .draw_ordered(frame, camera, ordered)
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
    fn vector_draw_rejects_non_finite_values() {
        let error = validate_draw(VectorDraw2d::new().with_rotation(f32::NAN))
            .expect_err("NaN is never sent to the GPU");
        assert_eq!(error, VectorSceneError2d::NonFiniteDraw);
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
}
