# Ограничения и совместимость

> **Статус:** Current compatibility contract  
> **Verified desktop platform:** Windows

Каждый лимит имеет одну из четырёх категорий: **hard limit**, **default**, **recommendation** или **backend-dependent**. Документация не должна выдавать backend/GPU-dependent значение за universal maximum.

Каждая page с лимитами указывает значение, конфигурационный API, поведение при превышении, корректный workaround и версию, в которой semantics появились или изменились.

## Compatibility matrix

| Возможность | Windows | Статус |
|---|---:|---|
| Runtime foundation | Да | Experimental |
| Bounded CPU/background task pool | Да | Experimental |
| Native window / WGPU surface | Да | Experimental |
| High-level application loop | Да | Experimental |
| In-memory typed asset handles | Да | Experimental |
| Typed importer plugin registry | Да | Experimental |
| WAV/MP3/Vorbis/FLAC default-device playback | Да | Experimental |
| Bounded versioned async TCP + typed JSON | Да | Experimental |
| PNG/JPEG/WebP decode with budgets | Да | Experimental |
| 2D metadata, sheets и animation | Да | Experimental |
| Instanced GPU sprite renderer | Да | Experimental |
| Gameplay actions/interactions/triggers | Да | Experimental |
| Lightweight 2D/3D physics foundation | Да | Experimental |
| Unlit 3D mesh renderer | Да | Experimental |
| Shader program configuration | Да | Experimental |
| Static glTF/GLB import | Да | Experimental |
| Textured unlit 3D mesh path | Да | Experimental |
| Lambert directional lighting | Да | Experimental |
| Safe model texture URI resolver | Да | Experimental |
| glTF scene/local-TRS/affine-matrix metadata import | Да | Experimental |
| 3D sphere raycast/overlap queries | Да | Experimental |
| glTF scene-to-ECS adapter | Да | Experimental |
| Semantic `game.use` sphere-raycast adapter | Да | Experimental |
| Semantic 2D pointer/touch AABB adapter | Да | Experimental |
| Winit keyboard semantic-action adapter | Да | Experimental |
| Fixed-step prototype 3D character motor | Да | Experimental |
| Event-driven quest counters and snapshots | Да | Experimental |
| Source 1 VMF reader and convex brush compiler | Да | Experimental |
| Source 1 VMT metadata, VTF 7.2 RGBA/BGRA decode and local base-texture resolve | Да | Experimental |
| Retained native UI, text shaping/raster data | Да | Experimental |
| Windows WebView2 local typed-bridge overlay | Да | Experimental |
| Direct-light factor-only PBR 3D renderer | Да | Experimental |
| glTF metallic/roughness textures and tangent-space normal maps | Да | Experimental |
| PBR image-based lighting (SH diffuse + GGX specular / skybox) | Да | Experimental |
| Directional shadow maps (single-cascade playable) | Да | Experimental |
| HDR bloom / FXAA / SSAO / parametric color grade | Да | Experimental |
| PBR alpha-mask cutouts | Да | Experimental |
| glTF cook cache (imported-asset + external fingerprints) | Да | Experimental |
| Source 2 maps | Исследование | Research |

## Гарантии

Windows-first означает, что другие ОС не являются implicit supported targets. Platform-neutral API shape не равен verified platform support.

## Текущие renderer limits

`RenderStatus` должен быть обработан host'ом: minimized window, timeout и
occlusion не являются fatal errors. При `SurfaceLost` renderer автоматически
пересоздаёт surface и возвращает `SurfaceRecreated`; `SurfaceLost` наружу
возвращается только если это восстановление не удалось. Device-loss recovery и
автоматический rebuild GPU resources пока не реализованы. Renderer владеет
acquire/submit/present; custom passes работают в scoped `RenderFrame` и не
должны создавать второй surface lifecycle. Текущее состояние доступно через
`Renderer::state`.

`RenderFrame::with_viewport` ограничивает draw физическим sub-rectangle через
одинаковые WGPU viewport/scissor и передаёт его размеры 2D/3D camera path через
`RenderFrame::draw_size`. Rectangle обязан быть ненулевым и полностью лежать
внутри presentation surface; Editor/docked host отвечает за DPI conversion и
input coordinates до входа в scoped frame.

## Task pool limits

`TaskPool` имеет fixed worker count и bounded submission queue. `shutdown` и
`Drop` закрывают submission, дренируют принятые jobs и блокируются до их
завершения; forced cancellation/preemption нет, поэтому never-returning job
может навсегда остановить shutdown. Это CPU pool, не async runtime, timer или
network executor. См. [Tasks guide](../guides/tasks.md).

## Текущие image limits

Image decoder принимает только разрешённые `PNG`, `JPEG` и `WebP` форматы и
применяет `DecodePolicy` до выдачи decoded RGBA data. Пользователь должен
выбирать budgets для своего trust boundary: asset из bundled game и file,
выбранный пользователем или полученный из network, имеют разные threat models.
Превышение budget — normal structured error, а не повод делать unbounded
allocation.

## Текущие audio limits

`AudioLoadLimits` жёстко ограничивает retained encoded bytes, а
`AudioEngine` явно и fallibly владеет default output device. Этот byte budget
не ограничивает decoded duration, codec CPU или внутреннюю память decoder;
для untrusted media host обязан добавить content/duration policy. Нет device
enumeration, capture, spatial audio, buses/effects или asset streaming cache.
См. [Audio guide](../guides/audio.md).

## Текущие 2D limits

`SpriteSheet::from_grid` работает только с полностью делящимся uniform grid.
Sprites с padding/irregular regions должны использовать explicit
`TextureRegion`. `SpriteRenderer2d` в текущем contract формирует batch для
одной texture; используйте atlas для снижения draw calls. `TileMap2d` имеет
viewport/chunk CPU extraction, shared animation timeline, bounded collision
snapshots and a static-AABB kinematic adapter, но не GPU
residency/streaming, navmesh, broadphase или multi-texture batching.

## Gameplay limits

`gameplay` поставляет semantic metadata/events, quests и interaction queries,
но не общую input-device поддержку, save serialization/migrations или
networking. `InteractionRequested` следует валидировать authority system; не
трактуйте request как совершившийся domain event. `QuestSignal` следует
выпускать только из confirmed domain outcome. 2D pointer/touch adapter scans
all AABB colliders, selects one top layer/entity-ID hit (including boundary),
and does not perform projection, broad-phase, UI routing or click-through;
full policy is in [2D pointer interaction](../guides/interaction-2d.md).

## Networking limits

`yuyib-net` проверяет big-endian frame length до allocation, различает clean
EOF и truncated frame и не создаёт hidden queues/runtime: TCP backpressure
проходит через awaited write. Phase 1 не предоставляет UDP/reliability, ECS
replication, authentication, TLS, discovery, reconnect или timeout policy;
caller владеет Tokio runtime и оборачивает operations в собственные deadlines.
См. [Networking guide](../guides/networking.md).

## Current input, motor and quest limits

`yuyib-input` принимает только Winit physical keyboard events. On focus loss
adapter releases mapped held keys and returns cancellation at the next selected
game frame; it has no mouse/gamepad/text/IME/touch/rebinding persistence.

`yuyib-character-3d` is a fixed-step kinematic prototype, not a rigid-body
controller. `CharacterMotor3d` resolves only against an infinite ground plane;
`CharacterController3d` also resolves its sphere against a static triangle
mesh or a caller-provided collision hook, with a configurable max walkable
slope (`max_walkable_slope_radians`) and optional kinematic platform carry
(`step_on_triangle_mesh_with_platform`). Dynamic bodies, step-offset climbing,
CCD, built-in broad phase, camera-relative controls and multiplayer authority
are intentionally outside the API.

`QuestBook` supplies in-memory deterministic definition/progress state and a
detached snapshot. Save encoding, schema migration, quest content loading,
UI, server authority and replication must be designed by the host game.

## Current Source 1 VMF limits

`yuyib-vmf` is a bounded parser for Source 1 text VMF only and preserves
unknown KeyValues blocks. `yuyib-source1::compile_map` bridges typed VMF brush
planes to one `Model`, but does not materialize map entities into ECS.
`yuyib-source1-scene` can materialize entity metadata and valid origins only;
it does not bind brush/prop models or infer gameplay/hierarchy/output rules.
`yuyib-vmf-model` compiles finite convex brushes with explicit work budgets
and material names only; it produces no UVs/lightmaps and does not support
BSP, props/displacements or Source 2 VMAP/VPK. Bounded VMT metadata, VTF 7.2
RGBA/BGRA decode and safe local base-texture resolution are separate
`yuyib-vmt`/`yuyib-vtf`/`yuyib-source1-assets` layers. See
[Source 1 / Hammer guide](../guides/source1-vmf.md).

`yuyib-vmt` itself only parses Source 1 material metadata. Its `$basetexture`
identifier can be passed to `yuyib-source1-assets` for safe local path
resolution and bounded VTF decode; VMT patch/include semantics and GPU binding
remain separate layers.

`yuyib-vtf` decodes a bounded VTF 7.2 RGBA8888/BGRA8888 subset to RGBA8, but
not VTF 7.0/7.1/7.3, compressed formats, cubemaps, frames, thumbnails or VPK
assets. A host chooses a local asset root and GPU upload explicitly;
`yuyib-source1-assets` supplies the safe base-texture path boundary, never a
VPK resolver.

## Physics limits

`physics` выполняет deterministic linear integration, circle/sphere overlap и
AABB point/ray/overlap queries. Отдельный static-only
`resolve_kinematic_aabb_2d` даёт bounded axis-sweep response для простых
tile/character prototypes; это не dynamic solver. Не используйте foundation
для arbitrary fast bodies, complex shapes или production network simulation
без собственного fixed-step, authority и подходящей anti-tunnelling policy.
Full details — в [Physics guide](../guides/physics.md) and
[tilemap kinematic collision](../guides/tilemap-kinematic-physics.md).

## Current 3D limits

The current 3D path caches GPU meshes, writes depth for opaque geometry and
supports solid/textured unlit, directional Lambert and direct-light PBR. The
PBR route has separate factor-only and partially/fully textured glTF
base/normal/MR/emissive-map pipelines; textured `BLEND` is sorted per primitive and rendered without depth
writes. Renderer-neutral ECS LOD selection and Source 1 brush import exist,
but there is no IBL, instancing, occlusion culling, shadow system or automatic
camera-prioritized residency. `Game3dScene` now performs CPU frustum
culling with cached per-model/per-mesh bounds; instancing, occlusion culling
and camera-prioritized residency remain absent. `LoadedGltfScene` provides bounded
Lambert/PBR texture and primitive publication with observable progress;
chunking inside one oversized primitive remains unsupported. Static
glTF/GLB is supported only by the
documented subset. Exact rules are in the
[3D GPU mesh guide](../guides/3d-renderer.md) and
[glTF importer guide](../guides/gltf-import.md).

## Current textured-material limits

`TextureCache` uploads one-mip RGBA8 sampled 2D images and replaces them only
on explicit `upsert`. `MeshPrimitive` and strict glTF import retain UV0–UV7;
`TexturedMeshRenderer3d` uses UV0, while textured PBR independently selects
the authored set for base/normal/MR/emissive. `ModelTextureLoader` separately
resolves approved local glTF URI metadata, decodes and uploads it. High-level
`Game3dScene` binds arbitrary non-empty PBR map subsets automatically; low-level
callers describe present channels with `PbrTexturePresence3d` and own the
fallback bindings explicitly. Low-level caller задаёт validated
`PbrAlphaMode3d::mask(cutoff)?`; передача `Blend` в opaque phase или `Mask` в
transparent phase возвращает typed `PbrMeshRenderError`. Factor-only blending
не реализован. Lambert and PBR streaming use worker-decoded textures and
per-frame texture/primitive byte and count budgets. See the
[textured material guide](../guides/textured-materials.md).

High-level PBR classifies exporter-authored, effectively opaque `BLEND`
textures using retained decoded-alpha statistics (minimum alpha plus 254/255
coverage). The default avoids non-depth-writing walls caused by exporter
noise; `PbrBlendPolicy3d::strict()` restores exact source semantics. Genuine
intersecting transparency still uses primitive-centre back-to-front sorting;
order-independent transparency is not implemented.

glTF `MASK` поддержан в factor-only и textured PBR: cutoff валидируется в
finite `0.0..=1.0`, discard использует итоговую base alpha, а surviving
fragments пишут depth. Это cutout coverage, не anti-aliased transparency;
alpha-to-coverage/MSAA и stochastic transparency пока отсутствуют.

## Current lighting and resolver limits

`LitMeshRenderer3d` supports Lambert diffuse with one extracted directional
light and explicit ambient term; it requires normals and has no shadow,
exposure/tone mapping, light clustering or PBR. `ModelTextureLoader` resolves
only local canonical filesystem paths below a declared root and performs
explicit decode/upload; it does not watch files or bind material groups. See
the [lighting/resolver guide](../guides/lit-materials.md).

## Current scene and physics-query limits

`ImportedScene` preserves hierarchy and local TRS or affine matrices;
`yuyib-scene` can spawn one selected all-TRS scene into ECS and propagate
derived world transforms. It does not yet synchronize imported cameras or all
light types, prioritize residency from camera visibility, or provide automatic
scene despawn. 3D physics includes deterministic sphere/AABB queries and a
static triangle-mesh path used by raycasts, spawn clearance and the fixed-step
character controller. There is still no broadphase, OBB, dynamic rigid-body
solver or engine-owned trigger policy.
See [scene/material guide](../guides/standard-material-and-scenes.md) and
[physics guide](../guides/physics.md).

## Current ECS scene and interaction limits

`spawn_scene` selects exactly one glTF scene and preserves local hierarchy
through `LocalTransform3d`/`Parent3d`; it lacks children cache, automatic
despawn and camera/light world-space synchronization. `game.use` interaction
uses Started-only semantic actions plus O(n) sphere raycast. See the
[Scene ECS guide](../guides/scene-ecs-and-interactions.md).
