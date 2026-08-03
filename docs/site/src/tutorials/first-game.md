# Tutorial: первая игра (`Game` + ECS)

> **Статус:** Experimental  
> **Requires:** feature `game` (входит в `desktop-full`)  
> **Цель:** понять, зачем `Game` поверх `Application`, и как живут schedules

Предыдущий шаг: [Первое окно](first-window.md).

Runnable:

```powershell
cargo run -p yuyib --example game_plugin_schedule
```

## 1. Зачем не остаться на `Application`

`Application` даёт окно и кадр. Игре обычно нужно ещё:

| Потребность | Кто закрывает |
|---|---|
| Хранить entities / components | ECS `World` |
| Двигать физику с **фиксированным** dt | `FixedUpdate` |
| Один раз на кадр обновлять камеру/UI | `Update` |
| Сгруппировать capability (input, scene, physics) | `GamePlugin` |

Можно связать `Application` + raw `World` вручную. `Game` делает это **явно и один раз**, без global mutable state и без передачи `World` в GPU callback.

## 2. Каркас

```rust,no_run
use yuyib::{
    ecs::prelude::{Res, ResMut, Resource},
    game::{FixedTime, Game, GamePlugin, GameSchedule, GameTime},
    platform::WindowConfig,
    render::ClearColor,
};

#[derive(Resource)]
struct Player {
    position: f32,
    velocity: f32,
}

struct MovementPlugin;

impl GamePlugin for MovementPlugin {
    fn build(self, game: &mut Game) {
        game.world_mut()
            .insert_resource(Player { position: 0.0, velocity: 2.0 });
        game.schedule_mut(GameSchedule::FixedUpdate)
            .add_systems(move_player);
        game.schedule_mut(GameSchedule::Update)
            .add_systems(observe_player);
    }
}

fn move_player(time: Res<FixedTime>, mut player: ResMut<Player>) {
    // FixedTime.delta — постоянный шаг симуляции, не presentation delta.
    player.position += player.velocity * time.delta.as_secs_f32();
}

fn observe_player(time: Res<GameTime>, player: Res<Player>) {
    let _ = (time.frame, player.position);
}

fn main() -> Result<(), yuyib::app::ApplicationError> {
    Game::new()
        .window(WindowConfig {
            title: "Моя игра".to_owned(),
            ..Default::default()
        })
        .clear_color(ClearColor::linear(0.03, 0.05, 0.1, 1.0))
        .add_plugin(MovementPlugin)
        .run()
}
```

## 3. Почему эти функции

### `Game::new() -> Game`

Создаёт host с:

- одним `World`;
- schedules `Startup`, `FixedUpdate`, `Update`;
- **continuous** render loop по умолчанию (игра почти всегда хочет кадры).

Почему не `World::new()` снаружи? Потому что `Game` связывает world с frame boundary: когда крутить FixedUpdate, когда Update, когда нельзя трогать GPU.

### `.add_plugin(MovementPlugin) -> Self`

`GamePlugin::build` — место регистрации resources и systems **до** `run`.

Почему plugin, а не «весь код в `main`»? Capability (движение, физика, UI) должны добавляться и убираться композицией. Plugin — caller-owned value, **не** dynamic DLL ABI.

### `game.world_mut().insert_resource(...)`

`Resource` — данные без entity (player state, config, scores).

Почему `Resource`, а не global `static`? Потому что systems получают явный `Res`/`ResMut`, а tests могут создать другой `World`.

### `schedule_mut(GameSchedule::FixedUpdate).add_systems(...)`

| Schedule | Когда | Зачем |
|---|---|---|
| `Startup` | Один раз до event loop | Spawn level, load config |
| `FixedUpdate` | 0..N раз за presentation frame | Physics / deterministic motion |
| `Update` | Ровно один раз за кадр | Camera, animation visual, UI glue |

**Почему `FixedTime` в `move_player`, а не `GameTime`?**  
Presentation delta прыгает (vsync, hitch). Физика на variable dt даёт разный результат на 30/144 FPS. `FixedTime::delta` — объявленный timestep; лишний накопленный time отбрасывается наблюдаемо (`FixedUpdateStats`), spiral of death не прячется.

### `.run()`

Тот же blocking host, что у `Application`, плюс schedules вокруг frame.

## 4. Callbacks vs systems

| API | Когда |
|---|---|
| `GamePlugin` + systems | Основной production path |
| `on_start` / `on_frame` | Маленький glue, миграция, быстрый prototype |

Systems лучше масштабируются: ordering, plugins, тестируемость. Callbacks оставляют «скрипт на коленке».

## 5. Чего `Game` намеренно не делает

- Не выбирает physics/network backend за вас — это plugins / features (`physics-rapier`, …).
- Не отдаёт `World` в `on_render` — extraction → snapshot → GPU.
- Не загружает glTF сам — см. [загрузку glTF](load-gltf-scene.md).

## Limits & Caveats

- Один `World` на `Game`.
- Background workers не мутируют world напрямую.
- Menu/turn-based может явно поставить `RenderLoop::OnDemand`.

## См. также

- Guide: [Игра: окно, ECS и кадр](../guides/game-lifecycle.md)
- Concepts: [Runtime, ECS и события](../concepts/runtime-ecs-events.md)
- Следующие tutorials: [glTF](load-gltf-scene.md), [2D](first-2d-playable.md)
