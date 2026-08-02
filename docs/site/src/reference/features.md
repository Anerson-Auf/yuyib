# Cargo features и выбор API surface

> **Статус:** Current feature map  
> **Crate:** `yuyib`  
> **Версия:** `0.1.0`

Cargo features определяют, какие named modules facade crate доступны в вашем
проекте. Default feature set — `desktop-full`; для server/headless tools есть
отдельный `headless`. Выключайте defaults только осознанно: import path может
быть корректным в Wiki, но отсутствовать в вашей сборке из-за feature flags.

## Обычная desktop-игра

Внутри этого workspace:

```toml
[dependencies]
yuyib = { path = "../yuyib/crates/yuyib" }
```

Default `desktop-full` включает game host, 2D, 3D, audio, networking, Source 1,
native UI и gameplay. WebView намеренно не включён.

## Минимальная конфигурация

```toml
[dependencies]
yuyib = {
    path = "../yuyib/crates/yuyib",
    default-features = false,
    features = ["game", "three-d"]
}
```

Cargo автоматически включает dependency features (`app`, `render`,
`platform`, `ecs`, `assets`, `tasks`, `physics`), нужные для выбранных
capabilities.

## Готовые наборы

| Feature | Что включает | Когда выбирать |
|---|---|---|
| `desktop-full` | `game`, `two-d`, `three-d`, `audio`, `net`, `source1`, `ui`, `gameplay` | стандартная native desktop-игра/приложение |
| `headless` | `core`, `ecs`, `tasks`, `assets`, `physics`, `gameplay`, `net` | server, importer tool, simulation без window/GPU |
| `webview` | `app` + optional WebView2 host | только если нужен HTML/CSS surface |

## Capability features

| Feature | Public modules | Важные dependencies |
|---|---|---|
| `core` | `yuyib::core` | — |
| `ecs` | `yuyib::ecs` | pinned `bevy_ecs` facade |
| `tasks` | `yuyib::tasks` | fixed bounded worker pool |
| `assets` | `yuyib::assets` | `tasks` |
| `physics` | `yuyib::physics` | `ecs` |
| `gameplay` | `yuyib::gameplay` | `ecs`, `physics` |
| `net` | `yuyib::net` | transport-specific async stack |
| `audio` | `yuyib::audio` | platform audio backend |
| `platform` | `yuyib::platform` | native window backend |
| `render` | `yuyib::render` | `platform`, WGPU |
| `app` | `yuyib::app` | `core`, `platform`, `render` |
| `game` | `yuyib::game` | `app`, `ecs` |
| `ui` | `yuyib::ui`, `ui_render`, `ui_text`, `ui_text_render` | `app` |
| `two-d` | `two_d`, `game_2d`, `image`, `render_2d`, `render_texture` | assets/ECS/physics/render |
| `three-d` | `character_3d`, `game_3d`, `gltf`, `input`, `model`, `model_assets`, `render_3d`, `scene`, `shader` | assets/ECS/physics/render |
| `source1` | `source1`, `source1_assets`, `source1_scene`, `vmf`, `vmf_model`, `vmt`, `vtf` | `three-d` |

## Проверить доступность type

1. Найдите type в [карте подсистем](subsystems.md).
2. Посмотрите module в первой колонке.
3. Убедитесь, что соответствующий feature включён напрямую или через готовый
   набор.
4. Проверьте точный path в [`yuyib` rustdoc](../api/yuyib/index.html).

`yuyib::prelude` — convenience layer. Отсутствие type в prelude не означает,
что API недоступен: импортируйте его из named module.

## Limits & Caveats

- `default-features = false` действительно удаляет невыбранные modules из
  facade на compile time.
- `webview` Windows-specific и не является частью `desktop-full`.
- Feature availability не означает platform verification: текущая verified
  desktop platform — Windows.
- Feature graph является public build contract foundation-версии, но всё ещё
  имеет статус Experimental.

