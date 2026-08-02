//! Stateful cross-fading between imported skeletal animation clips.

use std::{error::Error, fmt};

use super::{
    AnimationClipIndex, AnimationPlayState, AnimationPlayer, AnimationSampleError,
    AnimationSnapshot, ImportedScene, LocalTransform, NodeIndex, snapshot_from_local_transforms,
};

/// Validated duration of one animation cross-fade.
///
/// Zero means an immediate switch. The ten-second upper bound prevents a bad
/// configuration or network value from retaining a transition source pose for
/// an effectively unbounded interval.
#[derive(Clone, Copy, Debug, Default, PartialEq, PartialOrd)]
pub struct AnimationCrossFadeDuration(f32);

impl AnimationCrossFadeDuration {
    /// Largest accepted transition duration.
    pub const MAX_SECONDS: f32 = 10.0;
    /// Immediate clip replacement without pose blending.
    pub const IMMEDIATE: Self = Self(0.0);

    /// Creates a finite duration in `0.0..=10.0` seconds.
    ///
    /// # Errors
    ///
    /// Returns [`AnimationCrossFadeDurationError`] for NaN, infinity, negative
    /// values or durations above [`Self::MAX_SECONDS`].
    pub fn new(seconds: f32) -> Result<Self, AnimationCrossFadeDurationError> {
        if !seconds.is_finite() || !(0.0..=Self::MAX_SECONDS).contains(&seconds) {
            return Err(AnimationCrossFadeDurationError);
        }
        Ok(Self(seconds))
    }

    /// Returns the validated duration in seconds.
    #[must_use]
    pub const fn seconds(self) -> f32 {
        self.0
    }

    /// Returns whether this duration requests an immediate switch.
    #[must_use]
    pub fn is_immediate(self) -> bool {
        self.0 <= f32::EPSILON
    }
}

/// Invalid cross-fade duration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AnimationCrossFadeDurationError;

impl fmt::Display for AnimationCrossFadeDurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "animation cross-fade duration must be finite and in 0..={} seconds",
            AnimationCrossFadeDuration::MAX_SECONDS
        )
    }
}

impl Error for AnimationCrossFadeDurationError {}

/// Result of requesting a target clip.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnimationCrossFadeChange {
    /// The requested clip is already active or already targeted.
    Unchanged,
    /// A transition from the current sampled pose was started.
    Started,
    /// An in-progress transition was replaced without jumping back to its source clip.
    Retargeted,
    /// The target became active immediately.
    CompletedImmediately,
}

/// Stateful renderer-neutral skeletal animation cross-fade mixer.
///
/// The active and target clips are typed [`AnimationClipIndex`] values. A
/// transition freezes the last visible source snapshot and advances the target
/// player. Retargeting promotes the last blended snapshot to the new source,
/// avoiding a jump back to the last fully active clip during rapid eight-way
/// locomotion changes.
///
/// [`Self::advance_and_snapshot`] returns a borrowed ordinary
/// [`AnimationSnapshot`], so existing skeletal renderers need no mixer-specific
/// path and callers do not clone the final pose. Memory is bounded to one
/// retained source pose, one cached output pose and one temporary target sample.
/// The existing glTF sampler still builds snapshot vectors when animation time
/// changes; blending consumes the target's local/morph allocations instead of
/// creating a third copy of them.
///
/// ```no_run
/// # use yuyib_gltf::{
/// #     AnimationClipIndex, AnimationCrossFadeDuration, AnimationCrossFadeMixer,
/// #     ImportedScene,
/// # };
/// # fn frame(scene: &ImportedScene) -> Result<(), Box<dyn std::error::Error>> {
/// let walk = AnimationClipIndex::new(0);
/// let strafe = AnimationClipIndex::new(1);
/// let mut mixer = AnimationCrossFadeMixer::new(scene, walk)?;
/// mixer.transition_to(scene, strafe, AnimationCrossFadeDuration::new(0.15)?)?;
/// let snapshot = mixer.advance_and_snapshot(scene, 1.0 / 60.0)?;
/// # let _ = snapshot;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct AnimationCrossFadeMixer {
    active: AnimationPlayer,
    target: Option<AnimationCrossFadeTarget>,
    source_snapshot: Option<AnimationSnapshot>,
    output_snapshot: Option<AnimationSnapshot>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct AnimationCrossFadeTarget {
    player: AnimationPlayer,
    elapsed_seconds: f32,
    duration: AnimationCrossFadeDuration,
}

impl AnimationCrossFadeMixer {
    /// Creates a playing mixer and validates its initial clip and pose.
    ///
    /// # Errors
    ///
    /// Returns [`AnimationCrossFadeError`] when `active_clip` is absent or its
    /// initial snapshot cannot be constructed.
    pub fn new(
        scene: &ImportedScene,
        active_clip: AnimationClipIndex,
    ) -> Result<Self, AnimationCrossFadeError> {
        Self::from_player(scene, AnimationPlayer::new(active_clip))
    }

    /// Creates a mixer from existing playback state and samples it once.
    ///
    /// # Errors
    ///
    /// Returns [`AnimationCrossFadeError`] when the player's clip/pose is invalid.
    pub fn from_player(
        scene: &ImportedScene,
        active: AnimationPlayer,
    ) -> Result<Self, AnimationCrossFadeError> {
        let output_snapshot = Some(
            active
                .snapshot(scene)
                .map_err(AnimationCrossFadeError::Sample)?,
        );
        Ok(Self {
            active,
            target: None,
            source_snapshot: None,
            output_snapshot,
        })
    }

    /// Returns the last fully activated clip.
    #[must_use]
    pub const fn active_clip(&self) -> AnimationClipIndex {
        self.active.clip()
    }

    /// Returns the clip currently being faded toward.
    #[must_use]
    pub fn target_clip(&self) -> Option<AnimationClipIndex> {
        self.target.map(|target| target.player.clip())
    }

    /// Returns whether a non-immediate transition is active.
    #[must_use]
    pub const fn is_transitioning(&self) -> bool {
        self.target.is_some()
    }

    /// Returns transition completion in `0.0..=1.0`, or `1.0` when stable.
    #[must_use]
    pub fn transition_progress(&self) -> f32 {
        self.target.map_or(1.0, |target| {
            (target.elapsed_seconds / target.duration.seconds()).clamp(0.0, 1.0)
        })
    }

    /// Returns the cached visible pose without advancing animation time.
    ///
    /// # Errors
    ///
    /// Returns a sampling error only if the cache was explicitly invalidated by
    /// an immediate switch and the new active clip cannot be sampled.
    pub fn snapshot(
        &mut self,
        scene: &ImportedScene,
    ) -> Result<&AnimationSnapshot, AnimationCrossFadeError> {
        if self.output_snapshot.is_none() && self.source_snapshot.is_none() {
            self.output_snapshot = Some(
                self.active
                    .snapshot(scene)
                    .map_err(AnimationCrossFadeError::Sample)?,
            );
        }
        self.output_snapshot
            .as_ref()
            .or(self.source_snapshot.as_ref())
            .ok_or(AnimationCrossFadeError::MissingSnapshot)
    }

    /// Requests a target clip using a bounded duration.
    ///
    /// A request for the current target or the stable active clip is a no-op.
    /// Retargeting starts from the last visible blended pose. Switching back to
    /// the active clip reuses its retained playback time instead of restarting
    /// it at zero.
    ///
    /// # Errors
    ///
    /// Returns [`AnimationCrossFadeError::InvalidClip`] before changing state
    /// when `target_clip` is absent from `scene`.
    pub fn transition_to(
        &mut self,
        scene: &ImportedScene,
        target_clip: AnimationClipIndex,
        duration: AnimationCrossFadeDuration,
    ) -> Result<AnimationCrossFadeChange, AnimationCrossFadeError> {
        if scene.animations().get(target_clip.get()).is_none() {
            return Err(AnimationCrossFadeError::InvalidClip(target_clip));
        }
        if self.target_clip() == Some(target_clip)
            || (self.target.is_none() && self.active.clip() == target_clip)
        {
            return Ok(AnimationCrossFadeChange::Unchanged);
        }
        if duration.is_immediate() {
            self.active = self.player_for_target(target_clip);
            self.target = None;
            self.source_snapshot = None;
            self.output_snapshot = None;
            return Ok(AnimationCrossFadeChange::CompletedImmediately);
        }

        let was_transitioning = self.target.is_some();
        let current = if let Some(output) = self.output_snapshot.take() {
            output
        } else if let Some(source) = self.source_snapshot.take() {
            source
        } else {
            self.active
                .snapshot(scene)
                .map_err(AnimationCrossFadeError::Sample)?
        };
        self.source_snapshot = Some(current);
        self.target = Some(AnimationCrossFadeTarget {
            player: self.player_for_target(target_clip),
            elapsed_seconds: 0.0,
            duration,
        });
        Ok(if was_transitioning {
            AnimationCrossFadeChange::Retargeted
        } else {
            AnimationCrossFadeChange::Started
        })
    }

    /// Advances playback and returns the new cached visible pose.
    ///
    /// Stable paused/stopped mixers return their existing cache directly.
    /// During a transition, pausing also pauses transition progress.
    ///
    /// # Errors
    ///
    /// Returns a structured error for invalid delta, clip sampling, snapshot
    /// incompatibility, hierarchy failure or invalid quaternion data.
    pub fn advance_and_snapshot(
        &mut self,
        scene: &ImportedScene,
        delta_seconds: f32,
    ) -> Result<&AnimationSnapshot, AnimationCrossFadeError> {
        self.validate_advance(delta_seconds)?;
        let Some(mut target) = self.target else {
            if self.active.state() != AnimationPlayState::Playing {
                return self.snapshot(scene);
            }
            self.active
                .advance(scene, delta_seconds)
                .map_err(AnimationCrossFadeError::Sample)?;
            self.output_snapshot = Some(
                self.active
                    .snapshot(scene)
                    .map_err(AnimationCrossFadeError::Sample)?,
            );
            return self.snapshot(scene);
        };

        if target.player.state() != AnimationPlayState::Playing {
            return self.snapshot(scene);
        }
        target
            .player
            .advance(scene, delta_seconds)
            .map_err(AnimationCrossFadeError::Sample)?;
        target.elapsed_seconds += delta_seconds;
        let factor = (target.elapsed_seconds / target.duration.seconds()).clamp(0.0, 1.0);
        let target_snapshot = target
            .player
            .snapshot(scene)
            .map_err(AnimationCrossFadeError::Sample)?;
        if factor >= 1.0 {
            self.active = target.player;
            self.target = None;
            self.source_snapshot = None;
            self.output_snapshot = Some(target_snapshot);
            return self.snapshot(scene);
        }

        let source = self
            .source_snapshot
            .as_ref()
            .ok_or(AnimationCrossFadeError::MissingSnapshot)?;
        self.output_snapshot = Some(blend_animation_snapshots_owned(
            scene,
            source,
            target_snapshot,
            factor,
        )?);
        self.target = Some(target);
        self.snapshot(scene)
    }

    /// Starts or resumes stable and target clip playback.
    pub fn play(&mut self) {
        self.active.play();
        if let Some(target) = &mut self.target {
            target.player.play();
        }
    }

    /// Pauses playback and transition progress without changing the cached pose.
    pub fn pause(&mut self) {
        self.active.pause();
        if let Some(target) = &mut self.target {
            target.player.pause();
        }
    }

    /// Sets a shared positive finite speed for active and target players.
    ///
    /// # Errors
    ///
    /// Returns [`AnimationCrossFadeError::Sample`] without mutation when speed
    /// is negative, NaN or infinite.
    pub fn set_speed(&mut self, speed: f32) -> Result<(), AnimationCrossFadeError> {
        if !speed.is_finite() || speed < 0.0 {
            return Err(AnimationCrossFadeError::Sample(
                AnimationSampleError::InvalidPlaybackSpeed,
            ));
        }
        self.active
            .set_speed(speed)
            .map_err(AnimationCrossFadeError::Sample)?;
        if let Some(target) = &mut self.target {
            target
                .player
                .set_speed(speed)
                .map_err(AnimationCrossFadeError::Sample)?;
        }
        Ok(())
    }

    fn player_for_target(&self, clip: AnimationClipIndex) -> AnimationPlayer {
        if clip == self.active.clip() {
            return self.active;
        }
        let mut player = AnimationPlayer::new(clip).with_looping(self.active.looping());
        // Safe because the active player's speed was validated by AnimationPlayer.
        player
            .set_speed(self.active.speed())
            .expect("validated active animation speed");
        match self.active.state() {
            AnimationPlayState::Playing => player.play(),
            AnimationPlayState::Paused => player.pause(),
            AnimationPlayState::Stopped => player.stop(),
        }
        player
    }

    fn validate_advance(&self, delta_seconds: f32) -> Result<(), AnimationCrossFadeError> {
        let speed = self
            .target
            .map_or(self.active.speed(), |target| target.player.speed());
        if !delta_seconds.is_finite()
            || delta_seconds < 0.0
            || !(delta_seconds * speed).is_finite()
            || self
                .target
                .is_some_and(|target| !(target.elapsed_seconds + delta_seconds).is_finite())
        {
            return Err(AnimationCrossFadeError::Sample(
                AnimationSampleError::InvalidDelta,
            ));
        }
        Ok(())
    }
}

/// Blends two snapshots from the same scene into a new GPU-ready snapshot.
///
/// Translation, scale and morph weights use linear interpolation. Quaternion
/// rotations use normalized linear interpolation after flipping the target
/// quaternion when necessary to select the shortest four-dimensional arc.
/// Matrix-authored nodes must be identical because affine matrix blending can
/// introduce shear and singular transforms.
///
/// # Errors
///
/// Returns [`AnimationCrossFadeError`] for a factor outside `0.0..=1.0`,
/// snapshots incompatible with `scene`, mismatched transform representations,
/// invalid quaternion data, or failure to rebuild hierarchy/skin palettes.
pub fn blend_animation_snapshots(
    scene: &ImportedScene,
    source: &AnimationSnapshot,
    target: &AnimationSnapshot,
    factor: f32,
) -> Result<AnimationSnapshot, AnimationCrossFadeError> {
    blend_animation_snapshots_owned(scene, source, target.clone(), factor)
}

fn blend_animation_snapshots_owned(
    scene: &ImportedScene,
    source: &AnimationSnapshot,
    mut target: AnimationSnapshot,
    factor: f32,
) -> Result<AnimationSnapshot, AnimationCrossFadeError> {
    if !factor.is_finite() || !(0.0..=1.0).contains(&factor) {
        return Err(AnimationCrossFadeError::InvalidBlendFactor);
    }
    if source.local_transforms.len() != scene.nodes().len()
        || target.local_transforms.len() != scene.nodes().len()
    {
        return Err(AnimationCrossFadeError::NodeCountMismatch);
    }
    for (node, (from, to)) in source
        .local_transforms
        .iter()
        .copied()
        .zip(&mut target.local_transforms)
        .enumerate()
    {
        *to = match (from, *to) {
            (
                LocalTransform::Trs {
                    translation: from_translation,
                    rotation: from_rotation,
                    scale: from_scale,
                },
                LocalTransform::Trs {
                    translation: to_translation,
                    rotation: to_rotation,
                    scale: to_scale,
                },
            ) => LocalTransform::Trs {
                translation: lerp3(from_translation, to_translation, factor),
                rotation: nlerp_quaternion_shortest(from_rotation, to_rotation, factor).ok_or(
                    AnimationCrossFadeError::InvalidQuaternion(NodeIndex::new(node)),
                )?,
                scale: lerp3(from_scale, to_scale, factor),
            },
            (
                LocalTransform::Matrix { column_major: from },
                LocalTransform::Matrix { column_major: to },
            ) if matrices_are_identical(from, to) => LocalTransform::Matrix { column_major: from },
            (LocalTransform::Matrix { .. }, LocalTransform::Matrix { .. }) => {
                return Err(AnimationCrossFadeError::MatrixTransformMismatch(
                    NodeIndex::new(node),
                ));
            }
            _ => {
                return Err(AnimationCrossFadeError::TransformKindMismatch(
                    NodeIndex::new(node),
                ));
            }
        };
    }

    if source.morph_weights.len() != scene.nodes().len()
        || target.morph_weights.len() != scene.nodes().len()
    {
        return Err(AnimationCrossFadeError::MorphNodeCountMismatch);
    }
    for (node, (from, to)) in source
        .morph_weights
        .iter()
        .zip(&mut target.morph_weights)
        .enumerate()
    {
        if from.len() != to.len() {
            return Err(AnimationCrossFadeError::MorphWeightCountMismatch(
                NodeIndex::new(node),
            ));
        }
        for (from, to) in from.iter().zip(to) {
            *to = from + (*to - from) * factor;
        }
    }
    snapshot_from_local_transforms(scene, target.local_transforms, target.morph_weights)
        .map_err(AnimationCrossFadeError::Sample)
}

fn lerp3(from: [f32; 3], to: [f32; 3], factor: f32) -> [f32; 3] {
    std::array::from_fn(|index| from[index] + (to[index] - from[index]) * factor)
}

fn matrices_are_identical(left: [f32; 16], right: [f32; 16]) -> bool {
    left.into_iter()
        .zip(right)
        .all(|(left, right)| left.to_bits() == right.to_bits())
}

fn nlerp_quaternion_shortest(from: [f32; 4], mut to: [f32; 4], factor: f32) -> Option<[f32; 4]> {
    let from = normalize_quaternion(from)?;
    to = normalize_quaternion(to)?;
    let dot = from
        .iter()
        .zip(to)
        .map(|(left, right)| left * right)
        .sum::<f32>();
    if dot < 0.0 {
        to = to.map(|value| -value);
    }
    normalize_quaternion(std::array::from_fn(|index| {
        from[index] + (to[index] - from[index]) * factor
    }))
}

fn normalize_quaternion(value: [f32; 4]) -> Option<[f32; 4]> {
    let length_squared = value
        .iter()
        .map(|component| component * component)
        .sum::<f32>();
    if !length_squared.is_finite() || length_squared <= f32::EPSILON {
        return None;
    }
    let inverse_length = length_squared.sqrt().recip();
    let normalized = value.map(|component| component * inverse_length);
    normalized
        .iter()
        .all(|component| component.is_finite())
        .then_some(normalized)
}

/// Cross-fade setup, sampling or pose compatibility failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnimationCrossFadeError {
    /// A target clip was absent from the imported scene.
    InvalidClip(AnimationClipIndex),
    /// Blend factor was non-finite or outside `0.0..=1.0`.
    InvalidBlendFactor,
    /// A snapshot did not contain exactly one local transform per scene node.
    NodeCountMismatch,
    /// A snapshot did not contain one morph-weight vector per scene node.
    MorphNodeCountMismatch,
    /// Source and target used TRS versus matrix transforms for the same node.
    TransformKindMismatch(NodeIndex),
    /// Matrix-authored source and target transforms were not identical.
    MatrixTransformMismatch(NodeIndex),
    /// Source and target morph target counts differed for one node.
    MorphWeightCountMismatch(NodeIndex),
    /// A source, target or blended quaternion was non-finite or degenerate.
    InvalidQuaternion(NodeIndex),
    /// Internal state had no source or output pose.
    MissingSnapshot,
    /// Existing glTF sampling or hierarchy construction failed.
    Sample(AnimationSampleError),
}

impl fmt::Display for AnimationCrossFadeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidClip(clip) => {
                write!(
                    formatter,
                    "cross-fade target clip {} does not exist",
                    clip.get()
                )
            }
            Self::InvalidBlendFactor => {
                formatter.write_str("animation blend factor must be finite and in 0..=1")
            }
            Self::NodeCountMismatch => formatter
                .write_str("animation snapshots must contain one local transform per scene node"),
            Self::MorphNodeCountMismatch => formatter
                .write_str("animation snapshots must contain morph weights for every scene node"),
            Self::TransformKindMismatch(node) => write!(
                formatter,
                "animation snapshots use incompatible transform kinds for node {}",
                node.get()
            ),
            Self::MatrixTransformMismatch(node) => write!(
                formatter,
                "matrix-authored node {} differs between animation snapshots",
                node.get()
            ),
            Self::MorphWeightCountMismatch(node) => write!(
                formatter,
                "animation snapshots contain different morph target counts for node {}",
                node.get()
            ),
            Self::InvalidQuaternion(node) => write!(
                formatter,
                "animation snapshot contains an invalid quaternion for node {}",
                node.get()
            ),
            Self::MissingSnapshot => formatter.write_str("animation mixer has no sampled pose"),
            Self::Sample(source) => write!(formatter, "animation cross-fade failed: {source}"),
        }
    }
}

impl Error for AnimationCrossFadeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Sample(source) => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AnimationInterpolation, AnimationProperty, AnimationValue, ImportedAnimationClip,
        ImportedAnimationTrack, ImportedNode, sample_animation,
    };

    fn trs(translation: [f32; 3], rotation: [f32; 4], scale: [f32; 3]) -> LocalTransform {
        LocalTransform::Trs {
            translation,
            rotation,
            scale,
        }
    }

    fn scene_with_two_translation_clips() -> ImportedScene {
        let track = |value: f32| ImportedAnimationTrack {
            node: NodeIndex::new(0),
            property: AnimationProperty::Translation,
            interpolation: AnimationInterpolation::Linear,
            times_seconds: vec![0.0, 1.0],
            values: vec![
                AnimationValue::Vector3([value, 0.0, 0.0]),
                AnimationValue::Vector3([value, 0.0, 0.0]),
            ],
        };
        ImportedScene {
            nodes: vec![ImportedNode {
                name: Some("Root".to_owned()),
                local_transform: trs([0.0; 3], [0.0, 0.0, 0.0, 1.0], [1.0; 3]),
                mesh: None,
                camera: None,
                directional_light: None,
                skin: None,
                morph_weights: vec![0.0],
                children: Vec::new(),
            }],
            animations: vec![
                ImportedAnimationClip {
                    name: Some("From".to_owned()),
                    duration_seconds: 1.0,
                    tracks: vec![track(0.0)],
                    morph_tracks: Vec::new(),
                },
                ImportedAnimationClip {
                    name: Some("To".to_owned()),
                    duration_seconds: 1.0,
                    tracks: vec![track(10.0)],
                    morph_tracks: Vec::new(),
                },
            ],
            ..ImportedScene::default()
        }
    }

    fn translation_x(snapshot: &AnimationSnapshot) -> f32 {
        match snapshot.local_transforms()[0] {
            LocalTransform::Trs { translation, .. } => translation[0],
            LocalTransform::Matrix { .. } => panic!("fixture node must use TRS"),
        }
    }

    #[test]
    fn duration_is_explicitly_bounded() {
        assert_eq!(
            AnimationCrossFadeDuration::new(-0.1),
            Err(AnimationCrossFadeDurationError)
        );
        assert_eq!(
            AnimationCrossFadeDuration::new(f32::NAN),
            Err(AnimationCrossFadeDurationError)
        );
        assert!(AnimationCrossFadeDuration::new(0.0).is_ok());
        assert!(AnimationCrossFadeDuration::new(AnimationCrossFadeDuration::MAX_SECONDS).is_ok());
        assert_eq!(
            AnimationCrossFadeDuration::new(AnimationCrossFadeDuration::MAX_SECONDS + 0.1),
            Err(AnimationCrossFadeDurationError)
        );
    }

    #[test]
    fn snapshot_blend_uses_trs_morph_and_shortest_quaternion_path() {
        let mut scene = scene_with_two_translation_clips();
        scene.nodes[0].morph_weights = vec![0.0, 1.0];
        let source = snapshot_from_local_transforms(
            &scene,
            vec![trs([0.0, 2.0, 4.0], [0.0, 0.0, 0.0, 1.0], [1.0, 2.0, 3.0])],
            vec![vec![0.0, 1.0]],
        )
        .expect("valid source");
        // Negative representation of a positive 90-degree Y rotation forces
        // shortest-path sign correction before normalized interpolation.
        let target = snapshot_from_local_transforms(
            &scene,
            vec![trs(
                [10.0, 4.0, 8.0],
                [0.0, -DIAGONAL, 0.0, -DIAGONAL],
                [3.0, 4.0, 5.0],
            )],
            vec![vec![1.0, 0.0]],
        )
        .expect("valid target");
        let blended = blend_animation_snapshots(&scene, &source, &target, 0.5)
            .expect("compatible snapshots blend");

        let LocalTransform::Trs {
            translation,
            rotation,
            scale,
        } = blended.local_transforms()[0]
        else {
            panic!("blended node must remain TRS");
        };
        assert_eq!(translation, [5.0, 3.0, 6.0]);
        assert_eq!(scale, [2.0, 3.0, 4.0]);
        assert!(rotation[1] > 0.0 && rotation[3] > 0.0);
        assert!((rotation.iter().map(|value| value * value).sum::<f32>() - 1.0).abs() < 1.0e-6);
        assert_eq!(
            blended.morph_weights(NodeIndex::new(0)),
            Some([0.5, 0.5].as_slice())
        );
    }

    const DIAGONAL: f32 = std::f32::consts::FRAC_1_SQRT_2;

    #[test]
    fn mixer_retargets_from_last_visible_pose_without_backtracking() {
        let scene = scene_with_two_translation_clips();
        let duration = AnimationCrossFadeDuration::new(1.0).expect("valid duration");
        let mut mixer = AnimationCrossFadeMixer::new(&scene, AnimationClipIndex::new(0))
            .expect("valid initial clip");
        assert_eq!(
            mixer
                .transition_to(&scene, AnimationClipIndex::new(1), duration)
                .expect("valid target"),
            AnimationCrossFadeChange::Started
        );
        let halfway = mixer
            .advance_and_snapshot(&scene, 0.5)
            .expect("half transition");
        assert!((translation_x(halfway) - 5.0).abs() < 1.0e-6);

        assert_eq!(
            mixer
                .transition_to(&scene, AnimationClipIndex::new(0), duration)
                .expect("retarget to active clip"),
            AnimationCrossFadeChange::Retargeted
        );
        // The retarget starts exactly at the previously visible x=5 pose and
        // blends toward x=0 instead of jumping back before the next sample.
        assert!(
            (translation_x(mixer.snapshot(&scene).expect("cached source")) - 5.0).abs() < 1.0e-6
        );
        let retargeted = mixer
            .advance_and_snapshot(&scene, 0.5)
            .expect("retarget transition");
        assert!((translation_x(retargeted) - 2.5).abs() < 1.0e-6);
    }

    #[test]
    fn immediate_switch_and_invalid_clip_have_explicit_results() {
        let scene = scene_with_two_translation_clips();
        let mut mixer = AnimationCrossFadeMixer::new(&scene, AnimationClipIndex::new(0))
            .expect("valid initial clip");
        assert_eq!(
            mixer.transition_to(
                &scene,
                AnimationClipIndex::new(9),
                AnimationCrossFadeDuration::IMMEDIATE,
            ),
            Err(AnimationCrossFadeError::InvalidClip(
                AnimationClipIndex::new(9)
            ))
        );
        assert_eq!(
            mixer
                .transition_to(
                    &scene,
                    AnimationClipIndex::new(1),
                    AnimationCrossFadeDuration::IMMEDIATE,
                )
                .expect("immediate valid switch"),
            AnimationCrossFadeChange::CompletedImmediately
        );
        assert_eq!(mixer.active_clip(), AnimationClipIndex::new(1));
        assert_eq!(mixer.target_clip(), None);
        assert!(
            (translation_x(mixer.snapshot(&scene).expect("new target pose")) - 10.0).abs() < 1.0e-6
        );
    }

    #[test]
    fn incompatible_snapshot_shapes_fail_without_partial_pose() {
        let scene = scene_with_two_translation_clips();
        let source =
            sample_animation(&scene, AnimationClipIndex::new(0), 0.0).expect("source sample");
        let mut target = source.clone();
        target.local_transforms.clear();
        assert_eq!(
            blend_animation_snapshots(&scene, &source, &target, 0.5),
            Err(AnimationCrossFadeError::NodeCountMismatch)
        );
        assert_eq!(
            blend_animation_snapshots(&scene, &source, &source, 1.1),
            Err(AnimationCrossFadeError::InvalidBlendFactor)
        );
    }
}
