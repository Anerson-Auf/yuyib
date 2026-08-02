# Gameplay: actions, interactions и triggers

> **Статус:** Experimental  
> **Crate / module:** `yuyib::gameplay`  
> **Платформы:** platform-neutral metadata/ECS

Gameplay foundation не знает о клавише `E`, конкретном physics backend или
renderer. Adapter переводит physical input в semantic action, а query plugin
находит target через raycast, overlap, touch или UI hit-test.

## Поток событий

```text
keyboard / gamepad / touch
        -> ActionId("game.use")
        -> InteractionRequested
        -> authority validation
        -> InteractionResolved
```

`InteractionRequested` — command-like intent, а не доказательство, что дверь
открылась или item поднят. После validation application/game logic выпускает
свой domain event. Это оставляет один workflow для local game, headless server
и future networking.

## Основные entry points

| API | Задача |
|---|---|
| `ActionId`, `ActionValue`, `ActionStates` | Semantic input и lifecycle `Started/Performed/Canceled`. |
| `Interactable` | ECS component с capability, optional action и range. |
| `InteractionRequested` / `InteractionResolved` | Intent и authoritative outcome. |
| `interaction_2d::request_pointer_interaction_2d` | Started action + finite world point → deterministic top AABB target; see [2D pointer interaction](../guides/interaction-2d.md). |
| `WorldInteractionState` | Общий 2D/3D focus lifecycle, press/hold progress и `Interacted`; see [world interactions](../guides/world-interactions.md). |
| `Trigger`, `TriggerEvent` | Passive trigger metadata и enter/stay/exit event. |
| `QuestDefinition`, `QuestBook`, `QuestSignal` | Event-driven objective counters и lifecycle transitions. |

## Limits & Caveats

- `ActionStates` не делает device binding, dead zones, repeat policy или focus
  handling. `yuyib::input` даёт первый Winit keyboard adapter, остальные
  device backends остаются отдельными adapters.
- `ActionValue` clamped к `[-1.0, 1.0]`; `NaN` и infinity превращаются в zero.
- `Interactable::with_max_distance` принимает только finite non-negative
  values, но **не** исполняет distance/line-of-sight query.
- 2D pointer adapter выбирает один topmost `AabbCollider2d` по explicit
  `InteractionLayer2d`, затем entity ID. Non-interactable верхний collider
  блокирует lower target; query O(n), без screen-to-world conversion или
  broad phase. Подробный contract — [2D pointer interaction](../guides/interaction-2d.md).
- `Trigger` не создаёт collider. Physics/tilemap/script plugin обязан задать
  overlap semantics и не должен считать `Stayed` бесплатным для больших world.
- `WorldInteractionState` отслеживает одну выбранную цель; selection priority,
  line of sight и authority остаются в caller-owned query/game rules layer.
- `QuestBook` хранит deterministic counters и отдаёт detached snapshot, но
  serialization, migrations, networking/replication и authority policy пока
  принадлежат application/game layer. Не передавайте raw ECS entity IDs между
  process-ами как сетевой protocol.
