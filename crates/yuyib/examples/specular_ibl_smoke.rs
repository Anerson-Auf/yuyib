//! M2.2 smoke: factor-only PBR with prefiltered specular IBL.
//!
//! No window. Draws metallic probe cubes at three roughnesses under a
//! synthetic asymmetric LDR cubemap + BRDF LUT, captures RGBA8, and asserts
//! smooth vs rough probes differ in luminance.
//!
//! ```text
//! cargo run -p yuyib --example specular_ibl_smoke
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
        Camera3d, DepthLoad, DiffuseIrradianceSh3d, GpuSpecularIbl3d, LambertLighting3d,
        MeshTransform3d, PbrLighting3d, PbrMaterial3d, PbrMeshRenderer3d, PreparedSpecularIbl3d,
    },
};

const WIDTH: u32 = 480;
const HEIGHT: u32 = 180;
const CLEAR: [f64; 4] = [0.02, 0.03, 0.05, 1.0];

fn main() -> Result<(), Box<dyn Error>> {
    let mut gpu = OffscreenRenderer::new(WIDTH, HEIGHT)?;
    let cube = MeshPrimitive::cube(0.35)?;
    let camera = Camera3d::new(
        [0.0, 0.55, 3.2],
        [0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        50.0_f32.to_radians(),
        0.05,
        32.0,
    );
    let prepared = PreparedSpecularIbl3d::synthetic_asymmetric()?;
    let direct = LambertLighting3d::artistic(
        [-0.2, -1.0, -0.35],
        [1.0, 0.98, 0.94],
        0.35,
        [0.08, 0.09, 0.11],
    )?;
    let diffuse = DiffuseIrradianceSh3d::constant([0.12, 0.13, 0.16])?;
    let lighting = PbrLighting3d::new(direct, diffuse).with_specular_ibl_strength(1.0);

    let probes = [(-1.15_f32, 0.08_f32), (0.0, 0.35), (1.15, 0.85)];

    let mut draw_error = None;
    let captured = gpu.render_and_capture_rgba8(
        ClearColor::linear(CLEAR[0], CLEAR[1], CLEAR[2], CLEAR[3]),
        |frame| {
            let meshes = PbrMeshRenderer3d::new_for_frame(frame);
            let ibl = GpuSpecularIbl3d::upload_for_frame(frame, &prepared);
            let gpu_cube = match meshes.upload_mesh_for_frame(frame, &cube) {
                Ok(mesh) => mesh,
                Err(error) => {
                    draw_error = Some(error.to_string());
                    return;
                }
            };
            for (index, (x, roughness)) in probes.iter().copied().enumerate() {
                let model = match MeshTransform3d::new([x, 0.0, 0.0], [0.0, 0.35, 0.0], [1.0, 1.0, 1.0])
                    .matrix()
                {
                    Ok(matrix) => matrix,
                    Err(error) => {
                        draw_error = Some(error.to_string());
                        return;
                    }
                };
                let material = match PbrMaterial3d::new([0.92, 0.92, 0.94, 1.0], 1.0, roughness) {
                    Ok(material) => material,
                    Err(error) => {
                        draw_error = Some(error.to_string());
                        return;
                    }
                };
                let depth = if index == 0 {
                    DepthLoad::Clear
                } else {
                    DepthLoad::Load
                };
                if let Err(error) = meshes.draw_with_specular_ibl(
                    frame,
                    camera,
                    &gpu_cube,
                    model,
                    material,
                    lighting,
                    depth,
                    false,
                    Some(&ibl),
                    None,
                ) {
                    draw_error = Some(error.to_string());
                    return;
                }
            }
        },
    )?;
    if let Some(error) = draw_error {
        return Err(error.into());
    }

    let (smooth_luma, mid_luma, rough_luma) = probe_region_luma(&captured)?;
    let non_clear = count_non_clear(&captured);
    if non_clear < 256 {
        return Err(format!(
            "specular_ibl_smoke: frame too empty (non_clear={non_clear})"
        )
        .into());
    }
    let max_luma = smooth_luma.max(mid_luma).max(rough_luma);
    let min_luma = smooth_luma.min(mid_luma).min(rough_luma);
    if max_luma - min_luma < 0.004 {
        return Err(format!(
            "specular_ibl_smoke: expected roughness-dependent probe separation \
             (smooth={smooth_luma:.4}, mid={mid_luma:.4}, rough={rough_luma:.4})"
        )
        .into());
    }
    let path = write_capture_png(&captured)?;
    println!(
        "specular_ibl_smoke OK: {}x{}, non_clear={non_clear}, \
         smooth_luma={smooth_luma:.4} mid_luma={mid_luma:.4} rough_luma={rough_luma:.4}, wrote {}",
        captured.width(),
        captured.height(),
        path.display()
    );
    Ok(())
}

fn count_non_clear(frame: &CapturedFrameRgba8) -> usize {
    let clear_u8 = [
        (CLEAR[0] * 255.0).round() as u8,
        (CLEAR[1] * 255.0).round() as u8,
        (CLEAR[2] * 255.0).round() as u8,
        255,
    ];
    frame
        .pixels()
        .chunks_exact(4)
        .filter(|pixel| {
            pixel
                .iter()
                .zip(clear_u8)
                .map(|(sample, expected)| sample.abs_diff(expected))
                .max()
                .unwrap_or(0)
                > 8
        })
        .count()
}

fn probe_region_luma(frame: &CapturedFrameRgba8) -> Result<(f64, f64, f64), Box<dyn Error>> {
    let width = frame.width() as usize;
    let height = frame.height() as usize;
    let pixels = frame.pixels();
    let mut regions = [
        (0..width / 3, 0.0_f64, 0_usize),
        (width / 3..2 * width / 3, 0.0_f64, 0_usize),
        (2 * width / 3..width, 0.0_f64, 0_usize),
    ];
    let y0 = height / 4;
    let y1 = (3 * height) / 4;
    for y in y0..y1 {
        for (x_range, luma, count) in &mut regions {
            for x in x_range.clone() {
                let idx = (y * width + x) * 4;
                let r = f64::from(pixels[idx]) / 255.0;
                let g = f64::from(pixels[idx + 1]) / 255.0;
                let b = f64::from(pixels[idx + 2]) / 255.0;
                *luma += 0.2126 * r + 0.7152 * g + 0.0722 * b;
                *count += 1;
            }
        }
    }
    let means: Vec<f64> = regions
        .iter()
        .map(|(_, luma, count)| {
            if *count == 0 {
                0.0
            } else {
                *luma / *count as f64
            }
        })
        .collect();
    Ok((means[0], means[1], means[2]))
}

fn write_capture_png(frame: &CapturedFrameRgba8) -> Result<PathBuf, Box<dyn Error>> {
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let path = std::env::temp_dir().join(format!("yuyib_specular_ibl_smoke_{stamp}.png"));
    write_png_rgba8(&path, frame.width(), frame.height(), frame.pixels())?;
    Ok(path)
}
