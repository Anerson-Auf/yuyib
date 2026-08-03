# Morning check (3D basis closed → 2D next)

## What shipped overnight

1. **SoT contract** — [`docs/editor/SOURCE_OF_TRUTH.md`](SOURCE_OF_TRUTH.md)
2. **nodraw / nocollide** — schemas `yuyib.render3d` / `yuyib.collision3d`; Play + Editor materialize; collision mesh **independent** of draw
3. **Game loop checklist** — [`docs/editor/GAME_LOOP_3D.md`](GAME_LOOP_3D.md)
4. **prj2 golden** — TalkNpc, ExitVolume, **NoDrawSolid**, **GhostProp**
5. **QuestBook smoke** — auto on Play; E on TalkNpc → quest transition
6. Explicitly **not** done (deferred): shadow intents, cooked-only binary strip
   (Play Rapier overlay — closed via `yuyib-play --features physics-rapier`;
   Asset Preview + Play cook-hit via `.yuyib_cook` — closed;
   LSP command-only / `executeCommand` — closed allowlisted `rust-analyzer.*`)
7. **E1 smoke project** — [`editor_tests/prj5-e1-cook-lsp`](../../editor_tests/prj5-e1-cook-lsp)
   (cook / ypack / preview store / Play cook-hit / LSP checklist)

## Rebuild before Play

```text
cargo build -p yuyib-play
cargo build -p yuyib-play --features physics-rapier
cargo build -p yuyib-editor
```

Restart Editor. For Rapier props smoke open `editor_tests/prj4-rapier` (see its README).
For interact/nodraw golden use `editor_tests/prj2`.
For E1 cook / preview store / ypack / LSP smoke use `editor_tests/prj5-e1-cook-lsp`.

Indoor spawn: Play no longer prefers a high downward cast that lands on roof
tops above an authored indoor Player marker.

## What to verify

### prj2 — game loop / SoT

Bottom panel **Diagnostics**, filter/look for `source=play` (not mock Imported rows).

| Check | How | Pass |
|---|---|---|
| Boot logs | Start Play | `materialized Interactable`, `Trigger`, `nodraw`/`nocollide`, `QuestBook smoke ready` |
| Use | Walk to TalkNpc (~8,19.5), **E** | `use accepted` + `world.talk_npc` + `quest transition` |
| Trigger | Enter ExitVolume (~4.5,19.5) | `signal trigger id=level.exit phase=entered` |
| Nodraw | Go to (~10,19.5) NoDrawSolid | Invisible cube but **blocks** walk |
| Nocollide | Walk through GhostProp (~2,19.5) | Visible cube, **no** block |
| Inspector | Add Component | Render 3D (nodraw), Collision 3D (nocollide) in list |

### prj5 — E1 cook / preview / LSP

Full steps also in [`editor_tests/prj5-e1-cook-lsp/README.md`](../../editor_tests/prj5-e1-cook-lsp/README.md).

| Check | How | Pass |
|---|---|---|
| Cook | Toolbar **Cook assets** | Output: cook miss then hit for `alpha`/`bravo` |
| ypack | Export → wipe `.yuyib_cook` → Import | Hydrate restores cook cache |
| Preview cook-hit | Asset Preview on `alpha.glb` after cook/hydrate | Toast / `host.process` `cook_hit` |
| Preview store | Preview alpha → bravo → alpha | Second alpha: stage `cache_hit` (no full re-decode) |
| Play cook-hit | Play Main after cook | stderr `glTF cook hit|miss` for props |
| LSP | Open `src/demo_lsp.rs` | Diagnostics / completion / hover / **signature help** / Definition / References / rename / lightbulb; command-only = `rust-analyzer.*` only |
| Animation clip | Asset Preview on animated glTF (skeletal_preview policy) | Inspector Animation list; switch clip / Bind pose; viewport moves |
| Inspector | Select AlphaProp / BravoProp | Model3d tracked `asset://` refs |

## Then

2D variation can start on the same SoT + Intent Bridge (see SOURCE_OF_TRUTH «2D entry rule»).
