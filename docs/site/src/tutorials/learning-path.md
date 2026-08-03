# Учебный путь

> **Назначение:** пройти от пустого окна до playable 2D/3D без угадывания crates.  
> **Уровень:** beginner → intermediate

Этот раздел — **гайды для чайников**. Каждый tutorial отвечает на три вопроса:

1. **Что делаем** — какая цель у шага.
2. **Почему эта функция** — зачем выбран именно этот API, а не соседний.
3. **Что возвращает / чем владеет** — какой тип вы держите в руках и кто за него отвечает.

## Порядок чтения

| Шаг | Tutorial | Результат |
|---|---|---|
| 1 | [Первое окно (`Application`)](first-window.md) | Native window + clear color + frame callback |
| 2 | [Первая игра (`Game` + ECS)](first-game.md) | `World`, schedules, plugin |
| 3 | [High-level glTF-сцена](load-gltf-scene.md) | Асинхронная загрузка карты без зависания окна |
| 4 | [Первый 2D playable](first-2d-playable.md) | Sprite / tilemap / `Game2dScene` |
| 5 | [Physics prototype](../guides/physics.md) | Rapier facade / kinematic controller |

После шагов 1–2 вы уже понимаете host lifecycle. Шаги 3–4 — отдельные vertical slices: 3D и 2D не обязаны идти вместе.

## Что читать параллельно

- [Что вы хотите сделать?](../wiki/use-case-index.md) — если знаете задачу, но не type.
- [Cargo features](../reference/features.md) — если `use yuyib::…` не компилируется.
- [Запускаемые примеры](../reference/examples.md) — канонический runnable source.
- [Limits & Compatibility](../reference/limits-and-compatibility.md) — что ещё Planned.

## Правила Yuyib, которые tutorials повторяют

- **Один host** владеет Windows event loop и presentation (`Application` / `Game`).
- **Worker не мутирует `World` и не трогает GPU.** Результат публикуется на main thread.
- **High-level facade не скрывает ownership.** Escape hatch всегда рядом (`Renderer`, raw schedules, `import_scene_path`).
- **Ошибки typed.** `?` и `Result` — нормальный путь, не silent fallback.
