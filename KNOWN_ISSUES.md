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

- Full Asset DoD для glTF preview не закрыт: collision / tangents / UV
  overlays и animation selection ещё открыты (Bounds/Normals/mesh/material —
  есть).
- rust-analyzer / LSP sidecar не подключён; Monaco — UI без semantic
  diagnostics.
- Apply Play Mode Changes (reverse sync runtime → authored) выключен
  намеренно до whitelist adapters.
- Coverage gate есть в scoped tests; foundation GitHub Actions —
  `.github/workflows/` (не полный workspace gate).
- Component field mutation без typed adapter блокируется host-ом.

## Native UI / WebView

- Nested clipping, scrollbar/inertia, virtualization, IME и accessibility —
  открыты (есть bounded `ScrollView` vertical).
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
