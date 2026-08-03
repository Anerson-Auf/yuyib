//! Embedded and external (JSON `.tsj`) tileset resolution.

use yuyib_2d::{PixelPoint, TextureSize};
use yuyib_assets::ImportDependencyKind;

use super::{
    RawTile, TiledImportError, TiledImportLimits, strip_utf8_bom,
};

/// Host-supplied external tileset document (`uri` as written in map `source`).
#[derive(Clone, Copy, Debug)]
pub struct ExternalTilesetBytes<'a> {
    /// Logical URI matching the map tileset `source` field.
    pub uri: &'a str,
    /// Complete tileset JSON bytes (`.tsj` / JSON tileset).
    pub bytes: &'a [u8],
}

impl<'a> ExternalTilesetBytes<'a> {
    /// Borrows one host-resolved tileset document.
    #[must_use]
    pub const fn new(uri: &'a str, bytes: &'a [u8]) -> Self {
        Self { uri, bytes }
    }
}

/// Fully resolved tileset used for GID decode + atlas planning.
#[derive(Clone, Debug)]
pub(super) struct ResolvedTileset {
    pub firstgid: u32,
    pub image_uri: String,
    pub image_w: u32,
    pub image_h: u32,
    pub tilecount: u32,
    pub region_origins: Vec<PixelPoint>,
    pub region_size: TextureSize,
    pub solid_by_local: Vec<bool>,
    /// When the map referenced `source`, this is the tileset document URI.
    pub external_source_uri: Option<String>,
}

impl ResolvedTileset {
    pub(super) fn import_dependencies(
        &self,
    ) -> Vec<yuyib_assets::ImportDependency> {
        let mut deps = Vec::with_capacity(2);
        if let Some(uri) = &self.external_source_uri {
            deps.push(yuyib_assets::ImportDependency {
                uri: uri.clone(),
                kind: ImportDependencyKind::Required,
            });
        }
        deps.push(yuyib_assets::ImportDependency {
            uri: self.image_uri.clone(),
            kind: ImportDependencyKind::Required,
        });
        deps
    }
}

#[derive(Debug, serde::Deserialize)]
pub(super) struct RawTileset {
    pub firstgid: Option<u32>,
    pub name: Option<String>,
    pub image: Option<String>,
    pub imagewidth: Option<u32>,
    pub imageheight: Option<u32>,
    pub tilewidth: Option<u32>,
    pub tileheight: Option<u32>,
    pub tilecount: Option<u32>,
    pub columns: Option<u32>,
    pub margin: Option<u32>,
    pub spacing: Option<u32>,
    pub source: Option<String>,
    pub tiles: Option<Vec<RawTile>>,
    #[serde(rename = "type")]
    pub tileset_type: Option<String>,
}

/// Resolves the single map tileset (embedded or host-provided external JSON).
pub(super) fn resolve_map_tileset(
    entry: &RawTileset,
    map_tile_size: [u32; 2],
    limits: &TiledImportLimits,
    external_tilesets: &[ExternalTilesetBytes<'_>],
) -> Result<ResolvedTileset, TiledImportError> {
    let firstgid = entry
        .firstgid
        .ok_or(TiledImportError::Unsupported("tileset firstgid required"))?;
    if firstgid == 0 {
        return Err(TiledImportError::Unsupported("tileset firstgid must be >= 1"));
    }

    let (body, external_source_uri) = if let Some(source) = entry.source.as_deref() {
        if source.is_empty() || source.len() > limits.max_image_uri_bytes {
            return Err(TiledImportError::ExternalTilesetUri);
        }
        if entry.image.is_some()
            || entry.tilecount.is_some()
            || entry.tilewidth.is_some()
            || entry.tileheight.is_some()
        {
            return Err(TiledImportError::Unsupported(
                "external tileset map entry must not embed image/tile fields",
            ));
        }
        let bytes = external_tilesets
            .iter()
            .find(|doc| doc.uri == source)
            .map(|doc| doc.bytes)
            .ok_or_else(|| TiledImportError::ExternalTilesetUnresolved {
                uri: source.to_owned(),
            })?;
        if bytes.len() > limits.max_manifest_bytes {
            return Err(TiledImportError::ManifestTooLarge {
                bytes: bytes.len(),
                limit: limits.max_manifest_bytes,
            });
        }
        let file: RawTileset = serde_json::from_slice(strip_utf8_bom(bytes))?;
        if let Some(kind) = file.tileset_type.as_deref() {
            if kind != "tileset" {
                return Err(TiledImportError::Unsupported(
                    "external tileset type must be \"tileset\"",
                ));
            }
        }
        if file.source.is_some() {
            return Err(TiledImportError::Unsupported(
                "nested external tileset source is not supported",
            ));
        }
        (file, Some(source.to_owned()))
    } else {
        (clone_entry_as_body(entry), None)
    };

    build_resolved(firstgid, &body, map_tile_size, limits, external_source_uri)
}

fn clone_entry_as_body(entry: &RawTileset) -> RawTileset {
    RawTileset {
        firstgid: entry.firstgid,
        name: entry.name.clone(),
        image: entry.image.clone(),
        imagewidth: entry.imagewidth,
        imageheight: entry.imageheight,
        tilewidth: entry.tilewidth,
        tileheight: entry.tileheight,
        tilecount: entry.tilecount,
        columns: entry.columns,
        margin: entry.margin,
        spacing: entry.spacing,
        source: None,
        tiles: entry.tiles.clone(),
        tileset_type: entry.tileset_type.clone(),
    }
}

fn build_resolved(
    firstgid: u32,
    body: &RawTileset,
    map_tile_size: [u32; 2],
    limits: &TiledImportLimits,
    external_source_uri: Option<String>,
) -> Result<ResolvedTileset, TiledImportError> {
    let tilecount = body
        .tilecount
        .ok_or(TiledImportError::Unsupported("tileset tilecount required"))?;
    if tilecount == 0 || tilecount > limits.max_tileset_tiles {
        return Err(TiledImportError::TilesetTooLarge {
            tiles: tilecount,
            limit: limits.max_tileset_tiles,
        });
    }
    let tilewidth = body
        .tilewidth
        .ok_or(TiledImportError::Unsupported("tileset tilewidth required"))?;
    let tileheight = body
        .tileheight
        .ok_or(TiledImportError::Unsupported("tileset tileheight required"))?;
    if tilewidth == 0 || tileheight == 0 {
        return Err(TiledImportError::Unsupported("tileset tile size must be positive"));
    }
    if tilewidth != map_tile_size[0] || tileheight != map_tile_size[1] {
        return Err(TiledImportError::Unsupported(
            "map tile size must match tileset tile size in this slice",
        ));
    }
    let image_uri = body
        .image
        .as_deref()
        .filter(|uri| !uri.is_empty() && uri.len() <= limits.max_image_uri_bytes)
        .ok_or(TiledImportError::ImageUri)?
        .to_owned();
    let image_w = body
        .imagewidth
        .ok_or(TiledImportError::Unsupported("tileset imagewidth required"))?;
    let image_h = body
        .imageheight
        .ok_or(TiledImportError::Unsupported("tileset imageheight required"))?;
    if image_w == 0 || image_h == 0 {
        return Err(TiledImportError::Unsupported("tileset image size must be positive"));
    }

    let margin = body.margin.unwrap_or(0);
    let spacing = body.spacing.unwrap_or(0);
    let columns = body.columns.unwrap_or_else(|| {
        (image_w.saturating_sub(margin)) / (tilewidth + spacing).max(1)
    });
    if columns == 0 {
        return Err(TiledImportError::Unsupported("tileset columns resolved to zero"));
    }

    let region_size =
        TextureSize::new(tilewidth, tileheight).map_err(TiledImportError::TextureSize)?;
    let mut region_origins = Vec::with_capacity(tilecount as usize);
    for local_id in 0..tilecount {
        let column = local_id % columns;
        let row = local_id / columns;
        let x = margin + column * (tilewidth + spacing);
        let y = margin + row * (tileheight + spacing);
        if x + tilewidth > image_w || y + tileheight > image_h {
            return Err(TiledImportError::Unsupported(
                "tileset tile grid exceeds image bounds",
            ));
        }
        region_origins.push(PixelPoint { x, y });
    }

    let mut solid_by_local = vec![false; tilecount as usize];
    if let Some(tiles) = &body.tiles {
        for tile in tiles {
            if tile.id >= tilecount {
                return Err(TiledImportError::Unsupported(
                    "tileset tile id exceeds tilecount",
                ));
            }
            if tile_has_solid(tile) {
                solid_by_local[tile.id as usize] = true;
            }
        }
    }

    Ok(ResolvedTileset {
        firstgid,
        image_uri,
        image_w,
        image_h,
        tilecount,
        region_origins,
        region_size,
        solid_by_local,
        external_source_uri,
    })
}

fn tile_has_solid(tile: &RawTile) -> bool {
    tile.properties
        .as_ref()
        .into_iter()
        .flatten()
        .any(|property| {
            property.name == "solid"
                && match &property.value {
                    serde_json::Value::Bool(value) => *value,
                    serde_json::Value::String(value) => {
                        value.eq_ignore_ascii_case("true") || value == "1"
                    }
                    serde_json::Value::Number(value) => value.as_u64() == Some(1),
                    _ => false,
                }
        })
}