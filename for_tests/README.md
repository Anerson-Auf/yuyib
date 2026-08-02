# Integration fixtures

Локальные `.glb` / `.yasset` **не коммитятся** в Git (см. корневой
`.gitignore`). Нужны только на машине разработчика для examples / Editor /
smokes. Перед распространением ассетов сверяйте лицензию источника.

| Fixture (local) | Назначение |
|---|---|
| `cyber_samurai.glb` | Skeletal animation / character preview |
| `velina_zzz.glb` | Character import, materials, interaction |
| `no_i_am_not_a_human_location__map.glb` | Scene import, camera/light, LOD |
| `street_city_7_for_games_free.glb` | Playable / M1 smoke map |
| `outdoor_probe.hdr` | Tiny IBL equirect для street-city (в репо) |

Аудит одного локального GLB:

```powershell
cargo run -p xtask -- gltf-fixtures for_tests/velina_zzz.glb
```

Default `cargo test` эти файлы не требует.
