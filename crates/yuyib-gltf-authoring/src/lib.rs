//! Authoring surface for glTF: settings JSON → [`ImportOptions`] and a
//! [`PreviewAdapter`] over production [`GltfSceneLoad`].
//!
//! This crate does **not** add a second decoder. Animation selection remains a
//! future PreviewFeature. Bounds, Collision, Normals, Tangents and UV overlays
//! are registered on the adapter and drawn by the Editor host via
//! `GizmoUnlitPass`. Mesh/material selection is available.
//! Preview cache keys honor `PreviewCachePolicy` (content hash + import settings).

#![forbid(unsafe_code)]

mod import_settings;
mod preview;

pub use import_settings::{
    GLTF_IMPORT_SETTINGS_SCHEMA, GltfImportSettings, GltfImportSettingsError, default_settings_json,
    parse_import_settings,
};
pub use preview::{GltfPreviewAdapter, register_gltf_preview};

use std::sync::Arc;

use yuyib_authoring::{AuthoringRegistry, RegistrationError};

/// Registers the glTF preview adapter. Call after foundation coverage marks
/// `yuyib.gltf-preview` as [`yuyib_authoring::CoverageStatus::Asset`].
///
/// # Errors
///
/// Forwards registry registration failures (missing/unavailable capability,
/// duplicate adapter).
pub fn register(registry: &mut AuthoringRegistry) -> Result<(), RegistrationError> {
    register_gltf_preview(registry, Arc::new(GltfPreviewAdapter::new()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf};
    use yuyib_authoring_yuyib::register_foundation;

    fn full_editor_registry() -> AuthoringRegistry {
        let mut registry = AuthoringRegistry::new();
        register_foundation(&mut registry).expect("foundation");
        register(&mut registry).expect("gltf preview adapter");
        registry
            .validate_coverage_gate()
            .expect("Asset evidence + preview adapter close the coverage gate");
        registry
    }

    fn golden_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/coverage-manifest.json")
    }

    #[test]
    fn foundation_plus_preview_adapter_closes_coverage_gate() {
        let _ = full_editor_registry();
    }

    #[test]
    fn foundation_alone_fails_coverage_gate_without_preview_adapter() {
        let mut registry = AuthoringRegistry::new();
        register_foundation(&mut registry).expect("foundation");
        assert!(matches!(
            registry.validate_coverage_gate(),
            Err(yuyib_authoring::CoverageGateError::MissingPreviewAdapter { .. })
        ));
    }

    #[test]
    fn coverage_manifest_export_round_trips_and_matches_golden() {
        let registry = full_editor_registry();
        let manifest = registry.coverage_manifest();
        let json = manifest.to_pretty_json().expect("export");

        let parsed: yuyib_authoring::CoverageManifest =
            serde_json::from_str(&json).expect("deserialize export");
        assert_eq!(parsed, manifest);

        let value: serde_json::Value = serde_json::from_str(&json).expect("json value");
        let object = value.as_object().expect("object root");
        assert_eq!(object.len(), 6);
        for key in [
            "capabilities",
            "components",
            "import_settings",
            "systems",
            "previews",
            "migrations",
        ] {
            assert!(object.contains_key(key), "missing section {key}");
        }
        // Canonical pretty export keeps struct field order (not BTreeMap order).
        assert!(
            json.starts_with("{\n  \"capabilities\":"),
            "pretty export should start with capabilities section"
        );
        assert!(
            object["capabilities"]
                .as_array()
                .expect("capabilities")
                .iter()
                .any(|entry| entry["id"] == "yuyib.gltf-preview"
                    && entry["asset_evidence"].is_object())
        );
        assert_eq!(object["previews"].as_array().expect("previews").len(), 1);

        let path = golden_path();
        if std::env::var_os("UPDATE_COVERAGE_GOLDEN").is_some() {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("fixtures dir");
            }
            fs::write(&path, &json).expect("write golden");
        }
        let golden = fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "missing coverage golden at {}: {error}. Re-run with UPDATE_COVERAGE_GOLDEN=1",
                path.display()
            )
        });
        assert_eq!(
            json, golden,
            "coverage manifest drifted from golden; review and UPDATE_COVERAGE_GOLDEN=1 if intentional"
        );
    }
}
