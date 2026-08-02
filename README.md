# Yuyib

Native-first Rust runtime для Windows: desktop applications, 2D/3D games и optional WebView.

Yuyib даёт high-level facades (`Application`, `Game`, `Game2dScene`, `Game3dScene`) поверх явных low-level границ: ECS (`bevy_ecs`), WGPU render graph, typed assets/importers и bounded background work. Editor — отдельный consumer публичных contracts, не второй runtime.

> **Статус:** Experimental / foundation. Public API и capabilities ещё меняются. Перед использованием сверяйте [Limits & Compatibility](docs/site/src/reference/limits-and-compatibility.md) и [KNOWN_ISSUES.md](KNOWN_ISSUES.md).

## Что уже есть

| Область | Кратко |
|---|---|
| Application / Game | Window, WGPU surface, frame callbacks, ECS world |
| Assets | Typed handles, importer registry, cook cache (glTF) |
| 2D | Sprites, atlas, tilemap foundation, animation |
| 3D | glTF/GLB import, PBR (IBL, shadows, bloom, FXAA, SSAO), character motor |
| Editor | Project/scene, hierarchy, Inspector, gizmo, process-isolated Play |
| Optional | Native UI, WebView2, audio, networking, Source 1 VMF slice |

## Быстрый старт

Требования: Windows, Rust toolchain (edition 2024), GPU с поддержкой WGPU.

```powershell
cargo run -p yuyib --example clear_window
```

Другие runnable examples — в [каталоге примеров](docs/site/src/reference/examples.md). Минимальный код:

```rust
use yuyib::{
    app::{Application, RenderLoop},
    platform::WindowConfig,
};

fn main() -> Result<(), yuyib::app::ApplicationError> {
    Application::new()
        .window(WindowConfig {
            title: "Моё приложение".to_owned(),
            ..Default::default()
        })
        .render_loop(RenderLoop::Continuous)
        .run()
}
```

Больше деталей: [Начало работы](docs/site/src/getting-started.md).

## Документация

| Документ | Назначение |
|---|---|
| [Wiki (mdBook)](docs/site/src/index.md) | Пользовательские guides и reference |
| [Architecture / ADR](docs/architecture/README.md) | Решения и invariants для maintainers |
| [Roadmap](docs/architecture/ROADMAP.md) | Milestones до Engine MVP |
| [Editor integration](docs/editor/ENGINE_INTEGRATION.md) | Authoring contract и workflow |
| [Editor status](docs/editor/ENGINE_HANDOFF.md) | Фактическое состояние Editor/engine slice |
| [Known issues](KNOWN_ISSUES.md) | Ограничения и открытые дефекты |
| [Contributing](CONTRIBUTING.md) | Как вносить изменения |

Сборка wiki локально (через `xtask`, когда нужно):

```powershell
cargo run -p xtask -- docs
```

Собранный book появляется в `docs/site/book/`.

## Структура репозитория

```text
crates/          Runtime, content, gameplay, authoring, editor
docs/
  architecture/  ADR + roadmap
  editor/        Editor contracts и status
  site/          mdBook wiki
editor-ui/       WebView shell (Monaco, hierarchy, Inspector)
editor_tests/    Fixture projects
for_tests/       Shared assets для examples/smokes
xtask/           Docs/tooling helpers
```

## Лицензия

MIT OR Apache-2.0. См. [LICENSE-MIT](LICENSE-MIT) и [LICENSE-APACHE](LICENSE-APACHE).
