//! M4 platformer smoke: Rapier-backed [`yuyib::character_2d::PlatformerController2d`].
//!
//! Headless. Proves gravity land, jump, wall block, and one-way platform pass-through
//! without touching the top-down [`yuyib::game_2d::KinematicSpriteController2d`] path.
//!
//! ```text
//! cargo run -p yuyib --example physics_platformer2d_smoke --features character-2d
//! ```

use std::error::Error;

use yuyib::character_2d::{
    PlatformerController2d, PlatformerControllerConfig2d, PlatformerControllerEvent2d,
    PlatformerInput2d,
};
use yuyib::physics::{DynamicsWorldConfig2d, RapierDynamicsWorld2d};

fn main() -> Result<(), Box<dyn Error>> {
    let mut world = RapierDynamicsWorld2d::new(DynamicsWorldConfig2d::earth_60hz())?;
    let _ground = world.insert_fixed_cuboid([0.0, -0.5], [8.0, 0.5])?;
    let _wall = world.insert_fixed_cuboid([3.0, 1.0], [0.25, 1.5])?;
    let _one_way = world.insert_one_way_platform_cuboid([0.0, 2.5], [1.5, 0.1])?;

    let mut controller = PlatformerController2d::spawn(
        &mut world,
        PlatformerControllerConfig2d {
            jump_speed: 16.0,
            ..PlatformerControllerConfig2d::default()
        },
        [0.0, 3.0],
    )?;

    let mut landed = false;
    for _ in 0..180 {
        let step = controller.step(&mut world, PlatformerInput2d::neutral())?;
        if step
            .events
            .contains(&PlatformerControllerEvent2d::Landed)
        {
            landed = true;
            break;
        }
    }
    if !landed || !controller.grounded() {
        return Err("physics_platformer2d_smoke: failed to land on ground".into());
    }

    let jumped = controller.step(&mut world, PlatformerInput2d::new(0.0, true)?)?;
    if !jumped
        .events
        .contains(&PlatformerControllerEvent2d::Jumped)
    {
        return Err("physics_platformer2d_smoke: jump did not fire".into());
    }

    let mut on_one_way = false;
    for _ in 0..120 {
        let step = controller.step(&mut world, PlatformerInput2d::neutral())?;
        if step.grounded && step.translation[1] > 2.4 {
            on_one_way = true;
            break;
        }
    }
    if !on_one_way {
        return Err("physics_platformer2d_smoke: did not land on one-way platform".into());
    }

    // Drop to ground, then run into the wall.
    for _ in 0..30 {
        let _ = controller.step(&mut world, PlatformerInput2d::new(0.0, true)?)?;
    }
    for _ in 0..120 {
        let _ = controller.step(&mut world, PlatformerInput2d::neutral())?;
    }
    for _ in 0..180 {
        let _ = controller.step(&mut world, PlatformerInput2d::new(1.0, false)?)?;
    }
    let end = controller.step(&mut world, PlatformerInput2d::neutral())?;
    if end.translation[0] >= 2.8 {
        return Err(format!(
            "physics_platformer2d_smoke: walked through wall, x={}",
            end.translation[0]
        )
        .into());
    }

    println!(
        "physics_platformer2d_smoke OK: grounded={} pos=({:.2},{:.2})",
        end.grounded, end.translation[0], end.translation[1]
    );
    Ok(())
}
