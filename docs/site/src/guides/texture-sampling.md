# Texture sampling: mipmaps и anisotropy

Yuyib хранит colour-space semantics отдельно от sampling policy:

- base color и emissive загружаются как sRGB;
- normal и metallic-roughness — как linear;
- mipmaps для sRGB строятся после перевода RGB в linear space;
- прозрачные texels усредняются с alpha weighting, чтобы цвет невидимых
  пикселей не создавал кайму по краям.

## High-level preset

Для обычной 3D-сцены достаточно готового пресета:

```rust
use yuyib::prelude::{TextureSampler, TextureSamplingPreset};

let sampler: TextureSampler = TextureSamplingPreset::HighQuality.sampler();
```

`Balanced` является default: full mip chain, trilinear filtering и запрос 4x
anisotropy. `HighQuality` запрашивает 16x. `PixelArt` оставляет один уровень и
nearest filtering.

Этот production slice покрывает только RGBA8 2D textures. GPU/compute mip
generation, compressed/HDR/cubemap/array texture paths и deduplicated sampler
objects пока недоступны.

PBR diffuse environment lighting is independent of this texture path: the M2.2
slice accepts typed L2 spherical-harmonics irradiance through `PbrLighting3d`.
It does not sample a cubemap; HDR/cubemap import, specular prefilter and BRDF
LUT remain unavailable.

`ModelTextureLoader` по умолчанию применяет `Balanced` и к glTF textures:
сохраняет source address modes, но даёт всем изображениям full mip chain,
trilinear filtering и запрос 4x anisotropy. Это high-level baseline, который
не требует настройки каждого material вручную. `HighQuality` можно включить
явно через `with_texture_sampling_preset`.

## Low-level escape hatch

`TextureSampler` остаётся открытым low-level контрактом:

```rust
use yuyib::prelude::{TextureMipmapPolicy, TextureSampler};
use yuyib::render::wgpu;

let sampler = TextureSampler {
    address_mode_u: wgpu::AddressMode::Repeat,
    address_mode_v: wgpu::AddressMode::Repeat,
    address_mode_w: wgpu::AddressMode::ClampToEdge,
    mag_filter: wgpu::FilterMode::Linear,
    min_filter: wgpu::FilterMode::Linear,
    mipmap_filter: wgpu::MipmapFilterMode::Linear,
    mipmaps: TextureMipmapPolicy::Generate,
    anisotropy_clamp: 8,
};
```

Передайте его в `TextureCache::upsert*` напрямую или в
`ModelTextureLoader::with_sampler`. Последний вызов отключает high-level
preset: texture slots со своим importer sampler сохранят его точные значения,
а слоты без sampler descriptor используют переданную настройку. Для полного
сохранения importer semantics без собственного fallback есть
`preserve_imported_sampling()`.

## Diagnostics и limits

После upload вызовите `GpuTexture::sampling_diagnostics()`:

```rust,ignore
let diagnostics = gpu.sampling_diagnostics();
println!(
    "mips={}, anisotropy={} (requested={}), fallback={:?}",
    diagnostics.mip_level_count(),
    diagnostics.effective_anisotropy(),
    diagnostics.requested_anisotropy(),
    diagnostics.anisotropy_fallback(),
);
```

- anisotropy ограничивается portable maximum `16`;
- значение `0` поднимается до `1`;
- если device feature недоступен или один из filters не linear, используется
  безопасный `1x`, а причина остаётся в diagnostics;
- full mip chain занимает примерно на треть больше GPU memory, чем level zero;
- mip generation сейчас CPU-side. `ModelTextureLoader::prepare` строит уровни
  на asset worker, поэтому bounded render-thread publication только копирует
  уже готовую chain. Прямой `TextureCache::upsert*` делает подготовку на
  вызывающем thread; для больших textures используйте
  `PreparedTextureUpload::rgba8` на worker (`rgba8_owned`, если decoder уже
  вернул `Vec<u8>`) и
  `upsert_prepared_for_frame` в render callback. Byte budget streamed loader
  учитывает все mip levels, а не только source level.

## Reference screenshot foundation (M1)

Swapchain textures are not a portable `COPY_SRC` source. Prefer the headless
path:

```rust,ignore
use yuyib::prelude::*;

let mut gpu = OffscreenRenderer::new(320, 180)?;
let frame = gpu.render_and_capture_rgba8(ClearColor::linear(0.02, 0.03, 0.05, 1.0), |frame| {
    // draw with MeshRenderer3d / Game3dScene using this RenderFrame
    let _ = frame;
})?;
write_png_rgba8("smoke.png", frame.width(), frame.height(), frame.pixels())?;
```

Low-level pieces remain available separately:

1. ordinary texture with `RENDER_ATTACHMENT | COPY_SRC`;
2. `read_texture_rgba8` (BGRA→RGBA swizzle when needed);
3. `encode_png_rgba8` / `write_png_rgba8`.

Runnable smoke (no window):

```text
cargo run -p yuyib --example frame_capture_smoke
```

HDR colour post-process is not applied on `OffscreenRenderer` yet — captures
are display-referred `Rgba8Unorm` colour targets.
