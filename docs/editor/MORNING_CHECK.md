# Morning check (3D basis closed → 2D next)

## What shipped overnight

1. **SoT contract** — [`docs/editor/SOURCE_OF_TRUTH.md`](SOURCE_OF_TRUTH.md)
2. **nodraw / nocollide** — schemas `yuyib.render3d` / `yuyib.collision3d`; Play + Editor materialize; collision mesh **independent** of draw
3. **Game loop checklist** — [`docs/editor/GAME_LOOP_3D.md`](GAME_LOOP_3D.md)
4. **prj2 golden** — TalkNpc, ExitVolume, **NoDrawSolid**, **GhostProp**
5. **QuestBook smoke** — auto on Play; E on TalkNpc → quest transition
6. Explicitly **not** done (deferred): Rapier default Play, shadow intents, LSP completion, wizard, Play AddComponent model/parent

## Rebuild before Play

```text
cargo build -p yuyib-play
cargo build -p yuyib-editor
```

Restart Editor. Open `editor_tests/prj2`, reload scene from disk.

## What to verify

Bottom panel **Diagnostics**, filter/look for `source=play` (not mock Imported rows).

| Check | How | Pass |
|---|---|---|
| Boot logs | Start Play | `materialized Interactable`, `Trigger`, `nodraw`/`nocollide`, `QuestBook smoke ready` |
| Use | Walk to TalkNpc (~8,19.5), **E** | `use accepted` + `world.talk_npc` + `quest transition` |
| Trigger | Enter ExitVolume (~4.5,19.5) | `signal trigger id=level.exit phase=entered` |
| Nodraw | Go to (~10,19.5) NoDrawSolid | Invisible cube but **blocks** walk |
| Nocollide | Walk through GhostProp (~2,19.5) | Visible cube, **no** block |
| Inspector | Add Component | Render 3D (nodraw), Collision 3D (nocollide) in list |

## Then

2D variation can start on the same SoT + Intent Bridge (see SOURCE_OF_TRUTH «2D entry rule»).
