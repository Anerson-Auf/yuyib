//! Play `game.use` → Intent Bridge / QuestBook path.
//!
//! Prefers camera raycast ([`request_use_raycast_3d`]); on miss falls back to a
//! proximity sphere query so third-person chase look still works when the ray
//! grazes past a small collider. Authority is local Accept for Editor Play.

use serde_json::json;
use yuyib_authoring::SceneDocument;
use yuyib_ecs::prelude::{Entity, World};
use yuyib_game_3d::{LocalTransform3d, Transform3d};
use yuyib_gameplay::{
    ActionId, ActionStates, ActionValue, Interactable, InteractionOutcome, InteractionResolved,
    interaction_3d::{UseRaycast3dConfig, UseRaycastOutcome3d, request_use_raycast_3d},
};
use yuyib_physics::{Position3d, Ray3d, Sphere, SphereCollider3d, Vec3, overlap_spheres_3d};
use yuyib_scene_interaction::SceneInteractionIntent;

use yuyib_play::play_log::play_log;

/// Query collider radius (covers a unit cube for ray / proximity hits).
const DEFAULT_INTERACT_RADIUS: f32 = 1.0;
/// Proximity fallback when the camera ray misses (third-person).
const PROXIMITY_REACH: f32 = 3.0;

/// Attaches runtime [`Interactable`] + sphere query colliders from authored
/// `yuyib.interactable` components.
pub fn materialize_interactables(
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
            .find(|component| component.schema().as_str() == "yuyib.interactable")
        else {
            continue;
        };
        let Some(interactable) = interactable_from_payload(component.payload()) else {
            play_log(format!(
                "yuyib-play: skip invalid yuyib.interactable on `{}`",
                record.name.as_deref().unwrap_or("<unnamed>")
            ));
            continue;
        };
        let translation = entity_translation(world, entity).unwrap_or([0.0, 0.0, 0.0]);
        let Ok(position) = Position3d::new(Vec3::new(translation[0], translation[1], translation[2]))
        else {
            continue;
        };
        let Ok(sphere) = SphereCollider3d::new(DEFAULT_INTERACT_RADIUS) else {
            continue;
        };
        let id = interactable.interaction.as_str().to_owned();
        world.entity_mut(entity).insert((interactable, position, sphere));
        count += 1;
        play_log(format!(
            "yuyib-play: materialized Interactable `{id}` on `{}`",
            record.name.as_deref().unwrap_or("<unnamed>")
        ));
    }
    if count > 0 {
        play_log(format!(
            "yuyib-play: {count} interactable(s) ready — approach + E (Diagnostics source=play)"
        ));
    }
}

/// Keeps query [`Position3d`] in sync with authored/runtime transforms.
pub fn sync_interactable_positions(world: &mut World) {
    let entities: Vec<Entity> = {
        let mut query = world.query::<(Entity, &Interactable)>();
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

/// Runs one local-authority use attempt; returns an EmitSignal intent on accept.
pub fn try_use_interaction(
    world: &mut World,
    actions: &mut ActionStates,
    actor: Entity,
    eye: [f32; 3],
    target: [f32; 3],
) -> Option<SceneInteractionIntent> {
    let direction = [
        target[0] - eye[0],
        target[1] - eye[1],
        target[2] - eye[2],
    ];
    let Ok(ray) = Ray3d::new(
        Vec3::new(eye[0], eye[1], eye[2]),
        Vec3::new(direction[0], direction[1], direction[2]),
    ) else {
        play_log("yuyib-play: use ignored — invalid look ray");
        return None;
    };
    let Some(event) = actions.submit(ActionId::new("game.use"), ActionValue::digital(true), 1)
    else {
        play_log("yuyib-play: use ignored — game.use action submit returned None");
        return None;
    };
    let config = UseRaycast3dConfig::game_use();
    let outcome = match request_use_raycast_3d(world, actions, &event, actor, ray, &config) {
        Ok(outcome) => outcome,
        Err(error) => {
            play_log(format!("yuyib-play: use raycast error: {error}"));
            let _ = actions.submit(ActionId::new("game.use"), ActionValue::digital(false), 2);
            return None;
        }
    };
    let _ = actions.submit(ActionId::new("game.use"), ActionValue::digital(false), 2);

    if let UseRaycastOutcome3d::Requested(selected) = outcome {
        let resolution = InteractionResolved {
            request: selected.request,
            outcome: InteractionOutcome::Accepted,
        };
        let name = resolution.request.interaction.as_str().to_owned();
        play_log(format!("yuyib-play: use accepted → interaction `{name}`"));
        return Some(SceneInteractionIntent::EmitSignal {
            name,
            payload: json!({ "amount": 1 }),
        });
    }

    if let Some(intent) = try_proximity_use(world, eye) {
        return Some(intent);
    }
    play_log(format!(
        "yuyib-play: use miss — {outcome:?} (no proximity interactable within {PROXIMITY_REACH}m)"
    ));
    None
}

fn try_proximity_use(world: &mut World, eye: [f32; 3]) -> Option<SceneInteractionIntent> {
    let Ok(probe) = Sphere::new(PROXIMITY_REACH) else {
        return None;
    };
    let center = Vec3::new(eye[0], eye[1], eye[2]);
    let Ok(overlaps) = overlap_spheres_3d(world, center, probe, None) else {
        return None;
    };

    let mut best: Option<(f32, String)> = None;
    for overlap in overlaps {
        let Some(interactable) = world.get::<Interactable>(overlap.entity) else {
            continue;
        };
        if !interactable.enabled {
            continue;
        }
        if interactable
            .required_action
            .as_ref()
            .is_some_and(|required| required.as_str() != "game.use")
        {
            continue;
        }
        let Some(position) = world.get::<Position3d>(overlap.entity) else {
            continue;
        };
        let p = position.get();
        let dx = p.x - eye[0];
        let dy = p.y - eye[1];
        let dz = p.z - eye[2];
        let distance = (dx * dx + dy * dy + dz * dz).sqrt();
        if let Some(max) = interactable.max_distance
            && distance > max
        {
            continue;
        }
        let id = interactable.interaction.as_str().to_owned();
        if best.as_ref().is_none_or(|(best_d, _)| distance < *best_d) {
            best = Some((distance, id));
        }
    }
    let (distance, name) = best?;
    play_log(format!(
        "yuyib-play: use accepted (proximity {distance:.2}m) → interaction `{name}`"
    ));
    Some(SceneInteractionIntent::EmitSignal {
        name,
        payload: json!({ "amount": 1 }),
    })
}

fn interactable_from_payload(payload: &serde_json::Value) -> Option<Interactable> {
    let interaction = payload
        .get("interaction")
        .and_then(|value| value.as_str())
        .or_else(|| payload.as_str())?;
    if interaction.is_empty() {
        return None;
    }
    let mut interactable = Interactable::new(interaction).requiring_action("game.use");
    if let Some(enabled) = payload.get("enabled").and_then(serde_json::Value::as_bool) {
        interactable.enabled = enabled;
    }
    if let Some(action) = payload
        .get("required_action")
        .and_then(serde_json::Value::as_str)
    {
        interactable = interactable.requiring_action(action);
    }
    if let Some(distance) = payload
        .get("max_distance")
        .and_then(serde_json::Value::as_f64)
    {
        interactable = interactable.with_max_distance(distance as f32).ok()?;
    } else {
        interactable = interactable.with_max_distance(3.0).ok()?;
    }
    Some(interactable)
}

fn entity_translation(world: &World, entity: Entity) -> Option<[f32; 3]> {
    if let Some(local) = world.get::<LocalTransform3d>(entity) {
        return Some(local.translation);
    }
    world
        .get::<Transform3d>(entity)
        .map(|transform| transform.translation)
}
