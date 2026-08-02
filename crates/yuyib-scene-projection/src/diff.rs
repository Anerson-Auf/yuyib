//! Diff parsed projections against the live scene document.

use std::str::FromStr;

use serde_json::Value;
use yuyib_authoring::{
    ComponentRecord, ComponentSchemaId, EntityGuid, SceneDocument, SchemaVersion,
};

use crate::{
    export::flatten_payload_fields,
    parse::ParsedEntityProjection,
};

/// One document mutation derived from projection diff.
#[derive(Clone, Debug, PartialEq)]
pub enum ProjectionEdit {
    /// Rename an existing entity.
    Rename {
        /// Target entity GUID.
        entity_guid: String,
        /// New display name (`None` clears the label).
        name: Option<String>,
    },
    /// Add a missing component with full payload.
    AddComponent {
        /// Target entity GUID.
        entity_guid: String,
        /// Component schema id.
        schema: String,
        /// Schema version.
        version: u32,
        /// Full JSON payload.
        payload: Value,
    },
    /// Remove a component absent from the projection file.
    RemoveComponent {
        /// Target entity GUID.
        entity_guid: String,
        /// Component schema id.
        schema: String,
    },
    /// Set one dotted JSON field on an existing component.
    SetField {
        /// Target entity GUID.
        entity_guid: String,
        /// Component schema id.
        schema: String,
        /// Dotted JSON field path.
        field_path: String,
        /// Replacement JSON value.
        value: Value,
    },
}

/// Diffs `parsed` entity projections against `document`.
///
/// Only entities present in both sides (by GUID) are synced. Unknown GUIDs in
/// files and missing projection files are reported as errors (no silent create/delete).
///
/// # Errors
///
/// Returns when a file targets another scene, an unknown entity, or an invalid schema id.
pub fn diff_projection(
    document: &SceneDocument,
    parsed: &[ParsedEntityProjection],
) -> Result<Vec<ProjectionEdit>, String> {
    let scene_guid = document.scene_guid.to_string();
    let mut edits = Vec::new();
    let mut seen = std::collections::BTreeSet::new();

    for entity in parsed {
        if entity.scene_guid != scene_guid {
            return Err(format!(
                "projection scene_guid {} does not match open scene {scene_guid}",
                entity.scene_guid
            ));
        }
        let guid = EntityGuid::from_str(&entity.entity_guid)
            .map_err(|error| format!("invalid entity_guid {}: {error}", entity.entity_guid))?;
        if !seen.insert(guid) {
            return Err(format!(
                "duplicate entity projection for {}",
                entity.entity_guid
            ));
        }
        let Some(authored) = document.entities.iter().find(|record| record.guid == guid) else {
            return Err(format!(
                "projection entity {} is not in the open scene (create/delete via files is out of scope for v1)",
                entity.entity_guid
            ));
        };

        if authored.name != entity.name {
            edits.push(ProjectionEdit::Rename {
                entity_guid: entity.entity_guid.clone(),
                name: entity.name.clone(),
            });
        }

        let authored_schemas: std::collections::BTreeSet<_> = authored
            .components
            .iter()
            .map(|component| component.schema().as_str().to_owned())
            .collect();
        let mut projected_schemas = std::collections::BTreeSet::new();

        for component in &entity.components {
            let _ = ComponentSchemaId::new(&component.schema)
                .map_err(|error| format!("invalid schema {}: {error}", component.schema))?;
            SchemaVersion::new(component.version).map_err(|error| {
                format!("component {} version: {error}", component.schema)
            })?;
            projected_schemas.insert(component.schema.clone());

            match authored
                .components
                .iter()
                .find(|record| record.schema().as_str() == component.schema)
            {
                None => edits.push(ProjectionEdit::AddComponent {
                    entity_guid: entity.entity_guid.clone(),
                    schema: component.schema.clone(),
                    version: component.version,
                    payload: component.payload.clone(),
                }),
                Some(existing) => {
                    push_field_edits(
                        &mut edits,
                        &entity.entity_guid,
                        &component.schema,
                        existing,
                        &component.payload,
                    );
                }
            }
        }

        for schema in authored_schemas.difference(&projected_schemas) {
            edits.push(ProjectionEdit::RemoveComponent {
                entity_guid: entity.entity_guid.clone(),
                schema: schema.clone(),
            });
        }
    }

    Ok(edits)
}

fn push_field_edits(
    edits: &mut Vec<ProjectionEdit>,
    entity_guid: &str,
    schema: &str,
    existing: &ComponentRecord,
    next_payload: &Value,
) {
    let before = flatten_payload_fields(existing.payload());
    let after = flatten_payload_fields(next_payload);
    let mut keys: std::collections::BTreeSet<_> = before.keys().cloned().collect();
    keys.extend(after.keys().cloned());
    for key in keys {
        let left = before.get(&key);
        let right = after.get(&key);
        match (left, right) {
            (Some(previous), Some(next)) if previous == next => {}
            (_, Some(next)) => edits.push(ProjectionEdit::SetField {
                entity_guid: entity_guid.to_owned(),
                schema: schema.to_owned(),
                field_path: key,
                value: next.clone(),
            }),
            (Some(_), None) => {
                // Field removed in projection — set JSON null when parent is object-compatible.
                edits.push(ProjectionEdit::SetField {
                    entity_guid: entity_guid.to_owned(),
                    schema: schema.to_owned(),
                    field_path: key,
                    value: Value::Null,
                });
            }
            (None, None) => {}
        }
    }
}
