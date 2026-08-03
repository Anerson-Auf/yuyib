# Authoring guide: Scene, Play interactions, Intent Bridge

Практический гайд для текущего Editor / Play среза (2026-08).  
**SoT:** [`SOURCE_OF_TRUTH.md`](SOURCE_OF_TRUTH.md).  
Нормативные контракты: [`ENGINE_INTEGRATION.md`](ENGINE_INTEGRATION.md),
[`SCENE_FORMAT.md`](SCENE_FORMAT.md). Статус: [`ENGINE_HANDOFF.md`](ENGINE_HANDOFF.md).
Checklist: [`GAME_LOOP_3D.md`](GAME_LOOP_3D.md).

Цель документа: **показать API и payloads**, чтобы можно было тестировать
функционал без угадывания. Ручная правка `.yscene` не обязательна — Inspector
Add Component уже отдаёт зарегистрированные schemas.

---

## 0. Минимальный Play smoke

1. Открыть проект с `project.yuyib` и сцену `.yscene`.
2. Entity с именем **`Player`** + `yuyib.transform3d` (Play ищет это имя).
3. Play (Editor pin: `--project` + `--scene` + dual revision).
4. Управление по умолчанию: **WASD**, **Space**, **Shift**, мышь, **V** view,
   **E** = `game.use` (Interactable), **Esc** exit.

Без `Player` сцена рендерится, но locomotion / use / trigger probe не активны.

Готовый минимальный проект (Player + TalkNpc + ExitVolume):
[`samples/interaction-smoke/`](samples/interaction-smoke/).
Откройте `project.yuyib` в Editor → Play → подойдите к TalkNpc (**E**) или
войдите в ExitVolume (лог `trigger.level.exit`).

**Observable:** Play пишет в **Diagnostics** с `source=play` (не в mock
«Imported asset» rows). Строки также toast'ятся / идут в Output при
`signal` / `use accepted`. Parent PowerShell Editor **не** показывает Play
stderr (отдельный piped process) — смотрите нижнюю панель Diagnostics.

---

## 1. Components: Interactable и Trigger

Оба schema живут в capability `yuyib.gameplay-interactions`
(`yuyib-authoring-yuyib`). В Inspector: **Add Component** → выбрать из списка
(не свободный ввод имени).

### `yuyib.interactable`

Play materialize → `Interactable` + `Position3d` + `SphereCollider3d` (радиус
query ≈ `0.45`). **E** → `request_use_raycast_3d` → local Accept →
`EmitSignal` с именем interaction id → Intent Bridge / optional `QuestBook`.

| Field | Kind | Meaning |
|---|---|---|
| `interaction` | string (widget) | Semantic id, напр. `world.talk_npc` |
| `enabled` | bool (optional) | default `true` |
| `required_action` | string (optional) | default `game.use` |
| `max_distance` | f32 (optional) | default `3.0` |

Payload example:

```json
{
  "schema": "yuyib.interactable",
  "version": 1,
  "payload": {
    "interaction": "world.talk_npc",
    "enabled": true,
    "max_distance": 3.0
  }
}
```

Проверка: подойти к entity, нажать **E**. В логе Play ожидайте
`interaction flush … signals=…` и signal name = `world.talk_npc`
(не `quest.*` автоматически — см. §3).

### `yuyib.trigger`

Play materialize → gameplay `Trigger` + sphere. После locomotion каждый frame:
player probe (`radius ≈ 0.4`) через `overlap_spheres_3d` → phases
`entered` / `stayed` / `exited` → intents `trigger.<id>`.

| Field | Kind | Meaning |
|---|---|---|
| `trigger` | string | Semantic id, напр. `level.exit` |
| `enabled` | bool (optional) | default `true` |
| `radius` | f32 (optional) | default `1.0` (метры) |

```json
{
  "schema": "yuyib.trigger",
  "version": 1,
  "payload": {
    "trigger": "level.exit",
    "enabled": true,
    "radius": 1.5
  }
}
```

Signal wire shape:

```json
{
  "name": "trigger.level.exit",
  "payload": { "trigger": "level.exit", "phase": "entered" }
}
```

Phases: `entered` | `stayed` | `exited`.

**Не путать** с CharacterController↔Rapier mode switch (non-goal). Default Play
**может** поднять side-by-side [`DynamicsOverlay3d`] через
`cargo build -p yuyib-play --features physics-rapier`: props + soft reaction,
mesh motor неизменен. Без feature — stub. Authored triggers при active overlay
идут через Rapier sensors → `TriggerOverlapTracker`; без feature — sphere-query
path (`overlap_spheres_3d`).

---

### `yuyib.render3d` (nodraw)

| Field | Meaning |
|---|---|
| `draw` | `false` = hide from render; collision unchanged |

### `yuyib.collision3d` (nocollide / selective)

| Field | Meaning |
|---|---|
| `enabled` | `false` = exclude from player locomotion mesh |
| `layer` | optional tag (`door`, `prop`, …) |
| `collide_with` | empty = all; else comma/list — must include `player` to stay in Play mesh |

Prop↔prop selective collision is **not** on the CharacterController trimesh path.

---

## 2. Intent Bridge API (`yuyib-scene-interaction`)

Scripts / tooling **не** пишут `.yscene` и ECS напрямую. Они шлют
`SceneInteractionIntent` по `EntityGuid`.

### Intent kinds (serde tag `type`)

| `type` | Editor | Play (default) |
|---|---|---|
| `set_translation` | yes | yes |
| `set_component_field` | known 3D schemas | transform / model / light |
| `add_component` | known 3D schemas | transform / local-transform / directional-light / model3d (proxy) / parent3d |
| `emit_signal` | yes (host event) | yes (queue → QuestBook if set) |

Known 3D schemas:

- `yuyib.transform3d`
- `yuyib.local-transform3d`
- `yuyib.parent3d`
- `yuyib.model3d`
- `yuyib.directional-light3d`

Discoverability: `editor_capabilities()` / `play_capabilities()`. Unsupported →
**hard error**, не silent no-op.

### Wire JSON → Editor host

Endpoint: `scene.interaction.apply`

```json
{
  "expected_revision": null,
  "intents": [
    {
      "type": "set_translation",
      "entity": "018f0000-0000-7000-8000-000000000002",
      "translation": [1.0, 0.0, 2.0],
      "space": "world"
    },
    {
      "type": "set_component_field",
      "entity": "018f0000-0000-7000-8000-000000000002",
      "schema": "yuyib.model3d",
      "field_path": "visible",
      "value": false
    },
    {
      "type": "emit_signal",
      "name": "quest.intro.talked",
      "payload": { "amount": 1 }
    }
  ]
}
```

`space`: `world` (default) | `local`.

Результат: одна undoable command transaction; signals уходят как
`host.scene.interaction.signal` (+ optional `quest_progress` parse).

### Rust (Play / game host)

```rust
use yuyib_scene_interaction::{
    SceneInteractionIntent, TransformSpace, try_parse_quest_progress_signal,
    try_parse_trigger_signal,
};

// Enqueue (PlayRuntime / custom host):
// runtime.enqueue_interaction(SceneInteractionIntent::SetTranslation { … });

let intent = SceneInteractionIntent::EmitSignal {
    name: "quest.intro.talked".into(),
    payload: serde_json::json!({ "amount": 1 }),
};

// After flush, parse signals:
if let Some(q) = try_parse_quest_progress_signal(&name, &payload) {
    // q.event, q.amount → QuestSignal::new(q.event, q.amount)
}
if let Some(t) = try_parse_trigger_signal(&name, &payload) {
    // t.trigger_id, t.phase (Entered|Stayed|Exited)
}
```

Play typed field writes (SetComponentField):

| Schema | Fields |
|---|---|
| `yuyib.model3d` | `visible`, `render_order`, `mesh` |
| `yuyib.directional-light3d` | `enabled`, `illuminance`, `direction`, `color` |
| transforms | via `set_translation` / field paths on transform schemas |

`AddComponent` в Play: `yuyib.transform3d` / `yuyib.local-transform3d` /
`yuyib.directional-light3d` / `yuyib.model3d` (proxy cube when `model` is null) /
`yuyib.parent3d` (GUID map resolve; `parent: null` = authored root, no ECS edge).
Entity must already be in the GUID map.

Constants: `SCHEMA_*`, `SIGNAL_QUEST_PREFIX` (`quest.`),
`SIGNAL_TRIGGER_PREFIX` (`trigger.`).

Batch result: `SceneInteractionBatchResult { submitted, applied, signals }`.

---

## 3. Quest signals

Crate `yuyib-scene-interaction` **не** зависит от `yuyib-gameplay`. Host сам
мапит parsed progress → `QuestSignal`.

Accepted EmitSignal shapes:

```json
{ "name": "quest.intro.talked", "payload": { "amount": 1 } }
```

```json
{
  "name": "quest.apply",
  "payload": { "event": "intro.talked", "amount": 2 }
}
```

`amount` must be > 0.

Play: `PlayRuntime::set_quest_book(QuestBook)` (API на runtime; UI QuestBook
в Editor пока нет). Interactable signal name = interaction id — чтобы квест
подхватил use, objective event должен совпадать с этим id **или** behavior
должен переиздать `quest.*` signal.

---

## 4. Scene ↔ `.rs` projection

`.yscene` = persistence SoT. Projection под `project.code_root`:

```text
src/scenes/<scene_slug>/
  mod.rs
  entities/
    <entity_slug>__<8hex>.rs
```

| Action | Meaning |
|---|---|
| Sync Code | export current document → files |
| Apply Code | parse files → one undoable transaction |
| file watch | entity `.rs` change → auto-apply |

Out of scope v1: create/delete entity from file presence; freeform `syn` rewrite.

Inspector: **Open entity projection** открывает соответствующий `.rs` в Monaco.

---

## 5. Что пока не тестировать руками

| Topic | Status |
|---|---|
| Play `AddComponent` for `model3d` / `parent3d` | closed (proxy + GUID parent resolve) |
| CharacterController ↔ Rapier mode switch | non-goal |
| Play Rapier overlay (`physics-rapier`) | closed (props + soft reaction; sensors when active) |
| Shadow cascades / render via intents | non-goal |
| QuestBook authoring UI | not shipped |
| Freeform behavior modules as SoT | open |

---

## 6. Related crates / files

| Piece | Location |
|---|---|
| Intents / capabilities / signals | `crates/yuyib-scene-interaction/` |
| Editor apply | `scene.interaction.apply` in `yuyib-editor` |
| Play host | `crates/yuyib-play` (`interaction_bridge`, `use_interaction`, `trigger_volumes`) |
| Rapier overlay / sensors | `DynamicsOverlay3d` + `trigger_signals::TriggerOverlapTracker` (`--features physics-rapier`) |
| Schemas | `yuyib-authoring-yuyib` (`yuyib.interactable`, `yuyib.trigger`) |
| Projection | `yuyib-scene-projection` |
