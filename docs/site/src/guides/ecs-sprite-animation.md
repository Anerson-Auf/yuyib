# 2D: ECS animation

> **Статус:** Experimental  
> **Модуль:** `yuyib::game_2d`  
> **Уровень:** mid-level (поверх `Sprite2d`)

## Когда использовать

Нужно проигрывать frame sequence на уже существующем `Sprite2d` entity:
idle/walk/attack из atlas или из отдельных texture regions — **одним** API.

Не используйте этот путь, если ещё нет decode/upload: сначала
[первый 2D tutorial](../tutorials/first-2d-playable.md) или
[`Game2dScene::queue_texture`](game-2d-scene.md).

## Быстрый пример

```rust,no_run
use std::time::Duration;
use yuyib::game_2d::{AnimatedSprite2d, Sprite2d, step_sprite_animations_2d};
use yuyib::two_d::{PlaybackMode, SpriteAnimation};

# fn demo(
#     world: &mut yuyib::ecs::prelude::World,
#     regions: &[yuyib::two_d::TextureRegion],
# ) -> Result<(), Box<dyn std::error::Error>> {
// Один duration на все кадры; для per-frame duration — SpriteAnimation::new(frames, …).
let animation = SpriteAnimation::from_regions(
    regions,
    Duration::from_millis(100),
    PlaybackMode::Loop,
)?;
let first = animation.frames()[0].region();
world.spawn((Sprite2d::new(first), AnimatedSprite2d::new(animation)));

// В FixedUpdate или Update — host передаёт свой delta:
let events = step_sprite_animations_2d(world, Duration::from_millis(16));
for event in events {
    // FrameChanged / Finished — gameplay/audio glue на стороне приложения
    let _ = event;
}
# Ok(())
# }
```

Точные constructors/`SpriteAnimation` helpers — в rustdoc
[`yuyib_2d`](../api/yuyib_2d/index.html) / [`yuyib_game_2d`](../api/yuyib_game_2d/index.html).
Runnable: `sprite_animator_2d_smoke`, `two_d_animator_playable`.

## Почему эти функции

| API | Что делает | Почему так |
|---|---|---|
| `AnimatedSprite2d` | Держит animation state рядом со `Sprite2d` | Visual frame ≠ отдельный renderer object |
| `SpriteAnimation` из `TextureRegion`s | Кадры = регионы атласа или разных textures | Нет второго «AnimationAtlas-only» API |
| `step_sprite_animations_2d(world, delta)` | Копирует текущий region в `Sprite2d`, эмитит events | Host владеет timestep (fixed vs variable) |
| Events `FrameChanged` / `Finished` | Детерминированный сигнал | Нет скрытого audio/VFX dispatch внутри crate |

Система **не** читает wall clock, **не** грузит PNG и **не** трогает GPU.
`Game2dScene::render` позже увидит обновлённый `Sprite2d::region`.

## HL альтернатива

Для state machine + `play("walk")` + velocity/facing смотрите `SpriteAnimator2d`
и [sprites-and-animation](sprites-and-animation.md) / profile `PlayableLoop2d`.

## Limits & Caveats

- Нет blend trees, cross-fade 2D, tweening, automatic culling.
- `delta` обязан прийти из schedule host’а; для replay/physics берите fixed tick.
- GPU upload / missing texture — ответственность `Game2dScene` stats, не animation step.

## См. также

- [Sprites и animation](sprites-and-animation.md)
- [ECS atlas](ecs-sprite-atlas.md)
- [Tutorial 2D](../tutorials/first-2d-playable.md)
