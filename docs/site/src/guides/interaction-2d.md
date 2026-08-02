# Gameplay: 2D pointer and touch interaction

> **Статус:** Experimental  
> **Модуль:** `yuyib::gameplay::interaction_2d`

`request_pointer_interaction_2d` turns one started semantic action and a
caller-provided world point into a command-like `InteractionRequested`. It is
renderer-neutral: no `Sprite2d`, window, mouse device, touch backend or camera
projection is required.

```rust
use yuyib::{
    gameplay::{ActionId, ActionStates, ActionValue},
    gameplay::interaction_2d::{
        PointerInteraction2dConfig, request_pointer_interaction_2d,
    },
    physics::Vec2,
};

# let mut world = yuyib::ecs::prelude::World::new();
# let mut actions = ActionStates::default();
# let player = world.spawn_empty().id();
# let (world_x, world_y, frame) = (0.0, 0.0, 1);
// The host maps physical click/touch and screen coordinates itself.
let event = actions.submit(ActionId::new("game.use"), ActionValue::digital(true), frame)
    .expect("a press creates Started");
let outcome = request_pointer_interaction_2d(
    &mut world,
    &actions,
    &event,
    player,
    Vec2::new(world_x, world_y),
    &PointerInteraction2dConfig::default(),
)?;
# let _ = outcome;
# Ok::<(), Box<dyn std::error::Error>>(())
```

The world needs `Position2d + AabbCollider2d` on every hit-testable object and
`Interactable` only on objects that may receive requests. `InteractionLayer2d`
is optional gameplay metadata; its default is `0` and it deliberately has no
connection to a renderer layer.

## Selection contract

`point_in_aabb_2d` includes the AABB boundary. The actor's own collider is
ignored. From all other containing colliders, selection is deterministic:

1. highest `InteractionLayer2d` wins;
2. equal layers use the lowest full ECS entity ID.

The selected collider is checked *after* selection. A high-layer collider with
no `Interactable` returns `NotInteractable` and blocks any interactable beneath
it. This is intentionally equivalent to the 3D raycast adapter's nearest
non-interactable blocker policy, not UI "click through". Place decorative or
pass-through colliders below the target layer, or omit their `AabbCollider2d`.

## Actions, range and outcomes

Only an `ActionPhase::Started` event whose configured action is still active in
`ActionStates` may create a request. A held action or stale event reports an
explicit outcome rather than repeating an interaction.

After selection, the adapter checks `Interactable::enabled` and
`required_action`. For a target with `max_distance`, it compares Euclidean
centre-to-centre distance from the actor's `Position2d`. If the actor has no
position, it returns `MissingActorPosition`; an unrestricted target does not
need an actor position.

`Requested` contains `InteractionRequested { method: InteractionMethod::Pointer,
.. }`. It is an intent only: authority/game logic must validate and resolve
the effect separately.

## Limits & Caveats

- The supplied world point must be finite. Non-finite caller input and invalid
  participating ECS positions/distance calculations return structured errors;
  they are never treated as an off-screen miss.
- The query is O(n) over `Position2d + AabbCollider2d`; it has no broad phase,
  rotated boxes, mesh/visibility test, spatial cache or automatic screen-to-
  world conversion.
- This module does not bind mouse/touch devices, handle pointer capture, decide
  UI-vs-world routing, create gameplay effects or provide networking authority.
- `InteractionLayer2d` is separate from `Sprite2d::layer`, so a host must set
  interaction priority deliberately when visual and hit-test order differ.

Full signatures and every error/outcome: [gameplay Rustdoc](../api/yuyib_gameplay/index.html).
