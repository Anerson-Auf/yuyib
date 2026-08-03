//! Headless M7 smoke: Tiled JSON/TMX → `TileMap2d` (embedded + external `.tsj`/`.tsx`).
//!
//! ```text
//! cargo run -p yuyib --example tiled_map_2d_smoke --features two-d
//! ```

use std::error::Error;

use yuyib::{
    assets::{Assets, ImportSource, ImporterRegistry},
    image::{DecodePolicy, decode_bytes},
    tiled::{
        ExternalTilesetBytes, ImportedTiledMap, TiledMapImporter, register_tiled_map_importer,
    },
    two_d::Texture,
};

/// Same 32×16 four-cell atlas used by early 2D playgrounds.
const DEMO_ATLAS_PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 32, 0, 0, 0, 16, 8, 6,
    0, 0, 0, 119, 0, 125, 89, 0, 0, 0, 50, 73, 68, 65, 84, 120, 218, 99, 120, 233, 154, 240, 31,
    31, 230, 184, 121, 13, 47, 254, 127, 49, 13, 47, 78, 171, 123, 133, 23, 51, 140, 58, 96, 212,
    1, 163, 14, 24, 117, 192, 168, 3, 70, 29, 48, 234, 128, 129, 118, 0, 0, 53, 115, 162, 204, 4,
    161, 251, 150, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

const MAP_JSON: &str = include_str!("fixtures/tiled_unit_room.json");
const MAP_TMX: &str = include_str!("fixtures/tiled_unit_room.tmx");
const EXTERNAL_MAP_JSON: &str = include_str!("fixtures/tiled_external_tileset_room.json");
const EXTERNAL_TILESET_JSON: &str = include_str!("fixtures/demo_atlas.tsj");
const EXTERNAL_MAP_TMX: &str = include_str!("fixtures/tiled_external_tileset_room.tmx");
const EXTERNAL_TILESET_TSX: &str = include_str!("fixtures/demo_atlas.tsx");

fn main() -> Result<(), Box<dyn Error>> {
    let mut registry = ImporterRegistry::<ImportedTiledMap>::default();
    register_tiled_map_importer(&mut registry)?;
    let imported = registry.import(ImportSource::new(
        "fixtures/tiled_unit_room.json",
        MAP_JSON.as_bytes(),
    ))?;

    let map = &imported.asset;
    if map.grid() != [4, 3] {
        return Err(format!("unexpected grid {:?}", map.grid()).into());
    }
    if map.visual_layer() != "ground" {
        return Err(format!("unexpected layer {}", map.visual_layer()).into());
    }
    if imported.dependencies.len() != 1 || imported.dependencies[0].uri != "demo_atlas.png" {
        return Err("expected demo_atlas.png dependency".into());
    }

    let solid_count = map.solid().iter().filter(|&&s| s).count();
    if solid_count != 10 || map.solid()[5] {
        return Err(format!("collision looks wrong: solid_count={solid_count}").into());
    }
    if map.object_layers().len() != 1 || map.object_layers()[0].objects().len() != 2 {
        return Err(format!(
            "expected 1 object layer with 2 objects, got {} layers",
            map.object_layers().len()
        )
        .into());
    }
    let spawn = &map.object_layers()[0].objects()[0];
    if spawn.class() != "player_spawn" {
        return Err(format!("unexpected spawn class {}", spawn.class()).into());
    }
    let portal = &map.object_layers()[0].objects()[1];
    if portal.class() != "portal" {
        return Err(format!("unexpected portal class {}", portal.class()).into());
    }

    let image = decode_bytes(DEMO_ATLAS_PNG, DecodePolicy::default())?;
    let mut textures = Assets::<Texture>::new();
    let texture = textures.insert(image.texture().clone());
    let bound = imported.asset.bind_texture_with_world_tile_size(texture, [32.0, 32.0])?;
    let (tile_maps, _collision, objects) = bound.into_parts();
    if tile_maps.len() != 1 || tile_maps[0].grid() != [4, 3] {
        return Err("bound tile map grid mismatch".into());
    }
    if objects.len() != 1 {
        return Err("bound map dropped object layers".into());
    }

    let external = TiledMapImporter::default().import_map_with_external_tilesets(
        ImportSource::new(
            "fixtures/tiled_external_tileset_room.json",
            EXTERNAL_MAP_JSON.as_bytes(),
        ),
        &[ExternalTilesetBytes::new(
            "demo_atlas.tsj",
            EXTERNAL_TILESET_JSON.as_bytes(),
        )],
    )?;
    if external.asset.grid() != [4, 3] || external.asset.image_uri() != "demo_atlas.png" {
        return Err("external tileset import mismatch".into());
    }
    if external.dependencies.len() != 2
        || external.dependencies[0].uri != "demo_atlas.tsj"
        || external.dependencies[1].uri != "demo_atlas.png"
    {
        return Err(format!(
            "expected tsj+image deps, got {:?}",
            external
                .dependencies
                .iter()
                .map(|dep| dep.uri.as_str())
                .collect::<Vec<_>>()
        )
        .into());
    }

    let tmx = TiledMapImporter::default().import_map(ImportSource::new(
        "fixtures/tiled_unit_room.tmx",
        MAP_TMX.as_bytes(),
    ))?;
    if tmx.asset.grid() != [4, 3] || tmx.asset.cells() != map.cells() {
        return Err("tmx import mismatch vs json room".into());
    }

    let external_tsx = TiledMapImporter::default().import_map_with_external_tilesets(
        ImportSource::new(
            "fixtures/tiled_external_tileset_room.tmx",
            EXTERNAL_MAP_TMX.as_bytes(),
        ),
        &[ExternalTilesetBytes::new(
            "demo_atlas.tsx",
            EXTERNAL_TILESET_TSX.as_bytes(),
        )],
    )?;
    if external_tsx.asset.grid() != [4, 3]
        || external_tsx.dependencies[0].uri != "demo_atlas.tsx"
    {
        return Err("external tsx import mismatch".into());
    }

    println!(
        "tiled_map_2d_smoke OK: grid=4x3 layer=ground solid_tiles={solid_count} objects={} external_tsj=ok tmx=ok tsx=ok",
        objects[0].objects().len()
    );
    Ok(())
}
