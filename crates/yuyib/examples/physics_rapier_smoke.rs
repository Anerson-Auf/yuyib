//! M4.1 smoke: Rapier dynamics facade — sphere falls onto a fixed cuboid.
//!
//! No window. Proves [`yuyib::physics::DynamicsBackend3d`] +
//! [`yuyib::physics::RapierDynamicsWorld3d`] without touching the playable
//! [`yuyib::physics::TriangleMesh3d`] character path.
//!
//! ```text
//! cargo run -p yuyib --example physics_rapier_smoke --features physics-rapier
//! ```

use std::error::Error;

use yuyib::physics::{
    DynamicsBackend3d, DynamicsWorldConfig3d, RapierDynamicsWorld3d,
};

fn main() -> Result<(), Box<dyn Error>> {
    let mut world = RapierDynamicsWorld3d::new(DynamicsWorldConfig3d::earth_60hz())?;
    let _ground = world.insert_fixed_cuboid([0.0, -0.5, 0.0], [5.0, 0.5, 5.0])?;
    let ball = world.insert_dynamic_sphere([0.0, 4.0, 0.0], 0.5)?;

    let start = world
        .translation(ball)
        .ok_or("physics_rapier_smoke: missing ball at start")?;
    for _ in 0..240 {
        world.step(None)?;
    }
    let end = world
        .translation(ball)
        .ok_or("physics_rapier_smoke: missing ball after settle")?;

    if !(end[1] > 0.4 && end[1] < 0.75) {
        return Err(format!(
            "physics_rapier_smoke: expected resting height near 0.5, got y={}",
            end[1]
        )
        .into());
    }
    if end[0].abs() > 0.35 || end[2].abs() > 0.35 {
        return Err(format!(
            "physics_rapier_smoke: expected near XZ origin, got {end:?}"
        )
        .into());
    }

    println!(
        "physics_rapier_smoke OK: start_y={:.2}, end_y={:.2}, end_xz=({:.3},{:.3})",
        start[1], end[1], end[0], end[2]
    );
    Ok(())
}
