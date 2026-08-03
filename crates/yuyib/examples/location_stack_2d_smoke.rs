//! Headless M7 smoke: [`LocationStack2d`] push/pop swaps map grids.
//!
//! ```text
//! cargo run -p yuyib --example location_stack_2d_smoke --features two-d
//! ```

use std::error::Error;

use yuyib::{
    assets::Assets,
    ecs::prelude::World,
    game_2d::{TileCollision2d, TileMap2d, TileMapComposer2d, TileStamp2d},
    image::{DecodePolicy, decode_bytes},
    profile_2d::{
        LocationFrame2d, LocationPortal2d, LocationPortalAction2d, LocationStack2d,
    },
    two_d::{SpriteSheet, Texture, TextureSize},
};

/// Minimal valid 1×1 PNG (RGBA).
const TINY_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0xDA, 0x63, 0xFC, 0xCF, 0xC0, 0x50,
    0x0F, 0x00, 0x04, 0x85, 0x01, 0x80, 0x84, 0xA9, 0x8C, 0x21, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
];

fn main() -> Result<(), Box<dyn Error>> {
    let image = decode_bytes(TINY_PNG, DecodePolicy::default())?;
    let mut textures = Assets::<Texture>::new();
    let texture = textures.insert(image.texture().clone());
    let cell = TextureSize::new(1, 1)?;
    let sheet = SpriteSheet::from_grid(texture, image.texture().size(), cell)?;
    let region = sheet.region(0).ok_or("atlas missing region 0")?;
    let regions = vec![region];

    let outdoor_grid = [4_u32, 3];
    let outdoor_tiles = vec![Some(0_u32); 12];
    let outdoor_map = TileMap2d::new(outdoor_grid, [16.0, 16.0], regions.clone(), outdoor_tiles)?
        .with_layer(0);
    let outdoor_collision = TileCollision2d::new(outdoor_grid, vec![false; 12])?;

    let mut world = World::new();
    let outdoor_entity = world
        .spawn((outdoor_map.clone(), outdoor_collision.clone()))
        .id();
    let mut stack = LocationStack2d::new(LocationFrame2d {
        id: "outdoor".into(),
        entities: vec![outdoor_entity],
        portals: vec![LocationPortal2d::from_center_size(
            [32.0, 32.0],
            [16.0, 16.0],
            LocationPortalAction2d::Enter("house_interior".into()),
        )],
        spawn: [24.0, 24.0],
    });

    let (interior_map, interior_collision) = TileMapComposer2d::new([6, 5], [16.0, 16.0], regions)?
        .fill(Some(0))
        .fill_solid(false)
        .border(0, true)?
        .stamp([2, 2, 2, 1], |_column, _row| {
            Some(TileStamp2d::filled(0, false))
        })?
        .build()?;
    if interior_map.grid() != [6, 5] {
        return Err(format!("composer grid {:?}", interior_map.grid()).into());
    }
    let interior_entity = world
        .spawn((interior_map.with_layer(0), interior_collision))
        .id();
    stack.push(
        &mut world,
        LocationFrame2d {
            id: "house_interior".into(),
            entities: vec![interior_entity],
            portals: vec![LocationPortal2d::from_center_size(
                [48.0, 64.0],
                [16.0, 16.0],
                LocationPortalAction2d::Exit,
            )],
            spawn: [48.0, 40.0],
        },
    );

    let active_grid = world
        .get::<TileMap2d>(interior_entity)
        .ok_or("interior map missing after push")?
        .grid();
    if active_grid != [6, 5] {
        return Err(format!("expected interior grid [6,5], got {active_grid:?}").into());
    }
    if world.get_entity(outdoor_entity).is_ok() {
        return Err("outdoor map should be despawned on push".into());
    }

    let restored = stack.pop(&mut world)?;
    if restored != "outdoor" {
        return Err(format!("expected outdoor id, got {restored}").into());
    }
    let rebuilt = world
        .spawn((outdoor_map, outdoor_collision))
        .id();
    stack.replace_current(LocationFrame2d {
        id: "outdoor".into(),
        entities: vec![rebuilt],
        portals: Vec::new(),
        spawn: [24.0, 24.0],
    });
    let outdoor_grid_after = world
        .get::<TileMap2d>(rebuilt)
        .ok_or("outdoor map missing after rebuild")?
        .grid();
    if outdoor_grid_after != [4, 3] {
        return Err(format!("expected outdoor [4,3], got {outdoor_grid_after:?}").into());
    }

    println!("location_stack_2d_smoke OK: push interior [6,5], pop outdoor [4,3]");
    Ok(())
}
