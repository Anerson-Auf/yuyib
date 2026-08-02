# 2D-ресурсы и animation

> **Статус:** Experimental  
> **Crate / module:** `yuyib::two_d`, `yuyib::render_2d`  
> **Платформы:** metadata — platform-neutral; GPU rendering — Windows

`yuyib::two_d` отделяет описания texture/regions/animation от file decoding и
GPU residency. Поэтому один animation API подходит sprite sheet, atlas и
последовательности отдельных PNG-файлов.

## Основные entry points

| API | Задача |
|---|---|
| `TextureSize`, `Texture` | Проверенные dimensions и metadata texture asset. |
| `TextureRegion` | Валидный sub-rectangle одной texture. |
| `SpriteSheet::from_grid` | Обычный uniform sheet без padding/margins. |
| `SpriteAnimation` | Immutable frames, каждый со своим duration. |
| `SpriteAnimationState` | Runtime cursor, advance и current frame. |
| `PlaybackMode` | `Loop`, `Once`, `PingPong`. |
| `Camera2d`, `SpriteDraw`, `SpriteRenderer2d` | Experimental instanced GPU sprite pass. |

## Когда что использовать

- Используйте `SpriteSheet::from_grid`, если кадры образуют полный равномерный
  grid, например 4×4 в одном PNG.
- Стройте `SpriteAnimation::from_regions`, когда кадры лежат в нескольких
  texture assets или имеют разные duration.
- Для atlas с padding, margins или irregular rectangles создавайте
  `TextureRegion` явно; grid helper намеренно отвергает неполный grid.

## Limits & Caveats

- Размеры и region coordinates — physical pixels; zero-sized textures и frames
  запрещены type-level validation.
- `TextureRegion::new` rejects out-of-bounds rectangles, включая integer
  overflow. Не обходите проверку расчётом UV вручную для обычных sprites.
- `PingPong` не дублирует endpoint frames.
- Current `SpriteRenderer2d` batch строится вокруг **одной texture**. Atlas
  уменьшает draw calls; arbitrary multi-texture batching пока не контракт.
- Animation state не загружает image bytes и не создаёт GPU resources. Для
  decoding используйте `yuyib::image`, для upload — renderer 2D API.

См. [sprites и animation](../guides/sprites-and-animation.md) и
[каталог crates](../reference/crates.md).
