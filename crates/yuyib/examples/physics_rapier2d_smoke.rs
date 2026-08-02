//! M4.13 smoke: Rapier 2D dynamics facade — platformer settle, top-down trigger,
//! and kinematic platform carry.
//!
//! No window. Proves [`yuyib::physics::DynamicsBackend2d`] +
//! [`yuyib::physics::RapierDynamicsWorld2d`] without touching the 3D playable
//! [`yuyib::physics::TriangleMesh3d`] character path.
//!
//! ```text
//! cargo run -p yuyib --example physics_rapier2d_smoke --features physics-rapier2d
//! ```

use std::error::Error;

use yuyib::physics::{
    DynamicsBackend2d, DynamicsFixedStepper2d, DynamicsWorldConfig2d, RapierDynamicsWorld2d,
};

fn main() -> Result<(), Box<dyn Error>> {
    // --- platformer (earth gravity): ball settles on ground ---
    let mut platformer = RapierDynamicsWorld2d::new(DynamicsWorldConfig2d::earth_60hz())?;
    let _ground = platformer.insert_fixed_cuboid([0.0, -0.5], [5.0, 0.5])?;
    let ball = platformer.insert_dynamic_ball([0.0, 4.0], 0.5)?;
    platformer.set_ccd_enabled(ball, true)?;

    let start = platformer
        .translation(ball)
        .ok_or("physics_rapier2d_smoke: missing ball at start")?;
    let mut stepper = DynamicsFixedStepper2d::hz60();
    for _ in 0..240 {
        stepper.step_backend(&mut platformer, 1.0 / 60.0)?;
    }
    let end = platformer
        .translation(ball)
        .ok_or("physics_rapier2d_smoke: missing ball after settle")?;

    if !(end[1] > 0.4 && end[1] < 0.75) {
        return Err(format!(
            "physics_rapier2d_smoke: expected resting height near 0.5, got y={}",
            end[1]
        )
        .into());
    }
    if end[0].abs() > 0.35 {
        return Err(format!(
            "physics_rapier2d_smoke: expected near X origin, got {end:?}"
        )
        .into());
    }

    // --- top-down (zero-g): impulse motion + trigger overlap ---
    let mut top_down = RapierDynamicsWorld2d::new(DynamicsWorldConfig2d::top_down_60hz())?;
    let probe = top_down.insert_dynamic_ball([0.0, 0.0], 0.3)?;
    let trigger = top_down.insert_trigger_cuboid([2.0, 0.0], [0.6, 0.6])?;
    top_down.set_linear_velocity(probe, [3.0, 0.0])?;

    let mut saw_trigger = false;
    for _ in 0..120 {
        top_down.step(None)?;
        if top_down
            .collect_trigger_overlaps()
            .iter()
            .any(|(t, o)| *t == trigger && *o == probe)
        {
            saw_trigger = true;
        }
    }
    let probe_end = top_down
        .translation(probe)
        .ok_or("physics_rapier2d_smoke: missing top-down probe")?;

    if probe_end[0] < 1.0 {
        return Err(format!(
            "physics_rapier2d_smoke: top-down probe did not drift +X, got {probe_end:?}"
        )
        .into());
    }
    if probe_end[1].abs() > 0.25 {
        return Err(format!(
            "physics_rapier2d_smoke: top-down probe drifted off Y=0, got {probe_end:?}"
        )
        .into());
    }
    if !saw_trigger {
        return Err(
            "physics_rapier2d_smoke: probe never overlapped the top-down trigger".into(),
        );
    }

    // --- kinematic platform carry (earth gravity; friction coupling) ---
    let mut carry = RapierDynamicsWorld2d::new(DynamicsWorldConfig2d::earth_60hz())?;
    let platform = carry.insert_kinematic_cuboid([0.0, 0.5], [1.8, 0.15])?;
    let rider = carry.insert_dynamic_ball([0.0, 1.4], 0.3)?;
    for _ in 0..90 {
        carry.step(None)?;
    }
    let seated = carry
        .translation(rider)
        .ok_or("physics_rapier2d_smoke: missing rider after settle")?;
    carry.set_linear_velocity(platform, [1.0, 0.0])?;
    for _ in 0..90 {
        carry.step(None)?;
    }
    let rider_end = carry
        .translation(rider)
        .ok_or("physics_rapier2d_smoke: missing rider")?;
    let platform_end = carry
        .translation(platform)
        .ok_or("physics_rapier2d_smoke: missing platform")?;
    if platform_end[0] < 1.2 {
        return Err(format!(
            "physics_rapier2d_smoke: platform did not move enough, x={}",
            platform_end[0]
        )
        .into());
    }
    if rider_end[0] < seated[0] + 0.2 {
        return Err(format!(
            "physics_rapier2d_smoke: rider was not carried, start_x={} end_x={}",
            seated[0], rider_end[0]
        )
        .into());
    }

    println!(
        "physics_rapier2d_smoke OK: platformer y={:.2}->{:.2}; top_down x={:.2} trigger={saw_trigger}; carry rider_x={:.2}",
        start[1], end[1], probe_end[0], rider_end[0]
    );
    Ok(())
}
