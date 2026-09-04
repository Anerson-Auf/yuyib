//! Safe texture-asset resolution for [`yuyib_model::Model`].
//!
//! A model stores external texture URIs, embedded encoded image bytes, or
//! importer-decoded RGBA8 pixels as renderer-neutral metadata. This crate
//! resolves relative filesystem URIs under one canonical asset root, decodes
//! encoded bytes, validates importer pixels,
//! applies the [`DecodePolicy`], inserts typed CPU texture metadata, and uploads
//! sampled GPU textures through [`TextureCache`]. The returned [`ModelTextureBindings`]
//! maps every **material-referenced** [`ModelTextureIndex`] to the typed handle a
//! material renderer can look up in that cache. Unused texture descriptors remain
//! import/diagnostics inventory and are not decoded or uploaded.
//!
//! This is deliberately **not** automatic material binding. A 3D renderer must
//! still choose its bind-group layout and use `TextureCache::get(handle)` to
//! bind the returned view and sampler. Keeping that boundary explicit prevents
//! an importer from silently choosing a shader contract.

#![forbid(unsafe_code)]

use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fmt, fs,
    path::{Component, Path, PathBuf},
};

use yuyib_2d::{Texture, TextureColorSpace, TextureHandle, TextureSize};
use yuyib_assets::Assets;
use yuyib_image::{DecodePolicy, ImageImportError, decode_bytes, decode_path};
use yuyib_model::{
    Model, ModelTextureAddressMode, ModelTextureIndex, ModelTextureMagFilter,
    ModelTextureMinFilter, ModelTextureSampler, ModelTextureSource,
};
use yuyib_render::{RenderFrame, Renderer};
use yuyib_render_texture::{
    PreparedTextureUpload, TextureCache, TextureMipmapPolicy, TextureSampler,
    TextureSamplingPreset, TextureUploadError,
};

/// The sampling colour space inferred for one model texture slot.
///
/// Base-colour and emissive bindings use sRGB. Normal and metallic-roughness
/// bindings use linear. An unreferenced texture descriptor defaults to sRGB so
/// it remains useful for application-defined material extensions.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ModelTextureColorSpace {
    /// Display-oriented colour data.
    Srgb,
    /// Non-colour material data.
    Linear,
}

impl ModelTextureColorSpace {
    const fn into_texture_color_space(self) -> TextureColorSpace {
        match self {
            Self::Srgb => TextureColorSpace::Srgb,
            Self::Linear => TextureColorSpace::Linear,
        }
    }
}

impl fmt::Display for ModelTextureColorSpace {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Srgb => "sRGB",
            Self::Linear => "linear",
        })
    }
}

/// A resolved model texture slot and its resident typed texture asset.
#[derive(Clone, Debug)]
pub struct ResolvedModelTexture {
    index: ModelTextureIndex,
    handle: TextureHandle,
    source: ResolvedModelTextureSource,
    color_space: ModelTextureColorSpace,
    sampler: TextureSampler,
    alpha: TextureAlphaSummary,
}

/// Compact alpha-channel statistics retained after RGBA pixels leave the worker.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TextureAlphaSummary {
    minimum: u8,
    maximum: u8,
    pixels_at_least_254: u64,
    total_pixels: u64,
}

impl TextureAlphaSummary {
    /// Computes statistics from tightly packed RGBA8 pixels.
    #[must_use]
    pub fn from_rgba8(pixels: &[u8]) -> Self {
        let mut minimum = u8::MAX;
        let mut maximum = u8::MIN;
        let mut pixels_at_least_254 = 0_u64;
        let mut total_pixels = 0_u64;
        for pixel in pixels.as_chunks::<4>().0 {
            let alpha = pixel[3];
            minimum = minimum.min(alpha);
            maximum = maximum.max(alpha);
            pixels_at_least_254 += u64::from(alpha >= 254);
            total_pixels += 1;
        }
        if total_pixels == 0 {
            return Self::default();
        }
        Self {
            minimum,
            maximum,
            pixels_at_least_254,
            total_pixels,
        }
    }

    /// Lowest decoded alpha sample.
    #[must_use]
    pub const fn minimum(self) -> u8 {
        self.minimum
    }

    /// Highest decoded alpha sample.
    #[must_use]
    pub const fn maximum(self) -> u8 {
        self.maximum
    }

    /// Pixels whose alpha is 254 or 255.
    #[must_use]
    pub const fn pixels_at_least_254(self) -> u64 {
        self.pixels_at_least_254
    }

    /// Number of complete RGBA8 pixels measured.
    #[must_use]
    pub const fn total_pixels(self) -> u64 {
        self.total_pixels
    }
}

/// The source that supplied one resident model texture.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolvedModelTextureSource {
    /// A canonical path below the loader's configured asset root.
    Path(PathBuf),
    /// An image embedded in the imported source asset.
    Embedded {
        /// Declared source MIME type.
        mime_type: String,
        /// Encoded source byte length, retained for diagnostics without copying
        /// potentially large importer-owned image data.
        encoded_bytes: usize,
    },
    /// Pixels decoded by the source-format importer before residency.
    DecodedRgba8 {
        /// Pixel width.
        width: u32,
        /// Pixel height.
        height: u32,
        /// Tightly packed source byte length.
        bytes: usize,
    },
}

impl ResolvedModelTexture {
    /// Returns the model-local texture slot this resource resolves.
    #[must_use]
    pub const fn index(&self) -> ModelTextureIndex {
        self.index
    }

    /// Returns the typed CPU/GPU texture asset handle.
    #[must_use]
    pub const fn handle(&self) -> TextureHandle {
        self.handle
    }

    /// Returns the renderer-neutral source that supplied this texture.
    #[must_use]
    pub const fn source(&self) -> &ResolvedModelTextureSource {
        &self.source
    }

    /// Returns the canonical file path for an externally resolved texture.
    /// Embedded GLB images deliberately have no filesystem path.
    #[must_use]
    pub fn source_path(&self) -> Option<&Path> {
        match &self.source {
            ResolvedModelTextureSource::Path(path) => Some(path),
            ResolvedModelTextureSource::Embedded { .. }
            | ResolvedModelTextureSource::DecodedRgba8 { .. } => None,
        }
    }

    /// Returns the sampled colour space assigned from material semantics.
    #[must_use]
    pub const fn color_space(&self) -> ModelTextureColorSpace {
        self.color_space
    }

    /// Returns the effective GPU sampling settings for this texture slot.
    ///
    /// Imported glTF samplers are kept per slot. Textures created directly by
    /// an application use the fallback selected on [`ModelTextureLoader`].
    #[must_use]
    pub const fn sampler(&self) -> TextureSampler {
        self.sampler
    }

    /// Returns alpha statistics computed during image decode.
    #[must_use]
    pub const fn alpha_summary(&self) -> TextureAlphaSummary {
        self.alpha
    }
}

/// Complete texture residency for one [`Model`].
///
/// Bindings cover material-referenced slots only. `get(index)` looks up by
/// [`ModelTextureIndex`] (dense when every descriptor is used; sparse when unused
/// inventory slots were skipped). Multiple slots may share the same handle when
/// they resolve to the same path and colour space.
#[derive(Clone, Debug, Default)]
pub struct ModelTextureBindings {
    slots: Vec<ResolvedModelTexture>,
    unique_handles: Vec<TextureHandle>,
}

/// CPU-decoded model images awaiting bounded render-thread GPU upload.
///
/// Build this on a worker with [`ModelTextureLoader::prepare`], then call
/// [`Self::upload_some_for_frame`] with a deliberate per-frame slot budget.
/// No image codec runs in that upload method.
pub struct PreparedModelTextures {
    slots: Vec<PreparedModelTexture>,
    next_slot: usize,
    resolved: Vec<ResolvedModelTexture>,
    unique_handles: Vec<TextureHandle>,
    resident: HashMap<PreparedTextureKey, TextureHandle>,
}

/// Work performed by one bounded prepared-texture publication call.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PreparedTextureUploadStats {
    /// Model-local slots resolved during this call, including deduplicated slots.
    pub uploaded_slots: usize,
    /// Unique uncompressed RGBA8 bytes across every resident mip level copied
    /// to newly-created GPU textures.
    pub uploaded_unique_bytes: u64,
    /// Whether one unique texture exceeded the requested byte target.
    pub uploaded_oversized_texture: bool,
}

struct PreparedModelTexture {
    index: ModelTextureIndex,
    source: ResolvedModelTextureSource,
    color_space: ModelTextureColorSpace,
    sampler: TextureSampler,
    metadata: Texture,
    gpu_upload: PreparedTextureUpload,
    alpha: TextureAlphaSummary,
    key: PreparedTextureKey,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum PreparedTextureKey {
    Path(PathBuf, ModelTextureColorSpace, TextureSampler),
    Embedded(
        std::sync::Arc<[u8]>,
        String,
        ModelTextureColorSpace,
        TextureSampler,
    ),
    DecodedRgba8(
        std::sync::Arc<[u8]>,
        u32,
        u32,
        ModelTextureColorSpace,
        TextureSampler,
    ),
}

impl PreparedModelTextures {
    /// Returns the number of material-referenced texture slots awaiting upload.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.slots.len()
    }

    /// Returns whether no material-referenced textures need GPU upload.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Returns the number of slots still awaiting publication.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.slots.len().saturating_sub(self.next_slot)
    }

    /// Uploads at most `maximum_slots` already-decoded texture slots.
    ///
    /// Duplicate source slots reuse one GPU texture. On error, every resource
    /// published by this preparation is rolled back transactionally.
    ///
    /// # Errors
    ///
    /// Returns [`ModelTextureLoadError`] when WGPU rejects an upload.
    pub fn upload_some_for_frame(
        &mut self,
        frame: &RenderFrame<'_>,
        textures: &mut Assets<Texture>,
        gpu_textures: &mut TextureCache,
        maximum_slots: usize,
    ) -> Result<usize, ModelTextureLoadError> {
        self.upload_with_budget_for_frame(frame, textures, gpu_textures, maximum_slots, u64::MAX)
            .map(|stats| stats.uploaded_slots)
    }

    /// Uploads prepared slots within hard slot and soft resident-byte limits.
    ///
    /// Duplicate slots reuse an existing texture and consume no byte budget.
    /// A unique texture is atomic in the current WGPU API, so the first unique
    /// texture may exceed `target_unique_bytes`; this is reported explicitly
    /// and prevents a large valid texture from stalling forever. A zero slot or
    /// byte budget pauses publication.
    ///
    /// # Errors
    ///
    /// Returns [`ModelTextureLoadError`] when WGPU rejects an upload. Every
    /// resource previously published by this preparation is rolled back.
    pub fn upload_with_budget_for_frame(
        &mut self,
        frame: &RenderFrame<'_>,
        textures: &mut Assets<Texture>,
        gpu_textures: &mut TextureCache,
        maximum_slots: usize,
        target_unique_bytes: u64,
    ) -> Result<PreparedTextureUploadStats, ModelTextureLoadError> {
        let stats = self.plan_upload(maximum_slots, target_unique_bytes);
        let end = self.next_slot.saturating_add(stats.uploaded_slots);
        while self.next_slot < end {
            let slot = &self.slots[self.next_slot];
            let handle = if let Some(&handle) = self.resident.get(&slot.key) {
                handle
            } else {
                let handle = textures.insert(slot.metadata.clone());
                if let Err(source) =
                    gpu_textures.upsert_prepared_for_frame(frame, handle, &slot.gpu_upload)
                {
                    let _ = textures.remove(handle);
                    rollback(&self.unique_handles, textures, gpu_textures);
                    self.unique_handles.clear();
                    self.resident.clear();
                    self.resolved.clear();
                    self.next_slot = 0;
                    return Err(match &slot.source {
                        ResolvedModelTextureSource::Path(path) => {
                            ModelTextureLoadError::UploadTexture {
                                index: slot.index,
                                path: path.clone(),
                                source,
                            }
                        }
                        ResolvedModelTextureSource::Embedded { mime_type, .. } => {
                            ModelTextureLoadError::UploadEmbeddedTexture {
                                index: slot.index,
                                mime_type: mime_type.clone(),
                                source,
                            }
                        }
                        ResolvedModelTextureSource::DecodedRgba8 {
                            width,
                            height,
                            bytes,
                        } => ModelTextureLoadError::UploadDecodedRgba8 {
                            index: slot.index,
                            width: *width,
                            height: *height,
                            bytes: *bytes,
                            source,
                        },
                    });
                }
                self.resident.insert(slot.key.clone(), handle);
                self.unique_handles.push(handle);
                handle
            };
            self.resolved.push(ResolvedModelTexture {
                index: slot.index,
                handle,
                source: slot.source.clone(),
                color_space: slot.color_space,
                sampler: slot.sampler,
                alpha: slot.alpha,
            });
            self.next_slot += 1;
        }
        Ok(stats)
    }

    fn plan_upload(
        &self,
        maximum_slots: usize,
        target_unique_bytes: u64,
    ) -> PreparedTextureUploadStats {
        if maximum_slots == 0 || target_unique_bytes == 0 {
            return PreparedTextureUploadStats::default();
        }
        let mut stats = PreparedTextureUploadStats::default();
        let mut planned_unique = HashSet::new();
        for slot in self.slots.iter().skip(self.next_slot).take(maximum_slots) {
            let unique =
                !self.resident.contains_key(&slot.key) && planned_unique.insert(slot.key.clone());
            let bytes = unique.then(|| slot.gpu_upload.resident_bytes());
            if let Some(bytes) = bytes {
                if stats.uploaded_unique_bytes != 0
                    && stats.uploaded_unique_bytes.saturating_add(bytes) > target_unique_bytes
                {
                    break;
                }
                if stats.uploaded_unique_bytes == 0 && bytes > target_unique_bytes {
                    stats.uploaded_oversized_texture = true;
                }
                stats.uploaded_unique_bytes = stats.uploaded_unique_bytes.saturating_add(bytes);
            }
            stats.uploaded_slots += 1;
        }
        stats
    }

    /// Converts a fully uploaded preparation into stable model bindings.
    ///
    /// # Errors
    ///
    /// Returns [`PreparedModelTexturesIncomplete`] while slots remain.
    pub fn finish(self) -> Result<ModelTextureBindings, PreparedModelTexturesIncomplete> {
        if self.next_slot != self.slots.len() {
            return Err(PreparedModelTexturesIncomplete {
                remaining: self.remaining(),
            });
        }
        Ok(ModelTextureBindings {
            slots: self.resolved,
            unique_handles: self.unique_handles,
        })
    }

    /// Releases any slots already uploaded by this unfinished preparation.
    pub fn release(self, textures: &mut Assets<Texture>, gpu_textures: &mut TextureCache) {
        rollback(&self.unique_handles, textures, gpu_textures);
    }
}

/// A prepared texture set was finalized before all slots were uploaded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreparedModelTexturesIncomplete {
    remaining: usize,
}

impl PreparedModelTexturesIncomplete {
    /// Returns the number of slots still awaiting upload.
    #[must_use]
    pub const fn remaining(self) -> usize {
        self.remaining
    }
}

impl fmt::Display for PreparedModelTexturesIncomplete {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} prepared texture slots remain",
            self.remaining
        )
    }
}

impl Error for PreparedModelTexturesIncomplete {}

impl ModelTextureBindings {
    /// Returns the resolved resource for one model texture slot.
    ///
    /// Unused inventory indices that were skipped during prepare/load return
    /// [`None`].
    #[must_use]
    pub fn get(&self, index: ModelTextureIndex) -> Option<&ResolvedModelTexture> {
        if let Some(slot) = self.slots.get(index.get())
            && slot.index == index
        {
            return Some(slot);
        }
        self.slots.iter().find(|slot| slot.index == index)
    }

    /// Returns every prepared slot in ascending source-index order.
    #[must_use]
    pub fn slots(&self) -> &[ResolvedModelTexture] {
        &self.slots
    }

    /// Returns the number of prepared (material-referenced) texture slots.
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Returns whether no material-referenced textures were prepared.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Removes the CPU metadata and GPU texture resources created for this load.
    ///
    /// Do not call this while a material renderer still holds bind groups that
    /// reference these GPU textures. The typed handles become stale after this
    /// operation. Shared slots are released only once.
    pub fn release(self, textures: &mut Assets<Texture>, gpu_textures: &mut TextureCache) {
        for handle in self.unique_handles {
            let _ = gpu_textures.remove(handle);
            let _ = textures.remove(handle);
        }
    }
}

/// A resolver rooted at one canonical asset directory.
///
/// The root is captured canonically during construction. Every URI is
/// canonicalized before it is opened and must stay below this root, including
/// after symlink resolution.
#[derive(Clone, Debug)]
pub struct ModelTextureLoader {
    asset_root: PathBuf,
    decode_policy: DecodePolicy,
    sampler: TextureSampler,
    sampling_preset: Option<TextureSamplingPreset>,
}

impl ModelTextureLoader {
    /// Creates a resolver for texture paths below `asset_root`.
    ///
    /// # Errors
    ///
    /// Returns [`ModelTextureLoaderInitError`] if the root cannot be resolved
    /// or is not a directory.
    pub fn new(asset_root: impl AsRef<Path>) -> Result<Self, ModelTextureLoaderInitError> {
        let requested = asset_root.as_ref();
        let asset_root = fs::canonicalize(requested).map_err(|source| {
            ModelTextureLoaderInitError::ResolveAssetRoot {
                path: requested.to_owned(),
                source,
            }
        })?;
        if !asset_root.is_dir() {
            return Err(ModelTextureLoaderInitError::AssetRootNotDirectory { path: asset_root });
        }
        Ok(Self {
            asset_root,
            decode_policy: DecodePolicy::default(),
            sampler: TextureSampler::default(),
            sampling_preset: Some(TextureSamplingPreset::Balanced),
        })
    }

    /// Replaces the decode policy used for every referenced image.
    #[must_use]
    pub const fn with_decode_policy(mut self, decode_policy: DecodePolicy) -> Self {
        self.decode_policy = decode_policy;
        self
    }

    /// Replaces the fallback sampler used by texture descriptors that do not
    /// carry importer-provided settings and disables the high-level preset.
    /// Explicit importer samplers retain their exact semantics.
    #[must_use]
    pub const fn with_sampler(mut self, sampler: TextureSampler) -> Self {
        self.sampler = sampler;
        self.sampling_preset = None;
        self
    }

    /// Applies one high-level sampling preset to imported and fallback
    /// samplers while retaining source address modes.
    #[must_use]
    pub const fn with_texture_sampling_preset(mut self, preset: TextureSamplingPreset) -> Self {
        self.sampling_preset = Some(preset);
        self
    }

    /// Keeps importer filter and mip choices exactly as declared.
    ///
    /// This is the low-level escape hatch for assets whose sampler contract is
    /// authoritative. Address modes are preserved in either policy.
    #[must_use]
    pub const fn preserve_imported_sampling(mut self) -> Self {
        self.sampling_preset = None;
        self
    }

    /// Returns the canonical root containing all allowed texture files.
    #[must_use]
    pub fn asset_root(&self) -> &Path {
        &self.asset_root
    }

    /// Resolves and decodes every material-referenced model image without
    /// touching a GPU device.
    ///
    /// Unused texture descriptors (no material binding) stay import/diagnostics
    /// inventory and are not opened or decoded — including missing external URIs.
    ///
    /// This is the worker-thread half of bounded model publication. The
    /// returned pixels can be uploaded over several frames through
    /// [`PreparedModelTextures::upload_some_for_frame`].
    ///
    /// # Errors
    ///
    /// Returns [`ModelTextureLoadError`] for unsafe paths, incompatible colour
    /// semantics or image decode failures on referenced slots.
    pub fn prepare(&self, model: &Model) -> Result<PreparedModelTextures, ModelTextureLoadError> {
        let color_spaces = infer_color_spaces(model)?;
        let referenced = referenced_texture_indices(model);
        let slots = model
            .textures()
            .iter()
            .enumerate()
            .filter(|(slot, _)| referenced.contains(slot))
            .map(|(slot, descriptor)| {
                let index = ModelTextureIndex::new(slot);
                let color_space = color_spaces[slot];
                let sampler = self.resolve_sampler(descriptor);
                let (metadata, pixels, source, key) = match descriptor.source() {
                    ModelTextureSource::ExternalUri(uri) => {
                        let path = self.resolve_uri(index, uri)?;
                        let image = decode_path(&path, self.decode_policy).map_err(|source| {
                            ModelTextureLoadError::DecodeImage {
                                index,
                                path: path.clone(),
                                source,
                            }
                        })?;
                        let key = PreparedTextureKey::Path(path.clone(), color_space, sampler);
                        let metadata = Texture::new(image.texture().size())
                            .with_alpha_mode(image.texture().alpha_mode())
                            .with_color_space(color_space.into_texture_color_space());
                        (
                            metadata,
                            image.into_pixels(),
                            ResolvedModelTextureSource::Path(path),
                            key,
                        )
                    }
                    ModelTextureSource::Encoded { mime_type, bytes } => {
                        let image = decode_bytes(bytes, self.decode_policy).map_err(|source| {
                            ModelTextureLoadError::DecodeEmbeddedImage {
                                index,
                                mime_type: mime_type.clone(),
                                source,
                            }
                        })?;
                        let key = PreparedTextureKey::Embedded(
                            bytes.clone(),
                            mime_type.clone(),
                            color_space,
                            sampler,
                        );
                        let metadata = Texture::new(image.texture().size())
                            .with_alpha_mode(image.texture().alpha_mode())
                            .with_color_space(color_space.into_texture_color_space());
                        (
                            metadata,
                            image.into_pixels(),
                            ResolvedModelTextureSource::Embedded {
                                mime_type: mime_type.clone(),
                                encoded_bytes: bytes.len(),
                            },
                            key,
                        )
                    }
                    ModelTextureSource::DecodedRgba8 {
                        width,
                        height,
                        pixels,
                    } => {
                        let metadata =
                            decoded_rgba8_metadata(index, *width, *height, pixels, color_space)?;
                        (
                            metadata,
                            pixels.to_vec(),
                            ResolvedModelTextureSource::DecodedRgba8 {
                                width: *width,
                                height: *height,
                                bytes: pixels.len(),
                            },
                            PreparedTextureKey::DecodedRgba8(
                                pixels.clone(),
                                *width,
                                *height,
                                color_space,
                                sampler,
                            ),
                        )
                    }
                };
                let alpha = TextureAlphaSummary::from_rgba8(&pixels);
                let gpu_upload = PreparedTextureUpload::rgba8_owned(&metadata, pixels, sampler)
                    .map_err(|error| prepared_texture_error(index, &source, error))?;
                Ok(PreparedModelTexture {
                    index,
                    source,
                    color_space,
                    sampler,
                    metadata,
                    gpu_upload,
                    alpha,
                    key,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(PreparedModelTextures {
            slots,
            next_slot: 0,
            resolved: Vec::new(),
            unique_handles: Vec::new(),
            resident: HashMap::new(),
        })
    }

    /// Decodes and uploads every material-referenced texture descriptor of `model`.
    ///
    /// On failure, any textures inserted earlier in this call are removed from
    /// both stores, so the operation has no partial residency result. URI paths
    /// must be local, relative files that canonically remain below
    /// [`Self::asset_root`]. The loader deduplicates slots with an identical
    /// canonical path and colour space.
    ///
    /// # Errors
    ///
    /// Returns [`ModelTextureLoadError`] for unsafe paths, decode/upload errors
    /// or one source texture used as incompatible sRGB and linear material data.
    #[allow(
        clippy::too_many_lines,
        reason = "External and embedded source paths share one transactional rollback boundary."
    )]
    pub fn load(
        &self,
        renderer: &Renderer,
        model: &Model,
        textures: &mut Assets<Texture>,
        gpu_textures: &mut TextureCache,
    ) -> Result<ModelTextureBindings, ModelTextureLoadError> {
        let color_spaces = infer_color_spaces(model)?;
        let referenced = referenced_texture_indices(model);
        let mut resolved = Vec::with_capacity(referenced.len());
        let mut unique_handles = Vec::new();
        let mut path_cache = HashMap::<
            (PathBuf, ModelTextureColorSpace, TextureSampler),
            (TextureHandle, TextureAlphaSummary),
        >::new();
        let mut embedded_cache = HashMap::<
            (
                std::sync::Arc<[u8]>,
                String,
                ModelTextureColorSpace,
                TextureSampler,
            ),
            (TextureHandle, TextureAlphaSummary),
        >::new();
        let mut decoded_cache = HashMap::<
            (
                std::sync::Arc<[u8]>,
                u32,
                u32,
                ModelTextureColorSpace,
                TextureSampler,
            ),
            (TextureHandle, TextureAlphaSummary),
        >::new();

        for (slot, descriptor) in model.textures().iter().enumerate() {
            if !referenced.contains(&slot) {
                continue;
            }
            let index = ModelTextureIndex::new(slot);
            let color_space = color_spaces[slot];
            let sampler = self.resolve_sampler(descriptor);
            let (handle, source, alpha) = match descriptor.source() {
                ModelTextureSource::ExternalUri(uri) => {
                    let source_path = match self.resolve_uri(index, uri) {
                        Ok(path) => path,
                        Err(error) => {
                            rollback(&unique_handles, textures, gpu_textures);
                            return Err(error);
                        }
                    };
                    let key = (source_path.clone(), color_space, sampler);
                    let (handle, alpha) = if let Some(&resident) = path_cache.get(&key) {
                        resident
                    } else {
                        let image = match decode_path(&source_path, self.decode_policy) {
                            Ok(image) => image,
                            Err(source) => {
                                rollback(&unique_handles, textures, gpu_textures);
                                return Err(ModelTextureLoadError::DecodeImage {
                                    index,
                                    path: source_path.clone(),
                                    source,
                                });
                            }
                        };
                        let metadata = Texture::new(image.texture().size())
                            .with_alpha_mode(image.texture().alpha_mode())
                            .with_color_space(color_space.into_texture_color_space());
                        let alpha = TextureAlphaSummary::from_rgba8(image.pixels());
                        let handle = textures.insert(metadata.clone());
                        if let Err(source) = gpu_textures.upsert_rgba8(
                            renderer,
                            handle,
                            &metadata,
                            image.pixels(),
                            sampler,
                        ) {
                            let _ = textures.remove(handle);
                            rollback(&unique_handles, textures, gpu_textures);
                            return Err(ModelTextureLoadError::UploadTexture {
                                index,
                                path: source_path,
                                source,
                            });
                        }
                        path_cache.insert(key, (handle, alpha));
                        unique_handles.push(handle);
                        (handle, alpha)
                    };
                    (handle, ResolvedModelTextureSource::Path(source_path), alpha)
                }
                ModelTextureSource::Encoded { mime_type, bytes } => {
                    let key = (bytes.clone(), mime_type.clone(), color_space, sampler);
                    let (handle, alpha) = if let Some(&resident) = embedded_cache.get(&key) {
                        resident
                    } else {
                        let image = match decode_bytes(bytes, self.decode_policy) {
                            Ok(image) => image,
                            Err(source) => {
                                rollback(&unique_handles, textures, gpu_textures);
                                return Err(ModelTextureLoadError::DecodeEmbeddedImage {
                                    index,
                                    mime_type: mime_type.clone(),
                                    source,
                                });
                            }
                        };
                        let metadata = Texture::new(image.texture().size())
                            .with_alpha_mode(image.texture().alpha_mode())
                            .with_color_space(color_space.into_texture_color_space());
                        let alpha = TextureAlphaSummary::from_rgba8(image.pixels());
                        let handle = textures.insert(metadata.clone());
                        if let Err(source) = gpu_textures.upsert_rgba8(
                            renderer,
                            handle,
                            &metadata,
                            image.pixels(),
                            sampler,
                        ) {
                            let _ = textures.remove(handle);
                            rollback(&unique_handles, textures, gpu_textures);
                            return Err(ModelTextureLoadError::UploadEmbeddedTexture {
                                index,
                                mime_type: mime_type.clone(),
                                source,
                            });
                        }
                        embedded_cache.insert(key, (handle, alpha));
                        unique_handles.push(handle);
                        (handle, alpha)
                    };
                    (
                        handle,
                        ResolvedModelTextureSource::Embedded {
                            mime_type: mime_type.clone(),
                            encoded_bytes: bytes.len(),
                        },
                        alpha,
                    )
                }
                ModelTextureSource::DecodedRgba8 {
                    width,
                    height,
                    pixels,
                } => {
                    let key = (pixels.clone(), *width, *height, color_space, sampler);
                    let (handle, alpha) = if let Some(&resident) = decoded_cache.get(&key) {
                        resident
                    } else {
                        let metadata = match decoded_rgba8_metadata(
                            index,
                            *width,
                            *height,
                            pixels,
                            color_space,
                        ) {
                            Ok(metadata) => metadata,
                            Err(error) => {
                                rollback(&unique_handles, textures, gpu_textures);
                                return Err(error);
                            }
                        };
                        let alpha = TextureAlphaSummary::from_rgba8(pixels);
                        let handle = textures.insert(metadata.clone());
                        if let Err(source) =
                            gpu_textures.upsert_rgba8(renderer, handle, &metadata, pixels, sampler)
                        {
                            let _ = textures.remove(handle);
                            rollback(&unique_handles, textures, gpu_textures);
                            return Err(ModelTextureLoadError::UploadDecodedRgba8 {
                                index,
                                width: *width,
                                height: *height,
                                bytes: pixels.len(),
                                source,
                            });
                        }
                        decoded_cache.insert(key, (handle, alpha));
                        unique_handles.push(handle);
                        (handle, alpha)
                    };
                    (
                        handle,
                        ResolvedModelTextureSource::DecodedRgba8 {
                            width: *width,
                            height: *height,
                            bytes: pixels.len(),
                        },
                        alpha,
                    )
                }
            };
            resolved.push(ResolvedModelTexture {
                index,
                handle,
                source,
                color_space,
                sampler,
                alpha,
            });
        }
        Ok(ModelTextureBindings {
            slots: resolved,
            unique_handles,
        })
    }

    /// Decodes and uploads every material-referenced texture descriptor using the
    /// GPU device of the currently-recording frame.
    ///
    /// This is the frame-callback counterpart of [`Self::load`]. Retain the
    /// returned bindings, CPU texture store and GPU cache after the callback:
    /// calling this for an already-resident model would allocate textures again.
    ///
    /// # Errors
    ///
    /// Returns the same validation and decoding failures as [`Self::load`].
    #[allow(
        clippy::too_many_lines,
        reason = "External and embedded source paths share one transactional rollback boundary."
    )]
    pub fn load_for_frame(
        &self,
        frame: &RenderFrame<'_>,
        model: &Model,
        textures: &mut Assets<Texture>,
        gpu_textures: &mut TextureCache,
    ) -> Result<ModelTextureBindings, ModelTextureLoadError> {
        let color_spaces = infer_color_spaces(model)?;
        let referenced = referenced_texture_indices(model);
        let mut resolved = Vec::with_capacity(referenced.len());
        let mut unique_handles = Vec::new();
        let mut path_cache = HashMap::<
            (PathBuf, ModelTextureColorSpace, TextureSampler),
            (TextureHandle, TextureAlphaSummary),
        >::new();
        let mut embedded_cache = HashMap::<
            (
                std::sync::Arc<[u8]>,
                String,
                ModelTextureColorSpace,
                TextureSampler,
            ),
            (TextureHandle, TextureAlphaSummary),
        >::new();
        let mut decoded_cache = HashMap::<
            (
                std::sync::Arc<[u8]>,
                u32,
                u32,
                ModelTextureColorSpace,
                TextureSampler,
            ),
            (TextureHandle, TextureAlphaSummary),
        >::new();

        for (slot, descriptor) in model.textures().iter().enumerate() {
            if !referenced.contains(&slot) {
                continue;
            }
            let index = ModelTextureIndex::new(slot);
            let color_space = color_spaces[slot];
            let sampler = self.resolve_sampler(descriptor);
            let (handle, source, alpha) = match descriptor.source() {
                ModelTextureSource::ExternalUri(uri) => {
                    let source_path = match self.resolve_uri(index, uri) {
                        Ok(path) => path,
                        Err(error) => {
                            rollback(&unique_handles, textures, gpu_textures);
                            return Err(error);
                        }
                    };
                    let key = (source_path.clone(), color_space, sampler);
                    let (handle, alpha) = if let Some(&resident) = path_cache.get(&key) {
                        resident
                    } else {
                        let image = match decode_path(&source_path, self.decode_policy) {
                            Ok(image) => image,
                            Err(source) => {
                                rollback(&unique_handles, textures, gpu_textures);
                                return Err(ModelTextureLoadError::DecodeImage {
                                    index,
                                    path: source_path.clone(),
                                    source,
                                });
                            }
                        };
                        let metadata = Texture::new(image.texture().size())
                            .with_alpha_mode(image.texture().alpha_mode())
                            .with_color_space(color_space.into_texture_color_space());
                        let alpha = TextureAlphaSummary::from_rgba8(image.pixels());
                        let handle = textures.insert(metadata.clone());
                        if let Err(source) = gpu_textures.upsert_rgba8_for_frame(
                            frame,
                            handle,
                            &metadata,
                            image.pixels(),
                            sampler,
                        ) {
                            let _ = textures.remove(handle);
                            rollback(&unique_handles, textures, gpu_textures);
                            return Err(ModelTextureLoadError::UploadTexture {
                                index,
                                path: source_path,
                                source,
                            });
                        }
                        path_cache.insert(key, (handle, alpha));
                        unique_handles.push(handle);
                        (handle, alpha)
                    };
                    (handle, ResolvedModelTextureSource::Path(source_path), alpha)
                }
                ModelTextureSource::Encoded { mime_type, bytes } => {
                    let key = (bytes.clone(), mime_type.clone(), color_space, sampler);
                    let (handle, alpha) = if let Some(&resident) = embedded_cache.get(&key) {
                        resident
                    } else {
                        let image = match decode_bytes(bytes, self.decode_policy) {
                            Ok(image) => image,
                            Err(source) => {
                                rollback(&unique_handles, textures, gpu_textures);
                                return Err(ModelTextureLoadError::DecodeEmbeddedImage {
                                    index,
                                    mime_type: mime_type.clone(),
                                    source,
                                });
                            }
                        };
                        let metadata = Texture::new(image.texture().size())
                            .with_alpha_mode(image.texture().alpha_mode())
                            .with_color_space(color_space.into_texture_color_space());
                        let alpha = TextureAlphaSummary::from_rgba8(image.pixels());
                        let handle = textures.insert(metadata.clone());
                        if let Err(source) = gpu_textures.upsert_rgba8_for_frame(
                            frame,
                            handle,
                            &metadata,
                            image.pixels(),
                            sampler,
                        ) {
                            let _ = textures.remove(handle);
                            rollback(&unique_handles, textures, gpu_textures);
                            return Err(ModelTextureLoadError::UploadEmbeddedTexture {
                                index,
                                mime_type: mime_type.clone(),
                                source,
                            });
                        }
                        embedded_cache.insert(key, (handle, alpha));
                        unique_handles.push(handle);
                        (handle, alpha)
                    };
                    (
                        handle,
                        ResolvedModelTextureSource::Embedded {
                            mime_type: mime_type.clone(),
                            encoded_bytes: bytes.len(),
                        },
                        alpha,
                    )
                }
                ModelTextureSource::DecodedRgba8 {
                    width,
                    height,
                    pixels,
                } => {
                    let key = (pixels.clone(), *width, *height, color_space, sampler);
                    let (handle, alpha) = if let Some(&resident) = decoded_cache.get(&key) {
                        resident
                    } else {
                        let metadata = match decoded_rgba8_metadata(
                            index,
                            *width,
                            *height,
                            pixels,
                            color_space,
                        ) {
                            Ok(metadata) => metadata,
                            Err(error) => {
                                rollback(&unique_handles, textures, gpu_textures);
                                return Err(error);
                            }
                        };
                        let alpha = TextureAlphaSummary::from_rgba8(pixels);
                        let handle = textures.insert(metadata.clone());
                        if let Err(source) = gpu_textures
                            .upsert_rgba8_for_frame(frame, handle, &metadata, pixels, sampler)
                        {
                            let _ = textures.remove(handle);
                            rollback(&unique_handles, textures, gpu_textures);
                            return Err(ModelTextureLoadError::UploadDecodedRgba8 {
                                index,
                                width: *width,
                                height: *height,
                                bytes: pixels.len(),
                                source,
                            });
                        }
                        decoded_cache.insert(key, (handle, alpha));
                        unique_handles.push(handle);
                        (handle, alpha)
                    };
                    (
                        handle,
                        ResolvedModelTextureSource::DecodedRgba8 {
                            width: *width,
                            height: *height,
                            bytes: pixels.len(),
                        },
                        alpha,
                    )
                }
            };
            resolved.push(ResolvedModelTexture {
                index,
                handle,
                source,
                color_space,
                sampler,
                alpha,
            });
        }
        Ok(ModelTextureBindings {
            slots: resolved,
            unique_handles,
        })
    }

    fn resolve_uri(
        &self,
        index: ModelTextureIndex,
        uri: &str,
    ) -> Result<PathBuf, ModelTextureLoadError> {
        if uri.starts_with("data:") || uri.contains("://") {
            return Err(ModelTextureLoadError::UnsupportedUri {
                index,
                uri: uri.to_owned(),
            });
        }
        let relative = Path::new(uri);
        if relative
            .components()
            .any(|component| matches!(component, Component::Prefix(_) | Component::RootDir))
        {
            return Err(ModelTextureLoadError::UnsafeTexturePath {
                index,
                uri: uri.to_owned(),
            });
        }
        let requested = self.asset_root.join(relative);
        let canonical = fs::canonicalize(&requested).map_err(|source| {
            ModelTextureLoadError::ResolveTexturePath {
                index,
                uri: uri.to_owned(),
                path: requested,
                source,
            }
        })?;
        if !canonical.starts_with(&self.asset_root) {
            return Err(ModelTextureLoadError::UnsafeTexturePath {
                index,
                uri: uri.to_owned(),
            });
        }
        Ok(canonical)
    }

    fn resolve_sampler(&self, descriptor: &yuyib_model::ModelTexture) -> TextureSampler {
        let sampler = descriptor
            .sampler()
            .map_or(self.sampler, model_sampler_to_gpu);
        self.sampling_preset
            .map_or(sampler, |preset| apply_sampling_preset(sampler, preset))
    }
}

fn apply_sampling_preset(
    mut sampler: TextureSampler,
    preset: TextureSamplingPreset,
) -> TextureSampler {
    let settings = preset.sampler();
    sampler.mag_filter = settings.mag_filter;
    sampler.min_filter = settings.min_filter;
    sampler.mipmap_filter = settings.mipmap_filter;
    sampler.mipmaps = settings.mipmaps;
    sampler.anisotropy_clamp = settings.anisotropy_clamp;
    sampler
}

fn model_sampler_to_gpu(sampler: ModelTextureSampler) -> TextureSampler {
    let mipmaps = match sampler.min_filter {
        ModelTextureMinFilter::Nearest | ModelTextureMinFilter::Linear => {
            TextureMipmapPolicy::Disabled
        }
        ModelTextureMinFilter::NearestMipmapNearest
        | ModelTextureMinFilter::LinearMipmapNearest
        | ModelTextureMinFilter::NearestMipmapLinear
        | ModelTextureMinFilter::LinearMipmapLinear => TextureMipmapPolicy::Generate,
    };
    let anisotropy_clamp = if sampler.mag_filter == ModelTextureMagFilter::Linear
        && sampler.min_filter == ModelTextureMinFilter::LinearMipmapLinear
    {
        TextureSamplingPreset::Balanced.sampler().anisotropy_clamp
    } else {
        1
    };
    TextureSampler {
        address_mode_u: match sampler.address_mode_u {
            ModelTextureAddressMode::Repeat => wgpu::AddressMode::Repeat,
            ModelTextureAddressMode::MirroredRepeat => wgpu::AddressMode::MirrorRepeat,
            ModelTextureAddressMode::ClampToEdge => wgpu::AddressMode::ClampToEdge,
        },
        address_mode_v: match sampler.address_mode_v {
            ModelTextureAddressMode::Repeat => wgpu::AddressMode::Repeat,
            ModelTextureAddressMode::MirroredRepeat => wgpu::AddressMode::MirrorRepeat,
            ModelTextureAddressMode::ClampToEdge => wgpu::AddressMode::ClampToEdge,
        },
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: match sampler.mag_filter {
            ModelTextureMagFilter::Nearest => wgpu::FilterMode::Nearest,
            ModelTextureMagFilter::Linear => wgpu::FilterMode::Linear,
        },
        min_filter: match sampler.min_filter {
            ModelTextureMinFilter::Nearest
            | ModelTextureMinFilter::NearestMipmapNearest
            | ModelTextureMinFilter::NearestMipmapLinear => wgpu::FilterMode::Nearest,
            ModelTextureMinFilter::Linear
            | ModelTextureMinFilter::LinearMipmapNearest
            | ModelTextureMinFilter::LinearMipmapLinear => wgpu::FilterMode::Linear,
        },
        mipmap_filter: match sampler.min_filter {
            ModelTextureMinFilter::NearestMipmapLinear
            | ModelTextureMinFilter::LinearMipmapLinear => wgpu::MipmapFilterMode::Linear,
            ModelTextureMinFilter::Nearest
            | ModelTextureMinFilter::Linear
            | ModelTextureMinFilter::NearestMipmapNearest
            | ModelTextureMinFilter::LinearMipmapNearest => wgpu::MipmapFilterMode::Nearest,
        },
        mipmaps,
        anisotropy_clamp,
    }
}

fn referenced_texture_indices(model: &Model) -> HashSet<usize> {
    model
        .texture_usage()
        .textures()
        .iter()
        .filter(|entry| !entry.material_users.is_empty())
        .map(|entry| entry.index.get())
        .collect()
}

fn infer_color_spaces(model: &Model) -> Result<Vec<ModelTextureColorSpace>, ModelTextureLoadError> {
    let mut spaces = vec![None; model.textures().len()];
    for material in model.materials() {
        for binding in [material.base_color_texture(), material.emissive_texture()]
            .into_iter()
            .flatten()
        {
            record_color_space(&mut spaces, binding.texture(), ModelTextureColorSpace::Srgb)?;
        }
        if let Some(binding) = material.normal_texture() {
            record_color_space(
                &mut spaces,
                binding.binding().texture(),
                ModelTextureColorSpace::Linear,
            )?;
        }
        if let Some(binding) = material.metallic_roughness_texture() {
            record_color_space(
                &mut spaces,
                binding.texture(),
                ModelTextureColorSpace::Linear,
            )?;
        }
        if let Some(workflow) = material.specular_glossiness() {
            for binding in [
                workflow.diffuse_texture(),
                workflow.specular_glossiness_texture(),
            ]
            .into_iter()
            .flatten()
            {
                record_color_space(&mut spaces, binding.texture(), ModelTextureColorSpace::Srgb)?;
            }
        }
    }
    Ok(spaces
        .into_iter()
        .map(|space| space.unwrap_or(ModelTextureColorSpace::Srgb))
        .collect())
}

fn record_color_space(
    spaces: &mut [Option<ModelTextureColorSpace>],
    index: ModelTextureIndex,
    requested: ModelTextureColorSpace,
) -> Result<(), ModelTextureLoadError> {
    let entry = &mut spaces[index.get()];
    if let Some(existing) = *entry
        && existing != requested
    {
        return Err(ModelTextureLoadError::ConflictingColorSpace {
            index,
            first: existing,
            second: requested,
        });
    }
    *entry = Some(requested);
    Ok(())
}

fn prepared_texture_error(
    index: ModelTextureIndex,
    source: &ResolvedModelTextureSource,
    error: TextureUploadError,
) -> ModelTextureLoadError {
    match source {
        ResolvedModelTextureSource::Path(path) => ModelTextureLoadError::UploadTexture {
            index,
            path: path.clone(),
            source: error,
        },
        ResolvedModelTextureSource::Embedded { mime_type, .. } => {
            ModelTextureLoadError::UploadEmbeddedTexture {
                index,
                mime_type: mime_type.clone(),
                source: error,
            }
        }
        ResolvedModelTextureSource::DecodedRgba8 {
            width,
            height,
            bytes,
        } => ModelTextureLoadError::UploadDecodedRgba8 {
            index,
            width: *width,
            height: *height,
            bytes: *bytes,
            source: error,
        },
    }
}

fn decoded_rgba8_metadata(
    index: ModelTextureIndex,
    width: u32,
    height: u32,
    pixels: &[u8],
    color_space: ModelTextureColorSpace,
) -> Result<Texture, ModelTextureLoadError> {
    let size = TextureSize::new(width, height).map_err(|_| {
        ModelTextureLoadError::InvalidDecodedRgba8 {
            index,
            width,
            height,
            bytes: pixels.len(),
        }
    })?;
    let expected = usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|pixels| pixels.checked_mul(4));
    if expected != Some(pixels.len()) {
        return Err(ModelTextureLoadError::InvalidDecodedRgba8 {
            index,
            width,
            height,
            bytes: pixels.len(),
        });
    }
    Ok(Texture::new(size).with_color_space(color_space.into_texture_color_space()))
}

fn rollback(
    handles: &[TextureHandle],
    textures: &mut Assets<Texture>,
    gpu_textures: &mut TextureCache,
) {
    for &handle in handles {
        let _ = gpu_textures.remove(handle);
        let _ = textures.remove(handle);
    }
}

/// Resolver construction failed before any texture was loaded.
#[derive(Debug)]
#[non_exhaustive]
pub enum ModelTextureLoaderInitError {
    /// The requested root could not be canonicalized.
    ResolveAssetRoot {
        /// Caller-supplied path.
        path: PathBuf,
        /// Filesystem failure.
        source: std::io::Error,
    },
    /// The canonical root exists but is not a directory.
    AssetRootNotDirectory {
        /// Canonical filesystem path.
        path: PathBuf,
    },
}

impl fmt::Display for ModelTextureLoaderInitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ResolveAssetRoot { path, .. } => {
                write!(
                    formatter,
                    "could not resolve model texture root {}",
                    path.display()
                )
            }
            Self::AssetRootNotDirectory { path } => {
                write!(
                    formatter,
                    "model texture root is not a directory: {}",
                    path.display()
                )
            }
        }
    }
}

impl Error for ModelTextureLoaderInitError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ResolveAssetRoot { source, .. } => Some(source),
            Self::AssetRootNotDirectory { .. } => None,
        }
    }
}

/// A model texture URI could not be safely decoded or uploaded.
#[derive(Debug)]
#[non_exhaustive]
pub enum ModelTextureLoadError {
    /// The source model uses one descriptor as both colour and linear data.
    ConflictingColorSpace {
        /// Model-local texture slot.
        index: ModelTextureIndex,
        /// First semantic colour space seen.
        first: ModelTextureColorSpace,
        /// Incompatible later semantic colour space.
        second: ModelTextureColorSpace,
    },
    /// Network and data URIs are outside this filesystem-only resolver.
    UnsupportedUri {
        /// Model-local texture slot.
        index: ModelTextureIndex,
        /// Source URI.
        uri: String,
    },
    /// An absolute path, volume prefix or escaped canonical target was rejected.
    UnsafeTexturePath {
        /// Model-local texture slot.
        index: ModelTextureIndex,
        /// Source URI.
        uri: String,
    },
    /// A relative URI could not be canonicalized under the asset root.
    ResolveTexturePath {
        /// Model-local texture slot.
        index: ModelTextureIndex,
        /// Source URI.
        uri: String,
        /// Candidate path before canonicalization.
        path: PathBuf,
        /// Filesystem failure.
        source: std::io::Error,
    },
    /// An image file failed the configured decoding policy.
    DecodeImage {
        /// Model-local texture slot.
        index: ModelTextureIndex,
        /// Canonical source file path.
        path: PathBuf,
        /// Image decoder failure.
        source: ImageImportError,
    },
    /// An embedded source image failed the configured decoding policy.
    DecodeEmbeddedImage {
        /// Model-local texture slot.
        index: ModelTextureIndex,
        /// Declared source MIME type.
        mime_type: String,
        /// Decoder failure.
        source: ImageImportError,
    },
    /// Importer-decoded pixels had invalid dimensions or byte length.
    InvalidDecodedRgba8 {
        /// Model-local texture slot.
        index: ModelTextureIndex,
        /// Declared pixel width.
        width: u32,
        /// Declared pixel height.
        height: u32,
        /// Supplied byte count.
        bytes: usize,
    },
    /// A decoded image could not become a GPU sampled texture.
    UploadTexture {
        /// Model-local texture slot.
        index: ModelTextureIndex,
        /// Canonical source file path.
        path: PathBuf,
        /// GPU upload validation failure.
        source: TextureUploadError,
    },
    /// A decoded embedded image could not become a GPU sampled texture.
    UploadEmbeddedTexture {
        /// Model-local texture slot.
        index: ModelTextureIndex,
        /// Declared source MIME type.
        mime_type: String,
        /// GPU upload validation failure.
        source: TextureUploadError,
    },
    /// Importer-decoded RGBA8 pixels could not become a GPU sampled texture.
    UploadDecodedRgba8 {
        /// Model-local texture slot.
        index: ModelTextureIndex,
        /// Pixel width.
        width: u32,
        /// Pixel height.
        height: u32,
        /// Pixel byte count.
        bytes: usize,
        /// GPU upload validation failure.
        source: TextureUploadError,
    },
}

impl fmt::Display for ModelTextureLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConflictingColorSpace {
                index,
                first,
                second,
            } => write!(
                formatter,
                "model texture {} is used as both {first} and {second} data",
                index.get()
            ),
            Self::UnsupportedUri { index, uri } => write!(
                formatter,
                "model texture {} uses unsupported URI {uri:?}",
                index.get()
            ),
            Self::UnsafeTexturePath { index, uri } => write!(
                formatter,
                "model texture {} escapes the asset root: {uri:?}",
                index.get()
            ),
            Self::ResolveTexturePath { index, path, .. } => write!(
                formatter,
                "could not resolve model texture {} at {}",
                index.get(),
                path.display()
            ),
            Self::DecodeImage { index, path, .. } => write!(
                formatter,
                "could not decode model texture {} at {}",
                index.get(),
                path.display()
            ),
            Self::DecodeEmbeddedImage {
                index, mime_type, ..
            } => write!(
                formatter,
                "could not decode embedded model texture {} with MIME type {mime_type}",
                index.get()
            ),
            Self::InvalidDecodedRgba8 {
                index,
                width,
                height,
                bytes,
            } => write!(
                formatter,
                "model texture {} has invalid decoded RGBA8 payload: {width}x{height}, {bytes} bytes",
                index.get()
            ),
            Self::UploadTexture { index, path, .. } => write!(
                formatter,
                "could not upload model texture {} at {}",
                index.get(),
                path.display()
            ),
            Self::UploadEmbeddedTexture {
                index, mime_type, ..
            } => write!(
                formatter,
                "could not upload embedded model texture {} with MIME type {mime_type}",
                index.get()
            ),
            Self::UploadDecodedRgba8 {
                index,
                width,
                height,
                bytes,
                ..
            } => write!(
                formatter,
                "could not upload decoded model texture {} ({width}x{height}, {bytes} bytes)",
                index.get()
            ),
        }
    }
}

impl Error for ModelTextureLoadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ResolveTexturePath { source, .. } => Some(source),
            Self::DecodeImage { source, .. } | Self::DecodeEmbeddedImage { source, .. } => {
                Some(source)
            }
            Self::UploadTexture { source, .. }
            | Self::UploadEmbeddedTexture { source, .. }
            | Self::UploadDecodedRgba8 { source, .. } => Some(source),
            Self::ConflictingColorSpace { .. }
            | Self::UnsupportedUri { .. }
            | Self::UnsafeTexturePath { .. }
            | Self::InvalidDecodedRgba8 { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, path::Path, path::PathBuf};

    use yuyib_2d::{Texture, TextureSize};
    use yuyib_model::{
        Material, MaterialIndex, Mesh, MeshPrimitive, Model, ModelTexture, ModelTextureAddressMode,
        ModelTextureIndex, ModelTextureMagFilter, ModelTextureMinFilter, ModelTextureSampler,
        NormalTextureBinding, SpecularGlossinessMaterial, TextureBinding,
    };

    use super::{
        ModelTextureColorSpace, ModelTextureLoadError, ModelTextureLoader, PreparedModelTexture,
        PreparedModelTextures, PreparedTextureKey, PreparedTextureUpload,
        ResolvedModelTextureSource, TextureAlphaSummary, TextureMipmapPolicy,
        TextureSamplingPreset, apply_sampling_preset, infer_color_spaces, model_sampler_to_gpu,
    };

    fn prepared_slot(index: usize, source: &str, rgba_bytes: usize) -> PreparedModelTexture {
        let path = PathBuf::from(source);
        let sampler = yuyib_render_texture::TextureSampler::default();
        let pixels = vec![0; rgba_bytes];
        let pixel_count = u32::try_from(rgba_bytes / 4).expect("test pixel count fits u32");
        let metadata =
            Texture::new(TextureSize::new(pixel_count, 1).expect("test texture is non-empty"));
        let gpu_upload = PreparedTextureUpload::rgba8(&metadata, &pixels, sampler)
            .expect("test RGBA data matches metadata");
        PreparedModelTexture {
            index: ModelTextureIndex::new(index),
            source: ResolvedModelTextureSource::Path(path.clone()),
            color_space: ModelTextureColorSpace::Srgb,
            sampler,
            metadata,
            alpha: TextureAlphaSummary::from_rgba8(&pixels),
            gpu_upload,
            key: PreparedTextureKey::Path(path, ModelTextureColorSpace::Srgb, sampler),
        }
    }

    #[test]
    fn alpha_summary_tracks_nearly_opaque_coverage() {
        let summary = TextureAlphaSummary::from_rgba8(&[
            1, 2, 3, 255, 4, 5, 6, 254, 7, 8, 9, 247, 10, 11, 12, 255,
        ]);
        assert_eq!(summary.minimum(), 247);
        assert_eq!(summary.maximum(), 255);
        assert_eq!(summary.pixels_at_least_254(), 3);
        assert_eq!(summary.total_pixels(), 4);
    }

    fn prepared_plan_fixture() -> PreparedModelTextures {
        PreparedModelTextures {
            slots: vec![
                prepared_slot(0, "shared.png", 16),
                prepared_slot(1, "shared.png", 16),
                prepared_slot(2, "other.png", 16),
            ],
            next_slot: 0,
            resolved: Vec::new(),
            unique_handles: Vec::new(),
            resident: HashMap::new(),
        }
    }

    fn model_with_material(material: Material) -> Model {
        let primitive = MeshPrimitive::new(vec![[0.0; 3]; 3], vec![0, 1, 2])
            .expect("test triangle is valid")
            .with_material(MaterialIndex::new(0));
        let mesh = Mesh::new(None, vec![primitive]).expect("one primitive mesh is valid");
        Model::new(
            vec![mesh],
            vec![material],
            vec![ModelTexture::new("texture.png")],
        )
        .expect("model references texture zero")
    }

    #[test]
    fn prepare_skips_unused_external_uri() {
        /// Minimal valid 1×1 PNG (RGBA).
        const TINY_PNG: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78,
            0xDA, 0x63, 0xFC, 0xCF, 0xC0, 0x50, 0x0F, 0x00, 0x04, 0x85, 0x01, 0x80, 0x84, 0xA9,
            0x8C, 0x21, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];
        let primitive = MeshPrimitive::new(vec![[0.0; 3]; 3], vec![0, 1, 2])
            .expect("test triangle is valid")
            .with_material(MaterialIndex::new(0))
            .with_tex_coords_0(vec![[0.0; 2]; 3])
            .expect("uv0 matches positions");
        let mesh = Mesh::new(None, vec![primitive]).expect("one primitive mesh is valid");
        let material = Material::new()
            .with_base_color_texture(TextureBinding::new(ModelTextureIndex::new(0), 0));
        let model = Model::new(
            vec![mesh],
            vec![material],
            vec![
                ModelTexture::embedded("image/png", TINY_PNG.to_vec()).with_label("used"),
                ModelTexture::new("missing_orphan.png").with_label("unused_external"),
            ],
        )
        .expect("model references texture zero");
        let loader = ModelTextureLoader::new(".").expect("workspace root is a directory");
        let prepared = loader
            .prepare(&model)
            .expect("unused missing URI must not block material-referenced prepare");
        assert_eq!(prepared.len(), 1);
        assert_eq!(prepared.remaining(), 1);
    }

    #[test]
    fn material_semantics_select_srgb_or_linear_sampling() {
        let srgb = model_with_material(
            Material::new()
                .with_base_color_texture(TextureBinding::new(ModelTextureIndex::new(0), 0)),
        );
        assert_eq!(
            infer_color_spaces(&srgb).expect("base colour is unambiguous"),
            vec![ModelTextureColorSpace::Srgb]
        );
        let linear = model_with_material(Material::new().with_normal_texture(
            NormalTextureBinding::new(TextureBinding::new(ModelTextureIndex::new(0), 0), 1.0),
        ));
        assert_eq!(
            infer_color_spaces(&linear).expect("normal map is unambiguous"),
            vec![ModelTextureColorSpace::Linear]
        );
    }

    #[test]
    fn mixed_colour_and_normal_use_is_rejected() {
        let material = Material::new()
            .with_base_color_texture(TextureBinding::new(ModelTextureIndex::new(0), 0))
            .with_normal_texture(NormalTextureBinding::new(
                TextureBinding::new(ModelTextureIndex::new(0), 0),
                1.0,
            ));
        let error = infer_color_spaces(&model_with_material(material))
            .expect_err("one image cannot have incompatible sampled formats in one cache slot");
        assert!(matches!(
            error,
            ModelTextureLoadError::ConflictingColorSpace { .. }
        ));
    }

    #[test]
    fn specular_glossiness_textures_are_sampled_as_srgb() {
        let material = Material::new().with_specular_glossiness(
            SpecularGlossinessMaterial::new([1.0; 4], [1.0; 3], 1.0)
                .with_diffuse_texture(TextureBinding::new(ModelTextureIndex::new(0), 0)),
        );
        assert_eq!(
            infer_color_spaces(&model_with_material(material))
                .expect("diffuse workflow texture is sRGB"),
            vec![ModelTextureColorSpace::Srgb]
        );
    }

    #[test]
    fn source_sampler_reaches_gpu_with_repeat_and_filter_settings() {
        let sampler = model_sampler_to_gpu(ModelTextureSampler {
            address_mode_u: ModelTextureAddressMode::Repeat,
            address_mode_v: ModelTextureAddressMode::MirroredRepeat,
            mag_filter: ModelTextureMagFilter::Nearest,
            min_filter: ModelTextureMinFilter::LinearMipmapLinear,
        });
        assert_eq!(sampler.address_mode_u, wgpu::AddressMode::Repeat);
        assert_eq!(sampler.address_mode_v, wgpu::AddressMode::MirrorRepeat);
        assert_eq!(sampler.mag_filter, wgpu::FilterMode::Nearest);
        assert_eq!(sampler.min_filter, wgpu::FilterMode::Linear);
        assert_eq!(sampler.mipmap_filter, wgpu::MipmapFilterMode::Linear);
        assert_eq!(sampler.mipmaps, TextureMipmapPolicy::Generate);
        assert_eq!(sampler.anisotropy_clamp, 1);
    }

    #[test]
    fn trilinear_gltf_sampler_uses_balanced_anisotropy() {
        let sampler = model_sampler_to_gpu(ModelTextureSampler {
            address_mode_u: ModelTextureAddressMode::Repeat,
            address_mode_v: ModelTextureAddressMode::Repeat,
            mag_filter: ModelTextureMagFilter::Linear,
            min_filter: ModelTextureMinFilter::LinearMipmapLinear,
        });
        assert_eq!(sampler.mipmaps, TextureMipmapPolicy::Generate);
        assert_eq!(sampler.anisotropy_clamp, 4);
    }

    #[test]
    fn non_mipmapped_gltf_sampler_does_not_allocate_mips() {
        let sampler = model_sampler_to_gpu(ModelTextureSampler {
            address_mode_u: ModelTextureAddressMode::Repeat,
            address_mode_v: ModelTextureAddressMode::Repeat,
            mag_filter: ModelTextureMagFilter::Linear,
            min_filter: ModelTextureMinFilter::Linear,
        });
        assert_eq!(sampler.mipmaps, TextureMipmapPolicy::Disabled);
        assert_eq!(sampler.anisotropy_clamp, 1);
    }

    #[test]
    fn quality_preset_upgrades_filters_but_retains_imported_address_modes() {
        let imported = model_sampler_to_gpu(ModelTextureSampler {
            address_mode_u: ModelTextureAddressMode::Repeat,
            address_mode_v: ModelTextureAddressMode::MirroredRepeat,
            mag_filter: ModelTextureMagFilter::Nearest,
            min_filter: ModelTextureMinFilter::Nearest,
        });
        let quality = apply_sampling_preset(imported, TextureSamplingPreset::HighQuality);
        assert_eq!(quality.address_mode_u, wgpu::AddressMode::Repeat);
        assert_eq!(quality.address_mode_v, wgpu::AddressMode::MirrorRepeat);
        assert_eq!(quality.mag_filter, wgpu::FilterMode::Linear);
        assert_eq!(quality.mipmaps, TextureMipmapPolicy::Generate);
        assert_eq!(quality.anisotropy_clamp, 16);
    }

    #[test]
    fn texture_upload_plan_counts_unique_bytes_and_free_duplicate_slots() {
        let prepared = prepared_plan_fixture();
        let plan = prepared.plan_upload(3, 28);

        assert_eq!(plan.uploaded_slots, 2);
        assert_eq!(plan.uploaded_unique_bytes, 28);
        assert!(!plan.uploaded_oversized_texture);
    }

    #[test]
    fn texture_upload_plan_allows_one_oversized_unique_texture() {
        let prepared = prepared_plan_fixture();
        let plan = prepared.plan_upload(3, 8);

        assert_eq!(plan.uploaded_slots, 2);
        assert_eq!(plan.uploaded_unique_bytes, 28);
        assert!(plan.uploaded_oversized_texture);
        assert_eq!(prepared.plan_upload(3, 0).uploaded_slots, 0);
    }

    #[test]
    fn absolute_uri_is_rejected_before_the_file_is_opened() {
        let root = ModelTextureLoader::new(".").expect("workspace root is a directory");
        let error = root
            .resolve_uri(ModelTextureIndex::new(0), "C:\\outside.png")
            .expect_err("Windows absolute URI must be rejected");
        assert!(matches!(
            error,
            ModelTextureLoadError::UnsafeTexturePath { .. }
        ));
        assert!(Path::new("C:\\outside.png").components().next().is_some());
    }
}
