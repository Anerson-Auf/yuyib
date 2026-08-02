//! M1 smoke: playable map + animated playermodel → fixed-camera PNG.
//!
//! No window is opened. The example:
//!
//! 1. loads the active map profile (`sci-fi_lab.glb`) through [`GltfSceneLoad`];
//! 2. asserts semantic `solid` / `street` colliders and a grounded spawn;
//! 3. loads the skeletal character, advances the walk clip, and asserts pose moved;
//! 4. publishes map + character GPU residency through [`OffscreenRenderer`];
//! 5. draws map then skinned character at a fixed chase-like pose and captures PNG.
//!
//! ```text
//! cargo run -p yuyib --example street_city_m1_smoke
//! ```
//!
//! Requires both GLBs under `for_tests/` and a compatible DX12/Vulkan adapter.

#[path = "support/playable_character.rs"]
mod playable_character;
#[path = "support/street_city.rs"]
mod street_city;

use std::{
    error::Error,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use yuyib::{
    assets::Assets,
    character_3d::{CharacterController3d, CharacterControllerConfig3d, CharacterModelPlacement3d},
    image::{Rgba8ReferenceMetrics, reference_metrics_rgba8, write_png_rgba8},
    physics::Vec2,
    render::{CapturedFrameRgba8, ClearColor, OffscreenRenderer},
    render_3d::{
        Camera3d, DepthLoad, GltfSceneLoad, ModelUploadBudget3d, SkeletalTextureResources,
        TexturedSkeletalSceneRenderer3d,
    },
    render_texture::TextureCache,
};

use playable_character::{
    CHARACTER_CONTROLLER_RADIUS, CHARACTER_FILE, CHARACTER_MODEL_SCALE,
    CHARACTER_SMOKE_ADVANCE_SECONDS, bind_and_advanced_walk_poses,
    character_material_texture_summary, load_prepared_character, pose_translation_delta,
};
use street_city::{
    MAP_FILE, create_renderer, load_config, map_path, spawn_options_for_street,
    wait_for_loaded_map,
};

const WIDTH: u32 = 640;
const HEIGHT: u32 = 360;
const CLEAR: [f64; 4] = [0.45, 0.58, 0.72, 1.0];
const LOAD_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_PREPARE_FRAMES: usize = 512;
const CAMERA_OFFSET: [f32; 3] = [4.0, 2.5, 6.0];
const MIN_POSE_TRANSLATION_DELTA: f32 = 0.01;
const CHARACTER_TEXTURE_SLOTS_PER_FRAME: usize = 8;
const CHARACTER_TEXTURE_BYTES_PER_FRAME: u64 = 32 * 1024 * 1024;
const NON_CLEAR_DELTA_THRESHOLD: u8 = 12;
const MIN_MEAN_NON_CLEAR_LUMINANCE: f32 = 0.05;
const MAX_MEAN_NON_CLEAR_LUMINANCE: f32 = 0.85;

fn main() -> Result<(), Box<dyn Error>> {
    let asset_root = street_city::asset_root();
    let map_file = map_path(&asset_root);
    if !map_file.is_file() {
        return Err(format!(
            "missing street-city fixture at {} — place {MAP_FILE} under for_tests/",
            map_file.display()
        )
        .into());
    }

    let mut loading = GltfSceneLoad::start(map_file, load_config(&asset_root)?)?;
    let mut map = wait_for_loaded_map(&mut loading, LOAD_TIMEOUT)?;
    for diagnostic in map.diagnostics() {
        eprintln!(
            "city import {:?}: {} — {}",
            diagnostic.severity, diagnostic.code, diagnostic.message
        );
    }

    let solid_id = street_city::solid_layer_id()?;
    let street_id = street_city::street_layer_id()?;
    let solid = map
        .collider_layer(&solid_id)
        .ok_or("street-city smoke: solid collider layer missing")?;
    let street = map
        .collider_layer(&street_id)
        .ok_or("street-city smoke: street collider layer missing")?;

    let (controller, spawn_report) = CharacterController3d::spawn_on_surface_mesh_with_report(
        CharacterControllerConfig3d {
            radius: CHARACTER_CONTROLLER_RADIUS,
            ..CharacterControllerConfig3d::default()
        },
        street.mesh(),
        solid.mesh(),
        spawn_options_for_street(street.mesh()),
    )?;
    if !controller.is_grounded() {
        return Err("street-city smoke: spawn controller is not grounded".into());
    }
    let spawn = controller.position();
    let selected = spawn_report
        .selected
        .ok_or("street-city smoke: spawn report missing selected triangle")?;
    eprintln!(
        "street_city_m1_smoke: grounded spawn at ({:.2}, {:.2}, {:.2}) \
         triangle={} floor=({:.2},{:.2},{:.2}) ranked={} rejects={} \
         (steep={} radius={} contacts={} ceiling={} headroom={} sky={})",
        spawn.x,
        spawn.y,
        spawn.z,
        selected.triangle,
        selected.floor.x,
        selected.floor.y,
        selected.floor.z,
        spawn_report.ranked_candidate_count,
        spawn_report.reject_counts.total(),
        spawn_report.reject_counts.steep_normal,
        spawn_report.reject_counts.outside_horizontal_radius,
        spawn_report.reject_counts.sphere_contacts,
        spawn_report.reject_counts.missing_ceiling,
        spawn_report.reject_counts.low_headroom,
        spawn_report.reject_counts.insufficient_open_sky,
    );

    let (character_asset, character_textures) = load_prepared_character(&asset_root)?;
    eprintln!(
        "street_city_m1_smoke: {}",
        character_material_texture_summary(&character_asset)
    );
    let (bind_pose, walk_pose) = bind_and_advanced_walk_poses(&character_asset)?;
    let pose_delta = pose_translation_delta(&bind_pose, &walk_pose);
    eprintln!(
        "street_city_m1_smoke: {CHARACTER_FILE} walk advance {CHARACTER_SMOKE_ADVANCE_SECONDS:.2}s pose Δtranslation={pose_delta:.4}"
    );
    if pose_delta < MIN_POSE_TRANSLATION_DELTA {
        return Err(format!(
            "street-city smoke: walk pose did not move enough after advance (Δ={pose_delta:.5}, need ≥ {MIN_POSE_TRANSLATION_DELTA})"
        )
        .into());
    }

    let root = CharacterModelPlacement3d::from_controller(
        controller,
        Vec2::new(0.0, -1.0),
        CHARACTER_MODEL_SCALE,
    )?
    .model_to_world();

    let mut renderer = create_renderer(&asset_root)?;
    let focus = [spawn.x, spawn.y + 1.1, spawn.z];
    let camera = Camera3d::new(
        [
            focus[0] + CAMERA_OFFSET[0],
            focus[1] + CAMERA_OFFSET[1],
            focus[2] + CAMERA_OFFSET[2],
        ],
        focus,
        [0.0, 1.0, 0.0],
        55.0_f32.to_radians(),
        0.08,
        2_000.0,
    );
    *renderer.camera_mut() = camera;

    let mut gpu = OffscreenRenderer::new(WIDTH, HEIGHT)?;
    let clear = ClearColor::linear(CLEAR[0], CLEAR[1], CLEAR[2], CLEAR[3]);
    let budget = ModelUploadBudget3d {
        maximum_texture_slots: 64,
        target_texture_bytes: 64 * 1024 * 1024,
        maximum_primitives: 128,
        target_geometry_bytes: 64 * 1024 * 1024,
    };

    let mut textures = Assets::new();
    let mut gpu_textures = TextureCache::new();
    let mut prepared_character = Some(character_textures);
    let mut character_bindings = None;
    let mut character_renderer = None;
    let mut map_ready = false;
    let mut last_error = None;

    for frame_index in 0..MAX_PREPARE_FRAMES {
        gpu.render_frame(clear, |frame| {
            match map.prepare_for_frame_with_budget(frame, &mut renderer, budget) {
                Ok(progress) => {
                    map_ready = progress.ready;
                    if frame_index == 0 || frame_index % 32 == 0 || map_ready {
                        eprintln!(
                            "street_city_m1_smoke: map prepare {frame_index}: ready={} textures={}/{} primitives={}/{}",
                            progress.ready,
                            progress.completed_texture_slots,
                            progress.total_texture_slots,
                            progress.completed_primitives,
                            progress.total_primitives,
                        );
                    }
                }
                Err(error) => last_error = Some(error.to_string()),
            }
            if last_error.is_some() {
                return;
            }
            if character_bindings.is_none()
                && let Some(prepared) = prepared_character.as_mut()
            {
                match prepared.upload_with_budget_for_frame(
                    frame,
                    &mut textures,
                    &mut gpu_textures,
                    CHARACTER_TEXTURE_SLOTS_PER_FRAME,
                    CHARACTER_TEXTURE_BYTES_PER_FRAME,
                ) {
                    Ok(_) => {
                        if prepared.remaining() == 0 {
                            match prepared_character
                                .take()
                                .expect("completed character preparation")
                                .finish()
                            {
                                Ok(bindings) => character_bindings = Some(bindings),
                                Err(error) => last_error = Some(error.to_string()),
                            }
                        }
                    }
                    Err(error) => last_error = Some(error.to_string()),
                }
            }
            if last_error.is_some() {
                return;
            }
            if character_bindings.is_some() && character_renderer.is_none() {
                match TexturedSkeletalSceneRenderer3d::new_for_frame(
                    frame,
                    &character_asset.model,
                    &character_asset.scene,
                ) {
                    Ok(ready_renderer) => {
                        match street_city::character_key_light() {
                            Ok(lighting) => {
                                character_renderer = Some(ready_renderer.with_lighting(lighting));
                            }
                            Err(error) => last_error = Some(error.to_string()),
                        }
                    }
                    Err(error) => last_error = Some(error.to_string()),
                }
            }
        });
        if let Some(error) = last_error.take() {
            return Err(error.into());
        }
        if map_ready && character_bindings.is_some() && character_renderer.is_some() {
            break;
        }
    }
    if !map_ready {
        return Err(format!(
            "street-city smoke: map GPU residency not ready after {MAX_PREPARE_FRAMES} prepare frames"
        )
        .into());
    }
    let character_bindings = character_bindings
        .ok_or("street-city smoke: character textures did not finish GPU publication")?;
    let character_renderer = character_renderer
        .ok_or("street-city smoke: character skeletal renderer was not created")?;

    let mut draw_error = None;
    let captured = gpu.render_and_capture_rgba8(clear, |frame| {
        *renderer.camera_mut() = camera;
        if let Err(error) = map.render(frame, &mut renderer) {
            draw_error = Some(error.to_string());
            return;
        }
        if let Err(error) = character_renderer.draw_with_root_transform_and_depth_load(
            frame,
            camera,
            &character_asset.scene,
            &walk_pose,
            SkeletalTextureResources {
                bindings: &character_bindings,
                textures: &gpu_textures,
            },
            root,
            DepthLoad::Load,
        ) {
            draw_error = Some(error.to_string());
        }
    })?;
    if let Some(error) = draw_error {
        return Err(error.into());
    }

    let metrics = assert_capture_has_sane_reference_metrics(&captured)?;
    let path = write_capture_png(&captured)?;
    println!(
        "street_city_m1_smoke OK: {}x{}, grounded spawn ({:.2},{:.2},{:.2}), walk Δ={:.4}, scale={CHARACTER_MODEL_SCALE}, camera ({:.2},{:.2},{:.2}) -> ({:.2},{:.2},{:.2}), reference non_clear={} mean_luma={:.3} histogram=[{},{},{},{}], wrote {}",
        captured.width(),
        captured.height(),
        spawn.x,
        spawn.y,
        spawn.z,
        pose_delta,
        camera.position[0],
        camera.position[1],
        camera.position[2],
        camera.target[0],
        camera.target[1],
        camera.target[2],
        metrics.non_clear_pixels,
        metrics.mean_non_clear_luminance,
        metrics.non_clear_brightness_histogram[0],
        metrics.non_clear_brightness_histogram[1],
        metrics.non_clear_brightness_histogram[2],
        metrics.non_clear_brightness_histogram[3],
        path.display()
    );
    Ok(())
}

fn assert_capture_has_sane_reference_metrics(
    frame: &CapturedFrameRgba8,
) -> Result<Rgba8ReferenceMetrics, Box<dyn Error>> {
    let clear_u8 = [
        (CLEAR[0] * 255.0).round() as u8,
        (CLEAR[1] * 255.0).round() as u8,
        (CLEAR[2] * 255.0).round() as u8,
        255,
    ];
    let metrics = reference_metrics_rgba8(frame.pixels(), clear_u8, NON_CLEAR_DELTA_THRESHOLD);
    if metrics.non_clear_pixels < 256 {
        return Err(format!(
            "expected a visible street-city + character silhouette; only {} pixels differed from clear {:?}",
            metrics.non_clear_pixels,
            clear_u8
        )
        .into());
    }
    if !(MIN_MEAN_NON_CLEAR_LUMINANCE..=MAX_MEAN_NON_CLEAR_LUMINANCE)
        .contains(&metrics.mean_non_clear_luminance)
    {
        return Err(format!(
            "reference screenshot mean non-clear luminance {:.3} is outside the broad {:.2}..={:.2} band; expected neither all-black nor all-white rendering",
            metrics.mean_non_clear_luminance,
            MIN_MEAN_NON_CLEAR_LUMINANCE,
            MAX_MEAN_NON_CLEAR_LUMINANCE,
        )
        .into());
    }
    Ok(metrics)
}

fn write_capture_png(frame: &CapturedFrameRgba8) -> Result<PathBuf, Box<dyn Error>> {
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let path = std::env::temp_dir().join(format!("yuyib_street_city_m1_smoke_{stamp}.png"));
    write_png_rgba8(&path, frame.width(), frame.height(), frame.pixels())?;
    Ok(path)
}
