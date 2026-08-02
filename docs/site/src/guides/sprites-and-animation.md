# 2D: sprites и animation

> **Статус:** Experimental  
> **Crate / module:** `yuyib::two_d`, `yuyib::image`, `yuyib::render_2d`  
> **Платформы:** Windows для GPU path

2D stack разделён на три слоя: `two_d` описывает metadata/frames, `image`
декодирует approved image formats с budgets, а `render_2d` загружает RGBA8 data
в GPU и рисует instanced sprites. Разделение позволяет подготовить animation
до того, как texture станет GPU-resident.

## Sprite sheet

```rust
use std::time::Duration;
use yuyib::{assets::Assets, two_d::{PlaybackMode, SpriteSheet, Texture, TextureSize}};

let size = TextureSize::new(128, 64)?;
let cell = TextureSize::new(32, 32)?;
let mut textures = Assets::new();
let texture = textures.insert(Texture::new(size));
let sheet = SpriteSheet::from_grid(texture, size, cell)?;
let animation = sheet.animation(Duration::from_millis(100), PlaybackMode::Loop)?;
let mut state = animation.state();
state.advance(&animation, Duration::from_millis(100));
# Ok::<(), Box<dyn std::error::Error>>(())
```

Для отдельных файлов создайте один полный `TextureRegion` для каждой texture и
передайте regions в `SpriteAnimation::from_regions`. У frame может быть
individual duration через `AnimationFrame`.

## Rendering

`SpriteDraw` описывает region, transform, tint и `layer`; `Camera2d` создаёт
orthographic projection. `SpriteRenderer` рассчитан на instanced rendering и
stable painter ordering: sprites одного layer сохраняют input order.

## Limits & Caveats

- `SpriteSheet::from_grid` принимает только complete regular grid. Padding,
  margins и irregular atlas требуют explicit `TextureRegion`.
- Много уникальных textures снижает batching. Текущая renderer contract — one
  texture per batch; shipping content стоит pack'ить в atlas.
- Runtime atlas packing, transform animation и 2D lights ещё Planned. ECS
  frame/finish events доступны через `AnimatedSprite2d`; bounded CPU viewport
  culling обычных `Sprite2d`, tilemaps, chunk CPU culling и tile collision
  snapshots описаны в отдельных
  [2D ECS animation](ecs-sprite-animation.md) и
  [viewport](sprite-viewport-culling.md) / [tilemap guides](tilemaps.md).
- Pixel art требует explicit filtering/color-space policy; current API не
  обещает universal pixel-perfect result на любом DPI/scale.
