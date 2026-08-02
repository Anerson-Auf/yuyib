# Архитектура Yuyib

Эта директория хранит Architecture Decision Records (ADR/RFC): почему public
границы устроены именно так, какие alternatives отвергнуты и какие invariants
обязаны сохранять следующие изменения. Это документация для maintainers, а не
первый учебник пользователя. Практические задачи находятся в
[`docs/site/src`](../site/src/index.md).

Текущее состояние продукта, измеримые milestones и граница Engine MVP находятся
в [`ROADMAP.md`](ROADMAP.md). Публичные ограничения — в корневом
[`KNOWN_ISSUES.md`](../../KNOWN_ISSUES.md). ADR фиксирует решение; roadmap
фиксирует, завершён ли соответствующий пользовательский vertical slice.

## Карта системы

```text
Application / Game host
        |
        +-- ECS world + gameplay schedules
        |       |
        |       +-- 2D / 3D scene components
        |       +-- input / physics / interactions
        |
        +-- Assets and importer registry -- bounded background TaskPool
        |       |
        |       +-- image / glTF / Source 1 content
        |
        +-- Renderer -- render graph -- 2D / 3D / UI passes
        |
        +-- optional Audio / Network / WebView services

Yuyib Editor (separate consumer)
        |
        +-- authoring adapters -> capability registry
        +-- authored documents -> explicit runtime materialization
        +-- importer/cooker -> shared renderer preview
        +-- process-isolated Play runner
```

Главное разделение — CPU-authored state не владеет GPU/event-loop lifecycle.
Assets и ECS остаются renderer-neutral; GPU publication выполняется явно и с
budget. High-level facade сокращает common path, но не скрывает low-level
ownership и failure modes.

## Принятые решения

| RFC | Решение | Пользовательская документация |
|---|---|---|
| [0001](0001-platform-and-public-api.md) | Native-first platform и facade/public API | [Начало работы](../site/src/getting-started.md), [Subsystems](../site/src/reference/subsystems.md) |
| [0002](0002-assets-import-and-streaming.md) | Typed assets, importer boundary и streaming | [Assets](../site/src/concepts/assets.md), [Asset loading](../site/src/guides/asset-loading.md) |
| [0003](0003-render-graph-and-shaders.md) | Explicit render graph и shader contracts | [Custom passes](../site/src/guides/custom-render-passes.md) |
| [0004](0004-windows-surface-lifecycle.md) | Windows surface/event-loop lifecycle | [Application](../site/src/guides/application.md) |
| [0005](0005-game-lifecycle.md) | Game host, ECS world и frame callbacks | [Game lifecycle](../site/src/guides/game-lifecycle.md) |
| [0006](0006-2d-capability-status.md) | 2D foundation scope | [2D concepts](../site/src/concepts/two-d.md) |
| [0007](0007-capabilities-and-game-schedules.md) | Capability composition и schedules | [Game lifecycle](../site/src/guides/game-lifecycle.md) |
| [0008](0008-high-level-2d-and-3d-scenes.md) | High-level scene facades | [Game2dScene](../site/src/guides/game-2d-scene.md), [Game3dScene](../site/src/guides/game-3d-scene.md) |
| [0009](0009-extensible-importer-sdk.md) | Extensible bounded importer SDK | [Custom importers](../site/src/guides/custom-importers.md) |
| [0010](0010-high-level-streamed-gltf-scene.md) | Standard streamed glTF orchestration | [Streamed glTF](../site/src/guides/streamed-gltf-scene.md) |
| [0011](0011-editor-authoring-contract.md) | Editor как consumer, versioned authoring и shared preview | [Engine integration](../editor/ENGINE_INTEGRATION.md), [Scene format](../editor/SCENE_FORMAT.md) |

## Invariants для новых изменений

1. Один host владеет native event loop и presentation lifecycle.
2. Никакого скрытого file I/O, decode или unbounded upload на render thread.
3. Background queues, source sizes, decode и GPU publication имеют явные
   limits/backpressure.
4. ECS components и imported CPU data не зависят от конкретного GPU backend.
5. High-level API имеет low-level escape hatch и не дублирует ownership.
6. Errors и degraded states observable; silent fallback допускается только как
   документированная policy.
7. Public API change обновляет rustdoc, task guide, limits и navigation.
8. Authored data использует persistent GUID и versioned stable schema IDs, а не
   runtime `Entity`, `TypeId` или `AssetId<T>`.
9. Editor preview использует production importer/cooker/renderer paths; duplicate
   Editor-only decoder запрещён.
10. Каждая curated capability имеет machine-readable Editor status, даже если
    этот статус `CodeOnly` или `Unavailable`.

## Editor handoff

Canonical entry point для разработки движка после введения authoring boundary —
[`docs/editor/ENGINE_INTEGRATION.md`](../editor/ENGINE_INTEGRATION.md). Форматы,
coverage и scoped verification описаны рядом:

- [`SCENE_FORMAT.md`](../editor/SCENE_FORMAT.md);
- [`CAPABILITY_COVERAGE.md`](../editor/CAPABILITY_COVERAGE.md);
- [`TESTING.md`](../editor/TESTING.md).

Эти документы фиксируют принятый contract, но не заменяют implementation
evidence. Нереализованный adapter должен оставаться явно `Unavailable`.

## Статус RFC

Существующие файлы фиксируют принятый foundation design. Возможность,
упомянутая в RFC, не считается реализованной автоматически. Реальный public
contract определяется текущим Rust API, [API coverage map](../site/src/reference/api-reference.md)
и [Limits & Compatibility](../site/src/reference/limits-and-compatibility.md).
