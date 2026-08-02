# Загрузка ресурсов без остановки окна

> **Статус:** Experimental  
> **Crate / module:** `yuyib::assets`, `yuyib::tasks`  
> **Платформы:** platform-neutral

Загрузка ресурсов не должна останавливать окно. В Yuyib подготовка данных идёт
в фоновых потоках, а подключение результата к приложению — только в том потоке,
который вызывает шаг обновления. Поэтому карта может загружаться во время игры,
а окно продолжает отвечать на ввод и рисовать экран загрузки.

Есть три уровня CPU API:

- `AssetServer` — рекомендуемый путь. Он сразу выдаёт stable typed handle,
  сохраняет metadata и переводит его из `Loading` в `Ready` или `Failed`;
- `AssetLoader` — простой путь. Он сам владеет рабочими потоками, принимает
  функции загрузки и публикует handle только после готовности. Это compatibility
  path для кода, которому pre-residency handle не нужен;
- `AssetLoadQueue` — низкоуровневый путь. Он нужен, когда результат надо
  сначала загрузить в GPU, проверить, превратить в ECS-сущности или использовать
  общий `TaskPool` нескольких подсистем.

Source-format dispatch является отдельной typed границей:
`ImporterRegistry<T>` выбирает зарегистрированный `AssetImporter<T>`, а
`AssetServer::try_import_bytes` выполняет import в bounded worker pool и
публикует final metadata вместе со значением. Реализация нового формата описана
в [гайде по importer plugins](custom-importers.md).

## Принцип работы

Работа разделена на две безопасные части:

1. В фоне читаются файлы, распаковываются изображения и разбираются модели.
2. В основном потоке готовый результат публикуется в `Assets`, загружается в
   GPU или превращается в сущности ECS.

Фоновая задача **не** получает доступ к окну, `World` и GPU. Благодаря этому
окно продолжает обрабатывать события и рисовать экран загрузки, а игра не
показывает пользователю «не отвечает». Вызов `poll` не ждёт выполнения задач.

## Рекомендуемый путь: `AssetServer`

```rust,no_run
use yuyib::prelude::*;

let mut assets = Assets::<Vec<u8>>::new();
let mut server = AssetServer::<Vec<u8>, std::io::Error>::new(
    TaskPoolConfig::new(2, 32)?,
)?;
let map = server.try_load(
    &mut assets,
    "Карта",
    AssetMetadata {
        source: Some("assets/map.glb".to_owned()),
        importer_version: Some("yuyib-gltf@0.1".to_owned()),
        ..AssetMetadata::default()
    },
    |reporter| {
        reporter.reading();
        std::fs::read("assets/map.glb")
    },
)?;

assert_eq!(assets.state(map), Some(AssetState::Loading));
let placeholder = Vec::new();
let _visible_value = assets.get_or_placeholder(map, &placeholder);

// Вызывается один раз на main-thread frame boundary.
let update = server.update(&mut assets)?;
if update.ready.contains(&map) {
    assert_eq!(assets.state(map), Some(AssetState::Ready));
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

`AssetMetadata` хранит source, importer/cooker versions, content hash,
dependencies и известные CPU/GPU costs. Failed handle остаётся валидным:
причина доступна через `AssetServer::failure`, а UI не должен угадывать ошибку
по отсутствующему значению.

## Простой путь: `AssetLoader`

`AssetLoader` подходит большинству приложений, где подготовленный CPU-ресурс
нужно положить в `Assets`. Он не скрывает правила потоков: фоновая функция не
получает доступа к окну, GPU и ECS, а `update` вызывается из основного цикла.

```rust,no_run
use yuyib::prelude::*;

let mut loading = AssetLoader::<Vec<u8>, std::io::Error>::new(
    TaskPoolConfig::new(2, 32)?,
)?;

loading.try_load("Карта", |reporter| {
    reporter.reading();
    let bytes = std::fs::read("assets/map.glb")?;
    reporter.set_total_work(bytes.len() as u64);
    reporter.set_completed_work(bytes.len() as u64);
    reporter.decoding();
    Ok(bytes)
})?;

let mut assets = Assets::new();
// Вызывается из on_frame. Ничего не ждёт.
let update = loading.update(&mut assets);
for (_request, handle) in update.published {
    // Ресурс вставлен в Assets именно текущим, основным потоком.
    let _asset = assets.get(handle).expect("только что опубликован");
}

println!("Готово: {} из {}", update.summary.finished(), update.summary.total);
# Ok::<(), Box<dyn std::error::Error>>(())
```

`update.published` содержит только результаты, которые стали готовы в этом
кадре. Ошибки не теряются: сохраните `AssetLoadId`, а затем получите причину
через `loading.failure(request)`.

## Низкоуровневый путь: `AssetLoadQueue`

Выберите его для собственной публикации, например когда между чтением файла и
появлением объекта нужно создать GPU-текстуру или сущности ECS. Очередь получает
явно созданный `TaskPool`, поэтому у программы нет скрытого глобального потока
и неожиданной нагрузки на процессор.

```rust,no_run
use yuyib::prelude::*;

let pool = TaskPool::new(TaskPoolConfig::new(2, 32)? )?;
let mut loading = AssetLoadQueue::<Vec<u8>, std::io::Error>::new();

loading.try_queue(&pool, "Карта", |reporter| {
    reporter.reading();
    let bytes = std::fs::read("assets/map.glb")?;
    reporter.set_total_work(bytes.len() as u64);
    reporter.set_completed_work(bytes.len() as u64);
    reporter.decoding();
    Ok(bytes)
})?;

let mut assets = Assets::new();
// Вызывается из on_frame. Ничего не блокирует.
loading.poll();
for (_request, handle) in loading.publish_ready(&mut assets) {
    // Здесь ресурс уже resident в CPU store. Теперь можно создать сущность.
    let _asset = assets.get(handle).expect("только что опубликован");
}

let progress = loading.summary();
println!("Готово: {} из {}", progress.finished(), progress.total);
# Ok::<(), Box<dyn std::error::Error>>(())
```

`try_queue` не ждёт, когда освободится очередь рабочих задач. Если возвращён
`AssetLoadSubmitError::Full`, сохраните запрос и повторите его в следующем
кадре. Это важно для большого списка ресурсов: нельзя превращать загрузочный
экран в блокирующее ожидание.

## Экран загрузки

Полный запускаемый пример — `gltf_map_loading_screen`:

```text
cargo run -p yuyib --example gltf_map_loading_screen
```

Он использует настоящую фоновую загрузку `.glb`, а не искусственную задержку.
Worker выполняет file I/O, typed glTF import, создание ECS-сцены,
bounds/static collider и CPU decode изображений внутри `GltfSceneLoad`. После
`take_ready` вызов `LoadedGltfScene::prepare_for_frame` публикует не более
четырёх texture slots за кадр и продолжает показывать нативную полосу. Только
после GPU residency пример включает character controller и отрисовку карты.

Example теперь демонстрирует high-level путь, а не вручную повторяет asset
pipeline. Подробный API и shared-pool вариант находятся в
[High-level загрузке glTF-сцены](streamed-gltf-scene.md). Для собственного
cooker/GPU pipeline остаются описанные ниже `AssetLoadQueue` и
`AssetUploadQueue`.

Для UI не нужен специальный виджет движка. На каждом кадре вызовите `poll`,
прочитайте `summary` и нарисуйте свой фон, название игры, полоску и текст:

```rust,no_run
# use yuyib::assets::AssetLoadQueue;
# let loading = AssetLoadQueue::<(), ()>::new();
let state = loading.summary();
let caption = format!(
    "Загружено {} из {} ресурсов",
    state.finished(),
    state.total,
);
let bar = state.work_fraction().unwrap_or_else(|| {
    if state.total == 0 { 0.0 } else { state.finished() as f32 / state.total as f32 }
});
// Передайте caption и bar в native UI, свой UI-рендерер либо WebView.
# let _ = (caption, bar);
```

`finished` включает готовые, опубликованные и ошибочные запросы. Поэтому рядом
стоит явно показать `failed`, а причину получить через `failure(request)`.
Счётчик `work_fraction` точнее, если загрузчик вызывает `set_total_work` и
`advance`; если размер заранее неизвестен, используйте обычный счётчик файлов.

## Подгрузка во время игры

`take_ready` — низкоуровневый выход. Он забирает CPU-результат, но не вставляет
его в `Assets`. Это точка для создания GPU-ресурса небольшими порциями,
проверки игровых данных и добавления новых ECS-сущностей только после полной
готовности.

```rust,no_run
# use yuyib::assets::{AssetLoadId, AssetLoadQueue};
# fn publish_zone(
#     loading: &mut AssetLoadQueue<Vec<u8>, ()>,
#     request: AssetLoadId,
# ) {
if let Ok(bytes) = loading.take_ready(request) {
    // Главный поток: upload в GPU, затем spawn объектов новой зоны.
    # let _ = bytes;
}
# }
```

Сохраните `AssetLoadId`, который вернул `try_queue`, вместе с описанием зоны.

## Bounded GPU publication

`AssetUploadQueue<Context, Output, Failure>` хранит device-bound closures без
borrow-а renderer-а. На каждом render frame приложение передаёт контекст и
`AssetUploadBudget { max_bytes, max_jobs }`. Очередь выполняет `Required`,
`NearCamera`, `Prefetch`, затем `Background`, сохраняя FIFO внутри приоритета.

Если highest-priority upload не помещается в остаток byte budget, lower
priority не обгоняет его, а `blocked_by_byte_budget` становится `true`. Большой
ресурс следует разбить на chunks или явно увеличить budget; runtime не делает
скрытый oversized upload.

Для glTF-текстур есть специализированный двухфазный путь:
`ModelTextureLoader::prepare(&model)` безопасно вызывается в worker и создаёт
`PreparedModelTextures`, а `upload_some_for_frame(..., maximum_slots)`
публикует ограниченное число decoded textures в конкретный WGPU frame.
`finish()` разрешён только когда `remaining() == 0`; ранний вызов возвращает
явную ошибку, а `release()` откатывает уже созданные cache references.

## Limits & Caveats

- Нет принудительной отмены уже начатой задачи: она должна сама быстро
  завершаться. Это свойство `TaskPool`.
- `AssetLoadQueue` готовит CPU-данные, но не знает material/mesh layout. Это
  сознательная граница: конкретный upload closure создаёт GPU resource через
  renderer context только в `AssetUploadQueue::process`.
- Текущий слой ещё не записывает cooked artifact manifest на диск и не делает
  camera-driven priority автоматически. Эти решения будут построены поверх
  stable handles/metadata, не меняя ECS identity.
