# Persisted project, scene и asset schemas

Этот документ задаёт нормативную data model для Editor authoring formats.
Базовые JSON envelopes, bounded document store и opaque round-trip уже
реализованы в authoring/core crates. Typed materializers и migration functions
подключаются capability adapters по мере закрытия coverage.

## Разделение форматов

Authoring и shipping data имеют разные задачи:

| Artifact | Назначение |
|---|---|
| `project.yuyib` | Project GUID, capabilities/features, startup scenes, build/Play settings |
| `*.yscene` | Diff-friendly authored entities, components и references |
| `*.yasset` или sidecar metadata | Asset GUID, source URI, importer и versioned settings |
| `*.ypack`/cooked manifest | Runtime-optimized immutable artifacts и dependency hashes |

Scene не является сериализованным `bevy_ecs::World`. Cooked package не является
source of truth для authoring. Runtime materialization создаёт World и typed
handles из versioned documents.

`project.yuyib` также может содержать backward-compatible optional
`development` block с Cargo package, project-relative Play executable и literal
arguments. Он используется только Editor process supervisor-ом, не попадает в
shipping state и не является shell command. Отсутствие block-а оставляет
Play/Cargo UI disabled вместо угадывания команды.

## Identity

Persistent GUID используется для project, scene, entity, asset и других
переиспользуемых authored records. GUID не зависит от:

- ECS allocation/generation;
- Rust type name/`TypeId`;
- source path или line number;
- content hash;
- runtime asset slot;
- GPU/resource handle.

`AssetGuid` и content hash всегда хранятся раздельно. Rename/move source меняет
URI, но сохраняет GUID. Изменение source bytes сохраняет GUID, меняет hash и
invalidates только зависимые cooked artifacts. Duplicate GUID в одном resolved
project graph является hard error, пока пользователь явно не выполнит операцию
reassign/duplicate identity.

Reference на asset содержит GUID и optional stable subresource selector. Индекс
массива допустим только если importer гарантирует его стабильную persisted
семантику; предпочтительны importer-defined stable node/material/clip IDs.

## Scene envelope

Логическая структура `.yscene`:

```json
{
  "format": "yuyib.scene",
  "format_version": 1,
  "scene_guid": "018f0000-0000-7000-8000-000000000001",
  "entities": [
    {
      "guid": "018f0000-0000-7000-8000-000000000002",
      "name": "StreetLamp",
      "components": [
        {
          "schema": "yuyib.transform3d",
          "version": 1,
          "payload": {
            "translation": [12.0, 0.0, -4.0],
            "rotation": [0.0, 0.0, 0.0, 1.0],
            "scale": [1.0, 1.0, 1.0]
          }
        },
        {
          "schema": "yuyib.model3d",
          "version": 1,
          "payload": {
            "model": "018f0000-0000-7000-8000-000000000003",
            "mesh": null,
            "visible": true,
            "render_order": 0
          }
        }
      ]
    }
  ]
}
```

Disk revision не является полем scene schema: document store вычисляет его
как content fingerprint и передаёт отдельно через Editor bridge. Hierarchy
хранится versioned component record-ом yuyib.parent3d, а не вторым
параллельным полем entity envelope.

Точный textual/binary encoding должен иметь deterministic writer, bounded parser
и atomic save. Stable semantics задаются полями envelope, а не Rust layout.
Entity order может использоваться только для deterministic diffs/UI hints, но не
как identity или hierarchy reference.

## Component records

Каждый persisted component содержит:

- unique stable lowercase `schema` ID;
- positive integer `version`;
- bounded `payload`;
- optional envelope extensions, которые также должны сохраняться при
  forward-compatible round-trip.

Registry construction отклоняет duplicate schema IDs. Два crates не могут
«победить» в зависимости от registration order.

Known record проходит migration до current schema, validation и materialization.
Unknown schema, более новая неподдерживаемая version или неизвестный extension
сохраняются как opaque record. Editor может показать read-only raw metadata и
diagnostic, но не удаляет record при save.

Opaque preservation означает отсутствие semantic data loss: неизвестные поля,
types, array contents и envelope extensions сохраняются. Serializer может
нормализовать whitespace/field ordering, если формат это допускает, но не может
разобрать неизвестное значение в lossy generic representation. Если parser не
может гарантировать preservation, документ открывается read-only до явного
upgrade, а не «исправляется» молча.

Удаление component-а пользователем является явной command operation. Нельзя
трактовать отсутствие adapter-а как запрос на удаление.

## Import settings envelope

Asset metadata хранит identity отдельно от import result:

```json
{
  "format": "yuyib.asset",
  "format_version": 1,
  "guid": "018f0000-0000-7000-8000-000000000003",
  "source": "assets/city.glb",
  "content_hash": "blake3:0123456789abcdef",
  "importer": "yuyib.gltf-import",
  "importer_version": 1,
  "import_settings": {
    "schema": "yuyib.gltf-import-settings",
    "version": 1,
    "payload": {
      "policy": "skeletal_preview"
    }
  },
  "dependencies": []
}
```

Importer implementation version и settings schema version не одно и то же.
Первое участвует в cooked cache key и отражает изменение import interpretation;
второе определяет persisted settings migration. Оба versioned явно.

Reimport non-destructive:

1. Editor читает source через bounded resolver.
2. Settings мигрируются и валидируются до запуска job.
3. Import/cook выполняется cancellable job-ом с progress/diagnostics.
4. Новый result публикуется atomically только после успешной validation.
5. При failure последний валидный result/reference остаётся доступным.

Source hash не используется как identity и не перезаписывает GUID.

## Migrations

Persisted schema меняется отдельно от Rust API. Adapter поддерживает
детерминированную последовательность:

```text
schema v1 -> v2 -> ... -> current
```

Migration:

- не выполняет network/file I/O и не обращается к GPU;
- bounded по input size/depth;
- возвращает structured diagnostics;
- сохраняет stable entity/asset identities;
- не удаляет unknown sibling records/fields;
- имеет fixtures для каждой поддерживаемой старой version;
- deterministic для одинакового input.

Executable registry принимает только adjacent edges `vN -> vN+1`. Это делает
gap detection детерминированным и не создаёт неоднозначный выбор direct/stepwise
пути. Отсутствующая migration не вызывает best-effort load: component остаётся
opaque/read-only или загрузка materialization завершается диагностикой в
зависимости от declared requirement.

Переименование Rust-поля без изменения persisted representation не требует
migration. Переименование persisted key, смена units/default semantics,
разделение component-а или изменение reference meaning требует новой schema
version и migration.

## Project и scene revisions

`revision`/content fingerprint используется для optimistic conflict detection,
но не заменяет GUID. Editor запоминает base revision при открытии. Перед save он
повторно читает current external fingerprint:

- совпало — выполнить atomic save;
- изменилось и document clean — предложить reload/compare;
- изменилось и document dirty — показать conflict UI;
- overwrite — только после явного подтверждения и как отдельная операция.

Silent overwrite или silent merge запрещены. File watcher лишь сообщает об
изменении; он не мутирует открытый document сам.

Atomic save пишет новый bounded document во временный файл в той же целевой
области, flush/validates его и заменяет target безопасным platform operation.
Recovery/backup policy должна быть документирована реализацией отдельно.

Command transaction содержит base document revision. Undo/redo и
`Apply Play Mode Changes` проверяют revision conflicts так же, как Inspector
mutations.

## Materialization

Materializer работает с конкретным immutable scene revision:

1. проверяет envelope limits и unique GUIDs;
2. разрешает known schemas и migrations;
3. создаёт runtime entities;
4. строит `EntityGuid -> Entity` и `AssetGuid -> AssetId<T>` maps;
5. применяет components/adapters и validates references;
6. вычисляет derived state через обычные engine systems;
7. возвращает diagnostics и mapping для selection/debugging.

Неизвестный optional component может быть пропущен в runtime только с явной
diagnostic и сохранением его authored record. Неизвестный required component
блокирует Play/materialization, но не мешает открыть и сохранить документ без
потери данных.

Runtime entities, asset slots и derived components никогда не записываются
обратно без adapter-owned explicit command. Whole-World serialization и
whole-World `Apply Play Changes` запрещены.

## Limits и validation

Parser/project policy задаёт bounds как минимум на:

- document bytes и nesting depth;
- entity/component/dependency count;
- string/diagnostic/payload lengths;
- migration steps и expanded output size;
- GUID/reference graph validation work;
- imported source/decode/cooked artifact sizes.

Malformed или oversized input возвращает structured error до unbounded
allocation. Authoring files считаются project data, но importer source bytes всё
равно untrusted согласно RFC 0009.

## Compatibility checklist

При изменении persisted data проверьте:

- unique stable IDs и независимые versions;
- old -> current fixtures;
- current save/load/save determinism;
- unknown component и unknown field preservation;
- asset rename/move без смены GUID;
- content change с cache invalidation без смены GUID;
- missing/forward schema behavior;
- external revision conflict UI;
- materialization и explicit Apply whitelist;
- headless/runtime build без Editor serializer/UI dependencies.
