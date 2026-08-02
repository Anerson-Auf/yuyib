# Physics: быстрый playable prototype

> **Статус:** Experimental  
> **Crate / module:** `yuyib::physics`  
> **Платформы:** platform-neutral computation; ECS integration использует Yuyib ECS

`physics` — намеренно маленький, deterministic foundation для первого
playable slice. Он интегрирует linear velocity, проверяет 2D/3D sphere overlap
и даёт ECS queries. Это не замаскированный general-purpose physics engine.

## Быстрый 2D контакт

```rust
use yuyib::physics::{Body2d, Circle, Vec2, collide_circles};

let mut player = Body2d::new(Vec2::ZERO, Vec2::new(2.0, 0.0));
player.step(0.5)?;

let wall = Vec2::new(1.5, 0.0);
let contact = collide_circles(player.position, Circle::new(1.0)?, wall, Circle::new(1.0)?);
assert!(contact.is_some());
# Ok::<(), yuyib::physics::PhysicsConfigError>(())
```

`Vec2` и `Vec3` измеряются в world units; глобального conversion в pixels или
metres нет. Игра должна зафиксировать свою шкалу до добавления content.

`Aabb2d`/`AabbCollider2d` add strict axis-aligned box contacts, point tests,
bounded overlap extraction and deterministic `Ray2d` queries. Touching faces
are not an overlap; a ray starting inside returns distance zero. ECS results
use entity-ID ordering for ties. `yuyib-game-2d` optionally converts bounded
tile collision snapshots into `StaticAabb2d` without coupling physics to
tilemaps; see [tilemap kinematic collision](tilemap-kinematic-physics.md).

`resolve_kinematic_aabb_2d` moves one AABB against caller-keyed immutable
`StaticAabb2d` colliders. It sweeps X then Y, prevents axis-aligned tunnelling,
supports wall sliding and returns applied displacement plus ordered contact
normals. Work limits, duplicate keys and initial overlap are explicit errors.
This is a predictable tile/character prototype resolver, not a dynamic rigid
body solver or general diagonal time-of-impact implementation.

## Статичный индекс коробок для карты

`StaticAabbBroadphase2d` — готовый индекс для большого набора неподвижных
стен, тайлов и триггерных областей. Это не симуляция физики и не универсальная
структура для движущихся тел: он хранит снимок карты, упорядоченный по X, и
быстро отбрасывает заведомо далёкие коробки.

Высокоуровневый вариант сразу возвращает точный результат пересечения луча
или строгого overlap. Для собственного кода есть низкоуровневые методы
`candidate_keys_in_region` и `candidate_keys_for_ray`: они отдают только
стабильные `u64`-ключи кандидатов по возрастанию. Кандидат луча — ещё не
попадание: он выбран по прямоугольнику отрезка и требует вашего точного теста,
если вы не используете готовый `raycast`.

```rust
use yuyib::physics::{
    Aabb2d, Ray2d, StaticAabb2d, StaticAabbBroadphase2d,
    StaticAabbBroadphaseLimits2d, Vec2,
};

let wall = StaticAabb2d::new(
    100,
    Vec2::new(5.0, 0.0),
    Aabb2d::new(Vec2::new(1.0, 3.0))?,
)?;
let limits = StaticAabbBroadphaseLimits2d::new(10_000, 1_000)?;
let mut map = StaticAabbBroadphase2d::build([wall], limits)?;

let ray = Ray2d::new(Vec2::ZERO, Vec2::new(1.0, 0.0))?;
assert_eq!(map.raycast(ray, 20.0)?.unwrap().collider_key, 100);

// При загрузке следующего участка карты замените снимок атомарно.
map.rebuild([wall])?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

`build` и `rebuild` проверяют повторяющиеся ключи и не оставляют наполовину
собранный индекс. `insert`, `update` и `remove` подходят для редких правок
редактором; частое перемещение тысяч объектов требует отдельного динамического
индекса. Лимиты числа коробок и кандидатов обязательны. При их превышении
возвращается явная ошибка, а не непредсказуемое выделение памяти. Касание
границы входит в низкоуровневый список кандидатов, но не считается строгим
overlap в `overlaps_in_region`.

## ECS path

Для простого 2D мира добавьте к entity `Position2d`, `Velocity2d` и
`CircleCollider`, затем вызывайте `step_ecs_2d` один раз на fixed timestep.
Функция возвращает `Collision2d` с entity-pairs, а не применяет gameplay
effects сама: урон, звук, interaction и network authority остаются отдельными
systems.

## 3D interaction queries

`Position3d`, `Velocity3d`, `SphereCollider3d` и `step_ecs_3d` дают
detection-only simulation. `Ray3d` with `raycast_spheres_3d` returns nearest
hit; equal distances break ties by generational entity ID. `overlap_spheres_3d`
returns sorted volume hits. Pass `ignored: Some(actor)` to exclude the entity
that initiated an interaction ray.

`Aabb3d`/`AabbCollider3d` add axis-aligned volume queries for simple level
geometry. `raycast_aabbs_3d` has the same deterministic nearest-hit then
entity-ID tie rule and supports self exclusion; a ray starting inside a box
returns distance `0`. `overlap_aabbs_3d` is strict: touching faces are not an
overlap. Use `point_in_aabb_3d` for explicit point containment.

## Limits & Caveats

- `Circle` и `Sphere` требуют finite positive radius; некорректное значение
  возвращает `PhysicsConfigError`.
- Delta time должен быть finite и неотрицательным. Для reproducible behavior
  используйте фиксированный timestep, а не frame delta.
- Нет rotation, angular velocity, joints, continuous collision detection,
  broadphase/spatial index, solver response, mesh raycast, trigger policy или
  network authority. Быстрые тела могут tunneling сквозь collider.
- Общие 3D ECS queries остаются O(n), detection-only и покрывают sphere/AABB.
  Отдельный `TriangleMesh3d::resolve_sphere` даёт статическую mesh collision
  для персонажа и сначала консервативно отсеивает далёкие треугольники по
  AABB. Это не полноценный broadphase: для огромного или стримящегося мира
  создайте пространственный индекс и подключите его через
  `CharacterController3d::step_with_collision`.

Полный список функций, методов и ошибок — во встроенном
[Rust API reference](../api/yuyib_physics/index.html).
