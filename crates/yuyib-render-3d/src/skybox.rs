//! Fullscreen cubemap skybox presentation for M2 IBL.
//!
//! Draws after opaque geometry with depth test `LessEqual` and depth writes
//! off, so far-plane / empty pixels pick up the environment while scene depth
//! still occludes the sky. Source faces are usually mip0 of a cooked specular
//! probe ([`PreparedSkybox3d::from_specular_mip0`]).

use crate::ibl::{PreparedSpecularIbl3d, SPECULAR_IBL_FACE_COUNT};
use crate::{Camera3d, DepthLoad, MeshRenderError};

use std::{error::Error, fmt, mem::size_of};

use bytemuck::{Pod, Zeroable};
use yuyib_render::{RenderFrame, Renderer, wgpu};

const SKYBOX_WGSL: &str = r#"
struct SkyCamera {
    inv_view_proj: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> camera: SkyCamera;
@group(1) @binding(0) var sky_cube: texture_cube<f32>;
@group(1) @binding(1) var sky_sampler: sampler;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) ndc_xy: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    let ndc = positions[vertex_index];
    var out: VertexOutput;
    // z = 1 maps to the far plane in WGPU's 0..=1 depth range.
    out.clip_position = vec4<f32>(ndc, 1.0, 1.0);
    out.ndc_xy = ndc;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let clip = vec4<f32>(in.ndc_xy, 1.0, 1.0);
    let world = camera.inv_view_proj * clip;
    let direction = normalize(world.xyz / world.w);
    return textureSampleLevel(sky_cube, sky_sampler, direction, 0.0);
}
"#;

/// Failure while packing or validating a skybox cubemap.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SkyboxError {
    /// Face edge must be a positive power of two.
    InvalidFaceSize,
    /// A face byte length did not match `face_size² * 4`.
    InvalidFaceBytes {
        /// Face index in WGPU cube order.
        face: usize,
        /// Observed byte length.
        got: usize,
        /// Expected byte length.
        expected: usize,
    },
    /// Specular pack was missing mip0 faces.
    MissingSpecularMip0,
}

impl fmt::Display for SkyboxError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFaceSize => {
                formatter.write_str("skybox face size must be a positive power of two")
            }
            Self::InvalidFaceBytes {
                face,
                got,
                expected,
            } => write!(
                formatter,
                "skybox face {face} has {got} bytes, expected {expected}"
            ),
            Self::MissingSpecularMip0 => {
                formatter.write_str("specular IBL pack is missing mip0 faces for skybox")
            }
        }
    }
}

impl Error for SkyboxError {}

/// Failure while drawing a skybox.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SkyboxRenderError {
    /// Camera could not produce a sky inverse view-projection.
    Camera(MeshRenderError),
}

impl fmt::Display for SkyboxRenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Camera(error) => write!(formatter, "skybox camera failed: {error}"),
        }
    }
}

impl Error for SkyboxRenderError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Camera(error) => Some(error),
        }
    }
}

impl From<MeshRenderError> for SkyboxRenderError {
    fn from(error: MeshRenderError) -> Self {
        Self::Camera(error)
    }
}

/// CPU-packed LDR RGBA8 cubemap used for skybox presentation (usually mip0).
#[derive(Clone, Debug)]
pub struct PreparedSkybox3d {
    face_size: u32,
    faces: [Vec<u8>; SPECULAR_IBL_FACE_COUNT],
}

impl PreparedSkybox3d {
    /// Packs six tightly packed RGBA8 faces (`face_size² * 4` bytes each).
    ///
    /// # Errors
    ///
    /// Returns [`SkyboxError`] when the edge length or face byte counts are invalid.
    pub fn from_rgba8(
        face_size: u32,
        faces: [Vec<u8>; SPECULAR_IBL_FACE_COUNT],
    ) -> Result<Self, SkyboxError> {
        if face_size == 0 || !face_size.is_power_of_two() {
            return Err(SkyboxError::InvalidFaceSize);
        }
        let expected = (face_size as usize).saturating_mul(face_size as usize) * 4;
        for (face, pixels) in faces.iter().enumerate() {
            if pixels.len() != expected {
                return Err(SkyboxError::InvalidFaceBytes {
                    face,
                    got: pixels.len(),
                    expected,
                });
            }
        }
        Ok(Self { face_size, faces })
    }

    /// Copies mip0 faces from a prefiltered specular pack (same probe as IBL).
    ///
    /// # Errors
    ///
    /// Returns [`SkyboxError`] when mip0 is incomplete or face packing fails.
    pub fn from_specular_mip0(prepared: &PreparedSpecularIbl3d) -> Result<Self, SkyboxError> {
        let mut faces: [Vec<u8>; SPECULAR_IBL_FACE_COUNT] = Default::default();
        for (face, slot) in faces.iter_mut().enumerate() {
            let pixels = prepared
                .mip_face_rgba8(0, face)
                .ok_or(SkyboxError::MissingSpecularMip0)?;
            *slot = pixels.to_vec();
        }
        Self::from_rgba8(prepared.face_size(), faces)
    }

    /// Face edge length in texels.
    #[must_use]
    pub const fn face_size(&self) -> u32 {
        self.face_size
    }

    /// Tightly packed RGBA8 bytes for one cube face.
    #[must_use]
    pub fn face_rgba8(&self, face: usize) -> Option<&[u8]> {
        self.faces.get(face).map(Vec::as_slice)
    }
}

/// GPU cubemap + bind group for [`SkyboxRenderer3d`].
pub struct GpuSkybox3d {
    face_size: u32,
    _cube: wgpu::Texture,
    _cube_view: wgpu::TextureView,
    _cube_sampler: wgpu::Sampler,
    bind_group: wgpu::BindGroup,
}

impl GpuSkybox3d {
    /// Uploads [`PreparedSkybox3d`] to `renderer`'s device.
    #[must_use]
    pub fn upload(renderer: &Renderer, prepared: &PreparedSkybox3d) -> Self {
        renderer.with_raw_gpu(|device, queue, _configuration| {
            Self::upload_with(device, queue, prepared)
        })
    }

    /// Uploads through a frame's device/queue.
    #[must_use]
    pub fn upload_for_frame(frame: &RenderFrame<'_>, prepared: &PreparedSkybox3d) -> Self {
        Self::upload_with(frame.device(), frame.queue(), prepared)
    }

    /// Face edge length in texels.
    #[must_use]
    pub const fn face_size(&self) -> u32 {
        self.face_size
    }

    /// Bind group for `@group(1)` on the skybox pipeline.
    #[must_use]
    pub const fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind_group
    }

    pub(crate) fn bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("yuyib skybox layout"),
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
            ],
        })
    }

    pub(crate) fn upload_with(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        prepared: &PreparedSkybox3d,
    ) -> Self {
        let cube = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("yuyib skybox cube"),
            size: wgpu::Extent3d {
                width: prepared.face_size,
                height: prepared.face_size,
                depth_or_array_layers: SPECULAR_IBL_FACE_COUNT as u32,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for (face, pixels) in prepared.faces.iter().enumerate() {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &cube,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: u32::try_from(face).expect("face fits u32"),
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                pixels,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(prepared.face_size * 4),
                    rows_per_image: Some(prepared.face_size),
                },
                wgpu::Extent3d {
                    width: prepared.face_size,
                    height: prepared.face_size,
                    depth_or_array_layers: 1,
                },
            );
        }
        let cube_view = cube.create_view(&wgpu::TextureViewDescriptor {
            label: Some("yuyib skybox cube view"),
            dimension: Some(wgpu::TextureViewDimension::Cube),
            ..Default::default()
        });
        let cube_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("yuyib skybox cube sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let layout = Self::bind_group_layout(device);
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("yuyib skybox bind group"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&cube_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&cube_sampler),
                },
            ],
        });
        Self {
            face_size: prepared.face_size,
            _cube: cube,
            _cube_view: cube_view,
            _cube_sampler: cube_sampler,
            bind_group,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct SkyCameraUniform {
    inv_view_proj: [f32; 16],
}

/// Fullscreen cubemap skybox drawer.
pub struct SkyboxRenderer3d {
    pipeline: wgpu::RenderPipeline,
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
}

impl SkyboxRenderer3d {
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
    #[must_use]
    pub fn new_for_frame(frame: &RenderFrame<'_>) -> Self {
        Self::create(frame.device(), frame.surface_format(), frame.depth_format())
    }

    /// Draws the skybox into the current surface colour target.
    ///
    /// Prefer [`DepthLoad::Load`] after opaque geometry. When the sky is the
    /// first depth consumer, pass [`DepthLoad::Clear`].
    ///
    /// # Errors
    ///
    /// Returns [`SkyboxRenderError`] when the camera cannot be projected.
    pub fn draw(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        skybox: &GpuSkybox3d,
        depth_load: DepthLoad,
    ) -> Result<(), SkyboxRenderError> {
        let inv_view_proj = sky_inverse_view_projection(camera, frame.draw_size())?;
        let uniform = SkyCameraUniform { inv_view_proj };
        frame
            .queue()
            .write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&uniform));
        frame.with_surface_pass_with_depth(
            wgpu::LoadOp::Load,
            depth_load.operation(),
            |pass| {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, Some(&self.camera_bind_group), &[]);
                pass.set_bind_group(1, Some(skybox.bind_group()), &[]);
                pass.draw(0..3, 0..1);
            },
        );
        Ok(())
    }

    fn create(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        depth_format: wgpu::TextureFormat,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("yuyib skybox WGSL"),
            source: wgpu::ShaderSource::Wgsl(SKYBOX_WGSL.into()),
        });
        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("yuyib skybox camera layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(size_of::<SkyCameraUniform>() as u64),
                },
                count: None,
            }],
        });
        let sky_layout = GpuSkybox3d::bind_group_layout(device);
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("yuyib skybox pipeline layout"),
            bind_group_layouts: &[Some(&camera_layout), Some(&sky_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("yuyib skybox pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
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
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("yuyib skybox camera buffer"),
            size: size_of::<SkyCameraUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("yuyib skybox camera bind group"),
            layout: &camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });
        Self {
            pipeline,
            camera_buffer,
            camera_bind_group,
        }
    }
}

fn sky_inverse_view_projection(
    camera: Camera3d,
    surface_size: [u32; 2],
) -> Result<[f32; 16], MeshRenderError> {
    // Rebuild the same basis as Camera3d::view_projection, but drop translation
    // so the sky is camera-centric, then invert P * V_rot for NDC→direction.
    if surface_size[0] == 0 || surface_size[1] == 0 {
        return Err(MeshRenderError::InvalidCamera(
            "surface dimensions must be non-zero",
        ));
    }
    let position = camera.position;
    let target = camera.target;
    let up = camera.up;
    if !(position.iter().chain(target.iter()).chain(up.iter())).all(|v| v.is_finite()) {
        return Err(MeshRenderError::InvalidCamera(
            "camera position, target and up must be finite",
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

    let forward = normalize3([
        target[0] - position[0],
        target[1] - position[1],
        target[2] - position[2],
    ])
    .ok_or(MeshRenderError::InvalidCamera(
        "camera position and target must differ",
    ))?;
    let side = normalize3(cross3(forward, up)).ok_or(MeshRenderError::InvalidCamera(
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
        0.0,
        0.0,
        0.0,
        1.0,
    ];
    #[allow(clippy::cast_precision_loss)]
    let aspect = surface_size[0] as f32 / surface_size[1] as f32;
    let focal_length = 1.0 / (camera.vertical_fov_radians * 0.5).tan();
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
        camera.far / (camera.near - camera.far),
        -1.0,
        0.0,
        0.0,
        (camera.near * camera.far) / (camera.near - camera.far),
        0.0,
    ];
    let view_proj = multiply_matrix4(projection, view);
    invert_matrix4(view_proj).ok_or(MeshRenderError::InvalidCamera(
        "sky view-projection is not invertible",
    ))
}

fn cross3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalize3(value: [f32; 3]) -> Option<[f32; 3]> {
    let length_sq = value[0].mul_add(value[0], value[1].mul_add(value[1], value[2] * value[2]));
    if !length_sq.is_finite() || length_sq <= f32::EPSILON {
        return None;
    }
    let reciprocal = length_sq.sqrt().recip();
    let normalized = [
        value[0] * reciprocal,
        value[1] * reciprocal,
        value[2] * reciprocal,
    ];
    normalized.iter().all(|v| v.is_finite()).then_some(normalized)
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

fn invert_matrix4(matrix: [f32; 16]) -> Option<[f32; 16]> {
    // Classic adjugate inverse for column-major mat4.
    let m = matrix;
    let mut inv = [0.0_f32; 16];
    inv[0] = m[5] * m[10] * m[15] - m[5] * m[11] * m[14] - m[9] * m[6] * m[15]
        + m[9] * m[7] * m[14]
        + m[13] * m[6] * m[11]
        - m[13] * m[7] * m[10];
    inv[4] = -m[4] * m[10] * m[15] + m[4] * m[11] * m[14] + m[8] * m[6] * m[15]
        - m[8] * m[7] * m[14]
        - m[12] * m[6] * m[11]
        + m[12] * m[7] * m[10];
    inv[8] = m[4] * m[9] * m[15] - m[4] * m[11] * m[13] - m[8] * m[5] * m[15]
        + m[8] * m[7] * m[13]
        + m[12] * m[5] * m[11]
        - m[12] * m[7] * m[9];
    inv[12] = -m[4] * m[9] * m[14] + m[4] * m[10] * m[13] + m[8] * m[5] * m[14]
        - m[8] * m[6] * m[13]
        - m[12] * m[5] * m[10]
        + m[12] * m[6] * m[9];
    inv[1] = -m[1] * m[10] * m[15] + m[1] * m[11] * m[14] + m[9] * m[2] * m[15]
        - m[9] * m[3] * m[14]
        - m[13] * m[2] * m[11]
        + m[13] * m[3] * m[10];
    inv[5] = m[0] * m[10] * m[15] - m[0] * m[11] * m[14] - m[8] * m[2] * m[15]
        + m[8] * m[3] * m[14]
        + m[12] * m[2] * m[11]
        - m[12] * m[3] * m[10];
    inv[9] = -m[0] * m[9] * m[15] + m[0] * m[11] * m[13] + m[8] * m[1] * m[15]
        - m[8] * m[3] * m[13]
        - m[12] * m[1] * m[11]
        + m[12] * m[3] * m[9];
    inv[13] = m[0] * m[9] * m[14] - m[0] * m[10] * m[13] - m[8] * m[1] * m[14]
        + m[8] * m[2] * m[13]
        + m[12] * m[1] * m[10]
        - m[12] * m[2] * m[9];
    inv[2] = m[1] * m[6] * m[15] - m[1] * m[7] * m[14] - m[5] * m[2] * m[15]
        + m[5] * m[3] * m[14]
        + m[13] * m[2] * m[7]
        - m[13] * m[3] * m[6];
    inv[6] = -m[0] * m[6] * m[15] + m[0] * m[7] * m[14] + m[4] * m[2] * m[15]
        - m[4] * m[3] * m[14]
        - m[12] * m[2] * m[7]
        + m[12] * m[3] * m[6];
    inv[10] = m[0] * m[5] * m[15] - m[0] * m[7] * m[13] - m[4] * m[1] * m[15]
        + m[4] * m[3] * m[13]
        + m[12] * m[1] * m[7]
        - m[12] * m[3] * m[5];
    inv[14] = -m[0] * m[5] * m[14] + m[0] * m[6] * m[13] + m[4] * m[1] * m[14]
        - m[4] * m[2] * m[13]
        - m[12] * m[1] * m[6]
        + m[12] * m[2] * m[5];
    inv[3] = -m[1] * m[6] * m[11] + m[1] * m[7] * m[10] + m[5] * m[2] * m[11]
        - m[5] * m[3] * m[10]
        - m[9] * m[2] * m[7]
        + m[9] * m[3] * m[6];
    inv[7] = m[0] * m[6] * m[11] - m[0] * m[7] * m[10] - m[4] * m[2] * m[11]
        + m[4] * m[3] * m[10]
        + m[8] * m[2] * m[7]
        - m[8] * m[3] * m[6];
    inv[11] = -m[0] * m[5] * m[11] + m[0] * m[7] * m[9] + m[4] * m[1] * m[11]
        - m[4] * m[3] * m[9]
        - m[8] * m[1] * m[7]
        + m[8] * m[3] * m[5];
    inv[15] = m[0] * m[5] * m[10] - m[0] * m[6] * m[9] - m[4] * m[1] * m[10]
        + m[4] * m[2] * m[9]
        + m[8] * m[1] * m[6]
        - m[8] * m[2] * m[5];

    let det = m[0] * inv[0] + m[1] * inv[4] + m[2] * inv[8] + m[3] * inv[12];
    if !det.is_finite() || det.abs() <= f32::EPSILON {
        return None;
    }
    let inv_det = det.recip();
    for value in &mut inv {
        *value *= inv_det;
        if !value.is_finite() {
            return None;
        }
    }
    Some(inv)
}

#[cfg(test)]
mod tests {
    use super::{PreparedSkybox3d, SkyboxError, invert_matrix4, multiply_matrix4};
    use crate::{
        GgxCookConfig, PreparedEquirectEnvironment3d, cook_ggx_specular_ibl,
    };

    #[test]
    fn rejects_bad_face_size() {
        let faces = std::array::from_fn(|_| vec![0_u8; 4]);
        assert_eq!(
            PreparedSkybox3d::from_rgba8(3, faces).err(),
            Some(SkyboxError::InvalidFaceSize)
        );
    }

    #[test]
    fn from_specular_mip0_matches_face_size() {
        let env = PreparedEquirectEnvironment3d::synthetic_outdoor_probe().expect("equirect");
        let specular = cook_ggx_specular_ibl(&env, GgxCookConfig::smoke()).expect("cook");
        let sky = PreparedSkybox3d::from_specular_mip0(&specular).expect("sky");
        assert_eq!(sky.face_size(), specular.face_size());
        let up = sky.face_rgba8(2).expect("+Y");
        let down = sky.face_rgba8(3).expect("-Y");
        let up_luma = mean_luma(up);
        let down_luma = mean_luma(down);
        assert!(up_luma > down_luma, "sky {up_luma} vs ground {down_luma}");
    }

    #[test]
    fn invert_roundtrips_identity() {
        let identity = [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ];
        let inv = invert_matrix4(identity).expect("invert");
        let product = multiply_matrix4(identity, inv);
        for (index, value) in product.iter().enumerate() {
            let expected = if index % 5 == 0 { 1.0 } else { 0.0 };
            assert!((value - expected).abs() < 1e-5, "index {index}");
        }
    }

    fn mean_luma(pixels: &[u8]) -> f32 {
        let mut sum = 0.0_f32;
        let mut count = 0_u32;
        for texel in pixels.chunks_exact(4) {
            sum += 0.2126 * f32::from(texel[0])
                + 0.7152 * f32::from(texel[1])
                + 0.0722 * f32::from(texel[2]);
            count += 1;
        }
        sum / count.max(1) as f32
    }
}
