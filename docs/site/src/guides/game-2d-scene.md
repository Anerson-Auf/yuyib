# 2D: high-level `Game2dScene`

> **Статус:** Experimental  
> **Crate / module:** `yuyib::game_2d`  
> **Уровень:** high-level

`Game2dScene` закрывает типичный render path одной точкой входа: bounded GPU
texture upload, camera viewport, ECS extraction sprites и tilemaps, culling,
painter ordering, batching и diagnostics. Он не читает файлы и не декодирует
изображения на render thread: это остаётся задачей `AssetServer`/worker pool.

```rust,no_run
use yuyib::{
    ecs::prelude::World,
    game_2d::{Game2dScene, Sprite2d},
    image::{DecodePolicy, decode_path},
    two_d::{PixelPoint, TextureRegion},
};

# fn prepare(world: &mut World, texture: yuyib::two_d::TextureHandle)
# -> Result<Game2dScene, Box<dyn std::error::Error>> {
let image = decode_path("assets/player.png", DecodePolicy::default())?;
let size = image.texture().size();
let region = TextureRegion::new(texture, size, PixelPoint::default(), size)?;

let mut scene = Game2dScene::default();
scene.queue_texture(texture, image)?;
world.spawn(Sprite2d::new(region).with_size([64.0, 64.0]));
# Ok(scene)
# }
```

В `Application::on_render` достаточно обновить камеру и вызвать:

```rust,ignore
scene.camera_mut().position = player_position;
let stats = scene.render(frame, &mut world)?;
```

`Game2dSceneStats` сообщает visible/drawn counts, draw calls, uploaded bytes,
pending/resident textures, пропуски из-за ещё не resident texture и исчерпания
draw-call budget. Поэтому graceful degradation не скрыта.

## Limits & Caveats

- `TextureCacheConfig2d` ограничивает resident texture count, decoded pending
  bytes и soft upload bytes per frame. Один oversized первый upload допускается,
  иначе FIFO queue могла бы навсегда остановиться.
- `DrawBudget2d` ограничивает ordinary sprites, tiles и texture-batch draw calls.
- `Game2dSceneConfig::camera` определяет одновременно culling viewport и GPU
  projection; `tile_chunk_size` управляет CPU traversal.
- На одинаковом `layer` tiles идут перед ordinary sprites. Между разными
  layers сохраняется глобальный painter order, поэтому texture может дать
  несколько batches — это корректная цена transparent ordering.
- Неresident texture не вызывает panic: соответствующие draws попадают в
  `missing_texture_draws`.

Полный runnable use-case с atlas animation, camera follow, WASD и tile
collision: `cargo run -p yuyib --example two_d_tile_playground`.

Низкоуровневый escape hatch остаётся доступен через `SpriteRenderer`,
`extract_visible_sprites_2d`, `extract_tiles_chunked_2d` и `RenderGraph`.
