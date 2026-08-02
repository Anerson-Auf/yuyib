# Contributing

Спасибо за интерес к Yuyib. Проект на стадии foundation: приоритет —
измеримые vertical slices, а не разрозненные API.

## Перед изменением

1. Прочитайте [Architecture README](docs/architecture/README.md) и релевантный ADR.
2. Для Editor/authoring — [ENGINE_INTEGRATION.md](docs/editor/ENGINE_INTEGRATION.md)
   и [CAPABILITY_COVERAGE.md](docs/editor/CAPABILITY_COVERAGE.md).
3. Сверьте milestone с [ROADMAP.md](docs/architecture/ROADMAP.md).

## Invariants (кратко)

- Один host владеет event loop и presentation lifecycle.
- Нет скрытого file I/O / decode / unbounded upload на render thread.
- ECS и imported CPU data не зависят от конкретного GPU backend.
- High-level API сохраняет low-level escape hatch.
- Persisted identity — GUID / schema ID, не `Entity` / `TypeId`.
- Editor preview использует production importer/cooker/renderer path.
- Отсутствующая capability — явный `Unavailable`, не пустой panel.

Полный список — в architecture README.

## Стиль изменений

- Меняйте только затронутую область; не рефакторьте соседние crates «заодно».
- Документация: русский prose, English identifiers и type names.
- Новая curated capability обязана иметь coverage record (или явный
  `CodeOnly` / `Unavailable` с причиной).
- Breaking persisted schema → новая schema version + executable migration.

## Проверки

Запускайте **scoped** команды по изменённым crates. Пример:

```powershell
$env:CARGO_BUILD_JOBS = "2"
$env:RUST_TEST_THREADS = "2"
cargo test -p yuyib-gltf --lib -- --test-threads=2
```

Не запускайте `cargo test --workspace` / `--all-targets` без необходимости.
`xtask` и оконные examples — по явной просьбе maintainer.

Скоуп verification для Editor: [TESTING.md](docs/editor/TESTING.md).

## Pull requests

- Короткое описание: цель, затронутые crates, evidence (tests/examples).
- Укажите, что **не** проверялось (interactive window, full workspace, GPU golden).
- Обновите wiki / limits / coverage, если меняется public contract.

## Лицензия

Вклады принимаются под **MIT OR Apache-2.0**, как и весь workspace.
