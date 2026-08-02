//! Rust code projection over authored [`.yscene`](SceneDocument) documents.
//!
//! The scene JSON remains persistence SoT. Projection files under
//! `src/scenes/<slug>/entities/` are a human-editable view: export from the
//! document, parse edits, and apply via Editor command transactions.

#![forbid(unsafe_code)]

mod diff;
mod export;
mod parse;

pub use diff::{ProjectionEdit, diff_projection};
pub use export::{
    ProjectionFile, ProjectionTree, entity_projection_relative, export_scene,
    projection_dir_relative,
};
pub use parse::{ParsedComponent, ParsedEntityProjection, parse_entity_file};

/// Schema marker embedded in entity projection headers.
pub const ENTITY_PROJECTION_SCHEMA: &str = "yuyib.entity-projection@1";
/// Schema marker embedded in scene `mod.rs` headers.
pub const SCENE_PROJECTION_SCHEMA: &str = "yuyib.scene-projection@1";

/// Schemas pretty-printed as typed field maps (others use `raw { … }`).
pub const TYPED_SCHEMAS: &[&str] = &[
    "yuyib.transform3d",
    "yuyib.local-transform3d",
    "yuyib.model3d",
    "yuyib.parent3d",
    "yuyib.directional-light3d",
];

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use yuyib_authoring::SceneDocument;

    use super::*;

    fn fixture_scene() -> SceneDocument {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../editor_tests/prj2/scenes/main.yscene");
        let text = fs::read_to_string(&path).expect("prj2 main.yscene");
        serde_json::from_str(&text).expect("parse main.yscene")
    }

    #[test]
    fn export_parse_round_trips_prj2_entities() {
        let document = fixture_scene();
        let tree = export_scene(&document, "scenes/main.yscene");
        assert!(!tree.files.is_empty());
        assert!(
            tree.files
                .iter()
                .any(|file| file.relative_path.ends_with("mod.rs"))
        );

        let mut parsed = Vec::new();
        for file in &tree.files {
            if !file.contents.contains("yuyib_entity!") {
                continue;
            }
            let entity = parse_entity_file(&file.contents).expect("parse exported entity");
            assert_eq!(entity.scene_guid, document.scene_guid.to_string());
            parsed.push(entity);
        }
        assert_eq!(parsed.len(), document.entities.len());

        let edits = diff_projection(&document, &parsed).expect("diff");
        assert!(
            edits.is_empty(),
            "round-trip should be a no-op, got {edits:?}"
        );
    }

    #[test]
    fn diff_detects_translation_change() {
        let document = fixture_scene();
        let tree = export_scene(&document, "scenes/main.yscene");
        let mut file = tree
            .files
            .iter()
            .find(|file| file.contents.contains("yuyib_entity!"))
            .cloned()
            .expect("entity file");
        file.contents = file.contents.replacen("17.28954315185547", "1.5", 1);
        let entity = parse_entity_file(&file.contents).expect("parse");
        let edits = diff_projection(&document, &[entity]).expect("diff");
        assert!(
            edits.iter().any(|edit| matches!(
                edit,
                ProjectionEdit::SetField { field_path, .. } if field_path.starts_with("translation")
            )),
            "expected translation field edit, got {edits:?}"
        );
    }
}
