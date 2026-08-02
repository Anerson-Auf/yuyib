# Background tasks: bounded CPU pool

> **Статус:** Experimental  
> **Crate / module:** `yuyib::tasks` (`yuyib-tasks`)  
> **Назначение:** CPU/background work; это не async runtime

`TaskPool` — явно owned pool с фиксированным числом workers и bounded queue.
Используйте его для parsing, asset preparation и CPU calculations. Он не
создаёт global executor, не владеет I/O reactor и не заменяет Tokio.

## Быстрый пример

```rust
use yuyib::tasks::{TaskPool, TaskPoolConfig};

fn expensive_cpu_work() -> usize { 42 }

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let pool = TaskPool::new(TaskPoolConfig::new(4, 128)?)?;
    let task = pool.try_spawn(expensive_cpu_work)?;

    match task.join() {
        Ok(value) => println!("completed: {value}"),
        Err(error) => eprintln!("task failed: {error}"),
    }

    pool.shutdown()?;
    Ok(())
}
```

`TaskPoolConfig` требует ненулевое число workers и queue capacity. Queue bound
— обязательный resource contract, а не advisory metadata.

## Выбрать `spawn` или `try_spawn`

| API | Когда возвращается | Поведение при полной queue |
|---|---|---|
| `try_spawn` | сразу | `TaskSpawnError::Full` |
| `spawn` | после принятия job | блокируется до свободного slot |

Не вызывайте blocking `spawn`, удерживая application locks, которые нужны
worker: это может создать deadlock. В event/render loop обычно используйте
`try_spawn` и явно обрабатывайте backpressure.

Оба метода возвращают `TaskSpawnError::Closed`, когда начались `close` или
`shutdown`.

## Получить результат

- `Task::join` ждёт завершения;
- `Task::try_take` возвращает `None`, пока job выполняется;
- готовый result можно забрать только один раз; повторный вызов возвращает
  `TaskError::AlreadyTaken`;
- drop `Task<T>` прекращает только наблюдение за result и **не отменяет** уже
  принятую job.

Main/render thread не должен вызывать `join` для долгой работы. Poll
`try_take` на frame boundary или передавайте результат через owner-specific
publication layer, например `AssetLoadQueue`.

## Panic и lifecycle

Каждая user job выполняется внутри `catch_unwind`. Rust panic превращается в
`TaskError::Panic`, а worker продолжает принимать следующие jobs. Текст panic
payload не переносится. Process abort и failures вне protected boundary не
могут быть normal task result.

`TaskPool::close` запрещает новые submissions и даёт workers закончить уже
принятые jobs. `shutdown` выполняет close и joins всех workers. Drop pool делает
то же самое: detached threads нарушили бы ownership приложения.

`shutdown` и drop блокируются, пока не завершатся все accepted jobs. API не
имеет forced thread termination. Job, которая никогда не возвращает control,
может навсегда заблокировать shutdown; termination behavior принадлежит
пользовательскому коду.

## Один pool на subsystem/application

Не создавайте новый pool для каждого asset или streaming zone: fixed workers
и queues должны быть ограничены на уровне owner. Передавайте `Arc<TaskPool>` в
несколько loaders, когда API поддерживает application-owned pool. Это даёт
общий backpressure и предсказуемое число threads.

## API

| Задача | Type / method |
|---|---|
| Настроить workers/queue | `TaskPoolConfig` |
| Владеть workers | `TaskPool` |
| Не блокировать submitter | `TaskPool::try_spawn` |
| Дождаться свободной queue | `TaskPool::spawn` |
| Poll/join typed result | `Task<T>` |
| Запретить новые jobs | `TaskPool::close` |
| Drain и join workers | `TaskPool::shutdown` |

Полные signatures и errors: [`yuyib_tasks`](../api/yuyib_tasks/index.html).

## Limits & Caveats

- Нет priorities, work stealing, per-task CPU budgets и timers.
- Нет Futures executor, network I/O reactor или global scheduler.
- Нет cancellation/preemption, timeout и built-in cooperative token.
- Нет tracing-context propagation или structured concurrency.
- Один бесконечный user job способен заблокировать shutdown/drop.

## См. также

- [Asset loading](asset-loading.md)
- [Streamed glTF scene](streamed-gltf-scene.md#shared-task-pool)
- [Networking](networking.md)

