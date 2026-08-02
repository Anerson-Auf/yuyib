# 2D: offline atlas manifest и streaming boundary

> **Статус:** Experimental  
> **Crate / module:** `yuyib::two_d`, `yuyib::assets`  
> **Формат:** `yuyib.sprite_atlas`, version `1`, extension `.ysprite`

`SpriteAtlasImporter` загружает metadata заранее собранного texture atlas. Это
shipping-oriented путь: regions и animations подготавливаются build step-ом,
а runtime не перепаковывает изображения и не вызывает массовый re-upload.

```text
hero.ysprite bytes
  -> ImporterRegistry<ImportedSpriteAtlas>
  -> required logical dependency: textures/hero_atlas.png
  -> resolver + bounded image decode
  -> stable AssetId<Texture>
  -> ImportedSpriteAtlas::bind_texture
  -> RuntimeSpriteAtlas / SpriteAnimation / TextureRegion
```

Importer не открывает texture, не знает project root и не создаёт GPU objects.
`ImportedSpriteAtlas` — neutral CPU asset. `bind_texture` только связывает
metadata с уже зарезервированным либо resident typed handle, поэтому renderer
может использовать обычный placeholder до завершения загрузки изображения.

## Минимальный manifest

```json
{
  "format": "yuyib.sprite_atlas",
  "version": 1,
  "texture": {
    "uri": "textures/hero.png",
    "width": 64,
    "height": 32,
    "alpha": "straight",
    "color_space": "srgb"
  },
  "regions": [
    { "name": "walk_0", "x": 0, "y": 0, "width": 32, "height": 32 },
    { "name": "walk_1", "x": 32, "y": 0, "width": 32, "height": 32 }
  ],
  "animations": [{
    "name": "walk",
    "playback": "loop",
    "frames": [
      { "region": "walk_0", "duration_ms": 90 },
      { "region": "walk_1", "duration_ms": 110 }
    ]
  }]
}
```

`playback`: `loop`, `once` или `ping_pong`. `alpha` по умолчанию `straight`,
`color_space` — `srgb`, `animations` может отсутствовать. Unknown fields
отклоняются: опечатка не превращается в молча потерянную настройку.

## High-level регистрация

```rust
use yuyib::prelude::*;

let mut registry = ImporterRegistry::<ImportedSpriteAtlas>::default();
register_sprite_atlas_importer(&mut registry)?;
let result = registry.import(ImportSource::new("hero.ysprite", bytes))?;

// Host resolver читает result.dependencies[0] по собственной security policy.
let texture_handle = texture_assets.reserve(AssetMetadata::default());
let atlas = result.asset.bind_texture(texture_handle)?;
let walk = atlas.animation("walk").expect("validated content contract");
# Ok::<(), Box<dyn std::error::Error>>(())
```

Полный headless пример не требует PNG и окна:

```text
cargo run -p yuyib --example offline_sprite_atlas --no-default-features --features two-d
```

## Custom limits и cancellation

Format-specific bounds дополняют `ImporterRegistryLimits`:

```rust
use yuyib::prelude::*;

let importer = SpriteAtlasImporter::new(SpriteAtlasImportLimits {
    max_manifest_bytes: 256 * 1024,
    max_regions: 512,
    max_animations: 64,
    max_frames_per_animation: 256,
    max_total_frames: 4096,
    ..SpriteAtlasImportLimits::default()
})?;
let mut registry = ImporterRegistry::<ImportedSpriteAtlas>::default();
registry.register(importer)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

Для background import используйте
`AssetServer::try_import_bytes_cancellable`. Он возвращает stable asset handle
и `ImportCancellation`. `cancel()` является cooperative: registry проверяет
signal до/после plugin-а, а встроенный atlas importer — между bounded records.
Native Rust code принудительно не прерывается.

## Invariants и ошибки

- manifest bytes, regions, animations, per-animation и total frames ограничены;
- texture и region dimensions ненулевые, сложения координат проверяются на
  overflow, каждый rectangle обязан помещаться в texture;
- имена непустые, bounded, без control characters и уникальны в своей группе;
- frame ссылается только на существующий region, duration ненулевой и bounded;
- texture URI — один required logical dependency, а не filesystem capability;
- retained CPU estimate и importer `id@version` попадают в asset metadata;
- JSON с unknown fields, неподдерживаемой version или partial animation
  отклоняется structured error-ом, а не загружается частично.

## Что этот slice намеренно не делает

- runtime dynamic atlas packing;
- чтение PNG/WebP по URI и canonical-root policy;
- image decode и GPU upload — это следующие независимые bounded stages;
- автоматический dependency graph orchestration и hot reload;
- Tiled/LDtk import.

Так сохраняется важная граница: importer создаёт воспроизводимый neutral asset,
streaming решает residency, renderer только публикует ограниченную GPU работу.
