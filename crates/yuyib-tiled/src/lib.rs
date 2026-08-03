//! Bounded Tiled JSON → [`TileMap2d`] importer (M7).
//!
//! Supports orthogonal right-down maps with one tileset (embedded **or** external
//! JSON `.tsj` via host-resolved bytes), one visual tile layer, optional
//! `collision` tile layer, and `objectgroup` layers (point/rect objects +
//! bool/string/number properties).
//!
//! Explicitly out of scope for this slice: TMX/TSX XML, multiple tileset
//! textures, hex/iso, infinite maps, flip flags, ellipse/polygon/tile objects,
//! LDtk.

#![forbid(unsafe_code)]

mod objects;
mod tileset;

pub use objects::{
    ImportedTiledObject2d, ImportedTiledObjectLayer2d, TiledPropertyValue, world_from_tiled_px,
};
pub use tileset::ExternalTilesetBytes;

use std::{error::Error, fmt, mem};

use serde::Deserialize;
use yuyib_2d::{
    PixelPoint, TextureHandle, TextureRegion, TextureRegionError, TextureSize, TextureSizeError,
};
use yuyib_assets::{
    AssetImporter, ImportContext, ImportDependency, ImportMatch, ImportProbe, ImportSource,
    ImporterDescriptor, ImporterOutput, ImporterRegistrationError, ImporterRegistry,
};
use yuyib_game_2d::{TileCollision2d, TileCollisionError, TileMap2d, TileMapError};

use tileset::{RawTileset, ResolvedTileset, resolve_map_tileset};

/// Media type advertised by the Tiled JSON map importer.
pub const TILED_MAP_MEDIA_TYPE: &str = "application/vnd.yuyib.tiled-map+json";

const FORMAT_NAME: &str = "tiled.map.json";

const FLIPPED_HORIZONTALLY: u32 = 0x8000_0000;
const FLIPPED_VERTICALLY: u32 = 0x4000_0000;
const FLIPPED_DIAGONALLY: u32 = 0x2000_0000;
const GID_MASK: u32 = !(FLIPPED_HORIZONTALLY | FLIPPED_VERTICALLY | FLIPPED_DIAGONALLY);

/// Trust boundary for one Tiled JSON document.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TiledImportLimits {
    /// Maximum JSON document size.
    pub max_manifest_bytes: usize,
    /// Maximum map width in tiles.
    pub max_width: u32,
    /// Maximum map height in tiles.
    pub max_height: u32,
    /// Maximum tileset tile count.
    pub max_tileset_tiles: u32,
    /// Maximum UTF-8 bytes in the tileset image URI.
    pub max_image_uri_bytes: usize,
    /// Maximum UTF-8 bytes in a layer name.
    pub max_layer_name_bytes: usize,
    /// Maximum objectgroup layers.
    pub max_object_layers: usize,
    /// Maximum objects per objectgroup layer.
    pub max_objects_per_layer: usize,
    /// Maximum UTF-8 bytes in an object name or class.
    pub max_object_name_bytes: usize,
    /// Maximum custom properties per object.
    pub max_properties_per_object: usize,
    /// Maximum UTF-8 bytes in a string property value.
    pub max_property_string_bytes: usize,
}

impl Default for TiledImportLimits {
    fn default() -> Self {
        Self {
            max_manifest_bytes: 4 * 1024 * 1024,
            max_width: 512,
            max_height: 512,
            max_tileset_tiles: 4_096,
            max_image_uri_bytes: 4_096,
            max_layer_name_bytes: 256,
            max_object_layers: 32,
            max_objects_per_layer: 4_096,
            max_object_name_bytes: 256,
            max_properties_per_object: 64,
            max_property_string_bytes: 1_024,
        }
    }
}

impl TiledImportLimits {
    fn validate(self) -> Result<Self, TiledImportLimitsError> {
        for (field, value) in [
            ("max_manifest_bytes", self.max_manifest_bytes),
            ("max_image_uri_bytes", self.max_image_uri_bytes),
            ("max_layer_name_bytes", self.max_layer_name_bytes),
            ("max_object_layers", self.max_object_layers),
            ("max_objects_per_layer", self.max_objects_per_layer),
            ("max_object_name_bytes", self.max_object_name_bytes),
            ("max_properties_per_object", self.max_properties_per_object),
            ("max_property_string_bytes", self.max_property_string_bytes),
        ] {
            if value == 0 {
                return Err(TiledImportLimitsError::ZeroLimit(field));
            }
        }
        if self.max_width == 0 || self.max_height == 0 || self.max_tileset_tiles == 0 {
            return Err(TiledImportLimitsError::ZeroLimit("max grid/tileset"));
        }
        Ok(self)
    }
}

/// Invalid [`TiledImportLimits`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TiledImportLimitsError {
    /// Every limit must be positive.
    ZeroLimit(&'static str),
}

impl fmt::Display for TiledImportLimitsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroLimit(field) => write!(formatter, "tiled import limit `{field}` is zero"),
        }
    }
}

impl Error for TiledImportLimitsError {}

/// Built-in importer for Tiled JSON maps.
#[derive(Clone, Copy, Debug)]
pub struct TiledMapImporter {
    limits: TiledImportLimits,
}

impl TiledMapImporter {
    /// Creates an importer with explicit limits.
    ///
    /// # Errors
    ///
    /// Returns [`TiledImportLimitsError`] when any limit is zero.
    pub fn new(limits: TiledImportLimits) -> Result<Self, TiledImportLimitsError> {
        Ok(Self {
            limits: limits.validate()?,
        })
    }

    /// Parses `source` into a renderer-neutral imported map.
    ///
    /// Embedded tilesets work with no extra documents. Maps that reference an
    /// external tileset via `source` must use
    /// [`Self::import_map_with_external_tilesets`] instead.
    ///
    /// # Errors
    ///
    /// Returns [`TiledImportError`] for unsupported maps, limit violations,
    /// unresolved external tilesets, or malformed JSON.
    pub fn import_map(
        &self,
        source: ImportSource<'_>,
    ) -> Result<ImporterOutput<ImportedTiledMap>, TiledImportError> {
        self.import_map_with_external_tilesets(source, &[])
    }

    /// Parses a map and resolves external JSON tileset documents supplied by the host.
    ///
    /// Each map tileset `source` URI must match exactly one entry in
    /// `external_tilesets`. TSX XML is rejected.
    ///
    /// # Errors
    ///
    /// Returns [`TiledImportError`] for unsupported maps, missing external
    /// documents, limit violations, or malformed JSON.
    pub fn import_map_with_external_tilesets(
        &self,
        source: ImportSource<'_>,
        external_tilesets: &[ExternalTilesetBytes<'_>],
    ) -> Result<ImporterOutput<ImportedTiledMap>, TiledImportError> {
        if source.bytes().len() > self.limits.max_manifest_bytes {
            return Err(TiledImportError::ManifestTooLarge {
                bytes: source.bytes().len(),
                limit: self.limits.max_manifest_bytes,
            });
        }
        let raw: RawMap = serde_json::from_slice(strip_utf8_bom(source.bytes()))?;
        let (imported, dependencies) =
            convert_map(raw, &self.limits, external_tilesets)?;
        let cpu_bytes = estimate_cpu_bytes(&imported);
        let mut output = ImporterOutput::new(imported);
        output.cpu_bytes = Some(cpu_bytes as u64);
        output.dependencies = dependencies;
        Ok(output)
    }
}

impl Default for TiledMapImporter {
    fn default() -> Self {
        Self::new(TiledImportLimits::default())
            .expect("default tiled import limits are valid")
    }
}

impl AssetImporter<ImportedTiledMap> for TiledMapImporter {
    type Error = TiledImportError;

    fn descriptor(&self) -> ImporterDescriptor {
        ImporterDescriptor::new(FORMAT_NAME, env!("CARGO_PKG_VERSION"))
            .with_extension("json")
            .with_extension("tmj")
            .with_media_type(TILED_MAP_MEDIA_TYPE)
    }

    fn probe(&self, probe: ImportProbe<'_>) -> ImportMatch {
        if probe.media_type == Some(TILED_MAP_MEDIA_TYPE) {
            return ImportMatch::Exact;
        }
        let looks_tiled = contains_tiled_map_marker(probe.prefix);
        if (probe.extension == Some("tmj") || probe.extension == Some("json")) && looks_tiled {
            ImportMatch::Preferred
        } else if looks_tiled {
            ImportMatch::Possible
        } else {
            ImportMatch::Unsupported
        }
    }

    fn import(
        &self,
        source: ImportSource<'_>,
    ) -> Result<ImporterOutput<ImportedTiledMap>, Self::Error> {
        self.import_map(source)
    }

    fn import_with_context(
        &self,
        source: ImportSource<'_>,
        _context: ImportContext<'_>,
    ) -> Result<ImporterOutput<ImportedTiledMap>, Self::Error> {
        self.import_map(source)
    }
}

fn contains_tiled_map_marker(prefix: &[u8]) -> bool {
    let prefix = strip_utf8_bom(prefix);
    const NEEDLES: [&[u8]; 3] = [
        br#""type":"map""#,
        br#""type": "map""#,
        br#""orientation":"orthogonal""#,
    ];
    NEEDLES.iter().any(|needle| {
        prefix
            .windows(needle.len())
            .any(|window| window == *needle)
    })
}

/// Registers [`TiledMapImporter`] with default limits.
///
/// # Errors
///
/// Forwards registry registration failures.
pub fn register_tiled_map_importer(
    registry: &mut ImporterRegistry<ImportedTiledMap>,
) -> Result<(), ImporterRegistrationError> {
    registry.register(TiledMapImporter::default())
}

/// Neutral imported Tiled map (no texture handle yet).
#[derive(Clone, Debug, PartialEq)]
pub struct ImportedTiledMap {
    grid: [u32; 2],
    tile_pixel_size: [u32; 2],
    image_uri: String,
    image_pixel_size: [u32; 2],
    region_origins: Vec<PixelPoint>,
    region_size: TextureSize,
    cells: Vec<Option<u32>>,
    solid: Vec<bool>,
    visual_layer: String,
    object_layers: Vec<ImportedTiledObjectLayer2d>,
}

impl ImportedTiledMap {
    /// Map width/height in tiles.
    #[must_use]
    pub const fn grid(&self) -> [u32; 2] {
        self.grid
    }

    /// Tile size in source pixels.
    #[must_use]
    pub const fn tile_pixel_size(&self) -> [u32; 2] {
        self.tile_pixel_size
    }

    /// Logical tileset image dependency URI from the Tiled document.
    #[must_use]
    pub fn image_uri(&self) -> &str {
        &self.image_uri
    }

    /// Declared tileset image size in pixels.
    #[must_use]
    pub const fn image_pixel_size(&self) -> [u32; 2] {
        self.image_pixel_size
    }

    /// Visual tile layer name that was selected.
    #[must_use]
    pub fn visual_layer(&self) -> &str {
        &self.visual_layer
    }

    /// Imported objectgroup layers in document order.
    #[must_use]
    pub fn object_layers(&self) -> &[ImportedTiledObjectLayer2d] {
        &self.object_layers
    }

    /// Row-major cell → local tileset index (`None` = empty).
    #[must_use]
    pub fn cells(&self) -> &[Option<u32>] {
        &self.cells
    }

    /// Row-major solid flags (same length as [`Self::cells`]).
    #[must_use]
    pub fn solid(&self) -> &[bool] {
        &self.solid
    }

    /// Rewrites visual local tile ids before bind (authoring/demo retarget).
    ///
    /// Each `(from, to)` pair replaces every `Some(from)` cell with `Some(to)`.
    /// Collision flags are left untouched — hosts that also change solids should
    /// rebuild [`TileCollision2d`] after bind.
    pub fn replace_local_tiles(&mut self, replacements: &[(u32, u32)]) {
        for cell in &mut self.cells {
            let Some(index) = cell.as_mut() else {
                continue;
            };
            for &(from, to) in replacements {
                if *index == from {
                    *index = to;
                    break;
                }
            }
        }
    }

    /// Number of atlas regions (tileset local ids).
    #[must_use]
    pub fn region_count(&self) -> usize {
        self.region_origins.len()
    }

    /// Binds a resolved texture handle into runtime map + collision components.
    ///
    /// World tile size defaults to source pixel size (`1` pixel = `1` world unit).
    ///
    /// # Errors
    ///
    /// Returns bind / [`TileMap2d`] / [`TileCollision2d`] construction failures.
    pub fn bind_texture(
        self,
        texture: TextureHandle,
    ) -> Result<BoundTiledMap2d, TiledBindError> {
        let world_tile_size = [
            self.tile_pixel_size[0] as f32,
            self.tile_pixel_size[1] as f32,
        ];
        self.bind_texture_with_world_tile_size(texture, world_tile_size)
    }

    /// Like [`Self::bind_texture`], but with an explicit world-space tile size.
    ///
    /// # Errors
    ///
    /// Returns bind / map construction failures.
    pub fn bind_texture_with_world_tile_size(
        self,
        texture: TextureHandle,
        world_tile_size: [f32; 2],
    ) -> Result<BoundTiledMap2d, TiledBindError> {
        let texture_size = TextureSize::new(self.image_pixel_size[0], self.image_pixel_size[1])
            .map_err(TiledBindError::TextureSize)?;
        let mut regions = Vec::with_capacity(self.region_origins.len());
        for origin in &self.region_origins {
            regions.push(
                TextureRegion::new(texture, texture_size, *origin, self.region_size)
                    .map_err(TiledBindError::Region)?,
            );
        }
        let tile_map = TileMap2d::new(self.grid, world_tile_size, regions, self.cells)
            .map_err(TiledBindError::TileMap)?;
        let collision =
            TileCollision2d::new(self.grid, self.solid).map_err(TiledBindError::Collision)?;
        Ok(BoundTiledMap2d {
            tile_map,
            collision,
            image_uri: self.image_uri,
            visual_layer: self.visual_layer,
            object_layers: self.object_layers,
            tile_pixel_size: self.tile_pixel_size,
            world_tile_size,
        })
    }
}

/// Runtime map components ready to spawn on one ECS entity.
#[derive(Clone, Debug, PartialEq)]
pub struct BoundTiledMap2d {
    tile_map: TileMap2d,
    collision: TileCollision2d,
    image_uri: String,
    visual_layer: String,
    object_layers: Vec<ImportedTiledObjectLayer2d>,
    tile_pixel_size: [u32; 2],
    world_tile_size: [f32; 2],
}

impl BoundTiledMap2d {
    /// Visual [`TileMap2d`].
    #[must_use]
    pub const fn tile_map(&self) -> &TileMap2d {
        &self.tile_map
    }

    /// Collision [`TileCollision2d`].
    #[must_use]
    pub const fn collision(&self) -> &TileCollision2d {
        &self.collision
    }

    /// Consumes into spawnable components.
    #[must_use]
    pub fn into_components(self) -> (TileMap2d, TileCollision2d) {
        (self.tile_map, self.collision)
    }

    /// Consumes map, collision, and object layers together.
    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        TileMap2d,
        TileCollision2d,
        Vec<ImportedTiledObjectLayer2d>,
    ) {
        (self.tile_map, self.collision, self.object_layers)
    }

    /// Tileset image URI carried for host logging / asset lookup.
    #[must_use]
    pub fn image_uri(&self) -> &str {
        &self.image_uri
    }

    /// Selected visual layer name.
    #[must_use]
    pub fn visual_layer(&self) -> &str {
        &self.visual_layer
    }

    /// Object layers retained through bind (still in Tiled pixel space).
    #[must_use]
    pub fn object_layers(&self) -> &[ImportedTiledObjectLayer2d] {
        &self.object_layers
    }

    /// Source tile size in pixels.
    #[must_use]
    pub const fn tile_pixel_size(&self) -> [u32; 2] {
        self.tile_pixel_size
    }

    /// World tile size used at bind time.
    #[must_use]
    pub const fn world_tile_size(&self) -> [f32; 2] {
        self.world_tile_size
    }

    /// Converts one object's Tiled pixel geometry into world centre + size.
    #[must_use]
    pub fn object_world_rect(&self, object: &ImportedTiledObject2d) -> ([f32; 2], [f32; 2]) {
        world_from_tiled_px(
            object.position_px(),
            object.size_px(),
            self.tile_pixel_size,
            self.world_tile_size,
        )
    }
}

/// Failure while importing a Tiled JSON map.
#[derive(Debug)]
pub enum TiledImportError {
    /// JSON did not parse.
    Json(serde_json::Error),
    /// Document exceeded the byte budget.
    ManifestTooLarge {
        /// Observed size.
        bytes: usize,
        /// Configured limit.
        limit: usize,
    },
    /// Unsupported or invalid map contents.
    Unsupported(&'static str),
    /// Map dimensions exceed limits or are zero.
    GridOutOfLimits {
        /// Observed width/height.
        grid: [u32; 2],
        /// Configured max width/height.
        limit: [u32; 2],
    },
    /// Layer data length does not match width×height.
    LayerDataCount {
        /// Expected cell count.
        expected: usize,
        /// Actual cell count.
        actual: usize,
    },
    /// Tileset tile count exceeds limits.
    TilesetTooLarge {
        /// Observed tile count.
        tiles: u32,
        /// Configured limit.
        limit: u32,
    },
    /// Image URI is empty or too long.
    ImageUri,
    /// External tileset `source` URI is empty or too long.
    ExternalTilesetUri,
    /// Map references an external tileset that was not supplied by the host.
    ExternalTilesetUnresolved {
        /// Tileset `source` URI from the map.
        uri: String,
    },
    /// Layer name is empty or too long.
    LayerName,
    /// A GID was flipped (not supported in this slice).
    FlippedTile {
        /// Raw encoded GID.
        gid: u32,
    },
    /// GID resolved outside the embedded tileset.
    GidOutOfRange {
        /// Raw GID (flags stripped).
        gid: u32,
        /// Tileset firstgid.
        first_gid: u32,
        /// Tileset tilecount.
        tile_count: u32,
    },
    /// Texture size construction failed while planning regions.
    TextureSize(TextureSizeError),
    /// Too many objectgroup layers.
    ObjectLayerLimit {
        /// Configured limit.
        limit: usize,
    },
    /// Too many objects on one objectgroup layer.
    ObjectLimit {
        /// Configured limit.
        limit: usize,
        /// Observed count.
        actual: usize,
    },
    /// Object/class/property name is empty or too long.
    ObjectName,
    /// Too many properties on one object.
    ObjectPropertyLimit {
        /// Configured limit.
        limit: usize,
        /// Observed count.
        actual: usize,
    },
    /// String property value exceeds the byte budget.
    ObjectPropertyString,
}

impl fmt::Display for TiledImportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(error) => write!(formatter, "tiled json: {error}"),
            Self::ManifestTooLarge { bytes, limit } => {
                write!(formatter, "tiled manifest {bytes} bytes exceeds limit {limit}")
            }
            Self::Unsupported(reason) => write!(formatter, "unsupported tiled map: {reason}"),
            Self::GridOutOfLimits { grid, limit } => write!(
                formatter,
                "tiled grid {}x{} outside 1..={}x{}",
                grid[0], grid[1], limit[0], limit[1]
            ),
            Self::LayerDataCount { expected, actual } => write!(
                formatter,
                "tiled layer data length {actual} != expected {expected}"
            ),
            Self::TilesetTooLarge { tiles, limit } => {
                write!(formatter, "tiled tileset has {tiles} tiles; limit {limit}")
            }
            Self::ImageUri => formatter.write_str("tiled tileset image uri is empty or too long"),
            Self::ExternalTilesetUri => {
                formatter.write_str("tiled external tileset source uri is empty or too long")
            }
            Self::ExternalTilesetUnresolved { uri } => write!(
                formatter,
                "tiled external tileset `{uri}` was not supplied by the host resolver"
            ),
            Self::LayerName => formatter.write_str("tiled layer name is empty or too long"),
            Self::FlippedTile { gid } => {
                write!(formatter, "tiled flipped gid {gid:#x} is not supported yet")
            }
            Self::GidOutOfRange {
                gid,
                first_gid,
                tile_count,
            } => write!(
                formatter,
                "tiled gid {gid} outside tileset firstgid={first_gid} count={tile_count}"
            ),
            Self::TextureSize(error) => write!(formatter, "tiled region size: {error}"),
            Self::ObjectLayerLimit { limit } => {
                write!(formatter, "tiled object layer count exceeds limit {limit}")
            }
            Self::ObjectLimit { limit, actual } => write!(
                formatter,
                "tiled object count {actual} exceeds per-layer limit {limit}"
            ),
            Self::ObjectName => {
                formatter.write_str("tiled object/class/property name is empty or too long")
            }
            Self::ObjectPropertyLimit { limit, actual } => write!(
                formatter,
                "tiled object property count {actual} exceeds limit {limit}"
            ),
            Self::ObjectPropertyString => {
                formatter.write_str("tiled object string property exceeds byte limit")
            }
        }
    }
}

impl Error for TiledImportError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            Self::TextureSize(error) => Some(error),
            _ => None,
        }
    }
}

impl From<serde_json::Error> for TiledImportError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

/// Failure while binding an imported map to a texture handle.
#[derive(Debug)]
pub enum TiledBindError {
    /// Declared image size was invalid.
    TextureSize(TextureSizeError),
    /// A tileset region fell outside the image.
    Region(TextureRegionError),
    /// [`TileMap2d`] rejected the bound data.
    TileMap(TileMapError),
    /// [`TileCollision2d`] rejected the solid flags.
    Collision(TileCollisionError),
}

impl fmt::Display for TiledBindError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TextureSize(error) => write!(formatter, "tiled bind texture size: {error}"),
            Self::Region(error) => write!(formatter, "tiled bind region: {error}"),
            Self::TileMap(error) => write!(formatter, "tiled bind tile map: {error}"),
            Self::Collision(error) => write!(formatter, "tiled bind collision: {error}"),
        }
    }
}

impl Error for TiledBindError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::TextureSize(error) => Some(error),
            Self::Region(error) => Some(error),
            Self::TileMap(error) => Some(error),
            Self::Collision(error) => Some(error),
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawMap {
    #[serde(rename = "type")]
    map_type: Option<String>,
    orientation: Option<String>,
    renderorder: Option<String>,
    width: u32,
    height: u32,
    tilewidth: u32,
    tileheight: u32,
    tilesets: Vec<RawTileset>,
    layers: Vec<RawLayer>,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct RawTile {
    id: u32,
    properties: Option<Vec<RawProperty>>,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct RawProperty {
    name: String,
    #[serde(default)]
    value: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct RawLayer {
    #[serde(rename = "type")]
    layer_type: String,
    name: String,
    width: Option<u32>,
    height: Option<u32>,
    data: Option<Vec<u32>>,
    visible: Option<bool>,
    #[serde(default)]
    objects: Option<Vec<objects::RawObject>>,
}

pub(crate) fn strip_utf8_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes)
}

fn convert_map(
    raw: RawMap,
    limits: &TiledImportLimits,
    external_tilesets: &[ExternalTilesetBytes<'_>],
) -> Result<(ImportedTiledMap, Vec<ImportDependency>), TiledImportError> {
    if raw.map_type.as_deref() != Some("map") {
        return Err(TiledImportError::Unsupported("type must be \"map\""));
    }
    if raw.orientation.as_deref() != Some("orthogonal") {
        return Err(TiledImportError::Unsupported("only orthogonal maps"));
    }
    if let Some(order) = raw.renderorder.as_deref() {
        if order != "right-down" {
            return Err(TiledImportError::Unsupported(
                "only renderorder right-down",
            ));
        }
    }
    if raw.width == 0
        || raw.height == 0
        || raw.width > limits.max_width
        || raw.height > limits.max_height
    {
        return Err(TiledImportError::GridOutOfLimits {
            grid: [raw.width, raw.height],
            limit: [limits.max_width, limits.max_height],
        });
    }
    if raw.tilewidth == 0 || raw.tileheight == 0 {
        return Err(TiledImportError::Unsupported("tile size must be positive"));
    }
    if raw.tilesets.len() != 1 {
        return Err(TiledImportError::Unsupported(
            "exactly one tileset required in this slice",
        ));
    }
    let tileset = resolve_map_tileset(
        &raw.tilesets[0],
        [raw.tilewidth, raw.tileheight],
        limits,
        external_tilesets,
    )?;
    let dependencies = tileset.import_dependencies();

    let mut visual: Option<&RawLayer> = None;
    let mut collision_layer: Option<&RawLayer> = None;
    for layer in &raw.layers {
        if layer.layer_type == "objectgroup" {
            continue;
        }
        if layer.layer_type != "tilelayer" {
            return Err(TiledImportError::Unsupported(
                "only tilelayer and objectgroup layers are supported",
            ));
        }
        validate_layer_name(&layer.name, limits)?;
        if layer.name.eq_ignore_ascii_case("collision") {
            collision_layer = Some(layer);
            continue;
        }
        if layer.visible == Some(false) {
            continue;
        }
        if visual.is_none() {
            visual = Some(layer);
        }
    }
    let visual = visual.ok_or(TiledImportError::Unsupported(
        "no visible tile layer found (excluding collision)",
    ))?;

    let cells = decode_layer_cells(visual, raw.width, raw.height, &tileset)?;
    let mut solid = cells
        .iter()
        .map(|cell| {
            cell.map(|index| {
                tileset
                    .solid_by_local
                    .get(index as usize)
                    .copied()
                    .unwrap_or(false)
            })
            .unwrap_or(false)
        })
        .collect::<Vec<_>>();

    if let Some(collision) = collision_layer {
        let collision_cells = decode_layer_cells(collision, raw.width, raw.height, &tileset)?;
        for (dst, src) in solid.iter_mut().zip(collision_cells.iter()) {
            if src.is_some() {
                *dst = true;
            }
        }
    }

    let object_layers = objects::parse_object_layers(&raw.layers, limits)?;

    Ok((
        ImportedTiledMap {
            grid: [raw.width, raw.height],
            tile_pixel_size: [raw.tilewidth, raw.tileheight],
            image_uri: tileset.image_uri,
            image_pixel_size: [tileset.image_w, tileset.image_h],
            region_origins: tileset.region_origins,
            region_size: tileset.region_size,
            cells,
            solid,
            visual_layer: visual.name.clone(),
            object_layers,
        },
        dependencies,
    ))
}

fn validate_layer_name(name: &str, limits: &TiledImportLimits) -> Result<(), TiledImportError> {
    if name.is_empty() || name.len() > limits.max_layer_name_bytes {
        Err(TiledImportError::LayerName)
    } else {
        Ok(())
    }
}

fn decode_layer_cells(
    layer: &RawLayer,
    map_width: u32,
    map_height: u32,
    tileset: &ResolvedTileset,
) -> Result<Vec<Option<u32>>, TiledImportError> {
    let width = layer.width.unwrap_or(map_width);
    let height = layer.height.unwrap_or(map_height);
    if width != map_width || height != map_height {
        return Err(TiledImportError::Unsupported(
            "layer size must match map size in this slice",
        ));
    }
    let data = layer
        .data
        .as_ref()
        .ok_or(TiledImportError::Unsupported(
            "only uncompressed CSV/JSON tile layer data arrays are supported",
        ))?;
    let expected = usize::try_from(u64::from(map_width) * u64::from(map_height))
        .map_err(|_| TiledImportError::Unsupported("map area too large"))?;
    if data.len() != expected {
        return Err(TiledImportError::LayerDataCount {
            expected,
            actual: data.len(),
        });
    }
    let mut cells = Vec::with_capacity(expected);
    for &raw_gid in data {
        if raw_gid == 0 {
            cells.push(None);
            continue;
        }
        if raw_gid & !GID_MASK != 0 {
            return Err(TiledImportError::FlippedTile { gid: raw_gid });
        }
        let gid = raw_gid & GID_MASK;
        if gid < tileset.firstgid {
            return Err(TiledImportError::GidOutOfRange {
                gid,
                first_gid: tileset.firstgid,
                tile_count: tileset.tilecount,
            });
        }
        let local = gid - tileset.firstgid;
        if local >= tileset.tilecount {
            return Err(TiledImportError::GidOutOfRange {
                gid,
                first_gid: tileset.firstgid,
                tile_count: tileset.tilecount,
            });
        }
        cells.push(Some(local));
    }
    Ok(cells)
}

fn estimate_cpu_bytes(map: &ImportedTiledMap) -> usize {
    let mut bytes = mem::size_of_val(map)
        + map.image_uri.len()
        + map.visual_layer.len()
        + map.region_origins.len() * mem::size_of::<PixelPoint>()
        + map.cells.len() * mem::size_of::<Option<u32>>()
        + map.solid.len();
    for layer in &map.object_layers {
        bytes += layer.name().len();
        for object in layer.objects() {
            bytes += object.name().len() + object.class().len();
            for (key, value) in object.properties() {
                bytes += key.len();
                if let TiledPropertyValue::String(text) = value {
                    bytes += text.len();
                }
            }
        }
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use yuyib_2d::Texture;
    use yuyib_assets::Assets;

    const DEMO_MAP: &str = r#"{
      "type": "map",
      "orientation": "orthogonal",
      "renderorder": "right-down",
      "width": 4,
      "height": 3,
      "tilewidth": 8,
      "tileheight": 16,
      "tilesets": [{
        "firstgid": 1,
        "image": "demo_atlas.png",
        "imagewidth": 32,
        "imageheight": 16,
        "tilewidth": 8,
        "tileheight": 16,
        "tilecount": 4,
        "columns": 4,
        "tiles": [
          { "id": 2, "properties": [{ "name": "solid", "type": "bool", "value": true }] }
        ]
      }],
      "layers": [
        {
          "type": "tilelayer",
          "name": "ground",
          "width": 4,
          "height": 3,
          "visible": true,
          "data": [
            3,3,3,3,
            3,1,1,3,
            3,3,3,3
          ]
        },
        {
          "type": "tilelayer",
          "name": "collision",
          "width": 4,
          "height": 3,
          "visible": false,
          "data": [
            1,1,1,1,
            1,0,0,1,
            1,1,1,1
          ]
        },
        {
          "type": "objectgroup",
          "name": "objects",
          "objects": [
            {
              "id": 1,
              "name": "spawn",
              "class": "player_spawn",
              "x": 12.0,
              "y": 24.0,
              "width": 0.0,
              "height": 0.0,
              "point": true,
              "properties": [
                { "name": "tag", "type": "string", "value": "start" }
              ]
            },
            {
              "id": 2,
              "name": "door",
              "type": "portal",
              "x": 8.0,
              "y": 16.0,
              "width": 8.0,
              "height": 16.0,
              "properties": [
                { "name": "target", "type": "string", "value": "house_interior" }
              ]
            }
          ]
        }
      ]
    }"#;

    #[test]
    fn imports_room_with_solid_border() {
        let importer = TiledMapImporter::default();
        let output = importer
            .import_map(ImportSource::new("demo.json", DEMO_MAP.as_bytes()))
            .expect("valid demo map");
        let map = output.asset;
        assert_eq!(map.grid(), [4, 3]);
        assert_eq!(map.visual_layer(), "ground");
        assert_eq!(map.image_uri(), "demo_atlas.png");
        assert_eq!(map.cells()[0], Some(2)); // gid 3 → local 2
        assert_eq!(map.cells()[5], Some(0)); // gid 1 → local 0
        assert!(map.solid()[0]);
        assert!(!map.solid()[5]);
        assert_eq!(output.dependencies.len(), 1);
        assert_eq!(map.object_layers().len(), 1);
        assert_eq!(map.object_layers()[0].name(), "objects");
        assert_eq!(map.object_layers()[0].objects().len(), 2);
        let spawn = &map.object_layers()[0].objects()[0];
        assert_eq!(spawn.class(), "player_spawn");
        assert_eq!(spawn.position_px(), [12.0, 24.0]);
        assert_eq!(
            spawn.property("tag"),
            Some(&TiledPropertyValue::String("start".into()))
        );
        let portal = &map.object_layers()[0].objects()[1];
        assert_eq!(portal.class(), "portal");
        assert_eq!(portal.size_px(), [8.0, 16.0]);
    }

    #[test]
    fn world_from_tiled_px_scales_centre() {
        let (center, size) = world_from_tiled_px([8.0, 16.0], [8.0, 16.0], [8, 16], [32.0, 32.0]);
        assert_eq!(size, [32.0, 32.0]);
        assert_eq!(center, [48.0, 48.0]);
    }

    #[test]
    fn rejects_ellipse_object() {
        let map = DEMO_MAP.replace("\"point\": true", "\"ellipse\": true");
        let error = TiledMapImporter::default()
            .import_map(ImportSource::new("ellipse.json", map.as_bytes()))
            .expect_err("ellipse unsupported");
        assert!(matches!(error, TiledImportError::Unsupported(_)));
    }

    #[test]
    fn replace_local_tiles_rewrites_visual_cells_only() {
        let importer = TiledMapImporter::default();
        let mut map = importer
            .import_map(ImportSource::new("demo.json", DEMO_MAP.as_bytes()))
            .expect("valid demo map")
            .asset;
        let before_solid = map.solid().to_vec();
        map.replace_local_tiles(&[(2, 0)]);
        assert_eq!(map.cells()[0], Some(0));
        assert_eq!(map.solid(), before_solid.as_slice());
    }

    #[test]
    fn bind_builds_tilemap_components() {
        let importer = TiledMapImporter::default();
        let imported = importer
            .import_map(ImportSource::new("demo.json", DEMO_MAP.as_bytes()))
            .expect("valid")
            .asset;
        let mut textures = Assets::<Texture>::new();
        let size = TextureSize::new(32, 16).expect("size");
        let handle = textures.insert(Texture::new(size));
        let bound = imported.bind_texture(handle).expect("bind");
        assert_eq!(bound.tile_map().grid(), [4, 3]);
        let (map, collision) = bound.into_components();
        assert_eq!(map.grid(), [4, 3]);
        assert_eq!(collision, TileCollision2d::new([4, 3], vec![
            true, true, true, true,
            true, false, false, true,
            true, true, true, true,
        ]).expect("solid"));
    }

    #[test]
    fn accepts_utf8_bom_prefix() {
        let mut with_bom = vec![0xEF, 0xBB, 0xBF];
        with_bom.extend_from_slice(DEMO_MAP.as_bytes());
        let imported = TiledMapImporter::default()
            .import_map(ImportSource::new("bom.json", &with_bom))
            .expect("BOM should be stripped")
            .asset;
        assert_eq!(imported.grid(), [4, 3]);
    }

    #[test]
    fn rejects_flipped_gid() {
        let map = DEMO_MAP.replace(
            "3,1,1,3",
            &format!("{},1,1,3", 1_u32 | FLIPPED_HORIZONTALLY),
        );
        let error = TiledMapImporter::default()
            .import_map(ImportSource::new("flip.json", map.as_bytes()))
            .expect_err("flips unsupported");
        assert!(matches!(error, TiledImportError::FlippedTile { .. }));
    }

    const EXTERNAL_TILESET: &str = r#"{
      "type": "tileset",
      "name": "demo_atlas",
      "image": "demo_atlas.png",
      "imagewidth": 32,
      "imageheight": 16,
      "margin": 0,
      "spacing": 0,
      "tilewidth": 8,
      "tileheight": 16,
      "tilecount": 4,
      "columns": 4,
      "tiles": [
        { "id": 2, "properties": [{ "name": "solid", "type": "bool", "value": true }] }
      ]
    }"#;

    const EXTERNAL_MAP: &str = r#"{
      "type": "map",
      "orientation": "orthogonal",
      "renderorder": "right-down",
      "width": 4,
      "height": 3,
      "tilewidth": 8,
      "tileheight": 16,
      "infinite": false,
      "tilesets": [{ "firstgid": 1, "source": "demo_atlas.tsj" }],
      "layers": [
        {
          "type": "tilelayer",
          "name": "ground",
          "width": 4,
          "height": 3,
          "visible": true,
          "data": [3,3,3,3, 3,1,1,3, 3,3,3,3]
        },
        {
          "type": "tilelayer",
          "name": "collision",
          "width": 4,
          "height": 3,
          "visible": false,
          "data": [1,1,1,1, 1,0,0,1, 1,1,1,1]
        }
      ]
    }"#;

    #[test]
    fn imports_external_json_tileset_when_host_resolves() {
        let output = TiledMapImporter::default()
            .import_map_with_external_tilesets(
                ImportSource::new("external_map.json", EXTERNAL_MAP.as_bytes()),
                &[ExternalTilesetBytes::new(
                    "demo_atlas.tsj",
                    EXTERNAL_TILESET.as_bytes(),
                )],
            )
            .expect("external tileset resolves");
        assert_eq!(output.asset.grid(), [4, 3]);
        assert_eq!(output.asset.image_uri(), "demo_atlas.png");
        assert_eq!(output.asset.cells()[0], Some(2));
        assert_eq!(
            output
                .dependencies
                .iter()
                .map(|dep| dep.uri.as_str())
                .collect::<Vec<_>>(),
            vec!["demo_atlas.tsj", "demo_atlas.png"]
        );
    }

    #[test]
    fn rejects_unresolved_external_tileset() {
        let error = TiledMapImporter::default()
            .import_map(ImportSource::new("external_map.json", EXTERNAL_MAP.as_bytes()))
            .expect_err("missing host tileset");
        assert!(matches!(
            error,
            TiledImportError::ExternalTilesetUnresolved { .. }
        ));
    }
}
