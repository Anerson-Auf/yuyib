# RFC 0010 — high-level streamed glTF scene

- **Статус:** accepted, Lambert + PBR slice implemented
- **Дата:** 2026-08-01
- **Зависит от:** RFC 0002, RFC 0008, RFC 0009

## Проблема

`gltf_map_loading_screen` вручную связывал `TaskPool`, `AssetLoadQueue`, glTF
import, ECS spawn, bounds, static collision, texture decode, GPU publication и
renderer cache. Все отдельные APIs были корректны, но такой example учил
пользователя писать внутренний asset pipeline вместо приложения.

## Решение

`GltfSceneLoad` становится одной stateful facade для CPU-стадий:

```text
start -> update/progress -> take_ready
      worker: read -> typed importer -> ECS -> scene/local bounds + collider -> image decode
```

Результат `LoadedGltfScene` хранит `World`, typed model assets, spawned-scene
identity, bounds, optional collider и worker-prepared texture data. Render-thread
граница остаётся явной:

```text
LoadedGltfScene::prepare_for_frame(frame, Game3dScene)
LoadedGltfScene::prepare_for_frame_with_budget(frame, Game3dScene, budget)
LoadedGltfScene::render(frame, Game3dScene)
```

Facade не владеет окном, event loop, loading UI или camera/gameplay policy.
Пользователь может получить `world_mut`, collider и model handles. Низкий путь
через `AssetLoadQueue`, importer registry, `spawn_scene`,
`PreparedModelTextures` и explicit renderers остаётся public.

Encoded document и decoded glTF data имеют независимые limits. Facade хранит
`ImporterRegistryLimits` рядом с `ImportOptions`: default 64 MiB source bound
не ослабляется глобально, а trusted large maps явно меняют его через
`with_importer_registry_limits`. Нулевая конфигурация отклоняется до submission.

## Scheduling

`start` — ergonomic single-load path с private bounded pool.
`start_on(Arc<TaskPool>, ...)` — основной путь для нескольких streaming scenes;
он не создаёт pool на каждый asset. Request удерживает `Arc` до завершения job.

## GPU contract

Worker не получает WGPU objects. `prepare_for_frame` применяет
`ModelUploadBudget3d::default()` и ограничивает за frame как texture slots, так
и полные geometry primitives. `prepare_for_frame_with_budget` оставляет эти
лимиты явными для профилируемого приложения. Переход `textures -> geometry ->
ready` транзакционный: partial model не рисуется, ошибка освобождает её texture
ownership и GPU buffers.

`target_geometry_bytes` — soft limit по исходным vertex/index streams между
primitives. Текущий WGPU mesh API создаёт primitive атомарно, поэтому первый
primitive кадра может быть больше target; progress выставляет
`uploaded_oversized_primitive`. Hard byte bound потребует следующего слоя —
chunked buffer initialization/staging upload внутри одного primitive.

Textures имеют независимые hard slot и soft decoded-RGBA byte limits.
Deduplicated slots не расходуют byte budget. Первая unique texture может быть
oversized по той же progress-guarantee причине и отмечается отдельным flag.

Progress totals принадлежат renderer residency state, а не caller: queued,
uploading и cached model сохраняют одинаковые texture/primitive/byte totals.
Поэтому UI progress не меняет denominator на переходе от textures к geometry.
Отсутствующая worker preparation для textured model является typed error, не
вечным `ready=false` и не поводом для скрытого synchronous decode.

Bounded publication поддерживает standard Lambert и PBR routes через общий
renderer-owned residency contract. PBR создаёт route-specific material bind
groups вместе с соответствующим primitive, не заранее для всей модели. Unlit
пока не использует prepared publication: его cache остаётся отдельным eager
preview path.

## Character composition

`TexturedSkeletalSceneRenderer3d` принимает explicit root model-to-world matrix
и `DepthLoad`. Поэтому animated character можно поставить на controller feet и
дорисовать через `DepthLoad::Load` после карты без очистки world depth.
`AnimationSnapshot::world_matrices` остаётся reusable model-space pose и также
даёт bone socket для камеры. `cyberpunk_city_playable` держит chase / FPS focus
на eye sockets (`Eye_R_047` / `Eye_L_056`) и использует один root transform для
камеры и GPU draw.
