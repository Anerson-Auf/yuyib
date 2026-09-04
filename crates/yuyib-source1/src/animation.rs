//! Source 1 `StudioModel` skeleton and animation decoding.
//!
//! The binary layouts and sampling rules mirror Valve's `studio.h` and
//! `bone_setup.cpp`: three-weight VVD vertices, bind-pose bones, compressed
//! RLE channels, raw Vector48/Quaternion48/Quaternion64 values, animation
//! sections and demand-loaded `.ani` blocks.

use std::{error::Error, fmt};

use crate::{Source1StudioError, Source1StudioLimits};

const MDL_BONE_BYTES: usize = 216;
const MDL_ANIM_DESC_BYTES: usize = 100;
const MDL_SEQUENCE_DESC_BYTES: usize = 212;
const MDL_INCLUDE_BYTES: usize = 8;
const MDL_ANIM_BLOCK_BYTES: usize = 8;

const ANIM_RAW_POS: u8 = 0x01;
const ANIM_RAW_ROT: u8 = 0x02;
const ANIM_ANIM_POS: u8 = 0x04;
const ANIM_ANIM_ROT: u8 = 0x08;
const ANIM_DELTA: u8 = 0x10;
const ANIM_RAW_ROT2: u8 = 0x20;
const STUDIO_LOOPING: i32 = 0x0001;
const STUDIO_DELTA: i32 = 0x0004;

/// Four GPU joints and normalized weights for one Source VVD vertex.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Source1SkinVertex {
    joints: [u16; 4],
    weights: [f32; 4],
}

impl Source1SkinVertex {
    /// Creates a validated skin binding. Source itself uses at most three joints.
    #[must_use]
    pub const fn new(joints: [u16; 4], weights: [f32; 4]) -> Self {
        Self { joints, weights }
    }

    /// Joint indices in `StudioModel` bone order.
    #[must_use]
    pub const fn joints(self) -> [u16; 4] {
        self.joints
    }

    /// Normalized weights corresponding to [`Self::joints`].
    #[must_use]
    pub const fn weights(self) -> [f32; 4] {
        self.weights
    }
}

/// Local translation and quaternion rotation for one `StudioModel` bone.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Source1BoneTransform {
    /// Translation in Source model space.
    pub translation: [f32; 3],
    /// Unit quaternion in `[x, y, z, w]` order.
    pub rotation: [f32; 4],
}

/// One named `StudioModel` bone and its authored bind transform.
#[derive(Clone, Debug, PartialEq)]
pub struct Source1Bone {
    name: String,
    parent: Option<usize>,
    bind: Source1BoneTransform,
    position_scale: [f32; 3],
    rotation_euler: [f32; 3],
    rotation_scale: [f32; 3],
    alignment: [f32; 4],
    flags: i32,
}

impl Source1Bone {
    /// Bone name used by include-model remapping and diagnostics.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Parent bone, or `None` for a root.
    #[must_use]
    pub const fn parent(&self) -> Option<usize> {
        self.parent
    }

    /// Authored local bind transform.
    #[must_use]
    pub const fn bind_transform(&self) -> Source1BoneTransform {
        self.bind
    }
}

/// Ordered `StudioModel` skeleton and inverse bind matrices.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Source1Skeleton {
    bones: Vec<Source1Bone>,
    inverse_bind_source: Vec<[f32; 16]>,
}

impl Source1Skeleton {
    /// Bones in the palette order used by VVD weights.
    #[must_use]
    pub fn bones(&self) -> &[Source1Bone] {
        &self.bones
    }

    /// Whether the MDL contains a skeleton.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bones.is_empty()
    }

    /// Computes the bind-pose GPU palette in Yuyib coordinates.
    #[must_use]
    pub fn bind_pose(&self) -> Source1Pose {
        build_pose(
            self,
            &self.bones.iter().map(|bone| bone.bind).collect::<Vec<_>>(),
        )
    }
}

/// Fully decoded animation sequence. Frames contain local bone transforms.
#[derive(Clone, Debug, PartialEq)]
pub struct Source1AnimationClip {
    name: String,
    fps: f32,
    looping: bool,
    frames: Vec<Vec<Source1BoneTransform>>,
}

impl Source1AnimationClip {
    /// Sequence label exposed to game code.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Authored samples per second.
    #[must_use]
    pub const fn fps(&self) -> f32 {
        self.fps
    }

    /// Whether the Source sequence carries `STUDIO_LOOPING`.
    #[must_use]
    pub const fn looping(&self) -> bool {
        self.looping
    }

    /// Number of decoded frames.
    #[must_use]
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Playback duration in seconds.
    #[must_use]
    pub fn duration_seconds(&self) -> f32 {
        if self.frames.len() <= 1 || self.fps <= f32::EPSILON {
            0.0
        } else {
            f32::from(u16::try_from(self.frames.len() - 1).unwrap_or(u16::MAX)) / self.fps
        }
    }
}

/// Skeleton, sequences and external dependencies declared by an MDL.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Source1AnimationSet {
    skeleton: Source1Skeleton,
    clips: Vec<Source1AnimationClip>,
    included_models: Vec<String>,
    animation_block_name: Option<String>,
}

impl Source1AnimationSet {
    /// `StudioModel` skeleton.
    #[must_use]
    pub const fn skeleton(&self) -> &Source1Skeleton {
        &self.skeleton
    }

    /// Playable local and merged include-model sequences.
    #[must_use]
    pub fn clips(&self) -> &[Source1AnimationClip] {
        &self.clips
    }

    /// `$includemodel` dependencies in declaration order.
    #[must_use]
    pub fn included_models(&self) -> &[String] {
        &self.included_models
    }

    /// Demand-loaded animation sidecar name, if one was authored.
    #[must_use]
    pub fn animation_block_name(&self) -> Option<&str> {
        self.animation_block_name.as_deref()
    }

    /// Finds a sequence case-insensitively.
    #[must_use]
    pub fn find_clip(&self, name: &str) -> Option<usize> {
        self.clips
            .iter()
            .position(|clip| clip.name.eq_ignore_ascii_case(name))
    }

    /// Appends sequences from a compatible `$includemodel`, remapping bones by name.
    pub(crate) fn merge_included(&mut self, included: &Self) {
        if self.skeleton.is_empty() {
            self.skeleton = included.skeleton.clone();
        }
        let remap = included
            .skeleton
            .bones
            .iter()
            .map(|bone| {
                self.skeleton
                    .bones
                    .iter()
                    .position(|candidate| candidate.name.eq_ignore_ascii_case(&bone.name))
            })
            .collect::<Vec<_>>();
        for clip in &included.clips {
            if self.find_clip(&clip.name).is_some() {
                continue;
            }
            let frames = clip
                .frames
                .iter()
                .map(|source| {
                    let mut target = self
                        .skeleton
                        .bones
                        .iter()
                        .map(|bone| bone.bind)
                        .collect::<Vec<_>>();
                    for (source_index, target_index) in remap.iter().copied().enumerate() {
                        if let (Some(target_index), Some(transform)) =
                            (target_index, source.get(source_index))
                        {
                            target[target_index] = *transform;
                        }
                    }
                    target
                })
                .collect();
            self.clips.push(Source1AnimationClip {
                name: clip.name.clone(),
                fps: clip.fps,
                looping: clip.looping,
                frames,
            });
        }
    }
}

/// GPU-ready skin matrices in Yuyib's right-handed Y-up coordinates.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Source1Pose {
    matrices: Vec<[f32; 16]>,
}

impl Source1Pose {
    /// Joint matrices in `StudioModel` bone order.
    #[must_use]
    pub fn matrices(&self) -> &[[f32; 16]] {
        &self.matrices
    }
}

/// Runtime playback state for one Source sequence.
#[derive(Clone, Debug, PartialEq)]
pub struct Source1AnimationPlayer {
    clip: usize,
    time_seconds: f32,
    speed: f32,
    looping: bool,
    playing: bool,
}

impl Source1AnimationPlayer {
    /// Starts playback of one sequence.
    ///
    /// # Errors
    /// Returns an invalid-clip error when `clip` is absent.
    pub fn new(set: &Source1AnimationSet, clip: usize) -> Result<Self, Source1AnimationError> {
        let sequence = set
            .clips
            .get(clip)
            .ok_or(Source1AnimationError::InvalidClip { clip })?;
        Ok(Self {
            clip,
            time_seconds: 0.0,
            speed: 1.0,
            looping: sequence.looping,
            playing: true,
        })
    }

    /// Selected sequence index.
    #[must_use]
    pub const fn clip(&self) -> usize {
        self.clip
    }

    /// Current sample time.
    #[must_use]
    pub const fn time_seconds(&self) -> f32 {
        self.time_seconds
    }

    /// Selects a sequence and resets playback.
    ///
    /// # Errors
    /// Returns when `clip` is absent from the animation set.
    pub fn select(
        &mut self,
        set: &Source1AnimationSet,
        clip: usize,
    ) -> Result<(), Source1AnimationError> {
        let sequence = set
            .clips
            .get(clip)
            .ok_or(Source1AnimationError::InvalidClip { clip })?;
        self.clip = clip;
        self.time_seconds = 0.0;
        self.looping = sequence.looping;
        self.playing = true;
        Ok(())
    }

    /// Sets a non-negative finite playback multiplier.
    ///
    /// # Errors
    /// Returns when `speed` is negative, NaN or infinite.
    pub fn set_speed(&mut self, speed: f32) -> Result<(), Source1AnimationError> {
        if !speed.is_finite() || speed < 0.0 {
            return Err(Source1AnimationError::InvalidSpeed);
        }
        self.speed = speed;
        Ok(())
    }

    /// Overrides source looping policy.
    pub const fn set_looping(&mut self, looping: bool) {
        self.looping = looping;
    }

    /// Advances playback without sampling.
    ///
    /// # Errors
    /// Returns for an invalid delta or missing selected clip.
    pub fn advance(
        &mut self,
        set: &Source1AnimationSet,
        delta_seconds: f32,
    ) -> Result<(), Source1AnimationError> {
        if !delta_seconds.is_finite() || delta_seconds < 0.0 {
            return Err(Source1AnimationError::InvalidDelta);
        }
        let clip = set
            .clips
            .get(self.clip)
            .ok_or(Source1AnimationError::InvalidClip { clip: self.clip })?;
        if !self.playing {
            return Ok(());
        }
        let duration = clip.duration_seconds();
        if duration <= f32::EPSILON {
            self.time_seconds = 0.0;
        } else {
            self.time_seconds += delta_seconds * self.speed;
            if self.looping {
                self.time_seconds %= duration;
            } else if self.time_seconds >= duration {
                self.time_seconds = duration;
                self.playing = false;
            }
        }
        Ok(())
    }

    /// Samples the current sequence into a GPU skin palette.
    ///
    /// # Errors
    /// Returns when the selected clip is absent.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "time is non-negative and clamped to the bounded frame vector"
    )]
    pub fn sample(&self, set: &Source1AnimationSet) -> Result<Source1Pose, Source1AnimationError> {
        let clip = set
            .clips
            .get(self.clip)
            .ok_or(Source1AnimationError::InvalidClip { clip: self.clip })?;
        if clip.frames.is_empty() {
            return Ok(set.skeleton.bind_pose());
        }
        let frame = self.time_seconds * clip.fps;
        let first = (frame.floor() as usize).min(clip.frames.len() - 1);
        let second = if self.looping {
            (first + 1) % clip.frames.len()
        } else {
            (first + 1).min(clip.frames.len() - 1)
        };
        let fraction = frame.fract();
        let locals = clip.frames[first]
            .iter()
            .copied()
            .zip(clip.frames[second].iter().copied())
            .map(|(a, b)| Source1BoneTransform {
                translation: lerp3(a.translation, b.translation, fraction),
                rotation: slerp(a.rotation, b.rotation, fraction),
            })
            .collect::<Vec<_>>();
        Ok(build_pose(&set.skeleton, &locals))
    }
}

/// Runtime Source animation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Source1AnimationError {
    /// Selected sequence is absent.
    InvalidClip {
        /// Requested zero-based sequence index.
        clip: usize,
    },
    /// Playback speed was negative or non-finite.
    InvalidSpeed,
    /// Frame delta was negative or non-finite.
    InvalidDelta,
}

impl fmt::Display for Source1AnimationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidClip { clip } => {
                write!(formatter, "Source animation clip {clip} is absent")
            }
            Self::InvalidSpeed => {
                formatter.write_str("Source animation speed must be finite and non-negative")
            }
            Self::InvalidDelta => {
                formatter.write_str("Source animation delta must be finite and non-negative")
            }
        }
    }
}

impl Error for Source1AnimationError {}

/// Decodes skeleton and sequences from an MDL, optionally resolving its `.ani` bytes.
///
/// # Errors
/// Returns bounded binary-format failures. External animation blocks remain
/// absent when the MDL declares them but `ani` is `None`.
#[allow(
    clippy::too_many_lines,
    reason = "sequence metadata and bounded frame decoding share one validation boundary"
)]
pub fn decode_studio_animations(
    mdl: &[u8],
    ani: Option<&[u8]>,
    limits: Source1StudioLimits,
) -> Result<Source1AnimationSet, Source1StudioError> {
    let bytes = Bytes::new("MDL", mdl);
    bytes.require(0, 360, "animation header")?;
    let magic = bytes.require(0, 4, "magic")?;
    if magic != b"IDST" {
        let mut actual = [0_u8; 4];
        actual.copy_from_slice(magic);
        return Err(Source1StudioError::InvalidMagic {
            file: "MDL",
            actual,
        });
    }
    let skeleton = decode_skeleton(bytes, limits)?;
    let included_models = decode_includes(bytes, limits)?;
    let animation_block_name = bytes
        .non_negative(348, "animation block name")?
        .checked_sub(0)
        .filter(|offset| *offset != 0)
        .map(|offset| bytes.c_string(offset, limits.max_string_bytes, "animation block name"))
        .transpose()?
        .filter(|name| !name.is_empty());
    let anim_count = bytes.non_negative(180, "local animation count")?;
    let anim_offset = bytes.non_negative(184, "local animation offset")?;
    bytes.array(
        anim_offset,
        anim_count,
        MDL_ANIM_DESC_BYTES,
        "local animations",
        limits.max_animation_clips,
    )?;
    let sequence_count = bytes.non_negative(188, "local sequence count")?;
    let sequence_offset = bytes.non_negative(192, "local sequence offset")?;
    bytes.array(
        sequence_offset,
        sequence_count,
        MDL_SEQUENCE_DESC_BYTES,
        "local sequences",
        limits.max_animation_clips,
    )?;
    let blocks = decode_anim_blocks(bytes, limits)?;
    let mut clips = Vec::with_capacity(sequence_count);
    let mut total_samples = 0_usize;
    for sequence_index in 0..sequence_count {
        let sequence = sequence_offset + sequence_index * MDL_SEQUENCE_DESC_BYTES;
        let label =
            bytes.relative_string(sequence, 4, limits.max_string_bytes, "sequence label")?;
        let flags = bytes.i32(sequence + 12, "sequence flags")?;
        let blend_offset = bytes.relative(sequence, 60, "sequence animation indices")?;
        let animation_index = usize::from(bytes.u16(blend_offset, "sequence animation index")?);
        if animation_index >= anim_count {
            return Err(Source1StudioError::InvalidReference {
                section: "sequence animation",
                index: animation_index,
                available: anim_count,
            });
        }
        let desc = anim_offset + animation_index * MDL_ANIM_DESC_BYTES;
        let fps = bytes.f32(desc + 8, "animation fps")?;
        let anim_flags = bytes.i32(desc + 12, "animation flags")?;
        let frame_count = bytes.non_negative(desc + 16, "animation frame count")?;
        if !fps.is_finite() || fps <= 0.0 {
            return Err(Source1StudioError::InvalidNumber {
                section: "animation fps",
                offset: desc + 8,
            });
        }
        if frame_count > limits.max_animation_frames {
            return Err(Source1StudioError::RecordLimit {
                section: "animation frames",
                actual: frame_count,
                limit: limits.max_animation_frames,
            });
        }
        total_samples =
            total_samples.saturating_add(frame_count.saturating_mul(skeleton.bones.len()));
        if total_samples > limits.max_animation_samples {
            return Err(Source1StudioError::RecordLimit {
                section: "animation bone samples",
                actual: total_samples,
                limit: limits.max_animation_samples,
            });
        }
        let mut frames = Vec::with_capacity(frame_count);
        for frame in 0..frame_count {
            frames.push(decode_frame(
                bytes,
                ani.map(|value| Bytes::new("ANI", value)),
                &blocks,
                desc,
                anim_flags,
                frame,
                &skeleton,
            )?);
        }
        clips.push(Source1AnimationClip {
            name: label,
            fps,
            looping: flags & STUDIO_LOOPING != 0,
            frames,
        });
    }
    Ok(Source1AnimationSet {
        skeleton,
        clips,
        included_models,
        animation_block_name,
    })
}

fn decode_skeleton(
    bytes: Bytes<'_>,
    limits: Source1StudioLimits,
) -> Result<Source1Skeleton, Source1StudioError> {
    let count = bytes.non_negative(156, "bone count")?;
    let offset = bytes.non_negative(160, "bone offset")?;
    bytes.array(offset, count, MDL_BONE_BYTES, "bones", limits.max_bones)?;
    let mut bones = Vec::with_capacity(count);
    for index in 0..count {
        let record = offset + index * MDL_BONE_BYTES;
        let parent_raw = bytes.i32(record + 4, "bone parent")?;
        let parent = if parent_raw < 0 {
            None
        } else {
            let parent = usize::try_from(parent_raw).expect("non-negative parent");
            if parent >= index {
                return Err(Source1StudioError::InvalidReference {
                    section: "bone parent",
                    index: parent,
                    available: index,
                });
            }
            Some(parent)
        };
        let bind = Source1BoneTransform {
            translation: bytes.vec3(record + 32, "bone position")?,
            rotation: normalize_quat(bytes.vec4(record + 44, "bone quaternion")?),
        };
        bones.push(Source1Bone {
            name: bytes.relative_string(record, 0, limits.max_string_bytes, "bone name")?,
            parent,
            bind,
            rotation_euler: bytes.vec3(record + 60, "bone rotation")?,
            position_scale: bytes.vec3(record + 72, "bone position scale")?,
            rotation_scale: bytes.vec3(record + 84, "bone rotation scale")?,
            alignment: normalize_quat(bytes.vec4(record + 144, "bone alignment")?),
            flags: bytes.i32(record + 160, "bone flags")?,
        });
    }
    let globals = global_matrices(
        &bones,
        &bones.iter().map(|bone| bone.bind).collect::<Vec<_>>(),
    );
    let inverse_bind_source = globals.into_iter().map(invert_rigid).collect();
    Ok(Source1Skeleton {
        bones,
        inverse_bind_source,
    })
}

fn decode_includes(
    bytes: Bytes<'_>,
    limits: Source1StudioLimits,
) -> Result<Vec<String>, Source1StudioError> {
    let count = bytes.non_negative(336, "include model count")?;
    let offset = bytes.non_negative(340, "include model offset")?;
    bytes.array(
        offset,
        count,
        MDL_INCLUDE_BYTES,
        "include models",
        limits.max_included_models,
    )?;
    (0..count)
        .map(|index| {
            bytes.relative_string(
                offset + index * MDL_INCLUDE_BYTES,
                4,
                limits.max_string_bytes,
                "include model name",
            )
        })
        .collect()
}

fn decode_anim_blocks(
    bytes: Bytes<'_>,
    limits: Source1StudioLimits,
) -> Result<Vec<(usize, usize)>, Source1StudioError> {
    let count = bytes.non_negative(352, "animation block count")?;
    let offset = bytes.non_negative(356, "animation block offset")?;
    bytes.array(
        offset,
        count,
        MDL_ANIM_BLOCK_BYTES,
        "animation blocks",
        limits.max_animation_blocks,
    )?;
    (0..count)
        .map(|index| {
            let record = offset + index * MDL_ANIM_BLOCK_BYTES;
            Ok((
                bytes.non_negative(record, "animation block start")?,
                bytes.non_negative(record + 4, "animation block end")?,
            ))
        })
        .collect()
}

fn decode_frame(
    mdl: Bytes<'_>,
    ani: Option<Bytes<'_>>,
    blocks: &[(usize, usize)],
    desc: usize,
    desc_flags: i32,
    frame: usize,
    skeleton: &Source1Skeleton,
) -> Result<Vec<Source1BoneTransform>, Source1StudioError> {
    let (block, animation_index, local_frame) = animation_location(mdl, desc, frame)?;
    let mut locals = skeleton
        .bones
        .iter()
        .map(|bone| {
            if desc_flags & STUDIO_DELTA != 0 {
                Source1BoneTransform {
                    translation: [0.0; 3],
                    rotation: [0.0, 0.0, 0.0, 1.0],
                }
            } else {
                bone.bind
            }
        })
        .collect::<Vec<_>>();
    let (data, mut record) = if block == 0 {
        (
            mdl,
            desc.checked_add(animation_index)
                .ok_or(Source1StudioError::InvalidRange {
                    file: "MDL",
                    section: "local animation",
                    offset: desc,
                    length: animation_index,
                    available: mdl.bytes.len(),
                })?,
        )
    } else {
        let data = ani.ok_or(Source1StudioError::MissingAnimationBlock { block })?;
        let (start, end) =
            blocks
                .get(block)
                .copied()
                .ok_or(Source1StudioError::InvalidReference {
                    section: "animation block",
                    index: block,
                    available: blocks.len(),
                })?;
        data.require(start, end.saturating_sub(start), "animation block")?;
        (
            data,
            start
                .checked_add(animation_index)
                .ok_or(Source1StudioError::InvalidRange {
                    file: "ANI",
                    section: "animation block index",
                    offset: start,
                    length: animation_index,
                    available: data.bytes.len(),
                })?,
        )
    };
    if animation_index == 0 {
        return Ok(locals);
    }
    let mut visited = 0_usize;
    loop {
        data.require(record, 4, "bone animation record")?;
        let bone_index = usize::from(data.u8(record, "animated bone")?);
        if bone_index == 255 {
            break;
        }
        let bone = skeleton
            .bones
            .get(bone_index)
            .ok_or(Source1StudioError::InvalidReference {
                section: "animated bone",
                index: bone_index,
                available: skeleton.bones.len(),
            })?;
        let flags = data.u8(record + 1, "bone animation flags")?;
        locals[bone_index] = decode_bone_transform(data, record, flags, local_frame, bone)?;
        let next = usize::from(data.u16(record + 2, "next bone animation")?);
        if next == 0 {
            break;
        }
        record = record
            .checked_add(next)
            .ok_or(Source1StudioError::InvalidRange {
                file: data.kind,
                section: "next bone animation",
                offset: record,
                length: next,
                available: data.bytes.len(),
            })?;
        visited += 1;
        if visited > skeleton.bones.len() {
            return Err(Source1StudioError::RecordLimit {
                section: "bone animation chain",
                actual: visited,
                limit: skeleton.bones.len(),
            });
        }
    }
    Ok(locals)
}

fn animation_location(
    mdl: Bytes<'_>,
    desc: usize,
    frame: usize,
) -> Result<(usize, usize, usize), Source1StudioError> {
    let mut block = mdl.i32(desc + 52, "animation block")?;
    let mut index = mdl.non_negative(desc + 56, "animation index")?;
    let mut local_frame = frame;
    let frame_count = mdl.non_negative(desc + 16, "animation frame count")?;
    let section_offset = mdl.non_negative(desc + 80, "animation section offset")?;
    let section_frames = mdl.non_negative(desc + 84, "animation section frames")?;
    if section_frames != 0 {
        let section = if frame_count > section_frames && frame == frame_count.saturating_sub(1) {
            local_frame = 0;
            frame_count / section_frames + 1
        } else {
            let section = frame / section_frames;
            local_frame -= section * section_frames;
            section
        };
        let record = desc
            .checked_add(section_offset)
            .and_then(|offset| offset.checked_add(section * 8))
            .ok_or(Source1StudioError::InvalidRange {
                file: "MDL",
                section: "animation section",
                offset: desc,
                length: section_offset,
                available: mdl.bytes.len(),
            })?;
        block = mdl.i32(record, "animation section block")?;
        index = mdl.non_negative(record + 4, "animation section index")?;
    }
    if block < 0 {
        return Err(Source1StudioError::MissingAnimationBlock { block: usize::MAX });
    }
    Ok((
        usize::try_from(block).expect("non-negative block"),
        index,
        local_frame,
    ))
}

fn decode_bone_transform(
    data: Bytes<'_>,
    record: usize,
    flags: u8,
    frame: usize,
    bone: &Source1Bone,
) -> Result<Source1BoneTransform, Source1StudioError> {
    let payload = record + 4;
    let rotation = if flags & ANIM_RAW_ROT != 0 {
        decode_quaternion48(data.require(payload, 6, "raw Quaternion48")?)
    } else if flags & ANIM_RAW_ROT2 != 0 {
        decode_quaternion64(data.require(payload, 8, "raw Quaternion64")?)
    } else if flags & ANIM_ANIM_ROT != 0 {
        let angles = sample_value_ptr(data, payload, frame, bone.rotation_scale)?;
        let angles = if flags & ANIM_DELTA == 0 {
            add3(angles, bone.rotation_euler)
        } else {
            angles
        };
        let mut value = euler_quaternion(angles);
        if flags & ANIM_DELTA == 0 && bone.flags & 0x0010_0000 != 0 {
            value = align_quaternion(bone.alignment, value);
        }
        value
    } else if flags & ANIM_DELTA != 0 {
        [0.0, 0.0, 0.0, 1.0]
    } else {
        bone.bind.rotation
    };
    let raw_rotation_bytes = if flags & ANIM_RAW_ROT != 0 {
        6
    } else if flags & ANIM_RAW_ROT2 != 0 {
        8
    } else {
        0
    };
    let translation = if flags & ANIM_RAW_POS != 0 {
        decode_vector48(data.require(payload + raw_rotation_bytes, 6, "raw Vector48")?)
    } else if flags & ANIM_ANIM_POS != 0 {
        let pointer = payload + usize::from(flags & ANIM_ANIM_ROT != 0) * 6;
        let value = sample_value_ptr(data, pointer, frame, bone.position_scale)?;
        if flags & ANIM_DELTA == 0 {
            add3(value, bone.bind.translation)
        } else {
            value
        }
    } else if flags & ANIM_DELTA != 0 {
        [0.0; 3]
    } else {
        bone.bind.translation
    };
    Ok(Source1BoneTransform {
        translation,
        rotation: normalize_quat(rotation),
    })
}

fn sample_value_ptr(
    data: Bytes<'_>,
    pointer: usize,
    frame: usize,
    scale: [f32; 3],
) -> Result<[f32; 3], Source1StudioError> {
    data.require(pointer, 6, "animation value pointers")?;
    let mut output = [0.0; 3];
    for axis in 0..3 {
        let offset = usize::from(data.u16(pointer + axis * 2, "animation value pointer")?);
        output[axis] = if offset == 0 {
            0.0
        } else {
            f32::from(sample_rle(data, pointer + offset, frame)?) * scale[axis]
        };
    }
    Ok(output)
}

fn sample_rle(
    data: Bytes<'_>,
    mut offset: usize,
    mut frame: usize,
) -> Result<i16, Source1StudioError> {
    loop {
        let header = data.require(offset, 2, "animation RLE header")?;
        let valid = usize::from(header[0]);
        let total = usize::from(header[1]);
        if total == 0 || valid > total {
            return Err(Source1StudioError::InvalidAnimationRle { offset });
        }
        data.require(offset + 2, valid * 2, "animation RLE values")?;
        if frame < total {
            let sample = if frame < valid {
                frame
            } else {
                valid.saturating_sub(1)
            };
            if valid == 0 {
                return Ok(0);
            }
            return data.i16(offset + 2 + sample * 2, "animation RLE value");
        }
        frame -= total;
        offset += 2 + valid * 2;
    }
}

#[derive(Clone, Copy)]
struct Bytes<'a> {
    kind: &'static str,
    bytes: &'a [u8],
}

impl<'a> Bytes<'a> {
    const fn new(kind: &'static str, bytes: &'a [u8]) -> Self {
        Self { kind, bytes }
    }

    fn require(
        self,
        offset: usize,
        length: usize,
        section: &'static str,
    ) -> Result<&'a [u8], Source1StudioError> {
        self.bytes.get(offset..offset.saturating_add(length)).ok_or(
            Source1StudioError::InvalidRange {
                file: self.kind,
                section,
                offset,
                length,
                available: self.bytes.len(),
            },
        )
    }

    fn array(
        self,
        offset: usize,
        count: usize,
        stride: usize,
        section: &'static str,
        limit: usize,
    ) -> Result<(), Source1StudioError> {
        if count > limit {
            return Err(Source1StudioError::RecordLimit {
                section,
                actual: count,
                limit,
            });
        }
        self.require(
            offset,
            count
                .checked_mul(stride)
                .ok_or(Source1StudioError::RecordLimit {
                    section,
                    actual: count,
                    limit,
                })?,
            section,
        )?;
        Ok(())
    }

    fn u8(self, offset: usize, section: &'static str) -> Result<u8, Source1StudioError> {
        Ok(self.require(offset, 1, section)?[0])
    }

    fn u16(self, offset: usize, section: &'static str) -> Result<u16, Source1StudioError> {
        Ok(u16::from_le_bytes(
            self.require(offset, 2, section)?.try_into().expect("u16"),
        ))
    }

    fn i16(self, offset: usize, section: &'static str) -> Result<i16, Source1StudioError> {
        Ok(i16::from_le_bytes(
            self.require(offset, 2, section)?.try_into().expect("i16"),
        ))
    }

    fn i32(self, offset: usize, section: &'static str) -> Result<i32, Source1StudioError> {
        Ok(i32::from_le_bytes(
            self.require(offset, 4, section)?.try_into().expect("i32"),
        ))
    }

    fn f32(self, offset: usize, section: &'static str) -> Result<f32, Source1StudioError> {
        let value = f32::from_le_bytes(self.require(offset, 4, section)?.try_into().expect("f32"));
        if value.is_finite() {
            Ok(value)
        } else {
            Err(Source1StudioError::InvalidNumber { section, offset })
        }
    }

    fn vec3(self, offset: usize, section: &'static str) -> Result<[f32; 3], Source1StudioError> {
        Ok([
            self.f32(offset, section)?,
            self.f32(offset + 4, section)?,
            self.f32(offset + 8, section)?,
        ])
    }

    fn vec4(self, offset: usize, section: &'static str) -> Result<[f32; 4], Source1StudioError> {
        Ok([
            self.f32(offset, section)?,
            self.f32(offset + 4, section)?,
            self.f32(offset + 8, section)?,
            self.f32(offset + 12, section)?,
        ])
    }

    fn non_negative(self, offset: usize, field: &'static str) -> Result<usize, Source1StudioError> {
        let value = self.i32(offset, field)?;
        usize::try_from(value).map_err(|_| Source1StudioError::NegativeField {
            file: self.kind,
            field,
            value,
        })
    }

    fn relative(
        self,
        base: usize,
        field_offset: usize,
        field: &'static str,
    ) -> Result<usize, Source1StudioError> {
        base.checked_add(self.non_negative(base + field_offset, field)?)
            .ok_or(Source1StudioError::InvalidRange {
                file: self.kind,
                section: field,
                offset: base,
                length: usize::MAX,
                available: self.bytes.len(),
            })
    }

    fn relative_string(
        self,
        base: usize,
        field_offset: usize,
        max: usize,
        section: &'static str,
    ) -> Result<String, Source1StudioError> {
        self.c_string(self.relative(base, field_offset, section)?, max, section)
    }

    fn c_string(
        self,
        offset: usize,
        max: usize,
        section: &'static str,
    ) -> Result<String, Source1StudioError> {
        let available = self
            .bytes
            .get(offset..)
            .ok_or(Source1StudioError::InvalidString { section, offset })?;
        let bounded = &available[..available.len().min(max)];
        let end = bounded
            .iter()
            .position(|byte| *byte == 0)
            .ok_or(Source1StudioError::InvalidString { section, offset })?;
        std::str::from_utf8(&bounded[..end])
            .map(str::to_owned)
            .map_err(|_| Source1StudioError::InvalidString { section, offset })
    }
}

fn decode_quaternion48(bytes: &[u8]) -> [f32; 4] {
    let x = f32::from(u16::from_le_bytes([bytes[0], bytes[1]]));
    let y = f32::from(u16::from_le_bytes([bytes[2], bytes[3]]));
    let packed = u16::from_le_bytes([bytes[4], bytes[5]]);
    let z = f32::from(packed & 0x7fff);
    let mut q = [
        (x - 32768.0) / 32768.0,
        (y - 32768.0) / 32768.0,
        (z - 16384.0) / 16384.0,
        0.0,
    ];
    q[3] = (1.0 - q[0] * q[0] - q[1] * q[1] - q[2] * q[2])
        .max(0.0)
        .sqrt();
    if packed & 0x8000 != 0 {
        q[3] = -q[3];
    }
    normalize_quat(q)
}

#[allow(
    clippy::cast_precision_loss,
    reason = "21-bit packed components are exactly representable by f32"
)]
fn decode_quaternion64(bytes: &[u8]) -> [f32; 4] {
    let packed = u64::from_le_bytes(bytes.try_into().expect("eight bytes"));
    let x = (packed & 0x1f_ffff) as f32;
    let y = ((packed >> 21) & 0x1f_ffff) as f32;
    let z = ((packed >> 42) & 0x1f_ffff) as f32;
    let mut q = [
        (x - 1_048_576.0) / 1_048_576.5,
        (y - 1_048_576.0) / 1_048_576.5,
        (z - 1_048_576.0) / 1_048_576.5,
        0.0,
    ];
    q[3] = (1.0 - q[0] * q[0] - q[1] * q[1] - q[2] * q[2])
        .max(0.0)
        .sqrt();
    if packed >> 63 != 0 {
        q[3] = -q[3];
    }
    normalize_quat(q)
}

fn decode_vector48(bytes: &[u8]) -> [f32; 3] {
    [
        half_to_f32(u16::from_le_bytes([bytes[0], bytes[1]])),
        half_to_f32(u16::from_le_bytes([bytes[2], bytes[3]])),
        half_to_f32(u16::from_le_bytes([bytes[4], bytes[5]])),
    ]
}

fn half_to_f32(value: u16) -> f32 {
    let sign = u32::from(value >> 15) << 31;
    let exponent = u32::from((value >> 10) & 0x1f);
    let mantissa = u32::from(value & 0x03ff);
    let bits = if exponent == 0 {
        if mantissa == 0 {
            sign
        } else {
            let mut mantissa = mantissa;
            let mut exponent = 113_u32;
            while mantissa & 0x0400 == 0 {
                mantissa <<= 1;
                exponent -= 1;
            }
            sign | (exponent << 23) | ((mantissa & 0x03ff) << 13)
        }
    } else if exponent == 0x1f {
        sign | 0x7f7f_ffff
    } else {
        sign | ((exponent + 112) << 23) | (mantissa << 13)
    };
    f32::from_bits(bits)
}

fn euler_quaternion([x, y, z]: [f32; 3]) -> [f32; 4] {
    let (sx, cx) = (x * 0.5).sin_cos();
    let (sy, cy) = (y * 0.5).sin_cos();
    let (sz, cz) = (z * 0.5).sin_cos();
    normalize_quat([
        sx * cy * cz - cx * sy * sz,
        cx * sy * cz + sx * cy * sz,
        cx * cy * sz - sx * sy * cz,
        cx * cy * cz + sx * sy * sz,
    ])
}

fn align_quaternion(reference: [f32; 4], mut value: [f32; 4]) -> [f32; 4] {
    if dot4(reference, value) < 0.0 {
        value = value.map(|component| -component);
    }
    value
}

fn normalize_quat(value: [f32; 4]) -> [f32; 4] {
    let length = dot4(value, value).sqrt();
    if length > f32::EPSILON && length.is_finite() {
        value.map(|component| component / length)
    } else {
        [0.0, 0.0, 0.0, 1.0]
    }
}

fn slerp(mut a: [f32; 4], mut b: [f32; 4], t: f32) -> [f32; 4] {
    a = normalize_quat(a);
    b = normalize_quat(b);
    let mut cosine = dot4(a, b);
    if cosine < 0.0 {
        b = b.map(|value| -value);
        cosine = -cosine;
    }
    if cosine > 0.9995 {
        return normalize_quat([
            a[0] + (b[0] - a[0]) * t,
            a[1] + (b[1] - a[1]) * t,
            a[2] + (b[2] - a[2]) * t,
            a[3] + (b[3] - a[3]) * t,
        ]);
    }
    let angle = cosine.clamp(-1.0, 1.0).acos();
    let sine = angle.sin();
    let left = ((1.0 - t) * angle).sin() / sine;
    let right = (t * angle).sin() / sine;
    normalize_quat([
        a[0] * left + b[0] * right,
        a[1] * left + b[1] * right,
        a[2] * left + b[2] * right,
        a[3] * left + b[3] * right,
    ])
}

fn build_pose(skeleton: &Source1Skeleton, locals: &[Source1BoneTransform]) -> Source1Pose {
    let globals = global_matrices(&skeleton.bones, locals);
    let c = [
        1.0, 0.0, 0.0, 0.0, 0.0, 0.0, -1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ];
    let ci = [
        1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ];
    Source1Pose {
        matrices: globals
            .into_iter()
            .zip(skeleton.inverse_bind_source.iter().copied())
            .map(|(global, inverse)| mat_mul(mat_mul(mat_mul(c, global), inverse), ci))
            .collect(),
    }
}

fn global_matrices(bones: &[Source1Bone], locals: &[Source1BoneTransform]) -> Vec<[f32; 16]> {
    let mut output = Vec::with_capacity(bones.len());
    for (index, bone) in bones.iter().enumerate() {
        let local = locals.get(index).copied().unwrap_or(bone.bind);
        let local = transform_matrix(local);
        output.push(
            bone.parent
                .map_or(local, |parent| mat_mul(output[parent], local)),
        );
    }
    output
}

fn transform_matrix(transform: Source1BoneTransform) -> [f32; 16] {
    let [x, y, z, w] = normalize_quat(transform.rotation);
    let [tx, ty, tz] = transform.translation;
    [
        1.0 - 2.0 * (y * y + z * z),
        2.0 * (x * y + z * w),
        2.0 * (x * z - y * w),
        0.0,
        2.0 * (x * y - z * w),
        1.0 - 2.0 * (x * x + z * z),
        2.0 * (y * z + x * w),
        0.0,
        2.0 * (x * z + y * w),
        2.0 * (y * z - x * w),
        1.0 - 2.0 * (x * x + y * y),
        0.0,
        tx,
        ty,
        tz,
        1.0,
    ]
}

fn invert_rigid(matrix: [f32; 16]) -> [f32; 16] {
    let r = [
        matrix[0], matrix[4], matrix[8], 0.0, matrix[1], matrix[5], matrix[9], 0.0, matrix[2],
        matrix[6], matrix[10], 0.0, 0.0, 0.0, 0.0, 1.0,
    ];
    let t = [matrix[12], matrix[13], matrix[14]];
    let translated = [
        -(r[0] * t[0] + r[4] * t[1] + r[8] * t[2]),
        -(r[1] * t[0] + r[5] * t[1] + r[9] * t[2]),
        -(r[2] * t[0] + r[6] * t[1] + r[10] * t[2]),
    ];
    [
        r[0],
        r[1],
        r[2],
        0.0,
        r[4],
        r[5],
        r[6],
        0.0,
        r[8],
        r[9],
        r[10],
        0.0,
        translated[0],
        translated[1],
        translated[2],
        1.0,
    ]
}

fn mat_mul(a: [f32; 16], b: [f32; 16]) -> [f32; 16] {
    let mut out = [0.0; 16];
    for column in 0..4 {
        for row in 0..4 {
            out[column * 4 + row] = (0..4).map(|k| a[k * 4 + row] * b[column * 4 + k]).sum();
        }
    }
    out
}

fn dot4(a: [f32; 4], b: [f32; 4]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3]
}
fn add3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compressed_quaternions_and_half_vectors_decode() {
        let identity48 = [0x00, 0x80, 0x00, 0x80, 0x00, 0x40];
        assert_eq!(decode_quaternion48(&identity48), [0.0, 0.0, 0.0, 1.0]);
        let vector = decode_vector48(&[0x00, 0x3c, 0x00, 0xc0, 0x00, 0x38]);
        assert_eq!(vector, [1.0, -2.0, 0.5]);
    }

    #[test]
    fn bind_palette_is_identity_after_coordinate_conversion() {
        let bone = Source1Bone {
            name: "root".to_owned(),
            parent: None,
            bind: Source1BoneTransform {
                translation: [10.0, 20.0, 30.0],
                rotation: [0.0, 0.0, 0.0, 1.0],
            },
            position_scale: [1.0; 3],
            rotation_euler: [0.0; 3],
            rotation_scale: [1.0; 3],
            alignment: [0.0, 0.0, 0.0, 1.0],
            flags: 0,
        };
        let bind = transform_matrix(bone.bind);
        let skeleton = Source1Skeleton {
            bones: vec![bone],
            inverse_bind_source: vec![invert_rigid(bind)],
        };
        let pose = skeleton.bind_pose();
        let identity = [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ];
        for (actual, expected) in pose.matrices()[0].iter().zip(identity) {
            assert!((actual - expected).abs() < 1.0e-5);
        }
    }

    #[test]
    #[ignore = "set YUYIB_TEST_MDL to inspect a real StudioModel"]
    fn decodes_real_mdl_from_environment() {
        let path = std::env::var_os("YUYIB_TEST_MDL").expect("YUYIB_TEST_MDL");
        let mdl = std::fs::read(path).expect("read MDL");
        let set = decode_studio_animations(&mdl, None, Source1StudioLimits::default())
            .expect("decode skeleton and embedded animations");
        assert!(!set.skeleton().bones().is_empty());
        assert!(!set.clips().is_empty());
        for clip in 0..set.clips().len() {
            let player = Source1AnimationPlayer::new(&set, clip).expect("player");
            let pose = player.sample(&set).expect("sample");
            assert_eq!(pose.matrices().len(), set.skeleton().bones().len());
            assert!(
                pose.matrices()
                    .iter()
                    .flatten()
                    .all(|value| value.is_finite())
            );
        }
    }
}
