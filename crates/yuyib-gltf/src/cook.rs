//! Versioned bincode cook-cache support for imported glTF assets.

use std::{error::Error, path::Path};

use crate::{
    ImportError, ImportOptions, ImportPolicy, ImportedAsset, discover_external_dependencies,
    import_scene_bytes_embedded, import_scene_bytes_with_base_path, load_uri_buffer,
};
use yuyib_assets::{
    CookCache, CookError, CookKey, CookManifest, CookedArtifact, CookerIdentity,
    ImportDependencyKind, content_hash_blake3, options_hash_blake3,
};

/// Schema version of the bincode payload stored by this cooker.
pub const GLTF_IMPORTED_COOK_SCHEMA: u32 = 1;
/// Stable identifier for the glTF imported-asset cooker.
pub const GLTF_IMPORTED_COOKER_ID: &str = "yuyib.gltf.imported";

const MISSING_OPTIONAL_TOKEN: &[u8] = b"__yuyib_missing_optional__";

/// Returns the stable identity used for imported glTF cook-cache entries.
#[must_use]
pub fn gltf_imported_cooker_identity() -> CookerIdentity {
    CookerIdentity::new(GLTF_IMPORTED_COOKER_ID, env!("CARGO_PKG_VERSION"))
}

/// Returns a stable textual fingerprint for the complete import configuration.
#[must_use]
pub fn import_options_fingerprint(options: &ImportOptions) -> String {
    let limits = options.limits;
    let policy = match options.policy {
        ImportPolicy::Strict => "strict",
        ImportPolicy::StaticPreview => "static-preview",
        ImportPolicy::Skeletal => "skeletal",
        ImportPolicy::SkeletalPreview => "skeletal-preview",
    };
    format!(
        "policy={policy};max_buffer_bytes={};max_vertices={};max_indices={};\
         max_embedded_image_bytes={};max_skin_joints={};max_animation_keyframes={}",
        limits.max_buffer_bytes,
        limits.max_vertices,
        limits.max_indices,
        limits.max_embedded_image_bytes,
        limits.max_skin_joints,
        limits.max_animation_keyframes,
    )
}

/// Encodes an imported asset with its little-endian schema header.
///
/// # Errors
///
/// Returns [`CookError::Encode`] if bincode cannot encode the asset.
pub fn encode_imported_asset(asset: &ImportedAsset) -> Result<Vec<u8>, CookError> {
    let payload =
        bincode::serialize(asset).map_err(|error| CookError::Encode(error.to_string()))?;
    let mut bytes = Vec::with_capacity(std::mem::size_of::<u32>() + payload.len());
    bytes.extend_from_slice(&GLTF_IMPORTED_COOK_SCHEMA.to_le_bytes());
    bytes.extend_from_slice(&payload);
    Ok(bytes)
}

/// Decodes a schema-prefixed imported-asset cook-cache payload.
///
/// # Errors
///
/// Returns [`CookError::SchemaMismatch`] for another schema, or
/// [`CookError::Decode`] for malformed or incomplete bytes.
pub fn decode_imported_asset(bytes: &[u8]) -> Result<ImportedAsset, CookError> {
    let header = bytes
        .get(..std::mem::size_of::<u32>())
        .ok_or_else(|| CookError::Decode("missing schema header".to_owned()))?;
    let schema = u32::from_le_bytes(
        header
            .try_into()
            .map_err(|_| CookError::Decode("invalid schema header".to_owned()))?,
    );
    if schema != GLTF_IMPORTED_COOK_SCHEMA {
        return Err(CookError::SchemaMismatch {
            found: schema,
            expected: GLTF_IMPORTED_COOK_SCHEMA,
        });
    }
    bincode::deserialize(&bytes[std::mem::size_of::<u32>()..])
        .map_err(|error| CookError::Decode(error.to_string()))
}

/// Builds the deterministic cache key for glTF source bytes and import options.
///
/// External buffer/image files are **not** folded into this key. Path-aware
/// cookers keep the source-only key so an unchanged document can hit without
/// re-parsing, then re-check [`CookManifest::dependency_fingerprints`].
#[must_use]
pub fn cook_key_for_gltf_source(source_bytes: &[u8], options: &ImportOptions) -> CookKey {
    let identity = gltf_imported_cooker_identity();
    CookKey {
        content_hash: content_hash_blake3(source_bytes),
        importer_id: "yuyib.gltf".to_owned(),
        importer_version: env!("CARGO_PKG_VERSION").to_owned(),
        cooker_id: identity.id,
        cooker_version: identity.version,
        options_hash: options_hash_blake3(&import_options_fingerprint(options)),
        schema_version: GLTF_IMPORTED_COOK_SCHEMA,
    }
}

/// Fingerprints external glTF dependencies relative to `base_path`.
///
/// Required buffer URIs must resolve; optional image URIs may be missing and
/// then fingerprint as a stable sentinel so appearance later still misses.
///
/// # Errors
///
/// Returns [`ImportError`] when the document cannot be probed or a required
/// dependency cannot be read safely under `base_path`.
pub fn fingerprint_gltf_dependencies(
    source_bytes: &[u8],
    base_path: &Path,
) -> Result<(Vec<String>, Vec<String>), ImportError> {
    let dependencies = discover_external_dependencies(source_bytes)?;
    let base = std::fs::canonicalize(base_path).map_err(|source| ImportError::ReadBasePath {
        path: base_path.to_owned(),
        source,
    })?;
    let mut uris = Vec::with_capacity(dependencies.len());
    let mut fingerprints = Vec::with_capacity(dependencies.len());
    for dependency in dependencies {
        let token = match load_uri_buffer(&dependency.uri, &base) {
            Ok(bytes) => content_hash_blake3(&bytes),
            Err(ImportError::ReadBuffer { .. })
                if dependency.kind == ImportDependencyKind::Optional =>
            {
                content_hash_blake3(MISSING_OPTIONAL_TOKEN)
            }
            Err(error) => return Err(error),
        };
        uris.push(dependency.uri);
        fingerprints.push(token);
    }
    Ok((uris, fingerprints))
}

/// Returns true when every recorded dependency fingerprint still matches disk.
///
/// # Errors
///
/// Returns [`ImportError`] for unsafe paths or I/O while re-reading required
/// dependencies.
pub fn dependency_fingerprints_match(
    uris: &[String],
    fingerprints: &[String],
    base_path: &Path,
) -> Result<bool, ImportError> {
    if uris.len() != fingerprints.len() {
        return Ok(false);
    }
    if uris.is_empty() {
        return Ok(true);
    }
    let base = std::fs::canonicalize(base_path).map_err(|source| ImportError::ReadBasePath {
        path: base_path.to_owned(),
        source,
    })?;
    for (uri, expected) in uris.iter().zip(fingerprints.iter()) {
        let actual = match load_uri_buffer(uri, &base) {
            Ok(bytes) => content_hash_blake3(&bytes),
            Err(ImportError::ReadBuffer { .. }) => content_hash_blake3(MISSING_OPTIONAL_TOKEN),
            Err(error) => return Err(error),
        };
        if actual != *expected {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Imports self-contained glTF bytes, reusing or populating the supplied cache.
///
/// Embedded-only path: no external dependency fingerprints. Prefer
/// [`import_scene_bytes_cached_at`] for documents with external buffers/images.
///
/// The import report is intentionally omitted from cached blobs because it
/// contains `gltf::mesh::Mode`, which is not serializable. Consequently a
/// cache-hit result has an empty [`crate::ImportReport`].
///
/// # Errors
///
/// Returns an import, encoding, decoding, or cache I/O error.
pub fn import_scene_bytes_cached(
    source_bytes: &[u8],
    options: ImportOptions,
    cache: &CookCache,
) -> Result<(ImportedAsset, bool), Box<dyn Error + Send + Sync>> {
    let key = cook_key_for_gltf_source(source_bytes, &options);
    let (artifact, cache_hit) = cache.get_or_insert_with(&key, &[], &[], || {
        let asset = import_scene_bytes_embedded(source_bytes, options)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        encode_imported_asset(&asset).map_err(|error| std::io::Error::other(error.to_string()))
    })?;
    Ok((decode_imported_asset(&artifact.bytes)?, cache_hit))
}

/// Imports glTF bytes with external deps resolved under `base_path`, using cache.
///
/// Cache key stays source-only so an unchanged document can hit without a full
/// re-import. On hit, dependency fingerprints are re-checked cheaply; a changed
/// external texture/buffer forces a miss and rewrite.
///
/// # Errors
///
/// Returns an import, encoding, decoding, dependency, or cache I/O error.
pub fn import_scene_bytes_cached_at(
    source_bytes: &[u8],
    base_path: &Path,
    options: ImportOptions,
    cache: &CookCache,
) -> Result<(ImportedAsset, bool), Box<dyn Error + Send + Sync>> {
    let key = cook_key_for_gltf_source(source_bytes, &options);
    if let Some(hit) = cache.get(&key)? {
        let fresh = dependency_fingerprints_match(
            &hit.manifest.dependencies,
            &hit.manifest.dependency_fingerprints,
            base_path,
        )?;
        if fresh {
            return Ok((decode_imported_asset(&hit.bytes)?, true));
        }
    }

    let (uris, fingerprints) = fingerprint_gltf_dependencies(source_bytes, base_path)?;
    let asset = import_scene_bytes_with_base_path(source_bytes, base_path, options)?;
    let bytes = encode_imported_asset(&asset)?;
    let artifact = CookedArtifact {
        bytes,
        manifest: CookManifest {
            key,
            dependencies: uris,
            dependency_fingerprints: fingerprints,
        },
    };
    cache.put(&artifact)?;
    Ok((decode_imported_asset(&artifact.bytes)?, false))
}

#[cfg(test)]
mod tests {
    use super::{
        decode_imported_asset, encode_imported_asset, import_options_fingerprint,
        import_scene_bytes_cached, import_scene_bytes_cached_at,
    };
    use crate::{ImportOptions, ImportedAsset, ImportedScene};
    use base64::Engine as _;
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };
    use yuyib_assets::CookCache;
    use yuyib_model::Model;

    fn triangle_gltf() -> String {
        let bytes = [
            0, 0, 1, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 128, 63, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 128, 63, 0, 0, 0, 0,
        ];
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        format!(
            r#"{{"asset":{{"version":"2.0"}},"buffers":[{{"uri":"data:application/octet-stream;base64,{encoded}","byteLength":44}}],"bufferViews":[{{"buffer":0,"byteOffset":0,"byteLength":6}},{{"buffer":0,"byteOffset":8,"byteLength":36}}],"accessors":[{{"bufferView":0,"componentType":5123,"count":3,"type":"SCALAR"}},{{"bufferView":1,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}}],"meshes":[{{"primitives":[{{"attributes":{{"POSITION":1}},"indices":0}}]}}]}}"#
        )
    }

    fn temp_root(label: &str) -> std::path::PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        std::env::temp_dir().join(format!("yuyib_gltf_cook_{label}_{stamp}"))
    }

    #[test]
    fn round_trip_preserves_cube_model_and_scene() {
        let asset = ImportedAsset {
            model: Model::cube(0.5).expect("cube"),
            scene: ImportedScene::default(),
            report: Default::default(),
        };
        let decoded =
            decode_imported_asset(&encode_imported_asset(&asset).expect("encode")).expect("decode");
        assert_eq!(decoded, asset);
    }

    #[test]
    fn second_import_uses_disk_cache() {
        let root = temp_root("hit");
        let cache = CookCache::new(&root);
        let source = triangle_gltf();
        let options = ImportOptions::default();

        let (first, hit) =
            import_scene_bytes_cached(source.as_bytes(), options, &cache).expect("cache miss");
        assert!(!hit);
        let (second, hit) =
            import_scene_bytes_cached(source.as_bytes(), options, &cache).expect("cache hit");
        assert!(hit);
        assert_eq!(first, second);
        assert_eq!(
            import_options_fingerprint(&options),
            import_options_fingerprint(&options)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn external_buffer_change_forces_cache_miss() {
        let root = temp_root("deps");
        fs::create_dir_all(&root).expect("root");
        let cache = CookCache::new(root.join("cache"));
        let bin_path = root.join("geom.bin");
        let bin_v1 = [
            0_u8, 0, 1, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 128, 63, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 128, 63, 0, 0, 0, 0,
        ];
        fs::write(&bin_path, bin_v1).expect("bin v1");

        let json = r#"{"asset":{"version":"2.0"},"buffers":[{"uri":"geom.bin","byteLength":44}],"bufferViews":[{"buffer":0,"byteOffset":0,"byteLength":6},{"buffer":0,"byteOffset":8,"byteLength":36}],"accessors":[{"bufferView":0,"componentType":5123,"count":3,"type":"SCALAR"},{"bufferView":1,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}],"meshes":[{"primitives":[{"attributes":{"POSITION":1},"indices":0}]}]}"#;
        let options = ImportOptions::default();
        let (_first, hit) =
            import_scene_bytes_cached_at(json.as_bytes(), &root, options, &cache).expect("miss");
        assert!(!hit);
        let (_second, hit) =
            import_scene_bytes_cached_at(json.as_bytes(), &root, options, &cache).expect("hit");
        assert!(hit);

        // Change only the external buffer; glTF JSON stays identical.
        let mut bin_v2 = bin_v1;
        bin_v2[22] = 64; // tweak a float byte
        fs::write(&bin_path, bin_v2).expect("bin v2");
        let (_third, hit) =
            import_scene_bytes_cached_at(json.as_bytes(), &root, options, &cache).expect("dep miss");
        assert!(!hit, "changed external buffer must invalidate cook cache");
        let _ = fs::remove_dir_all(root);
    }
}
