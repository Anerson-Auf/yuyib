//! Versioned glTF import-settings projection for authoring / PreviewRequest.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use yuyib_gltf::{ImportOptions, ImportPolicy};

/// Stable schema id registered as `yuyib.gltf-import-settings`.
pub const GLTF_IMPORT_SETTINGS_SCHEMA: &str = "yuyib.gltf-import-settings";

/// Persisted / PreviewRequest import settings (schema version 1).
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct GltfImportSettings {
    /// Production importer policy. Accepts aliases used by docs/UI.
    #[serde(default)]
    pub policy: GltfImportPolicySetting,
}

/// JSON string values for [`GltfImportSettings::policy`].
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GltfImportPolicySetting {
    /// [`ImportPolicy::Strict`] — full import contract.
    #[default]
    Default,
    /// Explicit alias of [`Self::Default`].
    Strict,
    /// [`ImportPolicy::StaticPreview`].
    StaticPreview,
    /// [`ImportPolicy::Skeletal`].
    Skeletal,
    /// [`ImportPolicy::SkeletalPreview`].
    SkeletalPreview,
}

impl GltfImportPolicySetting {
    #[must_use]
    pub const fn to_import_policy(self) -> ImportPolicy {
        match self {
            Self::Default | Self::Strict => ImportPolicy::Strict,
            Self::StaticPreview => ImportPolicy::StaticPreview,
            Self::Skeletal => ImportPolicy::Skeletal,
            Self::SkeletalPreview => ImportPolicy::SkeletalPreview,
        }
    }
}

impl GltfImportSettings {
    /// Builds production [`ImportOptions`] from authored settings.
    #[must_use]
    pub fn to_import_options(&self) -> ImportOptions {
        ImportOptions::default().with_policy(self.policy.to_import_policy())
    }
}

/// Failure parsing authored glTF import settings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GltfImportSettingsError {
    message: String,
}

impl GltfImportSettingsError {
    /// Builds an error with a human-readable message.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Returns the error message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for GltfImportSettingsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for GltfImportSettingsError {}

/// Empty-object defaults (`policy = default`).
#[must_use]
pub fn default_settings_json() -> Value {
    serde_json::to_value(GltfImportSettings::default()).expect("default settings serialize")
}

/// Parses PreviewRequest / `.yasset` import-settings JSON (v1).
///
/// # Errors
///
/// Returns when JSON is not an object matching [`GltfImportSettings`].
pub fn parse_import_settings(value: &Value) -> Result<GltfImportSettings, GltfImportSettingsError> {
    if value.is_null() {
        return Ok(GltfImportSettings::default());
    }
    serde_json::from_value(value.clone()).map_err(|error| {
        GltfImportSettingsError::new(format!(
            "yuyib.gltf-import-settings v1: {error} (expected object with optional string `policy`)"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn default_and_aliases_map_to_strict() {
        for policy in [
            GltfImportPolicySetting::Default,
            GltfImportPolicySetting::Strict,
        ] {
            assert_eq!(policy.to_import_policy(), ImportPolicy::Strict);
        }
    }

    #[test]
    fn parses_policy_strings() {
        let settings = parse_import_settings(&json!({ "policy": "skeletal_preview" }))
            .expect("parse skeletal_preview");
        assert_eq!(settings.policy, GltfImportPolicySetting::SkeletalPreview);
        assert_eq!(
            settings.to_import_options().policy,
            ImportPolicy::SkeletalPreview
        );
    }

    #[test]
    fn null_and_empty_object_are_defaults() {
        assert_eq!(
            parse_import_settings(&Value::Null).expect("null"),
            GltfImportSettings::default()
        );
        assert_eq!(
            parse_import_settings(&json!({})).expect("empty"),
            GltfImportSettings::default()
        );
    }

    #[test]
    fn rejects_unknown_policy() {
        assert!(parse_import_settings(&json!({ "policy": "nope" })).is_err());
    }
}
