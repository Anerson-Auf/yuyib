//! M4.2–M4.6 windowed demo: Rapier dynamics + batched unlit cubes.
//!
//! Fixed ground, kinematic platform, trigger, pendulum, prismatic slider,
//! convex hull, and rope tether. Uses
//! [`MeshRenderer3d::draw_batch_depth_clear_double_sided`] and
//! [`DynamicsFixedStepper3d`].
//!
//! ```text
//! cargo run -p yuyib --example physics_rapier_window --features "app,three-d,physics-rapier"
//! ```
//!
//! Controls: WASD + mouse look, Esc exits.
//! Does **not** touch [`yuyib::physics::TriangleMesh3d`] / playable character collision.

use std::{cell::RefCell, error::Error, rc::Rc};

use yuyib::{
    app::{Application, RenderLoop},
    input::{FreeCameraConfig3d, FreeCameraController3d},
    model::MeshPrimitive,
    physics::{
        BodyId3d, DynamicsBackend3d, DynamicsFixedStepper3d, DynamicsWorldConfig3d,
        RapierDynamicsWorld3d,
    },
    platform::{CursorControl, WindowConfig},
    render::ClearColor,
    render_3d::MeshRenderer3d,
};

const MESH_HALF: f32 = 0.5;

struct BodyVisual {
    id: BodyId3d,
    /// Physics half-extents (sphere → `[r,r,r]`, capsule → `[r, half_height+r, r]`).
    half_extents: [f32; 3],
    color: [f32; 4],
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut world = RapierDynamicsWorld3d::new(DynamicsWorldConfig3d::earth_60hz())?;
    let ground = world.insert_fixed_cuboid([0.0, -0.25, 0.0], [8.0, 0.25, 8.0])?;

    let mut visuals = Vec::new();
    visuals.push(BodyVisual {
        id: ground,
        half_extents: [8.0, 0.25, 8.0],
        color: [0.45, 0.48, 0.55, 1.0],
    });

    let platform = world.insert_kinematic_cuboid([-2.5, 0.35, 0.0], [1.4, 0.12, 1.4])?;
    visuals.push(BodyVisual {
        id: platform,
        half_extents: [1.4, 0.12, 1.4],
        color: [0.85, 0.75, 0.25, 1.0],
    });
    let rider = world.insert_dynamic_sphere([-2.5, 1.2, 0.0], 0.3)?;
    visuals.push(BodyVisual {
        id: rider,
        half_extents: [0.3, 0.3, 0.3],
        color: [0.95, 0.4, 0.2, 1.0],
    });

    let trigger = world.insert_trigger_cuboid([2.2, 1.0, 0.0], [0.7, 1.0, 0.7])?;
    visuals.push(BodyVisual {
        id: trigger,
        half_extents: [0.7, 1.0, 0.7],
        color: [0.2, 0.9, 0.55, 1.0],
    });

    // Pendulum: fixed pivot + revolute bob (Z axis).
    let pivot = world.insert_fixed_cuboid([3.5, 3.2, -1.5], [0.12, 0.12, 0.12])?;
    visuals.push(BodyVisual {
        id: pivot,
        half_extents: [0.12, 0.12, 0.12],
        color: [0.7, 0.7, 0.75, 1.0],
    });
    let bob = world.insert_dynamic_sphere([5.0, 3.2, -1.5], 0.28)?;
    visuals.push(BodyVisual {
        id: bob,
        half_extents: [0.28, 0.28, 0.28],
        color: [0.95, 0.25, 0.55, 1.0],
    });
    let _pendulum = world.insert_revolute_joint(
        pivot,
        bob,
        [0.0, 0.0, 1.0],
        [0.0, 0.0, 0.0],
        [-1.5, 0.0, 0.0],
    )?;

    // Prismatic slider along X (visual cube approximates the body).
    let rail = world.insert_fixed_cuboid([-3.5, 1.8, 2.0], [0.15, 0.15, 0.15])?;
    visuals.push(BodyVisual {
        id: rail,
        half_extents: [0.15, 0.15, 0.15],
        color: [0.6, 0.65, 0.7, 1.0],
    });
    let slider = world.insert_dynamic_cuboid([-3.5, 1.8, 2.0], [0.3, 0.2, 0.2])?;
    visuals.push(BodyVisual {
        id: slider,
        half_extents: [0.3, 0.2, 0.2],
        color: [0.3, 0.75, 0.95, 1.0],
    });
    let _slide = world.insert_prismatic_joint(
        rail,
        slider,
        [1.0, 0.0, 0.0],
        [0.0, 0.0, 0.0],
        [0.0, 0.0, 0.0],
        Some([-1.2, 1.2]),
    )?;
    world.set_linear_velocity(slider, [2.5, 0.0, 0.0])?;

    // Convex diamond (drawn as a stretched cube; collision is the true hull).
    let diamond = [
        [0.45_f32, 0.0, 0.0],
        [-0.45, 0.0, 0.0],
        [0.0, 0.55, 0.0],
        [0.0, -0.55, 0.0],
        [0.0, 0.0, 0.45],
        [0.0, 0.0, -0.45],
    ];
    let hull = world.insert_dynamic_convex_hull([1.5, 3.5, 1.5], &diamond)?;
    visuals.push(BodyVisual {
        id: hull,
        half_extents: [0.45, 0.55, 0.45],
        color: [0.95, 0.9, 0.35, 1.0],
    });

    // Rope-tethered bob (max distance 1.4).
    let rope_anchor = world.insert_fixed_cuboid([-4.0, 3.5, -2.0], [0.12, 0.12, 0.12])?;
    visuals.push(BodyVisual {
        id: rope_anchor,
        half_extents: [0.12, 0.12, 0.12],
        color: [0.55, 0.55, 0.6, 1.0],
    });
    let rope_bob = world.insert_dynamic_sphere([-4.0, 1.8, -2.0], 0.28)?;
    visuals.push(BodyVisual {
        id: rope_bob,
        half_extents: [0.28, 0.28, 0.28],
        color: [0.85, 0.45, 0.95, 1.0],
    });
    let _rope = world.insert_rope_joint(
        rope_anchor,
        rope_bob,
        1.4,
        [0.0, 0.0, 0.0],
        [0.0, 0.0, 0.0],
    )?;

    let boxes = [
        ([0.0_f32, 2.8, 0.2], [0.35_f32, 0.35, 0.35], [0.95, 0.35, 0.15, 1.0]),
        ([0.5, 3.6, -0.15], [0.4, 0.25, 0.4], [0.2, 0.55, 0.95, 1.0]),
        ([-0.4, 4.4, 0.1], [0.3, 0.5, 0.3], [0.25, 0.9, 0.35, 1.0]),
    ];
    for (center, half, color) in boxes {
        let id = world.insert_dynamic_cuboid(center, half)?;
        visuals.push(BodyVisual {
            id,
            half_extents: half,
            color,
        });
    }

    let spheres = [
        ([1.2_f32, 3.0, -1.0], 0.35_f32, [0.95, 0.35, 0.75, 1.0]),
        ([-1.0, 4.5, 0.8], 0.35, [0.65, 0.35, 0.95, 1.0]),
    ];
    for (center, radius, color) in spheres {
        let id = world.insert_dynamic_sphere(center, radius)?;
        world.set_linear_velocity(id, [0.2, 0.0, -0.15])?;
        visuals.push(BodyVisual {
            id,
            half_extents: [radius, radius, radius],
            color,
        });
    }

    let capsule = world.insert_dynamic_capsule([0.8, 5.0, -0.4], 0.4, 0.2)?;
    world.set_linear_velocity(capsule, [-0.3, 0.0, 0.25])?;
    visuals.push(BodyVisual {
        id: capsule,
        half_extents: [0.2, 0.4 + 0.2, 0.2],
        color: [0.95, 0.55, 0.15, 1.0],
    });

    let cube = MeshPrimitive::cube(MESH_HALF)?;

    let world = Rc::new(RefCell::new(world));
    let visuals = Rc::new(visuals);
    let stepper = Rc::new(RefCell::new(DynamicsFixedStepper3d::hz60()));
    let sim_time = Rc::new(RefCell::new(0.0_f32));
    let trigger_latched = Rc::new(RefCell::new(false));
    let contact_latched = Rc::new(RefCell::new(false));

    let camera = Rc::new(RefCell::new(FreeCameraController3d::looking_at(
        FreeCameraConfig3d {
            move_speed: 5.0,
            ..FreeCameraConfig3d::default()
        },
        [9.0, 6.5, 11.0],
        [0.0, 1.0, 0.0],
    )?));
    let window_camera = Rc::clone(&camera);
    let device_camera = Rc::clone(&camera);
    let update_camera = Rc::clone(&camera);
    let render_camera = Rc::clone(&camera);
    let cursor_ready = Rc::new(RefCell::new(false));
    let update_cursor = Rc::clone(&cursor_ready);
    let step_world = Rc::clone(&world);
    let step_stepper = Rc::clone(&stepper);
    let step_time = Rc::clone(&sim_time);
    let step_trigger = Rc::clone(&trigger_latched);
    let step_contact = Rc::clone(&contact_latched);
    let draw_world = Rc::clone(&world);
    let draw_visuals = Rc::clone(&visuals);

    println!(
        "physics_rapier_window: platform, trigger, pendulum, slider, convex, rope. \
         Console logs trigger enter/exit and first ground contact. WASD + mouse, Esc."
    );

    Application::new()
        .window(WindowConfig {
            title: "Yuyib — Rapier dynamics (M4.6)".to_owned(),
            ..Default::default()
        })
        .clear_color(ClearColor::linear(0.12, 0.14, 0.18, 1.0))
        .render_loop(RenderLoop::Continuous)
        .cursor_control(CursorControl::Released)
        .on_window_event(move |event, context| {
            let result = window_camera.borrow_mut().handle_window_event(event);
            if let Some(cursor) = result.cursor_control {
                context.set_cursor_control(cursor);
            }
            if result.exit_requested {
                context.request_exit();
            }
        })
        .on_device_event(move |event, _context| {
            let _ = device_camera.borrow_mut().handle_device_event(event);
        })
        .on_frame(move |context| {
            if !*update_cursor.borrow() {
                context.set_cursor_control(update_camera.borrow().initial_cursor_control());
                *update_cursor.borrow_mut() = true;
            }
            let delta = context.frame().delta.as_secs_f32().clamp(0.0, 0.1);
            if let Err(error) = update_camera.borrow_mut().step(delta) {
                eprintln!("physics_rapier_window camera: {error}");
            }

            let mut stepper = step_stepper.borrow_mut();
            let steps = stepper.drain_steps(delta);
            let fixed_dt = stepper.fixed_dt();
            drop(stepper);
            for _ in 0..steps {
                let mut time = step_time.borrow_mut();
                *time += fixed_dt;
                let omega = 0.7_f32;
                let amp = 2.2_f32;
                let vx = (*time * omega).cos() * amp * omega;
                let mut world = step_world.borrow_mut();
                if let Err(error) = world.set_linear_velocity(platform, [vx, 0.0, 0.0]) {
                    eprintln!("physics_rapier_window platform vel: {error}");
                }
                if let Err(error) = world.step(Some(fixed_dt)) {
                    eprintln!("physics_rapier_window step: {error}");
                    break;
                }
                let hit = world
                    .collect_trigger_overlaps()
                    .iter()
                    .any(|(t, _)| *t == trigger);
                let mut latched = step_trigger.borrow_mut();
                if hit && !*latched {
                    println!("physics_rapier_window: ENTER trigger");
                    *latched = true;
                } else if !hit && *latched {
                    println!("physics_rapier_window: EXIT trigger");
                    *latched = false;
                }
                drop(latched);
                if !*step_contact.borrow() {
                    let contacts = world.collect_contact_pairs();
                    if !contacts.is_empty() {
                        println!(
                            "physics_rapier_window: first solid contact ({} pairs)",
                            contacts.len()
                        );
                        *step_contact.borrow_mut() = true;
                    }
                }
            }
        })
        .on_render(move |frame| {
            let meshes = MeshRenderer3d::new_for_frame(frame);
            let gpu_cube = match meshes.upload_mesh_for_frame(frame, &cube) {
                Ok(mesh) => mesh,
                Err(error) => {
                    eprintln!("physics_rapier_window upload: {error}");
                    return;
                }
            };
            let camera = render_camera.borrow().camera();
            let world = draw_world.borrow();

            let mut draws = Vec::with_capacity(draw_visuals.len());
            let mut matrices = Vec::with_capacity(draw_visuals.len());
            for visual in draw_visuals.iter() {
                let Some(translation) = world.translation(visual.id) else {
                    continue;
                };
                let Some(rotation) = world.rotation_xyzw(visual.id) else {
                    continue;
                };
                let scale = [
                    visual.half_extents[0] / MESH_HALF,
                    visual.half_extents[1] / MESH_HALF,
                    visual.half_extents[2] / MESH_HALF,
                ];
                let Ok(model) = trs_matrix_xyzw(translation, rotation, scale) else {
                    continue;
                };
                matrices.push((model, visual.color));
            }
            for (model, color) in &matrices {
                draws.push((&gpu_cube, *model, *color));
            }

            if let Err(error) =
                meshes.draw_batch_depth_clear_double_sided(frame, camera, &draws)
            {
                eprintln!("physics_rapier_window draw: {error}");
            }
        })
        .run()?;
    Ok(())
}

/// Column-major `T * R * S` from translation, quaternion `[x,y,z,w]`, and scale.
fn trs_matrix_xyzw(
    translation: [f32; 3],
    quat_xyzw: [f32; 4],
    scale: [f32; 3],
) -> Result<[f32; 16], &'static str> {
    if translation.iter().any(|v| !v.is_finite())
        || quat_xyzw.iter().any(|v| !v.is_finite())
        || scale.iter().any(|v| !v.is_finite())
        || scale.contains(&0.0)
    {
        return Err("non-finite TRS");
    }
    let [x, y, z, w] = quat_xyzw;
    let xx = x * x;
    let yy = y * y;
    let zz = z * z;
    let xy = x * y;
    let xz = x * z;
    let yz = y * z;
    let wx = w * x;
    let wy = w * y;
    let wz = w * z;

    let c0 = [
        (1.0 - 2.0 * (yy + zz)) * scale[0],
        (2.0 * (xy + wz)) * scale[0],
        (2.0 * (xz - wy)) * scale[0],
        0.0,
    ];
    let c1 = [
        (2.0 * (xy - wz)) * scale[1],
        (1.0 - 2.0 * (xx + zz)) * scale[1],
        (2.0 * (yz + wx)) * scale[1],
        0.0,
    ];
    let c2 = [
        (2.0 * (xz + wy)) * scale[2],
        (2.0 * (yz - wx)) * scale[2],
        (1.0 - 2.0 * (xx + yy)) * scale[2],
        0.0,
    ];
    Ok([
        c0[0], c0[1], c0[2], c0[3], c1[0], c1[1], c1[2], c1[3], c2[0], c2[1], c2[2], c2[3],
        translation[0], translation[1], translation[2], 1.0,
    ])
}
