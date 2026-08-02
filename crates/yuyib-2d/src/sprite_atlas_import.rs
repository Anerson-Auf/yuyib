//! Typed importer and runtime binding for offline-cooked sprite atlas manifests.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt, mem,
    time::Duration,
};

use serde::Deserialize;
use yuyib_assets::{
    AssetImporter, ImportContext, ImportDependency, ImportDependencyKind, ImportMatch, ImportProbe,
    ImportSource, ImporterDescriptor, ImporterOutput, ImporterRegistrationError, ImporterRegistry,
};

use crate::{
    AnimationFrame, PixelPoint, PlaybackMode, SpriteAnimation, SpriteAnimationError, Texture,
    TextureAlphaMode, TextureColorSpace, TextureHandle, TextureRegion, TextureRegionError,
    TextureSize, TextureSizeError,
};

/// Media type advertised by the offline sprite-atlas manifest importer.
pub const SPRITE_ATLAS_MANIFEST_MEDIA_TYPE: &str = "application/vnd.yuyib.sprite-atlas+json";

const FORMAT_NAME: &str = "yuyib.sprite_atlas";
const FORMAT_VERSION: u32 = 1;

/// Import-specific trust boundary for one offline atlas manifest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SpriteAtlasImportLimits {
    /// Maximum JSON document size accepted directly by this importer.
    pub max_manifest_bytes: usize,
    /// Maximum region records in one atlas.
    pub max_regions: usize,
    /// Maximum named animations in one atlas.
    pub max_animations: usize,
    /// Maximum frames in one animation.
    pub max_frames_per_animation: usize,
    /// Maximum frames summed across all animations.
    pub max_total_frames: usize,
    /// Maximum UTF-8 bytes in a region or animation name.
    pub max_name_bytes: usize,
    /// Maximum UTF-8 bytes in the logical texture dependency URI.
    pub max_dependency_uri_bytes: usize,
    /// Maximum duration of one frame in milliseconds.
    pub max_frame_duration_ms: u64,
}

impl Default for SpriteAtlasImportLimits {
    fn default() -> Self {
        Self {
            max_manifest_bytes: 1024 * 1024,
            max_regions: 4_096,
            max_animations: 256,
            max_frames_per_animation: 1_024,
            max_total_frames: 32_768,
            max_name_bytes: 256,
            max_dependency_uri_bytes: 4_096,
            max_frame_duration_ms: 10 * 60 * 1_000,
        }
    }
}

impl SpriteAtlasImportLimits {
    fn validate(self) -> Result<Self, SpriteAtlasImportLimitsError> {
        for (field, value) in [
            ("max_manifest_bytes", self.max_manifest_bytes),
            ("max_regions", self.max_regions),
            ("max_animations", self.max_animations),
            ("max_frames_per_animation", self.max_frames_per_animation),
            ("max_total_frames", self.max_total_frames),
            ("max_name_bytes", self.max_name_bytes),
            ("max_dependency_uri_bytes", self.max_dependency_uri_bytes),
        ] {
            if value == 0 {
                return Err(SpriteAtlasImportLimitsError::ZeroLimit(field));
            }
        }
        if self.max_frame_duration_ms == 0 {
            return Err(SpriteAtlasImportLimitsError::ZeroLimit(
                "max_frame_duration_ms",
            ));
        }
        Ok(self)
    }
}

/// Invalid [`SpriteAtlasImportLimits`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpriteAtlasImportLimitsError {
    /// Every limit must be positive.
    ZeroLimit(&'static str),
}

impl fmt::Display for SpriteAtlasImportLimitsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroLimit(field) => {
                write!(formatter, "sprite atlas import limit `{field}` is zero")
            }
        }
    }
}

impl Error for SpriteAtlasImportLimitsError {}

/// Built-in importer for a versioned offline sprite-atlas JSON manifest.
///
/// The importer does not read the referenced image. It emits one required
/// logical dependency and a renderer-neutral CPU value. Dependency resolution,
/// image decoding and GPU upload remain explicit host stages.
#[derive(Clone, Copy, Debug)]
pub struct SpriteAtlasImporter {
    limits: SpriteAtlasImportLimits,
}

impl SpriteAtlasImporter {
    /// Creates an importer with explicit format limits.
    ///
    /// # Errors
    ///
    /// Returns an error when any limit is zero.
    pub fn new(limits: SpriteAtlasImportLimits) -> Result<Self, SpriteAtlasImportLimitsError> {
        Ok(Self {
            limits: limits.validate()?,
        })
    }

    /// Returns the active per-manifest limits.
    #[must_use]
    pub const fn limits(self) -> SpriteAtlasImportLimits {
        self.limits
    }

    fn parse(
        self,
        source: ImportSource<'_>,
        context: Option<ImportContext<'_>>,
    ) -> Result<ImporterOutput<ImportedSpriteAtlas>, SpriteAtlasImportError> {
        Self::ensure_running(context)?;
        if source.bytes().len() > self.limits.max_manifest_bytes {
            return Err(SpriteAtlasImportError::LimitExceeded {
                field: "manifest bytes",
                actual: source.bytes().len(),
                maximum: self.limits.max_manifest_bytes,
            });
        }
        let document: ManifestDocument = serde_json::from_slice(source.bytes())
            .map_err(SpriteAtlasImportError::MalformedJson)?;
        Self::ensure_running(context)?;
        self.validate_document(document, context)
    }

    fn validate_document(
        self,
        document: ManifestDocument,
        context: Option<ImportContext<'_>>,
    ) -> Result<ImporterOutput<ImportedSpriteAtlas>, SpriteAtlasImportError> {
        if document.format != FORMAT_NAME {
            return Err(SpriteAtlasImportError::UnsupportedFormat(document.format));
        }
        if document.version != FORMAT_VERSION {
            return Err(SpriteAtlasImportError::UnsupportedVersion(document.version));
        }
        Self::validate_count("regions", document.regions.len(), self.limits.max_regions)?;
        Self::validate_count(
            "animations",
            document.animations.len(),
            self.limits.max_animations,
        )?;
        self.validate_dependency_uri(&document.texture.uri)?;

        let texture_size = TextureSize::new(document.texture.width, document.texture.height)
            .map_err(SpriteAtlasImportError::InvalidTextureSize)?;
        let texture = Texture::new(texture_size)
            .with_alpha_mode(document.texture.alpha.into())
            .with_color_space(document.texture.color_space.into());

        let mut regions = Vec::with_capacity(document.regions.len());
        let mut region_indices = BTreeMap::new();
        for region in document.regions {
            Self::ensure_running(context)?;
            self.validate_name("region", &region.name)?;
            if region_indices.contains_key(region.name.as_str()) {
                return Err(SpriteAtlasImportError::DuplicateRegion(region.name));
            }
            let size = TextureSize::new(region.width, region.height)
                .map_err(SpriteAtlasImportError::InvalidRegionSize)?;
            validate_region_bounds(
                texture_size,
                PixelPoint {
                    x: region.x,
                    y: region.y,
                },
                size,
            )?;
            let index = regions.len();
            region_indices.insert(region.name.clone(), index);
            regions.push(ImportedSpriteRegion {
                name: region.name,
                origin: PixelPoint {
                    x: region.x,
                    y: region.y,
                },
                size,
            });
        }

        let animations = self.validate_animations(document.animations, &region_indices, context)?;

        let cpu_bytes = estimate_cpu_bytes(&document.texture.uri, &regions, &animations);
        let dependency_uri = document.texture.uri;
        let atlas = ImportedSpriteAtlas {
            texture_dependency: dependency_uri.clone(),
            texture,
            regions,
            animations,
        };
        let mut output = ImporterOutput::new(atlas);
        output.dependencies.push(ImportDependency {
            uri: dependency_uri,
            kind: ImportDependencyKind::Required,
        });
        output.cpu_bytes = Some(cpu_bytes);
        Ok(output)
    }

    fn validate_animations(
        self,
        documents: Vec<AnimationDocument>,
        region_indices: &BTreeMap<String, usize>,
        context: Option<ImportContext<'_>>,
    ) -> Result<Vec<ImportedSpriteAnimation>, SpriteAtlasImportError> {
        let mut animations = Vec::with_capacity(documents.len());
        let mut animation_names = BTreeSet::new();
        let mut total_frames = 0_usize;
        for animation in documents {
            Self::ensure_running(context)?;
            self.validate_name("animation", &animation.name)?;
            if !animation_names.insert(animation.name.clone()) {
                return Err(SpriteAtlasImportError::DuplicateAnimation(animation.name));
            }
            if animation.frames.is_empty() {
                return Err(SpriteAtlasImportError::EmptyAnimation(animation.name));
            }
            Self::validate_count(
                "frames per animation",
                animation.frames.len(),
                self.limits.max_frames_per_animation,
            )?;
            total_frames = total_frames.checked_add(animation.frames.len()).ok_or(
                SpriteAtlasImportError::LimitExceeded {
                    field: "total frames",
                    actual: usize::MAX,
                    maximum: self.limits.max_total_frames,
                },
            )?;
            Self::validate_count("total frames", total_frames, self.limits.max_total_frames)?;

            let mut frames = Vec::with_capacity(animation.frames.len());
            for frame in animation.frames {
                Self::ensure_running(context)?;
                let Some(&region_index) = region_indices.get(frame.region.as_str()) else {
                    return Err(SpriteAtlasImportError::UnknownRegion {
                        animation: animation.name,
                        region: frame.region,
                    });
                };
                if frame.duration_ms == 0 {
                    return Err(SpriteAtlasImportError::ZeroFrameDuration {
                        animation: animation.name,
                    });
                }
                if frame.duration_ms > self.limits.max_frame_duration_ms {
                    return Err(SpriteAtlasImportError::FrameDurationTooLong {
                        animation: animation.name,
                        actual_ms: frame.duration_ms,
                        maximum_ms: self.limits.max_frame_duration_ms,
                    });
                }
                frames.push(ImportedSpriteAnimationFrame {
                    region_index,
                    duration: Duration::from_millis(frame.duration_ms),
                });
            }
            animations.push(ImportedSpriteAnimation {
                name: animation.name,
                playback: animation.playback.into(),
                frames,
            });
        }
        Ok(animations)
    }

    fn validate_count(
        field: &'static str,
        actual: usize,
        maximum: usize,
    ) -> Result<(), SpriteAtlasImportError> {
        if actual > maximum {
            return Err(SpriteAtlasImportError::LimitExceeded {
                field,
                actual,
                maximum,
            });
        }
        Ok(())
    }

    fn validate_name(self, kind: &'static str, name: &str) -> Result<(), SpriteAtlasImportError> {
        if name.is_empty()
            || name.len() > self.limits.max_name_bytes
            || name.chars().any(char::is_control)
        {
            return Err(SpriteAtlasImportError::InvalidName {
                kind,
                maximum: self.limits.max_name_bytes,
                name: name.to_owned(),
            });
        }
        Ok(())
    }

    fn validate_dependency_uri(self, uri: &str) -> Result<(), SpriteAtlasImportError> {
        if uri.is_empty()
            || uri.len() > self.limits.max_dependency_uri_bytes
            || uri.chars().any(char::is_control)
        {
            return Err(SpriteAtlasImportError::InvalidDependencyUri {
                maximum: self.limits.max_dependency_uri_bytes,
            });
        }
        Ok(())
    }

    fn ensure_running(context: Option<ImportContext<'_>>) -> Result<(), SpriteAtlasImportError> {
        if context.is_some_and(ImportContext::is_cancelled) {
            return Err(SpriteAtlasImportError::Cancelled);
        }
        Ok(())
    }
}

impl Default for SpriteAtlasImporter {
    fn default() -> Self {
        Self::new(SpriteAtlasImportLimits::default())
            .expect("default sprite atlas import limits are valid")
    }
}

impl AssetImporter<ImportedSpriteAtlas> for SpriteAtlasImporter {
    type Error = SpriteAtlasImportError;

    fn descriptor(&self) -> ImporterDescriptor {
        ImporterDescriptor::new("yuyib.sprite_atlas", env!("CARGO_PKG_VERSION"))
            .with_extension("ysprite")
            .with_media_type(SPRITE_ATLAS_MANIFEST_MEDIA_TYPE)
    }

    fn probe(&self, probe: ImportProbe<'_>) -> ImportMatch {
        if probe.media_type == Some(SPRITE_ATLAS_MANIFEST_MEDIA_TYPE)
            || (probe.extension == Some("ysprite") && contains_format_marker(probe.prefix))
        {
            ImportMatch::Exact
        } else if probe.extension == Some("ysprite") {
            ImportMatch::Preferred
        } else if contains_format_marker(probe.prefix) {
            ImportMatch::Possible
        } else {
            ImportMatch::Unsupported
        }
    }

    fn import(
        &self,
        source: ImportSource<'_>,
    ) -> Result<ImporterOutput<ImportedSpriteAtlas>, Self::Error> {
        self.parse(source, None)
    }

    fn import_with_context(
        &self,
        source: ImportSource<'_>,
        context: ImportContext<'_>,
    ) -> Result<ImporterOutput<ImportedSpriteAtlas>, Self::Error> {
        self.parse(source, Some(context))
    }
}

/// Registers the built-in atlas importer using conservative default limits.
///
/// Use `registry.register(SpriteAtlasImporter::new(custom_limits)?)` when a
/// project needs a different explicit trust boundary.
///
/// # Errors
///
/// Returns the registry's structured registration error.
pub fn register_sprite_atlas_importer(
    registry: &mut ImporterRegistry<ImportedSpriteAtlas>,
) -> Result<(), ImporterRegistrationError> {
    registry.register(SpriteAtlasImporter::default())
}

/// Renderer-neutral result of importing an offline sprite atlas manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportedSpriteAtlas {
    texture_dependency: String,
    texture: Texture,
    regions: Vec<ImportedSpriteRegion>,
    animations: Vec<ImportedSpriteAnimation>,
}

impl ImportedSpriteAtlas {
    /// Logical URI that the host must resolve under its own path policy.
    #[must_use]
    pub fn texture_dependency(&self) -> &str {
        &self.texture_dependency
    }

    /// Validated texture metadata available before image residency.
    #[must_use]
    pub const fn texture(&self) -> &Texture {
        &self.texture
    }

    /// Named, non-overflowing regions in manifest order.
    #[must_use]
    pub fn regions(&self) -> &[ImportedSpriteRegion] {
        &self.regions
    }

    /// Validated animation records in manifest order.
    #[must_use]
    pub fn animations(&self) -> &[ImportedSpriteAnimation] {
        &self.animations
    }

    /// Binds already-resolved texture metadata to runtime animation values.
    ///
    /// This step performs no IO or GPU work. The handle may still point to a
    /// loading asset and renderer code can use its normal placeholder policy.
    ///
    /// # Errors
    ///
    /// Returns a defensive error if imported data can no longer produce the
    /// foundational validated region/animation types.
    pub fn bind_texture(
        self,
        texture_handle: TextureHandle,
    ) -> Result<RuntimeSpriteAtlas, SpriteAtlasBindError> {
        let texture_size = self.texture.size();
        let mut runtime_regions = Vec::with_capacity(self.regions.len());
        for region in self.regions {
            let value =
                TextureRegion::new(texture_handle, texture_size, region.origin, region.size)
                    .map_err(SpriteAtlasBindError::Region)?;
            runtime_regions.push((region.name, value));
        }

        let mut runtime_animations = Vec::with_capacity(self.animations.len());
        for animation in self.animations {
            let frames = animation
                .frames
                .iter()
                .map(|frame| {
                    let region = runtime_regions
                        .get(frame.region_index)
                        .ok_or(SpriteAtlasBindError::MissingRegion(frame.region_index))?
                        .1;
                    AnimationFrame::new(region, frame.duration).map_err(SpriteAtlasBindError::from)
                })
                .collect::<Result<Vec<_>, _>>()?;
            runtime_animations.push((
                animation.name,
                SpriteAnimation::new(frames, animation.playback)?,
            ));
        }
        Ok(RuntimeSpriteAtlas {
            texture_dependency: self.texture_dependency,
            texture: self.texture,
            texture_handle,
            regions: runtime_regions,
            animations: runtime_animations,
        })
    }
}

/// One named region in an imported atlas.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportedSpriteRegion {
    name: String,
    origin: PixelPoint,
    size: TextureSize,
}

impl ImportedSpriteRegion {
    /// Stable content name used by animation frames.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Top-left pixel coordinate.
    #[must_use]
    pub const fn origin(&self) -> PixelPoint {
        self.origin
    }

    /// Non-zero pixel size.
    #[must_use]
    pub const fn size(&self) -> TextureSize {
        self.size
    }
}

/// One named animation in an imported atlas.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportedSpriteAnimation {
    name: String,
    playback: PlaybackMode,
    frames: Vec<ImportedSpriteAnimationFrame>,
}

impl ImportedSpriteAnimation {
    /// Stable animation name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Playback policy retained from the manifest.
    #[must_use]
    pub const fn playback(&self) -> PlaybackMode {
        self.playback
    }

    /// Validated frames in playback order.
    #[must_use]
    pub fn frames(&self) -> &[ImportedSpriteAnimationFrame] {
        &self.frames
    }
}

/// One validated frame referencing an imported region by index.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImportedSpriteAnimationFrame {
    region_index: usize,
    duration: Duration,
}

impl ImportedSpriteAnimationFrame {
    /// Index into [`ImportedSpriteAtlas::regions`].
    #[must_use]
    pub const fn region_index(self) -> usize {
        self.region_index
    }

    /// Non-zero bounded display duration.
    #[must_use]
    pub const fn duration(self) -> Duration {
        self.duration
    }
}

/// Runtime atlas metadata bound to a stable texture handle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeSpriteAtlas {
    texture_dependency: String,
    texture: Texture,
    texture_handle: TextureHandle,
    regions: Vec<(String, TextureRegion)>,
    animations: Vec<(String, SpriteAnimation)>,
}

impl RuntimeSpriteAtlas {
    /// Logical source retained for diagnostics and dependency lookup.
    #[must_use]
    pub fn texture_dependency(&self) -> &str {
        &self.texture_dependency
    }

    /// Stable texture handle used by every region.
    #[must_use]
    pub const fn texture_handle(&self) -> TextureHandle {
        self.texture_handle
    }

    /// Texture metadata retained while residency changes asynchronously.
    #[must_use]
    pub const fn texture(&self) -> &Texture {
        &self.texture
    }

    /// Finds a named texture region.
    #[must_use]
    pub fn region(&self, name: &str) -> Option<TextureRegion> {
        self.regions
            .iter()
            .find_map(|(candidate, region)| (candidate == name).then_some(*region))
    }

    /// Finds a named immutable animation.
    #[must_use]
    pub fn animation(&self, name: &str) -> Option<&SpriteAnimation> {
        self.animations
            .iter()
            .find_map(|(candidate, animation)| (candidate == name).then_some(animation))
    }

    /// Iterates named regions in deterministic manifest order.
    #[must_use]
    pub fn regions(&self) -> impl ExactSizeIterator<Item = (&str, TextureRegion)> {
        self.regions
            .iter()
            .map(|(name, region)| (name.as_str(), *region))
    }

    /// Iterates named animations in deterministic manifest order.
    #[must_use]
    pub fn animations(&self) -> impl ExactSizeIterator<Item = (&str, &SpriteAnimation)> {
        self.animations
            .iter()
            .map(|(name, animation)| (name.as_str(), animation))
    }
}

/// Atlas texture binding failed after a successful import.
#[derive(Debug)]
pub enum SpriteAtlasBindError {
    /// An animation referenced a region absent from the bound atlas.
    MissingRegion(usize),
    /// A region no longer satisfies the base texture-region contract.
    Region(TextureRegionError),
    /// An animation no longer satisfies the base animation contract.
    Animation(SpriteAnimationError),
}

impl fmt::Display for SpriteAtlasBindError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingRegion(index) => {
                write!(formatter, "cannot bind missing atlas region index {index}")
            }
            Self::Region(error) => write!(formatter, "cannot bind atlas region: {error}"),
            Self::Animation(error) => write!(formatter, "cannot bind atlas animation: {error}"),
        }
    }
}

impl Error for SpriteAtlasBindError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::MissingRegion(_) => None,
            Self::Region(error) => Some(error),
            Self::Animation(error) => Some(error),
        }
    }
}

impl From<SpriteAnimationError> for SpriteAtlasBindError {
    fn from(value: SpriteAnimationError) -> Self {
        Self::Animation(value)
    }
}

/// Structured failure while importing an offline atlas manifest.
#[derive(Debug)]
pub enum SpriteAtlasImportError {
    /// The host requested cooperative cancellation.
    Cancelled,
    /// A manifest-local trust-boundary limit was exceeded.
    LimitExceeded {
        /// Stable limit name.
        field: &'static str,
        /// Observed value.
        actual: usize,
        /// Configured maximum.
        maximum: usize,
    },
    /// The bounded document was not valid JSON/schema data.
    MalformedJson(serde_json::Error),
    /// The format discriminator did not identify a Yuyib atlas.
    UnsupportedFormat(String),
    /// The manifest version is not supported by this importer.
    UnsupportedVersion(u32),
    /// Texture dimensions were zero.
    InvalidTextureSize(TextureSizeError),
    /// Region dimensions were zero.
    InvalidRegionSize(TextureSizeError),
    /// A region exceeded the declared texture bounds.
    RegionOutOfBounds,
    /// A name was empty, oversized or contained control characters.
    InvalidName {
        /// Name category.
        kind: &'static str,
        /// Configured byte limit.
        maximum: usize,
        /// Invalid value retained for diagnostics.
        name: String,
    },
    /// Two regions used the same name.
    DuplicateRegion(String),
    /// Two animations used the same name.
    DuplicateAnimation(String),
    /// An animation contained no frames.
    EmptyAnimation(String),
    /// An animation referenced an unknown region.
    UnknownRegion {
        /// Animation containing the invalid reference.
        animation: String,
        /// Missing region name.
        region: String,
    },
    /// A frame duration was zero.
    ZeroFrameDuration {
        /// Animation containing the frame.
        animation: String,
    },
    /// A frame duration exceeded the importer limit.
    FrameDurationTooLong {
        /// Animation containing the frame.
        animation: String,
        /// Observed duration.
        actual_ms: u64,
        /// Configured maximum.
        maximum_ms: u64,
    },
    /// Texture dependency URI was empty, oversized or contained controls.
    InvalidDependencyUri {
        /// Configured byte limit.
        maximum: usize,
    },
}

impl fmt::Display for SpriteAtlasImportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("sprite atlas import was cancelled"),
            Self::LimitExceeded {
                field,
                actual,
                maximum,
            } => write!(
                formatter,
                "sprite atlas {field} is {actual}; maximum is {maximum}"
            ),
            Self::MalformedJson(error) => write!(formatter, "invalid sprite atlas JSON: {error}"),
            Self::UnsupportedFormat(format) => {
                write!(formatter, "unsupported sprite atlas format `{format}`")
            }
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported sprite atlas version {version}")
            }
            Self::InvalidTextureSize(error) => {
                write!(formatter, "invalid sprite atlas texture size: {error}")
            }
            Self::InvalidRegionSize(error) => {
                write!(formatter, "invalid sprite atlas region size: {error}")
            }
            Self::RegionOutOfBounds => {
                formatter.write_str("sprite atlas region exceeds the declared texture")
            }
            Self::InvalidName {
                kind,
                maximum,
                name,
            } => write!(
                formatter,
                "invalid {kind} name `{name}`; expected non-empty control-free UTF-8 up to {maximum} bytes"
            ),
            Self::DuplicateRegion(name) => {
                write!(formatter, "duplicate sprite atlas region `{name}`")
            }
            Self::DuplicateAnimation(name) => {
                write!(formatter, "duplicate sprite atlas animation `{name}`")
            }
            Self::EmptyAnimation(name) => {
                write!(formatter, "sprite atlas animation `{name}` has no frames")
            }
            Self::UnknownRegion { animation, region } => write!(
                formatter,
                "sprite atlas animation `{animation}` references unknown region `{region}`"
            ),
            Self::ZeroFrameDuration { animation } => write!(
                formatter,
                "sprite atlas animation `{animation}` has a zero-duration frame"
            ),
            Self::FrameDurationTooLong {
                animation,
                actual_ms,
                maximum_ms,
            } => write!(
                formatter,
                "sprite atlas animation `{animation}` frame is {actual_ms} ms; maximum is {maximum_ms} ms"
            ),
            Self::InvalidDependencyUri { maximum } => write!(
                formatter,
                "sprite atlas texture URI must be non-empty control-free UTF-8 up to {maximum} bytes"
            ),
        }
    }
}

impl Error for SpriteAtlasImportError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::MalformedJson(error) => Some(error),
            Self::InvalidTextureSize(error) | Self::InvalidRegionSize(error) => Some(error),
            Self::Cancelled
            | Self::LimitExceeded { .. }
            | Self::UnsupportedFormat(_)
            | Self::UnsupportedVersion(_)
            | Self::RegionOutOfBounds
            | Self::InvalidName { .. }
            | Self::DuplicateRegion(_)
            | Self::DuplicateAnimation(_)
            | Self::EmptyAnimation(_)
            | Self::UnknownRegion { .. }
            | Self::ZeroFrameDuration { .. }
            | Self::FrameDurationTooLong { .. }
            | Self::InvalidDependencyUri { .. } => None,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestDocument {
    format: String,
    version: u32,
    texture: TextureDocument,
    regions: Vec<RegionDocument>,
    #[serde(default)]
    animations: Vec<AnimationDocument>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TextureDocument {
    uri: String,
    width: u32,
    height: u32,
    #[serde(default)]
    alpha: AlphaDocument,
    #[serde(default)]
    color_space: ColorSpaceDocument,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AlphaDocument {
    #[default]
    Straight,
    Premultiplied,
    Opaque,
}

impl From<AlphaDocument> for TextureAlphaMode {
    fn from(value: AlphaDocument) -> Self {
        match value {
            AlphaDocument::Straight => Self::Straight,
            AlphaDocument::Premultiplied => Self::Premultiplied,
            AlphaDocument::Opaque => Self::Opaque,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ColorSpaceDocument {
    #[default]
    Srgb,
    Linear,
}

impl From<ColorSpaceDocument> for TextureColorSpace {
    fn from(value: ColorSpaceDocument) -> Self {
        match value {
            ColorSpaceDocument::Srgb => Self::Srgb,
            ColorSpaceDocument::Linear => Self::Linear,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegionDocument {
    name: String,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AnimationDocument {
    name: String,
    #[serde(default)]
    playback: PlaybackDocument,
    frames: Vec<FrameDocument>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PlaybackDocument {
    #[default]
    Loop,
    Once,
    PingPong,
}

impl From<PlaybackDocument> for PlaybackMode {
    fn from(value: PlaybackDocument) -> Self {
        match value {
            PlaybackDocument::Loop => Self::Loop,
            PlaybackDocument::Once => Self::Once,
            PlaybackDocument::PingPong => Self::PingPong,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FrameDocument {
    region: String,
    duration_ms: u64,
}

fn contains_format_marker(prefix: &[u8]) -> bool {
    prefix
        .windows(FORMAT_NAME.len())
        .any(|window| window == FORMAT_NAME.as_bytes())
}

fn validate_region_bounds(
    texture_size: TextureSize,
    origin: PixelPoint,
    size: TextureSize,
) -> Result<(), SpriteAtlasImportError> {
    let right = origin.x.checked_add(size.width());
    let bottom = origin.y.checked_add(size.height());
    if right.is_none_or(|value| value > texture_size.width())
        || bottom.is_none_or(|value| value > texture_size.height())
    {
        return Err(SpriteAtlasImportError::RegionOutOfBounds);
    }
    Ok(())
}

fn estimate_cpu_bytes(
    dependency: &str,
    regions: &[ImportedSpriteRegion],
    animations: &[ImportedSpriteAnimation],
) -> u64 {
    let fixed = mem::size_of::<ImportedSpriteAtlas>()
        .saturating_add(
            regions
                .len()
                .saturating_mul(mem::size_of::<ImportedSpriteRegion>()),
        )
        .saturating_add(
            animations
                .len()
                .saturating_mul(mem::size_of::<ImportedSpriteAnimation>()),
        );
    let dynamic = dependency
        .len()
        .saturating_add(regions.iter().map(|region| region.name.len()).sum())
        .saturating_add(
            animations
                .iter()
                .map(|animation| {
                    animation.name.len().saturating_add(
                        animation
                            .frames
                            .len()
                            .saturating_mul(mem::size_of::<ImportedSpriteAnimationFrame>()),
                    )
                })
                .sum(),
        );
    u64::try_from(fixed.saturating_add(dynamic)).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::{
        FORMAT_NAME, ImportedSpriteAtlas, SpriteAtlasImportError, SpriteAtlasImportLimits,
        SpriteAtlasImporter, register_sprite_atlas_importer,
    };
    use crate::{PlaybackMode, Texture};
    use yuyib_assets::{Assets, ImportCancellation, ImportError, ImportSource, ImporterRegistry};

    const MANIFEST: &[u8] = br#"{
        "format":"yuyib.sprite_atlas",
        "version":1,
        "texture":{"uri":"sprites/hero.png","width":32,"height":16},
        "regions":[
            {"name":"walk_0","x":0,"y":0,"width":16,"height":16},
            {"name":"walk_1","x":16,"y":0,"width":16,"height":16}
        ],
        "animations":[{
            "name":"walk","playback":"ping_pong",
            "frames":[
                {"region":"walk_0","duration_ms":80},
                {"region":"walk_1","duration_ms":120}
            ]
        }]
    }"#;

    fn registry() -> ImporterRegistry<ImportedSpriteAtlas> {
        let mut registry = ImporterRegistry::default();
        register_sprite_atlas_importer(&mut registry).expect("register atlas importer");
        registry
    }

    #[test]
    fn imports_dependency_and_binds_foundational_animation_types() {
        let result = registry()
            .import(ImportSource::new("hero.ysprite", MANIFEST))
            .expect("import manifest");
        assert_eq!(result.dependencies[0].uri, "sprites/hero.png");
        assert_eq!(result.asset.regions().len(), 2);

        let mut textures = Assets::<Texture>::new();
        let texture_handle = textures.insert(result.asset.texture().clone());
        let runtime = result
            .asset
            .bind_texture(texture_handle)
            .expect("bind texture handle");
        let animation = runtime.animation("walk").expect("walk animation");
        assert_eq!(animation.playback(), PlaybackMode::PingPong);
        assert_eq!(animation.frames().len(), 2);
        assert_eq!(
            runtime.region("walk_1").expect("second region").origin().x,
            16
        );
    }

    #[test]
    fn rejects_unknown_regions_and_out_of_bounds_regions() {
        let unknown = MANIFEST
            .windows("walk_1".len())
            .position(|window| window == b"walk_1")
            .expect("marker");
        let mut bytes = MANIFEST.to_vec();
        bytes[unknown..unknown + "walk_1".len()].copy_from_slice(b"missin");
        let error = registry()
            .import(ImportSource::new("hero.ysprite", &bytes))
            .expect_err("unknown frame region must fail");
        assert!(matches!(error, ImportError::ImporterFailed { .. }));

        let out_of_bounds = MANIFEST
            .iter()
            .position(|byte| *byte == b'3')
            .expect("texture width");
        let mut bytes = MANIFEST.to_vec();
        bytes[out_of_bounds] = b'1';
        let error = registry()
            .import(ImportSource::new("hero.ysprite", &bytes))
            .expect_err("out-of-bounds region must fail");
        assert!(matches!(error, ImportError::ImporterFailed { .. }));
    }

    #[test]
    fn enforces_counts_and_cooperative_cancellation() {
        let importer = SpriteAtlasImporter::new(SpriteAtlasImportLimits {
            max_regions: 1,
            ..SpriteAtlasImportLimits::default()
        })
        .expect("valid limits");
        let mut limited = ImporterRegistry::default();
        limited
            .register(importer)
            .expect("register limited importer");
        let error = limited
            .import(ImportSource::new("hero.ysprite", MANIFEST))
            .expect_err("region count must be bounded");
        let ImportError::ImporterFailed { source, .. } = error else {
            panic!("unexpected registry failure");
        };
        assert!(matches!(
            source.downcast_ref::<SpriteAtlasImportError>(),
            Some(SpriteAtlasImportError::LimitExceeded {
                field: "regions",
                ..
            })
        ));

        let cancellation = ImportCancellation::new();
        cancellation.cancel();
        assert!(matches!(
            registry().import_with_cancellation(
                ImportSource::new("hero.ysprite", MANIFEST),
                &cancellation
            ),
            Err(ImportError::Cancelled)
        ));
    }

    #[test]
    fn format_marker_matches_document_contract() {
        assert_eq!(FORMAT_NAME, "yuyib.sprite_atlas");
    }
}
