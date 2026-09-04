//! Transparent Source 1 water for already-cooked static BSP surfaces.
//!
//! This module deliberately owns a separate render phase from the opaque
//! static-world renderer. It provides repeating, scrolling normal maps,
//! view-dependent tint and distance fog, but it does not claim to implement
//! Source's `_rt_WaterReflection` / `_rt_WaterRefraction` render targets.
//! Those effects require caller-owned scene-colour passes and are therefore an
//! explicit future input rather than a fake opaque base texture.

use std::{collections::HashMap, error::Error, fmt, mem::size_of, sync::Arc};

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;
use yuyib_2d::{
    Texture, TextureAlphaMode, TextureColorSpace, TextureHandle, TextureSize, TextureSizeError,
};
use yuyib_assets::Assets;
use yuyib_model::MeshPrimitive;
use yuyib_render::{RenderFrame, wgpu};
use yuyib_render_texture::{
    PreparedTextureUpload, TextureCache, TextureSamplingPreset, TextureUploadError,
};

use crate::{
    Camera3d, DepthLoad, MeshRenderError, aligned_uniform_stride, dynamic_uniform_bind_group,
    dynamic_uniform_layout, uniform_bind_group, uniform_buffer, uniform_layout,
};

/// Default maximum number of resident water batches.
pub const SOURCE1_WATER_DEFAULT_BATCH_CAPACITY: usize = 256;

/// A decoded, CPU-prepared normal map with Source-compatible repeating sampling.
pub struct Source1WaterTexture3d {
    cache_key: String,
    metadata: Texture,
    prepared: PreparedTextureUpload,
}

impl Source1WaterTexture3d {
    /// Prepares linear RGBA8 normal-map pixels and a repeating mipmapped sampler.
    ///
    /// # Errors
    ///
    /// Returns a typed error for an empty cache identity, zero dimensions or an
    /// RGBA byte count that does not match `width * height * 4`.
    pub fn rgba8_repeating(
        cache_key: impl Into<String>,
        width: u32,
        height: u32,
        pixels: Vec<u8>,
    ) -> Result<Self, Source1WaterTextureError3d> {
        let cache_key = cache_key.into();
        if cache_key.trim().is_empty() {
            return Err(Source1WaterTextureError3d::EmptyCacheKey);
        }
        let size = TextureSize::new(width, height).map_err(Source1WaterTextureError3d::Size)?;
        let metadata = Texture::new(size)
            .with_alpha_mode(TextureAlphaMode::Opaque)
            .with_color_space(TextureColorSpace::Linear);
        let mut sampler = TextureSamplingPreset::HighQuality.sampler();
        sampler.address_mode_u = wgpu::AddressMode::Repeat;
        sampler.address_mode_v = wgpu::AddressMode::Repeat;
        let prepared = PreparedTextureUpload::rgba8_owned(&metadata, pixels, sampler)
            .map_err(Source1WaterTextureError3d::Upload)?;
        Ok(Self {
            cache_key,
            metadata,
            prepared,
        })
    }

    /// Stable identity used to deduplicate one GPU normal map across materials.
    #[must_use]
    pub fn cache_key(&self) -> &str {
        &self.cache_key
    }

    /// Physical source dimensions.
    #[must_use]
    pub const fn size(&self) -> TextureSize {
        self.metadata.size()
    }
}

impl fmt::Debug for Source1WaterTexture3d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Source1WaterTexture3d")
            .field("cache_key", &self.cache_key)
            .field("size", &self.metadata.size())
            .field("resident_bytes", &self.prepared.resident_bytes())
            .field("mip_level_count", &self.prepared.mip_level_count())
            .finish()
    }
}

/// Failure while preparing a repeating water normal map.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Source1WaterTextureError3d {
    /// Cache keys are material identities and must not be empty.
    EmptyCacheKey,
    /// Texture dimensions were empty.
    Size(TextureSizeError),
    /// RGBA8 preparation failed.
    Upload(TextureUploadError),
}

impl fmt::Display for Source1WaterTextureError3d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyCacheKey => {
                formatter.write_str("water normal-map cache key must not be empty")
            }
            Self::Size(source) => write!(formatter, "invalid water normal-map size: {source}"),
            Self::Upload(source) => write!(formatter, "cannot prepare water normal map: {source}"),
        }
    }
}

impl Error for Source1WaterTextureError3d {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Size(source) => Some(source),
            Self::Upload(source) => Some(source),
            Self::EmptyCacheKey => None,
        }
    }
}

/// Supported, renderer-owned subset of a Source 1 water material.
///
/// BSP UVs remain authoritative. `normal_uv_scale` and `scroll_velocity`
/// model the normal-map transform/proxies on top of those authored UVs.
#[derive(Clone, Debug)]
pub struct Source1WaterMaterial3d {
    normal_map_frames: Arc<[Arc<Source1WaterTexture3d>]>,
    normal_frame_rate: f32,
    tint: [f32; 3],
    opacity: f32,
    fog_color: [f32; 3],
    fog_start: f32,
    fog_end: f32,
    normal_strength: f32,
    normal_uv_scale: f32,
    scroll_velocity: [f32; 2],
    fresnel_power: f32,
    reflectivity: f32,
}

impl Source1WaterMaterial3d {
    /// Creates conservative clear-water defaults around one required normal map.
    #[must_use]
    pub fn new(normal_map: Arc<Source1WaterTexture3d>) -> Self {
        Self {
            normal_map_frames: vec![normal_map].into(),
            normal_frame_rate: 0.0,
            tint: [0.10, 0.22, 0.25],
            opacity: 0.72,
            fog_color: [0.10, 0.18, 0.20],
            fog_start: 150.0,
            fog_end: 304.0,
            normal_strength: 0.35,
            normal_uv_scale: 1.0,
            scroll_velocity: [0.07, 0.0],
            fresnel_power: 5.0,
            reflectivity: 0.09,
        }
    }

    /// Replaces the still normal map with decoded VTF animation frames.
    ///
    /// Every frame remains a separately cached GPU texture. The draw pass
    /// selects one bind group from elapsed seconds without re-uploading or
    /// allocating GPU resources. Empty frames and an invalid rate are rejected
    /// by [`Self::validate`] / [`Source1WaterBatch3d::new`].
    #[must_use]
    pub fn with_normal_animation(
        mut self,
        frames: Vec<Arc<Source1WaterTexture3d>>,
        frames_per_second: f32,
    ) -> Self {
        self.normal_map_frames = frames.into();
        self.normal_frame_rate = frames_per_second;
        self
    }

    /// Applies linear RGB surface tint and straight-alpha opacity.
    #[must_use]
    pub const fn with_tint_and_opacity(mut self, tint: [f32; 3], opacity: f32) -> Self {
        self.tint = tint;
        self.opacity = opacity;
        self
    }

    /// Applies Source-style distance fog in world units.
    #[must_use]
    pub const fn with_fog(mut self, color: [f32; 3], start: f32, end: f32) -> Self {
        self.fog_color = color;
        self.fog_start = start;
        self.fog_end = end;
        self
    }

    /// Scales normal perturbation; zero retains the geometric surface normal.
    #[must_use]
    pub const fn with_normal_strength(mut self, strength: f32) -> Self {
        self.normal_strength = strength;
        self
    }

    /// Scales authored BSP UVs before normal-map sampling.
    #[must_use]
    pub const fn with_normal_uv_scale(mut self, scale: f32) -> Self {
        self.normal_uv_scale = scale;
        self
    }

    /// Sets UV motion per second. A VMT angle/rate proxy should be resolved to
    /// this two-dimensional velocity by the importer.
    #[must_use]
    pub const fn with_scroll_velocity(mut self, velocity: [f32; 2]) -> Self {
        self.scroll_velocity = velocity;
        self
    }

    /// Sets view-angle response without claiming a scene reflection texture.
    #[must_use]
    pub const fn with_fresnel(mut self, power: f32, reflectivity: f32) -> Self {
        self.fresnel_power = power;
        self.reflectivity = reflectivity;
        self
    }

    /// Required repeating normal-map frames in source order.
    #[must_use]
    pub fn normal_map_frames(&self) -> &[Arc<Source1WaterTexture3d>] {
        &self.normal_map_frames
    }

    /// Animation rate. Zero is valid for a single still frame.
    #[must_use]
    pub const fn normal_frame_rate(&self) -> f32 {
        self.normal_frame_rate
    }

    /// Linear surface tint.
    #[must_use]
    pub const fn tint(&self) -> [f32; 3] {
        self.tint
    }

    /// Straight-alpha coverage.
    #[must_use]
    pub const fn opacity(&self) -> f32 {
        self.opacity
    }

    /// Fog colour, start and end in world units.
    #[must_use]
    pub const fn fog(&self) -> ([f32; 3], f32, f32) {
        (self.fog_color, self.fog_start, self.fog_end)
    }

    /// Normal-map UV scale.
    #[must_use]
    pub const fn normal_uv_scale(&self) -> f32 {
        self.normal_uv_scale
    }

    /// UV motion per second.
    #[must_use]
    pub const fn scroll_velocity(&self) -> [f32; 2] {
        self.scroll_velocity
    }

    /// Validates all GPU-facing scalar and vector values.
    pub fn validate(&self) -> Result<(), Source1WaterMaterialError3d> {
        if self.normal_map_frames.is_empty() {
            return Err(Source1WaterMaterialError3d::EmptyNormalMapAnimation);
        }
        validate_finite3("tint", self.tint)?;
        validate_finite3("fog color", self.fog_color)?;
        validate_finite2("scroll velocity", self.scroll_velocity)?;
        validate_finite("opacity", self.opacity)?;
        validate_finite("fog start", self.fog_start)?;
        validate_finite("fog end", self.fog_end)?;
        validate_finite("normal strength", self.normal_strength)?;
        validate_finite("normal UV scale", self.normal_uv_scale)?;
        validate_finite("Fresnel power", self.fresnel_power)?;
        validate_finite("reflectivity", self.reflectivity)?;
        validate_finite("normal-map frame rate", self.normal_frame_rate)?;
        if self.tint.iter().any(|component| *component < 0.0) {
            return Err(Source1WaterMaterialError3d::Negative("tint"));
        }
        if self.fog_color.iter().any(|component| *component < 0.0) {
            return Err(Source1WaterMaterialError3d::Negative("fog color"));
        }
        if !(0.0..=1.0).contains(&self.opacity) {
            return Err(Source1WaterMaterialError3d::UnitRange("opacity"));
        }
        if !(0.0..=1.0).contains(&self.reflectivity) {
            return Err(Source1WaterMaterialError3d::UnitRange("reflectivity"));
        }
        if self.fog_start < 0.0 || self.fog_end <= self.fog_start {
            return Err(Source1WaterMaterialError3d::InvalidFogRange);
        }
        if self.normal_strength < 0.0 {
            return Err(Source1WaterMaterialError3d::Negative("normal strength"));
        }
        if self.normal_uv_scale <= 0.0 {
            return Err(Source1WaterMaterialError3d::NonPositive("normal UV scale"));
        }
        if self.fresnel_power <= 0.0 {
            return Err(Source1WaterMaterialError3d::NonPositive("Fresnel power"));
        }
        if self.normal_frame_rate < 0.0 {
            return Err(Source1WaterMaterialError3d::Negative(
                "normal-map frame rate",
            ));
        }
        if self.normal_map_frames.len() > 1 && self.normal_frame_rate <= 0.0 {
            return Err(Source1WaterMaterialError3d::NonPositive(
                "animated normal-map frame rate",
            ));
        }
        Ok(())
    }

    fn uniform(&self) -> WaterDrawUniform {
        WaterDrawUniform {
            tint_opacity: [self.tint[0], self.tint[1], self.tint[2], self.opacity],
            fog_color_start: [
                self.fog_color[0],
                self.fog_color[1],
                self.fog_color[2],
                self.fog_start,
            ],
            fog_end_normal_fresnel_reflectivity: [
                self.fog_end,
                self.normal_strength,
                self.fresnel_power,
                self.reflectivity,
            ],
            uv_scale_scroll: [
                self.normal_uv_scale,
                self.scroll_velocity[0],
                self.scroll_velocity[1],
                0.0,
            ],
        }
    }
}

/// Invalid Source water material state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Source1WaterMaterialError3d {
    /// At least one decoded normal-map frame is required.
    EmptyNormalMapAnimation,
    /// A named value is NaN or infinite.
    NonFinite(&'static str),
    /// A named value must not be negative.
    Negative(&'static str),
    /// A named value must be greater than zero.
    NonPositive(&'static str),
    /// A named value must lie in `0..=1`.
    UnitRange(&'static str),
    /// Fog must satisfy `0 <= start < end`.
    InvalidFogRange,
}

impl fmt::Display for Source1WaterMaterialError3d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyNormalMapAnimation => {
                formatter.write_str("water material normal-map animation must not be empty")
            }
            Self::NonFinite(name) => write!(formatter, "water material {name} must be finite"),
            Self::Negative(name) => write!(formatter, "water material {name} must not be negative"),
            Self::NonPositive(name) => write!(formatter, "water material {name} must be positive"),
            Self::UnitRange(name) => write!(formatter, "water material {name} must be in 0..=1"),
            Self::InvalidFogRange => formatter.write_str("water fog must satisfy 0 <= start < end"),
        }
    }
}

impl Error for Source1WaterMaterialError3d {}

/// One static, material-compatible water mesh.
#[derive(Clone, Debug)]
pub struct Source1WaterBatch3d {
    primitive: MeshPrimitive,
    material: Source1WaterMaterial3d,
    center: [f32; 3],
}

impl Source1WaterBatch3d {
    /// Validates required positions, normals and UV0 before accepting a batch.
    ///
    /// # Errors
    ///
    /// Returns a typed geometry or material error. No missing stream is
    /// synthesized because doing so would hide an importer defect.
    pub fn new(
        primitive: MeshPrimitive,
        material: Source1WaterMaterial3d,
    ) -> Result<Self, Source1WaterBatchError3d> {
        material
            .validate()
            .map_err(Source1WaterBatchError3d::Material)?;
        let normals = primitive
            .normals()
            .ok_or(Source1WaterBatchError3d::MissingNormals)?;
        let tex_coords = primitive
            .tex_coords_0()
            .ok_or(Source1WaterBatchError3d::MissingTexCoords0)?;
        u32::try_from(primitive.indices().len()).map_err(|_| {
            Source1WaterBatchError3d::TooManyIndices {
                actual: primitive.indices().len(),
            }
        })?;

        let mut minimum = [f32::INFINITY; 3];
        let mut maximum = [f32::NEG_INFINITY; 3];
        for (index, ((position, normal), tex_coord)) in primitive
            .positions()
            .iter()
            .zip(normals)
            .zip(tex_coords)
            .enumerate()
        {
            if !position.iter().all(|value| value.is_finite()) {
                return Err(Source1WaterBatchError3d::NonFinitePosition { index });
            }
            if !normal.iter().all(|value| value.is_finite()) {
                return Err(Source1WaterBatchError3d::NonFiniteNormal { index });
            }
            if normal.iter().map(|value| value * value).sum::<f32>() <= f32::EPSILON {
                return Err(Source1WaterBatchError3d::DegenerateNormal { index });
            }
            if !tex_coord.iter().all(|value| value.is_finite()) {
                return Err(Source1WaterBatchError3d::NonFiniteTexCoords0 { index });
            }
            for axis in 0..3 {
                minimum[axis] = minimum[axis].min(position[axis]);
                maximum[axis] = maximum[axis].max(position[axis]);
            }
        }
        let center = [
            minimum[0] + (maximum[0] - minimum[0]) * 0.5,
            minimum[1] + (maximum[1] - minimum[1]) * 0.5,
            minimum[2] + (maximum[2] - minimum[2]) * 0.5,
        ];
        Ok(Self {
            primitive,
            material,
            center,
        })
    }

    /// Indexed triangle-list geometry.
    #[must_use]
    pub const fn primitive(&self) -> &MeshPrimitive {
        &self.primitive
    }

    /// Water material parameters.
    #[must_use]
    pub const fn material(&self) -> &Source1WaterMaterial3d {
        &self.material
    }

    /// World-space AABB centre used for transparent back-to-front ordering.
    #[must_use]
    pub const fn center(&self) -> [f32; 3] {
        self.center
    }
}

/// Failure while constructing one water batch.
#[derive(Debug)]
pub enum Source1WaterBatchError3d {
    /// Material parameters are invalid.
    Material(Source1WaterMaterialError3d),
    /// Geometric normals are required.
    MissingNormals,
    /// Authored BSP UV0 is required.
    MissingTexCoords0,
    /// An indexed draw count does not fit WGPU's `u32` contract.
    TooManyIndices {
        /// Observed index count.
        actual: usize,
    },
    /// A position contains NaN or infinity.
    NonFinitePosition {
        /// Vertex index.
        index: usize,
    },
    /// A normal contains NaN or infinity.
    NonFiniteNormal {
        /// Vertex index.
        index: usize,
    },
    /// A normal has zero length.
    DegenerateNormal {
        /// Vertex index.
        index: usize,
    },
    /// A UV contains NaN or infinity.
    NonFiniteTexCoords0 {
        /// Vertex index.
        index: usize,
    },
}

impl fmt::Display for Source1WaterBatchError3d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Material(source) => write!(formatter, "invalid water material: {source}"),
            Self::MissingNormals => formatter.write_str("water mesh has no normal stream"),
            Self::MissingTexCoords0 => formatter.write_str("water mesh has no UV0 stream"),
            Self::TooManyIndices { actual } => {
                write!(
                    formatter,
                    "water mesh has {actual} indices; maximum is u32::MAX"
                )
            }
            Self::NonFinitePosition { index } => {
                write!(formatter, "water vertex {index} has a non-finite position")
            }
            Self::NonFiniteNormal { index } => {
                write!(formatter, "water vertex {index} has a non-finite normal")
            }
            Self::DegenerateNormal { index } => {
                write!(formatter, "water vertex {index} has a degenerate normal")
            }
            Self::NonFiniteTexCoords0 { index } => {
                write!(formatter, "water vertex {index} has non-finite UV0")
            }
        }
    }
}

impl Error for Source1WaterBatchError3d {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Material(source) => Some(source),
            _ => None,
        }
    }
}

/// CPU resource ceilings applied before any GPU allocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Source1WaterLimits3d {
    /// Maximum material/mesh batches.
    pub max_batches: usize,
    /// Maximum aggregate vertex count.
    pub max_vertices: usize,
    /// Maximum aggregate index count.
    pub max_indices: usize,
    /// Maximum unique normal-map identities.
    pub max_unique_textures: usize,
}

impl Default for Source1WaterLimits3d {
    fn default() -> Self {
        Self {
            max_batches: SOURCE1_WATER_DEFAULT_BATCH_CAPACITY,
            max_vertices: 4_000_000,
            max_indices: 12_000_000,
            max_unique_textures: 256,
        }
    }
}

/// Aggregate CPU cooking/upload statistics.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Source1WaterBuildStats3d {
    /// Number of batches.
    pub batches: usize,
    /// Total vertices.
    pub vertices: usize,
    /// Total triangle-list indices.
    pub indices: usize,
    /// Total triangles.
    pub triangles: usize,
    /// Unique normal maps by cache key.
    pub unique_textures: usize,
}

/// Bounded static water data ready for one explicit GPU upload.
#[derive(Clone, Debug)]
pub struct Source1WaterWorld3d {
    batches: Vec<Source1WaterBatch3d>,
    stats: Source1WaterBuildStats3d,
}

impl Source1WaterWorld3d {
    /// Accepts already material-grouped water batches and enforces aggregate limits.
    pub fn new(
        batches: Vec<Source1WaterBatch3d>,
        limits: Source1WaterLimits3d,
    ) -> Result<Self, Source1WaterWorldBuildError3d> {
        if batches.len() > limits.max_batches {
            return Err(Source1WaterWorldBuildError3d::TooManyBatches {
                actual: batches.len(),
                maximum: limits.max_batches,
            });
        }
        let mut vertices = 0_usize;
        let mut indices = 0_usize;
        let mut textures = HashMap::<&str, ()>::new();
        for batch in &batches {
            vertices = vertices
                .checked_add(batch.primitive.positions().len())
                .ok_or(Source1WaterWorldBuildError3d::CountOverflow)?;
            indices = indices
                .checked_add(batch.primitive.indices().len())
                .ok_or(Source1WaterWorldBuildError3d::CountOverflow)?;
            for frame in batch.material.normal_map_frames() {
                textures.insert(frame.cache_key(), ());
            }
        }
        if vertices > limits.max_vertices {
            return Err(Source1WaterWorldBuildError3d::TooManyVertices {
                actual: vertices,
                maximum: limits.max_vertices,
            });
        }
        if indices > limits.max_indices {
            return Err(Source1WaterWorldBuildError3d::TooManyIndices {
                actual: indices,
                maximum: limits.max_indices,
            });
        }
        if textures.len() > limits.max_unique_textures {
            return Err(Source1WaterWorldBuildError3d::TooManyUniqueTextures {
                actual: textures.len(),
                maximum: limits.max_unique_textures,
            });
        }
        let stats = Source1WaterBuildStats3d {
            batches: batches.len(),
            vertices,
            indices,
            triangles: indices / 3,
            unique_textures: textures.len(),
        };
        Ok(Self { batches, stats })
    }

    /// Validated batches in importer order.
    #[must_use]
    pub fn batches(&self) -> &[Source1WaterBatch3d] {
        &self.batches
    }

    /// Aggregate counts.
    #[must_use]
    pub const fn stats(&self) -> Source1WaterBuildStats3d {
        self.stats
    }
}

/// Aggregate resource-limit failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Source1WaterWorldBuildError3d {
    /// Batch ceiling exceeded.
    TooManyBatches {
        /// Observed batch count.
        actual: usize,
        /// Configured maximum.
        maximum: usize,
    },
    /// Vertex ceiling exceeded.
    TooManyVertices {
        /// Observed vertex count.
        actual: usize,
        /// Configured maximum.
        maximum: usize,
    },
    /// Index ceiling exceeded.
    TooManyIndices {
        /// Observed index count.
        actual: usize,
        /// Configured maximum.
        maximum: usize,
    },
    /// Normal-map ceiling exceeded.
    TooManyUniqueTextures {
        /// Observed unique normal-map count.
        actual: usize,
        /// Configured maximum.
        maximum: usize,
    },
    /// Aggregate arithmetic overflowed.
    CountOverflow,
}

impl fmt::Display for Source1WaterWorldBuildError3d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyBatches { actual, maximum } => {
                write!(
                    formatter,
                    "water world has {actual} batches; maximum is {maximum}"
                )
            }
            Self::TooManyVertices { actual, maximum } => {
                write!(
                    formatter,
                    "water world has {actual} vertices; maximum is {maximum}"
                )
            }
            Self::TooManyIndices { actual, maximum } => {
                write!(
                    formatter,
                    "water world has {actual} indices; maximum is {maximum}"
                )
            }
            Self::TooManyUniqueTextures { actual, maximum } => write!(
                formatter,
                "water world has {actual} unique normal maps; maximum is {maximum}"
            ),
            Self::CountOverflow => formatter.write_str("water world aggregate count overflowed"),
        }
    }
}

impl Error for Source1WaterWorldBuildError3d {}

/// Counts recorded by one transparent water pass.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Source1WaterDrawStats3d {
    /// Resident batches submitted.
    pub batches: usize,
    /// Indexed triangles submitted.
    pub triangles: u64,
    /// Draw submissions recorded.
    pub draw_calls: usize,
}

/// Counts produced by a successful transactional GPU upload.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Source1WaterUploadStats3d {
    /// Resident batches.
    pub batches: usize,
    /// Unique resident normal maps.
    pub unique_textures: usize,
    /// Aggregate resident triangles.
    pub triangles: usize,
}

struct GpuWaterBatch {
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
    center: [f32; 3],
    normal_frame_bind_groups: Vec<Arc<wgpu::BindGroup>>,
    normal_frame_rate: f32,
}

/// Separate transparent GPU phase for static Source water surfaces.
pub struct Source1WaterRenderer3d {
    pipeline: wgpu::RenderPipeline,
    camera_buffer: wgpu::Buffer,
    draw_buffer: wgpu::Buffer,
    draw_uniform_stride: u64,
    batch_capacity: usize,
    camera_bind_group: wgpu::BindGroup,
    draw_bind_group: wgpu::BindGroup,
    texture_layout: wgpu::BindGroupLayout,
    texture_assets: Assets<Texture>,
    texture_cache: TextureCache,
    texture_handles: HashMap<String, TextureHandle>,
    batches: Vec<GpuWaterBatch>,
}

impl Source1WaterRenderer3d {
    /// Builds a renderer with [`SOURCE1_WATER_DEFAULT_BATCH_CAPACITY`].
    pub fn new_for_frame(
        frame: &RenderFrame<'_>,
    ) -> Result<Self, Source1WaterRendererCreateError3d> {
        Self::new_for_frame_with_batch_capacity(frame, SOURCE1_WATER_DEFAULT_BATCH_CAPACITY)
    }

    /// Builds a renderer with a hard resident batch ceiling.
    pub fn new_for_frame_with_batch_capacity(
        frame: &RenderFrame<'_>,
        batch_capacity: usize,
    ) -> Result<Self, Source1WaterRendererCreateError3d> {
        Self::create(
            frame.device(),
            frame.surface_format(),
            frame.depth_format(),
            batch_capacity,
        )
    }

    /// Maximum number of resident draw batches.
    #[must_use]
    pub const fn batch_capacity(&self) -> usize {
        self.batch_capacity
    }

    /// Number of unique normal maps in the current GPU cache.
    #[must_use]
    pub fn texture_count(&self) -> usize {
        self.texture_cache.len()
    }

    /// Number of resident batches.
    #[must_use]
    pub fn batch_count(&self) -> usize {
        self.batches.len()
    }

    /// Replaces the resident world only after every fallible upload succeeds.
    pub fn upload_for_frame(
        &mut self,
        frame: &RenderFrame<'_>,
        world: &Source1WaterWorld3d,
    ) -> Result<Source1WaterUploadStats3d, Source1WaterUploadError3d> {
        if world.batches.len() > self.batch_capacity {
            return Err(Source1WaterUploadError3d::BatchCapacityExceeded {
                actual: world.batches.len(),
                maximum: self.batch_capacity,
            });
        }

        let mut texture_assets = Assets::new();
        let mut texture_cache = TextureCache::new();
        let mut texture_handles = HashMap::<String, TextureHandle>::new();
        let mut material_bind_groups = HashMap::<String, Arc<wgpu::BindGroup>>::new();
        let mut batches = Vec::with_capacity(world.batches.len());
        let mut uniforms = Vec::with_capacity(world.batches.len());

        for (batch_index, source) in world.batches.iter().enumerate() {
            let mut normal_frame_bind_groups =
                Vec::with_capacity(source.material.normal_map_frames().len());
            for texture in source.material.normal_map_frames() {
                let key = texture.cache_key().to_owned();
                let handle = if let Some(handle) = texture_handles.get(&key).copied() {
                    handle
                } else {
                    let handle = texture_assets.insert(texture.metadata.clone());
                    texture_cache
                        .upsert_prepared_for_frame(frame, handle, &texture.prepared)
                        .map_err(|source| Source1WaterUploadError3d::Texture {
                            cache_key: key.clone(),
                            source,
                        })?;
                    texture_handles.insert(key.clone(), handle);
                    handle
                };
                let bind_group = if let Some(bind_group) = material_bind_groups.get(&key) {
                    Arc::clone(bind_group)
                } else {
                    let gpu = texture_cache.get(handle).ok_or_else(|| {
                        Source1WaterUploadError3d::MissingCachedTexture {
                            batch: batch_index,
                            cache_key: key.clone(),
                        }
                    })?;
                    let bind_group = Arc::new(frame.device().create_bind_group(
                        &wgpu::BindGroupDescriptor {
                            label: Some("yuyib Source 1 water normal-map bind group"),
                            layout: &self.texture_layout,
                            entries: &[
                                wgpu::BindGroupEntry {
                                    binding: 0,
                                    resource: wgpu::BindingResource::TextureView(gpu.view()),
                                },
                                wgpu::BindGroupEntry {
                                    binding: 1,
                                    resource: wgpu::BindingResource::Sampler(gpu.sampler()),
                                },
                            ],
                        },
                    ));
                    material_bind_groups.insert(key, Arc::clone(&bind_group));
                    bind_group
                };
                normal_frame_bind_groups.push(bind_group);
            }
            let vertices = water_vertices(source);
            let vertex_buffer =
                frame
                    .device()
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("yuyib Source 1 water vertices"),
                        contents: bytemuck::cast_slice(&vertices),
                        usage: wgpu::BufferUsages::VERTEX,
                    });
            let index_buffer =
                frame
                    .device()
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("yuyib Source 1 water indices"),
                        contents: bytemuck::cast_slice(source.primitive.indices()),
                        usage: wgpu::BufferUsages::INDEX,
                    });
            let index_count = u32::try_from(source.primitive.indices().len())
                .expect("Source1WaterBatch3d proves the index count fits u32");
            batches.push(GpuWaterBatch {
                vertex_buffer,
                index_buffer,
                index_count,
                center: source.center,
                normal_frame_bind_groups,
                normal_frame_rate: source.material.normal_frame_rate(),
            });
            uniforms.push(source.material.uniform());
        }

        // All typed failure points are complete; updating the shared draw
        // buffer cannot corrupt the previous resident world on an upload error.
        for (index, uniform) in uniforms.iter().enumerate() {
            let offset = u64::try_from(index)
                .expect("batch capacity fits u64")
                .saturating_mul(self.draw_uniform_stride);
            frame
                .queue()
                .write_buffer(&self.draw_buffer, offset, bytemuck::bytes_of(uniform));
        }

        let stats = Source1WaterUploadStats3d {
            batches: batches.len(),
            unique_textures: texture_cache.len(),
            triangles: world.stats.triangles,
        };
        self.texture_assets = texture_assets;
        self.texture_cache = texture_cache;
        self.texture_handles = texture_handles;
        self.batches = batches;
        Ok(stats)
    }

    /// Draws after an opaque phase, preserving its depth attachment.
    pub fn draw_for_frame(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        time_seconds: f32,
    ) -> Result<Source1WaterDrawStats3d, Source1WaterRenderError3d> {
        self.draw_for_frame_with_depth_load(frame, camera, time_seconds, DepthLoad::Load)
    }

    /// Draws with explicit initial depth behaviour.
    ///
    /// The pipeline tests depth but never writes it. Batches are sorted from
    /// far to near using their world-space AABB centres.
    pub fn draw_for_frame_with_depth_load(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        time_seconds: f32,
        depth_load: DepthLoad,
    ) -> Result<Source1WaterDrawStats3d, Source1WaterRenderError3d> {
        if !time_seconds.is_finite() {
            return Err(Source1WaterRenderError3d::NonFiniteTime);
        }
        if self.batches.is_empty() {
            return Ok(Source1WaterDrawStats3d::default());
        }
        let view_projection = camera
            .view_projection(frame.draw_size())
            .map_err(Source1WaterRenderError3d::Camera)?;
        let camera_uniform = WaterCameraUniform {
            view_projection,
            position_time: [
                camera.position[0],
                camera.position[1],
                camera.position[2],
                time_seconds,
            ],
        };
        frame
            .queue()
            .write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&camera_uniform));
        let order = sorted_back_to_front(&self.batches, camera.position);
        frame.with_surface_pass_with_depth(wgpu::LoadOp::Load, depth_load.operation(), |pass| {
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            for &batch_index in &order {
                let batch = &self.batches[batch_index];
                let offset = u64::try_from(batch_index)
                    .expect("batch capacity fits u64")
                    .saturating_mul(self.draw_uniform_stride);
                let offset = u32::try_from(offset)
                    .expect("renderer creation bounds every dynamic uniform offset");
                pass.set_bind_group(1, &self.draw_bind_group, &[offset]);
                let frame_index = animated_frame_index(
                    time_seconds,
                    batch.normal_frame_rate,
                    batch.normal_frame_bind_groups.len(),
                );
                pass.set_bind_group(2, batch.normal_frame_bind_groups[frame_index].as_ref(), &[]);
                pass.set_vertex_buffer(0, batch.vertex_buffer.slice(..));
                pass.set_index_buffer(batch.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..batch.index_count, 0, 0..1);
            }
        });
        Ok(Source1WaterDrawStats3d {
            batches: self.batches.len(),
            triangles: self
                .batches
                .iter()
                .map(|batch| u64::from(batch.index_count / 3))
                .sum(),
            draw_calls: self.batches.len(),
        })
    }

    fn create(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        depth_format: wgpu::TextureFormat,
        batch_capacity: usize,
    ) -> Result<Self, Source1WaterRendererCreateError3d> {
        if batch_capacity == 0 {
            return Err(Source1WaterRendererCreateError3d::ZeroBatchCapacity);
        }
        let draw_uniform_stride = aligned_uniform_stride(
            device.limits().min_uniform_buffer_offset_alignment,
            size_of::<WaterDrawUniform>() as u64,
        );
        let capacity_u64 = u64::try_from(batch_capacity)
            .map_err(|_| Source1WaterRendererCreateError3d::BatchCapacityTooLarge)?;
        let draw_buffer_size = draw_uniform_stride
            .checked_mul(capacity_u64)
            .ok_or(Source1WaterRendererCreateError3d::BatchCapacityTooLarge)?;
        let final_offset = draw_uniform_stride
            .checked_mul(capacity_u64 - 1)
            .ok_or(Source1WaterRendererCreateError3d::BatchCapacityTooLarge)?;
        if final_offset > u64::from(u32::MAX) || draw_buffer_size > device.limits().max_buffer_size
        {
            return Err(Source1WaterRendererCreateError3d::BatchCapacityTooLarge);
        }

        let camera_layout = uniform_layout(
            device,
            "yuyib Source 1 water camera layout",
            wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
        );
        let draw_layout = dynamic_uniform_layout(
            device,
            "yuyib Source 1 water draw layout",
            wgpu::ShaderStages::FRAGMENT,
            size_of::<WaterDrawUniform>() as u64,
        );
        let texture_layout = water_texture_layout(device);
        let camera_buffer = uniform_buffer(
            device,
            "yuyib Source 1 water camera",
            size_of::<WaterCameraUniform>() as u64,
        );
        let draw_buffer = uniform_buffer(device, "yuyib Source 1 water draws", draw_buffer_size);
        let camera_bind_group = uniform_bind_group(
            device,
            "yuyib Source 1 water camera bind group",
            &camera_layout,
            &camera_buffer,
        );
        let draw_bind_group = dynamic_uniform_bind_group(
            device,
            "yuyib Source 1 water draw bind group",
            &draw_layout,
            &draw_buffer,
            size_of::<WaterDrawUniform>() as u64,
        );
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("yuyib Source 1 water WGSL"),
            source: wgpu::ShaderSource::Wgsl(SOURCE1_WATER_WGSL.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("yuyib Source 1 water pipeline layout"),
            bind_group_layouts: &[
                Some(&camera_layout),
                Some(&draw_layout),
                Some(&texture_layout),
            ],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("yuyib Source 1 water pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(WATER_VERTEX_LAYOUT)],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: depth_format,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
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
        Ok(Self {
            pipeline,
            camera_buffer,
            draw_buffer,
            draw_uniform_stride,
            batch_capacity,
            camera_bind_group,
            draw_bind_group,
            texture_layout,
            texture_assets: Assets::new(),
            texture_cache: TextureCache::new(),
            texture_handles: HashMap::new(),
            batches: Vec::new(),
        })
    }
}

/// Renderer allocation/configuration failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Source1WaterRendererCreateError3d {
    /// At least one batch slot is required for a valid uniform buffer.
    ZeroBatchCapacity,
    /// Dynamic offsets or the selected GPU's maximum buffer size cannot
    /// represent the requested capacity.
    BatchCapacityTooLarge,
}

impl fmt::Display for Source1WaterRendererCreateError3d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroBatchCapacity => {
                formatter.write_str("Source 1 water batch capacity must be non-zero")
            }
            Self::BatchCapacityTooLarge => formatter.write_str(
                "Source 1 water batch capacity exceeds dynamic-offset or GPU buffer limits",
            ),
        }
    }
}

impl Error for Source1WaterRendererCreateError3d {}

/// Failure while replacing resident water GPU data.
#[derive(Debug)]
pub enum Source1WaterUploadError3d {
    /// CPU world cannot fit this renderer's bounded uniform storage.
    BatchCapacityExceeded {
        /// Observed batch count.
        actual: usize,
        /// Renderer capacity.
        maximum: usize,
    },
    /// Normal-map publication failed.
    Texture {
        /// Stable normal-map identity.
        cache_key: String,
        /// GPU publication failure.
        source: TextureUploadError,
    },
    /// Internal cache publication was inconsistent.
    MissingCachedTexture {
        /// Source batch index.
        batch: usize,
        /// Stable normal-map identity.
        cache_key: String,
    },
}

impl fmt::Display for Source1WaterUploadError3d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BatchCapacityExceeded { actual, maximum } => write!(
                formatter,
                "water upload has {actual} batches; renderer capacity is {maximum}"
            ),
            Self::Texture { cache_key, source } => {
                write!(
                    formatter,
                    "cannot upload water normal map {cache_key:?}: {source}"
                )
            }
            Self::MissingCachedTexture { batch, cache_key } => write!(
                formatter,
                "water batch {batch} normal map {cache_key:?} was uploaded but is absent from the GPU cache"
            ),
        }
    }
}

impl Error for Source1WaterUploadError3d {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Texture { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Failure while recording a transparent water pass.
#[derive(Debug)]
pub enum Source1WaterRenderError3d {
    /// Camera projection is invalid for the current target.
    Camera(MeshRenderError),
    /// Animation time is NaN or infinite.
    NonFiniteTime,
}

impl fmt::Display for Source1WaterRenderError3d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Camera(source) => write!(formatter, "cannot draw Source 1 water: {source}"),
            Self::NonFiniteTime => formatter.write_str("water animation time must be finite"),
        }
    }
}

impl Error for Source1WaterRenderError3d {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Camera(source) => Some(source),
            Self::NonFiniteTime => None,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct WaterVertex {
    position: [f32; 3],
    normal: [f32; 3],
    tex_coord: [f32; 2],
}

const WATER_VERTEX_LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
    array_stride: size_of::<WaterVertex>() as u64,
    step_mode: wgpu::VertexStepMode::Vertex,
    attributes: &[
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x3,
            offset: 0,
            shader_location: 0,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x3,
            offset: size_of::<[f32; 3]>() as u64,
            shader_location: 1,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x2,
            offset: size_of::<[f32; 3]>() as u64 * 2,
            shader_location: 2,
        },
    ],
};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct WaterCameraUniform {
    view_projection: [f32; 16],
    position_time: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct WaterDrawUniform {
    tint_opacity: [f32; 4],
    fog_color_start: [f32; 4],
    fog_end_normal_fresnel_reflectivity: [f32; 4],
    uv_scale_scroll: [f32; 4],
}

fn water_vertices(batch: &Source1WaterBatch3d) -> Vec<WaterVertex> {
    batch
        .primitive
        .positions()
        .iter()
        .copied()
        .zip(
            batch
                .primitive
                .normals()
                .expect("Source1WaterBatch3d requires normals")
                .iter()
                .copied(),
        )
        .zip(
            batch
                .primitive
                .tex_coords_0()
                .expect("Source1WaterBatch3d requires UV0")
                .iter()
                .copied(),
        )
        .map(|((position, normal), tex_coord)| WaterVertex {
            position,
            normal,
            tex_coord,
        })
        .collect()
}

fn sorted_back_to_front(batches: &[GpuWaterBatch], camera: [f32; 3]) -> Vec<usize> {
    let mut order: Vec<_> = (0..batches.len()).collect();
    order.sort_by(|left, right| {
        let left_distance = squared_distance(batches[*left].center, camera);
        let right_distance = squared_distance(batches[*right].center, camera);
        right_distance
            .total_cmp(&left_distance)
            .then_with(|| left.cmp(right))
    });
    order
}

fn squared_distance(point: [f32; 3], camera: [f32; 3]) -> f32 {
    let delta = [
        point[0] - camera[0],
        point[1] - camera[1],
        point[2] - camera[2],
    ];
    delta.iter().map(|value| value * value).sum()
}

fn animated_frame_index(time_seconds: f32, frames_per_second: f32, frame_count: usize) -> usize {
    if frame_count <= 1 || frames_per_second <= 0.0 {
        return 0;
    }
    let phase = f64::from(time_seconds) * f64::from(frames_per_second);
    let nearest = phase.round();
    // f32 cannot represent common frame durations such as 1/30 exactly. Snap
    // only values already within a tiny boundary tolerance; all interior
    // values retain ordinary floor-based frame selection.
    let frame = if (phase - nearest).abs() <= 1.0e-6 {
        nearest
    } else {
        phase.floor()
    };
    frame.rem_euclid(frame_count as f64) as usize
}

fn water_texture_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("yuyib Source 1 water normal-map layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    })
}

fn validate_finite(name: &'static str, value: f32) -> Result<(), Source1WaterMaterialError3d> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(Source1WaterMaterialError3d::NonFinite(name))
    }
}

fn validate_finite2(
    name: &'static str,
    value: [f32; 2],
) -> Result<(), Source1WaterMaterialError3d> {
    if value.iter().all(|component| component.is_finite()) {
        Ok(())
    } else {
        Err(Source1WaterMaterialError3d::NonFinite(name))
    }
}

fn validate_finite3(
    name: &'static str,
    value: [f32; 3],
) -> Result<(), Source1WaterMaterialError3d> {
    if value.iter().all(|component| component.is_finite()) {
        Ok(())
    } else {
        Err(Source1WaterMaterialError3d::NonFinite(name))
    }
}

const SOURCE1_WATER_WGSL: &str = r#"
struct Camera {
    view_projection: mat4x4<f32>,
    position_time: vec4<f32>,
};
struct Draw {
    tint_opacity: vec4<f32>,
    fog_color_start: vec4<f32>,
    fog_end_normal_fresnel_reflectivity: vec4<f32>,
    uv_scale_scroll: vec4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var<uniform> draw: Draw;
@group(2) @binding(0) var normal_texture: texture_2d<f32>;
@group(2) @binding(1) var normal_sampler: sampler;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) tex_coord: vec2<f32>,
};
struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) tex_coord: vec2<f32>,
};

@vertex fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    output.clip_position = camera.view_projection * vec4<f32>(input.position, 1.0);
    output.world_position = input.position;
    output.normal = input.normal;
    output.tex_coord = input.tex_coord;
    return output;
}

@fragment fn fs_main(
    input: VertexOutput,
    @builtin(front_facing) front_facing: bool,
) -> @location(0) vec4<f32> {
    let time = camera.position_time.w;
    let scale = draw.uv_scale_scroll.x;
    let scroll = draw.uv_scale_scroll.yz;
    let uv_a = input.tex_coord * scale + scroll * time;
    let rotated_scroll = vec2<f32>(-scroll.y, scroll.x);
    let uv_b = input.tex_coord * (scale * 0.731) + rotated_scroll * (time * 0.613);
    let sample_a = textureSample(normal_texture, normal_sampler, uv_a).xyz * 2.0 - 1.0;
    let sample_b = textureSample(normal_texture, normal_sampler, uv_b).xyz * 2.0 - 1.0;
    let mapped = normalize(vec3<f32>(sample_a.xy + sample_b.xy * 0.55, max(sample_a.z, 0.08)));

    let geometric = select(-normalize(input.normal), normalize(input.normal), front_facing);
    let reference_axis = select(
        vec3<f32>(0.0, 1.0, 0.0),
        vec3<f32>(1.0, 0.0, 0.0),
        abs(geometric.y) > 0.98,
    );
    let tangent = normalize(cross(reference_axis, geometric));
    let bitangent = normalize(cross(geometric, tangent));
    let strength = draw.fog_end_normal_fresnel_reflectivity.y;
    let normal = normalize(
        geometric * mapped.z + (tangent * mapped.x + bitangent * mapped.y) * strength,
    );
    let to_camera = camera.position_time.xyz - input.world_position;
    let distance_to_camera = length(to_camera);
    let view_direction = normalize(to_camera);
    let view_dot = clamp(dot(normal, view_direction), 0.0, 1.0);
    let fresnel_power = draw.fog_end_normal_fresnel_reflectivity.z;
    let reflectivity = draw.fog_end_normal_fresnel_reflectivity.w;
    let fresnel = reflectivity + (1.0 - reflectivity) * pow(1.0 - view_dot, fresnel_power);

    // View-dependent pale tint is intentional. No scene-colour texture is
    // sampled here, so this is not presented as reflection or refraction.
    let facing_light = 0.72 + max(normal.y, 0.0) * 0.28;
    let base = draw.tint_opacity.rgb * facing_light;
    let surface = mix(base, vec3<f32>(0.82, 0.92, 0.95), fresnel * 0.38);
    let fog_start = draw.fog_color_start.w;
    let fog_end = draw.fog_end_normal_fresnel_reflectivity.x;
    let fog_amount = smoothstep(fog_start, fog_end, distance_to_camera);
    let color = mix(surface, draw.fog_color_start.rgb, fog_amount);
    let alpha = clamp(draw.tint_opacity.a + fresnel * 0.12, 0.0, 1.0);
    return vec4<f32>(color, alpha);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn texture(key: &str) -> Arc<Source1WaterTexture3d> {
        Arc::new(
            Source1WaterTexture3d::rgba8_repeating(key, 1, 1, vec![128, 128, 255, 255])
                .expect("fixture texture must be valid"),
        )
    }

    fn primitive(with_normals: bool, with_uv: bool) -> MeshPrimitive {
        let primitive = MeshPrimitive::new(
            vec![[0.0, 0.0, 0.0], [2.0, 0.0, 0.0], [0.0, 0.0, 2.0]],
            vec![0, 1, 2],
        )
        .expect("fixture geometry must be valid");
        let primitive = if with_normals {
            primitive
                .with_normals(vec![[0.0, 1.0, 0.0]; 3])
                .expect("normal count must match")
        } else {
            primitive
        };
        if with_uv {
            primitive
                .with_tex_coords_0(vec![[0.0, 0.0], [2.0, 0.0], [0.0, 2.0]])
                .expect("UV count must match")
        } else {
            primitive
        }
    }

    #[test]
    fn texture_is_linear_repeating_and_validated() {
        let texture =
            Source1WaterTexture3d::rgba8_repeating("water/normal", 1, 1, vec![128, 128, 255, 255])
                .expect("valid normal map");
        assert_eq!(texture.cache_key(), "water/normal");
        assert_eq!(texture.metadata.color_space(), TextureColorSpace::Linear);
        assert_eq!(texture.prepared.mip_level_count(), 1);
        assert_eq!(
            Source1WaterTexture3d::rgba8_repeating("", 1, 1, vec![0; 4])
                .expect_err("empty key must fail"),
            Source1WaterTextureError3d::EmptyCacheKey
        );
    }

    #[test]
    fn batch_requires_real_water_vertex_streams() {
        let material = Source1WaterMaterial3d::new(texture("water/normal"));
        assert!(matches!(
            Source1WaterBatch3d::new(primitive(false, true), material.clone()),
            Err(Source1WaterBatchError3d::MissingNormals)
        ));
        assert!(matches!(
            Source1WaterBatch3d::new(primitive(true, false), material),
            Err(Source1WaterBatchError3d::MissingTexCoords0)
        ));
    }

    #[test]
    fn material_rejects_invalid_fog_and_preserves_proxy_values() {
        let invalid = Source1WaterMaterial3d::new(texture("water/normal")).with_fog(
            [0.1, 0.2, 0.3],
            304.0,
            150.0,
        );
        assert_eq!(
            invalid.validate(),
            Err(Source1WaterMaterialError3d::InvalidFogRange)
        );
        let valid = Source1WaterMaterial3d::new(texture("water/normal"))
            .with_normal_uv_scale(0.35)
            .with_scroll_velocity([0.07, 0.0]);
        valid.validate().expect("Source proxy values are valid");
        assert_eq!(valid.normal_uv_scale(), 0.35);
        assert_eq!(valid.scroll_velocity(), [0.07, 0.0]);
    }

    #[test]
    fn world_enforces_aggregate_resource_limits() {
        let batch = Source1WaterBatch3d::new(
            primitive(true, true),
            Source1WaterMaterial3d::new(texture("water/normal")),
        )
        .expect("fixture batch must be valid");
        let limits = Source1WaterLimits3d {
            max_batches: 1,
            max_vertices: 2,
            max_indices: 3,
            max_unique_textures: 1,
        };
        assert_eq!(
            Source1WaterWorld3d::new(vec![batch], limits).expect_err("vertex limit must fail"),
            Source1WaterWorldBuildError3d::TooManyVertices {
                actual: 3,
                maximum: 2,
            }
        );
    }

    #[test]
    fn world_reports_real_geometry_and_deduplicates_texture_keys() {
        let normal = texture("water/shared");
        let first = Source1WaterBatch3d::new(
            primitive(true, true),
            Source1WaterMaterial3d::new(Arc::clone(&normal)),
        )
        .expect("first batch must be valid");
        let second =
            Source1WaterBatch3d::new(primitive(true, true), Source1WaterMaterial3d::new(normal))
                .expect("second batch must be valid");
        let world = Source1WaterWorld3d::new(vec![first, second], Source1WaterLimits3d::default())
            .expect("world must fit defaults");
        assert_eq!(
            world.stats(),
            Source1WaterBuildStats3d {
                batches: 2,
                vertices: 6,
                indices: 6,
                triangles: 2,
                unique_textures: 1,
            }
        );
    }

    #[test]
    fn batch_center_uses_aabb_without_position_sum_overflow() {
        let batch = Source1WaterBatch3d::new(
            primitive(true, true),
            Source1WaterMaterial3d::new(texture("water/normal")),
        )
        .expect("fixture batch must be valid");
        assert_eq!(batch.center(), [1.0, 0.0, 1.0]);
    }

    #[test]
    fn draw_uniform_keeps_typed_material_fields() {
        let material = Source1WaterMaterial3d::new(texture("water/normal"))
            .with_tint_and_opacity([0.2, 0.3, 0.4], 0.6)
            .with_fog([0.1, 0.2, 0.25], 10.0, 40.0)
            .with_normal_strength(0.5)
            .with_normal_uv_scale(0.35)
            .with_scroll_velocity([0.07, -0.02])
            .with_fresnel(4.0, 0.09);
        material.validate().expect("material must be valid");
        let uniform = material.uniform();
        assert_eq!(uniform.tint_opacity, [0.2, 0.3, 0.4, 0.6]);
        assert_eq!(uniform.fog_color_start, [0.1, 0.2, 0.25, 10.0]);
        assert_eq!(
            uniform.fog_end_normal_fresnel_reflectivity,
            [40.0, 0.5, 4.0, 0.09]
        );
        assert_eq!(uniform.uv_scale_scroll, [0.35, 0.07, -0.02, 0.0]);
    }

    #[test]
    fn animated_normal_map_selects_source_frames_from_elapsed_time() {
        let material = Source1WaterMaterial3d::new(texture("water/still")).with_normal_animation(
            vec![
                texture("water/frame-0"),
                texture("water/frame-1"),
                texture("water/frame-2"),
            ],
            30.0,
        );
        material.validate().expect("30 FPS animation is valid");
        assert_eq!(material.normal_map_frames().len(), 3);
        assert_eq!(material.normal_frame_rate(), 30.0);
        assert_eq!(animated_frame_index(0.0, 30.0, 3), 0);
        assert_eq!(animated_frame_index(1.0 / 30.0, 30.0, 3), 1);
        assert_eq!(animated_frame_index(3.0 / 30.0, 30.0, 3), 0);
        assert_eq!(animated_frame_index(-1.0 / 30.0, 30.0, 3), 2);
    }

    #[test]
    fn animated_normal_map_rejects_empty_or_stalled_sequences() {
        let empty = Source1WaterMaterial3d::new(texture("water/still"))
            .with_normal_animation(Vec::new(), 30.0);
        assert_eq!(
            empty.validate(),
            Err(Source1WaterMaterialError3d::EmptyNormalMapAnimation)
        );
        let stalled = Source1WaterMaterial3d::new(texture("water/still")).with_normal_animation(
            vec![texture("water/frame-0"), texture("water/frame-1")],
            0.0,
        );
        assert_eq!(
            stalled.validate(),
            Err(Source1WaterMaterialError3d::NonPositive(
                "animated normal-map frame rate"
            ))
        );
    }
}
