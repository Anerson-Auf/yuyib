# 3D-трансформации: позиция, поворот и размер модели

> **Статус:** Experimental  
> **Crate / module:** `yuyib::game_3d`, `yuyib::scene`  
> **Платформы:** platform-neutral  
> **Поисковые слова:** изменить размер модели, масштаб, scale, position,
> rotation, transform, glTF root

Эта страница отвечает на практический вопрос: **как переместить, повернуть или
изменить размер модели в игровом мире**. Для уже загруженной модели не нужно
менять вершины mesh. Меняется ECS-компонент трансформации.

Если кроме transform нужно изменить visibility, source mesh, render order или
material, откройте [cookbook 3D-объекта](3d-objects-transforms.md).

## Короткий ответ: увеличить отдельную модель

Если вы сами создали entity с `Model3d` и `Transform3d`, измените `scale`:

```rust
use yuyib::{
    assets::Assets,
    ecs::prelude::World,
    game_3d::{Model3d, Transform3d},
    model::{Model, PrimitiveError},
};

fn main() -> Result<(), PrimitiveError> {
    let mut models = Assets::new();
    let cube = models.insert(Model::cube(0.5)?);
    let mut world = World::new();

    // Модель в два раза больше по всем трём осям.
    let entity = world
        .spawn((Model3d::new(cube), Transform3d::IDENTITY.with_uniform_scale(2.0)))
        .id();

    // Позже, например из gameplay-системы: растянуть только по высоте.
    world
        .entity_mut(entity)
        .get_mut::<Transform3d>()
        .expect("model entity must have Transform3d")
        .scale = [2.0, 3.0, 2.0];

    Ok(())
}
```

`[2.0, 3.0, 2.0]` означает множитель по осям `[x, y, z]`. Это scale, а не
размер в метрах: итоговый размер зависит от исходной геометрии модели.

## Какой transform менять

| Ситуация | Менять | Не менять |
|---|---|---|
| Самостоятельная entity без parent | `Transform3d` | — |
| Узел импортированной glTF-сцены | `LocalTransform3d` | `WorldTransform3d` |
| Вся импортированная сцена | Добавить общий parent с `LocalTransform3d` | Каждый mesh по отдельности |
| glTF-узел с authored matrix | Добавить TRS-parent | Не добавлять второй local transform на тот же узел |
| Физический collider | Пересобрать или изменить collider отдельно | Не ожидать синхронизации от render transform |

`WorldTransform3d` — вычисляемый результат hierarchy propagation. Он доступен
только для чтения. Если записать его вручную, следующий propagation всё равно
пересчитает значение из local transforms.

## Изменить размер всей загруженной glTF-сцены

`LoadedGltfScene` может иметь несколько root nodes, а отдельные узлы могут
хранить точную matrix вместо TRS. Универсальный вариант — создать один parent,
подключить к нему все roots и масштабировать parent:

```rust,no_run
use yuyib::{
    game_3d::{
        LocalTransform3d, TransformHierarchyError, propagate_world_transforms,
        set_parent_3d,
    },
    render_3d::LoadedGltfScene,
};

fn set_scene_scale(
    loaded: &mut LoadedGltfScene,
    scale: [f32; 3],
) -> Result<(), TransformHierarchyError> {
    // Копируем IDs до mutable borrow ECS world.
    let roots = loaded.spawned().roots().to_vec();
    let scale_root = loaded
        .world_mut()
        .spawn(LocalTransform3d::IDENTITY.with_scale(scale))
        .id();

    for root in roots {
        set_parent_3d(loaded.world_mut(), root, scale_root)?;
    }
    propagate_world_transforms(loaded.world_mut())?;
    Ok(())
}
```

Вызовите функцию после `loading.take_ready()` и до первого render. Для
изменения scale во время игры сохраните `scale_root`, измените его
`LocalTransform3d`, затем один раз вызовите `propagate_world_transforms` в
определённой точке кадра:

```rust,no_run
# use yuyib::{ecs::prelude::{Entity, World}, game_3d::{LocalTransform3d, TransformHierarchyError, propagate_world_transforms}};
fn animate_scene_scale(
    world: &mut World,
    scale_root: Entity,
    uniform_scale: f32,
) -> Result<(), TransformHierarchyError> {
    let mut transform = world
        .get_mut::<LocalTransform3d>(scale_root)
        .expect("saved scale root must exist");
    transform.scale = [uniform_scale; 3];
    drop(transform);

    propagate_world_transforms(world)?;
    Ok(())
}
```

Не запускайте propagation после изменения каждого child. Сначала примените
все изменения кадра, затем выполните один проход.

## Позиция и поворот

Builder methods используют те же структуры:

```rust
use yuyib::game_3d::Transform3d;

let transform = Transform3d::IDENTITY
    .with_translation([10.0, 0.0, -4.0])
    // Quaternion в порядке [x, y, z, w]. Здесь identity rotation.
    .with_rotation([0.0, 0.0, 0.0, 1.0])
    .with_scale([1.5, 1.5, 1.5]);
# let _ = transform;
```

Yuyib не объявляет глобально, что одна world unit равна одному метру. Выберите
единицу проекта один раз и используйте её одинаково для моделей, камеры,
скорости и physics.

## Scale, bounds и collision — разные данные

Render transform автоматически влияет на извлечение и отрисовку модели, но не
перезаписывает snapshots, созданные во время загрузки:

- `LoadedGltfScene::bounds()` — исходные bounds, вычисленные worker во время
  load;
- `LoadedGltfScene::collider()` — исходный static collider;
- semantic collider layers — также worker-built snapshots.

После изменения сцены пересчитайте актуальные данные:

```rust,no_run
use yuyib::{
    assets::Assets,
    ecs::prelude::World,
    game_3d::{
        SceneBoundsError3d, SceneBoundsResult3d, SceneCollisionError3d,
        StaticSceneCollider3d, build_static_scene_collider_3d,
        scene_bounds_3d,
    },
    model::Model,
};

fn rebuild_spatial_data(
    world: &mut World,
    models: &Assets<Model>,
) -> Result<(SceneBoundsResult3d, StaticSceneCollider3d), SpatialRebuildError> {
    let bounds = scene_bounds_3d(world, models)?;
    let collider = build_static_scene_collider_3d(world, models)?;
    Ok((bounds, collider))
}

#[derive(Debug)]
enum SpatialRebuildError {
    Bounds(SceneBoundsError3d),
    Collision(SceneCollisionError3d),
}

impl From<SceneBoundsError3d> for SpatialRebuildError {
    fn from(value: SceneBoundsError3d) -> Self { Self::Bounds(value) }
}
impl From<SceneCollisionError3d> for SpatialRebuildError {
    fn from(value: SceneCollisionError3d) -> Self { Self::Collision(value) }
}
# fn main() {}
```

Для часто движущихся объектов не пересобирайте static triangle collider каждый
кадр. Используйте dynamic collider gameplay/physics path. Static scene
collider предназначен для уровня, который меняется редко.

## Частые ошибки

- **Модель не меняется:** вы изменили `WorldTransform3d`, а не authoring
  component, либо забыли вызвать `propagate_world_transforms`.
- **Hierarchy возвращает ошибку:** scale содержит `0`, `NaN` или infinity;
  quaternion имеет нулевую длину; parent graph содержит cycle.
- **Видимый размер изменился, collision остался прежним:** collider — отдельный
  snapshot, пересоберите его или храните dynamic collider.
- **Ошибка `ConflictingLocalTransforms`:** на entity одновременно находятся
  `LocalTransform3d` и `LocalMatrixTransform3d`. Используйте новый parent.
- **Scale `[2.0; 3]` не даёт размер `2`:** scale — множитель исходных bounds,
  а не абсолютная длина.

## API

| Задача | API |
|---|---|
| Поставить отдельную модель в world | [`Model3d`](../api/yuyib_game_3d/struct.Model3d.html) + [`Transform3d`](../api/yuyib_game_3d/struct.Transform3d.html) |
| Изменить local transform hierarchy node | [`LocalTransform3d`](../api/yuyib_game_3d/struct.LocalTransform3d.html) |
| Присоединить root к parent | [`set_parent_3d`](../api/yuyib_game_3d/fn.set_parent_3d.html) |
| Пересчитать world transforms | [`propagate_world_transforms`](../api/yuyib_game_3d/fn.propagate_world_transforms.html) |
| Найти entities импортированной сцены | [`SpawnedScene`](../api/yuyib_scene/struct.SpawnedScene.html) |
| Получить mutable ECS scene | `LoadedGltfScene::world_mut` в [`yuyib_render_3d`](../api/yuyib_render_3d/index.html) |

## Limits & Caveats

- Нулевой scale запрещён для hierarchy transforms; отрицательный scale
  зеркалит geometry.
- Non-uniform scale родителя вместе с rotation child может создать shear. Exact
  matrix сохранится, но `WorldTransform3d::as_trs()` вернёт `None`.
- Масштабирование geometry не меняет texture resolution, animation duration
  или скорость gameplay-контроллера.
- `LoadedGltfScene` не предоставляет setter для worker-built collider/bounds;
  после runtime mutation храните пересчитанные spatial values в состоянии
  игры.

## См. также

- [Загрузка glTF-сцены](streamed-gltf-scene.md)
- [Scene ECS и interactions](scene-ecs-and-interactions.md)
- [3D model assets](model-assets.md)
- [3D object cookbook](3d-objects-transforms.md)
- [Physics](physics.md)
