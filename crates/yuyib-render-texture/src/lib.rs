//! Reusable sampled GPU textures for Yuyib render pipelines.
//!
//! This crate is the boundary between a validated CPU [`DecodedImage`] and a
//! WGPU texture that a material or other renderer can bind. It deliberately
//! does not define a material layout: different 2D, 3D, UI and shader pipelines
//! can use the same [`GpuTexture`] view and sampler in their own bind groups.
//!
//! [`TextureCache`] maps typed Yuyib texture asset handles to resident GPU
//! objects. Calling `upsert` with an existing handle replaces the whole GPU
//! resource, which is the safe update path when dimensions, colour space or
//! sampler settings change.
//!
//! # Example
//!
//! ```no_run
//! use yuyib_render_texture::{TextureCache, TextureSampler};
//!
//! # let renderer: yuyib_render::Renderer = todo!();
//! # let handle: yuyib_2d::TextureHandle = todo!();
//! # let image: yuyib_image::DecodedImage = todo!();
//! let mut textures = TextureCache::new();
//! textures.upsert(&renderer, handle, &image, TextureSampler::default())?;
//! let gpu = textures.get(handle).expect("the handle was just uploaded");
//! // Use gpu.view() and gpu.sampler() in this renderer's material bind group.
//! # Ok::<(), yuyib_render_texture::TextureUploadError>(())
//! ```

#![forbid(unsafe_code)]

use std::{collections::HashMap, error::Error, fmt};

use yuyib_2d::{Texture, TextureColorSpace, TextureHandle, TextureSize};
use yuyib_image::DecodedImage;
use yuyib_render::{RenderFrame, Renderer, wgpu};

/// Whether the uploader keeps only the source level or generates a complete
/// mip chain down to `1x1`.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum TextureMipmapPolicy {
    /// Keep only mip level zero. This is useful for pixel art and data that is
    /// never minified.
    Disabled,
    /// Generate and upload every mip level.
    #[default]
    Generate,
}

/// Ready-made sampling policies for common application choices.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum TextureSamplingPreset {
    /// Nearest filtering, one mip and no anisotropy.
    PixelArt,
    /// Trilinear mipmaps with moderate anisotropy.
    #[default]
    Balanced,
    /// Trilinear mipmaps with the portable maximum anisotropy request.
    HighQuality,
}

impl TextureSamplingPreset {
    /// Expands this high-level choice into the low-level sampler contract.
    #[must_use]
    pub const fn sampler(self) -> TextureSampler {
        match self {
            Self::PixelArt => TextureSampler {
                mag_filter: wgpu::FilterMode::Nearest,
                min_filter: wgpu::FilterMode::Nearest,
                mipmap_filter: wgpu::MipmapFilterMode::Nearest,
                mipmaps: TextureMipmapPolicy::Disabled,
                anisotropy_clamp: 1,
                ..TextureSampler::clamp_to_edge()
            },
            Self::Balanced => TextureSampler {
                anisotropy_clamp: 4,
                ..TextureSampler::trilinear()
            },
            Self::HighQuality => TextureSampler {
                anisotropy_clamp: MAX_ANISOTROPY_CLAMP,
                ..TextureSampler::trilinear()
            },
        }
    }
}

/// Portable maximum accepted by WGPU's sampler validation.
pub const MAX_ANISOTROPY_CLAMP: u16 = 16;

/// Why a requested anisotropy value was reduced before sampler creation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AnisotropyFallback {
    /// Zero is not a valid WGPU anisotropy clamp and was raised to one.
    RaisedToMinimum,
    /// The selected device did not enable anisotropic filtering.
    Unsupported,
    /// WGPU requires every filter to be linear for anisotropic sampling.
    NonLinearFilters,
    /// The request exceeded [`MAX_ANISOTROPY_CLAMP`].
    ClampedToPortableMaximum,
}

/// Effective texture residency and sampling values selected for one upload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextureSamplingDiagnostics {
    mip_level_count: u32,
    requested_anisotropy: u16,
    effective_anisotropy: u16,
    anisotropy_fallback: Option<AnisotropyFallback>,
}

impl TextureSamplingDiagnostics {
    /// Number of resident mip levels, including the source level.
    #[must_use]
    pub const fn mip_level_count(self) -> u32 {
        self.mip_level_count
    }

    /// Anisotropy requested by the low-level policy.
    #[must_use]
    pub const fn requested_anisotropy(self) -> u16 {
        self.requested_anisotropy
    }

    /// Anisotropy actually passed to WGPU.
    #[must_use]
    pub const fn effective_anisotropy(self) -> u16 {
        self.effective_anisotropy
    }

    /// Optional reason why the anisotropy request was reduced.
    #[must_use]
    pub const fn anisotropy_fallback(self) -> Option<AnisotropyFallback> {
        self.anisotropy_fallback
    }
}

/// Sampling state applied when a [`GpuTexture`] is created.
///
/// Changing this setting requires a new sampler. [`TextureCache::upsert`]
/// creates a replacement texture and sampler atomically from a caller's point
/// of view.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TextureSampler {
    /// Horizontal out-of-range coordinate behaviour.
    pub address_mode_u: wgpu::AddressMode,
    /// Vertical out-of-range coordinate behaviour.
    pub address_mode_v: wgpu::AddressMode,
    /// Depth out-of-range coordinate behaviour. It is retained for a uniform
    /// WGPU sampler descriptor even though this crate uploads 2D textures.
    pub address_mode_w: wgpu::AddressMode,
    /// Magnification filter.
    pub mag_filter: wgpu::FilterMode,
    /// Minification filter.
    pub min_filter: wgpu::FilterMode,
    /// Filter used when sampling between resident mip levels.
    pub mipmap_filter: wgpu::MipmapFilterMode,
    /// CPU-side mip residency policy.
    pub mipmaps: TextureMipmapPolicy,
    /// Requested anisotropy. Values outside `1..=16` are safely normalized;
    /// unsupported devices and non-linear filters fall back to `1`.
    pub anisotropy_clamp: u16,
}

impl Default for TextureSampler {
    fn default() -> Self {
        TextureSamplingPreset::Balanced.sampler()
    }
}

impl TextureSampler {
    const fn clamp_to_edge() -> Self {
        Self {
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            mipmaps: TextureMipmapPolicy::Generate,
            anisotropy_clamp: 1,
        }
    }

    const fn trilinear() -> Self {
        Self::clamp_to_edge()
    }

    /// Replaces only mip residency while retaining all low-level filters.
    #[must_use]
    pub const fn with_mipmaps(mut self, mipmaps: TextureMipmapPolicy) -> Self {
        self.mipmaps = mipmaps;
        self
    }

    /// Replaces the requested anisotropy clamp. Upload diagnostics report any
    /// normalization or unsupported-device fallback.
    #[must_use]
    pub const fn with_anisotropy_clamp(mut self, anisotropy_clamp: u16) -> Self {
        self.anisotropy_clamp = anisotropy_clamp;
        self
    }
}

/// A sampled 2D WGPU texture, its view and its sampler.
///
/// The contained WGPU texture remains private so it cannot be destroyed while
/// its view is used. `GpuTexture` is tied to one [`Renderer`] device; recreate
/// it after rebuilding the renderer/device.
pub struct GpuTexture {
    metadata: Texture,
    format: wgpu::TextureFormat,
    sampler_settings: TextureSampler,
    sampling_diagnostics: TextureSamplingDiagnostics,
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    sampler: wgpu::Sampler,
}

/// CPU-prepared RGBA8 mip chain ready for bounded GPU publication.
///
/// Construct this on an asset worker. Uploading it does no colour conversion,
/// alpha filtering or mip generation on the render thread.
pub struct PreparedTextureUpload {
    metadata: Texture,
    sampler: TextureSampler,
    mip_levels: Vec<PreparedMipLevel>,
    resident_bytes: u64,
    mip_level_count: u32,
}

struct PreparedMipLevel {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl PreparedTextureUpload {
    /// Validates level zero and builds the requested mip chain on the caller's
    /// thread.
    ///
    /// # Errors
    ///
    /// Returns [`TextureUploadError`] when the source payload does not match
    /// its metadata or an RGBA row/total size cannot be represented.
    pub fn rgba8(
        metadata: &Texture,
        pixels: &[u8],
        sampler: TextureSampler,
    ) -> Result<Self, TextureUploadError> {
        Self::rgba8_owned(metadata, pixels.to_vec(), sampler)
    }

    /// Owned equivalent of [`Self::rgba8`] that reuses level-zero storage.
    /// Prefer it when a decoder already returns an owned pixel buffer.
    ///
    /// # Errors
    ///
    /// Returns [`TextureUploadError`] under the same conditions as
    /// [`Self::rgba8`].
    pub fn rgba8_owned(
        metadata: &Texture,
        pixels: Vec<u8>,
        sampler: TextureSampler,
    ) -> Result<Self, TextureUploadError> {
        validate_rgba8(metadata, &pixels)?;
        let size = metadata.size();
        let maximum_levels = match sampler.mipmaps {
            TextureMipmapPolicy::Disabled => 1,
            TextureMipmapPolicy::Generate => full_mip_level_count(size.width(), size.height()),
        };
        let mut mip_levels = Vec::new();
        mip_levels.push(PreparedMipLevel {
            width: size.width(),
            height: size.height(),
            pixels,
        });
        if sampler.mipmaps == TextureMipmapPolicy::Generate {
            while let Some(previous) = mip_levels.last() {
                if previous.width == 1 && previous.height == 1 {
                    break;
                }
                let width = (previous.width / 2).max(1);
                let height = (previous.height / 2).max(1);
                let pixels = downsample_rgba8(
                    &previous.pixels,
                    previous.width,
                    previous.height,
                    width,
                    height,
                    metadata.color_space(),
                );
                mip_levels.push(PreparedMipLevel {
                    width,
                    height,
                    pixels,
                });
            }
        }
        let resident_bytes = mip_levels.iter().fold(0_u64, |total, level| {
            total.saturating_add(u64::try_from(level.pixels.len()).unwrap_or(u64::MAX))
        });
        Ok(Self {
            metadata: metadata.clone(),
            sampler,
            mip_levels,
            resident_bytes,
            mip_level_count: maximum_levels,
        })
    }

    /// Total uncompressed bytes across every prepared mip level.
    #[must_use]
    pub const fn resident_bytes(&self) -> u64 {
        self.resident_bytes
    }

    /// Number of levels that will become GPU resident.
    #[must_use]
    pub const fn mip_level_count(&self) -> u32 {
        self.mip_level_count
    }
}

impl GpuTexture {
    /// Returns the validated source texture metadata.
    #[must_use]
    pub const fn metadata(&self) -> &Texture {
        &self.metadata
    }

    /// Returns the source texture dimensions in physical pixels.
    #[must_use]
    pub const fn size(&self) -> TextureSize {
        self.metadata.size()
    }

    /// Returns the WGPU texture format selected from its colour space.
    #[must_use]
    pub const fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    /// Returns the sampling state used to create [`Self::sampler`].
    #[must_use]
    pub const fn sampler_settings(&self) -> TextureSampler {
        self.sampler_settings
    }

    /// Returns effective mip residency and sampler fallback information.
    #[must_use]
    pub const fn sampling_diagnostics(&self) -> TextureSamplingDiagnostics {
        self.sampling_diagnostics
    }

    /// Returns the texture view for a caller-owned material bind group.
    #[must_use]
    pub const fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    /// Returns the sampler for a caller-owned material bind group.
    #[must_use]
    pub const fn sampler(&self) -> &wgpu::Sampler {
        &self.sampler
    }
}

/// Result of adding or replacing an entry in [`TextureCache`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextureCacheUpdate {
    /// The handle had no resident GPU resource.
    Inserted,
    /// The handle already had a GPU resource, which was replaced.
    Replaced,
}

/// A device-local mapping from texture assets to reusable sampled GPU textures.
///
/// The cache does not watch CPU assets. Call [`Self::upsert`] after a new image
/// is decoded or the content of a texture asset changes, and [`Self::remove`]
/// when an asset is unloaded. Removing a stale or unknown handle is harmless.
#[derive(Default)]
pub struct TextureCache {
    textures: HashMap<TextureHandle, GpuTexture>,
}

impl TextureCache {
    /// Creates an empty GPU texture cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the resident GPU resource for a typed texture handle.
    #[must_use]
    pub fn get(&self, handle: TextureHandle) -> Option<&GpuTexture> {
        self.textures.get(&handle)
    }

    /// Returns the number of resident texture resources.
    #[must_use]
    pub fn len(&self) -> usize {
        self.textures.len()
    }

    /// Returns whether this cache holds no texture resources.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.textures.is_empty()
    }

    /// Uploads or replaces a texture using the renderer's selected device.
    ///
    /// This performs a full replacement rather than a partial write. That keeps
    /// view format, dimensions and sampler coherent if any image metadata has
    /// changed. Existing references returned by [`Self::get`] must not be held
    /// across this mutable call.
    ///
    /// # Errors
    ///
    /// Returns [`TextureUploadError`] if CPU metadata/bytes are inconsistent or
    /// the selected GPU cannot create the requested texture.
    pub fn upsert(
        &mut self,
        renderer: &Renderer,
        handle: TextureHandle,
        image: &DecodedImage,
        sampler: TextureSampler,
    ) -> Result<TextureCacheUpdate, TextureUploadError> {
        let anisotropy_supported = renderer.supports_anisotropic_filtering();
        renderer.with_raw_gpu(|device, queue, _configuration| {
            self.upsert_rgba8_with(
                device,
                queue,
                handle,
                image.texture(),
                image.pixels(),
                sampler,
                anisotropy_supported,
            )
        })
    }

    /// Uploads or replaces a texture using the device selected for `frame`.
    ///
    /// Use this while lazily initialising render resources inside an application
    /// render callback. Retain the cache outside the callback; creating the
    /// same texture every frame creates unnecessary GPU allocations.
    ///
    /// # Errors
    ///
    /// Returns [`TextureUploadError`] under the same conditions as
    /// [`Self::upsert`].
    pub fn upsert_for_frame(
        &mut self,
        frame: &RenderFrame<'_>,
        handle: TextureHandle,
        image: &DecodedImage,
        sampler: TextureSampler,
    ) -> Result<TextureCacheUpdate, TextureUploadError> {
        self.upsert_rgba8_with(
            frame.device(),
            frame.queue(),
            handle,
            image.texture(),
            image.pixels(),
            sampler,
            frame.supports_anisotropic_filtering(),
        )
    }

    /// Uploads or replaces tightly packed RGBA8 pixels with explicit metadata.
    ///
    /// This lower-level path is intended for importers that need to choose a
    /// colour space from material semantics. Most applications should use
    /// [`Self::upsert`] with the metadata produced by [`DecodedImage`].
    ///
    /// # Errors
    ///
    /// Returns [`TextureUploadError`] if `pixels` is not exactly the RGBA8
    /// payload described by `metadata`, or the GPU rejects its dimensions.
    pub fn upsert_rgba8(
        &mut self,
        renderer: &Renderer,
        handle: TextureHandle,
        metadata: &Texture,
        pixels: &[u8],
        sampler: TextureSampler,
    ) -> Result<TextureCacheUpdate, TextureUploadError> {
        let anisotropy_supported = renderer.supports_anisotropic_filtering();
        renderer.with_raw_gpu(|device, queue, _configuration| {
            self.upsert_rgba8_with(
                device,
                queue,
                handle,
                metadata,
                pixels,
                sampler,
                anisotropy_supported,
            )
        })
    }

    /// Frame-local equivalent of [`Self::upsert_rgba8`].
    ///
    /// # Errors
    ///
    /// Returns [`TextureUploadError`] under the same conditions as
    /// [`Self::upsert_rgba8`].
    pub fn upsert_rgba8_for_frame(
        &mut self,
        frame: &RenderFrame<'_>,
        handle: TextureHandle,
        metadata: &Texture,
        pixels: &[u8],
        sampler: TextureSampler,
    ) -> Result<TextureCacheUpdate, TextureUploadError> {
        self.upsert_rgba8_with(
            frame.device(),
            frame.queue(),
            handle,
            metadata,
            pixels,
            sampler,
            frame.supports_anisotropic_filtering(),
        )
    }

    /// Publishes a mip chain prepared on a worker using the frame's device.
    ///
    /// # Errors
    ///
    /// Returns [`TextureUploadError`] when the selected device cannot host the
    /// prepared dimensions.
    pub fn upsert_prepared_for_frame(
        &mut self,
        frame: &RenderFrame<'_>,
        handle: TextureHandle,
        prepared: &PreparedTextureUpload,
    ) -> Result<TextureCacheUpdate, TextureUploadError> {
        let texture = create_prepared_gpu_texture(
            frame.device(),
            frame.queue(),
            prepared,
            frame.supports_anisotropic_filtering(),
        )?;
        let outcome = if self.textures.insert(handle, texture).is_some() {
            TextureCacheUpdate::Replaced
        } else {
            TextureCacheUpdate::Inserted
        };
        Ok(outcome)
    }

    /// Removes a resident GPU texture and returns whether one existed.
    ///
    /// WGPU releases its backing resource once no more handles or submitted
    /// work reference it. This method does not invalidate the CPU asset handle.
    pub fn remove(&mut self, handle: TextureHandle) -> bool {
        self.textures.remove(&handle).is_some()
    }

    /// Releases every resident texture resource.
    pub fn clear(&mut self) {
        self.textures.clear();
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the private upload boundary keeps explicit device, asset, pixels, policy and capability"
    )]
    fn upsert_rgba8_with(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        handle: TextureHandle,
        metadata: &Texture,
        pixels: &[u8],
        sampler: TextureSampler,
        anisotropy_supported: bool,
    ) -> Result<TextureCacheUpdate, TextureUploadError> {
        let texture = create_gpu_texture(
            device,
            queue,
            metadata,
            pixels,
            sampler,
            anisotropy_supported,
        )?;
        let outcome = if self.textures.insert(handle, texture).is_some() {
            TextureCacheUpdate::Replaced
        } else {
            TextureCacheUpdate::Inserted
        };
        Ok(outcome)
    }
}

/// A validated image-to-GPU upload failure.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum TextureUploadError {
    /// Width × height × four cannot fit in the process address space.
    ByteSizeOverflow {
        /// Image width.
        width: u32,
        /// Image height.
        height: u32,
    },
    /// CPU pixels are not tightly packed RGBA8 data for their declared size.
    ByteLengthMismatch {
        /// Image width.
        width: u32,
        /// Image height.
        height: u32,
        /// Observed pixel byte count.
        actual: usize,
        /// Required `width * height * 4` count.
        expected: usize,
    },
    /// The GPU's maximum 2D texture dimension is too small.
    DimensionUnsupported {
        /// Image width.
        width: u32,
        /// Image height.
        height: u32,
        /// GPU maximum 2D dimension.
        maximum: u32,
    },
    /// The tightly-packed RGBA8 row pitch cannot fit WGPU's `u32` layout.
    RowPitchOverflow {
        /// Image width.
        width: u32,
    },
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
            Self::RowPitchOverflow { width } => {
                write!(formatter, "RGBA8 row pitch overflows for width {width}")
            }
        }
    }
}

impl Error for TextureUploadError {}

fn create_gpu_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    metadata: &Texture,
    pixels: &[u8],
    sampler_settings: TextureSampler,
    anisotropy_supported: bool,
) -> Result<GpuTexture, TextureUploadError> {
    let prepared = PreparedTextureUpload::rgba8(metadata, pixels, sampler_settings)?;
    create_prepared_gpu_texture(device, queue, &prepared, anisotropy_supported)
}

fn validate_rgba8(metadata: &Texture, pixels: &[u8]) -> Result<(), TextureUploadError> {
    let size = metadata.size();
    let width = size.width();
    let height = size.height();
    let expected = usize::try_from(u64::from(width) * u64::from(height) * 4)
        .map_err(|_| TextureUploadError::ByteSizeOverflow { width, height })?;
    if pixels.len() != expected {
        return Err(TextureUploadError::ByteLengthMismatch {
            width,
            height,
            actual: pixels.len(),
            expected,
        });
    }
    let _row_pitch = width
        .checked_mul(4)
        .ok_or(TextureUploadError::RowPitchOverflow { width })?;
    Ok(())
}

fn create_prepared_gpu_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    prepared: &PreparedTextureUpload,
    anisotropy_supported: bool,
) -> Result<GpuTexture, TextureUploadError> {
    let size = prepared.metadata.size();
    let width = size.width();
    let height = size.height();
    let maximum = device.limits().max_texture_dimension_2d;
    if width > maximum || height > maximum {
        return Err(TextureUploadError::DimensionUnsupported {
            width,
            height,
            maximum,
        });
    }
    let format = format_for(prepared.metadata.color_space());
    let mip_level_count = prepared.mip_level_count();
    let sampling_diagnostics =
        resolve_sampling(prepared.sampler, mip_level_count, anisotropy_supported);
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("yuyib sampled RGBA8 texture"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    for (mip_level, level) in prepared.mip_levels.iter().enumerate() {
        write_mip_level(
            queue,
            &texture,
            u32::try_from(mip_level).expect("RGBA8 mip count always fits u32"),
            level.width,
            level.height,
            level.width * 4,
            &level.pixels,
        );
    }
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("yuyib sampled texture sampler"),
        address_mode_u: prepared.sampler.address_mode_u,
        address_mode_v: prepared.sampler.address_mode_v,
        address_mode_w: prepared.sampler.address_mode_w,
        mag_filter: prepared.sampler.mag_filter,
        min_filter: prepared.sampler.min_filter,
        mipmap_filter: prepared.sampler.mipmap_filter,
        lod_max_clamp: f32::from(
            u16::try_from(mip_level_count.saturating_sub(1))
                .expect("RGBA8 mip count always fits u16"),
        ),
        anisotropy_clamp: sampling_diagnostics.effective_anisotropy,
        ..Default::default()
    });
    Ok(GpuTexture {
        metadata: prepared.metadata.clone(),
        format,
        sampler_settings: prepared.sampler,
        sampling_diagnostics,
        _texture: texture,
        view,
        sampler,
    })
}

fn write_mip_level(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    mip_level: u32,
    width: u32,
    height: u32,
    row_pitch: u32,
    pixels: &[u8],
) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(row_pitch),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
}

fn full_mip_level_count(width: u32, height: u32) -> u32 {
    width.max(height).bit_width()
}

fn resolve_sampling(
    sampler: TextureSampler,
    mip_level_count: u32,
    anisotropy_supported: bool,
) -> TextureSamplingDiagnostics {
    let requested_anisotropy = sampler.anisotropy_clamp;
    let normalized = requested_anisotropy.clamp(1, MAX_ANISOTROPY_CLAMP);
    let filters_are_linear = sampler.mag_filter == wgpu::FilterMode::Linear
        && sampler.min_filter == wgpu::FilterMode::Linear
        && sampler.mipmap_filter == wgpu::MipmapFilterMode::Linear;
    let (effective_anisotropy, anisotropy_fallback) = if requested_anisotropy == 0 {
        (1, Some(AnisotropyFallback::RaisedToMinimum))
    } else if normalized == 1 {
        (
            1,
            (requested_anisotropy > MAX_ANISOTROPY_CLAMP)
                .then_some(AnisotropyFallback::ClampedToPortableMaximum),
        )
    } else if !anisotropy_supported {
        (1, Some(AnisotropyFallback::Unsupported))
    } else if !filters_are_linear {
        (1, Some(AnisotropyFallback::NonLinearFilters))
    } else {
        (
            normalized,
            (requested_anisotropy > MAX_ANISOTROPY_CLAMP)
                .then_some(AnisotropyFallback::ClampedToPortableMaximum),
        )
    };
    TextureSamplingDiagnostics {
        mip_level_count,
        requested_anisotropy,
        effective_anisotropy,
        anisotropy_fallback,
    }
}

fn downsample_rgba8(
    source: &[u8],
    source_width: u32,
    source_height: u32,
    target_width: u32,
    target_height: u32,
    color_space: TextureColorSpace,
) -> Vec<u8> {
    let target_bytes = u64::from(target_width) * u64::from(target_height) * 4;
    let mut target = Vec::with_capacity(
        usize::try_from(target_bytes).expect("a downsampled RGBA8 level fits process memory"),
    );
    for target_y in 0..target_height {
        let start_y = target_y * source_height / target_height;
        let end_y = ((target_y + 1) * source_height / target_height).max(start_y + 1);
        for target_x in 0..target_width {
            let start_x = target_x * source_width / target_width;
            let end_x = ((target_x + 1) * source_width / target_width).max(start_x + 1);
            let mut premultiplied = [0.0_f32; 3];
            let mut alpha_sum = 0.0_f32;
            let mut samples = 0_u16;
            for source_y in start_y..end_y {
                for source_x in start_x..end_x {
                    let offset = usize::try_from(
                        (u64::from(source_y) * u64::from(source_width) + u64::from(source_x)) * 4,
                    )
                    .expect("validated RGBA8 source offset fits usize");
                    let alpha = f32::from(source[offset + 3]) / 255.0;
                    for channel in 0..3 {
                        let encoded = f32::from(source[offset + channel]) / 255.0;
                        let linear = match color_space {
                            TextureColorSpace::Srgb => srgb_to_linear(encoded),
                            TextureColorSpace::Linear => encoded,
                        };
                        premultiplied[channel] += linear * alpha;
                    }
                    alpha_sum += alpha;
                    samples += 1;
                }
            }
            let sample_count = f32::from(samples);
            let alpha = alpha_sum / sample_count;
            for value in premultiplied {
                let linear = if alpha_sum > 0.0 {
                    value / alpha_sum
                } else {
                    0.0
                };
                let encoded = match color_space {
                    TextureColorSpace::Srgb => linear_to_srgb(linear),
                    TextureColorSpace::Linear => linear,
                };
                target.push(float_to_unorm8(encoded));
            }
            target.push(float_to_unorm8(alpha));
        }
    }
    target
}

fn srgb_to_linear(value: f32) -> f32 {
    if value <= 0.040_45 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(value: f32) -> f32 {
    if value <= 0.003_130_8 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn float_to_unorm8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn format_for(color_space: TextureColorSpace) -> wgpu::TextureFormat {
    match color_space {
        TextureColorSpace::Srgb => wgpu::TextureFormat::Rgba8UnormSrgb,
        TextureColorSpace::Linear => wgpu::TextureFormat::Rgba8Unorm,
    }
}

#[cfg(test)]
mod tests {
    use yuyib_2d::{Texture, TextureColorSpace, TextureSize};
    use yuyib_assets::Assets;

    use super::{
        AnisotropyFallback, PreparedTextureUpload, TextureCache, TextureMipmapPolicy,
        TextureSampler, TextureSamplingPreset, downsample_rgba8, format_for, full_mip_level_count,
        resolve_sampling, wgpu,
    };

    #[test]
    fn an_empty_cache_has_no_entries() {
        let cache = TextureCache::new();
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn removing_unknown_or_stale_handles_is_harmless() {
        let mut assets = Assets::new();
        let handle = assets.insert(Texture::new(
            TextureSize::new(1, 1).expect("a one-pixel texture is valid"),
        ));
        let mut cache = TextureCache::new();
        assert!(!cache.remove(handle));
        let _ = assets.remove(handle);
        assert!(!cache.remove(handle));
    }

    #[test]
    fn colour_space_selects_the_expected_sampled_format() {
        assert_eq!(
            format_for(TextureColorSpace::Srgb),
            wgpu::TextureFormat::Rgba8UnormSrgb
        );
        assert_eq!(
            format_for(TextureColorSpace::Linear),
            wgpu::TextureFormat::Rgba8Unorm
        );
    }

    #[test]
    fn full_mip_chain_handles_rectangular_and_odd_sizes() {
        assert_eq!(full_mip_level_count(1, 1), 1);
        assert_eq!(full_mip_level_count(8, 3), 4);
        assert_eq!(full_mip_level_count(7, 9), 4);
    }

    #[test]
    fn prepared_upload_accounts_for_every_resident_level() {
        let metadata =
            Texture::new(TextureSize::new(4, 2).expect("test texture dimensions are non-zero"));
        let pixels = vec![255; 4 * 2 * 4];
        let mipmapped = PreparedTextureUpload::rgba8(
            &metadata,
            &pixels,
            TextureSamplingPreset::Balanced.sampler(),
        )
        .expect("test RGBA payload is valid");
        assert_eq!(mipmapped.mip_level_count(), 3);
        assert_eq!(mipmapped.resident_bytes(), 44);

        let level_zero = PreparedTextureUpload::rgba8(
            &metadata,
            &pixels,
            TextureSamplingPreset::PixelArt.sampler(),
        )
        .expect("test RGBA payload is valid");
        assert_eq!(level_zero.mip_level_count(), 1);
        assert_eq!(level_zero.resident_bytes(), 32);
    }

    #[test]
    fn presets_keep_high_level_intent_explicit() {
        let pixel_art = TextureSamplingPreset::PixelArt.sampler();
        assert_eq!(pixel_art.mipmaps, TextureMipmapPolicy::Disabled);
        assert_eq!(pixel_art.mag_filter, wgpu::FilterMode::Nearest);
        assert_eq!(pixel_art.anisotropy_clamp, 1);

        let quality = TextureSamplingPreset::HighQuality.sampler();
        assert_eq!(quality.mipmaps, TextureMipmapPolicy::Generate);
        assert_eq!(quality.mipmap_filter, wgpu::MipmapFilterMode::Linear);
        assert_eq!(quality.anisotropy_clamp, 16);
    }

    #[test]
    fn anisotropy_resolution_is_safe_and_diagnostic() {
        let sampler = TextureSampler::default().with_anisotropy_clamp(32);
        let supported = resolve_sampling(sampler, 5, true);
        assert_eq!(supported.effective_anisotropy(), 16);
        assert_eq!(
            supported.anisotropy_fallback(),
            Some(AnisotropyFallback::ClampedToPortableMaximum)
        );

        let unsupported = resolve_sampling(sampler, 5, false);
        assert_eq!(unsupported.effective_anisotropy(), 1);
        assert_eq!(
            unsupported.anisotropy_fallback(),
            Some(AnisotropyFallback::Unsupported)
        );

        let nearest = TextureSamplingPreset::PixelArt
            .sampler()
            .with_anisotropy_clamp(8);
        let incompatible = resolve_sampling(nearest, 1, true);
        assert_eq!(
            incompatible.anisotropy_fallback(),
            Some(AnisotropyFallback::NonLinearFilters)
        );
    }

    #[test]
    fn mip_generation_distinguishes_srgb_from_linear_data() {
        let pixels = [
            0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 255, 255, 255, 255, 255,
        ];
        let linear = downsample_rgba8(&pixels, 2, 2, 1, 1, TextureColorSpace::Linear);
        let srgb = downsample_rgba8(&pixels, 2, 2, 1, 1, TextureColorSpace::Srgb);
        assert_eq!(linear, [128, 128, 128, 255]);
        assert_eq!(srgb, [188, 188, 188, 255]);
    }

    #[test]
    fn transparent_mip_colors_are_alpha_weighted() {
        let pixels = [255, 0, 0, 255, 0, 255, 0, 0];
        let mip = downsample_rgba8(&pixels, 2, 1, 1, 1, TextureColorSpace::Linear);
        assert_eq!(mip, [255, 0, 0, 128]);
    }
}
