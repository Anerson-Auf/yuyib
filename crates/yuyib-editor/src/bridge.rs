use std::{
    cell::Cell,
    cell::RefCell,
    collections::VecDeque,
    error::Error,
    num::NonZeroU128,
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use yuyib_platform::Window;
use yuyib_webview::{BridgeLimits, BridgeRouter, EndpointName, PageSessionId, TypedEndpoint};

const COMMAND_QUEUE_CAPACITY: usize = 128;
static PAGE_SESSION_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceMode {
    #[default]
    Scene,
    /// Asset Preview tab — sibling WGPU surface draws glTF over the preview stage.
    Preview,
    Code,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ViewportTool {
    #[default]
    Move,
    Rotate,
    Scale,
    Select,
}

#[derive(Clone, Copy, Debug, Deserialize)]
pub struct ViewportToolRequest {
    pub tool: ViewportTool,
}

#[derive(Debug)]
pub enum EditorCommand {
    UiReady,
    SetWorkspaceMode(WorkspaceMode),
    SetViewportTool(ViewportTool),
    SetViewportBounds(ViewportBoundsRequest),
    ViewportPointer(ViewportPointerRequest),
    WindowControl(WindowControlRequest),
    StartPlay,
    StopPlay,
    CargoCheck(CargoCheckRequest),
    ReadSource(SourceRequest),
    SaveSource(SourceSaveRequest),
    SetSelection(SelectionRequest),
    OpenScene(SceneOpenRequest),
    CreateScene(SceneCreateRequest),
    SaveScene(SceneSaveRequest),
    EditScene(SceneCommandRequest),
    BrowseOpenProject,
    CreateProjectInteractive(ProjectCreateRequest),
    OpenProjectPath(ProjectOpenRequest),
    RefreshAssetIndex,
    OpenAsset(AssetOpenRequest),
    ReimportAsset(AssetReimportRequest),
    TrackAsset(AssetTrackRequest),
    RenameAsset(AssetRenameRequest),
    MigrateSceneModelRefs(MigrateSceneModelRefsRequest),
    SaveAssetImportSettings(AssetImportSettingsSaveRequest),
    SetPreviewOverlay(PreviewOverlayRequest),
    SetPreviewSelection(PreviewSelectionRequest),
}

#[derive(Debug, Deserialize)]
pub struct PreviewOverlayRequest {
    pub overlay: String,
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct PreviewSelectionRequest {
    /// Currently `"mesh"` or `"material"`.
    pub kind: String,
    /// Mesh/material index, or omit/`null` for the full model.
    #[serde(default)]
    pub index: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct ProjectCreateRequest {
    pub name: String,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub parent_directory: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ProjectOpenRequest {
    pub path: String,
}

#[derive(Debug, Deserialize)]
pub struct WorkspaceModeRequest {
    pub mode: WorkspaceMode,
}

#[derive(Clone, Copy, Debug, Deserialize)]
pub struct ViewportBoundsRequest {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Debug, Deserialize)]
#[allow(dead_code)]
pub struct ViewportPointerRequest {
    #[serde(rename = "type")]
    pub kind: ViewportPointerKind,
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub button: i32,
    #[serde(default)]
    pub buttons: i32,
    #[serde(default)]
    pub modifiers: ViewportPointerModifiers,
    #[serde(default)]
    pub delta_x: f64,
    #[serde(default)]
    pub delta_y: f64,
    #[serde(default)]
    pub pointer_id: Option<i32>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ViewportPointerKind {
    Pointerenter,
    Pointerleave,
    Pointermove,
    Pointerdown,
    Pointerup,
    Wheel,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[allow(dead_code)]
pub struct ViewportPointerModifiers {
    #[serde(default)]
    pub alt: bool,
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub meta: bool,
    #[serde(default)]
    pub shift: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WindowControlAction {
    Minimize,
    Maximize,
    Close,
}

#[derive(Clone, Copy, Debug, Deserialize)]
pub struct WindowControlRequest {
    pub action: WindowControlAction,
}

#[derive(Debug, Deserialize)]
pub struct AssetOpenRequest {
    pub id: String,
}

#[derive(Debug, Deserialize)]
pub struct AssetReimportRequest {
    pub id: String,
}

#[derive(Debug, Deserialize)]
pub struct AssetTrackRequest {
    /// Asset-root-relative source path, project-relative path, or `asset://…`.
    pub id: String,
}

#[derive(Debug, Deserialize)]
pub struct AssetRenameRequest {
    /// GUID (`asset://…` / bare UUID) or current source path.
    pub id: String,
    /// New asset-root-relative glTF path (e.g. `models/hero_v2.glb`).
    pub to: String,
}

#[derive(Debug, Deserialize)]
pub struct MigrateSceneModelRefsRequest {
    /// When true, report rewrites without writing scenes.
    #[serde(default)]
    pub dry_run: bool,
    /// Optional explicit scene list; defaults to project manifest scenes.
    #[serde(default)]
    pub scene_paths: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct AssetImportSettingsSaveRequest {
    /// GUID (`asset://…` / bare UUID) or tracked source path.
    pub id: String,
    /// Opaque settings payload validated by the importer settings schema.
    pub payload: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct CargoCheckRequest {
    pub package: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SourceRequest {
    #[serde(alias = "relativePath")]
    pub path: String,
}

#[derive(Debug, Deserialize)]
pub struct SourceSaveRequest {
    #[serde(alias = "relativePath")]
    pub path: String,
    pub content: String,
    #[serde(default)]
    pub revision: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SelectionRequest {
    #[serde(default)]
    pub id: Option<String>,
    /// World translation of the selection when known (editor viewport readout).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translation: Option<[f32; 3]>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SceneOpenRequest {
    pub path: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SceneCreateRequest {
    pub path: String,
    #[serde(default)]
    pub scene_guid: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
pub struct SceneSaveRequest {
    #[serde(default)]
    pub expected_revision: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SceneCommandRequest {
    pub base_revision: u64,
    pub transaction_id: String,
    pub command: SceneEditRequest,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type")]
pub enum SceneEditRequest {
    #[serde(rename = "entity.rename")]
    RenameEntity {
        entity_guid: String,
        name: Option<String>,
    },
    #[serde(rename = "entity.create")]
    CreateEntity {
        name: Option<String>,
        #[serde(default)]
        with_transform3d: bool,
    },
    #[serde(rename = "entity.delete")]
    DeleteEntity { entity_guid: String },
    #[serde(rename = "component.add")]
    AddComponent {
        entity_guid: String,
        component_id: String,
    },
    #[serde(rename = "component.remove")]
    RemoveComponent {
        entity_guid: String,
        component_id: String,
    },
    #[serde(rename = "component.field.set")]
    SetComponentField {
        entity_guid: String,
        component_id: String,
        field_path: String,
        value: serde_json::Value,
    },
    #[serde(rename = "history.undo")]
    Undo,
    #[serde(rename = "history.redo")]
    Redo,
}

pub type CommandQueue = Rc<RefCell<VecDeque<EditorCommand>>>;

pub struct BridgeBinding {
    pub router: BridgeRouter,
    pub queue: CommandQueue,
    pub session: PageSessionId,
    pub limits: BridgeLimits,
    pub dropped_commands: Rc<Cell<u64>>,
    pub bridge_failures: Rc<RefCell<Vec<String>>>,
}

fn is_viewport_noise(command: &EditorCommand) -> bool {
    matches!(
        command,
        EditorCommand::SetViewportBounds(_) | EditorCommand::ViewportPointer(_)
    )
}

fn enqueue_command(queue: &CommandQueue, dropped_commands: &Rc<Cell<u64>>, command: EditorCommand) {
    let Ok(mut commands) = queue.try_borrow_mut() else {
        dropped_commands.set(dropped_commands.get().saturating_add(1));
        return;
    };

    match &command {
        EditorCommand::SetViewportBounds(_) => {
            // Keep only the latest layout sample; never let it starve project/scene work.
            commands.retain(|existing| !matches!(existing, EditorCommand::SetViewportBounds(_)));
            commands.push_back(command);
        }
        EditorCommand::ViewportPointer(event)
            if matches!(
                event.kind,
                ViewportPointerKind::Pointermove | ViewportPointerKind::Wheel
            ) =>
        {
            commands.retain(|existing| {
                !matches!(
                    existing,
                    EditorCommand::ViewportPointer(previous)
                        if matches!(
                            previous.kind,
                            ViewportPointerKind::Pointermove | ViewportPointerKind::Wheel
                        )
                )
            });
            commands.push_back(command);
        }
        _ => {
            if commands.len() >= COMMAND_QUEUE_CAPACITY {
                let before = commands.len();
                commands.retain(|existing| !is_viewport_noise(existing));
                if commands.len() < before {
                    dropped_commands.set(
                        dropped_commands
                            .get()
                            .saturating_add((before - commands.len()) as u64),
                    );
                }
            }
            if commands.len() >= COMMAND_QUEUE_CAPACITY {
                // Still full of authoritative work — refuse rather than drop open/create.
                dropped_commands.set(dropped_commands.get().saturating_add(1));
                eprintln!("yuyib-editor: dropped authoritative bridge command; queue is saturated");
                return;
            }
            commands.push_back(command);
        }
    }
}

#[allow(clippy::too_many_lines)]
pub fn create_bridge(window: &Window) -> Result<BridgeBinding, Box<dyn Error>> {
    let session = fresh_page_session();
    // Source payloads remain bounded above the ProjectDocumentStore limit.
    // Extra room is required because JSON string escaping can expand UTF-8 text.
    let limits = BridgeLimits::new(1, 16 * 1024 * 1024, 12 * 1024 * 1024, 96)?;
    let queue = Rc::new(RefCell::new(VecDeque::with_capacity(
        COMMAND_QUEUE_CAPACITY,
    )));
    let dropped_commands = Rc::new(Cell::new(0_u64));
    let bridge_failures = Rc::new(RefCell::new(Vec::<String>::new()));
    let mut router = BridgeRouter::new(session, limits);

    register::<serde_json::Value, _>(
        &mut router,
        "ui.ready",
        &queue,
        &dropped_commands,
        window,
        |_| EditorCommand::UiReady,
    )?;
    register::<WorkspaceModeRequest, _>(
        &mut router,
        "workspace.mode",
        &queue,
        &dropped_commands,
        window,
        |request| EditorCommand::SetWorkspaceMode(request.mode),
    )?;
    register::<ViewportToolRequest, _>(
        &mut router,
        "viewport.tool",
        &queue,
        &dropped_commands,
        window,
        |request| EditorCommand::SetViewportTool(request.tool),
    )?;
    register::<ViewportBoundsRequest, _>(
        &mut router,
        "viewport.bounds",
        &queue,
        &dropped_commands,
        window,
        EditorCommand::SetViewportBounds,
    )?;
    register::<ViewportPointerRequest, _>(
        &mut router,
        "viewport.pointer",
        &queue,
        &dropped_commands,
        window,
        EditorCommand::ViewportPointer,
    )?;
    register::<WindowControlRequest, _>(
        &mut router,
        "window.control",
        &queue,
        &dropped_commands,
        window,
        EditorCommand::WindowControl,
    )?;
    register::<serde_json::Value, _>(
        &mut router,
        "play.start",
        &queue,
        &dropped_commands,
        window,
        |_| EditorCommand::StartPlay,
    )?;
    register::<serde_json::Value, _>(
        &mut router,
        "play.stop",
        &queue,
        &dropped_commands,
        window,
        |_| EditorCommand::StopPlay,
    )?;
    register::<serde_json::Value, _>(
        &mut router,
        "assets.refresh",
        &queue,
        &dropped_commands,
        window,
        |_| EditorCommand::RefreshAssetIndex,
    )?;
    register::<PreviewSelectionRequest, _>(
        &mut router,
        "preview.selection.set",
        &queue,
        &dropped_commands,
        window,
        EditorCommand::SetPreviewSelection,
    )?;
    register::<PreviewOverlayRequest, _>(
        &mut router,
        "preview.overlay.set",
        &queue,
        &dropped_commands,
        window,
        EditorCommand::SetPreviewOverlay,
    )?;
    register::<AssetOpenRequest, _>(
        &mut router,
        "asset.open",
        &queue,
        &dropped_commands,
        window,
        EditorCommand::OpenAsset,
    )?;
    register::<AssetReimportRequest, _>(
        &mut router,
        "asset.reimport",
        &queue,
        &dropped_commands,
        window,
        EditorCommand::ReimportAsset,
    )?;
    register::<AssetTrackRequest, _>(
        &mut router,
        "asset.track",
        &queue,
        &dropped_commands,
        window,
        EditorCommand::TrackAsset,
    )?;
    register::<AssetRenameRequest, _>(
        &mut router,
        "asset.rename",
        &queue,
        &dropped_commands,
        window,
        EditorCommand::RenameAsset,
    )?;
    register::<MigrateSceneModelRefsRequest, _>(
        &mut router,
        "assets.migrate_scene_model_refs",
        &queue,
        &dropped_commands,
        window,
        EditorCommand::MigrateSceneModelRefs,
    )?;
    register::<AssetImportSettingsSaveRequest, _>(
        &mut router,
        "asset.import_settings.save",
        &queue,
        &dropped_commands,
        window,
        EditorCommand::SaveAssetImportSettings,
    )?;
    register::<CargoCheckRequest, _>(
        &mut router,
        "cargo.check",
        &queue,
        &dropped_commands,
        window,
        EditorCommand::CargoCheck,
    )?;
    register::<SourceRequest, _>(
        &mut router,
        "source.open",
        &queue,
        &dropped_commands,
        window,
        EditorCommand::ReadSource,
    )?;
    register::<SourceRequest, _>(
        &mut router,
        "source.read",
        &queue,
        &dropped_commands,
        window,
        EditorCommand::ReadSource,
    )?;
    register::<SourceSaveRequest, _>(
        &mut router,
        "source.save",
        &queue,
        &dropped_commands,
        window,
        EditorCommand::SaveSource,
    )?;
    register::<SelectionRequest, _>(
        &mut router,
        "selection.set",
        &queue,
        &dropped_commands,
        window,
        EditorCommand::SetSelection,
    )?;
    register::<SceneOpenRequest, _>(
        &mut router,
        "scene.open",
        &queue,
        &dropped_commands,
        window,
        EditorCommand::OpenScene,
    )?;
    register::<SceneCreateRequest, _>(
        &mut router,
        "scene.create",
        &queue,
        &dropped_commands,
        window,
        EditorCommand::CreateScene,
    )?;
    register::<SceneSaveRequest, _>(
        &mut router,
        "scene.save",
        &queue,
        &dropped_commands,
        window,
        EditorCommand::SaveScene,
    )?;
    register::<SceneCommandRequest, _>(
        &mut router,
        "scene.command",
        &queue,
        &dropped_commands,
        window,
        EditorCommand::EditScene,
    )?;
    register::<serde_json::Value, _>(
        &mut router,
        "project.openInteractive",
        &queue,
        &dropped_commands,
        window,
        |_| EditorCommand::BrowseOpenProject,
    )?;
    register::<ProjectCreateRequest, _>(
        &mut router,
        "project.createInteractive",
        &queue,
        &dropped_commands,
        window,
        EditorCommand::CreateProjectInteractive,
    )?;
    register::<ProjectOpenRequest, _>(
        &mut router,
        "project.open",
        &queue,
        &dropped_commands,
        window,
        EditorCommand::OpenProjectPath,
    )?;

    Ok(BridgeBinding {
        router,
        queue,
        session,
        limits,
        dropped_commands,
        bridge_failures,
    })
}

fn fresh_page_session() -> PageSessionId {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let process = u128::from(std::process::id()) << 64;
    let sequence = u128::from(PAGE_SESSION_SEQUENCE.fetch_add(1, Ordering::Relaxed));
    let value = timestamp ^ process ^ sequence;
    PageSessionId::new(NonZeroU128::new(value).unwrap_or(NonZeroU128::MIN))
}

fn register<Request, Map>(
    router: &mut BridgeRouter,
    name: &str,
    queue: &CommandQueue,
    dropped_commands: &Rc<Cell<u64>>,
    window: &Window,
    map: Map,
) -> Result<(), Box<dyn Error>>
where
    Request: DeserializeOwned + 'static,
    Map: Fn(Request) -> EditorCommand + 'static,
{
    let queue = Rc::clone(queue);
    let dropped_commands = Rc::clone(dropped_commands);
    let window = window.clone();
    let endpoint = name.to_owned();
    router.register(TypedEndpoint::new(
        EndpointName::parse(name)?,
        move |request: Request| {
            if endpoint != "viewport.bounds" && endpoint != "viewport.pointer" {
                eprintln!("yuyib-editor: bridge ← {endpoint}");
            }
            enqueue_command(&queue, &dropped_commands, map(request));
            window.request_redraw();
        },
    ))?;
    Ok(())
}
