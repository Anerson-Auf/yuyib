//! GPU-instanced 2D sprites for Yuyib's shared renderer.
//!
//! The crate intentionally starts with one texture per draw batch. [`SpriteDraw`]
//! values are sorted stably by `layer`, then converted to instance data. When
//! several textures appear in one frame, submit them together through
//! [`SpriteRenderer::draw_prepared_batches`] so instance uploads cannot race
//! render passes on the shared buffer.
//!
//! # Example
//!
//! ```no_run
//! # use yuyib_2d::{PixelPoint, TextureRegion, TextureSize};
//! # use yuyib_assets::Assets;
//! # use yuyib_image::DecodedImage;
//! # use yuyib_render::Renderer;
//! # use yuyib_render_2d::{Camera2d, SpriteDraw, SpriteRenderer};
//! # fn demo(renderer: &Renderer, image: &DecodedImage) -> Result<(), Box<dyn std::error::Error>> {
//! let mut textures = Assets::new();
//! let texture_id = textures.insert(image.texture().clone());
//! let region = TextureRegion::new(texture_id, image.texture().size(), PixelPoint::default(), image.texture().size())?;
//! let mut sprites = SpriteRenderer::new(renderer);
//! let gpu_texture = sprites.upload_rgba8(renderer, texture_id, image)?;
//! let draw = SpriteDraw::new(region).with_position([24.0, 16.0]);
//! let batch = sprites.prepare(texture_id, image.texture().size(), [draw])?;
//! let _camera = Camera2d::default();
//! # let _ = (gpu_texture, batch);
//! # Ok(()) }
//! ```
//!
//! # Limits and caveats
//!
//! - A call to [`SpriteRenderer::draw`] accepts exactly one GPU texture. Submit
//!   several textures in one frame via [`SpriteRenderer::draw_prepared_batches`].
//!   Do not interleave `queue.write_buffer` of a shared instance buffer with
//!   multiple surface passes before `queue.submit` — every pass may observe only
//!   the last upload. Prefer one atlas texture when a single draw call matters.
//! - The renderer provides stable ordering *within a batch* by integer `layer`.
//!   It cannot order draws across separate calls or phases.
//! - Sprites are alpha blended and have no depth buffer. `layer` is the explicit
//!   painter-order mechanism for this MVP.

#![forbid(unsafe_code)]

use std::{error::Error, fmt, mem::size_of};

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;
use yuyib_2d::{
    PixelPoint, Texture, TextureAlphaMode, TextureColorSpace, TextureHandle, TextureRegion,
    TextureSize,
};
use yuyib_image::DecodedImage;
use yuyib_render::{RenderFrame, Renderer, wgpu};

/// Retained, GPU-native vector meshes and an instance-based scene facade.
///
/// Unlike browser Canvas commands, vector paths are tessellated before they
/// enter this API and uploaded only when their immutable mesh changes. Normal
/// frames update a compact transform/tint instance buffer.
pub mod vector;

pub use vector::{
    GpuVectorMesh2d, RetainedVectorScene2d, VectorDraw2d, VectorDrawStats2d, VectorMesh2d,
    VectorFill2d, VectorGradientStop2d, VectorMeshError2d, VectorMeshId2d, VectorPath2d,
    VectorPathCommand2d, VectorPathError2d, VectorRenderBudget2d, VectorRenderer2d,
    VectorSceneError2d, VectorVertex2d,
};

/// An orthographic 2D camera with its origin at the centre of the presentation surface.
///
/// World units are pixels by default. Increasing `pixels_per_unit` zooms in;
/// camera `position` selects the world coordinate shown at the screen centre.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera2d {
    /// World coordinate at the centre of the screen.
    pub position: [f32; 2],
    /// Number of physical pixels represented by one world unit.
    pub pixels_per_unit: f32,
}

impl Camera2d {
    /// Creates a camera centered at `position`.
    ///
    /// `pixels_per_unit` must be finite and greater than zero. Invalid input is
    /// rejected when the camera is used to draw, rather than silently producing
    /// a degenerate projection.
    #[must_use]
    pub const fn new(position: [f32; 2], pixels_per_unit: f32) -> Self {
        Self {
            position,
            pixels_per_unit,
        }
    }

    /// Returns the visible world-space origin and size for a physical surface.
    ///
    /// # Errors
    ///
    /// Returns [`SpriteRenderError::InvalidCamera`] for a zero surface or
    /// non-finite/degenerate camera values.
    #[allow(clippy::cast_precision_loss)]
    pub fn viewport(
        self,
        surface_size: [u32; 2],
    ) -> Result<([f32; 2], [f32; 2]), SpriteRenderError> {
        self.projection(surface_size)?;
        let size = [
            surface_size[0] as f32 / self.pixels_per_unit,
            surface_size[1] as f32 / self.pixels_per_unit,
        ];
        let origin = [
            self.position[0] - size[0] * 0.5,
            self.position[1] - size[1] * 0.5,
        ];
        Ok((origin, size))
    }

    /// Converts a physical pixel coordinate from top-left screen space to world space.
    ///
    /// # Errors
    ///
    /// Returns [`SpriteRenderError::InvalidCamera`] for invalid camera/surface
    /// data or a non-finite coordinate.
    pub fn screen_to_world(
        self,
        screen: [f32; 2],
        surface_size: [u32; 2],
    ) -> Result<[f32; 2], SpriteRenderError> {
        if !screen.iter().all(|value| value.is_finite()) {
            return Err(SpriteRenderError::InvalidCamera(
                "screen coordinate must be finite",
            ));
        }
        let (origin, _) = self.viewport(surface_size)?;
        Ok([
            origin[0] + screen[0] / self.pixels_per_unit,
            origin[1] + screen[1] / self.pixels_per_unit,
        ])
    }

    /// Converts a world position to a physical pixel coordinate from screen top-left.
    ///
    /// The result may be outside the surface; this is useful for indicators and
    /// explicit culling rather than silently clamping input.
    ///
    /// # Errors
    ///
    /// Returns [`SpriteRenderError::InvalidCamera`] for invalid camera/surface
    /// data or a non-finite coordinate.
    pub fn world_to_screen(
        self,
        world: [f32; 2],
        surface_size: [u32; 2],
    ) -> Result<[f32; 2], SpriteRenderError> {
        if !world.iter().all(|value| value.is_finite()) {
            return Err(SpriteRenderError::InvalidCamera(
                "world coordinate must be finite",
            ));
        }
        let (origin, _) = self.viewport(surface_size)?;
        Ok([
            (world[0] - origin[0]) * self.pixels_per_unit,
            (world[1] - origin[1]) * self.pixels_per_unit,
        ])
    }

    /// Produces the column-major orthographic transform for `surface_size`.
    ///
    /// The world Y axis grows downward, matching conventional 2D texture and
    /// window coordinates. The returned matrix is suitable for WGSL `mat4x4`.
    ///
    /// # Errors
    ///
    /// Returns [`SpriteRenderError::InvalidCamera`] if the camera or surface
    /// size cannot represent a finite projection.
    #[allow(clippy::cast_precision_loss)] // WGPU texture/surface dimensions are far below f32's exact integer range.
    pub fn projection(self, surface_size: [u32; 2]) -> Result<[f32; 16], SpriteRenderError> {
        if surface_size[0] == 0 || surface_size[1] == 0 {
            return Err(SpriteRenderError::InvalidCamera(
                "surface dimensions must be non-zero",
            ));
        }
        if !self.pixels_per_unit.is_finite() || self.pixels_per_unit <= 0.0 {
            return Err(SpriteRenderError::InvalidCamera(
                "pixels_per_unit must be finite and greater than zero",
            ));
        }
        if !self.position[0].is_finite() || !self.position[1].is_finite() {
            return Err(SpriteRenderError::InvalidCamera(
                "camera position must be finite",
            ));
        }

        let x_scale = 2.0 * self.pixels_per_unit / surface_size[0] as f32;
        let y_scale = -2.0 * self.pixels_per_unit / surface_size[1] as f32;
        let matrix = [
            x_scale,
            0.0,
            0.0,
            0.0,
            0.0,
            y_scale,
            0.0,
            0.0,
            0.0,
            0.0,
            1.0,
            0.0,
            -self.position[0] * x_scale,
            -self.position[1] * y_scale,
            0.0,
            1.0,
        ];
        if matrix.iter().any(|value| !value.is_finite()) {
            return Err(SpriteRenderError::InvalidCamera(
                "camera projection contains non-finite values",
            ));
        }
        Ok(matrix)
    }
}

impl Default for Camera2d {
    fn default() -> Self {
        Self::new([0.0, 0.0], 1.0)
    }
}

/// One user-facing sprite submission.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpriteDraw {
    /// A validated source rectangle inside its texture asset.
    pub region: TextureRegion,
    /// World-space centre position.
    pub position: [f32; 2],
    /// World-space width and height. Negative values mirror the sprite.
    pub size: [f32; 2],
    /// Clockwise rotation in radians because world Y grows downward.
    pub rotation_radians: f32,
    /// Straight-alpha tint multiplied with sampled texture colour.
    pub tint: [f32; 4],
    /// Painter order. Higher layers are drawn after lower layers.
    pub layer: i32,
}

impl SpriteDraw {
    /// Creates a unit-size, untinted sprite from `region`.
    #[must_use]
    pub const fn new(region: TextureRegion) -> Self {
        Self {
            region,
            position: [0.0, 0.0],
            size: [1.0, 1.0],
            rotation_radians: 0.0,
            tint: [1.0, 1.0, 1.0, 1.0],
            layer: 0,
        }
    }

    /// Sets the world-space centre position.
    #[must_use]
    pub const fn with_position(mut self, position: [f32; 2]) -> Self {
        self.position = position;
        self
    }

    /// Sets the world-space sprite size.
    #[must_use]
    pub const fn with_size(mut self, size: [f32; 2]) -> Self {
        self.size = size;
        self
    }

    /// Sets the clockwise rotation in radians.
    #[must_use]
    pub const fn with_rotation(mut self, rotation_radians: f32) -> Self {
        self.rotation_radians = rotation_radians;
        self
    }

    /// Sets a straight-alpha tint.
    #[must_use]
    pub const fn with_tint(mut self, tint: [f32; 4]) -> Self {
        self.tint = tint;
        self
    }

    /// Sets the stable painter-order layer.
    #[must_use]
    pub const fn with_layer(mut self, layer: i32) -> Self {
        self.layer = layer;
        self
    }
}

/// A GPU texture uploaded from validated row-major RGBA8 pixels.
pub struct GpuSpriteTexture {
    asset: TextureHandle,
    size: TextureSize,
    bind_group: wgpu::BindGroup,
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    sampler: wgpu::Sampler,
}

impl GpuSpriteTexture {
    /// Returns the asset handle this GPU texture represents.
    #[must_use]
    pub const fn asset(&self) -> TextureHandle {
        self.asset
    }

    /// Returns the uploaded source dimensions.
    #[must_use]
    pub const fn size(&self) -> TextureSize {
        self.size
    }

    /// Returns the WGPU texture view for an advanced render phase.
    ///
    /// The view remains valid only while this resource is retained. Typical 2D
    /// code should use [`SpriteRenderer::draw`]; 3D material APIs may bind the
    /// same uploaded RGBA texture through this explicit escape hatch.
    #[must_use]
    pub const fn texture_view(&self) -> &wgpu::TextureView {
        &self.view
    }

    /// Returns the filtering sampler selected during upload.
    #[must_use]
    pub const fn sampler(&self) -> &wgpu::Sampler {
        &self.sampler
    }
}

/// A CPU-prepared, single-texture batch of sorted sprite instances.
#[derive(Clone, Debug)]
pub struct PreparedSpriteBatch {
    texture: TextureHandle,
    texture_size: TextureSize,
    instances: Vec<GpuSpriteInstance>,
}

impl PreparedSpriteBatch {
    /// Returns the texture handle all draw submissions target.
    #[must_use]
    pub const fn texture(&self) -> TextureHandle {
        self.texture
    }

    /// Returns the physical dimensions used to normalize texture regions.
    #[must_use]
    pub const fn texture_size(&self) -> TextureSize {
        self.texture_size
    }

    /// Returns the number of sprites in the batch.
    #[must_use]
    pub fn len(&self) -> usize {
        self.instances.len()
    }

    /// Returns whether this batch contains no sprites.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.instances.is_empty()
    }
}

/// Counts work encoded by [`SpriteRenderer::draw`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SpriteDrawStats {
    /// Number of instances issued to the GPU.
    pub sprites: u32,
    /// Number of indexed draw calls issued. Currently zero or one.
    pub draw_calls: u32,
}

/// A reusable instanced sprite renderer.
pub struct SpriteRenderer {
    pipeline: wgpu::RenderPipeline,
    texture_bind_group_layout: wgpu::BindGroupLayout,
    camera_bind_group: wgpu::BindGroup,
    camera_buffer: wgpu::Buffer,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    instance_buffer: Option<wgpu::Buffer>,
    instance_capacity: u32,
}

impl SpriteRenderer {
    /// Creates shader, quad geometry, and pipeline state for Yuyib's surface format.
    #[must_use]
    pub fn new(renderer: &Renderer) -> Self {
        let color_format = renderer.color_target_format();
        renderer.with_raw_gpu(|device, _queue, _configuration| Self::create(device, color_format))
    }

    /// Creates the sprite pipeline from the GPU objects exposed by one frame.
    ///
    /// This is the high-level application path: it lets an application
    /// render callback lazily initialise 2D resources without taking ownership
    /// of the presentation surface. Keep the resulting renderer as application
    /// state; recreating it every frame unnecessarily recompiles its pipeline.
    ///
    #[must_use]
    pub fn new_for_frame(frame: &RenderFrame<'_>) -> Self {
        Self::create(frame.device(), frame.surface_format())
    }

    /// Uploads a decoded image to a sampled 2D GPU texture.
    ///
    /// The CPU image is revalidated before upload: dimensions must match its
    /// declared metadata, its bytes must be exactly RGBA8, and the device's
    /// maximum 2D texture dimension must not be exceeded.
    ///
    /// # Errors
    ///
    /// Returns [`TextureUploadError`] when source metadata or byte lengths are
    /// inconsistent, or the selected GPU device cannot support the dimensions.
    pub fn upload_rgba8(
        &self,
        renderer: &Renderer,
        asset: TextureHandle,
        image: &DecodedImage,
    ) -> Result<GpuSpriteTexture, TextureUploadError> {
        renderer.with_raw_gpu(|device, queue, _configuration| {
            self.upload_rgba8_with(device, queue, asset, image.texture(), image.pixels())
        })
    }

    /// Uploads a decoded image while recording an application render callback.
    ///
    /// It has the same validation and ownership rules as [`Self::upload_rgba8`]
    /// but obtains the selected device and queue from `frame`. Call this during
    /// one-time scene initialisation, then retain the returned GPU texture.
    ///
    /// # Errors
    ///
    /// Returns [`TextureUploadError`] when source metadata or byte lengths are
    /// inconsistent, or the selected GPU device cannot support the dimensions.
    pub fn upload_rgba8_for_frame(
        &self,
        frame: &RenderFrame<'_>,
        asset: TextureHandle,
        image: &DecodedImage,
    ) -> Result<GpuSpriteTexture, TextureUploadError> {
        self.upload_rgba8_with(
            frame.device(),
            frame.queue(),
            asset,
            image.texture(),
            image.pixels(),
        )
    }

    /// Prepares one texture batch using stable layer ordering.
    ///
    /// The sort is stable: sprites with equal `layer` retain their submission
    /// order. Input from another texture is rejected instead of silently
    /// splitting a call into unpredictable GPU batches.
    ///
    /// # Errors
    ///
    /// Returns [`SpriteRenderError`] when a draw refers to another texture or
    /// contains non-finite transform/tint data.
    pub fn prepare(
        &self,
        texture: TextureHandle,
        texture_size: TextureSize,
        draws: impl IntoIterator<Item = SpriteDraw>,
    ) -> Result<PreparedSpriteBatch, SpriteRenderError> {
        prepare_sprite_batch(texture, texture_size, draws)
    }

    /// Records one alpha-blended indexed instanced pass over the current frame.
    ///
    /// Prefer [`Self::draw_prepared_batches`] when submitting more than one
    /// texture in the same frame: repeated [`Self::draw`] calls that each
    /// `queue.write_buffer` the shared instance storage before `queue.submit`
    /// can make every pass observe only the last upload.
    ///
    /// `batch.texture()` must match `texture.asset()`. An empty batch performs
    /// no buffer update and emits no render pass.
    ///
    /// # Errors
    ///
    /// Returns [`SpriteRenderError`] for mismatched assets, invalid camera data,
    /// or an instance count beyond WGPU's `u32` draw range.
    ///
    /// # Panics
    ///
    /// Panics only if internal renderer state is inconsistent: a non-empty
    /// prepared batch without its required dynamic instance buffer.
    pub fn draw(
        &mut self,
        frame: &mut RenderFrame<'_>,
        camera: Camera2d,
        texture: &GpuSpriteTexture,
        batch: &PreparedSpriteBatch,
    ) -> Result<SpriteDrawStats, SpriteRenderError> {
        self.draw_prepared_batches(frame, camera, &[(texture, batch)])
    }

    /// Draws many prepared texture batches in **one** surface pass.
    ///
    /// Instance data is packed into non-overlapping regions of the shared
    /// instance buffer with a single `queue.write_buffer` before the pass.
    /// That ordering is required: `queue.write_buffer` is not part of the
    /// command encoder, so write→pass→write→pass→submit lets every pass see
    /// only the last write.
    ///
    /// # Errors
    ///
    /// Returns [`SpriteRenderError`] for mismatched assets, invalid camera data,
    /// or an instance count beyond WGPU's `u32` draw range.
    pub fn draw_prepared_batches(
        &mut self,
        frame: &mut RenderFrame<'_>,
        camera: Camera2d,
        batches: &[(&GpuSpriteTexture, &PreparedSpriteBatch)],
    ) -> Result<SpriteDrawStats, SpriteRenderError> {
        let mut ranges = Vec::with_capacity(batches.len());
        let mut packed: Vec<GpuSpriteInstance> = Vec::new();
        for &(texture, batch) in batches {
            if batch.texture != texture.asset {
                return Err(SpriteRenderError::BatchTextureMismatch {
                    batch: batch.texture,
                    gpu: texture.asset,
                });
            }
            if batch.texture_size != texture.size {
                return Err(SpriteRenderError::BatchTextureSizeMismatch {
                    batch: batch.texture_size,
                    gpu: texture.size,
                });
            }
            if batch.instances.is_empty() {
                continue;
            }
            let start = u32::try_from(packed.len()).map_err(|_| {
                SpriteRenderError::TooManySprites {
                    actual: packed.len().saturating_add(batch.instances.len()),
                }
            })?;
            let count = u32::try_from(batch.instances.len()).map_err(|_| {
                SpriteRenderError::TooManySprites {
                    actual: batch.instances.len(),
                }
            })?;
            packed.extend_from_slice(&batch.instances);
            ranges.push((texture, start, count));
        }
        if packed.is_empty() {
            return Ok(SpriteDrawStats::default());
        }

        let total = u32::try_from(packed.len()).map_err(|_| SpriteRenderError::TooManySprites {
            actual: packed.len(),
        })?;
        let projection = camera.projection(frame.draw_size())?;
        frame
            .queue()
            .write_buffer(&self.camera_buffer, 0, bytemuck::cast_slice(&projection));
        self.ensure_instance_capacity(frame.device(), total);
        let instance_buffer = self
            .instance_buffer
            .as_ref()
            .expect("a non-empty sprite batch creates an instance buffer");
        frame
            .queue()
            .write_buffer(instance_buffer, 0, bytemuck::cast_slice(&packed));

        let draw_calls = u32::try_from(ranges.len()).unwrap_or(u32::MAX);
        frame.with_surface_pass(wgpu::LoadOp::Load, |pass| {
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
            pass.set_vertex_buffer(1, instance_buffer.slice(..));
            pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
            for (texture, start, count) in ranges {
                pass.set_bind_group(1, &texture.bind_group, &[]);
                pass.draw_indexed(0..6, 0, start..start + count);
            }
        });
        Ok(SpriteDrawStats {
            sprites: total,
            draw_calls,
        })
    }

    #[allow(clippy::too_many_lines)] // WGPU pipeline descriptors are co-located for auditability.
    fn create(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let camera_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("yuyib sprite camera layout"),
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
        let texture_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("yuyib sprite texture layout"),
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
        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("yuyib sprite camera"),
            size: size_of::<[f32; 16]>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("yuyib sprite camera bind group"),
            layout: &camera_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("yuyib instanced sprite WGSL"),
            source: wgpu::ShaderSource::Wgsl(SPRITE_WGSL.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("yuyib sprite pipeline layout"),
            bind_group_layouts: &[
                Some(&camera_bind_group_layout),
                Some(&texture_bind_group_layout),
            ],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("yuyib instanced sprite pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(QUAD_VERTEX_LAYOUT), Some(SPRITE_INSTANCE_LAYOUT)],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
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
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("yuyib sprite quad vertices"),
            contents: bytemuck::cast_slice(&QUAD_VERTICES),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("yuyib sprite quad indices"),
            contents: bytemuck::cast_slice(&QUAD_INDICES),
            usage: wgpu::BufferUsages::INDEX,
        });
        Self {
            pipeline,
            texture_bind_group_layout,
            camera_bind_group,
            camera_buffer,
            vertex_buffer,
            index_buffer,
            instance_buffer: None,
            instance_capacity: 0,
        }
    }

    fn upload_rgba8_with(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        asset: TextureHandle,
        texture: &Texture,
        pixels: &[u8],
    ) -> Result<GpuSpriteTexture, TextureUploadError> {
        let size = texture.size();
        let width = size.width();
        let height = size.height();
        let expected_bytes = usize::try_from(u64::from(width) * u64::from(height) * 4)
            .map_err(|_| TextureUploadError::ByteSizeOverflow { width, height })?;
        if pixels.len() != expected_bytes {
            return Err(TextureUploadError::ByteLengthMismatch {
                width,
                height,
                actual: pixels.len(),
                expected: expected_bytes,
            });
        }
        if width > device.limits().max_texture_dimension_2d
            || height > device.limits().max_texture_dimension_2d
        {
            return Err(TextureUploadError::DimensionUnsupported {
                width,
                height,
                maximum: device.limits().max_texture_dimension_2d,
            });
        }
        if texture.alpha_mode() == TextureAlphaMode::Premultiplied {
            return Err(TextureUploadError::PremultipliedAlphaUnsupported);
        }
        let format = match texture.color_space() {
            TextureColorSpace::Srgb => wgpu::TextureFormat::Rgba8UnormSrgb,
            TextureColorSpace::Linear => wgpu::TextureFormat::Rgba8Unorm,
        };
        let gpu_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("yuyib sprite RGBA8 texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &gpu_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * width),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        let view = gpu_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("yuyib sprite sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            // Pixel-art default: Linear bleeds neighbouring atlas cells and makes
            // tilemaps shimmer when the camera moves by sub-pixels.
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("yuyib sprite texture bind group"),
            layout: &self.texture_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        Ok(GpuSpriteTexture {
            asset,
            size,
            bind_group,
            _texture: gpu_texture,
            view,
            sampler,
        })
    }

    fn ensure_instance_capacity(&mut self, device: &wgpu::Device, required: u32) {
        if required <= self.instance_capacity {
            return;
        }
        let capacity = required.checked_next_power_of_two().unwrap_or(required);
        self.instance_buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("yuyib sprite instances"),
            size: u64::from(capacity) * size_of::<GpuSpriteInstance>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        self.instance_capacity = capacity;
    }
}

/// Converts and stably sorts one texture's user-facing draw list.
///
/// This free function is useful for ECS systems that prepare render work before
/// a GPU frame is available.
///
/// # Errors
///
/// Returns [`SpriteRenderError`] for an asset mismatch or non-finite draw data.
pub fn prepare_sprite_batch(
    texture: TextureHandle,
    texture_size: TextureSize,
    draws: impl IntoIterator<Item = SpriteDraw>,
) -> Result<PreparedSpriteBatch, SpriteRenderError> {
    let mut ordered: Vec<SpriteDraw> = draws.into_iter().collect();
    ordered.sort_by_key(|draw| draw.layer);
    let mut instances = Vec::with_capacity(ordered.len());
    for draw in ordered {
        if draw.region.texture() != texture {
            return Err(SpriteRenderError::DrawTextureMismatch {
                expected: texture,
                actual: draw.region.texture(),
            });
        }
        validate_region_against_texture(draw.region, texture_size)?;
        validate_draw(draw)?;
        instances.push(GpuSpriteInstance::from_draw(draw, texture_size));
    }
    Ok(PreparedSpriteBatch {
        texture,
        texture_size,
        instances,
    })
}

fn validate_region_against_texture(
    region: TextureRegion,
    texture_size: TextureSize,
) -> Result<(), SpriteRenderError> {
    let origin = region.origin();
    let size = region.size();
    let right = origin.x.checked_add(size.width());
    let bottom = origin.y.checked_add(size.height());
    if right.is_none_or(|value| value > texture_size.width())
        || bottom.is_none_or(|value| value > texture_size.height())
    {
        return Err(SpriteRenderError::RegionOutsideBatchTexture {
            origin,
            size,
            texture_size,
        });
    }
    Ok(())
}

fn validate_draw(draw: SpriteDraw) -> Result<(), SpriteRenderError> {
    let all_finite = draw
        .position
        .iter()
        .chain(draw.size.iter())
        .chain(std::iter::once(&draw.rotation_radians))
        .chain(draw.tint.iter())
        .all(|value| value.is_finite());
    if !all_finite {
        return Err(SpriteRenderError::NonFiniteDraw);
    }
    if draw.size[0] == 0.0 || draw.size[1] == 0.0 {
        return Err(SpriteRenderError::ZeroSizeDraw);
    }
    Ok(())
}

/// A validated texture upload failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextureUploadError {
    /// Width × height × four cannot fit in `usize`.
    ByteSizeOverflow {
        /// Image width.
        width: u32,
        /// Image height.
        height: u32,
    },
    /// The source byte slice is not exactly tightly packed RGBA8.
    ByteLengthMismatch {
        /// Image width.
        width: u32,
        /// Image height.
        height: u32,
        /// Observed byte count.
        actual: usize,
        /// Expected `width * height * 4` count.
        expected: usize,
    },
    /// The selected GPU cannot create a texture of this size.
    DimensionUnsupported {
        /// Image width.
        width: u32,
        /// Image height.
        height: u32,
        /// Device maximum 2D dimension.
        maximum: u32,
    },
    /// The MVP has only a straight-alpha blending pipeline.
    ///
    /// Convert the source to straight alpha before upload, or wait for a
    /// premultiplied-alpha material variant.
    PremultipliedAlphaUnsupported,
}

impl fmt::Display for TextureUploadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ByteSizeOverflow { width, height } => {
                write!(formatter, "RGBA8 byte size overflows for {width}x{height}")
            }
            Self::ByteLengthMismatch {
                width,
                height,
                actual,
                expected,
            } => write!(
                formatter,
                "RGBA8 texture {width}x{height} has {actual} bytes; expected {expected}"
            ),
            Self::DimensionUnsupported {
                width,
                height,
                maximum,
            } => write!(
                formatter,
                "texture {width}x{height} exceeds GPU 2D dimension limit {maximum}"
            ),
            Self::PremultipliedAlphaUnsupported => formatter
                .write_str("premultiplied-alpha textures are not supported by the sprite MVP"),
        }
    }
}

impl Error for TextureUploadError {}

/// A CPU preparation or GPU draw failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpriteRenderError {
    /// A draw submission's region belongs to a different texture asset.
    DrawTextureMismatch {
        /// Texture requested for this batch.
        expected: TextureHandle,
        /// Texture referenced by the draw's region.
        actual: TextureHandle,
    },
    /// A prepared batch was submitted with the wrong GPU texture.
    BatchTextureMismatch {
        /// Texture recorded while preparing the batch.
        batch: TextureHandle,
        /// Asset represented by the GPU texture.
        gpu: TextureHandle,
    },
    /// The GPU texture's dimensions differ from the batch used for UV normalization.
    BatchTextureSizeMismatch {
        /// CPU texture size supplied when the batch was prepared.
        batch: TextureSize,
        /// Dimensions of the uploaded GPU texture.
        gpu: TextureSize,
    },
    /// A sprite region exceeds the texture dimensions passed to batch preparation.
    RegionOutsideBatchTexture {
        /// Top-left source coordinate.
        origin: PixelPoint,
        /// Region dimensions.
        size: TextureSize,
        /// Texture dimensions supplied to batch preparation.
        texture_size: TextureSize,
    },
    /// A draw contains NaN or infinity in a transform or tint field.
    NonFiniteDraw,
    /// A sprite has a zero dimension and would be invisible/ambiguous.
    ZeroSizeDraw,
    /// The camera cannot make a finite projection.
    InvalidCamera(&'static str),
    /// The batch length does not fit in WGPU's `u32` instance count.
    TooManySprites {
        /// Observed sprite count.
        actual: usize,
    },
}

impl fmt::Display for SpriteRenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DrawTextureMismatch { .. } => {
                formatter.write_str("sprite region belongs to a different texture batch")
            }
            Self::BatchTextureMismatch { .. } => {
                formatter.write_str("prepared batch and GPU texture refer to different assets")
            }
            Self::BatchTextureSizeMismatch { .. } => formatter
                .write_str("prepared batch and GPU texture use different source dimensions"),
            Self::RegionOutsideBatchTexture { .. } => {
                formatter.write_str("sprite region exceeds the prepared batch texture dimensions")
            }
            Self::NonFiniteDraw => formatter.write_str("sprite draw contains non-finite data"),
            Self::ZeroSizeDraw => formatter.write_str("sprite draw size must be non-zero"),
            Self::InvalidCamera(reason) => write!(formatter, "invalid 2D camera: {reason}"),
            Self::TooManySprites { actual } => {
                write!(
                    formatter,
                    "sprite batch has {actual} entries; maximum is u32::MAX"
                )
            }
        }
    }
}

impl Error for SpriteRenderError {}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct QuadVertex {
    position: [f32; 2],
    uv: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct GpuSpriteInstance {
    position: [f32; 2],
    size: [f32; 2],
    uv_min: [f32; 2],
    uv_max: [f32; 2],
    tint: [f32; 4],
    rotation_radians: f32,
}

impl GpuSpriteInstance {
    #[allow(clippy::cast_precision_loss)] // Valid texture coordinates are bounded by WGPU's maximum texture dimension.
    fn from_draw(draw: SpriteDraw, texture_size: TextureSize) -> Self {
        let origin: PixelPoint = draw.region.origin();
        let region_size = draw.region.size();
        let texture_width = texture_size.width() as f32;
        let texture_height = texture_size.height() as f32;
        Self {
            position: draw.position,
            size: draw.size,
            uv_min: [
                origin.x as f32 / texture_width,
                origin.y as f32 / texture_height,
            ],
            uv_max: [
                (origin.x + region_size.width()) as f32 / texture_width,
                (origin.y + region_size.height()) as f32 / texture_height,
            ],
            tint: draw.tint,
            rotation_radians: draw.rotation_radians,
        }
    }
}

const QUAD_VERTICES: [QuadVertex; 4] = [
    QuadVertex {
        position: [-0.5, -0.5],
        uv: [0.0, 0.0],
    },
    QuadVertex {
        position: [0.5, -0.5],
        uv: [1.0, 0.0],
    },
    QuadVertex {
        position: [-0.5, 0.5],
        uv: [0.0, 1.0],
    },
    QuadVertex {
        position: [0.5, 0.5],
        uv: [1.0, 1.0],
    },
];
const QUAD_INDICES: [u16; 6] = [0, 1, 2, 2, 1, 3];

const QUAD_VERTEX_LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
    array_stride: size_of::<QuadVertex>() as wgpu::BufferAddress,
    step_mode: wgpu::VertexStepMode::Vertex,
    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2],
};
const SPRITE_INSTANCE_LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
    array_stride: size_of::<GpuSpriteInstance>() as wgpu::BufferAddress,
    step_mode: wgpu::VertexStepMode::Instance,
    attributes: &wgpu::vertex_attr_array![
        2 => Float32x2,
        3 => Float32x2,
        4 => Float32x2,
        5 => Float32x2,
        6 => Float32x4,
        7 => Float32,
    ],
};

const SPRITE_WGSL: &str = r"
struct Camera {
    projection: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var sprite_texture: texture_2d<f32>;
@group(1) @binding(1) var sprite_sampler: sampler;

struct VertexInput {
    @location(0) local_position: vec2<f32>,
    @location(1) local_uv: vec2<f32>,
    @location(2) position: vec2<f32>,
    @location(3) size: vec2<f32>,
    @location(4) uv_min: vec2<f32>,
    @location(5) uv_max: vec2<f32>,
    @location(6) tint: vec4<f32>,
    @location(7) rotation_radians: f32,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) tint: vec4<f32>,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    let c = cos(input.rotation_radians);
    let s = sin(input.rotation_radians);
    let scaled = input.local_position * input.size;
    let rotated = vec2<f32>(
        scaled.x * c - scaled.y * s,
        scaled.x * s + scaled.y * c,
    );
    var output: VertexOutput;
    output.clip_position = camera.projection * vec4<f32>(input.position + rotated, 0.0, 1.0);
    output.uv = mix(input.uv_min, input.uv_max, input.local_uv);
    output.tint = input.tint;
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(sprite_texture, sprite_sampler, input.uv) * input.tint;
}
";

#[cfg(test)]
#[allow(clippy::float_cmp)] // Test inputs are exactly representable and verify deterministic ordering/projection constants.
mod tests {
    use yuyib_2d::{PixelPoint, Texture, TextureRegion, TextureSize};
    use yuyib_assets::Assets;

    use super::*;

    fn region(textures: &mut Assets<Texture>) -> TextureRegion {
        let size = TextureSize::new(32, 16).expect("valid test texture size");
        let texture = textures.insert(Texture::new(size));
        TextureRegion::new(texture, size, PixelPoint::default(), size)
            .expect("full texture region is valid")
    }

    #[test]
    fn prepare_uses_stable_layer_order() {
        let mut textures = Assets::new();
        let region = region(&mut textures);
        let texture = region.texture();
        let batch = prepare_sprite_batch(
            texture,
            textures.get(texture).expect("texture exists").size(),
            [
                SpriteDraw::new(region)
                    .with_position([30.0, 0.0])
                    .with_layer(2),
                SpriteDraw::new(region)
                    .with_position([10.0, 0.0])
                    .with_layer(1),
                SpriteDraw::new(region)
                    .with_position([20.0, 0.0])
                    .with_layer(1),
            ],
        )
        .expect("one texture batch is valid");

        assert_eq!(batch.len(), 3);
        assert_eq!(batch.instances[0].position, [10.0, 0.0]);
        assert_eq!(batch.instances[1].position, [20.0, 0.0]);
        assert_eq!(batch.instances[2].position, [30.0, 0.0]);
    }

    #[test]
    fn prepare_rejects_mixed_texture_assets() {
        let mut textures = Assets::new();
        let first = region(&mut textures);
        let second = region(&mut textures);
        let texture_size = textures
            .get(first.texture())
            .expect("texture exists")
            .size();
        let error = prepare_sprite_batch(first.texture(), texture_size, [SpriteDraw::new(second)])
            .expect_err("mixed asset batch is invalid");

        assert!(matches!(
            error,
            SpriteRenderError::DrawTextureMismatch { .. }
        ));
    }

    #[test]
    fn projection_uses_downward_world_y_axis() {
        let projection = Camera2d::new([0.0, 0.0], 1.0)
            .projection([100, 50])
            .expect("valid camera");

        assert_eq!(projection[0], 0.02);
        assert_eq!(projection[5], -0.04);
    }

    #[test]
    fn camera_screen_world_conversion_round_trips_with_zoom() {
        let camera = Camera2d::new([100.0, 50.0], 2.0);
        let screen = [25.0, 75.0];
        let world = camera
            .screen_to_world(screen, [200, 100])
            .expect("valid coordinate");
        assert_eq!(world, [62.5, 62.5]);
        assert_eq!(
            camera
                .world_to_screen(world, [200, 100])
                .expect("valid coordinate"),
            screen
        );
    }
}
