# Каталог запускаемых примеров

> **Статус:** Current examples map  
> **Расположение:** `crates/yuyib/examples`  
> **Запуск:** из корня workspace

Examples — executable documentation. Выберите ближайший use-case, запустите
его без изменений, затем переносите минимально нужные части в проект.

```powershell
cargo run -p yuyib --example <name>
```

Оконные examples интерактивны. Они не являются CI-тестами; visual behavior и
управление проверяются пользователем.

## Application, UI и infrastructure

| Example | Что показывает | Feature / ресурсы |
|---|---|---|
| `clear_window` | минимальное native WGPU window | default или `app` |
| `game_plugin_schedule` | `GamePlugin`, startup/update/fixed schedules | `game` |
| `render_graph_phases` | порядок custom render phases | `app` |
| `native_ui_gallery` | native widgets, layout, input and text | `ui` + локальный font/assets |
| `application_webview` | local WebView page and typed bridge | `webview`, Windows WebView2 |
| `asset_server_streaming` | background load/publication state | `assets` |
| `custom_importer` | registration/probe/import diagnostics | `assets` |
| `world_interaction_flow` | headless Enter/Stay/Exit, hold progress and authority request | `gameplay`, no fixtures/window |

## 2D

| Example | Что показывает | Feature / ресурсы |
|---|---|---|
| `sprite_atlas_ecs` | atlas, ECS sprite and animation | `two-d` |
| `offline_sprite_atlas` | typed bounded offline atlas import and runtime binding (headless) | `two-d` |
| `two_d_tile_playground` | tilemap, camera, animation and kinematic collision | `two-d` + example textures |

## 3D и glTF

| Example | Что показывает | Feature / ресурсы |
|---|---|---|
| `game_3d_scene` | procedural model, light and high-level renderer | `app`, `three-d` |
| `gltf_material_policy` | high-level `ModelMaterialPolicy` on `GltfSceneLoad`, diagnostics summary, left/right repaired panels | `app`, `three-d`, **no external fixtures** |
| `gltf_material_usage` | material usage inventory + remap all users of `material_0` by name (no mesh indices) | `app`, `three-d`, **no external fixtures** |
| `gltf_unbound_material_fallback` | explicit unbound-primitive fallback via `ModelMaterialPolicy` (no silent white material) | `app`, `three-d`, **no external fixtures** |
| `gltf_texture_diagnostics` | texture usage inventory + unused/external/missing-UV-set importer diagnostics | `app`, `three-d`, **no external fixtures** |
| `frame_capture_smoke` | headless `OffscreenRenderer` + fixed `Camera3d` cube draw + PNG capture | `three-d`, `two-d`, **no window / no external fixtures** |
| `street_city_m1_smoke` | asset-backed M1 smoke: street city + grounded spawn + walk-clip animation + skinned draw + fixed-camera PNG with soft reference metrics | `three-d`, `two-d`, **no window**, needs street city + character GLBs |
| `static_navigation_queries` | headless static collider, walkable graph, nearest/reachability/path queries and typed no-path telemetry | `three-d` |
| `gltf_map_static_scene` | import and ECS materialization of static map | test GLB path |
| `gltf_map_loading_screen` | responsive load screen and bounded GPU publication | test GLB path |
| `gltf_pbr_lab` | textured PBR material path and diagnostics | `sci-fi_lab.glb` fixture |
| `animation_crossfade` | fixture-free clip transition, mid-fade retarget and pose output | `three-d`, no window/assets |
| `animated_girl_preview` | imported animation preview | animated GLB fixture |
| `velina_skeletal_preview` | skeletal rendering path | matching character GLB |
| `playable_vertical` | input + character motor vertical slice | 3D test assets |
| `cyberpunk_city_playable` | streamed street city + character; optional Rapier props overlay | street city + character GLB; `--features physics-rapier` for overlay |
| `playable_dynamics_overlay_smoke` | M4.7–M4.9 headless Rapier playable overlay (solid trimesh + props) | `--features "three-d,physics-rapier"` |
| `equirect_hdr_smoke` | CPU Radiance/linear equirect ingest + hemisphere sample check | none (synthetic) |
| `ggx_ibl_cook_smoke` | CPU GGX cook equirect → cube mips + BRDF LUT + `Game3dScene` probe/sky queue | none (synthetic; needs `for_tests/`) |
| `asset_cook_cache_smoke` | M3 disk cook cache: second glTF import is a hit (parse skipped) | none (synthetic) |
| `physics_rapier_smoke` | M4.1 Rapier facade: dynamic sphere settles on fixed cuboid | `--features physics-rapier` |
| `physics_rapier_kinematic_smoke` | M4.3 kinematic platform + trigger overlap + CCD | `--features physics-rapier` |
| `physics_rapier_joints_smoke` | M4.4 fixed/revolute joints + collision groups | `--features physics-rapier` |
| `physics_rapier_convex_smoke` | M4.5 convex hull + limited prismatic joint | `--features physics-rapier` |
| `physics_rapier_contacts_smoke` | M4.6 contact pairs + rope + `DynamicsFixedStepper3d` | `--features physics-rapier` |
| `playable_dynamics_overlay_smoke` | M4.7–M4.9 playable Rapier overlay (solid trimesh + props) | `--features "three-d,physics-rapier"` |
| `physics_rapier_window` | M4.2–M4.6 Rapier window: full dynamics lab | `--features "app,three-d,physics-rapier"` |
| `skybox_smoke` | fullscreen cubemap skybox from cooked outdoor mip0 (+Y vs −Y luma) | none (synthetic) |
| `directional_shadow_smoke` | orthographic directional shadow map darkens occluded ground (factor PBR) | none |
| `pbr_alpha_mask_smoke` | PBR `MASK` discards occluder shading + shadow cutout vs opaque | none |
| `specular_ibl_smoke` | factor-only prefiltered specular IBL probes (headless PNG) | none |

Точные asset paths находятся в начале каждого source file. Если fixture
отсутствует, example должен завершиться явной import/I/O error, а не молча
подменить контент.

## От задачи к объяснению

| После example прочитайте | Guide |
|---|---|
| `clear_window`, `render_graph_phases` | [Application](../guides/application.md), [custom passes](../guides/custom-render-passes.md) |
| `asset_server_streaming`, `custom_importer` | [Asset loading](../guides/asset-loading.md), [custom importers](../guides/custom-importers.md) |
| `two_d_tile_playground` | [Game2dScene](../guides/game-2d-scene.md), [tilemap physics](../guides/tilemap-kinematic-physics.md) |
| `offline_sprite_atlas` | [offline atlas manifest](../guides/offline-sprite-atlas.md), [custom importers](../guides/custom-importers.md) |
| `game_3d_scene` | [Game3dScene](../guides/game-3d-scene.md), [3D transforms](../guides/3d-transforms.md) |
| `gltf_material_policy` | [3D objects](../guides/3d-objects-transforms.md), [Streamed glTF](../guides/streamed-gltf-scene.md) |
| `gltf_material_usage` | [3D objects](../guides/3d-objects-transforms.md), [Streamed glTF](../guides/streamed-gltf-scene.md) |
| `static_navigation_queries` | [Static navigation queries](../guides/static-navigation.md) |
| glTF loading examples | [Streamed glTF](../guides/streamed-gltf-scene.md) |
| `animation_crossfade` | [Animation cross-fade](../guides/animation-crossfade.md) |
| playable examples | [Input and character](../guides/input-character-quests.md) |
| `world_interaction_flow` | [World interactions](../guides/world-interactions.md) |

## Limits & Caveats

- Examples optimize clarity of one vertical slice, not reusable application
  architecture.
- Paths and expected diagnostic counts may depend on fixtures in `for_tests`.
- Default features compile a broad graph; use the [feature map](features.md)
  when переносите example в минимальный downstream crate.
- Команды здесь запускают interactive applications. Для compile-only проверки
  используйте конкретный `--example` с подходящей Cargo check command.
