# Editor / engine status

Фактическое состояние authoring boundary и связанных engine slices.
Нормативный contract — [`ENGINE_INTEGRATION.md`](ENGINE_INTEGRATION.md);
форматы — [`SCENE_FORMAT.md`](SCENE_FORMAT.md); policy coverage —
[`CAPABILITY_COVERAGE.md`](CAPABILITY_COVERAGE.md).
**Как пользоваться (API + payloads):** [`AUTHORING_GUIDE.md`](AUTHORING_GUIDE.md).

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
  physics-mode switch. Persist Play→`.yscene` remains Apply whitelist. Next:
  Play AddComponent; optional live Rapier world in default Play.
- Asset (incremental): `yuyib.gltf-import` / `yuyib.gltf-preview` через
  production `GltfSceneLoad`; Bounds/Collision/Normals/Tangents/UV overlays; mesh + material
  + animation clip selection; preview cache invalidation; non-destructive reimport.
- Session/disk cook: same-root reopen keeps import/GPU residency; editor glTF import uses
  `.yuyib_cook`.
- Play slice: Player motor, mesh collider, authored light, dark PBR fallback.
- Scene и Asset Preview на отдельных `Game3dScene` (изоляция GPU residency).

## Открыто

| Тема | Статус |
|---|---|
| glTF preview remainder | material factor + texture remap closed (base/MR/emissive/normal slots from model inventory) |
| Apply Play Mode Changes | closed for Transform3d + LocalTransform3d whitelist (`play.apply_changes` + undoable transaction) |
| Scene ↔ `.rs` projection | vertical #1 closed (known 3D schemas + watch); freeform Rust / entity create-delete from files / behavior scripts — open |
| Script ↔ object Intent Bridge | foundation + QuestBook + Play model/light + Interactable use (E) + authored `yuyib.trigger` + Play AddComponent (transform/local/light) + Inspector Interactable/Trigger Visual closed; Play model/parent AddComponent / live Rapier world in default Play — open |
| rust-analyzer / LSP | diagnostics-only closed (`host.lsp.status` / `host.lsp.diagnostics` → Monaco markers); completion/hover/rename open |
| Coverage CI (GitHub Actions) | foundation gate + `editor-coverage-manifest` artifact upload closed (incremental) |
| Field mutation без typed adapter | host блокирует edit |
| System/source navigation | closed incremental: coverage systems list + open runtime/authoring/`source.file` (workspace-ancestor resolve, read-only external) |
| Project creation wizard / cook export | нет |
| Full multi-entry host preview artifact store | thin |

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
| M5 high-level profiles | 3D M5.2 closed; Deep 2D A: PlayableLoop2d + CameraFollow2d |
| M6 native UI completion | Early partial (`ScrollView` + glyph clip + thumb) |
| Editor E1 remainder | Asset overlays / LSP / Actions CI |

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
