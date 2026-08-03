# Yuyib Wiki

Yuyib — native-first Rust runtime для Windows applications, 2D/3D games и
optional WebView surfaces. Native UI — стандартный UI path; WebView — только
там, где нужен HTML/CSS.

> **Foundation / Experimental.** Wiki описывает public contract текущего
> workspace. Planned / Research возможности так и помечены.

## С чего начать

1. **[Начало работы](getting-started.md)** — dependency, `clear_window`, минимальный `Application`.
2. **[Учебный путь](tutorials/learning-path.md)** — tutorials «что / почему / что возвращает».
3. **[Что вы хотите сделать?](wiki/use-case-index.md)** — task → guide, если знаете цель.
4. **[Примеры](reference/examples.md)** + **[features](reference/features.md)** — runnable evidence.

## Разделы sidebar

| Раздел | Содержание |
|---|---|
| Учебные tutorials | Окно → Game → glTF → 2D playable |
| Основы | Ownership, ECS, assets, limits |
| Application и UI | Shell, native UI, WebView |
| Game и ECS | Schedules, tasks, input, scene ECS |
| 2D | Game2dScene, sprites, tiles, interaction |
| 3D: сцены | Game3dScene, glTF stream, transforms, camera |
| 3D: materials | Import, PBR/Lambert, post-process, VMF |
| Assets / Physics / Audio | Отдельные capability tracks |
| Справочник | Subsystems, rustdoc map, troubleshooting |

Guides объясняют **задачу и production pattern**. Reference и rustdoc —
точные signatures. Не копируйте snippets вместо canonical example в
`crates/yuyib/examples/`.

## Реализовано в foundation

| Подсистема | Статус |
|---|---|
| Runtime, frame clock, frame events | Experimental |
| ECS facade (`bevy_ecs`) | Experimental |
| Windows window (`winit`) + WGPU surface | Experimental |
| High-level `Application` / `Game` | Experimental |
| Typed assets, importers, cook cache (glTF) | Experimental |
| 2D sprites, tiles, HL `Game2dScene`, Tiled/LDtk | Experimental |
| 3D PBR/IBL/shadows/post (usable MVP), `GltfSceneLoad` | Experimental |
| Rapier 2D/3D facade, character controllers | Experimental |
| Native UI + WebView2 overlay | Experimental |
| Source 1 VMF/VMT/VTF boundary | Experimental |
| Source 2 | Research |

Открытые gaps: device-loss recovery, полный Editor 2D authoring, validated CSM /
TAA / GTAO, shipping without importers. См.
[limits](reference/limits-and-compatibility.md) и
[roadmap](../../architecture/ROADMAP.md).
