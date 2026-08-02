# Renderer и declared render graph

> **Статус:** Experimental renderer boundary and render graph  
> **Crate / module:** `yuyib::render`  
> **Requires:** Cargo feature `render`

`Renderer::with_raw_gpu` — доступный low-level путь: closure получает
`&wgpu::Device`, `&wgpu::Queue` и `&wgpu::SurfaceConfiguration`. Это позволяет
создавать pipelines, buffers, textures и отправлять собственные command buffers
без передачи ownership renderer-а.

`RenderFrame` владеет одним surface acquisition/presentation lifecycle. Он
позволяет extension layers записывать work между acquire и present, не создавая
второй `wgpu::SurfaceTexture`.

## Declared graph

`RenderGraph` регистрирует passes с standard `RenderPhase`, stable ID,
dependencies и declared read/write resources. `Application::render_graph`
выполняет его после foundation clear, затем low-level `on_render`, затем native
UI. Pass получает только frame-local `RenderFrame`, поэтому не может вызвать
present или изменить surface configuration.

```rust,no_run
use yuyib::prelude::*;

let surface = RenderResourceId::surface_color();
let mut graph = RenderGraph::new();
let world = graph.add_infallible_pass(
    GraphPassDescriptor::new("game.world", RenderPhase::Opaque3d)
        .writes(surface.clone()),
    |_frame| {},
)?;
graph.add_infallible_pass(
    GraphPassDescriptor::new("game.post", RenderPhase::PostProcess)
        .after(world)
        .reads(surface.clone())
        .writes(surface),
    |_frame| {},
)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

Standard order: asset upload, compute, 3D shadow, opaque, transparent, 2D
world, post-process, native UI. Неиспользуемые phases не создают work.

`RenderGraphExecution` возвращает CPU recording duration каждого pass. Это не
GPU timestamp: GPU timings требуют backend query sets и отдельного capability
check. Runnable example: `render_graph_phases`.

## Limits & Caveats

`with_raw_gpu` работает синхронно во время borrow renderer. Не храните ссылки
на переданные WGPU values после closure. Dependency должен ссылаться на уже
зарегистрированный pass и не может идти из ранней phase в более позднюю.

Resource declarations пока используются для validation/diagnostics; transient
texture allocation и automatic barrier planning ещё не реализованы. Surface
texture acquisition и presentation остаются под контролем renderer/render
frame.
