//! Authoring `yuyib.trigger` → ECS sphere volumes → Intent Bridge signals.
//!
//! Uses existing [`overlap_spheres_3d`](yuyib_physics::overlap_spheres_3d), same
//! query stack as Interactable use — not a CharacterController↔Rapier switch.
//! Hosts that also own Rapier can still feed [`crate::trigger_signals::TriggerOverlapTracker`].

use std::collections::{HashMap, HashSet};

use serde_json::json;
use yuyib_authoring::SceneDocument;
use yuyib_ecs::prelude::{Entity, World};
use yuyib_game_3d::{LocalTransform3d, Transform3d};
use yuyib_gameplay::Trigger;
use yuyib_physics::{Position3d, Sphere, SphereCollider3d, Vec3, overlap_spheres_3d};
use yuyib_scene_interaction::{
    ParsedTriggerPhase, SIGNAL_TRIGGER_PREFIX, SceneInteractionIntent,
};

use crate::play_log::play_log;

/// Default authored trigger radius when `radius` is omitted / invalid.
const DEFAULT_TRIGGER_RADIUS: f32 = 1.0;
/// Player probe radius for volume tests (CharacterController capsule ≈).
const DEFAULT_PLAYER_RADIUS: f32 = 0.4;

/// Tracks last-frame `(trigger_entity, other)` pairs for Entered / Stayed / Exited.
#[derive(Clone, Debug, Default)]
pub struct EntityTriggerTracker {
    previous: HashSet<(Entity, Entity)>,
}

impl EntityTriggerTracker {
    /// Diffs current overlap pairs into Intent Bridge `trigger.*` signals.
    pub fn diff_to_intents(
        &mut self,
        current_pairs: &[(Entity, Entity)],
        trigger_ids: &HashMap<Entity, String>,
    ) -> Vec<SceneInteractionIntent> {
        let current: HashSet<_> = current_pairs.iter().copied().collect();
        let mut intents = Vec::new();

        for pair in &current {
            let Some(trigger_id) = trigger_ids.get(&pair.0) else {
                continue;
            };
            let phase = if self.previous.contains(pair) {
                ParsedTriggerPhase::Stayed
            } else {
                ParsedTriggerPhase::Entered
            };
            intents.push(trigger_intent(trigger_id, phase));
        }
        for pair in &self.previous {
            if current.contains(pair) {
                continue;
            }
            let Some(trigger_id) = trigger_ids.get(&pair.0) else {
                continue;
            };
            intents.push(trigger_intent(trigger_id, ParsedTriggerPhase::Exited));
        }

        self.previous = current;
        intents
    }
}

fn trigger_intent(trigger_id: &str, phase: ParsedTriggerPhase) -> SceneInteractionIntent {
    SceneInteractionIntent::EmitSignal {
        name: format!("{SIGNAL_TRIGGER_PREFIX}{trigger_id}"),
        payload: json!({
            "trigger": trigger_id,
            "phase": phase.as_str(),
        }),
    }
}

/// Attaches runtime [`Trigger`] + sphere query colliders from authored
/// `yuyib.trigger` components.
pub fn materialize_triggers(
    document: &SceneDocument,
    world: &mut World,
    entities: &std::collections::BTreeMap<yuyib_authoring::EntityGuid, Entity>,
) {
    let mut count = 0_usize;
    for (guid, &entity) in entities {
        let Some(record) = document
            .entities
            .iter()
            .find(|candidate| candidate.guid == *guid)
        else {
            continue;
        };
        let Some(component) = record
            .components
            .iter()
            .find(|component| component.schema().as_str() == "yuyib.trigger")
        else {
            continue;
        };
        let Some((trigger, radius)) = trigger_from_payload(component.payload()) else {
            play_log(format!(
                "yuyib-play: skip invalid yuyib.trigger on `{}`",
                record.name.as_deref().unwrap_or("<unnamed>")
            ));
            continue;
        };
        let translation = entity_translation(world, entity).unwrap_or([0.0, 0.0, 0.0]);
        let Ok(position) = Position3d::new(Vec3::new(translation[0], translation[1], translation[2]))
        else {
            continue;
        };
        let Ok(sphere) = SphereCollider3d::new(radius) else {
            continue;
        };
        let id = trigger.trigger.as_str().to_owned();
        // Keep the authored marker mesh, but mark it overlay so Scene/Play do not
        // frustum-flicker it with yaw. Do NOT strip Model3d — that hid ExitVolume
        // entirely and was the wrong fix for the angle bug.
        if let Some(mut model) = world.get_mut::<yuyib_game_3d::Model3d>(entity) {
            *model = model.clone().with_overlay(true);
        }
        world.entity_mut(entity).insert((trigger, position, sphere));
        count += 1;
        play_log(format!(
            "yuyib-play: materialized Trigger `{id}` on `{}` (overlay marker)",
            record.name.as_deref().unwrap_or("<unnamed>")
        ));
    }
    if count > 0 {
        play_log(format!(
            "yuyib-play: {count} trigger volume(s) ready — walk in/out (Diagnostics source=play)"
        ));
    }
}

/// Keeps trigger query [`Position3d`] in sync with transforms.
pub fn sync_trigger_positions(world: &mut World) {
    let entities: Vec<Entity> = {
        let mut query = world.query::<(Entity, &Trigger)>();
        query.iter(world).map(|(entity, _)| entity).collect()
    };
    for entity in entities {
        let Some(translation) = entity_translation(world, entity) else {
            continue;
        };
        if let Ok(position) =
            Position3d::new(Vec3::new(translation[0], translation[1], translation[2]))
        {
            world.entity_mut(entity).insert(position);
        }
    }
}

/// Collects `(trigger, player)` overlaps and diffs them into bridge intents.
pub fn step_trigger_volumes(
    world: &mut World,
    player: Entity,
    player_center: Vec3,
    tracker: &mut EntityTriggerTracker,
) -> Vec<SceneInteractionIntent> {
    let Ok(probe) = Sphere::new(DEFAULT_PLAYER_RADIUS) else {
        return Vec::new();
    };
    let Ok(overlaps) = overlap_spheres_3d(world, player_center, probe, Some(player)) else {
        return Vec::new();
    };

    // Full id map is required for Exited (pair gone → still need the trigger id).
    let mut trigger_ids = HashMap::new();
    {
        let mut query = world.query::<(Entity, &Trigger)>();
        for (entity, trigger) in query.iter(world) {
            if trigger.enabled {
                trigger_ids.insert(entity, trigger.trigger.as_str().to_owned());
            }
        }
    }

    let mut pairs = Vec::new();
    for overlap in overlaps {
        if !trigger_ids.contains_key(&overlap.entity) {
            continue;
        }
        pairs.push((overlap.entity, player));
    }
    tracker.diff_to_intents(&pairs, &trigger_ids)
}

fn trigger_from_payload(payload: &serde_json::Value) -> Option<(Trigger, f32)> {
    let id = payload.get("trigger")?.as_str()?.trim();
    if id.is_empty() {
        return None;
    }
    let mut trigger = Trigger::new(id);
    if let Some(enabled) = payload.get("enabled").and_then(|value| value.as_bool()) {
        trigger.enabled = enabled;
    }
    let radius = payload
        .get("radius")
        .and_then(|value| value.as_f64())
        .map(|value| value as f32)
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(DEFAULT_TRIGGER_RADIUS);
    Some((trigger, radius))
}

fn entity_translation(world: &World, entity: Entity) -> Option<[f32; 3]> {
    if let Some(local) = world.get::<LocalTransform3d>(entity) {
        return Some(local.translation);
    }
    world.get::<Transform3d>(entity).map(|value| value.translation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use yuyib_assets::Assets;
    use yuyib_game_3d::{Model3d, Transform3d, extract_models};
    use yuyib_model::Model;
    use yuyib_scene_interaction::try_parse_trigger_signal;

    #[test]
    fn materialize_marks_trigger_cube_as_overlay_not_stripped() {
        let mut models = Assets::new();
        let cube = models.insert(Model::cube(0.7).expect("cube"));
        let mut world = World::new();
        let exit = world
            .spawn((
                Model3d::new(cube),
                Transform3d::from_translation([4.5, 1.0, 19.5]),
            ))
            .id();
        if let Some(mut model) = world.get_mut::<Model3d>(exit) {
            *model = model.clone().with_overlay(true);
        }
        world.entity_mut(exit).insert((
            Trigger::new("level.exit"),
            Position3d::new(Vec3::new(4.5, 1.0, 19.5)).expect("pos"),
            SphereCollider3d::new(1.5).expect("sphere"),
        ));
        let extracted = extract_models(&mut world);
        assert_eq!(extracted.model_count(), 1);
        assert!(extracted.batches()[0].draws()[0].overlay);
    }

    #[test]
    fn entered_stayed_exited_around_player() {
        let mut world = World::new();
        let player = world
            .spawn((
                Transform3d {
                    translation: [0.0, 0.0, 0.0],
                    ..Default::default()
                },
                Position3d::new(Vec3::new(0.0, 0.0, 0.0)).expect("pos"),
                SphereCollider3d::new(0.3).expect("sphere"),
            ))
            .id();
        let trigger_entity = world
            .spawn((
                Transform3d {
                    translation: [0.5, 0.0, 0.0],
                    ..Default::default()
                },
                Trigger::new("level.exit"),
                Position3d::new(Vec3::new(0.5, 0.0, 0.0)).expect("pos"),
                SphereCollider3d::new(1.0).expect("sphere"),
            ))
            .id();
        let _ = trigger_entity;
        let mut tracker = EntityTriggerTracker::default();

        let first = step_trigger_volumes(
            &mut world,
            player,
            Vec3::new(0.0, 0.0, 0.0),
            &mut tracker,
        );
        assert_eq!(first.len(), 1);
        let parsed = match &first[0] {
            SceneInteractionIntent::EmitSignal { name, payload } => {
                try_parse_trigger_signal(name, payload).expect("parse")
            }
            _ => panic!("expected signal"),
        };
        assert_eq!(parsed.phase, ParsedTriggerPhase::Entered);
        assert_eq!(parsed.trigger_id, "level.exit");

        let second = step_trigger_volumes(
            &mut world,
            player,
            Vec3::new(0.0, 0.0, 0.0),
            &mut tracker,
        );
        assert_eq!(second.len(), 1);
        assert!(matches!(
            match &second[0] {
                SceneInteractionIntent::EmitSignal { name, payload } => {
                    try_parse_trigger_signal(name, payload).map(|value| value.phase)
                }
                _ => None,
            },
            Some(ParsedTriggerPhase::Stayed)
        ));

        let third = step_trigger_volumes(
            &mut world,
            player,
            Vec3::new(20.0, 0.0, 0.0),
            &mut tracker,
        );
        assert_eq!(third.len(), 1);
        assert!(matches!(
            match &third[0] {
                SceneInteractionIntent::EmitSignal { name, payload } => {
                    try_parse_trigger_signal(name, payload).map(|value| value.phase)
                }
                _ => None,
            },
            Some(ParsedTriggerPhase::Exited)
        ));
    }
}
