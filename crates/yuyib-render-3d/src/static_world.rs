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

use std::{collections::HashMap, error::Error, fmt};

use yuyib_model::{AlphaMode, MaterialIndex, MeshPrimitive, Model};
use yuyib_render::RenderFrame;

use crate::{Camera3d, DepthLoad, GpuMesh, MeshRenderError, MeshRenderer3d, MeshUploadError};

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
                let bucket_index = match bucket_by_material.get(&material_key) {
                    Some(index) => *index,
                    None => {
                        let index = buckets.len();
                        buckets.push(BuildBucket::new(color));
                        bucket_by_material.insert(material_key, index);
                        index
                    }
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
        let mut batches = Vec::with_capacity(world.batches().len());
        for (batch, source) in world.batches().iter().enumerate() {
            let mesh = self
                .renderer
                .upload_mesh_for_frame(frame, source.primitive())
                .map_err(|source| StaticWorldUploadError3d { batch, source })?;
            batches.push(StaticWorldGpuBatch3d {
                mesh,
                color: source.color(),
            });
        }
        self.batches = batches;
        Ok(())
    }

    /// Returns the number of currently resident material batches.
    #[must_use]
    pub fn batch_count(&self) -> usize {
        self.batches.len()
    }

    /// Draws the whole opaque world in chunks that respect the dynamic-uniform
    /// capacity of [`MeshRenderer3d`].
    ///
    /// The first chunk clears depth; later chunks keep it, so every batch
    /// participates in one coherent opaque depth phase.
    pub fn draw_for_frame(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
    ) -> Result<StaticWorldDrawStats3d, MeshRenderError> {
        const MAX_BATCH_DRAWS: usize = 1_024;
        let mut stats = StaticWorldDrawStats3d::default();
        for (chunk_index, chunk) in self.batches.chunks(MAX_BATCH_DRAWS).enumerate() {
            let draws: Vec<_> = chunk
                .iter()
                .map(|batch| (&batch.mesh, identity_matrix(), batch.color))
                .collect();
            let draw_stats = if chunk_index == 0 {
                self.renderer
                    .draw_batch_depth_clear_double_sided(frame, camera, &draws)?
            } else {
                self.renderer.draw_batch_with_depth_load_double_sided(
                    frame,
                    camera,
                    &draws,
                    DepthLoad::Load,
                )?
            };
            stats.batches += draws.len();
            stats.triangles += u64::from(draw_stats.triangles);
            stats.draw_calls += u64::from(draw_stats.draw_calls);
        }
        Ok(stats)
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

#[cfg(test)]
mod tests {
    use yuyib_model::{Material, MaterialIndex, Mesh, MeshPrimitive};

    use super::*;

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
}
