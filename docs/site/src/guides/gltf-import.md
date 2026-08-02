# glTF / Blender: статический импорт

> **Статус:** Experimental  
> **Модуль:** `yuyib::gltf`  
> **Подходит для:** Blender export в glTF 2.0 (`.gltf` или `.glb`)

`yuyib-gltf` — первый реальный content-import path для 3D. Он читает `.gltf`
и `.glb`, создавая проверенный renderer-neutral `yuyib::model::Model`.
Затем модель можно положить в `Assets<Model>` и использовать в ECS/renderer.

```rust,no_run
use yuyib::{assets::Assets, gltf::import_path, model::Model};

let spaceship = import_path("assets/models/spaceship.glb")?;
let mut models = Assets::<Model>::new();
let spaceship = models.insert(spaceship);
# Ok::<(), yuyib::gltf::ImportError>(())
```

## Подключение через общий importer registry

`GltfAssetImporter` является реальным встроенным plugin-ом для
`ImporterRegistry<ImportedAsset>`. Он подтверждает, что registry не знает о
glTF: зависимость направлена от format crate к `yuyib-assets` SDK.

```rust
use yuyib::prelude::*;

let mut importers = ImporterRegistry::default();
importers.register(GltfAssetImporter::new(ImportOptions::skeletal_preview()))?;

let bytes = std::fs::read("assets/character.glb")?;
let imported = importers.import(ImportSource::new("character.glb", &bytes))?;
let scene: ImportedAsset = imported.asset;
# let _ = scene;
# Ok::<(), Box<dyn std::error::Error>>(() )
```

Registry adapter намеренно не открывает external buffer URI: он принимает
self-contained GLB или `.gltf` с data-URI buffers. Для внешнего `mesh.bin`
возвращается `ExternalBufferRequiresResolver`, после чего host resolver должен
получить dependency под своей root/network policy. Старые path-based функции
сохраняются как explicit low-level API для доверенного local project tree.

Как реализовать такой plugin для нового формата: [Создание собственного
importer plugin](custom-importers.md).

## Что импортируется

- textual `.gltf`, binary `.glb`, GLB BIN chunk, local external buffers и
  Base64 data-URI buffers;
- indexed `TRIANGLES` с обязательными `POSITION` и `indices`;
- optional `NORMAL`, `TANGENT`, `TEXCOORD_0` through `TEXCOORD_7`;
- имена mesh-ей, коэффициенты базового цвета/metallic/roughness, `doubleSided`,
  `alphaMode`/`alphaCutoff` и точные исходные данные
  `KHR_materials_pbrSpecularGlossiness`;
- внешние URI и текстуры, вложенные в GLB через `bufferView`. Их исходные
  байты и MIME-тип сохраняются в `ModelTexture`, без повторного кодирования.

Используйте `ImportOptions` / `ImportLimits` для assets за недоверенной
границей. Ограничиваются aggregate decoded buffer bytes, vertices и indices.
External buffer path обязан оставаться внутри directory модели: importer не
разрешит `..` escape или absolute path. Embedded image bytes имеют отдельный
`max_embedded_image_bytes` budget и не декодируются до explicit asset-loading
step.

## Blender workflow

1. В Blender экспортируйте **glTF 2.0**, предпочтительно `.glb` для одного
   переносимого файла или `.gltf` с relative resource paths.
2. Примените transforms/triangulate в content pipeline, если это требуется
   вашему game contract.
3. Base-color-only renderers используют UV0. PBR сохраняет authored UV set
   отдельно для base/normal/metallic-roughness/emissive texture.
   `doubleSided` выбирает отдельный вариант без отсечения обратных граней.
   `alphaMode: "BLEND"` рисуется после непрозрачной геометрии и не пишет depth.
4. Импорт создаёт модель в памяти. `SceneRenderer3d` показывает только форму
   мешей. Для быстрой сцены с текстурами используйте
   `BaseColorSceneRenderer3d`: он подготовит меши и изображения при первом
   кадре.
5. Передайте `BaseColorSceneRenderer3d` один `ModelTextureLoader`. Он безопасно
   читает локальные URI внутри указанной папки, а изображения внутри `.glb`
   декодирует из самого файла. Низкоуровневый путь остаётся доступным: можно
   вручную работать с `ModelTextureLoader`, `TextureCache` и своим шейдером.

Для node hierarchy, cameras и source light metadata используйте
`import_scene_path`; details — в [scene data guide](standard-material-and-scenes.md).

## Статичный preview для rigged модели

По умолчанию импорт строгий: skin, animation, `JOINTS_0` и `WEIGHTS_0`
вызывают ошибку. UV0–UV7 при этом являются обычными lossless model streams и
не требуют preview policy. Это защищает игру от случая, когда модель внешне
загружается, но движения персонажа незаметно пропали.

Для осознанного просмотра такой модели без анимации включите высокий уровень
настройки:

```rust,no_run
use yuyib::gltf::{ImportOptions, import_path_with_options};

let model = import_path_with_options(
    "assets/characters/hero.glb",
    ImportOptions::static_preview(),
)?;
# Ok::<(), yuyib::gltf::ImportError>(())
```

Результат — mesh в исходной bind pose и обычные преобразования узлов. Importer
не применяет к нему skeleton или animation. Он сознательно игнорирует только
`TEXCOORD_1`…`TEXCOORD_7`, `JOINTS_0`, `WEIGHTS_0`, definitions skin и
animations. Цвета вершин, morph target, второй набор joints/weights,
`TEXCOORD_8` и другие неизвестные атрибуты по-прежнему вызывают ошибку.
Например, `for_tests/velina_zzz.glb` проходит static preview вместе с
метаданными прозрачных материалов. Его показ использует отдельную фазу
`BLEND`; skinning и анимация — следующий независимый слой, а не причина
ослаблять static preview.

Низкоуровневый вариант нужен, если одновременно задаются собственные limits:

```rust,no_run
use yuyib::gltf::{ImportLimits, ImportOptions, ImportPolicy};

let options = ImportOptions {
    limits: ImportLimits {
        max_vertices: 2_000_000,
        ..ImportLimits::default()
    },
    ..ImportOptions::default()
}
.with_policy(ImportPolicy::StaticPreview);
```

## Кости и анимация персонажа

Для персонажа используйте отдельную политику, а не `StaticPreview`. Она
сохраняет `JOINTS_0`, `WEIGHTS_0`, inverse bind matrix, skin и анимационные
каналы translation/rotation/scale:

```rust,no_run
use yuyib::gltf::{
    AnimationClipIndex, AnimationPlayer, ImportOptions, import_scene_path_with_options,
};

let asset = import_scene_path_with_options(
    "assets/characters/velina.glb",
    ImportOptions::skeletal(),
)?;
let mut player = AnimationPlayer::new(AnimationClipIndex::new(0));
player.advance(&asset.scene, 1.0 / 60.0)?;
let pose = player.snapshot(&asset.scene)?;

// В будущем renderer загрузит pose.skin_palettes()[0].matrices() на GPU.
# Ok::<(), Box<dyn std::error::Error>>(())
```

`AnimationPlayer` — высокий уровень с play/pause/stop, loop и скоростью.
Для своего timeline используйте низкоуровневую
`sample_animation(&asset.scene, clip, seconds)`.

Ограничения строгого `skeletal()` намеренные: только четыре влияния на вершину
(`JOINTS_0`/`WEIGHTS_0`), linear/step keyframes и TRS. Cubic spline,
morph animation, второй набор joints/weights и animation matrix-узлов
отклоняются. Extra UV `TEXCOORD_1`…`TEXCOORD_7` сохраняются в `MeshPrimitive`;
текущий base-colour skeletal renderer использует UV0, а custom/PBR path может
выбрать остальные sets. `ImportLimits` ограничивает суммарное
число joints и keyframes до выделения больших структур.

## Предпросмотр модели с линиями или точками

Blender и Sketchfab иногда сохраняют вместе с моделью вспомогательные
`LINES`, `LINE_STRIP`, `LINE_LOOP` или `POINTS`: например контур, guide или
point cloud. Обычный `skeletal()` намеренно остановится с
`UnsupportedPrimitiveMode`, чтобы игра не потеряла часть asset незаметно.

Для отдельного окна просмотра используйте высокий уровень
`skeletal_preview()`. Он импортирует скелет, animation и все треугольники, а
helper primitive явно пропускает. Результат содержит отчёт, поэтому UI/editor
может показать пользователю точное количество пропусков:

```rust,no_run
use yuyib::gltf::{ImportOptions, import_scene_path_with_options};

let asset = import_scene_path_with_options(
    "assets/characters/hero.glb",
    ImportOptions::skeletal_preview(),
)?;
if !asset.report().is_complete() {
    println!(
        "Не показано вспомогательных primitive: {}",
        asset.report().skipped_primitive_count(),
    );
    for skipped in asset.report().skipped_primitives() {
        println!("mesh {}, primitive {}, mode {:?}", skipped.mesh(), skipped.primitive(), skipped.mode());
    }
}
# Ok::<(), yuyib::gltf::ImportError>(())
```

Это не режим для production content pipeline: если линии или точки важны
игре, добавьте renderer для нужной topology и оставьте строгую политику.
`SkeletalPreview` также показывает модели с vertex-animated одеждой: position
morph targets и linear/step morph-weight channels сохраняются и сэмплируются
вместе с skeletal TRS animation. Текущий unlit character renderer обновляет
позиции на CPU и публикует их в persistent vertex buffer; morph normal/tangent
deltas пока не участвуют в lighting. Строгий `skeletal()` по-прежнему отклоняет
такой asset. Sparse accessor, vertex colour и неизвестные атрибуты не
ослабляются ни в одном preview policy.

## Limits & Caveats

- `import_path` возвращает только `Model`. `import_scene_path` дополнительно
  сохраняет исходные сцены, иерархию узлов, локальный TRS или точную affine
  matrix, камеры и направленные источники света. Он намеренно не создаёт и не
  «сплющивает» ECS-сущности.
- Matrix преобразований узлов хранится в column-major виде. Point/spot lights,
  некорректный TRS и неаффинные matrix отклоняются с понятной ошибкой.
- Animations, skins, morph targets, sparse accessors, Draco, lines/points,
  vertex colors и другие атрибуты вершины обычно отклоняются: импортёр не
  должен молча портить модель. Исключение — явный `SkeletalPreview`, который
  перечисляет пропущенные lines/points в `ImportReport`; поддерживаемые morph
  targets доступны через `ImportedScene::morph_primitives`.
- `KHR_materials_pbrSpecularGlossiness` сохраняется отдельным типом данных.
  Текущий renderer ещё не рисует этот вариант PBR и обязан сообщить об этом,
  а не подменять его metallic-roughness материалом.
- Импортируются `OPAQUE`, `MASK` и `BLEND` alpha modes, emissive factor/map,
  `KHR_materials_emissive_strength`, metallic/roughness material data и
  `TEXCOORD_0`–`TEXCOORD_7`. Emissive strength сразу умножается на linear
  emissive factor, поэтому renderer-neutral `Material` не теряет расширение.
  `BLEND` уже обрабатывает `BaseColorSceneRenderer3d`; его unlit `MASK` route
  пока возвращает понятную ошибку. `Game3dShading::Pbr` поддерживает `MASK`
  полностью: валидирует cutoff, отбрасывает uncovered fragments и пишет depth
  для surviving fragments. `doubleSided` выбирает отдельный вариант
  отрисовки без отсечения обратных граней.
- Вложенные PNG/JPEG из GLB и локальные URI загружаются через
  `ModelTextureLoader`. Для каждого импорта действует отдельный лимит
  `max_embedded_image_bytes`: он ограничивает общий размер скопированных
  исходных байтов изображений. Data-URI изображений пока нет.
- Importer diagnostics включают mesh/material/texture codes
  (`gltf-factor-only-material`, `gltf-unbound-material`, `gltf-unused-texture`,
  `gltf-missing-uv-set`, …). High-level inventory:
  `Model::texture_usage` / `LoadedGltfScene::texture_usage_summary`. See
  [Streamed glTF scene](streamed-gltf-scene.md) and example
  `gltf_texture_diagnostics`.
- Source VMF/VPK, Source 2 и Hammer — это отдельные форматы и адаптеры; этот
  импортёр их не подменяет.

## Runnable map example

`cargo run -p yuyib --example gltf_map_static_scene` imports the real map in
`for_tests`, preserves its affine hierarchy, draws only the source mesh owned
by each node and places a procedural cube at the calculated scene centre.
The same viewer accepts `the_billiards_room.glb`, the shield fixture or
`cyber_samurai.glb` after `--`; the samurai uses explicit preview policy and
reports its omitted line-helper primitive.
Основные текстуры карты загружаются автоматически. Normal map, PBR,
прозрачность и освещение в этом режиме пока не имитируются: для них нужен
следующий специализированный проход рендеринга.

Полный PBR fixture запускается через
`cargo run -p yuyib --example gltf_pbr_lab`: он использует high-level
`Game3dScene` и sci-fi GLB с base, normal, metallic/roughness и emissive maps.
`velina_zzz.glb` проверяется skeletal importer и запускается через
`cargo run -p yuyib --example velina_skeletal_preview`.
Новая sci-fi girl с клипом `walk` запускается через
`cargo run -p yuyib --example animated_girl_preview`: skeletal animation и
отдельная cloth morph animation проигрываются одним `AnimationPlayer`.

Управление в окне: `WASD` — движение, `Space`/`Ctrl` — вверх/вниз, `Shift` —
ускорение, мышь — поворот камеры, `Esc` — выход. Курсор скрыт и удерживается
в окне. Настройки управления и низкоуровневое вмешательство описаны в
[руководстве по свободной камере](free-camera.md).

Полные functions и error variants: [glTF API](../api/yuyib_gltf/index.html).
