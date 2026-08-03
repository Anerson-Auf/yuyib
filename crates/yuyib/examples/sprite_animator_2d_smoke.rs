//! Deep 2D smoke: `AnimationSet` + state machine + `play("walk")` + facing.
//!
//! No window. Proves clip rebind, once→idle `on_finished`, and velocity/facing
//! selection without rewriting the tile playground.
//!
//! ```text
//! cargo run -p yuyib --example sprite_animator_2d_smoke --features two-d
//! ```

use std::{error::Error, time::Duration};

use yuyib::{
    assets::Assets,
    game_2d::{
        AnimatedSprite2d, Sprite2d, SpriteAnimator2d, VelocityFacingPolicy2d,
        apply_velocity_facing_2d, step_sprite_animators_2d,
    },
    animation::{AnimationSet, AnimationStateDef, AnimationStateMachine},
    ecs::prelude::World,
    two_d::{PlaybackMode, SpriteAnimation, SpriteSheet, Texture, TextureSize},
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
    use yuyib::image::{DecodePolicy, decode_bytes};

    let image = decode_bytes(TINY_PNG, DecodePolicy::default())?;
    let mut textures = Assets::<Texture>::new();
    let texture = textures.insert(image.texture().clone());
    let cell = TextureSize::new(1, 1)?;
    let sheet = SpriteSheet::from_grid(texture, image.texture().size(), cell)?;
    let region = sheet.region(0).ok_or("missing region")?;

    let idle = SpriteAnimation::from_regions(&[region], Duration::from_millis(100), PlaybackMode::Loop)?;
    let walk = SpriteAnimation::from_regions(&[region, region], Duration::from_millis(50), PlaybackMode::Loop)?;
    let attack =
        SpriteAnimation::from_regions(&[region], Duration::from_millis(40), PlaybackMode::Once)?;

    let set = AnimationSet::new()
        .with("idle", idle.clone())
        .with("walk", walk)
        .with("attack", attack);
    let machine = AnimationStateMachine::new("idle")?
        .with_clip("walk")?
        .with_state(
            "attack",
            AnimationStateDef::clip("attack").on_finished("idle"),
        )?;

    let mut world = World::new();
    let actor = world
        .spawn((
            Sprite2d::new(region).with_size([16.0, 16.0]),
            AnimatedSprite2d::new(idle),
            SpriteAnimator2d::new(set, machine)?,
        ))
        .id();

    world
        .get_mut::<SpriteAnimator2d>(actor)
        .ok_or("animator")?
        .play("walk")?;
    step_sprite_animators_2d(&mut world, Duration::ZERO);
    if world
        .get::<SpriteAnimator2d>(actor)
        .ok_or("animator")?
        .current_state()
        != "walk"
    {
        return Err("play(\"walk\") did not stick".into());
    }

    let policy = VelocityFacingPolicy2d::new("idle", "walk");
    apply_velocity_facing_2d(&mut world, actor, [-1.0, 0.0], &policy)?;
    if world.get::<Sprite2d>(actor).ok_or("sprite")?.size[0] >= 0.0 {
        return Err("facing left should flip sprite width".into());
    }

    world
        .get_mut::<SpriteAnimator2d>(actor)
        .ok_or("animator")?
        .play("attack")?;
    step_sprite_animators_2d(&mut world, Duration::from_millis(40));
    if world
        .get::<SpriteAnimator2d>(actor)
        .ok_or("animator")?
        .current_state()
        != "idle"
    {
        return Err("attack on_finished should return to idle".into());
    }

    println!(
        "sprite_animator_2d_smoke OK: actor={actor:?} state=idle facing_flipped=true"
    );
    Ok(())
}
