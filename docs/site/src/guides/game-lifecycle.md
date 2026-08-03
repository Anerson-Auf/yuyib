# Игра: окно, ECS и кадр

> **Статус:** Experimental  
> **Модуль:** `yuyib::game`  
> **Tutorial:** [Первая игра](../tutorials/first-game.md)

`Game` — высокоуровневая точка входа для игры. Она создаёт тот же native
window/GPU path, что [`Application`](application.md), и добавляет **один**
ECS `World`, plugin registry и три schedules: `Startup`, `FixedUpdate`,
`Update`.

## Когда брать `Game`, а не `Application`

| Нужно | Выбор |
|---|---|
| Tool, shell, WebView host без simulation | `Application` |
| Entities, systems, fixed physics step | `Game` |
| Полный custom multi-world / multi-window | Low-level `platform` + `ecs` + `render` |

## Быстрый пример

```rust,no_run
use yuyib::{
    ecs::prelude::{Res, ResMut, Resource},
    game::{FixedTime, Game, GamePlugin, GameSchedule, GameTime},
    platform::WindowConfig,
    render::ClearColor,
};

#[derive(Resource)]
struct Player { position: f32, velocity: f32 }

struct MovementPlugin;

impl GamePlugin for MovementPlugin {
    fn build(self, game: &mut Game) {
        game.world_mut().insert_resource(Player { position: 0.0, velocity: 2.0 });
        game.schedule_mut(GameSchedule::FixedUpdate).add_systems(move_player);
        game.schedule_mut(GameSchedule::Update).add_systems(observe_player);
    }
}

fn move_player(time: Res<FixedTime>, mut player: ResMut<Player>) {
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

Runnable: [`game_plugin_schedule.rs`](../../../../crates/yuyib/examples/game_plugin_schedule.rs).

## Почему эти API

### `Game::new()`

Создаёт host с continuous render loop по умолчанию (игра почти всегда хочет
кадры). Menu/turn-based может явно поставить `RenderLoop::OnDemand`.

### `GamePlugin::build`

Регистрация resources/systems **до** event loop. Plugin — обычное Rust-value,
не dynamic ABI и не скрытый global registry.

### Schedules

```text
on_start / Startup (once)
    ↓
native event loop
    ↓
presentation frame:
    FixedUpdate 0..N  (constant dt, bounded catch-up)
    Update exactly once
    compatibility on_frame
    render snapshot / on_render
```

| Resource | Где брать dt | Зачем |
|---|---|---|
| `FixedTime` | `FixedUpdate` | Deterministic physics/motion |
| `GameTime` | `Update` | Frame index, presentation delta |
| `FixedUpdateStats` | После fixed steps | Сколько steps, сколько dropped time |

**Почему нельзя подставлять presentation delta в physics:** разные FPS →
разная симуляция; spiral of death маскируется. Лишний accumulated time
**сбрасывается наблюдаемо**, а не «догоняется вечно».

### `on_start` / `on_frame` / `on_render`

Оставлены для малого glue и миграции. Production capability лучше класть в
plugin systems. `on_render` даёт `RenderFrame` для 2D/3D renderer; `World` в
GPU callback не передаётся — сначала extraction.

### `ui(...)` (feature `ui`)

`ApplicationUi` поверх сцены в том же frame (alpha blend), не второй window
loop.

## Fixed update config

`FixedUpdateConfig` задаёт timestep, max steps per frame и max accepted frame
delta. Неверный config → typed error при построении, не silent clamp без
диагностики.

## Limits & Caveats

- Один `World` на `Game`.
- Workers не мутируют world; publish на main thread.
- Physics/network backends подключаются features/plugins, не «выбраны Game’ом».
- Escape hatch: `Application` + raw ECS schedules + `Renderer`.

## См. также

- [Tutorial: первая игра](../tutorials/first-game.md)
- [Runtime / ECS concepts](../concepts/runtime-ecs-events.md)
- [Tasks](tasks.md)
- [Input & character](input-character-quests.md)
