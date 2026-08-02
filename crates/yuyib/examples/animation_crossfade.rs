//! Fixture-free animation cross-fade and mid-transition retarget example.
//!
//! Run from the workspace root:
//!
//! ```text
//! cargo run -p yuyib --example animation_crossfade
//! ```
//!
//! The embedded glTF contains one node and three deliberately simple clips.
//! No window, GPU, model file or user-provided fixture is required.

use yuyib::gltf::{
    AnimationClipIndex, AnimationCrossFadeChange, AnimationCrossFadeDuration,
    AnimationCrossFadeError, AnimationCrossFadeMixer, AnimationSnapshot, ImportOptions,
    ImportedScene, LocalTransform, import_scene_bytes_embedded,
};

// Buffer layout: one minimal triangle, two f32 key times and two VEC3 values
// for each of idle (x=0), walk (x=4) and run (x=10). Keeping the fixture inline
// makes this executable documentation deterministic and repository-independent.
const ANIMATED_NODE_GLTF: &str = r#"{
  "asset":{"version":"2.0"},
  "buffers":[{
    "uri":"data:application/octet-stream;base64,AAABAAIAAAAAAAAAAAAAAAAAAAAAAIA/AAAAAAAAAAAAAAAAAACAPwAAAAAAAAAAAACAPwAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAgEAAAAAAAAAAAAAAgEAAAAAAAAAAAAAAIEEAAAAAAAAAAAAAIEEAAAAAAAAAAA==",
    "byteLength":124
  }],
  "bufferViews":[
    {"buffer":0,"byteOffset":0,"byteLength":6,"target":34963},
    {"buffer":0,"byteOffset":8,"byteLength":36,"target":34962},
    {"buffer":0,"byteOffset":44,"byteLength":8},
    {"buffer":0,"byteOffset":52,"byteLength":24},
    {"buffer":0,"byteOffset":76,"byteLength":24},
    {"buffer":0,"byteOffset":100,"byteLength":24}
  ],
  "accessors":[
    {"bufferView":0,"componentType":5123,"count":3,"type":"SCALAR","min":[0],"max":[2]},
    {"bufferView":1,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]},
    {"bufferView":2,"componentType":5126,"count":2,"type":"SCALAR","min":[0],"max":[1]},
    {"bufferView":3,"componentType":5126,"count":2,"type":"VEC3"},
    {"bufferView":4,"componentType":5126,"count":2,"type":"VEC3"},
    {"bufferView":5,"componentType":5126,"count":2,"type":"VEC3"}
  ],
  "meshes":[{"primitives":[{"attributes":{"POSITION":1},"indices":0}]}],
  "nodes":[{"name":"AnimatedRoot","mesh":0}],
  "scenes":[{"nodes":[0]}],
  "scene":0,
  "animations":[
    {
      "name":"idle",
      "samplers":[{"input":2,"output":3,"interpolation":"LINEAR"}],
      "channels":[{"sampler":0,"target":{"node":0,"path":"translation"}}]
    },
    {
      "name":"walk",
      "samplers":[{"input":2,"output":4,"interpolation":"LINEAR"}],
      "channels":[{"sampler":0,"target":{"node":0,"path":"translation"}}]
    },
    {
      "name":"run",
      "samplers":[{"input":2,"output":5,"interpolation":"LINEAR"}],
      "channels":[{"sampler":0,"target":{"node":0,"path":"translation"}}]
    }
  ]
}"#;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let asset =
        import_scene_bytes_embedded(ANIMATED_NODE_GLTF.as_bytes(), ImportOptions::skeletal())?;
    let scene = &asset.scene;
    let idle = AnimationClipIndex::new(0);
    let walk = AnimationClipIndex::new(1);
    let run = AnimationClipIndex::new(2);
    let duration = AnimationCrossFadeDuration::new(0.4)?;
    let mut mixer = AnimationCrossFadeMixer::new(scene, idle)?;

    print_mixer_pose("initial idle", scene, &mut mixer)?;

    let change = mixer.transition_to(scene, walk, duration)?;
    assert_eq!(change, AnimationCrossFadeChange::Started);
    let x = translation_x(mixer.advance_and_snapshot(scene, 0.2)?);
    println!(
        "idle -> walk: change={change:?}, progress={:.2}, x={:.2}",
        mixer.transition_progress(),
        x
    );

    // Retarget before idle -> walk finishes. The new blend starts at the last
    // visible x=2 pose, instead of snapping back to idle or restarting at walk.
    let change = mixer.transition_to(scene, run, duration)?;
    assert_eq!(change, AnimationCrossFadeChange::Retargeted);
    print_mixer_pose("retarget source", scene, &mut mixer)?;

    for step in 1..=2 {
        let x = translation_x(mixer.advance_and_snapshot(scene, 0.2)?);
        println!(
            "walk -> run step {step}: progress={:.2}, x={:.2}",
            mixer.transition_progress(),
            x
        );
    }

    assert!(!mixer.is_transitioning());
    assert_eq!(mixer.active_clip(), run);
    Ok(())
}

fn print_mixer_pose(
    label: &str,
    scene: &ImportedScene,
    mixer: &mut AnimationCrossFadeMixer,
) -> Result<(), AnimationCrossFadeError> {
    let x = translation_x(mixer.snapshot(scene)?);
    println!(
        "{label}: active={}, target={:?}, progress={:.2}, x={:.2}",
        mixer.active_clip().get(),
        mixer.target_clip().map(AnimationClipIndex::get),
        mixer.transition_progress(),
        x
    );
    Ok(())
}

fn translation_x(snapshot: &AnimationSnapshot) -> f32 {
    match snapshot.local_transforms()[0] {
        LocalTransform::Trs { translation, .. } => translation[0],
        LocalTransform::Matrix { .. } => unreachable!("fixture node is authored as TRS"),
    }
}
