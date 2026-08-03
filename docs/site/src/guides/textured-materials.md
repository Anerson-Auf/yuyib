# 3D: textured unlit materials

> **Статус:** Experimental  
> **Модули:** `yuyib::render_texture`, `yuyib::render_3d`  
> **Требует:** decoded RGBA8 image, `TEXCOORD_0` в mesh

## Когда использовать

Нужен **самый простой** textured mesh path: одна albedo texture, без lights/PBR.
Для glTF PBR / IBL берите [`Game3dScene`](game-3d-scene.md) /
[`GltfSceneLoad`](../tutorials/load-gltf-scene.md). Для Lambert — 
[lit-materials](lit-materials.md).

## Ownership

| Объект | Владеет | Почему разделено |
|---|---|---|
| `TextureCache` | GPU texture / view / sampler | Одна texture → много materials/renderers |
| `TexturedMeshRenderer3d` | Pipeline + bind-group layout | 3D API не зависит от 2D sprite bindings |
| `TexturedMaterial3d` | Binding на uploaded texture + tint | Material ≠ mesh upload |

## Пример

```rust,no_run
use yuyib::{
    render_3d::{TexturedMaterial3d, TexturedMeshRenderer3d},
    render_texture::{TextureCache, TextureSampler},
};

# fn demo(
#     renderer: &yuyib::render::Renderer,
#     handle: yuyib::two_d::TextureHandle,
#     image: &yuyib::image::DecodedImage,
#     primitive: &yuyib::model::MeshPrimitive,
# ) -> Result<(), Box<dyn std::error::Error>> {
let mut textures = TextureCache::new();
// upsert: validate RGBA8 length/format, create/replace GPU texture.
textures.upsert(renderer, handle, image, TextureSampler::default())?;
let texture = textures.get(handle).expect("uploaded texture");

let meshes = TexturedMeshRenderer3d::new(renderer);
// upload_mesh требует TEXCOORD_0; иначе MissingTexCoords0.
let mesh = meshes.upload_mesh(renderer, primitive)?;
let material = TexturedMaterial3d::new(texture, [1.0; 4]); // tint RGBA
# let _ = (mesh, material);
# Ok(())
# }
```

### Почему `upsert`, а не silent create

Cache проверяет byte length и device limits. sRGB → `Rgba8UnormSrgb`, linear →
`Rgba8Unorm`. Изменение CPU image **не** обновляет GPU само — нужен явный
`upsert`. После device rebuild — re-upload всех resident textures.

`ModelTextureLoader` умеет resolve local glTF URI → decode → `TextureCache`,
но **не** выбирает `TexturedMaterial3d` за вас (нет скрытой material policy).

## Limits & Caveats

- Только opaque unlit; нет normals/lights/PBR/alpha blend/mip gen в этом path.
- Depth: `Depth32Float` / `Less`. Transparent phase — отдельный future path.
- Missing texture → `TexturedMeshRenderError::MissingTexture`, не pink fallback
  без diagnostic.

## См. также

- [Lambert / model textures](lit-materials.md)
- [StandardMaterial](standard-material-and-scenes.md)
- [Texture sampling](texture-sampling.md)
- [Game3dScene](game-3d-scene.md)
