//! Batched renderer for large, immutable, factor-coloured worlds.
//!
//! [`StaticWorld3d`] is the CPU cooking step: it joins every compatible
//! primitive of a [`Model`] into one indexed mesh per material. A Source/VMF
//! map with thousands of brush faces therefore becomes a small, bounded set of
//! GPU buffers and draw submissions. [`StaticWorldRenderer3d`] owns those
//! buffers after one explicit upload and reuses them on every frame.
//!
//! This deliberately supports opaque, factor-only materials. It is a world
//! geometry path, not a replacement for glTF/PBR or dynamic character assets.

use std::{
    collections::{BTreeMap, HashMap},
    error::Error,
    fmt,
    mem::size_of,
    sync::Arc,
};

use bytemuck::{Pod, Zeroable};

use wgpu::util::DeviceExt;
use yuyib_2d::{Texture, TextureHandle, TextureSize, TextureSizeError};
use yuyib_assets::Assets;
use yuyib_game_3d::{ClipDepthRange3d, Frustum3d, LocalAabb3d, LocalBounds3d};
use yuyib_model::{AlphaMode, Material, MaterialIndex, MeshPrimitive, Model};
use yuyib_render::{RenderFrame, wgpu};
use yuyib_render_texture::{
    PreparedTextureUpload, TextureCache, TextureSampler, TextureSamplingPreset, TextureUploadError,
};

use crate::{
    Camera3d, CameraUniformRing, DepthLoad, GpuMesh, GpuTexture, GpuTexturedLitMaterial,
    GpuTexturedLitMesh, LambertLighting3d, LitMaterial3d, LitMeshInstance3d, LitMeshUniform,
    MeshDrawStats, MeshRenderError, MeshRenderer3d, MeshUploadError, TexturedLitBatchDraw,
    TexturedLitMeshRenderError, TexturedLitMeshRenderer3d, TexturedLitMeshUploadError,
    aligned_uniform_stride, dynamic_uniform_bind_group, dynamic_uniform_layout, uniform_buffer,
};

/// CPU-side immutable world geometry grouped by material.
#[derive(Clone, Debug, PartialEq)]
pub struct StaticWorld3d {
    batches: Vec<StaticWorldBatch3d>,
    stats: StaticWorldBuildStats3d,
}

/// One material-compatible combined mesh within [`StaticWorld3d`].
#[derive(Clone, Debug, PartialEq)]
pub struct StaticWorldBatch3d {
    primitive: MeshPrimitive,
    color: [f32; 4],
}

impl StaticWorldBatch3d {
    /// Combined indexed geometry for this material batch.
    #[must_use]
    pub const fn primitive(&self) -> &MeshPrimitive {
        &self.primitive
    }

    /// Linear RGBA factor copied from the source material.
    #[must_use]
    pub const fn color(&self) -> [f32; 4] {
        self.color
    }
}

/// Deterministic reduction produced by [`StaticWorld3d::from_model`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StaticWorldBuildStats3d {
    /// Number of source primitive submissions consumed.
    pub source_primitives: usize,
    /// Number of source vertices copied into combined batches.
    pub source_vertices: usize,
    /// Number of source triangles copied into combined batches.
    pub source_triangles: usize,
    /// Number of resulting material batches / GPU meshes.
    pub batches: usize,
}

impl StaticWorld3d {
    /// Cooks opaque, factor-coloured model geometry into material batches.
    ///
    /// Source meshes are never mutated. Vertex order inside every source
    /// primitive is retained; only their index bases change while joining a
    /// batch. Textured or blended materials return a typed error because their
    /// texture/sorting policy cannot be honestly represented by this path.
    ///
    /// # Errors
    ///
    /// Returns [`StaticWorldBuildError3d`] for unsupported material state,
    /// absent material slots, index overflow or invalid combined geometry.
    pub fn from_model(model: &Model) -> Result<Self, StaticWorldBuildError3d> {
        let mut buckets = Vec::<BuildBucket>::new();
        let mut bucket_by_material = HashMap::<Option<usize>, usize>::new();
        let mut stats = StaticWorldBuildStats3d::default();

        for (mesh_index, mesh) in model.meshes().iter().enumerate() {
            for (primitive_index, primitive) in mesh.primitives().iter().enumerate() {
                let (material_key, color) = match primitive.material() {
                    Some(index) => {
                        let material = model.materials().get(index.get()).ok_or(
                            StaticWorldBuildError3d::MissingMaterial {
                                mesh: mesh_index,
                                primitive: primitive_index,
                                material: index,
                            },
                        )?;
                        if material.base_color_texture().is_some() {
                            return Err(StaticWorldBuildError3d::TexturedMaterial {
                                mesh: mesh_index,
                                primitive: primitive_index,
                                material: index,
                            });
                        }
                        if material.alpha_mode() != AlphaMode::Opaque {
                            return Err(StaticWorldBuildError3d::NonOpaqueMaterial {
                                mesh: mesh_index,
                                primitive: primitive_index,
                                material: index,
                                alpha_mode: material.alpha_mode(),
                            });
                        }
                        (Some(index.get()), material.base_color_factor())
                    }
                    None => (None, [1.0; 4]),
                };
                let bucket_index = if let Some(index) = bucket_by_material.get(&material_key) {
                    *index
                } else {
                    let index = buckets.len();
                    buckets.push(BuildBucket::new(color));
                    bucket_by_material.insert(material_key, index);
                    index
                };
                let bucket = &mut buckets[bucket_index];
                let vertex_base = u32::try_from(bucket.positions.len()).map_err(|_| {
                    StaticWorldBuildError3d::TooManyVertices {
                        batch: bucket_index,
                    }
                })?;
                bucket.positions.extend_from_slice(primitive.positions());
                for &index in primitive.indices() {
                    let rebased = vertex_base.checked_add(index).ok_or(
                        StaticWorldBuildError3d::TooManyVertices {
                            batch: bucket_index,
                        },
                    )?;
                    bucket.indices.push(rebased);
                }
                stats.source_primitives += 1;
                stats.source_vertices += primitive.positions().len();
                stats.source_triangles += primitive.indices().len() / 3;
            }
        }

        let mut batches = Vec::with_capacity(buckets.len());
        for (batch, bucket) in buckets.into_iter().enumerate() {
            let primitive = MeshPrimitive::new(bucket.positions, bucket.indices)
                .map_err(|source| StaticWorldBuildError3d::CombinedGeometry { batch, source })?;
            batches.push(StaticWorldBatch3d {
                primitive,
                color: bucket.color,
            });
        }
        stats.batches = batches.len();
        Ok(Self { batches, stats })
    }

    /// Combined batches in deterministic first-material-use order.
    #[must_use]
    pub fn batches(&self) -> &[StaticWorldBatch3d] {
        &self.batches
    }

    /// CPU cooking reduction metrics.
    #[must_use]
    pub const fn stats(&self) -> StaticWorldBuildStats3d {
        self.stats
    }
}

struct BuildBucket {
    positions: Vec<[f32; 3]>,
    indices: Vec<u32>,
    color: [f32; 4],
}

impl BuildBucket {
    fn new(color: [f32; 4]) -> Self {
        Self {
            positions: Vec::new(),
            indices: Vec::new(),
            color,
        }
    }
}

/// Failure while reducing one source model to static world batches.
#[derive(Debug)]
pub enum StaticWorldBuildError3d {
    /// A primitive referenced an absent model material slot.
    MissingMaterial {
        /// Source mesh index.
        mesh: usize,
        /// Source primitive index within the mesh.
        primitive: usize,
        /// Missing material slot.
        material: MaterialIndex,
    },
    /// A material requires a texture-aware renderer.
    TexturedMaterial {
        /// Source mesh index.
        mesh: usize,
        /// Source primitive index within the mesh.
        primitive: usize,
        /// Source material slot.
        material: MaterialIndex,
    },
    /// A material requires a transparent/sorted phase.
    NonOpaqueMaterial {
        /// Source mesh index.
        mesh: usize,
        /// Source primitive index within the mesh.
        primitive: usize,
        /// Source material slot.
        material: MaterialIndex,
        /// Unsupported source alpha policy.
        alpha_mode: AlphaMode,
    },
    /// One combined batch cannot address another vertex with `u32` indices.
    TooManyVertices {
        /// Combined batch index.
        batch: usize,
    },
    /// The joined indexed geometry violates the model primitive contract.
    CombinedGeometry {
        /// Combined batch index.
        batch: usize,
        /// Model validation failure.
        source: yuyib_model::MeshValidationError,
    },
}

impl fmt::Display for StaticWorldBuildError3d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingMaterial {
                mesh,
                primitive,
                material,
            } => write!(
                formatter,
                "static world mesh {mesh}, primitive {primitive} references absent material {}",
                material.get()
            ),
            Self::TexturedMaterial {
                mesh,
                primitive,
                material,
            } => write!(
                formatter,
                "static world mesh {mesh}, primitive {primitive} uses textured material {}",
                material.get()
            ),
            Self::NonOpaqueMaterial {
                mesh,
                primitive,
                material,
                alpha_mode,
            } => write!(
                formatter,
                "static world mesh {mesh}, primitive {primitive} uses non-opaque material {} ({alpha_mode:?})",
                material.get()
            ),
            Self::TooManyVertices { batch } => write!(
                formatter,
                "static world batch {batch} exceeds u32 vertex addressing"
            ),
            Self::CombinedGeometry { batch, source } => {
                write!(formatter, "static world batch {batch} is invalid: {source}")
            }
        }
    }
}

impl Error for StaticWorldBuildError3d {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::CombinedGeometry { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// GPU-resident counterpart of [`StaticWorld3d`].
///
/// Upload it once during loading, then call [`Self::draw_for_frame`] every
/// frame. All batches use the identity model matrix and are intentionally
/// double-sided, which is the practical default for imported brush worlds.
pub struct StaticWorldRenderer3d {
    renderer: MeshRenderer3d,
    batches: Vec<StaticWorldGpuBatch3d>,
}

struct StaticWorldGpuBatch3d {
    mesh: GpuMesh,
    color: [f32; 4],
}

/// Render work reported by [`StaticWorldRenderer3d::draw_for_frame`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StaticWorldDrawStats3d {
    /// Combined GPU meshes submitted this frame.
    pub batches: usize,
    /// Indexed triangles submitted this frame.
    pub triangles: u64,
    /// Low-level indexed draw calls submitted this frame.
    pub draw_calls: u64,
    /// Resident material/cell batches rejected before GPU submission.
    pub culled_batches: u64,
    /// Indexed triangles omitted by camera-frustum rejection.
    pub culled_triangles: u64,
}

impl StaticWorldRenderer3d {
    /// Creates an empty renderer bound to the current frame's GPU device.
    #[must_use]
    pub fn new_for_frame(frame: &RenderFrame<'_>) -> Self {
        Self {
            renderer: MeshRenderer3d::new_for_frame(frame),
            batches: Vec::new(),
        }
    }

    /// Uploads all combined world batches exactly once.
    ///
    /// Replacing a loaded world is intentional and drops the old GPU buffers.
    /// Call this in a loading phase, not every frame.
    ///
    /// # Errors
    ///
    /// Returns the exact batch whose GPU upload failed.
    #[allow(clippy::too_many_lines)]
    pub fn upload_for_frame(
        &mut self,
        frame: &RenderFrame<'_>,
        world: &StaticWorld3d,
    ) -> Result<(), StaticWorldUploadError3d> {
        let renderer = MeshRenderer3d::new_for_frame_with_batch_capacity(
            frame,
            resident_batch_capacity(
                world.batches().len(),
                MeshRenderer3d::DEFAULT_BATCH_CAPACITY,
            ),
        );
        let mut batches = Vec::with_capacity(world.batches().len());
        for (batch, source) in world.batches().iter().enumerate() {
            let mesh = renderer
                .upload_mesh_for_frame(frame, source.primitive())
                .map_err(|source| StaticWorldUploadError3d { batch, source })?;
            batches.push(StaticWorldGpuBatch3d {
                mesh,
                color: source.color(),
            });
        }
        self.renderer = renderer;
        self.batches = batches;
        Ok(())
    }

    /// Returns the number of currently resident material batches.
    #[must_use]
    pub fn batch_count(&self) -> usize {
        self.batches.len()
    }

    /// Draws the whole opaque world through its upload-sized uniform buffer.
    ///
    /// Upload grows persistent capacity to the resident batch count, so the
    /// frame records one coherent depth-cleared pass without reallocating.
    pub fn draw_for_frame(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
    ) -> Result<StaticWorldDrawStats3d, MeshRenderError> {
        let draws: Vec<_> = self
            .batches
            .iter()
            .map(|batch| (&batch.mesh, identity_matrix(), batch.color))
            .collect();
        let draw = self
            .renderer
            .draw_batch_depth_clear_double_sided(frame, camera, &draws)?;
        Ok(StaticWorldDrawStats3d {
            batches: draws.len(),
            triangles: u64::from(draw.triangles),
            draw_calls: u64::from(draw.draw_calls),
            culled_batches: 0,
            culled_triangles: 0,
        })
    }
}

/// GPU upload failure for one combined world batch.
#[derive(Debug)]
pub struct StaticWorldUploadError3d {
    /// Combined batch index.
    pub batch: usize,
    /// Underlying mesh upload failure.
    pub source: MeshUploadError,
}

impl fmt::Display for StaticWorldUploadError3d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "cannot upload static world batch {}: {}",
            self.batch, self.source
        )
    }
}

impl Error for StaticWorldUploadError3d {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}

const fn identity_matrix() -> [f32; 16] {
    [
        1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ]
}

const fn resident_batch_capacity(batch_count: usize, default_capacity: usize) -> usize {
    if batch_count > default_capacity {
        batch_count
    } else {
        default_capacity
    }
}

/// Decoded, sampler-ready texture used by a [`TexturedStaticWorld3d`].
///
/// The cache key should identify the source asset rather than one material
/// slot. Source 1 integrations normally use the canonical VTF path, which
/// makes several VMTs that reference the same VTF share one GPU allocation.
/// Mip generation is completed by the constructor on the loading thread; GPU
/// upload therefore performs no VTF decoding or image filtering.
pub struct StaticWorldTexture3d {
    cache_key: String,
    metadata: Texture,
    prepared: PreparedTextureUpload,
}

impl StaticWorldTexture3d {
    /// Validates decoded RGBA8 pixels and prepares high-quality sampled mips.
    ///
    /// # Errors
    ///
    /// Returns [`StaticWorldTextureError3d`] for empty dimensions, a mismatched
    /// pixel payload or an empty cache identity.
    pub fn rgba8(
        cache_key: impl Into<String>,
        width: u32,
        height: u32,
        pixels: Vec<u8>,
    ) -> Result<Self, StaticWorldTextureError3d> {
        Self::rgba8_with_sampler(
            cache_key,
            width,
            height,
            pixels,
            TextureSamplingPreset::HighQuality.sampler(),
        )
    }

    /// Validates RGBA8 pixels and prepares a high-quality repeating 2D texture.
    ///
    /// This is the normal policy for brush/world materials whose authored UVs
    /// intentionally extend outside `0..=1`, including Source 1 BSP surfaces.
    ///
    /// # Errors
    ///
    /// Returns [`StaticWorldTextureError3d`] under the same conditions as
    /// [`Self::rgba8`].
    pub fn rgba8_repeating(
        cache_key: impl Into<String>,
        width: u32,
        height: u32,
        pixels: Vec<u8>,
    ) -> Result<Self, StaticWorldTextureError3d> {
        let mut sampler = TextureSamplingPreset::HighQuality.sampler();
        sampler.address_mode_u = wgpu::AddressMode::Repeat;
        sampler.address_mode_v = wgpu::AddressMode::Repeat;
        Self::rgba8_with_sampler(cache_key, width, height, pixels, sampler)
    }

    /// Sampler-configurable equivalent of [`Self::rgba8`].
    ///
    /// # Errors
    ///
    /// Returns [`StaticWorldTextureError3d`] when dimensions, bytes or the
    /// cache identity violate the texture upload contract.
    pub fn rgba8_with_sampler(
        cache_key: impl Into<String>,
        width: u32,
        height: u32,
        pixels: Vec<u8>,
        sampler: TextureSampler,
    ) -> Result<Self, StaticWorldTextureError3d> {
        let cache_key = cache_key.into();
        if cache_key.is_empty() {
            return Err(StaticWorldTextureError3d::EmptyCacheKey);
        }
        let size = TextureSize::new(width, height).map_err(StaticWorldTextureError3d::Size)?;
        let metadata = Texture::new(size);
        let prepared = PreparedTextureUpload::rgba8_owned(&metadata, pixels, sampler)
            .map_err(StaticWorldTextureError3d::Prepare)?;
        Ok(Self {
            cache_key,
            metadata,
            prepared,
        })
    }

    /// Stable identity used by the renderer's device-local texture cache.
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

/// Invalid decoded static-world texture input.
#[derive(Debug)]
pub enum StaticWorldTextureError3d {
    /// A cache identity must not be empty.
    EmptyCacheKey,
    /// Width or height was zero.
    Size(TextureSizeError),
    /// RGBA8 validation or mip preparation failed.
    Prepare(TextureUploadError),
}

impl fmt::Display for StaticWorldTextureError3d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyCacheKey => formatter.write_str("static-world texture cache key is empty"),
            Self::Size(source) => write!(formatter, "invalid static-world texture size: {source}"),
            Self::Prepare(source) => {
                write!(
                    formatter,
                    "cannot prepare static-world RGBA8 texture: {source}"
                )
            }
        }
    }
}

impl Error for StaticWorldTextureError3d {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Size(source) => Some(source),
            Self::Prepare(source) => Some(source),
            Self::EmptyCacheKey => None,
        }
    }
}

/// Material decision made while cooking a [`TexturedStaticWorld3d`].
///
/// This explicit policy is important for Source maps: ordinary
/// unresolved materials can remain visible as a factor fallback, while editor
/// surfaces such as `toolsnodraw` must be omitted rather than rendered as
/// opaque fallback geometry.
#[derive(Clone)]
pub enum TexturedStaticWorldMaterial3d {
    /// Draw the material with its factor only.
    Factor([f32; 4]),
    /// Sample a decoded texture and multiply it by this factor.
    Texture {
        /// Shared CPU-prepared source. Cloning the `Arc` does not duplicate mips.
        texture: Arc<StaticWorldTexture3d>,
        /// Linear RGBA material multiplier.
        factor: [f32; 4],
    },
    /// Blend two sampled textures using each vertex's texture-blend weight.
    BlendTextures {
        /// Texture selected at a blend weight of zero.
        first: Arc<StaticWorldTexture3d>,
        /// Texture selected at a blend weight of one.
        second: Arc<StaticWorldTexture3d>,
        /// Linear RGBA material multiplier applied after blending.
        factor: [f32; 4],
    },
    /// Do not include this material in render geometry.
    Skip,
}

impl TexturedStaticWorldMaterial3d {
    /// Uses the model material's own base-colour factor.
    #[must_use]
    pub const fn factor(material: &Material) -> Self {
        Self::Factor(material.base_color_factor())
    }

    /// Uses a texture with the model material's own base-colour factor.
    #[must_use]
    pub fn texture(material: &Material, texture: Arc<StaticWorldTexture3d>) -> Self {
        Self::Texture {
            texture,
            factor: material.base_color_factor(),
        }
    }

    /// Uses two textures blended by [`MeshPrimitive::texture_blend_weights`].
    #[must_use]
    pub fn blend_textures(
        material: &Material,
        first: Arc<StaticWorldTexture3d>,
        second: Arc<StaticWorldTexture3d>,
    ) -> Self {
        Self::BlendTextures {
            first,
            second,
            factor: material.base_color_factor(),
        }
    }
}

/// CPU-cooked immutable world with mixed factor and textured material batches.
///
/// Unlike [`StaticWorld3d`], this type retains normals and UV0 for textured
/// batches and accepts an explicit material policy. It is intentionally a
/// separate type so existing factor-only worlds do not acquire texture assets,
/// bind groups or wider vertex buffers by accident.
pub struct TexturedStaticWorld3d {
    batches: Vec<TexturedStaticWorldBatch3d>,
    stats: TexturedStaticWorldBuildStats3d,
}

enum TexturedStaticWorldBatch3d {
    Factor {
        primitive: MeshPrimitive,
        color: [f32; 4],
        bounds: LocalAabb3d,
    },
    Texture {
        primitive: MeshPrimitive,
        texture: Arc<StaticWorldTexture3d>,
        color: [f32; 4],
        bounds: LocalAabb3d,
    },
    BlendTextures {
        primitive: MeshPrimitive,
        first: Arc<StaticWorldTexture3d>,
        second: Arc<StaticWorldTexture3d>,
        color: [f32; 4],
        bounds: LocalAabb3d,
    },
}

/// Reduction metrics for a mixed static-world cook.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TexturedStaticWorldBuildStats3d {
    /// Source primitive submissions inspected.
    pub source_primitives: usize,
    /// Source triangles inspected, including deliberately skipped materials.
    pub source_triangles: usize,
    /// Resulting factor fallback batches.
    pub factor_batches: usize,
    /// Resulting sampled-texture batches.
    pub textured_batches: usize,
    /// Sampled-texture batches that use per-vertex two-texture blending.
    ///
    /// This is a subset of [`Self::textured_batches`].
    pub blended_batches: usize,
    /// Source primitives omitted by explicit material policy.
    pub skipped_primitives: usize,
}

impl TexturedStaticWorldBuildStats3d {
    /// Total resident geometry batches produced by the cook.
    #[must_use]
    pub const fn batches(self) -> usize {
        self.factor_batches + self.textured_batches
    }
}

/// Source-space edge length of one X/Z static-world culling cell.
///
/// This conservative partition keeps material batching inside a nearby region
/// while allowing the renderer to reject whole GPU meshes outside the camera
/// frustum. Source BSP worlds use this value by default.
pub const SOURCE_BSP_STATIC_WORLD_CELL_SIZE: f32 = 1_024.0;

impl TexturedStaticWorld3d {
    /// Cooks a model using an explicit decision for every referenced material.
    ///
    /// The callback runs once per material slot on first use. Unbound
    /// primitives use an opaque white factor. Textured batches require authored
    /// normals and UV0; the cooker never invents either stream.
    ///
    /// # Errors
    ///
    /// Returns [`TexturedStaticWorldBuildError3d`] for missing material slots,
    /// non-opaque state, missing texture inputs, overflow or invalid combined
    /// geometry.
    pub fn from_model_with_materials(
        model: &Model,
        resolve: impl FnMut(MaterialIndex, &Material) -> TexturedStaticWorldMaterial3d,
    ) -> Result<Self, TexturedStaticWorldBuildError3d> {
        Self::from_model_with_materials_partitioned(model, None, resolve)
    }

    /// Cooks a model into material batches partitioned into X/Z spatial cells.
    ///
    /// Geometry is assigned by triangle centroid, but every resident cell keeps
    /// a tight AABB around its complete triangles. Therefore frustum rejection
    /// never removes a triangle that intersects the camera, including triangles
    /// that cross a cell edge. This is the intended Source BSP path: it trades
    /// one map-wide material mesh for cullable nearby meshes without changing
    /// the source material or texture policy.
    ///
    /// # Errors
    ///
    /// Returns [`TexturedStaticWorldBuildError3d::InvalidSpatialCellSize`] for
    /// an invalid partition edge, plus the same errors as
    /// [`Self::from_model_with_materials`].
    pub fn from_model_with_materials_spatial(
        model: &Model,
        cell_size: f32,
        resolve: impl FnMut(MaterialIndex, &Material) -> TexturedStaticWorldMaterial3d,
    ) -> Result<Self, TexturedStaticWorldBuildError3d> {
        if !cell_size.is_finite() || !(1.0..=65_536.0).contains(&cell_size) {
            return Err(TexturedStaticWorldBuildError3d::InvalidSpatialCellSize);
        }
        Self::from_model_with_materials_partitioned(model, Some(cell_size), resolve)
    }

    fn from_model_with_materials_partitioned(
        model: &Model,
        cell_size: Option<f32>,
        mut resolve: impl FnMut(MaterialIndex, &Material) -> TexturedStaticWorldMaterial3d,
    ) -> Result<Self, TexturedStaticWorldBuildError3d> {
        let mut buckets = Vec::<TexturedBuildBucket>::new();
        let mut bucket_by_material = HashMap::<Option<usize>, usize>::new();
        let mut bucket_by_spatial_key = HashMap::<SpatialBucketKey, usize>::new();
        let mut resolved = HashMap::<usize, TexturedStaticWorldMaterial3d>::new();
        let mut stats = TexturedStaticWorldBuildStats3d::default();

        for (mesh_index, mesh) in model.meshes().iter().enumerate() {
            for (primitive_index, primitive) in mesh.primitives().iter().enumerate() {
                stats.source_primitives += 1;
                stats.source_triangles += primitive.indices().len() / 3;
                let material_key = primitive.material().map(MaterialIndex::get);
                let material = match primitive.material() {
                    Some(index) => {
                        let material = model.materials().get(index.get()).ok_or(
                            TexturedStaticWorldBuildError3d::MissingMaterial {
                                mesh: mesh_index,
                                primitive: primitive_index,
                                material: index,
                            },
                        )?;
                        if material.alpha_mode() != AlphaMode::Opaque {
                            return Err(TexturedStaticWorldBuildError3d::NonOpaqueMaterial {
                                mesh: mesh_index,
                                primitive: primitive_index,
                                material: index,
                                alpha_mode: material.alpha_mode(),
                            });
                        }
                        resolved
                            .entry(index.get())
                            .or_insert_with(|| resolve(index, material))
                            .clone()
                    }
                    None => TexturedStaticWorldMaterial3d::Factor([1.0; 4]),
                };
                if matches!(material, TexturedStaticWorldMaterial3d::Skip) {
                    stats.skipped_primitives += 1;
                    continue;
                }
                if let Some(cell_size) = cell_size {
                    let mut triangles_by_cell = BTreeMap::<SpatialCell3d, Vec<[u32; 3]>>::new();
                    for triangle in primitive.indices().chunks_exact(3) {
                        let triangle = [triangle[0], triangle[1], triangle[2]];
                        let cell = spatial_cell_for_triangle(primitive, triangle, cell_size);
                        triangles_by_cell.entry(cell).or_default().push(triangle);
                    }
                    for (cell, triangles) in triangles_by_cell {
                        let spatial_key = SpatialBucketKey { material: material_key, cell };
                        let bucket_index = if let Some(index) = bucket_by_spatial_key.get(&spatial_key) {
                            *index
                        } else {
                            let index = buckets.len();
                            buckets.push(TexturedBuildBucket::new(material.clone()));
                            bucket_by_spatial_key.insert(spatial_key, index);
                            index
                        };
                        buckets[bucket_index].append_triangles(
                            primitive,
                            &triangles,
                            mesh_index,
                            primitive_index,
                            bucket_index,
                        )?;
                    }
                } else {
                    let bucket_index = if let Some(index) = bucket_by_material.get(&material_key) {
                        *index
                    } else {
                        let index = buckets.len();
                        buckets.push(TexturedBuildBucket::new(material));
                        bucket_by_material.insert(material_key, index);
                        index
                    };
                    buckets[bucket_index].append(
                        primitive,
                        mesh_index,
                        primitive_index,
                        bucket_index,
                    )?;
                }
            }
        }

        let mut batches = Vec::with_capacity(buckets.len());
        for (batch, bucket) in buckets.into_iter().enumerate() {
            let (primitive, material, bounds) = bucket.finish(batch)?;
            match material {
                TexturedStaticWorldMaterial3d::Factor(color) => {
                    stats.factor_batches += 1;
                    batches.push(TexturedStaticWorldBatch3d::Factor { primitive, color, bounds });
                }
                TexturedStaticWorldMaterial3d::Texture { texture, factor } => {
                    stats.textured_batches += 1;
                    batches.push(TexturedStaticWorldBatch3d::Texture {
                        primitive,
                        texture,
                        color: factor,
                        bounds,
                    });
                }
                TexturedStaticWorldMaterial3d::BlendTextures {
                    first,
                    second,
                    factor,
                } => {
                    stats.textured_batches += 1;
                    stats.blended_batches += 1;
                    batches.push(TexturedStaticWorldBatch3d::BlendTextures {
                        primitive,
                        first,
                        second,
                        color: factor,
                        bounds,
                    });
                }
                TexturedStaticWorldMaterial3d::Skip => {
                    unreachable!("skipped materials have no bucket")
                }
            }
        }
        Ok(Self { batches, stats })
    }

    /// CPU cook metrics, including textured/fallback/omitted counts.
    #[must_use]
    pub const fn stats(&self) -> TexturedStaticWorldBuildStats3d {
        self.stats
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct SpatialCell3d {
    x: i32,
    z: i32,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct SpatialBucketKey {
    material: Option<usize>,
    cell: SpatialCell3d,
}

fn spatial_cell_for_triangle(
    primitive: &MeshPrimitive,
    triangle: [u32; 3],
    cell_size: f32,
) -> SpatialCell3d {
    let positions = primitive.positions();
    let [first, second, third] = triangle.map(|index| {
        positions[usize::try_from(index).expect("validated mesh index fits usize")]
    });
    let centre = [
        (first[0] + second[0] + third[0]) / 3.0,
        (first[2] + second[2] + third[2]) / 3.0,
    ];
    SpatialCell3d {
        x: (centre[0] / cell_size).floor() as i32,
        z: (centre[1] / cell_size).floor() as i32,
    }
}

struct TexturedBuildBucket {
    positions: Vec<[f32; 3]>,
    indices: Vec<u32>,
    normals: Vec<[f32; 3]>,
    tex_coords: Vec<[f32; 2]>,
    texture_blend_weights: Vec<f32>,
    material: TexturedStaticWorldMaterial3d,
}

impl TexturedBuildBucket {
    fn new(material: TexturedStaticWorldMaterial3d) -> Self {
        Self {
            positions: Vec::new(),
            indices: Vec::new(),
            normals: Vec::new(),
            tex_coords: Vec::new(),
            texture_blend_weights: Vec::new(),
            material,
        }
    }

    fn append(
        &mut self,
        primitive: &MeshPrimitive,
        mesh: usize,
        primitive_index: usize,
        batch: usize,
    ) -> Result<(), TexturedStaticWorldBuildError3d> {
        let textured = matches!(
            self.material,
            TexturedStaticWorldMaterial3d::Texture { .. }
                | TexturedStaticWorldMaterial3d::BlendTextures { .. }
        );
        let blended = matches!(
            self.material,
            TexturedStaticWorldMaterial3d::BlendTextures { .. }
        );
        let normals =
            if textured {
                Some(primitive.normals().ok_or(
                    TexturedStaticWorldBuildError3d::MissingNormals {
                        mesh,
                        primitive: primitive_index,
                    },
                )?)
            } else {
                None
            };
        let tex_coords = if textured {
            Some(primitive.tex_coords_0().ok_or(
                TexturedStaticWorldBuildError3d::MissingTexCoords0 {
                    mesh,
                    primitive: primitive_index,
                },
            )?)
        } else {
            None
        };
        let texture_blend_weights = if blended {
            Some(primitive.texture_blend_weights().ok_or(
                TexturedStaticWorldBuildError3d::MissingTextureBlendWeights {
                    mesh,
                    primitive: primitive_index,
                },
            )?)
        } else {
            None
        };
        let vertex_base = u32::try_from(self.positions.len())
            .map_err(|_| TexturedStaticWorldBuildError3d::TooManyVertices { batch })?;
        self.positions.extend_from_slice(primitive.positions());
        if let Some(normals) = normals {
            self.normals.extend_from_slice(normals);
        }
        if let Some(tex_coords) = tex_coords {
            self.tex_coords.extend_from_slice(tex_coords);
        }
        if let Some(texture_blend_weights) = texture_blend_weights {
            self.texture_blend_weights
                .extend_from_slice(texture_blend_weights);
        }
        for &index in primitive.indices() {
            self.indices.push(
                vertex_base
                    .checked_add(index)
                    .ok_or(TexturedStaticWorldBuildError3d::TooManyVertices { batch })?,
            );
        }
        Ok(())
    }

    fn append_triangles(
        &mut self,
        primitive: &MeshPrimitive,
        triangles: &[[u32; 3]],
        mesh: usize,
        primitive_index: usize,
        batch: usize,
    ) -> Result<(), TexturedStaticWorldBuildError3d> {
        let textured = matches!(
            self.material,
            TexturedStaticWorldMaterial3d::Texture { .. }
                | TexturedStaticWorldMaterial3d::BlendTextures { .. }
        );
        let blended = matches!(
            self.material,
            TexturedStaticWorldMaterial3d::BlendTextures { .. }
        );
        let normals = if textured {
            Some(primitive.normals().ok_or(
                TexturedStaticWorldBuildError3d::MissingNormals {
                    mesh,
                    primitive: primitive_index,
                },
            )?)
        } else {
            None
        };
        let tex_coords = if textured {
            Some(primitive.tex_coords_0().ok_or(
                TexturedStaticWorldBuildError3d::MissingTexCoords0 {
                    mesh,
                    primitive: primitive_index,
                },
            )?)
        } else {
            None
        };
        let texture_blend_weights = if blended {
            Some(primitive.texture_blend_weights().ok_or(
                TexturedStaticWorldBuildError3d::MissingTextureBlendWeights {
                    mesh,
                    primitive: primitive_index,
                },
            )?)
        } else {
            None
        };
        let mut local_vertices = HashMap::<u32, u32>::new();
        for triangle in triangles {
            for &source_index in triangle {
                let index = if let Some(index) = local_vertices.get(&source_index) {
                    *index
                } else {
                    let source = usize::try_from(source_index)
                        .expect("validated mesh index fits usize");
                    let index = u32::try_from(self.positions.len()).map_err(|_| {
                        TexturedStaticWorldBuildError3d::TooManyVertices { batch }
                    })?;
                    self.positions.push(primitive.positions()[source]);
                    if let Some(normals) = normals {
                        self.normals.push(normals[source]);
                    }
                    if let Some(tex_coords) = tex_coords {
                        self.tex_coords.push(tex_coords[source]);
                    }
                    if let Some(texture_blend_weights) = texture_blend_weights {
                        self.texture_blend_weights.push(texture_blend_weights[source]);
                    }
                    local_vertices.insert(source_index, index);
                    index
                };
                self.indices.push(index);
            }
        }
        Ok(())
    }

    fn finish(
        self,
        batch: usize,
    ) -> Result<(MeshPrimitive, TexturedStaticWorldMaterial3d, LocalAabb3d), TexturedStaticWorldBuildError3d>
    {
        let Self {
            positions,
            indices,
            normals,
            tex_coords,
            texture_blend_weights,
            material,
        } = self;
        let textured = matches!(
            material,
            TexturedStaticWorldMaterial3d::Texture { .. }
                | TexturedStaticWorldMaterial3d::BlendTextures { .. }
        );
        let blended = matches!(
            material,
            TexturedStaticWorldMaterial3d::BlendTextures { .. }
        );
        let bounds = local_aabb_for_positions(&positions);
        let mut primitive = MeshPrimitive::new(positions, indices).map_err(|source| {
            TexturedStaticWorldBuildError3d::CombinedGeometry { batch, source }
        })?;
        if textured {
            primitive = primitive.with_normals(normals).map_err(|source| {
                TexturedStaticWorldBuildError3d::CombinedGeometry { batch, source }
            })?;
            primitive = primitive.with_tex_coords_0(tex_coords).map_err(|source| {
                TexturedStaticWorldBuildError3d::CombinedGeometry { batch, source }
            })?;
        }
        if blended {
            primitive = primitive
                .with_texture_blend_weights(texture_blend_weights)
                .map_err(|source| TexturedStaticWorldBuildError3d::CombinedGeometry {
                    batch,
                    source,
                })?;
        }
        Ok((primitive, material, bounds))
    }
}

fn local_aabb_for_positions(positions: &[[f32; 3]]) -> LocalAabb3d {
    let mut minimum = positions[0];
    let mut maximum = positions[0];
    for position in &positions[1..] {
        for axis in 0..3 {
            minimum[axis] = minimum[axis].min(position[axis]);
            maximum[axis] = maximum[axis].max(position[axis]);
        }
    }
    LocalAabb3d::new(minimum, maximum).expect("validated static-world positions define finite bounds")
}

/// Failure while cooking a texture-aware static world.
#[derive(Debug)]
pub enum TexturedStaticWorldBuildError3d {
    /// Spatial partition edge is not finite or outside the supported range.
    InvalidSpatialCellSize,
    /// A primitive referenced an absent model material slot.
    MissingMaterial {
        /// Source mesh index.
        mesh: usize,
        /// Source primitive index.
        primitive: usize,
        /// Absent material slot.
        material: MaterialIndex,
    },
    /// Textured geometry had no authored normals.
    MissingNormals {
        /// Source mesh index.
        mesh: usize,
        /// Source primitive index.
        primitive: usize,
    },
    /// Textured geometry had no authored primary UV stream.
    MissingTexCoords0 {
        /// Source mesh index.
        mesh: usize,
        /// Source primitive index.
        primitive: usize,
    },
    /// A blended material had no authored per-vertex blend stream.
    MissingTextureBlendWeights {
        /// Source mesh index.
        mesh: usize,
        /// Source primitive index.
        primitive: usize,
    },
    /// A transparent material cannot enter the order-independent world pass.
    NonOpaqueMaterial {
        /// Source mesh index.
        mesh: usize,
        /// Source primitive index.
        primitive: usize,
        /// Source material slot.
        material: MaterialIndex,
        /// Rejected alpha policy.
        alpha_mode: AlphaMode,
    },
    /// One combined batch exceeded `u32` vertex addressing.
    TooManyVertices {
        /// Combined batch index.
        batch: usize,
    },
    /// Combined geometry violated the renderer-neutral model contract.
    CombinedGeometry {
        /// Combined batch index.
        batch: usize,
        /// Validation failure.
        source: yuyib_model::MeshValidationError,
    },
}

impl fmt::Display for TexturedStaticWorldBuildError3d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSpatialCellSize => formatter.write_str(
                "textured static-world spatial cell size must be finite and between 1 and 65536",
            ),
            Self::MissingMaterial {
                mesh,
                primitive,
                material,
            } => write!(
                formatter,
                "textured static world mesh {mesh}, primitive {primitive} references absent material {}",
                material.get()
            ),
            Self::MissingNormals { mesh, primitive } => write!(
                formatter,
                "textured static world mesh {mesh}, primitive {primitive} has no normals"
            ),
            Self::MissingTexCoords0 { mesh, primitive } => write!(
                formatter,
                "textured static world mesh {mesh}, primitive {primitive} has no UV0"
            ),
            Self::MissingTextureBlendWeights { mesh, primitive } => write!(
                formatter,
                "blended static world mesh {mesh}, primitive {primitive} has no texture blend weights"
            ),
            Self::NonOpaqueMaterial {
                mesh,
                primitive,
                material,
                alpha_mode,
            } => write!(
                formatter,
                "textured static world mesh {mesh}, primitive {primitive} uses non-opaque material {} ({alpha_mode:?})",
                material.get()
            ),
            Self::TooManyVertices { batch } => {
                write!(
                    formatter,
                    "textured static world batch {batch} exceeds u32 vertex addressing"
                )
            }
            Self::CombinedGeometry { batch, source } => {
                write!(
                    formatter,
                    "textured static world batch {batch} is invalid: {source}"
                )
            }
        }
    }
}

impl Error for TexturedStaticWorldBuildError3d {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::CombinedGeometry { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Device-local renderer and VTF-derived texture cache for one mixed static world.
pub struct TexturedStaticWorldRenderer3d {
    factor_renderer: MeshRenderer3d,
    textured_renderer: TexturedLitMeshRenderer3d,
    blended_renderer: BlendedStaticWorldRenderer3d,
    texture_assets: Assets<Texture>,
    texture_cache: TextureCache,
    texture_handles: HashMap<String, TextureHandle>,
    batches: Vec<TexturedStaticWorldGpuBatch3d>,
}

enum TexturedStaticWorldGpuBatch3d {
    Factor {
        mesh: GpuMesh,
        color: [f32; 4],
        bounds: LocalAabb3d,
    },
    Texture {
        mesh: GpuTexturedLitMesh,
        material: GpuTexturedLitMaterial,
        color: [f32; 4],
        bounds: LocalAabb3d,
    },
    BlendTextures {
        mesh: GpuBlendedStaticWorldMesh,
        material: GpuBlendedStaticWorldMaterial,
        color: [f32; 4],
        bounds: LocalAabb3d,
    },
}

impl TexturedStaticWorldGpuBatch3d {
    const fn bounds(&self) -> LocalAabb3d {
        match self {
            Self::Factor { bounds, .. }
            | Self::Texture { bounds, .. }
            | Self::BlendTextures { bounds, .. } => *bounds,
        }
    }

    fn triangle_count(&self) -> u64 {
        match self {
            Self::Factor { mesh, .. } => u64::from(mesh.index_count() / 3),
            Self::Texture { mesh, .. } => u64::from(mesh.index_count() / 3),
            Self::BlendTextures { mesh, .. } => u64::from(mesh.index_count / 3),
        }
    }
}

/// GPU upload/cache metrics for [`TexturedStaticWorldRenderer3d`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TexturedStaticWorldUploadStats3d {
    /// Combined GPU meshes uploaded.
    pub batches: usize,
    /// Unique sampled textures uploaded after cache-key deduplication.
    pub unique_textures: usize,
}

impl TexturedStaticWorldRenderer3d {
    /// Creates an empty device-local textured static-world renderer.
    #[must_use]
    pub fn new_for_frame(frame: &RenderFrame<'_>) -> Self {
        Self {
            factor_renderer: MeshRenderer3d::new_for_frame(frame),
            textured_renderer: TexturedLitMeshRenderer3d::new_for_frame(frame),
            blended_renderer: BlendedStaticWorldRenderer3d::new_for_frame(frame, 1),
            texture_assets: Assets::new(),
            texture_cache: TextureCache::new(),
            texture_handles: HashMap::new(),
            batches: Vec::new(),
        }
    }

    /// Uploads geometry, deduplicated textures and persistent material bind groups.
    ///
    /// Replacing a world builds a fresh cache and swaps it in only after every
    /// upload succeeds, so a failed reload leaves the previous world drawable.
    ///
    /// # Errors
    ///
    /// Returns the exact batch or cache key whose upload failed.
    pub fn upload_for_frame(
        &mut self,
        frame: &RenderFrame<'_>,
        world: &TexturedStaticWorld3d,
    ) -> Result<TexturedStaticWorldUploadStats3d, TexturedStaticWorldUploadError3d> {
        let factor_count = world
            .batches
            .iter()
            .filter(|batch| matches!(batch, TexturedStaticWorldBatch3d::Factor { .. }))
            .count();
        let textured_count = world.batches.len() - factor_count;
        let blended_count = world
            .batches
            .iter()
            .filter(|batch| matches!(batch, TexturedStaticWorldBatch3d::BlendTextures { .. }))
            .count();
        let single_textured_count = textured_count - blended_count;
        let factor_renderer = MeshRenderer3d::new_for_frame_with_batch_capacity(
            frame,
            resident_batch_capacity(factor_count, MeshRenderer3d::DEFAULT_BATCH_CAPACITY),
        );
        let textured_renderer = TexturedLitMeshRenderer3d::new_for_frame_with_batch_capacity(
            frame,
            resident_batch_capacity(
                single_textured_count,
                TexturedLitMeshRenderer3d::DEFAULT_BATCH_CAPACITY,
            ),
        );
        let blended_renderer = BlendedStaticWorldRenderer3d::new_for_frame(
            frame,
            resident_batch_capacity(blended_count, 32),
        );
        let mut texture_assets = Assets::new();
        let mut texture_cache = TextureCache::new();
        let mut texture_handles = HashMap::<String, TextureHandle>::new();
        let mut material_cache = HashMap::<String, GpuTexturedLitMaterial>::new();
        let mut blended_material_cache =
            HashMap::<(String, String), GpuBlendedStaticWorldMaterial>::new();
        let mut batches = Vec::with_capacity(world.batches.len());

        for (batch, source) in world.batches.iter().enumerate() {
            match source {
                TexturedStaticWorldBatch3d::Factor {
                    primitive,
                    color,
                    bounds,
                } => {
                    let mesh = factor_renderer
                        .upload_mesh_for_frame(frame, primitive)
                        .map_err(|source| TexturedStaticWorldUploadError3d::FactorMesh {
                            batch,
                            source,
                        })?;
                    batches.push(TexturedStaticWorldGpuBatch3d::Factor {
                        mesh,
                        color: *color,
                        bounds: *bounds,
                    });
                }
                TexturedStaticWorldBatch3d::Texture {
                    primitive,
                    texture,
                    color,
                    bounds,
                } => {
                    let key = texture.cache_key().to_owned();
                    let handle = if let Some(handle) = texture_handles.get(&key).copied() {
                        handle
                    } else {
                        let handle = texture_assets.insert(texture.metadata.clone());
                        texture_cache
                            .upsert_prepared_for_frame(frame, handle, &texture.prepared)
                            .map_err(|source| TexturedStaticWorldUploadError3d::Texture {
                                cache_key: key.clone(),
                                source,
                            })?;
                        texture_handles.insert(key.clone(), handle);
                        handle
                    };
                    let material = if let Some(material) = material_cache.get(&key) {
                        material.clone()
                    } else {
                        let gpu = texture_cache.get(handle).ok_or_else(|| {
                            TexturedStaticWorldUploadError3d::MissingCachedTexture {
                                cache_key: key.clone(),
                            }
                        })?;
                        let material = textured_renderer.upload_material_for_frame(frame, gpu);
                        material_cache.insert(key.clone(), material.clone());
                        material
                    };
                    let mesh = textured_renderer
                        .upload_mesh_for_frame(frame, primitive)
                        .map_err(|source| TexturedStaticWorldUploadError3d::TexturedMesh {
                            batch,
                            source,
                        })?;
                    batches.push(TexturedStaticWorldGpuBatch3d::Texture {
                        mesh,
                        material,
                        color: *color,
                        bounds: *bounds,
                    });
                }
                TexturedStaticWorldBatch3d::BlendTextures {
                    primitive,
                    first,
                    second,
                    color,
                    bounds,
                } => {
                    let first_key = first.cache_key().to_owned();
                    let second_key = second.cache_key().to_owned();
                    let first_handle = cache_static_world_texture(
                        frame,
                        first,
                        &mut texture_assets,
                        &mut texture_cache,
                        &mut texture_handles,
                    )?;
                    let second_handle = cache_static_world_texture(
                        frame,
                        second,
                        &mut texture_assets,
                        &mut texture_cache,
                        &mut texture_handles,
                    )?;
                    let material_key = (first_key.clone(), second_key.clone());
                    let material = if let Some(material) = blended_material_cache.get(&material_key)
                    {
                        material.clone()
                    } else {
                        let first_gpu = texture_cache.get(first_handle).ok_or_else(|| {
                            TexturedStaticWorldUploadError3d::MissingCachedTexture {
                                cache_key: first_key.clone(),
                            }
                        })?;
                        let second_gpu = texture_cache.get(second_handle).ok_or_else(|| {
                            TexturedStaticWorldUploadError3d::MissingCachedTexture {
                                cache_key: second_key.clone(),
                            }
                        })?;
                        let material = blended_renderer
                            .upload_material_for_frame(frame, first_gpu, second_gpu);
                        blended_material_cache.insert(material_key, material.clone());
                        material
                    };
                    let mesh =
                        BlendedStaticWorldRenderer3d::upload_mesh_for_frame(frame, primitive)
                            .map_err(|source| TexturedStaticWorldUploadError3d::BlendedMesh {
                                batch,
                                source,
                            })?;
                    batches.push(TexturedStaticWorldGpuBatch3d::BlendTextures {
                        mesh,
                        material,
                        color: *color,
                        bounds: *bounds,
                    });
                }
            }
        }

        let stats = TexturedStaticWorldUploadStats3d {
            batches: batches.len(),
            unique_textures: texture_cache.len(),
        };
        self.factor_renderer = factor_renderer;
        self.textured_renderer = textured_renderer;
        self.blended_renderer = blended_renderer;
        self.texture_assets = texture_assets;
        self.texture_cache = texture_cache;
        self.texture_handles = texture_handles;
        self.batches = batches;
        Ok(stats)
    }

    /// Number of unique resident sampled textures.
    #[must_use]
    pub fn texture_count(&self) -> usize {
        self.texture_cache.len()
    }

    /// Draws fallback factors and sampled batches in one coherent depth phase.
    ///
    /// Factor batches are submitted first. The first non-empty phase clears
    /// depth and the textured phase loads it. Textured draws reuse cached VTF
    /// textures and bind groups; no per-frame GPU resources are allocated.
    ///
    /// # Errors
    ///
    /// Returns a typed factor or textured rendering failure.
    pub fn draw_for_frame(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        lighting: LambertLighting3d,
    ) -> Result<StaticWorldDrawStats3d, TexturedStaticWorldRenderError3d> {
        let mut stats = StaticWorldDrawStats3d::default();
        let frustum = camera_frustum(frame, camera);
        let mut factors = Vec::new();
        let mut textured = Vec::new();
        let mut blended = Vec::new();
        for batch in &self.batches {
            if !batch_intersects_frustum(batch.bounds(), frustum.as_ref()) {
                stats.culled_batches += 1;
                stats.culled_triangles += batch.triangle_count();
                continue;
            }
            match batch {
                TexturedStaticWorldGpuBatch3d::Factor { mesh, color, .. } => {
                    factors.push((mesh, identity_matrix(), *color));
                }
                TexturedStaticWorldGpuBatch3d::Texture {
                    mesh,
                    material,
                    color,
                    ..
                } => textured.push(TexturedLitBatchDraw::new(
                    mesh,
                    LitMeshInstance3d::new(identity_matrix(), LitMaterial3d::new(*color), lighting),
                    material,
                    true,
                )),
                TexturedStaticWorldGpuBatch3d::BlendTextures {
                    mesh,
                    material,
                    color,
                    ..
                } => blended.push(BlendedStaticWorldBatchDraw {
                    mesh,
                    material,
                    color: *color,
                }),
            }
        }
        let mut has_depth = false;
        if !factors.is_empty() {
            let draw = self
                .factor_renderer
                .draw_batch_depth_clear_double_sided(frame, camera, &factors)
                .map_err(TexturedStaticWorldRenderError3d::Factor)?;
            has_depth = true;
            stats.batches += factors.len();
            stats.triangles += u64::from(draw.triangles);
            stats.draw_calls += u64::from(draw.draw_calls);
        }

        if !textured.is_empty() {
            let depth_load = if has_depth {
                DepthLoad::Load
            } else {
                DepthLoad::Clear
            };
            let draw = self
                .textured_renderer
                .draw_batch_with_depth_load(frame, camera, &textured, depth_load)
                .map_err(TexturedStaticWorldRenderError3d::Textured)?;
            stats.batches += textured.len();
            stats.triangles += u64::from(draw.triangles);
            stats.draw_calls += u64::from(draw.draw_calls);
            has_depth = true;
        }

        if !blended.is_empty() {
            let depth_load = if has_depth {
                DepthLoad::Load
            } else {
                DepthLoad::Clear
            };
            let draw = self
                .blended_renderer
                .draw_batch_with_depth_load(frame, camera, lighting, &blended, depth_load)
                .map_err(TexturedStaticWorldRenderError3d::Blended)?;
            stats.batches += blended.len();
            stats.triangles += u64::from(draw.triangles);
            stats.draw_calls += u64::from(draw.draw_calls);
        }
        Ok(stats)
    }
}

fn camera_frustum(frame: &RenderFrame<'_>, camera: Camera3d) -> Option<Frustum3d> {
    camera
        .view_projection(frame.draw_size())
        .ok()
        .and_then(|matrix| Frustum3d::from_clip_matrix(matrix, ClipDepthRange3d::ZeroToOne).ok())
}

fn batch_intersects_frustum(bounds: LocalAabb3d, frustum: Option<&Frustum3d>) -> bool {
    frustum.map_or(true, |frustum| {
        frustum
            .intersects_local_bounds(LocalBounds3d::Aabb(bounds), identity_matrix())
            .unwrap_or(true)
    })
}

fn cache_static_world_texture(
    frame: &RenderFrame<'_>,
    texture: &Arc<StaticWorldTexture3d>,
    texture_assets: &mut Assets<Texture>,
    texture_cache: &mut TextureCache,
    texture_handles: &mut HashMap<String, TextureHandle>,
) -> Result<TextureHandle, TexturedStaticWorldUploadError3d> {
    let key = texture.cache_key().to_owned();
    if let Some(handle) = texture_handles.get(&key).copied() {
        return Ok(handle);
    }
    let handle = texture_assets.insert(texture.metadata.clone());
    texture_cache
        .upsert_prepared_for_frame(frame, handle, &texture.prepared)
        .map_err(|source| TexturedStaticWorldUploadError3d::Texture {
            cache_key: key.clone(),
            source,
        })?;
    texture_handles.insert(key, handle);
    Ok(handle)
}

struct GpuBlendedStaticWorldMesh {
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
}

#[derive(Clone)]
struct GpuBlendedStaticWorldMaterial {
    bind_group: Arc<wgpu::BindGroup>,
}

struct BlendedStaticWorldBatchDraw<'a> {
    mesh: &'a GpuBlendedStaticWorldMesh,
    material: &'a GpuBlendedStaticWorldMaterial,
    color: [f32; 4],
}

struct BlendedStaticWorldRenderer3d {
    pipeline: wgpu::RenderPipeline,
    camera_uniforms: CameraUniformRing,
    draw_buffer: wgpu::Buffer,
    draw_uniform_stride: u64,
    batch_capacity: usize,
    draw_bind_group: wgpu::BindGroup,
    texture_layout: wgpu::BindGroupLayout,
}

impl BlendedStaticWorldRenderer3d {
    fn new_for_frame(frame: &RenderFrame<'_>, batch_capacity: usize) -> Self {
        Self::create(
            frame.device(),
            frame.surface_format(),
            frame.depth_format(),
            batch_capacity,
        )
    }

    fn upload_material_for_frame(
        &self,
        frame: &RenderFrame<'_>,
        first: &GpuTexture,
        second: &GpuTexture,
    ) -> GpuBlendedStaticWorldMaterial {
        GpuBlendedStaticWorldMaterial {
            bind_group: Arc::new(
                frame
                    .device()
                    .create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("yuyib blended static-world material bind group"),
                        layout: &self.texture_layout,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::TextureView(first.view()),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::Sampler(first.sampler()),
                            },
                            wgpu::BindGroupEntry {
                                binding: 2,
                                resource: wgpu::BindingResource::TextureView(second.view()),
                            },
                            wgpu::BindGroupEntry {
                                binding: 3,
                                resource: wgpu::BindingResource::Sampler(second.sampler()),
                            },
                        ],
                    }),
            ),
        }
    }

    fn upload_mesh_for_frame(
        frame: &RenderFrame<'_>,
        primitive: &MeshPrimitive,
    ) -> Result<GpuBlendedStaticWorldMesh, BlendedStaticWorldMeshUploadError3d> {
        let normals = primitive
            .normals()
            .ok_or(BlendedStaticWorldMeshUploadError3d::MissingNormals)?;
        let tex_coords = primitive
            .tex_coords_0()
            .ok_or(BlendedStaticWorldMeshUploadError3d::MissingTexCoords0)?;
        let blend_weights = primitive
            .texture_blend_weights()
            .ok_or(BlendedStaticWorldMeshUploadError3d::MissingTextureBlendWeights)?;
        let index_count = u32::try_from(primitive.indices().len()).map_err(|_| {
            BlendedStaticWorldMeshUploadError3d::TooManyIndices {
                actual: primitive.indices().len(),
            }
        })?;
        let vertices = primitive
            .positions()
            .iter()
            .copied()
            .zip(normals.iter().copied())
            .zip(tex_coords.iter().copied())
            .zip(blend_weights.iter().copied())
            .enumerate()
            .map(|(index, (((position, normal), tex_coord), blend_weight))| {
                if !position.iter().all(|value| value.is_finite()) {
                    return Err(BlendedStaticWorldMeshUploadError3d::NonFinitePosition { index });
                }
                if !normal.iter().all(|value| value.is_finite()) {
                    return Err(BlendedStaticWorldMeshUploadError3d::NonFiniteNormal { index });
                }
                let normal_length_squared = normal.iter().map(|value| value * value).sum::<f32>();
                if normal_length_squared <= f32::EPSILON {
                    return Err(BlendedStaticWorldMeshUploadError3d::DegenerateNormal { index });
                }
                if !tex_coord.iter().all(|value| value.is_finite()) {
                    return Err(BlendedStaticWorldMeshUploadError3d::NonFiniteTexCoords0 { index });
                }
                if !blend_weight.is_finite() {
                    return Err(
                        BlendedStaticWorldMeshUploadError3d::NonFiniteTextureBlendWeight { index },
                    );
                }
                Ok(BlendedStaticWorldVertex {
                    position,
                    normal,
                    tex_coord,
                    blend_weight,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let device = frame.device();
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("yuyib blended static-world vertices"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("yuyib blended static-world indices"),
            contents: bytemuck::cast_slice(primitive.indices()),
            usage: wgpu::BufferUsages::INDEX,
        });
        Ok(GpuBlendedStaticWorldMesh {
            vertex_buffer,
            index_buffer,
            index_count,
        })
    }

    fn draw_batch_with_depth_load(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        lighting: LambertLighting3d,
        draws: &[BlendedStaticWorldBatchDraw<'_>],
        depth_load: DepthLoad,
    ) -> Result<MeshDrawStats, TexturedLitMeshRenderError> {
        if draws.len() > self.batch_capacity {
            return Err(TexturedLitMeshRenderError::BatchTooLarge {
                actual: draws.len(),
                maximum: self.batch_capacity,
            });
        }
        if draws.is_empty() {
            return Ok(MeshDrawStats::default());
        }
        let view_projection = camera
            .view_projection(frame.draw_size())
            .map_err(TexturedLitMeshRenderError::Mesh)?;
        let uniforms = draws
            .iter()
            .map(|draw| {
                LitMeshUniform::new(identity_matrix(), LitMaterial3d::new(draw.color), lighting)
                    .map_err(TexturedLitMeshRenderError::Lit)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let camera_offset = self
            .camera_uniforms
            .write_for_frame(frame, view_projection)
            .map_err(TexturedLitMeshRenderError::Mesh)?;
        for (index, uniform) in uniforms.iter().enumerate() {
            let offset = u64::try_from(index)
                .expect("batch capacity fits u64")
                .saturating_mul(self.draw_uniform_stride);
            frame
                .queue()
                .write_buffer(&self.draw_buffer, offset, bytemuck::bytes_of(uniform));
        }
        frame.with_surface_pass_with_depth(wgpu::LoadOp::Load, depth_load.operation(), |pass| {
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, self.camera_uniforms.bind_group(), &[camera_offset]);
            for (index, draw) in draws.iter().enumerate() {
                let offset = u64::try_from(index)
                    .expect("batch capacity fits u64")
                    .saturating_mul(self.draw_uniform_stride);
                let offset = u32::try_from(offset).expect("blended static-world offset fits u32");
                pass.set_bind_group(1, &self.draw_bind_group, &[offset]);
                pass.set_bind_group(2, draw.material.bind_group.as_ref(), &[]);
                pass.set_vertex_buffer(0, draw.mesh.vertex_buffer.slice(..));
                pass.set_index_buffer(draw.mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..draw.mesh.index_count, 0, 0..1);
            }
        });
        Ok(MeshDrawStats {
            triangles: draws.iter().map(|draw| draw.mesh.index_count / 3).sum(),
            draw_calls: u32::try_from(draws.len()).expect("batch capacity fits u32"),
            transient_uniform_buffer_allocations: 0,
        })
    }

    fn create(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        depth_format: wgpu::TextureFormat,
        batch_capacity: usize,
    ) -> Self {
        let batch_capacity = batch_capacity.max(1);
        let camera_layout = dynamic_uniform_layout(
            device,
            "yuyib blended static-world camera layout",
            wgpu::ShaderStages::VERTEX,
            size_of::<[f32; 16]>() as u64,
        );
        let draw_layout = dynamic_uniform_layout(
            device,
            "yuyib blended static-world draw layout",
            wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
            size_of::<LitMeshUniform>() as u64,
        );
        let texture_layout = blended_static_world_texture_layout(device);
        let camera_uniforms =
            CameraUniformRing::new(device, "yuyib blended static-world camera", &camera_layout);
        let draw_uniform_stride = aligned_uniform_stride(
            device.limits().min_uniform_buffer_offset_alignment,
            size_of::<LitMeshUniform>() as u64,
        );
        let draw_buffer = uniform_buffer(
            device,
            "yuyib blended static-world draws",
            draw_uniform_stride
                .saturating_mul(u64::try_from(batch_capacity).expect("batch capacity fits u64")),
        );
        let draw_bind_group = dynamic_uniform_bind_group(
            device,
            "yuyib blended static-world draw bind group",
            &draw_layout,
            &draw_buffer,
            size_of::<LitMeshUniform>() as u64,
        );
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("yuyib blended static-world WGSL"),
            source: wgpu::ShaderSource::Wgsl(BLENDED_STATIC_WORLD_WGSL.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("yuyib blended static-world pipeline layout"),
            bind_group_layouts: &[
                Some(&camera_layout),
                Some(&draw_layout),
                Some(&texture_layout),
            ],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("yuyib blended static-world pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(BLENDED_STATIC_WORLD_VERTEX_LAYOUT)],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: depth_format,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
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
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        Self {
            pipeline,
            camera_uniforms,
            draw_buffer,
            draw_uniform_stride,
            batch_capacity,
            draw_bind_group,
            texture_layout,
        }
    }
}

fn blended_static_world_texture_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("yuyib blended static-world texture layout"),
        entries: &[
            sampled_texture_layout_entry(0),
            filtering_sampler_layout_entry(1),
            sampled_texture_layout_entry(2),
            filtering_sampler_layout_entry(3),
        ],
    })
}

const fn sampled_texture_layout_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
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

const fn filtering_sampler_layout_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct BlendedStaticWorldVertex {
    position: [f32; 3],
    normal: [f32; 3],
    tex_coord: [f32; 2],
    blend_weight: f32,
}

const BLENDED_STATIC_WORLD_VERTEX_LAYOUT: wgpu::VertexBufferLayout<'static> =
    wgpu::VertexBufferLayout {
        array_stride: size_of::<BlendedStaticWorldVertex>() as u64,
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
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32,
                offset: size_of::<[f32; 3]>() as u64 * 2 + size_of::<[f32; 2]>() as u64,
                shader_location: 3,
            },
        ],
    };

const BLENDED_STATIC_WORLD_WGSL: &str = r"
struct Camera { view_projection: mat4x4<f32>, };
struct Draw {
    model: mat4x4<f32>, normal_matrix: mat3x3<f32>, base_color: vec4<f32>,
    light_direction: vec4<f32>, light_color: vec4<f32>, ambient: vec4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var<uniform> draw: Draw;
@group(2) @binding(0) var first_texture: texture_2d<f32>;
@group(2) @binding(1) var first_sampler: sampler;
@group(2) @binding(2) var second_texture: texture_2d<f32>;
@group(2) @binding(3) var second_sampler: sampler;
struct VertexInput {
    @location(0) position: vec3<f32>, @location(1) normal: vec3<f32>,
    @location(2) tex_coord: vec2<f32>, @location(3) blend_weight: f32,
};
struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>, @location(0) normal: vec3<f32>,
    @location(1) tex_coord: vec2<f32>, @location(2) blend_weight: f32,
};
@vertex fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    output.clip_position = camera.view_projection * draw.model * vec4<f32>(input.position, 1.0);
    output.normal = normalize(draw.normal_matrix * input.normal);
    output.tex_coord = input.tex_coord;
    output.blend_weight = input.blend_weight;
    return output;
}
@fragment fn fs_main(input: VertexOutput, @builtin(front_facing) front_facing: bool) -> @location(0) vec4<f32> {
    let normal = select(-normalize(input.normal), normalize(input.normal), front_facing);
    let diffuse = max(dot(normal, normalize(-draw.light_direction.xyz)), 0.0);
    let light = draw.ambient.xyz + draw.light_color.xyz * diffuse;
    let first = textureSample(first_texture, first_sampler, input.tex_coord);
    let second = textureSample(second_texture, second_sampler, input.tex_coord);
    let base = mix(first, second, clamp(input.blend_weight, 0.0, 1.0)) * draw.base_color;
    return vec4<f32>(base.rgb * light, base.a);
}
";

/// Failure while uploading a two-texture blended static-world mesh.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlendedStaticWorldMeshUploadError3d {
    /// The source mesh has no normal stream.
    MissingNormals,
    /// The source mesh has no primary UV stream.
    MissingTexCoords0,
    /// The source mesh has no per-vertex texture blend weights.
    MissingTextureBlendWeights,
    /// A position was non-finite.
    NonFinitePosition {
        /// Vertex stream index.
        index: usize,
    },
    /// A normal was non-finite.
    NonFiniteNormal {
        /// Vertex stream index.
        index: usize,
    },
    /// A normal had zero length.
    DegenerateNormal {
        /// Vertex stream index.
        index: usize,
    },
    /// A texture coordinate was non-finite.
    NonFiniteTexCoords0 {
        /// Vertex stream index.
        index: usize,
    },
    /// A texture blend weight was non-finite.
    NonFiniteTextureBlendWeight {
        /// Vertex stream index.
        index: usize,
    },
    /// Index count cannot be represented by WGPU.
    TooManyIndices {
        /// Observed index count.
        actual: usize,
    },
}

impl fmt::Display for BlendedStaticWorldMeshUploadError3d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingNormals => {
                formatter.write_str("blended static-world mesh requires normals")
            }
            Self::MissingTexCoords0 => {
                formatter.write_str("blended static-world mesh requires UV0")
            }
            Self::MissingTextureBlendWeights => {
                formatter.write_str("blended static-world mesh requires texture blend weights")
            }
            Self::NonFinitePosition { index } => write!(
                formatter,
                "blended static-world position at index {index} is not finite"
            ),
            Self::NonFiniteNormal { index } => write!(
                formatter,
                "blended static-world normal at index {index} is not finite"
            ),
            Self::DegenerateNormal { index } => write!(
                formatter,
                "blended static-world normal at index {index} has zero length"
            ),
            Self::NonFiniteTexCoords0 { index } => write!(
                formatter,
                "blended static-world UV0 at index {index} is not finite"
            ),
            Self::NonFiniteTextureBlendWeight { index } => write!(
                formatter,
                "blended static-world texture blend weight at index {index} is not finite"
            ),
            Self::TooManyIndices { actual } => write!(
                formatter,
                "blended static-world mesh has {actual} indices; WGPU accepts at most u32::MAX"
            ),
        }
    }
}

impl Error for BlendedStaticWorldMeshUploadError3d {}

/// Failure while publishing a mixed static world to one GPU device.
#[derive(Debug)]
pub enum TexturedStaticWorldUploadError3d {
    /// Factor geometry upload failed.
    FactorMesh {
        /// Combined batch index.
        batch: usize,
        /// Mesh upload failure.
        source: MeshUploadError,
    },
    /// Textured geometry upload failed.
    TexturedMesh {
        /// Combined batch index.
        batch: usize,
        /// Textured vertex upload failure.
        source: TexturedLitMeshUploadError,
    },
    /// Two-texture blended geometry upload failed.
    BlendedMesh {
        /// Combined batch index.
        batch: usize,
        /// Blended vertex upload failure.
        source: BlendedStaticWorldMeshUploadError3d,
    },
    /// Prepared texture publication failed.
    Texture {
        /// Stable source identity.
        cache_key: String,
        /// GPU texture failure.
        source: TextureUploadError,
    },
    /// An internal cache insertion did not produce a retrievable texture.
    MissingCachedTexture {
        /// Missing stable source identity.
        cache_key: String,
    },
}

impl fmt::Display for TexturedStaticWorldUploadError3d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FactorMesh { batch, source } => {
                write!(
                    formatter,
                    "cannot upload factor static-world batch {batch}: {source}"
                )
            }
            Self::TexturedMesh { batch, source } => write!(
                formatter,
                "cannot upload textured static-world batch {batch}: {source}"
            ),
            Self::BlendedMesh { batch, source } => write!(
                formatter,
                "cannot upload blended static-world batch {batch}: {source}"
            ),
            Self::Texture { cache_key, source } => write!(
                formatter,
                "cannot upload static-world texture {cache_key}: {source}"
            ),
            Self::MissingCachedTexture { cache_key } => write!(
                formatter,
                "static-world texture cache lost freshly uploaded key {cache_key}"
            ),
        }
    }
}

impl Error for TexturedStaticWorldUploadError3d {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::FactorMesh { source, .. } => Some(source),
            Self::TexturedMesh { source, .. } => Some(source),
            Self::BlendedMesh { source, .. } => Some(source),
            Self::Texture { source, .. } => Some(source),
            Self::MissingCachedTexture { .. } => None,
        }
    }
}

/// Failure while recording one mixed static-world frame.
#[derive(Debug)]
pub enum TexturedStaticWorldRenderError3d {
    /// Factor phase failed.
    Factor(MeshRenderError),
    /// Sampled-texture phase failed.
    Textured(TexturedLitMeshRenderError),
    /// Two-texture blended phase failed.
    Blended(TexturedLitMeshRenderError),
}

impl fmt::Display for TexturedStaticWorldRenderError3d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Factor(source) => write!(formatter, "cannot draw factor static world: {source}"),
            Self::Textured(source) => {
                write!(formatter, "cannot draw textured static world: {source}")
            }
            Self::Blended(source) => {
                write!(formatter, "cannot draw blended static world: {source}")
            }
        }
    }
}

impl Error for TexturedStaticWorldRenderError3d {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Factor(source) => Some(source),
            Self::Textured(source) | Self::Blended(source) => Some(source),
        }
    }
}

#[cfg(test)]
mod tests {
    use yuyib_model::{Material, MaterialIndex, Mesh, MeshPrimitive};

    use super::*;

    #[test]
    fn static_world_capacity_grows_during_upload_not_during_draw() {
        assert_eq!(MeshRenderer3d::DEFAULT_BATCH_CAPACITY, 32);
        assert_eq!(resident_batch_capacity(0, 32), 32);
        assert_eq!(resident_batch_capacity(32, 32), 32);
        assert_eq!(resident_batch_capacity(96, 32), 96);
    }

    #[test]
    fn groups_primitives_by_material_without_changing_triangle_count() {
        let first = MeshPrimitive::new(
            vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            vec![0, 1, 2],
        )
        .expect("triangle")
        .with_material(MaterialIndex::new(0));
        let second = MeshPrimitive::new(
            vec![[2.0, 0.0, 0.0], [3.0, 0.0, 0.0], [2.0, 1.0, 0.0]],
            vec![0, 1, 2],
        )
        .expect("triangle")
        .with_material(MaterialIndex::new(0));
        let third = MeshPrimitive::new(
            vec![[4.0, 0.0, 0.0], [5.0, 0.0, 0.0], [4.0, 1.0, 0.0]],
            vec![0, 1, 2],
        )
        .expect("triangle")
        .with_material(MaterialIndex::new(1));
        let model = Model::new(
            vec![Mesh::new(Some("world".to_owned()), vec![first, second, third]).expect("mesh")],
            vec![Material::new(), Material::new()],
            Vec::new(),
        )
        .expect("model");

        let world = StaticWorld3d::from_model(&model).expect("static world");

        assert_eq!(world.stats().source_primitives, 3);
        assert_eq!(world.stats().source_triangles, 3);
        assert_eq!(world.stats().batches, 2);
        assert_eq!(world.batches()[0].primitive().positions().len(), 6);
        assert_eq!(
            world.batches()[0].primitive().indices(),
            &[0, 1, 2, 3, 4, 5]
        );
        assert_eq!(world.batches()[1].primitive().indices(), &[0, 1, 2]);
    }

    fn textured_triangle(material: usize, offset: f32) -> MeshPrimitive {
        MeshPrimitive::new(
            vec![
                [offset, 0.0, 0.0],
                [offset + 1.0, 0.0, 0.0],
                [offset, 1.0, 0.0],
            ],
            vec![0, 1, 2],
        )
        .expect("triangle")
        .with_normals(vec![[0.0, 0.0, 1.0]; 3])
        .expect("normals")
        .with_tex_coords_0(vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]])
        .expect("UV0")
        .with_material(MaterialIndex::new(material))
    }

    fn blended_triangle(material: usize) -> MeshPrimitive {
        textured_triangle(material, 0.0)
            .with_texture_blend_weights(vec![0.0, 0.35, 1.0])
            .expect("blend weights")
    }

    #[test]
    fn textured_world_groups_materials_and_honours_skip_policy() {
        let model = Model::new(
            vec![
                Mesh::new(
                    Some("world".to_owned()),
                    vec![
                        textured_triangle(0, 0.0),
                        textured_triangle(0, 2.0),
                        textured_triangle(1, 4.0),
                        textured_triangle(2, 6.0),
                    ],
                )
                .expect("mesh"),
            ],
            vec![
                Material::new().with_name("wall"),
                Material::new().with_name("missing"),
                Material::new().with_name("tools/toolsnodraw"),
            ],
            Vec::new(),
        )
        .expect("model");
        let texture = Arc::new(
            StaticWorldTexture3d::rgba8("wall.vtf", 1, 1, vec![255, 0, 0, 255]).expect("texture"),
        );

        let world =
            TexturedStaticWorld3d::from_model_with_materials(&model, |index, material| match index
                .get()
            {
                0 => TexturedStaticWorldMaterial3d::texture(material, Arc::clone(&texture)),
                1 => TexturedStaticWorldMaterial3d::factor(material),
                2 => TexturedStaticWorldMaterial3d::Skip,
                _ => unreachable!(),
            })
            .expect("mixed world");

        assert_eq!(world.stats().source_primitives, 4);
        assert_eq!(world.stats().source_triangles, 4);
        assert_eq!(world.stats().textured_batches, 1);
        assert_eq!(world.stats().factor_batches, 1);
        assert_eq!(world.stats().skipped_primitives, 1);
        assert_eq!(world.stats().batches(), 2);
        let TexturedStaticWorldBatch3d::Texture { primitive, .. } = &world.batches[0] else {
            panic!("first material must remain textured");
        };
        assert_eq!(primitive.positions().len(), 6);
        assert_eq!(primitive.tex_coords_0().map(<[_]>::len), Some(6));
        assert_eq!(primitive.normals().map(<[_]>::len), Some(6));
    }

    #[test]
    fn spatial_world_keeps_material_batches_cullable_by_xz_cell() {
        let model = Model::new(
            vec![
                Mesh::new(
                    Some("world".to_owned()),
                    vec![textured_triangle(0, 0.0), textured_triangle(0, 2_048.0)],
                )
                .expect("mesh"),
            ],
            vec![Material::new().with_name("wall")],
            Vec::new(),
        )
        .expect("model");
        let texture = Arc::new(
            StaticWorldTexture3d::rgba8("wall.vtf", 1, 1, vec![255; 4]).expect("texture"),
        );

        let world = TexturedStaticWorld3d::from_model_with_materials_spatial(
            &model,
            SOURCE_BSP_STATIC_WORLD_CELL_SIZE,
            |_, material| TexturedStaticWorldMaterial3d::texture(material, Arc::clone(&texture)),
        )
        .expect("spatial world");

        assert_eq!(world.stats().source_triangles, 2);
        assert_eq!(world.stats().textured_batches, 2);
        assert_eq!(world.stats().batches(), 2);
        let triangles = world
            .batches
            .iter()
            .map(|batch| match batch {
                TexturedStaticWorldBatch3d::Texture { primitive, .. } => {
                    primitive.indices().len() / 3
                }
                _ => 0,
            })
            .sum::<usize>();
        assert_eq!(triangles, 2);
    }

    #[test]
    fn textured_world_rejects_missing_uv_instead_of_inventing_one() {
        let primitive = MeshPrimitive::new(
            vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            vec![0, 1, 2],
        )
        .expect("triangle")
        .with_normals(vec![[0.0, 0.0, 1.0]; 3])
        .expect("normals")
        .with_material(MaterialIndex::new(0));
        let model = Model::new(
            vec![Mesh::new(None, vec![primitive]).expect("mesh")],
            vec![Material::new().with_name("wall")],
            Vec::new(),
        )
        .expect("model");
        let texture =
            Arc::new(StaticWorldTexture3d::rgba8("wall.vtf", 1, 1, vec![255; 4]).expect("texture"));

        let error = match TexturedStaticWorld3d::from_model_with_materials(&model, |_, material| {
            TexturedStaticWorldMaterial3d::texture(material, Arc::clone(&texture))
        }) {
            Ok(_) => panic!("UV0 is required"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            TexturedStaticWorldBuildError3d::MissingTexCoords0 {
                mesh: 0,
                primitive: 0
            }
        ));
    }

    #[test]
    fn textured_world_preserves_two_texture_blend_weights() {
        let model = Model::new(
            vec![Mesh::new(None, vec![blended_triangle(0)]).expect("mesh")],
            vec![Material::new().with_name("nature/blendgrassdirt")],
            Vec::new(),
        )
        .expect("model");
        let grass = Arc::new(
            StaticWorldTexture3d::rgba8_repeating("grass.vtf", 1, 1, vec![0, 255, 0, 255])
                .expect("grass"),
        );
        let dirt = Arc::new(
            StaticWorldTexture3d::rgba8_repeating("dirt.vtf", 1, 1, vec![96, 48, 0, 255])
                .expect("dirt"),
        );

        let world = TexturedStaticWorld3d::from_model_with_materials(&model, |_, material| {
            TexturedStaticWorldMaterial3d::blend_textures(
                material,
                Arc::clone(&grass),
                Arc::clone(&dirt),
            )
        })
        .expect("blended world");

        assert_eq!(world.stats().textured_batches, 1);
        assert_eq!(world.stats().blended_batches, 1);
        assert_eq!(world.stats().batches(), 1);
        let TexturedStaticWorldBatch3d::BlendTextures { primitive, .. } = &world.batches[0] else {
            panic!("material must retain its two-texture pipeline");
        };
        assert_eq!(
            primitive.texture_blend_weights(),
            Some(&[0.0, 0.35, 1.0][..])
        );
    }

    #[test]
    fn blended_world_rejects_missing_blend_stream() {
        let model = Model::new(
            vec![Mesh::new(None, vec![textured_triangle(0, 0.0)]).expect("mesh")],
            vec![Material::new().with_name("nature/blendgrassdirt")],
            Vec::new(),
        )
        .expect("model");
        let first =
            Arc::new(StaticWorldTexture3d::rgba8("first.vtf", 1, 1, vec![255; 4]).expect("first"));
        let second = Arc::new(
            StaticWorldTexture3d::rgba8("second.vtf", 1, 1, vec![255; 4]).expect("second"),
        );

        let error = match TexturedStaticWorld3d::from_model_with_materials(&model, |_, material| {
            TexturedStaticWorldMaterial3d::blend_textures(
                material,
                Arc::clone(&first),
                Arc::clone(&second),
            )
        }) {
            Ok(_) => panic!("blend stream is required"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            TexturedStaticWorldBuildError3d::MissingTextureBlendWeights {
                mesh: 0,
                primitive: 0,
            }
        ));
    }

    #[test]
    fn blended_shader_interpolates_two_sampled_textures() {
        assert!(BLENDED_STATIC_WORLD_WGSL.contains("textureSample(first_texture"));
        assert!(BLENDED_STATIC_WORLD_WGSL.contains("textureSample(second_texture"));
        assert!(BLENDED_STATIC_WORLD_WGSL.contains("mix(first, second"));
        assert!(BLENDED_STATIC_WORLD_WGSL.contains("clamp(input.blend_weight, 0.0, 1.0)"));
    }
}
