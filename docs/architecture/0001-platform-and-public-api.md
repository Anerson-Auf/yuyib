# RFC 0001 — платформа, модульные API и границы первого релиза

- **Статус:** accepted
- **Дата:** 2026-07-31
- **Область:** Yuyib, personal Rust runtime для приложений и игр

## Решение

Yuyib строится как **native-first Windows runtime**. Он предоставляет три независимых верхнеуровневых API — `Application`, `Game` и `Web` — поверх общего ядра. Native UI является стандартным UI-решением; WebView подключается явно как дополнительная поверхность, а не определяет lifecycle или rendering model приложения.

Rust и система движка остаются единственным обязательным языком разработки. HTML/CSS/vanilla JavaScript, TypeScript и Tailwind допустимы только внутри включённого WebView-модуля. JSX, React, Node.js и frontend build chain не являются обязательными зависимостями проекта.

## Цели

- Один lifecycle и один event loop для desktop application, 2D/3D game и optional WebView.
- Высокоуровневый API с production-safe defaults и низкоуровневые escape hatches без форка runtime.
- Равноценные 2D и 3D пути rendering-а, допускающие смешение в одном окне.
- ECS-first gameplay model, в которой input, interactions, quests и networking не привязаны к конкретному жанру или кнопке.
- Асинхронные assets, observability и управляемое потребление RAM/VRAM с первого foundation.
- Плагины и feature flags: headless/server builds не должны тянуть windowing, GPU или WebView.

## Не-цели MVP

- Собственный браузер, JavaScript runtime или JSX-like обязательный язык.
- Собственный ECS и собственный physics solver: в первых версиях применяются зрелые backend-и за нашими фасадами.
- Собственный shader language, node shader editor, полноценный editor и visual scripting.
- Runtime-import `.blend`; Blender экспортирует в glTF на build/import этапе.
- Гарантия полной бинарной совместимости с proprietary map formats без отдельного изученного importer-плагина.

## Публичные уровни API

### Верхний уровень

`Application` создаёт desktop shell: окна, native UI, system dialogs, tray, lifecycle и composition surfaces.

`Game` добавляет ECS world, 2D/3D scenes, cameras, physics, audio, assets и gameplay plugins.

`Web` создаёт `WebSurface`: локальные assets или разрешённый URL, navigation policy и typed bridge. Он может быть размещён в `Application` или поверх/рядом с `GameSurface`.

Верхний уровень обязан позволять собрать минимальное приложение через один runtime builder и `run()`. Он не скрывает базовые объекты — разработчик может получить window handle, raw input, ECS schedule, GPU device и render graph при необходимости.

### Средний уровень

Содержит ECS schedules, assets/handles, scenes, materials, physics facade, audio, input actions, native UI tree и typed command/event bridge.

### Низкий уровень

Содержит platform handles, OS events, GPU resources, command encoders, render graph passes, custom task executors и transport interfaces. Низкий уровень следует стабилизировать медленнее верхних ergonomic API.

## Модульные границы

```text
yuyib-core       lifecycle, errors, events, diagnostics, time, task abstraction
yuyib-platform   Windows windowing, input, clipboard, dialogs, DPI
yuyib-ecs        ECS facade, schedules, commands, resources
yuyib-assets     asset handles, loaders, cache, async IO, import metadata
yuyib-render     WGPU backend, 2D/3D renderer, materials, render graph
yuyib-ui         native retained-mode UI and layout/composition
yuyib-physics    2D/3D physics facade and collision/query API
yuyib-audio      playback and spatial-audio facade
yuyib-gameplay   input actions, interactions, triggers, quest/state-machine plugin
yuyib-webview    optional WebView surfaces and typed bridge
yuyib-net        optional transport/replication primitives

yuyib-app        ergonomic Application facade
yuyib-game       ergonomic Game facade
yuyib-web        ergonomic Web facade
yuyib            curated prelude and compatible defaults
```

Нижний crate не зависит от верхнего. В частности, `core` не зависит от renderer, WebView или gameplay; `render` не зависит от `gameplay`; `webview` не получает прямой mutable-доступ к ECS world.

## 2D/3D renderer

Renderer использует единый GPU backend и render graph. UI — финальный compositing pass, а не debug overlay. 2D и 3D могут находиться в одном `World` и одном frame: sprites/tilemaps/2D cameras и meshes/PBR materials/3D cameras живут в отдельных render phases с общей asset и visibility инфраструктурой.

С первого MVP закладываются: sprite, texture atlas, sprite sheet, sequence animation, transforms, tilemap boundary, mesh, camera, light, material, texture, normal map, basic PBR/unlit paths и dev diagnostics. Система должна поддерживать assets как из одиночных файлов, так и из build-generated atlases.

## Shaders

WGSL — основной portable shader source format renderer-а. Поддержка иных языков возможна importer/build-plugin-ом, но не должна диктовать runtime API.

Есть три пути:

1. effect/material presets — параметризованные outline, glow, dissolve, blur, water, toon, 2D lighting и post-processing без shader source;
2. custom material/template — строго типизированные параметры и расширяемые vertex/fragment hooks над standard pipeline;
3. full pipeline — shader modules, layouts, buffers, compute, custom render graph passes.

ECS entity ссылается только на `Mesh`/`Sprite` и `Material` handles. Shader source, pipeline cache и GPU binding details принадлежат renderer-у. Это обязательно для batching, pipeline caching, validation и hot reload.

## Gameplay events

Input не равен gameplay. `ActionMap` преобразует клавиатуру, mouse, touch и gamepad в семантические actions. Interactions строятся на physics queries и emit-ят domain events.

```text
Input Action -> interaction query -> InteractionRequested
             -> Interactable handler -> DomainEvent
             -> quest/state machine -> side effect
```

`gameplay` — opt-in plugin. Его базовые сущности: `Interactable`, `Trigger`, `ActionMap`, conditions, effects, quest state. Он не вшивает RPG semantics в core. Networked game впоследствии реплицирует явно выбранные commands/domain events, а не ECS world целиком.

## Asset/import policy

glTF 2.0 — стандартный 3D interchange format: Blender models, meshes, UVs, PBR materials, textures, normals/tangents, rigs и animations.

Любой asset имеет import settings: include/exclude mesh, texture, normals/tangents, animation/skeleton, collision; manual/auto/off LOD; texture quality/mips; material replacement. Отключённые данные выбирают корректный fallback material, а не создают undefined rendering.

Map formats реализуются importer plugins, которые переводят входной формат в neutral scene representation. Первые приоритеты: Source 1 VMF/BSP, затем Source 2, но конкретный Source 2 pipeline будет отдельным RFC после исследования формата. Game assets, на которые у разработчика нет прав, не входят в репозиторий и не поставляются движком.

Поддержка editor-ов ориентируется не на бесконечный список проприетарных форматов, а на common interchange и plugin ABI:

- Blender -> glTF;
- Hammer Source 1 -> VMF/BSP importer;
- Hammer Source 2 -> отдельный importer;
- TrenchBroom/Quake-style editors -> MAP importer;
- Tiled -> TMX/JSON plugin;
- LDtk -> JSON plugin.

## Performance/streaming contract

Renderer/assets имеют observable budgets для RAM, VRAM, asset load time и draw calls. MVP закладывает frustum and distance culling, mip policy, instancing/batching, pipeline/material caches, async asset loading и manual/automatic LOD boundaries. World partition/HLOD и occlusion culling — последующие plugins; их API boundaries нельзя запирать архитектурой MVP.

## Documentation contract

Документация ведётся одновременно с API. Финальным артефактом будет статический HTML site с быстрым поиском, боковой навигацией и тёмной темой, по модели wiki.garrysmod.com.

Для каждой public функции/типа: назначение, минимальный runnable example, lifetime/threading model, platform scope, performance/RAM/VRAM implications, limits/caveats, fallback and error semantics, связи с feature flags и network behavior. Нормативные ограничения должны быть описаны в разделах `Limits & Caveats`, а не спрятаны в source comments.

## Принятые допущения

- Первый target — Windows; platform API остаётся переносимым по форме.
- GPU backend выбран как WGPU-compatible abstraction; platform-specific implementation не входит в public API.
- Native UI — default; WebView — optional feature and separate capability boundary.
- Asset pipeline важнее количества форматов: новые editor/formats добавляются plugins без изменения ECS или renderer contracts.
