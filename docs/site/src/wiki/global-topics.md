# Глобальные темы

> **Статус:** Active policy  
> **Применяется к:** Application, ECS, assets, rendering и gameplay plugins

Эти темы — аналог верхнеуровневых страниц движковой wiki: они описывают
правила, которые не принадлежат одному module.

| Тема | Короткий контракт | Где читать дальше |
|---|---|---|
| Lifecycle | Один host владеет native event loop; `run()` блокирует вызывающий thread. | [Native Application](../guides/application.md) |
| Frames & events | Frame state изменяется на boundary; delayed events становятся видимыми на следующем frame. | [Runtime, ECS и события](../concepts/runtime-ecs-events.md) |
| Assets | Handles typed и generational; удалённый ресурс не становится валидным снова. | [Assets](../concepts/assets.md) |
| Rendering | Один renderer владеет acquire/submit/present; custom code работает внутри render frame. | [Low-level renderer](../guides/custom-render-passes.md) |
| 2D | Sprite regions всегда проверяются against texture bounds; animation не зависит от GPU backend. | [2D-ресурсы](../concepts/two-d.md) |
| Gameplay | Input выражается semantic action, а interaction request ещё не является world fact. | [Gameplay](../concepts/gameplay.md) |
| Compatibility | Windows — единственная verified platform на этом этапе. | [Limits & Compatibility](../reference/limits-and-compatibility.md) |

## Единицы и ownership

- Размеры texture/region измеряются в **physical pixels**.
- World units не определены глобально: gameplay и будущие physics plugins
  выбирают scale явно.
- `AssetId<T>` привязан к конкретному `Assets<T>` и становится stale после
  удаления/переиспользования slot.
- GPU object ownership остаётся у `Renderer`/специализированного renderer
  crate; user code получает узко scoped доступ, а не вторую presentation loop.

## Многопоточность

Thread-affinity каждого public item должна быть видна в rustdoc. Пока нет
public task scheduler или async asset streaming contract, wiki не обещает, что
любая операция безопасна с любого thread. Native window/event loop следует
считать host-thread-only.

## Production правило

`Experimental` APIs подходят для prototype и controlled internal products. Для
внешнего production release разработчик должен фиксировать версию Yuyib и
проверять changelog/limits при обновлении.
