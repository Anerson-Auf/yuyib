# Editor capability coverage

Capability coverage отвечает на вопрос: «как пользователь обнаруживает и
использует каждую curated возможность Yuyib?». Наличие runtime type или example
само по себе не означает Editor coverage.

> **Текущий implementation status (2026-08-02):** `AuthoringRegistry` и
> deterministic JSON manifest реализованы в `yuyib-authoring`; bootstrap в
> `yuyib-authoring-yuyib` отдаётся host-ом как `host.coverage`.
> **Visual (closed playable slice):** `Transform3d`, `LocalTransform3d`,
> `Parent3d`, `Model3d`, `DirectionalLight3d` — Inspector + materialize +
> viewport/Play evidence. **Asset (incremental):** `yuyib.gltf-import` /
> `yuyib.gltf-preview` — `GltfPreviewAdapter` over production `GltfSceneLoad`
> + settings mapping + non-destructive reimport + **Bounds/Normals** overlays + preview
> **cache key/invalidation** evidence + **mesh selection**; material/animation
> selection and remaining overlays still open (not full Asset DoD). **Coverage gate (incremental):**
> `validate_coverage_gate` + `AssetCoverageEvidence` enforced in scoped tests;
> canonical `CoverageManifest::to_pretty_json` + golden fixture in
> `yuyib-gltf-authoring`; GitHub Actions still open. Этот policy-документ не
> второй source of truth — machine snapshot важнее.

## Допустимые статусы

| Статус | Обязательный contract |
|---|---|
| `Visual` | Inspector/palette control, commands, validation, persistence/materialization и preview при применимости |
| `Asset` | Versioned import settings, importer/cooker path, preview, diagnostics, cache/invalidation |
| `Runtime` | Управление/наблюдение в Play Mode; отсутствие persisted scene state объяснено |
| `CodeOnly` | Docs, diagnostics и source navigation доступны; visual abstraction сознательно отсутствует |
| `Unavailable` | Capability ещё не подключена; есть причина/ограничение и tracking milestone |

У одной capability может быть несколько surfaces, например component — `Visual`,
а связанный simulation system — `Runtime`. Machine record хранит surface entries
явно; одно строковое значение не должно скрывать частичную поддержку.

`Unavailable` лучше молчаливого отсутствия, но не закрывает coverage gate для
milestone, который требует capability. Публичная curated capability вообще без
record является hard CI error.

## Machine-readable registry

Source of truth — compile-time declarations authoring adapters, собранные в
`AuthoringRegistry`. `coverage_manifest()` уже выдаёт deterministic JSON для
Editor palette; CI artifact/diff и documentation generator должны потреблять
тот же snapshot. Целевой расширенный record:

```json
{
  "capability_id": "yuyib.transform3d",
  "owner": "yuyib-game-3d",
  "schema": { "id": "yuyib.transform3d", "version": 2 },
  "surfaces": [
    {
      "kind": "Visual",
      "entry": "inspector.transform3d",
      "preview": "viewport.transform3d",
      "docs": "docs/site/src/guides/3d-transforms.md"
    }
  ],
  "materializer": "yuyib.transform3d.materialize",
  "apply_play_whitelist": ["translation", "rotation", "scale"],
  "tests": ["transform3d_scene_round_trip", "transform3d_inspector_command"]
}
```

Exact serialization может измениться до реализации, но следующие поля
обязательны по смыслу:

- unique stable capability ID;
- owning crate/plugin и Cargo feature;
- surface status и Editor entry point;
- optional persisted schema ID/version/migrations;
- preview/materialization adapters;
- Apply Play whitelist или явное отсутствие;
- docs и source-navigation targets;
- verification/test evidence;
- для `Unavailable` — reason и target milestone.

Текущий registry уже делает hard error для duplicate IDs, dangling capability/
component references, пустых или противоречивых surfaces и preview adapter-а у
`Unavailable` capability. Следующий evidence gate обязан дополнительно
отклонять:

- duplicate capability, schema, importer-settings, plugin или system stable ID;
- curated runtime capability без coverage record;
- `Visual` без command/validation mapping;
- persisted `Visual` без schema/materializer;
- `Asset` без settings schema, preview или diagnostics;
- migration gap для поддерживаемой persisted version;
- dangling docs/source/adapter reference;
- Apply Play property, не объявленной authored;
- Editor-only dependency в shipping/headless feature graph.

Human-readable documentation и Editor palette генерируются из тех же records.
Ручная параллельная таблица не является source of truth.

## Текущий bootstrap baseline

Следующая таблица поясняет machine snapshot после реализации Editor foundation.
Изменять status следует только вместе с реальным adapter registration и tests,
не редактированием этой таблицы.

| Capability group | Runtime foundation | Initial Editor status | Требование первого slice |
|---|---|---|---|
| Project/Application/Game profile | Есть частично | `Unavailable` | Project creation/open и profile diagnostics |
| glTF source/import settings | Есть importer foundation | `Unavailable` | `Asset`: bounded import/reimport, settings, diagnostics |
| Image/texture assets | Есть decode/assets foundation | `Unavailable` | `Asset`: channels/color-space/size preview |
| Mesh/material/animation subresources | Есть 3D foundation | `Unavailable` | `Asset`: selection, clip playback, material assignment |
| 3D transform/hierarchy | Есть ECS foundation | `Visual` | hierarchy, Inspector, gizmo, round-trip — **closed** |
| Camera/light/model instance | Есть частично | Model/Light `Visual`; Camera follow-up | Light transform/cone/Play — **closed** |
| Collision/bounds/normals/tangents/UV | Есть разные runtime paths | Bounds+Normals partial; rest `Unavailable` | Bounds AABB + normals shafts in Asset Preview; collision/tangents/UV open |
| 3D PBR/render presets | Есть частично | partial Scene/Play PBR parity | Preview Asset route still open |
| Scene persistence/materialization | `.yscene` foundation | partial | GUID, schemas, opaque preservation — **closed** for TRS/Model/Light |
| Play Mode | Game lifecycle есть | process-isolated `yuyib-play` | Player motor + mesh physics + lights — **closed**; Apply Play off |
| System/source navigation | ECS schedules есть | `Unavailable` | `SystemDescriptor`, read/write search, plugin ownership |
| Code workspace | WebView foundation есть | `Unavailable` | Mature editor component + rust-analyzer/LSP |
| 2D authoring | Runtime foundation partial | `Unavailable` | Следующий slice после 3D Asset coverage |
| Custom low-level Rust/WGPU | Есть escape hatches | `CodeOnly` target | Docs/navigation, без фиктивного visual control |

`CodeOnly target` в последней строке означает design classification, а не
утверждение, что code-navigation UI уже реализован. До его появления operational
status остаётся `Unavailable` в machine report.

## Definition of coverage

### Visual

Capability закрыта как `Visual`, только если:

- control обнаруживается через palette/Inspector;
- add/edit/remove идут через atomic commands;
- invalid value не попадает silently в document/runtime;
- persisted data проходит save/load/save и migrations;
- materialization изменяет реальный runtime component;
- unknown/newer records не теряются;
- docs, diagnostics и source navigation доступны.

### Asset

Capability закрыта как `Asset`, только если:

- asset имеет GUID, отдельный от content hash/path;
- import settings имеют stable schema/version/migrations;
- preview использует production importer/cooker/renderer, не duplicate decoder;
- load/reimport cancellable, bounded и показывает progress;
- dependencies, cache, invalidation и RAM/VRAM budgets наблюдаемы;
- import failure non-destructive;
- subresources/materials/clips и overlays доступны там, где применимо.

### Runtime

Capability закрыта как `Runtime`, только если Play runner позволяет наблюдать или
управлять ею через documented protocol, runner crash изолирован, а transient
state не возвращается в authored scene автоматически.

### CodeOnly

`CodeOnly` не означает «спрятано в wiki». Нужны:

- searchable palette/docs entry;
- open owning crate/plugin/bootstrap;
- component/system read/write navigation, если применимо;
- template или minimal example;
- scoped check action и diagnostics.

## Review policy

PR/change, добавляющий curated capability, должен включать machine record в том
же increment. Review проверяет не только наличие статуса, но и полноту evidence:
hard-coded empty Inspector не является `Visual`, а alternate decoder не является
`Asset` preview.

Coverage regression допустим только как явное breaking change с migration или
tracking issue. Удаление adapter-а без сохранения persisted schema reader/opaque
path запрещено.

После первого vertical slice generated summary должен как минимум показывать:

- общее число curated capabilities;
- counts по пяти статусам и surfaces;
- missing/duplicate/invalid records;
- schema versions и migration gaps;
- capabilities без preview/materializer/tests/docs;
- изменения относительно базовой revision.
