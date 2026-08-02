# Roadmap Yuyib до Engine MVP

Актуально на 2026-08-02. Этот файл измеряет готовность продукта. ADR рядом
описывают принятые решения, но наличие ADR или отдельного типа Rust не означает,
что пользовательский milestone завершён.

## Правило завершения этапа

Этап закрывается только когда одновременно существуют:

1. рабочий vertical use-case через high-level API;
2. документированный low-level escape hatch;
3. limits, diagnostics и failure behaviour;
4. scoped tests и runnable example;
5. wiki-guide и актуальная capability matrix;
6. согласованный performance/correctness gate там, где он применим.

Новый разрозненный API не считается продвижением этапа сам по себе.

## Текущее состояние

| Область | Состояние | Главный остаток |
|---|---|---|
| Windows application | partial | multi-window, device-loss recovery, production diagnostics |
| Game/ECS lifecycle | foundation + M5.1 profiles | Deep 2D HL started (`PlayableLoop2d`); save/load |
| Tasks/assets/importers | strong foundation | hot reload UI, eviction, shipping without importers |
| 2D | partial + Deep 2D A | PlayableLoop2d/camera follow; platformer HL, Tiled, UI slices |
| 3D/glTF/animation | strong partial | compatibility tail, material overrides, animation authoring |
| 3D rendering | usable MVP | validated CSM, TAA/GTAO, GPU instancing, timestamps |
| Shader API | not as planned | high-level effects/material templates |
| Physics | usable MVP (Rapier 3D/2D + platformer controller) | editor physics UI, tile broadphase cache |
| Navigation | early partial | agent navmesh, smoothing, dynamic obstacles |
| Gameplay | partial | persistence, UI composition, authority/replication |
| Native UI | early partial | ScrollView+thumb done; drag/IME/a11y/images open |
| WebView | partial | facade decision, focus/accessibility/composition |
| Audio | partial | spatial audio, buses, streaming and device policy |
| Networking | early partial | TLS/auth, replication, prediction and observability |
| Source 1 | early partial | BSP/VPK/lightmaps/displacements/props/material integration |
| Source 2 | missing/research | format/version/legal matrix and importer plugin |
| Editor/authoring | E1 in progress | Project wizard / cook export; LSP completion deferred to end-game |
| Documentation/release | partial | book freshness, golden tests, stable policy; Actions foundation CI landed |

## Критический путь

### E1 — Первый полезный Editor vertical slice

Editor — отдельный strategic development-tool track и consumer runtime, а не
новый владелец Engine lifecycle. Начало track не означает клон Unreal/Unity и
не закрывает недостающий runtime capability через UI.

Порядок реализации:

1. ~~Project/Asset Browser~~ — **partial closed:** open/create `project.yuyib`,
   scene list, asset browser shell; track `.yasset` + GUID-preserving rename for
   glTF. Открыто: GUID move across folders, file-watcher conflict, dependency
   graph;
2. production importer diagnostics и non-destructive reimport — **partial:**
   `yuyib-gltf-authoring` PreviewAdapter + settings → `ImportOptions`, Editor
   non-destructive reimport. Открыто: selection/overlays, full Asset DoD
   (`yuyib.gltf-import` / `yuyib.gltf-preview` = `Asset`);
3. ~~asset preview через общий importer/cooker/renderer path~~ — **partial:**
   production `GltfSceneLoad` path + host-registered adapter; overlays +
   mesh/material/**animation** selection. Открыто: full Asset DoD / material
   override;
4. ~~scene hierarchy, Inspector, commands и transform gizmo~~ — **closed:**
   hierarchy, Inspector, undo/redo, Move/Rotate/Scale gizmo, Model3d
   place/spawn;
5. ~~GUID/versioned scene save/load с opaque unknown preservation~~ — **closed**
   для foundation schemas (`.yscene` round-trip);
6. ~~process-isolated Play Mode~~ — **closed** для first playable slice:
   `yuyib-play` pins `--project` / `--scene` / `--scene-revision` /
   `--scene-file-revision` (blake3), Player motor + mesh collider, authored
   light, dark PBR fallback; `host.process` возвращает pin + exit code.
   Apply-Play reverse-sync отключён;
7. source/system navigation — **partial closed** (file tree + coverage systems /
   runtime/authoring jump); mature LSP completion/hover/rename —
   **deferred to end-game** (diagnostics-only already shipped).

Evidence (closed items): Transform gizmo; Player motor; mesh physics;
DirectionalLight transform/cone; dark Play fallback. Full E1 DoD ниже не
закрыт: Asset import/preview evidence, coverage CI, LSP.

Definition of Done: реальный 3D asset проходит bounded/cancellable import,
material/mesh/animation selection и geometry/material diagnostics; preview имеет
collision/normals/tangents/UV/bounds overlays и совпадает с Play renderer route;
asset можно разместить, изменить, сохранить и открыть без компиляции; runner
crash не роняет Editor; selected component ведёт к adapter-у, owning plugin и
читающим/пишущим systems. Capability coverage machine-readable и проверяется CI.

Normative contract: [RFC 0011](0011-editor-authoring-contract.md) и
[Editor engine integration](../editor/ENGINE_INTEGRATION.md).

### M1 — Correct playable 3D vertical slice

**Status: closed** (street-city playable/smoke path). Дальнейшая 3D-работа —
M2 и ниже по ROADMAP.

Definition of Done:

- importer публикует mesh/material/texture diagnostics;
- source geometry не скрывается эвристиками по node name/index;
- partial PBR materials и emissive strength сохраняются;
- missing source material получает явный fallback или override manifest;
- loading, grounded spawn, collision, animation и camera проходят smoke scenario;
- фиксированный camera pose имеет reference screenshot;
- asset-specific glue вынесен из example в reusable policy/cooker boundary.

Evidence:

| DoD item | Evidence |
|---|---|
| importer diagnostics | glTF codes + `LoadedGltfScene::diagnostics`; `gltf_texture_diagnostics` |
| no hide-by-name heuristics | semantic collision selectors; materials via `ModelMaterialPolicy` |
| partial PBR / emissive | PBR channel-wise UV fallback; night/street policies |
| missing material fallback | `ModelMaterialPolicy` + unbound/factor-only diagnostics |
| smoke: load/spawn/collision/animation/camera | `street_city_m1_smoke` |
| reference screenshot | headless PNG + soft `Rgba8ReferenceMetrics` in `street_city_m1_smoke`; `frame_capture_smoke` |
| asset glue boundary | `examples/support/street_city.rs`, `playable_character.rs`; `ModelMaterialPolicy` |
| spawn selection diagnostics | `CharacterSpawnReport3d` / `spawn_on_surface_mesh_with_report` |

Known material gap (deferred, не блокирует закрытие M1): `sci-fi_girl` использует
`KHR_materials_pbrSpecularGlossiness`; корректный read — через SG→MR conversion
или M2 IBL, не через metallic patches на skeletal diffuse path.

### M2 — Standard rendering baseline

**Status: usable MVP** по пунктам ниже. Полный DoD (visual regression, 1080p
budget) ещё открыт.

Порядок реализации:

1. ~~mipmaps, sampler policy, anisotropic filtering и color-space correctness~~ —
   **done:** `TextureSampler` / `TextureMipmapPolicy`, CPU mip generation
   (sRGB→linear), anisotropy fallback, model-to-PBR sampler binding
   (`yuyib-render-texture`, `yuyib-model-assets`). Остаток: array-texture mips,
   sampler dedupe, visual goldens;
2. environment lighting / IBL и BRDF LUT — **usable MVP:** L2 SH diffuse;
   factor-only prefiltered specular cube/LUT (`specular_ibl_smoke`);
   CPU Radiance HDR / equirect ingest (`PreparedEquirectEnvironment3d`,
   `equirect_hdr_smoke`); CPU GGX cook equirect → cube mips + BRDF LUT
   (`cook_ggx_specular_ibl`, `ggx_ibl_cook_smoke`); street-city / playable
   outdoor probe + skybox (`Game3dScene::with_environment`, `skybox_smoke`);
   `for_tests/outdoor_probe.hdr` via `street_city::load_outdoor_equirect`.
   Открыто: authored high-res probe capture;
3. directional shadow maps — **usable MVP** (street = 1 cascade): depth-only
   pass, ortho map, factor/textured PBR 3×3 PCF, texel-stable focus, MASK
   cutouts (`directional_shadow_smoke`, `pbr_alpha_mask_smoke`). 2-cascade API
   есть, для playable parked после nested-ortho regressions. Открыто:
   validated CSM, skinned casters;
4. alpha mask и transparency policy — **usable MVP:** PBR MASK (factor +
   textured) + shadow cutout (`pbr_alpha_mask_smoke`). Открыто: factor-only
   Blend, unlit MASK, A2C;
5. bloom для emissive — **usable MVP:** HDR bright-pass + 4-level
   downsample/upsample before ACES (`BloomConfig`,
   `ColorPostProcess::with_bloom`; street / `yuyib-play` —
   `BloomConfig::street_city()`). Открыто: dirty-lens, dirt mask, adaptive
   threshold, Offscreen HDR smoke;
6. FXAA/TAA — **usable MVP (FXAA):** LDR FXAA after ACES (`FxaaConfig` /
   `ColorPostProcess::with_fxaa`; playable / `yuyib-play` —
   `FxaaConfig::street_city()`). Открыто: TAA, SMAA;
7. SSAO/GTAO и color grading LUT — **usable MVP (SSAO + parametric grade):**
   half-res depth-only SSAO (`Game3dScene::with_ssao`); `ColorGradeConfig`
   after ACES (`ColorPostProcess::with_color_grade`). Открыто: GTAO, bilateral
   blur, cooked 3D LUT;
8. instancing, GPU timings и allocation/draw diagnostics — **usable MVP
   (diagnostics + textured PBR UBO ring):** `SceneDrawStats` /
   `Game3dSceneStats::summary_line`; 8-slot UBO ring for textured PBR;
   `YUYIB_RENDER_GRAPH_TIMINGS`. Открыто: true GPU mesh instancing, timestamp
   queries, broader UBO rings.

Definition of Done: outdoor, indoor PBR и transparent/emissive reference scenes,
visual regression, resize/resource lifecycle tests и согласованный 1080p budget
на reference Windows GPU.

### M3 — Cooked asset pipeline

**Status: M3.1 + M3.2 usable MVP.** Next critical path: **M4** physics facade.
Static mesh + kinematic character остаются для corridor playable до M4.

Evidence:

- `AssetCooker` / `CookKey` / `CookManifest` / `CookCache` (`yuyib-assets`);
- glTF imported-asset cooker (`import_scene_bytes_cached`,
  `import_scene_bytes_cached_at`, schema-prefixed bincode);
- source-only cache key (skip-parse); external buffer/image fingerprints
  invalidate dependents (`dependency_fingerprints_match`);
- `GltfSceneLoadConfig::with_cook_cache`; street-city / playable —
  `for_tests/.yuyib_cook`;
- `asset_cook_cache_smoke`, `yuyib-gltf` cook tests (incl. external buffer miss).

**Deferred to v2.0 / post-core (не блокирует M4–M5):**

- full reverse dependency graph + editor hot-reload UI / file watchers;
- shipping build without source importers (cooked-only feature);
- persist `ImportReport` in cook blob (`gltf::mesh::Mode` serde);
- cook-time oversized primitive split + camera/zone residency budgets;
- validated CSM, TAA/SMAA, GTAO, cooked 3D LUT, true GPU mesh instancing,
  GPU timestamp queries, broader UBO rings;
- Editor post-process knob persistence / HUD diagnostics polish.

Definition of Done (core): неизменённый GLB повторно не парсится (**done**);
изменение external buffer/image invalidates только зависимый cook entry
(**done**); shipping-without-importer + mesh-split + hot-reload UI → v2.0.

### M4 — Physics facade над mature backend

**M4.1–M4.13 usable MVP slice:** Rapier 3D facade + playable side-by-side overlay
(`DynamicsOverlay3d` in `yuyib-profile-3d`, feature `rapier` / `physics-rapier`):
local solid **map trimesh**, props, kinematic character sphere, soft two-way
reaction, **configurable walkable slope**, and **kinematic moving platforms** on
the mesh character path
(`CharacterController3d::step_on_triangle_mesh_with_platform` + carry). Character
locomotion still uses `TriangleMesh3d` queries — no motor rewrite onto Rapier.
**M4.13:** Rapier 2D facade (`RapierDynamicsWorld2d`, feature `physics-rapier2d`)
mirrors early 3D surface — fixed/dynamic box/ball/capsule, kinematic platforms,
triggers, collision groups, CCD, contacts, joints, `DynamicsFixedStepper2d`,
platformer + top-down configs; smoke `physics_rapier2d_smoke`.

Собственные static queries/BVH сохраняются. General-purpose rigid-body solver
не следует разрабатывать внутри Yuyib: dynamic bodies, broadphase, CCD, sleeping,
joints и determinism должны прийти через заменяемый mature backend adapter
(**Rapier #1**; Avian #2 later).

**Still open (M4 DoD remainder / v2.0):** editor physics UI; replacing 3D character
mesh queries with backend mesh queries.

**3D examples (done):** `physics_3d_showcase` (high-level mesh character + Rapier
facade tour) and `physics_3d_lowlevel` (custom `step_with_collision`, direct mesh
queries, collision groups / contacts).

**2D examples (done for facade + platformer MVP):** `physics_rapier2d_smoke`
(platformer settle + top-down kinematic/trigger) and `physics_platformer2d_smoke`
(`PlatformerController2d`: gravity/jump/coyote/buffer, walls, one-way platforms via
Rapier KCC — separate from top-down `KinematicSpriteController2d`).

**v2.0 polish (do not block):** editor physics UI, replacing character mesh
queries with backend mesh queries.

Definition of Done: static/kinematic/dynamic bodies, box/sphere/capsule/convex,
triggers/contacts, filters, CCD, joints, moving platforms и slope-aware character
controller; simulation изменяется только в `FixedUpdate`; существуют 2D
top-down, 2D platformer и 3D physics examples.

### M5 — Ergonomic high-level profiles

**M5.1 usable MVP slice:** composition profiles that collapse repeated owner
wiring without hiding budgets/errors:

- `Game3dProfile` (`yuyib-profile-3d`, feature `profile-3d`) — shared `TaskPool`,
  `Game3dScene`, one `GltfSceneLoad`/`LoadedGltfScene` lifecycle, prepare/render
  helpers; smoke `game3d_profile_smoke`.
- `CharacterGame3d` — opt-in mesh character spawn/step on profile layers or raw
  meshes (not folded into the base profile).
- `Game2dProfile` (`yuyib-profile-2d`, feature `profile-2d`) — `World` +
  `Game2dScene`, animation step + render helpers; smoke `game2d_profile_smoke`.

**Deferred / explicit decisions:**

- `NativeApplicationProfile` — low ROI while `Application` already is the native
  builder; revisit when ≥2 repeated app compositions exist.
- Standalone `Web` facade / `yuyib-web` — **deferred**. Keep
  `Application::webview(...)` behind `webview` until a first-class `WebSurface`
  lifecycle (non-overlay placement, focus/accessibility) exists. A thin alias
  now would fossilize an underspecified API (RFC 0001 gap).

Definition of Done (remaining): migrate more playables onto profiles where
policy is stable (`cyberpunk_city_playable` uses `Game3dProfile` +
`AnimatedCharacter3d` + `PlayableLoop3d` + `DynamicsOverlay3d`). **Current:**
Deep 2D A (`PlayableLoop2d` / `CameraFollow2d`). Audio/interaction remain
opt-in adapters.

## Ближайший порядок работ

1. **Current (узкий M6)** — ScrollView glyph clip + vertical thumb (**done**).
   Drag/inertia, IME, a11y, images — позже.
2. **High-Level composition (M5.2)** — engine-owned playable loop, не helpers в
   `examples/support`:
   - `AnimatedCharacter3d` / `AnimatedCharacterLoad3d` (**done**, smoke
     `animated_character_smoke`);
   - `EnvironmentPreset` / `Game3dProfile::with_environment` (**done**, street-city
     IBL/shadows/SSAO);
   - `Game3dPlayableLoad` co-load map+character (**done**);
   - `PlayableLoop3d` (controls+camera+step+draw) (**done**, playable migrated);
   - `DynamicsOverlay3d` (**done**, feature `rapier` / `physics-rapier`; smoke
     `playable_dynamics_overlay_smoke`).
   **DoD:** `cyberpunk_city_playable` ≈ 150–250 строк без glue-support для
   avatar/IBL/input/step/dynamics (остаётся window Application glue + street knobs).
3. **Deep 2D** — HL-паттерны как M5.2 3D:
   - A. `PlayableLoop2d` + `CameraFollow2d` (**done**, smoke
     `playable_loop_2d_smoke`; `two_d_tile_playground` migrated);
   - дальше: platformer HL / sprite↔body sync; thin UI (pause/HUD); Tiled/LDtk
     (M7) → `TileMap2d`.
   Rapier2D facade / `PlatformerController2d` / `Game2dProfile` smokes уже есть.

### M6 — Application capability completion

Native UI: scroll, nested clipping, images/icons, common widgets, text input,
Windows IME, DPI и accessibility. Input: keyboard/mouse/gamepad profiles и
persistent rebinding. Audio: listener/source, buses и master mixer. WebView:
focus/input/accessibility lifecycle и окончательное facade decision.

**Started:** bounded vertical `ScrollView` — retained wheel offset, viewport
pointer clipping, WGPU scissor для rectangles **и** `ApplicationUi::with_text`
glyph path (`UiLayout::clip` → `TextClipRect`), plus vertical thumb indicator
на input-aware extract (`UiVisualStyle::scroll_thumb`, no drag/inertia). Nested
scroll clips intersect на layout path. Открыто: scrollbar drag/inertia,
virtualization, IME, accessibility, images/icons.

### M7 — Content formats

Только после M3:

1. Tiled JSON/TMX или LDtk;
2. Source 1 VMF material/UV/entity completion;
3. BSP/VPK/lightmaps/displacements/props;
4. TrenchBroom MAP;
5. Source 2 research RFC и compatibility matrix;
6. Source 2 importer plugin.

Importer готов только вместе с supported-version matrix, coordinate conversion,
limits, fixtures, malformed corpus и cooked output.

### M8 — Client/server capability

Не реплицировать ECS world целиком. Нужны stable protocol/entity IDs,
authentication/TLS, admission control, timeouts/reconnect, snapshots/deltas,
authority validation, interpolation/prediction/reconciliation, replication
allow-list, metrics и replay tests.

### M9 — Release gate

Windows CI, reproducible wiki build, public API/limits consistency, examples для
каждой major capability, GPU/WebView smoke checklist, benchmarks, visual golden
tests, importer/network fuzzing, API stability/migration policy, changelog и хотя
бы один minor cycle без breaking high-level API.

## Граница Engine MVP

MVP достигнут, когда без internal workaround можно сделать:

- native desktop application с полноценным базовым UI;
- 2D игру с импортированной картой, animation, physics, audio и interactions;
- 3D игру со streamed glTF, PBR/IBL/shadows, animated character, dynamic physics,
  interactions и save state;
- optional WebView UI и headless server/simulation;
- custom importer по публичному guide и shipping cooked build;
- high-level examples плюс отдельные low-level escape-hatch examples.

Source 2, matchmaking и AAA world streaming в Engine MVP не входят. Editor
остаётся отдельным development-tool track: его vertical slice приоритетен для
проверки capabilities, но сам по себе не закрывает недостающие Engine MVP
runtime requirements.

## Не строить сейчас

- собственный ECS или general-purpose rigid-body solver;
- dynamic Rust plugin ABI;
- visual scripting, node shader editor и попытку сразу повторить весь
  Unreal/Unity поверх незавершённого first vertical slice;
- собственный text/code editor вместо зрелого component + rust-analyzer/LSP;
- Editor-only glTF/image decoder или renderer, расходящийся с Play Mode;
- Source 2 до cooked asset contract;
- HLOD/virtual texturing/world partition до обычной residency и instancing;
- full multiplayer replication до authority/security protocol;
- cross-platform до стабильного Windows MVP;
- WebView-as-GPU-texture;
- pixel-perfect копию Sketchfab через неизвестные light presets вместо
  корректного material/render pipeline.
