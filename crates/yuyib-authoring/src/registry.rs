use std::{collections::BTreeMap, error::Error, fmt, sync::Arc};

use crate::{
    CapabilityDescriptor, CapabilityId, ComponentDescriptor, ComponentMigration, ComponentSchemaId,
    CoverageManifest, FieldKind, ImportSettingsDescriptor, ImportSettingsMigration,
    ImportSettingsSchemaId, MigrationError, MigrationRegistry, PreviewAdapter, PreviewDescriptor,
    SystemDescriptor, SystemId,
};

/// Duplicate or dangling authoring registration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RegistrationError {
    /// An identifier is already registered in its strongly typed namespace.
    Duplicate {
        /// Registry namespace.
        kind: &'static str,
        /// Duplicate identifier text.
        id: String,
    },
    /// A descriptor references a capability that has not been registered.
    MissingCapability(CapabilityId),
    /// A system references a component schema that has not been registered.
    MissingComponent(ComponentSchemaId),
    /// A descriptor is internally ambiguous or structurally unsafe.
    InvalidDescriptor {
        /// Descriptor namespace.
        kind: &'static str,
        /// Stable owning identifier.
        id: String,
        /// Stable validation explanation.
        reason: &'static str,
    },
}

impl fmt::Display for RegistrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Duplicate { kind, id } => write!(formatter, "duplicate {kind} id {id}"),
            Self::MissingCapability(id) => write!(formatter, "unregistered capability {id}"),
            Self::MissingComponent(id) => write!(formatter, "unregistered component schema {id}"),
            Self::InvalidDescriptor { kind, id, reason } => {
                write!(formatter, "invalid {kind} {id}: {reason}")
            }
        }
    }
}

impl Error for RegistrationError {}

/// Coverage milestone gate failure after a complete registry is assembled.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CoverageGateError {
    /// An `Asset` capability declared no evidence record.
    MissingAssetEvidence {
        /// Capability missing evidence.
        capability: String,
    },
    /// Asset evidence listed no diagnostic codes.
    EmptyDiagnostics {
        /// Capability with empty diagnostics.
        capability: String,
    },
    /// Evidence references an unregistered import-settings schema.
    MissingImportSettings {
        /// Owning capability.
        capability: String,
        /// Missing schema id.
        schema: String,
    },
    /// Evidence references an unregistered preview capability.
    MissingPreviewCapability {
        /// Owning capability.
        capability: String,
        /// Missing preview capability id.
        preview: String,
    },
    /// Preview capability exists but has no registered adapter.
    MissingPreviewAdapter {
        /// Owning capability.
        capability: String,
        /// Preview capability without adapter.
        preview: String,
    },
    /// Preview capability is still marked Unavailable.
    PreviewUnavailable {
        /// Owning capability.
        capability: String,
        /// Unavailable preview capability.
        preview: String,
    },
}

impl fmt::Display for CoverageGateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingAssetEvidence { capability } => {
                write!(
                    formatter,
                    "Asset capability {capability} is missing coverage evidence"
                )
            }
            Self::EmptyDiagnostics { capability } => {
                write!(
                    formatter,
                    "Asset capability {capability} evidence has no diagnostic codes"
                )
            }
            Self::MissingImportSettings { capability, schema } => {
                write!(
                    formatter,
                    "Asset capability {capability} evidence references missing import settings {schema}"
                )
            }
            Self::MissingPreviewCapability { capability, preview } => {
                write!(
                    formatter,
                    "Asset capability {capability} evidence references missing preview capability {preview}"
                )
            }
            Self::MissingPreviewAdapter { capability, preview } => {
                write!(
                    formatter,
                    "Asset capability {capability} requires registered preview adapter {preview}"
                )
            }
            Self::PreviewUnavailable { capability, preview } => {
                write!(
                    formatter,
                    "Asset capability {capability} evidence points at unavailable preview {preview}"
                )
            }
        }
    }
}

impl Error for CoverageGateError {}

/// Deterministic registry consumed by editor UI, CI, docs, and source navigation.
#[derive(Default)]
pub struct AuthoringRegistry {
    capabilities: BTreeMap<CapabilityId, CapabilityDescriptor>,
    components: BTreeMap<ComponentSchemaId, ComponentDescriptor>,
    import_settings: BTreeMap<ImportSettingsSchemaId, ImportSettingsDescriptor>,
    systems: BTreeMap<SystemId, SystemDescriptor>,
    previews: BTreeMap<CapabilityId, Arc<dyn PreviewAdapter>>,
    migrations: MigrationRegistry,
}

impl AuthoringRegistry {
    /// Creates an empty registry.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            capabilities: BTreeMap::new(),
            components: BTreeMap::new(),
            import_settings: BTreeMap::new(),
            systems: BTreeMap::new(),
            previews: BTreeMap::new(),
            migrations: MigrationRegistry::new(),
        }
    }

    /// Registers exactly one coverage declaration.
    ///
    /// # Errors
    ///
    /// Duplicate stable capability IDs are a hard error.
    pub fn register_capability(
        &mut self,
        descriptor: CapabilityDescriptor,
    ) -> Result<(), RegistrationError> {
        let id = descriptor.id().clone();
        let unavailable = descriptor
            .surfaces()
            .contains(&crate::CoverageStatus::Unavailable);
        if descriptor.surfaces().is_empty() || (unavailable && descriptor.surfaces().len() != 1) {
            return Err(RegistrationError::InvalidDescriptor {
                kind: "capability",
                id: id.to_string(),
                reason: "coverage surfaces must be non-empty and cannot mix unavailable with implemented surfaces",
            });
        }
        if unavailable
            && (descriptor.unavailable_reason().is_none()
                || descriptor.target_milestone().is_none())
        {
            return Err(RegistrationError::InvalidDescriptor {
                kind: "capability",
                id: id.to_string(),
                reason: "unavailable coverage requires a reason and target milestone",
            });
        }
        if descriptor.surfaces().contains(&crate::CoverageStatus::Asset)
            && descriptor.asset_evidence().is_none()
        {
            return Err(RegistrationError::InvalidDescriptor {
                kind: "capability",
                id: id.to_string(),
                reason: "Asset coverage requires asset_evidence (settings + preview + diagnostics)",
            });
        }
        if self.capabilities.contains_key(&id) {
            return Err(RegistrationError::Duplicate {
                kind: "capability",
                id: id.to_string(),
            });
        }
        self.capabilities.insert(id, descriptor);
        Ok(())
    }

    /// Registers a persisted component schema.
    ///
    /// # Errors
    ///
    /// Fails on duplicate IDs or a missing capability declaration.
    pub fn register_component(
        &mut self,
        descriptor: ComponentDescriptor,
    ) -> Result<(), RegistrationError> {
        self.require_capability(descriptor.capability())?;
        validate_component_descriptor(&descriptor)?;
        let id = descriptor.id().clone();
        if self.components.contains_key(&id) {
            return Err(RegistrationError::Duplicate {
                kind: "component schema",
                id: id.to_string(),
            });
        }
        self.components.insert(id, descriptor);
        Ok(())
    }

    /// Registers a persisted importer-settings schema.
    ///
    /// # Errors
    ///
    /// Fails on duplicate IDs or a missing capability declaration.
    pub fn register_import_settings(
        &mut self,
        descriptor: ImportSettingsDescriptor,
    ) -> Result<(), RegistrationError> {
        self.require_capability(descriptor.capability())?;
        let id = descriptor.id().clone();
        if self.import_settings.contains_key(&id) {
            return Err(RegistrationError::Duplicate {
                kind: "import-settings schema",
                id: id.to_string(),
            });
        }
        self.import_settings.insert(id, descriptor);
        Ok(())
    }

    /// Registers system discoverability metadata.
    ///
    /// # Errors
    ///
    /// Fails on duplicate system IDs or references to unregistered component
    /// schemas.
    pub fn register_system(
        &mut self,
        descriptor: SystemDescriptor,
    ) -> Result<(), RegistrationError> {
        for component in descriptor.reads().iter().chain(descriptor.writes()) {
            if !self.components.contains_key(component) {
                return Err(RegistrationError::MissingComponent(component.clone()));
            }
        }
        let id = descriptor.id().clone();
        if self.systems.contains_key(&id) {
            return Err(RegistrationError::Duplicate {
                kind: "system",
                id: id.to_string(),
            });
        }
        self.systems.insert(id, descriptor);
        Ok(())
    }

    /// Registers a preview adapter backed by the runtime import/cook pipeline.
    ///
    /// # Errors
    ///
    /// Fails on duplicate preview capability IDs or a missing coverage entry.
    pub fn register_preview(
        &mut self,
        adapter: Arc<dyn PreviewAdapter>,
    ) -> Result<(), RegistrationError> {
        let id = adapter.descriptor().capability().clone();
        self.require_capability(&id)?;
        if self.capabilities.get(&id).is_some_and(|capability| {
            capability
                .surfaces()
                .contains(&crate::CoverageStatus::Unavailable)
        }) {
            return Err(RegistrationError::InvalidDescriptor {
                kind: "preview",
                id: id.to_string(),
                reason: "an unavailable capability cannot register a preview adapter",
            });
        }
        if self.previews.contains_key(&id) {
            return Err(RegistrationError::Duplicate {
                kind: "preview capability",
                id: id.to_string(),
            });
        }
        self.previews.insert(id, adapter);
        Ok(())
    }

    /// Registers one executable component migration edge.
    ///
    /// # Errors
    ///
    /// Duplicate/non-adjacent edges are rejected by the migration registry.
    pub fn register_component_migration(
        &mut self,
        migration: ComponentMigration,
    ) -> Result<(), MigrationError> {
        self.migrations.register_component(migration)
    }

    /// Registers one executable importer-settings migration edge.
    ///
    /// # Errors
    ///
    /// Duplicate/non-adjacent edges are rejected by the migration registry.
    pub fn register_import_settings_migration(
        &mut self,
        migration: ImportSettingsMigration,
    ) -> Result<(), MigrationError> {
        self.migrations.register_import_settings(migration)
    }

    /// Returns executable migration chains for load/materialization.
    #[must_use]
    pub const fn migrations(&self) -> &MigrationRegistry {
        &self.migrations
    }

    /// Finds one capability coverage declaration.
    #[must_use]
    pub fn capability(&self, id: &CapabilityId) -> Option<&CapabilityDescriptor> {
        self.capabilities.get(id)
    }

    /// Finds one persisted component schema.
    #[must_use]
    pub fn component(&self, id: &ComponentSchemaId) -> Option<&ComponentDescriptor> {
        self.components.get(id)
    }

    /// Finds one importer-settings schema.
    #[must_use]
    pub fn import_settings(
        &self,
        id: &ImportSettingsSchemaId,
    ) -> Option<&ImportSettingsDescriptor> {
        self.import_settings.get(id)
    }

    /// Finds one system descriptor.
    #[must_use]
    pub fn system(&self, id: &SystemId) -> Option<&SystemDescriptor> {
        self.systems.get(id)
    }

    /// Finds one registered preview descriptor.
    #[must_use]
    pub fn preview_descriptor(&self, id: &CapabilityId) -> Option<&PreviewDescriptor> {
        self.previews.get(id).map(|adapter| adapter.descriptor())
    }

    /// Clones one registered preview adapter for starting jobs outside the registry lock scope.
    #[must_use]
    pub fn preview_adapter(&self, id: &CapabilityId) -> Option<Arc<dyn PreviewAdapter>> {
        self.previews.get(id).cloned()
    }

    /// Produces a deterministic, machine-readable capability snapshot.
    #[must_use]
    pub fn coverage_manifest(&self) -> CoverageManifest {
        CoverageManifest {
            capabilities: self.capabilities.values().cloned().collect(),
            components: self.components.values().cloned().collect(),
            import_settings: self.import_settings.values().cloned().collect(),
            systems: self.systems.values().cloned().collect(),
            previews: self
                .previews
                .values()
                .map(|adapter| adapter.descriptor().clone())
                .collect(),
            migrations: self.migrations.descriptors(),
        }
    }

    /// Validates that every [`CoverageStatus::Asset`] capability has closable
    /// evidence: registered import settings, a non-Unavailable preview
    /// capability with a live adapter, and at least one diagnostic code.
    ///
    /// Call after the host finishes foundation + production adapter registration.
    ///
    /// # Errors
    ///
    /// Returns the first gate violation in capability-id order.
    pub fn validate_coverage_gate(&self) -> Result<(), CoverageGateError> {
        for capability in self.capabilities.values() {
            if !capability
                .surfaces()
                .contains(&crate::CoverageStatus::Asset)
            {
                continue;
            }
            let id = capability.id().to_string();
            let Some(evidence) = capability.asset_evidence() else {
                return Err(CoverageGateError::MissingAssetEvidence { capability: id });
            };
            if evidence.diagnostic_codes().is_empty() {
                return Err(CoverageGateError::EmptyDiagnostics { capability: id });
            }
            if !self
                .import_settings
                .contains_key(evidence.import_settings_schema())
            {
                return Err(CoverageGateError::MissingImportSettings {
                    capability: id,
                    schema: evidence.import_settings_schema().to_string(),
                });
            }
            let preview = evidence.preview_capability();
            let Some(preview_capability) = self.capabilities.get(preview) else {
                return Err(CoverageGateError::MissingPreviewCapability {
                    capability: id,
                    preview: preview.to_string(),
                });
            };
            if preview_capability
                .surfaces()
                .contains(&crate::CoverageStatus::Unavailable)
            {
                return Err(CoverageGateError::PreviewUnavailable {
                    capability: id,
                    preview: preview.to_string(),
                });
            }
            if !self.previews.contains_key(preview) {
                return Err(CoverageGateError::MissingPreviewAdapter {
                    capability: id,
                    preview: preview.to_string(),
                });
            }
        }
        Ok(())
    }

    fn require_capability(&self, id: &CapabilityId) -> Result<(), RegistrationError> {
        if self.capabilities.contains_key(id) {
            Ok(())
        } else {
            Err(RegistrationError::MissingCapability(id.clone()))
        }
    }
}

fn validate_component_descriptor(
    descriptor: &ComponentDescriptor,
) -> Result<(), RegistrationError> {
    let mut paths = std::collections::BTreeSet::new();
    for field in descriptor.fields() {
        let path = field.path();
        if path.is_empty()
            || path.len() > 191
            || !path
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            return Err(RegistrationError::InvalidDescriptor {
                kind: "component schema",
                id: descriptor.id().to_string(),
                reason: "field paths must be non-empty portable ASCII identifiers",
            });
        }
        if field.title().trim().is_empty() {
            return Err(RegistrationError::InvalidDescriptor {
                kind: "component schema",
                id: descriptor.id().to_string(),
                reason: "field titles must not be empty",
            });
        }
        if !paths.insert(path) {
            return Err(RegistrationError::InvalidDescriptor {
                kind: "component schema",
                id: descriptor.id().to_string(),
                reason: "field paths must be unique",
            });
        }
        match field.kind() {
            FieldKind::Enum { values } => {
                let unique = values.iter().collect::<std::collections::BTreeSet<_>>();
                if values.is_empty()
                    || unique.len() != values.len()
                    || values.iter().any(String::is_empty)
                {
                    return Err(RegistrationError::InvalidDescriptor {
                        kind: "component schema",
                        id: descriptor.id().to_string(),
                        reason: "enum values must be non-empty and unique",
                    });
                }
            }
            FieldKind::Specialized { widget } if CapabilityId::new(widget).is_err() => {
                return Err(RegistrationError::InvalidDescriptor {
                    kind: "component schema",
                    id: descriptor.id().to_string(),
                    reason: "specialized widget must use a stable identifier",
                });
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CoverageStatus, PluginId, SchemaVersion};

    fn capability(id: &str) -> CapabilityDescriptor {
        CapabilityDescriptor::new(
            CapabilityId::new(id).expect("id"),
            "Transform",
            CoverageStatus::Visual,
            PluginId::new("yuyib.game-3d").expect("plugin"),
        )
    }

    #[test]
    fn duplicate_capability_ids_are_hard_errors() {
        let mut registry = AuthoringRegistry::new();
        registry
            .register_capability(capability("yuyib.transform3d"))
            .expect("first registration");
        assert!(matches!(
            registry.register_capability(capability("yuyib.transform3d")),
            Err(RegistrationError::Duplicate {
                kind: "capability",
                ..
            })
        ));
    }

    #[test]
    fn descriptors_must_reference_registered_capabilities() {
        let mut registry = AuthoringRegistry::new();
        let component = ComponentDescriptor::new(
            ComponentSchemaId::new("yuyib.transform3d").expect("id"),
            CapabilityId::new("yuyib.transform3d").expect("id"),
            SchemaVersion::new(1).expect("version"),
        );
        assert!(matches!(
            registry.register_component(component.clone()),
            Err(RegistrationError::MissingCapability(_))
        ));
        registry
            .register_capability(capability("yuyib.transform3d"))
            .expect("capability");
        registry.register_component(component).expect("component");
        assert!(
            registry
                .component(&ComponentSchemaId::new("yuyib.transform3d").expect("id"))
                .is_some()
        );
    }

    #[test]
    fn coverage_manifest_is_deterministic_and_machine_readable() {
        let mut registry = AuthoringRegistry::new();
        registry
            .register_capability(capability("yuyib.z-last"))
            .expect("capability");
        registry
            .register_capability(capability("yuyib.a-first"))
            .expect("capability");
        let manifest = registry.coverage_manifest();
        assert_eq!(manifest.capabilities[0].id().as_str(), "yuyib.a-first");
        let json = serde_json::to_value(&manifest).expect("manifest JSON");
        assert_eq!(json["capabilities"][0]["surfaces"][0], "visual");
        assert_eq!(json["migrations"], serde_json::json!([]));

        let pretty = manifest.to_pretty_json().expect("pretty");
        assert!(pretty.ends_with('\n'));
        let round_trip: crate::CoverageManifest =
            serde_json::from_str(&pretty).expect("deserialize pretty");
        assert_eq!(round_trip, manifest);

        let mut reversed = AuthoringRegistry::new();
        reversed
            .register_capability(capability("yuyib.a-first"))
            .expect("capability");
        reversed
            .register_capability(capability("yuyib.z-last"))
            .expect("capability");
        assert_eq!(
            reversed.coverage_manifest().to_pretty_json().expect("pretty"),
            pretty
        );
    }

    #[test]
    fn component_field_paths_are_unique_and_machine_readable() {
        let mut registry = AuthoringRegistry::new();
        registry
            .register_capability(capability("yuyib.transform-fields"))
            .expect("capability");
        let descriptor = ComponentDescriptor::new(
            ComponentSchemaId::new("yuyib.transform-fields").expect("id"),
            CapabilityId::new("yuyib.transform-fields").expect("id"),
            SchemaVersion::new(1).expect("version"),
        )
        .with_field(crate::FieldDescriptor::new(
            "translation",
            "Translation",
            crate::FieldKind::Vec3,
        ))
        .with_field(crate::FieldDescriptor::new(
            "translation",
            "Duplicate",
            crate::FieldKind::Vec3,
        ));
        assert!(matches!(
            registry.register_component(descriptor),
            Err(RegistrationError::InvalidDescriptor { .. })
        ));
    }
}
