# Assets и импорт

> **Статус:** Experimental foundation  
> **Crate / module:** `yuyib::assets`, `yuyib::image`  
> **Платформы:** storage/decode — platform-neutral; GPU upload — Windows

`yuyib-assets` реализует in-memory storage с typed generational handles:
`Assets<T>` и `AssetId<T>`. `yuyib-image` добавляет controlled decoding
PNG/JPEG/WebP в RGBA8 data под `DecodePolicy`. `AssetServer` предоставляет
stable loading handles, а `ImporterRegistry<T>` — opt-in typed importer plugins
для одного neutral output type.

`AssetId<T>` привязан к type `T` и поколению slot. После `remove` все копии
старого handle становятся invalid: `get` и `get_mut` вернут `None`, даже если
тот же slot уже занят новым asset. Практическое использование описано в
[руководстве по typed handles](../guides/assets.md).

Целевой pipeline остаётся таким: `source -> importer -> neutral imported asset
-> cooked runtime asset -> typed handle`. Typed importer registry, bounded
source/probe contracts, stable loading handles и placeholder/error states уже
реализованы. Disk cook cache + `AssetCooker` keying для imported glTF
(`CookCache`, `import_scene_bytes_cached`) — **Experimental / M3.1**. Dependency-
driven selective invalidation, shipping-without-importer и residency budgets
пока **Planned**. Собственный format подключается по руководству
[Создание importer plugin](../guides/custom-importers.md).

glTF 2.0 — целевой 3D interchange format для Blender. Current 2D API уже
унифицирует single image, sprite sheet и sequence files через `TextureRegion`,
но generated atlas pipeline пока Planned.

## Limits & Caveats

`DecodePolicy` важен для untrusted assets: decoder ограничивает допустимые
форматы и размер decoded output до allocation. Normal/tangent metadata и
static glTF 2.0 import живут в отдельных `model`/`gltf` contracts; image
decoder не выводит их сам. Content cooking, hot reload и general import
settings остаются отдельным future asset pipeline.

Current Source 1 slice включает VMF text/brushes, VMT metadata, bounded VTF
7.2 RGBA/BGRA decode и safe local `$basetexture` path resolution. Source 2
имеет отдельный compatibility contract и не является частью Source 1 MVP.
