# RFC 0005 — высокий игровой lifecycle

- **Статус:** accepted
- **Дата:** 2026-07-31
- **Зависит от:** RFC 0001, RFC 0004

## Проблема

`Application` правильно владеет окном и GPU surface, но игра поверх него
вынуждена самостоятельно хранить ECS-мир в `Rc<RefCell<_>>`, согласовывать
границу кадра и решать, где допустима мутация мира. Такой код быстро
расползается по examples и создаёт ложное впечатление, что фоновые загрузчики
или renderer могут менять `World` в любой момент.

## Решение

Вводится `yuyib-game` и фасад `yuyib::game::Game`.

```text
Game::on_start  -> подготовка одного World до открытия окна
Application     -> окно, OS events, surface acquire/present
Game::on_frame  -> единственная высокая фаза мутации World
on_render       -> GPU-проходы над уже подготовленным snapshot
```

`GameFrame` выдаёт `&mut World`, `FrameInfo`, запрос завершения и настройку
курсора только на время `on_frame`. Он не передаёт `World` в GPU callback и
не создаёт скрытый scheduler. Это намеренное ограничение: в раннем runtime
проект должен явно выбрать физику, порядок ECS-систем, extraction и сетевую
authority model.

## Высокий и низкий уровни

Высокий путь:

```rust,no_run
use yuyib::game::Game;

Game::new()
    .on_start(|world| { let _ = world.spawn_empty(); })
    .on_frame(|game| { let _world = game.world(); })
    .run()?;
# Ok::<(), yuyib::app::ApplicationError>(())
```

Низкий путь остаётся прежним: `Application`, `yuyib::ecs::prelude`,
`RenderFrame`, raw input и custom GPU-passes доступны отдельно. `Game` также
передаёт `on_window_event`, `on_device_event`, `on_render` и feature-gated
native UI, не требуя форка facade.

## Не-цели этого RFC

- универсальное расписание ECS;
- lifetime ECS-контекста между потоками;
- автоматическая репликация мира;
- renderer, который читает `World` прямо в GPU callback;
- объединение assets, physics и audio в скрытый global singleton.

Эти части добавляются отдельными plugins/фасадами с собственными границами
потоков, лимитами и диагностикой.
