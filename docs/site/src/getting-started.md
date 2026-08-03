# Начало работы

**Статус:** Experimental  
**Платформы:** Windows

Этот файл — **короткий старт**. Если вы хотите понять *почему* вызывается каждая
функция, сразу откройте [учебный путь](tutorials/learning-path.md).

## Что поставить

1. Rust toolchain (edition 2024 workspace).
2. Windows + GPU driver для WGPU.
3. Клон репозитория Yuyib; dependency пока обычно `path` / workspace member.

Минимальные features: `app` для окна; `game` для ECS; `two-d` / `three-d` для
соответствующих сцен. Карта features: [Cargo features](reference/features.md).

## 30 секунд: пустое окно

```powershell
cargo run -p yuyib --example clear_window
```

Окно `Yuyib — first GPU surface`, continuous loop, exit после ~600 кадров или
по системной кнопке закрытия.

## Минимальное приложение

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

| Вызов | Зачем |
|---|---|
| `Application::new()` | Builder с defaults (1280×720, OnDemand, тёмный clear) |
| `.window(...)` | Title / размер / `WindowMode` |
| `.render_loop(Continuous)` | Игре нужны кадры без ожидания input |
| `.run()` | Blocking native event loop; `Result` при surface/UI ошибках |

Пошаговый разбор: [Tutorial — первое окно](tutorials/first-window.md).  
Полный surface (UI, WebView, callbacks): [Native Application](guides/application.md).

## Минимальная игра

```rust,ignore
Game::new()
    .window(WindowConfig { title: "Моя игра".into(), ..Default::default() })
    .add_plugin(MyPlugin)
    .run()?;
```

`Game` = тот же window/GPU host + один ECS `World` + schedules
`Startup` / `FixedUpdate` / `Update`.  
Tutorial: [Первая игра](tutorials/first-game.md).

## Куда дальше по цели

| Цель | Открыть |
|---|---|
| Учиться с нуля по шагам | [Учебный путь](tutorials/learning-path.md) |
| Загрузить 3D карту без freeze | [Tutorial glTF](tutorials/load-gltf-scene.md) |
| 2D sprite / tilemap | [Tutorial 2D](tutorials/first-2d-playable.md) |
| Найти API по задаче | [Что вы хотите сделать?](wiki/use-case-index.md) |
| Запустить готовый example | [Каталог примеров](reference/examples.md) |

## Limits & Caveats

Public API — Experimental. Актуальные дефекты:
[KNOWN_ISSUES](../../../KNOWN_ISSUES.md). Стабильность:
[API stability](reference/api-stability.md).
