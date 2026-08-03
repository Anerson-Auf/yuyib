//! Opt-in sprite animation set + state machine driver for 2D.
//!
//! [`SpriteAnimator2d`] owns named [`SpriteAnimation`] clips and an
//! [`AnimationStateMachine`]. [`step_sprite_animators_2d`] syncs the active
//! clip into [`super::AnimatedSprite2d`], advances frames, and applies
//! `on_finished` transitions. Mid-level [`super::AnimatedSprite2d`] alone still
//! works without an animator.

use std::time::Duration;

use yuyib_2d::SpriteAnimation;
use yuyib_animation::{
    AnimationError, AnimationSet, AnimationStateMachine, PlayOutcome,
};
use yuyib_ecs::{bevy_ecs::entity::Entity, prelude::*};

use super::{AnimatedSprite2d, Sprite2d, SpriteAnimationEvent2d, step_sprite_animations_2d};

/// Named clip set + state machine living on a sprite entity.
#[derive(Component, Clone, Debug, PartialEq)]
pub struct SpriteAnimator2d {
    set: AnimationSet<SpriteAnimation>,
    machine: AnimationStateMachine,
    /// Last clip key applied to the paired [`AnimatedSprite2d`].
    applied_clip: Option<String>,
}

impl SpriteAnimator2d {
    /// Builds an animator; the machine's current clip must exist in `set`.
    ///
    /// # Errors
    ///
    /// Returns [`SpriteAnimatorError2d::Animation`] when the active clip is
    /// missing from the set.
    pub fn new(
        set: AnimationSet<SpriteAnimation>,
        machine: AnimationStateMachine,
    ) -> Result<Self, SpriteAnimatorError2d> {
        let clip = machine.current_clip();
        if !set.contains(clip) {
            return Err(SpriteAnimatorError2d::Animation(AnimationError::MissingClip(
                clip.to_owned(),
            )));
        }
        Ok(Self {
            set,
            machine,
            applied_clip: None,
        })
    }

    /// Returns the owned clip set.
    #[must_use]
    pub const fn set(&self) -> &AnimationSet<SpriteAnimation> {
        &self.set
    }

    /// Returns the owned state machine.
    #[must_use]
    pub const fn machine(&self) -> &AnimationStateMachine {
        &self.machine
    }

    /// Returns a mutable state machine escape hatch.
    pub const fn machine_mut(&mut self) -> &mut AnimationStateMachine {
        &mut self.machine
    }

    /// Active state name (`"walk"`, `"idle"`, …).
    #[must_use]
    pub fn current_state(&self) -> &str {
        self.machine.current_state()
    }

    /// Requests `play("walk")`-style state selection.
    ///
    /// # Errors
    ///
    /// Forwards unknown-state errors from the machine.
    pub fn play(&mut self, name: &str) -> Result<PlayOutcome, SpriteAnimatorError2d> {
        let outcome = self
            .machine
            .play(name)
            .map_err(SpriteAnimatorError2d::Animation)?;
        if outcome == PlayOutcome::Changed {
            self.applied_clip = None;
        }
        Ok(outcome)
    }

    /// Forces a state (and clip rebind) even when already current.
    ///
    /// # Errors
    ///
    /// Forwards unknown-state errors from the machine.
    pub fn play_restart(&mut self, name: &str) -> Result<PlayOutcome, SpriteAnimatorError2d> {
        let outcome = self
            .machine
            .play_restart(name)
            .map_err(SpriteAnimatorError2d::Animation)?;
        self.applied_clip = None;
        Ok(outcome)
    }

    fn sync_animated(
        &mut self,
        animated: &mut AnimatedSprite2d,
    ) -> Result<(), SpriteAnimatorError2d> {
        let clip_name = self.machine.current_clip().to_owned();
        if self.applied_clip.as_deref() == Some(clip_name.as_str()) {
            return Ok(());
        }
        let Some(clip) = self.set.get(&clip_name) else {
            return Err(SpriteAnimatorError2d::Animation(AnimationError::MissingClip(
                clip_name,
            )));
        };
        animated.replace_animation(clip.clone());
        self.applied_clip = Some(clip_name);
        Ok(())
    }
}

/// Policy for selecting idle/walk from planar velocity or move input.
#[derive(Clone, Debug, PartialEq)]
pub struct VelocityFacingPolicy2d {
    /// State played when speed is below the threshold.
    pub idle: String,
    /// State played when speed is at or above the threshold.
    pub walk: String,
    /// Absolute axis magnitude that counts as moving (per component OR).
    pub move_epsilon: f32,
}

impl VelocityFacingPolicy2d {
    /// Builds a policy with the common idle/walk names.
    #[must_use]
    pub fn new(idle: impl Into<String>, walk: impl Into<String>) -> Self {
        Self {
            idle: idle.into(),
            walk: walk.into(),
            move_epsilon: 0.01,
        }
    }

    /// Sets the move threshold.
    #[must_use]
    pub const fn with_move_epsilon(mut self, move_epsilon: f32) -> Self {
        self.move_epsilon = move_epsilon;
        self
    }
}

/// Result of [`resolve_velocity_facing_2d`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VelocityFacingPose2d {
    /// State name to [`SpriteAnimator2d::play`].
    pub state: String,
    /// When `Some`, mirrors the sprite on X (`true` = face left).
    pub flip_x: Option<bool>,
}

/// Picks idle/walk and optional horizontal facing from a 2D axis.
#[must_use]
pub fn resolve_velocity_facing_2d(
    axis: [f32; 2],
    policy: &VelocityFacingPolicy2d,
) -> VelocityFacingPose2d {
    let eps = policy.move_epsilon.abs();
    let moving = axis[0].abs() >= eps || axis[1].abs() >= eps;
    let flip_x = if axis[0].abs() >= eps {
        Some(axis[0] < 0.0)
    } else {
        None
    };
    VelocityFacingPose2d {
        state: if moving {
            policy.walk.clone()
        } else {
            policy.idle.clone()
        },
        flip_x,
    }
}

/// Applies velocity/facing selection to one entity's animator + sprite.
///
/// When the active state is neither `idle` nor `walk` (for example `attack`),
/// locomotion selection is skipped so combat clips are not interrupted; horizontal
/// flip still updates when the axis is decisive.
///
/// # Errors
///
/// Missing components or unknown locomotion states.
pub fn apply_velocity_facing_2d(
    world: &mut World,
    entity: Entity,
    axis: [f32; 2],
    policy: &VelocityFacingPolicy2d,
) -> Result<VelocityFacingPose2d, SpriteAnimatorError2d> {
    let pose = resolve_velocity_facing_2d(axis, policy);
    {
        let mut animator = world
            .get_mut::<SpriteAnimator2d>(entity)
            .ok_or(SpriteAnimatorError2d::MissingAnimator(entity))?;
        let current = animator.current_state();
        let locomotion = current == policy.idle || current == policy.walk;
        if locomotion {
            animator.play(&pose.state)?;
        }
    }
    if let Some(flip) = pose.flip_x {
        let mut sprite = world
            .get_mut::<Sprite2d>(entity)
            .ok_or(SpriteAnimatorError2d::MissingSprite(entity))?;
        let width = sprite.size[0].abs();
        sprite.size[0] = if flip { -width } else { width };
    }
    Ok(pose)
}

/// Four-way facing for top-down sprite sheets (down / up / side + flip).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum Cardinal2d {
    /// Toward +Y / facing the camera (sheet row 0 in the farm pack).
    #[default]
    Down,
    /// Toward -Y / facing away (sheet row 1).
    Up,
    /// Toward +X; use flip for -X (sheet row 2).
    Side,
}

/// Remembers last non-idle facing so idle can keep the correct row.
#[derive(Component, Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SpriteFacing2d {
    /// Last decided cardinal facing.
    pub cardinal: Cardinal2d,
    /// Whether side-facing art is mirrored for left.
    pub flip_left: bool,
}

/// Named idle/walk clips for each cardinal facing.
#[derive(Clone, Debug, PartialEq)]
pub struct CardinalClipPolicy2d {
    /// Idle clip while facing down.
    pub idle_down: String,
    /// Idle clip while facing up.
    pub idle_up: String,
    /// Idle clip while facing side (right-facing art).
    pub idle_side: String,
    /// Walk clip while facing down.
    pub walk_down: String,
    /// Walk clip while facing up.
    pub walk_up: String,
    /// Walk clip while facing side (right-facing art).
    pub walk_side: String,
    /// Absolute axis magnitude that counts as moving.
    pub move_epsilon: f32,
}

impl CardinalClipPolicy2d {
    /// Builds the common `idle_*` / `walk_*` naming scheme.
    #[must_use]
    pub fn standard() -> Self {
        Self {
            idle_down: "idle_down".into(),
            idle_up: "idle_up".into(),
            idle_side: "idle_side".into(),
            walk_down: "walk_down".into(),
            walk_up: "walk_up".into(),
            walk_side: "walk_side".into(),
            move_epsilon: 0.01,
        }
    }

    fn clip_for(&self, cardinal: Cardinal2d, walking: bool) -> &str {
        match (cardinal, walking) {
            (Cardinal2d::Down, false) => self.idle_down.as_str(),
            (Cardinal2d::Up, false) => self.idle_up.as_str(),
            (Cardinal2d::Side, false) => self.idle_side.as_str(),
            (Cardinal2d::Down, true) => self.walk_down.as_str(),
            (Cardinal2d::Up, true) => self.walk_up.as_str(),
            (Cardinal2d::Side, true) => self.walk_side.as_str(),
        }
    }

    fn is_locomotion(&self, state: &str) -> bool {
        state == self.idle_down
            || state == self.idle_up
            || state == self.idle_side
            || state == self.walk_down
            || state == self.walk_up
            || state == self.walk_side
    }
}

/// Result of [`resolve_cardinal_clips_2d`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CardinalClipPose2d {
    /// State name to play.
    pub state: String,
    /// Updated facing memory.
    pub facing: SpriteFacing2d,
}

/// Picks a directional idle/walk clip from move input and last facing.
#[must_use]
pub fn resolve_cardinal_clips_2d(
    axis: [f32; 2],
    facing: SpriteFacing2d,
    policy: &CardinalClipPolicy2d,
) -> CardinalClipPose2d {
    let eps = policy.move_epsilon.abs();
    let moving = axis[0].abs() >= eps || axis[1].abs() >= eps;
    let mut next = facing;
    if moving {
        if axis[0].abs() >= axis[1].abs() && axis[0].abs() >= eps {
            next.cardinal = Cardinal2d::Side;
            next.flip_left = axis[0] < 0.0;
        } else if axis[1] > 0.0 {
            next.cardinal = Cardinal2d::Down;
            next.flip_left = false;
        } else {
            next.cardinal = Cardinal2d::Up;
            next.flip_left = false;
        }
    }
    CardinalClipPose2d {
        state: policy.clip_for(next.cardinal, moving).to_owned(),
        facing: next,
    }
}

/// Applies cardinal clip selection to animator + sprite flip + facing memory.
///
/// # Errors
///
/// Missing components or unknown clip states.
pub fn apply_cardinal_clips_2d(
    world: &mut World,
    entity: Entity,
    axis: [f32; 2],
    policy: &CardinalClipPolicy2d,
) -> Result<CardinalClipPose2d, SpriteAnimatorError2d> {
    let facing = world
        .get::<SpriteFacing2d>(entity)
        .copied()
        .unwrap_or_default();
    let pose = resolve_cardinal_clips_2d(axis, facing, policy);
    {
        let mut animator = world
            .get_mut::<SpriteAnimator2d>(entity)
            .ok_or(SpriteAnimatorError2d::MissingAnimator(entity))?;
        let current = animator.current_state();
        if policy.is_locomotion(current) {
            animator.play(&pose.state)?;
        }
    }
    {
        let mut sprite = world
            .get_mut::<Sprite2d>(entity)
            .ok_or(SpriteAnimatorError2d::MissingSprite(entity))?;
        let width = sprite.size[0].abs();
        sprite.size[0] = if pose.facing.flip_left { -width } else { width };
    }
    if let Some(mut facing_mut) = world.get_mut::<SpriteFacing2d>(entity) {
        *facing_mut = pose.facing;
    } else {
        world.entity_mut(entity).insert(pose.facing);
    }
    Ok(pose)
}

/// Syncs animators, advances [`AnimatedSprite2d`], and applies `on_finished`.
///
/// Entities with only [`AnimatedSprite2d`] (no animator) are still stepped.
pub fn step_sprite_animators_2d(
    world: &mut World,
    delta: Duration,
) -> Vec<SpriteAnimationEvent2d> {
    sync_all_animators(world);
    let events = step_sprite_animations_2d(world, delta);
    let mut finished = Vec::new();
    for event in &events {
        if let SpriteAnimationEvent2d::Finished { entity } = *event {
            finished.push(entity);
        }
    }
    for entity in finished {
        let Some(mut animator) = world.get_mut::<SpriteAnimator2d>(entity) else {
            continue;
        };
        if animator
            .machine
            .on_clip_finished()
            .ok()
            .is_some_and(|outcome| outcome == PlayOutcome::Changed)
        {
            animator.applied_clip = None;
        }
    }
    sync_all_animators(world);
    events
}

fn sync_all_animators(world: &mut World) {
    let mut query = world.query::<(&mut SpriteAnimator2d, &mut AnimatedSprite2d)>();
    for (mut animator, mut animated) in query.iter_mut(world) {
        let _ = animator.sync_animated(&mut animated);
    }
}

/// Failure while driving [`SpriteAnimator2d`].
#[derive(Clone, Debug, PartialEq)]
pub enum SpriteAnimatorError2d {
    /// Underlying set/machine error.
    Animation(AnimationError),
    /// Entity lacks [`SpriteAnimator2d`].
    MissingAnimator(Entity),
    /// Entity lacks [`Sprite2d`].
    MissingSprite(Entity),
}

impl std::fmt::Display for SpriteAnimatorError2d {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Animation(error) => write!(formatter, "sprite animator: {error}"),
            Self::MissingAnimator(entity) => {
                write!(formatter, "entity {entity:?} lacks SpriteAnimator2d")
            }
            Self::MissingSprite(entity) => {
                write!(formatter, "entity {entity:?} lacks Sprite2d")
            }
        }
    }
}

impl std::error::Error for SpriteAnimatorError2d {}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use yuyib_2d::{
        PixelPoint, PlaybackMode, SpriteAnimation, Texture, TextureRegion, TextureSize,
    };
    use yuyib_animation::{AnimationSet, AnimationStateDef, AnimationStateMachine};
    use yuyib_assets::Assets;
    use yuyib_ecs::prelude::*;

    use super::{
        SpriteAnimator2d, VelocityFacingPolicy2d, apply_velocity_facing_2d,
        resolve_velocity_facing_2d, step_sprite_animators_2d,
    };
    use crate::{AnimatedSprite2d, Sprite2d};

    fn region(textures: &mut Assets<Texture>, x: u32) -> TextureRegion {
        let size = TextureSize::new(8, 8).expect("non-empty");
        let texture = textures.insert(Texture::new(size));
        TextureRegion::new(
            texture,
            size,
            PixelPoint { x, y: 0 },
            TextureSize::new(1, 1).expect("cell"),
        )
        .expect("in bounds")
    }

    fn clip(regions: &[TextureRegion], mode: PlaybackMode) -> SpriteAnimation {
        SpriteAnimation::from_regions(regions, Duration::from_millis(50), mode).expect("non-empty")
    }

    #[test]
    fn play_walk_rebinds_animated_sprite() {
        let mut textures = Assets::new();
        let idle_r = region(&mut textures, 0);
        let walk_r = region(&mut textures, 1);
        let idle = clip(&[idle_r], PlaybackMode::Loop);
        let walk = clip(&[walk_r, walk_r], PlaybackMode::Loop);
        let set = AnimationSet::new().with("idle", idle.clone()).with("walk", walk);
        let machine = AnimationStateMachine::new("idle")
            .expect("idle")
            .with_clip("walk")
            .expect("walk");
        let mut world = World::new();
        let entity = world
            .spawn((
                Sprite2d::new(idle_r),
                AnimatedSprite2d::new(idle),
                SpriteAnimator2d::new(set, machine).expect("clips present"),
            ))
            .id();

        world
            .get_mut::<SpriteAnimator2d>(entity)
            .expect("animator")
            .play("walk")
            .expect("walk state");
        step_sprite_animators_2d(&mut world, Duration::ZERO);
        assert_eq!(
            world.get::<Sprite2d>(entity).expect("sprite").region,
            walk_r
        );
        assert_eq!(
            world
                .get::<SpriteAnimator2d>(entity)
                .expect("animator")
                .current_state(),
            "walk"
        );
    }

    #[test]
    fn once_clip_returns_to_idle() {
        let mut textures = Assets::new();
        let idle_r = region(&mut textures, 0);
        let attack_r = region(&mut textures, 1);
        let idle = clip(&[idle_r], PlaybackMode::Loop);
        let attack = clip(&[attack_r], PlaybackMode::Once);
        let set = AnimationSet::new()
            .with("idle", idle.clone())
            .with("attack", attack);
        let machine = AnimationStateMachine::new("idle")
            .expect("idle")
            .with_state(
                "attack",
                AnimationStateDef::clip("attack").on_finished("idle"),
            )
            .expect("attack");
        let mut world = World::new();
        let entity = world
            .spawn((
                Sprite2d::new(idle_r),
                AnimatedSprite2d::new(idle),
                SpriteAnimator2d::new(set, machine).expect("ok"),
            ))
            .id();
        world
            .get_mut::<SpriteAnimator2d>(entity)
            .expect("animator")
            .play("attack")
            .expect("attack");
        step_sprite_animators_2d(&mut world, Duration::from_millis(50));
        assert_eq!(
            world
                .get::<SpriteAnimator2d>(entity)
                .expect("animator")
                .current_state(),
            "idle"
        );
    }

    #[test]
    fn velocity_facing_flips_and_selects_walk() {
        let policy = VelocityFacingPolicy2d::new("idle", "walk");
        let pose = resolve_velocity_facing_2d([-1.0, 0.0], &policy);
        assert_eq!(pose.state, "walk");
        assert_eq!(pose.flip_x, Some(true));

        let mut textures = Assets::new();
        let idle_r = region(&mut textures, 0);
        let walk_r = region(&mut textures, 1);
        let idle = clip(&[idle_r], PlaybackMode::Loop);
        let walk = clip(&[walk_r], PlaybackMode::Loop);
        let set = AnimationSet::new().with("idle", idle.clone()).with("walk", walk);
        let machine = AnimationStateMachine::new("idle")
            .expect("idle")
            .with_clip("walk")
            .expect("walk");
        let mut world = World::new();
        let entity = world
            .spawn((
                Sprite2d::new(idle_r).with_size([10.0, 10.0]),
                AnimatedSprite2d::new(idle),
                SpriteAnimator2d::new(set, machine).expect("ok"),
            ))
            .id();
        apply_velocity_facing_2d(&mut world, entity, [-1.0, 0.0], &policy).expect("apply");
        assert_eq!(
            world.get::<Sprite2d>(entity).expect("sprite").size[0],
            -10.0
        );
    }
}
