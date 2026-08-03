# Tutorial: первое окно (`Application`)

> **Статус:** Experimental  
> **Requires:** default `desktop-full` или feature `app`  
> **Цель:** создать native Windows window, выбрать render loop и понять frame callback

Это первый шаг учебного пути. Здесь нет ECS, assets и 3D — только shell приложения.

Канонический runnable:

```powershell
cargo run -p yuyib --example clear_window
```

## 1. Что создаём

Нужен объект, который:

- поднимает `winit` event loop;
- создаёт окно;
- создаёт WGPU surface / `Renderer`;
- каждый кадр даёт вам callback, затем рисует clear pass;
- корректно завершает process по `request_exit` или закрытию окна.

В Yuyib это **`Application`**, а не raw `EventLoop` + `Window` + `Renderer` вручную. Низкий уровень существует (`yuyib::platform`, `yuyib::render`), но для первого окна он лишний: вы бы сами связали resize, redraw и surface recovery.

## 2. Минимальный код

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
        .on_frame(|context| {
            // context.frame() — snapshot времени текущего кадра
            if context.frame().index == 600 {
                context.request_exit();
            }
        })
        .run()
}
```

Разберём **каждую** функцию.

### `Application::new() -> Application`

Создаёт builder с безопасными defaults:

| Default | Значение | Почему так |
|---|---|---|
| Размер окна | 1280×720, resizable | Удобный desktop start без fullscreen surprises |
| `RenderLoop` | `OnDemand` | Forms/editors не жгут GPU в idle |
| Clear color | тёмный linear RGBA | Видно, что surface жив, без «белого мигания» |

Возвращает **ещё не запущенный** builder. Окно и GPU появляются только внутри `run()`.

### `.window(WindowConfig) -> Self`

Задаёт title, размер, `WindowMode` (`Windowed` / `Borderless` / `Fullscreen`).

Почему не `Window::new` напрямую? Потому что `Application` **владеет** окном вместе с renderer lifecycle. Отдельное окно без host пришлось бы самим подключать к `resumed` / `Resized` / `RedrawRequested`.

`..Default::default()` оставляет размер/resizable из defaults и меняет только title — типичный beginner path.

### `.render_loop(RenderLoop::Continuous) -> Self`

| Вариант | Когда брать | Почему |
|---|---|---|
| `OnDemand` (default) | UI, editor, menu | Redraw только при событии / явном `request_redraw` |
| `Continuous` | Игры, live preview | Каждый wait event loop’а планирует кадр |

Для первого «живого» окна берём `Continuous`, иначе без input/resize экран может остаться на первом clear.

### `.on_frame(callback) -> Self`

Callback получает `&mut FrameContext<'_>`:

- `frame()` → immutable timing snapshot (`index`, delta, …);
- `runtime()` → lifecycle events;
- `request_exit()` → корректное завершение event loop.

Почему callback **до** clear pass? Чтобы приложение могло решить exit / UI / simulation **до** GPU work. Пока callback **не** получает mutable `Renderer`: foundation clear остаётся у host. Custom draw — через `on_render` (см. [Native Application](../guides/application.md)).

### `.run() -> Result<(), ApplicationError>`

**Блокирует** текущий thread до выхода из native event loop.

Почему `Result`? Surface loss, UI failure, WebView dispatch и другие host errors не panic’ают молча — они typed. Не вызывайте `run()` из worker или из самого frame callback.

## 3. Типичные расширения (ещё без Game)

```rust
use yuyib::{
    app::{Application, RenderLoop},
    platform::{WindowConfig, WindowMode},
    render::ClearColor,
};

Application::new()
    .window(WindowConfig {
        title: "Игра".to_owned(),
        mode: WindowMode::Fullscreen,
        ..Default::default()
    })
    .clear_color(ClearColor::linear(0.02, 0.03, 0.08, 1.0))
    .render_loop(RenderLoop::Continuous)
    .run()?;
```

- `ClearColor::linear(...)` — цвет в **linear** space (не sRGB байты 0–255).
- `WindowMode::Fullscreen` — borderless fullscreen на primary monitor без смены desktop resolution.

## 4. Когда переходить к `Game`

Оставьте `Application`, если нужен tool / shell / WebView host без ECS.

Переходите к [`Game`](first-game.md), когда нужны:

- один `World`;
- systems и schedules (`Startup` / `FixedUpdate` / `Update`);
- plugins для physics, 2D/3D scene, input.

`Game` **внутри** создаёт тот же window/GPU path, что `Application`. Это не второй движок.

## Limits & Caveats

- Один high-level window host на вызов `run`.
- Device-loss recovery пользовательских GPU resources пока на host.
- Public API Experimental — см. [API stability](../reference/api-stability.md).

## См. также

- Полный guide: [Native Application](../guides/application.md)
- Следующий tutorial: [Первая игра](first-game.md)
- Example: `crates/yuyib/examples/clear_window.rs`
