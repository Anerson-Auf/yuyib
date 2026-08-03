//! Play-time [`SceneInteractionBridge`] over a materialized GUID → Entity map.
//!
//! Wires existing engine types (`Transform3d` / `Model3d` / `DirectionalLight3d`,
//! `QuestBook`) without inventing physics-mode switching or quest UI.

use std::collections::BTreeMap;

use yuyib_authoring::EntityGuid;
use yuyib_ecs::prelude::{Entity, World};
use yuyib_game_3d::{DirectionalLight3d, LocalTransform3d, Model3d, Transform3d, WorldTransform3d};
use yuyib_gameplay::{QuestBook, QuestSignal, QuestTransition};
use yuyib_scene_interaction::{
    BridgeCapabilities, SCHEMA_DIRECTIONAL_LIGHT_3D, SCHEMA_LOCAL_TRANSFORM_3D, SCHEMA_MODEL_3D,
    SCHEMA_TRANSFORM_3D, SceneInteractionBatchResult, SceneInteractionBridge,
    SceneInteractionIntent, SceneInteractionSignal, TransformSpace, play_capabilities,
    translation_schemas, try_parse_quest_progress_signal, try_parse_trigger_signal, validate_intent,
};

use yuyib_play::play_log::play_log;

/// Runtime bridge: intents mutate ECS; signals are returned in the batch result.
pub struct PlayWorldBridge<'world> {
    world: &'world mut World,
    entities: &'world BTreeMap<EntityGuid, Entity>,
}

impl<'world> PlayWorldBridge<'world> {
    /// Borrows the Play world and GUID map from materialization.
    pub fn new(world: &'world mut World, entities: &'world BTreeMap<EntityGuid, Entity>) -> Self {
        Self { world, entities }
    }

    fn resolve(&self, guid: EntityGuid) -> Result<Entity, String> {
        self.entities.get(&guid).copied().ok_or_else(|| {
            format!("play entity {guid} is not in the materialized GUID map")
        })
    }

    fn set_translation(
        &mut self,
        entity: EntityGuid,
        translation: [f32; 3],
        space: TransformSpace,
    ) -> Result<bool, String> {
        let runtime = self.resolve(entity)?;
        let mut matched = false;
        let mut wrote = false;
        for schema in translation_schemas(space) {
            match *schema {
                SCHEMA_LOCAL_TRANSFORM_3D => {
                    if let Some(mut local) = self.world.get_mut::<LocalTransform3d>(runtime) {
                        matched = true;
                        if local.translation != translation {
                            local.translation = translation;
                            wrote = true;
                        }
                        self.world.entity_mut(runtime).remove::<WorldTransform3d>();
                        if matches!(space, TransformSpace::Local) {
                            return Ok(wrote);
                        }
                    }
                }
                SCHEMA_TRANSFORM_3D => {
                    if let Some(mut transform) = self.world.get_mut::<Transform3d>(runtime) {
                        matched = true;
                        if transform.translation != translation {
                            transform.translation = translation;
                            wrote = true;
                        }
                        self.world.entity_mut(runtime).remove::<WorldTransform3d>();
                        if matches!(space, TransformSpace::Local) {
                            return Ok(wrote);
                        }
                    }
                }
                _ => {}
            }
        }
        if matched {
            Ok(wrote)
        } else {
            Err(format!(
                "play entity {entity} has no Transform3d / LocalTransform3d"
            ))
        }
    }

    fn set_model_field(
        &mut self,
        entity: Entity,
        field_path: &str,
        value: &serde_json::Value,
    ) -> Result<bool, String> {
        let Some(mut model) = self.world.get_mut::<Model3d>(entity) else {
            return Err("play entity has no Model3d component".to_owned());
        };
        match field_path {
            "visible" => {
                let visible = value
                    .as_bool()
                    .ok_or_else(|| "model3d.visible requires a JSON bool".to_owned())?;
                if model.visible == visible {
                    return Ok(false);
                }
                model.visible = visible;
                Ok(true)
            }
            "render_order" => {
                let order = value.as_i64().ok_or_else(|| {
                    "model3d.render_order requires a JSON integer".to_owned()
                })?;
                let order = i32::try_from(order)
                    .map_err(|_| "model3d.render_order out of i32 range".to_owned())?;
                if model.render_order == order {
                    return Ok(false);
                }
                model.render_order = order;
                Ok(true)
            }
            "mesh" => {
                if value.is_null() {
                    if model.mesh.is_none() {
                        return Ok(false);
                    }
                    model.mesh = None;
                    return Ok(true);
                }
                let mesh = value
                    .as_u64()
                    .ok_or_else(|| "model3d.mesh requires null or a JSON integer".to_owned())?;
                let mesh = usize::try_from(mesh)
                    .map_err(|_| "model3d.mesh out of usize range".to_owned())?;
                if model.mesh == Some(mesh) {
                    return Ok(false);
                }
                model.mesh = Some(mesh);
                Ok(true)
            }
            other => Err(format!(
                "play SetComponentField for yuyib.model3d does not support `{other}` yet (visible/render_order/mesh)"
            )),
        }
    }

    fn set_light_field(
        &mut self,
        entity: Entity,
        field_path: &str,
        value: &serde_json::Value,
    ) -> Result<bool, String> {
        let Some(light) = self.world.get::<DirectionalLight3d>(entity).copied() else {
            return Err("play entity has no DirectionalLight3d component".to_owned());
        };
        let updated = match field_path {
            "enabled" => {
                let enabled = value
                    .as_bool()
                    .ok_or_else(|| "directional-light3d.enabled requires a JSON bool".to_owned())?;
                if light.is_enabled() == enabled {
                    return Ok(false);
                }
                light.with_enabled(enabled)
            }
            "illuminance_lux" => {
                let lux = value.as_f64().ok_or_else(|| {
                    "directional-light3d.illuminance_lux requires a JSON number".to_owned()
                })? as f32;
                if (light.illuminance_lux() - lux).abs() <= f32::EPSILON {
                    return Ok(false);
                }
                light
                    .with_illuminance_lux(lux)
                    .map_err(|error| format!("directional-light3d.illuminance_lux: {error:?}"))?
            }
            "direction.x" | "direction.y" | "direction.z" => {
                let mut direction = light.direction();
                let number = value.as_f64().ok_or_else(|| {
                    format!("{field_path} requires a JSON number")
                })? as f32;
                match field_path {
                    "direction.x" => direction[0] = number,
                    "direction.y" => direction[1] = number,
                    "direction.z" => direction[2] = number,
                    _ => unreachable!(),
                }
                light
                    .with_direction(direction)
                    .map_err(|error| format!("directional-light3d.direction: {error:?}"))?
            }
            "color.x" | "color.y" | "color.z" | "color.r" | "color.g" | "color.b" => {
                let mut color = light.color();
                let number = value.as_f64().ok_or_else(|| {
                    format!("{field_path} requires a JSON number")
                })? as f32;
                match field_path {
                    "color.x" | "color.r" => color[0] = number,
                    "color.y" | "color.g" => color[1] = number,
                    "color.z" | "color.b" => color[2] = number,
                    _ => unreachable!(),
                }
                light
                    .with_color(color)
                    .map_err(|error| format!("directional-light3d.color: {error:?}"))?
            }
            other => {
                return Err(format!(
                    "play SetComponentField for yuyib.directional-light3d does not support `{other}` yet"
                ));
            }
        };
        self.world.entity_mut(entity).insert(updated);
        Ok(true)
    }

    fn add_component(
        &mut self,
        entity: EntityGuid,
        schema: &str,
        payload: Option<&serde_json::Value>,
    ) -> Result<bool, String> {
        let runtime = self.resolve(entity)?;
        match schema {
            SCHEMA_TRANSFORM_3D => {
                if self.world.get::<Transform3d>(runtime).is_some() {
                    return Err(format!(
                        "play entity {entity} already has Transform3d"
                    ));
                }
                let transform = transform3d_from_payload(payload)?;
                self.world.entity_mut(runtime).insert(transform);
                self.world.entity_mut(runtime).remove::<WorldTransform3d>();
                Ok(true)
            }
            SCHEMA_LOCAL_TRANSFORM_3D => {
                if self.world.get::<LocalTransform3d>(runtime).is_some() {
                    return Err(format!(
                        "play entity {entity} already has LocalTransform3d"
                    ));
                }
                let local = local_transform3d_from_payload(payload)?;
                self.world.entity_mut(runtime).insert(local);
                self.world.entity_mut(runtime).remove::<WorldTransform3d>();
                Ok(true)
            }
            SCHEMA_DIRECTIONAL_LIGHT_3D => {
                if self.world.get::<DirectionalLight3d>(runtime).is_some() {
                    return Err(format!(
                        "play entity {entity} already has DirectionalLight3d"
                    ));
                }
                let Some(light) = directional_light_from_payload(payload)? else {
                    return Ok(false);
                };
                self.world.entity_mut(runtime).insert(light);
                Ok(true)
            }
            _ => Err(format!(
                "play AddComponent does not support `{schema}` yet"
            )),
        }
    }
}

impl SceneInteractionBridge for PlayWorldBridge<'_> {
    type Error = String;

    fn capabilities(&self) -> BridgeCapabilities {
        play_capabilities()
    }

    fn apply_intents(
        &mut self,
        intents: &[SceneInteractionIntent],
    ) -> Result<SceneInteractionBatchResult, Self::Error> {
        let caps = self.capabilities();
        let mut result = SceneInteractionBatchResult::empty(intents.len());
        for intent in intents {
            validate_intent(intent)?;
            if !caps.supports(intent) {
                return Err(caps.unsupported_message(intent));
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
                    if self.set_translation(*entity, *translation, *space)? {
                        result.applied += 1;
                    }
                }
                SceneInteractionIntent::SetComponentField {
                    entity,
                    schema,
                    field_path,
                    value,
                } => {
                    let runtime = self.resolve(*entity)?;
                    let wrote = match schema.as_str() {
                        SCHEMA_TRANSFORM_3D | SCHEMA_LOCAL_TRANSFORM_3D => {
                            let Some(axis) = field_path.strip_prefix("translation.") else {
                                return Err(format!(
                                    "play transform SetComponentField only supports translation.{{x,y,z}}, got `{field_path}`"
                                ));
                            };
                            let number = value.as_f64().ok_or_else(|| {
                                format!("translation field `{field_path}` requires a JSON number")
                            })? as f32;
                            let mut translation = [0.0_f32; 3];
                            let prefer_local = schema == SCHEMA_LOCAL_TRANSFORM_3D
                                || self.world.get::<LocalTransform3d>(runtime).is_some();
                            if prefer_local {
                                if let Some(local) = self.world.get::<LocalTransform3d>(runtime) {
                                    translation = local.translation;
                                }
                            } else if let Some(transform) = self.world.get::<Transform3d>(runtime)
                            {
                                translation = transform.translation;
                            }
                            match axis {
                                "x" => translation[0] = number,
                                "y" => translation[1] = number,
                                "z" => translation[2] = number,
                                _ => {
                                    return Err(format!("unsupported translation axis `{axis}`"));
                                }
                            }
                            let space = if schema == SCHEMA_LOCAL_TRANSFORM_3D {
                                TransformSpace::Local
                            } else {
                                TransformSpace::World
                            };
                            self.set_translation(*entity, translation, space)?
                        }
                        SCHEMA_MODEL_3D => self.set_model_field(runtime, field_path, value)?,
                        SCHEMA_DIRECTIONAL_LIGHT_3D => {
                            self.set_light_field(runtime, field_path, value)?
                        }
                        _ => return Err(caps.unsupported_message(intent)),
                    };
                    if wrote {
                        result.applied += 1;
                    }
                }
                SceneInteractionIntent::AddComponent {
                    entity,
                    schema,
                    payload,
                    ..
                } => {
                    if self.add_component(*entity, schema, payload.as_ref())? {
                        result.applied += 1;
                    }
                }
            }
        }
        Ok(result)
    }
}

/// Applies drained interaction signals onto an existing [`QuestBook`].
///
/// Non-quest-shaped signals are skipped here; hosts log them via
/// [`PlayInteractionHost::consume_signals`].
pub fn apply_quest_signals(
    book: &mut QuestBook,
    signals: &[SceneInteractionSignal],
) -> Vec<QuestTransition> {
    let mut transitions = Vec::new();
    for signal in signals {
        if let Some(parsed) = try_parse_quest_progress_signal(&signal.name, &signal.payload) {
            match QuestSignal::new(parsed.event.as_str(), parsed.amount) {
                Ok(quest_signal) => transitions.extend(book.apply_signal(&quest_signal)),
                Err(error) => play_log(format!(
                    "yuyib-play: skip quest signal `{}`: {error}",
                    signal.name
                )),
            }
            continue;
        }
        // Interactable / custom EmitSignal often uses the event id as the name
        // (`world.talk_npc`) with `{ "amount": 1 }` — map that onto QuestBook too.
        if let Some(amount) = signal.payload.get("amount").and_then(|value| value.as_u64()) {
            let amount = u32::try_from(amount).unwrap_or(0);
            if amount == 0 {
                continue;
            }
            match QuestSignal::new(signal.name.as_str(), amount) {
                Ok(quest_signal) => transitions.extend(book.apply_signal(&quest_signal)),
                Err(_) => {}
            }
        }
    }
    transitions
}

fn transform3d_from_payload(payload: Option<&serde_json::Value>) -> Result<Transform3d, String> {
    let Some(payload) = payload else {
        return Ok(Transform3d::IDENTITY);
    };
    Ok(Transform3d {
        translation: vec3_or(payload, "translation", [0.0, 0.0, 0.0])?,
        rotation: quat_or(payload, "rotation", [0.0, 0.0, 0.0, 1.0])?,
        scale: vec3_or(payload, "scale", [1.0, 1.0, 1.0])?,
    })
}

fn local_transform3d_from_payload(
    payload: Option<&serde_json::Value>,
) -> Result<LocalTransform3d, String> {
    let Some(payload) = payload else {
        return Ok(LocalTransform3d::IDENTITY);
    };
    Ok(LocalTransform3d {
        translation: vec3_or(payload, "translation", [0.0, 0.0, 0.0])?,
        rotation: quat_or(payload, "rotation", [0.0, 0.0, 0.0, 1.0])?,
        scale: vec3_or(payload, "scale", [1.0, 1.0, 1.0])?,
    })
}

fn directional_light_from_payload(
    payload: Option<&serde_json::Value>,
) -> Result<Option<DirectionalLight3d>, String> {
    let payload = payload.unwrap_or(&serde_json::Value::Null);
    if payload
        .get("enabled")
        .and_then(serde_json::Value::as_bool)
        .is_some_and(|enabled| !enabled)
    {
        return Ok(None);
    }
    let direction = vec3_or(payload, "direction", [0.0, -1.0, 0.0])?;
    let color = vec3_or(payload, "color", [1.0, 0.95, 0.9])?;
    let illuminance = payload
        .get("illuminance_lux")
        .or_else(|| payload.get("illuminance"))
        .and_then(|value| match value {
            serde_json::Value::Number(number) => number.as_f64().map(|v| v as f32),
            serde_json::Value::String(text) => text.trim().parse().ok(),
            _ => None,
        })
        .filter(|value| value.is_finite())
        .unwrap_or(8.0);
    DirectionalLight3d::new(direction, color, illuminance)
        .map(Some)
        .map_err(|error| error.to_string())
}

fn vec3_or(
    payload: &serde_json::Value,
    key: &str,
    default: [f32; 3],
) -> Result<[f32; 3], String> {
    let Some(value) = payload.get(key) else {
        return Ok(default);
    };
    let Some(items) = value.as_array() else {
        return Err(format!("{key} must be a JSON array of 3 numbers"));
    };
    if items.len() < 3 {
        return Err(format!("{key} must have at least 3 numbers"));
    }
    let mut out = [0.0_f32; 3];
    for (index, item) in items.iter().take(3).enumerate() {
        out[index] = item
            .as_f64()
            .ok_or_else(|| format!("{key}[{index}] must be a number"))? as f32;
        if !out[index].is_finite() {
            return Err(format!("{key}[{index}] must be finite"));
        }
    }
    Ok(out)
}

fn quat_or(
    payload: &serde_json::Value,
    key: &str,
    default: [f32; 4],
) -> Result<[f32; 4], String> {
    let Some(value) = payload.get(key) else {
        return Ok(default);
    };
    let Some(items) = value.as_array() else {
        return Err(format!("{key} must be a JSON array of 4 numbers"));
    };
    if items.len() < 4 {
        return Err(format!("{key} must have at least 4 numbers"));
    }
    let mut out = [0.0_f32; 4];
    for (index, item) in items.iter().take(4).enumerate() {
        out[index] = item
            .as_f64()
            .ok_or_else(|| format!("{key}[{index}] must be a number"))? as f32;
        if !out[index].is_finite() {
            return Err(format!("{key}[{index}] must be finite"));
        }
    }
    Ok(out)
}

/// Host-side pending queue + frame-boundary signal drain for Play.
#[derive(Default)]
pub struct PlayInteractionHost {
    /// Intents enqueued by scripts / tests; drained once per frame.
    pub pending: Vec<SceneInteractionIntent>,
    /// Optional gameplay quest book (register/start from the game / tests).
    pub quests: Option<QuestBook>,
    current_signals: Vec<SceneInteractionSignal>,
    next_signals: Vec<SceneInteractionSignal>,
}

impl PlayInteractionHost {
    /// Attaches a quest book owned by the Play host (no UI).
    pub fn set_quest_book(&mut self, book: QuestBook) {
        self.quests = Some(book);
    }

    /// Queues one intent for the next flush.
    pub fn enqueue(&mut self, intent: SceneInteractionIntent) {
        self.pending.push(intent);
    }

    /// Applies pending intents against the live World + GUID map.
    ///
    /// # Errors
    ///
    /// Returns the first adapter failure; pending is cleared before apply.
    pub fn flush(
        &mut self,
        world: &mut World,
        entities: &BTreeMap<EntityGuid, Entity>,
    ) -> Result<SceneInteractionBatchResult, String> {
        if self.pending.is_empty() {
            return Ok(SceneInteractionBatchResult::empty(0));
        }
        let intents = std::mem::take(&mut self.pending);
        let mut bridge = PlayWorldBridge::new(world, entities);
        let batch = bridge.apply_intents(&intents)?;
        self.next_signals.extend(batch.signals.iter().cloned());
        Ok(batch)
    }

    /// Makes queued signals visible (call once per rendered/simulation frame).
    pub fn advance_signals(&mut self) {
        self.current_signals.clear();
        std::mem::swap(&mut self.current_signals, &mut self.next_signals);
    }

    /// Signals visible this frame after [`Self::advance_signals`].
    #[must_use]
    #[allow(dead_code)]
    pub fn signals(&self) -> &[SceneInteractionSignal] {
        &self.current_signals
    }

    /// Drains quest-shaped signals into [`QuestBook`] when attached; logs every signal.
    pub fn consume_signals(&mut self) {
        if let Some(book) = self.quests.as_mut() {
            let transitions = apply_quest_signals(book, &self.current_signals);
            for transition in &transitions {
                play_log(format!("yuyib-play: quest transition {transition:?}"));
            }
        }
        for signal in &self.current_signals {
            if let Some(parsed) = try_parse_quest_progress_signal(&signal.name, &signal.payload) {
                if self.quests.is_none() {
                    play_log(format!(
                        "yuyib-play: signal quest event={} amount={} (no QuestBook attached)",
                        parsed.event, parsed.amount
                    ));
                } else {
                    play_log(format!(
                        "yuyib-play: signal quest event={} amount={}",
                        parsed.event, parsed.amount
                    ));
                }
                continue;
            }
            if let Some(parsed) = try_parse_trigger_signal(&signal.name, &signal.payload) {
                play_log(format!(
                    "yuyib-play: signal trigger id={} phase={}",
                    parsed.trigger_id,
                    parsed.phase.as_str()
                ));
                continue;
            }
            play_log(format!(
                "yuyib-play: signal `{}` payload={}",
                signal.name, signal.payload
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yuyib_gameplay::{ObjectiveId, QuestDefinition, QuestId, QuestObjective};
    use yuyib_model::Model;
    use yuyib_scene_interaction::SceneInteractionBridge;

    #[test]
    fn emit_signal_queues_without_entities() {
        let mut world = World::new();
        let map = BTreeMap::new();
        let mut bridge = PlayWorldBridge::new(&mut world, &map);
        let batch = bridge
            .apply_intent(SceneInteractionIntent::EmitSignal {
                name: "world.generator_activated".to_owned(),
                payload: serde_json::json!({ "amount": 1 }),
            })
            .expect("signal");
        assert_eq!(batch.signals.len(), 1);
        assert_eq!(batch.applied, 1);
    }

    #[test]
    fn host_flush_feeds_quest_book() {
        let mut world = World::new();
        let map = BTreeMap::new();
        let mut host = PlayInteractionHost::default();
        let quest_id = QuestId::new("tutorial.power_up");
        let objective_id = ObjectiveId::new("activate_generator");
        let mut book = QuestBook::default();
        book.register(
            QuestDefinition::new(
                quest_id.clone(),
                vec![QuestObjective::new(
                    objective_id.clone(),
                    "world.generator_activated",
                    1,
                )
                .expect("objective")],
            )
            .expect("definition"),
        )
        .expect("register");
        book.start(&quest_id).expect("start");
        host.set_quest_book(book);

        host.enqueue(SceneInteractionIntent::EmitSignal {
            name: "world.generator_activated".to_owned(),
            payload: serde_json::json!({ "amount": 1 }),
        });
        host.flush(&mut world, &map).expect("flush");
        host.advance_signals();
        host.consume_signals();
        let progress = host
            .quests
            .as_ref()
            .and_then(|book| book.progress(&quest_id))
            .expect("progress");
        assert_eq!(progress.objective(&objective_id), Some(1));
    }

    #[test]
    fn set_model_visible_via_bridge() {
        let mut models = yuyib_assets::Assets::<Model>::new();
        let handle = models.insert(Model::cube(0.5).expect("cube"));
        let mut world = World::new();
        let entity = world
            .spawn((
                Transform3d::default(),
                Model3d::new(handle).with_visible(true),
            ))
            .id();
        let guid = EntityGuid::new();
        let mut map = BTreeMap::new();
        map.insert(guid, entity);
        let mut bridge = PlayWorldBridge::new(&mut world, &map);
        bridge
            .apply_intent(SceneInteractionIntent::SetComponentField {
                entity: guid,
                schema: SCHEMA_MODEL_3D.to_owned(),
                field_path: "visible".to_owned(),
                value: serde_json::json!(false),
            })
            .expect("set visible");
        assert!(!world.get::<Model3d>(entity).expect("model").visible);
        let _keep = models;
    }

    #[test]
    fn add_directional_light_then_disable() {
        let mut world = World::new();
        let entity = world.spawn(Transform3d::IDENTITY).id();
        let guid = EntityGuid::new();
        let mut map = BTreeMap::new();
        map.insert(guid, entity);
        {
            let mut bridge = PlayWorldBridge::new(&mut world, &map);
            bridge
                .apply_intent(SceneInteractionIntent::AddComponent {
                    entity: guid,
                    schema: SCHEMA_DIRECTIONAL_LIGHT_3D.to_owned(),
                    version: None,
                    payload: None,
                })
                .expect("add light");
        }
        assert!(world.get::<DirectionalLight3d>(entity).is_some());
        {
            let mut bridge = PlayWorldBridge::new(&mut world, &map);
            bridge
                .apply_intent(SceneInteractionIntent::SetComponentField {
                    entity: guid,
                    schema: SCHEMA_DIRECTIONAL_LIGHT_3D.to_owned(),
                    field_path: "enabled".to_owned(),
                    value: serde_json::json!(false),
                })
                .expect("disable");
        }
        assert!(!world
            .get::<DirectionalLight3d>(entity)
            .expect("light")
            .is_enabled());
    }

    #[test]
    fn add_transform_rejects_duplicate() {
        let mut world = World::new();
        let entity = world.spawn(Transform3d::IDENTITY).id();
        let guid = EntityGuid::new();
        let mut map = BTreeMap::new();
        map.insert(guid, entity);
        let mut bridge = PlayWorldBridge::new(&mut world, &map);
        let err = bridge
            .apply_intent(SceneInteractionIntent::AddComponent {
                entity: guid,
                schema: SCHEMA_TRANSFORM_3D.to_owned(),
                version: None,
                payload: None,
            })
            .expect_err("duplicate");
        assert!(err.contains("already has Transform3d"));
    }
}
