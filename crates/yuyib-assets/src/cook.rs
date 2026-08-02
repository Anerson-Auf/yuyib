//! Versioned asset cooking contracts for the M3 pipeline.
//!
//! Importers produce renderer-neutral values. Cookers turn those values into
//! runtime artifacts and participate in [`CookCache`] keying through a stable
//! cooker identity and options fingerprint.

use std::{error::Error, fmt};

/// Stable cooker plugin identity used in cache keys and metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CookerIdentity {
    /// Stable cooker id, for example `yuyib.gltf.imported`.
    pub id: String,
    /// Semantic cooker version string.
    pub version: String,
}

impl CookerIdentity {
    /// Creates a cooker identity.
    #[must_use]
    pub fn new(id: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            version: version.into(),
        }
    }

    /// Formats the identity as `id@version` for [`crate::AssetMetadata`].
    #[must_use]
    pub fn metadata_label(&self) -> String {
        format!("{}@{}", self.id, self.version)
    }
}

/// Cache key inputs that must all match for a cook hit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CookKey {
    /// Content hash of the source bytes (`blake3:<hex>`).
    pub content_hash: String,
    /// Importer id, for example `yuyib.gltf`.
    pub importer_id: String,
    /// Importer semantic version.
    pub importer_version: String,
    /// Cooker id.
    pub cooker_id: String,
    /// Cooker semantic version.
    pub cooker_version: String,
    /// Fingerprint of cook/import options (stable hex or literal).
    pub options_hash: String,
    /// Artifact schema version encoded in the blob header/manifest.
    pub schema_version: u32,
}

impl CookKey {
    /// Builds a filesystem-safe relative key path under a cook cache root.
    #[must_use]
    pub fn relative_path(&self) -> String {
        // Keep path segments short and stable; full key also lives in the
        // sidecar manifest for collision diagnostics.
        let digest = crate::content_hash_blake3(
            format!(
                "{}|{}@{}|{}@{}|{}|{}",
                self.content_hash,
                self.importer_id,
                self.importer_version,
                self.cooker_id,
                self.cooker_version,
                self.options_hash,
                self.schema_version
            )
            .as_bytes(),
        );
        let hex = digest
            .strip_prefix("blake3:")
            .unwrap_or(digest.as_str());
        format!(
            "{}/{}/{}.ycook",
            &hex[..2.min(hex.len())],
            &hex[2..4.min(hex.len())],
            hex
        )
    }
}

/// Sidecar metadata written next to a cooked artifact blob.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CookManifest {
    /// Cache key that produced this artifact.
    pub key: CookKey,
    /// Logical dependency URIs in deterministic order.
    pub dependencies: Vec<String>,
    /// Content fingerprints parallel to [`Self::dependencies`].
    ///
    /// Used by path-aware cookers to invalidate a source-only cache hit when an
    /// external buffer/image file changes without rewriting the glTF document.
    pub dependency_fingerprints: Vec<String>,
}

/// Opaque cooked bytes plus the manifest used for invalidation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CookedArtifact {
    /// Versioned artifact payload.
    pub bytes: Vec<u8>,
    /// Manifest copied into the sidecar on put.
    pub manifest: CookManifest,
}

/// Context passed to [`AssetCooker::cook`].
#[derive(Clone, Debug, Default)]
pub struct CookContext {
    /// Optional logical source URI for diagnostics.
    pub source: Option<String>,
}

/// Converts a neutral imported value into a runtime artifact.
pub trait AssetCooker<Neutral, Runtime>: Send + Sync {
    /// Failure while cooking one asset.
    type Error: Error + Send + Sync + 'static;

    /// Returns the stable cooker identity for cache keys.
    fn identity(&self) -> CookerIdentity;

    /// Cooks one neutral value into a runtime artifact.
    ///
    /// # Errors
    ///
    /// Returns [`Self::Error`] when cooking fails.
    fn cook(&self, input: &Neutral, context: &CookContext) -> Result<Runtime, Self::Error>;
}

/// Failure while hashing, encoding, or validating cook inputs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CookError {
    /// Content hash input was empty.
    EmptyContent,
    /// Artifact schema did not match the expected version.
    SchemaMismatch {
        /// Schema found in the blob.
        found: u32,
        /// Schema expected by the reader.
        expected: u32,
    },
    /// Payload could not be decoded.
    Decode(String),
    /// Payload could not be encoded.
    Encode(String),
}

impl fmt::Display for CookError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyContent => formatter.write_str("cannot hash empty cook content"),
            Self::SchemaMismatch { found, expected } => write!(
                formatter,
                "cook artifact schema {found} does not match expected {expected}"
            ),
            Self::Decode(message) => write!(formatter, "cook decode failed: {message}"),
            Self::Encode(message) => write!(formatter, "cook encode failed: {message}"),
        }
    }
}

impl Error for CookError {}

/// Hashes bytes as `blake3:<lowercase-hex>`.
#[must_use]
pub fn content_hash_blake3(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

/// Hashes a UTF-8 options fingerprint string.
#[must_use]
pub fn options_hash_blake3(options: &str) -> String {
    content_hash_blake3(options.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::{
        AssetCooker, CookContext, CookError, CookKey, CookManifest, CookedArtifact, CookerIdentity,
        content_hash_blake3, options_hash_blake3,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingIdentityCooker {
        cooks: AtomicUsize,
    }

    impl AssetCooker<Vec<u8>, Vec<u8>> for CountingIdentityCooker {
        type Error = CookError;

        fn identity(&self) -> CookerIdentity {
            CookerIdentity::new("yuyib.test.identity", "0.1.0")
        }

        fn cook(&self, input: &Vec<u8>, _context: &CookContext) -> Result<Vec<u8>, Self::Error> {
            self.cooks.fetch_add(1, Ordering::SeqCst);
            Ok(input.clone())
        }
    }

    #[test]
    fn content_hash_is_stable_and_prefixed() {
        let hash = content_hash_blake3(b"yuyib");
        assert!(hash.starts_with("blake3:"));
        assert_eq!(hash, content_hash_blake3(b"yuyib"));
        assert_ne!(hash, content_hash_blake3(b"yuyib!"));
    }

    #[test]
    fn options_hash_changes_with_settings() {
        assert_ne!(
            options_hash_blake3("policy=strict"),
            options_hash_blake3("policy=preview")
        );
    }

    #[test]
    fn cooker_counter_proves_miss_path() {
        let cooker = CountingIdentityCooker {
            cooks: AtomicUsize::new(0),
        };
        let runtime = cooker
            .cook(&b"mesh".to_vec(), &CookContext::default())
            .expect("cook");
        assert_eq!(runtime, b"mesh");
        assert_eq!(cooker.cooks.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn relative_path_is_stable_for_same_key() {
        let key = CookKey {
            content_hash: content_hash_blake3(b"src"),
            importer_id: "yuyib.gltf".into(),
            importer_version: "0.1.0".into(),
            cooker_id: "yuyib.gltf.imported".into(),
            cooker_version: "0.1.0".into(),
            options_hash: options_hash_blake3("default"),
            schema_version: 1,
        };
        assert_eq!(key.relative_path(), key.relative_path());
        let artifact = CookedArtifact {
            bytes: b"blob".to_vec(),
            manifest: CookManifest {
                key: key.clone(),
                dependencies: Vec::new(),
                dependency_fingerprints: Vec::new(),
            },
        };
        assert_eq!(artifact.manifest.key, key);
    }
}
