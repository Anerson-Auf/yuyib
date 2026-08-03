//! ECS-facing 2D sprite extraction.
//!
//! [`Sprite2d`] belongs on a gameplay entity; [`extract_sprites`] converts the
//! complete ECS state into renderer-facing [`SpriteDraw`] batches, while
//! [`extract_visible_sprites_2d`] builds a bounded viewport-culled snapshot.
//! Extraction is deliberately CPU-only. GPU uploads and render passes remain
//! the responsibility of `yuyib-render-2d`.
//!
//! # Painter-order guarantee
//!
//! Sprites are sorted by ascending [`Sprite2d::layer`]. Equal layers use the
//! entity's full generational ID as a deterministic tie-breaker. Batches only
//! join *adjacent* sprites using the same texture. Consequently a texture may
//! produce more than one batch in a frame, but a texture change never moves a
//! sprite across another sprite's painter order.
//!
//! ```
//! use yuyib_ecs::prelude::*;
//! use yuyib_game_2d::{Sprite2d, extract_sprites};
//!
//! # use yuyib_2d::{PixelPoint, Texture, TextureRegion, TextureSize};
//! # use yuyib_assets::Assets;
//! # let mut textures = Assets::new();
//! # let size = TextureSize::new(1, 1).unwrap();
//! # let texture = textures.insert(Texture::new(size));
//! # let region = TextureRegion::new(texture, size, PixelPoint::default(), size).unwrap();
//! let mut world = World::new();
//! world.spawn(Sprite2d::new(region).with_position([48.0, 64.0]));
//!
//! let extracted = extract_sprites(&mut world);
//! assert_eq!(extracted.sprite_count(), 1);
//! ```

#![forbid(unsafe_code)]

mod animator;
mod composer;
mod scene;

pub use animator::{
    Cardinal2d, CardinalClipPolicy2d, SpriteAnimator2d, SpriteAnimatorError2d, SpriteFacing2d,
    VelocityFacingPolicy2d, VelocityFacingPose2d, apply_cardinal_clips_2d,
    apply_velocity_facing_2d, resolve_cardinal_clips_2d, resolve_velocity_facing_2d,
    step_sprite_animators_2d,
};
pub use composer::{TileMapComposer2d, TileMapComposerError2d, TileStamp2d};
pub use scene::{
    DrawBudget2d, Game2dScene, Game2dSceneConfig, Game2dSceneConfigError, Game2dSceneError,
    Game2dSceneStats, TextureCacheConfig2d, TextureQueueError2d,
};
pub use yuyib_animation::{
    AnimationError, AnimationSet, AnimationStateDef, AnimationStateMachine, PlayOutcome,
};

use std::time::Duration;

use yuyib_2d::{
    AnimationAdvance, SpriteAnimation, SpriteAnimationState, TextureHandle, TextureRegion,
};
use yuyib_ecs::{bevy_ecs::entity::Entity, prelude::*};
use yuyib_physics::{
    Aabb2d, KinematicAabbMove2d, KinematicAabbMoveError, KinematicAabbMoveLimits2d,
    PhysicsConfigError, StaticAabb2d, Vec2, resolve_kinematic_aabb_2d,
};
use yuyib_render_2d::SpriteDraw;

/// An ECS component describing one 2D sprite in world space.
///
/// Coordinates use the renderer's screen-like convention: world Y grows
/// downward, and positive [`rotation_radians`](Self::rotation_radians) rotates
/// clockwise. [`size`](Self::size) is measured in world units; a negative axis
/// mirrors the sprite on that axis.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct Sprite2d {
    /// Validated source rectangle in a texture asset.
    pub region: TextureRegion,
    /// World-space centre position.
    pub position: [f32; 2],
    /// World-space width and height. Negative values mirror the sprite.
    pub size: [f32; 2],
    /// Clockwise rotation in radians because world Y grows downward.
    pub rotation_radians: f32,
    /// Straight-alpha tint multiplied with sampled texture colour.
    pub tint: [f32; 4],
    /// Painter order. Higher layers are rendered after lower layers.
    pub layer: i32,
}

impl Sprite2d {
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

    /// Sets clockwise rotation in radians.
    #[must_use]
    pub const fn with_rotation(mut self, rotation_radians: f32) -> Self {
        self.rotation_radians = rotation_radians;
        self
    }

    /// Sets the sprite's straight-alpha tint.
    #[must_use]
    pub const fn with_tint(mut self, tint: [f32; 4]) -> Self {
        self.tint = tint;
        self
    }

    /// Sets the painter-order layer.
    #[must_use]
    pub const fn with_layer(mut self, layer: i32) -> Self {
        self.layer = layer;
        self
    }

    /// Converts this component to the renderer's user-facing draw type.
    #[must_use]
    pub const fn to_draw(self) -> SpriteDraw {
        SpriteDraw {
            region: self.region,
            position: self.position,
            size: self.size,
            rotation_radians: self.rotation_radians,
            tint: self.tint,
            layer: self.layer,
        }
    }
}

/// One ordered, single-texture group ready for `prepare_sprite_batch`.
///
/// Multiple groups may refer to the same texture when other textures appear
/// between them in painter order. Do not merge those groups: doing so changes
/// the visual result for transparent sprites.
#[derive(Clone, Debug, PartialEq)]
pub struct SpriteDrawBatch {
    texture: TextureHandle,
    draws: Vec<SpriteDraw>,
}

impl SpriteDrawBatch {
    /// Returns the texture used by every draw in this batch.
    #[must_use]
    pub const fn texture(&self) -> TextureHandle {
        self.texture
    }

    /// Returns draws in the exact painter order for this batch.
    #[must_use]
    pub fn draws(&self) -> &[SpriteDraw] {
        &self.draws
    }

    /// Returns the number of draws in this batch.
    #[must_use]
    pub fn len(&self) -> usize {
        self.draws.len()
    }

    /// Returns whether the batch contains no draws.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.draws.is_empty()
    }
}

/// Renderer-facing 2D snapshot extracted from an ECS [`World`].
///
/// The snapshot owns its draws, so it can safely outlive mutable gameplay
/// updates until the render phase begins.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExtractedSprites {
    batches: Vec<SpriteDrawBatch>,
    sprite_count: usize,
}

impl ExtractedSprites {
    /// Returns texture groups in global painter order.
    #[must_use]
    pub fn batches(&self) -> &[SpriteDrawBatch] {
        &self.batches
    }

    /// Returns how many sprite components were extracted.
    #[must_use]
    pub const fn sprite_count(&self) -> usize {
        self.sprite_count
    }

    /// Returns whether no sprite components were extracted.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.sprite_count == 0
    }
}

/// A finite, positive world-space rectangle for ordinary sprite culling.
///
/// The rectangle uses the screen-like convention shared by [`Sprite2d`]:
/// `origin` is its top-left corner and world Y grows downward. A sprite that
/// merely touches an edge is outside; this makes neighbouring viewports form
/// non-overlapping half-open ranges.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpriteViewport2d {
    origin: [f32; 2],
    size: [f32; 2],
}

impl SpriteViewport2d {
    /// Creates a finite viewport with positive dimensions and finite end point.
    ///
    /// # Errors
    ///
    /// Returns [`SpriteViewportError`] when an input is non-finite, a dimension
    /// is not positive, or adding the size to the origin would overflow.
    pub fn new(origin: [f32; 2], size: [f32; 2]) -> Result<Self, SpriteViewportError> {
        if !origin.iter().all(|value| value.is_finite()) {
            return Err(SpriteViewportError::NonFiniteOrigin);
        }
        if !size.iter().all(|value| value.is_finite() && *value > 0.0) {
            return Err(SpriteViewportError::InvalidSize);
        }
        if !(origin[0] + size[0]).is_finite() || !(origin[1] + size[1]).is_finite() {
            return Err(SpriteViewportError::EndOverflow);
        }
        Ok(Self { origin, size })
    }

    /// Returns the top-left world-space corner.
    #[must_use]
    pub const fn origin(self) -> [f32; 2] {
        self.origin
    }

    /// Returns the positive world-space width and height.
    #[must_use]
    pub const fn size(self) -> [f32; 2] {
        self.size
    }

    fn end(self) -> [f32; 2] {
        [self.origin[0] + self.size[0], self.origin[1] + self.size[1]]
    }
}

/// Invalid [`SpriteViewport2d`] authoring input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpriteViewportError {
    /// At least one origin coordinate was `NaN` or infinite.
    NonFiniteOrigin,
    /// At least one size coordinate was `NaN`, infinite, zero or negative.
    InvalidSize,
    /// Adding size to origin did not produce a finite viewport endpoint.
    EndOverflow,
}

impl std::fmt::Display for SpriteViewportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid sprite viewport: {self:?}")
    }
}

impl std::error::Error for SpriteViewportError {}

/// Validated maximum for one viewport-culled sprite snapshot.
///
/// The limit covers visible sprites, not the total entity count inspected by
/// the ECS query. It bounds all snapshot draws and batches returned by
/// [`extract_visible_sprites_2d`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SpriteExtractionLimits2d {
    max_visible_sprites: usize,
}

impl SpriteExtractionLimits2d {
    /// Creates a positive visible-sprite limit.
    ///
    /// # Errors
    ///
    /// Returns [`SpriteExtractionLimitsError::ZeroVisibleSpriteLimit`] when
    /// `max_visible_sprites` is zero.
    pub const fn new(max_visible_sprites: usize) -> Result<Self, SpriteExtractionLimitsError> {
        if max_visible_sprites == 0 {
            return Err(SpriteExtractionLimitsError::ZeroVisibleSpriteLimit);
        }
        Ok(Self {
            max_visible_sprites,
        })
    }

    /// Returns the maximum visible sprites this extractor may snapshot.
    #[must_use]
    pub const fn max_visible_sprites(self) -> usize {
        self.max_visible_sprites
    }
}

impl Default for SpriteExtractionLimits2d {
    fn default() -> Self {
        // This constant satisfies the constructor invariant.
        Self {
            max_visible_sprites: 65_536,
        }
    }
}

/// Invalid [`SpriteExtractionLimits2d`] authoring input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpriteExtractionLimitsError {
    /// The visible-sprite maximum was zero.
    ZeroVisibleSpriteLimit,
}

impl std::fmt::Display for SpriteExtractionLimitsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid sprite extraction limits: {self:?}")
    }
}

impl std::error::Error for SpriteExtractionLimitsError {}

/// Failure while extracting a viewport-culled [`Sprite2d`] snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VisibleSpriteExtractError {
    /// A component had non-finite position, size, rotation or derived AABB.
    InvalidSpriteGeometry {
        /// Entity that owns the invalid component.
        entity: Entity,
    },
    /// The visible snapshot would exceed its configured allocation budget.
    VisibleSpriteLimitExceeded {
        /// Configured maximum number of visible sprites.
        maximum: usize,
    },
}

impl std::fmt::Display for VisibleSpriteExtractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "visible sprite extraction failed: {self:?}")
    }
}

impl std::error::Error for VisibleSpriteExtractError {}

fn sprite_intersects_viewport(sprite: &Sprite2d, viewport: SpriteViewport2d) -> Result<bool, ()> {
    if !sprite.position.iter().all(|value| value.is_finite())
        || !sprite.size.iter().all(|value| value.is_finite())
        || !sprite.rotation_radians.is_finite()
    {
        return Err(());
    }

    let half_width = sprite.size[0].abs() * 0.5;
    let half_height = sprite.size[1].abs() * 0.5;
    let cosine = sprite.rotation_radians.cos().abs();
    let sine = sprite.rotation_radians.sin().abs();
    let extent_x = cosine * half_width + sine * half_height;
    let extent_y = sine * half_width + cosine * half_height;
    let left = sprite.position[0] - extent_x;
    let right = sprite.position[0] + extent_x;
    let top = sprite.position[1] - extent_y;
    let bottom = sprite.position[1] + extent_y;
    if ![
        half_width,
        half_height,
        cosine,
        sine,
        extent_x,
        extent_y,
        left,
        right,
        top,
        bottom,
    ]
    .iter()
    .all(|value| value.is_finite())
    {
        return Err(());
    }

    let viewport_end = viewport.end();
    Ok(right > viewport.origin[0]
        && left < viewport_end[0]
        && bottom > viewport.origin[1]
        && top < viewport_end[1])
}

fn batches_from_ordered_sprite_draws(extracted: Vec<(u64, SpriteDraw)>) -> ExtractedSprites {
    let mut extracted = extracted;
    extracted.sort_by_key(|(entity_bits, draw)| (draw.layer, *entity_bits));
    let sprite_count = extracted.len();
    let mut batches: Vec<SpriteDrawBatch> = Vec::new();

    for (_, draw) in extracted {
        let texture = draw.region.texture();
        match batches.last_mut() {
            Some(batch) if batch.texture == texture => batch.draws.push(draw),
            _ => {
                batches.push(SpriteDrawBatch {
                    texture,
                    draws: vec![draw],
                });
            }
        }
    }

    ExtractedSprites {
        batches,
        sprite_count,
    }
}

/// Extracts all [`Sprite2d`] components from `world` into ordered texture groups.
///
/// Needs mutable world access because Bevy ECS query
/// state is initialized lazily. It does not mutate any gameplay component or
/// resource.
#[must_use]
pub fn extract_sprites(world: &mut World) -> ExtractedSprites {
    let extracted: Vec<(u64, SpriteDraw)> = world
        .query::<(Entity, &Sprite2d)>()
        .iter(world)
        .map(|(entity, sprite)| (entity.to_bits(), sprite.to_draw()))
        .collect();
    batches_from_ordered_sprite_draws(extracted)
}

/// Extracts a bounded, viewport-visible [`Sprite2d`] snapshot.
///
/// Every sprite uses a conservative rotated AABB. Negative sizes mirror the
/// draw but use their absolute dimensions for culling. The output preserves the
/// same layer/entity painter order and adjacent-only texture batching as
/// [`extract_sprites`]. A sprite touching only a viewport edge is excluded.
///
/// This is CPU culling only: it performs no camera transform, GPU upload,
/// occlusion test, spatial index lookup, or texture residency management.
///
/// # Errors
///
/// Returns [`VisibleSpriteExtractError::InvalidSpriteGeometry`] for non-finite
/// component geometry or a non-finite derived AABB. Returns
/// [`VisibleSpriteExtractError::VisibleSpriteLimitExceeded`] before adding a
/// draw beyond `limits`, so it does not construct an unbounded snapshot.
pub fn extract_visible_sprites_2d(
    world: &mut World,
    viewport: SpriteViewport2d,
    limits: SpriteExtractionLimits2d,
) -> Result<ExtractedSprites, VisibleSpriteExtractError> {
    let mut visible = Vec::new();
    let mut query = world.query::<(Entity, &Sprite2d)>();
    for (entity, sprite) in query.iter(world) {
        let intersects = sprite_intersects_viewport(sprite, viewport)
            .map_err(|()| VisibleSpriteExtractError::InvalidSpriteGeometry { entity })?;
        if !intersects {
            continue;
        }
        if visible.len() == limits.max_visible_sprites {
            return Err(VisibleSpriteExtractError::VisibleSpriteLimitExceeded {
                maximum: limits.max_visible_sprites,
            });
        }
        visible.push((entity.to_bits(), sprite.to_draw()));
    }
    Ok(batches_from_ordered_sprite_draws(visible))
}

/// Mutable ECS animation state linked to an existing [`Sprite2d`] component.
///
/// [`SpriteAnimation`] frames already carry [`TextureRegion`] values, so this
/// supports both atlas regions and separate texture files without a second
/// format. Image decode, asset streaming and GPU upload remain external.
#[derive(Component, Clone, Debug, PartialEq)]
pub struct AnimatedSprite2d {
    animation: SpriteAnimation,
    state: SpriteAnimationState,
    playing: bool,
}

impl AnimatedSprite2d {
    /// Creates a playing animation at its first source frame.
    #[must_use]
    pub fn new(animation: SpriteAnimation) -> Self {
        Self {
            state: animation.state(),
            animation,
            playing: true,
        }
    }
    /// Returns immutable authored frames/playback data.
    #[must_use]
    pub const fn animation(&self) -> &SpriteAnimation {
        &self.animation
    }
    /// Returns current deterministic playback state.
    #[must_use]
    pub const fn state(&self) -> &SpriteAnimationState {
        &self.state
    }
    /// Returns whether this component advances when stepped.
    #[must_use]
    pub const fn is_playing(&self) -> bool {
        self.playing
    }
    /// Sets playback without resetting the frame.
    pub const fn set_playing(&mut self, playing: bool) {
        self.playing = playing;
    }
    /// Restarts from frame zero and resumes playback.
    pub const fn restart(&mut self) {
        self.state.reset();
        self.playing = true;
    }

    /// Replaces authored frames and restarts from the first frame.
    pub fn replace_animation(&mut self, animation: SpriteAnimation) {
        self.state = animation.state();
        self.animation = animation;
        self.playing = true;
    }
}

/// Observable transition emitted by [`step_sprite_animations_2d`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpriteAnimationEvent2d {
    /// The sprite region changed to a new source frame.
    FrameChanged {
        /// Entity whose [`Sprite2d::region`] changed.
        entity: Entity,
    },
    /// A once animation reached its final frame and stopped advancing.
    Finished {
        /// Entity whose once animation finished.
        entity: Entity,
    },
}

/// Advances every animated sprite by caller-supplied simulation time.
///
/// The current visible region is copied into [`Sprite2d`] even for zero delta,
/// so spawning an animation with any atlas/standalone first frame becomes
/// renderable immediately. Returned events are sorted by full entity ID, with
/// `FrameChanged` before `Finished` for the same entity.
///
/// This is a pure ECS/data update: no wall clock, texture decode, GPU upload,
/// render pass or asset validity check is performed.
pub fn step_sprite_animations_2d(
    world: &mut World,
    delta: Duration,
) -> Vec<SpriteAnimationEvent2d> {
    let mut ordered = Vec::new();
    let mut query = world.query::<(Entity, &mut Sprite2d, &mut AnimatedSprite2d)>();
    for (entity, mut sprite, mut animated) in query.iter_mut(world) {
        let animation = animated.animation.clone();
        let advance = if animated.playing {
            animated.state.advance(&animation, delta)
        } else {
            AnimationAdvance::default()
        };
        sprite.region = animated.state.frame(&animation).region();
        if advance.frame_changed {
            ordered.push((
                entity.to_bits(),
                0_u8,
                SpriteAnimationEvent2d::FrameChanged { entity },
            ));
        }
        if advance.finished_now {
            ordered.push((
                entity.to_bits(),
                1_u8,
                SpriteAnimationEvent2d::Finished { entity },
            ));
        }
    }
    ordered.sort_by_key(|(bits, kind, _)| (*bits, *kind));
    ordered.into_iter().map(|(_, _, event)| event).collect()
}

/// Explicit finite 2D viewport used for renderer-neutral tile culling.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TileViewport2d {
    /// Top-left world position.
    pub origin: [f32; 2],
    /// Positive world-space width and height.
    pub size: [f32; 2],
}
impl TileViewport2d {
    /// Creates a finite viewport with positive dimensions.
    ///
    /// # Errors
    ///
    /// Returns [`TileMapError::InvalidViewport`] for non-finite origin or non-positive size.
    pub fn new(origin: [f32; 2], size: [f32; 2]) -> Result<Self, TileMapError> {
        if !origin.iter().all(|value| value.is_finite())
            || !size.iter().all(|value| value.is_finite() && *value > 0.0)
        {
            return Err(TileMapError::InvalidViewport);
        }
        Ok(Self { origin, size })
    }
}

/// One atlas-backed tile map. Tile coordinates increase right/down.
///
/// Each `Some(index)` selects an atlas region; `None` is empty. All regions
/// must use the same texture, but this component does not decode or upload it.
#[derive(Component, Clone, Debug, PartialEq)]
pub struct TileMap2d {
    grid: [u32; 2],
    tile_size: [f32; 2],
    regions: Vec<TextureRegion>,
    tiles: Vec<Option<u32>>,
    /// Top-left world position of tile `(0, 0)`.
    pub position: [f32; 2],
    /// Painter layer inherited by every tile.
    pub layer: i32,
    /// Whether extraction includes this map.
    pub visible: bool,
    animation: Option<AnimatedSprite2d>,
}
impl TileMap2d {
    /// Creates a validated one-atlas tile map in row-major order.
    ///
    /// # Errors
    ///
    /// Returns [`TileMapError`] for invalid dimensions/data, atlas mismatch or tile indices outside `regions`.
    pub fn new(
        grid: [u32; 2],
        tile_size: [f32; 2],
        regions: Vec<TextureRegion>,
        tiles: Vec<Option<u32>>,
    ) -> Result<Self, TileMapError> {
        if grid[0] == 0 || grid[1] == 0 {
            return Err(TileMapError::ZeroGrid);
        }
        if !tile_size
            .iter()
            .all(|value| value.is_finite() && *value > 0.0)
        {
            return Err(TileMapError::InvalidTileSize);
        }
        let expected = usize::try_from(u64::from(grid[0]) * u64::from(grid[1]))
            .map_err(|_| TileMapError::GridTooLarge)?;
        if tiles.len() != expected {
            return Err(TileMapError::TileCount {
                expected,
                actual: tiles.len(),
            });
        }
        let texture = regions.first().ok_or(TileMapError::NoRegions)?.texture();
        if regions.iter().any(|region| region.texture() != texture) {
            return Err(TileMapError::MultipleTextures);
        }
        if tiles
            .iter()
            .flatten()
            .any(|index| usize::try_from(*index).map_or(true, |value| value >= regions.len()))
        {
            return Err(TileMapError::InvalidTileIndex);
        }
        Ok(Self {
            grid,
            tile_size,
            regions,
            tiles,
            position: [0.0; 2],
            layer: 0,
            visible: true,
            animation: None,
        })
    }
    /// Sets top-left world position.
    #[must_use]
    pub const fn with_position(mut self, position: [f32; 2]) -> Self {
        self.position = position;
        self
    }
    /// Sets painter layer.
    #[must_use]
    pub const fn with_layer(mut self, layer: i32) -> Self {
        self.layer = layer;
        self
    }
    /// Sets visibility.
    #[must_use]
    pub const fn with_visible(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }
    /// Returns grid width/height.
    #[must_use]
    pub const fn grid(&self) -> [u32; 2] {
        self.grid
    }
    /// Returns all atlas regions.
    #[must_use]
    pub fn regions(&self) -> &[TextureRegion] {
        &self.regions
    }
    /// Attaches one shared timeline for every non-empty tile in this map.
    ///
    /// # Errors
    ///
    /// Returns [`TileMapError::AnimationTextureMismatch`] for a frame outside the atlas texture.
    pub fn with_animation(mut self, animation: SpriteAnimation) -> Result<Self, TileMapError> {
        let texture = self.regions[0].texture();
        if animation
            .frames()
            .iter()
            .any(|frame| frame.region().texture() != texture)
        {
            return Err(TileMapError::AnimationTextureMismatch);
        }
        self.animation = Some(AnimatedSprite2d::new(animation));
        Ok(self)
    }
    #[allow(clippy::cast_precision_loss)] // Tile-grid dimensions are practical renderer coordinates below f32 precision limits.
    fn draw_at(&self, column: u32, row: u32) -> Option<SpriteDraw> {
        let offset =
            usize::try_from(u64::from(row) * u64::from(self.grid[0]) + u64::from(column)).ok()?;
        let index = usize::try_from(self.tiles.get(offset).copied().flatten()?).ok()?;
        Some(SpriteDraw {
            region: self
                .animation
                .as_ref()
                .map_or(self.regions[index], |animation| {
                    animation.state.frame(&animation.animation).region()
                }),
            position: [
                self.position[0] + (column as f32 + 0.5) * self.tile_size[0],
                self.position[1] + (row as f32 + 0.5) * self.tile_size[1],
            ],
            size: self.tile_size,
            rotation_radians: 0.0,
            tint: [1.0; 4],
            layer: self.layer,
        })
    }
}

/// Tile-map authoring or viewport validation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TileMapError {
    /// Grid has a zero dimension.
    ZeroGrid,
    /// Tile size was non-finite or not positive.
    InvalidTileSize,
    /// Grid product cannot fit memory indexing.
    GridTooLarge,
    /// Tile count differs from grid area.
    TileCount {
        /// Expected cells.
        expected: usize,
        /// Supplied cells.
        actual: usize,
    },
    /// No atlas regions supplied.
    NoRegions,
    /// Atlas regions use different texture handles.
    MultipleTextures,
    /// A tile referred outside atlas regions.
    InvalidTileIndex,
    /// Viewport origin/size was invalid.
    InvalidViewport,
    /// Animated frame belongs to another texture than the map atlas.
    AnimationTextureMismatch,
}
impl std::fmt::Display for TileMapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid tile map: {self:?}")
    }
}
impl std::error::Error for TileMapError {}

/// Renderer-neutral visible tile snapshot preserving global painter order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExtractedTiles2d {
    draws: Vec<SpriteDraw>,
}
impl ExtractedTiles2d {
    /// Returns visible tile draws ordered by layer, entity, row and column.
    #[must_use]
    pub fn draws(&self) -> &[SpriteDraw] {
        &self.draws
    }
    /// Returns visible tile count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.draws.len()
    }
    /// Returns whether no tile is visible.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.draws.is_empty()
    }
}

/// Validated CPU extraction limits for chunked tile maps.
///
/// Chunk dimensions are measured in tiles. They affect CPU traversal only:
/// the returned snapshot remains globally painter-sorted and does not imply a
/// GPU texture or geometry allocation per chunk.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TileChunkConfig2d {
    size: [u32; 2],
    max_draws: usize,
}
impl TileChunkConfig2d {
    /// Creates chunk traversal configuration with a strict output limit.
    ///
    /// # Errors
    ///
    /// Returns [`TileChunkConfigError`] when a chunk dimension or draw limit is zero.
    pub const fn new(size: [u32; 2], max_draws: usize) -> Result<Self, TileChunkConfigError> {
        if size[0] == 0 || size[1] == 0 {
            return Err(TileChunkConfigError::ZeroChunkSize);
        }
        if max_draws == 0 {
            return Err(TileChunkConfigError::ZeroDrawLimit);
        }
        Ok(Self { size, max_draws })
    }

    /// Returns chunk width and height in tiles.
    #[must_use]
    pub const fn size(self) -> [u32; 2] {
        self.size
    }

    /// Returns the maximum number of visible tile draws the extractor may return.
    #[must_use]
    pub const fn max_draws(self) -> usize {
        self.max_draws
    }
}
impl Default for TileChunkConfig2d {
    fn default() -> Self {
        // These constants satisfy the constructor invariants.
        Self {
            size: [32, 32],
            max_draws: 65_536,
        }
    }
}

/// Invalid [`TileChunkConfig2d`] authoring input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TileChunkConfigError {
    /// At least one chunk dimension was zero.
    ZeroChunkSize,
    /// The extractor draw limit was zero.
    ZeroDrawLimit,
}
impl std::fmt::Display for TileChunkConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid tile chunk configuration: {self:?}")
    }
}
impl std::error::Error for TileChunkConfigError {}

/// Failure while extracting a chunked tile-map snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TileChunkExtractError {
    /// A visible map would emit more draws than configured.
    DrawLimitExceeded {
        /// Configured maximum number of draws.
        maximum: usize,
    },
}
impl std::fmt::Display for TileChunkExtractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "chunked tile extraction failed: {self:?}")
    }
}
impl std::error::Error for TileChunkExtractError {}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)] // Viewport values are clamped to non-negative map dimensions before integer indexing.
fn visible_tile_range(map: &TileMap2d, viewport: TileViewport2d) -> Option<([u32; 2], [u32; 2])> {
    let viewport_end = [
        viewport.origin[0] + viewport.size[0],
        viewport.origin[1] + viewport.size[1],
    ];
    let first = [
        ((viewport.origin[0] - map.position[0]) / map.tile_size[0])
            .floor()
            .max(0.0) as u32,
        ((viewport.origin[1] - map.position[1]) / map.tile_size[1])
            .floor()
            .max(0.0) as u32,
    ];
    let end_exclusive = [
        ((viewport_end[0] - map.position[0]) / map.tile_size[0])
            .ceil()
            .clamp(0.0, map.grid[0] as f32) as u32,
        ((viewport_end[1] - map.position[1]) / map.tile_size[1])
            .ceil()
            .clamp(0.0, map.grid[1] as f32) as u32,
    ];
    if first[0] >= end_exclusive[0] || first[1] >= end_exclusive[1] {
        return None;
    }
    Some((first, [end_exclusive[0] - 1, end_exclusive[1] - 1]))
}

/// Extracts visible tiles by traversing only map chunks that intersect `viewport`.
///
/// The returned draws preserve the same global order as [`extract_tiles_2d`]:
/// layer, entity, row, then column. Use [`extract_tiles_2d`] for small maps or
/// when a hard extraction limit is unnecessary. This function does not stream
/// GPU resources, generate collision, or retain chunk residency between calls.
///
/// # Errors
///
/// Returns [`TileChunkExtractError::DrawLimitExceeded`] before returning an
/// unbounded snapshot when the configured draw maximum would be exceeded.
pub fn extract_tiles_chunked_2d(
    world: &mut World,
    viewport: TileViewport2d,
    config: TileChunkConfig2d,
) -> Result<ExtractedTiles2d, TileChunkExtractError> {
    let mut ordered = Vec::new();
    let mut query = world.query::<(Entity, &TileMap2d)>();
    for (entity, map) in query.iter(world) {
        if !map.visible {
            continue;
        }
        let Some((first, last)) = visible_tile_range(map, viewport) else {
            continue;
        };
        let first_chunk = [first[0] / config.size[0], first[1] / config.size[1]];
        let last_chunk = [last[0] / config.size[0], last[1] / config.size[1]];
        for chunk_row in first_chunk[1]..=last_chunk[1] {
            let row_start = first[1].max(chunk_row * config.size[1]);
            let row_end = last[1].min((chunk_row + 1).saturating_mul(config.size[1]) - 1);
            for chunk_column in first_chunk[0]..=last_chunk[0] {
                let column_start = first[0].max(chunk_column * config.size[0]);
                let column_end = last[0].min((chunk_column + 1).saturating_mul(config.size[0]) - 1);
                for row in row_start..=row_end {
                    for column in column_start..=column_end {
                        let Some(draw) = map.draw_at(column, row) else {
                            continue;
                        };
                        if ordered.len() == config.max_draws {
                            return Err(TileChunkExtractError::DrawLimitExceeded {
                                maximum: config.max_draws,
                            });
                        }
                        ordered.push((draw.layer, entity.to_bits(), row, column, draw));
                    }
                }
            }
        }
    }
    ordered.sort_by_key(|(layer, entity, row, column, _)| (*layer, *entity, *row, *column));
    Ok(ExtractedTiles2d {
        draws: ordered.into_iter().map(|(_, _, _, _, draw)| draw).collect(),
    })
}

/// Extracts visible atlas tiles intersecting `viewport`.
///
/// This performs CPU rectangle culling only. It does not batch GPU calls,
/// load images, cull sprites, or merge maps across transparent painter order.
#[must_use]
#[allow(clippy::cast_precision_loss)] // Tile-grid dimensions are practical renderer coordinates below f32 precision limits.
pub fn extract_tiles_2d(world: &mut World, viewport: TileViewport2d) -> ExtractedTiles2d {
    let mut ordered = Vec::new();
    let mut query = world.query::<(Entity, &TileMap2d)>();
    for (entity, map) in query.iter(world) {
        if !map.visible {
            continue;
        }
        for row in 0..map.grid[1] {
            for column in 0..map.grid[0] {
                let left = map.position[0] + column as f32 * map.tile_size[0];
                let top = map.position[1] + row as f32 * map.tile_size[1];
                if left + map.tile_size[0] <= viewport.origin[0]
                    || left >= viewport.origin[0] + viewport.size[0]
                    || top + map.tile_size[1] <= viewport.origin[1]
                    || top >= viewport.origin[1] + viewport.size[1]
                {
                    continue;
                }
                if let Some(draw) = map.draw_at(column, row) {
                    ordered.push((draw.layer, entity.to_bits(), row, column, draw));
                }
            }
        }
    }
    ordered.sort_by_key(|(layer, entity, row, column, _)| (*layer, *entity, *row, *column));
    ExtractedTiles2d {
        draws: ordered.into_iter().map(|(_, _, _, _, draw)| draw).collect(),
    }
}

/// Advances shared animated tile-map timelines by caller-supplied delta.
///
/// State is one timeline per map, not one allocation per visible tile. Call it
/// before [`extract_tiles_2d`]; viewport culling then determines emitted draws.
pub fn step_tile_map_animations_2d(world: &mut World, delta: Duration) {
    let mut query = world.query::<&mut TileMap2d>();
    for mut map in query.iter_mut(world) {
        if let Some(animation) = &mut map.animation {
            let source = animation.animation.clone();
            if animation.playing {
                let _ = animation.state.advance(&source, delta);
            }
        }
    }
}

/// Per-cell collision metadata for a [`TileMap2d`] grid in row-major order.
#[derive(Component, Clone, Debug, Eq, PartialEq)]
pub struct TileCollision2d {
    solid: Vec<bool>,
}
impl TileCollision2d {
    /// Creates validated collision flags for `grid`.
    ///
    /// # Errors
    ///
    /// Returns [`TileCollisionError::CellCount`] when flags do not match the map grid area.
    pub fn new(grid: [u32; 2], solid: Vec<bool>) -> Result<Self, TileCollisionError> {
        let expected = usize::try_from(u64::from(grid[0]) * u64::from(grid[1]))
            .map_err(|_| TileCollisionError::GridTooLarge)?;
        if solid.len() != expected {
            return Err(TileCollisionError::CellCount {
                expected,
                actual: solid.len(),
            });
        }
        Ok(Self { solid })
    }

    /// Row-major solid flags.
    #[must_use]
    pub fn solid(&self) -> &[bool] {
        &self.solid
    }

    fn is_solid(&self, index: usize) -> bool {
        self.solid.get(index).copied().unwrap_or(false)
    }
}
/// Tile collision metadata failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TileCollisionError {
    /// Grid product cannot fit indexing.
    GridTooLarge,
    /// Flag count differs from grid area.
    CellCount {
        /// Number of cells required by the map dimensions.
        expected: usize,
        /// Number of cells supplied by the author.
        actual: usize,
    },
}
impl std::fmt::Display for TileCollisionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid tile collision metadata: {self:?}")
    }
}
impl std::error::Error for TileCollisionError {}

/// One axis-aligned world-space rectangle emitted for a solid tile.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TileCollisionRect2d {
    /// Tilemap entity that produced this rectangle.
    pub entity: Entity,
    /// Zero-based tile column in source map order.
    pub column: u32,
    /// Zero-based tile row in source map order.
    pub row: u32,
    /// World-space top-left rectangle origin.
    pub origin: [f32; 2],
    /// World-space rectangle width and height.
    pub size: [f32; 2],
}
/// Bounded tile collision extraction policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TileCollisionLimits {
    /// Maximum collision rectangles returned from one extraction call.
    pub max_rectangles: usize,
}
impl Default for TileCollisionLimits {
    fn default() -> Self {
        Self {
            max_rectangles: 65_536,
        }
    }
}
/// Collision extraction failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TileCollisionExtractError {
    /// Extraction would exceed its configured rectangle budget.
    LimitExceeded {
        /// Configured maximum rectangle count.
        maximum: usize,
    },
}
impl std::fmt::Display for TileCollisionExtractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "tile collision extraction failed: {self:?}")
    }
}
impl std::error::Error for TileCollisionExtractError {}

/// Extracts all marked solid cells into deterministic world-space AABBs.
///
/// Ordering is entity ID, row, column and is deliberately independent of
/// visibility/viewport. This is a query hook for a caller-owned physics solver;
/// it performs no collision response, merging, navmesh or GPU work.
///
/// # Errors
///
/// Returns [`TileCollisionExtractError::LimitExceeded`] when the configured
/// rectangle budget would be exceeded.
#[allow(clippy::cast_precision_loss)] // Tile-grid dimensions are practical renderer coordinates below f32 precision limits.
pub fn extract_tile_collisions_2d(
    world: &mut World,
    limits: TileCollisionLimits,
) -> Result<Vec<TileCollisionRect2d>, TileCollisionExtractError> {
    let mut rectangles = Vec::new();
    let mut query = world.query::<(Entity, &TileMap2d, &TileCollision2d)>();
    for (entity, map, collision) in query.iter(world) {
        for row in 0..map.grid[1] {
            for column in 0..map.grid[0] {
                let index =
                    usize::try_from(u64::from(row) * u64::from(map.grid[0]) + u64::from(column))
                        .unwrap_or(usize::MAX);
                if collision.is_solid(index) {
                    if rectangles.len() == limits.max_rectangles {
                        return Err(TileCollisionExtractError::LimitExceeded {
                            maximum: limits.max_rectangles,
                        });
                    }
                    rectangles.push(TileCollisionRect2d {
                        entity,
                        column,
                        row,
                        origin: [
                            map.position[0] + column as f32 * map.tile_size[0],
                            map.position[1] + row as f32 * map.tile_size[1],
                        ],
                        size: map.tile_size,
                    });
                }
            }
        }
    }
    rectangles.sort_by_key(|rect| (rect.entity.to_bits(), rect.row, rect.column));
    Ok(rectangles)
}

/// Bounded policy shared by tile-collision conversion and kinematic movement.
///
/// The same non-zero maximum is enforced while extracting tile rectangles,
/// converting a caller-provided snapshot, and by the physics resolver. This
/// prevents the adapter from silently making either an unbounded temporary
/// collider list or an oversized physics call.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TileKinematicAabbLimits2d {
    physics: KinematicAabbMoveLimits2d,
}

impl TileKinematicAabbLimits2d {
    /// Creates a positive maximum static-tile collider count.
    ///
    /// # Errors
    ///
    /// Returns [`PhysicsConfigError::InvalidKinematicColliderLimit`] when
    /// `max_static_colliders` is zero.
    pub fn new(max_static_colliders: usize) -> Result<Self, PhysicsConfigError> {
        Ok(Self {
            physics: KinematicAabbMoveLimits2d::new(max_static_colliders)?,
        })
    }

    /// Returns the maximum solid tiles accepted for one movement query.
    #[must_use]
    pub const fn max_static_colliders(self) -> usize {
        self.physics.max_static_colliders()
    }
}

/// One source tile paired with its validated static physics collider.
///
/// Collider keys are deterministic snapshot positions, starting at zero. They
/// are internal to this adapter; use [`TileStaticCollider2d::source`] or
/// [`TileKinematicAabbContact2d`] instead of persisting those keys.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TileStaticCollider2d {
    source: TileCollisionRect2d,
    collider: StaticAabb2d,
}

impl TileStaticCollider2d {
    /// Returns the tile rectangle that authored this static collider.
    #[must_use]
    pub const fn source(self) -> TileCollisionRect2d {
        self.source
    }

    /// Returns the validated generic physics collider.
    ///
    /// This allows a caller to compose its own [`StaticAabb2d`] query without
    /// exposing tile-map knowledge to `yuyib-physics`.
    #[must_use]
    pub const fn collider(self) -> StaticAabb2d {
        self.collider
    }
}

/// Completed kinematic movement against a tile-collision snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct TileKinematicAabbMove2d {
    /// Final finite world-space centre after deterministic X then Y sweeps.
    pub final_center: Vec2,
    /// Actual finite displacement after tile blocking.
    pub applied_delta: Vec2,
    contacts: Vec<TileKinematicAabbContact2d>,
}

impl TileKinematicAabbMove2d {
    /// Returns contacts in physics X-sweep then Y-sweep order.
    #[must_use]
    pub fn contacts(&self) -> &[TileKinematicAabbContact2d] {
        &self.contacts
    }
}

/// One tile that blocked a kinematic AABB axis sweep.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TileKinematicAabbContact2d {
    /// Source solid tile, including its owner entity and row/column.
    pub tile: TileCollisionRect2d,
    /// Outward obstacle-face normal from the physics resolver.
    pub normal: Vec2,
}

/// Failure while adapting tile collision to a kinematic AABB movement query.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TileKinematicAabbError2d {
    /// Tile extraction exceeded the configured collider maximum.
    Extraction(TileCollisionExtractError),
    /// A caller-provided collision snapshot exceeds the configured maximum.
    SnapshotLimitExceeded {
        /// Maximum accepted static colliders.
        maximum: usize,
        /// Number of supplied collision rectangles.
        actual: usize,
    },
    /// Tile dimensions, position, or world bounds cannot produce a physics AABB.
    InvalidTileCollider {
        /// Tile-map entity which produced the invalid rectangle.
        entity: Entity,
        /// Source tile column.
        column: u32,
        /// Source tile row.
        row: u32,
        /// Underlying finite-AABB validation failure.
        source: PhysicsConfigError,
    },
    /// This platform cannot represent a bounded snapshot index as a physics key.
    ColliderKeyOverflow {
        /// Snapshot index that could not become a `u64` key.
        index: usize,
    },
    /// The generic kinematic resolver rejected otherwise valid adapter input.
    Physics(KinematicAabbMoveError),
    /// The resolver returned a key absent from the adapter's collider snapshot.
    ///
    /// This is a defensive invariant error; current `yuyib-physics` only
    /// returns keys supplied in the input slice.
    UnknownColliderKey(u64),
}

impl std::fmt::Display for TileKinematicAabbError2d {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Extraction(error) => {
                write!(formatter, "tile collision extraction failed: {error}")
            }
            Self::SnapshotLimitExceeded { maximum, actual } => write!(
                formatter,
                "tile collision snapshot limit {maximum} exceeded by {actual} rectangles"
            ),
            Self::InvalidTileCollider {
                entity,
                column,
                row,
                source,
            } => write!(
                formatter,
                "tile collider for entity {entity:?}, row {row}, column {column} is invalid: {source}"
            ),
            Self::ColliderKeyOverflow { index } => {
                write!(
                    formatter,
                    "tile collider index {index} cannot become a physics key"
                )
            }
            Self::Physics(error) => write!(formatter, "tile kinematic movement failed: {error}"),
            Self::UnknownColliderKey(key) => {
                write!(
                    formatter,
                    "physics returned unknown tile collider key {key}"
                )
            }
        }
    }
}

impl std::error::Error for TileKinematicAabbError2d {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Extraction(error) => Some(error),
            Self::InvalidTileCollider { source, .. } => Some(source),
            Self::Physics(error) => Some(error),
            Self::SnapshotLimitExceeded { .. }
            | Self::ColliderKeyOverflow { .. }
            | Self::UnknownColliderKey(_) => None,
        }
    }
}

/// Converts an already-extracted tile collision snapshot into static AABBs.
///
/// Snapshot order is retained exactly, so generic [`StaticAabb2d::key`] values
/// are deterministic contiguous snapshot indices. This is the low-level
/// boundary for callers which cache, spatially filter, or otherwise own the
/// tile snapshot themselves.
///
/// # Errors
///
/// Returns an explicit limit or tile-geometry error before passing invalid data
/// to physics.
pub fn build_tile_static_colliders_2d(
    rectangles: &[TileCollisionRect2d],
    limits: TileKinematicAabbLimits2d,
) -> Result<Vec<TileStaticCollider2d>, TileKinematicAabbError2d> {
    if rectangles.len() > limits.max_static_colliders() {
        return Err(TileKinematicAabbError2d::SnapshotLimitExceeded {
            maximum: limits.max_static_colliders(),
            actual: rectangles.len(),
        });
    }

    rectangles
        .iter()
        .copied()
        .enumerate()
        .map(|(index, source)| {
            let key = u64::try_from(index)
                .map_err(|_| TileKinematicAabbError2d::ColliderKeyOverflow { index })?;
            let half_extents = Vec2::new(source.size[0] * 0.5, source.size[1] * 0.5);
            let center = Vec2::new(
                source.origin[0] + half_extents.x,
                source.origin[1] + half_extents.y,
            );
            let aabb = Aabb2d::new(half_extents).map_err(|source_error| {
                TileKinematicAabbError2d::InvalidTileCollider {
                    entity: source.entity,
                    column: source.column,
                    row: source.row,
                    source: source_error,
                }
            })?;
            let collider = StaticAabb2d::new(key, center, aabb).map_err(|source_error| {
                TileKinematicAabbError2d::InvalidTileCollider {
                    entity: source.entity,
                    column: source.column,
                    row: source.row,
                    source: source_error,
                }
            })?;
            Ok(TileStaticCollider2d { source, collider })
        })
        .collect()
}

/// Extracts a bounded tile snapshot and resolves one kinematic AABB against it.
///
/// This high-level adapter owns no persistent physics state: it takes the
/// current `TileMap2d`/`TileCollision2d` ECS data, builds immutable static
/// boxes, executes the generic physics resolver, and maps contacts back to
/// source tiles. It never adds physics components to map entities or makes
/// `yuyib-physics` depend on tile maps.
///
/// # Errors
///
/// Returns extraction, tile-conversion, or generic kinematic errors explicitly.
/// There is no depenetration policy for a mover starting inside a solid tile.
pub fn resolve_kinematic_tilemap_aabb_2d(
    world: &mut World,
    center: Vec2,
    aabb: Aabb2d,
    desired_delta: Vec2,
    limits: TileKinematicAabbLimits2d,
) -> Result<TileKinematicAabbMove2d, TileKinematicAabbError2d> {
    let rectangles = extract_tile_collisions_2d(
        world,
        TileCollisionLimits {
            max_rectangles: limits.max_static_colliders(),
        },
    )
    .map_err(TileKinematicAabbError2d::Extraction)?;
    let colliders = build_tile_static_colliders_2d(&rectangles, limits)?;
    let physics_colliders: Vec<_> = colliders
        .iter()
        .map(|collider| collider.collider())
        .collect();
    let movement = resolve_kinematic_aabb_2d(
        center,
        aabb,
        desired_delta,
        &physics_colliders,
        limits.physics,
    )
    .map_err(TileKinematicAabbError2d::Physics)?;
    map_tile_kinematic_move(&movement, &colliders)
}

/// High-level kinematic controller for a visible [`Sprite2d`].
///
/// Add this component next to a sprite and call
/// [`step_kinematic_sprite_controller_2d`] once per simulation tick. It keeps
/// the rendered sprite centre and its collision centre identical, normalises a
/// diagonal input axis, and delegates the actual collision query to the
/// bounded tile-map adapter.
///
/// This is intentionally a controller for top-down games. It does not add
/// gravity, slopes, moving platforms or a hidden physics world. Games needing
/// those policies should use [`resolve_kinematic_tilemap_aabb_2d`] or the raw
/// `yuyib-physics` solver directly.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct KinematicSpriteController2d {
    collider: Aabb2d,
    speed: f32,
}

impl KinematicSpriteController2d {
    /// Creates a controller from the visible sprite's full world-space size.
    ///
    /// `speed` is measured in world units per second. The controller does not
    /// automatically resize a sprite: keeping its visual size and collision
    /// size separate is useful for characters with transparent padding.
    ///
    /// # Errors
    ///
    /// Returns [`KinematicSpriteControllerError2d::InvalidSpeed`] for a
    /// non-finite or negative speed, and wraps the physics validation error
    /// when `size` cannot form a finite, positive AABB.
    pub fn new(size: [f32; 2], speed: f32) -> Result<Self, KinematicSpriteControllerError2d> {
        if !speed.is_finite() || speed < 0.0 {
            return Err(KinematicSpriteControllerError2d::InvalidSpeed(speed));
        }
        let collider = Aabb2d::new(Vec2::new(size[0] * 0.5, size[1] * 0.5))
            .map_err(KinematicSpriteControllerError2d::InvalidCollider)?;
        Ok(Self { collider, speed })
    }

    /// Creates a controller from a previously validated low-level collider.
    ///
    /// This is the escape hatch for games that author the collision box
    /// independently of the sprite's visible rectangle.
    ///
    /// # Errors
    ///
    /// Returns [`KinematicSpriteControllerError2d::InvalidSpeed`] for a
    /// non-finite or negative speed.
    pub fn from_collider(
        collider: Aabb2d,
        speed: f32,
    ) -> Result<Self, KinematicSpriteControllerError2d> {
        if !speed.is_finite() || speed < 0.0 {
            return Err(KinematicSpriteControllerError2d::InvalidSpeed(speed));
        }
        Ok(Self { collider, speed })
    }

    /// Returns the low-level collision AABB.
    #[must_use]
    pub const fn collider(self) -> Aabb2d {
        self.collider
    }

    /// Returns movement speed in world units per second.
    #[must_use]
    pub const fn speed(self) -> f32 {
        self.speed
    }
}

/// Validated semantic movement input for [`KinematicSpriteController2d`].
///
/// `axis` convention is right/down: `[1, 0]` moves right and `[0, 1]` moves
/// down, matching [`Sprite2d`] and tile-map coordinates. The controller
/// normalises diagonals, so holding two keys cannot make a character faster.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpriteMoveInput2d {
    axis: Vec2,
}

impl SpriteMoveInput2d {
    /// Creates finite semantic movement input.
    ///
    /// # Errors
    ///
    /// Returns [`KinematicSpriteControllerError2d::InvalidInput`] for `NaN`
    /// or infinite axes.
    pub fn new(axis: [f32; 2]) -> Result<Self, KinematicSpriteControllerError2d> {
        if !axis.iter().all(|value| value.is_finite()) {
            return Err(KinematicSpriteControllerError2d::InvalidInput(axis));
        }
        Ok(Self {
            axis: Vec2::new(axis[0], axis[1]),
        })
    }

    /// Creates a zero movement input.
    #[must_use]
    pub const fn idle() -> Self {
        Self { axis: Vec2::ZERO }
    }

    /// Returns the authored, pre-normalisation input axis.
    #[must_use]
    pub const fn axis(self) -> Vec2 {
        self.axis
    }
}

/// Completed high-level sprite-controller step.
#[derive(Clone, Debug, PartialEq)]
pub struct KinematicSpriteMove2d {
    /// Entity that owns the moved [`Sprite2d`] and controller.
    pub actor: Entity,
    /// Completed bounded tile collision query.
    pub movement: TileKinematicAabbMove2d,
}

/// Failure while stepping [`KinematicSpriteController2d`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum KinematicSpriteControllerError2d {
    /// Full visual/collision size could not form a valid low-level AABB.
    InvalidCollider(PhysicsConfigError),
    /// Speed must be finite and non-negative.
    InvalidSpeed(f32),
    /// A semantic input axis contained `NaN` or infinity.
    InvalidInput([f32; 2]),
    /// The supplied frame duration cannot be represented as finite `f32` seconds.
    InvalidDelta,
    /// The actor has no [`Sprite2d`] component.
    MissingSprite(Entity),
    /// The actor has no [`KinematicSpriteController2d`] component.
    MissingController(Entity),
    /// Tile extraction, collider conversion, or kinematic physics failed.
    TileCollision(TileKinematicAabbError2d),
}

impl std::fmt::Display for KinematicSpriteControllerError2d {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidCollider(error) => write!(formatter, "invalid sprite collider: {error}"),
            Self::InvalidSpeed(speed) => write!(
                formatter,
                "sprite speed must be finite and non-negative, got {speed}"
            ),
            Self::InvalidInput(axis) => write!(
                formatter,
                "sprite movement input must be finite, got ({}, {})",
                axis[0], axis[1]
            ),
            Self::InvalidDelta => formatter
                .write_str("sprite movement delta cannot be represented as finite f32 seconds"),
            Self::MissingSprite(entity) => write!(
                formatter,
                "sprite controller actor {entity:?} has no Sprite2d"
            ),
            Self::MissingController(entity) => write!(
                formatter,
                "sprite controller actor {entity:?} has no KinematicSpriteController2d"
            ),
            Self::TileCollision(error) => {
                write!(formatter, "sprite tile collision failed: {error}")
            }
        }
    }
}

impl std::error::Error for KinematicSpriteControllerError2d {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidCollider(error) => Some(error),
            Self::TileCollision(error) => Some(error),
            Self::InvalidSpeed(_)
            | Self::InvalidInput(_)
            | Self::InvalidDelta
            | Self::MissingSprite(_)
            | Self::MissingController(_) => None,
        }
    }
}

/// Moves a sprite-controller entity through all static tile collision layers.
///
/// This is the high-level playable-2D entry point. It reads the actor's
/// [`Sprite2d`] and [`KinematicSpriteController2d`] components, applies
/// `input × speed × delta`, resolves collision, and writes the final position
/// back to the sprite. It has no wall clock, keyboard binding or camera policy:
/// applications own those choices and submit semantic [`SpriteMoveInput2d`].
///
/// For a custom collision snapshot, a spatial cache, or a non-sprite actor,
/// call [`resolve_kinematic_tilemap_aabb_2d`] (medium level) or
/// `yuyib_physics::resolve_kinematic_aabb_2d` (low level) directly.
///
/// # Errors
///
/// Returns [`KinematicSpriteControllerError2d`] when the actor lacks either
/// required component, input/delta are invalid, or the bounded tile collision
/// adapter rejects the current map or movement query.
pub fn step_kinematic_sprite_controller_2d(
    world: &mut World,
    actor: Entity,
    input: SpriteMoveInput2d,
    delta: Duration,
    limits: TileKinematicAabbLimits2d,
) -> Result<KinematicSpriteMove2d, KinematicSpriteControllerError2d> {
    let sprite = world
        .get::<Sprite2d>(actor)
        .copied()
        .ok_or(KinematicSpriteControllerError2d::MissingSprite(actor))?;
    let controller = world
        .get::<KinematicSpriteController2d>(actor)
        .copied()
        .ok_or(KinematicSpriteControllerError2d::MissingController(actor))?;
    let seconds = delta.as_secs_f32();
    if !seconds.is_finite() {
        return Err(KinematicSpriteControllerError2d::InvalidDelta);
    }
    let direction = input.axis.normalized_or_zero();
    let desired_delta = Vec2::new(
        direction.x * controller.speed * seconds,
        direction.y * controller.speed * seconds,
    );
    let movement = resolve_kinematic_tilemap_aabb_2d(
        world,
        Vec2::new(sprite.position[0], sprite.position[1]),
        controller.collider,
        desired_delta,
        limits,
    )
    .map_err(KinematicSpriteControllerError2d::TileCollision)?;
    let mut sprite = world
        .get_mut::<Sprite2d>(actor)
        .ok_or(KinematicSpriteControllerError2d::MissingSprite(actor))?;
    sprite.position = [movement.final_center.x, movement.final_center.y];
    Ok(KinematicSpriteMove2d { actor, movement })
}

fn map_tile_kinematic_move(
    movement: &KinematicAabbMove2d,
    colliders: &[TileStaticCollider2d],
) -> Result<TileKinematicAabbMove2d, TileKinematicAabbError2d> {
    let contacts = movement
        .contacts()
        .iter()
        .map(|contact| {
            let index = usize::try_from(contact.collider_key)
                .map_err(|_| TileKinematicAabbError2d::UnknownColliderKey(contact.collider_key))?;
            let tile = colliders
                .get(index)
                .ok_or(TileKinematicAabbError2d::UnknownColliderKey(
                    contact.collider_key,
                ))?
                .source();
            Ok(TileKinematicAabbContact2d {
                tile,
                normal: contact.normal,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(TileKinematicAabbMove2d {
        final_center: movement.final_center,
        applied_delta: movement.applied_delta,
        contacts,
    })
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // Inputs are exactly representable; assertions cover ordering only.
mod tests {
    use std::time::Duration;

    use yuyib_2d::{
        PixelPoint, PlaybackMode, SpriteAnimation, Texture, TextureRegion, TextureSize,
    };
    use yuyib_assets::Assets;

    use super::*;

    fn region(textures: &mut Assets<Texture>) -> TextureRegion {
        let size = TextureSize::new(16, 16).expect("valid test texture size");
        let texture = textures.insert(Texture::new(size));
        TextureRegion::new(texture, size, PixelPoint::default(), size)
            .expect("full-texture region is valid")
    }

    #[test]
    fn animation_step_updates_atlas_or_standalone_region_and_orders_events() {
        let mut textures = Assets::new();
        let first = region(&mut textures);
        let second = region(&mut textures);
        let animation = SpriteAnimation::from_regions(
            &[first, second],
            Duration::from_millis(10),
            PlaybackMode::Loop,
        )
        .expect("valid animation");
        let mut world = World::new();
        let entity = world
            .spawn((Sprite2d::new(first), AnimatedSprite2d::new(animation)))
            .id();
        let events = step_sprite_animations_2d(&mut world, Duration::from_millis(10));
        assert!(
            matches!(events.as_slice(), [SpriteAnimationEvent2d::FrameChanged { entity: observed }] if *observed == entity)
        );
        assert_eq!(
            world.get::<Sprite2d>(entity).expect("sprite").region,
            second
        );
    }

    #[test]
    fn extraction_sorts_by_layer_and_keeps_same_layer_entity_order() {
        let mut textures = Assets::new();
        let region = region(&mut textures);
        let mut world = World::new();
        let first = world
            .spawn(
                Sprite2d::new(region)
                    .with_position([20.0, 0.0])
                    .with_layer(1),
            )
            .id();
        world.spawn(
            Sprite2d::new(region)
                .with_position([10.0, 0.0])
                .with_layer(0),
        );
        let last = world
            .spawn(
                Sprite2d::new(region)
                    .with_position([30.0, 0.0])
                    .with_layer(1),
            )
            .id();

        let extracted = extract_sprites(&mut world);
        assert_eq!(extracted.sprite_count(), 3);
        assert_eq!(extracted.batches().len(), 1);
        let draws = extracted.batches()[0].draws();
        let mut expected = [
            (first.to_bits(), [20.0, 0.0]),
            (last.to_bits(), [30.0, 0.0]),
        ];
        expected.sort_by_key(|(entity_bits, _)| *entity_bits);

        assert_eq!(draws[0].position, [10.0, 0.0]);
        assert_eq!(draws[1].position, expected[0].1);
        assert_eq!(draws[2].position, expected[1].1);
    }

    #[test]
    fn extraction_splits_non_adjacent_textures_to_preserve_painter_order() {
        let mut textures = Assets::new();
        let first = region(&mut textures);
        let second = region(&mut textures);
        let mut world = World::new();
        world.spawn(Sprite2d::new(first).with_layer(0));
        world.spawn(Sprite2d::new(second).with_layer(1));
        world.spawn(Sprite2d::new(first).with_layer(2));

        let extracted = extract_sprites(&mut world);
        let batches = extracted.batches();
        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0].texture(), first.texture());
        assert_eq!(batches[1].texture(), second.texture());
        assert_eq!(batches[2].texture(), first.texture());
    }

    #[test]
    fn empty_world_produces_no_batches() {
        let mut world = World::new();
        let extracted = extract_sprites(&mut world);

        assert!(extracted.is_empty());
        assert!(extracted.batches().is_empty());
    }

    #[test]
    fn visible_sprite_culling_excludes_edge_touching_and_keeps_overlap() {
        let mut textures = Assets::new();
        let texture = region(&mut textures);
        let viewport = SpriteViewport2d::new([0.0, 0.0], [10.0, 10.0]).expect("viewport");
        let mut world = World::new();
        world.spawn(Sprite2d::new(texture).with_position([-0.5, 5.0]));
        world.spawn(Sprite2d::new(texture).with_position([10.5, 5.0]));
        world.spawn(Sprite2d::new(texture).with_position([5.0, -0.5]));
        world.spawn(Sprite2d::new(texture).with_position([5.0, 10.5]));
        world.spawn(Sprite2d::new(texture).with_position([0.0, 5.0]));

        let extracted = extract_visible_sprites_2d(
            &mut world,
            viewport,
            SpriteExtractionLimits2d::new(8).expect("positive limit"),
        )
        .expect("finite sprite geometry");

        assert_eq!(extracted.sprite_count(), 1);
        assert_eq!(extracted.batches()[0].draws()[0].position, [0.0, 5.0]);
    }

    #[test]
    fn visible_sprite_culling_uses_conservative_rotated_aabb_and_mirrored_size() {
        let mut textures = Assets::new();
        let texture = region(&mut textures);
        let viewport = SpriteViewport2d::new([0.0, 0.0], [10.0, 10.0]).expect("viewport");
        let mut world = World::new();
        world.spawn(
            Sprite2d::new(texture)
                .with_position([12.0, 5.0])
                .with_size([2.0, 8.0])
                .with_rotation(std::f32::consts::FRAC_PI_2),
        );
        world.spawn(
            Sprite2d::new(texture)
                .with_position([10.5, 5.0])
                .with_size([-2.0, -2.0]),
        );

        let extracted =
            extract_visible_sprites_2d(&mut world, viewport, SpriteExtractionLimits2d::default())
                .expect("rotated and mirrored finite sprites");

        assert_eq!(extracted.sprite_count(), 2);
    }

    #[test]
    fn visible_sprite_culling_preserves_global_order_and_adjacent_texture_batches() {
        let mut textures = Assets::new();
        let first_texture = region(&mut textures);
        let second_texture = region(&mut textures);
        let mut world = World::new();
        world.spawn(
            Sprite2d::new(first_texture)
                .with_position([1.0, 1.0])
                .with_layer(2),
        );
        world.spawn(
            Sprite2d::new(second_texture)
                .with_position([2.0, 1.0])
                .with_layer(1),
        );
        world.spawn(
            Sprite2d::new(first_texture)
                .with_position([3.0, 1.0])
                .with_layer(0),
        );
        world.spawn(
            Sprite2d::new(first_texture)
                .with_position([4.0, 1.0])
                .with_layer(0),
        );

        let extracted = extract_visible_sprites_2d(
            &mut world,
            SpriteViewport2d::new([0.0, 0.0], [10.0, 10.0]).expect("viewport"),
            SpriteExtractionLimits2d::default(),
        )
        .expect("finite sprites");

        let batches = extracted.batches();
        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0].texture(), first_texture.texture());
        assert_eq!(batches[1].texture(), second_texture.texture());
        assert_eq!(batches[2].texture(), first_texture.texture());
        assert_eq!(batches[0].len(), 2);
        assert_eq!(batches[0].draws()[0].layer, 0);
        assert_eq!(batches[0].draws()[1].layer, 0);
        assert_eq!(batches[1].draws()[0].layer, 1);
        assert_eq!(batches[2].draws()[0].layer, 2);
    }

    #[test]
    fn visible_sprite_culling_validates_limits_and_stops_before_limit_overflow() {
        assert_eq!(
            SpriteExtractionLimits2d::new(0),
            Err(SpriteExtractionLimitsError::ZeroVisibleSpriteLimit)
        );

        let mut textures = Assets::new();
        let texture = region(&mut textures);
        let mut world = World::new();
        world.spawn(Sprite2d::new(texture).with_position([1.0, 1.0]));
        world.spawn(Sprite2d::new(texture).with_position([2.0, 1.0]));
        let result = extract_visible_sprites_2d(
            &mut world,
            SpriteViewport2d::new([0.0, 0.0], [10.0, 10.0]).expect("viewport"),
            SpriteExtractionLimits2d::new(1).expect("positive limit"),
        );

        assert_eq!(
            result,
            Err(VisibleSpriteExtractError::VisibleSpriteLimitExceeded { maximum: 1 })
        );
    }

    #[test]
    fn visible_sprite_culling_reports_invalid_viewport_and_sprite_geometry() {
        assert_eq!(
            SpriteViewport2d::new([f32::NAN, 0.0], [1.0, 1.0]),
            Err(SpriteViewportError::NonFiniteOrigin)
        );
        assert_eq!(
            SpriteViewport2d::new([0.0, 0.0], [0.0, 1.0]),
            Err(SpriteViewportError::InvalidSize)
        );
        assert_eq!(
            SpriteViewport2d::new([f32::MAX, 0.0], [f32::MAX, 1.0]),
            Err(SpriteViewportError::EndOverflow)
        );

        let mut textures = Assets::new();
        let texture = region(&mut textures);
        let viewport = SpriteViewport2d::new([0.0, 0.0], [10.0, 10.0]).expect("viewport");
        for sprite in [
            Sprite2d::new(texture).with_position([f32::NAN, 0.0]),
            Sprite2d::new(texture).with_size([f32::INFINITY, 1.0]),
            Sprite2d::new(texture).with_rotation(f32::NAN),
            Sprite2d::new(texture)
                .with_position([f32::MAX, 0.0])
                .with_size([f32::MAX, 1.0]),
        ] {
            let mut world = World::new();
            let entity = world.spawn(sprite).id();
            assert_eq!(
                extract_visible_sprites_2d(
                    &mut world,
                    viewport,
                    SpriteExtractionLimits2d::default(),
                ),
                Err(VisibleSpriteExtractError::InvalidSpriteGeometry { entity })
            );
        }
    }

    #[test]
    fn tile_map_culls_and_validates_bounds() {
        let mut textures = Assets::new();
        let atlas = region(&mut textures);
        assert!(matches!(
            TileMap2d::new([0, 1], [1.0, 1.0], vec![atlas], vec![]),
            Err(TileMapError::ZeroGrid)
        ));
        assert!(matches!(
            TileMap2d::new([1, 1], [1.0, 1.0], vec![atlas], vec![Some(1)]),
            Err(TileMapError::InvalidTileIndex)
        ));
        let map = TileMap2d::new([2, 1], [10.0, 10.0], vec![atlas], vec![Some(0), Some(0)])
            .expect("valid");
        let mut world = World::new();
        world.spawn(map);
        let extracted = extract_tiles_2d(
            &mut world,
            TileViewport2d::new([0.0, 0.0], [10.0, 10.0]).expect("viewport"),
        );
        assert_eq!(extracted.len(), 1);
        assert_eq!(extracted.draws()[0].position, [5.0, 5.0]);
    }

    #[test]
    fn chunked_tiles_cross_chunk_boundaries_without_extracting_outside_viewport() {
        let mut textures = Assets::new();
        let atlas = region(&mut textures);
        let map =
            TileMap2d::new([6, 1], [10.0, 10.0], vec![atlas], vec![Some(0); 6]).expect("valid map");
        let mut world = World::new();
        world.spawn(map);

        let extracted = extract_tiles_chunked_2d(
            &mut world,
            TileViewport2d::new([15.0, 0.0], [10.0, 10.0]).expect("viewport"),
            TileChunkConfig2d::new([2, 1], 8).expect("valid chunks"),
        )
        .expect("visible draws stay under bound");

        let positions: Vec<_> = extracted.draws().iter().map(|draw| draw.position).collect();
        assert_eq!(positions, vec![[15.0, 5.0], [25.0, 5.0]]);
    }

    #[test]
    fn chunked_tiles_keep_global_painter_order() {
        let mut textures = Assets::new();
        let atlas = region(&mut textures);
        let mut world = World::new();
        world.spawn(
            TileMap2d::new([1, 1], [10.0, 10.0], vec![atlas], vec![Some(0)])
                .expect("valid map")
                .with_position([30.0, 0.0])
                .with_layer(4),
        );
        world.spawn(
            TileMap2d::new([2, 1], [10.0, 10.0], vec![atlas], vec![Some(0), Some(0)])
                .expect("valid map")
                .with_position([0.0, 0.0])
                .with_layer(-1),
        );

        let extracted = extract_tiles_chunked_2d(
            &mut world,
            TileViewport2d::new([0.0, 0.0], [40.0, 10.0]).expect("viewport"),
            TileChunkConfig2d::new([1, 1], 8).expect("valid chunks"),
        )
        .expect("visible draws stay under bound");

        let positions: Vec<_> = extracted.draws().iter().map(|draw| draw.position).collect();
        assert_eq!(positions, vec![[5.0, 5.0], [15.0, 5.0], [35.0, 5.0]]);
        assert_eq!(extracted.draws()[0].layer, -1);
        assert_eq!(extracted.draws()[2].layer, 4);
    }

    #[test]
    fn chunk_config_and_draw_limit_are_validated() {
        assert!(matches!(
            TileChunkConfig2d::new([0, 1], 1),
            Err(TileChunkConfigError::ZeroChunkSize)
        ));
        assert!(matches!(
            TileChunkConfig2d::new([1, 1], 0),
            Err(TileChunkConfigError::ZeroDrawLimit)
        ));

        let mut textures = Assets::new();
        let atlas = region(&mut textures);
        let mut world = World::new();
        world.spawn(
            TileMap2d::new([2, 1], [10.0, 10.0], vec![atlas], vec![Some(0), Some(0)])
                .expect("valid map"),
        );
        let result = extract_tiles_chunked_2d(
            &mut world,
            TileViewport2d::new([0.0, 0.0], [20.0, 10.0]).expect("viewport"),
            TileChunkConfig2d::new([1, 1], 1).expect("valid chunks"),
        );
        assert_eq!(
            result,
            Err(TileChunkExtractError::DrawLimitExceeded { maximum: 1 })
        );
    }

    #[test]
    fn tile_kinematic_adapter_resolves_and_maps_contact_to_source_tile() {
        let mut textures = Assets::new();
        let atlas = region(&mut textures);
        let mut world = World::new();
        let map_entity = world
            .spawn((
                TileMap2d::new([2, 1], [10.0, 10.0], vec![atlas], vec![Some(0), Some(0)])
                    .expect("valid map"),
                TileCollision2d::new([2, 1], vec![false, true]).expect("matching collision cells"),
            ))
            .id();
        let mover = Aabb2d::new(Vec2::new(1.0, 1.0)).expect("valid mover");

        let movement = resolve_kinematic_tilemap_aabb_2d(
            &mut world,
            Vec2::new(2.0, 5.0),
            mover,
            Vec2::new(20.0, 0.0),
            TileKinematicAabbLimits2d::new(4).expect("positive bound"),
        )
        .expect("wall movement must resolve");

        assert_eq!(movement.final_center, Vec2::new(8.5, 5.0));
        assert_eq!(movement.applied_delta, Vec2::new(6.5, 0.0));
        assert_eq!(
            movement.contacts(),
            &[TileKinematicAabbContact2d {
                tile: TileCollisionRect2d {
                    entity: map_entity,
                    column: 1,
                    row: 0,
                    origin: [10.0, 0.0],
                    size: [10.0, 10.0],
                },
                normal: Vec2::new(-1.0, 0.0),
            }]
        );
    }

    #[test]
    fn tile_static_adapter_keeps_snapshot_order_and_bounds_work() {
        let mut world = World::new();
        let first_entity = world.spawn_empty().id();
        let second_entity = world.spawn_empty().id();
        let rectangles = vec![
            TileCollisionRect2d {
                entity: first_entity,
                column: 2,
                row: 4,
                origin: [20.0, 40.0],
                size: [10.0, 20.0],
            },
            TileCollisionRect2d {
                entity: second_entity,
                column: 0,
                row: 0,
                origin: [-10.0, -10.0],
                size: [2.0, 4.0],
            },
        ];
        let limits = TileKinematicAabbLimits2d::new(2).expect("positive bound");

        let colliders =
            build_tile_static_colliders_2d(&rectangles, limits).expect("valid snapshot");
        assert_eq!(colliders[0].source(), rectangles[0]);
        assert_eq!(colliders[0].collider().key(), 0);
        assert_eq!(colliders[0].collider().center(), Vec2::new(25.0, 50.0));
        assert_eq!(colliders[1].source(), rectangles[1]);
        assert_eq!(colliders[1].collider().key(), 1);
        assert_eq!(colliders[1].collider().center(), Vec2::new(-9.0, -8.0));

        assert_eq!(
            build_tile_static_colliders_2d(
                &rectangles,
                TileKinematicAabbLimits2d::new(1).expect("positive")
            ),
            Err(TileKinematicAabbError2d::SnapshotLimitExceeded {
                maximum: 1,
                actual: 2,
            })
        );
    }

    #[test]
    fn tile_kinematic_adapter_has_explicit_extraction_and_geometry_errors() {
        assert!(matches!(
            TileKinematicAabbLimits2d::new(0),
            Err(PhysicsConfigError::InvalidKinematicColliderLimit(0))
        ));

        let mut textures = Assets::new();
        let atlas = region(&mut textures);
        let mut world = World::new();
        world.spawn((
            TileMap2d::new([2, 1], [10.0, 10.0], vec![atlas], vec![Some(0), Some(0)])
                .expect("valid map"),
            TileCollision2d::new([2, 1], vec![true, true]).expect("matching collision cells"),
        ));
        let mover = Aabb2d::new(Vec2::new(1.0, 1.0)).expect("valid mover");
        assert_eq!(
            resolve_kinematic_tilemap_aabb_2d(
                &mut world,
                Vec2::ZERO,
                mover,
                Vec2::ZERO,
                TileKinematicAabbLimits2d::new(1).expect("positive"),
            ),
            Err(TileKinematicAabbError2d::Extraction(
                TileCollisionExtractError::LimitExceeded { maximum: 1 }
            ))
        );

        let entity = world.spawn_empty().id();
        let invalid = [TileCollisionRect2d {
            entity,
            column: 0,
            row: 0,
            origin: [0.0, 0.0],
            size: [0.0, 10.0],
        }];
        assert!(matches!(
            build_tile_static_colliders_2d(
                &invalid,
                TileKinematicAabbLimits2d::new(1).expect("positive"),
            ),
            Err(TileKinematicAabbError2d::InvalidTileCollider {
                source: PhysicsConfigError::InvalidAabb2dHalfExtents(_),
                ..
            })
        ));
    }

    #[test]
    fn sprite_controller_moves_visible_sprite_and_normalizes_diagonal_input() {
        let mut textures = Assets::new();
        let atlas = region(&mut textures);
        let mut world = World::new();
        let mut solid = vec![false; 25];
        for row in 0..5 {
            for column in 0..5 {
                if row == 0 || row == 4 || column == 0 || column == 4 {
                    solid[row * 5 + column] = true;
                }
            }
        }
        world.spawn((
            TileMap2d::new([5, 5], [10.0, 10.0], vec![atlas], vec![Some(0); 25])
                .expect("valid map"),
            TileCollision2d::new([5, 5], solid).expect("matching cells"),
        ));
        let actor = world
            .spawn((
                Sprite2d::new(atlas).with_position([25.0, 25.0]),
                KinematicSpriteController2d::new([4.0, 4.0], 10.0).expect("valid controller"),
            ))
            .id();

        let move_result = step_kinematic_sprite_controller_2d(
            &mut world,
            actor,
            SpriteMoveInput2d::new([1.0, 1.0]).expect("finite input"),
            Duration::from_secs(1),
            TileKinematicAabbLimits2d::new(32).expect("positive bound"),
        )
        .expect("diagonal move stays in room");

        let diagonal = 10.0 * 0.5_f32.sqrt();
        assert_eq!(
            move_result.movement.applied_delta,
            Vec2::new(diagonal, diagonal)
        );
        assert_eq!(
            world.get::<Sprite2d>(actor).expect("sprite").position,
            [25.0 + diagonal, 25.0 + diagonal]
        );
    }

    #[test]
    fn sprite_controller_reports_missing_components_and_blocks_at_wall() {
        let mut textures = Assets::new();
        let atlas = region(&mut textures);
        let mut world = World::new();
        let map =
            TileMap2d::new([2, 1], [10.0, 10.0], vec![atlas], vec![Some(0); 2]).expect("valid map");
        world.spawn((
            map,
            TileCollision2d::new([2, 1], vec![false, true]).expect("matching collision"),
        ));
        let actor = world
            .spawn((
                Sprite2d::new(atlas).with_position([5.0, 5.0]),
                KinematicSpriteController2d::new([2.0, 2.0], 20.0).expect("valid controller"),
            ))
            .id();
        let result = step_kinematic_sprite_controller_2d(
            &mut world,
            actor,
            SpriteMoveInput2d::new([1.0, 0.0]).expect("finite input"),
            Duration::from_secs(1),
            TileKinematicAabbLimits2d::new(4).expect("positive bound"),
        )
        .expect("wall movement resolves");
        assert_eq!(result.movement.final_center, Vec2::new(8.5, 5.0));
        assert_eq!(result.movement.contacts().len(), 1);

        let missing = world.spawn_empty().id();
        assert_eq!(
            step_kinematic_sprite_controller_2d(
                &mut world,
                missing,
                SpriteMoveInput2d::idle(),
                Duration::ZERO,
                TileKinematicAabbLimits2d::new(4).expect("positive bound"),
            ),
            Err(KinematicSpriteControllerError2d::MissingSprite(missing))
        );
    }
}
