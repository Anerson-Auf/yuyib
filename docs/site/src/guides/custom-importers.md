# Создание собственного importer plugin

> **Статус:** Experimental  
> **Requires:** `yuyib::assets`

Новый importer не требует изменять Yuyib. Создайте обычный Rust crate,
реализуйте `AssetImporter<YourNeutralType>` и явно зарегистрируйте значение в
`ImporterRegistry<YourNeutralType>`.

Полный запускаемый пример:

```text
cargo run -p yuyib --example custom_importer --no-default-features --features assets
```

Он показывает оба пути: прямой low-level `ImporterRegistry::import` и
background publication через `AssetServer::try_import_bytes`.

## Архитектурная граница

```text
file/network/package resolver
  -> OwnedImportSource
  -> bounded probe
  -> AssetImporter<NeutralType>
  -> ImportResult<NeutralType>
  -> cooker или PreparedAsset
  -> Assets<RuntimeType>
```

Importer не должен владеть renderer, окном, ECS world или глобальным task
runtime. Он получает уже прочитанные bounded bytes. Внешние dependencies он
возвращает как logical URI; открыть их может только host resolver со своей
root/network policy.

Это важное ограничение безопасности: source bytes могут быть untrusted, но
скомпилированный native importer является trusted code. Rust trait не
sandbox-ит вредоносный plugin. Для недоверенных third-party plugins нужен
отдельный process/WASM capability boundary.

## 1. Выберите neutral output type

Output описывает данные формата, но не GPU objects и не игровую логику:

```rust
#[derive(Debug)]
pub struct DialogueAsset {
    pub lines: Vec<String>,
}
```

Для карты это обычно neutral scene: geometry, materials, lights, cameras,
collision, named entities и KeyValue metadata. Толкование `npc_guard` или
`quest_trigger` принадлежит game plugin, а не map importer.

Если несколько formats производят один neutral type, они регистрируются в одном
`ImporterRegistry<T>`. Registry разных `T` намеренно несовместимы на уровне
типов.

## 2. Сделайте настройки полями importer-а

Не используйте `HashMap<String, Any>` для обязательных options. Обычные поля
дают проверяемый API и документацию:

```rust
struct DialogueImporter {
    max_lines: usize,
    allow_empty: bool,
}
```

Проверяйте format-specific limits: количество vertices/nodes/frames,
decompression ratio, recursion depth и числовую конечность. Общий registry
дополнительно ограничивает encoded bytes и metadata, но не знает внутреннюю
сложность каждого формата.

## 3. Реализуйте trait

```rust
use std::{error::Error, fmt};
use yuyib::prelude::*;

# #[derive(Debug)]
# pub struct DialogueAsset { pub lines: Vec<String> }
#[derive(Debug)]
struct DialogueError(&'static str);

impl fmt::Display for DialogueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}
impl Error for DialogueError {}

struct DialogueImporter { max_lines: usize }

impl AssetImporter<DialogueAsset> for DialogueImporter {
    type Error = DialogueError;

    fn descriptor(&self) -> ImporterDescriptor {
        ImporterDescriptor::new("my-game.dialogue", "1.0.0")
            .with_extension("ydlg")
            .with_media_type("text/x-yuyib-dialogue")
    }

    fn probe(&self, probe: ImportProbe<'_>) -> ImportMatch {
        if probe.prefix.starts_with(b"YDLG\n") {
            ImportMatch::Exact
        } else if probe.extension == Some("ydlg") {
            ImportMatch::Possible
        } else {
            ImportMatch::Unsupported
        }
    }

    fn import(
        &self,
        source: ImportSource<'_>,
    ) -> Result<ImporterOutput<DialogueAsset>, Self::Error> {
        let text = std::str::from_utf8(source.bytes())
            .map_err(|_| DialogueError("dialogue is not UTF-8"))?;
        let body = text.strip_prefix("YDLG\n")
            .ok_or(DialogueError("missing YDLG header"))?;
        let lines = body.lines().map(str::to_owned).collect::<Vec<_>>();
        if lines.len() > self.max_lines {
            return Err(DialogueError("too many dialogue lines"));
        }

        let mut output = ImporterOutput::new(DialogueAsset { lines });
        output.cpu_bytes = u64::try_from(body.len()).ok();
        Ok(output)
    }
}
# Ok::<(), Box<dyn Error>>(() )
```

`probe` получает только prefix, ограниченный `max_probe_bytes`. Он должен быть
быстрым и не делать полного parse. Extension/media type являются hints:
структурная magic/version проверка должна давать более высокий score.

Registry не выбирает первый importer при равном лучшем score. Он возвращает
`ImportError::Ambiguous`, поэтому подключение второго plugin-а не меняет
результат скрыто из-за порядка регистрации.

## 4. Зарегистрируйте plugin явно

```rust
# use yuyib::prelude::*;
# struct DialogueAsset;
# struct DialogueImporter { max_lines: usize }
# #[derive(Debug)] struct E;
# impl std::fmt::Display for E { fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("error") } }
# impl std::error::Error for E {}
# impl AssetImporter<DialogueAsset> for DialogueImporter {
# type Error = E;
# fn descriptor(&self) -> ImporterDescriptor { ImporterDescriptor::new("my-game.dialogue", "1") }
# fn probe(&self, _: ImportProbe<'_>) -> ImportMatch { ImportMatch::Exact }
# fn import(&self, _: ImportSource<'_>) -> Result<ImporterOutput<DialogueAsset>, E> { Ok(ImporterOutput::new(DialogueAsset)) }
# }
let mut importers = ImporterRegistry::default();
importers.register(DialogueImporter { max_lines: 10_000 })?;
# Ok::<(), Box<dyn std::error::Error>>(() )
```

Для Internet-facing или user-selected content задайте меньшие limits:

```rust
use yuyib::assets::{ImporterRegistry, ImporterRegistryLimits};

# struct DialogueAsset;
let importers = ImporterRegistry::<DialogueAsset>::new(ImporterRegistryLimits {
    max_source_bytes: 2 * 1024 * 1024,
    max_probe_bytes: 512,
    max_dependencies: 64,
    max_diagnostics: 128,
    ..ImporterRegistryLimits::default()
})?;
# let _ = importers;
# Ok::<(), Box<dyn std::error::Error>>(() )
```

## 5. Low-level synchronous path

```rust
# use yuyib::prelude::*;
# struct DialogueAsset;
# let importers = ImporterRegistry::<DialogueAsset>::default();
let bytes = std::fs::read("assets/intro.ydlg")?;
let imported = importers.import(ImportSource::new("assets/intro.ydlg", &bytes))?;

// Здесь можно вызвать собственный cooker, создать dependency graph или
// передать neutral value в другую проверяемую стадию.
let _neutral_asset = imported.asset;
# Ok::<(), Box<dyn std::error::Error>>(() )
```

`import` синхронный: не вызывайте его в window event/render callback для
тяжёлого файла. Low-level означает контроль над scheduling, а не разрешение
блокировать кадр.

## 6. High-level AssetServer path

```rust,no_run
use std::sync::Arc;
use yuyib::prelude::*;

# struct DialogueAsset;
# let importers = ImporterRegistry::<DialogueAsset>::default();
let registry = Arc::new(importers);
let source = OwnedImportSource::new(
    "assets/intro.ydlg",
    std::fs::read("assets/intro.ydlg")?,
);
let mut assets = Assets::<DialogueAsset>::new();
let mut server = AssetServer::<DialogueAsset, ImportError>::new(
    TaskPoolConfig::new(2, 32)?,
)?;

let handle = server.try_import_bytes(
    &mut assets,
    registry,
    "Intro dialogue",
    source,
)?;

// Один раз на main-thread frame boundary; update не ждёт worker.
let update = server.update(&mut assets)?;
if update.ready.contains(&handle) {
    let dialogue = assets.get(handle).expect("ready handle");
    let metadata = assets.metadata(handle).expect("import metadata");
    let _ = (dialogue, metadata);
}
# Ok::<(), Box<dyn std::error::Error>>(() )
```

`try_import_bytes` импортирует уже прочитанные bytes в bounded worker pool.
Чтение файла в примере оставлено явным. Production resolver должен читать файл
асинхронно, проверять canonical root и затем передавать `OwnedImportSource`.

Для долгого parser-а переопределите `AssetImporter::import_with_context` и
проверяйте `ImportContext::is_cancelled()` между bounded units of work. Метод
`AssetServer::try_import_bytes_cancellable` возвращает `(handle,
ImportCancellation)`; `cancel()` выставляет cooperative signal. Это не
forceful thread preemption: trusted native plugin обязан дойти до проверки.

При успешной публикации metadata содержит source, `id@version`, dependencies,
CPU estimate и bounded diagnostics. Stable `AssetId` не меняется при переходе
`Loading → Ready/Failed`.

## Dependencies и diagnostics

Importer не должен сам угадывать project root. Верните request:

```rust
# use yuyib::prelude::*;
# struct Scene;
let mut output = ImporterOutput::new(Scene);
output.dependencies.push(ImportDependency {
    uri: "textures/wall_base.png".to_owned(),
    kind: ImportDependencyKind::Required,
});
output.diagnostics.push(ImportDiagnostic {
    code: "missing-normal".to_owned(),
    message: "normal map absent; compatible fallback material selected".to_owned(),
    severity: ImportDiagnosticSeverity::Warning,
});
```

Warning допустим только при определённом fallback. Повреждённая geometry,
невалидные indices, NaN transforms или превышение budget должны возвращать
`Err`, а не partial asset без явной семантики.

## Versioning и checklist

Перед публикацией importer crate проверьте:

- stable lowercase `id`, уникальный в registry данного output type;
- version меняется при изменении neutral output semantics;
- `probe` bounded и не выполняет полный parse;
- magic/version проверяется независимо от extension;
- все counts/depth/bytes/decompression ограничены;
- indices, numeric ranges и finite floats проверены;
- coordinate system и unit conversion документированы;
- dependencies только логические, без обхода resolver policy;
- unsupported features дают error либо documented diagnostic+fallback;
- importer не касается GPU, окна и ECS;
- есть valid, malformed, oversized и ambiguous-selection tests;
- wiki перечисляет supported versions, options, costs и limitations.

Canonical API reference: `yuyib_assets::AssetImporter`,
`yuyib_assets::ImporterRegistry` и `yuyib_assets::AssetServer::try_import_bytes`.

## Limits & Caveats

- Importer выполняет bounded CPU conversion и не владеет GPU, ECS или window.
- Dependency URI является request к host resolver, а не разрешением читать
  произвольный filesystem path.
- `probe` выбирает формат, но не заменяет полный validation pass.
- Diagnostics не должны превращать повреждённый или oversized input в
  неявно частичный asset.
