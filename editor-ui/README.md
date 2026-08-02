# Yuyib Editor UI

Standalone desktop authoring shell for the native `yuyib-editor` host. The UI
is an ordinary local Vite bundle: it has no CDN, runtime Node dependency, or
remote network requirement. `monaco-editor` and its workers are emitted into
`dist/` at build time.

The page also runs in a normal browser. When `window.yuyib.post` is absent,
`src/main.js` installs a deterministic mock host which emits the same events as
the native shell.

## Local commands

```text
npm install
npm run dev
npm run build
```

The Rust host should package the complete `dist/` directory and serve
`dist/index.html` as the local page entry. Vite is configured with a relative
base so the bundle does not depend on a specific custom-protocol hostname.

## Bridge protocol v1

Every page-to-host request uses the existing Yuyib envelope:

```json
{
  "version": 1,
  "id": 41,
  "endpoint": "selection.set",
  "payload": { "id": "entity://neon-sign-07" }
}
```

The native bridge adds its page session. The page never sends filesystem or
process requests outside these typed endpoints:

| Endpoint | Payload |
| --- | --- |
| `ui.ready` | `{}` |
| `workspace.mode` | `{ mode: "scene" | "code" }` |
| `viewport.bounds` | `{ x, y, width, height }` in logical document client pixels |
| `selection.set` | `{ id? }` |
| `play.start` | `{ executable, args? }` |
| `play.stop` | `{}` |
| `cargo.check` | `{ package }` |
| `source.open` | `{ path }` |
| `source.read` | `{ path }` |
| `source.save` | `{ path, content, revision? }` |
| `scene.open` | `{ path }` |
| `scene.create` | `{ path, scene_guid? }` |
| `scene.save` | `{}`; the host saves its currently loaded revision |
| `scene.command` | `{ base_revision, transaction_id, command }` |

In hosted Scene mode the WebView leaves the central viewport transparent for
the native WGPU surface. `viewport.bounds` is sent after layout changes and
contains `getBoundingClientRect()` coordinates without device-scale
conversion. Code and non-scene modes send `{ x: 0, y: 0, width: 0, height: 0 }`
so the host removes the native viewport. Browser mock mode retains its canvas
preview and does not send this endpoint.

Required `scene.command` variants are:

```json
{
  "base_revision": 12,
  "transaction_id": "scene-tx-7",
  "command": {
    "type": "entity.rename",
    "entity_guid": "2ed36075-22e6-4bc5-8baa-957d6c94f751",
    "name": "Neon Sign"
  }
}
```

```json
{
  "base_revision": 12,
  "transaction_id": "scene-tx-8",
  "command": {
    "type": "component.field.set",
    "entity_guid": "2ed36075-22e6-4bc5-8baa-957d6c94f751",
    "component_id": "yuyib.transform3d",
    "field_path": "translation.x",
    "value": 14.5
  }
}
```

`history.undo` and `history.redo` are optional command types using the same
revision envelope. Native `SceneGuid` and `EntityGuid` values are canonical
UUID strings; the URI-like IDs in the browser fixture are mock-only.

Host-to-page messages are DOM `yuyib:event` events with
`detail = { version: 1, event, payload }`. The UI handles:

- `host.coverage`
- `host.diagnostics`
- `host.source`
- `host.sourceConflict`
- `host.process`
- `host.selection`
- `host.scene.document`
- `host.scene.conflict`
- `host.scene.history`

The scene event payloads are:

```text
host.scene.document {
  path,
  revision,
  document: {
    schema: "yuyib.scene",
    version,
    scene_guid,
    name,
    roots: [entity_guid],
    entities: [{
      guid,
      name,
      parent_guid?,
      children: [entity_guid],
      components: [{ id, schema_version, data }]
    }]
  }
}

host.scene.conflict {
  path,
  expected_revision,
  actual_revision,
  message?
}

host.scene.history {
  revision,
  dirty,
  can_undo,
  can_redo
}
```

The Inspector never infers editable fields from arbitrary JSON. `host.coverage`
provides `components[]` directly or inside a coverage `surface`:

```text
{
  id,
  label,
  status,
  schema_version,
  fields: [{ path, label, kind, group?, step?, min?, max?, options?, read_only? }],
  source?: { component?, adapter?, systems? }
}
```

Supported field kinds are `number`, `boolean`, `string`, `enum`, `asset`, and
`color`. A component without a descriptor is rendered as read-only opaque JSON;
the UI never drops or rewrites it.

Inspector field edits, entity renames, history, open/create/save, and revision
conflicts use the scene contract above. Preview-only controls remain local UI
state and make no runtime materialization claim.

## Code workspace

Monaco is the editor implementation; this project does not implement a custom
text editor. The bundled setup provides Monaco models, undo, selections,
folding, minimap, bracket pairing, automatic indentation, local workers, and a
small Rust language configuration. The native host remains responsible for
safe file access, revision conflicts, LSP integration, and scoped Cargo
execution. A successful Cargo check is reported only as `cargo check · success`;
it never implies that an LSP or `rust-analyzer` session exists.
