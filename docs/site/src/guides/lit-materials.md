# 3D: Lambert lighting и текстуры материалов

> **Статус:** Experimental  
> **Модули:** `yuyib::game_3d`, `yuyib::render_3d`, `yuyib::model_assets`

Первый lit path использует `LitMeshRenderer3d`: indexed positions + обязательные
normals, directional light и `LitMaterial3d`. Это Lambert diffuse + explicit
ambient, а не попытка выдать ранний pipeline за PBR.

```rust,no_run
use yuyib::{
    game_3d::{DirectionalLight3d, extract_directional_lights},
    render_3d::{LambertLighting3d, LitMaterial3d, LitMeshRenderer3d},
};

# let renderer: yuyib::render::Renderer = todo!();
# let primitive: yuyib::model::MeshPrimitive = todo!();
# let mut world = yuyib::ecs::prelude::World::new();
world.spawn(DirectionalLight3d::sun([0.3, -1.0, -0.2])?);
let light = extract_directional_lights(&mut world).lights()[0];
let lighting = LambertLighting3d::new(light, [0.04; 3])?;
let renderer_3d = LitMeshRenderer3d::new(&renderer);
let mesh = renderer_3d.upload_mesh(&renderer, &primitive)?;
let material = LitMaterial3d::new([0.8, 0.4, 0.1, 1.0]);
# let _ = (mesh, lighting, material);
# Ok::<(), Box<dyn std::error::Error>>(())
```

`DirectionalLight3d` is gameplay/ECS metadata. Its ray direction is **from
the light toward the scene**; `extract_directional_lights` produces a stable,
renderer-neutral snapshot. `LambertLighting3d` validates it again at renderer
boundary and accepts an explicit linear ambient RGB term.

Для первой карты или прототипа обычно удобнее высокий художественный API. Он
не притворяется физическим освещением: пока в renderer нет exposure и tone
mapping, передавать туда настоящие десятки тысяч lux нельзя.

```rust,no_run
use yuyib::render_3d::LambertLighting3d;

let lighting = LambertLighting3d::artistic(
    [0.62, -0.58, 0.38], // луч к карте: немного сверху и сбоку
    [0.44, 1.0, 0.64],   // мягкий зелёный оттенок
    0.55,                // прямая яркость Lambert-прохода
    [0.15, 0.19, 0.16],  // свет на теневых сторонах
)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

`artistic` — высокий вариант. `LambertLighting3d::new(DirectionalLightDraw,
ambient)` остаётся низким API для игры, уже извлекшей свет из ECS.

## Текстура + свет

`TexturedLitMeshRenderer3d` — низкоуровневый вариант, когда у меша есть
позиции, `normal` и UV0. Он не теряет рисунок текстуры: фрагмент сначала
берёт base color, затем умножает его на Lambert-свет. Так можно сделать
мягкий зелёный художественный свет без отдельного «зелёного фильтра»:

```rust,no_run
use yuyib::render_3d::{
    LitMaterial3d, LitMeshInstance3d, TexturedLitMaterial3d,
    TexturedLitMeshRenderer3d,
};

# let frame: &mut yuyib::render::RenderFrame<'_> = todo!();
# let camera: yuyib::render_3d::Camera3d = todo!();
# let primitive: yuyib::model::MeshPrimitive = todo!();
# let texture: &yuyib::render_texture::GpuTexture = todo!();
# let lighting: yuyib::render_3d::LambertLighting3d = todo!();
let renderer = TexturedLitMeshRenderer3d::new_for_frame(frame);
let mesh = renderer.upload_mesh_for_frame(frame, &primitive)?;
let instance = LitMeshInstance3d::new(
    [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0],
    LitMaterial3d::default(),
    lighting,
);
renderer.draw(frame, camera, &mesh, instance, TexturedLitMaterial3d::new(texture, [1.0; 4]))?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

`LitSceneRenderer3d` — высокий вариант для обычной ECS-карты. Ему передаются
`Assets<Model>`, `ExtractedModels` и уже проверенный `LambertLighting3d`; он
сам держит кэш мешей и текстур. Выбор света остаётся у игры: renderer не
молча берёт первый свет из ECS и не меняет результат при перестановке entity.

Для обычной непрозрачной карты этот высокий путь собирает все части с UV0,
текстурой и normals в один GPU-проход глубины. Сами indexed draw calls
остаются отдельными — у них разные меши и материалы, — но больше не создаётся
render pass на каждую стену. Привязка изображения к материалу создаётся один
раз при загрузке модели и живёт в кэше. В `SceneDrawStats` это можно проверить
полями `render_passes` и `material_bind_group_creations`: после первого кадра
второе должно быть `0`.

Низкий уровень нужен только для своей очереди непрозрачных мешей:
`TexturedLitMeshRenderer3d::upload_material_for_frame` создаёт явный
`GpuTexturedLitMaterial`, а `draw_batch_with_depth_load` принимает до 512
`TexturedLitBatchDraw` и пишет их одним проходом. Не передавайте туда
прозрачные вещи: им нужна сортировка от камеры и отдельная фаза.

У glTF-карт нередко заданы `metallicFactor` и `roughnessFactor`. Для этого
Lambert-пути они **явно игнорируются**: он использует только base color,
base-color texture, UV0 и `doubleSided`. Это позволяет показать обычную карту
без ложной ошибки, но не объявляет её PBR-отрисованной. Текстура
metallic/roughness, normal/emissive map, `MASK`/`BLEND` и
specular-glossiness по-прежнему завершают подготовку понятной ошибкой — для
них нужен соответствующий render path.

## Model texture resolver

`ModelTextureLoader` closes the content path for filesystem image URIs:
`ModelTexture URI → DecodePolicy → Assets<Texture> → TextureCache →
ModelTextureBindings`.

```rust,no_run
use yuyib::model_assets::ModelTextureLoader;

# let renderer: yuyib::render::Renderer = todo!();
# let model: yuyib::model::Model = todo!();
# let mut cpu_textures = yuyib::assets::Assets::new();
# let mut gpu_textures = yuyib::render_texture::TextureCache::new();
let loader = ModelTextureLoader::new("assets")?;
let bindings = loader.load(&renderer, &model, &mut cpu_textures, &mut gpu_textures)?;
# let _ = bindings;
# Ok::<(), Box<dyn std::error::Error>>(())
```

The resolver rejects URI schemes, absolute paths and symlink/path traversal
outside canonical asset root. It rolls back CPU/GPU inserts if one decode or
upload fails. Base/emissive maps infer sRGB; normal and metallic-roughness
maps infer linear. The same source URI in incompatible roles fails explicitly.

## Limits & Caveats

- Lit mesh requires a normal per position. Missing, non-finite or zero normals
  are errors; normals are never invented silently.
- Direct radiance is `ECS light colour × illuminance_lux`; no camera exposure,
  tone mapping, physically calibrated units, shadow or light clustering yet.
- Renderer computes inverse-transpose normal matrix for non-uniform scale;
  non-invertible transform fails structurally.
- Есть textured Lambert, но это всё ещё не normal map, transparent phase,
  metallic/roughness, shadows или PBR. Для `MASK`/`BLEND` нужен отдельный
  материалный проход.
- Resolver supports only local filesystem URI, current PNG/JPEG/WebP decoder,
  RGBA8 one-mip textures and explicit reload. It does not watch files or bind
  resolved textures to a material automatically.

Full API: [game 3D](../api/yuyib_game_3d/index.html),
[renderer](../api/yuyib_render_3d/index.html),
[model assets](../api/yuyib_model_assets/index.html).
