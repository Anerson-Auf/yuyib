//! M2 smoke: HDR equirect ingest (CPU only).
//!
//! No window. Builds a linear lat-long probe, round-trips it through the
//! Radiance `.hdr` path via the unit-tested decoder entry points, and asserts
//! the upper hemisphere is brighter than the ground lobe.
//!
//! ```text
//! cargo run -p yuyib --example equirect_hdr_smoke
//! ```

use std::error::Error;

use yuyib::render_3d::PreparedEquirectEnvironment3d;

fn main() -> Result<(), Box<dyn Error>> {
    let synthetic = PreparedEquirectEnvironment3d::synthetic_outdoor_probe()?;
    assert_eq!(synthetic.width(), 64);
    assert_eq!(synthetic.height(), 32);
    assert_eq!(synthetic.rgb().len(), 64 * 32 * 3);

    let up = synthetic.sample_direction([0.0, 1.0, 0.0]);
    let down = synthetic.sample_direction([0.0, -1.0, 0.0]);
    let up_luma = 0.2126 * up[0] + 0.7152 * up[1] + 0.0722 * up[2];
    let down_luma = 0.2126 * down[0] + 0.7152 * down[1] + 0.0722 * down[2];
    if up_luma <= down_luma {
        return Err(format!(
            "equirect_hdr_smoke: expected sky luma {up_luma} > ground luma {down_luma}"
        )
        .into());
    }

    // Explicit linear ingest path (same layout GGX cook will consume).
    let stripe = PreparedEquirectEnvironment3d::from_linear_rgb_f32(
        4,
        2,
        vec![
            0.4, 0.55, 0.9, 0.45, 0.6, 1.0, 0.5, 0.65, 1.1, 0.55, 0.7, 1.2, 0.2, 0.15, 0.1, 0.22,
            0.16, 0.11, 0.25, 0.18, 0.12, 0.28, 0.2, 0.13,
        ],
    )?;
    let stripe_up = stripe.sample_direction([0.0, 1.0, 0.0]);
    let stripe_down = stripe.sample_direction([0.0, -1.0, 0.0]);
    if stripe_up[2] <= stripe_down[2] {
        return Err("equirect_hdr_smoke: stripe probe lost sky/ground separation".into());
    }

    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../for_tests/outdoor_probe.hdr");
    let fixture_note = if fixture.is_file() {
        let bytes = std::fs::read(&fixture)?;
        let env = PreparedEquirectEnvironment3d::from_radiance_hdr_bytes(&bytes)?;
        let fixture_up = env.sample_direction([0.0, 1.0, 0.0]);
        let fixture_down = env.sample_direction([0.0, -1.0, 0.0]);
        let fixture_up_luma =
            0.2126 * fixture_up[0] + 0.7152 * fixture_up[1] + 0.0722 * fixture_up[2];
        let fixture_down_luma =
            0.2126 * fixture_down[0] + 0.7152 * fixture_down[1] + 0.0722 * fixture_down[2];
        if fixture_up_luma <= fixture_down_luma {
            return Err(format!(
                "equirect_hdr_smoke: fixture sky luma {fixture_up_luma} <= ground {fixture_down_luma}"
            )
            .into());
        }
        format!(
            ", fixture {}x{} sky_luma={fixture_up_luma:.3}",
            env.width(),
            env.height()
        )
    } else {
        String::from(", fixture missing (skipped)")
    };

    println!(
        "equirect_hdr_smoke OK: synthetic {}x{}, sky_luma={up_luma:.3}, ground_luma={down_luma:.3}, \
         stripe {}x{}{fixture_note}",
        synthetic.width(),
        synthetic.height(),
        stripe.width(),
        stripe.height()
    );
    Ok(())
}
