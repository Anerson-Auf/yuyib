# Engine integration для Yuyib Editor

Этот документ — основной entry point для разработчика движка после появления
Editor boundary. Перед обычным engine change не нужно повторно сканировать весь
Editor. Сначала прочитайте этот файл, затем только связанный schema/adapter и
актуальный coverage entry.

> **Статус:** authoring registry, GUID/schema envelopes, migrations, commands,
> document/process core, viewport shell, scene bridge и Monaco — foundation.
> Visual materializers закрыты для TRS / Model / DirectionalLight; glTF Asset
> preview — incremental (не full DoD). Apply Play (TRS whitelist) и rust-analyzer
> diagnostics + completion + hover + signatureHelp + definition + references + rename + code actions + allowlisted
> `executeCommand` (`rust-analyzer.*`) — closed
> (E1); texture remap closed; `project.cook` + export/import `*.ypack` hydrate
> closed; cooked-only binary without importers — open.
> Операционный статус: [`ENGINE_HANDOFF.md`](ENGINE_HANDOFF.md).
> **SoT:** [`SOURCE_OF_TRUTH.md`](SOURCE_OF_TRUTH.md).
> Практический authoring API: [`AUTHORING_GUIDE.md`](AUTHORING_GUIDE.md).
> 3D game-loop checklist: [`GAME_LOOP_3D.md`](GAME_LOOP_3D.md).

## Что изменилось концептуально

Editor остаётся consumer Yuyib и не становится владельцем runtime:

```text
runtime capability
    -> colocated/companion authoring adapter
    -> EditorCapabilityRegistry
    -> Inspector / Scene I/O / Preview / Diagnostics / Code navigation
```

Runtime crate не зависит от Editor shell. Для shipping/headless сборок
authoring registration отключается feature-ом или исключением companion crate;
WGPU, WebView, Monaco, rust-analyzer и Editor process management не протекают в
runtime dependency graph.

Неподдержанная capability не замалчивается. Она получает machine-readable
набор independent surfaces: `Visual`, `Asset`, `Runtime`, `CodeOnly` или
единственный `Unavailable`. Смешивать `Unavailable` с implemented surface
реестр запрещает; unavailable record обязан указать reason и milestone.

## Минимальный workflow для engine change

Для каждой новой или изменённой capability выполните эти шаги:

1. Реализуйте runtime API и scoped tests в owning crate.
2. Решите, является ли состояние runtime-only, authored или derived.
3. Назначьте/сохраните unique stable capability ID. Если состояние persisted —
   назначьте независимые schema ID/version.
4. Добавьте authoring descriptor либо явно зарегистрируйте `CodeOnly` или
   `Unavailable` с причиной и tracking reference.
5. Для `Visual` добавьте field metadata, defaults, units, validation и Inspector
   mapping. Изменения должны идти через commands.
6. Для persisted state добавьте serializer/deserializer и migration. Проверьте
   сохранение неизвестных records/fields.
7. Для asset/import settings добавьте stable settings schema, dependency/hash
   policy, preview adapter и non-destructive reimport.
8. Добавьте authored-to-runtime materialization. Runtime-to-authored разрешайте
   только через явный `Apply Play Changes` whitelist.
9. Добавьте diagnostics, docs и source navigation. Для systems зарегистрируйте
   ownership, schedule и read/write component IDs.
10. Обновите machine-readable coverage manifest. Human report и palette должны
    генерироваться из него, а не поддерживаться вручную как второй source of
    truth.
11. Выполните scoped checks из [`TESTING.md`](TESTING.md). Не расширяйте их до
    всего workspace, если изменённая область уже доказана.

Перед началом ответьте в change description на четыре вопроса:

- Это runtime-only или authored state?
- Какой stable capability/schema ID?
- Как capability preview/materialize-ится?
- Где пользователь найдёт visual control, diagnostics, docs и source code?

## Куда помещать integration

| Изменение | Основной владелец integration |
|---|---|
| Primitive/vector/enum field component-а | Descriptor/derive рядом с component |
| Component с domain validation | Owning crate или companion authoring adapter |
| Importer и import settings | Importer crate + asset authoring adapter |
| Material/render preset | Material/render crate + viewport preview adapter |
| Physics/collision shape | Physics facade adapter, не backend-specific Editor code |
| Gameplay system | Plugin registration + `SystemDescriptor` |
| Custom render graph/network authority | Specialized adapter или явный `CodeOnly` |
| Editor panel/workspace behavior | Editor shell, без runtime semantics |

Editor core не должен импортировать каждый engine component и строить
централизованный `match`. Если новый component требует изменения общего
Inspector renderer-а, сначала проверьте, не является ли это новым reusable
`FieldKind` или domain widget contract.

## Stable identity и schema

Никогда не сохраняйте raw `Entity`, Rust `TypeId`, type name, `AssetId<T>`, GPU
handle или source `file:line`. Используйте:

- persistent `EntityGuid`, `AssetGuid`, `SceneGuid`, `ProjectGuid`;
- stable lowercase capability/schema ID;
- integer schema version;
- explicit GUID-to-runtime-handle resolution при materialization.

`AssetGuid` и content hash имеют разные назначения. GUID отвечает за identity и
не меняется при rename/move. Hash отвечает за bytes/cache invalidation и меняется
при изменении content.

Breaking persisted change требует migration. Rust rename без изменения
persisted meaning не обязан повышать schema version. Удаление stable ID,
изменение transform/hierarchy semantics или material parameter meaning — не
локальный refactor; сначала требуется compatibility review.

Полная envelope и forward-compatibility policy описаны в
[`SCENE_FORMAT.md`](SCENE_FORMAT.md).

## Preview integration

Preview — обязательный ответ на вопрос «что реально увидит runtime»:

```text
source -> import settings -> importer/cooker -> neutral/cooked asset
       -> preview adapter -> Yuyib renderer viewport + diagnostics
```

Не добавляйте Editor-only decoder, material evaluator или simplified renderer.
Если общего runtime path недостаточно для preview, исправьте reusable engine
boundary или явно задокументируйте ограничение. Дублирование glTF/image decode
создаёт две несовместимые истины и является архитектурной ошибкой.

Preview adapter должен объявить:

- input asset/subresource types и stable IDs;
- cancellable job и bounded source/decode/publication limits;
- progress phases и structured diagnostics;
- cache key, dependencies, invalidation и memory budgets;
- selection для scene/mesh/material/animation clip;
- доступные overlays: collision, normals, tangents, UV, bounds, origin;
- material overrides и одинаковые с Play Mode PBR/render presets;
- import setting schema и non-destructive reimport behavior.

Diagnostics должны позволять пройти от draw result к source metadata. Для mesh
нужны как минимум material assignment, missing/unsupported channels, selected
fallback, bounds и importer/cooker versions. Ошибка reimport оставляет последний
валидный result доступным и не меняет asset identity.

## Commands и authored mutations

Inspector, hierarchy, gizmo, asset settings и bridge создают commands, а не
мутируют document напрямую. Для новой операции определите:

- validation и preconditions;
- atomic transaction boundary;
- inverse operation;
- merge/coalescing key для continuous edits;
- base revision и conflict behavior;
- preview/runtime invalidation после commit.

Несколько изменений одного gesture, например перемещение selection с children,
фиксируются одной транзакцией. Drag и повторный text edit могут coalesce, но
отдельное подтверждённое действие пользователя не должно случайно сливаться с
предыдущим.

External file watcher не имеет права silently reload/overwrite dirty document.
При несовпадении last-seen revision показывается conflict UI: compare/reload,
save as, merge там, где он поддержан, или explicit overwrite.

## Materialization и Play Mode

Materializer принимает immutable authored snapshot/revision и создаёт отдельный
runtime world. Он разрешает GUID references, создаёт process-local handles,
вычисляет derived state и возвращает structured diagnostics. Частично
материализованный мир не должен незаметно считаться успешным.

Полноценный Play Mode работает в subprocess. Runner получает project/scene
revision и build/cooked inputs, публикует lifecycle/diagnostics и может быть
остановлен независимо. Его panic/game crash не меняют Editor document.

`Apply Play Mode Changes`:

- запускается только явно;
- работает только для adapter whitelist authored properties;
- создаёт обычную command transaction с preview diff;
- проверяет исходную revision и conflicts;
- никогда автоматически не копирует весь ECS `World` назад.

## Script ↔ object Intent Bridge

Пошаговые payloads и capability table —
[`AUTHORING_GUIDE.md`](AUTHORING_GUIDE.md) (§2–3).

Полноценные `.rs` scripts не мутируют `.yscene` / ECS напрямую. Они шлют
`SceneInteractionIntent` (`yuyib-scene-interaction`) по `EntityGuid`:

- ops: `set_translation` / `set_component_field` / `add_component` / `emit_signal`;
- discoverability: `BridgeCapabilities` (`editor_capabilities` /
  `play_capabilities`) — unsupported → hard error, не silent no-op;
- batch: `SceneInteractionBatchResult` (submitted / applied / signals);
- signals: opaque `(name, payload)`; optional
  `try_parse_quest_progress_signal` → host maps onto `QuestSignal` (crate не
  зависит от `yuyib-gameplay`);
- **Editor** (`scene.interaction.apply`, `EditorDocumentBridge`) → одна undoable
  command transaction + `host.scene.interaction.signal`;
- **Play** (`PlayInteractionHost` + `PlayWorldBridge`) держит GUID map, pending
  queue и frame signal drain; TRS + `Model3d`/`DirectionalLight3d` fields;
  `EmitSignal` → optional host `QuestBook::apply_signal`;
- **Interactable use**: materialize `yuyib.interactable` → `Position3d` +
  `SphereCollider3d`; `KeyE` → `request_use_raycast_3d` → local Accept →
  `EmitSignal(interaction id)` into the same bridge/QuestBook path;
- **Authoring triggers**: materialize `yuyib.trigger` → gameplay `Trigger` +
  sphere query; player probe via `overlap_spheres_3d` → `trigger.*` intents
  (Entered/Stayed/Exited) each frame after locomotion;
- **Rapier triggers**: `TriggerOverlapTracker` converts
  `collect_trigger_overlaps` pairs into the same `trigger.*` intents for hosts
  that compose Rapier beside CharacterController — no physics-mode switch;
- Play → `.yscene` только через существующий Apply whitelist;
- **не** смешивать с conflating player authority and Intent Bridge document writes.

Это отдельный слой от gameplay `InteractionRequested` (player interactables) —
Play use-loop consumes that adapter and feeds the bridge.

Asset и scene edits должны применяться без Rust-компиляции. Изменение Rust-кода
проходит через build/check и restart runner; dynamic Rust library hot swap не
является частью контракта.

## Code navigation вместо «script объекта»

ECS entity может читаться и изменяться множеством systems. Поэтому команда
`Open code for object` разворачивается в осмысленные действия:

- открыть source выбранного component-а;
- открыть его authoring adapter;
- найти systems, читающие component;
- найти systems, пишущие component;
- открыть owning plugin;
- создать behavior component + system из template;
- открыть project bootstrap/world configuration;
- выполнить scoped Cargo check.

Целевой editor component — зрелый Monaco в WebView shell с
`rust-analyzer`/LSP. Собственный text editor, простое textarea или имитация
syntax-aware editing не разрабатываются.

Planned editor-only `SystemDescriptor` содержит stable system ID, plugin ID,
schedule, read/write stable component IDs, documentation и navigation target.
`file:line` допустим как metadata конкретной workspace revision, но не как
persisted identity и не в `.yscene`. Runtime scheduling semantics остаются в
engine; Editor registry лишь описывает их для поиска.

## Compatibility-sensitive changes

Следующие изменения требуют проверки Editor contract даже при небольшом diff:

- transform, hierarchy, prefab или scene materialization semantics;
- asset GUID resolution, dependency graph или content hash policy;
- persisted component/import-settings schema;
- stable capability ID, plugin ID или system ID;
- importer output meaning и fallback policy;
- material/shader parameter schema;
- viewport/input coordinate convention;
- Play runner lifecycle или `Apply Play Changes` whitelist;
- удаление migration или opaque-record preservation;
- feature graph, из-за которого headless/shipping начинает тянуть authoring UI.

## Что остаётся неизменным

- Один host владеет native event loop и presentation lifecycle.
- ECS/imported CPU state renderer-neutral.
- Worker не мутирует `World` произвольно и не получает WGPU objects.
- File I/O, decode и upload bounded; GPU publication происходит на render
  boundary.
- High-level API сохраняет low-level escape hatch.
- Compile-time Cargo plugins остаются стандартным extension path.
- Runtime, shipping и headless builds не зависят от Editor shell.

## Перед handoff engine change

В change summary укажите:

- owning crate и authoring adapter;
- stable IDs и schema versions;
- migration/forward-compatibility impact;
- preview/materialization path;
- capability coverage status;
- commands/Play whitelist impact;
- выполненные scoped checks;
- сознательно не выполненные heavy/workspace checks;
- известные ограничения и следующий milestone.

Если adapter или registry ещё не реализован, зафиксируйте отсутствие явно: оставьте
machine-readable `Unavailable` с причиной. Это лучше, чем UI, который молча не
показывает существующую capability.
