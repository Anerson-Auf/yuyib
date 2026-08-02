//! Play `game.use` raycast → Intent Bridge / QuestBook path.
//!
//! Uses existing gameplay [`request_use_raycast_3d`] (sphere query), not Rapier.
//! Authority is local Accept for Editor Play (single-player host).

use serde_json::json;
use yuyib_authoring::SceneDocument;
use yuyib_ecs::prelude::{Entity, World};
use yuyib_game_3d::{LocalTransform3d, Transform3d};
use yuyib_gameplay::{
    ActionId, ActionStates, ActionValue, Interactable, InteractionOutcome, InteractionResolved,
    interaction_3d::{UseRaycast3dConfig, UseRaycastOutcome3d, request_use_raycast_3d},
};
use yuyib_physics::{Position3d, Ray3d, SphereCollider3d, Vec3};
use yuyib_scene_interaction::SceneInteractionIntent;

/// Default reach for Play use queries (matches [`UseRaycast3dConfig::game_use`]).
const DEFAULT_INTERACT_RADIUS: f32 = 0.45;

/// Attaches runtime [`Interactable`] + sphere query colliders from authored
/// `yuyib.interactable` components.
pub fn materialize_interactables(
    document: &SceneDocument,
    world: &mut World,
    entities: &std::collections::BTreeMap<yuyib_authoring::EntityGuid, Entity>,
) {
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
            eprintln!(
                "yuyib-play: skip invalid yuyib.interactable on `{}`",
                record.name.as_deref().unwrap_or("<unnamed>")
            );
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
        world.entity_mut(entity).insert((interactable, position, sphere));
        eprintln!(
            "yuyib-play: materialized Interactable `{}` on `{}`",
            world
                .get::<Interactable>(entity)
                .map(|value| value.interaction.as_str().to_owned())
                .unwrap_or_default(),
            record.name.as_deref().unwrap_or("<unnamed>")
        );
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
    let ray = Ray3d::new(
        Vec3::new(eye[0], eye[1], eye[2]),
        Vec3::new(direction[0], direction[1], direction[2]),
    )
    .ok()?;
    let event = actions.submit(ActionId::new("game.use"), ActionValue::digital(true), 1)?;
    let config = UseRaycast3dConfig::game_use();
    let outcome = request_use_raycast_3d(world, actions, &event, actor, ray, &config).ok();
    let _ = actions.submit(ActionId::new("game.use"), ActionValue::digital(false), 2);
    let outcome = outcome?;
    let UseRaycastOutcome3d::Requested(selected) = outcome else {
        return None;
    };
    let resolution = InteractionResolved {
        request: selected.request,
        outcome: InteractionOutcome::Accepted,
    };
    if resolution.outcome != InteractionOutcome::Accepted {
        return None;
    }
    let name = resolution.request.interaction.as_str().to_owned();
    eprintln!("yuyib-play: use accepted → interaction `{name}`");
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
