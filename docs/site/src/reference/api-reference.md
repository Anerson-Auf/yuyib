# API Reference и покрытие

> **Статус:** Current public API map  
> **Source of truth:** public Rust doc comments + local `rustdoc`

Эта страница — индекс, не вручную поддерживаемая копия каждой сигнатуры. Она
гарантирует, что у каждой доступной public capability есть discoverable entry
в wiki, guide/limit context и canonical Rust reference.

## Полный API reference

```powershell
cargo run -p xtask -- docs
```

Команда создаёт wiki и Rustdoc в одном static site. После неё откройте
[`api/yuyib/index.html`](../api/yuyib/index.html): это entry point facade.
Reference остальных public crate’ов расположен рядом, например
`api/yuyib_render/index.html`. В отличие от ручного списка, Rustdoc содержит
каждый публичный module, type, trait, method, error и constant.

Pipeline документирует только crates из таблицы ниже; internal tooling и
transitive dependencies намеренно не публикуются как часть API Yuyib.

## Покрытие public API

| Facade/module | Public entry points | Wiki context | Limits / статус |
|---|---|---|---|
| `yuyib::app` | `Application`, event hook, native UI and feature-gated `ApplicationWebView::with_event_queue` / UI-thread `ApplicationWebViewHandle` | [Native Application](../guides/application.md) | Windows; WebView is a full-client native overlay; outbound events are bounded FIFO, auto-wake redraw and `!Send`/`!Sync` — Experimental |
| `yuyib::game` | `Game`, `GameFrame`, подготовка ECS-мира и игровой callback | [Игра: окно, ECS и кадр](../guides/game-lifecycle.md) | Один `World`, один window lifecycle; physics/renderer/schedule выбирает приложение — Experimental |
| `yuyib::core` | `Runtime`, `FrameClock`, `FrameEvents<E>`, `FrameInfo`, `RuntimeEvent` | [Runtime, ECS и события](../concepts/runtime-ecs-events.md) | Frame-boundary delivery — Experimental |
| `yuyib::tasks` | `TaskPool`, `TaskPoolConfig`, typed `Task<T>` and bounded spawn/shutdown errors | [Tasks](../guides/tasks.md) | Fixed workers; blocking drain on drop; no cancellation/async runtime — Experimental |
| `yuyib::ecs` | `prelude`, `bevy_ecs` re-export | [Runtime, ECS и события](../concepts/runtime-ecs-events.md) | Backend facade; API follows pinned dependency — Experimental |
| `yuyib::assets` | `Assets<T>`, `AssetId<T>`, `AssetLoader`, `AssetLoadQueue` | [Assets и импорт](../concepts/assets.md), [загрузка без остановки](../guides/asset-loading.md) | `AssetLoader` публикует CPU-значения в основном потоке; GPU/ECS остаются явным шагом — Experimental |
| `yuyib::audio` | `AudioClip`, `DecodedAudio`, `AudioEngine`, `AudioPlaybackHandle` | [Audio playback](../guides/audio.md) | Explicit default-device lifecycle; encoded bytes bounded, decoded duration caller-owned — Experimental |
| `yuyib::platform` | `WindowConfig`, `Window`, platform errors | [Native Application](../guides/application.md) | Host-thread lifecycle — Experimental |
| `yuyib::render` | `Renderer`, `RenderFrame`, `ClearColor`, `RenderStatus` | [Low-level renderer](../guides/custom-render-passes.md) | Surface status/recovery is explicit — Experimental |
| `yuyib::two_d` | `Texture`, `TextureSize`, `TextureRegion`, `SpriteSheet`, `SpriteAnimation`, `SpriteAnimationState`, `PlaybackMode` | [2D-ресурсы](../concepts/two-d.md), [sprites](../guides/sprites-and-animation.md) | Metadata only; bounds validated — Experimental |
| `yuyib::image` | `DecodePolicy`, `ImageFormatPolicy`, `ImageFormat`, `DecodedImage`, decode errors | [Assets](../guides/assets.md) | PNG/JPEG/WebP allow-list and resource budgets — Experimental |
| `yuyib::gltf` | model/scene import, constrained skins/transform animation, sampled poses, options/limits and structured errors | [glTF import](../guides/gltf-import.md), [glTF scene data](../guides/standard-material-and-scenes.md) | Strict and explicit preview policies; ECS materialization lives in `yuyib::scene`; no morph-target animation — Experimental |
| `yuyib::model` | `Model`, `Mesh`, `MeshPrimitive`, `Material`, texture/material indices and bindings | [3D model assets](../guides/model-assets.md) | Static CPU data only; no glTF/Hammer/GPU importer — Experimental |
| `yuyib::game_3d` | `Transform3d`, `Model3d`, deterministic model/light snapshots and `LodGroup3d` selection | [3D GPU mesh path](../guides/3d-renderer.md) | Renderer-neutral; no direct GPU bridge or residency — Experimental |
| `yuyib::game_2d` animation | `AnimatedSprite2d`, deterministic step and frame/finish events | [2D ECS animation](../guides/ecs-sprite-animation.md) | No asset/GPU/timestep ownership — Experimental |
| `yuyib::game_2d` sprites | `Sprite2d`, full-world extraction and bounded viewport-culling snapshot | [2D ordinary sprite culling](../guides/sprite-viewport-culling.md) | Conservative rotated AABB; CPU-only, no spatial index/occlusion — Experimental |
| `yuyib::game_2d` scene | `Game2dScene`, bounded residency/upload/culling/batching diagnostics | [High-level 2D scene](../guides/game-2d-scene.md) | No file I/O on render thread; missing assets degrade explicitly — Experimental |
| `yuyib::render_3d` scene | `Game3dScene`, hierarchy/camera/light policy, frustum culling, partial PBR-map binding, blend classification, persistent caches and bounded Lambert/PBR residency | [High-level 3D scene](../guides/game-3d-scene.md), [streamed glTF](../guides/streamed-gltf-scene.md) | Cached per-mesh bounds and culling telemetry; partial core PBR sets and effectively-opaque exporter blends are handled, IBL/shadows remain planned — Experimental |
| `yuyib::game_2d` tilemaps | `TileMap2d`, deterministic snapshots and bounded tile→static-AABB adapter | [2D tilemaps](../guides/tilemaps.md), [kinematic collision](../guides/tilemap-kinematic-physics.md) | One atlas; static AABB prototype, no cache/broadphase/navmesh — Experimental |
| `yuyib::game_3d` lighting | `DirectionalLight3d`, extracted stable light snapshot | [Lambert lighting](../guides/lit-materials.md) | Directional lights only; no shadows/clustering — Experimental |
| `yuyib::game_3d` hierarchy | local/world transforms, `Parent3d`, propagation and errors | [3D transforms](../guides/3d-transforms.md), [Scene ECS](../guides/scene-ecs-and-interactions.md) | `WorldTransform3d` is derived; bounds/colliders are separate snapshots; no children cache/automatic despawn policy — Experimental |
| `yuyib::game_3d` static scene collision | `build_static_scene_collider_3d`, `StaticSceneCollider3d` and explicit extracted-scene variant | [Scene ECS](../guides/scene-ecs-and-interactions.md) | Exact static triangles, currently linear query cost; no moving meshes or broadphase — Experimental |
| `yuyib::render_2d` | `Camera2d`, `SpriteDraw`, `SpriteRenderer`, upload/render errors | [2D-ресурсы](../concepts/two-d.md) | One-texture batch contract — Experimental |
| `yuyib::render_3d` | `Camera3d`, mesh/scene renderers, textured Lambert/PBR, skeletal root transforms/depth composition и standard-material selection | [3D GPU mesh path](../guides/3d-renderer.md), [streamed glTF](../guides/streamed-gltf-scene.md), [3D and shaders](../guides/3d-and-shaders.md) | Direct-light normal-mapped PBR + sorted textured blend; skeletal geometry upload is still atomic; no IBL/shadows/instancing — Experimental |
| `yuyib::render_texture` | `TextureCache`, `GpuTexture`, sampler config and upload errors | [Textured materials](../guides/textured-materials.md) | RGBA8 sampled 2D, one mip and explicit refresh — Experimental |
| `yuyib::model_assets` | `ModelTextureLoader`, bindings and safe release | [Lambert lighting](../guides/lit-materials.md) | Local files only; explicit material binding — Experimental |
| `yuyib::scene` | `spawn_scene`, selection, node/camera/light source metadata | [Scene ECS](../guides/scene-ecs-and-interactions.md) | One selected scene; light direction sync explicit — Experimental |
| `yuyib::gameplay::interaction_3d` | use-raycast config, outcomes and requests | [Scene ECS](../guides/scene-ecs-and-interactions.md) | Sphere-only O(n) query; no authority/controller — Experimental |
| `yuyib::gameplay::interaction_2d` | pointer/touch action config, explicit outcomes and `InteractionLayer2d` | [2D pointer interaction](../guides/interaction-2d.md) | Inclusive AABB hit, O(n), no screen conversion/broad phase/authority — Experimental |
| `yuyib::gameplay::world_interaction` | typed target, Enter/Stay/Exit, press/hold progress and authority-request bridge | [World interactions](../guides/world-interactions.md) | One selected target/action per state; spatial selection and authority remain explicit — Experimental |
| `yuyib::render_3d` lit path | `TexturedLitMeshRenderer3d`, `LitSceneRenderer3d`, `LambertLighting3d` | [Lambert lighting](../guides/lit-materials.md) | Lambert only; no normal maps/PBR/shadows — Experimental |
| `yuyib::render_3d` standard path | `StandardMaterial3d`, resource bridge and `StandardRenderer3d` | [Standard material](../guides/standard-material-and-scenes.md) | No PBR or mask/blend material phase — Experimental |
| `yuyib::shader` | `ShaderSource`, `ShaderProgram`, `ShaderPrototype`, explicit interfaces | [3D: scenes, materials и shaders](../guides/3d-and-shaders.md) | Configuration only; no compile/reflection/hot reload — Experimental |
| `yuyib::gameplay` | semantic actions, interactables, typed world-interaction lifecycle, triggers and events | [Gameplay](../concepts/gameplay.md) | No input binding, hidden physics or authority; specialised 2D/3D query adapters are explicit — Experimental |
| `yuyib::input` | `KeyboardActionMap`, `KeyBinding`, `WinitKeyboardAdapter` and input updates/errors | [Input, character motor и quests](../guides/input-character-quests.md) | Winit physical keyboard only; focus cancellation is explicit — Experimental |
| `yuyib::character_3d` | fixed-step motor, automatic indoor/outdoor spawn, triangle-mesh collision, input/configuration, transition events and ECS sync | [Input, character motor и quests](../guides/input-character-quests.md), [streamed glTF](../guides/streamed-gltf-scene.md) | Static triangle world; no dynamic-body solver or broadphase — Experimental |
| `yuyib::gameplay::quest` | definitions, objective counters, `QuestBook`, signals, transitions and snapshot | [Input, character motor и quests](../guides/input-character-quests.md) | No serialization, UI, authority or replication policy — Experimental |
| `yuyib::physics` | 2D/3D sphere and AABB queries, triangle-mesh raycast/contact and static-only 2D kinematic AABB sweep | [Physics](../guides/physics.md) | No dynamic solver/general CCD/broadphase/OBB — Experimental |
| `yuyib::vmf` | bounded Source 1 VMF parser, typed map/brush views, preserved generic blocks and structured diagnostics | [Source 1 / Hammer](../guides/source1-vmf.md) | Text VMF only; no BSP/Source 2/numeric plane conversion — Experimental |
| `yuyib::vmf_model` | convex brush inputs, bounds and deterministic brush-to-`Model` compiler | [Source 1 / Hammer](../guides/source1-vmf.md) | No VMT/VTF/UV/lightmaps/displacements/entity runtime — Experimental |
| `yuyib::source1` | Source 1 VMF plane adapter, selection/origins and one-call brush `Model` compilation | [Source 1 / Hammer](../guides/source1-vmf.md) | Geometry only; no entity ECS/VMT/VTF/BSP/Source 2 — Experimental |
| `yuyib::source1_scene` | VMF entity metadata, selection/origin policy and ECS materialization | [Source 1 / Hammer](../guides/source1-vmf.md) | No prop/brush binding, output routing or gameplay semantics — Experimental |
| `yuyib::vmt` | bounded Source 1 VMT KeyValues parse and typed material metadata | [Source 1 / Hammer](../guides/source1-vmf.md) | VMT crate has no filesystem/VTF resolution, include/patch behavior or GPU material; `source1_assets` is the separate local resolver — Experimental |
| `yuyib::vtf` | bounded VTF 7.2 RGBA/BGRA 2D decoder | [Source 1 / Hammer](../guides/source1-vmf.md) | No compressed formats, resources, VPK/filesystem or Source 2 — Experimental |
| `yuyib::ui` | retained widget tree, tokens, layout, pointer and keyboard focus actions | [Native UI](../guides/native-ui.md) | No text editor/IME/accessibility implementation — Experimental |
| `yuyib::ui_render` | UI rectangle draw list, explicit CPU/WGPU rectangular clipping, and alpha-blended WGPU pass | [Native UI](../guides/native-ui.md) | No text/images/nested clip stack/scrolling/DPI/window loop — Experimental |
| `yuyib::ui_text` | bounded explicit-font text shaping, visual glyph runs and metrics | [Native UI](../guides/native-ui.md) | No rasterizer/atlas/system fallback/editor/IME — Experimental |
| `yuyib::ui_text_render` | bounded explicit-font glyph rasterizer, RGBA8 atlas/UV quads, and explicit WGPU glyph pass | [Native UI](../guides/native-ui.md) | No UiTree binding/nested clipping/scrolling/fallback/editor/IME — Experimental |
| `yuyib::webview` (feature) | locked local pages, bounded typed router and current-session page events | [WebView Phase 1](../guides/webview-windows.md) | Windows overlay; no GPU texture or raw host capabilities — Experimental |
| `yuyib::source1_assets` | safe VMT base-texture path resolution and VTF-to-RGBA8 boundary | [Source 1 / Hammer](../guides/source1-vmf.md) | No GPU cache/VPK/includes/PBR/Source 2 — Experimental |
| `yuyib::net` | `FrameCodec`, `WireFrame`, explicit Tokio TCP bind/connect/accept, `JsonConnection` | [Networking Phase 1](../guides/networking.md) | TCP only; no runtime, global queue, UDP/reliability/ECS replication/auth/TLS — Experimental |

`yuyib::prelude` re-exports common starting types. Набор неполный:
reference для specialised API всегда начинается с named module.

## Проверка покрытия при изменении

Перед тем как считать public API change законченным:

1. запустите `cargo run -p xtask -- docs` и проверьте docs нового item;
2. обновите его строку выше или добавьте новую;
3. добавьте/обновите guide и `Limits & Caveats`, когда user-visible semantics
   нельзя вывести из одной signature;
4. пометьте страницу правильным API status.

Полная policy: [как устроена эта документация](../wiki/documentation-contract.md).
