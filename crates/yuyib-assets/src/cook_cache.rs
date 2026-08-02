//! Disk-backed cook artifact cache with atomic replace.

use std::{
    error::Error,
    fmt,
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::cook::{CookKey, CookManifest, CookedArtifact, content_hash_blake3};

const MANIFEST_SCHEMA: u32 = 1;

/// On-disk cook artifact cache rooted at one directory.
#[derive(Clone, Debug)]
pub struct CookCache {
    root: PathBuf,
}

impl CookCache {
    /// Creates a cache rooted at `root` (created on first put).
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Returns the cache root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Looks up a cooked blob by key.
    ///
    /// # Errors
    ///
    /// Returns [`CookCacheError`] for I/O or corrupt sidecar/blob pairs.
    pub fn get(&self, key: &CookKey) -> Result<Option<CookedArtifact>, CookCacheError> {
        let artifact_path = self.root.join(key.relative_path());
        let manifest_path = manifest_path_for(&artifact_path);
        if !artifact_path.is_file() || !manifest_path.is_file() {
            return Ok(None);
        }
        let (manifest, artifact_hash) = read_manifest(&manifest_path)?;
        if manifest.key != *key {
            return Err(CookCacheError::KeyMismatch {
                path: artifact_path,
            });
        }
        let bytes = fs::read(&artifact_path).map_err(|source| CookCacheError::Io {
            path: artifact_path.clone(),
            source,
        })?;
        let actual_hash = content_hash_blake3(&bytes);
        if actual_hash != artifact_hash {
            return Err(CookCacheError::ArtifactHashMismatch {
                path: artifact_path,
                expected: artifact_hash,
                actual: actual_hash,
            });
        }
        Ok(Some(CookedArtifact { bytes, manifest }))
    }

    /// Writes a cooked blob and sidecar manifest atomically.
    ///
    /// # Errors
    ///
    /// Returns [`CookCacheError`] for I/O failures.
    pub fn put(&self, artifact: &CookedArtifact) -> Result<(), CookCacheError> {
        let artifact_path = self.root.join(artifact.manifest.key.relative_path());
        let manifest_path = manifest_path_for(&artifact_path);
        if let Some(parent) = artifact_path.parent() {
            fs::create_dir_all(parent).map_err(|source| CookCacheError::Io {
                path: parent.to_owned(),
                source,
            })?;
        }
        let tmp_artifact = artifact_path.with_extension("ycook.tmp");
        let tmp_manifest = manifest_path.with_extension("json.tmp");
        write_bytes_atomic(&tmp_artifact, &artifact_path, &artifact.bytes)?;
        let manifest_json = encode_manifest(artifact)?;
        write_bytes_atomic(&tmp_manifest, &manifest_path, manifest_json.as_bytes())?;
        Ok(())
    }

    /// Returns a cached blob or cooks, stores, and returns a fresh one.
    ///
    /// `cook` runs only on a miss. The returned flag is `true` on a cache hit.
    ///
    /// # Errors
    ///
    /// Returns [`CookCacheError`] for cache I/O, or maps cook failures through
    /// [`CookCacheError::Cook`].
    pub fn get_or_insert_with<E, F>(
        &self,
        key: &CookKey,
        dependencies: &[String],
        dependency_fingerprints: &[String],
        cook: F,
    ) -> Result<(CookedArtifact, bool), CookCacheError>
    where
        E: Error + Send + Sync + 'static,
        F: FnOnce() -> Result<Vec<u8>, E>,
    {
        if let Some(hit) = self.get(key)? {
            return Ok((hit, true));
        }
        let bytes = cook().map_err(|error| CookCacheError::Cook(error.to_string()))?;
        let artifact = CookedArtifact {
            bytes,
            manifest: CookManifest {
                key: key.clone(),
                dependencies: dependencies.to_vec(),
                dependency_fingerprints: dependency_fingerprints.to_vec(),
            },
        };
        self.put(&artifact)?;
        Ok((artifact, false))
    }
}

/// Failure while reading or writing the cook cache.
#[derive(Debug)]
pub enum CookCacheError {
    /// Filesystem failure for a concrete path.
    Io {
        /// Path involved in the failure.
        path: PathBuf,
        /// Underlying I/O error.
        source: io::Error,
    },
    /// Sidecar JSON could not be parsed or validated.
    Manifest(String),
    /// Blob exists but its sidecar key does not match the lookup key.
    KeyMismatch {
        /// Artifact path that failed validation.
        path: PathBuf,
    },
    /// Artifact bytes no longer match the sidecar integrity hash.
    ArtifactHashMismatch {
        /// Artifact path that failed validation.
        path: PathBuf,
        /// Hash recorded in the sidecar.
        expected: String,
        /// Hash of the bytes on disk.
        actual: String,
    },
    /// User cook closure failed.
    Cook(String),
}

impl fmt::Display for CookCacheError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(
                    formatter,
                    "cook cache I/O failed for {}: {source}",
                    path.display()
                )
            }
            Self::Manifest(message) => write!(formatter, "cook manifest invalid: {message}"),
            Self::KeyMismatch { path } => write!(
                formatter,
                "cook cache key mismatch for {}",
                path.display()
            ),
            Self::ArtifactHashMismatch {
                path,
                expected,
                actual,
            } => write!(
                formatter,
                "cook artifact hash mismatch for {} (expected {expected}, got {actual})",
                path.display()
            ),
            Self::Cook(message) => write!(formatter, "cook failed: {message}"),
        }
    }
}

impl Error for CookCacheError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Manifest(_)
            | Self::KeyMismatch { .. }
            | Self::ArtifactHashMismatch { .. }
            | Self::Cook(_) => None,
        }
    }
}

fn manifest_path_for(artifact_path: &Path) -> PathBuf {
    artifact_path.with_extension("ycook.json")
}

fn write_bytes_atomic(tmp: &Path, final_path: &Path, bytes: &[u8]) -> Result<(), CookCacheError> {
    {
        let mut file = File::create(tmp).map_err(|source| CookCacheError::Io {
            path: tmp.to_owned(),
            source,
        })?;
        file.write_all(bytes).map_err(|source| CookCacheError::Io {
            path: tmp.to_owned(),
            source,
        })?;
        file.sync_all().map_err(|source| CookCacheError::Io {
            path: tmp.to_owned(),
            source,
        })?;
    }
    fs::rename(tmp, final_path).map_err(|source| CookCacheError::Io {
        path: final_path.to_owned(),
        source,
    })?;
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct ManifestFile {
    schema_version: u32,
    content_hash: String,
    importer_id: String,
    importer_version: String,
    cooker_id: String,
    cooker_version: String,
    options_hash: String,
    artifact_schema_version: u32,
    dependencies: Vec<String>,
    #[serde(default)]
    dependency_fingerprints: Vec<String>,
    artifact_hash: String,
}

fn encode_manifest(artifact: &CookedArtifact) -> Result<String, CookCacheError> {
    if artifact.manifest.dependencies.len() != artifact.manifest.dependency_fingerprints.len() {
        return Err(CookCacheError::Manifest(
            "dependencies and dependency_fingerprints length mismatch".into(),
        ));
    }
    let file = ManifestFile {
        schema_version: MANIFEST_SCHEMA,
        content_hash: artifact.manifest.key.content_hash.clone(),
        importer_id: artifact.manifest.key.importer_id.clone(),
        importer_version: artifact.manifest.key.importer_version.clone(),
        cooker_id: artifact.manifest.key.cooker_id.clone(),
        cooker_version: artifact.manifest.key.cooker_version.clone(),
        options_hash: artifact.manifest.key.options_hash.clone(),
        artifact_schema_version: artifact.manifest.key.schema_version,
        dependencies: artifact.manifest.dependencies.clone(),
        dependency_fingerprints: artifact.manifest.dependency_fingerprints.clone(),
        artifact_hash: content_hash_blake3(&artifact.bytes),
    };
    serde_json::to_string_pretty(&file).map_err(|error| CookCacheError::Manifest(error.to_string()))
}

fn read_manifest(path: &Path) -> Result<(CookManifest, String), CookCacheError> {
    let text = fs::read_to_string(path).map_err(|source| CookCacheError::Io {
        path: path.to_owned(),
        source,
    })?;
    let file: ManifestFile =
        serde_json::from_str(&text).map_err(|error| CookCacheError::Manifest(error.to_string()))?;
    if file.schema_version != MANIFEST_SCHEMA {
        return Err(CookCacheError::Manifest(format!(
            "unsupported manifest schema {}",
            file.schema_version
        )));
    }
    if file.dependencies.len() != file.dependency_fingerprints.len() {
        return Err(CookCacheError::Manifest(
            "dependencies and dependency_fingerprints length mismatch".into(),
        ));
    }
    Ok((
        CookManifest {
            key: CookKey {
                content_hash: file.content_hash,
                importer_id: file.importer_id,
                importer_version: file.importer_version,
                cooker_id: file.cooker_id,
                cooker_version: file.cooker_version,
                options_hash: file.options_hash,
                schema_version: file.artifact_schema_version,
            },
            dependencies: file.dependencies,
            dependency_fingerprints: file.dependency_fingerprints,
        },
        file.artifact_hash,
    ))
}

#[cfg(test)]
mod tests {
    use super::CookCache;
    use crate::cook::{CookKey, content_hash_blake3, options_hash_blake3};
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    fn temp_root(label: &str) -> std::path::PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        std::env::temp_dir().join(format!("yuyib_cook_cache_{label}_{stamp}"))
    }

    fn sample_key(content: &[u8]) -> CookKey {
        CookKey {
            content_hash: content_hash_blake3(content),
            importer_id: "yuyib.test".into(),
            importer_version: "0.1.0".into(),
            cooker_id: "yuyib.test.identity".into(),
            cooker_version: "0.1.0".into(),
            options_hash: options_hash_blake3("default"),
            schema_version: 1,
        }
    }

    #[test]
    fn second_lookup_is_cache_hit_without_recook() {
        let root = temp_root("hit");
        let cache = CookCache::new(&root);
        let key = sample_key(b"source-bytes");
        let cooks = AtomicUsize::new(0);
        let (first, hit) = cache
            .get_or_insert_with(&key, &[], &[], || {
                cooks.fetch_add(1, Ordering::SeqCst);
                Ok::<_, std::io::Error>(b"cooked-payload".to_vec())
            })
            .expect("miss cook");
        assert!(!hit);
        assert_eq!(first.bytes, b"cooked-payload");
        assert_eq!(cooks.load(Ordering::SeqCst), 1);

        let (second, hit) = cache
            .get_or_insert_with(&key, &[], &[], || {
                cooks.fetch_add(1, Ordering::SeqCst);
                Ok::<_, std::io::Error>(b"should-not-run".to_vec())
            })
            .expect("hit");
        assert!(hit);
        assert_eq!(second.bytes, b"cooked-payload");
        assert_eq!(cooks.load(Ordering::SeqCst), 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn options_fingerprint_misses_separate_entries() {
        let root = temp_root("options");
        let cache = CookCache::new(&root);
        let mut key_a = sample_key(b"same-source");
        key_a.options_hash = options_hash_blake3("strict");
        let mut key_b = key_a.clone();
        key_b.options_hash = options_hash_blake3("preview");
        let _ = cache
            .get_or_insert_with(&key_a, &[], &[], || Ok::<_, std::io::Error>(b"a".to_vec()))
            .expect("a");
        let (b, hit) = cache
            .get_or_insert_with(&key_b, &[], &[], || Ok::<_, std::io::Error>(b"b".to_vec()))
            .expect("b");
        assert!(!hit);
        assert_eq!(b.bytes, b"b");
        let _ = std::fs::remove_dir_all(root);
    }
}
