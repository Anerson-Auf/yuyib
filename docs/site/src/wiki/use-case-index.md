# Что вы хотите сделать?

> **Статус:** Current task index  
> **Назначение:** найти рабочий API по задаче, не зная устройство crates

Начинайте отсюда, если знаете желаемый результат, но не название type или
module. Ссылки ведут на guide с lifecycle, полным примером и ограничениями.
Точные signatures — в [API Reference](../reference/api-reference.md).

Если вы **новичок** и хотите идти по шагам с объяснением «почему эта
функция» — сначала [учебный путь](../tutorials/learning-path.md).

## Проект и цикл приложения

| Я хочу… | Открыть | Основной API |
|---|---|---|
| Пройти обучение с нуля | [Учебный путь](../tutorials/learning-path.md) | tutorials 1–4 |
| Создать native window и очистить frame | [Tutorial окно](../tutorials/first-window.md), [Application](../guides/application.md) | `Application`, `WindowConfig`, `ClearColor` |
| Создать игру с ECS world | [Tutorial Game](../tutorials/first-game.md), [Game lifecycle](../guides/game-lifecycle.md) | `Game`, `GamePlugin`, `GameSchedule` |
| Выполнять fixed-step simulation | [Game lifecycle](../guides/game-lifecycle.md) | `FixedUpdateConfig`, `FixedTime` |
| Выполнить CPU-задачу в фоне | [Tasks](../guides/tasks.md) | `TaskPool`, `Task<T>` |
| Показать native UI | [Native UI](../guides/native-ui.md) | `ApplicationUi`, `UiBuilder`, `UiTokens` |
| Встроить локальную HTML-страницу | [WebView](../guides/webview-windows.md) | `ApplicationWebView` |

## 3D-мир и модели

| Я хочу… | Открыть | Основной API |
|---|---|---|
| **Изменить размер, позицию или поворот модели** | **[3D object cookbook](../guides/3d-objects-transforms.md)**, [hierarchy и collision](../guides/3d-transforms.md) | `Transform3d`, `LocalTransform3d` |
| Загрузить glTF/GLB без остановки окна | [Tutorial glTF](../tutorials/load-gltf-scene.md), [Streamed glTF](../guides/streamed-gltf-scene.md) | `GltfSceneLoad`, `LoadedGltfScene` |
| Импортировать glTF вручную | [glTF import](../guides/gltf-import.md) | `ImportOptions`, `import_scene_path` |
| Создать procedural mesh/model | [Model assets](../guides/model-assets.md) | `Model`, `Mesh`, `MeshPrimitive` |
| Показать сцену стандартным renderer | [Game3dScene](../guides/game-3d-scene.md) | `Game3dScene`, `Game3dSceneConfig` |
| Выбрать Lambert или PBR material path | [Standard materials](../guides/standard-material-and-scenes.md) | `Game3dShading`, `StandardMaterial3d` |
| Управлять камерой | [Free camera](../guides/free-camera.md) | `FreeCameraController3d`, `Camera3d` |
| Построить hierarchy parent/child | [Scene ECS](../guides/scene-ecs-and-interactions.md) | `Parent3d`, `set_parent_3d` |
| Добавить static level collision | [Scene ECS](../guides/scene-ecs-and-interactions.md#статические-стены-карты) | `build_static_scene_collider_3d` |
| Создать playable character | [Input, character and quests](../guides/input-character-quests.md) | `CharacterController3d`, `KeyboardActionMap` |
| Загрузить карту Hammer/Source 1 | [Source 1 VMF](../guides/source1-vmf.md) | `compile_vmf_model`, source adapters |
| Добавить custom render pass/shader | [Low-level renderer](../guides/custom-render-passes.md) | `RenderGraph`, `ShaderProgram` |

## 2D

| Я хочу… | Открыть | Основной API |
|---|---|---|
| Собрать первый 2D playable | [Tutorial 2D](../tutorials/first-2d-playable.md) | `Game2dScene`, `Sprite2d`, `PlayableLoop2d` |
| Нарисовать sprite | [Sprites and animation](../guides/sprites-and-animation.md) | `Sprite2d`, `TextureRegion` |
| Создать sprite sheet/atlas | [ECS sprite atlas](../guides/ecs-sprite-atlas.md) | `SpriteSheet`, `AnimatedSprite2d` |
| Проиграть animation | [ECS animation](../guides/ecs-sprite-animation.md) | `AnimatedSprite2d`, `step_sprite_animations_2d` |
| Отсечь sprites вне камеры | [Sprite viewport culling](../guides/sprite-viewport-culling.md) | `SpriteViewport2d` |
| Построить tilemap / Tiled | [Tilemaps](../guides/tilemaps.md) | `TileMap2d`, `yuyib::tiled` |
| Добавить collision/controller к tilemap | [Tilemap kinematic physics](../guides/tilemap-kinematic-physics.md) | `KinematicSpriteController2d` |
| Обработать click/touch по entity | [2D interaction](../guides/interaction-2d.md) | `request_pointer_interaction_2d` |

## Assets, I/O и сервисы

| Я хочу… | Открыть | Основной API |
|---|---|---|
| Хранить typed assets | [Assets](../guides/assets.md) | `Assets<T>`, `AssetId<T>` |
| Загружать assets в фоне | [Asset loading](../guides/asset-loading.md) | `AssetServer`, `AssetLoadQueue` |
| Написать свой importer | [Custom importers](../guides/custom-importers.md) | `AssetImporter`, `ImporterRegistry` |
| Проиграть audio | [Audio](../guides/audio.md) | `AudioEngine`, `AudioClip` |
| Открыть bounded TCP connection | [Networking](../guides/networking.md) | `TcpServer`, `JsonConnection` |

## Если API не найден

1. Ищите действие или type через полнотекстовый поиск Wiki.
2. Откройте [карточку subsystem](../reference/subsystems.md), чтобы увидеть
   ownership, lifecycle и все связанные guides.
3. Перейдите в embedded [`rustdoc`](../api/yuyib/index.html) для полного списка
   public types, traits, functions, methods, constants и errors.
4. Проверьте [Limits & Compatibility](../reference/limits-and-compatibility.md):
   возможность может быть Planned, а не реализованной.
5. Для уже работающего, но неверно ведущего себя API используйте
   [Troubleshooting](../reference/troubleshooting.md).
