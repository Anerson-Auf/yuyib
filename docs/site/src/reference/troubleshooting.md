# Troubleshooting

> **Статус:** Current symptom index  
> **Правило:** сначала найдите observable error/status, затем меняйте policy

## Module или type не находится

**Симптом:** `could not find ... in yuyib` или unresolved import.

1. Найдите type в [карте подсистем](subsystems.md).
2. Проверьте требуемый [Cargo feature](features.md).
3. Помните, что `yuyib::prelude` неполный; импортируйте specialised API из
   named module.
4. После изменения features выполните scoped check downstream crate.

Не копируйте type из internal workspace crate только ради обхода facade:
сначала убедитесь, что capability действительно включена.

## Wiki открывается, а API links возвращают 404

Plain `mdbook build docs/site` строит только Wiki и очищает output directory.
Он не публикует Rustdoc. Используйте полный pipeline:

```powershell
cargo run -p xtask -- docs
```

Готовый сайт должен содержать одновременно `docs/site/book/index.html` и
`docs/site/book/api/yuyib/index.html`.

## 3D-модель не видна

Проверьте по порядку:

1. entity имеет `Model3d` и `Transform3d` либо resolved hierarchy transform;
2. `Model3d::visible` не выключен;
3. handle указывает в тот же `Assets<Model>`, который передан scene renderer;
4. camera смотрит на model и она не отсеяна frustum;
5. texture/geometry publication закончена либо stats явно показывают pending;
6. material поддерживается выбранным `Game3dShading` route;
7. для lit route есть light/fallback policy.

Смотрите counters `Game3dSceneStats`: input/culled/visible models, missing
resources, draw calls и render passes быстрее локализуют boundary, чем
случайное отключение culling.

## Размер модели изменился, collision — нет

Render transform и collider не являются одной структурой. Worker-built
`LoadedGltfScene::collider()` — static snapshot. Пересоберите collider после
изменения уровня либо используйте dynamic collider для движущегося object.

Полный рецепт: [3D-трансформации](../guides/3d-transforms.md).

## `ConflictingLocalTransforms` или transform не обновляется

- Entity не может одновременно содержать `LocalTransform3d` и
  `LocalMatrixTransform3d`.
- `WorldTransform3d` derived; меняйте local authoring component.
- После batch изменений вызовите `propagate_world_transforms` один раз.
- Для matrix-authored glTF root создайте новый TRS parent.

## Окно зависает во время загрузки

На event/render thread не должно быть file read, decode, blocking
`TaskPool::spawn` при полной queue или `Task::join`. Используйте
`AssetServer`/`AssetLoadQueue`, non-blocking `try_spawn` и poll ready state на
frame boundary. GPU publication разбивайте per-frame budget.

См. [Asset loading](../guides/asset-loading.md) и
[Background tasks](../guides/tasks.md).

## Texture не загружается у glTF

- Проверьте asset root и URI относительно source model.
- External path должен оставаться внутри canonical asset root после symlink
  resolution.
- Embedded GLB image не требует отдельного файла.
- Encoded source, decoded image и GPU upload имеют разные limits.
- Смотрите typed importer/preparation error; не отключайте path security.

См. [glTF import](../guides/gltf-import.md) и
[Model assets](../guides/model-assets.md).

## Task queue заполнена

`TaskSpawnError::Full` — ожидаемый backpressure, а не transient panic. Варианты:

1. отложить submission до следующего update;
2. объединить мелкие jobs;
3. использовать один application-owned pool;
4. увеличить capacity только после telemetry/profiling.

Не заменяйте `try_spawn` на blocking `spawn` внутри event loop.

## Audio работает локально, но падает в CI

Headless runner может не иметь physical output device. Отделите load/decode
tests от `AudioEngine::open_default`; `AudioOutputError` в device integration
test является нормальным platform failure.

## WebView API отсутствует или не создаётся

- Включите feature `webview`; он не входит в `desktop-full`.
- Current implementation требует Windows/WebView2.
- Host pages должны соответствовать local-page/security contract; raw remote
  browser capabilities намеренно не выдаются.

См. [WebView for Windows](../guides/webview-windows.md).

## TCP connection закрывается на malformed input

Проверьте `FrameLimits`, protocol version и JSON limits обеих сторон.
Oversized/invalid frame отклоняется явно; увеличивать global buffer без
bounded protocol policy нельзя. Yuyib transport не добавляет TLS/auth/ECS
replication автоматически.

## Документация и код расходятся

Canonical signature — текущий generated Rustdoc. Semantic workflow и limits —
Wiki. Если public item существует, но не находится в task/subsystem index, это
documentation bug: обновите rustdoc comment, coverage map, relevant guide и
`SUMMARY.md` по [documentation contract](../wiki/documentation-contract.md).

