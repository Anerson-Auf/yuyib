//! M5.2 smoke: [`AnimatedCharacterLoad3d`] imports a skeletal fixture on a
//! shared [`TaskPool`], then advances the walk clip headlessly.
//!
//! No window. Proves CPU load + animation without playable glue.
//!
//! ```text
//! cargo run -p yuyib --example animated_character_smoke --features profile-3d
//! ```

use std::{
    error::Error,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use yuyib::{
    profile_3d::AnimatedCharacterLoad3d,
    tasks::{TaskPool, TaskPoolConfig},
};

fn asset_root() -> Result<PathBuf, Box<dyn Error>> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest
        .parent()
        .and_then(|crates| crates.parent())
        .ok_or("workspace root")?
        .join("for_tests");
    if !root.is_dir() {
        return Err(format!("missing for_tests at {}", root.display()).into());
    }
    Ok(root)
}

fn main() -> Result<(), Box<dyn Error>> {
    let root = asset_root()?;
    let path = root.join("sci-fi_girl_v.02_walkcycle_test.glb");
    let pool = Arc::new(TaskPool::new(TaskPoolConfig::new(2, 4)?)?);
    let mut load = AnimatedCharacterLoad3d::start_on(&pool, &path, &root, 0)?;
    let mut character = load.wait_ready(Duration::from_secs(60))?;

    let bind = character.pose().world_matrices().to_vec();
    character.advance(0.4, true);
    let advanced = character.pose().world_matrices();
    let changed = bind
        .iter()
        .zip(advanced.iter())
        .any(|(before, after)| before != after);
    if !changed {
        return Err("walk clip advance produced no pose change".into());
    }

    let left = character
        .find_node("Eye_L_056")
        .ok_or("missing Eye_L_056")?;
    let right = character
        .find_node("Eye_R_047")
        .ok_or("missing Eye_R_047")?;
    let identity = [
        1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ];
    let _ = character.node_world_position(left, identity)?;
    let _ = character.node_world_position(right, identity)?;
    let focus = character.camera_focus_from_bones(
        "Eye_L_056",
        "Eye_R_047",
        identity,
        Some((0.8, 2.2)),
    )?;
    if !focus[1].is_finite() {
        return Err("camera focus is not finite".into());
    }

    println!(
        "animated_character_smoke OK: loaded {} skins={} clips={} focus=[{:.2},{:.2},{:.2}]",
        path.display(),
        character.asset().scene.skins().len(),
        character.asset().scene.animations().len(),
        focus[0],
        focus[1],
        focus[2],
    );
    Ok(())
}
