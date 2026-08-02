use std::{
    collections::BTreeMap,
    error::Error,
    fmt,
    io::{self, Write},
    num::{NonZeroU32, NonZeroU64},
    sync::Arc,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ComponentSchemaId, ImportSettingsSchemaId, SchemaVersion};

type MigrationFn =
    Arc<dyn Fn(Value) -> Result<Value, MigrationTransformError> + Send + Sync + 'static>;

/// Strongly separated persisted-schema namespace used in migration diagnostics.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(tag = "kind", content = "schema", rename_all = "snake_case")]
pub enum MigrationKey {
    /// Persisted ECS component schema.
    Component(ComponentSchemaId),
    /// Persisted importer-settings schema.
    ImportSettings(ImportSettingsSchemaId),
}

/// Serializable evidence that one executable adjacent edge is installed.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MigrationEdgeDescriptor {
    /// Strongly namespaced persisted schema.
    pub key: MigrationKey,
    /// Edge source version.
    pub from: SchemaVersion,
    /// Adjacent edge destination version.
    pub to: SchemaVersion,
}

impl fmt::Display for MigrationKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Component(id) => write!(formatter, "component:{id}"),
            Self::ImportSettings(id) => write!(formatter, "import-settings:{id}"),
        }
    }
}

/// Hard limits applied while validating and executing one migration chain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MigrationLimits {
    /// Maximum adjacent migration edges traversed in one operation.
    pub max_steps: NonZeroU32,
    /// Maximum serialized JSON bytes accepted initially and after every edge.
    pub max_serialized_output_bytes: NonZeroU64,
}

impl MigrationLimits {
    /// Creates non-zero migration limits.
    ///
    /// # Errors
    ///
    /// Returns [`MigrationError::InvalidLimits`] when either limit is zero.
    pub fn new(max_steps: u32, max_serialized_output_bytes: u64) -> Result<Self, MigrationError> {
        let Some(max_steps) = NonZeroU32::new(max_steps) else {
            return Err(MigrationError::InvalidLimits { field: "max_steps" });
        };
        let Some(max_serialized_output_bytes) = NonZeroU64::new(max_serialized_output_bytes) else {
            return Err(MigrationError::InvalidLimits {
                field: "max_serialized_output_bytes",
            });
        };
        Ok(Self {
            max_steps,
            max_serialized_output_bytes,
        })
    }
}

/// Error returned by one schema-specific `Value -> Value` transform.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MigrationTransformError {
    message: String,
}

impl MigrationTransformError {
    /// Creates a transform failure with a user-facing explanation.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Returns the transform failure explanation.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for MigrationTransformError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for MigrationTransformError {}

/// One executable adjacent component-schema migration edge.
#[derive(Clone)]
pub struct ComponentMigration {
    schema: ComponentSchemaId,
    from: SchemaVersion,
    to: SchemaVersion,
    transform: MigrationFn,
}

impl ComponentMigration {
    /// Creates an executable `from -> from + 1` component migration.
    ///
    /// # Errors
    ///
    /// Returns [`MigrationError::NonAdjacentEdge`] when versions are not
    /// strictly adjacent.
    pub fn new<F>(
        schema: ComponentSchemaId,
        from: SchemaVersion,
        to: SchemaVersion,
        transform: F,
    ) -> Result<Self, MigrationError>
    where
        F: Fn(Value) -> Result<Value, MigrationTransformError> + Send + Sync + 'static,
    {
        let key = MigrationKey::Component(schema.clone());
        validate_adjacent(&key, from, to)?;
        Ok(Self {
            schema,
            from,
            to,
            transform: Arc::new(transform),
        })
    }

    /// Returns the migrated component schema.
    #[must_use]
    pub const fn schema(&self) -> &ComponentSchemaId {
        &self.schema
    }

    /// Returns the source version.
    #[must_use]
    pub const fn from(&self) -> SchemaVersion {
        self.from
    }

    /// Returns the adjacent destination version.
    #[must_use]
    pub const fn to(&self) -> SchemaVersion {
        self.to
    }
}

impl fmt::Debug for ComponentMigration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ComponentMigration")
            .field("schema", &self.schema)
            .field("from", &self.from)
            .field("to", &self.to)
            .finish_non_exhaustive()
    }
}

/// One executable adjacent importer-settings migration edge.
#[derive(Clone)]
pub struct ImportSettingsMigration {
    schema: ImportSettingsSchemaId,
    from: SchemaVersion,
    to: SchemaVersion,
    transform: MigrationFn,
}

impl ImportSettingsMigration {
    /// Creates an executable `from -> from + 1` importer-settings migration.
    ///
    /// # Errors
    ///
    /// Returns [`MigrationError::NonAdjacentEdge`] when versions are not
    /// strictly adjacent.
    pub fn new<F>(
        schema: ImportSettingsSchemaId,
        from: SchemaVersion,
        to: SchemaVersion,
        transform: F,
    ) -> Result<Self, MigrationError>
    where
        F: Fn(Value) -> Result<Value, MigrationTransformError> + Send + Sync + 'static,
    {
        let key = MigrationKey::ImportSettings(schema.clone());
        validate_adjacent(&key, from, to)?;
        Ok(Self {
            schema,
            from,
            to,
            transform: Arc::new(transform),
        })
    }

    /// Returns the migrated importer-settings schema.
    #[must_use]
    pub const fn schema(&self) -> &ImportSettingsSchemaId {
        &self.schema
    }

    /// Returns the source version.
    #[must_use]
    pub const fn from(&self) -> SchemaVersion {
        self.from
    }

    /// Returns the adjacent destination version.
    #[must_use]
    pub const fn to(&self) -> SchemaVersion {
        self.to
    }
}

impl fmt::Debug for ImportSettingsMigration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImportSettingsMigration")
            .field("schema", &self.schema)
            .field("from", &self.from)
            .field("to", &self.to)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
struct MigrationEdge {
    to: SchemaVersion,
    transform: MigrationFn,
}

/// Deterministic registry of executable component and importer-settings chains.
#[derive(Default)]
pub struct MigrationRegistry {
    edges: BTreeMap<(MigrationKey, SchemaVersion), MigrationEdge>,
}

impl MigrationRegistry {
    /// Creates an empty migration registry.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            edges: BTreeMap::new(),
        }
    }

    /// Returns deterministic machine-readable evidence for coverage/CI.
    #[must_use]
    pub fn descriptors(&self) -> Vec<MigrationEdgeDescriptor> {
        self.edges
            .iter()
            .map(|((key, from), edge)| MigrationEdgeDescriptor {
                key: key.clone(),
                from: *from,
                to: edge.to,
            })
            .collect()
    }

    /// Registers one executable component migration edge.
    ///
    /// # Errors
    ///
    /// Duplicate `(schema, from-version)` edges are a hard error.
    pub fn register_component(
        &mut self,
        migration: ComponentMigration,
    ) -> Result<(), MigrationError> {
        self.register(
            MigrationKey::Component(migration.schema),
            migration.from,
            migration.to,
            migration.transform,
        )
    }

    /// Registers one executable importer-settings migration edge.
    ///
    /// # Errors
    ///
    /// Duplicate `(schema, from-version)` edges are a hard error. Component and
    /// importer-settings namespaces never collide, even when their text IDs do.
    pub fn register_import_settings(
        &mut self,
        migration: ImportSettingsMigration,
    ) -> Result<(), MigrationError> {
        self.register(
            MigrationKey::ImportSettings(migration.schema),
            migration.from,
            migration.to,
            migration.transform,
        )
    }

    /// Validates a complete deterministic component path without executing it.
    ///
    /// # Errors
    ///
    /// Returns a structured future-version, step-limit, or gap error.
    pub fn validate_component_path(
        &self,
        schema: &ComponentSchemaId,
        source: SchemaVersion,
        current: SchemaVersion,
        limits: MigrationLimits,
    ) -> Result<(), MigrationError> {
        self.validate_path(
            &MigrationKey::Component(schema.clone()),
            source,
            current,
            limits,
        )
    }

    /// Validates a complete deterministic importer-settings path without executing it.
    ///
    /// # Errors
    ///
    /// Returns a structured future-version, step-limit, or gap error.
    pub fn validate_import_settings_path(
        &self,
        schema: &ImportSettingsSchemaId,
        source: SchemaVersion,
        current: SchemaVersion,
        limits: MigrationLimits,
    ) -> Result<(), MigrationError> {
        self.validate_path(
            &MigrationKey::ImportSettings(schema.clone()),
            source,
            current,
            limits,
        )
    }

    /// Executes an ordered, bounded component migration chain.
    ///
    /// # Errors
    ///
    /// Returns a structured path, transform, serialization, or output-bound error.
    pub fn migrate_component(
        &self,
        schema: &ComponentSchemaId,
        source: SchemaVersion,
        current: SchemaVersion,
        payload: Value,
        limits: MigrationLimits,
    ) -> Result<Value, MigrationError> {
        self.migrate(
            &MigrationKey::Component(schema.clone()),
            source,
            current,
            payload,
            limits,
        )
    }

    /// Executes an ordered, bounded importer-settings migration chain.
    ///
    /// # Errors
    ///
    /// Returns a structured path, transform, serialization, or output-bound error.
    pub fn migrate_import_settings(
        &self,
        schema: &ImportSettingsSchemaId,
        source: SchemaVersion,
        current: SchemaVersion,
        payload: Value,
        limits: MigrationLimits,
    ) -> Result<Value, MigrationError> {
        self.migrate(
            &MigrationKey::ImportSettings(schema.clone()),
            source,
            current,
            payload,
            limits,
        )
    }

    fn register(
        &mut self,
        key: MigrationKey,
        from: SchemaVersion,
        to: SchemaVersion,
        transform: MigrationFn,
    ) -> Result<(), MigrationError> {
        let edge_key = (key.clone(), from);
        if self.edges.contains_key(&edge_key) {
            return Err(MigrationError::DuplicateEdge { key, from });
        }
        self.edges.insert(edge_key, MigrationEdge { to, transform });
        Ok(())
    }

    fn validate_path(
        &self,
        key: &MigrationKey,
        source: SchemaVersion,
        current: SchemaVersion,
        limits: MigrationLimits,
    ) -> Result<(), MigrationError> {
        validate_direction_and_steps(key, source, current, limits)?;
        let mut version = source;
        while version != current {
            let Some(edge) = self.edges.get(&(key.clone(), version)) else {
                return Err(MigrationError::MissingEdge {
                    key: key.clone(),
                    from: version,
                    current,
                });
            };
            version = edge.to;
        }
        Ok(())
    }

    fn migrate(
        &self,
        key: &MigrationKey,
        source: SchemaVersion,
        current: SchemaVersion,
        mut payload: Value,
        limits: MigrationLimits,
    ) -> Result<Value, MigrationError> {
        self.validate_path(key, source, current, limits)?;
        ensure_output_bound(key, source, &payload, limits)?;

        let mut version = source;
        while version != current {
            let Some(edge) = self.edges.get(&(key.clone(), version)) else {
                return Err(MigrationError::MissingEdge {
                    key: key.clone(),
                    from: version,
                    current,
                });
            };
            let to = edge.to;
            payload = (edge.transform)(payload).map_err(|error| MigrationError::Transform {
                key: key.clone(),
                from: version,
                to,
                error,
            })?;
            ensure_output_bound(key, to, &payload, limits)?;
            version = to;
        }
        Ok(payload)
    }
}

fn validate_adjacent(
    key: &MigrationKey,
    from: SchemaVersion,
    to: SchemaVersion,
) -> Result<(), MigrationError> {
    if from.get().checked_add(1) == Some(to.get()) {
        Ok(())
    } else {
        Err(MigrationError::NonAdjacentEdge {
            key: key.clone(),
            from,
            to,
        })
    }
}

fn validate_direction_and_steps(
    key: &MigrationKey,
    source: SchemaVersion,
    current: SchemaVersion,
    limits: MigrationLimits,
) -> Result<(), MigrationError> {
    let Some(required_steps) = current.get().checked_sub(source.get()) else {
        return Err(MigrationError::FutureVersion {
            key: key.clone(),
            source,
            current,
        });
    };
    if required_steps > limits.max_steps.get() {
        return Err(MigrationError::StepLimitExceeded {
            key: key.clone(),
            required_steps,
            max_steps: limits.max_steps.get(),
        });
    }
    Ok(())
}

fn ensure_output_bound(
    key: &MigrationKey,
    version: SchemaVersion,
    payload: &Value,
    limits: MigrationLimits,
) -> Result<(), MigrationError> {
    let mut writer = BoundedCountWriter::new(limits.max_serialized_output_bytes.get());
    if let Err(error) = serde_json::to_writer(&mut writer, payload) {
        if writer.exceeded {
            return Err(MigrationError::OutputTooLarge {
                key: key.clone(),
                version,
                max_bytes: writer.limit,
            });
        }
        return Err(MigrationError::Serialization {
            key: key.clone(),
            version,
            message: error.to_string(),
        });
    }
    Ok(())
}

struct BoundedCountWriter {
    written: u64,
    limit: u64,
    exceeded: bool,
}

impl BoundedCountWriter {
    const fn new(limit: u64) -> Self {
        Self {
            written: 0,
            limit,
            exceeded: false,
        }
    }
}

impl Write for BoundedCountWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let byte_count = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if self
            .written
            .checked_add(byte_count)
            .is_none_or(|total| total > self.limit)
        {
            self.exceeded = true;
            return Err(io::Error::other("migration output exceeded byte limit"));
        }
        self.written += byte_count;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Structured migration registration, validation, or execution failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MigrationError {
    /// A configured hard limit was zero.
    InvalidLimits {
        /// Invalid limit field.
        field: &'static str,
    },
    /// A migration attempted to skip, repeat, or reverse a schema version.
    NonAdjacentEdge {
        /// Persisted schema namespace and ID.
        key: MigrationKey,
        /// Edge source.
        from: SchemaVersion,
        /// Invalid edge destination.
        to: SchemaVersion,
    },
    /// The same schema and source version were registered twice.
    DuplicateEdge {
        /// Persisted schema namespace and ID.
        key: MigrationKey,
        /// Duplicate edge source.
        from: SchemaVersion,
    },
    /// Persisted data is newer than the installed schema.
    FutureVersion {
        /// Persisted schema namespace and ID.
        key: MigrationKey,
        /// Version found in the document.
        source: SchemaVersion,
        /// Latest version understood by the application.
        current: SchemaVersion,
    },
    /// A complete adjacent chain would exceed configured work bounds.
    StepLimitExceeded {
        /// Persisted schema namespace and ID.
        key: MigrationKey,
        /// Adjacent edges required by version arithmetic.
        required_steps: u32,
        /// Configured maximum edges.
        max_steps: u32,
    },
    /// No executable edge continues the chain to the current version.
    MissingEdge {
        /// Persisted schema namespace and ID.
        key: MigrationKey,
        /// Version at which the gap starts.
        from: SchemaVersion,
        /// Requested current version.
        current: SchemaVersion,
    },
    /// A registered transform rejected its input.
    Transform {
        /// Persisted schema namespace and ID.
        key: MigrationKey,
        /// Transform source version.
        from: SchemaVersion,
        /// Transform destination version.
        to: SchemaVersion,
        /// Schema-specific failure.
        error: MigrationTransformError,
    },
    /// Initial or transformed JSON exceeds its serialized output budget.
    OutputTooLarge {
        /// Persisted schema namespace and ID.
        key: MigrationKey,
        /// Version whose payload exceeded the bound.
        version: SchemaVersion,
        /// Configured serialized byte limit.
        max_bytes: u64,
    },
    /// JSON could not be measured with the bounded serializer.
    Serialization {
        /// Persisted schema namespace and ID.
        key: MigrationKey,
        /// Version being measured.
        version: SchemaVersion,
        /// Serializer failure.
        message: String,
    },
}

impl fmt::Display for MigrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLimits { field } => {
                write!(formatter, "migration limit {field} must be non-zero")
            }
            Self::NonAdjacentEdge { key, from, to } => write!(
                formatter,
                "migration {key} edge {} -> {} is not adjacent",
                from.get(),
                to.get()
            ),
            Self::DuplicateEdge { key, from } => {
                write!(
                    formatter,
                    "duplicate migration {key} edge from {}",
                    from.get()
                )
            }
            Self::FutureVersion {
                key,
                source,
                current,
            } => write!(
                formatter,
                "migration {key} source version {} is newer than current {}",
                source.get(),
                current.get()
            ),
            Self::StepLimitExceeded {
                key,
                required_steps,
                max_steps,
            } => write!(
                formatter,
                "migration {key} requires {required_steps} steps, limit is {max_steps}"
            ),
            Self::MissingEdge { key, from, current } => write!(
                formatter,
                "migration {key} has no edge from {} on path to {}",
                from.get(),
                current.get()
            ),
            Self::Transform {
                key,
                from,
                to,
                error,
            } => write!(
                formatter,
                "migration {key} transform {} -> {} failed: {error}",
                from.get(),
                to.get()
            ),
            Self::OutputTooLarge {
                key,
                version,
                max_bytes,
            } => write!(
                formatter,
                "migration {key} version {} exceeds {max_bytes} serialized bytes",
                version.get()
            ),
            Self::Serialization {
                key,
                version,
                message,
            } => write!(
                formatter,
                "migration {key} version {} could not be serialized: {message}",
                version.get()
            ),
        }
    }
}

impl Error for MigrationError {}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn version(value: u32) -> SchemaVersion {
        SchemaVersion::new(value).expect("non-zero test version")
    }

    fn limits(bytes: u64) -> MigrationLimits {
        MigrationLimits::new(8, bytes).expect("non-zero test limits")
    }

    fn component_id() -> ComponentSchemaId {
        ComponentSchemaId::new("yuyib.test-component").expect("valid test id")
    }

    fn increment(value: Value) -> Result<Value, MigrationTransformError> {
        let Some(number) = value.get("value").and_then(Value::as_u64) else {
            return Err(MigrationTransformError::new("missing numeric value"));
        };
        Ok(json!({"value": number + 1}))
    }

    #[test]
    fn duplicate_source_edge_is_a_hard_error() {
        let mut registry = MigrationRegistry::new();
        let edge = || {
            ComponentMigration::new(component_id(), version(1), version(2), increment)
                .expect("adjacent edge")
        };
        registry.register_component(edge()).expect("first edge");
        assert!(matches!(
            registry.register_component(edge()),
            Err(MigrationError::DuplicateEdge { from, .. }) if from == version(1)
        ));
    }

    #[test]
    fn gap_is_reported_at_the_first_missing_version() {
        let mut registry = MigrationRegistry::new();
        registry
            .register_component(
                ComponentMigration::new(component_id(), version(1), version(2), increment)
                    .expect("edge"),
            )
            .expect("register edge");
        assert!(matches!(
            registry.validate_component_path(&component_id(), version(1), version(3), limits(128)),
            Err(MigrationError::MissingEdge { from, .. }) if from == version(2)
        ));
    }

    #[test]
    fn chain_executes_in_version_order_even_when_registered_out_of_order() {
        let mut registry = MigrationRegistry::new();
        registry
            .register_component(
                ComponentMigration::new(component_id(), version(2), version(3), increment)
                    .expect("edge"),
            )
            .expect("register v2 edge");
        registry
            .register_component(
                ComponentMigration::new(component_id(), version(1), version(2), increment)
                    .expect("edge"),
            )
            .expect("register v1 edge");

        let output = registry
            .migrate_component(
                &component_id(),
                version(1),
                version(3),
                json!({"value": 1}),
                limits(128),
            )
            .expect("complete chain");
        assert_eq!(output, json!({"value": 3}));
    }

    #[test]
    fn transformed_output_is_bounded_without_a_serialized_copy() {
        let mut registry = MigrationRegistry::new();
        registry
            .register_component(
                ComponentMigration::new(component_id(), version(1), version(2), |_| {
                    Ok(json!({"large": "x".repeat(256)}))
                })
                .expect("edge"),
            )
            .expect("register edge");
        assert!(matches!(
            registry.migrate_component(
                &component_id(),
                version(1),
                version(2),
                json!({}),
                limits(32),
            ),
            Err(MigrationError::OutputTooLarge { version: output_version, .. })
                if output_version == version(2)
        ));
    }

    #[test]
    fn future_document_version_is_rejected_before_lookup() {
        let registry = MigrationRegistry::new();
        assert!(matches!(
            registry.migrate_component(
                &component_id(),
                version(4),
                version(3),
                json!({}),
                limits(128),
            ),
            Err(MigrationError::FutureVersion { source, current, .. })
                if source == version(4) && current == version(3)
        ));
    }

    #[test]
    fn component_and_import_settings_namespaces_never_mix() {
        let mut registry = MigrationRegistry::new();
        let import_id = ImportSettingsSchemaId::new("yuyib.test-component").expect("valid id");
        registry
            .register_component(
                ComponentMigration::new(component_id(), version(1), version(2), |_| {
                    Ok(json!({"namespace": "component"}))
                })
                .expect("edge"),
            )
            .expect("component edge");
        registry
            .register_import_settings(
                ImportSettingsMigration::new(import_id.clone(), version(1), version(2), |_| {
                    Ok(json!({"namespace": "import"}))
                })
                .expect("edge"),
            )
            .expect("import edge");

        let component = registry
            .migrate_component(
                &component_id(),
                version(1),
                version(2),
                json!({}),
                limits(128),
            )
            .expect("component migration");
        let import = registry
            .migrate_import_settings(&import_id, version(1), version(2), json!({}), limits(128))
            .expect("import migration");
        assert_eq!(component["namespace"], "component");
        assert_eq!(import["namespace"], "import");
    }
}
