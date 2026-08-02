# RFC 0007 — capability features, plugins и игровые schedules

- **Статус:** accepted
- **Дата:** 2026-08-01
- **Зависит от:** RFC 0001, RFC 0005

## Решение

Curated crate `yuyib` использует capability features. Default
`desktop-full` сохраняет полный desktop development experience, но
`default-features = false` не подключает platform backend. Набор `headless`
содержит core, ECS, tasks, assets, physics/gameplay foundations и networking;
его normal dependency graph не может содержать Winit, WGPU, Wry/WebView или
audio device backend.

`Game` остаётся владельцем одного ECS `World`, но получает три явных schedule:

```text
on_start callback -> Startup once -> native event loop
presentation frame -> FixedUpdate zero or more bounded steps
                   -> Update exactly once
                   -> compatibility on_frame callback
                   -> renderer snapshot/pass
```

Это уточняет раннюю не-цель RFC 0005. Runtime не создаёт скрытый жанровый
scheduler: schedules публичны через `GameSchedule`, а ordering и systems
регистрирует приложение или `GamePlugin`. Callback API сохраняется как
низкоуровневый и migration path.

## Fixed update contract

`FixedUpdateConfig` задаёт ненулевой timestep, максимальное число шагов за
presentation frame и максимальный принимаемый frame delta. После исчерпания
step budget лишнее накопленное время отбрасывается наблюдаемым образом через
`FixedUpdateStats::dropped_time`; spiral of death не скрывается.

`FixedTime` доступен только как последний опубликованный fixed tick resource.
`GameTime` и `FixedUpdateStats` обновляются перед `Update`. Presentation-frame
delta нельзя использовать как замену fixed delta в deterministic physics.

## Plugin contract

`GamePlugin` — caller-owned configuration value. Он может добавлять resources,
systems и callbacks до запуска окна, но не получает implicit global state.
GPU initialization по-прежнему выполняется на renderer lifecycle boundary.

Plugins не являются dynamic ABI и не загружают arbitrary code из assets.
Поздняя динамическая plugin ABI потребует отдельного RFC по versioning,
memory ownership и security.

## Совместимость

- `Game` использует continuous rendering по умолчанию; on-demand остаётся
  явной настройкой для menu/turn-based приложений.
- Старые `on_start`, `on_frame`, `on_render`, window/device callbacks остаются.
- Low-level `Application`, raw ECS schedules и `RenderFrame` не скрываются.
