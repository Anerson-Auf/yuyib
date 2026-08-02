//! M4.4 smoke: fixed + revolute joints and collision groups.
//!
//! Headless. Does not touch [`yuyib::physics::TriangleMesh3d`].
//!
//! ```text
//! cargo run -p yuyib --example physics_rapier_joints_smoke --features physics-rapier
//! ```

use std::error::Error;

use yuyib::physics::{
    CollisionGroups3d, DynamicsBackend3d, DynamicsWorldConfig3d, RapierDynamicsWorld3d,
};

fn main() -> Result<(), Box<dyn Error>> {
    // Fixed joint holds a hanging cuboid.
    let mut world = RapierDynamicsWorld3d::new(DynamicsWorldConfig3d::earth_60hz())?;
    let anchor = world.insert_fixed_cuboid([0.0, 3.0, 0.0], [0.2, 0.2, 0.2])?;
    let hanging = world.insert_dynamic_cuboid([0.0, 2.25, 0.0], [0.25, 0.25, 0.25])?;
    let _fixed = world.insert_fixed_joint(anchor, hanging, [0.0, -0.5, 0.0], [0.0, 0.25, 0.0])?;
    for _ in 0..120 {
        world.step(None)?;
    }
    let hang_y = world
        .translation(hanging)
        .ok_or("physics_rapier_joints_smoke: missing hanging")?[1];
    if !(hang_y > 2.0 && hang_y < 2.6) {
        return Err(format!(
            "physics_rapier_joints_smoke: fixed joint failed, y={hang_y}"
        )
        .into());
    }

    // Revolute pendulum swings down.
    let mut pendulum = RapierDynamicsWorld3d::new(DynamicsWorldConfig3d::earth_60hz())?;
    let pivot = pendulum.insert_fixed_cuboid([0.0, 3.0, 0.0], [0.15, 0.15, 0.15])?;
    let bob = pendulum.insert_dynamic_sphere([1.5, 3.0, 0.0], 0.25)?;
    let _rev = pendulum.insert_revolute_joint(
        pivot,
        bob,
        [0.0, 0.0, 1.0],
        [0.0, 0.0, 0.0],
        [-1.5, 0.0, 0.0],
    )?;
    for _ in 0..240 {
        pendulum.step(None)?;
    }
    let bob_y = pendulum
        .translation(bob)
        .ok_or("physics_rapier_joints_smoke: missing bob")?[1];
    if bob_y >= 2.6 {
        return Err(format!(
            "physics_rapier_joints_smoke: revolute bob did not drop, y={bob_y}"
        )
        .into());
    }

    // Ghost layer falls through filtered ground.
    let mut filtered = RapierDynamicsWorld3d::new(DynamicsWorldConfig3d::earth_60hz())?;
    let ground = filtered.insert_fixed_cuboid([0.0, -0.25, 0.0], [3.0, 0.25, 3.0])?;
    filtered.set_collision_groups(ground, CollisionGroups3d::new(1, 1 | 2))?;
    let ghost = filtered.insert_dynamic_sphere([0.0, 2.0, 0.0], 0.3)?;
    filtered.set_collision_groups(ghost, CollisionGroups3d::new(4, 4))?;
    for _ in 0..150 {
        filtered.step(None)?;
    }
    let ghost_y = filtered
        .translation(ghost)
        .ok_or("physics_rapier_joints_smoke: missing ghost")?[1];
    if ghost_y > -0.5 {
        return Err(format!(
            "physics_rapier_joints_smoke: ghost should fall through, y={ghost_y}"
        )
        .into());
    }

    println!(
        "physics_rapier_joints_smoke OK: fixed_y={hang_y:.2}, revolute_y={bob_y:.2}, ghost_y={ghost_y:.2}"
    );
    Ok(())
}
