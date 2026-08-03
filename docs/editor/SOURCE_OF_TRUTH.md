# Source of Truth (Editor / Play / Code)

Нормативный контракт поверх [`ENGINE_INTEGRATION.md`](ENGINE_INTEGRATION.md).
Перед 2D-вариацией и любыми behavior scripts — читать этот файл.

## Один control plane

| Слой | Роль | Мутации |
|---|---|---|
| **`.yscene`** | Persistence SoT (entities, components, GUIDs) | Inspector, commands, Intent Bridge (Editor), Apply Play whitelist |
| **Intent Bridge** (`SceneInteractionIntent`) | Script / gameplay surface по `EntityGuid` | Editor → undoable transaction; Play → runtime ECS + signals |
| **Projection** (`src/scenes/.../*.rs`) | Human-editable **view** over `.yscene` | Sync/Apply/watch → снова commands в document; **не** SoT |
| **Play ECS `World`** | Ephemeral runtime | Не пишется обратно, кроме explicit Apply whitelist |

```text
behavior / script ──Intent──▶ Editor document  OR  Play World
                                    │
                                    ▼
                              .yscene (SoT)
                                    │
                          projection view (optional)
```

## Запрещено (anti-patterns)

1. **Второй SoT** в freeform Rust / 2D scripts, которые пишут сцену в обход commands/intents.
2. **Whole-World serialize** или silent Play→document merge.
3. **`Model3d.visible=false` как nocollide** — ломает независимость render/collision (см. ниже).
4. CharacterController ↔ Rapier **mode switch** как prerequisite 2D.
5. Shadow / render intents — **deferred**; не блокируют 2D.
   Project scaffold + Create Project exist; multi-step wizard UX is incremental.

## Render vs collision (3D Play)

| Authored | Meaning |
|---|---|
| `yuyib.render3d.draw=false` (**nodraw**) | Не попадает в render extract; collision **не** трогает |
| `yuyib.model3d.visible=false` | То же для draw (legacy alias); collision **не** трогает |
| `yuyib.collision3d.enabled=false` (**nocollide**) | Исключается из static player mesh collider |
| `yuyib.collision3d.collide_with` | Если непустой список — в player mesh только при наличии `"player"` |

Triggers / Interactable — отдельные sphere queries; не зависят от mesh collider.

Prop↔prop selective collision → Rapier overlay (`yuyib-play --features physics-rapier`;
authored prop markers still open for full selective layers).

## Game loop checklist (3D)

См. [`GAME_LOOP_3D.md`](GAME_LOOP_3D.md) + golden scene `editor_tests/prj2`.

## 2D entry rule

Тот же SoT + Intent Bridge. Новые 2D schemas materialize в Play/profile;
не invent parallel script bus.
