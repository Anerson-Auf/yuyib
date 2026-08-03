//! Yuyib Play Mode runtime.
//!
//! Scenes are **data** (`.yscene`), not generated Rust. The Editor pins
//! `--project` + `--scene`; this binary materializes the document and opens a
//! native window. When an authored entity is named `Player`,
//! [`PlayerCharacterControls3d`] (remappable WASD / jump / sprint, code
//! defaults) drives a [`CharacterController3d`] with mouse-look follow camera
//! (plane motor fallback when the scene has no triangles).
//!
//! Optional side-by-side Rapier props overlay (mesh motor unchanged):
//!
//! ```text
//! cargo build -p yuyib-play --features physics-rapier
//! ```

#![forbid(unsafe_code)]

mod interaction_bridge;
mod trigger_signals;
mod use_interaction;

use std::{
    cell::RefCell,
    collections::HashMap,
    env, fs,
    path::{Path, PathBuf},
    process::ExitCode,
    rc::Rc,
    time::Instant,
};

use serde_json::Value;
use yuyib::{
    app::{Application, RenderLoop},
    assets::Assets,
    character_3d::{
        CharacterCollisionError3d, CharacterController3d, CharacterControllerConfig3d,
        CharacterControllerError3d, CharacterInput3d, CharacterMotor3d, CharacterMotorConfig3d,
        CharacterSpawnAnchor3d, CharacterSpawnOptions3d, CharacterSpawnReport3d,
        CharacterSpawnSurfaceSelection3d,
    },
    game_3d::{
        CollisionFlags3d, DirectionalLight3d, LocalTransform3d, Model3d, Transform3d,
        WorldTransform3d,
    },
    input::{
        CharacterFollowCamera3d, PlayerCharacterBindings3d, PlayerCharacterControlConfig3d,
        PlayerCharacterControls3d,
    },
    model::{Model, ModelHandle},
    physics::{Ray3d, TriangleMesh3d, Vec2, Vec3},
    platform::{CursorControl, WindowConfig},
    profile_3d::DynamicsOverlay3d,
    render::{BloomConfig, ClearColor, ColorGradeConfig, ColorPostProcess, FxaaConfig},
    render_3d::{
        Game3dLighting, Game3dScene, Game3dSceneConfig, Game3dShading, LambertLighting3d,
        UnboundMaterialPolicy3d,
    },
};
use yuyib_assets::CookCache;
use yuyib_authoring::SceneDocument;
use yuyib_ecs::bevy_ecs::entity::Entity;
use yuyib_game_3d::{
    DirectionalLightDraw, SceneBoundsResult3d, StaticSceneCollider3d,
    build_static_scene_collider_3d, scene_bounds_3d, set_parent_3d,
};
use yuyib_game_3d_authoring::materialize_transform_scene;
use yuyib_gameplay::{
    ActionStates, ObjectiveId, QuestBook, QuestDefinition, QuestId, QuestObjective,
};
use yuyib_gltf::{ImportOptions, import_scene_bytes_cached_at};
use yuyib_platform::winit::{
    event::{ElementState, WindowEvent},
    keyboard::{KeyCode, PhysicalKey},
};
use yuyib_scene::{SceneSelection, spawn_scene};

use interaction_bridge::PlayInteractionHost;
use trigger_signals::TriggerOverlapTracker;
use use_interaction::{
    materialize_interactables, sync_interactable_positions, try_use_interaction,
};
use yuyib_play::{
    play_log::play_log,
    scene_flags::{hide_player_visual, materialize_render_collision_flags},
    trigger_volumes::{
        EntityTriggerTracker, materialize_triggers, step_trigger_volumes, sync_trigger_positions,
    },
};

const MAX_FIXED_STEPS_PER_FRAME: u32 = 8;

fn install_quest_smoke(host: &mut PlayInteractionHost) {
    let quest_id = QuestId::new("smoke.talk");
    let objective_id = ObjectiveId::new("talk_npc");
    let mut book = QuestBook::default();
    if book
        .register(
            QuestDefinition::new(
                quest_id.clone(),
                vec![
                    QuestObjective::new(objective_id, "world.talk_npc", 1).expect("objective"),
                ],
            )
            .expect("definition"),
        )
        .is_err()
    {
        return;
    }
    let _ = book.start(&quest_id);
    host.set_quest_book(book);
    play_log(
        "yuyib-play: QuestBook smoke ready — E on TalkNpc advances smoke.talk / talk_npc",
    );
}

/// Code entry-point for Play player feel. Remap keys / retune speeds here
/// (Inspector schema can mirror this later). Defaults: W/A/S/D move, Space jump,
/// Shift sprint, V toggle view, mouse look, Esc exit.
fn player_control_config() -> PlayerCharacterControlConfig3d {
    PlayerCharacterControlConfig3d {
        // Example remap (keep defaults unless you need a custom layout):
        // bindings: PlayerCharacterBindings3d {
        //     forward: winit::keyboard::KeyCode::ArrowUp,
        //     ..PlayerCharacterBindings3d::default()
        // },
        bindings: PlayerCharacterBindings3d::default(),
        ..PlayerCharacterControlConfig3d::default()
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("yuyib-play: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = env::args().skip(1).collect();
    let project = arg_value(&args, "--project")
        .map(PathBuf::from)
        .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let scene_rel = arg_value(&args, "--scene").ok_or_else(|| {
        "missing --scene <project-relative.yscene> (Editor Play always pins the open scene)"
            .to_owned()
    })?;
    let scene_path = project.join(&scene_rel);
    if !scene_path.is_file() {
        return Err(format!(
            "scene file not found: {} (project {})",
            scene_path.display(),
            project.display()
        ));
    }

    let history_revision = arg_value(&args, "--scene-revision").map(str::to_owned);
    let expected_file_revision = arg_value(&args, "--scene-file-revision").map(str::to_owned);
    let apply_report = arg_value(&args, "--apply-report").map(PathBuf::from);

    let bytes = fs::read(&scene_path).map_err(|error| error.to_string())?;
    if let Some(expected) = expected_file_revision.as_deref() {
        validate_scene_file_revision(&bytes, expected)?;
    }
    if let Some(revision) = history_revision.as_deref() {
        eprintln!(
            "yuyib-play: pinned scene `{scene_rel}` history_revision={revision} file_ok={}",
            expected_file_revision.is_some()
        );
    }

    let json = String::from_utf8(bytes).map_err(|error| error.to_string())?;
    let document = SceneDocument::from_json(&json).map_err(|error| error.to_string())?;
    let materialized = materialize_transform_scene(&document).map_err(|error| error.to_string())?;

    let mut models = Assets::new();
    let proxy = models.insert(Model::cube(0.7).map_err(|error| error.to_string())?);
    let mut model_cache = HashMap::<String, ModelHandle>::new();
    let mut world = materialized.world;

    for (guid, entity) in &materialized.entities {
        let Some(record) = document
            .entities
            .iter()
            .find(|entity_record| &entity_record.guid == guid)
        else {
            continue;
        };
        let model3d = record
            .components
            .iter()
            .find(|component| component.schema().as_str() == "yuyib.model3d");
        if let Some(component) = model3d {
            let payload = component.payload();
            let mesh = payload
                .get("mesh")
                .and_then(Value::as_u64)
                .map(|v| v as usize);
            if mesh.is_none() && payload_is_gltf(payload) {
                let attached = attach_gltf_hierarchy(
                    &project,
                    &mut world,
                    *entity,
                    payload,
                    &mut models,
                    &mut model_cache,
                )?;
                if !attached {
                    let model_ref = payload
                        .get("model")
                        .and_then(Value::as_str)
                        .unwrap_or("<missing>");
                    let name = record.name.as_deref().unwrap_or("<unnamed>");
                    eprintln!(
                        "yuyib-play: glTF attach skipped for `{name}` (`{model_ref}`) — proxy cube"
                    );
                    world.entity_mut(*entity).insert(Model3d::new(proxy));
                }
            } else {
                let handle = resolve_model(&project, payload, &mut models, &mut model_cache, proxy)
                    .unwrap_or(proxy);
                let mut model = Model3d::new(handle);
                if let Some(visible) = payload.get("visible").and_then(Value::as_bool) {
                    model = model.with_visible(visible);
                }
                if let Some(mesh) = mesh {
                    model = model.with_mesh(mesh);
                }
                if let Some(order) = payload.get("render_order").and_then(Value::as_i64) {
                    model = model.with_render_order(order as i32);
                }
                world.entity_mut(*entity).insert(model);
            }
        }

        if let Some(light) = record
            .components
            .iter()
            .find(|component| component.schema().as_str() == "yuyib.directional-light3d")
        {
            match directional_from_payload(light.payload())? {
                Some(directional) => {
                    world.entity_mut(*entity).insert(directional);
                }
                None => {
                    world.entity_mut(*entity).remove::<DirectionalLight3d>();
                }
            }
        }
    }

    if materialized.entities.is_empty() {
        let _cube = world
            .spawn((Model3d::new(proxy), Transform3d::default()))
            .id();
    }

    let _ = yuyib_game_3d::propagate_world_transforms(&mut world);
    apply_world_light_directions(&mut world, &document, &materialized.entities);
    materialize_interactables(&document, &mut world, &materialized.entities);
    materialize_triggers(&document, &mut world, &materialized.entities);
    materialize_render_collision_flags(&document, &mut world, &materialized.entities);

    let player_config = player_control_config();
    let player_entity = find_player_entity(&document, &materialized.entities);
    if let Some(player) = player_entity {
        hide_player_visual(&mut world, player);
    }
    let player_body = build_player_body(&mut world, &models, player_entity, &player_config);

    let controls = PlayerCharacterControls3d::new(player_config)
        .map_err(|error| error.to_string())?;
    let follow_camera = match &player_body {
        Some(body) => {
            let position = body.position();
            let focus = [
                position.x,
                position.y + player_config.eye_height,
                position.z,
            ];
            let (yaw_sin, yaw_cos) = 0.7_f32.sin_cos();
            let (pitch_sin, pitch_cos) = 0.28_f32.sin_cos();
            let distance = player_config.chase_distance;
            let chase_eye = [
                focus[0] + distance * yaw_sin * pitch_cos,
                focus[1] + distance * pitch_sin,
                focus[2] + distance * yaw_cos * pitch_cos,
            ];
            Some(
                CharacterFollowCamera3d::looking_at(
                    player_config.look_config(),
                    player_config.chase_config(),
                    chase_eye,
                    focus,
                )
                .map_err(|error| error.to_string())?,
            )
        }
        None => None,
    };

    let mut orbit_fallback = OrbitFallback::default();
    if player_body.is_none() {
        if let Ok(SceneBoundsResult3d::Bounds(bounds)) = scene_bounds_3d(&mut world, &models) {
            orbit_fallback.target = bounds.centre();
            orbit_fallback.radius = (bounds.radius() * 2.6).clamp(2.0, 5_000.0);
        }
    }

    let models = Rc::new(models);
    let entity_map = Rc::new(materialized.entities);
    let world = Rc::new(RefCell::new(world));
    let scene = Rc::new(RefCell::new(
        Game3dScene::new(
            &project,
            Game3dSceneConfig::default()
                .with_shading(Game3dShading::Pbr)
                .with_lighting(play_scene_lighting())
                // Imported maps often have unbound primitives; failing the whole
                // frame leaves a clear viewport with only the Player cube visible.
                .with_unbound_material_policy(UnboundMaterialPolicy3d::DebugMagenta),
        )
        .map_err(|error| error.to_string())?,
    ));
    apply_runtime_camera(
        &mut scene.borrow_mut(),
        follow_camera.as_ref(),
        &orbit_fallback,
        player_body.as_ref().map(|body| {
            let position = body.position();
            [
                position.x,
                position.y + player_config.eye_height,
                position.z,
            ]
        }),
    );

    let title = format!("Yuyib Play — {scene_rel}");
    let dynamics = build_dynamics_overlay(player_body.as_ref(), player_config.radius);
    let play = Rc::new(RefCell::new(PlayRuntime {
        player_entity,
        body: player_body,
        controls,
        follow_camera,
        orbit_fallback,
        applied_move_speed: player_config.move_speed,
        fixed_accumulator_seconds: 0.0,
        last_frame: Instant::now(),
        plane_chase_mesh: ground_chase_mesh()?,
        entity_map: Rc::clone(&entity_map),
        interaction: PlayInteractionHost::default(),
        actions: ActionStates::default(),
        pending_use: false,
        trigger_tracker: EntityTriggerTracker::default(),
        dynamics,
        rapier_trigger_tracker: TriggerOverlapTracker::default(),
    }));
    {
        let mut runtime = play.borrow_mut();
        runtime.interaction.set_proxy_model(proxy);
        install_quest_smoke(&mut runtime.interaction);
        register_rapier_trigger_sensors(&mut runtime.dynamics, &mut world.borrow_mut());
        if runtime.dynamics.is_active() {
            play_log(
                "yuyib-play: Rapier dynamics overlay active (props + soft reaction; mesh motor unchanged)",
            );
        }
    }

    let event_play = Rc::clone(&play);
    let device_play = Rc::clone(&play);
    let render_play = Rc::clone(&play);
    let render_world = Rc::clone(&world);
    let render_scene = Rc::clone(&scene);
    let render_models = Rc::clone(&models);
    let step_world = Rc::clone(&world);

    Application::new()
        .window(WindowConfig {
            title,
            width: 1280,
            height: 720,
            ..Default::default()
        })
        .render_loop(RenderLoop::Continuous)
        .clear_color(ClearColor::linear(0.012, 0.018, 0.032, 1.0))
        .cursor_control(if player_config.lock_cursor {
            CursorControl::LockedHidden
        } else {
            CursorControl::Released
        })
        // Match editor viewport: no filmic lift. Positive exposure made dark PBR
        // scenes look like flat full-bright BaseColor and hid authored lights.
        .color_post_process(
            ColorPostProcess::filmic()
                .with_exposure_ev(-0.6)
                .map_err(|error| error.to_string())?
                .with_bloom(BloomConfig::street_city())
                .with_color_grade(ColorGradeConfig::street_city())
                .with_fxaa(FxaaConfig::street_city()),
        )
        .on_window_event(move |event, context| {
            let mut runtime = event_play.borrow_mut();
            runtime.controls.handle_window_event(event);
            if let WindowEvent::KeyboardInput { event: key_event, .. } = event {
                if key_event.state == ElementState::Pressed
                    && !key_event.repeat
                    && matches!(
                        key_event.physical_key,
                        PhysicalKey::Code(KeyCode::KeyE)
                    )
                {
                    runtime.pending_use = true;
                }
            }
            if let Some(follow) = runtime.follow_camera.as_mut() {
                let result = follow.handle_window_event(event);
                if let Some(cursor) = result.cursor_control {
                    context.set_cursor_control(cursor);
                }
                if result.exit_requested {
                    context.request_exit();
                }
            }
        })
        .on_device_event(move |event, _context| {
            if let Some(follow) = device_play.borrow_mut().follow_camera.as_mut() {
                let _ = follow.handle_device_event(event);
            }
        })
        .on_render(move |frame| {
            {
                let mut runtime = render_play.borrow_mut();
                let frame_delta = runtime.last_frame.elapsed().as_secs_f32();
                runtime.last_frame = Instant::now();
                runtime.step(frame_delta, &mut step_world.borrow_mut());
                apply_runtime_camera(
                    &mut render_scene.borrow_mut(),
                    runtime.follow_camera.as_ref(),
                    &runtime.orbit_fallback,
                    runtime.camera_focus(),
                );
            }
            if let Err(error) = render_scene.borrow_mut().render(
                frame,
                &mut render_world.borrow_mut(),
                render_models.as_ref(),
            ) {
                eprintln!("yuyib-play: render failed: {error}");
            }
            let camera = *render_scene.borrow_mut().camera_mut();
            if let Err(error) = render_play.borrow_mut().dynamics.draw(frame, camera) {
                eprintln!("yuyib-play: dynamics overlay draw failed: {error}");
            }
        })
        .run()
        .map_err(|error| error.to_string())?;

    if let Some(report_path) = apply_report {
        write_apply_report(
            &report_path,
            &scene_rel,
            history_revision.as_deref(),
            expected_file_revision.as_deref(),
            &entity_map,
            &world.borrow(),
        )?;
    }
    Ok(())
}

fn write_apply_report(
    path: &Path,
    scene_path: &str,
    history_revision: Option<&str>,
    file_revision: Option<&str>,
    entities: &std::collections::BTreeMap<yuyib_authoring::EntityGuid, Entity>,
    world: &yuyib_ecs::prelude::World,
) -> Result<(), String> {
    let changes: Vec<Value> = entities
        .iter()
        .filter_map(|(guid, &entity)| {
            if let Some(local) = world.get::<LocalTransform3d>(entity) {
                return Some(serde_json::json!({
                    "entity": guid.to_string(),
                    "component": "yuyib.local-transform3d",
                    "fields": {
                        "translation.x": local.translation[0],
                        "translation.y": local.translation[1],
                        "translation.z": local.translation[2],
                        "rotation.x": local.rotation[0],
                        "rotation.y": local.rotation[1],
                        "rotation.z": local.rotation[2],
                        "rotation.w": local.rotation[3],
                        "scale.x": local.scale[0],
                        "scale.y": local.scale[1],
                        "scale.z": local.scale[2],
                    }
                }));
            }
            let transform = world.get::<Transform3d>(entity)?;
            Some(serde_json::json!({
                "entity": guid.to_string(),
                "component": "yuyib.transform3d",
                "fields": {
                    "translation.x": transform.translation[0],
                    "translation.y": transform.translation[1],
                    "translation.z": transform.translation[2],
                    "rotation.x": transform.rotation[0],
                    "rotation.y": transform.rotation[1],
                    "rotation.z": transform.rotation[2],
                    "rotation.w": transform.rotation[3],
                    "scale.x": transform.scale[0],
                    "scale.y": transform.scale[1],
                    "scale.z": transform.scale[2],
                }
            }))
        })
        .collect();
    let report = serde_json::json!({
        "schema": "yuyib.play-apply-report@1",
        "scene_path": scene_path,
        "history_revision": history_revision.and_then(|value| value.parse::<u64>().ok()),
        "file_revision": file_revision,
        "changes": changes,
    });
    let parent = path.parent().ok_or_else(|| "apply report path has no parent".to_owned())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let temporary = path.with_extension("json.tmp");
    fs::write(
        &temporary,
        serde_json::to_vec_pretty(&report).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    fs::rename(&temporary, path).map_err(|error| error.to_string())
}

#[derive(Clone, Copy, Debug)]
struct OrbitFallback {
    yaw: f32,
    pitch: f32,
    radius: f32,
    target: [f32; 3],
}

impl Default for OrbitFallback {
    fn default() -> Self {
        Self {
            yaw: 0.55,
            pitch: 0.35,
            radius: 4.0,
            target: [0.0, 0.0, 0.0],
        }
    }
}

struct PlayRuntime {
    player_entity: Option<Entity>,
    body: Option<PlayerBody>,
    controls: PlayerCharacterControls3d,
    follow_camera: Option<CharacterFollowCamera3d>,
    orbit_fallback: OrbitFallback,
    applied_move_speed: f32,
    fixed_accumulator_seconds: f32,
    last_frame: Instant,
    plane_chase_mesh: TriangleMesh3d,
    /// Stable authored GUID → runtime Entity (materialization map).
    entity_map: Rc<std::collections::BTreeMap<yuyib_authoring::EntityGuid, Entity>>,
    /// Script interaction pending queue + frame signal drain.
    interaction: PlayInteractionHost,
    /// Semantic action states for `game.use` (Interactable raycast).
    actions: ActionStates,
    /// Edge-triggered `KeyE` → attempt use this frame.
    pending_use: bool,
    /// Authoring sphere triggers → `trigger.*` bridge signals (no Rapier).
    trigger_tracker: EntityTriggerTracker,
    /// Side-by-side Rapier props (+ optional sensors when feature-enabled).
    dynamics: DynamicsOverlay3d,
    /// Rapier sensor pair → `trigger.*` intents (used when overlay is active).
    rapier_trigger_tracker: TriggerOverlapTracker,
}

enum PlayerBody {
    Mesh {
        controller: CharacterController3d,
        collider: StaticSceneCollider3d,
    },
    Plane(CharacterMotor3d),
}

impl PlayerBody {
    fn position(&self) -> Vec3 {
        match self {
            Self::Mesh { controller, .. } => controller.position(),
            Self::Plane(motor) => motor.position(),
        }
    }

    fn fixed_delta_seconds(&self) -> f32 {
        match self {
            Self::Mesh { controller, .. } => controller.config().fixed_delta_seconds,
            Self::Plane(motor) => motor.config().fixed_delta_seconds,
        }
    }

    fn set_move_speed(&mut self, move_speed: f32) -> Result<(), String> {
        match self {
            Self::Mesh { controller, .. } => {
                let mut config = controller.config();
                config.move_speed = move_speed;
                controller.set_config(config).map_err(|error| error.to_string())
            }
            Self::Plane(motor) => {
                let mut config = motor.config();
                config.move_speed = move_speed;
                motor.set_config(config).map_err(|error| error.to_string())
            }
        }
    }

    fn step(&mut self, input: CharacterInput3d) -> Result<(), String> {
        match self {
            Self::Mesh {
                controller,
                collider,
            } => controller
                .step_on_triangle_mesh(input, collider.mesh())
                .map(|_| ())
                .map_err(|error| error.to_string()),
            Self::Plane(motor) => motor
                .step(input)
                .map(|_| ())
                .map_err(|error| error.to_string()),
        }
    }

    fn apply_external_displacement(&mut self, displacement: Vec3) -> Result<(), String> {
        match self {
            Self::Mesh {
                controller,
                collider,
            } => controller
                .apply_external_displacement(displacement, collider.mesh())
                .map_err(|error| error.to_string()),
            Self::Plane(_) => Ok(()),
        }
    }
}

impl PlayRuntime {
    /// Attaches a gameplay [`yuyib_gameplay::QuestBook`] for signal consumption.
    #[allow(dead_code)]
    fn set_quest_book(&mut self, book: yuyib_gameplay::QuestBook) {
        self.interaction.set_quest_book(book);
    }

    /// Enqueues a script intent (behavior modules / tests).
    #[allow(dead_code)]
    fn enqueue_interaction(&mut self, intent: yuyib_scene_interaction::SceneInteractionIntent) {
        self.interaction.enqueue(intent);
    }

    fn flush_interactions(&mut self, world: &mut yuyib_ecs::prelude::World) {
        match self.interaction.flush(world, &self.entity_map) {
            Ok(batch) if batch.submitted > 0 => {
                let _ = yuyib_game_3d::propagate_world_transforms(world);
                if batch.applied > 0 || !batch.signals.is_empty() {
                    play_log(format!(
                        "yuyib-play: interaction flush submitted={} applied={} signals={}",
                        batch.submitted,
                        batch.applied,
                        batch.signals.len()
                    ));
                }
            }
            Ok(_) => {}
            Err(error) => play_log(format!("yuyib-play: interaction flush failed: {error}")),
        }
        self.interaction.advance_signals();
        self.interaction.consume_signals();
    }

    fn step(&mut self, frame_delta_seconds: f32, world: &mut yuyib_ecs::prelude::World) {
        sync_interactable_positions(world);
        if self.pending_use {
            self.pending_use = false;
            if let (Some(actor), Some(follow)) = (self.player_entity, self.follow_camera.as_ref()) {
                let camera = follow.look().camera();
                if let Some(intent) =
                    try_use_interaction(world, &mut self.actions, actor, camera.position, camera.target)
                {
                    self.interaction.enqueue(intent);
                }
            }
        }
        self.flush_interactions(world);
        if let Some(follow) = self.follow_camera.as_mut() {
            if follow.apply_look_input().is_err() {
                return;
            }
            if self.controls.take_view_toggle() {
                let mode = follow.toggle_mode();
                eprintln!("yuyib-play: camera view {mode:?}");
            }
        } else {
            return;
        }

        let Some(entity) = self.player_entity else {
            return;
        };
        if !(frame_delta_seconds.is_finite() && frame_delta_seconds >= 0.0) {
            return;
        }

        let desired_speed = self.controls.effective_move_speed();
        if let Some(body) = self.body.as_mut() {
            if (desired_speed - self.applied_move_speed).abs() > 1.0e-4
                && body.set_move_speed(desired_speed).is_ok()
            {
                self.applied_move_speed = desired_speed;
            }

            let fixed_delta = body.fixed_delta_seconds();
            self.fixed_accumulator_seconds = (self.fixed_accumulator_seconds
                + frame_delta_seconds.min(0.125))
            .min(fixed_delta * MAX_FIXED_STEPS_PER_FRAME as f32);

            let movement = self
                .follow_camera
                .as_ref()
                .map(|follow| self.controls.movement_in_camera_space(follow.look()))
                .unwrap_or_default();
            let mut first_step = true;
            for _ in 0..MAX_FIXED_STEPS_PER_FRAME {
                if self.fixed_accumulator_seconds < fixed_delta {
                    break;
                }
                let jump = first_step && self.controls.take_jump();
                first_step = false;
                let Ok(input) = CharacterInput3d::new(movement, jump) else {
                    break;
                };
                if body.step(input).is_err() {
                    break;
                }
                let position = body.position();
                let reaction = self.dynamics.step(
                    fixed_delta,
                    [position.x, position.y, position.z],
                );
                if reaction[0] != 0.0 || reaction[1] != 0.0 || reaction[2] != 0.0 {
                    let _ = body.apply_external_displacement(Vec3::new(
                        reaction[0],
                        reaction[1],
                        reaction[2],
                    ));
                }
                self.fixed_accumulator_seconds -= fixed_delta;
            }

            let position = body.position();
            sync_player_visual_transform(world, entity, position);
            sync_trigger_positions(world);
            if self.dynamics.is_active() {
                let pairs = self.dynamics.trigger_overlap_pairs();
                let ids = self.dynamics.trigger_ids().clone();
                for intent in self.rapier_trigger_tracker.diff_to_intents(&pairs, &ids) {
                    self.interaction.enqueue(intent);
                }
            } else {
                for intent in
                    step_trigger_volumes(world, entity, position, &mut self.trigger_tracker)
                {
                    self.interaction.enqueue(intent);
                }
            }
            self.flush_interactions(world);
        }

        let Some(focus) = self.camera_focus() else {
            return;
        };
        let delta = frame_delta_seconds.min(0.125);
        let mut follow = self.follow_camera.take();
        if let Some(follow_camera) = follow.as_mut() {
            // Chase boom must not resolve_sphere against the full static map:
            // inside a room the probe sits in solid volume and ejects the camera
            // into the sky, which looks like “no map / eternal fall”.
            // Player locomotion still uses the real mesh collider.
            let _ = follow_camera.update_chase(focus, delta, &self.plane_chase_mesh);
        }
        self.follow_camera = follow;
    }

    fn camera_focus(&self) -> Option<[f32; 3]> {
        let body = self.body.as_ref()?;
        let position = body.position();
        Some([
            position.x,
            position.y + self.controls.config().eye_height,
            position.z,
        ])
    }
}

fn apply_runtime_camera(
    scene: &mut Game3dScene,
    follow: Option<&CharacterFollowCamera3d>,
    orbit: &OrbitFallback,
    focus: Option<[f32; 3]>,
) {
    if let (Some(follow), Some(focus)) = (follow, focus) {
        *scene.camera_mut() = follow.camera(focus);
        return;
    }
    let (yaw_sin, yaw_cos) = orbit.yaw.sin_cos();
    let (pitch_sin, pitch_cos) = orbit.pitch.sin_cos();
    let camera = scene.camera_mut();
    camera.target = orbit.target;
    camera.position = [
        orbit.target[0] + orbit.radius * yaw_sin * pitch_cos,
        orbit.target[1] + orbit.radius * pitch_sin,
        orbit.target[2] + orbit.radius * yaw_cos * pitch_cos,
    ];
    camera.up = [0.0, 1.0, 0.0];
    camera.near = (orbit.radius * 0.0005).max(0.05);
    camera.far = (orbit.radius * 40.0).max(camera.near * 200.0);
}

fn build_player_body(
    world: &mut yuyib_ecs::prelude::World,
    models: &Assets<Model>,
    player_entity: Option<Entity>,
    player: &PlayerCharacterControlConfig3d,
) -> Option<PlayerBody> {
    let entity = player_entity?;
    let translation = world
        .get::<Transform3d>(entity)
        .map(|transform| transform.translation)
        .unwrap_or([0.0, 1.0, 0.0]);
    let spawn = Vec3::new(translation[0], translation[1], translation[2]);
    let controller_config = CharacterControllerConfig3d {
        move_speed: player.move_speed,
        jump_speed: player.jump_speed,
        radius: player.radius,
        gravity_y: player.gravity_y,
        ..CharacterControllerConfig3d::default()
    };

    let previous_collision = world.get::<CollisionFlags3d>(entity).cloned();
    world
        .entity_mut(entity)
        .insert(CollisionFlags3d::nocollide());
    let collider = build_static_scene_collider_3d(world, models);
    match previous_collision {
        Some(flags) => {
            world.entity_mut(entity).insert(flags);
        }
        None => {
            world.entity_mut(entity).remove::<CollisionFlags3d>();
        }
    }

    match collider {
        Ok(collider) if collider.triangle_count() > 0 => {
            match spawn_player_on_mesh(controller_config, &collider, spawn) {
                Ok(controller) => {
                    eprintln!(
                        "yuyib-play: Player ready ({} tris, {} draws) at ({:.2}, {:.2}, {:.2}) — WASD / mouse / Space / Shift / V",
                        collider.triangle_count(),
                        collider.source_draw_count(),
                        controller.position().x,
                        controller.position().y,
                        controller.position().z
                    );
                    // Keep the authored cube under the controller before the first frame.
                    sync_player_visual_transform(world, entity, controller.position());
                    Some(PlayerBody::Mesh {
                        controller,
                        collider,
                    })
                }
                Err(error) => {
                    eprintln!(
                        "yuyib-play: mesh spawn failed ({error}); falling back to plane motor"
                    );
                    plane_motor_fallback(spawn, player)
                }
            }
        }
        Ok(_) => {
            eprintln!("yuyib-play: empty scene collider — plane motor fallback");
            plane_motor_fallback(spawn, player)
        }
        Err(error) => {
            eprintln!("yuyib-play: collider build failed ({error}) — plane motor fallback");
            plane_motor_fallback(spawn, player)
        }
    }
}

/// Writes the controller pose into every transform flavor the renderer may read.
///
/// glTF parenting promotes authored roots to [`LocalTransform3d`]. Each frame
/// `propagate_world_transforms` rebuilds [`Transform3d`] / [`WorldTransform3d`]
/// from that local — so updating only `Transform3d` leaves the Player cube stuck
/// at the authored spawn while the chase camera follows the physics body.
fn sync_player_visual_transform(
    world: &mut yuyib_ecs::prelude::World,
    entity: Entity,
    position: Vec3,
) {
    let translation = [position.x, position.y, position.z];
    if let Some(mut local) = world.get_mut::<LocalTransform3d>(entity) {
        local.translation = translation;
    }
    if let Some(mut transform) = world.get_mut::<Transform3d>(entity) {
        transform.translation = translation;
    } else if world.get::<LocalTransform3d>(entity).is_none() {
        world
            .entity_mut(entity)
            .insert(Transform3d::from_translation(translation));
    }
    // Drop stale world snapshot so the next propagate/extract cannot prefer an
    // old matrix over the pose we just wrote.
    world.entity_mut(entity).remove::<WorldTransform3d>();
}

/// Places the player on a walkable floor near the authored transform.
///
/// Prefer a downward raycast and a tight local floor search. Global outdoor
/// lowest-surface search teleports the chase camera into empty map regions and
/// looks like “no location / eternal fall”.
///
/// Indoor authored spawns must not snap to a roof: casting from high above the
/// player hits the roof **top** (upward normal) before the interior floor.
fn spawn_player_on_mesh(
    config: CharacterControllerConfig3d,
    collider: &StaticSceneCollider3d,
    spawn: Vec3,
) -> Result<CharacterController3d, CharacterControllerError3d> {
    let mesh = collider.mesh();
    if let Some(controller) = try_spawn_by_downward_raycast(config, mesh, spawn)? {
        eprintln!(
            "yuyib-play: spawn via downward raycast near authored ({:.2}, {:.2}, {:.2})",
            spawn.x, spawn.y, spawn.z
        );
        return Ok(controller);
    }

    let preferred_xz = Vec2::new(spawn.x, spawn.z);
    // Open rooms often have no collision ceiling — do not require one.
    let local = CharacterSpawnOptions3d {
        require_ceiling: false,
        ..CharacterSpawnOptions3d::default()
    }
    .with_anchor(CharacterSpawnAnchor3d::PreferredXz(preferred_xz))
    .with_maximum_horizontal_distance(12.0)
    .with_surface_selection(CharacterSpawnSurfaceSelection3d::ClosestToElevation(spawn.y));
    match CharacterController3d::spawn_in_triangle_mesh_with_options(config, mesh, local) {
        Ok(controller) => {
            eprintln!(
                "yuyib-play: spawn via local floor search near authored ({:.2}, {:.2}, {:.2})",
                spawn.x, spawn.y, spawn.z
            );
            return Ok(controller);
        }
        Err(CharacterControllerError3d::NoPlayableSpawn(report)) => {
            eprintln!(
                "yuyib-play: local floor spawn missed near ({:.2}, {:.2}, {:.2}) \
                 (ranked={}, rejects={:?})",
                spawn.x,
                spawn.y,
                spawn.z,
                report.ranked_candidate_count,
                report.reject_counts
            );
        }
        Err(error) => return Err(error),
    }

    Err(CharacterControllerError3d::NoPlayableSpawn(
        CharacterSpawnReport3d {
            anchor: preferred_xz,
            surface_triangle_count: u32::try_from(mesh.triangles().len()).unwrap_or(u32::MAX),
            ranked_candidate_count: 0,
            reject_counts: Default::default(),
            selected: None,
        },
    ))
}

/// Max how far above the authored spawn a floor hit may land.
///
/// Roof tops over indoor markers sit well above this; interior floors sit at or
/// slightly below the authored feet.
const SPAWN_FLOOR_MAX_ABOVE_AUTHORED: f32 = 0.75;

#[must_use]
fn spawn_floor_hit_acceptable(hit_y: f32, authored_y: f32) -> bool {
    hit_y.is_finite()
        && authored_y.is_finite()
        && hit_y <= authored_y + SPAWN_FLOOR_MAX_ABOVE_AUTHORED
}

fn try_spawn_by_downward_raycast(
    config: CharacterControllerConfig3d,
    mesh: &TriangleMesh3d,
    spawn: Vec3,
) -> Result<Option<CharacterController3d>, CharacterControllerError3d> {
    // Probe from near authored feet first. A high outdoor cast (spawn.y+6) hits
    // roof tops that cover indoor rooms and teleports the player onto the roof.
    let origins = [
        Vec3::new(spawn.x, spawn.y + 0.35, spawn.z),
        spawn,
        Vec3::new(spawn.x, spawn.y + 1.5, spawn.z),
        Vec3::new(spawn.x, spawn.y + 6.0, spawn.z),
    ];
    let mut best: Option<(f32, CharacterController3d)> = None;
    for origin in origins {
        let Ok(ray) = Ray3d::new(origin, Vec3::new(0.0, -1.0, 0.0)) else {
            continue;
        };
        let hit = match mesh.raycast(ray, 48.0) {
            Ok(hit) => hit,
            Err(error) => {
                return Err(CharacterControllerError3d::Physics(error));
            }
        };
        let Some(hit) = hit else {
            continue;
        };
        // Prefer upward-facing support (walkable floor), not ceiling underside.
        if hit.normal.y < 0.35 {
            continue;
        }
        if !spawn_floor_hit_acceptable(hit.position.y, spawn.y) {
            continue;
        }
        let position = Vec3::new(
            hit.position.x,
            hit.position.y + config.radius + 0.02,
            hit.position.z,
        );
        let clearance = mesh
            .resolve_sphere(position, config.radius, config.collision_iterations)
            .map_err(CharacterCollisionError3d::from)
            .map_err(CharacterControllerError3d::Collision)?;
        let controller = if clearance.contacts != 0 {
            let mut controller = CharacterController3d::new(config, clearance.position)?;
            controller.place_on_triangle_mesh(mesh)?;
            controller
        } else {
            CharacterController3d::new(config, position)?
        };
        let elevation_error = (controller.position().y - spawn.y).abs();
        match &best {
            Some((best_error, _)) if *best_error <= elevation_error => {}
            _ => best = Some((elevation_error, controller)),
        }
    }
    Ok(best.map(|(_, controller)| controller))
}

fn build_dynamics_overlay(
    body: Option<&PlayerBody>,
    character_radius: f32,
) -> DynamicsOverlay3d {
    let (spawn, solid) = match body {
        Some(PlayerBody::Mesh {
            controller,
            collider,
        }) => {
            let position = controller.position();
            (
                [position.x, position.y, position.z],
                Some(collider.mesh()),
            )
        }
        Some(PlayerBody::Plane(motor)) => {
            let position = motor.position();
            ([position.x, position.y, position.z], None)
        }
        None => ([0.0, 1.0, 0.0], None),
    };
    let radius = character_radius.max(0.05);
    let result = match solid {
        Some(mesh) => DynamicsOverlay3d::around_spawn_with_solid_mesh(spawn, radius, mesh),
        None => DynamicsOverlay3d::around_spawn(spawn, radius),
    };
    match result {
        Ok(overlay) => overlay,
        Err(error) => {
            play_log(format!(
                "yuyib-play: dynamics overlay unavailable: {error}"
            ));
            DynamicsOverlay3d::around_spawn(spawn, radius).unwrap_or_else(|_| {
                DynamicsOverlay3d::around_spawn([0.0, 0.0, 0.0], 0.3).expect("stub overlay")
            })
        }
    }
}

fn register_rapier_trigger_sensors(
    dynamics: &mut DynamicsOverlay3d,
    world: &mut yuyib_ecs::prelude::World,
) {
    if !dynamics.is_active() {
        return;
    }
    use yuyib_gameplay::Trigger;
    use yuyib_physics::{Position3d, SphereCollider3d};

    let mut query = world.query::<(&Trigger, &Position3d, &SphereCollider3d)>();
    let mut count = 0_usize;
    for (trigger, position, sphere) in query.iter(world) {
        if !trigger.enabled {
            continue;
        }
        let center = position.get();
        if let Err(error) = dynamics.register_trigger_sphere(
            [center.x, center.y, center.z],
            sphere.sphere().radius(),
            trigger.trigger.as_str(),
        ) {
            play_log(format!(
                "yuyib-play: Rapier trigger `{}` skipped: {error}",
                trigger.trigger.as_str()
            ));
            continue;
        }
        count += 1;
    }
    if count > 0 {
        play_log(format!(
            "yuyib-play: {count} Rapier trigger sensor(s) registered (sphere-query path off)"
        ));
    }
}

fn plane_motor_fallback(
    spawn: Vec3,
    player: &PlayerCharacterControlConfig3d,
) -> Option<PlayerBody> {
    let config = CharacterMotorConfig3d {
        ground_y: 0.0,
        move_speed: player.move_speed,
        jump_speed: player.jump_speed,
        radius: player.radius,
        gravity_y: player.gravity_y,
        ..CharacterMotorConfig3d::default()
    };
    match CharacterMotor3d::new(config, spawn) {
        Ok(motor) => {
            eprintln!(
                "yuyib-play: plane motor at ({:.2}, {:.2}, {:.2})",
                motor.position().x,
                motor.position().y,
                motor.position().z
            );
            Some(PlayerBody::Plane(motor))
        }
        Err(error) => {
            eprintln!("yuyib-play: plane motor failed: {error}");
            None
        }
    }
}

fn ground_chase_mesh() -> Result<TriangleMesh3d, String> {
    let extent = 2_000.0_f32;
    TriangleMesh3d::from_indexed(
        &[
            Vec3::new(-extent, 0.0, -extent),
            Vec3::new(extent, 0.0, -extent),
            Vec3::new(extent, 0.0, extent),
            Vec3::new(-extent, 0.0, extent),
        ],
        &[0, 2, 1, 0, 3, 2],
    )
    .map_err(|error| error.to_string())
}

fn find_player_entity(
    document: &SceneDocument,
    entities: &std::collections::BTreeMap<yuyib_authoring::EntityGuid, Entity>,
) -> Option<Entity> {
    let record = document.entities.iter().find(|entity| {
        entity
            .name
            .as_deref()
            .is_some_and(|name| name.eq_ignore_ascii_case("Player"))
    })?;
    entities.get(&record.guid).copied()
}

fn arg_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|argument| argument == flag)
        .and_then(|index| args.get(index + 1).map(String::as_str))
        .or_else(|| {
            args.iter().find_map(|argument| {
                argument
                    .strip_prefix(&format!("{flag}="))
                    .filter(|value| !value.is_empty())
            })
        })
}

/// Validates Editor-pinned blake3 content revision against on-disk bytes.
///
/// Accepts bare 64-hex or `blake3:<hex>`. History `--scene-revision` is session
/// correlation only and cannot be checked against `.yscene` contents.
fn validate_scene_file_revision(bytes: &[u8], expected: &str) -> Result<(), String> {
    let expected = expected
        .strip_prefix("blake3:")
        .unwrap_or(expected)
        .trim();
    if expected.len() != 64
        || !expected
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(format!(
            "invalid --scene-file-revision `{expected}` (expected 64 lowercase hex chars)"
        ));
    }
    let actual = blake3::hash(bytes).to_hex();
    if actual.as_str() != expected {
        return Err(format!(
            "scene file revision mismatch: pinned {expected}, on disk {actual} \
             (Editor and Play must see the same saved .yscene bytes)"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod pin_tests {
    use super::validate_scene_file_revision;

    #[test]
    fn accepts_matching_file_revision() {
        let bytes = b"{\"format\":\"yuyib.scene\"}";
        let digest = blake3::hash(bytes).to_hex();
        validate_scene_file_revision(bytes, digest.as_str()).expect("match");
        validate_scene_file_revision(bytes, &format!("blake3:{digest}")).expect("prefixed");
    }

    #[test]
    fn rejects_mismatch_and_malformed() {
        let bytes = b"scene-a";
        let other = blake3::hash(b"scene-b").to_hex();
        assert!(validate_scene_file_revision(bytes, other.as_str()).is_err());
        assert!(validate_scene_file_revision(bytes, "not-a-hash").is_err());
        assert!(validate_scene_file_revision(bytes, "ABCDEF").is_err());
    }
}

#[cfg(test)]
mod spawn_tests {
    use super::spawn_floor_hit_acceptable;

    #[test]
    fn rejects_roof_hits_above_authored_indoor_spawn() {
        let authored_y = 1.58_f32;
        assert!(spawn_floor_hit_acceptable(1.0, authored_y));
        assert!(spawn_floor_hit_acceptable(authored_y, authored_y));
        assert!(spawn_floor_hit_acceptable(authored_y + 0.5, authored_y));
        assert!(!spawn_floor_hit_acceptable(authored_y + 2.0, authored_y));
        assert!(!spawn_floor_hit_acceptable(4.5, authored_y));
    }
}

fn attach_gltf_hierarchy(
    project: &Path,
    world: &mut yuyib_ecs::prelude::World,
    root: Entity,
    payload: &Value,
    models: &mut Assets<Model>,
    cache: &mut HashMap<String, ModelHandle>,
) -> Result<bool, String> {
    let raw = payload
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let Some(raw) = raw else {
        return Ok(false);
    };
    if raw.eq_ignore_ascii_case("builtin:cube") {
        return Ok(false);
    }
    let path = raw.strip_prefix("asset://").unwrap_or(raw);
    let path = resolve_play_model_ref(project, path).unwrap_or_else(|| path.to_owned());
    let mut candidates = vec![project.join(&path), project.join("assets").join(&path)];
    if Path::new(&path).extension().is_none() {
        for base in candidates.clone() {
            candidates.push(base.with_extension("glb"));
            candidates.push(base.with_extension("gltf"));
        }
    }
    let absolute = candidates.into_iter().find(|candidate| candidate.is_file());
    let Some(absolute) = absolute else {
        eprintln!(
            "yuyib-play: no glTF file for model ref `{raw}` (resolved `{path}`) under {}",
            project.display()
        );
        return Ok(false);
    };
    let extension = absolute
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !matches!(extension.as_str(), "glb" | "gltf") {
        eprintln!(
            "yuyib-play: model path is not glTF (`{}`)",
            absolute.display()
        );
        return Ok(false);
    }
    let _ = cache;
    let cook_root = play_cook_cache_root(project);
    let (imported, cook_hit) = import_gltf_with_cook_cache(&absolute, &cook_root)?;
    eprintln!(
        "yuyib-play: glTF {} {} (cook={})",
        if cook_hit { "cook hit" } else { "cook miss" },
        absolute.display(),
        cook_root.display()
    );
    let spawned = spawn_scene(world, models, &imported, SceneSelection::Default)
        .map_err(|error| error.to_string())?;
    for child in spawned.roots() {
        set_parent_3d(world, *child, root).map_err(|error| error.to_string())?;
    }
    Ok(true)
}

fn play_cook_cache_root(project: &Path) -> PathBuf {
    project.join(".yuyib_cook")
}

/// Same disk cook path as Editor Preview / `project.cook` / ypack hydrate.
fn import_gltf_with_cook_cache(
    absolute: &Path,
    cook_root: &Path,
) -> Result<(yuyib_gltf::ImportedAsset, bool), String> {
    let bytes = fs::read(absolute).map_err(|error| error.to_string())?;
    let parent = absolute.parent().unwrap_or_else(|| Path::new("."));
    let cache = CookCache::new(cook_root);
    import_scene_bytes_cached_at(&bytes, parent, ImportOptions::default(), &cache)
        .map_err(|error| error.to_string())
}

fn resolve_model(
    project: &Path,
    payload: &Value,
    models: &mut Assets<Model>,
    cache: &mut HashMap<String, ModelHandle>,
    proxy: ModelHandle,
) -> Option<ModelHandle> {
    let raw = payload.get("model")?.as_str()?.trim();
    if raw.is_empty() || raw.eq_ignore_ascii_case("builtin:cube") {
        return Some(proxy);
    }
    let path = raw.strip_prefix("asset://").unwrap_or(raw);
    let path = resolve_play_model_ref(project, path).unwrap_or_else(|| path.to_owned());
    if path_looks_like_gltf(&path) {
        return None;
    }
    let _ = (project, models, cache);
    Some(proxy)
}

fn payload_is_gltf(payload: &Value) -> bool {
    payload
        .get("model")
        .and_then(Value::as_str)
        .is_some_and(|path| {
            let path = path.strip_prefix("asset://").unwrap_or(path).trim();
            if path.is_empty() || path.eq_ignore_ascii_case("builtin:cube") {
                return false;
            }
            looks_like_asset_guid(path)
                || path_looks_like_gltf(path)
                || Path::new(path).extension().is_none()
        })
}

/// Resolves `asset://{AssetGuid}` through sibling `.yasset` metadata under the project.
fn resolve_play_model_ref(project: &Path, path_or_guid: &str) -> Option<String> {
    if !looks_like_asset_guid(path_or_guid) {
        return None;
    }
    let roots = [project.to_path_buf(), project.join("assets")];
    for root in roots {
        if let Some(source) = find_yasset_source(&root, path_or_guid) {
            return Some(source);
        }
    }
    None
}

fn find_yasset_source(root: &Path, guid: &str) -> Option<String> {
    let entries = fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let file_type = entry.file_type().ok()?;
        if file_type.is_dir() {
            if let Some(found) = find_yasset_source(&path, guid) {
                return Some(found);
            }
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("yasset") {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        let Some(file_guid) = value.get("guid").and_then(Value::as_str) else {
            continue;
        };
        if !file_guid.eq_ignore_ascii_case(guid) {
            continue;
        }
        return value
            .get("source")
            .and_then(Value::as_str)
            .map(|source| source.replace('\\', "/"));
    }
    None
}

fn looks_like_asset_guid(value: &str) -> bool {
    let value = value.trim();
    if value.len() != 36 {
        return false;
    }
    let mut parts = value.split('-');
    matches!(
        (
            parts.next().map(str::len),
            parts.next().map(str::len),
            parts.next().map(str::len),
            parts.next().map(str::len),
            parts.next().map(str::len),
            parts.next(),
        ),
        (Some(8), Some(4), Some(4), Some(4), Some(12), None)
    ) && value
        .bytes()
        .all(|byte| byte == b'-' || byte.is_ascii_hexdigit())
}

fn path_looks_like_gltf(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            let extension = extension.to_ascii_lowercase();
            matches!(extension.as_str(), "glb" | "gltf")
        })
}

fn directional_from_payload(payload: &Value) -> Result<Option<DirectionalLight3d>, String> {
    if payload
        .get("enabled")
        .and_then(Value::as_bool)
        .is_some_and(|enabled| !enabled)
    {
        return Ok(None);
    }
    let direction = vec3_field(payload, "direction")?.unwrap_or([0.0, -1.0, 0.0]);
    let color = vec3_field(payload, "color")?.unwrap_or([1.0, 1.0, 1.0]);
    let illuminance = payload
        .get("illuminance_lux")
        .or_else(|| payload.get("illuminance"))
        .and_then(|value| match value {
            Value::Number(number) => number.as_f64().map(|v| v as f32),
            Value::String(text) => text.trim().parse().ok(),
            _ => None,
        })
        .filter(|value| value.is_finite())
        .unwrap_or(1.0);
    DirectionalLight3d::new(direction, color, illuminance)
        .map(Some)
        .map_err(|error| error.to_string())
}

fn play_scene_lighting() -> Game3dLighting {
    let fallback = LambertLighting3d::new(
        DirectionalLightDraw {
            direction: [0.0, -1.0, 0.0],
            color: [0.0, 0.0, 0.0],
            illuminance_lux: 0.0,
        },
        [0.016, 0.016, 0.02],
    )
    .expect("dark zero-illuminance Lambert fallback is valid");
    Game3dLighting::FirstDirectional {
        ambient: [0.016, 0.016, 0.02],
        fallback,
    }
}

fn apply_world_light_directions(
    world: &mut yuyib_ecs::prelude::World,
    document: &SceneDocument,
    entities: &std::collections::BTreeMap<yuyib_authoring::EntityGuid, Entity>,
) {
    let updates: Vec<(Entity, DirectionalLight3d)> = entities
        .iter()
        .filter_map(|(guid, entity)| {
            let record = document
                .entities
                .iter()
                .find(|entity_record| &entity_record.guid == guid)?;
            let light_component = record
                .components
                .iter()
                .find(|component| component.schema().as_str() == "yuyib.directional-light3d")?;
            let local = directional_from_payload(light_component.payload()).ok()??;
            let world_dir = rotate_direction_by_entity(world, *entity, local.direction());
            let world_light = local.with_direction(world_dir).unwrap_or(local);
            Some((*entity, world_light))
        })
        .collect();
    for (entity, light) in updates {
        world.entity_mut(entity).insert(light);
    }
}

fn rotate_direction_by_entity(
    world: &yuyib_ecs::prelude::World,
    entity: Entity,
    local_direction: [f32; 3],
) -> [f32; 3] {
    let rotation = if let Some(world_transform) = world.get::<WorldTransform3d>(entity) {
        world_transform
            .rotation()
            .or_else(|| world.get::<Transform3d>(entity).map(|t| t.rotation))
            .unwrap_or([0.0, 0.0, 0.0, 1.0])
    } else {
        world
            .get::<Transform3d>(entity)
            .map(|transform| transform.rotation)
            .unwrap_or([0.0, 0.0, 0.0, 1.0])
    };
    let [qx, qy, qz, qw] = rotation;
    let v = local_direction;
    let tx = 2.0 * (qy * v[2] - qz * v[1]);
    let ty = 2.0 * (qz * v[0] - qx * v[2]);
    let tz = 2.0 * (qx * v[1] - qy * v[0]);
    let rotated = [
        v[0] + qw * tx + (qy * tz - qz * ty),
        v[1] + qw * ty + (qz * tx - qx * tz),
        v[2] + qw * tz + (qx * ty - qy * tx),
    ];
    let len_sq = rotated[0] * rotated[0] + rotated[1] * rotated[1] + rotated[2] * rotated[2];
    if !len_sq.is_finite() || len_sq < 1.0e-12 {
        return local_direction;
    }
    let inv = 1.0 / len_sq.sqrt();
    [rotated[0] * inv, rotated[1] * inv, rotated[2] * inv]
}

fn vec3_field(payload: &Value, key: &str) -> Result<Option<[f32; 3]>, String> {
    let Some(value) = payload.get(key) else {
        return Ok(None);
    };
    let Some(array) = value.as_array() else {
        return Err(format!("{key} must be a JSON array"));
    };
    if array.len() != 3 {
        return Err(format!("{key} must contain 3 components"));
    }
    let mut out = [0.0; 3];
    for (index, component) in array.iter().enumerate() {
        out[index] = component
            .as_f64()
            .or_else(|| component.as_str().and_then(|text| text.parse().ok()))
            .map(|value| value as f32)
            .ok_or_else(|| format!("{key}[{index}] must be a finite number"))?;
        if !out[index].is_finite() {
            return Err(format!("{key}[{index}] must be finite"));
        }
    }
    Ok(Some(out))
}

#[cfg(test)]
mod cook_cache_tests {
    use super::{import_gltf_with_cook_cache, play_cook_cache_root};
    use std::fs;

    fn temporary_root(label: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "yuyib_play_cook_{label}_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("assets/models")).expect("dirs");
        root
    }

    fn valid_triangle_glb() -> Vec<u8> {
        let mut binary = Vec::new();
        binary.extend([0_u16, 1, 2].into_iter().flat_map(u16::to_le_bytes));
        binary.extend([0_u8; 2]);
        for position in [[0.0_f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
            binary.extend(position.into_iter().flat_map(f32::to_le_bytes));
        }
        let json = br#"{"asset":{"version":"2.0"},"scene":0,"scenes":[{"nodes":[0]}],"nodes":[{"name":"root","mesh":0}],"buffers":[{"byteLength":44}],"bufferViews":[{"buffer":0,"byteOffset":0,"byteLength":6},{"buffer":0,"byteOffset":8,"byteLength":36}],"accessors":[{"bufferView":0,"componentType":5123,"count":3,"type":"SCALAR"},{"bufferView":1,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}],"meshes":[{"primitives":[{"attributes":{"POSITION":1},"indices":0}]}]}"#;
        let mut padded_json = json.to_vec();
        while !padded_json.len().is_multiple_of(4) {
            padded_json.push(b' ');
        }
        while !binary.len().is_multiple_of(4) {
            binary.push(0);
        }
        let total = 12 + 8 + padded_json.len() + 8 + binary.len();
        let mut glb = Vec::with_capacity(total);
        glb.extend(b"glTF");
        glb.extend(2_u32.to_le_bytes());
        glb.extend(u32::try_from(total).expect("glb size").to_le_bytes());
        glb.extend(u32::try_from(padded_json.len()).expect("json size").to_le_bytes());
        glb.extend(0x4E4F_534A_u32.to_le_bytes());
        glb.extend(padded_json);
        glb.extend(u32::try_from(binary.len()).expect("bin size").to_le_bytes());
        glb.extend(0x004E_4942_u32.to_le_bytes());
        glb.extend(binary);
        glb
    }

    #[test]
    fn play_cook_cache_miss_then_hit() {
        let root = temporary_root("hit");
        let abs = root.join("assets/models/hero.glb");
        fs::write(&abs, valid_triangle_glb()).expect("write");
        let cook_root = play_cook_cache_root(&root);

        let (_, first_hit) = import_gltf_with_cook_cache(&abs, &cook_root).expect("first");
        assert!(!first_hit);
        let (_, second_hit) = import_gltf_with_cook_cache(&abs, &cook_root).expect("second");
        assert!(second_hit);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn play_cook_cache_root_is_project_dot_yuyib_cook() {
        let project = std::path::Path::new("D:/games/demo");
        assert_eq!(
            play_cook_cache_root(project),
            project.join(".yuyib_cook")
        );
    }
}
