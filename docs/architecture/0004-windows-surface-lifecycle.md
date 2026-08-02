# RFC 0004 — Windows window и GPU surface lifecycle

- **Статус:** accepted
- **Дата:** 2026-07-31
- **Зависит от:** RFC 0001, RFC 0003

## Решение

Первый GPU vertical slice использует stable `winit 0.30.12` application model и `wgpu 30`. `yuyib-platform` владеет event loop и private `Arc<winit::window::Window>`; `yuyib-render` владеет instance, surface, adapter, device, queue и surface configuration; facade координирует OS events and render lifecycle. Renderer не знает ECS, UI или gameplay.

Surface создаётся через safe WGPU API с передачей `Arc<Window>` по значению. Это позволяет получить `Surface<'static>` без `unsafe` lifetime extension: WGPU surface сохраняет handle source, а window остаётся доступным платформенному слою.

## Lifecycle

```text
EventLoop::new -> run_app
  resumed -> create PlatformWindow -> initializing renderer -> ready
  Resized/ScaleFactorChanged -> record physical size
  RedrawRequested -> configure if needed -> acquire -> encode -> submit -> present
  CloseRequested -> orderly exit
```

Window создаётся только из `ApplicationHandler::resumed` через `ActiveEventLoop::create_window`. Rendering выполняется только в `WindowEvent::RedrawRequested`. `about_to_wait` запрашивает новый redraw в game loop mode; normal native applications используют wait-driven loop и не получают постоянный redraw без потребности.

## Surface states

Renderer имеет явные состояния `Initializing`, `Ready`, `Minimized`, `Recovering` и `Failed`. Нулевой physical size не конфигурируется и не рендерится: это `Minimized`, а не ошибка.

Low-level acquire semantics WGPU 30 должны быть представлены в нейтральном `RenderStatus`, а не утекать в high-level game API:

- `Presented`;
- `SkippedMinimized`;
- `SkippedTimeout`;
- `SkippedOccluded`;
- `Reconfigured`;
- `SurfaceRecreated`.

`Outdated` reconfigure-ит surface и пропускает current frame. `Suboptimal` допускает present, затем planned reconfiguration. `Lost` пересоздаёт surface. Device loss требует controlled device recovery и invalidation/rebuild GPU resources. Перед `configure` не должно существовать live acquired texture; в противном случае WGPU может panic.

## Public low-level boundary

Renderer даёт borrow-only access к device/queue/configuration в closure. Он не передаёт ownership WGPU surface или lifecycle mutable state наружу. Custom render passes регистрируются в graph с declared dependencies, не вызывают present и не меняют global configuration.

## Limits and caveats

- MVP поддерживает один renderer на одно window. Multi-window — отдельное расширение, не скрытая гарантия.
- Первое создание GPU device асинхронно. Однократный `pollster::block_on` допустим только в demo/startup; production host не должен блокировать main event loop во время gameplay.
- WGPU backend policy не считается публичным contract. Windows build проверяется на DX12/Vulkan, а fallback behavior документируется отдельно.
- `winit 0.30` и `wgpu 30` имеют несовместимые с устаревшими tutorials APIs. В частности, `request_adapter` возвращает `Result`, а surface acquire/present имеет новый status-driven flow.
