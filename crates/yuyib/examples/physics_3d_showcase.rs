//! M4 3D physics **high-level** showcase (headless).
//!
//! One short tour of the APIs games should reach for first:
//! mesh character (slope + moving platform) and Rapier facade (shapes, CCD,
//! trigger, joint, fixed stepper). No window — asserts + log line.
//!
//! For custom collision / filters / contact surgery see
//! `physics_3d_lowlevel`.
//!
//! ```text
//! cargo run -p yuyib --example physics_3d_showcase --features "three-d,physics-rapier"
//! ```

use std::error::Error;

use yuyib::{
    character_3d::{
        CharacterController3d, CharacterControllerConfig3d, CharacterInput3d,
        CharacterMovingPlatform3d,
    },
    physics::{
        DynamicsBackend3d, DynamicsFixedStepper3d, DynamicsWorldConfig3d, RapierDynamicsWorld3d,
        TriangleMesh3d, Vec3,
    },
};

fn flat_floor(y: f32) -> Result<TriangleMesh3d, Box<dyn Error>> {
    Ok(TriangleMesh3d::from_indexed(
        &[
            Vec3::new(-4.0, y, -4.0),
            Vec3::new(4.0, y, -4.0),
            Vec3::new(4.0, y, 4.0),
            Vec3::new(-4.0, y, 4.0),
        ],
        &[0, 2, 1, 0, 3, 2],
    )?)
}

fn ramp_30_deg() -> Result<TriangleMesh3d, Box<dyn Error>> {
    let angle = 30.0_f32.to_radians();
    let run = 4.0;
    let rise = run * angle.tan();
    Ok(TriangleMesh3d::from_indexed(
        &[
            Vec3::new(-2.0, 0.0, 0.0),
            Vec3::new(2.0, 0.0, 0.0),
            Vec3::new(2.0, rise, run),
            Vec3::new(-2.0, rise, run),
        ],
        &[0, 2, 1, 0, 3, 2],
    )?)
}

fn demo_mesh_character() -> Result<(), Box<dyn Error>> {
    let world = flat_floor(-40.0)?;
    let platform_a = flat_floor(0.0)?;
    let platform_b = flat_floor(0.4)?;
    let mut controller = CharacterController3d::new(
        CharacterControllerConfig3d {
            radius: 0.35,
            gravity_y: 0.0,
            move_speed: 0.0,
            max_walkable_slope_radians: 35.0_f32.to_radians(),
            ..CharacterControllerConfig3d::default()
        },
        Vec3::new(0.0, 0.1, 0.0),
    )?;
    controller.place_on_triangle_mesh(&platform_a)?;
    if !controller.is_grounded() {
        return Err("showcase: character failed to ground on platform".into());
    }
    let before = controller.position();
    controller.step_on_triangle_mesh_with_platform(
        CharacterInput3d::idle(),
        &world,
        Some(CharacterMovingPlatform3d {
            mesh: &platform_b,
            translation_delta: Vec3::new(0.0, 0.4, 0.0),
        }),
    )?;
    if !(controller.is_grounded() && controller.position().y > before.y + 0.25) {
        return Err(format!(
            "showcase: platform carry failed, before_y={} after_y={} grounded={}",
            before.y,
            controller.position().y,
            controller.is_grounded()
        )
        .into());
    }

    let ramp = ramp_30_deg()?;
    let surface_y = 1.0 * 30.0_f32.to_radians().tan();
    let mut on_ramp = CharacterController3d::new(
        CharacterControllerConfig3d {
            radius: 0.35,
            gravity_y: 0.0,
            move_speed: 0.0,
            max_walkable_slope_radians: 35.0_f32.to_radians(),
            ..CharacterControllerConfig3d::default()
        },
        Vec3::new(0.0, surface_y + 0.1, 1.0),
    )?;
    on_ramp.place_on_triangle_mesh(&ramp)?;
    if !on_ramp.is_grounded() {
        return Err("showcase: 30° ramp should be walkable at 35° max slope".into());
    }
    Ok(())
}

fn demo_rapier_facade() -> Result<(), Box<dyn Error>> {
    let mut world = RapierDynamicsWorld3d::new(DynamicsWorldConfig3d::earth_60hz())?;
    let _ground = world.insert_fixed_cuboid([0.0, -0.25, 0.0], [4.0, 0.25, 4.0])?;
    let ball = world.insert_dynamic_sphere([0.0, 2.0, 0.0], 0.3)?;
    let crate_id = world.insert_dynamic_cuboid([1.2, 1.0, 0.0], [0.25, 0.25, 0.25])?;
    let _capsule = world.insert_dynamic_capsule([-1.0, 2.0, 0.0], 0.35, 0.2)?;
    world.set_ccd_enabled(ball, true)?;

    let trigger = world.insert_trigger_sphere([0.0, 0.8, 0.0], 0.6)?;
    let platform = world.insert_kinematic_cuboid([0.0, 0.4, 2.0], [1.0, 0.1, 1.0])?;

    let anchor = world.insert_fixed_cuboid([2.5, 2.5, 0.0], [0.1, 0.1, 0.1])?;
    let bob = world.insert_dynamic_sphere([2.5, 1.0, 0.0], 0.2)?;
    let _rope = world.insert_rope_joint(anchor, bob, 1.0, [0.0, 0.0, 0.0], [0.0, 0.0, 0.0])?;

    let mut stepper = DynamicsFixedStepper3d::hz60();
    let mut saw_trigger = false;
    for i in 0..180 {
        if i == 60 {
            world.set_linear_velocity(platform, [0.8, 0.0, 0.0])?;
        }
        let _ = stepper.step_backend(&mut world, stepper.fixed_dt())?;
        if world
            .collect_trigger_overlaps()
            .iter()
            .any(|(t, _)| *t == trigger)
        {
            saw_trigger = true;
        }
    }

    let ball_y = world
        .translation(ball)
        .ok_or("showcase: missing ball")?[1];
    if ball_y > 1.0 {
        return Err(format!("showcase: ball did not settle, y={ball_y}").into());
    }
    let crate_y = world
        .translation(crate_id)
        .ok_or("showcase: missing crate")?[1];
    if crate_y > 1.2 {
        return Err(format!("showcase: crate did not settle, y={crate_y}").into());
    }
    if !saw_trigger {
        return Err("showcase: expected trigger overlap while ball fell".into());
    }
    let contacts = world.collect_contact_pairs();
    if contacts.is_empty() {
        return Err("showcase: expected at least one contact pair".into());
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    demo_mesh_character()?;
    demo_rapier_facade()?;
    println!(
        "physics_3d_showcase OK: mesh character (slope+platform) + Rapier facade \
         (shapes/CCD/trigger/joint/stepper/contacts)"
    );
    Ok(())
}
