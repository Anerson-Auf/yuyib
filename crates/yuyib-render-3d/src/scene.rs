//! High-level ECS 3D scene composition over the explicit renderer slices.

use std::{collections::HashMap, error::Error, fmt, path::Path};

use yuyib_2d::Texture;
use yuyib_assets::Assets;
use yuyib_ecs::prelude::World;
use yuyib_game_3d::{
    ClipDepthRange3d, ComputeModelBoundsError3d, Frustum3d, Frustum3dError, FrustumCullingError3d,
    FrustumCullingStats3d, LodSelectionError, MeshFrustumCullingError3d, MeshFrustumCullingStats3d,
    ModelBoundsRegistry3d, TransformHierarchyError, extract_directional_lights, extract_models,
    extract_models_with_lod_3d, filter_extracted_model_meshes_by_frustum_3d,
    propagate_world_transforms, register_computed_model_bounds_3d,
};
use yuyib_model::{AlphaMode, Material, Model, ModelHandle};
use yuyib_model_assets::{
    ModelTextureBindings, ModelTextureLoadError, ModelTextureLoader, ModelTextureLoaderInitError,
    PreparedModelTextures, PreparedModelTexturesIncomplete, PreparedTextureUploadStats,
    TextureAlphaSummary,
};
use yuyib_render::RenderFrame;
use yuyib_render_texture::TextureCache;

use crate::{
    BaseColorSceneRenderError, BaseColorSceneRenderer3d, Camera3d, GpuTexturedPbrMaterial,
    GpuTexturedPbrMesh, LambertLighting3d, LambertLightingError, LitSceneRenderError,
    LitSceneRenderer3d, ModelUploadBudget3d, ModelUploadProgress3d, PbrAlphaMode3d, PbrLighting3d,
    PbrMaterial3d, PbrMaterialError, PbrMeshRenderError, PbrMeshRenderer3d,
    PbrTextureCoordinateSets3d, PbrTexturePresence3d, SceneDrawStats, TexturedPbrBatchDraw,
    TexturedPbrMeshRenderer3d, TexturedPbrMeshUploadError,
};

/// Standard high-level shading route for a [`Game3dScene`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Game3dShading {
    /// Base-colour factors/textures without lighting.
    Unlit,
    /// Base-colour factors/textures with one directional Lambert light.
    #[default]
    Lambert,
    /// Metallic/roughness Cook-Torrance lighting, including the standard glTF
    /// base-colour + normal + metallic/roughness texture workflow.
    Pbr,
}

/// High-level policy for exporter-authored `BLEND` materials.
///
/// Strict mode preserves the source declaration. The default policy promotes
/// only textures that contain no meaningfully transparent pixels and are at
/// least 99% alpha 254/255. This avoids unstable transparent sorting for
/// effectively opaque walls while retaining real glass and cutouts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PbrBlendPolicy3d {
    promote_effectively_opaque: bool,
    minimum_alpha: u8,
    minimum_opaque_coverage_per_mille: u16,
}

impl PbrBlendPolicy3d {
    /// Preserves every glTF `BLEND` declaration exactly.
    #[must_use]
    pub const fn strict() -> Self {
        Self {
            promote_effectively_opaque: false,
            minimum_alpha: 0,
            minimum_opaque_coverage_per_mille: 1_000,
        }
    }

    /// Creates a configurable effectively-opaque classifier.
    ///
    /// `minimum_opaque_coverage_per_mille` is in `0..=1000`; alpha samples of
    /// 254 and 255 count towards that coverage.
    ///
    /// # Errors
    ///
    /// Returns [`PbrBlendPolicyError3d`] when coverage exceeds 1000.
    pub const fn effectively_opaque(
        minimum_alpha: u8,
        minimum_opaque_coverage_per_mille: u16,
    ) -> Result<Self, PbrBlendPolicyError3d> {
        if minimum_opaque_coverage_per_mille > 1_000 {
            return Err(PbrBlendPolicyError3d::CoverageOutOfRange);
        }
        Ok(Self {
            promote_effectively_opaque: true,
            minimum_alpha,
            minimum_opaque_coverage_per_mille,
        })
    }

    fn promotes(self, base_factor_alpha: f32, alpha: TextureAlphaSummary) -> bool {
        if !self.promote_effectively_opaque
            || (base_factor_alpha - 1.0).abs() > f32::EPSILON
            || alpha.total_pixels() == 0
            || alpha.minimum() < self.minimum_alpha
        {
            return false;
        }
        u128::from(alpha.pixels_at_least_254()).saturating_mul(1_000)
            >= u128::from(alpha.total_pixels())
                .saturating_mul(u128::from(self.minimum_opaque_coverage_per_mille))
    }

    fn promotes_factor(self, base_factor_alpha: f32) -> bool {
        self.promote_effectively_opaque && (base_factor_alpha - 1.0).abs() <= f32::EPSILON
    }
}

impl Default for PbrBlendPolicy3d {
    fn default() -> Self {
        Self::effectively_opaque(242, 990).expect("built-in PBR blend thresholds are valid")
    }
}

/// Invalid effectively-opaque classification thresholds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PbrBlendPolicyError3d {
    /// Per-mille coverage must not exceed 1000.
    CoverageOutOfRange,
}

impl fmt::Display for PbrBlendPolicyError3d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PBR opaque alpha coverage must be in 0..=1000 per mille")
    }
}

impl Error for PbrBlendPolicyError3d {}

/// Selection policy for the single directional light supported by Lambert.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Game3dLighting {
    /// Always use this validated artistic light.
    Fixed(LambertLighting3d),
    /// PBR-only fixed direct light plus L2 SH diffuse irradiance.
    ///
    /// Lambert scenes intentionally keep using [`Self::Fixed`] and do not
    /// evaluate SH.
    FixedPbr(PbrLighting3d),
    /// Use the first deterministic ECS directional light, or `fallback` when absent.
    FirstDirectional {
        /// Linear ambient contribution paired with an ECS light.
        ambient: [f32; 3],
        /// Light used when the ECS snapshot contains no enabled directional light.
        fallback: LambertLighting3d,
    },
}

impl Default for Game3dLighting {
    fn default() -> Self {
        Self::FirstDirectional {
            ambient: [0.08; 3],
            fallback: LambertLighting3d::default(),
        }
    }
}

/// How high-level scene rendering treats primitives without a material binding.
///
/// Prefer repairing assets with [`yuyib_model::ModelMaterialPolicy::with_unbound_primitive_fallback`]
/// before GPU publication. This policy is the last-resort renderer contract when
/// an unbound primitive still reaches the draw path.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum UnboundMaterialPolicy3d {
    /// Fail with a structured unbound-material error.
    ///
    /// This is the default: silent white `Material::default()` substitution is
    /// not allowed on the high-level route.
    #[default]
    Error,
    /// Draw an obvious magenta debug material instead of failing.
    ///
    /// Useful while diagnosing imports; not a shipping material policy.
    DebugMagenta,
}

impl UnboundMaterialPolicy3d {
    pub(crate) fn resolve<'a>(self, material: Option<&'a Material>) -> Result<&'a Material, ()> {
        match material {
            Some(material) => Ok(material),
            None => match self {
                Self::Error => Err(()),
                Self::DebugMagenta => Ok(debug_unbound_material()),
            },
        }
    }
}

fn debug_unbound_material() -> &'static Material {
    use std::sync::OnceLock;
    static MATERIAL: OnceLock<Material> = OnceLock::new();
    MATERIAL.get_or_init(|| {
        Material::new()
            .with_name("yuyib.debug.unbound_material")
            .with_base_color_factor([1.0, 0.0, 1.0, 1.0])
            .with_metallic_roughness(0.0, 0.45)
            .with_emissive_factor([0.55, 0.0, 0.55])
            .with_double_sided(true)
    })
}

/// Policies for high-level 3D extraction and rendering.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Game3dSceneConfig {
    camera: Camera3d,
    shading: Game3dShading,
    lighting: Game3dLighting,
    propagate_hierarchy: bool,
    select_lod: bool,
    visible_model_limit: usize,
    pbr_blend_policy: PbrBlendPolicy3d,
    frustum_culling: bool,
    unbound_material_policy: UnboundMaterialPolicy3d,
}

impl Game3dSceneConfig {
    /// Creates the default scene policy with a positive visible-model bound.
    ///
    /// # Errors
    ///
    /// Returns [`Game3dSceneConfigError::ZeroVisibleModelLimit`] for zero.
    pub fn new(visible_model_limit: usize) -> Result<Self, Game3dSceneConfigError> {
        if visible_model_limit == 0 {
            return Err(Game3dSceneConfigError::ZeroVisibleModelLimit);
        }
        Ok(Self {
            camera: Camera3d::default(),
            shading: Game3dShading::default(),
            lighting: Game3dLighting::default(),
            propagate_hierarchy: true,
            select_lod: true,
            visible_model_limit,
            pbr_blend_policy: PbrBlendPolicy3d::default(),
            frustum_culling: true,
            unbound_material_policy: UnboundMaterialPolicy3d::Error,
        })
    }

    /// Replaces the initial camera.
    #[must_use]
    pub const fn with_camera(mut self, camera: Camera3d) -> Self {
        self.camera = camera;
        self
    }

    /// Selects the unlit, Lambert or PBR route.
    #[must_use]
    pub const fn with_shading(mut self, shading: Game3dShading) -> Self {
        self.shading = shading;
        self
    }

    /// Replaces the Lambert light-selection policy.
    #[must_use]
    pub const fn with_lighting(mut self, lighting: Game3dLighting) -> Self {
        self.lighting = lighting;
        self
    }

    /// Enables or disables hierarchy propagation immediately before extraction.
    #[must_use]
    pub const fn with_hierarchy_propagation(mut self, enabled: bool) -> Self {
        self.propagate_hierarchy = enabled;
        self
    }

    /// Enables or disables camera-distance selection through `LodGroup3d`.
    #[must_use]
    pub const fn with_lod_selection(mut self, enabled: bool) -> Self {
        self.select_lod = enabled;
        self
    }

    /// Replaces high-level PBR classification of exporter-authored blends.
    #[must_use]
    pub const fn with_pbr_blend_policy(mut self, policy: PbrBlendPolicy3d) -> Self {
        self.pbr_blend_policy = policy;
        self
    }

    /// Selects how unbound primitives are handled on the high-level draw path.
    #[must_use]
    pub const fn with_unbound_material_policy(mut self, policy: UnboundMaterialPolicy3d) -> Self {
        self.unbound_material_policy = policy;
        self
    }

    /// Enables or disables CPU frustum filtering before draw submission.
    #[must_use]
    pub const fn with_frustum_culling(mut self, enabled: bool) -> Self {
        self.frustum_culling = enabled;
        self
    }

    /// Returns the hard visible-model limit.
    #[must_use]
    pub const fn visible_model_limit(self) -> usize {
        self.visible_model_limit
    }
}

impl Default for Game3dSceneConfig {
    fn default() -> Self {
        Self::new(65_536).expect("the built-in visible-model limit is positive")
    }
}

/// Invalid high-level 3D scene policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Game3dSceneConfigError {
    /// Extraction would never accept a visible model.
    ZeroVisibleModelLimit,
}

impl fmt::Display for Game3dSceneConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Game3dScene visible model limit must be non-zero")
    }
}

impl Error for Game3dSceneConfigError {}

/// High-level owner of 3D camera, extraction policy, model cache and standard shading.
pub struct Game3dScene {
    config: Game3dSceneConfig,
    texture_loader: ModelTextureLoader,
    unlit: Option<BaseColorSceneRenderer3d>,
    lambert: Option<LitSceneRenderer3d>,
    pbr: Option<PbrSceneRenderer>,
    pending_prepared_models: HashMap<ModelHandle, PreparedModelTextures>,
    model_bounds: ModelBoundsRegistry3d,
    /// CPU-prefiltered specular environment applied on the next PBR GPU upload.
    pending_specular_environment: Option<crate::PreparedSpecularIbl3d>,
    /// Cubemap skybox applied on the next PBR GPU upload.
    pending_skybox: Option<crate::PreparedSkybox3d>,
    /// Camera-follow directional shadow policy applied once the PBR renderer exists.
    pending_shadow_policy: Option<crate::DirectionalShadowPolicy>,
    /// SSAO policy applied after opaque PBR.
    ssao_policy: Option<crate::SsaoPolicy>,
    ssao: Option<crate::ssao::GpuSsao>,
}

impl Game3dScene {
    /// Creates a scene whose external model textures are confined below `asset_root`.
    ///
    /// # Errors
    ///
    /// Returns [`ModelTextureLoaderInitError`] when the canonical asset root is
    /// missing or not a directory.
    pub fn new(
        asset_root: impl AsRef<Path>,
        config: Game3dSceneConfig,
    ) -> Result<Self, ModelTextureLoaderInitError> {
        Ok(Self {
            config,
            texture_loader: ModelTextureLoader::new(asset_root)?,
            unlit: None,
            lambert: None,
            pbr: None,
            pending_prepared_models: HashMap::new(),
            model_bounds: ModelBoundsRegistry3d::new(),
            pending_specular_environment: None,
            pending_skybox: None,
            pending_shadow_policy: None,
            ssao_policy: None,
            ssao: None,
        })
    }

    /// Queues a prefiltered specular environment for the PBR path.
    ///
    /// Upload happens on the next PBR prepare/render frame. Pair with
    /// [`PbrLighting3d::with_specular_ibl_strength`] so the split-sum term is
    /// actually scaled above zero.
    #[must_use]
    pub fn with_specular_environment(
        mut self,
        prepared: crate::PreparedSpecularIbl3d,
    ) -> Self {
        self.pending_specular_environment = Some(prepared);
        if let Some(renderer) = &mut self.pbr {
            renderer.specular_ibl = None;
        }
        self
    }

    /// Replaces the active prefiltered specular environment.
    pub fn set_specular_environment(&mut self, prepared: crate::PreparedSpecularIbl3d) {
        self.pending_specular_environment = Some(prepared);
        if let Some(renderer) = &mut self.pbr {
            renderer.specular_ibl = None;
        }
    }

    /// Queues a cubemap skybox for the PBR path (drawn after opaque, before transparent).
    ///
    /// Upload happens on the next PBR prepare/render frame. Pair with
    /// [`Self::with_specular_environment`] when the sky should match reflections.
    #[must_use]
    pub fn with_skybox(mut self, prepared: crate::PreparedSkybox3d) -> Self {
        self.pending_skybox = Some(prepared);
        if let Some(renderer) = &mut self.pbr {
            renderer.skybox = None;
        }
        self
    }

    /// Replaces the active cubemap skybox.
    pub fn set_skybox(&mut self, prepared: crate::PreparedSkybox3d) {
        self.pending_skybox = Some(prepared);
        if let Some(renderer) = &mut self.pbr {
            renderer.skybox = None;
        }
    }

    /// Queues the same cooked specular pack for reflections **and** skybox mip0.
    ///
    /// # Errors
    ///
    /// Returns [`crate::SkyboxError`] when mip0 cannot be extracted.
    pub fn with_environment(
        self,
        prepared: crate::PreparedSpecularIbl3d,
    ) -> Result<Self, crate::SkyboxError> {
        let sky = crate::PreparedSkybox3d::from_specular_mip0(&prepared)?;
        Ok(self.with_specular_environment(prepared).with_skybox(sky))
    }

    /// Enables camera-follow directional shadows on the PBR path.
    #[must_use]
    pub fn with_directional_shadow(mut self, policy: crate::DirectionalShadowPolicy) -> Self {
        self.pending_shadow_policy = Some(policy);
        if let Some(renderer) = &mut self.pbr {
            renderer.shadow_policy = Some(policy);
            renderer.directional_shadow = None;
        }
        self
    }

    /// Enables half-resolution SSAO after opaque PBR (multiplies HDR/LDR colour).
    #[must_use]
    pub fn with_ssao(mut self, policy: crate::SsaoPolicy) -> Self {
        self.ssao_policy = Some(policy);
        self.ssao = None;
        self
    }

    /// Replaces the active SSAO policy.
    pub fn set_ssao(&mut self, policy: Option<crate::SsaoPolicy>) {
        self.ssao_policy = policy;
        self.ssao = None;
    }

    /// Replaces the active directional shadow policy.
    pub fn set_directional_shadow(&mut self, policy: crate::DirectionalShadowPolicy) {
        self.pending_shadow_policy = Some(policy);
        if let Some(renderer) = &mut self.pbr {
            renderer.shadow_policy = Some(policy);
            renderer.directional_shadow = None;
        }
    }

    /// Returns the mutable active camera.
    #[must_use]
    pub const fn camera_mut(&mut self) -> &mut Camera3d {
        &mut self.config.camera
    }

    /// Returns the currently selected standard shading route.
    #[must_use]
    pub const fn shading(&self) -> Game3dShading {
        self.config.shading
    }

    /// Selects the shading route used by the next frame.
    pub const fn set_shading(&mut self, shading: Game3dShading) {
        self.config.shading = shading;
    }

    /// Replaces the Lambert light policy.
    pub const fn set_lighting(&mut self, lighting: Game3dLighting) {
        self.config.lighting = lighting;
    }

    /// Queues worker-decoded model textures for bounded Lambert or PBR publication.
    ///
    /// The prepared value contains no WGPU objects and may be produced off the
    /// render thread. Actual texture and mesh creation remains in
    /// [`Self::prepare_model_for_frame`]. Re-queueing replaces the previous
    /// not-yet-published value deterministically.
    pub fn queue_prepared_model(&mut self, model: ModelHandle, prepared: PreparedModelTextures) {
        if let Some(renderer) = &mut self.lambert {
            renderer.invalidate_model(model);
        }
        if let Some(renderer) = &mut self.pbr {
            renderer.invalidate_model(model);
        }
        self.pending_prepared_models.insert(model, prepared);
    }

    /// Merges worker-computed local bounds used by high-level frustum culling.
    pub fn extend_model_bounds(&mut self, bounds: ModelBoundsRegistry3d) {
        self.model_bounds.extend(bounds);
    }

    /// Returns texture slots still awaiting bounded Lambert or PBR upload.
    #[must_use]
    pub fn prepared_model_remaining(&self, model: ModelHandle) -> Option<usize> {
        self.pending_prepared_models
            .get(&model)
            .map(PreparedModelTextures::remaining)
            .or_else(|| {
                self.lambert
                    .as_ref()
                    .and_then(|renderer| renderer.prepared_model_remaining(model))
            })
            .or_else(|| {
                self.pbr
                    .as_ref()
                    .and_then(|renderer| renderer.prepared_model_remaining(model))
            })
    }

    /// Publishes a bounded number of worker-prepared texture slots.
    ///
    /// Returns `true` when the model is fully resident and may be passed to
    /// [`Self::render`] without synchronous image decode. This first streaming
    /// slice supports both standard Lambert and PBR routes with route-specific
    /// material bindings and one shared budget/progress contract.
    ///
    /// # Errors
    ///
    /// Returns [`Game3dSceneError::PreparedShadingUnsupported`] for unlit
    /// shading, or a structured Lambert/PBR upload/material error.
    pub fn prepare_model_for_frame(
        &mut self,
        frame: &RenderFrame<'_>,
        models: &Assets<Model>,
        model: ModelHandle,
        maximum_texture_slots: usize,
    ) -> Result<bool, Game3dSceneError> {
        self.prepare_model_for_frame_with_budget(
            frame,
            models,
            model,
            ModelUploadBudget3d {
                maximum_texture_slots,
                target_texture_bytes: u64::MAX,
                maximum_primitives: usize::MAX,
                target_geometry_bytes: u64::MAX,
            },
        )
        .map(|progress| progress.ready)
    }

    /// Publishes worker-prepared textures and geometry within one explicit budget.
    ///
    /// This is the configurable counterpart to [`Self::prepare_model_for_frame`].
    /// The returned counters are suitable for a loading screen and remain
    /// stable across the texture-to-geometry phase transition.
    ///
    /// # Errors
    ///
    /// Returns [`Game3dSceneError::PreparedShadingUnsupported`] for unlit
    /// shading, or a structured Lambert/PBR upload/material error.
    pub fn prepare_model_for_frame_with_budget(
        &mut self,
        frame: &RenderFrame<'_>,
        models: &Assets<Model>,
        model: ModelHandle,
        budget: ModelUploadBudget3d,
    ) -> Result<ModelUploadProgress3d, Game3dSceneError> {
        match self.config.shading {
            Game3dShading::Lambert => {
                let renderer = self.lambert.get_or_insert_with(|| {
                    LitSceneRenderer3d::new_for_frame(frame, self.texture_loader.clone())
                        .with_unbound_material_policy(self.config.unbound_material_policy)
                });
                if let Some(prepared) = self.pending_prepared_models.remove(&model) {
                    renderer.queue_prepared_model(model, prepared);
                }
                renderer
                    .prepare_model_for_frame_with_budget(frame, models, model, budget)
                    .map_err(Game3dSceneError::Lambert)
            }
            Game3dShading::Pbr => {
                let renderer = self.pbr.get_or_insert_with(|| {
                    PbrSceneRenderer::new_for_frame(
                        frame,
                        self.texture_loader.clone(),
                        self.config.pbr_blend_policy,
                        self.config.unbound_material_policy,
                    )
                });
                if let Some(prepared) = self.pending_prepared_models.remove(&model) {
                    renderer.queue_prepared_model(model, prepared);
                }
                renderer
                    .prepare_model_for_frame_with_budget(frame, models, model, budget)
                    .map_err(Game3dSceneError::Pbr)
            }
            Game3dShading::Unlit => Err(Game3dSceneError::PreparedShadingUnsupported {
                shading: self.config.shading,
            }),
        }
    }

    /// Invalidates one model in every initialized shading cache.
    pub fn invalidate_model(&mut self, model: yuyib_model::ModelHandle) -> bool {
        let bounds = self.model_bounds.remove_model(model) != 0;
        let pending = self.pending_prepared_models.remove(&model).is_some();
        let unlit = self
            .unlit
            .as_mut()
            .is_some_and(|renderer| renderer.invalidate_model(model));
        let lambert = self
            .lambert
            .as_mut()
            .is_some_and(|renderer| renderer.invalidate_model(model));
        let pbr = self
            .pbr
            .as_mut()
            .is_some_and(|renderer| renderer.invalidate_model(model));
        pending || unlit || lambert || pbr || bounds
    }

    /// Releases every standard model/texture cache owned by this scene.
    pub fn clear_model_caches(&mut self) {
        self.pending_prepared_models.clear();
        self.model_bounds = ModelBoundsRegistry3d::new();
        if let Some(renderer) = &mut self.unlit {
            renderer.clear_model_cache();
        }
        if let Some(renderer) = &mut self.lambert {
            renderer.clear_model_cache();
        }
        if let Some(renderer) = &mut self.pbr {
            renderer.clear_model_cache();
        }
    }

    /// Propagates hierarchy state, extracts the ECS scene and renders the selected route.
    ///
    /// # Errors
    ///
    /// Returns [`Game3dSceneError`] for invalid hierarchy/camera/light data, a
    /// visible-model budget violation, model texture/import failures or GPU draw failures.
    pub fn render(
        &mut self,
        frame: &mut RenderFrame<'_>,
        world: &mut World,
        models: &Assets<Model>,
    ) -> Result<Game3dSceneStats, Game3dSceneError> {
        if self.config.propagate_hierarchy {
            propagate_world_transforms(world)?;
        }
        let mut extracted = if self.config.select_lod {
            extract_models_with_lod_3d(world, self.config.camera.position)?
        } else {
            extract_models(world)
        };
        if extracted.model_count() > self.config.visible_model_limit {
            return Err(Game3dSceneError::VisibleModelLimitExceeded {
                maximum: self.config.visible_model_limit,
                actual: extracted.model_count(),
            });
        }
        let view_projection = self.config.camera.view_projection(frame.draw_size())?;
        let mut frustum = FrustumCullingStats3d {
            input_draws: extracted.model_count(),
            visible_draws: extracted.model_count(),
            ..FrustumCullingStats3d::default()
        };
        let mut mesh_frustum = MeshFrustumCullingStats3d::default();
        if self.config.frustum_culling {
            let (visible, draw_stats, mesh_stats) =
                self.filter_visible_meshes(models, &extracted, view_projection)?;
            frustum = draw_stats;
            mesh_frustum = mesh_stats;
            extracted = visible;
        }
        let (scene_extracted, overlay_extracted) = extracted.partition_overlay();
        let (draw, directional_lights, used_scene_light) = match self.config.shading {
            Game3dShading::Unlit => {
                let renderer = self.unlit.get_or_insert_with(|| {
                    BaseColorSceneRenderer3d::new_for_frame(frame, self.texture_loader.clone())
                });
                let mut draw =
                    renderer.draw_for_frame(frame, self.config.camera, models, &scene_extracted)?;
                if !overlay_extracted.is_empty() {
                    let overlay = renderer.draw_for_frame(
                        frame,
                        self.config.camera,
                        models,
                        &overlay_extracted,
                    )?;
                    accumulate_scene_draw_stats(&mut draw, overlay);
                }
                (draw, 0, false)
            }
            Game3dShading::Lambert => {
                let (lighting, directional_lights, used_scene_light) =
                    self.resolve_lighting(world)?;
                let renderer = self.lambert.get_or_insert_with(|| {
                    LitSceneRenderer3d::new_for_frame(frame, self.texture_loader.clone())
                        .with_unbound_material_policy(self.config.unbound_material_policy)
                });
                let mut draw = renderer.draw_for_frame(
                    frame,
                    self.config.camera,
                    lighting,
                    models,
                    &scene_extracted,
                )?;
                if !overlay_extracted.is_empty() {
                    let overlay = renderer.draw_for_frame(
                        frame,
                        self.config.camera,
                        lighting,
                        models,
                        &overlay_extracted,
                    )?;
                    accumulate_scene_draw_stats(&mut draw, overlay);
                }
                (draw, directional_lights, used_scene_light)
            }
            Game3dShading::Pbr => {
                let (lighting, directional_lights, used_scene_light) =
                    self.resolve_pbr_lighting(world)?;
                let pending_env = self.pending_specular_environment.take();
                let pending_sky = self.pending_skybox.take();
                let pending_shadow = self.pending_shadow_policy.take();
                let renderer = self.pbr.get_or_insert_with(|| {
                    PbrSceneRenderer::new_for_frame(
                        frame,
                        self.texture_loader.clone(),
                        self.config.pbr_blend_policy,
                        self.config.unbound_material_policy,
                    )
                });
                if let Some(prepared) = pending_env {
                    renderer.specular_ibl =
                        Some(crate::GpuSpecularIbl3d::upload_for_frame(frame, &prepared));
                }
                if let Some(prepared) = pending_sky {
                    renderer.skybox =
                        Some(crate::GpuSkybox3d::upload_for_frame(frame, &prepared));
                }
                if let Some(policy) = pending_shadow {
                    renderer.shadow_policy = Some(policy);
                    renderer.directional_shadow = None;
                }
                let mut draw = renderer.draw_for_frame(
                    frame,
                    self.config.camera,
                    lighting,
                    models,
                    &scene_extracted,
                    None,
                    true,
                )?;
                if !overlay_extracted.is_empty() {
                    let overlay = renderer.draw_for_frame(
                        frame,
                        self.config.camera,
                        lighting,
                        models,
                        &overlay_extracted,
                        None,
                        false,
                    )?;
                    accumulate_scene_draw_stats(&mut draw, overlay);
                }
                (draw, directional_lights, used_scene_light)
            }
        };
        if matches!(self.config.shading, Game3dShading::Pbr) {
            if let Some(policy) = self.ssao_policy {
                let full = frame.surface_size();
                let half_w = (full[0] / 2).max(1);
                let half_h = (full[1] / 2).max(1);
                let recreate = self.ssao.as_ref().is_none_or(|ssao| {
                    !ssao.matches(half_w, half_h, frame.surface_format())
                });
                if recreate {
                    self.ssao = Some(crate::ssao::GpuSsao::new(frame));
                }
                if let Some(ssao) = &self.ssao {
                    ssao.encode(frame, self.config.camera, policy)
                        .map_err(Game3dSceneError::Camera)?;
                }
            }
        }
        Ok(Game3dSceneStats {
            visible_models: frustum.visible_draws,
            directional_lights,
            used_scene_light,
            shading: self.config.shading,
            frustum,
            mesh_frustum,
            draw,
        })
    }

    fn filter_visible_meshes(
        &mut self,
        models: &Assets<Model>,
        extracted: &yuyib_game_3d::ExtractedModels,
        view_projection: [f32; 16],
    ) -> Result<
        (
            yuyib_game_3d::ExtractedModels,
            FrustumCullingStats3d,
            MeshFrustumCullingStats3d,
        ),
        Game3dSceneError,
    > {
        for batch in extracted.batches() {
            if !self.model_bounds.contains_model(batch.model()) {
                register_computed_model_bounds_3d(&mut self.model_bounds, models, batch.model())?;
            }
        }
        let camera_frustum =
            Frustum3d::from_clip_matrix(view_projection, ClipDepthRange3d::ZeroToOne)?;
        filter_extracted_model_meshes_by_frustum_3d(
            extracted,
            &camera_frustum,
            models,
            &self.model_bounds,
        )
        .map(yuyib_game_3d::MeshFrustumCullingResult3d::into_parts)
        .map_err(Game3dSceneError::from)
    }

    fn resolve_lighting(
        &self,
        world: &mut World,
    ) -> Result<(LambertLighting3d, usize, bool), Game3dSceneError> {
        match self.config.lighting {
            Game3dLighting::Fixed(lighting) => Ok((lighting, 0, false)),
            Game3dLighting::FixedPbr(lighting) => Ok((lighting.direct(), 0, false)),
            Game3dLighting::FirstDirectional { ambient, fallback } => {
                let lights = extract_directional_lights(world);
                let Some(light) = lights.lights().first().copied() else {
                    return Ok((fallback, 0, false));
                };
                Ok((LambertLighting3d::new(light, ambient)?, lights.len(), true))
            }
        }
    }

    fn resolve_pbr_lighting(
        &self,
        world: &mut World,
    ) -> Result<(PbrLighting3d, usize, bool), Game3dSceneError> {
        if let Game3dLighting::FixedPbr(lighting) = self.config.lighting {
            return Ok((lighting, 0, false));
        }
        let (lighting, count, from_scene) = self.resolve_lighting(world)?;
        Ok((lighting.into(), count, from_scene))
    }
}

fn accumulate_scene_draw_stats(into: &mut SceneDrawStats, extra: SceneDrawStats) {
    into.model_instances += extra.model_instances;
    into.primitive_draws += extra.primitive_draws;
    into.triangles += extra.triangles;
    into.draw_calls += extra.draw_calls;
    into.cache_misses += extra.cache_misses;
    into.render_passes += extra.render_passes;
    into.material_bind_group_creations += extra.material_bind_group_creations;
    into.promoted_blend_draws += extra.promoted_blend_draws;
    into.transient_uniform_buffer_allocations += extra.transient_uniform_buffer_allocations;
}

/// Diagnostics returned by one high-level 3D scene frame.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Game3dSceneStats {
    /// Visible ECS model entities submitted to the standard renderer.
    pub visible_models: usize,
    /// Enabled directional lights observed when the scene-light policy ran.
    pub directional_lights: usize,
    /// Whether the selected light came from ECS rather than the fallback.
    pub used_scene_light: bool,
    /// Active standard shading route.
    pub shading: Game3dShading,
    /// Renderer-neutral frustum filtering counters for this frame.
    pub frustum: FrustumCullingStats3d,
    /// Physical source-mesh visibility and bounds-quality counters.
    ///
    /// These remain zero when CPU frustum culling is disabled.
    pub mesh_frustum: MeshFrustumCullingStats3d,
    /// Lower-level GPU/cache counters.
    pub draw: SceneDrawStats,
}

impl Game3dSceneStats {
    /// Compact one-line summary for consoles and editor overlays.
    #[must_use]
    pub fn summary_line(&self) -> String {
        format!(
            "shading={:?} visible={}/{} culled={} lights={} {} mesh_visible={}",
            self.shading,
            self.frustum.visible_draws,
            self.frustum.input_draws,
            self.frustum.culled_draws,
            self.directional_lights,
            self.draw.summary_line(),
            self.mesh_frustum.visible_meshes,
        )
    }
}

/// Failure in high-level 3D extraction, standard material residency or drawing.
#[derive(Debug)]
pub enum Game3dSceneError {
    /// Scene hierarchy propagation failed transactionally.
    Hierarchy(TransformHierarchyError),
    /// Camera-distance LOD selection failed.
    Lod(LodSelectionError),
    /// CPU model-local bounds generation failed.
    Bounds(ComputeModelBoundsError3d),
    /// Camera frustum extraction failed.
    Frustum(Frustum3dError),
    /// Projecting one model bound into the frustum failed.
    FrustumCulling(FrustumCullingError3d),
    /// Expanding or projecting one physical model mesh failed.
    MeshFrustumCulling(MeshFrustumCullingError3d),
    /// More visible model entities were extracted than permitted.
    VisibleModelLimitExceeded {
        /// Configured maximum.
        maximum: usize,
        /// Observed visible entity count.
        actual: usize,
    },
    /// Prepared texture publication is not implemented for this shading route.
    PreparedShadingUnsupported {
        /// Active shading route.
        shading: Game3dShading,
    },
    /// A streamed model cannot change material route during or after publication.
    PreparedShadingChanged {
        /// Route which owns the prepared/resident resources.
        prepared: Game3dShading,
        /// Route requested by the caller now.
        current: Game3dShading,
    },
    /// The camera cannot produce a valid projection.
    Camera(crate::MeshRenderError),
    /// An ECS directional light and ambient term could not form Lambert input.
    Lighting(LambertLightingError),
    /// Unlit base-colour rendering failed.
    Unlit(BaseColorSceneRenderError),
    /// Lambert standard rendering failed.
    Lambert(LitSceneRenderError),
    /// Factor-only PBR scene rendering failed.
    Pbr(PbrSceneRenderError),
}

impl fmt::Display for Game3dSceneError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hierarchy(error) => write!(formatter, "3D hierarchy propagation failed: {error}"),
            Self::Lod(error) => write!(formatter, "3D LOD selection failed: {error}"),
            Self::Bounds(error) => write!(formatter, "3D model bounds failed: {error}"),
            Self::Frustum(error) => write!(formatter, "3D camera frustum failed: {error}"),
            Self::FrustumCulling(error) => write!(formatter, "3D frustum culling failed: {error}"),
            Self::MeshFrustumCulling(error) => {
                write!(formatter, "3D per-mesh frustum culling failed: {error}")
            }
            Self::VisibleModelLimitExceeded { maximum, actual } => write!(
                formatter,
                "3D scene contains {actual} visible models; configured maximum is {maximum}"
            ),
            Self::PreparedShadingUnsupported { shading } => write!(
                formatter,
                "bounded prepared-model publication is unavailable for {shading:?} shading"
            ),
            Self::PreparedShadingChanged { prepared, current } => write!(
                formatter,
                "streamed model publication started with {prepared:?} shading and cannot continue with {current:?}; load a separate scene for the other residency route"
            ),
            Self::Camera(error) => write!(formatter, "3D camera is invalid: {error}"),
            Self::Lighting(error) => write!(formatter, "3D lighting is invalid: {error}"),
            Self::Unlit(error) => write!(formatter, "unlit 3D scene failed: {error}"),
            Self::Lambert(error) => write!(formatter, "Lambert 3D scene failed: {error}"),
            Self::Pbr(error) => write!(formatter, "PBR 3D scene failed: {error}"),
        }
    }
}

impl Error for Game3dSceneError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Hierarchy(error) => Some(error),
            Self::Lod(error) => Some(error),
            Self::Bounds(error) => Some(error),
            Self::Frustum(error) => Some(error),
            Self::FrustumCulling(error) => Some(error),
            Self::MeshFrustumCulling(error) => Some(error),
            Self::Camera(error) => Some(error),
            Self::Lighting(error) => Some(error),
            Self::Unlit(error) => Some(error),
            Self::Lambert(error) => Some(error),
            Self::Pbr(error) => Some(error),
            Self::VisibleModelLimitExceeded { .. }
            | Self::PreparedShadingUnsupported { .. }
            | Self::PreparedShadingChanged { .. } => None,
        }
    }
}

impl From<TransformHierarchyError> for Game3dSceneError {
    fn from(value: TransformHierarchyError) -> Self {
        Self::Hierarchy(value)
    }
}

impl From<LodSelectionError> for Game3dSceneError {
    fn from(value: LodSelectionError) -> Self {
        Self::Lod(value)
    }
}

impl From<ComputeModelBoundsError3d> for Game3dSceneError {
    fn from(value: ComputeModelBoundsError3d) -> Self {
        Self::Bounds(value)
    }
}

impl From<Frustum3dError> for Game3dSceneError {
    fn from(value: Frustum3dError) -> Self {
        Self::Frustum(value)
    }
}

impl From<FrustumCullingError3d> for Game3dSceneError {
    fn from(value: FrustumCullingError3d) -> Self {
        Self::FrustumCulling(value)
    }
}

impl From<MeshFrustumCullingError3d> for Game3dSceneError {
    fn from(value: MeshFrustumCullingError3d) -> Self {
        Self::MeshFrustumCulling(value)
    }
}

impl From<crate::MeshRenderError> for Game3dSceneError {
    fn from(value: crate::MeshRenderError) -> Self {
        Self::Camera(value)
    }
}

impl From<LambertLightingError> for Game3dSceneError {
    fn from(value: LambertLightingError) -> Self {
        Self::Lighting(value)
    }
}

impl From<BaseColorSceneRenderError> for Game3dSceneError {
    fn from(value: BaseColorSceneRenderError) -> Self {
        Self::Unlit(value)
    }
}

impl From<LitSceneRenderError> for Game3dSceneError {
    fn from(value: LitSceneRenderError) -> Self {
        Self::Lambert(value)
    }
}

impl From<PbrSceneRenderError> for Game3dSceneError {
    fn from(value: PbrSceneRenderError) -> Self {
        Self::Pbr(value)
    }
}

struct PbrSceneRenderer {
    factor_renderer: PbrMeshRenderer3d,
    textured_renderer: TexturedPbrMeshRenderer3d,
    texture_loader: ModelTextureLoader,
    texture_assets: Assets<Texture>,
    texture_cache: TextureCache,
    models: HashMap<ModelHandle, PbrGpuModel>,
    prepared_models: HashMap<ModelHandle, PreparedModelTextures>,
    preparing_models: HashMap<ModelHandle, PbrGpuModelUpload>,
    blend_policy: PbrBlendPolicy3d,
    unbound_material_policy: UnboundMaterialPolicy3d,
    specular_ibl: Option<crate::GpuSpecularIbl3d>,
    skybox_renderer: crate::SkyboxRenderer3d,
    skybox: Option<crate::GpuSkybox3d>,
    shadow_policy: Option<crate::DirectionalShadowPolicy>,
    shadow_caster: crate::DirectionalShadowCaster3d,
    directional_shadow: Option<crate::GpuDirectionalShadow>,
}

struct PbrGpuModel {
    meshes: Vec<Vec<PbrGpuPrimitive>>,
    textures: ModelTextureBindings,
    texture_slots: usize,
    material_bind_group_creations: u64,
    geometry_bytes: u64,
}

struct PbrGpuModelUpload {
    meshes: Vec<Vec<PbrGpuPrimitive>>,
    source_primitive_counts: Vec<usize>,
    textures: ModelTextureBindings,
    texture_slots: usize,
    next_mesh: usize,
    next_primitive: usize,
    completed_primitives: usize,
    total_primitives: usize,
    completed_geometry_bytes: u64,
    total_geometry_bytes: u64,
    material_bind_group_creations: u64,
}

impl PbrGpuModelUpload {
    fn new(model: &Model, textures: ModelTextureBindings) -> Self {
        let texture_slots = textures.len();
        let (total_primitives, total_geometry_bytes) = crate::model_geometry_totals(model);
        Self {
            meshes: model
                .meshes()
                .iter()
                .map(|mesh| Vec::with_capacity(mesh.primitives().len()))
                .collect(),
            source_primitive_counts: model
                .meshes()
                .iter()
                .map(|mesh| mesh.primitives().len())
                .collect(),
            textures,
            texture_slots,
            next_mesh: 0,
            next_primitive: 0,
            completed_primitives: 0,
            total_primitives,
            completed_geometry_bytes: 0,
            total_geometry_bytes,
            material_bind_group_creations: 0,
        }
    }

    fn matches(&self, model: &Model) -> bool {
        model.meshes().len() == self.meshes.len()
            && model
                .meshes()
                .iter()
                .zip(&self.source_primitive_counts)
                .all(|(mesh, expected)| mesh.primitives().len() == *expected)
            && crate::model_geometry_totals(model).1 == self.total_geometry_bytes
    }

    fn progress(&self, uploaded_oversized_primitive: bool) -> ModelUploadProgress3d {
        ModelUploadProgress3d {
            ready: self.completed_primitives == self.total_primitives,
            completed_texture_slots: self.texture_slots,
            total_texture_slots: self.texture_slots,
            completed_primitives: self.completed_primitives,
            total_primitives: self.total_primitives,
            completed_geometry_bytes: self.completed_geometry_bytes,
            total_geometry_bytes: self.total_geometry_bytes,
            uploaded_oversized_primitive,
            ..ModelUploadProgress3d::default()
        }
    }

    fn finish(self) -> PbrGpuModel {
        PbrGpuModel {
            meshes: self.meshes,
            textures: self.textures,
            texture_slots: self.texture_slots,
            material_bind_group_creations: self.material_bind_group_creations,
            geometry_bytes: self.total_geometry_bytes,
        }
    }
}

enum PbrGpuPrimitive {
    Factor {
        mesh: crate::GpuPbrMesh,
        material: PbrMaterial3d,
        double_sided: bool,
        promoted_from_blend: bool,
    },
    Textured {
        mesh: GpuTexturedPbrMesh,
        binding: GpuTexturedPbrMaterial,
        material: PbrMaterial3d,
        normal_scale: f32,
        double_sided: bool,
        transparent: bool,
        local_center: [f32; 3],
        promoted_from_blend: bool,
    },
}

#[derive(Clone, Copy)]
struct FactorPbrRequest {
    model: ModelHandle,
    mesh_index: usize,
    primitive_index: usize,
    model_matrix: [f32; 16],
}

#[derive(Clone, Copy)]
struct TexturedPbrRequest {
    model: ModelHandle,
    mesh_index: usize,
    primitive_index: usize,
    model_matrix: [f32; 16],
    camera_distance_squared: f32,
}

impl PbrSceneRenderer {
    fn new_for_frame(
        frame: &RenderFrame<'_>,
        texture_loader: ModelTextureLoader,
        blend_policy: PbrBlendPolicy3d,
        unbound_material_policy: UnboundMaterialPolicy3d,
    ) -> Self {
        Self {
            factor_renderer: PbrMeshRenderer3d::new_for_frame(frame),
            textured_renderer: TexturedPbrMeshRenderer3d::new_for_frame(frame),
            texture_loader,
            texture_assets: Assets::new(),
            texture_cache: TextureCache::new(),
            models: HashMap::new(),
            prepared_models: HashMap::new(),
            preparing_models: HashMap::new(),
            blend_policy,
            unbound_material_policy,
            specular_ibl: None,
            skybox_renderer: crate::SkyboxRenderer3d::new_for_frame(frame),
            skybox: None,
            shadow_policy: None,
            shadow_caster: crate::DirectionalShadowCaster3d::new_for_frame(frame),
            directional_shadow: None,
        }
    }

    fn queue_prepared_model(&mut self, model: ModelHandle, prepared: PreparedModelTextures) {
        self.invalidate_model(model);
        if let Some(previous) = self.prepared_models.insert(model, prepared) {
            previous.release(&mut self.texture_assets, &mut self.texture_cache);
        }
    }

    fn prepared_model_remaining(&self, model: ModelHandle) -> Option<usize> {
        self.prepared_models
            .get(&model)
            .map(PreparedModelTextures::remaining)
    }

    fn invalidate_model(&mut self, model: ModelHandle) -> bool {
        let prepared = self.prepared_models.remove(&model).map(|prepared| {
            prepared.release(&mut self.texture_assets, &mut self.texture_cache);
        });
        let preparing = self.preparing_models.remove(&model).map(|upload| {
            upload
                .textures
                .release(&mut self.texture_assets, &mut self.texture_cache);
        });
        let Some(cached) = self.models.remove(&model) else {
            return prepared.is_some() || preparing.is_some();
        };
        cached
            .textures
            .release(&mut self.texture_assets, &mut self.texture_cache);
        true
    }

    fn clear_model_cache(&mut self) {
        for (_, prepared) in self.prepared_models.drain() {
            prepared.release(&mut self.texture_assets, &mut self.texture_cache);
        }
        for (_, upload) in self.preparing_models.drain() {
            upload
                .textures
                .release(&mut self.texture_assets, &mut self.texture_cache);
        }
        for (_, cached) in self.models.drain() {
            cached
                .textures
                .release(&mut self.texture_assets, &mut self.texture_cache);
        }
    }

    fn model_upload_progress(
        &self,
        models: &Assets<Model>,
        model: ModelHandle,
    ) -> Result<ModelUploadProgress3d, PbrSceneRenderError> {
        if let Some(cached) = self.models.get(&model) {
            let total_primitives = cached.meshes.iter().map(Vec::len).sum();
            return Ok(ModelUploadProgress3d {
                ready: true,
                completed_texture_slots: cached.texture_slots,
                total_texture_slots: cached.texture_slots,
                completed_primitives: total_primitives,
                total_primitives,
                completed_geometry_bytes: cached.geometry_bytes,
                total_geometry_bytes: cached.geometry_bytes,
                ..ModelUploadProgress3d::default()
            });
        }
        if let Some(upload) = self.preparing_models.get(&model) {
            return Ok(upload.progress(false));
        }
        let source = models
            .get(model)
            .ok_or(PbrSceneRenderError::MissingModel(model))?;
        let (total_primitives, total_geometry_bytes) = crate::model_geometry_totals(source);
        let (completed_texture_slots, total_texture_slots) = self
            .prepared_models
            .get(&model)
            .map_or((0, source.textures().len()), |prepared| {
                (
                    prepared.len().saturating_sub(prepared.remaining()),
                    prepared.len(),
                )
            });
        Ok(ModelUploadProgress3d {
            completed_texture_slots,
            total_texture_slots,
            total_primitives,
            total_geometry_bytes,
            ..ModelUploadProgress3d::default()
        })
    }

    #[allow(
        clippy::too_many_lines,
        reason = "PBR texture-to-geometry transition and rollback form one residency transaction."
    )]
    fn prepare_model_for_frame_with_budget(
        &mut self,
        frame: &RenderFrame<'_>,
        models: &Assets<Model>,
        model: ModelHandle,
        budget: ModelUploadBudget3d,
    ) -> Result<ModelUploadProgress3d, PbrSceneRenderError> {
        if self.models.contains_key(&model) {
            return self.model_upload_progress(models, model);
        }
        let Some(source) = models.get(model) else {
            self.invalidate_model(model);
            return Err(PbrSceneRenderError::MissingModel(model));
        };
        let mut texture_upload = PreparedTextureUploadStats::default();
        if !self.preparing_models.contains_key(&model) && !self.prepared_models.contains_key(&model)
        {
            if source.textures().is_empty() {
                self.preparing_models.insert(
                    model,
                    PbrGpuModelUpload::new(source, ModelTextureBindings::default()),
                );
            } else {
                return Err(PbrSceneRenderError::ModelNotQueuedForPreparation(model));
            }
        }
        if !self.preparing_models.contains_key(&model) {
            let Some(prepared) = self.prepared_models.get_mut(&model) else {
                return Err(PbrSceneRenderError::ModelNotQueuedForPreparation(model));
            };
            texture_upload = prepared.upload_with_budget_for_frame(
                frame,
                &mut self.texture_assets,
                &mut self.texture_cache,
                budget.maximum_texture_slots,
                budget.target_texture_bytes,
            )?;
            if prepared.remaining() != 0 {
                let mut progress = self.model_upload_progress(models, model)?;
                progress.uploaded_texture_bytes = texture_upload.uploaded_unique_bytes;
                progress.uploaded_oversized_texture = texture_upload.uploaded_oversized_texture;
                return Ok(progress);
            }
            let Some(prepared) = self.prepared_models.remove(&model) else {
                return Err(PbrSceneRenderError::ModelNotQueuedForPreparation(model));
            };
            let textures = prepared
                .finish()
                .map_err(PbrSceneRenderError::PreparedIncomplete)?;
            self.preparing_models
                .insert(model, PbrGpuModelUpload::new(source, textures));
        }
        let Some(mut upload) = self.preparing_models.remove(&model) else {
            return Err(PbrSceneRenderError::ModelNotQueuedForPreparation(model));
        };
        if !upload.matches(source) {
            upload
                .textures
                .release(&mut self.texture_assets, &mut self.texture_cache);
            return Err(PbrSceneRenderError::ModelChangedDuringPreparation(model));
        }
        let uploaded_oversized_primitive =
            match self.upload_primitives_for_frame(frame, model, source, &mut upload, budget) {
                Ok(value) => value,
                Err(error) => {
                    // Keep decoded GPU texture residency so the caller can retry
                    // without re-queuing worker-prepared publication.
                    self.preparing_models.insert(model, upload);
                    return Err(error);
                }
            };
        let mut progress = upload.progress(uploaded_oversized_primitive);
        progress.uploaded_texture_bytes = texture_upload.uploaded_unique_bytes;
        progress.uploaded_oversized_texture = texture_upload.uploaded_oversized_texture;
        if progress.ready {
            self.models.insert(model, upload.finish());
        } else {
            self.preparing_models.insert(model, upload);
        }
        Ok(progress)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "Opaque submission and globally sorted transparent submission share cache and depth-phase accounting."
    )]
    fn draw_for_frame(
        &mut self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        lighting: PbrLighting3d,
        models: &Assets<Model>,
        scene: &yuyib_game_3d::ExtractedModels,
        shadow_casters: Option<&yuyib_game_3d::ExtractedModels>,
        rebuild_shadows: bool,
    ) -> Result<SceneDrawStats, PbrSceneRenderError> {
        let mut stats = SceneDrawStats::default();
        self.textured_renderer.reset_batch_uniform_ring();
        self.ensure_models_uploaded(frame, models, scene, &mut stats)?;
        let _ = shadow_casters;

        let mut factor_opaque_requests = Vec::new();
        let mut opaque_requests = Vec::new();
        let mut transparent_requests = Vec::new();
        self.collect_pbr_draw_requests(
            camera,
            scene,
            &mut factor_opaque_requests,
            &mut opaque_requests,
            &mut transparent_requests,
            &mut stats,
        )?;

        // Cast the same opaque set we shade. GPU ortho clips outside the map.
        // Do not CPU-coverage-filter here: a tight centre test previously wiped
        // the caster list for large glTF meshes whose translation sits far from
        // the mesh surface.
        if rebuild_shadows {
            self.prepare_directional_shadow(
                frame,
                camera,
                lighting,
                &factor_opaque_requests,
                &opaque_requests,
            )?;
        }
        let shadow = self.directional_shadow.as_ref();

        let mut depth_started = false;
        for request in &factor_opaque_requests {
            let (mesh, material, double_sided) = self.resolve_factor_draw(request)?;
            let depth_load = if depth_started {
                crate::DepthLoad::Load
            } else {
                crate::DepthLoad::Clear
            };
            let draw_stats = self.factor_renderer.draw_with_specular_ibl(
                frame,
                camera,
                mesh,
                request.model_matrix,
                material,
                lighting,
                depth_load,
                double_sided,
                self.specular_ibl.as_ref(),
                shadow,
            )?;
            depth_started = true;
            stats.primitive_draws += 1;
            stats.draw_calls += u64::from(draw_stats.draw_calls);
            stats.triangles += u64::from(draw_stats.triangles);
            stats.transient_uniform_buffer_allocations +=
                u64::from(draw_stats.transient_uniform_buffer_allocations);
            stats.render_passes += 1;
        }
        let opaque_draws = opaque_requests
            .iter()
            .map(|request| self.resolve_textured_draw(request))
            .collect::<Result<Vec<_>, _>>()?;
        for chunk in opaque_draws.chunks(512) {
            let draw_stats = self.textured_renderer.draw_batch_with_specular_ibl(
                frame,
                camera,
                chunk,
                lighting,
                if depth_started {
                    crate::DepthLoad::Load
                } else {
                    crate::DepthLoad::Clear
                },
                false,
                self.specular_ibl.as_ref(),
                shadow,
            )?;
            depth_started = true;
            stats.primitive_draws += u64::try_from(chunk.len()).expect("batch length fits u64");
            stats.draw_calls += u64::from(draw_stats.draw_calls);
            stats.triangles += u64::from(draw_stats.triangles);
            stats.transient_uniform_buffer_allocations +=
                u64::from(draw_stats.transient_uniform_buffer_allocations);
            stats.render_passes += 1;
        }
        if let Some(skybox) = self.skybox.as_ref() {
            let depth_load = if depth_started {
                crate::DepthLoad::Load
            } else {
                crate::DepthLoad::Clear
            };
            self.skybox_renderer
                .draw(frame, camera, skybox, depth_load)?;
            depth_started = true;
            stats.draw_calls += 1;
            stats.render_passes += 1;
        }
        transparent_requests.sort_by(|left, right| {
            right
                .camera_distance_squared
                .total_cmp(&left.camera_distance_squared)
        });
        let transparent_draws = transparent_requests
            .iter()
            .map(|request| self.resolve_textured_draw(request))
            .collect::<Result<Vec<_>, _>>()?;
        for chunk in transparent_draws.chunks(512) {
            let draw_stats = self.textured_renderer.draw_batch_with_specular_ibl(
                frame,
                camera,
                chunk,
                lighting,
                if depth_started {
                    crate::DepthLoad::Load
                } else {
                    crate::DepthLoad::Clear
                },
                true,
                self.specular_ibl.as_ref(),
                shadow,
            )?;
            depth_started = true;
            stats.primitive_draws += u64::try_from(chunk.len()).expect("batch length fits u64");
            stats.draw_calls += u64::from(draw_stats.draw_calls);
            stats.triangles += u64::from(draw_stats.triangles);
            stats.transient_uniform_buffer_allocations +=
                u64::from(draw_stats.transient_uniform_buffer_allocations);
            stats.render_passes += 1;
        }
        Ok(stats)
    }

    fn ensure_models_uploaded(
        &mut self,
        frame: &mut RenderFrame<'_>,
        models: &Assets<Model>,
        scene: &yuyib_game_3d::ExtractedModels,
        stats: &mut SceneDrawStats,
    ) -> Result<(), PbrSceneRenderError> {
        for batch in scene.batches() {
            if self.models.contains_key(&batch.model()) {
                continue;
            }
            if self.prepared_models.contains_key(&batch.model())
                || self.preparing_models.contains_key(&batch.model())
            {
                return Err(PbrSceneRenderError::ModelPreparationInProgress(
                    batch.model(),
                ));
            }
            let source = models
                .get(batch.model())
                .ok_or(PbrSceneRenderError::MissingModel(batch.model()))?;
            let uploaded = self.upload_model(frame, batch.model(), source)?;
            stats.material_bind_group_creations += uploaded.material_bind_group_creations;
            self.models.insert(batch.model(), uploaded);
            stats.cache_misses += 1;
        }
        Ok(())
    }

    fn collect_pbr_draw_requests(
        &self,
        camera: Camera3d,
        scene: &yuyib_game_3d::ExtractedModels,
        factor_opaque_requests: &mut Vec<FactorPbrRequest>,
        opaque_requests: &mut Vec<TexturedPbrRequest>,
        transparent_requests: &mut Vec<TexturedPbrRequest>,
        stats: &mut SceneDrawStats,
    ) -> Result<(), PbrSceneRenderError> {
        for batch in scene.batches() {
            let cached = self
                .models
                .get(&batch.model())
                .ok_or(PbrSceneRenderError::MissingModel(batch.model()))?;
            for draw in batch.draws() {
                if draw.mesh.is_some_and(|mesh| mesh >= cached.meshes.len()) {
                    return Err(PbrSceneRenderError::MissingMesh {
                        model: batch.model(),
                        mesh: draw.mesh.unwrap_or_default(),
                    });
                }
                stats.model_instances += 1;
                for (mesh_index, primitives) in cached.meshes.iter().enumerate() {
                    if draw.mesh.is_some_and(|selected| selected != mesh_index) {
                        continue;
                    }
                    for (primitive_index, primitive) in primitives.iter().enumerate() {
                        match primitive {
                            PbrGpuPrimitive::Textured {
                                transparent,
                                local_center,
                                promoted_from_blend,
                                ..
                            } => {
                                stats.promoted_blend_draws += u64::from(*promoted_from_blend);
                                let world_center =
                                    crate::transform_point(draw.model_matrix, *local_center);
                                let request = TexturedPbrRequest {
                                    model: batch.model(),
                                    mesh_index,
                                    primitive_index,
                                    model_matrix: draw.model_matrix,
                                    camera_distance_squared: crate::squared_distance(
                                        world_center,
                                        camera.position,
                                    ),
                                };
                                if *transparent {
                                    transparent_requests.push(request);
                                } else {
                                    opaque_requests.push(request);
                                }
                            }
                            PbrGpuPrimitive::Factor {
                                promoted_from_blend,
                                ..
                            } => {
                                stats.promoted_blend_draws += u64::from(*promoted_from_blend);
                                factor_opaque_requests.push(FactorPbrRequest {
                                    model: batch.model(),
                                    mesh_index,
                                    primitive_index,
                                    model_matrix: draw.model_matrix,
                                });
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn prepare_directional_shadow(
        &mut self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        lighting: PbrLighting3d,
        factor_opaque: &[FactorPbrRequest],
        textured_opaque: &[TexturedPbrRequest],
    ) -> Result<(), PbrSceneRenderError> {
        let Some(policy) = self.shadow_policy else {
            self.directional_shadow = None;
            return Ok(());
        };
        let config = policy
            .config_for_camera(camera)
            .map_err(PbrSceneRenderError::Shadow)?;
        let light_dir = lighting.direct().light().direction;
        let needs_new = self.directional_shadow.as_ref().is_none_or(|shadow| {
            shadow.resolution() != config.resolution()
                || shadow.cascade_count() != config.cascade_count()
        });
        if needs_new {
            self.directional_shadow = Some(
                crate::GpuDirectionalShadow::create_for_frame(frame, config, light_dir)
                    .map_err(PbrSceneRenderError::Shadow)?,
            );
        } else if let Some(shadow) = self.directional_shadow.as_mut() {
            shadow
                .update_light(frame.queue(), config, light_dir)
                .map_err(PbrSceneRenderError::Shadow)?;
        }
        let Some(shadow) = self.directional_shadow.as_ref() else {
            return Ok(());
        };

        // Draw every opaque submitted for shading. MASK cutoffs discard in the
        // caster fragment stage. GPU ortho clips outside the map — avoid CPU
        // coverage filters that drop large glTF meshes by translation alone.
        let mut factor_casters = Vec::with_capacity(factor_opaque.len());
        for request in factor_opaque {
            let (mesh, material, _) = self.resolve_factor_draw(request)?;
            factor_casters.push(crate::FactorShadowCasterDraw {
                mesh,
                model_matrix: request.model_matrix,
                base_color: material.base_color(),
                alpha_cutoff: material.alpha_mode().shader_cutoff(),
            });
        }

        let mut textured_casters = Vec::with_capacity(textured_opaque.len());
        for request in textured_opaque {
            let draw = self.resolve_textured_draw(request)?;
            textured_casters.push(crate::TexturedShadowCasterDraw {
                mesh: draw.mesh,
                material: draw.binding,
                model_matrix: request.model_matrix,
                base_color: draw.material.base_color(),
                alpha_cutoff: draw.material.alpha_mode().shader_cutoff(),
            });
        }
        self.shadow_caster.draw_opaque_casters(
            frame,
            shadow,
            &factor_casters,
            &textured_casters,
        );
        Ok(())
    }

    fn resolve_factor_draw(
        &self,
        request: &FactorPbrRequest,
    ) -> Result<(&crate::GpuPbrMesh, PbrMaterial3d, bool), PbrSceneRenderError> {
        let cached = self
            .models
            .get(&request.model)
            .ok_or(PbrSceneRenderError::MissingModel(request.model))?;
        let primitive = cached
            .meshes
            .get(request.mesh_index)
            .and_then(|mesh| mesh.get(request.primitive_index))
            .ok_or(PbrSceneRenderError::MissingMesh {
                model: request.model,
                mesh: request.mesh_index,
            })?;
        match primitive {
            PbrGpuPrimitive::Factor {
                mesh,
                material,
                double_sided,
                ..
            } => Ok((mesh, *material, *double_sided)),
            PbrGpuPrimitive::Textured { .. } => Err(PbrSceneRenderError::MissingMesh {
                model: request.model,
                mesh: request.mesh_index,
            }),
        }
    }

    fn resolve_textured_draw<'a>(
        &'a self,
        request: &TexturedPbrRequest,
    ) -> Result<TexturedPbrBatchDraw<'a>, PbrSceneRenderError> {
        let cached = self
            .models
            .get(&request.model)
            .ok_or(PbrSceneRenderError::MissingModel(request.model))?;
        let primitive = cached
            .meshes
            .get(request.mesh_index)
            .and_then(|mesh| mesh.get(request.primitive_index))
            .ok_or(PbrSceneRenderError::MissingMesh {
                model: request.model,
                mesh: request.mesh_index,
            })?;
        let PbrGpuPrimitive::Textured {
            mesh,
            binding,
            material,
            normal_scale,
            double_sided,
            ..
        } = primitive
        else {
            unreachable!("only textured requests enter the PBR batch")
        };
        Ok(TexturedPbrBatchDraw {
            mesh,
            binding,
            model_matrix: request.model_matrix,
            material: *material,
            normal_scale: *normal_scale,
            double_sided: *double_sided,
        })
    }

    fn upload_primitives_for_frame(
        &self,
        frame: &RenderFrame<'_>,
        handle: ModelHandle,
        model: &Model,
        upload: &mut PbrGpuModelUpload,
        budget: ModelUploadBudget3d,
    ) -> Result<bool, PbrSceneRenderError> {
        if budget.maximum_primitives == 0 || budget.target_geometry_bytes == 0 {
            return Ok(false);
        }
        let mut uploaded_primitives = 0_usize;
        let mut uploaded_geometry_bytes = 0_u64;
        let mut uploaded_oversized_primitive = false;
        while upload.next_mesh < model.meshes().len() {
            let source_mesh = &model.meshes()[upload.next_mesh];
            if upload.next_primitive >= source_mesh.primitives().len() {
                upload.next_mesh += 1;
                upload.next_primitive = 0;
                continue;
            }
            if uploaded_primitives >= budget.maximum_primitives {
                break;
            }
            let primitive = &source_mesh.primitives()[upload.next_primitive];
            let primitive_bytes = crate::primitive_source_geometry_bytes(primitive);
            if uploaded_primitives != 0
                && uploaded_geometry_bytes.saturating_add(primitive_bytes)
                    > budget.target_geometry_bytes
            {
                break;
            }
            if uploaded_primitives == 0 && primitive_bytes > budget.target_geometry_bytes {
                uploaded_oversized_primitive = true;
            }
            let mesh_index = upload.next_mesh;
            let primitive_index = upload.next_primitive;
            let (primitive, material_bind_group_creations) = self.upload_primitive(
                frame,
                handle,
                model,
                &upload.textures,
                mesh_index,
                primitive_index,
            )?;
            upload.meshes[mesh_index].push(primitive);
            upload.material_bind_group_creations += material_bind_group_creations;
            upload.next_primitive += 1;
            upload.completed_primitives += 1;
            upload.completed_geometry_bytes = upload
                .completed_geometry_bytes
                .saturating_add(primitive_bytes);
            uploaded_primitives += 1;
            uploaded_geometry_bytes = uploaded_geometry_bytes.saturating_add(primitive_bytes);
        }
        Ok(uploaded_oversized_primitive)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "Both honest PBR workflows share validation and material binding decisions."
    )]
    fn upload_primitive(
        &self,
        frame: &RenderFrame<'_>,
        handle: ModelHandle,
        model: &Model,
        textures: &ModelTextureBindings,
        mesh_index: usize,
        primitive_index: usize,
    ) -> Result<(PbrGpuPrimitive, u64), PbrSceneRenderError> {
        let primitive = &model.meshes()[mesh_index].primitives()[primitive_index];
        let bound = primitive
            .material()
            .map(|index| {
                model
                    .materials()
                    .get(index.get())
                    .ok_or(PbrSceneRenderError::MissingMaterial {
                        model: handle,
                        mesh: mesh_index,
                        primitive: primitive_index,
                        material: index.get(),
                    })
            })
            .transpose()?;
        let source = self
            .unbound_material_policy
            .resolve(bound)
            .map_err(|()| PbrSceneRenderError::UnboundMaterial {
                model: handle,
                mesh: mesh_index,
                primitive: primitive_index,
            })?
            .clone();
        if source.specular_glossiness().is_some() {
            return Err(PbrSceneRenderError::SpecularGlossinessUnsupported {
                model: handle,
                mesh: mesh_index,
                primitive: primitive_index,
            });
        }
        let alpha_mode = PbrAlphaMode3d::try_from(source.alpha_mode())?;
        let material = PbrMaterial3d::new(
            source.base_color_factor(),
            source.metallic_factor(),
            source.roughness_factor(),
        )?
        .with_emissive(source.emissive_factor())?
        .with_alpha_mode(alpha_mode);
        let base = source
            .base_color_texture()
            .filter(|binding| primitive.tex_coords(binding.tex_coord_set()).is_some());
        let normal = source.normal_texture().filter(|binding| {
            primitive
                .tex_coords(binding.binding().tex_coord_set())
                .is_some()
        });
        let metallic_roughness = source
            .metallic_roughness_texture()
            .filter(|binding| primitive.tex_coords(binding.tex_coord_set()).is_some());
        let emissive = source
            .emissive_texture()
            .filter(|binding| primitive.tex_coords(binding.tex_coord_set()).is_some());
        let textured = base.is_some()
            || normal.is_some()
            || metallic_roughness.is_some()
            || emissive.is_some();
        // Drop only channels whose UV set is absent; keep UV0 base-color maps
        // instead of collapsing the whole primitive to factor-only.
        if !textured {
            let promoted_from_blend = source.alpha_mode() == AlphaMode::Blend
                && self
                    .blend_policy
                    .promotes_factor(source.base_color_factor()[3]);
            if source.alpha_mode() == AlphaMode::Blend && !promoted_from_blend {
                return Err(PbrSceneRenderError::AlphaUnsupported {
                    model: handle,
                    mesh: mesh_index,
                    primitive: primitive_index,
                });
            }
            let material = if promoted_from_blend {
                material.with_alpha_mode(PbrAlphaMode3d::Opaque)
            } else {
                material
            };
            return Ok((
                PbrGpuPrimitive::Factor {
                    mesh: self
                        .factor_renderer
                        .upload_mesh_for_frame(frame, primitive)?,
                    material,
                    double_sided: source.double_sided(),
                    promoted_from_blend,
                },
                0,
            ));
        }

        let presence = PbrTexturePresence3d::from_channels([
            base.is_some(),
            normal.is_some(),
            metallic_roughness.is_some(),
            emissive.is_some(),
        ]);
        let fallback = base
            .or_else(|| normal.map(yuyib_model::NormalTextureBinding::binding))
            .or(metallic_roughness)
            .or(emissive)
            .expect("the factor-only case returned above");
        let get_texture = |binding: yuyib_model::TextureBinding| {
            let resolved =
                textures
                    .get(binding.texture())
                    .ok_or(PbrSceneRenderError::MissingTexture {
                        model: handle,
                        mesh: mesh_index,
                        primitive: primitive_index,
                        texture: binding.texture().get(),
                    })?;
            self.texture_cache.get(resolved.handle()).ok_or(
                PbrSceneRenderError::MissingGpuTexture {
                    model: handle,
                    texture: binding.texture().get(),
                },
            )
        };
        let fallback_gpu = get_texture(fallback)?;
        let base_gpu = match base {
            Some(value) => get_texture(value)?,
            None => fallback_gpu,
        };
        let normal_gpu = match normal {
            Some(value) => get_texture(value.binding())?,
            None => fallback_gpu,
        };
        let metallic_roughness_gpu = match metallic_roughness {
            Some(value) => get_texture(value)?,
            None => fallback_gpu,
        };
        let emissive_gpu = match emissive {
            Some(value) => get_texture(value)?,
            None => fallback_gpu,
        };
        let promoted_from_blend = source.alpha_mode() == AlphaMode::Blend
            && if let Some(base) = base {
                let resolved =
                    textures
                        .get(base.texture())
                        .ok_or(PbrSceneRenderError::MissingTexture {
                            model: handle,
                            mesh: mesh_index,
                            primitive: primitive_index,
                            texture: base.texture().get(),
                        })?;
                self.blend_policy
                    .promotes(source.base_color_factor()[3], resolved.alpha_summary())
            } else {
                self.blend_policy
                    .promotes_factor(source.base_color_factor()[3])
            };
        let material = if promoted_from_blend {
            material.with_alpha_mode(PbrAlphaMode3d::Opaque)
        } else {
            material
        };
        let binding = self.textured_renderer.upload_partial_material_for_frame(
            frame,
            base_gpu,
            normal_gpu,
            metallic_roughness_gpu,
            emissive_gpu,
            presence,
        );
        let fallback_set = fallback.tex_coord_set();
        Ok((
            PbrGpuPrimitive::Textured {
                mesh: self.textured_renderer.upload_partial_mesh_for_frame(
                    frame,
                    primitive,
                    PbrTextureCoordinateSets3d {
                        base_color: base
                            .map_or(fallback_set, yuyib_model::TextureBinding::tex_coord_set),
                        normal: normal
                            .map_or(fallback_set, |value| value.binding().tex_coord_set()),
                        metallic_roughness: metallic_roughness
                            .map_or(fallback_set, yuyib_model::TextureBinding::tex_coord_set),
                        emissive: emissive
                            .map_or(fallback_set, yuyib_model::TextureBinding::tex_coord_set),
                    },
                    presence,
                )?,
                binding,
                material,
                normal_scale: normal.map_or(1.0, yuyib_model::NormalTextureBinding::scale),
                double_sided: source.double_sided(),
                transparent: source.alpha_mode() == AlphaMode::Blend && !promoted_from_blend,
                local_center: crate::primitive_local_center(primitive),
                promoted_from_blend,
            },
            1,
        ))
    }

    fn upload_model(
        &mut self,
        frame: &RenderFrame<'_>,
        handle: ModelHandle,
        model: &Model,
    ) -> Result<PbrGpuModel, PbrSceneRenderError> {
        let textures = self.texture_loader.load_for_frame(
            frame,
            model,
            &mut self.texture_assets,
            &mut self.texture_cache,
        )?;
        let texture_slots = textures.len();
        let (_, geometry_bytes) = crate::model_geometry_totals(model);
        let mut material_bind_group_creations = 0;
        let mut meshes = model
            .meshes()
            .iter()
            .map(|mesh| Vec::with_capacity(mesh.primitives().len()))
            .collect::<Vec<_>>();
        for (mesh_index, source_mesh) in model.meshes().iter().enumerate() {
            for primitive_index in 0..source_mesh.primitives().len() {
                match self.upload_primitive(
                    frame,
                    handle,
                    model,
                    &textures,
                    mesh_index,
                    primitive_index,
                ) {
                    Ok((primitive, creations)) => {
                        meshes[mesh_index].push(primitive);
                        material_bind_group_creations += creations;
                    }
                    Err(error) => {
                        textures.release(&mut self.texture_assets, &mut self.texture_cache);
                        return Err(error);
                    }
                }
            }
        }
        Ok(PbrGpuModel {
            meshes,
            textures,
            texture_slots,
            material_bind_group_creations,
            geometry_bytes,
        })
    }
}

/// Failure while publishing or drawing a standard factor/textured PBR scene.
#[derive(Debug)]
pub enum PbrSceneRenderError {
    /// An extracted model handle is stale or absent.
    MissingModel(ModelHandle),
    /// An extracted mesh selection is out of range.
    MissingMesh {
        /// Source model.
        model: ModelHandle,
        /// Requested mesh index.
        mesh: usize,
    },
    /// A primitive references an absent material.
    MissingMaterial {
        /// Source model.
        model: ModelHandle,
        /// Source mesh index.
        mesh: usize,
        /// Source primitive index.
        primitive: usize,
        /// Requested material index.
        material: usize,
    },
    /// A primitive has no material binding and unbound policy is [`UnboundMaterialPolicy3d::Error`].
    UnboundMaterial {
        /// Source model.
        model: ModelHandle,
        /// Source mesh index.
        mesh: usize,
        /// Source primitive index.
        primitive: usize,
    },
    /// A material references an absent model texture slot.
    MissingTexture {
        /// Source model.
        model: ModelHandle,
        /// Source mesh index.
        mesh: usize,
        /// Source primitive index.
        primitive: usize,
        /// Requested texture slot.
        texture: usize,
    },
    /// A resolved texture has no resident GPU resource.
    MissingGpuTexture {
        /// Source model.
        model: ModelHandle,
        /// Requested texture slot.
        texture: usize,
    },
    /// A non-promoted factor-only blend has no supported transparent path.
    AlphaUnsupported {
        /// Source model.
        model: ModelHandle,
        /// Source mesh index.
        mesh: usize,
        /// Source primitive index.
        primitive: usize,
    },
    /// Legacy specular/glossiness cannot be mixed into metallic/roughness PBR.
    SpecularGlossinessUnsupported {
        /// Source model.
        model: ModelHandle,
        /// Source mesh index.
        mesh: usize,
        /// Source primitive index.
        primitive: usize,
    },
    /// Material factors were invalid.
    Material(PbrMaterialError),
    /// Model image decoding or GPU upload failed.
    TextureLoad(ModelTextureLoadError),
    /// Prepared texture slots were finalized before all became resident.
    PreparedIncomplete(PreparedModelTexturesIncomplete),
    /// Source model topology changed during a multi-frame upload.
    ModelChangedDuringPreparation(ModelHandle),
    /// Drawing was requested before bounded PBR publication completed.
    ModelPreparationInProgress(ModelHandle),
    /// A textured model reached publication without worker-prepared images.
    ModelNotQueuedForPreparation(ModelHandle),
    /// Position/normal upload failed.
    Upload(crate::LitMeshUploadError),
    /// Tangent-space textured geometry upload failed.
    TexturedUpload(TexturedPbrMeshUploadError),
    /// GPU draw validation failed.
    Draw(PbrMeshRenderError),
    /// Skybox draw failed.
    Skybox(crate::SkyboxRenderError),
    /// Directional shadow prepare/cast failed.
    Shadow(crate::DirectionalShadowError),
}

impl fmt::Display for PbrSceneRenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingModel(model) => write!(formatter, "missing PBR model {model:?}"),
            Self::MissingMesh { model, mesh } => {
                write!(formatter, "PBR model {model:?} has no mesh {mesh}")
            }
            Self::MissingMaterial {
                model,
                mesh,
                primitive,
                material,
            } => write!(
                formatter,
                "PBR model {model:?} mesh {mesh} primitive {primitive} has no material {material}"
            ),
            Self::UnboundMaterial {
                model,
                mesh,
                primitive,
            } => write!(
                formatter,
                "PBR model {model:?} mesh {mesh} primitive {primitive} has no material binding; repair with ModelMaterialPolicy::with_unbound_primitive_fallback or enable UnboundMaterialPolicy3d::DebugMagenta"
            ),
            Self::MissingTexture {
                model,
                mesh,
                primitive,
                texture,
            } => write!(
                formatter,
                "PBR model {model:?} mesh {mesh} primitive {primitive} has no texture slot {texture}"
            ),
            Self::MissingGpuTexture { model, texture } => write!(
                formatter,
                "PBR model {model:?} texture slot {texture} is not resident on the GPU"
            ),
            Self::AlphaUnsupported {
                model,
                mesh,
                primitive,
            } => write!(
                formatter,
                "PBR model {model:?} mesh {mesh} primitive {primitive} uses unsupported factor-only blending"
            ),
            Self::SpecularGlossinessUnsupported {
                model,
                mesh,
                primitive,
            } => write!(
                formatter,
                "PBR model {model:?} mesh {mesh} primitive {primitive} uses the unsupported specular-glossiness workflow"
            ),
            Self::Material(error) => write!(formatter, "invalid PBR model material: {error}"),
            Self::TextureLoad(error) => {
                write!(formatter, "cannot load PBR model textures: {error}")
            }
            Self::PreparedIncomplete(error) => {
                write!(formatter, "cannot finalize prepared PBR textures: {error}")
            }
            Self::ModelChangedDuringPreparation(model) => write!(
                formatter,
                "PBR model {model:?} changed while its GPU upload was in progress; queue it again"
            ),
            Self::ModelPreparationInProgress(model) => write!(
                formatter,
                "PBR model {model:?} is still being published; wait for ready progress before drawing"
            ),
            Self::ModelNotQueuedForPreparation(model) => write!(
                formatter,
                "PBR model {model:?} has textures but no worker-prepared publication"
            ),
            Self::Upload(error) => write!(formatter, "cannot upload PBR model mesh: {error}"),
            Self::TexturedUpload(error) => {
                write!(formatter, "cannot upload textured PBR model mesh: {error}")
            }
            Self::Draw(error) => write!(formatter, "cannot draw PBR model mesh: {error}"),
            Self::Skybox(error) => write!(formatter, "cannot draw skybox: {error}"),
            Self::Shadow(error) => write!(formatter, "cannot prepare directional shadow: {error}"),
        }
    }
}

impl Error for PbrSceneRenderError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Material(error) => Some(error),
            Self::TextureLoad(error) => Some(error),
            Self::PreparedIncomplete(error) => Some(error),
            Self::Upload(error) => Some(error),
            Self::TexturedUpload(error) => Some(error),
            Self::Draw(error) => Some(error),
            Self::Skybox(error) => Some(error),
            Self::Shadow(error) => Some(error),
            Self::MissingModel(_)
            | Self::MissingMesh { .. }
            | Self::MissingMaterial { .. }
            | Self::UnboundMaterial { .. }
            | Self::MissingTexture { .. }
            | Self::MissingGpuTexture { .. }
            | Self::AlphaUnsupported { .. }
            | Self::SpecularGlossinessUnsupported { .. }
            | Self::ModelChangedDuringPreparation(_)
            | Self::ModelPreparationInProgress(_)
            | Self::ModelNotQueuedForPreparation(_) => None,
        }
    }
}

impl From<PbrMaterialError> for PbrSceneRenderError {
    fn from(value: PbrMaterialError) -> Self {
        Self::Material(value)
    }
}

impl From<ModelTextureLoadError> for PbrSceneRenderError {
    fn from(value: ModelTextureLoadError) -> Self {
        Self::TextureLoad(value)
    }
}

impl From<crate::LitMeshUploadError> for PbrSceneRenderError {
    fn from(value: crate::LitMeshUploadError) -> Self {
        Self::Upload(value)
    }
}

impl From<TexturedPbrMeshUploadError> for PbrSceneRenderError {
    fn from(value: TexturedPbrMeshUploadError) -> Self {
        Self::TexturedUpload(value)
    }
}

impl From<PbrMeshRenderError> for PbrSceneRenderError {
    fn from(value: PbrMeshRenderError) -> Self {
        Self::Draw(value)
    }
}

impl From<crate::SkyboxRenderError> for PbrSceneRenderError {
    fn from(value: crate::SkyboxRenderError) -> Self {
        Self::Skybox(value)
    }
}

impl From<crate::DirectionalShadowError> for PbrSceneRenderError {
    fn from(value: crate::DirectionalShadowError) -> Self {
        Self::Shadow(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_model_limit_is_explicit_and_positive() {
        assert_eq!(
            Game3dSceneConfig::new(0),
            Err(Game3dSceneConfigError::ZeroVisibleModelLimit)
        );
        assert_eq!(
            Game3dSceneConfig::new(123)
                .expect("positive limit")
                .visible_model_limit(),
            123
        );
    }

    #[test]
    fn pbr_upload_state_owns_stable_model_totals() {
        let model = Model::cube(0.5).expect("valid cube");
        let mut upload = PbrGpuModelUpload::new(&model, ModelTextureBindings::default());

        assert!(upload.matches(&model));
        assert_eq!(
            upload.progress(false),
            ModelUploadProgress3d {
                total_primitives: 1,
                total_geometry_bytes: 912,
                ..ModelUploadProgress3d::default()
            }
        );

        upload.completed_primitives = 1;
        upload.completed_geometry_bytes = upload.total_geometry_bytes;
        let progress = upload.progress(true);
        assert!(progress.ready);
        assert!(progress.uploaded_oversized_primitive);
        assert_eq!(progress.completed_geometry_bytes, 912);

        let resident = upload.finish();
        assert_eq!(resident.texture_slots, 0);
        assert_eq!(resident.geometry_bytes, 912);
    }

    #[test]
    fn pbr_upload_state_rejects_topology_changes() {
        let original = Model::cube(0.5).expect("valid cube");
        let changed = Model::new(
            vec![
                yuyib_model::Mesh::new(
                    Some("changed".to_owned()),
                    vec![
                        yuyib_model::MeshPrimitive::cube(0.5).expect("valid cube"),
                        yuyib_model::MeshPrimitive::cube(0.25).expect("valid cube"),
                    ],
                )
                .expect("non-empty mesh"),
            ],
            Vec::new(),
            Vec::new(),
        )
        .expect("valid model");

        let upload = PbrGpuModelUpload::new(&original, ModelTextureBindings::default());
        assert!(!upload.matches(&changed));
    }

    #[test]
    fn pbr_blend_policy_promotes_exporter_noise_but_not_glass() {
        let policy = PbrBlendPolicy3d::default();
        let mut nearly_opaque = Vec::with_capacity(1_000 * 4);
        for index in 0..1_000 {
            nearly_opaque.extend_from_slice(&[255, 255, 255, if index < 990 { 255 } else { 242 }]);
        }
        let nearly_opaque = TextureAlphaSummary::from_rgba8(&nearly_opaque);
        assert!(policy.promotes(1.0, nearly_opaque));

        let glass = TextureAlphaSummary::from_rgba8(&[255, 255, 255, 128]);
        assert!(!policy.promotes(1.0, glass));
        assert!(!PbrBlendPolicy3d::strict().promotes(1.0, nearly_opaque));
        assert!(!policy.promotes(0.98, nearly_opaque));
    }

    #[test]
    fn pbr_blend_policy_rejects_invalid_coverage() {
        assert_eq!(
            PbrBlendPolicy3d::effectively_opaque(242, 1_001),
            Err(PbrBlendPolicyError3d::CoverageOutOfRange)
        );
    }
}
