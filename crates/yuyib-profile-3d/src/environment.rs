//! Outdoor / IBL composition recipes for [`super::Game3dProfile`].
//!
//! [`EnvironmentPreset`] owns lighting + probe cook + shadow/SSAO attachment.
//! Map collision, spawn policy, and post-process remain separate owners.

use std::{
    error::Error,
    fmt,
    path::{Path, PathBuf},
};

use yuyib_model_assets::ModelTextureLoaderInitError;
use yuyib_render_3d::{
    DirectionalShadowPolicy, EquirectEnvironmentError, Game3dLighting, Game3dScene,
    Game3dSceneConfig, Game3dShading, GgxCookConfig, GgxCookError, LambertLighting3d,
    LambertLightingError, PbrLighting3d, PreparedEquirectEnvironment3d, SkyboxError, SsaoPolicy,
    cook_ggx_specular_ibl,
};

/// Default relative HDR probe under the profile asset root.
pub const OUTDOOR_PROBE_HDR: &str = "outdoor_probe.hdr";

/// Where the equirect radiance probe comes from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EnvironmentProbeSource {
    /// `{asset_root}/{relative}`; missing file falls back to synthetic outdoor.
    OutdoorHdrRelative {
        /// Path relative to the profile asset root.
        relative: PathBuf,
    },
    /// Always use the tiny synthetic outdoor probe (tests / CI without fixtures).
    SyntheticOutdoor,
    /// Exact HDR file path (must exist and decode).
    AbsoluteHdr(PathBuf),
}

impl Default for EnvironmentProbeSource {
    fn default() -> Self {
        Self::OutdoorHdrRelative {
            relative: PathBuf::from(OUTDOOR_PROBE_HDR),
        }
    }
}

/// Composition recipe: PBR lighting + cooked specular IBL/sky + shadow + SSAO.
#[derive(Clone, Debug)]
pub struct EnvironmentPreset {
    probe: EnvironmentProbeSource,
    cook: GgxCookConfig,
    lighting: Game3dLighting,
    shading: Game3dShading,
    shadow: Option<DirectionalShadowPolicy>,
    ssao: Option<SsaoPolicy>,
}

impl EnvironmentPreset {
    /// Matches today's street-city playable look (IBL + shadows + SSAO).
    ///
    /// # Errors
    ///
    /// Returns lighting validation failures.
    pub fn street_city() -> Result<Self, EnvironmentPresetError> {
        let direct = LambertLighting3d::artistic(
            [-0.15, -1.0, -0.35],
            [1.0, 0.98, 0.94],
            1.35,
            [0.06, 0.07, 0.09],
        )
        .map_err(EnvironmentPresetError::Lighting)?;
        let lighting = PbrLighting3d::from(direct).with_specular_ibl_strength(0.35);
        Ok(Self {
            probe: EnvironmentProbeSource::default(),
            cook: GgxCookConfig::smoke(),
            lighting: Game3dLighting::FixedPbr(lighting),
            shading: Game3dShading::Pbr,
            shadow: Some(DirectionalShadowPolicy::street_city()),
            ssao: Some(SsaoPolicy::street_city()),
        })
    }

    /// Replaces the probe source.
    #[must_use]
    pub fn with_probe(mut self, probe: EnvironmentProbeSource) -> Self {
        self.probe = probe;
        self
    }

    /// Replaces the GGX cook budget (`smoke` vs `quality`).
    #[must_use]
    pub const fn with_cook(mut self, cook: GgxCookConfig) -> Self {
        self.cook = cook;
        self
    }

    /// Replaces fixed lighting (still applied as scene shading/lighting).
    #[must_use]
    pub const fn with_lighting(mut self, lighting: Game3dLighting) -> Self {
        self.lighting = lighting;
        self
    }

    /// Enables or disables directional shadows.
    #[must_use]
    pub const fn with_shadow(mut self, shadow: Option<DirectionalShadowPolicy>) -> Self {
        self.shadow = shadow;
        self
    }

    /// Enables or disables SSAO.
    #[must_use]
    pub const fn with_ssao(mut self, ssao: Option<SsaoPolicy>) -> Self {
        self.ssao = ssao;
        self
    }

    /// Builds a fresh [`Game3dScene`] under `asset_root` with this recipe applied.
    ///
    /// Preset shading/lighting overwrite the default scene config.
    ///
    /// # Errors
    ///
    /// Returns probe I/O, cook, skybox, or scene construction failures.
    pub fn build_scene(
        &self,
        asset_root: impl AsRef<Path>,
    ) -> Result<Game3dScene, EnvironmentPresetError> {
        let asset_root = asset_root.as_ref();
        let scene = Game3dScene::new(
            asset_root,
            Game3dSceneConfig::default()
                .with_shading(self.shading)
                .with_lighting(self.lighting),
        )
        .map_err(EnvironmentPresetError::Scene)?;
        self.apply(asset_root, scene)
    }

    /// Applies probe cook + shadow/SSAO onto an existing scene.
    ///
    /// Prefer [`Self::build_scene`] when constructing from scratch. This method
    /// does **not** rewrite the scene's shading/lighting config (already set).
    ///
    /// # Errors
    ///
    /// Returns probe I/O, cook, or skybox failures.
    pub fn apply(
        &self,
        asset_root: &Path,
        scene: Game3dScene,
    ) -> Result<Game3dScene, EnvironmentPresetError> {
        let equirect = load_probe(asset_root, &self.probe)?;
        let specular =
            cook_ggx_specular_ibl(&equirect, self.cook).map_err(EnvironmentPresetError::Cook)?;
        let mut scene = scene
            .with_environment(specular)
            .map_err(EnvironmentPresetError::Skybox)?;
        if let Some(shadow) = self.shadow {
            scene = scene.with_directional_shadow(shadow);
        }
        if let Some(ssao) = self.ssao {
            scene = scene.with_ssao(ssao);
        }
        Ok(scene)
    }
}

fn load_probe(
    asset_root: &Path,
    source: &EnvironmentProbeSource,
) -> Result<PreparedEquirectEnvironment3d, EnvironmentPresetError> {
    match source {
        EnvironmentProbeSource::SyntheticOutdoor => {
            PreparedEquirectEnvironment3d::synthetic_outdoor_probe()
                .map_err(EnvironmentPresetError::Equirect)
        }
        EnvironmentProbeSource::AbsoluteHdr(path) => {
            let bytes = std::fs::read(path).map_err(|error| EnvironmentPresetError::Io {
                path: path.clone(),
                error,
            })?;
            PreparedEquirectEnvironment3d::from_radiance_hdr_bytes(&bytes)
                .map_err(EnvironmentPresetError::Equirect)
        }
        EnvironmentProbeSource::OutdoorHdrRelative { relative } => {
            let path = asset_root.join(relative);
            match std::fs::read(&path) {
                Ok(bytes) => PreparedEquirectEnvironment3d::from_radiance_hdr_bytes(&bytes)
                    .map_err(EnvironmentPresetError::Equirect),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    PreparedEquirectEnvironment3d::synthetic_outdoor_probe()
                        .map_err(EnvironmentPresetError::Equirect)
                }
                Err(error) => Err(EnvironmentPresetError::Io { path, error }),
            }
        }
    }
}

/// Failure while building or applying an [`EnvironmentPreset`].
#[derive(Debug)]
pub enum EnvironmentPresetError {
    /// Key light / ambient validation failed.
    Lighting(LambertLightingError),
    /// Scene texture-root construction failed.
    Scene(ModelTextureLoaderInitError),
    /// Equirect decode or synthetic probe failed.
    Equirect(EquirectEnvironmentError),
    /// GGX specular cook failed.
    Cook(GgxCookError),
    /// Skybox mip0 extraction failed.
    Skybox(SkyboxError),
    /// HDR file I/O failed.
    Io {
        /// Path that failed.
        path: PathBuf,
        /// Underlying I/O error.
        error: std::io::Error,
    },
}

impl fmt::Display for EnvironmentPresetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lighting(error) => write!(formatter, "environment lighting: {error}"),
            Self::Scene(error) => write!(formatter, "environment scene: {error}"),
            Self::Equirect(error) => write!(formatter, "environment equirect: {error}"),
            Self::Cook(error) => write!(formatter, "environment ggx cook: {error}"),
            Self::Skybox(error) => write!(formatter, "environment skybox: {error}"),
            Self::Io { path, error } => {
                write!(formatter, "environment probe {}: {error}", path.display())
            }
        }
    }
}

impl Error for EnvironmentPresetError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Lighting(error) => Some(error),
            Self::Scene(error) => Some(error),
            Self::Equirect(error) => Some(error),
            Self::Cook(error) => Some(error),
            Self::Skybox(error) => Some(error),
            Self::Io { error, .. } => Some(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{EnvironmentPreset, EnvironmentProbeSource};

    #[test]
    fn street_city_preset_builds_scene_with_synthetic_probe() {
        let root = std::env::temp_dir().join(format!(
            "yuyib_env_preset_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("temp root");
        let preset = EnvironmentPreset::street_city()
            .expect("lighting")
            .with_probe(EnvironmentProbeSource::SyntheticOutdoor);
        let scene = preset.build_scene(&root).expect("scene");
        let _ = scene;
        let _ = std::fs::remove_dir_all(root);
    }
}
