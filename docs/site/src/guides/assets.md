# Assets: typed handles

**Статус:** Experimental foundation  
**Requires:** `yuyib::assets`

`Assets<T>` хранит значения одного type `T`, а `AssetId<T>` является их typed,
copyable handle. Это удобно для ECS components: entity может хранить
`AssetId<Texture>`, но не сможет случайно передать его туда, где ожидается
`AssetId<Mesh>`.

## Insert, read и remove

```rust
use yuyib::assets::Assets;

#[derive(Debug, Eq, PartialEq)]
struct Label(String);

let mut labels = Assets::new();
let welcome = labels.insert(Label("Hello".to_owned()));

assert_eq!(labels.get(welcome), Some(&Label("Hello".to_owned())));

let removed = labels.remove(welcome);
assert_eq!(removed, Some(Label("Hello".to_owned())));
assert_eq!(labels.get(welcome), None);
```

`get_mut` даёт mutable reference, если handle current и asset ещё resident.
После `remove` handle становится stale. Если storage повторно использует его
slot для нового asset, старый `AssetId<T>` всё равно не получит новый value:
generation проверяется при каждом `get`, `get_mut` и `remove`.

## Где использовать

Храните `Assets<T>` в application-owned resource, а `AssetId<T>` — в ECS
components или domain state. Например, future `Sprite` сможет хранить
`AssetId<Image>`, а `MeshRenderer` — `AssetId<Mesh>` и `AssetId<Material>`.
Текущая crate не навязывает ECS dependency, поэтому её можно применять и вне
ECS.

## Что находится в соседних слоях

`AssetServer` предоставляет async CPU preparation, stable loading handles,
failure states и placeholders. `ImporterRegistry<T>` подключает typed source
importers, а `AssetUploadQueue` ограничивает device-bound publication по jobs и
bytes на frame. File/package resolver, disk cooker/cache manifest, dependency
invalidation и hot reload пока не завершены. Texture atlases и sprite animation
принадлежат 2D crates. Не используйте `Assets<T>` как claim, что `T` уже
resident в GPU: само хранилище остаётся CPU in-memory boundary.

## Limits & Caveats

- `AssetId<T>` действителен только для конкретного `Assets<T>` instance. Не
  сериализуйте его как durable cross-process identifier.
- `Assets<T>` не является thread-safe queue и не синхронизирует concurrent
  access; locking/scheduling остаётся ответственностью host.
- Количество slots ограничено `u32::MAX`; при переполнении `insert` panic'ит.
  Это hard limit типа handle, но практически не является нормальным runtime
  limit.
