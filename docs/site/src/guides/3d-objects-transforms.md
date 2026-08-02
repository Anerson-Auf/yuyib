# 3D-объекты: положение, поворот, размер и параметры

> **Статус:** Experimental  
> **Модули:** `yuyib::game_3d`, `yuyib::model`, `yuyib::render_3d`  
> **Уровень:** high-level ECS + low-level matrix escape hatch  
> **Поисковые слова:** объект, модель, размер, scale, material, visibility,
> render order, skinned root

В Yuyib «объект» не является одним большим mutable классом. Это ECS entity с
небольшими typed-компонентами. Геометрия хранится один раз в `Assets<Model>`, а
каждый экземпляр задаёт ссылку `Model3d` и transform. Поэтому уменьшение одного
экземпляра не копирует вершины и не изменяет остальные экземпляры модели.

## Минимальный объект и уменьшение

```rust,no_run
use yuyib::prelude::*;

# fn setup() -> Result<(), Box<dyn std::error::Error>> {
let mut models = Assets::new();
let cube = models.insert(Model::cube(0.5)?);
let mut world = World::new();

world.spawn((
    Model3d::new(cube),
    Transform3d::default()
        .with_translation([2.0, 1.0, -4.0])
        .with_uniform_scale(0.5),
));
# Ok(())
# }
```

`[0.5, 0.5, 0.5]` уменьшает объект вдвое по каждой оси. Для uniform scale
всегда используйте одинаковые три значения. `[1.0, 0.5, 2.0]` — допустимый
non-uniform scale: ширина останется прежней, высота уменьшится вдвое, глубина
увеличится вдвое.

## Параметры renderable entity

### `Transform3d`

| Поле | Формат | Значение |
|---|---|---|
| `translation` | `[x, y, z]` | World-space положение в engine units |
| `rotation` | `[x, y, z, w]` | Unit quaternion, не Euler degrees |
| `scale` | `[x, y, z]` | Размер по осям; `1.0` — исходный размер |

Builders: `from_translation`, `with_translation`, `with_rotation`,
`with_scale`, `with_uniform_scale`. `Transform3d` применяется к standalone
gameplay entity.

### `Model3d`

| Поле | Значение |
|---|---|
| `model` | Typed `ModelHandle` в `Assets<Model>` |
| `mesh` | `None` рисует всю модель; `Some(index)` — один source mesh |
| `visible` | Участвует ли entity в render extraction |
| `render_order` | Stable явный порядок; не заменяет depth test |

Builders: `new`, `with_mesh`, `with_visible`, `with_render_order`.

```rust,no_run
# use yuyib::prelude::*;
# fn example(model: ModelHandle) {
let body_only = Model3d::new(model)
    .with_mesh(0)
    .with_visible(true)
    .with_render_order(10);
# let _ = body_only;
# }
```

## Поворот quaternion

`rotation` — quaternion `[x, y, z, w]`, а не `[pitch, yaw, roll]`. Например,
поворот вокруг Y на угол `yaw`:

```rust
use yuyib::game_3d::Transform3d;

let yaw = std::f32::consts::FRAC_PI_2;
let half = yaw * 0.5;
let transform = Transform3d::default().with_rotation([
    0.0,
    half.sin(),
    0.0,
    half.cos(),
]);
# let _ = transform;
```

Quaternion должен быть finite и normalized. Lightweight `Transform3d` не
нормализует его при каждом присваивании; hierarchy propagation проверяет
authoring data и возвращает typed error.

## Плавный поворот персонажа по направлению движения

Не интерполируйте Euler yaw вручную: на переходе `+PI/-PI` модель сделает
почти полный оборот. `LocomotionFacingSmoother` хранит normalized world-XZ
направление, выбирает shortest arc и ограничивает angular speed:

```rust
use yuyib::{character_3d::LocomotionFacingSmoother, physics::Vec2};

let mut facing = LocomotionFacingSmoother::new(
    Vec2::new(0.0, -1.0),
    std::f32::consts::TAU * 1.25,
)?;

let direction = facing.update(Vec2::new(1.0, 0.0), delta_seconds)?;
```

Скорость `TAU * 1.25` равна 450 градусам в секунду. Clip selection и facing
намеренно разделены: `LocomotionController8` выбирает 8-way animation в
camera-relative space, а smoother поворачивает rendered model в world space.
Это позволяет независимо менять animation policy и ощущение управления.

## Объекты в hierarchy и импортированный glTF

У дочернего объекта используется `LocalTransform3d`: его translation,
rotation и scale считаются относительно parent. После изменения вызовите
`propagate_world_transforms`; derived `WorldTransform3d` вручную не меняют.

```rust,ignore
let root = spawned.roots()[0];
let mut local = world
    .get_mut::<LocalTransform3d>(root)
    .ok_or("root is not authored as TRS")?;
local.scale = [0.75, 0.75, 0.75];
drop(local);
propagate_world_transforms(&mut world)?;
```

glTF node может хранить exact matrix вместо TRS. Такой entity содержит
`LocalMatrixTransform3d`, а не `LocalTransform3d`. Не добавляйте оба компонента:
это `ConflictingLocalTransforms`. Чтобы uniform-уменьшить existing column-major
matrix и сохранить translation, умножьте только первые три basis columns:

```rust
fn uniformly_scaled_matrix(mut matrix: [f32; 16], factor: f32) -> [f32; 16] {
    for column in 0..3 {
        for row in 0..3 {
            matrix[column * 4 + row] *= factor;
        }
    }
    matrix
}

let matrix = uniformly_scaled_matrix([
    1.0, 0.0, 0.0, 0.0,
    0.0, 1.0, 0.0, 0.0,
    0.0, 0.0, 1.0, 0.0,
    2.0, 1.0, -4.0, 1.0,
], 0.5);
assert_eq!([matrix[12], matrix[13], matrix[14]], [2.0, 1.0, -4.0]);
```

Это low-level path. High-level gameplay-коду предпочтительнее TRS-компоненты.

## Root transform у animated/skinned модели

Skeletal renderer принимает дополнительный `root_transform`. Он размещает
полностью sampled character в мире, не изменяя skeleton/asset. Uniform scale
также задаётся тремя basis columns, а translation остаётся положением ног:

```rust
fn character_root(scale: f32, feet: [f32; 3]) -> [f32; 16] {
    [
        scale, 0.0, 0.0, 0.0,
        0.0, scale, 0.0, 0.0,
        0.0, 0.0, scale, 0.0,
        feet[0], feet[1], feet[2], 1.0,
    ]
}
```

В `cyberpunk_city_playable` используется `CHARACTER_MODEL_SCALE = 0.65`.
Одна и та же root matrix применяется к draw и animated head socket, поэтому
модель уменьшается вместе с camera focus и не отрывается от controller feet.
Physics radius настраивается отдельно (`CHARACTER_CONTROLLER_RADIUS = 0.28`),
чтобы уменьшенная модель не сталкивалась с объектами невидимой большой сферой.

## Параметры материала

`Material` принадлежит CPU `Model`, а не `Model3d` entity. Основные параметры:

- `base_color_factor` и optional base-colour texture;
- `metallic_factor`, `roughness_factor` и optional combined texture;
- optional normal texture и её scale;
- `emissive_factor` и optional emissive texture;
- `double_sided`;
- `AlphaMode::{Opaque, Mask, Blend}`;
- optional preserved specular-glossiness workflow;
- у texture binding есть номер UV set.

Изменение shared `Material` меняет все экземпляры этого `Model`. Для
instance-specific материала нужен отдельный model/material asset либо
low-level renderer с собственным material binding.

## Исправление потерянных материалов после импорта

Некоторые экспортёры оставляют primitive на пустом fallback material, хотя по
именам и структуре asset видно, что binding был потерян. Не исправляйте это
скрытием mesh или эвристикой внутри renderer. До первого GPU upload loaded
scene разрешает валидированные metadata edits:

```rust,ignore
use yuyib::model::{Material, MaterialFactorPatch, ModelMaterialPolicy};

let policy = ModelMaterialPolicy::new()
    .patch_named(
        "material_0",
        MaterialFactorPatch::new()
            .with_base_color_factor([0.04, 0.06, 0.10, 1.0])
            .with_metallic_roughness(0.05, 0.82)
            .with_double_sided(true),
    )
    .remap_named_meshes_to_named(
        ["Object_21", "Object_24", "Object_25"],
        "advertising_screens_texture_01",
    )
    .add_and_remap_named_meshes(
        Material::new()
            .with_name("project.recovered_neon")
            .with_base_color_factor([0.9, 0.02, 0.1, 1.0])
            .with_emissive_factor([2.5, 0.05, 0.15])
            .with_double_sided(true),
        ["Object_62"],
    );

let loaded = GltfSceneLoad::start(path, GltfSceneLoadConfig::default().with_material_policy(policy))?;
// After take_ready():
println!("{}", loaded.diagnostics_summary());
```

Runnable smoke (no external fixtures):

```text
cargo run -p yuyib --example gltf_material_policy
cargo run -p yuyib --example gltf_material_usage
cargo run -p yuyib --example gltf_unbound_material_fallback
cargo run -p yuyib --example gltf_texture_diagnostics
```

`ModelMaterialPolicy` is the reusable post-import boundary: it patches named
materials, remaps mesh primitives by **stable mesh names**
(`remap_named_meshes_to_named` / `add_and_remap_named_meshes`), remaps **all
users of a named material** (`add_and_remap_users_of_named` /
`remap_users_of_named`), and can assign an explicit unbound-primitive fallback.
Index-based `remap_meshes_to_named` remains as a low-level escape hatch.
Inspect bindings with
`LoadedGltfScene::material_usage_summary()` / `texture_usage_summary()`
before/after apply. Low-level
`add_material` / `replace_material` / `set_primitive_material` remain available
through `model_mut_before_publication` for one-off edits. After
`prepare_for_frame` starts, CPU material edits return a typed lifecycle error.

In `cyberpunk_city.glb` eight source meshes (`Object_21` … `Object_67`) are
bound to empty single-sided `material_0`. The playable example supplies an
explicit `ModelMaterialPolicy` profile on `GltfSceneLoadConfig` using those
mesh names — not renderer heuristics or hard-coded mesh indices. Lost textures
cannot be recovered from a file that does not contain them; importer
diagnostics report factor-only, unbound, unused, and missing-UV
materials/textures.

## Limits & Caveats

- Scale `0.0` недопустим для hierarchy и lit rendering: normal basis становится
  non-invertible. Для скрытия используйте `Model3d::with_visible(false)`.
- Negative scale зеркалит winding. Standard renderer выбирает mirrored
  rasterization variant, но custom shader/render pass обязан учесть это сам.
- После изменения transform статической карты ранее построенный collider не
  обновляется автоматически. Пересоберите `StaticSceneCollider3d`.
- Visual scale персонажа не меняет radius/height physics controller. Эти
  параметры согласуются отдельно, иначе модель и столкновения будут разных
  размеров.
- Frustum bounds используют transform entity, но после изменения самой CPU
  geometry нужно пересчитать model bounds.
- Не редактируйте миллионы positions ради обычного resize. Transform дешевле,
  сохраняет shared asset и работает с LOD/culling/hierarchy.

API: [`yuyib_game_3d`](../api/yuyib_game_3d/index.html),
[`yuyib_model`](../api/yuyib_model/index.html),
[`yuyib_render_3d`](../api/yuyib_render_3d/index.html).

Подробный lifecycle hierarchy, способ масштабировать все roots загруженной
сцены и правила пересборки bounds/collider описаны в
[3D-трансформациях](3d-transforms.md).
