# High-level загрузка glTF-сцены

> **Статус:** Experimental  
> **Requires:** `yuyib::three-d`, `yuyib::app`

`GltfSceneLoad` убирает orchestration импорта из приложения, не скрывая важную
границу CPU/GPU:

```text
CPU worker                                render thread
read/import/spawn/collider/bounds/decode  bounded textures/primitives -> cull -> draw
```

Полный запускаемый Use-Case:

```text
cargo run -p yuyib --example gltf_map_loading_screen
```

Пример использует реальную GLB-карту, продолжает перерисовывать окно во время
импорта, порционно публикует textures и geometry primitives и после готовности
включает физический character controller.

Полный playable vertical slice с street-city картой, animated playermodel,
collision-aware third-person / first-person toggle и triangle-mesh collision:

```text
cargo run -p yuyib --example cyberpunk_city_playable
```

Он загружает `street_city_7_for_games_free.glb` и
`sci-fi_girl_v.02_walkcycle_test.glb` на одном bounded pool из двух workers.
Карта начинает рисоваться только после порционной GPU publication. Chase-камера
держит focus на eye sockets; `V` переключает вид от первого лица (модель игрока
тогда не рисуется).

Headless M1 smoke той же карты **и** animated playermodel (load → grounded
spawn → walk-clip advance → skinned draw → fixed-camera PNG, без окна):

```text
cargo run -p yuyib --example street_city_m1_smoke
```

Общий street-city / character profile живёт в
`examples/support/street_city.rs` и `examples/support/playable_character.rs`
(масштаб модели — `CHARACTER_MODEL_SCALE`), чтобы playable и smoke не расходились.

## Короткий путь

Запустите одну загрузку:

```rust,no_run
use yuyib::prelude::*;

let mut loading = GltfSceneLoad::start(
    "assets/maps/city.glb",
    GltfSceneLoadConfig::default(),
)?;

// Один раз в on_frame. Никогда не ждёт worker.
let progress = loading.update();
if progress.stage == GltfSceneLoadStage::Ready {
    let loaded: LoadedGltfScene = loading.take_ready()?;
    # let _ = loaded;
}
# Ok::<(), Box<dyn std::error::Error>>(() )
```

`GltfSceneLoadConfig::default()`:

- использует strict `ImportOptions`;
- выбирает default glTF scene;
- строит static triangle collider;
- вычисляет model-wide и per-mesh local bounds для frustum culling;
- декодирует model textures в worker;
- создаёт bounded pool из двух workers для этого request.

`with_texture_preparation(false)` предназначен для consumers, которые забирают
`World`/`Model` и используют собственный low-level residency pipeline. Вызов
standard bounded publication для textured model в таком режиме возвращает
`ModelNotQueuedForPreparation`, а не зависает и не выполняет скрытый decode на
render thread. Textureless model продолжает загружать geometry без этого шага.

Настройки меняются builder-методами:

```rust
use yuyib::prelude::*;

let config = GltfSceneLoadConfig::default()
    .with_import_options(ImportOptions::skeletal_preview())
    .with_scene_selection(SceneSelection::Default)
    .with_static_collider(false)
    .with_texture_preparation(true);
# let _ = config;
```

Encoded source limit registry и decoded glTF limits — разные trust boundaries.
Default registry принимает до 64 MiB. Для большой доверенной GLB-карты
увеличьте только source limit, не отключая geometry/image budgets:

```rust
use yuyib::{assets::ImporterRegistryLimits, render_3d::GltfSceneLoadConfig};

let source_limits = ImporterRegistryLimits {
    max_source_bytes: 128 * 1024 * 1024,
    ..ImporterRegistryLimits::default()
};
let config = GltfSceneLoadConfig::default()
    .with_importer_registry_limits(source_limits);
# let _ = config;
```

Нулевой registry limit отклоняется синхронно из `start`/`start_on`. Превышение
валидного source limit остаётся typed worker failure и показывается loading UI.

## Material override policy

Asset-specific material repair is an explicit load policy, not a renderer
heuristic. Attach a `ModelMaterialPolicy` before starting the worker:

```rust
use yuyib::model::{MaterialFactorPatch, ModelMaterialPolicy};
use yuyib::render_3d::GltfSceneLoadConfig;

let policy = ModelMaterialPolicy::new().patch_named(
    "material_0",
    MaterialFactorPatch::new().with_double_sided(true),
);
let config = GltfSceneLoadConfig::default().with_material_policy(policy);
# let _ = config;
```

Importer diagnostics (`gltf-factor-only-material`, `gltf-unbound-material`,
`gltf-unused-texture`, `gltf-external-texture-uri`, `gltf-missing-uv-set`,
`gltf-texcoord-set-nonzero`, …) and policy diagnostics are retained on
`LoadedGltfScene::diagnostics()`. Prefer this path over ad-hoc
`model_mut_before_publication` edits in examples.

Texture inventory (referenced vs unused, external URIs, missing UV sets) is
available without walking low-level bindings:

```rust
# use yuyib::render_3d::LoadedGltfScene;
# fn show(loaded: &LoadedGltfScene) -> Result<(), Box<dyn std::error::Error>> {
println!("{}", loaded.texture_usage_summary()?);
println!("{}", loaded.material_usage_summary()?);
# Ok(())
# }
```

Runnable fixture-free smoke:

```text
cargo run -p yuyib --example gltf_texture_diagnostics
cargo run -p yuyib --example gltf_unbound_material_fallback
```

`ModelTextureLoader::prepare` / `load` decode only **material-referenced**
texture slots. Unused descriptors stay import inventory and do not block GPU
publication (including a missing unused external URI). PBR publication keeps
preparing residency after a geometry-slot failure; a missing UV set falls back
to a factor-only draw while importer diagnostics remain the source of truth.

## Semantic collision layers

Default `collider()` по-прежнему содержит всю видимую geometry. Если gameplay
нужен отдельный ground/road collider, loader может worker-side собрать
дополнительные слои по source metadata без asset-specific правил в engine:

```rust
use yuyib::prelude::*;

let ground_id = GltfSceneColliderLayerId3d::new("ground")?;
let street = GltfSceneCollisionNameMatch3d::prefix("street_level_")?;
let ground = GltfSceneColliderLayer3d::new(
    ground_id.clone(),
    GltfSceneCollisionSelector3d::any([
        GltfSceneCollisionPredicate3d::NodeOrAncestorName(street),
    ]),
);
let collision = GltfSceneCollisionConfig3d::new([ground])?;
let config = GltfSceneLoadConfig::default()
    .with_semantic_collision(collision);

# fn use_loaded(loaded: &LoadedGltfScene, ground_id: &GltfSceneColliderLayerId3d) {
let ground_mesh = loaded
    .collider_layer(ground_id)
    .expect("required semantic layer")
    .mesh();
# let _ = ground_mesh;
# }
# let _ = config;
# Ok::<(), Box<dyn std::error::Error>>(())
```

Predicates поддерживают exact/prefix match для direct node, node-or-ancestor,
mesh и material names. Внутри selector используется явный `All` или `Any`;
recursive boolean/regex DSL намеренно отсутствует. `excluding(...)` задаёт
отдельный exclusion selector. Required layer с пустым результатом отклоняет
load, `.optional()` просто не публикуется.

`GltfSceneCollisionLimits3d` ограничивает layers, predicates, длину имён,
суммарные triangles и per-layer draws/primitives/vertices/triangles. Эти
лимиты проверяются до/во время worker build. `LowestElevation` в character
spawn выбирает нижнюю валидную поверхность уже внутри ground collider, но не
является navmesh reachability test.

## Shared task pool

Не создавайте отдельный pool для каждой streaming zone. Для нескольких
одновременных loads используйте application-owned pool:

```rust,no_run
use std::sync::Arc;
use yuyib::prelude::*;

let pool = Arc::new(TaskPool::new(TaskPoolConfig::new(4, 64)?)?);
let city = GltfSceneLoad::start_on(
    "assets/maps/city.glb",
    GltfSceneLoadConfig::default(),
    Arc::clone(&pool),
)?;
let interior = GltfSceneLoad::start_on(
    "assets/maps/interior.glb",
    GltfSceneLoadConfig::default(),
    pool,
)?;
# let _ = (city, interior);
# Ok::<(), Box<dyn std::error::Error>>(() )
```

`start_on` сохраняет bounded backpressure: заполненная task queue возвращает
`GltfSceneLoadStartError::Submit`, а не блокирует event loop.

## Progress UI

`GltfSceneLoadProgress` содержит:

- `stage`: `Queued`, `Reading`, `Processing`, `Ready`, `Failed` или `Taken`;
- точные `completed_work` и `total_work`;
- `fraction()` для progress bar.

UI принадлежит приложению. Загрузчик не создаёт окно, overlay или обязательный
стиль. Это позволяет одинаково использовать native UI, WebView либо игровой
loading screen.

Ошибка сохраняется после worker failure:

```rust,no_run
# use yuyib::prelude::*;
# let loading = GltfSceneLoad::start("assets/map.glb", GltfSceneLoadConfig::default())?;
if loading.progress().stage == GltfSceneLoadStage::Failed {
    if let Some(error) = loading.failure() {
        eprintln!("scene load failed: {error}");
    }
}
# Ok::<(), Box<dyn std::error::Error>>(() )
```

## Bounded GPU publication

После `take_ready` создайте standard high-level renderer и на каждом render
frame вызывайте короткий default path:

```rust,no_run
# use yuyib::prelude::*;
# fn render(
# frame: &mut yuyib::render::RenderFrame<'_>,
# loaded: &mut LoadedGltfScene,
# scene: &mut Game3dScene,
# ) -> Result<(), Box<dyn std::error::Error>> {
let gpu = loaded.prepare_for_frame(frame, scene)?;
if gpu.ready {
    let stats = loaded.render(frame, scene)?;
    println!("draw calls: {}", stats.draw.draw_calls);
} else {
    println!(
        "GPU textures: {}/{}, primitives: {}/{}",
        gpu.completed_texture_slots,
        gpu.total_texture_slots,
        gpu.completed_primitives,
        gpu.total_primitives,
    );
}
# Ok(())
# }
```

`prepare_for_frame` поддерживает `Game3dShading::Lambert` (default) и
`Game3dShading::Pbr`. Выберите shading на `Game3dSceneConfig` до первого вызова
publication: одна prepared-модель публикуется в cache выбранного route.
`Game3dShading::Unlit` возвращает
`Game3dSceneError::PreparedShadingUnsupported`; скрытого eager fallback нет.
Если сменить Lambert/PBR route после начала publication, facade возвращает
`PreparedShadingChanged`: prepared texture ownership уже принадлежит первому
route, поэтому скрытое дублирование VRAM не допускается.

Для PBR достаточно заменить policy, остальной loading loop остаётся тем же:

```rust,no_run
# use yuyib::prelude::*;
# fn setup() -> Result<(), Box<dyn std::error::Error>> {
let scene = Game3dScene::new(
    "assets",
    Game3dSceneConfig::default().with_shading(Game3dShading::Pbr),
)?;
# let _ = scene;
# Ok(())
# }
```

Default `ModelUploadBudget3d` публикует до 4 texture slots, целится в 16 MiB
unique decoded texture bytes, допускает до 8 primitives и целится в 8 MiB
исходных geometry streams за frame. После профилирования лимиты можно заменить
явно:

```rust,no_run
# use yuyib::prelude::*;
# fn render(frame: &mut yuyib::render::RenderFrame<'_>, loaded: &mut LoadedGltfScene, scene: &mut Game3dScene) -> Result<(), Box<dyn std::error::Error>> {
let budget = ModelUploadBudget3d {
    maximum_texture_slots: 2,
    target_texture_bytes: 8 * 1024 * 1024,
    maximum_primitives: 4,
    target_geometry_bytes: 2 * 1024 * 1024,
};
let progress = loaded.prepare_for_frame_with_budget(frame, scene, budget)?;
# let _ = progress;
# Ok(())
# }
```

Texture/primitive counts дают простой `fraction()` для UI. Per-call byte
counters остаются точными отдельно. Duplicate texture slots переиспользуют GPU
resource и не расходуют byte budget. Если одна texture или primitive больше
своего byte target, она всё равно публикуется целиком как единственная такая
operation кадра, а соответствующий `uploaded_oversized_*` становится `true`;
иначе загрузка крупного валидного ресурса могла бы зависнуть навсегда.

## Что возвращает LoadedGltfScene

- `world()` / `world_mut()` — ECS scene;
- `models()` — typed model assets;
- `spawned()` / `model()` — identity импортированной сцены;
- `bounds()` — worker-calculated world bounds;
- `collider()` — optional static triangle collider;
- `prepare_for_frame` / `render` — короткий standard rendering path.

Gameplay остаётся отдельным. Например, character controller можно создать из
`loaded.collider()`, а camera перед frame передать через
`*game_scene.camera_mut() = controller.camera()`.

## Low-level escape hatch

Если нужен собственный cooker, несколько dependency jobs, нестандартная GPU
publication или custom shader pipeline, используйте напрямую:

- `ImporterRegistry<ImportedAsset>` и `GltfAssetImporter`;
- `AssetLoadQueue` на общем `TaskPool`;
- `spawn_scene`;
- `ModelTextureLoader::prepare` и `PreparedModelTextures`;
- `LitSceneRenderer3d` либо raw render graph pass.

High-level facade не удаляет эти APIs и не меняет их ownership.

## Limits & Caveats

- Default encoded-source limit равен 64 MiB. Настраивайте его per load через
  `with_importer_registry_limits`; это не изменяет `ImportOptions::limits`.
- Registry path принимает self-contained GLB или glTF с data-URI buffers.
  External buffers требуют отдельного host resolver.
- Texture decode идёт в worker; GPU texture slots и geometry primitives имеют
  отдельные per-frame limits.
- Primitive — текущая атомарная единица WGPU upload. Огромный единый primitive
  всё ещё может дать frame spike; режьте его в cooker или проверяйте
  `uploaded_oversized_primitive` до появления chunked buffer initialization.
- Нет cancellation/preemption уже выполняющейся задачи.
- Один `GltfSceneLoad` представляет один request и позволяет взять result один
  раз.
- Bounded prepared publication поддерживает Lambert и PBR. Unlit остаётся
  eager preview route.
- Skeletal textures публикуются bounded, но skeletal geometry пока создаётся
  одним атомарным high-level шагом.
