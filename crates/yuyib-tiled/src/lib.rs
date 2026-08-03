//! Bounded Tiled JSON / TMX → [`TileMap2d`] importer (M7).
//!
//! Supports orthogonal right-down maps (JSON `.tmj`/`.json` or XML `.tmx`) with
//! one or more tilesets (embedded, external JSON `.tsj`, or XML `.tsx`), one or
//! more visual tile layers, optional `collision`, `objectgroup` layers, and
//! Tiled GID flip flags (H/V/D). Layer tile data: JSON arrays, TMX CSV, or TMX
//! XML `<tile gid>` lists, or base64 (+ zlib/gzip/zstd).
//!
//! Also includes a bounded **LDtk** project importer ([`LdtkProjectImporter`])
//! that assembles the same [`ImportedTiledMap`] (square tiles — LDtk format
//! constraint; embedded or host-resolved `.ldtkl`; multi-tileset; layer
//! pixel offsets) plus a separate [`LdtkWorld2d`] world-layout readout.
//!
//! Explicitly out of scope for this slice: hex/iso, infinite maps,
//! polyline/tile-gid objects. LDtk tiles remain square (format
//! constraint); use Tiled for non-square atlases.

#![forbid(unsafe_code)]

mod ldtk;
mod objects;
mod tileset;
mod xml;

pub use ldtk::{
    ExternalLdtkLevelBytes, LDTK_PROJECT_MEDIA_TYPE, LdtkLevelNeighbour2d, LdtkLevelPlacement2d,
    LdtkProjectImporter, LdtkWorld2d, LdtkWorldLayoutKind2d, register_ldtk_project_importer,
};
pub use objects::{
    ImportedTiledObject2d, ImportedTiledObjectLayer2d, MAX_POLYGON_POINTS, TiledObjectShape2d,
    TiledPropertyValue, world_from_tiled_px,
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
use yuyib_game_2d::{TileCollision2d, TileCollisionError, TileFlip2d, TileMap2d, TileMapError};

use tileset::{RawTileset, ResolvedTileset, resolve_map_tileset};

/// Media type advertised by the Tiled JSON map importer.
pub const TILED_MAP_MEDIA_TYPE: &str = "application/vnd.yuyib.tiled-map+json";

/// Media type advertised for Tiled TMX maps.
pub const TILED_MAP_XML_MEDIA_TYPE: &str = "application/vnd.yuyib.tiled-map+xml";

const FORMAT_NAME: &str = "tiled.map";

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
    /// Maximum tilesets on one map.
    pub max_tilesets: usize,
    /// Maximum UTF-8 bytes in the tileset image URI.
    pub max_image_uri_bytes: usize,
    /// Maximum UTF-8 bytes in a layer name.
    pub max_layer_name_bytes: usize,
    /// Maximum objectgroup layers.
    pub max_object_layers: usize,
    /// Maximum objects per objectgroup layer.
    pub max_objects_per_layer: usize,
    /// Maximum visible tile layers (excluding `collision`).
    pub max_visual_tile_layers: usize,
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
            max_tilesets: 8,
            max_image_uri_bytes: 4_096,
            max_layer_name_bytes: 256,
            max_object_layers: 32,
            max_objects_per_layer: 4_096,
            max_visual_tile_layers: 8,
            max_object_name_bytes: 256,
            max_properties_per_object: 64,
            max_property_string_bytes: 1_024,
        }
    }
}

impl TiledImportLimits {
    pub(crate) fn validate(self) -> Result<Self, TiledImportLimitsError> {
        for (field, value) in [
            ("max_manifest_bytes", self.max_manifest_bytes),
            ("max_image_uri_bytes", self.max_image_uri_bytes),
            ("max_layer_name_bytes", self.max_layer_name_bytes),
            ("max_object_layers", self.max_object_layers),
            ("max_objects_per_layer", self.max_objects_per_layer),
            ("max_visual_tile_layers", self.max_visual_tile_layers),
            ("max_tilesets", self.max_tilesets),
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
    /// Accepts Tiled JSON (`.tmj`/`.json`) or TMX XML (`.tmx`). Embedded
    /// tilesets work with no extra documents. Maps that reference an external
    /// tileset via `source` must use
    /// [`Self::import_map_with_external_tilesets`] instead.
    ///
    /// # Errors
    ///
    /// Returns [`TiledImportError`] for unsupported maps, limit violations,
    /// unresolved external tilesets, or malformed documents.
    pub fn import_map(
        &self,
        source: ImportSource<'_>,
    ) -> Result<ImporterOutput<ImportedTiledMap>, TiledImportError> {
        self.import_map_with_external_tilesets(source, &[])
    }

    /// Parses a map and resolves external tileset documents supplied by the host.
    ///
    /// Each map tileset `source` URI must match exactly one entry in
    /// `external_tilesets`. Documents may be JSON `.tsj` or XML `.tsx`
    /// (content-sniffed).
    ///
    /// # Errors
    ///
    /// Returns [`TiledImportError`] for unsupported maps, missing external
    /// documents, limit violations, or malformed documents.
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
        let raw = parse_map_document(source.bytes())?;
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
            .with_extension("tmx")
            .with_media_type(TILED_MAP_MEDIA_TYPE)
            .with_media_type(TILED_MAP_XML_MEDIA_TYPE)
    }

    fn probe(&self, probe: ImportProbe<'_>) -> ImportMatch {
        if probe.media_type == Some(TILED_MAP_MEDIA_TYPE)
            || probe.media_type == Some(TILED_MAP_XML_MEDIA_TYPE)
        {
            return ImportMatch::Exact;
        }
        let looks_tiled = contains_tiled_map_marker(probe.prefix);
        let tiled_ext = matches!(
            probe.extension,
            Some("tmj" | "json" | "tmx")
        );
        if tiled_ext && looks_tiled {
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
    if xml::looks_like_xml(prefix) {
        const XML_NEEDLES: [&[u8]; 3] = [
            b"<map",
            br#"orientation="orthogonal""#,
            br#"orientation='orthogonal'"#,
        ];
        return XML_NEEDLES.iter().any(|needle| {
            prefix
                .windows(needle.len())
                .any(|window| window == *needle)
        });
    }
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

fn parse_map_document(bytes: &[u8]) -> Result<RawMap, TiledImportError> {
    let bytes = strip_utf8_bom(bytes);
    if xml::looks_like_xml(bytes) {
        xml::parse_tmx_map(bytes)
    } else {
        Ok(serde_json::from_slice(bytes)?)
    }
}

pub(crate) fn parse_tileset_document(bytes: &[u8]) -> Result<RawTileset, TiledImportError> {
    let bytes = strip_utf8_bom(bytes);
    if xml::looks_like_xml(bytes) {
        xml::parse_tsx_tileset(bytes)
    } else {
        Ok(serde_json::from_slice(bytes)?)
    }
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

/// One imported tileset atlas (embedded or resolved external).
#[derive(Clone, Debug, PartialEq)]
pub struct ImportedTiledTileset2d {
    image_uri: String,
    image_pixel_size: [u32; 2],
    region_origins: Vec<PixelPoint>,
    region_size: TextureSize,
    /// First global region index owned by this tileset.
    region_base: u32,
}

impl ImportedTiledTileset2d {
    /// Builds one tileset atlas descriptor (authoring / LDtk bridge).
    #[must_use]
    pub fn new(
        image_uri: impl Into<String>,
        image_pixel_size: [u32; 2],
        region_origins: Vec<PixelPoint>,
        region_size: TextureSize,
        region_base: u32,
    ) -> Self {
        Self {
            image_uri: image_uri.into(),
            image_pixel_size,
            region_origins,
            region_size,
            region_base,
        }
    }

    /// Logical image URI.
    #[must_use]
    pub fn image_uri(&self) -> &str {
        &self.image_uri
    }

    /// Declared image size in pixels.
    #[must_use]
    pub const fn image_pixel_size(&self) -> [u32; 2] {
        self.image_pixel_size
    }

    /// Region origins in document order (local tile ids).
    #[must_use]
    pub fn region_origins(&self) -> &[PixelPoint] {
        &self.region_origins
    }

    /// Pixel size of one tile region.
    #[must_use]
    pub const fn region_size(&self) -> TextureSize {
        self.region_size
    }

    /// Global region index of local id 0.
    #[must_use]
    pub const fn region_base(&self) -> u32 {
        self.region_base
    }
}

/// One imported visible tile layer.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportedTiledVisualLayer2d {
    name: String,
    /// Global region indices (across all tilesets).
    cells: Vec<Option<u32>>,
    flips: Vec<TileFlip2d>,
    /// Layer pixel offset (LDtk `__pxTotalOffset*`; Tiled usually `[0,0]`).
    offset_px: [f32; 2],
    /// Tiled `parallaxx` / `parallaxy` (default `[1, 1]` = no parallax).
    parallax: [f32; 2],
}

impl ImportedTiledVisualLayer2d {
    /// Builds one visual layer; `cells` and `flips` must share length.
    ///
    /// # Errors
    ///
    /// Returns [`TiledAssembleError::LayerLen`] when lengths differ.
    pub fn new(
        name: impl Into<String>,
        cells: Vec<Option<u32>>,
        flips: Vec<TileFlip2d>,
    ) -> Result<Self, TiledAssembleError> {
        if cells.len() != flips.len() {
            return Err(TiledAssembleError::LayerLen {
                cells: cells.len(),
                flips: flips.len(),
            });
        }
        Ok(Self {
            name: name.into(),
            cells,
            flips,
            offset_px: [0.0, 0.0],
            parallax: [1.0, 1.0],
        })
    }

    /// Sets a pixel-space layer offset (applied at bind using world tile scale).
    #[must_use]
    pub const fn with_offset_px(mut self, offset_px: [f32; 2]) -> Self {
        self.offset_px = offset_px;
        self
    }

    /// Sets Tiled-compatible parallax factors.
    #[must_use]
    pub const fn with_parallax(mut self, parallax: [f32; 2]) -> Self {
        self.parallax = parallax;
        self
    }

    /// Layer name from Tiled.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Row-major global region indices.
    #[must_use]
    pub fn cells(&self) -> &[Option<u32>] {
        &self.cells
    }

    /// Row-major flip flags.
    #[must_use]
    pub fn flips(&self) -> &[TileFlip2d] {
        &self.flips
    }

    /// Pixel-space layer offset.
    #[must_use]
    pub const fn offset_px(&self) -> [f32; 2] {
        self.offset_px
    }

    /// Parallax factors (`parallaxx`, `parallaxy`).
    #[must_use]
    pub const fn parallax(&self) -> [f32; 2] {
        self.parallax
    }
}

/// Neutral imported Tiled map (no texture handle yet).
#[derive(Clone, Debug, PartialEq)]
pub struct ImportedTiledMap {
    grid: [u32; 2],
    tile_pixel_size: [u32; 2],
    tilesets: Vec<ImportedTiledTileset2d>,
    visual_layers: Vec<ImportedTiledVisualLayer2d>,
    solid: Vec<bool>,
    object_layers: Vec<ImportedTiledObjectLayer2d>,
    /// Tiled per-tile animations (global region indices).
    tile_animations: Vec<ImportedTileAnimation2d>,
}

/// One imported Tiled tile animation (global region indices + dwell ms).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportedTileAnimation2d {
    /// Authored/base region index placed in the layer grid.
    pub base_region: u32,
    /// Frames as `(region_index, duration_ms)`.
    pub frames: Vec<(u32, u32)>,
}

impl ImportedTiledMap {
    /// Assembles a map from already-decoded authoring parts (LDtk / tools).
    ///
    /// # Errors
    ///
    /// Returns [`TiledAssembleError`] when grid/solid/layer sizes disagree or
    /// tilesets are empty.
    pub fn assemble(
        grid: [u32; 2],
        tile_pixel_size: [u32; 2],
        tilesets: Vec<ImportedTiledTileset2d>,
        visual_layers: Vec<ImportedTiledVisualLayer2d>,
        solid: Vec<bool>,
        object_layers: Vec<ImportedTiledObjectLayer2d>,
    ) -> Result<Self, TiledAssembleError> {
        if grid[0] == 0 || grid[1] == 0 {
            return Err(TiledAssembleError::ZeroGrid);
        }
        if tile_pixel_size[0] == 0 || tile_pixel_size[1] == 0 {
            return Err(TiledAssembleError::ZeroTileSize);
        }
        if tilesets.is_empty() {
            return Err(TiledAssembleError::NoTilesets);
        }
        if visual_layers.is_empty() {
            return Err(TiledAssembleError::NoVisualLayers);
        }
        let expected = usize::try_from(u64::from(grid[0]) * u64::from(grid[1]))
            .map_err(|_| TiledAssembleError::GridTooLarge)?;
        if solid.len() != expected {
            return Err(TiledAssembleError::SolidLen {
                expected,
                actual: solid.len(),
            });
        }
        for layer in &visual_layers {
            if layer.cells.len() != expected {
                return Err(TiledAssembleError::LayerCells {
                    layer: layer.name.clone(),
                    expected,
                    actual: layer.cells.len(),
                });
            }
        }
        Ok(Self {
            grid,
            tile_pixel_size,
            tilesets,
            visual_layers,
            solid,
            object_layers,
            tile_animations: Vec::new(),
        })
    }

    /// Attaches Tiled tile animations (global region indices).
    #[must_use]
    pub fn with_tile_animations(mut self, tile_animations: Vec<ImportedTileAnimation2d>) -> Self {
        self.tile_animations = tile_animations;
        self
    }

    /// Imported tile animations.
    #[must_use]
    pub fn tile_animations(&self) -> &[ImportedTileAnimation2d] {
        &self.tile_animations
    }

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

    /// First tileset image URI (compat with single-tileset hosts).
    #[must_use]
    pub fn image_uri(&self) -> &str {
        self.tilesets
            .first()
            .map(ImportedTiledTileset2d::image_uri)
            .unwrap_or("")
    }

    /// First tileset image size (compat).
    #[must_use]
    pub fn image_pixel_size(&self) -> [u32; 2] {
        self.tilesets
            .first()
            .map(ImportedTiledTileset2d::image_pixel_size)
            .unwrap_or([0, 0])
    }

    /// All imported tilesets in document order.
    #[must_use]
    pub fn tilesets(&self) -> &[ImportedTiledTileset2d] {
        &self.tilesets
    }

    /// First visual tile layer name (document order).
    #[must_use]
    pub fn visual_layer(&self) -> &str {
        self.visual_layers
            .first()
            .map(ImportedTiledVisualLayer2d::name)
            .unwrap_or("")
    }

    /// All visible tile layers in document order.
    #[must_use]
    pub fn visual_layers(&self) -> &[ImportedTiledVisualLayer2d] {
        &self.visual_layers
    }

    /// Imported objectgroup layers in document order.
    #[must_use]
    pub fn object_layers(&self) -> &[ImportedTiledObjectLayer2d] {
        &self.object_layers
    }

    /// Row-major cell → global region index for the first visual layer.
    #[must_use]
    pub fn cells(&self) -> &[Option<u32>] {
        self.visual_layers
            .first()
            .map(ImportedTiledVisualLayer2d::cells)
            .unwrap_or(&[])
    }

    /// Row-major flip flags for the first visual layer.
    #[must_use]
    pub fn flips(&self) -> &[TileFlip2d] {
        self.visual_layers
            .first()
            .map(ImportedTiledVisualLayer2d::flips)
            .unwrap_or(&[])
    }

    /// Row-major solid flags (union of tile `solid` props + collision layer).
    #[must_use]
    pub fn solid(&self) -> &[bool] {
        &self.solid
    }

    /// Rewrites visual global region ids before bind (authoring/demo retarget).
    pub fn replace_local_tiles(&mut self, replacements: &[(u32, u32)]) {
        for layer in &mut self.visual_layers {
            for cell in &mut layer.cells {
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
    }

    /// Total atlas regions across all tilesets.
    #[must_use]
    pub fn region_count(&self) -> usize {
        self.tilesets
            .iter()
            .map(|tileset| tileset.region_origins.len())
            .sum()
    }

    /// Binds a single texture (maps with exactly one tileset image).
    ///
    /// # Errors
    ///
    /// Returns [`TiledBindError::TextureCount`] when the map has ≠ 1 tileset.
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
    /// Returns [`TiledBindError::TextureCount`] when the map has ≠ 1 tileset.
    pub fn bind_texture_with_world_tile_size(
        self,
        texture: TextureHandle,
        world_tile_size: [f32; 2],
    ) -> Result<BoundTiledMap2d, TiledBindError> {
        if self.tilesets.len() != 1 {
            return Err(TiledBindError::TextureCount {
                expected: 1,
                actual: self.tilesets.len(),
            });
        }
        let uri = self.tilesets[0].image_uri.clone();
        self.bind_textures_with_world_tile_size(&[(uri.as_str(), texture)], world_tile_size)
    }

    /// Binds one texture handle per tileset image URI.
    ///
    /// # Errors
    ///
    /// Missing URI, wrong texture count, or map construction failures.
    pub fn bind_textures_with_world_tile_size(
        self,
        textures: &[(&str, TextureHandle)],
        world_tile_size: [f32; 2],
    ) -> Result<BoundTiledMap2d, TiledBindError> {
        if textures.len() != self.tilesets.len() {
            return Err(TiledBindError::TextureCount {
                expected: self.tilesets.len(),
                actual: textures.len(),
            });
        }
        let mut regions = Vec::with_capacity(self.region_count());
        for tileset in &self.tilesets {
            let handle = textures
                .iter()
                .find(|(uri, _)| *uri == tileset.image_uri)
                .map(|(_, handle)| *handle)
                .ok_or_else(|| TiledBindError::MissingTexture {
                    uri: tileset.image_uri.clone(),
                })?;
            let texture_size =
                TextureSize::new(tileset.image_pixel_size[0], tileset.image_pixel_size[1])
                    .map_err(TiledBindError::TextureSize)?;
            for origin in &tileset.region_origins {
                regions.push(
                    TextureRegion::new(handle, texture_size, *origin, tileset.region_size)
                        .map_err(TiledBindError::Region)?,
                );
            }
        }
        let mut tile_maps = Vec::with_capacity(self.visual_layers.len());
        let mut visual_layer_names = Vec::with_capacity(self.visual_layers.len());
        for (index, layer) in self.visual_layers.into_iter().enumerate() {
            let painter_layer = i32::try_from(index).unwrap_or(i32::MAX);
            let scale = [
                world_tile_size[0] / self.tile_pixel_size[0] as f32,
                world_tile_size[1] / self.tile_pixel_size[1] as f32,
            ];
            let position = [layer.offset_px[0] * scale[0], layer.offset_px[1] * scale[1]];
            let mut tile_map = TileMap2d::new(
                self.grid,
                world_tile_size,
                regions.clone(),
                layer.cells,
            )
            .map_err(TiledBindError::TileMap)?
            .with_flips(layer.flips)
            .map_err(TiledBindError::TileMap)?
            .with_parallax_factor(layer.parallax)
            .map_err(TiledBindError::TileMap)?
            .with_layer(painter_layer)
            .with_position(position);
            if !self.tile_animations.is_empty() {
                let mut runtime = Vec::with_capacity(self.tile_animations.len());
                for animation in &self.tile_animations {
                    let frames = animation
                        .frames
                        .iter()
                        .map(|(region, duration_ms)| {
                            yuyib_game_2d::TileAnimFrame2d {
                                region_index: *region,
                                duration: std::time::Duration::from_millis(u64::from(*duration_ms)),
                            }
                        })
                        .collect();
                    runtime.push(
                        yuyib_game_2d::TileRegionAnimation2d::new(animation.base_region, frames)
                            .map_err(TiledBindError::TileMap)?,
                    );
                }
                tile_map = tile_map
                    .with_region_animations(runtime)
                    .map_err(TiledBindError::TileMap)?;
            }
            visual_layer_names.push(layer.name);
            tile_maps.push(tile_map);
        }
        let collision =
            TileCollision2d::new(self.grid, self.solid).map_err(TiledBindError::Collision)?;
        let image_uri = self
            .tilesets
            .first()
            .map(|tileset| tileset.image_uri.clone())
            .unwrap_or_default();
        Ok(BoundTiledMap2d {
            tile_maps,
            collision,
            image_uri,
            visual_layers: visual_layer_names,
            object_layers: self.object_layers,
            tile_pixel_size: self.tile_pixel_size,
            world_tile_size,
        })
    }
}

/// Runtime map components ready to spawn (one [`TileMap2d`] per visual layer).
#[derive(Clone, Debug, PartialEq)]
pub struct BoundTiledMap2d {
    tile_maps: Vec<TileMap2d>,
    collision: TileCollision2d,
    image_uri: String,
    visual_layers: Vec<String>,
    object_layers: Vec<ImportedTiledObjectLayer2d>,
    tile_pixel_size: [u32; 2],
    world_tile_size: [f32; 2],
}

impl BoundTiledMap2d {
    /// First visual [`TileMap2d`] (lowest painter layer).
    ///
    /// # Panics
    ///
    /// Never panics for maps produced by this importer (at least one visual layer).
    #[must_use]
    pub fn tile_map(&self) -> &TileMap2d {
        &self.tile_maps[0]
    }

    /// All visual tile maps in document order (painter layer = index).
    #[must_use]
    pub fn tile_maps(&self) -> &[TileMap2d] {
        &self.tile_maps
    }

    /// Collision [`TileCollision2d`].
    #[must_use]
    pub const fn collision(&self) -> &TileCollision2d {
        &self.collision
    }

    /// Consumes into visual maps + collision (all layers).
    #[must_use]
    pub fn into_components(self) -> (Vec<TileMap2d>, TileCollision2d) {
        (self.tile_maps, self.collision)
    }

    /// Consumes maps, collision, and object layers together.
    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        Vec<TileMap2d>,
        TileCollision2d,
        Vec<ImportedTiledObjectLayer2d>,
    ) {
        (self.tile_maps, self.collision, self.object_layers)
    }

    /// Tileset image URI carried for host logging / asset lookup.
    #[must_use]
    pub fn image_uri(&self) -> &str {
        &self.image_uri
    }

    /// First visual layer name.
    #[must_use]
    pub fn visual_layer(&self) -> &str {
        self.visual_layers.first().map(String::as_str).unwrap_or("")
    }

    /// Visual layer names in document order.
    #[must_use]
    pub fn visual_layers(&self) -> &[String] {
        &self.visual_layers
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
    /// XML did not parse.
    Xml(roxmltree::Error),
    /// Document bytes were not valid UTF-8 (TMX/TSX).
    Utf8(std::str::Utf8Error),
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
    /// External LDtk level `externalRelPath` is empty or too long.
    ExternalLdtkLevelUri,
    /// Project references an external `.ldtkl` that was not supplied by the host.
    ExternalLdtkLevelUnresolved {
        /// Level `externalRelPath` from the project.
        uri: String,
    },
    /// Layer name is empty or too long.
    LayerName,
    /// GID resolved outside the tileset.
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
    /// Too many visible tile layers.
    VisualLayerLimit {
        /// Configured limit.
        limit: usize,
        /// Observed count.
        actual: usize,
    },
    /// Too many tilesets on the map.
    TilesetLimit {
        /// Configured limit.
        limit: usize,
        /// Observed count.
        actual: usize,
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
            Self::Xml(error) => write!(formatter, "tiled xml: {error}"),
            Self::Utf8(error) => write!(formatter, "tiled utf-8: {error}"),
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
            Self::ExternalLdtkLevelUri => {
                formatter.write_str("ldtk external level path is empty or too long")
            }
            Self::ExternalLdtkLevelUnresolved { uri } => write!(
                formatter,
                "ldtk external level `{uri}` was not supplied by the host resolver"
            ),
            Self::LayerName => formatter.write_str("tiled layer name is empty or too long"),
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
            Self::VisualLayerLimit { limit, actual } => write!(
                formatter,
                "tiled visual tile layer count {actual} exceeds limit {limit}"
            ),
            Self::TilesetLimit { limit, actual } => write!(
                formatter,
                "tiled tileset count {actual} exceeds limit {limit}"
            ),
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
            Self::Xml(error) => Some(error),
            Self::Utf8(error) => Some(error),
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

impl From<roxmltree::Error> for TiledImportError {
    fn from(value: roxmltree::Error) -> Self {
        Self::Xml(value)
    }
}

impl From<std::str::Utf8Error> for TiledImportError {
    fn from(value: std::str::Utf8Error) -> Self {
        Self::Utf8(value)
    }
}

/// Failure while assembling an [`ImportedTiledMap`] from parts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TiledAssembleError {
    /// Grid has a zero dimension.
    ZeroGrid,
    /// Tile pixel size has a zero dimension.
    ZeroTileSize,
    /// Grid product cannot fit memory indexing.
    GridTooLarge,
    /// No tilesets supplied.
    NoTilesets,
    /// No visual layers supplied.
    NoVisualLayers,
    /// Solid flag count differs from grid area.
    SolidLen {
        /// Expected cells.
        expected: usize,
        /// Supplied flags.
        actual: usize,
    },
    /// Visual layer cell count differs from grid area.
    LayerCells {
        /// Layer name.
        layer: String,
        /// Expected cells.
        expected: usize,
        /// Supplied cells.
        actual: usize,
    },
    /// Visual layer `cells`/`flips` length mismatch.
    LayerLen {
        /// Cell count.
        cells: usize,
        /// Flip count.
        flips: usize,
    },
}

impl fmt::Display for TiledAssembleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroGrid => formatter.write_str("assembled tiled map grid has a zero dimension"),
            Self::ZeroTileSize => {
                formatter.write_str("assembled tiled map tile size has a zero dimension")
            }
            Self::GridTooLarge => formatter.write_str("assembled tiled map grid is too large"),
            Self::NoTilesets => formatter.write_str("assembled tiled map requires tilesets"),
            Self::NoVisualLayers => {
                formatter.write_str("assembled tiled map requires visual layers")
            }
            Self::SolidLen { expected, actual } => write!(
                formatter,
                "assembled tiled solid length {actual} != expected {expected}"
            ),
            Self::LayerCells {
                layer,
                expected,
                actual,
            } => write!(
                formatter,
                "assembled tiled layer `{layer}` cell length {actual} != expected {expected}"
            ),
            Self::LayerLen { cells, flips } => write!(
                formatter,
                "assembled tiled layer cells {cells} != flips {flips}"
            ),
        }
    }
}

impl Error for TiledAssembleError {}

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
    /// Host supplied the wrong number of textures for the imported tilesets.
    TextureCount {
        /// Tileset / expected texture count.
        expected: usize,
        /// Actual texture slice length.
        actual: usize,
    },
    /// No texture was provided for a tileset image URI.
    MissingTexture {
        /// Logical image URI from the tileset.
        uri: String,
    },
}

impl fmt::Display for TiledBindError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TextureSize(error) => write!(formatter, "tiled bind texture size: {error}"),
            Self::Region(error) => write!(formatter, "tiled bind region: {error}"),
            Self::TileMap(error) => write!(formatter, "tiled bind tile map: {error}"),
            Self::Collision(error) => write!(formatter, "tiled bind collision: {error}"),
            Self::TextureCount { expected, actual } => write!(
                formatter,
                "tiled bind expected {expected} textures, got {actual}"
            ),
            Self::MissingTexture { uri } => {
                write!(formatter, "tiled bind missing texture for image `{uri}`")
            }
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
            Self::TextureCount { .. } | Self::MissingTexture { .. } => None,
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawMap {
    #[serde(rename = "type")]
    pub(crate) map_type: Option<String>,
    pub(crate) orientation: Option<String>,
    pub(crate) renderorder: Option<String>,
    #[serde(default)]
    pub(crate) infinite: Option<bool>,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) tilewidth: u32,
    pub(crate) tileheight: u32,
    pub(crate) tilesets: Vec<RawTileset>,
    pub(crate) layers: Vec<RawLayer>,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct RawTile {
    pub(crate) id: u32,
    pub(crate) properties: Option<Vec<RawProperty>>,
    #[serde(default)]
    pub(crate) animation: Option<Vec<RawTileAnimFrame>>,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct RawTileAnimFrame {
    pub(crate) tileid: u32,
    pub(crate) duration: u32,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct RawProperty {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) value: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawLayer {
    #[serde(rename = "type")]
    pub(crate) layer_type: String,
    pub(crate) name: String,
    pub(crate) width: Option<u32>,
    pub(crate) height: Option<u32>,
    pub(crate) data: Option<Vec<u32>>,
    pub(crate) visible: Option<bool>,
    #[serde(default = "default_parallax_axis")]
    pub(crate) parallaxx: f32,
    #[serde(default = "default_parallax_axis")]
    pub(crate) parallaxy: f32,
    #[serde(default)]
    pub(crate) objects: Option<Vec<objects::RawObject>>,
}

const fn default_parallax_axis() -> f32 {
    1.0
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
    if raw.infinite == Some(true) {
        return Err(TiledImportError::Unsupported("infinite maps"));
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
    if raw.tilesets.is_empty() {
        return Err(TiledImportError::Unsupported(
            "at least one tileset required",
        ));
    }
    if raw.tilesets.len() > limits.max_tilesets {
        return Err(TiledImportError::TilesetLimit {
            limit: limits.max_tilesets,
            actual: raw.tilesets.len(),
        });
    }

    let mut resolved = Vec::with_capacity(raw.tilesets.len());
    for entry in &raw.tilesets {
        resolved.push(resolve_map_tileset(
            entry,
            [raw.tilewidth, raw.tileheight],
            limits,
            external_tilesets,
        )?);
    }
    validate_tileset_gid_ranges(&resolved)?;

    let mut region_base = 0_u32;
    let mut dependencies = Vec::new();
    let mut imported_tilesets = Vec::with_capacity(resolved.len());
    for tileset in &mut resolved {
        tileset.region_base = region_base;
        dependencies.extend(tileset.import_dependencies());
        imported_tilesets.push(ImportedTiledTileset2d {
            image_uri: tileset.image_uri.clone(),
            image_pixel_size: [tileset.image_w, tileset.image_h],
            region_origins: tileset.region_origins.clone(),
            region_size: tileset.region_size,
            region_base,
        });
        region_base = region_base.saturating_add(tileset.tilecount);
    }

    let mut visual_raw: Vec<&RawLayer> = Vec::new();
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
        visual_raw.push(layer);
    }
    if visual_raw.is_empty() {
        return Err(TiledImportError::Unsupported(
            "no visible tile layer found (excluding collision)",
        ));
    }
    if visual_raw.len() > limits.max_visual_tile_layers {
        return Err(TiledImportError::VisualLayerLimit {
            limit: limits.max_visual_tile_layers,
            actual: visual_raw.len(),
        });
    }

    let mut visual_layers = Vec::with_capacity(visual_raw.len());
    let mut solid = vec![false; usize::try_from(u64::from(raw.width) * u64::from(raw.height))
        .map_err(|_| TiledImportError::Unsupported("map area too large"))?];
    for layer in visual_raw {
        let (cells, flips) = decode_layer_cells(layer, raw.width, raw.height, &resolved)?;
        for (dst, cell) in solid.iter_mut().zip(cells.iter()) {
            if let Some(index) = cell {
                if global_region_is_solid(&resolved, *index) {
                    *dst = true;
                }
            }
        }
        let parallax = [layer.parallaxx, layer.parallaxy];
        if !parallax.iter().all(|value| value.is_finite()) {
            return Err(TiledImportError::Unsupported(
                "layer parallax must be finite",
            ));
        }
        visual_layers.push(ImportedTiledVisualLayer2d {
            name: layer.name.clone(),
            cells,
            flips,
            offset_px: [0.0, 0.0],
            parallax,
        });
    }

    if let Some(collision) = collision_layer {
        let (collision_cells, _) =
            decode_layer_cells(collision, raw.width, raw.height, &resolved)?;
        for (dst, src) in solid.iter_mut().zip(collision_cells.iter()) {
            if src.is_some() {
                *dst = true;
            }
        }
    }

    let object_layers = objects::parse_object_layers(&raw.layers, limits)?;

    let mut tile_animations = Vec::new();
    for tileset in &resolved {
        for (local_id, frames) in &tileset.animations {
            tile_animations.push(ImportedTileAnimation2d {
                base_region: tileset.region_base.saturating_add(*local_id),
                frames: frames
                    .iter()
                    .map(|(tileid, duration)| {
                        (
                            tileset.region_base.saturating_add(*tileid),
                            *duration,
                        )
                    })
                    .collect(),
            });
        }
    }

    Ok((
        ImportedTiledMap {
            grid: [raw.width, raw.height],
            tile_pixel_size: [raw.tilewidth, raw.tileheight],
            tilesets: imported_tilesets,
            visual_layers,
            solid,
            object_layers,
            tile_animations,
        },
        dependencies,
    ))
}

fn validate_tileset_gid_ranges(tilesets: &[ResolvedTileset]) -> Result<(), TiledImportError> {
    let mut ordered: Vec<&ResolvedTileset> = tilesets.iter().collect();
    ordered.sort_by_key(|tileset| tileset.firstgid);
    for window in ordered.windows(2) {
        let left = window[0];
        let right = window[1];
        if left.firstgid == right.firstgid {
            return Err(TiledImportError::Unsupported(
                "duplicate tileset firstgid",
            ));
        }
        let left_end = left
            .firstgid
            .checked_add(left.tilecount)
            .ok_or(TiledImportError::Unsupported("tileset gid range overflow"))?;
        if left_end > right.firstgid {
            return Err(TiledImportError::Unsupported(
                "overlapping tileset gid ranges",
            ));
        }
    }
    Ok(())
}

fn global_region_is_solid(tilesets: &[ResolvedTileset], global: u32) -> bool {
    for tileset in tilesets {
        let end = tileset.region_base.saturating_add(tileset.tilecount);
        if global >= tileset.region_base && global < end {
            let local = (global - tileset.region_base) as usize;
            return tileset.solid_by_local.get(local).copied().unwrap_or(false);
        }
    }
    false
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
    tilesets: &[ResolvedTileset],
) -> Result<(Vec<Option<u32>>, Vec<TileFlip2d>), TiledImportError> {
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
    let mut flips = Vec::with_capacity(expected);
    for &raw_gid in data {
        if raw_gid == 0 {
            cells.push(None);
            flips.push(TileFlip2d::NONE);
            continue;
        }
        let flip = TileFlip2d::from_tiled_gid_flags(raw_gid);
        let gid = raw_gid & GID_MASK;
        let tileset = tilesets
            .iter()
            .filter(|candidate| candidate.firstgid <= gid)
            .max_by_key(|candidate| candidate.firstgid)
            .ok_or(TiledImportError::GidOutOfRange {
                gid,
                first_gid: tilesets.first().map(|tileset| tileset.firstgid).unwrap_or(0),
                tile_count: 0,
            })?;
        let local = gid - tileset.firstgid;
        if local >= tileset.tilecount {
            return Err(TiledImportError::GidOutOfRange {
                gid,
                first_gid: tileset.firstgid,
                tile_count: tileset.tilecount,
            });
        }
        cells.push(Some(tileset.region_base + local));
        flips.push(flip);
    }
    Ok((cells, flips))
}

pub(crate) fn estimate_cpu_bytes(map: &ImportedTiledMap) -> usize {
    let mut bytes = mem::size_of_val(map) + map.solid.len();
    for tileset in &map.tilesets {
        bytes += tileset.image_uri.len()
            + tileset.region_origins.len() * mem::size_of::<PixelPoint>();
    }
    for layer in &map.visual_layers {
        bytes += layer.name.len()
            + layer.cells.len() * mem::size_of::<Option<u32>>()
            + layer.flips.len() * mem::size_of::<TileFlip2d>();
    }
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
        assert_eq!(map.visual_layers().len(), 1);
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
    fn imports_multiple_visual_tile_layers() {
        let map = DEMO_MAP.replace(
            r#""data": [
            3,3,3,3,
            3,1,1,3,
            3,3,3,3
          ]
        },
        {
          "type": "tilelayer",
          "name": "collision","#,
            r#""data": [
            3,3,3,3,
            3,1,1,3,
            3,3,3,3
          ]
        },
        {
          "type": "tilelayer",
          "name": "deco",
          "width": 4,
          "height": 3,
          "visible": true,
          "data": [
            0,0,0,0,
            0,0,2,0,
            0,0,0,0
          ]
        },
        {
          "type": "tilelayer",
          "name": "collision","#,
        );
        let imported = TiledMapImporter::default()
            .import_map(ImportSource::new("multi.json", map.as_bytes()))
            .expect("multi layer")
            .asset;
        assert_eq!(imported.visual_layers().len(), 2);
        assert_eq!(imported.visual_layers()[1].name(), "deco");
        assert_eq!(imported.visual_layers()[1].cells()[6], Some(1));
        let mut textures = Assets::<Texture>::new();
        let size = TextureSize::new(32, 16).expect("size");
        let handle = textures.insert(Texture::new(size));
        let bound = imported.bind_texture(handle).expect("bind");
        assert_eq!(bound.tile_maps().len(), 2);
        assert_eq!(bound.tile_maps()[0].layer, 0);
        assert_eq!(bound.tile_maps()[1].layer, 1);
    }

    #[test]
    fn world_from_tiled_px_scales_centre() {
        let (center, size) = world_from_tiled_px([8.0, 16.0], [8.0, 16.0], [8, 16], [32.0, 32.0]);
        assert_eq!(size, [32.0, 32.0]);
        assert_eq!(center, [48.0, 48.0]);
    }

    #[test]
    fn imports_ellipse_and_polygon_objects() {
        let ellipse_map = DEMO_MAP.replace(
            r#"{
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
            }"#,
            r#"{
              "id": 1,
              "name": "spawn",
              "class": "player_spawn",
              "x": 12.0,
              "y": 24.0,
              "width": 16.0,
              "height": 8.0,
              "ellipse": true,
              "properties": [
                { "name": "tag", "type": "string", "value": "start" }
              ]
            }"#,
        );
        let imported = TiledMapImporter::default()
            .import_map(ImportSource::new("ellipse.json", ellipse_map.as_bytes()))
            .expect("ellipse")
            .asset;
        let spawn = &imported.object_layers()[0].objects()[0];
        assert!(matches!(spawn.shape(), TiledObjectShape2d::Ellipse));
        assert_eq!(spawn.size_px(), [16.0, 8.0]);
        assert!(spawn.contains_point_px([20.0, 28.0]));
        assert!(!spawn.contains_point_px([12.0, 24.0]));

        let polygon_map = DEMO_MAP.replace(
            r#"{
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
            }"#,
            r#"{
              "id": 1,
              "name": "spawn",
              "class": "player_spawn",
              "x": 12.0,
              "y": 24.0,
              "width": 0.0,
              "height": 0.0,
              "polygon": [{"x":0,"y":0},{"x":16,"y":0},{"x":8,"y":12}],
              "properties": [
                { "name": "tag", "type": "string", "value": "start" }
              ]
            }"#,
        );
        let imported = TiledMapImporter::default()
            .import_map(ImportSource::new("poly.json", polygon_map.as_bytes()))
            .expect("polygon")
            .asset;
        let spawn = &imported.object_layers()[0].objects()[0];
        assert!(matches!(spawn.shape(), TiledObjectShape2d::Polygon { .. }));
        assert!(spawn.contains_point_px([20.0, 28.0]));
    }

    #[test]
    fn imports_tile_animation_and_binds() {
        let map = DEMO_MAP.replace(
            r#""tiles": [
          { "id": 2, "properties": [{ "name": "solid", "type": "bool", "value": true }] }
        ]"#,
            r#""tiles": [
          { "id": 2, "properties": [{ "name": "solid", "type": "bool", "value": true }] },
          {
            "id": 0,
            "animation": [
              { "tileid": 0, "duration": 100 },
              { "tileid": 1, "duration": 100 }
            ]
          }
        ]"#,
        );
        let imported = TiledMapImporter::default()
            .import_map(ImportSource::new("anim.json", map.as_bytes()))
            .expect("anim")
            .asset;
        assert_eq!(imported.tile_animations().len(), 1);
        assert_eq!(imported.tile_animations()[0].base_region, 0);
        assert_eq!(
            imported.tile_animations()[0].frames,
            vec![(0, 100), (1, 100)]
        );
        let mut textures = Assets::<Texture>::new();
        let size = TextureSize::new(32, 16).expect("size");
        let handle = textures.insert(Texture::new(size));
        let bound = imported.bind_texture(handle).expect("bind");
        assert_eq!(bound.tile_map().region_animations().len(), 1);
    }

    #[test]
    fn imports_layer_parallax_and_binds() {
        let map = DEMO_MAP.replace(
            r#""name": "ground","#,
            r#""name": "ground",
          "parallaxx": 0.5,
          "parallaxy": 0.25,"#,
        );
        let imported = TiledMapImporter::default()
            .import_map(ImportSource::new("parallax.json", map.as_bytes()))
            .expect("parallax")
            .asset;
        assert_eq!(imported.visual_layers()[0].parallax(), [0.5, 0.25]);
        let mut textures = Assets::<Texture>::new();
        let size = TextureSize::new(32, 16).expect("size");
        let handle = textures.insert(Texture::new(size));
        let bound = imported.bind_texture(handle).expect("bind");
        assert_eq!(bound.tile_map().parallax_factor, [0.5, 0.25]);
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
        let (maps, collision) = bound.into_components();
        assert_eq!(maps.len(), 1);
        assert_eq!(maps[0].grid(), [4, 3]);
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
    fn imports_horizontal_flip_flag() {
        let map = DEMO_MAP.replace(
            "3,1,1,3",
            &format!("{},1,1,3", 1_u32 | FLIPPED_HORIZONTALLY),
        );
        let imported = TiledMapImporter::default()
            .import_map(ImportSource::new("flip.json", map.as_bytes()))
            .expect("flips supported")
            .asset;
        assert_eq!(imported.cells()[4], Some(0));
        assert!(imported.flips()[4].horizontal);
        assert!(!imported.flips()[4].vertical);
        assert!(!imported.flips()[4].diagonal);
        let mut textures = Assets::<Texture>::new();
        let size = TextureSize::new(32, 16).expect("size");
        let handle = textures.insert(Texture::new(size));
        let bound = imported.bind_texture(handle).expect("bind");
        assert!(bound.tile_map().flips()[4].horizontal);
    }

    #[test]
    fn imports_diagonal_flip_flag() {
        let map = DEMO_MAP.replace(
            "3,1,1,3",
            &format!("{},1,1,3", 1_u32 | FLIPPED_DIAGONALLY),
        );
        let imported = TiledMapImporter::default()
            .import_map(ImportSource::new("diag.json", map.as_bytes()))
            .expect("diagonal flip")
            .asset;
        assert!(imported.flips()[4].diagonal);
        let (size, rotation) = imported.flips()[4].to_draw_size_rotation([8.0, 16.0]);
        assert!((rotation - std::f32::consts::FRAC_PI_2).abs() < 1e-5);
        assert_eq!(size, [8.0, -16.0]);
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

    const DEMO_MAP_TMX: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<map orientation="orthogonal" renderorder="right-down" width="4" height="3" tilewidth="8" tileheight="16" infinite="0">
 <tileset firstgid="1" name="demo_atlas" tilewidth="8" tileheight="16" tilecount="4" columns="4">
  <image source="demo_atlas.png" width="32" height="16"/>
  <tile id="2">
   <properties>
    <property name="solid" type="bool" value="true"/>
   </properties>
  </tile>
 </tileset>
 <layer name="ground" width="4" height="3" visible="1">
  <data encoding="csv">
3,3,3,3,
3,1,1,3,
3,3,3,3
</data>
 </layer>
 <layer name="collision" width="4" height="3" visible="0">
  <data encoding="csv">
1,1,1,1,
1,0,0,1,
1,1,1,1
</data>
 </layer>
 <objectgroup name="objects">
  <object name="spawn" type="player_spawn" x="12" y="24">
   <point/>
   <properties>
    <property name="tag" value="start"/>
   </properties>
  </object>
  <object name="door" type="portal" x="8" y="16" width="8" height="16">
   <properties>
    <property name="target" value="house_interior"/>
   </properties>
  </object>
 </objectgroup>
</map>"#;

    const EXTERNAL_TILESET_TSX: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<tileset name="demo_atlas" tilewidth="8" tileheight="16" tilecount="4" columns="4">
 <image source="demo_atlas.png" width="32" height="16"/>
 <tile id="2">
  <properties>
   <property name="solid" type="bool" value="true"/>
  </properties>
 </tile>
</tileset>"#;

    const EXTERNAL_MAP_TMX: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<map orientation="orthogonal" renderorder="right-down" width="4" height="3" tilewidth="8" tileheight="16" infinite="0">
 <tileset firstgid="1" source="demo_atlas.tsx"/>
 <layer name="ground" width="4" height="3" visible="1">
  <data encoding="csv">3,3,3,3,3,1,1,3,3,3,3,3</data>
 </layer>
 <layer name="collision" width="4" height="3" visible="0">
  <data encoding="csv">1,1,1,1,1,0,0,1,1,1,1,1</data>
 </layer>
</map>"#;

    #[test]
    fn imports_tmx_matches_json_room() {
        let json = TiledMapImporter::default()
            .import_map(ImportSource::new("demo.json", DEMO_MAP.as_bytes()))
            .expect("json")
            .asset;
        let tmx = TiledMapImporter::default()
            .import_map(ImportSource::new("demo.tmx", DEMO_MAP_TMX.as_bytes()))
            .expect("tmx")
            .asset;
        assert_eq!(tmx.grid(), json.grid());
        assert_eq!(tmx.cells(), json.cells());
        assert_eq!(tmx.solid(), json.solid());
        assert_eq!(tmx.image_uri(), json.image_uri());
        assert_eq!(tmx.object_layers().len(), json.object_layers().len());
        assert_eq!(
            tmx.object_layers()[0].objects()[0].class(),
            "player_spawn"
        );
        assert_eq!(
            tmx.object_layers()[0].objects()[1].class(),
            "portal"
        );
    }

    #[test]
    fn imports_tmx_xml_tile_gid_list() {
        let map = DEMO_MAP_TMX.replace(
            r#"<data encoding="csv">
3,3,3,3,
3,1,1,3,
3,3,3,3
</data>"#,
            r#"<data>
 <tile gid="3"/><tile gid="3"/><tile gid="3"/><tile gid="3"/>
 <tile gid="3"/><tile gid="1"/><tile gid="1"/><tile gid="3"/>
 <tile gid="3"/><tile gid="3"/><tile gid="3"/><tile gid="3"/>
</data>"#,
        );
        let imported = TiledMapImporter::default()
            .import_map(ImportSource::new("xml_tiles.tmx", map.as_bytes()))
            .expect("xml tile list")
            .asset;
        assert_eq!(imported.cells()[0], Some(2));
        assert_eq!(imported.cells()[5], Some(0));
    }

    #[test]
    fn imports_external_tsx_tileset() {
        let output = TiledMapImporter::default()
            .import_map_with_external_tilesets(
                ImportSource::new("external.tmx", EXTERNAL_MAP_TMX.as_bytes()),
                &[ExternalTilesetBytes::new(
                    "demo_atlas.tsx",
                    EXTERNAL_TILESET_TSX.as_bytes(),
                )],
            )
            .expect("external tsx");
        assert_eq!(output.asset.grid(), [4, 3]);
        assert_eq!(output.asset.image_uri(), "demo_atlas.png");
        assert_eq!(output.asset.cells()[0], Some(2));
        assert_eq!(
            output
                .dependencies
                .iter()
                .map(|dep| dep.uri.as_str())
                .collect::<Vec<_>>(),
            vec!["demo_atlas.tsx", "demo_atlas.png"]
        );
    }

    #[test]
    fn imports_base64_tmx_layer_data() {
        use base64::Engine;
        use std::io::Write;

        let gids = [3_u32, 3, 3, 3, 3, 1, 1, 3, 3, 3, 3, 3];
        let mut raw = Vec::with_capacity(gids.len() * 4);
        for gid in gids {
            raw.extend_from_slice(&gid.to_le_bytes());
        }
        let plain = base64::engine::general_purpose::STANDARD.encode(&raw);
        let mut zlib_bytes = Vec::new();
        {
            let mut encoder =
                flate2::write::ZlibEncoder::new(&mut zlib_bytes, flate2::Compression::default());
            encoder.write_all(&raw).expect("zlib");
            encoder.finish().expect("zlib finish");
        }
        let zlib = base64::engine::general_purpose::STANDARD.encode(&zlib_bytes);

        let plain_map = DEMO_MAP_TMX.replace(
            r#"<data encoding="csv">
3,3,3,3,
3,1,1,3,
3,3,3,3
</data>"#,
            &format!(r#"<data encoding="base64">{plain}</data>"#),
        );
        let imported = TiledMapImporter::default()
            .import_map(ImportSource::new("b64.tmx", plain_map.as_bytes()))
            .expect("base64")
            .asset;
        assert_eq!(imported.cells()[0], Some(2));
        assert_eq!(imported.cells()[5], Some(0));

        let zlib_map = DEMO_MAP_TMX.replace(
            r#"<data encoding="csv">
3,3,3,3,
3,1,1,3,
3,3,3,3
</data>"#,
            &format!(r#"<data encoding="base64" compression="zlib">{zlib}</data>"#),
        );
        let zlib_imported = TiledMapImporter::default()
            .import_map(ImportSource::new("zlib.tmx", zlib_map.as_bytes()))
            .expect("zlib base64")
            .asset;
        assert_eq!(zlib_imported.cells(), imported.cells());

        let zstd_bytes = zstd::encode_all(raw.as_slice(), 0).expect("zstd encode");
        let zstd_b64 = base64::engine::general_purpose::STANDARD.encode(&zstd_bytes);
        let zstd_map = DEMO_MAP_TMX.replace(
            r#"<data encoding="csv">
3,3,3,3,
3,1,1,3,
3,3,3,3
</data>"#,
            &format!(r#"<data encoding="base64" compression="zstd">{zstd_b64}</data>"#),
        );
        let zstd_imported = TiledMapImporter::default()
            .import_map(ImportSource::new("zstd.tmx", zstd_map.as_bytes()))
            .expect("zstd base64")
            .asset;
        assert_eq!(zstd_imported.cells(), imported.cells());
    }

    #[test]
    fn rejects_unknown_tmx_compression() {
        let map = DEMO_MAP_TMX.replace(
            r#"encoding="csv""#,
            r#"encoding="base64" compression="lz4""#,
        );
        let error = TiledMapImporter::default()
            .import_map(ImportSource::new("lz4.tmx", map.as_bytes()))
            .expect_err("lz4 unsupported");
        assert!(matches!(error, TiledImportError::Unsupported(_)));
    }

    const MULTI_TILESET_MAP: &str = r#"{
      "type": "map",
      "orientation": "orthogonal",
      "renderorder": "right-down",
      "width": 2,
      "height": 2,
      "tilewidth": 8,
      "tileheight": 16,
      "tilesets": [
        {
          "firstgid": 1,
          "image": "ground.png",
          "imagewidth": 16,
          "imageheight": 16,
          "tilewidth": 8,
          "tileheight": 16,
          "tilecount": 2,
          "columns": 2,
          "tiles": [
            { "id": 0, "properties": [{ "name": "solid", "type": "bool", "value": true }] }
          ]
        },
        {
          "firstgid": 10,
          "image": "deco.png",
          "imagewidth": 16,
          "imageheight": 16,
          "tilewidth": 8,
          "tileheight": 16,
          "tilecount": 2,
          "columns": 2
        }
      ],
      "layers": [
        {
          "type": "tilelayer",
          "name": "ground",
          "width": 2,
          "height": 2,
          "visible": true,
          "data": [1, 11, 0, 10]
        }
      ]
    }"#;

    #[test]
    fn imports_and_binds_multiple_tilesets() {
        let imported = TiledMapImporter::default()
            .import_map(ImportSource::new("multi_ts.json", MULTI_TILESET_MAP.as_bytes()))
            .expect("multi tileset")
            .asset;
        assert_eq!(imported.tilesets().len(), 2);
        assert_eq!(imported.tilesets()[0].region_base(), 0);
        assert_eq!(imported.tilesets()[1].region_base(), 2);
        assert_eq!(imported.region_count(), 4);
        // gid 1 → base0+0; gid 11 → base2+1; gid 10 → base2+0
        assert_eq!(imported.cells(), &[Some(0), Some(3), None, Some(2)]);
        assert!(imported.solid()[0]);
        assert!(!imported.solid()[1]);

        let mut textures = Assets::<Texture>::new();
        let size = TextureSize::new(16, 16).expect("size");
        let ground = textures.insert(Texture::new(size));
        let deco = textures.insert(Texture::new(size));
        let bound = imported
            .bind_textures_with_world_tile_size(
                &[("ground.png", ground), ("deco.png", deco)],
                [8.0, 16.0],
            )
            .expect("bind multi");
        assert_eq!(bound.tile_map().regions().len(), 4);
        assert_ne!(
            bound.tile_map().regions()[0].texture(),
            bound.tile_map().regions()[2].texture()
        );
    }

    #[test]
    fn rejects_overlapping_tileset_gid_ranges() {
        let map = MULTI_TILESET_MAP.replace("\"firstgid\": 10", "\"firstgid\": 2");
        let error = TiledMapImporter::default()
            .import_map(ImportSource::new("overlap.json", map.as_bytes()))
            .expect_err("overlap");
        assert!(matches!(error, TiledImportError::Unsupported(_)));
    }

    #[test]
    fn bind_texture_rejects_multi_tileset_maps() {
        let imported = TiledMapImporter::default()
            .import_map(ImportSource::new("multi_ts.json", MULTI_TILESET_MAP.as_bytes()))
            .expect("multi tileset")
            .asset;
        let mut textures = Assets::<Texture>::new();
        let size = TextureSize::new(16, 16).expect("size");
        let handle = textures.insert(Texture::new(size));
        let error = imported.bind_texture(handle).expect_err("need bind_textures");
        assert!(matches!(
            error,
            TiledBindError::TextureCount {
                expected: 1,
                actual: 2
            }
        ));
    }

    const LDTK_UNIT_ROOM: &str = r#"{
      "jsonVersion": "1.5.3",
      "worldLayout": "Free",
      "defs": {
        "tilesets": [{
          "uid": 1,
          "identifier": "demo_atlas",
          "relPath": "demo_atlas_8.png",
          "pxWid": 32,
          "pxHei": 8,
          "tileGridSize": 8,
          "spacing": 0,
          "padding": 0
        }]
      },
      "levels": [{
        "identifier": "UnitRoom",
        "iid": "level_unit",
        "worldX": 64,
        "worldY": -32,
        "pxWid": 32,
        "pxHei": 24,
        "__neighbours": [
          { "levelIid": "level_east", "dir": "e" }
        ],
        "layerInstances": [
          {
            "__type": "Entities",
            "__identifier": "Entities",
            "__cWid": 4,
            "__cHei": 3,
            "__gridSize": 8,
            "entityInstances": [
              {
                "__identifier": "player_spawn",
                "iid": "spawn",
                "px": [12, 20],
                "width": 0,
                "height": 0,
                "fieldInstances": [{ "__identifier": "tag", "__value": "start" }]
              }
            ]
          },
          {
            "__type": "IntGrid",
            "__identifier": "Collisions",
            "__cWid": 4,
            "__cHei": 3,
            "__gridSize": 8,
            "intGridCsv": [1,1,1,1, 1,0,0,1, 1,1,1,1]
          },
          {
            "__type": "Tiles",
            "__identifier": "Ground",
            "__cWid": 4,
            "__cHei": 3,
            "__gridSize": 8,
            "__tilesetDefUid": 1,
            "__tilesetRelPath": "demo_atlas_8.png",
            "gridTiles": [
              { "px": [0,0], "src": [16,0], "f": 0 },
              { "px": [8,0], "src": [16,0], "f": 0 },
              { "px": [16,0], "src": [16,0], "f": 0 },
              { "px": [24,0], "src": [16,0], "f": 0 },
              { "px": [0,8], "src": [16,0], "f": 0 },
              { "px": [8,8], "src": [0,0], "f": 1 },
              { "px": [16,8], "src": [0,0], "f": 0 },
              { "px": [24,8], "src": [16,0], "f": 0 },
              { "px": [0,16], "src": [16,0], "f": 0 },
              { "px": [8,16], "src": [16,0], "f": 0 },
              { "px": [16,16], "src": [16,0], "f": 0 },
              { "px": [24,16], "src": [16,0], "f": 0 }
            ]
          }
        ]
      }]
    }"#;

    #[test]
    fn imports_ldtk_unit_room() {
        let output = LdtkProjectImporter::default()
            .import_project(
                ImportSource::new("unit.ldtk", LDTK_UNIT_ROOM.as_bytes()),
                Some("UnitRoom"),
            )
            .expect("ldtk");
        let map = output.asset;
        assert_eq!(map.grid(), [4, 3]);
        assert_eq!(map.tile_pixel_size(), [8, 8]);
        assert_eq!(map.image_uri(), "demo_atlas_8.png");
        assert_eq!(map.visual_layer(), "Ground");
        assert_eq!(map.cells()[0], Some(2)); // src [16,0] → col 2
        assert_eq!(map.cells()[5], Some(0));
        assert!(map.flips()[5].horizontal);
        assert!(map.solid()[0]);
        assert!(!map.solid()[5]);
        assert_eq!(map.object_layers().len(), 1);
        assert_eq!(map.object_layers()[0].objects()[0].class(), "player_spawn");
        assert_eq!(
            output.dependencies[0].uri.as_str(),
            "demo_atlas_8.png"
        );
        let mut textures = Assets::<Texture>::new();
        let size = TextureSize::new(32, 8).expect("size");
        let handle = textures.insert(Texture::new(size));
        let bound = map.bind_texture(handle).expect("bind");
        assert_eq!(bound.tile_map().grid(), [4, 3]);
    }

    #[test]
    fn imports_ldtk_world_layout() {
        let world = LdtkProjectImporter::default()
            .import_world_layout(ImportSource::new("unit.ldtk", LDTK_UNIT_ROOM.as_bytes()))
            .expect("world");
        assert_eq!(world.layout, LdtkWorldLayoutKind2d::Free);
        assert_eq!(world.levels.len(), 1);
        let level = &world.levels[0];
        assert_eq!(level.identifier, "UnitRoom");
        assert_eq!(level.iid, "level_unit");
        assert_eq!(level.world_px, [64, -32]);
        assert_eq!(level.size_px, [32, 24]);
        assert_eq!(level.neighbours.len(), 1);
        assert_eq!(level.neighbours[0].level_iid, "level_east");
        assert_eq!(level.neighbours[0].dir, "e");
    }

    #[test]
    fn imports_ldtk_layer_pixel_offset() {
        let project = LDTK_UNIT_ROOM.replace(
            r#""__tilesetRelPath": "demo_atlas_8.png","#,
            r#""__tilesetRelPath": "demo_atlas_8.png",
            "__pxTotalOffsetX": 8,
            "__pxTotalOffsetY": 16,"#,
        );
        let imported = LdtkProjectImporter::default()
            .import_project(ImportSource::new("offset.ldtk", project.as_bytes()), None)
            .expect("offset")
            .asset;
        assert_eq!(imported.visual_layers()[0].offset_px(), [8.0, 16.0]);
        let mut textures = Assets::<Texture>::new();
        let size = TextureSize::new(32, 8).expect("size");
        let handle = textures.insert(Texture::new(size));
        let bound = imported
            .bind_texture_with_world_tile_size(handle, [8.0, 8.0])
            .expect("bind");
        assert_eq!(bound.tile_map().position, [8.0, 16.0]);
    }

    #[test]
    fn rejects_ldtk_external_levels() {
        let stub = r#"{
          "jsonVersion":"1.5.3",
          "defs":{"tilesets":[{
            "uid":1,"relPath":"a.png","pxWid":8,"pxHei":8,"tileGridSize":8
          }]},
          "levels":[{"identifier":"A","externalRelPath":"levels/A.ldtkl","layerInstances":null}]
        }"#;
        let error = LdtkProjectImporter::default()
            .import_project(ImportSource::new("ext.ldtk", stub.as_bytes()), None)
            .expect_err("external levels");
        assert!(matches!(
            error,
            TiledImportError::ExternalLdtkLevelUnresolved { .. }
        ));
    }

    const LDTK_EXTERNAL_LEVEL: &str = r#"{
      "identifier": "UnitRoom",
      "layerInstances": [
        {
          "__type": "Tiles",
          "__identifier": "Ground",
          "__cWid": 2,
          "__cHei": 2,
          "__gridSize": 8,
          "__tilesetDefUid": 1,
          "__tilesetRelPath": "demo_atlas_8.png",
          "gridTiles": [
            { "px": [0,0], "src": [0,0], "f": 0 },
            { "px": [8,0], "src": [8,0], "f": 0 },
            { "px": [0,8], "src": [0,0], "f": 0 },
            { "px": [8,8], "src": [8,0], "f": 0 }
          ]
        }
      ]
    }"#;

    const LDTK_EXTERNAL_PROJECT: &str = r#"{
      "jsonVersion": "1.5.3",
      "defs": {
        "tilesets": [{
          "uid": 1,
          "relPath": "demo_atlas_8.png",
          "pxWid": 32,
          "pxHei": 8,
          "tileGridSize": 8,
          "spacing": 0,
          "padding": 0
        }]
      },
      "levels": [{
        "identifier": "UnitRoom",
        "externalRelPath": "levels/UnitRoom.ldtkl",
        "layerInstances": null
      }]
    }"#;

    #[test]
    fn imports_ldtk_external_level_when_host_resolves() {
        let output = LdtkProjectImporter::default()
            .import_project_with_external_levels(
                ImportSource::new("project.ldtk", LDTK_EXTERNAL_PROJECT.as_bytes()),
                Some("UnitRoom"),
                &[ExternalLdtkLevelBytes::new(
                    "levels/UnitRoom.ldtkl",
                    LDTK_EXTERNAL_LEVEL.as_bytes(),
                )],
            )
            .expect("external ldtkl");
        assert_eq!(output.asset.grid(), [2, 2]);
        assert_eq!(output.asset.cells()[0], Some(0));
        assert_eq!(
            output
                .dependencies
                .iter()
                .map(|dep| dep.uri.as_str())
                .collect::<Vec<_>>(),
            vec!["levels/UnitRoom.ldtkl", "demo_atlas_8.png"]
        );
    }

    const LDTK_MULTI_TILESET: &str = r#"{
      "jsonVersion": "1.5.3",
      "defs": {
        "tilesets": [
          {
            "uid": 1,
            "relPath": "ground.png",
            "pxWid": 16,
            "pxHei": 8,
            "tileGridSize": 8,
            "spacing": 0,
            "padding": 0
          },
          {
            "uid": 2,
            "relPath": "deco.png",
            "pxWid": 16,
            "pxHei": 8,
            "tileGridSize": 8,
            "spacing": 0,
            "padding": 0
          }
        ]
      },
      "levels": [{
        "identifier": "Multi",
        "layerInstances": [
          {
            "__type": "Tiles",
            "__identifier": "Deco",
            "__cWid": 2,
            "__cHei": 2,
            "__gridSize": 8,
            "__tilesetDefUid": 2,
            "__tilesetRelPath": "deco.png",
            "gridTiles": [
              { "px": [8,8], "src": [8,0], "f": 0 }
            ]
          },
          {
            "__type": "Tiles",
            "__identifier": "Ground",
            "__cWid": 2,
            "__cHei": 2,
            "__gridSize": 8,
            "__tilesetDefUid": 1,
            "__tilesetRelPath": "ground.png",
            "gridTiles": [
              { "px": [0,0], "src": [0,0], "f": 0 },
              { "px": [8,0], "src": [8,0], "f": 0 },
              { "px": [0,8], "src": [0,0], "f": 0 },
              { "px": [8,8], "src": [0,0], "f": 0 }
            ]
          }
        ]
      }]
    }"#;

    #[test]
    fn imports_ldtk_multi_tileset_and_binds() {
        let imported = LdtkProjectImporter::default()
            .import_project(
                ImportSource::new("multi.ldtk", LDTK_MULTI_TILESET.as_bytes()),
                None,
            )
            .expect("multi")
            .asset;
        // Document visual order after reverse: Ground then Deco
        assert_eq!(imported.visual_layers().len(), 2);
        assert_eq!(imported.tilesets().len(), 2);
        assert_eq!(imported.tilesets()[0].region_base(), 0);
        assert_eq!(imported.tilesets()[1].region_base(), 2);
        // Deco tile at (1,1): tileset2 local 1 → global 3
        assert_eq!(imported.visual_layers()[1].cells()[3], Some(3));
        let mut textures = Assets::<Texture>::new();
        let size = TextureSize::new(16, 8).expect("size");
        let ground = textures.insert(Texture::new(size));
        let deco = textures.insert(Texture::new(size));
        let bound = imported
            .bind_textures_with_world_tile_size(
                &[("ground.png", ground), ("deco.png", deco)],
                [8.0, 8.0],
            )
            .expect("bind");
        assert_eq!(bound.tile_maps().len(), 2);
        assert_eq!(bound.tile_maps()[0].regions().len(), 4);
    }
}
