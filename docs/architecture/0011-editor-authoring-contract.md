# RFC 0011 — Editor и authoring contract

- **Статус:** accepted, implementation in progress
- **Дата:** 2026-08-01
- **Зависит от:** RFC 0001, RFC 0002, RFC 0005, RFC 0007, RFC 0009, RFC 0010

## Проблема

Engine capabilities сейчас проверяются главным образом через отдельные examples
и project-specific code glue. Этот путь полезен для low-level API, но плохо
масштабируется для assets, scenes, materials, collisions и animations:
положение и scale приходится менять в Rust, результат импорта виден только после
компиляции, а diagnostics разных подсистем не собраны в одном постоянном
authoring environment.

Editor должен решать эту проблему, не становясь вторым runtime и не создавая
альтернативные import/render semantics. Простая оболочка с hard-coded панелями
также неприемлема: при развитии Yuyib она быстро разойдётся с capability crates.

## Решение

`Yuyib Editor` является отдельным application и **consumer публичных Yuyib
contracts**. Он не владеет engine lifecycle, ECS semantics, asset decoding или
renderer policy. Общая граница выглядит так:

```text
Yuyib capability crate
        |
        +-- runtime API
        +-- optional authoring adapter
                    |
                    v
          EditorCapabilityRegistry
             |       |       |
             v       v       v
        Inspector  Scene I/O  Preview/diagnostics

Editor shell -> authored document -> materialization -> isolated Play runner
```

Authoring contract должен жить в небольшом renderer/UI-neutral слое. Capability
crate регистрирует descriptor рядом со своим runtime типом либо поставляет
companion authoring crate. Editor shell отвечает за workspace, panels, viewport,
commands и process management, но не содержит централизованный `match` по всем
engine типам.

Shipping и headless dependency graphs не должны получать Editor, WebView,
source-code workspace или authoring-only dependencies. Compile-time Cargo crates
и feature flags остаются основным extension mechanism; dynamic Rust DLL ABI не
вводится.

Этот RFC задаёт целевой контракт. Упомянутые ниже Rust types, manifests и file
formats считаются реализованными только после появления соответствующего кода и
scoped tests. Текущее состояние отражается в
[`CAPABILITY_COVERAGE.md`](../editor/CAPABILITY_COVERAGE.md), а не выводится из
самого RFC.

## Authored state и runtime state

Editor хранит `AuthoringWorld`/scene document отдельно от runtime `World`.
Materialization создаёт runtime entities, process-local handles и derived
components из authored records. Runtime systems могут свободно менять runtime
world в рамках engine lifecycle, но эти изменения не перезаписывают исходную
сцену автоматически.

`Apply Play Mode Changes` является отдельной пользовательской операцией. Adapter
обязан перечислить whitelist authored properties, которые разрешено вернуть из
runtime. Derived transforms, caches, physics contacts, renderer residency,
network state и другие transient values никогда не копируются назад по
умолчанию.

Play Mode запускается в отдельном процессе. Panic, game crash и device loss в
runner не должны завершать Editor или повреждать authored document. Первый
prototype может использовать ограниченный in-process preview только как
временный implementation detail; это не меняет process-isolation contract для
полноценного Play Mode.

## Persistent identity и versioning

В persisted authoring data запрещены:

- raw `bevy_ecs::Entity`;
- Rust `TypeId` или type name как identity;
- process-local `AssetId<T>`;
- GPU handles, pointers и runtime cache keys;
- source `file:line` как часть scene schema.

Entity, asset, scene и project получают persistent GUID. Asset GUID существует
независимо от source/content hash: rename или move файла не меняет identity, а
изменение bytes меняет hash и invalidates cooked artifacts без создания нового
asset. Runtime resolver явно переводит GUID в typed process-local handle.

Каждая сохраняемая capability имеет уникальный stable lowercase schema ID и
независимую integer schema version, например `yuyib.transform3d@2`. Import
settings имеют собственные stable ID/version и migration chain. Изменение Rust
API само по себе не определяет изменение persisted schema.

Duplicate stable IDs являются hard error при registry construction и в CI.
Удалять старую schema или migration нельзя, пока поддерживаемые project versions
могут содержать соответствующие records.

Forward compatibility обязательна: неизвестный component или неизвестные поля
его record загружаются как opaque payload и сохраняются обратно без потери
данных. Старый Editor может запретить редактирование или materialization
неизвестной capability, но не имеет права удалить её при save.

Нормативная модель формата описана в
[`SCENE_FORMAT.md`](../editor/SCENE_FORMAT.md).

## Authoring adapters

Обычный descriptor должен предоставлять достаточную информацию для Inspector,
serialization, validation и materialization:

- stable capability/schema ID, version и display metadata;
- field descriptors, defaults, ranges, units и validation;
- serialize/deserialize и migrations;
- add/remove/apply operations;
- authored-to-runtime materialization;
- optional runtime-to-authored whitelist;
- docs, diagnostics и source-navigation metadata;
- declared independent capability surfaces: `Visual`, `Asset`, `Runtime`,
  `CodeOnly` или единственный `Unavailable` с reason/milestone.

Primitive, vector, enum и resource-reference fields должны поддерживать derive
или другой declarative registration path. Сложные capabilities — render graph,
network authority, physics backend policy — получают specialized adapter или
явный `CodeOnly`; фиктивное автоматическое reflection через строки не заменяет
domain semantics.

Coverage registry machine-readable. Из него генерируются Editor palette,
documentation index и human-readable coverage report. Наличие публичной curated
capability без статуса является CI error. Подробный workflow находится в
[`ENGINE_INTEGRATION.md`](../editor/ENGINE_INTEGRATION.md).

## Preview pipeline

Preview является частью authoring contract, а не декоративным Editor feature:

```text
source asset
  -> versioned import settings
  -> registered importer/cooker
  -> neutral/cooked asset
  -> authoring preview adapter
  -> Yuyib viewport + structured diagnostics
```

Editor не реализует альтернативный glTF, image, Source 1 или иной decoder.
Preview использует те же registered importer/cooker, neutral representation,
material selection, renderer presets и GPU publication boundaries, что dev Play
Mode. Иначе Editor и игра будут показывать разные assets, а preview потеряет
диагностическую ценность.

Каждый preview job имеет bounded source/decode/publication work, cancellation,
progress, structured diagnostics, cache key, invalidation policy и RAM/VRAM
budget. Reimport non-destructive: import settings сохраняются отдельно от source
asset; неуспешный reimport не уничтожает последнюю валидную authored reference.

В зависимости от capability preview adapter предоставляет:

- выбор scene, mesh, material, subresource и animation clip;
- animation playback/scrubbing и material override preview;
- collision, normals, tangents, UV, origin/pivot и bounds overlays;
- missing texture channels, unsupported attributes и fallback diagnostics;
- поиск meshes/nodes по material и source metadata;
- сравнение imported metadata с фактически выбранным runtime route;
- те же PBR/render presets и limits, которые применяет Play Mode.

Например, для большой glTF-карты пользователь должен без custom example увидеть,
какие primitives используют `material_0`, какие texture channels отсутствуют,
какой fallback выбран и почему imported metadata отличается от результата draw.

## Commands, revisions и внешние изменения

Любая authored mutation проходит через command layer. Прямая мутация document
из Inspector, gizmo, importer или bridge запрещена, поскольку ломает undo/redo,
dirty tracking, validation и live synchronization.

Command contract включает:

- atomic transactions для нескольких связанных mutations;
- merge/coalescing для drag, text input и continuous property edits;
- undo/redo с восстановлением stable identities;
- base revision и revision conflict detection;
- validation до commit и observable failure;
- возможность отменить ещё не опубликованный preview/import job.

Save использует last-seen external revision/content hash. Если project, scene или
asset metadata изменились на диске после открытия, Editor показывает conflict UI
с reload/compare/save-as/explicit-overwrite; silent overwrite запрещён.

## Code workspace и ECS navigation

Editor не разрабатывает собственный text editor. Code workspace должен
встраивать зрелый editor component (предпочтительно Monaco в WebView shell) и
подключать `rust-analyzer`/LSP, форматирование и Cargo diagnostics. Простое
многострочное поле без syntax highlighting, indentation, bracket handling и
diagnostics не удовлетворяет milestone.

В ECS entity не владеет единственным script, поэтому Editor не имитирует
Unity-style `MonoBehaviour`. Для selected entity доступны операции:

- `Open component source`;
- `Open authoring adapter`;
- `Find systems reading component`;
- `Find systems writing component`;
- `Open owning plugin`;
- `Create behavior component + system`;
- `Open project world/bootstrap code`;
- `Run scoped cargo check`.

Для этого authoring registry дополняется editor-only `SystemDescriptor`:

- stable system ID и plugin ownership;
- read/write component IDs;
- schedule;
- source navigation и documentation;
- optional source `file:line` для текущей build/workspace revision.

`file:line` является ephemeral Editor metadata и не сериализуется в `.yscene`:
пути и номера строк нестабильны. Для уникального object behavior позднее может
появиться `BehaviorRef` со stable ID, но он не превращает Rust ECS в модель «один
script на GameObject».

Scoped Cargo actions запускаются как управляемые cancellable processes с одним
Cargo process за раз, конечным timeout и resource limits проекта.

## Capability evolution

Каждое новое или изменённое engine API обязано ответить на четыре вопроса:

1. Это runtime-only или authored state?
2. Какой stable capability/schema ID и migration policy?
3. Как capability preview/materialize-ится теми же engine paths?
4. Где пользователь находит visual control, diagnostics, docs и source code?

Обычное новое поле component-а должно требовать только descriptor/derive change
рядом с capability crate. Новый domain subsystem может потребовать specialized
adapter, но не изменения Editor core. Capability, которую нельзя редактировать
визуально без ложной abstraction, маркируется `CodeOnly` и получает navigation,
docs и diagnostics.

## Первый полезный milestone

Editor развивается одним проверяемым vertical slice, а не попыткой немедленно
повторить весь Unreal/Unity:

```text
Project/Asset Browser
  -> import diagnostics
  -> asset preview
  -> scene hierarchy
  -> Inspector + gizmo
  -> save/load
  -> isolated Play Mode
  -> code navigation/workspace
```

Milestone закрывается на реальном 3D asset: import/reimport, material и geometry
diagnostics, placement, transform edit, preview overlays, scene round-trip, Play
runner и переход от selected component к читающим/пишущим systems. 2D,
Application profile, advanced cooker/build packaging и расширение coverage идут
следующими increments через тот же contract.

## Не-цели первой итерации

- клон всего Unreal Engine или Unity;
- visual scripting и node shader editor;
- dynamic hot-load произвольного Rust ABI;
- альтернативные asset decoders или Editor-only renderer;
- автоматический reverse-sync всего runtime `World`;
- собственный text editor или language server;
- скрытая сериализация `World` как opaque binary blob.

## Consequences

Editor становится постоянным visual authoring/debugging environment и
одновременно compatibility consumer публичных APIs. Цена решения — stable IDs,
migrations, adapters и coverage gates становятся обязательной частью развития
capability. Эта цена намеренна: без неё расширение engine снова будет доступно
только через случайные examples и wiki knowledge.
