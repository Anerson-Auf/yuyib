# 3D: Model assets

**Статус:** Experimental CPU model data  
**Модуль:** `yuyib::model`  
**Используется с:** `yuyib::render_3d` для фактического draw call

`yuyib::model` — первый устойчивый asset boundary для 3D. Он хранит
validated indexed triangle meshes, optional normals/tangents/UVs и
PBR-oriented material metadata. В нём нет WGPU objects, filesystem I/O или
скрытого глобального asset cache.

```rust
use yuyib::model::Model;

let prototype = Model::cube(0.5)?;
let primitive = &prototype.meshes()[0].primitives()[0];
assert!(primitive.normals().is_some());
# Ok::<(), yuyib::model::PrimitiveError>(())
```

## Low-level mesh API

`MeshPrimitive::new(positions, indices)` принимает только indexed triangle
lists: индексов должно быть кратно трём, и каждый индекс обязан попадать в
position stream. Optional streams добавляются цепочкой:

```rust
use yuyib::model::MeshPrimitive;

let triangle = MeshPrimitive::new(
    vec![[0.0, 0.5, 0.0], [-0.5, -0.5, 0.0], [0.5, -0.5, 0.0]],
    vec![0, 1, 2],
)?.with_normals(vec![[0.0, 0.0, 1.0]; 3])?;
# Ok::<(), yuyib::model::MeshValidationError>(())
```

All attribute streams are either absent or have exactly one entry per
position. Отсутствие normals — допустимо для unlit/custom renderer paths; оно
не означает, что normal mapping magically станет работать.

## Materials and textures

`ModelTexture` хранит либо URI, либо encoded image bytes из GLB buffer view,
но сам не делает I/O и не декодирует их. `Material` ссылается на
`ModelTextureIndex`; `Model::new` проверяет каждый reference. Поэтому cooker
может позже разрешить URI через local folder, pak, HTTP cache или virtual FS,
либо передать embedded bytes в decoder, не меняя scene data.

Base colour и emissive binding предполагают sRGB sampling; normal и
metallic-roughness textures — linear data. Это семантика material API, а не
автоматический GPU upload в текущем milestone.

`ModelTextureLoader` вычисляет `TextureAlphaSummary` во время уже выполняемого
RGBA decode и сохраняет его в `ResolvedModelTexture`. Повторно читать pixels
на render thread не требуется. Low-level renderer сам решает, как использовать
minimum/maximum alpha и coverage 254/255; high-level PBR применяет к ним
настраиваемый `PbrBlendPolicy3d`.

`Model::texture_usage()` / `LoadedGltfScene::texture_usage_summary()` дают
inventory: unused slots, external URIs, empty embedded blobs и material→mesh
UV mismatches. Importer публикует соответствующие codes
(`gltf-unused-texture`, `gltf-external-texture-uri`, `gltf-missing-uv-set`, …).
`ModelTextureLoader::prepare` декодирует только material-referenced slots —
unused inventory не блокирует GPU publication.

Sampler descriptor импортируется вместе с glTF texture. High-level loader
по умолчанию сохраняет address modes, но применяет `Balanced` filtering:
resident mip chain, trilinear sampling и portable anisotropy с диагностируемым
fallback. Exact source semantics доступны как явный low-level opt-out. Готовые
presets, memory trade-offs и настройка описаны в
[Texture sampling](texture-sampling.md).

## Limits & Caveats

- Current slice supports static indexed triangle meshes only.
- Static glTF/GLB import for Blender content is available through
  [`yuyib::gltf`](gltf-import.md). Skeletons, animations, meshlets, collision
  cooking and **asset-imported** LOD are not implemented yet. Renderer-neutral
  runtime `LodGroup3d` selection is available separately in `yuyib::game_3d`.
- Source 1 VMF, Source 2 and Hammer are **not** supported here.
- `yuyib::model` itself does no I/O. Use explicit `ModelTextureLoader` for its
  safe local URI or embedded-image decode/upload boundary; renderer material
  binding remains caller-owned.
