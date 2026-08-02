# 3D: StandardMaterial и glTF scene data

> **Статус:** Experimental  
> **Модули:** `yuyib::render_3d`, `yuyib::gltf`

`StandardMaterial3d` — высокий мост от `yuyib_model::Material` к
доступным render paths. Он принимает source material,
`ModelTextureBindings` и `TextureCache`, после чего выбирает solid unlit,
textured unlit, colour Lambert или textured Lambert renderer.

```rust,no_run
use yuyib::render_3d::StandardMaterial3d;

# let material: yuyib::model::Material = todo!();
let standard = StandardMaterial3d::from_model_material(&material)?;
# let _ = standard;
# Ok::<(), yuyib::render_3d::StandardMaterialError>(())
```

Unsupported features fail at conversion time: normal/emissive/
metallic-roughness textures, non-default metallic/roughness factors and UV
sets other than zero. Base texture + Lambert теперь выбирает отдельный
textured Lambert pipeline: текстура и свет применяются вместе, но путь
остаётся Lambert (без полноценного PBR).

Для low-level PBR cutout задаётся без raw pipeline flags:

```rust
use yuyib::render_3d::{PbrAlphaMode3d, PbrMaterial3d};

let foliage = PbrMaterial3d::new([1.0; 4], 0.0, 0.8)?
    .with_alpha_mode(PbrAlphaMode3d::mask(0.5)?);
# let _ = foliage;
# Ok::<(), yuyib::render_3d::PbrMaterialError>(())
```

Обычный `draw_with_depth_load` выбирает depth-writing mask semantics. Alpha
ниже cutoff отбрасывается; равный cutoff сохраняется согласно glTF 2.0.
`Blend` нельзя случайно отправить этим методом в opaque phase: API вернёт
`PbrMeshRenderError::BlendRequiresTransparentPhase`.

## glTF scene import

Use `import_scene_path` when Blender content contains scene structure rather
than only one mesh asset.

```rust,no_run
use yuyib::gltf::import_scene_path;

let imported = import_scene_path("assets/level.glb")?;
let model = imported.model;
let scene = imported.scene;
# let _ = (model, scene);
# Ok::<(), yuyib::gltf::ImportError>(())
```

`ImportedScene` preserves every source scene, default-scene selection, node
source order, child links, local TRS или exact affine matrix, mesh index links, camera metadata and
`KHR_lights_punctual` directional-light metadata. `ImportedScene` itself is
pure imported data and deliberately does **not** mutate an ECS world.

`yuyib-scene` now provides the explicit ECS adapter for one chosen source
scene without flattening its hierarchy; see
[Scene ECS guide](scene-ecs-and-interactions.md).

## Limits & Caveats

- glTF matrix transforms проходят в `LocalMatrixTransform3d`; hierarchy
  multiplies affine matrices exactly. `WorldTransform3d::as_trs()` returns
  `None` for shear instead of a lossy approximation, while extraction passes
  `model_matrix` directly to the renderer.
- Point/spot punctual lights are rejected; directional light data is preserved.
- `StandardMaterial3d` выбирает только непрозрачный путь. Материал с `MASK`
  или `BLEND` вернёт ошибку выбора фазы, а не будет случайно нарисован как
  непрозрачный. Для быстрой импортированной ECS-сцены используйте
  `BaseColorSceneRenderer3d`: он уже рисует `BLEND` после opaque-фазы,
  сортируя примитивы от дальних к ближним по их центру. Внутри одного
  прозрачного меша идеальная сортировка треугольников пока не выполняется.
  PBR доступен отдельно через factor-only `PbrMeshRenderer3d` и tangent-space
  `TexturedPbrMeshRenderer3d`; `Game3dShading::Pbr` выбирает их автоматически.
  Partial PBR texture sets и `AlphaMode::Mask` поддерживаются: mask остаётся в
  depth-writing opaque phase и выполняет discard по factor × sampled alpha.
  IBL остаётся planned. Lambert
  textured route уже batch'ит common path.
- Scene import preserves local transforms; `yuyib-scene` converts them into
  `LocalTransform3d`/`Parent3d` and propagates derived world transforms.

Full API: [render 3D](../api/yuyib_render_3d/index.html) and
[glTF importer](../api/yuyib_gltf/index.html).
