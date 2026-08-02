//! Shared playable map profile for `cyberpunk_city_playable` and M1 smoke.
//!
//! `MAP_FILE`, collision selectors, material policy and spawn options are one
//! profile: swap the GLB only together with those knobs. Hard-required named
//! material patches for a previous pack brick load on asset change — use
//! optional quirks or an empty policy for a fresh map.

use std::{
    error::Error,
    path::{Path, PathBuf},
    time::Duration,
};

use yuyib::{
    assets::ImporterRegistryLimits,
    character_3d::{CharacterSpawnOptions3d, CharacterSpawnSurfaceSelection3d},
    model::ModelMaterialPolicy,
    physics::Vec2,
    profile_3d::EnvironmentPreset,
    render_3d::{
        Game3dScene, GltfSceneColliderLayer3d, GltfSceneColliderLayerId3d,
        GltfSceneCollisionConfig3d, GltfSceneCollisionConfigError3d, GltfSceneCollisionNameMatch3d,
        GltfSceneCollisionPredicate3d, GltfSceneCollisionSelector3d, GltfSceneLoad,
        GltfSceneLoadConfig, GltfSceneLoadStage, LambertLighting3d, LoadedGltfScene,
        PreparedEquirectEnvironment3d,
    },
};

pub use yuyib::profile_3d::OUTDOOR_PROBE_HDR;

/// Active playable map under `for_tests/`.
pub const MAP_FILE: &str = "sci-fi_lab.glb";
/// Trusted upper bound for the playable map GLB source size.
pub const TRUSTED_CITY_SOURCE_BYTES: usize = 128 * 1024 * 1024;
/// Preferred walkable elevation for spawn selection (lab floor ≈ origin).
pub const CITY_STREET_ELEVATION: f32 = 0.0;
/// Horizontal search radius around the walkable-layer centroid.
pub const CITY_STREET_SEARCH_RADIUS: f32 = 28.0;
/// Indoor maps keep a short clearance; outdoor packs can raise this.
pub const CITY_OPEN_SKY_CLEARANCE: f32 = 2.5;
/// Full-geometry collision layer id.
pub const CITY_SOLID_COLLIDER_LAYER: &str = "solid";
/// Walkable street/floor collision layer id.
pub const CITY_STREET_COLLIDER_LAYER: &str = "street";
/// Floor materials in `sci-fi_lab.glb` (meshes are generic `Object_*`).
pub const LAB_FLOOR_MATERIAL_NAMES: &[&str] =
    &["Podoga2", "SCIANA_PODLOGA", "rura_glowna_podloga"];

/// Returns the repository `for_tests` directory used by playable examples.
#[must_use]
pub fn asset_root() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../for_tests"))
}

/// Absolute path to the playable map GLB fixture.
#[must_use]
pub fn map_path(asset_root: &Path) -> PathBuf {
    asset_root.join(MAP_FILE)
}

/// Solid collider layer id.
///
/// # Errors
///
/// Forwards empty-id rejection from the collision API.
pub fn solid_layer_id() -> Result<GltfSceneColliderLayerId3d, GltfSceneCollisionConfigError3d> {
    GltfSceneColliderLayerId3d::new(CITY_SOLID_COLLIDER_LAYER)
}

/// Street collider layer id.
///
/// # Errors
///
/// Forwards empty-id rejection from the collision API.
pub fn street_layer_id() -> Result<GltfSceneColliderLayerId3d, GltfSceneCollisionConfigError3d> {
    GltfSceneColliderLayerId3d::new(CITY_STREET_COLLIDER_LAYER)
}

/// Semantic solid + walkable collision layers for the active map profile.
///
/// # Errors
///
/// Returns a collision config error for invalid layer ids or selectors.
pub fn collision_config() -> Result<GltfSceneCollisionConfig3d, GltfSceneCollisionConfigError3d> {
    let mut floor_predicates = Vec::with_capacity(LAB_FLOOR_MATERIAL_NAMES.len());
    for name in LAB_FLOOR_MATERIAL_NAMES {
        floor_predicates.push(GltfSceneCollisionPredicate3d::MaterialName(
            GltfSceneCollisionNameMatch3d::exact(*name)?,
        ));
    }
    GltfSceneCollisionConfig3d::new([
        GltfSceneColliderLayer3d::new(
            solid_layer_id()?,
            GltfSceneCollisionSelector3d::all_geometry(),
        ),
        GltfSceneColliderLayer3d::new(
            street_layer_id()?,
            GltfSceneCollisionSelector3d::any(floor_predicates),
        ),
    ])
}

/// Material policy for the active map.
///
/// `sci-fi_lab.glb` needs no named factor patches. Keep this empty (or use
/// [`ModelMaterialPolicy::patch_named_optional`] for quirks) so a MAP_FILE swap
/// does not hard-fail on Blender slot names from another pack.
#[must_use]
pub fn material_policy() -> ModelMaterialPolicy {
    ModelMaterialPolicy::new()
}

/// High-level load config shared by playable and smoke.
///
/// Enables the M3 disk cook cache under `asset_root/.yuyib_cook` so a second
/// load of an unchanged map GLB skips glTF parse.
///
/// # Errors
///
/// Forwards semantic collision config failures.
pub fn load_config(
    asset_root: &Path,
) -> Result<GltfSceneLoadConfig, GltfSceneCollisionConfigError3d> {
    let city_import_limits = ImporterRegistryLimits {
        max_source_bytes: TRUSTED_CITY_SOURCE_BYTES,
        ..ImporterRegistryLimits::default()
    };
    Ok(GltfSceneLoadConfig::default()
        .with_importer_registry_limits(city_import_limits)
        .with_static_collider(false)
        .with_semantic_collision(collision_config()?)
        .with_material_policy(material_policy())
        .with_cook_cache(yuyib::assets::CookCache::new(asset_root.join(".yuyib_cook"))))
}

/// Walkable spawn options for the active map profile.
///
/// Prefer the horizontal centroid of the semantic street/floor layer so the
/// player lands on a deck near walkable geometry, not on an arbitrary triangle
/// near world origin.
#[must_use]
pub fn spawn_options() -> CharacterSpawnOptions3d {
    spawn_options_near(Vec2::ZERO)
}

/// Spawn options anchored at a caller-chosen horizontal district.
#[must_use]
pub fn spawn_options_near(preferred_xz: Vec2) -> CharacterSpawnOptions3d {
    CharacterSpawnOptions3d::outdoor_lowest(preferred_xz)
        .with_maximum_horizontal_distance(CITY_STREET_SEARCH_RADIUS)
        .with_minimum_open_sky_clearance(CITY_OPEN_SKY_CLEARANCE)
        .with_surface_selection(CharacterSpawnSurfaceSelection3d::ClosestToElevation(
            CITY_STREET_ELEVATION,
        ))
}

/// Horizontal centroid of a street collider mesh (XZ), used as spawn anchor.
#[must_use]
pub fn street_horizontal_centroid(street: &yuyib::physics::TriangleMesh3d) -> Vec2 {
    let mut sum = Vec2::ZERO;
    let mut count = 0.0_f32;
    for face in street.triangles() {
        let centroid = (face[0] + face[1] + face[2]) * (1.0 / 3.0);
        sum.x += centroid.x;
        sum.y += centroid.z;
        count += 1.0;
    }
    if count <= 0.0 {
        return Vec2::ZERO;
    }
    sum * count.recip()
}

/// Spawn policy anchored at the street-layer centroid.
#[must_use]
pub fn spawn_options_for_street(street: &yuyib::physics::TriangleMesh3d) -> CharacterSpawnOptions3d {
    spawn_options_near(street_horizontal_centroid(street))
}

/// Daytime map renderer: overhead key + tiny fill + cooked outdoor IBL/sky + shadows.
///
/// Delegates to [`EnvironmentPreset::street_city`] so the engine owns the look;
/// this helper remains for M1 smoke anti-drift.
///
/// # Errors
///
/// Returns lighting, GGX cook, or scene construction failures.
pub fn create_renderer(asset_root: &Path) -> Result<Game3dScene, Box<dyn Error>> {
    Ok(EnvironmentPreset::street_city()?.build_scene(asset_root)?)
}

/// Loads `outdoor_probe.hdr` when present; otherwise the synthetic sky/ground probe.
///
/// Prefer [`EnvironmentPreset`] for new code. Kept for GGX smoke / diagnostics.
///
/// # Errors
///
/// Returns decode failures only when the fixture exists but is invalid. A missing
/// file falls back to [`PreparedEquirectEnvironment3d::synthetic_outdoor_probe`].
pub fn load_outdoor_equirect(
    asset_root: &Path,
) -> Result<PreparedEquirectEnvironment3d, Box<dyn Error>> {
    let path = asset_root.join(OUTDOOR_PROBE_HDR);
    match std::fs::read(&path) {
        Ok(bytes) => {
            let env = PreparedEquirectEnvironment3d::from_radiance_hdr_bytes(&bytes)?;
            eprintln!(
                "street_city: loaded Radiance HDR probe {} ({}x{})",
                path.display(),
                env.width(),
                env.height()
            );
            Ok(env)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!(
                "street_city: missing {}; using synthetic outdoor probe",
                path.display()
            );
            Ok(PreparedEquirectEnvironment3d::synthetic_outdoor_probe()?)
        }
        Err(error) => Err(error.into()),
    }
}

/// Flat view-independent exposure for the playable character.
///
/// Directional shading is intentionally deferred: orbiting a fixed pose must
/// not change whole-avatar brightness, and unlit cloth morphs previously read
/// brighter than lit skin. This RGB multiplies every skinned and morph draw.
///
/// # Errors
///
/// Returns lighting validation failures.
pub fn character_key_light() -> Result<LambertLighting3d, Box<dyn Error>> {
    // Zero direct term: shader uses ambient + radiance as a flat multiplier.
    Ok(LambertLighting3d::artistic(
        [-0.35, -1.0, -0.25],
        [1.0, 0.97, 0.92],
        0.0,
        [0.78, 0.78, 0.80],
    )?)
}

/// Blocks until the high-level city load reaches Ready and returns the scene.
///
/// Headless smoke uses this helper; the interactive playable path polls
/// asynchronously on the frame loop instead.
///
/// # Errors
///
/// Returns load failure messages or a timeout when the worker never finishes.
#[allow(
    dead_code,
    reason = "Shared with street_city_m1_smoke; playable polls asynchronously."
)]
pub fn wait_for_loaded_map(
    loading: &mut GltfSceneLoad,
    timeout: Duration,
) -> Result<LoadedGltfScene, Box<dyn Error>> {
    let started = std::time::Instant::now();
    loop {
        match loading.update().stage {
            GltfSceneLoadStage::Ready => match loading.take_ready() {
                Ok(scene) => return Ok(scene),
                Err(error) => {
                    return Err(format!("map Ready but take_ready failed: {error}").into());
                }
            },
            GltfSceneLoadStage::Failed => {
                return Err(loading.failure().map_or_else(
                    || "map load failed".to_owned(),
                    |error| error.to_string(),
                )
                .into());
            }
            GltfSceneLoadStage::Queued
            | GltfSceneLoadStage::Reading
            | GltfSceneLoadStage::Processing => {
                if started.elapsed() > timeout {
                    return Err(format!(
                        "timed out after {timeout:?} waiting for map Ready (last={:?})",
                        loading.update().stage
                    )
                    .into());
                }
                std::thread::sleep(Duration::from_millis(16));
            }
            GltfSceneLoadStage::Taken => {
                return Err("map Ready payload was already taken".into());
            }
        }
    }
}
