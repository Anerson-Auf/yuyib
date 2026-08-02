//! M2 smoke: equirect → GGX cook → `Game3dScene` specular attachment.
//!
//! No window. Cooks the synthetic outdoor equirect into a 16² cube mip chain,
//! asserts +Y is brighter than −Y on the mirror mip, queues the pack on a
//! `Game3dScene` (same path as street-city), and prints LOD mapping.
//!
//! ```text
//! cargo run -p yuyib --example ggx_ibl_cook_smoke
//! ```

use std::{error::Error, path::PathBuf};

use yuyib::render_3d::{
    GgxCookConfig, Game3dLighting, Game3dScene, Game3dSceneConfig, Game3dShading,
    LambertLighting3d, PbrLighting3d, PreparedEquirectEnvironment3d, cook_ggx_specular_ibl,
};

fn face_mean_luma(pixels: &[u8]) -> f32 {
    let mut sum = 0.0_f32;
    let mut count = 0_u32;
    for texel in pixels.chunks_exact(4) {
        sum += 0.2126 * f32::from(texel[0])
            + 0.7152 * f32::from(texel[1])
            + 0.0722 * f32::from(texel[2]);
        count += 1;
    }
    sum / count.max(1) as f32
}

fn main() -> Result<(), Box<dyn Error>> {
    let env = PreparedEquirectEnvironment3d::synthetic_outdoor_probe()?;
    let prepared = cook_ggx_specular_ibl(&env, GgxCookConfig::smoke())?;

    assert_eq!(prepared.face_size(), 16);
    assert_eq!(prepared.mip_level_count(), 5);
    assert_eq!(prepared.lut_size(), 64);

    let plus_y = prepared
        .mip_face_rgba8(0, 2)
        .ok_or("missing +Y mip0 face")?;
    let minus_y = prepared
        .mip_face_rgba8(0, 3)
        .ok_or("missing -Y mip0 face")?;
    let up = face_mean_luma(plus_y);
    let down = face_mean_luma(minus_y);
    if up <= down {
        return Err(format!(
            "ggx_ibl_cook_smoke: expected +Y luma {up} > -Y luma {down}"
        )
        .into());
    }

    let face_size = prepared.face_size();
    let mip_level_count = prepared.mip_level_count();
    let lut_size = prepared.lut_size();
    let lod_half = prepared.roughness_to_lod(0.5);

    // Same attachment contract as street-city `create_renderer`.
    let asset_root = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../for_tests"));
    let direct = LambertLighting3d::artistic(
        [-0.15, -1.0, -0.35],
        [1.0, 0.98, 0.94],
        1.35,
        [0.06, 0.07, 0.09],
    )?;
    let lighting = PbrLighting3d::from(direct).with_specular_ibl_strength(0.35);
    let _scene = Game3dScene::new(
        &asset_root,
        Game3dSceneConfig::default()
            .with_shading(Game3dShading::Pbr)
            .with_lighting(Game3dLighting::FixedPbr(lighting)),
    )?
    .with_environment(prepared)?;

    println!(
        "ggx_ibl_cook_smoke OK: face={face_size} mips={mip_level_count} lut={lut_size} \
         +Y_luma={up:.1} -Y_luma={down:.1} lod(0.5)={lod_half:.2} \
         scene_probe=queued scene_sky=queued"
    );
    Ok(())
}
