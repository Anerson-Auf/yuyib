//! Direct-light metallic/roughness PBR material preset.

use std::{cell::Cell, error::Error, fmt, mem::size_of, sync::Arc};

use bytemuck::{Pod, Zeroable};
use yuyib_model::{AlphaMode, MeshPrimitive};
use yuyib_render::{RenderFrame, Renderer, wgpu};
use yuyib_render_texture::GpuTexture;

use wgpu::util::DeviceExt;

use crate::{
    Camera3d, DepthLoad, GpuLitMesh, LambertLighting3d, LitMeshRenderer3d, LitMeshUploadError,
    MeshDrawStats, MeshRenderError, aligned_uniform_stride, all_finite, dynamic_uniform_bind_group,
    dynamic_uniform_layout, inverse_transpose_3x3, non_negative_finite, normalize3,
    uniform_bind_group, uniform_buffer, uniform_layout,
};

/// L2 real spherical-harmonics coefficients for diffuse PBR lighting.
///
/// Coefficients use the common real SH basis in this order:
/// `Y00`, `Y1-1` (y), `Y10` (z), `Y11` (x), `Y2-2` (xy), `Y2-1` (yz),
/// `Y20`, `Y21` (xz), `Y22`. They encode the *Lambert-normalized* diffuse
/// irradiance (`E / π`), so the shader evaluates `kD * albedo * irradiance`,
/// consistent with the prior ambient term and without applying `/π` twice.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DiffuseIrradianceSh3d {
    coefficients: [[f32; 3]; 9],
}

impl DiffuseIrradianceSh3d {
    /// Creates validated L2 real-SH diffuse irradiance coefficients.
    ///
    /// Coefficients may be negative: directional projections require this, and
    /// the shader clamps the reconstructed irradiance to non-negative RGB.
    pub fn l2(coefficients: [[f32; 3]; 9]) -> Result<Self, DiffuseIrradianceShError> {
        if coefficients
            .iter()
            .flatten()
            .any(|channel| !channel.is_finite())
        {
            return Err(DiffuseIrradianceShError::NonFiniteCoefficient);
        }
        Ok(Self { coefficients })
    }

    /// Creates direction-independent Lambert-normalized diffuse irradiance.
    pub fn constant(irradiance: [f32; 3]) -> Result<Self, DiffuseIrradianceShError> {
        if !non_negative_finite(&irradiance) {
            return Err(DiffuseIrradianceShError::InvalidConstant);
        }
        // Y00 = 0.2820947918, therefore c0 = irradiance / Y00.
        Self::l2([
            [
                irradiance[0] * 3.544_907_8,
                irradiance[1] * 3.544_907_8,
                irradiance[2] * 3.544_907_8,
            ],
            [0.0; 3],
            [0.0; 3],
            [0.0; 3],
            [0.0; 3],
            [0.0; 3],
            [0.0; 3],
            [0.0; 3],
            [0.0; 3],
        ])
    }

    /// Returns the real-SH coefficients in the documented basis order.
    #[must_use]
    pub const fn coefficients(self) -> [[f32; 3]; 9] {
        self.coefficients
    }
}

/// Invalid [`DiffuseIrradianceSh3d`] input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiffuseIrradianceShError {
    /// An L2 coefficient contained NaN or infinity.
    NonFiniteCoefficient,
    /// Constant irradiance must be finite and non-negative.
    InvalidConstant,
}

impl fmt::Display for DiffuseIrradianceShError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NonFiniteCoefficient => "diffuse SH coefficients must be finite",
            Self::InvalidConstant => "constant diffuse irradiance must be finite and non-negative",
        })
    }
}

impl Error for DiffuseIrradianceShError {}

/// Direct and diffuse image-based lighting inputs for the PBR renderers.
///
/// Specular IBL strength scales the factor-only prefiltered-cube term. The
/// textured PBR path ignores it until that pipeline gains an IBL bind group.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PbrLighting3d {
    direct: LambertLighting3d,
    diffuse_irradiance: DiffuseIrradianceSh3d,
    specular_ibl_strength: f32,
}

impl PbrLighting3d {
    /// Combines the existing directional direct light with diffuse L2 SH IBL.
    ///
    /// Specular IBL strength defaults to `0.0` (diffuse-only). Use
    /// [`Self::with_specular_ibl_strength`] for the factor-only cube path.
    #[must_use]
    pub const fn new(direct: LambertLighting3d, diffuse_irradiance: DiffuseIrradianceSh3d) -> Self {
        Self {
            direct,
            diffuse_irradiance,
            specular_ibl_strength: 0.0,
        }
    }

    /// Scales the factor-only prefiltered specular IBL contribution.
    #[must_use]
    pub const fn with_specular_ibl_strength(mut self, strength: f32) -> Self {
        self.specular_ibl_strength = strength;
        self
    }

    /// Returns the directional light used by the unchanged GGX direct path.
    #[must_use]
    pub const fn direct(self) -> LambertLighting3d {
        self.direct
    }

    /// Returns the diffuse L2 SH irradiance field.
    #[must_use]
    pub const fn diffuse_irradiance(self) -> DiffuseIrradianceSh3d {
        self.diffuse_irradiance
    }

    /// Returns the factor-only specular IBL strength multiplier.
    #[must_use]
    pub const fn specular_ibl_strength(self) -> f32 {
        self.specular_ibl_strength
    }
}

impl From<LambertLighting3d> for PbrLighting3d {
    fn from(direct: LambertLighting3d) -> Self {
        let diffuse_irradiance =
            DiffuseIrradianceSh3d::constant(direct.ambient()).expect("validated ambient is valid");
        Self::new(direct, diffuse_irradiance)
    }
}

/// Validated metallic/roughness material factors for the built-in PBR preset.
///
/// This first PBR slice evaluates a GGX/Smith/Schlick direct-light BRDF. It has
/// L2 SH diffuse IBL and texture sampling; specular IBL remains unavailable.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PbrMaterial3d {
    base_color: [f32; 4],
    metallic: f32,
    roughness: f32,
    emissive: [f32; 3],
    alpha_mode: PbrAlphaMode3d,
}

impl PbrMaterial3d {
    /// Creates a factor-only metallic/roughness material.
    ///
    /// # Errors
    ///
    /// Returns [`PbrMaterialError`] unless colours are finite/non-negative and
    /// metallic/roughness are finite values in `0.0..=1.0`.
    pub fn new(
        base_color: [f32; 4],
        metallic: f32,
        roughness: f32,
    ) -> Result<Self, PbrMaterialError> {
        if !non_negative_finite(&base_color) {
            return Err(PbrMaterialError::InvalidBaseColor);
        }
        if !unit_factor(metallic) {
            return Err(PbrMaterialError::InvalidMetallic);
        }
        if !unit_factor(roughness) {
            return Err(PbrMaterialError::InvalidRoughness);
        }
        Ok(Self {
            base_color,
            metallic,
            roughness,
            emissive: [0.0; 3],
            alpha_mode: PbrAlphaMode3d::Opaque,
        })
    }

    /// Adds a finite, non-negative linear emissive colour.
    ///
    /// # Errors
    ///
    /// Returns [`PbrMaterialError::InvalidEmissive`] for invalid channels.
    pub fn with_emissive(mut self, emissive: [f32; 3]) -> Result<Self, PbrMaterialError> {
        if !non_negative_finite(&emissive) {
            return Err(PbrMaterialError::InvalidEmissive);
        }
        self.emissive = emissive;
        Ok(self)
    }

    /// Selects the validated opaque, cutout or blended material state.
    #[must_use]
    pub const fn with_alpha_mode(mut self, alpha_mode: PbrAlphaMode3d) -> Self {
        self.alpha_mode = alpha_mode;
        self
    }

    /// Returns the linear base-colour factor.
    #[must_use]
    pub const fn base_color(self) -> [f32; 4] {
        self.base_color
    }

    /// Returns the metallic factor in `0.0..=1.0`.
    #[must_use]
    pub const fn metallic(self) -> f32 {
        self.metallic
    }

    /// Returns the perceptual roughness factor in `0.0..=1.0`.
    #[must_use]
    pub const fn roughness(self) -> f32 {
        self.roughness
    }

    /// Returns the linear emissive RGB factor.
    #[must_use]
    pub const fn emissive(self) -> [f32; 3] {
        self.emissive
    }

    /// Returns the render-phase and fragment-coverage policy.
    #[must_use]
    pub const fn alpha_mode(self) -> PbrAlphaMode3d {
        self.alpha_mode
    }
}

/// Validated alpha state for a PBR draw.
///
/// `Mask` stays in the opaque, depth-writing phase and discards fragments
/// below its cutoff. `Blend` belongs to the caller-sorted transparent phase.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum PbrAlphaMode3d {
    /// Replace colour and write depth.
    #[default]
    Opaque,
    /// Replace colour, discard uncovered fragments and write surviving depth.
    Mask {
        /// Finite cutoff in `0.0..=1.0`.
        cutoff: PbrAlphaCutoff3d,
    },
    /// Source-over blending with depth testing and no depth writes.
    Blend,
}

impl PbrAlphaMode3d {
    /// Creates a validated cutout state.
    ///
    /// # Errors
    ///
    /// Returns [`PbrMaterialError::InvalidAlphaCutoff`] unless `cutoff` is
    /// finite and in `0.0..=1.0`.
    pub fn mask(cutoff: f32) -> Result<Self, PbrMaterialError> {
        Ok(Self::Mask {
            cutoff: PbrAlphaCutoff3d::new(cutoff)?,
        })
    }

    /// Returns the cutoff for `Mask` and `None` for phase-only modes.
    #[must_use]
    pub const fn cutoff(self) -> Option<f32> {
        match self {
            Self::Mask { cutoff } => Some(cutoff.get()),
            Self::Opaque | Self::Blend => None,
        }
    }

    /// Cutoff for fragment shaders (`Mask` → value, otherwise `-1` disables discard).
    #[must_use]
    pub const fn shader_cutoff(self) -> f32 {
        match self {
            Self::Mask { cutoff } => cutoff.get(),
            Self::Opaque | Self::Blend => -1.0,
        }
    }
}

impl TryFrom<AlphaMode> for PbrAlphaMode3d {
    type Error = PbrMaterialError;

    fn try_from(value: AlphaMode) -> Result<Self, Self::Error> {
        match value {
            AlphaMode::Opaque => Ok(Self::Opaque),
            AlphaMode::Mask { cutoff } => Self::mask(cutoff),
            AlphaMode::Blend => Ok(Self::Blend),
        }
    }
}

/// Validated alpha cutoff used inside [`PbrAlphaMode3d::Mask`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PbrAlphaCutoff3d(f32);

impl PbrAlphaCutoff3d {
    fn new(cutoff: f32) -> Result<Self, PbrMaterialError> {
        if !unit_factor(cutoff) {
            return Err(PbrMaterialError::InvalidAlphaCutoff);
        }
        Ok(Self(cutoff))
    }

    /// Returns the finite cutoff in `0.0..=1.0`.
    #[must_use]
    pub const fn get(self) -> f32 {
        self.0
    }
}

impl Default for PbrMaterial3d {
    fn default() -> Self {
        Self::new([0.8, 0.8, 0.82, 1.0], 0.0, 0.5).expect("the built-in PBR factors are valid")
    }
}

fn unit_factor(value: f32) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}

/// Invalid PBR factor input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PbrMaterialError {
    /// Base colour contained a negative or non-finite channel.
    InvalidBaseColor,
    /// Metallic was outside the finite unit interval.
    InvalidMetallic,
    /// Roughness was outside the finite unit interval.
    InvalidRoughness,
    /// Emissive colour contained a negative or non-finite channel.
    InvalidEmissive,
    /// Alpha-mask cutoff was outside the finite unit interval.
    InvalidAlphaCutoff,
}

impl fmt::Display for PbrMaterialError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid PBR material factor: {self:?}")
    }
}

impl Error for PbrMaterialError {}

/// Position/normal geometry uploaded for the PBR preset.
pub struct GpuPbrMesh(pub(crate) GpuLitMesh);

impl GpuPbrMesh {
    /// Returns the uploaded triangle-list index count.
    #[must_use]
    pub const fn index_count(&self) -> u32 {
        self.0.index_count
    }
}

/// Built-in factor-only metallic/roughness renderer.
///
/// It is a high-level material preset and a low-level mesh draw boundary: users
/// choose factors, camera and light without authoring WGSL, while custom paths
/// may continue to use `RenderGraph` and raw WGPU. Specular IBL binds a
/// prefiltered cube + BRDF LUT at `@group(2)`; omit it to use the neutral
/// black default (strength still comes from [`PbrLighting3d`]).
pub struct PbrMeshRenderer3d {
    pipeline: wgpu::RenderPipeline,
    double_sided_pipeline: wgpu::RenderPipeline,
    mirrored_pipeline: wgpu::RenderPipeline,
    mirrored_double_sided_pipeline: wgpu::RenderPipeline,
    camera_buffer: wgpu::Buffer,
    draw_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    draw_bind_group: wgpu::BindGroup,
    neutral_specular_ibl: crate::GpuSpecularIbl3d,
    neutral_shadow: crate::GpuDirectionalShadow,
}

impl PbrMeshRenderer3d {
    /// Creates the PBR preset for a persistent renderer.
    #[must_use]
    pub fn new(renderer: &Renderer) -> Self {
        let depth_format = renderer.depth_format();
        let color_format = renderer.color_target_format();
        renderer.with_raw_gpu(|device, queue, _configuration| {
            Self::create(device, queue, color_format, depth_format)
        })
    }

    /// Creates the PBR preset lazily from a frame.
    #[must_use]
    pub fn new_for_frame(frame: &RenderFrame<'_>) -> Self {
        Self::create(
            frame.device(),
            frame.queue(),
            frame.surface_format(),
            frame.depth_format(),
        )
    }

    /// Uploads finite position/normal geometry.
    ///
    /// # Errors
    ///
    /// Returns [`LitMeshUploadError`] because the PBR and Lambert presets share
    /// the exact validated position/normal stream contract.
    pub fn upload_mesh(
        &self,
        renderer: &Renderer,
        primitive: &MeshPrimitive,
    ) -> Result<GpuPbrMesh, LitMeshUploadError> {
        renderer.with_raw_gpu(|device, _queue, _configuration| {
            LitMeshRenderer3d::upload_with(device, primitive).map(GpuPbrMesh)
        })
    }

    /// Uploads geometry through the current frame's device.
    ///
    /// # Errors
    ///
    /// Returns [`LitMeshUploadError`] for missing or invalid normals/geometry.
    pub fn upload_mesh_for_frame(
        &self,
        frame: &RenderFrame<'_>,
        primitive: &MeshPrimitive,
    ) -> Result<GpuPbrMesh, LitMeshUploadError> {
        LitMeshRenderer3d::upload_with(frame.device(), primitive).map(GpuPbrMesh)
    }

    /// Draws one opaque PBR mesh with a fresh depth phase.
    ///
    /// # Errors
    ///
    /// Returns [`PbrMeshRenderError`] for invalid camera/model/light data.
    pub fn draw(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        mesh: &GpuPbrMesh,
        model_matrix: [f32; 16],
        material: PbrMaterial3d,
        lighting: PbrLighting3d,
    ) -> Result<MeshDrawStats, PbrMeshRenderError> {
        self.draw_with_specular_ibl(
            frame,
            camera,
            mesh,
            model_matrix,
            material,
            lighting,
            DepthLoad::Clear,
            false,
            None,
            None,
        )
    }

    /// Draws with explicit depth and culling policy.
    ///
    /// # Errors
    ///
    /// Returns [`PbrMeshRenderError`] for invalid camera/model/light data.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_with_depth_load(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        mesh: &GpuPbrMesh,
        model_matrix: [f32; 16],
        material: PbrMaterial3d,
        lighting: PbrLighting3d,
        depth_load: DepthLoad,
        double_sided: bool,
    ) -> Result<MeshDrawStats, PbrMeshRenderError> {
        self.draw_with_specular_ibl(
            frame,
            camera,
            mesh,
            model_matrix,
            material,
            lighting,
            depth_load,
            double_sided,
            None,
            None,
        )
    }

    /// Draws with an explicit prefiltered specular environment at `@group(2)`.
    ///
    /// When `specular_ibl` is `None`, the neutral black default is bound. Set
    /// [`PbrLighting3d::with_specular_ibl_strength`] positive to enable the
    /// split-sum term. When `shadow` is `None`, a disabled 1×1 placeholder is
    /// bound so direct light stays unoccluded.
    ///
    /// # Errors
    ///
    /// Returns [`PbrMeshRenderError`] for invalid camera/model/light data.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_with_specular_ibl(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        mesh: &GpuPbrMesh,
        model_matrix: [f32; 16],
        material: PbrMaterial3d,
        lighting: PbrLighting3d,
        depth_load: DepthLoad,
        double_sided: bool,
        specular_ibl: Option<&crate::GpuSpecularIbl3d>,
        shadow: Option<&crate::GpuDirectionalShadow>,
    ) -> Result<MeshDrawStats, PbrMeshRenderError> {
        validate_alpha_phase(material.alpha_mode(), false)?;
        if !lighting.specular_ibl_strength().is_finite() || lighting.specular_ibl_strength() < 0.0
        {
            return Err(PbrMeshRenderError::InvalidSpecularIblStrength);
        }
        let view_projection = camera
            .view_projection(frame.draw_size())
            .map_err(PbrMeshRenderError::Mesh)?;
        let camera_uniform = PbrCameraUniform {
            view_projection,
            position: [
                camera.position[0],
                camera.position[1],
                camera.position[2],
                0.0,
            ],
        };
        let draw_uniform = PbrDrawUniform::new(model_matrix, material, lighting)?;
        let mirrored = crate::model_matrix_reverses_winding(model_matrix);
        let ibl_bind_group = specular_ibl
            .unwrap_or(&self.neutral_specular_ibl)
            .bind_group();
        let shadow_bind_group = shadow
            .unwrap_or(&self.neutral_shadow)
            .bind_group();
        frame
            .queue()
            .write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&camera_uniform));
        frame
            .queue()
            .write_buffer(&self.draw_buffer, 0, bytemuck::bytes_of(&draw_uniform));
        frame.with_surface_pass_with_depth(wgpu::LoadOp::Load, depth_load.operation(), |pass| {
            pass.set_pipeline(match (mirrored, double_sided) {
                (false, false) => &self.pipeline,
                (false, true) => &self.double_sided_pipeline,
                (true, false) => &self.mirrored_pipeline,
                (true, true) => &self.mirrored_double_sided_pipeline,
            });
            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            pass.set_bind_group(1, &self.draw_bind_group, &[]);
            pass.set_bind_group(2, ibl_bind_group, &[]);
            pass.set_bind_group(3, shadow_bind_group, &[]);
            pass.set_vertex_buffer(0, mesh.0.vertex_buffer.slice(..));
            pass.set_index_buffer(mesh.0.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..mesh.0.index_count, 0, 0..1);
        });
        Ok(MeshDrawStats {
            triangles: mesh.0.index_count / 3,
            draw_calls: 1,
            transient_uniform_buffer_allocations: 0,
        })
    }

    #[allow(clippy::too_many_lines)]
    fn create(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        depth_format: wgpu::TextureFormat,
    ) -> Self {
        let camera_layout = uniform_layout(
            device,
            "yuyib PBR camera layout",
            wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
        );
        let draw_layout = uniform_layout(
            device,
            "yuyib PBR draw layout",
            wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
        );
        let ibl_layout = crate::GpuSpecularIbl3d::bind_group_layout(device);
        let shadow_layout = crate::GpuDirectionalShadow::bind_group_layout(device);
        let camera_buffer = uniform_buffer(
            device,
            "yuyib PBR camera",
            size_of::<PbrCameraUniform>() as u64,
        );
        let draw_buffer =
            uniform_buffer(device, "yuyib PBR draw", size_of::<PbrDrawUniform>() as u64);
        let camera_bind_group = uniform_bind_group(
            device,
            "yuyib PBR camera bind group",
            &camera_layout,
            &camera_buffer,
        );
        let draw_bind_group = uniform_bind_group(
            device,
            "yuyib PBR draw bind group",
            &draw_layout,
            &draw_buffer,
        );
        let neutral_prepared = crate::PreparedSpecularIbl3d::neutral_black()
            .expect("neutral specular IBL packing is fixed");
        let neutral_specular_ibl =
            crate::GpuSpecularIbl3d::upload_with(device, queue, &neutral_prepared);
        let neutral_shadow = crate::GpuDirectionalShadow::neutral_lit(device, queue);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("yuyib metallic-roughness PBR WGSL"),
            source: wgpu::ShaderSource::Wgsl(PBR_WGSL.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("yuyib PBR pipeline layout"),
            bind_group_layouts: &[
                Some(&camera_layout),
                Some(&draw_layout),
                Some(&ibl_layout),
                Some(&shadow_layout),
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
                    buffers: &[Some(crate::LIT_VERTEX_LAYOUT)],
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
                "yuyib PBR pipeline",
                wgpu::FrontFace::Ccw,
                Some(wgpu::Face::Back),
            ),
            double_sided_pipeline: make_pipeline(
                "yuyib PBR double-sided pipeline",
                wgpu::FrontFace::Ccw,
                None,
            ),
            mirrored_pipeline: make_pipeline(
                "yuyib PBR mirrored pipeline",
                wgpu::FrontFace::Cw,
                Some(wgpu::Face::Back),
            ),
            mirrored_double_sided_pipeline: make_pipeline(
                "yuyib PBR mirrored double-sided pipeline",
                wgpu::FrontFace::Cw,
                None,
            ),
            camera_buffer,
            draw_buffer,
            camera_bind_group,
            draw_bind_group,
            neutral_specular_ibl,
            neutral_shadow,
        }
    }
}

/// Position, normal, tangent and material-selected UV geometry for the glTF path.
pub struct GpuTexturedPbrMesh {
    pub(crate) vertex_buffer: wgpu::Buffer,
    pub(crate) index_buffer: wgpu::Buffer,
    pub(crate) index_count: u32,
}

impl GpuTexturedPbrMesh {
    /// Returns the uploaded triangle-list index count.
    #[must_use]
    pub const fn index_count(&self) -> u32 {
        self.index_count
    }
}

/// Cached sampled resources for one textured PBR material.
#[derive(Clone)]
pub struct GpuTexturedPbrMaterial {
    bind_group: Arc<wgpu::BindGroup>,
    texture_presence: PbrTexturePresence3d,
}

impl GpuTexturedPbrMaterial {
    /// Material bind group (base/normal/MR/emissive texture pairs).
    #[must_use]
    pub(crate) fn bind_group(&self) -> &wgpu::BindGroup {
        self.bind_group.as_ref()
    }
}

/// Texture channels authored by a partially textured glTF PBR material.
///
/// The fixed GPU layout still binds four resources, while the fragment shader
/// samples only channels marked present. Missing bindings can therefore reuse
/// any resident fallback texture without changing material semantics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PbrTexturePresence3d {
    bits: u8,
}

impl PbrTexturePresence3d {
    /// Describes the four optional core metallic/roughness texture channels.
    /// Channel order is base colour, normal, metallic-roughness and emissive.
    #[must_use]
    pub const fn from_channels(channels: [bool; 4]) -> Self {
        let base_color = if channels[0] { 1 } else { 0 };
        let normal = if channels[1] { 1 << 1 } else { 0 };
        let metallic_roughness = if channels[2] { 1 << 2 } else { 0 };
        let emissive = if channels[3] { 1 << 3 } else { 0 };
        Self {
            bits: base_color | normal | metallic_roughness | emissive,
        }
    }

    /// All four channels are backed by authored textures.
    #[must_use]
    pub const fn complete() -> Self {
        Self { bits: 0b1111 }
    }

    const fn bits(self) -> u8 {
        self.bits
    }

    const fn has_normal(self) -> bool {
        self.bits & (1 << 1) != 0
    }
}

/// Authored UV set selected independently by each glTF PBR texture slot.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PbrTextureCoordinateSets3d {
    /// Base-colour texture UV set.
    pub base_color: u8,
    /// Normal texture UV set.
    pub normal: u8,
    /// Metallic-roughness texture UV set.
    pub metallic_roughness: u8,
    /// Emissive texture UV set.
    pub emissive: u8,
}

/// One opaque or transparent draw submitted to a textured PBR batch.
pub struct TexturedPbrBatchDraw<'a> {
    /// GPU mesh with tangent-space vertex streams.
    pub mesh: &'a GpuTexturedPbrMesh,
    /// Cached four-map material binding.
    pub binding: &'a GpuTexturedPbrMaterial,
    /// Model-to-world matrix.
    pub model_matrix: [f32; 16],
    /// Validated material factors.
    pub material: PbrMaterial3d,
    /// glTF normal-map scale.
    pub normal_scale: f32,
    /// Disables back-face culling for this item.
    pub double_sided: bool,
}

const TEXTURED_PBR_BATCH_CAPACITY: usize = 512;
/// Immutable batch uniform slots rotated within one frame so opaque /
/// transparent / chunked draws cannot overwrite each other before submit.
const TEXTURED_PBR_BATCH_RING_SLOTS: usize = 8;

struct TexturedPbrBatchUniformSlot {
    buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

/// Built-in glTF metallic/roughness renderer with tangent-space normal mapping.
///
/// The fixed material layout supports optional base-colour, normal, combined
/// metallic/roughness and emissive maps. Keeping this separate from
/// [`PbrMeshRenderer3d`] avoids optional-texture branches in the cheap
/// factor-only pipeline.
pub struct TexturedPbrMeshRenderer3d {
    pipeline: wgpu::RenderPipeline,
    double_sided_pipeline: wgpu::RenderPipeline,
    transparent_pipeline: wgpu::RenderPipeline,
    transparent_double_sided_pipeline: wgpu::RenderPipeline,
    mirrored_pipeline: wgpu::RenderPipeline,
    mirrored_double_sided_pipeline: wgpu::RenderPipeline,
    mirrored_transparent_pipeline: wgpu::RenderPipeline,
    mirrored_transparent_double_sided_pipeline: wgpu::RenderPipeline,
    camera_buffer: wgpu::Buffer,
    draw_buffer: wgpu::Buffer,
    draw_uniform_stride: u64,
    camera_bind_group: wgpu::BindGroup,
    draw_bind_group: wgpu::BindGroup,
    draw_layout: wgpu::BindGroupLayout,
    material_layout: wgpu::BindGroupLayout,
    environment_layout: wgpu::BindGroupLayout,
    neutral_specular_ibl: crate::GpuSpecularIbl3d,
    neutral_shadow: crate::GpuDirectionalShadow,
    neutral_environment_bind_group: wgpu::BindGroup,
    batch_uniform_ring: Vec<TexturedPbrBatchUniformSlot>,
    batch_uniform_ring_cursor: Cell<usize>,
}

impl TexturedPbrMeshRenderer3d {
    /// Creates the textured PBR renderer for a persistent renderer.
    #[must_use]
    pub fn new(renderer: &Renderer) -> Self {
        let depth_format = renderer.depth_format();
        let color_format = renderer.color_target_format();
        renderer.with_raw_gpu(|device, queue, _configuration| {
            Self::create(device, queue, color_format, depth_format)
        })
    }

    /// Creates the textured PBR renderer lazily from a frame.
    #[must_use]
    pub fn new_for_frame(frame: &RenderFrame<'_>) -> Self {
        Self::create(
            frame.device(),
            frame.queue(),
            frame.surface_format(),
            frame.depth_format(),
        )
    }

    /// Uploads a primitive with position, normal, tangent and UV0 for all maps.
    ///
    /// # Errors
    ///
    /// Returns [`TexturedPbrMeshUploadError`] for absent or invalid streams.
    pub fn upload_mesh_for_frame(
        &self,
        frame: &RenderFrame<'_>,
        primitive: &MeshPrimitive,
    ) -> Result<GpuTexturedPbrMesh, TexturedPbrMeshUploadError> {
        Self::upload_with(
            frame.device(),
            primitive,
            PbrTextureCoordinateSets3d::default(),
            true,
        )
    }

    /// Uploads a primitive while selecting the authored UV set used by each
    /// PBR texture slot.
    ///
    /// The resulting GPU vertex stores four resolved UV pairs rather than all
    /// eight source streams. This preserves material semantics without paying
    /// an eight-set vertex bandwidth cost at draw time.
    ///
    /// # Errors
    ///
    /// Returns [`TexturedPbrMeshUploadError::MissingTexCoords`] when a material
    /// names a stream absent from the primitive, or the ordinary validation
    /// errors returned by [`Self::upload_mesh_for_frame`].
    pub fn upload_mesh_with_texture_coordinate_sets_for_frame(
        &self,
        frame: &RenderFrame<'_>,
        primitive: &MeshPrimitive,
        sets: PbrTextureCoordinateSets3d,
    ) -> Result<GpuTexturedPbrMesh, TexturedPbrMeshUploadError> {
        Self::upload_with(frame.device(), primitive, sets, true)
    }

    /// Uploads geometry for a partially textured PBR material.
    ///
    /// Tangents are required only when the material actually has a normal map.
    ///
    /// # Errors
    ///
    /// Returns [`TexturedPbrMeshUploadError`] for missing material-selected UVs,
    /// missing normal-map tangents or invalid geometry streams.
    pub fn upload_partial_mesh_for_frame(
        &self,
        frame: &RenderFrame<'_>,
        primitive: &MeshPrimitive,
        sets: PbrTextureCoordinateSets3d,
        presence: PbrTexturePresence3d,
    ) -> Result<GpuTexturedPbrMesh, TexturedPbrMeshUploadError> {
        Self::upload_with(frame.device(), primitive, sets, presence.has_normal())
    }

    /// Creates the cached four-map material bind group.
    #[must_use]
    pub fn upload_material_for_frame(
        &self,
        frame: &RenderFrame<'_>,
        base_color: &GpuTexture,
        normal: &GpuTexture,
        metallic_roughness: &GpuTexture,
        emissive: &GpuTexture,
    ) -> GpuTexturedPbrMaterial {
        self.upload_partial_material_for_frame(
            frame,
            base_color,
            normal,
            metallic_roughness,
            emissive,
            PbrTexturePresence3d::complete(),
        )
    }

    /// Creates a binding for an arbitrary non-empty subset of PBR maps.
    ///
    /// Missing slots may reference any valid fallback texture because
    /// `presence` prevents the shader from sampling them.
    #[must_use]
    pub fn upload_partial_material_for_frame(
        &self,
        frame: &RenderFrame<'_>,
        base_color: &GpuTexture,
        normal: &GpuTexture,
        metallic_roughness: &GpuTexture,
        emissive: &GpuTexture,
        presence: PbrTexturePresence3d,
    ) -> GpuTexturedPbrMaterial {
        let textures = [base_color, normal, metallic_roughness, emissive];
        let mut entries = Vec::with_capacity(8);
        for (binding, texture) in [0_u32, 2, 4, 6].into_iter().zip(textures) {
            entries.push(wgpu::BindGroupEntry {
                binding,
                resource: wgpu::BindingResource::TextureView(texture.view()),
            });
            entries.push(wgpu::BindGroupEntry {
                binding: binding + 1,
                resource: wgpu::BindingResource::Sampler(texture.sampler()),
            });
        }
        GpuTexturedPbrMaterial {
            bind_group: Arc::new(
                frame
                    .device()
                    .create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("yuyib textured PBR cached material bind group"),
                        layout: &self.material_layout,
                        entries: &entries,
                    }),
            ),
            texture_presence: presence,
        }
    }

    /// Draws one opaque textured PBR primitive with explicit shared depth policy.
    ///
    /// # Errors
    ///
    /// Returns [`PbrMeshRenderError`] for invalid camera, transform or light data.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_with_depth_load(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        mesh: &GpuTexturedPbrMesh,
        material_binding: &GpuTexturedPbrMaterial,
        model_matrix: [f32; 16],
        material: PbrMaterial3d,
        normal_scale: f32,
        lighting: PbrLighting3d,
        depth_load: DepthLoad,
        double_sided: bool,
    ) -> Result<MeshDrawStats, PbrMeshRenderError> {
        self.draw_with_depth_load_alpha(
            frame,
            camera,
            mesh,
            material_binding,
            model_matrix,
            material,
            normal_scale,
            lighting,
            depth_load,
            double_sided,
            false,
        )
    }

    /// Draws one opaque or alpha-blended textured PBR primitive.
    ///
    /// Transparent draws keep depth testing but disable depth writes. The
    /// caller owns back-to-front ordering across transparent primitives.
    ///
    /// # Errors
    ///
    /// Returns [`PbrMeshRenderError`] for invalid camera, transform or light data.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_with_depth_load_alpha(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        mesh: &GpuTexturedPbrMesh,
        material_binding: &GpuTexturedPbrMaterial,
        model_matrix: [f32; 16],
        material: PbrMaterial3d,
        normal_scale: f32,
        lighting: PbrLighting3d,
        depth_load: DepthLoad,
        double_sided: bool,
        transparent: bool,
    ) -> Result<MeshDrawStats, PbrMeshRenderError> {
        validate_alpha_phase(material.alpha_mode(), transparent)?;
        if !normal_scale.is_finite() {
            return Err(PbrMeshRenderError::InvalidNormalScale);
        }
        if !lighting.specular_ibl_strength().is_finite() || lighting.specular_ibl_strength() < 0.0
        {
            return Err(PbrMeshRenderError::InvalidSpecularIblStrength);
        }
        let view_projection = camera
            .view_projection(frame.draw_size())
            .map_err(PbrMeshRenderError::Mesh)?;
        let camera_uniform = PbrCameraUniform {
            view_projection,
            position: [
                camera.position[0],
                camera.position[1],
                camera.position[2],
                0.0,
            ],
        };
        let mut draw_uniform = PbrDrawUniform::new(model_matrix, material, lighting)?;
        draw_uniform.material[2] = normal_scale;
        let mirrored = crate::model_matrix_reverses_winding(model_matrix);
        draw_uniform.material[3] = if mirrored { -1.0 } else { 1.0 };
        draw_uniform.emissive[3] = f32::from(material_binding.texture_presence.bits());
        frame
            .queue()
            .write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&camera_uniform));
        frame
            .queue()
            .write_buffer(&self.draw_buffer, 0, bytemuck::bytes_of(&draw_uniform));
        frame.with_surface_pass_with_depth(wgpu::LoadOp::Load, depth_load.operation(), |pass| {
            pass.set_pipeline(self.pipeline_for(transparent, double_sided, mirrored));
            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            pass.set_bind_group(1, &self.draw_bind_group, &[0]);
            pass.set_bind_group(2, material_binding.bind_group.as_ref(), &[]);
            pass.set_bind_group(3, &self.neutral_environment_bind_group, &[]);
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

    /// Records up to 512 textured PBR items in one render pass.
    ///
    /// Set `transparent` only for a caller-sorted back-to-front slice. That
    /// phase depth-tests but does not write depth.
    ///
    /// # Errors
    ///
    /// Returns [`PbrMeshRenderError`] when camera, transform, light or normal
    /// scale data is invalid, or when the fixed batch capacity is exceeded.
    ///
    /// # Panics
    ///
    /// Panics only if a GPU reports a dynamic-uniform alignment that makes
    /// the fixed 512-item batch exceed WGPU's `u32` dynamic-offset range.
    pub fn draw_batch_with_depth_load(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        draws: &[TexturedPbrBatchDraw<'_>],
        lighting: PbrLighting3d,
        depth_load: DepthLoad,
        transparent: bool,
    ) -> Result<MeshDrawStats, PbrMeshRenderError> {
        self.draw_batch_with_specular_ibl(
            frame,
            camera,
            draws,
            lighting,
            depth_load,
            transparent,
            None,
            None,
        )
    }

    /// Batch draw with an explicit prefiltered specular environment at `@group(3)`.
    ///
    /// Optional `shadow` is packed into the same `@group(3)` as IBL (bindings
    /// 4..=6) so the pipeline stays within the portable `max_bind_groups = 4`
    /// limit. `None` uses a disabled placeholder shadow.
    ///
    /// Batch uniforms are written into a ring of immutable slots so opaque and
    /// transparent encodes in the same frame cannot overwrite each other.
    /// Call [`Self::reset_batch_uniform_ring`] once at the start of a scene
    /// frame before issuing batches. If the ring is exhausted mid-frame, the
    /// draw falls back to a one-shot `create_buffer_init` allocation.
    ///
    /// # Errors
    ///
    /// Returns [`PbrMeshRenderError`] for invalid camera/model/light data or an
    /// oversized batch.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_batch_with_specular_ibl(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        draws: &[TexturedPbrBatchDraw<'_>],
        lighting: PbrLighting3d,
        depth_load: DepthLoad,
        transparent: bool,
        specular_ibl: Option<&crate::GpuSpecularIbl3d>,
        shadow: Option<&crate::GpuDirectionalShadow>,
    ) -> Result<MeshDrawStats, PbrMeshRenderError> {
        if draws.len() > TEXTURED_PBR_BATCH_CAPACITY {
            return Err(PbrMeshRenderError::BatchTooLarge {
                actual: draws.len(),
                maximum: TEXTURED_PBR_BATCH_CAPACITY,
            });
        }
        if draws.is_empty() {
            return Ok(MeshDrawStats::default());
        }
        if !lighting.specular_ibl_strength().is_finite() || lighting.specular_ibl_strength() < 0.0
        {
            return Err(PbrMeshRenderError::InvalidSpecularIblStrength);
        }
        let view_projection = camera
            .view_projection(frame.draw_size())
            .map_err(PbrMeshRenderError::Mesh)?;
        let camera_uniform = PbrCameraUniform {
            view_projection,
            position: [
                camera.position[0],
                camera.position[1],
                camera.position[2],
                0.0,
            ],
        };
        let uniforms = draws
            .iter()
            .map(|draw| {
                validate_alpha_phase(draw.material.alpha_mode(), transparent)?;
                if !draw.normal_scale.is_finite() {
                    return Err(PbrMeshRenderError::InvalidNormalScale);
                }
                let mut uniform = PbrDrawUniform::new(draw.model_matrix, draw.material, lighting)?;
                uniform.material[2] = draw.normal_scale;
                uniform.material[3] = if crate::model_matrix_reverses_winding(draw.model_matrix) {
                    -1.0
                } else {
                    1.0
                };
                uniform.emissive[3] = f32::from(draw.binding.texture_presence.bits());
                Ok(uniform)
            })
            .collect::<Result<Vec<_>, _>>()?;
        frame
            .queue()
            .write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&camera_uniform));
        let byte_len = self
            .draw_uniform_stride
            .saturating_mul(u64::try_from(uniforms.len()).expect("batch capacity fits u64"));
        let mut uniform_bytes =
            vec![0_u8; usize::try_from(byte_len).expect("batch bytes fit usize")];
        for (index, uniform) in uniforms.iter().enumerate() {
            let offset = index.saturating_mul(
                usize::try_from(self.draw_uniform_stride).expect("uniform stride fits usize"),
            );
            let bytes = bytemuck::bytes_of(uniform);
            uniform_bytes[offset..offset + bytes.len()].copy_from_slice(bytes);
        }
        // Prefer a preallocated ring slot. Overflow keeps the previous
        // create_buffer_init path so mid-frame reuse cannot clobber in-flight
        // command-buffer bindings. Cursor uses Cell so draw stays `&self`
        // while batch draws borrow mesh/material caches from the scene.
        let overflow_bind_group;
        let cursor = self.batch_uniform_ring_cursor.get();
        let (draw_bind_group, transient_uniform_buffer_allocations) =
            if cursor < self.batch_uniform_ring.len() {
                self.batch_uniform_ring_cursor.set(cursor.saturating_add(1));
                let slot = &self.batch_uniform_ring[cursor];
                frame
                    .queue()
                    .write_buffer(&slot.buffer, 0, &uniform_bytes);
                (&slot.bind_group, 0_u32)
            } else {
                let batch_buffer = frame
                    .device()
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("yuyib textured PBR overflow batch uniforms"),
                        contents: &uniform_bytes,
                        usage: wgpu::BufferUsages::UNIFORM,
                    });
                overflow_bind_group = dynamic_uniform_bind_group(
                    frame.device(),
                    "yuyib textured PBR overflow batch bind group",
                    &self.draw_layout,
                    &batch_buffer,
                    size_of::<PbrDrawUniform>() as u64,
                );
                (&overflow_bind_group, 1_u32)
            };
        let ibl = specular_ibl.unwrap_or(&self.neutral_specular_ibl);
        let shadow_map = shadow.unwrap_or(&self.neutral_shadow);
        let environment_bind_group = create_textured_environment_bind_group(
            frame.device(),
            &self.environment_layout,
            ibl,
            shadow_map,
        );
        frame.with_surface_pass_with_depth(wgpu::LoadOp::Load, depth_load.operation(), |pass| {
            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            pass.set_bind_group(3, &environment_bind_group, &[]);
            for (index, draw) in draws.iter().enumerate() {
                let mirrored = crate::model_matrix_reverses_winding(draw.model_matrix);
                pass.set_pipeline(self.pipeline_for(transparent, draw.double_sided, mirrored));
                let offset = u64::try_from(index)
                    .expect("batch capacity fits u64")
                    .saturating_mul(self.draw_uniform_stride);
                let offset = u32::try_from(offset).expect("PBR dynamic offset fits u32");
                pass.set_bind_group(1, draw_bind_group, &[offset]);
                pass.set_bind_group(2, draw.binding.bind_group.as_ref(), &[]);
                pass.set_vertex_buffer(0, draw.mesh.vertex_buffer.slice(..));
                pass.set_index_buffer(draw.mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..draw.mesh.index_count, 0, 0..1);
            }
        });
        Ok(MeshDrawStats {
            triangles: draws.iter().map(|draw| draw.mesh.index_count / 3).sum(),
            draw_calls: u32::try_from(draws.len()).expect("batch capacity fits u32"),
            transient_uniform_buffer_allocations,
        })
    }

    /// Resets the batch uniform ring cursor for a new scene frame.
    pub fn reset_batch_uniform_ring(&self) {
        self.batch_uniform_ring_cursor.set(0);
    }

    fn pipeline_for(
        &self,
        transparent: bool,
        double_sided: bool,
        mirrored: bool,
    ) -> &wgpu::RenderPipeline {
        match (mirrored, transparent, double_sided) {
            (false, false, false) => &self.pipeline,
            (false, false, true) => &self.double_sided_pipeline,
            (false, true, false) => &self.transparent_pipeline,
            (false, true, true) => &self.transparent_double_sided_pipeline,
            (true, false, false) => &self.mirrored_pipeline,
            (true, false, true) => &self.mirrored_double_sided_pipeline,
            (true, true, false) => &self.mirrored_transparent_pipeline,
            (true, true, true) => &self.mirrored_transparent_double_sided_pipeline,
        }
    }

    #[allow(clippy::too_many_lines)]
    fn create(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        depth_format: wgpu::TextureFormat,
    ) -> Self {
        let camera_layout = uniform_layout(
            device,
            "yuyib textured PBR camera layout",
            wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
        );
        let draw_layout = dynamic_uniform_layout(
            device,
            "yuyib textured PBR draw layout",
            wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
            size_of::<PbrDrawUniform>() as u64,
        );
        let material_layout = textured_pbr_material_layout(device);
        let environment_layout = textured_environment_bind_group_layout(device);
        let camera_buffer = uniform_buffer(
            device,
            "yuyib textured PBR camera",
            size_of::<PbrCameraUniform>() as u64,
        );
        let draw_uniform_stride = aligned_uniform_stride(
            device.limits().min_uniform_buffer_offset_alignment,
            size_of::<PbrDrawUniform>() as u64,
        );
        let draw_buffer = uniform_buffer(
            device,
            "yuyib textured PBR draw",
            draw_uniform_stride.saturating_mul(TEXTURED_PBR_BATCH_CAPACITY as u64),
        );
        let camera_bind_group = uniform_bind_group(
            device,
            "yuyib textured PBR camera bind group",
            &camera_layout,
            &camera_buffer,
        );
        let draw_bind_group = dynamic_uniform_bind_group(
            device,
            "yuyib textured PBR draw bind group",
            &draw_layout,
            &draw_buffer,
            size_of::<PbrDrawUniform>() as u64,
        );
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("yuyib textured metallic-roughness PBR WGSL"),
            source: wgpu::ShaderSource::Wgsl(TEXTURED_PBR_WGSL.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("yuyib textured PBR pipeline layout"),
            bind_group_layouts: &[
                Some(&camera_layout),
                Some(&draw_layout),
                Some(&material_layout),
                Some(&environment_layout),
            ],
            immediate_size: 0,
        });
        let make_pipeline = |label, front_face, cull_mode, blend, depth_write_enabled| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[Some(TEXTURED_PBR_VERTEX_LAYOUT)],
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    front_face,
                    cull_mode,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: depth_format,
                    depth_write_enabled: Some(depth_write_enabled),
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
                        blend: Some(blend),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let neutral_prepared = crate::PreparedSpecularIbl3d::neutral_black()
            .expect("neutral specular IBL packing is fixed");
        let neutral_specular_ibl =
            crate::GpuSpecularIbl3d::upload_with(device, queue, &neutral_prepared);
        let neutral_shadow = crate::GpuDirectionalShadow::neutral_lit(device, queue);
        let neutral_environment_bind_group = create_textured_environment_bind_group(
            device,
            &environment_layout,
            &neutral_specular_ibl,
            &neutral_shadow,
        );
        let batch_buffer_size =
            draw_uniform_stride.saturating_mul(TEXTURED_PBR_BATCH_CAPACITY as u64);
        let batch_uniform_ring = (0..TEXTURED_PBR_BATCH_RING_SLOTS)
            .map(|_| {
                let buffer = uniform_buffer(
                    device,
                    "yuyib textured PBR batch ring slot",
                    batch_buffer_size,
                );
                let bind_group = dynamic_uniform_bind_group(
                    device,
                    "yuyib textured PBR batch ring bind group",
                    &draw_layout,
                    &buffer,
                    size_of::<PbrDrawUniform>() as u64,
                );
                TexturedPbrBatchUniformSlot { buffer, bind_group }
            })
            .collect();
        Self {
            pipeline: make_pipeline(
                "yuyib textured PBR pipeline",
                wgpu::FrontFace::Ccw,
                Some(wgpu::Face::Back),
                wgpu::BlendState::REPLACE,
                true,
            ),
            double_sided_pipeline: make_pipeline(
                "yuyib textured PBR double-sided pipeline",
                wgpu::FrontFace::Ccw,
                None,
                wgpu::BlendState::REPLACE,
                true,
            ),
            transparent_pipeline: make_pipeline(
                "yuyib textured PBR transparent pipeline",
                wgpu::FrontFace::Ccw,
                Some(wgpu::Face::Back),
                wgpu::BlendState::ALPHA_BLENDING,
                false,
            ),
            transparent_double_sided_pipeline: make_pipeline(
                "yuyib textured PBR transparent double-sided pipeline",
                wgpu::FrontFace::Ccw,
                None,
                wgpu::BlendState::ALPHA_BLENDING,
                false,
            ),
            mirrored_pipeline: make_pipeline(
                "yuyib textured PBR mirrored pipeline",
                wgpu::FrontFace::Cw,
                Some(wgpu::Face::Back),
                wgpu::BlendState::REPLACE,
                true,
            ),
            mirrored_double_sided_pipeline: make_pipeline(
                "yuyib textured PBR mirrored double-sided pipeline",
                wgpu::FrontFace::Cw,
                None,
                wgpu::BlendState::REPLACE,
                true,
            ),
            mirrored_transparent_pipeline: make_pipeline(
                "yuyib textured PBR mirrored transparent pipeline",
                wgpu::FrontFace::Cw,
                Some(wgpu::Face::Back),
                wgpu::BlendState::ALPHA_BLENDING,
                false,
            ),
            mirrored_transparent_double_sided_pipeline: make_pipeline(
                "yuyib textured PBR mirrored transparent double-sided pipeline",
                wgpu::FrontFace::Cw,
                None,
                wgpu::BlendState::ALPHA_BLENDING,
                false,
            ),
            camera_buffer,
            draw_buffer,
            draw_uniform_stride,
            camera_bind_group,
            draw_bind_group,
            draw_layout,
            material_layout,
            environment_layout: environment_layout.clone(),
            neutral_specular_ibl,
            neutral_shadow,
            neutral_environment_bind_group,
            batch_uniform_ring,
            batch_uniform_ring_cursor: Cell::new(0),
        }
    }

    fn upload_with(
        device: &wgpu::Device,
        primitive: &MeshPrimitive,
        sets: PbrTextureCoordinateSets3d,
        require_tangents: bool,
    ) -> Result<GpuTexturedPbrMesh, TexturedPbrMeshUploadError> {
        let normals = primitive
            .normals()
            .ok_or(TexturedPbrMeshUploadError::MissingNormals)?;
        let fallback_tangents;
        let tangents = if let Some(tangents) = primitive.tangents() {
            tangents
        } else if require_tangents {
            return Err(TexturedPbrMeshUploadError::MissingTangents);
        } else {
            fallback_tangents = vec![[1.0, 0.0, 0.0, 1.0]; primitive.positions().len()];
            &fallback_tangents
        };
        let resolve_tex_coords = |set| {
            primitive
                .tex_coords(set)
                .ok_or(TexturedPbrMeshUploadError::MissingTexCoords { set })
        };
        let base_tex_coords = resolve_tex_coords(sets.base_color)?;
        let normal_tex_coords = resolve_tex_coords(sets.normal)?;
        let metallic_roughness_tex_coords = resolve_tex_coords(sets.metallic_roughness)?;
        let emissive_tex_coords = resolve_tex_coords(sets.emissive)?;
        let index_count = u32::try_from(primitive.indices().len()).map_err(|_| {
            TexturedPbrMeshUploadError::TooManyIndices {
                actual: primitive.indices().len(),
            }
        })?;
        let vertices = primitive
            .positions()
            .iter()
            .copied()
            .zip(normals.iter().copied())
            .zip(tangents.iter().copied())
            .zip(base_tex_coords.iter().copied())
            .zip(normal_tex_coords.iter().copied())
            .zip(metallic_roughness_tex_coords.iter().copied())
            .zip(emissive_tex_coords.iter().copied())
            .enumerate()
            .map(
                |(
                    index,
                    (
                        (
                            ((((position, normal), tangent), base_tex_coord), normal_tex_coord),
                            metallic_roughness_tex_coord,
                        ),
                        emissive_tex_coord,
                    ),
                )| {
                    if !all_finite(&position) {
                        return Err(TexturedPbrMeshUploadError::NonFinitePosition { index });
                    }
                    if !all_finite(&normal) || normalize3(normal).is_none() {
                        return Err(TexturedPbrMeshUploadError::InvalidNormal { index });
                    }
                    if !all_finite(&tangent)
                        || normalize3([tangent[0], tangent[1], tangent[2]]).is_none()
                        || (tangent[3].abs() - 1.0).abs() > 0.001
                    {
                        return Err(TexturedPbrMeshUploadError::InvalidTangent { index });
                    }
                    for (set, tex_coord) in [
                        (sets.base_color, base_tex_coord),
                        (sets.normal, normal_tex_coord),
                        (sets.metallic_roughness, metallic_roughness_tex_coord),
                        (sets.emissive, emissive_tex_coord),
                    ] {
                        if !all_finite(&tex_coord) {
                            return Err(TexturedPbrMeshUploadError::NonFiniteTexCoords {
                                set,
                                index,
                            });
                        }
                    }
                    Ok(TexturedPbrVertex {
                        position,
                        normal,
                        tangent,
                        base_tex_coord,
                        normal_tex_coord,
                        metallic_roughness_tex_coord,
                        emissive_tex_coord,
                    })
                },
            )
            .collect::<Result<Vec<_>, _>>()?;
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("yuyib textured PBR vertices"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("yuyib textured PBR indices"),
            contents: bytemuck::cast_slice(primitive.indices()),
            usage: wgpu::BufferUsages::INDEX,
        });
        Ok(GpuTexturedPbrMesh {
            vertex_buffer,
            index_buffer,
            index_count,
        })
    }
}

/// Failure while uploading tangent-space PBR geometry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TexturedPbrMeshUploadError {
    /// Normal stream is absent.
    MissingNormals,
    /// Tangent stream is absent.
    MissingTangents,
    /// A material-selected UV stream is absent.
    MissingTexCoords {
        /// Missing authored set.
        set: u8,
    },
    /// Position is non-finite.
    NonFinitePosition {
        /// Vertex stream index.
        index: usize,
    },
    /// Normal is non-finite or degenerate.
    InvalidNormal {
        /// Vertex stream index.
        index: usize,
    },
    /// Tangent is non-finite, degenerate or has invalid handedness.
    InvalidTangent {
        /// Vertex stream index.
        index: usize,
    },
    /// A selected UV is non-finite.
    NonFiniteTexCoords {
        /// Authored set.
        set: u8,
        /// Vertex stream index.
        index: usize,
    },
    /// Index count exceeds WGPU's representation.
    TooManyIndices {
        /// Observed index count.
        actual: usize,
    },
}

impl fmt::Display for TexturedPbrMeshUploadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid textured PBR mesh: {self:?}")
    }
}

impl Error for TexturedPbrMeshUploadError {}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct TexturedPbrVertex {
    position: [f32; 3],
    normal: [f32; 3],
    tangent: [f32; 4],
    base_tex_coord: [f32; 2],
    normal_tex_coord: [f32; 2],
    metallic_roughness_tex_coord: [f32; 2],
    emissive_tex_coord: [f32; 2],
}

pub(crate) const TEXTURED_PBR_VERTEX_LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
    array_stride: size_of::<TexturedPbrVertex>() as u64,
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
            format: wgpu::VertexFormat::Float32x4,
            offset: size_of::<[f32; 3]>() as u64 * 2,
            shader_location: 2,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x2,
            offset: (size_of::<[f32; 3]>() * 2 + size_of::<[f32; 4]>()) as u64,
            shader_location: 3,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x2,
            offset: (size_of::<[f32; 3]>() * 2 + size_of::<[f32; 4]>() + size_of::<[f32; 2]>())
                as u64,
            shader_location: 4,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x2,
            offset: (size_of::<[f32; 3]>() * 2 + size_of::<[f32; 4]>() + size_of::<[f32; 2]>() * 2)
                as u64,
            shader_location: 5,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x2,
            offset: (size_of::<[f32; 3]>() * 2 + size_of::<[f32; 4]>() + size_of::<[f32; 2]>() * 3)
                as u64,
            shader_location: 6,
        },
    ],
};

fn textured_environment_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    // IBL (0..=3) + directional shadow (4..=6) share one group so textured PBR
    // stays within the portable `max_bind_groups = 4` limit (groups 0..=3).
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("yuyib textured PBR IBL+shadow layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::Cube,
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
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 4,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Depth,
                    view_dimension: wgpu::TextureViewDimension::D2Array,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 5,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 6,
                visibility: wgpu::ShaderStages::FRAGMENT | wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(
                        crate::GpuDirectionalShadow::sample_uniform_size(),
                    ),
                },
                count: None,
            },
        ],
    })
}

fn create_textured_environment_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    ibl: &crate::GpuSpecularIbl3d,
    shadow: &crate::GpuDirectionalShadow,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("yuyib textured PBR IBL+shadow bind group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(ibl.cube_view()),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(ibl.cube_sampler()),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(ibl.lut_view()),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::Sampler(ibl.lut_sampler()),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::TextureView(shadow.depth_view()),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: wgpu::BindingResource::Sampler(shadow.compare_sampler()),
            },
            wgpu::BindGroupEntry {
                binding: 6,
                resource: shadow.params_buffer().as_entire_binding(),
            },
        ],
    })
}

pub(crate) fn textured_pbr_material_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    let mut entries = Vec::with_capacity(8);
    for index in 0_u32..4 {
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: index * 2,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        });
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: index * 2 + 1,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        });
    }
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("yuyib textured PBR material layout"),
        entries: &entries,
    })
}

/// Failure while preparing a PBR draw.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PbrMeshRenderError {
    /// Camera or model matrix validation failed.
    Mesh(MeshRenderError),
    /// The normal matrix could not be inverted.
    NonInvertibleNormalMatrix,
    /// Normal-map strength was non-finite.
    InvalidNormalScale,
    /// A blended material was sent to a depth-writing opaque phase.
    BlendRequiresTransparentPhase,
    /// A non-blended material was sent to the transparent phase.
    TransparentPhaseRequiresBlend,
    /// A textured batch exceeded its fixed dynamic-uniform capacity.
    BatchTooLarge {
        /// Requested item count.
        actual: usize,
        /// Maximum accepted item count.
        maximum: usize,
    },
    /// Specular IBL strength was negative or non-finite.
    InvalidSpecularIblStrength,
}

impl fmt::Display for PbrMeshRenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mesh(error) => write!(formatter, "cannot draw PBR mesh: {error}"),
            Self::NonInvertibleNormalMatrix => {
                formatter.write_str("PBR model matrix has a non-invertible normal basis")
            }
            Self::InvalidNormalScale => formatter.write_str("PBR normal scale must be finite"),
            Self::BlendRequiresTransparentPhase => {
                formatter.write_str("AlphaMode::Blend requires the sorted transparent PBR phase")
            }
            Self::TransparentPhaseRequiresBlend => {
                formatter.write_str("the transparent PBR phase requires AlphaMode::Blend")
            }
            Self::BatchTooLarge { actual, maximum } => write!(
                formatter,
                "textured PBR batch contains {actual} draws; maximum is {maximum}"
            ),
            Self::InvalidSpecularIblStrength => {
                formatter.write_str("PBR specular IBL strength must be finite and >= 0")
            }
        }
    }
}

impl Error for PbrMeshRenderError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Mesh(error) => Some(error),
            Self::NonInvertibleNormalMatrix
            | Self::InvalidNormalScale
            | Self::BlendRequiresTransparentPhase
            | Self::TransparentPhaseRequiresBlend
            | Self::BatchTooLarge { .. }
            | Self::InvalidSpecularIblStrength => None,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct PbrCameraUniform {
    view_projection: [f32; 16],
    position: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct PbrDrawUniform {
    model: [f32; 16],
    normal_matrix: [f32; 12],
    base_color: [f32; 4],
    material: [f32; 4],
    light_direction: [f32; 4],
    light_color: [f32; 4],
    diffuse_irradiance_sh: [[f32; 4]; 9],
    emissive: [f32; 4],
    alpha: [f32; 4],
}

impl PbrDrawUniform {
    fn new(
        model: [f32; 16],
        material: PbrMaterial3d,
        lighting: PbrLighting3d,
    ) -> Result<Self, PbrMeshRenderError> {
        if !all_finite(&model) {
            return Err(PbrMeshRenderError::Mesh(
                MeshRenderError::InvalidModelMatrix,
            ));
        }
        let normal_matrix =
            inverse_transpose_3x3(model).ok_or(PbrMeshRenderError::NonInvertibleNormalMatrix)?;
        let direct = lighting.direct();
        let light = direct.light();
        let coefficients = lighting.diffuse_irradiance().coefficients();
        Ok(Self {
            model,
            normal_matrix,
            base_color: material.base_color,
            material: [material.metallic, material.roughness, 0.0, 0.0],
            light_direction: [
                light.direction[0],
                light.direction[1],
                light.direction[2],
                0.0,
            ],
            light_color: [
                light.color[0] * light.illuminance_lux,
                light.color[1] * light.illuminance_lux,
                light.color[2] * light.illuminance_lux,
                0.0,
            ],
            diffuse_irradiance_sh: coefficients
                .map(|coefficient| [coefficient[0], coefficient[1], coefficient[2], 0.0]),
            emissive: [
                material.emissive[0],
                material.emissive[1],
                material.emissive[2],
                0.0,
            ],
            // alpha.x = mask cutoff (-1 disables); alpha.y = specular IBL strength.
            // Textured draws overwrite material.z/w for normal scale / handedness.
            alpha: [
                material.alpha_mode.shader_cutoff(),
                lighting.specular_ibl_strength(),
                0.0,
                0.0,
            ],
        })
    }
}

fn validate_alpha_phase(
    alpha_mode: PbrAlphaMode3d,
    transparent: bool,
) -> Result<(), PbrMeshRenderError> {
    match (alpha_mode, transparent) {
        (PbrAlphaMode3d::Blend, false) => Err(PbrMeshRenderError::BlendRequiresTransparentPhase),
        (PbrAlphaMode3d::Opaque | PbrAlphaMode3d::Mask { .. }, true) => {
            Err(PbrMeshRenderError::TransparentPhaseRequiresBlend)
        }
        _ => Ok(()),
    }
}

const PBR_WGSL: &str = r"
const PI: f32 = 3.141592653589793;
struct Camera { view_projection: mat4x4<f32>, position: vec4<f32>, };
struct Draw {
    model: mat4x4<f32>, normal_matrix: mat3x3<f32>, base_color: vec4<f32>,
    material: vec4<f32>, light_direction: vec4<f32>, light_color: vec4<f32>,
    diffuse_irradiance_sh: array<vec4<f32>, 9>, emissive: vec4<f32>, alpha: vec4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var<uniform> draw: Draw;
@group(2) @binding(0) var specular_cube: texture_cube<f32>;
@group(2) @binding(1) var specular_sampler: sampler;
@group(2) @binding(2) var brdf_lut: texture_2d<f32>;
@group(2) @binding(3) var brdf_sampler: sampler;
struct ShadowParams {
    light_view_proj_0: mat4x4<f32>,
    light_view_proj_1: mat4x4<f32>,
    params: vec4<f32>,
};
@group(3) @binding(0) var shadow_map: texture_depth_2d_array;
@group(3) @binding(1) var shadow_sampler: sampler_comparison;
@group(3) @binding(2) var<uniform> shadow: ShadowParams;
struct VertexInput { @location(0) position: vec3<f32>, @location(1) normal: vec3<f32>, };
struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_position: vec3<f32>, @location(1) normal: vec3<f32>,
};
@vertex fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    let world = draw.model * vec4<f32>(input.position, 1.0);
    output.clip_position = camera.view_projection * world;
    output.world_position = world.xyz;
    output.normal = normalize(draw.normal_matrix * input.normal);
    return output;
}
fn distribution_ggx(n: vec3<f32>, h: vec3<f32>, roughness: f32) -> f32 {
    let a = max(roughness * roughness, 0.0025);
    let a2 = a * a;
    let n_dot_h = max(dot(n, h), 0.0);
    let denominator = n_dot_h * n_dot_h * (a2 - 1.0) + 1.0;
    return a2 / max(PI * denominator * denominator, 0.000001);
}
fn geometry_schlick_ggx(n_dot_v: f32, roughness: f32) -> f32 {
    let r = roughness + 1.0;
    let k = (r * r) / 8.0;
    return n_dot_v / max(n_dot_v * (1.0 - k) + k, 0.000001);
}
fn fresnel_schlick(cos_theta: f32, f0: vec3<f32>) -> vec3<f32> {
    return f0 + (vec3<f32>(1.0) - f0) * pow(1.0 - cos_theta, 5.0);
}
fn diffuse_irradiance(normal: vec3<f32>) -> vec3<f32> {
    let x = normal.x;
    let y = normal.y;
    let z = normal.z;
    let sh = draw.diffuse_irradiance_sh;
    return max(
        sh[0].rgb * 0.282094792
        + sh[1].rgb * (0.488602512 * y)
        + sh[2].rgb * (0.488602512 * z)
        + sh[3].rgb * (0.488602512 * x)
        + sh[4].rgb * (1.092548431 * x * y)
        + sh[5].rgb * (1.092548431 * y * z)
        + sh[6].rgb * (0.315391565 * (3.0 * z * z - 1.0))
        + sh[7].rgb * (1.092548431 * x * z)
        + sh[8].rgb * (0.546274215 * (x * x - y * y)),
        vec3<f32>(0.0)
    );
}
fn sample_cascade_pcf(world_position: vec3<f32>, cascade: i32, light_vp: mat4x4<f32>) -> f32 {
    let light_clip = light_vp * vec4<f32>(world_position, 1.0);
    let ndc = light_clip.xyz / light_clip.w;
    let uv = ndc.xy * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5, 0.5);
    let depth = ndc.z;
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0 || depth < 0.0 || depth > 1.0) {
        return -1.0;
    }
    let bias = shadow.params.y;
    let texel = max(shadow.params.z, 0.000001);
    var visibility = 0.0;
    for (var x = -1; x <= 1; x = x + 1) {
        for (var y = -1; y <= 1; y = y + 1) {
            let offset = vec2<f32>(f32(x), f32(y)) * texel;
            visibility = visibility + textureSampleCompare(
                shadow_map, shadow_sampler, uv + offset, cascade, depth - bias
            );
        }
    }
    return visibility / 9.0;
}
fn sample_directional_shadow(world_position: vec3<f32>) -> f32 {
    // Nested orthos: a receiver inside the near slab is also inside the far
    // slab. Occluders outside the near XY only land in the far map, so we must
    // combine both (min = most shadowed). Preferring near alone left feet fully
    // lit whenever nearby buildings sat just outside the tight cascade.
    var visibility = 1.0;
    let near = sample_cascade_pcf(world_position, 0, shadow.light_view_proj_0);
    if (near >= 0.0) {
        visibility = min(visibility, near);
    }
    if (shadow.params.w > 1.5) {
        let far = sample_cascade_pcf(world_position, 1, shadow.light_view_proj_1);
        if (far >= 0.0) {
            visibility = min(visibility, far);
        }
    }
    return visibility;
}
@fragment fn fs_main(
    input: VertexOutput, @builtin(front_facing) front_facing: bool,
) -> @location(0) vec4<f32> {
    if (draw.alpha.x >= 0.0 && draw.base_color.a < draw.alpha.x) { discard; }
    let n = select(-normalize(input.normal), normalize(input.normal), front_facing);
    let v = normalize(camera.position.xyz - input.world_position);
    let l = normalize(-draw.light_direction.xyz);
    let h = normalize(v + l);
    let n_dot_v = max(dot(n, v), 0.0);
    let n_dot_l = max(dot(n, l), 0.0);
    let metallic = draw.material.x;
    let roughness = max(draw.material.y, 0.045);
    let specular_ibl_strength = draw.alpha.y;
    let albedo = draw.base_color.rgb;
    let f0 = mix(vec3<f32>(0.04), albedo, metallic);
    let f = fresnel_schlick(max(dot(h, v), 0.0), f0);
    let d = distribution_ggx(n, h, roughness);
    let g = geometry_schlick_ggx(n_dot_v, roughness) * geometry_schlick_ggx(n_dot_l, roughness);
    let specular = (d * g * f) / max(4.0 * n_dot_v * n_dot_l, 0.0001);
    let diffuse_weight = (vec3<f32>(1.0) - f) * (1.0 - metallic);
    let diffuse = diffuse_weight * albedo / PI;
    var shadow_visibility = 1.0;
    if (shadow.params.x > 0.5) {
        shadow_visibility = sample_directional_shadow(input.world_position);
    }
    let direct = (diffuse + specular) * draw.light_color.rgb * n_dot_l * shadow_visibility;
    let ibl_diffuse = diffuse_weight * albedo * diffuse_irradiance(n);
    var ibl_specular = vec3<f32>(0.0);
    if (specular_ibl_strength > 0.0) {
        let reflection = reflect(-v, n);
        let max_lod = f32(textureNumLevels(specular_cube) - 1u);
        let lod = roughness * max_lod;
        let prefiltered = textureSampleLevel(specular_cube, specular_sampler, reflection, lod).rgb;
        let brdf = textureSample(brdf_lut, brdf_sampler, vec2<f32>(n_dot_v, roughness)).rg;
        let fresnel_ibl = f0 + (max(vec3<f32>(1.0 - roughness), f0) - f0) * pow(1.0 - n_dot_v, 5.0);
        ibl_specular = prefiltered * (fresnel_ibl * brdf.x + brdf.y) * specular_ibl_strength;
    }
    return vec4<f32>(direct + ibl_diffuse + ibl_specular + draw.emissive.rgb, draw.base_color.a);
}
";

const TEXTURED_PBR_WGSL: &str = r"
const PI: f32 = 3.141592653589793;
struct Camera { view_projection: mat4x4<f32>, position: vec4<f32>, };
struct Draw {
    model: mat4x4<f32>, normal_matrix: mat3x3<f32>, base_color: vec4<f32>,
    material: vec4<f32>, light_direction: vec4<f32>, light_color: vec4<f32>,
    diffuse_irradiance_sh: array<vec4<f32>, 9>, emissive: vec4<f32>, alpha: vec4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var<uniform> draw: Draw;
@group(2) @binding(0) var base_color_texture: texture_2d<f32>;
@group(2) @binding(1) var base_color_sampler: sampler;
@group(2) @binding(2) var normal_texture: texture_2d<f32>;
@group(2) @binding(3) var normal_sampler: sampler;
@group(2) @binding(4) var metallic_roughness_texture: texture_2d<f32>;
@group(2) @binding(5) var metallic_roughness_sampler: sampler;
@group(2) @binding(6) var emissive_texture: texture_2d<f32>;
@group(2) @binding(7) var emissive_sampler: sampler;
@group(3) @binding(0) var specular_cube: texture_cube<f32>;
@group(3) @binding(1) var specular_sampler: sampler;
@group(3) @binding(2) var brdf_lut: texture_2d<f32>;
@group(3) @binding(3) var brdf_sampler: sampler;
struct ShadowParams {
    light_view_proj_0: mat4x4<f32>,
    light_view_proj_1: mat4x4<f32>,
    params: vec4<f32>,
};
// Shadow shares group 3 with IBL: portable adapters often cap max_bind_groups at 4.
@group(3) @binding(4) var shadow_map: texture_depth_2d_array;
@group(3) @binding(5) var shadow_sampler: sampler_comparison;
@group(3) @binding(6) var<uniform> shadow: ShadowParams;
struct VertexInput {
    @location(0) position: vec3<f32>, @location(1) normal: vec3<f32>,
    @location(2) tangent: vec4<f32>,
    @location(3) base_tex_coord: vec2<f32>, @location(4) normal_tex_coord: vec2<f32>,
    @location(5) metallic_roughness_tex_coord: vec2<f32>,
    @location(6) emissive_tex_coord: vec2<f32>,
};
struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_position: vec3<f32>, @location(1) normal: vec3<f32>,
    @location(2) tangent: vec4<f32>,
    @location(3) base_tex_coord: vec2<f32>, @location(4) normal_tex_coord: vec2<f32>,
    @location(5) metallic_roughness_tex_coord: vec2<f32>,
    @location(6) emissive_tex_coord: vec2<f32>,
};
@vertex fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    let world = draw.model * vec4<f32>(input.position, 1.0);
    let n = normalize(draw.normal_matrix * input.normal);
    let texture_flags = u32(draw.emissive.w + 0.5);
    var tangent = vec3<f32>(1.0, 0.0, 0.0);
    if ((texture_flags & 2u) != 0u) {
        let raw_tangent = (draw.model * vec4<f32>(input.tangent.xyz, 0.0)).xyz;
        tangent = normalize(raw_tangent - n * dot(n, raw_tangent));
    }
    output.clip_position = camera.view_projection * world;
    output.world_position = world.xyz;
    output.normal = n;
    output.tangent = vec4<f32>(tangent, input.tangent.w * draw.material.w);
    output.base_tex_coord = input.base_tex_coord;
    output.normal_tex_coord = input.normal_tex_coord;
    output.metallic_roughness_tex_coord = input.metallic_roughness_tex_coord;
    output.emissive_tex_coord = input.emissive_tex_coord;
    return output;
}
fn distribution_ggx(n: vec3<f32>, h: vec3<f32>, roughness: f32) -> f32 {
    let a = max(roughness * roughness, 0.0025);
    let a2 = a * a;
    let n_dot_h = max(dot(n, h), 0.0);
    let denominator = n_dot_h * n_dot_h * (a2 - 1.0) + 1.0;
    return a2 / max(PI * denominator * denominator, 0.000001);
}
fn geometry_schlick_ggx(n_dot_v: f32, roughness: f32) -> f32 {
    let r = roughness + 1.0;
    let k = (r * r) / 8.0;
    return n_dot_v / max(n_dot_v * (1.0 - k) + k, 0.000001);
}
fn fresnel_schlick(cos_theta: f32, f0: vec3<f32>) -> vec3<f32> {
    return f0 + (vec3<f32>(1.0) - f0) * pow(1.0 - cos_theta, 5.0);
}
fn diffuse_irradiance(normal: vec3<f32>) -> vec3<f32> {
    let x = normal.x;
    let y = normal.y;
    let z = normal.z;
    let sh = draw.diffuse_irradiance_sh;
    return max(
        sh[0].rgb * 0.282094792
        + sh[1].rgb * (0.488602512 * y)
        + sh[2].rgb * (0.488602512 * z)
        + sh[3].rgb * (0.488602512 * x)
        + sh[4].rgb * (1.092548431 * x * y)
        + sh[5].rgb * (1.092548431 * y * z)
        + sh[6].rgb * (0.315391565 * (3.0 * z * z - 1.0))
        + sh[7].rgb * (1.092548431 * x * z)
        + sh[8].rgb * (0.546274215 * (x * x - y * y)),
        vec3<f32>(0.0)
    );
}
fn sample_cascade_pcf(world_position: vec3<f32>, cascade: i32, light_vp: mat4x4<f32>) -> f32 {
    let light_clip = light_vp * vec4<f32>(world_position, 1.0);
    let ndc = light_clip.xyz / light_clip.w;
    let uv = ndc.xy * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5, 0.5);
    let depth = ndc.z;
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0 || depth < 0.0 || depth > 1.0) {
        return -1.0;
    }
    let bias = shadow.params.y;
    let texel = max(shadow.params.z, 0.000001);
    var visibility = 0.0;
    for (var x = -1; x <= 1; x = x + 1) {
        for (var y = -1; y <= 1; y = y + 1) {
            let offset = vec2<f32>(f32(x), f32(y)) * texel;
            visibility = visibility + textureSampleCompare(
                shadow_map, shadow_sampler, uv + offset, cascade, depth - bias
            );
        }
    }
    return visibility / 9.0;
}
fn sample_directional_shadow(world_position: vec3<f32>) -> f32 {
    // Nested orthos: a receiver inside the near slab is also inside the far
    // slab. Occluders outside the near XY only land in the far map, so we must
    // combine both (min = most shadowed). Preferring near alone left feet fully
    // lit whenever nearby buildings sat just outside the tight cascade.
    var visibility = 1.0;
    let near = sample_cascade_pcf(world_position, 0, shadow.light_view_proj_0);
    if (near >= 0.0) {
        visibility = min(visibility, near);
    }
    if (shadow.params.w > 1.5) {
        let far = sample_cascade_pcf(world_position, 1, shadow.light_view_proj_1);
        if (far >= 0.0) {
            visibility = min(visibility, far);
        }
    }
    return visibility;
}
@fragment fn fs_main(
    input: VertexOutput, @builtin(front_facing) front_facing: bool,
) -> @location(0) vec4<f32> {
    let geometric_normal = select(-normalize(input.normal), normalize(input.normal), front_facing);
    let texture_flags = u32(draw.emissive.w + 0.5);
    var n = geometric_normal;
    if ((texture_flags & 2u) != 0u) {
        let tangent = normalize(input.tangent.xyz);
        let bitangent = normalize(cross(geometric_normal, tangent)) * input.tangent.w;
        var mapped_normal = textureSample(
            normal_texture, normal_sampler, input.normal_tex_coord
        ).xyz * 2.0 - 1.0;
        mapped_normal = vec3<f32>(mapped_normal.xy * draw.material.z, mapped_normal.z);
        if dot(mapped_normal, mapped_normal) <= 0.000001 {
            mapped_normal = vec3<f32>(0.0, 0.0, 1.0);
        } else {
            mapped_normal = normalize(mapped_normal);
        }
        n = normalize(mat3x3<f32>(tangent, bitangent, geometric_normal) * mapped_normal);
    }
    var sampled_base = vec4<f32>(1.0);
    if ((texture_flags & 1u) != 0u) {
        sampled_base = textureSample(
            base_color_texture, base_color_sampler, input.base_tex_coord
        );
    }
    let base_color = sampled_base * draw.base_color;
    if (draw.alpha.x >= 0.0 && base_color.a < draw.alpha.x) { discard; }
    var sampled_mr = vec4<f32>(1.0);
    if ((texture_flags & 4u) != 0u) {
        sampled_mr = textureSample(
            metallic_roughness_texture, metallic_roughness_sampler,
            input.metallic_roughness_tex_coord
        );
    }
    let metallic = clamp(sampled_mr.b * draw.material.x, 0.0, 1.0);
    let roughness = max(clamp(sampled_mr.g * draw.material.y, 0.0, 1.0), 0.045);
    let v = normalize(camera.position.xyz - input.world_position);
    let l = normalize(-draw.light_direction.xyz);
    let h = normalize(v + l);
    let n_dot_v = max(dot(n, v), 0.0);
    let n_dot_l = max(dot(n, l), 0.0);
    let albedo = base_color.rgb;
    let f0 = mix(vec3<f32>(0.04), albedo, metallic);
    let f = fresnel_schlick(max(dot(h, v), 0.0), f0);
    let d = distribution_ggx(n, h, roughness);
    let g = geometry_schlick_ggx(n_dot_v, roughness) * geometry_schlick_ggx(n_dot_l, roughness);
    let specular = (d * g * f) / max(4.0 * n_dot_v * n_dot_l, 0.0001);
    let diffuse_weight = (vec3<f32>(1.0) - f) * (1.0 - metallic);
    let diffuse = diffuse_weight * albedo / PI;
    var shadow_visibility = 1.0;
    if (shadow.params.x > 0.5) {
        shadow_visibility = sample_directional_shadow(input.world_position);
    }
    let direct = (diffuse + specular) * draw.light_color.rgb * n_dot_l * shadow_visibility;
    let ibl_diffuse = diffuse_weight * albedo * diffuse_irradiance(n);
    var ibl_specular = vec3<f32>(0.0);
    let specular_ibl_strength = draw.alpha.y;
    if (specular_ibl_strength > 0.0) {
        let reflection = reflect(-v, n);
        let max_lod = f32(textureNumLevels(specular_cube) - 1u);
        let lod = roughness * max_lod;
        let prefiltered = textureSampleLevel(specular_cube, specular_sampler, reflection, lod).rgb;
        let brdf = textureSample(brdf_lut, brdf_sampler, vec2<f32>(n_dot_v, roughness)).rg;
        let fresnel_ibl = f0 + (max(vec3<f32>(1.0 - roughness), f0) - f0) * pow(1.0 - n_dot_v, 5.0);
        ibl_specular = prefiltered * (fresnel_ibl * brdf.x + brdf.y) * specular_ibl_strength;
    }
    var emissive = draw.emissive.rgb;
    if ((texture_flags & 8u) != 0u) {
        emissive *= textureSample(
            emissive_texture, emissive_sampler, input.emissive_tex_coord
        ).rgb;
    }
    return vec4<f32>(direct + ibl_diffuse + ibl_specular + emissive, base_color.a);
}
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pbr_material_validates_unit_factors_and_colours() {
        assert_eq!(
            PbrMaterial3d::new([1.0; 4], -0.1, 0.5),
            Err(PbrMaterialError::InvalidMetallic)
        );
        assert_eq!(
            PbrMaterial3d::new([1.0; 4], 0.5, 1.1),
            Err(PbrMaterialError::InvalidRoughness)
        );
        assert!(
            PbrMaterial3d::new([0.8, 0.2, 0.1, 1.0], 0.7, 0.25)
                .and_then(|material| material.with_emissive([0.1, 0.0, 0.0]))
                .is_ok()
        );
    }

    #[test]
    fn pbr_alpha_mask_is_typed_and_validated() {
        let mask = PbrAlphaMode3d::mask(0.42).expect("finite unit cutoff");
        assert_eq!(mask.cutoff(), Some(0.42));
        assert_eq!(PbrAlphaMode3d::Opaque.cutoff(), None);
        assert_eq!(
            PbrAlphaMode3d::mask(f32::NAN),
            Err(PbrMaterialError::InvalidAlphaCutoff)
        );
        assert_eq!(
            PbrAlphaMode3d::try_from(AlphaMode::Mask { cutoff: 1.1 }),
            Err(PbrMaterialError::InvalidAlphaCutoff)
        );
    }

    #[test]
    fn pbr_alpha_phase_contract_preserves_depth_writing_masks() {
        let mask = PbrAlphaMode3d::mask(0.5).expect("valid cutoff");
        assert_eq!(validate_alpha_phase(mask, false), Ok(()));
        assert_eq!(
            validate_alpha_phase(mask, true),
            Err(PbrMeshRenderError::TransparentPhaseRequiresBlend)
        );
        assert_eq!(
            validate_alpha_phase(PbrAlphaMode3d::Blend, false),
            Err(PbrMeshRenderError::BlendRequiresTransparentPhase)
        );
        assert_eq!(validate_alpha_phase(PbrAlphaMode3d::Blend, true), Ok(()));
    }

    #[test]
    fn diffuse_irradiance_sh_constant_reconstructs_every_normal() {
        let sh = DiffuseIrradianceSh3d::constant([0.3, 0.5, 0.7]).expect("valid irradiance");
        let coefficients = sh.coefficients();
        for channel in 0..3 {
            assert!(
                (coefficients[0][channel] * 0.282_094_8 - [0.3, 0.5, 0.7][channel]).abs()
                    < 0.000_001
            );
        }
        assert!(
            coefficients[1..]
                .iter()
                .flatten()
                .all(|value| *value == 0.0)
        );
        assert_eq!(
            DiffuseIrradianceSh3d::constant([-0.1, 0.0, 0.0]),
            Err(DiffuseIrradianceShError::InvalidConstant)
        );
        assert_eq!(
            DiffuseIrradianceSh3d::l2([[f32::NAN, 0.0, 0.0]; 9]),
            Err(DiffuseIrradianceShError::NonFiniteCoefficient)
        );
    }

    #[test]
    fn pbr_shader_contains_the_expected_microfacet_terms() {
        assert!(PBR_WGSL.contains("distribution_ggx"));
        assert!(PBR_WGSL.contains("geometry_schlick_ggx"));
        assert!(PBR_WGSL.contains("fresnel_schlick"));
        assert!(PBR_WGSL.contains("draw.base_color.a < draw.alpha.x"));
        assert!(PBR_WGSL.contains("diffuse_irradiance_sh"));
        assert!(PBR_WGSL.contains("ibl_diffuse = diffuse_weight * albedo"));
        assert!(PBR_WGSL.contains("draw.alpha.y"));
        assert!(PBR_WGSL.contains("texture_cube"));
        assert!(PBR_WGSL.contains("brdf_lut"));
        assert!(PBR_WGSL.contains("ibl_specular"));
        assert!(PBR_WGSL.contains("textureSampleLevel(specular_cube"));
    }

    #[test]
    fn textured_pbr_shader_uses_gltf_channel_and_tangent_conventions() {
        assert!(TEXTURED_PBR_WGSL.contains("sampled_mr.g * draw.material.y"));
        assert!(TEXTURED_PBR_WGSL.contains("sampled_mr.b * draw.material.x"));
        assert!(
            TEXTURED_PBR_WGSL.contains("input.tangent.w * draw.material.w"),
            "mirrored transforms must flip tangent-space handedness"
        );
        assert!(TEXTURED_PBR_WGSL.contains("normal_texture, normal_sampler"));
        assert!(TEXTURED_PBR_WGSL.contains("emissive_texture, emissive_sampler"));
        assert!(TEXTURED_PBR_WGSL.contains("input.normal_tex_coord"));
        assert!(TEXTURED_PBR_WGSL.contains("input.metallic_roughness_tex_coord"));
        assert!(TEXTURED_PBR_WGSL.contains("base_color.a < draw.alpha.x"));
        assert!(TEXTURED_PBR_WGSL.contains("ibl_diffuse = diffuse_weight * albedo"));
        assert!(TEXTURED_PBR_WGSL.contains("@group(3)"));
        assert!(TEXTURED_PBR_WGSL.contains("ibl_specular"));
        assert!(TEXTURED_PBR_WGSL.contains("draw.alpha.y"));
    }

    #[test]
    fn partial_pbr_texture_presence_has_stable_shader_bits() {
        assert_eq!(PbrTexturePresence3d::complete().bits(), 0b1111);
        assert_eq!(
            PbrTexturePresence3d::from_channels([true, false, false, true]).bits(),
            0b1001
        );
        assert!(TEXTURED_PBR_WGSL.contains("(texture_flags & 1u)"));
        assert!(TEXTURED_PBR_WGSL.contains("(texture_flags & 2u)"));
        assert!(TEXTURED_PBR_WGSL.contains("(texture_flags & 4u)"));
        assert!(TEXTURED_PBR_WGSL.contains("(texture_flags & 8u)"));
    }
}
