# 3D: high-level `Game3dScene`

> **Статус:** Experimental  
> **Crate / module:** `yuyib::render_3d`  
> **Уровень:** high-level

`Game3dScene` — короткий стандартный путь от ECS `World` и `Assets<Model>` до
кадра. Он выполняет hierarchy propagation, извлекает visible `Model3d`,
проверяет hard model bound и camera, выбирает ECS directional light, lazily
создаёт renderer и сохраняет model/texture GPU cache между кадрами.
По умолчанию facade также строит WGPU-compatible camera frustum и фильтрует
model/per-mesh local AABB до renderer submission.

```rust,no_run
use yuyib::prelude::*;

# fn setup() -> Result<(), Box<dyn std::error::Error>> {
let mut models = Assets::new();
let cube = models.insert(Model::cube(0.5)?);
let mut world = World::new();
world.spawn((Model3d::new(cube), Transform3d::default()));
world.spawn(DirectionalLight3d::new(
    [-0.3, -1.0, -0.4],
    [1.0, 0.95, 0.9],
    0.8,
)?);

let mut scene = Game3dScene::new("assets", Game3dSceneConfig::default())?;
# let _ = (&mut scene, &mut world, &models);
# Ok(())
# }
```

В render callback:

```rust,ignore
let stats = scene.render(frame, &mut world, &models)?;
```

`asset_root` canonicalized и ограничивает external model textures после
symlink resolution. Embedded GLB images не требуют отдельного файла.

## Policies

- `Game3dShading::Lambert` — default standard path с base-colour factor/texture
  и одним directional light.
- `Game3dShading::Unlit` — быстрый preview/base-colour path.
- `Game3dShading::Pbr` — metallic/roughness Cook-Torrance path с
  GGX/Smith/Schlick. Factor-only материалы используют дешёвый pipeline;
  любой непустой subset glTF textures `base`, `normal`, `metallic/roughness`,
  `emissive` использует отдельный pipeline. Tangent stream обязателен только
  при наличии normal map; отсутствующие channels используют material factors,
  а не случайно выбранную fallback texture.
- `PbrBlendPolicy3d::default()` защищает high-level scene от exporter noise:
  `BLEND` с base factor alpha 1, без реально прозрачных pixels и с 99% alpha
  254/255 публикуется как opaque. Это возвращает depth writes большим стенам,
  которые иначе нестабильно сортировались бы как стекло. Настоящие translucent
  textures остаются `BLEND`; для точного source contract выберите
  `PbrBlendPolicy3d::strict()` через `with_pbr_blend_policy`.
- glTF `MASK` не участвует в transparent sorting: `alphaCutoff` переносится в
  validated `PbrAlphaMode3d`, fragment shader отбрасывает alpha ниже порога,
  а surviving fragments продолжают писать depth. `doubleSided` и mirrored
  winding по-прежнему выбирают соответствующий culling pipeline.
- `Game3dLighting::FirstDirectional` берёт первый свет в стабильном ECS order
  и имеет явный fallback; `Fixed` не читает lights из ECS.
- `Game3dSceneConfig` по умолчанию propagates hierarchy и ограничивает visible
  model count. Camera-distance LOD bridge сохраняет source mesh selection для
  обычных glTF nodes; только настоящий `LodGroup3d` заменяет модель целиком.
  Переключение shading сохраняет отдельный cache каждого route.
- `Game3dSceneStats` раскрывает model/light counts и нижние counters:
  triangles, draw calls, render passes, cache misses, material allocations и
  `frustum` (`input/tested/culled/visible/unbounded`).
- `with_frustum_culling(false)` отключает high-level filtering. Низкий путь —
  `Frustum3d`, `ModelBoundsRegistry3d` и
  `filter_extracted_models_by_frustum_3d_with` с собственным bounds cache.

Примеры: factor-only сцена —
`cargo run -p yuyib --example game_3d_scene`; реальный textured GLB —
`cargo run -p yuyib --example gltf_pbr_lab`.

Textured PBR submissions объединяются в opaque и transparent passes (до 512
draws в одном batch). На текущем `sci-fi_lab.glb` диагностический контракт —
29 primitive draws и два render passes; glTF node не должен повторно рисовать
все mesh модели.

Каждый encoded batch владеет immutable uniform storage до выполнения command
buffer. Нельзя повторно записывать slots 0..N для transparent pass в том же
кадре: GPU увидит последние transform/material values и opaque объекты начнут
исчезать или менять emissive при camera-dependent сортировке.

`SceneDrawStats::promoted_blend_draws` показывает число draw requests, для
которых high-level policy исправила effectively-opaque `BLEND`. Negative-scale
nodes используют отдельные clockwise pipeline variants; material sidedness и
normal-map tangent handedness при этом сохраняются.

## Limits & Caveats

Lambert route не называется PBR. PBR route корректно читает glTF channels:
roughness из G, metallic из B, normal scale и tangent handedness. Он принимает
factor-only и произвольный непустой texture subset с UV0–UV7. Factor-only
`BLEND` остаётся неподдержанным high-level случаем, поскольку без texture
alpha он не даёт полезной сортируемой поверхности; IBL и shadows ещё не
реализованы.
Textured `BLEND` сортируется back-to-front, depth-tests и не пишет depth.
Обычный `render()` всё ещё eager-загружает cache miss текущего кадра. Для
loading screen/streamed zone используйте `GltfSceneLoad`, затем короткий
`LoadedGltfScene::prepare_for_frame(frame, scene)`: default budget ограничивает
и texture slots, и полные geometry primitives. Настраиваемый путь —
`prepare_for_frame_with_budget`; low-level ownership остаётся у
`ModelTextureLoader::prepare`, `PreparedModelTextures` и
`LitSceneRenderer3d`. Тот же bounded contract поддерживает PBR и создаёт
material bindings постепенно вместе с primitives. Выберите shading до первого
`prepare_for_frame`; Unlit остаётся eager preview route. Для custom phases
доступны `RenderGraph`, low-level PBR renderers и raw `RenderFrame`.
