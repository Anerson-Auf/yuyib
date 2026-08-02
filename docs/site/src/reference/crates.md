# Каталог crates

> **Статус:** Current workspace map  
> **Версия:** `0.1.0` foundation

| Public facade | Workspace crate | Назначение | Status |
|---|---|---|---|
| `yuyib::app` | `yuyib-app` | Высокоуровневый lifecycle native Windows-приложения. | Experimental |
| `yuyib::game` | `yuyib-game` | Lifecycle игры: окно, ECS-мир, schedules и кадр. | Experimental |
| `yuyib::assets` | `yuyib-assets` | Типизированные generational assets, import и publication. | Experimental |
| `yuyib::audio` | `yuyib-audio` | Bounded audio sources и воспроизведение через default device. | Experimental |
| `yuyib::core` | `yuyib-core` | Runtime, frame clock и события на границе кадра. | Experimental |
| `yuyib::tasks` | `yuyib-tasks` | Bounded CPU/background pool с фиксированными workers. | Experimental |
| `yuyib::ecs` | `yuyib-ecs` | Public facade над `bevy_ecs`. | Experimental |
| `yuyib::platform` | `yuyib-platform` | Настройка окна и низкоуровневая граница `winit`. | Experimental |
| `yuyib::render` | `yuyib-render` | Lifecycle WGPU surface, render frame и render graph. | Experimental |
| `yuyib::two_d` | `yuyib-2d` | Metadata textures, regions, sprite sheets и animation. | Experimental |
| `yuyib::game_2d` | `yuyib-game-2d` | ECS sprites/tilemaps, culling, animation и high-level 2D scene. | Experimental |
| `yuyib::image` | `yuyib-image` | Bounded декодирование PNG/JPEG/WebP. | Experimental |
| `yuyib::gltf` | `yuyib-gltf` | Bounded import glTF/GLB models, scenes и animation. | Experimental |
| `yuyib::model` | `yuyib-model` | Валидированные CPU meshes и metadata материалов. | Experimental |
| `yuyib::model_assets` | `yuyib-model-assets` | Безопасный resolve/decode и подготовка model textures. | Experimental |
| `yuyib::game_3d` | `yuyib-game-3d` | ECS models, transforms, hierarchy, bounds и LOD. | Experimental |
| `yuyib::character_3d` | `yuyib-character-3d` | Fixed-step character movement и collision с картой. | Experimental |
| `yuyib::physics` | `yuyib-physics` | Primitive colliders, raycasts и triangle-mesh queries. | Experimental |
| `yuyib::render_2d` | `yuyib-render-2d` | GPU upload textures и instanced sprite rendering. | Experimental |
| `yuyib::render_3d` | `yuyib-render-3d` | GPU caches и high/low-level 3D rendering paths. | Experimental |
| `yuyib::render_texture` | `yuyib-render-texture` | Общие sampled GPU texture resources. | Experimental |
| `yuyib::scene` | `yuyib-scene` | Явное materialization glTF scene в ECS. | Experimental |
| `yuyib::shader` | `yuyib-shader` | Renderer-neutral contracts для WGSL programs. | Experimental |
| `yuyib::vmf` | `yuyib-vmf` | Bounded parser Source 1 VMF/KeyValues. | Experimental |
| `yuyib::vmf_model` | `yuyib-vmf-model` | Deterministic compiler convex brushes в model. | Experimental |
| `yuyib::source1` | `yuyib-source1` | High-level VMF selection и brush-model compile. | Experimental |
| `yuyib::source1_scene` | `yuyib-source1-scene` | VMF entity metadata и origin-to-ECS adapter. | Experimental |
| `yuyib::vmt` | `yuyib-vmt` | Bounded parser Source 1 VMT metadata. | Experimental |
| `yuyib::vtf` | `yuyib-vtf` | Проверенный узкий decoder VTF 7.2 RGBA/BGRA. | Experimental |
| `yuyib::source1_assets` | `yuyib-source1-assets` | Безопасный VMT/VTF material-texture resolver. | Experimental |
| `yuyib::ui` | `yuyib-ui` | Retained native UI tree, layout и input. | Experimental |
| `yuyib::ui_render` | `yuyib-ui-render` | WGPU rendering прямоугольников native UI. | Experimental |
| `yuyib::ui_text` | `yuyib-ui-text` | Bounded text shaping и measurement. | Experimental |
| `yuyib::ui_text_render` | `yuyib-ui-text-render` | Glyph rasterization, atlas и WGPU text quads. | Experimental |
| `yuyib::webview` | `yuyib-webview` | Optional Windows WebView2 child host. | Experimental |
| `yuyib::gameplay` | `yuyib-gameplay` | Semantic actions, interactions, triggers и quests. | Experimental |
| `yuyib::input` | `yuyib-input` | Адаптеры physical input в semantic actions. | Experimental |
| `yuyib::net` | `yuyib-net` | Bounded versioned TCP frames и typed JSON transport. | Experimental |

## Limits & Caveats

Facade re-exports crates for convenience, но feature/module availability и
точные signatures определяются rustdoc соответствующего workspace crate.
`yuyib::prelude` — curated import set, не обещание, что весь API доступен без
module path.
