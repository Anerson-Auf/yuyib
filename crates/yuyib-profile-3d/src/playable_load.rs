//! Co-load of map [`super::Game3dProfile`] + [`super::AnimatedCharacterLoad3d`].
//!
//! Collapses the dual-queue loading screen glue used by playable hosts.

use std::{path::PathBuf, sync::Arc};

use yuyib_tasks::TaskPool;

use super::{
    AnimatedCharacter3d, AnimatedCharacterError, AnimatedCharacterLoad3d, AnimatedCharacterStatus,
    EnvironmentPresetError, Game3dProfile, Game3dProfileConfig, Game3dProfileError,
    Game3dProfileStatus,
};

/// Status of a map + character co-load.
#[derive(Clone, Debug)]
pub enum Game3dPlayableLoadStatus {
    /// Still importing map and/or character.
    Loading {
        /// Combined approximate progress in `0.0..=1.0`.
        progress: f32,
    },
    /// Both assets are CPU-ready via [`Game3dPlayableLoad::take_ready`].
    Ready,
    /// Map or character load failed.
    Failed {
        /// Human-readable failure.
        message: String,
    },
}

/// Owns one shared-pool map profile load and one animated-character load.
pub struct Game3dPlayableLoad {
    profile: Option<Game3dProfile>,
    character: AnimatedCharacterLoad3d,
}

impl Game3dPlayableLoad {
    /// Starts map + character loads on `pool`.
    ///
    /// `profile_config` should already include load policy and optional
    /// [`super::EnvironmentPreset`].
    ///
    /// # Errors
    ///
    /// Returns profile construction, map start, or character queue failures.
    pub fn start(
        pool: Arc<TaskPool>,
        profile_config: Game3dProfileConfig,
        map_path: impl AsRef<std::path::Path>,
        character_path: impl Into<PathBuf>,
        character_clip: usize,
    ) -> Result<Self, Game3dPlayableLoadError> {
        let asset_root = profile_config.asset_root.clone();
        let mut profile = Game3dProfile::with_shared_pool(Arc::clone(&pool), profile_config)
            .map_err(Game3dPlayableLoadError::Profile)?;
        profile
            .start_gltf(map_path)
            .map_err(Game3dPlayableLoadError::Profile)?;
        let character = AnimatedCharacterLoad3d::start_on(
            &pool,
            character_path,
            asset_root,
            character_clip,
        )
        .map_err(Game3dPlayableLoadError::Character)?;
        Ok(Self {
            profile: Some(profile),
            character,
        })
    }

    /// Polls both loads.
    pub fn poll(&mut self) -> Game3dPlayableLoadStatus {
        let Some(profile) = self.profile.as_mut() else {
            return Game3dPlayableLoadStatus::Failed {
                message: "playable load already taken".to_owned(),
            };
        };
        match profile.poll() {
            Game3dProfileStatus::Failed { message } => {
                return Game3dPlayableLoadStatus::Failed { message };
            }
            Game3dProfileStatus::Ready
            | Game3dProfileStatus::Idle
            | Game3dProfileStatus::Loading { .. } => {}
        }
        match self.character.poll() {
            AnimatedCharacterStatus::Failed { message } => {
                return Game3dPlayableLoadStatus::Failed { message };
            }
            AnimatedCharacterStatus::Ready
            | AnimatedCharacterStatus::Idle
            | AnimatedCharacterStatus::Loading { .. } => {}
        }
        if profile.loaded().is_some()
            && matches!(self.character.poll(), AnimatedCharacterStatus::Ready)
        {
            return Game3dPlayableLoadStatus::Ready;
        }
        Game3dPlayableLoadStatus::Loading {
            progress: self.progress_fraction(),
        }
    }

    /// Combined loading-screen progress.
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        reason = "loading UI is approximate while exact counters remain authoritative"
    )]
    pub fn progress_fraction(&self) -> f32 {
        let Some(profile) = self.profile.as_ref() else {
            return 0.99;
        };
        let map = profile.load_progress();
        let map_fraction = if map.total_work == 0 {
            0.03
        } else {
            (map.completed_work.min(map.total_work) as f32 / map.total_work as f32).clamp(0.03, 0.99)
        };
        f32::midpoint(map_fraction, self.character.progress_fraction())
    }

    /// Takes the ready profile + character presenter.
    ///
    /// # Errors
    ///
    /// Returns when either side is not ready.
    pub fn take_ready(&mut self) -> Result<(Game3dProfile, AnimatedCharacter3d), Game3dPlayableLoadError> {
        let _ = self.poll();
        if !matches!(self.poll(), Game3dPlayableLoadStatus::Ready) {
            return Err(Game3dPlayableLoadError::NotReady);
        }
        let profile = self
            .profile
            .take()
            .ok_or(Game3dPlayableLoadError::AlreadyTaken)?;
        let character = self
            .character
            .take_ready()
            .map_err(Game3dPlayableLoadError::Character)?;
        Ok((profile, character))
    }
}

/// Failure while co-loading a playable map + character.
#[derive(Debug)]
pub enum Game3dPlayableLoadError {
    /// Profile construction or map load failed.
    Profile(Game3dProfileError),
    /// Character load failed.
    Character(AnimatedCharacterError),
    /// Environment construction failed (reserved for future folding).
    Environment(EnvironmentPresetError),
    /// Co-load is not ready yet.
    NotReady,
    /// Profile was already taken.
    AlreadyTaken,
}

impl std::fmt::Display for Game3dPlayableLoadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Profile(error) => write!(formatter, "playable co-load profile: {error}"),
            Self::Character(error) => write!(formatter, "playable co-load character: {error}"),
            Self::Environment(error) => write!(formatter, "playable co-load environment: {error}"),
            Self::NotReady => formatter.write_str("playable co-load is not ready"),
            Self::AlreadyTaken => formatter.write_str("playable co-load already taken"),
        }
    }
}

impl std::error::Error for Game3dPlayableLoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Profile(error) => Some(error),
            Self::Character(error) => Some(error),
            Self::Environment(error) => Some(error),
            Self::NotReady | Self::AlreadyTaken => None,
        }
    }
}
