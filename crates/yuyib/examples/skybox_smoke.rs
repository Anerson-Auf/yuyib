//! M2 smoke: fullscreen cubemap skybox from a cooked outdoor probe.
//!
//! No window. Cooks the synthetic outdoor equirect, uploads mip0 as a skybox,
//! draws looking +Y then −Y, and asserts sky luma exceeds ground luma.
//!
//! ```text
//! cargo run -p yuyib --example skybox_smoke
//! ```

use std::error::Error;

use yuyib::{
    render::{ClearColor, OffscreenRenderer},
    render_3d::{
        Camera3d, DepthLoad, GgxCookConfig, GpuSkybox3d, PreparedEquirectEnvironment3d,
        PreparedSkybox3d, SkyboxRenderer3d, cook_ggx_specular_ibl,
    },
};

const WIDTH: u32 = 160;
const HEIGHT: u32 = 90;
const CLEAR: [f64; 4] = [0.02, 0.03, 0.05, 1.0];

fn mean_luma(pixels: &[u8]) -> f32 {
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

fn capture_look(
    gpu: &mut OffscreenRenderer,
    sky_cpu: &PreparedSkybox3d,
    target: [f32; 3],
) -> Result<f32, Box<dyn Error>> {
    let camera = Camera3d::new(
        [0.0, 0.0, 0.0],
        target,
        [0.0, 0.0, 1.0],
        70.0_f32.to_radians(),
        0.05,
        64.0,
    );
    let mut draw_error = None;
    let captured = gpu.render_and_capture_rgba8(
        ClearColor::linear(CLEAR[0], CLEAR[1], CLEAR[2], CLEAR[3]),
        |frame| {
            let skies = SkyboxRenderer3d::new_for_frame(frame);
            let sky = GpuSkybox3d::upload_for_frame(frame, sky_cpu);
            if let Err(error) = skies.draw(frame, camera, &sky, DepthLoad::Clear) {
                draw_error = Some(error.to_string());
            }
        },
    )?;
    if let Some(error) = draw_error {
        return Err(error.into());
    }
    Ok(mean_luma(captured.pixels()))
}

fn main() -> Result<(), Box<dyn Error>> {
    let env = PreparedEquirectEnvironment3d::synthetic_outdoor_probe()?;
    let specular = cook_ggx_specular_ibl(&env, GgxCookConfig::smoke())?;
    let sky_cpu = PreparedSkybox3d::from_specular_mip0(&specular)?;

    let mut gpu = OffscreenRenderer::new(WIDTH, HEIGHT)?;
    // Look along +Y / −Y; use +Z as up so the look axis is not parallel to up.
    let up = capture_look(&mut gpu, &sky_cpu, [0.0, 1.0, 0.0])?;
    let down = capture_look(&mut gpu, &sky_cpu, [0.0, -1.0, 0.0])?;
    if up <= down {
        return Err(format!("skybox_smoke: expected +Y luma {up} > -Y luma {down}").into());
    }

    println!(
        "skybox_smoke OK: face={} +Y_luma={up:.1} -Y_luma={down:.1}",
        sky_cpu.face_size()
    );
    Ok(())
}
