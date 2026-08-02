//! M5 high-level 2D composition profile.
//!
//! [`Game2dProfile`] owns the repeated playground wiring: one ECS [`World`] and
//! one [`Game2dScene`]. [`PlayableLoop2d`] adds top-down input → kinematic step
//! → animation → camera follow → render. Rapier platformer remains a separate
//! opt-in path (not folded into this profile yet).

#![forbid(unsafe_code)]

mod camera_follow;
mod playable_loop;

pub use camera_follow::CameraFollow2d;
pub use playable_loop::{PlayableLoop2d, PlayableLoopDesc2d, PlayableLoopError2d};

use std::{error::Error, fmt, time::Duration};

use yuyib_2d::TextureHandle;
use yuyib_ecs::prelude::World;
use yuyib_game_2d::{
    Game2dScene, Game2dSceneConfig, Game2dSceneError, Game2dSceneStats, TextureQueueError2d,
    step_sprite_animations_2d,
};
use yuyib_image::DecodedImage;
use yuyib_render::RenderFrame;

/// Owns a 2D ECS world and high-level scene renderer.
pub struct Game2dProfile {
    world: World,
    scene: Game2dScene,
}

impl Game2dProfile {
    /// Creates a profile with an empty world and the supplied scene policy.
    #[must_use]
    pub fn new(config: Game2dSceneConfig) -> Self {
        Self {
            world: World::new(),
            scene: Game2dScene::new(config),
        }
    }

    /// Returns the ECS world.
    #[must_use]
    pub const fn world(&self) -> &World {
        &self.world
    }

    /// Returns a mutable ECS world escape hatch.
    pub const fn world_mut(&mut self) -> &mut World {
        &mut self.world
    }

    /// Returns the 2D scene.
    #[must_use]
    pub const fn scene(&self) -> &Game2dScene {
        &self.scene
    }

    /// Returns a mutable 2D scene escape hatch.
    pub const fn scene_mut(&mut self) -> &mut Game2dScene {
        &mut self.scene
    }

    /// Queues a decoded texture for GPU publication on later renders.
    ///
    /// # Errors
    ///
    /// Forwards [`Game2dScene::queue_texture`] failures.
    pub fn queue_texture(
        &mut self,
        handle: TextureHandle,
        image: DecodedImage,
    ) -> Result<(), Game2dProfileError> {
        self.scene
            .queue_texture(handle, image)
            .map_err(Game2dProfileError::TextureQueue)
    }

    /// Advances sprite animations in the owned world by `delta`.
    pub fn step_animations(&mut self, delta: Duration) {
        step_sprite_animations_2d(&mut self.world, delta);
    }

    /// Renders the owned world through [`Game2dScene`].
    ///
    /// # Errors
    ///
    /// Forwards [`Game2dScene::render`] failures.
    pub fn render(
        &mut self,
        frame: &mut RenderFrame<'_>,
    ) -> Result<Game2dSceneStats, Game2dProfileError> {
        self.scene
            .render(frame, &mut self.world)
            .map_err(Game2dProfileError::Render)
    }
}

/// Failure while driving a 2D profile.
#[derive(Debug)]
pub enum Game2dProfileError {
    /// Texture queue rejected the upload.
    TextureQueue(TextureQueueError2d),
    /// Scene render failed.
    Render(Game2dSceneError),
}

impl fmt::Display for Game2dProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TextureQueue(error) => write!(f, "2d profile texture queue: {error}"),
            Self::Render(error) => write!(f, "2d profile render: {error}"),
        }
    }
}

impl Error for Game2dProfileError {}

#[cfg(test)]
mod tests {
    use super::Game2dProfile;
    use yuyib_game_2d::Game2dSceneConfig;

    #[test]
    fn profile_constructs() {
        let mut profile = Game2dProfile::new(Game2dSceneConfig::default());
        profile.scene_mut().camera_mut().position = [1.0, 2.0];
        assert_eq!(profile.scene_mut().camera_mut().position, [1.0, 2.0]);
        let _ = profile.world_mut();
    }
}
