//! M3 smoke: disk cook cache skips a second glTF parse.
//!
//! No window. Imports a tiny embedded triangle glTF twice through
//! [`yuyib::prelude::import_scene_bytes_cached`]. The second call must be a
//! cache hit.
//!
//! ```text
//! cargo run -p yuyib --example asset_cook_cache_smoke
//! ```

use std::{
    error::Error,
    time::{SystemTime, UNIX_EPOCH},
};

use yuyib::assets::{CookCache, content_hash_blake3};
use yuyib::prelude::{ImportOptions, import_scene_bytes_cached};

fn tiny_triangle_gltf() -> Vec<u8> {
    // Keep in sync with `yuyib-gltf` `cook::tests::triangle_gltf`.
    br#"{"asset":{"version":"2.0"},"buffers":[{"uri":"data:application/octet-stream;base64,AAABAAIAAAAAAAAAAAAAAAAAAAAAAIA/AAAAAAAAAAAAAAAAAACAPwAAAAA=","byteLength":44}],"bufferViews":[{"buffer":0,"byteOffset":0,"byteLength":6},{"buffer":0,"byteOffset":8,"byteLength":36}],"accessors":[{"bufferView":0,"componentType":5123,"count":3,"type":"SCALAR"},{"bufferView":1,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}],"meshes":[{"primitives":[{"attributes":{"POSITION":1},"indices":0}]}]}"#.to_vec()
}

fn main() -> Result<(), Box<dyn Error>> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_nanos();
    let root = std::env::temp_dir().join(format!("yuyib_asset_cook_cache_smoke_{stamp}"));
    let cache = CookCache::new(&root);
    let source = tiny_triangle_gltf();
    let options = ImportOptions::default();

    let (first, first_hit) = import_scene_bytes_cached(&source, options, &cache)
        .map_err(|error| error.to_string())?;
    if first_hit {
        return Err("asset_cook_cache_smoke: first import unexpectedly hit cache".into());
    }
    let (second, second_hit) = import_scene_bytes_cached(&source, options, &cache)
        .map_err(|error| error.to_string())?;
    if !second_hit {
        return Err("asset_cook_cache_smoke: second import missed cook cache".into());
    }
    if first.model.meshes().len() != second.model.meshes().len() {
        return Err("asset_cook_cache_smoke: cooked model mesh count diverged".into());
    }

    println!(
        "asset_cook_cache_smoke OK: source={}, hash={}, meshes={}, first_hit={first_hit}, \
         second_hit={second_hit}, cache={}",
        source.len(),
        content_hash_blake3(&source),
        first.model.meshes().len(),
        root.display()
    );
    let _ = std::fs::remove_dir_all(root);
    Ok(())
}
