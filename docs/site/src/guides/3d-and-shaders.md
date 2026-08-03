# 3D: scenes, materials и shaders

> **Статус:** Experimental  
> **Назначение:** карта выбора «какой material/shader path взять»

Yuyib не прячет один «магический Material». Есть несколько **явных** уровней.
Выбирайте самый высокий, который закрывает задачу.

## Карта решений

| Задача | Берите | Guide |
|---|---|---|
| Playable glTF карта + PBR/IBL/shadows | `Game3dScene` + `GltfSceneLoad` | [Game3dScene](game-3d-scene.md), [tutorial glTF](../tutorials/load-gltf-scene.md) |
| Standard glTF materials / shading enum | `Game3dShading`, `StandardMaterial3d` | [standard-material](standard-material-and-scenes.md) |
| Простой lit mesh без полного PBR | Lambert path | [lit-materials](lit-materials.md) |
| Только albedo texture | `TexturedMaterial3d` | [textured-materials](textured-materials.md) |
| Procedural / custom mesh | `Model` / `MeshPrimitive` + upload | [model-assets](model-assets.md), [3d-renderer](3d-renderer.md) |
| Post: exposure, bloom, FXAA, grade | `ColorPostProcess` | [hdr-post-processing](hdr-post-processing.md) |
| Свой pass / WGSL | `RenderGraph`, `ShaderProgram` | [custom-render-passes](custom-render-passes.md) |

## Почему нет одного «EffectMaterial» preset API

RFC 0003 описывает Tier 1 effect presets как цель. Сейчас usable path — 
composition через `Game3dScene` / post-process config, а не скрытый node
shader editor. ROADMAP помечает «Shader API not as planned» для high-level
effect templates: не ждите Unreal-like material graph в foundation.

## Materials vs import diagnostics

glTF часто приходит с unbound / factor-only / missing UV materials.
Исправление — **`ModelMaterialPolicy`** на load, не тихий renderer hack.
Diagnostics остаются на `LoadedGltfScene::diagnostics()`.

## Shaders

Low-level: `ShaderSource` / `ShaderProgram` / `ShaderPrototype` в
`yuyib::shader`. High-level scenes уже содержат нужные WGSL pipelines; custom
pass добавляется в render graph **после** понимания phase order
(opaque → transparent → post → UI).

## См. также

- [Architecture](../concepts/architecture.md)
- [Limits](../reference/limits-and-compatibility.md)
- [Custom render passes](custom-render-passes.md)
