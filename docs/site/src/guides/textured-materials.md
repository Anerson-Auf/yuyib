# 3D: textured unlit materials

> **Статус:** Experimental  
> **Модули:** `yuyib::render_texture`, `yuyib::render_3d`  
> **Требует:** decoded RGBA8 image, `TEXCOORD_0` в mesh

`TextureCache` владеет GPU texture, view и sampler, тогда как
`TexturedMeshRenderer3d` владеет material bind-group layout и pipeline. Одна
texture может использоваться несколькими renderer'ами без связи 3D API с
внутренними 2D sprite bindings.

```rust,no_run
use yuyib::{
    render_3d::{TexturedMaterial3d, TexturedMeshRenderer3d},
    render_texture::{TextureCache, TextureSampler},
};

# let renderer: yuyib::render::Renderer = todo!();
# let handle: yuyib::two_d::TextureHandle = todo!();
# let image: yuyib::image::DecodedImage = todo!();
# let primitive: yuyib::model::MeshPrimitive = todo!();
let mut textures = TextureCache::new();
textures.upsert(&renderer, handle, &image, TextureSampler::default())?;
let texture = textures.get(handle).expect("uploaded texture");
let meshes = TexturedMeshRenderer3d::new(&renderer);
let mesh = meshes.upload_mesh(&renderer, &primitive)?;
let material = TexturedMaterial3d::new(texture, [1.0; 4]);
# let _ = (mesh, material);
# Ok::<(), Box<dyn std::error::Error>>(())
```

Cache validates RGBA8 byte length/device dimension and maps sRGB to
`Rgba8UnormSrgb`, linear data to `Rgba8Unorm`. CPU texture changes require
explicit `upsert`; a rebuilt GPU device requires re-upload.

## Limits & Caveats

- Mesh requires `TEXCOORD_0`; absent UV returns
  `TexturedMeshUploadError::MissingTexCoords0`. Unbound material returns
  `TexturedMeshRenderError::MissingTexture`.
- Only one-mip sampled RGBA8 2D textures; no mip generation, anisotropy,
  streaming, async eviction or hot reload.
- The material is opaque and unlit: no normals/lights/PBR/normal maps,
  alpha blending or material batching.
- Draws use the opaque `Depth32Float`/`Less` policy. A separate transparent
  phase is required before alpha-blended materials are added.
- `ModelTextureLoader` can resolve approved local glTF URI metadata, decode it
  and upload it into `TextureCache`; it does not automatically choose or bind
  a `TexturedMaterial3d` for a model primitive.

Full API: [GPU texture cache](../api/yuyib_render_texture/index.html) and
[textured mesh renderer](../api/yuyib_render_3d/index.html).
