//! Versioned `*.ypack` shipping package over cooked artifacts.
//!
//! A pack is an immutable snapshot of `.ycook` blobs + manifests from a cook
//! cache. Authoring documents stay the source of truth; packs are derived.

use std::{
    error::Error,
    fmt,
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::cook::{CookKey, CookManifest, CookedArtifact, content_hash_blake3};
use crate::cook_cache::CookCache;

/// Magic bytes at the start of every pack file.
pub const YPACK_MAGIC: &[u8; 4] = b"YPCK";
/// Current on-disk pack format version.
pub const YPACK_FORMAT_VERSION: u32 = 1;
/// Stable format id embedded in the JSON index.
pub const YPACK_FORMAT_ID: &str = "yuyib.ypack";

/// One packed cooked artifact (cache-relative path + payload).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct YPackEntry {
    /// Path relative to the cook cache root (`CookKey::relative_path`).
    pub relative_path: String,
    /// Cooked bytes + invalidation manifest.
    pub artifact: CookedArtifact,
}

/// JSON index stored after the binary header.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct YPackIndexFile {
    format: String,
    format_version: u32,
    entries: Vec<YPackIndexEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct YPackIndexEntry {
    relative_path: String,
    artifact_hash: String,
    byte_offset: u64,
    byte_length: u64,
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
}

/// Failure while reading or writing a `*.ypack`.
#[derive(Debug)]
pub enum YPackError {
    /// Filesystem failure for a concrete path.
    Io {
        /// Path involved in the failure.
        path: PathBuf,
        /// Underlying I/O error.
        source: io::Error,
    },
    /// Pack header/index/blob failed validation.
    Format(String),
    /// Entry artifact hash does not match recorded integrity hash.
    ArtifactHashMismatch {
        /// Relative path inside the pack.
        relative_path: String,
        /// Hash recorded in the index.
        expected: String,
        /// Hash of the decoded bytes.
        actual: String,
    },
}

impl fmt::Display for YPackError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(formatter, "ypack I/O failed for {}: {source}", path.display())
            }
            Self::Format(message) => write!(formatter, "ypack format error: {message}"),
            Self::ArtifactHashMismatch {
                relative_path,
                expected,
                actual,
            } => write!(
                formatter,
                "ypack artifact hash mismatch for `{relative_path}` (expected {expected}, got {actual})"
            ),
        }
    }
}

impl Error for YPackError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Format(_) | Self::ArtifactHashMismatch { .. } => None,
        }
    }
}

/// Writes a versioned pack file atomically (temp + rename).
///
/// # Errors
///
/// Returns [`YPackError`] for encode or filesystem failures.
pub fn write_ypack(path: impl AsRef<Path>, entries: &[YPackEntry]) -> Result<(), YPackError> {
    let path = path.as_ref();
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|source| YPackError::Io {
            path: parent.to_owned(),
            source,
        })?;
    }
    let payload = encode_ypack(entries)?;
    let tmp = path.with_extension("ypack.tmp");
    {
        let mut file = File::create(&tmp).map_err(|source| YPackError::Io {
            path: tmp.clone(),
            source,
        })?;
        file.write_all(&payload).map_err(|source| YPackError::Io {
            path: tmp.clone(),
            source,
        })?;
        file.sync_all().map_err(|source| YPackError::Io {
            path: tmp.clone(),
            source,
        })?;
    }
    fs::rename(&tmp, path).map_err(|source| YPackError::Io {
        path: path.to_owned(),
        source,
    })?;
    Ok(())
}

/// Reads and validates a pack file into ordered entries.
///
/// # Errors
///
/// Returns [`YPackError`] for I/O, format, or integrity failures.
pub fn read_ypack(path: impl AsRef<Path>) -> Result<Vec<YPackEntry>, YPackError> {
    let path = path.as_ref();
    let bytes = fs::read(path).map_err(|source| YPackError::Io {
        path: path.to_owned(),
        source,
    })?;
    decode_ypack(&bytes)
}

/// Encodes pack bytes in memory (useful for tests).
///
/// # Errors
///
/// Returns [`YPackError::Format`] when JSON index encoding fails.
pub fn encode_ypack(entries: &[YPackEntry]) -> Result<Vec<u8>, YPackError> {
    let mut index_entries = Vec::with_capacity(entries.len());
    let mut blobs = Vec::new();
    let mut offset = 0_u64;
    for entry in entries {
        if entry.artifact.manifest.dependencies.len()
            != entry.artifact.manifest.dependency_fingerprints.len()
        {
            return Err(YPackError::Format(
                "dependencies and dependency_fingerprints length mismatch".into(),
            ));
        }
        let artifact_hash = content_hash_blake3(&entry.artifact.bytes);
        let byte_length = entry.artifact.bytes.len() as u64;
        index_entries.push(YPackIndexEntry {
            relative_path: entry.relative_path.clone(),
            artifact_hash,
            byte_offset: offset,
            byte_length,
            content_hash: entry.artifact.manifest.key.content_hash.clone(),
            importer_id: entry.artifact.manifest.key.importer_id.clone(),
            importer_version: entry.artifact.manifest.key.importer_version.clone(),
            cooker_id: entry.artifact.manifest.key.cooker_id.clone(),
            cooker_version: entry.artifact.manifest.key.cooker_version.clone(),
            options_hash: entry.artifact.manifest.key.options_hash.clone(),
            artifact_schema_version: entry.artifact.manifest.key.schema_version,
            dependencies: entry.artifact.manifest.dependencies.clone(),
            dependency_fingerprints: entry.artifact.manifest.dependency_fingerprints.clone(),
        });
        blobs.extend_from_slice(&entry.artifact.bytes);
        offset = offset.saturating_add(byte_length);
    }
    let index = YPackIndexFile {
        format: YPACK_FORMAT_ID.to_owned(),
        format_version: YPACK_FORMAT_VERSION,
        entries: index_entries,
    };
    let index_json = serde_json::to_vec(&index)
        .map_err(|error| YPackError::Format(format!("index encode failed: {error}")))?;
    let index_len = u32::try_from(index_json.len())
        .map_err(|_| YPackError::Format("index larger than u32::MAX".into()))?;

    let mut out = Vec::with_capacity(4 + 4 + 4 + index_json.len() + blobs.len());
    out.extend_from_slice(YPACK_MAGIC);
    out.extend_from_slice(&YPACK_FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&index_len.to_le_bytes());
    out.extend_from_slice(&index_json);
    out.extend_from_slice(&blobs);
    Ok(out)
}

/// Decodes pack bytes produced by [`encode_ypack`] / [`write_ypack`].
///
/// # Errors
///
/// Returns [`YPackError`] for header, index, or integrity failures.
pub fn decode_ypack(bytes: &[u8]) -> Result<Vec<YPackEntry>, YPackError> {
    if bytes.len() < 12 {
        return Err(YPackError::Format("truncated header".into()));
    }
    if &bytes[..4] != YPACK_MAGIC {
        return Err(YPackError::Format("bad magic".into()));
    }
    let format_version = u32::from_le_bytes(bytes[4..8].try_into().expect("4 bytes"));
    if format_version != YPACK_FORMAT_VERSION {
        return Err(YPackError::Format(format!(
            "unsupported format_version {format_version} (expected {YPACK_FORMAT_VERSION})"
        )));
    }
    let index_len = u32::from_le_bytes(bytes[8..12].try_into().expect("4 bytes")) as usize;
    let index_start: usize = 12;
    let index_end = index_start
        .checked_add(index_len)
        .ok_or_else(|| YPackError::Format("index length overflow".into()))?;
    if index_end > bytes.len() {
        return Err(YPackError::Format("truncated index".into()));
    }
    let index: YPackIndexFile = serde_json::from_slice(&bytes[index_start..index_end])
        .map_err(|error| YPackError::Format(format!("index decode failed: {error}")))?;
    if index.format != YPACK_FORMAT_ID {
        return Err(YPackError::Format(format!(
            "unexpected format id `{}`",
            index.format
        )));
    }
    if index.format_version != YPACK_FORMAT_VERSION {
        return Err(YPackError::Format(format!(
            "index format_version {} mismatch",
            index.format_version
        )));
    }
    let blob_base = index_end;
    let mut out = Vec::with_capacity(index.entries.len());
    for entry in index.entries {
        let start = blob_base
            .checked_add(entry.byte_offset as usize)
            .ok_or_else(|| YPackError::Format("blob offset overflow".into()))?;
        let end = start
            .checked_add(entry.byte_length as usize)
            .ok_or_else(|| YPackError::Format("blob length overflow".into()))?;
        if end > bytes.len() {
            return Err(YPackError::Format(format!(
                "truncated blob for `{}`",
                entry.relative_path
            )));
        }
        let artifact_bytes = bytes[start..end].to_vec();
        let actual_hash = content_hash_blake3(&artifact_bytes);
        if actual_hash != entry.artifact_hash {
            return Err(YPackError::ArtifactHashMismatch {
                relative_path: entry.relative_path,
                expected: entry.artifact_hash,
                actual: actual_hash,
            });
        }
        if entry.dependencies.len() != entry.dependency_fingerprints.len() {
            return Err(YPackError::Format(
                "dependencies and dependency_fingerprints length mismatch".into(),
            ));
        }
        out.push(YPackEntry {
            relative_path: entry.relative_path,
            artifact: CookedArtifact {
                bytes: artifact_bytes,
                manifest: CookManifest {
                    key: CookKey {
                        content_hash: entry.content_hash,
                        importer_id: entry.importer_id,
                        importer_version: entry.importer_version,
                        cooker_id: entry.cooker_id,
                        cooker_version: entry.cooker_version,
                        options_hash: entry.options_hash,
                        schema_version: entry.artifact_schema_version,
                    },
                    dependencies: entry.dependencies,
                    dependency_fingerprints: entry.dependency_fingerprints,
                },
            },
        });
    }
    Ok(out)
}

/// Collects every valid `.ycook` + sidecar pair under a cook cache root.
///
/// # Errors
///
/// Returns [`YPackError`] for I/O or corrupt sidecar/blob pairs.
pub fn collect_ypack_entries_from_cook_root(
    cook_root: impl AsRef<Path>,
) -> Result<Vec<YPackEntry>, YPackError> {
    let cook_root = cook_root.as_ref();
    let mut entries = Vec::new();
    if !cook_root.is_dir() {
        return Ok(entries);
    }
    collect_ycook_files(cook_root, cook_root, &mut entries)?;
    entries.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(entries)
}

/// Result of hydrating a cook cache from a pack file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct YPackHydrateReport {
    /// Entries present in the pack.
    pub entries: usize,
    /// Artifacts successfully written into the cache.
    pub written: usize,
}

/// Reads a `*.ypack` and writes every artifact into `cache` via [`CookCache::put`].
///
/// Existing cache entries with the same [`CookKey`] are replaced atomically.
/// Source glTF files are not required after a successful hydrate — subsequent
/// cook lookups can hit from disk.
///
/// # Errors
///
/// Returns [`YPackError`] for pack decode/integrity failures or cache I/O.
pub fn hydrate_cook_cache_from_ypack(
    pack_path: impl AsRef<Path>,
    cache: &CookCache,
) -> Result<YPackHydrateReport, YPackError> {
    let entries = read_ypack(pack_path)?;
    let total = entries.len();
    let mut written = 0_usize;
    for entry in &entries {
        cache
            .put(&entry.artifact)
            .map_err(|error| YPackError::Format(format!("cook cache put failed: {error}")))?;
        written += 1;
    }
    Ok(YPackHydrateReport {
        entries: total,
        written,
    })
}

fn collect_ycook_files(
    cook_root: &Path,
    dir: &Path,
    out: &mut Vec<YPackEntry>,
) -> Result<(), YPackError> {
    let read_dir = fs::read_dir(dir).map_err(|source| YPackError::Io {
        path: dir.to_owned(),
        source,
    })?;
    let mut children: Vec<_> = read_dir.filter_map(Result::ok).collect();
    children.sort_by_key(|entry| entry.file_name());
    for child in children {
        let path = child.path();
        if path.is_dir() {
            collect_ycook_files(cook_root, &path, out)?;
            continue;
        }
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if !name.ends_with(".ycook") || name.ends_with(".ycook.tmp") {
            continue;
        }
        let relative = path
            .strip_prefix(cook_root)
            .map_err(|_| {
                YPackError::Format(format!(
                    "cook path {} escapes root {}",
                    path.display(),
                    cook_root.display()
                ))
            })?
            .to_string_lossy()
            .replace('\\', "/");
        let artifact = read_cook_pair(&path)?;
        out.push(YPackEntry {
            relative_path: relative,
            artifact,
        });
    }
    Ok(())
}

fn read_cook_pair(artifact_path: &Path) -> Result<CookedArtifact, YPackError> {
    let manifest_path = artifact_path.with_extension("ycook.json");
    let text = fs::read_to_string(&manifest_path).map_err(|source| YPackError::Io {
        path: manifest_path.clone(),
        source,
    })?;
    let file: SidecarManifest =
        serde_json::from_str(&text).map_err(|error| YPackError::Format(error.to_string()))?;
    if file.schema_version != 1 {
        return Err(YPackError::Format(format!(
            "unsupported cook sidecar schema {}",
            file.schema_version
        )));
    }
    let mut file_handle = File::open(artifact_path).map_err(|source| YPackError::Io {
        path: artifact_path.to_owned(),
        source,
    })?;
    let mut bytes = Vec::new();
    file_handle
        .read_to_end(&mut bytes)
        .map_err(|source| YPackError::Io {
            path: artifact_path.to_owned(),
            source,
        })?;
    let actual_hash = content_hash_blake3(&bytes);
    if actual_hash != file.artifact_hash {
        return Err(YPackError::ArtifactHashMismatch {
            relative_path: artifact_path.display().to_string(),
            expected: file.artifact_hash,
            actual: actual_hash,
        });
    }
    if file.dependencies.len() != file.dependency_fingerprints.len() {
        return Err(YPackError::Format(
            "dependencies and dependency_fingerprints length mismatch".into(),
        ));
    }
    Ok(CookedArtifact {
        bytes,
        manifest: CookManifest {
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
    })
}

#[derive(Clone, Debug, Deserialize)]
struct SidecarManifest {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cook::{CookKey, CookManifest, CookedArtifact};
    use crate::CookCache;

    fn sample_artifact(label: &str) -> CookedArtifact {
        let bytes = format!("cooked-{label}").into_bytes();
        CookedArtifact {
            bytes,
            manifest: CookManifest {
                key: CookKey {
                    content_hash: content_hash_blake3(label.as_bytes()),
                    importer_id: "yuyib.gltf".into(),
                    importer_version: "0.1.0".into(),
                    cooker_id: "yuyib.gltf.imported".into(),
                    cooker_version: "0.1.0".into(),
                    options_hash: content_hash_blake3(b"opts"),
                    schema_version: 1,
                },
                dependencies: vec!["dep.bin".into()],
                dependency_fingerprints: vec![content_hash_blake3(b"dep")],
            },
        }
    }

    #[test]
    fn encode_decode_round_trip() {
        let entries = vec![
            YPackEntry {
                relative_path: "aa/bb/one.ycook".into(),
                artifact: sample_artifact("one"),
            },
            YPackEntry {
                relative_path: "cc/dd/two.ycook".into(),
                artifact: sample_artifact("two"),
            },
        ];
        let bytes = encode_ypack(&entries).expect("encode");
        let decoded = decode_ypack(&bytes).expect("decode");
        assert_eq!(decoded, entries);
    }

    #[test]
    fn pack_from_cook_cache_round_trip() {
        let root = std::env::temp_dir().join(format!(
            "yuyib_ypack_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        let cache = CookCache::new(root.join("cook"));
        let first = sample_artifact("alpha");
        let second = sample_artifact("beta");
        cache.put(&first).expect("put first");
        cache.put(&second).expect("put second");

        let entries = collect_ypack_entries_from_cook_root(cache.root()).expect("collect");
        assert_eq!(entries.len(), 2);
        let pack_path = root.join("out.ypack");
        write_ypack(&pack_path, &entries).expect("write");
        let decoded = read_ypack(&pack_path).expect("read");
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].artifact.bytes, first.bytes);
        assert_eq!(decoded[1].artifact.bytes, second.bytes);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn hydrate_empty_cache_from_pack_yields_hits() {
        let root = std::env::temp_dir().join(format!(
            "yuyib_ypack_hydrate_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        let source = CookCache::new(root.join("source_cook"));
        let first = sample_artifact("hydrate-a");
        let second = sample_artifact("hydrate-b");
        source.put(&first).expect("put first");
        source.put(&second).expect("put second");
        let pack_path = root.join("ship.ypack");
        let entries = collect_ypack_entries_from_cook_root(source.root()).expect("collect");
        write_ypack(&pack_path, &entries).expect("write pack");

        let empty = CookCache::new(root.join("empty_cook"));
        assert!(empty.get(&first.manifest.key).expect("get").is_none());
        let report = hydrate_cook_cache_from_ypack(&pack_path, &empty).expect("hydrate");
        assert_eq!(report.entries, 2);
        assert_eq!(report.written, 2);

        let hit_a = empty.get(&first.manifest.key).expect("get a").expect("hit a");
        let hit_b = empty.get(&second.manifest.key).expect("get b").expect("hit b");
        assert_eq!(hit_a.bytes, first.bytes);
        assert_eq!(hit_b.bytes, second.bytes);
        let _ = fs::remove_dir_all(&root);
    }
}
