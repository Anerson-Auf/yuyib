# Почему выбранный стек и архитектурные решения

Ниже — короткая, но практичная карта: **что выбрано**, **почему так**, и **что это даёт для Editor/Rust-проектов в будущем**.

## 1) Базовый смысл: почему это не случайный набор либ

Сейчас проект строится по принципу:

- **Runtime first**: ядро (ECS/asset/renderer/physics) — отдельное от редактора.
- **Editor как потребитель runtime contract**, а не второй runtime.
- **Одна source-of-truth для authored-проектов** (scene/project/assets) через стабильные форматы.
- **Никаких альтернативных декодеров/схем в редакторе**, чтобы preview совпадал с тем, как игра реально запускается.

Это видно в RFC:
- `docs/architecture/0001-platform-and-public-api.md`
- `docs/architecture/0011-editor-authoring-contract.md`
- `docs/editor/ENGINE_INTEGRATION.md`

## 2) Основные либы и почему они такие

### `bevy_ecs` (через `yuyib-ecs`)
- Где: `Cargo.toml` (workspace), `crates/yuyib-ecs`.
- Причина:
  - не пишем свой ECS с нуля (дорого в поддержке и bug‑сюрпризах),
  - уже есть проверенный schedule/commands/system model,
  - есть хорошее API-сочетание с runtime-подходом (schedules, системы, resources),
  - через facade (`yuyib-ecs`) мы изолируем проект от API-shocks.
- Почему не Legion/shipyard/выпилить в пользу self-made: для этого уже есть рабочий, battle-tested экосистемный backend.

### `winit`
- Где: `Cargo.toml`, `crates/yuyib-platform`, `crates/yuyib-render`.
- Причина:
  - единый Windows event-loop/окно/ввод для `Application` и `Game`,
  - хорошая совместимость с рендером и WebView,
  - меньше “самописной платформенной кости”.
- Почему не SDL2 (тогда бы пришлось строить и поддерживать отдельную интеграцию во всех слоях отдельно под текущий webview/renderer путь).

### `wgpu`
- Где: `Cargo.toml`, `crates/yuyib-render`.
- Причина:
  - единый GPU API-слой для DX12/Vulkan-подхода и будущего расширения,
  - хороший баланс между перформансом и переносимостью,
  - позволяет держать `render graph` и low-level control в одном месте.
- Почему не OpenGL/Direct3D напрямую:
  - больше платформенных “граблей” и сильнее привязка к одной графической семье,
  - труднее удерживать единый контракт для всех слоёв.

### `rapier2d/rapier3d`
- Где: `Cargo.toml`, `crates/yuyib-physics`, `crates/yuyib-game-2d`, `crates/yuyib-game-3d`.
- Причина:
  - готовый физический backend для 2D/3D с Rust-ориентированной экосистемой,
  - меньше времени на базовую физику и больше на геймплей/интеграцию.
- Почему не писать свою физику: это самый дорогой путь с постоянными краями-сюрпризами (стабильность столкновений, сетка, детерминизм).

### `monaco-editor`
- Где: `editor-ui/package.json`, `editor-ui/src/main.js`.
- Причина:
  - код-редактор для Rust из коробки (highlighting/syntax/folding/диагностики workflow),
  - критично для roadmap: без “свистелки” без LSP/форматирования редактор фактически непригоден.
- Почему не plain textarea / code mirror:
  - недобирает качество редакторского опыта сразу (completion/diagnostics/rename/hover/signature).

### `wry` (опционально, feature-gated)
- Где: `crates/yuyib-webview/Cargo.toml`, `docs/site/src/concepts/webview-architecture.md`.
- Причина:
  - позволяет строить WebView UI поверх родного окна без превращения проекта в Tauri,
  - хорошая база для `window.yuyib.post(...)` bridge и строгого протокола.
- Почему не Node/Tauri как основной путь: это меняло бы архитектурную цель `native-first` в сторону полноценного app-framework, что не соответствует текущей цели.

### `serde` + `serde_json`
- Где: почти везде.
- Причина:
  - единый типизированный контракт для registry manifests, scene/project docs, diagnostics.
- Прямая польза: меньше ручного парсинга, лучше миграции и ревью.

### `uuid`, `blake3`, `atomic-write-file`, `command-group`
- Где: `crates/yuyib-authoring`, `yuyib-editor-core`.
- Причина:
  - `uuid`: устойчивые authored-идентификаторы (Entity/Asset/Scene/Project),
  - `blake3`: стабильные revision/hash‑цепочки для conflict detection,
 - `atomic-write-file`: безопасные сохранения без порчи файлов при падении,
  - `command-group`: контроль и корректное завершение процессов (play/cargo).

### `pollster`
- Где: `crates/yuyib-render`, `crates/yuyib-editor`.
- Причина: удобный и маленький способ вызвать async-пути рендера/инициализации из sync call без полноценного runtime.

## 3) Ключевые архитектурные решения (вместо либ)

### 1. `Editor != Runtime`
Почему важно: если редактор станет вторым runtime, он начнёт дублировать semantics, расходовать память/времени и уходить в рассинхрон.

Текущее правило: редактор материализует authored data, запускает play в process isolation, не пытается “владеть” runtime ECS.

### 2. Stable IDs + Versioned schema
`CapabilityId`, `ComponentSchemaId`, `AssetGuid`, `EntityGuid`, `SchemaVersion`.

- Это защищает от миграций и конфликтов при rename/move/обновлениях.
- Без этого все сохранения ломаются от переименований путей и изменений API.

### 3. Persistent authored docs + opaque preservation
- `.yscene` сохраняет semantically stable state, а неизвестные поля/capabilities round-trip’ятся как payload.
- Нельзя просто терять неизвестные данные при save.

### 4. Command layer с revision/undo/redo
- Все мутации сцены проходят через commands (а не через произвольный mutate),
- даёт atomicity, conflict detection, coalescing, откат и UI history.

### 5. Preview через production importer/cooker
- Один pipeline для preview и play: нет “editor-only” декодеров.
- Иначе вы получаете невалидное подтверждение того, что будет в игре.

### 6. Process isolation (`yuyib-play`, scoped cargo check)
- crash editor не роняет при запуске игры/проверке,
- один Cargo-процесс одновременно (без гонок/перекрытий).

## 4) Что было бы “неправильным” (короткий anti-pattern)

- Dynamic Rust DLL ABI как основной плагин-механизм — высокий риск ABI-стабильности и security-риск.
- Открывать `scene` как runtime-world dump — ломает совместимость и Undo/Play/диагностику.
- Считать `Entity`/`TypeId`/процессный handle stable identity — это рвёт сохранения при restart.

## 5) Мини-расписание обучения (чтобы не утонуть)

1) Сначала понять **форматы и контракты**:
   - `docs/editor/SCENE_FORMAT.md`
   - `docs/editor/SOURCE_OF_TRUTH.md`
2) Потом — **runtime contract**:
   - `docs/architecture/0011-editor-authoring-contract.md`
   - `crates/yuyib-authoring/*`
3) Потом — **editor shell + bridge**:
   - `crates/yuyib-editor/*`, `docs/site/src/concepts/webview-architecture.md`
4) Потом — **asset/import + preview pipeline**:
   - `crates/yuyib-assets`, `crates/yuyib-gltf`, `crates/yuyib-gltf-authoring`

После каждого шага делай маленький тест/пример:
- открыть/сохранить `.yscene`,
- изменить transform через команду,
- задепрекейтить/добавить поле и проверить migration,
- прогнать scoped `cargo check` и Play.

Так ты не “пересканиваешь весь проект”, а строишь опорную карту от 1 к 1.
