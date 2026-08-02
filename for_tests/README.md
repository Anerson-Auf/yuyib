# Integration fixtures

These user-provided `.glb` files are opt-in integration fixtures, not unit-test
inputs. They intentionally remain outside the default `cargo test --workspace`
path so a normal verification run stays fast and does not depend on large
binary assets.

| Fixture | Intended future example |
|---|---|
| `cyber_samurai.glb` | Skeletal animation and transparent character preview. |
| `velina_zzz.glb` | Static/animated character import, material and interaction demo. |
| `no_i_am_not_a_human_location__map.glb` | Scene import, camera/light, LOD/culling and map interaction demo. |
| `outdoor_probe.hdr` | Radiance equirect for street-city / playable IBL (`load_outdoor_equirect`). Tiny 64×32 sky/ground probe; replace with a real capture when available. |

Both files are GLB v2 containers. A future `xtask` integration-fixture audit
reports exactly which currently unsupported glTF features each file uses:

```powershell
cargo run -p xtask -- gltf-fixtures
```

The command never adds these heavy files to default tests and prints one result
per fixture. Embedded image buffers and
`KHR_materials_pbrSpecularGlossiness`, double-sided material metadata and
affine matrix node metadata are supported. The map can be materialized without
lossy matrix decomposition. Character previews support bounded skinning and a
sorted `BLEND` phase; unsupported primitive modes and material workflows must
still be reported rather than silently skipped.

Use an exact path to audit one additional GLB:

```powershell
cargo run -p xtask -- gltf-fixtures for_tests/velina_zzz.glb
```

Future examples must not silently skip unsupported data. Keep source/licensing
information with the files before distributing the repository or a built demo.
