# 2D: движение sprite и столкновения с tilemap

> **Статус:** Experimental  
> **Crate:** yuyib::game_2d  
> **Основа:** yuyib::physics::resolve_kinematic_aabb_2d

`yuyib-game-2d` переводит рядовой `TileCollision2d` в обычные неизменяемые
`StaticAabb2d`. Сам crate physics намеренно не знает ни о tile grid, ни об ECS,
ни о пикселях: его можно использовать и для другого 2D представления мира.

## Готовый контроллер персонажа

Для top-down игры обычно не нужно вручную умножать оси WASD на скорость,
строить AABB и записывать позицию назад в `Sprite2d`. Положите рядом со
sprite `KinematicSpriteController2d` и на каждом fixed tick вызовите одну
функцию:

~~~rust
use std::time::Duration;
use yuyib::game_2d::{
    KinematicSpriteController2d, Sprite2d, SpriteMoveInput2d,
    TileKinematicAabbLimits2d, step_kinematic_sprite_controller_2d,
};

let player = world.spawn((
    sprite,
    KinematicSpriteController2d::new([20.0, 30.0], 220.0)?,
)).id();

// Клавиатура, gamepad или сеть приводятся приложением к одной смысловой оси.
let input = SpriteMoveInput2d::new([right - left, down - up])?;
let step = step_kinematic_sprite_controller_2d(
    &mut world,
    player,
    input,
    Duration::from_millis(16),
    TileKinematicAabbLimits2d::new(4_096)?,
)?;
assert_eq!(step.actor, player);
# Ok::<(), Box<dyn std::error::Error>>(())
~~~

Диагональ автоматически нормализуется: две зажатые клавиши не ускоряют
персонажа. Функция использует все сущности с парой `TileMap2d` +
`TileCollision2d`, поэтому collision layers можно собирать из нескольких map.
Она не имеет скрытого clock, keyboard binding или камеры — эти policy остаются
у приложения.

Полный runnable пример: `cargo run -p yuyib --example two_d_tile_playground`.
Он показывает один atlas, animation, camera follow, WASD/стрелки и стены.

## Средний уровень: один запрос движения

Если actor не является `Sprite2d`, либо его размер зависит от состояния,
используйте adapter напрямую. Он превращает текущую ECS-карту в colliders и
возвращает контакты с координатами tile:

~~~rust
use yuyib::game_2d::{
    TileKinematicAabbLimits2d, resolve_kinematic_tilemap_aabb_2d,
};
use yuyib::physics::{Aabb2d, Vec2};

let actor_box = Aabb2d::new(Vec2::new(6.0, 8.0))?;
let move_result = resolve_kinematic_tilemap_aabb_2d(
    &mut world,
    Vec2::new(20.0, 30.0),
    actor_box,
    Vec2::new(4.0, 0.0),
    TileKinematicAabbLimits2d::new(4_096)?,
)?;

for contact in move_result.contacts() {
    println!(
        "hit map {:?}, tile ({}, {})",
        contact.tile.entity, contact.tile.column, contact.tile.row
    );
}
# Ok::<(), Box<dyn std::error::Error>>(())
~~~

Результат содержит `final_center`, реально применённый `applied_delta` и
контакты с tile в детерминированном порядке X sweep, затем Y sweep. Координаты
tile растут вправо/вниз; позиция map — её верхний левый угол. Не существует
неявного перевода «пиксели в метры»: единицу мира выбирает игра.

## Низкий уровень: snapshot и общий physics solver

Используйте `extract_tile_collisions_2d`, если игра сама кэширует snapshot,
фильтрует пространство или применяет иной solver. Передайте результат в
`build_tile_static_colliders_2d`; порядок сохраняется, а ключи общих
`StaticAabb2d` идут подряд. `TileStaticCollider2d::source()` связывает physics
box обратно с entity/row/column.

This separation keeps tilemaps modular: yuyib-game-2d may depend on
yuyib-physics; yuyib-physics never takes a tilemap dependency.

## Лимиты и ошибки

TileKinematicAabbLimits2d::new(max_static_colliders) rejects zero and enforces
one maximum at all three steps:

1. ECS rectangle extraction;
2. snapshot-to-static-AABB conversion;
3. generic kinematic resolution.

Exceeding the world extraction budget returns
TileKinematicAabbError2d::Extraction(LimitExceeded). An already-created
snapshot over the budget returns SnapshotLimitExceeded; invalid tile
geometry/finite bounds identifies entity, row and column in
InvalidTileCollider. Generic solver failures, including initial overlap,
remain explicit under TileKinematicAabbError2d::Physics.

## Limits & Caveats

Это static AABB collision для playable прототипа, а не полный character
controller:

- Each call rebuilds its bounded static list; it has no cache, chunk broadphase
  or streaming residency.
- The physics path sweeps X then Y and supports axis-aligned wall sliding only.
  It has no dynamic bodies, rotation, slopes, one-way platforms, arbitrary
  trajectory CCD, depenetration, triggers or network authority.
- A mover that starts in strict overlap returns an error. The game must choose
  spawn validation or a depenetration policy explicitly.

Для raw solver см. [Physics](physics.md). Для authoring tiles и renderer
snapshot см. [tilemaps](tilemaps.md).
