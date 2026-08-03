# 2D: sprites и animation

> **Статус:** Experimental  
> **Модули:** `yuyib::two_d`, `yuyib::game_2d`, `yuyib::animation`

## Цель страницы

Собрать **выбор API** для 2D картинки: от одного quad до state machine.
Пошаговый beginner path — [tutorial](../tutorials/first-2d-playable.md).

## Уровни API (снизу вверх)

| Уровень | Тип | Когда |
|---|---|---|
| Metadata | `Texture`, `TextureRegion`, `SpriteSheet` | Offline atlas / import result |
| ECS draw | `Sprite2d` | Любой видимый quad в world |
| Simple anim | `AnimatedSprite2d` + `step_sprite_animations_2d` | Одна timeline / clip |
| HL animator | `SpriteAnimator2d` + `AnimationSet` / state machine | `play("walk")`, facing, once→idle |
| Scene facade | `Game2dScene` | Upload + cull + batch + stats |
| Playable | `PlayableLoop2d` / `PlatformerPlayable2d` | Готовый input/camera/step/draw |

Не прыгайте сразу в `SpriteRenderer` / `RenderGraph`, пока HL не упёрся в limit.

## `Sprite2d` — базовый component

```rust,ignore
world.spawn(
    Sprite2d::new(region)
        .with_position([x, y])  // Y вниз
        .with_size([w, h])      // отрицательная ось = mirror
        .with_layer(0),
);
```

| Поле / метод | Зачем |
|---|---|
| `region` | Откуда брать тексели (atlas slot) |
| `position` / `size` / `rotation` | World transform 2D |
| `layer` | Painter order (ascending); tie-break = entity id |

Extraction (`extract_sprites` / visible variant) сортирует и режет batches по
**adjacent** same-texture runs — texture может дать несколько draw calls,
чтобы не ломать transparent order.

## Animation: какой путь выбрать

### A. `AnimatedSprite2d`

Один clip, frames = `TextureRegion`. Host вызывает
`step_sprite_animations_2d(world, delta)`.  
Guide: [ECS animation](ecs-sprite-animation.md).

### B. `SpriteAnimator2d` + `yuyib-animation`

`AnimationSet` / `AnimationStateMachine`, `play("walk")`, velocity→facing
helpers (`apply_velocity_facing_2d`, cardinal clips).

```powershell
cargo run -p yuyib --example sprite_animator_2d_smoke
cargo run -p yuyib --example two_d_animator_playable
```

Почему отдельный crate `yuyib-animation`? State machine переиспользуется
вне sprite-only сценариев; `game_2d` остаётся ECS/render-facing.

## Atlas

- Offline `.ysprite` → [`offline-sprite-atlas`](offline-sprite-atlas.md)
- Runtime sheet helpers → [`ecs-sprite-atlas`](ecs-sprite-atlas.md)

Importer пишет metadata; GPU upload всё равно через `Game2dScene::queue_texture`
или raw `TextureCache`.

## Limits & Caveats

- Нет 2D lights / normal maps в foundation (см. RFC 0006).
- Нет blend trees / skeletal 2D.
- Culling ordinary sprites: [sprite-viewport-culling](sprite-viewport-culling.md);
  tiles — [tilemaps](tilemaps.md).

## См. также

- [Game2dScene](game-2d-scene.md)
- [2D concepts](../concepts/two-d.md)
- [2D interaction](interaction-2d.md)
