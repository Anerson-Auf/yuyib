# Как устроена эта документация

> **Статус:** Active policy  
> **Область:** все public crates, examples и guides

Эта wiki — часть public API contract. Изменение public type, method, error,
default, platform support или observable limit считается неполным, пока оно не
отражено здесь и в `rustdoc` рядом с исходником.

## Два слоя документации

| Слой | Для чего нужен | Источник истины |
|---|---|---|
| Wiki guides | архитектура, workflow, выбор API, ограничения, полноценные примеры | страницы `docs/site/src` |
| API reference | все public crates, modules, types, traits, methods, errors и constants | doc comments в Rust + `cargo doc` |

Guide никогда не является исчерпывающим перечнем методов. Rustdoc никогда не
должен скрывать lifecycle, performance или platform caveat, нужный для
безопасного применения API.

## Правило изменения API

Перед merge любой public API change обязан обновить:

1. doc comments у public item и runnable/doctest example, если он уместен;
2. строку в [API Reference и покрытии](../reference/api-reference.md);
3. relevant guide либо новую guide-страницу, если появился новый workflow;
4. [Limits & Compatibility](../reference/limits-and-compatibility.md), если
   изменились budgets, fallback, platform support или performance contract;
5. этот `SUMMARY.md`, если capability должна быть доступна из навигации.

Если пункт не применим, change description должен объяснить почему. Это не
процесс ради процесса: так пользователь не обнаружит hidden API только после
чтения исходников.

## Генерация reference

Единый HTML-сайт строится workspace command:

```powershell
cargo run -p xtask -- docs
```

Команда запускает `cargo doc --no-deps --all-features` для каждого public
crate, строит mdBook и копирует generated Rustdoc в `docs/site/book/api/`.
`--all-features` обязателен: иначе feature-gated WebView API исчезнет из
reference, хотя остаётся доступным разработчику. Не публикуйте результат
голого `mdbook build`: он проверяет wiki-разметку, но не добавляет Rustdoc.
Wiki хранит
coverage map и ссылки на crate/module names, но не копирует сигнатуры вручную:
все доступные public items остаются canonical в embedded Rustdoc.

Откройте `docs/site/book/index.html`, затем `api/yuyib/index.html`. При
изменении списка public crates его нужно одновременно обновить в
`xtask/src/main.rs` и [coverage map](../reference/api-reference.md).

## Статусы API

- **Stable** — semantic-versioning contract.
- **Experimental** — работает, но может меняться в minor release.
- **Planned** — направление, а не доступная возможность.
- **Research** — контракт ещё не принят.
- **Deprecated** — есть replacement и срок удаления.

Полное определение и критерии — в [статусе API](../reference/api-stability.md).
