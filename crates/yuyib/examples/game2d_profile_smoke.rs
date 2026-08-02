//! M5 smoke: [`yuyib::profile_2d::Game2dProfile`] owns World + Game2dScene.
//!
//! No window. Proves the composition shell constructs and exposes escape hatches.
//!
//! ```text
//! cargo run -p yuyib --example game2d_profile_smoke --features profile-2d
//! ```

use std::error::Error;

use yuyib::game_2d::Game2dSceneConfig;
use yuyib::profile_2d::Game2dProfile;

fn main() -> Result<(), Box<dyn Error>> {
    let mut profile = Game2dProfile::new(Game2dSceneConfig::default());
    profile.scene_mut().camera_mut().position = [10.0, 20.0];
    let position = profile.scene_mut().camera_mut().position;
    let _ = profile.world_mut();

    println!(
        "game2d_profile_smoke OK: camera=({:.1},{:.1})",
        position[0], position[1]
    );
    Ok(())
}
