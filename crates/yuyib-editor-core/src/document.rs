use std::{
    error::Error,
    fmt, fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    str::FromStr,
};

use atomic_write_file::AtomicWriteFile;
use serde::{Serialize, de::DeserializeOwned};

/// Content revision used for optimistic external-change detection.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct DocumentRevision([u8; 32]);

impl DocumentRevision {
    fn from_bytes(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    /// Parses the canonical 64-character hexadecimal revision.
    ///
    /// # Errors
    ///
    /// Returns [`DocumentRevisionParseError`] for malformed input.
    pub fn parse(value: &str) -> Result<Self, DocumentRevisionParseError> {
        blake3::Hash::from_hex(value)
            .map(|hash| Self(*hash.as_bytes()))
            .map_err(|_| DocumentRevisionParseError)
    }
}

impl FromStr for DocumentRevision {
    type Err = DocumentRevisionParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl fmt::Debug for DocumentRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&blake3::Hash::from_bytes(self.0).to_hex())
    }
}

impl fmt::Display for DocumentRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&blake3::Hash::from_bytes(self.0).to_hex())
    }
}

/// Invalid external document revision string.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DocumentRevisionParseError;

impl fmt::Display for DocumentRevisionParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("document revision must be 64 hexadecimal characters")
    }
}

impl Error for DocumentRevisionParseError {}

/// Parsed document paired with the exact revision loaded from disk.
#[derive(Debug)]
pub struct DocumentSnapshot<T> {
    /// Parsed value.
    pub value: T,
    /// Content revision that must be supplied for conflict-aware save.
    pub revision: DocumentRevision,
}

/// External file state no longer matches the editor's loaded revision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DocumentConflict {
    /// Revision expected by the editor, or `None` for a newly created file.
    pub expected: Option<DocumentRevision>,
    /// Revision currently present on disk, or `None` if it was removed.
    pub actual: Option<DocumentRevision>,
}

/// Confined and bounded JSON document storage below one project root.
#[derive(Clone, Debug)]
pub struct ProjectDocumentStore {
    root: PathBuf,
    maximum_bytes: usize,
}

impl ProjectDocumentStore {
    /// Opens an existing project root with a positive document-size bound.
    ///
    /// # Errors
    ///
    /// Rejects a missing/non-directory root or a zero byte limit.
    pub fn new(root: impl AsRef<Path>, maximum_bytes: usize) -> Result<Self, DocumentError> {
        if maximum_bytes == 0 {
            return Err(DocumentError::ZeroLimit);
        }
        let root = root.as_ref().canonicalize().map_err(DocumentError::Io)?;
        if !root.is_dir() {
            return Err(DocumentError::RootNotDirectory(root));
        }
        Ok(Self {
            root,
            maximum_bytes,
        })
    }

    /// Returns the canonical project root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the on-disk content revision for a confined project-relative path.
    ///
    /// `Ok(None)` means the file is absent. Used by the Editor watch loop to
    /// detect external edits without silently reloading documents.
    ///
    /// # Errors
    ///
    /// Rejects path escape, oversized files, and I/O failures other than missing.
    pub fn peek_revision(
        &self,
        relative: impl AsRef<Path>,
    ) -> Result<Option<DocumentRevision>, DocumentError> {
        validate_relative(relative.as_ref())?;
        let path = self.root.join(relative.as_ref());
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                Err(DocumentError::SymbolicLink(path))
            }
            Ok(metadata) if metadata.is_file() => {
                let canonical = path.canonicalize().map_err(DocumentError::Io)?;
                if !canonical.starts_with(&self.root) {
                    return Err(DocumentError::PathEscape(relative.as_ref().to_path_buf()));
                }
                self.revision_if_present(&canonical)
            }
            Ok(_) => Err(DocumentError::NotFile(path)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(DocumentError::Io(error)),
        }
    }

    /// Loads one bounded JSON document and records its content revision.
    ///
    /// # Errors
    ///
    /// Rejects path escape, oversized content, I/O, and JSON schema errors.
    pub fn load_json<T: DeserializeOwned>(
        &self,
        relative: impl AsRef<Path>,
    ) -> Result<DocumentSnapshot<T>, DocumentError> {
        let path = self.resolve_existing(relative.as_ref())?;
        let bytes = self.read_bounded(&path)?;
        let revision = DocumentRevision::from_bytes(&bytes);
        let value = serde_json::from_slice(&bytes).map_err(DocumentError::Json)?;
        Ok(DocumentSnapshot { value, revision })
    }

    /// Loads one bounded UTF-8 project file for the Monaco code workspace.
    ///
    /// # Errors
    ///
    /// Applies the same path confinement and byte bound as JSON loading and
    /// additionally rejects non-UTF-8 content.
    pub fn load_text(
        &self,
        relative: impl AsRef<Path>,
    ) -> Result<DocumentSnapshot<String>, DocumentError> {
        let path = self.resolve_existing(relative.as_ref())?;
        let bytes = self.read_bounded(&path)?;
        let revision = DocumentRevision::from_bytes(&bytes);
        let value = String::from_utf8(bytes).map_err(DocumentError::Utf8)?;
        Ok(DocumentSnapshot { value, revision })
    }

    /// Saves JSON only when the external revision still matches `expected`.
    ///
    /// `expected = None` means the editor expects a new file. This method
    /// performs a second revision read immediately before an atomic replace so an
    /// external edit becomes an explicit conflict instead of silent overwrite.
    /// The host should surface [`DocumentError::Conflict`] and ask the user to
    /// reload, compare, or explicitly overwrite.
    ///
    /// # Errors
    ///
    /// Rejects path escape, missing parent directory, conflicts, oversized
    /// serialized data, JSON errors, and I/O failures.
    pub fn save_json<T: Serialize>(
        &self,
        relative: impl AsRef<Path>,
        value: &T,
        expected: Option<DocumentRevision>,
    ) -> Result<DocumentRevision, DocumentError> {
        let bytes = serde_json::to_vec_pretty(value).map_err(DocumentError::Json)?;
        self.save_bytes(relative.as_ref(), &bytes, expected)
    }

    /// Saves one UTF-8 project file with optimistic conflict detection.
    ///
    /// # Errors
    ///
    /// Applies the same confinement, bound, conflict, and I/O policy as
    /// [`Self::save_json`].
    pub fn save_text(
        &self,
        relative: impl AsRef<Path>,
        value: &str,
        expected: Option<DocumentRevision>,
    ) -> Result<DocumentRevision, DocumentError> {
        self.save_bytes(relative.as_ref(), value.as_bytes(), expected)
    }

    fn save_bytes(
        &self,
        relative: &Path,
        bytes: &[u8],
        expected: Option<DocumentRevision>,
    ) -> Result<DocumentRevision, DocumentError> {
        if bytes.len() > self.maximum_bytes {
            return Err(DocumentError::TooLarge {
                actual: bytes.len(),
                maximum: self.maximum_bytes,
            });
        }
        let path = self.resolve_for_write(relative)?;
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(DocumentError::SymbolicLink(path));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(DocumentError::Io(error)),
        }
        let actual = self.revision_if_present(&path)?;
        if actual != expected {
            return Err(DocumentError::Conflict(DocumentConflict {
                expected,
                actual,
            }));
        }

        // The replacement is prepared beside the target. Until `commit`, a
        // crash, short write, or serialization failure leaves the old file
        // intact. Generic filesystem APIs offer no cross-process compare-and-
        // swap, so we verify immediately before commit and document the tiny
        // last-writer race with non-cooperating external editors.
        let mut file = AtomicWriteFile::open(&path).map_err(DocumentError::Io)?;
        file.write_all(bytes).map_err(DocumentError::Io)?;
        file.sync_all().map_err(DocumentError::Io)?;

        let before_commit = self.revision_if_present(&path)?;
        if before_commit != expected {
            return Err(DocumentError::Conflict(DocumentConflict {
                expected,
                actual: before_commit,
            }));
        }
        file.commit().map_err(DocumentError::Io)?;
        Ok(DocumentRevision::from_bytes(bytes))
    }

    fn revision_if_present(&self, path: &Path) -> Result<Option<DocumentRevision>, DocumentError> {
        match fs::metadata(path) {
            Ok(metadata) if metadata.is_file() => self
                .read_bounded(path)
                .map(|bytes| Some(DocumentRevision::from_bytes(&bytes))),
            Ok(_) => Err(DocumentError::NotFile(path.to_path_buf())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(DocumentError::Io(error)),
        }
    }

    fn read_bounded(&self, path: &Path) -> Result<Vec<u8>, DocumentError> {
        let metadata = fs::metadata(path).map_err(DocumentError::Io)?;
        if !metadata.is_file() {
            return Err(DocumentError::NotFile(path.to_path_buf()));
        }
        if metadata.len() > self.maximum_bytes as u64 {
            return Err(DocumentError::TooLarge {
                actual: usize::try_from(metadata.len()).unwrap_or(usize::MAX),
                maximum: self.maximum_bytes,
            });
        }
        let maximum_plus_one = self.maximum_bytes.saturating_add(1);
        let mut bytes = Vec::with_capacity(
            usize::try_from(metadata.len())
                .unwrap_or(self.maximum_bytes)
                .min(self.maximum_bytes),
        );
        fs::File::open(path)
            .map_err(DocumentError::Io)?
            .take(u64::try_from(maximum_plus_one).unwrap_or(u64::MAX))
            .read_to_end(&mut bytes)
            .map_err(DocumentError::Io)?;
        if bytes.len() > self.maximum_bytes {
            return Err(DocumentError::TooLarge {
                actual: bytes.len(),
                maximum: self.maximum_bytes,
            });
        }
        Ok(bytes)
    }

    fn resolve_existing(&self, relative: &Path) -> Result<PathBuf, DocumentError> {
        validate_relative(relative)?;
        let canonical = self
            .root
            .join(relative)
            .canonicalize()
            .map_err(DocumentError::Io)?;
        if !canonical.starts_with(&self.root) {
            return Err(DocumentError::PathEscape(relative.to_path_buf()));
        }
        Ok(canonical)
    }

    fn resolve_for_write(&self, relative: &Path) -> Result<PathBuf, DocumentError> {
        validate_relative(relative)?;
        let joined = self.root.join(relative);
        let parent = joined
            .parent()
            .ok_or_else(|| DocumentError::PathEscape(relative.to_path_buf()))?
            .canonicalize()
            .map_err(DocumentError::Io)?;
        if !parent.starts_with(&self.root) {
            return Err(DocumentError::PathEscape(relative.to_path_buf()));
        }
        Ok(parent.join(
            joined
                .file_name()
                .ok_or_else(|| DocumentError::PathEscape(relative.to_path_buf()))?,
        ))
    }
}

fn validate_relative(path: &Path) -> Result<(), DocumentError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(DocumentError::PathEscape(path.to_path_buf()));
    }
    Ok(())
}

/// Project document storage failure.
#[derive(Debug)]
pub enum DocumentError {
    /// Document byte limit must be positive.
    ZeroLimit,
    /// Canonical project root is not a directory.
    RootNotDirectory(PathBuf),
    /// Requested path may leave the project root.
    PathEscape(PathBuf),
    /// Requested path exists but is not a regular file.
    NotFile(PathBuf),
    /// Symbolic-link documents are rejected instead of replacing their link.
    SymbolicLink(PathBuf),
    /// Encoded or observed file exceeds its configured bound.
    TooLarge {
        /// Observed byte count.
        actual: usize,
        /// Configured maximum.
        maximum: usize,
    },
    /// External state changed since load.
    Conflict(DocumentConflict),
    /// Filesystem operation failed.
    Io(std::io::Error),
    /// JSON encoding or decoding failed.
    Json(serde_json::Error),
    /// Project text file was not valid UTF-8.
    Utf8(std::string::FromUtf8Error),
}

impl fmt::Display for DocumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroLimit => formatter.write_str("document byte limit must be positive"),
            Self::RootNotDirectory(path) => {
                write!(
                    formatter,
                    "project root is not a directory: {}",
                    path.display()
                )
            }
            Self::PathEscape(path) => {
                write!(
                    formatter,
                    "project-relative path escapes its root: {}",
                    path.display()
                )
            }
            Self::NotFile(path) => write!(formatter, "document is not a file: {}", path.display()),
            Self::SymbolicLink(path) => write!(
                formatter,
                "document path is a symbolic link and cannot be replaced: {}",
                path.display()
            ),
            Self::TooLarge { actual, maximum } => {
                write!(formatter, "document has {actual} bytes, limit is {maximum}")
            }
            Self::Conflict(conflict) => write!(
                formatter,
                "document revision conflict: expected {:?}, found {:?}",
                conflict.expected, conflict.actual
            ),
            Self::Io(error) => write!(formatter, "document I/O failed: {error}"),
            Self::Json(error) => write!(formatter, "document JSON failed: {error}"),
            Self::Utf8(error) => write!(formatter, "document text is not UTF-8: {error}"),
        }
    }
}

impl Error for DocumentError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Utf8(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
    struct TestDocument {
        value: u32,
        #[serde(flatten)]
        future: BTreeMap<String, serde_json::Value>,
    }

    fn temporary_root(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("yuyib-editor-{name}-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).expect("create temporary project");
        root
    }

    #[test]
    fn save_detects_external_revision_conflict() {
        let root = temporary_root("conflict");
        let store = ProjectDocumentStore::new(&root, 4096).expect("store");
        let first = TestDocument {
            value: 1,
            future: BTreeMap::new(),
        };
        let revision = store
            .save_json("project.yuyib", &first, None)
            .expect("initial save");
        fs::write(root.join("project.yuyib"), br#"{"value":2}"#).expect("external edit");
        assert!(matches!(
            store.save_json("project.yuyib", &first, Some(revision)),
            Err(DocumentError::Conflict(_))
        ));
        fs::remove_dir_all(root).expect("remove temporary project");
    }

    #[test]
    fn peek_revision_tracks_external_edits() {
        let root = temporary_root("peek");
        let store = ProjectDocumentStore::new(&root, 4096).expect("store");
        assert_eq!(store.peek_revision("missing.json").expect("missing"), None);
        let revision = store
            .save_json(
                "doc.json",
                &TestDocument {
                    value: 1,
                    future: BTreeMap::new(),
                },
                None,
            )
            .expect("save");
        assert_eq!(store.peek_revision("doc.json").expect("peek"), Some(revision));
        fs::write(root.join("doc.json"), br#"{"value":9}"#).expect("external");
        let after = store.peek_revision("doc.json").expect("peek after");
        assert_ne!(after, Some(revision));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn load_is_bounded_and_confined() {
        let root = temporary_root("bounds");
        fs::write(root.join("small.json"), br#"{"value":7,"unknown":true}"#).expect("fixture");
        let store = ProjectDocumentStore::new(&root, 1024).expect("store");
        let loaded = store
            .load_json::<TestDocument>("small.json")
            .expect("load document");
        assert_eq!(loaded.value.value, 7);
        assert_eq!(loaded.value.future["unknown"], true);
        assert!(matches!(
            store.load_json::<TestDocument>("../outside.json"),
            Err(DocumentError::PathEscape(_))
        ));
        fs::remove_dir_all(root).expect("remove temporary project");
    }

    #[test]
    fn text_workspace_round_trips_and_detects_stale_save() {
        let root = temporary_root("text");
        let store = ProjectDocumentStore::new(&root, 4096).expect("store");
        let revision = store
            .save_text("lib.rs", "fn main() {}\n", None)
            .expect("initial text save");
        let loaded = store.load_text("lib.rs").expect("load text");
        assert_eq!(loaded.value, "fn main() {}\n");
        assert_eq!(loaded.revision, revision);
        fs::write(root.join("lib.rs"), "// external\n").expect("external edit");
        assert!(matches!(
            store.save_text("lib.rs", "fn changed() {}\n", Some(revision)),
            Err(DocumentError::Conflict(_))
        ));
        fs::remove_dir_all(root).expect("remove temporary project");
    }
}
