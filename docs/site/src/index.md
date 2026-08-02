# Yuyib Wiki

Yuyib — native-first Rust runtime для Windows applications, 2D/3D games и optional WebView surfaces. Native UI является стандартным интерфейсным путём; WebView подключается только там, где HTML/CSS действительно полезны.

Проект находится на раннем этапе foundation. API и возможности, которые ещё не реализованы, маркируются как **Planned** или **Experimental** и не должны восприниматься как release guarantee.

> **Foundation / Experimental.** Wiki описывает только public contract, который
> существует в текущем workspace. Целевые возможности явно отмечаются
> **Planned** или **Research**.

## Навигация

- **[Что вы хотите сделать?](wiki/use-case-index.md)** — task-first индекс:
  найдите действие вроде «изменить размер модели» и перейдите к готовому
  workflow, не угадывая crate.
- **Глобальные темы** фиксируют правила, которые действуют во всех modules:
  lifecycle, ownership, performance, platform support и compatibility.
- **Guides** объясняют задачи и production patterns; они не заменяют reference.
- **API Reference** — точный каталог public types/methods, который ведёт к
  `rustdoc` после его локальной генерации.
- **Limits & Caveats** описывает лимиты, fallback behavior и performance trade-offs без скрытых допущений.
- Каждый runnable example должен ссылаться на настоящий исходник из workspace, а не на вручную скопированный фрагмент.

Если вы впервые открыли проект, начните с [начала работы](getting-started.md).
Если уже знаете задачу — используйте [индекс use-cases](wiki/use-case-index.md).
Если знаете type/module — откройте [карту подсистем](reference/subsystems.md)
или полный [`rustdoc`](api/yuyib/index.html).
Если код не ведёт себя ожидаемо — начните с
[troubleshooting по симптомам](reference/troubleshooting.md).

## Реализовано в foundation

| Подсистема | Статус |
|---|---|
| Runtime, frame clock и frame-boundary events | Experimental |
| ECS facade над `bevy_ecs` | Experimental |
| Windows window (`winit`) | Experimental |
| WGPU surface, resize и render frame | Experimental |
| High-level `Application` loop | Experimental |
| Typed generational in-memory asset handles | Experimental |
| 2D metadata, sheets и deterministic animation | Experimental |
| PNG/JPEG/WebP decoding с resource budgets | Experimental |
| Instanced GPU sprite rendering | Experimental |
| Semantic actions, interactions и triggers | Experimental |
| Native UI tree, layout, input foundation и text asset layers | Experimental |
| Windows WebView2 local-page/typed-bridge overlay | Experimental |
| Source 1 VMF, VMT/VTF и bounded local base-texture boundary | Experimental |
| Source 2 importer | Research |

Renderer: clear/render-frame, instanced 2D sprites, static 3D meshes,
textured-unlit, Lambert и glTF PBR (direct light, IBL, shadows, alpha-mask,
bloom, FXAA, SSAO, color grade) как experimental usable MVP. ECS
materialization, LOD selection, glTF import, cook cache и Source 1 brush
compilation доступны отдельными slices. Открыты: chunking oversized
primitives, validated CSM / TAA / GTAO, true GPU instancing; Source 2 —
Research. См. [ограничения и совместимость](reference/limits-and-compatibility.md)
и [roadmap](../../architecture/ROADMAP.md).
