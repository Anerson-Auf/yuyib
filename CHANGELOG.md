# Changelog

Формат близок к [Keep a Changelog](https://keepachangelog.com/).
Проект на стадии Experimental: breaking changes допустимы до первого stable
minor. Подробный статус milestones — в
[`docs/architecture/ROADMAP.md`](docs/architecture/ROADMAP.md).

## [Unreleased]

### Added

- Корневые GitHub-материалы: `README`, `CONTRIBUTING`, `KNOWN_ISSUES`, dual
  license (`LICENSE-MIT` / `LICENSE-APACHE`).
- Документация Editor status (`docs/editor/ENGINE_HANDOFF.md`) приведена к
  публичному maintainer-формату.

### Changed

- Синхронизированы публичные статусы wiki (IBL / shadows / post-process /
  cook) с закрытыми M1–M3 milestones.
- Сжаты session-notes в roadmap; убраны agent-handoff формулировки из
  editor/wiki docs.

## [0.1.0] — 2026-08-02

### Added

- Foundation runtime: Application / Game, ECS facade, WGPU render graph.
- 2D sprites / atlas / tilemap foundation.
- 3D glTF import, PBR usable MVP (IBL, shadows, bloom, FXAA, SSAO, grade).
- Cook cache для glTF imported assets.
- Editor foundation: project/scene, hierarchy, Inspector, gizmo, Play runner.
- Optional Native UI, WebView2, audio, networking, Source 1 VMF slice.
