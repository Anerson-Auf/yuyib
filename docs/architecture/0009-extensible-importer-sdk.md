# RFC 0009 — extensible typed importer SDK

- **Статус:** accepted, first registry slice implemented
- **Дата:** 2026-08-01
- **Зависит от:** RFC 0001, RFC 0002

## Проблема

Новые source formats не должны требовать изменений `yuyib-assets`, ECS или
renderer. При этом registry из `Box<dyn Any>` и строковых property bags потерял
бы compile-time contracts, а загрузка произвольных Rust DLL опиралась бы на
нестабильный ABI и внесла `unsafe` в главный extension boundary.

## Решение

Importer является обычным Rust crate и реализует `AssetImporter<T>`, где `T` —
конкретный neutral output type. Сильные настройки являются полями конкретного
importer-а; type erasure происходит только внутри `ImporterRegistry<T>` после
регистрации. Output type, ошибки importer-а и metadata остаются типизированными.

```text
trusted resolver -> bounded ImportSource bytes
                 -> ImporterRegistry<NeutralType>
                 -> probe / deterministic dispatch
                 -> AssetImporter<NeutralType>
                 -> validated ImportResult<NeutralType>
                 -> cooker or PreparedAsset<RuntimeType>
```

Registry ограничивает source/probe bytes, число plugins, dependencies,
diagnostics и длины metadata. Равный лучший probe score является
`ImportError::Ambiguous`, а не зависит от случайного registration order.

## Security boundary

Source bytes считаются untrusted. Importer получает slice и логический URI, но
не получает implicit filesystem, network, GPU, window или ECS access.
Dependencies возвращаются как logical requests и разрешаются host policy.

Сам native Rust importer является trusted executable code: trait не является
sandbox. Third-party untrusted plugins потребуют отдельного WASM process/plugin
RFC либо стабильного C ABI и capability protocol. Этот механизм нельзя
маскировать как свойство текущего registry.

## Plugin distribution

Первый стабильный путь — compile-time Cargo crate:

1. plugin зависит от `yuyib-assets` либо curated `yuyib` facade;
2. реализует `AssetImporter<NeutralType>`;
3. application явно регистрирует его при bootstrap;
4. shipping feature graph включает только нужные importer crates.

Global inventory/constructor registration не используется: явная регистрация
делает feature ownership, порядок bootstrap и тестовую конфигурацию видимыми.

## Importer/cooker boundary

Importer разбирает внешний формат и выдаёт neutral asset. Cooker нормализует и
оптимизирует его для runtime. `AssetServer::try_import_bytes` сейчас публикует
результат importer-а непосредственно, что подходит neutral values и dev tools.
Shipping content pipeline следующим слоем будет передавать `ImportResult<T>` в
typed cooker и сохранять artifact manifest/content hash.

## Compatibility

`ImporterDescriptor::id` — стабильный lowercase identifier. `version` меняется,
когда меняется результат импорта либо его interpretation; вместе с source hash
он станет частью cooked cache key. Extension и media type — hints, а не замена
структурной проверки magic/version внутри `probe` и `import`.
