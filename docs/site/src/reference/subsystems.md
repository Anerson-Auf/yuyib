# Карта подсистем и public API

> **Статус:** Current public capability map  
> **Для кого:** пользователи, которым нужен путь от задачи к crate и API

Yuyib предоставляет один facade crate `yuyib`. Его named modules re-export
специализированные crates; `yuyib::prelude` содержит только частые entry
points и намеренно не является полным API.

## Как читать карту

| Слой | Ответственность | Примеры |
|---|---|---|
| Host | event loop, window, frame lifecycle | `app`, `game`, `platform` |
| World | ECS state и gameplay simulation | `ecs`, `game_2d`, `game_3d`, `gameplay` |
| Content | CPU assets, import, validation | `assets`, `image`, `gltf`, `model`, Source 1 crates |
| Presentation | GPU/audio/UI output | `render`, `render_2d`, `render_3d`, `audio`, `ui_*` |
| Services | background work и transport | `tasks`, `net` |

## Application foundation

| Module | Когда использовать | Guide | Полный API |
|---|---|---|---|
| `yuyib::core` | frame clock, runtime events | [Runtime and events](../concepts/runtime-ecs-events.md) | [`yuyib_core`](../api/yuyib_core/index.html) |
| `yuyib::ecs` | entities, components, systems, resources | [Runtime and ECS](../concepts/runtime-ecs-events.md) | [`yuyib_ecs`](../api/yuyib_ecs/index.html) |
| `yuyib::platform` | window config and low-level window boundary | [Application](../guides/application.md) | [`yuyib_platform`](../api/yuyib_platform/index.html) |
| `yuyib::render` | WGPU surface, render frame and graph | [Low-level renderer](../guides/custom-render-passes.md) | [`yuyib_render`](../api/yuyib_render/index.html) |
| `yuyib::app` | standard native application loop | [Application](../guides/application.md) | [`yuyib_app`](../api/yuyib_app/index.html) |
| `yuyib::game` | ECS game lifecycle and schedules | [Game lifecycle](../guides/game-lifecycle.md) | [`yuyib_game`](../api/yuyib_game/index.html) |

## Assets and import

| Module | Когда использовать | Guide | Полный API |
|---|---|---|---|
| `yuyib::assets` | typed storage, importer registry and background publication | [Assets](../concepts/assets.md) | [`yuyib_assets`](../api/yuyib_assets/index.html) |
| `yuyib::image` | bounded PNG/JPEG/WebP decode | [Assets](../guides/assets.md) | [`yuyib_image`](../api/yuyib_image/index.html) |
| `yuyib::model` | validated CPU mesh/material model | [Model assets](../guides/model-assets.md) | [`yuyib_model`](../api/yuyib_model/index.html) |
| `yuyib::gltf` | glTF/GLB parsing, scene and animation data | [glTF import](../guides/gltf-import.md) | [`yuyib_gltf`](../api/yuyib_gltf/index.html) |
| `yuyib::model_assets` | model texture resolution/preparation | [Model assets](../guides/model-assets.md) | [`yuyib_model_assets`](../api/yuyib_model_assets/index.html) |
| `yuyib::scene` | imported scene to ECS entities | [Scene ECS](../guides/scene-ecs-and-interactions.md) | [`yuyib_scene`](../api/yuyib_scene/index.html) |

## 2D

| Module | Когда использовать | Guide | Полный API |
|---|---|---|---|
| `yuyib::two_d` | texture metadata, regions, sheets, animation | [2D concepts](../concepts/two-d.md) | [`yuyib_2d`](../api/yuyib_2d/index.html) |
| `yuyib::game_2d` | ECS sprites, tilemaps, culling, high-level scene | [Game2dScene](../guides/game-2d-scene.md) | [`yuyib_game_2d`](../api/yuyib_game_2d/index.html) |
| `yuyib::render_2d` | low-level sprite GPU upload/draw | [Sprites](../guides/sprites-and-animation.md) | [`yuyib_render_2d`](../api/yuyib_render_2d/index.html) |
| `yuyib::render_texture` | shared sampled GPU textures | [Textured materials](../guides/textured-materials.md) | [`yuyib_render_texture`](../api/yuyib_render_texture/index.html) |

## 3D

| Module | Когда использовать | Guide | Полный API |
|---|---|---|---|
| `yuyib::game_3d` | models, transforms, hierarchy, bounds, LOD | [3D transforms](../guides/3d-transforms.md) | [`yuyib_game_3d`](../api/yuyib_game_3d/index.html) |
| `yuyib::render_3d` | high/low-level 3D rendering and streamed glTF | [Game3dScene](../guides/game-3d-scene.md) | [`yuyib_render_3d`](../api/yuyib_render_3d/index.html) |
| `yuyib::character_3d` | fixed-step character movement/collision | [Character](../guides/input-character-quests.md) | [`yuyib_character_3d`](../api/yuyib_character_3d/index.html) |
| `yuyib::input` | semantic keyboard/camera adapters | [Free camera](../guides/free-camera.md) | [`yuyib_input`](../api/yuyib_input/index.html) |
| `yuyib::shader` | renderer-neutral WGSL contract | [Shaders](../guides/3d-and-shaders.md) | [`yuyib_shader`](../api/yuyib_shader/index.html) |

## Gameplay, physics and services

| Module | Когда использовать | Guide | Полный API |
|---|---|---|---|
| `yuyib::physics` | primitive colliders, raycasts and triangle mesh queries | [Physics](../guides/physics.md) | [`yuyib_physics`](../api/yuyib_physics/index.html) |
| `yuyib::gameplay` | actions, interactions, triggers, quests | [Gameplay](../concepts/gameplay.md) | [`yuyib_gameplay`](../api/yuyib_gameplay/index.html) |
| `yuyib::tasks` | bounded background CPU pool | [Tasks](../guides/tasks.md) | [`yuyib_tasks`](../api/yuyib_tasks/index.html) |
| `yuyib::audio` | decode and default-device playback | [Audio](../guides/audio.md) | [`yuyib_audio`](../api/yuyib_audio/index.html) |
| `yuyib::net` | bounded versioned TCP/JSON transport | [Networking](../guides/networking.md) | [`yuyib_net`](../api/yuyib_net/index.html) |

## Native UI and WebView

| Module | Когда использовать | Guide | Полный API |
|---|---|---|---|
| `yuyib::ui` | retained widget tree, layout and input | [Native UI](../guides/native-ui.md) | [`yuyib_ui`](../api/yuyib_ui/index.html) |
| `yuyib::ui_render` | UI rectangle rendering | [Native UI](../guides/native-ui.md) | [`yuyib_ui_render`](../api/yuyib_ui_render/index.html) |
| `yuyib::ui_text` | bounded shaping and measurement | [Native UI](../guides/native-ui.md) | [`yuyib_ui_text`](../api/yuyib_ui_text/index.html) |
| `yuyib::ui_text_render` | glyph rasterization/atlas/rendering | [Native UI](../guides/native-ui.md) | [`yuyib_ui_text_render`](../api/yuyib_ui_text_render/index.html) |
| `yuyib::webview` | optional local WebView2 surface | [WebView](../guides/webview-windows.md) | [`yuyib_webview`](../api/yuyib_webview/index.html) |

## Source 1 content

| Module | Responsibility | Полный API |
|---|---|---|
| `yuyib::vmf` | bounded VMF parser | [`yuyib_vmf`](../api/yuyib_vmf/index.html) |
| `yuyib::vmf_model` | convex brush to model compiler | [`yuyib_vmf_model`](../api/yuyib_vmf_model/index.html) |
| `yuyib::source1` | high-level VMF selection/compile adapter | [`yuyib_source1`](../api/yuyib_source1/index.html) |
| `yuyib::source1_scene` | VMF entity metadata to ECS | [`yuyib_source1_scene`](../api/yuyib_source1_scene/index.html) |
| `yuyib::vmt` | bounded VMT metadata parser | [`yuyib_vmt`](../api/yuyib_vmt/index.html) |
| `yuyib::vtf` | narrow VTF RGBA/BGRA decoder | [`yuyib_vtf`](../api/yuyib_vtf/index.html) |
| `yuyib::source1_assets` | safe material/texture resolver | [`yuyib_source1_assets`](../api/yuyib_source1_assets/index.html) |

Общий workflow описан в [Source 1 / Hammer](../guides/source1-vmf.md).

## Полнота reference

Таблицы перечисляют все 38 named facade modules (плюс сам facade crate
`yuyib`), но не копируют тысячи signatures. Embedded Rustdoc является
machine-generated inventory и
содержит **каждый** public module, type, trait, function, method, associated
constant и error variant. Такое разделение исключает рассинхронизацию ручной
Wiki с Rust source.
