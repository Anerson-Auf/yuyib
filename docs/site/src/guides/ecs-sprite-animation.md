# 2D: ECS animation

> **Статус:** Experimental  
> **Модуль:** `yuyib::game_2d`

`AnimatedSprite2d` adds a high-level animation state to an existing `Sprite2d`.
Call `step_sprite_animations_2d(&mut world, delta)` from the application's
chosen schedule. The system copies the current animation `TextureRegion` into
the sprite and returns deterministic `FrameChanged`/`Finished` events.

`SpriteAnimation` frames are `TextureRegion`s: therefore one animation can use
an atlas or separate texture files without a different ECS API. The caller owns
decoded assets, GPU upload, fixed-vs-variable timestep policy and handling of
events; this system reads no wall clock and does no renderer work.

Full API: [game 2D](../api/yuyib_game_2d/index.html).

## Limits & Caveats

No texture loading, atlas packing, GPU upload, culling, tweening, blend trees
or automatic gameplay/audio dispatch is included. `delta` must come from a
host-defined schedule; use a fixed tick when deterministic simulation matters.
