# RFC 0008 — high-level 2D и 3D scene facades

- **Статус:** accepted
- **Дата:** 2026-08-01
- **Зависит от:** RFC 0002, RFC 0003, RFC 0006, RFC 0007

## Проблема

Корректные low/mid-level crates не образовывали короткий application path.
Examples вручную связывали extraction, camera viewport, texture/model cache и
renderer calls. Это нарушало исходную цель: prototype не должен требовать
переписывания внутреннего render lifecycle.

## Решение

Вводятся два небольших stateful facade без глобального singleton:

- `Game2dScene`: camera culling, bounded decoded queue/GPU residency,
  sprite+tile extraction, painter-safe batching и degradation diagnostics;
- `Game3dScene`: hierarchy propagation, bounded visible model snapshot,
  camera/light policy, renderer-neutral frustum filtering и persistent
  standard model/texture/bounds cache.

Оба facade получают `World` и assets явно. Они не владеют event loop, не читают
network input и не прячут ошибки. Low-level renderers/extractors остаются
публичными escape hatches и могут быть вызваны из отдельных `RenderGraph` phases.

## Осознанные ограничения

`Game2dScene` принимает уже decoded image: I/O/decode должны выполняться через
worker/`AssetServer`. Обычный `Game3dScene::render` сохраняет удобный eager
cache-miss path для малых сцен, а streamed worlds обязаны заранее выполнять
CPU `ModelTextureLoader::prepare` в worker и bounded GPU publication по
texture slots. LOD extraction сохраняет `Model3d::mesh` без `LodGroup3d`,
иначе каждый glTF node ошибочно инстанцировал бы всю модель. Standard 3D facade предлагает Unlit,
Lambert и direct-light PBR modes. PBR имеет отдельные factor-only и standard
glTF metallic/roughness pipelines. Второй требует position/normal/tangent и
authored UV set для каждого texture slot, семантически корректно загружает
sRGB base/emissive и linear normal/MR maps и применяет tangent-space normal
mapping. На GPU primitive хранит четыре выбранных material UV-пары вместо всех
восьми source streams. Textured draws группируются в bounded
opaque/transparent batches, уменьшая render-pass churn без нарушения порядка
BLEND. Неполные комбинации texture maps,
alpha phases, IBL и environment lighting пока явно не поддерживаются.

High-level PBR сохраняет strict low-level glTF contract, но по умолчанию
классифицирует exporter-authored `BLEND`, который фактически opaque по
worker-computed alpha summary. Threshold policy конфигурируется и имеет strict
режим; promotion отражается в draw telemetry. Это не замена OIT: настоящий
translucent content остаётся в sorted, non-depth-writing phase.

Frustum planes извлекаются из WGPU 0..1 clip matrix. Model-wide и per-mesh
local AABB рассчитываются на worker для `GltfSceneLoad`; direct scene users
получают lazy transactional cache. Filtering сохраняет deterministic draw
order, поддерживает affine transforms/non-uniform scale/shear и никогда не
отбрасывает draw без bounds. Counters возвращаются вместе со scene stats.
