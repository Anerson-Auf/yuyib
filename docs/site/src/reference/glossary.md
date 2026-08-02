# Термины и стиль Wiki

> **Статус:** Active terminology policy

Основной язык Wiki — русский. Идентификаторы Rust API, общепринятые technical
terms и точные protocol/format names сохраняются на английском. Текст не должен
переключаться между языками внутри одной фразы без необходимости.

| Пишем | Значение | Не используем без пояснения |
|---|---|---|
| кадр (`frame`) | один цикл update/render | frame как обычное русское слово |
| игровой мир (`World`) | ECS container | world, если речь не об identifier |
| сущность (`Entity`) | ECS identity | entity без первого определения |
| компонент (`Component`) | typed ECS data | component в prose |
| ресурс (`Resource`) | singleton ECS data | resource, если это не Rust trait |
| дескриптор (`AssetId<T>`, handle) | typed asset identity | handle без указания типа |
| трансформация (`transform`) | position/rotation/scale | transform без пояснения |
| масштаб (`scale`) | множитель размера по осям | size, когда API принимает scale |
| ограничивающий объём (`bounds`) | spatial extent для culling/camera | bounds без первого определения |
| коллайдер (`collider`) | форма collision | collision object |
| основной поток (`main thread`) | host/event-loop thread | UI thread, если контракт иной |
| фоновая задача (`background task`) | работа вне main thread | async как синоним concurrency |

## Правила для API-страниц

- Identifier всегда пишется точно: `LocalTransform3d`, а не
  «local-transform».
- При первом упоминании термина даётся русский смысл и English search keyword.
- Code comments в примерах пишутся по-русски; имена variables остаются
  idiomatic Rust.
- `Experimental`, `Stable`, `Planned`, `Research` и `Deprecated` не
  переводятся: это фиксированные статусы документации.
- Builder, callback, facade, snapshot, worker, budget и fallback допустимы
  только там, где русский перевод ухудшает точность; первое употребление
  должно объяснять роль.

Это правило относится к Wiki prose. Rustdoc может оставаться англоязычным,
поскольку его identifiers, intra-doc links и crate-level reference рассчитаны
на Rust ecosystem tooling.

