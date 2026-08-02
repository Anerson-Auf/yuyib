# Native Application

**Статус:** Experimental  
**Платформы:** Windows  
**Requires:** `yuyib::app`, `yuyib::platform`, `yuyib::render`

`Application` — high-level entry point для обычного native window с WGPU
surface. Он создаёт `winit` event loop, window и `Renderer`, вызывает ваш
callback перед clear pass и корректно завершает event loop по запросу.

## Простейшая конфигурация

```rust
use yuyib::{
    app::{Application, RenderLoop},
    platform::{WindowConfig, WindowMode},
    render::ClearColor,
};

fn main() -> Result<(), yuyib::app::ApplicationError> {
    Application::new()
        .window(WindowConfig {
            title: "Yuyib application".to_owned(),
            width: 1440,
            height: 900,
            resizable: true,
            mode: WindowMode::Windowed,
        })
        .clear_color(ClearColor::linear(0.02, 0.03, 0.08, 1.0))
        .render_loop(RenderLoop::Continuous)
        .on_frame(|context| {
            if context.frame().index == 600 {
                context.request_exit();
            }
        })
        .run()
}
```

## Режим окна

`WindowConfig::mode` выбирает один из трёх понятных начальных режимов:

- `WindowMode::Windowed` — обычное окно. Использует `width`, `height` и
  `resizable`.
- `WindowMode::Borderless` — обычное top-level окно без рамки и заголовка,
  растянутое точно на основной монитор. Это удобно для game-like приложений,
  которым не нужно Winit fullscreen-состояние; поля размера и `resizable` не
  учитываются.
- `WindowMode::Fullscreen` — полноэкранное окно без рамки на основном
  мониторе. Это удобный для приложений режим без переключения разрешения
  рабочего стола; поля размера и `resizable` также не учитываются.

Например, для игры достаточно поменять только режим:

```rust
use yuyib::platform::{WindowConfig, WindowMode};

let window = WindowConfig {
    title: "Моя игра".to_owned(),
    mode: WindowMode::Fullscreen,
    ..Default::default()
};
```

Если нужен особый размер окна без рамки, это уже низкоуровневая задача:
создайте окно через `yuyib::platform::winit` и задайте его правила напрямую.
Так высокоуровневый API не скрывает конфликтующие настройки.

Это тот же API, что использует
[`clear_window`](../../../../crates/yuyib/examples/clear_window.rs). Callback
получает `FrameContext`: `frame()` возвращает immutable timing snapshot, а
`runtime()` открывает `Runtime` для lifecycle events. `request_exit()` —
краткий путь к `Runtime::request_exit`.

## Render loop policy

`RenderLoop::OnDemand` — default. Он подходит forms, editors и большинству
desktop-apps: redraw запрашивается при старте, а future UI/renderer modules
будут сами запрашивать его после visual changes.

`RenderLoop::Continuous` запрашивает redraw после каждого event-loop wait. Он
подходит live views и games, но тратит GPU/CPU даже без визуальных изменений.
Выбирайте его только когда animation или simulation действительно требует
постоянного frame cadence.

## Что делает host

Во время `resumed` host создаёт `Window` и `Renderer`. На `Resized` он передаёт
physical size в `Renderer::resize`. На `RedrawRequested` порядок такой:

1. `Runtime::begin_frame` обновляет time и frame-boundary events.
2. Выполняется `on_frame`, если он зарегистрирован.
3. Если callback запросил exit, event loop завершается.
4. При feature `webview` Application опустошает configured outbound `PageEvent` FIFO.
5. `Renderer::clear` выполняет foundation clear pass.

`Application::ui(ApplicationUi::new(tree))` opt-in composes retained native UI
after your `on_render` callback in the same `RenderFrame`. Application owns
the window/renderer; the UI overlay is rendered with alpha blending and is not
a second window loop. It caches layout by presentation size and converts a UI
layout/render failure into `ApplicationError::Ui` rather than panicking.

Application имеет opt-in `ApplicationUi::with_winit_input` для explicit UI
adapter policy, но не владеет general text/IME/DPI policy, ECS schedules или
scene orchestration. Для custom scheduling или raw GPU integrations используйте
`yuyib-platform` и `yuyib-render` напрямую.

`on_window_event` exposes each original Winit event before Application's own
close/resize/redraw handling. Its `WindowEventContext` intentionally permits
only `request_exit` and `request_redraw`; it never exposes mutable renderer,
window or event-loop ownership. Forward events to `WinitUiAdapter` here for a
custom policy. The ergonomic path is
`ApplicationUi::with_winit_input(adapter, on_response)`: Application owns the
event routing, drains FIFO responses after `on_frame` and before `on_render`,
then draws the UI above `on_render`. The callback is synchronous on the UI
thread, so keep it small and store larger application state externally.

With facade feature `webview`, pass a validated `WebViewBuilder` or
`ApplicationWebView` to `Application::webview(...)`. Application creates it on
the UI thread after its parent window/renderer, owns resize/occlusion visibility
and drops it before the window. This high-level path uses full client bounds;
use `yuyib-webview` directly for split/custom rectangles.

## WebView: bounded host-to-page events

`ApplicationWebView::with_event_queue(capacity)` добавляет safe high-level
канал **host → current local page**. Он возвращает саму конфигурацию и
cloneable `ApplicationWebViewHandle`; отдайте конфигурацию в
`Application::webview`, а handle захватите в UI-thread callback. Никакого
`WebViewHost`, Wry handle или arbitrary JavaScript execution этот API не
открывает.

```rust
use yuyib::{
    app::{Application, ApplicationWebView},
    webview::{BridgeLimits, EndpointName, PageEvent, WebViewBuilder},
};

fn run(builder: WebViewBuilder) -> Result<(), Box<dyn std::error::Error>> {
    let (webview, page) = ApplicationWebView::new(builder).with_event_queue(32)?;
    let limits = BridgeLimits::default();
    let event_name = EndpointName::parse("ui.initialised")?;
    let mut sent = false;

    Application::new()
        .webview(webview)
        .on_frame(move |_| {
            if sent {
                return;
            }
            let Ok(session) = page.page_session() else {
                // NotReady before the native child/local typed bridge exists.
                return;
            };
            let event = PageEvent::from_typed(
                limits.protocol_version(),
                session,
                event_name.clone(),
                true,
                limits,
            );
            if let Ok(event) = event
                && page.enqueue(event).is_ok()
            {
                sent = true;
            }
        })
        .run()?;
    Ok(())
}
```

Получайте `PageSessionId` из `page.page_session()` непосредственно перед
`PageEvent::from_typed`. Событие привязано к этой session: попытка положить в
очередь event от предыдущей/другой page возвращает
`ApplicationWebViewCommandError::StaleSession`, а не доставляет данные не той
странице.

### Lifecycle, FIFO и redraw

- `capacity` должен быть больше нуля; на одном `ApplicationWebView` разрешена
  ровно одна queue. Queue — bounded FIFO: `enqueue` не блокирует и не
  отбрасывает старые events, а возвращает `Full { capacity }`.
- До создания native child с `LocalPage` и typed bridge handle возвращает
  `NotReady`; для page без local typed bridge — `NoLocalBridge`; после close,
  build failure или shutdown — `Closed`. Close очищает pending events.
- После каждого `on_frame` Application drains FIFO в порядке enqueue и только
  затем получает GPU frame. Нативная dispatch error завершает `run` как
  `ApplicationError::WebView`.
- Successful `enqueue` сам будит Winit event loop и планирует redraw, включая
  default `RenderLoop::OnDemand`; не вызывайте `request_redraw` и не включайте
  `RenderLoop::Continuous` только ради delivery. Event, положенный в
  `on_render`, поэтому будет отправлен на автоматически запланированном
  следующем redraw. Wakeups coalesced: пока pending FIFO не drained, queue
  держит не более одного outstanding Winit wake.
- Handle intentionally `!Send + !Sync` (внутри `Rc<RefCell<_>>`). Clone/use
  его только на native UI thread. Worker/network task должен передать owned
  domain message через собственный bounded channel; UI callback превращает
  его в `PageEvent` на следующем UI-thread boundary.

Page bootstrap receives each event as `CustomEvent("yuyib:event")`; payload is
validated JSON data, not executable script. Full bridge/security rules and
direct low-level host API: [WebView: Windows Phase 1](webview-windows.md).

## Limits & Caveats

- `Application::run` блокирует текущий thread до выхода из native event loop;
  не вызывайте его из per-frame callback или worker task.
- Сейчас допустим только один high-level window host на invocation `run`.
- Потерянный surface автоматически пересоздаётся. `ApplicationError::SurfaceLost`
  означает, что попытка recovery уже завершилась неудачно; device-loss recovery
  и rebuild пользовательских GPU resources пока остаются обязанностью host.
- Callback запускается до clear pass, но пока не получает renderer или current
  presentation texture. `on_render` получает scoped texture, а UI рисуется
  поверх него; отдельного custom UI phase ordering пока нет.
- Current UI overlay draws rectangles only. Text, keyboard/focus input, DPI
  conversion, clipping and accessibility remain separate layers. The raw
  window-event hook makes keyboard/pointer adapter integration possible, but
  ApplicationUi does not yet own it automatically.
- High-level outbound WebView queue does not perform navigation, retries,
  acknowledgements, request/response correlation or cross-thread scheduling.
  Treat `Full`, `StaleSession`, and later native dispatch failure as explicit
  application policy decisions.
