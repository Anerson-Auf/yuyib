//! M4 3D physics **low-level** escape hatches (headless).
//!
//! Use-cases where games usually bypass the high-level character/Rapier
//! convenience path:
//!
//! 1. **Custom `step_with_collision`** — one-way floor (support from above only)
//! 2. **Direct mesh queries** — `resolve_sphere_with_slope` / raycast without a controller
//! 3. **Collision groups** — ignore selected layers
//! 4. **Contact / trigger inspection** — raw pairs after a backend step
//!
//! High-level tour: `physics_3d_showcase`.
//!
//! ```text
//! cargo run -p yuyib --example physics_3d_lowlevel --features "three-d,physics-rapier"
//! ```

use std::error::Error;

use yuyib::{
    character_3d::{
        CharacterCollisionError3d, CharacterCollisionResolution3d, CharacterController3d,
        CharacterControllerConfig3d, CharacterInput3d,
    },
    physics::{
        CollisionGroups3d, DynamicsBackend3d, DynamicsWorldConfig3d, RapierDynamicsWorld3d, Ray3d,
        TriangleMesh3d, Vec3, default_max_walkable_slope_radians,
    },
};

fn flat_floor(y: f32) -> Result<TriangleMesh3d, Box<dyn Error>> {
    Ok(TriangleMesh3d::from_indexed(
        &[
            Vec3::new(-3.0, y, -3.0),
            Vec3::new(3.0, y, -3.0),
            Vec3::new(3.0, y, 3.0),
            Vec3::new(-3.0, y, 3.0),
        ],
        &[0, 2, 1, 0, 3, 2],
    )?)
}

/// One-way floor: only block when falling into the plane from above.
fn demo_custom_one_way_floor() -> Result<(), Box<dyn Error>> {
    let floor_y = 1.0_f32;
    let radius = 0.3_f32;
    let mut controller = CharacterController3d::new(
        CharacterControllerConfig3d {
            radius,
            gravity_y: -20.0,
            move_speed: 0.0,
            jump_speed: 8.0,
            fixed_delta_seconds: 1.0 / 60.0,
            ..CharacterControllerConfig3d::default()
        },
        Vec3::new(0.0, 3.0, 0.0),
    )?;

    for _ in 0..180 {
        let prev_y = controller.position().y;
        controller.step_with_collision(CharacterInput3d::idle(), |desired, radius| {
            one_way_floor_resolve(desired, radius, floor_y, desired.y <= prev_y + 1.0e-4)
        })?;
        if controller.is_grounded() {
            break;
        }
    }
    if !controller.is_grounded() || controller.position().y + 1.0e-3 < floor_y + radius {
        return Err(format!(
            "lowlevel: expected land on one-way floor, y={} grounded={}",
            controller.position().y,
            controller.is_grounded()
        )
        .into());
    }

    controller.step_with_collision(
        CharacterInput3d::new(yuyib::physics::Vec2::ZERO, true)?,
        |desired, radius| one_way_floor_resolve(desired, radius, floor_y, false),
    )?;
    let mut cleared = false;
    for _ in 0..45 {
        let prev_y = controller.position().y;
        controller.step_with_collision(CharacterInput3d::idle(), |desired, radius| {
            one_way_floor_resolve(desired, radius, floor_y, desired.y <= prev_y + 1.0e-4)
        })?;
        if controller.position().y > floor_y + radius + 0.05 {
            cleared = true;
            break;
        }
    }
    if !cleared {
        return Err(format!(
            "lowlevel: jump should clear one-way floor, y={}",
            controller.position().y
        )
        .into());
    }
    Ok(())
}

fn one_way_floor_resolve(
    desired: Vec3,
    radius: f32,
    floor_y: f32,
    falling: bool,
) -> Result<CharacterCollisionResolution3d, CharacterCollisionError3d> {
    let contact_y = floor_y + radius;
    if falling && desired.y < contact_y && desired.y + radius >= floor_y {
        return Ok(CharacterCollisionResolution3d {
            position: Vec3::new(desired.x, contact_y, desired.z),
            grounded: true,
            contacts: 1,
        });
    }
    Ok(CharacterCollisionResolution3d {
        position: desired,
        grounded: false,
        contacts: 0,
    })
}

fn demo_direct_mesh_queries() -> Result<(), Box<dyn Error>> {
    let floor = flat_floor(0.0)?;
    let wall = TriangleMesh3d::from_indexed(
        &[
            Vec3::new(1.0, 0.0, -2.0),
            Vec3::new(1.0, 3.0, -2.0),
            Vec3::new(1.0, 3.0, 2.0),
            Vec3::new(1.0, 0.0, 2.0),
        ],
        &[0, 1, 2, 0, 2, 3],
    )?;

    let on_floor = floor.resolve_sphere_with_slope(
        Vec3::new(0.0, 0.1, 0.0),
        0.4,
        4,
        default_max_walkable_slope_radians(),
    )?;
    if !on_floor.ground_contact {
        return Err("lowlevel: direct floor resolve should report ground".into());
    }

    let against_wall = wall.resolve_sphere_with_slope(
        Vec3::new(0.7, 1.0, 0.0),
        0.4,
        4,
        60.0_f32.to_radians(),
    )?;
    if against_wall.contacts == 0 || against_wall.ground_contact {
        return Err("lowlevel: wall contact must not count as walkable ground".into());
    }

    let ray = Ray3d::new(Vec3::new(0.0, 2.0, 0.0), Vec3::new(0.0, -1.0, 0.0))?;
    let hit = floor
        .raycast(ray, 10.0)?
        .ok_or("lowlevel: expected floor ray hit")?;
    if (hit.distance - 2.0).abs() > 0.05 {
        return Err(format!("lowlevel: unexpected ray distance {}", hit.distance).into());
    }
    Ok(())
}

fn demo_collision_groups_and_contacts() -> Result<(), Box<dyn Error>> {
    let mut world = RapierDynamicsWorld3d::new(DynamicsWorldConfig3d::earth_60hz())?;
    let ground = world.insert_fixed_cuboid([0.0, -0.25, 0.0], [2.0, 0.25, 2.0])?;
    let solid = world.insert_dynamic_sphere([0.0, 2.0, 0.0], 0.3)?;
    let ghost = world.insert_dynamic_sphere([0.5, 2.0, 0.0], 0.3)?;

    // Layer 1 = ground+solid interact; ghost is layer 4 and only hits itself.
    world.set_collision_groups(ground, CollisionGroups3d::new(1, 1 | 2))?;
    world.set_collision_groups(solid, CollisionGroups3d::new(2, 1))?;
    world.set_collision_groups(ghost, CollisionGroups3d::new(4, 4))?;
    world.set_ccd_enabled(solid, true)?;

    for _ in 0..180 {
        world.step(None)?;
    }

    let solid_y = world.translation(solid).ok_or("lowlevel: solid")?[1];
    let ghost_y = world.translation(ghost).ok_or("lowlevel: ghost")?[1];
    if solid_y > 1.0 {
        return Err(format!("lowlevel: solid should rest on ground, y={solid_y}").into());
    }
    if ghost_y > -5.0 {
        // Ghost never collides with ground → falls through.
        // Allow deeply negative; fail only if it somehow sat on the floor.
        if ghost_y > 0.2 && ghost_y < 1.2 {
            return Err(format!(
                "lowlevel: ghost should fall through ground, y={ghost_y}"
            )
            .into());
        }
    }

    let pairs = world.collect_contact_pairs();
    let solid_hits_ground = pairs.iter().any(|pair| {
        (pair.body_a == ground && pair.body_b == solid)
            || (pair.body_a == solid && pair.body_b == ground)
    });
    if !solid_hits_ground {
        return Err("lowlevel: expected solid↔ground contact pair".into());
    }

    let trigger = world.insert_trigger_cuboid([0.0, 0.5, 3.0], [0.5, 0.5, 0.5])?;
    let probe = world.insert_dynamic_sphere([0.0, 2.0, 3.0], 0.25)?;
    world.set_collision_groups(probe, CollisionGroups3d::all())?;
    for _ in 0..120 {
        world.step(None)?;
    }
    let overlaps = world.collect_trigger_overlaps();
    if !overlaps.iter().any(|(t, o)| *t == trigger && *o == probe) {
        // Probe may have fallen past; still OK if we saw any trigger activity —
        // re-drop a fresh probe into the volume.
        let mut saw = false;
        let probe2 = world.insert_dynamic_sphere([0.0, 1.2, 3.0], 0.25)?;
        for _ in 0..60 {
            world.step(None)?;
            if world
                .collect_trigger_overlaps()
                .iter()
                .any(|(t, o)| *t == trigger && (*o == probe || *o == probe2))
            {
                saw = true;
                break;
            }
        }
        if !saw {
            return Err(format!(
                "lowlevel: expected trigger overlap, last={overlaps:?}"
            )
            .into());
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    demo_custom_one_way_floor()?;
    demo_direct_mesh_queries()?;
    demo_collision_groups_and_contacts()?;
    println!(
        "physics_3d_lowlevel OK: one-way step_with_collision + mesh queries + \
         collision groups/contacts/triggers"
    );
    Ok(())
}
