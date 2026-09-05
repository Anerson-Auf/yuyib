//! Directional shadow-map MVP (M2.3).
//!
//! Orthographic depth map(s) from a directional light, sampled with a
//! comparison sampler and 3×3 PCF in PBR. Up to two cascades
//! (`texture_depth_2d_array`): a tight near slab for street density and an
//! optional far slab for coverage. Focus XZ snaps to the near-cascade texel
//! grid (Valient/Bevy-style). Skinned casters remain a later slice. Alpha-mask
//! materials discard in the caster fragment stage so cutouts do not cast solid
//! blobs. The shadow target is a caller-owned `Depth32Float` texture — it never
//! touches the surface depth attachment.

use crate::{
    GpuPbrMesh, GpuTexturedPbrMaterial, GpuTexturedPbrMesh, LIT_VERTEX_LAYOUT, all_finite,
    normalize3,
};
use crate::pbr::{TEXTURED_PBR_VERTEX_LAYOUT, textured_pbr_material_layout};

use std::{error::Error, fmt, mem::size_of};

use bytemuck::{Pod, Zeroable};
use yuyib_render::{RenderFrame, Renderer, wgpu};

/// Maximum cascade layers stored in the shadow depth array.
pub const DIRECTIONAL_SHADOW_MAX_CASCADES: u32 = 2;

const SHADOW_CASTER_FACTOR_WGSL: &str = r#"
struct ShadowDraw {
    light_view_proj: mat4x4<f32>,
    model: mat4x4<f32>,
    base_color: vec4<f32>,
    alpha_cutoff: vec4<f32>,
};
@group(0) @binding(0) var<uniform> draw: ShadowDraw;
struct VertexInput { @location(0) position: vec3<f32>, @location(1) normal: vec3<f32>, };
struct VertexOutput { @builtin(position) clip_position: vec4<f32>, };
@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    let world = draw.model * vec4<f32>(input.position, 1.0);
    output.clip_position = draw.light_view_proj * world;
    return output;
}
@fragment
fn fs_main(_input: VertexOutput) {
    if (draw.alpha_cutoff.x >= 0.0 && draw.base_color.a < draw.alpha_cutoff.x) {
        discard;
    }
}
"#;

const SHADOW_CASTER_TEXTURED_WGSL: &str = r#"
struct ShadowDraw {
    light_view_proj: mat4x4<f32>,
    model: mat4x4<f32>,
    base_color: vec4<f32>,
    alpha_cutoff: vec4<f32>,
};
@group(0) @binding(0) var<uniform> draw: ShadowDraw;
@group(1) @binding(0) var base_color_texture: texture_2d<f32>;
@group(1) @binding(1) var base_color_sampler: sampler;
struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) tangent: vec4<f32>,
    @location(3) base_tex_coord: vec2<f32>,
    @location(4) normal_tex_coord: vec2<f32>,
    @location(5) metallic_roughness_tex_coord: vec2<f32>,
    @location(6) emissive_tex_coord: vec2<f32>,
};
struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) base_tex_coord: vec2<f32>,
};
@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    let world = draw.model * vec4<f32>(input.position, 1.0);
    output.clip_position = draw.light_view_proj * world;
    output.base_tex_coord = input.base_tex_coord;
    return output;
}
@fragment
fn fs_main(input: VertexOutput) {
    let sampled = textureSample(base_color_texture, base_color_sampler, input.base_tex_coord);
    let alpha = sampled.a * draw.base_color.a;
    if (draw.alpha_cutoff.x >= 0.0 && alpha < draw.alpha_cutoff.x) {
        discard;
    }
}
"#;

/// Failure while validating a directional shadow configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DirectionalShadowError {
    /// Map edge must be a positive power of two.
    InvalidResolution,
    /// Focus half-extent must be finite and positive on every axis.
    InvalidFocusExtent,
    /// Focus centre contained a non-finite value.
    InvalidFocusCenter,
    /// Light direction was non-finite or too small to normalize.
    InvalidLightDirection,
    /// Depth bias was non-finite or negative.
    InvalidDepthBias,
}

impl fmt::Display for DirectionalShadowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidResolution => {
                formatter.write_str("shadow map resolution must be a positive power of two")
            }
            Self::InvalidFocusExtent => {
                formatter.write_str("shadow focus extent must be finite and positive")
            }
            Self::InvalidFocusCenter => {
                formatter.write_str("shadow focus centre must be finite")
            }
            Self::InvalidLightDirection => {
                formatter.write_str("shadow light direction must be finite and non-zero")
            }
            Self::InvalidDepthBias => {
                formatter.write_str("shadow depth bias must be finite and non-negative")
            }
        }
    }
}

impl Error for DirectionalShadowError {}

/// Orthographic focus volume and map settings for one directional light.
///
/// When [`Self::far_half_extent`] is set, the GPU allocates a 2-layer depth
/// array: cascade 0 uses `focus_half_extent` (near), cascade 1 uses the far
/// extent. Sampling prefers the near cascade when the receiver projects inside
/// it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DirectionalShadowConfig {
    resolution: u32,
    focus_center: [f32; 3],
    focus_half_extent: [f32; 3],
    far_half_extent: Option<[f32; 3]>,
    depth_bias: f32,
}

impl DirectionalShadowConfig {
    /// Creates a validated single-cascade shadow-map configuration.
    ///
    /// `focus_half_extent` is the orthographic half-size around `focus_center`
    /// in world units (X/Y = map plane, Z = near/far thickness along the light).
    ///
    /// # Errors
    ///
    /// Returns [`DirectionalShadowError`] for invalid resolution, extent, centre,
    /// or bias.
    pub fn new(
        resolution: u32,
        focus_center: [f32; 3],
        focus_half_extent: [f32; 3],
        depth_bias: f32,
    ) -> Result<Self, DirectionalShadowError> {
        if resolution == 0 || !resolution.is_power_of_two() {
            return Err(DirectionalShadowError::InvalidResolution);
        }
        if !all_finite(&focus_center) {
            return Err(DirectionalShadowError::InvalidFocusCenter);
        }
        if !all_finite(&focus_half_extent)
            || focus_half_extent.iter().any(|axis| *axis <= 0.0)
        {
            return Err(DirectionalShadowError::InvalidFocusExtent);
        }
        if !depth_bias.is_finite() || depth_bias < 0.0 {
            return Err(DirectionalShadowError::InvalidDepthBias);
        }
        Ok(Self {
            resolution,
            focus_center,
            focus_half_extent,
            far_half_extent: None,
            depth_bias,
        })
    }

    /// Enables a second, wider cascade around the same focus centre.
    ///
    /// # Errors
    ///
    /// Returns [`DirectionalShadowError::InvalidFocusExtent`] when any axis is
    /// non-finite or non-positive.
    pub fn with_far_cascade(
        mut self,
        far_half_extent: [f32; 3],
    ) -> Result<Self, DirectionalShadowError> {
        if !all_finite(&far_half_extent) || far_half_extent.iter().any(|axis| *axis <= 0.0) {
            return Err(DirectionalShadowError::InvalidFocusExtent);
        }
        self.far_half_extent = Some(far_half_extent);
        Ok(self)
    }

    /// Compact smoke preset: 512² map around the origin (single cascade).
    #[must_use]
    pub fn smoke() -> Self {
        Self::new(512, [0.0, 0.35, 0.0], [3.5, 3.5, 6.0], 0.002).expect("smoke config")
    }

    /// Map edge length in texels.
    #[must_use]
    pub const fn resolution(self) -> u32 {
        self.resolution
    }

    /// World-space focus centre.
    #[must_use]
    pub const fn focus_center(self) -> [f32; 3] {
        self.focus_center
    }

    /// Near-cascade orthographic half-extents.
    #[must_use]
    pub const fn focus_half_extent(self) -> [f32; 3] {
        self.focus_half_extent
    }

    /// Far-cascade half-extents when two cascades are enabled.
    #[must_use]
    pub const fn far_half_extent(self) -> Option<[f32; 3]> {
        self.far_half_extent
    }

    /// Number of cascade layers (1 or 2).
    #[must_use]
    pub const fn cascade_count(self) -> u32 {
        if self.far_half_extent.is_some() {
            2
        } else {
            1
        }
    }

    /// Half-extents used for CPU caster coverage (union of cascades).
    #[must_use]
    pub fn coverage_half_extent(self) -> [f32; 3] {
        match self.far_half_extent {
            Some(far) => [
                self.focus_half_extent[0].max(far[0]),
                self.focus_half_extent[1].max(far[1]),
                self.focus_half_extent[2].max(far[2]),
            ],
            None => self.focus_half_extent,
        }
    }

    /// Constant depth bias applied when sampling.
    #[must_use]
    pub const fn depth_bias(self) -> f32 {
        self.depth_bias
    }

    /// Returns a copy with a different focus centre.
    ///
    /// # Errors
    ///
    /// Returns [`DirectionalShadowError::InvalidFocusCenter`] for non-finite values.
    pub fn with_focus_center(mut self, focus_center: [f32; 3]) -> Result<Self, DirectionalShadowError> {
        if !all_finite(&focus_center) {
            return Err(DirectionalShadowError::InvalidFocusCenter);
        }
        self.focus_center = focus_center;
        Ok(self)
    }
}

/// Camera-follow directional shadow policy for [`crate::Game3dScene`].
///
/// Each frame rebuilds the light VP around a **target-locked** focus (follow
/// pivot / character). Camera orbit must not move the slab — otherwise casters
/// and receivers churn every frame and shadows appear to "randomly" pop.
/// Focus XZ snaps to the near-cascade texel size by default (stable CSM practice).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DirectionalShadowPolicy {
    resolution: u32,
    focus_half_extent: [f32; 3],
    far_half_extent: Option<[f32; 3]>,
    depth_bias: f32,
    /// Quantize focus XZ to this cell size in world units.
    /// `0` = snap to one near-cascade texel (`2 * max(hx, hy) / resolution`).
    focus_snap_xz: f32,
}

impl DirectionalShadowPolicy {
    /// Creates a validated target-locked policy (single cascade).
    ///
    /// # Errors
    ///
    /// Returns [`DirectionalShadowError`] for invalid resolution, extent, bias,
    /// or snap size.
    pub fn new(
        resolution: u32,
        focus_half_extent: [f32; 3],
        depth_bias: f32,
        focus_snap_xz: f32,
    ) -> Result<Self, DirectionalShadowError> {
        let _ = DirectionalShadowConfig::new(resolution, [0.0; 3], focus_half_extent, depth_bias)?;
        if !focus_snap_xz.is_finite() || focus_snap_xz < 0.0 {
            return Err(DirectionalShadowError::InvalidFocusCenter);
        }
        Ok(Self {
            resolution,
            focus_half_extent,
            far_half_extent: None,
            depth_bias,
            focus_snap_xz,
        })
    }

    /// Adds a wider far cascade (street / outdoor coverage).
    ///
    /// # Errors
    ///
    /// Returns [`DirectionalShadowError::InvalidFocusExtent`] for invalid extents.
    pub fn with_far_cascade(
        mut self,
        far_half_extent: [f32; 3],
    ) -> Result<Self, DirectionalShadowError> {
        let _ = DirectionalShadowConfig::new(
            self.resolution,
            [0.0; 3],
            far_half_extent,
            self.depth_bias,
        )?;
        self.far_half_extent = Some(far_half_extent);
        Ok(self)
    }

    /// Street-city default: single 1024² slab (~80 m) locked to the follow target.
    ///
    /// Multi-cascade remains available via [`Self::with_far_cascade`], but street
    /// playable stays on one map until CSM is validated in-engine — nested ortho
    /// cascades previously produced empty near maps and invisible shadows.
    #[must_use]
    pub fn street_city() -> Self {
        Self::new(1024, [40.0, 40.0, 70.0], 0.002, 0.0).expect("street-city shadow policy")
    }

    /// World-space size of one near-cascade shadow-map texel.
    #[must_use]
    pub fn texel_world_size(self) -> f32 {
        shadow_texel_world_size(self.resolution, self.focus_half_extent)
    }

    /// Builds a concrete config focused on `camera.target` (not the view ray).
    ///
    /// # Errors
    ///
    /// Returns [`DirectionalShadowError`] when the focus centre is non-finite.
    pub fn config_for_camera(self, camera: crate::Camera3d) -> Result<DirectionalShadowConfig, DirectionalShadowError> {
        let snap = if self.focus_snap_xz > 0.0 {
            self.focus_snap_xz
        } else {
            self.texel_world_size()
        };
        let focus = [
            snap_axis(camera.target[0], snap),
            camera.target[1],
            snap_axis(camera.target[2], snap),
        ];
        let mut config = DirectionalShadowConfig::new(
            self.resolution,
            focus,
            self.focus_half_extent,
            self.depth_bias,
        )?;
        if let Some(far) = self.far_half_extent {
            config = config.with_far_cascade(far)?;
        }
        Ok(config)
    }

    /// Map edge length in texels.
    #[must_use]
    pub const fn resolution(self) -> u32 {
        self.resolution
    }
}

/// World-space texel size for an orthographic map of `resolution` covering
/// `2 * max(hx, hy)` metres on the light plane.
#[must_use]
pub fn shadow_texel_world_size(resolution: u32, focus_half_extent: [f32; 3]) -> f32 {
    let diameter = 2.0 * focus_half_extent[0].max(focus_half_extent[1]);
    if resolution == 0 || !diameter.is_finite() || diameter <= 0.0 {
        0.0
    } else {
        diameter / resolution as f32
    }
}

/// Conservative world-space coverage test for caster selection helpers.
///
/// Uses the union of near/far cascade extents so casters that only matter for
/// the far map are still submitted.
#[must_use]
pub fn shadow_coverage_contains(
    point: [f32; 3],
    config: DirectionalShadowConfig,
    padding: f32,
) -> bool {
    let focus = config.focus_center();
    let half = config.coverage_half_extent();
    let horizontal = half[0].max(half[1]) + padding;
    let vertical = half[2] + padding;
    (point[0] - focus[0]).abs() <= horizontal
        && (point[2] - focus[2]).abs() <= horizontal
        && (point[1] - focus[1]).abs() <= vertical
}

fn snap_axis(value: f32, cell: f32) -> f32 {
    if cell <= 0.0 {
        value
    } else {
        (value / cell).round() * cell
    }
}

/// GPU directional shadow map + sampling bind group.
pub struct GpuDirectionalShadow {
    resolution: u32,
    depth_bias: f32,
    cascade_count: u32,
    focus_center: [f32; 3],
    cascade_half_extents: [[f32; 3]; DIRECTIONAL_SHADOW_MAX_CASCADES as usize],
    light_view_projs: [[f32; 16]; DIRECTIONAL_SHADOW_MAX_CASCADES as usize],
    _texture: wgpu::Texture,
    /// Full array view for PBR sampling (`texture_depth_2d_array`).
    array_view: wgpu::TextureView,
    /// Per-cascade render attachments.
    layer_views: [wgpu::TextureView; DIRECTIONAL_SHADOW_MAX_CASCADES as usize],
    sampler: wgpu::Sampler,
    params_buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

impl GpuDirectionalShadow {
    /// Allocates a depth map and sampling resources for `config`.
    ///
    /// # Errors
    ///
    /// Returns [`DirectionalShadowError`] when the light direction is invalid.
    pub fn create(
        renderer: &Renderer,
        config: DirectionalShadowConfig,
        light_direction_toward_scene: [f32; 3],
    ) -> Result<Self, DirectionalShadowError> {
        renderer.with_raw_gpu(|device, queue, _configuration| {
            Self::create_with(device, queue, config, light_direction_toward_scene)
        })
    }

    /// Allocates through a frame's device/queue.
    ///
    /// # Errors
    ///
    /// Returns [`DirectionalShadowError`] when the light direction is invalid.
    pub fn create_for_frame(
        frame: &RenderFrame<'_>,
        config: DirectionalShadowConfig,
        light_direction_toward_scene: [f32; 3],
    ) -> Result<Self, DirectionalShadowError> {
        Self::create_with(
            frame.device(),
            frame.queue(),
            config,
            light_direction_toward_scene,
        )
    }

    /// Map edge length in texels.
    #[must_use]
    pub const fn resolution(&self) -> u32 {
        self.resolution
    }

    /// Active cascade count (1 or 2).
    #[must_use]
    pub const fn cascade_count(&self) -> u32 {
        self.cascade_count
    }

    /// Receiver depth bias used by sampling passes sharing this shadow map.
    #[must_use]
    pub const fn depth_bias(&self) -> f32 {
        self.depth_bias
    }

    /// Column-major light view-projection for cascade 0 (smoke / single-cascade).
    #[must_use]
    pub const fn light_view_proj(&self) -> [f32; 16] {
        self.light_view_projs[0]
    }

    /// Column-major light view-projection for `cascade` (clamped to active count).
    #[must_use]
    pub fn light_view_proj_cascade(&self, cascade: u32) -> [f32; 16] {
        let index = cascade.min(self.cascade_count.saturating_sub(1)) as usize;
        self.light_view_projs[index]
    }

    /// Bind group for factor-only PBR `@group(3)` (map + compare sampler + params).
    #[must_use]
    pub const fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind_group
    }

    /// Depth array view for PBR sampling.
    #[must_use]
    pub const fn depth_view(&self) -> &wgpu::TextureView {
        &self.array_view
    }

    /// Per-cascade depth attachment for caster passes.
    #[must_use]
    pub fn cascade_depth_view(&self, cascade: u32) -> &wgpu::TextureView {
        let index = cascade.min(self.cascade_count.saturating_sub(1)) as usize;
        &self.layer_views[index]
    }

    /// Rewrites light view-projections and sampling params in place.
    ///
    /// The depth texture is kept; callers must re-draw casters after this.
    ///
    /// # Errors
    ///
    /// Returns [`DirectionalShadowError`] when the light direction is invalid or
    /// `config.resolution` / cascade count do not match this map.
    pub fn update_light(
        &mut self,
        queue: &wgpu::Queue,
        config: DirectionalShadowConfig,
        light_direction_toward_scene: [f32; 3],
    ) -> Result<(), DirectionalShadowError> {
        if config.resolution != self.resolution || config.cascade_count() != self.cascade_count {
            return Err(DirectionalShadowError::InvalidResolution);
        }
        let (light_view_projs, cascade_count) =
            cascade_light_matrices(light_direction_toward_scene, config)?;
        self.light_view_projs = light_view_projs;
        self.cascade_count = cascade_count;
        self.focus_center = config.focus_center();
        self.cascade_half_extents = cascade_half_extents(config);
        self.depth_bias = config.depth_bias;
        queue.write_buffer(
            &self.params_buffer,
            0,
            bytemuck::bytes_of(&sample_uniform(
                &light_view_projs,
                cascade_count,
                self.resolution,
                config.depth_bias,
                true,
            )),
        );
        Ok(())
    }

    pub(crate) fn bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("yuyib directional shadow layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT | wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(
                            size_of::<ShadowSampleUniform>() as u64,
                        ),
                    },
                    count: None,
                },
            ],
        })
    }

    /// Builds a 1×1 always-lit placeholder (depth = 1.0, sampling disabled).
    #[must_use]
    pub(crate) fn neutral_lit(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let config = DirectionalShadowConfig::new(1, [0.0; 3], [1.0; 3], 0.0)
            .expect("neutral shadow config");
        Self::create_with(device, queue, config, [0.0, -1.0, 0.0])
            .expect("neutral shadow create")
            .with_enabled(device, queue, false)
    }

    fn with_enabled(self, device: &wgpu::Device, queue: &wgpu::Queue, enabled: bool) -> Self {
        queue.write_buffer(
            &self.params_buffer,
            0,
            bytemuck::bytes_of(&sample_uniform(
                &self.light_view_projs,
                self.cascade_count,
                self.resolution,
                self.depth_bias,
                enabled,
            )),
        );
        let _ = device;
        self
    }

    fn create_with(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        config: DirectionalShadowConfig,
        light_direction_toward_scene: [f32; 3],
    ) -> Result<Self, DirectionalShadowError> {
        let (light_view_projs, cascade_count) =
            cascade_light_matrices(light_direction_toward_scene, config)?;
        let layers = cascade_count.max(1);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("yuyib directional shadow map"),
            size: wgpu::Extent3d {
                width: config.resolution,
                height: config.resolution,
                depth_or_array_layers: layers,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let array_view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("yuyib directional shadow array view"),
            format: None,
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            aspect: wgpu::TextureAspect::All,
            base_mip_level: 0,
            mip_level_count: Some(1),
            base_array_layer: 0,
            array_layer_count: Some(layers),
            usage: None,
        });
        let layer_views = std::array::from_fn(|layer| {
            let layer_index = (layer as u32).min(layers - 1);
            texture.create_view(&wgpu::TextureViewDescriptor {
                label: Some("yuyib directional shadow cascade layer"),
                format: None,
                dimension: Some(wgpu::TextureViewDimension::D2),
                aspect: wgpu::TextureAspect::All,
                base_mip_level: 0,
                mip_level_count: Some(1),
                base_array_layer: layer_index,
                array_layer_count: Some(1),
                usage: None,
            })
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("yuyib directional shadow compare sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });
        let enabled = config.resolution > 1;
        let uniform = sample_uniform(
            &light_view_projs,
            cascade_count,
            config.resolution,
            config.depth_bias,
            enabled,
        );
        let params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("yuyib directional shadow params"),
            size: size_of::<ShadowSampleUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&params_buffer, 0, bytemuck::bytes_of(&uniform));
        let layout = Self::bind_group_layout(device);
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("yuyib directional shadow bind group"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&array_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: params_buffer.as_entire_binding(),
                },
            ],
        });
        Ok(Self {
            resolution: config.resolution,
            depth_bias: config.depth_bias,
            cascade_count,
            focus_center: config.focus_center(),
            cascade_half_extents: cascade_half_extents(config),
            light_view_projs,
            _texture: texture,
            array_view,
            layer_views,
            sampler,
            params_buffer,
            bind_group,
        })
    }

    pub(crate) const fn compare_sampler(&self) -> &wgpu::Sampler {
        &self.sampler
    }

    pub(crate) const fn params_buffer(&self) -> &wgpu::Buffer {
        &self.params_buffer
    }

    /// Byte size of the shadow sampling uniform (cascade VPs + params).
    #[must_use]
    pub(crate) const fn sample_uniform_size() -> u64 {
        size_of::<ShadowSampleUniform>() as u64
    }
}

fn sample_uniform(
    light_view_projs: &[[f32; 16]; DIRECTIONAL_SHADOW_MAX_CASCADES as usize],
    cascade_count: u32,
    resolution: u32,
    depth_bias: f32,
    enabled: bool,
) -> ShadowSampleUniform {
    ShadowSampleUniform {
        light_view_proj_0: light_view_projs[0],
        light_view_proj_1: light_view_projs[1],
        params: [
            if enabled { 1.0 } else { 0.0 },
            depth_bias,
            1.0 / resolution.max(1) as f32,
            cascade_count as f32,
        ],
    }
}

fn cascade_half_extents(
    config: DirectionalShadowConfig,
) -> [[f32; 3]; DIRECTIONAL_SHADOW_MAX_CASCADES as usize] {
    let near = config.focus_half_extent();
    let far = config.far_half_extent().unwrap_or(near);
    [near, far]
}

fn cascade_light_matrices(
    light_direction_toward_scene: [f32; 3],
    config: DirectionalShadowConfig,
) -> Result<([[f32; 16]; DIRECTIONAL_SHADOW_MAX_CASCADES as usize], u32), DirectionalShadowError>
{
    let near = light_orthographic_view_projection(light_direction_toward_scene, config)?;
    let mut matrices = [near, near];
    let mut count = 1u32;
    if let Some(far_extent) = config.far_half_extent() {
        let far_config = DirectionalShadowConfig::new(
            config.resolution(),
            config.focus_center(),
            far_extent,
            config.depth_bias(),
        )?;
        matrices[1] = light_orthographic_view_projection(light_direction_toward_scene, far_config)?;
        count = 2;
    }
    Ok((matrices, count))
}

/// One factor-only shadow caster draw (mesh + model + mask inputs).
#[derive(Clone, Copy)]
pub struct FactorShadowCasterDraw<'a> {
    /// Factor PBR mesh.
    pub mesh: &'a GpuPbrMesh,
    /// Column-major model matrix.
    pub model_matrix: [f32; 16],
    /// Material base colour (alpha participates in MASK).
    pub base_color: [f32; 4],
    /// `Mask` cutoff, or `-1.0` to disable discard.
    pub alpha_cutoff: f32,
}

/// One textured shadow caster draw (mesh + material + model + mask inputs).
#[derive(Clone, Copy)]
pub struct TexturedShadowCasterDraw<'a> {
    /// Textured PBR mesh.
    pub mesh: &'a GpuTexturedPbrMesh,
    /// Material bind group (base colour sampled for MASK).
    pub material: &'a GpuTexturedPbrMaterial,
    /// Column-major model matrix.
    pub model_matrix: [f32; 16],
    /// Material base colour factor (alpha multiplies sampled alpha).
    pub base_color: [f32; 4],
    /// `Mask` cutoff, or `-1.0` to disable discard.
    pub alpha_cutoff: f32,
}

/// Depth-only caster drawer for factor and textured PBR geometry.
pub struct DirectionalShadowCaster3d {
    factor_pipeline: wgpu::RenderPipeline,
    textured_pipeline: wgpu::RenderPipeline,
    draw_buffer: wgpu::Buffer,
    draw_bind_group: wgpu::BindGroup,
}

impl DirectionalShadowCaster3d {
    /// Creates a caster pipeline for `renderer`'s depth format.
    #[must_use]
    pub fn new(renderer: &Renderer) -> Self {
        let depth_format = renderer.depth_format();
        renderer.with_raw_gpu(|device, _queue, _configuration| Self::create(device, depth_format))
    }

    /// Creates a caster pipeline from a currently-recording frame.
    #[must_use]
    pub fn new_for_frame(frame: &RenderFrame<'_>) -> Self {
        Self::create(frame.device(), frame.depth_format())
    }

    /// Clears every cascade and draws factor-only casters into each layer.
    ///
    /// Separate passes avoid stomping a shared model uniform inside one WGPU
    /// render pass. Empty `casters` still clears each cascade to the far plane.
    pub fn draw_casters(
        &self,
        frame: &mut RenderFrame<'_>,
        shadow: &GpuDirectionalShadow,
        casters: &[FactorShadowCasterDraw<'_>],
    ) {
        self.draw_opaque_casters(frame, shadow, casters, &[]);
    }

    /// Draws textured PBR casters with optional alpha-mask discard.
    ///
    /// When `clear_first` is true, each cascade is cleared before drawing.
    /// Prefer [`Self::draw_opaque_casters`] when both factor and textured lists
    /// are available — it avoids a wasted clear pass.
    pub fn draw_textured_casters(
        &self,
        frame: &mut RenderFrame<'_>,
        shadow: &GpuDirectionalShadow,
        casters: &[TexturedShadowCasterDraw<'_>],
        clear_first: bool,
    ) {
        if clear_first {
            self.draw_opaque_casters(frame, shadow, &[], casters);
        } else {
            let textured: Vec<_> = casters.iter().copied().map(CasterDraw::Textured).collect();
            self.draw_cascades(frame, shadow, &[], &textured, false);
        }
    }

    /// Clears each cascade once, then draws factor + textured casters.
    pub fn draw_opaque_casters(
        &self,
        frame: &mut RenderFrame<'_>,
        shadow: &GpuDirectionalShadow,
        factor: &[FactorShadowCasterDraw<'_>],
        textured: &[TexturedShadowCasterDraw<'_>],
    ) {
        let factor: Vec<_> = factor.iter().copied().map(CasterDraw::Factor).collect();
        let textured: Vec<_> = textured.iter().copied().map(CasterDraw::Textured).collect();
        self.draw_cascades(frame, shadow, &factor, &textured, true);
    }

    fn draw_cascades(
        &self,
        frame: &mut RenderFrame<'_>,
        shadow: &GpuDirectionalShadow,
        factor: &[CasterDraw<'_>],
        textured: &[CasterDraw<'_>],
        clear_first: bool,
    ) {
        for cascade in 0..shadow.cascade_count() {
            let depth_view = shadow.cascade_depth_view(cascade);
            let light_vp = shadow.light_view_proj_cascade(cascade);
            if factor.is_empty() && textured.is_empty() {
                if clear_first {
                    frame.with_depth_only_pass(depth_view, wgpu::LoadOp::Clear(1.0), |_| {});
                }
                continue;
            }
            let mut first = clear_first;
            for draw in factor.iter().chain(textured.iter()) {
                let depth_load = if first {
                    first = false;
                    wgpu::LoadOp::Clear(1.0)
                } else {
                    wgpu::LoadOp::Load
                };
                self.encode_caster(frame, depth_view, light_vp, draw, depth_load);
            }
            if first {
                frame.with_depth_only_pass(depth_view, wgpu::LoadOp::Clear(1.0), |_| {});
            }
        }
    }

    fn encode_caster(
        &self,
        frame: &mut RenderFrame<'_>,
        depth_view: &wgpu::TextureView,
        light_view_proj: [f32; 16],
        draw: &CasterDraw<'_>,
        depth_load: wgpu::LoadOp<f32>,
    ) {
        let (pipeline, vertex, index, index_count, model, base_color, cutoff, material) =
            match draw {
                CasterDraw::Factor(item) => (
                    &self.factor_pipeline,
                    item.mesh.0.vertex_buffer.slice(..),
                    item.mesh.0.index_buffer.slice(..),
                    item.mesh.0.index_count,
                    item.model_matrix,
                    item.base_color,
                    item.alpha_cutoff,
                    None,
                ),
                CasterDraw::Textured(item) => (
                    &self.textured_pipeline,
                    item.mesh.vertex_buffer.slice(..),
                    item.mesh.index_buffer.slice(..),
                    item.mesh.index_count,
                    item.model_matrix,
                    item.base_color,
                    item.alpha_cutoff,
                    Some(item.material),
                ),
            };
        let uniform = ShadowCasterUniform {
            light_view_proj,
            model,
            base_color,
            alpha_cutoff: [cutoff, 0.0, 0.0, 0.0],
        };
        frame
            .queue()
            .write_buffer(&self.draw_buffer, 0, bytemuck::bytes_of(&uniform));
        frame.with_depth_only_pass(depth_view, depth_load, |pass| {
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, Some(&self.draw_bind_group), &[]);
            if let Some(material) = material {
                pass.set_bind_group(1, Some(material.bind_group()), &[]);
            }
            pass.set_vertex_buffer(0, vertex);
            pass.set_index_buffer(index, wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..index_count, 0, 0..1);
        });
    }

    fn create(device: &wgpu::Device, depth_format: wgpu::TextureFormat) -> Self {
        let factor_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("yuyib directional shadow caster factor WGSL"),
            source: wgpu::ShaderSource::Wgsl(SHADOW_CASTER_FACTOR_WGSL.into()),
        });
        let textured_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("yuyib directional shadow caster textured WGSL"),
            source: wgpu::ShaderSource::Wgsl(SHADOW_CASTER_TEXTURED_WGSL.into()),
        });
        let draw_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("yuyib shadow caster draw layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(size_of::<ShadowCasterUniform>() as u64),
                },
                count: None,
            }],
        });
        let material_layout = textured_pbr_material_layout(device);
        let factor_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("yuyib shadow caster factor pipeline layout"),
            bind_group_layouts: &[Some(&draw_layout)],
            immediate_size: 0,
        });
        let textured_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("yuyib shadow caster textured pipeline layout"),
                bind_group_layouts: &[Some(&draw_layout), Some(&material_layout)],
                immediate_size: 0,
            });
        let depth_stencil = wgpu::DepthStencilState {
            format: depth_format,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState {
                constant: 2,
                slope_scale: 1.5,
                clamp: 0.0,
            },
        };
        let make_pipeline =
            |label, layout, shader, buffers| {
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some(label),
                    layout: Some(layout),
                    vertex: wgpu::VertexState {
                        module: shader,
                        entry_point: Some("vs_main"),
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        buffers: &[Some(buffers)],
                    },
                    primitive: wgpu::PrimitiveState {
                        topology: wgpu::PrimitiveTopology::TriangleList,
                        front_face: wgpu::FrontFace::Ccw,
                        cull_mode: Some(wgpu::Face::Back),
                        ..Default::default()
                    },
                    depth_stencil: Some(depth_stencil.clone()),
                    multisample: wgpu::MultisampleState::default(),
                    fragment: Some(wgpu::FragmentState {
                        module: shader,
                        entry_point: Some("fs_main"),
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        targets: &[],
                    }),
                    multiview_mask: None,
                    cache: None,
                })
            };
        let factor_pipeline = make_pipeline(
            "yuyib shadow caster factor",
            &factor_pipeline_layout,
            &factor_shader,
            LIT_VERTEX_LAYOUT,
        );
        let textured_pipeline = make_pipeline(
            "yuyib shadow caster textured",
            &textured_pipeline_layout,
            &textured_shader,
            TEXTURED_PBR_VERTEX_LAYOUT,
        );
        let draw_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("yuyib shadow caster draw buffer"),
            size: size_of::<ShadowCasterUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let draw_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("yuyib shadow caster draw bind group"),
            layout: &draw_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: draw_buffer.as_entire_binding(),
            }],
        });
        Self {
            factor_pipeline,
            textured_pipeline,
            draw_buffer,
            draw_bind_group,
        }
    }
}

enum CasterDraw<'a> {
    Factor(FactorShadowCasterDraw<'a>),
    Textured(TexturedShadowCasterDraw<'a>),
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ShadowSampleUniform {
    light_view_proj_0: [f32; 16],
    light_view_proj_1: [f32; 16],
    /// x = enabled, y = depth bias, z = 1/resolution, w = cascade count.
    params: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ShadowCasterUniform {
    light_view_proj: [f32; 16],
    model: [f32; 16],
    base_color: [f32; 4],
    alpha_cutoff: [f32; 4],
}

fn light_orthographic_view_projection(
    light_direction_toward_scene: [f32; 3],
    config: DirectionalShadowConfig,
) -> Result<[f32; 16], DirectionalShadowError> {
    let forward = normalize3(light_direction_toward_scene)
        .ok_or(DirectionalShadowError::InvalidLightDirection)?;
    // Prefer world +Y as up; fall back to +Z when the light is nearly vertical.
    let up_hint = if forward[1].abs() > 0.95 {
        [0.0, 0.0, 1.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    let side = normalize3(cross3(forward, up_hint))
        .ok_or(DirectionalShadowError::InvalidLightDirection)?;
    let actual_up = cross3(side, forward);
    let center = config.focus_center;
    let pullback = config.focus_half_extent[2];
    let eye = [
        center[0] - forward[0] * pullback,
        center[1] - forward[1] * pullback,
        center[2] - forward[2] * pullback,
    ];
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
        -dot3(side, eye),
        -dot3(actual_up, eye),
        dot3(forward, eye),
        1.0,
    ];
    let hx = config.focus_half_extent[0];
    let hy = config.focus_half_extent[1];
    let hz = config.focus_half_extent[2];
    // View space uses Camera3d's convention (looks down −Z), so points in
    // front of the light have negative view Z. Map view_z ∈ [−2hz, 0] → depth
    // 1‥0 so nearer casters store smaller Depth32Float values.
    let projection = [
        1.0 / hx,
        0.0,
        0.0,
        0.0,
        0.0,
        1.0 / hy,
        0.0,
        0.0,
        0.0,
        0.0,
        -1.0 / (hz * 2.0),
        0.0,
        0.0,
        0.0,
        0.0,
        1.0,
    ];
    let matrix = multiply_matrix4(projection, view);
    if !all_finite(&matrix) {
        return Err(DirectionalShadowError::InvalidLightDirection);
    }
    Ok(matrix)
}

fn cross3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0].mul_add(b[0], a[1].mul_add(b[1], a[2] * b[2]))
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
    use super::{
        DirectionalShadowConfig, DirectionalShadowError, DirectionalShadowPolicy,
        light_orthographic_view_projection,
    };

    #[test]
    fn rejects_non_pow2_resolution() {
        assert_eq!(
            DirectionalShadowConfig::new(300, [0.0; 3], [1.0; 3], 0.001)
                .err(),
            Some(DirectionalShadowError::InvalidResolution)
        );
    }

    #[test]
    fn street_policy_locks_focus_to_texel_snapped_target() {
        let policy = DirectionalShadowPolicy::street_city();
        let texel = policy.texel_world_size();
        assert!(
            (texel - (80.0 / 1024.0)).abs() < 1e-6,
            "street texel should be 80/1024 m, got {texel}"
        );
        let config = policy
            .config_for_camera(crate::Camera3d::new(
                [2.0, 28.0, 6.0],
                [2.3, 1.2, 0.4],
                [0.0, 1.0, 0.0],
                1.0,
                0.1,
                200.0,
            ))
            .expect("policy");
        assert_eq!(config.cascade_count(), 1);
        assert_eq!(config.far_half_extent(), None);
        let focus = config.focus_center();
        assert!(
            (focus[1] - 1.2).abs() < 1e-4,
            "focus Y must stay on target, got {focus:?}"
        );
        assert!(
            (focus[0] / texel).round() * texel - focus[0] < 1e-4,
            "focus X must land on texel grid {focus:?} texel={texel}"
        );
        assert!(
            (focus[2] / texel).round() * texel - focus[2] < 1e-4,
            "focus Z must land on texel grid {focus:?} texel={texel}"
        );

        let orbit = crate::Camera3d::new(
            [-18.0, 12.0, -10.0],
            [2.3, 1.2, 0.4],
            [0.0, 1.0, 0.0],
            1.0,
            0.1,
            200.0,
        );
        assert_eq!(
            focus,
            policy
                .config_for_camera(orbit)
                .expect("orbit policy")
                .focus_center()
        );

        let walked = crate::Camera3d::new(
            [2.0, 28.0, 6.0],
            [
                focus[0] + texel * 0.4,
                focus[1],
                focus[2] + texel * 0.4,
            ],
            [0.0, 1.0, 0.0],
            1.0,
            0.1,
            200.0,
        );
        assert_eq!(
            focus,
            policy
                .config_for_camera(walked)
                .expect("walked policy")
                .focus_center()
        );
    }

    #[test]
    fn single_cascade_smoke_config_stays_at_one_layer() {
        assert_eq!(DirectionalShadowConfig::smoke().cascade_count(), 1);
    }

    #[test]
    fn shadow_caster_shaders_discard_on_mask_cutoff() {
        assert!(super::SHADOW_CASTER_FACTOR_WGSL.contains("discard"));
        assert!(super::SHADOW_CASTER_FACTOR_WGSL.contains("alpha_cutoff"));
        assert!(super::SHADOW_CASTER_TEXTURED_WGSL.contains("discard"));
        assert!(super::SHADOW_CASTER_TEXTURED_WGSL.contains("base_color_texture"));
    }

    #[test]
    fn coverage_contains_focus_and_rejects_far_points() {
        let config = DirectionalShadowConfig::new(512, [0.0, 1.0, 0.0], [10.0, 10.0, 20.0], 0.001)
            .expect("config");
        assert!(super::shadow_coverage_contains([1.0, 1.0, 1.0], config, 0.0));
        assert!(!super::shadow_coverage_contains([40.0, 1.0, 0.0], config, 0.0));
        assert!(super::shadow_coverage_contains([14.0, 1.0, 0.0], config, 5.0));
    }

    #[test]
    fn light_matrix_is_finite() {
        let config = DirectionalShadowConfig::smoke();
        let matrix = light_orthographic_view_projection([-0.2, -1.0, -0.35], config)
            .expect("light matrix");
        assert!(matrix.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn focus_center_projects_inside_clip() {
        let config = DirectionalShadowConfig::smoke();
        let matrix = light_orthographic_view_projection([-0.45, -1.0, -0.25], config)
            .expect("light matrix");
        let center = config.focus_center();
        let clip = transform_point(matrix, center);
        let ndc = [clip[0] / clip[3], clip[1] / clip[3], clip[2] / clip[3]];
        assert!(
            ndc.iter().all(|v| (-1.05..=1.05).contains(v)),
            "center ndc {ndc:?}"
        );
        let ground = [0.0_f32, 0.0, 0.0];
        let gclip = transform_point(matrix, ground);
        let gndc = [gclip[0] / gclip[3], gclip[1] / gclip[3], gclip[2] / gclip[3]];
        assert!(
            gndc[0].abs() <= 1.05 && gndc[1].abs() <= 1.05 && (0.0..=1.05).contains(&gndc[2]),
            "ground ndc {gndc:?}"
        );
    }

    fn transform_point(matrix: [f32; 16], point: [f32; 3]) -> [f32; 4] {
        let p = [point[0], point[1], point[2], 1.0];
        let mut out = [0.0; 4];
        for row in 0..4 {
            out[row] = (0..4).map(|col| matrix[col * 4 + row] * p[col]).sum();
        }
        out
    }
}
