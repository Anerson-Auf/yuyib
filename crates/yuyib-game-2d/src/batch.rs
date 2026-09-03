//! Atlas-backed immediate sprite submission over the shared instanced renderer.
//!
//! [`SpriteBatch2d`] owns only one [`RuntimeSpriteAtlas`]. Consequently every
//! visible queued sprite samples one GPU texture and becomes one instanced draw
//! call. Keep separate textures in separate batches; texture arrays and bindless
//! rendering are deliberately not hidden behind this contract.

use std::{error::Error, fmt};

use yuyib_2d::RuntimeSpriteAtlas;
use yuyib_image::DecodedImage;
use yuyib_render::RenderFrame;
use yuyib_render_2d::{
    Camera2d, GpuSpriteTexture, SpriteDraw, SpriteRenderError, SpriteRenderer, TextureUploadError,
};

use crate::{SpriteCullingError2d, SpriteViewport2d, SpriteViewportError};

/// Per-sprite authoring data accepted by [`SpriteBatch2d::draw`].
///
/// The defaults are suitable for a one-world-unit sprite. Use
/// [`SpriteBatch2d::draw_sprite`] when the natural source-region pixel size is
/// wanted instead.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpriteInstance2d {
    /// World-space centre position.
    pub position: [f32; 2],
    /// World-space size. A negative axis mirrors the sprite.
    pub size: [f32; 2],
    /// Clockwise rotation in radians, matching Yuyib's down-growing Y axis.
    pub rotation_radians: f32,
    /// Straight-alpha tint multiplied with sampled texture colour.
    pub tint: [f32; 4],
    /// Stable painter-order layer. Higher layers appear later.
    pub layer: i32,
}

impl SpriteInstance2d {
    /// Creates a unit-size, unrotated, untinted sprite at `position`.
    #[must_use]
    pub const fn new(position: [f32; 2]) -> Self {
        Self {
            position,
            size: [1.0, 1.0],
            rotation_radians: 0.0,
            tint: [1.0, 1.0, 1.0, 1.0],
            layer: 0,
        }
    }

    /// Sets the world-space size.
    #[must_use]
    pub const fn with_size(mut self, size: [f32; 2]) -> Self {
        self.size = size;
        self
    }

    /// Sets clockwise rotation in radians.
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

    /// Sets painter order.
    #[must_use]
    pub const fn with_layer(mut self, layer: i32) -> Self {
        self.layer = layer;
        self
    }
}

/// Bounded CPU submission policy for one [`SpriteBatch2d`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SpriteBatch2dConfig {
    max_sprites: usize,
    cull_offscreen: bool,
}

impl SpriteBatch2dConfig {
    /// Creates a batch policy with a positive queue bound.
    ///
    /// # Errors
    ///
    /// Returns [`SpriteBatch2dConfigError::ZeroSpriteLimit`] when
    /// `max_sprites` is zero.
    pub const fn new(max_sprites: usize) -> Result<Self, SpriteBatch2dConfigError> {
        if max_sprites == 0 {
            return Err(SpriteBatch2dConfigError::ZeroSpriteLimit);
        }
        Ok(Self {
            max_sprites,
            cull_offscreen: true,
        })
    }

    /// Returns the hard number of queued sprite instances.
    #[must_use]
    pub const fn max_sprites(self) -> usize {
        self.max_sprites
    }

    /// Returns whether camera-viewport culling runs before GPU preparation.
    #[must_use]
    pub const fn cull_offscreen(self) -> bool {
        self.cull_offscreen
    }

    /// Opts in or out of conservative camera-viewport culling.
    #[must_use]
    pub const fn with_culling(mut self, cull_offscreen: bool) -> Self {
        self.cull_offscreen = cull_offscreen;
        self
    }
}

impl Default for SpriteBatch2dConfig {
    fn default() -> Self {
        // A single atlas of 100,000 instances remains one draw call; games may
        // choose a lower budget to preserve CPU frame-time headroom.
        Self {
            max_sprites: 100_000,
            cull_offscreen: true,
        }
    }
}

/// Invalid [`SpriteBatch2dConfig`] input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpriteBatch2dConfigError {
    /// The batch would never accept a sprite.
    ZeroSpriteLimit,
}

impl fmt::Display for SpriteBatch2dConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid SpriteBatch2d configuration: {self:?}")
    }
}

impl Error for SpriteBatch2dConfigError {}

/// Work observed during one [`SpriteBatch2d::render`] call.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SpriteBatch2dStats {
    /// Number of queued sprites inspected this frame.
    pub queued_sprites: usize,
    /// Number rejected by the conservative camera-viewport test.
    pub culled_sprites: usize,
    /// Number of instances sent to the GPU.
    pub drawn_sprites: usize,
    /// Indexed instanced draw calls recorded. This is zero or one per batch.
    pub draw_calls: u32,
}

/// An atlas-backed, retained list of immediate sprite submissions.
///
/// Call [`Self::upload_texture`] once after a decoded atlas image is available,
/// queue named regions through [`Self::draw`] or [`Self::draw_sprite`], then
/// call [`Self::render`] inside the application's render callback. The queue is
/// retained deliberately, so static scenery can be rendered every frame without
/// re-submitting it. Call [`Self::clear`] before building an immediate list for
/// the next frame.
pub struct SpriteBatch2d {
    atlas: RuntimeSpriteAtlas,
    config: SpriteBatch2dConfig,
    draws: Vec<SpriteDraw>,
    renderer: Option<SpriteRenderer>,
    texture: Option<GpuSpriteTexture>,
}

impl SpriteBatch2d {
    /// Creates an empty batch with the default 100,000-sprite bound.
    #[must_use]
    pub fn new(atlas: RuntimeSpriteAtlas) -> Self {
        Self::with_config(atlas, SpriteBatch2dConfig::default())
    }

    /// Creates an empty batch with explicit culling and allocation policy.
    #[must_use]
    pub fn with_config(atlas: RuntimeSpriteAtlas, config: SpriteBatch2dConfig) -> Self {
        Self {
            atlas,
            config,
            draws: Vec::new(),
            renderer: None,
            texture: None,
        }
    }

    /// Returns the runtime atlas used to resolve submitted names.
    #[must_use]
    pub const fn atlas(&self) -> &RuntimeSpriteAtlas {
        &self.atlas
    }

    /// Returns this batch's immutable queue and culling policy.
    #[must_use]
    pub const fn config(&self) -> SpriteBatch2dConfig {
        self.config
    }

    /// Returns queued sprite submissions in their insertion order.
    #[must_use]
    pub fn draws(&self) -> &[SpriteDraw] {
        &self.draws
    }

    /// Removes all queued sprite submissions and returns how many were removed.
    pub fn clear(&mut self) -> usize {
        let count = self.draws.len();
        self.draws.clear();
        count
    }

    /// Uploads or replaces this atlas's decoded image on the active render device.
    ///
    /// The supplied image metadata must exactly match the imported atlas. This
    /// prevents a manifest/image reimport race from producing incorrect UVs.
    /// Replacing a texture is intentional and supports non-destructive reimport.
    ///
    /// # Errors
    ///
    /// Returns an error for mismatched metadata or a failed WGPU upload.
    pub fn upload_texture(
        &mut self,
        frame: &RenderFrame<'_>,
        image: &DecodedImage,
    ) -> Result<(), SpriteBatch2dError> {
        if image.texture() != self.atlas.texture() {
            return Err(SpriteBatch2dError::ImageMetadataMismatch);
        }
        let renderer = self
            .renderer
            .get_or_insert_with(|| SpriteRenderer::new_for_frame(frame));
        self.texture = Some(
            renderer
                .upload_rgba8_for_frame(frame, self.atlas.texture_handle(), image)
                .map_err(SpriteBatch2dError::TextureUpload)?,
        );
        Ok(())
    }

    /// Queues a fully configured named atlas region.
    ///
    /// Resolution and capacity checks happen before mutating the queue, so a
    /// failed call cannot leave a partial sprite submission behind.
    ///
    /// # Errors
    ///
    /// Returns an error when `name` is absent or the bounded queue is full.
    pub fn draw(
        &mut self,
        name: &str,
        instance: SpriteInstance2d,
    ) -> Result<(), SpriteBatch2dError> {
        let region = self
            .atlas
            .region(name)
            .ok_or_else(|| SpriteBatch2dError::UnknownSprite(name.to_owned()))?;
        if self.draws.len() == self.config.max_sprites {
            return Err(SpriteBatch2dError::SpriteLimitExceeded {
                maximum: self.config.max_sprites,
            });
        }
        self.draws.push(SpriteDraw {
            region,
            position: instance.position,
            size: instance.size,
            rotation_radians: instance.rotation_radians,
            tint: instance.tint,
            layer: instance.layer,
        });
        Ok(())
    }

    /// Queues a named region at its natural source-pixel size.
    ///
    /// This is the compact high-level path for conventional pixel-art worlds:
    /// one world unit equals one source pixel under the default [`Camera2d`].
    /// Use [`Self::draw`] to select another size or layer.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::draw`].
    #[allow(clippy::cast_precision_loss)] // Texture dimensions are constrained to practical GPU sizes.
    pub fn draw_sprite(
        &mut self,
        name: &str,
        position: [f32; 2],
        rotation_radians: f32,
        tint: [f32; 4],
    ) -> Result<(), SpriteBatch2dError> {
        let region = self
            .atlas
            .region(name)
            .ok_or_else(|| SpriteBatch2dError::UnknownSprite(name.to_owned()))?;
        let size = region.size();
        self.draw(
            name,
            SpriteInstance2d::new(position)
                .with_size([size.width() as f32, size.height() as f32])
                .with_rotation(rotation_radians)
                .with_tint(tint),
        )
    }

    /// Culls queued sprites against `camera`, prepares their one-atlas GPU batch,
    /// and records at most one indexed instanced draw call into `frame`.
    ///
    /// The retained queue remains unchanged. Call [`Self::clear`] only after a
    /// successful render when using an immediate-per-frame submission model.
    ///
    /// # Errors
    ///
    /// Returns an error when no image is resident, camera/draw data is invalid,
    /// or the renderer rejects the batch.
    pub fn render(
        &mut self,
        frame: &mut RenderFrame<'_>,
        camera: Camera2d,
    ) -> Result<SpriteBatch2dStats, SpriteBatch2dError> {
        let mut stats = SpriteBatch2dStats {
            queued_sprites: self.draws.len(),
            ..SpriteBatch2dStats::default()
        };
        let visible = self.visible_draws(frame, camera, &mut stats)?;
        let Some(texture) = self.texture.as_ref() else {
            return Err(SpriteBatch2dError::TextureNotUploaded);
        };
        let renderer = self
            .renderer
            .get_or_insert_with(|| SpriteRenderer::new_for_frame(frame));
        let prepared = renderer
            .prepare(self.atlas.texture_handle(), texture.size(), visible)
            .map_err(SpriteBatch2dError::Render)?;
        let draw_stats = renderer
            .draw(frame, camera, texture, &prepared)
            .map_err(SpriteBatch2dError::Render)?;
        stats.drawn_sprites = draw_stats.sprites as usize;
        stats.draw_calls = draw_stats.draw_calls;
        Ok(stats)
    }

    fn visible_draws(
        &self,
        frame: &RenderFrame<'_>,
        camera: Camera2d,
        stats: &mut SpriteBatch2dStats,
    ) -> Result<Vec<SpriteDraw>, SpriteBatch2dError> {
        if !self.config.cull_offscreen {
            return Ok(self.draws.clone());
        }
        let (origin, size) = camera
            .viewport(frame.draw_size())
            .map_err(SpriteBatch2dError::Render)?;
        let viewport = SpriteViewport2d::new(origin, size).map_err(SpriteBatch2dError::Viewport)?;
        let mut visible = Vec::with_capacity(self.draws.len());
        for draw in &self.draws {
            if viewport
                .intersects_draw(*draw)
                .map_err(SpriteBatch2dError::Culling)?
            {
                visible.push(*draw);
            } else {
                stats.culled_sprites += 1;
            }
        }
        Ok(visible)
    }
}

/// Failure while configuring, filling, uploading, or rendering a [`SpriteBatch2d`].
#[derive(Debug)]
pub enum SpriteBatch2dError {
    /// The requested region name is absent from the bound atlas.
    UnknownSprite(String),
    /// The bounded retained queue has reached its maximum length.
    SpriteLimitExceeded {
        /// Configured retained-queue maximum.
        maximum: usize,
    },
    /// Decoded image metadata differs from the atlas manifest metadata.
    ImageMetadataMismatch,
    /// A texture upload was rejected by the low-level renderer.
    TextureUpload(TextureUploadError),
    /// No matching GPU texture has been uploaded yet.
    TextureNotUploaded,
    /// Camera data or draw preparation was rejected by the sprite renderer.
    Render(SpriteRenderError),
    /// Camera viewport construction failed.
    Viewport(SpriteViewportError),
    /// A queued draw has invalid culling geometry.
    Culling(SpriteCullingError2d),
}

impl fmt::Display for SpriteBatch2dError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownSprite(name) => {
                write!(formatter, "atlas does not contain sprite {name:?}")
            }
            Self::SpriteLimitExceeded { maximum } => {
                write!(formatter, "sprite batch reached its {maximum}-sprite limit")
            }
            Self::ImageMetadataMismatch => formatter.write_str(
                "decoded image metadata does not match the SpriteBatch2d atlas manifest",
            ),
            Self::TextureUpload(error) => {
                write!(formatter, "sprite batch texture upload failed: {error}")
            }
            Self::TextureNotUploaded => formatter
                .write_str("sprite batch texture is not uploaded; call upload_texture first"),
            Self::Render(error) => write!(formatter, "sprite batch rendering failed: {error}"),
            Self::Viewport(error) => write!(formatter, "sprite batch viewport is invalid: {error}"),
            Self::Culling(error) => write!(formatter, "sprite batch culling failed: {error}"),
        }
    }
}

impl Error for SpriteBatch2dError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::TextureUpload(error) => Some(error),
            Self::Render(error) => Some(error),
            Self::Viewport(error) => Some(error),
            Self::Culling(error) => Some(error),
            Self::UnknownSprite(_)
            | Self::SpriteLimitExceeded { .. }
            | Self::ImageMetadataMismatch
            | Self::TextureNotUploaded => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        SpriteBatch2d, SpriteBatch2dConfig, SpriteBatch2dConfigError, SpriteBatch2dError,
        SpriteInstance2d,
    };
    use yuyib_2d::{register_sprite_atlas_importer, RuntimeSpriteAtlas, Texture};
    use yuyib_assets::{Assets, ImportSource, ImporterRegistry};

    const MANIFEST: &[u8] = br#"{
        "format":"yuyib.sprite_atlas",
        "version":1,
        "texture":{"uri":"sprites/hero.png","width":32,"height":16},
        "regions":[
            {"name":"walk_0","x":0,"y":0,"width":16,"height":16},
            {"name":"walk_1","x":16,"y":0,"width":16,"height":16}
        ],
        "animations":[]
    }"#;

    fn atlas() -> RuntimeSpriteAtlas {
        let mut registry = ImporterRegistry::default();
        register_sprite_atlas_importer(&mut registry).expect("register atlas importer");
        let imported = registry
            .import(ImportSource::new("hero.ysprite", MANIFEST))
            .expect("import manifest")
            .asset;
        let mut textures = Assets::<Texture>::new();
        let handle = textures.insert(imported.texture().clone());
        imported.bind_texture(handle).expect("bind atlas texture")
    }

    #[test]
    fn named_draw_keeps_explicit_instance_properties() {
        let mut batch = SpriteBatch2d::new(atlas());
        batch
            .draw(
                "walk_1",
                SpriteInstance2d::new([2.0, 3.0])
                    .with_size([12.0, -8.0])
                    .with_rotation(0.25)
                    .with_tint([0.5, 0.75, 1.0, 0.25])
                    .with_layer(7),
            )
            .expect("known named region");

        assert_eq!(batch.draws().len(), 1);
        let draw = batch.draws()[0];
        assert_eq!(draw.position, [2.0, 3.0]);
        assert_eq!(draw.size, [12.0, -8.0]);
        assert_eq!(draw.rotation_radians, 0.25);
        assert_eq!(draw.tint, [0.5, 0.75, 1.0, 0.25]);
        assert_eq!(draw.layer, 7);
        assert_eq!(draw.region.origin().x, 16);
    }

    #[test]
    fn compact_draw_uses_source_region_pixel_size() {
        let mut batch = SpriteBatch2d::new(atlas());
        batch
            .draw_sprite("walk_0", [8.0, 4.0], 0.5, [0.25, 0.5, 0.75, 1.0])
            .expect("known named region");

        let draw = batch.draws()[0];
        assert_eq!(draw.size, [16.0, 16.0]);
        assert_eq!(draw.position, [8.0, 4.0]);
        assert_eq!(draw.rotation_radians, 0.5);
        assert_eq!(draw.tint, [0.25, 0.5, 0.75, 1.0]);
    }

    #[test]
    fn failures_leave_the_retained_queue_unchanged() {
        let config = SpriteBatch2dConfig::new(1).expect("positive sprite limit");
        let mut batch = SpriteBatch2d::with_config(atlas(), config);
        assert_eq!(
            SpriteBatch2dConfig::new(0),
            Err(SpriteBatch2dConfigError::ZeroSpriteLimit)
        );

        assert!(matches!(
            batch.draw("missing", SpriteInstance2d::new([0.0, 0.0])),
            Err(SpriteBatch2dError::UnknownSprite(name)) if name == "missing"
        ));
        assert!(batch.draws().is_empty());

        batch
            .draw("walk_0", SpriteInstance2d::new([0.0, 0.0]))
            .expect("first sprite fits");
        assert!(matches!(
            batch.draw("walk_1", SpriteInstance2d::new([1.0, 0.0])),
            Err(SpriteBatch2dError::SpriteLimitExceeded { maximum: 1 })
        ));
        assert_eq!(batch.draws().len(), 1);
        assert_eq!(batch.clear(), 1);
        assert!(batch.draws().is_empty());
    }
}
