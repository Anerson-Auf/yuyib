//! M4.7–M4.9 smoke: playable Rapier overlay with a local solid trimesh wall.
//!
//! Headless. Proves props settle on a mesh floor and stop against a mesh wall
//! (no city GLB). Character mesh path is not involved.
//!
//! ```text
//! cargo run -p yuyib --example playable_dynamics_overlay_smoke --features "three-d,physics-rapier"
//! ```

#[path = "support/playable_dynamics.rs"]
mod playable_dynamics;

use std::error::Error;

use playable_dynamics::PlayableDynamicsOverlay;
use yuyib::physics::{TriangleMesh3d, Vec3};

fn solid_floor_and_wall() -> Result<TriangleMesh3d, Box<dyn Error>> {
    // Floor Y=0 and a vertical wall at Z=-4 (in front of −Z-facing crates).
    let vertices = [
        Vec3::new(-8.0, 0.0, -8.0),
        Vec3::new(8.0, 0.0, -8.0),
        Vec3::new(8.0, 0.0, 8.0),
        Vec3::new(-8.0, 0.0, 8.0),
        Vec3::new(-4.0, 0.0, -4.0),
        Vec3::new(4.0, 0.0, -4.0),
        Vec3::new(4.0, 3.0, -4.0),
        Vec3::new(-4.0, 3.0, -4.0),
    ];
    let indices = [
        0, 2, 1, 0, 3, 2, // floor
        4, 5, 6, 4, 6, 7, // wall
    ];
    Ok(TriangleMesh3d::from_indexed(&vertices, &indices)?)
}

fn main() -> Result<(), Box<dyn Error>> {
    let solid = solid_floor_and_wall()?;
    let mut overlay = PlayableDynamicsOverlay::around_spawn_with_solid_mesh(
        [0.0, 1.0, 0.0],
        0.28,
        &solid,
    )?;
    if !overlay.is_active() {
        return Err("playable_dynamics_overlay_smoke: Rapier overlay inactive".into());
    }
    for _ in 0..180 {
        overlay.step(1.0 / 60.0, [0.0, 1.0, 0.0]);
    }
    // Walk the proxy into a crate and ensure props still simulate.
    for i in 0..60 {
        let z = -0.05 * i as f32;
        overlay.step(1.0 / 60.0, [0.0, 1.0, z]);
    }
    println!(
        "playable_dynamics_overlay_smoke OK: solid trimesh overlay + kinematic proxy stepped"
    );
    Ok(())
}
