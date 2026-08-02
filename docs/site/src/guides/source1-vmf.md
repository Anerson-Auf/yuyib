# Source 1 / Hammer: VMF brushes

> **Статус:** Experimental  
> **Модули:** `yuyib::vmf`, `yuyib::source1`, `yuyib::source1_scene`, `yuyib::vmf_model`, `yuyib::vmt`, `yuyib::vtf`, `yuyib::source1_assets`

Первая Source 1 pipeline intentionally разделена на format boundary и geometry
boundary. `yuyib::vmf` читает текстовый VMF безопасно и сохраняет неизвестные
KeyValues blocks; `yuyib::vmf_model` компилирует уже нормализованные convex
brush planes в renderer-neutral `Model`. Это не Source 2 и не BSP runtime.

```text
Hammer VMF text -> bounded VmfMap -> normalized BrushSolid -> Model -> renderer
```

## Read a VMF document

```rust,no_run
use yuyib::vmf::{parse_with_limits, VmfLimits};

let source = std::fs::read_to_string("maps/tutorial.vmf")?;
let map = parse_with_limits(&source, VmfLimits::default())?;
let world = map.world().expect("a world block");
println!("world has {} solids", world.solids().len());
# Ok::<(), Box<dyn std::error::Error>>(())
```

`VmfMap` exposes `world`, document-ordered `entities` and unrelated top-level
blocks. `VmfEntity`, `VmfSolid` and `VmfSide` give typed access to common
Source 1 fields, while `VmfBlock`/`VmfProperty` retain source-order unknown
data. This is important for tools: an importer need not silently discard
editor metadata just because the current runtime does not consume it.

`VmfLimits` bounds input bytes, tokens, nesting, individual string bytes,
blocks and properties. `VmfParseError` reports a line/column and a stable
`VmfParseErrorKind`/`VmfLimit`, so untrusted or accidentally malformed maps
fail as data errors rather than allocating without a policy.

## Compile normalized convex brushes

`BrushSolid` and `BrushSide` are deliberately parser-independent. This lets a
tool convert VMF side strings once, validate authoring policy, then compile
with explicit `BrushCompileLimits`.

```rust,no_run
use yuyib::vmf_model::{
    BrushSide, BrushSolid, PlanePoints, compile_solid,
};

let side = BrushSide::new(
    PlanePoints::new([0.0, 0.0, 0.0], [128.0, 0.0, 0.0], [0.0, 128.0, 0.0]),
    "brick/brickwall001a",
);
let _model = compile_solid(&BrushSolid::new(Some("demo".into()), vec![side]))?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

A valid solid needs a closed non-degenerate convex volume, not a single side
as in the abbreviated snippet. The compiler intersects every plane triple,
deduplicates points, builds deterministic face windings and fan-triangulates
them. Every emitted primitive preserves its VMF side material string in
`Material::name`; it is a VMT identifier, **not** an automatically loaded
texture.

Source coordinates are converted explicitly from `[x, y, z]` to
`[x, z, -y]`: Source `Z` becomes Yuyib up and handedness/winding is preserved.

## Complete VMF brush path

`yuyib::source1` is the thin bounded bridge between those two stages. It
strictly parses every VMF `plane` field as exactly three finite parenthesized
point tuples and then delegates all convex geometry compilation to
`yuyib::vmf_model`.

```rust,no_run
use yuyib::{
    source1::{MapBrushSelection, Source1AdapterLimits, compile_map},
    vmf::parse,
    vmf_model::BrushCompileLimits,
};

let source = std::fs::read_to_string("maps/tutorial.vmf")?;
let map = parse(&source)?;
let model = compile_map(
    &map,
    &MapBrushSelection::WorldAndEntities,
    Source1AdapterLimits::default(),
    BrushCompileLimits::default(),
)?;
assert!(!model.meshes().is_empty());
# Ok::<(), Box<dyn std::error::Error>>(())
```

`MapBrushSelection` makes world/entity selection explicit. The output order is
always world solids first, then entity solids in source order; explicit entity
indices are normalized to ascending order and duplicate/out-of-range requests
are errors. `adapt_map` exposes selected normalized brushes and their
`VmfBrushOrigin` for editor diagnostics without forcing a GPU dependency.

## Entity metadata to ECS

`spawn_entities` turns selected VMF world/entity KeyValues into ECS metadata,
not into magic gameplay. `Source1Entity` retains `classname`, Hammer `id` and
every KeyValue in document order, including unknown/repeated keys. A valid
`origin` is converted with the same `[x, y, z] -> [x, z, -y]` rule and creates
`LocalTransform3d`; the existing propagation publishes world/render snapshots.

```rust,no_run
use yuyib::{
    ecs::prelude::World,
    source1_scene::{Source1SpawnOptions, spawn_entities},
    vmf::parse,
};

let map = parse(&std::fs::read_to_string("maps/tutorial.vmf")?)?;
let mut world = World::new();
let report = spawn_entities(&mut world, &map, Source1SpawnOptions::default())?;
assert!(!report.spawned().is_empty());
# Ok::<(), Box<dyn std::error::Error>>(())
```

`Source1OriginPolicy::MetadataOnly` is the default: missing/invalid origin
still makes metadata but never invents `[0, 0, 0]`. Use `RequireValid` when an
import must pre-validate the full selection before creating any entity.

## VMT metadata (without texture decoding)

`yuyib::vmt` parses a bounded Source 1 material text file and offers a typed
`VmtMaterial` view: shader, optional `$basetexture`/`$bumpmap`, strict
`$translucent` and `$alphatest` flags, `$surfaceprop`, plus unknown ordered
KeyValues blocks for tools. Use `parse_with_vmt_limits` at untrusted file
boundaries. The `$basetexture` string is only a VTF identifier: it does not
open a file, decode a texture or create a GPU material.

`yuyib::vtf` supplies the first separate binary decode boundary:
`decode_with_limits` reads **only** verified little-endian Source 1 VTF 7.2,
one 2D frame/depth-1 texture with RGBA8888 or BGRA8888 high-resolution data,
and returns RGBA8 `VtfImage`. It validates the complete smallest-to-largest
mip layout before returning the base image.

## Limits & Caveats

- Accepted text grammar is Source 1 quoted KeyValues blocks, nested braces and
  `//` comments outside strings. No Source 2 VMAP/VPK, compiled BSP, block
  comments, unquoted properties or arbitrary escapes.
- Only finite convex brushes are geometry. VMT metadata and a bounded VTF 7.2
  RGBA/BGRA subset are separate asset layers; brush compilation still has no
  texture-axis UV, lightmap, displacement, prop binding, visibility/PVS or
  collision runtime.
- `BrushCompileLimits` bounds solids, sides, generated vertices/face vertices
  and triangle indices. Compile errors are structured; do not ignore a
  degeneracy error and render a partial map.
- `compile_map` makes only geometry. It does not materialize VMF entities into
  ECS or interpret non-brush map logic. `source1_scene` materializes only
  KeyValue metadata/transforms: no brush/prop model binding, hierarchy
  inference, output routing or automatic gameplay mapping.
- VMT is Source 1 text metadata only. `yuyib-source1-assets` can resolve a safe
  local `$basetexture` path and invoke the bounded VTF decoder, but there is no
  VPK lookup, VMT `patch`/include behavior, automatic GPU/material binding or
  Source 2 material format. `$translucent` and `$alphatest` accept only exact
  `0`/`1` values.
- VTF decode is intentionally not a generic Source texture loader: it rejects
  VTF 7.0/7.1/7.3, DXT/compressed data, thumbnails, cubemaps, frames, VPK and
  filesystem loading. A host uploads returned RGBA8 explicitly through its
  chosen Yuyib texture API.

Full API: [VMF reader](../api/yuyib_vmf/index.html) and
[brush compiler](../api/yuyib_vmf_model/index.html) and
[Source 1 adapter](../api/yuyib_source1/index.html) and
[Source 1 ECS metadata](../api/yuyib_source1_scene/index.html) and
[VMT metadata](../api/yuyib_vmt/index.html) and
[VTF decoder](../api/yuyib_vtf/index.html) and
[Source 1 asset resolver](../api/yuyib_source1_assets/index.html).
