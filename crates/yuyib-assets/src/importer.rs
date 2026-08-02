//! Extensible, typed source-asset importer contracts.

use std::{
    error::Error,
    fmt,
    marker::PhantomData,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use crate::{AssetMetadata, AssetServer, Assets, PreparedAsset};

const DEFAULT_MAX_IMPORTERS: usize = 256;
const DEFAULT_MAX_SOURCE_BYTES: usize = 64 * 1024 * 1024;
const DEFAULT_MAX_PROBE_BYTES: usize = 4 * 1024;
const DEFAULT_MAX_DEPENDENCIES: usize = 4_096;
const DEFAULT_MAX_DIAGNOSTICS: usize = 1_024;
const DEFAULT_MAX_TEXT_BYTES: usize = 4_096;

/// Shared limits enforced before and after untrusted asset data reaches an importer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImporterRegistryLimits {
    /// Maximum registered importers for one neutral output type.
    pub max_importers: usize,
    /// Maximum encoded input size accepted by [`ImporterRegistry::import`].
    pub max_source_bytes: usize,
    /// Maximum prefix exposed to format probes.
    pub max_probe_bytes: usize,
    /// Maximum dependencies returned by one importer.
    pub max_dependencies: usize,
    /// Maximum diagnostics returned by one importer.
    pub max_diagnostics: usize,
    /// Maximum bytes in a URI, diagnostic field, ID, version or media type.
    pub max_text_bytes: usize,
}

impl Default for ImporterRegistryLimits {
    fn default() -> Self {
        Self {
            max_importers: DEFAULT_MAX_IMPORTERS,
            max_source_bytes: DEFAULT_MAX_SOURCE_BYTES,
            max_probe_bytes: DEFAULT_MAX_PROBE_BYTES,
            max_dependencies: DEFAULT_MAX_DEPENDENCIES,
            max_diagnostics: DEFAULT_MAX_DIAGNOSTICS,
            max_text_bytes: DEFAULT_MAX_TEXT_BYTES,
        }
    }
}

/// Invalid registry limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImporterRegistryConfigError {
    /// Every bound must be greater than zero.
    ZeroLimit(&'static str),
}

impl fmt::Display for ImporterRegistryConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroLimit(field) => {
                write!(formatter, "importer registry limit `{field}` is zero")
            }
        }
    }
}

impl Error for ImporterRegistryConfigError {}

impl ImporterRegistryLimits {
    fn validate(self) -> Result<Self, ImporterRegistryConfigError> {
        for (name, value) in [
            ("max_importers", self.max_importers),
            ("max_source_bytes", self.max_source_bytes),
            ("max_probe_bytes", self.max_probe_bytes),
            ("max_dependencies", self.max_dependencies),
            ("max_diagnostics", self.max_diagnostics),
            ("max_text_bytes", self.max_text_bytes),
        ] {
            if value == 0 {
                return Err(ImporterRegistryConfigError::ZeroLimit(name));
            }
        }
        Ok(self)
    }
}

/// Stable metadata used for registration, dispatch and cache invalidation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImporterDescriptor {
    id: String,
    version: String,
    extensions: Vec<String>,
    media_types: Vec<String>,
}

impl ImporterDescriptor {
    /// Creates a descriptor. Validation happens when it is registered.
    #[must_use]
    pub fn new(id: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            version: version.into(),
            extensions: Vec::new(),
            media_types: Vec::new(),
        }
    }

    /// Declares one extension without a leading dot.
    #[must_use]
    pub fn with_extension(mut self, extension: impl Into<String>) -> Self {
        self.extensions.push(extension.into());
        self
    }

    /// Declares one Internet media type such as `model/gltf-binary`.
    #[must_use]
    pub fn with_media_type(mut self, media_type: impl Into<String>) -> Self {
        self.media_types.push(media_type.into());
        self
    }

    /// Stable importer ID.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Importer implementation or format-contract version.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Declared extensions in normalized lowercase form after registration.
    #[must_use]
    pub fn extensions(&self) -> &[String] {
        &self.extensions
    }

    /// Declared media types in normalized lowercase form after registration.
    #[must_use]
    pub fn media_types(&self) -> &[String] {
        &self.media_types
    }
}

/// Why registration failed before any source data was processed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ImporterRegistrationError {
    /// The registry reached its configured plugin bound.
    TooManyImporters {
        /// Configured maximum registry entries.
        maximum: usize,
    },
    /// A stable ID is already registered for this output type.
    DuplicateId(String),
    /// The descriptor violates a stable registry contract.
    InvalidDescriptor {
        /// Invalid descriptor field.
        field: &'static str,
        /// Stable validation explanation.
        reason: String,
    },
}

impl fmt::Display for ImporterRegistrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyImporters { maximum } => {
                write!(
                    formatter,
                    "importer registry is limited to {maximum} entries"
                )
            }
            Self::DuplicateId(id) => write!(formatter, "importer `{id}` is already registered"),
            Self::InvalidDescriptor { field, reason } => {
                write!(formatter, "invalid importer descriptor `{field}`: {reason}")
            }
        }
    }
}

impl Error for ImporterRegistrationError {}

/// Borrowed source bytes and format hints supplied by a trusted resolver.
#[derive(Clone, Copy, Debug)]
pub struct ImportSource<'a> {
    uri: &'a str,
    bytes: &'a [u8],
    extension: Option<&'a str>,
    media_type: Option<&'a str>,
}

impl<'a> ImportSource<'a> {
    /// Creates a source. The registry derives an extension from `uri` if needed.
    #[must_use]
    pub const fn new(uri: &'a str, bytes: &'a [u8]) -> Self {
        Self {
            uri,
            bytes,
            extension: None,
            media_type: None,
        }
    }

    /// Supplies an explicit extension when the URI has none or is virtual.
    #[must_use]
    pub const fn with_extension(mut self, extension: &'a str) -> Self {
        self.extension = Some(extension);
        self
    }

    /// Supplies an optional resolver-validated media type hint.
    #[must_use]
    pub const fn with_media_type(mut self, media_type: &'a str) -> Self {
        self.media_type = Some(media_type);
        self
    }

    /// Logical source URI; this is not permission to access the filesystem.
    #[must_use]
    pub const fn uri(self) -> &'a str {
        self.uri
    }

    /// Complete encoded source bytes.
    #[must_use]
    pub const fn bytes(self) -> &'a [u8] {
        self.bytes
    }

    /// Explicit extension hint, if supplied.
    #[must_use]
    pub const fn extension(self) -> Option<&'a str> {
        self.extension
    }

    /// Explicit media type hint, if supplied.
    #[must_use]
    pub const fn media_type(self) -> Option<&'a str> {
        self.media_type
    }
}

/// Owned source suitable for transfer into a background import task.
#[derive(Clone, Debug)]
pub struct OwnedImportSource {
    uri: String,
    bytes: Arc<[u8]>,
    extension: Option<String>,
    media_type: Option<String>,
}

impl OwnedImportSource {
    /// Creates an owned source without copying when the input is already an `Arc`.
    #[must_use]
    pub fn new(uri: impl Into<String>, bytes: impl Into<Arc<[u8]>>) -> Self {
        Self {
            uri: uri.into(),
            bytes: bytes.into(),
            extension: None,
            media_type: None,
        }
    }

    /// Supplies an explicit extension hint.
    #[must_use]
    pub fn with_extension(mut self, extension: impl Into<String>) -> Self {
        self.extension = Some(extension.into());
        self
    }

    /// Supplies an explicit media type hint.
    #[must_use]
    pub fn with_media_type(mut self, media_type: impl Into<String>) -> Self {
        self.media_type = Some(media_type.into());
        self
    }

    /// Logical source URI.
    #[must_use]
    pub fn uri(&self) -> &str {
        &self.uri
    }

    /// Number of retained encoded bytes.
    #[must_use]
    pub fn byte_len(&self) -> usize {
        self.bytes.len()
    }

    /// Borrows this owned value for registry dispatch.
    #[must_use]
    pub fn as_source(&self) -> ImportSource<'_> {
        ImportSource {
            uri: &self.uri,
            bytes: &self.bytes,
            extension: self.extension.as_deref(),
            media_type: self.media_type.as_deref(),
        }
    }
}

/// Bounded data available during cheap format detection.
#[derive(Clone, Copy, Debug)]
pub struct ImportProbe<'a> {
    /// Logical URI.
    pub uri: &'a str,
    /// Normalized lowercase extension, if known.
    pub extension: Option<&'a str>,
    /// Normalized lowercase media type, if known.
    pub media_type: Option<&'a str>,
    /// At most `ImporterRegistryLimits::max_probe_bytes` leading bytes.
    pub prefix: &'a [u8],
}

/// Importer confidence. Equal best scores are rejected as ambiguous.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ImportMatch {
    /// The importer does not understand the source.
    Unsupported,
    /// A weak hint matched; stronger probes should win.
    Possible,
    /// Extension/media type and structural evidence matched.
    Preferred,
    /// A format magic/version signature matched exactly.
    Exact,
}

/// Cloneable cooperative-cancellation signal for background imports.
///
/// Cancellation never preempts trusted native plugin code. Importers that do
/// substantial parsing should periodically inspect the [`ImportContext`]
/// passed to [`AssetImporter::import_with_context`]. The registry additionally
/// checks the signal before probing and after the plugin returns.
#[derive(Clone, Debug, Default)]
pub struct ImportCancellation {
    cancelled: Arc<AtomicBool>,
}

impl ImportCancellation {
    /// Creates a signal in the running state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cooperative cancellation.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// Returns whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

/// Capability-limited context supplied to a typed importer.
///
/// This deliberately exposes no filesystem, network, GPU or ECS access.
#[derive(Clone, Copy, Debug)]
pub struct ImportContext<'a> {
    cancellation: &'a ImportCancellation,
}

impl ImportContext<'_> {
    /// Returns whether the host requested cooperative cancellation.
    #[must_use]
    pub fn is_cancelled(self) -> bool {
        self.cancellation.is_cancelled()
    }
}

/// Dependency availability contract recorded by an importer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImportDependencyKind {
    /// Import cannot be completed correctly without this dependency.
    Required,
    /// Import remains valid with a documented fallback.
    Optional,
}

/// One logical dependency request. Resolution remains host-controlled.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportDependency {
    /// Project-relative or scheme-qualified logical URI.
    pub uri: String,
    /// Whether a missing dependency is fatal.
    pub kind: ImportDependencyKind,
}

/// Diagnostic severity retained after a successful import.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImportDiagnosticSeverity {
    /// Informational format or conversion note.
    Info,
    /// Recoverable issue with an explicit fallback.
    Warning,
}

/// Structured non-fatal diagnostic emitted by an importer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportDiagnostic {
    /// Stable machine-readable code.
    pub code: String,
    /// Human-readable context.
    pub message: String,
    /// Severity of the retained diagnostic.
    pub severity: ImportDiagnosticSeverity,
}

/// Typed neutral value and bounded metadata emitted by a plugin.
#[derive(Debug)]
pub struct ImporterOutput<T> {
    /// Neutral imported asset.
    pub asset: T,
    /// Logical dependencies; the importer does not open them itself.
    pub dependencies: Vec<ImportDependency>,
    /// Recoverable conversion diagnostics.
    pub diagnostics: Vec<ImportDiagnostic>,
    /// Estimated retained CPU bytes, when cheaply known.
    pub cpu_bytes: Option<u64>,
}

impl<T> ImporterOutput<T> {
    /// Creates output without dependencies or diagnostics.
    #[must_use]
    pub const fn new(asset: T) -> Self {
        Self {
            asset,
            dependencies: Vec::new(),
            diagnostics: Vec::new(),
            cpu_bytes: None,
        }
    }
}

/// Compile-time plugin contract for one neutral output type.
///
/// Settings should be strongly typed fields of the importer value configured
/// before registration. The registry erases only the concrete implementation,
/// never `T` or its validation contract.
pub trait AssetImporter<T>: Send + Sync + 'static {
    /// Structured importer-specific failure.
    type Error: Error + Send + Sync + 'static;

    /// Returns stable identity and advertised format hints.
    fn descriptor(&self) -> ImporterDescriptor;

    /// Performs bounded, allocation-light format detection.
    fn probe(&self, probe: ImportProbe<'_>) -> ImportMatch;

    /// Parses and validates the complete bounded input into neutral data.
    ///
    /// # Errors
    ///
    /// Returns an importer-specific structured error for malformed,
    /// unsupported or out-of-policy source data.
    fn import(&self, source: ImportSource<'_>) -> Result<ImporterOutput<T>, Self::Error>;

    /// Imports with access to cooperative cancellation.
    ///
    /// Existing small importers can rely on this default. Importers with long
    /// loops should override it, check `context.is_cancelled()` between bounded
    /// units of work and return their own cancellation error.
    ///
    /// # Errors
    ///
    /// Returns the same importer-specific structured error contract as
    /// [`Self::import`], including a cooperative cancellation error when an
    /// overriding importer observes a cancelled context.
    fn import_with_context(
        &self,
        source: ImportSource<'_>,
        _context: ImportContext<'_>,
    ) -> Result<ImporterOutput<T>, Self::Error> {
        self.import(source)
    }
}

trait DynImporter<T>: Send + Sync {
    fn descriptor(&self) -> &ImporterDescriptor;
    fn probe(&self, probe: ImportProbe<'_>) -> ImportMatch;
    fn import(
        &self,
        source: ImportSource<'_>,
        context: ImportContext<'_>,
    ) -> Result<ImporterOutput<T>, Box<dyn Error + Send + Sync>>;
}

struct RegisteredImporter<I> {
    implementation: I,
    descriptor: ImporterDescriptor,
}

impl<T, I> DynImporter<T> for RegisteredImporter<I>
where
    I: AssetImporter<T>,
{
    fn descriptor(&self) -> &ImporterDescriptor {
        &self.descriptor
    }

    fn probe(&self, probe: ImportProbe<'_>) -> ImportMatch {
        self.implementation.probe(probe)
    }

    fn import(
        &self,
        source: ImportSource<'_>,
        context: ImportContext<'_>,
    ) -> Result<ImporterOutput<T>, Box<dyn Error + Send + Sync>> {
        self.implementation
            .import_with_context(source, context)
            .map_err(|error| Box::new(error) as Box<dyn Error + Send + Sync>)
    }
}

/// Stable selected importer identity retained in a successful result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImporterIdentity {
    /// Stable importer ID.
    pub id: String,
    /// Importer implementation/contract version.
    pub version: String,
}

/// Registry-validated result ready for a cooker or [`AssetServer`].
#[derive(Debug)]
pub struct ImportResult<T> {
    /// Neutral typed value.
    pub asset: T,
    /// Importer that won deterministic probing.
    pub importer: ImporterIdentity,
    /// Validated logical dependencies.
    pub dependencies: Vec<ImportDependency>,
    /// Validated non-fatal diagnostics.
    pub diagnostics: Vec<ImportDiagnostic>,
    /// Estimated retained CPU bytes.
    pub cpu_bytes: Option<u64>,
}

impl<T> ImportResult<T> {
    /// Converts the result to an atomically publishable asset value.
    #[must_use]
    pub fn into_prepared_asset(self, source: impl Into<String>) -> PreparedAsset<T> {
        let diagnostics = self
            .diagnostics
            .iter()
            .map(|diagnostic| {
                format!(
                    "{:?} {}: {}",
                    diagnostic.severity, diagnostic.code, diagnostic.message
                )
            })
            .collect();
        PreparedAsset::new(
            self.asset,
            AssetMetadata {
                source: Some(source.into()),
                importer_version: Some(format!("{}@{}", self.importer.id, self.importer.version)),
                dependencies: self
                    .dependencies
                    .into_iter()
                    .map(|dependency| dependency.uri)
                    .collect(),
                cpu_bytes: self.cpu_bytes,
                diagnostics,
                ..AssetMetadata::default()
            },
        )
    }
}

/// Dispatch/import failure with importer identity preserved.
#[derive(Debug)]
pub enum ImportError {
    /// The host requested cooperative cancellation.
    Cancelled,
    /// Encoded source exceeds the registry trust boundary.
    SourceTooLarge {
        /// Actual encoded bytes.
        actual: usize,
        /// Configured maximum encoded bytes.
        maximum: usize,
    },
    /// A source hint exceeds the configured text bound.
    SourceTextTooLong {
        /// Oversized source field.
        field: &'static str,
        /// Configured maximum text bytes.
        maximum: usize,
    },
    /// No registered importer accepted the source.
    NoImporterFound {
        /// Source URI used for diagnostics.
        uri: String,
    },
    /// Multiple importers returned the same best confidence.
    Ambiguous {
        /// Equally ranked importer IDs.
        importers: Vec<String>,
    },
    /// The selected importer returned a structured error.
    ImporterFailed {
        /// Selected importer ID.
        importer: String,
        /// Importer-specific structured failure.
        source: Box<dyn Error + Send + Sync>,
    },
    /// A plugin returned output outside the shared registry contract.
    ContractViolation {
        /// Importer that returned invalid output.
        importer: String,
        /// Violated registry constraint.
        reason: String,
    },
}

impl fmt::Display for ImportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("asset import was cancelled"),
            Self::SourceTooLarge { actual, maximum } => {
                write!(formatter, "source has {actual} bytes, maximum is {maximum}")
            }
            Self::SourceTextTooLong { field, maximum } => {
                write!(formatter, "source `{field}` exceeds {maximum} bytes")
            }
            Self::NoImporterFound { uri } => write!(formatter, "no importer accepted `{uri}`"),
            Self::Ambiguous { importers } => {
                write!(
                    formatter,
                    "ambiguous importer match: {}",
                    importers.join(", ")
                )
            }
            Self::ImporterFailed { importer, source } => {
                write!(formatter, "importer `{importer}` failed: {source}")
            }
            Self::ContractViolation { importer, reason } => {
                write!(
                    formatter,
                    "importer `{importer}` violated its contract: {reason}"
                )
            }
        }
    }
}

impl Error for ImportError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ImporterFailed { source, .. } => Some(source.as_ref()),
            Self::Cancelled
            | Self::SourceTooLarge { .. }
            | Self::SourceTextTooLong { .. }
            | Self::NoImporterFound { .. }
            | Self::Ambiguous { .. }
            | Self::ContractViolation { .. } => None,
        }
    }
}

/// Deterministic plugin registry for one safe neutral output type `T`.
pub struct ImporterRegistry<T> {
    limits: ImporterRegistryLimits,
    importers: Vec<Box<dyn DynImporter<T>>>,
    marker: PhantomData<fn() -> T>,
}

impl<T: 'static> fmt::Debug for ImporterRegistry<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImporterRegistry")
            .field("limits", &self.limits)
            .field("importers", &self.importer_ids().collect::<Vec<_>>())
            .finish()
    }
}

impl<T: 'static> Default for ImporterRegistry<T> {
    fn default() -> Self {
        Self::new(ImporterRegistryLimits::default()).expect("default importer limits are valid")
    }
}

impl<T: 'static> ImporterRegistry<T> {
    /// Creates an empty registry with an explicit trust boundary.
    ///
    /// # Errors
    ///
    /// Returns an error when any limit is zero.
    pub fn new(limits: ImporterRegistryLimits) -> Result<Self, ImporterRegistryConfigError> {
        Ok(Self {
            limits: limits.validate()?,
            importers: Vec::new(),
            marker: PhantomData,
        })
    }

    /// Registers a compile-time plugin after validating stable metadata.
    ///
    /// # Errors
    ///
    /// Rejects duplicate IDs, malformed descriptors and registry overflow.
    pub fn register<I>(&mut self, importer: I) -> Result<(), ImporterRegistrationError>
    where
        I: AssetImporter<T>,
    {
        if self.importers.len() >= self.limits.max_importers {
            return Err(ImporterRegistrationError::TooManyImporters {
                maximum: self.limits.max_importers,
            });
        }
        let mut descriptor = importer.descriptor();
        validate_descriptor(&mut descriptor, self.limits.max_text_bytes)?;
        if self
            .importers
            .iter()
            .any(|entry| entry.descriptor().id == descriptor.id)
        {
            return Err(ImporterRegistrationError::DuplicateId(descriptor.id));
        }
        self.importers.push(Box::new(RegisteredImporter {
            implementation: importer,
            descriptor,
        }));
        Ok(())
    }

    /// Returns registered IDs in deterministic registration order.
    pub fn importer_ids(&self) -> impl Iterator<Item = &str> {
        self.importers.iter().map(|entry| entry.descriptor().id())
    }

    /// Probes, dispatches, imports and validates one source synchronously.
    ///
    /// Use this low-level call inside a bounded worker. For the usual high-level
    /// path use [`AssetServer::try_import_bytes`].
    ///
    /// # Errors
    ///
    /// Returns bounded input, dispatch, plugin or output-contract failures.
    pub fn import(&self, source: ImportSource<'_>) -> Result<ImportResult<T>, ImportError> {
        self.import_with_cancellation(source, &ImportCancellation::new())
    }

    /// Imports with a cooperative cancellation signal shared with the host.
    ///
    /// # Errors
    ///
    /// Returns [`ImportError::Cancelled`] when cancellation is observed before,
    /// during or immediately after plugin execution, in addition to the errors
    /// documented by [`Self::import`].
    pub fn import_with_cancellation(
        &self,
        source: ImportSource<'_>,
        cancellation: &ImportCancellation,
    ) -> Result<ImportResult<T>, ImportError> {
        if cancellation.is_cancelled() {
            return Err(ImportError::Cancelled);
        }
        self.validate_source(source)?;
        let derived_extension = source
            .extension()
            .map(normalize_hint)
            .or_else(|| extension_from_uri(source.uri()).map(normalize_hint));
        let media_type = source.media_type().map(normalize_hint);
        let prefix_len = source.bytes().len().min(self.limits.max_probe_bytes);
        let probe = ImportProbe {
            uri: source.uri(),
            extension: derived_extension.as_deref(),
            media_type: media_type.as_deref(),
            prefix: &source.bytes()[..prefix_len],
        };

        let mut best = ImportMatch::Unsupported;
        let mut matches = Vec::new();
        for (index, importer) in self.importers.iter().enumerate() {
            if cancellation.is_cancelled() {
                return Err(ImportError::Cancelled);
            }
            let score = importer.probe(probe);
            if score > best {
                best = score;
                matches.clear();
                matches.push(index);
            } else if score == best && score != ImportMatch::Unsupported {
                matches.push(index);
            }
        }
        if best == ImportMatch::Unsupported {
            return Err(ImportError::NoImporterFound {
                uri: source.uri().to_owned(),
            });
        }
        if matches.len() != 1 {
            return Err(ImportError::Ambiguous {
                importers: matches
                    .into_iter()
                    .map(|index| self.importers[index].descriptor().id.clone())
                    .collect(),
            });
        }

        let importer = &self.importers[matches[0]];
        let descriptor = importer.descriptor();
        let context = ImportContext { cancellation };
        let output = match importer.import(source, context) {
            Ok(output) => output,
            Err(_) if cancellation.is_cancelled() => return Err(ImportError::Cancelled),
            Err(source) => {
                return Err(ImportError::ImporterFailed {
                    importer: descriptor.id.clone(),
                    source,
                });
            }
        };
        if cancellation.is_cancelled() {
            return Err(ImportError::Cancelled);
        }
        validate_output(descriptor.id(), &output, self.limits)?;
        Ok(ImportResult {
            asset: output.asset,
            importer: ImporterIdentity {
                id: descriptor.id.clone(),
                version: descriptor.version.clone(),
            },
            dependencies: output.dependencies,
            diagnostics: output.diagnostics,
            cpu_bytes: output.cpu_bytes,
        })
    }

    fn validate_source(&self, source: ImportSource<'_>) -> Result<(), ImportError> {
        if source.bytes().len() > self.limits.max_source_bytes {
            return Err(ImportError::SourceTooLarge {
                actual: source.bytes().len(),
                maximum: self.limits.max_source_bytes,
            });
        }
        for (field, value) in [
            ("uri", Some(source.uri())),
            ("extension", source.extension()),
            ("media_type", source.media_type()),
        ] {
            if value.is_some_and(|value| value.len() > self.limits.max_text_bytes) {
                return Err(ImportError::SourceTextTooLong {
                    field,
                    maximum: self.limits.max_text_bytes,
                });
            }
        }
        Ok(())
    }
}

impl<T> AssetServer<T, ImportError>
where
    T: Send + 'static,
{
    /// Imports already-read bytes on the server's bounded worker pool.
    ///
    /// The source resolver remains explicit: read or download bytes under the
    /// host's path/network policy, then hand ownership to this method. Importer
    /// metadata replaces the loading metadata when publication succeeds.
    ///
    /// # Errors
    ///
    /// Returns a submission error when the bounded task queue is full or closed.
    pub fn try_import_bytes(
        &mut self,
        assets: &mut Assets<T>,
        registry: Arc<ImporterRegistry<T>>,
        label: impl Into<String>,
        source: OwnedImportSource,
    ) -> Result<crate::AssetId<T>, crate::AssetLoadSubmitError> {
        self.try_import_bytes_cancellable(assets, registry, label, source)
            .map(|(handle, _)| handle)
    }

    /// Starts a background import and returns its stable handle plus a
    /// cooperative cancellation signal.
    ///
    /// Cancelling transitions the asset to `Failed` when the worker observes
    /// the request. Native importer code is not forcefully preempted.
    ///
    /// # Errors
    ///
    /// Returns a submission error when the bounded task queue is full or closed.
    pub fn try_import_bytes_cancellable(
        &mut self,
        assets: &mut Assets<T>,
        registry: Arc<ImporterRegistry<T>>,
        label: impl Into<String>,
        source: OwnedImportSource,
    ) -> Result<(crate::AssetId<T>, ImportCancellation), crate::AssetLoadSubmitError> {
        let uri = source.uri().to_owned();
        let initial_metadata = AssetMetadata {
            source: Some(uri.clone()),
            cpu_bytes: u64::try_from(source.byte_len()).ok(),
            ..AssetMetadata::default()
        };
        let cancellation = ImportCancellation::new();
        let worker_cancellation = cancellation.clone();
        let handle = self.try_load_prepared(assets, label, initial_metadata, move |reporter| {
            reporter.set_total_work(source.byte_len() as u64);
            reporter.set_completed_work(source.byte_len() as u64);
            reporter.decoding();
            registry
                .import_with_cancellation(source.as_source(), &worker_cancellation)
                .map(|result| result.into_prepared_asset(uri))
        })?;
        Ok((handle, cancellation))
    }
}

fn normalize_hint(value: &str) -> String {
    value.trim().trim_start_matches('.').to_ascii_lowercase()
}

fn extension_from_uri(uri: &str) -> Option<&str> {
    let path = uri.split(['?', '#']).next().unwrap_or(uri);
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let (_, extension) = name.rsplit_once('.')?;
    (!extension.is_empty()).then_some(extension)
}

fn validate_descriptor(
    descriptor: &mut ImporterDescriptor,
    max_text_bytes: usize,
) -> Result<(), ImporterRegistrationError> {
    if descriptor.id.is_empty()
        || !descriptor.id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_.".contains(&byte)
        })
    {
        return Err(ImporterRegistrationError::InvalidDescriptor {
            field: "id",
            reason: "use non-empty lowercase ASCII letters, digits, '-', '_' or '.'".to_owned(),
        });
    }
    if descriptor.id.len() > max_text_bytes || descriptor.version.len() > max_text_bytes {
        return Err(ImporterRegistrationError::InvalidDescriptor {
            field: "id/version",
            reason: format!("value exceeds {max_text_bytes} bytes"),
        });
    }
    if descriptor.version.trim().is_empty() {
        return Err(ImporterRegistrationError::InvalidDescriptor {
            field: "version",
            reason: "version must not be empty".to_owned(),
        });
    }
    for extension in &mut descriptor.extensions {
        *extension = normalize_hint(extension);
        if extension.is_empty()
            || extension.len() > max_text_bytes
            || !extension
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return Err(ImporterRegistrationError::InvalidDescriptor {
                field: "extension",
                reason: "use lowercase-compatible ASCII without a leading dot".to_owned(),
            });
        }
    }
    descriptor.extensions.sort();
    descriptor.extensions.dedup();
    for media_type in &mut descriptor.media_types {
        *media_type = media_type.trim().to_ascii_lowercase();
        if media_type.is_empty()
            || media_type.len() > max_text_bytes
            || !media_type.contains('/')
            || !media_type.is_ascii()
        {
            return Err(ImporterRegistrationError::InvalidDescriptor {
                field: "media_type",
                reason: "expected a bounded ASCII type/subtype value".to_owned(),
            });
        }
    }
    descriptor.media_types.sort();
    descriptor.media_types.dedup();
    Ok(())
}

fn validate_output<T>(
    importer: &str,
    output: &ImporterOutput<T>,
    limits: ImporterRegistryLimits,
) -> Result<(), ImportError> {
    if output.dependencies.len() > limits.max_dependencies {
        return Err(contract_error(
            importer,
            format!(
                "returned {} dependencies, maximum is {}",
                output.dependencies.len(),
                limits.max_dependencies
            ),
        ));
    }
    if output.diagnostics.len() > limits.max_diagnostics {
        return Err(contract_error(
            importer,
            format!(
                "returned {} diagnostics, maximum is {}",
                output.diagnostics.len(),
                limits.max_diagnostics
            ),
        ));
    }
    for dependency in &output.dependencies {
        if dependency.uri.is_empty() || dependency.uri.len() > limits.max_text_bytes {
            return Err(contract_error(
                importer,
                "dependency URI is empty or exceeds the text bound".to_owned(),
            ));
        }
    }
    for diagnostic in &output.diagnostics {
        if diagnostic.code.is_empty()
            || diagnostic.code.len() > limits.max_text_bytes
            || diagnostic.message.len() > limits.max_text_bytes
        {
            return Err(contract_error(
                importer,
                "diagnostic code/message violates the text bound".to_owned(),
            ));
        }
    }
    Ok(())
}

fn contract_error(importer: &str, reason: String) -> ImportError {
    ImportError::ContractViolation {
        importer: importer.to_owned(),
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Eq, PartialEq)]
    struct Lines(Vec<String>);

    #[derive(Debug)]
    struct LinesError;

    impl fmt::Display for LinesError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("not UTF-8")
        }
    }

    impl Error for LinesError {}

    struct LinesImporter;

    struct Other;

    impl AssetImporter<Lines> for Other {
        type Error = LinesError;

        fn descriptor(&self) -> ImporterDescriptor {
            ImporterDescriptor::new("example.other", "1")
        }

        fn probe(&self, _probe: ImportProbe<'_>) -> ImportMatch {
            ImportMatch::Exact
        }

        fn import(&self, _source: ImportSource<'_>) -> Result<ImporterOutput<Lines>, Self::Error> {
            Ok(ImporterOutput::new(Lines(Vec::new())))
        }
    }

    impl AssetImporter<Lines> for LinesImporter {
        type Error = LinesError;

        fn descriptor(&self) -> ImporterDescriptor {
            ImporterDescriptor::new("example.lines", "1").with_extension("LINES")
        }

        fn probe(&self, probe: ImportProbe<'_>) -> ImportMatch {
            if probe.prefix.starts_with(b"LINES\n") {
                ImportMatch::Exact
            } else if probe.extension == Some("lines") {
                ImportMatch::Possible
            } else {
                ImportMatch::Unsupported
            }
        }

        fn import(&self, source: ImportSource<'_>) -> Result<ImporterOutput<Lines>, Self::Error> {
            let text = std::str::from_utf8(source.bytes()).map_err(|_| LinesError)?;
            let lines = text.lines().skip(1).map(str::to_owned).collect::<Vec<_>>();
            let mut output = ImporterOutput::new(Lines(lines));
            output.cpu_bytes = u64::try_from(source.bytes().len()).ok();
            Ok(output)
        }
    }

    #[test]
    fn imports_typed_output_and_normalizes_extension() {
        let mut registry = ImporterRegistry::default();
        registry.register(LinesImporter).expect("register importer");
        let result = registry
            .import(ImportSource::new("dialogue.LINES", b"LINES\nhello\nworld"))
            .expect("import lines");
        assert_eq!(
            result.asset,
            Lines(vec!["hello".to_owned(), "world".to_owned()])
        );
        assert_eq!(result.importer.id, "example.lines");
    }

    #[test]
    fn duplicate_ids_and_ambiguous_matches_are_rejected() {
        let mut registry = ImporterRegistry::default();
        registry.register(LinesImporter).expect("first importer");
        assert!(matches!(
            registry.register(LinesImporter),
            Err(ImporterRegistrationError::DuplicateId(_))
        ));

        registry.register(Other).expect("other importer");
        assert!(matches!(
            registry.import(ImportSource::new("a.lines", b"LINES\na")),
            Err(ImportError::Ambiguous { .. })
        ));
    }

    #[test]
    fn source_and_output_contracts_are_bounded() {
        let limits = ImporterRegistryLimits {
            max_source_bytes: 4,
            ..ImporterRegistryLimits::default()
        };
        let mut registry = ImporterRegistry::new(limits).expect("valid limits");
        registry.register(LinesImporter).expect("register importer");
        assert!(matches!(
            registry.import(ImportSource::new("x.lines", b"LINES\n")),
            Err(ImportError::SourceTooLarge { .. })
        ));
    }
}
