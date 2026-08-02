//! Bounded glyph rasterization, atlas packing, draw-list output, and WGPU pass.
//!
//! [`TextRasterizer`] consumes [`yuyib_ui_text::ShapedText`] from an equivalent
//! explicit font source, uses `cosmic-text` 0.19's [`SwashCache`] to rasterize
//! glyphs, and stores RGBA pixels in a deterministic shelf atlas.
//! [`TextGlyphRenderer`] uploads or updates the explicitly supplied RGBA8
//! atlas and draws its [`TextDrawList`] as alpha-blended WGPU quads. The GPU
//! pass has an explicit physical viewport plus an optional bounded scissor;
//! it deliberately has no UI tree integration, nested clipping, scrolling,
//! text editing, IME, global cache, or system-font discovery.

#![forbid(unsafe_code)]

use std::{
    collections::HashMap,
    error::Error,
    fmt,
    fs::File,
    io::{self, Read},
    mem::size_of,
    path::PathBuf,
};

use bytemuck::{Pod, Zeroable};
use cosmic_text::{
    CacheKey, CacheKeyFlags, FontSystem, SwashCache, SwashContent,
    fontdb::{Database, ID, Weight},
};
use yuyib_render::{RenderFrame, Renderer, wgpu};
use yuyib_ui_text::{FontSource, PositionedGlyph, ShapedText};

/// Immutable RGBA colour multiplied by a later text renderer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextColor {
    /// Red channel in linear or sRGB policy selected by the later renderer.
    pub red: f32,
    /// Green channel in linear or sRGB policy selected by the later renderer.
    pub green: f32,
    /// Blue channel in linear or sRGB policy selected by the later renderer.
    pub blue: f32,
    /// Alpha channel.
    pub alpha: f32,
}

impl TextColor {
    /// Creates a colour after validating finite channels in the inclusive 0–1 range.
    ///
    /// # Errors
    ///
    /// Returns [`TextRenderError::InvalidColor`] for invalid channels.
    pub fn new(red: f32, green: f32, blue: f32, alpha: f32) -> Result<Self, TextRenderError> {
        let color = Self {
            red,
            green,
            blue,
            alpha,
        };
        if [red, green, blue, alpha]
            .iter()
            .any(|channel| !channel.is_finite() || !(0.0..=1.0).contains(channel))
        {
            return Err(TextRenderError::InvalidColor);
        }
        Ok(color)
    }

    /// Returns opaque white, suitable for alpha-mask glyph pixels.
    #[must_use]
    pub const fn white() -> Self {
        Self {
            red: 1.0,
            green: 1.0,
            blue: 1.0,
            alpha: 1.0,
        }
    }
}

/// Bounded CPU atlas policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GlyphAtlasConfig {
    /// Atlas width in physical pixels.
    pub width: u32,
    /// Atlas height in physical pixels.
    pub height: u32,
    /// Empty pixel border around every rasterized glyph.
    pub padding: u32,
    /// Maximum cached glyph regions in this atlas.
    pub max_glyphs: usize,
    /// Maximum RGBA atlas allocation in bytes.
    pub max_atlas_bytes: usize,
    /// Maximum bytes read from an explicit rasterizer font source.
    pub max_font_bytes: usize,
}

impl Default for GlyphAtlasConfig {
    fn default() -> Self {
        Self {
            width: 2048,
            height: 2048,
            padding: 1,
            max_glyphs: 32_768,
            max_atlas_bytes: 32 * 1024 * 1024,
            max_font_bytes: 64 * 1024 * 1024,
        }
    }
}

impl GlyphAtlasConfig {
    /// Validates dimensions and all bounded allocation/cache policies.
    ///
    /// # Errors
    ///
    /// Returns [`TextRenderError::InvalidAtlasConfig`] or
    /// [`TextRenderError::AtlasTooLarge`].
    pub fn validate(self) -> Result<(), TextRenderError> {
        if self.width == 0
            || self.height == 0
            || self.max_glyphs == 0
            || self.max_atlas_bytes == 0
            || self.max_font_bytes == 0
        {
            return Err(TextRenderError::InvalidAtlasConfig);
        }
        let bytes = rgba_bytes_len(self.width, self.height)?;
        if bytes > self.max_atlas_bytes {
            return Err(TextRenderError::AtlasTooLarge {
                actual: bytes,
                limit: self.max_atlas_bytes,
            });
        }
        Ok(())
    }
}

/// A caller-provided RGBA8 bitmap for low-level atlas insertion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GlyphBitmap {
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// Row-major RGBA8 pixels without row padding.
    pub pixels: Vec<u8>,
}

impl GlyphBitmap {
    /// Validates and creates an RGBA8 bitmap.
    ///
    /// # Errors
    ///
    /// Returns [`TextRenderError::InvalidBitmap`] for empty or mismatched data.
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self, TextRenderError> {
        let expected = rgba_bytes_len(width, height)?;
        if width == 0 || height == 0 || pixels.len() != expected {
            return Err(TextRenderError::InvalidBitmap);
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }
}

/// Pixel rectangle allocated inside a [`GlyphAtlas`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AtlasRegion {
    /// Left atlas pixel.
    pub x: u32,
    /// Top atlas pixel.
    pub y: u32,
    /// Bitmap width in pixels.
    pub width: u32,
    /// Bitmap height in pixels.
    pub height: u32,
}

impl AtlasRegion {
    /// Returns normalized texture coordinates as `[left, top, right, bottom]`.
    #[must_use]
    pub fn uv(self, atlas_width: u32, atlas_height: u32) -> [f32; 4] {
        let width = to_f32(atlas_width);
        let height = to_f32(atlas_height);
        [
            to_f32(self.x) / width,
            to_f32(self.y) / height,
            to_f32(self.x.saturating_add(self.width)) / width,
            to_f32(self.y.saturating_add(self.height)) / height,
        ]
    }
}

/// Deterministic shelf-packed RGBA8 glyph atlas.
pub struct GlyphAtlas {
    config: GlyphAtlasConfig,
    pixels: Vec<u8>,
    cursor_x: u32,
    cursor_y: u32,
    row_height: u32,
    glyph_count: usize,
}

impl GlyphAtlas {
    /// Allocates an empty bounded RGBA8 atlas.
    ///
    /// # Errors
    ///
    /// Returns a configuration or allocation-bound error.
    pub fn new(config: GlyphAtlasConfig) -> Result<Self, TextRenderError> {
        config.validate()?;
        let bytes = rgba_bytes_len(config.width, config.height)?;
        Ok(Self {
            config,
            pixels: vec![0; bytes],
            cursor_x: 0,
            cursor_y: 0,
            row_height: 0,
            glyph_count: 0,
        })
    }

    /// Returns the immutable atlas policy.
    #[must_use]
    pub const fn config(&self) -> GlyphAtlasConfig {
        self.config
    }

    /// Returns the atlas size in pixels.
    #[must_use]
    pub const fn size(&self) -> [u32; 2] {
        [self.config.width, self.config.height]
    }

    /// Returns cached region count.
    #[must_use]
    pub const fn glyph_count(&self) -> usize {
        self.glyph_count
    }

    /// Returns row-major RGBA8 atlas data for [`TextGlyphRenderer`] upload/update.
    #[must_use]
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Inserts one low-level bitmap with deterministic shelf placement.
    ///
    /// Failed placement does not modify atlas pixels or allocator state.
    ///
    /// # Errors
    ///
    /// Returns cache or atlas-capacity errors.
    pub fn insert_bitmap(&mut self, bitmap: &GlyphBitmap) -> Result<AtlasRegion, TextRenderError> {
        if self.glyph_count == self.config.max_glyphs {
            return Err(TextRenderError::GlyphCacheFull {
                limit: self.config.max_glyphs,
            });
        }
        let region = self.allocate(bitmap.width, bitmap.height)?;
        self.copy_bitmap(region, bitmap);
        self.glyph_count = self.glyph_count.saturating_add(1);
        Ok(region)
    }

    fn allocate(&mut self, width: u32, height: u32) -> Result<AtlasRegion, TextRenderError> {
        let horizontal = width
            .checked_add(self.config.padding.saturating_mul(2))
            .ok_or(TextRenderError::AtlasFull)?;
        let vertical = height
            .checked_add(self.config.padding.saturating_mul(2))
            .ok_or(TextRenderError::AtlasFull)?;
        if horizontal > self.config.width || vertical > self.config.height {
            return Err(TextRenderError::GlyphTooLarge {
                width,
                height,
                atlas_width: self.config.width,
                atlas_height: self.config.height,
            });
        }
        let mut cursor_x = self.cursor_x;
        let mut cursor_y = self.cursor_y;
        let mut row_height = self.row_height;
        if cursor_x.saturating_add(horizontal) > self.config.width {
            cursor_x = 0;
            cursor_y = cursor_y.saturating_add(row_height);
            row_height = 0;
        }
        if cursor_y.saturating_add(vertical) > self.config.height {
            return Err(TextRenderError::AtlasFull);
        }
        let region = AtlasRegion {
            x: cursor_x.saturating_add(self.config.padding),
            y: cursor_y.saturating_add(self.config.padding),
            width,
            height,
        };
        self.cursor_x = cursor_x.saturating_add(horizontal);
        self.cursor_y = cursor_y;
        self.row_height = row_height.max(vertical);
        Ok(region)
    }

    fn copy_bitmap(&mut self, region: AtlasRegion, bitmap: &GlyphBitmap) {
        let atlas_width = usize::try_from(self.config.width).unwrap_or(usize::MAX);
        let width = usize::try_from(region.width).unwrap_or(usize::MAX);
        let height = usize::try_from(region.height).unwrap_or(usize::MAX);
        let origin_x = usize::try_from(region.x).unwrap_or(usize::MAX);
        let origin_y = usize::try_from(region.y).unwrap_or(usize::MAX);
        for row in 0..height {
            let destination = ((origin_y + row) * atlas_width + origin_x) * 4;
            let source = row * width * 4;
            let length = width * 4;
            self.pixels[destination..destination + length]
                .copy_from_slice(&bitmap.pixels[source..source + length]);
        }
    }
}

/// One logical text glyph quad referencing a [`GlyphAtlas`] region.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextQuad {
    /// Logical left edge.
    pub x: f32,
    /// Logical top edge.
    pub y: f32,
    /// Pixel-space raster width represented in logical units at scale 1.
    pub width: f32,
    /// Pixel-space raster height represented in logical units at scale 1.
    pub height: f32,
    /// Normalized atlas UV rectangle `[left, top, right, bottom]`.
    pub uv: [f32; 4],
    /// Per-quad colour.
    pub color: TextColor,
}

/// CPU draw data for a single atlas texture.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextDrawList {
    atlas_size: [u32; 2],
    quads: Vec<TextQuad>,
}

impl TextDrawList {
    /// Returns the RGBA atlas dimensions expected by this draw list.
    #[must_use]
    pub const fn atlas_size(&self) -> [u32; 2] {
        self.atlas_size
    }

    /// Returns glyph quads in the order supplied by the shaped text.
    #[must_use]
    pub fn quads(&self) -> &[TextQuad] {
        &self.quads
    }

    /// Returns a copy positioned relative to another logical origin.
    ///
    /// Shaping starts at `(0, 0)`. A retained UI or a custom overlay can use
    /// this low-level helper to place that shaped text inside its own rectangle
    /// without touching atlas coordinates, colour, or painter order. The GPU
    /// draw method keeps validating resulting geometry before recording a pass.
    #[must_use]
    pub fn translated(&self, offset_x: f32, offset_y: f32) -> Self {
        let quads = self
            .quads
            .iter()
            .map(|quad| TextQuad {
                x: quad.x + offset_x,
                y: quad.y + offset_y,
                ..*quad
            })
            .collect();
        Self {
            atlas_size: self.atlas_size,
            quads,
        }
    }

    /// Adds quads from the same glyph atlas while preserving their draw order.
    ///
    /// This is the low-level batching primitive for HUDs and editor overlays:
    /// shape/rasterize individual strings as needed, translate them, then append
    /// their lists and issue one GPU draw. An empty receiver adopts the atlas of
    /// the first list. Different atlases cannot share one draw because they need
    /// separate GPU textures.
    ///
    /// # Errors
    ///
    /// Returns [`TextDrawListAppendError`] when both non-empty lists reference
    /// different atlas sizes.
    pub fn append(&mut self, other: &Self) -> Result<(), TextDrawListAppendError> {
        if self.quads.is_empty() {
            self.atlas_size = other.atlas_size;
        } else if self.atlas_size != other.atlas_size {
            return Err(TextDrawListAppendError::AtlasMismatch {
                target: self.atlas_size,
                appended: other.atlas_size,
            });
        }
        self.quads.extend_from_slice(&other.quads);
        Ok(())
    }
}

/// Two [`TextDrawList`] values cannot be merged because their atlas sizes differ.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextDrawListAppendError {
    /// The target and appended lists reference different glyph atlas dimensions.
    AtlasMismatch {
        /// Atlas expected by the target list.
        target: [u32; 2],
        /// Atlas referenced by the appended list.
        appended: [u32; 2],
    },
}

impl fmt::Display for TextDrawListAppendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AtlasMismatch { target, appended } => write!(
                formatter,
                "cannot append text draw lists with atlas sizes {}x{} and {}x{}",
                target[0], target[1], appended[0], appended[1]
            ),
        }
    }
}

impl Error for TextDrawListAppendError {}

/// Result of rasterizing one [`ShapedText`] request into a persistent atlas.
#[derive(Clone, Debug, PartialEq)]
pub struct TextAtlasFrame {
    draw_list: TextDrawList,
    /// Count of glyph bitmaps inserted during this call rather than served from cache.
    pub glyphs_added: usize,
}

impl TextAtlasFrame {
    /// Returns atlas-bound quads ready for [`TextGlyphRenderer`].
    #[must_use]
    pub const fn draw_list(&self) -> &TextDrawList {
        &self.draw_list
    }
}

/// A physical-pixel surface region that receives one glyph pass.
///
/// [`TextQuad`] coordinates are interpreted relative to this viewport's
/// top-left corner. High-DPI conversion remains the caller's responsibility at
/// this API boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextViewport {
    /// Surface pixel left edge.
    pub x: u32,
    /// Surface pixel top edge.
    pub y: u32,
    /// Viewport width in physical pixels.
    pub width: u32,
    /// Viewport height in physical pixels.
    pub height: u32,
}

impl TextViewport {
    /// Creates an explicit physical-pixel viewport.
    #[must_use]
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

/// An optional viewport-local logical-pixel glyph scissor rectangle.
///
/// Negative origins and out-of-viewport values are valid. They are intersected
/// with both the viewport and the presentation surface before WGPU receives a
/// scissor command; an empty intersection emits no draw call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextClipRect {
    /// Horizontal offset from the viewport's left edge.
    pub x: i32,
    /// Vertical offset from the viewport's top edge.
    pub y: i32,
    /// Clip width in physical pixels.
    pub width: u32,
    /// Clip height in physical pixels.
    pub height: u32,
}

impl TextClipRect {
    /// Creates a viewport-local clip rectangle.
    #[must_use]
    pub const fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

/// Bounded GPU upload and geometry policy for [`TextGlyphRenderer`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextGlyphRenderLimits {
    /// Maximum glyph quads accepted by one GPU draw call.
    pub max_quads: usize,
    /// Maximum RGBA8 atlas bytes accepted for upload or update.
    pub max_atlas_bytes: usize,
}

impl Default for TextGlyphRenderLimits {
    fn default() -> Self {
        Self {
            max_quads: 100_000,
            max_atlas_bytes: 32 * 1024 * 1024,
        }
    }
}

impl TextGlyphRenderLimits {
    /// Validates required non-zero GPU work bounds.
    ///
    /// # Errors
    ///
    /// Returns [`TextGpuRenderError::InvalidLimits`] when either bound is zero.
    pub const fn validate(self) -> Result<(), TextGpuRenderError> {
        if self.max_quads == 0 || self.max_atlas_bytes == 0 {
            return Err(TextGpuRenderError::InvalidLimits);
        }
        Ok(())
    }
}

/// Rendering options for one glyph overlay pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextGlyphDrawOptions {
    /// Explicit destination area in the presentation surface.
    pub viewport: TextViewport,
    /// Optional viewport-local clip applied as a WGPU scissor rectangle.
    pub clip: Option<TextClipRect>,
    /// Bounded upload and quad policy.
    pub limits: TextGlyphRenderLimits,
}

impl TextGlyphDrawOptions {
    /// Creates options with no additional clip and default bounds.
    #[must_use]
    pub const fn new(viewport: TextViewport) -> Self {
        Self {
            viewport,
            clip: None,
            limits: TextGlyphRenderLimits {
                max_quads: 100_000,
                max_atlas_bytes: 32 * 1024 * 1024,
            },
        }
    }

    /// Sets an optional viewport-local WGPU scissor rectangle.
    #[must_use]
    pub const fn with_clip(mut self, clip: Option<TextClipRect>) -> Self {
        self.clip = clip;
        self
    }

    /// Sets bounded GPU upload and geometry limits.
    #[must_use]
    pub const fn with_limits(mut self, limits: TextGlyphRenderLimits) -> Self {
        self.limits = limits;
        self
    }
}

/// Resident sampled GPU resource for one [`GlyphAtlas`].
///
/// It is tied to the WGPU device that created it. Recreate it after a renderer
/// or device rebuild; use [`TextGlyphRenderer::update_atlas`] only while the
/// CPU atlas dimensions are unchanged.
pub struct GpuGlyphAtlas {
    size: [u32; 2],
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
}

impl GpuGlyphAtlas {
    /// Returns the resident atlas dimensions in pixels.
    #[must_use]
    pub const fn size(&self) -> [u32; 2] {
        self.size
    }
}

/// Counts work recorded by one [`TextGlyphRenderer`] pass.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TextGlyphDrawStats {
    /// Glyph quads issued to WGPU after clip validation.
    pub quads: u32,
    /// Triangles issued to WGPU.
    pub triangles: u32,
    /// GPU draw calls; zero for empty text or an empty bounded clip.
    pub draw_calls: u32,
}

/// Alpha-blended RGBA8 glyph-atlas renderer.
///
/// The renderer is deliberately independent from retained UI: callers upload
/// a [`GlyphAtlas`] and submit the matching [`TextDrawList`] with an explicit
/// viewport. It owns reusable pipeline and vertex-buffer state, while
/// [`GpuGlyphAtlas`] owns the GPU texture/bind group.
pub struct TextGlyphRenderer {
    pipeline: wgpu::RenderPipeline,
    atlas_bind_group_layout: wgpu::BindGroupLayout,
    vertex_buffer: Option<wgpu::Buffer>,
    vertex_capacity: u32,
}

impl TextGlyphRenderer {
    /// Creates glyph pipeline state for the renderer's presentation format.
    #[must_use]
    pub fn new(renderer: &Renderer) -> Self {
        let color_format = renderer.color_target_format();
        renderer.with_raw_gpu(|device, _queue, _configuration| Self::create(device, color_format))
    }

    /// Creates glyph pipeline state from an active presentation frame.
    #[must_use]
    pub fn new_for_frame(frame: &RenderFrame<'_>) -> Self {
        Self::create(frame.device(), frame.surface_format())
    }

    /// Uploads a bounded CPU atlas into a new sampled RGBA8 GPU texture.
    ///
    /// # Errors
    ///
    /// Returns [`TextGpuRenderError`] for invalid limits/atlas bytes or when
    /// the selected WGPU device cannot support the atlas dimensions.
    pub fn upload_atlas(
        &self,
        frame: &RenderFrame<'_>,
        atlas: &GlyphAtlas,
        limits: TextGlyphRenderLimits,
    ) -> Result<GpuGlyphAtlas, TextGpuRenderError> {
        validate_gpu_atlas(atlas, limits, frame.device())?;
        let size = atlas.size();
        let texture = frame.device().create_texture(&wgpu::TextureDescriptor {
            label: Some("yuyib UI glyph RGBA8 atlas"),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        write_gpu_atlas(frame.queue(), &texture, atlas)?;
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = frame.device().create_sampler(&wgpu::SamplerDescriptor {
            label: Some("yuyib UI glyph atlas sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let bind_group = frame
            .device()
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("yuyib UI glyph atlas bind group"),
                layout: &self.atlas_bind_group_layout,
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
        Ok(GpuGlyphAtlas {
            size,
            texture,
            bind_group,
        })
    }

    /// Updates every RGBA8 pixel of a same-size resident GPU atlas.
    ///
    /// This intentionally performs a complete bounded upload so a caller can
    /// use the persistent CPU shelf atlas without tracking dirty rows. If the
    /// atlas dimensions change, create a replacement with [`Self::upload_atlas`].
    ///
    /// # Errors
    ///
    /// Returns an error for invalid bounds/data, unsupported dimensions, or a
    /// CPU/GPU atlas size mismatch.
    pub fn update_atlas(
        &self,
        frame: &RenderFrame<'_>,
        gpu_atlas: &mut GpuGlyphAtlas,
        atlas: &GlyphAtlas,
        limits: TextGlyphRenderLimits,
    ) -> Result<(), TextGpuRenderError> {
        validate_gpu_atlas(atlas, limits, frame.device())?;
        if gpu_atlas.size != atlas.size() {
            return Err(TextGpuRenderError::AtlasSizeChanged {
                gpu: gpu_atlas.size,
                cpu: atlas.size(),
            });
        }
        write_gpu_atlas(frame.queue(), &gpu_atlas.texture, atlas)
    }

    /// Draws a prepared list from an already resident atlas over a frame.
    ///
    /// Quads use coordinates local to `options.viewport`. The optional clip is
    /// converted to a bounded WGPU scissor; a zero-sized/outside clip emits no
    /// pass. This method does not update atlas pixels.
    ///
    /// # Errors
    ///
    /// Returns validation errors before GPU recording for mismatched atlas
    /// dimensions, invalid quads, viewport bounds, or configured work limits.
    pub fn draw(
        &mut self,
        frame: &mut RenderFrame<'_>,
        gpu_atlas: &GpuGlyphAtlas,
        draw_list: &TextDrawList,
        options: TextGlyphDrawOptions,
    ) -> Result<TextGlyphDrawStats, TextGpuRenderError> {
        let prepared =
            prepare_glyph_draw(draw_list, gpu_atlas.size, frame.surface_size(), options)?;
        if prepared.vertices.is_empty() || prepared.scissor.is_none() {
            return Ok(TextGlyphDrawStats::default());
        }
        let vertices = u32::try_from(prepared.vertices.len())
            .map_err(|_| TextGpuRenderError::TooManyVertices)?;
        self.ensure_vertex_capacity(frame.device(), vertices);
        let Some(vertex_buffer) = self.vertex_buffer.as_ref() else {
            return Err(TextGpuRenderError::RendererStateUnavailable);
        };
        frame
            .queue()
            .write_buffer(vertex_buffer, 0, bytemuck::cast_slice(&prepared.vertices));
        let Some(scissor) = prepared.scissor else {
            return Ok(TextGlyphDrawStats::default());
        };
        frame.with_surface_pass(wgpu::LoadOp::Load, |pass| {
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &gpu_atlas.bind_group, &[]);
            pass.set_vertex_buffer(0, vertex_buffer.slice(..));
            pass.set_scissor_rect(scissor.x, scissor.y, scissor.width, scissor.height);
            pass.draw(0..vertices, 0..1);
        });
        let quads = vertices / 6;
        Ok(TextGlyphDrawStats {
            quads,
            triangles: quads.saturating_mul(2),
            draw_calls: 1,
        })
    }

    /// Updates the atlas then draws one [`TextAtlasFrame`] in one convenient call.
    ///
    /// Use [`Self::draw`] when the atlas update is scheduled separately or no
    /// new glyph bitmap was added. This helper does not inspect fonts and has
    /// no dependency on a UI tree.
    ///
    /// # Errors
    ///
    /// Returns the same validation/upload/draw errors as [`Self::update_atlas`]
    /// and [`Self::draw`].
    pub fn update_and_draw(
        &mut self,
        frame: &mut RenderFrame<'_>,
        gpu_atlas: &mut GpuGlyphAtlas,
        atlas: &GlyphAtlas,
        text_frame: &TextAtlasFrame,
        options: TextGlyphDrawOptions,
    ) -> Result<TextGlyphDrawStats, TextGpuRenderError> {
        self.update_atlas(frame, gpu_atlas, atlas, options.limits)?;
        self.draw(frame, gpu_atlas, text_frame.draw_list(), options)
    }

    #[allow(clippy::too_many_lines)] // WGPU pipeline descriptors are co-located for auditability.
    fn create(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let atlas_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("yuyib UI glyph atlas layout"),
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
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("yuyib UI glyph WGSL"),
            source: wgpu::ShaderSource::Wgsl(GLYPH_WGSL.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("yuyib UI glyph pipeline layout"),
            bind_group_layouts: &[Some(&atlas_bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("yuyib UI glyph pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(GLYPH_VERTEX_LAYOUT)],
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
        Self {
            pipeline,
            atlas_bind_group_layout,
            vertex_buffer: None,
            vertex_capacity: 0,
        }
    }

    fn ensure_vertex_capacity(&mut self, device: &wgpu::Device, required: u32) {
        if required <= self.vertex_capacity {
            return;
        }
        let capacity = required.checked_next_power_of_two().unwrap_or(required);
        self.vertex_buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("yuyib UI glyph vertices"),
            size: u64::from(capacity) * size_of::<GlyphVertex>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        self.vertex_capacity = capacity;
    }
}

#[derive(Clone, Copy, Debug)]
struct CachedGlyph {
    region: Option<AtlasRegion>,
    placement_left: i32,
    placement_top: i32,
}

/// Isolated glyph rasterizer and cache for a matching explicit text font source.
pub struct TextRasterizer {
    font_system: FontSystem,
    faces: Vec<(ID, Weight)>,
    swash_cache: SwashCache,
    atlas: GlyphAtlas,
    cached: HashMap<CacheKey, CachedGlyph>,
}

impl TextRasterizer {
    /// Creates a rasterizer from the same explicit source used by [`yuyib_ui_text::TextEngine`].
    ///
    /// This uses an isolated font database and never scans installed fonts.
    /// For stable `font_index` matching, construct this rasterizer from the
    /// same immutable bytes (or unchanged explicit path) as the text engine.
    ///
    /// # Errors
    ///
    /// Returns font-source, byte-budget, parse, or atlas-configuration errors.
    pub fn from_source(
        source: FontSource,
        config: GlyphAtlasConfig,
    ) -> Result<Self, TextRenderError> {
        config.validate()?;
        let bytes = read_font_source(source, config.max_font_bytes)?;
        let mut database = Database::new();
        database.load_font_data(bytes);
        let faces: Vec<_> = database
            .faces()
            .map(|face| (face.id, face.weight))
            .collect();
        if faces.is_empty() {
            return Err(TextRenderError::InvalidFontData);
        }
        let font_system = FontSystem::new_with_locale_and_db(String::from("en-US"), database);
        Ok(Self {
            font_system,
            faces,
            swash_cache: SwashCache::new(),
            atlas: GlyphAtlas::new(config)?,
            cached: HashMap::new(),
        })
    }

    /// Returns the persistent CPU atlas for [`TextGlyphRenderer`] upload/update.
    #[must_use]
    pub const fn atlas(&self) -> &GlyphAtlas {
        &self.atlas
    }

    /// Rasterizes a shaped layout into cached atlas entries and ordered quads.
    ///
    /// Mask glyphs become white RGB plus raster alpha; colour glyphs retain
    /// their `RGBA` pixels. Subpixel mask glyphs are rejected rather than
    /// silently changing LCD filtering semantics.
    ///
    /// # Errors
    ///
    /// Returns a source-index, rasterization, bitmap, cache, or atlas error.
    pub fn rasterize(
        &mut self,
        shaped: &ShapedText,
        color: TextColor,
    ) -> Result<TextAtlasFrame, TextRenderError> {
        let mut quads = Vec::with_capacity(shaped.metrics().glyph_count);
        let mut glyphs_added = 0_usize;
        for line in shaped.lines() {
            for glyph in &line.glyphs {
                let (cached, origin_x, origin_y) = self.cached_glyph(glyph, &mut glyphs_added)?;
                if let Some(region) = cached.region {
                    quads.push(TextQuad {
                        x: to_f32_i32(origin_x.saturating_add(cached.placement_left)),
                        y: to_f32_i32(origin_y.saturating_sub(cached.placement_top)),
                        width: to_f32(region.width),
                        height: to_f32(region.height),
                        uv: region.uv(self.atlas.config.width, self.atlas.config.height),
                        color,
                    });
                }
            }
        }
        Ok(TextAtlasFrame {
            draw_list: TextDrawList {
                atlas_size: self.atlas.size(),
                quads,
            },
            glyphs_added,
        })
    }

    fn cached_glyph(
        &mut self,
        glyph: &PositionedGlyph,
        glyphs_added: &mut usize,
    ) -> Result<(CachedGlyph, i32, i32), TextRenderError> {
        let index = usize::try_from(glyph.font_index)
            .map_err(|_| TextRenderError::UnknownFontIndex(glyph.font_index))?;
        let (font_id, weight) = *self
            .faces
            .get(index)
            .ok_or(TextRenderError::UnknownFontIndex(glyph.font_index))?;
        let position = (
            glyph.x + glyph.font_size * glyph.x_offset,
            glyph.y - glyph.font_size * glyph.y_offset,
        );
        let (key, origin_x, origin_y) = CacheKey::new(
            font_id,
            glyph.glyph_id,
            glyph.font_size,
            position,
            weight,
            CacheKeyFlags::empty(),
        );
        if let Some(cached) = self.cached.get(&key) {
            return Ok((*cached, origin_x, origin_y));
        }
        let image = self
            .swash_cache
            .get_image_uncached(&mut self.font_system, key)
            .ok_or(TextRenderError::RasterizationUnavailable)?;
        let bitmap = bitmap_from_swash(&image)?;
        let region = bitmap
            .as_ref()
            .map(|bitmap| self.atlas.insert_bitmap(bitmap))
            .transpose()?;
        let cached = CachedGlyph {
            region,
            placement_left: image.placement.left,
            placement_top: image.placement.top,
        };
        self.cached.insert(key, cached);
        if bitmap.is_some() {
            *glyphs_added = glyphs_added.saturating_add(1);
        }
        Ok((cached, origin_x, origin_y))
    }
}

/// Glyph-atlas/rasterization error.
#[derive(Debug)]
pub enum TextRenderError {
    /// Atlas has a zero dimension or zero required budget.
    InvalidAtlasConfig,
    /// RGBA atlas byte length overflowed `usize`.
    AtlasSizeOverflow,
    /// Atlas RGBA byte allocation exceeds its configured limit.
    AtlasTooLarge {
        /// Required bytes.
        actual: usize,
        /// Configured maximum.
        limit: usize,
    },
    /// Exact explicit font file could not be read.
    FontRead {
        /// Requested path.
        path: PathBuf,
        /// Operating-system read failure.
        source: io::Error,
    },
    /// Explicit font source exceeded its byte budget.
    FontTooLarge {
        /// Supplied byte length.
        actual: usize,
        /// Configured byte limit.
        limit: usize,
    },
    /// Font parsing produced no usable face.
    InvalidFontData,
    /// Bitmap dimensions/data are invalid.
    InvalidBitmap,
    /// Cache has reached its configured glyph region limit.
    GlyphCacheFull {
        /// Configured maximum regions.
        limit: usize,
    },
    /// One bitmap cannot fit in an empty atlas row.
    GlyphTooLarge {
        /// Bitmap width.
        width: u32,
        /// Bitmap height.
        height: u32,
        /// Atlas width.
        atlas_width: u32,
        /// Atlas height.
        atlas_height: u32,
    },
    /// No remaining shelf region can fit the bitmap.
    AtlasFull,
    /// Layout referred to a font face not represented by this rasterizer source.
    UnknownFontIndex(u32),
    /// `SwashCache` could not produce a glyph image.
    RasterizationUnavailable,
    /// LCD subpixel glyph masks are intentionally unsupported in this RGBA atlas.
    UnsupportedSubpixelMask,
    /// Per-quad colour contains non-finite or out-of-range values.
    InvalidColor,
}

impl fmt::Display for TextRenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidAtlasConfig => formatter.write_str("invalid glyph atlas configuration"),
            Self::AtlasSizeOverflow => formatter.write_str("glyph atlas byte length overflow"),
            Self::AtlasTooLarge { actual, limit } => {
                write!(
                    formatter,
                    "glyph atlas requires {actual} bytes, limit is {limit}"
                )
            }
            Self::FontRead { path, source } => {
                write!(
                    formatter,
                    "failed to read raster font {}: {source}",
                    path.display()
                )
            }
            Self::FontTooLarge { actual, limit } => {
                write!(
                    formatter,
                    "raster font byte length {actual} exceeds limit {limit}"
                )
            }
            Self::InvalidFontData => {
                formatter.write_str("raster font bytes contain no usable face")
            }
            Self::InvalidBitmap => formatter.write_str("glyph bitmap is not tightly packed RGBA8"),
            Self::GlyphCacheFull { limit } => {
                write!(formatter, "glyph cache limit {limit} reached")
            }
            Self::GlyphTooLarge {
                width,
                height,
                atlas_width,
                atlas_height,
            } => write!(
                formatter,
                "glyph {width}x{height} cannot fit atlas {atlas_width}x{atlas_height}"
            ),
            Self::AtlasFull => formatter.write_str("glyph atlas has no remaining space"),
            Self::UnknownFontIndex(index) => {
                write!(
                    formatter,
                    "shaped glyph references unknown font index {index}"
                )
            }
            Self::RasterizationUnavailable => {
                formatter.write_str("glyph rasterization is unavailable")
            }
            Self::UnsupportedSubpixelMask => {
                formatter.write_str("LCD subpixel glyph masks are not supported by this atlas")
            }
            Self::InvalidColor => formatter.write_str("text colour channels must be finite 0–1"),
        }
    }
}

impl Error for TextRenderError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::FontRead { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// GPU atlas upload or glyph-pass validation failure.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum TextGpuRenderError {
    /// A required GPU work limit is zero.
    InvalidLimits,
    /// Atlas pixels exceed the configured upload byte limit.
    AtlasTooLarge {
        /// Actual RGBA8 byte count.
        actual: usize,
        /// Configured byte limit.
        limit: usize,
    },
    /// CPU atlas dimensions exceed the selected WGPU device capability.
    AtlasDimensionUnsupported {
        /// CPU atlas width.
        width: u32,
        /// CPU atlas height.
        height: u32,
        /// WGPU maximum 2D texture dimension.
        maximum: u32,
    },
    /// The resident GPU atlas must be recreated because dimensions changed.
    AtlasSizeChanged {
        /// Current resident dimensions.
        gpu: [u32; 2],
        /// Current CPU atlas dimensions.
        cpu: [u32; 2],
    },
    /// A draw list references an atlas with other dimensions.
    DrawListAtlasMismatch {
        /// Draw-list dimensions.
        draw_list: [u32; 2],
        /// Resident GPU atlas dimensions.
        gpu: [u32; 2],
    },
    /// The explicit viewport has zero extent or exceeds the surface bounds.
    InvalidViewport {
        /// Requested viewport.
        viewport: TextViewport,
        /// Presentation surface size.
        surface: [u32; 2],
    },
    /// A quad exceeded the configured bounded draw-list size.
    TooManyQuads {
        /// Observed quad count.
        actual: usize,
        /// Configured maximum.
        limit: usize,
    },
    /// A quad contains non-finite, non-positive, or invalid UV/colour values.
    InvalidQuad {
        /// Quad index in retained painter order.
        index: usize,
    },
    /// A vertex count cannot fit WGPU's `u32` draw range.
    TooManyVertices,
    /// Internal reusable GPU state was not created for a non-empty pass.
    RendererStateUnavailable,
    /// RGBA atlas row pitch cannot fit WGPU's copy layout.
    RowPitchOverflow {
        /// Atlas width.
        width: u32,
    },
}

impl fmt::Display for TextGpuRenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLimits => formatter.write_str("invalid glyph GPU render limits"),
            Self::AtlasTooLarge { actual, limit } => {
                write!(
                    formatter,
                    "glyph GPU atlas requires {actual} bytes, limit is {limit}"
                )
            }
            Self::AtlasDimensionUnsupported {
                width,
                height,
                maximum,
            } => write!(
                formatter,
                "glyph atlas {width}x{height} exceeds GPU dimension limit {maximum}"
            ),
            Self::AtlasSizeChanged { gpu, cpu } => write!(
                formatter,
                "resident glyph atlas is {}x{} but CPU atlas is {}x{}; upload a replacement",
                gpu[0], gpu[1], cpu[0], cpu[1]
            ),
            Self::DrawListAtlasMismatch { draw_list, gpu } => write!(
                formatter,
                "glyph draw list atlas is {}x{} but GPU atlas is {}x{}",
                draw_list[0], draw_list[1], gpu[0], gpu[1]
            ),
            Self::InvalidViewport { viewport, surface } => write!(
                formatter,
                "glyph viewport {}:{} {}x{} is outside surface {}x{}",
                viewport.x, viewport.y, viewport.width, viewport.height, surface[0], surface[1]
            ),
            Self::TooManyQuads { actual, limit } => {
                write!(formatter, "glyph quad count {actual} exceeds limit {limit}")
            }
            Self::InvalidQuad { index } => write!(formatter, "glyph quad {index} is invalid"),
            Self::TooManyVertices => formatter.write_str("glyph vertex count exceeds WGPU range"),
            Self::RendererStateUnavailable => {
                formatter.write_str("glyph renderer did not allocate its reusable vertex buffer")
            }
            Self::RowPitchOverflow { width } => {
                write!(
                    formatter,
                    "glyph atlas row pitch overflows for width {width}"
                )
            }
        }
    }
}

impl Error for TextGpuRenderError {}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
struct GlyphVertex {
    position: [f32; 2],
    uv: [f32; 2],
    color: [f32; 4],
}

const GLYPH_VERTEX_LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
    array_stride: size_of::<GlyphVertex>() as u64,
    step_mode: wgpu::VertexStepMode::Vertex,
    attributes: &[
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x2,
            offset: 0,
            shader_location: 0,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x2,
            offset: size_of::<[f32; 2]>() as u64,
            shader_location: 1,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x4,
            offset: (size_of::<[f32; 2]>() * 2) as u64,
            shader_location: 2,
        },
    ],
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct GlyphScissor {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

#[derive(Debug)]
struct PreparedGlyphDraw {
    vertices: Vec<GlyphVertex>,
    scissor: Option<GlyphScissor>,
}

fn validate_gpu_atlas(
    atlas: &GlyphAtlas,
    limits: TextGlyphRenderLimits,
    device: &wgpu::Device,
) -> Result<(), TextGpuRenderError> {
    limits.validate()?;
    let size = atlas.size();
    let actual =
        rgba_bytes_len(size[0], size[1]).map_err(|_| TextGpuRenderError::AtlasTooLarge {
            actual: usize::MAX,
            limit: limits.max_atlas_bytes,
        })?;
    if atlas.pixels().len() != actual || actual > limits.max_atlas_bytes {
        return Err(TextGpuRenderError::AtlasTooLarge {
            actual: atlas.pixels().len().max(actual),
            limit: limits.max_atlas_bytes,
        });
    }
    let maximum = device.limits().max_texture_dimension_2d;
    if size[0] > maximum || size[1] > maximum {
        return Err(TextGpuRenderError::AtlasDimensionUnsupported {
            width: size[0],
            height: size[1],
            maximum,
        });
    }
    Ok(())
}

fn write_gpu_atlas(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    atlas: &GlyphAtlas,
) -> Result<(), TextGpuRenderError> {
    let size = atlas.size();
    let bytes_per_row = size[0]
        .checked_mul(4)
        .ok_or(TextGpuRenderError::RowPitchOverflow { width: size[0] })?;
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        atlas.pixels(),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(bytes_per_row),
            rows_per_image: Some(size[1]),
        },
        wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
    );
    Ok(())
}

fn prepare_glyph_draw(
    draw_list: &TextDrawList,
    gpu_atlas_size: [u32; 2],
    surface: [u32; 2],
    options: TextGlyphDrawOptions,
) -> Result<PreparedGlyphDraw, TextGpuRenderError> {
    options.limits.validate()?;
    if draw_list.atlas_size != gpu_atlas_size {
        return Err(TextGpuRenderError::DrawListAtlasMismatch {
            draw_list: draw_list.atlas_size,
            gpu: gpu_atlas_size,
        });
    }
    validate_viewport(options.viewport, surface)?;
    if draw_list.quads.len() > options.limits.max_quads {
        return Err(TextGpuRenderError::TooManyQuads {
            actual: draw_list.quads.len(),
            limit: options.limits.max_quads,
        });
    }
    for (index, quad) in draw_list.quads.iter().copied().enumerate() {
        validate_quad(quad, index)?;
    }
    let scissor = bounded_glyph_scissor(options.viewport, options.clip, surface);
    if draw_list.quads.is_empty() || scissor.is_none() {
        return Ok(PreparedGlyphDraw {
            vertices: Vec::new(),
            scissor,
        });
    }
    let capacity = draw_list
        .quads
        .len()
        .checked_mul(6)
        .ok_or(TextGpuRenderError::TooManyVertices)?;
    u32::try_from(capacity).map_err(|_| TextGpuRenderError::TooManyVertices)?;
    let mut vertices = Vec::with_capacity(capacity);
    for quad in draw_list.quads.iter().copied() {
        append_glyph_vertices(&mut vertices, quad, options.viewport, surface);
    }
    Ok(PreparedGlyphDraw { vertices, scissor })
}

fn validate_viewport(viewport: TextViewport, surface: [u32; 2]) -> Result<(), TextGpuRenderError> {
    if viewport.width == 0
        || viewport.height == 0
        || viewport
            .x
            .checked_add(viewport.width)
            .is_none_or(|right| right > surface[0])
        || viewport
            .y
            .checked_add(viewport.height)
            .is_none_or(|bottom| bottom > surface[1])
    {
        return Err(TextGpuRenderError::InvalidViewport { viewport, surface });
    }
    Ok(())
}

fn bounded_glyph_scissor(
    viewport: TextViewport,
    clip: Option<TextClipRect>,
    surface: [u32; 2],
) -> Option<GlyphScissor> {
    if surface[0] == 0 || surface[1] == 0 {
        return None;
    }
    let left = i64::from(viewport.x);
    let top = i64::from(viewport.y);
    let right = left + i64::from(viewport.width);
    let bottom = top + i64::from(viewport.height);
    let (clip_left, clip_top, clip_right, clip_bottom) = match clip {
        Some(clip) => {
            let clip_left = left + i64::from(clip.x);
            let clip_top = top + i64::from(clip.y);
            (
                clip_left,
                clip_top,
                clip_left + i64::from(clip.width),
                clip_top + i64::from(clip.height),
            )
        }
        None => (left, top, right, bottom),
    };
    let bounded_left = left.max(clip_left).max(0);
    let bounded_top = top.max(clip_top).max(0);
    let bounded_right = right.min(clip_right).min(i64::from(surface[0]));
    let bounded_bottom = bottom.min(clip_bottom).min(i64::from(surface[1]));
    if bounded_right <= bounded_left || bounded_bottom <= bounded_top {
        return None;
    }
    Some(GlyphScissor {
        x: u32::try_from(bounded_left).ok()?,
        y: u32::try_from(bounded_top).ok()?,
        width: u32::try_from(bounded_right - bounded_left).ok()?,
        height: u32::try_from(bounded_bottom - bounded_top).ok()?,
    })
}

fn validate_quad(quad: TextQuad, index: usize) -> Result<(), TextGpuRenderError> {
    let valid_geometry = [quad.x, quad.y, quad.width, quad.height]
        .iter()
        .all(|value| value.is_finite());
    let valid_uv = quad
        .uv
        .iter()
        .all(|value| value.is_finite() && (0.0..=1.0).contains(value))
        && quad.uv[0] <= quad.uv[2]
        && quad.uv[1] <= quad.uv[3];
    let valid_color = [
        quad.color.red,
        quad.color.green,
        quad.color.blue,
        quad.color.alpha,
    ]
    .iter()
    .all(|value| value.is_finite() && (0.0..=1.0).contains(value));
    if !valid_geometry || quad.width <= 0.0 || quad.height <= 0.0 || !valid_uv || !valid_color {
        return Err(TextGpuRenderError::InvalidQuad { index });
    }
    Ok(())
}

#[allow(clippy::cast_precision_loss)] // Presentation dimensions are bounded by WGPU.
fn append_glyph_vertices(
    vertices: &mut Vec<GlyphVertex>,
    quad: TextQuad,
    viewport: TextViewport,
    surface: [u32; 2],
) {
    let left = ((viewport.x as f32 + quad.x) / surface[0] as f32).mul_add(2.0, -1.0);
    let top = -((viewport.y as f32 + quad.y) / surface[1] as f32).mul_add(2.0, -1.0);
    let right = ((viewport.x as f32 + quad.x + quad.width) / surface[0] as f32).mul_add(2.0, -1.0);
    let bottom =
        -((viewport.y as f32 + quad.y + quad.height) / surface[1] as f32).mul_add(2.0, -1.0);
    let [u0, v0, u1, v1] = quad.uv;
    let color = [
        quad.color.red,
        quad.color.green,
        quad.color.blue,
        quad.color.alpha,
    ];
    vertices.extend([
        GlyphVertex {
            position: [left, top],
            uv: [u0, v0],
            color,
        },
        GlyphVertex {
            position: [right, top],
            uv: [u1, v0],
            color,
        },
        GlyphVertex {
            position: [right, bottom],
            uv: [u1, v1],
            color,
        },
        GlyphVertex {
            position: [left, top],
            uv: [u0, v0],
            color,
        },
        GlyphVertex {
            position: [right, bottom],
            uv: [u1, v1],
            color,
        },
        GlyphVertex {
            position: [left, bottom],
            uv: [u0, v1],
            color,
        },
    ]);
}

const GLYPH_WGSL: &str = r"
struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@group(0) @binding(0) var glyph_atlas: texture_2d<f32>;
@group(0) @binding(1) var glyph_sampler: sampler;

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    output.clip_position = vec4<f32>(input.position, 0.0, 1.0);
    output.uv = input.uv;
    output.color = input.color;
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(glyph_atlas, glyph_sampler, input.uv) * input.color;
}
";

fn rgba_bytes_len(width: u32, height: u32) -> Result<usize, TextRenderError> {
    usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(TextRenderError::AtlasSizeOverflow)
}

#[allow(clippy::cast_precision_loss)]
fn to_f32(value: u32) -> f32 {
    value as f32
}

#[allow(clippy::cast_precision_loss)]
fn to_f32_i32(value: i32) -> f32 {
    value as f32
}

fn read_font_source(source: FontSource, max_bytes: usize) -> Result<Vec<u8>, TextRenderError> {
    match source {
        FontSource::Bytes(bytes) => {
            if bytes.len() > max_bytes {
                return Err(TextRenderError::FontTooLarge {
                    actual: bytes.len(),
                    limit: max_bytes,
                });
            }
            Ok(bytes)
        }
        FontSource::File(path) => {
            let file = File::open(&path).map_err(|source| TextRenderError::FontRead {
                path: path.clone(),
                source,
            })?;
            let read_limit = u64::try_from(max_bytes.saturating_add(1)).unwrap_or(u64::MAX);
            let mut bytes = Vec::new();
            file.take(read_limit)
                .read_to_end(&mut bytes)
                .map_err(|source| TextRenderError::FontRead { path, source })?;
            if bytes.len() > max_bytes {
                return Err(TextRenderError::FontTooLarge {
                    actual: bytes.len(),
                    limit: max_bytes,
                });
            }
            Ok(bytes)
        }
    }
}

fn bitmap_from_swash(
    image: &cosmic_text::SwashImage,
) -> Result<Option<GlyphBitmap>, TextRenderError> {
    let width = image.placement.width;
    let height = image.placement.height;
    if width == 0 || height == 0 {
        return Ok(None);
    }
    let pixel_count = usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .ok_or(TextRenderError::AtlasSizeOverflow)?;
    let mut pixels = Vec::with_capacity(pixel_count.saturating_mul(4));
    match image.content {
        SwashContent::Mask => {
            if image.data.len() != pixel_count {
                return Err(TextRenderError::InvalidBitmap);
            }
            for alpha in &image.data {
                pixels.extend_from_slice(&[u8::MAX, u8::MAX, u8::MAX, *alpha]);
            }
        }
        SwashContent::Color => {
            let expected = pixel_count
                .checked_mul(4)
                .ok_or(TextRenderError::AtlasSizeOverflow)?;
            if image.data.len() != expected {
                return Err(TextRenderError::InvalidBitmap);
            }
            pixels.extend_from_slice(&image.data);
        }
        SwashContent::SubpixelMask => return Err(TextRenderError::UnsupportedSubpixelMask),
    }
    GlyphBitmap::new(width, height, pixels).map(Some)
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // Exact normalized test inputs make geometry expectations auditable.
mod tests {
    use super::*;

    fn bitmap(red: u8, width: u32, height: u32) -> GlyphBitmap {
        let pixels = vec![red, 0, 0, u8::MAX]
            .into_iter()
            .cycle()
            .take(usize::try_from(width.saturating_mul(height).saturating_mul(4)).unwrap_or(0))
            .collect();
        GlyphBitmap::new(width, height, pixels).expect("test bitmap")
    }

    fn quad() -> TextQuad {
        TextQuad {
            x: 2.0,
            y: 4.0,
            width: 10.0,
            height: 8.0,
            uv: [0.0, 0.25, 0.5, 0.75],
            color: TextColor::white(),
        }
    }

    fn draw_list(quads: Vec<TextQuad>) -> TextDrawList {
        TextDrawList {
            atlas_size: [8, 8],
            quads,
        }
    }

    #[test]
    fn translated_draw_list_moves_only_glyph_origins() {
        let original = draw_list(vec![quad()]);
        let translated = original.translated(12.0, -3.0);

        assert_eq!(translated.atlas_size(), original.atlas_size());
        assert_eq!(translated.quads()[0].x, 14.0);
        assert_eq!(translated.quads()[0].y, 1.0);
        assert_eq!(translated.quads()[0].width, original.quads()[0].width);
        assert_eq!(translated.quads()[0].height, original.quads()[0].height);
        assert_eq!(translated.quads()[0].uv, original.quads()[0].uv);
        assert_eq!(translated.quads()[0].color, original.quads()[0].color);
        assert_eq!(original.quads()[0], quad());
    }

    #[test]
    fn append_batches_same_atlas_in_submission_order() {
        let first = draw_list(vec![quad()]);
        let second = first.translated(20.0, 0.0);
        let mut combined = TextDrawList::default();

        combined.append(&first).expect("first atlas is adopted");
        combined
            .append(&second)
            .expect("matching atlas can be batched");

        assert_eq!(combined.atlas_size(), [8, 8]);
        assert_eq!(combined.quads(), &[quad(), second.quads()[0]]);
    }

    #[test]
    fn append_rejects_different_atlases_without_mutating_target() {
        let mut first = draw_list(vec![quad()]);
        let other = TextDrawList {
            atlas_size: [16, 16],
            quads: vec![quad()],
        };

        assert_eq!(
            first.append(&other),
            Err(TextDrawListAppendError::AtlasMismatch {
                target: [8, 8],
                appended: [16, 16],
            })
        );
        assert_eq!(first.quads(), &[quad()]);
    }

    #[test]
    fn atlas_places_bitmaps_deterministically_and_preserves_pixels() {
        let mut atlas = GlyphAtlas::new(GlyphAtlasConfig {
            width: 8,
            height: 8,
            padding: 1,
            max_glyphs: 4,
            max_atlas_bytes: 256,
            max_font_bytes: 8,
        })
        .expect("atlas");
        let first = atlas.insert_bitmap(&bitmap(10, 2, 2)).expect("first");
        let second = atlas.insert_bitmap(&bitmap(20, 2, 2)).expect("second");
        assert_eq!(
            first,
            AtlasRegion {
                x: 1,
                y: 1,
                width: 2,
                height: 2
            }
        );
        assert_eq!(
            second,
            AtlasRegion {
                x: 5,
                y: 1,
                width: 2,
                height: 2
            }
        );
        let first_offset = 4 * (8 + 1);
        let second_offset = 4 * (8 + 5);
        assert_eq!(atlas.pixels()[first_offset], 10);
        assert_eq!(atlas.pixels()[second_offset], 20);
    }

    #[test]
    fn atlas_rejects_bad_bounds_without_mutating_allocator() {
        let mut atlas = GlyphAtlas::new(GlyphAtlasConfig {
            width: 4,
            height: 4,
            padding: 1,
            max_glyphs: 1,
            max_atlas_bytes: 64,
            max_font_bytes: 8,
        })
        .expect("atlas");
        assert!(matches!(
            atlas.insert_bitmap(&bitmap(1, 3, 3)),
            Err(TextRenderError::GlyphTooLarge { .. })
        ));
        assert_eq!(atlas.glyph_count(), 0);
        let first = atlas
            .insert_bitmap(&bitmap(2, 2, 2))
            .expect("first valid bitmap");
        assert_eq!(first.x, 1);
        assert_eq!(atlas.glyph_count(), 1);
    }

    #[test]
    fn validation_and_font_budgets_fail_before_font_rasterization() {
        assert!(matches!(
            GlyphAtlasConfig {
                width: 0,
                ..GlyphAtlasConfig::default()
            }
            .validate(),
            Err(TextRenderError::InvalidAtlasConfig)
        ));
        assert!(matches!(
            TextRasterizer::from_source(
                FontSource::bytes(vec![0, 1, 2]),
                GlyphAtlasConfig {
                    max_font_bytes: 2,
                    ..GlyphAtlasConfig::default()
                }
            ),
            Err(TextRenderError::FontTooLarge {
                actual: 3,
                limit: 2
            })
        ));
        assert!(matches!(
            TextColor::new(1.0, 0.0, 0.0, f32::NAN),
            Err(TextRenderError::InvalidColor)
        ));
    }

    #[test]
    fn prepared_glyph_geometry_uses_explicit_viewport_and_bounded_scissor() {
        let options = TextGlyphDrawOptions::new(TextViewport::new(10, 20, 80, 60))
            .with_clip(Some(TextClipRect::new(-5, 3, 20, 10)));
        let prepared = prepare_glyph_draw(&draw_list(vec![quad()]), [8, 8], [100, 100], options)
            .expect("prepared glyphs");

        assert_eq!(prepared.vertices.len(), 6);
        assert_eq!(prepared.vertices[0].position, [-0.76, 0.52]);
        assert_eq!(prepared.vertices[0].uv, [0.0, 0.25]);
        assert_eq!(
            prepared.scissor,
            Some(GlyphScissor {
                x: 10,
                y: 23,
                width: 15,
                height: 10,
            })
        );
    }

    #[test]
    fn empty_or_outside_clip_skips_gpu_geometry_without_error() {
        let options = TextGlyphDrawOptions::new(TextViewport::new(10, 10, 20, 20))
            .with_clip(Some(TextClipRect::new(30, 0, 1, 1)));
        let prepared = prepare_glyph_draw(&draw_list(vec![quad()]), [8, 8], [100, 100], options)
            .expect("empty clip is not an error");

        assert!(prepared.vertices.is_empty());
        assert_eq!(prepared.scissor, None);
    }

    #[test]
    fn glyph_draw_validation_rejects_bad_viewport_quad_and_limits() {
        let list = draw_list(vec![quad()]);
        assert!(matches!(
            prepare_glyph_draw(
                &list,
                [8, 8],
                [100, 100],
                TextGlyphDrawOptions::new(TextViewport::new(90, 0, 20, 10)),
            ),
            Err(TextGpuRenderError::InvalidViewport { .. })
        ));
        let invalid = TextQuad {
            width: f32::NAN,
            ..quad()
        };
        assert!(matches!(
            prepare_glyph_draw(
                &draw_list(vec![invalid]),
                [8, 8],
                [100, 100],
                TextGlyphDrawOptions::new(TextViewport::new(0, 0, 100, 100)),
            ),
            Err(TextGpuRenderError::InvalidQuad { index: 0 })
        ));
        let limits = TextGlyphRenderLimits {
            max_quads: 1,
            ..TextGlyphRenderLimits::default()
        };
        assert!(matches!(
            prepare_glyph_draw(
                &draw_list(vec![quad(), quad()]),
                [8, 8],
                [100, 100],
                TextGlyphDrawOptions::new(TextViewport::new(0, 0, 100, 100)).with_limits(limits),
            ),
            Err(TextGpuRenderError::TooManyQuads {
                actual: 2,
                limit: 1
            })
        ));
    }
}
