# World interactions: Enter, Stay, Exit и hold-to-interact

> **Статус:** Experimental  
> **Модуль:** `yuyib::gameplay::world_interaction`  
> **Example:** `world_interaction_flow` (headless, без fixtures)

`WorldInteractionState` — общий high-level lifecycle поверх уже существующих
2D/3D queries. Spatial adapter выбирает цель, input adapter передаёт только
semantic состояние действия, а state machine выдаёт одинаковые события для
клавиатуры, gamepad, touch, accessibility input и authoritative server.

```text
2D hit-test / 3D raycast / overlap / script
                    │
                    v
       Option<WorldInteractionTarget<Id>>
                    + semantic action active
                    + fixed delta
                    │
                    v
 Entered → Stayed + Progress → Interacted → Exited
                              │
                              v
                  InteractionRequested
                              │
                              v
                    authority validation
```

## Минимальный hold interaction

```rust
use std::time::Duration;
use yuyib::{
    ecs::prelude::World,
    gameplay::{
        InteractionId, InteractionMethod, WorldInteractionActivation,
        WorldInteractionEvent, WorldInteractionState, WorldInteractionTarget,
    },
};

let mut world = World::new();
let actor = world.spawn_empty().id();
let door_entity = world.spawn_empty().id();
let door = WorldInteractionTarget::new(
    door_entity,
    InteractionId::new("world.open_door"),
).with_activation(
    WorldInteractionActivation::hold(Duration::from_millis(800))?
);
let mut state = WorldInteractionState::default();

let events = state.step(Some(door), true, Duration::from_millis(200));
for event in events {
    if let WorldInteractionEvent::Progress { fraction, .. } = &event {
        println!("hold: {:.0}%", fraction * 100.0);
    }
    if let Some(request) = event.interaction_request(
        actor,
        InteractionMethod::Proximity,
    ) {
        // Client request is not yet a world fact: validate authority first.
        println!("request for {}", request.interaction);
    }
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

Для обычного нажатия используйте `WorldInteractionActivation::press()` — это
default у `WorldInteractionTarget::new`.

## Как подключаются 2D и 3D

State machine намеренно не делает physics query:

- 2D: результат `request_pointer_interaction_2d` даёт выбранный `Entity` и
  `InteractionId`; соберите из них `WorldInteractionTarget`;
- 3D: `request_use_raycast_3d` возвращает те же semantic данные вместе с hit;
- overlap/trigger: application-owned overlap tracker выбирает target, а
  `WorldInteractionState` нормализует lifecycle;
- headless/server: target может прийти из navigation, script или validated
  client intent без renderer и window.

`Entered`, `Stayed`, `Exited` относятся к **текущей выбранной interaction
цели**, а не заменяют physics contact events. Это важно: один broad-phase может
видеть десятки overlaps, тогда как UI prompt обычно имеет одну deterministic
цель.

## Typed ID

Generic параметр `Id` может быть application enum/newtype:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UseTarget { Door, Generator, Npc }

let target = WorldInteractionTarget::new(entity, UseTarget::Generator);
let mut state = WorldInteractionState::<UseTarget>::default();
```

Для интеграции с готовым `InteractionRequested` используйте стандартный
`InteractionId`: только событие `Interacted` преобразуется в request. Local ECS
`Entity` не является stable network/save ID; сериализуйте свой semantic ID.

## Fixed schedule и input

Передавайте `delta` из `FixedUpdate`, иначе длительность hold будет зависеть от
presentation FPS. `action_active` — уже semantic sample. Например, его можно
получить из `ActionStates`, но state machine не требует конкретного input API:

```rust
let active = actions.get(&use_action).is_some_and(|state| state.is_active());
let events = interaction.step(selected_target, active, fixed_time.delta);
```

При смене target уже удерживаемое действие не переносит progress и не активирует
новый объект. Необходимо отпустить и нажать снова. Это предотвращает случайное
использование соседнего объекта при повороте камеры. `clear()` предназначен для
focus loss и не выпускает `Exited`; если gameplay должен увидеть exit, сначала
вызовите `step(None, false, delta)`.

## Limits & Caveats

- Один state machine отслеживает одну выбранную цель и одно semantic действие.
  Для нескольких actors храните отдельный state на actor/controller.
- Один `step` возвращает не больше четырёх событий; скрытой unbounded queue нет.
- Query priority, line of sight, distance, cooldown, permissions и authority
  остаются в spatial/game rules layer.
- `Progress` — presentation-friendly observation, но world mutation должна
  происходить только после `Interacted` и authority validation.
- State не сериализуется автоматически. Save/network layer должен хранить
  semantic ID и собственную policy восстановления незавершённого hold.
