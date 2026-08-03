# Tutorial: первый 2D playable

> **Статус:** Experimental  
> **Requires:** features `app` + `two-d`  
> **Цель:** понять цепочку texture → `Sprite2d` / `TileMap2d` → `Game2dScene::render`

Предыдущие шаги: [окно](first-window.md), [игра](first-game.md).  
3D карта — отдельный путь: [glTF](load-gltf-scene.md).

Runnable:

```powershell
cargo run -p yuyib --example two_d_tile_playground
cargo run -p yuyib --example playable_loop_2d_smoke
cargo run -p yuyib --example two_d_tiled_playable
```

## 1. Модель 2D в Yuyib (коротко)

```text
файл PNG/atlas  →  decode (CPU)  →  TextureHandle + TextureRegion
                                      ↓
                              ECS: Sprite2d / TileMap2d / AnimatedSprite2d
                                      ↓
                         Game2dScene::queue_texture + render(frame, world)
                                      ↓
                              extract → cull → batch → GPU sprites
```

**Почему столько шагов?**  
Чтобы render thread не читал диск и не декодировал JPEG «между кадрами». Decode/import — workers / `on_start`; GPU upload — bounded queue внутри `Game2dScene`.

## 2. Минимальный sprite path

```rust,no_run
use yuyib::{
    ecs::prelude::World,
    game_2d::{Game2dScene, Sprite2d},
    image::{DecodePolicy, decode_path},
    two_d::{PixelPoint, TextureRegion},
};

fn spawn_player(
    world: &mut World,
    scene: &mut Game2dScene,
    texture: yuyib::two_d::TextureHandle,
) -> Result<(), Box<dyn std::error::Error>> {
    // 1) Decode на CPU. Политика лимитов — DecodePolicy, не «прочитать как получится».
    let image = decode_path("assets/player.png", DecodePolicy::default())?;
    let size = image.texture().size();

    // 2) Region — validated sub-rectangle атласа/текстуры.
    let region = TextureRegion::new(texture, size, PixelPoint::default(), size)?;

    // 3) Поставить decoded bytes в очередь GPU upload (не upload «прямо сейчас навсегда»).
    scene.queue_texture(texture, image)?;

    // 4) Entity со sprite в world space (Y вниз, как у 2D renderer).
    world.spawn(
        Sprite2d::new(region)
            .with_position([64.0, 64.0])
            .with_size([32.0, 32.0]),
    );
    Ok(())
}
```

### Почему эти типы

| API | Роль | Почему не иначе |
|---|---|---|
| `decode_path` | CPU decode с budgets | Не `image` crate напрямую в render callback |
| `TextureHandle` | Stable id текстуры | Не raw GPU id / path string в ECS |
| `TextureRegion` | UV-прямоугольник | Один atlas → много sprites без копий GPU texture |
| `Sprite2d` | ECS component | Extraction читает world; renderer получает snapshot |
| `Game2dScene::queue_texture` | Bounded residency | Oversized upload не блокирует FIFO навсегда без диагностики |
| `scene.render(frame, &mut world)` | Весь HL path | Не собирать cull/batch/painter order вручную в первом prototype |

`render` возвращает `Game2dSceneStats`: сколько sprites видно, сколько draw calls, сколько bytes залито, сколько draws пропущено из‑за ещё не resident texture. **Graceful degradation видима**, а не «пропал персонаж без причины».

## 3. Добавить tilemap

```rust,no_run
use yuyib::game_2d::TileMap2d;
use yuyib::two_d::TextureRegion;

# fn demo(regions: Vec<TextureRegion>) -> Result<(), Box<dyn std::error::Error>> {
// grid = [width, height] в тайлах; tiles — row-major, None = пустая клетка.
let width = 8_u32;
let height = 6_u32;
let tiles: Vec<Option<u32>> = (0..width * height)
    .map(|i| if i % 3 == 0 { None } else { Some(0) })
    .collect();
let map = TileMap2d::new([width, height], [16.0, 16.0], regions, tiles)?;
# let _ = map;
# Ok(())
# }
```

На практике заполнение чаще идёт из `yuyib-tiled` (Tiled JSON/TMX, LDtk) →
`ImportedTiledMap` → bind texture → `TileMap2d` + optional `TileCollision2d`.

| API | Зачем |
|---|---|
| `TileMap2d` | Один component = один слой сетки на atlas |
| `extract_tiles_2d` / chunked variant | CPU snapshot только видимого viewport |
| `TileCollision2d` | Solid cells → AABB snapshot для kinematic / Rapier |
| `KinematicSpriteController2d` | Top-down движение с wall slide |
| `PlayableLoop2d` | HL: input + camera follow + step + draw |

**Почему collision отдельным component?**  
Render tiles и solid collision — разные concerns (как `render3d` / `collision3d` в 3D authoring). Пустой visual tile может быть solid, и наоборот.

## 4. Рекомендуемый HL stack для прототипа

1. `Application` или `Game` — host.
2. `Game2dScene` — render facade.
3. `PlayableLoop2d` / `PlatformerPlayable2d` — если нужен готовый loop (см. `yuyib::profile_2d`).
4. `yuyib-tiled` — если карта из Tiled/LDtk, а не ручной `TileMap2d::filled`.

Не начинайте с raw `SpriteRenderer` + ручного `RenderGraph`, пока HL path не упёрся в limit.

## 5. Animation

`AnimatedSprite2d` + `step_sprite_animations_2d(world, delta)`:

- frames = список `TextureRegion` (atlas или отдельные textures — один API);
- `delta` задаёт **host** (обычно `FixedTime` или `GameTime`);
- система не читает wall clock и не трогает GPU.

Подробнее: [ECS animation](../guides/ecs-sprite-animation.md), [SpriteAnimator2d](../guides/sprites-and-animation.md).

## Limits & Caveats

- `Game2dScene` не читает файлы сам.
- Painter order: layer → entity id; texture batching не ломает transparent order ценой лишних batches.
- 2D Editor authoring schemas пока `Unavailable` в coverage — runtime path есть, visual Editor slice позже.

## См. также

- [Game2dScene](../guides/game-2d-scene.md)
- [Tilemaps](../guides/tilemaps.md)
- [Tilemap kinematic physics](../guides/tilemap-kinematic-physics.md)
- [2D concepts](../concepts/two-d.md)
- Use-case index: [2D](../wiki/use-case-index.md#2d)
