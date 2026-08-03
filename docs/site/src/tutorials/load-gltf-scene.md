# Tutorial: high-level загрузка glTF-сцены

> **Статус:** Experimental  
> **Requires:** features `app` + `three-d` (входят в `desktop-full`)  
> **Цель:** загрузить `.glb` / `.gltf` **без зависания окна** и понять, что лежит в `LoadedGltfScene`

Предыдущие шаги: [окно](first-window.md), [игра](first-game.md).  
Справочный guide (весь surface): [Streamed glTF scene](../guides/streamed-gltf-scene.md).

Runnable:

```powershell
cargo run -p yuyib --example gltf_map_loading_screen
cargo run -p yuyib --example street_city_m1_smoke
```

## 1. Какую проблему решаем

Наивный путь:

```text
прочитать файл → распарсить glTF → создать meshes → upload GPU → только потом кадр
```

На большой карте окно «замирает», progress bar не обновляется, input мёртв.

Yuyib разделяет:

```text
CPU worker: read / import / spawn / collider / decode textures
render thread: poll progress → take_ready → bounded GPU publication → draw
```

High-level тип для этого — **`GltfSceneLoad`**, а не ручной `std::fs::read` + `import_scene_path` в `on_frame`.

## 2. Короткий путь (с пояснениями)

```rust,no_run
use yuyib::prelude::*;

fn start_loading() -> Result<GltfSceneLoad, Box<dyn std::error::Error>> {
    // 1) Конфиг: что делать в worker (import policy, collider, textures…)
    let config = GltfSceneLoadConfig::default();

    // 2) Старт job. Не блокирует. Окно продолжает кадры.
    let loading = GltfSceneLoad::start("assets/maps/city.glb", config)?;
    Ok(loading)
}

fn on_frame(loading: &mut GltfSceneLoad) -> Result<(), Box<dyn std::error::Error>> {
    // 3) Poll без ожидания worker.
    let progress = loading.update();

    match progress.stage {
        GltfSceneLoadStage::Ready => {
            // 4) Забрать готовый CPU-результат ровно один раз.
            let loaded: LoadedGltfScene = loading.take_ready()?;
            // дальше: Game3dScene / prepare_for_frame / render
            let _ = loaded;
        }
        GltfSceneLoadStage::Failed => {
            if let Some(err) = loading.failure() {
                eprintln!("load failed: {err}");
            }
        }
        _ => {
            // Queued / Reading / Processing — рисуйте loading UI по progress.fraction()
            let _ = progress.fraction();
        }
    }
    Ok(())
}
```

## 3. Почему именно эти функции

### `GltfSceneLoadConfig::default() -> GltfSceneLoadConfig`

Готовый набор для playable map:

| Что включает default | Зачем |
|---|---|
| Strict `ImportOptions` | Предсказуемый import без «магии ради красивой картинки» |
| Default glTF scene | Не гадать индекс сцены |
| Static triangle collider | Сразу есть mesh для character / queries |
| Model / per-mesh bounds | Frustum culling без второго прохода authoring |
| Texture preparation в worker | Decode **не** на render thread |
| Bounded pool (2 workers) | Один request не захватывает весь CPU |

Меняйте builder-методами (`with_static_collider(false)`, `with_import_options(...)`), а не копируйте low-level pipeline.

### `GltfSceneLoad::start(path, config) -> Result<GltfSceneLoad, GltfSceneLoadStartError>`

**Почему `start`, а не `load_blocking`?**  
Чтобы event loop оставался живым. Синхронный fail (битый config, task pool, registry limits) возвращается **сразу**. Тяжёлый I/O/parse — в worker.

**Что возвращает:** handle на **один** in-flight request (`GltfSceneLoad`). Это не `LoadedGltfScene` и не GPU mesh.

Для нескольких одновременных карт используйте `start_on(..., Arc<TaskPool>)` — общий pool и backpressure (`Submit` error), а не N отдельных pools.

### `loading.update() -> GltfSceneLoadProgress`

| Поле | Смысл |
|---|---|
| `stage` | `Queued` → `Reading` → `Processing` → `Ready` / `Failed` / `Taken` |
| `completed_work` / `total_work` | Для progress bar |
| `fraction()` | Удобный 0..1 |

**Почему poll, а не callback из worker?**  
Worker не трогает UI/`World`/WGPU. Main thread сам решает, когда читать progress и что рисовать.

Вызывайте `update()` **раз за frame** (в `on_frame` / `Update`). Не крутите busy-wait.

### `loading.take_ready() -> Result<LoadedGltfScene, AssetLoadTakeError>`

Забирает результат, когда `stage == Ready`.

| Ошибка | Когда |
|---|---|
| Not ready | Вызвали слишком рано |
| Failed | Смотрите `failure()` |
| Already taken | Повторный `take_ready` |

**Почему отдельный `take`, а не `update` сразу отдаёт сцену?**  
Явная ownership-передача: до `take` request в очереди; после — вы владеете `LoadedGltfScene`, stage становится `Taken`.

### Что внутри `LoadedGltfScene`

Это **CPU / ECS результат** после import + spawn, ещё до полной GPU residency:

| Содержимое | Зачем вам |
|---|---|
| `World` + spawned entities | Hierarchy, `Model3d`, transforms |
| `Assets<Model>` | Typed meshes/materials |
| Optional static collider | Character / ray queries |
| Bounds | Culling / spawn helpers |
| Diagnostics | Missing UV, unbound materials, cook cache hit… |
| Prepared textures (если включено) | Bounded upload на render thread |

**Почему не сразу GPU buffers?**  
Upload ограничен budget’ом на кадр (`prepare` / publication path). Иначе один huge GLB снова заморозит frame. См. раздел Bounded GPU publication в [streamed guide](../guides/streamed-gltf-scene.md).

## 4. Связка с отрисовкой (следующий слой)

После `take_ready`:

1. Создайте `Game3dScene` (high-level PBR path) — [Game3dScene](../guides/game-3d-scene.md).
2. Каждый render frame публикуйте residency budget’ом и рисуйте.
3. Character / camera — [input & character](../guides/input-character-quests.md) или profile helpers (`Game3dProfile`, `PlayableLoop3d`).

Не пишите второй glTF decoder «для preview»: Editor и Play обязаны делить production path (см. architecture RFC 0011).

## 5. Когда брать low-level вместо `GltfSceneLoad`

| Задача | API |
|---|---|
| Offline cook / custom importer tests | `import_scene_path` / `import_scene_bytes_cached` |
| Свой residency pipeline | `with_texture_preparation(false)` + ручной upload |
| Один mesh без scene orchestration | `Model` + `Game3dScene` / mesh renderer |

Для «карта в игре с loading screen» почти всегда нужен `GltfSceneLoad`.

## Limits & Caveats

- Source size и decode budgets — разные trust boundaries; для большой доверенной карты поднимите `ImporterRegistryLimits::max_source_bytes`, не отключая geometry limits.
- Material «починка» — через `ModelMaterialPolicy`, не через silent renderer heuristics.
- Shipping без source importers — roadmap post-core.

## См. также

- Полный surface: [High-level загрузка glTF](../guides/streamed-gltf-scene.md)
- Import details: [glTF / Blender import](../guides/gltf-import.md)
- 3D scene facade: [Game3dScene](../guides/game-3d-scene.md)
- Tutorial 2D: [Первый 2D playable](first-2d-playable.md)
