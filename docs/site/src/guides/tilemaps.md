# 2D: tilemaps and viewport culling

> **Статус:** Experimental  
> **Модуль:** `yuyib::game_2d`

`TileMap2d` is one ECS component for a row-major grid backed by one texture
atlas. Empty cells are explicit; atlas regions and bounds are validated at
authoring time. `extract_tiles_2d(world, viewport)` emits a CPU snapshot only
for tiles in a caller-provided `TileViewport2d`.

The ordering contract is deterministic: layer, then full entity ID, then row,
then column. It preserves transparent painter order rather than performing an
unsafe global texture merge.

Full API: [game 2D](../api/yuyib_game_2d/index.html).

## Limits & Caveats

This is CPU extraction for one atlas; no image decode, GPU upload/batching,
streaming chunks or collision/navmesh generation is included. Bounded viewport
culling for ordinary `Sprite2d` entities is a separate API; see
[ordinary sprite culling](sprite-viewport-culling.md). The camera/viewport
policy remains with the host application.

`TileMap2d::with_animation` provides one shared `SpriteAnimation` timeline for
all non-empty cells in a map layer. Advance it with
`step_tile_map_animations_2d(world, delta)` before extraction. The frames must
use the same atlas texture; independent per-cell timelines are intentionally
not allocated by this API.

`TileCollision2d` marks row-major solid cells and
`extract_tile_collisions_2d` returns a viewport-independent, deterministic
snapshot of world-space AABB rectangles (entity, row, column order). It is an
input to a chosen physics solver, not automatic collision response or navmesh
generation; `TileCollisionLimits` bounds generated rectangles. For the
bounded high-level static tile AABB adapter and contact-to-tile mapping, see
[tilemap kinematic collision](tilemap-kinematic-physics.md).

For large maps, `extract_tiles_chunked_2d(world, viewport, config)` visits only
chunks that intersect the viewport. `TileChunkConfig2d` validates chunk size
and a maximum draw budget; exceeding it returns a structured error rather than
silently allocating an unbounded snapshot. Output order remains identical to
the non-chunked extractor. It is CPU culling, not GPU streaming or residency.
