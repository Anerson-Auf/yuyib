//! Semantic pointer/touch interaction selection through deterministic 2D AABB hit testing.
//!
//! This adapter owns neither a window, camera, screen-to-world projection nor
//! physical pointer binding. A host submits a semantic [`ActionEvent`] to
//! [`ActionStates`] and supplies its already finite world-space point. The
//! adapter ignores the actor's own collider, selects the topmost hit AABB, and
//! returns an [`InteractionRequested`] only for a started action.
//!
//! [`InteractionLayer2d`](crate::interaction_2d::InteractionLayer2d) is deliberately gameplay metadata rather than a
//! renderer component. It lets world/UI-style 2D actors establish deterministic
//! pointer priority without coupling interaction selection to `Sprite2d`.
//! A topmost collider without [`Interactable`] blocks targets below it, matching
//! the deliberate line-of-sight policy of the 3D use-raycast adapter.
//!
//! It is not an authority or gameplay-effect system. A game/server layer must
//! still validate the returned request and emit [`crate::InteractionResolved`].

use std::{error::Error, fmt};

use yuyib_ecs::prelude::{Component, Entity, World};
use yuyib_physics::{AabbCollider2d, PhysicsConfigError, Position2d, Vec2, point_in_aabb_2d};

use crate::{
    ActionEvent, ActionId, ActionPhase, ActionStates, Interactable, InteractionMethod,
    InteractionRequested,
};

/// Optional painter-like priority for [`request_pointer_interaction_2d`].
///
/// Higher values are selected before lower values. A collider without this
/// component has layer zero. Equal layers use ascending full ECS entity ID as
/// a deterministic tie-breaker.
#[derive(Clone, Copy, Component, Debug, Default, Eq, PartialEq)]
pub struct InteractionLayer2d(pub i32);

/// Configuration for a semantic 2D pointer/touch interaction attempt.
#[derive(Clone, Debug, PartialEq)]
pub struct PointerInteraction2dConfig {
    action: ActionId,
}

impl PointerInteraction2dConfig {
    /// Creates a configuration activated by `action`.
    #[must_use]
    pub fn new(action: impl Into<ActionId>) -> Self {
        Self {
            action: action.into(),
        }
    }

    /// Creates the conventional `game.use` pointer interaction configuration.
    #[must_use]
    pub fn game_use() -> Self {
        Self::new("game.use")
    }

    /// Returns the semantic action that permits this query.
    #[must_use]
    pub const fn action(&self) -> &ActionId {
        &self.action
    }
}

impl Default for PointerInteraction2dConfig {
    fn default() -> Self {
        Self::game_use()
    }
}

/// Deterministic result of one semantic pointer/touch interaction attempt.
#[derive(Clone, Debug, PartialEq)]
pub enum PointerInteractionOutcome2d {
    /// The supplied event belongs to another semantic action.
    IgnoredAction,
    /// The action event was not a press/start transition.
    IgnoredPhase {
        /// Observed semantic lifecycle phase.
        phase: ActionPhase,
    },
    /// The action event is stale or the action is no longer active in [`ActionStates`].
    InactiveAction,
    /// No non-self AABB collider contained the world point.
    NoHit,
    /// The topmost point-hit collider does not expose an [`Interactable`] component.
    ///
    /// This deliberately blocks a lower-layer target.
    NotInteractable {
        /// Topmost collider containing the point.
        target: Entity,
    },
    /// The topmost interaction candidate is disabled.
    Disabled {
        /// Selected collider entity.
        target: Entity,
    },
    /// The topmost candidate requires a different semantic action.
    WrongAction {
        /// Selected collider entity.
        target: Entity,
    },
    /// The selected candidate has a distance restriction, but `actor` has no [`Position2d`].
    MissingActorPosition {
        /// Entity that initiated the request.
        actor: Entity,
        /// Selected collider entity.
        target: Entity,
    },
    /// The actor-to-target centre distance exceeded the target-specific limit.
    OutOfRange {
        /// Selected collider entity.
        target: Entity,
        /// Euclidean actor-to-target centre distance in world units.
        distance: f32,
        /// Target-specific permitted distance.
        max_distance: f32,
    },
    /// A fully validated, command-like interaction request.
    Requested(InteractionRequested),
}

/// Failure while executing [`request_pointer_interaction_2d`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PointerInteractionError2d {
    /// Caller supplied a `NaN` or infinite world point.
    InvalidWorldPoint(Vec2),
    /// A participating collider/actor position or distance calculation was invalid.
    Physics(PhysicsConfigError),
}

impl fmt::Display for PointerInteractionError2d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidWorldPoint(point) => write!(
                formatter,
                "pointer interaction world point must be finite, got ({}, {})",
                point.x, point.y
            ),
            Self::Physics(source) => write!(formatter, "pointer interaction failed: {source}"),
        }
    }
}

impl Error for PointerInteractionError2d {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidWorldPoint(_) => None,
            Self::Physics(source) => Some(source),
        }
    }
}

fn finite(point: Vec2) -> bool {
    point.x.is_finite() && point.y.is_finite()
}

fn actor_distance_2d(
    actor_position: Vec2,
    target_position: Vec2,
) -> Result<f32, PointerInteractionError2d> {
    if !finite(actor_position) {
        return Err(PointerInteractionError2d::Physics(
            PhysicsConfigError::NonFiniteVec2 {
                field: "actor Position2d",
            },
        ));
    }
    let delta = target_position - actor_position;
    let squared = delta.length_squared();
    if !finite(delta) || !squared.is_finite() {
        return Err(PointerInteractionError2d::Physics(
            PhysicsConfigError::NonFiniteVec2 {
                field: "actor-to-target 2D distance",
            },
        ));
    }
    Ok(squared.sqrt())
}

/// Runs one started semantic action through a 2D pointer/touch world-point query.
///
/// `world_point` must be supplied after caller-owned screen/camera conversion.
/// [`AabbCollider2d`] containment is inclusive, so a point on a collider edge
/// counts as a hit. The actor's own collider is ignored. The selected hit has
/// the greatest [`InteractionLayer2d`] value (or zero when absent), then the
/// lowest full ECS entity ID; no renderer component or texture influences the
/// result.
///
/// The adapter selects exactly one collider before examining [`Interactable`].
/// Consequently a topmost non-interactable collider blocks targets underneath
/// it. Enabled state and required action are checked on that selected entity.
/// A target-specific `max_distance` uses Euclidean distance between actor and
/// target [`Position2d`] centres; missing actor position is an explicit outcome.
///
/// `event` must be the event returned by [`ActionStates::submit`]. Only
/// [`ActionPhase::Started`] is accepted, and current action state must still be
/// active. This prevents repeated requests while held and stale event replay
/// after cancellation/focus loss.
///
/// # Errors
///
/// Returns [`PointerInteractionError2d::InvalidWorldPoint`] for non-finite
/// caller input. [`PointerInteractionError2d::Physics`] reports a non-finite
/// participating collider/actor position or an unrepresentable distance; it
/// never silently treats corrupted ECS geometry as off-screen.
pub fn request_pointer_interaction_2d(
    world: &mut World,
    actions: &ActionStates,
    event: &ActionEvent,
    actor: Entity,
    world_point: Vec2,
    config: &PointerInteraction2dConfig,
) -> Result<PointerInteractionOutcome2d, PointerInteractionError2d> {
    if !finite(world_point) {
        return Err(PointerInteractionError2d::InvalidWorldPoint(world_point));
    }
    if event.action != config.action {
        return Ok(PointerInteractionOutcome2d::IgnoredAction);
    }
    if event.phase != ActionPhase::Started {
        return Ok(PointerInteractionOutcome2d::IgnoredPhase { phase: event.phase });
    }
    let active = actions
        .get(&config.action)
        .is_some_and(crate::ActionState::is_active);
    if !active {
        return Ok(PointerInteractionOutcome2d::InactiveAction);
    }

    let selected = {
        let mut selected: Option<(Entity, i32, Vec2)> = None;
        let mut colliders = world.query::<(
            Entity,
            &Position2d,
            &AabbCollider2d,
            Option<&InteractionLayer2d>,
        )>();
        for (entity, position, collider, layer) in colliders.iter(world) {
            if entity == actor {
                continue;
            }
            let contains = point_in_aabb_2d(world_point, position.0, collider.aabb())
                .map_err(PointerInteractionError2d::Physics)?;
            if !contains {
                continue;
            }
            let layer = layer.map_or(0, |layer| layer.0);
            if selected.is_none_or(|(current, current_layer, _)| {
                layer > current_layer
                    || (layer == current_layer && entity.to_bits() < current.to_bits())
            }) {
                selected = Some((entity, layer, position.0));
            }
        }
        selected
    };

    let Some((target, _, target_position)) = selected else {
        return Ok(PointerInteractionOutcome2d::NoHit);
    };
    let Some(interactable) = world.get::<Interactable>(target) else {
        return Ok(PointerInteractionOutcome2d::NotInteractable { target });
    };
    if !interactable.enabled {
        return Ok(PointerInteractionOutcome2d::Disabled { target });
    }
    if interactable
        .required_action
        .as_ref()
        .is_some_and(|required| required != &config.action)
    {
        return Ok(PointerInteractionOutcome2d::WrongAction { target });
    }
    if let Some(max_distance) = interactable.max_distance {
        let Some(actor_position) = world.get::<Position2d>(actor).map(|position| position.0) else {
            return Ok(PointerInteractionOutcome2d::MissingActorPosition { actor, target });
        };
        let distance = actor_distance_2d(actor_position, target_position)?;
        if distance > max_distance {
            return Ok(PointerInteractionOutcome2d::OutOfRange {
                target,
                distance,
                max_distance,
            });
        }
    }

    Ok(PointerInteractionOutcome2d::Requested(
        InteractionRequested {
            actor,
            target,
            interaction: interactable.interaction.clone(),
            method: InteractionMethod::Pointer,
        },
    ))
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // Test values are exactly representable.
mod tests {
    use super::*;

    fn collider() -> AabbCollider2d {
        AabbCollider2d::new(Vec2::new(1.0, 1.0)).expect("valid collider")
    }

    fn started_event(actions: &mut ActionStates) -> ActionEvent {
        actions
            .submit(
                ActionId::new("game.use"),
                crate::ActionValue::digital(true),
                7,
            )
            .expect("press emits Started")
    }

    fn pointer_query(
        world: &mut World,
        actions: &ActionStates,
        event: &ActionEvent,
        actor: Entity,
    ) -> Result<PointerInteractionOutcome2d, PointerInteractionError2d> {
        request_pointer_interaction_2d(
            world,
            actions,
            event,
            actor,
            Vec2::new(0.0, 0.0),
            &PointerInteraction2dConfig::default(),
        )
    }

    #[test]
    fn pointer_hit_returns_renderer_neutral_request() {
        let mut world = World::new();
        let actor = world.spawn_empty().id();
        let target = world
            .spawn((
                Position2d(Vec2::ZERO),
                collider(),
                Interactable::new("world.open").requiring_action("game.use"),
            ))
            .id();
        let mut actions = ActionStates::default();
        let event = started_event(&mut actions);

        let outcome = pointer_query(&mut world, &actions, &event, actor).expect("valid query");

        assert!(matches!(
            outcome,
            PointerInteractionOutcome2d::Requested(InteractionRequested {
                actor: observed_actor,
                target: observed_target,
                method: InteractionMethod::Pointer,
                ..
            }) if observed_actor == actor && observed_target == target
        ));
    }

    #[test]
    fn highest_layer_then_entity_id_selects_one_topmost_hit() {
        let mut world = World::new();
        let actor = world.spawn_empty().id();
        let lower = world
            .spawn((
                Position2d(Vec2::ZERO),
                collider(),
                InteractionLayer2d(1),
                Interactable::new("world.lower"),
            ))
            .id();
        let first_top = world
            .spawn((
                Position2d(Vec2::ZERO),
                collider(),
                InteractionLayer2d(3),
                Interactable::new("world.first_top"),
            ))
            .id();
        let second_top = world
            .spawn((
                Position2d(Vec2::ZERO),
                collider(),
                InteractionLayer2d(3),
                Interactable::new("world.second_top"),
            ))
            .id();
        let expected = if first_top.to_bits() < second_top.to_bits() {
            first_top
        } else {
            second_top
        };
        let mut actions = ActionStates::default();
        let event = started_event(&mut actions);

        let outcome = pointer_query(&mut world, &actions, &event, actor).expect("valid query");

        assert!(
            matches!(outcome, PointerInteractionOutcome2d::Requested(request) if request.target == expected)
        );
        assert_ne!(expected, lower);
    }

    #[test]
    fn topmost_non_interactable_collider_blocks_lower_target_and_actor_is_ignored() {
        let mut world = World::new();
        let actor = world
            .spawn((Position2d(Vec2::ZERO), collider(), InteractionLayer2d(99)))
            .id();
        let target = world
            .spawn((
                Position2d(Vec2::ZERO),
                collider(),
                InteractionLayer2d(1),
                Interactable::new("world.target"),
            ))
            .id();
        let blocker = world
            .spawn((Position2d(Vec2::ZERO), collider(), InteractionLayer2d(2)))
            .id();
        let mut actions = ActionStates::default();
        let event = started_event(&mut actions);

        let outcome = pointer_query(&mut world, &actions, &event, actor).expect("valid query");

        assert!(
            matches!(outcome, PointerInteractionOutcome2d::NotInteractable { target: observed } if observed == blocker)
        );
        assert_ne!(blocker, target);
    }

    #[test]
    fn action_lifecycle_and_no_hit_outcomes_are_explicit() {
        let mut world = World::new();
        let actor = world.spawn_empty().id();
        let mut actions = ActionStates::default();
        let event = started_event(&mut actions);
        let different = ActionEvent {
            action: ActionId::new("game.other"),
            ..event.clone()
        };
        assert_eq!(
            pointer_query(&mut world, &actions, &different, actor).expect("valid query"),
            PointerInteractionOutcome2d::IgnoredAction
        );
        let held = ActionEvent {
            phase: ActionPhase::Performed,
            ..event.clone()
        };
        assert_eq!(
            pointer_query(&mut world, &actions, &held, actor).expect("valid query"),
            PointerInteractionOutcome2d::IgnoredPhase {
                phase: ActionPhase::Performed
            }
        );
        actions.clear();
        assert_eq!(
            pointer_query(&mut world, &actions, &event, actor).expect("valid query"),
            PointerInteractionOutcome2d::InactiveAction
        );

        let mut actions = ActionStates::default();
        let event = started_event(&mut actions);
        assert_eq!(
            pointer_query(&mut world, &actions, &event, actor).expect("valid query"),
            PointerInteractionOutcome2d::NoHit
        );
    }

    #[test]
    fn disabled_and_wrong_action_candidates_are_not_requested() {
        let mut world = World::new();
        let actor = world.spawn_empty().id();
        let disabled = world
            .spawn((
                Position2d(Vec2::ZERO),
                collider(),
                Interactable {
                    enabled: false,
                    ..Interactable::new("world.disabled")
                },
            ))
            .id();
        let mut actions = ActionStates::default();
        let event = started_event(&mut actions);
        assert!(matches!(
            pointer_query(&mut world, &actions, &event, actor),
            Ok(PointerInteractionOutcome2d::Disabled { target }) if target == disabled
        ));

        world.entity_mut(disabled).despawn();
        let wrong = world
            .spawn((
                Position2d(Vec2::ZERO),
                collider(),
                Interactable::new("world.wrong").requiring_action("game.other"),
            ))
            .id();
        assert!(matches!(
            pointer_query(&mut world, &actions, &event, actor),
            Ok(PointerInteractionOutcome2d::WrongAction { target }) if target == wrong
        ));
    }

    #[test]
    fn target_range_requires_actor_position_and_enforces_distance() {
        let mut world = World::new();
        let actor = world.spawn_empty().id();
        let target = world
            .spawn((
                Position2d(Vec2::new(2.0, 0.0)),
                collider(),
                Interactable::new("world.ranged")
                    .with_max_distance(1.0)
                    .expect("valid range"),
            ))
            .id();
        let mut actions = ActionStates::default();
        let event = started_event(&mut actions);

        assert_eq!(
            pointer_query(&mut world, &actions, &event, actor).expect("valid query"),
            PointerInteractionOutcome2d::NoHit
        );
        let point = Vec2::new(1.0, 0.0);
        assert_eq!(
            request_pointer_interaction_2d(
                &mut world,
                &actions,
                &event,
                actor,
                point,
                &PointerInteraction2dConfig::default(),
            )
            .expect("valid query"),
            PointerInteractionOutcome2d::MissingActorPosition { actor, target }
        );

        world.entity_mut(actor).insert(Position2d(Vec2::ZERO));
        assert_eq!(
            request_pointer_interaction_2d(
                &mut world,
                &actions,
                &event,
                actor,
                point,
                &PointerInteraction2dConfig::default(),
            )
            .expect("valid query"),
            PointerInteractionOutcome2d::OutOfRange {
                target,
                distance: 2.0,
                max_distance: 1.0,
            }
        );
    }

    #[test]
    fn invalid_point_and_ecs_geometry_are_structured_errors() {
        let mut world = World::new();
        let actor = world.spawn_empty().id();
        let mut actions = ActionStates::default();
        let event = started_event(&mut actions);
        assert!(matches!(
            request_pointer_interaction_2d(
                &mut world,
                &actions,
                &event,
                actor,
                Vec2::new(f32::NAN, 0.0),
                &PointerInteraction2dConfig::default(),
            ),
            Err(PointerInteractionError2d::InvalidWorldPoint(point)) if point.x.is_nan()
        ));

        world.spawn((Position2d(Vec2::new(f32::INFINITY, 0.0)), collider()));
        assert!(matches!(
            pointer_query(&mut world, &actions, &event, actor),
            Err(PointerInteractionError2d::Physics(
                PhysicsConfigError::NonFiniteVec2 { .. }
            ))
        ));
    }
}
