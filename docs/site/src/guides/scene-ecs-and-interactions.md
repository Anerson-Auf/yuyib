# Scene ECS, world transforms и `game.use`

> **Статус:** Experimental  
> **Модули:** `yuyib::scene`, `yuyib::game_3d`, `yuyib::gameplay::interaction_3d`

`yuyib-scene` materializes one selected `ImportedScene` into an ECS world
without flattening its node tree. `spawn_scene` creates `LocalTransform3d`,
`Parent3d`, model references and source metadata, then calls
`propagate_world_transforms` to publish `WorldTransform3d` and renderer-facing
`Transform3d`.

```rust,no_run
use yuyib::{assets::Assets, gltf::import_scene_path, model::Model, scene::*};

# let mut world = yuyib::ecs::prelude::World::new();
let imported = import_scene_path("assets/level.glb")?;
let mut models = Assets::<Model>::new();
let spawned = spawn_scene(&mut world, &mut models, &imported, SceneSelection::Default)?;
assert!(!spawned.roots().is_empty());
# Ok::<(), Box<dyn std::error::Error>>(())
```

Only one glTF root scene is selected intentionally. `SpawnedScene` preserves a
stable `NodeIndex → Entity` mapping; `SceneNode`, `SceneCamera` and
`SceneDirectionalLight` retain source metadata.

## Transform contract

Write authoring data to `LocalTransform3d`, create links through
`set_parent_3d`, then run `propagate_world_transforms` at the frame boundary.
The system validates the complete graph before writing derived output: cycles,
missing parents, zero scale and invalid quaternion values leave prior world
transforms intact rather than half-updating a scene.

## Границы сцены для камеры и загрузки

Не перебирайте вершины карты вручную в каждом примере. Готовая функция
`scene_bounds_3d` обновляет иерархию, учитывает точные матрицы glTF и выбранный
у сущности mesh, после чего возвращает границы в мировых координатах.

```rust,no_run
use yuyib::{
    assets::Assets,
    game_3d::{SceneBoundsResult3d, scene_bounds_3d},
    model::Model,
};

# let mut world = yuyib::ecs::prelude::World::new();
# let models = Assets::<Model>::new();
match scene_bounds_3d(&mut world, &models)? {
    SceneBoundsResult3d::Bounds(bounds) => {
        let centre = bounds.centre();
        let camera_distance = bounds.radius().max(10.0);
        // Разместите камеру относительно `centre`.
        let _ = (centre, camera_distance);
    }
    // Это обычное состояние: сцена пуста или нужные модели ещё загружаются.
    SceneBoundsResult3d::Empty => {}
}
# Ok::<(), yuyib::game_3d::SceneBoundsError3d>(())
```

Это высокоуровневый вариант для загрузки уровня и смены сцены. Если ваш
порядок ECS-систем уже вызвал `propagate_world_transforms`, используйте
`scene_bounds_3d_from_current_transforms`: он не выполняет второй обход
иерархии. Отсутствующий ресурс в `Assets` пропускается, чтобы границы можно
было пересчитать после фоновой подгрузки. Неверный номер mesh или повреждённая
геометрия возвращаются как явная ошибка, а не превращаются в случайную позицию
камеры.

## Статические стены карты

Не заменяйте коридор карты одной большой AABB: она перекрывает пустые места,
лестницы и проходы. Для уровня есть готовый высокий API
`build_static_scene_collider_3d`: он обновляет иерархию ECS и превращает
видимые треугольники импортированных моделей в один неподвижный коллайдер.

```rust,no_run
use yuyib::{
    assets::Assets,
    game_3d::build_static_scene_collider_3d,
    model::Model,
};

# let mut world = yuyib::ecs::prelude::World::new();
# let models = Assets::<Model>::new();
let level_walls = build_static_scene_collider_3d(&mut world, &models)?;
// В фиксированном шаге контроллер вызывает level_walls.mesh().resolve_sphere(...).
assert!(level_walls.triangle_count() > 0);
# if level_walls.skipped_degenerate_triangle_count() > 0 {
#     eprintln!("В карте пропущены пустые декоративные лица");
# }
# Ok::<(), yuyib::game_3d::SceneCollisionError3d>(())
```

Коллайдер — снимок уровня, а не компонент, автоматически следящий за миром.
После перемещения, удаления или догрузки частей уровня соберите его заново.
Отсутствующая модель здесь является ошибкой: тихо пропустить её означало бы
дать игроку пройти сквозь видимую стену.

Импортированные карты нередко несут нулевые декоративные лица — например, с
двумя одинаковыми вершинами. Этот высокий API исключает их до создания
`TriangleMesh3d`; количество доступно через
`skipped_degenerate_triangle_count()`. Низкоуровневый
`TriangleMesh3d::from_indexed` по-прежнему строго отвергает такие данные: это
полезно, когда вы сами создаёте физическую сетку и хотите заметить ошибку.

Если вы уже сами определили границу кадра, используйте низкоуровневый вариант
`build_static_scene_collider_3d_from_extracted(&extract_models(...), &models)`.
Для процедурной геометрии, отдельных прямоугольников или собственного
ускорения используйте базовый `TriangleMesh3d::from_indexed`; он принимает
обычные вершины и индексы, без ECS и загрузчика моделей. Внутренний compact
static BVH не дублирует vertices/AABB и отсекает далёкие faces до exact query;
`resolve_sphere` при этом сохраняет deterministic source-order semantics.
`acceleration_stats`, `raycast_with_stats` и `resolve_sphere_with_stats`
показывают размер индекса и реальное количество проверенных candidates.

## Semantic 3D interaction

`request_use_raycast_3d` maps a started `game.use` action plus `Ray3d` to a
command-like `InteractionRequested`. It ignores actor's own collider and uses
deterministic nearest sphere raycast. A closer non-interactable collider blocks
a target behind it; authority/game rules still decide `InteractionResolved`.

## Limits & Caveats

- No `Children` cache, automatic despawn propagation, scene prefabs, animation
  blending or camera/light world-direction synchronization yet.
- `SceneDirectionalLight` intentionally is source metadata, not automatic
  `DirectionalLight3d`: parent rotation must be applied explicitly first.
- Current interaction raycast is O(n), sphere-only and has no mesh visibility,
  controller, input binding or networking policy.
- Holding use does not repeat requests; a new Started transition is required.

Full API: [scene adapter](../api/yuyib_scene/index.html),
[game 3D](../api/yuyib_game_3d/index.html),
[gameplay](../api/yuyib_gameplay/index.html) and
[physics](../api/yuyib_physics/index.html).
