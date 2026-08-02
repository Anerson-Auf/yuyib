//! M4.5 smoke: convex hull collider + limited prismatic joint.
//!
//! Headless. Does not touch [`yuyib::physics::TriangleMesh3d`].
//!
//! ```text
//! cargo run -p yuyib --example physics_rapier_convex_smoke --features physics-rapier
//! ```

use std::error::Error;

use yuyib::physics::{
    DynamicsBackend3d, DynamicsWorldConfig3d, RapierDynamicsWorld3d,
};

fn diamond_points() -> [[f32; 3]; 6] {
    [
        [0.5, 0.0, 0.0],
        [-0.5, 0.0, 0.0],
        [0.0, 0.6, 0.0],
        [0.0, -0.6, 0.0],
        [0.0, 0.0, 0.5],
        [0.0, 0.0, -0.5],
    ]
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut world = RapierDynamicsWorld3d::new(DynamicsWorldConfig3d::earth_60hz())?;
    let _ground = world.insert_fixed_cuboid([0.0, -0.25, 0.0], [4.0, 0.25, 4.0])?;
    let hull = world.insert_dynamic_convex_hull([0.0, 2.5, 0.0], &diamond_points())?;
    for _ in 0..240 {
        world.step(None)?;
    }
    let hull_y = world
        .translation(hull)
        .ok_or("physics_rapier_convex_smoke: missing hull")?[1];
    if !(hull_y > 0.4 && hull_y < 1.2) {
        return Err(format!(
            "physics_rapier_convex_smoke: hull should rest on ground, y={hull_y}"
        )
        .into());
    }

    let mut slide = RapierDynamicsWorld3d::new(DynamicsWorldConfig3d::earth_60hz())?;
    let rail = slide.insert_fixed_cuboid([0.0, 1.0, 0.0], [0.2, 0.2, 0.2])?;
    let slider = slide.insert_dynamic_cuboid([0.0, 1.0, 0.0], [0.25, 0.25, 0.25])?;
    let _joint = slide.insert_prismatic_joint(
        rail,
        slider,
        [1.0, 0.0, 0.0],
        [0.0, 0.0, 0.0],
        [0.0, 0.0, 0.0],
        Some([-0.8, 0.8]),
    )?;
    slide.set_linear_velocity(slider, [6.0, 0.0, 0.0])?;
    for _ in 0..180 {
        slide.step(None)?;
    }
    let slider_x = slide
        .translation(slider)
        .ok_or("physics_rapier_convex_smoke: missing slider")?[0];
    if !(slider_x > 0.5 && slider_x < 1.0) {
        return Err(format!(
            "physics_rapier_convex_smoke: prismatic limit failed, x={slider_x}"
        )
        .into());
    }

    println!(
        "physics_rapier_convex_smoke OK: hull_y={hull_y:.2}, slider_x={slider_x:.2}"
    );
    Ok(())
}
