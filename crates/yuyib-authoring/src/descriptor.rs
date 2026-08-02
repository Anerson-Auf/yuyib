use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{
    CapabilityId, ComponentSchemaId, ImportSettingsSchemaId, PluginId, PreviewDescriptor,
    SchemaVersion, SourceNavigation, SystemDescriptor,
};

/// Declares how an engine capability is exposed to authoring tools.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageStatus {
    /// Editable through generated or specialized visual controls.
    Visual,
    /// Exposed through asset importing, cooking, or asset settings.
    Asset,
    /// Exposed through Play Mode or other runtime controls.
    Runtime,
    /// Intentionally available only through project code.
    CodeOnly,
    /// Known by the editor, but not implemented by the engine or adapter yet.
    Unavailable,
}

/// UI-neutral value shape used to generate ordinary Inspector controls.
///
/// Domain-specific values that cannot be represented faithfully should use a
/// specialized authoring adapter instead of pretending to be a primitive.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "options", rename_all = "snake_case")]
pub enum FieldKind {
    /// Boolean toggle.
    Bool,
    /// Signed 32-bit integer.
    I32,
    /// Unsigned 32-bit integer.
    U32,
    /// Finite 32-bit floating-point value.
    F32,
    /// UTF-8 text.
    String,
    /// Two finite floats.
    Vec2,
    /// Three finite floats.
    Vec3,
    /// Four finite floats.
    Vec4,
    /// Unit quaternion in engine component order.
    Quaternion,
    /// Linear RGBA colour.
    Color,
    /// Persistent entity reference.
    EntityReference,
    /// Persistent asset reference.
    AssetReference,
    /// Closed string-valued selection.
    Enum {
        /// Stable serialized values in palette order.
        values: Vec<String>,
    },
    /// Domain widget implemented by a named reusable Editor adapter.
    Specialized {
        /// Stable widget contract identifier.
        widget: String,
    },
}

/// One persisted property exposed by a component authoring adapter.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FieldDescriptor {
    path: String,
    title: String,
    kind: FieldKind,
    unit: Option<String>,
    read_only: bool,
    apply_play_changes: bool,
    documentation: Option<String>,
    source: Option<SourceNavigation>,
}

impl FieldDescriptor {
    /// Creates an editable field. Registry validation rejects duplicate or
    /// structurally unsafe paths.
    #[must_use]
    pub fn new(path: impl Into<String>, title: impl Into<String>, kind: FieldKind) -> Self {
        Self {
            path: path.into(),
            title: title.into(),
            kind,
            unit: None,
            read_only: false,
            apply_play_changes: false,
            documentation: None,
            source: None,
        }
    }

    /// Adds a human-facing unit such as `metres`, `radians`, or `lux`.
    #[must_use]
    pub fn with_unit(mut self, unit: impl Into<String>) -> Self {
        self.unit = Some(unit.into());
        self
    }

    /// Marks a derived/diagnostic property as non-editable.
    #[must_use]
    pub const fn read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        if read_only {
            self.apply_play_changes = false;
        }
        self
    }

    /// Explicitly whitelists this authored property for Apply Play Changes.
    #[must_use]
    pub const fn allow_apply_play_changes(mut self, allowed: bool) -> Self {
        self.apply_play_changes = allowed && !self.read_only;
        self
    }

    /// Attaches documentation or a project-relative documentation location.
    #[must_use]
    pub fn with_documentation(mut self, documentation: impl Into<String>) -> Self {
        self.documentation = Some(documentation.into());
        self
    }

    /// Attaches a source-navigation target for the owning runtime capability.
    #[must_use]
    pub fn with_source(mut self, source: SourceNavigation) -> Self {
        self.source = Some(source);
        self
    }

    /// Returns the stable field path inside the component payload.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns the human-facing field label.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Returns the generated or specialized control shape.
    #[must_use]
    pub const fn kind(&self) -> &FieldKind {
        &self.kind
    }

    /// Returns the optional display unit.
    #[must_use]
    pub fn unit(&self) -> Option<&str> {
        self.unit.as_deref()
    }

    /// Returns whether the Inspector must prevent authored mutation.
    #[must_use]
    pub const fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Returns whether runtime state may be explicitly copied to this field.
    #[must_use]
    pub const fn applies_play_changes(&self) -> bool {
        self.apply_play_changes
    }

    /// Returns optional field documentation.
    #[must_use]
    pub fn documentation(&self) -> Option<&str> {
        self.documentation.as_deref()
    }

    /// Returns the owning runtime source target.
    #[must_use]
    pub const fn source(&self) -> Option<&SourceNavigation> {
        self.source.as_ref()
    }
}

/// Machine-readable evidence required before an [`CoverageStatus::Asset`]
/// capability may close a coverage gate.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AssetCoverageEvidence {
    /// Persisted import-settings schema that must be registered.
    import_settings_schema: ImportSettingsSchemaId,
    /// Preview capability that must have a registered [`crate::PreviewAdapter`].
    preview_capability: CapabilityId,
    /// Stable diagnostic codes the production pipeline can emit.
    diagnostic_codes: Vec<String>,
}

impl AssetCoverageEvidence {
    /// Builds evidence linking settings, preview, and diagnostic codes.
    #[must_use]
    pub fn new(
        import_settings_schema: ImportSettingsSchemaId,
        preview_capability: CapabilityId,
        diagnostic_codes: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            import_settings_schema,
            preview_capability,
            diagnostic_codes: diagnostic_codes.into_iter().map(Into::into).collect(),
        }
    }

    /// Returns the required import-settings schema.
    #[must_use]
    pub const fn import_settings_schema(&self) -> &ImportSettingsSchemaId {
        &self.import_settings_schema
    }

    /// Returns the required preview capability.
    #[must_use]
    pub const fn preview_capability(&self) -> &CapabilityId {
        &self.preview_capability
    }

    /// Returns stable diagnostic codes claimed by this Asset surface.
    #[must_use]
    pub fn diagnostic_codes(&self) -> &[String] {
        &self.diagnostic_codes
    }
}

/// Machine-readable coverage declaration for one public capability.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapabilityDescriptor {
    id: CapabilityId,
    title: String,
    surfaces: BTreeSet<CoverageStatus>,
    owner: PluginId,
    documentation: Option<String>,
    source: Option<SourceNavigation>,
    unavailable_reason: Option<String>,
    target_milestone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    asset_evidence: Option<AssetCoverageEvidence>,
}

impl CapabilityDescriptor {
    /// Creates a capability coverage declaration.
    #[must_use]
    pub fn new(
        id: CapabilityId,
        title: impl Into<String>,
        coverage: CoverageStatus,
        owner: PluginId,
    ) -> Self {
        Self {
            id,
            title: title.into(),
            surfaces: BTreeSet::from([coverage]),
            owner,
            documentation: None,
            source: None,
            unavailable_reason: None,
            target_milestone: None,
            asset_evidence: None,
        }
    }

    /// Adds a documentation URI or project-relative documentation path.
    #[must_use]
    pub fn with_documentation(mut self, documentation: impl Into<String>) -> Self {
        self.documentation = Some(documentation.into());
        self
    }

    /// Attaches a source-navigation target for the owning runtime capability.
    #[must_use]
    pub fn with_source(mut self, source: SourceNavigation) -> Self {
        self.source = Some(source);
        self
    }

    /// Explains why operational authoring coverage is not available yet.
    #[must_use]
    pub fn unavailable(
        mut self,
        reason: impl Into<String>,
        target_milestone: impl Into<String>,
    ) -> Self {
        self.surfaces.clear();
        self.surfaces.insert(CoverageStatus::Unavailable);
        self.unavailable_reason = Some(reason.into());
        self.target_milestone = Some(target_milestone.into());
        self.asset_evidence = None;
        self
    }

    /// Returns the stable capability identifier.
    #[must_use]
    pub const fn id(&self) -> &CapabilityId {
        &self.id
    }

    /// Returns the human-facing title.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Adds one independently discoverable authoring surface.
    ///
    /// Registry validation rejects mixing Unavailable with implemented
    /// surfaces, so partial support remains explicit instead of being hidden
    /// behind one primary status.
    #[must_use]
    pub fn with_surface(mut self, surface: CoverageStatus) -> Self {
        self.surfaces.insert(surface);
        self
    }

    /// Returns all explicitly declared authoring surfaces.
    #[must_use]
    pub const fn surfaces(&self) -> &BTreeSet<CoverageStatus> {
        &self.surfaces
    }

    /// Returns the owning plugin.
    #[must_use]
    pub const fn owner(&self) -> &PluginId {
        &self.owner
    }

    /// Returns the optional documentation location.
    #[must_use]
    pub fn documentation(&self) -> Option<&str> {
        self.documentation.as_deref()
    }

    /// Returns the owning runtime source target.
    #[must_use]
    pub const fn source(&self) -> Option<&SourceNavigation> {
        self.source.as_ref()
    }

    /// Returns the explicit gap for unavailable coverage.
    #[must_use]
    pub fn unavailable_reason(&self) -> Option<&str> {
        self.unavailable_reason.as_deref()
    }

    /// Returns the milestone expected to close an unavailable record.
    #[must_use]
    pub fn target_milestone(&self) -> Option<&str> {
        self.target_milestone.as_deref()
    }

    /// Attaches evidence required for [`CoverageStatus::Asset`] gate closure.
    #[must_use]
    pub fn with_asset_evidence(mut self, evidence: AssetCoverageEvidence) -> Self {
        self.asset_evidence = Some(evidence);
        self
    }

    /// Returns Asset evidence when declared.
    #[must_use]
    pub const fn asset_evidence(&self) -> Option<&AssetCoverageEvidence> {
        self.asset_evidence.as_ref()
    }
}

/// Persisted component schema and migration coverage.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ComponentDescriptor {
    id: ComponentSchemaId,
    capability: CapabilityId,
    current_version: SchemaVersion,
    fields: Vec<FieldDescriptor>,
    runtime_source: Option<SourceNavigation>,
    authoring_source: Option<SourceNavigation>,
}

impl ComponentDescriptor {
    /// Creates a descriptor at its current persisted schema version.
    #[must_use]
    pub fn new(
        id: ComponentSchemaId,
        capability: CapabilityId,
        current_version: SchemaVersion,
    ) -> Self {
        Self {
            id,
            capability,
            current_version,
            fields: Vec::new(),
            runtime_source: None,
            authoring_source: None,
        }
    }

    /// Adds one Inspector field in deterministic display order.
    #[must_use]
    pub fn with_field(mut self, field: FieldDescriptor) -> Self {
        self.fields.push(field);
        self
    }

    /// Attaches the source of the runtime component type.
    #[must_use]
    pub fn with_runtime_source(mut self, source: SourceNavigation) -> Self {
        self.runtime_source = Some(source);
        self
    }

    /// Attaches the source of the authoring adapter, when implemented.
    #[must_use]
    pub fn with_authoring_source(mut self, source: SourceNavigation) -> Self {
        self.authoring_source = Some(source);
        self
    }

    /// Returns the stable component schema identifier.
    #[must_use]
    pub const fn id(&self) -> &ComponentSchemaId {
        &self.id
    }

    /// Returns the capability represented by this component.
    #[must_use]
    pub const fn capability(&self) -> &CapabilityId {
        &self.capability
    }

    /// Returns the current persisted version.
    #[must_use]
    pub const fn current_version(&self) -> SchemaVersion {
        self.current_version
    }

    /// Returns Inspector fields in declared display order.
    #[must_use]
    pub fn fields(&self) -> &[FieldDescriptor] {
        &self.fields
    }

    /// Returns the runtime component source target.
    #[must_use]
    pub const fn runtime_source(&self) -> Option<&SourceNavigation> {
        self.runtime_source.as_ref()
    }

    /// Returns the authoring adapter source target.
    #[must_use]
    pub const fn authoring_source(&self) -> Option<&SourceNavigation> {
        self.authoring_source.as_ref()
    }
}

/// Persisted importer-settings schema and migration coverage.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ImportSettingsDescriptor {
    id: ImportSettingsSchemaId,
    capability: CapabilityId,
    current_version: SchemaVersion,
}

impl ImportSettingsDescriptor {
    /// Creates an importer-settings descriptor.
    #[must_use]
    pub fn new(
        id: ImportSettingsSchemaId,
        capability: CapabilityId,
        current_version: SchemaVersion,
    ) -> Self {
        Self {
            id,
            capability,
            current_version,
        }
    }

    /// Returns the stable importer-settings schema identifier.
    #[must_use]
    pub const fn id(&self) -> &ImportSettingsSchemaId {
        &self.id
    }

    /// Returns the capability represented by these settings.
    #[must_use]
    pub const fn capability(&self) -> &CapabilityId {
        &self.capability
    }

    /// Returns the current persisted version.
    #[must_use]
    pub const fn current_version(&self) -> SchemaVersion {
        self.current_version
    }
}

/// Serializable snapshot used by CI, documentation generation, and editor palettes.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CoverageManifest {
    /// Registered public capabilities in deterministic identifier order.
    pub capabilities: Vec<CapabilityDescriptor>,
    /// Registered persisted component schemas in deterministic identifier order.
    pub components: Vec<ComponentDescriptor>,
    /// Registered persisted importer settings in deterministic identifier order.
    pub import_settings: Vec<ImportSettingsDescriptor>,
    /// Registered systems in deterministic identifier order.
    pub systems: Vec<SystemDescriptor>,
    /// Registered preview contracts in deterministic capability order.
    pub previews: Vec<PreviewDescriptor>,
    /// Installed executable migration edges in deterministic order.
    pub migrations: Vec<crate::MigrationEdgeDescriptor>,
}

impl CoverageManifest {
    /// Serializes a canonical pretty JSON document terminated by `\n`.
    ///
    /// The output is intended for CI golden diffs and documentation generators.
    /// Registry construction already sorts entries by stable identifier.
    ///
    /// # Errors
    ///
    /// Returns when the manifest cannot be encoded as JSON.
    pub fn to_pretty_json(&self) -> Result<String, serde_json::Error> {
        let mut json = serde_json::to_string_pretty(self)?;
        json.push('\n');
        Ok(json)
    }
}
