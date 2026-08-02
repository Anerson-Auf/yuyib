# Scoped verification Yuyib Editor integration

Этот документ задаёт gates для authoring boundary. Имена конкретных crates/tests
должны быть заменены реальными после реализации; наличие command template здесь
не означает, что соответствующий crate уже существует.

## Общий порядок

Разработка идёт маленькими increments:

```text
implementation
  -> scoped static check изменённого crate
  -> focused unit/contract tests
  -> один vertical integration test
  -> короткий отчёт о выполненном и пропущенном
```

Не накапливайте большой Editor diff до первой проверки. Не запускайте весь
workspace только ради формальной полноты, если scoped tests и Clippy уже покрыли
изменённый boundary.

Для Rust/Cargo:

- только один Cargo process одновременно;
- каждая Cargo-команда получает `CARGO_BUILD_JOBS=2`;
- tests получают `RUST_TEST_THREADS=2` или `--test-threads=2`;
- default scope — изменённый crate, конкретный test или затронутый example;
- не использовать `cargo test --workspace`, `cargo check --workspace`,
  `--all-targets` без отдельной явной просьбы;
- не запускать `xtask` без новой явной просьбы пользователя;
- не запускать оконный/interactive Editor или example без явной просьбы;
- перед тяжёлой командой сообщить scope, resource limits и timeout;
- после timeout завершать только подтверждённое process tree этой команды и не
  повторять её, пока прежние `cargo`/`rustc`/linker/test processes не завершены.

PowerShell template для будущего scoped check:

```powershell
$env:CARGO_BUILD_JOBS = "2"
cargo check -p <changed-crate>
```

Focused test template:

```powershell
$env:CARGO_BUILD_JOBS = "2"
$env:RUST_TEST_THREADS = "2"
cargo test -p <changed-crate> <focused_test_name> -- --test-threads=2
```

В handoff всегда указывайте точные реально выполненные команды. Template не
следует копировать в отчёт как evidence.

## Contract test matrix

### Registry и coverage

- Все curated runtime capabilities имеют machine-readable record.
- Duplicate capability/schema/import-settings/plugin/system IDs дают hard error.
- `Visual` без commands/schema/materializer отклоняется.
- `Asset` без settings/preview/diagnostics отклоняется.
- `CodeOnly` имеет docs/source navigation, а не пустой placeholder.
- Human report и Editor palette детерминированно строятся из одного registry.
- Feature combination без authoring не тянет Editor/WebView/code-workspace
  dependencies.

### Scene round-trip и compatibility

Focused fixtures должны покрывать:

1. current `load -> save -> load` equivalence;
2. deterministic `save -> load -> save` для known records;
3. unknown component opaque preservation;
4. unknown field/envelope extension preservation;
5. newer unsupported version без silent deletion;
6. каждую поддерживаемую migration edge;
7. missing migration с typed diagnostic/read-only behavior;
8. duplicate entity/asset/schema GUID rejection;
9. broken GUID reference diagnostics;
10. parser size/count/depth limits до unbounded allocation.

Asset-specific fixtures:

- rename/move source сохраняет `AssetGuid`;
- content edit меняет hash/cache key, но не GUID;
- failed reimport сохраняет последний валидный result;
- import settings проходят independent schema migration;
- selective invalidation затрагивает только declared dependencies.

### Commands и conflicts

- Inspector/gizmo/hierarchy не мутируют document в обход command layer.
- Transaction либо применяется целиком, либо не меняет document.
- Undo/redo восстанавливает values, GUID references, dirty state и revision.
- Continuous drag/text edits coalesce только в пределах одного merge key/
  gesture.
- Concurrent/stale base revision отклоняется или открывает conflict flow.
- External file change никогда silently не перезаписывается.
- Cancelled preview/import command не публикует поздний stale result.
- `Apply Play Mode Changes` создаёт обычную transaction и принимает только
  adapter whitelist properties.
- Derived/runtime-only components не возвращаются в authored scene.

### Preview fidelity и budgets

Для одного reference asset проверяется полный путь:

```text
source -> production importer -> cooker/neutral data
       -> preview adapter -> production renderer route
```

Нужны проверки:

- Editor не регистрирует alternative glTF/image decoder;
- importer/cooker version и settings входят в preview cache key;
- cancellation работает на read/decode/cook/publication phases;
- progress denominator не меняется случайно между phases;
- source/decode/diagnostic/dependency limits применяются;
- GPU publication ограничена slot/primitive/byte budget;
- cache hit не выполняет повторный import/decode;
- settings/source/dependency change invalidates правильный subset;
- RAM/VRAM estimates и oversized transaction diagnostics наблюдаемы;
- material/mesh/animation clip selection соответствует imported metadata;
- collision, normals, tangents, UV и bounds overlays используют те же neutral
  data, а не повторный parser;
- material override preview обратим и не меняет source asset;
- PBR/render preset совпадает с Play Mode route;
- missing texture channels и fallback material объяснены diagnostics.

Для cyberpunk/reference map нужен regression fixture, который позволяет найти
все meshes/primitives, использующие выбранный material (например,
`material_0`), и объясняет расхождение source metadata с фактическим draw route.
Visual golden добавляется только после стабилизации camera/render preset; до
этого structured assertions обязательны и не заменяются субъективным взглядом.

### Materialization и Play runner

- Одинаковый authored revision materializes deterministically.
- Mapping `EntityGuid -> Entity` и `AssetGuid -> AssetId<T>` не протекает в
  persisted data.
- Required unknown component блокирует Play с diagnostic; optional unknown
  record сохраняется.
- Derived transforms/caches вычисляются обычными runtime systems.
- Runner запускается отдельным process и получает конкретную project/scene
  revision.
- Runner panic/crash/device failure не завершает Editor и не меняет document.
- Stop/restart не оставляет orphan processes/resources.
- Asset/scene edit не требует Rust rebuild.
- Rust-code edit проходит через scoped check/build и controlled runner restart.
- Stale runner result не применяется после смены Editor revision.

### SystemDescriptor и code workspace

- Stable system IDs unique; plugin ownership и schedule присутствуют.
- Read/write component IDs разрешаются в capability registry.
- `Find readers`/`Find writers` возвращают зарегистрированные systems.
- Source `file:line` не появляется в `.yscene` и не используется как identity.
- Перемещение source file может обновить navigation metadata без scene
  migration.
- Component -> adapter -> system -> plugin navigation не требует repository scan.
- Code workspace использует mature editor component и LSP diagnostics; простое
  textarea не закрывает test/milestone.
- Scoped Cargo action сериализует Cargo processes, поддерживает cancellation,
  timeout и ограничение build jobs.

### External files и save safety

- Save проверяет last-seen external revision/content fingerprint.
- Dirty + external change всегда создаёт conflict state.
- Reload, save-as и explicit overwrite являются разными commands.
- Atomic save не оставляет target частично записанным при simulated failure.
- File watcher event сам не мутирует open document.
- Conflict resolution сохраняет unknown opaque records.

## Первый vertical integration scenario

Первый Editor milestone проверяется одним реальным 3D flow:

1. Создать/открыть project.
2. Импортировать glTF через production importer с bounded progress.
3. Просмотреть mesh/material/texture/animation diagnostics.
4. Выбрать material и найти использующие его meshes.
5. Включить bounds/normals/tangents/UV/collision overlays.
6. Добавить asset в scene и изменить transform gizmo/Inspector-ом.
7. Сохранить, закрыть и открыть scene; GUID и values совпадают.
8. Изменить import setting и выполнить non-destructive reimport.
9. Запустить isolated Play runner с тем же renderer preset.
10. Остановить runner без изменения authored scene.
11. Перейти от component-а к adapter-у и читающим/пишущим systems.
12. Открыть source в mature code workspace и выполнить scoped check.

Сценарий не закрыт, если его шаг требует редактировать generated Rust-код для
обычного placement/scale/material change или запускать отдельный custom example.

## Когда расширять scope

После focused tests допустим один затронутый integration crate/example. Полный
workspace имеет смысл только при изменении общих feature graphs, public facade
или release gate и только по явной просьбе пользователя.

UI/viewport visual smoke обычно требует интерактивного окна; его выполняет
пользователь. Maintainer может ограничиться scoped compilation и перечислить
непроверенное interactive behavior.

## Формат краткого отчёта

```text
Increment: <capability/adapter>
Changed: <owning crates/docs>
Stable IDs/schemas: <IDs + versions or none>
Preview/materialization: <path>
Checks run: <exact commands>
Not run: <interactive/full-workspace/heavy checks>
Known limits: <open gaps for this increment>
Coverage: <status and generated record>
```

Отчёт не заявляет «всё проверено», если были только unit tests, и не запускает
heavy checks для компенсации отсутствующего focused contract test.
