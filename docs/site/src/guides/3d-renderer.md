# 3D: первый GPU mesh path

> **Статус:** Experimental  
> **Модули:** `yuyib::model`, `yuyib::game_3d`, `yuyib::render_3d`  
> **Платформа:** Windows + compatible WGPU adapter

Первый 3D vertical slice уже создаёт настоящий WGPU draw call:
`MeshPrimitive` → immutable GPU buffers → `Camera3d` + instance → scoped
`RenderFrame`. Это unlit path для prototypes, preview и
проверки content pipeline; он не выдаёт себя за готовый PBR renderer.

```rust,no_run
use yuyib::{
    model::MeshPrimitive,
    render_3d::{Camera3d, MeshInstance3d, MeshRenderer3d},
};

# fn setup(renderer: &yuyib::render::Renderer) -> Result<(), Box<dyn std::error::Error>> {
let cube = MeshPrimitive::cube(0.5)?;
let meshes = MeshRenderer3d::new(renderer);
let gpu_cube = meshes.upload_mesh(renderer, &cube)?;

// In `Application::on_render`:
// meshes.draw(frame, Camera3d::default(), &gpu_cube, MeshInstance3d::default())?;
# let _ = gpu_cube;
# Ok(())
# }
```

`MeshRenderer3d::new_for_frame` и `upload_mesh_for_frame` доступны для lazy
prototype initialization внутри `on_render`; для normal application lifetime
создавайте pipeline и GPU buffers во время setup/loading и храните их в scene
resource.

## ECS scene boundary

`game_3d::Model3d` держит typed `ModelHandle`, а `game_3d::Transform3d` —
authoring TRS с quaternion. `extract_models` выдаёт owned, deterministic
snapshot. Это изолирует gameplay world от GPU resource lifetime.

`render_3d::MeshTransform3d` называется намеренно иначе: это временный
renderer-local Euler transform для unlit mesh instance. Не смешивайте его с
quaternion `game_3d::Transform3d`; общий math/transform contract будет
выделен до прямого ECS-to-renderer bridge.

## Shader tiers сейчас

Для быстрого прототипа есть `ShaderPrototype::VertexColor`, а для explicit
WGSL — `ShaderSource`/`ShaderProgram`. Они описывают source и interface,
однако пока не подключают arbitrary WGSL к `MeshRenderer3d`: этот первый mesh
pipeline использует fixed unlit WGSL. Реальная compilation/diagnostics должна
остаться задачей GPU backend, а не fake validation на CPU.

Отдельный [textured unlit path](textured-materials.md) требует UV0 и
`GpuTexture`, но использует ту же depth policy opaque scene.

`LitMeshRenderer3d` и ECS `DirectionalLight3d` дают отдельный
[Lambert lighting path](lit-materials.md) для geometry с normals.

## Быстрый импорт сцены с текстурами

`BaseColorSceneRenderer3d` — высокоуровневый путь для `.gltf`/`.glb`-сцены.
Ему достаточно хранилища `Assets<Model>`, результата `extract_models` и
`ModelTextureLoader`. При первом кадре он загрузит изображения и выберет
нужный вариант для каждого меша: однотонный либо с основной текстурой.

Он использует только базовый цвет материала, его множитель, UV0 и флаг
двусторонней отрисовки. Это полезно для просмотра карты и ранних версий игры;
normal map, металлическость, шероховатость, свечение, прозрачность и свет он
намеренно не пытается угадать. Когда нужен полный контроль, используйте
`MeshRenderer3d` и `TexturedMeshRenderer3d` напрямую.

Для проверки такой сцены используйте готовую
[свободную камеру](free-camera.md): она берёт на себя `WASD`, мышь, скрытие и
захват курсора, но оставляет точки для своего ввода.

## Кости и анимация

Для обычного персонажа с текстурами используйте высокий уровень:
`TexturedSkeletalSceneRenderer3d`. Он один раз загружает skinned-примитивы,
а для `SkeletalPreview` также удерживает отдельные morph mesh instances.
`AnimationPlayer` одним снимком возвращает bone palettes и morph weights;
position targets обновляются в persistent vertex buffer до draw. `ModelTextureLoader`
отдельно делает изображения GPU-resident; это намеренная граница загрузки,
а не скрытая работа внутри draw call. Рендерер сам находит нужный узел модели,
его точную матрицу и палитру костей. Готовый рабочий пример:

```text
cargo run -p yuyib --example velina_skeletal_preview
cargo run -p yuyib --example animated_girl_preview
```

```rust,no_run
use yuyib::{
    gltf::{AnimationClipIndex, AnimationPlayer, ImportOptions, import_scene_path_with_options},
    assets::Assets,
    model_assets::ModelTextureLoader,
    render_3d::{Camera3d, SkeletalTextureResources, TexturedSkeletalSceneRenderer3d},
    render_texture::TextureCache,
    two_d::Texture,
};

# fn setup(frame: &mut yuyib::render::RenderFrame<'_>) -> Result<(), Box<dyn std::error::Error>> {
let asset = import_scene_path_with_options("assets/hero.glb", ImportOptions::skeletal())?;
let mut player = AnimationPlayer::new(AnimationClipIndex::new(0));
let mut cpu_textures = Assets::<Texture>::new();
let mut gpu_textures = TextureCache::new();
let bindings = ModelTextureLoader::new("assets")?
    .load_for_frame(frame, &asset.model, &mut cpu_textures, &mut gpu_textures)?;
let character = TexturedSkeletalSceneRenderer3d::new_for_frame(frame, &asset.model, &asset.scene)?;

player.advance(&asset.scene, 1.0 / 60.0)?;
let pose = player.snapshot(&asset.scene)?;
character.draw(frame, Camera3d::default(), &asset.scene, &pose,
    SkeletalTextureResources { bindings: &bindings, textures: &gpu_textures })?;
# Ok(())
# }
```

Это пока текстурный, но сознательно простой внешний вид: применяются image из
`baseColorTexture` и base-color multiplier. `Mask` отсекает невидимые пиксели
по `alphaCutoff` и пишет depth; `Blend` рисуется после непрозрачных частей
сзади наперёд, смешивается с уже нарисованным цветом и depth не пишет.
Morph normal/tangent deltas, normal/emissive maps и PBR lighting пока не входят
в этот unlit character path. Например, cloth diffuse texture в bundled girl
fixture буквально белая; без lit/PBR path ткань корректно белая, но выглядит
плоско несмотря на отдельные normal/emissive изображения.
Normal map и PBR остаются отдельными проходами. Материал без изображения или
с UV-набором, отличным от UV0, не пропускается: высокий API рисует его ровно
одним `baseColorFactor`, без выдуманной белой текстуры. Количество таких частей
даёт `factor_only_primitive_count()`. Для `MASK` этот постоянный alpha либо
целиком отсеивает часть, либо пишет depth; для `BLEND` часть по-прежнему идёт в
отсортированный прозрачный проход. Анимация и деформация выполняются в vertex
shader на GPU.

### Низкий уровень

`TexturedSkinnedMeshRenderer3d` — явный строительный блок для модели с костями
и base-colour изображением. Он
берёт геометрию из `yuyib-model`, четыре индекса костей и веса из результата
`yuyib-gltf`, а матрицы текущей позы — из `SkinPalette`. Сама библиотека не
выбирает анимацию, не ищет узел сцены и не управляет материалами. Используйте
его для собственного render pass или необычной системы персонажей. Передайте
`TexturedSkinnedMaterial3d::with_alpha_mode(AlphaMode::Mask { .. })` в обычный
depth-проход, а `AlphaMode::Blend` — только через
`draw_transparent_with_depth_load`, самостоятельно отсортировав части от
камеры к дальним.

```rust,no_run
use yuyib::{
    gltf::{ImportOptions, import_scene_path_with_options},
    render_3d::{Camera3d, TexturedSkinnedMeshRenderer3d},
};

# fn setup(renderer: &yuyib::render::Renderer) -> Result<(), Box<dyn std::error::Error>> {
let asset = import_scene_path_with_options("assets/hero.glb", ImportOptions::skeletal())?;
let primitive = &asset.model.meshes()[0].primitives()[0];
let skin_data = &asset.scene.skinned_primitives()[0];
let skinned = TexturedSkinnedMeshRenderer3d::new(renderer);
let gpu_mesh = skinned.upload_mesh(renderer, primitive, skin_data)?;

// В игровом цикле: player.advance(delta)?; let pose = player.snapshot(&asset.scene)?;
// Resolve the base-colour image with ModelTextureLoader, then:
// skinned.draw(frame, Camera3d::default(), &gpu_mesh, &pose.skin_palettes()[0],
//     [1.0, 0.0, 0.0, 0.0,  0.0, 1.0, 0.0, 0.0,  0.0, 0.0, 1.0, 0.0,  0.0, 0.0, 0.0, 1.0], material)?;
# let _ = gpu_mesh;
# Ok(())
# }
```

Палитра отправляется на GPU заново каждый кадр, сама геометрия — только при
загрузке. Один draw принимает не более 512 костей; модель с большим числом
костей нужно разделить на части. Сейчас это непрозрачная текстурная отрисовка
с depth test: normal map, тени и смешивание прозрачности будут подключены
отдельными фазами, а не скрыты под неполным API.

## Limits & Caveats

- Базовый `MeshRenderer3d` читает только positions/indices. Простые textured
  renderers используют UV0; `TexturedPbrMeshRenderer3d` выбирает отдельный
  authored UV0–UV7 для каждого texture slot и также требует normals/tangents
  для normal-map TBN basis.
- Opaque geometry writes `Depth32Float` with `CompareFunction::Less`;
  `SceneRenderer3d` clears it once per scene phase, so opaque visibility does
  not depend on extraction order. Transparent sorting, occlusion, frustum
  culling and instancing are still absent. Renderer-neutral distance-based LOD
  selection exists in `yuyib::game_3d`, but hysteresis and GPU residency do not.
- `Camera3d` — right-handed, Y-up, forward -Z и WGPU depth range `0..=1`.
- `GpuMesh` — immutable GPU resource. Не upload'те одинаковую geometry на
  каждом frame; положите его в cache, связанный с `ModelHandle`.
- Static glTF/Blender import, Source 1 VMF brush-to-`Model` compilation, unlit
  textures, Lambert directional lighting и низкоуровневая GPU-отрисовка
  скелетной анимации, direct-light PBR, normal maps и render graph доступны.
  Direct VMF scene/material binding, тени, IBL и текстурный/прозрачный renderer
  персонажей остаются Planned.

Полные signatures и errors: [render 3D API](../api/yuyib_render_3d/index.html),
[ECS scene API](../api/yuyib_game_3d/index.html) и
[shader API](../api/yuyib_shader/index.html).
