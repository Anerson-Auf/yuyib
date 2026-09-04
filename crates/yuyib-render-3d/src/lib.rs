//! The first GPU 3D mesh phase for Yuyib.
//!
//! [`MeshRenderer3d`] uploads a validated [`yuyib_model::MeshPrimitive`] and
//! draws it through the shared [`yuyib_render::RenderFrame`]. [`SceneRenderer3d`]
//! is the higher-level bridge from an ECS [`yuyib_game_3d::ExtractedModels`]
//! snapshot and [`yuyib_assets::Assets`] to the solid-colour mesh API.
//! [`TexturedMeshRenderer3d`] is a separate explicit pipeline for opaque
//! base-colour textures and UV0. [`LitMeshRenderer3d`] consumes normals for one
//! directional Lambert-lighting path, while [`StandardRenderer3d`] selects the
//! currently supported solid, textured-unlit or colour-Lambert pipeline from
//! model materials. Standard materials preserve per-material `doubleSided`
//! rasterization through dedicated no-cull variants; lit back faces invert
//! their normals before Lambert evaluation. The separate PBR path supports
//! factor-only and arbitrary core base/normal/metallic-roughness/emissive
//! texture subsets with direct lighting. PBR alpha masks discard by the
//! validated glTF cutoff while retaining opaque depth writes; blended textures
//! remain in the sorted non-depth-writing phase. The high-level scene applies renderer-neutral CPU frustum
//! culling at physical source-mesh granularity, expanding whole-model draws in
//! deterministic mesh order and reporting exact/fallback bound quality. Diffuse
//! IBL uses typed L2 SH; factor-only PBR also samples a caller-supplied
//! prefiltered specular cube + BRDF LUT. HDR equirect ingestion is available as
//! [`PreparedEquirectEnvironment3d`]; CPU GGX cook
//! ([`cook_ggx_specular_ibl`]) produces that cube pack. Skybox presentation
//! ([`SkyboxRenderer3d`]) samples mip0 of the same probe. Textured-PBR specular
//! probe wiring beyond the scene attachment path, float HDR cubes, textured
//! shadow sampling / cascaded shadows, instancing and occlusion culling remain
//! outside the first shadow MVP.
//! [`SkinnedMeshRenderer3d`] is a separate low-level path
//! for a glTF four-joint skin and a sampled [`yuyib_gltf::SkinPalette`]; scene
//! ownership, animation choice and character materials remain outside it.
//! Renderer-neutral LOD selection and Hammer import live in the game/source
//! adapter crates.
//! Opaque geometry uses a standard depth test.
//!
//! # Example
//!
//! ```no_run
//! # use yuyib_model::MeshPrimitive;
//! # use yuyib_render::Renderer;
//! # use yuyib_render_3d::{Camera3d, MeshInstance3d, MeshRenderer3d};
//! # fn setup(renderer: &Renderer) -> Result<(), Box<dyn std::error::Error>> {
//! let cube = MeshPrimitive::cube(0.5)?;
//! let meshes = MeshRenderer3d::new(renderer);
//! let gpu_cube = meshes.upload_mesh(renderer, &cube)?;
//! let _camera = Camera3d::default();
//! let _instance = MeshInstance3d::default();
//! # let _ = gpu_cube;
//! # Ok(())
//! # }
//! ```
//!
//! During an application render callback, call [`MeshRenderer3d::draw`] after
//! the clear phase. It loads the colour result and clears the 3D depth phase.
//! To issue more opaque meshes without clearing their depth, use
//! [`MeshRenderer3d::draw_with_depth_load`] with [`DepthLoad::Load`], or use
//! [`SceneRenderer3d`] for an extracted ECS scene.

#![forbid(unsafe_code)]

mod equirect;
mod ggx_cook;
mod ibl;
mod loading;
mod pbr;
mod scene;
mod shadow;
mod skybox;
mod ssao;
mod static_world;

pub use equirect::{EquirectEnvironmentError, PreparedEquirectEnvironment3d};
pub use ggx_cook::{GgxCookConfig, GgxCookError, cook_ggx_specular_ibl};
pub use ibl::{
    GpuSpecularIbl3d, PreparedSpecularIbl3d, SPECULAR_IBL_FACE_COUNT, SpecularIblError,
};
pub use loading::{
    GltfAnimationPreviewGpu, GltfAnimationPreviewGpuError, GltfSceneColliderLayer3d,
    GltfSceneColliderLayerId3d, GltfSceneCollisionConfig3d, GltfSceneCollisionConfigError3d,
    GltfSceneCollisionLimits3d, GltfSceneCollisionMatchMode3d, GltfSceneCollisionNameMatch3d,
    GltfSceneCollisionPredicate3d, GltfSceneCollisionSelector3d, GltfSceneGpuProgress,
    GltfSceneLoad, GltfSceneLoadConfig, GltfSceneLoadError, GltfSceneLoadProgress,
    GltfSceneLoadStage, GltfSceneLoadStartError, LoadedGltfScene,
    LoadedGltfSceneMaterialPolicyError, LoadedGltfSceneRenderError,
};
pub use pbr::{
    DiffuseIrradianceSh3d, DiffuseIrradianceShError, GpuPbrMesh, GpuTexturedPbrMaterial,
    GpuTexturedPbrMesh, PbrAlphaCutoff3d, PbrAlphaMode3d, PbrLighting3d, PbrMaterial3d,
    PbrMaterialError, PbrMeshRenderError, PbrMeshRenderer3d, PbrTextureCoordinateSets3d,
    PbrTexturePresence3d, TexturedPbrBatchDraw, TexturedPbrMeshRenderer3d,
    TexturedPbrMeshUploadError,
};
pub use scene::{
    Game3dLighting, Game3dScene, Game3dSceneConfig, Game3dSceneConfigError, Game3dSceneError,
    Game3dSceneStats, Game3dShading, PbrBlendPolicy3d, PbrBlendPolicyError3d, PbrSceneRenderError,
    UnboundMaterialPolicy3d,
};
pub use shadow::{
    DIRECTIONAL_SHADOW_MAX_CASCADES, DirectionalShadowCaster3d, DirectionalShadowConfig,
    DirectionalShadowError, DirectionalShadowPolicy, FactorShadowCasterDraw, GpuDirectionalShadow,
    TexturedShadowCasterDraw, shadow_coverage_contains, shadow_texel_world_size,
};
pub use skybox::{
    GpuSkybox3d, PreparedSkybox3d, SkyboxError, SkyboxRenderError, SkyboxRenderer3d,
};
pub use ssao::{SsaoPolicy, SsaoPolicyError};
pub use static_world::{
    StaticWorld3d, StaticWorldBatch3d, StaticWorldBuildError3d, StaticWorldBuildStats3d,
    StaticWorldDrawStats3d, StaticWorldRenderer3d, StaticWorldTexture3d, StaticWorldTextureError3d,
    StaticWorldUploadError3d, TexturedStaticWorld3d, TexturedStaticWorldBuildError3d,
    TexturedStaticWorldBuildStats3d, TexturedStaticWorldMaterial3d,
    TexturedStaticWorldRenderError3d, TexturedStaticWorldRenderer3d,
    TexturedStaticWorldUploadError3d, TexturedStaticWorldUploadStats3d,
};

use std::{collections::HashMap, error::Error, fmt, mem::size_of, num::NonZeroU64, sync::Arc};

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;
use yuyib_2d::Texture;
use yuyib_assets::Assets;
use yuyib_game_3d::{DirectionalLightDraw, ExtractedModels};
use yuyib_gltf::{
    AnimationSnapshot, ImportedScene, ImportedSkinnedPrimitive, NodeIndex, SkinPalette,
};
use yuyib_model::{
    AlphaMode, MAX_TEX_COORD_SETS, Material, MeshPrimitive, Model, ModelHandle, ModelTextureIndex,
    TextureBinding,
};
use yuyib_model_assets::{
    ModelTextureBindings, ModelTextureLoadError, ModelTextureLoader, PreparedModelTextures,
    PreparedModelTexturesIncomplete, PreparedTextureUploadStats,
};
use yuyib_render::{RenderFrame, Renderer, wgpu};
use yuyib_render_texture::{GpuTexture, TextureCache};

/// A right-handed perspective camera looking from `position` towards `target`.
///
/// Yuyib's initial 3D convention is: positive Y is up, the camera looks down
/// local negative Z, and the projection's depth range is the WGPU range 0..=1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera3d {
    /// World-space camera position.
    pub position: [f32; 3],
    /// World-space point at the centre of the view.
    pub target: [f32; 3],
    /// Approximate world-space up direction.
    pub up: [f32; 3],
    /// Vertical field of view in radians.
    pub vertical_fov_radians: f32,
    /// Positive distance to the near clipping plane.
    pub near: f32,
    /// Distance to the far clipping plane, greater than `near`.
    pub far: f32,
}

impl Camera3d {
    /// Creates a perspective camera.
    ///
    /// Values are validated when [`Self::view_projection`] is requested, which
    /// lets applications build camera values incrementally in ECS components.
    #[must_use]
    pub const fn new(
        position: [f32; 3],
        target: [f32; 3],
        up: [f32; 3],
        vertical_fov_radians: f32,
        near: f32,
        far: f32,
    ) -> Self {
        Self {
            position,
            target,
            up,
            vertical_fov_radians,
            near,
            far,
        }
    }

    /// Produces a column-major WGPU view-projection matrix for a surface.
    ///
    /// # Errors
    ///
    /// Returns [`MeshRenderError::InvalidCamera`] when the surface dimensions,
    /// clip planes, field of view or look-at vectors cannot produce a finite,
    /// non-degenerate projection.
    #[allow(clippy::cast_precision_loss)] // Window dimensions are well below f32's exact integer range.
    pub fn view_projection(self, surface_size: [u32; 2]) -> Result<[f32; 16], MeshRenderError> {
        if surface_size[0] == 0 || surface_size[1] == 0 {
            return Err(MeshRenderError::InvalidCamera(
                "surface dimensions must be non-zero",
            ));
        }
        if !all_finite(&self.position) || !all_finite(&self.target) || !all_finite(&self.up) {
            return Err(MeshRenderError::InvalidCamera(
                "camera position, target and up must be finite",
            ));
        }
        if !self.vertical_fov_radians.is_finite()
            || self.vertical_fov_radians <= 0.0
            || self.vertical_fov_radians >= std::f32::consts::PI
        {
            return Err(MeshRenderError::InvalidCamera(
                "vertical field of view must be finite and between zero and pi",
            ));
        }
        if !self.near.is_finite()
            || !self.far.is_finite()
            || self.near <= 0.0
            || self.far <= self.near
        {
            return Err(MeshRenderError::InvalidCamera(
                "clip planes must be finite with 0 < near < far",
            ));
        }

        let forward = normalize3(sub3(self.target, self.position)).ok_or(
            MeshRenderError::InvalidCamera("camera position and target must differ"),
        )?;
        let side = normalize3(cross3(forward, self.up)).ok_or(MeshRenderError::InvalidCamera(
            "camera up must not be parallel to the view direction",
        ))?;
        let actual_up = cross3(side, forward);
        let view = [
            side[0],
            actual_up[0],
            -forward[0],
            0.0,
            side[1],
            actual_up[1],
            -forward[1],
            0.0,
            side[2],
            actual_up[2],
            -forward[2],
            0.0,
            -dot3(side, self.position),
            -dot3(actual_up, self.position),
            dot3(forward, self.position),
            1.0,
        ];
        let aspect = surface_size[0] as f32 / surface_size[1] as f32;
        let focal_length = 1.0 / (self.vertical_fov_radians * 0.5).tan();
        let projection = [
            focal_length / aspect,
            0.0,
            0.0,
            0.0,
            0.0,
            focal_length,
            0.0,
            0.0,
            0.0,
            0.0,
            self.far / (self.near - self.far),
            -1.0,
            0.0,
            0.0,
            (self.near * self.far) / (self.near - self.far),
            0.0,
        ];
        let matrix = multiply_matrix4(projection, view);
        if !all_finite(&matrix) {
            return Err(MeshRenderError::InvalidCamera(
                "camera projection contains non-finite values",
            ));
        }
        Ok(matrix)
    }
}

impl Default for Camera3d {
    fn default() -> Self {
        Self::new(
            [0.0, 0.0, 3.0],
            [0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            std::f32::consts::FRAC_PI_3,
            0.1,
            1_000.0,
        )
    }
}

/// A finite scale, Euler rotation and translation for an unlit mesh instance.
///
/// Rotation uses intrinsic XYZ Euler angles in radians. This is convenient for
/// prototypes; a future transform crate should offer quaternions for animation
/// and interpolation-sensitive production code.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeshTransform3d {
    /// World-space translation.
    pub translation: [f32; 3],
    /// Intrinsic XYZ Euler rotation in radians.
    pub rotation_radians: [f32; 3],
    /// Per-axis scale. Negative scale is valid; zero scale is rejected.
    pub scale: [f32; 3],
}

impl MeshTransform3d {
    /// Creates a transform from translation, rotation and scale.
    #[must_use]
    pub const fn new(translation: [f32; 3], rotation_radians: [f32; 3], scale: [f32; 3]) -> Self {
        Self {
            translation,
            rotation_radians,
            scale,
        }
    }

    /// Returns the finite column-major model matrix.
    ///
    /// # Errors
    ///
    /// Returns [`MeshRenderError::InvalidTransform`] when any value is
    /// non-finite or a scale component is zero.
    pub fn matrix(self) -> Result<[f32; 16], MeshRenderError> {
        if !all_finite(&self.translation)
            || !all_finite(&self.rotation_radians)
            || !all_finite(&self.scale)
        {
            return Err(MeshRenderError::InvalidTransform(
                "translation, rotation and scale must be finite",
            ));
        }
        if self.scale.contains(&0.0) {
            return Err(MeshRenderError::InvalidTransform(
                "scale components must not be zero",
            ));
        }
        let [rx, ry, rz] = self.rotation_radians;
        let (sin_x, cos_x) = rx.sin_cos();
        let (sin_y, cos_y) = ry.sin_cos();
        let (sin_z, cos_z) = rz.sin_cos();
        let scale = [
            self.scale[0],
            0.0,
            0.0,
            0.0,
            0.0,
            self.scale[1],
            0.0,
            0.0,
            0.0,
            0.0,
            self.scale[2],
            0.0,
            0.0,
            0.0,
            0.0,
            1.0,
        ];
        let rotate_x = [
            1.0, 0.0, 0.0, 0.0, 0.0, cos_x, sin_x, 0.0, 0.0, -sin_x, cos_x, 0.0, 0.0, 0.0, 0.0, 1.0,
        ];
        let rotate_y = [
            cos_y, 0.0, -sin_y, 0.0, 0.0, 1.0, 0.0, 0.0, sin_y, 0.0, cos_y, 0.0, 0.0, 0.0, 0.0, 1.0,
        ];
        let rotate_z = [
            cos_z, sin_z, 0.0, 0.0, -sin_z, cos_z, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ];
        let translation = [
            1.0,
            0.0,
            0.0,
            0.0,
            0.0,
            1.0,
            0.0,
            0.0,
            0.0,
            0.0,
            1.0,
            0.0,
            self.translation[0],
            self.translation[1],
            self.translation[2],
            1.0,
        ];
        let matrix = multiply_matrix4(
            translation,
            multiply_matrix4(
                rotate_z,
                multiply_matrix4(rotate_y, multiply_matrix4(rotate_x, scale)),
            ),
        );
        if !all_finite(&matrix) {
            return Err(MeshRenderError::InvalidTransform(
                "model matrix contains non-finite values",
            ));
        }
        Ok(matrix)
    }
}

impl Default for MeshTransform3d {
    fn default() -> Self {
        Self::new([0.0; 3], [0.0; 3], [1.0; 3])
    }
}

/// A high-level mesh draw request for the unlit pipeline.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeshInstance3d {
    /// Model-to-world transform.
    pub transform: MeshTransform3d,
    /// Linear RGBA colour. Values are multiplied directly by the unlit shader.
    pub color: [f32; 4],
}

impl MeshInstance3d {
    /// Creates an unlit mesh instance.
    #[must_use]
    pub const fn new(transform: MeshTransform3d, color: [f32; 4]) -> Self {
        Self { transform, color }
    }

    /// Validates and produces the shader uniform data.
    fn uniform(self) -> Result<MeshUniform, MeshRenderError> {
        if !all_finite(&self.color) {
            return Err(MeshRenderError::InvalidInstanceColor);
        }
        Ok(MeshUniform {
            model: self.transform.matrix()?,
            color: self.color,
        })
    }
}

impl Default for MeshInstance3d {
    fn default() -> Self {
        Self::new(MeshTransform3d::default(), [1.0, 1.0, 1.0, 1.0])
    }
}

/// A position/index mesh resident on the GPU.
///
/// The resource owns its buffers and can be drawn for as long as it is kept
/// alive. It does not retain the source `MeshPrimitive` or optional streams.
pub struct GpuMesh {
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    vertex_count: u32,
    index_count: u32,
}

impl GpuMesh {
    /// Returns the uploaded position vertex count.
    #[must_use]
    pub const fn vertex_count(&self) -> u32 {
        self.vertex_count
    }

    /// Returns the uploaded triangle-list index count.
    #[must_use]
    pub const fn index_count(&self) -> u32 {
        self.index_count
    }
}

/// Counts the commands recorded by one [`MeshRenderer3d::draw`] call.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MeshDrawStats {
    /// Number of indexed triangles issued.
    pub triangles: u32,
    /// Number of GPU draw calls issued; currently zero or one.
    pub draw_calls: u32,
    /// Per-frame `create_buffer` / `create_buffer_init` allocations for draw
    /// uniforms (should stay low; true steady-state reuse is still open).
    pub transient_uniform_buffer_allocations: u32,
}

/// Selects how one 3D draw interacts with the frame's depth attachment.
///
/// A depth phase normally starts with [`Self::Clear`] and then uses
/// [`Self::Load`] for every later opaque draw in that phase. The colour target
/// always uses `LoadOp::Load`, preserving the application's base clear and
/// preceding rendering phases.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DepthLoad {
    /// Clear depth to the far-plane value (`1.0`) before drawing this mesh.
    Clear,
    /// Preserve the depth values written by an earlier compatible 3D draw.
    Load,
}

impl DepthLoad {
    const fn operation(self) -> wgpu::LoadOp<f32> {
        match self {
            Self::Clear => wgpu::LoadOp::Clear(1.0),
            Self::Load => wgpu::LoadOp::Load,
        }
    }
}

/// Maximum unlit instances uploaded in one [`MeshRenderer3d`] batch.
///
/// Editor gizmos need a handful of parts; keeping this small avoids oversized
/// uniform buffers while still covering multi-draw overlays.
const UNLIT_MESH_INSTANCE_CAPACITY: u64 = 32;

/// High-level unlit mesh GPU API.
///
/// Construct this renderer once per presentation format, upload meshes during
/// loading, then call [`Self::draw`] inside an application's render callback.
/// `Renderer::resize` keeps the surface format, while a future format-changing
/// renderer lifecycle will require rebuilding this pipeline.
///
/// Multi-draw note: do **not** issue several single-instance draws that each
/// `queue.write_buffer` the same uniform slot inside one frame. WGPU may flush
/// those writes before the command buffer runs, so every pass would see only
/// the last uniform. Use [`Self::draw_batch_depth_clear_double_sided`] (or the
/// single-draw helpers) instead.
pub struct MeshRenderer3d {
    pipeline: wgpu::RenderPipeline,
    double_sided_pipeline: wgpu::RenderPipeline,
    transparent_pipeline: wgpu::RenderPipeline,
    transparent_double_sided_pipeline: wgpu::RenderPipeline,
    camera_buffer: wgpu::Buffer,
    instance_buffer: wgpu::Buffer,
    instance_stride: u64,
    camera_bind_group: wgpu::BindGroup,
    instance_bind_group: wgpu::BindGroup,
}

impl MeshRenderer3d {
    /// Creates a renderer using the presentation format configured on `renderer`.
    #[must_use]
    pub fn new(renderer: &Renderer) -> Self {
        let depth_format = renderer.depth_format();
        let color_format = renderer.color_target_format();
        renderer.with_raw_gpu(|device, _queue, _configuration| {
            Self::create(device, color_format, depth_format)
        })
    }

    /// Creates a renderer from a currently-recording frame.
    ///
    /// This is useful for prototypes that lazily initialise rendering state.
    /// Production applications should generally use [`Self::new`] during setup.
    #[must_use]
    pub fn new_for_frame(frame: &RenderFrame<'_>) -> Self {
        Self::create(frame.device(), frame.surface_format(), frame.depth_format())
    }

    /// Uploads positions and indices from a validated primitive.
    ///
    /// Normals, tangents and UVs are deliberately ignored by this unlit MVP;
    /// this method does not mutate or discard those source streams. Each call
    /// creates independent immutable GPU buffers, so use an asset cache rather
    /// than uploading the same primitive per frame.
    ///
    /// # Errors
    ///
    /// Returns [`MeshUploadError`] when a position is non-finite or a stream
    /// count does not fit WGPU's `u32` draw API.
    pub fn upload_mesh(
        &self,
        renderer: &Renderer,
        primitive: &MeshPrimitive,
    ) -> Result<GpuMesh, MeshUploadError> {
        renderer.with_raw_gpu(|device, _queue, _format| Self::upload_with(device, primitive))
    }

    /// Uploads a mesh with the device bound to the current frame.
    ///
    /// It has the same behavior and limits as [`Self::upload_mesh`].
    /// Prefer uploading during setup to avoid frame-time allocations.
    ///
    /// # Errors
    ///
    /// Returns [`MeshUploadError`] when the primitive cannot be represented by
    /// the GPU unlit mesh format.
    pub fn upload_mesh_for_frame(
        &self,
        frame: &RenderFrame<'_>,
        primitive: &MeshPrimitive,
    ) -> Result<GpuMesh, MeshUploadError> {
        Self::upload_with(frame.device(), primitive)
    }

    /// Records one unlit indexed draw that begins a fresh opaque depth phase.
    ///
    /// The colour attachment uses `LoadOp::Load`, while depth is cleared to
    /// `1.0` and written with a `Less` comparison. This method is convenient
    /// for a single mesh. For more than one opaque draw, call this method only
    /// for the first mesh and use [`Self::draw_with_depth_load`] with
    /// [`DepthLoad::Load`] for the rest. [`SceneRenderer3d`] applies that rule
    /// automatically for an extracted ECS scene.
    ///
    /// # Errors
    ///
    /// Returns [`MeshRenderError`] for invalid camera, transform or colour
    /// data. A valid GPU mesh always produces exactly one draw call.
    pub fn draw(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        mesh: &GpuMesh,
        instance: MeshInstance3d,
    ) -> Result<MeshDrawStats, MeshRenderError> {
        self.draw_with_depth_load(frame, camera, mesh, instance, DepthLoad::Clear)
    }

    /// Records one unlit indexed draw with explicit depth load behavior.
    ///
    /// This is the multi-draw companion to [`Self::draw`]. Start an opaque
    /// phase with [`DepthLoad::Clear`], then use [`DepthLoad::Load`] for each
    /// later opaque mesh so nearer fragments correctly hide farther fragments.
    /// Transparent meshes are not supported by this unlit path and must not be
    /// mixed into this depth-writing phase without a dedicated transparency
    /// policy.
    ///
    /// # Errors
    ///
    /// Returns [`MeshRenderError`] for invalid camera, transform or colour
    /// data. A valid GPU mesh always produces exactly one draw call.
    pub fn draw_with_depth_load(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        mesh: &GpuMesh,
        instance: MeshInstance3d,
        depth_load: DepthLoad,
    ) -> Result<MeshDrawStats, MeshRenderError> {
        let view_projection = camera.view_projection(frame.draw_size())?;
        let uniform = instance.uniform()?;
        Ok(self.draw_uniform(
            frame,
            view_projection,
            mesh,
            uniform,
            depth_load,
            false,
            false,
        ))
    }

    /// Records one unlit indexed draw with a caller-provided model matrix.
    ///
    /// This is the low-level companion to [`Self::draw`]. It is useful when a
    /// scene owns a quaternion or matrix transform and must not round-trip it
    /// through the prototype Euler representation in [`MeshTransform3d`]. The
    /// matrix is column-major and uses the same world convention as
    /// [`Camera3d::view_projection`].
    ///
    /// Like [`Self::draw`], this convenience form clears depth and begins an
    /// opaque phase. Use [`Self::draw_with_model_matrix_depth_load`] when
    /// issuing multiple meshes in that phase.
    ///
    /// # Errors
    ///
    /// Returns [`MeshRenderError`] when the camera, model matrix or colour
    /// contains invalid data.
    pub fn draw_with_model_matrix(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        mesh: &GpuMesh,
        model_matrix: [f32; 16],
        color: [f32; 4],
    ) -> Result<MeshDrawStats, MeshRenderError> {
        self.draw_with_model_matrix_depth_load(
            frame,
            camera,
            mesh,
            model_matrix,
            color,
            DepthLoad::Clear,
        )
    }

    /// Records one unlit indexed draw with an explicit matrix and depth load.
    ///
    /// Start with [`DepthLoad::Clear`] and use [`DepthLoad::Load`] for later
    /// opaque primitives that share the current surface frame. This is the
    /// lowest-level draw entry point used by [`SceneRenderer3d`].
    ///
    /// # Errors
    ///
    /// Returns [`MeshRenderError`] when the camera, model matrix or colour
    /// contains invalid data.
    pub fn draw_with_model_matrix_depth_load(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        mesh: &GpuMesh,
        model_matrix: [f32; 16],
        color: [f32; 4],
        depth_load: DepthLoad,
    ) -> Result<MeshDrawStats, MeshRenderError> {
        self.draw_with_model_matrix_depth_load_rasterized(
            frame,
            camera,
            mesh,
            model_matrix,
            color,
            depth_load,
            false,
        )
    }

    /// Like [`Self::draw_with_model_matrix_depth_load`], but disables back-face
    /// culling. Editor overlays (gizmos) need this: axis/rings are rotated and
    /// would otherwise vanish when winding flips relative to the camera.
    pub fn draw_with_model_matrix_depth_load_double_sided(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        mesh: &GpuMesh,
        model_matrix: [f32; 16],
        color: [f32; 4],
        depth_load: DepthLoad,
    ) -> Result<MeshDrawStats, MeshRenderError> {
        self.draw_with_model_matrix_depth_load_rasterized(
            frame,
            camera,
            mesh,
            model_matrix,
            color,
            depth_load,
            true,
        )
    }

    /// Draws many double-sided unlit meshes in **one** depth-cleared pass.
    ///
    /// Uploads every instance uniform up-front with dynamic offsets, then
    /// records all draws inside a single render pass. This is the correct API
    /// for editor overlays (transform gizmos): multiple separate passes that
    /// rewrite one uniform slot collapse to the last matrix on the GPU.
    ///
    /// # Errors
    ///
    /// Returns [`MeshRenderError`] for invalid camera/matrix/colour data, or
    /// when `draws` exceeds [`UNLIT_MESH_INSTANCE_CAPACITY`].
    pub fn draw_batch_depth_clear_double_sided(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        draws: &[(&GpuMesh, [f32; 16], [f32; 4])],
    ) -> Result<MeshDrawStats, MeshRenderError> {
        self.draw_batch_with_depth_load_double_sided(frame, camera, draws, DepthLoad::Clear)
    }

    /// Like [`Self::draw_batch_depth_clear_double_sided`], but preserves an
    /// existing depth buffer (`DepthLoad::Load`) so overlays can compose after
    /// a world pass without wiping occlusion.
    ///
    /// # Errors
    ///
    /// Returns [`MeshRenderError`] for invalid camera/matrix/colour data, or
    /// when `draws` exceeds [`UNLIT_MESH_INSTANCE_CAPACITY`].
    pub fn draw_batch_with_depth_load_double_sided(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        draws: &[(&GpuMesh, [f32; 16], [f32; 4])],
        depth_load: DepthLoad,
    ) -> Result<MeshDrawStats, MeshRenderError> {
        if draws.is_empty() {
            return Ok(MeshDrawStats {
                triangles: 0,
                draw_calls: 0,
                transient_uniform_buffer_allocations: 0,
            });
        }
        if draws.len() as u64 > UNLIT_MESH_INSTANCE_CAPACITY {
            return Err(MeshRenderError::BatchTooLarge {
                requested: draws.len(),
                capacity: UNLIT_MESH_INSTANCE_CAPACITY as usize,
            });
        }
        let view_projection = camera.view_projection(frame.draw_size())?;
        let mut uniforms = Vec::with_capacity(draws.len());
        for &(_, model_matrix, color) in draws {
            uniforms.push(MeshUniform::from_matrix(model_matrix, color)?);
        }
        frame
            .queue()
            .write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&view_projection));
        let mut packed = vec![0_u8; self.instance_stride as usize * draws.len()];
        for (index, uniform) in uniforms.iter().enumerate() {
            let start = index * self.instance_stride as usize;
            let bytes = bytemuck::bytes_of(uniform);
            packed[start..start + bytes.len()].copy_from_slice(bytes);
        }
        frame
            .queue()
            .write_buffer(&self.instance_buffer, 0, &packed);

        let mut triangles = 0_u32;
        let draw_calls = draws.len() as u32;
        frame.with_surface_pass_with_depth(
            wgpu::LoadOp::Load,
            depth_load.operation(),
            |pass| {
                pass.set_pipeline(&self.double_sided_pipeline);
                pass.set_bind_group(0, &self.camera_bind_group, &[]);
                for (index, (mesh, _, _)) in draws.iter().enumerate() {
                    let offset = u32::try_from(index as u64 * self.instance_stride)
                        .expect("unlit instance dynamic offset fits u32");
                    pass.set_bind_group(1, &self.instance_bind_group, &[offset]);
                    pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                    pass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..mesh.index_count, 0, 0..1);
                    triangles = triangles.saturating_add(mesh.index_count / 3);
                }
            },
        );
        Ok(MeshDrawStats {
            triangles,
            draw_calls,
            transient_uniform_buffer_allocations: 0,
        })
    }

    fn draw_with_model_matrix_depth_load_rasterized(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        mesh: &GpuMesh,
        model_matrix: [f32; 16],
        color: [f32; 4],
        depth_load: DepthLoad,
        double_sided: bool,
    ) -> Result<MeshDrawStats, MeshRenderError> {
        let view_projection = camera.view_projection(frame.draw_size())?;
        let uniform = MeshUniform::from_matrix(model_matrix, color)?;
        Ok(self.draw_uniform(
            frame,
            view_projection,
            mesh,
            uniform,
            depth_load,
            double_sided,
            false,
        ))
    }

    /// Records one unlit draw for the standard-material bridge.
    ///
    /// Unlike the public low-level API, this internal bridge preserves the
    /// source material's rasterization mode.  It intentionally remains
    /// private: callers that need custom raster state should own their own
    /// explicit pipeline instead of changing global renderer behavior.
    #[allow(clippy::too_many_arguments)] // Matrix, depth phase and material-owned raster state are explicit at this boundary.
    fn draw_with_model_matrix_depth_load_rasterization(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        mesh: &GpuMesh,
        model_matrix: [f32; 16],
        color: [f32; 4],
        depth_load: DepthLoad,
        double_sided: bool,
    ) -> Result<MeshDrawStats, MeshRenderError> {
        let view_projection = camera.view_projection(frame.draw_size())?;
        let uniform = MeshUniform::from_matrix(model_matrix, color)?;
        Ok(self.draw_uniform(
            frame,
            view_projection,
            mesh,
            uniform,
            depth_load,
            double_sided,
            false,
        ))
    }

    /// Draws one source-over transparent unlit mesh over an existing opaque
    /// depth phase.
    ///
    /// The draw tests the current depth buffer but never writes it. Callers
    /// must submit transparent meshes from back to front; this low-level API
    /// deliberately leaves that ordering policy under application control.
    /// [`BaseColorSceneRenderer3d`] provides a sorted high-level scene phase.
    ///
    /// # Errors
    ///
    /// Returns [`MeshRenderError`] for invalid camera, matrix or colour data.
    pub fn draw_transparent_with_model_matrix(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        mesh: &GpuMesh,
        model_matrix: [f32; 16],
        color: [f32; 4],
    ) -> Result<MeshDrawStats, MeshRenderError> {
        self.draw_transparent_with_model_matrix_rasterization(
            frame,
            camera,
            mesh,
            model_matrix,
            color,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_transparent_with_model_matrix_rasterization(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        mesh: &GpuMesh,
        model_matrix: [f32; 16],
        color: [f32; 4],
        double_sided: bool,
    ) -> Result<MeshDrawStats, MeshRenderError> {
        let view_projection = camera.view_projection(frame.draw_size())?;
        let uniform = MeshUniform::from_matrix(model_matrix, color)?;
        Ok(self.draw_uniform(
            frame,
            view_projection,
            mesh,
            uniform,
            DepthLoad::Load,
            double_sided,
            true,
        ))
    }

    /// Clears the frame-local depth attachment while preserving surface colour.
    ///
    /// Use this when a scene may contain zero meshes but must nevertheless
    /// begin a well-defined opaque phase before a later renderer uses
    /// [`DepthLoad::Load`]. Most applications do not need it: [`Self::draw`]
    /// clears depth together with its first draw and [`SceneRenderer3d`] calls
    /// it automatically.
    pub fn begin_depth_phase(frame: &mut RenderFrame<'_>) {
        frame.with_surface_pass_with_depth(wgpu::LoadOp::Load, wgpu::LoadOp::Clear(1.0), |_| {});
    }

    #[allow(clippy::too_many_arguments)] // GPU pipeline choice is explicit at this draw boundary.
    fn draw_uniform(
        &self,
        frame: &mut RenderFrame<'_>,
        view_projection: [f32; 16],
        mesh: &GpuMesh,
        uniform: MeshUniform,
        depth_load: DepthLoad,
        double_sided: bool,
        transparent: bool,
    ) -> MeshDrawStats {
        frame
            .queue()
            .write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&view_projection));
        frame
            .queue()
            .write_buffer(&self.instance_buffer, 0, bytemuck::bytes_of(&uniform));
        frame.with_surface_pass_with_depth(wgpu::LoadOp::Load, depth_load.operation(), |pass| {
            pass.set_pipeline(match (transparent, double_sided) {
                (false, false) => &self.pipeline,
                (false, true) => &self.double_sided_pipeline,
                (true, false) => &self.transparent_pipeline,
                (true, true) => &self.transparent_double_sided_pipeline,
            });
            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            pass.set_bind_group(1, &self.instance_bind_group, &[0]);
            pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
            pass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..mesh.index_count, 0, 0..1);
        });
        MeshDrawStats {
            triangles: mesh.index_count / 3,
            draw_calls: 1,
            transient_uniform_buffer_allocations: 0,
        }
    }

    #[allow(clippy::too_many_lines)] // Pipeline state is intentionally adjacent for review.
    fn create(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        depth_format: wgpu::TextureFormat,
    ) -> Self {
        let instance_uniform_size = size_of::<MeshUniform>() as u64;
        let instance_stride = aligned_uniform_stride(
            device.limits().min_uniform_buffer_offset_alignment,
            instance_uniform_size,
        );
        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("yuyib 3d camera layout"),
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
        let instance_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("yuyib 3d instance layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: NonZeroU64::new(instance_uniform_size),
                },
                count: None,
            }],
        });
        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("yuyib 3d camera"),
            size: size_of::<[f32; 16]>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("yuyib 3d instance"),
            size: instance_stride.saturating_mul(UNLIT_MESH_INSTANCE_CAPACITY),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("yuyib 3d camera bind group"),
            layout: &camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });
        let instance_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("yuyib 3d instance bind group"),
            layout: &instance_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &instance_buffer,
                    offset: 0,
                    size: NonZeroU64::new(instance_uniform_size),
                }),
            }],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("yuyib unlit mesh WGSL"),
            source: wgpu::ShaderSource::Wgsl(UNLIT_MESH_WGSL.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("yuyib unlit mesh pipeline layout"),
            bind_group_layouts: &[Some(&camera_layout), Some(&instance_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("yuyib unlit mesh pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(POSITION_VERTEX_LAYOUT)],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: Some(wgpu::Face::Back),
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
        let double_sided_pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("yuyib unlit mesh double-sided pipeline"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[Some(POSITION_VERTEX_LAYOUT)],
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
        let transparent_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("yuyib unlit mesh transparent pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(POSITION_VERTEX_LAYOUT)],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: Some(wgpu::Face::Back),
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
        let transparent_double_sided_pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("yuyib unlit mesh transparent double-sided pipeline"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[Some(POSITION_VERTEX_LAYOUT)],
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
        Self {
            pipeline,
            double_sided_pipeline,
            transparent_pipeline,
            transparent_double_sided_pipeline,
            camera_buffer,
            instance_buffer,
            instance_stride,
            camera_bind_group,
            instance_bind_group,
        }
    }

    fn upload_with(
        device: &wgpu::Device,
        primitive: &MeshPrimitive,
    ) -> Result<GpuMesh, MeshUploadError> {
        let vertex_count = u32::try_from(primitive.positions().len()).map_err(|_| {
            MeshUploadError::TooManyVertices {
                actual: primitive.positions().len(),
            }
        })?;
        let index_count = u32::try_from(primitive.indices().len()).map_err(|_| {
            MeshUploadError::TooManyIndices {
                actual: primitive.indices().len(),
            }
        })?;
        let vertices: Vec<PositionVertex> = primitive
            .positions()
            .iter()
            .copied()
            .enumerate()
            .map(|(index, position)| {
                if all_finite(&position) {
                    Ok(PositionVertex { position })
                } else {
                    Err(MeshUploadError::NonFinitePosition { index })
                }
            })
            .collect::<Result<_, _>>()?;
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("yuyib 3d mesh positions"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("yuyib 3d mesh indices"),
            contents: bytemuck::cast_slice(primitive.indices()),
            usage: wgpu::BufferUsages::INDEX,
        });
        Ok(GpuMesh {
            vertex_buffer,
            index_buffer,
            vertex_count,
            index_count,
        })
    }
}

/// Maximum number of joints accepted by one [`SkinnedMeshRenderer3d`] draw.
///
/// The fixed limit keeps the first skinning path compatible with ordinary
/// desktop GPU storage-buffer limits and makes an accidental giant palette an
/// explicit loading error. 512 matrices occupy only 32 KiB, while also
/// covering production character rigs such as the Velina fixture (which uses
/// joint index 320). Split a model into several skinned primitives when it
/// needs more joints.
pub const MAX_SKIN_JOINTS: usize = 512;

/// Indexed mesh with four glTF joint bindings per vertex, uploaded to the GPU.
///
/// It is deliberately separate from [`GpuMesh`]: a static mesh does not pay
/// for joint indices and weights. Keep this value in an asset cache; it owns
/// immutable GPU vertex and index buffers.
pub struct GpuSkinnedMesh {
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    vertex_count: u32,
    index_count: u32,
    required_joint_count: u32,
}

impl GpuSkinnedMesh {
    /// Returns the number of uploaded vertices.
    #[must_use]
    pub const fn vertex_count(&self) -> u32 {
        self.vertex_count
    }

    /// Returns the number of uploaded triangle-list indices.
    #[must_use]
    pub const fn index_count(&self) -> u32 {
        self.index_count
    }

    /// Returns the smallest palette length that can draw this mesh safely.
    #[must_use]
    pub const fn required_joint_count(&self) -> u32 {
        self.required_joint_count
    }
}

/// Failure while uploading glTF skin data to a [`GpuSkinnedMesh`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SkinnedMeshUploadError {
    /// The source geometry cannot be represented by the static GPU mesh path.
    Geometry(MeshUploadError),
    /// The skin stream has a different vertex count than the primitive.
    VertexCountMismatch {
        /// Geometry position count.
        positions: usize,
        /// Imported skin-vertex count.
        skin_vertices: usize,
    },
    /// A vertex refers to a joint outside this renderer's fixed limit.
    JointLimitExceeded {
        /// Source vertex index.
        vertex: usize,
        /// Referenced joint index.
        joint: u16,
        /// Maximum supported joint count.
        maximum: usize,
    },
    /// A skin weight was NaN, infinite or negative.
    InvalidWeight {
        /// Source vertex index.
        vertex: usize,
    },
}

impl fmt::Display for SkinnedMeshUploadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Geometry(source) => write!(formatter, "cannot upload skinned geometry: {source}"),
            Self::VertexCountMismatch {
                positions,
                skin_vertices,
            } => write!(
                formatter,
                "skinned mesh has {skin_vertices} skin vertices for {positions} positions"
            ),
            Self::JointLimitExceeded {
                vertex,
                joint,
                maximum,
            } => write!(
                formatter,
                "skin vertex {vertex} refers to joint {joint}; this renderer supports fewer than {maximum} joints"
            ),
            Self::InvalidWeight { vertex } => {
                write!(
                    formatter,
                    "skin vertex {vertex} has an invalid joint weight"
                )
            }
        }
    }
}

impl Error for SkinnedMeshUploadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Geometry(source) => Some(source),
            Self::VertexCountMismatch { .. }
            | Self::JointLimitExceeded { .. }
            | Self::InvalidWeight { .. } => None,
        }
    }
}

/// Failure while drawing a [`GpuSkinnedMesh`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SkinnedMeshRenderError {
    /// Camera, model matrix or colour cannot produce valid GPU uniform data.
    Mesh(MeshRenderError),
    /// A glTF pose has no matrices.
    EmptyPalette,
    /// The pose exceeds [`MAX_SKIN_JOINTS`].
    PaletteLimitExceeded {
        /// Number of matrices in the sampled pose.
        actual: usize,
        /// Maximum supported joint count.
        maximum: usize,
    },
    /// The pose is too short for the mesh's referenced joint indices.
    PaletteTooShort {
        /// Number of matrices available in the sampled pose.
        available: usize,
        /// Number required by the mesh.
        required: u32,
    },
    /// A palette matrix has NaN or infinity.
    NonFinitePaletteMatrix {
        /// Matrix index in the sampled pose.
        index: usize,
    },
}

impl fmt::Display for SkinnedMeshRenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mesh(source) => write!(formatter, "cannot draw skinned mesh: {source}"),
            Self::EmptyPalette => {
                formatter.write_str("skinned mesh needs at least one joint matrix")
            }
            Self::PaletteLimitExceeded { actual, maximum } => write!(
                formatter,
                "skin palette has {actual} joints; this renderer accepts at most {maximum}"
            ),
            Self::PaletteTooShort {
                available,
                required,
            } => write!(
                formatter,
                "skin palette has {available} joints but the mesh needs {required}"
            ),
            Self::NonFinitePaletteMatrix { index } => {
                write!(formatter, "skin palette matrix {index} is not finite")
            }
        }
    }
}

impl Error for SkinnedMeshRenderError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Mesh(source) => Some(source),
            Self::EmptyPalette
            | Self::PaletteLimitExceeded { .. }
            | Self::PaletteTooShort { .. }
            | Self::NonFinitePaletteMatrix { .. } => None,
        }
    }
}

/// Explicit low-level GPU skinning renderer for one opaque glTF primitive.
///
/// Create it next to the application's [`Renderer`], upload each
/// [`ImportedSkinnedPrimitive`] once, sample an animation with `yuyib-gltf`,
/// then pass the matching [`SkinPalette`] to [`Self::draw`]. The renderer
/// uploads only the palette per frame; vertices and indices remain immutable.
///
/// This is intentionally a low-level building block. It does not find a skin
/// for a scene node, select animation clips, load textures, or sort alpha
/// geometry. Its fragment stage is a supplied flat colour and it writes the
/// regular opaque depth phase. A future high-level character renderer can own
/// those policies without hiding this API.
pub struct SkinnedMeshRenderer3d {
    pipeline: wgpu::RenderPipeline,
    double_sided_pipeline: wgpu::RenderPipeline,
    transparent_pipeline: wgpu::RenderPipeline,
    transparent_double_sided_pipeline: wgpu::RenderPipeline,
    camera_buffer: wgpu::Buffer,
    instance_buffer: wgpu::Buffer,
    palette_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    instance_bind_group: wgpu::BindGroup,
    palette_bind_group: wgpu::BindGroup,
}

/// Explicit factor-only material for [`SkinnedMeshRenderer3d`].
///
/// This is useful when a glTF primitive deliberately has no base-colour image.
/// The colour is the source `baseColorFactor`; it is not a synthetic white
/// texture.  Blended draws must still be issued in back-to-front order through
/// [`SkinnedMeshRenderer3d::draw_transparent_material_with_model_matrix_depth_load`].
#[derive(Clone, Copy)]
pub struct SkinnedMaterial3d {
    color: [f32; 4],
    double_sided: bool,
}

impl SkinnedMaterial3d {
    /// Creates a one-colour material from a linear RGBA factor.
    #[must_use]
    pub const fn new(color: [f32; 4]) -> Self {
        Self {
            color,
            double_sided: false,
        }
    }

    /// Selects whether the primitive is rasterised from both sides.
    #[must_use]
    pub const fn with_double_sided(mut self, double_sided: bool) -> Self {
        self.double_sided = double_sided;
        self
    }
}

impl SkinnedMeshRenderer3d {
    /// Creates a skinning renderer for the presentation format of `renderer`.
    #[must_use]
    pub fn new(renderer: &Renderer) -> Self {
        let depth_format = renderer.depth_format();
        let color_format = renderer.color_target_format();
        renderer.with_raw_gpu(|device, _queue, _configuration| {
            Self::create(device, color_format, depth_format)
        })
    }

    /// Creates a skinning renderer from the GPU device of the recording frame.
    #[must_use]
    pub fn new_for_frame(frame: &RenderFrame<'_>) -> Self {
        Self::create(frame.device(), frame.surface_format(), frame.depth_format())
    }

    /// Uploads a validated glTF primitive and its matching four-joint stream.
    ///
    /// The method validates matching vertex counts and bounds joint indices to
    /// [`MAX_SKIN_JOINTS`]. It does not inspect mesh/primitive source indices:
    /// applications select the matching pair through their own imported model
    /// and scene ownership.
    ///
    /// # Errors
    ///
    /// Returns [`SkinnedMeshUploadError`] when geometry or skin data cannot be
    /// represented by this fixed four-joint GPU format.
    pub fn upload_mesh(
        &self,
        renderer: &Renderer,
        primitive: &MeshPrimitive,
        skin: &ImportedSkinnedPrimitive,
    ) -> Result<GpuSkinnedMesh, SkinnedMeshUploadError> {
        renderer.with_raw_gpu(|device, _queue, _configuration| {
            Self::upload_with(device, primitive, skin)
        })
    }

    /// Frame-bound counterpart to [`Self::upload_mesh`].
    ///
    /// # Errors
    ///
    /// Returns the same [`SkinnedMeshUploadError`] variants as
    /// [`Self::upload_mesh`].
    pub fn upload_mesh_for_frame(
        &self,
        frame: &RenderFrame<'_>,
        primitive: &MeshPrimitive,
        skin: &ImportedSkinnedPrimitive,
    ) -> Result<GpuSkinnedMesh, SkinnedMeshUploadError> {
        Self::upload_with(frame.device(), primitive, skin)
    }

    /// Draws a skinned opaque mesh and starts a fresh depth phase.
    ///
    /// Use [`Self::draw_with_depth_load`] with [`DepthLoad::Load`] for later
    /// opaque meshes in the same frame.
    ///
    /// # Errors
    ///
    /// Returns [`SkinnedMeshRenderError`] for invalid camera/instance data or
    /// an incompatible sampled pose.
    pub fn draw(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        mesh: &GpuSkinnedMesh,
        palette: &SkinPalette,
        instance: MeshInstance3d,
    ) -> Result<MeshDrawStats, SkinnedMeshRenderError> {
        self.draw_with_depth_load(frame, camera, mesh, palette, instance, DepthLoad::Clear)
    }

    /// Draws a skinned opaque mesh with explicit depth-phase behavior.
    ///
    /// The palette is copied into one fixed GPU storage buffer. Do not retain
    /// references to it across frames: sample or build the pose before the
    /// render callback and pass it here each frame.
    ///
    /// # Errors
    ///
    /// Returns [`SkinnedMeshRenderError`] for invalid camera/instance data or
    /// a pose that is empty, non-finite, too short or larger than the limit.
    pub fn draw_with_depth_load(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        mesh: &GpuSkinnedMesh,
        palette: &SkinPalette,
        instance: MeshInstance3d,
        depth_load: DepthLoad,
    ) -> Result<MeshDrawStats, SkinnedMeshRenderError> {
        let view_projection = camera
            .view_projection(frame.draw_size())
            .map_err(SkinnedMeshRenderError::Mesh)?;
        let uniform = instance.uniform().map_err(SkinnedMeshRenderError::Mesh)?;
        self.draw_uniform(frame, view_projection, mesh, palette, uniform, depth_load)
    }

    /// Draws a skinned opaque mesh using an exact column-major model matrix.
    ///
    /// This low-level variant keeps imported glTF matrix nodes intact instead
    /// of forcing them through prototype Euler transforms. It starts a fresh
    /// opaque depth phase; use [`Self::draw_with_model_matrix_depth_load`] for
    /// several primitives in one frame.
    ///
    /// # Errors
    ///
    /// Returns [`SkinnedMeshRenderError`] when the camera, matrix, colour or
    /// sampled palette is invalid.
    pub fn draw_with_model_matrix(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        mesh: &GpuSkinnedMesh,
        palette: &SkinPalette,
        model_matrix: [f32; 16],
        color: [f32; 4],
    ) -> Result<MeshDrawStats, SkinnedMeshRenderError> {
        self.draw_with_model_matrix_depth_load(
            frame,
            camera,
            mesh,
            palette,
            model_matrix,
            color,
            DepthLoad::Clear,
        )
    }

    /// Draws a skinned mesh with an exact model matrix and explicit depth
    /// phase. Use [`DepthLoad::Clear`] for the first opaque primitive and
    /// [`DepthLoad::Load`] for later ones.
    ///
    /// # Errors
    ///
    /// Returns [`SkinnedMeshRenderError`] when the camera, matrix, colour or
    /// sampled palette is invalid.
    #[allow(clippy::too_many_arguments)] // The low-level draw boundary keeps every GPU input explicit.
    pub fn draw_with_model_matrix_depth_load(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        mesh: &GpuSkinnedMesh,
        palette: &SkinPalette,
        model_matrix: [f32; 16],
        color: [f32; 4],
        depth_load: DepthLoad,
    ) -> Result<MeshDrawStats, SkinnedMeshRenderError> {
        let view_projection = camera
            .view_projection(frame.draw_size())
            .map_err(SkinnedMeshRenderError::Mesh)?;
        let uniform =
            MeshUniform::from_matrix(model_matrix, color).map_err(SkinnedMeshRenderError::Mesh)?;
        self.draw_uniform(frame, view_projection, mesh, palette, uniform, depth_load)
    }

    /// Draws a factor-only opaque or masked primitive with explicit culling.
    ///
    /// For a material without an image, a glTF `MASK` test has one constant
    /// alpha value: its `baseColorFactor.a`.  The caller therefore either
    /// omits it before this call or submits it as an opaque draw.
    ///
    /// # Errors
    ///
    /// Returns [`SkinnedMeshRenderError`] for invalid camera, matrix, colour
    /// or sampled joint palette data.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_material_with_model_matrix_depth_load(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        mesh: &GpuSkinnedMesh,
        palette: &SkinPalette,
        model_matrix: [f32; 16],
        material: SkinnedMaterial3d,
        depth_load: DepthLoad,
    ) -> Result<MeshDrawStats, SkinnedMeshRenderError> {
        let view_projection = camera
            .view_projection(frame.draw_size())
            .map_err(SkinnedMeshRenderError::Mesh)?;
        let uniform = MeshUniform::from_matrix(model_matrix, material.color)
            .map_err(SkinnedMeshRenderError::Mesh)?;
        self.draw_uniform_phase(
            frame,
            view_projection,
            mesh,
            palette,
            uniform,
            material.double_sided,
            false,
            depth_load,
        )
    }

    /// Draws a factor-only blended primitive over an existing depth phase.
    ///
    /// Submit calls back to front. This path tests depth but does not write it.
    ///
    /// # Errors
    ///
    /// Returns [`SkinnedMeshRenderError`] for invalid camera, matrix, colour
    /// or sampled joint palette data.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_transparent_material_with_model_matrix_depth_load(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        mesh: &GpuSkinnedMesh,
        palette: &SkinPalette,
        model_matrix: [f32; 16],
        material: SkinnedMaterial3d,
        depth_load: DepthLoad,
    ) -> Result<MeshDrawStats, SkinnedMeshRenderError> {
        let view_projection = camera
            .view_projection(frame.draw_size())
            .map_err(SkinnedMeshRenderError::Mesh)?;
        let uniform = MeshUniform::from_matrix(model_matrix, material.color)
            .map_err(SkinnedMeshRenderError::Mesh)?;
        self.draw_uniform_phase(
            frame,
            view_projection,
            mesh,
            palette,
            uniform,
            material.double_sided,
            true,
            depth_load,
        )
    }

    fn draw_uniform(
        &self,
        frame: &mut RenderFrame<'_>,
        view_projection: [f32; 16],
        mesh: &GpuSkinnedMesh,
        palette: &SkinPalette,
        uniform: MeshUniform,
        depth_load: DepthLoad,
    ) -> Result<MeshDrawStats, SkinnedMeshRenderError> {
        self.draw_uniform_phase(
            frame,
            view_projection,
            mesh,
            palette,
            uniform,
            false,
            false,
            depth_load,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_uniform_phase(
        &self,
        frame: &mut RenderFrame<'_>,
        view_projection: [f32; 16],
        mesh: &GpuSkinnedMesh,
        palette: &SkinPalette,
        uniform: MeshUniform,
        double_sided: bool,
        transparent: bool,
        depth_load: DepthLoad,
    ) -> Result<MeshDrawStats, SkinnedMeshRenderError> {
        validate_skin_palette(palette.matrices(), mesh.required_joint_count)?;
        frame
            .queue()
            .write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&view_projection));
        frame
            .queue()
            .write_buffer(&self.instance_buffer, 0, bytemuck::bytes_of(&uniform));
        frame.queue().write_buffer(
            &self.palette_buffer,
            0,
            bytemuck::cast_slice(palette.matrices()),
        );
        frame.with_surface_pass_with_depth(wgpu::LoadOp::Load, depth_load.operation(), |pass| {
            pass.set_pipeline(match (transparent, double_sided) {
                (false, false) => &self.pipeline,
                (false, true) => &self.double_sided_pipeline,
                (true, false) => &self.transparent_pipeline,
                (true, true) => &self.transparent_double_sided_pipeline,
            });
            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            pass.set_bind_group(1, &self.instance_bind_group, &[]);
            pass.set_bind_group(2, &self.palette_bind_group, &[]);
            pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
            pass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..mesh.index_count, 0, 0..1);
        });
        Ok(MeshDrawStats {
            triangles: mesh.index_count / 3,
            draw_calls: 1,
            transient_uniform_buffer_allocations: 0,
        })
    }

    #[allow(clippy::too_many_lines)] // GPU resource ownership and pipeline state need adjacent review.
    fn create(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        depth_format: wgpu::TextureFormat,
    ) -> Self {
        let camera_layout = uniform_layout(
            device,
            "yuyib skinned camera layout",
            wgpu::ShaderStages::VERTEX,
        );
        let instance_layout = uniform_layout(
            device,
            "yuyib skinned instance layout",
            wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
        );
        let palette_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("yuyib skinned palette layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("yuyib skinned camera"),
            size: size_of::<[f32; 16]>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("yuyib skinned instance"),
            size: size_of::<MeshUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let palette_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("yuyib skinned palette"),
            size: (MAX_SKIN_JOINTS * size_of::<[f32; 16]>()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("yuyib skinned camera bind group"),
            layout: &camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });
        let instance_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("yuyib skinned instance bind group"),
            layout: &instance_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: instance_buffer.as_entire_binding(),
            }],
        });
        let palette_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("yuyib skinned palette bind group"),
            layout: &palette_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: palette_buffer.as_entire_binding(),
            }],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("yuyib skinned mesh WGSL"),
            source: wgpu::ShaderSource::Wgsl(SKINNED_MESH_WGSL.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("yuyib skinned mesh pipeline layout"),
            bind_group_layouts: &[
                Some(&camera_layout),
                Some(&instance_layout),
                Some(&palette_layout),
            ],
            immediate_size: 0,
        });
        let make_pipeline =
            |label: &'static str, cull_mode: Option<wgpu::Face>, transparent: bool| {
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some(label),
                    layout: Some(&pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: &shader,
                        entry_point: Some("vs_main"),
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        buffers: &[Some(SKINNED_VERTEX_LAYOUT)],
                    },
                    primitive: wgpu::PrimitiveState {
                        topology: wgpu::PrimitiveTopology::TriangleList,
                        front_face: wgpu::FrontFace::Ccw,
                        cull_mode,
                        ..Default::default()
                    },
                    depth_stencil: Some(wgpu::DepthStencilState {
                        format: depth_format,
                        depth_write_enabled: Some(!transparent),
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
                            blend: Some(if transparent {
                                wgpu::BlendState::ALPHA_BLENDING
                            } else {
                                wgpu::BlendState::REPLACE
                            }),
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                    }),
                    multiview_mask: None,
                    cache: None,
                })
            };
        let pipeline = make_pipeline("yuyib skinned mesh pipeline", Some(wgpu::Face::Back), false);
        let double_sided_pipeline =
            make_pipeline("yuyib skinned mesh double-sided pipeline", None, false);
        let transparent_pipeline = make_pipeline(
            "yuyib skinned mesh transparent pipeline",
            Some(wgpu::Face::Back),
            true,
        );
        let transparent_double_sided_pipeline = make_pipeline(
            "yuyib skinned mesh transparent double-sided pipeline",
            None,
            true,
        );
        Self {
            pipeline,
            double_sided_pipeline,
            transparent_pipeline,
            transparent_double_sided_pipeline,
            camera_buffer,
            instance_buffer,
            palette_buffer,
            camera_bind_group,
            instance_bind_group,
            palette_bind_group,
        }
    }

    fn upload_with(
        device: &wgpu::Device,
        primitive: &MeshPrimitive,
        skin: &ImportedSkinnedPrimitive,
    ) -> Result<GpuSkinnedMesh, SkinnedMeshUploadError> {
        if primitive.positions().len() != skin.vertices().len() {
            return Err(SkinnedMeshUploadError::VertexCountMismatch {
                positions: primitive.positions().len(),
                skin_vertices: skin.vertices().len(),
            });
        }
        let vertex_count = u32::try_from(primitive.positions().len()).map_err(|_| {
            SkinnedMeshUploadError::Geometry(MeshUploadError::TooManyVertices {
                actual: primitive.positions().len(),
            })
        })?;
        let index_count = u32::try_from(primitive.indices().len()).map_err(|_| {
            SkinnedMeshUploadError::Geometry(MeshUploadError::TooManyIndices {
                actual: primitive.indices().len(),
            })
        })?;
        let mut required_joint_count = 0_u32;
        let vertices = primitive
            .positions()
            .iter()
            .copied()
            .zip(skin.vertices())
            .enumerate()
            .map(|(vertex, (position, skin_vertex))| {
                if !all_finite(&position) {
                    return Err(SkinnedMeshUploadError::Geometry(
                        MeshUploadError::NonFinitePosition { index: vertex },
                    ));
                }
                let weights = skin_vertex.weights();
                if weights
                    .iter()
                    .any(|weight| !weight.is_finite() || *weight < 0.0)
                {
                    return Err(SkinnedMeshUploadError::InvalidWeight { vertex });
                }
                let joints = skin_vertex.joints();
                for joint in joints {
                    if usize::from(joint) >= MAX_SKIN_JOINTS {
                        return Err(SkinnedMeshUploadError::JointLimitExceeded {
                            vertex,
                            joint,
                            maximum: MAX_SKIN_JOINTS,
                        });
                    }
                    required_joint_count = required_joint_count.max(u32::from(joint) + 1);
                }
                Ok(SkinnedVertex {
                    position,
                    joints,
                    weights,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("yuyib skinned mesh vertices"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("yuyib skinned mesh indices"),
            contents: bytemuck::cast_slice(primitive.indices()),
            usage: wgpu::BufferUsages::INDEX,
        });
        Ok(GpuSkinnedMesh {
            vertex_buffer,
            index_buffer,
            vertex_count,
            index_count,
            required_joint_count,
        })
    }
}

/// GPU resources prepared by [`SkeletalSceneRenderer3d`] for one source
/// primitive. The private source indices make the high-level renderer select
/// the right palette automatically without exposing a second scene format.
struct SkeletalGpuPrimitive {
    mesh: usize,
    primitive: usize,
    gpu: GpuSkinnedMesh,
    color: [f32; 4],
}

/// High-level skeletal renderer for one imported glTF asset.
///
/// Construct it once after [`yuyib_gltf::import_scene_path_with_options`] with
/// [`yuyib_gltf::ImportOptions::skeletal`]. On every frame, advance an
/// [`yuyib_gltf::AnimationPlayer`], obtain its snapshot and pass it to
/// [`Self::draw`]. The renderer finds the source mesh node, its world matrix
/// and the matching joint palette itself.
///
/// Renders only opaque, untextured character geometry. It uses
/// each material's base colour factor, but does not support texture
/// sampling, mask/blend materials, normal maps or PBR yet. Those policies will
/// be added as a separate character-material layer rather than changing the
/// low-level [`SkinnedMeshRenderer3d`] contract.
pub struct SkeletalSceneRenderer3d {
    renderer: SkinnedMeshRenderer3d,
    primitives: Vec<SkeletalGpuPrimitive>,
}

/// A skinned mesh with primary texture coordinates prepared for GPU sampling.
///
/// This is intentionally distinct from [`GpuSkinnedMesh`].  The solid-colour
/// skinning route does not require UV data, while a textured character must
/// reject an asset that cannot name its base-colour coordinates.
pub struct GpuTexturedSkinnedMesh {
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
    required_joint_count: u32,
}

/// Failure while uploading [`GpuTexturedSkinnedMesh`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TexturedSkinnedMeshUploadError {
    /// The common skin stream is invalid.
    Skin(SkinnedMeshUploadError),
    /// The source primitive does not have UV0.
    MissingTexCoords0,
    /// A texture coordinate is NaN or infinite.
    NonFiniteTexCoords0 {
        /// Vertex-stream index.
        index: usize,
    },
}

impl fmt::Display for TexturedSkinnedMeshUploadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Skin(error) => write!(formatter, "cannot upload textured skin: {error}"),
            Self::MissingTexCoords0 => formatter.write_str("textured skinned mesh requires UV0"),
            Self::NonFiniteTexCoords0 { index } => {
                write!(
                    formatter,
                    "skinned texture coordinate {index} is not finite"
                )
            }
        }
    }
}

impl Error for TexturedSkinnedMeshUploadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Skin(error) => Some(error),
            _ => None,
        }
    }
}

/// Failure while drawing a textured skinned mesh.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TexturedSkinnedMeshRenderError {
    /// The sampled pose or transform is invalid.
    Skin(SkinnedMeshRenderError),
    /// The caller has not made the material texture GPU-resident.
    MissingTexture,
    /// The alpha cutoff is NaN, infinite, or outside the inclusive 0..=1 range.
    InvalidAlphaCutoff,
    /// A blended material must use the explicit sorted transparent phase.
    BlendRequiresTransparentPhase,
    /// The transparent phase only accepts a blended material.
    TransparentPhaseRequiresBlend,
}

impl fmt::Display for TexturedSkinnedMeshRenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Skin(error) => write!(formatter, "cannot draw textured skin: {error}"),
            Self::MissingTexture => {
                formatter.write_str("textured skinned material has no GPU texture")
            }
            Self::InvalidAlphaCutoff => {
                formatter.write_str("textured skinned alpha cutoff must be finite and in 0..=1")
            }
            Self::BlendRequiresTransparentPhase => formatter
                .write_str("blended textured skin must be submitted through the transparent phase"),
            Self::TransparentPhaseRequiresBlend => {
                formatter.write_str("textured skin transparent phase requires AlphaMode::Blend")
            }
        }
    }
}
impl Error for TexturedSkinnedMeshRenderError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Skin(error) => Some(error),
            Self::MissingTexture
            | Self::InvalidAlphaCutoff
            | Self::BlendRequiresTransparentPhase
            | Self::TransparentPhaseRequiresBlend => None,
        }
    }
}

/// Explicit texture, colour and alpha-phase inputs for one skinned draw.
///
/// The default is the depth-writing opaque phase. [`Self::with_alpha_mode`]
/// preserves the source `Mask` or `Blend` policy instead of treating it as
/// opaque.
#[derive(Clone, Copy)]
pub struct TexturedSkinnedMaterial3d<'texture> {
    texture: Option<&'texture GpuTexture>,
    base_color_factor: [f32; 4],
    double_sided: bool,
    alpha_mode: AlphaMode,
    light_direction: [f32; 3],
    light_radiance: [f32; 3],
    ambient: [f32; 3],
}

impl<'texture> TexturedSkinnedMaterial3d<'texture> {
    /// Creates an opaque UV0 material from a resident base-colour texture.
    #[must_use]
    pub const fn new(texture: &'texture GpuTexture, base_color_factor: [f32; 4]) -> Self {
        Self {
            texture: Some(texture),
            base_color_factor,
            double_sided: false,
            alpha_mode: AlphaMode::Opaque,
            light_direction: [0.25, -1.0, -0.5],
            light_radiance: [1.0, 1.0, 1.0],
            ambient: [0.22, 0.23, 0.26],
        }
    }

    /// Chooses whether both sides of this particular material are rasterized.
    #[must_use]
    pub const fn with_double_sided(mut self, double_sided: bool) -> Self {
        self.double_sided = double_sided;
        self
    }

    /// Uses the source glTF alpha policy for this draw.
    ///
    /// `Mask` discards fragments below its cutoff while retaining depth writes.
    /// `Blend` uses source-over blending, tests depth, and never writes it.
    /// Blend calls must be submitted back to front.
    #[must_use]
    pub const fn with_alpha_mode(mut self, alpha_mode: AlphaMode) -> Self {
        self.alpha_mode = alpha_mode;
        self
    }

    /// Sets the Lambert key + ambient used by the skinned fragment stage.
    #[must_use]
    pub const fn with_lighting(mut self, lighting: LambertLighting3d) -> Self {
        let light = lighting.light();
        self.light_direction = light.direction;
        self.light_radiance = [
            light.color[0] * light.illuminance_lux,
            light.color[1] * light.illuminance_lux,
            light.color[2] * light.illuminance_lux,
        ];
        self.ambient = lighting.ambient();
        self
    }
}

/// Low-level renderer for textured glTF skins.
///
/// It owns no asset resolver and never substitutes a missing source image with
/// white.  Use [`ModelTextureLoader`] to make images resident, then pass the
/// resulting [`GpuTexture`] here. [`AlphaMode::Mask`] and
/// [`AlphaMode::Blend`] are explicit material modes; blended calls must be
/// submitted back to front through [`Self::draw_transparent_with_depth_load`].
pub struct TexturedSkinnedMeshRenderer3d {
    pipeline: wgpu::RenderPipeline,
    double_sided_pipeline: wgpu::RenderPipeline,
    transparent_pipeline: wgpu::RenderPipeline,
    transparent_double_sided_pipeline: wgpu::RenderPipeline,
    camera_buffer: wgpu::Buffer,
    instance_buffer: wgpu::Buffer,
    palette_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    instance_bind_group: wgpu::BindGroup,
    palette_bind_group: wgpu::BindGroup,
    texture_layout: wgpu::BindGroupLayout,
}

impl TexturedSkinnedMeshRenderer3d {
    /// Creates the renderer from the persistent application GPU device.
    #[must_use]
    pub fn new(renderer: &Renderer) -> Self {
        let depth_format = renderer.depth_format();
        let color_format = renderer.color_target_format();
        renderer.with_raw_gpu(|device, _queue, _configuration| {
            Self::create(device, color_format, depth_format)
        })
    }

    /// Creates the renderer during lazy initialisation in a render callback.
    #[must_use]
    pub fn new_for_frame(frame: &RenderFrame<'_>) -> Self {
        Self::create(frame.device(), frame.surface_format(), frame.depth_format())
    }

    /// Uploads a primitive and its matching glTF skin stream.
    ///
    /// # Errors
    ///
    /// Returns an error if UV0, skin bindings, geometry or numeric inputs are invalid.
    pub fn upload_mesh_for_frame(
        &self,
        frame: &RenderFrame<'_>,
        primitive: &MeshPrimitive,
        skin: &ImportedSkinnedPrimitive,
    ) -> Result<GpuTexturedSkinnedMesh, TexturedSkinnedMeshUploadError> {
        Self::upload_with(frame.device(), primitive, skin)
    }

    /// Uploads a primitive through the persistent application renderer.
    ///
    /// # Errors
    ///
    /// Returns an error if UV0, skin bindings, geometry or numeric inputs are invalid.
    pub fn upload_mesh(
        &self,
        renderer: &Renderer,
        primitive: &MeshPrimitive,
        skin: &ImportedSkinnedPrimitive,
    ) -> Result<GpuTexturedSkinnedMesh, TexturedSkinnedMeshUploadError> {
        renderer.with_raw_gpu(|device, _queue, _configuration| {
            Self::upload_with(device, primitive, skin)
        })
    }

    /// Draws one opaque skinned primitive; later draws can retain depth with
    /// [`Self::draw_with_depth_load`].
    ///
    /// # Errors
    ///
    /// Returns an error for a missing texture, invalid pose or invalid transform.
    #[allow(clippy::too_many_lines)] // Opaque and sorted alpha phases stay adjacent for review.
    pub fn draw(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        mesh: &GpuTexturedSkinnedMesh,
        palette: &SkinPalette,
        model_matrix: [f32; 16],
        material: TexturedSkinnedMaterial3d<'_>,
    ) -> Result<MeshDrawStats, TexturedSkinnedMeshRenderError> {
        self.draw_with_depth_load(
            frame,
            camera,
            mesh,
            palette,
            model_matrix,
            material,
            DepthLoad::Clear,
        )
    }

    /// Draws one opaque skinned primitive while preserving an existing depth phase.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing texture, invalid pose or invalid transform.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_with_depth_load(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        mesh: &GpuTexturedSkinnedMesh,
        palette: &SkinPalette,
        model_matrix: [f32; 16],
        material: TexturedSkinnedMaterial3d<'_>,
        depth_load: DepthLoad,
    ) -> Result<MeshDrawStats, TexturedSkinnedMeshRenderError> {
        if material.alpha_mode == AlphaMode::Blend {
            return Err(TexturedSkinnedMeshRenderError::BlendRequiresTransparentPhase);
        }
        self.draw_with_depth_load_phase(
            frame,
            camera,
            mesh,
            palette,
            model_matrix,
            material,
            depth_load,
            false,
        )
    }

    /// Draws one blended skinned primitive over the existing colour result.
    ///
    /// This path tests but does not write depth. Submit calls back to front;
    /// use [`Self::draw_transparent_with_depth_load`] with `Clear` only when
    /// this is the first 3D depth user in the frame.
    ///
    /// # Errors
    ///
    /// Returns an error if `material` is not [`AlphaMode::Blend`], or if the
    /// texture, pose or transform is invalid.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_transparent_with_depth_load(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        mesh: &GpuTexturedSkinnedMesh,
        palette: &SkinPalette,
        model_matrix: [f32; 16],
        material: TexturedSkinnedMaterial3d<'_>,
        depth_load: DepthLoad,
    ) -> Result<MeshDrawStats, TexturedSkinnedMeshRenderError> {
        if material.alpha_mode != AlphaMode::Blend {
            return Err(TexturedSkinnedMeshRenderError::TransparentPhaseRequiresBlend);
        }
        self.draw_with_depth_load_phase(
            frame,
            camera,
            mesh,
            palette,
            model_matrix,
            material,
            depth_load,
            true,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_with_depth_load_phase(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        mesh: &GpuTexturedSkinnedMesh,
        palette: &SkinPalette,
        model_matrix: [f32; 16],
        material: TexturedSkinnedMaterial3d<'_>,
        depth_load: DepthLoad,
        transparent: bool,
    ) -> Result<MeshDrawStats, TexturedSkinnedMeshRenderError> {
        let texture = material
            .texture
            .ok_or(TexturedSkinnedMeshRenderError::MissingTexture)?;
        let alpha_cutoff = match material.alpha_mode {
            AlphaMode::Opaque | AlphaMode::Blend => -1.0,
            AlphaMode::Mask { cutoff } if cutoff.is_finite() && (0.0..=1.0).contains(&cutoff) => {
                cutoff
            }
            AlphaMode::Mask { .. } => {
                return Err(TexturedSkinnedMeshRenderError::InvalidAlphaCutoff);
            }
        };
        validate_skin_palette(palette.matrices(), mesh.required_joint_count)
            .map_err(TexturedSkinnedMeshRenderError::Skin)?;
        let view_projection = camera.view_projection(frame.draw_size()).map_err(|error| {
            TexturedSkinnedMeshRenderError::Skin(SkinnedMeshRenderError::Mesh(error))
        })?;
        let uniform = TexturedSkinnedUniform::from_parts(
            model_matrix,
            material.base_color_factor,
            alpha_cutoff,
            material.light_direction,
            material.light_radiance,
            material.ambient,
        )
        .map_err(|error| {
            TexturedSkinnedMeshRenderError::Skin(SkinnedMeshRenderError::Mesh(error))
        })?;
        let texture_bind_group = frame
            .device()
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("yuyib textured skinned material bind group"),
                layout: &self.texture_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(texture.view()),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(texture.sampler()),
                    },
                ],
            });
        frame
            .queue()
            .write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&view_projection));
        frame
            .queue()
            .write_buffer(&self.instance_buffer, 0, bytemuck::bytes_of(&uniform));
        frame.queue().write_buffer(
            &self.palette_buffer,
            0,
            bytemuck::cast_slice(palette.matrices()),
        );
        frame.with_surface_pass_with_depth(wgpu::LoadOp::Load, depth_load.operation(), |pass| {
            pass.set_pipeline(match (transparent, material.double_sided) {
                (false, false) => &self.pipeline,
                (false, true) => &self.double_sided_pipeline,
                (true, false) => &self.transparent_pipeline,
                (true, true) => &self.transparent_double_sided_pipeline,
            });
            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            pass.set_bind_group(1, &self.instance_bind_group, &[]);
            pass.set_bind_group(2, &self.palette_bind_group, &[]);
            pass.set_bind_group(3, &texture_bind_group, &[]);
            pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
            pass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..mesh.index_count, 0, 0..1);
        });
        Ok(MeshDrawStats {
            triangles: mesh.index_count / 3,
            draw_calls: 1,
            transient_uniform_buffer_allocations: 0,
        })
    }

    #[allow(clippy::too_many_lines)]
    fn create(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        depth_format: wgpu::TextureFormat,
    ) -> Self {
        let camera_layout = uniform_layout(
            device,
            "yuyib textured skinned camera layout",
            wgpu::ShaderStages::VERTEX,
        );
        let instance_layout = uniform_layout(
            device,
            "yuyib textured skinned instance layout",
            wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
        );
        let palette_layout = skin_palette_layout(device, "yuyib textured skinned palette layout");
        let texture_layout =
            textured_material_layout(device, "yuyib textured skinned material layout");
        let camera_buffer = uniform_buffer(
            device,
            "yuyib textured skinned camera",
            size_of::<[f32; 16]>() as u64,
        );
        let instance_buffer = uniform_buffer(
            device,
            "yuyib textured skinned instance",
            size_of::<TexturedSkinnedUniform>() as u64,
        );
        let palette_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("yuyib textured skinned palette"),
            size: (MAX_SKIN_JOINTS * size_of::<[f32; 16]>()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let camera_bind_group = uniform_bind_group(
            device,
            "yuyib textured skinned camera bind group",
            &camera_layout,
            &camera_buffer,
        );
        let instance_bind_group = uniform_bind_group(
            device,
            "yuyib textured skinned instance bind group",
            &instance_layout,
            &instance_buffer,
        );
        let palette_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("yuyib textured skinned palette bind group"),
            layout: &palette_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: palette_buffer.as_entire_binding(),
            }],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("yuyib textured skinned mesh WGSL"),
            source: wgpu::ShaderSource::Wgsl(TEXTURED_SKINNED_MESH_WGSL.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("yuyib textured skinned mesh pipeline layout"),
            bind_group_layouts: &[
                Some(&camera_layout),
                Some(&instance_layout),
                Some(&palette_layout),
                Some(&texture_layout),
            ],
            immediate_size: 0,
        });
        let make_pipeline =
            |label: &'static str, cull_mode: Option<wgpu::Face>, transparent: bool| {
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some(label),
                    layout: Some(&layout),
                    vertex: wgpu::VertexState {
                        module: &shader,
                        entry_point: Some("vs_main"),
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        buffers: &[Some(TEXTURED_SKINNED_VERTEX_LAYOUT)],
                    },
                    primitive: wgpu::PrimitiveState {
                        topology: wgpu::PrimitiveTopology::TriangleList,
                        front_face: wgpu::FrontFace::Ccw,
                        cull_mode,
                        ..Default::default()
                    },
                    depth_stencil: Some(wgpu::DepthStencilState {
                        format: depth_format,
                        depth_write_enabled: Some(!transparent),
                        depth_compare: Some(if transparent {
                            wgpu::CompareFunction::LessEqual
                        } else {
                            wgpu::CompareFunction::Less
                        }),
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
                            blend: Some(if transparent {
                                wgpu::BlendState::ALPHA_BLENDING
                            } else {
                                wgpu::BlendState::REPLACE
                            }),
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                    }),
                    multiview_mask: None,
                    cache: None,
                })
            };
        Self {
            pipeline: make_pipeline(
                "yuyib textured skinned mesh pipeline",
                Some(wgpu::Face::Back),
                false,
            ),
            double_sided_pipeline: make_pipeline(
                "yuyib textured skinned mesh double-sided pipeline",
                None,
                false,
            ),
            transparent_pipeline: make_pipeline(
                "yuyib textured skinned transparent pipeline",
                Some(wgpu::Face::Back),
                true,
            ),
            transparent_double_sided_pipeline: make_pipeline(
                "yuyib textured skinned transparent double-sided pipeline",
                None,
                true,
            ),
            camera_buffer,
            instance_buffer,
            palette_buffer,
            camera_bind_group,
            instance_bind_group,
            palette_bind_group,
            texture_layout,
        }
    }

    fn upload_with(
        device: &wgpu::Device,
        primitive: &MeshPrimitive,
        skin: &ImportedSkinnedPrimitive,
    ) -> Result<GpuTexturedSkinnedMesh, TexturedSkinnedMeshUploadError> {
        let tex_coords = primitive
            .tex_coords_0()
            .ok_or(TexturedSkinnedMeshUploadError::MissingTexCoords0)?;
        if primitive.positions().len() != skin.vertices().len() {
            return Err(TexturedSkinnedMeshUploadError::Skin(
                SkinnedMeshUploadError::VertexCountMismatch {
                    positions: primitive.positions().len(),
                    skin_vertices: skin.vertices().len(),
                },
            ));
        }
        let vertex_count = u32::try_from(primitive.positions().len()).map_err(|_| {
            TexturedSkinnedMeshUploadError::Skin(SkinnedMeshUploadError::Geometry(
                MeshUploadError::TooManyVertices {
                    actual: primitive.positions().len(),
                },
            ))
        })?;
        let index_count = u32::try_from(primitive.indices().len()).map_err(|_| {
            TexturedSkinnedMeshUploadError::Skin(SkinnedMeshUploadError::Geometry(
                MeshUploadError::TooManyIndices {
                    actual: primitive.indices().len(),
                },
            ))
        })?;
        let mut required_joint_count = 0_u32;
        let normals = primitive.normals();
        let vertices = primitive
            .positions()
            .iter()
            .copied()
            .zip(tex_coords.iter().copied())
            .zip(skin.vertices())
            .enumerate()
            .map(|(vertex, ((position, tex_coord), skin_vertex))| {
                if !all_finite(&position) {
                    return Err(TexturedSkinnedMeshUploadError::Skin(
                        SkinnedMeshUploadError::Geometry(MeshUploadError::NonFinitePosition {
                            index: vertex,
                        }),
                    ));
                }
                if !all_finite(&tex_coord) {
                    return Err(TexturedSkinnedMeshUploadError::NonFiniteTexCoords0 {
                        index: vertex,
                    });
                }
                let normal = normals
                    .and_then(|values| values.get(vertex).copied())
                    .unwrap_or([0.0, 1.0, 0.0]);
                if !all_finite(&normal) {
                    return Err(TexturedSkinnedMeshUploadError::Skin(
                        SkinnedMeshUploadError::Geometry(MeshUploadError::NonFinitePosition {
                            index: vertex,
                        }),
                    ));
                }
                let weights = skin_vertex.weights();
                if weights
                    .iter()
                    .any(|weight| !weight.is_finite() || *weight < 0.0)
                {
                    return Err(TexturedSkinnedMeshUploadError::Skin(
                        SkinnedMeshUploadError::InvalidWeight { vertex },
                    ));
                }
                let joints = skin_vertex.joints();
                for joint in joints {
                    if usize::from(joint) >= MAX_SKIN_JOINTS {
                        return Err(TexturedSkinnedMeshUploadError::Skin(
                            SkinnedMeshUploadError::JointLimitExceeded {
                                vertex,
                                joint,
                                maximum: MAX_SKIN_JOINTS,
                            },
                        ));
                    }
                    required_joint_count = required_joint_count.max(u32::from(joint) + 1);
                }
                Ok(TexturedSkinnedVertex {
                    position,
                    normal,
                    tex_coord,
                    joints,
                    weights,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("yuyib textured skinned mesh vertices"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("yuyib textured skinned mesh indices"),
            contents: bytemuck::cast_slice(primitive.indices()),
            usage: wgpu::BufferUsages::INDEX,
        });
        let _ = vertex_count; // Retained validation: WGPU vertex addressing is u32.
        Ok(GpuTexturedSkinnedMesh {
            vertex_buffer,
            index_buffer,
            index_count,
            required_joint_count,
        })
    }
}

/// Texture residency required by [`TexturedSkeletalSceneRenderer3d`].
#[derive(Clone, Copy)]
pub struct SkeletalTextureResources<'a> {
    /// Model-local resolved texture slots.
    pub bindings: &'a ModelTextureBindings,
    /// GPU texture cache populated by [`ModelTextureLoader`].
    pub textures: &'a TextureCache,
}

impl<'a> SkeletalTextureResources<'a> {
    fn resolve(
        self,
        index: ModelTextureIndex,
    ) -> Result<&'a GpuTexture, TexturedSkeletalSceneRenderError> {
        let binding = self
            .bindings
            .get(index)
            .ok_or(TexturedSkeletalSceneRenderError::MissingTextureBinding { index })?;
        self.textures
            .get(binding.handle())
            .ok_or(TexturedSkeletalSceneRenderError::MissingGpuTexture { index })
    }
}

/// Stable identity of one drawable primitive in an imported skeletal scene.
///
/// `node` is part of the identity because one source mesh may be instantiated
/// by several scene nodes. Hiding a primitive on a first-person player must not
/// accidentally hide another character which shares the same mesh.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SkeletalPrimitive3d {
    node: NodeIndex,
    mesh: usize,
    primitive: usize,
}

impl SkeletalPrimitive3d {
    /// Creates an exact node/mesh/primitive identity.
    #[must_use]
    pub const fn new(node: NodeIndex, mesh: usize, primitive: usize) -> Self {
        Self {
            node,
            mesh,
            primitive,
        }
    }

    /// Returns the scene node which instantiates this primitive.
    #[must_use]
    pub const fn node(self) -> NodeIndex {
        self.node
    }

    /// Returns the source [`Model::meshes`] index.
    #[must_use]
    pub const fn mesh(self) -> usize {
        self.mesh
    }

    /// Returns the primitive index inside its source mesh.
    #[must_use]
    pub const fn primitive(self) -> usize {
        self.primitive
    }
}

/// Read-only source metadata supplied to a setup-time skeletal visibility selector.
///
/// Names are optional debug metadata from the imported asset. The exact typed
/// [`SkeletalPrimitive3d`] identity remains the runtime contract.
#[derive(Clone, Copy, Debug)]
pub struct SkeletalPrimitiveInfo3d<'a> {
    id: SkeletalPrimitive3d,
    node_name: Option<&'a str>,
    mesh_name: Option<&'a str>,
    material_name: Option<&'a str>,
}

impl<'a> SkeletalPrimitiveInfo3d<'a> {
    /// Returns the exact runtime identity.
    #[must_use]
    pub const fn id(self) -> SkeletalPrimitive3d {
        self.id
    }

    /// Returns the optional glTF node name.
    #[must_use]
    pub const fn node_name(self) -> Option<&'a str> {
        self.node_name
    }

    /// Returns the optional model mesh name.
    #[must_use]
    pub const fn mesh_name(self) -> Option<&'a str> {
        self.mesh_name
    }

    /// Returns the optional material name.
    #[must_use]
    pub const fn material_name(self) -> Option<&'a str> {
        self.material_name
    }
}

/// Reusable node/primitive visibility mask for skeletal scene renderers.
///
/// Mutations may allocate while a character or camera mode is configured.
/// [`Self::is_visible`] is allocation-free and is the only operation performed
/// by render loops. A first-person camera can therefore hide independently
/// authored head, eye and hair primitives without cloning the model or changing
/// animation data.
///
/// Prefer exact primitive identities when an asset combines body and head in
/// one mesh. [`Self::hide_named_parts`] is a setup convenience and deliberately
/// uses exact case-insensitive names rather than substring heuristics: a label
/// such as `Headphones` must not unexpectedly hide the player's head or body.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SkeletalVisibilityMask3d {
    hidden_nodes: Vec<NodeIndex>,
    hidden_primitives: Vec<SkeletalPrimitive3d>,
}

impl SkeletalVisibilityMask3d {
    /// Creates a mask which shows every primitive.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            hidden_nodes: Vec::new(),
            hidden_primitives: Vec::new(),
        }
    }

    /// Hides every drawable primitive instantiated by `node`.
    pub fn hide_node(&mut self, node: NodeIndex) -> bool {
        if self.hidden_nodes.contains(&node) {
            return false;
        }
        self.hidden_nodes.push(node);
        true
    }

    /// Makes a previously hidden node visible again.
    pub fn show_node(&mut self, node: NodeIndex) -> bool {
        let Some(index) = self.hidden_nodes.iter().position(|hidden| *hidden == node) else {
            return false;
        };
        self.hidden_nodes.swap_remove(index);
        true
    }

    /// Hides one exact node/mesh/primitive identity.
    pub fn hide_primitive(&mut self, primitive: SkeletalPrimitive3d) -> bool {
        if self.hidden_primitives.contains(&primitive) {
            return false;
        }
        self.hidden_primitives.push(primitive);
        true
    }

    /// Makes a previously hidden primitive visible again.
    pub fn show_primitive(&mut self, primitive: SkeletalPrimitive3d) -> bool {
        let Some(index) = self
            .hidden_primitives
            .iter()
            .position(|hidden| *hidden == primitive)
        else {
            return false;
        };
        self.hidden_primitives.swap_remove(index);
        true
    }

    /// Returns whether a primitive should be submitted by a skeletal renderer.
    ///
    /// This steady-state query never allocates.
    #[must_use]
    pub fn is_visible(&self, primitive: SkeletalPrimitive3d) -> bool {
        !self.hidden_nodes.contains(&primitive.node) && !self.hidden_primitives.contains(&primitive)
    }

    /// Clears every node and primitive exclusion while retaining allocations.
    pub fn show_all(&mut self) {
        self.hidden_nodes.clear();
        self.hidden_primitives.clear();
    }

    /// Hides renderable skeletal primitives selected from source metadata.
    ///
    /// Call this during character setup, not every frame. The predicate sees
    /// exact node/mesh/material names and the resulting typed identity. This is
    /// the escape hatch for assets whose author uses names other than
    /// `Head`/`Eyes`/`Hair`.
    pub fn hide_where(
        &mut self,
        model: &Model,
        scene: &ImportedScene,
        mut predicate: impl FnMut(SkeletalPrimitiveInfo3d<'_>) -> bool,
    ) -> usize {
        let mut hidden = 0;
        for (node_index, node) in scene.nodes().iter().enumerate() {
            let Some(mesh_index) = node.mesh() else {
                continue;
            };
            let Some(mesh) = model.meshes().get(mesh_index) else {
                continue;
            };
            for (primitive_index, primitive) in mesh.primitives().iter().enumerate() {
                if !skeletal_primitive_is_imported(
                    scene,
                    node.skin().is_some(),
                    mesh_index,
                    primitive_index,
                ) {
                    continue;
                }
                let material_name = primitive
                    .material()
                    .and_then(|index| model.materials().get(index.get()))
                    .and_then(Material::name);
                let info = SkeletalPrimitiveInfo3d {
                    id: SkeletalPrimitive3d::new(
                        NodeIndex::new(node_index),
                        mesh_index,
                        primitive_index,
                    ),
                    node_name: node.name(),
                    mesh_name: mesh.name(),
                    material_name,
                };
                if predicate(info) && self.hide_primitive(info.id) {
                    hidden += 1;
                }
            }
        }
        hidden
    }

    /// Hides parts whose node, mesh or material name exactly matches `names`.
    ///
    /// Matching is ASCII-case-insensitive but otherwise exact. For example,
    /// `hair` matches `Hair`, while `head` does not match `Headphones`. Pass
    /// exporter-specific labels explicitly when they contain suffixes.
    pub fn hide_named_parts(
        &mut self,
        model: &Model,
        scene: &ImportedScene,
        names: &[&str],
    ) -> usize {
        self.hide_where(model, scene, |part| {
            [part.node_name(), part.mesh_name(), part.material_name()]
                .into_iter()
                .flatten()
                .any(|label| {
                    names
                        .iter()
                        .any(|expected| label.eq_ignore_ascii_case(expected))
                })
        })
    }
}

fn skeletal_primitive_is_imported(
    scene: &ImportedScene,
    skinned_node: bool,
    mesh: usize,
    primitive: usize,
) -> bool {
    if skinned_node {
        scene
            .skinned_primitives()
            .iter()
            .any(|source| source.mesh() == mesh && source.primitive() == primitive)
    } else {
        scene
            .morph_primitives()
            .iter()
            .any(|source| source.mesh() == mesh && source.primitive() == primitive)
    }
}

struct TexturedSkeletalGpuPrimitive {
    mesh: usize,
    primitive: usize,
    gpu: TexturedSkeletalGpuMesh,
    color: [f32; 4],
    double_sided: bool,
    alpha_mode: AlphaMode,
    local_center: [f32; 3],
}

struct TexturedMorphGpuPrimitive {
    node: NodeIndex,
    mesh: usize,
    primitive: usize,
    gpu: GpuTexturedMesh,
    texture: ModelTextureIndex,
    color: [f32; 4],
    double_sided: bool,
    alpha_mode: AlphaMode,
    local_center: [f32; 3],
    base_positions: Vec<[f32; 3]>,
    tex_coords: Vec<[f32; 2]>,
    position_targets: Vec<Vec<[f32; 3]>>,
}

enum TexturedCharacterTransparent<'a> {
    Skin {
        primitive: &'a TexturedSkeletalGpuPrimitive,
        palette: &'a yuyib_gltf::SkinPalette,
        matrix: [f32; 16],
        distance: f32,
    },
    Morph {
        primitive: &'a TexturedMorphGpuPrimitive,
        matrix: [f32; 16],
        distance: f32,
    },
}

impl TexturedCharacterTransparent<'_> {
    fn distance(&self) -> f32 {
        match self {
            Self::Skin { distance, .. } | Self::Morph { distance, .. } => *distance,
        }
    }

    /// Stable tie-break so near-equal camera distances do not reorder blend draws.
    fn sort_key(&self) -> (u8, usize, usize) {
        match self {
            Self::Skin { primitive, .. } => (0, primitive.mesh, primitive.primitive),
            Self::Morph { primitive, .. } => (1, primitive.mesh, primitive.primitive),
        }
    }
}

/// The source material inputs that form a character's visible colour layer.
///
/// Metallic/roughness materials use `baseColor*`; the older
/// `KHR_materials_pbrSpecularGlossiness` workflow uses `diffuse*`.  This first
/// character path is not a full specular/glossiness shader, but it must use
/// that diffuse image and multiplier. Ignoring them makes legitimate assets
/// such as Velina render white.
#[derive(Clone, Copy)]
struct CharacterBaseColor {
    factor: [f32; 4],
    texture: Option<TextureBinding>,
}

fn character_base_color(material: Option<&Material>) -> CharacterBaseColor {
    let Some(material) = material else {
        return CharacterBaseColor {
            factor: [1.0; 4],
            texture: None,
        };
    };
    let mut surface = if let Some(specular_glossiness) = material.specular_glossiness() {
        CharacterBaseColor {
            factor: specular_glossiness.diffuse_factor(),
            texture: specular_glossiness.diffuse_texture(),
        }
    } else {
        CharacterBaseColor {
            factor: material.base_color_factor(),
            texture: material.base_color_texture(),
        }
    };
    // Specular-glossiness eye/tear shells often author near-black diffuse RGB and
    // rely on specular/IBL for the visible look. The unlit/Lambert character path
    // has no specular term, so black diffuse paints opaque silhouettes. Restore a
    // neutral albedo and keep the authored alpha so blend cards stay transparent.
    let luma = 0.2126_f32
        .mul_add(surface.factor[0], 0.7152_f32.mul_add(surface.factor[1], 0.0722 * surface.factor[2]));
    if luma < 0.04 {
        surface.factor[0] = 1.0;
        surface.factor[1] = 1.0;
        surface.factor[2] = 1.0;
    }
    surface
}

/// GPU representation selected from the source material contract.
///
/// `Factor` means the glTF primitive had no usable UV0 visible-colour image. It
/// uses the source factor directly; no placeholder texture is created.
enum TexturedSkeletalGpuMesh {
    Textured {
        mesh: GpuTexturedSkinnedMesh,
        texture: ModelTextureIndex,
    },
    Factor(GpuSkinnedMesh),
}

/// High-level renderer for textured glTF skins with factor-only fallback when
/// textures or UV sets are unavailable.
///
/// Build it from an imported skeletal scene, load images once through
/// [`ModelTextureLoader`], then draw a sampled pose with [`Self::draw`]. This
/// joins mesh/skin/palette ownership and texture slots without concealing the
/// texture residency boundary. Opaque and masked primitives write depth;
/// blended primitives are sorted back to front per primitive, test depth, and
/// do not write it. Textured skinned and morph draws share a flat
/// view-independent exposure ([`Self::with_lighting`]); directional N·L is
/// deferred until skinned PBR shares the world probe, because orbiting a fixed
/// pose must not change whole-avatar brightness. When a primitive has no usable
/// UV0 base-colour image, it is rendered with its actual source factor through
/// the factor-only skinning path. Both metallic/roughness `baseColor*` and
/// legacy `KHR_materials_pbrSpecularGlossiness` `diffuse*` inputs select the
/// visible colour layer. Near-black specular-glossiness diffuse RGB (typical
/// eye/tear shells) is restored to a neutral albedo so the path without
/// specular does not paint black silhouettes. This does not invent a white
/// texture. Its count is exposed by [`Self::factor_only_primitive_count`].
pub struct TexturedSkeletalSceneRenderer3d {
    renderer: TexturedSkinnedMeshRenderer3d,
    factor_renderer: SkinnedMeshRenderer3d,
    morph_renderer: TexturedMeshRenderer3d,
    primitives: Vec<TexturedSkeletalGpuPrimitive>,
    morph_primitives: Vec<TexturedMorphGpuPrimitive>,
    factor_only_primitives: usize,
    /// Fallback ambient when [`Self::with_lighting`] was not set.
    ambient_fill: [f32; 3],
    /// Optional shared Lambert key for every textured skinned draw.
    lighting: Option<LambertLighting3d>,
}

/// Failure while preparing a textured skeletal scene.
#[derive(Debug)]
#[allow(missing_docs)] // Each variant is a precise source association; keep the public error compact.
pub enum TexturedSkeletalSceneUploadError {
    MissingMesh {
        mesh: usize,
    },
    MissingPrimitive {
        mesh: usize,
        primitive: usize,
    },
    MissingMaterial {
        mesh: usize,
        primitive: usize,
    },
    MissingBaseColorTexture {
        mesh: usize,
        primitive: usize,
    },
    AlphaModeUnsupported {
        mesh: usize,
        primitive: usize,
        alpha_mode: AlphaMode,
    },
    BaseTextureUvSetUnsupported {
        mesh: usize,
        primitive: usize,
        actual: u8,
    },
    Upload(TexturedSkinnedMeshUploadError),
    FactorUpload(SkinnedMeshUploadError),
    MorphUpload(TexturedMeshUploadError),
}
impl fmt::Display for TexturedSkeletalSceneUploadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingMesh { mesh } => write!(f, "textured skin references missing mesh {mesh}"),
            Self::MissingPrimitive { mesh, primitive } => write!(
                f,
                "textured skin references missing primitive {primitive} in mesh {mesh}"
            ),
            Self::MissingMaterial { mesh, primitive } => {
                write!(f, "textured skin {mesh}/{primitive} has no material")
            }
            Self::MissingBaseColorTexture { mesh, primitive } => write!(
                f,
                "textured skin {mesh}/{primitive} has no base-colour texture"
            ),
            Self::AlphaModeUnsupported {
                mesh,
                primitive,
                alpha_mode,
            } => write!(
                f,
                "textured skin {mesh}/{primitive} uses unsupported alpha mode {alpha_mode:?}"
            ),
            Self::BaseTextureUvSetUnsupported {
                mesh,
                primitive,
                actual,
            } => write!(
                f,
                "textured skin {mesh}/{primitive} uses UV{actual}; only UV0 is supported"
            ),
            Self::Upload(error) => write!(f, "could not upload textured skin: {error}"),
            Self::FactorUpload(error) => write!(f, "could not upload factor-only skin: {error}"),
            Self::MorphUpload(error) => write!(f, "could not upload textured morph mesh: {error}"),
        }
    }
}
impl Error for TexturedSkeletalSceneUploadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Upload(error) => Some(error),
            Self::FactorUpload(error) => Some(error),
            Self::MorphUpload(error) => Some(error),
            _ => None,
        }
    }
}

/// Failure while rendering a textured skeletal scene.
#[derive(Debug)]
#[allow(missing_docs)] // The display messages retain the contextual node/texture identifiers.
pub enum TexturedSkeletalSceneRenderError {
    MissingMeshNode(NodeIndex),
    PaletteOwnerHasNoMesh(NodeIndex),
    MissingWorldMatrix(NodeIndex),
    MissingTextureBinding {
        index: ModelTextureIndex,
    },
    MissingGpuTexture {
        index: ModelTextureIndex,
    },
    Draw(TexturedSkinnedMeshRenderError),
    FactorDraw(SkinnedMeshRenderError),
    MorphDraw(TexturedMeshRenderError),
    MissingMorphWeights(NodeIndex),
    MorphWeightCount {
        node: NodeIndex,
        expected: usize,
        actual: usize,
    },
}
impl fmt::Display for TexturedSkeletalSceneRenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingMeshNode(node) => write!(
                f,
                "skin palette references missing mesh node {}",
                node.get()
            ),
            Self::PaletteOwnerHasNoMesh(node) => {
                write!(f, "skin palette owner node {} has no mesh", node.get())
            }
            Self::MissingWorldMatrix(node) => {
                write!(f, "pose has no world matrix for mesh node {}", node.get())
            }
            Self::MissingTextureBinding { index } => write!(
                f,
                "textured skin has no resolved texture binding {}",
                index.get()
            ),
            Self::MissingGpuTexture { index } => write!(
                f,
                "textured skin texture {} is not GPU-resident",
                index.get()
            ),
            Self::Draw(error) => write!(f, "could not draw textured skin: {error}"),
            Self::FactorDraw(error) => write!(f, "could not draw factor-only skin: {error}"),
            Self::MorphDraw(error) => write!(f, "could not draw textured morph mesh: {error}"),
            Self::MissingMorphWeights(node) => {
                write!(f, "pose has no morph weights for node {}", node.get())
            }
            Self::MorphWeightCount {
                node,
                expected,
                actual,
            } => write!(
                f,
                "morph node {} needs {expected} weights but pose supplied {actual}",
                node.get()
            ),
        }
    }
}
impl Error for TexturedSkeletalSceneRenderError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Draw(error) => Some(error),
            Self::FactorDraw(error) => Some(error),
            Self::MorphDraw(error) => Some(error),
            _ => None,
        }
    }
}

impl TexturedSkeletalSceneRenderer3d {
    /// Builds the textured skin GPU state lazily in a render callback.
    ///
    /// # Errors
    ///
    /// Returns an error when a scene skin refers to absent geometry or cannot be uploaded.
    pub fn new_for_frame(
        frame: &RenderFrame<'_>,
        model: &Model,
        scene: &ImportedScene,
    ) -> Result<Self, TexturedSkeletalSceneUploadError> {
        let renderer = TexturedSkinnedMeshRenderer3d::new_for_frame(frame);
        let factor_renderer = SkinnedMeshRenderer3d::new_for_frame(frame);
        let morph_renderer = TexturedMeshRenderer3d::new_for_frame(frame);
        Self::build(
            renderer,
            factor_renderer,
            morph_renderer,
            model,
            scene,
            |renderer, primitive, skin| renderer.upload_mesh_for_frame(frame, primitive, skin),
            |renderer, primitive, skin| renderer.upload_mesh_for_frame(frame, primitive, skin),
            |renderer, primitive| renderer.upload_mesh_for_frame(frame, primitive),
        )
    }
    /// Builds the textured skin GPU state with the persistent renderer.
    ///
    /// # Errors
    ///
    /// Returns an error when a scene skin refers to absent geometry or cannot be uploaded.
    pub fn new(
        renderer: &Renderer,
        model: &Model,
        scene: &ImportedScene,
    ) -> Result<Self, TexturedSkeletalSceneUploadError> {
        let skin_renderer = TexturedSkinnedMeshRenderer3d::new(renderer);
        let factor_renderer = SkinnedMeshRenderer3d::new(renderer);
        let morph_renderer = TexturedMeshRenderer3d::new(renderer);
        Self::build(
            skin_renderer,
            factor_renderer,
            morph_renderer,
            model,
            scene,
            |skin_renderer, primitive, skin| skin_renderer.upload_mesh(renderer, primitive, skin),
            |factor_renderer, primitive, skin| {
                factor_renderer.upload_mesh(renderer, primitive, skin)
            },
            |morph_renderer, primitive| morph_renderer.upload_mesh(renderer, primitive),
        )
    }
    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "skin, factor and morph GPU ownership are validated in one transactional character build"
    )]
    fn build<F, S, M>(
        renderer: TexturedSkinnedMeshRenderer3d,
        factor_renderer: SkinnedMeshRenderer3d,
        morph_renderer: TexturedMeshRenderer3d,
        model: &Model,
        scene: &ImportedScene,
        mut upload: F,
        mut upload_factor: S,
        mut upload_morph: M,
    ) -> Result<Self, TexturedSkeletalSceneUploadError>
    where
        F: FnMut(
            &TexturedSkinnedMeshRenderer3d,
            &MeshPrimitive,
            &ImportedSkinnedPrimitive,
        ) -> Result<GpuTexturedSkinnedMesh, TexturedSkinnedMeshUploadError>,
        S: FnMut(
            &SkinnedMeshRenderer3d,
            &MeshPrimitive,
            &ImportedSkinnedPrimitive,
        ) -> Result<GpuSkinnedMesh, SkinnedMeshUploadError>,
        M: FnMut(
            &TexturedMeshRenderer3d,
            &MeshPrimitive,
        ) -> Result<GpuTexturedMesh, TexturedMeshUploadError>,
    {
        let mut primitives = Vec::with_capacity(scene.skinned_primitives().len());
        let mut factor_only_primitives = 0;
        for skin in scene.skinned_primitives() {
            let mesh = model
                .meshes()
                .get(skin.mesh())
                .ok_or(TexturedSkeletalSceneUploadError::MissingMesh { mesh: skin.mesh() })?;
            let primitive = mesh.primitives().get(skin.primitive()).ok_or(
                TexturedSkeletalSceneUploadError::MissingPrimitive {
                    mesh: skin.mesh(),
                    primitive: skin.primitive(),
                },
            )?;
            let material = primitive
                .material()
                .and_then(|index| model.materials().get(index.get()));
            let base_color = character_base_color(material);
            let color = base_color.factor;
            let double_sided = material.is_some_and(Material::double_sided);
            let alpha_mode = material.map_or(AlphaMode::Opaque, Material::alpha_mode);
            let gpu = match base_color.texture {
                Some(binding) if binding.tex_coord_set() == 0 => {
                    TexturedSkeletalGpuMesh::Textured {
                        mesh: upload(&renderer, primitive, skin)
                            .map_err(TexturedSkeletalSceneUploadError::Upload)?,
                        texture: binding.texture(),
                    }
                }
                // A texture on UV1+ cannot be sampled by this first path. The
                // material factor remains meaningful and is drawn explicitly.
                _ => {
                    factor_only_primitives += 1;
                    TexturedSkeletalGpuMesh::Factor(
                        upload_factor(&factor_renderer, primitive, skin)
                            .map_err(TexturedSkeletalSceneUploadError::FactorUpload)?,
                    )
                }
            };
            primitives.push(TexturedSkeletalGpuPrimitive {
                mesh: skin.mesh(),
                primitive: skin.primitive(),
                gpu,
                color,
                double_sided,
                alpha_mode,
                local_center: primitive_local_center(primitive),
            });
        }
        let mut morph_primitives = Vec::new();
        for morph in scene.morph_primitives() {
            let mesh = model
                .meshes()
                .get(morph.mesh())
                .ok_or(TexturedSkeletalSceneUploadError::MissingMesh { mesh: morph.mesh() })?;
            let primitive = mesh.primitives().get(morph.primitive()).ok_or(
                TexturedSkeletalSceneUploadError::MissingPrimitive {
                    mesh: morph.mesh(),
                    primitive: morph.primitive(),
                },
            )?;
            let material = primitive
                .material()
                .and_then(|index| model.materials().get(index.get()));
            let base_color = character_base_color(material);
            let alpha_mode = material.map_or(AlphaMode::Opaque, Material::alpha_mode);
            if matches!(alpha_mode, AlphaMode::Mask { .. }) {
                return Err(TexturedSkeletalSceneUploadError::AlphaModeUnsupported {
                    mesh: morph.mesh(),
                    primitive: morph.primitive(),
                    alpha_mode,
                });
            }
            let Some(binding) = base_color
                .texture
                .filter(|binding| binding.tex_coord_set() == 0)
            else {
                return Err(TexturedSkeletalSceneUploadError::MissingBaseColorTexture {
                    mesh: morph.mesh(),
                    primitive: morph.primitive(),
                });
            };
            let tex_coords = primitive.tex_coords_0().ok_or(
                TexturedSkeletalSceneUploadError::BaseTextureUvSetUnsupported {
                    mesh: morph.mesh(),
                    primitive: morph.primitive(),
                    actual: 0,
                },
            )?;
            for (node_index, node) in scene.nodes().iter().enumerate() {
                if node.mesh() != Some(morph.mesh()) || node.skin().is_some() {
                    continue;
                }
                morph_primitives.push(TexturedMorphGpuPrimitive {
                    node: NodeIndex::new(node_index),
                    mesh: morph.mesh(),
                    primitive: morph.primitive(),
                    gpu: upload_morph(&morph_renderer, primitive)
                        .map_err(TexturedSkeletalSceneUploadError::MorphUpload)?,
                    texture: binding.texture(),
                    color: base_color.factor,
                    double_sided: material.is_some_and(Material::double_sided),
                    alpha_mode,
                    local_center: primitive_local_center(primitive),
                    base_positions: primitive.positions().to_vec(),
                    tex_coords: tex_coords.to_vec(),
                    position_targets: morph
                        .targets()
                        .iter()
                        .map(|target| target.position_deltas().to_vec())
                        .collect(),
                });
            }
        }
        Ok(Self {
            renderer,
            factor_renderer,
            morph_renderer,
            primitives,
            morph_primitives,
            factor_only_primitives,
            ambient_fill: [0.14, 0.14, 0.16],
            lighting: None,
        })
    }

    /// Multiplicative albedo lift so unlit skins stay readable next to IBL worlds.
    ///
    /// Deprecated path kept for call-site compatibility: prefers
    /// [`Self::with_lighting`] (flat view-independent exposure today).
    #[must_use]
    pub const fn with_ambient_fill(mut self, ambient_fill: [f32; 3]) -> Self {
        self.ambient_fill = ambient_fill;
        self
    }

    /// Sets the shared Lambert key light for every textured skinned draw.
    #[must_use]
    pub const fn with_lighting(mut self, lighting: LambertLighting3d) -> Self {
        self.lighting = Some(lighting);
        self
    }

    fn draw_lighting(&self) -> LambertLighting3d {
        self.lighting.unwrap_or_else(|| {
            LambertLighting3d::artistic(
                [0.25, -1.0, -0.5],
                [1.0, 1.0, 1.0],
                0.0,
                self.ambient_fill,
            )
            .unwrap_or_else(|_| LambertLighting3d::default())
        })
    }

    /// Flat RGB multiplier shared by skinned and morph character draws.
    fn draw_exposure(&self) -> [f32; 3] {
        let lighting = self.draw_lighting();
        let light = lighting.light();
        let ambient = lighting.ambient();
        [
            ambient[0] + light.color[0] * light.illuminance_lux,
            ambient[1] + light.color[1] * light.illuminance_lux,
            ambient[2] + light.color[2] * light.illuminance_lux,
        ]
    }

    fn exposed_color(&self, color: [f32; 4]) -> [f32; 4] {
        let exposure = self.draw_exposure();
        [
            color[0] * exposure[0],
            color[1] * exposure[1],
            color[2] * exposure[2],
            color[3],
        ]
    }

    /// Returns primitives rendered from their source factor without sampling.
    ///
    /// Their source has no image or names a UV set other than zero. They are
    /// still visible; this is not a missing-texture counter.
    #[must_use]
    pub const fn factor_only_primitive_count(&self) -> usize {
        self.factor_only_primitives
    }

    /// Draws a sampled character through an explicit reusable visibility mask.
    ///
    /// This is the intended first-person path: hide separately authored head,
    /// eye and hair primitives once during setup, then keep the camera at its
    /// animated eye anchor without rendering enclosing head geometry. The mask
    /// does not move the camera and does not modify the animation snapshot.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::draw`].
    pub fn draw_with_visibility(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        scene: &ImportedScene,
        pose: &AnimationSnapshot,
        textures: SkeletalTextureResources<'_>,
        visibility: &SkeletalVisibilityMask3d,
    ) -> Result<MeshDrawStats, TexturedSkeletalSceneRenderError> {
        self.draw_with_root_transform_depth_load_and_visibility(
            frame,
            camera,
            scene,
            pose,
            textures,
            [
                1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0,
            ],
            DepthLoad::Clear,
            visibility,
        )
    }

    /// Draws all textured skin primitives for a sampled pose.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid pose or an unresolved/non-resident texture.
    #[allow(clippy::too_many_lines)] // Opaque and sorted alpha phases stay adjacent for review.
    pub fn draw(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        scene: &ImportedScene,
        pose: &AnimationSnapshot,
        textures: SkeletalTextureResources<'_>,
    ) -> Result<MeshDrawStats, TexturedSkeletalSceneRenderError> {
        self.draw_with_root_transform_and_depth_load(
            frame,
            camera,
            scene,
            pose,
            textures,
            [
                1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
            ],
            DepthLoad::Clear,
        )
    }

    /// Draws a sampled character while preserving or resetting an existing
    /// opaque depth phase.
    ///
    /// Use [`DepthLoad::Load`] when composing the character after a world
    /// renderer in the same frame. This keeps walls and other nearer world
    /// geometry in front of the character.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::draw`].
    pub fn draw_with_depth_load(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        scene: &ImportedScene,
        pose: &AnimationSnapshot,
        textures: SkeletalTextureResources<'_>,
        depth_load: DepthLoad,
    ) -> Result<MeshDrawStats, TexturedSkeletalSceneRenderError> {
        self.draw_with_root_transform_and_depth_load(
            frame,
            camera,
            scene,
            pose,
            textures,
            [
                1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
            ],
            depth_load,
        )
    }

    /// Draws a visibility-filtered character in an explicit depth phase.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::draw`].
    #[allow(clippy::too_many_arguments)]
    pub fn draw_with_depth_load_and_visibility(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        scene: &ImportedScene,
        pose: &AnimationSnapshot,
        textures: SkeletalTextureResources<'_>,
        depth_load: DepthLoad,
        visibility: &SkeletalVisibilityMask3d,
    ) -> Result<MeshDrawStats, TexturedSkeletalSceneRenderError> {
        self.draw_with_root_transform_depth_load_and_visibility(
            frame,
            camera,
            scene,
            pose,
            textures,
            [
                1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0,
            ],
            depth_load,
            visibility,
        )
    }

    /// Draws a sampled character under one model-to-world root transform.
    ///
    /// The transform is applied to every skinned and morph node without
    /// mutating the reusable animation snapshot. [`DepthLoad::Load`] composes
    /// the character into a depth phase started by an earlier world renderer.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid pose, root transform or unresolved
    /// texture.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub fn draw_with_root_transform_and_depth_load(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        scene: &ImportedScene,
        pose: &AnimationSnapshot,
        textures: SkeletalTextureResources<'_>,
        root_transform: [f32; 16],
        depth_load: DepthLoad,
    ) -> Result<MeshDrawStats, TexturedSkeletalSceneRenderError> {
        self.draw_with_root_transform_depth_load_and_visibility(
            frame,
            camera,
            scene,
            pose,
            textures,
            root_transform,
            depth_load,
            &SkeletalVisibilityMask3d::new(),
        )
    }

    /// Draws a visibility-filtered sampled character under a root transform.
    ///
    /// This most explicit composition API combines first-person part filtering,
    /// world placement and depth ownership without mutating source assets.
    /// Visibility lookup performs no frame-time allocation.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::draw_with_root_transform_and_depth_load`].
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub fn draw_with_root_transform_depth_load_and_visibility(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        scene: &ImportedScene,
        pose: &AnimationSnapshot,
        textures: SkeletalTextureResources<'_>,
        root_transform: [f32; 16],
        depth_load: DepthLoad,
        visibility: &SkeletalVisibilityMask3d,
    ) -> Result<MeshDrawStats, TexturedSkeletalSceneRenderError> {
        let mut result = MeshDrawStats::default();
        let mut has_depth = matches!(depth_load, DepthLoad::Load);
        let mut transparent = Vec::<TexturedCharacterTransparent<'_>>::new();
        for palette in pose.skin_palettes() {
            let node_index = palette.mesh_node();
            let node = scene.nodes().get(node_index.get()).ok_or(
                TexturedSkeletalSceneRenderError::MissingMeshNode(node_index),
            )?;
            let mesh_index =
                node.mesh()
                    .ok_or(TexturedSkeletalSceneRenderError::PaletteOwnerHasNoMesh(
                        node_index,
                    ))?;
            let matrix = *pose.world_matrices().get(node_index.get()).ok_or(
                TexturedSkeletalSceneRenderError::MissingWorldMatrix(node_index),
            )?;
            let matrix = multiply_matrix4(root_transform, matrix);
            for primitive in self
                .primitives
                .iter()
                .filter(|primitive| primitive.mesh == mesh_index)
            {
                let identity =
                    SkeletalPrimitive3d::new(node_index, mesh_index, primitive.primitive);
                if !visibility.is_visible(identity) {
                    continue;
                }
                if primitive.alpha_mode == AlphaMode::Blend {
                    let center = transform_point(matrix, primitive.local_center);
                    transparent.push(TexturedCharacterTransparent::Skin {
                        primitive,
                        palette,
                        matrix,
                        distance: squared_distance(camera.position, center),
                    });
                    continue;
                }
                // With no source image, mask alpha is the constant material
                // factor. It is either fully absent or an ordinary depth draw.
                if let AlphaMode::Mask { cutoff } = primitive.alpha_mode
                    && primitive.color[3] < cutoff
                {
                    continue;
                }
                let depth_load = if has_depth {
                    DepthLoad::Load
                } else {
                    DepthLoad::Clear
                };
                let draw = match &primitive.gpu {
                    TexturedSkeletalGpuMesh::Textured { mesh, texture } => {
                        let texture = textures.resolve(*texture)?;
                        self.renderer
                            .draw_with_depth_load(
                                frame,
                                camera,
                                mesh,
                                palette,
                                matrix,
                                TexturedSkinnedMaterial3d::new(texture, primitive.color)
                                    .with_double_sided(primitive.double_sided)
                                    .with_alpha_mode(primitive.alpha_mode)
                                    .with_lighting(self.draw_lighting()),
                                depth_load,
                            )
                            .map_err(TexturedSkeletalSceneRenderError::Draw)?
                    }
                    TexturedSkeletalGpuMesh::Factor(mesh) => self
                        .factor_renderer
                        .draw_material_with_model_matrix_depth_load(
                            frame,
                            camera,
                            mesh,
                            palette,
                            matrix,
                            SkinnedMaterial3d::new(self.exposed_color(primitive.color))
                                .with_double_sided(primitive.double_sided),
                            depth_load,
                        )
                        .map_err(TexturedSkeletalSceneRenderError::FactorDraw)?,
                };
                has_depth = true;
                result.triangles += draw.triangles;
                result.draw_calls += draw.draw_calls;
            }
        }
        for primitive in &self.morph_primitives {
            let identity =
                SkeletalPrimitive3d::new(primitive.node, primitive.mesh, primitive.primitive);
            if !visibility.is_visible(identity) {
                continue;
            }
            let weights = pose.morph_weights(primitive.node).ok_or(
                TexturedSkeletalSceneRenderError::MissingMorphWeights(primitive.node),
            )?;
            if weights.len() != primitive.position_targets.len() {
                return Err(TexturedSkeletalSceneRenderError::MorphWeightCount {
                    node: primitive.node,
                    expected: primitive.position_targets.len(),
                    actual: weights.len(),
                });
            }
            let vertices = primitive
                .base_positions
                .iter()
                .copied()
                .zip(primitive.tex_coords.iter().copied())
                .enumerate()
                .map(|(vertex, (mut position, tex_coord))| {
                    for (target, weight) in primitive.position_targets.iter().zip(weights) {
                        let delta = target[vertex];
                        position[0] += delta[0] * weight;
                        position[1] += delta[1] * weight;
                        position[2] += delta[2] * weight;
                    }
                    TexturedVertex {
                        position,
                        tex_coord,
                    }
                })
                .collect::<Vec<_>>();
            frame.queue().write_buffer(
                &primitive.gpu.vertex_buffer,
                0,
                bytemuck::cast_slice(&vertices),
            );
            let matrix = *pose.world_matrices().get(primitive.node.get()).ok_or(
                TexturedSkeletalSceneRenderError::MissingWorldMatrix(primitive.node),
            )?;
            let matrix = multiply_matrix4(root_transform, matrix);
            if primitive.alpha_mode == AlphaMode::Blend {
                let center = transform_point(matrix, primitive.local_center);
                transparent.push(TexturedCharacterTransparent::Morph {
                    primitive,
                    matrix,
                    distance: squared_distance(camera.position, center),
                });
                continue;
            }
            if let AlphaMode::Mask { cutoff } = primitive.alpha_mode
                && primitive.color[3] < cutoff
            {
                continue;
            }
            let texture = textures.resolve(primitive.texture)?;
            let draw = self
                .morph_renderer
                .draw_with_depth_load_rasterization_phase(
                    frame,
                    camera,
                    &primitive.gpu,
                    matrix,
                    TexturedMaterial3d::new(texture, self.exposed_color(primitive.color)),
                    if has_depth {
                        DepthLoad::Load
                    } else {
                        DepthLoad::Clear
                    },
                    primitive.double_sided,
                    false,
                )
                .map_err(TexturedSkeletalSceneRenderError::MorphDraw)?;
            has_depth = true;
            result.triangles += draw.triangles;
            result.draw_calls += draw.draw_calls;
        }
        transparent.sort_by(|left, right| {
            right
                .distance()
                .total_cmp(&left.distance())
                .then_with(|| left.sort_key().cmp(&right.sort_key()))
        });
        for transparent_draw in transparent {
            let depth_load = if has_depth {
                DepthLoad::Load
            } else {
                DepthLoad::Clear
            };
            let draw = match transparent_draw {
                TexturedCharacterTransparent::Skin {
                    primitive,
                    palette,
                    matrix,
                    ..
                } => match &primitive.gpu {
                    TexturedSkeletalGpuMesh::Textured { mesh, texture } => {
                        let texture = textures.resolve(*texture)?;
                        self.renderer
                            .draw_transparent_with_depth_load(
                                frame,
                                camera,
                                mesh,
                                palette,
                                matrix,
                                TexturedSkinnedMaterial3d::new(texture, primitive.color)
                                    .with_double_sided(primitive.double_sided)
                                    .with_alpha_mode(primitive.alpha_mode)
                                    .with_lighting(self.draw_lighting()),
                                depth_load,
                            )
                            .map_err(TexturedSkeletalSceneRenderError::Draw)?
                    }
                    TexturedSkeletalGpuMesh::Factor(mesh) => self
                        .factor_renderer
                        .draw_transparent_material_with_model_matrix_depth_load(
                            frame,
                            camera,
                            mesh,
                            palette,
                            matrix,
                            SkinnedMaterial3d::new(self.exposed_color(primitive.color))
                                .with_double_sided(primitive.double_sided),
                            depth_load,
                        )
                        .map_err(TexturedSkeletalSceneRenderError::FactorDraw)?,
                },
                TexturedCharacterTransparent::Morph {
                    primitive, matrix, ..
                } => {
                    let texture = textures.resolve(primitive.texture)?;
                    self.morph_renderer
                        .draw_with_depth_load_rasterization_phase(
                            frame,
                            camera,
                            &primitive.gpu,
                            matrix,
                            TexturedMaterial3d::new(texture, self.exposed_color(primitive.color)),
                            depth_load,
                            primitive.double_sided,
                            true,
                        )
                        .map_err(TexturedSkeletalSceneRenderError::MorphDraw)?
                }
            };
            has_depth = true;
            result.triangles += draw.triangles;
            result.draw_calls += draw.draw_calls;
        }
        Ok(result)
    }
}

/// Failure while preparing one [`SkeletalSceneRenderer3d`].
#[derive(Debug)]
pub enum SkeletalSceneUploadError {
    /// A skin stream refers to a mesh absent from the accompanying model.
    MissingMesh {
        /// Referenced source mesh index.
        mesh: usize,
    },
    /// A skin stream refers to a primitive absent from its source mesh.
    MissingPrimitive {
        /// Referenced source mesh index.
        mesh: usize,
        /// Referenced primitive index.
        primitive: usize,
    },
    /// The low-level GPU upload rejected the imported geometry.
    Upload(SkinnedMeshUploadError),
}

impl fmt::Display for SkeletalSceneUploadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingMesh { mesh } => write!(
                formatter,
                "skinned primitive references missing mesh {mesh}"
            ),
            Self::MissingPrimitive { mesh, primitive } => write!(
                formatter,
                "skinned primitive references missing primitive {primitive} in mesh {mesh}"
            ),
            Self::Upload(error) => write!(formatter, "could not upload skinned primitive: {error}"),
        }
    }
}

impl Error for SkeletalSceneUploadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Upload(error) => Some(error),
            Self::MissingMesh { .. } | Self::MissingPrimitive { .. } => None,
        }
    }
}

/// Failure while drawing a sampled glTF skeleton through
/// [`SkeletalSceneRenderer3d`].
#[derive(Debug)]
pub enum SkeletalSceneRenderError {
    /// A sampled palette references a node no longer present in the scene.
    MissingMeshNode(NodeIndex),
    /// The palette owner is not a mesh node.
    PaletteOwnerHasNoMesh(NodeIndex),
    /// The pose does not contain the palette owner's world matrix.
    MissingWorldMatrix(NodeIndex),
    /// The low-level draw rejected the pose or GPU draw data.
    Draw(SkinnedMeshRenderError),
}

impl fmt::Display for SkeletalSceneRenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingMeshNode(node) => write!(
                formatter,
                "skin palette references missing mesh node {}",
                node.get()
            ),
            Self::PaletteOwnerHasNoMesh(node) => write!(
                formatter,
                "skin palette owner node {} has no mesh",
                node.get()
            ),
            Self::MissingWorldMatrix(node) => write!(
                formatter,
                "pose has no world matrix for mesh node {}",
                node.get()
            ),
            Self::Draw(error) => write!(formatter, "could not draw skinned primitive: {error}"),
        }
    }
}

impl Error for SkeletalSceneRenderError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Draw(error) => Some(error),
            Self::MissingMeshNode(_)
            | Self::PaletteOwnerHasNoMesh(_)
            | Self::MissingWorldMatrix(_) => None,
        }
    }
}

impl SkeletalSceneRenderer3d {
    /// Builds and uploads every skinned primitive of an imported asset.
    ///
    /// This frame-bound constructor is suitable for a lazily-created example
    /// renderer. Applications with setup-time access to [`Renderer`] can use
    /// [`Self::new`] instead and avoid first-frame uploads.
    ///
    /// # Errors
    ///
    /// Returns [`SkeletalSceneUploadError`] when the model/skin association is
    /// inconsistent or its data cannot fit the first GPU skinning format.
    pub fn new_for_frame(
        frame: &RenderFrame<'_>,
        model: &Model,
        scene: &ImportedScene,
    ) -> Result<Self, SkeletalSceneUploadError> {
        let renderer = SkinnedMeshRenderer3d::new_for_frame(frame);
        Self::build(renderer, model, scene, |renderer, primitive, skin| {
            renderer.upload_mesh_for_frame(frame, primitive, skin)
        })
    }

    /// Builds and uploads every skinned primitive through the application's
    /// persistent [`Renderer`].
    ///
    /// # Errors
    ///
    /// Returns [`SkeletalSceneUploadError`] when the model/skin association is
    /// inconsistent or its data cannot fit the first GPU skinning format.
    pub fn new(
        renderer: &Renderer,
        model: &Model,
        scene: &ImportedScene,
    ) -> Result<Self, SkeletalSceneUploadError> {
        let skin_renderer = SkinnedMeshRenderer3d::new(renderer);
        Self::build(
            skin_renderer,
            model,
            scene,
            |skin_renderer, primitive, skin| skin_renderer.upload_mesh(renderer, primitive, skin),
        )
    }

    fn build<F>(
        renderer: SkinnedMeshRenderer3d,
        model: &Model,
        scene: &ImportedScene,
        mut upload: F,
    ) -> Result<Self, SkeletalSceneUploadError>
    where
        F: FnMut(
            &SkinnedMeshRenderer3d,
            &MeshPrimitive,
            &ImportedSkinnedPrimitive,
        ) -> Result<GpuSkinnedMesh, SkinnedMeshUploadError>,
    {
        let mut primitives = Vec::with_capacity(scene.skinned_primitives().len());
        for skin in scene.skinned_primitives() {
            let mesh = model
                .meshes()
                .get(skin.mesh())
                .ok_or(SkeletalSceneUploadError::MissingMesh { mesh: skin.mesh() })?;
            let primitive = mesh.primitives().get(skin.primitive()).ok_or(
                SkeletalSceneUploadError::MissingPrimitive {
                    mesh: skin.mesh(),
                    primitive: skin.primitive(),
                },
            )?;
            let color = primitive
                .material()
                .and_then(|index| model.materials().get(index.get()))
                .map_or([0.82, 0.82, 0.86, 1.0], |material| {
                    let mut color = material.base_color_factor();
                    color[3] = 1.0;
                    color
                });
            primitives.push(SkeletalGpuPrimitive {
                mesh: skin.mesh(),
                primitive: skin.primitive(),
                gpu: upload(&renderer, primitive, skin)
                    .map_err(SkeletalSceneUploadError::Upload)?,
                color,
            });
        }
        Ok(Self {
            renderer,
            primitives,
        })
    }

    /// Draws all skinned primitives belonging to the sampled pose.
    ///
    /// The source node's exact glTF world matrix is retained, so matrix nodes
    /// and animated hierarchy transforms do not round-trip through Euler
    /// angles. A scene with no skinned primitives returns zero counters.
    ///
    /// # Errors
    ///
    /// Returns [`SkeletalSceneRenderError`] when the snapshot is not from this
    /// scene or the GPU path rejects a palette/draw input.
    pub fn draw(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        scene: &ImportedScene,
        pose: &AnimationSnapshot,
    ) -> Result<MeshDrawStats, SkeletalSceneRenderError> {
        self.draw_with_visibility(frame, camera, scene, pose, &SkeletalVisibilityMask3d::new())
    }

    /// Draws factor-only skinned primitives through a visibility mask.
    ///
    /// Use the same mask as [`TexturedSkeletalSceneRenderer3d::draw_with_visibility`]
    /// when switching material paths for one first-person character.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::draw`].
    pub fn draw_with_visibility(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        scene: &ImportedScene,
        pose: &AnimationSnapshot,
        visibility: &SkeletalVisibilityMask3d,
    ) -> Result<MeshDrawStats, SkeletalSceneRenderError> {
        let mut result = MeshDrawStats::default();
        let mut has_depth = false;
        for palette in pose.skin_palettes() {
            let node_index = palette.mesh_node();
            let node = scene
                .nodes()
                .get(node_index.get())
                .ok_or(SkeletalSceneRenderError::MissingMeshNode(node_index))?;
            let mesh_index = node
                .mesh()
                .ok_or(SkeletalSceneRenderError::PaletteOwnerHasNoMesh(node_index))?;
            let matrix = *pose
                .world_matrices()
                .get(node_index.get())
                .ok_or(SkeletalSceneRenderError::MissingWorldMatrix(node_index))?;
            for primitive in self
                .primitives
                .iter()
                .filter(|primitive| primitive.mesh == mesh_index)
            {
                let identity =
                    SkeletalPrimitive3d::new(node_index, mesh_index, primitive.primitive);
                if !visibility.is_visible(identity) {
                    continue;
                }
                let draw = self
                    .renderer
                    .draw_with_model_matrix_depth_load(
                        frame,
                        camera,
                        &primitive.gpu,
                        palette,
                        matrix,
                        primitive.color,
                        if has_depth {
                            DepthLoad::Load
                        } else {
                            DepthLoad::Clear
                        },
                    )
                    .map_err(SkeletalSceneRenderError::Draw)?;
                has_depth = true;
                result.triangles += draw.triangles;
                result.draw_calls += draw.draw_calls;
            }
        }
        Ok(result)
    }
}

/// High-level bridge from an ECS 3D snapshot to cached GPU mesh resources.
///
/// Construct this next to [`MeshRenderer3d`] during renderer setup, then pass
/// an [`ExtractedModels`] snapshot and the CPU [`Assets<Model>`] store once per
/// frame. The bridge uploads every primitive of a model the first time its
/// current [`ModelHandle`] is encountered and keeps those immutable buffers in
/// a generational-handle cache.
///
/// # Cache lifetime
///
/// A removed model receives a new asset generation when its slot is reused, so
/// an old cached entry cannot be selected by the replacement. `Assets` has no
/// asset-change revision, however: mutating a resident `Model` through
/// [`Assets::get_mut`] does **not** refresh its cached GPU meshes. Call
/// [`Self::invalidate_model`] after such a mutation, or
/// [`Self::clear_model_cache`] when rebuilding a whole level. This deliberate
/// explicit policy prevents hidden frame-time asset comparisons.
///
/// # Current render phase
///
/// Draw order exactly follows the extracted order. Every primitive is drawn
/// separately using the unlit depth-writing pipeline. The bridge clears depth
/// once at the start of each call, then loads it for every primitive; opaque
/// visibility is therefore independent of extraction order.
pub struct SceneRenderer3d {
    meshes: MeshRenderer3d,
    cached_models: HashMap<ModelHandle, Vec<Vec<SceneGpuPrimitive>>>,
}

struct SceneGpuPrimitive {
    mesh: GpuMesh,
    double_sided: bool,
}

impl SceneRenderer3d {
    /// Creates an empty scene bridge for `renderer`'s surface format.
    #[must_use]
    pub fn new(renderer: &Renderer) -> Self {
        Self {
            meshes: MeshRenderer3d::new(renderer),
            cached_models: HashMap::new(),
        }
    }

    /// Creates a scene renderer lazily from the device attached to `frame`.
    ///
    /// This permits a native `yuyib_app::Application` render callback to own
    /// the scene bridge without exposing a second renderer lifecycle.
    #[must_use]
    pub fn new_for_frame(frame: &RenderFrame<'_>) -> Self {
        Self {
            meshes: MeshRenderer3d::new_for_frame(frame),
            cached_models: HashMap::new(),
        }
    }

    /// Returns the low-level mesh renderer owned by this bridge.
    ///
    /// Use it only for an explicitly separate render phase. Drawing a cached
    /// scene through [`Self::draw`] is preferred because it preserves the ECS
    /// snapshot ordering and asset cache contract.
    #[must_use]
    pub const fn mesh_renderer(&self) -> &MeshRenderer3d {
        &self.meshes
    }

    /// Returns how many distinct model handles currently have GPU residency.
    #[must_use]
    pub fn cached_model_count(&self) -> usize {
        self.cached_models.len()
    }

    /// Discards cached GPU buffers for one model handle.
    ///
    /// Returns `true` when a resident entry was removed. Call this after
    /// changing a model in place with [`Assets::get_mut`]. The old GPU buffers
    /// are released when WGPU drops them; the next draw uploads fresh buffers.
    pub fn invalidate_model(&mut self, model: ModelHandle) -> bool {
        self.cached_models.remove(&model).is_some()
    }

    /// Discards every cached GPU model.
    ///
    /// This is useful after a bulk asset reload or a renderer/device rebuild.
    pub fn clear_model_cache(&mut self) {
        self.cached_models.clear();
    }

    /// Ensures a model has GPU buffers without issuing a draw.
    ///
    /// Returns `true` only when this call uploaded the model. It is useful for
    /// loading screens that want to move uploads outside the presentation
    /// frame.
    ///
    /// # Errors
    ///
    /// Returns [`SceneRenderError`] when `model` is stale or missing, or when
    /// one of its primitives cannot be represented by the unlit GPU pipeline.
    pub fn prepare_model(
        &mut self,
        renderer: &Renderer,
        models: &Assets<Model>,
        model: ModelHandle,
    ) -> Result<bool, SceneRenderError> {
        if self.cached_models.contains_key(&model) {
            return Ok(false);
        }
        let gpu_meshes = self.upload_model(renderer, models, model)?;
        self.cached_models.insert(model, gpu_meshes);
        Ok(true)
    }

    /// Ensures a model is resident using the GPU device attached to `frame`.
    ///
    /// This is the render-frame counterpart to [`Self::prepare_model`].
    /// Applications that expose [`Renderer`] during loading should prefer the
    /// latter to move uploads out of a presentation frame.
    ///
    /// # Errors
    ///
    /// Returns [`SceneRenderError`] under the same conditions as
    /// [`Self::prepare_model`].
    pub fn prepare_model_for_frame(
        &mut self,
        frame: &RenderFrame<'_>,
        models: &Assets<Model>,
        model: ModelHandle,
    ) -> Result<bool, SceneRenderError> {
        if self.cached_models.contains_key(&model) {
            return Ok(false);
        }
        let gpu_meshes = self.upload_model_for_frame(frame, models, model)?;
        self.cached_models.insert(model, gpu_meshes);
        Ok(true)
    }

    /// Draws an ECS scene snapshot in its deterministic extraction order.
    ///
    /// Models may contain multiple meshes and primitives; each primitive
    /// becomes one indexed GPU draw. The colour is currently solid white
    /// because the first renderer does not consume model material data yet.
    ///
    /// # Errors
    ///
    /// Returns [`SceneRenderError`] for a stale/missing model asset, an upload
    /// failure, a malformed transform, or an invalid camera.
    pub fn draw(
        &mut self,
        renderer: &Renderer,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        models: &Assets<Model>,
        scene: &ExtractedModels,
    ) -> Result<SceneDrawStats, SceneRenderError> {
        let mut cache_misses = 0;
        for batch in scene.batches() {
            if self.prepare_model(renderer, models, batch.model())? {
                cache_misses += 1;
            }
        }
        let mut stats = self.draw_prepared(frame, camera, scene)?;
        stats.cache_misses = cache_misses;
        Ok(stats)
    }

    /// Draws a scene using the GPU device attached to `frame` for any first-use upload.
    ///
    /// This makes the scene bridge usable directly from
    /// `yuyib_app::Application::on_render`. Repeated frames reuse the same
    /// cache and perform no uploads unless it is invalidated.
    ///
    /// # Errors
    ///
    /// Returns [`SceneRenderError`] for missing assets, invalid mesh selection,
    /// upload failure, camera validation failure or invalid draw data.
    pub fn draw_for_frame(
        &mut self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        models: &Assets<Model>,
        scene: &ExtractedModels,
    ) -> Result<SceneDrawStats, SceneRenderError> {
        let mut cache_misses = 0;
        for batch in scene.batches() {
            if self.prepare_model_for_frame(frame, models, batch.model())? {
                cache_misses += 1;
            }
        }
        let mut stats = self.draw_prepared(frame, camera, scene)?;
        stats.cache_misses = cache_misses;
        Ok(stats)
    }

    fn draw_prepared(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        scene: &ExtractedModels,
    ) -> Result<SceneDrawStats, SceneRenderError> {
        let mut stats = SceneDrawStats::default();
        MeshRenderer3d::begin_depth_phase(frame);
        for batch in scene.batches() {
            let gpu_meshes =
                self.cached_models
                    .get(&batch.model())
                    .ok_or(SceneRenderError::MissingModel {
                        model: batch.model(),
                    })?;
            for draw in batch.draws() {
                if let Some(mesh) = draw.mesh
                    && mesh >= gpu_meshes.len()
                {
                    return Err(SceneRenderError::MissingMesh {
                        model: batch.model(),
                        mesh,
                    });
                }
                stats.model_instances += 1;
                for (mesh_index, primitives) in gpu_meshes.iter().enumerate() {
                    if draw.mesh.is_some_and(|requested| requested != mesh_index) {
                        continue;
                    }
                    for primitive in primitives {
                        let draw_stats = self
                            .meshes
                            .draw_with_model_matrix_depth_load_rasterization(
                                frame,
                                camera,
                                &primitive.mesh,
                                draw.model_matrix,
                                [1.0, 1.0, 1.0, 1.0],
                                DepthLoad::Load,
                                primitive.double_sided,
                            )
                            .map_err(SceneRenderError::MeshRender)?;
                        stats.primitive_draws += 1;
                        stats.triangles += u64::from(draw_stats.triangles);
                        stats.draw_calls += u64::from(draw_stats.draw_calls);
                    }
                }
            }
        }
        Ok(stats)
    }

    fn upload_model(
        &self,
        renderer: &Renderer,
        models: &Assets<Model>,
        model: ModelHandle,
    ) -> Result<Vec<Vec<SceneGpuPrimitive>>, SceneRenderError> {
        let source = models
            .get(model)
            .ok_or(SceneRenderError::MissingModel { model })?;
        let mut gpu_meshes = Vec::with_capacity(source.meshes().len());
        for (mesh_index, mesh) in source.meshes().iter().enumerate() {
            let mut primitives = Vec::with_capacity(mesh.primitives().len());
            for (primitive_index, primitive) in mesh.primitives().iter().enumerate() {
                let gpu_mesh = self
                    .meshes
                    .upload_mesh(renderer, primitive)
                    .map_err(|source| SceneRenderError::MeshUpload {
                        model,
                        mesh_index,
                        primitive_index,
                        source,
                    })?;
                let double_sided = primitive
                    .material()
                    .and_then(|index| source.materials().get(index.get()))
                    .is_some_and(yuyib_model::Material::double_sided);
                primitives.push(SceneGpuPrimitive {
                    mesh: gpu_mesh,
                    double_sided,
                });
            }
            gpu_meshes.push(primitives);
        }
        Ok(gpu_meshes)
    }

    fn upload_model_for_frame(
        &self,
        frame: &RenderFrame<'_>,
        models: &Assets<Model>,
        model: ModelHandle,
    ) -> Result<Vec<Vec<SceneGpuPrimitive>>, SceneRenderError> {
        let source = models
            .get(model)
            .ok_or(SceneRenderError::MissingModel { model })?;
        let mut gpu_meshes = Vec::with_capacity(source.meshes().len());
        for (mesh_index, mesh) in source.meshes().iter().enumerate() {
            let mut primitives = Vec::with_capacity(mesh.primitives().len());
            for (primitive_index, primitive) in mesh.primitives().iter().enumerate() {
                let gpu_mesh = self
                    .meshes
                    .upload_mesh_for_frame(frame, primitive)
                    .map_err(|source| SceneRenderError::MeshUpload {
                        model,
                        mesh_index,
                        primitive_index,
                        source,
                    })?;
                let double_sided = primitive
                    .material()
                    .and_then(|index| source.materials().get(index.get()))
                    .is_some_and(yuyib_model::Material::double_sided);
                primitives.push(SceneGpuPrimitive {
                    mesh: gpu_mesh,
                    double_sided,
                });
            }
            gpu_meshes.push(primitives);
        }
        Ok(gpu_meshes)
    }
}

/// Statistics produced by [`SceneRenderer3d::draw`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SceneDrawStats {
    /// Number of model primitives encountered for visible entity instances.
    pub model_instances: u64,
    /// Number of primitive draw requests emitted. It currently equals
    /// [`Self::model_instances`] times the primitive count of each model.
    pub primitive_draws: u64,
    /// Number of indexed triangles issued.
    pub triangles: u64,
    /// Number of GPU draw calls issued.
    pub draw_calls: u64,
    /// Number of models uploaded to the cache during this call.
    pub cache_misses: u64,
    /// Number of GPU render passes recorded by this scene call.
    ///
    /// This is intentionally separate from [`Self::draw_calls`]: many indexed
    /// draws in one pass are cheap compared with opening a new pass for every
    /// primitive.
    pub render_passes: u64,
    /// Number of sampled-material bind groups created while preparing models.
    ///
    /// A steady scene should report zero after its first frame. This lets an
    /// application catch accidental per-frame GPU material allocation.
    pub material_bind_group_creations: u64,
    /// Draw requests promoted from exporter-authored `BLEND` to opaque by the
    /// high-level effectively-opaque policy.
    pub promoted_blend_draws: u64,
    /// Transient uniform buffers allocated while encoding this frame's draws.
    ///
    /// Textured PBR batches currently create one immutable uniform buffer per
    /// batch so opaque/transparent passes cannot overwrite each other before
    /// submit. Non-zero every frame is expected until a ring/slab allocator
    /// lands; the counter exists so regressions (e.g. one buffer per draw)
    /// are visible in diagnostics.
    pub transient_uniform_buffer_allocations: u64,
}

impl SceneDrawStats {
    /// Compact one-line summary for consoles and editor overlays.
    #[must_use]
    pub fn summary_line(&self) -> String {
        format!(
            "draws={} passes={} tris={} primitives={} cache_miss={} mat_bg={} promoted_blend={} transient_ubo={}",
            self.draw_calls,
            self.render_passes,
            self.triangles,
            self.primitive_draws,
            self.cache_misses,
            self.material_bind_group_creations,
            self.promoted_blend_draws,
            self.transient_uniform_buffer_allocations,
        )
    }
}

/// Failure while resolving a CPU model asset or drawing an extracted scene.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SceneRenderError {
    /// The extracted model handle is stale or absent from the supplied store.
    MissingModel {
        /// Handle requested by the extracted scene.
        model: ModelHandle,
    },
    /// An extracted scene node selected a mesh absent from its model.
    MissingMesh {
        /// Model requested by the extracted scene.
        model: ModelHandle,
        /// Source mesh index requested by that node.
        mesh: usize,
    },
    /// One CPU primitive could not be uploaded into the current GPU format.
    MeshUpload {
        /// Model containing the failing primitive.
        model: ModelHandle,
        /// Index in [`Model::meshes`].
        mesh_index: usize,
        /// Index in [`yuyib_model::Mesh::primitives`].
        primitive_index: usize,
        /// Underlying GPU upload failure.
        source: MeshUploadError,
    },
    /// A scene transform or the downstream draw data was invalid.
    MeshRender(MeshRenderError),
}

impl fmt::Display for SceneRenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingModel { model } => {
                write!(formatter, "extracted model asset is missing: {model:?}")
            }
            Self::MissingMesh { model, mesh } => write!(
                formatter,
                "model asset {model:?} has no source mesh at index {mesh}"
            ),
            Self::MeshUpload {
                model,
                mesh_index,
                primitive_index,
                source,
            } => write!(
                formatter,
                "cannot upload model {model:?}, mesh {mesh_index}, primitive {primitive_index}: {source}"
            ),
            Self::MeshRender(source) => write!(formatter, "cannot draw extracted scene: {source}"),
        }
    }
}

impl Error for SceneRenderError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::MeshUpload { source, .. } => Some(source),
            Self::MeshRender(source) => Some(source),
            Self::MissingModel { .. } | Self::MissingMesh { .. } => None,
        }
    }
}

/// Высокоуровневый рендерер непрозрачной ECS-сцены с текстурами и Lambert-светом.
///
/// Это обычный путь для карты: он один раз загружает меши и изображения модели,
/// затем сам применяет один направленный свет ко всем извлечённым экземплярам.
/// Цвет текстуры сохраняется; зелёный оттенок, например, задаётся цветом
/// [`DirectionalLightDraw`], а не пост-обработкой.  Прозрачность, normal maps,
/// тени и PBR намеренно не входят в этот первый проход.
///
/// Для нескольких светильников, теней либо собственного shader graph берите
/// низкоуровневые [`TexturedLitMeshRenderer3d`] и [`LitMeshRenderer3d`].
pub struct LitSceneRenderer3d {
    standard: StandardRenderer3d,
    texture_loader: ModelTextureLoader,
    texture_assets: Assets<Texture>,
    texture_cache: TextureCache,
    cached_models: HashMap<ModelHandle, LitSceneGpuModel>,
    prepared_models: HashMap<ModelHandle, PreparedModelTextures>,
    preparing_models: HashMap<ModelHandle, LitSceneGpuModelUpload>,
    unbound_material_policy: UnboundMaterialPolicy3d,
}

/// Per-frame render-thread budget for publishing one streamed 3D model.
///
/// Texture slots and primitives are hard limits. `target_geometry_bytes` is a
/// soft scheduling target measured from source vertex/index streams: a
/// primitive is the smallest atomic upload supported by the current WGPU mesh
/// API, so the first primitive of a frame is allowed to exceed that target.
/// Set any field to zero to pause that part of publication deliberately.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModelUploadBudget3d {
    /// Maximum decoded texture slots published this frame.
    pub maximum_texture_slots: usize,
    /// Target unique decoded RGBA8 bytes uploaded as textures this frame.
    pub target_texture_bytes: u64,
    /// Maximum complete mesh primitives published this frame.
    pub maximum_primitives: usize,
    /// Target source geometry bytes published this frame.
    pub target_geometry_bytes: u64,
}

impl Default for ModelUploadBudget3d {
    fn default() -> Self {
        Self {
            maximum_texture_slots: 4,
            target_texture_bytes: 16 * 1024 * 1024,
            maximum_primitives: 8,
            target_geometry_bytes: 8 * 1024 * 1024,
        }
    }
}

/// Exact progress of one streamed model's render-thread publication.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ModelUploadProgress3d {
    /// Whether all textures, material bindings and primitives are resident.
    pub ready: bool,
    /// Texture slots already uploaded.
    pub completed_texture_slots: usize,
    /// Total decoded texture slots.
    pub total_texture_slots: usize,
    /// Unique decoded texture bytes uploaded by this call.
    pub uploaded_texture_bytes: u64,
    /// Whether this call uploaded one texture larger than the byte target.
    pub uploaded_oversized_texture: bool,
    /// Mesh primitives already uploaded.
    pub completed_primitives: usize,
    /// Total mesh primitives in the source model.
    pub total_primitives: usize,
    /// Source geometry bytes represented by resident primitives.
    pub completed_geometry_bytes: u64,
    /// Total source geometry bytes in the model.
    pub total_geometry_bytes: u64,
    /// Whether this call had to upload one primitive larger than the byte target.
    pub uploaded_oversized_primitive: bool,
}

struct LitSceneGpuModel {
    meshes: Vec<Vec<LitSceneGpuPrimitive>>,
    textures: ModelTextureBindings,
    texture_slots: usize,
    material_bind_group_creations: u64,
    geometry_bytes: u64,
}

struct LitSceneGpuPrimitive {
    mesh: StandardMesh3d,
    material: StandardMaterial3d,
    textured_lit_material: Option<GpuTexturedLitMaterial>,
}

struct LitSceneGpuModelUpload {
    meshes: Vec<Vec<LitSceneGpuPrimitive>>,
    source_primitive_counts: Vec<usize>,
    textures: ModelTextureBindings,
    texture_slots: usize,
    textured_lit_materials: HashMap<ModelTextureIndex, GpuTexturedLitMaterial>,
    next_mesh: usize,
    next_primitive: usize,
    completed_primitives: usize,
    total_primitives: usize,
    completed_geometry_bytes: u64,
    total_geometry_bytes: u64,
}

impl LitSceneGpuModelUpload {
    fn new(source: &Model, textures: ModelTextureBindings) -> Self {
        let texture_slots = textures.len();
        let (total_primitives, total_geometry_bytes) = model_geometry_totals(source);
        Self {
            meshes: source
                .meshes()
                .iter()
                .map(|mesh| Vec::with_capacity(mesh.primitives().len()))
                .collect(),
            source_primitive_counts: source
                .meshes()
                .iter()
                .map(|mesh| mesh.primitives().len())
                .collect(),
            textures,
            texture_slots,
            textured_lit_materials: HashMap::new(),
            next_mesh: 0,
            next_primitive: 0,
            completed_primitives: 0,
            total_primitives,
            completed_geometry_bytes: 0,
            total_geometry_bytes,
        }
    }

    fn matches(&self, source: &Model) -> bool {
        source.meshes().len() == self.meshes.len()
            && source
                .meshes()
                .iter()
                .zip(&self.source_primitive_counts)
                .all(|(source, expected)| source.primitives().len() == *expected)
            && source
                .meshes()
                .iter()
                .flat_map(yuyib_model::Mesh::primitives)
                .map(primitive_source_geometry_bytes)
                .fold(0_u64, u64::saturating_add)
                == self.total_geometry_bytes
    }

    fn progress(&self, uploaded_oversized_primitive: bool) -> ModelUploadProgress3d {
        ModelUploadProgress3d {
            ready: self.completed_primitives == self.total_primitives,
            completed_texture_slots: self.texture_slots,
            total_texture_slots: self.texture_slots,
            uploaded_texture_bytes: 0,
            uploaded_oversized_texture: false,
            completed_primitives: self.completed_primitives,
            total_primitives: self.total_primitives,
            completed_geometry_bytes: self.completed_geometry_bytes,
            total_geometry_bytes: self.total_geometry_bytes,
            uploaded_oversized_primitive,
        }
    }

    fn finish(self) -> LitSceneGpuModel {
        LitSceneGpuModel {
            meshes: self.meshes,
            textures: self.textures,
            texture_slots: self.texture_slots,
            material_bind_group_creations: u64::try_from(self.textured_lit_materials.len())
                .expect("texture binding count fits u64"),
            geometry_bytes: self.total_geometry_bytes,
        }
    }
}

pub(crate) fn model_geometry_totals(model: &Model) -> (usize, u64) {
    let total_primitives = model
        .meshes()
        .iter()
        .map(|mesh| mesh.primitives().len())
        .sum();
    let total_geometry_bytes = model
        .meshes()
        .iter()
        .flat_map(yuyib_model::Mesh::primitives)
        .map(primitive_source_geometry_bytes)
        .fold(0_u64, u64::saturating_add);
    (total_primitives, total_geometry_bytes)
}

pub(crate) fn primitive_source_geometry_bytes(primitive: &MeshPrimitive) -> u64 {
    fn stream_bytes<T>(length: usize) -> u64 {
        u64::try_from(length)
            .unwrap_or(u64::MAX)
            .saturating_mul(u64::try_from(size_of::<T>()).expect("element size fits u64"))
    }

    stream_bytes::<[f32; 3]>(primitive.positions().len())
        .saturating_add(stream_bytes::<u32>(primitive.indices().len()))
        .saturating_add(
            primitive
                .normals()
                .map_or(0, |values| stream_bytes::<[f32; 3]>(values.len())),
        )
        .saturating_add(
            primitive
                .tangents()
                .map_or(0, |values| stream_bytes::<[f32; 4]>(values.len())),
        )
        .saturating_add(
            (0..MAX_TEX_COORD_SETS)
                .filter_map(|set| primitive.tex_coords(u8::try_from(set).ok()?))
                .map(|values| stream_bytes::<[f32; 2]>(values.len()))
                .fold(0_u64, u64::saturating_add),
        )
}

/// Returns whether the affine transform reverses triangle winding.
///
/// This parity selects a clockwise front-face variant while preserving the
/// material's culling policy. Singular/non-finite matrices are rejected by
/// draw-uniform validation before GPU submission.
pub(crate) fn model_matrix_reverses_winding(matrix: [f32; 16]) -> bool {
    let determinant = matrix[0] * matrix[5].mul_add(matrix[10], -(matrix[9] * matrix[6]))
        - matrix[4] * matrix[1].mul_add(matrix[10], -(matrix[9] * matrix[2]))
        + matrix[8] * matrix[1].mul_add(matrix[6], -(matrix[5] * matrix[2]));
    determinant.is_finite() && determinant < 0.0
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LambertRasterization {
    Regular,
    DoubleSided,
    Mirrored,
    MirroredDoubleSided,
}

fn lambert_rasterization(model_matrix: [f32; 16], double_sided: bool) -> LambertRasterization {
    match (model_matrix_reverses_winding(model_matrix), double_sided) {
        (false, false) => LambertRasterization::Regular,
        (false, true) => LambertRasterization::DoubleSided,
        (true, false) => LambertRasterization::Mirrored,
        (true, true) => LambertRasterization::MirroredDoubleSided,
    }
}

impl LitSceneRenderer3d {
    /// Создаёт renderer из устройства текущего кадра.
    #[must_use]
    pub fn new_for_frame(frame: &RenderFrame<'_>, texture_loader: ModelTextureLoader) -> Self {
        Self {
            standard: StandardRenderer3d::new_for_frame(frame),
            texture_loader,
            texture_assets: Assets::new(),
            texture_cache: TextureCache::new(),
            cached_models: HashMap::new(),
            prepared_models: HashMap::new(),
            preparing_models: HashMap::new(),
            unbound_material_policy: UnboundMaterialPolicy3d::Error,
        }
    }

    /// Selects how unbound primitives are handled on this Lambert scene path.
    #[must_use]
    pub const fn with_unbound_material_policy(mut self, policy: UnboundMaterialPolicy3d) -> Self {
        self.unbound_material_policy = policy;
        self
    }

    /// Возвращает число подготовленных моделей.
    #[must_use]
    pub fn cached_model_count(&self) -> usize {
        self.cached_models.len()
    }

    /// Queues worker-decoded textures for bounded publication of one model.
    pub fn queue_prepared_model(&mut self, model: ModelHandle, prepared: PreparedModelTextures) {
        if let Some(previous) = self.preparing_models.remove(&model) {
            previous
                .textures
                .release(&mut self.texture_assets, &mut self.texture_cache);
        }
        if let Some(previous) = self.cached_models.remove(&model) {
            previous
                .textures
                .release(&mut self.texture_assets, &mut self.texture_cache);
        }
        if let Some(previous) = self.prepared_models.insert(model, prepared) {
            previous.release(&mut self.texture_assets, &mut self.texture_cache);
        }
    }

    /// Returns slots still awaiting upload for a queued model.
    #[must_use]
    pub fn prepared_model_remaining(&self, model: ModelHandle) -> Option<usize> {
        self.prepared_models
            .get(&model)
            .map(PreparedModelTextures::remaining)
    }

    /// Returns the current publication progress for a queued or uploading model.
    ///
    /// # Errors
    ///
    /// Returns [`LitSceneRenderError::MissingModel`] when `model` is absent
    /// from the supplied typed asset storage.
    pub fn model_upload_progress(
        &self,
        models: &Assets<Model>,
        model: ModelHandle,
    ) -> Result<ModelUploadProgress3d, LitSceneRenderError> {
        if let Some(cached) = self.cached_models.get(&model) {
            let total_primitives = cached.meshes.iter().map(Vec::len).sum();
            return Ok(ModelUploadProgress3d {
                ready: true,
                completed_texture_slots: cached.texture_slots,
                total_texture_slots: cached.texture_slots,
                uploaded_texture_bytes: 0,
                uploaded_oversized_texture: false,
                completed_primitives: total_primitives,
                total_primitives,
                completed_geometry_bytes: cached.geometry_bytes,
                total_geometry_bytes: cached.geometry_bytes,
                uploaded_oversized_primitive: false,
            });
        }
        if let Some(upload) = self.preparing_models.get(&model) {
            return Ok(upload.progress(false));
        }
        let source = models
            .get(model)
            .ok_or(LitSceneRenderError::MissingModel { model })?;
        let (total_primitives, total_geometry_bytes) = model_geometry_totals(source);
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

    /// Uploads a bounded number of prepared texture slots and finalizes the
    /// model cache when all slots become resident.
    ///
    /// Returns `true` once [`Self::draw_for_frame`] can draw this model without
    /// any image decode work.
    ///
    /// # Errors
    ///
    /// Returns [`LitSceneRenderError`] for stale model data, upload failures or
    /// incompatible mesh/material data.
    pub fn prepare_model_for_frame(
        &mut self,
        frame: &RenderFrame<'_>,
        models: &Assets<Model>,
        model: ModelHandle,
        maximum_texture_slots: usize,
    ) -> Result<bool, LitSceneRenderError> {
        let progress = self.prepare_model_for_frame_with_budget(
            frame,
            models,
            model,
            ModelUploadBudget3d {
                maximum_texture_slots,
                target_texture_bytes: u64::MAX,
                maximum_primitives: usize::MAX,
                target_geometry_bytes: u64::MAX,
            },
        )?;
        Ok(progress.ready)
    }

    /// Publishes decoded textures and complete primitives within one frame budget.
    ///
    /// Publication is transactional: the model enters the drawable cache only
    /// after every primitive succeeds. On failure all texture ownership and
    /// partial GPU state created by this transaction are released.
    ///
    /// # Errors
    ///
    /// Returns [`LitSceneRenderError`] for stale model data, upload failures or
    /// incompatible mesh/material data.
    #[allow(
        clippy::too_many_lines,
        reason = "Texture-to-geometry state transition and rollback stay adjacent for review."
    )]
    pub fn prepare_model_for_frame_with_budget(
        &mut self,
        frame: &RenderFrame<'_>,
        models: &Assets<Model>,
        model: ModelHandle,
        budget: ModelUploadBudget3d,
    ) -> Result<ModelUploadProgress3d, LitSceneRenderError> {
        if self.cached_models.contains_key(&model) {
            return self.model_upload_progress(models, model);
        }
        let Some(source) = models.get(model) else {
            self.invalidate_model(model);
            return Err(LitSceneRenderError::MissingModel { model });
        };
        let mut texture_upload = PreparedTextureUploadStats::default();
        if !self.preparing_models.contains_key(&model) && !self.prepared_models.contains_key(&model)
        {
            if source.textures().is_empty() {
                self.preparing_models.insert(
                    model,
                    LitSceneGpuModelUpload::new(source, ModelTextureBindings::default()),
                );
            } else {
                return Err(LitSceneRenderError::ModelNotQueuedForPreparation { model });
            }
        }
        if !self.preparing_models.contains_key(&model) {
            let Some(prepared) = self.prepared_models.get_mut(&model) else {
                return Err(LitSceneRenderError::ModelNotQueuedForPreparation { model });
            };
            texture_upload = prepared
                .upload_with_budget_for_frame(
                    frame,
                    &mut self.texture_assets,
                    &mut self.texture_cache,
                    budget.maximum_texture_slots,
                    budget.target_texture_bytes,
                )
                .map_err(|source| LitSceneRenderError::TextureLoad { model, source })?;
            if prepared.remaining() != 0 {
                let mut progress = self.model_upload_progress(models, model)?;
                progress.uploaded_texture_bytes = texture_upload.uploaded_unique_bytes;
                progress.uploaded_oversized_texture = texture_upload.uploaded_oversized_texture;
                return Ok(progress);
            }
            let Some(prepared) = self.prepared_models.remove(&model) else {
                return self.model_upload_progress(models, model);
            };
            let textures = prepared
                .finish()
                .map_err(|source| LitSceneRenderError::PreparedIncomplete { model, source })?;
            self.preparing_models
                .insert(model, LitSceneGpuModelUpload::new(source, textures));
        }
        let Some(mut upload) = self.preparing_models.remove(&model) else {
            return self.model_upload_progress(models, model);
        };
        if !upload.matches(source) {
            upload
                .textures
                .release(&mut self.texture_assets, &mut self.texture_cache);
            return Err(LitSceneRenderError::ModelChangedDuringPreparation { model });
        }
        let result = self.upload_primitives_for_frame(frame, model, source, &mut upload, budget);
        let uploaded_oversized_primitive = match result {
            Ok(value) => value,
            Err(error) => {
                // Keep decoded GPU texture residency so the caller can retry
                // without re-queuing worker-prepared publication.
                self.preparing_models.insert(model, upload);
                return Err(error);
            }
        };
        if upload.completed_primitives == upload.total_primitives {
            let mut progress = upload.progress(uploaded_oversized_primitive);
            progress.uploaded_texture_bytes = texture_upload.uploaded_unique_bytes;
            progress.uploaded_oversized_texture = texture_upload.uploaded_oversized_texture;
            self.cached_models.insert(model, upload.finish());
            Ok(progress)
        } else {
            let mut progress = upload.progress(uploaded_oversized_primitive);
            progress.uploaded_texture_bytes = texture_upload.uploaded_unique_bytes;
            progress.uploaded_oversized_texture = texture_upload.uploaded_oversized_texture;
            self.preparing_models.insert(model, upload);
            Ok(progress)
        }
    }

    /// Удаляет одну модель из CPU/GPU-кэша; следующий кадр загрузит её заново.
    pub fn invalidate_model(&mut self, model: ModelHandle) -> bool {
        let prepared = self.prepared_models.remove(&model).map(|prepared| {
            prepared.release(&mut self.texture_assets, &mut self.texture_cache);
        });
        let preparing = self.preparing_models.remove(&model).map(|upload| {
            upload
                .textures
                .release(&mut self.texture_assets, &mut self.texture_cache);
        });
        let Some(cached) = self.cached_models.remove(&model) else {
            return prepared.is_some() || preparing.is_some();
        };
        cached
            .textures
            .release(&mut self.texture_assets, &mut self.texture_cache);
        true
    }

    /// Освобождает кэш всех моделей и их текстур.
    pub fn clear_model_cache(&mut self) {
        for (_, prepared) in self.prepared_models.drain() {
            prepared.release(&mut self.texture_assets, &mut self.texture_cache);
        }
        for (_, upload) in self.preparing_models.drain() {
            upload
                .textures
                .release(&mut self.texture_assets, &mut self.texture_cache);
        }
        for (_, cached) in self.cached_models.drain() {
            cached
                .textures
                .release(&mut self.texture_assets, &mut self.texture_cache);
        }
    }

    /// Рисует извлечённую ECS-сцену с одним Lambert-светом.
    ///
    /// `lighting` создаётся один раз через [`LambertLighting3d::new`] и может
    /// переиспользоваться между кадрами. При необходимости брать свет из ECS
    /// приложение явно выбирает нужный [`DirectionalLightDraw`] из снимка —
    /// скрытый выбор «первого» света здесь не делает сцену непредсказуемой.
    ///
    /// # Errors
    ///
    /// Возвращает [`LitSceneRenderError`] при отсутствующей модели, ошибке
    /// загрузки текстуры, неподдержанном material feature или GPU draw.
    #[allow(
        clippy::too_many_lines,
        reason = "The high-level route keeps cache, batch and exceptional fallback phases adjacent for review."
    )]
    pub fn draw_for_frame(
        &mut self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        lighting: LambertLighting3d,
        models: &Assets<Model>,
        scene: &ExtractedModels,
    ) -> Result<SceneDrawStats, LitSceneRenderError> {
        let mut cache_misses = 0;
        let mut material_bind_group_creations = 0;
        for batch in scene.batches() {
            if !self.cached_models.contains_key(&batch.model()) {
                if self.prepared_models.contains_key(&batch.model())
                    || self.preparing_models.contains_key(&batch.model())
                {
                    return Err(LitSceneRenderError::ModelPreparationInProgress {
                        model: batch.model(),
                    });
                }
                let uploaded = self.upload_model_for_frame(frame, models, batch.model())?;
                material_bind_group_creations += uploaded.material_bind_group_creations;
                self.cached_models.insert(batch.model(), uploaded);
                cache_misses += 1;
            }
        }
        let mut stats = SceneDrawStats {
            cache_misses,
            material_bind_group_creations,
            ..Default::default()
        };
        // The common map path is fully textured + lit.  Collect it first so
        // the GPU gets one depth render pass instead of one pass per imported
        // primitive. Opaque draw order is irrelevant under the shared depth
        // test; the unusual solid/untextured material variants are submitted
        // afterwards with `Load` and keep their existing StandardRenderer API.
        let mut textured_lit_draws = Vec::new();
        for batch in scene.batches() {
            let cached = self.cached_models.get(&batch.model()).ok_or(
                LitSceneRenderError::MissingModel {
                    model: batch.model(),
                },
            )?;
            for draw in batch.draws() {
                if let Some(mesh) = draw.mesh
                    && mesh >= cached.meshes.len()
                {
                    return Err(LitSceneRenderError::MissingMesh {
                        model: batch.model(),
                        mesh,
                    });
                }
                stats.model_instances += 1;
                for (mesh_index, primitives) in cached.meshes.iter().enumerate() {
                    if draw.mesh.is_some_and(|requested| requested != mesh_index) {
                        continue;
                    }
                    for primitive in primitives {
                        stats.primitive_draws += 1;
                        if let (Some(mesh), Some(material)) = (
                            primitive.mesh.textured_lit.as_ref(),
                            primitive.textured_lit_material.as_ref(),
                        ) {
                            textured_lit_draws.push(TexturedLitBatchDraw::new(
                                mesh,
                                LitMeshInstance3d::new(
                                    draw.model_matrix,
                                    LitMaterial3d::new(primitive.material.base_color_factor),
                                    lighting,
                                ),
                                material,
                                primitive.material.double_sided,
                            ));
                        }
                    }
                }
            }
        }
        let mut depth_started = false;
        for draws in textured_lit_draws.chunks(TEXTURED_LIT_BATCH_CAPACITY) {
            let draw_stats = self
                .standard
                .textured_lit
                .draw_batch_with_depth_load(
                    frame,
                    camera,
                    draws,
                    if depth_started {
                        DepthLoad::Load
                    } else {
                        DepthLoad::Clear
                    },
                )
                .map_err(|source| {
                    LitSceneRenderError::StandardRender(StandardRenderError::TexturedLit(source))
                })?;
            depth_started = true;
            stats.render_passes += 1;
            stats.triangles += u64::from(draw_stats.triangles);
            stats.draw_calls += u64::from(draw_stats.draw_calls);
        }
        // Only the exceptional material variants reach this loop.  Keeping it
        // explicit preserves StandardRenderer's exact fallback/error behavior
        // without charging the normal textured-map path an extra pass.
        for batch in scene.batches() {
            let cached = self.cached_models.get(&batch.model()).ok_or(
                LitSceneRenderError::MissingModel {
                    model: batch.model(),
                },
            )?;
            for draw in batch.draws() {
                for (mesh_index, primitives) in cached.meshes.iter().enumerate() {
                    if draw.mesh.is_some_and(|requested| requested != mesh_index) {
                        continue;
                    }
                    for primitive in primitives {
                        if primitive.textured_lit_material.is_some()
                            && primitive.mesh.textured_lit.is_some()
                        {
                            continue;
                        }
                        let draw_stats = self
                            .standard
                            .draw_with_depth_load(
                                frame,
                                camera,
                                &primitive.mesh,
                                StandardDraw3d::new(
                                    draw.model_matrix,
                                    primitive.material,
                                    Some(lighting),
                                ),
                                StandardTextureResources {
                                    bindings: &cached.textures,
                                    textures: &self.texture_cache,
                                },
                                if depth_started {
                                    DepthLoad::Load
                                } else {
                                    DepthLoad::Clear
                                },
                            )
                            .map_err(LitSceneRenderError::StandardRender)?;
                        depth_started = true;
                        stats.render_passes += 1;
                        stats.triangles += u64::from(draw_stats.triangles);
                        stats.draw_calls += u64::from(draw_stats.draw_calls);
                    }
                }
            }
        }
        if !depth_started {
            MeshRenderer3d::begin_depth_phase(frame);
            stats.render_passes = 1;
        }
        Ok(stats)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "Material validation, binding reuse and atomic primitive upload form one transaction."
    )]
    fn upload_primitives_for_frame(
        &mut self,
        frame: &RenderFrame<'_>,
        model: ModelHandle,
        source: &Model,
        upload: &mut LitSceneGpuModelUpload,
        budget: ModelUploadBudget3d,
    ) -> Result<bool, LitSceneRenderError> {
        if budget.maximum_primitives == 0 || budget.target_geometry_bytes == 0 {
            return Ok(false);
        }
        let mut uploaded_primitives = 0_usize;
        let mut uploaded_geometry_bytes = 0_u64;
        let mut uploaded_oversized_primitive = false;
        while upload.next_mesh < source.meshes().len() {
            let source_mesh = &source.meshes()[upload.next_mesh];
            if upload.next_primitive >= source_mesh.primitives().len() {
                upload.next_mesh += 1;
                upload.next_primitive = 0;
                continue;
            }
            if uploaded_primitives >= budget.maximum_primitives {
                break;
            }
            let primitive = &source_mesh.primitives()[upload.next_primitive];
            let primitive_bytes = primitive_source_geometry_bytes(primitive);
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
            let bound = primitive
                .material()
                .map(|index| {
                    source.materials().get(index.get()).ok_or(
                        LitSceneRenderError::MissingMaterial {
                            model,
                            mesh_index,
                            primitive_index,
                            material: index.get(),
                        },
                    )
                })
                .transpose()?;
            let material = self.unbound_material_policy.resolve(bound).map_err(|()| {
                LitSceneRenderError::UnboundMaterial {
                    model,
                    mesh_index,
                    primitive_index,
                }
            })?;
            let material = StandardMaterial3d::from_model_material_for_lambert(material).map_err(
                |source| LitSceneRenderError::Material {
                    model,
                    mesh_index,
                    primitive_index,
                    source,
                },
            )?;
            let textured_lit_material = if primitive.normals().is_some()
                && primitive.tex_coords_0().is_some()
            {
                material
                    .base_color_texture
                    .map(|texture_index| {
                        if let Some(cached) = upload.textured_lit_materials.get(&texture_index) {
                            return Ok(cached.clone());
                        }
                        let binding = upload.textures.get(texture_index).ok_or(
                            LitSceneRenderError::StandardRender(
                                StandardRenderError::MissingTextureBinding {
                                    index: texture_index,
                                },
                            ),
                        )?;
                        let texture = self.texture_cache.get(binding.handle()).ok_or(
                            LitSceneRenderError::StandardRender(
                                StandardRenderError::MissingGpuTexture {
                                    index: texture_index,
                                },
                            ),
                        )?;
                        let cached = self
                            .standard
                            .textured_lit
                            .upload_material_for_frame(frame, texture);
                        upload
                            .textured_lit_materials
                            .insert(texture_index, cached.clone());
                        Ok(cached)
                    })
                    .transpose()?
            } else {
                None
            };
            let mesh = self
                .standard
                .upload_mesh_for_frame(frame, primitive)
                .map_err(|source| LitSceneRenderError::MeshUpload {
                    model,
                    mesh_index,
                    primitive_index,
                    source,
                })?;
            upload.meshes[mesh_index].push(LitSceneGpuPrimitive {
                mesh,
                material,
                textured_lit_material,
            });
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
        reason = "All model residency decisions share one atomic upload-and-cleanup boundary."
    )]
    fn upload_model_for_frame(
        &mut self,
        frame: &RenderFrame<'_>,
        models: &Assets<Model>,
        model: ModelHandle,
    ) -> Result<LitSceneGpuModel, LitSceneRenderError> {
        let source = models
            .get(model)
            .ok_or(LitSceneRenderError::MissingModel { model })?;
        let textures = self
            .texture_loader
            .load_for_frame(
                frame,
                source,
                &mut self.texture_assets,
                &mut self.texture_cache,
            )
            .map_err(|source| LitSceneRenderError::TextureLoad { model, source })?;
        self.upload_model_with_bindings(frame, model, source, textures)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "All material and mesh binding decisions share one transactional cache boundary."
    )]
    fn upload_model_with_bindings(
        &mut self,
        frame: &RenderFrame<'_>,
        model: ModelHandle,
        source: &Model,
        textures: ModelTextureBindings,
    ) -> Result<LitSceneGpuModel, LitSceneRenderError> {
        let texture_slots = textures.len();
        let (_, geometry_bytes) = model_geometry_totals(source);
        let upload = {
            let mut textured_lit_materials =
                HashMap::<ModelTextureIndex, GpuTexturedLitMaterial>::new();
            let meshes = source
                .meshes()
                .iter()
                .enumerate()
                .map(|(mesh_index, mesh)| {
                    mesh.primitives()
                        .iter()
                        .enumerate()
                        .map(|(primitive_index, primitive)| {
                            let bound = primitive
                                .material()
                                .map(|index| {
                                    source.materials().get(index.get()).ok_or(
                                        LitSceneRenderError::MissingMaterial {
                                            model,
                                            mesh_index,
                                            primitive_index,
                                            material: index.get(),
                                        },
                                    )
                                })
                                .transpose()?;
                            let material =
                                self.unbound_material_policy.resolve(bound).map_err(|()| {
                                    LitSceneRenderError::UnboundMaterial {
                                        model,
                                        mesh_index,
                                        primitive_index,
                                    }
                                })?;
                            // Lambert uses only base colour, UV0 and culling. PBR-only channels
                            // have no effect on this deliberately projected pass; refusing an
                            // otherwise usable map for them would make the high-level route less
                            // useful than its stated contract. Alpha/spec-gloss semantics remain
                            // rejected by this conversion.
                            let material =
                                StandardMaterial3d::from_model_material_for_lambert(material)
                                    .map_err(|source| LitSceneRenderError::Material {
                                        model,
                                        mesh_index,
                                        primitive_index,
                                        source,
                                    })?;
                            let textured_lit_material = if primitive.normals().is_some()
                                && primitive.tex_coords_0().is_some()
                            {
                                material
                                    .base_color_texture
                                    .map(|texture_index| {
                                        if let Some(cached) =
                                            textured_lit_materials.get(&texture_index)
                                        {
                                            return Ok(cached.clone());
                                        }
                                        let binding = textures.get(texture_index).ok_or(
                                            LitSceneRenderError::StandardRender(
                                                StandardRenderError::MissingTextureBinding {
                                                    index: texture_index,
                                                },
                                            ),
                                        )?;
                                        let texture = self
                                            .texture_cache
                                            .get(binding.handle())
                                            .ok_or(LitSceneRenderError::StandardRender(
                                                StandardRenderError::MissingGpuTexture {
                                                    index: texture_index,
                                                },
                                            ))?;
                                        let cached = self
                                            .standard
                                            .textured_lit
                                            .upload_material_for_frame(frame, texture);
                                        textured_lit_materials
                                            .insert(texture_index, cached.clone());
                                        Ok(cached)
                                    })
                                    .transpose()?
                            } else {
                                None
                            };
                            let mesh = self
                                .standard
                                .upload_mesh_for_frame(frame, primitive)
                                .map_err(|source| LitSceneRenderError::MeshUpload {
                                    model,
                                    mesh_index,
                                    primitive_index,
                                    source,
                                })?;
                            Ok(LitSceneGpuPrimitive {
                                mesh,
                                material,
                                textured_lit_material,
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()
                })
                .collect::<Result<Vec<_>, _>>();
            meshes.map(|meshes| {
                (
                    meshes,
                    u64::try_from(textured_lit_materials.len())
                        .expect("texture binding count fits u64"),
                )
            })
        };
        match upload {
            Ok((meshes, material_bind_group_creations)) => Ok(LitSceneGpuModel {
                meshes,
                textures,
                texture_slots,
                material_bind_group_creations,
                geometry_bytes,
            }),
            Err(error) => {
                textures.release(&mut self.texture_assets, &mut self.texture_cache);
                Err(error)
            }
        }
    }
}

/// Ошибка подготовки либо отрисовки [`LitSceneRenderer3d`].
#[allow(
    missing_docs,
    reason = "Each variant identifies fields already explained by the enclosing error contract."
)]
#[derive(Debug)]
pub enum LitSceneRenderError {
    /// ECS-снимок ссылается на отсутствующую модель.
    MissingModel { model: ModelHandle },
    /// Снимок выбрал отсутствующий меш модели.
    MissingMesh { model: ModelHandle, mesh: usize },
    /// Примитив ссылается на отсутствующий материал.
    MissingMaterial {
        model: ModelHandle,
        mesh_index: usize,
        primitive_index: usize,
        material: usize,
    },
    /// Примитив без material binding при политике [`UnboundMaterialPolicy3d::Error`].
    UnboundMaterial {
        model: ModelHandle,
        mesh_index: usize,
        primitive_index: usize,
    },
    /// Не удалось декодировать или загрузить изображение модели.
    TextureLoad {
        model: ModelHandle,
        source: ModelTextureLoadError,
    },
    /// Не все подготовленные texture slots были опубликованы.
    PreparedIncomplete {
        model: ModelHandle,
        source: PreparedModelTexturesIncomplete,
    },
    /// Source mesh topology changed while a multi-frame upload was active.
    ModelChangedDuringPreparation { model: ModelHandle },
    /// Drawing was requested before a queued model became fully resident.
    ModelPreparationInProgress { model: ModelHandle },
    /// A textured model reached bounded publication without prepared images.
    ModelNotQueuedForPreparation { model: ModelHandle },
    /// Материал использует функцию, для которой нужен другой render path.
    Material {
        model: ModelHandle,
        mesh_index: usize,
        primitive_index: usize,
        source: StandardMaterialError,
    },
    /// Меш нельзя подготовить для выбранного render path.
    MeshUpload {
        model: ModelHandle,
        mesh_index: usize,
        primitive_index: usize,
        source: StandardMeshUploadError,
    },
    /// Низкоуровневый draw завершился ошибкой.
    StandardRender(StandardRenderError),
}

impl fmt::Display for LitSceneRenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingModel { model } => write!(formatter, "missing model asset: {model:?}"),
            Self::MissingMesh { model, mesh } => {
                write!(formatter, "model {model:?} has no mesh {mesh}")
            }
            Self::MissingMaterial {
                model,
                mesh_index,
                primitive_index,
                material,
            } => write!(
                formatter,
                "model {model:?}, mesh {mesh_index}, primitive {primitive_index} has no material {material}"
            ),
            Self::UnboundMaterial {
                model,
                mesh_index,
                primitive_index,
            } => write!(
                formatter,
                "model {model:?}, mesh {mesh_index}, primitive {primitive_index} has no material binding; repair with ModelMaterialPolicy::with_unbound_primitive_fallback or enable UnboundMaterialPolicy3d::DebugMagenta"
            ),
            Self::TextureLoad { model, source } => {
                write!(formatter, "cannot load textures for {model:?}: {source}")
            }
            Self::PreparedIncomplete { model, source } => {
                write!(
                    formatter,
                    "cannot finalize textures for {model:?}: {source}"
                )
            }
            Self::ModelChangedDuringPreparation { model } => write!(
                formatter,
                "model {model:?} changed while its GPU upload was in progress; queue it again"
            ),
            Self::ModelPreparationInProgress { model } => write!(
                formatter,
                "model {model:?} is still being published; wait for ready progress before drawing"
            ),
            Self::ModelNotQueuedForPreparation { model } => write!(
                formatter,
                "model {model:?} has textures but no worker-prepared publication; queue prepared textures first"
            ),
            Self::Material {
                model,
                mesh_index,
                primitive_index,
                source,
            } => write!(
                formatter,
                "model {model:?}, mesh {mesh_index}, primitive {primitive_index} has incompatible standard material: {source}"
            ),
            Self::MeshUpload {
                model,
                mesh_index,
                primitive_index,
                source,
            } => write!(
                formatter,
                "cannot upload model {model:?}, mesh {mesh_index}, primitive {primitive_index}: {source}"
            ),
            Self::StandardRender(source) => {
                write!(formatter, "cannot draw lit scene primitive: {source}")
            }
        }
    }
}
impl Error for LitSceneRenderError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::TextureLoad { source, .. } => Some(source),
            Self::PreparedIncomplete { source, .. } => Some(source),
            Self::Material { source, .. } => Some(source),
            Self::MeshUpload { source, .. } => Some(source),
            Self::StandardRender(source) => Some(source),
            Self::MissingModel { .. }
            | Self::MissingMesh { .. }
            | Self::MissingMaterial { .. }
            | Self::UnboundMaterial { .. }
            | Self::ModelChangedDuringPreparation { .. }
            | Self::ModelPreparationInProgress { .. }
            | Self::ModelNotQueuedForPreparation { .. } => None,
        }
    }
}

/// Высокоуровневый рендерер ECS-сцены с текстурами базового цвета.
///
/// Он предназначен для быстрого показа импортированной сцены: сам декодирует
/// изображения модели, хранит их на GPU и выбирает простой однотонный либо
/// текстурированный проход для каждой примитивной части меша. Это **не** PBR:
/// normal map, metallic/roughness, emissive, прозрачность и освещение здесь
/// намеренно не обрабатываются. Поэтому такой режим не выдаёт себя за более
/// точный материал, чем он есть.
///
/// Для собственного шейдера используйте низкоуровневые
/// [`MeshRenderer3d`] и [`TexturedMeshRenderer3d`].
///
/// # Жизненный цикл
///
/// Создайте рендерер один раз, передав ему [`ModelTextureLoader`], и храните
/// его между кадрами. При первом появлении [`ModelHandle`] он загружает все
/// изображения модели и её меши. После изменения модели вызовите
/// [`Self::invalidate_model`].
pub struct BaseColorSceneRenderer3d {
    solid: MeshRenderer3d,
    textured: TexturedMeshRenderer3d,
    texture_loader: ModelTextureLoader,
    texture_assets: Assets<Texture>,
    texture_cache: TextureCache,
    cached_models: HashMap<ModelHandle, BaseColorGpuModel>,
}

struct BaseColorGpuModel {
    meshes: Vec<Vec<BaseColorGpuPrimitive>>,
    textures: ModelTextureBindings,
}

/// One deferred transparent primitive. Indices point into immutable cached
/// GPU data, so sorting does not clone buffers or GPU resource handles.
#[derive(Clone, Copy)]
struct TransparentPrimitiveRequest {
    model: ModelHandle,
    mesh_index: usize,
    primitive_index: usize,
    model_matrix: [f32; 16],
    camera_distance_squared: f32,
}

enum BaseColorGpuPrimitive {
    Solid {
        mesh: GpuMesh,
        color: [f32; 4],
        double_sided: bool,
        transparent: bool,
        local_center: [f32; 3],
    },
    Textured {
        mesh: GpuTexturedMesh,
        texture: ModelTextureIndex,
        color: [f32; 4],
        double_sided: bool,
        transparent: bool,
        local_center: [f32; 3],
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct BaseColorMaterial {
    color: [f32; 4],
    texture: Option<ModelTextureIndex>,
    double_sided: bool,
    transparent: bool,
}

fn base_color_material(material: &Material) -> Result<BaseColorMaterial, u8> {
    let texture = match material.base_color_texture() {
        Some(binding) if binding.tex_coord_set() != 0 => return Err(binding.tex_coord_set()),
        Some(binding) => Some(binding.texture()),
        None => None,
    };
    Ok(BaseColorMaterial {
        color: material.base_color_factor(),
        texture,
        double_sided: material.double_sided(),
        transparent: material.alpha_mode() == AlphaMode::Blend,
    })
}

#[allow(clippy::cast_precision_loss)] // GPU-addressable mesh vertex counts are practical f32 ranges.
fn primitive_local_center(primitive: &MeshPrimitive) -> [f32; 3] {
    let count = primitive.positions().len() as f32;
    primitive
        .positions()
        .iter()
        .fold([0.0; 3], |mut sum, position| {
            sum[0] += position[0] / count;
            sum[1] += position[1] / count;
            sum[2] += position[2] / count;
            sum
        })
}

fn transform_point(matrix: [f32; 16], point: [f32; 3]) -> [f32; 3] {
    [
        matrix[0] * point[0] + matrix[4] * point[1] + matrix[8] * point[2] + matrix[12],
        matrix[1] * point[0] + matrix[5] * point[1] + matrix[9] * point[2] + matrix[13],
        matrix[2] * point[0] + matrix[6] * point[1] + matrix[10] * point[2] + matrix[14],
    ]
}

fn squared_distance(left: [f32; 3], right: [f32; 3]) -> f32 {
    let delta = sub3(left, right);
    dot3(delta, delta)
}

impl BaseColorSceneRenderer3d {
    /// Создаёт рендерер из устройства текущего кадра.
    ///
    /// `texture_loader` задаёт единственный разрешённый корень для внешних
    /// изображений. Встроенные изображения GLB также поддерживаются и не
    /// читаются с диска.
    #[must_use]
    pub fn new_for_frame(frame: &RenderFrame<'_>, texture_loader: ModelTextureLoader) -> Self {
        Self {
            solid: MeshRenderer3d::new_for_frame(frame),
            textured: TexturedMeshRenderer3d::new_for_frame(frame),
            texture_loader,
            texture_assets: Assets::new(),
            texture_cache: TextureCache::new(),
            cached_models: HashMap::new(),
        }
    }

    /// Возвращает число моделей, чьи меши и изображения уже подготовлены для GPU.
    #[must_use]
    pub fn cached_model_count(&self) -> usize {
        self.cached_models.len()
    }

    /// Удаляет меши и изображения одной модели из кэша.
    ///
    /// Возвращает `true`, если модель находилась в кэше. Следующий кадр
    /// загрузит её заново.
    pub fn invalidate_model(&mut self, model: ModelHandle) -> bool {
        let Some(cached) = self.cached_models.remove(&model) else {
            return false;
        };
        cached
            .textures
            .release(&mut self.texture_assets, &mut self.texture_cache);
        true
    }

    /// Освобождает кэш всех моделей, мешей и изображений.
    pub fn clear_model_cache(&mut self) {
        for (_, cached) in self.cached_models.drain() {
            cached
                .textures
                .release(&mut self.texture_assets, &mut self.texture_cache);
        }
    }

    /// Рисует ECS-снимок, сохраняя его порядок и общую фазу depth-buffer.
    ///
    /// При первом использовании модели загружаются её изображения и меши.
    /// Текстурированная часть требует UV0; при его отсутствии возвращается
    /// структурированная ошибка вместо белого «запасного» изображения.
    ///
    /// # Errors
    ///
    /// Возвращает ошибку импорта изображения, подготовки GPU, материала либо
    /// некорректных данных камеры/сцены.
    #[allow(clippy::too_many_lines)] // Opaque and sorted transparent phase are one transaction.
    pub fn draw_for_frame(
        &mut self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        models: &Assets<Model>,
        scene: &ExtractedModels,
    ) -> Result<SceneDrawStats, BaseColorSceneRenderError> {
        let mut cache_misses = 0;
        for batch in scene.batches() {
            if !self.cached_models.contains_key(&batch.model()) {
                let uploaded = self.upload_model_for_frame(frame, models, batch.model())?;
                self.cached_models.insert(batch.model(), uploaded);
                cache_misses += 1;
            }
        }

        let mut stats = SceneDrawStats {
            cache_misses,
            ..Default::default()
        };
        let mut transparent = Vec::new();
        MeshRenderer3d::begin_depth_phase(frame);
        for batch in scene.batches() {
            let cached = self.cached_models.get(&batch.model()).ok_or(
                BaseColorSceneRenderError::MissingModel {
                    model: batch.model(),
                },
            )?;
            for draw in batch.draws() {
                if let Some(mesh) = draw.mesh
                    && mesh >= cached.meshes.len()
                {
                    return Err(BaseColorSceneRenderError::MissingMesh {
                        model: batch.model(),
                        mesh,
                    });
                }
                stats.model_instances += 1;
                for (mesh_index, primitives) in cached.meshes.iter().enumerate() {
                    if draw.mesh.is_some_and(|requested| requested != mesh_index) {
                        continue;
                    }
                    for (primitive_index, primitive) in primitives.iter().enumerate() {
                        let (is_transparent, local_center) = match primitive {
                            BaseColorGpuPrimitive::Solid {
                                transparent,
                                local_center,
                                ..
                            }
                            | BaseColorGpuPrimitive::Textured {
                                transparent,
                                local_center,
                                ..
                            } => (*transparent, *local_center),
                        };
                        if is_transparent {
                            let center = transform_point(draw.model_matrix, local_center);
                            transparent.push(TransparentPrimitiveRequest {
                                model: batch.model(),
                                mesh_index,
                                primitive_index,
                                model_matrix: draw.model_matrix,
                                camera_distance_squared: squared_distance(camera.position, center),
                            });
                            continue;
                        }
                        let draw_stats = match primitive {
                            BaseColorGpuPrimitive::Solid {
                                mesh,
                                color,
                                double_sided,
                                ..
                            } => self
                                .solid
                                .draw_with_model_matrix_depth_load_rasterization(
                                    frame,
                                    camera,
                                    mesh,
                                    draw.model_matrix,
                                    *color,
                                    DepthLoad::Load,
                                    *double_sided,
                                )
                                .map_err(BaseColorSceneRenderError::SolidRender)?,
                            BaseColorGpuPrimitive::Textured {
                                mesh,
                                texture,
                                color,
                                double_sided,
                                ..
                            } => {
                                let binding = cached.textures.get(*texture).ok_or(
                                    BaseColorSceneRenderError::MissingTextureBinding {
                                        model: batch.model(),
                                        texture: *texture,
                                    },
                                )?;
                                let gpu_texture = self.texture_cache.get(binding.handle()).ok_or(
                                    BaseColorSceneRenderError::MissingGpuTexture {
                                        model: batch.model(),
                                        texture: *texture,
                                    },
                                )?;
                                self.textured
                                    .draw_with_depth_load_rasterization(
                                        frame,
                                        camera,
                                        mesh,
                                        draw.model_matrix,
                                        TexturedMaterial3d::new(gpu_texture, *color),
                                        DepthLoad::Load,
                                        *double_sided,
                                    )
                                    .map_err(BaseColorSceneRenderError::TexturedRender)?
                            }
                        };
                        stats.primitive_draws += 1;
                        stats.triangles += u64::from(draw_stats.triangles);
                        stats.draw_calls += u64::from(draw_stats.draw_calls);
                    }
                }
            }
        }
        transparent.sort_by(|left, right| {
            right
                .camera_distance_squared
                .total_cmp(&left.camera_distance_squared)
        });
        for request in transparent {
            let cached = self.cached_models.get(&request.model).ok_or(
                BaseColorSceneRenderError::MissingModel {
                    model: request.model,
                },
            )?;
            let primitive = &cached.meshes[request.mesh_index][request.primitive_index];
            let draw_stats = match primitive {
                BaseColorGpuPrimitive::Solid {
                    mesh,
                    color,
                    double_sided,
                    ..
                } => self
                    .solid
                    .draw_transparent_with_model_matrix_rasterization(
                        frame,
                        camera,
                        mesh,
                        request.model_matrix,
                        *color,
                        *double_sided,
                    )
                    .map_err(BaseColorSceneRenderError::SolidRender)?,
                BaseColorGpuPrimitive::Textured {
                    mesh,
                    texture,
                    color,
                    double_sided,
                    ..
                } => {
                    let binding = cached.textures.get(*texture).ok_or(
                        BaseColorSceneRenderError::MissingTextureBinding {
                            model: request.model,
                            texture: *texture,
                        },
                    )?;
                    let gpu_texture = self.texture_cache.get(binding.handle()).ok_or(
                        BaseColorSceneRenderError::MissingGpuTexture {
                            model: request.model,
                            texture: *texture,
                        },
                    )?;
                    self.textured
                        .draw_with_depth_load_rasterization_phase(
                            frame,
                            camera,
                            mesh,
                            request.model_matrix,
                            TexturedMaterial3d::new(gpu_texture, *color),
                            DepthLoad::Load,
                            *double_sided,
                            true,
                        )
                        .map_err(BaseColorSceneRenderError::TexturedRender)?
                }
            };
            stats.primitive_draws += 1;
            stats.triangles += u64::from(draw_stats.triangles);
            stats.draw_calls += u64::from(draw_stats.draw_calls);
        }
        Ok(stats)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "Texture residency, material selection and transactional cleanup form one GPU upload boundary."
    )]
    fn upload_model_for_frame(
        &mut self,
        frame: &RenderFrame<'_>,
        models: &Assets<Model>,
        model: ModelHandle,
    ) -> Result<BaseColorGpuModel, BaseColorSceneRenderError> {
        let source = models
            .get(model)
            .ok_or(BaseColorSceneRenderError::MissingModel { model })?;
        let textures = self
            .texture_loader
            .load_for_frame(
                frame,
                source,
                &mut self.texture_assets,
                &mut self.texture_cache,
            )
            .map_err(|source| BaseColorSceneRenderError::TextureLoad { model, source })?;
        let upload = (|| {
            let mut meshes = Vec::with_capacity(source.meshes().len());
            for (mesh_index, mesh) in source.meshes().iter().enumerate() {
                let mut primitives = Vec::with_capacity(mesh.primitives().len());
                for (primitive_index, primitive) in mesh.primitives().iter().enumerate() {
                    let material = if let Some(index) = primitive.material() {
                        source.materials().get(index.get()).ok_or(
                            BaseColorSceneRenderError::MissingMaterial {
                                model,
                                mesh_index,
                                primitive_index,
                                material: index.get(),
                            },
                        )?
                    } else {
                        let gpu_mesh = self.solid.upload_mesh_for_frame(frame, primitive).map_err(
                            |source| BaseColorSceneRenderError::SolidUpload {
                                model,
                                mesh_index,
                                primitive_index,
                                source,
                            },
                        )?;
                        primitives.push(BaseColorGpuPrimitive::Solid {
                            mesh: gpu_mesh,
                            color: [1.0; 4],
                            double_sided: false,
                            transparent: false,
                            local_center: primitive_local_center(primitive),
                        });
                        continue;
                    };
                    if let AlphaMode::Mask { cutoff } = material.alpha_mode() {
                        return Err(BaseColorSceneRenderError::AlphaMaskUnsupported {
                            model,
                            mesh_index,
                            primitive_index,
                            cutoff,
                        });
                    }
                    let material = match base_color_material(material) {
                        Ok(material) => material,
                        Err(actual) => {
                            return Err(BaseColorSceneRenderError::TextureCoordinatesUnsupported {
                                model,
                                mesh_index,
                                primitive_index,
                                actual,
                            });
                        }
                    };
                    if let Some(texture) = material.texture {
                        let gpu_mesh = match self.textured.upload_mesh_for_frame(frame, primitive) {
                            Ok(mesh) => mesh,
                            Err(source) => {
                                return Err(BaseColorSceneRenderError::TexturedUpload {
                                    model,
                                    mesh_index,
                                    primitive_index,
                                    source,
                                });
                            }
                        };
                        primitives.push(BaseColorGpuPrimitive::Textured {
                            mesh: gpu_mesh,
                            texture,
                            color: material.color,
                            double_sided: material.double_sided,
                            transparent: material.transparent,
                            local_center: primitive_local_center(primitive),
                        });
                    } else {
                        let gpu_mesh = match self.solid.upload_mesh_for_frame(frame, primitive) {
                            Ok(mesh) => mesh,
                            Err(source) => {
                                return Err(BaseColorSceneRenderError::SolidUpload {
                                    model,
                                    mesh_index,
                                    primitive_index,
                                    source,
                                });
                            }
                        };
                        primitives.push(BaseColorGpuPrimitive::Solid {
                            mesh: gpu_mesh,
                            color: material.color,
                            double_sided: material.double_sided,
                            transparent: material.transparent,
                            local_center: primitive_local_center(primitive),
                        });
                    }
                }
                meshes.push(primitives);
            }
            Ok(meshes)
        })();
        match upload {
            Ok(meshes) => Ok(BaseColorGpuModel { meshes, textures }),
            Err(error) => {
                textures.release(&mut self.texture_assets, &mut self.texture_cache);
                Err(error)
            }
        }
    }
}

/// Ошибка подготовки либо отрисовки [`BaseColorSceneRenderer3d`].
#[allow(
    missing_docs,
    reason = "Each error variant documents its field meanings without repeating identifiers."
)]
#[derive(Debug)]
pub enum BaseColorSceneRenderError {
    /// ECS-снимок ссылается на отсутствующую модель.
    MissingModel { model: ModelHandle },
    /// Узел сцены выбрал отсутствующий меш модели.
    MissingMesh { model: ModelHandle, mesh: usize },
    /// Примитив ссылается на отсутствующий материал.
    MissingMaterial {
        model: ModelHandle,
        mesh_index: usize,
        primitive_index: usize,
        material: usize,
    },
    /// Не удалось декодировать или загрузить изображения модели.
    TextureLoad {
        model: ModelHandle,
        source: ModelTextureLoadError,
    },
    /// Текстура базового цвета выбрала неподдерживаемый набор UV.
    TextureCoordinatesUnsupported {
        model: ModelHandle,
        mesh_index: usize,
        primitive_index: usize,
        actual: u8,
    },
    /// Для alpha-mask нужен отдельный проход с отбрасыванием фрагментов.
    AlphaMaskUnsupported {
        model: ModelHandle,
        mesh_index: usize,
        primitive_index: usize,
        cutoff: f32,
    },
    /// Однотонный меш нельзя загрузить в GPU.
    SolidUpload {
        model: ModelHandle,
        mesh_index: usize,
        primitive_index: usize,
        source: MeshUploadError,
    },
    /// Текстурированный меш нельзя загрузить в GPU.
    TexturedUpload {
        model: ModelHandle,
        mesh_index: usize,
        primitive_index: usize,
        source: TexturedMeshUploadError,
    },
    /// В кэше нет соответствия слота текстуры модели.
    MissingTextureBinding {
        model: ModelHandle,
        texture: ModelTextureIndex,
    },
    /// Текстура модели исчезла из GPU-кэша.
    MissingGpuTexture {
        model: ModelHandle,
        texture: ModelTextureIndex,
    },
    /// Однотонный draw получил некорректные данные.
    SolidRender(MeshRenderError),
    /// Текстурированный draw получил некорректные данные.
    TexturedRender(TexturedMeshRenderError),
}

impl fmt::Display for BaseColorSceneRenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingModel { model } => write!(formatter, "missing model asset: {model:?}"),
            Self::MissingMesh { model, mesh } => {
                write!(formatter, "model {model:?} has no mesh {mesh}")
            }
            Self::MissingMaterial {
                model,
                mesh_index,
                primitive_index,
                material,
            } => write!(
                formatter,
                "model {model:?}, mesh {mesh_index}, primitive {primitive_index} has no material {material}"
            ),
            Self::TextureLoad { model, source } => {
                write!(formatter, "cannot load textures for {model:?}: {source}")
            }
            Self::TextureCoordinatesUnsupported {
                model,
                mesh_index,
                primitive_index,
                actual,
            } => write!(
                formatter,
                "model {model:?}, mesh {mesh_index}, primitive {primitive_index} uses unsupported UV{actual}"
            ),
            Self::AlphaMaskUnsupported {
                model,
                mesh_index,
                primitive_index,
                cutoff,
            } => write!(
                formatter,
                "model {model:?}, mesh {mesh_index}, primitive {primitive_index} needs alpha-mask cutoff {cutoff}"
            ),
            Self::SolidUpload {
                model,
                mesh_index,
                primitive_index,
                source,
            } => write!(
                formatter,
                "cannot upload solid model {model:?}, mesh {mesh_index}, primitive {primitive_index}: {source}"
            ),
            Self::TexturedUpload {
                model,
                mesh_index,
                primitive_index,
                source,
            } => write!(
                formatter,
                "cannot upload textured model {model:?}, mesh {mesh_index}, primitive {primitive_index}: {source}"
            ),
            Self::MissingTextureBinding { model, texture } => write!(
                formatter,
                "model {model:?} has no resolved texture slot {}",
                texture.get()
            ),
            Self::MissingGpuTexture { model, texture } => write!(
                formatter,
                "model {model:?} texture slot {} is not resident on the GPU",
                texture.get()
            ),
            Self::SolidRender(source) => {
                write!(formatter, "cannot draw solid scene primitive: {source}")
            }
            Self::TexturedRender(source) => {
                write!(formatter, "cannot draw textured scene primitive: {source}")
            }
        }
    }
}

impl Error for BaseColorSceneRenderError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::TextureLoad { source, .. } => Some(source),
            Self::SolidUpload { source, .. } => Some(source),
            Self::TexturedUpload { source, .. } => Some(source),
            Self::SolidRender(source) => Some(source),
            Self::TexturedRender(source) => Some(source),
            Self::MissingModel { .. }
            | Self::MissingMesh { .. }
            | Self::MissingMaterial { .. }
            | Self::TextureCoordinatesUnsupported { .. }
            | Self::AlphaMaskUnsupported { .. }
            | Self::MissingTextureBinding { .. }
            | Self::MissingGpuTexture { .. } => None,
        }
    }
}

/// Failure while converting CPU mesh data into unlit GPU buffers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MeshUploadError {
    /// A position contained NaN or infinity.
    NonFinitePosition {
        /// Position stream index.
        index: usize,
    },
    /// The position count exceeds WGPU's `u32` vertex address range.
    TooManyVertices {
        /// Observed count.
        actual: usize,
    },
    /// The index count exceeds WGPU's `u32` draw range.
    TooManyIndices {
        /// Observed count.
        actual: usize,
    },
}

impl fmt::Display for MeshUploadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFinitePosition { index } => {
                write!(formatter, "mesh position at index {index} is not finite")
            }
            Self::TooManyVertices { actual } => {
                write!(
                    formatter,
                    "mesh has {actual} vertices; WGPU accepts at most u32::MAX"
                )
            }
            Self::TooManyIndices { actual } => {
                write!(
                    formatter,
                    "mesh has {actual} indices; WGPU accepts at most u32::MAX"
                )
            }
        }
    }
}

impl Error for MeshUploadError {}

/// Failure while creating camera or per-draw GPU uniform data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MeshRenderError {
    /// Camera data cannot make a finite non-degenerate view-projection matrix.
    InvalidCamera(&'static str),
    /// Transform data cannot make a finite non-degenerate model matrix.
    InvalidTransform(&'static str),
    /// A caller-provided column-major model matrix contains NaN or infinity.
    InvalidModelMatrix,
    /// The unlit RGBA multiplier contains NaN or infinity.
    InvalidInstanceColor,
    /// A batch draw requested more instances than the uniform buffer holds.
    BatchTooLarge {
        /// Number of draws requested by the caller.
        requested: usize,
        /// Maximum instances the renderer uploaded capacity for.
        capacity: usize,
    },
}

impl fmt::Display for MeshRenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCamera(message) => write!(formatter, "invalid 3D camera: {message}"),
            Self::InvalidTransform(message) => write!(formatter, "invalid 3D transform: {message}"),
            Self::InvalidModelMatrix => {
                formatter.write_str("model matrix must contain only finite values")
            }
            Self::InvalidInstanceColor => formatter.write_str("mesh instance color must be finite"),
            Self::BatchTooLarge {
                requested,
                capacity,
            } => write!(
                formatter,
                "unlit batch has {requested} draws but capacity is {capacity}"
            ),
        }
    }
}

impl Error for MeshRenderError {}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct PositionVertex {
    position: [f32; 3],
}

const POSITION_VERTEX_LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
    array_stride: size_of::<PositionVertex>() as u64,
    step_mode: wgpu::VertexStepMode::Vertex,
    attributes: &[wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32x3,
        offset: 0,
        shader_location: 0,
    }],
};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SkinnedVertex {
    position: [f32; 3],
    joints: [u16; 4],
    weights: [f32; 4],
}

const SKINNED_VERTEX_LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
    array_stride: size_of::<SkinnedVertex>() as u64,
    step_mode: wgpu::VertexStepMode::Vertex,
    attributes: &[
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x3,
            offset: 0,
            shader_location: 0,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Uint16x4,
            offset: size_of::<[f32; 3]>() as u64,
            shader_location: 1,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x4,
            offset: (size_of::<[f32; 3]>() + size_of::<[u16; 4]>()) as u64,
            shader_location: 2,
        },
    ],
};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct TexturedSkinnedVertex {
    position: [f32; 3],
    normal: [f32; 3],
    tex_coord: [f32; 2],
    joints: [u16; 4],
    weights: [f32; 4],
}

const TEXTURED_SKINNED_VERTEX_LAYOUT: wgpu::VertexBufferLayout<'static> =
    wgpu::VertexBufferLayout {
        array_stride: size_of::<TexturedSkinnedVertex>() as u64,
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
                offset: (2 * size_of::<[f32; 3]>()) as u64,
                shader_location: 2,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Uint16x4,
                offset: (2 * size_of::<[f32; 3]>() + size_of::<[f32; 2]>()) as u64,
                shader_location: 3,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: (2 * size_of::<[f32; 3]>()
                    + size_of::<[f32; 2]>()
                    + size_of::<[u16; 4]>()) as u64,
                shader_location: 4,
            },
        ],
    };

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct MeshUniform {
    model: [f32; 16],
    color: [f32; 4],
}

impl MeshUniform {
    fn from_matrix(model: [f32; 16], color: [f32; 4]) -> Result<Self, MeshRenderError> {
        if !all_finite(&model) {
            return Err(MeshRenderError::InvalidModelMatrix);
        }
        if !all_finite(&color) {
            return Err(MeshRenderError::InvalidInstanceColor);
        }
        Ok(Self { model, color })
    }
}

/// Uniform layout used only by the textured skin pipeline.
///
/// Keeping the mask cutoff here rather than in a global renderer setting lets
/// one character draw opaque, masked and blended primitives in one frame.
///
/// Layout must match WGSL uniform alignment: `vec3` after a scalar is aligned
/// to 16 bytes, and the whole struct rounds up to 16 → 144 bytes.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct TexturedSkinnedUniform {
    model: [f32; 16],
    color: [f32; 4],
    alpha_cutoff: f32,
    _pad_after_cutoff: [f32; 3],
    ambient: [f32; 3],
    _pad_after_ambient: f32,
    /// Direction the light travels (same convention as [`LambertLighting3d`]).
    light_direction: [f32; 3],
    _pad_after_light_dir: f32,
    /// `color × intensity` radiance for the Lambert term.
    light_radiance: [f32; 3],
    _pad_end: f32,
}

const _: () = assert!(size_of::<TexturedSkinnedUniform>() == 144);

impl TexturedSkinnedUniform {
    fn from_parts(
        model: [f32; 16],
        color: [f32; 4],
        alpha_cutoff: f32,
        light_direction: [f32; 3],
        light_radiance: [f32; 3],
        ambient: [f32; 3],
    ) -> Result<Self, MeshRenderError> {
        if !all_finite(&model) {
            return Err(MeshRenderError::InvalidModelMatrix);
        }
        if !all_finite(&color) {
            return Err(MeshRenderError::InvalidInstanceColor);
        }
        if !all_finite(&ambient)
            || ambient.iter().any(|channel| *channel < 0.0)
            || !all_finite(&light_direction)
            || !all_finite(&light_radiance)
            || light_radiance.iter().any(|channel| *channel < 0.0)
        {
            return Err(MeshRenderError::InvalidInstanceColor);
        }
        Ok(Self {
            model,
            color,
            alpha_cutoff,
            _pad_after_cutoff: [0.0; 3],
            ambient,
            _pad_after_ambient: 0.0,
            light_direction,
            _pad_after_light_dir: 0.0,
            light_radiance,
            _pad_end: 0.0,
        })
    }
}

const UNLIT_MESH_WGSL: &str = r"
struct Camera {
    view_projection: mat4x4<f32>,
};

struct Instance {
    model: mat4x4<f32>,
    color: vec4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var<uniform> instance: Instance;

struct VertexInput {
    @location(0) position: vec3<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    output.clip_position = camera.view_projection * instance.model * vec4<f32>(input.position, 1.0);
    return output;
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return instance.color;
}
";

const SKINNED_MESH_WGSL: &str = r"
struct Camera {
    view_projection: mat4x4<f32>,
};

struct Instance {
    model: mat4x4<f32>,
    color: vec4<f32>,
};

struct SkinPalette {
    matrices: array<mat4x4<f32>, 512>,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var<uniform> instance: Instance;
@group(2) @binding(0) var<storage, read> skin: SkinPalette;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) joints: vec4<u32>,
    @location(2) weights: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    let local_position = vec4<f32>(input.position, 1.0);
    let skinned_position =
        input.weights.x * (skin.matrices[input.joints.x] * local_position) +
        input.weights.y * (skin.matrices[input.joints.y] * local_position) +
        input.weights.z * (skin.matrices[input.joints.z] * local_position) +
        input.weights.w * (skin.matrices[input.joints.w] * local_position);
    var output: VertexOutput;
    output.clip_position = camera.view_projection * instance.model * skinned_position;
    return output;
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return instance.color;
}
";

const TEXTURED_SKINNED_MESH_WGSL: &str = r"
struct Camera { view_projection: mat4x4<f32>, };
struct Instance {
    model: mat4x4<f32>,
    color: vec4<f32>,
    alpha_cutoff: f32,
    // WGSL aligns the following vec3 to 16 bytes (12 bytes implicit pad).
    ambient: vec3<f32>,
    light_direction: vec3<f32>,
    light_radiance: vec3<f32>,
};
struct SkinPalette { matrices: array<mat4x4<f32>, 512>, };
@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var<uniform> instance: Instance;
@group(2) @binding(0) var<storage, read> skin: SkinPalette;
@group(3) @binding(0) var base_color_texture: texture_2d<f32>;
@group(3) @binding(1) var base_color_sampler: sampler;
struct VertexInput {
    @location(0) position: vec3<f32>, @location(1) normal: vec3<f32>,
    @location(2) tex_coord: vec2<f32>,
    @location(3) joints: vec4<u32>, @location(4) weights: vec4<f32>,
};
struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) tex_coord: vec2<f32>,
    @location(1) world_normal: vec3<f32>,
};
fn skin_matrix(joints: vec4<u32>, weights: vec4<f32>) -> mat4x4<f32> {
    return weights.x * skin.matrices[joints.x]
        + weights.y * skin.matrices[joints.y]
        + weights.z * skin.matrices[joints.z]
        + weights.w * skin.matrices[joints.w];
}
@vertex fn vs_main(input: VertexInput) -> VertexOutput {
    let skin_m = skin_matrix(input.joints, input.weights);
    let skinned_position = skin_m * vec4<f32>(input.position, 1.0);
    let skinned_normal = normalize((skin_m * vec4<f32>(input.normal, 0.0)).xyz);
    var output: VertexOutput;
    output.clip_position = camera.view_projection * instance.model * skinned_position;
    output.tex_coord = input.tex_coord;
    // Uniform playermodel scale: mat3(model) is enough; normalize afterwards.
    output.world_normal = normalize((instance.model * vec4<f32>(skinned_normal, 0.0)).xyz);
    return output;
}
@fragment fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let base = textureSample(base_color_texture, base_color_sampler, input.tex_coord) * instance.color;
    if instance.alpha_cutoff >= 0.0 && base.a < instance.alpha_cutoff { discard; }
    // View-independent exposure only. Directional N·L is deferred until skinned
    // PBR shares the world probe — orbiting a fixed pose must not change
    // whole-avatar brightness. World normals stay in the vertex stage for that
    // future path; light_direction is reserved in the uniform layout.
    let light = instance.ambient + instance.light_radiance;
    return vec4<f32>(base.rgb * light, base.a);
}
";

/// A mesh with positions, indices and mandatory primary UV coordinates on the GPU.
///
/// This resource is intentionally distinct from [`GpuMesh`]. The solid-colour
/// path need not pay for UV bandwidth, while this path makes the texture input
/// requirement explicit at upload time.
pub struct GpuTexturedMesh {
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
}

impl GpuTexturedMesh {
    /// Returns the uploaded triangle-list index count.
    #[must_use]
    pub const fn index_count(&self) -> u32 {
        self.index_count
    }
}

/// Borrowed base-colour input for one textured unlit draw.
///
/// [`Self::unbound`] exists for asset-resolution code which has not acquired a
/// GPU texture yet. Drawing with it returns [`TexturedMeshRenderError::MissingTexture`]
/// rather than silently falling back to white.
#[derive(Clone, Copy)]
pub struct TexturedMaterial3d<'texture> {
    texture: Option<&'texture GpuTexture>,
    base_color_factor: [f32; 4],
}

impl<'texture> TexturedMaterial3d<'texture> {
    /// Creates a material that samples `texture` and multiplies it by a linear RGBA factor.
    #[must_use]
    pub const fn new(texture: &'texture GpuTexture, base_color_factor: [f32; 4]) -> Self {
        Self {
            texture: Some(texture),
            base_color_factor,
        }
    }

    /// Creates an unresolved material placeholder.
    ///
    /// This is useful while a streaming system resolves texture assets. It is
    /// not drawable and returns an explicit missing-texture error.
    #[must_use]
    pub const fn unbound(base_color_factor: [f32; 4]) -> Self {
        Self {
            texture: None,
            base_color_factor,
        }
    }

    /// Returns the texture, if asset resolution has completed.
    #[must_use]
    pub const fn texture(self) -> Option<&'texture GpuTexture> {
        self.texture
    }

    /// Returns the linear RGBA multiplier.
    #[must_use]
    pub const fn base_color_factor(self) -> [f32; 4] {
        self.base_color_factor
    }
}

/// GPU renderer for opaque, textured unlit meshes.
///
/// It consumes [`GpuTexture`] so image decoding/upload can be shared with the
/// 2D renderer. This is deliberately **not** PBR: normals, lighting,
/// alpha blending, mip generation and texture transforms are outside this
/// phase. The material writes depth with the same `Less` policy as
/// [`MeshRenderer3d`].
pub struct TexturedMeshRenderer3d {
    pipeline: wgpu::RenderPipeline,
    double_sided_pipeline: wgpu::RenderPipeline,
    transparent_pipeline: wgpu::RenderPipeline,
    transparent_double_sided_pipeline: wgpu::RenderPipeline,
    camera_buffer: wgpu::Buffer,
    instance_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    instance_bind_group: wgpu::BindGroup,
    texture_layout: wgpu::BindGroupLayout,
}

impl TexturedMeshRenderer3d {
    /// Creates a textured unlit renderer for `renderer`'s presentation format.
    #[must_use]
    pub fn new(renderer: &Renderer) -> Self {
        let depth_format = renderer.depth_format();
        let color_format = renderer.color_target_format();
        renderer.with_raw_gpu(|device, _queue, _configuration| {
            Self::create(device, color_format, depth_format)
        })
    }

    /// Creates a textured renderer from a currently-recording frame.
    #[must_use]
    pub fn new_for_frame(frame: &RenderFrame<'_>) -> Self {
        Self::create(frame.device(), frame.surface_format(), frame.depth_format())
    }

    /// Uploads one primitive that has primary UV coordinates.
    ///
    /// # Errors
    ///
    /// Returns [`TexturedMeshUploadError::MissingTexCoords0`] when the source
    /// primitive has no UV0 stream. Texture coordinates are never invented.
    pub fn upload_mesh(
        &self,
        renderer: &Renderer,
        primitive: &MeshPrimitive,
    ) -> Result<GpuTexturedMesh, TexturedMeshUploadError> {
        renderer.with_raw_gpu(|device, _queue, _configuration| Self::upload_with(device, primitive))
    }

    /// Uploads a textured primitive using the device attached to `frame`.
    ///
    /// # Errors
    ///
    /// Returns [`TexturedMeshUploadError`] under the same conditions as
    /// [`Self::upload_mesh`], including a missing primary UV stream.
    pub fn upload_mesh_for_frame(
        &self,
        frame: &RenderFrame<'_>,
        primitive: &MeshPrimitive,
    ) -> Result<GpuTexturedMesh, TexturedMeshUploadError> {
        Self::upload_with(frame.device(), primitive)
    }

    /// Draws a textured opaque mesh and starts a fresh depth phase.
    ///
    /// # Errors
    ///
    /// Returns [`TexturedMeshRenderError::MissingTexture`] for an unresolved
    /// material, or wraps invalid camera/matrix/factor data.
    pub fn draw(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        mesh: &GpuTexturedMesh,
        model_matrix: [f32; 16],
        material: TexturedMaterial3d<'_>,
    ) -> Result<MeshDrawStats, TexturedMeshRenderError> {
        self.draw_with_depth_load(
            frame,
            camera,
            mesh,
            model_matrix,
            material,
            DepthLoad::Clear,
        )
    }

    /// Draws a textured opaque mesh with explicit depth load behavior.
    ///
    /// Begin a multi-mesh opaque phase with [`DepthLoad::Clear`] and use
    /// [`DepthLoad::Load`] for every later draw. This matches
    /// [`SceneRenderer3d`]'s depth contract, so callers can place this phase
    /// before or after a scene only when they intentionally choose the depth
    /// reset boundary.
    ///
    /// # Errors
    ///
    /// Returns [`TexturedMeshRenderError::MissingTexture`] for an unresolved
    /// material, or wraps invalid camera/matrix/factor data.
    pub fn draw_with_depth_load(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        mesh: &GpuTexturedMesh,
        model_matrix: [f32; 16],
        material: TexturedMaterial3d<'_>,
        depth_load: DepthLoad,
    ) -> Result<MeshDrawStats, TexturedMeshRenderError> {
        self.draw_with_depth_load_rasterization(
            frame,
            camera,
            mesh,
            model_matrix,
            material,
            depth_load,
            false,
        )
    }

    /// Records one textured draw while preserving a standard material's
    /// explicit rasterization mode.
    #[allow(clippy::too_many_arguments)] // Texture draw already requires explicit GPU and raster state.
    fn draw_with_depth_load_rasterization(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        mesh: &GpuTexturedMesh,
        model_matrix: [f32; 16],
        material: TexturedMaterial3d<'_>,
        depth_load: DepthLoad,
        double_sided: bool,
    ) -> Result<MeshDrawStats, TexturedMeshRenderError> {
        self.draw_with_depth_load_rasterization_phase(
            frame,
            camera,
            mesh,
            model_matrix,
            material,
            depth_load,
            double_sided,
            false,
        )
    }

    /// Draws one transparent textured mesh over an existing opaque depth phase.
    ///
    /// It uses source-over blending, tests but does not write depth, and
    /// therefore requires the caller to submit meshes back to front.
    ///
    /// # Errors
    ///
    /// Returns [`TexturedMeshRenderError`] when input data or the texture is invalid.
    pub fn draw_transparent_with_model_matrix(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        mesh: &GpuTexturedMesh,
        model_matrix: [f32; 16],
        material: TexturedMaterial3d<'_>,
    ) -> Result<MeshDrawStats, TexturedMeshRenderError> {
        self.draw_with_depth_load_rasterization_phase(
            frame,
            camera,
            mesh,
            model_matrix,
            material,
            DepthLoad::Load,
            false,
            true,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_with_depth_load_rasterization_phase(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        mesh: &GpuTexturedMesh,
        model_matrix: [f32; 16],
        material: TexturedMaterial3d<'_>,
        depth_load: DepthLoad,
        double_sided: bool,
        transparent: bool,
    ) -> Result<MeshDrawStats, TexturedMeshRenderError> {
        let view_projection = camera
            .view_projection(frame.draw_size())
            .map_err(TexturedMeshRenderError::Mesh)?;
        let uniform = MeshUniform::from_matrix(model_matrix, material.base_color_factor)
            .map_err(TexturedMeshRenderError::Mesh)?;
        let texture = material
            .texture
            .ok_or(TexturedMeshRenderError::MissingTexture)?;
        let texture_bind_group = frame
            .device()
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("yuyib textured mesh material bind group"),
                layout: &self.texture_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(texture.view()),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(texture.sampler()),
                    },
                ],
            });
        frame
            .queue()
            .write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&view_projection));
        frame
            .queue()
            .write_buffer(&self.instance_buffer, 0, bytemuck::bytes_of(&uniform));
        frame.with_surface_pass_with_depth(wgpu::LoadOp::Load, depth_load.operation(), |pass| {
            pass.set_pipeline(match (transparent, double_sided) {
                (false, false) => &self.pipeline,
                (false, true) => &self.double_sided_pipeline,
                (true, false) => &self.transparent_pipeline,
                (true, true) => &self.transparent_double_sided_pipeline,
            });
            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            pass.set_bind_group(1, &self.instance_bind_group, &[]);
            pass.set_bind_group(2, &texture_bind_group, &[]);
            pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
            pass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..mesh.index_count, 0, 0..1);
        });
        Ok(MeshDrawStats {
            triangles: mesh.index_count / 3,
            draw_calls: 1,
            transient_uniform_buffer_allocations: 0,
        })
    }

    #[allow(clippy::too_many_lines)] // Keeping the complete immutable pipeline together aids review.
    fn create(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        depth_format: wgpu::TextureFormat,
    ) -> Self {
        let camera_layout = uniform_layout(
            device,
            "yuyib textured mesh camera layout",
            wgpu::ShaderStages::VERTEX,
        );
        let instance_layout = uniform_layout(
            device,
            "yuyib textured mesh instance layout",
            wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
        );
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("yuyib textured mesh material layout"),
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
        });
        let camera_buffer = uniform_buffer(
            device,
            "yuyib textured mesh camera",
            size_of::<[f32; 16]>() as u64,
        );
        let instance_buffer = uniform_buffer(
            device,
            "yuyib textured mesh instance",
            size_of::<MeshUniform>() as u64,
        );
        let camera_bind_group = uniform_bind_group(
            device,
            "yuyib textured mesh camera bind group",
            &camera_layout,
            &camera_buffer,
        );
        let instance_bind_group = uniform_bind_group(
            device,
            "yuyib textured mesh instance bind group",
            &instance_layout,
            &instance_buffer,
        );
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("yuyib textured unlit mesh WGSL"),
            source: wgpu::ShaderSource::Wgsl(TEXTURED_UNLIT_MESH_WGSL.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("yuyib textured unlit mesh pipeline layout"),
            bind_group_layouts: &[
                Some(&camera_layout),
                Some(&instance_layout),
                Some(&texture_layout),
            ],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("yuyib textured unlit mesh pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(TEXTURED_VERTEX_LAYOUT)],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: Some(wgpu::Face::Back),
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
        let double_sided_pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("yuyib textured unlit mesh double-sided pipeline"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[Some(TEXTURED_VERTEX_LAYOUT)],
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
        let transparent_pipeline = create_textured_transparent_pipeline(
            device,
            &pipeline_layout,
            &shader,
            format,
            depth_format,
            false,
        );
        let transparent_double_sided_pipeline = create_textured_transparent_pipeline(
            device,
            &pipeline_layout,
            &shader,
            format,
            depth_format,
            true,
        );
        Self {
            pipeline,
            double_sided_pipeline,
            transparent_pipeline,
            transparent_double_sided_pipeline,
            camera_buffer,
            instance_buffer,
            camera_bind_group,
            instance_bind_group,
            texture_layout,
        }
    }

    fn upload_with(
        device: &wgpu::Device,
        primitive: &MeshPrimitive,
    ) -> Result<GpuTexturedMesh, TexturedMeshUploadError> {
        let tex_coords = primitive
            .tex_coords_0()
            .ok_or(TexturedMeshUploadError::MissingTexCoords0)?;
        let vertex_count = u32::try_from(primitive.positions().len()).map_err(|_| {
            TexturedMeshUploadError::TooManyVertices {
                actual: primitive.positions().len(),
            }
        })?;
        let index_count = u32::try_from(primitive.indices().len()).map_err(|_| {
            TexturedMeshUploadError::TooManyIndices {
                actual: primitive.indices().len(),
            }
        })?;
        let vertices: Vec<TexturedVertex> = primitive
            .positions()
            .iter()
            .copied()
            .zip(tex_coords.iter().copied())
            .enumerate()
            .map(|(index, (position, tex_coord))| {
                if !all_finite(&position) {
                    return Err(TexturedMeshUploadError::NonFinitePosition { index });
                }
                if !all_finite(&tex_coord) {
                    return Err(TexturedMeshUploadError::NonFiniteTexCoords0 { index });
                }
                Ok(TexturedVertex {
                    position,
                    tex_coord,
                })
            })
            .collect::<Result<_, _>>()?;
        debug_assert_eq!(
            vertices.len(),
            usize::try_from(vertex_count).unwrap_or(usize::MAX)
        );
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("yuyib 3d textured mesh vertices"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        });
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("yuyib 3d textured mesh indices"),
            contents: bytemuck::cast_slice(primitive.indices()),
            usage: wgpu::BufferUsages::INDEX,
        });
        Ok(GpuTexturedMesh {
            vertex_buffer,
            index_buffer,
            index_count,
        })
    }
}

/// A mesh with positions, normals and UV0 prepared for textured Lambert light.
///
/// This is a separate GPU resource because the unlit texture path deliberately
/// does not spend bandwidth on normals.  Keep it with the renderer that owns
/// the shader contract.
pub struct GpuTexturedLitMesh {
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
}

/// A sampled base-colour binding kept on the GPU.
///
/// Create this once when a material becomes resident, then reuse it for every
/// draw.  It owns no texture: the caller must retain the [`GpuTexture`] that
/// was used to create it for at least as long as this binding is submitted.
/// This explicit lifetime boundary means a texture replacement cannot leave a
/// hidden stale descriptor inside the renderer.
#[derive(Clone)]
pub struct GpuTexturedLitMaterial {
    bind_group: Arc<wgpu::BindGroup>,
}

/// One item for [`TexturedLitMeshRenderer3d::draw_batch_with_depth_load`].
///
/// The batch is deliberately opaque-only.  Transparent materials need a
/// sorted phase and must not be folded into this order-independent pass.
pub struct TexturedLitBatchDraw<'a> {
    /// GPU mesh containing position, normal and UV0 streams.
    pub mesh: &'a GpuTexturedLitMesh,
    /// Transform and shared Lambert light for this instance.
    pub instance: LitMeshInstance3d,
    /// Cached sampled base-colour binding.
    pub material: &'a GpuTexturedLitMaterial,
    /// Keeps both sides of this primitive visible without changing other draws.
    pub double_sided: bool,
}

impl<'a> TexturedLitBatchDraw<'a> {
    /// Creates one opaque textured-Lambert batch item.
    #[must_use]
    pub const fn new(
        mesh: &'a GpuTexturedLitMesh,
        instance: LitMeshInstance3d,
        material: &'a GpuTexturedLitMaterial,
        double_sided: bool,
    ) -> Self {
        Self {
            mesh,
            instance,
            material,
            double_sided,
        }
    }
}

impl GpuTexturedLitMesh {
    /// Returns the triangle-list index count uploaded to GPU memory.
    #[must_use]
    pub const fn index_count(&self) -> u32 {
        self.index_count
    }
}

/// Borrowed base-colour input for one textured Lambert draw.
///
/// Texture sampling and the RGBA multiplier are intentionally the same
/// material inputs as [`TexturedMaterial3d`].  Lighting is passed separately
/// through [`LitMeshInstance3d`], which prevents a material asset from owning
/// scene light state.
#[derive(Clone, Copy)]
pub struct TexturedLitMaterial3d<'texture> {
    texture: Option<&'texture GpuTexture>,
    base_color_factor: [f32; 4],
}

impl<'texture> TexturedLitMaterial3d<'texture> {
    /// Creates a material that samples a base-colour texture.
    #[must_use]
    pub const fn new(texture: &'texture GpuTexture, base_color_factor: [f32; 4]) -> Self {
        Self {
            texture: Some(texture),
            base_color_factor,
        }
    }

    /// Creates a not-yet-resident material. Drawing it returns an explicit error.
    #[must_use]
    pub const fn unbound(base_color_factor: [f32; 4]) -> Self {
        Self {
            texture: None,
            base_color_factor,
        }
    }

    /// Returns the sampled base-colour texture, if it is resident.
    #[must_use]
    pub const fn texture(self) -> Option<&'texture GpuTexture> {
        self.texture
    }

    /// Returns the linear RGBA multiplier.
    #[must_use]
    pub const fn base_color_factor(self) -> [f32; 4] {
        self.base_color_factor
    }
}

/// Low-level renderer for textured Lambert materials.
///
/// It preserves a material's base-colour texture, transforms vertex normals
/// using the inverse-transpose model basis and combines an ambient colour with
/// one directional Lambert light.  This is deliberately not PBR: normal maps,
/// shadows, metallic/roughness and image based lighting are separate layers.
pub struct TexturedLitMeshRenderer3d {
    pipeline: wgpu::RenderPipeline,
    double_sided_pipeline: wgpu::RenderPipeline,
    mirrored_pipeline: wgpu::RenderPipeline,
    mirrored_double_sided_pipeline: wgpu::RenderPipeline,
    camera_buffer: wgpu::Buffer,
    draw_buffer: wgpu::Buffer,
    draw_uniform_stride: u64,
    camera_bind_group: wgpu::BindGroup,
    draw_bind_group: wgpu::BindGroup,
    texture_layout: wgpu::BindGroupLayout,
}

/// Maximum opaque items recorded in one textured-Lambert pass.
///
/// The uniform buffer is intentionally bounded instead of growing during a
/// frame.  Larger scenes are transparently split into multiple passes by the
/// high-level scene renderer, which keeps GPU allocation out of gameplay.
const TEXTURED_LIT_BATCH_CAPACITY: usize = 512;

impl TexturedLitMeshRenderer3d {
    /// Creates a renderer from an application renderer.
    #[must_use]
    pub fn new(renderer: &Renderer) -> Self {
        let depth_format = renderer.depth_format();
        let color_format = renderer.color_target_format();
        renderer.with_raw_gpu(|device, _queue, _configuration| {
            Self::create(device, color_format, depth_format)
        })
    }

    /// Creates a renderer from the device associated with the current frame.
    #[must_use]
    pub fn new_for_frame(frame: &RenderFrame<'_>) -> Self {
        Self::create(frame.device(), frame.surface_format(), frame.depth_format())
    }

    /// Uploads a primitive which has both normals and UV0.
    ///
    /// # Errors
    ///
    /// Returns a structured error instead of generating normals or UVs: those
    /// choices belong to the importer or a mesh-building tool.
    pub fn upload_mesh(
        &self,
        renderer: &Renderer,
        primitive: &MeshPrimitive,
    ) -> Result<GpuTexturedLitMesh, TexturedLitMeshUploadError> {
        renderer.with_raw_gpu(|device, _queue, _configuration| Self::upload_with(device, primitive))
    }

    /// Uploads a primitive through the current frame device.
    ///
    /// # Errors
    ///
    /// Returns [`TexturedLitMeshUploadError`] if normals/UV0 are absent or a
    /// source vertex cannot form valid GPU data.
    pub fn upload_mesh_for_frame(
        &self,
        frame: &RenderFrame<'_>,
        primitive: &MeshPrimitive,
    ) -> Result<GpuTexturedLitMesh, TexturedLitMeshUploadError> {
        Self::upload_with(frame.device(), primitive)
    }

    /// Creates a reusable sampled binding for one resident texture.
    ///
    /// This is the low-level counterpart to the model cache in
    /// [`LitSceneRenderer3d`].  Call it while a material is loaded, rather than
    /// every frame, and recreate it whenever the underlying [`GpuTexture`] is
    /// replaced.
    #[must_use]
    pub fn upload_material_for_frame(
        &self,
        frame: &RenderFrame<'_>,
        texture: &GpuTexture,
    ) -> GpuTexturedLitMaterial {
        GpuTexturedLitMaterial {
            bind_group: Arc::new(
                frame
                    .device()
                    .create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("yuyib textured Lambert cached material bind group"),
                        layout: &self.texture_layout,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::TextureView(texture.view()),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::Sampler(texture.sampler()),
                            },
                        ],
                    }),
            ),
        }
    }

    /// Records opaque textured-Lambert meshes in one shared GPU pass.
    ///
    /// Each item still issues one indexed draw, but camera state, depth state
    /// and the surface pass are shared. This is the intended low-level API for
    /// maps and static scenes with many material primitives. A batch can hold
    /// at most 512 items; callers with more work
    /// should split it at a deliberate phase boundary.
    ///
    /// # Errors
    ///
    /// Returns an error before encoding anything when a camera, transform or
    /// light is invalid, or when the caller exceeds the fixed batch capacity.
    ///
    /// # Panics
    ///
    /// Panics only if a GPU reports a dynamic-uniform alignment that makes the
    /// documented 512-item phase exceed WGPU's `u32` dynamic-offset range.
    /// Such a device cannot satisfy this renderer's fixed pipeline contract.
    pub fn draw_batch_with_depth_load(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        draws: &[TexturedLitBatchDraw<'_>],
        depth_load: DepthLoad,
    ) -> Result<MeshDrawStats, TexturedLitMeshRenderError> {
        if draws.len() > TEXTURED_LIT_BATCH_CAPACITY {
            return Err(TexturedLitMeshRenderError::BatchTooLarge {
                actual: draws.len(),
                maximum: TEXTURED_LIT_BATCH_CAPACITY,
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
                LitMeshUniform::new(
                    draw.instance.model_matrix,
                    draw.instance.material,
                    draw.instance.lighting,
                )
                .map_err(TexturedLitMeshRenderError::Lit)
            })
            .collect::<Result<Vec<_>, _>>()?;
        frame
            .queue()
            .write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&view_projection));
        for (index, uniform) in uniforms.iter().enumerate() {
            let offset = u64::try_from(index)
                .expect("batch capacity fits u64")
                .saturating_mul(self.draw_uniform_stride);
            frame
                .queue()
                .write_buffer(&self.draw_buffer, offset, bytemuck::bytes_of(uniform));
        }
        frame.with_surface_pass_with_depth(wgpu::LoadOp::Load, depth_load.operation(), |pass| {
            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            for (index, draw) in draws.iter().enumerate() {
                pass.set_pipeline(self.pipeline_for(draw.instance.model_matrix, draw.double_sided));
                let offset = u64::try_from(index)
                    .expect("batch capacity fits u64")
                    .saturating_mul(self.draw_uniform_stride);
                let offset =
                    u32::try_from(offset).expect("textured Lambert dynamic offset fits u32");
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

    /// Starts a fresh opaque depth phase and draws one material.
    ///
    /// # Errors
    ///
    /// Returns [`TexturedLitMeshRenderError`] for an unresolved texture or
    /// invalid camera, transform and Lambert data.
    pub fn draw(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        mesh: &GpuTexturedLitMesh,
        instance: LitMeshInstance3d,
        material: TexturedLitMaterial3d<'_>,
    ) -> Result<MeshDrawStats, TexturedLitMeshRenderError> {
        self.draw_with_depth_load(frame, camera, mesh, instance, material, DepthLoad::Clear)
    }

    /// Draws one opaque material with an explicit shared depth phase.
    ///
    /// # Errors
    ///
    /// Returns [`TexturedLitMeshRenderError`] under the same conditions as
    /// [`Self::draw`].
    pub fn draw_with_depth_load(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        mesh: &GpuTexturedLitMesh,
        instance: LitMeshInstance3d,
        material: TexturedLitMaterial3d<'_>,
        depth_load: DepthLoad,
    ) -> Result<MeshDrawStats, TexturedLitMeshRenderError> {
        self.draw_with_depth_load_rasterization(
            frame, camera, mesh, instance, material, depth_load, false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_with_depth_load_rasterization(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        mesh: &GpuTexturedLitMesh,
        mut instance: LitMeshInstance3d,
        material: TexturedLitMaterial3d<'_>,
        depth_load: DepthLoad,
        double_sided: bool,
    ) -> Result<MeshDrawStats, TexturedLitMeshRenderError> {
        let texture = material
            .texture
            .ok_or(TexturedLitMeshRenderError::MissingTexture)?;
        instance.material = LitMaterial3d::new(material.base_color_factor);
        let view_projection = camera
            .view_projection(frame.draw_size())
            .map_err(TexturedLitMeshRenderError::Mesh)?;
        let uniform =
            LitMeshUniform::new(instance.model_matrix, instance.material, instance.lighting)
                .map_err(TexturedLitMeshRenderError::Lit)?;
        let texture_bind_group = self.upload_material_for_frame(frame, texture);
        frame
            .queue()
            .write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&view_projection));
        frame
            .queue()
            .write_buffer(&self.draw_buffer, 0, bytemuck::bytes_of(&uniform));
        frame.with_surface_pass_with_depth(wgpu::LoadOp::Load, depth_load.operation(), |pass| {
            pass.set_pipeline(self.pipeline_for(instance.model_matrix, double_sided));
            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            pass.set_bind_group(1, &self.draw_bind_group, &[0]);
            pass.set_bind_group(2, texture_bind_group.bind_group.as_ref(), &[]);
            pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
            pass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..mesh.index_count, 0, 0..1);
        });
        Ok(MeshDrawStats {
            triangles: mesh.index_count / 3,
            draw_calls: 1,
            transient_uniform_buffer_allocations: 0,
        })
    }

    #[allow(clippy::too_many_lines)]
    fn create(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        depth_format: wgpu::TextureFormat,
    ) -> Self {
        let camera_layout = uniform_layout(
            device,
            "yuyib textured Lambert camera layout",
            wgpu::ShaderStages::VERTEX,
        );
        let draw_layout = dynamic_uniform_layout(
            device,
            "yuyib textured Lambert draw layout",
            wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
            size_of::<LitMeshUniform>() as u64,
        );
        let texture_layout =
            textured_material_layout(device, "yuyib textured Lambert material layout");
        let camera_buffer = uniform_buffer(
            device,
            "yuyib textured Lambert camera",
            size_of::<[f32; 16]>() as u64,
        );
        let draw_uniform_stride = aligned_uniform_stride(
            device.limits().min_uniform_buffer_offset_alignment,
            size_of::<LitMeshUniform>() as u64,
        );
        let draw_buffer = uniform_buffer(
            device,
            "yuyib textured Lambert draw",
            draw_uniform_stride.saturating_mul(
                u64::try_from(TEXTURED_LIT_BATCH_CAPACITY).expect("capacity fits u64"),
            ),
        );
        let camera_bind_group = uniform_bind_group(
            device,
            "yuyib textured Lambert camera bind group",
            &camera_layout,
            &camera_buffer,
        );
        let draw_bind_group = dynamic_uniform_bind_group(
            device,
            "yuyib textured Lambert draw bind group",
            &draw_layout,
            &draw_buffer,
            size_of::<LitMeshUniform>() as u64,
        );
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("yuyib textured Lambert mesh WGSL"),
            source: wgpu::ShaderSource::Wgsl(TEXTURED_LIT_MESH_WGSL.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("yuyib textured Lambert mesh pipeline layout"),
            bind_group_layouts: &[
                Some(&camera_layout),
                Some(&draw_layout),
                Some(&texture_layout),
            ],
            immediate_size: 0,
        });
        let make_pipeline = |label, front_face, cull_mode| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[Some(TEXTURED_LIT_VERTEX_LAYOUT)],
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    front_face,
                    cull_mode,
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
            })
        };
        Self {
            pipeline: make_pipeline(
                "yuyib textured Lambert mesh pipeline",
                wgpu::FrontFace::Ccw,
                Some(wgpu::Face::Back),
            ),
            double_sided_pipeline: make_pipeline(
                "yuyib textured Lambert mesh double-sided pipeline",
                wgpu::FrontFace::Ccw,
                None,
            ),
            mirrored_pipeline: make_pipeline(
                "yuyib textured Lambert mesh mirrored pipeline",
                wgpu::FrontFace::Cw,
                Some(wgpu::Face::Back),
            ),
            mirrored_double_sided_pipeline: make_pipeline(
                "yuyib textured Lambert mesh mirrored double-sided pipeline",
                wgpu::FrontFace::Cw,
                None,
            ),
            camera_buffer,
            draw_buffer,
            draw_uniform_stride,
            camera_bind_group,
            draw_bind_group,
            texture_layout,
        }
    }

    fn pipeline_for(&self, model_matrix: [f32; 16], double_sided: bool) -> &wgpu::RenderPipeline {
        match lambert_rasterization(model_matrix, double_sided) {
            LambertRasterization::Regular => &self.pipeline,
            LambertRasterization::DoubleSided => &self.double_sided_pipeline,
            LambertRasterization::Mirrored => &self.mirrored_pipeline,
            LambertRasterization::MirroredDoubleSided => &self.mirrored_double_sided_pipeline,
        }
    }

    fn upload_with(
        device: &wgpu::Device,
        primitive: &MeshPrimitive,
    ) -> Result<GpuTexturedLitMesh, TexturedLitMeshUploadError> {
        let normals = primitive
            .normals()
            .ok_or(TexturedLitMeshUploadError::MissingNormals)?;
        let tex_coords = primitive
            .tex_coords_0()
            .ok_or(TexturedLitMeshUploadError::MissingTexCoords0)?;
        let index_count = u32::try_from(primitive.indices().len()).map_err(|_| {
            TexturedLitMeshUploadError::TooManyIndices {
                actual: primitive.indices().len(),
            }
        })?;
        let vertices: Vec<TexturedLitVertex> = primitive
            .positions()
            .iter()
            .copied()
            .zip(normals.iter().copied())
            .zip(tex_coords.iter().copied())
            .enumerate()
            .map(|(index, ((position, normal), tex_coord))| {
                if !all_finite(&position) {
                    return Err(TexturedLitMeshUploadError::NonFinitePosition { index });
                }
                if !all_finite(&normal) {
                    return Err(TexturedLitMeshUploadError::NonFiniteNormal { index });
                }
                if normalize3(normal).is_none() {
                    return Err(TexturedLitMeshUploadError::DegenerateNormal { index });
                }
                if !all_finite(&tex_coord) {
                    return Err(TexturedLitMeshUploadError::NonFiniteTexCoords0 { index });
                }
                Ok(TexturedLitVertex {
                    position,
                    normal,
                    tex_coord,
                })
            })
            .collect::<Result<_, _>>()?;
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("yuyib 3d textured Lambert mesh vertices"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("yuyib 3d textured Lambert mesh indices"),
            contents: bytemuck::cast_slice(primitive.indices()),
            usage: wgpu::BufferUsages::INDEX,
        });
        Ok(GpuTexturedLitMesh {
            vertex_buffer,
            index_buffer,
            index_count,
        })
    }
}

/// Failure while uploading a textured Lambert mesh.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TexturedLitMeshUploadError {
    /// The source mesh has no normal stream.
    MissingNormals,
    /// The source mesh has no primary texture coordinates.
    MissingTexCoords0,
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
    /// Index count cannot be represented by WGPU.
    TooManyIndices {
        /// Observed index count.
        actual: usize,
    },
}

impl fmt::Display for TexturedLitMeshUploadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingNormals => formatter.write_str("textured Lambert mesh requires normals"),
            Self::MissingTexCoords0 => formatter.write_str("textured Lambert mesh requires UV0"),
            Self::NonFinitePosition { index } => write!(
                formatter,
                "textured Lambert position at index {index} is not finite"
            ),
            Self::NonFiniteNormal { index } => write!(
                formatter,
                "textured Lambert normal at index {index} is not finite"
            ),
            Self::DegenerateNormal { index } => write!(
                formatter,
                "textured Lambert normal at index {index} has zero length"
            ),
            Self::NonFiniteTexCoords0 { index } => write!(
                formatter,
                "textured Lambert UV0 at index {index} is not finite"
            ),
            Self::TooManyIndices { actual } => write!(
                formatter,
                "textured Lambert mesh has {actual} indices; WGPU accepts at most u32::MAX"
            ),
        }
    }
}
impl Error for TexturedLitMeshUploadError {}

/// Failure while drawing a textured Lambert mesh.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TexturedLitMeshRenderError {
    /// The material has no resident base-colour texture.
    MissingTexture,
    /// The caller requested more draws than the fixed dynamic-uniform phase can hold.
    BatchTooLarge {
        /// Requested draw count.
        actual: usize,
        /// Maximum draw count in one pass.
        maximum: usize,
    },
    /// Camera or model state is invalid.
    Mesh(MeshRenderError),
    /// The Lambert transform/material state is invalid.
    Lit(LitMeshRenderError),
}

impl fmt::Display for TexturedLitMeshRenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingTexture => {
                formatter.write_str("textured Lambert material has no resident texture")
            }
            Self::BatchTooLarge { actual, maximum } => write!(
                formatter,
                "textured Lambert batch contains {actual} draws; the pass accepts at most {maximum}"
            ),
            Self::Mesh(source) => write!(formatter, "cannot draw textured Lambert mesh: {source}"),
            Self::Lit(source) => write!(
                formatter,
                "cannot apply textured Lambert lighting: {source}"
            ),
        }
    }
}
impl Error for TexturedLitMeshRenderError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Mesh(source) => Some(source),
            Self::Lit(source) => Some(source),
            Self::MissingTexture | Self::BatchTooLarge { .. } => None,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct TexturedLitVertex {
    position: [f32; 3],
    normal: [f32; 3],
    tex_coord: [f32; 2],
}

const TEXTURED_LIT_VERTEX_LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
    array_stride: size_of::<TexturedLitVertex>() as u64,
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

fn textured_material_layout(device: &wgpu::Device, label: &'static str) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
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

const TEXTURED_LIT_MESH_WGSL: &str = r"
struct Camera { view_projection: mat4x4<f32>, };
struct Draw {
    model: mat4x4<f32>, normal_matrix: mat3x3<f32>, base_color: vec4<f32>,
    light_direction: vec4<f32>, light_color: vec4<f32>, ambient: vec4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var<uniform> draw: Draw;
@group(2) @binding(0) var base_color_texture: texture_2d<f32>;
@group(2) @binding(1) var base_color_sampler: sampler;
struct VertexInput { @location(0) position: vec3<f32>, @location(1) normal: vec3<f32>, @location(2) tex_coord: vec2<f32>, };
struct VertexOutput { @builtin(position) clip_position: vec4<f32>, @location(0) normal: vec3<f32>, @location(1) tex_coord: vec2<f32>, };
@vertex fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    output.clip_position = camera.view_projection * draw.model * vec4<f32>(input.position, 1.0);
    output.normal = normalize(draw.normal_matrix * input.normal);
    output.tex_coord = input.tex_coord;
    return output;
}
@fragment fn fs_main(input: VertexOutput, @builtin(front_facing) front_facing: bool) -> @location(0) vec4<f32> {
    let normal = select(-normalize(input.normal), normalize(input.normal), front_facing);
    let diffuse = max(dot(normal, normalize(-draw.light_direction.xyz)), 0.0);
    let light = draw.ambient.xyz + draw.light_color.xyz * diffuse;
    let base = textureSample(base_color_texture, base_color_sampler, input.tex_coord) * draw.base_color;
    return vec4<f32>(base.rgb * light, base.a);
}
";

fn create_textured_transparent_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
    depth_format: wgpu::TextureFormat,
    double_sided: bool,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(if double_sided {
            "yuyib textured transparent double-sided pipeline"
        } else {
            "yuyib textured transparent pipeline"
        }),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[Some(TEXTURED_VERTEX_LAYOUT)],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: if double_sided {
                None
            } else {
                Some(wgpu::Face::Back)
            },
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
            module: shader,
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

/// Failure while uploading a textured mesh.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TexturedMeshUploadError {
    /// The mesh has no primary UV stream and cannot sample a base-colour texture.
    MissingTexCoords0,
    /// A position contained NaN or infinity.
    NonFinitePosition {
        /// Position stream index.
        index: usize,
    },
    /// A UV0 value contained NaN or infinity.
    NonFiniteTexCoords0 {
        /// UV0 stream index.
        index: usize,
    },
    /// The vertex count exceeds WGPU's address range.
    TooManyVertices {
        /// Observed count.
        actual: usize,
    },
    /// The index count exceeds WGPU's draw range.
    TooManyIndices {
        /// Observed count.
        actual: usize,
    },
}

impl fmt::Display for TexturedMeshUploadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingTexCoords0 => {
                formatter.write_str("textured mesh requires primary UV coordinates (TEXCOORD_0)")
            }
            Self::NonFinitePosition { index } => write!(
                formatter,
                "textured mesh position at index {index} is not finite"
            ),
            Self::NonFiniteTexCoords0 { index } => write!(
                formatter,
                "textured mesh UV0 at index {index} is not finite"
            ),
            Self::TooManyVertices { actual } => write!(
                formatter,
                "textured mesh has {actual} vertices; WGPU accepts at most u32::MAX"
            ),
            Self::TooManyIndices { actual } => write!(
                formatter,
                "textured mesh has {actual} indices; WGPU accepts at most u32::MAX"
            ),
        }
    }
}
impl Error for TexturedMeshUploadError {}

/// Failure while recording a textured mesh draw.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TexturedMeshRenderError {
    /// No GPU texture was supplied by the material.
    MissingTexture,
    /// Camera, matrix or base-colour factor data was invalid.
    Mesh(MeshRenderError),
}

impl fmt::Display for TexturedMeshRenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingTexture => {
                formatter.write_str("textured material has no resolved GPU texture")
            }
            Self::Mesh(source) => write!(formatter, "cannot draw textured mesh: {source}"),
        }
    }
}
impl Error for TexturedMeshRenderError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Mesh(source) => Some(source),
            Self::MissingTexture => None,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct TexturedVertex {
    position: [f32; 3],
    tex_coord: [f32; 2],
}

const TEXTURED_VERTEX_LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
    array_stride: size_of::<TexturedVertex>() as u64,
    step_mode: wgpu::VertexStepMode::Vertex,
    attributes: &[
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x3,
            offset: 0,
            shader_location: 0,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x2,
            offset: size_of::<[f32; 3]>() as u64,
            shader_location: 1,
        },
    ],
};

fn uniform_layout(
    device: &wgpu::Device,
    label: &'static str,
    visibility: wgpu::ShaderStages,
) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    })
}

fn dynamic_uniform_layout(
    device: &wgpu::Device,
    label: &'static str,
    visibility: wgpu::ShaderStages,
    minimum_size: u64,
) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: true,
                min_binding_size: NonZeroU64::new(minimum_size),
            },
            count: None,
        }],
    })
}

fn aligned_uniform_stride(alignment: u32, minimum_size: u64) -> u64 {
    let alignment = u64::from(alignment.max(1));
    minimum_size.div_ceil(alignment).saturating_mul(alignment)
}

fn skin_palette_layout(device: &wgpu::Device, label: &'static str) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    })
}

fn uniform_buffer(device: &wgpu::Device, label: &'static str, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn uniform_bind_group(
    device: &wgpu::Device,
    label: &'static str,
    layout: &wgpu::BindGroupLayout,
    buffer: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: buffer.as_entire_binding(),
        }],
    })
}

fn dynamic_uniform_bind_group(
    device: &wgpu::Device,
    label: &'static str,
    layout: &wgpu::BindGroupLayout,
    buffer: &wgpu::Buffer,
    binding_size: u64,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer,
                offset: 0,
                size: NonZeroU64::new(binding_size),
            }),
        }],
    })
}

const TEXTURED_UNLIT_MESH_WGSL: &str = r"
struct Camera { view_projection: mat4x4<f32>, };
struct Instance { model: mat4x4<f32>, color: vec4<f32>, };
@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var<uniform> instance: Instance;
@group(2) @binding(0) var base_color_texture: texture_2d<f32>;
@group(2) @binding(1) var base_color_sampler: sampler;
struct VertexInput { @location(0) position: vec3<f32>, @location(1) tex_coord: vec2<f32>, };
struct VertexOutput { @builtin(position) clip_position: vec4<f32>, @location(0) tex_coord: vec2<f32>, };
@vertex fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    output.clip_position = camera.view_projection * instance.model * vec4<f32>(input.position, 1.0);
    output.tex_coord = input.tex_coord;
    return output;
}
@fragment fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(base_color_texture, base_color_sampler, input.tex_coord) * instance.color;
}
";

/// Renderer lighting parameters for one Lambert opaque phase.
///
/// The directional source is the renderer-neutral ECS extraction type from
/// `yuyib-game-3d`. Its ray direction is normalized again at this boundary so
/// manually constructed draw values cannot poison a GPU uniform. Direct light
/// is `color × illuminance_lux`; this initial renderer deliberately has no
/// exposure or physical-unit conversion, so applications should choose an
/// exposure policy before supplying large real-world lux values.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LambertLighting3d {
    light: DirectionalLightDraw,
    ambient: [f32; 3],
}

impl LambertLighting3d {
    /// Создаёт свет из простых художественных параметров.
    ///
    /// Это высокий путь для небольших приложений и прототипов: `intensity`
    /// — безразмерная яркость именно этого Lambert-прохода, а не физические
    /// люксы. Поэтому значение около `0.5` обычно даёт мягкий свет без
    /// пересвета, а `ambient` заполняет тёмные стороны.
    ///
    /// Для света, который уже живёт в ECS как [`DirectionalLightDraw`],
    /// используйте низкоуровневый [`Self::new`]. Так реальные физические
    /// единицы не смешиваются с художественной настройкой камеры и экрана.
    ///
    /// # Errors
    ///
    /// Возвращает ту же [`LambertLightingError`], что и [`Self::new`], при
    /// некорректном направлении, цвете, яркости или ambient-компоненте.
    pub fn artistic(
        direction: [f32; 3],
        color: [f32; 3],
        intensity: f32,
        ambient: [f32; 3],
    ) -> Result<Self, LambertLightingError> {
        Self::new(
            DirectionalLightDraw {
                direction,
                color,
                illuminance_lux: intensity,
            },
            ambient,
        )
    }

    /// Validates a renderer-neutral directional light and ambient RGB term.
    ///
    /// # Errors
    ///
    /// Returns [`LambertLightingError`] when light data or ambient values are
    /// non-finite, negative where prohibited, or use a zero direction.
    pub fn new(
        light: DirectionalLightDraw,
        ambient: [f32; 3],
    ) -> Result<Self, LambertLightingError> {
        let direction =
            normalize3(light.direction).ok_or(LambertLightingError::InvalidDirection)?;
        if !non_negative_finite(&light.color) {
            return Err(LambertLightingError::InvalidColor);
        }
        if !light.illuminance_lux.is_finite() || light.illuminance_lux < 0.0 {
            return Err(LambertLightingError::InvalidIlluminance);
        }
        if !all_finite(&mul3(light.color, light.illuminance_lux)) {
            return Err(LambertLightingError::DirectRadianceOverflow);
        }
        if !non_negative_finite(&ambient) {
            return Err(LambertLightingError::InvalidAmbient);
        }
        Ok(Self {
            light: DirectionalLightDraw { direction, ..light },
            ambient,
        })
    }

    /// Returns the normalized ECS-extracted directional light.
    #[must_use]
    pub const fn light(self) -> DirectionalLightDraw {
        self.light
    }

    /// Returns the linear non-negative ambient RGB term.
    #[must_use]
    pub const fn ambient(self) -> [f32; 3] {
        self.ambient
    }
}

impl Default for LambertLighting3d {
    fn default() -> Self {
        Self::new(
            DirectionalLightDraw {
                direction: [0.25, -1.0, -0.5],
                color: [1.0; 3],
                illuminance_lux: 1.0,
            },
            [0.08; 3],
        )
        .expect("the built-in Lambert lighting is valid")
    }
}

/// Validation failure for [`LambertLighting3d`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LambertLightingError {
    /// Light direction was non-finite or too small to normalize.
    InvalidDirection,
    /// Direct-light RGB contained a non-finite or negative value.
    InvalidColor,
    /// Illuminance was non-finite or negative.
    InvalidIlluminance,
    /// `color × illuminance_lux` overflowed to a non-finite GPU value.
    DirectRadianceOverflow,
    /// Ambient RGB contained a non-finite or negative value.
    InvalidAmbient,
}

impl fmt::Display for LambertLightingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDirection => {
                formatter.write_str("directional light direction must be finite and non-zero")
            }
            Self::InvalidColor => {
                formatter.write_str("directional light colour must be finite and non-negative")
            }
            Self::InvalidIlluminance => {
                formatter.write_str("directional light illuminance must be finite and non-negative")
            }
            Self::DirectRadianceOverflow => formatter
                .write_str("directional light colour multiplied by illuminance must remain finite"),
            Self::InvalidAmbient => {
                formatter.write_str("Lambert ambient light must be finite and non-negative")
            }
        }
    }
}
impl Error for LambertLightingError {}

/// Base-colour input for one lit opaque mesh draw.
///
/// The first lit phase intentionally accepts colour only. A later lit texture
/// variant can reuse [`GpuTexturedMesh`] and add a material bind group without
/// overloading this API with incomplete PBR semantics.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LitMaterial3d {
    base_color_factor: [f32; 4],
}

impl LitMaterial3d {
    /// Creates a linear RGBA base-colour multiplier.
    #[must_use]
    pub const fn new(base_color_factor: [f32; 4]) -> Self {
        Self { base_color_factor }
    }

    /// Returns the linear RGBA base-colour multiplier.
    #[must_use]
    pub const fn base_color_factor(self) -> [f32; 4] {
        self.base_color_factor
    }
}

impl Default for LitMaterial3d {
    fn default() -> Self {
        Self::new([1.0, 1.0, 1.0, 1.0])
    }
}

/// A complete per-draw input for [`LitMeshRenderer3d`].
///
/// Keeping transform, material and light together avoids parameter-order bugs
/// in the low-level depth-load entry point while still letting ECS or a scene
/// system own those values independently.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LitMeshInstance3d {
    /// Column-major model-to-world matrix.
    pub model_matrix: [f32; 16],
    /// Linear base-colour material.
    pub material: LitMaterial3d,
    /// Lambert directional/ambient lighting for this draw.
    pub lighting: LambertLighting3d,
}

impl LitMeshInstance3d {
    /// Creates one lit draw input.
    #[must_use]
    pub const fn new(
        model_matrix: [f32; 16],
        material: LitMaterial3d,
        lighting: LambertLighting3d,
    ) -> Self {
        Self {
            model_matrix,
            material,
            lighting,
        }
    }
}

impl Default for LitMeshInstance3d {
    fn default() -> Self {
        Self::new(
            [
                1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
            ],
            LitMaterial3d::default(),
            LambertLighting3d::default(),
        )
    }
}

/// A position-and-normal mesh resident on the GPU for Lambert lighting.
pub struct GpuLitMesh {
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
}

impl GpuLitMesh {
    /// Returns the uploaded triangle-list index count.
    #[must_use]
    pub const fn index_count(&self) -> u32 {
        self.index_count
    }
}

/// Opaque Lambert renderer with one directional light and ambient term.
///
/// This is a deliberately narrow lighting phase: it uses vertex normals,
/// Lambert diffuse and ambient RGB only. It has no shadow maps, normal maps,
/// image-based lighting, specular BRDF, transparency or PBR texture inputs.
/// The CPU derives an inverse-transpose normal matrix from each model matrix,
/// so non-uniform scale is handled correctly when the transform is invertible.
pub struct LitMeshRenderer3d {
    pipeline: wgpu::RenderPipeline,
    double_sided_pipeline: wgpu::RenderPipeline,
    mirrored_pipeline: wgpu::RenderPipeline,
    mirrored_double_sided_pipeline: wgpu::RenderPipeline,
    camera_buffer: wgpu::Buffer,
    draw_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    draw_bind_group: wgpu::BindGroup,
}

impl LitMeshRenderer3d {
    /// Creates a lit renderer for `renderer`'s presentation format.
    #[must_use]
    pub fn new(renderer: &Renderer) -> Self {
        let depth_format = renderer.depth_format();
        let color_format = renderer.color_target_format();
        renderer.with_raw_gpu(|device, _queue, _configuration| {
            Self::create(device, color_format, depth_format)
        })
    }

    /// Creates a lit renderer from a currently-recording frame.
    #[must_use]
    pub fn new_for_frame(frame: &RenderFrame<'_>) -> Self {
        Self::create(frame.device(), frame.surface_format(), frame.depth_format())
    }

    /// Uploads a mesh with mandatory finite vertex normals.
    ///
    /// # Errors
    ///
    /// Returns [`LitMeshUploadError::MissingNormals`] rather than generating
    /// normals implicitly. Importers and procedural mesh builders therefore
    /// remain responsible for their smoothing policy.
    pub fn upload_mesh(
        &self,
        renderer: &Renderer,
        primitive: &MeshPrimitive,
    ) -> Result<GpuLitMesh, LitMeshUploadError> {
        renderer.with_raw_gpu(|device, _queue, _configuration| Self::upload_with(device, primitive))
    }

    /// Uploads a lit mesh using the device attached to `frame`.
    ///
    /// # Errors
    ///
    /// Returns [`LitMeshUploadError`] under the same conditions as
    /// [`Self::upload_mesh`].
    pub fn upload_mesh_for_frame(
        &self,
        frame: &RenderFrame<'_>,
        primitive: &MeshPrimitive,
    ) -> Result<GpuLitMesh, LitMeshUploadError> {
        Self::upload_with(frame.device(), primitive)
    }

    /// Draws one lit opaque mesh and starts a fresh depth phase.
    ///
    /// # Errors
    ///
    /// Returns [`LitMeshRenderError`] for invalid camera/model/material data.
    pub fn draw(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        mesh: &GpuLitMesh,
        instance: LitMeshInstance3d,
    ) -> Result<MeshDrawStats, LitMeshRenderError> {
        self.draw_with_depth_load(frame, camera, mesh, instance, DepthLoad::Clear)
    }

    /// Draws one lit opaque mesh with explicit depth load behavior.
    ///
    /// Use [`DepthLoad::Clear`] for the first opaque draw of a phase and
    /// [`DepthLoad::Load`] afterwards. The depth format, comparison and writes
    /// match [`MeshRenderer3d`] and [`SceneRenderer3d`].
    ///
    /// # Errors
    ///
    /// Returns [`LitMeshRenderError`] for invalid camera/model/material data.
    pub fn draw_with_depth_load(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        mesh: &GpuLitMesh,
        instance: LitMeshInstance3d,
        depth_load: DepthLoad,
    ) -> Result<MeshDrawStats, LitMeshRenderError> {
        self.draw_with_depth_load_rasterization(frame, camera, mesh, instance, depth_load, false)
    }

    /// Records one Lambert draw while preserving standard material culling.
    ///
    /// The double-sided pipeline invokes the same shader, whose fragment
    /// stage flips back-facing normals before evaluating Lambert diffuse.
    fn draw_with_depth_load_rasterization(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        mesh: &GpuLitMesh,
        instance: LitMeshInstance3d,
        depth_load: DepthLoad,
        double_sided: bool,
    ) -> Result<MeshDrawStats, LitMeshRenderError> {
        let view_projection = camera
            .view_projection(frame.draw_size())
            .map_err(LitMeshRenderError::Mesh)?;
        let uniform =
            LitMeshUniform::new(instance.model_matrix, instance.material, instance.lighting)?;
        frame
            .queue()
            .write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&view_projection));
        frame
            .queue()
            .write_buffer(&self.draw_buffer, 0, bytemuck::bytes_of(&uniform));
        frame.with_surface_pass_with_depth(wgpu::LoadOp::Load, depth_load.operation(), |pass| {
            pass.set_pipeline(self.pipeline_for(instance.model_matrix, double_sided));
            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            pass.set_bind_group(1, &self.draw_bind_group, &[]);
            pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
            pass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..mesh.index_count, 0, 0..1);
        });
        Ok(MeshDrawStats {
            triangles: mesh.index_count / 3,
            draw_calls: 1,
            transient_uniform_buffer_allocations: 0,
        })
    }

    #[allow(clippy::too_many_lines)] // Pipeline descriptors must remain co-located for review.
    fn create(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        depth_format: wgpu::TextureFormat,
    ) -> Self {
        let camera_layout = uniform_layout(
            device,
            "yuyib lit mesh camera layout",
            wgpu::ShaderStages::VERTEX,
        );
        let draw_layout = uniform_layout(
            device,
            "yuyib lit mesh draw layout",
            wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
        );
        let camera_buffer = uniform_buffer(
            device,
            "yuyib lit mesh camera",
            size_of::<[f32; 16]>() as u64,
        );
        let draw_buffer = uniform_buffer(
            device,
            "yuyib lit mesh draw",
            size_of::<LitMeshUniform>() as u64,
        );
        let camera_bind_group = uniform_bind_group(
            device,
            "yuyib lit mesh camera bind group",
            &camera_layout,
            &camera_buffer,
        );
        let draw_bind_group = uniform_bind_group(
            device,
            "yuyib lit mesh draw bind group",
            &draw_layout,
            &draw_buffer,
        );
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("yuyib Lambert mesh WGSL"),
            source: wgpu::ShaderSource::Wgsl(LIT_MESH_WGSL.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("yuyib Lambert mesh pipeline layout"),
            bind_group_layouts: &[Some(&camera_layout), Some(&draw_layout)],
            immediate_size: 0,
        });
        let make_pipeline = |label, front_face, cull_mode| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[Some(LIT_VERTEX_LAYOUT)],
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    front_face,
                    cull_mode,
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
            })
        };
        Self {
            pipeline: make_pipeline(
                "yuyib Lambert mesh pipeline",
                wgpu::FrontFace::Ccw,
                Some(wgpu::Face::Back),
            ),
            double_sided_pipeline: make_pipeline(
                "yuyib Lambert mesh double-sided pipeline",
                wgpu::FrontFace::Ccw,
                None,
            ),
            mirrored_pipeline: make_pipeline(
                "yuyib Lambert mesh mirrored pipeline",
                wgpu::FrontFace::Cw,
                Some(wgpu::Face::Back),
            ),
            mirrored_double_sided_pipeline: make_pipeline(
                "yuyib Lambert mesh mirrored double-sided pipeline",
                wgpu::FrontFace::Cw,
                None,
            ),
            camera_buffer,
            draw_buffer,
            camera_bind_group,
            draw_bind_group,
        }
    }

    fn pipeline_for(&self, model_matrix: [f32; 16], double_sided: bool) -> &wgpu::RenderPipeline {
        match lambert_rasterization(model_matrix, double_sided) {
            LambertRasterization::Regular => &self.pipeline,
            LambertRasterization::DoubleSided => &self.double_sided_pipeline,
            LambertRasterization::Mirrored => &self.mirrored_pipeline,
            LambertRasterization::MirroredDoubleSided => &self.mirrored_double_sided_pipeline,
        }
    }

    fn upload_with(
        device: &wgpu::Device,
        primitive: &MeshPrimitive,
    ) -> Result<GpuLitMesh, LitMeshUploadError> {
        let normals = primitive
            .normals()
            .ok_or(LitMeshUploadError::MissingNormals)?;
        let index_count = u32::try_from(primitive.indices().len()).map_err(|_| {
            LitMeshUploadError::TooManyIndices {
                actual: primitive.indices().len(),
            }
        })?;
        let vertices: Vec<LitVertex> = primitive
            .positions()
            .iter()
            .copied()
            .zip(normals.iter().copied())
            .enumerate()
            .map(|(index, (position, normal))| {
                if !all_finite(&position) {
                    return Err(LitMeshUploadError::NonFinitePosition { index });
                }
                if !all_finite(&normal) {
                    return Err(LitMeshUploadError::NonFiniteNormal { index });
                }
                normalize3(normal).ok_or(LitMeshUploadError::DegenerateNormal { index })?;
                Ok(LitVertex { position, normal })
            })
            .collect::<Result<_, _>>()?;
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("yuyib 3d lit mesh vertices"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("yuyib 3d lit mesh indices"),
            contents: bytemuck::cast_slice(primitive.indices()),
            usage: wgpu::BufferUsages::INDEX,
        });
        Ok(GpuLitMesh {
            vertex_buffer,
            index_buffer,
            index_count,
        })
    }
}

/// Failure while uploading a lit mesh.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LitMeshUploadError {
    /// The mesh has no normal stream.
    MissingNormals,
    /// A position contained NaN or infinity.
    NonFinitePosition {
        /// Position stream index.
        index: usize,
    },
    /// A normal contained NaN or infinity.
    NonFiniteNormal {
        /// Normal stream index.
        index: usize,
    },
    /// A normal was too small to normalize.
    DegenerateNormal {
        /// Normal stream index.
        index: usize,
    },
    /// The index count exceeds WGPU's draw range.
    TooManyIndices {
        /// Observed count.
        actual: usize,
    },
}

/// Failure while drawing a lit mesh.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LitMeshRenderError {
    /// Camera or model data could not make a valid GPU transform.
    Mesh(MeshRenderError),
    /// The model matrix has a non-invertible 3×3 basis, so normals cannot be transformed correctly.
    NonInvertibleNormalMatrix,
    /// The linear RGBA base-colour multiplier contained non-finite or negative values.
    InvalidBaseColorFactor,
}

impl fmt::Display for LitMeshUploadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingNormals => formatter.write_str("lit mesh requires a normal stream"),
            Self::NonFinitePosition { index } => write!(
                formatter,
                "lit mesh position at index {index} is not finite"
            ),
            Self::NonFiniteNormal { index } => {
                write!(formatter, "lit mesh normal at index {index} is not finite")
            }
            Self::DegenerateNormal { index } => write!(
                formatter,
                "lit mesh normal at index {index} has zero length"
            ),
            Self::TooManyIndices { actual } => write!(
                formatter,
                "lit mesh has {actual} indices; WGPU accepts at most u32::MAX"
            ),
        }
    }
}
impl Error for LitMeshUploadError {}

impl fmt::Display for LitMeshRenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mesh(source) => write!(formatter, "cannot draw lit mesh: {source}"),
            Self::NonInvertibleNormalMatrix => {
                formatter.write_str("lit mesh model matrix has a non-invertible normal basis")
            }
            Self::InvalidBaseColorFactor => formatter
                .write_str("lit material base colour factor must be finite and non-negative"),
        }
    }
}
impl Error for LitMeshRenderError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Mesh(source) => Some(source),
            _ => None,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct LitVertex {
    position: [f32; 3],
    normal: [f32; 3],
}

pub(crate) const LIT_VERTEX_LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
    array_stride: size_of::<LitVertex>() as u64,
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
    ],
};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct LitMeshUniform {
    model: [f32; 16],
    normal_matrix: [f32; 12],
    base_color: [f32; 4],
    light_direction: [f32; 4],
    light_color: [f32; 4],
    ambient: [f32; 4],
}

impl LitMeshUniform {
    fn new(
        model: [f32; 16],
        material: LitMaterial3d,
        lighting: LambertLighting3d,
    ) -> Result<Self, LitMeshRenderError> {
        if !all_finite(&model) {
            return Err(LitMeshRenderError::Mesh(
                MeshRenderError::InvalidModelMatrix,
            ));
        }
        if !non_negative_finite(&material.base_color_factor) {
            return Err(LitMeshRenderError::InvalidBaseColorFactor);
        }
        let normal_matrix =
            inverse_transpose_3x3(model).ok_or(LitMeshRenderError::NonInvertibleNormalMatrix)?;
        Ok(Self {
            model,
            normal_matrix,
            base_color: material.base_color_factor,
            light_direction: [
                lighting.light.direction[0],
                lighting.light.direction[1],
                lighting.light.direction[2],
                0.0,
            ],
            light_color: [
                lighting.light.color[0] * lighting.light.illuminance_lux,
                lighting.light.color[1] * lighting.light.illuminance_lux,
                lighting.light.color[2] * lighting.light.illuminance_lux,
                0.0,
            ],
            ambient: [
                lighting.ambient[0],
                lighting.ambient[1],
                lighting.ambient[2],
                0.0,
            ],
        })
    }
}

const LIT_MESH_WGSL: &str = r"
struct Camera { view_projection: mat4x4<f32>, };
struct Draw {
    model: mat4x4<f32>,
    normal_matrix: mat3x3<f32>,
    base_color: vec4<f32>,
    light_direction: vec4<f32>,
    light_color: vec4<f32>,
    ambient: vec4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var<uniform> draw: Draw;
struct VertexInput { @location(0) position: vec3<f32>, @location(1) normal: vec3<f32>, };
struct VertexOutput { @builtin(position) clip_position: vec4<f32>, @location(0) normal: vec3<f32>, };
@vertex fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    output.clip_position = camera.view_projection * draw.model * vec4<f32>(input.position, 1.0);
    output.normal = normalize(draw.normal_matrix * input.normal);
    return output;
}
@fragment fn fs_main(
    input: VertexOutput,
    @builtin(front_facing) front_facing: bool,
) -> @location(0) vec4<f32> {
    let surface_normal = select(-normalize(input.normal), normalize(input.normal), front_facing);
    let diffuse = max(dot(surface_normal, normalize(-draw.light_direction.xyz)), 0.0);
    let lighting = draw.ambient.xyz + draw.light_color.xyz * diffuse;
    return vec4<f32>(draw.base_color.rgb * lighting, draw.base_color.a);
}
";

/// A deliberately small high-level material distilled from [`Material`].
///
/// It supports base-colour factor, optional base-colour texture and explicit
/// double-sided rasterization. It is not a PBR material: normal maps, emissive
/// maps and metallic/roughness maps are rejected at conversion. The strict
/// [`Self::from_model_material`] conversion also rejects non-default
/// metallic/roughness factors, because the general standard path would ignore
/// them silently.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StandardMaterial3d {
    base_color_factor: [f32; 4],
    base_color_texture: Option<ModelTextureIndex>,
    double_sided: bool,
}

impl StandardMaterial3d {
    /// Converts the supported subset of renderer-neutral model material metadata.
    ///
    /// # Errors
    ///
    /// Returns [`StandardMaterialError`] for material features this initial
    /// standard path cannot render or for a base texture using UV set other
    /// than zero.
    #[allow(clippy::float_cmp)] // Metadata must retain exact default-factor semantics; epsilon would hide authored PBR input.
    pub fn from_model_material(material: &Material) -> Result<Self, StandardMaterialError> {
        Self::from_model_material_inner(material, true)
    }

    /// Converts a material for the Lambert-lit scene route.
    ///
    /// The Lambert pass deliberately uses only base colour, base-colour texture
    /// and culling. Therefore it explicitly projects out authored metallic,
    /// roughness, normal and emissive channels; this is not PBR and does not
    /// pretend those channels affected the result. Specular-glossiness and
    /// alpha phases remain rejected because they change base-colour semantics.
    ///
    /// This is intentionally separate from [`Self::from_model_material`]: the
    /// latter remains strict for callers that expect every accepted field to
    /// participate in the general standard path.
    ///
    /// # Errors
    ///
    /// Returns [`StandardMaterialError`] for unsupported render phases,
    /// workflows or a base texture using UV set other than zero.
    pub fn from_model_material_for_lambert(
        material: &Material,
    ) -> Result<Self, StandardMaterialError> {
        Self::from_model_material_inner(material, false)
    }

    #[allow(clippy::float_cmp)] // The strict conversion must retain exact default-factor semantics.
    fn from_model_material_inner(
        material: &Material,
        reject_metallic_roughness_factors: bool,
    ) -> Result<Self, StandardMaterialError> {
        if material.alpha_mode() != AlphaMode::Opaque {
            return Err(StandardMaterialError::AlphaModeUnsupported);
        }
        if material.specular_glossiness().is_some() {
            return Err(StandardMaterialError::SpecularGlossinessUnsupported);
        }
        if reject_metallic_roughness_factors && material.normal_texture().is_some() {
            return Err(StandardMaterialError::NormalMapUnsupported);
        }
        if reject_metallic_roughness_factors && material.metallic_roughness_texture().is_some() {
            return Err(StandardMaterialError::MetallicRoughnessTextureUnsupported);
        }
        if reject_metallic_roughness_factors && material.emissive_texture().is_some() {
            return Err(StandardMaterialError::EmissiveTextureUnsupported);
        }
        if reject_metallic_roughness_factors && material.emissive_factor() != [0.0; 3] {
            return Err(StandardMaterialError::EmissiveFactorUnsupported);
        }
        if reject_metallic_roughness_factors
            && (material.metallic_factor() != 1.0 || material.roughness_factor() != 1.0)
        {
            return Err(StandardMaterialError::MetallicRoughnessFactorsUnsupported);
        }
        let base_color_texture = material.base_color_texture();
        if let Some(binding) = base_color_texture
            && binding.tex_coord_set() != 0
        {
            return Err(StandardMaterialError::BaseTextureUvSetUnsupported {
                actual: binding.tex_coord_set(),
            });
        }
        Ok(Self {
            base_color_factor: material.base_color_factor(),
            base_color_texture: base_color_texture.map(yuyib_model::TextureBinding::texture),
            double_sided: material.double_sided(),
        })
    }

    /// Returns the linear RGBA base-colour multiplier.
    #[must_use]
    pub const fn base_color_factor(self) -> [f32; 4] {
        self.base_color_factor
    }

    /// Returns the model-local base-colour texture slot, if any.
    #[must_use]
    pub const fn base_color_texture(self) -> Option<ModelTextureIndex> {
        self.base_color_texture
    }

    /// Returns whether this material must render both triangle faces.
    ///
    /// A `true` value selects a dedicated no-cull pipeline only for this draw;
    /// it does not modify the default rasterization of other materials.
    #[must_use]
    pub const fn double_sided(self) -> bool {
        self.double_sided
    }
}

/// Conversion failure for [`StandardMaterial3d`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StandardMaterialError {
    /// Alpha mask/blending requires an explicit transparent or masked pass.
    AlphaModeUnsupported,
    /// The specular-glossiness workflow needs a dedicated PBR pipeline.
    SpecularGlossinessUnsupported,
    /// Normal-map sampling needs tangents and a dedicated pipeline.
    NormalMapUnsupported,
    /// Metallic/roughness texture sampling is PBR work not implemented here.
    MetallicRoughnessTextureUnsupported,
    /// Emissive texture sampling is not implemented by this standard path.
    EmissiveTextureUnsupported,
    /// Non-zero emissive colour needs an emissive-capable material path.
    EmissiveFactorUnsupported,
    /// Non-default metallic/roughness factors would be silently ignored.
    MetallicRoughnessFactorsUnsupported,
    /// The base texture references a UV set other than the supported UV0.
    BaseTextureUvSetUnsupported {
        /// Requested source UV set.
        actual: u8,
    },
}

impl fmt::Display for StandardMaterialError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlphaModeUnsupported => {
                formatter.write_str("standard material does not select an alpha render phase")
            }
            Self::SpecularGlossinessUnsupported => formatter
                .write_str("standard material does not support the specular-glossiness workflow"),
            Self::NormalMapUnsupported => {
                formatter.write_str("standard material does not support normal maps")
            }
            Self::MetallicRoughnessTextureUnsupported => formatter
                .write_str("standard material does not support metallic-roughness textures"),
            Self::EmissiveTextureUnsupported => {
                formatter.write_str("standard material does not support emissive textures")
            }
            Self::EmissiveFactorUnsupported => {
                formatter.write_str("standard material does not support emissive factors")
            }
            Self::MetallicRoughnessFactorsUnsupported => formatter
                .write_str("standard material only accepts default metallic and roughness factors"),
            Self::BaseTextureUvSetUnsupported { actual } => write!(
                formatter,
                "standard material only supports base texture UV0, requested UV{actual}"
            ),
        }
    }
}
impl Error for StandardMaterialError {}

/// Resolved model texture bindings and their resident GPU cache.
#[derive(Clone, Copy)]
pub struct StandardTextureResources<'a> {
    /// Mapping from model-local texture slots to typed texture asset handles.
    pub bindings: &'a ModelTextureBindings,
    /// Device-local sampled texture cache.
    pub textures: &'a yuyib_render_texture::TextureCache,
}

impl<'a> StandardTextureResources<'a> {
    /// Resolves a model-local base texture to its GPU resource.
    ///
    /// # Errors
    ///
    /// Returns [`StandardRenderError`] when texture loading has not resolved
    /// the model slot or has not uploaded its typed texture asset.
    fn resolve(&self, index: ModelTextureIndex) -> Result<&'a GpuTexture, StandardRenderError> {
        let binding = self
            .bindings
            .get(index)
            .ok_or(StandardRenderError::MissingTextureBinding { index })?;
        self.textures
            .get(binding.handle())
            .ok_or(StandardRenderError::MissingGpuTexture { index })
    }
}

/// GPU representations of every source stream the standard path can select.
///
/// Optional source streams are uploaded only when present. A later draw that
/// selects a missing stream returns a structured error instead of uploading or
/// synthesizing attributes at frame time.
pub struct StandardMesh3d {
    solid: GpuMesh,
    textured: Option<GpuTexturedMesh>,
    lit: Option<GpuLitMesh>,
    textured_lit: Option<GpuTexturedLitMesh>,
}

/// Upload failure for [`StandardMesh3d`].
#[derive(Debug)]
pub enum StandardMeshUploadError {
    /// Solid position/index upload failed.
    Solid(MeshUploadError),
    /// A present UV stream could not be uploaded.
    Textured(TexturedMeshUploadError),
    /// A present normal stream could not be uploaded.
    Lit(LitMeshUploadError),
    /// Present normals and UV0 could not be uploaded for textured Lambert.
    TexturedLit(TexturedLitMeshUploadError),
}

impl fmt::Display for StandardMeshUploadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Solid(source) => write!(formatter, "cannot upload standard solid mesh: {source}"),
            Self::Textured(source) => write!(formatter, "cannot upload standard UV mesh: {source}"),
            Self::Lit(source) => write!(formatter, "cannot upload standard normal mesh: {source}"),
            Self::TexturedLit(source) => write!(
                formatter,
                "cannot upload standard textured Lambert mesh: {source}"
            ),
        }
    }
}
impl Error for StandardMeshUploadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Solid(source) => Some(source),
            Self::Textured(source) => Some(source),
            Self::Lit(source) => Some(source),
            Self::TexturedLit(source) => Some(source),
        }
    }
}

/// Per-draw input for [`StandardRenderer3d`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StandardDraw3d {
    /// Column-major model-to-world matrix.
    pub model_matrix: [f32; 16],
    /// Base-colour material subset.
    pub material: StandardMaterial3d,
    /// Optional Lambert lighting. `None` uses the unlit pipelines.
    pub lighting: Option<LambertLighting3d>,
}

impl StandardDraw3d {
    /// Creates one high-level standard draw.
    #[must_use]
    pub const fn new(
        model_matrix: [f32; 16],
        material: StandardMaterial3d,
        lighting: Option<LambertLighting3d>,
    ) -> Self {
        Self {
            model_matrix,
            material,
            lighting,
        }
    }
}

/// High-level material renderer selecting low-level unlit / Lambert / PBR slices.
///
/// Solid, textured-unlit, colour-Lambert and textured-Lambert paths are
/// selected automatically.  The combined path is still deliberately only
/// Lambert; callers do not accidentally get a pretend-PBR material. All
/// selected paths retain their low-level [`DepthLoad`] opaque semantics.
pub struct StandardRenderer3d {
    solid: MeshRenderer3d,
    textured: TexturedMeshRenderer3d,
    lit: LitMeshRenderer3d,
    textured_lit: TexturedLitMeshRenderer3d,
}

impl StandardRenderer3d {
    /// Creates standard renderer state for `renderer`.
    #[must_use]
    pub fn new(renderer: &Renderer) -> Self {
        Self {
            solid: MeshRenderer3d::new(renderer),
            textured: TexturedMeshRenderer3d::new(renderer),
            lit: LitMeshRenderer3d::new(renderer),
            textured_lit: TexturedLitMeshRenderer3d::new(renderer),
        }
    }

    /// Creates standard renderer state from a frame during lazy scene setup.
    #[must_use]
    pub fn new_for_frame(frame: &RenderFrame<'_>) -> Self {
        Self {
            solid: MeshRenderer3d::new_for_frame(frame),
            textured: TexturedMeshRenderer3d::new_for_frame(frame),
            lit: LitMeshRenderer3d::new_for_frame(frame),
            textured_lit: TexturedLitMeshRenderer3d::new_for_frame(frame),
        }
    }

    /// Uploads all source streams needed by future standard draw variants.
    ///
    /// # Errors
    ///
    /// Returns [`StandardMeshUploadError`] if a present source stream is
    /// invalid for its compatible low-level pipeline.
    pub fn upload_mesh(
        &self,
        renderer: &Renderer,
        primitive: &MeshPrimitive,
    ) -> Result<StandardMesh3d, StandardMeshUploadError> {
        let solid = self
            .solid
            .upload_mesh(renderer, primitive)
            .map_err(StandardMeshUploadError::Solid)?;
        let textured = primitive
            .tex_coords_0()
            .is_some()
            .then(|| self.textured.upload_mesh(renderer, primitive))
            .transpose()
            .map_err(StandardMeshUploadError::Textured)?;
        let lit = primitive
            .normals()
            .is_some()
            .then(|| self.lit.upload_mesh(renderer, primitive))
            .transpose()
            .map_err(StandardMeshUploadError::Lit)?;
        let textured_lit = (primitive.tex_coords_0().is_some() && primitive.normals().is_some())
            .then(|| self.textured_lit.upload_mesh(renderer, primitive))
            .transpose()
            .map_err(StandardMeshUploadError::TexturedLit)?;
        Ok(StandardMesh3d {
            solid,
            textured,
            lit,
            textured_lit,
        })
    }

    /// Uploads a standard mesh through the device attached to `frame`.
    ///
    /// # Errors
    ///
    /// Returns [`StandardMeshUploadError`] if a present stream is invalid for
    /// its selected GPU representation.
    pub fn upload_mesh_for_frame(
        &self,
        frame: &RenderFrame<'_>,
        primitive: &MeshPrimitive,
    ) -> Result<StandardMesh3d, StandardMeshUploadError> {
        let solid = self
            .solid
            .upload_mesh_for_frame(frame, primitive)
            .map_err(StandardMeshUploadError::Solid)?;
        let textured = primitive
            .tex_coords_0()
            .is_some()
            .then(|| self.textured.upload_mesh_for_frame(frame, primitive))
            .transpose()
            .map_err(StandardMeshUploadError::Textured)?;
        let lit = primitive
            .normals()
            .is_some()
            .then(|| self.lit.upload_mesh_for_frame(frame, primitive))
            .transpose()
            .map_err(StandardMeshUploadError::Lit)?;
        let textured_lit = (primitive.tex_coords_0().is_some() && primitive.normals().is_some())
            .then(|| self.textured_lit.upload_mesh_for_frame(frame, primitive))
            .transpose()
            .map_err(StandardMeshUploadError::TexturedLit)?;
        Ok(StandardMesh3d {
            solid,
            textured,
            lit,
            textured_lit,
        })
    }

    /// Draws a standard material with a fresh opaque depth phase.
    ///
    /// # Errors
    ///
    /// Returns [`StandardRenderError`] for missing resources/streams,
    /// missing resources/streams or an underlying low-level draw failure.
    pub fn draw(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        mesh: &StandardMesh3d,
        draw: StandardDraw3d,
        textures: StandardTextureResources<'_>,
    ) -> Result<MeshDrawStats, StandardRenderError> {
        self.draw_with_depth_load(frame, camera, mesh, draw, textures, DepthLoad::Clear)
    }

    /// Draws with explicit opaque depth phase behavior.
    ///
    /// # Errors
    ///
    /// Returns [`StandardRenderError`] under the same conditions as [`Self::draw`].
    #[allow(clippy::too_many_arguments)] // Depth is intentionally explicit at this phase boundary.
    pub fn draw_with_depth_load(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        mesh: &StandardMesh3d,
        draw: StandardDraw3d,
        textures: StandardTextureResources<'_>,
        depth_load: DepthLoad,
    ) -> Result<MeshDrawStats, StandardRenderError> {
        match (draw.material.base_color_texture, draw.lighting) {
            (None, None) => self
                .solid
                .draw_with_model_matrix_depth_load_rasterization(
                    frame,
                    camera,
                    &mesh.solid,
                    draw.model_matrix,
                    draw.material.base_color_factor,
                    depth_load,
                    draw.material.double_sided,
                )
                .map_err(StandardRenderError::Solid),
            (Some(index), None) => {
                let texture = textures.resolve(index)?;
                let mesh = mesh
                    .textured
                    .as_ref()
                    .ok_or(StandardRenderError::MissingTexCoords0)?;
                self.textured
                    .draw_with_depth_load_rasterization(
                        frame,
                        camera,
                        mesh,
                        draw.model_matrix,
                        TexturedMaterial3d::new(texture, draw.material.base_color_factor),
                        depth_load,
                        draw.material.double_sided,
                    )
                    .map_err(StandardRenderError::Textured)
            }
            (None, Some(lighting)) => {
                let mesh = mesh
                    .lit
                    .as_ref()
                    .ok_or(StandardRenderError::MissingNormals)?;
                self.lit
                    .draw_with_depth_load_rasterization(
                        frame,
                        camera,
                        mesh,
                        LitMeshInstance3d::new(
                            draw.model_matrix,
                            LitMaterial3d::new(draw.material.base_color_factor),
                            lighting,
                        ),
                        depth_load,
                        draw.material.double_sided,
                    )
                    .map_err(StandardRenderError::Lit)
            }
            (Some(index), Some(lighting)) => {
                let texture = textures.resolve(index)?;
                let mesh = mesh.textured_lit.as_ref().ok_or_else(|| {
                    if mesh.textured.is_none() {
                        StandardRenderError::MissingTexCoords0
                    } else {
                        StandardRenderError::MissingNormals
                    }
                })?;
                self.textured_lit
                    .draw_with_depth_load_rasterization(
                        frame,
                        camera,
                        mesh,
                        LitMeshInstance3d::new(
                            draw.model_matrix,
                            LitMaterial3d::new(draw.material.base_color_factor),
                            lighting,
                        ),
                        TexturedLitMaterial3d::new(texture, draw.material.base_color_factor),
                        depth_load,
                        draw.material.double_sided,
                    )
                    .map_err(StandardRenderError::TexturedLit)
            }
        }
    }
}

/// High-level standard render failure.
#[derive(Debug)]
pub enum StandardRenderError {
    /// The model's texture resolver has no binding for this model-local slot.
    MissingTextureBinding {
        /// Model-local texture slot.
        index: ModelTextureIndex,
    },
    /// A resolved texture binding has no resident GPU resource.
    MissingGpuTexture {
        /// Model-local texture slot.
        index: ModelTextureIndex,
    },
    /// A base texture needs UV0 but the source mesh had none.
    MissingTexCoords0,
    /// Lambert lighting needs normals but the source mesh had none.
    MissingNormals,
    /// Solid low-level draw failed.
    Solid(MeshRenderError),
    /// Textured low-level draw failed.
    Textured(TexturedMeshRenderError),
    /// Lit low-level draw failed.
    Lit(LitMeshRenderError),
    /// Textured Lambert low-level draw failed.
    TexturedLit(TexturedLitMeshRenderError),
}

impl fmt::Display for StandardRenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingTextureBinding { index } => write!(
                formatter,
                "model texture slot {} has no resolved binding",
                index.get()
            ),
            Self::MissingGpuTexture { index } => write!(
                formatter,
                "model texture slot {} is not resident on the GPU",
                index.get()
            ),
            Self::MissingTexCoords0 => formatter.write_str("standard textured draw requires UV0"),
            Self::MissingNormals => formatter.write_str("standard Lambert draw requires normals"),
            Self::Solid(source) => write!(formatter, "standard solid draw failed: {source}"),
            Self::Textured(source) => write!(formatter, "standard textured draw failed: {source}"),
            Self::Lit(source) => write!(formatter, "standard Lambert draw failed: {source}"),
            Self::TexturedLit(source) => {
                write!(formatter, "standard textured Lambert draw failed: {source}")
            }
        }
    }
}
impl Error for StandardRenderError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Solid(source) => Some(source),
            Self::Textured(source) => Some(source),
            Self::Lit(source) => Some(source),
            Self::TexturedLit(source) => Some(source),
            _ => None,
        }
    }
}

/// Produces the inverse-transpose of a column-major model matrix's 3×3 basis.
///
/// WGPU uniform matrices store each `vec3` column at a 16-byte stride, hence
/// the three padded columns in the returned value.
#[allow(clippy::many_single_char_names)] // Conventional cofactor notation keeps the matrix derivation auditable.
fn inverse_transpose_3x3(model: [f32; 16]) -> Option<[f32; 12]> {
    let basis = [
        model[0], model[4], model[8], model[1], model[5], model[9], model[2], model[6], model[10],
    ];
    let basis_scale = basis.iter().copied().map(f32::abs).fold(0.0_f32, f32::max);
    if !basis_scale.is_finite() || basis_scale == 0.0 {
        return None;
    }

    // Test invertibility after factoring out the basis' absolute scale. An
    // absolute determinant epsilon rejects perfectly valid unit conversions:
    // a uniform 0.001 scale has determinant 1e-9 even though its condition
    // number is one. The normalized determinant instead measures the shape of
    // the basis and still rejects genuinely collapsed or ill-conditioned axes.
    let [a, b, c, d, e, f, g, h, i] = basis.map(|value| value / basis_scale);
    let c00 = e * i - f * h;
    let c01 = f * g - d * i;
    let c02 = d * h - e * g;
    let c10 = c * h - b * i;
    let c11 = a * i - c * g;
    let c12 = b * g - a * h;
    let c20 = b * f - c * e;
    let c21 = c * d - a * f;
    let c22 = a * e - b * d;
    let normalized_determinant = a * c00 + b * c01 + c * c02;
    if !normalized_determinant.is_finite() || normalized_determinant.abs() <= f32::EPSILON {
        return None;
    }
    let reciprocal = normalized_determinant.recip() / basis_scale;
    let matrix = [
        c00 * reciprocal,
        c10 * reciprocal,
        c20 * reciprocal,
        0.0,
        c01 * reciprocal,
        c11 * reciprocal,
        c21 * reciprocal,
        0.0,
        c02 * reciprocal,
        c12 * reciprocal,
        c22 * reciprocal,
        0.0,
    ];
    all_finite(&matrix).then_some(matrix)
}

fn non_negative_finite<const N: usize>(values: &[f32; N]) -> bool {
    values
        .iter()
        .all(|value| value.is_finite() && *value >= 0.0)
}

fn all_finite<const N: usize>(values: &[f32; N]) -> bool {
    values.iter().all(|value| value.is_finite())
}

fn validate_skin_palette(
    matrices: &[[f32; 16]],
    required_joint_count: u32,
) -> Result<(), SkinnedMeshRenderError> {
    if matrices.is_empty() {
        return Err(SkinnedMeshRenderError::EmptyPalette);
    }
    if matrices.len() > MAX_SKIN_JOINTS {
        return Err(SkinnedMeshRenderError::PaletteLimitExceeded {
            actual: matrices.len(),
            maximum: MAX_SKIN_JOINTS,
        });
    }
    if matrices.len() < required_joint_count as usize {
        return Err(SkinnedMeshRenderError::PaletteTooShort {
            available: matrices.len(),
            required: required_joint_count,
        });
    }
    if let Some((index, _)) = matrices
        .iter()
        .enumerate()
        .find(|(_, matrix)| !all_finite(matrix))
    {
        return Err(SkinnedMeshRenderError::NonFinitePaletteMatrix { index });
    }
    Ok(())
}

fn sub3(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
}

fn mul3(value: [f32; 3], scalar: f32) -> [f32; 3] {
    [value[0] * scalar, value[1] * scalar, value[2] * scalar]
}

fn dot3(left: [f32; 3], right: [f32; 3]) -> f32 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

fn cross3(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [
        left[1] * right[2] - left[2] * right[1],
        left[2] * right[0] - left[0] * right[2],
        left[0] * right[1] - left[1] * right[0],
    ]
}

fn normalize3(value: [f32; 3]) -> Option<[f32; 3]> {
    let length_squared = dot3(value, value);
    if !length_squared.is_finite() || length_squared <= f32::EPSILON {
        return None;
    }
    let reciprocal = length_squared.sqrt().recip();
    let normalized = [
        value[0] * reciprocal,
        value[1] * reciprocal,
        value[2] * reciprocal,
    ];
    all_finite(&normalized).then_some(normalized)
}

fn multiply_matrix4(left: [f32; 16], right: [f32; 16]) -> [f32; 16] {
    let mut result = [0.0; 16];
    for column in 0..4 {
        for row in 0..4 {
            result[column * 4 + row] = (0..4)
                .map(|inner| left[inner * 4 + row] * right[column * 4 + inner])
                .sum();
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skeletal_visibility_distinguishes_nodes_and_primitives() {
        let body_node = NodeIndex::new(2);
        let other_instance = NodeIndex::new(3);
        let head = SkeletalPrimitive3d::new(body_node, 7, 1);
        let torso = SkeletalPrimitive3d::new(body_node, 7, 0);
        let instanced_head = SkeletalPrimitive3d::new(other_instance, 7, 1);
        let mut visibility = SkeletalVisibilityMask3d::new();

        assert!(visibility.is_visible(head));
        assert!(visibility.hide_primitive(head));
        assert!(!visibility.hide_primitive(head));
        assert!(!visibility.is_visible(head));
        assert!(visibility.is_visible(torso));
        assert!(visibility.is_visible(instanced_head));

        assert!(visibility.hide_node(body_node));
        assert!(!visibility.is_visible(torso));
        assert!(visibility.is_visible(instanced_head));
        assert!(visibility.show_node(body_node));
        assert!(visibility.is_visible(torso));
        assert!(!visibility.is_visible(head));
        assert!(visibility.show_primitive(head));
        assert!(visibility.is_visible(head));
    }

    #[test]
    fn skeletal_visibility_show_all_retains_setup_allocations() {
        let mut visibility = SkeletalVisibilityMask3d::new();
        visibility.hide_node(NodeIndex::new(1));
        visibility.hide_primitive(SkeletalPrimitive3d::new(NodeIndex::new(2), 3, 4));
        let node_capacity = visibility.hidden_nodes.capacity();
        let primitive_capacity = visibility.hidden_primitives.capacity();

        visibility.show_all();

        assert!(visibility.hidden_nodes.is_empty());
        assert!(visibility.hidden_primitives.is_empty());
        assert_eq!(visibility.hidden_nodes.capacity(), node_capacity);
        assert_eq!(visibility.hidden_primitives.capacity(), primitive_capacity);
    }

    #[test]
    fn textured_lit_batch_uniform_stride_obeys_device_alignment() {
        let uniform_size =
            u64::try_from(size_of::<LitMeshUniform>()).expect("uniform size fits u64");
        let stride = aligned_uniform_stride(256, uniform_size);
        assert!(stride >= uniform_size);
        assert_eq!(stride % 256, 0);
    }

    #[test]
    fn scene_draw_stats_expose_zero_allocation_steady_state() {
        let stats = SceneDrawStats::default();
        assert_eq!(stats.render_passes, 0);
        assert_eq!(stats.material_bind_group_creations, 0);
        assert_eq!(stats.transient_uniform_buffer_allocations, 0);
        assert!(stats.summary_line().contains("transient_ubo=0"));
    }

    #[test]
    fn streamed_model_budget_has_bounded_balanced_defaults() {
        let budget = ModelUploadBudget3d::default();
        assert_eq!(budget.maximum_texture_slots, 4);
        assert_eq!(budget.target_texture_bytes, 16 * 1024 * 1024);
        assert_eq!(budget.maximum_primitives, 8);
        assert_eq!(budget.target_geometry_bytes, 8 * 1024 * 1024);
    }

    #[test]
    fn geometry_budget_counts_every_source_stream() {
        let cube = MeshPrimitive::cube(0.5).expect("built-in cube is valid");
        assert_eq!(primitive_source_geometry_bytes(&cube), 912);
    }

    #[test]
    #[allow(clippy::float_cmp)] // Source colour factors are preserved exactly.
    fn character_surface_uses_metallic_roughness_base_colour_inputs() {
        let material = Material::new()
            .with_base_color_factor([0.2, 0.4, 0.6, 0.8])
            .with_base_color_texture(TextureBinding::new(ModelTextureIndex::new(3), 0));

        let surface = character_base_color(Some(&material));
        assert_eq!(surface.factor, [0.2, 0.4, 0.6, 0.8]);
        assert_eq!(
            surface.texture,
            Some(TextureBinding::new(ModelTextureIndex::new(3), 0))
        );
    }

    #[test]
    #[allow(clippy::float_cmp)] // Restored albedo keeps authored alpha exactly.
    fn character_surface_restores_near_black_specular_glossiness_diffuse_rgb() {
        let material = Material::new().with_specular_glossiness(
            yuyib_model::SpecularGlossinessMaterial::new(
                [0.0, 0.0, 0.0, 0.6],
                [1.0; 3],
                1.0,
            )
            .with_diffuse_texture(TextureBinding::new(ModelTextureIndex::new(5), 0)),
        );

        let surface = character_base_color(Some(&material));
        assert_eq!(surface.factor, [1.0, 1.0, 1.0, 0.6]);
        assert_eq!(
            surface.texture,
            Some(TextureBinding::new(ModelTextureIndex::new(5), 0))
        );
    }

    #[test]
    #[allow(clippy::float_cmp)] // Source colour factors are preserved exactly.
    fn character_surface_uses_specular_glossiness_diffuse_inputs() {
        let material = Material::new()
            .with_base_color_factor([1.0; 4])
            .with_specular_glossiness(
                yuyib_model::SpecularGlossinessMaterial::new([0.3, 0.5, 0.7, 0.9], [1.0; 3], 1.0)
                    .with_diffuse_texture(TextureBinding::new(ModelTextureIndex::new(4), 0)),
            );

        let surface = character_base_color(Some(&material));
        assert_eq!(surface.factor, [0.3, 0.5, 0.7, 0.9]);
        assert_eq!(
            surface.texture,
            Some(TextureBinding::new(ModelTextureIndex::new(4), 0))
        );
    }

    #[test]
    fn skin_palette_validation_accepts_a_finite_palette_long_enough_for_mesh() {
        let matrix = [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ];
        assert_eq!(validate_skin_palette(&[matrix; 2], 2), Ok(()));
    }

    #[test]
    fn skin_palette_validation_accepts_the_documented_joint_limit() {
        let matrix = [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ];
        let matrices = vec![matrix; MAX_SKIN_JOINTS];
        assert_eq!(
            validate_skin_palette(
                &matrices,
                u32::try_from(MAX_SKIN_JOINTS)
                    .expect("documented palette limit fits a joint index"),
            ),
            Ok(())
        );
    }

    #[test]
    fn skin_palette_validation_refuses_more_than_the_documented_joint_limit() {
        let matrix = [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ];
        let matrices = vec![matrix; MAX_SKIN_JOINTS + 1];
        assert_eq!(
            validate_skin_palette(
                &matrices,
                u32::try_from(MAX_SKIN_JOINTS)
                    .expect("documented palette limit fits a joint index"),
            ),
            Err(SkinnedMeshRenderError::PaletteLimitExceeded {
                actual: MAX_SKIN_JOINTS + 1,
                maximum: MAX_SKIN_JOINTS,
            })
        );
    }

    #[test]
    fn skin_palette_validation_refuses_missing_referenced_joint() {
        let matrix = [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ];
        assert_eq!(
            validate_skin_palette(&[matrix], 2),
            Err(SkinnedMeshRenderError::PaletteTooShort {
                available: 1,
                required: 2,
            })
        );
    }

    #[test]
    fn skin_palette_validation_refuses_non_finite_matrices() {
        let mut matrix = [0.0; 16];
        matrix[5] = f32::NAN;
        assert_eq!(
            validate_skin_palette(&[matrix], 1),
            Err(SkinnedMeshRenderError::NonFinitePaletteMatrix { index: 0 })
        );
    }

    #[test]
    #[allow(clippy::float_cmp)] // Imported material multipliers must remain bit-exact.
    fn base_color_scene_material_keeps_base_texture_without_claiming_pbr() {
        let material = Material::new()
            .with_base_color_factor([0.2, 0.4, 0.6, 1.0])
            .with_metallic_roughness(0.0, 0.35)
            .with_double_sided(true)
            .with_base_color_texture(yuyib_model::TextureBinding::new(
                ModelTextureIndex::new(3),
                0,
            ));

        let resolved = base_color_material(&material).expect("UV0 is supported");
        assert_eq!(resolved.color, [0.2, 0.4, 0.6, 1.0]);
        assert_eq!(resolved.texture, Some(ModelTextureIndex::new(3)));
        assert!(resolved.double_sided);
    }

    #[test]
    fn base_color_scene_material_marks_blend_for_the_sorted_phase() {
        let material = Material::new().with_alpha_mode(AlphaMode::Blend);
        assert!(
            base_color_material(&material)
                .expect("blend has no extra UV requirement")
                .transparent
        );
    }

    #[test]
    #[allow(clippy::float_cmp)] // The fixture uses exactly representable integer components.
    fn transparent_sort_center_tracks_primitive_geometry_and_model_transform() {
        let primitive = MeshPrimitive::new(
            vec![[1.0, 2.0, 3.0], [3.0, 2.0, 1.0], [2.0, 5.0, 2.0]],
            vec![0, 1, 2],
        )
        .expect("triangle is valid");
        let center = primitive_local_center(&primitive);
        assert_eq!(center, [2.0, 3.0, 2.0]);
        assert_eq!(
            transform_point(
                [
                    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 4.0, 0.0, -1.0,
                    1.0,
                ],
                center,
            ),
            [6.0, 3.0, 1.0]
        );
    }

    #[test]
    fn base_color_scene_material_rejects_second_uv_set() {
        let material = Material::new().with_base_color_texture(yuyib_model::TextureBinding::new(
            ModelTextureIndex::new(0),
            1,
        ));
        assert_eq!(base_color_material(&material), Err(1));
    }

    #[test]
    fn camera_rejects_parallel_up_vector() {
        let camera = Camera3d::new(
            [0.0, 0.0, 1.0],
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            1.0,
            0.1,
            10.0,
        );
        assert!(matches!(
            camera.view_projection([1280, 720]),
            Err(MeshRenderError::InvalidCamera(_))
        ));
    }

    #[test]
    fn camera_projection_is_finite() {
        let matrix = Camera3d::default()
            .view_projection([1280, 720])
            .expect("default camera is valid");
        assert!(all_finite(&matrix));
        assert!(matrix[0] > 0.0);
        assert!(matrix[5] > 0.0);
    }

    #[test]
    fn transform_keeps_translation_in_last_column() {
        let matrix = MeshTransform3d::new([2.0, -3.0, 4.0], [0.0; 3], [1.0; 3])
            .matrix()
            .expect("finite transform");
        assert_eq!(&matrix[12..16], &[2.0, -3.0, 4.0, 1.0]);
    }

    #[test]
    fn transform_rejects_zero_scale() {
        assert!(matches!(
            MeshTransform3d::new([0.0; 3], [0.0; 3], [1.0, 0.0, 1.0]).matrix(),
            Err(MeshRenderError::InvalidTransform(_))
        ));
    }

    #[test]
    fn depth_load_selects_clear_or_preserve_operation() {
        assert!(matches!(
            DepthLoad::Clear.operation(),
            wgpu::LoadOp::Clear(value) if (value - 1.0).abs() < f32::EPSILON
        ));
        assert!(matches!(DepthLoad::Load.operation(), wgpu::LoadOp::Load));
    }

    #[test]
    #[allow(clippy::float_cmp)] // This const accessor must preserve supplied IEEE-754 values exactly.
    fn unresolved_textured_material_stays_explicit() {
        let material = TexturedMaterial3d::unbound([0.25, 0.5, 0.75, 1.0]);
        assert!(material.texture().is_none());
        assert_eq!(material.base_color_factor(), [0.25, 0.5, 0.75, 1.0]);
        assert_eq!(
            TexturedMeshRenderError::MissingTexture.to_string(),
            "textured material has no resolved GPU texture"
        );
    }

    #[test]
    #[allow(clippy::float_cmp)] // The constructor normalization has exact simple-component results.
    fn lambert_lighting_normalizes_the_ecs_light_draw() {
        let lighting = LambertLighting3d::new(
            DirectionalLightDraw {
                direction: [0.0, -2.0, 0.0],
                color: [0.5, 1.0, 0.25],
                illuminance_lux: 2.0,
            },
            [0.1; 3],
        )
        .expect("valid renderer-neutral light");
        assert_eq!(lighting.light().direction, [0.0, -1.0, 0.0]);
        assert_eq!(lighting.light().illuminance_lux, 2.0);
    }

    #[test]
    #[allow(clippy::float_cmp)] // The high-level helper forwards exact scalar values to the validated renderer state.
    fn artistic_lambert_lighting_keeps_the_simple_display_controls() {
        let lighting = LambertLighting3d::artistic(
            [0.0, -3.0, 0.0],
            [0.4, 1.0, 0.6],
            0.55,
            [0.15, 0.19, 0.16],
        )
        .expect("artistic controls are valid renderer values");

        assert_eq!(lighting.light().direction, [0.0, -1.0, 0.0]);
        assert_eq!(lighting.light().illuminance_lux, 0.55);
        assert_eq!(lighting.ambient(), [0.15, 0.19, 0.16]);
    }

    #[test]
    fn lambert_lighting_rejects_a_degenerate_extracted_light() {
        let result = LambertLighting3d::new(
            DirectionalLightDraw {
                direction: [0.0; 3],
                color: [1.0; 3],
                illuminance_lux: 1.0,
            },
            [0.0; 3],
        );
        assert!(matches!(
            result,
            Err(LambertLightingError::InvalidDirection)
        ));
    }

    #[test]
    fn normal_matrix_uses_inverse_transpose_for_non_uniform_scale() {
        let normal_matrix = inverse_transpose_3x3([
            2.0, 0.0, 0.0, 0.0, 0.0, 3.0, 0.0, 0.0, 0.0, 0.0, 4.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ])
        .expect("non-zero scale is invertible");
        assert!((normal_matrix[0] - 0.5).abs() < f32::EPSILON);
        assert!((normal_matrix[5] - 1.0 / 3.0).abs() < f32::EPSILON);
        assert!((normal_matrix[10] - 0.25).abs() < f32::EPSILON);
    }

    #[test]
    fn normal_matrix_accepts_small_uniform_scene_unit_conversion() {
        // Exact basis authored by cyberpunk_city.glb. Its determinant is about
        // 2.19e-10 solely because the scene converts from large source units;
        // the normalized basis is a well-conditioned rotation.
        let normal_matrix = inverse_transpose_3x3([
            0.000_602_337_94,
            0.0,
            0.0,
            0.0,
            0.0,
            -1.824_759_9e-10,
            -0.000_602_337_8,
            0.0,
            0.0,
            0.000_602_337_8,
            -1.824_759_9e-10,
            0.0,
            0.0,
            0.0,
            0.0,
            1.0,
        ])
        .expect("a small but well-conditioned unit conversion is invertible");

        assert!(normal_matrix.iter().all(|value| value.is_finite()));
        assert!((normal_matrix[0] - 1_660.197).abs() < 0.01);
    }

    #[test]
    fn normal_matrix_still_rejects_a_collapsed_axis_at_small_scale() {
        assert!(
            inverse_transpose_3x3([
                0.000_6, 0.0, 0.0, 0.0, 0.0, 0.000_6, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
                1.0,
            ])
            .is_none()
        );
    }

    #[test]
    fn mirrored_model_matrix_reverses_triangle_winding() {
        let identity = [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ];
        let mut one_reflection = identity;
        one_reflection[0] = -1.0;
        let mut two_reflections = one_reflection;
        two_reflections[5] = -1.0;

        assert!(!model_matrix_reverses_winding(identity));
        assert!(model_matrix_reverses_winding(one_reflection));
        assert!(!model_matrix_reverses_winding(two_reflections));
    }

    #[test]
    fn lambert_rasterization_preserves_mirrored_and_double_sided_parity() {
        let identity = [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ];
        let mut mirrored = identity;
        mirrored[0] = -1.0;

        assert_eq!(
            lambert_rasterization(identity, false),
            LambertRasterization::Regular
        );
        assert_eq!(
            lambert_rasterization(identity, true),
            LambertRasterization::DoubleSided
        );
        assert_eq!(
            lambert_rasterization(mirrored, false),
            LambertRasterization::Mirrored
        );
        assert_eq!(
            lambert_rasterization(mirrored, true),
            LambertRasterization::MirroredDoubleSided
        );
    }

    #[test]
    fn standard_material_accepts_the_model_default_subset() {
        let material = StandardMaterial3d::from_model_material(&Material::default())
            .expect("default model material uses only the supported subset");
        assert!(material.base_color_texture().is_none());
        assert!(!material.double_sided());
    }

    #[test]
    fn standard_material_preserves_per_material_double_sided_rasterization() {
        let material =
            StandardMaterial3d::from_model_material(&Material::new().with_double_sided(true))
                .expect("double-sided is supported by the standard rasterization variants");
        assert!(material.double_sided());
    }

    #[test]
    fn standard_material_refuses_to_draw_blend_as_opaque() {
        assert_eq!(
            StandardMaterial3d::from_model_material(
                &Material::new().with_alpha_mode(AlphaMode::Blend)
            ),
            Err(StandardMaterialError::AlphaModeUnsupported)
        );
    }

    #[test]
    #[allow(clippy::float_cmp)] // Authored factors are intentionally retained only in source metadata.
    fn lambert_material_explicitly_ignores_metallic_roughness_factors() {
        let material = Material::new()
            .with_base_color_factor([0.2, 0.4, 0.6, 1.0])
            .with_metallic_roughness(0.0, 0.35)
            .with_normal_texture(yuyib_model::NormalTextureBinding::new(
                TextureBinding::new(ModelTextureIndex::new(1), 2),
                0.8,
            ))
            .with_metallic_roughness_texture(TextureBinding::new(ModelTextureIndex::new(2), 1))
            .with_emissive_factor([1.0, 0.2, 0.8])
            .with_emissive_texture(TextureBinding::new(ModelTextureIndex::new(3), 3));

        assert_eq!(
            StandardMaterial3d::from_model_material(&material),
            Err(StandardMaterialError::NormalMapUnsupported)
        );
        let lambert = StandardMaterial3d::from_model_material_for_lambert(&material)
            .expect("Lambert deliberately projects the material to base colour");
        assert_eq!(lambert.base_color_factor(), [0.2, 0.4, 0.6, 1.0]);
    }

    #[test]
    fn lambert_material_keeps_alpha_and_spec_gloss_errors_honest() {
        assert_eq!(
            StandardMaterial3d::from_model_material_for_lambert(
                &Material::new().with_alpha_mode(AlphaMode::Blend)
            ),
            Err(StandardMaterialError::AlphaModeUnsupported)
        );
        let spec_gloss =
            Material::new().with_specular_glossiness(yuyib_model::SpecularGlossinessMaterial::new(
                [1.0, 1.0, 1.0, 1.0],
                [1.0, 1.0, 1.0],
                1.0,
            ));
        assert_eq!(
            StandardMaterial3d::from_model_material_for_lambert(&spec_gloss),
            Err(StandardMaterialError::SpecularGlossinessUnsupported)
        );
    }

    #[test]
    fn standard_material_rejects_non_zero_base_texture_uv_set() {
        let material = Material::new().with_base_color_texture(yuyib_model::TextureBinding::new(
            ModelTextureIndex::new(0),
            1,
        ));
        assert!(matches!(
            StandardMaterial3d::from_model_material(&material),
            Err(StandardMaterialError::BaseTextureUvSetUnsupported { actual: 1 })
        ));
    }

    #[test]
    fn standard_material_rejects_specular_glossiness_workflow() {
        let material =
            Material::new().with_specular_glossiness(yuyib_model::SpecularGlossinessMaterial::new(
                [1.0, 1.0, 1.0, 1.0],
                [1.0, 1.0, 1.0],
                1.0,
            ));
        assert_eq!(
            StandardMaterial3d::from_model_material(&material),
            Err(StandardMaterialError::SpecularGlossinessUnsupported)
        );
    }

    #[test]
    fn textured_skinned_fragment_lighting_is_flat_not_ndotl() {
        // Locks the sci-fi-girl yaw-brightness regression: orbiting a fixed pose
        // must not change whole-avatar exposure via directional N·L.
        assert!(
            TEXTURED_SKINNED_MESH_WGSL.contains("instance.ambient + instance.light_radiance"),
            "skinned fragment must use flat ambient+radiance exposure"
        );
        let fragment = TEXTURED_SKINNED_MESH_WGSL
            .split("@fragment")
            .nth(1)
            .expect("fragment stage");
        assert!(
            !fragment.contains("dot("),
            "skinned fragment must not sample N·L (found dot in fragment):\n{fragment}"
        );
    }

    #[test]
    fn skinned_material_lighting_with_zero_illuminance_is_pure_ambient() {
        let lighting = LambertLighting3d::artistic(
            [-0.35, -1.0, -0.25],
            [1.0, 0.97, 0.92],
            0.0,
            [0.92, 0.92, 0.94],
        )
        .expect("valid key");
        assert_eq!(lighting.light().illuminance_lux, 0.0);
        let exposure = [
            lighting.ambient()[0]
                + lighting.light().color[0] * lighting.light().illuminance_lux,
            lighting.ambient()[1]
                + lighting.light().color[1] * lighting.light().illuminance_lux,
            lighting.ambient()[2]
                + lighting.light().color[2] * lighting.light().illuminance_lux,
        ];
        assert_eq!(exposure, lighting.ambient());
        assert_eq!(exposure, [0.92, 0.92, 0.94]);
    }
}
