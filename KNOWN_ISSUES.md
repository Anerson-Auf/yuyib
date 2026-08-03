# Known issues

Актуально на 2026-08-02. Список фиксирует публично заметные ограничения и
открытые дефекты foundation. Детальный roadmap — в
[`docs/architecture/ROADMAP.md`](docs/architecture/ROADMAP.md).

## Платформа и API

- Поддерживается только **Windows**. Другие ОС не являются verified targets.
- Public API помечен как **Experimental**: breaking changes возможны до первого
  stable minor.

## Renderer / 3D

- Device-loss recovery и полный rebuild GPU resources не реализованы.
- Shadow cascades (CSM): API на 2 cascade есть, playable path использует
  одну cascade; validated multi-cascade и skinned casters — открыты.
- TAA / SMAA, GTAO, cooked 3D LUT, true GPU mesh instancing и GPU timestamp
  queries отложены (см. M2 remainder / v2.0 в roadmap).
- Array-texture / compressed / HDR mip paths и sampler dedupe — частично.

## Physics

- General-purpose dynamic rigid-body solver отсутствует. Сейчас: static mesh /
  BVH queries и kinematic character. Mature backend facade — milestone **M4**.

## Editor

- Full Asset DoD для glTF preview не закрыт (timeline / crossfade / scene
  assignment). Preview overlays (Bounds/Collision/Normals/Tangents/UV) и
  mesh/material/animation clip selection — closed thin.
- LSP: diagnostics / completion / hover / **signature help** / definition /
  references / rename / code actions / allowlisted `executeCommand`
  (`rust-analyzer.*` only); broader command surface open.
- Apply Play Mode Changes closed for Transform3d + LocalTransform3d whitelist.
- Coverage gate: Asset evidence, Visual→schema (+ runtime source/fields),
  capability docs/source, system source, migration `1→current`,
  import-settings→Asset, Apply Play⇒Visual — enforced in scoped tests +
  foundation Actions. Still open outside registry: materializer/command maps,
  FS dangling paths, Cargo editor-dep lint.
- Component field mutation без typed adapter: host `read_only` + Inspector tip.

## Native UI / WebView

- Nested clipping stack, scroll inertia, virtualization, IME и accessibility —
  открыты (есть bounded `ScrollView`: wheel + thumb drag/track jump + image
  extract; GPU textured UI pass ещё нет).
- WebView — native overlay, не GPU texture; composition hosting не в scope.

## Assets / cook

- Third-party `.glb` / `.gltf` / `.yasset` не хранятся в Git (локальные
  fixtures в `for_tests/` и `editor_tests/*/assets/`).
- Shipping без source importers (cooked-only feature) — post-core.
- Hot-reload UI / file watchers и полный reverse dependency graph — частично /
  deferred.
- Source 2 — research only.

## Документация

- Собранный `docs/site/book/` может отставать от `docs/site/src` до явной
  пересборки wiki.
- Отдельные wiki-страницы ещё синхронизируются с закрытыми M2 capabilities
  (проверяйте ROADMAP и этот файл при сомнении).

## Как сообщать

При issue указывайте: OS/GPU, example или scene, ожидаемое vs фактическое
поведение, минимальный repro и версию workspace (`0.1.0`).
