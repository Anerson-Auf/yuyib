//! Shadow-aware volumetric directional-light scattering.
//!
//! The pass raymarches in reconstructed world space against the engine-owned
//! cascaded shadow map. It renders at half resolution and composites additively
//! so applications do not need access to private shadow bind-group layouts.

use std::{error::Error, fmt, mem::size_of};

use bytemuck::{Pod, Zeroable};
use yuyib_render::{RenderFrame, wgpu};

use crate::{Camera3d, GpuDirectionalShadow, MeshRenderError};

/// Quality and medium parameters for one volumetric directional-light pass.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VolumetricLighting3d {
    color: [f32; 3],
    intensity: f32,
    density: f32,
    anisotropy: f32,
    max_distance: f32,
    step_count: u32,
}

impl VolumetricLighting3d {
    /// Creates validated world-space scattering parameters.
    ///
    /// `density` is extinction per world unit. `anisotropy` is the
    /// Henyey-Greenstein `g` term. The bounded step count prevents an
    /// accidentally unbounded fragment workload.
    ///
    /// # Errors
    ///
    /// Returns [`VolumetricLightingError3d`] for non-finite or out-of-range
    /// medium, colour, distance, intensity, or workload values.
    pub fn new(
        color: [f32; 3],
        intensity: f32,
        density: f32,
        anisotropy: f32,
        max_distance: f32,
        step_count: u32,
    ) -> Result<Self, VolumetricLightingError3d> {
        if !color
            .iter()
            .all(|channel| channel.is_finite() && *channel >= 0.0)
        {
            return Err(VolumetricLightingError3d::InvalidColor);
        }
        if !intensity.is_finite() || intensity < 0.0 {
            return Err(VolumetricLightingError3d::InvalidIntensity);
        }
        if !density.is_finite() || density <= 0.0 {
            return Err(VolumetricLightingError3d::InvalidDensity);
        }
        if !anisotropy.is_finite() || !(-0.95..=0.95).contains(&anisotropy) {
            return Err(VolumetricLightingError3d::InvalidAnisotropy);
        }
        if !max_distance.is_finite() || max_distance <= 0.0 {
            return Err(VolumetricLightingError3d::InvalidMaxDistance);
        }
        if !(8..=64).contains(&step_count) {
            return Err(VolumetricLightingError3d::InvalidStepCount);
        }
        Ok(Self {
            color,
            intensity,
            density,
            anisotropy,
            max_distance,
            step_count,
        })
    }

    /// Outdoor preset sized for Source-style world coordinates.
    #[must_use]
    pub const fn source_sun() -> Self {
        Self {
            color: [1.0, 0.88, 0.68],
            intensity: 1.35,
            density: 0.000_16,
            anisotropy: 0.72,
            max_distance: 12_000.0,
            step_count: 24,
        }
    }

    /// Returns a copy with a new non-negative intensity.
    ///
    /// # Errors
    ///
    /// Returns [`VolumetricLightingError3d::InvalidIntensity`] for a negative
    /// or non-finite value.
    pub fn with_intensity(mut self, intensity: f32) -> Result<Self, VolumetricLightingError3d> {
        if !intensity.is_finite() || intensity < 0.0 {
            return Err(VolumetricLightingError3d::InvalidIntensity);
        }
        self.intensity = intensity;
        Ok(self)
    }

    /// Returns a copy with a new positive extinction density.
    ///
    /// # Errors
    ///
    /// Returns [`VolumetricLightingError3d::InvalidDensity`] unless `density`
    /// is finite and positive.
    pub fn with_density(mut self, density: f32) -> Result<Self, VolumetricLightingError3d> {
        if !density.is_finite() || density <= 0.0 {
            return Err(VolumetricLightingError3d::InvalidDensity);
        }
        self.density = density;
        Ok(self)
    }
}

/// Invalid [`VolumetricLighting3d`] construction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VolumetricLightingError3d {
    /// RGB contained a negative or non-finite component.
    InvalidColor,
    /// Intensity was negative or non-finite.
    InvalidIntensity,
    /// Density was not positive and finite.
    InvalidDensity,
    /// Anisotropy was non-finite or outside `-0.95..=0.95`.
    InvalidAnisotropy,
    /// Maximum ray distance was not positive and finite.
    InvalidMaxDistance,
    /// Step count was outside `8..=64`.
    InvalidStepCount,
}

impl fmt::Display for VolumetricLightingError3d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidColor => "volumetric light color must be finite and non-negative",
            Self::InvalidIntensity => "volumetric intensity must be finite and non-negative",
            Self::InvalidDensity => "volumetric density must be finite and positive",
            Self::InvalidAnisotropy => "volumetric anisotropy must be in -0.95..=0.95",
            Self::InvalidMaxDistance => "volumetric max distance must be finite and positive",
            Self::InvalidStepCount => "volumetric step count must be in 8..=64",
        })
    }
}

impl Error for VolumetricLightingError3d {}

/// Draw failure for [`VolumetricLightingRenderer3d`].
#[derive(Debug)]
pub enum VolumetricLightingRenderError3d {
    /// The camera could not produce an invertible view-projection matrix.
    Camera(MeshRenderError),
    /// The light direction was zero or non-finite.
    InvalidLightDirection,
}

impl fmt::Display for VolumetricLightingRenderError3d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Camera(error) => write!(formatter, "volumetric camera: {error}"),
            Self::InvalidLightDirection => {
                formatter.write_str("volumetric light direction must be finite and non-zero")
            }
        }
    }
}

impl Error for VolumetricLightingRenderError3d {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Camera(error) => Some(error),
            Self::InvalidLightDirection => None,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct VolumetricUniform {
    inverse_view_projection: [f32; 16],
    light_view_projection_0: [f32; 16],
    light_view_projection_1: [f32; 16],
    camera_position: [f32; 4],
    light_direction: [f32; 4],
    light_color_intensity: [f32; 4],
    medium: [f32; 4],
    shadow: [f32; 4],
}

const _: () = assert!(size_of::<VolumetricUniform>() == 272);

/// Half-resolution world-space raymarcher backed by [`GpuDirectionalShadow`].
pub struct VolumetricLightingRenderer3d {
    width: u32,
    height: u32,
    color_format: wgpu::TextureFormat,
    _scattering_texture: wgpu::Texture,
    scattering_view: wgpu::TextureView,
    scattering_sampler: wgpu::Sampler,
    uniform_buffer: wgpu::Buffer,
    raymarch_layout: wgpu::BindGroupLayout,
    composite_layout: wgpu::BindGroupLayout,
    raymarch_pipeline: wgpu::RenderPipeline,
    composite_pipeline: wgpu::RenderPipeline,
}

impl VolumetricLightingRenderer3d {
    /// Allocates pipelines and a half-resolution scattering target for `frame`.
    #[must_use]
    #[allow(
        clippy::too_many_lines,
        reason = "two tightly-coupled GPU pipelines share one allocation boundary"
    )]
    pub fn new_for_frame(frame: &RenderFrame<'_>) -> Self {
        let draw_size = frame.draw_size();
        let width = (draw_size[0] / 2).max(1);
        let height = (draw_size[1] / 2).max(1);
        let color_format = frame.surface_format();
        let device = frame.device();
        let scattering_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("yuyib volumetric scattering"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let scattering_view =
            scattering_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let scattering_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("yuyib volumetric scattering sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("yuyib volumetric uniform"),
            size: size_of::<VolumetricUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let raymarch_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("yuyib volumetric raymarch layout"),
            entries: &[
                uniform_entry(0),
                depth_entry(1, wgpu::TextureViewDimension::D2),
                depth_entry(2, wgpu::TextureViewDimension::D2Array),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
            ],
        });
        let composite_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("yuyib volumetric composite layout"),
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
        let raymarch_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("yuyib volumetric raymarch WGSL"),
            source: wgpu::ShaderSource::Wgsl(VOLUMETRIC_RAYMARCH_WGSL.into()),
        });
        let composite_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("yuyib volumetric composite WGSL"),
            source: wgpu::ShaderSource::Wgsl(VOLUMETRIC_COMPOSITE_WGSL.into()),
        });
        let raymarch_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("yuyib volumetric raymarch pipeline"),
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("yuyib volumetric raymarch pipeline layout"),
                    bind_group_layouts: &[Some(&raymarch_layout)],
                    immediate_size: 0,
                }),
            ),
            vertex: fullscreen_vertex_state(&raymarch_shader),
            fragment: Some(wgpu::FragmentState {
                module: &raymarch_shader,
                entry_point: Some("fragment_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba16Float,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let composite_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("yuyib volumetric composite pipeline"),
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("yuyib volumetric composite pipeline layout"),
                    bind_group_layouts: &[Some(&composite_layout)],
                    immediate_size: 0,
                }),
            ),
            vertex: fullscreen_vertex_state(&composite_shader),
            fragment: Some(wgpu::FragmentState {
                module: &composite_shader,
                entry_point: Some("fragment_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: color_format,
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::One,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::Zero,
                            dst_factor: wgpu::BlendFactor::One,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        Self {
            width,
            height,
            color_format,
            _scattering_texture: scattering_texture,
            scattering_view,
            scattering_sampler,
            uniform_buffer,
            raymarch_layout,
            composite_layout,
            raymarch_pipeline,
            composite_pipeline,
        }
    }

    /// Whether the cached target and pipelines match the current frame.
    #[must_use]
    pub fn matches(&self, frame: &RenderFrame<'_>) -> bool {
        let size = frame.draw_size();
        self.width == (size[0] / 2).max(1)
            && self.height == (size[1] / 2).max(1)
            && self.color_format == frame.surface_format()
    }

    /// Raymarches the current camera depth against `shadow`, then composites.
    ///
    /// # Errors
    ///
    /// Returns [`VolumetricLightingRenderError3d`] when the camera matrix is
    /// invalid/non-invertible or the light direction is degenerate.
    pub fn draw_for_frame(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        shadow: &GpuDirectionalShadow,
        light_direction_toward_scene: [f32; 3],
        settings: VolumetricLighting3d,
    ) -> Result<(), VolumetricLightingRenderError3d> {
        let view_projection = camera
            .view_projection(frame.draw_size())
            .map_err(VolumetricLightingRenderError3d::Camera)?;
        let inverse_view_projection =
            inverse_matrix4(view_projection).ok_or(VolumetricLightingRenderError3d::Camera(
                MeshRenderError::InvalidCamera("volumetric inverse view-projection failed"),
            ))?;
        let direction = normalize3(light_direction_toward_scene)
            .ok_or(VolumetricLightingRenderError3d::InvalidLightDirection)?;
        let uniform = VolumetricUniform {
            inverse_view_projection,
            light_view_projection_0: shadow.light_view_proj_cascade(0),
            light_view_projection_1: shadow.light_view_proj_cascade(1),
            camera_position: [
                camera.position[0],
                camera.position[1],
                camera.position[2],
                1.0,
            ],
            light_direction: [
                direction[0],
                direction[1],
                direction[2],
                u8::try_from(shadow.cascade_count()).map_or(2.0, f32::from),
            ],
            light_color_intensity: [
                settings.color[0],
                settings.color[1],
                settings.color[2],
                settings.intensity,
            ],
            medium: [
                settings.density,
                settings.anisotropy,
                settings.max_distance,
                u8::try_from(settings.step_count).map_or(64.0, f32::from),
            ],
            shadow: [shadow.depth_bias(), 0.0, 0.0, 0.0],
        };
        frame
            .queue()
            .write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniform));
        let raymarch_bind_group = frame
            .device()
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("yuyib volumetric raymarch bind group"),
                layout: &self.raymarch_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.uniform_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(frame.camera_depth_view()),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(shadow.depth_view()),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(shadow.compare_sampler()),
                    },
                ],
            });
        frame.with_color_only_pass(
            &self.scattering_view,
            wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
            |pass| {
                pass.set_pipeline(&self.raymarch_pipeline);
                pass.set_bind_group(0, Some(&raymarch_bind_group), &[]);
                pass.draw(0..3, 0..1);
            },
        );
        let composite_bind_group = frame
            .device()
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("yuyib volumetric composite bind group"),
                layout: &self.composite_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&self.scattering_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.scattering_sampler),
                    },
                ],
            });
        frame.with_surface_pass(wgpu::LoadOp::Load, |pass| {
            pass.set_pipeline(&self.composite_pipeline);
            pass.set_bind_group(0, Some(&composite_bind_group), &[]);
            pass.draw(0..3, 0..1);
        });
        Ok(())
    }
}

fn fullscreen_vertex_state(module: &wgpu::ShaderModule) -> wgpu::VertexState<'_> {
    wgpu::VertexState {
        module,
        entry_point: Some("vertex_main"),
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        buffers: &[],
    }
}

fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: wgpu::BufferSize::new(size_of::<VolumetricUniform>() as u64),
        },
        count: None,
    }
}

fn depth_entry(
    binding: u32,
    view_dimension: wgpu::TextureViewDimension,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Depth,
            view_dimension,
            multisampled: false,
        },
        count: None,
    }
}

fn normalize3(value: [f32; 3]) -> Option<[f32; 3]> {
    if !value.iter().all(|component| component.is_finite()) {
        return None;
    }
    let length_squared = value
        .iter()
        .map(|component| component * component)
        .sum::<f32>();
    if !length_squared.is_finite() || length_squared <= 1.0e-12 {
        return None;
    }
    let inverse_length = length_squared.sqrt().recip();
    Some(value.map(|component| component * inverse_length))
}

fn inverse_matrix4(m: [f32; 16]) -> Option<[f32; 16]> {
    let (a00, a01, a02, a03) = (m[0], m[1], m[2], m[3]);
    let (a10, a11, a12, a13) = (m[4], m[5], m[6], m[7]);
    let (a20, a21, a22, a23) = (m[8], m[9], m[10], m[11]);
    let (a30, a31, a32, a33) = (m[12], m[13], m[14], m[15]);
    let b00 = a00 * a11 - a01 * a10;
    let b01 = a00 * a12 - a02 * a10;
    let b02 = a00 * a13 - a03 * a10;
    let b03 = a01 * a12 - a02 * a11;
    let b04 = a01 * a13 - a03 * a11;
    let b05 = a02 * a13 - a03 * a12;
    let b06 = a20 * a31 - a21 * a30;
    let b07 = a20 * a32 - a22 * a30;
    let b08 = a20 * a33 - a23 * a30;
    let b09 = a21 * a32 - a22 * a31;
    let b10 = a21 * a33 - a23 * a31;
    let b11 = a22 * a33 - a23 * a32;
    let determinant = b00 * b11 - b01 * b10 + b02 * b09 + b03 * b08 - b04 * b07 + b05 * b06;
    if !determinant.is_finite() || determinant.abs() < 1.0e-12 {
        return None;
    }
    let inverse = determinant.recip();
    Some([
        (a11 * b11 - a12 * b10 + a13 * b09) * inverse,
        (a02 * b10 - a01 * b11 - a03 * b09) * inverse,
        (a31 * b05 - a32 * b04 + a33 * b03) * inverse,
        (a22 * b04 - a21 * b05 - a23 * b03) * inverse,
        (a12 * b08 - a10 * b11 - a13 * b07) * inverse,
        (a00 * b11 - a02 * b08 + a03 * b07) * inverse,
        (a32 * b02 - a30 * b05 - a33 * b01) * inverse,
        (a20 * b05 - a22 * b02 + a23 * b01) * inverse,
        (a10 * b10 - a11 * b08 + a13 * b06) * inverse,
        (a01 * b08 - a00 * b10 - a03 * b06) * inverse,
        (a30 * b04 - a31 * b02 + a33 * b00) * inverse,
        (a21 * b02 - a20 * b04 - a23 * b00) * inverse,
        (a11 * b07 - a10 * b09 - a12 * b06) * inverse,
        (a00 * b09 - a01 * b07 + a02 * b06) * inverse,
        (a31 * b01 - a30 * b03 - a32 * b00) * inverse,
        (a20 * b03 - a21 * b01 + a22 * b00) * inverse,
    ])
}

const VOLUMETRIC_RAYMARCH_WGSL: &str = r"
const PI: f32 = 3.141592653589793;
struct Uniforms {
    inverse_view_projection: mat4x4<f32>,
    light_view_projection_0: mat4x4<f32>,
    light_view_projection_1: mat4x4<f32>,
    camera_position: vec4<f32>,
    light_direction: vec4<f32>,
    light_color_intensity: vec4<f32>,
    medium: vec4<f32>,
    shadow: vec4<f32>,
};
@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var scene_depth: texture_depth_2d;
@group(0) @binding(2) var shadow_map: texture_depth_2d_array;
@group(0) @binding(3) var shadow_sampler: sampler_comparison;
struct FullscreenVertex {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};
@vertex fn vertex_main(@builtin(vertex_index) index: u32) -> FullscreenVertex {
    var output: FullscreenVertex;
    let xy = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    output.uv = xy;
    output.position = vec4<f32>(xy * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    return output;
}
fn reconstruct(uv: vec2<f32>, depth: f32) -> vec3<f32> {
    let clip = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), depth, 1.0);
    let world = uniforms.inverse_view_projection * clip;
    return world.xyz / world.w;
}
fn shadow_sample(world_position: vec3<f32>, layer: i32, matrix: mat4x4<f32>) -> f32 {
    let clip = matrix * vec4<f32>(world_position, 1.0);
    let ndc = clip.xyz / clip.w;
    let uv = ndc.xy * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5);
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0 || ndc.z < 0.0 || ndc.z > 1.0) {
        return -1.0;
    }
    return textureSampleCompare(shadow_map, shadow_sampler, uv, layer, ndc.z - uniforms.shadow.x);
}
fn visibility(world_position: vec3<f32>) -> f32 {
    var result = 1.0;
    var covered = false;
    let near = shadow_sample(world_position, 0, uniforms.light_view_projection_0);
    if (near >= 0.0) {
        result = min(result, near);
        covered = true;
    }
    if (uniforms.light_direction.w > 1.5) {
        let far = shadow_sample(world_position, 1, uniforms.light_view_projection_1);
        if (far >= 0.0) {
            result = min(result, far);
            covered = true;
        }
    }
    return select(0.0, result, covered);
}
fn hash12(value: vec2<f32>) -> f32 {
    let h = dot(value, vec2<f32>(127.1, 311.7));
    return fract(sin(h) * 43758.5453123);
}
@fragment fn fragment_main(input: FullscreenVertex) -> @location(0) vec4<f32> {
    let depth_size = textureDimensions(scene_depth);
    let pixel = min(vec2<u32>(input.uv * vec2<f32>(depth_size)), depth_size - vec2<u32>(1u));
    let depth = textureLoad(scene_depth, vec2<i32>(pixel), 0);
    let far_point = reconstruct(input.uv, 1.0);
    let ray_direction = normalize(far_point - uniforms.camera_position.xyz);
    var ray_length = uniforms.medium.z;
    if (depth < 0.999999) {
        ray_length = min(distance(reconstruct(input.uv, depth), uniforms.camera_position.xyz), ray_length);
    }
    let steps = u32(uniforms.medium.w + 0.5);
    let step_length = ray_length / max(f32(steps), 1.0);
    let jitter = hash12(input.position.xy);
    let density = uniforms.medium.x;
    let g = uniforms.medium.y;
    let cosine = dot(ray_direction, normalize(-uniforms.light_direction.xyz));
    let phase_denominator = max(pow(1.0 + g * g - 2.0 * g * cosine, 1.5), 0.0001);
    let phase = (1.0 - g * g) / (4.0 * PI * phase_denominator);
    var accumulated = 0.0;
    for (var index = 0u; index < 64u; index = index + 1u) {
        if (index >= steps) { break; }
        let distance_along_ray = (f32(index) + jitter) * step_length;
        let sample_position = uniforms.camera_position.xyz + ray_direction * distance_along_ray;
        let transmittance = exp(-density * distance_along_ray);
        accumulated += visibility(sample_position) * transmittance * density * step_length;
    }
    let scattering = uniforms.light_color_intensity.rgb
        * uniforms.light_color_intensity.a * phase * accumulated;
    return vec4<f32>(scattering, 0.0);
}
";

const VOLUMETRIC_COMPOSITE_WGSL: &str = r"
@group(0) @binding(0) var scattering: texture_2d<f32>;
@group(0) @binding(1) var scattering_sampler: sampler;
struct FullscreenVertex {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};
@vertex fn vertex_main(@builtin(vertex_index) index: u32) -> FullscreenVertex {
    var output: FullscreenVertex;
    let xy = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    output.uv = xy;
    output.position = vec4<f32>(xy * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    return output;
}
@fragment fn fragment_main(input: FullscreenVertex) -> @location(0) vec4<f32> {
    return vec4<f32>(textureSample(scattering, scattering_sampler, input.uv).rgb, 0.0);
}
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_rejects_unbounded_work_and_invalid_medium() {
        assert_eq!(
            VolumetricLighting3d::new([1.0; 3], 1.0, 0.001, 0.7, 100.0, 65),
            Err(VolumetricLightingError3d::InvalidStepCount)
        );
        assert_eq!(
            VolumetricLighting3d::new([1.0; 3], 1.0, 0.0, 0.7, 100.0, 24),
            Err(VolumetricLightingError3d::InvalidDensity)
        );
    }

    #[test]
    fn shaders_parse_as_wgsl() {
        for (label, source) in [
            ("raymarch", VOLUMETRIC_RAYMARCH_WGSL),
            ("composite", VOLUMETRIC_COMPOSITE_WGSL),
        ] {
            let module = naga::front::wgsl::parse_str(source)
                .unwrap_or_else(|error| panic!("{label} WGSL parse failed: {error}"));
            naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::all(),
            )
            .validate(&module)
            .unwrap_or_else(|error| panic!("{label} WGSL validation failed: {error}"));
        }
    }
}
