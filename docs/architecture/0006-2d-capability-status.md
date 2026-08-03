# RFC 0006 — состояние 2D слоя и ближайшие границы

- **Статус:** accepted, high-level scene slice implemented
- **Дата:** 2026-08-01
- **Область:** `yuyib-2d`, `yuyib-game-2d`, `yuyib-render-2d`, image/assets/physics

## Заключение

2D foundation закрывает базовый use-case: texture regions, uniform sprite
sheet, произвольные кадры, loop/once/ping-pong animation, ECS sprites, tilemap,
viewport/chunk CPU culling и bounded collision с tiles. `SpriteRenderer`
рисует instanced sprites на GPU.

Следующий gap был в стыке этих возможностей: чтобы получить управляемого героя,
разработчик вручную связывал keyboard → axis → скорость → AABB → tile adapter
→ Sprite2d. Это частый сценарий, поэтому в этой поставке вводится верхний API
`KinematicSpriteController2d` + `step_kinematic_sprite_controller_2d`. Низкий
уровень не скрывается: `resolve_kinematic_tilemap_aabb_2d`, collision snapshots
и `yuyib-physics::resolve_kinematic_aabb_2d` остаются отдельными границами.

Runnable `two_d_tile_playground` проверяет этот вертикальный путь в окне через
`Game2dProfile` + `PlayableLoop2d`: atlas, анимация, WASD/стрелки, camera follow
и блокировка стенами.

## Реализовано

| Область | Состояние | Граница API |
|---|---|---|
| Изображение | bounded PNG/JPEG/WebP decode, RGBA8 upload | `image` низкий, `AssetLoader` — async shell |
| Sprite source | одиночный image, regular sheet, произвольные regions/кадры | `two_d` |
| Animation | mid: frames + events; HL: named set + opt-in SM + `play` + facing | `SpriteAnimation`, `AnimatedSprite2d`; `AnimationSet` / `AnimationStateMachine` (`yuyib-animation`); `SpriteAnimator2d` / `VelocityFacingPolicy2d` |
| Rendering | instanced alpha sprite pass, camera, stable painter order | `SpriteRenderer` низкий; ECS extraction — средний |
| Видимость | ordinary sprite viewport, tile viewport и chunk CPU culling | `game_2d` |
| Tile collision | static AABB snapshot, contacts tile↔physics | средний и низкий API |
| Playable 2D | движение sprite с нормализацией диагонали и tile walls; Rapier platformer HL | `KinematicSpriteController2d` / `PlayableLoop2d`; `PlatformerPlayable2d` (`character-2d`) |
| UI/input | pause overlay HL + native keyboard; richer HUD/menus later | `ApplicationUi::with_active_flag` / `pause_overlay_tree`; common `app`, `input` |

## Не реализовано или сделано не по целевому плану

### Приоритет 1 — 2D scene/runtime (реализовано)

`Game2dScene` реализует запланированную policy structure:

```text
Game2dSceneConfig { camera, visibility, texture_cache, draw_budget }
```

Он принимает stable texture handles и decoded images, выполняет bounded upload
на render thread, lazily создаёт renderer один раз, объединяет camera viewport,
bounded sprite/tile extraction, global painter order и adjacent texture batches.
Публичный `Game2dSceneStats` показывает draw/upload/missing/budget counts.
Файлы scene не читает; escape hatch остаётся нынешний `SpriteRenderer`.

### Приоритет 2 — streaming и cooked 2D content (первый slice реализован)

`SpriteAtlasImporter` теперь превращает versioned offline `.ysprite` manifest
в bounded neutral `ImportedSpriteAtlas`: logical texture dependency, validated
regions и animations. `bind_texture` связывает metadata со stable typed texture
handle без IO/GPU dependency; `offline_sprite_atlas` проверяет headless путь.
Общий importer contract получил cooperative cancellation.

Остаются: orchestration dependency resolver → async image decode вне кадра,
end-to-end progress UI/hot reload и optional file sequence cooker/importer.
Bounded GPU upload и placeholder уже существуют в `Game2dScene`, но пока не
соединены автоматически с atlas dependency graph. Runtime dynamic atlas packing
по-прежнему не нужен первым: он вносит fragmentation/re-upload.

### Приоритет 3 — освещение и материалы 2D

План RFC 0001 упоминает 2D lighting/effect presets. Сейчас существует только
unlit straight-alpha sprite path. Отсутствуют normal map, 2D lights, shadow
occluders, post-processing, blend modes кроме обычного alpha и pixel-art
sampler policy. Их нельзя маскировать tint-ом: это другой material contract.

Оптимальный порядок: sampler/color-space/pixel-perfect policy → material
variant → normal-map point lights → optional post-process. Каждый уровень
должен иметь preset высокого API и WGSL/custom pass низкого API.

### Приоритет 4 — физика beyond static AABB tiles

Top-down `KinematicSpriteController2d` по-прежнему только wall sliding. Отдельный
platformer controller поверх Rapier 2D (**done**):
`yuyib-character-2d::PlatformerController2d` — gravity/jump/coyote/buffer, walls,
one-way platforms через `RapierDynamicsWorld2d::move_kinematic_character`
(Rapier KCC). Semantics top-down controller не менялись.

Остаётся: tile collider cache + chunk broadphase; drop-through input; richer
moving-platform carry policy; dynamic prop interactions beyond kinematic query.

### Приоритет 5 — камеры, ввод и взаимодействие

`Camera2d` существует; high-level `CameraFollow2d` /
`PlayableLoop2d` (Deep 2D A) закрывают follow + WASD kinematic loop.
**`WorldBounds2d` + `CameraFollow2d::with_bounds` / `apply_with_surface`**
клампят viewport внутри map AABB. **Zoom / pan / shake** — `with_zoom` +
`with_base_pixels_per_unit`, `with_pan` / `set_pan`, trauma
[`CameraShake2d`]. **Cinematic** — `with_smoothing` + `with_look_ahead_scale`
+ [`CameraFollowRuntime2d`] / `apply_cinematic` (loops тикают каждый step).
Multi-camera / scripted cuts: [`CameraDirector2d`] + [`CameraCut2d`]
(timed smoothstep blend). Keyboard adapter также общий; analog sticks —
`VirtualStick2d` + `set_external_move_axis`; **`InteractionPrompt2d`**
(cursor/label/highlight из `WorldInteractionState`). Analog sticks —
`VirtualStick2d` + optional **`GilrsGamepadAdapter2d`** (feature `gamepad`).
`TileNavGrid2d` — 4-connected A* over `!solid`.

2D pointer interaction уже есть, но не связан с render picking, layers и
camera transform. Явный `screen_to_world` низкого API нужен до автоматических
click-to-move систем.

### Приоритет 6 — карты редакторов

**Started (M7: Tiled JSON/TMX + LDtk):** `yuyib-tiled` — orthogonal JSON or
TMX (embedded or external `.tsj`/`.tsx`; CSV / XML `<tile gid>` / **base64 +
zlib/gzip/zstd**), **N tilesets**, **N visual layers**, `collision`/`solid`,
`objectgroup` (**point/rect/ellipse/polygon**), GID flips → `TileFlip2d`,
**layer parallax**, **tile `animation` → `TileRegionAnimation2d`**.
`LdtkProjectImporter` — square `tileGridSize` (format), `Tiles`/`AutoLayer` +
`IntGrid` + `Entities`, **embedded or host-resolved `.ldtkl`**, **multi-tileset**,
**layer pixel offsets** + **`LdtkWorld2d`**. `LocationStack2d`;
`TileMapComposer2d`; **`TileNavGrid2d`**. Smokes + `two_d_tiled_playable`.

Ещё нет: polyline/tile-gid objects. LDtk tiles are square by format
(non-square → use Tiled). Layer offsets, parallax, world layout, tile anim,
nav, gilrs (`gamepad` feature) supported. Interaction prompt:
`InteractionPrompt2d`. Interpretation object/entity classes остаётся game
plugin (RFC 0002).

**UI LL (M6):** visuals / image extract / scroll drag. **Dialogue HL:**
`DialogueSession` + `StoryFlags` + `dialogue_overlay_tree` (JSON asset later).
Open: GPU textured UI, inertia, IME, a11y.

## Правило дальнейшего развития

Новый частый сценарий должен появляться как одна small config structure и один
step/render entry point высокого уровня, сохраняя видимый data boundary для
низкого уровня. Нельзя «упрощать» API тем, что он скрывает allocations,
background mutation, physics policy или GPU lifetime: эти решения должны быть
явны в diagnostics и escape hatch.
