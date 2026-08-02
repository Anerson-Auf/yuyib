# Начало работы

**Статус:** Experimental  
**Платформы:** Windows

Текущий минимальный путь создаёт native window, инициализирует WGPU surface и
отрисовывает clear pass. Готовый runnable example находится в
[`crates/yuyib/examples/clear_window.rs`](../../../crates/yuyib/examples/clear_window.rs).

## Запуск примера

Из корня workspace выполните:

```powershell
cargo run -p yuyib --example clear_window
```

Остальные готовые vertical slices собраны в
[каталоге запускаемых примеров](reference/examples.md). Если нужный import
недоступен, сначала проверьте [Cargo feature map](reference/features.md).

Окно называется `Yuyib — first GPU surface`. Пример использует continuous
render loop и самостоятельно запрашивает корректное завершение после 600
frame callbacks. Закрытие окна через системную кнопку также завершает process.

## Минимальное приложение

Для локального проекта добавьте `yuyib` как dependency (пока workspace/path
dependency), затем создайте entry point:

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

`Application::new()` создаёт resizable window размером 1280×720, использует
`RenderLoop::OnDemand` и тёмный clear color. Подробнее о callbacks, scheduling
и failure modes — в [руководстве Native Application](guides/application.md).

Для игры с ECS-миром используйте `Game::new()` — он оставляет тот же window и
GPU lifecycle, но даёт `World` в `on_start` и `on_frame`. Минимальный путь и
границы с низкоуровневым renderer-ом описаны в
[руководстве игры](guides/game-lifecycle.md).

## Limits & Caveats

Public API пока Experimental. Несовместимые изменения будут отмечаться в
changelog и [API stability](reference/api-stability.md). Актуальные дефекты —
[KNOWN_ISSUES](../../../KNOWN_ISSUES.md).
