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

Runnable `two_d_tile_playground` проверяет этот вертикальный путь в окне:
atlas, анимация, WASD/стрелки, camera follow и блокировка стенами.

## Реализовано

| Область | Состояние | Граница API |
|---|---|---|
| Изображение | bounded PNG/JPEG/WebP decode, RGBA8 upload | `image` низкий, `AssetLoader` — async shell |
| Sprite source | одиночный image, regular sheet, произвольные regions/кадры | `two_d` |
| Animation | разные duration, loop/once/ping-pong, ECS frame/finish events | `SpriteAnimation`, `AnimatedSprite2d` |
| Rendering | instanced alpha sprite pass, camera, stable painter order | `SpriteRenderer` низкий; ECS extraction — средний |
| Видимость | ordinary sprite viewport, tile viewport и chunk CPU culling | `game_2d` |
| Tile collision | static AABB snapshot, contacts tile↔physics | средний и низкий API |
| Playable 2D | движение sprite с нормализацией диагонали и tile walls | `KinematicSpriteController2d` высокий API |
| UI/input | native UI, keyboard event boundary, pointer interaction 2D | общие `app`, `input`, `gameplay` crates |

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

Нынешний controller годится для top-down wall sliding. В нём нет gravity,
jump, one-way platform, slope, moving platform, dynamic body, circle/capsule
sweep, trigger dispatch и broadphase cache. Вдобавок collider snapshot
перестраивается на каждом controller step — это безопасно и прозрачно, но
плохо для больших maps.

Не следует делать один огромный `Physics2d` с десятками флагов. Правильнее:

1. static tile collider cache + chunk broadphase;
2. отдельный platformer controller (gravity/jump/one-way), не меняющий
   top-down controller semantics;
3. dynamic solver/plugin поверх общих queries.

### Приоритет 5 — камеры, ввод и взаимодействие

`Camera2d` существует, но camera follow, bounds, zoom/pan/shake и screen↔world
conversion не объединены в одну policy API. Пример реализует follow локально.
Keyboard adapter также общий; 2D semantic movement/action mapping ещё не имеет
готового profile, gamepad/touch virtual controls отсутствуют.

2D pointer interaction уже есть, но не связан с render picking, layers и
camera transform. Нужен простой `CameraController2d` высокого API и явный
`screen_to_world` низкого API до автоматических click-to-move систем.

### Приоритет 6 — карты редакторов

Tiled/LDtk importer, tileset metadata, layers, object layers, parallax,
tile animation и navigation отсутствуют. Это заметное расхождение с RFC 0002.
Первым нужен Tiled JSON/TMX importer plugin в neutral TileMap2d representation;
не надо делать runtime dependency на редактор.

## Правило дальнейшего развития

Новый частый сценарий должен появляться как одна small config structure и один
step/render entry point высокого уровня, сохраняя видимый data boundary для
низкого уровня. Нельзя «упрощать» API тем, что он скрывает allocations,
background mutation, physics policy или GPU lifetime: эти решения должны быть
явны в diagnostics и escape hatch.
