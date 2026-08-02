//! Optional Rapier dynamics overlay for the street-city playable (M4.7–M4.9).
//!
//! Side-by-side with [`yuyib::physics::TriangleMesh3d`] character collision:
//! props live in a local Rapier world. Prefer a **budgeted local slice** of the
//! solid collision mesh so crates collide with nearby walls/floors. An invisible
//! **kinematic sphere** tracks the mesh character each fixed tick so the player
//! can push props (one-way). The character proxy collides with **props only**,
//! never with the map trimesh — map collision for the player stays on the mesh
//! query path (avoids a dense sphere↔trimesh narrowphase every tick).
//!
//! Enable with `--features physics-rapier`. Without the feature this module is a
//! no-op stub so default playable builds stay light.

use std::error::Error;

#[cfg(feature = "physics-rapier")]
use yuyib::{
    model::MeshPrimitive,
    physics::{
        BodyId3d, CollisionGroups3d, DynamicsBackend3d, DynamicsWorldConfig3d,
        RapierDynamicsWorld3d, TriangleMesh3d,
    },
    render::RenderFrame,
    render_3d::{Camera3d, DepthLoad, MeshRenderer3d},
};

#[cfg(not(feature = "physics-rapier"))]
use yuyib::{render::RenderFrame, render_3d::Camera3d};

/// Local-space cube half-extent shared by all prop visuals.
#[cfg(feature = "physics-rapier")]
const MESH_HALF: f32 = 0.5;

/// Soft XZ radius when selecting solid faces for the Rapier map collider.
#[cfg(feature = "physics-rapier")]
const MAP_TRIMESH_CAPTURE_RADIUS: f32 = 16.0;

/// Hard cap on faces inserted into Rapier (closest-to-spawn first).
///
/// Dense `all_geometry` lab meshes easily exceed this inside a few metres; the
/// budget keeps narrowphase bounded for a handful of dynamic props.
#[cfg(feature = "physics-rapier")]
const MAP_TRIMESH_MAX_FACES: usize = 4_096;

/// Collision layer: fixed map geometry (trimesh / proxy floor).
#[cfg(feature = "physics-rapier")]
const GROUP_MAP: u32 = 1;
/// Collision layer: dynamic props.
#[cfg(feature = "physics-rapier")]
const GROUP_PROP: u32 = 2;
/// Collision layer: kinematic character proxy (props only).
#[cfg(feature = "physics-rapier")]
const GROUP_CHAR: u32 = 4;

/// Rapier props demo layered next to mesh character collision.
pub struct PlayableDynamicsOverlay {
    #[cfg(feature = "physics-rapier")]
    inner: Option<OverlayInner>,
}

#[cfg(feature = "physics-rapier")]
struct OverlayInner {
    world: RapierDynamicsWorld3d,
    visuals: Vec<PropVisual>,
    /// Invisible kinematic sphere that mirrors the mesh character centre.
    character_proxy: BodyId3d,
    cube: MeshPrimitive,
}

#[cfg(feature = "physics-rapier")]
struct PropVisual {
    id: BodyId3d,
    half_extents: [f32; 3],
    color: [f32; 4],
}

impl PlayableDynamicsOverlay {
    /// Creates a no-op overlay when Rapier is disabled.
    ///
    /// # Errors
    ///
    /// Never fails without the Rapier feature.
    #[cfg(not(feature = "physics-rapier"))]
    pub fn around_spawn(
        _spawn: [f32; 3],
        _character_radius: f32,
    ) -> Result<Self, Box<dyn Error>> {
        Ok(Self {})
    }

    /// Creates a no-op overlay when Rapier is disabled.
    ///
    /// # Errors
    ///
    /// Never fails without the Rapier feature.
    #[cfg(not(feature = "physics-rapier"))]
    pub fn around_spawn_with_solid_mesh(
        _spawn: [f32; 3],
        _character_radius: f32,
        _solid: &yuyib::physics::TriangleMesh3d,
    ) -> Result<Self, Box<dyn Error>> {
        Ok(Self {})
    }

    /// Spawns a proxy floor, dynamic props, and a kinematic character proxy.
    ///
    /// Prefer [`Self::around_spawn_with_solid_mesh`] in the playable so props
    /// collide with map walls. This entry stays for headless smoke without a city
    /// mesh.
    ///
    /// # Errors
    ///
    /// Returns dynamics/mesh errors when Rapier setup fails.
    #[cfg(feature = "physics-rapier")]
    pub fn around_spawn(
        spawn: [f32; 3],
        character_radius: f32,
    ) -> Result<Self, Box<dyn Error>> {
        Self::build(spawn, character_radius, None)
    }

    /// Same as [`Self::around_spawn`], but inserts a budgeted fixed trimesh from
    /// the solid collision layer so props hit nearby walls/floors (M4.9). Falls
    /// back to a proxy floor if the mesh slice is empty or Rapier rejects it.
    ///
    /// # Errors
    ///
    /// Returns dynamics/mesh errors when Rapier setup fails.
    #[cfg(feature = "physics-rapier")]
    pub fn around_spawn_with_solid_mesh(
        spawn: [f32; 3],
        character_radius: f32,
        solid: &TriangleMesh3d,
    ) -> Result<Self, Box<dyn Error>> {
        Self::build(spawn, character_radius, Some(solid))
    }

    #[cfg(feature = "physics-rapier")]
    fn build(
        spawn: [f32; 3],
        character_radius: f32,
        solid: Option<&TriangleMesh3d>,
    ) -> Result<Self, Box<dyn Error>> {
        let mut world = RapierDynamicsWorld3d::new(DynamicsWorldConfig3d::earth_60hz())?;
        let floor_top = spawn[1] - character_radius;
        let mut visuals = Vec::new();
        let mut used_proxy_floor = false;
        let mut map_body: Option<BodyId3d> = None;

        if let Some(mesh) = solid {
            let slice = local_trimesh_from_mesh(
                mesh,
                spawn,
                MAP_TRIMESH_CAPTURE_RADIUS,
                MAP_TRIMESH_MAX_FACES,
            );
            if !slice.vertices.is_empty()
                && !slice.indices.is_empty()
                && let Ok(body) = world.insert_fixed_trimesh(&slice.vertices, &slice.indices)
            {
                map_body = Some(body);
                eprintln!(
                    "playable Rapier overlay: solid map trimesh ({} / {} candidates within \
                     {:.0}m XZ, cap {}); character↔map contacts disabled",
                    slice.indices.len(),
                    slice.candidates,
                    MAP_TRIMESH_CAPTURE_RADIUS,
                    MAP_TRIMESH_MAX_FACES
                );
            } else {
                eprintln!(
                    "playable Rapier overlay: solid trimesh unavailable \
                     (candidates={}); falling back to proxy floor",
                    slice.candidates
                );
                used_proxy_floor = true;
            }
        } else {
            used_proxy_floor = true;
        }

        if used_proxy_floor {
            let floor_half = 0.2_f32;
            let floor = world.insert_fixed_cuboid(
                [spawn[0], floor_top - floor_half, spawn[2]],
                [6.0, floor_half, 6.0],
            )?;
            map_body = Some(floor);
            visuals.push(PropVisual {
                id: floor,
                half_extents: [6.0, floor_half, 6.0],
                color: [0.35, 0.38, 0.42, 1.0],
            });
        }

        if let Some(map) = map_body {
            world.set_collision_groups(map, CollisionGroups3d::new(GROUP_MAP, GROUP_PROP))?;
        }

        // Place crates in front of a typical −Z spawn facing so they are easy to find.
        let props = [
            (
                [spawn[0] + 0.6, floor_top + 0.55, spawn[2] - 2.2],
                [0.28_f32, 0.28, 0.28],
                [0.95, 0.4, 0.2, 1.0],
            ),
            (
                [spawn[0] - 0.5, floor_top + 0.45, spawn[2] - 2.8],
                [0.35, 0.2, 0.35],
                [0.25, 0.65, 0.95, 1.0],
            ),
            (
                [spawn[0] + 1.2, floor_top + 0.7, spawn[2] - 1.6],
                [0.22, 0.4, 0.22],
                [0.4, 0.9, 0.35, 1.0],
            ),
        ];
        let prop_filter = GROUP_MAP | GROUP_PROP | GROUP_CHAR;
        for (center, half, color) in props {
            let id = world.insert_dynamic_cuboid(center, half)?;
            world.set_collision_groups(id, CollisionGroups3d::new(GROUP_PROP, prop_filter))?;
            visuals.push(PropVisual {
                id,
                half_extents: half,
                color,
            });
        }
        let ball = world.insert_dynamic_sphere(
            [spawn[0], floor_top + 1.2, spawn[2] - 2.0],
            0.3,
        )?;
        world.set_collision_groups(ball, CollisionGroups3d::new(GROUP_PROP, prop_filter))?;
        visuals.push(PropVisual {
            id: ball,
            half_extents: [0.3, 0.3, 0.3],
            color: [0.95, 0.75, 0.2, 1.0],
        });

        let character_proxy =
            world.insert_kinematic_position_sphere(spawn, character_radius)?;
        world.set_collision_groups(
            character_proxy,
            CollisionGroups3d::new(GROUP_CHAR, GROUP_PROP),
        )?;

        let prop_count = if used_proxy_floor {
            visuals.len().saturating_sub(1)
        } else {
            visuals.len()
        };
        eprintln!(
            "playable Rapier overlay: {prop_count} props + kinematic character sphere \
             (one-way push; mesh character unchanged)"
        );

        Ok(Self {
            inner: Some(OverlayInner {
                world,
                visuals,
                character_proxy,
                cube: MeshPrimitive::cube(MESH_HALF)?,
            }),
        })
    }

    /// Syncs the kinematic character proxy, then advances Rapier one fixed tick.
    ///
    /// `character_center` is the mesh character sphere centre (same as
    /// [`yuyib::character_3d::CharacterController3d::position`]).
    #[allow(clippy::unused_self, reason = "self is used under physics-rapier")]
    pub fn step(&mut self, fixed_dt: f32, character_center: [f32; 3]) {
        #[cfg(feature = "physics-rapier")]
        if let Some(inner) = self.inner.as_mut()
            && fixed_dt.is_finite()
            && fixed_dt > 0.0
            && character_center.iter().all(|channel| channel.is_finite())
        {
            let _ = inner
                .world
                .set_next_kinematic_translation(inner.character_proxy, character_center);
            let _ = inner.world.step(Some(fixed_dt));
        }
        #[cfg(not(feature = "physics-rapier"))]
        {
            let _ = (fixed_dt, character_center);
        }
    }

    /// Draws prop meshes after the city/character passes (no-op without the feature).
    ///
    /// # Errors
    ///
    /// Returns render errors when mesh upload/draw fails.
    pub fn draw(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
    ) -> Result<(), Box<dyn Error>> {
        #[cfg(feature = "physics-rapier")]
        if let Some(inner) = self.inner.as_ref() {
            let meshes = MeshRenderer3d::new_for_frame(frame);
            let gpu_cube = meshes.upload_mesh_for_frame(frame, &inner.cube)?;
            let mut draws = Vec::with_capacity(inner.visuals.len());
            let mut matrices = Vec::with_capacity(inner.visuals.len());
            for visual in &inner.visuals {
                let Some(translation) = inner.world.translation(visual.id) else {
                    continue;
                };
                let Some(rotation) = inner.world.rotation_xyzw(visual.id) else {
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
            meshes.draw_batch_with_depth_load_double_sided(
                frame,
                camera,
                &draws,
                DepthLoad::Load,
            )?;
        }
        #[cfg(not(feature = "physics-rapier"))]
        {
            let _ = (frame, camera);
        }
        Ok(())
    }

    /// Returns whether the Rapier overlay is active.
    #[must_use]
    #[allow(dead_code, reason = "used by playable_dynamics_overlay_smoke")]
    pub fn is_active(&self) -> bool {
        #[cfg(feature = "physics-rapier")]
        {
            self.inner.is_some()
        }
        #[cfg(not(feature = "physics-rapier"))]
        {
            false
        }
    }
}

#[cfg(feature = "physics-rapier")]
struct LocalTrimeshSlice {
    vertices: Vec<[f32; 3]>,
    indices: Vec<[u32; 3]>,
    candidates: usize,
}

/// Copies non-degenerate faces near `center` on XZ, closest-first, up to `max_faces`.
#[cfg(feature = "physics-rapier")]
fn local_trimesh_from_mesh(
    mesh: &TriangleMesh3d,
    center: [f32; 3],
    radius_xz: f32,
    max_faces: usize,
) -> LocalTrimeshSlice {
    let r2 = radius_xz * radius_xz;
    let mut ranked: Vec<(f32, [[f32; 3]; 3])> = Vec::new();
    for face in mesh.triangles() {
        let mut near = false;
        let mut min_d2 = f32::INFINITY;
        for vertex in face {
            let dx = vertex.x - center[0];
            let dz = vertex.z - center[2];
            let d2 = dx * dx + dz * dz;
            if d2 < min_d2 {
                min_d2 = d2;
            }
            if d2 <= r2 {
                near = true;
            }
        }
        if !near {
            continue;
        }
        let abx = face[1].x - face[0].x;
        let aby = face[1].y - face[0].y;
        let abz = face[1].z - face[0].z;
        let acx = face[2].x - face[0].x;
        let acy = face[2].y - face[0].y;
        let acz = face[2].z - face[0].z;
        let cx = aby * acz - abz * acy;
        let cy = abz * acx - abx * acz;
        let cz = abx * acy - aby * acx;
        if cx * cx + cy * cy + cz * cz < 1.0e-12 {
            continue;
        }
        ranked.push((
            min_d2,
            [
                [face[0].x, face[0].y, face[0].z],
                [face[1].x, face[1].y, face[1].z],
                [face[2].x, face[2].y, face[2].z],
            ],
        ));
    }
    let candidates = ranked.len();
    ranked.sort_by(|a, b| a.0.total_cmp(&b.0));
    if ranked.len() > max_faces {
        ranked.truncate(max_faces);
    }

    let mut vertices = Vec::with_capacity(ranked.len() * 3);
    let mut indices = Vec::with_capacity(ranked.len());
    for (_d2, face) in ranked {
        let base = vertices.len() as u32;
        vertices.push(face[0]);
        vertices.push(face[1]);
        vertices.push(face[2]);
        indices.push([base, base + 1, base + 2]);
    }
    LocalTrimeshSlice {
        vertices,
        indices,
        candidates,
    }
}

#[cfg(feature = "physics-rapier")]
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
