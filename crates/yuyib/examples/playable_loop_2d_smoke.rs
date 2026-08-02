//! Deep 2D smoke: [`PlayableLoop2d`] steps kinematic motion and camera follow.
//!
//! No window. Proves profile + loop construct, step without input, and sync the
//! camera to the actor sprite.
//!
//! ```text
//! cargo run -p yuyib --example playable_loop_2d_smoke --features profile-2d
//! ```

use std::{error::Error, time::Duration};

use yuyib::{
    assets::Assets,
    game_2d::{
        Game2dSceneConfig, KinematicSpriteController2d, Sprite2d, TileCollision2d, TileMap2d,
    },
    image::{DecodePolicy, decode_bytes},
    profile_2d::{Game2dProfile, PlayableLoop2d, PlayableLoopDesc2d},
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
    let region = sheet
        .region(0)
        .ok_or("atlas missing region 0")?;

    let mut profile = Game2dProfile::new(Game2dSceneConfig::default());
    let grid = [8_u32, 8];
    let mut solid = vec![false; 64];
    for i in 0..8 {
        solid[i] = true;
        solid[56 + i] = true;
        solid[i * 8] = true;
        solid[i * 8 + 7] = true;
    }
    profile.world_mut().spawn((
        TileMap2d::new(grid, [16.0, 16.0], vec![region], vec![Some(0); 64])?.with_layer(0),
        TileCollision2d::new(grid, solid)?,
    ));
    let start = [64.0_f32, 64.0];
    let player = profile
        .world_mut()
        .spawn((
            Sprite2d::new(region)
                .with_position(start)
                .with_size([14.0, 14.0])
                .with_layer(10),
            KinematicSpriteController2d::new([7.0, 7.0], 120.0)?,
        ))
        .id();

    let mut playable = PlayableLoop2d::new(PlayableLoopDesc2d::new(player, 64)?);
    playable.step(&mut profile, Duration::from_millis(16))?;
    playable.step(&mut profile, Duration::from_millis(16))?;

    let camera = profile.scene_mut().camera_mut().position;
    if (camera[0] - start[0]).abs() > 0.01 || (camera[1] - start[1]).abs() > 0.01 {
        return Err(format!("camera follow mismatch: {camera:?} vs start {start:?}").into());
    }

    println!(
        "playable_loop_2d_smoke OK: camera=({:.1},{:.1}) actor={player:?}",
        camera[0], camera[1]
    );
    Ok(())
}
