//! Bounded LDtk JSON → [`ImportedTiledMap`] importer (M7).
//!
//! Orthogonal square-grid levels with `Tiles` / `AutoLayer`, optional `IntGrid`
//! solid, and `Entities` as point/rect objects. Embedded levels work out of the
//! box; separate `.ldtkl` files are resolved when the host supplies
//! [`ExternalLdtkLevelBytes`]. Multiple tilesets are concatenated into global
//! region indices (same bind contract as Tiled multi-tileset).
//!
//! World placement (`worldLayout`, `worldX`/`worldY`, `__neighbours`) is a
//! separate readout via [`LdtkProjectImporter::import_world_layout`] — it is
//! not folded into [`ImportedTiledMap`].

use serde::Deserialize;
use yuyib_2d::{PixelPoint, TextureSize};
use yuyib_assets::{
    AssetImporter, ImportContext, ImportDependency, ImportDependencyKind, ImportMatch, ImportProbe,
    ImportSource, ImporterDescriptor, ImporterOutput, ImporterRegistrationError, ImporterRegistry,
};
use yuyib_game_2d::TileFlip2d;

use super::{
    ImportedTiledMap, ImportedTiledObject2d, ImportedTiledObjectLayer2d, ImportedTiledTileset2d,
    ImportedTiledVisualLayer2d, TiledAssembleError, TiledImportError, TiledImportLimits,
    TiledImportLimitsError, TiledPropertyValue, strip_utf8_bom,
};

/// Media type for LDtk project JSON.
pub const LDTK_PROJECT_MEDIA_TYPE: &str = "application/vnd.yuyib.ldtk-project+json";

const FORMAT_NAME: &str = "ldtk.project";

/// Host-supplied external LDtk level document (`uri` = level `externalRelPath`).
#[derive(Clone, Copy, Debug)]
pub struct ExternalLdtkLevelBytes<'a> {
    /// Logical URI matching the project level `externalRelPath`.
    pub uri: &'a str,
    /// Complete `.ldtkl` / level JSON bytes.
    pub bytes: &'a [u8],
}

impl<'a> ExternalLdtkLevelBytes<'a> {
    /// Borrows one host-resolved level document.
    #[must_use]
    pub const fn new(uri: &'a str, bytes: &'a [u8]) -> Self {
        Self { uri, bytes }
    }
}

/// LDtk project `worldLayout` value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LdtkWorldLayoutKind2d {
    /// Free placement (`Free`).
    Free,
    /// GridVania / metroidvania grid (`GridVania`).
    GridVania,
    /// Horizontal linear strip (`LinearHorizontal`).
    LinearHorizontal,
    /// Vertical linear strip (`LinearVertical`).
    LinearVertical,
    /// Unknown / future LDtk value — still imported for diagnostics.
    Unknown,
}

impl LdtkWorldLayoutKind2d {
    fn parse(raw: Option<&str>) -> Self {
        match raw {
            None | Some("") | Some("Free") => Self::Free,
            Some("GridVania") => Self::GridVania,
            Some("LinearHorizontal") => Self::LinearHorizontal,
            Some("LinearVertical") => Self::LinearVertical,
            Some(_) => Self::Unknown,
        }
    }
}

/// One `__neighbours` entry on an LDtk level.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LdtkLevelNeighbour2d {
    /// Target level instance id (`levelIid`).
    pub level_iid: String,
    /// Cardinal / corner direction string from LDtk (`n`, `s`, `e`, `w`, …).
    pub dir: String,
}

/// World-space placement of one LDtk level (no tile payload).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LdtkLevelPlacement2d {
    /// Level `identifier`.
    pub identifier: String,
    /// Level instance id (`iid`).
    pub iid: String,
    /// World origin in pixels (`worldX`, `worldY`).
    pub world_px: [i32; 2],
    /// Level size in pixels (`pxWid`, `pxHei`).
    pub size_px: [u32; 2],
    /// Neighbour links (`__neighbours`).
    pub neighbours: Vec<LdtkLevelNeighbour2d>,
}

/// Project-wide world layout readout (separate from map tile import).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LdtkWorld2d {
    /// Project `worldLayout`.
    pub layout: LdtkWorldLayoutKind2d,
    /// Every level's placement metadata (order matches the project file).
    pub levels: Vec<LdtkLevelPlacement2d>,
}

/// Built-in importer for LDtk project JSON.
#[derive(Clone, Copy, Debug)]
pub struct LdtkProjectImporter {
    limits: TiledImportLimits,
}

impl LdtkProjectImporter {
    /// Creates an importer with explicit limits (shared Tiled trust budget).
    ///
    /// # Errors
    ///
    /// Returns [`TiledImportLimitsError`] when any limit is zero.
    pub fn new(limits: TiledImportLimits) -> Result<Self, TiledImportLimitsError> {
        Ok(Self {
            limits: limits.validate()?,
        })
    }

    /// Imports the first level, or the level matching `level_identifier`.
    ///
    /// Embedded `layerInstances` work with no extra documents. Levels that
    /// store data in a separate `.ldtkl` must use
    /// [`Self::import_project_with_external_levels`].
    ///
    /// # Errors
    ///
    /// Returns [`TiledImportError`] for unsupported / unresolved content.
    pub fn import_project(
        &self,
        source: ImportSource<'_>,
        level_identifier: Option<&str>,
    ) -> Result<ImporterOutput<ImportedTiledMap>, TiledImportError> {
        self.import_project_with_external_levels(source, level_identifier, &[])
    }

    /// Parses a project and resolves external `.ldtkl` documents supplied by the host.
    ///
    /// Each selected level with `layerInstances: null` must have an
    /// `externalRelPath` that matches exactly one entry in `external_levels`.
    ///
    /// # Errors
    ///
    /// Returns [`TiledImportError`] for missing external levels, limit
    /// violations, or malformed documents.
    pub fn import_project_with_external_levels(
        &self,
        source: ImportSource<'_>,
        level_identifier: Option<&str>,
        external_levels: &[ExternalLdtkLevelBytes<'_>],
    ) -> Result<ImporterOutput<ImportedTiledMap>, TiledImportError> {
        if source.bytes().len() > self.limits.max_manifest_bytes {
            return Err(TiledImportError::ManifestTooLarge {
                bytes: source.bytes().len(),
                limit: self.limits.max_manifest_bytes,
            });
        }
        let project: RawProject = serde_json::from_slice(strip_utf8_bom(source.bytes()))?;
        let (imported, mut dependencies) =
            convert_project(&project, &self.limits, level_identifier, external_levels)?;
        let cpu_bytes = super::estimate_cpu_bytes(&imported);
        for tileset in imported.tilesets() {
            dependencies.push(ImportDependency {
                uri: tileset.image_uri().to_owned(),
                kind: ImportDependencyKind::Required,
            });
        }
        let mut output = ImporterOutput::new(imported);
        output.cpu_bytes = Some(cpu_bytes as u64);
        output.dependencies = dependencies;
        Ok(output)
    }

    /// Reads world layout / level placements without assembling tile maps.
    ///
    /// Does not require external `.ldtkl` documents — only project-level
    /// placement fields (`worldX`/`worldY`/`pxWid`/`pxHei`/`__neighbours`).
    ///
    /// # Errors
    ///
    /// Returns [`TiledImportError`] for oversized or malformed project JSON.
    pub fn import_world_layout(
        &self,
        source: ImportSource<'_>,
    ) -> Result<LdtkWorld2d, TiledImportError> {
        if source.bytes().len() > self.limits.max_manifest_bytes {
            return Err(TiledImportError::ManifestTooLarge {
                bytes: source.bytes().len(),
                limit: self.limits.max_manifest_bytes,
            });
        }
        let project: RawProject = serde_json::from_slice(strip_utf8_bom(source.bytes()))?;
        Ok(convert_world_layout(&project))
    }
}

impl Default for LdtkProjectImporter {
    fn default() -> Self {
        Self::new(TiledImportLimits::default())
            .expect("default tiled import limits are valid")
    }
}

impl AssetImporter<ImportedTiledMap> for LdtkProjectImporter {
    type Error = TiledImportError;

    fn descriptor(&self) -> ImporterDescriptor {
        ImporterDescriptor::new(FORMAT_NAME, env!("CARGO_PKG_VERSION"))
            .with_extension("ldtk")
            .with_media_type(LDTK_PROJECT_MEDIA_TYPE)
    }

    fn probe(&self, probe: ImportProbe<'_>) -> ImportMatch {
        if probe.media_type == Some(LDTK_PROJECT_MEDIA_TYPE) {
            return ImportMatch::Exact;
        }
        let looks = contains_ldtk_marker(probe.prefix);
        if probe.extension == Some("ldtk") && looks {
            ImportMatch::Preferred
        } else if looks {
            ImportMatch::Possible
        } else {
            ImportMatch::Unsupported
        }
    }

    fn import(
        &self,
        source: ImportSource<'_>,
    ) -> Result<ImporterOutput<ImportedTiledMap>, Self::Error> {
        self.import_project(source, None)
    }

    fn import_with_context(
        &self,
        source: ImportSource<'_>,
        _context: ImportContext<'_>,
    ) -> Result<ImporterOutput<ImportedTiledMap>, Self::Error> {
        self.import_project(source, None)
    }
}

/// Registers [`LdtkProjectImporter`] with default limits.
///
/// # Errors
///
/// Forwards registry registration failures.
pub fn register_ldtk_project_importer(
    registry: &mut ImporterRegistry<ImportedTiledMap>,
) -> Result<(), ImporterRegistrationError> {
    registry.register(LdtkProjectImporter::default())
}

fn contains_ldtk_marker(prefix: &[u8]) -> bool {
    let prefix = strip_utf8_bom(prefix);
    const NEEDLES: [&[u8]; 3] = [
        br#""ldtk""#,
        br#""jsonVersion""#,
        br#""layerInstances""#,
    ];
    NEEDLES.iter().any(|needle| {
        prefix
            .windows(needle.len())
            .any(|window| window == *needle)
    })
}

#[derive(Debug, Deserialize)]
struct RawProject {
    #[serde(default, rename = "worldLayout")]
    world_layout: Option<String>,
    defs: RawDefs,
    levels: Vec<RawLevel>,
}

#[derive(Debug, Deserialize)]
struct RawDefs {
    tilesets: Vec<RawTilesetDef>,
}

#[derive(Debug, Deserialize, Clone)]
struct RawTilesetDef {
    uid: i64,
    #[serde(rename = "relPath")]
    rel_path: Option<String>,
    #[serde(rename = "pxWid")]
    px_wid: u32,
    #[serde(rename = "pxHei")]
    px_hei: u32,
    #[serde(rename = "tileGridSize")]
    tile_grid_size: u32,
    #[serde(default)]
    spacing: u32,
    #[serde(default)]
    padding: u32,
}

impl RawTilesetDef {
    fn rel_path(&self) -> Option<&str> {
        self.rel_path.as_deref().filter(|path| !path.is_empty())
    }
}

#[derive(Debug, Deserialize)]
struct RawLevel {
    identifier: String,
    #[serde(default)]
    iid: String,
    #[serde(default, rename = "worldX")]
    world_x: i32,
    #[serde(default, rename = "worldY")]
    world_y: i32,
    #[serde(default, rename = "pxWid")]
    px_wid: u32,
    #[serde(default, rename = "pxHei")]
    px_hei: u32,
    #[serde(default, rename = "__neighbours")]
    neighbours: Vec<RawNeighbour>,
    #[serde(rename = "externalRelPath")]
    external_rel_path: Option<String>,
    #[serde(rename = "layerInstances")]
    layer_instances: Option<Vec<RawLayerInstance>>,
}

#[derive(Debug, Deserialize, Clone)]
struct RawNeighbour {
    #[serde(rename = "levelIid")]
    level_iid: String,
    dir: String,
}

#[derive(Debug, Deserialize, Clone)]
struct RawLayerInstance {
    #[serde(rename = "__type")]
    layer_type: String,
    #[serde(rename = "__identifier")]
    identifier: String,
    #[serde(rename = "__cWid")]
    c_wid: u32,
    #[serde(rename = "__cHei")]
    c_hei: u32,
    #[serde(rename = "__gridSize")]
    grid_size: u32,
    #[serde(rename = "__tilesetDefUid")]
    tileset_def_uid: Option<i64>,
    #[serde(rename = "__tilesetRelPath")]
    tileset_rel_path: Option<String>,
    #[serde(default, rename = "gridTiles")]
    grid_tiles: Vec<RawTileInstance>,
    #[serde(default, rename = "autoLayerTiles")]
    auto_layer_tiles: Vec<RawTileInstance>,
    #[serde(default, rename = "intGridCsv")]
    int_grid_csv: Vec<i32>,
    #[serde(default, rename = "entityInstances")]
    entity_instances: Vec<RawEntityInstance>,
    #[serde(default, rename = "visible")]
    visible: Option<bool>,
    #[serde(default, rename = "__pxTotalOffsetX")]
    px_total_offset_x: f32,
    #[serde(default, rename = "__pxTotalOffsetY")]
    px_total_offset_y: f32,
}

#[derive(Debug, Deserialize, Clone)]
struct RawTileInstance {
    px: [i32; 2],
    src: [i32; 2],
    #[serde(default)]
    f: u32,
}

#[derive(Debug, Deserialize, Clone)]
struct RawEntityInstance {
    #[serde(rename = "__identifier")]
    identifier: String,
    #[serde(default)]
    iid: String,
    #[serde(rename = "px")]
    px: [i32; 2],
    width: i32,
    height: i32,
    #[serde(default, rename = "fieldInstances")]
    field_instances: Vec<RawFieldInstance>,
}

#[derive(Debug, Deserialize, Clone)]
struct RawFieldInstance {
    #[serde(rename = "__identifier")]
    identifier: String,
    #[serde(rename = "__value")]
    value: serde_json::Value,
}

struct ResolvedTileset {
    uid: i64,
    def: RawTilesetDef,
    image_uri: String,
    region_base: u32,
}

fn convert_world_layout(project: &RawProject) -> LdtkWorld2d {
    LdtkWorld2d {
        layout: LdtkWorldLayoutKind2d::parse(project.world_layout.as_deref()),
        levels: project
            .levels
            .iter()
            .map(|level| LdtkLevelPlacement2d {
                identifier: level.identifier.clone(),
                iid: level.iid.clone(),
                world_px: [level.world_x, level.world_y],
                size_px: [level.px_wid, level.px_hei],
                neighbours: level
                    .neighbours
                    .iter()
                    .map(|n| LdtkLevelNeighbour2d {
                        level_iid: n.level_iid.clone(),
                        dir: n.dir.clone(),
                    })
                    .collect(),
            })
            .collect(),
    }
}

fn convert_project(
    project: &RawProject,
    limits: &TiledImportLimits,
    level_identifier: Option<&str>,
    external_levels: &[ExternalLdtkLevelBytes<'_>],
) -> Result<(ImportedTiledMap, Vec<ImportDependency>), TiledImportError> {
    if project.levels.is_empty() {
        return Err(TiledImportError::Unsupported("ldtk project has no levels"));
    }
    let level = if let Some(name) = level_identifier {
        project
            .levels
            .iter()
            .find(|level| level.identifier == name)
            .ok_or(TiledImportError::Unsupported("ldtk level identifier not found"))?
    } else {
        &project.levels[0]
    };

    let (layers, external_dep) = resolve_level_layers(level, limits, external_levels)?;
    if layers.is_empty() {
        return Err(TiledImportError::Unsupported("ldtk level has no layers"));
    }

    let grid = [layers[0].c_wid, layers[0].c_hei];
    let tile_size = layers[0].grid_size;
    if tile_size == 0 {
        return Err(TiledImportError::Unsupported("ldtk grid size must be positive"));
    }
    for layer in &layers {
        if layer.c_wid != grid[0] || layer.c_hei != grid[1] {
            return Err(TiledImportError::Unsupported(
                "ldtk layers must share the same grid size in this slice",
            ));
        }
        if layer.grid_size != tile_size {
            return Err(TiledImportError::Unsupported(
                "ldtk layers must share tileGridSize in this slice",
            ));
        }
    }

    let cell_count = usize::try_from(u64::from(grid[0]) * u64::from(grid[1]))
        .map_err(|_| TiledImportError::Unsupported("ldtk map area too large"))?;

    let mut visual_layers = Vec::new();
    let mut solid = vec![false; cell_count];
    let mut object_layers = Vec::new();
    let mut used_tilesets: Vec<ResolvedTileset> = Vec::new();
    let mut region_base = 0_u32;

    // LDtk lists layers top→bottom; Tiled visual order is bottom→top.
    for layer in layers.iter().rev() {
        if layer.visible == Some(false) {
            continue;
        }
        match layer.layer_type.as_str() {
            "Tiles" | "AutoLayer" => {
                if visual_layers.len() >= limits.max_visual_tile_layers {
                    return Err(TiledImportError::VisualLayerLimit {
                        limit: limits.max_visual_tile_layers,
                        actual: visual_layers.len() + 1,
                    });
                }
                let uid = layer.tileset_def_uid.ok_or(TiledImportError::Unsupported(
                    "ldtk tile layer requires tilesetDefUid",
                ))?;
                let resolved_index =
                    ensure_tileset(&mut used_tilesets, &mut region_base, project, layer, uid, tile_size, limits)?;
                let resolved = &used_tilesets[resolved_index];
                let tiles = if layer.layer_type == "Tiles" {
                    &layer.grid_tiles
                } else {
                    &layer.auto_layer_tiles
                };
                let (cells, flips) =
                    decode_tile_layer(tiles, grid, tile_size, &resolved.def, resolved.region_base)?;
                let offset_px = [layer.px_total_offset_x, layer.px_total_offset_y];
                if !offset_px.iter().all(|value| value.is_finite()) {
                    return Err(TiledImportError::Unsupported(
                        "ldtk layer offset must be finite",
                    ));
                }
                visual_layers.push(
                    ImportedTiledVisualLayer2d::new(layer.identifier.clone(), cells, flips)
                        .map_err(assemble_err)?
                        .with_offset_px(offset_px),
                );
            }
            "IntGrid" => {
                if layer.int_grid_csv.len() != cell_count {
                    return Err(TiledImportError::LayerDataCount {
                        expected: cell_count,
                        actual: layer.int_grid_csv.len(),
                    });
                }
                for (dst, value) in solid.iter_mut().zip(layer.int_grid_csv.iter()) {
                    if *value != 0 {
                        *dst = true;
                    }
                }
            }
            "Entities" => {
                if object_layers.len() >= limits.max_object_layers {
                    return Err(TiledImportError::ObjectLayerLimit {
                        limit: limits.max_object_layers,
                    });
                }
                if layer.entity_instances.len() > limits.max_objects_per_layer {
                    return Err(TiledImportError::ObjectLimit {
                        limit: limits.max_objects_per_layer,
                        actual: layer.entity_instances.len(),
                    });
                }
                let mut objects = Vec::with_capacity(layer.entity_instances.len());
                for entity in &layer.entity_instances {
                    objects.push(convert_entity(entity, limits)?);
                }
                object_layers.push(ImportedTiledObjectLayer2d::new(
                    layer.identifier.clone(),
                    objects,
                ));
            }
            _ => {
                return Err(TiledImportError::Unsupported(
                    "unsupported ldtk layer type in this slice",
                ));
            }
        }
    }

    if visual_layers.is_empty() {
        return Err(TiledImportError::Unsupported(
            "ldtk level has no visible Tiles/AutoLayer",
        ));
    }
    if used_tilesets.is_empty() {
        return Err(TiledImportError::Unsupported(
            "ldtk level has no tileset reference",
        ));
    }
    if used_tilesets.len() > limits.max_tilesets {
        return Err(TiledImportError::TilesetLimit {
            limit: limits.max_tilesets,
            actual: used_tilesets.len(),
        });
    }

    let region_size = TextureSize::new(tile_size, tile_size).map_err(TiledImportError::TextureSize)?;
    let mut imported_tilesets = Vec::with_capacity(used_tilesets.len());
    for resolved in &used_tilesets {
        let region_origins = build_region_origins(&resolved.def, limits)?;
        imported_tilesets.push(ImportedTiledTileset2d::new(
            resolved.image_uri.clone(),
            [resolved.def.px_wid, resolved.def.px_hei],
            region_origins,
            region_size,
            resolved.region_base,
        ));
    }

    let map = ImportedTiledMap::assemble(
        grid,
        [tile_size, tile_size],
        imported_tilesets,
        visual_layers,
        solid,
        object_layers,
    )
    .map_err(assemble_err)?;

    let mut dependencies = Vec::new();
    if let Some(uri) = external_dep {
        dependencies.push(ImportDependency {
            uri,
            kind: ImportDependencyKind::Required,
        });
    }
    Ok((map, dependencies))
}

fn resolve_level_layers(
    level: &RawLevel,
    limits: &TiledImportLimits,
    external_levels: &[ExternalLdtkLevelBytes<'_>],
) -> Result<(Vec<RawLayerInstance>, Option<String>), TiledImportError> {
    if let Some(layers) = &level.layer_instances {
        return Ok((layers.clone(), None));
    }
    let path = level
        .external_rel_path
        .as_deref()
        .filter(|uri| !uri.is_empty())
        .ok_or(TiledImportError::Unsupported(
            "ldtk level has null layerInstances without externalRelPath",
        ))?;
    if path.len() > limits.max_image_uri_bytes {
        return Err(TiledImportError::ExternalLdtkLevelUri);
    }
    let bytes = external_levels
        .iter()
        .find(|doc| doc.uri == path)
        .map(|doc| doc.bytes)
        .ok_or_else(|| TiledImportError::ExternalLdtkLevelUnresolved {
            uri: path.to_owned(),
        })?;
    if bytes.len() > limits.max_manifest_bytes {
        return Err(TiledImportError::ManifestTooLarge {
            bytes: bytes.len(),
            limit: limits.max_manifest_bytes,
        });
    }
    let external: RawLevel = serde_json::from_slice(strip_utf8_bom(bytes))?;
    let layers = external.layer_instances.ok_or(TiledImportError::Unsupported(
        "ldtk external level file has null layerInstances",
    ))?;
    Ok((layers, Some(path.to_owned())))
}

fn ensure_tileset(
    used: &mut Vec<ResolvedTileset>,
    region_base: &mut u32,
    project: &RawProject,
    layer: &RawLayerInstance,
    uid: i64,
    tile_size: u32,
    limits: &TiledImportLimits,
) -> Result<usize, TiledImportError> {
    if let Some(index) = used.iter().position(|tileset| tileset.uid == uid) {
        return Ok(index);
    }
    if used.len() >= limits.max_tilesets {
        return Err(TiledImportError::TilesetLimit {
            limit: limits.max_tilesets,
            actual: used.len() + 1,
        });
    }
    let def = project
        .defs
        .tilesets
        .iter()
        .find(|tileset| tileset.uid == uid)
        .cloned()
        .ok_or(TiledImportError::Unsupported(
            "ldtk tileset def uid not found",
        ))?;
    if def.tile_grid_size != tile_size {
        return Err(TiledImportError::Unsupported(
            "ldtk tileset tileGridSize must match layer grid",
        ));
    }
    let image_uri = layer
        .tileset_rel_path
        .clone()
        .or_else(|| def.rel_path().map(str::to_owned))
        .ok_or(TiledImportError::ImageUri)?;
    if image_uri.is_empty() || image_uri.len() > limits.max_image_uri_bytes {
        return Err(TiledImportError::ImageUri);
    }
    let base = *region_base;
    let tile_count = estimate_tile_count(&def)?;
    *region_base = region_base
        .checked_add(tile_count)
        .ok_or(TiledImportError::Unsupported("ldtk region base overflow"))?;
    used.push(ResolvedTileset {
        uid,
        def,
        image_uri,
        region_base: base,
    });
    Ok(used.len() - 1)
}

fn estimate_tile_count(def: &RawTilesetDef) -> Result<u32, TiledImportError> {
    let tile = def.tile_grid_size;
    if tile == 0 {
        return Err(TiledImportError::Unsupported("ldtk tileGridSize must be positive"));
    }
    let stride = tile.saturating_add(def.spacing);
    let usable_w = def.px_wid.saturating_sub(def.padding);
    let usable_h = def.px_hei.saturating_sub(def.padding);
    if usable_w < tile || usable_h < tile {
        return Err(TiledImportError::Unsupported("ldtk tileset has no tile regions"));
    }
    let columns = (usable_w - tile) / stride + 1;
    let rows = (usable_h - tile) / stride + 1;
    columns
        .checked_mul(rows)
        .ok_or(TiledImportError::Unsupported("ldtk tileset tile count overflow"))
}

fn build_region_origins(
    def: &RawTilesetDef,
    limits: &TiledImportLimits,
) -> Result<Vec<PixelPoint>, TiledImportError> {
    let tile = def.tile_grid_size;
    if tile == 0 {
        return Err(TiledImportError::Unsupported("ldtk tileGridSize must be positive"));
    }
    let mut origins = Vec::new();
    let mut y = def.padding;
    while y + tile <= def.px_hei {
        let mut x = def.padding;
        while x + tile <= def.px_wid {
            if origins.len() as u32 >= limits.max_tileset_tiles {
                return Err(TiledImportError::TilesetTooLarge {
                    tiles: limits.max_tileset_tiles.saturating_add(1),
                    limit: limits.max_tileset_tiles,
                });
            }
            origins.push(PixelPoint { x, y });
            x = x
                .checked_add(tile + def.spacing)
                .ok_or(TiledImportError::Unsupported("ldtk tileset grid overflow"))?;
        }
        y = y
            .checked_add(tile + def.spacing)
            .ok_or(TiledImportError::Unsupported("ldtk tileset grid overflow"))?;
    }
    if origins.is_empty() {
        return Err(TiledImportError::Unsupported("ldtk tileset has no tile regions"));
    }
    Ok(origins)
}

fn decode_tile_layer(
    tiles: &[RawTileInstance],
    grid: [u32; 2],
    tile_size: u32,
    def: &RawTilesetDef,
    region_base: u32,
) -> Result<(Vec<Option<u32>>, Vec<TileFlip2d>), TiledImportError> {
    let expected = usize::try_from(u64::from(grid[0]) * u64::from(grid[1]))
        .map_err(|_| TiledImportError::Unsupported("ldtk map area too large"))?;
    let mut cells = vec![None; expected];
    let mut flips = vec![TileFlip2d::NONE; expected];
    for tile in tiles {
        if tile.px[0] < 0 || tile.px[1] < 0 || tile.src[0] < 0 || tile.src[1] < 0 {
            return Err(TiledImportError::Unsupported(
                "ldtk tile coordinates must be non-negative",
            ));
        }
        let px = [tile.px[0] as u32, tile.px[1] as u32];
        let src = [tile.src[0] as u32, tile.src[1] as u32];
        if px[0] % tile_size != 0 || px[1] % tile_size != 0 {
            return Err(TiledImportError::Unsupported(
                "ldtk tile px must align to grid",
            ));
        }
        let column = px[0] / tile_size;
        let row = px[1] / tile_size;
        if column >= grid[0] || row >= grid[1] {
            return Err(TiledImportError::Unsupported(
                "ldtk tile px outside level grid",
            ));
        }
        let index = usize::try_from(u64::from(row) * u64::from(grid[0]) + u64::from(column))
            .map_err(|_| TiledImportError::Unsupported("ldtk map area too large"))?;
        let local = src_to_region_index(src, tile_size, def)?;
        cells[index] = Some(
            region_base
                .checked_add(local)
                .ok_or(TiledImportError::Unsupported("ldtk global region overflow"))?,
        );
        flips[index] = TileFlip2d {
            horizontal: tile.f & 1 != 0,
            vertical: tile.f & 2 != 0,
            diagonal: false,
        };
    }
    Ok((cells, flips))
}

fn src_to_region_index(
    src: [u32; 2],
    tile_size: u32,
    def: &RawTilesetDef,
) -> Result<u32, TiledImportError> {
    if def.tile_grid_size != tile_size {
        return Err(TiledImportError::Unsupported(
            "ldtk tile src tileset grid mismatch",
        ));
    }
    if src[0] < def.padding || src[1] < def.padding {
        return Err(TiledImportError::Unsupported(
            "ldtk tile src inside padding",
        ));
    }
    let local_x = src[0] - def.padding;
    let local_y = src[1] - def.padding;
    let stride = tile_size + def.spacing;
    if local_x % stride != 0 || local_y % stride != 0 {
        return Err(TiledImportError::Unsupported(
            "ldtk tile src does not align to tileset grid",
        ));
    }
    let usable = def.px_wid.saturating_sub(def.padding);
    if usable < tile_size {
        return Err(TiledImportError::Unsupported("ldtk tileset too small"));
    }
    let columns = (usable - tile_size) / stride + 1;
    let col = local_x / stride;
    let row = local_y / stride;
    if col >= columns {
        return Err(TiledImportError::Unsupported(
            "ldtk tile src column out of range",
        ));
    }
    row.checked_mul(columns)
        .and_then(|value| value.checked_add(col))
        .ok_or(TiledImportError::Unsupported("ldtk tile index overflow"))
}

fn convert_entity(
    entity: &RawEntityInstance,
    limits: &TiledImportLimits,
) -> Result<ImportedTiledObject2d, TiledImportError> {
    let name = if entity.iid.is_empty() {
        entity.identifier.clone()
    } else {
        entity.iid.clone()
    };
    if name.len() > limits.max_object_name_bytes
        || entity.identifier.len() > limits.max_object_name_bytes
    {
        return Err(TiledImportError::ObjectName);
    }
    if entity.width < 0 || entity.height < 0 {
        return Err(TiledImportError::Unsupported(
            "ldtk entity size must be non-negative",
        ));
    }
    let size_px = [entity.width as f32, entity.height as f32];
    let position_px = [entity.px[0] as f32, entity.px[1] as f32];
    if entity.field_instances.len() > limits.max_properties_per_object {
        return Err(TiledImportError::ObjectPropertyLimit {
            limit: limits.max_properties_per_object,
            actual: entity.field_instances.len(),
        });
    }
    let mut properties = Vec::with_capacity(entity.field_instances.len());
    for field in &entity.field_instances {
        if field.identifier.is_empty() || field.identifier.len() > limits.max_object_name_bytes {
            return Err(TiledImportError::ObjectName);
        }
        let value = match &field.value {
            serde_json::Value::Bool(value) => TiledPropertyValue::Bool(*value),
            serde_json::Value::String(value) => {
                if value.len() > limits.max_property_string_bytes {
                    return Err(TiledImportError::ObjectPropertyString);
                }
                TiledPropertyValue::String(value.clone())
            }
            serde_json::Value::Number(value) => {
                let number = value.as_f64().ok_or(TiledImportError::Unsupported(
                    "ldtk field number must be finite f64",
                ))?;
                if !number.is_finite() {
                    return Err(TiledImportError::Unsupported(
                        "ldtk field number must be finite",
                    ));
                }
                TiledPropertyValue::Number(number)
            }
            serde_json::Value::Null => continue,
            _ => {
                return Err(TiledImportError::Unsupported(
                    "ldtk entity fields support bool/string/number only in this slice",
                ));
            }
        };
        properties.push((field.identifier.clone(), value));
    }
    Ok(ImportedTiledObject2d::new(
        name,
        entity.identifier.clone(),
        position_px,
        size_px,
        properties,
    ))
}

fn assemble_err(error: TiledAssembleError) -> TiledImportError {
    TiledImportError::Unsupported(match error {
        TiledAssembleError::ZeroGrid => "ldtk assembled zero grid",
        TiledAssembleError::ZeroTileSize => "ldtk assembled zero tile size",
        TiledAssembleError::GridTooLarge => "ldtk assembled grid too large",
        TiledAssembleError::NoTilesets => "ldtk assembled no tilesets",
        TiledAssembleError::NoVisualLayers => "ldtk assembled no visual layers",
        TiledAssembleError::SolidLen { .. } => "ldtk assembled solid length mismatch",
        TiledAssembleError::LayerCells { .. } => "ldtk assembled layer cell mismatch",
        TiledAssembleError::LayerLen { .. } => "ldtk assembled layer flips mismatch",
    })
}
