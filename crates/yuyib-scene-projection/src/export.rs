//! Export `.yscene` documents into Rust projection files.

use std::fmt::Write as _;

use serde_json::{Map, Value};
use yuyib_authoring::{ComponentRecord, SceneDocument, SceneEntityRecord};

use crate::{ENTITY_PROJECTION_SCHEMA, SCENE_PROJECTION_SCHEMA, TYPED_SCHEMAS};

/// One relative file in a projection tree (paths use `/`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectionFile {
    /// Project-relative path using forward slashes.
    pub relative_path: String,
    /// UTF-8 file body.
    pub contents: String,
}

/// Full projection tree for one scene.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProjectionTree {
    /// Scene slug (`main` from `scenes/main.yscene`).
    pub scene_slug: String,
    /// Directory `src/scenes/<slug>` (forward slashes, no trailing slash).
    pub root_relative: String,
    /// Files to write (mod.rs + entity modules).
    pub files: Vec<ProjectionFile>,
}

/// Relative directory for a scene projection under `code_root`.
#[must_use]
pub fn projection_dir_relative(scene_path: &str) -> String {
    let slug = scene_slug(scene_path);
    format!("src/scenes/{slug}")
}

/// Project-relative (under `code_root`) path for one entity projection file.
#[must_use]
pub fn entity_projection_relative(scene_path: &str, entity: &SceneEntityRecord) -> String {
    format!(
        "{}/entities/{}.rs",
        projection_dir_relative(scene_path),
        entity_module_name(entity)
    )
}

/// Builds the projection file tree for `document`.
#[must_use]
pub fn export_scene(document: &SceneDocument, scene_path: &str) -> ProjectionTree {
    let scene_slug = scene_slug(scene_path);
    let root_relative = format!("src/scenes/{scene_slug}");
    let scene_guid = document.scene_guid.to_string();
    let mut files = Vec::new();
    let mut entity_mods = Vec::new();

    for entity in &document.entities {
        let module = entity_module_name(entity);
        entity_mods.push(module.clone());
        let relative_path = format!("{root_relative}/entities/{module}.rs");
        files.push(ProjectionFile {
            relative_path,
            contents: export_entity_file(document, entity),
        });
    }

    let mut mod_rs = String::new();
    let _ = writeln!(mod_rs, "//! {SCENE_PROJECTION_SCHEMA}");
    let _ = writeln!(mod_rs, "//! scene_guid = \"{scene_guid}\"");
    let _ = writeln!(mod_rs, "//! scene_path = \"{}\"", scene_path.replace('\\', "/"));
    let _ = writeln!(mod_rs);
    let _ = writeln!(mod_rs, "#![allow(non_snake_case)]");
    let _ = writeln!(mod_rs);
    let _ = writeln!(mod_rs, "pub mod entities;");
    files.insert(
        0,
        ProjectionFile {
            relative_path: format!("{root_relative}/mod.rs"),
            contents: mod_rs,
        },
    );

    let mut entities_mod = String::new();
    let _ = writeln!(
        entities_mod,
        "//! Entity projection modules for `{scene_slug}`."
    );
    let _ = writeln!(entities_mod, "#![allow(non_snake_case)]");
    let _ = writeln!(entities_mod);
    for module in &entity_mods {
        let _ = writeln!(entities_mod, "pub mod {module};");
    }
    files.insert(
        1,
        ProjectionFile {
            relative_path: format!("{root_relative}/entities/mod.rs"),
            contents: entities_mod,
        },
    );

    ProjectionTree {
        scene_slug,
        root_relative,
        files,
    }
}

fn export_entity_file(document: &SceneDocument, entity: &SceneEntityRecord) -> String {
    let scene_guid = document.scene_guid.to_string();
    let entity_guid = entity.guid.to_string();
    let name = entity.name.as_deref().unwrap_or("");
    let mut out = String::new();
    let _ = writeln!(out, "//! {ENTITY_PROJECTION_SCHEMA}");
    let _ = writeln!(out, "//! scene_guid = \"{scene_guid}\"");
    let _ = writeln!(out, "//! entity_guid = \"{entity_guid}\"");
    let _ = writeln!(out);
    let _ = writeln!(out, "yuyib_entity! {{");
    let _ = writeln!(out, "    name: {},", format_string(name));
    let _ = writeln!(out, "    components: {{");
    for component in &entity.components {
        write_component(&mut out, component);
    }
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out, "}}");
    out
}

fn write_component(out: &mut String, component: &ComponentRecord) {
    let schema = component.schema().as_str();
    let version = component.version().get();
    let typed = TYPED_SCHEMAS.iter().any(|candidate| *candidate == schema);
    if typed {
        let _ = writeln!(out, "        \"{schema}\" @ {version}: {{");
        write_typed_payload(out, component.payload(), "            ");
        let _ = writeln!(out, "        }},");
    } else {
        let _ = writeln!(out, "        \"{schema}\" @ {version}: raw {{");
        let pretty = serde_json::to_string_pretty(component.payload()).unwrap_or_else(|_| {
            component.payload().to_string()
        });
        for line in pretty.lines() {
            let _ = writeln!(out, "            {line}");
        }
        let _ = writeln!(out, "        }},");
    }
}

fn write_typed_payload(out: &mut String, payload: &Value, indent: &str) {
    let Some(object) = payload.as_object() else {
        let _ = writeln!(out, "{indent}// non-object payload");
        return;
    };
    let mut keys: Vec<_> = object.keys().cloned().collect();
    keys.sort();
    for key in keys {
        let Some(value) = object.get(&key) else {
            continue;
        };
        let _ = write!(out, "{indent}{key}: ");
        write_value(out, value);
        let _ = writeln!(out, ",");
    }
}

fn write_value(out: &mut String, value: &Value) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(flag) => {
            let _ = write!(out, "{flag}");
        }
        Value::Number(number) => {
            let _ = write!(out, "{number}");
        }
        Value::String(text) => out.push_str(&format_string(text)),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                write_value(out, item);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            let mut first = true;
            let mut keys: Vec<_> = map.keys().cloned().collect();
            keys.sort();
            for key in keys {
                if !first {
                    out.push_str(", ");
                }
                first = false;
                let _ = write!(out, "{key}: ");
                write_value(out, &map[&key]);
            }
            out.push('}');
        }
    }
}

fn format_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn scene_slug(scene_path: &str) -> String {
    let normalized = scene_path.replace('\\', "/");
    let stem_holder = PathStem(&normalized);
    let stem = stem_holder.file_stem();
    sanitize_ident(stem)
}

struct PathStem<'a>(&'a str);

impl PathStem<'_> {
    fn file_stem(&self) -> &str {
        let name = self.0.rsplit('/').next().unwrap_or(self.0);
        name.rsplit_once('.')
            .map(|(stem, _)| stem)
            .unwrap_or(name)
    }
}

fn entity_module_name(entity: &SceneEntityRecord) -> String {
    let slug = entity
        .name
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .map(sanitize_ident)
        .unwrap_or_else(|| "entity".to_owned());
    let short = short_guid(&entity.guid.to_string());
    format!("{slug}__{short}")
}

fn short_guid(guid: &str) -> String {
    guid.chars()
        .filter(|ch| ch.is_ascii_hexdigit())
        .take(8)
        .collect::<String>()
        .to_ascii_lowercase()
}

fn sanitize_ident(raw: &str) -> String {
    let mut out = String::new();
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if ch == '_' || ch == '-' {
            out.push('_');
        }
    }
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        "entity".to_owned()
    } else if trimmed
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_digit())
    {
        format!("e_{trimmed}")
    } else {
        trimmed.to_owned()
    }
}

/// Flattens a JSON object into dotted leaf paths for field.set diffs.
#[must_use]
pub fn flatten_payload_fields(payload: &Value) -> Map<String, Value> {
    let mut out = Map::new();
    flatten_into(&mut out, String::new(), payload);
    out
}

fn flatten_into(out: &mut Map<String, Value>, prefix: String, value: &Value) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let next = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                match child {
                    Value::Object(_) => flatten_into(out, next, child),
                    Value::Array(items)
                        if items.iter().any(|item| matches!(item, Value::Object(_))) =>
                    {
                        for (index, item) in items.iter().enumerate() {
                            flatten_into(out, format!("{next}.{index}"), item);
                        }
                    }
                    _ => {
                        out.insert(next, child.clone());
                    }
                }
            }
        }
        other => {
            if !prefix.is_empty() {
                out.insert(prefix, other.clone());
            }
        }
    }
}
