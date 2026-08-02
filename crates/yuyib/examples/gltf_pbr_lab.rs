//! High-level streamed PBR demo using the real sci-fi lab GLB fixture.
//!
//! Run from the workspace root:
//!
//! ```text
//! cargo run -p yuyib --example gltf_pbr_lab
//! ```
//!
//! This exercises background glTF import/image decode, bounded PBR GPU
//! residency, hierarchy rendering, normal mapping, metallic/roughness and
//! emissive textures. Controls: `WASD`, `Space`/`Ctrl`, `Shift`, mouse, `Esc`.

mod support;

use std::{cell::RefCell, error::Error, path::PathBuf, rc::Rc};

use support::LoadingScreen;
use yuyib::{
    app::{Application, RenderLoop},
    assets::AssetLoadTakeError,
    game_3d::SceneBoundsResult3d,
    input::{FreeCameraConfig3d, FreeCameraController3d},
    platform::{CursorControl, WindowConfig},
    render::ClearColor,
    render_3d::{
        Game3dScene, Game3dSceneConfig, Game3dShading, GltfSceneGpuProgress, GltfSceneLoad,
        GltfSceneLoadConfig, GltfSceneLoadStage, LoadedGltfScene,
    },
};

const LAB_FILE: &str = "sci-fi_lab.glb";

#[allow(
    clippy::too_many_lines,
    reason = "The complete window callbacks stay together while loading and overlay internals use high-level helpers."
)]
fn main() -> Result<(), Box<dyn Error>> {
    let asset_root = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../for_tests"));
    let loading = GltfSceneLoad::start(
        asset_root.join(LAB_FILE),
        GltfSceneLoadConfig::default().with_static_collider(false),
    )?;
    let state = Rc::new(RefCell::new(DemoState::Loading(loading)));
    let update_state = Rc::clone(&state);
    let window_state = Rc::clone(&state);
    let device_state = Rc::clone(&state);
    let render_state = Rc::clone(&state);
    let loading_screen = Rc::new(RefCell::new(LoadingScreen::default()));
    let render_loading_screen = Rc::clone(&loading_screen);

    Application::new()
        .window(WindowConfig {
            title: "Yuyib — streamed textured PBR sci-fi lab".to_owned(),
            ..Default::default()
        })
        .clear_color(ClearColor::linear(0.008, 0.012, 0.025, 1.0))
        .render_loop(RenderLoop::Continuous)
        .cursor_control(CursorControl::Released)
        .on_window_event(move |event, context| {
            if let DemoState::Loaded(lab) = &mut *window_state.borrow_mut() {
                let result = lab.camera.handle_window_event(event);
                if let Some(cursor) = result.cursor_control {
                    context.set_cursor_control(cursor);
                }
                if result.exit_requested {
                    context.request_exit();
                }
            }
        })
        .on_device_event(move |event, _context| {
            if let DemoState::Loaded(lab) = &mut *device_state.borrow_mut() {
                let _ = lab.camera.handle_device_event(event);
            }
        })
        .on_frame(move |context| {
            let mut state = update_state.borrow_mut();
            let replacement = match &mut *state {
                DemoState::Loading(loading) => match loading.update().stage {
                    GltfSceneLoadStage::Ready => match loading.take_ready() {
                        Ok(scene) => Some(PbrLab::new(scene, &asset_root).map_or_else(
                            |error| DemoState::Failed(error.to_string()),
                            |lab| DemoState::Loaded(Box::new(lab)),
                        )),
                        Err(AssetLoadTakeError::NotReady) => None,
                        Err(error) => Some(DemoState::Failed(error.to_string())),
                    },
                    GltfSceneLoadStage::Failed => {
                        Some(DemoState::Failed(loading.failure().map_or_else(
                            || "unknown scene load failure".to_owned(),
                            ToString::to_string,
                        )))
                    }
                    GltfSceneLoadStage::Queued
                    | GltfSceneLoadStage::Reading
                    | GltfSceneLoadStage::Processing
                    | GltfSceneLoadStage::Taken => None,
                },
                DemoState::Loaded(lab) => {
                    if lab.gpu.ready {
                        if !lab.cursor_activated {
                            context.set_cursor_control(lab.camera.initial_cursor_control());
                            lab.cursor_activated = true;
                        }
                        if let Err(error) = lab.camera.step(context.frame().delta.as_secs_f32()) {
                            eprintln!("PBR camera update failed: {error}");
                        }
                    }
                    None
                }
                DemoState::Failed(_) => None,
            };
            if let Some(replacement) = replacement {
                *state = replacement;
            }
        })
        .on_render(move |frame| match &mut *render_state.borrow_mut() {
            DemoState::Loading(loading) => render_loading_screen.borrow_mut().draw(
                frame,
                loading.progress().fraction().unwrap_or(0.03),
                false,
            ),
            DemoState::Loaded(lab) => {
                *lab.renderer.camera_mut() = lab.camera.camera();
                match lab.scene.prepare_for_frame(frame, &mut lab.renderer) {
                    Ok(progress) => {
                        lab.gpu = progress;
                        if progress.ready {
                            match lab.scene.render(frame, &mut lab.renderer) {
                                Ok(draw_summary) if !lab.reported_stats => {
                                    lab.reported_stats = true;
                                    println!("PBR ready: {}", draw_summary.summary_line());
                                }
                                Ok(_) => {}
                                Err(error) => eprintln!("PBR lab render failed: {error}"),
                            }
                        } else {
                            render_loading_screen.borrow_mut().draw(
                                frame,
                                progress.fraction(),
                                false,
                            );
                        }
                    }
                    Err(error) => {
                        eprintln!("PBR GPU publication failed: {error}");
                        render_loading_screen.borrow_mut().draw(frame, 1.0, true);
                    }
                }
            }
            DemoState::Failed(error) => {
                eprintln!("PBR scene load failed: {error}");
                render_loading_screen.borrow_mut().draw(frame, 1.0, true);
            }
        })
        .run()?;
    Ok(())
}

enum DemoState {
    Loading(GltfSceneLoad),
    Loaded(Box<PbrLab>),
    Failed(String),
}

struct PbrLab {
    scene: LoadedGltfScene,
    renderer: Game3dScene,
    camera: FreeCameraController3d,
    gpu: GltfSceneGpuProgress,
    cursor_activated: bool,
    reported_stats: bool,
}

impl PbrLab {
    fn new(scene: LoadedGltfScene, asset_root: &PathBuf) -> Result<Self, Box<dyn Error>> {
        let (centre, radius) = match scene.bounds() {
            SceneBoundsResult3d::Bounds(bounds) => (bounds.centre(), bounds.radius().max(2.0)),
            SceneBoundsResult3d::Empty => ([0.0; 3], 10.0),
        };
        let camera = FreeCameraController3d::looking_at(
            FreeCameraConfig3d {
                move_speed: (radius * 0.35).clamp(2.0, 100.0),
                near: (radius * 0.001).max(0.01),
                far: radius * 10.0,
                ..FreeCameraConfig3d::default()
            },
            [
                centre[0] + radius * 1.2,
                centre[1] + radius * 0.7,
                centre[2] + radius * 1.2,
            ],
            centre,
        )?;
        let renderer = Game3dScene::new(
            asset_root,
            Game3dSceneConfig::default()
                .with_camera(camera.camera())
                .with_shading(Game3dShading::Pbr),
        )?;
        Ok(Self {
            scene,
            renderer,
            camera,
            gpu: GltfSceneGpuProgress::default(),
            cursor_activated: false,
            reported_stats: false,
        })
    }
}
