//! Playable streamed street-city vertical slice with chase / first-person views.
//!
//! Map + character load through [`yuyib::profile_3d::Game3dPlayableLoad`].
//! Input, mesh motor, chase camera, and avatar draw go through
//! [`yuyib::profile_3d::PlayableLoop3d`] (M5.2). WASD moves, Space jumps, V
//! toggles first/third person, mouse looks and Esc exits.
//!
//! Optional M4.7–M4.10 Rapier props overlay (map trimesh + two-way soft push).
//! Walk into crates to push them; heavy props can nudge the mesh character back.
//! Props collide with map walls/floors from the solid layer. Does **not** replace
//! [`yuyib::physics::TriangleMesh3d`] character collision:
//!
//! ```text
//! cargo run -p yuyib --example cyberpunk_city_playable --features physics-rapier
//! ```
//!
//! Default (mesh character only):
//!
//! ```text
//! cargo run -p yuyib --example cyberpunk_city_playable
//! ```
//!
//! Character look A/B (front≠back skin). Orbit yaw after each toggle:
//! - `F1` — SSAO on/off (map AO pass)
//! - `F2` — morph/cloth on/off (skin only when off)
//! - `F3` — character flat lighting: normal key vs pure white ×1
//! - Startup: `YUYIB_CHAR_DIAG_NO_SSAO=1`, `YUYIB_CHAR_DIAG_NO_MORPH=1`,
//!   `YUYIB_CHAR_DIAG_NO_POST=1` (no bloom/filmic/FXAA)

mod support;

use std::{
    cell::RefCell,
    env,
    error::Error,
    path::Path,
    rc::Rc,
    sync::Arc,
};

use yuyib::{
    app::{Application, RenderLoop},
    input::PlayerCharacterControlConfig3d,
    platform::{CursorControl, WindowConfig},
    profile_3d::{
        AnimatedCharacter3d, DynamicsOverlay3d, EnvironmentPreset, Game3dPlayableLoad,
        Game3dPlayableLoadStatus, Game3dProfile, Game3dProfileConfig, PlayableDrawStatus,
        PlayableLoop3d, PlayableLoopDesc3d, PlayableLoopError3d,
    },
    render::{BloomConfig, ClearColor, ColorGradeConfig, ColorPostProcess, FxaaConfig},
    render_3d::{
        GltfSceneGpuProgress, LambertLighting3d, ModelUploadBudget3d, SsaoPolicy,
    },
    tasks::{TaskPool, TaskPoolConfig},
};
use yuyib_platform::winit::{
    event::{ElementState, WindowEvent},
    keyboard::{KeyCode, PhysicalKey},
};

use support::{LoadingScreen, playable_character, street_city};

const CAMERA_MIN_HEIGHT_ABOVE_FEET: f32 = 0.85;
const CAMERA_MAX_HEIGHT_ABOVE_FEET: f32 = 1.65;
const CAMERA_CHASE_DISTANCE: f32 = 3.2;
const CAMERA_INITIAL_HEIGHT: f32 = 0.25;
const CHARACTER_TURN_SPEED_RADIANS_PER_SECOND: f32 = std::f32::consts::TAU * 1.25;
const CHARACTER_TEXTURE_SLOTS_PER_FRAME: usize = 2;
const CHARACTER_TEXTURE_BYTES_PER_FRAME: u64 = 8 * 1024 * 1024;

fn env_flag(name: &str) -> bool {
    env::var_os(name).is_some_and(|value| {
        let text = value.to_string_lossy();
        !(text.is_empty() || text == "0" || text.eq_ignore_ascii_case("false"))
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "Window callbacks stay together while loading, physics and rendering live in focused state types."
)]
fn main() -> Result<(), Box<dyn Error>> {
    let asset_root = street_city::asset_root();
    let loading = LoadingAssets::start(&asset_root)?;
    let state = Rc::new(RefCell::new(DemoState::Loading(Box::new(loading))));
    let window_state = Rc::clone(&state);
    let device_state = Rc::clone(&state);
    let update_state = Rc::clone(&state);
    let render_state = Rc::clone(&state);
    let loading_renderer = Rc::new(RefCell::new(LoadingScreen::default()));
    let render_loading_renderer = Rc::clone(&loading_renderer);
    let no_post = env_flag("YUYIB_CHAR_DIAG_NO_POST");
    // Filmic is on for M1, but keep EV neutral/slightly down so white albedo
    // (dress) does not read as Sketchfab "fullbright".
    let post_process = if no_post {
        eprintln!("YUYIB_CHAR_DIAG_NO_POST: bloom/filmic/FXAA disabled");
        None
    } else {
        Some(
            ColorPostProcess::filmic()
                .with_exposure_ev(-0.25)?
                .with_bloom(BloomConfig::street_city())
                .with_color_grade(ColorGradeConfig::street_city())
                .with_fxaa(FxaaConfig::street_city()),
        )
    };

    let mut app = Application::new()
        .window(WindowConfig {
            title: "Yuyib — playable street city".to_owned(),
            mode: yuyib_platform::WindowMode::Fullscreen,
            ..Default::default()
        })
        .clear_color(ClearColor::linear(0.45, 0.58, 0.72, 1.0))
        .render_loop(RenderLoop::Continuous)
        .cursor_control(CursorControl::Released);
    if let Some(post) = post_process {
        app = app.color_post_process(post);
    }
    app.on_window_event(move |event, context| {
            if let DemoState::Playing(city) = &mut *window_state.borrow_mut() {
                if let WindowEvent::KeyboardInput { event: key, .. } = event
                    && key.state == ElementState::Pressed
                    && !key.repeat
                {
                    city.handle_look_diag_key(key.physical_key);
                }
                let result = city.playable.handle_window_event(event);
                if let Some(cursor) = result.cursor_control {
                    context.set_cursor_control(cursor);
                }
                if result.exit_requested {
                    context.request_exit();
                }
            }
        })
        .on_device_event(move |event, _context| {
            if let DemoState::Playing(city) = &mut *device_state.borrow_mut() {
                let _ = city.playable.handle_device_event(event);
            }
        })
        .on_frame(move |context| {
            let mut state = update_state.borrow_mut();
            let replacement = match &mut *state {
                DemoState::Loading(loading) => match loading.update() {
                    Ok(Some((profile, character))) => {
                        Some(PlayableCity::new(profile, character).map_or_else(
                            |error| DemoState::Failed(error.to_string()),
                            |city| DemoState::Playing(Box::new(city)),
                        ))
                    }
                    Ok(None) => None,
                    Err(error) => Some(DemoState::Failed(error)),
                },
                DemoState::Playing(city) => {
                    if city.is_gpu_ready() {
                        if let Some(cursor) = city.playable.take_initial_cursor_control() {
                            context.set_cursor_control(cursor);
                        }
                        city.step(context.frame().delta.as_secs_f32());
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
            DemoState::Loading(loading) => {
                render_loading_renderer.borrow_mut().draw(
                    frame,
                    loading.progress_fraction(),
                    false,
                );
            }
            DemoState::Playing(city) => match city.render(frame) {
                Ok(true) => {}
                Ok(false) => render_loading_renderer.borrow_mut().draw(
                    frame,
                    city.gpu_progress_fraction(),
                    false,
                ),
                Err(error) => {
                    eprintln!("Street city scene render failed: {error}");
                    render_loading_renderer.borrow_mut().draw(frame, 1.0, true);
                }
            },
            DemoState::Failed(error) => {
                eprintln!("Street city scene load failed: {error}");
                render_loading_renderer.borrow_mut().draw(frame, 1.0, true);
            }
        })
        .run()?;
    Ok(())
}

enum DemoState {
    Loading(Box<LoadingAssets>),
    Playing(Box<PlayableCity>),
    Failed(String),
}

struct LoadingAssets {
    load: Game3dPlayableLoad,
}

impl LoadingAssets {
    fn start(asset_root: &Path) -> Result<Self, Box<dyn Error>> {
        let pool = Arc::new(TaskPool::new(TaskPoolConfig::new(2, 4)?)?);
        let load = Game3dPlayableLoad::start(
            pool,
            Game3dProfileConfig::new(asset_root)
                .with_load(street_city::load_config(asset_root)?)
                .with_environment(EnvironmentPreset::street_city()?),
            street_city::map_path(asset_root),
            playable_character::character_path(asset_root),
            playable_character::CHARACTER_WALK_CLIP,
        )?;
        Ok(Self { load })
    }

    fn update(&mut self) -> Result<Option<(Game3dProfile, AnimatedCharacter3d)>, String> {
        match self.load.poll() {
            Game3dPlayableLoadStatus::Ready => self
                .load
                .take_ready()
                .map(Some)
                .map_err(|error| error.to_string()),
            Game3dPlayableLoadStatus::Failed { message } => Err(message),
            Game3dPlayableLoadStatus::Loading { .. } => Ok(None),
        }
    }

    fn progress_fraction(&self) -> f32 {
        self.load.progress_fraction()
    }
}

struct PlayableCity {
    profile: Game3dProfile,
    map_gpu: GltfSceneGpuProgress,
    playable: PlayableLoop3d,
    dynamics: DynamicsOverlay3d,
    look_diag: CharacterLookDiag,
}

/// Runtime A/B for front≠back character look (see module docs).
struct CharacterLookDiag {
    ssao: bool,
    morph: bool,
    white_light: bool,
    normal_light: LambertLighting3d,
}

impl CharacterLookDiag {
    fn new(normal_light: LambertLighting3d) -> Self {
        Self {
            ssao: !env_flag("YUYIB_CHAR_DIAG_NO_SSAO"),
            morph: !env_flag("YUYIB_CHAR_DIAG_NO_MORPH"),
            white_light: false,
            normal_light,
        }
    }

    fn log_help() {
        eprintln!(
            "character look diag: F1=SSAO  F2=morph/cloth  F3=white flat light  \
             (orbit yaw after each toggle; note front vs back)"
        );
    }

    fn log_state(&self) {
        eprintln!(
            "character look diag: ssao={} morph={} white_light={}",
            self.ssao, self.morph, self.white_light
        );
    }
}

impl PlayableCity {
    fn new(
        mut profile: Game3dProfile,
        character: AnimatedCharacter3d,
    ) -> Result<Self, Box<dyn Error>> {
        let solid_collider_id = street_city::solid_layer_id()?;
        let street_collider_id = street_city::street_layer_id()?;
        let (spawn, street_anchor) = {
            let map = profile
                .loaded()
                .ok_or("Game3dProfile map is not ready for playable city")?;
            for diagnostic in map.diagnostics() {
                eprintln!(
                    "city import {:?}: {} — {}",
                    diagnostic.severity, diagnostic.code, diagnostic.message
                );
            }
            let street_mesh = map
                .collider_layer(&street_collider_id)
                .ok_or("city semantic collision did not build the street layer")?
                .mesh();
            (
                street_city::spawn_options_for_street(street_mesh),
                street_city::street_horizontal_centroid(street_mesh),
            )
        };

        let controls = PlayerCharacterControlConfig3d {
            radius: playable_character::CHARACTER_CONTROLLER_RADIUS,
            chase_distance: CAMERA_CHASE_DISTANCE,
            near: 0.08,
            ..PlayerCharacterControlConfig3d::default()
        };
        let normal_light = street_city::character_key_light()?;
        let desc = PlayableLoopDesc3d::new(
            solid_collider_id.clone(),
            street_collider_id,
            spawn,
        )
            .with_controls(controls)
            .with_model_scale(playable_character::CHARACTER_MODEL_SCALE)
            .with_turn_speed(CHARACTER_TURN_SPEED_RADIANS_PER_SECOND)
            .with_eye_bones(
                playable_character::LEFT_EYE_BONE_NAME,
                playable_character::RIGHT_EYE_BONE_NAME,
            )
            .with_camera_height_clamp(Some((
                CAMERA_MIN_HEIGHT_ABOVE_FEET,
                CAMERA_MAX_HEIGHT_ABOVE_FEET,
            )))
            .with_camera_initial_height(CAMERA_INITIAL_HEIGHT)
            .with_character_lighting(Some(normal_light))
            .with_upload_budget(ModelUploadBudget3d {
                maximum_texture_slots: CHARACTER_TEXTURE_SLOTS_PER_FRAME,
                target_texture_bytes: CHARACTER_TEXTURE_BYTES_PER_FRAME,
                ..ModelUploadBudget3d::default()
            });

        let (mut playable, report) = PlayableLoop3d::new(&profile, character, desc)?;
        let look_diag = CharacterLookDiag::new(normal_light);
        CharacterLookDiag::log_help();
        if !look_diag.ssao {
            profile.scene_mut().set_ssao(None);
            eprintln!("YUYIB_CHAR_DIAG_NO_SSAO: SSAO off at start");
        }
        if !look_diag.morph {
            let mask = playable.character().morph_hidden_visibility();
            playable.set_character_visibility(mask);
            eprintln!("YUYIB_CHAR_DIAG_NO_MORPH: cloth morph hidden at start");
        }
        look_diag.log_state();

        let spawn_pos = playable.controller().position();
        eprintln!(
            "Street-city player spawned grounded={} at ({:.2}, {:.2}, {:.2}); \
             street anchor XZ=({:.2}, {:.2}) target Y={:.2}; spawn={report:?}",
            playable.controller().is_grounded(),
            spawn_pos.x,
            spawn_pos.y,
            spawn_pos.z,
            street_anchor.x,
            street_anchor.y,
            street_city::CITY_STREET_ELEVATION,
        );

        let solid_mesh = profile
            .collider_layer(&solid_collider_id)
            .ok_or("city semantic collision did not build the solid layer")?
            .mesh();
        let dynamics = DynamicsOverlay3d::around_spawn_with_solid_mesh(
            [spawn_pos.x, spawn_pos.y, spawn_pos.z],
            playable.controller().config().radius,
            solid_mesh,
        )?;
        Ok(Self {
            profile,
            map_gpu: GltfSceneGpuProgress::default(),
            playable,
            dynamics,
            look_diag,
        })
    }

    fn handle_look_diag_key(&mut self, key: PhysicalKey) {
        let PhysicalKey::Code(code) = key else {
            return;
        };
        match code {
            KeyCode::F1 => {
                self.look_diag.ssao = !self.look_diag.ssao;
                if self.look_diag.ssao {
                    self.profile
                        .scene_mut()
                        .set_ssao(Some(SsaoPolicy::street_city()));
                } else {
                    self.profile.scene_mut().set_ssao(None);
                }
            }
            KeyCode::F2 => {
                self.look_diag.morph = !self.look_diag.morph;
                if self.look_diag.morph {
                    self.playable.clear_character_visibility();
                } else {
                    let mask = self.playable.character().morph_hidden_visibility();
                    self.playable.set_character_visibility(mask);
                }
            }
            KeyCode::F3 => {
                self.look_diag.white_light = !self.look_diag.white_light;
                let lighting = if self.look_diag.white_light {
                    LambertLighting3d::artistic(
                        [-0.35, -1.0, -0.25],
                        [1.0, 1.0, 1.0],
                        0.0,
                        [1.0, 1.0, 1.0],
                    )
                    .unwrap_or(self.look_diag.normal_light)
                } else {
                    self.look_diag.normal_light
                };
                self.playable.character_mut().set_lighting(lighting);
            }
            _ => return,
        }
        self.look_diag.log_state();
    }

    fn is_gpu_ready(&self) -> bool {
        self.map_gpu.ready && self.playable.character_gpu_ready()
    }

    fn gpu_progress_fraction(&self) -> f32 {
        f32::midpoint(
            self.map_gpu.fraction(),
            self.playable.character_gpu_progress(),
        )
    }

    fn step(&mut self, frame_delta_seconds: f32) {
        let dynamics = &mut self.dynamics;
        self.playable
            .step(&self.profile, frame_delta_seconds, |controller, fixed_delta, mesh| {
                let position = controller.position();
                let reaction =
                    dynamics.step(fixed_delta, [position.x, position.y, position.z]);
                if (reaction[0] != 0.0 || reaction[1] != 0.0 || reaction[2] != 0.0)
                    && let Err(error) = controller.apply_external_displacement(
                        yuyib::physics::Vec3::new(reaction[0], reaction[1], reaction[2]),
                        mesh,
                    )
                {
                    eprintln!("playable Rapier reaction skipped: {error}");
                }
            });
    }

    fn render(
        &mut self,
        frame: &mut yuyib::render::RenderFrame<'_>,
    ) -> Result<bool, Box<dyn Error>> {
        let dynamics = &mut self.dynamics;
        match self.playable.prepare_and_draw(
            &mut self.profile,
            frame,
            &mut self.map_gpu,
            |frame, camera| {
                dynamics
                    .draw(frame, camera)
                    .map_err(|error| PlayableLoopError3d::Host(error.to_string()))
            },
        )? {
            PlayableDrawStatus::Loading => Ok(false),
            PlayableDrawStatus::Drawn => Ok(true),
        }
    }
}
