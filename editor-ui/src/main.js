import "./style.css";
import * as monaco from "monaco-editor/esm/vs/editor/editor.api";
import EditorWorker from "monaco-editor/esm/vs/editor/editor.worker?worker";
import JsonWorker from "monaco-editor/esm/vs/language/json/json.worker?worker";
import CssWorker from "monaco-editor/esm/vs/language/css/css.worker?worker";
import HtmlWorker from "monaco-editor/esm/vs/language/html/html.worker?worker";
import TypeScriptWorker from "monaco-editor/esm/vs/language/typescript/ts.worker?worker";

self.MonacoEnvironment = {
  getWorker(_moduleId, label) {
    if (label === "json") return new JsonWorker();
    if (label === "css" || label === "scss" || label === "less") return new CssWorker();
    if (label === "html" || label === "handlebars" || label === "razor") return new HtmlWorker();
    if (label === "typescript" || label === "javascript") return new TypeScriptWorker();
    return new EditorWorker();
  },
};

const hosted = Boolean(window.yuyib && typeof window.yuyib.post === "function");
const defaultAddableComponents = [
  { id: "yuyib.transform3d", label: "Transform 3D" },
  { id: "yuyib.local-transform3d", label: "Local Transform 3D" },
  { id: "yuyib.parent3d", label: "Parent 3D" },
  { id: "yuyib.model3d", label: "Model 3D" },
  { id: "yuyib.directional-light3d", label: "Directional Light 3D" },
];
const state = {
  revision: 1842,
  selection: null,
  view: "scene",
  playMode: "stopped",
  monacoEditor: null,
  monacoModel: null,
  requestId: 1,
  transactionId: 1,
  sourcePath: hosted ? null : "src/neon_sign.rs",
  sourceRevision: hosted ? null : 12,
  sourceTree: [],
  lspStatus: "idle",
  lspPending: new Map(),
  lspProvidersRegistered: false,
  pendingDefinitionReveal: null,
  sourceChangeTimer: null,
  overlays: { bounds: true, collision: false, normals: true, tangents: false, uv: false },
  previewMeshes: [],
  previewSelectedMesh: null,
  previewMaterials: [],
  previewSelectedMaterial: null,
  previewMaterialOverride: null,
  previewTextures: [],
  previewAnimations: [],
  previewSelectedAnimation: null,
  scene: { path: null, revision: 0, document: null, dirty: false, canUndo: false, canRedo: false, readOnly: false },
  componentCoverage: new Map(),
  systemsCoverage: [],
  assetPreview: null,
  previewLoadingPath: null,
  availableComponents: defaultAddableComponents,
  projectConfig: { name: null, package: null, executable: null, args: [], ready: false, root: null, scenes: [] },
  assets: [],
  activeTool: "move",
  pendingProjectAction: null,
  pendingProjectTimer: null,
  diagnosticsCopyBuffer: "",
};

const sourceDocuments = {
  "component.neon-sign": {
    stableId: "project.component.neon_sign",
    name: "neon_sign.rs",
    path: "src/neon_sign.rs",
    uri: "yuyib://project/src/neon_sign.rs",
    content: `use bevy_ecs::prelude::*;
use yuyib::prelude::*;

/// Authored behavior data for a pulsing emissive sign.
#[derive(Component, YuyibAuthoring)]
#[authoring(id = "neon_district.neon_sign", version = 1)]
pub struct NeonSign {
    #[authoring(min = 0.0, max = 20.0)]
    pub peak_intensity: f32,
    pub pulse_hz: f32,
}

pub fn pulse_neon_signs(
    time: Res<Time>,
    mut signs: Query<(&NeonSign, &mut MaterialOverride3d)>,
) {
    for (sign, mut material) in &mut signs {
        let wave = (time.elapsed_secs() * sign.pulse_hz).sin();
        material.emissive_intensity = sign.peak_intensity * (0.82 + wave * 0.18);
    }
}

pub fn register(plugin: &mut GamePlugin) {
    plugin
        .register_authoring::<NeonSign>()
        .add_systems(Update, pulse_neon_signs);
}
`,
  },
  "project.world": {
    stableId: "project.bootstrap",
    name: "main.rs",
    path: "src/main.rs",
    uri: "yuyib://project/src/main.rs",
    content: `use yuyib::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    Application::new()
        .game(Game3d::from_project("project.yuyib")?)
        .run()?;
    Ok(())
}
`,
  },
  "systems.neon": {
    stableId: "project.systems.neon",
    name: "neon_systems.rs",
    path: "src/neon_systems.rs",
    uri: "yuyib://project/src/neon_systems.rs",
    content: `use bevy_ecs::prelude::*;
use crate::neon_sign::NeonSign;

#[system_descriptor(
    id = "neon_district.pulse_neon_signs",
    plugin = "neon_district.gameplay",
    reads(NeonSign, Time),
    writes(MaterialOverride3d),
    schedule = Update,
)]
pub fn pulse_neon_signs(/* registered by the gameplay plugin */) {
    // The implementation lives beside NeonSign so source navigation stays local.
}
`,
  },
};

const mockCoverage = [
  {
    id: "yuyib.transform3d",
    label: "Transform 3D",
    status: "Visual",
    schema_version: 2,
    source: { component: "src/neon_sign.rs", adapter: "src/neon_sign.rs", systems: "src/neon_systems.rs" },
    fields: [
      { path: "translation.x", label: "Position X", group: "Position", kind: "number", step: 0.1 },
      { path: "translation.y", label: "Position Y", group: "Position", kind: "number", step: 0.1 },
      { path: "translation.z", label: "Position Z", group: "Position", kind: "number", step: 0.1 },
      { path: "rotation.x", label: "Rotation X", group: "Rotation", kind: "number", step: 1 },
      { path: "rotation.y", label: "Rotation Y", group: "Rotation", kind: "number", step: 1 },
      { path: "rotation.z", label: "Rotation Z", group: "Rotation", kind: "number", step: 1 },
      { path: "scale.x", label: "Scale X", group: "Scale", kind: "number", step: 0.1, min: 0.0001 },
      { path: "scale.y", label: "Scale Y", group: "Scale", kind: "number", step: 0.1, min: 0.0001 },
      { path: "scale.z", label: "Scale Z", group: "Scale", kind: "number", step: 0.1, min: 0.0001 },
    ],
  },
  {
    id: "yuyib.model3d",
    label: "Model 3D",
    status: "Visual",
    schema_version: 3,
    source: { component: "src/neon_sign.rs", adapter: "src/neon_sign.rs", systems: "src/neon_systems.rs" },
    fields: [
      { path: "model", label: "Model", kind: "asset" },
      { path: "visible", label: "Visible", kind: "boolean" },
      { path: "render_order", label: "Render order", kind: "number", step: 1 },
    ],
  },
  {
    id: "yuyib.standard_material",
    label: "Material Override",
    status: "Visual",
    schema_version: 1,
    fields: [
      { path: "slot", label: "Slot", kind: "enum", options: ["material_0", "frame_metal", "cables"] },
      { path: "emissive", label: "Emissive", kind: "color" },
      { path: "intensity", label: "Intensity", kind: "number", min: 0, max: 20, step: 0.1 },
    ],
  },
  {
    id: "yuyib.collider3d",
    label: "Collider 3D",
    status: "Visual",
    schema_version: 1,
    fields: [
      { path: "shape", label: "Shape", kind: "enum", options: ["aabb", "sphere", "triangle_mesh"] },
      { path: "enabled", label: "Enabled", kind: "boolean" },
    ],
  },
];

let mockSceneRevision = 7;
let mockScenePath = "district_01.yscene";
let mockSceneDirty = false;
const mockUndoStack = [];
const mockRedoStack = [];
let mockSceneDocument = {
  schema: "yuyib.scene",
  version: 1,
  scene_guid: "scene://district-01",
  name: "district_01",
  roots: ["entity://district-root"],
  entities: [
    {
      guid: "entity://district-root",
      name: "District Root",
      children: ["entity://environment", "entity://lighting", "entity://gameplay"],
      components: [{ id: "yuyib.transform3d", schema_version: 2, data: { translation: { x: 0, y: 0, z: 0 }, rotation: { x: 0, y: 0, z: 0 }, scale: { x: 1, y: 1, z: 1 } } }],
    },
    {
      guid: "entity://environment",
      name: "Environment",
      parent_guid: "entity://district-root",
      children: ["entity://street", "entity://props"],
      components: [],
    },
    {
      guid: "entity://street",
      name: "Street",
      parent_guid: "entity://environment",
      children: [],
      components: [
        { id: "yuyib.transform3d", schema_version: 2, data: { translation: { x: 0, y: 0, z: 0 }, rotation: { x: 0, y: 0, z: 0 }, scale: { x: 1, y: 1, z: 1 } } },
        { id: "yuyib.model3d", schema_version: 3, data: { asset_guid: "asset://models/street", visible: true, cast_shadows: true, lod_bias: 0 } },
      ],
    },
    {
      guid: "entity://props",
      name: "Props",
      parent_guid: "entity://environment",
      children: ["entity://neon-sign-07", "entity://dumpster"],
      components: [],
    },
    {
      guid: "entity://neon-sign-07",
      name: "Neon Sign 07",
      parent_guid: "entity://props",
      children: [],
      components: [
        { id: "yuyib.transform3d", schema_version: 2, data: { translation: { x: 12.4, y: 3.2, z: -8.65 }, rotation: { x: 0, y: -34.5, z: 0 }, scale: { x: 1, y: 1, z: 1 } } },
        { id: "yuyib.model3d", schema_version: 3, data: { asset_guid: "asset://models/neon_sign", visible: true, cast_shadows: true, lod_bias: 0 } },
        { id: "yuyib.standard_material", schema_version: 1, data: { slot: "material_0", emissive: "#ff2dbb", intensity: 12 } },
        { id: "yuyib.collider3d", schema_version: 1, data: { shape: "aabb", enabled: true } },
      ],
    },
    {
      guid: "entity://dumpster",
      name: "Dumpster",
      parent_guid: "entity://props",
      children: [],
      components: [{ id: "yuyib.model3d", schema_version: 3, data: { asset_guid: "asset://models/dumpster", visible: true, cast_shadows: true, lod_bias: 0 } }],
    },
    {
      guid: "entity://lighting",
      name: "Lighting",
      parent_guid: "entity://district-root",
      children: ["entity://moon-key"],
      components: [],
    },
    {
      guid: "entity://moon-key",
      name: "Moon Key",
      parent_guid: "entity://lighting",
      children: [],
      components: [{ id: "yuyib.directional_light3d", schema_version: 1, data: { illuminance: 2.4, color: "#8fa8ff" } }],
    },
    {
      guid: "entity://gameplay",
      name: "Gameplay",
      parent_guid: "entity://district-root",
      children: [],
      components: [{ id: "neon_district.spawn_rules", schema_version: 4, data: { max_npcs: 24, district: "alley" }, opaque: true }],
    },
  ],
};

function cloneMockScene() {
  return structuredClone(mockSceneDocument);
}

function setJsonPath(target, fieldPath, value) {
  const segments = fieldPath.split(".").filter(Boolean);
  if (!segments.length) return;
  let cursor = target;
  for (const segment of segments.slice(0, -1)) {
    const key = jsonPathKey(cursor, segment);
    if (cursor[key] === undefined || cursor[key] === null || typeof cursor[key] !== "object") {
      cursor[key] = {};
    }
    cursor = cursor[key];
  }
  cursor[jsonPathKey(cursor, segments.at(-1))] = value;
}

function jsonPathKey(parent, segment) {
  if (Array.isArray(parent)) {
    const axis = { x: 0, y: 1, z: 2, w: 3, r: 0, g: 1, b: 2, a: 3 }[segment];
    if (axis !== undefined) return axis;
    const index = Number(segment);
    if (Number.isInteger(index)) return index;
  }
  return segment;
}

function getJsonPath(target, fieldPath) {
  return fieldPath.split(".").filter(Boolean).reduce((value, key) => {
    if (value === undefined || value === null) return undefined;
    return value[jsonPathKey(value, key)];
  }, target);
}

function emitMockScene(delay = 45) {
  window.setTimeout(() => emitPageEvent("host.scene.document", {
    path: mockScenePath,
    revision: mockSceneRevision,
    document: cloneMockScene(),
  }), delay);
  window.setTimeout(() => emitPageEvent("host.scene.history", {
    revision: mockSceneRevision,
    dirty: mockSceneDirty,
    can_undo: mockUndoStack.length > 0,
    can_redo: mockRedoStack.length > 0,
  }), delay + 10);
}

function emitPageEvent(event, payload) {
  window.dispatchEvent(
    new CustomEvent("yuyib:event", {
      detail: { version: 1, event, payload },
    }),
  );
}

function mockHost(message) {
  const respond = (event, payload, delay = 70) => window.setTimeout(() => emitPageEvent(event, payload), delay);

  switch (message.endpoint) {
    case "ui.ready":
      respond("host.coverage", {
        revision: state.revision,
        status: "Visual",
        covered: 48,
        total: 52,
        project: { name: "neon-district", package: "neon-district", play: { executable: "neon-district", args: ["--scene", "district_01.yscene"] } },
        components: mockCoverage,
      });
      break;
    case "selection.set":
      respond("host.selection", {
        id: message.payload.id,
        label: labelForStableId(message.payload.id),
      }, 35);
      break;
    case "workspace.mode":
      respond("host.process", { kind: "workspace", status: "ready", mode: message.payload.mode }, 25);
      break;
    case "viewport.tool":
      break;
    case "viewport.bounds":
      break;
    case "play.start":
      state.revision += 1;
      respond("host.process", { kind: "play", status: "playing", executable: message.payload.executable, args: message.payload.args || [] }, 80);
      break;
    case "play.stop":
      respond("host.process", { kind: "play", status: "stopped" }, 55);
      break;
    case "source.open":
    case "source.read": {
      const lookup = Object.keys(sourceDocuments).find((key) => sourceDocuments[key].path === message.payload.path) || "component.neon-sign";
      const document = sourceDocuments[lookup] || sourceDocuments["component.neon-sign"];
      respond("host.source", {
        request_id: message.id,
        path: document.path,
        display_name: document.name,
        language: "rust",
        uri: document.uri,
        content: document.content,
        revision: 12,
        read_only: false,
      }, 90);
      break;
    }
    case "source.save":
      respond("host.source", { path: message.payload.path, content: message.payload.content, revision: (message.payload.revision || 0) + 1, saved: true }, 75);
      break;
    case "source.change":
      // Buffer sync; browser mock has no rust-analyzer sidecar.
      break;
    case "lsp.completion":
      respond("host.lsp.completion", {
        request_id: message.payload.request_id,
        items: [
          { label: "mock_complete", kind: 6, insert_text: "mock_complete", detail: "mock" },
        ],
      }, 40);
      break;
    case "lsp.hover":
      respond("host.lsp.hover", {
        request_id: message.payload.request_id,
        markdown: "**mock hover**\n\nBrowser mock has no rust-analyzer.",
      }, 40);
      break;
    case "lsp.signatureHelp":
      respond("host.lsp.signatureHelp", {
        request_id: message.payload.request_id,
        help: {
          signatures: [{
            label: "fn smoke_note(project: &str) -> String",
            documentation: "Mock signature help",
            parameters: [{
              label: "project: &str",
              documentation: "project name",
            }],
            active_parameter: 0,
          }],
          active_signature: 0,
          active_parameter: 0,
        },
      }, 40);
      break;
    case "lsp.definition":
      respond("host.lsp.definition", {
        request_id: message.payload.request_id,
        locations: [{
          path: message.payload.path || state.sourcePath || "src/demo_lsp.rs",
          start_line: 1,
          start_column: 1,
          end_line: 1,
          end_column: 12,
        }],
      }, 40);
      break;
    case "lsp.references":
      respond("host.lsp.references", {
        request_id: message.payload.request_id,
        locations: [
          {
            path: message.payload.path || state.sourcePath || "src/demo_lsp.rs",
            start_line: 11,
            start_column: 12,
            end_line: 11,
            end_column: 23,
          },
          {
            path: message.payload.path || state.sourcePath || "src/demo_lsp.rs",
            start_line: 22,
            start_column: 18,
            end_line: 22,
            end_column: 29,
          },
        ],
      }, 40);
      break;
    case "lsp.rename":
      respond("host.lsp.rename", {
        request_id: message.payload.request_id,
        files: [{
          path: message.payload.path || state.sourcePath,
          edits: [{
            start_line: message.payload.line || 1,
            start_column: Math.max(1, (message.payload.column || 1) - 3),
            end_line: message.payload.line || 1,
            end_column: message.payload.column || 4,
            new_text: message.payload.new_name || "renamed",
          }],
        }],
        error: null,
      }, 50);
      break;
    case "lsp.codeAction":
      respond("host.lsp.codeAction", {
        request_id: message.payload.request_id,
        actions: [{
          title: "Mock: insert TODO",
          kind: "quickfix",
          is_preferred: true,
          disabled: null,
          files: [{
            path: message.payload.path || state.sourcePath,
            edits: [{
              start_line: message.payload.start_line || 1,
              start_column: message.payload.start_column || 1,
              end_line: message.payload.start_line || 1,
              end_column: message.payload.start_column || 1,
              new_text: "// TODO\n",
            }],
          }],
        }, {
          title: "Mock: rust-analyzer command",
          kind: "source",
          is_preferred: false,
          disabled: null,
          files: [],
          command: {
            command: "rust-analyzer.mockCommand",
            title: "mock",
            arguments: [],
          },
        }],
      }, 50);
      break;
    case "lsp.executeCommand":
      respond("host.lsp.executeCommand", {
        request_id: message.payload.request_id,
        files: [],
        error: message.payload.command && String(message.payload.command).startsWith("rust-analyzer.")
          ? null
          : `command \`${message.payload.command || ""}\` is not allowlisted (only rust-analyzer.*)`,
      }, 40);
      break;
    case "source.list":
      respond("host.source.tree", {
        root: "mock-project",
        code_root: ".",
        files: ["src/main.rs", "src/neon_sign.rs", "src/neon_systems.rs"],
        preferred: "src/main.rs",
      }, 40);
      break;
    case "cargo.check": {
      respond("host.process", { kind: "cargo", status: "queued", package: message.payload.package, completed: 0.05 }, 50);
      respond("host.process", { kind: "cargo", status: "checking", package: message.payload.package, completed: 0.64 }, 420);
      respond("host.process", { kind: "cargo", status: "success", package: message.payload.package, completed: 1, elapsed_ms: 842, errors: 0, warnings: 2 }, 900);
      break;
    }
    case "project.cook": {
      respond("host.process", { kind: "cook", status: "started", total: 1, completed: 0 }, 30);
      respond("host.process", {
        kind: "cook",
        status: "progress",
        path: "assets/models/hero.glb",
        index: 1,
        total: 1,
        completed: 1,
        cook_hit: false,
        error: null,
      }, 120);
      respond("host.process", {
        kind: "cook",
        status: "finished",
        total: 1,
        hits: 0,
        misses: 1,
        errors: 0,
        completed: 1,
      }, 200);
      break;
    }
    case "project.export_ypack": {
      const path = message.payload.path || "build/project.ypack";
      respond("host.process", { kind: "ypack", op: "export", status: "started", path, completed: 0.05 }, 30);
      respond("host.process", {
        kind: "ypack",
        op: "export",
        status: "finished",
        path,
        entries: 1,
        completed: 1,
      }, 140);
      break;
    }
    case "project.import_ypack": {
      const path = message.payload.path || "build/project.ypack";
      respond("host.process", { kind: "ypack", op: "import", status: "started", path, completed: 0.05 }, 30);
      respond("host.process", {
        kind: "ypack",
        op: "import",
        status: "finished",
        path,
        entries: 1,
        written: 1,
        completed: 1,
      }, 140);
      break;
    }
    case "scene.open":
      mockScenePath = message.payload.path;
      emitMockScene(45);
      respond("host.selection", { id: "entity://neon-sign-07", label: "Neon Sign 07" }, 70);
      break;
    case "scene.create": {
      mockScenePath = message.payload.path;
      mockSceneRevision = 1;
      mockSceneDirty = true;
      mockUndoStack.length = 0;
      mockRedoStack.length = 0;
      const sceneGuid = message.payload.scene_guid || `scene://${crypto.randomUUID()}`;
      const rootGuid = `entity://${crypto.randomUUID()}`;
      mockSceneDocument = {
        schema: "yuyib.scene",
        version: 1,
        scene_guid: sceneGuid,
        name: mockScenePath.replace(/\.yscene$/i, ""),
        roots: [rootGuid],
        entities: [{ guid: rootGuid, name: "Scene Root", children: [], components: [] }],
      };
      emitMockScene(45);
      respond("host.selection", { id: rootGuid, label: "Scene Root" }, 70);
      break;
    }
    case "scene.save":
      mockSceneDirty = false;
      respond("host.scene.history", { revision: mockSceneRevision, dirty: false, can_undo: mockUndoStack.length > 0, can_redo: mockRedoStack.length > 0 }, 45);
      break;
    case "project.openInteractive": {
      const path = window.prompt("Project folder (must contain project.yuyib)", "")?.trim();
      if (!path) {
        respond("host.diagnostics", { diagnostics: [{ severity: "info", source: "project.open", message: "Folder selection cancelled." }] }, 20);
        break;
      }
      respond("host.project", {
        ready: true,
        root: path,
        project: { ready: true, name: path.split(/[/\\]/).filter(Boolean).at(-1) || "project", root: path, package: null, play: { executable: null, args: [] }, scenes: [] },
      }, 40);
      respond("host.assets", { items: [], status: "ready" }, 60);
      break;
    }
    case "project.open": {
      const path = String(message.payload.path || "").trim();
      if (!path) {
        respond("host.diagnostics", { diagnostics: [{ severity: "warning", source: "project.open", message: "Project path is empty." }] }, 20);
        respond("host.project", { ready: false, root: null, project: { ready: false, name: null, root: null } }, 30);
        break;
      }
      respond("host.project", {
        ready: true,
        root: path,
        project: { ready: true, name: path.split(/[/\\]/).filter(Boolean).at(-1) || "project", root: path, package: null, play: { executable: null, args: [] }, scenes: [] },
      }, 40);
      respond("host.assets", { items: [], status: "ready" }, 60);
      break;
    }
    case "project.createInteractive": {
      const parent = window.prompt("Parent folder for the new project", "")?.trim();
      if (!parent) {
        respond("host.diagnostics", { diagnostics: [{ severity: "info", source: "project.create", message: "Folder selection cancelled." }] }, 20);
        break;
      }
      const name = message.payload.name || "untitled";
      const root = `${parent.replace(/[/\\]$/, "")}/${name}`;
      respond("host.project", {
        ready: true,
        root,
        project: { ready: true, name, root, package: null, play: { executable: null, args: [] } },
      }, 40);
      respond("host.assets", { items: [], status: "ready" }, 60);
      break;
    }
    case "scene.command": {
      if (message.payload.base_revision !== mockSceneRevision) {
        respond("host.scene.conflict", {
          path: mockScenePath,
          expected_revision: message.payload.base_revision,
          actual_revision: mockSceneRevision,
          message: "The browser mock rejected a stale scene command.",
        });
        break;
      }
      const command = message.payload.command || {};
      if (command.type === "history.undo") {
        const previous = mockUndoStack.pop();
        if (previous) {
          mockRedoStack.push(cloneMockScene());
          mockSceneDocument = previous;
          mockSceneRevision += 1;
          mockSceneDirty = true;
        }
      } else if (command.type === "history.redo") {
        const next = mockRedoStack.pop();
        if (next) {
          mockUndoStack.push(cloneMockScene());
          mockSceneDocument = next;
          mockSceneRevision += 1;
          mockSceneDirty = true;
        }
      } else {
        mockUndoStack.push(cloneMockScene());
        mockRedoStack.length = 0;
        if (command.type === "entity.create") {
          const guid = `entity://${crypto.randomUUID()}`;
          const components = command.with_transform3d
            ? [{ id: "yuyib.transform3d", label: "Transform3d", data: { translation: [0, 0, 0], rotation: [0, 0, 0, 1], scale: [1, 1, 1] }, coverage: "Visual" }]
            : [];
          mockSceneDocument.entities.push({ guid, name: command.name || "Entity", children: [], components });
          mockSceneDocument.roots = mockSceneDocument.roots || [];
          mockSceneDocument.roots.push(guid);
        } else if (command.type === "entity.delete") {
          const index = mockSceneDocument.entities.findIndex((candidate) => candidate.guid === command.entity_guid);
          if (index >= 0) {
            mockSceneDocument.entities.splice(index, 1);
            mockSceneDocument.roots = (mockSceneDocument.roots || []).filter((guid) => guid !== command.entity_guid);
          }
        } else if (command.type === "component.add") {
          const entity = mockSceneDocument.entities.find((candidate) => candidate.guid === command.entity_guid);
          if (entity && !entity.components.some((candidate) => candidate.id === command.component_id)) {
            entity.components.push({ id: command.component_id, label: command.component_id, data: {}, coverage: "Structural" });
          }
        } else if (command.type === "component.remove") {
          const entity = mockSceneDocument.entities.find((candidate) => candidate.guid === command.entity_guid);
          if (entity) {
            const index = entity.components.findIndex((candidate) => candidate.id === command.component_id);
            if (index >= 0) entity.components.splice(index, 1);
          }
        } else {
          const entity = mockSceneDocument.entities.find((candidate) => candidate.guid === command.entity_guid);
          if (entity && command.type === "entity.rename") entity.name = command.name;
          if (entity && command.type === "component.field.set") {
            const component = entity.components.find((candidate) => candidate.id === command.component_id);
            if (component) setJsonPath(component.data, command.field_path, command.value);
          }
        }
        mockSceneRevision += 1;
        mockSceneDirty = true;
      }
      emitMockScene(45);
      break;
    }
    default:
      respond("host.process", { kind: "protocol", status: "error", request_id: message.id, code: "unknown_endpoint", message: `Mock host does not implement ${message.endpoint}` });
  }
}

function post(endpoint, payload = {}) {
  const message = { version: 1, id: state.requestId++, endpoint, payload };
  if (hosted) {
    console.info("[yuyib] post →", endpoint, payload);
    if (String(endpoint).startsWith("project.")) {
      appendOutput("bridge", `post ${endpoint}`);
    }
    try {
      window.yuyib.post(message);
    } catch (error) {
      console.error("[yuyib] post failed", endpoint, error);
      appendOutput("bridge", `post FAILED ${endpoint}: ${error}`);
      throw error;
    }
  } else {
    mockHost(message);
  }
  return message.id;
}

function executeCommand(commandId, args = {}, options = {}) {
  const transactionId = options.transactionId || `ui-tx-${state.transactionId++}`;
  state.revision += 1;
  appendOutput("authoring", `${commandId} staged locally (${transactionId})`);
  if (!hosted) {
    window.setTimeout(() => emitPageEvent("host.process", {
      kind: "command",
      command_id: commandId,
      transaction_id: transactionId,
      arguments: args,
      status: "applied",
      revision: state.revision,
    }), 35);
    if (commandId === "asset.reimport") {
      window.setTimeout(() => emitPageEvent("host.process", { kind: "preview", status: "progress", stage: "decode", completed: 0.68 }), 100);
      window.setTimeout(() => emitPageEvent("host.process", { kind: "preview", status: "ready", cache: "miss", gpu_bytes: 44040192, primitive_count: 5 }), 900);
    }
    return;
  }
  showToast(
    "Host command not wired",
    `${commandId} has no native endpoint yet; scene/source/play/cargo use typed bridge posts instead`,
    "warning",
  );
}

function labelForStableId(stableId) {
  const labels = {
    "entity://district-root": "District Root",
    "entity://environment": "Environment",
    "entity://street": "Street",
    "entity://buildings": "Buildings",
    "entity://props": "Props",
    "entity://neon-sign-07": "Neon Sign 07",
    "entity://dumpster": "Dumpster",
    "entity://lighting": "Lighting",
    "entity://moon-key": "Moon Key",
    "entity://alley-fill": "Alley Fill",
    "entity://gameplay": "Gameplay",
    "asset://models/neon_arch": "neon_arch.glb",
    "asset://models/neon_sign": "neon_sign.glb",
    "asset://textures/alley_wall": "alley_wall.ktx2",
    "asset://materials/holo_ad": "holo_ad.yasset",
  };
  return labels[stableId] || stableId.split(/[/:]/).filter(Boolean).at(-1) || "Selection";
}

function showToast(title, detail, tone = "info") {
  const toast = document.createElement("div");
  toast.className = `toast toast--${tone}`;
  toast.innerHTML = `<span><strong></strong><small></small></span>`;
  toast.querySelector("strong").textContent = title;
  toast.querySelector("small").textContent = detail;
  document.querySelector("#toastStack").append(toast);
  window.setTimeout(() => toast.classList.add("is-leaving"), 2600);
  window.setTimeout(() => toast.remove(), 2850);
}

function appendOutput(scope, message) {
  const output = document.querySelector("#outputLog");
  output.append(document.createTextNode("\n"));
  const prefix = document.createElement("span");
  prefix.textContent = `[${scope}]`;
  output.append(prefix, document.createTextNode(` ${message}`));
  output.parentElement.scrollTop = output.parentElement.scrollHeight;
}

function updateSelection(payload) {
  const stableId = payload.id || payload.stable_id;
  const kind = payload.kind || (stableId?.startsWith("asset://") ? "asset" : "entity");
  state.selection = { kind, stableId, label: payload.label || labelForStableId(stableId) };
  document.querySelectorAll("[data-kind][data-id]").forEach((element) => {
    element.classList.toggle("is-selected", element.dataset.kind === kind && element.dataset.id === stableId);
  });
  const displayName = state.selection.label;
  document.querySelector("#selectionName").textContent = displayName;
  const meta = document.querySelector("#selectionMeta");
  if (meta) meta.textContent = stableId || "Open a .yscene document";
  updateSelectionCoords(payload.translation);
  document.querySelector("#inspectorName").textContent = displayName;
  document.querySelector(".inspector-titlebar small").textContent = kind === "asset" ? "Asset · imported" : "Entity · authored";

  if (kind === "asset") {
    setMainView("preview");
  } else if (state.view === "preview") {
    setMainView("scene");
  }
  renderInspector();
  drawScene();
}

function updateSelectionCoords(translation) {
  const node = document.querySelector("#selectionCoords");
  if (!node) return;
  if (!Array.isArray(translation) || translation.length < 3) {
    node.classList.add("is-hidden");
    node.textContent = "";
    return;
  }
  const [x, y, z] = translation.map((value) => Number(value));
  if (![x, y, z].every(Number.isFinite)) {
    node.classList.add("is-hidden");
    node.textContent = "";
    return;
  }
  const fmt = (value) => (Math.abs(value) < 1e-4 ? "0.00" : value.toFixed(2));
  node.innerHTML = `<span class="axis-x">X ${fmt(x)}</span>  <span class="axis-y">Y ${fmt(y)}</span>  <span class="axis-z">Z ${fmt(z)}</span>`;
  node.classList.remove("is-hidden");
}

function updateViewportAxis(payload = {}) {
  const yaw = Number(payload.yaw) || 0;
  const pitch = Number(payload.pitch) || 0;
  const cy = Math.cos(yaw);
  const sy = Math.sin(yaw);
  const cp = Math.cos(pitch);
  const sp = Math.sin(pitch);
  // Orbit eye looks toward target; project world axes into a camera-facing 2D triad.
  const project = (x, y, z) => {
    const camX = x * cy + z * sy;
    const camY = x * (-sy * sp) + y * cp + z * (cy * sp);
    return [28 + camX * 16, 28 - camY * 16];
  };
  const axes = [
    { line: "axisLineX", label: "axisLabelX", dir: [1, 0, 0] },
    { line: "axisLineY", label: "axisLabelY", dir: [0, 1, 0] },
    { line: "axisLineZ", label: "axisLabelZ", dir: [0, 0, 1] },
  ];
  for (const axis of axes) {
    const [x2, y2] = project(...axis.dir);
    const line = document.querySelector(`#${axis.line}`);
    const label = document.querySelector(`#${axis.label}`);
    if (line) {
      line.setAttribute("x2", x2.toFixed(1));
      line.setAttribute("y2", y2.toFixed(1));
    }
    if (label) {
      label.setAttribute("x", (x2 + (x2 - 28) * 0.25).toFixed(1));
      label.setAttribute("y", (y2 + (y2 - 28) * 0.25 + 3).toFixed(1));
    }
  }
}

function updateAssetPreviewPanel(payload = {}) {
  const path = payload.path || payload.name || "—";
  const name = payload.name || path.split(/[/\\]/).pop() || path;
  state.assetPreview = {
    id: payload.id || state.selection?.stableId || path,
    path,
    name,
    kind: payload.kind || "asset",
    tracking: payload.tracking || state.assetPreview?.tracking || null,
    importSettings: payload.import_settings || state.assetPreview?.importSettings || null,
    dependencies: payload.dependencies ?? state.assetPreview?.dependencies ?? null,
    dependents: payload.dependents ?? state.assetPreview?.dependents ?? null,
    dependencyDiagnostics: payload.dependency_diagnostics ?? state.assetPreview?.dependencyDiagnostics ?? null,
  };
  const title = document.querySelector("#assetPreviewTitle");
  const subtitle = document.querySelector("#assetPreviewSubtitle");
  if (title) title.textContent = name;
  if (subtitle) {
    const tracking = payload.tracking ? ` · ${payload.tracking}` : "";
    subtitle.textContent = `${payload.kind || "asset"} · ${path}${tracking}`;
  }
  const place = document.querySelector("#placeAssetInSceneButton");
  if (place) place.disabled = !/\.(glb|gltf)$/i.test(path);
  if (state.selection?.kind === "asset" || payload.import_settings) {
    state.selection = {
      kind: "asset",
      stableId: state.assetPreview.id,
      label: name,
    };
    renderInspector();
  }
}

function updateAssetPreviewStats({ path, primitives, gpuMb }) {
  if (path) {
    updateAssetPreviewPanel({
      id: state.assetPreview?.id,
      path,
      name: path.split(/[/\\]/).pop() || path,
      kind: "model",
      tracking: state.assetPreview?.tracking,
      import_settings: state.assetPreview?.importSettings,
      dependencies: state.assetPreview?.dependencies,
      dependency_diagnostics: state.assetPreview?.dependencyDiagnostics,
    });
  }
  const meshes = document.querySelector("#assetPreviewMeshes");
  const gpu = document.querySelector("#assetPreviewGpu");
  if (meshes) meshes.textContent = String(primitives ?? "—");
  if (gpu) gpu.textContent = Number.isFinite(gpuMb) ? `${gpuMb} MB` : "—";
}

function looksLikeAssetGuidRef(value) {
  if (!value) return false;
  const raw = String(value).replace(/^asset:\/\//i, "").trim();
  return /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(raw);
}

function modelRefForPlace() {
  const preview = state.assetPreview;
  if (!preview) return null;
  // Prefer stable GUID when the asset is tracked (host sends asset://{guid}).
  if (looksLikeAssetGuidRef(preview.id)) {
    return String(preview.id).startsWith("asset://") ? preview.id : `asset://${preview.id}`;
  }
  const path = preview.path;
  // Preview may still hold a path id after Track — look up the refreshed index.
  if (path) {
    const tracked = (state.assets || []).find((asset) => {
      if (!(asset.kind === "model" || /\.(glb|gltf)$/i.test(asset.path || ""))) return false;
      if (asset.path !== path && asset.path !== preview.path) return false;
      return asset.tracking === "tracked" && looksLikeAssetGuidRef(asset.id);
    });
    if (tracked?.id) {
      return String(tracked.id).startsWith("asset://") ? tracked.id : `asset://${tracked.id}`;
    }
  }
  if (path && /\.(glb|gltf)$/i.test(path)) return path;
  return null;
}

function placeSelectedAssetInScene() {
  const modelRef = modelRefForPlace();
  const path = state.assetPreview?.path;
  if (!modelRef || !path || !/\.(glb|gltf)$/i.test(path)) {
    showToast("No glTF selected", "Open a .glb/.gltf in Assets first", "warning");
    return;
  }
  if (!state.scene.document) {
    showToast("No scene open", "Open a .yscene before placing the asset", "warning");
    return;
  }
  const name = (path.split(/[/\\]/).pop() || "Model").replace(/\.(glb|gltf)$/i, "");
  showToast("Placing model", `${name} ← ${modelRef}`, "info");
  setMainView("scene");
  sendSceneCommand({ type: "entity.create", name, with_transform3d: true });
  // After create, host republishes the document; place model on the newest matching entity next tick.
  window.setTimeout(() => {
    const entities = state.scene.document?.entities || [];
    const entity = [...entities].reverse().find((item) => item.name === name) || entities.at(-1);
    if (!entity) {
      showToast("Place failed", "Could not find the created entity", "warning");
      return;
    }
    const hasModel = (entity.components || []).some((component) => (component.schema || component.id) === "yuyib.model3d");
    const afterAdd = () => {
      sendSceneCommand({
        type: "component.field.set",
        entity_guid: entity.guid,
        component_id: "yuyib.model3d",
        field_path: "model",
        value: modelRef,
      });
      showToast("Placed in scene", `${name} → ${modelRef} (loading if needed)`, "success");
    };
    if (!hasModel) {
      sendSceneCommand({ type: "component.add", entity_guid: entity.guid, component_id: "yuyib.model3d" });
      window.setTimeout(afterAdd, 80);
    } else {
      afterAdd();
    }
  }, 120);
}

function setPlayMode(mode) {
  const normalizedMode = mode === "running" || mode === "started" ? "playing" : mode;
  state.playMode = normalizedMode;
  document.querySelector("#playButton").classList.toggle("is-playing", normalizedMode === "playing");
  document.querySelector("#pauseButton").classList.toggle("is-active", normalizedMode === "paused");
  const labels = { playing: "Play runner started", paused: "Play runner paused", stopped: "Play runner stopped" };
  showToast(labels[normalizedMode] || "Play process updated", hosted ? "State reported by host.process" : "Browser mock process state", normalizedMode === "stopped" ? "info" : "success");
}

function sceneEntityMap() {
  return new Map((state.scene.document?.entities || []).map((entity) => [entity.guid, entity]));
}

function selectedSceneEntity() {
  if (state.selection?.kind !== "entity") return null;
  return sceneEntityMap().get(state.selection.stableId) || null;
}

function configureAvailableComponents(components) {
  if (!Array.isArray(components)) return;
  state.availableComponents = components
    .map((component) => typeof component === "string"
      ? { id: component, label: component }
      : { id: component?.id || component?.component_id, label: component?.label || component?.name || component?.id || component?.component_id })
    .filter((component) => typeof component.id === "string" && component.id.length > 0);
}

function availableComponentsForEntity(entity = selectedSceneEntity()) {
  const assigned = new Set((entity?.components || []).map((component) => component.id));
  return state.availableComponents.filter((component) => !assigned.has(component.id));
}

function closeAddComponentDialog() {
  const dialog = document.querySelector("#addComponentDialog");
  if (!dialog.hidden) {
    dialog.hidden = true;
    document.querySelector("#addComponentButton")?.focus();
    setNativeViewportVisible(true);
  }
}

let pendingRevisionConflict = null;

function closeRevisionConflictDialog() {
  const dialog = document.querySelector("#revisionConflictDialog");
  if (!dialog.hidden) {
    dialog.hidden = true;
    pendingRevisionConflict = null;
    setNativeViewportVisible(true);
  }
}

function formatRevisionPair(expected, actual) {
  const parts = [];
  if (expected !== undefined && expected !== null && expected !== "") parts.push(`expected ${expected}`);
  if (actual !== undefined && actual !== null && actual !== "") parts.push(`actual ${actual}`);
  return parts.join(" · ");
}

function showRevisionConflictDialog(kind, payload = {}) {
  const path = payload.path || (kind === "scene" ? state.scene.path : state.sourcePath) || "—";
  const expected = payload.expected_revision ?? payload.expected;
  const actual = payload.actual_revision ?? payload.actual;
  const message = payload.message || (kind === "scene"
    ? "The scene changed outside the Editor. Reload to discard local edits and pick up the latest revision."
    : "The source file changed outside the Editor. Reload to discard local edits and pick up the latest revision.");
  pendingRevisionConflict = { kind, path };

  document.querySelector("#revisionConflictTitle").textContent = kind === "scene"
    ? "Scene revision conflict"
    : "Source revision conflict";
  document.querySelector("#revisionConflictPath").textContent = path;
  document.querySelector("#revisionConflictMessage").textContent = message;
  const revisions = formatRevisionPair(expected, actual);
  const revisionsElement = document.querySelector("#revisionConflictRevisions");
  revisionsElement.textContent = revisions;
  revisionsElement.hidden = !revisions;

  document.querySelector("#revisionConflictDialog").hidden = false;
  setNativeViewportVisible(false);
  window.requestAnimationFrame(() => document.querySelector("#revisionConflictReload")?.focus());
}

function reloadRevisionConflict() {
  if (!pendingRevisionConflict) return;
  const { kind, path } = pendingRevisionConflict;
  closeRevisionConflictDialog();
  if (kind === "scene") {
    const scenePath = path || state.scene.path;
    if (!scenePath) {
      showToast("No scene path", "Cannot reload without an open scene path", "warning");
      return;
    }
    post("scene.open", { path: scenePath });
    appendOutput("scene", `Reload requested for ${scenePath}`);
    return;
  }
  const sourcePath = path || state.sourcePath;
  if (!sourcePath) {
    showToast("No source path", "Cannot reload without an open source path", "warning");
    return;
  }
  post("source.read", { path: sourcePath });
  appendOutput("source", `Reload requested for ${sourcePath}`);
}

function openAddComponentDialog() {
  const entity = selectedSceneEntity();
  if (!entity) {
    showToast("Select an entity", "Choose an entity in the hierarchy before adding a component", "warning");
    return;
  }
  const dialog = document.querySelector("#addComponentDialog");
  const options = document.querySelector("#addComponentOptions");
  const components = availableComponentsForEntity(entity);
  document.querySelector("#addComponentDialogEntity").textContent = entity.name || entity.guid;
  options.replaceChildren();
  if (!components.length) {
    const empty = document.createElement("p");
    empty.className = "component-dialog__empty";
    empty.textContent = "All supported components are already attached to this entity.";
    options.append(empty);
  } else {
    components.forEach((component) => {
      const option = document.createElement("button");
      option.type = "button";
      option.className = "component-dialog__option";
      const label = document.createElement("strong");
      label.textContent = component.label;
      const id = document.createElement("small");
      id.textContent = component.id;
      option.append(label, id);
      option.addEventListener("click", () => {
        closeAddComponentDialog();
        sendSceneCommand({ type: "component.add", entity_guid: entity.guid, component_id: component.id });
      });
      options.append(option);
    });
  }
  dialog.hidden = false;
  setNativeViewportVisible(false);
  window.requestAnimationFrame(() => options.querySelector("button")?.focus());
}

function makeSvgIcon(symbolId, className = "") {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  if (className) svg.setAttribute("class", className);
  const use = document.createElementNS("http://www.w3.org/2000/svg", "use");
  use.setAttribute("href", `#${symbolId}`);
  svg.append(use);
  return svg;
}

function renderSceneTree() {
  const tree = document.querySelector("#sceneTree");
  const sceneDocument = state.scene.document;
  tree.replaceChildren();
  document.querySelector("#scenePathLabel").textContent = state.scene.path || "No scene open";
  if (!sceneDocument) {
    const empty = document.createElement("div");
    empty.className = "data-empty-state";
    empty.textContent = "Open a .yscene document to inspect its hierarchy";
    tree.append(empty);
    return;
  }

  const entities = sceneEntityMap();
  const roots = Array.isArray(sceneDocument.roots) && sceneDocument.roots.length
    ? sceneDocument.roots
    : [...entities.values()].filter((entity) => !entity.parent_guid).map((entity) => entity.guid);
  const visited = new Set();

  function appendEntity(entityGuid, depth) {
    const entity = entities.get(entityGuid);
    if (!entity || visited.has(entityGuid)) return;
    visited.add(entityGuid);
    const children = Array.isArray(entity.children) ? entity.children.filter((child) => entities.has(child)) : [];
    const row = document.createElement("button");
    row.className = `tree-row${depth === 0 ? " tree-row--root" : ""}`;
    row.style.paddingLeft = `${7 + depth * 14}px`;
    row.dataset.kind = "entity";
    row.dataset.id = entity.guid;
    row.draggable = hosted && !state.scene.readOnly;
    row.classList.toggle("is-selected", state.selection?.stableId === entity.guid);
    if (children.length) {
      row.append(makeSvgIcon("i-chevron", "tree-chevron is-open"));
    } else {
      const spacer = document.createElement("span");
      spacer.className = "tree-spacer";
      row.append(spacer);
    }
    const componentIds = new Set((entity.components || []).map((component) => component.id));
    const icon = document.createElement("span");
    icon.className = componentIds.has("yuyib.model3d")
      ? "entity-icon entity-icon--mesh"
      : [...componentIds].some((id) => id.includes("light"))
        ? "entity-icon entity-icon--light"
        : children.length
          ? "entity-icon entity-icon--group"
          : "entity-icon entity-icon--code";
    row.append(icon);
    const label = document.createElement("span");
    label.textContent = entity.name || entity.guid;
    row.append(label);
    if (children.length) {
      const count = document.createElement("span");
      count.className = "row-count";
      count.textContent = String(children.length);
      row.append(count);
    }
    if (hosted && !state.scene.readOnly) {
      row.addEventListener("dragstart", (event) => {
        event.dataTransfer.setData("text/yuyib-entity", entity.guid);
        event.dataTransfer.effectAllowed = "move";
        row.classList.add("is-dragging");
      });
      row.addEventListener("dragend", () => row.classList.remove("is-dragging"));
      row.addEventListener("dragover", (event) => {
        event.preventDefault();
        event.dataTransfer.dropEffect = "move";
        row.classList.add("is-drop-target");
      });
      row.addEventListener("dragleave", () => row.classList.remove("is-drop-target"));
      row.addEventListener("drop", (event) => {
        event.preventDefault();
        row.classList.remove("is-drop-target");
        const childGuid = event.dataTransfer.getData("text/yuyib-entity");
        if (!childGuid || childGuid === entity.guid) return;
        if (isHierarchyDescendant(childGuid, entity.guid, entities)) {
          showToast("Invalid reparent", "Cannot parent an entity under its own descendant", "warning");
          return;
        }
        reparentEntity(childGuid, entity.guid);
      });
    }
    tree.append(row);
    children.forEach((child) => appendEntity(child, depth + 1));
  }

  roots.forEach((root) => appendEntity(root, 0));
  [...entities.keys()].filter((guid) => !visited.has(guid)).forEach((guid) => appendEntity(guid, 0));

  if (hosted && !state.scene.readOnly) {
    tree.addEventListener("dragover", (event) => {
      if (event.target !== tree) return;
      event.preventDefault();
      event.dataTransfer.dropEffect = "move";
    });
    tree.addEventListener("drop", (event) => {
      if (event.target !== tree) return;
      event.preventDefault();
      const childGuid = event.dataTransfer.getData("text/yuyib-entity");
      if (!childGuid) return;
      reparentEntity(childGuid, null);
    });
  }
}

function isHierarchyDescendant(ancestorGuid, candidateGuid, entities) {
  let current = entities.get(candidateGuid);
  const guard = new Set();
  while (current?.parent_guid) {
    if (current.parent_guid === ancestorGuid) return true;
    if (guard.has(current.parent_guid)) break;
    guard.add(current.parent_guid);
    current = entities.get(current.parent_guid);
  }
  return false;
}

function reparentEntity(childGuid, parentGuid) {
  const entity = sceneEntityMap().get(childGuid);
  if (!entity) return;
  const hasParentComponent = (entity.components || []).some((component) => component.id === "yuyib.parent3d");
  if (!hasParentComponent) {
    showToast("Missing Parent 3D", "Add a Parent 3D component before reparenting this entity", "warning");
    return;
  }
  sendSceneCommand({
    type: "component.field.set",
    entity_guid: childGuid,
    component_id: "yuyib.parent3d",
    field_path: "parent",
    value: parentGuid,
  });
}

function createFieldControl(component, field, value) {
  let control;
  if (field.kind === "boolean") {
    control = document.createElement("input");
    control.type = "checkbox";
    control.className = "switch";
    control.checked = Boolean(value);
  } else if (field.kind === "enum") {
    control = document.createElement("select");
    for (const optionValue of field.options || []) {
      const option = document.createElement("option");
      option.value = String(optionValue);
      option.textContent = String(optionValue);
      option.selected = optionValue === value;
      control.append(option);
    }
  } else if (field.kind === "asset") {
    control = document.createElement("select");
    control.className = "asset-field";
    const current = value === undefined || value === null ? "" : String(value);
    const none = document.createElement("option");
    none.value = "";
    none.textContent = "— none —";
    none.selected = !current;
    control.append(none);
    const modelAssets = (state.assets || []).filter((asset) =>
      /\.(glb|gltf)$/i.test(asset.path || asset.name || "")
    );
    let matched = false;
    for (const asset of modelAssets) {
      const option = document.createElement("option");
      const path = asset.path || asset.id || "";
      option.value = path;
      option.textContent = asset.name || path.split(/[/\\]/).pop() || path;
      if (
        path === current
        || `asset://${path}` === current
        || path.replace(/^asset:\/\//, "") === current.replace(/^asset:\/\//, "")
      ) {
        option.selected = true;
        matched = true;
      }
      control.append(option);
    }
    if (current && !matched) {
      const custom = document.createElement("option");
      custom.value = current;
      custom.textContent = current;
      custom.selected = true;
      control.append(custom);
    }
  } else if (field.kind === "number" || field.kind === "f32" || field.kind === "i32" || field.kind === "u32") {
    control = document.createElement("input");
    control.className = "number-field";
    control.type = "number";
    control.dataset.fieldKind = "number";
    control.value = Number.isFinite(Number(value)) ? String(value) : "0";
    if (field.step !== undefined) control.step = String(field.step);
    if (field.min !== undefined) control.min = String(field.min);
    if (field.max !== undefined) control.max = String(field.max);
  } else if (field.kind === "color" && typeof value === "string" && /^#[0-9a-f]{6}$/i.test(value)) {
    control = document.createElement("input");
    control.type = "color";
    control.value = value;
  } else {
    control = document.createElement("input");
    control.type = "text";
    control.value = value === undefined || value === null ? "" : typeof value === "object" ? JSON.stringify(value) : String(value);
    if (field.kind === "string") control.placeholder = "";
  }
  control.dataset.sceneField = field.path;
  control.dataset.componentId = component.id;
  if (!control.dataset.fieldKind) {
    control.dataset.fieldKind = field.kind === "boolean" ? "boolean" : field.kind || "string";
  }
  const locked = Boolean(
    field.read_only
    || state.scene.readOnly
    || field.kind === "specialized"
    || (field.kind === "color" && control.type !== "color"),
  );
  control.disabled = locked;
  if (locked) {
    const reason = state.scene.readOnly
      ? "Scene document is read-only (newer format_version)"
      : (field.read_only_reason || (field.kind === "specialized"
        ? "Specialized field — no Inspector control yet"
        : "Field is read-only"));
    control.title = reason;
  }
  return control;
}

function renderComponentCard(component) {
  const descriptor = state.componentCoverage.get(component.id);
  const card = document.createElement("section");
  card.className = "component-card is-open";
  card.dataset.component = component.id;
  const header = document.createElement("header");
  const chevron = document.createElement("button");
  chevron.className = "component-chevron";
  chevron.append(makeSvgIcon("i-chevron"));
  const icon = document.createElement("span");
  icon.className = "component-icon component-icon--model";
  icon.textContent = (descriptor?.label || component.id).charAt(0).toUpperCase();
  const title = document.createElement("strong");
  title.textContent = descriptor?.label || component.id;
  const id = document.createElement("span");
  id.className = "component-id";
  id.textContent = `${component.id}@${component.schema_version ?? descriptor?.schema_version ?? "?"}`;
  header.append(chevron, icon, title, id);
  if (hosted) {
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "component-remove-button icon-button icon-button--small";
    remove.title = `Remove ${descriptor?.label || component.id}`;
    remove.textContent = "×";
    remove.disabled = state.scene.readOnly;
    remove.addEventListener("click", (event) => {
      event.stopPropagation();
      removeComponent(component);
    });
    header.append(remove);
  }
  header.addEventListener("click", () => card.classList.toggle("is-open"));
  card.append(header);

  const body = document.createElement("div");
  body.className = "component-body property-list";
  const fields = descriptor?.fields || [];
  if (fields.length) {
    const lockedReasons = [...new Set(
      fields
        .map((field) => field.read_only_reason)
        .filter(Boolean),
    )];
    if (lockedReasons.length || state.scene.readOnly) {
      const notice = document.createElement("div");
      notice.className = "opaque-component-notice";
      const help = document.createElement("small");
      help.textContent = state.scene.readOnly
        ? "Scene is read-only — Inspector edits are blocked."
        : lockedReasons[0];
      notice.append(help);
      body.append(notice);
    }
    fields.forEach((field) => {
      const label = document.createElement("label");
      const caption = document.createElement("span");
      caption.textContent = field.label || field.path;
      caption.title = field.read_only_reason
        || (field.group ? `${field.group} · ${field.path}` : field.path);
      label.append(caption, createFieldControl(component, field, getJsonPath(component.data, field.path)));
      body.append(label);
    });
  } else {
    const notice = document.createElement("div");
    notice.className = "opaque-component-notice";
    const heading = document.createElement("strong");
    heading.textContent = "Opaque component payload";
    const help = document.createElement("small");
    help.textContent = "No field descriptors were supplied by host.coverage. Data remains read-only and must be preserved by the host.";
    const payload = document.createElement("pre");
    payload.textContent = JSON.stringify(component.data, null, 2);
    notice.append(heading, help, payload);
    body.append(notice);
  }
  card.append(body);
  return card;
}

function renderInspector() {
  const list = document.querySelector("#componentList");
  const nameInput = document.querySelector("#entityNameInput");
  list.replaceChildren();

  if (state.selection?.kind === "asset" || (state.view === "preview" && state.assetPreview)) {
    renderAssetInspector(list, nameInput);
    return;
  }

  const entity = selectedSceneEntity();
  if (!entity) {
    document.querySelector("#inspectorName").textContent = "No selection";
    document.querySelector("#inspectorGuid").textContent = "—";
    nameInput.value = "";
    nameInput.disabled = true;
    const deleteIdle = document.querySelector("#deleteEntityButton");
    if (deleteIdle) deleteIdle.disabled = true;
    const addIdle = document.querySelector("#addComponentButton");
    if (addIdle) {
      addIdle.disabled = true;
      addIdle.hidden = false;
    }
    const empty = document.createElement("div");
    empty.className = "data-empty-state";
    empty.textContent = state.scene.document ? "Select an entity to inspect components" : "Waiting for host.scene.document";
    list.append(empty);
    updateSourceNavigation(null);
    return;
  }
  document.querySelector("#inspectorName").textContent = entity.name || entity.guid;
  document.querySelector(".inspector-titlebar small").textContent = "Entity · authored";
  document.querySelector("#inspectorGuid").textContent = entity.guid;
  nameInput.value = entity.name || "";
  nameInput.disabled = state.scene.readOnly;
  const deleteButton = document.querySelector("#deleteEntityButton");
  if (deleteButton) deleteButton.disabled = state.scene.readOnly;
  const addButton = document.querySelector("#addComponentButton");
  if (addButton) {
    addButton.hidden = false;
    addButton.disabled = state.scene.readOnly || !availableComponentsForEntity(entity).length;
  }
  (entity.components || []).forEach((component) => list.append(renderComponentCard(component)));
  if (!(entity.components || []).length) {
    const empty = document.createElement("div");
    empty.className = "data-empty-state";
    empty.textContent = "This entity has no authored components";
    list.append(empty);
  }
  updateSourceNavigation(entity);
}

const GLTF_IMPORT_POLICIES = [
  { value: "default", label: "Default (strict)" },
  { value: "strict", label: "Strict" },
  { value: "static_preview", label: "Static preview" },
  { value: "skeletal", label: "Skeletal" },
  { value: "skeletal_preview", label: "Skeletal preview" },
];

/** Prefer readable labels when glTF names contain replacement chars / undecodable bytes. */
function formatMeshOptionLabel(entry) {
  const raw = typeof entry.name === "string" ? entry.name.trim() : "";
  const usable = raw && !raw.includes("\uFFFD") ? raw : "";
  return usable ? `${entry.index}: ${usable}` : `Mesh ${entry.index}`;
}

function formatMaterialOptionLabel(entry) {
  const raw = typeof entry.name === "string" ? entry.name.trim() : "";
  const usable = raw && !raw.includes("\uFFFD") ? raw : "";
  return usable ? `${entry.index}: ${usable}` : `Material ${entry.index}`;
}

function renderAssetInspector(list, nameInput) {
  const preview = state.assetPreview || {};
  const settings = preview.importSettings || {};
  const editable = Boolean(settings.editable);
  const payload = settings.payload || {};
  const policy = payload.policy || "default";
  const id = preview.id || state.selection?.stableId || "—";

  document.querySelector("#inspectorName").textContent = preview.name || preview.path || "Asset";
  document.querySelector(".inspector-titlebar small").textContent = "Asset · import settings";
  document.querySelector("#inspectorGuid").textContent = id;
  nameInput.value = preview.path || "";
  nameInput.disabled = true;
  const deleteButton = document.querySelector("#deleteEntityButton");
  if (deleteButton) deleteButton.disabled = true;
  const addButton = document.querySelector("#addComponentButton");
  if (addButton) {
    addButton.disabled = true;
    addButton.hidden = true;
  }

  const card = document.createElement("article");
  card.className = "component-card";
  const header = document.createElement("header");
  header.innerHTML = `<strong>glTF Import Settings</strong><small>${settings.schema || "yuyib.gltf-import-settings"}@${settings.version || 1}</small>`;
  card.append(header);

  if (!editable) {
    const hint = document.createElement("p");
    hint.className = "field-hint";
    hint.textContent = settings.reason === "track_required"
      ? "Track this glTF (Assets · T) to edit and persist import settings."
      : "Import settings are read-only for this asset.";
    card.append(hint);
  }

  const field = document.createElement("label");
  field.className = "field";
  const span = document.createElement("span");
  span.textContent = "Policy";
  const select = document.createElement("select");
  select.disabled = !editable;
  GLTF_IMPORT_POLICIES.forEach((option) => {
    const node = document.createElement("option");
    node.value = option.value;
    node.textContent = option.label;
    if (option.value === policy) node.selected = true;
    select.append(node);
  });
  select.addEventListener("change", () => {
    if (!editable) return;
    const nextPayload = { ...(payload || {}), policy: select.value };
    post("asset.import_settings.save", { id, payload: nextPayload });
    showToast("Saving import settings", select.value, "info");
  });
  field.append(span, select);
  card.append(field);

  const meshes = Array.isArray(state.previewMeshes) ? state.previewMeshes : [];
  if (meshes.length) {
    const meshField = document.createElement("label");
    meshField.className = "field";
    const meshSpan = document.createElement("span");
    meshSpan.textContent = "Mesh";
    const meshSelect = document.createElement("select");
    const allOption = document.createElement("option");
    allOption.value = "";
    allOption.textContent = `All meshes (${meshes.length})`;
    meshSelect.append(allOption);
    meshes.forEach((entry) => {
      const option = document.createElement("option");
      option.value = String(entry.index);
      const label = formatMeshOptionLabel(entry);
      option.textContent = label;
      option.title = entry.name && entry.name !== label ? entry.name : label;
      if (state.previewSelectedMesh != null && Number(state.previewSelectedMesh) === Number(entry.index)) {
        option.selected = true;
      }
      meshSelect.append(option);
    });
    if (state.previewSelectedMesh == null) allOption.selected = true;
    meshSelect.addEventListener("change", () => {
      const value = meshSelect.value;
      const index = value === "" ? null : Number(value);
      state.previewSelectedMesh = index;
      if (hosted) {
        post("preview.selection.set", { kind: "mesh", index });
      } else {
        executeCommand("preview.selection.set", { kind: "mesh", index });
      }
    });
    meshField.append(meshSpan, meshSelect);
    card.append(meshField);
  }

  const materials = Array.isArray(state.previewMaterials) ? state.previewMaterials : [];
  if (materials.length) {
    const materialField = document.createElement("label");
    materialField.className = "field";
    const materialSpan = document.createElement("span");
    materialSpan.textContent = "Material";
    const materialSelect = document.createElement("select");
    const allMaterials = document.createElement("option");
    allMaterials.value = "";
    allMaterials.textContent = `All materials (${materials.length})`;
    materialSelect.append(allMaterials);
    materials.forEach((entry) => {
      const option = document.createElement("option");
      option.value = String(entry.index);
      const label = formatMaterialOptionLabel(entry);
      option.textContent = label;
      option.title = entry.name && entry.name !== label ? entry.name : label;
      if (state.previewSelectedMaterial != null && Number(state.previewSelectedMaterial) === Number(entry.index)) {
        option.selected = true;
      }
      materialSelect.append(option);
    });
    if (state.previewSelectedMaterial == null) allMaterials.selected = true;
    materialSelect.addEventListener("change", () => {
      const value = materialSelect.value;
      const index = value === "" ? null : Number(value);
      state.previewSelectedMaterial = index;
      state.previewMaterialOverride = null;
      if (hosted) {
        post("preview.selection.set", { kind: "material", index });
      } else {
        executeCommand("preview.selection.set", { kind: "material", index });
      }
    });
    materialField.append(materialSpan, materialSelect);
    card.append(materialField);

    if (state.previewSelectedMaterial != null) {
      const override = state.previewMaterialOverride || {};
      const postOverride = () => {
        if (!hosted) return;
        post("preview.material_override.set", {
          material_index: state.previewSelectedMaterial,
          parameters: state.previewMaterialOverride,
        });
      };
      const addFactor = (label, key, count, defaults) => {
        const field = document.createElement("label");
        field.className = "field";
        const span = document.createElement("span");
        span.textContent = label;
        const values = Array.isArray(override[key])
          ? override[key]
          : (count === 1 && typeof override[key] === "number" ? [override[key]] : defaults);
        const inputs = [];
        for (let i = 0; i < count; i += 1) {
          const input = document.createElement("input");
          input.type = "number";
          input.step = "0.01";
          input.value = String(values[i]);
          input.addEventListener("change", () => {
            state.previewMaterialOverride = state.previewMaterialOverride || {};
            const next = inputs.map((entry) => Number(entry.value));
            state.previewMaterialOverride[key] = count === 1 ? next[0] : next;
            postOverride();
          });
          inputs.push(input);
        }
        field.append(span, ...inputs);
        card.append(field);
      };
      addFactor("Base color", "base_color_factor", 4, [1, 1, 1, 1]);
      addFactor("Metallic", "metallic_factor", 1, [1]);
      addFactor("Roughness", "roughness_factor", 1, [1]);
      addFactor("Emissive", "emissive_factor", 3, [0, 0, 0]);

      const sidedField = document.createElement("label");
      sidedField.className = "field";
      const sidedInput = document.createElement("input");
      sidedInput.type = "checkbox";
      sidedInput.checked = Boolean(override.double_sided);
      sidedInput.addEventListener("change", () => {
        state.previewMaterialOverride = state.previewMaterialOverride || {};
        state.previewMaterialOverride.double_sided = sidedInput.checked;
        postOverride();
      });
      sidedField.append(document.createTextNode("Double sided"), sidedInput);
      card.append(sidedField);

      const textures = Array.isArray(state.previewTextures) ? state.previewTextures : [];
      if (textures.length) {
        const addTextureSelect = (label, key) => {
          const field = document.createElement("label");
          field.className = "field";
          const span = document.createElement("span");
          span.textContent = label;
          const select = document.createElement("select");
          const keep = document.createElement("option");
          keep.value = "";
          keep.textContent = "(unchanged)";
          select.append(keep);
          const clear = document.createElement("option");
          clear.value = "null";
          clear.textContent = "(clear texture)";
          select.append(clear);
          textures.forEach((entry) => {
            const option = document.createElement("option");
            option.value = String(entry.index);
            option.textContent = entry.name ? `${entry.index}: ${entry.name}` : `texture_${entry.index}`;
            select.append(option);
          });
          if (override[key] === null) clear.selected = true;
          else if (override[key] != null && override[key] !== "") {
            select.value = String(override[key]);
          } else {
            keep.selected = true;
          }
          select.addEventListener("change", () => {
            state.previewMaterialOverride = state.previewMaterialOverride || {};
            if (select.value === "") delete state.previewMaterialOverride[key];
            else if (select.value === "null") state.previewMaterialOverride[key] = null;
            else state.previewMaterialOverride[key] = Number(select.value);
            if (Object.keys(state.previewMaterialOverride).length === 0) {
              state.previewMaterialOverride = null;
            }
            postOverride();
          });
          field.append(span, select);
          card.append(field);
        };
        addTextureSelect("Base color tex", "base_color_texture");
        addTextureSelect("MetallicRoughness tex", "metallic_roughness_texture");
        addTextureSelect("Emissive tex", "emissive_texture");
        addTextureSelect("Normal tex", "normal_texture");
      }

      const reset = document.createElement("button");
      reset.type = "button";
      reset.textContent = "Reset material override";
      reset.addEventListener("click", () => {
        state.previewMaterialOverride = null;
        postOverride();
        renderInspector();
      });
      card.append(reset);
    }
  }

  const animations = Array.isArray(state.previewAnimations) ? state.previewAnimations : [];
  if (animations.length) {
    const animField = document.createElement("label");
    animField.className = "field";
    const animSpan = document.createElement("span");
    animSpan.textContent = "Animation";
    const animSelect = document.createElement("select");
    const noneOption = document.createElement("option");
    noneOption.value = "";
    noneOption.textContent = "Bind pose (no clip)";
    animSelect.append(noneOption);
    animations.forEach((entry) => {
      const option = document.createElement("option");
      option.value = String(entry.index);
      const name = entry.name || `Clip ${entry.index}`;
      const duration =
        typeof entry.duration_seconds === "number" ? entry.duration_seconds.toFixed(2) : "?";
      option.textContent = `${name} (${duration}s)`;
      option.title = entry.name || option.textContent;
      if (
        state.previewSelectedAnimation != null &&
        Number(state.previewSelectedAnimation) === Number(entry.index)
      ) {
        option.selected = true;
      }
      animSelect.append(option);
    });
    if (state.previewSelectedAnimation == null) noneOption.selected = true;
    animSelect.addEventListener("change", () => {
      const value = animSelect.value;
      const index = value === "" ? null : Number(value);
      state.previewSelectedAnimation = index;
      if (hosted) {
        post("preview.selection.set", { kind: "animation", index });
      } else {
        executeCommand("preview.selection.set", { kind: "animation", index });
      }
    });
    animField.append(animSpan, animSelect);
    card.append(animField);
  }

  const tracking = document.createElement("p");
  tracking.className = "field-hint";
  tracking.textContent = `Tracking: ${preview.tracking || "n/a"} · Path: ${preview.path || "—"}`;
  card.append(tracking);

  const deps = Array.isArray(preview.dependencies) ? preview.dependencies : [];
  const dependents = Array.isArray(preview.dependents) ? preview.dependents : [];
  const unresolved = Array.isArray(preview.dependencyDiagnostics) ? preview.dependencyDiagnostics : [];
  if (deps.length || dependents.length || unresolved.length) {
    const depCard = document.createElement("article");
    depCard.className = "component-card";
    const depHeader = document.createElement("header");
    depHeader.innerHTML = `<strong>Dependencies</strong><small>${deps.length} out · ${dependents.length} in · ${unresolved.length} unresolved</small>`;
    depCard.append(depHeader);
    if (deps.length) {
      const listEl = document.createElement("ul");
      listEl.className = "field-hint";
      deps.forEach((guid) => {
        const item = document.createElement("li");
        item.textContent = `→ ${guid}`;
        listEl.append(item);
      });
      depCard.append(listEl);
    }
    if (dependents.length) {
      const listEl = document.createElement("ul");
      listEl.className = "field-hint";
      dependents.forEach((guid) => {
        const item = document.createElement("li");
        const button = document.createElement("button");
        button.type = "button";
        button.className = "linkish";
        button.textContent = `← ${guid}`;
        button.title = "Open dependent asset";
        button.addEventListener("click", () => {
          if (hosted) post("asset.open", { id: guid });
          else executeCommand("asset.open", { asset_guid: guid });
        });
        item.append(button);
        listEl.append(item);
      });
      depCard.append(listEl);
    }
    unresolved.forEach((entry) => {
      const hint = document.createElement("p");
      hint.className = "field-hint";
      hint.textContent = `${entry.code || "unresolved"}: ${entry.uri || "—"}${entry.message ? ` — ${entry.message}` : ""}`;
      depCard.append(hint);
    });
    list.append(card, depCard);
  } else {
    list.append(card);
  }
  updateSourceNavigation(null);
}

function updateSourceNavigation(entity) {
  const component = entity?.components?.[0];
  const descriptor = component ? state.componentCoverage.get(component.id || component.schema) : null;
  document.querySelector(".coverage-chip").textContent = descriptor?.status || (component ? "Opaque" : "No component");
  const systemsForComponent = findSystemsForComponent(component?.id || component?.schema);
  document.querySelectorAll("[data-open-source]").forEach((button) => {
    const kind = button.dataset.openSource;
    let path = "";
    let hint = "Not provided by host coverage";
    if (kind === "entity.projection") {
      path = entity?.projection_path || "";
      hint = path || "Save/Sync Code to create entity projection";
    } else if (kind === "component") {
      path = descriptor?.source?.component || descriptor?.runtime_source?.file || "";
      hint = path || "No runtime_source.file";
    } else if (kind === "adapter") {
      path = descriptor?.source?.adapter || descriptor?.authoring_source?.file || "";
      hint = path || "No authoring_source.file";
    } else if (kind === "systems.reading") {
      const withSource = systemsForComponent.find((system) => system?.source?.file);
      path = withSource?.source?.file || "";
      hint = systemsForComponent.length
        ? `${systemsForComponent.length} system(s)${path ? ` · ${path}` : " · no source.file"}`
        : "No systems read/write this component";
      button.dataset.systems = JSON.stringify(systemsForComponent.map((system) => ({
        id: system.id,
        file: system.source?.file || null,
        line: system.source?.line || null,
      })));
    }
    button.dataset.path = path || "";
    button.disabled = kind === "systems.reading" ? systemsForComponent.length === 0 : !path;
    const small = button.querySelector("small");
    if (small) small.textContent = hint;
  });
}

function findSystemsForComponent(componentId) {
  if (!componentId) return [];
  const systems = Array.isArray(state.systemsCoverage) ? state.systemsCoverage : [];
  return systems.filter((system) => {
    const reads = Array.isArray(system.reads) ? system.reads : [];
    const writes = Array.isArray(system.writes) ? system.writes : [];
    return reads.includes(componentId) || writes.includes(componentId);
  });
}

function openCoverageSource(path, systemsPayload) {
  if (Array.isArray(systemsPayload) && systemsPayload.length) {
    const withFile = systemsPayload.filter((entry) => entry.file);
    if (withFile.length > 1) {
      const listing = withFile.map((entry, index) => `${index + 1}. ${entry.id} — ${entry.file}${entry.line ? `:${entry.line}` : ""}`).join("\n");
      const choice = window.prompt(`Open system source (number):\n${listing}`, "1")?.trim();
      if (!choice) return;
      const picked = withFile[Number(choice) - 1] || withFile.find((entry) => entry.id === choice || entry.file === choice);
      if (!picked?.file) {
        showToast("No source", "Selected system has no source.file", "warning");
        return;
      }
      setMainView("code");
      post("source.read", { path: picked.file });
      return;
    }
    if (withFile.length === 1) {
      setMainView("code");
      post("source.read", { path: withFile[0].file });
      return;
    }
    showToast("Systems found", `${systemsPayload.length} system(s), none expose source.file`, "info");
    renderSystemsOutline(systemsPayload.map((entry) => entry.id));
    setMainView("code");
    return;
  }
  if (!path) {
    showToast("Source location unavailable", "host.coverage did not provide source metadata", "warning");
    return;
  }
  setMainView("code");
  post("source.read", { path });
}

function renderSystemsOutline(filterIds) {
  const explorer = document.querySelector(".code-explorer");
  if (!explorer) return;
  explorer.querySelectorAll(".code-outline-heading, .outline-row").forEach((el) => el.remove());
  const systems = Array.isArray(state.systemsCoverage) ? state.systemsCoverage : [];
  const filtered = filterIds?.length
    ? systems.filter((system) => filterIds.includes(system.id))
    : systems;
  if (!filtered.length) return;
  const heading = document.createElement("div");
  heading.className = "code-outline-heading";
  heading.textContent = filterIds?.length ? "Systems (selection)" : "Systems";
  explorer.append(heading);
  filtered.forEach((system) => {
    const row = document.createElement("button");
    row.type = "button";
    row.className = "outline-row";
    const file = system.source?.file;
    row.innerHTML = `<span class="outline-symbol outline-symbol--fn">ƒ</span><span></span>`;
    row.querySelectorAll("span")[1].textContent = system.id || "system";
    row.title = file ? `${file}${system.source?.line ? `:${system.source.line}` : ""}` : "No source.file";
    row.disabled = !file;
    row.addEventListener("click", () => {
      if (!file) return;
      post("source.read", { path: file });
    });
    explorer.append(row);
  });
}

function applySceneDocument(payload) {
  if (!payload.document || !Array.isArray(payload.document.entities)) {
    showToast("Invalid scene payload", "host.scene.document must contain document.entities[]", "warning");
    return;
  }
  state.scene.path = payload.path || state.scene.path;
  state.scene.revision = Number(payload.revision ?? state.scene.revision);
  state.scene.document = payload.document;
  state.scene.readOnly = Boolean(payload.read_only);
  state.revision = state.scene.revision;
  document.querySelector("#sceneStatusLabel").textContent = `${payload.document.entities.length} entities · revision ${state.scene.revision}`;
  // Do not steal Asset Preview while a glTF session is open — scene document
  // refresh used to force Scene and zero out the preview hole mid-upload.
  const previewingAsset = state.view === "preview" && Boolean(state.assetPreview?.path || state.assetPreview?.id);
  if (state.view !== "scene" && !previewingAsset) setMainView("scene");
  const selectedStillExists = state.selection?.kind === "entity" && payload.document.entities.some((entity) => entity.guid === state.selection.stableId);
  if (!selectedStillExists) {
    const firstGuid = payload.document.roots?.[0] || payload.document.entities[0]?.guid;
    state.selection = firstGuid ? { kind: "entity", stableId: firstGuid, label: labelForStableId(firstGuid) } : null;
  }
  if (state.selection?.kind === "entity") {
    const entity = payload.document.entities.find((candidate) => candidate.guid === state.selection.stableId);
    if (entity) state.selection.label = entity.name;
  }
  renderSceneTree();
  renderInspector();
  if (state.selection) updateSelection({ id: state.selection.stableId, kind: state.selection.kind, label: state.selection.label });
}

function applySceneHistory(payload) {
  state.scene.revision = Number(payload.revision ?? state.scene.revision);
  state.scene.dirty = Boolean(payload.dirty);
  state.scene.canUndo = Boolean(payload.can_undo);
  state.scene.canRedo = Boolean(payload.can_redo);
  state.scene.readOnly = Boolean(payload.read_only ?? state.scene.readOnly);
  document.querySelectorAll(".dirty-mark, .tab-modified").forEach((element) => { element.style.visibility = state.scene.dirty ? "visible" : "hidden"; });
  const undo = document.querySelector('[data-command="history.undo"]');
  const redo = document.querySelector('[data-command="history.redo"]');
  if (undo) undo.disabled = !state.scene.canUndo;
  if (redo) redo.disabled = !state.scene.canRedo;
  document.querySelector("#saveButton").disabled = state.scene.readOnly;
  const syncCode = document.querySelector("#syncCodeButton");
  if (syncCode) syncCode.disabled = !state.scene.document || state.scene.readOnly;
  const applyCode = document.querySelector("#applyCodeButton");
  if (applyCode) applyCode.disabled = !state.scene.document || state.scene.readOnly;
}

function collectComponentCoverage(payload) {
  const direct = [payload.components, payload.manifest?.components];
  const surfaces = payload.surfaces || payload.manifest?.surfaces || [];
  surfaces.forEach((surface) => direct.push(surface.components, surface.component_descriptors));
  return direct.filter(Array.isArray).flat().filter((descriptor) => descriptor && typeof descriptor.id === "string");
}

function collectUnavailableCapabilities(payload) {
  if (Array.isArray(payload.unavailable)) return payload.unavailable;
  const capabilities = payload.manifest?.capabilities;
  if (!Array.isArray(capabilities)) return [];
  return capabilities.flatMap((capability) => {
    const surfaces = Array.isArray(capability.surfaces) ? capability.surfaces : [];
    return surfaces
      .filter((surface) => String(surface?.status || surface?.state || surface).toLowerCase() === "unavailable")
      .map((surface) => ({
        label: capability.label || capability.name || capability.id,
        reason: surface.reason || capability.reason,
        milestone: surface.milestone || capability.milestone,
      }));
  });
}

function renderUnavailableCapabilities(payload) {
  const container = document.querySelector("#unavailableCapabilities");
  const unavailable = collectUnavailableCapabilities(payload);
  container.replaceChildren();
  container.hidden = !unavailable.length;
  if (!unavailable.length) return;

  const details = document.createElement("details");
  const summary = document.createElement("summary");
  summary.textContent = `${unavailable.length} engine capabilities not authorable yet (expected)`;
  const tip = document.createElement("p");
  tip.className = "unavailable-tip";
  tip.textContent = "Authoring that works now: scenes, hierarchy, Transform gizmo, Model3d + lights, Play pin, Asset Preview overlays/selection/animation/material factors+texture remap, Apply Play Changes, Sync/Apply Code scene↔.rs projection, coverage CI, rust-analyzer diagnostics, source/system navigation.";
  const list = document.createElement("ul");
  unavailable.forEach((entry) => {
    const item = document.createElement("li");
    if (typeof entry === "string") item.textContent = entry;
    else {
      const label = entry.title || entry.label || entry.name || entry.id || entry.capability || "Unnamed capability";
      const detail = [entry.reason, entry.milestone ? `milestone: ${entry.milestone}` : null].filter(Boolean).join(" · ");
      item.textContent = detail ? `${label} — ${detail}` : label;
      item.title = item.textContent;
    }
    list.append(item);
  });
  details.append(summary, tip, list);
  container.append(details);
}

function configureCoverage(payload) {
  state.componentCoverage = new Map(collectComponentCoverage(payload).map((descriptor) => [descriptor.id, descriptor]));
  state.systemsCoverage = Array.isArray(payload.systems)
    ? payload.systems
    : Array.isArray(payload.manifest?.systems)
      ? payload.manifest.systems
      : [];
  configureAvailableComponents(payload.available_components ?? payload.availableComponents ?? payload.manifest?.available_components);
  renderUnavailableCapabilities(payload);
  const project = typeof payload.project === "object" && payload.project
    ? payload.project
    : { ready: false };
  const hasProject = payload.hasProject === true || project.ready === true;
  configureProject(project, hasProject);
  if (hosted && payload.preview) {
    const labels = document.querySelectorAll(".viewport-stats span");
    labels[0].textContent = "Native WGPU";
    labels[1].textContent = payload.preview.foundationViewport ? "Foundation viewport" : "Viewport unavailable";
    labels[2].textContent = `glTF ${payload.preview.gltf || "not reported"}`;
  }
  if (state.view === "code") renderSystemsOutline();
  renderInspector();
}

function configureProject(project, hasProject = Boolean(project?.ready)) {
  const ready = hasProject === true || Boolean(project?.ready);
  state.projectConfig = {
    name: ready ? (project.name || null) : null,
    package: ready ? (project.package || null) : null,
    executable: ready ? (project.play?.executable || project.executable || null) : null,
    args: ready && Array.isArray(project.play?.args) ? project.play.args : ready && Array.isArray(project.args) ? project.args : [],
    ready,
    root: ready ? (project.root || null) : null,
    scenes: ready && Array.isArray(project.scenes) ? project.scenes : [],
  };
  document.querySelector("#playButton").disabled = !ready;
  document.querySelector("#buildButton").disabled = !state.projectConfig.package;
  const cookButton = document.querySelector("#cookButton");
  if (cookButton) cookButton.disabled = !ready;
  const exportYpackButton = document.querySelector("#exportYpackButton");
  if (exportYpackButton) exportYpackButton.disabled = !ready;
  const importYpackButton = document.querySelector("#importYpackButton");
  if (importYpackButton) importYpackButton.disabled = !ready;
  document.querySelector("#runCheckButton").disabled = !state.projectConfig.package;
  document.querySelector("#runCheckButton small").textContent = state.projectConfig.package ? `package: ${state.projectConfig.package}` : "Cargo package not configured";
  document.querySelector("#projectNameLabel").textContent = state.projectConfig.name || "No project";
  document.querySelector(".project-chip").hidden = false;
  document.querySelector(".branch-name").textContent = state.projectConfig.root ? state.projectConfig.root.split(/[/\\]/).filter(Boolean).at(-1) || "—" : "—";
  if (!hosted && state.projectConfig.name) {
    document.querySelector(".code-workspace-name").textContent = state.projectConfig.name.toUpperCase();
    document.querySelector(".code-breadcrumb span").textContent = state.projectConfig.name;
  }
  renderProjectScenes(state.projectConfig.scenes);
  // Until a real project is open, the launcher is mandatory — no dismiss path.
  setLauncherVisible(hosted && !state.projectConfig.ready);
}

function renderProjectScenes(scenes) {
  const row = document.querySelector("#projectSceneList");
  if (!row) return;
  row.replaceChildren();
  if (!scenes?.length) {
    row.hidden = true;
    return;
  }
  row.hidden = false;
  scenes.forEach((scene) => {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "scene-chip";
    button.textContent = scene.name || scene.path;
    button.title = scene.path;
    button.addEventListener("click", () => post("scene.open", { path: scene.path }));
    row.append(button);
  });
}

function setLauncherVisible(visible, options = {}) {
  const launcher = document.querySelector("#projectLauncher");
  if (!launcher) return;
  launcher.classList.toggle("is-hidden", !visible);
  document.body.classList.toggle("launcher-open", Boolean(visible));
  if (visible && !options.preserveStatus) {
    document.querySelector("#launcherStatus").textContent = "";
    document.querySelector("#launcherStatus").className = "launcher-status";
    window.requestAnimationFrame(() => document.querySelector("#launcherProjectName")?.focus());
  }
  // After dismissing the launcher the hole must publish bounds or the native
  // surface never clears (Windows HWND default shows as a white rectangle).
  if (!visible) {
    window.requestAnimationFrame(sendViewportBounds);
  }
}

function setLauncherStatus(message, tone = "info") {
  const status = document.querySelector("#launcherStatus");
  if (!status) return;
  status.textContent = message || "";
  status.className = tone === "error" || tone === "warning"
    ? "launcher-status is-error"
    : tone === "success"
      ? "launcher-status is-ok"
      : "launcher-status";
}

function clearPendingProjectAction() {
  if (state.pendingProjectTimer !== null) {
    window.clearTimeout(state.pendingProjectTimer);
    state.pendingProjectTimer = null;
  }
  if (state.pendingProjectAction) {
    console.info("[yuyib] pending project action cleared:", state.pendingProjectAction);
  }
  state.pendingProjectAction = null;
}

function beginPendingProjectAction(action, detail, timeoutMs = 120000) {
  clearPendingProjectAction();
  state.pendingProjectAction = action;
  console.info("[yuyib] pending project action begin:", action, detail, `timeout=${timeoutMs}ms`);
  setLauncherStatus(detail || `${action}…`);
  state.pendingProjectTimer = window.setTimeout(() => {
    if (!state.pendingProjectAction) return;
    const stuck = state.pendingProjectAction;
    clearPendingProjectAction();
    console.error("[yuyib] pending project action timed out:", stuck);
    setLauncherStatus(
      `${stuck} timed out — host did not answer. Check the native console for bridge errors, then retry.`,
      "error",
    );
    showToast("Project action timed out", "No host.project / project.* diagnostic arrived", "warning");
  }, timeoutMs);
}

function renderAssetIndex(items) {
  state.assets = Array.isArray(items) ? items : [];
  const assetGrid = document.querySelector("#assetGrid");
  assetGrid.replaceChildren();
  if (!state.assets.length) {
    const empty = document.createElement("div");
    empty.className = "data-empty-state data-empty-state--wide";
    empty.textContent = state.projectConfig.ready
      ? "No recognized assets under assets/ yet"
      : "Create or open a project to index assets";
    assetGrid.append(empty);
    if (state.selection?.kind === "entity") renderInspector();
    return;
  }
  state.assets.forEach((asset) => {
    const card = document.createElement("button");
    card.className = "asset-card";
    card.dataset.kind = "asset";
    card.dataset.id = asset.id || asset.path;
    const thumb = document.createElement("span");
    thumb.className = `asset-thumb asset-thumb--${asset.kind || "asset"}`;
    const name = document.createElement("span");
    name.className = "asset-name";
    name.textContent = asset.name || asset.path;
    const meta = document.createElement("span");
    meta.className = "asset-meta";
    meta.textContent = `${(asset.kind || "file").toUpperCase()} · ${asset.tracking || "n/a"} · ${asset.extension || ""}`;
    card.append(thumb, name, meta);
    card.addEventListener("click", () => requestAssetOpen(card.dataset.id));
    assetGrid.append(card);
  });
  document.querySelector(".show-all-button").hidden = false;
  document.querySelector(".show-all-button").textContent = `Show all ${state.assets.length} assets`;
  if (state.selection?.kind === "entity") renderInspector();
}

function sendSceneCommand(command) {
  if (!state.scene.document) {
    showToast("No scene open", "Open or create a .yscene document first", "warning");
    return;
  }
  if (state.scene.readOnly) {
    showToast("Scene is read-only", "The host retained opaque or incompatible data and rejected mutation", "warning");
    return;
  }
  post("scene.command", {
    base_revision: state.scene.revision,
    transaction_id: `scene-tx-${state.transactionId++}`,
    command,
  });
}

function setViewportTool(tool) {
  if (!["move", "rotate", "scale", "select"].includes(tool)) return;
  document.querySelectorAll("[data-tool]").forEach((item) => item.classList.toggle("is-active", item.dataset.tool === tool));
  state.activeTool = tool;
  post("viewport.tool", { tool });
  appendOutput("viewport", `Active tool: ${state.activeTool}`);
}

function removeComponent(component) {
  const entity = selectedSceneEntity();
  if (!entity) {
    showToast("Select an entity", "Choose an entity in the hierarchy before removing a component", "warning");
    return;
  }
  sendSceneCommand({
    type: "component.remove",
    entity_guid: entity.guid,
    component_id: component.id,
  });
}

function deleteSelectedEntity() {
  const entity = selectedSceneEntity();
  if (!entity) {
    showToast("Select an entity", "Choose an entity in the hierarchy before deleting", "warning");
    return;
  }
  sendSceneCommand({ type: "entity.delete", entity_guid: entity.guid });
}

window.addEventListener("yuyib:event", ({ detail }) => {
  if (!detail || detail.version !== 1 || typeof detail.event !== "string") return;
  const payload = detail.payload || {};
  if (
    detail.event === "host.project"
    || detail.event === "host.diagnostics"
    || detail.event === "host.coverage"
  ) {
    console.info("[yuyib] event ←", detail.event, payload);
  }

  switch (detail.event) {
    case "host.coverage":
      state.revision = payload.revision ?? state.revision;
      configureCoverage(payload);
      {
        const total = payload.total ?? payload.manifest?.capabilities?.length ?? "?";
        const available = payload.covered ?? "?";
        const unavailable = collectUnavailableCapabilities(payload).length;
        const summary = `${available} / ${total} capabilities${unavailable ? ` · ${unavailable} unavailable` : ""}`;
        document.querySelector(".coverage-chip").textContent = summary;
        document.querySelector(".coverage-chip").title = unavailable ? `${summary} · unavailable listed below` : summary;
        appendOutput("coverage", `${summary} · ${payload.status || "ready"}`);
      }
      break;
    case "host.available_components":
    case "available_components":
      configureAvailableComponents(payload.components || payload.available_components || payload);
      renderInspector();
      break;
    case "host.project": {
      const wasReady = state.projectConfig.ready;
      const ready = payload.ready === true || payload.project?.ready === true;
      clearPendingProjectAction();
      configureProject(payload.project || payload, ready);
      window.requestAnimationFrame(sendViewportBounds);
      if (ready) {
        const root = payload.root || payload.project?.root || state.projectConfig.root || "project";
        appendOutput("project", `Opened ${root}`);
        setLauncherStatus(`Opened ${root}`, "success");
        if (!wasReady) showToast("Project opened", String(root), "success");
        if (hosted) post("source.list", {});
      } else {
        appendOutput("project", "Waiting for project selection");
      }
      break;
    }
    case "host.source.tree":
      renderSourceTree(payload);
      break;
    case "host.assets":
      renderAssetIndex(payload.items || []);
      appendOutput("assets", `${(payload.items || []).length} assets · ${payload.status || "ready"}`);
      if (Array.isArray(payload.diagnostics) && payload.diagnostics.length) updateDiagnostics(payload.diagnostics);
      break;
    case "host.asset": {
      appendOutput("assets", `Opened ${payload.path || payload.id || "asset"}`);
      updateAssetPreviewPanel(payload);
      if (
        payload.kind === "model"
        || payload.kind === "asset"
        || payload.preview?.status === "available"
        || /\.(glb|gltf|yasset)$/i.test(payload.path || "")
      ) {
        if (state.view !== "preview") setMainView("preview");
        else sendViewportBounds();
        showToast("Asset Preview", payload.path || payload.name || "asset", "info");
      }
      break;
    }
    case "host.selection":
      updateSelection(payload);
      break;
    case "host.viewport.orbit":
      updateViewportAxis(payload);
      break;
    case "host.diagnostics":
      updateDiagnostics(payload.items || payload.diagnostics || []);
      break;
    case "host.lsp.status":
      handleLspStatus(payload);
      break;
    case "host.lsp.diagnostics":
      applyLspDiagnostics(payload);
      break;
    case "host.lsp.completion":
      resolveLspPending(payload?.request_id, payload);
      break;
    case "host.lsp.hover":
      resolveLspPending(payload?.request_id, payload);
      break;
    case "host.lsp.signatureHelp":
      resolveLspPending(payload?.request_id, payload);
      break;
    case "host.lsp.definition":
      resolveLspPending(payload?.request_id, payload);
      break;
    case "host.lsp.references":
      resolveLspPending(payload?.request_id, payload);
      break;
    case "host.lsp.rename":
      resolveLspPending(payload?.request_id, payload);
      break;
    case "host.lsp.codeAction":
      resolveLspPending(payload?.request_id, payload);
      break;
    case "host.lsp.executeCommand":
      resolveLspPending(payload?.request_id, payload);
      break;
    case "host.source":
      if (payload.saved) {
        state.sourceRevision = payload.revision ?? state.sourceRevision;
        showToast("Source saved", `${payload.path} · revision ${state.sourceRevision}`, "success");
      } else {
        openDocument(payload);
      }
      break;
    case "host.sourceConflict":
      showRevisionConflictDialog("source", payload);
      appendOutput("source", payload.message || "External source revision conflict");
      break;
    case "host.process":
      handleHostProcess(payload);
      break;
    case "host.scene.document":
    case "scene.document":
      applySceneDocument(payload);
      break;
    case "host.scene.conflict":
    case "scene.conflict":
      showRevisionConflictDialog("scene", payload);
      appendOutput("scene", payload.message || "Host rejected a stale scene command");
      break;
    case "host.scene.history":
    case "scene.history":
      applySceneHistory(payload);
      break;
    case "host.scene.interaction.signal": {
      const name = payload?.name || "?";
      const phase = payload?.payload?.phase;
      const quest = payload?.quest_progress;
      const summary = quest
        ? `quest ${quest.event} ×${quest.amount}`
        : phase
          ? `${name} phase=${phase}`
          : name;
      appendOutput("interaction", `signal ${summary}`);
      showToast("Interaction signal", summary, "info");
      break;
    }
    default:
      appendOutput("bridge", `Ignored unsupported event ${detail.event}`);
  }
});

function maybeToastReimportCascade(cascade) {
  if (!cascade || typeof cascade !== "object") return;
  const refreshed = Array.isArray(cascade.refreshed) ? cascade.refreshed.length : 0;
  const dependents = Array.isArray(cascade.dependents) ? cascade.dependents.length : refreshed;
  if (!dependents) return;
  showToast(
    "Dependents refreshed",
    `${refreshed || dependents} asset(s) invalidated after reimport of ${cascade.root || "root"}`,
    "info",
  );
  appendOutput("assets", `reimport cascade ${cascade.root || "?"} → ${dependents} dependent(s)`);
}

function handleHostProcess(payload) {
  if (payload.kind === "command") {
    state.revision = payload.revision ?? state.revision;
    appendOutput("command", `${payload.command_id} ${payload.status} @ revision ${state.revision}`);
    if (payload.command_id === "scene.save") {
      document.querySelectorAll(".dirty-mark, .tab-modified").forEach((element) => { element.style.visibility = "hidden"; });
      showToast("Scene saved", `${state.scene.path || "scene"} · revision committed`, "success");
    }
    return;
  }
  if (payload.kind === "play") {
    if (payload.status === "building") {
      showToast("Play · building", "Compiling game binary, then launching…", "info");
      return;
    }
    setPlayMode(payload.status === "success" ? "stopped" : payload.status);
    const pin = payload.pinned_scene;
    const pinLabel = pin?.path
      ? `${pin.path}${pin.revision != null ? ` @ hist ${pin.revision}` : ""}`
      : "";
    if (payload.status === "playing") {
      appendOutput("play", `playing ${pinLabel || "(no pin)"} · apply_play_changes=false`);
      return;
    }
    const code = payload.code == null ? "—" : String(payload.code);
    const reason = payload.reason || payload.status;
    appendOutput("play", `${payload.status} code=${code} reason=${reason} ${pinLabel}`.trim());
    if (payload.status === "stopped" || payload.status === "timeout" || payload.status === "error") {
      const ok = payload.success === true;
      const applyReady = ok && payload.apply_play_changes === true;
      const applyCount = Number(payload.apply_change_count || 0);
      const applyButton = document.querySelector("#applyPlayButton");
      if (applyButton) {
        applyButton.hidden = !applyReady;
        applyButton.textContent = applyReady
          ? `Apply Play (${applyCount})`
          : "Apply Play";
      }
      showToast(
        ok ? "Play stopped" : "Play ended",
        applyReady
          ? `${pinLabel || "scene"} · ${applyCount} transform change(s) ready to apply`
          : (pinLabel || reason || ""),
        ok ? "success" : "warning",
      );
      return;
    }
    if (payload.status === "applied") {
      const applyButton = document.querySelector("#applyPlayButton");
      if (applyButton) applyButton.hidden = true;
      showToast(
        "Play changes applied",
        `${payload.applied_entities || 0} entit(y/ies) · undoable`,
        "success",
      );
      return;
    }
    return;
  }
  if (payload.kind === "assets" && payload.status === "migrate_scene_model_refs") {
    const report = payload.report || {};
    const rewritten = report.refs_rewritten || 0;
    const changed = report.scenes_changed || 0;
    const untracked = report.refs_skipped_untracked || 0;
    const summary = `${report.dry_run ? "Dry run" : "Applied"}: ${changed} scene(s), ${rewritten} ref(s) → GUID` +
      (untracked ? `, ${untracked} untracked left as path` : "");
    appendOutput("assets", summary);
    if (report.dry_run) {
      if (rewritten === 0) {
        showToast(
          "Nothing to migrate",
          "G rewrites scene Model3d paths→GUID for tracked glTF only (Assets · T). Scene entity selection is ignored.",
          "info",
        );
        return;
      }
      const ok = window.confirm(
        `${summary}\n\nWrite asset://GUID into ${changed} scene file(s)?\n(Save the open scene first if it is dirty — dirty scenes are skipped.)`,
      );
      if (ok) post("assets.migrate_scene_model_refs", { dry_run: false });
      return;
    }
    showToast(
      rewritten ? "Model refs migrated" : "Migration finished",
      summary,
      rewritten ? "success" : "info",
    );
    return;
  }
  if (payload.kind === "preview") {
    maybeToastReimportCascade(payload.cascade);
    if (Array.isArray(payload.meshes)) {
      state.previewMeshes = payload.meshes;
      state.previewSelectedMesh = payload.selected_mesh ?? null;
      if (state.selection?.kind === "asset" || state.view === "preview") {
        renderInspector();
      }
    }
    if (Array.isArray(payload.materials)) {
      state.previewMaterials = payload.materials;
      state.previewSelectedMaterial = payload.selected_material ?? null;
      if (state.selection?.kind === "asset" || state.view === "preview") {
        renderInspector();
      }
    }
    if (Array.isArray(payload.animations)) {
      state.previewAnimations = payload.animations;
      state.previewSelectedAnimation = payload.selected_animation ?? null;
      if (state.selection?.kind === "asset" || state.view === "preview") {
        renderInspector();
      }
    }
    if (Array.isArray(payload.textures)) {
      state.previewTextures = payload.textures;
      if (state.selection?.kind === "asset" || state.view === "preview") {
        renderInspector();
      }
    }
    if (payload.status === "selection") {
      appendOutput(
        "preview",
        `selection mesh=${payload.selected_mesh == null ? "all" : payload.selected_mesh} material=${payload.selected_material == null ? "all" : payload.selected_material} animation=${payload.selected_animation == null ? "none" : payload.selected_animation} textures=${(payload.textures || []).length}`,
      );
      return;
    }
    if (payload.status === "progress") {
      const assetLoading = document.querySelector("#assetPreviewLoading");
      if (assetLoading) {
        assetLoading.classList.remove("is-hidden");
        const strong = assetLoading.querySelector("strong");
        if (strong) strong.textContent = `${Math.round((payload.completed || 0) * 100)}%`;
      }
      if (payload.path) {
        state.previewLoadingPath = String(payload.path).replace(/\\/g, "/");
      }
      if (payload.stage === "already_loading") {
        appendOutput("preview", `already loading ${payload.path || ""}`);
        return;
      }
      if (payload.stage === "import") {
        showToast("Loading model", payload.path || "glTF import…", "info");
      }
      appendOutput("preview", `${payload.stage} ${Math.round((payload.completed || 0) * 100)}%${payload.cook_hit ? " · cook hit" : ""}`);
    } else if (payload.status === "scene_model_ready") {
      document.querySelector("#assetPreviewLoading")?.classList.add("is-hidden");
      state.previewLoadingPath = null;
      showToast("Model ready", payload.path || "glTF hierarchy spawned", "success");
      appendOutput("preview", `Scene model ready ${payload.path || ""}`);
    } else if (payload.status === "ready") {
      document.querySelector("#assetPreviewLoading")?.classList.add("is-hidden");
      state.previewLoadingPath = null;
      const primitives = Number.isFinite(payload.primitive_count) ? payload.primitive_count : 0;
      const gpuMb = Number.isFinite(payload.gpu_bytes)
        ? Math.round(payload.gpu_bytes / 1048576)
        : 0;
      updateAssetPreviewStats({
        path: payload.path,
        primitives,
        gpuMb,
      });
      showToast("Asset Preview ready", `${primitives} primitives · ${gpuMb} MB GPU${payload.cook_hit ? " · cook hit" : ""}`, "success");
      appendOutput("preview", `Asset ready in Asset Preview (${payload.cache || "production"}${payload.cook_hit ? " · cook hit" : ""})`);
      if (state.view !== "preview") setMainView("preview");
      window.requestAnimationFrame(sendViewportBounds);
    } else if (payload.status === "failed") {
      document.querySelector("#assetPreviewLoading")?.classList.add("is-hidden");
      state.previewLoadingPath = null;
      showToast("Preview failed", payload.message || payload.stage || "import error", "warning");
    }
    return;
  }
  if (payload.kind === "asset" && payload.status === "reimport_cascade") {
    maybeToastReimportCascade(payload.cascade);
    return;
  }
  if (payload.kind === "cargo") {
    setCargoStatus(`cargo check · ${payload.status}`, payload.completed);
    appendOutput("cargo", `${payload.package}: ${payload.status} ${Math.round((payload.completed || 0) * 100)}%`);
    if (payload.status === "success") {
      const summary = [payload.package, payload.elapsed_ms !== undefined ? `${payload.elapsed_ms} ms` : null, payload.warnings !== undefined ? `${payload.warnings} warnings` : null].filter(Boolean).join(" · ");
      showToast("Scoped Cargo check passed", summary || "Host reported success", "success");
    }
    return;
  }
  if (payload.kind === "cook") {
    const total = Number(payload.total || 0);
    const index = Number(payload.index || 0);
    if (payload.status === "started") {
      setCargoStatus(`cook · started (${total})`, 0.02);
      appendOutput("cook", `started · ${total} glTF source(s)`);
      showToast("Cook assets", total ? `Cooking ${total} glTF source(s)…` : "Starting…", "info");
      return;
    }
    if (payload.status === "progress") {
      const label = payload.cook_hit ? "hit" : (payload.error ? "error" : "miss");
      setCargoStatus(`cook · ${index}/${total} ${label}`, payload.completed);
      appendOutput("cook", `${payload.path || "?"} · ${label}${payload.error ? `: ${payload.error}` : ""}`);
      return;
    }
    if (payload.status === "finished") {
      const hits = Number(payload.hits || 0);
      const misses = Number(payload.misses || 0);
      const errors = Number(payload.errors || 0);
      setCargoStatus(`cook · finished`, 1);
      const summary = payload.message
        || `${total} asset(s) · ${hits} hit · ${misses} miss · ${errors} error`;
      appendOutput("cook", `finished · ${summary}`);
      showToast(
        errors ? "Cook finished with errors" : "Cook finished",
        summary,
        errors ? "warning" : "success",
      );
    }
    return;
  }
  if (payload.kind === "ypack") {
    const op = payload.op || "export";
    if (payload.status === "started") {
      setCargoStatus(`ypack ${op} · started`, 0.05);
      appendOutput("ypack", `${op} started · ${payload.path || "?"}`);
      showToast(op === "import" ? "Import ypack" : "Export ypack", payload.path || "Working…", "info");
      return;
    }
    if (payload.status === "finished") {
      setCargoStatus(`ypack ${op} · finished`, 1);
      const summary = op === "import"
        ? `${payload.path || "pack"} · ${payload.written || 0}/${payload.entries || 0} written`
        : `${payload.path || "pack"} · ${payload.entries || 0} entr(y/ies)`;
      appendOutput("ypack", `${op} finished · ${summary}`);
      showToast(op === "import" ? "Ypack imported" : "Ypack exported", summary, "success");
      return;
    }
    if (payload.status === "error") {
      setCargoStatus(`ypack ${op} · error`, 1);
      appendOutput("ypack", `${op} error · ${payload.error || payload.path || "?"}`);
      showToast(op === "import" ? "Ypack import failed" : "Ypack export failed", payload.error || "error", "warning");
    }
    return;
  }
  if (payload.kind === "protocol" && payload.status === "error") {
    showToast("Bridge protocol error", payload.message || payload.code, "warning");
  }
}

function formatDiagnosticText(item) {
  const severity = item.severity || "info";
  const source = item.source || "host";
  const message = item.message || "Host diagnostic";
  const detail = item.detail || item.code || "";
  const time = item.time || "";
  return [severity, source, message, detail, time].filter(Boolean).join(" · ");
}

function updateDiagnostics(items) {
  if (!items.length) return;
  const panel = document.querySelector("#diagnosticsPanel");
  // Append host batches so earlier errors remain copyable.
  const empty = panel.querySelector(".data-empty-state");
  if (empty) empty.remove();
  const existing = Array.from(panel.querySelectorAll(".diagnostic-row")).map((row) => row.dataset.copyText || "");
  items.forEach((item) => {
    const row = document.createElement("button");
    const severity = item.severity === "error" ? "warning" : item.severity === "info" ? "info" : "warning";
    const copyText = formatDiagnosticText(item);
    row.type = "button";
    row.className = `diagnostic-row diagnostic-row--${severity}`;
    row.dataset.copyText = copyText;
    row.title = "Click to copy";
    row.innerHTML = `<span class="diagnostic-icon"></span><span class="diagnostic-main"><strong></strong><small></small></span><span class="diagnostic-source"></span><span class="diagnostic-time"></span>`;
    row.querySelector(".diagnostic-icon").textContent = severity === "info" ? "i" : "!";
    row.querySelector("strong").textContent = item.message || "Host diagnostic";
    row.querySelector("small").textContent = item.detail || item.code || "";
    row.querySelector(".diagnostic-source").textContent = item.source || "host";
    row.querySelector(".diagnostic-time").textContent = item.time || "now";
    row.addEventListener("click", async () => {
      try {
        await navigator.clipboard.writeText(copyText);
        showToast("Copied", copyText.slice(0, 120), "info");
      } catch {
        showToast("Copy failed", "Clipboard is unavailable", "warning");
      }
    });
    panel.append(row);
    existing.push(copyText);

    const msg = String(item.message || "");
    if (item.source === "play" && /yuyib-play:.*(signal|use accepted|use miss|materialized|interactable)/i.test(msg)) {
      appendOutput("play", msg.replace(/^yuyib-play:\s*/, ""));
      if (/signal|use accepted/i.test(msg)) {
        showToast("Play", msg.replace(/^yuyib-play:\s*/, "").slice(0, 140), "info");
      }
    }
  });
  state.diagnosticsCopyBuffer = existing.join("\n");
  const total = panel.querySelectorAll(".diagnostic-row").length;
  document.querySelectorAll(".count-badge, .rail-badge").forEach((badge) => { badge.textContent = String(total); });
  const launcher = document.querySelector("#projectLauncher");
  const latest = items[items.length - 1];
  const source = latest?.source || "";
  if (source === "project" || source.startsWith("project.")) {
    clearPendingProjectAction();
    const tone = latest.severity === "error" || latest.severity === "warning" ? "error" : "info";
    if (launcher && !launcher.classList.contains("is-hidden")) {
      setLauncherStatus(latest.message || "", tone);
    }
    if (latest.severity === "error" || latest.severity === "warning") {
      showToast(source, latest.message || "Project action failed", "warning");
    }
  }
}

function setCargoStatus(text, completed = 0) {
  const status = document.querySelector("#rustAnalyzerState");
  status.lastChild.textContent = text;
  status.querySelector("i").style.background = completed >= 1 ? "var(--green)" : "var(--orange)";
}

function requestSelection(kind, stableId) {
  post("selection.set", { id: stableId });
}

function requestAssetOpen(assetId) {
  if (hosted) {
    // Switch Preview + wait for non-zero Preview-stage bounds BEFORE asset.open
    // so the native hole is not painted with stale Scene rect / cleared by 0×0.
    const id = String(assetId || "");
    const fromIndex = (state.assets || []).find((item) =>
      item.id === id || item.path === id || `asset://${item.path}` === id
    );
    const path = fromIndex?.path || id;
    const kind = fromIndex?.kind || "";
    const wantsPreview = kind === "model" || kind === "asset"
      || /\.(glb|gltf|yasset)$/i.test(path)
      || /^asset:\/\//i.test(id);
    const normalizedPath = String(path).replace(/\\/g, "/");
    // Same glTF already importing — do not restart the production import.
    if (
      wantsPreview
      && state.previewLoadingPath
      && (normalizedPath === state.previewLoadingPath
        || normalizedPath.endsWith(state.previewLoadingPath)
        || state.previewLoadingPath.endsWith(normalizedPath)
        || id === state.previewLoadingPath
        || `asset://${normalizedPath}` === state.previewLoadingPath)
    ) {
      appendOutput("preview", `ignored reopen while loading ${state.previewLoadingPath}`);
      return;
    }
    const open = () => {
      if (wantsPreview) state.previewLoadingPath = normalizedPath;
      post("asset.open", { id: assetId });
    };
    if (wantsPreview) {
      if (state.view !== "preview") setMainView("preview");
      else {
        post("workspace.mode", { mode: "preview" });
        sendViewportBounds();
      }
      waitForPreviewBounds(open);
      return;
    }
    open();
    return;
  }
  requestSelection("asset", assetId);
}

function waitForPreviewBounds(then, attempts = 0) {
  const stage = document.querySelector(".preview-stage");
  const rect = stage?.getBoundingClientRect();
  if (rect && rect.width > 2 && rect.height > 2) {
    sendViewportBounds();
    then();
    return;
  }
  if (attempts >= 20) {
    sendViewportBounds();
    then();
    return;
  }
  window.requestAnimationFrame(() => waitForPreviewBounds(then, attempts + 1));
}

function setMainView(view) {
  state.view = view;
  document.querySelectorAll(".document-tab").forEach((tab) => tab.classList.toggle("is-active", tab.dataset.view === view));
  document.querySelectorAll(".main-view").forEach((panel) => {
    const active = panel.id === `${view}View`;
    panel.classList.toggle("is-visible", active);
    panel.hidden = !active;
  });
  if (view === "code") {
    ensureMonaco();
    window.requestAnimationFrame(() => state.monacoEditor?.layout());
    if (hosted) post("source.list", {});
  }
  if (view === "scene") window.requestAnimationFrame(drawScene);
  // Preview/Code must hide the sibling WGPU HWND or it covers WebView dialogs/tabs.
  post("workspace.mode", { mode: view === "code" ? "code" : view === "preview" ? "preview" : "scene" });
  // Sync bounds immediately (getBoundingClientRect forces layout) then again on rAF.
  sendViewportBounds();
  window.requestAnimationFrame(sendViewportBounds);
}

function setNativeViewportVisible(visible) {
  if (!hosted) return;
  if (!visible) {
    post("viewport.bounds", { x: 0, y: 0, width: 0, height: 0 });
    return;
  }
  sendViewportBounds();
}

function sendViewportBounds() {
  if (!hosted) return;
  if (document.body.classList.contains("launcher-open")) return;
  const dialogOpen = !document.querySelector("#addComponentDialog")?.hidden
    || !document.querySelector("#revisionConflictDialog")?.hidden;
  if (dialogOpen) {
    post("viewport.bounds", { x: 0, y: 0, width: 0, height: 0 });
    return;
  }
  let target = null;
  if (state.view === "scene") {
    target = document.querySelector(".viewport-canvas-wrap");
  } else if (state.view === "preview") {
    target = document.querySelector(".preview-stage");
  }
  const rect = target?.getBoundingClientRect();
  const visible = Boolean(rect && rect.width > 0 && rect.height > 0);
  post("viewport.bounds", {
    x: visible ? rect.left : 0,
    y: visible ? rect.top : 0,
    width: visible ? rect.width : 0,
    height: visible ? rect.height : 0,
  });
}

function relayViewportPointer(event) {
  if (!hosted) return;
  const viewport = state.view === "preview"
    ? document.querySelector(".preview-stage")
    : document.querySelector(".viewport-canvas-wrap");
  if (!viewport) return;
  const rect = viewport.getBoundingClientRect();
  try {
    post("viewport.pointer", {
      type: event.type,
      x: event.clientX - rect.left,
      y: event.clientY - rect.top,
      button: event.button ?? 0,
      buttons: event.buttons ?? 0,
      modifiers: { alt: event.altKey, ctrl: event.ctrlKey, meta: event.metaKey, shift: event.shiftKey },
      delta_x: event.deltaX ?? 0,
      delta_y: event.deltaY ?? 0,
      pointer_id: event.pointerId ?? null,
    });
  } catch {
    // Older hosts may not expose viewport.pointer yet.
  }
}

function bindViewportPointerRelay(relay) {
  if (!relay) return;
  let pendingMove = null;
  let moveFrame = null;
  const flushMove = () => {
    moveFrame = null;
    if (pendingMove) relayViewportPointer(pendingMove);
    pendingMove = null;
  };
  relay.addEventListener("pointermove", (event) => {
    pendingMove = event;
    if (moveFrame === null) moveFrame = window.requestAnimationFrame(flushMove);
  });
  ["pointerenter", "pointerleave", "pointerdown", "pointerup"].forEach((type) => {
    relay.addEventListener(type, (event) => {
      if (type === "pointerdown") relay.setPointerCapture?.(event.pointerId);
      if (type === "pointerleave" || type === "pointerup") {
        if (relay.hasPointerCapture?.(event.pointerId)) relay.releasePointerCapture(event.pointerId);
      }
      relayViewportPointer(event);
    });
  });
  relay.addEventListener("wheel", relayViewportPointer, { passive: true });
}

function initializeViewportPointerRelay() {
  bindViewportPointerRelay(document.querySelector("#viewportInputRelay"));
  bindViewportPointerRelay(document.querySelector("#previewInputRelay"));
}

function registerRustLanguage() {
  if (!monaco.languages.getLanguages().some(({ id }) => id === "rust")) monaco.languages.register({ id: "rust", extensions: [".rs"], aliases: ["Rust", "rust"] });
  monaco.languages.setLanguageConfiguration("rust", {
    comments: { lineComment: "//", blockComment: ["/*", "*/"] },
    brackets: [["{", "}"], ["[", "]"], ["(", ")"]],
    autoClosingPairs: [
      { open: "{", close: "}" }, { open: "[", close: "]" }, { open: "(", close: ")" },
      { open: "\"", close: "\"", notIn: ["string"] }, { open: "'", close: "'", notIn: ["string", "comment"] },
    ],
    surroundingPairs: [{ open: "{", close: "}" }, { open: "[", close: "]" }, { open: "(", close: ")" }, { open: "\"", close: "\"" }, { open: "'", close: "'" }],
    indentationRules: {
      increaseIndentPattern: /^.*\{[^}"']*$/,
      decreaseIndentPattern: /^\s*\}/,
    },
  });
  monaco.languages.setMonarchTokensProvider("rust", {
    defaultToken: "",
    tokenPostfix: ".rust",
    keywords: ["as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where", "while"],
    typeKeywords: ["bool", "char", "f32", "f64", "i8", "i16", "i32", "i64", "i128", "isize", "str", "u8", "u16", "u32", "u64", "u128", "usize"],
    tokenizer: {
      root: [
        [/\b[A-Z][\w]*\b/, "type.identifier"],
        [/[a-zA-Z_]\w*!/, "macro"],
        [/[a-zA-Z_]\w*/, { cases: { "@keywords": "keyword", "@typeKeywords": "keyword.type", "@default": "identifier" } }],
        [/#\!?\[/, "annotation"],
        [/\/\//, "comment", "@lineComment"],
        [/\/\*/, "comment", "@blockComment"],
        [/r#*\"/, "string", "@rawString"],
        [/\"/, "string", "@string"],
        [/'([^'\\]|\\.)'/, "string"],
        [/0[xX][0-9a-fA-F_]+|0[bB][01_]+|0[oO][0-7_]+/, "number"],
        [/\d[\d_]*(\.\d[\d_]*)?([eE][+-]?\d[\d_]*)?/, "number"],
        [/[{}()\[\]]/, "@brackets"],
        [/[<>]/, "@brackets"],
        [/[=><!~?:&|+\-*\/%^]+/, "operator"],
      ],
      lineComment: [[/.*/, "comment", "@pop"]],
      blockComment: [[/[^/*]+/, "comment"], [/\/\*/, "comment", "@push"], ["\\*/", "comment", "@pop"], [/[/*]/, "comment"]],
      string: [[/[^\\\"]+/, "string"], [/\\./, "string.escape"], [/\"/, "string", "@pop"]],
      rawString: [[/\"#*/, "string", "@pop"], [/./, "string"]],
    },
  });
}

function ensureMonaco() {
  if (state.monacoEditor) return state.monacoEditor;
  registerRustLanguage();
  monaco.editor.defineTheme("yuyib-night", {
    base: "vs-dark",
    inherit: true,
    rules: [
      { token: "comment", foreground: "59677A", fontStyle: "italic" },
      { token: "keyword", foreground: "C48CFF" },
      { token: "keyword.type", foreground: "5FD2EA" },
      { token: "type.identifier", foreground: "68D6C1" },
      { token: "macro", foreground: "E8B56B" },
      { token: "annotation", foreground: "E38CC8" },
      { token: "string", foreground: "A9D077" },
      { token: "number", foreground: "E7A56D" },
      { token: "operator", foreground: "88A9C4" },
    ],
    colors: {
      "editor.background": "#0B0E14",
      "editor.foreground": "#C7D0DC",
      "editorLineNumber.foreground": "#343E4E",
      "editorLineNumber.activeForeground": "#7D899B",
      "editorCursor.foreground": "#31D5F4",
      "editor.selectionBackground": "#18495E88",
      "editor.inactiveSelectionBackground": "#19364666",
      "editor.lineHighlightBackground": "#111722",
      "editorIndentGuide.background1": "#1E2632",
      "editorIndentGuide.activeBackground1": "#35475A",
      "editorBracketHighlight.foreground1": "#31D5F4",
      "editorBracketHighlight.foreground2": "#C48CFF",
      "editorBracketHighlight.foreground3": "#FF63BC",
      "editorGutter.background": "#0B0E14",
      "editorOverviewRuler.border": "#00000000",
      "minimap.background": "#0B0E14",
      "scrollbarSlider.background": "#33405255",
      "scrollbarSlider.hoverBackground": "#46556888",
    },
  });

  const initialSource = hosted
    ? {
        content: "// No source document is open.\n// Choose a host-provided file from the Code workspace.\n",
        uri: "yuyib://editor/no-source.rs",
      }
    : sourceDocuments["component.neon-sign"];
  state.monacoModel = monaco.editor.createModel(initialSource.content, "rust", monaco.Uri.parse(initialSource.uri));
  state.monacoEditor = monaco.editor.create(window.document.querySelector("#monacoHost"), {
    model: state.monacoModel,
    theme: "yuyib-night",
    automaticLayout: true,
    fontFamily: '"Cascadia Code", "JetBrains Mono", Consolas, monospace',
    fontSize: 12,
    lineHeight: 20,
    fontLigatures: true,
    minimap: { enabled: true, maxColumn: 70, renderCharacters: false, scale: 0.75 },
    padding: { top: 8, bottom: 12 },
    scrollBeyondLastLine: false,
    smoothScrolling: true,
    cursorSmoothCaretAnimation: "on",
    cursorBlinking: "smooth",
    mouseWheelZoom: true,
    tabSize: 4,
    insertSpaces: true,
    detectIndentation: true,
    formatOnPaste: true,
    autoClosingBrackets: "always",
    autoClosingQuotes: "always",
    autoIndent: "full",
    bracketPairColorization: { enabled: true, independentColorPoolPerBracketType: true },
    guides: { bracketPairs: true, bracketPairsHorizontal: true, highlightActiveBracketPair: true, indentation: true },
    stickyScroll: { enabled: true },
    folding: true,
    foldingHighlight: true,
    showFoldingControls: "mouseover",
    renderWhitespace: "selection",
    renderLineHighlight: "all",
    overviewRulerLanes: 2,
    lightbulb: { enabled: "on" },
    readOnly: hosted,
  });
  if (!hosted) {
    monaco.editor.setModelMarkers(state.monacoModel, "yuyib-mock", [
      { startLineNumber: 17, startColumn: 13, endLineNumber: 17, endColumn: 17, severity: monaco.MarkerSeverity.Warning, message: "Mock: material query currently updates every frame", source: "clippy" },
      { startLineNumber: 18, startColumn: 9, endLineNumber: 18, endColumn: 36, severity: monaco.MarkerSeverity.Info, message: "Preview values are applied to runtime state only", source: "yuyib-authoring" },
    ]);
  } else {
    monaco.editor.setModelMarkers(state.monacoModel, "yuyib-mock", []);
  }
  state.monacoEditor.onDidChangeModelContent(() => {
    if (!hosted || !state.sourcePath) return;
    if (state.sourceChangeTimer) window.clearTimeout(state.sourceChangeTimer);
    state.sourceChangeTimer = window.setTimeout(() => {
      state.sourceChangeTimer = null;
      post("source.change", { path: state.sourcePath, content: state.monacoEditor.getValue() });
    }, 400);
  });
  registerLspProviders();
  state.monacoEditor.focus();
  return state.monacoEditor;
}

function flushSourceChange() {
  if (!hosted || !state.sourcePath || !state.monacoEditor) return;
  if (state.sourceChangeTimer) {
    window.clearTimeout(state.sourceChangeTimer);
    state.sourceChangeTimer = null;
  }
  post("source.change", { path: state.sourcePath, content: state.monacoEditor.getValue() });
}

function resolveLspPending(requestId, payload) {
  if (!requestId || !state.lspPending.has(requestId)) return;
  const pending = state.lspPending.get(requestId);
  state.lspPending.delete(requestId);
  if (pending.timer) window.clearTimeout(pending.timer);
  pending.resolve(payload);
}

function requestLsp(endpoint, line, column, extra = {}, timeoutMs = 2500) {
  return requestLspPayload(endpoint, { line, column, ...extra }, timeoutMs);
}

function requestLspPayload(endpoint, payload, timeoutMs = 2500) {
  return new Promise((resolve) => {
    if (!state.sourcePath) {
      resolve(null);
      return;
    }
    const requestId = `lsp-${state.requestId}-${Date.now()}`;
    const timer = window.setTimeout(() => {
      if (!state.lspPending.has(requestId)) return;
      state.lspPending.delete(requestId);
      resolve(null);
    }, timeoutMs);
    state.lspPending.set(requestId, { resolve, timer });
    post(endpoint, {
      request_id: requestId,
      path: state.sourcePath,
      ...payload,
    });
  });
}

function normalizeSourcePath(path) {
  return String(path || "").replace(/\\/g, "/");
}

function resolveRenameResource(path) {
  const normalized = normalizeSourcePath(path);
  const current = normalizeSourcePath(state.sourcePath);
  if (state.monacoModel && (normalized === current || normalized.endsWith(current) || current.endsWith(normalized))) {
    return state.monacoModel.uri;
  }
  const models = monaco.editor.getModels();
  for (const model of models) {
    const uriPath = normalizeSourcePath(model.uri.path || model.uri.toString());
    if (uriPath.endsWith(normalized) || uriPath.includes(`/${normalized}`)) return model.uri;
  }
  return monaco.Uri.parse(`yuyib://project/${normalized}`);
}

function workspaceEditsFromFiles(files) {
  const edits = [];
  let otherFiles = 0;
  const current = normalizeSourcePath(state.sourcePath);
  for (const file of files || []) {
    const filePath = normalizeSourcePath(file.path);
    const isCurrent = filePath === current
      || filePath.endsWith(current)
      || current.endsWith(filePath);
    if (!isCurrent) otherFiles += 1;
    const resource = resolveRenameResource(file.path);
    for (const edit of (file.edits || [])) {
      edits.push({
        resource,
        textEdit: {
          range: {
            startLineNumber: edit.start_line || 1,
            startColumn: edit.start_column || 1,
            endLineNumber: edit.end_line || edit.start_line || 1,
            endColumn: edit.end_column || (edit.start_column || 1) + 1,
          },
          text: edit.new_text ?? "",
        },
      });
    }
  }
  return { edits, otherFiles };
}

function applyWorkspaceTextEdits(edits) {
  if (!Array.isArray(edits) || !edits.length) return;
  const byModel = new Map();
  for (const item of edits) {
    const model = monaco.editor.getModel(item.resource) || (
      state.monacoModel
      && item.resource
      && state.monacoModel.uri.toString() === item.resource.toString()
        ? state.monacoModel
        : null
    );
    if (!model) continue;
    if (!byModel.has(model)) byModel.set(model, []);
    byModel.get(model).push({
      range: item.textEdit.range,
      text: item.textEdit.text,
      forceMoveMarkers: true,
    });
  }
  for (const [model, modelEdits] of byModel) {
    if (state.monacoEditor && state.monacoEditor.getModel() === model) {
      state.monacoEditor.executeEdits("yuyib-lsp", modelEdits);
    } else {
      model.pushEditOperations([], modelEdits, () => null);
    }
  }
}

function lspCodeActionKindToMonaco(kind) {
  const value = String(kind || "");
  if (value.startsWith("quickfix")) return monaco.languages.CodeActionKind.QuickFix;
  if (value.startsWith("refactor.extract")) return monaco.languages.CodeActionKind.RefactorExtract;
  if (value.startsWith("refactor.inline")) return monaco.languages.CodeActionKind.RefactorInline;
  if (value.startsWith("refactor.rewrite")) return monaco.languages.CodeActionKind.RefactorRewrite;
  if (value.startsWith("refactor")) return monaco.languages.CodeActionKind.Refactor;
  if (value.startsWith("source.organizeImports")) return monaco.languages.CodeActionKind.SourceOrganizeImports;
  if (value.startsWith("source")) return monaco.languages.CodeActionKind.Source;
  return monaco.languages.CodeActionKind.Empty;
}

function lspCompletionKindToMonaco(kind) {
  const map = {
    1: monaco.languages.CompletionItemKind.Text,
    2: monaco.languages.CompletionItemKind.Method,
    3: monaco.languages.CompletionItemKind.Function,
    4: monaco.languages.CompletionItemKind.Constructor,
    5: monaco.languages.CompletionItemKind.Field,
    6: monaco.languages.CompletionItemKind.Variable,
    7: monaco.languages.CompletionItemKind.Class,
    8: monaco.languages.CompletionItemKind.Interface,
    9: monaco.languages.CompletionItemKind.Module,
    10: monaco.languages.CompletionItemKind.Property,
    11: monaco.languages.CompletionItemKind.Unit,
    12: monaco.languages.CompletionItemKind.Value,
    13: monaco.languages.CompletionItemKind.Enum,
    14: monaco.languages.CompletionItemKind.Keyword,
    15: monaco.languages.CompletionItemKind.Snippet,
    16: monaco.languages.CompletionItemKind.Color,
    17: monaco.languages.CompletionItemKind.File,
    18: monaco.languages.CompletionItemKind.Reference,
    19: monaco.languages.CompletionItemKind.Folder,
    20: monaco.languages.CompletionItemKind.EnumMember,
    21: monaco.languages.CompletionItemKind.Constant,
    22: monaco.languages.CompletionItemKind.Struct,
    23: monaco.languages.CompletionItemKind.Event,
    24: monaco.languages.CompletionItemKind.Operator,
    25: monaco.languages.CompletionItemKind.TypeParameter,
  };
  return map[Number(kind)] || monaco.languages.CompletionItemKind.Text;
}

function registerLspProviders() {
  if (state.lspProvidersRegistered) return;
  state.lspProvidersRegistered = true;
  monaco.editor.registerCommand("yuyib.lsp.executeCommand", async (_accessor, payload) => {
    const command = payload?.command;
    if (!command) return;
    flushSourceChange();
    const result = await requestLspPayload("lsp.executeCommand", {
      command,
      arguments: Array.isArray(payload?.arguments) ? payload.arguments : [],
    }, 8000);
    if (!result) {
      showToast("Code action", "executeCommand timed out", "warning");
      return;
    }
    if (result.error) {
      showToast("Code action failed", result.error, "warning");
      appendOutput("lsp", `executeCommand ${command} error: ${result.error}`);
      return;
    }
    const files = Array.isArray(result.files) ? result.files : [];
    if (files.length) {
      const { edits, otherFiles } = workspaceEditsFromFiles(files);
      applyWorkspaceTextEdits(edits);
      if (otherFiles > 0) {
        showToast(
          "Command applied",
          `Also touches ${otherFiles} other file(s). Open them to see RA-synced buffers.`,
          "info",
        );
      }
    }
    appendOutput("lsp", `executeCommand ${command}`);
  });
  monaco.languages.registerCompletionItemProvider("rust", {
    triggerCharacters: [".", ":", "<", "'"],
    provideCompletionItems: async (model, position) => {
      flushSourceChange();
      const word = model.getWordUntilPosition(position);
      const range = {
        startLineNumber: position.lineNumber,
        endLineNumber: position.lineNumber,
        startColumn: word.startColumn,
        endColumn: word.endColumn,
      };
      const payload = await requestLsp("lsp.completion", position.lineNumber, position.column);
      const items = Array.isArray(payload?.items) ? payload.items : [];
      return {
        suggestions: items.map((item) => ({
          label: item.label,
          kind: lspCompletionKindToMonaco(item.kind),
          detail: item.detail || undefined,
          documentation: item.documentation || undefined,
          insertText: item.insert_text || item.label,
          filterText: item.filter_text || undefined,
          sortText: item.sort_text || undefined,
          range,
        })),
      };
    },
  });
  monaco.languages.registerHoverProvider("rust", {
    provideHover: async (model, position) => {
      flushSourceChange();
      const payload = await requestLsp("lsp.hover", position.lineNumber, position.column);
      const markdown = payload?.markdown;
      if (!markdown) return null;
      return {
        contents: [{ value: String(markdown) }],
      };
    },
  });
  monaco.languages.registerSignatureHelpProvider("rust", {
    signatureHelpTriggerCharacters: ["(", ","],
    signatureHelpRetriggerCharacters: [","],
    provideSignatureHelp: async (model, position, _token, context) => {
      flushSourceChange();
      const payload = await requestLsp(
        "lsp.signatureHelp",
        position.lineNumber,
        position.column,
        {
          trigger_kind: Number(context?.triggerKind) || 1,
          trigger_character: context?.triggerCharacter || null,
          is_retrigger: Boolean(context?.isRetrigger),
        },
        3000,
      );
      const help = payload?.help;
      if (!help || !Array.isArray(help.signatures) || !help.signatures.length) return null;
      return {
        value: {
          signatures: help.signatures.map((signature) => ({
            label: signature.label || "",
            documentation: signature.documentation || undefined,
            parameters: (signature.parameters || []).map((parameter) => ({
              label: parameter.label || "",
              documentation: parameter.documentation || undefined,
            })),
            activeParameter: signature.active_parameter,
          })),
          activeSignature: Number(help.active_signature) || 0,
          activeParameter: Number(help.active_parameter) || 0,
        },
        dispose() {},
      };
    },
  });
  monaco.languages.registerDefinitionProvider("rust", {
    provideDefinition: async (model, position) => {
      flushSourceChange();
      const payload = await requestLsp("lsp.definition", position.lineNumber, position.column, {}, 4000);
      const locations = Array.isArray(payload?.locations) ? payload.locations : [];
      if (!locations.length) return null;
      const current = normalizeSourcePath(state.sourcePath);
      const monacoLocations = [];
      for (const loc of locations) {
        const path = normalizeSourcePath(loc.path);
        const isCurrent = path === current
          || path.endsWith(current)
          || current.endsWith(path);
        const range = {
          startLineNumber: loc.start_line || 1,
          startColumn: loc.start_column || 1,
          endLineNumber: loc.end_line || loc.start_line || 1,
          endColumn: loc.end_column || (loc.start_column || 1) + 1,
        };
        if (isCurrent) {
          monacoLocations.push({ uri: model.uri, range });
        } else if (!state.pendingDefinitionReveal) {
          state.pendingDefinitionReveal = {
            path: loc.path,
            line: range.startLineNumber,
            column: range.startColumn,
          };
          post("source.read", { path: loc.path });
          appendOutput("lsp", `definition → open ${loc.path}:${range.startLineNumber}`);
        }
      }
      return monacoLocations.length ? monacoLocations : null;
    },
  });
  monaco.languages.registerReferenceProvider("rust", {
    provideReferences: async (model, position, context) => {
      flushSourceChange();
      const includeDeclaration = context?.includeDeclaration !== false;
      const payload = await requestLsp(
        "lsp.references",
        position.lineNumber,
        position.column,
        { include_declaration: includeDeclaration },
        5000,
      );
      const locations = Array.isArray(payload?.locations) ? payload.locations : [];
      if (!locations.length) return null;
      const monacoLocations = locations.map((loc) => ({
        uri: resolveRenameResource(loc.path),
        range: {
          startLineNumber: loc.start_line || 1,
          startColumn: loc.start_column || 1,
          endLineNumber: loc.end_line || loc.start_line || 1,
          endColumn: loc.end_column || (loc.start_column || 1) + 1,
        },
      }));
      appendOutput("lsp", `references → ${monacoLocations.length} location(s)`);
      return monacoLocations;
    },
  });
  monaco.languages.registerRenameProvider("rust", {
    provideRenameEdits: async (model, position, newName) => {
      flushSourceChange();
      const payload = await requestLsp(
        "lsp.rename",
        position.lineNumber,
        position.column,
        { new_name: newName },
        5000,
      );
      if (!payload) {
        return { edits: [], rejectReason: "Rename timed out" };
      }
      if (payload.error) {
        return { edits: [], rejectReason: String(payload.error) };
      }
      const files = Array.isArray(payload.files) ? payload.files : [];
      const { edits, otherFiles } = workspaceEditsFromFiles(files);
      if (otherFiles > 0) {
        showToast(
          "Rename",
          `Also touches ${otherFiles} other file(s). Open them to see RA-synced buffers, or save/reload after disk apply.`,
          "info",
        );
      }
      appendOutput("lsp", `rename → ${edits.length} edit(s) across ${files.length} file(s)`);
      return { edits };
    },
  });
  monaco.languages.registerCodeActionProvider("rust", {
    provideCodeActions: async (model, range, context) => {
      flushSourceChange();
      const markers = Array.isArray(context?.markers) ? context.markers : [];
      const payload = await requestLspPayload("lsp.codeAction", {
        start_line: range.startLineNumber,
        start_column: range.startColumn,
        end_line: range.endLineNumber,
        end_column: range.endColumn,
        diagnostics: markers.map((marker) => ({
          startLineNumber: marker.startLineNumber,
          startColumn: marker.startColumn,
          endLineNumber: marker.endLineNumber,
          endColumn: marker.endColumn,
          severity: marker.severity,
          message: marker.message,
          source: marker.source,
        })),
      }, 4000);
      const actions = Array.isArray(payload?.actions) ? payload.actions : [];
      return {
        actions: actions.map((action) => {
          const { edits, otherFiles } = workspaceEditsFromFiles(action.files || []);
          if (otherFiles > 0) {
            appendOutput("lsp", `codeAction "${action.title}" touches ${otherFiles} other file(s)`);
          }
          const mapped = {
            title: action.title || "Code action",
            kind: lspCodeActionKindToMonaco(action.kind),
            isPreferred: Boolean(action.is_preferred),
            disabled: action.disabled || undefined,
            diagnostics: markers,
          };
          if (edits.length) {
            mapped.edit = { edits };
          }
          if (action.command?.command) {
            mapped.command = {
              id: "yuyib.lsp.executeCommand",
              title: action.command.title || action.title || action.command.command,
              arguments: [{
                command: action.command.command,
                arguments: Array.isArray(action.command.arguments) ? action.command.arguments : [],
              }],
            };
          }
          return mapped;
        }),
        dispose() {},
      };
    },
  });
}

function lspSeverityToMonaco(severity) {
  switch (Number(severity)) {
    case 1: return monaco.MarkerSeverity.Error;
    case 2: return monaco.MarkerSeverity.Warning;
    case 3: return monaco.MarkerSeverity.Info;
    case 4: return monaco.MarkerSeverity.Hint;
    default: return monaco.MarkerSeverity.Warning;
  }
}

function handleLspStatus(payload) {
  state.lspStatus = payload?.status || "idle";
  const message = payload?.message ? ` · ${payload.message}` : "";
  appendOutput("lsp", `status ${state.lspStatus}${message}`);
  if (state.lspStatus === "unavailable" || state.lspStatus === "error") {
    showToast("rust-analyzer", payload?.message || state.lspStatus, "warning");
  }
}

function applyLspDiagnostics(payload) {
  const path = payload?.path || "";
  const diagnostics = Array.isArray(payload?.diagnostics) ? payload.diagnostics : [];
  if (!state.monacoModel) return;
  const modelPath = (state.sourcePath || "").replace(/\//g, "\\");
  const diagPath = String(path).replace(/\//g, "\\");
  const pathMatches = !path
    || !state.sourcePath
    || diagPath.endsWith(modelPath)
    || diagPath.toLowerCase().endsWith(modelPath.toLowerCase())
    || modelPath.endsWith(diagPath.split("\\").slice(-2).join("\\"));
  if (!pathMatches && state.sourcePath) return;
  const markers = diagnostics.map((item) => ({
    startLineNumber: item.start_line || 1,
    startColumn: item.start_column || 1,
    endLineNumber: item.end_line || item.start_line || 1,
    endColumn: item.end_column || (item.start_column || 1) + 1,
    severity: lspSeverityToMonaco(item.severity),
    message: item.message || "diagnostic",
    source: item.source || "rust-analyzer",
  }));
  monaco.editor.setModelMarkers(state.monacoModel, "rust-analyzer", markers);
  appendOutput("lsp", `${markers.length} diagnostic(s) for ${path || state.sourcePath || "?"}`);
}

function renderSourceTree(payload) {
  const explorer = document.querySelector(".code-explorer");
  if (!explorer) return;
  const files = Array.isArray(payload?.files) ? payload.files : [];
  state.sourceTree = files;
  explorer.querySelectorAll(
    ".code-tree-row, .code-tree-group, .code-outline-heading, .outline-row, .data-empty-state",
  ).forEach((el) => el.remove());
  const workspaceName = explorer.querySelector(".code-workspace-name");
  if (workspaceName) {
    const rootLabel = String(payload?.root || state.projectConfig.root || "WORKSPACE").replace(/\\/g, "/").split("/").filter(Boolean).at(-1) || "WORKSPACE";
    workspaceName.textContent = rootLabel.toUpperCase();
  }
  if (!files.length) {
    const empty = document.createElement("div");
    empty.className = "data-empty-state";
    empty.textContent = "No .rs files found under project code_root";
    explorer.append(empty);
    return;
  }
  const tree = buildSourceFolderTree(files);
  renderSourceFolderNode(explorer, tree, "", 0);
  const preferred = payload?.preferred || files.find((path) => path.endsWith("main.rs")) || files[0];
  if (preferred && !state.sourcePath && state.view === "code") {
    post("source.read", { path: preferred });
  } else if (state.sourcePath) {
    highlightSourceTreePath(state.sourcePath);
  }
  renderSystemsOutline();
}

function buildSourceFolderTree(files) {
  const root = { dirs: new Map(), files: [] };
  for (const path of files) {
    const parts = String(path).replace(/\\/g, "/").split("/").filter(Boolean);
    if (!parts.length) continue;
    let node = root;
    for (let i = 0; i < parts.length - 1; i += 1) {
      const part = parts[i];
      if (!node.dirs.has(part)) node.dirs.set(part, { dirs: new Map(), files: [] });
      node = node.dirs.get(part);
    }
    const fileName = parts.at(-1);
    if (fileName && !node.files.includes(fileName)) node.files.push(fileName);
  }
  return root;
}

function renderSourceFolderNode(parent, node, prefix, depth) {
  const dirNames = [...node.dirs.keys()].sort((a, b) => a.localeCompare(b));
  for (const name of dirNames) {
    const folderPath = prefix ? `${prefix}/${name}` : name;
    const group = document.createElement("div");
    group.className = "code-tree-group is-expanded";
    group.dataset.folder = folderPath;

    const row = document.createElement("button");
    row.type = "button";
    row.className = "code-tree-row is-open";
    row.style.paddingLeft = `${7 + depth * 14}px`;
    row.dataset.folder = folderPath;
    row.innerHTML = `<svg><use href="#i-chevron"/></svg><span></span>`;
    row.querySelector("span").textContent = name;
    row.addEventListener("click", (event) => {
      event.preventDefault();
      const expanded = group.classList.toggle("is-expanded");
      row.classList.toggle("is-open", expanded);
    });

    const children = document.createElement("div");
    children.className = "code-tree-children";
    renderSourceFolderNode(children, node.dirs.get(name), folderPath, depth + 1);

    group.append(row, children);
    parent.append(group);
  }

  const fileNames = [...node.files].sort((a, b) => a.localeCompare(b));
  for (const name of fileNames) {
    const path = prefix ? `${prefix}/${name}` : name;
    const row = document.createElement("button");
    row.type = "button";
    row.className = "code-tree-row";
    row.style.paddingLeft = `${7 + depth * 14}px`;
    row.dataset.path = path;
    row.innerHTML = `<span class="rust-file-icon">Rs</span><span></span>`;
    row.querySelectorAll("span")[1].textContent = name;
    row.title = path;
    row.addEventListener("click", () => {
      highlightSourceTreePath(path);
      post("source.read", { path });
    });
    parent.append(row);
  }
}

function highlightSourceTreePath(path) {
  const normalized = String(path || "").replace(/\\/g, "/");
  document.querySelectorAll(".code-explorer [data-path]").forEach((row) => {
    row.classList.toggle("is-selected", row.dataset.path === normalized);
  });
}

function openDocument(sourceDoc) {
  setMainView("code");
  const editor = ensureMonaco();
  const uri = monaco.Uri.parse(sourceDoc.uri || `yuyib://project/${sourceDoc.path || sourceDoc.display_name}`);
  let model = monaco.editor.getModel(uri);
  if (!model) model = monaco.editor.createModel(sourceDoc.content || "", sourceDoc.language || "rust", uri);
  else if (typeof sourceDoc.content === "string" && model.getValue() !== sourceDoc.content) model.setValue(sourceDoc.content);
  editor.setModel(model);
  editor.updateOptions({ readOnly: Boolean(sourceDoc.read_only) });
  state.monacoModel = model;
  state.sourcePath = sourceDoc.path || state.sourcePath;
  state.sourceRevision = sourceDoc.revision ?? state.sourceRevision;
  monaco.editor.setModelMarkers(model, "rust-analyzer", []);
  highlightSourceTreePath(state.sourcePath);
  const displayName = sourceDoc.display_name || state.sourcePath.split("/").at(-1);
  document.querySelector(".code-file-tab").childNodes[1].textContent = displayName;
  const crumbs = document.querySelectorAll(".code-breadcrumb span");
  if (crumbs[0]) crumbs[0].textContent = sourceDoc.external ? "workspace" : "project";
  if (crumbs[1]) {
    const parts = String(state.sourcePath || "").replace(/\\/g, "/").split("/");
    crumbs[1].textContent = parts.length > 1 ? parts.slice(0, -1).join("/") : "src";
  }
  if (crumbs[2]) crumbs[2].textContent = displayName;
  applyPendingDefinitionReveal(editor);
  showToast(
    sourceDoc.external ? "Engine source opened" : "Source opened",
    `${displayName} · ${state.sourcePath}${sourceDoc.read_only ? " · read-only" : ""}`,
    "success",
  );
}

function applyPendingDefinitionReveal(editor) {
  const pending = state.pendingDefinitionReveal;
  if (!pending || !editor) return;
  const current = normalizeSourcePath(state.sourcePath);
  const want = normalizeSourcePath(pending.path);
  if (!(current === want || current.endsWith(want) || want.endsWith(current))) return;
  state.pendingDefinitionReveal = null;
  const position = {
    lineNumber: pending.line || 1,
    column: pending.column || 1,
  };
  editor.setPosition(position);
  editor.revealPositionInCenter(position);
  editor.focus();
  appendOutput("lsp", `definition reveal ${state.sourcePath}:${position.lineNumber}`);
}

function runScopedCargoCheck() {
  if (!state.projectConfig.package) {
    showToast("Cargo check not configured", "host.coverage did not provide project.package", "warning");
    return;
  }
  setCargoStatus("cargo check · queued", 0.02);
  post("cargo.check", { package: state.projectConfig.package });
}

function runProjectCook() {
  if (!state.projectConfig.ready) {
    showToast("Cook assets", "Open a project first", "warning");
    return;
  }
  setCargoStatus("cook · queued", 0.02);
  post("project.cook", {});
}

function runYpackExport() {
  if (!state.projectConfig.ready) {
    showToast("Export ypack", "Open a project first", "warning");
    return;
  }
  setCargoStatus("ypack export · queued", 0.02);
  post("project.export_ypack", {});
}

function runYpackImport() {
  if (!state.projectConfig.ready) {
    showToast("Import ypack", "Open a project first", "warning");
    return;
  }
  setCargoStatus("ypack import · queued", 0.02);
  post("project.import_ypack", {});
}

function drawScene() {
  const canvas = document.querySelector("#sceneCanvas");
  if (!canvas || canvas.clientWidth === 0 || canvas.clientHeight === 0) return;
  const ratio = Math.min(window.devicePixelRatio || 1, 2);
  const width = canvas.clientWidth;
  const height = canvas.clientHeight;
  canvas.width = Math.round(width * ratio);
  canvas.height = Math.round(height * ratio);
  const ctx = canvas.getContext("2d");
  ctx.setTransform(ratio, 0, 0, ratio, 0, 0);

  if (hosted) {
    ctx.clearRect(0, 0, width, height);
    return;
  }

  const sky = ctx.createLinearGradient(0, 0, 0, height);
  sky.addColorStop(0, "#090d18");
  sky.addColorStop(0.42, "#11182a");
  sky.addColorStop(0.68, "#0c111a");
  sky.addColorStop(1, "#07090d");
  ctx.fillStyle = sky;
  ctx.fillRect(0, 0, width, height);

  const haze = ctx.createRadialGradient(width * 0.54, height * 0.39, 0, width * 0.54, height * 0.39, width * 0.5);
  haze.addColorStop(0, "rgba(47,81,128,.24)");
  haze.addColorStop(0.45, "rgba(49,38,92,.08)");
  haze.addColorStop(1, "rgba(0,0,0,0)");
  ctx.fillStyle = haze;
  ctx.fillRect(0, 0, width, height);

  const skyline = [
    [0, .30, .12, .40], [.09, .20, .10, .52], [.17, .35, .08, .37], [.23, .16, .12, .56], [.34, .29, .08, .43],
    [.40, .11, .10, .61], [.49, .26, .11, .46], [.57, .18, .08, .54], [.63, .31, .10, .41], [.72, .14, .13, .58], [.84, .25, .08, .47], [.91, .19, .11, .53],
  ];
  skyline.forEach(([x, y, w, h], index) => {
    const x0 = x * width;
    const y0 = y * height;
    const bw = w * width;
    const bh = h * height;
    const building = ctx.createLinearGradient(x0, 0, x0 + bw, 0);
    building.addColorStop(0, index % 2 ? "#101722" : "#0d141e");
    building.addColorStop(0.75, index % 2 ? "#171e2a" : "#131a25");
    building.addColorStop(1, "#080d14");
    ctx.fillStyle = building;
    ctx.fillRect(x0, y0, bw, bh);
    ctx.strokeStyle = "rgba(77,96,126,.18)";
    ctx.strokeRect(x0 + .5, y0 + .5, bw - 1, bh - 1);
    for (let wy = y0 + 11; wy < y0 + bh - 8; wy += 14) {
      for (let wx = x0 + 7; wx < x0 + bw - 5; wx += 12) {
        const lit = ((Math.floor(wx) + Math.floor(wy) + index * 7) % 5) === 0;
        ctx.fillStyle = lit ? (index % 3 === 0 ? "rgba(255,66,177,.48)" : "rgba(59,201,239,.38)") : "rgba(40,50,68,.38)";
        ctx.fillRect(wx, wy, 4, 3);
      }
    }
  });

  const horizon = height * 0.55;
  const road = ctx.createLinearGradient(0, horizon, 0, height);
  road.addColorStop(0, "#10141b");
  road.addColorStop(1, "#080a0e");
  ctx.fillStyle = road;
  ctx.beginPath();
  ctx.moveTo(width * .35, horizon);
  ctx.lineTo(width * .67, horizon);
  ctx.lineTo(width * .92, height);
  ctx.lineTo(width * .09, height);
  ctx.closePath();
  ctx.fill();

  const vanishingX = width * .52;
  ctx.lineWidth = 1;
  ctx.strokeStyle = "rgba(57,72,91,.32)";
  for (let x = -width; x <= width * 2; x += width * .07) {
    ctx.beginPath(); ctx.moveTo(vanishingX, horizon); ctx.lineTo(x, height); ctx.stroke();
  }
  for (let i = 0; i < 18; i += 1) {
    const t = i / 18;
    const y = horizon + (height - horizon) * t * t;
    ctx.globalAlpha = .18 + t * .28;
    ctx.beginPath(); ctx.moveTo(0, y); ctx.lineTo(width, y); ctx.stroke();
  }
  ctx.globalAlpha = 1;

  ctx.strokeStyle = "rgba(35,207,236,.32)";
  ctx.lineWidth = 1;
  ctx.beginPath(); ctx.moveTo(width * .25, height); ctx.lineTo(width * .46, horizon); ctx.stroke();
  ctx.beginPath(); ctx.moveTo(width * .79, height); ctx.lineTo(width * .59, horizon); ctx.stroke();

  const objects = [
    { x: .24, y: .57, w: .10, h: .24, color: "#17222b" },
    { x: .69, y: .53, w: .13, h: .31, color: "#111b25" },
    { x: .34, y: .63, w: .09, h: .17, color: "#151d25" },
    { x: .58, y: .67, w: .08, h: .12, color: "#17202a" },
  ];
  objects.forEach((object, index) => {
    const ox = width * object.x;
    const oy = height * object.y;
    const ow = width * object.w;
    const oh = height * object.h;
    ctx.fillStyle = object.color;
    ctx.fillRect(ox, oy, ow, oh);
    ctx.strokeStyle = "rgba(65,85,107,.44)";
    ctx.strokeRect(ox + .5, oy + .5, ow - 1, oh - 1);
    ctx.fillStyle = index % 2 ? "rgba(49,193,229,.20)" : "rgba(237,51,166,.18)";
    ctx.fillRect(ox + ow * .12, oy + oh * .16, ow * .76, 2);
  });

  const signX = width * .64;
  const signY = height * .43;
  const signW = Math.max(82, width * .13);
  const signH = Math.max(40, height * .12);
  ctx.save();
  ctx.shadowColor = "#ff2cad";
  ctx.shadowBlur = 18;
  ctx.fillStyle = "rgba(238,38,160,.18)";
  ctx.fillRect(signX, signY, signW, signH);
  ctx.shadowBlur = 7;
  ctx.strokeStyle = "#ff43b6";
  ctx.lineWidth = 1.5;
  ctx.strokeRect(signX, signY, signW, signH);
  ctx.font = `700 ${Math.max(14, signH * .47)}px sans-serif`;
  ctx.textAlign = "center";
  ctx.textBaseline = "middle";
  ctx.fillStyle = "#ff6bc6";
  ctx.fillText("夜雨", signX + signW / 2, signY + signH / 2);
  ctx.restore();

  if (state.overlays.bounds) {
    ctx.save();
    ctx.setLineDash([4, 4]);
    ctx.strokeStyle = "rgba(59,218,245,.92)";
    ctx.strokeRect(signX - 8, signY - 8, signW + 16, signH + 16);
    ctx.setLineDash([]);
    const handles = [[signX - 8, signY - 8], [signX + signW + 8, signY - 8], [signX - 8, signY + signH + 8], [signX + signW + 8, signY + signH + 8]];
    handles.forEach(([x, y]) => { ctx.fillStyle = "#081018"; ctx.fillRect(x - 3, y - 3, 6, 6); ctx.strokeStyle = "#43daf4"; ctx.strokeRect(x - 3, y - 3, 6, 6); });
    ctx.restore();
  }

  if (state.overlays.collision) {
    ctx.save();
    ctx.fillStyle = "rgba(62,222,144,.10)";
    ctx.strokeStyle = "rgba(76,232,157,.75)";
    ctx.setLineDash([5, 3]);
    ctx.fillRect(signX - 3, signY - 3, signW + 6, signH + 6);
    ctx.strokeRect(signX - 3, signY - 3, signW + 6, signH + 6);
    ctx.restore();
  }

  if (state.overlays.normals) {
    ctx.strokeStyle = "rgba(132,119,255,.75)";
    for (let i = 0; i < 8; i += 1) {
      const nx = signX + (i + .5) * signW / 8;
      ctx.beginPath(); ctx.moveTo(nx, signY); ctx.lineTo(nx - 3, signY - 14); ctx.stroke();
    }
  }

  const gizmoX = signX + signW / 2;
  const gizmoY = signY + signH + 8;
  ctx.lineWidth = 2;
  ctx.strokeStyle = "#ff5a70";
  ctx.beginPath(); ctx.moveTo(gizmoX, gizmoY); ctx.lineTo(gizmoX + 45, gizmoY + 10); ctx.stroke();
  ctx.fillStyle = "#ff5a70"; ctx.beginPath(); ctx.arc(gizmoX + 45, gizmoY + 10, 3, 0, Math.PI * 2); ctx.fill();
  ctx.strokeStyle = "#49db9e";
  ctx.beginPath(); ctx.moveTo(gizmoX, gizmoY); ctx.lineTo(gizmoX, gizmoY - 45); ctx.stroke();
  ctx.fillStyle = "#49db9e"; ctx.beginPath(); ctx.arc(gizmoX, gizmoY - 45, 3, 0, Math.PI * 2); ctx.fill();
  ctx.strokeStyle = "#5b85ff";
  ctx.beginPath(); ctx.moveTo(gizmoX, gizmoY); ctx.lineTo(gizmoX - 25, gizmoY + 25); ctx.stroke();

  const vignette = ctx.createRadialGradient(width / 2, height / 2, Math.min(width, height) * .2, width / 2, height / 2, Math.max(width, height) * .75);
  vignette.addColorStop(0, "rgba(0,0,0,0)");
  vignette.addColorStop(1, "rgba(0,0,0,.45)");
  ctx.fillStyle = vignette;
  ctx.fillRect(0, 0, width, height);
}

document.querySelector("#bridgeMode").textContent = hosted ? "Yuyib bridge" : "Browser mock";
document.querySelector("#bridgeMode").classList.toggle("is-hosted", hosted);

document.querySelector("#sceneTree").addEventListener("click", (event) => {
  const row = event.target.closest("[data-kind='entity'][data-id]");
  if (row) requestSelection("entity", row.dataset.id);
});

document.querySelectorAll("#assetGrid [data-kind][data-id]").forEach((element) => {
  element.addEventListener("click", () => requestAssetOpen(element.dataset.id));
});

document.querySelectorAll(".document-tab").forEach((tab) => tab.addEventListener("click", () => setMainView(tab.dataset.view)));

document.querySelectorAll(".bottom-tab").forEach((tab) => {
  tab.addEventListener("click", () => {
    document.querySelectorAll(".bottom-tab").forEach((item) => item.classList.toggle("is-active", item === tab));
    document.querySelectorAll(".bottom-content").forEach((panel) => panel.classList.remove("is-visible"));
    const ids = { diagnostics: "#diagnosticsPanel", "preview-log": "#previewLogPanel", output: "#outputPanel" };
    document.querySelector(ids[tab.dataset.bottom]).classList.add("is-visible");
  });
});

document.querySelector("#componentList").addEventListener("change", (event) => {
  const input = event.target.closest("[data-scene-field][data-component-id]");
  if (!input || !state.selection) return;
  let value = input.type === "checkbox" ? input.checked : input.value;
  const wantsNumber = input.dataset.fieldKind === "number"
    || input.dataset.fieldKind === "f32"
    || input.dataset.fieldKind === "i32"
    || input.dataset.fieldKind === "u32"
    || input.type === "number"
    || /^(translation|rotation|scale)\.[xyzw]$/.test(input.dataset.sceneField || "");
  if (wantsNumber) {
    value = Number(input.value);
    if (!Number.isFinite(value)) {
      showToast("Invalid number", `${input.dataset.sceneField} requires a finite number`, "warning");
      return;
    }
  }
  if (input.dataset.fieldKind === "asset" && value === "") {
    value = null;
  }
  sendSceneCommand({
    type: "component.field.set",
    entity_guid: state.selection.stableId,
    component_id: input.dataset.componentId,
    field_path: input.dataset.sceneField,
    value,
  });
});

document.querySelector("#entityNameInput").addEventListener("change", (event) => {
  const name = event.target.value.trim();
  if (!name || !state.selection) return;
  sendSceneCommand({ type: "entity.rename", entity_guid: state.selection.stableId, name });
});

document.querySelectorAll("[data-overlay]").forEach((input) => input.addEventListener("change", () => {
  const overlay = input.dataset.overlay;
  state.overlays[overlay] = input.checked;
  document.querySelectorAll(`[data-overlay="${overlay}"]`).forEach((other) => {
    other.checked = input.checked;
  });
  if (hosted) {
    if (overlay !== "bounds" && overlay !== "collision" && overlay !== "normals" && overlay !== "tangents" && overlay !== "uv") {
      showToast("Overlay unavailable", `${overlay} overlay is not wired on the native host yet`, "info");
      state.overlays[overlay] = false;
      document.querySelectorAll(`[data-overlay="${overlay}"]`).forEach((other) => {
        other.checked = false;
      });
      return;
    }
    if (state.view !== "preview") {
      showToast("Asset Preview only", "Preview overlays draw on the Asset Preview tab after opening a .glb/.gltf", "info");
      setMainView("preview");
    }
    post("preview.overlay.set", { overlay, enabled: input.checked });
    return;
  }
  executeCommand("preview.overlay.set", { overlay, enabled: input.checked });
  drawScene();
}));

document.querySelectorAll("[data-tool]").forEach((button) => button.addEventListener("click", () => {
  setViewportTool(button.dataset.tool);
}));

document.querySelectorAll("[data-window]").forEach((button) => button.addEventListener("click", () => {
  const action = button.dataset.window;
  if (!action) return;
  if (hosted) {
    post("window.control", { action });
    return;
  }
  if (action === "close") window.close();
}));

document.querySelectorAll("[data-command]").forEach((button) => button.addEventListener("click", () => {
  if (button.dataset.command === "history.undo") sendSceneCommand({ type: "history.undo" });
  else if (button.dataset.command === "history.redo") sendSceneCommand({ type: "history.redo" });
  else executeCommand(button.dataset.command);
}));

document.querySelectorAll("[data-menu]").forEach((button) => button.addEventListener("click", () => {
  switch (button.dataset.menu) {
    case "file-open-project":
      if (state.projectConfig.ready) {
        setLauncherStatus("Opening folder picker…");
        post("project.openInteractive", {});
      } else {
        setLauncherVisible(true, { preserveStatus: true });
        setLauncherStatus("Create or open a project to begin.");
      }
      break;
    case "edit-menu":
      showToast("Edit", "Use the toolbar Undo/Redo buttons, or Ctrl+Z / Ctrl+Y", "info");
      document.querySelector('[data-command="history.undo"]')?.focus();
      break;
    case "assets-focus":
      document.querySelector('[data-rail="assets"]')?.click();
      document.querySelector("#assetSearch")?.focus();
      break;
    case "scene-open":
      document.querySelector("#openSceneButton")?.click();
      break;
    case "build-check":
      runScopedCargoCheck();
      break;
    case "build-cook":
      runProjectCook();
      break;
    case "build-ypack":
      runYpackExport();
      break;
    case "build-ypack-import":
      runYpackImport();
      break;
    case "help-about":
      showToast("Yuyib Editor", "Foundation authoring shell · create a project to begin", "info");
      break;
    default:
      break;
  }
}));

document.querySelectorAll("[data-rail]").forEach((button) => button.addEventListener("click", () => {
  document.querySelectorAll("[data-rail]").forEach((item) => item.classList.toggle("is-active", item === button));
  const rail = button.dataset.rail;
  if (rail === "diagnostics") {
    document.querySelector('[data-bottom="diagnostics"]')?.click();
  } else if (rail === "scene") {
    setMainView("scene");
    document.querySelector("#openSceneButton")?.focus();
  } else if (rail === "assets" || rail === "project") {
    document.querySelector("#assetSearch")?.focus();
  } else if (rail === "settings") {
    showToast("Settings", state.projectConfig.ready
      ? `Project: ${state.projectConfig.root || state.projectConfig.name}`
      : "Open a project first", "info");
  }
}));

document.querySelector("#launcherOpenButton")?.addEventListener("click", () => {
  console.info("[yuyib] click Open project folder");
  beginPendingProjectAction("Open project", "Opening folder picker…");
  post("project.openInteractive", {});
});

document.querySelector("#launcherOpenPathButton")?.addEventListener("click", () => {
  const path = document.querySelector("#launcherOpenPath").value.trim();
  console.info("[yuyib] click Open path", path);
  if (!path) {
    setLauncherStatus("Paste a folder path that contains project.yuyib.", "error");
    return;
  }
  beginPendingProjectAction("Open path", `Opening ${path}…`, 15000);
  try {
    post("project.open", { path });
  } catch (error) {
    clearPendingProjectAction();
    setLauncherStatus(`Bridge post failed: ${error}`, "error");
  }
});

document.querySelector("#launcherOpenPath")?.addEventListener("keydown", (event) => {
  if (event.key === "Enter") document.querySelector("#launcherOpenPathButton")?.click();
});

document.querySelector("#launcherCreateButton")?.addEventListener("click", () => {
  const name = document.querySelector("#launcherProjectName").value.trim();
  const profile = document.querySelector("#launcherProfile").value;
  if (!name) {
    setLauncherStatus("Enter a project name first.", "error");
    return;
  }
  beginPendingProjectAction("Create project", "Opening folder picker for the parent directory…");
  try {
    post("project.createInteractive", { name, profile });
  } catch (error) {
    clearPendingProjectAction();
    setLauncherStatus(`Bridge post failed: ${error}`, "error");
  }
});

document.querySelector("#launcherProjectName")?.addEventListener("keydown", (event) => {
  if (event.key === "Enter") document.querySelector("#launcherCreateButton")?.click();
});

function setLauncherWizardStep(step) {
  document.querySelector("#launcherPaneChoose")?.classList.toggle("is-hidden", step !== "choose");
  document.querySelector("#launcherPaneCreate")?.classList.toggle("is-hidden", step !== "create");
  document.querySelector("#launcherPaneOpen")?.classList.toggle("is-hidden", step !== "open");
  document.querySelectorAll("[data-launcher-step]").forEach((button) => {
    const key = button.dataset.launcherStep;
    button.classList.toggle("is-active", key === step);
    button.disabled = key !== "choose" && key !== step;
  });
  const subtitle = document.querySelector("#launcherSubtitle");
  if (subtitle) {
    subtitle.textContent = step === "create"
      ? "Name the project, pick a profile, then choose a parent folder"
      : step === "open"
        ? "Open a folder that already contains project.yuyib"
        : "Open an existing project or create a new one";
  }
}

document.querySelector("#launcherChooseCreate")?.addEventListener("click", () => {
  setLauncherWizardStep("create");
  document.querySelector("#launcherProjectName")?.focus();
});
document.querySelector("#launcherChooseOpen")?.addEventListener("click", () => {
  setLauncherWizardStep("open");
});
document.querySelector("#launcherBackFromCreate")?.addEventListener("click", () => {
  setLauncherWizardStep("choose");
});
document.querySelector("#launcherBackFromOpen")?.addEventListener("click", () => {
  setLauncherWizardStep("choose");
});
document.querySelectorAll("[data-launcher-step='choose']").forEach((button) => {
  button.addEventListener("click", () => setLauncherWizardStep("choose"));
});
setLauncherWizardStep("choose");

document.querySelectorAll("[data-open-source]").forEach((button) => button.addEventListener("click", () => {
  let systemsPayload = null;
  try {
    systemsPayload = button.dataset.systems ? JSON.parse(button.dataset.systems) : null;
  } catch {
    systemsPayload = null;
  }
  openCoverageSource(button.dataset.path, systemsPayload);
}));

document.querySelectorAll("[data-source]").forEach((button) => button.addEventListener("click", () => {
  document.querySelectorAll("[data-source]").forEach((item) => item.classList.toggle("is-selected", item === button));
  post("source.read", { path: sourceDocuments[button.dataset.source].path });
}));

document.querySelector("#playButton").addEventListener("click", () => {
  if (!state.projectConfig.ready) {
    showToast("No project", "Open or create a project before Play", "warning");
    return;
  }
  post("play.start", {});
});
document.querySelector("#pauseButton").addEventListener("click", () => {
  showToast("Pause unavailable", "Pinned Play v1 supports Start/Stop only — Apply/Pause are out of scope", "info");
});
document.querySelector("#stopButton").addEventListener("click", () => post("play.stop", {}));
document.querySelector("#applyPlayButton")?.addEventListener("click", () => {
  post("play.apply_changes", {});
});
document.querySelector("#syncCodeButton")?.addEventListener("click", () => {
  post("scene.projection.export", {});
});
document.querySelector("#applyCodeButton")?.addEventListener("click", () => {
  post("scene.projection.apply", {
    expected_revision: state.scene.revision ?? undefined,
  });
});
document.querySelector("#saveButton").addEventListener("click", () => {
  if (state.scene.document && state.scene.dirty) post("scene.save", {});
  else if (state.scene.document) showToast("Scene already saved", state.scene.path || "No pending scene changes", "info");
  else showToast("No scene open", "Open or create a .yscene document first", "warning");
});
document.querySelector("#buildButton").addEventListener("click", runScopedCargoCheck);
document.querySelector("#cookButton")?.addEventListener("click", runProjectCook);
document.querySelector("#exportYpackButton")?.addEventListener("click", runYpackExport);
document.querySelector("#importYpackButton")?.addEventListener("click", runYpackImport);
document.querySelector("#runCheckButton").addEventListener("click", runScopedCargoCheck);
function resolveSelectedGltfAssetId() {
  const previewPath = state.assetPreview?.path;
  if (previewPath && /\.(glb|gltf)$/i.test(previewPath)) return previewPath;
  const previewId = state.assetPreview?.id;
  if (previewId && (String(previewId).startsWith("asset://") || /\.(glb|gltf)$/i.test(String(previewId)))) {
    return previewId;
  }
  const selected = state.selection?.stableId || state.selection?.id;
  if (selected) {
    const asAsset = state.assets.find(
      (asset) => asset.id === selected
        || asset.path === selected
        || `asset://${asset.path}` === selected
        || (asset.id && String(asset.id).replace(/^asset:\/\//, "") === String(selected).replace(/^asset:\/\//, "")),
    );
    if (asAsset && (asAsset.kind === "model" || /\.(glb|gltf)$/i.test(asAsset.path || ""))) {
      return asAsset.path || asAsset.id;
    }
    if (String(selected).startsWith("asset://") || /\.(glb|gltf)$/i.test(String(selected))) {
      return selected;
    }
  }
  return null;
}

document.querySelector("#assetsRefreshButton")?.addEventListener("click", () => post("assets.refresh", {}));
document.querySelector("#assetsTrackButton")?.addEventListener("click", () => {
  const assetId = resolveSelectedGltfAssetId();
  if (!assetId) {
    showToast("Select a glTF asset", "Click a .glb/.gltf card in Assets (not a scene entity)", "warning");
    return;
  }
  post("asset.track", { id: assetId });
});
document.querySelector("#assetsRenameButton")?.addEventListener("click", () => {
  const assetId = resolveSelectedGltfAssetId();
  if (!assetId) {
    showToast("Select a tracked glTF", "Track a .glb first (T), then rename", "warning");
    return;
  }
  const current = state.assets.find((asset) => asset.id === assetId || asset.path === assetId
    || (asset.id && String(asset.id).replace(/^asset:\/\//, "") === String(assetId).replace(/^asset:\/\//, "")));
  const suggestion = current?.path?.replace(/(\.glb|\.gltf)$/i, "_v2$1") || "models/renamed.glb";
  const to = window.prompt("New asset-root-relative glTF path", suggestion)?.trim();
  if (!to) return;
  post("asset.rename", { id: assetId, to });
});

document.querySelector("#assetsMigrateModelRefsButton")?.addEventListener("click", () => {
  if (!state.projectConfig?.scenes?.length && !state.project) {
    showToast("No project", "Open a project before migrating model refs", "warning");
    return;
  }
  showToast("Migrating model refs", "Dry-run first…", "info");
  post("assets.migrate_scene_model_refs", { dry_run: true });
});

document.querySelector("#reimportButton").addEventListener("click", () => {
  const assetId = resolveSelectedGltfAssetId();
  if (!assetId) {
    showToast("Select a glTF asset", "Click a .glb/.gltf in the Assets panel first", "warning");
    return;
  }
  setMainView("preview");
  if (hosted) {
    post("asset.reimport", { id: assetId });
    return;
  }
  executeCommand("asset.reimport", { asset_guid: assetId, settings_schema: "yuyib.gltf-import-settings@1", non_destructive: true });
});

document.querySelector("#placeAssetInSceneButton")?.addEventListener("click", () => {
  placeSelectedAssetInScene();
});

document.querySelector("#openSceneButton").addEventListener("click", () => {
  const scenes = state.projectConfig.scenes || [];
  if (scenes.length === 1) {
    post("scene.open", { path: scenes[0].path });
    return;
  }
  if (scenes.length > 1) {
    const listing = scenes.map((scene, index) => `${index + 1}. ${scene.name || scene.path} — ${scene.path}`).join("\n");
    const choice = window.prompt(`Open scene (number or path):\n${listing}`, scenes[0].path)?.trim();
    if (!choice) return;
    const byIndex = scenes[Number(choice) - 1];
    const byPath = scenes.find((scene) => scene.path === choice || scene.name === choice);
    const path = byIndex?.path || byPath?.path || choice;
    post("scene.open", { path });
    return;
  }
  const path = window.prompt("Scene path", hosted ? "scenes/main.yscene" : "district_01.yscene")?.trim();
  if (path) post("scene.open", { path });
});

document.querySelector("#createSceneButton").addEventListener("click", () => {
  const path = window.prompt("New scene path", hosted ? "scenes/untitled.yscene" : "untitled.yscene")?.trim();
  if (path) post("scene.create", { path, scene_guid: crypto.randomUUID() });
});

document.querySelector("#createEntityButton")?.addEventListener("click", () => {
  const name = window.prompt("Entity name", "New Entity")?.trim();
  if (!name) return;
  sendSceneCommand({ type: "entity.create", name, with_transform3d: true });
});

document.querySelector("#createPlayerButton")?.addEventListener("click", () => {
  if (!state.scene.document) {
    showToast("No scene open", "Open or create a .yscene before spawning a player", "warning");
    return;
  }
  sendSceneCommand({ type: "entity.create", name: "Player", with_transform3d: true });
  window.setTimeout(() => {
    const entities = state.scene.document?.entities || [];
    const entity = [...entities].reverse().find((item) => item.name === "Player") || entities.at(-1);
    if (!entity) return;
    const hasModel = (entity.components || []).some((component) => (component.schema || component.id) === "yuyib.model3d");
    const finish = () => {
      sendSceneCommand({
        type: "component.field.set",
        entity_guid: entity.guid,
        component_id: "yuyib.model3d",
        field_path: "model",
        value: "builtin:cube",
      });
      sendSceneCommand({
        type: "component.field.set",
        entity_guid: entity.guid,
        component_id: "yuyib.transform3d",
        field_path: "translation.y",
        value: 1.0,
      });
      requestSelection("entity", entity.guid);
      showToast("Player spawned", "WASD in Play Mode · camera framed on Player", "success");
    };
    if (!hasModel) {
      sendSceneCommand({ type: "component.add", entity_guid: entity.guid, component_id: "yuyib.model3d" });
      window.setTimeout(finish, 80);
    } else {
      finish();
    }
  }, 120);
});

document.querySelector("#addComponentButton")?.addEventListener("click", () => {
  openAddComponentDialog();
});

document.querySelectorAll("[data-close-component-dialog]").forEach((button) => {
  button.addEventListener("click", closeAddComponentDialog);
});

document.querySelectorAll("[data-close-revision-conflict]").forEach((button) => {
  button.addEventListener("click", closeRevisionConflictDialog);
});
document.querySelector("#revisionConflictReload")?.addEventListener("click", reloadRevisionConflict);

document.querySelector("#deleteEntityButton")?.addEventListener("click", deleteSelectedEntity);

document.querySelector("#copyEntityGuid").addEventListener("click", async () => {
  const guid = selectedSceneEntity()?.guid;
  if (!guid) return;
  try {
    await navigator.clipboard.writeText(guid);
    showToast("Entity GUID copied", guid, "success");
  } catch {
    showToast("Clipboard unavailable", guid, "warning");
  }
});

document.querySelector("#assetSearch").addEventListener("input", ({ target }) => {
  const query = target.value.trim().toLowerCase();
  document.querySelectorAll(".asset-card").forEach((card) => {
    card.hidden = query && !card.querySelector(".asset-name").textContent.toLowerCase().includes(query);
  });
});

document.querySelector("#clearDiagnostics").addEventListener("click", () => {
  document.querySelector("#diagnosticsPanel").replaceChildren();
  state.diagnosticsCopyBuffer = "";
  document.querySelectorAll(".count-badge, .rail-badge").forEach((badge) => { badge.textContent = "0"; });
});

document.querySelector("#copyDiagnostics")?.addEventListener("click", async () => {
  const panel = document.querySelector("#diagnosticsPanel");
  const texts = Array.from(panel.querySelectorAll(".diagnostic-row"))
    .map((row) => row.dataset.copyText || row.innerText.trim())
    .filter(Boolean);
  const payload = texts.join("\n") || state.diagnosticsCopyBuffer;
  if (!payload) {
    showToast("Nothing to copy", "Diagnostics panel is empty", "info");
    return;
  }
  try {
    await navigator.clipboard.writeText(payload);
    showToast("Diagnostics copied", `${texts.length || 1} line(s)`, "info");
  } catch {
    showToast("Copy failed", "Clipboard is unavailable", "warning");
  }
});

document.addEventListener("keydown", (event) => {
  if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "s") {
    event.preventDefault();
    if (state.scene.document && state.scene.dirty) {
      post("scene.save", {});
    } else if (state.view === "code" && state.monacoEditor) {
      if (state.sourcePath) post("source.save", { path: state.sourcePath, content: state.monacoEditor.getValue(), revision: state.sourceRevision });
      else showToast("No source file open", "Wait for host.source before saving", "warning");
    }
  }
  if (event.key === "Escape" && !document.querySelector("#revisionConflictDialog").hidden) {
    closeRevisionConflictDialog();
  } else if (event.key === "Escape" && !document.querySelector("#addComponentDialog").hidden) {
    closeAddComponentDialog();
  }
  if ((event.ctrlKey || event.metaKey) && event.key === "Enter") {
    event.preventDefault();
    runScopedCargoCheck();
  }
  if (
    !event.ctrlKey
    && !event.metaKey
    && !event.altKey
    && !event.repeat
    && !(event.target instanceof Element && event.target.matches("input, textarea, select, [contenteditable='true']"))
  ) {
    const toolForKey = { w: "move", e: "rotate", r: "scale", t: "select" }[event.key.toLowerCase()];
    if (toolForKey) {
      event.preventDefault();
      setViewportTool(toolForKey);
      return;
    }
  }
  if (
    event.key === "Delete"
    && !event.ctrlKey
    && !event.metaKey
    && !event.altKey
    && !(event.target instanceof Element && event.target.matches("input, textarea, select, [contenteditable='true']"))
  ) {
    const entity = selectedSceneEntity();
    if (entity && !state.scene.readOnly) {
      event.preventDefault();
      deleteSelectedEntity();
    }
  }
});

new ResizeObserver(() => {
  if (state.view === "scene") window.requestAnimationFrame(drawScene);
  window.requestAnimationFrame(sendViewportBounds);
}).observe(document.querySelector(".viewport-canvas-wrap"));
const previewStage = document.querySelector(".preview-stage");
if (previewStage) {
  new ResizeObserver(() => window.requestAnimationFrame(sendViewportBounds)).observe(previewStage);
}
window.addEventListener("resize", () => window.requestAnimationFrame(sendViewportBounds));

function initializeUiMode() {
  document.querySelector("#playButton").disabled = true;
  document.querySelector("#buildButton").disabled = true;
  const cookButton = document.querySelector("#cookButton");
  if (cookButton) cookButton.disabled = true;
  const exportYpackButton = document.querySelector("#exportYpackButton");
  if (exportYpackButton) exportYpackButton.disabled = true;
  const importYpackButton = document.querySelector("#importYpackButton");
  if (importYpackButton) importYpackButton.disabled = true;
  document.querySelector("#runCheckButton").disabled = true;
  document.querySelector('[data-command="history.undo"]').disabled = true;
  document.querySelector('[data-command="history.redo"]').disabled = true;
  const deleteButton = document.querySelector("#deleteEntityButton");
  if (deleteButton) deleteButton.disabled = true;
  if (!hosted) {
    setLauncherVisible(false);
    return;
  }

  document.body.classList.add("hosted-mode");
  document.querySelector("#pauseButton").disabled = true;
  document.querySelector("#pauseButton").hidden = true;
  document.querySelector("#pauseButton").title = "Pause is not part of pinned Play v1 (Start/Stop only)";
  document.querySelectorAll("[data-overlay]").forEach((input) => {
    const overlay = input.dataset.overlay;
    if (overlay === "bounds" || overlay === "collision" || overlay === "normals" || overlay === "tangents" || overlay === "uv") {
      input.disabled = false;
      input.checked = Boolean(state.overlays[overlay]);
      const label = input.closest("label");
      if (label) {
        label.title = {
          bounds: "Toggle AABB wireframe in Asset Preview",
          collision: "Toggle collision mesh wireframe in Asset Preview",
          normals: "Toggle vertex normal shafts in Asset Preview",
          tangents: "Toggle vertex tangent shafts in Asset Preview",
          uv: "Toggle UV0 vertex markers (u→R, v→G) in Asset Preview",
        }[overlay] || "";
        label.classList.remove("is-unavailable");
      }
      return;
    }
    input.disabled = true;
    const label = input.closest("label");
    if (label) {
      label.title = "This viewport overlay is not wired to the native host yet";
      label.classList.add("is-unavailable");
    }
  });
  // Push default overlay state so host matches both Scene and Preview toolbars.
  post("preview.overlay.set", { overlay: "bounds", enabled: Boolean(state.overlays.bounds) });
  post("preview.overlay.set", { overlay: "collision", enabled: Boolean(state.overlays.collision) });
  post("preview.overlay.set", { overlay: "normals", enabled: Boolean(state.overlays.normals) });
  post("preview.overlay.set", { overlay: "tangents", enabled: Boolean(state.overlays.tangents) });
  post("preview.overlay.set", { overlay: "uv", enabled: Boolean(state.overlays.uv) });
  document.querySelector('[data-view="preview"]').disabled = false;
  document.querySelector('[data-view="preview"]').title = "Asset Preview — open a .glb/.gltf from Assets";
  document.querySelector("#reimportButton").disabled = false;
  document.querySelector("#reimportButton").title = "Reimport selected glTF through the production importer";
  document.querySelector(".show-all-button").hidden = true;
  renderAssetIndex([]);
  setLauncherVisible(true);

  // Purge browser-mock chrome that ships in index.html (neon_sign / district_01 / cyberpunk).
  const projectCrumb = document.querySelector(".project-breadcrumb");
  if (projectCrumb) {
    projectCrumb.replaceChildren();
    const root = document.createElement("button");
    root.textContent = "Assets";
    const sep = document.createElement("span");
    sep.textContent = "/";
    const leaf = document.createElement("button");
    leaf.textContent = "—";
    projectCrumb.append(root, sep, leaf);
  }

  const codeExplorer = document.querySelector(".code-explorer");
  codeExplorer.querySelectorAll(".code-tree-row, .code-tree-group, .code-outline-heading, .outline-row").forEach((element) => element.remove());
  codeExplorer.querySelector(".code-workspace-name").textContent = "HOST WORKSPACE";
  const codeTab = document.querySelector(".code-file-tab");
  if (codeTab) {
    codeTab.querySelector(".code-dirty-dot")?.remove();
    if (codeTab.childNodes[1]) codeTab.childNodes[1].textContent = "No source";
  }
  const hostedCrumbs = document.querySelectorAll(".code-breadcrumb span");
  if (hostedCrumbs[0]) hostedCrumbs[0].textContent = "project";
  if (hostedCrumbs[1]) hostedCrumbs[1].textContent = "";
  if (hostedCrumbs[2]) hostedCrumbs[2].textContent = "No source";
  const breadcrumbSymbol = document.querySelector(".code-breadcrumb strong");
  if (breadcrumbSymbol) breadcrumbSymbol.textContent = "—";
  // Host fills Explorer via host.source.tree after project open / Code tab.

  const previewSummary = document.querySelector(".preview-summary");
  if (previewSummary) {
    const kicker = previewSummary.querySelector(".summary-kicker");
    if (kicker) kicker.textContent = "No asset selected";
    const heading = previewSummary.querySelector("h3");
    if (heading) heading.textContent = "—";
    const blurb = previewSummary.querySelector("p");
    if (blurb) blurb.textContent = "Open a .glb/.gltf from Assets to inspect import metadata.";
  }

  const diagnostics = document.querySelector("#diagnosticsPanel");
  diagnostics.replaceChildren();
  state.diagnosticsCopyBuffer = "";
  const diagnosticsEmpty = document.createElement("div");
  diagnosticsEmpty.className = "data-empty-state";
  diagnosticsEmpty.textContent = "Waiting for host.diagnostics";
  diagnostics.append(diagnosticsEmpty);
  document.querySelectorAll(".count-badge, .rail-badge").forEach((badge) => { badge.textContent = "0"; });
  document.querySelector("#outputLog").textContent = "[bridge] Waiting for host events";
  document.querySelectorAll(".viewport-stats span").forEach((label, index) => {
    label.textContent = ["Authoring", "Document", "Foundation viewport"][index];
  });
  document.querySelectorAll(".resource-stat, .resource-meter").forEach((element) => { element.hidden = true; });
  const lspDot = document.querySelector("#rustAnalyzerState i");
  document.querySelector("#rustAnalyzerState").lastChild.textContent = "LSP unavailable";
  lspDot.style.background = "var(--muted-2)";
  lspDot.style.boxShadow = "none";
}

initializeUiMode();
initializeViewportPointerRelay();

console.info("[yuyib] boot", {
  hosted,
  hasYuyib: Boolean(window.yuyib),
  pageSession: window.yuyib?.pageSession || null,
});
post("ui.ready", {});
if (!hosted) post("scene.open", { path: "district_01.yscene" });
else window.requestAnimationFrame(sendViewportBounds);

window.requestAnimationFrame(drawScene);
