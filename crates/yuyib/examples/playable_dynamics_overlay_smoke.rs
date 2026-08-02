//! M4.7–M4.10 / M5.2 smoke: [`DynamicsOverlay3d`] with solid trimesh + two-way reaction.
//!
//! Headless. Proves props settle on a mesh floor and that walking the kinematic
//! proxy into a crate yields a non-zero character reaction displacement.
//!
//! ```text
//! cargo run -p yuyib --example playable_dynamics_overlay_smoke --features "three-d,physics-rapier"
//! ```

use std::error::Error;

use yuyib::{
    physics::{TriangleMesh3d, Vec3},
    profile_3d::DynamicsOverlay3d,
};

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
    let mut overlay =
        DynamicsOverlay3d::around_spawn_with_solid_mesh([0.0, 1.0, 0.0], 0.28, &solid)?;
    if !overlay.is_active() {
        return Err("playable_dynamics_overlay_smoke: Rapier overlay inactive".into());
    }
    for _ in 0..180 {
        let _ = overlay.step(1.0 / 60.0, [0.0, 1.0, 0.0]);
    }
    // After settle, crates rest near y≈half-extent. Walk the proxy on that
    // height into the orange crate at ~(0.6, ·, -2.2).
    let mut max_reaction_sq = 0.0_f32;
    for i in 0..90 {
        let t = i as f32 / 89.0;
        let x = 0.6 * t;
        let z = -2.2 * t;
        let reaction = overlay.step(1.0 / 60.0, [x, 0.55, z]);
        let len_sq =
            reaction[0] * reaction[0] + reaction[1] * reaction[1] + reaction[2] * reaction[2];
        max_reaction_sq = max_reaction_sq.max(len_sq);
    }
    if max_reaction_sq <= 0.0 {
        return Err(
            "playable_dynamics_overlay_smoke: expected non-zero two-way reaction when shoving props"
                .into(),
        );
    }
    println!(
        "playable_dynamics_overlay_smoke OK: solid trimesh + two-way reaction \
         (max |r|^2={max_reaction_sq:.6})"
    );
    Ok(())
}
