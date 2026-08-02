//! Foundational 2D texture, sprite-sheet and deterministic sprite-animation types.
//!
//! Contains no file loading or GPU resources. Texture
//! bytes and uploads belong to the asset and renderer layers; this crate models
//! their 2D metadata and safe pixel regions.

#![forbid(unsafe_code)]

mod sprite_atlas_import;

pub use sprite_atlas_import::{
    ImportedSpriteAnimation, ImportedSpriteAnimationFrame, ImportedSpriteAtlas,
    ImportedSpriteRegion, RuntimeSpriteAtlas, SPRITE_ATLAS_MANIFEST_MEDIA_TYPE,
    SpriteAtlasBindError, SpriteAtlasImportError, SpriteAtlasImportLimits,
    SpriteAtlasImportLimitsError, SpriteAtlasImporter, register_sprite_atlas_importer,
};

use std::{error::Error, fmt, num::NonZeroU32, time::Duration};

use yuyib_assets::AssetId;

/// A typed reference to texture metadata stored in [`yuyib_assets::Assets`].
pub type TextureHandle = AssetId<Texture>;

/// Metadata for a texture asset.
///
/// The asset pipeline owns the encoded image and eventual GPU texture. Keeping
/// this metadata small lets sprites be created before the texture is resident.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Texture {
    size: TextureSize,
    alpha_mode: TextureAlphaMode,
    color_space: TextureColorSpace,
}

impl Texture {
    /// Creates texture metadata with straight alpha and sRGB colour data.
    #[must_use]
    pub const fn new(size: TextureSize) -> Self {
        Self {
            size,
            alpha_mode: TextureAlphaMode::Straight,
            color_space: TextureColorSpace::Srgb,
        }
    }

    /// Sets the alpha representation expected by the source texture.
    #[must_use]
    pub const fn with_alpha_mode(mut self, alpha_mode: TextureAlphaMode) -> Self {
        self.alpha_mode = alpha_mode;
        self
    }

    /// Sets the colour space expected by the source texture.
    #[must_use]
    pub const fn with_color_space(mut self, color_space: TextureColorSpace) -> Self {
        self.color_space = color_space;
        self
    }

    /// Returns the texture dimensions in physical pixels.
    #[must_use]
    pub const fn size(&self) -> TextureSize {
        self.size
    }

    /// Returns the source alpha representation.
    #[must_use]
    pub const fn alpha_mode(&self) -> TextureAlphaMode {
        self.alpha_mode
    }

    /// Returns the source texture colour space.
    #[must_use]
    pub const fn color_space(&self) -> TextureColorSpace {
        self.color_space
    }
}

/// The alpha representation of a texture's colour channels.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TextureAlphaMode {
    /// RGB channels are not pre-multiplied by alpha.
    #[default]
    Straight,
    /// RGB channels have already been multiplied by alpha.
    Premultiplied,
    /// The texture has no meaningful alpha channel.
    Opaque,
}

/// The transfer function used by texture colour channels.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TextureColorSpace {
    /// Display-oriented sRGB data, appropriate for albedo and UI textures.
    #[default]
    Srgb,
    /// Linear data, appropriate for masks and data textures.
    Linear,
}

/// A non-zero width and height in physical pixels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextureSize {
    width: NonZeroU32,
    height: NonZeroU32,
}

impl TextureSize {
    /// Creates a non-empty pixel size.
    ///
    /// # Errors
    ///
    /// Returns [`TextureSizeError`] when either dimension is zero.
    pub fn new(width: u32, height: u32) -> Result<Self, TextureSizeError> {
        let width = NonZeroU32::new(width).ok_or(TextureSizeError::ZeroWidth)?;
        let height = NonZeroU32::new(height).ok_or(TextureSizeError::ZeroHeight)?;
        Ok(Self { width, height })
    }

    /// Returns the width in physical pixels.
    #[must_use]
    pub const fn width(self) -> u32 {
        self.width.get()
    }

    /// Returns the height in physical pixels.
    #[must_use]
    pub const fn height(self) -> u32 {
        self.height.get()
    }
}

/// An invalid [`TextureSize`] construction request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextureSizeError {
    /// Width was zero.
    ZeroWidth,
    /// Height was zero.
    ZeroHeight,
}

impl fmt::Display for TextureSizeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroWidth => formatter.write_str("texture width must be non-zero"),
            Self::ZeroHeight => formatter.write_str("texture height must be non-zero"),
        }
    }
}

impl Error for TextureSizeError {}

/// A pixel coordinate measured from the top-left of a texture.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PixelPoint {
    /// Horizontal pixel coordinate.
    pub x: u32,
    /// Vertical pixel coordinate.
    pub y: u32,
}

/// A validated rectangular area of a texture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextureRegion {
    texture: TextureHandle,
    origin: PixelPoint,
    size: TextureSize,
}

impl TextureRegion {
    /// Creates a region after checking that it lies inside `texture_size`.
    ///
    /// This accepts metadata rather than an asset store so regions can be
    /// prepared by importers before texture bytes have been loaded.
    ///
    /// # Errors
    ///
    /// Returns [`TextureRegionError::OutOfBounds`] when the rectangle exceeds
    /// the texture, including an overflowing coordinate calculation.
    pub fn new(
        texture: TextureHandle,
        texture_size: TextureSize,
        origin: PixelPoint,
        size: TextureSize,
    ) -> Result<Self, TextureRegionError> {
        let right = origin.x.checked_add(size.width());
        let bottom = origin.y.checked_add(size.height());
        if right.is_none_or(|value| value > texture_size.width())
            || bottom.is_none_or(|value| value > texture_size.height())
        {
            return Err(TextureRegionError::OutOfBounds {
                origin,
                size,
                texture_size,
            });
        }
        Ok(Self {
            texture,
            origin,
            size,
        })
    }

    /// Returns the texture referenced by this region.
    #[must_use]
    pub const fn texture(self) -> TextureHandle {
        self.texture
    }

    /// Returns the top-left pixel coordinate.
    #[must_use]
    pub const fn origin(self) -> PixelPoint {
        self.origin
    }

    /// Returns the region's non-zero dimensions.
    #[must_use]
    pub const fn size(self) -> TextureSize {
        self.size
    }
}

/// A requested texture rectangle was outside its texture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextureRegionError {
    /// The requested region crosses the texture's right or bottom edge.
    OutOfBounds {
        /// Requested top-left coordinate.
        origin: PixelPoint,
        /// Requested non-zero region dimensions.
        size: TextureSize,
        /// Dimensions of the referenced texture.
        texture_size: TextureSize,
    },
}

impl fmt::Display for TextureRegionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfBounds { .. } => formatter.write_str("texture region is out of bounds"),
        }
    }
}

impl Error for TextureRegionError {}

/// A regular grid of sprite regions in one texture.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpriteSheet {
    regions: Vec<TextureRegion>,
    columns: NonZeroU32,
    rows: NonZeroU32,
}

impl SpriteSheet {
    /// Creates a sheet by splitting an entire texture into equally sized cells.
    ///
    /// Every row and column must be complete. Atlases with padding, margins or
    /// irregular rectangles should construct [`TextureRegion`] values directly.
    ///
    /// # Errors
    ///
    /// Returns [`SpriteSheetError::IncompleteGrid`] when `cell_size` does not
    /// divide the texture dimensions exactly.
    pub fn from_grid(
        texture: TextureHandle,
        texture_size: TextureSize,
        cell_size: TextureSize,
    ) -> Result<Self, SpriteSheetError> {
        let texture_width = texture_size.width();
        let texture_height = texture_size.height();
        let cell_width = cell_size.width();
        let cell_height = cell_size.height();
        if !texture_width.is_multiple_of(cell_width) || !texture_height.is_multiple_of(cell_height)
        {
            return Err(SpriteSheetError::IncompleteGrid {
                texture_size,
                cell_size,
            });
        }

        let columns = texture_width / cell_width;
        let rows = texture_height / cell_height;
        let columns = NonZeroU32::new(columns).ok_or(SpriteSheetError::IncompleteGrid {
            texture_size,
            cell_size,
        })?;
        let rows = NonZeroU32::new(rows).ok_or(SpriteSheetError::IncompleteGrid {
            texture_size,
            cell_size,
        })?;
        let capacity = usize::try_from(u64::from(columns.get()) * u64::from(rows.get()))
            .map_err(|_| SpriteSheetError::TooManyCells)?;
        let mut regions = Vec::with_capacity(capacity);
        for row in 0..rows.get() {
            for column in 0..columns.get() {
                let origin = PixelPoint {
                    x: column * cell_width,
                    y: row * cell_height,
                };
                regions.push(
                    TextureRegion::new(texture, texture_size, origin, cell_size)
                        .map_err(SpriteSheetError::InvalidRegion)?,
                );
            }
        }
        Ok(Self {
            regions,
            columns,
            rows,
        })
    }

    /// Returns all regions in row-major order.
    #[must_use]
    pub fn regions(&self) -> &[TextureRegion] {
        &self.regions
    }

    /// Returns a region by its zero-based row-major index.
    #[must_use]
    pub fn region(&self, index: usize) -> Option<TextureRegion> {
        self.regions.get(index).copied()
    }

    /// Returns the number of columns in the source grid.
    #[must_use]
    pub const fn columns(&self) -> u32 {
        self.columns.get()
    }

    /// Returns the number of rows in the source grid.
    #[must_use]
    pub const fn rows(&self) -> u32 {
        self.rows.get()
    }

    /// Builds a uniform-duration animation from all sheet regions.
    ///
    /// The result is equivalent to [`SpriteAnimation::from_regions`].
    ///
    /// # Errors
    ///
    /// Returns [`SpriteAnimationError`] when `frame_duration` is zero.
    pub fn animation(
        &self,
        frame_duration: Duration,
        playback: PlaybackMode,
    ) -> Result<SpriteAnimation, SpriteAnimationError> {
        SpriteAnimation::from_regions(&self.regions, frame_duration, playback)
    }

    /// Builds an animation from selected row-major sheet indices.
    ///
    /// # Errors
    ///
    /// Returns [`SpriteAnimationError::MissingSheetFrame`] for an invalid
    /// index, or [`SpriteAnimationError::ZeroFrameDuration`] for zero duration.
    pub fn animation_from_indices(
        &self,
        indices: &[usize],
        frame_duration: Duration,
        playback: PlaybackMode,
    ) -> Result<SpriteAnimation, SpriteAnimationError> {
        let mut regions = Vec::with_capacity(indices.len());
        for &index in indices {
            regions.push(
                self.region(index)
                    .ok_or(SpriteAnimationError::MissingSheetFrame { index })?,
            );
        }
        SpriteAnimation::from_regions(&regions, frame_duration, playback)
    }
}

/// A grid cannot be represented as complete equal-sized cells.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpriteSheetError {
    /// The texture has unused pixels after splitting into the requested cells.
    IncompleteGrid {
        /// Source texture dimensions.
        texture_size: TextureSize,
        /// Requested cell dimensions.
        cell_size: TextureSize,
    },
    /// The target architecture cannot index every generated grid cell.
    TooManyCells,
    /// A generated grid cell did not fit its texture.
    ///
    /// This variant is defensive: complete grid dimensions should always
    /// produce valid regions.
    InvalidRegion(TextureRegionError),
}

impl fmt::Display for SpriteSheetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IncompleteGrid { .. } => {
                formatter.write_str("sprite-sheet cell size must divide texture dimensions exactly")
            }
            Self::TooManyCells => formatter.write_str("sprite sheet contains too many cells"),
            Self::InvalidRegion(error) => {
                write!(formatter, "invalid generated sprite-sheet region: {error}")
            }
        }
    }
}

impl Error for SpriteSheetError {}

/// A single sprite frame and the amount of time it remains visible.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AnimationFrame {
    region: TextureRegion,
    duration: Duration,
}

impl AnimationFrame {
    /// Creates a frame with a non-zero display duration.
    ///
    /// # Errors
    ///
    /// Returns [`SpriteAnimationError::ZeroFrameDuration`] for zero duration.
    pub fn new(region: TextureRegion, duration: Duration) -> Result<Self, SpriteAnimationError> {
        if duration.is_zero() {
            return Err(SpriteAnimationError::ZeroFrameDuration);
        }
        Ok(Self { region, duration })
    }

    /// Returns the visible texture region.
    #[must_use]
    pub const fn region(self) -> TextureRegion {
        self.region
    }

    /// Returns the frame display duration.
    #[must_use]
    pub const fn duration(self) -> Duration {
        self.duration
    }
}

/// Determines what happens after the last animation frame.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PlaybackMode {
    /// Repeats from the first frame after the last frame.
    #[default]
    Loop,
    /// Stops at the final frame after one pass.
    Once,
    /// Repeats forward then backward without duplicating endpoint frames.
    PingPong,
}

/// An immutable sequence of sprite animation frames.
///
/// Each frame contains a [`TextureRegion`], so a sequence may use either one
/// sprite-sheet texture or individual texture files.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpriteAnimation {
    frames: Vec<AnimationFrame>,
    playback: PlaybackMode,
}

impl SpriteAnimation {
    /// Creates an animation from frames with independent durations.
    ///
    /// # Errors
    ///
    /// Returns [`SpriteAnimationError::Empty`] when `frames` has no entries.
    pub fn new(
        frames: Vec<AnimationFrame>,
        playback: PlaybackMode,
    ) -> Result<Self, SpriteAnimationError> {
        if frames.is_empty() {
            return Err(SpriteAnimationError::Empty);
        }
        Ok(Self { frames, playback })
    }

    /// Creates an animation from a sequence of regions with one frame duration.
    ///
    /// This is the preferred construction path for separate `walk_01.png`,
    /// `walk_02.png`, and similar source files.
    ///
    /// # Errors
    ///
    /// Returns [`SpriteAnimationError::Empty`] for no regions and
    /// [`SpriteAnimationError::ZeroFrameDuration`] for zero duration.
    pub fn from_regions(
        regions: &[TextureRegion],
        frame_duration: Duration,
        playback: PlaybackMode,
    ) -> Result<Self, SpriteAnimationError> {
        if frame_duration.is_zero() {
            return Err(SpriteAnimationError::ZeroFrameDuration);
        }
        let frames = regions
            .iter()
            .copied()
            .map(|region| AnimationFrame {
                region,
                duration: frame_duration,
            })
            .collect();
        Self::new(frames, playback)
    }

    /// Returns the source frames in display order.
    #[must_use]
    pub fn frames(&self) -> &[AnimationFrame] {
        &self.frames
    }

    /// Returns the configured end-of-sequence behaviour.
    #[must_use]
    pub const fn playback(&self) -> PlaybackMode {
        self.playback
    }

    /// Creates state positioned at the first frame.
    #[must_use]
    pub const fn state(&self) -> SpriteAnimationState {
        SpriteAnimationState::new()
    }

    fn phase_count(&self) -> usize {
        match self.playback {
            PlaybackMode::Loop | PlaybackMode::Once => self.frames.len(),
            PlaybackMode::PingPong => self.frames.len().saturating_mul(2).saturating_sub(2).max(1),
        }
    }

    fn frame_index_for_phase(&self, phase: usize) -> usize {
        if self.playback != PlaybackMode::PingPong
            || self.frames.len() == 1
            || phase < self.frames.len()
        {
            return phase;
        }
        (self.frames.len() * 2 - 2) - phase
    }

    fn duration_for_phase(&self, phase: usize) -> Duration {
        self.frames[self.frame_index_for_phase(phase)].duration
    }

    fn cycle_nanos(&self) -> u128 {
        self.nanos_until_phase(self.phase_count())
    }

    fn nanos_until_phase(&self, exclusive_phase: usize) -> u128 {
        (0..exclusive_phase).fold(0_u128, |total, phase| {
            total
                .checked_add(self.duration_for_phase(phase).as_nanos())
                .expect("sprite animation cycle duration overflow")
        })
    }

    fn offset_nanos_for(&self, phase: usize, elapsed: Duration) -> u128 {
        self.nanos_until_phase(phase)
            .checked_add(elapsed.as_nanos())
            .expect("sprite animation duration overflow")
    }

    fn state_at_offset(&self, offset: u128) -> (usize, Duration) {
        let mut remaining = offset;
        for phase in 0..self.phase_count() {
            let duration = self.duration_for_phase(phase).as_nanos();
            if remaining < duration {
                return (phase, duration_from_nanos(remaining));
            }
            remaining = remaining
                .checked_sub(duration)
                .expect("remaining animation time exceeded frame duration");
        }
        unreachable!("repeat animation offset must be smaller than its cycle duration");
    }
}

/// An error while creating a [`SpriteAnimation`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpriteAnimationError {
    /// An animation needs at least one frame.
    Empty,
    /// A frame cannot have zero duration because advancement would be ambiguous.
    ZeroFrameDuration,
    /// A requested sprite-sheet frame index does not exist.
    MissingSheetFrame {
        /// Invalid row-major sheet index.
        index: usize,
    },
}

impl fmt::Display for SpriteAnimationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("sprite animation needs at least one frame"),
            Self::ZeroFrameDuration => {
                formatter.write_str("sprite animation frame duration must be non-zero")
            }
            Self::MissingSheetFrame { index } => {
                write!(
                    formatter,
                    "sprite sheet does not contain frame index {index}"
                )
            }
        }
    }
}

impl Error for SpriteAnimationError {}

/// Mutable deterministic playback state for one [`SpriteAnimation`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpriteAnimationState {
    phase: usize,
    elapsed: Duration,
    finished: bool,
}

impl SpriteAnimationState {
    /// Creates state positioned at the start of an animation.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            phase: 0,
            elapsed: Duration::ZERO,
            finished: false,
        }
    }

    /// Restarts playback from the first frame.
    pub const fn reset(&mut self) {
        self.phase = 0;
        self.elapsed = Duration::ZERO;
        self.finished = false;
    }

    /// Returns the visible source-frame index.
    #[must_use]
    pub fn frame_index(&self, animation: &SpriteAnimation) -> usize {
        let phase = self.phase.min(animation.phase_count() - 1);
        animation.frame_index_for_phase(phase)
    }

    /// Returns the visible frame.
    #[must_use]
    pub fn frame<'a>(&self, animation: &'a SpriteAnimation) -> &'a AnimationFrame {
        &animation.frames[self.frame_index(animation)]
    }

    /// Returns elapsed time within the visible frame.
    #[must_use]
    pub const fn elapsed(&self) -> Duration {
        self.elapsed
    }

    /// Reports whether [`PlaybackMode::Once`] has reached its final frame.
    #[must_use]
    pub const fn is_finished(&self) -> bool {
        self.finished
    }

    /// Advances state by `delta` without using wall-clock time.
    ///
    /// Advancing once by `a + b` has the same result as advancing by `a` then
    /// `b`, which makes replay and fixed-step simulations deterministic. Repeat
    /// modes use a cycle-time calculation, so a very large `delta` does not
    /// iterate once per frame transition.
    pub fn advance(&mut self, animation: &SpriteAnimation, delta: Duration) -> AnimationAdvance {
        if self.phase >= animation.phase_count() {
            self.reset();
        }
        if self.finished || delta.is_zero() {
            return AnimationAdvance {
                frame_changed: false,
                finished_now: false,
            };
        }

        let old_frame = self.frame_index(animation);
        match animation.playback {
            PlaybackMode::Once => self.advance_once(animation, delta),
            PlaybackMode::Loop | PlaybackMode::PingPong => self.advance_repeating(animation, delta),
        }
        AnimationAdvance {
            frame_changed: old_frame != self.frame_index(animation),
            finished_now: self.finished,
        }
    }

    fn advance_once(&mut self, animation: &SpriteAnimation, delta: Duration) {
        let total = animation.cycle_nanos();
        let target = animation
            .offset_nanos_for(self.phase, self.elapsed)
            .checked_add(delta.as_nanos())
            .expect("sprite animation duration overflow");
        if target >= total {
            self.phase = animation.frames.len() - 1;
            self.elapsed = animation.frames[self.phase].duration;
            self.finished = true;
            return;
        }
        (self.phase, self.elapsed) = animation.state_at_offset(target);
    }

    fn advance_repeating(&mut self, animation: &SpriteAnimation, delta: Duration) {
        let cycle = animation.cycle_nanos();
        let target = animation
            .offset_nanos_for(self.phase, self.elapsed)
            .checked_add(delta.as_nanos())
            .expect("sprite animation duration overflow")
            % cycle;
        (self.phase, self.elapsed) = animation.state_at_offset(target);
    }
}

impl Default for SpriteAnimationState {
    fn default() -> Self {
        Self::new()
    }
}

/// The observable effect of one [`SpriteAnimationState::advance`] call.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AnimationAdvance {
    /// Whether the call ended on a different source frame.
    pub frame_changed: bool,
    /// Whether this call completed a [`PlaybackMode::Once`] animation.
    pub finished_now: bool,
}

fn duration_from_nanos(nanos: u128) -> Duration {
    const NANOS_PER_SECOND: u128 = 1_000_000_000;
    let seconds = u64::try_from(nanos / NANOS_PER_SECOND).expect("duration seconds exceed u64");
    let subsec_nanos = u32::try_from(nanos % NANOS_PER_SECOND).expect("subsecond nanos exceed u32");
    Duration::new(seconds, subsec_nanos)
}

#[cfg(test)]
mod tests {
    use super::{
        PixelPoint, PlaybackMode, SpriteAnimation, SpriteSheet, Texture, TextureRegion, TextureSize,
    };
    use std::time::Duration;
    use yuyib_assets::Assets;

    fn region(texture: super::TextureHandle, texture_size: TextureSize, x: u32) -> TextureRegion {
        TextureRegion::new(
            texture,
            texture_size,
            PixelPoint { x, y: 0 },
            TextureSize::new(10, 10).unwrap(),
        )
        .unwrap()
    }

    fn animation(mode: PlaybackMode) -> SpriteAnimation {
        let size = TextureSize::new(30, 10).unwrap();
        let mut textures = Assets::new();
        let texture = textures.insert(Texture::new(size));
        let regions = [
            region(texture, size, 0),
            region(texture, size, 10),
            region(texture, size, 20),
        ];
        SpriteAnimation::from_regions(&regions, Duration::from_millis(100), mode).unwrap()
    }

    #[test]
    fn texture_regions_reject_overflow_and_out_of_bounds_rectangles() {
        let size = TextureSize::new(16, 16).unwrap();
        let mut textures = Assets::new();
        let texture = textures.insert(Texture::new(size));
        assert!(
            TextureRegion::new(
                texture,
                size,
                PixelPoint { x: 15, y: 0 },
                TextureSize::new(2, 1).unwrap(),
            )
            .is_err()
        );
        assert!(
            TextureRegion::new(
                texture,
                size,
                PixelPoint { x: u32::MAX, y: 0 },
                TextureSize::new(1, 1).unwrap(),
            )
            .is_err()
        );
    }

    #[test]
    fn sheet_is_row_major_and_requires_complete_grid() {
        let size = TextureSize::new(32, 16).unwrap();
        let mut textures = Assets::new();
        let texture = textures.insert(Texture::new(size));
        let sheet =
            SpriteSheet::from_grid(texture, size, TextureSize::new(16, 8).unwrap()).unwrap();
        assert_eq!(sheet.columns(), 2);
        assert_eq!(sheet.rows(), 2);
        assert_eq!(sheet.region(2).unwrap().origin(), PixelPoint { x: 0, y: 8 });
        assert!(SpriteSheet::from_grid(texture, size, TextureSize::new(10, 8).unwrap()).is_err());
    }

    #[test]
    fn loop_animation_wraps_without_transition_by_transition_iteration() {
        let animation = animation(PlaybackMode::Loop);
        let mut state = animation.state();
        state.advance(&animation, Duration::from_millis(350));
        assert_eq!(state.frame_index(&animation), 0);
        assert_eq!(state.elapsed(), Duration::from_millis(50));

        state.advance(&animation, Duration::from_hours(1));
        assert_eq!(state.frame_index(&animation), 0);
        assert_eq!(state.elapsed(), Duration::from_millis(50));
    }

    #[test]
    fn once_animation_stops_on_final_frame() {
        let animation = animation(PlaybackMode::Once);
        let mut state = animation.state();
        let advance = state.advance(&animation, Duration::from_millis(300));
        assert!(advance.finished_now);
        assert!(state.is_finished());
        assert_eq!(state.frame_index(&animation), 2);
        assert_eq!(state.elapsed(), Duration::from_millis(100));
        assert!(
            !state
                .advance(&animation, Duration::from_secs(1))
                .finished_now
        );
    }

    #[test]
    fn ping_pong_does_not_duplicate_endpoints() {
        let animation = animation(PlaybackMode::PingPong);
        let mut state = animation.state();
        let mut frames = vec![state.frame_index(&animation)];
        for _ in 0..7 {
            state.advance(&animation, Duration::from_millis(100));
            frames.push(state.frame_index(&animation));
        }
        assert_eq!(frames, [0, 1, 2, 1, 0, 1, 2, 1]);
    }

    #[test]
    fn separate_advances_match_one_combined_advance() {
        let animation = animation(PlaybackMode::PingPong);
        let mut combined = animation.state();
        combined.advance(&animation, Duration::from_millis(365));

        let mut split = animation.state();
        split.advance(&animation, Duration::from_millis(111));
        split.advance(&animation, Duration::from_millis(254));
        assert_eq!(combined, split);
    }
}
