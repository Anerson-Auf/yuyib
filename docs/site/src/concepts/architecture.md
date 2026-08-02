# Архитектура Yuyib для пользователя API

> **Статус:** Current foundation architecture  
> **Область:** public facade, lifecycle, ECS, assets и presentation  
> **Verified desktop platform:** Windows

Yuyib — native-first runtime, а не монолитный game engine object. Приложение
собирается из capability crates: host владеет window/event loop, ECS хранит
игровое состояние, assets/importers создают CPU data, renderer публикует их на
GPU. High-level facades автоматизируют common path, но сохраняют low-level
escape hatch.

## Слои

```text
┌─────────────────────────────────────────────────────────────┐
│ Host: Application / Game                                    │
│ window events, frame boundary, schedules, shutdown          │
├──────────────────────┬──────────────────────────────────────┤
│ World                │ Services                             │
│ ECS components       │ TaskPool, assets, audio, networking  │
│ gameplay + physics   │ bounded queues and explicit owners   │
├──────────────────────┴──────────────────────────────────────┤
│ Content: image / glTF / Source 1 → validated CPU assets     │
├─────────────────────────────────────────────────────────────┤
│ Presentation: render graph → 2D / 3D / UI → WGPU surface    │
└─────────────────────────────────────────────────────────────┘
```

Dependencies направлены вниз/вбок через узкие data contracts. Например,
`yuyib::gltf` не создаёт ECS entities и GPU buffers: он возвращает neutral
`ImportedAsset`. `yuyib::scene` materializes его в ECS, а
`yuyib::render_3d` публикует geometry/textures на GPU.

## Главные public entry points

| Требуемый уровень | Начните с | Когда опускаться ниже |
|---|---|---|
| Native application | `Application` | custom window/surface lifecycle |
| Игра с ECS | `Game` + `GamePlugin` | собственный host/scheduler |
| 2D rendering | `Game2dScene` | custom extraction/batching/pass |
| 3D rendering | `Game3dScene` | custom materials, phases, residency |
| glTF level | `GltfSceneLoad` | custom importer/dependency jobs/GPU cooker |
| Assets | `AssetServer` | special publication transaction |
| Background CPU | shared `TaskPool` | transport-specific async runtime |

Полный module map: [подсистемы и public API](../reference/subsystems.md).

## Frame lifecycle

Конкретные callbacks зависят от `Application` или `Game`, но ownership остаётся
одинаковым:

```text
native events
    ↓
input/window adapters
    ↓
zero or more fixed simulation steps
    ↓
one variable update
    ↓
ECS extraction / derived snapshots
    ↓
render graph passes
    ↓
submit + present
```

Event loop, window и WGPU surface принадлежат host thread. Fixed update нужен
для deterministic simulation; render callback не должен выполнять blocking
I/O, decode или `Task::join`.

Подробнее: [Application](../guides/application.md),
[Game lifecycle](../guides/game-lifecycle.md),
[Runtime и события](runtime-ecs-events.md).

## Три формы state

Одна из самых важных границ — различать authored, derived и resident state:

| Форма | Пример | Кто меняет |
|---|---|---|
| Authored gameplay state | `LocalTransform3d`, `Sprite2d`, `Model3d` | user systems/import adapter |
| Derived snapshot | `WorldTransform3d`, extracted draws, scene bounds | propagation/extraction system |
| Resident backend state | GPU buffers/textures/pipelines | renderer/cache |

Меняйте authored component, а не derived result. Renderer может сохранить GPU
cache между frames, но CPU asset/ECS остаётся source of truth. Практический
пример: [как изменить размер модели](../guides/3d-transforms.md).

## Asset pipeline и backpressure

```text
bounded source bytes
    → importer probe/parse/validate (worker)
    → neutral CPU asset
    → main-thread publication into Assets<T>/ECS
    → bounded per-frame GPU publication
    → resident render resource
```

Каждая стрелка является failure/resource boundary. Encoded source limit,
decoded geometry/image limits, task queue capacity и GPU upload budget — разные
ограничения; увеличение одного не отключает остальные.

Не передавайте WGPU objects через generic asset worker. GPU resources создаёт
renderer на thread/lifecycle, которому принадлежит device. См.
[Assets и импорт](assets.md), [Asset loading](../guides/asset-loading.md) и
[Streamed glTF](../guides/streamed-gltf-scene.md).

## ECS и renderer связаны extraction boundary

Gameplay components не содержат WGPU handles. Перед render создаётся owned
snapshot: он освобождает borrow ECS world и даёт renderer deterministic input.
Это позволяет:

- тестировать simulation/headless code без GPU;
- менять backend без переписывания gameplay components;
- ограничивать visible/draw counts до GPU submission;
- делать ordering и degraded states observable через stats.

Цена — derived state надо обновлять в определённой точке кадра. Например,
после изменения hierarchy вызывается один `propagate_world_transforms`, а не
произвольные пересчёты внутри каждого component setter.

## Ownership

| Объект | Owner | Lifetime rule |
|---|---|---|
| Window/event loop | `Application` / `Game` | один host, `run()` блокирует thread |
| ECS `World` | game/application state | systems borrow на ограниченное время |
| `Assets<T>` | application/subsystem | handle валиден только для своего storage generation |
| `TaskPool` | application/subsystem | drop drains accepted jobs |
| Renderer/GPU cache | presentation layer | используется внутри render lifecycle |
| Audio engine | application audio state | drop завершает mixer sounds |
| Network connection | caller-owned service | framing limits принадлежат connection policy |

## High-level API не скрывает стоимость

High-level scene может выполнить propagation, culling, cache lookup и draw за
один вызов, но возвращает stats/errors. Большие uploads используют отдельный
prepare step с budget. API, который выглядел бы синхронным `load_and_render`,
был бы анти-паттерном: file I/O и GPU allocation дали бы непредсказуемый frame
spike и зависшее окно.

## Где лежат архитектурные решения

Maintainer-facing ADR/RFC находятся в `docs/architecture`. Их индекс описывает
принятые alternatives и invariants. Wiki остаётся user contract: она должна
объяснять, какой API выбрать и как он ведёт себя сегодня, а не только почему
он когда-то был спроектирован.

## Limits & Caveats

- Foundation APIs имеют статус Experimental.
- Windows — единственная verified desktop platform; neutral crates могут
  компилироваться шире, но это не равно platform support guarantee.
- High-level facades не отменяют resource limits и thread affinity.
- Возможность из RFC не считается реализованной, пока её нет в public Rustdoc
  и [compatibility matrix](../reference/limits-and-compatibility.md).

