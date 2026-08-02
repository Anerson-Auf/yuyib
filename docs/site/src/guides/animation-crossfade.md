# 3D animation: плавный переход между clips

`AnimationCrossFadeMixer` решает одну runtime-задачу: переключает imported glTF
animation clips без скачка pose. Он renderer-neutral — результат остаётся обычным
`AnimationSnapshot`, который уже понимает skeletal renderer.

Полный fixture-free пример:
[`animation_crossfade.rs`](../../../../crates/yuyib/examples/animation_crossfade.rs).

```powershell
cargo run -p yuyib --example animation_crossfade
```

Пример не открывает окно и не требует `.glb`: три коротких clips находятся во
встроенном glTF document. Поэтому на нём удобно проверить state transitions до
подключения собственной модели.

## High-level flow

```rust
use yuyib::gltf::{
    AnimationClipIndex, AnimationCrossFadeDuration, AnimationCrossFadeMixer,
};

let idle = AnimationClipIndex::new(0);
let walk = AnimationClipIndex::new(1);
let mut mixer = AnimationCrossFadeMixer::new(&asset.scene, idle)?;

let fade = AnimationCrossFadeDuration::new(0.15)?;
mixer.transition_to(&asset.scene, walk, fade)?;

// Вызывается один раз на simulation/render frame.
let pose = mixer.advance_and_snapshot(&asset.scene, delta_seconds)?;
renderer.set_animation_snapshot(pose);
# Ok::<(), Box<dyn std::error::Error>>(())
```

Индекс относится к `asset.scene.animations()`, а не к имени файла. Для проекта
лучше один раз сопоставить имена clips с typed indices при загрузке и затем
хранить эти indices в gameplay state.

## Duration и состояние перехода

`AnimationCrossFadeDuration::new(seconds)` принимает только конечное значение
из `0.0..=10.0`. Это не декоративная проверка: она не позволяет ошибочному
config/network value удерживать source pose неограниченно долго.

- `0.0` или `AnimationCrossFadeDuration::IMMEDIATE` — немедленное переключение;
- `transition_progress()` — значение `0.0..=1.0`; стабильный mixer возвращает `1.0`;
- `is_transitioning()` — активен ли non-immediate fade;
- `active_clip()` — последний полностью активированный clip;
- `target_clip()` — clip, к которому сейчас идёт переход.

`transition_to` возвращает явный результат:

| Result | Значение |
|---|---|
| `Unchanged` | clip уже active или уже является target |
| `Started` | начат обычный transition |
| `Retargeted` | незавершённый transition перенаправлен |
| `CompletedImmediately` | target стал active без blending |

## Retarget без pose pop

Gameplay может быстро менять направление: `idle -> walk_forward`, а через два
frames — `strafe_left`. Нельзя начинать второй fade снова из idle: персонаж
визуально дёрнется назад.

Mixer сохраняет последний видимый blended pose и использует его как source для
нового target:

```rust
mixer.transition_to(&scene, walk, fade)?;
let _visible_pose = mixer.advance_and_snapshot(&scene, 0.08)?;

let result = mixer.transition_to(&scene, strafe_left, fade)?;
assert_eq!(result, AnimationCrossFadeChange::Retargeted);
```

Это делает API пригодным для locomotion state machine и 8-way animation. Сам
mixer не выбирает gameplay state: правила `speed -> idle/walk/run` остаются в
character controller или animation graph.

## Pose output

`snapshot` и `advance_and_snapshot` возвращают borrowed `&AnimationSnapshot`:

- `local_transforms()` — TRS каждого source node;
- `world_matrices()` — уже разрешённая hierarchy;
- `skin_palettes()` — joint matrices для skinned mesh instances;
- `morph_weights(node)` — blended morph target weights.

Translation, scale и morph weights смешиваются линейно. Rotation использует
нормализованную интерполяцию quaternion по shortest path. После blending mixer
заново разрешает hierarchy и skin palettes; renderer не должен повторно смешивать
кости.

Ссылка действует до следующего mutable вызова mixer. Если pose нужен дольше
одного frame, его можно клонировать осознанно; обычный render path должен
потребить borrowed snapshot сразу, чтобы не делать лишнюю allocation.

## Pause, speed и ошибки

- `pause()` замораживает clip time и progress перехода;
- `play()` продолжает оба;
- `set_speed()` задаёт одинаковую validated speed active и target players;
- invalid clip, negative/NaN delta и несовместимые snapshots возвращаются как
  `AnimationCrossFadeError`, а не дают частично обновлённую pose.

Matrix-authored nodes можно смешать только если matrices совпадают. Для
анимируемых объектов экспортируйте TRS channels: произвольное linear blending
матриц создаёт shear и non-invertible transforms.

## Low-level blending

Если state machine принадлежит приложению, используйте
`blend_animation_snapshots(scene, source, target, factor)`. Контракт factor —
finite `0.0..=1.0`; snapshots должны принадлежать одной scene и иметь одинаковую
структуру TRS/morph data. Это низкоуровневый primitive, а не replacement для
`AnimationCrossFadeMixer`: playback time, pause, retarget и cached pose тогда
обслуживает приложение.
