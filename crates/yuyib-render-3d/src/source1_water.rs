//! Transparent Source 1 water for already-cooked static BSP surfaces.
//!
//! This module deliberately owns a separate render phase from the opaque
//! static-world renderer. It provides repeating, scrolling normal maps,
//! view-dependent tint and distance fog. [`Source1WaterRenderer3d`] also owns
//! a bounded full-resolution compositor: applications capture the opaque scene
//! and a mirrored reflection through ordinary renderer closures, then composite
//! water without ever sampling the active colour attachment. The original
//! tint-only draw remains available for callers that do not need scene capture.

use std::{
    collections::{BTreeMap, HashMap},
    error::Error,
    fmt,
    mem::size_of,
    sync::Arc,
};

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;
use yuyib_2d::{
    Texture, TextureAlphaMode, TextureColorSpace, TextureHandle, TextureSize, TextureSizeError,
};
use yuyib_assets::Assets;
use yuyib_model::MeshPrimitive;
use yuyib_render::{FrameRenderTarget, FrameRenderTargetError, RenderFrame, wgpu};
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
    refraction_distortion: f32,
    reflection_distortion: f32,
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
            refraction_distortion: 0.09,
            reflection_distortion: 0.09,
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

    /// Sets Source-style screen-space offsets for refraction and reflection.
    ///
    /// Importers normally map `$refractamount` and `$reflectamount` here. The
    /// values are applied in normalized screen UV space and should therefore
    /// remain small.
    #[must_use]
    pub const fn with_scene_distortion(
        mut self,
        refraction_amount: f32,
        reflection_amount: f32,
    ) -> Self {
        self.refraction_distortion = refraction_amount;
        self.reflection_distortion = reflection_amount;
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

    /// Normal-derived screen UV offsets as `[refraction, reflection]`.
    #[must_use]
    pub const fn scene_distortion(&self) -> [f32; 2] {
        [self.refraction_distortion, self.reflection_distortion]
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
        validate_finite("refraction distortion", self.refraction_distortion)?;
        validate_finite("reflection distortion", self.reflection_distortion)?;
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
        if self.refraction_distortion < 0.0 {
            return Err(Source1WaterMaterialError3d::Negative(
                "refraction distortion",
            ));
        }
        if self.reflection_distortion < 0.0 {
            return Err(Source1WaterMaterialError3d::Negative(
                "reflection distortion",
            ));
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
            scene_distortion: [
                self.refraction_distortion,
                self.reflection_distortion,
                0.0,
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
    dominant_horizontal_plane_height: Option<f32>,
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
        let dominant_horizontal_plane_height = dominant_horizontal_plane_height(&batches);
        let stats = Source1WaterBuildStats3d {
            batches: batches.len(),
            vertices,
            indices,
            triangles: indices / 3,
            unique_textures: textures.len(),
        };
        Ok(Self {
            batches,
            stats,
            dominant_horizontal_plane_height,
        })
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

    /// Representative horizontal water-plane height, weighted by visible area.
    ///
    /// Horizontal triangles are grouped into one-centimetre height buckets.
    /// The bucket with the largest projected XZ area wins, so a small fountain
    /// does not move the map-wide planar reflection away from a dominant canal
    /// or lake. Returns `None` when no sufficiently horizontal triangle exists.
    #[must_use]
    pub const fn dominant_horizontal_plane_height(&self) -> Option<f32> {
        self.dominant_horizontal_plane_height
    }
}

/// Mirrors a camera across the horizontal plane `world_y = plane_height`.
///
/// Position, target and up direction receive the same reflection transform;
/// projection and clip settings remain unchanged.
///
/// # Errors
///
/// Returns [`Source1WaterReflectionCameraError3d::NonFinitePlaneHeight`] when
/// the requested plane cannot define a finite reflection.
pub fn mirror_camera_across_horizontal_plane(
    camera: Camera3d,
    plane_height: f32,
) -> Result<Camera3d, Source1WaterReflectionCameraError3d> {
    if !plane_height.is_finite() {
        return Err(Source1WaterReflectionCameraError3d::NonFinitePlaneHeight);
    }
    let mirror_y = |value: f32| plane_height * 2.0 - value;
    Ok(Camera3d::new(
        [
            camera.position[0],
            mirror_y(camera.position[1]),
            camera.position[2],
        ],
        [
            camera.target[0],
            mirror_y(camera.target[1]),
            camera.target[2],
        ],
        [camera.up[0], -camera.up[1], camera.up[2]],
        camera.vertical_fov_radians,
        camera.near,
        camera.far,
    ))
}

/// Invalid planar-reflection camera request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Source1WaterReflectionCameraError3d {
    /// Plane height is NaN or infinite.
    NonFinitePlaneHeight,
}

impl fmt::Display for Source1WaterReflectionCameraError3d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFinitePlaneHeight => {
                formatter.write_str("water reflection plane height must be finite")
            }
        }
    }
}

impl Error for Source1WaterReflectionCameraError3d {}

#[derive(Default)]
struct HorizontalPlaneAccumulator {
    projected_area: f64,
    weighted_height: f64,
}

fn dominant_horizontal_plane_height(batches: &[Source1WaterBatch3d]) -> Option<f32> {
    const HEIGHT_BUCKET_SIZE: f64 = 0.01;
    const MINIMUM_UP_ALIGNMENT: f64 = 0.98;
    let mut planes = BTreeMap::<i64, HorizontalPlaneAccumulator>::new();
    for batch in batches {
        let positions = batch.primitive.positions();
        for triangle in batch.primitive.indices().chunks_exact(3) {
            let a = positions[triangle[0] as usize];
            let b = positions[triangle[1] as usize];
            let c = positions[triangle[2] as usize];
            let ab = [
                f64::from(b[0] - a[0]),
                f64::from(b[1] - a[1]),
                f64::from(b[2] - a[2]),
            ];
            let ac = [
                f64::from(c[0] - a[0]),
                f64::from(c[1] - a[1]),
                f64::from(c[2] - a[2]),
            ];
            let cross = [
                ab[1] * ac[2] - ab[2] * ac[1],
                ab[2] * ac[0] - ab[0] * ac[2],
                ab[0] * ac[1] - ab[1] * ac[0],
            ];
            let length = (cross[0] * cross[0] + cross[1] * cross[1] + cross[2] * cross[2]).sqrt();
            if length <= f64::EPSILON || cross[1].abs() / length < MINIMUM_UP_ALIGNMENT {
                continue;
            }
            let projected_area = cross[1].abs() * 0.5;
            let height = (f64::from(a[1]) + f64::from(b[1]) + f64::from(c[1])) / 3.0;
            let key = (height / HEIGHT_BUCKET_SIZE).round() as i64;
            let accumulator = planes.entry(key).or_default();
            accumulator.projected_area += projected_area;
            accumulator.weighted_height += height * projected_area;
        }
    }
    planes
        .into_iter()
        .max_by(|(left_key, left), (right_key, right)| {
            left.projected_area
                .total_cmp(&right.projected_area)
                .then_with(|| right_key.cmp(left_key))
        })
        .and_then(|(_, plane)| {
            (plane.projected_area > f64::EPSILON)
                .then_some((plane.weighted_height / plane.projected_area) as f32)
        })
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

struct WaterCaptureTarget {
    _color: wgpu::Texture,
    color_view: wgpu::TextureView,
    _depth: wgpu::Texture,
    depth_view: wgpu::TextureView,
}

struct WaterCompositorTargets {
    size: [u32; 2],
    color_format: wgpu::TextureFormat,
    depth_format: wgpu::TextureFormat,
    opaque: WaterCaptureTarget,
    reflection: WaterCaptureTarget,
    _composite_color: wgpu::Texture,
    composite_color_view: wgpu::TextureView,
    scene_bind_group: wgpu::BindGroup,
    composite_present_bind_group: wgpu::BindGroup,
    opaque_captured: bool,
    reflection_captured: bool,
}

/// Separate transparent GPU phase for static Source water surfaces.
pub struct Source1WaterRenderer3d {
    pipeline: wgpu::RenderPipeline,
    present_pipeline: wgpu::RenderPipeline,
    camera_layout: wgpu::BindGroupLayout,
    draw_layout: wgpu::BindGroupLayout,
    camera_buffer: wgpu::Buffer,
    draw_buffer: wgpu::Buffer,
    draw_uniform_stride: u64,
    batch_capacity: usize,
    camera_bind_group: wgpu::BindGroup,
    draw_bind_group: wgpu::BindGroup,
    texture_layout: wgpu::BindGroupLayout,
    scene_layout: wgpu::BindGroupLayout,
    scene_sampler: wgpu::Sampler,
    _fallback_scene_texture: wgpu::Texture,
    fallback_scene_bind_group: wgpu::BindGroup,
    surface_format: wgpu::TextureFormat,
    depth_format: wgpu::TextureFormat,
    compositor_targets: Option<WaterCompositorTargets>,
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

    /// Captures the opaque/refraction scene into renderer-owned colour/depth.
    ///
    /// The target is recreated when the active draw size or formats change and
    /// is cleared before `render` runs. Existing 3D renderers can draw through
    /// the nested [`RenderFrame`] without knowing that the target is offscreen.
    pub fn capture_opaque_for_frame<Result>(
        &mut self,
        frame: &mut RenderFrame<'_>,
        clear_color: wgpu::Color,
        render: impl FnOnce(&mut RenderFrame<'_>) -> Result,
    ) -> std::result::Result<Result, Source1WaterCompositorError3d> {
        self.ensure_compositor_targets(frame)?;
        let targets = self
            .compositor_targets
            .as_ref()
            .expect("ensure_compositor_targets publishes targets");
        let target = FrameRenderTarget::new(
            &targets.opaque.color_view,
            &targets.opaque.depth_view,
            targets.size,
            targets.color_format,
            targets.depth_format,
        )
        .map_err(Source1WaterCompositorError3d::FrameTarget)?;
        let result = frame
            .with_render_target(target, |nested| {
                nested.with_surface_pass_with_depth(
                    wgpu::LoadOp::Clear(clear_color),
                    wgpu::LoadOp::Clear(1.0),
                    |_| {},
                );
                render(nested)
            })
            .map_err(Source1WaterCompositorError3d::FrameTarget)?;
        self.compositor_targets
            .as_mut()
            .expect("capture target remains resident")
            .opaque_captured = true;
        Ok(result)
    }

    /// Captures a caller-rendered planar reflection into separate colour/depth.
    ///
    /// Use [`mirror_camera_across_horizontal_plane`] with
    /// [`Source1WaterWorld3d::dominant_horizontal_plane_height`] to construct
    /// the camera passed to the scene renderers inside `render`.
    pub fn capture_reflection_for_frame<Result>(
        &mut self,
        frame: &mut RenderFrame<'_>,
        clear_color: wgpu::Color,
        render: impl FnOnce(&mut RenderFrame<'_>) -> Result,
    ) -> std::result::Result<Result, Source1WaterCompositorError3d> {
        self.ensure_compositor_targets(frame)?;
        let targets = self
            .compositor_targets
            .as_ref()
            .expect("ensure_compositor_targets publishes targets");
        let target = FrameRenderTarget::new(
            &targets.reflection.color_view,
            &targets.reflection.depth_view,
            targets.size,
            targets.color_format,
            targets.depth_format,
        )
        .map_err(Source1WaterCompositorError3d::FrameTarget)?;
        let result = frame
            .with_render_target(target, |nested| {
                nested.with_surface_pass_with_depth(
                    wgpu::LoadOp::Clear(clear_color),
                    wgpu::LoadOp::Clear(1.0),
                    |_| {},
                );
                render(nested)
            })
            .map_err(Source1WaterCompositorError3d::FrameTarget)?;
        self.compositor_targets
            .as_mut()
            .expect("capture target remains resident")
            .reflection_captured = true;
        Ok(result)
    }

    /// Composites captured opaque colour, optional reflection and water, then
    /// presents the finished full-screen colour into the active surface region.
    ///
    /// The active composite colour is distinct from both sampled captures.
    /// Water depth-tests against the opaque capture's depth and keeps depth
    /// writes disabled, eliminating attachment feedback and foreground leaks.
    pub fn composite_captured_for_frame(
        &mut self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        time_seconds: f32,
    ) -> Result<Source1WaterDrawStats3d, Source1WaterCompositorError3d> {
        self.ensure_compositor_targets(frame)?;
        let targets = self
            .compositor_targets
            .as_ref()
            .expect("ensure_compositor_targets publishes targets");
        if !targets.opaque_captured {
            return Err(Source1WaterCompositorError3d::OpaqueSceneNotCaptured);
        }
        let target = FrameRenderTarget::new(
            &targets.composite_color_view,
            &targets.opaque.depth_view,
            targets.size,
            targets.color_format,
            targets.depth_format,
        )
        .map_err(Source1WaterCompositorError3d::FrameTarget)?;
        let reflection_captured = targets.reflection_captured;
        let scene_bind_group = &targets.scene_bind_group;
        let draw = frame
            .with_render_target(target, |nested| {
                self.blit_to_active_target(
                    nested,
                    scene_bind_group,
                    wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                );
                self.draw_internal(
                    nested,
                    camera,
                    time_seconds,
                    DepthLoad::Load,
                    scene_bind_group,
                    true,
                    reflection_captured,
                )
            })
            .map_err(Source1WaterCompositorError3d::FrameTarget)?
            .map_err(Source1WaterCompositorError3d::Render)?;
        let targets = self
            .compositor_targets
            .as_ref()
            .expect("composite targets remain resident");
        self.blit_to_active_target(
            frame,
            &targets.composite_present_bind_group,
            wgpu::LoadOp::Load,
        );
        Ok(draw)
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
        self.draw_internal(
            frame,
            camera,
            time_seconds,
            depth_load,
            &self.fallback_scene_bind_group,
            false,
            false,
        )
    }

    fn draw_internal(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        time_seconds: f32,
        depth_load: DepthLoad,
        scene_bind_group: &wgpu::BindGroup,
        has_refraction: bool,
        has_reflection: bool,
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
            viewport_inverse_scene: [
                1.0 / frame.draw_size()[0] as f32,
                1.0 / frame.draw_size()[1] as f32,
                u32::from(has_refraction) as f32,
                u32::from(has_reflection) as f32,
            ],
        };
        frame
            .queue()
            .write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&camera_uniform));
        let order = sorted_back_to_front(&self.batches, camera.position);
        frame.with_surface_pass_with_depth(wgpu::LoadOp::Load, depth_load.operation(), |pass| {
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            pass.set_bind_group(3, scene_bind_group, &[]);
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

    fn blit_to_active_target(
        &self,
        frame: &mut RenderFrame<'_>,
        bind_group: &wgpu::BindGroup,
        load: wgpu::LoadOp<wgpu::Color>,
    ) {
        frame.with_surface_pass(load, |pass| {
            pass.set_pipeline(&self.present_pipeline);
            pass.set_bind_group(0, bind_group, &[]);
            pass.draw(0..3, 0..1);
        });
    }

    fn ensure_compositor_targets(
        &mut self,
        frame: &RenderFrame<'_>,
    ) -> Result<(), Source1WaterCompositorError3d> {
        let size = frame.draw_size();
        if size.contains(&0) {
            return Err(Source1WaterCompositorError3d::EmptyTarget { size });
        }
        let color_format = frame.surface_format();
        let depth_format = frame.depth_format();
        if self.surface_format != color_format || self.depth_format != depth_format {
            self.pipeline = create_water_pipeline(
                frame.device(),
                color_format,
                depth_format,
                &self.camera_layout,
                &self.draw_layout,
                &self.texture_layout,
                &self.scene_layout,
            );
            self.present_pipeline =
                create_water_present_pipeline(frame.device(), color_format, &self.scene_layout);
            self.surface_format = color_format;
            self.depth_format = depth_format;
            self.compositor_targets = None;
        }
        let recreate = self.compositor_targets.as_ref().is_none_or(|targets| {
            targets.size != size
                || targets.color_format != color_format
                || targets.depth_format != depth_format
        });
        if recreate {
            self.compositor_targets = Some(WaterCompositorTargets::new(
                frame.device(),
                size,
                color_format,
                depth_format,
                &self.scene_layout,
                &self.scene_sampler,
            ));
        }
        Ok(())
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
        let scene_layout = water_scene_texture_layout(device);
        let scene_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("yuyib Source 1 water scene sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let fallback_scene_texture = create_water_color_texture(
            device,
            [1, 1],
            wgpu::TextureFormat::Rgba8Unorm,
            "yuyib Source 1 water fallback scene",
            wgpu::TextureUsages::TEXTURE_BINDING,
        );
        let fallback_scene_view =
            fallback_scene_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let fallback_scene_bind_group = create_water_scene_bind_group(
            device,
            &scene_layout,
            &scene_sampler,
            &fallback_scene_view,
            &fallback_scene_view,
            "yuyib Source 1 water fallback scene bind group",
        );
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
        let pipeline = create_water_pipeline(
            device,
            format,
            depth_format,
            &camera_layout,
            &draw_layout,
            &texture_layout,
            &scene_layout,
        );
        let present_pipeline = create_water_present_pipeline(device, format, &scene_layout);
        Ok(Self {
            pipeline,
            present_pipeline,
            camera_layout,
            draw_layout,
            camera_buffer,
            draw_buffer,
            draw_uniform_stride,
            batch_capacity,
            camera_bind_group,
            draw_bind_group,
            texture_layout,
            scene_layout,
            scene_sampler,
            _fallback_scene_texture: fallback_scene_texture,
            fallback_scene_bind_group,
            surface_format: format,
            depth_format,
            compositor_targets: None,
            texture_assets: Assets::new(),
            texture_cache: TextureCache::new(),
            texture_handles: HashMap::new(),
            batches: Vec::new(),
        })
    }
}

impl WaterCompositorTargets {
    fn new(
        device: &wgpu::Device,
        size: [u32; 2],
        color_format: wgpu::TextureFormat,
        depth_format: wgpu::TextureFormat,
        scene_layout: &wgpu::BindGroupLayout,
        scene_sampler: &wgpu::Sampler,
    ) -> Self {
        let opaque = create_water_capture_target(
            device,
            size,
            color_format,
            depth_format,
            "yuyib Source 1 water opaque capture",
        );
        let reflection = create_water_capture_target(
            device,
            size,
            color_format,
            depth_format,
            "yuyib Source 1 water reflection capture",
        );
        let composite_color = create_water_color_texture(
            device,
            size,
            color_format,
            "yuyib Source 1 water composite color",
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        let composite_color_view =
            composite_color.create_view(&wgpu::TextureViewDescriptor::default());
        let scene_bind_group = create_water_scene_bind_group(
            device,
            scene_layout,
            scene_sampler,
            &opaque.color_view,
            &reflection.color_view,
            "yuyib Source 1 water captured scene bind group",
        );
        let composite_present_bind_group = create_water_scene_bind_group(
            device,
            scene_layout,
            scene_sampler,
            &composite_color_view,
            &composite_color_view,
            "yuyib Source 1 water composite present bind group",
        );
        Self {
            size,
            color_format,
            depth_format,
            opaque,
            reflection,
            _composite_color: composite_color,
            composite_color_view,
            scene_bind_group,
            composite_present_bind_group,
            opaque_captured: false,
            reflection_captured: false,
        }
    }
}

fn create_water_capture_target(
    device: &wgpu::Device,
    size: [u32; 2],
    color_format: wgpu::TextureFormat,
    depth_format: wgpu::TextureFormat,
    label: &'static str,
) -> WaterCaptureTarget {
    let color = create_water_color_texture(
        device,
        size,
        color_format,
        label,
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
    );
    let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
    let depth = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: depth_format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());
    WaterCaptureTarget {
        _color: color,
        color_view,
        _depth: depth,
        depth_view,
    }
}

fn create_water_color_texture(
    device: &wgpu::Device,
    size: [u32; 2],
    format: wgpu::TextureFormat,
    label: &'static str,
    usage: wgpu::TextureUsages,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    })
}

fn create_water_scene_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    refraction: &wgpu::TextureView,
    reflection: &wgpu::TextureView,
    label: &'static str,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(refraction),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(reflection),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    })
}

fn create_water_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    depth_format: wgpu::TextureFormat,
    camera_layout: &wgpu::BindGroupLayout,
    draw_layout: &wgpu::BindGroupLayout,
    texture_layout: &wgpu::BindGroupLayout,
    scene_layout: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("yuyib Source 1 water WGSL"),
        source: wgpu::ShaderSource::Wgsl(SOURCE1_WATER_WGSL.into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("yuyib Source 1 water pipeline layout"),
        bind_group_layouts: &[
            Some(camera_layout),
            Some(draw_layout),
            Some(texture_layout),
            Some(scene_layout),
        ],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
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
    })
}

fn create_water_present_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    scene_layout: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("yuyib Source 1 water present WGSL"),
        source: wgpu::ShaderSource::Wgsl(SOURCE1_WATER_PRESENT_WGSL.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("yuyib Source 1 water present pipeline layout"),
        bind_group_layouts: &[Some(scene_layout)],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("yuyib Source 1 water present pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
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

/// Failure while capturing or compositing the offscreen water scene.
#[derive(Debug)]
pub enum Source1WaterCompositorError3d {
    /// Active draw region cannot allocate a render target.
    EmptyTarget {
        /// Rejected physical dimensions.
        size: [u32; 2],
    },
    /// Caller-owned render-target seam rejected compositor metadata.
    FrameTarget(FrameRenderTargetError),
    /// Composition was requested before the opaque scene capture.
    OpaqueSceneNotCaptured,
    /// Transparent water recording failed.
    Render(Source1WaterRenderError3d),
}

impl fmt::Display for Source1WaterCompositorError3d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyTarget { size } => write!(
                formatter,
                "water compositor target is empty: {}x{}",
                size[0], size[1]
            ),
            Self::FrameTarget(source) => write!(formatter, "invalid water render target: {source}"),
            Self::OpaqueSceneNotCaptured => formatter.write_str(
                "water composition requires capture_opaque_for_frame in the current target generation",
            ),
            Self::Render(source) => write!(formatter, "cannot composite Source 1 water: {source}"),
        }
    }
}

impl Error for Source1WaterCompositorError3d {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::FrameTarget(source) => Some(source),
            Self::Render(source) => Some(source),
            Self::EmptyTarget { .. } | Self::OpaqueSceneNotCaptured => None,
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
    viewport_inverse_scene: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct WaterDrawUniform {
    tint_opacity: [f32; 4],
    fog_color_start: [f32; 4],
    fog_end_normal_fresnel_reflectivity: [f32; 4],
    uv_scale_scroll: [f32; 4],
    scene_distortion: [f32; 4],
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

fn water_scene_texture_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("yuyib Source 1 water scene texture layout"),
        entries: &[
            water_sampled_texture_entry(0),
            water_sampled_texture_entry(1),
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    })
}

const fn water_sampled_texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
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
    viewport_inverse_scene: vec4<f32>,
};
struct Draw {
    tint_opacity: vec4<f32>,
    fog_color_start: vec4<f32>,
    fog_end_normal_fresnel_reflectivity: vec4<f32>,
    uv_scale_scroll: vec4<f32>,
    scene_distortion: vec4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var<uniform> draw: Draw;
@group(2) @binding(0) var normal_texture: texture_2d<f32>;
@group(2) @binding(1) var normal_sampler: sampler;
@group(3) @binding(0) var refraction_texture: texture_2d<f32>;
@group(3) @binding(1) var reflection_texture: texture_2d<f32>;
@group(3) @binding(2) var scene_sampler: sampler;

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

    let facing_light = 0.72 + max(normal.y, 0.0) * 0.28;
    let base = draw.tint_opacity.rgb * facing_light;
    let pale_reflection = mix(base, vec3<f32>(0.82, 0.92, 0.95), fresnel * 0.38);
    let fog_start = draw.fog_color_start.w;
    let fog_end = draw.fog_end_normal_fresnel_reflectivity.x;
    let fog_amount = smoothstep(fog_start, fog_end, distance_to_camera);
    let has_refraction = camera.viewport_inverse_scene.z > 0.5;
    let has_reflection = camera.viewport_inverse_scene.w > 0.5;
    if has_refraction {
        let screen_uv = clamp(
            input.clip_position.xy * camera.viewport_inverse_scene.xy,
            vec2<f32>(0.0),
            vec2<f32>(1.0),
        );
        let refraction_uv = clamp(
            screen_uv + mapped.xy * draw.scene_distortion.x,
            vec2<f32>(0.0),
            vec2<f32>(1.0),
        );
        let reflection_uv = clamp(
            screen_uv - mapped.xy * draw.scene_distortion.y,
            vec2<f32>(0.0),
            vec2<f32>(1.0),
        );
        let refracted = textureSample(refraction_texture, scene_sampler, refraction_uv).rgb;
        var reflected = pale_reflection;
        if has_reflection {
            reflected = textureSample(reflection_texture, scene_sampler, reflection_uv).rgb;
        }
        let filtered_refraction = refracted * mix(
            vec3<f32>(1.0),
            clamp(draw.tint_opacity.rgb + vec3<f32>(0.55), vec3<f32>(0.0), vec3<f32>(1.5)),
            0.22,
        );
        let scene_surface = mix(filtered_refraction, reflected, clamp(fresnel, 0.0, 1.0));
        let scene_color = mix(scene_surface, draw.fog_color_start.rgb, fog_amount);
        // Refraction already contains the opaque background. Alpha one avoids
        // blending that background into itself a second time.
        return vec4<f32>(scene_color, 1.0);
    }
    let color = mix(pale_reflection, draw.fog_color_start.rgb, fog_amount);
    let alpha = clamp(draw.tint_opacity.a + fresnel * 0.12, 0.0, 1.0);
    return vec4<f32>(color, alpha);
}
"#;

const SOURCE1_WATER_PRESENT_WGSL: &str = r#"
@group(0) @binding(0) var source_texture: texture_2d<f32>;
@group(0) @binding(2) var source_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) tex_coord: vec2<f32>,
};

@vertex fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    let positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -3.0),
        vec2<f32>(3.0, 1.0),
        vec2<f32>(-1.0, 1.0),
    );
    let tex_coords = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 2.0),
        vec2<f32>(2.0, 0.0),
        vec2<f32>(0.0, 0.0),
    );
    var output: VertexOutput;
    output.position = vec4<f32>(positions[vertex_index], 0.0, 1.0);
    output.tex_coord = tex_coords[vertex_index];
    return output;
}

@fragment fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(source_texture, source_sampler, input.tex_coord);
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

    fn horizontal_primitive(height: f32, extent: f32) -> MeshPrimitive {
        MeshPrimitive::new(
            vec![
                [0.0, height, 0.0],
                [extent, height, 0.0],
                [extent, height, extent],
                [0.0, height, extent],
            ],
            vec![0, 2, 1, 0, 3, 2],
        )
        .expect("horizontal fixture geometry must be valid")
        .with_normals(vec![[0.0, 1.0, 0.0]; 4])
        .expect("horizontal fixture normals must match")
        .with_tex_coords_0(vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]])
        .expect("horizontal fixture UVs must match")
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
            .with_fresnel(4.0, 0.09)
            .with_scene_distortion(0.08, 0.04);
        material.validate().expect("material must be valid");
        let uniform = material.uniform();
        assert_eq!(uniform.tint_opacity, [0.2, 0.3, 0.4, 0.6]);
        assert_eq!(uniform.fog_color_start, [0.1, 0.2, 0.25, 10.0]);
        assert_eq!(
            uniform.fog_end_normal_fresnel_reflectivity,
            [40.0, 0.5, 4.0, 0.09]
        );
        assert_eq!(uniform.uv_scale_scroll, [0.35, 0.07, -0.02, 0.0]);
        assert_eq!(uniform.scene_distortion, [0.08, 0.04, 0.0, 0.0]);
        assert_eq!(material.scene_distortion(), [0.08, 0.04]);
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

    #[test]
    fn dominant_plane_uses_horizontal_projected_area() {
        let small_high = Source1WaterBatch3d::new(
            horizontal_primitive(10.0, 1.0),
            Source1WaterMaterial3d::new(texture("water/high")),
        )
        .expect("small high plane must be valid");
        let large_low = Source1WaterBatch3d::new(
            horizontal_primitive(3.0, 4.0),
            Source1WaterMaterial3d::new(texture("water/low")),
        )
        .expect("large low plane must be valid");
        let world =
            Source1WaterWorld3d::new(vec![small_high, large_low], Source1WaterLimits3d::default())
                .expect("fixture world must fit limits");
        assert_eq!(world.dominant_horizontal_plane_height(), Some(3.0));
    }

    #[test]
    fn reflection_camera_mirrors_position_target_and_up() {
        let camera = Camera3d::new(
            [2.0, 7.0, 4.0],
            [5.0, 2.0, 8.0],
            [0.0, 1.0, 0.0],
            1.0,
            0.1,
            500.0,
        );
        let mirrored =
            mirror_camera_across_horizontal_plane(camera, 1.0).expect("finite plane must mirror");
        assert_eq!(mirrored.position, [2.0, -5.0, 4.0]);
        assert_eq!(mirrored.target, [5.0, 0.0, 8.0]);
        assert_eq!(mirrored.up, [0.0, -1.0, 0.0]);
        assert_eq!(mirrored.vertical_fov_radians, camera.vertical_fov_radians);
        assert_eq!(
            mirror_camera_across_horizontal_plane(camera, f32::NAN),
            Err(Source1WaterReflectionCameraError3d::NonFinitePlaneHeight)
        );
    }

    #[test]
    fn scene_shader_samples_non_aliasing_refraction_and_reflection() {
        assert!(SOURCE1_WATER_WGSL.contains("@group(3) @binding(0) var refraction_texture"));
        assert!(SOURCE1_WATER_WGSL.contains("@group(3) @binding(1) var reflection_texture"));
        assert_eq!(SOURCE1_WATER_WGSL.matches("@builtin(position)").count(), 1);
        assert!(SOURCE1_WATER_WGSL.contains("input.clip_position.xy"));
        assert!(SOURCE1_WATER_WGSL.contains("return vec4<f32>(scene_color, 1.0)"));
        assert!(SOURCE1_WATER_PRESENT_WGSL.contains("textureSample(source_texture"));
    }

    #[test]
    fn water_shaders_pass_wgsl_validation() {
        for (label, source) in [
            ("water", SOURCE1_WATER_WGSL),
            ("present", SOURCE1_WATER_PRESENT_WGSL),
        ] {
            let module = naga::front::wgsl::parse_str(source)
                .unwrap_or_else(|error| panic!("{label} WGSL failed to parse: {error}"));
            naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::all(),
            )
            .validate(&module)
            .unwrap_or_else(|error| panic!("{label} WGSL failed validation: {error}"));
        }
    }
}
