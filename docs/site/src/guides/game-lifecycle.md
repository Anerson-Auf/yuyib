# Игра: окно, ECS и кадр

> **Статус:** экспериментальный  
> **Модуль:** `yuyib::game`

`Game` — высокоуровневая точка входа для игры. Она создаёт `Application`,
владеет одним ECS-миром, plugin registry и тремя schedules: `Startup`,
`FixedUpdate`, `Update`. Поэтому простая игра не должна сама связывать окно,
event loop и `World` через глобальные переменные или потоки.

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

Game::new()
    .window(WindowConfig {
        title: "Моя игра".to_owned(),
        ..Default::default()
    })
    .clear_color(ClearColor::linear(0.03, 0.05, 0.1, 1.0))
    .add_plugin(MovementPlugin)
    .run()?;
# Ok::<(), yuyib::app::ApplicationError>(())
```

## Что делает готовый путь

- `Game` использует continuous render loop по умолчанию; menu/turn-based game
  может явно выбрать `RenderLoop::OnDemand`;
- `Startup` выполняется один раз до открытия event loop;
- `FixedUpdate` выполняется с постоянным timestep и bounded catch-up;
- `Update` выполняется один раз на presentation frame;
- `GameTime`, `FixedTime` и `FixedUpdateStats` доступны как ECS resources;
- `GamePlugin` объединяет resources, systems и integration callbacks одной
  capability без global registry;
- `on_start` и `on_frame` сохранены для небольших callbacks и миграции старого
  кода;
- `request_exit` и `set_cursor_control` доступны из `GameFrame`;
- `on_window_event` и `on_device_event` оставляют явный путь для клавиатуры,
  UI и свободной камеры;
- при feature `ui` метод `ui` помещает `ApplicationUi` поверх игровой сцены;
- `on_render` оставляет доступ к низкоуровневому `RenderFrame` для 2D/3D
  renderer-а и собственных GPU-проходов.

## Limits & Caveats

`Game` не выбирает физический или сетевой backend вместо проекта. Эти
capabilities подключаются plugins. Он не передаёт `World` в GPU callback и не
разрешает фоновым задачам менять мир. Результат загрузки публикуется на main
thread boundary, затем extraction создаёт renderer-owned snapshot.

Полный runnable пример находится в
[`game_plugin_schedule.rs`](../../../../crates/yuyib/examples/game_plugin_schedule.rs).

Если нужен особый порядок событий, несколько окон либо свой task executor,
используйте `yuyib::app::Application`, `yuyib::ecs` и `yuyib::render`
напрямую. Это не другой движок, а нижний уровень того же runtime.
