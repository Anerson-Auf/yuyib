# RFC 0002 — assets, importers и streaming

- **Статус:** accepted
- **Дата:** 2026-07-31
- **Зависит от:** RFC 0001

## Проблема

Игровой runtime должен принимать изображения, 2D-анимации, 3D-модели и карты из editor-ов, но renderer не может зависеть от Blender, Hammer или одного proprietary map format. Форматы отличаются versioning-ом, лицензированием, coordinate systems, материалами и entity metadata. Direct runtime import editor source files делает конечную игру медленнее и менее воспроизводимой.

## Решение

Вводится двухступенчатый pipeline:

```text
source asset -> importer plugin -> neutral imported asset -> cooked runtime asset -> typed handle
```

Importer отвечает за конкретный внешний формат. Cooker нормализует platform-neutral data в оптимальные runtime ресурсы, генерирует metadata и cache key. Runtime загружает только cooked asset и создаёт immutable typed handle. В development mode допускается auto-import/hot reload; в shipping build source importer не обязан присутствовать.

## Universal asset handles

Assets получают stable typed handles, а не передаются строковыми путями через ECS. Handle может временно указывать на placeholder, пока resource готовится асинхронно. Вызовы к нему должны быть safe при loading/error state и доступны diagnostics UI.

Каждый asset хранит source URI, importer version, cooker options, dependencies, content hash, memory/VRAM estimate и status. Это даёт детерминированную invalidation цепочки и документацию реальных costs.

## 2D

### Image sources

- PNG/WebP и другие decoder plugins;
- single sprite;
- grid/rect sprite sheet;
- sequence из отдельных frame files;
- atlas, сгенерированный build step;
- tilemap source через отдельный importer plugin.

`SpriteAnimation` всегда оперирует нормализованными frame regions и durations, поэтому sheet и набор файлов имеют один runtime API. Frame events и playback modes (`loop`, `once`, `ping-pong`) принадлежат animation component. Position/rotation/scale — transform animation и не должны смешиваться с frame decoding.

### Limits and caveats

- Dynamic texture atlases полезны в editor/dev mode, но runtime packing может вызывать fragmentation и re-upload; shipping assets предпочтительно cook-ить заранее.
- Большое число уникальных textures ломает batching. Диагностика должна показывать sprite batch count, texture switches и VRAM usage.
- Pixel art требует explicit sampler/color-space policy, иначе mipmapping и filtering дадут размытие.

## 3D

### Interchange format

glTF 2.0 — обязательный первый 3D importer. Он используется для Blender-exported scenes/models, meshes, UVs, PBR material data, textures, normals, tangents, skeletons и animation clips.

`.blend` не является runtime format. Optional developer tool может запускать Blender для конвертации `.blend` в glTF; результат и настройки конвертации попадают в cache. Shipping game не зависит от установленного Blender.

### Import controls

К каждому model/scene asset применимы настройки `include_meshes`, `include_textures`, `normals`, `tangents`, `animations`, `skeleton`, `collision`, `lod_policy`, `mip_policy` и material overrides. Значения имеют явную семантику `preserve`, `generate`, `drop` или `replace` там, где это применимо.

Если normal/tangent data отключены, cooker выбирает совместимый material variant. Renderer обязан отказать с диагностикой, если custom shader требует отсутствующий vertex attribute: silent visual corruption недопустим.

## Maps and editors

Сцены от внешних editor-ов превращаются в neutral representation: geometry, materials, lights, cameras, collision, named entities, key/value metadata и trigger volumes. Interpretation entity class остаётся задачей game plugin, а не map importer.

Поддерживаемые направления в порядке приоритета:

1. Source 1 Hammer: VMF source importer; BSP read/import исследуется и поставляется отдельно от VMF.
2. Source 2 Hammer: отдельный plugin и отдельный compatibility matrix после исследования version/asset pipeline. Он не должен блокировать Source 1 MVP.
3. TrenchBroom/Quake-family: MAP plugin.
4. Tiled: TMX/JSON 2D plugin.
5. LDtk: JSON 2D plugin.

Ни один importer не поставляет, не извлекает и не обходит права на game assets. Необходимые source data и права на неё остаются ответственностью проекта, использующего runtime.

## Streaming and visibility

`AssetStreaming` отделён от import. Import создаёт данные; streaming принимает решения о resident state по budget-ам, view/camera importance и явным gameplay prefetch hints.

MVP guarantees: async loading, placeholder/failure states, frustum culling, distance culling, mip policy, manual LOD selection, instancing/batching diagnostics и per-asset RAM/VRAM metrics.

### Ближайший этап: постепенная загрузка

`GltfSceneLoad` уже выносит чтение, typed import, ECS spawn, static collision и
image decode большой карты в worker. `LoadedGltfScene::prepare_for_frame`
публикует ограниченное число texture slots и geometry primitives, предоставляет
точный progress/error state и атомарно открывает модель для draw только после
полной residency. Оставшийся разрыв до общего `AssetStreaming`: один primitive
пока является минимальной GPU-транзакцией, а решения о residency ещё не
принимаются автоматически по camera/budget/dependency graph.

Камера и сцена в этот момент продолжают работать. До готовности используется
явный placeholder, а не блокировка окна и не «случайный» белый материал.
Игровой код сможет добавить приоритет: ближайшие объекты, предзагрузка зоны и
обязательные стартовые ресурсы. Лимиты работы на кадр и приоритеты будут
высокоуровневой настройкой; низкоуровневый API сохранит доступ к отдельным
задачам и моменту публикации готового ресурса.

Future extensions: generated LOD, HLOD, world partition, occlusion culling, sector/portal visibility and texture virtualisation. Их добавление не меняет typed asset handles или scene semantics.

## Public safety rules

- Importer не исполняет code из asset или metadata.
- Непроверенные inputs валидируются до allocation, decompression и GPU upload.
- Ограничения размера, глубины и количества dependencies задаются в import config.
- Все background loads публикуют результат на main/runtime boundary, а не мутируют World произвольно.

## Documentation requirements

Каждый importer получает отдельную page: supported versions, coordinate/unit conversion, material mapping, unsupported features, legal constraints, import options, performance notes и reproducible sample asset. Для loader-ов указываются размерные/VRAM costs и placeholder/failure behavior.
