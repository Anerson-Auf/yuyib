//! Screen-space ambient occlusion for the high-level 3D scene path.
//!
//! SSAO runs after opaque PBR into a half-resolution AO buffer, then multiplies
//! onto the colour target. Sky / clear-depth pixels stay unoccluded. Depth-only
//! MVP — GTAO and temporal accumulation remain open.

use yuyib_render::{RenderFrame, wgpu};

use crate::{Camera3d, MeshRenderError};

/// Half-resolution SSAO policy for [`crate::Game3dScene::with_ssao`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SsaoPolicy {
    radius: f32,
    bias: f32,
    intensity: f32,
    sample_count: u32,
}

impl SsaoPolicy {
    /// Creates validated SSAO settings.
    ///
    /// # Errors
    ///
    /// Rejects non-finite or non-positive radius/intensity, negative bias, or
    /// sample counts outside `4..=32`.
    pub fn new(
        radius: f32,
        bias: f32,
        intensity: f32,
        sample_count: u32,
    ) -> Result<Self, SsaoPolicyError> {
        if !radius.is_finite() || radius <= 0.0 {
            return Err(SsaoPolicyError::InvalidRadius);
        }
        if !bias.is_finite() || bias < 0.0 {
            return Err(SsaoPolicyError::InvalidBias);
        }
        if !intensity.is_finite() || intensity <= 0.0 {
            return Err(SsaoPolicyError::InvalidIntensity);
        }
        if !(4..=32).contains(&sample_count) {
            return Err(SsaoPolicyError::InvalidSampleCount);
        }
        Ok(Self {
            radius,
            bias,
            intensity,
            sample_count,
        })
    }

    /// Indoor / street playable preset: short radius, readable contact darkening.
    #[must_use]
    pub fn street_city() -> Self {
        Self::new(0.55, 0.025, 0.85, 12).expect("street-city SSAO preset")
    }

    /// Approximate view-space sample radius.
    #[must_use]
    pub const fn radius(self) -> f32 {
        self.radius
    }

    /// Depth bias to reduce self-occlusion.
    #[must_use]
    pub const fn bias(self) -> f32 {
        self.bias
    }

    /// How strongly AO multiplies scene colour.
    #[must_use]
    pub const fn intensity(self) -> f32 {
        self.intensity
    }

    /// Hemisphere sample count.
    #[must_use]
    pub const fn sample_count(self) -> u32 {
        self.sample_count
    }
}

/// Invalid [`SsaoPolicy`] construction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SsaoPolicyError {
    /// Radius was non-finite or not positive.
    InvalidRadius,
    /// Bias was non-finite or negative.
    InvalidBias,
    /// Intensity was non-finite or not positive.
    InvalidIntensity,
    /// Sample count was outside the supported range.
    InvalidSampleCount,
}

impl std::fmt::Display for SsaoPolicyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidRadius => "SSAO radius must be finite and > 0",
            Self::InvalidBias => "SSAO bias must be finite and >= 0",
            Self::InvalidIntensity => "SSAO intensity must be finite and > 0",
            Self::InvalidSampleCount => "SSAO sample count must be in 4..=32",
        })
    }
}

impl std::error::Error for SsaoPolicyError {}

pub(crate) struct GpuSsao {
    width: u32,
    height: u32,
    color_format: wgpu::TextureFormat,
    _ao_texture: wgpu::Texture,
    ao_view: wgpu::TextureView,
    params: wgpu::Buffer,
    ao_sampler: wgpu::Sampler,
    depth_sampler: wgpu::Sampler,
    extract_pipeline: wgpu::RenderPipeline,
    composite_pipeline: wgpu::RenderPipeline,
    extract_layout: wgpu::BindGroupLayout,
    composite_layout: wgpu::BindGroupLayout,
}

impl GpuSsao {
    pub(crate) fn matches(
        &self,
        width: u32,
        height: u32,
        color_format: wgpu::TextureFormat,
    ) -> bool {
        self.width == width && self.height == height && self.color_format == color_format
    }

    pub(crate) fn new(frame: &RenderFrame<'_>) -> Self {
        let full = frame.surface_size();
        let width = (full[0] / 2).max(1);
        let height = (full[1] / 2).max(1);
        let color_format = frame.surface_format();
        let device = frame.device();
        let ao_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("yuyib SSAO ao"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R16Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let ao_view = ao_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("yuyib SSAO params"),
            size: 96,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let ao_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("yuyib SSAO ao sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let depth_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("yuyib SSAO depth sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let extract_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("yuyib SSAO extract layout"),
            entries: &[
                uniform_entry(0),
                depth_tex_entry(1),
                non_filtering_sampler_entry(2),
            ],
        });
        let composite_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("yuyib SSAO composite layout"),
            entries: &[
                uniform_entry(0),
                float_tex_entry(1),
                filtering_sampler_entry(2),
            ],
        });
        let extract_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("yuyib SSAO extract WGSL"),
            source: wgpu::ShaderSource::Wgsl(SSAO_EXTRACT_SHADER.into()),
        });
        let composite_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("yuyib SSAO composite WGSL"),
            source: wgpu::ShaderSource::Wgsl(SSAO_COMPOSITE_SHADER.into()),
        });
        let extract_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("yuyib SSAO extract"),
            layout: Some(&device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("yuyib SSAO extract pipeline layout"),
                bind_group_layouts: &[Some(&extract_layout)],
                immediate_size: 0,
            })),
            vertex: wgpu::VertexState {
                module: &extract_shader,
                entry_point: Some("vertex_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &extract_shader,
                entry_point: Some("extract_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::R16Float,
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
            label: Some("yuyib SSAO composite"),
            layout: Some(&device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("yuyib SSAO composite pipeline layout"),
                bind_group_layouts: &[Some(&composite_layout)],
                immediate_size: 0,
            })),
            vertex: wgpu::VertexState {
                module: &composite_shader,
                entry_point: Some("vertex_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &composite_shader,
                entry_point: Some("composite_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: color_format,
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::Zero,
                            dst_factor: wgpu::BlendFactor::Src,
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
            _ao_texture: ao_texture,
            ao_view,
            params,
            ao_sampler,
            depth_sampler,
            extract_pipeline,
            composite_pipeline,
            extract_layout,
            composite_layout,
        }
    }

    pub(crate) fn encode(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        policy: SsaoPolicy,
    ) -> Result<(), MeshRenderError> {
        let inv_proj = inverse_matrix4(projection_matrix(camera, frame.draw_size())?).ok_or(
            MeshRenderError::InvalidCamera("SSAO inverse projection failed"),
        )?;
        let mut bytes = [0_u8; 96];
        for (index, value) in inv_proj.iter().enumerate() {
            bytes[index * 4..(index + 1) * 4].copy_from_slice(&value.to_ne_bytes());
        }
        bytes[64..68].copy_from_slice(&policy.radius.to_ne_bytes());
        bytes[68..72].copy_from_slice(&policy.bias.to_ne_bytes());
        bytes[72..76].copy_from_slice(&policy.intensity.to_ne_bytes());
        bytes[76..80].copy_from_slice(&policy.sample_count.to_ne_bytes());
        bytes[80..84].copy_from_slice(&(1.0 / self.width as f32).to_ne_bytes());
        bytes[84..88].copy_from_slice(&(1.0 / self.height as f32).to_ne_bytes());
        frame.queue().write_buffer(&self.params, 0, &bytes);

        let extract_bg = {
            let device = frame.device();
            let depth_view = frame.camera_depth_view();
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("yuyib SSAO extract bind group"),
                layout: &self.extract_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.params.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(depth_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.depth_sampler),
                    },
                ],
            })
        };
        frame.with_color_only_pass(
            &self.ao_view,
            wgpu::LoadOp::Clear(wgpu::Color::WHITE),
            |pass| {
                pass.set_pipeline(&self.extract_pipeline);
                pass.set_bind_group(0, Some(&extract_bg), &[]);
                pass.draw(0..3, 0..1);
            },
        );

        let composite_bg = {
            let device = frame.device();
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("yuyib SSAO composite bind group"),
                layout: &self.composite_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.params.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&self.ao_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.ao_sampler),
                    },
                ],
            })
        };
        frame.with_surface_pass(wgpu::LoadOp::Load, |pass| {
            pass.set_pipeline(&self.composite_pipeline);
            pass.set_bind_group(0, Some(&composite_bg), &[]);
            pass.draw(0..3, 0..1);
        });
        Ok(())
    }
}

fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: wgpu::BufferSize::new(96),
        },
        count: None,
    }
}

fn depth_tex_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Depth,
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn float_tex_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
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

fn filtering_sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

fn non_filtering_sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
        count: None,
    }
}

fn projection_matrix(
    camera: Camera3d,
    surface_size: [u32; 2],
) -> Result<[f32; 16], MeshRenderError> {
    if surface_size[0] == 0 || surface_size[1] == 0 {
        return Err(MeshRenderError::InvalidCamera(
            "surface dimensions must be non-zero",
        ));
    }
    if !camera.vertical_fov_radians.is_finite()
        || camera.vertical_fov_radians <= 0.0
        || camera.vertical_fov_radians >= std::f32::consts::PI
    {
        return Err(MeshRenderError::InvalidCamera(
            "vertical field of view must be finite and between zero and pi",
        ));
    }
    if !camera.near.is_finite()
        || !camera.far.is_finite()
        || camera.near <= 0.0
        || camera.far <= camera.near
    {
        return Err(MeshRenderError::InvalidCamera(
            "clip planes must be finite with 0 < near < far",
        ));
    }
    let aspect = surface_size[0] as f32 / surface_size[1] as f32;
    let focal_length = 1.0 / (camera.vertical_fov_radians * 0.5).tan();
    Ok([
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
        camera.far / (camera.near - camera.far),
        -1.0,
        0.0,
        0.0,
        (camera.near * camera.far) / (camera.near - camera.far),
        0.0,
    ])
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

    let det = b00 * b11 - b01 * b10 + b02 * b09 + b03 * b08 - b04 * b07 + b05 * b06;
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    let inv_det = det.recip();
    Some([
        (a11 * b11 - a12 * b10 + a13 * b09) * inv_det,
        (a02 * b10 - a01 * b11 - a03 * b09) * inv_det,
        (a31 * b05 - a32 * b04 + a33 * b03) * inv_det,
        (a22 * b04 - a21 * b05 - a23 * b03) * inv_det,
        (a12 * b08 - a10 * b11 - a13 * b07) * inv_det,
        (a00 * b11 - a02 * b08 + a03 * b07) * inv_det,
        (a32 * b02 - a30 * b05 - a33 * b01) * inv_det,
        (a20 * b05 - a22 * b02 + a23 * b01) * inv_det,
        (a10 * b10 - a11 * b08 + a13 * b06) * inv_det,
        (a01 * b08 - a00 * b10 - a03 * b06) * inv_det,
        (a30 * b04 - a31 * b02 + a33 * b00) * inv_det,
        (a21 * b02 - a20 * b04 - a23 * b00) * inv_det,
        (a11 * b07 - a10 * b09 - a12 * b06) * inv_det,
        (a00 * b09 - a01 * b07 + a02 * b06) * inv_det,
        (a31 * b01 - a30 * b03 - a32 * b00) * inv_det,
        (a20 * b03 - a21 * b01 + a22 * b00) * inv_det,
    ])
}

const SSAO_EXTRACT_SHADER: &str = r"
struct Params {
    inv_projection: mat4x4<f32>,
    radius: f32,
    bias: f32,
    intensity: f32,
    sample_count: u32,
    inv_ao_size: vec2<f32>,
    _pad0: vec2<f32>,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var depth_tex: texture_depth_2d;
@group(0) @binding(2) var depth_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vertex_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    var output: VertexOutput;
    let pos = positions[index];
    output.position = vec4<f32>(pos, 0.0, 1.0);
    output.uv = pos * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5, 0.5);
    return output;
}

fn view_position(uv: vec2<f32>, depth: f32) -> vec3<f32> {
    let ndc = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, depth, 1.0);
    var view = params.inv_projection * ndc;
    view = view / view.w;
    return view.xyz;
}

fn ign(uv: vec2<f32>) -> f32 {
    return fract(52.9829189 * fract(dot(uv, vec2<f32>(0.06711056, 0.00583715))));
}

@fragment
fn extract_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let depth_size = vec2<f32>(textureDimensions(depth_tex));
    let depth = textureSample(depth_tex, depth_sampler, input.uv);
    if (depth >= 0.999) {
        return vec4<f32>(1.0);
    }
    let origin = view_position(input.uv, depth);
    let noise = ign(input.uv * depth_size);
    var occlusion = 0.0;
    let count = params.sample_count;
    for (var i = 0u; i < 32u; i = i + 1u) {
        if (i >= count) { break; }
        let fi = f32(i) + 1.0;
        let angle = fi * 2.399963 + noise * 6.283185;
        let radius_scale = (fi / f32(count)) * params.radius;
        let offset = vec3<f32>(
            cos(angle),
            sin(angle),
            0.35 + 0.65 * fract(noise + fi * 0.17),
        ) * radius_scale;
        let scale = 8.0 / max(-origin.z, 0.05);
        let sample_uv = clamp(
            input.uv + vec2<f32>(offset.x, -offset.y) * params.inv_ao_size * scale,
            vec2<f32>(0.0),
            vec2<f32>(1.0),
        );
        let sample_depth = textureSample(depth_tex, depth_sampler, sample_uv);
        if (sample_depth >= 0.999) { continue; }
        let sample_view = view_position(sample_uv, sample_depth);
        let dist = length(sample_view - origin);
        let range_check = smoothstep(0.0, 1.0, params.radius / max(dist, 0.0001));
        if (sample_view.z >= origin.z + params.bias) {
            occlusion = occlusion + range_check;
        }
    }
    let ao = clamp(1.0 - (occlusion / f32(count)), 0.0, 1.0);
    return vec4<f32>(ao);
}
";

const SSAO_COMPOSITE_SHADER: &str = r"
struct Params {
    inv_projection: mat4x4<f32>,
    radius: f32,
    bias: f32,
    intensity: f32,
    sample_count: u32,
    inv_ao_size: vec2<f32>,
    _pad0: vec2<f32>,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var ao_tex: texture_2d<f32>;
@group(0) @binding(2) var ao_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vertex_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    var output: VertexOutput;
    let pos = positions[index];
    output.position = vec4<f32>(pos, 0.0, 1.0);
    output.uv = pos * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5, 0.5);
    return output;
}

@fragment
fn composite_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let ao = textureSampleLevel(ao_tex, ao_sampler, input.uv, 0.0).r;
    let factor = mix(1.0, ao, params.intensity);
    return vec4<f32>(factor, factor, factor, 1.0);
}
";

#[cfg(test)]
mod tests {
    use super::{SsaoPolicy, SsaoPolicyError};

    #[test]
    fn street_city_preset_is_valid() {
        let policy = SsaoPolicy::street_city();
        assert!(policy.radius() > 0.0);
        assert!(policy.intensity() > 0.0);
        assert!((4..=32).contains(&policy.sample_count()));
    }

    #[test]
    fn rejects_zero_radius() {
        assert_eq!(
            SsaoPolicy::new(0.0, 0.01, 1.0, 8),
            Err(SsaoPolicyError::InvalidRadius)
        );
    }
}
