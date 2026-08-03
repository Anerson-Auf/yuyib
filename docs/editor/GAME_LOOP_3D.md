# 3D game loop checklist (Editor → Play)

Golden project: [`editor_tests/prj2`](../../editor_tests/prj2).  
SoT: [`SOURCE_OF_TRUTH.md`](SOURCE_OF_TRUTH.md).  
API: [`AUTHORING_GUIDE.md`](AUTHORING_GUIDE.md).

## Preconditions

1. `cargo build -p yuyib-play` (engine workspace).
2. Rebuild/restart **yuyib-editor** (embeds UI).
3. Open `editor_tests/prj2` → reload scene from disk if already open.

## Loop

| Step | Action | Expect in Diagnostics `source=play` |
|---|---|---|
| Boot | Play | `materialized Interactable` / `Trigger` / optional `nodraw`/`nocollide` / `QuestBook smoke ready` |
| Move | WASD near TalkNpc (~8, 19.5) | — |
| Use | **E** | `use accepted → interaction \`world.talk_npc\`` + `signal \`world.talk_npc\`` + `quest transition` |
| Trigger | Walk into ExitVolume (~4.5, 19.5) | `signal trigger id=level.exit phase=entered` (then stayed/exited) |
| Nodraw | Entity with `yuyib.render3d.draw=false` | Invisible mesh; still solid unless nocollide |
| Nocollide | Entity with `yuyib.collision3d.enabled=false` | Walk through; may still render |
| Selective | `collide_with` without `player` | Same as nocollide vs locomotion mesh |

## Authored flags

```json
{ "schema": "yuyib.render3d", "version": 1, "payload": { "draw": false } }
```

```json
{
  "schema": "yuyib.collision3d",
  "version": 1,
  "payload": {
    "enabled": true,
    "layer": "prop",
    "collide_with": "player"
  }
}
```

- `enabled: false` → full nocollide vs player mesh.
- `collide_with: "prop"` (no `player`) → excluded from player mesh.
- Empty `collide_with` + enabled → collide with all (default solid).

## Deferred (do not block 2D)

Shadow intents, cooked-only Play (no source importers),
prop↔prop authored selective layers.
