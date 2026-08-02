# 3D: scenes, materials и shaders

**Статус:** Experimental unlit/textured/Lambert/textured-PBR + render graph + shader configuration  
**Платформы:** Windows

`yuyib-render-3d` рисует solid unlit, base-colour textured unlit и colour
Lambert indexed meshes через `RenderFrame`; `PbrMeshRenderer3d` добавляет
factor-only Cook-Torrance preset, `TexturedPbrMeshRenderer3d` — glTF
base/normal/metallic-roughness/emissive path, а `StandardRenderer3d` выбирает
поддерживаемый path по model material. `yuyib-game-3d` extracts deterministic
ECS scene/LOD/light snapshots. CPU-side mesh/material data и static glTF/GLB
import доступны через [`yuyib::model`](model-assets.md) и
[`yuyib::gltf`](gltf-import.md). Полный current path описан в
[GPU mesh guide](3d-renderer.md).

`yuyib-shader` уже даёт `ShaderPrototype::VertexColor` для prototypes и
`ShaderSource`/`ShaderProgram` для explicit WGSL configurations. Он намеренно
не делает CPU-side fake compilation или reflection; backend отвечает за
валидный pipeline и diagnostics. Custom material templates остаются planned.
`RenderGraph` уже регистрирует declared phases/resources/dependencies и CPU
timings. `Renderer::with_raw_gpu` даёт borrow-only доступ к
`wgpu::Device`, `wgpu::Queue` и surface configuration, но не регистрирует
pass в render graph.

## Limits & Caveats

Opaque material variants, depth testing, direct-light PBR, arbitrary core glTF
texture subsets, tangent-space normal mapping и alpha mask реализованы. Mask
discard пишет surviving depth; textured PBR `BLEND` имеет отдельную sorted
non-depth-writing phase. IBL, shadows, factor-only blend и instancing ещё не
реализованы. Lambert textured path имеет batching, остальные routes пока
открывают больше passes. Fixed renderers не потребляют custom `ShaderProgram`: для
произвольного WGSL разработчик пока строит собственный WGPU pipeline через
low-level scoped API. `with_raw_gpu` не передаёт texture view current
presentation frame; draw calls записываются только через `RenderFrame`.
