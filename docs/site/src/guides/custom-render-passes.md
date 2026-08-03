# Low-level renderer / custom passes

> **Статус:** Experimental  
> **Модули:** `yuyib::render`, `yuyib::shader`  
> **Уровень:** escape hatch

## Когда спускаться сюда

High-level `Game2dScene` / `Game3dScene` / `Application` clear pass **не**
закрывают задачу: нужен свой fullscreen effect, debug overlay, или порядок
pass’ов вне стандартной фазы.

Если задача — «показать glTF» или «нарисовать sprite», **не начинайте отсюда**.
Сначала: [tutorial окно](../tutorials/first-window.md),
[Game3dScene](game-3d-scene.md), [Game2dScene](game-2d-scene.md).

## Модель

```text
Renderer (device, surface, frame)
    ↓
RenderGraph: ordered passes (declare deps / phases)
    ↓
your encode on RenderFrame / pass context
    ↓
present
```

| API | Роль |
|---|---|
| `Renderer` | WGPU device/surface ownership у host |
| `RenderFrame` | Scoped access на один redraw |
| `RenderGraph` / `GraphPassDescriptor` | Explicit pass order, не скрытый global |
| `ShaderProgram` | WGSL program registration |
| `OffscreenRenderer` | Headless / smoke capture path |

## Почему explicit graph

Неявный «добавь effect в конец» ломает ordering с UI, shadows и post-process.
Graph делает зависимости видимыми и тестируемыми (`render_graph_phases`
example).

## Пример направления

```rust,ignore
// Pseudocode shape — см. rustdoc и example render_graph_phases
let mut graph = RenderGraph::new();
graph.add_pass(/* descriptor: name, phase, execute closure */)?;
// Application/Game on_render: graph.execute(&mut frame)?;
```

Канонический example:

```powershell
cargo run -p yuyib --example render_graph_phases
```

## Limits & Caveats

- Нет node material editor / visual shader graph.
- EffectMaterial presets (outline, 2D tint templates) — ещё не HL product API.
- Device-loss: user GPU resources rebuild — host responsibility.
- Не выполняйте file I/O / decode внутри pass encode.

## См. также

- [HDR post-processing](hdr-post-processing.md) — готовые post knobs
- [3D scenes & shaders map](3d-and-shaders.md)
- [Application on_render](application.md)
