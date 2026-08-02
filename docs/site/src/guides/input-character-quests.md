# Input, character motor и quests

> **Статус:** Experimental  
> **Модули:** `yuyib::input`, `yuyib::character_3d`, `yuyib::gameplay::quest`

Этот слой закрывает минимальный playable loop без смешивания платформы,
движения и game rules: physical Winit keyboard создаёт semantic actions,
fixed-step motor получает уже готовое desired movement, а quests принимают
только подтверждённые domain signals.

```text
Winit KeyCode -> ActionId / ActionStates -> CharacterInput3d -> motor
                                    \
                                     accepted InteractionResolved -> QuestSignal -> QuestBook
```

## Keyboard actions

`KeyboardActionMap` связывает physical `winit::keyboard::KeyCode` с
`ActionId`; несколько клавиш могут принадлежать одной action, но одна клавиша
не может иметь двух owners. `WinitKeyboardAdapter` получает каждый
`WindowEvent` и выпускает buffered transitions только в выбранной game-frame
boundary.

```rust,no_run
use winit::{event::ElementState, keyboard::KeyCode};
use yuyib::{gameplay::ActionStates, input::*};

let mut map = KeyboardActionMap::new();
map.bind(KeyCode::KeyE, "game.use")?;
let mut keyboard = WinitKeyboardAdapter::new(map);
let mut actions = ActionStates::default();

keyboard.handle_key_code(KeyCode::KeyE, ElementState::Pressed);
let events = keyboard.emit_frame(&mut actions, 42);
assert_eq!(events.len(), 1);
# Ok::<(), Box<dyn std::error::Error>>(())
```

Передайте `WindowEvent::Focused(false)` в `handle_window_event`: adapter
очищает held keys и на следующем `emit_frame` выдаёт обычный semantic
`Canceled`. Это предотвращает stuck movement после alt-tab. Event order
stable: по `ActionId`, не по порядку OS callbacks.

## Fixed-step character motor

`CharacterMotor3d` не читает keyboard и не знает о camera. Caller переводит
semantic state в world-space `CharacterInput3d` и вызывает `step` ровно один
раз на своем fixed tick. ECS adapter обновляет `LocalTransform3d`, после него
обязателен `propagate_world_transforms`.

```rust,no_run
use yuyib::{
    character_3d::{CharacterInput3d, CharacterMotor3d, CharacterMotorConfig3d},
    physics::{Vec2, Vec3},
};

let mut motor = CharacterMotor3d::new(
    CharacterMotorConfig3d::default(),
    Vec3::new(0.0, 0.5, 0.0),
)?;
let input = CharacterInput3d::new(Vec2::new(0.0, 1.0), false)?;
let step = motor.step(input)?;
assert!(step.events().is_empty());
# Ok::<(), Box<dyn std::error::Error>>(())
```

`CharacterInput3d::new` normalizes diagonal/analogue magnitude above one.
`CharacterMotorEvent3d::{Jumped, Landed}` — domain-friendly transitions, а
не input events. Для ECS используйте
`step_character_motors_3d(&mut world, input_for)`, затем
`propagate_world_transforms(&mut world)`.

## Игрок внутри карты: стены, пол и прыжок

Для обычной статической карты не собирайте свой цикл из десятка запросов.
Высокий API `CharacterController3d` получает `TriangleMesh3d`, сохраняет
скорость и grounded state, сам применяет fixed step, прыжок и разрешение
сферы о точные треугольники. Карта остаётся коридором: у неё нет ложной
сплошной AABB-стены вокруг всего уровня.

```rust,no_run
use yuyib::{
    character_3d::{CharacterController3d, CharacterControllerConfig3d, CharacterInput3d},
    physics::{TriangleMesh3d, Vec2, Vec3},
};

# let vertices = [Vec3::new(0.0, 0.0, 0.0), Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 1.0)];
# let indices = [0, 2, 1];
let map_collision = TriangleMesh3d::from_indexed(&vertices, &indices)?;
// Для закрытого коридора сам найдёт пол с местом для игрока и потолком.
let mut player = CharacterController3d::spawn_in_triangle_mesh(
    CharacterControllerConfig3d::default(),
    &map_collision,
)?;
let step = player.step_on_triangle_mesh(
    CharacterInput3d::new(Vec2::new(0.0, 1.0), false)?,
    &map_collision,
)?;
println!("contacts this tick: {}", step.contacts());
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Playermodel placement (scale / facing / feet)

Не собирайте column-major matrix вручную в example. Высокий API
`CharacterModelPlacement3d` ставит root на ноги контроллера, поворачивает
локальный **+Z** к horizontal facing и применяет uniform scale:

```rust,no_run
use yuyib::{
    character_3d::{CharacterController3d, CharacterControllerConfig3d, CharacterModelPlacement3d},
    physics::{Vec2, Vec3},
};

# let controller = CharacterController3d::new(
#     CharacterControllerConfig3d::default(),
#     Vec3::new(0.0, 0.5, 0.0),
# )?;
let root = controller
    .model_placement(Vec2::new(0.0, -1.0), 0.3)?
    .model_to_world();
// later: placement.with_uniform_scale(0.25)?.model_to_world()
# let _ = root;
# Ok::<(), Box<dyn std::error::Error>>(())
```

## First / third person follow camera

`CharacterFollowCamera3d` склеивает free-look и collision-aware chase boom.
`toggle_mode()` переключает вид; в first-person `draws_playermodel()` == false.

```rust,no_run
use yuyib::input::{
    CharacterFollowCamera3d, FreeCameraConfig3d, ThirdPersonCameraConfig3d,
};

let mut camera = CharacterFollowCamera3d::looking_at(
    FreeCameraConfig3d::default(),
    ThirdPersonCameraConfig3d::default(),
    [0.0, 2.0, 4.0],
    [0.0, 1.5, 0.0],
)?;
camera.toggle_mode();
assert!(!camera.draws_playermodel());
# Ok::<(), Box<dyn std::error::Error>>(())
```

`spawn_in_triangle_mesh` — высокий API для быстрого запуска карты. Он
детерминированно перебирает почти горизонтальные поверхности от центра карты,
проверяет свободную коллизионную сферу и, по умолчанию, достаточную высоту до
потолка. Поэтому верхняя внешняя поверхность карты не становится player start.
Для открытой карты передайте `CharacterSpawnOptions3d { require_ceiling: false,
..Default::default() }` в `spawn_in_triangle_mesh_with_options`.

Если разрешённые поверхности вынесены в semantic layer (`street`, `road`,
`walkable`), проверять свободное место только по этому layer нельзя: в нём уже
нет стен и потолков здания. Передайте surface layer и полный collider отдельно:

```rust,ignore
let player = CharacterController3d::spawn_on_surface_mesh_with_options(
    CharacterControllerConfig3d::default(),
    street_layer.mesh(),
    full_map.mesh(),
    CharacterSpawnOptions3d::outdoor_lowest(Vec2::ZERO)
        .with_maximum_horizontal_distance(14.0)
        .with_minimum_open_sky_clearance(12.0),
)?;
```

Кандидаты берутся только из `street_layer`, но sphere clearance, стены,
низкие потолки и open-sky ray проверяются по `full_map`. Это предотвращает
спавн на геометрически корректной дороге, проходящей внутри здания.

Если расположение задаёт editor или gameplay, низкий API остаётся простым:
`CharacterController3d::new` плюс `place_on_triangle_mesh`. Для собственного
поиска пола, телепорта или editor picking используйте
`TriangleMesh3d::raycast(Ray3d, max_distance)`: он возвращает точный hit
треугольника, позицию и normal без скрытой логики появления игрока.

`CharacterControllerEvent3d::{Jumped, Landed, Collided}` подходит для звука,
анимации и UI. Высокий путь карты делит обычное fixed movement на небольшие
шаги: поэтому прыжок не проходит через тонкий потолок. Это **не** dynamic
rigid-body physics и не непрерывная collision detection: статическая карта,
сфера и ограниченное число проверок. Внутри `TriangleMesh3d` далёкие лица
сначала отсекаются по сохранённым AABB, но это лишь дешёвый консервативный
фильтр: для очень большой или стримящейся карты всё равно нужен собственный
пространственный индекс. Не устанавливайте огромные скорость или fixed delta
— для такого случая подключайте низкий `step_with_collision` к своему
CCD/broad phase.

Если карта использует свой broad phase или иной движок физики, низкоуровневый
`step_with_collision(input, resolver)` получает желаемую позицию центра и
радиус сферы. Resolver возвращает `CharacterCollisionResolution3d` (позиция,
grounded, число контактов); управление прыжком и fixed-step policy остаются
одинаковыми. Это точка вмешательства, а не второй параллельный character API.

## Quests from confirmed events

`QuestDefinition` задаёт non-empty набор counters. `QuestBook` — ECS
`Resource`, но может использоваться и как plain state. Его `QuestSignal` не
принимает zero amount, ordering definitions/objectives стабилен, counters
saturate/clamp к target. `snapshot` возвращает detached state для будущего
save layer; сериализацию, migration и authority она намеренно не решает.

```rust,no_run
use yuyib::gameplay::{
    QuestBook, QuestDefinition, QuestId, QuestObjective, QuestSignal,
};

let mut quests = QuestBook::default();
quests.register(QuestDefinition::new(
    "main.restore_power",
    vec![QuestObjective::new("generator", "world.generator_activated", 2)?],
)?)?;
let quest = QuestId::new("main.restore_power");
quests.start(&quest)?;

// Emit this only after InteractionResolved has outcome Accepted.
let transitions = quests.apply_signal(&QuestSignal::new("world.generator_activated", 1)?);
assert_eq!(transitions.len(), 1);
# Ok::<(), Box<dyn std::error::Error>>(())
```

Не выдавайте `QuestSignal` из raw `InteractionRequested`: request — намерение,
не подтверждённый result. Local client, server или script authority должен
сначала подтвердить interaction и только затем превратить accepted outcome в
signal.

## Runnable vertical slice

В repository есть headless end-to-end example:

```powershell
cargo run -p yuyib --example playable_vertical
```

`crates/yuyib/examples/playable_vertical.rs` показывает один `game.use` press,
fixed motor tick, sphere raycast, explicit
authority acceptance и progression objective. Он намеренно не открывает окно
и не создаёт GPU: Winit event loop, fixed-tick accumulator и trust boundary
принадлежат host application.

После успешной проверки пример сразу завершает процесс с кодом `0` и печатает
краткий итог в консоль. Это не оконная демо-игра: его задача — быстро
проверить связку игровых систем без платформы и рендера. Для окна с управляемой
камерой используйте `gltf_map_static_scene` или `gltf_map_loading_screen`.

## Limits & Caveats

- Input adapter поддерживает только identified physical keyboard `KeyCode`.
  Mouse, gamepad, text/IME, touch, rebinding UI и persistent bindings пока не
  входят в contract.
- `CharacterMotor3d` сохраняет прежний простой infinite-ground-plane режим.
  `CharacterController3d` добавляет static triangle-map стены/пол,
  настраиваемый max walkable slope (`max_walkable_slope_radians`) и kinematic
  moving platforms (`step_on_triangle_mesh_with_platform`), но всё ещё не
  имеет step-offset climbing, CCD, broad phase или camera policy.
- Quest engine не загружает definitions из файлов, не сериализует snapshot,
  не делает replication и не устанавливает trust/authority boundary. Это
  обязанность game/application layer.

Полный API: [input](../api/yuyib_input/index.html),
[character motor](../api/yuyib_character_3d/index.html) и
[gameplay/quests](../api/yuyib_gameplay/quest/index.html).
