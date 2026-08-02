//! High-level material usage inventory + remap-by-name smoke.
//!
//! Shows the preferred game-code path when many meshes share a broken fallback
//! material such as `material_0`:
//!
//! 1. load the scene (no mesh-index glue);
//! 2. print [`LoadedGltfScene::material_usage_summary`];
//! 3. apply [`ModelMaterialPolicy::add_and_remap_users_of_named`];
//! 4. print usage again and render.
//!
//! Visual expectation after policy:
//! - left + center panels → bright cyan emissive (former `material_0` users);
//! - right panel → warm gray (`metal_gray`, untouched).
//!
//! ```text
//! cargo run -p yuyib --example gltf_material_usage
//! ```
//!
//! No external textures or `.glb` fixtures are required.

use std::{
    cell::RefCell,
    error::Error,
    fs,
    path::PathBuf,
    rc::Rc,
    time::{SystemTime, UNIX_EPOCH},
};

use yuyib::{
    app::{Application, RenderLoop},
    assets::AssetLoadTakeError,
    game_3d::SceneBoundsResult3d,
    input::{FreeCameraConfig3d, FreeCameraController3d},
    model::{Material, ModelMaterialPolicy},
    platform::{CursorControl, WindowConfig},
    render::{ClearColor, ColorPostProcess},
    render_3d::{
        Game3dLighting, Game3dScene, Game3dSceneConfig, Game3dShading, GltfSceneGpuProgress,
        GltfSceneLoad, GltfSceneLoadConfig, GltfSceneLoadStage, LambertLighting3d, LoadedGltfScene,
    },
};

fn main() -> Result<(), Box<dyn Error>> {
    let fixture = write_demo_glb()?;
    println!("demo GLB: {}", fixture.display());

    let loading = GltfSceneLoad::start(
        &fixture,
        GltfSceneLoadConfig::default().with_static_collider(false),
    )?;

    let state = Rc::new(RefCell::new(DemoState::Loading(loading)));
    let update_state = Rc::clone(&state);
    let window_state = Rc::clone(&state);
    let device_state = Rc::clone(&state);
    let render_state = Rc::clone(&state);

    Application::new()
        .window(WindowConfig {
            title: "Yuyib — material usage + remap-by-name".to_owned(),
            width: 1100,
            height: 540,
            ..Default::default()
        })
        .clear_color(ClearColor::linear(0.01, 0.015, 0.03, 1.0))
        .color_post_process(
            ColorPostProcess::filmic()
                .with_exposure_ev(0.35)
                .expect("demo exposure is within renderer limits"),
        )
        .render_loop(RenderLoop::Continuous)
        .cursor_control(CursorControl::Released)
        .on_window_event(move |event, context| {
            if let DemoState::Loaded(demo) = &mut *window_state.borrow_mut() {
                let result = demo.camera.handle_window_event(event);
                if let Some(cursor) = result.cursor_control {
                    context.set_cursor_control(cursor);
                }
                if result.exit_requested {
                    context.request_exit();
                }
            }
        })
        .on_device_event(move |event, _context| {
            if let DemoState::Loaded(demo) = &mut *device_state.borrow_mut() {
                let _ = demo.camera.handle_device_event(event);
            }
        })
        .on_frame(move |context| {
            let mut state = update_state.borrow_mut();
            let replacement = match &mut *state {
                DemoState::Loading(loading) => match loading.update().stage {
                    GltfSceneLoadStage::Ready => match loading.take_ready() {
                        Ok(scene) => Some(match DemoScene::new(scene) {
                            Ok(demo) => DemoState::Loaded(Box::new(demo)),
                            Err(error) => DemoState::Failed {
                                message: error.to_string(),
                                reported: false,
                            },
                        }),
                        Err(AssetLoadTakeError::NotReady) => None,
                        Err(error) => Some(DemoState::Failed {
                            message: error.to_string(),
                            reported: false,
                        }),
                    },
                    GltfSceneLoadStage::Failed => Some(DemoState::Failed {
                        message: loading.failure().map_or_else(
                            || "unknown scene load failure".to_owned(),
                            ToString::to_string,
                        ),
                        reported: false,
                    }),
                    GltfSceneLoadStage::Queued
                    | GltfSceneLoadStage::Reading
                    | GltfSceneLoadStage::Processing
                    | GltfSceneLoadStage::Taken => None,
                },
                DemoState::Loaded(demo) => {
                    if demo.gpu.ready {
                        if !demo.cursor_activated {
                            context.set_cursor_control(demo.camera.initial_cursor_control());
                            demo.cursor_activated = true;
                        }
                        if let Err(error) = demo.camera.step(context.frame().delta.as_secs_f32()) {
                            eprintln!("camera update failed: {error}");
                        }
                    }
                    None
                }
                DemoState::Failed { message, reported } => {
                    if !*reported {
                        eprintln!("material usage demo failed: {message}");
                        *reported = true;
                    }
                    None
                }
            };
            if let Some(replacement) = replacement {
                *state = replacement;
            }
        })
        .on_render(move |frame| match &mut *render_state.borrow_mut() {
            DemoState::Loading(_) | DemoState::Failed { .. } => {}
            DemoState::Loaded(demo) => {
                *demo.renderer.camera_mut() = demo.camera.camera();
                match demo.scene.prepare_for_frame(frame, &mut demo.renderer) {
                    Ok(progress) => {
                        demo.gpu = progress;
                        if progress.ready
                            && let Err(error) = demo.scene.render(frame, &mut demo.renderer)
                        {
                            eprintln!("render failed: {error}");
                        }
                    }
                    Err(error) => eprintln!("GPU publication failed: {error}"),
                }
            }
        })
        .run()?;
    Ok(())
}

enum DemoState {
    Loading(GltfSceneLoad),
    Loaded(Box<DemoScene>),
    Failed { message: String, reported: bool },
}

struct DemoScene {
    scene: LoadedGltfScene,
    renderer: Game3dScene,
    camera: FreeCameraController3d,
    gpu: GltfSceneGpuProgress,
    cursor_activated: bool,
}

impl DemoScene {
    fn new(mut scene: LoadedGltfScene) -> Result<Self, Box<dyn Error>> {
        println!("--- before policy ---");
        println!("{}", scene.material_usage_summary()?);
        let import_summary = scene.diagnostics_summary();
        if !import_summary.is_empty() {
            println!("import diagnostics:\n{import_summary}");
        }

        // High-level: remap every user of material_0 without listing mesh indices.
        let policy = ModelMaterialPolicy::new().add_and_remap_users_of_named(
            "material_0",
            Material::new()
                .with_name("yuyib.demo_cyan_neon")
                .with_base_color_factor([0.05, 0.55, 0.7, 1.0])
                .with_metallic_roughness(0.0, 0.35)
                .with_emissive_factor([0.2, 2.4, 2.8])
                .with_double_sided(true),
        );
        scene.apply_material_policy(&policy)?;

        println!("--- after policy ---");
        println!("{}", scene.material_usage_summary()?);
        let policy_summary = scene.diagnostics_summary();
        if !policy_summary.is_empty() {
            println!("import/policy diagnostics:\n{policy_summary}");
        }
        println!(
            "Expect: left+center cyan neon (former material_0 users), right warm gray (metal_gray)."
        );
        println!("Controls: WASD move, mouse look, Esc exit.");

        let radius = match scene.bounds() {
            SceneBoundsResult3d::Bounds(bounds) => bounds.radius().max(2.5),
            SceneBoundsResult3d::Empty => 2.5,
        };
        let camera = FreeCameraController3d::looking_at(
            FreeCameraConfig3d {
                move_speed: 3.0,
                near: 0.05,
                far: radius * 8.0,
                ..FreeCameraConfig3d::default()
            },
            [0.0, 0.55, 4.5],
            [0.0, 0.0, 0.0],
        )?;
        let lighting = LambertLighting3d::artistic(
            [-0.25, -1.0, -0.35],
            [1.0, 0.96, 0.9],
            1.1,
            [0.08, 0.09, 0.12],
        )?;
        let renderer = Game3dScene::new(
            ".",
            Game3dSceneConfig::default()
                .with_shading(Game3dShading::Pbr)
                .with_lighting(Game3dLighting::Fixed(lighting)),
        )?;
        Ok(Self {
            scene,
            renderer,
            camera,
            gpu: GltfSceneGpuProgress::default(),
            cursor_activated: false,
        })
    }
}

/// Three panels: left+center on `material_0`, right on `metal_gray`.
fn write_demo_glb() -> Result<PathBuf, Box<dyn Error>> {
    let mut binary = Vec::new();
    binary.extend(
        [0_u16, 1, 2, 0, 2, 3]
            .into_iter()
            .flat_map(u16::to_le_bytes),
    );
    for position in [
        [-0.55_f32, -0.55, 0.0],
        [0.55, -0.55, 0.0],
        [0.55, 0.55, 0.0],
        [-0.55, 0.55, 0.0],
    ] {
        binary.extend(position.into_iter().flat_map(f32::to_le_bytes));
    }
    for _ in 0..4 {
        binary.extend([0.0_f32, 0.0, 1.0].into_iter().flat_map(f32::to_le_bytes));
    }
    let json = format!(
        r#"{{"asset":{{"version":"2.0"}},"scene":0,"scenes":[{{"nodes":[0,1,2]}}],"nodes":[{{"name":"left","mesh":0,"translation":[-2.0,0,0]}},{{"name":"center","mesh":1,"translation":[0.0,0,0]}},{{"name":"right","mesh":2,"translation":[2.0,0,0]}}],"materials":[{{"name":"material_0","pbrMetallicRoughness":{{"baseColorFactor":[0.85,0.85,0.85,1],"metallicFactor":0,"roughnessFactor":1}}}},{{"name":"metal_gray","pbrMetallicRoughness":{{"baseColorFactor":[0.45,0.42,0.38,1],"metallicFactor":0.7,"roughnessFactor":0.35}}}}],"meshes":[{{"name":"left","primitives":[{{"attributes":{{"POSITION":1,"NORMAL":2}},"indices":0,"material":0}}]}},{{"name":"center","primitives":[{{"attributes":{{"POSITION":1,"NORMAL":2}},"indices":0,"material":0}}]}},{{"name":"right","primitives":[{{"attributes":{{"POSITION":1,"NORMAL":2}},"indices":0,"material":1}}]}}],"buffers":[{{"byteLength":{}}}],"bufferViews":[{{"buffer":0,"byteOffset":0,"byteLength":12}},{{"buffer":0,"byteOffset":12,"byteLength":48}},{{"buffer":0,"byteOffset":60,"byteLength":48}}],"accessors":[{{"bufferView":0,"componentType":5123,"count":6,"type":"SCALAR"}},{{"bufferView":1,"componentType":5126,"count":4,"type":"VEC3","min":[-0.55,-0.55,0],"max":[0.55,0.55,0]}},{{"bufferView":2,"componentType":5126,"count":4,"type":"VEC3"}}]}}"#,
        binary.len()
    );
    let glb = pack_glb(json.as_bytes(), binary)?;
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let path = std::env::temp_dir().join(format!("yuyib_material_usage_{stamp}.glb"));
    fs::write(&path, glb)?;
    Ok(path)
}

fn pack_glb(json: &[u8], binary: Vec<u8>) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut json = json.to_vec();
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    let mut binary = binary;
    while !binary.len().is_multiple_of(4) {
        binary.push(0);
    }
    let total = 12 + 8 + json.len() + 8 + binary.len();
    let mut glb = Vec::with_capacity(total);
    glb.extend(b"glTF");
    glb.extend(2_u32.to_le_bytes());
    glb.extend(u32::try_from(total)?.to_le_bytes());
    glb.extend(u32::try_from(json.len())?.to_le_bytes());
    glb.extend(*b"JSON");
    glb.extend(json);
    glb.extend(u32::try_from(binary.len())?.to_le_bytes());
    glb.extend([b'B', b'I', b'N', 0]);
    glb.extend(binary);
    Ok(glb)
}
