//! M2.4 smoke: PBR alpha MASK discards shading fragments and shadow cutouts.
//!
//! No window. Two captures of the same cube/ground setup:
//! 1. Opaque occluder → centre pixels are solid and the ground is shadowed.
//! 2. MASK occluder with base alpha below cutoff → cube disappears and the
//!    ground under it stays lit (caster discard).
//!
//! ```text
//! cargo run -p yuyib --example pbr_alpha_mask_smoke
//! ```

use std::{
    error::Error,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use yuyib::{
    image::write_png_rgba8,
    model::MeshPrimitive,
    render::{CapturedFrameRgba8, ClearColor, OffscreenRenderer},
    render_3d::{
        Camera3d, DepthLoad, DirectionalShadowCaster3d, DirectionalShadowConfig,
        FactorShadowCasterDraw, GpuDirectionalShadow, LambertLighting3d, MeshTransform3d,
        PbrAlphaMode3d, PbrLighting3d, PbrMaterial3d, PbrMeshRenderer3d,
    },
};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 240;
const CLEAR: [f64; 4] = [0.08, 0.09, 0.11, 1.0];

fn region_mean_luma(frame: &CapturedFrameRgba8, x0: u32, y0: u32, x1: u32, y1: u32) -> f32 {
    let width = frame.width();
    let pixels = frame.pixels();
    let mut sum = 0.0_f32;
    let mut count = 0_u32;
    for y in y0..y1 {
        for x in x0..x1 {
            let index = ((y * width + x) * 4) as usize;
            sum += 0.2126 * f32::from(pixels[index])
                + 0.7152 * f32::from(pixels[index + 1])
                + 0.0722 * f32::from(pixels[index + 2]);
            count += 1;
        }
    }
    sum / count.max(1) as f32
}

fn non_clear_fraction(frame: &CapturedFrameRgba8) -> f32 {
    let clear_rgb = [
        (CLEAR[0] * 255.0) as u8,
        (CLEAR[1] * 255.0) as u8,
        (CLEAR[2] * 255.0) as u8,
    ];
    let pixels = frame.pixels();
    let mut non_clear = 0_u32;
    let mut total = 0_u32;
    for texel in pixels.chunks_exact(4) {
        total += 1;
        if (texel[0].abs_diff(clear_rgb[0]) > 8)
            || (texel[1].abs_diff(clear_rgb[1]) > 8)
            || (texel[2].abs_diff(clear_rgb[2]) > 8)
        {
            non_clear += 1;
        }
    }
    non_clear as f32 / total.max(1) as f32
}

fn mean_luma(frame: &CapturedFrameRgba8) -> f32 {
    region_mean_luma(frame, 0, 0, frame.width(), frame.height())
}

fn clear_luma() -> f32 {
    (0.2126 * CLEAR[0] + 0.7152 * CLEAR[1] + 0.0722 * CLEAR[2]) as f32 * 255.0
}

fn capture(
    gpu: &mut OffscreenRenderer,
    occluder_mat: PbrMaterial3d,
) -> Result<CapturedFrameRgba8, Box<dyn Error>> {
    let cube = MeshPrimitive::cube(0.45)?;
    let ground = MeshPrimitive::cube(2.0)?;
    let camera = Camera3d::new(
        [3.0, 2.4, 3.6],
        [0.0, 0.2, 0.0],
        [0.0, 1.0, 0.0],
        45.0_f32.to_radians(),
        0.05,
        64.0,
    );
    let light_dir = [-0.55, -1.0, -0.15];
    let direct =
        LambertLighting3d::artistic(light_dir, [1.0, 0.98, 0.94], 2.4, [0.02, 0.022, 0.025])?;
    let lighting = PbrLighting3d::from(direct).with_specular_ibl_strength(0.0);
    let shadow_config = DirectionalShadowConfig::new(512, [0.0, 0.5, 0.0], [4.0, 4.0, 6.0], 0.0015)?;
    let cube_model =
        MeshTransform3d::new([0.0, 0.45, 0.0], [0.0, 0.15, 0.0], [1.0, 1.0, 1.0]).matrix()?;
    let ground_model =
        MeshTransform3d::new([0.0, -2.0, 0.0], [0.0, 0.0, 0.0], [1.0, 1.0, 1.0]).matrix()?;
    let ground_mat = PbrMaterial3d::new([0.62, 0.64, 0.58, 1.0], 0.0, 0.9)?;

    let mut draw_error = None;
    let captured = gpu.render_and_capture_rgba8(
        ClearColor::linear(CLEAR[0], CLEAR[1], CLEAR[2], CLEAR[3]),
        |frame| {
            let meshes = PbrMeshRenderer3d::new_for_frame(frame);
            let casters = DirectionalShadowCaster3d::new_for_frame(frame);
            let shadow = match GpuDirectionalShadow::create_for_frame(frame, shadow_config, light_dir)
            {
                Ok(shadow) => shadow,
                Err(error) => {
                    draw_error = Some(error.to_string());
                    return;
                }
            };
            let gpu_cube = match meshes.upload_mesh_for_frame(frame, &cube) {
                Ok(mesh) => mesh,
                Err(error) => {
                    draw_error = Some(error.to_string());
                    return;
                }
            };
            let gpu_ground = match meshes.upload_mesh_for_frame(frame, &ground) {
                Ok(mesh) => mesh,
                Err(error) => {
                    draw_error = Some(error.to_string());
                    return;
                }
            };
            casters.draw_casters(
                frame,
                &shadow,
                &[
                    FactorShadowCasterDraw {
                        mesh: &gpu_cube,
                        model_matrix: cube_model,
                        base_color: occluder_mat.base_color(),
                        alpha_cutoff: occluder_mat.alpha_mode().shader_cutoff(),
                    },
                    FactorShadowCasterDraw {
                        mesh: &gpu_ground,
                        model_matrix: ground_model,
                        base_color: ground_mat.base_color(),
                        alpha_cutoff: ground_mat.alpha_mode().shader_cutoff(),
                    },
                ],
            );
            if let Err(error) = meshes.draw_with_specular_ibl(
                frame,
                camera,
                &gpu_ground,
                ground_model,
                ground_mat,
                lighting,
                DepthLoad::Clear,
                false,
                None,
                Some(&shadow),
            ) {
                draw_error = Some(error.to_string());
                return;
            }
            if let Err(error) = meshes.draw_with_specular_ibl(
                frame,
                camera,
                &gpu_cube,
                cube_model,
                occluder_mat,
                lighting,
                DepthLoad::Load,
                false,
                None,
                Some(&shadow),
            ) {
                draw_error = Some(error.to_string());
            }
        },
    )?;
    if let Some(error) = draw_error {
        return Err(error.into());
    }
    Ok(captured)
}

fn write_debug_png(frame: &CapturedFrameRgba8, tag: &str) -> Result<PathBuf, Box<dyn Error>> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!("yuyib_pbr_alpha_mask_smoke_{tag}_{stamp}.png"));
    write_png_rgba8(&path, frame.width(), frame.height(), frame.pixels())?;
    Ok(path)
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut gpu = OffscreenRenderer::new(WIDTH, HEIGHT)?;

    let opaque = PbrMaterial3d::new([0.85, 0.82, 0.78, 1.0], 0.05, 0.35)?;
    let masked = PbrMaterial3d::new([0.85, 0.82, 0.78, 0.2], 0.05, 0.35)?
        .with_alpha_mode(PbrAlphaMode3d::mask(0.5)?);

    let opaque_frame = capture(&mut gpu, opaque)?;
    let masked_frame = capture(&mut gpu, masked)?;

    let opaque_fill = non_clear_fraction(&opaque_frame);
    let masked_fill = non_clear_fraction(&masked_frame);
    let opaque_luma = mean_luma(&opaque_frame);
    let masked_luma = mean_luma(&masked_frame);
    let clear = clear_luma();

    if opaque_fill < 0.08 {
        let path = write_debug_png(&opaque_frame, "opaque")?;
        return Err(format!(
            "pbr_alpha_mask_smoke: opaque scene almost empty \
             (non_clear={opaque_fill:.3}, png {})",
            path.display()
        )
        .into());
    }
    if masked_fill > opaque_fill * 0.55 {
        let path = write_debug_png(&masked_frame, "masked")?;
        return Err(format!(
            "pbr_alpha_mask_smoke: MASK should discard most of the occluder \
             (masked_fill={masked_fill:.3}, opaque_fill={opaque_fill:.3}, png {})",
            path.display()
        )
        .into());
    }
    // Clear colour is dark; discarding the cube (and its shadow) pulls mean luma
    // toward clear and shrinks non-clear coverage.
    let opaque_delta = (opaque_luma - clear).abs();
    let masked_delta = (masked_luma - clear).abs();
    if masked_delta >= opaque_delta * 0.5 {
        let path = write_debug_png(&masked_frame, "shadow_cutout")?;
        return Err(format!(
            "pbr_alpha_mask_smoke: expected MASK cutout closer to clear \
             (masked_delta={masked_delta:.1}, opaque_delta={opaque_delta:.1}, png {})",
            path.display()
        )
        .into());
    }

    let opaque_png = write_debug_png(&opaque_frame, "opaque")?;
    let masked_png = write_debug_png(&masked_frame, "masked")?;
    println!(
        "pbr_alpha_mask_smoke OK: opaque_fill={opaque_fill:.3} masked_fill={masked_fill:.3} \
         opaque_luma={opaque_luma:.1} masked_luma={masked_luma:.1} \
         pngs={} {}",
        opaque_png.display(),
        masked_png.display()
    );
    Ok(())
}
