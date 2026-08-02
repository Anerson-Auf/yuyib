# 3D: static navigation queries

`WalkableSurface3d` — immutable navigation snapshot over static triangle-map
geometry. It extracts upward-facing triangles, connects compatible shared edges
and provides bounded nearest-point, reachability and coarse path queries.

Полный headless example:
`crates/yuyib/examples/static_navigation_queries.rs`.

```powershell
cargo run -p yuyib --example static_navigation_queries
```

## High-level: из ECS scene collider

После загрузки статической карты соберите collision snapshot, затем navigation
snapshot. Оба результата immutable и не наблюдают за последующими изменениями
ECS world.

```rust
use yuyib::game_3d::{
    WalkableSurfaceConfig3d, build_static_scene_collider_3d,
};

let collider = build_static_scene_collider_3d(&mut world, &models)?;
let config = WalkableSurfaceConfig3d::new(
    60.0_f32.to_radians(), // maximum slope from world up
    0.30,                  // maximum step between shared-edge endpoints
    0.001,                 // XZ edge matching tolerance
    1.0,                   // nearest-point spatial-grid cell size
)?;
let navigation = collider.build_walkable_surface(config)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

Когда static geometry меняется, collider и navigation snapshot нужно явно
пересобрать в точке смены уровня. Это исключает скрыто устаревшие стены и paths.

## Low-level: из triangle mesh

Importer или procedural generator может обойти ECS extraction:

```rust
use yuyib::game_3d::WalkableSurface3d;

let navigation = WalkableSurface3d::from_triangle_mesh(&triangle_mesh, config)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

`TriangleMesh3d::from_indexed` остаётся строгой границей: non-finite vertices,
invalid indices и degenerate triangles возвращают typed error.

## Nearest point и telemetry

Каждый query имеет явный work budget. Он нужен, чтобы повреждённая или огромная
карта не превратила один gameplay request в unbounded CPU spike.

```rust
use yuyib::{
    game_3d::NearestWalkableQueryLimits3d,
    physics::Vec3,
};

let result = navigation.nearest_walkable_point(
    Vec3::new(2.0, 4.0, 1.0),
    5.0,
    NearestWalkableQueryLimits3d::new(256, 1024),
)?;

println!(
    "cells={}, candidates={}",
    result.stats.cells_visited,
    result.stats.candidates_tested,
);
if let Some(hit) = result.point {
    println!("projected={:?}, source triangle={}", hit.point, hit.source_triangle);
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

`None` — нормальный результат, когда поверхность не найдена внутри radius.
Invalid input и исчерпание limits возвращаются как `WalkableQueryError3d`.

## Reachability и path

`reachability` работает с stable walkable-triangle indices. `find_path` сначала
проецирует world-space endpoints и возвращает typed outcome:

- `Path(path)` — triangle corridor и coarse debug points;
- `StartNotFound` или `GoalNotFound` — endpoint не спроецирован;
- `Unreachable` — endpoints существуют, но принадлежат разным graph components.

No-path — ожидаемое значение, а не ошибка runtime:

```rust
use yuyib::game_3d::{WalkablePathOutcome3d, WalkablePathQueryLimits3d};

let result = navigation.find_path(
    start,
    goal,
    2.0,
    WalkablePathQueryLimits3d::default(),
)?;

match result.outcome {
    WalkablePathOutcome3d::Path(path) => follow(path.points()),
    WalkablePathOutcome3d::Unreachable => choose_another_goal(),
    WalkablePathOutcome3d::StartNotFound
    | WalkablePathOutcome3d::GoalNotFound => recover_to_walkable_surface(),
}
println!("visited={}", result.stats.graph.visited_triangles);
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Limits текущего foundation

- Path — deterministic BFS corridor, не shortest geometric path.
- Points не проходят radius-aware erosion или funnel smoothing.
- T-junctions и частично перекрывающиеся edges автоматически не соединяются.
- Dynamic obstacles и moving platforms не входят в static snapshot.
- Agent radius/height нужно проверять collision/controller layer отдельно.

Для production character movement используйте navigation как planning layer, а
`CharacterController3d` и `StaticSceneCollider3d` — как authoritative collision
layer. Это предотвращает прохождение через стену даже при coarse path.

