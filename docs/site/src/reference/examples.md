# РљР°С‚Р°Р»РѕРі Р·Р°РїСѓСЃРєР°РµРјС‹С… РїСЂРёРјРµСЂРѕРІ

> **РЎС‚Р°С‚СѓСЃ:** Current examples map  
> **Р Р°СЃРїРѕР»РѕР¶РµРЅРёРµ:** `crates/yuyib/examples`  
> **Р—Р°РїСѓСЃРє:** РёР· РєРѕСЂРЅСЏ workspace

Examples вЂ” executable documentation. Р’С‹Р±РµСЂРёС‚Рµ Р±Р»РёР¶Р°Р№С€РёР№ use-case, Р·Р°РїСѓСЃС‚РёС‚Рµ
РµРіРѕ Р±РµР· РёР·РјРµРЅРµРЅРёР№, Р·Р°С‚РµРј РїРµСЂРµРЅРѕСЃРёС‚Рµ РјРёРЅРёРјР°Р»СЊРЅРѕ РЅСѓР¶РЅС‹Рµ С‡Р°СЃС‚Рё РІ РїСЂРѕРµРєС‚.

```powershell
cargo run -p yuyib --example <name>
```

РћРєРѕРЅРЅС‹Рµ examples РёРЅС‚РµСЂР°РєС‚РёРІРЅС‹. РћРЅРё РЅРµ СЏРІР»СЏСЋС‚СЃСЏ CI-С‚РµСЃС‚Р°РјРё; visual behavior Рё
СѓРїСЂР°РІР»РµРЅРёРµ РїСЂРѕРІРµСЂСЏСЋС‚СЃСЏ РїРѕР»СЊР·РѕРІР°С‚РµР»РµРј.

## Application, UI Рё infrastructure

| Example | Р§С‚Рѕ РїРѕРєР°Р·С‹РІР°РµС‚ | Feature / СЂРµСЃСѓСЂСЃС‹ |
|---|---|---|
| `clear_window` | РјРёРЅРёРјР°Р»СЊРЅРѕРµ native WGPU window | default РёР»Рё `app` |
| `game_plugin_schedule` | `GamePlugin`, startup/update/fixed schedules | `game` |
| `render_graph_phases` | РїРѕСЂСЏРґРѕРє custom render phases | `app` |
| `native_ui_gallery` | native widgets, layout, input and text | `ui` + Р»РѕРєР°Р»СЊРЅС‹Р№ font/assets |
| `application_webview` | local WebView page and typed bridge | `webview`, Windows WebView2 |
| `asset_server_streaming` | background load/publication state | `assets` |
| `custom_importer` | registration/probe/import diagnostics | `assets` |
| `world_interaction_flow` | headless Enter/Stay/Exit, hold progress and authority request | `gameplay`, no fixtures/window |

## 2D

| Example | Р§С‚Рѕ РїРѕРєР°Р·С‹РІР°РµС‚ | Feature / СЂРµСЃСѓСЂСЃС‹ |
|---|---|---|
| `sprite_atlas_ecs` | atlas, ECS sprite and animation | `two-d` |
| `offline_sprite_atlas` | typed bounded offline atlas import and runtime binding (headless) | `two-d` |
| `two_d_tile_playground` | tilemap + `PlayableLoop2d` (WASD, camera follow, walls) | `two-d` |
| `two_d_platformer_playable` | HL Rapier platformer (`PlatformerPlayable2d`, A/D + Space) | `--features "app,two-d,character-2d"` |
| `two_d_playable_hud` | Deep 2D C: `PlayableLoop2d` + Esc pause overlay | `--features "app,two-d,ui"` |
| `two_d_animator_playable` | Deep 2D D: AnimationSet + SM + `play` + velocity/facing (Space=attack) | `--features "app,two-d"` |
| `two_d_tiled_playable` | M7: Tiled farm from `for_tests/2d` + 4-dir walk | `--features "app,two-d"` + local `for_tests/2d` |

## 3D Рё glTF

| Example | Р§С‚Рѕ РїРѕРєР°Р·С‹РІР°РµС‚ | Feature / СЂРµСЃСѓСЂСЃС‹ |
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
| `playable_dynamics_overlay_smoke` | M5.2 `DynamicsOverlay3d`: solid trimesh + two-way reaction | `--features "three-d,physics-rapier"` |
| `equirect_hdr_smoke` | CPU Radiance/linear equirect ingest + hemisphere sample check | none (synthetic) |
| `ggx_ibl_cook_smoke` | CPU GGX cook equirect в†’ cube mips + BRDF LUT + `Game3dScene` probe/sky queue | none (synthetic; needs `for_tests/`) |
| `asset_cook_cache_smoke` | M3 disk cook cache: second glTF import is a hit (parse skipped) | none (synthetic) |
| `physics_rapier_smoke` | M4.1 Rapier facade: dynamic sphere settles on fixed cuboid | `--features physics-rapier` |
| `physics_rapier_kinematic_smoke` | M4.3 kinematic platform + trigger overlap + CCD | `--features physics-rapier` |
| `physics_rapier_joints_smoke` | M4.4 fixed/revolute joints + collision groups | `--features physics-rapier` |
| `physics_rapier_convex_smoke` | M4.5 convex hull + limited prismatic joint | `--features physics-rapier` |
| `physics_rapier_contacts_smoke` | M4.6 contact pairs + rope + `DynamicsFixedStepper3d` | `--features physics-rapier` |
| `playable_dynamics_overlay_smoke` | M5.2 `DynamicsOverlay3d`: solid trimesh + two-way reaction | `--features "three-d,physics-rapier"` |
| `physics_rapier_window` | M4.2вЂ“M4.6 Rapier window: full dynamics lab | `--features "app,three-d,physics-rapier"` |
| `physics_3d_showcase` | M4 3D high-level tour: mesh character slope+platform + Rapier facade | `--features "three-d,physics-rapier"` |
| `physics_3d_lowlevel` | M4 3D low-level escapes: one-way `step_with_collision`, mesh queries, groups | `--features "three-d,physics-rapier"` |
| `physics_rapier2d_smoke` | M4.13 Rapier 2D facade: platformer settle + top-down kinematic/trigger | `--features physics-rapier2d` |
| `physics_platformer2d_smoke` | M4 platformer controller: land/jump/one-way/wall via Rapier KCC | `--features character-2d` |
| `game3d_profile_smoke` | M5.1 `Game3dProfile`: shared pool + glTF load to Ready | `--features profile-3d` |
| `animated_character_smoke` | M5.2 `AnimatedCharacterLoad3d`: skeletal import + walk advance | `--features profile-3d` |
| `game2d_profile_smoke` | M5.1 `Game2dProfile`: World + Game2dScene shell | `--features profile-2d` |
| `playable_loop_2d_smoke` | Deep 2D A `PlayableLoop2d`: kinematic step + camera follow | `--features profile-2d` |
| `platformer_playable_2d_smoke` | Deep 2D B `PlatformerPlayable2d`: land + sprite/camera sync | `--features "two-d,character-2d"` |
| `playable_hud_2d_smoke` | Deep 2D C pause overlay tree + `with_active_flag` | `--features "app,ui"` |
| `sprite_animator_2d_smoke` | Deep 2D D `SpriteAnimator2d`: play/walk + facing + on_finished | `--features two-d` |
| `tiled_map_2d_smoke` | M7 Tiled JSON → bind `TileMap2d`/`TileCollision2d` | `--features two-d` |
| `two_d_platformer_playable` | Deep 2D B windowed HL platformer (A/D + Space) | `--features "app,two-d,character-2d"` |
| `skybox_smoke` | fullscreen cubemap skybox from cooked outdoor mip0 (+Y vs в€’Y luma) | none (synthetic) |
| `directional_shadow_smoke` | orthographic directional shadow map darkens occluded ground (factor PBR) | none |
| `pbr_alpha_mask_smoke` | PBR `MASK` discards occluder shading + shadow cutout vs opaque | none |
| `specular_ibl_smoke` | factor-only prefiltered specular IBL probes (headless PNG) | none |

РўРѕС‡РЅС‹Рµ asset paths РЅР°С…РѕРґСЏС‚СЃСЏ РІ РЅР°С‡Р°Р»Рµ РєР°Р¶РґРѕРіРѕ source file. Р•СЃР»Рё fixture
РѕС‚СЃСѓС‚СЃС‚РІСѓРµС‚, example РґРѕР»Р¶РµРЅ Р·Р°РІРµСЂС€РёС‚СЊСЃСЏ СЏРІРЅРѕР№ import/I/O error, Р° РЅРµ РјРѕР»С‡Р°
РїРѕРґРјРµРЅРёС‚СЊ РєРѕРЅС‚РµРЅС‚.

## РћС‚ Р·Р°РґР°С‡Рё Рє РѕР±СЉСЏСЃРЅРµРЅРёСЋ

| РџРѕСЃР»Рµ example РїСЂРѕС‡РёС‚Р°Р№С‚Рµ | Guide |
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
  when РїРµСЂРµРЅРѕСЃРёС‚Рµ example РІ РјРёРЅРёРјР°Р»СЊРЅС‹Р№ downstream crate.
- РљРѕРјР°РЅРґС‹ Р·РґРµСЃСЊ Р·Р°РїСѓСЃРєР°СЋС‚ interactive applications. Р”Р»СЏ compile-only РїСЂРѕРІРµСЂРєРё
  РёСЃРїРѕР»СЊР·СѓР№С‚Рµ РєРѕРЅРєСЂРµС‚РЅС‹Р№ `--example` СЃ РїРѕРґС…РѕРґСЏС‰РµР№ Cargo check command.
