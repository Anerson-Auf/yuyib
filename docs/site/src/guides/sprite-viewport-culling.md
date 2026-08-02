# 2D: viewport culling for ordinary sprites

> **Статус:** Experimental  
> **Модуль:** `yuyib::game_2d`

`extract_sprites(world)` остаётся совместимым full-world extractor. Когда
render pass должен получить только обычные `Sprite2d`, пересекающие видимую
область, используйте bounded `extract_visible_sprites_2d`:

```rust
use yuyib::game_2d::{
    SpriteExtractionLimits2d, SpriteViewport2d, extract_visible_sprites_2d,
};

let viewport = SpriteViewport2d::new([camera_left, camera_top], [width, height])?;
let limits = SpriteExtractionLimits2d::new(16_384)?;
let snapshot = extract_visible_sprites_2d(&mut world, viewport, limits)?;

for batch in snapshot.batches() {
    // Передайте only this adjacent-texture batch в renderer.
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Contract

- `SpriteViewport2d` requires finite `origin`, finite positive `size` and a
  finite `origin + size`; its origin is top-left, as in the rest of the 2D
  stack.
- Negative `Sprite2d::size` mirrors rendering, but culling always uses its
  absolute width/height.
- Rotation is conservatively covered by an axis-aligned box with extents
  `abs(cos(r))*half_width + abs(sin(r))*half_height` and the analogous Y
  expression. A sprite can therefore be retained even where its rotated
  quad's exact corners would not intersect the viewport; it is never rejected
  because an unrotated rectangle was used.
- Rectangles use strict intersection. A sprite that only touches a viewport
  edge is outside. This lets neighbouring viewports divide the plane without
  rendering an edge-only sprite twice.
- Output is sorted by `layer`, then full ECS entity ID. Batches merge only
  adjacent equal textures, preserving transparent painter order.

## Limits & Caveats

`SpriteExtractionLimits2d::new` rejects zero. Its maximum bounds the visible
snapshot and is checked before the next visible draw is added. Exceeding it
returns `VisibleSpriteExtractError::VisibleSpriteLimitExceeded`; no partial
snapshot is returned.

`VisibleSpriteExtractError::InvalidSpriteGeometry` identifies the owning ECS
entity when position, size, rotation, or the derived conservative AABB is
non-finite. Fix or remove that component; silently treating invalid geometry
as off-screen can conceal simulation bugs.

This is CPU extraction only. It does not choose a camera transform, maintain a
spatial index, stream textures, perform occlusion, upload GPU resources, or
make a draw call. The host owns those policies. For atlas-backed tile grids and
chunk traversal, see [tilemaps](tilemaps.md).
