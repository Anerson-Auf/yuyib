//! GPU-resident Source 1 `StudioModel` skeletal playback.
//!
//! The Source importer owns binary decoding and animation sampling. This crate
//! bridges its renderer-neutral skin streams and palettes to Yuyib's shared
//! textured skinning pipeline, without routing Source data through glTF types.

#![forbid(unsafe_code)]

use std::{error::Error, fmt};

use yuyib_2d::Texture;
use yuyib_assets::Assets;
use yuyib_model::{AlphaMode, ModelTextureIndex};
use yuyib_model_assets::{
    ModelTextureBindings, ModelTextureLoadError, ModelTextureLoader, PreparedModelTextures,
    PreparedModelTexturesIncomplete,
};
use yuyib_render::RenderFrame;
use yuyib_render_3d::{
    Camera3d, DepthLoad, GpuTexturedSkinnedMesh, LambertLighting3d, ModelUploadBudget3d,
    SkinVertex3d, TexturedSkinnedMaterial3d, TexturedSkinnedMeshRenderError,
    TexturedSkinnedMeshRenderer3d, TexturedSkinnedMeshUploadError,
};
use yuyib_render_texture::TextureCache;
use yuyib_source1::{
    LoadedSource1Model, Source1AnimationError, Source1AnimationPlayer, Source1Pose,
};

/// Source `StudioModel` plus independent texture/mesh GPU residency and playback.
pub struct Source1AnimatedModel3d {
    asset: LoadedSource1Model,
    player: Option<Source1AnimationPlayer>,
    pose: Source1Pose,
    prepared: Option<PreparedModelTextures>,
    texture_assets: Assets<Texture>,
    texture_cache: TextureCache,
    bindings: Option<ModelTextureBindings>,
    renderer: Option<TexturedSkinnedMeshRenderer3d>,
    meshes: Vec<GpuTexturedSkinnedMesh>,
    next_mesh: usize,
    lighting: LambertLighting3d,
}

impl Source1AnimatedModel3d {
    /// Prepares decoded model textures on the CPU. GPU objects are created by
    /// [`Self::prepare_for_frame`] inside a render callback.
    ///
    /// # Errors
    /// Returns texture preparation or initial animation failures.
    pub fn new(
        asset: LoadedSource1Model,
        texture_loader: &ModelTextureLoader,
    ) -> Result<Self, Source1AnimatedModelError> {
        let prepared = texture_loader
            .prepare(asset.model())
            .map_err(Source1AnimatedModelError::Texture)?;
        let player = if asset.animations().clips().is_empty() {
            None
        } else {
            Some(
                Source1AnimationPlayer::new(asset.animations(), 0)
                    .map_err(Source1AnimatedModelError::Animation)?,
            )
        };
        let pose = player.as_ref().map_or_else(
            || asset.animations().skeleton().bind_pose(),
            |player| {
                player
                    .sample(asset.animations())
                    .unwrap_or_else(|_| asset.animations().skeleton().bind_pose())
            },
        );
        Ok(Self {
            asset,
            player,
            pose,
            prepared: Some(prepared),
            texture_assets: Assets::new(),
            texture_cache: TextureCache::new(),
            bindings: None,
            renderer: None,
            meshes: Vec::new(),
            next_mesh: 0,
            lighting: LambertLighting3d::default(),
        })
    }

    /// Replaces character lighting.
    #[must_use]
    pub const fn with_lighting(mut self, lighting: LambertLighting3d) -> Self {
        self.lighting = lighting;
        self
    }

    /// Loaded Source model and animation catalog.
    #[must_use]
    pub const fn asset(&self) -> &LoadedSource1Model {
        &self.asset
    }

    /// Current player, absent when the MDL family declares no sequences.
    #[must_use]
    pub const fn animation(&self) -> Option<&Source1AnimationPlayer> {
        self.player.as_ref()
    }

    /// Mutable playback controls.
    #[must_use]
    pub fn animation_mut(&mut self) -> Option<&mut Source1AnimationPlayer> {
        self.player.as_mut()
    }

    /// Selects a sequence and samples its first frame.
    ///
    /// # Errors
    /// Returns when `clip` is absent or its first pose cannot be sampled.
    pub fn select_animation(&mut self, clip: usize) -> Result<(), Source1AnimatedModelError> {
        match &mut self.player {
            Some(player) => player
                .select(self.asset.animations(), clip)
                .map_err(Source1AnimatedModelError::Animation)?,
            None => {
                self.player = Some(
                    Source1AnimationPlayer::new(self.asset.animations(), clip)
                        .map_err(Source1AnimatedModelError::Animation)?,
                );
            }
        }
        self.sample_pose()
    }

    /// Advances and samples the current sequence.
    ///
    /// # Errors
    /// Returns for an invalid delta or animation sampling failure.
    pub fn advance(&mut self, delta_seconds: f32) -> Result<(), Source1AnimatedModelError> {
        if let Some(player) = &mut self.player {
            player
                .advance(self.asset.animations(), delta_seconds)
                .map_err(Source1AnimatedModelError::Animation)?;
        }
        self.sample_pose()
    }

    fn sample_pose(&mut self) -> Result<(), Source1AnimatedModelError> {
        self.pose = self
            .player
            .as_ref()
            .map_or_else(
                || Ok(self.asset.animations().skeleton().bind_pose()),
                |player| player.sample(self.asset.animations()),
            )
            .map_err(Source1AnimatedModelError::Animation)?;
        Ok(())
    }

    /// Whether all textures and skinned primitives are GPU resident.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.bindings.is_some()
            && self.next_mesh == self.asset.model().meshes().len()
            && self.meshes.len() == self.asset.model().meshes().len()
    }

    /// Publishes textures and skin geometry within a per-frame budget.
    ///
    /// # Errors
    /// Returns for texture residency, missing CPU streams or GPU upload failures.
    pub fn prepare_for_frame(
        &mut self,
        frame: &RenderFrame<'_>,
        budget: ModelUploadBudget3d,
    ) -> Result<bool, Source1AnimatedModelError> {
        if self.bindings.is_none()
            && let Some(prepared) = &mut self.prepared
        {
            prepared
                .upload_with_budget_for_frame(
                    frame,
                    &mut self.texture_assets,
                    &mut self.texture_cache,
                    budget.maximum_texture_slots,
                    budget.target_texture_bytes,
                )
                .map_err(Source1AnimatedModelError::Texture)?;
            if prepared.remaining() == 0 {
                let completed = self
                    .prepared
                    .take()
                    .ok_or(Source1AnimatedModelError::NotReady)?;
                self.bindings = Some(
                    completed
                        .finish()
                        .map_err(Source1AnimatedModelError::PreparedIncomplete)?,
                );
            }
        }
        if self.renderer.is_none() {
            self.renderer = Some(TexturedSkinnedMeshRenderer3d::new_for_frame(frame));
        }
        let renderer = self
            .renderer
            .as_ref()
            .ok_or(Source1AnimatedModelError::NotReady)?;
        let mut uploaded = 0_usize;
        let mut uploaded_bytes = 0_u64;
        while self.next_mesh < self.asset.model().meshes().len()
            && uploaded < budget.maximum_primitives
        {
            let mesh = &self.asset.model().meshes()[self.next_mesh];
            let primitive =
                mesh.primitives()
                    .first()
                    .ok_or(Source1AnimatedModelError::MissingPrimitive {
                        mesh: self.next_mesh,
                    })?;
            let bytes = primitive
                .positions()
                .len()
                .saturating_mul(64)
                .saturating_add(primitive.indices().len().saturating_mul(4))
                as u64;
            if uploaded != 0 && uploaded_bytes.saturating_add(bytes) > budget.target_geometry_bytes
            {
                break;
            }
            let source_skin = self.asset.skin_vertices().get(self.next_mesh).ok_or(
                Source1AnimatedModelError::MissingSkin {
                    mesh: self.next_mesh,
                },
            )?;
            let skin = source_skin
                .iter()
                .map(|vertex| SkinVertex3d::new(vertex.joints(), vertex.weights()))
                .collect::<Vec<_>>();
            self.meshes.push(
                renderer
                    .upload_skin_stream_for_frame(frame, primitive, &skin)
                    .map_err(Source1AnimatedModelError::Upload)?,
            );
            self.next_mesh += 1;
            uploaded += 1;
            uploaded_bytes = uploaded_bytes.saturating_add(bytes);
        }
        Ok(self.is_ready())
    }

    /// Draws opaque/masked primitives first and blended materials afterwards.
    ///
    /// # Errors
    /// Returns until residency is complete, or for invalid material/texture/palette data.
    pub fn draw(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        model_matrix: [f32; 16],
        depth_load: DepthLoad,
    ) -> Result<(), Source1AnimatedModelError> {
        if !self.is_ready() {
            return Err(Source1AnimatedModelError::NotReady);
        }
        let mut next_depth = depth_load;
        for transparent in [false, true] {
            for (mesh_index, (mesh, gpu)) in self
                .asset
                .model()
                .meshes()
                .iter()
                .zip(&self.meshes)
                .enumerate()
            {
                let primitive = mesh
                    .primitives()
                    .first()
                    .ok_or(Source1AnimatedModelError::MissingPrimitive { mesh: mesh_index })?;
                let material_index = primitive
                    .material()
                    .ok_or(Source1AnimatedModelError::MissingMaterial { mesh: mesh_index })?;
                let material = self
                    .asset
                    .model()
                    .materials()
                    .get(material_index.get())
                    .ok_or(Source1AnimatedModelError::MissingMaterial { mesh: mesh_index })?;
                if (material.alpha_mode() == AlphaMode::Blend) != transparent {
                    continue;
                }
                let texture_index = material
                    .base_color_texture()
                    .map(yuyib_model::TextureBinding::texture)
                    .ok_or(Source1AnimatedModelError::MissingBaseTexture { mesh: mesh_index })?;
                let texture = self.texture(texture_index)?;
                let draw_material =
                    TexturedSkinnedMaterial3d::new(texture, material.base_color_factor())
                        .with_double_sided(material.double_sided())
                        .with_alpha_mode(material.alpha_mode())
                        .with_lighting(self.lighting);
                let renderer = self
                    .renderer
                    .as_ref()
                    .ok_or(Source1AnimatedModelError::NotReady)?;
                if transparent {
                    renderer
                        .draw_transparent_palette_with_depth_load(
                            frame,
                            camera,
                            gpu,
                            self.pose.matrices(),
                            model_matrix,
                            draw_material,
                            next_depth,
                        )
                        .map_err(Source1AnimatedModelError::Draw)?;
                } else {
                    renderer
                        .draw_palette_with_depth_load(
                            frame,
                            camera,
                            gpu,
                            self.pose.matrices(),
                            model_matrix,
                            draw_material,
                            next_depth,
                        )
                        .map_err(Source1AnimatedModelError::Draw)?;
                }
                next_depth = DepthLoad::Load;
            }
        }
        Ok(())
    }

    fn texture(
        &self,
        index: ModelTextureIndex,
    ) -> Result<&yuyib_render_texture::GpuTexture, Source1AnimatedModelError> {
        let binding = self
            .bindings
            .as_ref()
            .and_then(|bindings| bindings.get(index))
            .ok_or(Source1AnimatedModelError::MissingTextureBinding { index })?;
        self.texture_cache
            .get(binding.handle())
            .ok_or(Source1AnimatedModelError::MissingGpuTexture { index })
    }
}

/// Source skeletal residency, playback or draw failure.
#[derive(Debug)]
pub enum Source1AnimatedModelError {
    /// Texture decode/upload failed.
    Texture(ModelTextureLoadError),
    /// A completed texture plan was internally incomplete.
    PreparedIncomplete(PreparedModelTexturesIncomplete),
    /// Animation control or sampling failed.
    Animation(Source1AnimationError),
    /// Skin geometry upload failed.
    Upload(TexturedSkinnedMeshUploadError),
    /// Skin draw failed.
    Draw(TexturedSkinnedMeshRenderError),
    /// GPU residency is incomplete.
    NotReady,
    /// Imported mesh has no primitive.
    MissingPrimitive {
        /// Zero-based model mesh index.
        mesh: usize,
    },
    /// Imported mesh has no matching skin stream.
    MissingSkin {
        /// Zero-based model mesh index.
        mesh: usize,
    },
    /// Imported primitive has no material.
    MissingMaterial {
        /// Zero-based model mesh index.
        mesh: usize,
    },
    /// Skinned material has no base texture.
    MissingBaseTexture {
        /// Zero-based model mesh index.
        mesh: usize,
    },
    /// Model texture slot was not resolved.
    MissingTextureBinding {
        /// Renderer-neutral texture slot.
        index: ModelTextureIndex,
    },
    /// Resolved texture handle is not resident.
    MissingGpuTexture {
        /// Renderer-neutral texture slot.
        index: ModelTextureIndex,
    },
}

impl fmt::Display for Source1AnimatedModelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Texture(error) => write!(formatter, "Source model texture: {error}"),
            Self::PreparedIncomplete(error) => write!(formatter, "Source texture plan: {error}"),
            Self::Animation(error) => write!(formatter, "Source animation: {error}"),
            Self::Upload(error) => write!(formatter, "Source skin upload: {error}"),
            Self::Draw(error) => write!(formatter, "Source skin draw: {error}"),
            Self::NotReady => formatter.write_str("Source animated model is not GPU-ready"),
            Self::MissingPrimitive { mesh } => {
                write!(formatter, "Source model mesh {mesh} has no primitive")
            }
            Self::MissingSkin { mesh } => {
                write!(formatter, "Source model mesh {mesh} has no skin stream")
            }
            Self::MissingMaterial { mesh } => {
                write!(formatter, "Source model mesh {mesh} has no material")
            }
            Self::MissingBaseTexture { mesh } => {
                write!(formatter, "Source model mesh {mesh} has no base texture")
            }
            Self::MissingTextureBinding { index } => write!(
                formatter,
                "Source texture slot {} is unresolved",
                index.get()
            ),
            Self::MissingGpuTexture { index } => write!(
                formatter,
                "Source texture slot {} is not GPU-resident",
                index.get()
            ),
        }
    }
}

impl Error for Source1AnimatedModelError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Texture(error) => Some(error),
            Self::PreparedIncomplete(error) => Some(error),
            Self::Animation(error) => Some(error),
            Self::Upload(error) => Some(error),
            Self::Draw(error) => Some(error),
            _ => None,
        }
    }
}
