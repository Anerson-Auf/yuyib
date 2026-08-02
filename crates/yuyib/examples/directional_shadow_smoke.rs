//! M2.3 smoke: directional shadow map darkens an occluded ground region.
//!
//! No window. Casts a cube onto a ground plane from a slanted directional
//! light, draws factor-only PBR with the shadow map, and asserts the shadowed
//! ground ROI is darker than a lit ROI.
//!
//! ```text
//! cargo run -p yuyib --example directional_shadow_smoke
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
        PbrLighting3d, PbrMaterial3d, PbrMeshRenderer3d,
    },
};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 240;
const CLEAR: [f64; 4] = [0.05, 0.06, 0.08, 1.0];

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

/// Scans overlapping windows and returns (brightest, darkest) mean luma.
fn extremal_window_luma(frame: &CapturedFrameRgba8, y0: u32, y1: u32, window: u32) -> (f32, f32) {
    let width = frame.width();
    let mut brightest = 0.0_f32;
    let mut darkest = f32::MAX;
    let mut x = 0_u32;
    while x + window <= width {
        let luma = region_mean_luma(frame, x, y0, x + window, y1);
        brightest = brightest.max(luma);
        darkest = darkest.min(luma);
        x += window / 2;
    }
    (brightest, darkest)
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut gpu = OffscreenRenderer::new(WIDTH, HEIGHT)?;
    let cube = MeshPrimitive::cube(0.4)?;
    // Large cube sunk so its top face is the ground at y≈0 (no plane primitive).
    let ground = MeshPrimitive::cube(2.0)?;
    let camera = Camera3d::new(
        [3.2, 2.6, 3.8],
        [0.0, 0.15, 0.0],
        [0.0, 1.0, 0.0],
        45.0_f32.to_radians(),
        0.05,
        64.0,
    );
    let light_dir = [-0.55, -1.0, -0.15];
    // Strong direct, tiny ambient so shadow contrast lives in the direct term.
    let direct =
        LambertLighting3d::artistic(light_dir, [1.0, 0.98, 0.94], 2.4, [0.02, 0.022, 0.025])?;
    let lighting = PbrLighting3d::from(direct).with_specular_ibl_strength(0.0);
    let shadow_config = DirectionalShadowConfig::new(512, [0.0, 0.5, 0.0], [4.0, 4.0, 6.0], 0.0015)?;

    let cube_model =
        MeshTransform3d::new([0.0, 0.4, 0.0], [0.0, 0.2, 0.0], [1.0, 1.0, 1.0]).matrix()?;
    let ground_model =
        MeshTransform3d::new([0.0, -2.0, 0.0], [0.0, 0.0, 0.0], [1.0, 1.0, 1.0]).matrix()?;
    let cube_mat = PbrMaterial3d::new([0.78, 0.80, 0.84, 1.0], 0.05, 0.4)?;
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
                        base_color: cube_mat.base_color(),
                        alpha_cutoff: cube_mat.alpha_mode().shader_cutoff(),
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
                cube_mat,
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

    // Top-face band: direct light dominates, so 1-tap shadows should create a
    // measurable bright/dark split near the occluder.
    let (lit, shadowed) = extremal_window_luma(&captured, 70, 130, 36);
    if shadowed >= lit * 0.88 {
        let path = write_debug_png(&captured)?;
        return Err(format!(
            "directional_shadow_smoke: expected shadowed luma {shadowed:.1} < lit {lit:.1} \
             (png {})",
            path.display()
        )
        .into());
    }

    let path = write_debug_png(&captured)?;
    println!(
        "directional_shadow_smoke OK: lit_luma={lit:.1} shadow_luma={shadowed:.1} png={}",
        path.display()
    );
    Ok(())
}

fn write_debug_png(frame: &CapturedFrameRgba8) -> Result<PathBuf, Box<dyn Error>> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!("yuyib_directional_shadow_smoke_{stamp}.png"));
    write_png_rgba8(&path, frame.width(), frame.height(), frame.pixels())?;
    Ok(path)
}
