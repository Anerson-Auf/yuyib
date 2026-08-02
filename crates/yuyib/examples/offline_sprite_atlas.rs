//! Headless vertical slice for an offline-cooked sprite atlas manifest.
//!
//! Run with:
//! `cargo run -p yuyib --example offline_sprite_atlas --no-default-features --features two-d`

use std::{error::Error, time::Duration};

use yuyib::prelude::{
    Assets, ImportSource, ImportedSpriteAtlas, ImporterRegistry, SpriteAnimationState, Texture,
    register_sprite_atlas_importer,
};

const HERO_ATLAS: &[u8] = br#"{
  "format": "yuyib.sprite_atlas",
  "version": 1,
  "texture": {
    "uri": "textures/hero_atlas.png",
    "width": 96,
    "height": 32,
    "alpha": "straight",
    "color_space": "srgb"
  },
  "regions": [
    { "name": "walk_0", "x": 0,  "y": 0, "width": 32, "height": 32 },
    { "name": "walk_1", "x": 32, "y": 0, "width": 32, "height": 32 },
    { "name": "walk_2", "x": 64, "y": 0, "width": 32, "height": 32 }
  ],
  "animations": [{
    "name": "walk",
    "playback": "loop",
    "frames": [
      { "region": "walk_0", "duration_ms": 90 },
      { "region": "walk_1", "duration_ms": 120 },
      { "region": "walk_2", "duration_ms": 90 }
    ]
  }]
}"#;

fn main() -> Result<(), Box<dyn Error>> {
    let mut registry = ImporterRegistry::<ImportedSpriteAtlas>::default();
    register_sprite_atlas_importer(&mut registry)?;

    let imported = registry.import(ImportSource::new("hero.ysprite", HERO_ATLAS))?;
    println!(
        "importer={}@{} dependency={} cpu_bytes={}",
        imported.importer.id,
        imported.importer.version,
        imported.dependencies[0].uri,
        imported.cpu_bytes.unwrap_or_default()
    );

    // A real resolver now loads/decodes the dependency asynchronously. Binding
    // only connects its stable typed handle; it performs no file or GPU work.
    let mut textures = Assets::<Texture>::new();
    let texture = textures.insert(imported.asset.texture().clone());
    let atlas = imported.asset.bind_texture(texture)?;
    let walk = atlas.animation("walk").ok_or("missing walk animation")?;
    let mut playback = SpriteAnimationState::new();
    for delta_ms in [0, 100, 120, 100] {
        playback.advance(walk, Duration::from_millis(delta_ms));
        let frame = playback.frame(walk);
        println!(
            "delta_ms={delta_ms} frame_x={} duration_ms={}",
            frame.region().origin().x,
            frame.duration().as_millis()
        );
    }
    Ok(())
}
