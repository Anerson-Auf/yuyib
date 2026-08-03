//! Shared animated playermodel fixture for street-city playable + M1 smoke.
//!
//! Keeps character path, scale and eye-socket names from drifting between
//! interactive and headless examples. Edit [`CHARACTER_MODEL_SCALE`] here when
//! tuning the model size by hand.

use std::{
    error::Error,
    fmt::Write as _,
    path::{Path, PathBuf},
    sync::Arc,
};

use yuyib::{
    gltf::{
        AnimationClipIndex, AnimationPlayer, AnimationSnapshot, ImportOptions, ImportedAsset,
        import_scene_path_with_options, sample_bind_pose,
    },
    image::{DecodePolicy, compress_rgba8_luminance, decode_bytes, encode_png_rgba8},
    model::{MaterialFactorPatch, Model, ModelMaterialPolicy, ModelTexture, ModelTextureSource},
    model_assets::{ModelTextureLoader, PreparedModelTextures},
};

/// Character GLB under `for_tests/`.
pub const CHARACTER_FILE: &str = "sci-fi_girl_v.02_walkcycle_test.glb";
/// Right-eye bone used for chase / FPS focus.
#[allow(
    dead_code,
    reason = "Used by cyberpunk_city_playable; shared for anti-drift."
)]
pub const RIGHT_EYE_BONE_NAME: &str = "Eye_R_047";
/// Left-eye bone used for chase / FPS focus.
#[allow(
    dead_code,
    reason = "Used by cyberpunk_city_playable; shared for anti-drift."
)]
pub const LEFT_EYE_BONE_NAME: &str = "Eye_L_056";
/// Uniform playermodel scale (edit this when resizing by hand).
pub const CHARACTER_MODEL_SCALE: f32 = 0.3;
/// Capsule radius paired with the street-city controller.
pub const CHARACTER_CONTROLLER_RADIUS: f32 = 0.28;
/// Walk-clip index in the character fixture.
pub const CHARACTER_WALK_CLIP: usize = 0;
/// Headless smoke advances the walk clip by this many seconds.
#[allow(
    dead_code,
    reason = "Used by street_city_m1_smoke; shared for anti-drift."
)]
pub const CHARACTER_SMOKE_ADVANCE_SECONDS: f32 = 0.4;

/// Absolute path to the character GLB.
#[must_use]
pub fn character_path(asset_root: &Path) -> PathBuf {
    asset_root.join(CHARACTER_FILE)
}

/// Optional named material corrections for this exact character source.
///
/// The inspected `sci-fi_girl_v.02_walkcycle_test.glb` uses the legacy
/// `KHR_materials_pbrSpecularGlossiness` workflow for both `body_mat` and
/// `cloth_mat`, with no core metallic-roughness object. Its core
/// `metallicFactor` therefore defaults to 1.0, but the skeletal renderer uses
/// that workflow's `diffuse*` inputs for visible colour rather than the core
/// metallic value. Do not add a metallic patch merely to brighten the model:
/// it would not correct this renderer path and would invent a material policy
/// without asset-backed effect.
///
/// Force `body_mat` **single-sided**. The source (or a prior policy) may mark it
/// double-sided; thin limbs then rasterize front and back faces at nearly the
/// same depth, and a millimetre of camera yaw z-fights between different UV
/// islands — the whole arm/skin island flips from bright to muddy while the
/// white cloth morph stays blown-out. Hair cards may stay double-sided.
///
/// After import we also luminance-compress the `body_mat` diffuse albedo (see
/// [`unbake_body_mat_diffuse`]): the fixture ships front/back UV islands with
/// baked lighting baked into the colour map. Flat skinned shading shows that
/// honestly as front≠back. A clean re-export without bake remains the correct
/// long-term fix; this runtime unbake is fixture-specific triage.
#[must_use]
pub fn character_material_policy() -> ModelMaterialPolicy {
    // Only `body_mat` exists on the sci-fi girl fixture. A required `Body_mat`
    // patch aborts street-city / cyberpunk load when the name is absent.
    ModelMaterialPolicy::new().patch_named(
        "body_mat",
        MaterialFactorPatch::new().with_double_sided(false),
    )
}

/// Contrast used when compressing baked luminance in `body_mat` diffuse.
///
/// `0.0` would flatten to the mean; `1.0` keeps the source. `≈0.40` softens
/// front/back bake islands without washing skin hue entirely.
const BODY_ALBEDO_UNBAKE_CONTRAST: f32 = 0.40;

/// Softens baked lighting in the named `body_mat` visible colour texture.
///
/// Spec-gloss `diffuse_texture` wins over core `base_color_texture`. Embedded
/// GLB bytes are decoded, luminance-compressed, re-encoded as PNG, and written
/// back via [`Model::replace_texture`]. External URI textures are skipped with
/// a warning (this fixture embeds the map).
///
/// # Errors
///
/// Propagates decode / encode / texture-slot failures for embedded sources.
fn unbake_body_mat_diffuse(model: &mut Model) -> Result<(), Box<dyn Error>> {
    let Some(texture_index) = model.materials().iter().find_map(|material| {
        if material.name() != Some("body_mat") {
            return None;
        }
        material
            .specular_glossiness()
            .map_or_else(
                || material.base_color_texture(),
                |workflow| workflow.diffuse_texture(),
            )
            .map(|binding| binding.texture())
    }) else {
        return Ok(());
    };
    let (encoded_bytes, label, sampler) = {
        let texture = model
            .textures()
            .get(texture_index.get())
            .ok_or("body_mat diffuse texture index is out of range")?;
        match texture.source() {
            ModelTextureSource::Encoded { bytes, .. } => (
                Arc::clone(bytes),
                texture.label().map(str::to_owned),
                texture.sampler(),
            ),
            ModelTextureSource::ExternalUri(_) => {
                eprintln!(
                    "body_mat diffuse is an external URI; skipping runtime albedo unbake"
                );
                return Ok(());
            }
        }
    };
    let decoded = decode_bytes(&encoded_bytes, DecodePolicy::default())?;
    let width = decoded.texture().size().width();
    let height = decoded.texture().size().height();
    let mut pixels = decoded.into_pixels();
    compress_rgba8_luminance(&mut pixels, BODY_ALBEDO_UNBAKE_CONTRAST);
    let encoded = encode_png_rgba8(width, height, &pixels)?;
    let mut replacement = ModelTexture::embedded("image/png", Arc::<[u8]>::from(encoded));
    if let Some(label) = label {
        replacement = replacement.with_label(label);
    }
    if let Some(sampler) = sampler {
        replacement = replacement.with_sampler(sampler);
    }
    model.replace_texture(texture_index, replacement)?;
    Ok(())
}

/// Imports the skeletal character and applies the shared named material policy.
///
/// # Errors
///
/// Returns import or material-policy failures, or rejects a character with no
/// skeleton / no animation clips.
pub fn import_character(asset_root: &Path) -> Result<ImportedAsset, Box<dyn Error>> {
    let path = character_path(asset_root);
    if !path.is_file() {
        return Err(format!(
            "missing character fixture at {} — place {CHARACTER_FILE} under for_tests/",
            path.display()
        )
        .into());
    }
    let mut asset = import_scene_path_with_options(&path, ImportOptions::skeletal_preview())?;
    character_material_policy().apply(&mut asset.model)?;
    unbake_body_mat_diffuse(&mut asset.model)?;
    if asset.scene.skins().is_empty() {
        return Err(format!("{CHARACTER_FILE} contains no skeleton").into());
    }
    if asset.scene.animations().is_empty() {
        return Err(format!("{CHARACTER_FILE} contains no walk animation").into());
    }
    Ok(asset)
}

/// Imports the skeletal character and prepares material-referenced textures.
///
/// # Errors
///
/// Returns import or texture-preparation failures, or rejects a character with
/// no skeleton / no animation clips.
#[allow(
    dead_code,
    reason = "Used by street_city_m1_smoke; shared for anti-drift."
)]
pub fn load_prepared_character(
    asset_root: &Path,
) -> Result<(ImportedAsset, PreparedModelTextures), Box<dyn Error>> {
    let asset = import_character(asset_root)?;
    let textures = ModelTextureLoader::new(asset_root)?.prepare(&asset.model)?;
    Ok((asset, textures))
}

/// Formats asset-backed character material, texture and UV diagnostics.
///
/// `ImportedAsset` intentionally retains source material metadata but not the
/// registry adapter's `ImportDiagnostic` vector. This summary uses the same
/// `Model::material_usage` and `Model::texture_usage` inventory that powers
/// those diagnostics; warnings are reported and remain non-fatal.
#[must_use]
#[allow(
    dead_code,
    reason = "Used by street_city_m1_smoke; shared with playable policy hook."
)]
pub fn character_material_texture_summary(asset: &ImportedAsset) -> String {
    let model = &asset.model;
    let material_usage = model.material_usage();
    let texture_usage = model.texture_usage();
    let mut output = String::from("character material/texture inventory:\n");

    for (index, material) in model.materials().iter().enumerate() {
        let name = material.name().unwrap_or("<unnamed>");
        let visible_colour_texture = material.specular_glossiness().map_or_else(
            || material.base_color_texture(),
            |workflow| workflow.diffuse_texture(),
        );
        let visible_colour_source = if material.specular_glossiness().is_some() {
            "KHR_materials_pbrSpecularGlossiness diffuse"
        } else {
            "metallic-roughness baseColor"
        };
        let core_metallic = if material.metallic_factor() == 1.0
            && material.metallic_roughness_texture().is_none()
        {
            "1.00 without metallic-roughness map (source default or explicit factor)"
        } else {
            "non-default or texture-backed"
        };
        let visible_colour = if visible_colour_texture.is_some() {
            "texture-backed"
        } else {
            "factor-only visible colour"
        };
        let _ = writeln!(
            output,
            "  material #{index} `{name}`: {visible_colour_source}, {visible_colour}; core metallic={core_metallic}"
        );
    }

    output.push_str(&material_usage.summary());
    output.push_str(&texture_usage.summary());
    if texture_usage.missing_uv_bindings().is_empty() {
        output.push_str("UV diagnostics: no material texture binding requires an absent UV set.\n");
    } else {
        output.push_str(
            "UV diagnostics: missing UV bindings are recoverable warnings; the renderer uses its documented factor fallback.\n",
        );
    }
    output
}

/// Bind pose plus a walk-clip pose advanced by [`CHARACTER_SMOKE_ADVANCE_SECONDS`].
///
/// # Errors
///
/// Forwards animation sampling failures.
#[allow(
    dead_code,
    reason = "Used by street_city_m1_smoke; shared for anti-drift."
)]
pub fn bind_and_advanced_walk_poses(
    asset: &ImportedAsset,
) -> Result<(AnimationSnapshot, AnimationSnapshot), Box<dyn Error>> {
    let bind = sample_bind_pose(&asset.scene)?;
    let mut player = AnimationPlayer::new(AnimationClipIndex::new(CHARACTER_WALK_CLIP));
    player.play();
    player.advance(&asset.scene, CHARACTER_SMOKE_ADVANCE_SECONDS)?;
    let advanced = player.snapshot(&asset.scene)?;
    Ok((bind, advanced))
}

/// Mean absolute translation delta between two pose world matrices.
#[must_use]
#[allow(
    dead_code,
    reason = "Used by street_city_m1_smoke; shared for anti-drift."
)]
pub fn pose_translation_delta(left: &AnimationSnapshot, right: &AnimationSnapshot) -> f32 {
    let left = left.world_matrices();
    let right = right.world_matrices();
    let count = left.len().min(right.len());
    if count == 0 {
        return 0.0;
    }
    let mut total = 0.0_f32;
    for index in 0..count {
        total += (left[index][12] - right[index][12]).abs()
            + (left[index][13] - right[index][13]).abs()
            + (left[index][14] - right[index][14]).abs();
    }
    #[allow(
        clippy::cast_precision_loss,
        reason = "pose node counts stay far below f32 precision limits"
    )]
    {
        total / count as f32
    }
}
