//! Semantic `game.use` interaction selection through a deterministic 3D raycast.
//!
//! This adapter deliberately owns neither a camera nor physical input bindings.
//! An input backend submits an [`ActionEvent`] to [`ActionStates`], while a
//! gameplay camera/controller supplies a validated [`yuyib_physics::Ray3d`]. The adapter
//! ignores the actor's own collider, finds the nearest sphere hit, validates
//! [`Interactable`], and returns a command-like [`InteractionRequested`] only
//! for a started semantic use action.
//!
//! It is not an authority system. A server or game rule layer must still turn
//! the returned request into [`InteractionResolved`].

use std::{error::Error, fmt};

use yuyib_ecs::prelude::{Entity, World};
use yuyib_physics::{PhysicsConfigError, Ray3d, RaycastHit3d, raycast_spheres_3d};

use crate::{
    ActionEvent, ActionId, ActionPhase, ActionStates, Interactable, InteractionMethod,
    InteractionRequested,
};

/// Configuration for a semantic use-raycast interaction query.
#[derive(Clone, Debug, PartialEq)]
pub struct UseRaycast3dConfig {
    action: ActionId,
    max_distance: f32,
}

impl UseRaycast3dConfig {
    /// Creates a query configuration for `action` with a finite non-negative reach.
    ///
    /// # Errors
    ///
    /// Returns [`UseRaycastConfigError::InvalidDistance`] when `max_distance`
    /// is negative, NaN or infinite.
    pub fn new(
        action: impl Into<ActionId>,
        max_distance: f32,
    ) -> Result<Self, UseRaycastConfigError> {
        if !max_distance.is_finite() || max_distance < 0.0 {
            return Err(UseRaycastConfigError::InvalidDistance(max_distance));
        }
        Ok(Self {
            action: action.into(),
            max_distance,
        })
    }

    /// Creates the conventional `game.use` query with three world units reach.
    #[must_use]
    pub fn game_use() -> Self {
        Self {
            action: ActionId::new("game.use"),
            max_distance: 3.0,
        }
    }

    /// Returns the semantic action that permits this query.
    #[must_use]
    pub fn action(&self) -> &ActionId {
        &self.action
    }

    /// Returns the query's maximum world-space distance.
    #[must_use]
    pub const fn max_distance(&self) -> f32 {
        self.max_distance
    }
}

impl Default for UseRaycast3dConfig {
    fn default() -> Self {
        Self::game_use()
    }
}

/// Configuration failure for [`UseRaycast3dConfig`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum UseRaycastConfigError {
    /// Reach was negative, NaN or infinite.
    InvalidDistance(f32),
}

impl fmt::Display for UseRaycastConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDistance(value) => write!(
                formatter,
                "use raycast distance must be finite and non-negative, got {value}"
            ),
        }
    }
}

impl Error for UseRaycastConfigError {}

/// Successful candidate produced by [`request_use_raycast_3d`].
#[derive(Clone, Debug, PartialEq)]
pub struct UseRaycastRequest3d {
    /// Command to pass into an authority/game-rule interaction system.
    pub request: InteractionRequested,
    /// Physics hit used to select the target.
    pub hit: RaycastHit3d,
}

/// Deterministic result of one semantic use-raycast attempt.
#[derive(Clone, Debug, PartialEq)]
pub enum UseRaycastOutcome3d {
    /// The supplied event belongs to another semantic action.
    IgnoredAction,
    /// The action event was not a press/start transition.
    IgnoredPhase {
        /// Observed semantic lifecycle phase.
        phase: ActionPhase,
    },
    /// The action event is stale or the action is no longer active in [`ActionStates`].
    InactiveAction,
    /// No non-self sphere collider was hit within the configured reach.
    NoHit,
    /// The nearest raycast hit does not expose an [`Interactable`] component.
    NotInteractable {
        /// Collider entity selected by the raycast.
        target: Entity,
    },
    /// The candidate interaction is disabled.
    Disabled {
        /// Collider entity selected by the raycast.
        target: Entity,
    },
    /// The target requires a different semantic action.
    WrongAction {
        /// Collider entity selected by the raycast.
        target: Entity,
    },
    /// The hit exceeded the target-specific interaction reach.
    OutOfRange {
        /// Collider entity selected by the raycast.
        target: Entity,
        /// Distance along the ray in world units.
        distance: f32,
        /// Target-specific permitted distance.
        max_distance: f32,
    },
    /// A fully validated interaction command and the hit that selected it.
    Requested(UseRaycastRequest3d),
}

/// Runs one started semantic action through a 3D interaction raycast.
///
/// The raycast ignores `actor`, preventing a player sphere collider from
/// selecting itself. `raycast_spheres_3d` deterministically picks the nearest
/// remaining collider and resolves equal distances by entity ID. A closer
/// non-interactable collider blocks a target behind it deliberately; that is a
/// physics/line-of-sight decision, not a UI focus query.
///
/// `event` must be the event returned by [`ActionStates::submit`]. The adapter
/// only reacts to [`ActionPhase::Started`], so holding a button does not create
/// repeated interaction requests. It also checks the current `actions` state,
/// preventing an old event from being replayed after focus loss or cancellation.
///
/// # Errors
///
/// Returns [`UseRaycastError::Physics`] only when the supplied physics query
/// cannot run. A validated [`UseRaycast3dConfig`] and [`Ray3d`] normally make
/// this unreachable unless ECS state is extended with an invalid physics type.
pub fn request_use_raycast_3d(
    world: &mut World,
    actions: &ActionStates,
    event: &ActionEvent,
    actor: Entity,
    ray: Ray3d,
    config: &UseRaycast3dConfig,
) -> Result<UseRaycastOutcome3d, UseRaycastError> {
    if event.action != config.action {
        return Ok(UseRaycastOutcome3d::IgnoredAction);
    }
    if event.phase != ActionPhase::Started {
        return Ok(UseRaycastOutcome3d::IgnoredPhase { phase: event.phase });
    }
    let active = actions
        .get(&config.action)
        .is_some_and(crate::ActionState::is_active);
    if !active {
        return Ok(UseRaycastOutcome3d::InactiveAction);
    }

    let Some(hit) = raycast_spheres_3d(world, ray, config.max_distance, Some(actor))
        .map_err(UseRaycastError::Physics)?
    else {
        return Ok(UseRaycastOutcome3d::NoHit);
    };
    let Some(interactable) = world.get::<Interactable>(hit.entity) else {
        return Ok(UseRaycastOutcome3d::NotInteractable { target: hit.entity });
    };
    if !interactable.enabled {
        return Ok(UseRaycastOutcome3d::Disabled { target: hit.entity });
    }
    if interactable
        .required_action
        .as_ref()
        .is_some_and(|required| required != &config.action)
    {
        return Ok(UseRaycastOutcome3d::WrongAction { target: hit.entity });
    }
    if let Some(max_distance) = interactable.max_distance
        && hit.hit.distance > max_distance
    {
        return Ok(UseRaycastOutcome3d::OutOfRange {
            target: hit.entity,
            distance: hit.hit.distance,
            max_distance,
        });
    }
    Ok(UseRaycastOutcome3d::Requested(UseRaycastRequest3d {
        request: InteractionRequested {
            actor,
            target: hit.entity,
            interaction: interactable.interaction.clone(),
            method: InteractionMethod::Action(config.action.clone()),
        },
        hit,
    }))
}

/// Failure while executing [`request_use_raycast_3d`].
#[derive(Debug)]
pub enum UseRaycastError {
    /// Underlying deterministic sphere raycast could not execute.
    Physics(PhysicsConfigError),
}

impl fmt::Display for UseRaycastError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Physics(source) => write!(formatter, "use raycast failed: {source}"),
        }
    }
}

impl Error for UseRaycastError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Physics(source) => Some(source),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yuyib_physics::{Position3d, SphereCollider3d, Vec3};

    fn use_event(actions: &mut ActionStates) -> ActionEvent {
        actions
            .submit(
                ActionId::new("game.use"),
                crate::ActionValue::digital(true),
                7,
            )
            .expect("press emits Started")
    }

    fn ray() -> Ray3d {
        Ray3d::new(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0)).expect("valid ray")
    }

    #[test]
    fn raycast_ignores_actor_and_returns_interactable_request() {
        let mut world = World::new();
        let actor = world
            .spawn((
                Position3d::new(Vec3::ZERO).expect("finite"),
                SphereCollider3d::new(1.0).expect("positive"),
            ))
            .id();
        let target = world
            .spawn((
                Position3d::new(Vec3::new(0.0, 0.0, -2.0)).expect("finite"),
                SphereCollider3d::new(0.5).expect("positive"),
                Interactable::new("world.open").requiring_action("game.use"),
            ))
            .id();
        let mut actions = ActionStates::default();
        let event = use_event(&mut actions);
        let outcome = request_use_raycast_3d(
            &mut world,
            &actions,
            &event,
            actor,
            ray(),
            &UseRaycast3dConfig::default(),
        )
        .expect("query succeeds");
        assert!(
            matches!(outcome, UseRaycastOutcome3d::Requested(request) if request.request.target == target)
        );
    }

    #[test]
    fn non_interactable_nearest_hit_blocks_target_behind_it() {
        let mut world = World::new();
        let actor = world.spawn_empty().id();
        let blocker = world
            .spawn((
                Position3d::new(Vec3::new(0.0, 0.0, -1.0)).expect("finite"),
                SphereCollider3d::new(0.25).expect("positive"),
            ))
            .id();
        let _target = world
            .spawn((
                Position3d::new(Vec3::new(0.0, 0.0, -2.0)).expect("finite"),
                SphereCollider3d::new(0.25).expect("positive"),
                Interactable::new("world.open"),
            ))
            .id();
        let mut actions = ActionStates::default();
        let event = use_event(&mut actions);
        let outcome = request_use_raycast_3d(
            &mut world,
            &actions,
            &event,
            actor,
            ray(),
            &UseRaycast3dConfig::default(),
        )
        .expect("query succeeds");
        assert!(
            matches!(outcome, UseRaycastOutcome3d::NotInteractable { target } if target == blocker)
        );
    }

    #[test]
    fn target_range_is_validated_after_physics_hit() {
        let mut world = World::new();
        let actor = world.spawn_empty().id();
        let target = world
            .spawn((
                Position3d::new(Vec3::new(0.0, 0.0, -2.0)).expect("finite"),
                SphereCollider3d::new(0.25).expect("positive"),
                Interactable::new("world.open")
                    .with_max_distance(1.0)
                    .expect("valid range"),
            ))
            .id();
        let mut actions = ActionStates::default();
        let event = use_event(&mut actions);
        let outcome = request_use_raycast_3d(
            &mut world,
            &actions,
            &event,
            actor,
            ray(),
            &UseRaycast3dConfig::default(),
        )
        .expect("query succeeds");
        assert!(
            matches!(outcome, UseRaycastOutcome3d::OutOfRange { target: observed, .. } if observed == target)
        );
    }
}
