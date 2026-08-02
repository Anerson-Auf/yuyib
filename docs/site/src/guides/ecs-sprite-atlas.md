# 2D: ECS atlas example

> **Статус:** Experimental  
> **Полный исходник:**
> [`sprite_atlas_ecs.rs`](../../../../crates/yuyib/examples/sprite_atlas_ecs.rs)  
> **Платформа:** Windows + compatible WGPU adapter

Это runnable vertical slice текущего 2D path: PNG decoder → typed asset handle
→ GPU upload → ECS component → extraction → instanced draw. Атлас встроен в
исходный код, поэтому для запуска не нужны каталог `assets/`, watcher или
внешний editor.

```powershell
cargo run -p yuyib --example sprite_atlas_ecs
```

Откроется окно с четырьмя вращающимися спрайтами. Каждый берёт отдельную
область 8×16 из одной текстуры 32×16. Закройте окно стандартной кнопкой
Windows, чтобы завершить программу.

## Что демонстрирует пример

1. `decode_bytes` проверяет PNG against `DecodePolicy` и normalizes pixels в
   row-major RGBA8.
2. `Assets<Texture>` выдаёт typed `TextureHandle`; `TextureRegion` проверяет
   границы каждого atlas cell до рендера.
3. `Sprite2d` — ECS component, а не GPU object. Update callback меняет его
   transform без обращения к WGPU.
4. `extract_sprites` сортирует по `layer`, затем детерминированно по entity ID,
   и формирует batches только из adjacent sprites с одной texture.
5. Первый `on_render` lazily создаёт `SpriteRenderer` и один раз загружает
   texture через `RenderFrame`; дальше только готовит instances и рисует их.

## Почему atlas один

`SpriteRenderer` MVP принимает одну GPU texture на batch. Один atlas означает
один texture group и один instanced draw call для всех четырёх sprites. Не
сливайте batches с разными textures вручную: это нарушит painter order для
transparent sprites. Когда source content состоит из отдельных PNG, сначала
создавайте independent batches; runtime atlas packing пока не реализован.

## API, использованный в примере

| API | Роль |
| --- | --- |
| `decode_bytes`, `DecodePolicy` | bounded decoding embedded/remote bytes |
| `Assets<Texture>`, `TextureHandle` | typed ownership of texture metadata |
| `TextureRegion` | validated UV source rectangle |
| `Sprite2d`, `extract_sprites` | ECS-facing sprite state and ordered full-world extraction |
| `SpriteViewport2d`, `extract_visible_sprites_2d` | validated bounded CPU culling for ordinary sprites; see [viewport guide](sprite-viewport-culling.md) |
| `RenderFrame` | scoped access to active device/queue and presentation format |
| `SpriteRenderer::new_for_frame` | one-time 2D pipeline creation inside `on_render` |
| `upload_rgba8_for_frame` | one-time GPU texture upload inside `on_render` |
| `SpriteRenderer::prepare` / `draw` | instance preparation and alpha-blended draw |

## Limits & Caveats

- Example uses `Rc<RefCell<_>>`, because high-level callbacks are `'static` and
  run on the native event-loop thread. Это удобный sample pattern, не
  многопоточная game architecture.
- Upload выполняется при первом render frame. Не пересоздавайте pipeline или
  GPU texture на каждом frame; храните их в scene/render resource.
- Current renderer has a frame-local depth attachment and automatic surface-loss
  recovery, but no high-level culling, texture-array/bindless path or input-to-ECS
  adapter in this example.
- `SpriteRenderer::new_for_frame` следует вызывать только в initialization
  branch. Presentation format берётся из текущего frame; pipeline надо
  пересоздать, если будущая host policy сменит format.
