//! M1 smoke: headless offscreen capture with a fixed camera and a solid cube.
//!
//! No window is opened. The example:
//!
//! 1. creates [`OffscreenRenderer`] (320×180);
//! 2. draws `MeshPrimitive::cube` through `MeshRenderer3d` with a fixed
//!    [`Camera3d`] pose;
//! 3. captures RGBA8, asserts the frame is not a flat clear colour;
//! 4. writes a PNG under the system temp directory for visual inspection.
//!
//! ```text
//! cargo run -p yuyib --example frame_capture_smoke
//! ```
//!
//! Requires a compatible DX12/Vulkan adapter. No external fixtures.

use std::{
    error::Error,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use yuyib::{
    image::write_png_rgba8,
    model::MeshPrimitive,
    render::{CapturedFrameRgba8, ClearColor, OffscreenRenderer},
    render_3d::{Camera3d, MeshInstance3d, MeshRenderer3d, MeshTransform3d},
};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 180;
const CLEAR: [f64; 4] = [0.02, 0.03, 0.05, 1.0];
const CUBE_COLOR: [f32; 4] = [0.85, 0.55, 0.15, 1.0];

fn main() -> Result<(), Box<dyn Error>> {
    let mut gpu = OffscreenRenderer::new(WIDTH, HEIGHT)?;
    let cube = MeshPrimitive::cube(0.45)?;
    let camera = Camera3d::new(
        [1.6, 1.1, 2.2],
        [0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        55.0_f32.to_radians(),
        0.05,
        32.0,
    );
    let instance = MeshInstance3d::new(
        MeshTransform3d::new([0.0, 0.0, 0.0], [0.35, 0.55, 0.15], [1.0, 1.0, 1.0]),
        CUBE_COLOR,
    );

    let mut draw_error = None;
    let captured = gpu.render_and_capture_rgba8(
        ClearColor::linear(CLEAR[0], CLEAR[1], CLEAR[2], CLEAR[3]),
        |frame| {
            let meshes = MeshRenderer3d::new_for_frame(frame);
            match meshes.upload_mesh_for_frame(frame, &cube) {
                Ok(gpu_cube) => {
                    if let Err(error) = meshes.draw(frame, camera, &gpu_cube, instance) {
                        draw_error = Some(error.to_string());
                    }
                }
                Err(error) => draw_error = Some(error.to_string()),
            }
        },
    )?;
    if let Some(error) = draw_error {
        return Err(error.into());
    }

    assert_capture_has_non_clear_pixels(&captured)?;
    let path = write_capture_png(&captured)?;
    println!(
        "frame_capture_smoke OK: {}x{}, wrote {}",
        captured.width(),
        captured.height(),
        path.display()
    );
    Ok(())
}

fn assert_capture_has_non_clear_pixels(frame: &CapturedFrameRgba8) -> Result<(), Box<dyn Error>> {
    let clear_u8 = [
        (CLEAR[0] * 255.0).round() as u8,
        (CLEAR[1] * 255.0).round() as u8,
        (CLEAR[2] * 255.0).round() as u8,
        255,
    ];
    let mut non_clear = 0_usize;
    for pixel in frame.pixels().chunks_exact(4) {
        let delta = pixel
            .iter()
            .zip(clear_u8)
            .map(|(sample, expected)| sample.abs_diff(expected))
            .max()
            .unwrap_or(0);
        if delta > 8 {
            non_clear += 1;
        }
    }
    if non_clear < 64 {
        return Err(format!(
            "expected a visible cube silhouette; only {non_clear} pixels differed from clear {:?}",
            clear_u8
        )
        .into());
    }
    Ok(())
}

fn write_capture_png(frame: &CapturedFrameRgba8) -> Result<PathBuf, Box<dyn Error>> {
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let path = std::env::temp_dir().join(format!("yuyib_frame_capture_smoke_{stamp}.png"));
    write_png_rgba8(&path, frame.width(), frame.height(), frame.pixels())?;
    Ok(path)
}
