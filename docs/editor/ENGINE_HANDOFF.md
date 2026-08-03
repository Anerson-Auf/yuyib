# Editor / engine status

Фактическое состояние authoring boundary и связанных engine slices.
Нормативный contract — [`ENGINE_INTEGRATION.md`](ENGINE_INTEGRATION.md);
**SoT** — [`SOURCE_OF_TRUTH.md`](SOURCE_OF_TRUTH.md);
форматы — [`SCENE_FORMAT.md`](SCENE_FORMAT.md); policy coverage —
[`CAPABILITY_COVERAGE.md`](CAPABILITY_COVERAGE.md).
**Как пользоваться:** [`AUTHORING_GUIDE.md`](AUTHORING_GUIDE.md).
**3D checklist:** [`GAME_LOOP_3D.md`](GAME_LOOP_3D.md).

Актуально на 2026-08-03.

## Dependency direction

```text
runtime capability crates
        ^
        | production APIs / types
        |
optional colocated / companion authoring adapters
        |
        v
yuyib-authoring          (UI/renderer/ECS-neutral contracts)
        ^
        |
yuyib-authoring-yuyib    (bootstrap aggregator)
        ^
        |
yuyib-editor-core        (documents / process / project; headless)
        ^
        |
yuyib-editor + editor-ui (native consumer)
```

Runtime, shipping и headless crates не зависят от Editor, WebView или Monaco.
`yuyib-authoring-yuyib` агрегирует registration; typed adapters живут рядом с
owning capability или в companion crate.

## Карта crates

| Область | Владелец | Роль |
|---|---|---|
| Stable IDs / schema versions | `yuyib-authoring` (`identity`) | GUID/ID types; `AssetGuid` ≠ content hash |
| Scene envelope | `yuyib-authoring` (`scene`) | `yuyib.scene@1`, opaque round-trip |
| Commands | `yuyib-authoring` (`command`) | Atomic transactions, undo/redo |
| Migrations | `yuyib-authoring` (`migration`) | Adjacent version chains с bounds |
| Coverage / systems | `descriptor` / `registry` / `system` | Manifest, hard errors на duplicates |
| Preview contract | `yuyib-authoring` (`preview`) | Bounded job, diagnostics, cache policy |
| Bootstrap records | `yuyib-authoring-yuyib` | Capabilities, schemas, systems |
| Project / assets | `yuyib-editor-core` (`project`) | Profiles, scene refs, asset metadata |
| Safe file I/O | `yuyib-editor-core` (`document`) | Confinement, revisions, atomic replace |
| Play / Cargo | `yuyib-editor-core` (`process`) | Process groups, bounded logs, timeouts |
| Scene session | `yuyib-editor` (`scene_authoring`) | Open/create/save, undo/redo |
| Scene ↔ `.rs` projection | `yuyib-scene-projection` | Export/parse/diff view over `.yscene` |
| Script ↔ object Intent Bridge | `yuyib-scene-interaction` | `SceneInteractionIntent` + Editor/Play adapters |
| Host bridge | `yuyib-editor` (`bridge`, `app`) | Typed endpoints, WGPU viewport |
| Scoped viewport | `yuyib-render` | DPI-safe rect, private viewport depth |
| UI shell | `editor-ui/` | Hierarchy, Inspector, Monaco 0.55.1 |

## Работает сейчас

- Native Editor window, transparent WebView hole и WGPU viewport.
- Empty startup: cwd не считается открытым проектом; нужен `project.yuyib`.
- `.yscene` open/create/save, hierarchy, selection, Inspector.
- Viewport LMB picking (RMB orbit, wheel zoom).
- Entity create/rename/delete, dirty state, atomic save, undo/redo.
- Opaque preservation unknown components / extensions; future schema → read-only.
- Coverage manifest и source/system metadata в UI.
- Monaco: models, syntax, folding, minimap, revision-aware open/save.
- Scoped Cargo check и process-isolated Play (`yuyib-play`) с dual pin
  (`--scene-revision` + `--scene-file-revision` blake3).
- Visual: `Transform3d`, `LocalTransform3d`, `Parent3d`, `Model3d`,
  `DirectionalLight3d` — Inspector + materialization + viewport/Play.
- Transform gizmo (Move / Rotate / Scale).
- Scene ↔ Rust projection (vertical #1): `.yscene` remains persistence SoT;
  human-editable `src/scenes/<slug>/entities/*.rs` via Sync Code / Apply Code /
  file watch; one undoable `CommandTransaction` + viewport rematerialize.
- Scene interaction Intent Bridge (foundation + engine wiring): scripts talk to
  objects by `EntityGuid`. Neutral crate: intents, capabilities, batch result,
  quest/trigger signal parse helpers. Editor → undoable commands. Play →
  `PlayInteractionHost` (GUID map, pending queue, frame drain) + `QuestBook`
  consumer + typed `Model3d` / `DirectionalLight3d` field writes + **E / game.use**
  Interactable raycast (materialize `yuyib.interactable` → sphere query →
  `EmitSignal` → QuestBook). Authoring `yuyib.trigger` → sphere volumes via
  `overlap_spheres_3d` → `trigger.*` intents. `TriggerOverlapTracker` maps Rapier
  sensor pairs for hosts that compose Rapier beside CharacterController — no
  physics-mode switch. Persist Play→`.yscene` remains Apply whitelist. Play
  `AddComponent` covers transform / local-transform / light / **model3d (proxy)** /
  **parent3d (GUID resolve)**. Live Rapier overlay: opt-in
  `yuyib-play --features physics-rapier` (`DynamicsOverlay3d` side-by-side;
  mesh motor unchanged; authored triggers → sensors when active). Next: shadow
  intents (non-goal for Intent Bridge) / cooked-only binary strip.
- Asset (incremental): `yuyib.gltf-import` / `yuyib.gltf-preview` через
  production `GltfSceneLoad`; Bounds/Collision/Normals/Tangents/UV overlays; mesh + material
  + animation clip selection; preview cache invalidation; non-destructive reimport;
  **host multi-entry** `HostGltfPreviewStore` (park CPU-ready on A→B, restore A→B→A as
  `host.process` stage `cache_hit`; one GPU-resident session).
- Session/disk cook: same-root reopen keeps import/GPU residency; editor glTF import uses
  `.yuyib_cook`; **`project.cook`** batch-cooks indexed glTF/GLB into the same cache
  (toolbar Cook assets / menu Cook; progress via `host.process` kind `cook`);
  **`project.export_ypack`** packs `.yuyib_cook` → `build/<project>.ypack`;
  **`project.import_ypack`** hydrates pack → `.yuyib_cook` (cook-hit path;
  `host.process` kind `ypack`, `op` export|import);
  **Asset Preview** loads via `GltfSceneLoadConfig::with_cook_cache` and reports
  `cook_hit` / `cache: cook_hit` on `host.process` kind `preview`.
- Play slice: Player motor, mesh collider, authored light, dark PBR fallback;
  glTF hierarchy attach uses the same `.yuyib_cook` as Editor (`cook hit`/`miss` log).
- Scene и Asset Preview на отдельных `Game3dScene` (изоляция GPU residency).

## Открыто

| Тема | Статус |
|---|---|
| glTF preview remainder | material factor + texture remap closed (base/MR/emissive/normal slots from model inventory) |
| Apply Play Mode Changes | closed for Transform3d + LocalTransform3d whitelist (`play.apply_changes` + undoable transaction) |
| Scene ↔ `.rs` projection | vertical #1 closed (known 3D schemas + watch); freeform Rust / entity create-delete from files / behavior scripts — open |
| Script ↔ object Intent Bridge | playable MVP closed incl. Play AddComponent model/parent + Rapier overlay opt-in; deferred: shadow intents |
| rust-analyzer / LSP | diagnostics + completion + hover + **signatureHelp** + definition + references + rename + code actions + allowlisted `executeCommand` closed (`host.lsp.*` → Monaco; `rust-analyzer.*` only) |
| Coverage CI (GitHub Actions) | foundation gate + `editor-coverage-manifest` artifact; registry gate covers Asset/Visual/docs/source/migration/import-settings/system (**incremental** vs full wishlist) |
| Field mutation без typed adapter | closed thin (host `read_only` + `read_only_reason`; Inspector tip/notice) |
| System/source navigation | closed incremental: coverage systems list + open runtime/authoring/`source.file` (workspace-ancestor resolve, read-only external) |
| Project creation wizard / cook export | wizard UX + cook + export/import ypack (hydrate) + Preview/Play cook-hit evidence closed; cooked-only binary without importers open |
| Full multi-entry host preview artifact store | closed thin (`HostGltfPreviewStore` + A→B→A CPU restore / `cache_hit`; one GPU session) |

Metadata и UI control сами по себе не дают статус `Visual` / `Asset` /
`Runtime`. Центральный rotating cube в viewport — layout smoke, не asset preview.

## Invariants

1. Editor — consumer runtime contracts, не второй runtime.
2. Persisted identity — GUID / stable ID (не `Entity`, `TypeId`, type name,
   GPU handle, `file:line`).
3. `AssetGuid` переживает move/rename; content hash — только invalidation.
4. Unknown / newer records сохраняются opaque.
5. Breaking persisted change → новая schema version + executable migration.
6. Authored mutation — только через command transaction с base revision.
7. Play World отделён от authored document; reverse sync — explicit whitelist.
8. Preview = production importer / cooker / renderer. Editor-only decoder запрещён.
9. Duplicate stable IDs — hard error; отсутствие integration — `Unavailable`.
10. Dynamic Rust DLL ABI не вводится; changes — managed Cargo + runner restart.

Scoped viewport: viewport/scissor + отдельный depth attachment. Color
`LoadOp::Clear` — attachment-wide; full-surface clear делает shell, scoped
passes используют `Load`.

## Recipe: новая capability

1. Классифицировать state: `authored` / `derived` / `runtime-only` / `code-only`.
2. Назначить stable IDs (`CapabilityId`, schema + version, system IDs).
3. Adapter рядом с owning crate; не добавлять global `match` в `yuyib-editor`.
4. Persisted change — migration до registration.
5. Asset — `PreviewAdapter` на production path (cancellable, budgets, diagnostics).
6. Зарегистрировать source / plugin / schedule / read-write component IDs.
7. Поднять coverage surface только с evidence (tests + materialization / preview).

Подробный checklist — в [`ENGINE_INTEGRATION.md`](ENGINE_INTEGRATION.md).

## Engine track (кратко)

| Milestone | Состояние |
|---|---|
| M1 playable 3D (street-city smoke) | Closed |
| M2 rendering baseline (IBL, shadows, bloom, FXAA, SSAO, grade, diagnostics) | Usable MVP |
| M3.1 / M3.2 cook cache (glTF + external dep fingerprints) | Usable MVP |
| M4 physics facade (mature backend) | M4.1–M4.13 usable MVP + `PlatformerController2d` (Rapier KCC); open: editor physics polish |
| M5 high-level profiles | 3D M5.2 closed; Deep 2D A–I + M7; M6 UI + Dialogue HL (Rust graph) |
| M6 native UI completion | Early partial (ScrollView drag + image extract); IME/a11y open |
| Editor E1 remainder | Thin deferred: broader `executeCommand` beyond `rust-analyzer.*`; materializer/command registry gates; FS dangling docs |

Порядок и Definition of Done — [`ROADMAP.md`](../architecture/ROADMAP.md).
Открытые публичные дефекты — [`KNOWN_ISSUES.md`](../../KNOWN_ISSUES.md).

## `project.yuyib` development block

Опциональный fragment для native host:

```json
{
  "development": {
    "cargo_package": "my-game",
    "play_executable": "target/debug/my-game.exe",
    "play_arguments": ["--scene", "scenes/main.yscene"]
  }
}
```

Package — restricted identifier grammar; executable confined project root;
arguments передаются буквально (не shell).

Editor Play runner from the engine repo:

```text
cargo build -p yuyib-play
cargo build -p yuyib-play --features physics-rapier   # props overlay + trigger sensors
```

## Verification

Scoped checks для authoring / editor foundation:

```text
CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=2
cargo test -p yuyib-authoring -p yuyib-authoring-yuyib \
  -p yuyib-editor-core -p yuyib-render --lib -- --test-threads=2

CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=2
cargo test -p yuyib-editor --bin yuyib-editor -- --test-threads=2

node --check editor-ui/src/main.js
npm run build   # в editor-ui/

CARGO_BUILD_JOBS=2 cargo check -p yuyib-editor
```

Интерактивное окно, full workspace и `xtask` в этот список не входят.
Матрица контрактов — [`TESTING.md`](TESTING.md).
