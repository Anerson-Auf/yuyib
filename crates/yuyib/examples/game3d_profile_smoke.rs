//! M5 smoke: [`yuyib::profile_3d::Game3dProfile`] loads a tiny glTF to Ready.
//!
//! No window. Proves shared TaskPool + Game3dScene + GltfSceneLoad lifecycle
//! without manually wiring those owners. Character adapter is covered by unit
//! tests in `yuyib-profile-3d`.
//!
//! ```text
//! cargo run -p yuyib --example game3d_profile_smoke --features profile-3d
//! ```

use std::{
    error::Error,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use yuyib::profile_3d::{Game3dProfile, Game3dProfileConfig};

fn tiny_triangle_gltf() -> Vec<u8> {
    br#"{"asset":{"version":"2.0"},"scene":0,"scenes":[{"nodes":[0]}],"nodes":[{"mesh":0}],"buffers":[{"uri":"data:application/octet-stream;base64,AAABAAIAAAAAAAAAAAAAAAAAAAAAAIA/AAAAAAAAAAAAAAAAAACAPwAAAAA=","byteLength":44}],"bufferViews":[{"buffer":0,"byteOffset":0,"byteLength":6},{"buffer":0,"byteOffset":8,"byteLength":36}],"accessors":[{"bufferView":0,"componentType":5123,"count":3,"type":"SCALAR"},{"bufferView":1,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}],"meshes":[{"primitives":[{"attributes":{"POSITION":1},"indices":0}]}]}"#.to_vec()
}

fn main() -> Result<(), Box<dyn Error>> {
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let root = std::env::temp_dir().join(format!("yuyib_game3d_profile_smoke_{stamp}"));
    std::fs::create_dir_all(&root)?;
    let path = root.join("triangle.gltf");
    std::fs::write(&path, tiny_triangle_gltf())?;

    let mut profile = Game3dProfile::new(Game3dProfileConfig::new(&root))?;
    profile.start_gltf(&path)?;
    let loaded = profile.wait_ready(Duration::from_secs(30))?;
    let _ = loaded.bounds();

    println!(
        "game3d_profile_smoke OK: loaded via Game3dProfile from {}",
        path.display()
    );
    let _ = std::fs::remove_dir_all(root);
    Ok(())
}
