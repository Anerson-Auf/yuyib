//! Playable streamed street-city vertical slice with chase / first-person views.
//!
//! The city and character are imported on a shared bounded worker pool. The map
//! then uses the high-level bounded residency path while character textures are
//! published in bounded slices. WASD moves, Space jumps, V toggles first/third
//! person, mouse looks and Esc exits.
//!
//! ```text
//! cargo run -p yuyib --example cyberpunk_city_playable
//! ```

mod support;

use std::{
    cell::RefCell,
    error::Error,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

use yuyib::{
    app::{Application, RenderLoop},
    assets::{AssetLoadId, AssetLoadQueue, AssetLoadState, AssetLoadTakeError, Assets},
    character_3d::{
        CharacterController3d, CharacterControllerConfig3d, CharacterInput3d,
        CharacterModelPlacement3d, LocomotionClipSet8, LocomotionController8,
        LocomotionFacingSmoother, LocomotionState8,
    },
    game_3d::SceneBoundsResult3d,
    gltf::{
        AnimationClipIndex, AnimationPlayer, AnimationSnapshot, ImportedAsset, NodeIndex,
        sample_bind_pose,
    },
    input::{
        CharacterFollowCamera3d, FreeCameraConfig3d, FreeCameraController3d,
        ThirdPersonCameraConfig3d,
    },
    model_assets::{ModelTextureLoader, PreparedModelTextures},
    physics::Vec2,
    platform::{CursorControl, WindowConfig, winit},
    render::{BloomConfig, ClearColor, ColorGradeConfig, ColorPostProcess, FxaaConfig},
    render_3d::{
        Camera3d, DepthLoad, Game3dScene, GltfSceneColliderLayerId3d, GltfSceneGpuProgress,
        GltfSceneLoad, GltfSceneLoadStage, LoadedGltfScene, SkeletalTextureResources,
        TexturedSkeletalSceneRenderer3d,
    },
    render_texture::TextureCache,
    tasks::{TaskPool, TaskPoolConfig},
    two_d::Texture,
};

use support::{LoadingScreen, playable_character, street_city};

const RIGHT_EYE_BONE_NAME: &str = playable_character::RIGHT_EYE_BONE_NAME;
const LEFT_EYE_BONE_NAME: &str = playable_character::LEFT_EYE_BONE_NAME;
const CAMERA_MIN_HEIGHT_ABOVE_FEET: f32 = 0.85;
const CAMERA_MAX_HEIGHT_ABOVE_FEET: f32 = 1.65;
const CAMERA_CHASE_DISTANCE: f32 = 3.2;
const CAMERA_INITIAL_HEIGHT: f32 = 0.25;
const CHARACTER_TURN_SPEED_RADIANS_PER_SECOND: f32 = std::f32::consts::TAU * 1.25;
const CHARACTER_TEXTURE_SLOTS_PER_FRAME: usize = 2;
const CHARACTER_TEXTURE_BYTES_PER_FRAME: u64 = 8 * 1024 * 1024;

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
    // Filmic is on for M1, but keep EV neutral/slightly down so white albedo
    // (dress) does not read as Sketchfab "fullbright".
    let post_process = ColorPostProcess::filmic()
        .with_exposure_ev(-0.25)?
        .with_bloom(BloomConfig::street_city())
        .with_color_grade(ColorGradeConfig::street_city())
        .with_fxaa(FxaaConfig::street_city());

    Application::new()
        .window(WindowConfig {
            title: "Yuyib — playable street city".to_owned(),
            mode: yuyib_platform::WindowMode::Fullscreen,
            ..Default::default()
        })
        .clear_color(ClearColor::linear(0.45, 0.58, 0.72, 1.0))
        .color_post_process(post_process)
        .render_loop(RenderLoop::Continuous)
        .cursor_control(CursorControl::Released)
        .on_window_event(move |event, context| {
            if let DemoState::Playing(city) = &mut *window_state.borrow_mut() {
                city.input.handle_window_event(event);
                let result = city.follow_camera.handle_window_event(event);
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
                let _ = city.follow_camera.handle_device_event(event);
            }
        })
        .on_frame(move |context| {
            let mut state = update_state.borrow_mut();
            let replacement = match &mut *state {
                DemoState::Loading(loading) => match loading.update() {
                    Ok(Some((map, character))) => {
                        Some(PlayableCity::new(map, character, &asset_root).map_or_else(
                            |error| DemoState::Failed(error.to_string()),
                            |city| DemoState::Playing(Box::new(city)),
                        ))
                    }
                    Ok(None) => None,
                    Err(error) => Some(DemoState::Failed(error)),
                },
                DemoState::Playing(city) => {
                    if city.is_gpu_ready() {
                        if !city.cursor_activated {
                            context.set_cursor_control(city.follow_camera.initial_cursor_control());
                            city.cursor_activated = true;
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

struct PreparedCharacter {
    asset: ImportedAsset,
    textures: PreparedModelTextures,
}

struct LoadingAssets {
    map: GltfSceneLoad,
    character_queue: AssetLoadQueue<PreparedCharacter, String>,
    character_request: AssetLoadId,
    ready_map: Option<LoadedGltfScene>,
    ready_character: Option<PreparedCharacter>,
}

impl LoadingAssets {
    fn start(asset_root: &Path) -> Result<Self, Box<dyn Error>> {
        // One shared pool prevents two independent high-level loads from each
        // creating their own workers. Two jobs may make progress concurrently.
        let pool = Arc::new(TaskPool::new(TaskPoolConfig::new(2, 4)?)?);
        let map = GltfSceneLoad::start_on(
            street_city::map_path(asset_root),
            street_city::load_config(asset_root)?,
            Arc::clone(&pool),
        )?;
        let mut character_queue = AssetLoadQueue::new();
        let character_root = asset_root.to_owned();
        let character_request =
            character_queue.try_queue(pool.as_ref(), "animated sci-fi girl", move |progress| {
                progress.set_total_work(2);
                progress.decoding();
                let asset = playable_character::import_character(&character_root)
                    .map_err(|error| error.to_string())?;
                progress.advance(1);
                let loader =
                    ModelTextureLoader::new(&character_root).map_err(|error| error.to_string())?;
                let textures = loader
                    .prepare(&asset.model)
                    .map_err(|error| error.to_string())?;
                progress.advance(1);
                Ok(PreparedCharacter { asset, textures })
            })?;
        Ok(Self {
            map,
            character_queue,
            character_request,
            ready_map: None,
            ready_character: None,
        })
    }

    fn update(&mut self) -> Result<Option<(LoadedGltfScene, PreparedCharacter)>, String> {
        if self.ready_map.is_none() {
            match self.map.update().stage {
                GltfSceneLoadStage::Ready => match self.map.take_ready() {
                    Ok(map) => self.ready_map = Some(map),
                    Err(AssetLoadTakeError::NotReady) => {}
                    Err(error) => return Err(format!("city map could not be taken: {error}")),
                },
                GltfSceneLoadStage::Failed => {
                    return Err(self.map.failure().map_or_else(
                        || "unknown city map load failure".to_owned(),
                        ToString::to_string,
                    ));
                }
                GltfSceneLoadStage::Queued
                | GltfSceneLoadStage::Reading
                | GltfSceneLoadStage::Processing
                | GltfSceneLoadStage::Taken => {}
            }
        }

        if self.ready_character.is_none() {
            self.character_queue.poll();
            let Some(info) = self.character_queue.info(self.character_request) else {
                return Err("animated character request disappeared".to_owned());
            };
            match info.state {
                AssetLoadState::ReadyToPublish => {
                    match self.character_queue.take_ready(self.character_request) {
                        Ok(character) => self.ready_character = Some(character),
                        Err(AssetLoadTakeError::NotReady) => {}
                        Err(error) => {
                            return Err(format!("animated character could not be taken: {error}"));
                        }
                    }
                }
                AssetLoadState::Failed => {
                    return Err(self
                        .character_queue
                        .failure(self.character_request)
                        .map_or_else(
                            || "unknown animated character load failure".to_owned(),
                            ToString::to_string,
                        ));
                }
                AssetLoadState::Queued
                | AssetLoadState::Reading
                | AssetLoadState::Decoding
                | AssetLoadState::Published => {}
            }
        }

        if self.ready_map.is_none() || self.ready_character.is_none() {
            return Ok(None);
        }
        let map = self
            .ready_map
            .take()
            .expect("both CPU assets were checked as ready");
        let character = self
            .ready_character
            .take()
            .expect("both CPU assets were checked as ready");
        Ok(Some((map, character)))
    }

    #[allow(
        clippy::cast_precision_loss,
        reason = "loading UI is approximate while exact u64 counters remain authoritative"
    )]
    fn progress_fraction(&self) -> f32 {
        let map = self.map.progress();
        let character = self.character_queue.info(self.character_request);
        let completed = map
            .completed_work
            .saturating_add(character.as_ref().map_or(0, |info| info.progress.completed));
        let total = map
            .total_work
            .saturating_add(character.as_ref().map_or(0, |info| info.progress.total));
        if total == 0 {
            0.03
        } else {
            (completed.min(total) as f32 / total as f32).clamp(0.03, 0.99)
        }
    }
}

struct PlayableCity {
    map: LoadedGltfScene,
    map_renderer: Game3dScene,
    map_gpu: GltfSceneGpuProgress,
    solid_collider_id: GltfSceneColliderLayerId3d,
    follow_camera: CharacterFollowCamera3d,
    controller: CharacterController3d,
    input: PlayerInput,
    fixed_accumulator_seconds: f32,
    character: CharacterRenderState,
    locomotion: LocomotionController8,
    locomotion_clips: LocomotionClipSet8<AnimationClipIndex>,
    character_facing: LocomotionFacingSmoother,
    cursor_activated: bool,
    reported_draw_stats: bool,
}

impl PlayableCity {
    fn new(
        map: LoadedGltfScene,
        character: PreparedCharacter,
        asset_root: &PathBuf,
    ) -> Result<Self, Box<dyn Error>> {
        if character.asset.scene.skins().is_empty() {
            return Err(format!(
                "{} contains no skeleton",
                playable_character::CHARACTER_FILE
            )
            .into());
        }
        if character.asset.scene.animations().is_empty() {
            return Err(format!(
                "{} contains no walk animation",
                playable_character::CHARACTER_FILE
            )
            .into());
        }
        for diagnostic in map.diagnostics() {
            eprintln!(
                "city import {:?}: {} — {}",
                diagnostic.severity, diagnostic.code, diagnostic.message
            );
        }
        let solid_collider_id = street_city::solid_layer_id()?;
        let solid_collider = map
            .collider_layer(&solid_collider_id)
            .ok_or("city semantic collision did not build the solid layer")?;
        let street_collider_id = street_city::street_layer_id()?;
        let street_collider = map
            .collider_layer(&street_collider_id)
            .ok_or("city semantic collision did not build the street layer")?;
        let controller = CharacterController3d::spawn_on_surface_mesh_with_options(
            CharacterControllerConfig3d {
                radius: playable_character::CHARACTER_CONTROLLER_RADIUS,
                ..CharacterControllerConfig3d::default()
            },
            street_collider.mesh(),
            solid_collider.mesh(),
            street_city::spawn_options_for_street(street_collider.mesh()),
        )?;
        let radius = match map.bounds() {
            SceneBoundsResult3d::Bounds(bounds) => bounds.radius().max(20.0),
            SceneBoundsResult3d::Empty => 20.0,
        };
        let spawn = controller.position();
        let street_anchor = street_city::street_horizontal_centroid(street_collider.mesh());
        eprintln!(
            "Street-city player spawned grounded={} at ({:.2}, {:.2}, {:.2}); \
             street anchor XZ=({:.2}, {:.2}) target Y={:.2}",
            controller.is_grounded(),
            spawn.x,
            spawn.y,
            spawn.z,
            street_anchor.x,
            street_anchor.y,
            street_city::CITY_STREET_ELEVATION,
        );
        let character = CharacterRenderState::new(character)?;
        let character_facing = LocomotionFacingSmoother::new(
            Vec2::new(0.0, -1.0),
            CHARACTER_TURN_SPEED_RADIANS_PER_SECOND,
        )?;
        let initial_root = character_model_root(controller, character_facing.direction());
        let focus = character.camera_focus_world_position(initial_root)?;
        eprintln!(
            "Animated head height above street: {:.2}",
            focus[1] - (spawn.y - controller.config().radius),
        );
        let follow_camera = CharacterFollowCamera3d::looking_at(
            FreeCameraConfig3d {
                move_speed: 5.0,
                near: 0.08,
                far: radius * 8.0,
                ..FreeCameraConfig3d::default()
            },
            ThirdPersonCameraConfig3d {
                distance: CAMERA_CHASE_DISTANCE,
                target_height: 0.0,
                shoulder_offset: 0.0,
                near: 0.08,
                far: radius * 8.0,
                ..ThirdPersonCameraConfig3d::default()
            },
            [
                focus[0],
                focus[1] + CAMERA_INITIAL_HEIGHT,
                focus[2] + CAMERA_CHASE_DISTANCE,
            ],
            focus,
        )?;
        let map_renderer = street_city::create_renderer(asset_root)?;
        Ok(Self {
            map,
            map_renderer,
            map_gpu: GltfSceneGpuProgress::default(),
            solid_collider_id,
            follow_camera,
            controller,
            input: PlayerInput::default(),
            fixed_accumulator_seconds: 0.0,
            character,
            locomotion: LocomotionController8::default(),
            locomotion_clips: LocomotionClipSet8::new(AnimationClipIndex::new(0)),
            character_facing,
            cursor_activated: false,
            reported_draw_stats: false,
        })
    }

    fn is_gpu_ready(&self) -> bool {
        self.map_gpu.ready && self.character.is_ready()
    }

    fn gpu_progress_fraction(&self) -> f32 {
        f32::midpoint(self.map_gpu.fraction(), self.character.progress_fraction())
    }

    fn step(&mut self, frame_delta_seconds: f32) {
        const MAX_FIXED_STEPS_PER_FRAME: usize = 8;

        if self.follow_camera.apply_look_input().is_err()
            || !frame_delta_seconds.is_finite()
            || frame_delta_seconds < 0.0
        {
            return;
        }
        if self.input.take_view_toggle() {
            let mode = self.follow_camera.toggle_mode();
            eprintln!("Camera view: {mode:?}");
        }
        let fixed_delta = self.controller.config().fixed_delta_seconds;
        self.fixed_accumulator_seconds = (self.fixed_accumulator_seconds
            + frame_delta_seconds.min(0.125))
        .min(fixed_delta * 8.0);
        let camera_relative_movement = self.input.movement_axes();
        let movement = self
            .input
            .movement_in_camera_space(self.follow_camera.look());
        let Ok(locomotion) = self
            .locomotion
            .update(camera_relative_movement, &self.locomotion_clips)
        else {
            return;
        };
        if locomotion.state() == LocomotionState8::Moving
            && let Some(facing) = normalized_vec2(movement)
        {
            let _ = self
                .character_facing
                .update(facing, frame_delta_seconds.min(0.125));
        }
        let mut first_step = true;
        for _ in 0..MAX_FIXED_STEPS_PER_FRAME {
            if self.fixed_accumulator_seconds < fixed_delta {
                break;
            }
            let Ok(input) = CharacterInput3d::new(movement, first_step && self.input.take_jump())
            else {
                break;
            };
            first_step = false;
            let Some(collider) = self.map.collider_layer(&self.solid_collider_id) else {
                break;
            };
            if self
                .controller
                .step_on_triangle_mesh(input, collider.mesh())
                .is_err()
            {
                break;
            }
            self.fixed_accumulator_seconds -= fixed_delta;
        }

        self.character.advance(
            frame_delta_seconds,
            locomotion.state() == LocomotionState8::Moving,
        );
        let root = character_model_root(self.controller, self.character_facing.direction());
        if let Ok(focus) = self.character.camera_focus_world_position(root)
            && let Some(collider) = self.map.collider_layer(&self.solid_collider_id)
        {
            let _ = self
                .follow_camera
                .update_chase(focus, frame_delta_seconds, collider.mesh());
        }
    }

    fn render(
        &mut self,
        frame: &mut yuyib::render::RenderFrame<'_>,
    ) -> Result<bool, Box<dyn Error>> {
        let root = character_model_root(self.controller, self.character_facing.direction());
        let eye_focus = self.character.camera_focus_world_position(root)?;
        let camera = self.follow_camera.camera(eye_focus);
        *self.map_renderer.camera_mut() = camera;
        self.map_gpu = self.map.prepare_for_frame(frame, &mut self.map_renderer)?;
        self.character.prepare_for_frame(frame)?;
        if !self.is_gpu_ready() {
            return Ok(false);
        }

        let map_stats = self.map.render(frame, &mut self.map_renderer)?;
        if !self.reported_draw_stats {
            self.reported_draw_stats = true;
            eprintln!("playable draw: {}", map_stats.summary_line());
        }
        if self.follow_camera.draws_playermodel() {
            self.character.draw(frame, camera, root)?;
        }
        Ok(true)
    }
}

struct CharacterRenderState {
    asset: ImportedAsset,
    animation: Option<AnimationPlayer>,
    pose: AnimationSnapshot,
    camera_rig: CameraRigNodes,
    prepared: Option<PreparedModelTextures>,
    total_texture_slots: usize,
    completed_texture_slots: usize,
    textures: Assets<Texture>,
    gpu_textures: TextureCache,
    bindings: Option<yuyib::model_assets::ModelTextureBindings>,
    renderer: Option<TexturedSkeletalSceneRenderer3d>,
}

impl CharacterRenderState {
    fn new(character: PreparedCharacter) -> Result<Self, Box<dyn Error>> {
        let total_texture_slots = character.textures.len();
        let animation = (!character.asset.scene.animations().is_empty())
            .then(|| AnimationPlayer::new(AnimationClipIndex::new(0)));
        let pose = sample_bind_pose(&character.asset.scene)?;
        let camera_rig = CameraRigNodes::find(&character.asset)?;
        Ok(Self {
            asset: character.asset,
            animation,
            pose,
            camera_rig,
            prepared: Some(character.textures),
            total_texture_slots,
            completed_texture_slots: 0,
            textures: Assets::new(),
            gpu_textures: TextureCache::new(),
            bindings: None,
            renderer: None,
        })
    }

    fn is_ready(&self) -> bool {
        self.bindings.is_some() && self.renderer.is_some()
    }

    #[allow(
        clippy::cast_precision_loss,
        reason = "loading UI is approximate while exact slot counters remain available"
    )]
    fn progress_fraction(&self) -> f32 {
        if self.is_ready() {
            return 1.0;
        }
        if self.total_texture_slots == 0 {
            return if self.bindings.is_some() { 0.9 } else { 0.75 };
        }
        let textures = self.completed_texture_slots.min(self.total_texture_slots) as f32
            / self.total_texture_slots as f32;
        // Geometry creation is currently one atomic high-level skeletal step.
        textures * 0.9
    }

    fn prepare_for_frame(
        &mut self,
        frame: &yuyib::render::RenderFrame<'_>,
    ) -> Result<(), Box<dyn Error>> {
        if self.bindings.is_none()
            && let Some(prepared) = &mut self.prepared
        {
            let stats = prepared.upload_with_budget_for_frame(
                frame,
                &mut self.textures,
                &mut self.gpu_textures,
                CHARACTER_TEXTURE_SLOTS_PER_FRAME,
                CHARACTER_TEXTURE_BYTES_PER_FRAME,
            )?;
            self.completed_texture_slots = self
                .completed_texture_slots
                .saturating_add(stats.uploaded_slots);
            if prepared.remaining() == 0 {
                let completed = self
                    .prepared
                    .take()
                    .expect("completed preparation remains owned by character state");
                self.bindings = Some(completed.finish()?);
            }
        }
        if self.bindings.is_some() && self.renderer.is_none() {
            self.renderer = Some(
                TexturedSkeletalSceneRenderer3d::new_for_frame(
                    frame,
                    &self.asset.model,
                    &self.asset.scene,
                )?
                .with_lighting(street_city::character_key_light()?),
            );
        }
        Ok(())
    }

    fn advance(&mut self, delta_seconds: f32, moving: bool) {
        let Some(animation) = self.animation.as_mut() else {
            return;
        };
        if moving {
            animation.play();
        } else {
            animation.pause();
        }
        if let Err(error) = animation.advance(&self.asset.scene, delta_seconds) {
            eprintln!("Character animation advance failed: {error}");
            return;
        }
        match animation.snapshot(&self.asset.scene) {
            Ok(pose) => self.pose = pose,
            Err(error) => eprintln!("Character pose sampling failed: {error}"),
        }
    }

    fn camera_focus_world_position(
        &self,
        root_transform: [f32; 16],
    ) -> Result<[f32; 3], Box<dyn Error>> {
        let matrices = self.pose.world_matrices();
        let right_eye = node_position(matrices, self.camera_rig.right_eye)?;
        let left_eye = node_position(matrices, self.camera_rig.left_eye)?;
        let socket = midpoint3(right_eye, left_eye);
        let mut socket = transform_point(root_transform, socket);
        let feet_y = root_transform[13];
        socket[1] = socket[1].clamp(
            feet_y + CAMERA_MIN_HEIGHT_ABOVE_FEET,
            feet_y + CAMERA_MAX_HEIGHT_ABOVE_FEET,
        );
        Ok(socket)
    }

    fn draw(
        &self,
        frame: &mut yuyib::render::RenderFrame<'_>,
        camera: Camera3d,
        root_transform: [f32; 16],
    ) -> Result<(), Box<dyn Error>> {
        let renderer = self
            .renderer
            .as_ref()
            .ok_or("character renderer is not GPU-ready")?;
        let bindings = self
            .bindings
            .as_ref()
            .ok_or("character textures are not GPU-ready")?;
        renderer.draw_with_root_transform_and_depth_load(
            frame,
            camera,
            &self.asset.scene,
            &self.pose,
            SkeletalTextureResources {
                bindings,
                textures: &self.gpu_textures,
            },
            root_transform,
            DepthLoad::Load,
        )?;
        Ok(())
    }
}

/// Builds the playermodel root from the controller feet, locomotion facing and
/// the local uniform scale knob (`CHARACTER_MODEL_SCALE`).
fn character_model_root(controller: CharacterController3d, facing: Vec2) -> [f32; 16] {
    CharacterModelPlacement3d::from_controller(
        controller,
        facing,
        playable_character::CHARACTER_MODEL_SCALE,
    )
    .expect("playable facing and scale stay finite and positive")
    .model_to_world()
}

fn normalized_vec2(value: Vec2) -> Option<Vec2> {
    let length = value.length_squared().sqrt();
    (length.is_finite() && length > f32::EPSILON).then_some(value * length.recip())
}

fn transform_point(matrix: [f32; 16], point: [f32; 3]) -> [f32; 3] {
    [
        matrix[0].mul_add(
            point[0],
            matrix[4].mul_add(point[1], matrix[8].mul_add(point[2], matrix[12])),
        ),
        matrix[1].mul_add(
            point[0],
            matrix[5].mul_add(point[1], matrix[9].mul_add(point[2], matrix[13])),
        ),
        matrix[2].mul_add(
            point[0],
            matrix[6].mul_add(point[1], matrix[10].mul_add(point[2], matrix[14])),
        ),
    ]
}

#[derive(Clone, Copy, Debug)]
struct CameraRigNodes {
    right_eye: NodeIndex,
    left_eye: NodeIndex,
}

impl CameraRigNodes {
    fn find(asset: &ImportedAsset) -> Result<Self, Box<dyn Error>> {
        let find = |name| {
            asset
                .scene
                .nodes()
                .iter()
                .position(|node| node.name() == Some(name))
                .map(NodeIndex::new)
                .ok_or_else(|| {
                    format!(
                        "{} has no `{name}` camera rig node",
                        playable_character::CHARACTER_FILE
                    )
                })
        };
        Ok(Self {
            right_eye: find(RIGHT_EYE_BONE_NAME)?,
            left_eye: find(LEFT_EYE_BONE_NAME)?,
        })
    }
}

fn node_position(matrices: &[[f32; 16]], node: NodeIndex) -> Result<[f32; 3], Box<dyn Error>> {
    let matrix = matrices
        .get(node.get())
        .ok_or("sampled pose has no camera rig world matrix")?;
    Ok([matrix[12], matrix[13], matrix[14]])
}

fn midpoint3(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [
        left[0].midpoint(right[0]),
        left[1].midpoint(right[1]),
        left[2].midpoint(right[2]),
    ]
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "Independent keys preserve simultaneous opposing-key semantics."
)]
#[derive(Clone, Copy, Debug, Default)]
struct PlayerInput {
    forward: bool,
    backward: bool,
    left: bool,
    right: bool,
    jump_queued: bool,
    view_toggle_queued: bool,
}

impl PlayerInput {
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
                    KeyCode::KeyV if held && !event.repeat => self.view_toggle_queued = true,
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
        let axes = self.movement_axes();
        Vec2::new(
            right[0].mul_add(axes.x, forward[0] * axes.y),
            right[1].mul_add(axes.x, forward[2] * axes.y),
        )
    }

    fn movement_axes(self) -> Vec2 {
        Vec2::new(
            f32::from(i8::from(self.right) - i8::from(self.left)),
            f32::from(i8::from(self.forward) - i8::from(self.backward)),
        )
    }

    fn take_jump(&mut self) -> bool {
        let queued = self.jump_queued;
        self.jump_queued = false;
        queued
    }

    fn take_view_toggle(&mut self) -> bool {
        let queued = self.view_toggle_queued;
        self.view_toggle_queued = false;
        queued
    }
}
