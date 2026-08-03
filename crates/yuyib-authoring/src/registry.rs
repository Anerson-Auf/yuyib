use std::{collections::BTreeMap, error::Error, fmt, sync::Arc};

use crate::{
    CapabilityDescriptor, CapabilityId, ComponentDescriptor, ComponentMigration, ComponentSchemaId,
    CoverageManifest, FieldKind, ImportSettingsDescriptor, ImportSettingsMigration,
    ImportSettingsSchemaId, MigrationError, MigrationRegistry, PreviewAdapter, PreviewDescriptor,
    SchemaVersion, SystemDescriptor, SystemId,
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
    /// Capability documentation is missing or blank.
    CapabilityMissingDocumentation {
        /// Capability missing documentation.
        capability: String,
    },
    /// Capability source navigation is missing or blank.
    CapabilityMissingSource {
        /// Capability missing source.
        capability: String,
    },
    /// A `Visual` capability has no registered component schema.
    ///
    /// Shell-level Visual surfaces (`yuyib.application`, `yuyib.game-lifecycle`)
    /// are exempt; persisted Inspector Visuals must declare a schema.
    VisualWithoutComponentSchema {
        /// Capability missing a component schema.
        capability: String,
    },
    /// A Visual component schema has no runtime source navigation.
    VisualComponentMissingRuntimeSource {
        /// Owning Visual capability.
        capability: String,
        /// Component schema id.
        schema: String,
    },
    /// A Visual component schema declares no Inspector fields.
    VisualComponentWithoutFields {
        /// Owning Visual capability.
        capability: String,
        /// Component schema id.
        schema: String,
    },
    /// Migration chain from version 1 to current is incomplete.
    MigrationGap {
        /// `"component"` or `"import-settings"`.
        kind: &'static str,
        /// Schema id.
        schema: String,
        /// Version where the gap starts.
        from: u32,
        /// Declared current version.
        current: u32,
    },
    /// Migration path validation failed for a reason other than a missing edge.
    MigrationInvalid {
        /// `"component"` or `"import-settings"`.
        kind: &'static str,
        /// Schema id.
        schema: String,
        /// Stable explanation.
        detail: String,
    },
    /// Import-settings schema points at a capability without an Asset surface.
    ImportSettingsNotAsset {
        /// Import-settings schema id.
        schema: String,
        /// Referenced capability id.
        capability: String,
    },
    /// A field advertises Apply Play on a non-Visual capability.
    ApplyPlayRequiresVisual {
        /// Component schema id.
        schema: String,
        /// Owning capability id.
        capability: String,
        /// Field path with `apply_play_changes`.
        field: String,
    },
    /// A registered system has no usable source-navigation target for Editor open.
    SystemWithoutSource {
        /// System missing source metadata.
        system: String,
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
            Self::CapabilityMissingDocumentation { capability } => {
                write!(
                    formatter,
                    "capability {capability} is missing non-empty documentation"
                )
            }
            Self::CapabilityMissingSource { capability } => {
                write!(
                    formatter,
                    "capability {capability} is missing non-empty SourceNavigation"
                )
            }
            Self::VisualWithoutComponentSchema { capability } => {
                write!(
                    formatter,
                    "Visual capability {capability} has no ComponentDescriptor"
                )
            }
            Self::VisualComponentMissingRuntimeSource { capability, schema } => {
                write!(
                    formatter,
                    "Visual capability {capability} schema {schema} has no runtime SourceNavigation"
                )
            }
            Self::VisualComponentWithoutFields { capability, schema } => {
                write!(
                    formatter,
                    "Visual capability {capability} schema {schema} has no Inspector fields"
                )
            }
            Self::MigrationGap {
                kind,
                schema,
                from,
                current,
            } => {
                write!(
                    formatter,
                    "{kind} schema {schema} migration gap from v{from} to v{current}"
                )
            }
            Self::MigrationInvalid {
                kind,
                schema,
                detail,
            } => {
                write!(
                    formatter,
                    "{kind} schema {schema} migration path invalid: {detail}"
                )
            }
            Self::ImportSettingsNotAsset { schema, capability } => {
                write!(
                    formatter,
                    "import-settings schema {schema} capability {capability} is not an Asset surface"
                )
            }
            Self::ApplyPlayRequiresVisual {
                schema,
                capability,
                field,
            } => {
                write!(
                    formatter,
                    "schema {schema} field `{field}` applies_play_changes but capability {capability} is not Visual"
                )
            }
            Self::SystemWithoutSource { system } => {
                write!(
                    formatter,
                    "system {system} has no SourceNavigation for Editor open"
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

    /// Validates coverage evidence across capabilities, schemas, migrations, and
    /// systems.
    ///
    /// Enforced rules (incremental):
    /// - every capability has non-empty documentation + [`crate::SourceNavigation`];
    /// - every `Asset` surface has closable import-settings evidence (schema,
    ///   diagnostics, non-Unavailable preview capability + adapter);
    /// - every `Visual` surface (except shell allowlist `yuyib.application` /
    ///   `yuyib.game-lifecycle`) has a [`ComponentDescriptor`] with runtime
    ///   source and at least one Inspector field;
    /// - every component / import-settings schema has a migration path from
    ///   version 1 to its declared current version;
    /// - every import-settings schema references an Asset capability;
    /// - `apply_play_changes` fields only on Visual capability schemas;
    /// - every system has non-empty source navigation.
    ///
    /// Call after the host finishes foundation + production adapter registration.
    ///
    /// # Errors
    ///
    /// Returns the first gate violation in deterministic id order.
    pub fn validate_coverage_gate(&self) -> Result<(), CoverageGateError> {
        let visual_component_capabilities = self
            .components
            .values()
            .map(ComponentDescriptor::capability)
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        let baseline = SchemaVersion::new(1).expect("schema version 1");
        let limits = crate::MigrationLimits::new(64, 16 * 1024 * 1024).expect("migration limits");

        for capability in self.capabilities.values() {
            let id = capability.id().to_string();
            if capability
                .documentation()
                .is_none_or(|docs| docs.trim().is_empty())
            {
                return Err(CoverageGateError::CapabilityMissingDocumentation { capability: id });
            }
            if source_navigation_missing(capability.source()) {
                return Err(CoverageGateError::CapabilityMissingSource { capability: id });
            }
            if capability
                .surfaces()
                .contains(&crate::CoverageStatus::Visual)
                && !visual_capability_allows_missing_component(capability.id())
                && !visual_component_capabilities.contains(capability.id())
            {
                return Err(CoverageGateError::VisualWithoutComponentSchema { capability: id });
            }
            if !capability
                .surfaces()
                .contains(&crate::CoverageStatus::Asset)
            {
                continue;
            }
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

        for component in self.components.values() {
            let schema = component.id().to_string();
            map_migration_path(
                "component",
                &schema,
                self.migrations.validate_component_path(
                    component.id(),
                    baseline,
                    component.current_version(),
                    limits,
                ),
            )?;
            let Some(capability) = self.capabilities.get(component.capability()) else {
                continue;
            };
            if !capability
                .surfaces()
                .contains(&crate::CoverageStatus::Visual)
                || visual_capability_allows_missing_component(capability.id())
            {
                continue;
            }
            let capability_id = capability.id().to_string();
            if source_navigation_missing(component.runtime_source()) {
                return Err(CoverageGateError::VisualComponentMissingRuntimeSource {
                    capability: capability_id,
                    schema,
                });
            }
            if component.fields().is_empty() {
                return Err(CoverageGateError::VisualComponentWithoutFields {
                    capability: capability_id,
                    schema,
                });
            }
        }

        for component in self.components.values() {
            let Some(capability) = self.capabilities.get(component.capability()) else {
                continue;
            };
            if capability
                .surfaces()
                .contains(&crate::CoverageStatus::Visual)
            {
                continue;
            }
            for field in component.fields() {
                if field.applies_play_changes() {
                    return Err(CoverageGateError::ApplyPlayRequiresVisual {
                        schema: component.id().to_string(),
                        capability: capability.id().to_string(),
                        field: field.path().to_owned(),
                    });
                }
            }
        }

        for settings in self.import_settings.values() {
            let schema = settings.id().to_string();
            map_migration_path(
                "import-settings",
                &schema,
                self.migrations.validate_import_settings_path(
                    settings.id(),
                    baseline,
                    settings.current_version(),
                    limits,
                ),
            )?;
            let Some(capability) = self.capabilities.get(settings.capability()) else {
                continue;
            };
            if !capability
                .surfaces()
                .contains(&crate::CoverageStatus::Asset)
            {
                return Err(CoverageGateError::ImportSettingsNotAsset {
                    schema,
                    capability: capability.id().to_string(),
                });
            }
        }

        for system in self.systems.values() {
            if source_navigation_missing(system.source()) {
                return Err(CoverageGateError::SystemWithoutSource {
                    system: system.id().to_string(),
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

/// Shell Visual surfaces that intentionally have no persisted component schema.
fn visual_capability_allows_missing_component(id: &CapabilityId) -> bool {
    matches!(id.as_str(), "yuyib.application" | "yuyib.game-lifecycle")
}

fn source_navigation_missing(source: Option<&crate::SourceNavigation>) -> bool {
    source.is_none_or(|navigation| navigation.file.trim().is_empty())
}

fn map_migration_path(
    kind: &'static str,
    schema: &str,
    result: Result<(), MigrationError>,
) -> Result<(), CoverageGateError> {
    match result {
        Ok(()) => Ok(()),
        Err(MigrationError::MissingEdge { from, current, .. }) => {
            Err(CoverageGateError::MigrationGap {
                kind,
                schema: schema.to_owned(),
                from: from.get(),
                current: current.get(),
            })
        }
        Err(error) => Err(CoverageGateError::MigrationInvalid {
            kind,
            schema: schema.to_owned(),
            detail: error.to_string(),
        }),
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
    use crate::{
        CoverageStatus, FieldDescriptor, FieldKind, PluginId, ScheduleId, SchemaVersion,
        SourceNavigation, SystemId,
    };

    fn capability(id: &str) -> CapabilityDescriptor {
        CapabilityDescriptor::new(
            CapabilityId::new(id).expect("id"),
            "Transform",
            CoverageStatus::Visual,
            PluginId::new("yuyib.game-3d").expect("plugin"),
        )
        .with_documentation("crates/yuyib-game-3d/src/lib.rs")
        .with_source(SourceNavigation::file("crates/yuyib-game-3d/src/lib.rs"))
    }

    fn visual_component(id: &str) -> ComponentDescriptor {
        ComponentDescriptor::new(
            ComponentSchemaId::new(id).expect("id"),
            CapabilityId::new(id).expect("id"),
            SchemaVersion::new(1).expect("version"),
        )
        .with_runtime_source(SourceNavigation::file("crates/yuyib-game-3d/src/lib.rs"))
        .with_field(FieldDescriptor::new("value", "Value", FieldKind::F32))
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

    #[test]
    fn visual_without_component_schema_fails_coverage_gate() {
        let mut registry = AuthoringRegistry::new();
        registry
            .register_capability(capability("yuyib.orphan-visual"))
            .expect("capability");
        assert!(matches!(
            registry.validate_coverage_gate(),
            Err(CoverageGateError::VisualWithoutComponentSchema { capability })
                if capability == "yuyib.orphan-visual"
        ));
    }

    #[test]
    fn visual_with_component_schema_passes_coverage_gate() {
        let mut registry = AuthoringRegistry::new();
        registry
            .register_capability(capability("yuyib.linked-visual"))
            .expect("capability");
        registry
            .register_component(visual_component("yuyib.linked-visual"))
            .expect("component");
        registry
            .validate_coverage_gate()
            .expect("linked Visual must pass");
    }

    #[test]
    fn shell_visual_without_component_is_allowlisted() {
        let mut registry = AuthoringRegistry::new();
        for id in ["yuyib.application", "yuyib.game-lifecycle"] {
            registry
                .register_capability(capability(id))
                .expect("shell capability");
        }
        registry
            .validate_coverage_gate()
            .expect("shell Visual allowlist");
    }

    #[test]
    fn system_without_source_fails_coverage_gate() {
        let mut registry = AuthoringRegistry::new();
        registry
            .register_system(SystemDescriptor::new(
                SystemId::new("yuyib.system.orphan").expect("id"),
                PluginId::new("yuyib.game-3d").expect("plugin"),
                ScheduleId::new("yuyib.schedule.caller-driven").expect("schedule"),
            ))
            .expect("system");
        assert!(matches!(
            registry.validate_coverage_gate(),
            Err(CoverageGateError::SystemWithoutSource { system })
                if system == "yuyib.system.orphan"
        ));
    }

    #[test]
    fn system_with_source_passes_coverage_gate() {
        let mut registry = AuthoringRegistry::new();
        registry
            .register_system(
                SystemDescriptor::new(
                    SystemId::new("yuyib.system.navigable").expect("id"),
                    PluginId::new("yuyib.game-3d").expect("plugin"),
                    ScheduleId::new("yuyib.schedule.caller-driven").expect("schedule"),
                )
                .with_source(SourceNavigation::file("crates/yuyib-game-3d/src/lib.rs")),
            )
            .expect("system");
        registry
            .validate_coverage_gate()
            .expect("system with source must pass");
    }

    #[test]
    fn capability_without_documentation_fails_coverage_gate() {
        let mut registry = AuthoringRegistry::new();
        let descriptor = CapabilityDescriptor::new(
            CapabilityId::new("yuyib.undocumented").expect("id"),
            "Undocumented",
            CoverageStatus::Unavailable,
            PluginId::new("yuyib.game-3d").expect("plugin"),
        )
        .unavailable("not ready", "future")
        .with_source(SourceNavigation::file("crates/yuyib-game-3d/src/lib.rs"));
        registry
            .register_capability(descriptor)
            .expect("capability");
        assert!(matches!(
            registry.validate_coverage_gate(),
            Err(CoverageGateError::CapabilityMissingDocumentation { capability })
                if capability == "yuyib.undocumented"
        ));
    }

    #[test]
    fn visual_component_without_fields_fails_coverage_gate() {
        let mut registry = AuthoringRegistry::new();
        registry
            .register_capability(capability("yuyib.empty-fields"))
            .expect("capability");
        registry
            .register_component(
                ComponentDescriptor::new(
                    ComponentSchemaId::new("yuyib.empty-fields").expect("id"),
                    CapabilityId::new("yuyib.empty-fields").expect("id"),
                    SchemaVersion::new(1).expect("version"),
                )
                .with_runtime_source(SourceNavigation::file("crates/yuyib-game-3d/src/lib.rs")),
            )
            .expect("component");
        assert!(matches!(
            registry.validate_coverage_gate(),
            Err(CoverageGateError::VisualComponentWithoutFields { schema, .. })
                if schema == "yuyib.empty-fields"
        ));
    }

    #[test]
    fn visual_component_without_runtime_source_fails_coverage_gate() {
        let mut registry = AuthoringRegistry::new();
        registry
            .register_capability(capability("yuyib.no-runtime-source"))
            .expect("capability");
        registry
            .register_component(
                ComponentDescriptor::new(
                    ComponentSchemaId::new("yuyib.no-runtime-source").expect("id"),
                    CapabilityId::new("yuyib.no-runtime-source").expect("id"),
                    SchemaVersion::new(1).expect("version"),
                )
                .with_field(FieldDescriptor::new("value", "Value", FieldKind::F32)),
            )
            .expect("component");
        assert!(matches!(
            registry.validate_coverage_gate(),
            Err(CoverageGateError::VisualComponentMissingRuntimeSource { schema, .. })
                if schema == "yuyib.no-runtime-source"
        ));
    }

    #[test]
    fn import_settings_without_asset_surface_fails_coverage_gate() {
        let mut registry = AuthoringRegistry::new();
        registry
            .register_capability(capability("yuyib.settings-owner"))
            .expect("visual capability");
        registry
            .register_component(visual_component("yuyib.settings-owner"))
            .expect("component");
        registry
            .register_import_settings(ImportSettingsDescriptor::new(
                ImportSettingsSchemaId::new("yuyib.settings-owner-settings").expect("id"),
                CapabilityId::new("yuyib.settings-owner").expect("id"),
                SchemaVersion::new(1).expect("version"),
            ))
            .expect("settings");
        assert!(matches!(
            registry.validate_coverage_gate(),
            Err(CoverageGateError::ImportSettingsNotAsset { schema, .. })
                if schema == "yuyib.settings-owner-settings"
        ));
    }

    #[test]
    fn migration_gap_fails_coverage_gate() {
        let mut registry = AuthoringRegistry::new();
        registry
            .register_capability(capability("yuyib.migrated"))
            .expect("capability");
        registry
            .register_component(
                ComponentDescriptor::new(
                    ComponentSchemaId::new("yuyib.migrated").expect("id"),
                    CapabilityId::new("yuyib.migrated").expect("id"),
                    SchemaVersion::new(2).expect("version"),
                )
                .with_runtime_source(SourceNavigation::file("crates/yuyib-game-3d/src/lib.rs"))
                .with_field(FieldDescriptor::new("value", "Value", FieldKind::F32)),
            )
            .expect("component");
        assert!(matches!(
            registry.validate_coverage_gate(),
            Err(CoverageGateError::MigrationGap {
                kind: "component",
                schema,
                from: 1,
                current: 2,
            }) if schema == "yuyib.migrated"
        ));
    }

    #[test]
    fn apply_play_on_non_visual_fails_coverage_gate() {
        let mut registry = AuthoringRegistry::new();
        registry
            .register_capability(
                CapabilityDescriptor::new(
                    CapabilityId::new("yuyib.future-2d").expect("id"),
                    "Future 2D",
                    CoverageStatus::Unavailable,
                    PluginId::new("yuyib.game-2d").expect("plugin"),
                )
                .unavailable("not wired", "2d-visual")
                .with_documentation("crates/yuyib-game-2d/src/lib.rs")
                .with_source(SourceNavigation::file("crates/yuyib-game-2d/src/lib.rs")),
            )
            .expect("capability");
        registry
            .register_component(
                ComponentDescriptor::new(
                    ComponentSchemaId::new("yuyib.future-2d").expect("id"),
                    CapabilityId::new("yuyib.future-2d").expect("id"),
                    SchemaVersion::new(1).expect("version"),
                )
                .with_runtime_source(SourceNavigation::file("crates/yuyib-game-2d/src/lib.rs"))
                .with_field(
                    FieldDescriptor::new("position", "Position", FieldKind::Vec2)
                        .allow_apply_play_changes(true),
                ),
            )
            .expect("component");
        assert!(matches!(
            registry.validate_coverage_gate(),
            Err(CoverageGateError::ApplyPlayRequiresVisual {
                schema,
                field,
                ..
            }) if schema == "yuyib.future-2d" && field == "position"
        ));
    }
}
