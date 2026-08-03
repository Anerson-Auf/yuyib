//! High-level composition of 2D extraction, texture residency and GPU drawing.

use std::{
    collections::{HashMap, VecDeque},
    error::Error,
    fmt,
};

use yuyib_2d::TextureHandle;
use yuyib_ecs::prelude::World;
use yuyib_image::DecodedImage;
use yuyib_render::RenderFrame;
use yuyib_render_2d::{
    Camera2d, GpuSpriteTexture, PreparedSpriteBatch, SpriteDraw, SpriteRenderError, SpriteRenderer,
    TextureUploadError,
};

use crate::{
    SpriteExtractionLimits2d, SpriteViewport2d, SpriteViewportError, TileChunkConfig2d,
    TileChunkExtractError, TileMapError, TileViewport2d, VisibleSpriteExtractError,
    extract_tiles_chunked_2d, extract_visible_sprites_2d,
};

/// Bounded GPU residency and per-frame upload policy for a [`Game2dScene`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextureCacheConfig2d {
    max_resident_textures: usize,
    max_pending_bytes: usize,
    upload_bytes_per_frame: usize,
}

impl TextureCacheConfig2d {
    /// Creates a texture cache policy with non-zero hard limits.
    ///
    /// # Errors
    ///
    /// Returns [`Game2dSceneConfigError`] when any limit is zero.
    pub const fn new(
        max_resident_textures: usize,
        max_pending_bytes: usize,
        upload_bytes_per_frame: usize,
    ) -> Result<Self, Game2dSceneConfigError> {
        if max_resident_textures == 0 {
            return Err(Game2dSceneConfigError::ZeroResidentTextures);
        }
        if max_pending_bytes == 0 {
            return Err(Game2dSceneConfigError::ZeroPendingBytes);
        }
        if upload_bytes_per_frame == 0 {
            return Err(Game2dSceneConfigError::ZeroUploadBudget);
        }
        Ok(Self {
            max_resident_textures,
            max_pending_bytes,
            upload_bytes_per_frame,
        })
    }

    /// Returns the maximum number of simultaneously resident GPU textures.
    #[must_use]
    pub const fn max_resident_textures(self) -> usize {
        self.max_resident_textures
    }

    /// Returns the maximum decoded bytes waiting for GPU publication.
    #[must_use]
    pub const fn max_pending_bytes(self) -> usize {
        self.max_pending_bytes
    }

    /// Returns the soft upload-byte budget for one rendered frame.
    #[must_use]
    pub const fn upload_bytes_per_frame(self) -> usize {
        self.upload_bytes_per_frame
    }
}

impl Default for TextureCacheConfig2d {
    fn default() -> Self {
        Self {
            max_resident_textures: 256,
            max_pending_bytes: 128 * 1024 * 1024,
            upload_bytes_per_frame: 16 * 1024 * 1024,
        }
    }
}

/// Hard extraction and draw limits for one 2D frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DrawBudget2d {
    sprites: usize,
    tiles: usize,
    draw_calls: usize,
}

impl DrawBudget2d {
    /// Creates a non-zero frame budget.
    ///
    /// # Errors
    ///
    /// Returns [`Game2dSceneConfigError`] when any limit is zero.
    pub const fn new(
        max_sprites: usize,
        max_tiles: usize,
        max_draw_calls: usize,
    ) -> Result<Self, Game2dSceneConfigError> {
        if max_sprites == 0 {
            return Err(Game2dSceneConfigError::ZeroSpriteBudget);
        }
        if max_tiles == 0 {
            return Err(Game2dSceneConfigError::ZeroTileBudget);
        }
        if max_draw_calls == 0 {
            return Err(Game2dSceneConfigError::ZeroDrawCallBudget);
        }
        Ok(Self {
            sprites: max_sprites,
            tiles: max_tiles,
            draw_calls: max_draw_calls,
        })
    }

    /// Returns the visible ordinary-sprite limit.
    #[must_use]
    pub const fn max_sprites(self) -> usize {
        self.sprites
    }

    /// Returns the visible tile limit.
    #[must_use]
    pub const fn max_tiles(self) -> usize {
        self.tiles
    }

    /// Returns the maximum texture batches submitted in one frame.
    #[must_use]
    pub const fn max_draw_calls(self) -> usize {
        self.draw_calls
    }
}

impl Default for DrawBudget2d {
    fn default() -> Self {
        Self {
            sprites: 65_536,
            tiles: 65_536,
            draw_calls: 2_048,
        }
    }
}

/// High-level 2D scene policies. All allocations remain explicitly bounded.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Game2dSceneConfig {
    /// World camera used for culling and drawing.
    pub camera: Camera2d,
    /// GPU texture residency and upload policy.
    pub texture_cache: TextureCacheConfig2d,
    /// CPU extraction and GPU draw limits.
    pub draw_budget: DrawBudget2d,
    /// Tile traversal chunk dimensions.
    pub tile_chunk_size: [u32; 2],
}

impl Default for Game2dSceneConfig {
    fn default() -> Self {
        Self {
            camera: Camera2d::default(),
            texture_cache: TextureCacheConfig2d::default(),
            draw_budget: DrawBudget2d::default(),
            tile_chunk_size: [32, 32],
        }
    }
}

/// Invalid high-level scene policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Game2dSceneConfigError {
    /// No GPU texture could become resident.
    ZeroResidentTextures,
    /// No decoded texture could wait for upload.
    ZeroPendingBytes,
    /// The upload loop could never make progress.
    ZeroUploadBudget,
    /// Ordinary sprites could never be extracted.
    ZeroSpriteBudget,
    /// Tiles could never be extracted.
    ZeroTileBudget,
    /// No draw call could be submitted.
    ZeroDrawCallBudget,
}

impl fmt::Display for Game2dSceneConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid Game2dScene configuration: {self:?}")
    }
}

impl Error for Game2dSceneConfigError {}

struct PendingTexture {
    handle: TextureHandle,
    image: DecodedImage,
    bytes: usize,
}

/// End-to-end 2D scene facade over ECS extraction and the low-level sprite renderer.
pub struct Game2dScene {
    config: Game2dSceneConfig,
    renderer: Option<SpriteRenderer>,
    resident: HashMap<TextureHandle, GpuSpriteTexture>,
    pending: VecDeque<PendingTexture>,
    pending_bytes: usize,
}

impl Game2dScene {
    /// Creates an empty scene with explicit policies.
    #[must_use]
    pub fn new(config: Game2dSceneConfig) -> Self {
        Self {
            config,
            renderer: None,
            resident: HashMap::new(),
            pending: VecDeque::new(),
            pending_bytes: 0,
        }
    }

    /// Returns the mutable camera used by the next extraction and render.
    #[must_use]
    pub const fn camera_mut(&mut self) -> &mut Camera2d {
        &mut self.config.camera
    }

    /// Queues a decoded image for bounded upload on the render thread.
    ///
    /// The typed handle remains owned by the caller and can already be used by
    /// sprites. Until upload completes, draws referencing it are counted as
    /// `missing_texture_draws` rather than causing a panic.
    ///
    /// # Errors
    ///
    /// Returns [`TextureQueueError2d`] for duplicate handles or if the pending
    /// decoded-byte bound would be exceeded.
    pub fn queue_texture(
        &mut self,
        handle: TextureHandle,
        image: DecodedImage,
    ) -> Result<(), TextureQueueError2d> {
        if self.resident.contains_key(&handle)
            || self.pending.iter().any(|pending| pending.handle == handle)
        {
            return Err(TextureQueueError2d::AlreadyKnown);
        }
        let bytes = image.pixels().len();
        let next = self.pending_bytes.checked_add(bytes).ok_or(
            TextureQueueError2d::PendingBytesExceeded {
                maximum: self.config.texture_cache.max_pending_bytes,
            },
        )?;
        if next > self.config.texture_cache.max_pending_bytes {
            return Err(TextureQueueError2d::PendingBytesExceeded {
                maximum: self.config.texture_cache.max_pending_bytes,
            });
        }
        self.pending.push_back(PendingTexture {
            handle,
            image,
            bytes,
        });
        self.pending_bytes = next;
        Ok(())
    }

    /// Removes a resident or pending texture and returns whether it was known.
    pub fn remove_texture(&mut self, handle: TextureHandle) -> bool {
        if self.resident.remove(&handle).is_some() {
            return true;
        }
        let Some(index) = self
            .pending
            .iter()
            .position(|pending| pending.handle == handle)
        else {
            return false;
        };
        if let Some(removed) = self.pending.remove(index) {
            self.pending_bytes = self.pending_bytes.saturating_sub(removed.bytes);
        }
        true
    }

    /// Returns the number of GPU-resident textures.
    #[must_use]
    pub fn resident_texture_count(&self) -> usize {
        self.resident.len()
    }

    /// Returns decoded images currently waiting for render-thread upload.
    #[must_use]
    pub fn pending_texture_count(&self) -> usize {
        self.pending.len()
    }

    /// Extracts visible ECS sprites and tiles, uploads bounded texture work and draws.
    ///
    /// Tiles win equal-layer ties before ordinary sprites. Within each source,
    /// existing deterministic ordering is preserved. Adjacent draws using one
    /// texture become one instanced draw call.
    ///
    /// # Errors
    ///
    /// Returns [`Game2dSceneError`] when authored camera/viewport data is invalid,
    /// an extraction bound is exceeded, or GPU preparation/upload rejects data.
    pub fn render(
        &mut self,
        frame: &mut RenderFrame<'_>,
        world: &mut World,
    ) -> Result<Game2dSceneStats, Game2dSceneError> {
        if self.renderer.is_none() {
            self.renderer = Some(SpriteRenderer::new_for_frame(frame));
        }
        let mut stats = self.upload_pending(frame)?;
        let (sprite_viewport, tile_viewport) = self.viewports(frame.draw_size())?;
        let sprites = extract_visible_sprites_2d(
            world,
            sprite_viewport,
            SpriteExtractionLimits2d::new(self.config.draw_budget.sprites)
                .map_err(|_| Game2dSceneError::InvalidInternalPolicy)?,
        )?;
        let tiles = extract_tiles_chunked_2d(
            world,
            tile_viewport,
            TileChunkConfig2d::new(self.config.tile_chunk_size, self.config.draw_budget.tiles)
                .map_err(|_| Game2dSceneError::InvalidInternalPolicy)?,
        )?;

        stats.visible_sprites = sprites.sprite_count();
        stats.visible_tiles = tiles.len();
        let mut draws = Vec::with_capacity(stats.visible_sprites + stats.visible_tiles);
        draws.extend_from_slice(tiles.draws());
        for batch in sprites.batches() {
            draws.extend_from_slice(batch.draws());
        }
        draws.sort_by_key(|draw| draw.layer);
        self.draw_batches(frame, &draws, &mut stats)?;
        Ok(stats)
    }

    fn upload_pending(
        &mut self,
        frame: &RenderFrame<'_>,
    ) -> Result<Game2dSceneStats, Game2dSceneError> {
        let mut stats = Game2dSceneStats::default();
        let mut uploaded_bytes = 0_usize;
        while self.resident.len() < self.config.texture_cache.max_resident_textures {
            let Some(next) = self.pending.front() else {
                break;
            };
            let exceeds_budget = uploaded_bytes
                .checked_add(next.bytes)
                .is_none_or(|bytes| bytes > self.config.texture_cache.upload_bytes_per_frame);
            if exceeds_budget && stats.uploaded_textures > 0 {
                break;
            }
            let Some(next) = self.pending.pop_front() else {
                break;
            };
            self.pending_bytes = self.pending_bytes.saturating_sub(next.bytes);
            let texture = self
                .renderer
                .as_ref()
                .ok_or(Game2dSceneError::InvalidInternalPolicy)?
                .upload_rgba8_for_frame(frame, next.handle, &next.image)?;
            uploaded_bytes = uploaded_bytes.saturating_add(next.bytes);
            stats.uploaded_textures += 1;
            self.resident.insert(next.handle, texture);
        }
        stats.uploaded_bytes = uploaded_bytes;
        stats.pending_textures = self.pending.len();
        stats.resident_textures = self.resident.len();
        Ok(stats)
    }

    fn viewports(
        &self,
        surface: [u32; 2],
    ) -> Result<(SpriteViewport2d, TileViewport2d), Game2dSceneError> {
        let (origin, size) = self.config.camera.viewport(surface)?;
        Ok((
            SpriteViewport2d::new(origin, size)?,
            TileViewport2d::new(origin, size)?,
        ))
    }

    fn draw_batches(
        &mut self,
        frame: &mut RenderFrame<'_>,
        draws: &[SpriteDraw],
        stats: &mut Game2dSceneStats,
    ) -> Result<(), Game2dSceneError> {
        let mut jobs: Vec<(TextureHandle, PreparedSpriteBatch)> = Vec::new();
        let mut start = 0;
        while start < draws.len() {
            let texture_handle = draws[start].region.texture();
            let end = draws[start..]
                .iter()
                .position(|draw| draw.region.texture() != texture_handle)
                .map_or(draws.len(), |relative| start + relative);
            let count = end - start;
            let Some(texture) = self.resident.get(&texture_handle) else {
                stats.missing_texture_draws += count;
                start = end;
                continue;
            };
            if stats.draw_calls == self.config.draw_budget.draw_calls {
                stats.budget_limited_draws += draws.len() - start;
                break;
            }
            let renderer = self
                .renderer
                .as_mut()
                .ok_or(Game2dSceneError::InvalidInternalPolicy)?;
            let batch = renderer.prepare(
                texture_handle,
                texture.size(),
                draws[start..end].iter().copied(),
            )?;
            jobs.push((texture_handle, batch));
            // One GPU pass will submit every remaining job together; count each
            // texture batch against the draw-call budget up front.
            stats.draw_calls += 1;
            start = end;
        }
        if jobs.is_empty() {
            return Ok(());
        }
        let renderer = self
            .renderer
            .as_mut()
            .ok_or(Game2dSceneError::InvalidInternalPolicy)?;
        let mut prepared: Vec<(&GpuSpriteTexture, &PreparedSpriteBatch)> =
            Vec::with_capacity(jobs.len());
        for (handle, batch) in &jobs {
            let Some(texture) = self.resident.get(handle) else {
                continue;
            };
            prepared.push((texture, batch));
        }
        let submitted = renderer.draw_prepared_batches(frame, self.config.camera, &prepared)?;
        stats.drawn_sprites += submitted.sprites as usize;
        Ok(())
    }
}

impl Default for Game2dScene {
    fn default() -> Self {
        Self::new(Game2dSceneConfig::default())
    }
}

/// Observable work and degradation for one high-level 2D render.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Game2dSceneStats {
    /// Ordinary sprites intersecting the camera viewport.
    pub visible_sprites: usize,
    /// Tiles intersecting the camera viewport.
    pub visible_tiles: usize,
    /// Instances submitted to the GPU.
    pub drawn_sprites: usize,
    /// Texture batches submitted to the GPU.
    pub draw_calls: usize,
    /// Draws skipped because their texture is not resident yet.
    pub missing_texture_draws: usize,
    /// Draws skipped after exhausting the draw-call budget.
    pub budget_limited_draws: usize,
    /// Textures uploaded during this frame.
    pub uploaded_textures: usize,
    /// Decoded bytes uploaded during this frame.
    pub uploaded_bytes: usize,
    /// Textures still waiting for upload.
    pub pending_textures: usize,
    /// Textures resident after this frame.
    pub resident_textures: usize,
}

/// Failure to admit decoded texture work into a scene cache.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextureQueueError2d {
    /// This handle is already pending or resident.
    AlreadyKnown,
    /// Adding the image would exceed the decoded pending-byte limit.
    PendingBytesExceeded {
        /// Configured maximum decoded pending bytes.
        maximum: usize,
    },
}

impl fmt::Display for TextureQueueError2d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "could not queue 2D texture: {self:?}")
    }
}

impl Error for TextureQueueError2d {}

/// Failure in high-level 2D extraction, upload or drawing.
#[derive(Debug)]
pub enum Game2dSceneError {
    /// Ordinary sprite culling failed.
    SpriteExtraction(VisibleSpriteExtractError),
    /// Camera-derived ordinary-sprite viewport was invalid.
    SpriteViewport(SpriteViewportError),
    /// Tile extraction failed.
    TileExtraction(TileChunkExtractError),
    /// Camera-derived tile viewport was invalid.
    TileViewport(TileMapError),
    /// GPU upload rejected decoded image data.
    TextureUpload(TextureUploadError),
    /// Batch preparation or drawing failed.
    SpriteRender(SpriteRenderError),
    /// An invariant guaranteed by the public default/constructors was bypassed.
    InvalidInternalPolicy,
}

impl fmt::Display for Game2dSceneError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SpriteExtraction(error) => {
                write!(formatter, "2D sprite extraction failed: {error}")
            }
            Self::SpriteViewport(error) => write!(formatter, "2D sprite viewport failed: {error}"),
            Self::TileExtraction(error) => write!(formatter, "2D tile extraction failed: {error}"),
            Self::TileViewport(error) => write!(formatter, "2D tile viewport failed: {error}"),
            Self::TextureUpload(error) => write!(formatter, "2D texture upload failed: {error}"),
            Self::SpriteRender(error) => write!(formatter, "2D sprite rendering failed: {error}"),
            Self::InvalidInternalPolicy => {
                formatter.write_str("invalid internal Game2dScene policy")
            }
        }
    }
}

impl Error for Game2dSceneError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::SpriteExtraction(error) => Some(error),
            Self::SpriteViewport(error) => Some(error),
            Self::TileExtraction(error) => Some(error),
            Self::TileViewport(error) => Some(error),
            Self::TextureUpload(error) => Some(error),
            Self::SpriteRender(error) => Some(error),
            Self::InvalidInternalPolicy => None,
        }
    }
}

impl From<VisibleSpriteExtractError> for Game2dSceneError {
    fn from(value: VisibleSpriteExtractError) -> Self {
        Self::SpriteExtraction(value)
    }
}

impl From<SpriteViewportError> for Game2dSceneError {
    fn from(value: SpriteViewportError) -> Self {
        Self::SpriteViewport(value)
    }
}

impl From<TileChunkExtractError> for Game2dSceneError {
    fn from(value: TileChunkExtractError) -> Self {
        Self::TileExtraction(value)
    }
}

impl From<TileMapError> for Game2dSceneError {
    fn from(value: TileMapError) -> Self {
        Self::TileViewport(value)
    }
}

impl From<TextureUploadError> for Game2dSceneError {
    fn from(value: TextureUploadError) -> Self {
        Self::TextureUpload(value)
    }
}

impl From<SpriteRenderError> for Game2dSceneError {
    fn from(value: SpriteRenderError) -> Self {
        Self::SpriteRender(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scene_policies_reject_inert_limits() {
        assert_eq!(
            TextureCacheConfig2d::new(0, 1, 1),
            Err(Game2dSceneConfigError::ZeroResidentTextures)
        );
        assert_eq!(
            TextureCacheConfig2d::new(1, 0, 1),
            Err(Game2dSceneConfigError::ZeroPendingBytes)
        );
        assert_eq!(
            TextureCacheConfig2d::new(1, 1, 0),
            Err(Game2dSceneConfigError::ZeroUploadBudget)
        );
        assert_eq!(
            DrawBudget2d::new(0, 1, 1),
            Err(Game2dSceneConfigError::ZeroSpriteBudget)
        );
        assert_eq!(
            DrawBudget2d::new(1, 0, 1),
            Err(Game2dSceneConfigError::ZeroTileBudget)
        );
        assert_eq!(
            DrawBudget2d::new(1, 1, 0),
            Err(Game2dSceneConfigError::ZeroDrawCallBudget)
        );
    }

    #[test]
    fn empty_scene_has_no_hidden_residency() {
        let scene = Game2dScene::default();
        assert_eq!(scene.resident_texture_count(), 0);
        assert_eq!(scene.pending_texture_count(), 0);
    }
}
