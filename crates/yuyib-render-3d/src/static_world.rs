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

use std::{collections::HashMap, error::Error, fmt, sync::Arc};

use yuyib_2d::{Texture, TextureHandle, TextureSize, TextureSizeError};
use yuyib_assets::Assets;
use yuyib_model::{AlphaMode, Material, MaterialIndex, MeshPrimitive, Model};
use yuyib_render::{RenderFrame, wgpu};
use yuyib_render_texture::{
    PreparedTextureUpload, TextureCache, TextureSampler, TextureSamplingPreset, TextureUploadError,
};

use crate::{
    Camera3d, DepthLoad, GpuMesh, GpuTexturedLitMaterial, GpuTexturedLitMesh, LambertLighting3d,
    LitMaterial3d, LitMeshInstance3d, MeshRenderError, MeshRenderer3d, MeshUploadError,
    TexturedLitBatchDraw, TexturedLitMeshRenderError, TexturedLitMeshRenderer3d,
    TexturedLitMeshUploadError,
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
/// This explicit three-way policy is important for Source maps: ordinary
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
    },
    Texture {
        primitive: MeshPrimitive,
        texture: Arc<StaticWorldTexture3d>,
        color: [f32; 4],
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
        mut resolve: impl FnMut(MaterialIndex, &Material) -> TexturedStaticWorldMaterial3d,
    ) -> Result<Self, TexturedStaticWorldBuildError3d> {
        let mut buckets = Vec::<TexturedBuildBucket>::new();
        let mut bucket_by_material = HashMap::<Option<usize>, usize>::new();
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

        let mut batches = Vec::with_capacity(buckets.len());
        for (batch, bucket) in buckets.into_iter().enumerate() {
            let (primitive, material) = bucket.finish(batch)?;
            match material {
                TexturedStaticWorldMaterial3d::Factor(color) => {
                    stats.factor_batches += 1;
                    batches.push(TexturedStaticWorldBatch3d::Factor { primitive, color });
                }
                TexturedStaticWorldMaterial3d::Texture { texture, factor } => {
                    stats.textured_batches += 1;
                    batches.push(TexturedStaticWorldBatch3d::Texture {
                        primitive,
                        texture,
                        color: factor,
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

struct TexturedBuildBucket {
    positions: Vec<[f32; 3]>,
    indices: Vec<u32>,
    normals: Vec<[f32; 3]>,
    tex_coords: Vec<[f32; 2]>,
    material: TexturedStaticWorldMaterial3d,
}

impl TexturedBuildBucket {
    fn new(material: TexturedStaticWorldMaterial3d) -> Self {
        Self {
            positions: Vec::new(),
            indices: Vec::new(),
            normals: Vec::new(),
            tex_coords: Vec::new(),
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
        let textured = matches!(self.material, TexturedStaticWorldMaterial3d::Texture { .. });
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
        let vertex_base = u32::try_from(self.positions.len())
            .map_err(|_| TexturedStaticWorldBuildError3d::TooManyVertices { batch })?;
        self.positions.extend_from_slice(primitive.positions());
        if let Some(normals) = normals {
            self.normals.extend_from_slice(normals);
        }
        if let Some(tex_coords) = tex_coords {
            self.tex_coords.extend_from_slice(tex_coords);
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

    fn finish(
        self,
        batch: usize,
    ) -> Result<(MeshPrimitive, TexturedStaticWorldMaterial3d), TexturedStaticWorldBuildError3d>
    {
        let Self {
            positions,
            indices,
            normals,
            tex_coords,
            material,
        } = self;
        let textured = matches!(material, TexturedStaticWorldMaterial3d::Texture { .. });
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
        Ok((primitive, material))
    }
}

/// Failure while cooking a texture-aware static world.
#[derive(Debug)]
pub enum TexturedStaticWorldBuildError3d {
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
    texture_assets: Assets<Texture>,
    texture_cache: TextureCache,
    texture_handles: HashMap<String, TextureHandle>,
    batches: Vec<TexturedStaticWorldGpuBatch3d>,
}

enum TexturedStaticWorldGpuBatch3d {
    Factor {
        mesh: GpuMesh,
        color: [f32; 4],
    },
    Texture {
        mesh: GpuTexturedLitMesh,
        material: GpuTexturedLitMaterial,
        color: [f32; 4],
    },
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
        let factor_renderer = MeshRenderer3d::new_for_frame_with_batch_capacity(
            frame,
            resident_batch_capacity(factor_count, MeshRenderer3d::DEFAULT_BATCH_CAPACITY),
        );
        let textured_renderer = TexturedLitMeshRenderer3d::new_for_frame_with_batch_capacity(
            frame,
            resident_batch_capacity(
                textured_count,
                TexturedLitMeshRenderer3d::DEFAULT_BATCH_CAPACITY,
            ),
        );
        let mut texture_assets = Assets::new();
        let mut texture_cache = TextureCache::new();
        let mut texture_handles = HashMap::<String, TextureHandle>::new();
        let mut material_cache = HashMap::<String, GpuTexturedLitMaterial>::new();
        let mut batches = Vec::with_capacity(world.batches.len());

        for (batch, source) in world.batches.iter().enumerate() {
            match source {
                TexturedStaticWorldBatch3d::Factor { primitive, color } => {
                    let mesh = factor_renderer
                        .upload_mesh_for_frame(frame, primitive)
                        .map_err(|source| TexturedStaticWorldUploadError3d::FactorMesh {
                            batch,
                            source,
                        })?;
                    batches.push(TexturedStaticWorldGpuBatch3d::Factor {
                        mesh,
                        color: *color,
                    });
                }
                TexturedStaticWorldBatch3d::Texture {
                    primitive,
                    texture,
                    color,
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
        let factors: Vec<_> = self
            .batches
            .iter()
            .filter_map(|batch| match batch {
                TexturedStaticWorldGpuBatch3d::Factor { mesh, color } => {
                    Some((mesh, identity_matrix(), *color))
                }
                TexturedStaticWorldGpuBatch3d::Texture { .. } => None,
            })
            .collect();
        let mut stats = StaticWorldDrawStats3d::default();
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

        let textured: Vec<_> = self
            .batches
            .iter()
            .filter_map(|batch| match batch {
                TexturedStaticWorldGpuBatch3d::Texture {
                    mesh,
                    material,
                    color,
                } => Some(TexturedLitBatchDraw::new(
                    mesh,
                    LitMeshInstance3d::new(identity_matrix(), LitMaterial3d::new(*color), lighting),
                    material,
                    true,
                )),
                TexturedStaticWorldGpuBatch3d::Factor { .. } => None,
            })
            .collect();
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
        }
        Ok(stats)
    }
}

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
}

impl fmt::Display for TexturedStaticWorldRenderError3d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Factor(source) => write!(formatter, "cannot draw factor static world: {source}"),
            Self::Textured(source) => {
                write!(formatter, "cannot draw textured static world: {source}")
            }
        }
    }
}

impl Error for TexturedStaticWorldRenderError3d {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Factor(source) => Some(source),
            Self::Textured(source) => Some(source),
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
}
