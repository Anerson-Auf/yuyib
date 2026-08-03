//! Deep 2D B smoke: [`PlatformerPlayable2d`] lands and syncs sprite/camera.
//!
//! Headless. Proves Rapier platformer HL constructs, settles on ground, and
//! mirrors physics→sprite Y-flip scale into the profile camera.
//!
//! ```text
//! cargo run -p yuyib --example platformer_playable_2d_smoke --features "two-d,character-2d"
//! ```

use std::{error::Error, time::Duration};

use yuyib::{
    assets::Assets,
    character_2d::PlatformerControllerConfig2d,
    game_2d::{Game2dSceneConfig, Sprite2d},
    image::{DecodePolicy, decode_bytes},
    profile_2d::{
        Game2dProfile, PlatformerPlayable2d, PlatformerPlayableDesc2d, physics_to_sprite,
    },
    two_d::{SpriteSheet, Texture, TextureSize},
};

const TINY_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0xDA, 0x63, 0xFC, 0xCF, 0xC0, 0x50,
    0x0F, 0x00, 0x04, 0x85, 0x01, 0x80, 0x84, 0xA9, 0x8C, 0x21, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
];

const PPU: f32 = 32.0;
const SPAWN: [f32; 2] = [0.0, 3.0];

fn main() -> Result<(), Box<dyn Error>> {
    let image = decode_bytes(TINY_PNG, DecodePolicy::default())?;
    let mut textures = Assets::<Texture>::new();
    let texture = textures.insert(image.texture().clone());
    let cell = TextureSize::new(1, 1)?;
    let sheet = SpriteSheet::from_grid(texture, image.texture().size(), cell)?;
    let region = sheet.region(0).ok_or("missing region")?;

    let mut profile = Game2dProfile::new(Game2dSceneConfig::default());
    let player = profile
        .world_mut()
        .spawn(
            Sprite2d::new(region)
                .with_position(physics_to_sprite(SPAWN, PPU))
                .with_size([16.0, 32.0])
                .with_layer(10),
        )
        .id();

    let mut playable = PlatformerPlayable2d::spawn(
        PlatformerPlayableDesc2d::new(player, SPAWN)?
            .with_pixels_per_unit(PPU)
            .with_config(PlatformerControllerConfig2d {
                jump_speed: 16.0,
                ..PlatformerControllerConfig2d::default()
            }),
    )?;
    playable
        .dynamics_mut()
        .insert_fixed_cuboid([0.0, -0.5], [8.0, 0.5])?;

    let mut landed = false;
    for _ in 0..180 {
        let step = playable.step(&mut profile, Duration::from_millis(16))?;
        if playable.grounded() {
            landed = true;
            let _ = step;
            break;
        }
    }
    if !landed {
        return Err("platformer_playable_2d_smoke: failed to land".into());
    }

    let sprite_y = profile
        .world()
        .get::<Sprite2d>(player)
        .ok_or("missing sprite")?
        .position[1];
    let camera_y = profile.scene_mut().camera_mut().position[1];
    if (sprite_y - camera_y).abs() > 0.01 {
        return Err(format!("camera not following sprite: sprite_y={sprite_y} camera_y={camera_y}")
            .into());
    }
    // Ground top ≈ 0 in physics; standing capsule centre is above 0 → sprite Y negative.
    if sprite_y >= 0.0 {
        return Err(format!(
            "expected Y-flip into negative sprite Y after landing, got {sprite_y}"
        )
        .into());
    }

    println!(
        "platformer_playable_2d_smoke OK: grounded sprite_y={sprite_y:.1} camera_y={camera_y:.1}"
    );
    Ok(())
}
