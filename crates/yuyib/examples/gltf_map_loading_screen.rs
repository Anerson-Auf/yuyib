//! High-level streamed glTF map: responsive loading UI, bounded GPU upload and play.
//!
//! ```text
//! cargo run -p yuyib --example gltf_map_loading_screen
//! ```
//!
//! After loading: WASD moves, Space jumps, mouse looks and Esc exits.

mod support;

use std::{cell::RefCell, error::Error, path::PathBuf, rc::Rc};

use yuyib::{
    app::{Application, RenderLoop},
    assets::AssetLoadTakeError,
    character_3d::{CharacterController3d, CharacterControllerConfig3d, CharacterInput3d},
    game_3d::SceneBoundsResult3d,
    input::{FreeCameraConfig3d, FreeCameraController3d},
    physics::Vec2,
    platform::{CursorControl, WindowConfig, winit},
    render::ClearColor,
    render_3d::{
        Game3dScene, Game3dSceneConfig, GltfSceneGpuProgress, GltfSceneLoad, GltfSceneLoadConfig,
        GltfSceneLoadStage, LoadedGltfScene,
    },
};

use support::LoadingScreen;

const MAP_FILE: &str = "no_i_am_not_a_human_location__map.glb";

#[allow(
    clippy::too_many_lines,
    reason = "The complete window callbacks remain together; loading internals live in GltfSceneLoad."
)]
fn main() -> Result<(), Box<dyn Error>> {
    let asset_root = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../for_tests"));
    let loading = GltfSceneLoad::start(asset_root.join(MAP_FILE), GltfSceneLoadConfig::default())?;
    let state = Rc::new(RefCell::new(DemoState::Loading(loading)));
    let update_state = Rc::clone(&state);
    let window_state = Rc::clone(&state);
    let device_state = Rc::clone(&state);
    let render_state = Rc::clone(&state);
    let loading_renderer = Rc::new(RefCell::new(LoadingScreen::default()));
    let render_loading_renderer = Rc::clone(&loading_renderer);

    Application::new()
        .window(WindowConfig {
            title: "Yuyib — high-level streamed glTF map".to_owned(),
            mode: yuyib_platform::WindowMode::Fullscreen,
            ..Default::default()
        })
        .clear_color(ClearColor::linear(0.02, 0.03, 0.055, 1.0))
        .render_loop(RenderLoop::Continuous)
        .cursor_control(CursorControl::Released)
        .on_window_event(move |event, context| {
            if let DemoState::Loaded(map) = &mut *window_state.borrow_mut() {
                map.input.handle_window_event(event);
                let result = map.camera.handle_window_event(event);
                if let Some(cursor) = result.cursor_control {
                    context.set_cursor_control(cursor);
                }
                if result.exit_requested {
                    context.request_exit();
                }
            }
        })
        .on_device_event(move |event, _context| {
            if let DemoState::Loaded(map) = &mut *device_state.borrow_mut() {
                let _ = map.camera.handle_device_event(event);
            }
        })
        .on_frame(move |context| {
            let mut state = update_state.borrow_mut();
            let replacement = match &mut *state {
                DemoState::Loading(loading) => match loading.update().stage {
                    GltfSceneLoadStage::Ready => match loading.take_ready() {
                        Ok(scene) => Some(PlayableMap::new(scene, &asset_root).map_or_else(
                            |error| DemoState::Failed(error.to_string()),
                            |map| DemoState::Loaded(Box::new(map)),
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
                DemoState::Loaded(map) => {
                    if map.gpu.ready {
                        if !map.cursor_activated {
                            context.set_cursor_control(map.camera.initial_cursor_control());
                            map.cursor_activated = true;
                        }
                        map.step(context.frame().delta.as_secs_f32());
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
            DemoState::Loading(loading) => render_loading_renderer.borrow_mut().draw(
                frame,
                loading.progress().fraction().unwrap_or(0.03),
                false,
            ),
            DemoState::Loaded(map) => {
                *map.renderer.camera_mut() = map.camera.camera();
                match map.scene.prepare_for_frame(frame, &mut map.renderer) {
                    Ok(progress) => {
                        map.gpu = progress;
                        if progress.ready {
                            if let Err(error) = map.scene.render(frame, &mut map.renderer) {
                                eprintln!("Не удалось нарисовать карту: {error}");
                            }
                        } else {
                            render_loading_renderer.borrow_mut().draw(
                                frame,
                                progress.fraction(),
                                false,
                            );
                        }
                    }
                    Err(error) => {
                        eprintln!("Не удалось опубликовать GPU-ресурсы карты: {error}");
                        render_loading_renderer.borrow_mut().draw(frame, 1.0, true);
                    }
                }
            }
            DemoState::Failed(error) => {
                eprintln!("Не удалось загрузить карту: {error}");
                render_loading_renderer.borrow_mut().draw(frame, 1.0, true);
            }
        })
        .run()?;
    Ok(())
}

enum DemoState {
    Loading(GltfSceneLoad),
    Loaded(Box<PlayableMap>),
    Failed(String),
}

struct PlayableMap {
    scene: LoadedGltfScene,
    renderer: Game3dScene,
    camera: FreeCameraController3d,
    player: CharacterController3d,
    input: MapPlayerInput,
    physics_accumulator_seconds: f32,
    gpu: GltfSceneGpuProgress,
    cursor_activated: bool,
}

impl PlayableMap {
    fn new(scene: LoadedGltfScene, asset_root: &PathBuf) -> Result<Self, Box<dyn Error>> {
        let radius = match scene.bounds() {
            SceneBoundsResult3d::Bounds(bounds) => bounds.radius().max(10.0),
            SceneBoundsResult3d::Empty => 10.0,
        };
        let collider = scene
            .collider()
            .ok_or("high-level map config did not build a static collider")?;
        let player = CharacterController3d::spawn_in_triangle_mesh(
            CharacterControllerConfig3d::default(),
            collider.mesh(),
        )?;
        let spawn = player.position();
        let camera = FreeCameraController3d::looking_at(
            FreeCameraConfig3d {
                move_speed: (radius * 0.3).clamp(2.0, 150.0),
                near: (radius * 0.001).max(0.01),
                far: radius * 8.0,
                ..FreeCameraConfig3d::default()
            },
            [spawn.x, spawn.y + 1.45, spawn.z],
            [spawn.x, spawn.y + 1.45, spawn.z - 1.0],
        )?;
        Ok(Self {
            scene,
            renderer: Game3dScene::new(asset_root, Game3dSceneConfig::default())?,
            camera,
            player,
            input: MapPlayerInput::default(),
            physics_accumulator_seconds: 0.0,
            gpu: GltfSceneGpuProgress::default(),
            cursor_activated: false,
        })
    }

    fn step(&mut self, frame_delta_seconds: f32) {
        const FIXED_STEPS_PER_FRAME: usize = 8;
        const EYE_HEIGHT: f32 = 1.45;

        if self.camera.step(0.0).is_err()
            || !frame_delta_seconds.is_finite()
            || frame_delta_seconds < 0.0
        {
            return;
        }
        let fixed_delta = self.player.config().fixed_delta_seconds;
        self.physics_accumulator_seconds = (self.physics_accumulator_seconds
            + frame_delta_seconds.min(0.125))
        .min(fixed_delta * 8.0);
        let movement = self.input.movement_in_camera_space(&self.camera);
        let mut first = true;
        for _ in 0..FIXED_STEPS_PER_FRAME {
            if self.physics_accumulator_seconds < fixed_delta {
                break;
            }
            let Ok(input) = CharacterInput3d::new(movement, first && self.input.take_jump()) else {
                break;
            };
            first = false;
            let Some(collider) = self.scene.collider() else {
                break;
            };
            if self
                .player
                .step_on_triangle_mesh(input, collider.mesh())
                .is_err()
            {
                break;
            }
            self.physics_accumulator_seconds -= fixed_delta;
        }
        let position = self.player.position();
        let _ = self
            .camera
            .set_position([position.x, position.y + EYE_HEIGHT, position.z]);
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "Independent keys preserve simultaneous opposing-key semantics."
)]
#[derive(Clone, Copy, Debug, Default)]
struct MapPlayerInput {
    forward: bool,
    backward: bool,
    left: bool,
    right: bool,
    jump_queued: bool,
}

impl MapPlayerInput {
    fn handle_window_event(&mut self, event: &winit::event::WindowEvent) {
        use winit::{
            event::ElementState,
            keyboard::{KeyCode, PhysicalKey},
        };
        match event {
            winit::event::WindowEvent::KeyboardInput { event, .. } => {
                let PhysicalKey::Code(key) = event.physical_key else {
                    return;
                };
                let held = event.state == ElementState::Pressed;
                match key {
                    KeyCode::KeyW => self.forward = held,
                    KeyCode::KeyS => self.backward = held,
                    KeyCode::KeyA => self.left = held,
                    KeyCode::KeyD => self.right = held,
                    KeyCode::Space if held && !event.repeat => self.jump_queued = true,
                    _ => {}
                }
            }
            winit::event::WindowEvent::Focused(false) => *self = Self::default(),
            _ => {}
        }
    }

    fn movement_in_camera_space(self, camera: &FreeCameraController3d) -> Vec2 {
        let view = camera.camera();
        let mut forward = [
            view.target[0] - view.position[0],
            0.0,
            view.target[2] - view.position[2],
        ];
        let length = forward[0].hypot(forward[2]);
        if length <= f32::EPSILON {
            return Vec2::ZERO;
        }
        forward[0] /= length;
        forward[2] /= length;
        let right = [-forward[2], forward[0]];
        let forward_axis = f32::from(i8::from(self.forward) - i8::from(self.backward));
        let right_axis = f32::from(i8::from(self.right) - i8::from(self.left));
        Vec2::new(
            right[0].mul_add(right_axis, forward[0] * forward_axis),
            right[1].mul_add(right_axis, forward[2] * forward_axis),
        )
    }

    fn take_jump(&mut self) -> bool {
        let queued = self.jump_queued;
        self.jump_queued = false;
        queued
    }
}
