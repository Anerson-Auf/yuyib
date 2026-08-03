use std::{any::Any, error::Error, fmt, str::FromStr};

use serde_json::{Value, json};
use yuyib_authoring::{
    CommandError, CommandHistory, CommandTransaction, ComponentRecord, ComponentSchemaId,
    DocumentCommand, EntityGuid, Revision, SCENE_FORMAT_VERSION, SceneDocument, SceneEntityRecord,
    SceneFormatError, SceneGuid, SchemaVersion, TransactionError,
};
use yuyib_editor_core::{DocumentError, DocumentRevision, ProjectDocumentStore};

use crate::bridge::{SceneCommandRequest, SceneCreateRequest, SceneEditRequest};

pub struct SceneSession {
    path: String,
    document: SceneDocument,
    saved_document: Option<SceneDocument>,
    file_revision: Option<DocumentRevision>,
    history: CommandHistory<SceneDocument>,
    dirty: bool,
    read_only: bool,
}

impl SceneSession {
    pub fn open(documents: &ProjectDocumentStore, path: String) -> Result<Self, SceneSessionError> {
        let snapshot = documents
            .load_json::<SceneDocument>(&path)
            .map_err(SceneSessionError::Document)?;
        snapshot
            .value
            .validate()
            .map_err(SceneSessionError::Format)?;
        let saved_document = snapshot.value.clone();
        let mut document = snapshot.value;
        let coerced = yuyib_game_3d_authoring::coerce_document_transform_payloads(&mut document);
        let read_only = saved_document.format_version.get() > SCENE_FORMAT_VERSION;
        Ok(Self {
            path,
            document,
            saved_document: Some(saved_document),
            file_revision: Some(snapshot.revision),
            history: CommandHistory::new(),
            // Numeric-string TRS from disk should be rewritten on the next save.
            dirty: coerced > 0 && !read_only,
            read_only,
        })
    }

    pub fn create(request: SceneCreateRequest) -> Result<Self, SceneSessionError> {
        let mut document = SceneDocument::new(
            SchemaVersion::new(SCENE_FORMAT_VERSION)
                .expect("the current Editor scene format version is non-zero"),
        );
        if let Some(scene_guid) = request.scene_guid {
            document.scene_guid = SceneGuid::from_str(&scene_guid)
                .map_err(|error| SceneSessionError::Invalid(error.to_string()))?;
        }
        Ok(Self {
            path: request.path,
            document,
            saved_document: None,
            file_revision: None,
            history: CommandHistory::new(),
            dirty: true,
            read_only: false,
        })
    }

    /// Seeds a visible starter cube (Transform3d + Model3d) for empty new scenes.
    pub fn seed_starter_cube(&mut self) -> Result<(), SceneMutationError> {
        if !self.document.entities.is_empty() {
            return Ok(());
        }
        let entity = EntityGuid::new();
        let transform = default_transform3d_component()?;
        let model = default_component_record("yuyib.model3d")?;
        let mut model = model;
        {
            let version = model.version();
            let mut payload = model.payload().clone();
            if let Some(object) = payload.as_object_mut() {
                object.insert("model".to_owned(), json!("builtin:cube"));
            }
            model.replace_payload(version, payload);
        }
        self.document.entities.push(SceneEntityRecord {
            guid: entity,
            name: Some("Cube".to_owned()),
            components: vec![transform, model],
            extensions: Default::default(),
        });
        self.dirty = true;
        Ok(())
    }

    pub fn save(
        &mut self,
        documents: &ProjectDocumentStore,
    ) -> Result<DocumentRevision, SceneSessionError> {
        if self.read_only {
            return Err(SceneSessionError::Invalid(format!(
                "scene container version {} is newer than the Editor mutation version {SCENE_FORMAT_VERSION}; the document is read-only",
                self.document.format_version.get()
            )));
        }
        // Persist real JSON numbers so Play/external tools never see `"0"` strings.
        let _ = yuyib_game_3d_authoring::coerce_document_transform_payloads(&mut self.document);
        self.document
            .validate()
            .map_err(SceneSessionError::Format)?;
        let revision = documents
            .save_json(&self.path, &self.document, self.file_revision)
            .map_err(SceneSessionError::Document)?;
        self.file_revision = Some(revision);
        self.saved_document = Some(self.document.clone());
        self.dirty = false;
        Ok(revision)
    }

    pub fn apply(&mut self, request: SceneCommandRequest) -> Result<Revision, SceneMutationError> {
        if self.read_only {
            return Err(SceneMutationError::Invalid(format!(
                "scene container version {} is newer than the Editor mutation version {}",
                self.document.format_version.get(),
                SCENE_FORMAT_VERSION
            )));
        }
        validate_transaction_id(&request.transaction_id)?;
        let expected_revision = Revision::new(request.base_revision);
        let revision = match request.command {
            SceneEditRequest::RenameEntity { entity_guid, name } => {
                validate_name(name.as_deref())?;
                let entity = parse_entity_guid(&entity_guid)?;
                let before = find_entity(&self.document, entity)?.name.clone();
                let transaction = CommandTransaction::new("Rename entity")
                    .with_merge_key(format!("entity/{entity}/name"))
                    .push(RenameEntity {
                        entity,
                        before,
                        after: name,
                    });
                self.history
                    .commit(&mut self.document, expected_revision, transaction)?
            }
            SceneEditRequest::CreateEntity {
                name,
                with_transform3d,
            } => {
                validate_name(name.as_deref())?;
                let entity = EntityGuid::new();
                let components = if with_transform3d {
                    vec![default_transform3d_component()?]
                } else {
                    Vec::new()
                };
                let transaction = CommandTransaction::new("Create entity").push(CreateEntity {
                    entity,
                    name,
                    components,
                });
                self.history
                    .commit(&mut self.document, expected_revision, transaction)?
            }
            SceneEditRequest::DeleteEntity { entity_guid } => {
                let entity = parse_entity_guid(&entity_guid)?;
                let index = self
                    .document
                    .entities
                    .iter()
                    .position(|record| record.guid == entity)
                    .ok_or_else(|| {
                        SceneMutationError::Invalid(format!("scene entity {entity} was not found"))
                    })?;
                let deleted = self.document.entities[index].clone();
                let transaction = CommandTransaction::new("Delete entity").push(DeleteEntity {
                    entity,
                    deleted,
                    index,
                });
                self.history
                    .commit(&mut self.document, expected_revision, transaction)?
            }
            SceneEditRequest::AddComponent {
                entity_guid,
                component_id,
            } => {
                let entity = parse_entity_guid(&entity_guid)?;
                let component = default_component_record(&component_id)?;
                if find_component(&self.document, entity, component.schema()).is_ok() {
                    return Err(SceneMutationError::Invalid(format!(
                        "component {component_id} already exists on scene entity {entity}"
                    )));
                }
                let transaction = CommandTransaction::new("Add component")
                    .push(AddComponent { entity, component });
                self.history
                    .commit(&mut self.document, expected_revision, transaction)?
            }
            SceneEditRequest::RemoveComponent {
                entity_guid,
                component_id,
            } => {
                let entity = parse_entity_guid(&entity_guid)?;
                let schema = ComponentSchemaId::new(&component_id)
                    .map_err(|error| SceneMutationError::Invalid(error.to_string()))?;
                let entity_record = find_entity(&self.document, entity)?;
                let index = entity_record
                    .components
                    .iter()
                    .position(|record| record.schema() == &schema)
                    .ok_or_else(|| {
                        SceneMutationError::Invalid(format!(
                            "component {component_id} was not found on scene entity {entity}"
                        ))
                    })?;
                let removed = entity_record.components[index].clone();
                let transaction =
                    CommandTransaction::new("Remove component").push(RemoveComponent {
                        entity,
                        schema,
                        removed,
                        index,
                    });
                self.history
                    .commit(&mut self.document, expected_revision, transaction)?
            }
            SceneEditRequest::SetComponentField {
                entity_guid,
                component_id,
                field_path,
                value,
            } => {
                validate_field(&field_path)?;
                let entity = parse_entity_guid(&entity_guid)?;
                let schema = ComponentSchemaId::new(component_id)
                    .map_err(|error| SceneMutationError::Invalid(error.to_string()))?;
                let component = find_component(&self.document, entity, &schema)?;
                let before = read_object_field(component, &field_path)?;
                let transaction = CommandTransaction::new("Set component field")
                    .with_merge_key(format!(
                        "entity/{entity}/component/{schema}/field/{field_path}"
                    ))
                    .push(SetComponentField {
                        entity,
                        schema,
                        field: field_path,
                        before,
                        after: value,
                    });
                self.history
                    .commit(&mut self.document, expected_revision, transaction)?
            }
            SceneEditRequest::Undo => self.history.undo(&mut self.document, expected_revision)?,
            SceneEditRequest::Redo => self.history.redo(&mut self.document, expected_revision)?,
        };
        self.dirty = self.saved_document.as_ref() != Some(&self.document);
        Ok(revision)
    }

    /// Commits a full Transform3d snapshot as one undo step (viewport drag end).
    ///
    /// When the entity also carries `yuyib.local-transform3d` (parented preview),
    /// both schemas are updated in the same transaction so rematerialize does not
    /// restore a stale local pose.
    pub fn commit_transform3d(
        &mut self,
        expected_revision: u64,
        entity_guid: &str,
        translation: [f32; 3],
        rotation: [f32; 4],
        scale: [f32; 3],
    ) -> Result<Revision, SceneMutationError> {
        if self.read_only {
            return Err(SceneMutationError::Invalid(format!(
                "scene container version {} is newer than the Editor mutation version {}",
                self.document.format_version.get(),
                SCENE_FORMAT_VERSION
            )));
        }
        let entity = parse_entity_guid(entity_guid)?;
        let field_values = [
            ("translation.x", json!(translation[0])),
            ("translation.y", json!(translation[1])),
            ("translation.z", json!(translation[2])),
            ("rotation.x", json!(rotation[0])),
            ("rotation.y", json!(rotation[1])),
            ("rotation.z", json!(rotation[2])),
            ("rotation.w", json!(rotation[3])),
            ("scale.x", json!(scale[0])),
            ("scale.y", json!(scale[1])),
            ("scale.z", json!(scale[2])),
        ];
        let mut transaction = CommandTransaction::new("Viewport transform");
        let mut matched_schema = false;
        for schema_name in ["yuyib.transform3d", "yuyib.local-transform3d"] {
            let schema = ComponentSchemaId::new(schema_name)
                .map_err(|error| SceneMutationError::Invalid(error.to_string()))?;
            let Ok(component) = find_component(&self.document, entity, &schema) else {
                continue;
            };
            matched_schema = true;
            for (field, value) in &field_values {
                validate_field(field)?;
                let before = read_object_field(component, field)?;
                if before.as_ref() == Some(value) {
                    continue;
                }
                transaction = transaction.push(SetComponentField {
                    entity,
                    schema: schema.clone(),
                    field: (*field).to_owned(),
                    before,
                    after: value.clone(),
                });
            }
        }
        if !matched_schema {
            return Err(SceneMutationError::Invalid(format!(
                "scene entity {entity_guid} has no Transform3d / LocalTransform3d to edit"
            )));
        }
        if transaction.is_empty() {
            return Ok(self.history.revision());
        }
        let revision = self.history.commit(
            &mut self.document,
            Revision::new(expected_revision),
            transaction,
        )?;
        self.dirty = self.saved_document.as_ref() != Some(&self.document);
        Ok(revision)
    }

    /// Applies Rust projection edits as a single undoable transaction.
    ///
    /// Maps [`yuyib_scene_projection::ProjectionEdit`] 1:1 onto rename /
    /// component add-remove / field-set commands. Empty edit lists are a no-op.
    pub fn apply_projection_edits(
        &mut self,
        expected_revision: u64,
        edits: &[yuyib_scene_projection::ProjectionEdit],
    ) -> Result<Revision, SceneMutationError> {
        if self.read_only {
            return Err(SceneMutationError::Invalid(format!(
                "scene container version {} is newer than the Editor mutation version {}",
                self.document.format_version.get(),
                SCENE_FORMAT_VERSION
            )));
        }
        if edits.is_empty() {
            return Ok(self.history.revision());
        }

        let mut transaction = CommandTransaction::new("Apply projection code");
        let mut pending_removes: Vec<(EntityGuid, ComponentSchemaId, ComponentRecord, usize)> =
            Vec::new();

        for edit in edits {
            match edit {
                yuyib_scene_projection::ProjectionEdit::Rename { entity_guid, name } => {
                    validate_name(name.as_deref())?;
                    let entity = parse_entity_guid(entity_guid)?;
                    let before = find_entity(&self.document, entity)?.name.clone();
                    if before == *name {
                        continue;
                    }
                    transaction = transaction.push(RenameEntity {
                        entity,
                        before,
                        after: name.clone(),
                    });
                }
                yuyib_scene_projection::ProjectionEdit::AddComponent {
                    entity_guid,
                    schema,
                    version,
                    payload,
                } => {
                    let entity = parse_entity_guid(entity_guid)?;
                    let schema_id = ComponentSchemaId::new(schema)
                        .map_err(|error| SceneMutationError::Invalid(error.to_string()))?;
                    if find_component(&self.document, entity, &schema_id).is_ok() {
                        return Err(SceneMutationError::Invalid(format!(
                            "component {schema} already exists on scene entity {entity}"
                        )));
                    }
                    let schema_version = SchemaVersion::new(*version)
                        .map_err(|error| SceneMutationError::Invalid(error.to_string()))?;
                    let component =
                        ComponentRecord::new(schema_id, schema_version, payload.clone());
                    transaction = transaction.push(AddComponent { entity, component });
                }
                yuyib_scene_projection::ProjectionEdit::RemoveComponent {
                    entity_guid,
                    schema,
                } => {
                    let entity = parse_entity_guid(entity_guid)?;
                    let schema_id = ComponentSchemaId::new(schema)
                        .map_err(|error| SceneMutationError::Invalid(error.to_string()))?;
                    let entity_record = find_entity(&self.document, entity)?;
                    let index = entity_record
                        .components
                        .iter()
                        .position(|record| record.schema() == &schema_id)
                        .ok_or_else(|| {
                            SceneMutationError::Invalid(format!(
                                "component {schema} was not found on scene entity {entity}"
                            ))
                        })?;
                    let removed = entity_record.components[index].clone();
                    pending_removes.push((entity, schema_id, removed, index));
                }
                yuyib_scene_projection::ProjectionEdit::SetField {
                    entity_guid,
                    schema,
                    field_path,
                    value,
                } => {
                    validate_field(field_path)?;
                    let entity = parse_entity_guid(entity_guid)?;
                    let schema_id = ComponentSchemaId::new(schema)
                        .map_err(|error| SceneMutationError::Invalid(error.to_string()))?;
                    let component = find_component(&self.document, entity, &schema_id)?;
                    let before = read_object_field(component, field_path)?;
                    if before.as_ref() == Some(value) {
                        continue;
                    }
                    transaction = transaction.push(SetComponentField {
                        entity,
                        schema: schema_id,
                        field: field_path.clone(),
                        before,
                        after: value.clone(),
                    });
                }
            }
        }

        // Apply removals highest-index-first so stored indices stay valid within
        // one transaction when several components leave the same entity.
        pending_removes.sort_by(|left, right| right.3.cmp(&left.3));
        for (entity, schema, removed, index) in pending_removes {
            transaction = transaction.push(RemoveComponent {
                entity,
                schema,
                removed,
                index,
            });
        }

        if transaction.is_empty() {
            return Ok(self.history.revision());
        }
        let revision = self.history.commit(
            &mut self.document,
            Revision::new(expected_revision),
            transaction,
        )?;
        self.dirty = self.saved_document.as_ref() != Some(&self.document);
        Ok(revision)
    }

    /// Applies script interaction intents as one undoable transaction.
    ///
    /// [`yuyib_scene_interaction::SceneInteractionIntent::EmitSignal`] does not
    /// mutate the document; signals are returned in the batch result for the
    /// host to publish. Document-facing intents share validators with Inspector.
    pub fn apply_interaction_intents(
        &mut self,
        expected_revision: u64,
        intents: &[yuyib_scene_interaction::SceneInteractionIntent],
    ) -> Result<
        (
            Revision,
            yuyib_scene_interaction::SceneInteractionBatchResult,
        ),
        SceneMutationError,
    > {
        use yuyib_scene_interaction::{
            SceneInteractionBatchResult, SceneInteractionIntent, SceneInteractionSignal,
            TransformSpace, editor_capabilities, translation_field_writes, translation_schemas,
            validate_intent,
        };

        if self.read_only {
            return Err(SceneMutationError::Invalid(format!(
                "scene container version {} is newer than the Editor mutation version {}",
                self.document.format_version.get(),
                SCENE_FORMAT_VERSION
            )));
        }

        let caps = editor_capabilities();
        let mut result = SceneInteractionBatchResult::empty(intents.len());
        let mut transaction = CommandTransaction::new("Scene interaction");

        for intent in intents {
            validate_intent(intent).map_err(SceneMutationError::Invalid)?;
            if !caps.supports(intent) {
                return Err(SceneMutationError::Invalid(caps.unsupported_message(intent)));
            }
            match intent {
                SceneInteractionIntent::EmitSignal { name, payload } => {
                    result
                        .signals
                        .push(SceneInteractionSignal::new(name.clone(), payload.clone()));
                    result.applied += 1;
                }
                SceneInteractionIntent::SetTranslation {
                    entity,
                    translation,
                    space,
                } => {
                    let fields = translation_field_writes(*translation);
                    let mut matched = false;
                    let mut wrote = false;
                    for schema_name in translation_schemas(*space) {
                        let schema = ComponentSchemaId::new(*schema_name)
                            .map_err(|error| SceneMutationError::Invalid(error.to_string()))?;
                        let Ok(component) = find_component(&self.document, *entity, &schema) else {
                            continue;
                        };
                        matched = true;
                        for (field, value) in &fields {
                            validate_field(field)?;
                            let before = read_object_field(component, field)?;
                            if before.as_ref() == Some(value) {
                                continue;
                            }
                            transaction = transaction.push(SetComponentField {
                                entity: *entity,
                                schema: schema.clone(),
                                field: field.clone(),
                                before,
                                after: value.clone(),
                            });
                            wrote = true;
                        }
                        // Local space: stop after first matching schema in preference order.
                        // World space: mirror onto Local when both exist (gizmo parity).
                        if matches!(space, TransformSpace::Local) {
                            break;
                        }
                    }
                    if !matched {
                        return Err(SceneMutationError::Invalid(format!(
                            "scene entity {entity} has no Transform3d / LocalTransform3d for SetTranslation"
                        )));
                    }
                    if wrote {
                        result.applied += 1;
                    }
                }
                SceneInteractionIntent::SetComponentField {
                    entity,
                    schema,
                    field_path,
                    value,
                } => {
                    validate_field(field_path)?;
                    let schema_id = ComponentSchemaId::new(schema)
                        .map_err(|error| SceneMutationError::Invalid(error.to_string()))?;
                    let component = find_component(&self.document, *entity, &schema_id)?;
                    let before = read_object_field(component, field_path)?;
                    if before.as_ref() == Some(value) {
                        continue;
                    }
                    transaction = transaction.push(SetComponentField {
                        entity: *entity,
                        schema: schema_id,
                        field: field_path.clone(),
                        before,
                        after: value.clone(),
                    });
                    result.applied += 1;
                }
                SceneInteractionIntent::AddComponent {
                    entity,
                    schema,
                    version,
                    payload,
                } => {
                    let schema_id = ComponentSchemaId::new(schema)
                        .map_err(|error| SceneMutationError::Invalid(error.to_string()))?;
                    if find_component(&self.document, *entity, &schema_id).is_ok() {
                        return Err(SceneMutationError::Invalid(format!(
                            "component {schema} already exists on scene entity {entity}"
                        )));
                    }
                    let component = match payload {
                        Some(payload) => {
                            let schema_version = SchemaVersion::new(version.unwrap_or(1))
                                .map_err(|error| SceneMutationError::Invalid(error.to_string()))?;
                            ComponentRecord::new(schema_id, schema_version, payload.clone())
                        }
                        None => default_component_record(schema)?,
                    };
                    transaction = transaction.push(AddComponent {
                        entity: *entity,
                        component,
                    });
                    result.applied += 1;
                }
            }
        }

        if transaction.is_empty() {
            return Ok((self.history.revision(), result));
        }
        let revision = self.history.commit(
            &mut self.document,
            Revision::new(expected_revision),
            transaction,
        )?;
        self.dirty = self.saved_document.as_ref() != Some(&self.document);
        Ok((revision, result))
    }

    /// Applies Play Mode TRS report fields for Transform3d / LocalTransform3d.
    ///
    /// Only fields that already exist on the authored component are written.
    /// Empty / identical values are skipped. One undoable transaction.
    pub fn apply_play_transform_report(
        &mut self,
        expected_revision: u64,
        changes: &[(String, String, Vec<(String, Value)>)],
    ) -> Result<Revision, SceneMutationError> {
        if self.read_only {
            return Err(SceneMutationError::Invalid(format!(
                "scene container version {} is newer than the Editor mutation version {}",
                self.document.format_version.get(),
                SCENE_FORMAT_VERSION
            )));
        }
        let mut transaction = CommandTransaction::new("Apply Play Changes");
        for (entity_guid, component_id, fields) in changes {
            if !matches!(
                component_id.as_str(),
                "yuyib.transform3d" | "yuyib.local-transform3d"
            ) {
                continue;
            }
            let entity = parse_entity_guid(entity_guid)?;
            let schema = ComponentSchemaId::new(component_id.as_str())
                .map_err(|error| SceneMutationError::Invalid(error.to_string()))?;
            let Ok(component) = find_component(&self.document, entity, &schema) else {
                continue;
            };
            for (field, value) in fields {
                validate_field(field)?;
                let before = read_object_field(component, field)?;
                if before.as_ref() == Some(value) {
                    continue;
                }
                transaction = transaction.push(SetComponentField {
                    entity,
                    schema: schema.clone(),
                    field: field.clone(),
                    before,
                    after: value.clone(),
                });
            }
        }
        if transaction.is_empty() {
            return Ok(self.history.revision());
        }
        let revision = self.history.commit(
            &mut self.document,
            Revision::new(expected_revision),
            transaction,
        )?;
        self.dirty = self.saved_document.as_ref() != Some(&self.document);
        Ok(revision)
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub const fn document(&self) -> &SceneDocument {
        &self.document
    }

    pub const fn file_revision(&self) -> Option<DocumentRevision> {
        self.file_revision
    }

    pub const fn history_revision(&self) -> Revision {
        self.history.revision()
    }

    pub fn undo_len(&self) -> usize {
        self.history.undo_len()
    }

    pub fn redo_len(&self) -> usize {
        self.history.redo_len()
    }

    pub const fn history_poisoned(&self) -> bool {
        self.history.is_poisoned()
    }

    pub const fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub const fn is_read_only(&self) -> bool {
        self.read_only
    }
}

#[derive(Debug)]
pub enum SceneSessionError {
    Document(DocumentError),
    Format(SceneFormatError),
    Invalid(String),
}

impl fmt::Display for SceneSessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Document(error) => error.fmt(formatter),
            Self::Format(error) => error.fmt(formatter),
            Self::Invalid(error) => formatter.write_str(error),
        }
    }
}

impl Error for SceneSessionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Document(error) => Some(error),
            Self::Format(error) => Some(error),
            Self::Invalid(_) => None,
        }
    }
}

#[derive(Debug)]
pub enum SceneMutationError {
    Invalid(String),
    Transaction(TransactionError),
}

impl fmt::Display for SceneMutationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(error) => formatter.write_str(error),
            Self::Transaction(error) => error.fmt(formatter),
        }
    }
}

impl Error for SceneMutationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Transaction(error) => Some(error),
            Self::Invalid(_) => None,
        }
    }
}

impl From<TransactionError> for SceneMutationError {
    fn from(error: TransactionError) -> Self {
        Self::Transaction(error)
    }
}

fn parse_entity_guid(value: &str) -> Result<EntityGuid, SceneMutationError> {
    EntityGuid::from_str(value).map_err(|error| SceneMutationError::Invalid(error.to_string()))
}

fn validate_name(name: Option<&str>) -> Result<(), SceneMutationError> {
    if name.is_some_and(|name| name.len() > 256 || name.chars().any(char::is_control)) {
        return Err(SceneMutationError::Invalid(
            "entity name must contain at most 256 bytes and no controls".to_owned(),
        ));
    }
    Ok(())
}

fn validate_transaction_id(transaction_id: &str) -> Result<(), SceneMutationError> {
    if transaction_id.is_empty()
        || transaction_id.len() > 128
        || transaction_id.chars().any(char::is_control)
    {
        return Err(SceneMutationError::Invalid(
            "transaction ID must contain 1..=128 bytes and no controls".to_owned(),
        ));
    }
    Ok(())
}

fn validate_field(field: &str) -> Result<(), SceneMutationError> {
    if field.is_empty() || field.len() > 256 || field.chars().any(char::is_control) {
        return Err(SceneMutationError::Invalid(
            "component JSON field must contain 1..=256 bytes and no controls".to_owned(),
        ));
    }
    Ok(())
}

fn default_transform3d_component() -> Result<ComponentRecord, SceneMutationError> {
    default_component_record("yuyib.transform3d")
}

fn default_component_record(component_id: &str) -> Result<ComponentRecord, SceneMutationError> {
    let schema = ComponentSchemaId::new(component_id)
        .map_err(|error| SceneMutationError::Invalid(error.to_string()))?;
    let version =
        SchemaVersion::new(1).map_err(|error| SceneMutationError::Invalid(error.to_string()))?;
    let payload = match component_id {
        "yuyib.transform3d" | "yuyib.local-transform3d" => json!({
            "translation": [0.0, 0.0, 0.0],
            "rotation": [0.0, 0.0, 0.0, 1.0],
            "scale": [1.0, 1.0, 1.0]
        }),
        "yuyib.parent3d" => json!({ "parent": null }),
        "yuyib.model3d" => json!({
            "model": null,
            "mesh": null,
            "visible": true,
            "render_order": 0
        }),
        "yuyib.directional-light3d" => json!({
            "direction": [0.0, -1.0, 0.0],
            "color": [1.0, 0.95, 0.9],
            "illuminance_lux": 8.0,
            "enabled": true
        }),
        "yuyib.render3d" => json!({ "draw": true }),
        "yuyib.collision3d" => json!({
            "enabled": true,
            "layer": "",
            "collide_with": ""
        }),
        "yuyib.interactable" => json!({
            "interaction": "world.interact",
            "enabled": true,
            "max_distance": 3.0
        }),
        "yuyib.trigger" => json!({
            "trigger": "level.trigger",
            "enabled": true,
            "radius": 1.0
        }),
        _ => {
            return Err(SceneMutationError::Invalid(format!(
                "component {component_id} cannot be added until its typed adapter is registered"
            )));
        }
    };
    Ok(ComponentRecord::new(schema, version, payload))
}

fn find_entity(
    document: &SceneDocument,
    entity: EntityGuid,
) -> Result<&yuyib_authoring::SceneEntityRecord, SceneMutationError> {
    document
        .entities
        .iter()
        .find(|record| record.guid == entity)
        .ok_or_else(|| SceneMutationError::Invalid(format!("scene entity {entity} was not found")))
}

fn find_entity_mut(
    document: &mut SceneDocument,
    entity: EntityGuid,
) -> Result<&mut yuyib_authoring::SceneEntityRecord, CommandError> {
    document
        .entities
        .iter_mut()
        .find(|record| record.guid == entity)
        .ok_or_else(|| CommandError::new(format!("scene entity {entity} was not found")))
}

fn find_component<'document>(
    document: &'document SceneDocument,
    entity: EntityGuid,
    schema: &ComponentSchemaId,
) -> Result<&'document ComponentRecord, SceneMutationError> {
    find_entity(document, entity)?
        .components
        .iter()
        .find(|record| record.schema() == schema)
        .ok_or_else(|| {
            SceneMutationError::Invalid(format!(
                "component {schema} was not found on scene entity {entity}"
            ))
        })
}

fn find_component_mut<'document>(
    document: &'document mut SceneDocument,
    entity: EntityGuid,
    schema: &ComponentSchemaId,
) -> Result<&'document mut ComponentRecord, CommandError> {
    find_entity_mut(document, entity)?
        .components
        .iter_mut()
        .find(|record| record.schema() == schema)
        .ok_or_else(|| {
            CommandError::new(format!(
                "component {schema} was not found on scene entity {entity}"
            ))
        })
}

fn read_object_field(
    component: &ComponentRecord,
    field: &str,
) -> Result<Option<Value>, SceneMutationError> {
    read_json_field(component.payload(), field)
        .map(clone_optional_json_value)
        .map_err(|error| {
            SceneMutationError::Invalid(format!("component {}: {error}", component.schema()))
        })
}

fn clone_optional_json_value(value: Option<&Value>) -> Option<Value> {
    value.cloned()
}

struct RenameEntity {
    entity: EntityGuid,
    before: Option<String>,
    after: Option<String>,
}

impl DocumentCommand<SceneDocument> for RenameEntity {
    #[allow(clippy::unnecessary_literal_bound)]
    fn label(&self) -> &str {
        "rename entity"
    }

    fn apply(&mut self, document: &mut SceneDocument) -> Result<(), CommandError> {
        let entity = find_entity_mut(document, self.entity)?;
        if entity.name != self.before {
            return Err(CommandError::new(
                "entity name changed before command apply",
            ));
        }
        entity.name.clone_from(&self.after);
        Ok(())
    }

    fn revert(&mut self, document: &mut SceneDocument) -> Result<(), CommandError> {
        let entity = find_entity_mut(document, self.entity)?;
        if entity.name != self.after {
            return Err(CommandError::new(
                "entity name changed before command revert",
            ));
        }
        entity.name.clone_from(&self.before);
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn merge_applied(&mut self, newer: &dyn DocumentCommand<SceneDocument>) -> bool {
        let Some(newer) = newer.as_any().downcast_ref::<Self>() else {
            return false;
        };
        if self.entity != newer.entity || self.after != newer.before {
            return false;
        }
        self.after.clone_from(&newer.after);
        true
    }
}

struct CreateEntity {
    entity: EntityGuid,
    name: Option<String>,
    components: Vec<ComponentRecord>,
}

impl DocumentCommand<SceneDocument> for CreateEntity {
    fn label(&self) -> &str {
        "create entity"
    }

    fn apply(&mut self, document: &mut SceneDocument) -> Result<(), CommandError> {
        if document
            .entities
            .iter()
            .any(|entity| entity.guid == self.entity)
        {
            return Err(CommandError::new(format!(
                "scene entity {} already exists",
                self.entity
            )));
        }
        document.entities.push(SceneEntityRecord {
            guid: self.entity,
            name: self.name.clone(),
            components: self.components.clone(),
            extensions: Default::default(),
        });
        Ok(())
    }

    fn revert(&mut self, document: &mut SceneDocument) -> Result<(), CommandError> {
        let index = document
            .entities
            .iter()
            .position(|entity| entity.guid == self.entity)
            .ok_or_else(|| {
                CommandError::new(format!("scene entity {} was not found", self.entity))
            })?;
        document.entities.remove(index);
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn merge_applied(&mut self, _newer: &dyn DocumentCommand<SceneDocument>) -> bool {
        false
    }
}

struct DeleteEntity {
    entity: EntityGuid,
    deleted: SceneEntityRecord,
    index: usize,
}

impl DocumentCommand<SceneDocument> for DeleteEntity {
    fn label(&self) -> &str {
        "delete entity"
    }

    fn apply(&mut self, document: &mut SceneDocument) -> Result<(), CommandError> {
        let index = document
            .entities
            .iter()
            .position(|record| record.guid == self.entity)
            .ok_or_else(|| {
                CommandError::new(format!("scene entity {} was not found", self.entity))
            })?;
        if index != self.index {
            return Err(CommandError::new(format!(
                "scene entity {} changed position before command apply",
                self.entity
            )));
        }
        document.entities.remove(index);
        Ok(())
    }

    fn revert(&mut self, document: &mut SceneDocument) -> Result<(), CommandError> {
        if document
            .entities
            .iter()
            .any(|record| record.guid == self.entity)
        {
            return Err(CommandError::new(format!(
                "scene entity {} already exists before command revert",
                self.entity
            )));
        }
        if self.index > document.entities.len() {
            return Err(CommandError::new(format!(
                "scene entity {} cannot be restored at index {}",
                self.entity, self.index
            )));
        }
        document.entities.insert(self.index, self.deleted.clone());
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn merge_applied(&mut self, _newer: &dyn DocumentCommand<SceneDocument>) -> bool {
        false
    }
}

struct AddComponent {
    entity: EntityGuid,
    component: ComponentRecord,
}

impl DocumentCommand<SceneDocument> for AddComponent {
    fn label(&self) -> &str {
        "add component"
    }

    fn apply(&mut self, document: &mut SceneDocument) -> Result<(), CommandError> {
        let entity = find_entity_mut(document, self.entity)?;
        if entity
            .components
            .iter()
            .any(|component| component.schema() == self.component.schema())
        {
            return Err(CommandError::new(format!(
                "component {} already exists on scene entity {}",
                self.component.schema(),
                self.entity
            )));
        }
        entity.components.push(self.component.clone());
        Ok(())
    }

    fn revert(&mut self, document: &mut SceneDocument) -> Result<(), CommandError> {
        let entity = find_entity_mut(document, self.entity)?;
        let index = entity
            .components
            .iter()
            .position(|component| component.schema() == self.component.schema())
            .ok_or_else(|| {
                CommandError::new(format!(
                    "component {} was not found on scene entity {}",
                    self.component.schema(),
                    self.entity
                ))
            })?;
        entity.components.remove(index);
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn merge_applied(&mut self, _newer: &dyn DocumentCommand<SceneDocument>) -> bool {
        false
    }
}

struct RemoveComponent {
    entity: EntityGuid,
    schema: ComponentSchemaId,
    removed: ComponentRecord,
    index: usize,
}

impl DocumentCommand<SceneDocument> for RemoveComponent {
    fn label(&self) -> &str {
        "remove component"
    }

    fn apply(&mut self, document: &mut SceneDocument) -> Result<(), CommandError> {
        let entity = find_entity_mut(document, self.entity)?;
        let index = entity
            .components
            .iter()
            .position(|component| component.schema() == &self.schema)
            .ok_or_else(|| {
                CommandError::new(format!(
                    "component {} was not found on scene entity {}",
                    self.schema, self.entity
                ))
            })?;
        if index != self.index {
            return Err(CommandError::new(format!(
                "component {} changed position before command apply",
                self.schema
            )));
        }
        entity.components.remove(index);
        Ok(())
    }

    fn revert(&mut self, document: &mut SceneDocument) -> Result<(), CommandError> {
        let entity = find_entity_mut(document, self.entity)?;
        if entity
            .components
            .iter()
            .any(|component| component.schema() == &self.schema)
        {
            return Err(CommandError::new(format!(
                "component {} already exists on scene entity {} before command revert",
                self.schema, self.entity
            )));
        }
        if self.index > entity.components.len() {
            return Err(CommandError::new(format!(
                "component {} cannot be restored at index {} on scene entity {}",
                self.schema, self.index, self.entity
            )));
        }
        entity.components.insert(self.index, self.removed.clone());
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn merge_applied(&mut self, _newer: &dyn DocumentCommand<SceneDocument>) -> bool {
        false
    }
}

struct SetComponentField {
    entity: EntityGuid,
    schema: ComponentSchemaId,
    field: String,
    before: Option<Value>,
    after: Value,
}

impl DocumentCommand<SceneDocument> for SetComponentField {
    #[allow(clippy::unnecessary_literal_bound)]
    fn label(&self) -> &str {
        "set component JSON field"
    }

    fn apply(&mut self, document: &mut SceneDocument) -> Result<(), CommandError> {
        replace_component_field(
            document,
            self.entity,
            &self.schema,
            &self.field,
            self.before.as_ref(),
            Some(&self.after),
        )
    }

    fn revert(&mut self, document: &mut SceneDocument) -> Result<(), CommandError> {
        replace_component_field(
            document,
            self.entity,
            &self.schema,
            &self.field,
            Some(&self.after),
            self.before.as_ref(),
        )
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn merge_applied(&mut self, newer: &dyn DocumentCommand<SceneDocument>) -> bool {
        let Some(newer) = newer.as_any().downcast_ref::<Self>() else {
            return false;
        };
        if self.entity != newer.entity
            || self.schema != newer.schema
            || self.field != newer.field
            || Some(&self.after) != newer.before.as_ref()
        {
            return false;
        }
        self.after.clone_from(&newer.after);
        true
    }
}

fn replace_component_field(
    document: &mut SceneDocument,
    entity: EntityGuid,
    schema: &ComponentSchemaId,
    field: &str,
    expected: Option<&Value>,
    replacement: Option<&Value>,
) -> Result<(), CommandError> {
    let component = find_component_mut(document, entity, schema)?;
    let version = component.version();
    let mut payload = component.payload().clone();
    let current = read_json_field(&payload, field)
        .map_err(|error| CommandError::new(format!("component {schema}: {error}")))?;
    if current != expected {
        return Err(CommandError::new(format!(
            "component {schema} field {field:?} changed before command replay"
        )));
    }
    write_json_field(&mut payload, field, replacement)
        .map_err(|error| CommandError::new(format!("component {schema}: {error}")))?;
    component.replace_payload(version, payload);
    Ok(())
}

fn read_json_field<'value>(
    value: &'value Value,
    path: &str,
) -> Result<Option<&'value Value>, String> {
    let segments: Vec<_> = path.split('.').collect();
    let (last, parents) = segments
        .split_last()
        .ok_or_else(|| "field path is empty".to_owned())?;
    let mut current = value;
    for segment in parents {
        current = descend_json(current, segment)?
            .ok_or_else(|| format!("field path segment {segment:?} does not exist"))?;
    }
    match current {
        Value::Object(object) => Ok(object.get(*last)),
        Value::Array(array) => {
            let index = json_array_index(last)
                .ok_or_else(|| format!("array field segment {last:?} is not an index or axis"))?;
            array
                .get(index)
                .map(Some)
                .ok_or_else(|| format!("array index {index} is outside the payload"))
        }
        _ => Err(format!(
            "field path parent for {last:?} is neither an object nor an array"
        )),
    }
}

fn descend_json<'value>(
    value: &'value Value,
    segment: &str,
) -> Result<Option<&'value Value>, String> {
    match value {
        Value::Object(object) => Ok(object.get(segment)),
        Value::Array(array) => {
            let index = json_array_index(segment).ok_or_else(|| {
                format!("array field segment {segment:?} is not an index or axis")
            })?;
            Ok(array.get(index))
        }
        _ => Err(format!(
            "field path segment {segment:?} traverses a scalar value"
        )),
    }
}

fn write_json_field(
    value: &mut Value,
    path: &str,
    replacement: Option<&Value>,
) -> Result<(), String> {
    let segments: Vec<_> = path.split('.').collect();
    let (last, parents) = segments
        .split_last()
        .ok_or_else(|| "field path is empty".to_owned())?;
    let mut current = value;
    for segment in parents {
        current = match current {
            Value::Object(object) => object
                .get_mut(*segment)
                .ok_or_else(|| format!("field path segment {segment:?} does not exist"))?,
            Value::Array(array) => {
                let index = json_array_index(segment).ok_or_else(|| {
                    format!("array field segment {segment:?} is not an index or axis")
                })?;
                array
                    .get_mut(index)
                    .ok_or_else(|| format!("array index {index} is outside the payload"))?
            }
            _ => {
                return Err(format!(
                    "field path segment {segment:?} traverses a scalar value"
                ));
            }
        };
    }
    match current {
        Value::Object(object) => match replacement {
            Some(value) => {
                object.insert((*last).to_owned(), value.clone());
            }
            None => {
                object.remove(*last);
            }
        },
        Value::Array(array) => {
            let index = json_array_index(last)
                .ok_or_else(|| format!("array field segment {last:?} is not an index or axis"))?;
            let Some(slot) = array.get_mut(index) else {
                return Err(format!("array index {index} is outside the payload"));
            };
            let Some(replacement) = replacement else {
                return Err("array elements cannot be removed by a field edit".to_owned());
            };
            replacement.clone_into(slot);
        }
        _ => {
            return Err(format!(
                "field path parent for {last:?} is neither an object nor an array"
            ));
        }
    }
    Ok(())
}

fn json_array_index(segment: &str) -> Option<usize> {
    match segment {
        "x" | "r" => Some(0),
        "y" | "g" => Some(1),
        "z" | "b" => Some(2),
        "w" | "a" => Some(3),
        _ => segment.parse().ok(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;

    use super::*;

    fn temporary_root() -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("yuyib-editor-scene-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).expect("temporary project");
        root
    }

    #[test]
    fn rename_undo_and_save_preserve_unknown_scene_envelopes() {
        let root = temporary_root();
        let entity = EntityGuid::new();
        let scene_guid = SceneGuid::new();
        let source = format!(
            r#"{{
                "format": "yuyib.scene",
                "format_version": 1,
                "scene_guid": "{scene_guid}",
                "future_scene": {{"preserve": true}},
                "entities": [{{
                    "guid": "{entity}",
                    "name": "Before",
                    "future_entity": 9,
                    "components": [{{
                        "schema": "third-party.component",
                        "version": 4,
                        "future_component": "keep",
                        "payload": {{"value": 7}}
                    }}]
                }}]
            }}"#
        );
        fs::write(root.join("main.yscene"), source).expect("scene fixture");
        let store = ProjectDocumentStore::new(&root, 64 * 1024).expect("store");
        let mut session = SceneSession::open(&store, "main.yscene".to_owned()).expect("open scene");

        let revision = session
            .apply(SceneCommandRequest {
                base_revision: 0,
                transaction_id: "rename-1".to_owned(),
                command: SceneEditRequest::RenameEntity {
                    entity_guid: entity.to_string(),
                    name: Some("After".to_owned()),
                },
            })
            .expect("rename");
        assert_eq!(session.document.entities[0].name.as_deref(), Some("After"));
        session
            .apply(SceneCommandRequest {
                base_revision: revision.get(),
                transaction_id: "undo-1".to_owned(),
                command: SceneEditRequest::Undo,
            })
            .expect("undo");
        assert_eq!(session.document.entities[0].name.as_deref(), Some("Before"));

        session.save(&store).expect("save");
        let saved = store
            .load_json::<serde_json::Value>("main.yscene")
            .expect("reload raw")
            .value;
        assert_eq!(saved["future_scene"], json!({"preserve": true}));
        assert_eq!(saved["entities"][0]["future_entity"], 9);
        assert_eq!(
            saved["entities"][0]["components"][0]["future_component"],
            "keep"
        );
        fs::remove_dir_all(root).expect("remove temporary project");
    }

    #[test]
    fn newer_scene_container_is_read_only_but_loadable() {
        let root = temporary_root();
        let entity = EntityGuid::new();
        let source = format!(
            r#"{{
                "format": "yuyib.scene",
                "format_version": 2,
                "scene_guid": "{}",
                "entities": [{{"guid": "{entity}", "name": "Future", "components": []}}]
            }}"#,
            SceneGuid::new()
        );
        fs::write(root.join("future.yscene"), source).expect("future fixture");
        let store = ProjectDocumentStore::new(&root, 64 * 1024).expect("store");
        let mut session =
            SceneSession::open(&store, "future.yscene".to_owned()).expect("open future");
        assert!(session.is_read_only());
        assert!(matches!(
            session.apply(SceneCommandRequest {
                base_revision: 0,
                transaction_id: "future-edit".to_owned(),
                command: SceneEditRequest::RenameEntity {
                    entity_guid: entity.to_string(),
                    name: Some("Changed".to_owned()),
                },
            }),
            Err(SceneMutationError::Invalid(_))
        ));
        fs::remove_dir_all(root).expect("remove temporary project");
    }

    #[test]
    fn transform_field_edit_round_trips_through_commands() {
        let root = temporary_root();
        let entity = EntityGuid::new();
        let scene_guid = SceneGuid::new();
        let source = format!(
            r#"{{
                "format": "yuyib.scene",
                "format_version": 1,
                "scene_guid": "{scene_guid}",
                "entities": [{{
                    "guid": "{entity}",
                    "name": "Cube",
                    "components": [{{
                        "schema": "yuyib.transform3d",
                        "version": 1,
                        "payload": {{
                            "translation": [0.0, 0.0, 0.0],
                            "rotation": [0.0, 0.0, 0.0, 1.0],
                            "scale": [1.0, 1.0, 1.0]
                        }}
                    }}]
                }}]
            }}"#
        );
        fs::write(root.join("main.yscene"), source).expect("scene fixture");
        let store = ProjectDocumentStore::new(&root, 64 * 1024).expect("store");
        let mut session = SceneSession::open(&store, "main.yscene".to_owned()).expect("open scene");
        let revision = session
            .apply(SceneCommandRequest {
                base_revision: session.history_revision().get(),
                transaction_id: "tx-transform-1".to_owned(),
                command: SceneEditRequest::SetComponentField {
                    entity_guid: entity.to_string(),
                    component_id: "yuyib.transform3d".to_owned(),
                    field_path: "translation.y".to_owned(),
                    value: json!(2.5),
                },
            })
            .expect("set translation.y");
        assert_eq!(revision.get(), 1);
        let payload = session
            .document()
            .entities
            .iter()
            .find(|record| record.guid == entity)
            .expect("entity")
            .components[0]
            .payload();
        assert_eq!(payload["translation"][1], json!(2.5));
        session
            .apply(SceneCommandRequest {
                base_revision: revision.get(),
                transaction_id: "tx-transform-undo".to_owned(),
                command: SceneEditRequest::Undo,
            })
            .expect("undo");
        let restored = session
            .document()
            .entities
            .iter()
            .find(|record| record.guid == entity)
            .expect("entity")
            .components[0]
            .payload();
        assert_eq!(restored["translation"][1], json!(0.0));
        fs::remove_dir_all(root).expect("remove temporary project");
    }

    #[test]
    fn create_entity_with_transform_and_add_component() {
        let root = temporary_root();
        let scene_guid = SceneGuid::new();
        let source = format!(
            r#"{{
                "format": "yuyib.scene",
                "format_version": 1,
                "scene_guid": "{scene_guid}",
                "entities": []
            }}"#
        );
        fs::write(root.join("main.yscene"), source).expect("scene fixture");
        let store = ProjectDocumentStore::new(&root, 64 * 1024).expect("store");
        let mut session = SceneSession::open(&store, "main.yscene".to_owned()).expect("open scene");

        let revision = session
            .apply(SceneCommandRequest {
                base_revision: 0,
                transaction_id: "create-1".to_owned(),
                command: SceneEditRequest::CreateEntity {
                    name: Some("Prop".to_owned()),
                    with_transform3d: true,
                },
            })
            .expect("create entity");
        assert_eq!(session.document.entities.len(), 1);
        assert_eq!(session.document.entities[0].name.as_deref(), Some("Prop"));
        assert_eq!(session.document.entities[0].components.len(), 1);
        assert_eq!(
            session.document.entities[0].components[0].schema().as_str(),
            "yuyib.transform3d"
        );

        let entity = session.document.entities[0].guid;
        session
            .apply(SceneCommandRequest {
                base_revision: revision.get(),
                transaction_id: "add-1".to_owned(),
                command: SceneEditRequest::AddComponent {
                    entity_guid: entity.to_string(),
                    component_id: "yuyib.model3d".to_owned(),
                },
            })
            .expect("add model3d");
        assert_eq!(session.document.entities[0].components.len(), 2);
        fs::remove_dir_all(root).expect("remove temporary project");
    }

    #[test]
    fn delete_entity_undo_restores_the_complete_record_and_order() {
        let root = temporary_root();
        let first = EntityGuid::new();
        let second = EntityGuid::new();
        let source = format!(
            r#"{{
                "format": "yuyib.scene",
                "format_version": 1,
                "scene_guid": "{}",
                "entities": [
                    {{"guid": "{first}", "name": "First", "components": []}},
                    {{
                        "guid": "{second}",
                        "name": "Delete me",
                        "future_entity": {{"preserve": true}},
                        "components": [{{
                            "schema": "third-party.component",
                            "version": 1,
                            "payload": {{"value": 7}}
                        }}]
                    }}
                ]
            }}"#,
            SceneGuid::new()
        );
        fs::write(root.join("main.yscene"), source).expect("scene fixture");
        let store = ProjectDocumentStore::new(&root, 64 * 1024).expect("store");
        let mut session = SceneSession::open(&store, "main.yscene".to_owned()).expect("open scene");

        let revision = session
            .apply(SceneCommandRequest {
                base_revision: 0,
                transaction_id: "delete-1".to_owned(),
                command: SceneEditRequest::DeleteEntity {
                    entity_guid: second.to_string(),
                },
            })
            .expect("delete entity");
        assert_eq!(session.document.entities.len(), 1);
        assert_eq!(session.document.entities[0].guid, first);

        session
            .apply(SceneCommandRequest {
                base_revision: revision.get(),
                transaction_id: "delete-undo-1".to_owned(),
                command: SceneEditRequest::Undo,
            })
            .expect("undo delete");
        assert_eq!(session.document.entities.len(), 2);
        assert_eq!(session.document.entities[1].guid, second);
        assert_eq!(
            session.document.entities[1].name.as_deref(),
            Some("Delete me")
        );
        assert_eq!(
            session.document.entities[1].extensions["future_entity"],
            json!({"preserve": true})
        );
        assert_eq!(
            session.document.entities[1].components[0].payload()["value"],
            json!(7)
        );
        fs::remove_dir_all(root).expect("remove temporary project");
    }

    #[test]
    fn remove_component_undo_restores_the_complete_record_and_order() {
        let root = temporary_root();
        let entity = EntityGuid::new();
        let source = format!(
            r#"{{
                "format": "yuyib.scene",
                "format_version": 1,
                "scene_guid": "{}",
                "entities": [{{
                    "guid": "{entity}",
                    "name": "Prop",
                    "components": [
                        {{
                            "schema": "yuyib.transform3d",
                            "version": 1,
                            "payload": {{
                                "translation": [0.0, 0.0, 0.0],
                                "rotation": [0.0, 0.0, 0.0, 1.0],
                                "scale": [1.0, 1.0, 1.0]
                            }}
                        }},
                        {{
                            "schema": "third-party.component",
                            "version": 4,
                            "future_component": "keep",
                            "payload": {{"value": 7}}
                        }}
                    ]
                }}]
            }}"#,
            SceneGuid::new()
        );
        fs::write(root.join("main.yscene"), source).expect("scene fixture");
        let store = ProjectDocumentStore::new(&root, 64 * 1024).expect("store");
        let mut session = SceneSession::open(&store, "main.yscene".to_owned()).expect("open scene");

        let revision = session
            .apply(SceneCommandRequest {
                base_revision: 0,
                transaction_id: "remove-1".to_owned(),
                command: SceneEditRequest::RemoveComponent {
                    entity_guid: entity.to_string(),
                    component_id: "third-party.component".to_owned(),
                },
            })
            .expect("remove component");
        assert_eq!(session.document.entities[0].components.len(), 1);
        assert_eq!(
            session.document.entities[0].components[0].schema().as_str(),
            "yuyib.transform3d"
        );

        session
            .apply(SceneCommandRequest {
                base_revision: revision.get(),
                transaction_id: "remove-undo-1".to_owned(),
                command: SceneEditRequest::Undo,
            })
            .expect("undo remove");
        assert_eq!(session.document.entities[0].components.len(), 2);
        assert_eq!(
            session.document.entities[0].components[1].schema().as_str(),
            "third-party.component"
        );
        assert_eq!(
            session.document.entities[0].components[1].payload()["value"],
            json!(7)
        );
        session.save(&store).expect("save");
        let saved = store
            .load_json::<serde_json::Value>("main.yscene")
            .expect("reload raw")
            .value;
        assert_eq!(
            saved["entities"][0]["components"][1]["future_component"],
            "keep"
        );
        fs::remove_dir_all(root).expect("remove temporary project");
    }
}
