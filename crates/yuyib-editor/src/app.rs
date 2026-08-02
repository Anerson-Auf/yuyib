use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    env,
    error::Error,
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
    rc::Rc,
    str::FromStr,
    sync::{
        Arc,
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::Instant,
};

use serde::Serialize;
use serde_json::{Value, json};
use yuyib_assets::{Assets, CookCache};
use yuyib_authoring::{AssetGuid, AuthoringRegistry, EntityGuid, TransactionError};
use yuyib_ecs::{bevy_ecs::entity::Entity, prelude::World};
use yuyib_editor_core::{
    AssetDependencyKind, AssetKind, AssetLogicalDependency, AssetOpenIntent, AssetOpsError,
    DocumentError, DocumentRevision, ManagedProcess, ProcessPoll, ProjectAssetIndex,
    ProjectDocumentStore, ProjectManifest, ProjectProfile, ScaffoldRequest,
    SceneModelRefMigrationRequest, build_asset_dependency_graph, build_asset_index,
    ensure_tracked_gltf, migrate_scene_model_refs, open_existing_project, plan_reimport_cascade,
    refresh_tracked_content_hash, refresh_tracked_dependencies, rename_tracked_gltf,
    resolve_tracked_asset, save_tracked_import_settings, scaffold_project,
};
use yuyib_game_3d::{
    DirectionalLight3d, DirectionalLightDraw, LocalTransform3d, Model3d, Parent3d,
    SceneBoundsResult3d, Transform3d, WorldTransform3d, propagate_world_transforms, scene_bounds_3d,
    set_parent_3d,
};
use yuyib_game_3d_authoring::{
    coerce_transform_field_value, json_f32, materialize_transform_scene,
    validate_directional_light_field, validate_model3d_field, validate_parent_field,
    validate_transform_field,
};
use yuyib_gltf::{
    ImportOptions, discover_external_dependencies, import_scene_bytes_cached_at,
};
use yuyib_gltf_authoring::default_settings_json;
use yuyib_model::{Model, ModelHandle};
use yuyib_platform::{
    ChildWindowPlacement, Window, WindowConfig,
    winit::{
        application::ApplicationHandler,
        dpi::PhysicalPosition,
        event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
        event_loop::{ActiveEventLoop, ControlFlow},
        window::WindowId,
    },
};
use yuyib_render::{
    BloomConfig, ClearColor, ColorGradeConfig, ColorPostProcess, FxaaConfig, RenderViewport,
    Renderer,
};
use yuyib_render_3d::{
    Camera3d, DiffuseIrradianceSh3d, Game3dLighting, Game3dScene, Game3dSceneConfig, Game3dShading,
    LambertLighting3d, PbrLighting3d, SsaoPolicy,
};
use yuyib_scene::{SceneSelection, spawn_scene_with_model};
use yuyib_webview::{
    AssetBundle, AssetLimits, AssetPath, BridgeLimits, EndpointName, LocalCsp, LocalPage,
    MimePolicy, PageEvent, PageSessionId, WebViewBounds, WebViewBuilder, WebViewHost,
};

use crate::{
    EMBEDDED_EDITOR_ASSETS,
    bridge::{
        AssetOpenRequest, AssetReimportRequest, AssetRenameRequest, AssetTrackRequest,
        AssetImportSettingsSaveRequest, BridgeBinding, CargoCheckRequest, CommandQueue,
        EditorCommand, MigrateSceneModelRefsRequest, PreviewOverlayRequest,
        PreviewSelectionRequest, ProjectCreateRequest, ProjectOpenRequest, SceneCommandRequest,
        SceneCreateRequest, SceneEditRequest, SceneOpenRequest, SceneSaveRequest, SelectionRequest,
        SourceRequest, SourceSaveRequest, ViewportBoundsRequest, ViewportPointerKind,
        ViewportPointerModifiers, ViewportPointerRequest, ViewportTool, WindowControlAction,
        WindowControlRequest, WorkspaceMode, create_bridge,
    },
    editor_gizmo::{self, GizmoLayout, GizmoState, GizmoUnlitPass},
    gltf_preview::{GltfPreviewFrame, GltfPreviewReimport, GltfPreviewSession},
    scene_authoring::{SceneMutationError, SceneSession, SceneSessionError},
    viewport_gizmo::{GizmoAxis, GizmoToolKind, apply_axis_scale, axis_parameter, rotate_quat},
    viewport_picking::{
        FOUNDATION_CUBE_SELECTION, PROXY_CUBE_HALF_EXTENT, ViewportRay, entity_model_matrix,
        intersect_horizontal_plane, pick_closest_proxy, viewport_ray_from_pointer,
    },
};

const DOCUMENT_BYTE_LIMIT: usize = 2 * 1024 * 1024;
const PROCESS_EVENT_CAPACITY: usize = 512;
const OUTBOUND_EVENT_CAPACITY: usize = 512;
const PROCESS_LINE_BYTE_LIMIT: usize = 16 * 1024;

pub struct EditorApp {
    // Drop order: WebView and GPU surface before the windows that own them.
    webview: Option<WebViewHost>,
    renderer: Option<Renderer>,
    viewport_window: Option<Window>,
    window: Option<Window>,
    command_queue: Option<CommandQueue>,
    dropped_commands: Option<Rc<Cell<u64>>>,
    bridge_failures: Option<Rc<RefCell<Vec<String>>>>,
    observed_dropped_commands: u64,
    page_session: Option<PageSessionId>,
    bridge_limits: Option<BridgeLimits>,
    ui_ready: bool,
    outbound: VecDeque<OutboundEvent>,
    mode: WorkspaceMode,
    logical_viewport: Option<ViewportBoundsRequest>,
    viewport: Option<RenderViewport>,
    viewport_buttons: i32,
    viewport_cursor: PhysicalPosition<f64>,
    viewport_shift: bool,
    close_requested: bool,
    occluded: bool,
    project_root: PathBuf,
    documents: ProjectDocumentStore,
    project: Option<ProjectManifest>,
    pending_startup_error: Option<String>,
    coverage: Value,
    world: World,
    models: Assets<Model>,
    proxy_model: ModelHandle,
    gizmo_unlit: Option<GizmoUnlitPass>,
    model_cache: HashMap<String, ModelHandle>,
    /// CPU-imported glTF scenes, shared across rematerialize passes.
    gltf_import_cache: HashMap<String, Arc<yuyib_gltf::ImportedAsset>>,
    /// Stable GPU model handles for glTF paths — rematerialize reuses residency.
    gltf_model_handles: HashMap<String, ModelHandle>,
    /// Paths currently importing on a worker thread.
    gltf_import_inflight: HashSet<String>,
    job_sender: SyncSender<EditorJob>,
    job_receiver: Receiver<EditorJob>,
    cube: Entity,
    materialized_entities: BTreeMap<EntityGuid, Entity>,
    gizmo: Option<GizmoState>,
    /// Authored Scene viewport GPU cache (Player cubes, scene models).
    scene: Game3dScene,
    /// Isolated Asset Preview GPU cache — avoids ModelHandle collisions with Scene
    /// and removes the need to flush Scene caches on every mode switch.
    preview_scene: Game3dScene,
    orbit: ViewportOrbit,
    viewport_tool: ViewportTool,
    transform_drag: Option<ViewportTransformDrag>,
    last_frame: Instant,
    last_render_error: Option<String>,
    selection: SelectionRequest,
    authored_scene: Option<SceneSession>,
    asset_index: Option<ProjectAssetIndex>,
    gltf_preview: Option<GltfPreviewSession>,
    /// Asset Preview AABB wireframe (`preview.overlay.set` / Bounds).
    preview_overlay_bounds: bool,
    /// Asset Preview collision mesh wireframe (`preview.overlay.set` / Collision).
    preview_overlay_collision: bool,
    /// Asset Preview vertex normals (`preview.overlay.set` / Normals).
    preview_overlay_normals: bool,
    /// Asset Preview vertex tangents (`preview.overlay.set` / Tangents).
    preview_overlay_tangents: bool,
    /// Asset Preview UV0 markers (`preview.overlay.set` / UV).
    preview_overlay_uv: bool,
    play: Option<ManagedProcess>,
    /// Last Play pin echoed on stop/timeout/exit (`path` + history + file revision).
    play_pin: Option<Value>,
    /// When true, a successful cargo build should launch the Play binary next.
    play_launch_after_build: bool,
    pending_play_args: Option<(Vec<String>, Option<Value>)>,
    cargo_check: Option<ManagedProcess>,
    cargo_package: Option<String>,
    process_sender: SyncSender<ProcessOutput>,
    process_receiver: Receiver<ProcessOutput>,
    /// Debounce for project file / asset index watch polls.
    watch_last_poll: Option<Instant>,
    /// Last published asset-index fingerprint (skip redundant host.assets).
    watch_asset_revision: Option<u64>,
    /// One-shot external scene conflict dialog until reload/save clears it.
    watch_scene_conflict_active: bool,
}

#[derive(Clone, Debug)]
enum PreviewRefresh {
    None,
    LiveTransform { entity_guid: String },
    EnsureEntities,
    EntityModel { entity_guid: String },
    RemoveEntity { entity_guid: String },
    Full,
}

impl PreviewRefresh {
    fn from_edit(command: &SceneEditRequest) -> Self {
        match command {
            SceneEditRequest::SetComponentField {
                entity_guid,
                component_id,
                ..
            } if matches!(
                component_id.as_str(),
                "yuyib.transform3d" | "yuyib.local-transform3d"
            ) =>
            {
                Self::LiveTransform {
                    entity_guid: entity_guid.clone(),
                }
            }
            SceneEditRequest::SetComponentField {
                entity_guid,
                component_id,
                field_path,
                ..
            } if component_id == "yuyib.model3d"
                && (field_path == "model" || field_path.starts_with("model.")) =>
            {
                Self::EntityModel {
                    entity_guid: entity_guid.clone(),
                }
            }
            SceneEditRequest::CreateEntity { .. } => Self::EnsureEntities,
            SceneEditRequest::RenameEntity { .. } => Self::None,
            SceneEditRequest::AddComponent {
                entity_guid,
                component_id,
            } if component_id == "yuyib.model3d" => Self::EntityModel {
                entity_guid: entity_guid.clone(),
            },
            SceneEditRequest::AddComponent { component_id, .. }
                if component_id == "yuyib.directional-light3d" =>
            {
                Self::Full
            }
            SceneEditRequest::AddComponent { .. } => Self::EnsureEntities,
            SceneEditRequest::DeleteEntity { entity_guid } => Self::RemoveEntity {
                entity_guid: entity_guid.clone(),
            },
            SceneEditRequest::Undo | SceneEditRequest::Redo => Self::Full,
            SceneEditRequest::RemoveComponent { .. }
            | SceneEditRequest::SetComponentField { .. } => Self::Full,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct ViewportOrbit {
    yaw: f32,
    pitch: f32,
    radius: f32,
    near: f32,
    far: f32,
    target: [f32; 3],
    dragging: bool,
    panning: bool,
    last_x: f64,
    last_y: f64,
}

#[derive(Clone, Debug)]
struct ViewportTransformDrag {
    entity_guid: String,
    kind: GizmoToolKind,
    axis: GizmoAxis,
    axis_constrained: bool,
    start_translation: [f32; 3],
    /// World-space gizmo origin at pointer-down (for axis picking math).
    start_world: [f32; 3],
    start_rotation: [f32; 4],
    start_scale: [f32; 3],
    pending_translation: [f32; 3],
    pending_rotation: [f32; 4],
    pending_scale: [f32; 3],
    start_axis_t: f32,
    last_x: f64,
    last_y: f64,
}

impl Default for ViewportOrbit {
    fn default() -> Self {
        Self {
            yaw: 0.0,
            pitch: 0.2,
            radius: 3.0,
            near: 0.1,
            far: 1000.0,
            target: [0.0, 0.0, 0.0],
            dragging: false,
            panning: false,
            last_x: 0.0,
            last_y: 0.0,
        }
    }
}

#[derive(Clone)]
struct OutboundEvent {
    name: &'static str,
    payload: Value,
}

struct ProcessOutput {
    process: String,
    stream: &'static str,
    line: String,
}

enum EditorJob {
    GltfImported {
        path: String,
        result: Result<yuyib_gltf::ImportedAsset, String>,
        cook_hit: bool,
    },
}

enum ProcessCompletion {
    Exited(std::process::ExitStatus),
    TimedOut(std::process::ExitStatus),
    PollFailed {
        error: String,
        process: ManagedProcess,
    },
}

#[derive(Serialize)]
struct Diagnostic<'a> {
    severity: &'a str,
    source: &'a str,
    message: &'a str,
}

struct EditorLayout {
    webview: WebViewBounds,
    viewport: Option<RenderViewport>,
}

impl EditorApp {
    /// Starts with no project loaded. The UI must open or create one.
    pub fn empty() -> Result<Self, Box<dyn Error>> {
        let root = ephemeral_workspace_root()?;
        Self::new_with_documents(ProjectDocumentStore::new(root, DOCUMENT_BYTE_LIMIT)?, None)
    }

    /// Opens `path` when it is a valid project root; otherwise starts empty and
    /// reports the failure after the UI is ready.
    pub fn from_project_path(path: impl AsRef<Path>) -> Result<Self, Box<dyn Error>> {
        match open_existing_project(path.as_ref(), DOCUMENT_BYTE_LIMIT) {
            Ok((documents, manifest)) => Self::new_with_documents(documents, Some(manifest)),
            Err(error) => {
                let mut app = Self::empty()?;
                app.pending_startup_error = Some(error.to_string());
                Ok(app)
            }
        }
    }

    fn new_with_documents(
        documents: ProjectDocumentStore,
        project: Option<ProjectManifest>,
    ) -> Result<Self, Box<dyn Error>> {
        let project_root = documents.root().to_path_buf();

        let mut registry = AuthoringRegistry::new();
        yuyib_authoring_yuyib::register_foundation(&mut registry)?;
        yuyib_gltf_authoring::register(&mut registry)?;
        registry.validate_coverage_gate()?;
        let coverage = serde_json::to_value(registry.coverage_manifest())?;

        let mut models = Assets::new();
        let cube_model = models.insert(Model::cube(0.7)?);
        let mut world = World::new();
        let cube = world
            .spawn((Model3d::new(cube_model), Transform3d::default()))
            .id();
        let scene = create_editor_game_scene(&project_root)?;
        let preview_scene = create_editor_preview_scene(&project_root)?;
        let (process_sender, process_receiver) = mpsc::sync_channel(PROCESS_EVENT_CAPACITY);
        let (job_sender, job_receiver) = mpsc::sync_channel(64);

        Ok(Self {
            webview: None,
            renderer: None,
            window: None,
            command_queue: None,
            dropped_commands: None,
            bridge_failures: None,
            observed_dropped_commands: 0,
            page_session: None,
            bridge_limits: None,
            ui_ready: false,
            outbound: VecDeque::new(),
            mode: WorkspaceMode::Scene,
            logical_viewport: None,
            viewport: None,
            viewport_window: None,
            viewport_buttons: 0,
            viewport_cursor: PhysicalPosition::new(0.0, 0.0),
            viewport_shift: false,
            close_requested: false,
            occluded: false,
            project_root,
            documents,
            project,
            pending_startup_error: None,
            coverage,
            world,
            models,
            proxy_model: cube_model,
            gizmo_unlit: None,
            model_cache: HashMap::new(),
            gltf_import_cache: HashMap::new(),
            gltf_model_handles: HashMap::new(),
            gltf_import_inflight: HashSet::new(),
            job_sender,
            job_receiver,
            cube,
            materialized_entities: BTreeMap::new(),
            gizmo: None,
            scene,
            preview_scene,
            orbit: ViewportOrbit::default(),
            viewport_tool: ViewportTool::Move,
            transform_drag: None,
            last_frame: Instant::now(),
            last_render_error: None,
            selection: SelectionRequest {
                id: Some("editor://foundation-cube".to_owned()),
                translation: None,
            },
            authored_scene: None,
            asset_index: None,
            gltf_preview: None,
            preview_overlay_bounds: true,
            preview_overlay_collision: false,
            preview_overlay_normals: true,
            preview_overlay_tangents: false,
            preview_overlay_uv: false,
            play: None,
            play_pin: None,
            play_launch_after_build: false,
            pending_play_args: None,
            cargo_check: None,
            cargo_package: None,
            process_sender,
            process_receiver,
            watch_last_poll: None,
            watch_asset_revision: None,
            watch_scene_conflict_active: false,
        })
    }

    fn initialize_window(&mut self, event_loop: &ActiveEventLoop) -> Result<(), Box<dyn Error>> {
        let window = Window::create(
            event_loop,
            &WindowConfig {
                title: if self.project.is_some() {
                    format!("Yuyib Editor — {}", self.project_root.display())
                } else {
                    "Yuyib Editor".to_owned()
                },
                width: 1440,
                height: 900,
                // Custom HTML titlebar owns chrome; native caption would double it.
                decorations: false,
                ..WindowConfig::default()
            },
        )?;
        let layout = editor_layout(
            &window,
            self.mode,
            self.logical_viewport,
            self.project.is_some(),
        )?;
        let BridgeBinding {
            router,
            queue,
            session,
            limits,
            dropped_commands,
            bridge_failures,
        } = create_bridge(&window)?;
        let page = embedded_editor_page()?;
        let webview = WebViewBuilder::new()
            .with_local_page(page)
            .with_bridge_router(router)
            .with_bridge_failures(Rc::clone(&bridge_failures))
            .with_bounds(layout.webview)
            .with_transparent(true)
            .with_devtools(true)
            .build(&window)?;

        if env::var_os("YUYIB_EDITOR_DEVTOOLS").is_some() {
            webview.open_devtools();
            eprintln!("yuyib-editor: opened WebView DevTools (YUYIB_EDITOR_DEVTOOLS)");
        }

        // WGPU must live on a sibling HWND above WebView2: a transparent hole in
        // the windowed WebView controller shows the parent brush, not the parent
        // DXGI swapchain (visible briefly when the WebView tears down on close).
        let placement = viewport_placement(layout.viewport.as_ref())?;
        let viewport_window = Window::create_child(event_loop, &window, placement)?;
        if layout.viewport.is_none() {
            viewport_window.hide();
        }
        let mut renderer = Renderer::new(&viewport_window)?;
        // Same opt-in HDR post path as playable / yuyib-play: filmic + bloom + FXAA.
        // SceneDocument persistence for these knobs is still open; session default
        // matches the engine presets so editor viewport is not a second lighting path.
        renderer.set_color_post_process(Some(
            ColorPostProcess::filmic()
                .with_exposure_ev(-0.25)
                .unwrap_or_else(|_| ColorPostProcess::filmic())
                .with_bloom(BloomConfig::street_city())
                .with_color_grade(ColorGradeConfig::street_city())
                .with_fxaa(FxaaConfig::street_city()),
        ));
        let gizmo_unlit = match GizmoUnlitPass::new(&renderer) {
            Ok(pass) => Some(pass),
            Err(error) => {
                eprintln!("yuyib-editor: gizmo unlit pass unavailable: {error}");
                None
            }
        };

        self.viewport = layout.viewport;
        self.command_queue = Some(queue);
        self.dropped_commands = Some(dropped_commands);
        self.bridge_failures = Some(bridge_failures);
        self.page_session = Some(session);
        self.bridge_limits = Some(limits);
        self.webview = Some(webview);
        self.renderer = Some(renderer);
        self.gizmo_unlit = gizmo_unlit;
        self.viewport_window = Some(viewport_window);
        self.window = Some(window);
        eprintln!(
            "yuyib-editor: window ready; session={} project={:?}",
            self.page_session
                .map(PageSessionId::to_hex)
                .unwrap_or_default(),
            self.project
                .as_ref()
                .map(|_| self.project_root.display().to_string())
        );
        self.window
            .as_ref()
            .expect("window was installed")
            .request_redraw();
        Ok(())
    }

    fn process_commands(&mut self) {
        self.observe_bridge_failures();
        let mut commands = VecDeque::new();
        if let Some(queue) = &self.command_queue
            && let Ok(mut shared) = queue.try_borrow_mut()
        {
            std::mem::swap(&mut *shared, &mut commands);
        }
        if !commands.is_empty() {
            eprintln!(
                "yuyib-editor: processing {} bridge command(s); ui_ready={}",
                commands.len(),
                self.ui_ready
            );
            // Any inbound UI command proves the page session is live; do not
            // leave host→UI emits stuck behind a missed ui.ready.
            if !self.ui_ready {
                eprintln!("yuyib-editor: marking ui_ready from inbound command");
                self.ui_ready = true;
            }
        }
        while let Some(command) = commands.pop_front() {
            match command {
                EditorCommand::UiReady => self.on_ui_ready(),
                EditorCommand::SetWorkspaceMode(mode) => self.set_workspace_mode(mode),
                EditorCommand::SetViewportTool(tool) => self.set_viewport_tool(tool),
                EditorCommand::SetViewportBounds(bounds) => self.set_viewport_bounds(bounds),
                EditorCommand::ViewportPointer(event) => self.handle_viewport_pointer(event),
                EditorCommand::WindowControl(request) => self.handle_window_control(request),
                EditorCommand::StartPlay => self.start_play(),
                EditorCommand::StopPlay => self.stop_play(),
                EditorCommand::CargoCheck(request) => self.start_cargo_check(&request),
                EditorCommand::ReadSource(request) => self.read_source(&request),
                EditorCommand::SaveSource(request) => self.save_source(&request),
                EditorCommand::SetSelection(selection) => {
                    // Selection only moves the gizmo — rematerializing every click
                    // reimports glTF hierarchies and freezes the UI.
                    self.set_selection_id(selection.id);
                }
                EditorCommand::OpenScene(request) => self.open_scene(request),
                EditorCommand::CreateScene(request) => self.create_scene(request),
                EditorCommand::SaveScene(request) => self.save_scene(request),
                EditorCommand::EditScene(request) => self.edit_scene(request),
                EditorCommand::BrowseOpenProject => self.browse_open_project(),
                EditorCommand::CreateProjectInteractive(request) => {
                    self.create_project_interactive(request)
                }
                EditorCommand::OpenProjectPath(request) => self.open_project_path(request),
                EditorCommand::RefreshAssetIndex => self.publish_asset_index(),
                EditorCommand::OpenAsset(request) => self.open_asset(request),
                EditorCommand::ReimportAsset(request) => self.reimport_asset(request),
                EditorCommand::TrackAsset(request) => self.track_asset(request),
                EditorCommand::RenameAsset(request) => self.rename_asset(request),
                EditorCommand::MigrateSceneModelRefs(request) => {
                    self.migrate_scene_model_refs(request)
                }
                EditorCommand::SaveAssetImportSettings(request) => {
                    self.save_asset_import_settings(request)
                }
                EditorCommand::SetPreviewOverlay(request) => self.set_preview_overlay(request),
                EditorCommand::SetPreviewSelection(request) => self.set_preview_selection(request),
            }
        }
        self.observe_command_overflow();
    }

    fn on_ui_ready(&mut self) {
        eprintln!("yuyib-editor: ui.ready acknowledged");
        self.ui_ready = true;
        self.publish_project_session(true);
        if let Some(error) = self.pending_startup_error.take() {
            self.publish_diagnostic("error", "project.open", &error);
        }
        self.flush_events();
    }

    fn publish_project_session(&mut self, include_coverage: bool) {
        // Publish project state first so the launcher can leave "Opening…" even when
        // a later oversized coverage/assets event fails to emit.
        self.publish_project_state();
        if include_coverage {
            self.publish_coverage();
        }
        self.publish_asset_index();
        if self.project.is_some() {
            self.auto_open_startup_scene();
        }
        self.emit_typed("host.selection", self.selection.clone());
        self.emit_viewport_orbit();
        if self.project.is_none() {
            self.publish_diagnostic(
                "info",
                "project",
                "Choose or create a project to begin authoring.",
            );
        }
    }

    fn publish_coverage(&mut self) {
        let total = self.coverage["capabilities"].as_array().map_or(0, Vec::len);
        let unavailable_items = collect_unavailable_capabilities(&self.coverage);
        let unavailable = unavailable_items.len();
        let component_coverage = normalized_component_coverage(&self.coverage);
        let project = self.project_payload();
        self.emit_value(
            "host.coverage",
            json!({
                "manifest": self.coverage,
                "status": "Registered",
                "covered": total.saturating_sub(unavailable),
                "total": total,
                "unavailable": unavailable_items,
                "project": project,
                "projectRoot": self.project_root.to_string_lossy(),
                "hasProject": self.project.is_some(),
                "components": component_coverage,
                "available_components": available_components(),
                "preview": {
                    "foundationViewport": true,
                    "transform": if self.materialized_entities.is_empty() {
                        "foundation-cube"
                    } else {
                        "materialized-proxy"
                    },
                    "gltf": if self.gltf_preview.as_ref().is_some_and(GltfPreviewSession::is_gpu_ready) {
                        "ready"
                    } else if self.gltf_preview.as_ref().is_some_and(GltfPreviewSession::is_cpu_ready) {
                        "uploading"
                    } else if self.gltf_preview.is_some() {
                        "loading"
                    } else {
                        "available"
                    }
                }
            }),
        );
        self.publish_diagnostic(
            "info",
            "editor",
            "Ready: scenes, Transform3d authoring, glTF preview via production importer, Cargo check.",
        );
    }

    fn project_payload(&self) -> Value {
        self.project.as_ref().map_or_else(
            || json!({ "ready": false, "name": serde_json::Value::Null, "root": serde_json::Value::Null }),
            |project| {
                json!({
                    "ready": true,
                    "name": project.name,
                    "profile": project.profile,
                    "package": project.development.cargo_package,
                    "play": {
                        "executable": project.development.play_executable,
                        "args": project.development.play_arguments,
                        "available": project.development.play_executable.is_some()
                            || self.project_root.join("Cargo.toml").is_file()
                    },
                    "scenes": project.scenes,
                    "startup_scene": project.startup_scene,
                    "asset_root": project.asset_root,
                    "code_root": project.code_root,
                    "root": self.project_root.to_string_lossy()
                })
            },
        )
    }

    fn publish_project_state(&mut self) {
        self.emit_value(
            "host.project",
            json!({
                "ready": self.project.is_some(),
                "root": self.project.as_ref().map(|_| self.project_root.to_string_lossy()),
                "project": self.project_payload()
            }),
        );
    }

    fn browse_open_project(&mut self) {
        eprintln!("yuyib-editor: project.openInteractive → folder picker");
        self.publish_diagnostic("info", "project.open", "Opening folder picker…");
        let Some(path) = self.pick_project_folder("Open Yuyib project") else {
            eprintln!("yuyib-editor: folder picker cancelled / returned None");
            self.publish_diagnostic(
                "info",
                "project.open",
                "Folder selection cancelled. Choose a directory that contains project.yuyib.",
            );
            // Re-sync so an already-open project can dismiss the launcher again.
            self.publish_project_state();
            return;
        };
        eprintln!("yuyib-editor: folder picker selected {}", path.display());
        self.open_project_at(path);
    }

    fn create_project_interactive(&mut self, request: ProjectCreateRequest) {
        let name = request.name.trim();
        if name.is_empty() {
            self.publish_diagnostic("warning", "project.create", "Project name is required.");
            return;
        }
        let profile = match request.profile.as_deref().unwrap_or("game3d") {
            "game2d" => ProjectProfile::Game2d,
            "application" => ProjectProfile::Application,
            _ => ProjectProfile::Game3d,
        };
        let parent = if let Some(parent) =
            request.parent_directory.filter(|value| !value.is_empty())
        {
            PathBuf::from(parent)
        } else {
            let Some(parent) = self.pick_project_folder("Choose parent folder for the new project")
            else {
                self.publish_diagnostic(
                    "info",
                    "project.create",
                    "Folder selection cancelled. Pick the parent directory for the new project.",
                );
                self.publish_project_state();
                return;
            };
            parent
        };
        match scaffold_project(ScaffoldRequest {
            parent_directory: parent,
            project_name: name.to_owned(),
            profile,
        }) {
            Ok(project) => self.open_project_at(project.root),
            Err(error) => {
                self.publish_diagnostic("error", "project.create", &error.to_string());
                self.publish_project_state();
            }
        }
    }

    fn pick_project_folder(&self, title: &str) -> Option<PathBuf> {
        // Do not attach to the WebView2 HWND: on Windows that combination often
        // fails to surface the native folder picker while still returning None.
        rfd::FileDialog::new().set_title(title).pick_folder()
    }

    fn open_project_path(&mut self, request: ProjectOpenRequest) {
        let path = request.path.trim();
        eprintln!("yuyib-editor: project.open path={path:?}");
        if path.is_empty() {
            self.publish_diagnostic("warning", "project.open", "Project path is empty.");
            self.publish_project_state();
            return;
        }
        self.publish_diagnostic("info", "project.open", &format!("Opening {path}…"));
        self.open_project_at(PathBuf::from(path));
    }

    fn open_project_at(&mut self, root: PathBuf) {
        eprintln!("yuyib-editor: open_project_at {}", root.display());
        match open_existing_project(&root, DOCUMENT_BYTE_LIMIT) {
            Ok((documents, manifest)) => {
                eprintln!(
                    "yuyib-editor: opened ok root={} name={}",
                    documents.root().display(),
                    manifest.name
                );
                let same_root = roots_equal(&self.project_root, documents.root());
                self.documents = documents;
                self.project_root = self.documents.root().to_path_buf();
                self.project = Some(manifest);
                self.authored_scene = None;
                self.selection = SelectionRequest {
                    id: None,
                    translation: None,
                };
                self.asset_index = None;
                self.gltf_preview = None;
                // Preview is always project-local; Scene GPU residency can stay warm
                // when reopening the same root in-process.
                self.preview_scene.clear_model_caches();
                self.watch_asset_revision = None;
                self.watch_scene_conflict_active = false;
                if same_root {
                    eprintln!(
                        "yuyib-editor: same-root reopen — keeping import/GPU session caches"
                    );
                } else {
                    self.gltf_import_cache.clear();
                    self.gltf_model_handles.clear();
                    self.gltf_import_inflight.clear();
                    self.model_cache.clear();
                    if let Err(error) = self.rebuild_preview_scene() {
                        self.publish_diagnostic("error", "project.open", &error.to_string());
                    }
                }
                self.reset_foundation_preview();
                if self.play.is_some() {
                    self.stop_play();
                }
                if let Some(process) = self.cargo_check.take() {
                    stop_process_async(process, "cargo", self.process_sender.clone());
                }
                self.publish_project_session(true);
                self.publish_diagnostic(
                    "info",
                    "project",
                    &format!(
                        "Opened project {}{}",
                        self.project_root.display(),
                        if same_root {
                            " (session caches warm)"
                        } else {
                            ""
                        }
                    ),
                );
            }
            Err(error) => {
                eprintln!("yuyib-editor: open_project_at failed: {error}");
                self.publish_diagnostic("error", "project.open", &error.to_string());
                self.publish_project_state();
            }
        }
    }

    fn rebuild_preview_scene(&mut self) -> Result<(), Box<dyn Error>> {
        self.scene = create_editor_game_scene(&self.project_root)?;
        self.preview_scene = create_editor_preview_scene(&self.project_root)?;
        Ok(())
    }

    fn auto_open_startup_scene(&mut self) {
        let Some(project) = &self.project else {
            return;
        };
        let path = project
            .startup_scene
            .and_then(|guid| {
                project
                    .scenes
                    .iter()
                    .find(|scene| scene.guid == guid)
                    .map(|scene| scene.path.clone())
            })
            .or_else(|| project.scenes.first().map(|scene| scene.path.clone()));
        let Some(path) = path else {
            return;
        };
        if self
            .authored_scene
            .as_ref()
            .is_some_and(|scene| scene.path() == path)
        {
            self.publish_scene_state();
            return;
        }
        self.open_scene(SceneOpenRequest { path });
    }

    fn publish_asset_index(&mut self) {
        let Some(project) = self.project.clone() else {
            self.asset_index = None;
            self.emit_value(
                "host.assets",
                json!({
                    "root": "",
                    "revision": 0,
                    "items": [],
                    "diagnostics": [],
                    "status": "waiting_for_project"
                }),
            );
            return;
        };
        let asset_root = project.asset_root.clone();
        match build_asset_index(&self.documents, &asset_root) {
            Ok(index) => {
                self.watch_asset_revision = Some(index.revision);
                let mut payload = asset_index_payload(&index, "ready");
                merge_project_scenes_into_assets(&mut payload, &project);
                self.asset_index = Some(index);
                self.emit_value("host.assets", payload);
            }
            Err(error) => {
                self.asset_index = None;
                self.watch_asset_revision = None;
                let mut payload = json!({
                    "root": asset_root,
                    "revision": 0,
                    "items": [],
                    "diagnostics": [{
                        "path": "",
                        "code": "invalid_asset_root",
                        "message": error.to_string(),
                        "severity": "error",
                        "source": "assets"
                    }],
                    "status": "ready"
                });
                merge_project_scenes_into_assets(&mut payload, &project);
                self.emit_value("host.assets", payload);
                self.publish_diagnostic("error", "assets", &error.to_string());
            }
        }
    }

    fn open_asset(&mut self, request: AssetOpenRequest) {
        if let Some(project) = &self.project {
            if let Some(scene) = project.scenes.iter().find(|scene| {
                scene.path == request.id
                    || scene.name == request.id
                    || scene.guid.to_string() == request.id
            }) {
                let path = scene.path.clone();
                self.open_scene(SceneOpenRequest { path });
                return;
            }
        }
        if request.id.ends_with(".yscene") {
            self.open_scene(SceneOpenRequest {
                path: request.id.clone(),
            });
            return;
        }
        let Some(index) = &self.asset_index else {
            self.publish_diagnostic(
                "warning",
                "asset.open",
                "Asset index is empty; refresh after opening a project.",
            );
            return;
        };
        let asset_root = index.root.clone();
        let matched = index.items.iter().find(|item| {
            item.path == request.id
                || item.id.as_ref().is_some_and(|guid| {
                    guid.to_string() == request.id || format!("asset://{guid}") == request.id
                })
        });
        let Some(item) = matched else {
            self.publish_diagnostic(
                "warning",
                "asset.open",
                &format!("Asset {} was not found in the current index.", request.id),
            );
            return;
        };
        let open_scene = item.open == Some(AssetOpenIntent::Scene) || item.kind == AssetKind::Scene;
        let mut open_gltf =
            item.open == Some(AssetOpenIntent::GltfPreview) || item.kind == AssetKind::GltfSource;
        // .yasset metadata cards share the GUID with the glTF source — resolve
        // the real .glb/.gltf path so preview starts on the first click.
        if !open_gltf && item.kind == AssetKind::AssetMetadata {
            open_gltf = item.metadata.as_ref().is_some_and(|metadata| {
                let source = metadata.source.replace('\\', "/");
                source.ends_with(".glb") || source.ends_with(".gltf")
            });
        }
        let scene_path = if open_scene {
            Some(if asset_root.is_empty() {
                item.path.clone()
            } else {
                format!("{asset_root}/{}", item.path)
            })
        } else {
            None
        };
        let selection_id = item
            .id
            .as_ref()
            .map(|guid| format!("asset://{guid}"))
            .unwrap_or_else(|| format!("asset://{}", item.path));
        let kind = item.kind;
        let path = item.path.clone();
        let name = item.name.clone();
        let tracking = item.tracking;
        let preview = item.preview;
        let reimport = item.reimport;
        let import_settings = item
            .metadata
            .as_ref()
            .map(|metadata| {
                json!({
                    "schema": metadata.import_settings.schema.as_str(),
                    "version": metadata.import_settings.version.get(),
                    "payload": metadata.import_settings.payload.clone(),
                    "editable": true,
                })
            })
            .or_else(|| {
                (kind == AssetKind::GltfSource).then(|| {
                    json!({
                        "schema": "yuyib.gltf-import-settings",
                        "version": 1,
                        "payload": default_settings_json(),
                        "editable": false,
                        "reason": "track_required",
                    })
                })
            });
        let dependencies = item.metadata.as_ref().map(|metadata| {
            metadata
                .dependencies
                .iter()
                .map(|guid| format!("asset://{guid}"))
                .collect::<Vec<_>>()
        });
        let dependency_diagnostics = item.metadata.as_ref().and_then(|metadata| {
            metadata
                .extensions
                .get("yuyib.dependency_diagnostics")
                .cloned()
        });
        let dependents = self.asset_dependents_payload(&selection_id);
        if let Some(path) = scene_path {
            self.open_scene(SceneOpenRequest { path });
            return;
        }
        self.set_selection_id(Some(selection_id.clone()));
        self.emit_value(
            "host.asset",
            json!({
                "id": selection_id,
                "kind": asset_kind_label(kind),
                "path": path,
                "name": name,
                "tracking": asset_tracking_label(tracking),
                "preview": preview.map(action_status_payload),
                "reimport": reimport.map(action_status_payload),
                "import_settings": import_settings,
                "dependencies": dependencies,
                "dependents": dependents,
                "dependency_diagnostics": dependency_diagnostics,
            }),
        );
        if open_gltf {
            let preview_path = self
                .resolve_gltf_source_under_root(&selection_id)
                .or_else(|| self.resolve_gltf_source_under_root(&path))
                .unwrap_or_else(|| path.clone());
            // Mode first so the next non-zero bounds apply to the Preview hole
            // before CPU import completes and GPU upload starts.
            if self.mode != WorkspaceMode::Preview {
                self.set_workspace_mode(WorkspaceMode::Preview);
            }
            self.start_gltf_preview(&preview_path);
        }
    }

    fn reimport_asset(&mut self, request: AssetReimportRequest) {
        let path = request
            .id
            .strip_prefix("asset://")
            .unwrap_or(&request.id)
            .to_owned();
        eprintln!("yuyib-editor: asset.reimport {path}");
        let settings = self
            .resolve_gltf_import_settings(&path)
            .unwrap_or_else(default_settings_json);
        let under = self
            .resolve_gltf_source_under_root(&path)
            .unwrap_or(path.clone());
        // Explicit reimport refreshes dependency snapshot; preview open does not.
        self.refresh_asset_dependencies(&under);
        if let Some(project) = self.project.clone() {
            if let Err(error) =
                refresh_tracked_content_hash(&self.documents, &project.asset_root, &under)
            {
                if !matches!(error, AssetOpsError::NotTracked(_)) {
                    self.publish_diagnostic(
                        "warning",
                        "asset.reimport",
                        &format!("Could not refresh content hash: {error}"),
                    );
                }
            }
        }
        self.publish_asset_index();
        let preview_reimport = {
            let under_ref = under.as_str();
            self.gltf_preview.as_mut().and_then(|session| {
                (session.relative_path() == under_ref)
                    .then(|| session.reimport_with_settings(&settings))
            })
        };
        if let Some(result) = preview_reimport {
            match result {
                Ok(GltfPreviewReimport::CacheHit) => {
                    let cascade_payload = self.propagate_reimport_dependents(&path);
                    self.emit_value(
                        "host.process",
                        json!({
                            "kind": "preview",
                            "status": "ready",
                            "stage": "cache_hit",
                            "path": under,
                            "completed": 1.0,
                            "non_destructive": true,
                            "cascade": cascade_payload,
                        }),
                    );
                }
                Ok(GltfPreviewReimport::Started) => {
                    let cascade_payload = self.propagate_reimport_dependents(&path);
                    self.emit_value(
                        "host.process",
                        json!({
                            "kind": "preview",
                            "status": "progress",
                            "stage": "reimport",
                            "path": under,
                            "completed": 0.0,
                            "non_destructive": true,
                            "cascade": cascade_payload,
                        }),
                    );
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                }
                Err(error) => {
                    self.publish_diagnostic("error", "asset.reimport", &error.to_string());
                }
            }
            return;
        }
        let cascade_payload = self.propagate_reimport_dependents(&path);
        if cascade_payload.is_some() {
            self.emit_value(
                "host.process",
                json!({
                    "kind": "asset",
                    "status": "reimport_cascade",
                    "path": under,
                    "cascade": cascade_payload,
                }),
            );
        }
        self.start_gltf_preview(&under);
    }

    /// Refreshes reverse dependents after a root reimport (no mass rematerialize).
    fn propagate_reimport_dependents(&mut self, root_identity: &str) -> Option<Value> {
        let project = self.project.clone()?;
        let tracked =
            resolve_tracked_asset(&self.documents, &project.asset_root, root_identity).ok()?;
        let graph = build_asset_dependency_graph(&self.documents, &project.asset_root).ok()?;
        let plan = plan_reimport_cascade(&graph, tracked.guid);
        if plan.dependents.is_empty() {
            return Some(json!({
                "root": format!("asset://{}", plan.root),
                "dependents": [],
                "refreshed": [],
                "preview_reimported": [],
            }));
        }

        let mut refreshed = Vec::new();
        let mut preview_reimported = Vec::new();
        for dependent in &plan.dependents {
            let Ok(dep) =
                resolve_tracked_asset(&self.documents, &project.asset_root, &dependent.to_string())
            else {
                continue;
            };
            let under = dep
                .source
                .strip_prefix(&format!("{}/", project.asset_root.replace('\\', "/")))
                .or_else(|| {
                    let root = project.asset_root.replace('\\', "/");
                    dep.source.strip_prefix(&root).and_then(|rest| {
                        rest.strip_prefix('/').or(Some(rest)).filter(|s| !s.is_empty())
                    })
                })
                .unwrap_or(dep.source.as_str())
                .to_owned();

            self.invalidate_cached_source(&dep.source, &under);
            if under.ends_with(".glb")
                || under.ends_with(".gltf")
                || dep.source.ends_with(".glb")
                || dep.source.ends_with(".gltf")
            {
                self.refresh_asset_dependencies(&dependent.to_string());
            }

            let mut did_preview = false;
            let preview_matches = self
                .gltf_preview
                .as_ref()
                .is_some_and(|session| session.relative_path() == under);
            if preview_matches {
                let settings = self
                    .resolve_gltf_import_settings(&under)
                    .unwrap_or_else(default_settings_json);
                if let Some(session) = self.gltf_preview.as_mut() {
                    match session.reimport_with_settings(&settings) {
                        Ok(GltfPreviewReimport::CacheHit | GltfPreviewReimport::Started) => {
                            did_preview = true;
                        }
                        Err(error) => {
                            self.publish_diagnostic(
                                "warning",
                                "asset.reimport.dependents",
                                &format!(
                                    "Dependent preview reimport failed for `{under}`: {error}"
                                ),
                            );
                        }
                    }
                }
            }
            let id = format!("asset://{dependent}");
            refreshed.push(id.clone());
            if did_preview {
                preview_reimported.push(id);
            }
        }

        self.publish_diagnostic(
            "info",
            "asset.reimport.dependents",
            &format!(
                "Reimport cascade from asset://{}: {} dependent(s) invalidated",
                plan.root,
                refreshed.len()
            ),
        );

        Some(json!({
            "root": format!("asset://{}", plan.root),
            "dependents": plan
                .dependents
                .iter()
                .map(|guid| format!("asset://{guid}"))
                .collect::<Vec<_>>(),
            "refreshed": refreshed,
            "preview_reimported": preview_reimported,
        }))
    }

    fn invalidate_cached_source(&mut self, project_source: &str, under_root: &str) {
        self.model_cache.remove(project_source);
        self.model_cache.remove(under_root);
        self.gltf_import_cache.remove(project_source);
        self.gltf_import_cache.remove(under_root);
        self.invalidate_gltf_model_handle(project_source);
        self.invalidate_gltf_model_handle(under_root);
        if let Some(prefixed) = project_source
            .strip_prefix("assets/")
            .map(str::to_owned)
            .or_else(|| Some(format!("assets/{under_root}")))
        {
            self.model_cache.remove(&prefixed);
            self.gltf_import_cache.remove(&prefixed);
            self.invalidate_gltf_model_handle(&prefixed);
        }
    }

    fn invalidate_gltf_model_handle(&mut self, path: &str) {
        if let Some(handle) = self.gltf_model_handles.remove(path) {
            self.scene.invalidate_model(handle);
        }
    }

    fn track_asset(&mut self, request: AssetTrackRequest) {
        let Some(project) = self.project.clone() else {
            self.publish_diagnostic(
                "warning",
                "asset.track",
                "Open a project before tracking assets.",
            );
            return;
        };
        let asset_root = project.asset_root.clone();
        let Some(source_under_root) = self.resolve_gltf_source_under_root(&request.id) else {
            self.publish_diagnostic(
                "warning",
                "asset.track",
                &format!(
                    "Could not resolve glTF source `{}`. Select a .glb/.gltf card in Assets (path), not a scene entity UUID.",
                    request.id
                ),
            );
            return;
        };
        match ensure_tracked_gltf(&self.documents, &asset_root, &source_under_root) {
            Ok(tracked) => {
                eprintln!(
                    "yuyib-editor: asset.track guid={} source={} created={}",
                    tracked.guid, tracked.source, tracked.created
                );
                self.publish_diagnostic(
                    "info",
                    "asset.track",
                    &format!(
                        "{} `{}` as asset://{}",
                        if tracked.created {
                            "Tracked"
                        } else {
                            "Already tracked"
                        },
                        tracked.source,
                        tracked.guid
                    ),
                );
                self.refresh_asset_dependencies(&tracked.guid.to_string());
                self.publish_asset_index();
            }
            Err(error) => {
                self.publish_diagnostic("error", "asset.track", &error.to_string());
            }
        }
    }

    fn rename_asset(&mut self, request: AssetRenameRequest) {
        let Some(project) = self.project.clone() else {
            self.publish_diagnostic(
                "warning",
                "asset.rename",
                "Open a project before renaming assets.",
            );
            return;
        };
        let asset_root = project.asset_root.clone();
        let to = request
            .to
            .strip_prefix(&format!("{asset_root}/"))
            .unwrap_or(&request.to)
            .trim()
            .replace('\\', "/");
        if to.is_empty() {
            self.publish_diagnostic(
                "warning",
                "asset.rename",
                "Rename target path must not be empty.",
            );
            return;
        }
        // Keep glTF preview path coherent when renaming the active preview source.
        let previous_preview = self
            .gltf_preview
            .as_ref()
            .map(|session| session.relative_path().to_owned());
        match rename_tracked_gltf(&self.documents, &asset_root, &request.id, &to) {
            Ok(tracked) => {
                eprintln!(
                    "yuyib-editor: asset.rename guid={} -> {}",
                    tracked.guid, tracked.source
                );
                let under = tracked
                    .source
                    .strip_prefix(&format!("{asset_root}/"))
                    .unwrap_or(&tracked.source)
                    .to_owned();
                if previous_preview
                    .as_ref()
                    .is_some_and(|old| self.paths_refer_same_asset(old, &request.id))
                    || self.selection_matches_asset(&request.id)
                {
                    self.start_gltf_preview(&under);
                }
                self.publish_diagnostic(
                    "info",
                    "asset.rename",
                    &format!(
                        "Renamed tracked asset://{} -> `{}` (GUID preserved)",
                        tracked.guid, tracked.source
                    ),
                );
                self.publish_asset_index();
            }
            Err(error) => {
                self.publish_diagnostic("error", "asset.rename", &error.to_string());
            }
        }
    }

    fn migrate_scene_model_refs(&mut self, request: MigrateSceneModelRefsRequest) {
        let Some(project) = self.project.clone() else {
            self.publish_diagnostic(
                "warning",
                "assets.migrate_scene_model_refs",
                "Open a project before migrating scene model refs.",
            );
            return;
        };
        let scene_paths = request.scene_paths.unwrap_or_else(|| {
            project
                .scenes
                .iter()
                .map(|scene| scene.path.replace('\\', "/"))
                .collect()
        });
        if scene_paths.is_empty() {
            self.publish_diagnostic(
                "warning",
                "assets.migrate_scene_model_refs",
                "No scenes listed in the project manifest.",
            );
            return;
        }

        let mut skip_paths = Vec::new();
        if let Some(scene) = &self.authored_scene
            && scene.is_dirty()
        {
            skip_paths.push(scene.path().replace('\\', "/"));
        }

        let open_path = self
            .authored_scene
            .as_ref()
            .map(|scene| scene.path().replace('\\', "/"));

        match migrate_scene_model_refs(
            &self.documents,
            &project.asset_root,
            &SceneModelRefMigrationRequest {
                scene_paths,
                dry_run: request.dry_run,
                skip_paths,
            },
        ) {
            Ok(report) => {
                eprintln!(
                    "yuyib-editor: assets.migrate_scene_model_refs dry_run={} scanned={} changed={} rewritten={}",
                    report.dry_run,
                    report.scenes_scanned,
                    report.scenes_changed,
                    report.refs_rewritten
                );
                let summary = format!(
                    "{} · scanned {} · changed {} · rewritten {} · untracked {} · already GUID {}",
                    if report.dry_run { "Dry run" } else { "Applied" },
                    report.scenes_scanned,
                    report.scenes_changed,
                    report.refs_rewritten,
                    report.refs_skipped_untracked,
                    report.refs_already_guid
                );
                self.publish_diagnostic(
                    if report.scenes.iter().any(|entry| {
                        entry.status == "error" || entry.status == "conflict"
                    }) {
                        "warning"
                    } else {
                        "info"
                    },
                    "assets.migrate_scene_model_refs",
                    &summary,
                );
                self.emit_value(
                    "host.process",
                    json!({
                        "kind": "assets",
                        "status": "migrate_scene_model_refs",
                        "report": report,
                    }),
                );

                if !request.dry_run
                    && let Some(path) = open_path
                    && report.scenes.iter().any(|entry| {
                        entry.path == path && entry.changed && entry.status == "ok"
                    })
                {
                    match SceneSession::open(&self.documents, path.clone()) {
                        Ok(scene) => {
                            self.authored_scene = Some(scene);
                            self.publish_scene_document(None);
                            self.rebuild_transform_preview();
                        }
                        Err(error) => {
                            self.publish_diagnostic(
                                "warning",
                                "assets.migrate_scene_model_refs",
                                &format!("Migrated `{path}` on disk but reload failed: {error}"),
                            );
                        }
                    }
                }
            }
            Err(error) => {
                self.publish_diagnostic(
                    "error",
                    "assets.migrate_scene_model_refs",
                    &error.to_string(),
                );
            }
        }
    }

    fn save_asset_import_settings(&mut self, request: AssetImportSettingsSaveRequest) {
        let Some(project) = self.project.clone() else {
            self.publish_diagnostic(
                "warning",
                "asset.import_settings.save",
                "Open a project before editing import settings.",
            );
            return;
        };
        if let Err(error) = yuyib_gltf_authoring::parse_import_settings(&request.payload) {
            self.publish_diagnostic(
                "warning",
                "asset.import_settings.save",
                error.message(),
            );
            return;
        }
        match save_tracked_import_settings(
            &self.documents,
            &project.asset_root,
            &request.id,
            request.payload.clone(),
        ) {
            Ok(tracked) => {
                eprintln!(
                    "yuyib-editor: asset.import_settings.save guid={} source={}",
                    tracked.guid, tracked.source
                );
                self.publish_diagnostic(
                    "info",
                    "asset.import_settings.save",
                    &format!(
                        "Saved import settings for asset://{} (`{}`)",
                        tracked.guid, tracked.source
                    ),
                );
                self.publish_asset_index();
                let under = tracked
                    .source
                    .strip_prefix(&format!("{}/", project.asset_root.replace('\\', "/")))
                    .unwrap_or(&tracked.source)
                    .to_owned();
                if let Some(session) = self.gltf_preview.as_mut() {
                    if session.relative_path() == under {
                        match session.reimport_with_settings(&request.payload) {
                            Ok(GltfPreviewReimport::CacheHit) => {}
                            Ok(GltfPreviewReimport::Started) => {
                                if let Some(window) = &self.window {
                                    window.request_redraw();
                                }
                            }
                            Err(error) => {
                                self.publish_diagnostic(
                                    "warning",
                                    "asset.import_settings.save",
                                    &format!("Settings saved, but reimport failed: {error}"),
                                );
                            }
                        }
                    }
                }
                self.emit_value(
                    "host.asset",
                    json!({
                        "id": format!("asset://{}", tracked.guid),
                        "kind": "model",
                        "path": under,
                        "name": Path::new(&under)
                            .file_stem()
                            .and_then(|stem| stem.to_str())
                            .unwrap_or("asset"),
                        "tracking": "tracked",
                        "import_settings": {
                            "schema": "yuyib.gltf-import-settings",
                            "version": 1,
                            "payload": request.payload,
                            "editable": true,
                        }
                    }),
                );
            }
            Err(error) => {
                self.publish_diagnostic(
                    "error",
                    "asset.import_settings.save",
                    &error.to_string(),
                );
            }
        }
    }

    /// Discovers glTF external URIs and persists resolved GUID edges on `.yasset`.
    fn refresh_asset_dependencies(&mut self, identity: &str) {
        let Some(project) = self.project.clone() else {
            return;
        };
        let Some(under) = self.resolve_gltf_source_under_root(identity) else {
            return;
        };
        let project_source = if project.asset_root.is_empty() {
            under.clone()
        } else {
            format!(
                "{}/{}",
                project.asset_root.replace('\\', "/"),
                under.trim_start_matches(['/', '\\'])
            )
        };
        let absolute = self.project_root.join(&project_source);
        let bytes = match fs::read(&absolute) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.publish_diagnostic(
                    "warning",
                    "asset.dependencies",
                    &format!("Could not read `{project_source}` for dependency discovery: {error}"),
                );
                return;
            }
        };
        let discovered = match discover_external_dependencies(&bytes) {
            Ok(deps) => deps,
            Err(error) => {
                self.publish_diagnostic(
                    "warning",
                    "asset.dependencies",
                    &format!("Dependency discovery failed for `{project_source}`: {error}"),
                );
                return;
            }
        };
        let logical: Vec<AssetLogicalDependency> = discovered
            .into_iter()
            .map(|dependency| AssetLogicalDependency {
                uri: dependency.uri,
                kind: match dependency.kind {
                    yuyib_assets::ImportDependencyKind::Required => AssetDependencyKind::Required,
                    yuyib_assets::ImportDependencyKind::Optional => AssetDependencyKind::Optional,
                },
            })
            .collect();
        match refresh_tracked_dependencies(
            &self.documents,
            &project.asset_root,
            identity,
            &logical,
        ) {
            Ok((_tracked, report)) => {
                eprintln!(
                    "yuyib-editor: asset.dependencies resolved={} unresolved={}",
                    report.dependencies.len(),
                    report.unresolved.len()
                );
                if !report.unresolved.is_empty() {
                    self.publish_diagnostic(
                        "info",
                        "asset.dependencies",
                        &format!(
                            "`{project_source}`: {} resolved GUID edge(s), {} unresolved URI(s)",
                            report.dependencies.len(),
                            report.unresolved.len()
                        ),
                    );
                }
            }
            Err(error) => {
                if !matches!(error, AssetOpsError::NotTracked(_)) {
                    self.publish_diagnostic("warning", "asset.dependencies", &error.to_string());
                }
            }
        }
    }

    /// Reverse edges for a tracked asset GUID (`asset://…` or bare UUID).
    fn asset_dependents_payload(&self, identity: &str) -> Option<Vec<String>> {
        let project = self.project.as_ref()?;
        let raw = identity
            .strip_prefix("asset://")
            .unwrap_or(identity)
            .trim();
        let guid = AssetGuid::from_str(raw).ok()?;
        let graph = build_asset_dependency_graph(&self.documents, &project.asset_root).ok()?;
        Some(
            graph
                .dependents_of(guid)
                .iter()
                .map(|dependent| format!("asset://{dependent}"))
                .collect(),
        )
    }

    fn paths_refer_same_asset(&self, preview_relative: &str, identity: &str) -> bool {
        let identity = identity.strip_prefix("asset://").unwrap_or(identity);
        preview_relative == identity
            || self.selection_matches_asset(identity)
            || self.asset_index.as_ref().is_some_and(|index| {
                index.items.iter().any(|item| {
                    item.path == preview_relative
                        && item
                            .id
                            .as_ref()
                            .is_some_and(|guid| guid.to_string() == identity)
                })
            })
    }

    fn selection_matches_asset(&self, identity: &str) -> bool {
        let Some(selection) = self.selection.id.as_deref() else {
            return false;
        };
        let left = selection.strip_prefix("asset://").unwrap_or(selection);
        let right = identity.strip_prefix("asset://").unwrap_or(identity);
        left == right || selection == identity
    }

    /// Reads `import_settings.payload` from a tracked `.yasset` when present.
    fn resolve_gltf_import_settings(&self, identity: &str) -> Option<Value> {
        let under = self.resolve_gltf_source_under_root(identity)?;
        let index = self.asset_index.as_ref()?;
        let project = self.project.as_ref()?;
        let asset_root = project.asset_root.replace('\\', "/");
        let project_source = if asset_root.is_empty() {
            under.clone()
        } else {
            format!("{asset_root}/{under}")
        };
        for item in &index.items {
            let Some(metadata) = &item.metadata else {
                continue;
            };
            let source = metadata.source.replace('\\', "/");
            if source != project_source && source != under {
                continue;
            }
            return Some(metadata.import_settings.payload.clone());
        }
        None
    }

    fn resolve_gltf_source_under_root(&self, identity: &str) -> Option<String> {
        let project = self.project.as_ref()?;
        let asset_root = project.asset_root.replace('\\', "/");
        let raw = identity
            .strip_prefix("asset://")
            .unwrap_or(identity)
            .replace('\\', "/");
        if let Some(index) = &self.asset_index {
            if let Some(item) = index.items.iter().find(|item| {
                item.kind == AssetKind::GltfSource
                    && (item.path == raw
                        || item.id.as_ref().is_some_and(|guid| guid.to_string() == raw)
                        || format!("{asset_root}/{}", item.path) == raw)
            }) {
                return Some(item.path.clone());
            }
            // Tracked metadata card / selection by GUID → source path in .yasset.
            if let Some(item) = index.items.iter().find(|item| {
                item.kind == AssetKind::AssetMetadata
                    && item.id.as_ref().is_some_and(|guid| guid.to_string() == raw)
            }) {
                if let Some(metadata) = &item.metadata {
                    let source = metadata.source.replace('\\', "/");
                    let under = source
                        .strip_prefix(&format!("{asset_root}/"))
                        .unwrap_or(&source);
                    if under.ends_with(".glb") || under.ends_with(".gltf") {
                        return Some(under.to_owned());
                    }
                }
            }
        }
        if raw.ends_with(".glb") || raw.ends_with(".gltf") {
            let under = raw
                .strip_prefix(&format!("{asset_root}/"))
                .unwrap_or(&raw)
                .to_owned();
            return Some(under);
        }
        None
    }

    fn start_gltf_preview(&mut self, relative_path: &str) {
        let Some(project) = &self.project else {
            self.publish_diagnostic(
                "warning",
                "asset.preview",
                "Open a project before previewing glTF assets.",
            );
            return;
        };
        let asset_root = project.asset_root.clone();
        // New import targets preview_scene only — Scene residency stays warm.
        self.preview_scene.clear_model_caches();
        let settings = self
            .resolve_gltf_import_settings(relative_path)
            .unwrap_or_else(default_settings_json);
        match GltfPreviewSession::start_with_settings(
            &self.project_root,
            &asset_root,
            relative_path,
            &settings,
        ) {
            Ok(session) => {
                eprintln!(
                    "yuyib-editor: glTF preview start {}",
                    session.absolute_path().display()
                );
                self.gltf_preview = Some(session);
                self.emit_value(
                    "host.process",
                    json!({
                        "kind": "preview",
                        "status": "progress",
                        "stage": "queued",
                        "path": relative_path,
                        "completed": 0.0
                    }),
                );
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            Err(error) => {
                // Do not clear an existing last-good session on a failed open of
                // a different path; only clear when starting fresh fails with no session.
                if self.gltf_preview.is_none() {
                    self.gltf_preview = None;
                }
                self.publish_diagnostic("error", "asset.preview", &error.to_string());
            }
        }
    }

    fn poll_gltf_preview(&mut self) {
        let Some(session) = &mut self.gltf_preview else {
            return;
        };
        if session.is_gpu_ready() {
            return;
        }
        if session.is_cpu_ready() {
            if let Some(gpu) = session.last_gpu() {
                let path = session.relative_path().to_owned();
                let total = gpu.total_primitives.max(1) as f64;
                let completed = (gpu.completed_primitives as f64 / total).clamp(0.0, 0.99);
                self.emit_value(
                    "host.process",
                    json!({
                        "kind": "preview",
                        "status": "progress",
                        "stage": "gpu_upload",
                        "path": path,
                        "completed": completed,
                        "primitive_count": gpu.total_primitives,
                        "gpu_bytes": gpu.total_geometry_bytes
                    }),
                );
            }
            if let Some(window) = &self.window {
                window.request_redraw();
            }
            return;
        }
        let poll = session.poll();
        if let Some(error) = poll.failed {
            self.gltf_preview = None;
            self.publish_diagnostic("error", "asset.preview", &error);
            self.emit_value(
                "host.process",
                json!({
                    "kind": "preview",
                    "status": "failed",
                    "stage": poll.stage,
                    "path": poll.relative_path,
                    "message": error
                }),
            );
            return;
        }
        if poll.cpu_ready {
            if let Some(session) = &self.gltf_preview {
                session.frame_orbit(
                    &mut self.orbit.target,
                    &mut self.orbit.radius,
                    &mut self.orbit.near,
                    &mut self.orbit.far,
                );
                self.apply_orbit_camera();
            }
            self.emit_value(
                "host.process",
                json!({
                    "kind": "preview",
                    "status": "progress",
                    "stage": "gpu_upload",
                    "path": poll.relative_path,
                    "completed": 0.5
                }),
            );
            if let Some(window) = &self.window {
                window.request_redraw();
            }
            return;
        }
        let completed = if poll.total == 0 {
            0.0
        } else {
            poll.completed as f64 / poll.total as f64 * 0.5
        };
        self.emit_value(
            "host.process",
            json!({
                "kind": "preview",
                "status": "progress",
                "stage": poll.stage,
                "path": poll.relative_path,
                "completed": completed
            }),
        );
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn set_preview_overlay(&mut self, request: PreviewOverlayRequest) {
        match request.overlay.as_str() {
            "bounds" => {
                self.preview_overlay_bounds = request.enabled;
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            "collision" => {
                self.preview_overlay_collision = request.enabled;
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            "normals" => {
                self.preview_overlay_normals = request.enabled;
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            "tangents" => {
                self.preview_overlay_tangents = request.enabled;
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            "uv" => {
                self.preview_overlay_uv = request.enabled;
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            other => self.publish_diagnostic(
                "warning",
                "preview.overlay.set",
                &format!(
                    "preview overlay `{other}` is not supported yet (Bounds/Collision/Normals/Tangents/UV are available)"
                ),
            ),
        }
    }

    fn set_preview_selection(&mut self, request: PreviewSelectionRequest) {
        let Some(session) = self.gltf_preview.as_mut() else {
            self.publish_diagnostic(
                "warning",
                "preview.selection.set",
                "Open a glTF in Asset Preview before selecting a mesh/material",
            );
            return;
        };
        let result = match request.kind.as_str() {
            "mesh" => session.set_mesh_selection(request.index),
            "material" => session.set_material_selection(request.index),
            other => {
                self.publish_diagnostic(
                    "warning",
                    "preview.selection.set",
                    &format!(
                        "preview selection kind `{other}` is not supported yet (mesh/material are available)"
                    ),
                );
                return;
            }
        };
        match result {
            Ok(()) => {
                self.emit_preview_mesh_state();
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            Err(error) => {
                self.publish_diagnostic("warning", "preview.selection.set", &error.to_string());
            }
        }
    }

    fn emit_preview_mesh_state(&mut self) {
        let Some(session) = &self.gltf_preview else {
            return;
        };
        let meshes = session.mesh_inventory();
        let materials = session.material_inventory();
        if meshes.is_empty()
            && materials.is_empty()
            && session.selected_mesh().is_none()
            && session.selected_material().is_none()
        {
            return;
        }
        self.emit_value(
            "host.process",
            json!({
                "kind": "preview",
                "status": "selection",
                "path": session.relative_path(),
                "selected_mesh": session.selected_mesh(),
                "selected_material": session.selected_material(),
                "meshes": meshes,
                "materials": materials,
            }),
        );
    }

    fn set_viewport_bounds(&mut self, bounds: ViewportBoundsRequest) {
        // Ignore transient 0×0 reports from ResizeObserver while Asset Preview
        // is active — they hide the HWND and stall GPU upload mid-import.
        if bounds.width <= 0.0 || bounds.height <= 0.0 {
            if self.gltf_preview.is_some() && matches!(self.mode, WorkspaceMode::Preview) {
                return;
            }
            self.logical_viewport = None;
        } else {
            self.logical_viewport = Some(bounds);
        }
        if let Err(error) = self.apply_layout() {
            self.viewport = None;
            self.publish_diagnostic("error", "viewport.bounds", &error.to_string());
        }
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn handle_viewport_pointer(&mut self, event: ViewportPointerRequest) {
        match event.kind {
            ViewportPointerKind::Pointerleave => {
                self.orbit.dragging = false;
                self.orbit.panning = false;
                self.commit_viewport_tool_drag();
            }
            ViewportPointerKind::Pointerdown if event.button == 1 => {
                // Middle mouse: pan orbit target through the scene.
                self.orbit.panning = true;
                self.orbit.last_x = event.x;
                self.orbit.last_y = event.y;
            }
            ViewportPointerKind::Pointerdown if event.button == 2 && event.modifiers.shift => {
                // Shift+RMB: pan (for mice without a middle button).
                self.orbit.panning = true;
                self.orbit.last_x = event.x;
                self.orbit.last_y = event.y;
            }
            ViewportPointerKind::Pointerdown if event.button == 2 || event.buttons & 2 != 0 => {
                self.orbit.dragging = true;
                self.orbit.last_x = event.x;
                self.orbit.last_y = event.y;
            }
            ViewportPointerKind::Pointerdown
                if self.mode == WorkspaceMode::Scene && event.button == 0 =>
            {
                if self.selection.id.clone().is_some_and(|selection_id| {
                    self.begin_viewport_tool_drag(selection_id, event.x, event.y)
                }) {
                    return;
                }
                if let Some(selection_id) = self.pick_viewport_selection(event.x, event.y) {
                    self.set_selection_id(Some(selection_id.to_owned()));
                    // Do not rematerialize on pick — that reimports glTF and stalls input.
                    let _ =
                        self.begin_viewport_tool_drag(selection_id.to_owned(), event.x, event.y);
                }
            }
            ViewportPointerKind::Pointerup => {
                self.orbit.dragging = false;
                self.orbit.panning = false;
                self.commit_viewport_tool_drag();
            }
            ViewportPointerKind::Pointermove if self.orbit.panning => {
                let dx = (event.x - self.orbit.last_x) as f32;
                let dy = (event.y - self.orbit.last_y) as f32;
                self.orbit.last_x = event.x;
                self.orbit.last_y = event.y;
                let scale = (self.orbit.radius * 0.0018).max(0.001);
                let (yaw_sin, yaw_cos) = self.orbit.yaw.sin_cos();
                let (pitch_sin, pitch_cos) = self.orbit.pitch.sin_cos();
                // Camera-right and camera-up for screen-space pan.
                let right = [yaw_cos, 0.0, -yaw_sin];
                let up = [yaw_sin * pitch_sin, pitch_cos, yaw_cos * pitch_sin];
                self.orbit.target[0] += (-dx * right[0] + dy * up[0]) * scale;
                self.orbit.target[1] += (-dx * right[1] + dy * up[1]) * scale;
                self.orbit.target[2] += (-dx * right[2] + dy * up[2]) * scale;
                self.apply_orbit_camera();
                self.sync_overlay_gizmos();
                self.emit_viewport_orbit();
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            ViewportPointerKind::Pointermove if self.orbit.dragging => {
                let dx = (event.x - self.orbit.last_x) as f32;
                let dy = (event.y - self.orbit.last_y) as f32;
                self.orbit.last_x = event.x;
                self.orbit.last_y = event.y;
                self.orbit.yaw -= dx * 0.005;
                self.orbit.pitch = (self.orbit.pitch + dy * 0.005).clamp(-1.45, 1.45);
                self.apply_orbit_camera();
                self.sync_overlay_gizmos();
                self.emit_viewport_orbit();
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            ViewportPointerKind::Pointermove if self.transform_drag.is_some() => {
                self.update_transform_drag(event.x, event.y);
            }
            ViewportPointerKind::Wheel => {
                let zoom = 1.0 + (event.delta_y as f32) * 0.0015;
                self.orbit.radius = (self.orbit.radius * zoom.clamp(0.85, 1.15))
                    .clamp(0.75, self.orbit.far.max(40.0) * 0.45);
                self.apply_orbit_camera();
                self.sync_overlay_gizmos();
                self.emit_viewport_orbit();
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            _ => {}
        }
    }

    fn set_viewport_tool(&mut self, tool: ViewportTool) {
        self.viewport_tool = tool;
        self.clear_viewport_tool_drag();
        // Never rematerialize the scene on W/E/R — only swap the gizmo draw list.
        self.refresh_gizmo();
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn clear_viewport_tool_drag(&mut self) {
        self.transform_drag = None;
    }

    fn commit_viewport_tool_drag(&mut self) {
        let Some(drag) = self.transform_drag.take() else {
            return;
        };
        let unchanged = drag.pending_translation == drag.start_translation
            && drag.pending_rotation == drag.start_rotation
            && drag.pending_scale == drag.start_scale;
        if unchanged {
            return;
        }
        let Some(scene) = &self.authored_scene else {
            return;
        };
        let base_revision = scene.history_revision().get();
        let entity_guid = drag.entity_guid.clone();
        let result = self.authored_scene.as_mut().map(|scene| {
            scene.commit_transform3d(
                base_revision,
                &entity_guid,
                drag.pending_translation,
                drag.pending_rotation,
                drag.pending_scale,
            )
        });
        match result {
            Some(Ok(_)) => {
                self.publish_scene_document(Some("viewport-transform"));
                self.publish_scene_history_with_transaction(Some("viewport-transform"));
                // Live drag already updated ECS transforms — rematerializing
                // would reimport every glTF and freeze the window.
                self.refresh_gizmo();
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            Some(Err(SceneMutationError::Transaction(TransactionError::RevisionConflict {
                expected,
                actual,
            }))) => {
                let path = self
                    .authored_scene
                    .as_ref()
                    .map(|scene| scene.path().to_owned())
                    .unwrap_or_default();
                self.emit_value(
                    "host.scene.conflict",
                    json!({
                        "path": path,
                        "expected_revision": expected.get(),
                        "actual_revision": actual.get(),
                        "transaction_id": "viewport-transform",
                        "message": "The page attempted to edit a stale authoring revision."
                    }),
                );
                self.publish_scene_history();
                self.rebuild_transform_preview();
            }
            Some(Err(error)) => {
                self.publish_diagnostic("error", "viewport.transform", &error.to_string());
                self.publish_scene_history();
                self.rebuild_transform_preview();
            }
            None => {}
        }
    }

    fn begin_viewport_tool_drag(&mut self, selection_id: String, x: f64, y: f64) -> bool {
        let Some(kind) = editor_gizmo::tool_kind(self.viewport_tool) else {
            return false;
        };
        let Some(start_translation) = self.authored_translation(&selection_id) else {
            return false;
        };
        let Some(world_origin) = self.selection_world_origin() else {
            return false;
        };
        let Some(ray) = self.viewport_ray(x, y) else {
            return false;
        };
        let layout = self.gizmo_layout_at(world_origin);
        let Some(pick) = editor_gizmo::pick(self.viewport_tool, ray, world_origin, layout) else {
            return false;
        };
        let start_axis_t = axis_parameter(ray, world_origin, pick.axis).unwrap_or_default();
        let start_rotation = self
            .authored_rotation(&selection_id)
            .unwrap_or([0.0, 0.0, 0.0, 1.0]);
        let start_scale = self
            .authored_scale(&selection_id)
            .unwrap_or([1.0, 1.0, 1.0]);
        self.transform_drag = Some(ViewportTransformDrag {
            entity_guid: selection_id.clone(),
            kind,
            axis: pick.axis,
            axis_constrained: pick.axis_constrained,
            start_translation,
            start_world: world_origin,
            start_rotation,
            start_scale,
            pending_translation: start_translation,
            pending_rotation: start_rotation,
            pending_scale: start_scale,
            start_axis_t,
            last_x: x,
            last_y: y,
        });
        true
    }

    fn update_transform_drag(&mut self, x: f64, y: f64) {
        let Some(mut drag) = self.transform_drag.clone() else {
            return;
        };
        match drag.kind {
            GizmoToolKind::Move if drag.axis_constrained => {
                let Some(ray) = self.viewport_ray(x, y) else {
                    return;
                };
                let Some(axis_t) = axis_parameter(ray, drag.start_world, drag.axis) else {
                    return;
                };
                let axis = drag.axis.as_vec3();
                let delta = axis_t - drag.start_axis_t;
                drag.pending_translation = [
                    drag.start_translation[0] + axis[0] * delta,
                    drag.start_translation[1] + axis[1] * delta,
                    drag.start_translation[2] + axis[2] * delta,
                ];
            }
            GizmoToolKind::Move => {
                if let Some(hit) = self.viewport_plane_hit(x, y, drag.start_translation[1]) {
                    drag.pending_translation = [hit[0], drag.start_translation[1], hit[2]];
                } else {
                    let scale = (self.orbit.radius * 0.0025).max(0.001);
                    let dx = (x - drag.last_x) as f32 * scale;
                    let dy = (y - drag.last_y) as f32 * scale;
                    let (yaw_sin, yaw_cos) = self.orbit.yaw.sin_cos();
                    // Camera-right / camera-forward on XZ so W still moves when the Y plane misses.
                    drag.pending_translation[0] += yaw_cos * dx + yaw_sin * dy;
                    drag.pending_translation[2] += -yaw_sin * dx + yaw_cos * dy;
                }
            }
            GizmoToolKind::Rotate => {
                let delta_radians = ((x - drag.last_x) * 0.0125) as f32;
                drag.pending_rotation =
                    rotate_quat(drag.pending_rotation, drag.axis, delta_radians);
            }
            GizmoToolKind::Scale if drag.axis_constrained => {
                let Some(ray) = self.viewport_ray(x, y) else {
                    return;
                };
                let Some(axis_t) = axis_parameter(ray, drag.start_world, drag.axis) else {
                    return;
                };
                drag.pending_scale =
                    apply_axis_scale(drag.start_scale, drag.axis, axis_t - drag.start_axis_t);
            }
            GizmoToolKind::Scale => {
                let delta = ((y - drag.last_y) as f32) * -0.01;
                let next = |value: f32| (value + delta).abs().max(0.001);
                drag.pending_scale = [
                    next(drag.pending_scale[0]),
                    next(drag.pending_scale[1]),
                    next(drag.pending_scale[2]),
                ];
            }
        };
        drag.last_x = x;
        drag.last_y = y;
        self.apply_live_transform(
            &drag.entity_guid,
            drag.pending_translation,
            drag.pending_rotation,
            drag.pending_scale,
        );
        self.transform_drag = Some(drag);
    }

    fn apply_live_transform(
        &mut self,
        entity_guid: &str,
        translation: [f32; 3],
        rotation: [f32; 4],
        scale: [f32; 3],
    ) {
        if !translation
            .iter()
            .chain(rotation.iter())
            .chain(scale.iter())
            .all(|value| value.is_finite())
        {
            return;
        }
        let Ok(guid) = EntityGuid::from_str(entity_guid) else {
            return;
        };
        let Some(entity) = self.materialized_entities.get(&guid).copied() else {
            return;
        };
        // Game3dScene::render re-propagates LocalTransform3d → WorldTransform3d every
        // frame. Updating only Transform3d is overwritten when the entity is parented.
        if let Some(mut transform) = self.world.get_mut::<Transform3d>(entity) {
            transform.translation = translation;
            transform.rotation = rotation;
            transform.scale = scale;
        }
        if let Some(mut local) = self.world.get_mut::<LocalTransform3d>(entity) {
            local.translation = translation;
            local.rotation = rotation;
            local.scale = scale;
        } else {
            self.world.entity_mut(entity).remove::<WorldTransform3d>();
        }
        if self.world.get::<LocalTransform3d>(entity).is_some()
            || self.world.get::<Parent3d>(entity).is_some()
        {
            let _ = propagate_world_transforms(&mut self.world);
        }
        self.sync_authored_lights();
        self.selection.translation = Some(translation);
        self.emit_typed("host.selection", self.selection.clone());
        // Keep handles glued to the live world position of the selection.
        if let Some(origin) = self.selection_world_origin() {
            self.place_selection_gizmo(origin);
        }
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn viewport_plane_hit(&mut self, x: f64, y: f64, plane_y: f32) -> Option<[f32; 3]> {
        intersect_horizontal_plane(self.viewport_ray(x, y)?, plane_y)
    }

    fn viewport_ray(&mut self, x: f64, y: f64) -> Option<ViewportRay> {
        let viewport = self.viewport?;
        let logical = self.logical_viewport?;
        if !(0.0..logical.width).contains(&x) || !(0.0..logical.height).contains(&y) {
            return None;
        }
        self.apply_orbit_camera();
        viewport_ray_from_pointer(
            *self.scene.camera_mut(),
            [viewport.width(), viewport.height()],
            x,
            y,
            logical.width,
            logical.height,
        )
        .ok()
    }

    fn authored_translation(&self, entity_guid: &str) -> Option<[f32; 3]> {
        self.authored_transform_vec3(entity_guid, "translation")
    }

    fn authored_scale(&self, entity_guid: &str) -> Option<[f32; 3]> {
        self.authored_transform_vec3(entity_guid, "scale")
    }

    fn authored_transform_vec3(&self, entity_guid: &str, field: &str) -> Option<[f32; 3]> {
        let scene = self.authored_scene.as_ref()?;
        let entity = scene
            .document()
            .entities
            .iter()
            .find(|entity| entity.guid.to_string() == entity_guid)?;
        let translation = entity
            .components
            .iter()
            .find(|component| component.schema().as_str() == "yuyib.transform3d")?
            .payload()
            .get(field)?
            .as_array()?;
        Some([
            json_f32(translation.first()?)?,
            json_f32(translation.get(1)?)?,
            json_f32(translation.get(2)?)?,
        ])
    }

    fn authored_rotation(&self, entity_guid: &str) -> Option<[f32; 4]> {
        let scene = self.authored_scene.as_ref()?;
        let entity = scene
            .document()
            .entities
            .iter()
            .find(|entity| entity.guid.to_string() == entity_guid)?;
        let rotation = entity
            .components
            .iter()
            .find(|component| component.schema().as_str() == "yuyib.transform3d")?
            .payload()
            .get("rotation")?
            .as_array()?;
        Some([
            json_f32(rotation.first()?)?,
            json_f32(rotation.get(1)?)?,
            json_f32(rotation.get(2)?)?,
            json_f32(rotation.get(3)?)?,
        ])
    }

    fn apply_orbit_camera(&mut self) {
        let (yaw_sin, yaw_cos) = self.orbit.yaw.sin_cos();
        let (pitch_sin, pitch_cos) = self.orbit.pitch.sin_cos();
        let position = [
            self.orbit.target[0] + self.orbit.radius * yaw_sin * pitch_cos,
            self.orbit.target[1] + self.orbit.radius * pitch_sin,
            self.orbit.target[2] + self.orbit.radius * yaw_cos * pitch_cos,
        ];
        let target = self.orbit.target;
        let near = self.orbit.near;
        let far = self.orbit.far;
        {
            let camera = self.scene.camera_mut();
            camera.target = target;
            camera.position = position;
            camera.up = [0.0, 1.0, 0.0];
            camera.near = near;
            camera.far = far;
        }
        {
            let camera = self.preview_scene.camera_mut();
            camera.target = target;
            camera.position = position;
            camera.up = [0.0, 1.0, 0.0];
            camera.near = near;
            camera.far = far;
        }
    }

    fn set_selection_id(&mut self, id: Option<String>) {
        self.selection.id = id;
        self.refresh_selection_translation();
        self.refresh_gizmo();
        self.emit_typed("host.selection", self.selection.clone());
        // Selecting Player frames the viewport — spawn/create leaves orbit on the map
        // corner otherwise. Other entities keep the current framing (F-focus later).
        if let Some(id) = self.selection.id.clone() {
            if let Ok(guid) = EntityGuid::from_str(&id) {
                if self
                    .entity_display_name(&guid)
                    .is_some_and(|name| name.eq_ignore_ascii_case("Player"))
                {
                    self.frame_orbit_to_selection();
                }
            }
        }
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    /// Focuses the orbit camera on the current selection (Unity-like Frame).
    fn frame_orbit_to_selection(&mut self) {
        let Some(origin) = self.selection_world_origin().or_else(|| {
            self.selection
                .id
                .as_deref()
                .and_then(|id| self.authored_translation(id))
        }) else {
            return;
        };
        self.frame_orbit_to_point(origin, 4.5);
    }

    fn frame_orbit_to_point(&mut self, target: [f32; 3], radius: f32) {
        self.orbit.target = target;
        self.orbit.radius = radius.clamp(2.0, 5_000.0);
        self.orbit.near = (self.orbit.radius * 0.0005).max(0.05);
        self.orbit.far = (self.orbit.radius * 40.0).max(self.orbit.near * 200.0);
        self.apply_orbit_camera();
        self.sync_overlay_gizmos();
        self.emit_viewport_orbit();
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn entity_display_name(&self, guid: &EntityGuid) -> Option<String> {
        self.authored_scene.as_ref().and_then(|scene| {
            scene
                .document()
                .entities
                .iter()
                .find(|entity| &entity.guid == guid)
                .and_then(|entity| entity.name.clone())
        })
    }

    fn refresh_selection_translation(&mut self) {
        self.selection.translation = self.selection.id.as_deref().and_then(|id| {
            if let Some(drag) = &self.transform_drag {
                if drag.entity_guid == id {
                    return Some(drag.pending_translation);
                }
            }
            self.authored_translation(id)
        });
    }

    fn emit_viewport_orbit(&mut self) {
        self.emit_value(
            "host.viewport.orbit",
            json!({
                "yaw": self.orbit.yaw,
                "pitch": self.orbit.pitch,
                "radius": self.orbit.radius,
                "target": self.orbit.target,
            }),
        );
    }

    /// Keeps gizmo screen size roughly constant while orbiting/zooming.
    fn gizmo_layout_at(&self, origin: [f32; 3]) -> GizmoLayout {
        GizmoLayout::from_camera_distance(self.camera_distance_to(origin))
    }

    fn camera_distance_to(&self, origin: [f32; 3]) -> f32 {
        let (yaw_sin, yaw_cos) = self.orbit.yaw.sin_cos();
        let (pitch_sin, pitch_cos) = self.orbit.pitch.sin_cos();
        let eye = [
            self.orbit.target[0] + self.orbit.radius * yaw_sin * pitch_cos,
            self.orbit.target[1] + self.orbit.radius * pitch_sin,
            self.orbit.target[2] + self.orbit.radius * yaw_cos * pitch_cos,
        ];
        let dx = eye[0] - origin[0];
        let dy = eye[1] - origin[1];
        let dz = eye[2] - origin[2];
        (dx * dx + dy * dy + dz * dz).sqrt()
    }

    fn selection_world_origin(&self) -> Option<[f32; 3]> {
        let selection_id = self.selection.id.as_deref()?;
        if let Ok(guid) = EntityGuid::from_str(selection_id) {
            if let Some(entity) = self.materialized_entities.get(&guid).copied() {
                if let Some(origin) = editor_gizmo::entity_world_translation(&self.world, entity) {
                    return Some(origin);
                }
            }
        }
        // Live drag: authored local may already be updated on the entity.
        if let Some(drag) = &self.transform_drag {
            if drag.entity_guid == selection_id {
                let delta = [
                    drag.pending_translation[0] - drag.start_translation[0],
                    drag.pending_translation[1] - drag.start_translation[1],
                    drag.pending_translation[2] - drag.start_translation[2],
                ];
                return Some([
                    drag.start_world[0] + delta[0],
                    drag.start_world[1] + delta[1],
                    drag.start_world[2] + delta[2],
                ]);
            }
        }
        self.authored_translation(selection_id)
    }

    fn sync_overlay_gizmos(&mut self) {
        self.refresh_gizmo();
    }

    fn place_selection_gizmo(&mut self, origin: [f32; 3]) {
        let layout = self.gizmo_layout_at(origin);
        self.gizmo = editor_gizmo::make(self.viewport_tool, origin, layout);
    }

    fn refresh_gizmo(&mut self) {
        if let Some(origin) = self.selection_world_origin() {
            self.place_selection_gizmo(origin);
        } else {
            self.gizmo = None;
        }
    }

    /// Applies authored local light direction through entity `Transform3d` /
    /// `WorldTransform3d` so rotating the light entity rotates the beam.
    fn sync_authored_lights(&mut self) {
        let Some(document) = self.authored_scene.as_ref().map(SceneSession::document) else {
            return;
        };
        let updates: Vec<(yuyib_ecs::bevy_ecs::entity::Entity, DirectionalLight3d)> = self
            .materialized_entities
            .iter()
            .filter_map(|(guid, entity)| {
                let record = document
                    .entities
                    .iter()
                    .find(|entity_record| &entity_record.guid == guid)?;
                let light_component = record.components.iter().find(|component| {
                    component.schema().as_str() == "yuyib.directional-light3d"
                })?;
                let local = directional_light_from_payload(light_component.payload()).ok()??;
                let world_dir = rotate_direction_by_entity(&self.world, *entity, local.direction());
                let world_light = local
                    .with_direction(world_dir)
                    .unwrap_or(local);
                Some((*entity, world_light))
            })
            .collect();
        for (entity, light) in updates {
            self.world.entity_mut(entity).insert(light);
        }
    }

    fn selection_light_cone_parts(&self) -> Vec<editor_gizmo::GizmoDrawPart> {
        let Some(selection_id) = self.selection.id.as_deref() else {
            return Vec::new();
        };
        let Ok(guid) = EntityGuid::from_str(selection_id) else {
            return Vec::new();
        };
        let Some(entity) = self.materialized_entities.get(&guid).copied() else {
            return Vec::new();
        };
        let Some(light) = self.world.get::<DirectionalLight3d>(entity) else {
            return Vec::new();
        };
        let Some(origin) = editor_gizmo::entity_world_translation(&self.world, entity) else {
            return Vec::new();
        };
        let unit = self.gizmo_layout_at(origin).unit;
        editor_gizmo::light_direction_parts(origin, light.direction(), unit)
    }

    fn pick_viewport_selection(&mut self, x: f64, y: f64) -> Option<String> {
        let viewport = self.viewport?;
        let logical = self.logical_viewport?;
        if !(0.0..logical.width).contains(&x) || !(0.0..logical.height).contains(&y) {
            return None;
        }
        self.apply_orbit_camera();
        let camera = *self.scene.camera_mut();
        let ray = viewport_ray_from_pointer(
            camera,
            [viewport.width(), viewport.height()],
            x,
            y,
            logical.width,
            logical.height,
        )
        .ok()?;
        if self.materialized_entities.is_empty() {
            let matrix = entity_model_matrix(&self.world, self.cube)?;
            return pick_closest_proxy(
                ray,
                &[(FOUNDATION_CUBE_SELECTION, matrix)],
                PROXY_CUBE_HALF_EXTENT,
            )
            .map(str::to_owned);
        }
        let mut targets = Vec::with_capacity(self.materialized_entities.len());
        for (guid, entity) in &self.materialized_entities {
            let Some(matrix) = entity_model_matrix(&self.world, *entity) else {
                continue;
            };
            targets.push((guid.to_string(), matrix));
        }
        let borrowed: Vec<(&str, [f32; 16])> = targets
            .iter()
            .map(|(id, matrix)| (id.as_str(), *matrix))
            .collect();
        pick_closest_proxy(ray, &borrowed, PROXY_CUBE_HALF_EXTENT).map(str::to_owned)
    }

    fn set_workspace_mode(&mut self, mode: WorkspaceMode) {
        self.mode = mode;
        // Scene and Preview use separate Game3dScene facades — no full cache flush.
        if mode != WorkspaceMode::Scene {
            self.orbit.dragging = false;
            self.clear_viewport_tool_drag();
        }
        if let Err(error) = self.apply_layout() {
            self.publish_diagnostic("error", "layout", &error.to_string());
        }
        self.emit_value(
            "host.process",
            json!({ "kind": "workspace", "status": "ready", "mode": mode }),
        );
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn apply_layout(&mut self) -> Result<(), Box<dyn Error>> {
        let Some(window) = &self.window else {
            return Ok(());
        };
        let layout = editor_layout(
            window,
            self.mode,
            self.logical_viewport,
            self.project.is_some(),
        )?;
        self.viewport = layout.viewport;
        if let Some(webview) = &self.webview {
            webview.set_bounds(layout.webview)?;
        }
        match (layout.viewport, self.viewport_window.as_ref()) {
            (Some(viewport), Some(child)) => {
                let placement = viewport_placement(Some(&viewport))?;
                child.set_child_placement(placement);
                if let Some(renderer) = &mut self.renderer {
                    renderer.resize(viewport.width(), viewport.height());
                }
            }
            (None, Some(child)) => child.hide(),
            _ => {}
        }
        Ok(())
    }

    fn handle_window_control(&mut self, request: WindowControlRequest) {
        let Some(window) = self.window.as_ref() else {
            return;
        };
        match request.action {
            WindowControlAction::Minimize => window.raw().set_minimized(true),
            WindowControlAction::Maximize => {
                window.raw().set_maximized(!window.raw().is_maximized());
            }
            WindowControlAction::Close => self.close_requested = true,
        }
    }

    fn read_source(&mut self, request: &SourceRequest) {
        match self.documents.load_text(&request.path) {
            Ok(snapshot) => self.emit_value(
                "host.source",
                json!({
                    "path": request.path,
                    "content": snapshot.value,
                    "revision": snapshot.revision.to_string(),
                    "saved": false,
                    "display_name": Path::new(&request.path).file_name().map_or_else(
                        || request.path.clone(),
                        |name| name.to_string_lossy().into_owned()
                    ),
                    "language": "rust",
                    "uri": format!("yuyib://project/{}", request.path.replace('\\', "/")),
                    "read_only": false
                }),
            ),
            Err(error) => self.publish_diagnostic("error", "source.read", &error.to_string()),
        }
    }

    fn save_source(&mut self, request: &SourceSaveRequest) {
        let expected = match request.revision.as_deref() {
            Some(revision) => match DocumentRevision::parse(revision) {
                Ok(revision) => Some(revision),
                Err(error) => {
                    self.publish_diagnostic("error", "source.save", &error.to_string());
                    return;
                }
            },
            None => None,
        };
        match self
            .documents
            .save_text(&request.path, &request.content, expected)
        {
            Ok(revision) => self.emit_value(
                "host.source",
                json!({
                    "path": request.path,
                    "content": request.content,
                    "revision": revision.to_string(),
                    "saved": true
                }),
            ),
            Err(DocumentError::Conflict(conflict)) => self.emit_value(
                "host.sourceConflict",
                json!({
                    "path": request.path,
                    "expected": conflict.expected.map(|value| value.to_string()),
                    "actual": conflict.actual.map(|value| value.to_string()),
                    "message": "The file changed outside the Editor; reload or compare before saving."
                }),
            ),
            Err(error) => self.publish_diagnostic("error", "source.save", &error.to_string()),
        }
    }

    fn open_scene(&mut self, request: SceneOpenRequest) {
        // Scene owns the viewport; never leave a prior glTF exclusive preview on top.
        self.gltf_preview = None;
        self.preview_scene.clear_model_caches();
        self.watch_scene_conflict_active = false;
        self.transform_drag = None;
        self.gizmo = None;
        self.materialized_entities.clear();
        self.reset_foundation_preview();
        match SceneSession::open(&self.documents, request.path) {
            Ok(scene) => {
                self.authored_scene = Some(scene);
                if self.mode != WorkspaceMode::Scene {
                    self.set_workspace_mode(WorkspaceMode::Scene);
                }
                self.publish_scene_state();
                self.frame_orbit_to_authored_content();
            }
            Err(error) => self.publish_diagnostic("error", "scene.open", &error.to_string()),
        }
    }

    fn create_scene(&mut self, request: SceneCreateRequest) {
        self.gltf_preview = None;
        self.preview_scene.clear_model_caches();
        self.transform_drag = None;
        self.gizmo = None;
        self.materialized_entities.clear();
        self.reset_foundation_preview();
        match SceneSession::create(request) {
            Ok(mut scene) => {
                if let Err(error) = scene.seed_starter_cube() {
                    self.publish_diagnostic("warning", "scene.create", &error.to_string());
                }
                self.authored_scene = Some(scene);
                if self.mode != WorkspaceMode::Scene {
                    self.set_workspace_mode(WorkspaceMode::Scene);
                }
                self.publish_scene_state();
                self.frame_orbit_to_authored_content();
            }
            Err(error) => self.publish_diagnostic("error", "scene.create", &error.to_string()),
        }
    }

    fn save_scene(&mut self, request: SceneSaveRequest) {
        let Some(scene) = &self.authored_scene else {
            self.publish_diagnostic("warning", "scene.save", "No authored scene is open.");
            return;
        };
        let stale_revision = request.expected_revision.and_then(|expected| {
            let actual = scene.history_revision().get();
            (expected != actual).then(|| (scene.path().to_owned(), expected, actual))
        });
        if let Some((path, expected, actual)) = stale_revision {
            self.emit_value(
                "host.scene.conflict",
                json!({
                    "path": path,
                    "expected_revision": expected,
                    "actual_revision": actual,
                    "message": "The page attempted to save a stale authoring revision."
                }),
            );
            self.publish_scene_history();
            return;
        }

        let result = self
            .authored_scene
            .as_mut()
            .expect("the scene was checked above")
            .save(&self.documents);
        match result {
            Ok(_) => {
                self.watch_scene_conflict_active = false;
                self.publish_scene_state();
            }
            Err(SceneSessionError::Document(DocumentError::Conflict(conflict))) => {
                self.watch_scene_conflict_active = true;
                let path = self
                    .authored_scene
                    .as_ref()
                    .expect("the conflicted scene remains open")
                    .path()
                    .to_owned();
                self.emit_value(
                    "host.scene.conflict",
                    json!({
                        "path": path,
                        "expected_revision": conflict.expected.map(|value| value.to_string()),
                        "actual_revision": conflict.actual.map(|value| value.to_string()),
                        "message": "The scene changed outside the Editor; reload or compare before saving."
                    }),
                );
                self.publish_scene_history();
            }
            Err(error) => self.publish_diagnostic("error", "scene.save", &error.to_string()),
        }
    }

    fn edit_scene(&mut self, mut request: SceneCommandRequest) {
        let known_entities = self.authored_scene.as_ref().map(|scene| {
            scene
                .document()
                .entities
                .iter()
                .map(|entity| entity.guid)
                .collect::<BTreeSet<_>>()
        });
        if let SceneEditRequest::SetComponentField {
            component_id,
            field_path,
            value,
            ..
        } = &mut request.command
        {
            if matches!(
                component_id.as_str(),
                "yuyib.transform3d" | "yuyib.local-transform3d"
            ) {
                if let Some(coerced) = coerce_transform_field_value(value) {
                    *value = coerced;
                }
            }
            if component_id == "yuyib.directional-light3d"
                && matches!(
                    field_path.as_str(),
                    "direction.x"
                        | "direction.y"
                        | "direction.z"
                        | "color.x"
                        | "color.y"
                        | "color.z"
                        | "illuminance_lux"
                        | "illuminance"
                )
            {
                // Mid-edit empty numeric field — ignore until the user finishes typing.
                if value
                    .as_str()
                    .is_some_and(|text| text.trim().is_empty())
                {
                    return;
                }
                if let Some(coerced) = coerce_transform_field_value(value) {
                    *value = coerced;
                }
            }
            if component_id == "yuyib.model3d" && field_path == "model" {
                if let Some(raw) = value.as_str() {
                    if let Some(canonical) = self.canonicalize_model_ref(raw) {
                        *value = Value::String(canonical);
                    }
                }
            }
            if let Err(message) = validate_component_field_edit(
                component_id,
                field_path,
                value,
                known_entities.as_ref(),
            ) {
                self.publish_diagnostic("warning", "scene.command", &message);
                return;
            }
        } else if let SceneEditRequest::AddComponent { component_id, .. } = &request.command {
            if let Err(message) = default_component_allowed(component_id) {
                self.publish_diagnostic("warning", "scene.command", &message);
                return;
            }
        }
        let preview = PreviewRefresh::from_edit(&request.command);
        let transaction_id = request.transaction_id.clone();
        let Some(scene) = &mut self.authored_scene else {
            self.publish_diagnostic("warning", "scene.command", "No authored scene is open.");
            return;
        };
        let result = scene.apply(request);
        match result {
            Ok(_) => {
                self.publish_scene_document(Some(&transaction_id));
                self.publish_scene_history_with_transaction(Some(&transaction_id));
                self.apply_preview_refresh(preview);
            }
            Err(SceneMutationError::Transaction(TransactionError::RevisionConflict {
                expected,
                actual,
            })) => {
                let path = self
                    .authored_scene
                    .as_ref()
                    .expect("the conflicted scene remains open")
                    .path()
                    .to_owned();
                self.emit_value(
                    "host.scene.conflict",
                    json!({
                        "path": path,
                        "expected_revision": expected.get(),
                        "actual_revision": actual.get(),
                        "transaction_id": transaction_id,
                        "message": "The page attempted to edit a stale authoring revision."
                    }),
                );
                self.publish_scene_history();
            }
            Err(error) => {
                self.publish_diagnostic("error", "scene.command", &error.to_string());
                self.publish_scene_history();
            }
        }
    }

    fn publish_scene_state(&mut self) {
        self.publish_scene_document(None);
        self.publish_scene_history();
        self.rebuild_transform_preview();
    }

    fn apply_preview_refresh(&mut self, preview: PreviewRefresh) {
        match preview {
            PreviewRefresh::None => {
                self.refresh_gizmo();
            }
            PreviewRefresh::LiveTransform { entity_guid } => {
                if let (Some(translation), Some(rotation), Some(scale)) = (
                    self.authored_translation(&entity_guid),
                    self.authored_rotation(&entity_guid),
                    self.authored_scale(&entity_guid),
                ) {
                    self.apply_live_transform(&entity_guid, translation, rotation, scale);
                } else {
                    self.refresh_gizmo();
                }
            }
            PreviewRefresh::EnsureEntities => {
                let spawned = self.ensure_missing_preview_entities();
                for guid in spawned {
                    let is_player = self
                        .entity_display_name(&guid)
                        .is_some_and(|name| name.eq_ignore_ascii_case("Player"));
                    if is_player {
                        let id = guid.to_string();
                        self.set_selection_id(Some(id));
                        self.frame_orbit_to_selection();
                        break;
                    }
                }
                self.refresh_gizmo();
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            PreviewRefresh::EntityModel { entity_guid } => {
                self.ensure_missing_preview_entities();
                self.refresh_entity_model(&entity_guid);
                self.refresh_gizmo();
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            PreviewRefresh::RemoveEntity { entity_guid } => {
                self.remove_preview_entity(&entity_guid);
                self.refresh_gizmo();
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            PreviewRefresh::Full => self.rebuild_transform_preview(),
        }
    }

    fn ensure_missing_preview_entities(&mut self) -> Vec<EntityGuid> {
        let Some(document) = self.authored_scene.as_ref().map(SceneSession::document) else {
            return Vec::new();
        };
        let missing: Vec<EntityGuid> = document
            .entities
            .iter()
            .map(|entity| entity.guid)
            .filter(|guid| !self.materialized_entities.contains_key(guid))
            .collect();
        let mut spawned = Vec::with_capacity(missing.len());
        for guid in missing {
            let guid_text = guid.to_string();
            let translation = self
                .authored_translation(&guid_text)
                .unwrap_or([0.0, 0.0, 0.0]);
            let rotation = self
                .authored_rotation(&guid_text)
                .unwrap_or([0.0, 0.0, 0.0, 1.0]);
            let scale = self.authored_scale(&guid_text).unwrap_or([1.0, 1.0, 1.0]);
            let entity = self
                .world
                .spawn((
                    Model3d::new(self.proxy_model),
                    Transform3d {
                        translation,
                        rotation,
                        scale,
                    },
                ))
                .id();
            self.materialized_entities.insert(guid, entity);
            self.refresh_entity_model(&guid_text);
            spawned.push(guid);
        }
        spawned
    }

    fn remove_preview_entity(&mut self, entity_guid: &str) {
        let Ok(guid) = EntityGuid::from_str(entity_guid) else {
            return;
        };
        let Some(entity) = self.materialized_entities.remove(&guid) else {
            return;
        };
        let _ = self.world.despawn(entity);
    }

    fn refresh_entity_model(&mut self, entity_guid: &str) {
        let Ok(guid) = EntityGuid::from_str(entity_guid) else {
            return;
        };
        let Some(entity) = self.materialized_entities.get(&guid).copied() else {
            return;
        };
        let Some(payload) = self.authored_scene.as_ref().and_then(|scene| {
            scene
                .document()
                .entities
                .iter()
                .find(|entity_record| entity_record.guid == guid)
                .and_then(|record| {
                    record
                        .components
                        .iter()
                        .find(|component| component.schema().as_str() == "yuyib.model3d")
                        .map(|component| component.payload().clone())
                })
        }) else {
            return;
        };
        let mesh = payload
            .get("mesh")
            .and_then(Value::as_u64)
            .map(|value| value as usize);
        let model_path = extract_model_path(&payload);
        let prefer_hierarchy = mesh.is_none()
            && model_path.as_deref().is_some_and(|path| {
                path_looks_like_gltf(path)
                    || self
                        .resolve_project_model_path(path)
                        .is_some_and(|absolute| {
                            absolute
                                .extension()
                                .and_then(|ext| ext.to_str())
                                .is_some_and(|ext| {
                                    matches!(ext.to_ascii_lowercase().as_str(), "glb" | "gltf")
                                })
                        })
            });
        if prefer_hierarchy {
            let mut world = std::mem::take(&mut self.world);
            let attached = self
                .attach_gltf_hierarchy(&mut world, entity, &payload)
                .unwrap_or(false);
            if !attached {
                world
                    .entity_mut(entity)
                    .insert(Model3d::new(self.proxy_model));
            }
            self.world = world;
            return;
        }
        let handle = self
            .resolve_model_handle(&payload)
            .unwrap_or(self.proxy_model);
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
        self.world.entity_mut(entity).insert(model);
    }

    fn canonicalize_model_ref(&mut self, raw: &str) -> Option<String> {
        let trimmed = raw
            .strip_prefix("asset://")
            .unwrap_or(raw)
            .trim()
            .trim_start_matches(['/', '\\']);
        if trimmed.is_empty() {
            return None;
        }
        if trimmed.eq_ignore_ascii_case("builtin:cube") {
            return Some("builtin:cube".to_owned());
        }
        // Persist tracked identity, never collapse GUID → path.
        if looks_like_asset_guid(trimmed) {
            let _absolute = self.resolve_project_model_path(trimmed)?;
            return Some(format!("asset://{trimmed}"));
        }
        if let Some(guid) = self.find_tracked_guid_for_source(trimmed) {
            return Some(format!("asset://{guid}"));
        }
        let absolute = self.resolve_project_model_path(trimmed)?;
        let relative = absolute
            .strip_prefix(&self.project_root)
            .unwrap_or(absolute.as_path());
        Some(relative.to_string_lossy().replace('\\', "/"))
    }

    /// Maps a project/asset-root-relative glTF path to its tracked AssetGuid.
    fn find_tracked_guid_for_source(&self, path_or_under: &str) -> Option<String> {
        let index = self.asset_index.as_ref()?;
        let project = self.project.as_ref()?;
        let asset_root = project.asset_root.replace('\\', "/");
        let normalized = path_or_under.replace('\\', "/");
        let under = normalized
            .strip_prefix(&format!("{asset_root}/"))
            .unwrap_or(normalized.as_str());
        let project_source = if asset_root.is_empty() {
            under.to_owned()
        } else {
            format!("{asset_root}/{under}")
        };
        for item in &index.items {
            if let Some(metadata) = &item.metadata {
                let source = metadata.source.replace('\\', "/");
                if source == project_source || source == under || source == normalized {
                    return Some(metadata.guid.to_string());
                }
            }
            if item.kind == AssetKind::GltfSource
                && (item.path == under || item.path == normalized)
                && let Some(guid) = item.id
            {
                return Some(guid.to_string());
            }
        }
        None
    }

    fn rebuild_transform_preview(&mut self) {
        let Some(document) = self.authored_scene.as_ref().map(SceneSession::document) else {
            self.reset_foundation_preview();
            return;
        };
        let document = document.clone();
        match materialize_transform_scene(&document) {
            Ok(materialized) if !materialized.entities.is_empty() => {
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
                        let model_path = extract_model_path(payload);
                        let prefer_hierarchy = mesh.is_none()
                            && model_path.as_deref().is_some_and(|path| {
                                path_looks_like_gltf(path)
                                    || self
                                        .resolve_project_model_path(path)
                                        .is_some_and(|absolute| {
                                            absolute
                                                .extension()
                                                .and_then(|ext| ext.to_str())
                                                .is_some_and(|ext| {
                                                    matches!(
                                                        ext.to_ascii_lowercase().as_str(),
                                                        "glb" | "gltf"
                                                    )
                                                })
                                        })
                            });
                        if prefer_hierarchy {
                            let attached = self
                                .attach_gltf_hierarchy(&mut world, *entity, payload)
                                .unwrap_or(false);
                            if !attached {
                                world
                                    .entity_mut(*entity)
                                    .insert(Model3d::new(self.proxy_model));
                            }
                        } else {
                            let handle = self
                                .resolve_model_handle(payload)
                                .unwrap_or(self.proxy_model);
                            let mut model = Model3d::new(handle);
                            if let Some(visible) = payload.get("visible").and_then(Value::as_bool) {
                                model = model.with_visible(visible);
                            }
                            if let Some(mesh) = mesh {
                                model = model.with_mesh(mesh);
                            }
                            if let Some(order) = payload.get("render_order").and_then(Value::as_i64)
                            {
                                model = model.with_render_order(order as i32);
                            }
                            world.entity_mut(*entity).insert(model);
                        }
                    }

                    if let Some(light_component) = record.components.iter().find(|component| {
                        component.schema().as_str() == "yuyib.directional-light3d"
                    }) {
                        match directional_light_from_payload(light_component.payload()) {
                            Ok(Some(light)) => {
                                // Local-space direction; world direction applied after propagate.
                                world.entity_mut(*entity).insert(light);
                            }
                            Ok(None) => {
                                world.entity_mut(*entity).remove::<DirectionalLight3d>();
                            }
                            Err(error) => self.publish_diagnostic(
                                "warning",
                                "preview.materialize",
                                &format!("Could not materialize light on {guid}: {error}"),
                            ),
                        }
                    }
                }
                if let Err(error) = propagate_world_transforms(&mut world) {
                    self.publish_diagnostic(
                        "error",
                        "preview.hierarchy",
                        &format!("3D hierarchy propagation failed: {error}"),
                    );
                    self.reset_foundation_preview();
                    return;
                }
                self.world = world;
                self.materialized_entities = materialized.entities;
                self.sync_authored_lights();
                self.refresh_gizmo();
                self.refresh_selection_translation();
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            Ok(_) => self.reset_foundation_preview(),
            Err(error) => {
                self.publish_diagnostic("warning", "preview.materialize", &error.to_string());
                self.reset_foundation_preview();
            }
        }
    }

    fn attach_gltf_hierarchy(
        &mut self,
        world: &mut World,
        root: yuyib_ecs::bevy_ecs::entity::Entity,
        payload: &Value,
    ) -> Result<bool, ()> {
        let Some(raw) = extract_model_path(payload) else {
            return Ok(false);
        };
        if raw.eq_ignore_ascii_case("builtin:cube") {
            return Ok(false);
        }
        let Some(absolute) = self.resolve_project_model_path(&raw) else {
            return Ok(false);
        };
        let extension = absolute
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !matches!(extension.as_str(), "glb" | "gltf") {
            return Ok(false);
        }
        let path = raw.clone();
        if let Some(imported) = self.gltf_import_cache.get(&path).cloned() {
            return self.spawn_cached_gltf(world, root, &path, &imported);
        }
        if self.gltf_import_inflight.contains(&path) {
            return Ok(false);
        }
        self.gltf_import_inflight.insert(path.clone());
        self.emit_value(
            "host.process",
            json!({
                "kind": "preview",
                "status": "progress",
                "stage": "import",
                "completed": 0.2,
                "path": path,
            }),
        );
        let sender = self.job_sender.clone();
        let absolute_for_job = absolute;
        let path_for_job = path;
        let cook_root = editor_cook_cache_root(&self.project_root);
        thread::spawn(move || {
            let (result, cook_hit) = match import_gltf_with_cook_cache(&absolute_for_job, &cook_root)
            {
                Ok((imported, hit)) => (Ok(imported), hit),
                Err(error) => (Err(error), false),
            };
            let _ = sender.try_send(EditorJob::GltfImported {
                path: path_for_job,
                result,
                cook_hit,
            });
        });
        Ok(false)
    }

    fn spawn_cached_gltf(
        &mut self,
        world: &mut World,
        root: yuyib_ecs::bevy_ecs::entity::Entity,
        path: &str,
        imported: &yuyib_gltf::ImportedAsset,
    ) -> Result<bool, ()> {
        let model = match self.gltf_model_handles.get(path).copied() {
            Some(handle) if self.models.get(handle).is_some() => handle,
            _ => {
                let handle = self.models.insert(imported.model.clone());
                self.gltf_model_handles.insert(path.to_owned(), handle);
                handle
            }
        };
        let spawned = match spawn_scene_with_model(
            world,
            model,
            imported,
            SceneSelection::Default,
        ) {
            Ok(spawned) => spawned,
            Err(error) => {
                self.publish_diagnostic(
                    "warning",
                    "preview.model3d",
                    &format!("Could not spawn glTF hierarchy `{path}`: {error}"),
                );
                return Ok(false);
            }
        };
        if spawned.roots().is_empty() {
            self.publish_diagnostic(
                "warning",
                "preview.model3d",
                &format!("glTF `{path}` spawned with zero scene roots"),
            );
            return Ok(false);
        }
        for child in spawned.roots() {
            if let Err(error) = set_parent_3d(world, *child, root) {
                self.publish_diagnostic(
                    "warning",
                    "preview.model3d",
                    &format!("Could not parent glTF root under `{path}`: {error}"),
                );
            }
        }
        Ok(true)
    }

    fn drain_editor_jobs(&mut self) {
        let jobs: Vec<_> = self.job_receiver.try_iter().collect();
        let mut need_rebuild = false;
        for job in jobs {
            match job {
                EditorJob::GltfImported {
                    path,
                    result,
                    cook_hit,
                } => {
                    self.gltf_import_inflight.remove(&path);
                    match result {
                        Ok(imported) => {
                            self.gltf_import_cache
                                .insert(path.clone(), Arc::new(imported));
                            self.emit_value(
                                "host.process",
                                json!({
                                    "kind": "preview",
                                    "status": "scene_model_ready",
                                    "path": path,
                                    "completed": 1.0,
                                    "cook_hit": cook_hit,
                                }),
                            );
                            if cook_hit {
                                eprintln!(
                                    "yuyib-editor: glTF cook cache hit for `{path}` (parse skipped)"
                                );
                            }
                            need_rebuild = true;
                        }
                        Err(error) => {
                            self.publish_diagnostic(
                                "warning",
                                "preview.model3d",
                                &format!("Background import failed for `{path}`: {error}"),
                            );
                            self.emit_value(
                                "host.process",
                                json!({
                                    "kind": "preview",
                                    "status": "failed",
                                    "stage": "import",
                                    "path": path,
                                    "message": error,
                                }),
                            );
                        }
                    }
                }
            }
        }
        if need_rebuild {
            self.rebuild_transform_preview();
        }
    }

    fn resolve_model_handle(&mut self, payload: &Value) -> Option<ModelHandle> {
        let raw = extract_model_path(payload)?;
        if raw.is_empty() || raw.eq_ignore_ascii_case("builtin:cube") {
            return Some(self.proxy_model);
        }
        if path_looks_like_gltf(&raw) {
            return None;
        }
        if let Some(handle) = self.model_cache.get(&raw) {
            return Some(*handle);
        }
        let absolute = self.resolve_project_model_path(&raw)?;
        if absolute
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "glb" | "gltf"))
        {
            return None;
        }
        self.publish_diagnostic(
            "warning",
            "preview.model3d",
            &format!("Model path `{raw}` is not glTF; using proxy cube."),
        );
        Some(self.proxy_model)
    }

    fn resolve_project_model_path(&mut self, path: &str) -> Option<PathBuf> {
        let path = path
            .strip_prefix("asset://")
            .unwrap_or(path)
            .trim_start_matches(['/', '\\']);
        // Tracked Model3d refs: `asset://{AssetGuid}` → `.yasset.source`.
        if looks_like_asset_guid(path) {
            if let Some(source) = self.resolve_tracked_asset_source(path) {
                return self.resolve_project_model_path(&source);
            }
            self.publish_diagnostic(
                "warning",
                "preview.model3d",
                &format!(
                    "Tracked asset `{path}` is not in the asset index; Track the .glb or refresh Assets."
                ),
            );
            return None;
        }
        let mut candidates = Vec::new();
        candidates.push(self.project_root.join(path));
        candidates.push(self.project_root.join("assets").join(path));
        if let Some(project) = &self.project {
            let asset_root = project.asset_root.trim().trim_matches(['/', '\\']);
            if !asset_root.is_empty() {
                candidates.push(self.project_root.join(asset_root).join(path));
                if let Some(stripped) = path.strip_prefix(asset_root) {
                    let stripped = stripped.trim_start_matches(['/', '\\']);
                    if !stripped.is_empty() {
                        candidates.push(self.project_root.join(asset_root).join(stripped));
                    }
                }
            }
        }
        // If author omitted extension, try common glTF suffixes.
        if Path::new(path).extension().is_none() {
            for base in candidates.clone() {
                candidates.push(base.with_extension("glb"));
                candidates.push(base.with_extension("gltf"));
            }
        }
        if let Some(found) = candidates.into_iter().find(|candidate| candidate.is_file()) {
            return Some(found);
        }
        self.publish_diagnostic(
            "warning",
            "preview.model3d",
            &format!("Model path `{path}` is not a file under the project; using proxy cube."),
        );
        None
    }

    fn resolve_tracked_asset_source(&self, guid_text: &str) -> Option<String> {
        let index = self.asset_index.as_ref()?;
        let guid_text = guid_text.strip_prefix("asset://").unwrap_or(guid_text);
        for item in &index.items {
            let Some(metadata) = &item.metadata else {
                continue;
            };
            if metadata.guid.to_string() == guid_text {
                return Some(metadata.source.replace('\\', "/"));
            }
            if item
                .id
                .as_ref()
                .is_some_and(|guid| guid.to_string() == guid_text)
            {
                return Some(metadata.source.replace('\\', "/"));
            }
        }
        // Tracked glTF source cards carry the GUID on the source item itself.
        for item in &index.items {
            if item.kind != AssetKind::GltfSource {
                continue;
            }
            if item
                .id
                .as_ref()
                .is_some_and(|guid| guid.to_string() == guid_text)
            {
                let project = self.project.as_ref()?;
                let asset_root = project.asset_root.replace('\\', "/");
                return Some(if asset_root.is_empty() {
                    item.path.clone()
                } else {
                    format!("{asset_root}/{}", item.path)
                });
            }
        }
        None
    }

    fn reset_foundation_preview(&mut self) {
        let mut world = World::new();
        let cube = world
            .spawn((Model3d::new(self.proxy_model), Transform3d::default()))
            .id();
        self.world = world;
        self.cube = cube;
        self.materialized_entities.clear();
        self.gizmo = None;
        // Static placeholder — never idle-spin; that masked real scene transforms.
        self.orbit = ViewportOrbit::default();
        self.apply_orbit_camera();
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn frame_orbit_to_authored_content(&mut self) {
        if self.materialized_entities.is_empty() {
            self.orbit = ViewportOrbit::default();
            self.apply_orbit_camera();
            return;
        }
        match scene_bounds_3d(&mut self.world, &self.models) {
            Ok(SceneBoundsResult3d::Bounds(bounds)) => {
                let radius = bounds.radius().max(1.0);
                self.orbit.target = bounds.centre();
                self.orbit.radius = (radius * 2.6).clamp(2.0, 5_000.0);
                self.orbit.near = (self.orbit.radius * 0.0005).max(0.05);
                self.orbit.far = (self.orbit.radius * 40.0).max(self.orbit.near * 200.0);
                self.apply_orbit_camera();
                self.sync_overlay_gizmos();
                self.emit_viewport_orbit();
            }
            Ok(SceneBoundsResult3d::Empty) | Err(_) => {
                let mut min = [f32::INFINITY; 3];
                let mut max = [f32::NEG_INFINITY; 3];
                let mut any = false;
                for entity in self.materialized_entities.values().copied() {
                    let Some(transform) = self.world.get::<Transform3d>(entity) else {
                        continue;
                    };
                    any = true;
                    for axis in 0..3 {
                        min[axis] = min[axis].min(transform.translation[axis]);
                        max[axis] = max[axis].max(transform.translation[axis]);
                    }
                }
                if !any {
                    self.orbit = ViewportOrbit::default();
                    self.apply_orbit_camera();
                    return;
                }
                let target = [
                    (min[0] + max[0]) * 0.5,
                    (min[1] + max[1]) * 0.5,
                    (min[2] + max[2]) * 0.5,
                ];
                let extent = (max[0] - min[0])
                    .max(max[1] - min[1])
                    .max(max[2] - min[2])
                    .max(1.0);
                self.orbit.target = target;
                self.orbit.radius = (extent * 2.4).clamp(2.0, 5_000.0);
                self.orbit.near = (self.orbit.radius * 0.0005).max(0.05);
                self.orbit.far = (self.orbit.radius * 40.0).max(self.orbit.near * 200.0);
                self.apply_orbit_camera();
                self.sync_overlay_gizmos();
                self.emit_viewport_orbit();
            }
        }
    }

    fn publish_scene_document(&mut self, transaction_id: Option<&str>) {
        let Some(scene) = &self.authored_scene else {
            return;
        };
        let payload = scene_document_payload(scene, transaction_id);
        self.emit_value("host.scene.document", payload);
    }

    fn publish_scene_history(&mut self) {
        self.publish_scene_history_with_transaction(None);
    }

    fn publish_scene_history_with_transaction(&mut self, transaction_id: Option<&str>) {
        let Some(scene) = &self.authored_scene else {
            return;
        };
        let payload = json!({
            "path": scene.path(),
            "revision": scene.history_revision().get(),
            "dirty": scene.is_dirty(),
            "can_undo": scene.undo_len() > 0,
            "can_redo": scene.redo_len() > 0,
            "poisoned": scene.history_poisoned(),
            "read_only": scene.is_read_only(),
            "transaction_id": transaction_id
        });
        self.emit_value("host.scene.history", payload);
    }

    fn start_play(&mut self) {
        if self.play.is_some() || self.play_launch_after_build {
            self.publish_diagnostic(
                "warning",
                "play",
                "Play Mode is already running or building.",
            );
            return;
        }
        if self.project.is_none() {
            self.publish_diagnostic("warning", "play", "Open a project before Play.");
            return;
        }
        // Flush coerced TRS (`"0"` → 0.0) to disk so yuyib-play reads clean numbers.
        if self
            .authored_scene
            .as_ref()
            .is_some_and(SceneSession::is_dirty)
        {
            let save = SceneSaveRequest {
                expected_revision: self
                    .authored_scene
                    .as_ref()
                    .map(|scene| scene.history_revision().get()),
            };
            self.save_scene(save);
            if self
                .authored_scene
                .as_ref()
                .is_some_and(SceneSession::is_dirty)
            {
                self.publish_diagnostic(
                    "warning",
                    "play",
                    "Scene has unsaved edits; save before Play so the runner sees the latest Transform3d values.",
                );
                return;
            }
        }
        let mut arguments = match self
            .project
            .as_ref()
            .expect("project checked above")
            .build_play_argv()
        {
            Ok(arguments) => arguments,
            Err(error) => {
                self.publish_diagnostic("error", "play", &error.to_string());
                return;
            }
        };
        if let Some(scene) = &self.authored_scene {
            strip_play_scene_args(&mut arguments);
            arguments.push("--scene".to_owned());
            arguments.push(scene.path().to_owned());
            arguments.push("--scene-revision".to_owned());
            arguments.push(scene.history_revision().get().to_string());
            if let Some(file_revision) = scene.file_revision() {
                arguments.push("--scene-file-revision".to_owned());
                arguments.push(file_revision.to_string());
            }
        }
        let pin = self.authored_scene.as_ref().map(|scene| {
            json!({
                "path": scene.path(),
                "revision": scene.history_revision().get(),
                "file_revision": scene.file_revision().map(|revision| revision.to_string()),
            })
        });
        let package = self
            .project
            .as_ref()
            .expect("project checked above")
            .development
            .cargo_package
            .clone();
        match self.resolve_play_executable(package.as_deref()) {
            Ok(executable) => self.launch_play_executable(executable, arguments, pin),
            Err(missing) => self.publish_diagnostic("error", "play", &missing),
        }
    }

    fn launch_play_executable(
        &mut self,
        executable: PathBuf,
        mut arguments: Vec<String>,
        pin: Option<Value>,
    ) {
        let is_engine_player = executable
            .file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| stem.eq_ignore_ascii_case("yuyib-play"));
        if is_engine_player {
            strip_flag_pair(&mut arguments, "--project");
            arguments.insert(0, "--project".to_owned());
            arguments.insert(1, self.project_root.to_string_lossy().into_owned());
            if !arguments.iter().any(|argument| argument == "--scene") {
                if let Some(scene) = &self.authored_scene {
                    arguments.push("--scene".to_owned());
                    arguments.push(scene.path().to_owned());
                } else if let Some(path) = self
                    .project
                    .as_ref()
                    .and_then(|project| project.startup_scene_path().ok().flatten())
                {
                    arguments.push("--scene".to_owned());
                    arguments.push(path.to_owned());
                }
            }
        }
        let started = if is_engine_player {
            ManagedProcess::start_play_engine_runner(&executable, &arguments, &self.project_root)
        } else {
            ManagedProcess::start_play(&executable, &arguments, &self.project_root)
        };
        match started {
            Ok(mut process) => {
                attach_process_output(&mut process, "play", &self.process_sender);
                self.play = Some(process);
                self.play_pin = pin.clone();
                self.play_launch_after_build = false;
                self.emit_value(
                    "host.process",
                    json!({
                        "kind": "play",
                        "status": "playing",
                        "args": arguments,
                        "pinned_scene": pin,
                        "via": if is_engine_player { "yuyib-play" } else { "executable" },
                        "executable": executable.to_string_lossy(),
                        "apply_play_changes": false
                    }),
                );
            }
            Err(error) => {
                self.play_launch_after_build = false;
                self.play_pin = None;
                self.publish_diagnostic("error", "play", &error.to_string());
            }
        }
    }

    fn resolve_play_executable(&self, package: Option<&str>) -> Result<PathBuf, String> {
        if let Some(project) = &self.project
            && let Some(executable) = project.development.play_executable.as_ref()
        {
            return confined_existing_file(&self.project_root, executable);
        }
        // Engine player loads .yscene. Scaffold stub println! binaries are not Play.
        if let Some(runner) = find_engine_play_runner() {
            return Ok(runner);
        }
        let package = package.ok_or_else(|| {
            "yuyib-play runner not found. From the engine repo run: cargo build -p yuyib-play"
                .to_owned()
        })?;
        if let Some(binary) = find_package_binary(&self.project_root, package) {
            if project_main_looks_like_stub(&self.project_root) {
                return Err(
                    "yuyib-play not built, and project src/main.rs is still the scaffold stub. Run `cargo build -p yuyib-play` in the engine repo."
                        .to_owned(),
                );
            }
            return Ok(binary);
        }
        Err(format!(
            "yuyib-play not found, and `{package}` binary missing under target/ (including target/<triple>/debug|release)"
        ))
    }

    fn stop_play(&mut self) {
        self.play_launch_after_build = false;
        self.pending_play_args = None;
        let Some(process) = self.play.take() else {
            self.publish_diagnostic("info", "play", "Play Mode is not running.");
            return;
        };
        let pin = self.play_pin.take();
        self.emit_value(
            "host.process",
            json!({
                "kind": "play",
                "status": "stopped",
                "success": true,
                "code": null,
                "reason": "user_stop",
                "pinned_scene": pin,
                "apply_play_changes": false
            }),
        );
        stop_process_async(process, "play", self.process_sender.clone());
    }

    fn start_cargo_check(&mut self, request: &CargoCheckRequest) {
        if self.cargo_check.is_some() {
            self.publish_diagnostic(
                "warning",
                "cargo",
                "A scoped Cargo check is already running.",
            );
            return;
        }
        match ManagedProcess::start_cargo_check(request.package.clone(), &self.project_root) {
            Ok(mut process) => {
                attach_process_output(&mut process, "cargo.check", &self.process_sender);
                self.cargo_check = Some(process);
                self.cargo_package = Some(request.package.clone());
                self.emit_value(
                    "host.process",
                    json!({ "kind": "cargo", "status": "checking", "package": request.package, "completed": 0.05 }),
                );
            }
            Err(error) => self.publish_diagnostic("error", "cargo", &error.to_string()),
        }
    }

    fn drain_process_output(&mut self) {
        let messages: Vec<_> = self.process_receiver.try_iter().collect();
        for output in messages {
            self.emit_value(
                "host.diagnostics",
                json!({
                    "diagnostics": [{
                        "severity": if output.stream == "stderr" { "warning" } else { "info" },
                        "source": output.process,
                        "stream": output.stream,
                        "message": output.line
                    }]
                }),
            );
        }
    }

    fn poll_processes(&mut self) {
        if let Some(result) = poll_process(&mut self.play) {
            match result {
                ProcessCompletion::Exited(status) => {
                    self.report_process_result("play", Ok(status));
                }
                ProcessCompletion::TimedOut(status) => {
                    let pin = self.play_pin.take();
                    self.emit_value(
                        "host.process",
                        json!({
                            "kind": "play",
                            "status": "timeout",
                            "success": false,
                            "code": status.code(),
                            "reason": "timeout",
                            "pinned_scene": pin,
                            "apply_play_changes": false
                        }),
                    );
                }
                ProcessCompletion::PollFailed { error, process } => {
                    self.report_process_result("play", Err(error));
                    stop_process_async(process, "play", self.process_sender.clone());
                }
            }
        }
        if let Some(result) = poll_process(&mut self.cargo_check) {
            let package = self.cargo_package.take().unwrap_or_default();
            let launch_play = self.play_launch_after_build;
            match result {
                ProcessCompletion::Exited(status) => {
                    self.emit_value(
                        "host.process",
                        json!({
                            "kind": "cargo",
                            "status": if status.success() { "success" } else { "error" },
                            "package": package,
                            "completed": 1.0,
                            "success": status.success(),
                            "code": status.code(),
                            "warnings": 0
                        }),
                    );
                    if launch_play {
                        self.play_launch_after_build = false;
                        let pending = self.pending_play_args.take();
                        if status.success() {
                            match self.resolve_play_executable(Some(package.as_str())) {
                                Ok(executable) => {
                                    let (arguments, pin) = pending.unwrap_or_default();
                                    self.launch_play_executable(executable, arguments, pin);
                                }
                                Err(error) => self.publish_diagnostic(
                                    "error",
                                    "play",
                                    &format!("Build finished but binary is missing: {error}"),
                                ),
                            }
                        } else {
                            self.publish_diagnostic(
                                "error",
                                "play",
                                "Play build failed — fix compile errors, then press Play again.",
                            );
                        }
                    }
                }
                ProcessCompletion::TimedOut(status) => {
                    self.play_launch_after_build = false;
                    self.pending_play_args = None;
                    self.emit_value(
                        "host.process",
                        json!({
                            "kind": "cargo",
                            "status": "timeout",
                            "package": package,
                            "completed": 1.0,
                            "success": false,
                            "code": status.code(),
                            "warnings": 0,
                            "message": "Scoped Cargo check exceeded its 15-minute deadline."
                        }),
                    );
                }
                ProcessCompletion::PollFailed { error, process } => {
                    self.play_launch_after_build = false;
                    self.pending_play_args = None;
                    self.publish_diagnostic("error", "cargo.check", &error);
                    stop_process_async(process, "cargo.check", self.process_sender.clone());
                }
            }
        }
    }

    fn report_process_result(
        &mut self,
        kind: &'static str,
        result: Result<std::process::ExitStatus, String>,
    ) {
        match result {
            Ok(status) => {
                let mut payload = json!({
                    "kind": kind,
                    "status": "stopped",
                    "success": status.success(),
                    "code": status.code(),
                    "apply_play_changes": false
                });
                if kind == "play" {
                    payload["pinned_scene"] = self.play_pin.take().unwrap_or(Value::Null);
                    payload["reason"] = json!(if status.success() {
                        "exited"
                    } else {
                        "exited_error"
                    });
                }
                self.emit_value("host.process", payload);
            }
            Err(error) => {
                if kind == "play" {
                    let pin = self.play_pin.take();
                    self.emit_value(
                        "host.process",
                        json!({
                            "kind": "play",
                            "status": "error",
                            "success": false,
                            "code": null,
                            "reason": "poll_failed",
                            "pinned_scene": pin,
                            "apply_play_changes": false,
                            "message": error.clone()
                        }),
                    );
                }
                self.publish_diagnostic("error", kind, &error);
            }
        }
    }

    /// Bounded project watch: external scene edits → conflict dialog (never silent
    /// reload); asset-index fingerprint drift → refresh `host.assets`.
    fn poll_project_watch(&mut self) {
        if self.project.is_none() {
            return;
        }
        let now = Instant::now();
        if self
            .watch_last_poll
            .is_some_and(|last| now.duration_since(last) < std::time::Duration::from_millis(750))
        {
            return;
        }
        self.watch_last_poll = Some(now);
        self.poll_open_scene_external_change();
        self.poll_asset_index_drift();
    }

    fn poll_open_scene_external_change(&mut self) {
        if self.watch_scene_conflict_active {
            return;
        }
        let Some(scene) = &self.authored_scene else {
            return;
        };
        let Some(expected) = scene.file_revision() else {
            return;
        };
        let path = scene.path().to_owned();
        match self.documents.peek_revision(&path) {
            Ok(Some(actual)) if actual == expected => {}
            Ok(actual) => {
                self.watch_scene_conflict_active = true;
                let dirty = self
                    .authored_scene
                    .as_ref()
                    .is_some_and(SceneSession::is_dirty);
                self.emit_value(
                    "host.scene.conflict",
                    json!({
                        "path": path,
                        "expected": expected.to_string(),
                        "actual": actual.map(|revision| revision.to_string()),
                        "dirty": dirty,
                        "source": "watch",
                        "message": if dirty {
                            "The scene file changed on disk while you have unsaved edits. Reload discards local edits."
                        } else {
                            "The scene file changed on disk. Reload to pick up the external revision (silent reload is disabled)."
                        }
                    }),
                );
            }
            Err(error) => {
                self.publish_diagnostic("warning", "watch.scene", &error.to_string());
            }
        }
    }

    fn poll_asset_index_drift(&mut self) {
        let Some(project) = self.project.clone() else {
            return;
        };
        let asset_root = project.asset_root.clone();
        match build_asset_index(&self.documents, &asset_root) {
            Ok(index) => {
                let revision = index.revision;
                if self.watch_asset_revision == Some(revision) {
                    return;
                }
                let previous = self.watch_asset_revision;
                self.watch_asset_revision = Some(revision);
                if previous.is_none() {
                    // First sample after open — publish_asset_index already ran.
                    self.asset_index = Some(index);
                    return;
                }
                let mut payload = asset_index_payload(&index, "ready");
                merge_project_scenes_into_assets(&mut payload, &project);
                self.asset_index = Some(index);
                self.emit_value("host.assets", payload);
                self.publish_diagnostic(
                    "info",
                    "watch.assets",
                    "Asset index refreshed after on-disk change.",
                );
            }
            Err(error) => {
                self.publish_diagnostic("warning", "watch.assets", &error.to_string());
            }
        }
    }

    fn observe_bridge_failures(&mut self) {
        let Some(failures) = &self.bridge_failures else {
            return;
        };
        let Ok(mut shared) = failures.try_borrow_mut() else {
            return;
        };
        if shared.is_empty() {
            return;
        }
        let batch: Vec<String> = shared.drain(..).collect();
        drop(shared);
        for error in batch {
            eprintln!("yuyib-editor: bridge IPC failure: {error}");
            self.publish_diagnostic("error", "bridge.dispatch", &error);
        }
    }

    fn observe_command_overflow(&mut self) {
        let Some(counter) = &self.dropped_commands else {
            return;
        };
        let current = counter.get();
        if current > self.observed_dropped_commands {
            let dropped = current - self.observed_dropped_commands;
            self.observed_dropped_commands = current;
            eprintln!("yuyib-editor: dropped {dropped} UI command(s); queue full");
            self.publish_diagnostic(
                "warning",
                "bridge",
                &format!("Dropped {dropped} UI commands because the bounded host queue was full."),
            );
        }
    }

    #[allow(clippy::result_large_err)]
    fn render_preview(&mut self) {
        if !matches!(self.mode, WorkspaceMode::Scene | WorkspaceMode::Preview)
            || self.occluded
            || self.viewport.is_none()
        {
            return;
        }
        self.last_frame = Instant::now();
        self.apply_orbit_camera();
        if matches!(self.mode, WorkspaceMode::Scene) {
            self.sync_authored_lights();
        }
        let light_cone_parts = if matches!(self.mode, WorkspaceMode::Scene) {
            self.selection_light_cone_parts()
        } else {
            Vec::new()
        };

        let mut preview = self.gltf_preview.take();
        let gizmo_unlit = self.gizmo_unlit.take();
        let gizmo_state = self.gizmo;
        let mode = self.mode;
        let preview_overlay_bounds = self.preview_overlay_bounds;
        let preview_overlay_collision = self.preview_overlay_collision;
        let preview_overlay_normals = self.preview_overlay_normals;
        let preview_overlay_tangents = self.preview_overlay_tangents;
        let preview_overlay_uv = self.preview_overlay_uv;
        let orbit_radius = self.orbit.radius;
        let scene = &mut self.scene;
        let preview_scene = &mut self.preview_scene;
        let world = &mut self.world;
        let models = &self.models;
        let Some(renderer) = &mut self.renderer else {
            self.gltf_preview = preview;
            self.gizmo_unlit = gizmo_unlit;
            return;
        };
        let camera: Camera3d = *scene.camera_mut();
        let mut scene_error: Option<String> = None;
        let surface_result =
            renderer.render_frame(ClearColor::linear(0.012, 0.018, 0.032, 1.0), |frame| {
                let result = match mode {
                    // Asset Preview owns the hole: upload + draw into preview_scene only.
                    WorkspaceMode::Preview => match preview.as_mut() {
                        None => Ok(()),
                        Some(session) if !session.is_cpu_ready() => Ok(()),
                        Some(session) => match session.render(frame, preview_scene) {
                            Ok(Some(GltfPreviewFrame::Drawn { .. })) => {
                                let mut result = Ok(());
                                let overlay_camera = *preview_scene.camera_mut();
                                if let Some(pass) = gizmo_unlit.as_ref() {
                                    // Bounds first (≤12 draws). Collision/Normals are
                                    // chunked in GizmoUnlitPass; cheap overlays stay
                                    // visible even if a denser pass errors.
                                    if preview_overlay_bounds {
                                        if let Some(bounds) = session.bounds() {
                                            let parts = editor_gizmo::bounds_box_parts(
                                                bounds.minimum(),
                                                bounds.maximum(),
                                            );
                                            if !parts.is_empty() {
                                                if let Err(error) =
                                                    pass.draw(frame, overlay_camera, &parts)
                                                {
                                                    result = Err(error.to_string());
                                                }
                                            }
                                        }
                                    }
                                    if result.is_ok() && preview_overlay_collision {
                                        let thickness = session
                                            .bounds()
                                            .map(|bounds| {
                                                editor_gizmo::collision_edge_thickness_for_radius(
                                                    bounds.radius(),
                                                )
                                            })
                                            .unwrap_or(0.008);
                                        let parts = session
                                            .collision_overlay_parts_with_thickness(thickness);
                                        if !parts.is_empty() {
                                            if let Err(error) =
                                                pass.draw(frame, overlay_camera, &parts)
                                            {
                                                result = Err(error.to_string());
                                            }
                                        }
                                    }
                                    if result.is_ok() && preview_overlay_normals {
                                        let length = session
                                            .bounds()
                                            .map(|bounds| {
                                                editor_gizmo::normal_shaft_length_for_radius(
                                                    bounds.radius(),
                                                )
                                            })
                                            .unwrap_or(0.12)
                                            .max((orbit_radius * 0.017).clamp(0.06, 4.0));
                                        let parts =
                                            session.normal_overlay_parts_with_length(length);
                                        if !parts.is_empty() {
                                            if let Err(error) =
                                                pass.draw(frame, overlay_camera, &parts)
                                            {
                                                result = Err(error.to_string());
                                            }
                                        }
                                    }
                                    if result.is_ok() && preview_overlay_tangents {
                                        let length = session
                                            .bounds()
                                            .map(|bounds| {
                                                editor_gizmo::normal_shaft_length_for_radius(
                                                    bounds.radius(),
                                                )
                                            })
                                            .unwrap_or(0.12)
                                            .max((orbit_radius * 0.017).clamp(0.06, 4.0));
                                        let parts =
                                            session.tangent_overlay_parts_with_length(length);
                                        if !parts.is_empty() {
                                            if let Err(error) =
                                                pass.draw(frame, overlay_camera, &parts)
                                            {
                                                result = Err(error.to_string());
                                            }
                                        }
                                    }
                                    if result.is_ok() && preview_overlay_uv {
                                        let size = session
                                            .bounds()
                                            .map(|bounds| {
                                                (bounds.radius() * 0.012).clamp(0.02, 0.35)
                                            })
                                            .unwrap_or(0.06);
                                        let parts = session.uv_overlay_parts_with_size(size);
                                        if !parts.is_empty() {
                                            if let Err(error) =
                                                pass.draw(frame, overlay_camera, &parts)
                                            {
                                                result = Err(error.to_string());
                                            }
                                        }
                                    }
                                }
                                result
                            }
                            Ok(Some(GltfPreviewFrame::Uploading(_))) | Ok(None) => Ok(()),
                            Err(error) => Err(error.to_string()),
                        },
                    },
                    // Scene owns authored content on a separate Game3dScene facade.
                    // Scene owns authored content on a separate Game3dScene facade.
                    WorkspaceMode::Scene => {
                        let mut result = scene
                            .render(frame, world, models)
                            .map(|_| ())
                            .map_err(|error| error.to_string());
                        if result.is_ok() {
                            let mut parts = gizmo_state
                                .map(editor_gizmo::draw_parts)
                                .unwrap_or_default();
                            parts.extend_from_slice(&light_cone_parts);
                            if !parts.is_empty() {
                                if let Some(pass) = gizmo_unlit.as_ref() {
                                    if let Err(error) = pass.draw(frame, camera, &parts) {
                                        result = Err(error.to_string());
                                    }
                                }
                            }
                        }
                        result
                    }
                    WorkspaceMode::Code => Ok(()),
                };
                if let Err(error) = result {
                    scene_error = Some(error);
                }
            });
        self.gltf_preview = preview;
        self.gizmo_unlit = gizmo_unlit;
        let gpu_ready = self
            .gltf_preview
            .as_mut()
            .and_then(GltfPreviewSession::take_gpu_ready_event);
        if let Some(gpu) = gpu_ready {
            let path = self
                .gltf_preview
                .as_ref()
                .map(|session| session.relative_path().to_owned())
                .unwrap_or_default();
            if let Some(session) = &self.gltf_preview {
                session.frame_orbit(
                    &mut self.orbit.target,
                    &mut self.orbit.radius,
                    &mut self.orbit.near,
                    &mut self.orbit.far,
                );
                self.apply_orbit_camera();
            }
            let (meshes, materials, selected_mesh, selected_material) = self
                .gltf_preview
                .as_ref()
                .map(|session| {
                    (
                        session.mesh_inventory(),
                        session.material_inventory(),
                        session.selected_mesh(),
                        session.selected_material(),
                    )
                })
                .unwrap_or_default();
            self.emit_value(
                "host.process",
                json!({
                    "kind": "preview",
                    "status": "ready",
                    "stage": "ready",
                    "path": path,
                    "completed": 1.0,
                    "primitive_count": gpu.total_primitives,
                    "gpu_bytes": gpu.total_geometry_bytes,
                    "cache": "production",
                    "meshes": meshes,
                    "materials": materials,
                    "selected_mesh": selected_mesh,
                    "selected_material": selected_material,
                }),
            );
        }
        let error = match surface_result {
            Err(error) => Some(error.to_string()),
            Ok(_) => scene_error,
        };
        if error != self.last_render_error {
            if let Some(message) = &error {
                let message = message.clone();
                self.publish_diagnostic("error", "viewport", &message);
            }
            self.last_render_error = error;
        }
    }

    fn emit_typed<T: Serialize>(&mut self, name: &'static str, payload: T) {
        match serde_json::to_value(payload) {
            Ok(payload) => self.emit_value(name, payload),
            Err(error) => eprintln!("could not serialize {name}: {error}"),
        }
    }

    fn emit_value(&mut self, name: &'static str, payload: Value) {
        if self.outbound.len() == OUTBOUND_EVENT_CAPACITY {
            self.outbound.pop_front();
        }
        self.outbound.push_back(OutboundEvent { name, payload });
        self.flush_events();
    }

    fn flush_events(&mut self) {
        if !self.ui_ready {
            if !self.outbound.is_empty() {
                eprintln!(
                    "yuyib-editor: flush deferred (ui_ready=false); {} queued event(s)",
                    self.outbound.len()
                );
            }
            return;
        }
        let (Some(webview), Some(session), Some(limits)) =
            (&self.webview, self.page_session, self.bridge_limits)
        else {
            eprintln!("yuyib-editor: flush skipped; webview/session/limits missing");
            return;
        };
        while let Some(event) = self.outbound.pop_front() {
            if event.name.starts_with("host.project")
                || event.name.starts_with("host.diagnostics")
                || event.name.starts_with("host.coverage")
            {
                eprintln!("yuyib-editor: emit → {}", event.name);
            }
            let result = EndpointName::parse(event.name)
                .map_err(|error| error.to_string())
                .and_then(|name| {
                    PageEvent::new(1, session, name, event.payload, limits)
                        .map_err(|error| error.to_string())
                })
                .and_then(|event| {
                    webview
                        .emit_event(&event)
                        .map_err(|error| error.to_string())
                });
            if let Err(error) = result {
                eprintln!("yuyib-editor: could not emit {}: {error}", event.name);
            }
        }
    }

    fn publish_diagnostic(&mut self, severity: &str, source: &str, message: &str) {
        self.emit_typed(
            "host.diagnostics",
            json!({ "diagnostics": [Diagnostic { severity, source, message }] }),
        );
    }

    fn shutdown(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(process) = self.play.take() {
            stop_process_async(process, "play", self.process_sender.clone());
        }
        if let Some(process) = self.cargo_check.take() {
            stop_process_async(process, "cargo.check", self.process_sender.clone());
        }
        self.ui_ready = false;
        self.close_requested = false;
        self.command_queue = None;
        self.dropped_commands = None;
        self.bridge_failures = None;
        // Hide the GPU child first so teardown never flashes a second surface.
        if let Some(child) = self.viewport_window.as_ref() {
            child.hide();
        }
        self.renderer.take();
        self.viewport_window.take();
        self.webview.take();
        self.window.take();
        event_loop.exit();
    }
}

impl ApplicationHandler for EditorApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        if let Err(error) = self.initialize_window(event_loop) {
            eprintln!("could not initialize Yuyib Editor: {error}");
            self.shutdown(event_loop);
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let parent_id = self.window.as_ref().map(|window| window.raw().id());
        let viewport_id = self
            .viewport_window
            .as_ref()
            .map(|window| window.raw().id());
        if parent_id == Some(window_id) {
            match event {
                WindowEvent::CloseRequested => self.shutdown(event_loop),
                WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                    if let Err(error) = self.apply_layout() {
                        self.publish_diagnostic("error", "layout", &error.to_string());
                    }
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                }
                WindowEvent::Occluded(occluded) => self.occluded = occluded,
                WindowEvent::RedrawRequested => {
                    self.process_commands();
                    if self.close_requested {
                        self.shutdown(event_loop);
                        return;
                    }
                    self.poll_gltf_preview();
                    self.drain_editor_jobs();
                    self.drain_process_output();
                    self.poll_processes();
                    self.render_preview();
                    self.flush_events();
                }
                _ => {}
            }
            return;
        }
        if viewport_id == Some(window_id) {
            self.handle_viewport_window_event(event);
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.process_commands();
        if self.close_requested {
            self.shutdown(event_loop);
            return;
        }
        self.poll_gltf_preview();
        self.drain_editor_jobs();
        self.drain_process_output();
        self.poll_processes();
        self.poll_project_watch();
        self.flush_events();
        if matches!(self.mode, WorkspaceMode::Scene | WorkspaceMode::Preview) && !self.occluded {
            if let Some(window) = &self.window {
                window.request_redraw();
            }
            event_loop.set_control_flow(ControlFlow::Wait);
        } else if self.play.is_some()
            || self.cargo_check.is_some()
            || self.gltf_preview.is_some()
            || !self.gltf_import_inflight.is_empty()
        {
            event_loop.set_control_flow(ControlFlow::WaitUntil(
                Instant::now() + std::time::Duration::from_millis(50),
            ));
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }
}

impl EditorApp {
    fn handle_viewport_window_event(&mut self, event: WindowEvent) {
        match event {
            WindowEvent::ModifiersChanged(modifiers) => {
                self.viewport_shift = modifiers.state().shift_key();
            }
            WindowEvent::Resized(size) => {
                if let Some(renderer) = &mut self.renderer {
                    renderer.resize(size.width, size.height);
                }
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.viewport_cursor = position;
                self.dispatch_viewport_pointer(
                    ViewportPointerKind::Pointermove,
                    position,
                    0,
                    self.viewport_buttons,
                    0.0,
                    0.0,
                );
            }
            WindowEvent::CursorLeft { .. } => {
                self.viewport_buttons = 0;
                self.dispatch_viewport_pointer(
                    ViewportPointerKind::Pointerleave,
                    self.viewport_cursor,
                    0,
                    0,
                    0.0,
                    0.0,
                );
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let mask = match button {
                    MouseButton::Left => 1,
                    MouseButton::Right => 2,
                    MouseButton::Middle => 4,
                    _ => 0,
                };
                let button_code = match button {
                    MouseButton::Left => 0,
                    MouseButton::Right => 2,
                    MouseButton::Middle => 1,
                    _ => 0,
                };
                match state {
                    ElementState::Pressed => self.viewport_buttons |= mask,
                    ElementState::Released => self.viewport_buttons &= !mask,
                }
                let kind = match state {
                    ElementState::Pressed => ViewportPointerKind::Pointerdown,
                    ElementState::Released => ViewportPointerKind::Pointerup,
                };
                self.dispatch_viewport_pointer(
                    kind,
                    self.viewport_cursor,
                    button_code,
                    self.viewport_buttons,
                    0.0,
                    0.0,
                );
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (delta_x, delta_y) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => {
                        (f64::from(x) * 40.0, f64::from(y) * -40.0)
                    }
                    MouseScrollDelta::PixelDelta(pos) => (pos.x, -pos.y),
                };
                self.dispatch_viewport_pointer(
                    ViewportPointerKind::Wheel,
                    self.viewport_cursor,
                    0,
                    self.viewport_buttons,
                    delta_x,
                    delta_y,
                );
            }
            WindowEvent::RedrawRequested => {
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            _ => {}
        }
    }

    fn dispatch_viewport_pointer(
        &mut self,
        kind: ViewportPointerKind,
        position: PhysicalPosition<f64>,
        button: i32,
        buttons: i32,
        delta_x: f64,
        delta_y: f64,
    ) {
        let scale = self
            .viewport_window
            .as_ref()
            .map(|window| window.raw().scale_factor())
            .or_else(|| {
                self.window
                    .as_ref()
                    .map(|window| window.raw().scale_factor())
            })
            .unwrap_or(1.0);
        let logical = position.to_logical::<f64>(scale);
        self.handle_viewport_pointer(ViewportPointerRequest {
            kind,
            x: logical.x,
            y: logical.y,
            button,
            buttons,
            modifiers: ViewportPointerModifiers {
                shift: self.viewport_shift,
                ..ViewportPointerModifiers::default()
            },
            delta_x,
            delta_y,
            pointer_id: None,
        });
    }
}

fn normalized_component_coverage(coverage: &Value) -> Value {
    let capabilities: BTreeMap<_, _> = coverage["capabilities"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|capability| {
            Some((
                capability["id"].as_str()?.to_owned(),
                (
                    capability["title"].as_str().unwrap_or_default().to_owned(),
                    capability["surfaces"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default(),
                ),
            ))
        })
        .collect();
    Value::Array(
        coverage["components"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|component| {
                let id = component["id"].as_str()?;
                let capability_id = component["capability"].as_str().unwrap_or_default();
                let (title, surfaces) = capabilities
                    .get(capability_id)
                    .cloned()
                    .unwrap_or_else(|| (id.to_owned(), Vec::new()));
                let visual = surfaces.iter().any(|surface| surface == "visual");
                let status = if visual {
                    "Visual"
                } else if surfaces.iter().any(|surface| surface == "asset") {
                    "Asset"
                } else if surfaces.iter().any(|surface| surface == "runtime") {
                    "Runtime"
                } else if surfaces.iter().any(|surface| surface == "code_only") {
                    "CodeOnly"
                } else {
                    "Unavailable"
                };
                let fields: Vec<_> = component["fields"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|field| {
                        let kind_record = field.get("kind")?;
                        let (raw_kind, options) = match kind_record {
                            Value::String(kind) => (kind.as_str(), None),
                            Value::Object(_) => (
                                kind_record
                                    .get("kind")
                                    .and_then(Value::as_str)
                                    .unwrap_or("specialized"),
                                kind_record.get("options"),
                            ),
                            _ => return None,
                        };
                        let kind = match raw_kind {
                            "bool" => "boolean",
                            "i32" | "u32" | "f32" => "number",
                            "color" => "color",
                            "asset_reference" => "asset",
                            "enum" => "enum",
                            "string" => "string",
                            _ => "specialized",
                        };
                        Some(json!({
                            "path": field["path"],
                            "label": field["title"],
                            "kind": kind,
                            "unit": field["unit"],
                            "options": options.and_then(|value| value.get("values")).cloned().unwrap_or(Value::Null),
                            "read_only": field["read_only"].as_bool().unwrap_or(false) || !visual,
                            "apply_play_changes": field["apply_play_changes"].as_bool().unwrap_or(false),
                            "documentation": field["documentation"]
                        }))
                    })
                    .collect();
                Some(json!({
                    "id": id,
                    "label": title,
                    "status": status,
                    "surfaces": surfaces,
                    "schema_version": component["current_version"],
                    "fields": fields,
                    "source": {
                        "component": component["runtime_source"]["file"],
                        "adapter": component["authoring_source"]["file"]
                    }
                }))
            })
            .collect(),
    )
}

fn scene_document_payload(scene: &SceneSession, transaction_id: Option<&str>) -> Value {
    let document = scene.document();
    let entity_guids: HashSet<String> = document
        .entities
        .iter()
        .map(|entity| entity.guid.to_string())
        .collect();
    let mut parents = BTreeMap::<String, String>::new();
    for entity in &document.entities {
        let Some(parent) = entity
            .components
            .iter()
            .find(|component| component.schema().as_str() == "yuyib.parent3d")
            .and_then(|component| component.payload().get("parent"))
            .and_then(Value::as_str)
            .filter(|parent| entity_guids.contains(*parent))
        else {
            continue;
        };
        parents.insert(entity.guid.to_string(), parent.to_owned());
    }
    let mut children = BTreeMap::<String, Vec<String>>::new();
    for (child, parent) in &parents {
        children
            .entry(parent.clone())
            .or_default()
            .push(child.clone());
    }
    let roots: Vec<_> = document
        .entities
        .iter()
        .map(|entity| entity.guid.to_string())
        .filter(|guid| !parents.contains_key(guid))
        .collect();
    let entities: Vec<_> = document
        .entities
        .iter()
        .map(|entity| {
            let guid = entity.guid.to_string();
            let components: Vec<_> = entity
                .components
                .iter()
                .map(|component| {
                    json!({
                        "id": component.schema().as_str(),
                        "schema_version": component.version().get(),
                        "data": component.payload()
                    })
                })
                .collect();
            json!({
                "guid": guid,
                "name": entity.name,
                "parent_guid": parents.get(&guid),
                "children": children.get(&guid).cloned().unwrap_or_default(),
                "components": components
            })
        })
        .collect();
    let name = Path::new(scene.path()).file_stem().map_or_else(
        || scene.path().to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    json!({
        "path": scene.path(),
        "revision": scene.history_revision().get(),
        "file_revision": scene.file_revision().map(|revision| revision.to_string()),
        "dirty": scene.is_dirty(),
        "read_only": scene.is_read_only(),
        "transaction_id": transaction_id,
        "document": {
            "schema": document.format,
            "version": document.format_version.get(),
            "scene_guid": document.scene_guid.to_string(),
            "name": name,
            "roots": roots,
            "entities": entities
        }
    })
}

fn embedded_editor_page() -> Result<LocalPage, Box<dyn Error>> {
    let entry = AssetPath::parse("index.html")?;
    let mut assets = AssetBundle::new(
        MimePolicy::strict().with_web_assembly(true),
        AssetLimits {
            max_assets: 4_096,
            max_asset_bytes: 32 * 1024 * 1024,
            max_bundle_bytes: 128 * 1024 * 1024,
        },
    );
    for (logical_path, bytes) in EMBEDDED_EDITOR_ASSETS {
        assets.insert(AssetPath::parse(logical_path)?, bytes.to_vec())?;
    }
    Ok(LocalPage::new(
        entry,
        assets,
        LocalCsp::strict().with_blob_workers().with_inline_styles(),
    )?)
}

fn viewport_placement(
    viewport: Option<&RenderViewport>,
) -> Result<ChildWindowPlacement, Box<dyn Error>> {
    let Some(viewport) = viewport else {
        return Ok(ChildWindowPlacement::new(0, 0, 1, 1)?);
    };
    Ok(ChildWindowPlacement::new(
        i32::try_from(viewport.x()).unwrap_or(0),
        i32::try_from(viewport.y()).unwrap_or(0),
        viewport.width(),
        viewport.height(),
    )?)
}

fn editor_layout(
    window: &Window,
    mode: WorkspaceMode,
    logical_viewport: Option<ViewportBoundsRequest>,
    has_project: bool,
) -> Result<EditorLayout, Box<dyn Error>> {
    let physical = window.physical_size();
    let scale = window.raw().scale_factor();
    let logical = physical.to_logical::<f64>(scale);
    let logical_width = logical.width.max(1.0);
    let logical_height = logical.height.max(1.0);
    let webview = WebViewBounds::new(0.0, 0.0, logical_width, logical_height)?;
    // No project / Code / missing UI bounds → hide the sibling GPU HWND. A
    // fallback centered hole used to paint the foundation cube over the launcher.
    let viewport = if !has_project
        || mode == WorkspaceMode::Code
        || physical.width == 0
        || physical.height == 0
    {
        None
    } else if let Some(bounds) = logical_viewport {
        let left = bounds.x.clamp(0.0, logical_width);
        let top = bounds.y.clamp(0.0, logical_height);
        let right = (bounds.x + bounds.width).clamp(left, logical_width);
        let bottom = (bounds.y + bounds.height).clamp(top, logical_height);
        if right <= left || bottom <= top {
            None
        } else {
            Some(RenderViewport::from_logical(
                left,
                top,
                right - left,
                bottom - top,
                scale,
                [physical.width, physical.height],
            )?)
        }
    } else {
        None
    };
    Ok(EditorLayout { webview, viewport })
}

fn merge_project_scenes_into_assets(payload: &mut Value, project: &ProjectManifest) {
    let Some(items) = payload.get_mut("items").and_then(Value::as_array_mut) else {
        return;
    };
    for scene in &project.scenes {
        let already = items.iter().any(|item| item["path"] == scene.path);
        if already {
            continue;
        }
        items.push(json!({
            "id": scene.path,
            "path": scene.path,
            "name": scene.name,
            "kind": "scene",
            "extension": "yscene",
            "tracking": "n/a",
            "open": "scene",
            "preview": null,
            "reimport": null
        }));
    }
}

fn asset_index_payload(index: &ProjectAssetIndex, status: &str) -> Value {
    let items: Vec<Value> = index
        .items
        .iter()
        .map(|item| {
            let extension = Path::new(&item.path)
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let id = item
                .id
                .as_ref()
                .map(|guid| format!("asset://{guid}"))
                .unwrap_or_else(|| item.path.clone());
            json!({
                "id": id,
                "path": item.path,
                "name": item.name,
                "kind": asset_kind_label(item.kind),
                "extension": extension,
                "tracking": asset_tracking_label(item.tracking),
                "open": item.open.map(|intent| match intent {
                    AssetOpenIntent::Scene => "scene",
                    AssetOpenIntent::GltfPreview => "gltf_preview",
                }),
                "preview": item.preview.map(action_status_payload),
                "reimport": item.reimport.map(action_status_payload),
            })
        })
        .collect();
    let diagnostics: Vec<Value> = index
        .diagnostics
        .iter()
        .map(|diagnostic| {
            json!({
                "path": diagnostic.path,
                "code": format!("{:?}", diagnostic.code),
                "message": diagnostic.message,
                "severity": "warning",
                "source": "assets"
            })
        })
        .collect();
    json!({
        "root": index.root,
        "revision": index.revision,
        "items": items,
        "diagnostics": diagnostics,
        "status": status
    })
}

fn asset_kind_label(kind: AssetKind) -> &'static str {
    match kind {
        AssetKind::Scene => "scene",
        AssetKind::AssetMetadata => "asset",
        AssetKind::GltfSource => "model",
        AssetKind::ImageSource => "texture",
        AssetKind::Other => "file",
    }
}

fn action_status_payload(status: yuyib_editor_core::AssetActionStatus) -> Value {
    match status {
        yuyib_editor_core::AssetActionStatus::Available => json!({ "status": "available" }),
        yuyib_editor_core::AssetActionStatus::Unavailable { reason_code } => json!({
            "status": "unavailable",
            "reason_code": reason_code
        }),
    }
}

fn asset_tracking_label(tracking: yuyib_editor_core::AssetTracking) -> &'static str {
    match tracking {
        yuyib_editor_core::AssetTracking::Tracked(_) => "tracked",
        yuyib_editor_core::AssetTracking::UntrackedSource => "untracked",
        yuyib_editor_core::AssetTracking::InvalidMetadata => "invalid",
        yuyib_editor_core::AssetTracking::NotApplicable => "n/a",
    }
}

fn collect_unavailable_capabilities(coverage: &Value) -> Vec<Value> {
    coverage["capabilities"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| {
            item["surfaces"]
                .as_array()
                .is_some_and(|surfaces| surfaces.iter().any(|surface| surface == "unavailable"))
        })
        .map(|item| {
            json!({
                "id": item["id"],
                "title": item["title"].as_str().or_else(|| item["id"].as_str()).unwrap_or("capability"),
                "reason": item["unavailable_reason"].as_str().unwrap_or("Unavailable until its evidence gate closes."),
                "milestone": item["target_milestone"],
                "owner": item["owner"]
            })
        })
        .collect()
}

fn strip_play_scene_args(arguments: &mut Vec<String>) {
    strip_flag_pair(arguments, "--scene");
    strip_flag_pair(arguments, "--scene-revision");
    strip_flag_pair(arguments, "--scene-file-revision");
}

fn strip_flag_pair(arguments: &mut Vec<String>, flag: &str) {
    let mut index = 0;
    while index < arguments.len() {
        let arg = &arguments[index];
        if arg == flag {
            arguments.remove(index);
            if index < arguments.len() && !arguments[index].starts_with('-') {
                arguments.remove(index);
            }
            continue;
        }
        if let Some(prefix) = arg.strip_prefix(&format!("{flag}=")) {
            let _ = prefix;
            arguments.remove(index);
            continue;
        }
        index += 1;
    }
}

fn engine_workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn find_engine_play_runner() -> Option<PathBuf> {
    if let Ok(current) = env::current_exe()
        && let Some(dir) = current.parent()
    {
        #[cfg(target_os = "windows")]
        let sibling = dir.join("yuyib-play.exe");
        #[cfg(not(target_os = "windows"))]
        let sibling = dir.join("yuyib-play");
        if sibling.is_file() {
            return sibling.canonicalize().ok();
        }
    }
    find_package_binary(&engine_workspace_root(), "yuyib-play")
}

fn find_package_binary(root: &Path, package: &str) -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    let file_name = format!("{package}.exe");
    #[cfg(not(target_os = "windows"))]
    let file_name = package.to_owned();

    let mut roots = Vec::new();
    if let Ok(target_dir) = env::var("CARGO_TARGET_DIR") {
        roots.push(PathBuf::from(target_dir));
    }
    roots.push(root.join("target"));

    for target_root in roots {
        for profile in ["debug", "release"] {
            let direct = target_root.join(profile).join(&file_name);
            if direct.is_file() {
                return direct.canonicalize().ok();
            }
            let Ok(entries) = fs::read_dir(&target_root) else {
                continue;
            };
            for entry in entries.flatten() {
                let triple_dir = entry.path();
                if !triple_dir.is_dir() {
                    continue;
                }
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name == "debug" || name == "release" || name == "tmp" || name.starts_with('.') {
                    continue;
                }
                let candidate = triple_dir.join(profile).join(&file_name);
                if candidate.is_file() {
                    return candidate.canonicalize().ok();
                }
            }
        }
    }
    None
}

fn project_main_looks_like_stub(project_root: &Path) -> bool {
    fs::read_to_string(project_root.join("src/main.rs"))
        .map(|source| {
            source.contains("Yuyib project ready")
                && !source.contains("Application::new")
                && !source.contains("yuyib_play")
        })
        .unwrap_or(false)
}

fn editor_scene_lighting() -> Game3dLighting {
    // Authored Scene: prefer ECS DirectionalLight. Keep a dim studio fallback so
    // empty / foundation geometry stays readable before the user places a light.
    let fallback = LambertLighting3d::new(
        DirectionalLightDraw {
            direction: [0.35, -1.0, -0.45],
            color: [1.0, 0.98, 0.94],
            illuminance_lux: 0.55,
        },
        [0.035, 0.036, 0.04],
    )
    .expect("dim scene Lambert fallback is valid");
    Game3dLighting::FirstDirectional {
        ambient: [0.035, 0.036, 0.04],
        fallback,
    }
}

fn editor_preview_lighting() -> Game3dLighting {
    // Asset Preview: bright global key + high ambient so imports read clearly
    // without authored lights.
    let direct = LambertLighting3d::new(
        DirectionalLightDraw {
            direction: [0.28, -1.0, -0.42],
            color: [1.0, 0.99, 0.97],
            illuminance_lux: 8.0,
        },
        [0.45, 0.47, 0.52],
    )
    .expect("preview global Lambert is valid");
    let irradiance = DiffuseIrradianceSh3d::constant([0.38, 0.40, 0.44])
        .expect("preview global irradiance is valid");
    Game3dLighting::FixedPbr(PbrLighting3d::new(direct, irradiance))
}

fn roots_equal(a: &Path, b: &Path) -> bool {
    fn normalize(path: &Path) -> PathBuf {
        let mut out = PathBuf::new();
        for component in path.components() {
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    out.pop();
                }
                other => out.push(other.as_os_str()),
            }
        }
        out
    }
    normalize(a) == normalize(b)
}

/// Disk cook cache root for editor scene glTF imports (M3).
fn editor_cook_cache_root(project_root: &Path) -> PathBuf {
    project_root.join(".yuyib_cook")
}

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

fn create_editor_game_scene(project_root: &Path) -> Result<Game3dScene, Box<dyn Error>> {
    Ok(Game3dScene::new(
        project_root,
        Game3dSceneConfig::default()
            .with_shading(Game3dShading::Pbr)
            .with_lighting(editor_scene_lighting()),
    )?
    .with_ssao(SsaoPolicy::street_city()))
}

fn create_editor_preview_scene(project_root: &Path) -> Result<Game3dScene, Box<dyn Error>> {
    Ok(Game3dScene::new(
        project_root,
        Game3dSceneConfig::default()
            .with_shading(Game3dShading::Pbr)
            .with_lighting(editor_preview_lighting()),
    )?
    .with_ssao(SsaoPolicy::street_city()))
}

fn rotate_direction_by_entity(
    world: &World,
    entity: yuyib_ecs::bevy_ecs::entity::Entity,
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
    let rotated = rotate_vec3_by_quat(rotation, local_direction);
    let len_sq = rotated[0] * rotated[0] + rotated[1] * rotated[1] + rotated[2] * rotated[2];
    if !len_sq.is_finite() || len_sq < 1.0e-12 {
        return local_direction;
    }
    let inv = 1.0 / len_sq.sqrt();
    [rotated[0] * inv, rotated[1] * inv, rotated[2] * inv]
}

fn rotate_vec3_by_quat(quat: [f32; 4], v: [f32; 3]) -> [f32; 3] {
    let [qx, qy, qz, qw] = quat;
    let tx = 2.0 * (qy * v[2] - qz * v[1]);
    let ty = 2.0 * (qz * v[0] - qx * v[2]);
    let tz = 2.0 * (qx * v[1] - qy * v[0]);
    [
        v[0] + qw * tx + (qy * tz - qz * ty),
        v[1] + qw * ty + (qz * tx - qx * tz),
        v[2] + qw * tz + (qx * ty - qy * tx),
    ]
}

fn directional_light_from_payload(payload: &Value) -> Result<Option<DirectionalLight3d>, String> {
    let enabled = payload
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    if !enabled {
        return Ok(None);
    }
    let direction = vec3_from_payload(payload, "direction").unwrap_or([0.0, -1.0, 0.0]);
    let color = vec3_from_payload(payload, "color").unwrap_or([1.0, 1.0, 1.0]);
    let illuminance = payload
        .get("illuminance_lux")
        .or_else(|| payload.get("illuminance"))
        .and_then(json_f32)
        .unwrap_or(1.0);
    DirectionalLight3d::new(direction, color, illuminance)
        .map(Some)
        .map_err(|error| error.to_string())
}

fn vec3_from_payload(payload: &Value, field: &str) -> Option<[f32; 3]> {
    let value = payload.get(field)?;
    if let Some(array) = value.as_array() {
        return Some([
            json_f32(array.first()?)?,
            json_f32(array.get(1)?)?,
            json_f32(array.get(2)?)?,
        ]);
    }
    Some([
        json_f32(value.get("x")?)?,
        json_f32(value.get("y")?)?,
        json_f32(value.get("z")?)?,
    ])
}

fn confined_existing_file(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let relative = Path::new(relative);
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err("Play executable must be a project-relative path.".to_owned());
    }
    let executable = root
        .join(relative)
        .canonicalize()
        .map_err(|error| format!("Could not resolve Play executable: {error}"))?;
    if !executable.starts_with(root) || !executable.is_file() {
        return Err("Play executable is not a file below the project root.".to_owned());
    }
    Ok(executable)
}

fn validate_component_field_edit(
    component_id: &str,
    field_path: &str,
    value: &Value,
    known_entities: Option<&BTreeSet<EntityGuid>>,
) -> Result<(), String> {
    match component_id {
        "yuyib.transform3d" | "yuyib.local-transform3d" => {
            validate_transform_field(component_id, field_path, value)
                .map_err(|error| error.to_string())
        }
        "yuyib.parent3d" => validate_parent_field(component_id, field_path, value, known_entities)
            .map(|_| ())
            .map_err(|error| error.to_string()),
        "yuyib.model3d" => validate_model3d_field(component_id, field_path, value)
            .map_err(|error| error.to_string()),
        "yuyib.directional-light3d" => {
            validate_directional_light_field(component_id, field_path, value)
                .map_err(|error| error.to_string())
        }
        _ => Err(format!(
            "Component editing for {component_id} remains read-only until its typed validation/materialization adapter closes Visual coverage."
        )),
    }
}

fn default_component_allowed(component_id: &str) -> Result<(), String> {
    match component_id {
        "yuyib.transform3d"
        | "yuyib.local-transform3d"
        | "yuyib.parent3d"
        | "yuyib.model3d"
        | "yuyib.directional-light3d" => Ok(()),
        _ => Err(format!(
            "component {component_id} cannot be added until its typed adapter is registered"
        )),
    }
}

fn available_components() -> Vec<Value> {
    vec![
        json!({ "id": "yuyib.transform3d", "label": "Transform 3D" }),
        json!({ "id": "yuyib.local-transform3d", "label": "Local Transform 3D" }),
        json!({ "id": "yuyib.parent3d", "label": "Parent 3D" }),
        json!({ "id": "yuyib.model3d", "label": "Model 3D" }),
        json!({ "id": "yuyib.directional-light3d", "label": "Directional Light 3D" }),
    ]
}

fn extract_model_path(payload: &Value) -> Option<String> {
    let value = payload.get("model")?;
    if let Some(text) = value.as_str() {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return None;
        }
        // Keep GUID refs intact for resolve_project_model_path (asset://{guid}).
        if looks_like_asset_guid(trimmed.strip_prefix("asset://").unwrap_or(trimmed)) {
            return Some(if trimmed.starts_with("asset://") {
                trimmed.replace('\\', "/")
            } else {
                format!("asset://{}", trimmed.replace('\\', "/"))
            });
        }
        return Some(
            trimmed
                .strip_prefix("asset://")
                .unwrap_or(trimmed)
                .trim_start_matches(['/', '\\'])
                .replace('\\', "/"),
        );
    }
    // Tolerate object-shaped asset refs from older UI drafts.
    if let Some(text) = value
        .get("path")
        .or_else(|| value.get("uri"))
        .or_else(|| value.get("id"))
        .and_then(Value::as_str)
    {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return None;
        }
        if looks_like_asset_guid(trimmed.strip_prefix("asset://").unwrap_or(trimmed)) {
            return Some(if trimmed.starts_with("asset://") {
                trimmed.replace('\\', "/")
            } else {
                format!("asset://{}", trimmed.replace('\\', "/"))
            });
        }
        return Some(
            trimmed
                .strip_prefix("asset://")
                .unwrap_or(trimmed)
                .trim_start_matches(['/', '\\'])
                .replace('\\', "/"),
        );
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
            extension == "glb" || extension == "gltf"
        })
}

fn attach_process_output(
    process: &mut ManagedProcess,
    label: &'static str,
    sender: &SyncSender<ProcessOutput>,
) {
    if let Some(stdout) = process.take_stdout() {
        spawn_output_reader(stdout, label, "stdout", sender.clone());
    }
    if let Some(stderr) = process.take_stderr() {
        spawn_output_reader(stderr, label, "stderr", sender.clone());
    }
}

fn spawn_output_reader(
    mut reader: impl Read + Send + 'static,
    process: &'static str,
    stream: &'static str,
    sender: SyncSender<ProcessOutput>,
) {
    thread::spawn(move || {
        let mut chunk = [0_u8; 4_096];
        let mut line = Vec::with_capacity(512);
        let mut truncated = false;
        loop {
            let count = match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(count) => count,
                Err(error) => {
                    let _ = sender.try_send(ProcessOutput {
                        process: process.to_owned(),
                        stream,
                        line: format!("output reader failed: {error}"),
                    });
                    return;
                }
            };
            for byte in &chunk[..count] {
                if *byte == b'\n' {
                    publish_process_line(&sender, process, stream, &mut line, truncated);
                    truncated = false;
                } else if line.len() < PROCESS_LINE_BYTE_LIMIT {
                    if *byte != b'\r' {
                        line.push(*byte);
                    }
                } else {
                    truncated = true;
                }
            }
        }
        if !line.is_empty() || truncated {
            publish_process_line(&sender, process, stream, &mut line, truncated);
        }
    });
}

fn publish_process_line(
    sender: &SyncSender<ProcessOutput>,
    process: &str,
    stream: &'static str,
    line: &mut Vec<u8>,
    truncated: bool,
) {
    let mut message = String::from_utf8_lossy(line).into_owned();
    line.clear();
    if truncated {
        message.push_str(" … [line truncated]");
    }
    let _ = sender.try_send(ProcessOutput {
        process: process.to_owned(),
        stream,
        line: message,
    });
}

fn poll_process(process: &mut Option<ManagedProcess>) -> Option<ProcessCompletion> {
    let result = process.as_mut()?.poll();
    match result {
        Ok(ProcessPoll::Running) => None,
        Ok(ProcessPoll::Exited(status)) => {
            process.take();
            Some(ProcessCompletion::Exited(status))
        }
        Ok(ProcessPoll::TimedOut(status)) => {
            process.take();
            Some(ProcessCompletion::TimedOut(status))
        }
        Err(error) => {
            let process = process
                .take()
                .expect("the polled process is still installed");
            Some(ProcessCompletion::PollFailed {
                error: error.to_string(),
                process,
            })
        }
    }
}

fn ephemeral_workspace_root() -> Result<PathBuf, Box<dyn Error>> {
    let root = env::temp_dir().join(format!("yuyib-editor-empty-{}", std::process::id()));
    if root.exists() {
        let _ = fs::remove_dir_all(&root);
    }
    fs::create_dir_all(root.join("assets"))?;
    Ok(root)
}

fn stop_process_async(
    mut process: ManagedProcess,
    label: &'static str,
    sender: SyncSender<ProcessOutput>,
) {
    thread::spawn(move || {
        let line = match process.stop() {
            Ok(status) => format!("process stopped with {status}"),
            Err(error) => format!("could not stop process: {error}"),
        };
        let _ = sender.try_send(ProcessOutput {
            process: label.to_owned(),
            stream: "lifecycle",
            line,
        });
    });
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::json;
    use yuyib_authoring::EntityGuid;

    use super::{available_components, validate_component_field_edit};

    #[test]
    fn available_components_match_registered_add_component_schemas() {
        let components = available_components();
        let component_ids: Vec<_> = components
            .iter()
            .map(|component| component["id"].as_str().unwrap())
            .collect();

        assert_eq!(
            component_ids,
            [
                "yuyib.transform3d",
                "yuyib.local-transform3d",
                "yuyib.parent3d",
                "yuyib.model3d",
                "yuyib.directional-light3d",
            ]
        );
    }

    #[test]
    fn parent3d_field_edit_uses_authoring_validation_and_scene_existence() {
        let parent = EntityGuid::new();
        let missing = EntityGuid::new();
        let known = BTreeSet::from([parent]);

        assert!(
            validate_component_field_edit(
                "yuyib.parent3d",
                "parent",
                &json!(parent.to_string()),
                Some(&known)
            )
            .is_ok()
        );
        assert!(
            validate_component_field_edit(
                "yuyib.parent3d",
                "parent",
                &json!(missing.to_string()),
                Some(&known)
            )
            .is_err()
        );
        assert!(
            validate_component_field_edit("yuyib.parent3d", "parent", &json!(null), Some(&known))
                .is_ok()
        );
    }
}
