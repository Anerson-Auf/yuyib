//! M4.3 smoke: kinematic platform + trigger overlap + CCD flag.
//!
//! Headless. Proves velocity-based kinematic motion, sensor volumes that do not
//! block dynamics, and CCD enablement — without touching
//! [`yuyib::physics::TriangleMesh3d`].
//!
//! ```text
//! cargo run -p yuyib --example physics_rapier_kinematic_smoke --features physics-rapier
//! ```

use std::error::Error;

use yuyib::physics::{
    DynamicsBackend3d, DynamicsWorldConfig3d, RapierDynamicsWorld3d,
};

fn main() -> Result<(), Box<dyn Error>> {
    let mut world = RapierDynamicsWorld3d::new(DynamicsWorldConfig3d::earth_60hz())?;

    // --- kinematic carry ---
    let platform = world.insert_kinematic_cuboid([0.0, 0.5, 0.0], [1.8, 0.15, 1.8])?;
    let rider = world.insert_dynamic_sphere([0.0, 1.4, 0.0], 0.3)?;
    world.set_ccd_enabled(rider, true)?;

    for _ in 0..90 {
        world.step(None)?;
    }
    let seated = world
        .translation(rider)
        .ok_or("physics_rapier_kinematic_smoke: missing rider after settle")?;
    if !(seated[1] > 0.6 && seated[1] < 1.2) {
        return Err(format!(
            "physics_rapier_kinematic_smoke: expected rider on platform, y={}",
            seated[1]
        )
        .into());
    }

    world.set_linear_velocity(platform, [1.0, 0.0, 0.0])?;
    for _ in 0..90 {
        world.step(None)?;
    }
    let end_rider = world
        .translation(rider)
        .ok_or("physics_rapier_kinematic_smoke: missing rider")?;
    let end_platform = world
        .translation(platform)
        .ok_or("physics_rapier_kinematic_smoke: missing platform")?;
    if end_platform[0] < 1.2 {
        return Err(format!(
            "physics_rapier_kinematic_smoke: platform did not move enough, x={}",
            end_platform[0]
        )
        .into());
    }
    if end_rider[0] < seated[0] + 0.2 {
        return Err(format!(
            "physics_rapier_kinematic_smoke: rider was not carried, start_x={} end_x={}",
            seated[0], end_rider[0]
        )
        .into());
    }

    // --- trigger (independent fall-through) ---
    let mut trigger_world = RapierDynamicsWorld3d::new(DynamicsWorldConfig3d::earth_60hz())?;
    let _ground = trigger_world.insert_fixed_cuboid([0.0, -0.25, 0.0], [3.0, 0.25, 3.0])?;
    let trigger = trigger_world.insert_trigger_cuboid([0.0, 1.5, 0.0], [0.8, 0.8, 0.8])?;
    let probe = trigger_world.insert_dynamic_sphere([0.0, 4.0, 0.0], 0.3)?;

    let mut saw_trigger = false;
    for _ in 0..240 {
        trigger_world.step(None)?;
        if trigger_world
            .collect_trigger_overlaps()
            .iter()
            .any(|(t, o)| *t == trigger && *o == probe)
        {
            saw_trigger = true;
        }
    }
    if !saw_trigger {
        return Err(
            "physics_rapier_kinematic_smoke: probe never overlapped the trigger volume".into(),
        );
    }
    let probe_end = trigger_world
        .translation(probe)
        .ok_or("physics_rapier_kinematic_smoke: missing probe")?;
    if probe_end[1] > 0.8 {
        return Err(format!(
            "physics_rapier_kinematic_smoke: sensor blocked the probe, y={}",
            probe_end[1]
        )
        .into());
    }

    println!(
        "physics_rapier_kinematic_smoke OK: platform_x={:.2}, rider_x={:.2}, trigger_hit={saw_trigger}",
        end_platform[0], end_rider[0]
    );
    Ok(())
}
