# RFC 0003 — render graph, 2D/3D phases и shader API

- **Статус:** accepted
- **Дата:** 2026-07-31
- **Зависит от:** RFC 0001, RFC 0002

## Решение

Один `Renderer` владеет WGPU device, queue, surface configurations и render graph. Scene/ECS не записывает команды GPU напрямую в стандартном high-level path. Он описывает cameras, visibility, meshes/sprites, materials и renderable instances; renderer строит phases и кэширует pipelines. Low-level API сохраняет доступ к device, queue и custom graph pass.

Render graph выполняется в следующем порядке, если соответствующий feature включён:

```text
asset uploads -> compute -> 3D shadow -> 3D opaque -> 3D transparent
              -> 2D world -> post processing -> native UI -> presentation
```

Ни одна phase не обязана быть включена. Headless/server build не создаёт WGPU device. WebView является отдельной platform surface и не участвует в scene renderer напрямую; его OS-level composition contract определяется модулем WebView.

## 2D и 3D

`SpriteRenderer` и `MeshRenderer` — равноправные render components. Они используют общую visibility/budget/asset систему, но имеют собственные batching and camera projection правила. 2D camera допускает orthographic projection и sorting layers; 3D camera — perspective/orthographic projection, depth, lights и shadow settings. UI не является ни sprite, ни mesh: он получает поздний compositing phase.

## Material contract

Material — asset/handle с immutable shader/layout contract и изменяемым параметрическим состоянием. Обновление параметра не должно создавать новый pipeline. Pipeline variation определяется только normalised material features: например, `skinning`, `instancing`, `alpha_mode`, `normal_map`, `shadows`.

Standard material paths:

- `UnlitMaterial`: 2D sprites, UI-like world geometry, debug/preview;
- `SpriteMaterial`: texture/color, blend mode, sampler policy, optional pixel-art filtering;
- `PbrMaterial`: base color, metallic/roughness, normal, emissive, alpha and culling policy;
- `EffectMaterial`: preset effect с documented parameter ranges and GPU cost.

## Shader API tiers

### Tier 1 — effect presets

Пользователь выбирает effect и задаёт параметры. Первые кандидаты: outline, bloom/glow, dissolve, blur, pixelate, water, toon, 2D light и color grading. Каждая preset page должна описывать GPU cost, required passes, order и platform fallback.

### Tier 2 — custom material templates

Template фиксирует vertex layout, standard bindings и render state. Разработчик задаёт schema параметров и ограниченные hooks (vertex displacement, fragment color/alpha). Система генерирует/валидирует совместимый WGSL module and pipeline variant. Необходимые vertex attributes и texture formats проверяются при загрузке.

### Tier 3 — explicit renderer

Разработчик получает WGPU `Device`, `Queue`, texture/buffer handles и регистрирует pass в render graph c declared read/write dependencies. Этот путь применяется для compute particles, custom post-processing, GPU simulation и специальных rendering techniques. Custom pass не может незаявленно изменять global renderer state.

## Shader safety and diagnostics

- WGSL source валидируется до pipeline creation;
- material/shader mismatch даёт structured error c parameter/attribute name;
- pipeline cache key включает shader content hash, target formats, vertex layout and normalized variant flags;
- shader hot reload возможен только development feature и сохраняет prior valid pipeline, если новая версия не скомпилировалась;
- runtime не компилирует arbitrary shader text, полученный из network/WebView/untrusted asset;
- diagnostics предоставляют draw calls, visible instances, pipeline cache hits/misses, GPU upload bytes и per-pass timing там, где backend это поддерживает.

## Limits and caveats

- Параметры материала не должны оборачиваться в отдельный uniform buffer на entity: это разрушит batching. Per-instance parameters ограничиваются documented packed instance layout или storage-buffer path.
- Transparent 3D geometry требует сортировки и не имеет тех же batching guarantees, что opaque geometry.
- Encoded passes не могут переиспользовать и перезаписывать один uniform range
  до GPU submission. Opaque/transparent batch должен владеть immutable range
  или отдельным buffer, иначе camera-dependent sorting меняет уже записанные
  transform/material данные предыдущей phase.
- `normal_map` требует tangents. При `tangents = drop` importer переключает standard material на совместимый variant; custom material с hard requirement должен fail predictably.
- Occlusion culling и HLOD не входят в первый renderer slice, но visibility result нельзя спроектировать так, чтобы они потребовали изменения public `Renderable` API позже.
