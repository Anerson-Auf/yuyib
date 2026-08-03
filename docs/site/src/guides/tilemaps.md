# 2D: tilemaps and viewport culling

> **Статус:** Experimental  
> **Модуль:** `yuyib::game_2d`  
> **Связанный import:** `yuyib::tiled` (Tiled JSON/TMX, bounded LDtk)

## Когда использовать

Нужна ортогональная tile-карта: визуальные слои + optional collision, с
отсечением тайлов вне камеры на CPU. Для «просто один sprite» хватит
[`Sprite2d`](sprites-and-animation.md). Для готового playable loop —
[tutorial 2D](../tutorials/first-2d-playable.md).

## Модель данных

`TileMap2d` — ECS component: row-major grid, **один** texture atlas на map
layer instance. Пустая клетка — явное empty, не «мусорный gid».

```text
authoring (Tiled/LDtk/ручной fill)
        ↓
 TileMap2d (+ optional TileCollision2d)
        ↓
 extract_tiles_2d / extract_tiles_chunked_2d(viewport)
        ↓
 Game2dScene painter order + GPU batches
```

### Почему extract, а не «renderer читает World»

Extraction строит **owned CPU snapshot**. Renderer не держит borrow на `World`
и не зависит от ECS layout. Это тот же invariant, что в 3D: CPU state ≠ GPU
residency.

## Ключевые API

| API | Возвращает / делает | Зачем |
|---|---|---|
| `TileMap2d` | Component сетки | Authoring-time validation регионов |
| `TileViewport2d` | Окно видимости в tile/world space | Caller задаёт camera policy |
| `extract_tiles_2d(world, viewport)` | Snapshot видимых тайлов | Без chunk overhead на малых картах |
| `extract_tiles_chunked_2d(..., config)` | То же + chunk traverse + draw budget | Большие карты; structured error при budget |
| `TileMap2d::with_animation` + `step_tile_map_animations_2d` | Shared timeline на non-empty cells | Вода/лавы без per-cell state machine |
| `TileCollision2d` + `extract_tile_collisions_2d` | Детерминированные world AABB | Вход в kinematic / Rapier, не navmesh |

Порядок extract детерминирован: **layer → entity id → row → column**. Так
сохраняется transparent painter order; глобальный merge по texture намеренно
не делается.

## Import из Tiled / LDtk

`yuyib-tiled` даёт `ImportedTiledMap` → bind texture → `TileMap2d` + objects.

```powershell
cargo run -p yuyib --example tiled_map_2d_smoke
cargo run -p yuyib --example two_d_tiled_playable
```

Out of scope текущего slice: hex/iso, infinite maps, base64 compressed TMX,
external LDtk levels. Interpretation object classes — game plugin, не importer.

## Collision path

1. `TileCollision2d` помечает solid cells.
2. `extract_tile_collisions_2d` → AABB list (viewport-independent, bounded).
3. Дальше: [`KinematicSpriteController2d`](tilemap-kinematic-physics.md) или
   Rapier 2D / `PlatformerController2d`.

Visual tile и solid cell независимы: можно иметь невидимую стену.

## Limits & Caveats

- Один atlas на component instance; multi-texture карты — несколько entities/layers.
- Нет GPU tile streaming / megatexture.
- Chunk extractor — CPU culling, не residency API.
- `Game2dScene` сам вызывает extraction; raw extract нужен для custom pipelines.

## См. также

- [Tilemap kinematic collision](tilemap-kinematic-physics.md)
- [Game2dScene](game-2d-scene.md)
- [Tutorial 2D](../tutorials/first-2d-playable.md)
