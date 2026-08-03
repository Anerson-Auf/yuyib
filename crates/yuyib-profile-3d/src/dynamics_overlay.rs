//! Opt-in Rapier dynamics overlay next to mesh character collision (M4.7–M4.10 / M5.2).
//!
//! Props live in a local Rapier world. Prefer a **budgeted local slice** of the
//! solid collision mesh so crates collide with nearby walls/floors. An invisible
//! **kinematic sphere** tracks the mesh character each fixed tick so the player
//! can push props. After each Rapier step the overlay returns a soft
//! **reaction displacement** derived from how far props were shoved by the
//! kinematic (two-way without rewriting the mesh motor onto Rapier).
//!
//! Enable with crate feature `rapier` (wired from `yuyib` via `physics-rapier`).
//! Without the feature this type is a no-op stub.

use std::{collections::HashMap, error::Error, fmt};

use yuyib_physics::{BodyId3d, TriangleMesh3d};
use yuyib_render::RenderFrame;
use yuyib_render_3d::Camera3d;

#[cfg(feature = "rapier")]
use yuyib_model::{MeshPrimitive, PrimitiveError};
#[cfg(feature = "rapier")]
use yuyib_physics::{
    CollisionGroups3d, DynamicsBackend3d, DynamicsBackendError3d, DynamicsWorldConfig3d,
    RapierDynamicsWorld3d,
};
#[cfg(feature = "rapier")]
use yuyib_render_3d::{DepthLoad, MeshRenderError, MeshRenderer3d, MeshUploadError};

/// Local-space cube half-extent shared by all prop visuals.
#[cfg(feature = "rapier")]
const MESH_HALF: f32 = 0.5;

/// Soft XZ radius when selecting solid faces for the Rapier map collider.
#[cfg(feature = "rapier")]
const MAP_TRIMESH_CAPTURE_RADIUS: f32 = 24.0;

/// Hard cap on faces inserted into Rapier (closest-to-spawn first).
#[cfg(feature = "rapier")]
const MAP_TRIMESH_MAX_FACES: usize = 24_576;

#[cfg(feature = "rapier")]
const GROUP_MAP: u32 = 1;
#[cfg(feature = "rapier")]
const GROUP_PROP: u32 = 2;
#[cfg(feature = "rapier")]
const GROUP_CHAR: u32 = 4;

#[cfg(feature = "rapier")]
const PROP_REACTION_SHARE: f32 = 0.35;
#[cfg(feature = "rapier")]
const PROP_REACTION_Y_SCALE: f32 = 0.12;
#[cfg(feature = "rapier")]
const PROP_REACTION_MAX: f32 = 0.22;

/// Rapier props demo layered next to mesh character collision.
pub struct DynamicsOverlay3d {
    #[cfg(feature = "rapier")]
    inner: Option<OverlayInner>,
}

#[cfg(feature = "rapier")]
struct OverlayInner {
    world: RapierDynamicsWorld3d,
    visuals: Vec<PropVisual>,
    dynamic_props: Vec<BodyId3d>,
    character_proxy: BodyId3d,
    cube: MeshPrimitive,
    /// Fixed sensor body → semantic trigger id (`level.exit`).
    trigger_ids: HashMap<BodyId3d, String>,
}

#[cfg(feature = "rapier")]
struct PropVisual {
    id: BodyId3d,
    half_extents: [f32; 3],
    color: [f32; 4],
}

impl DynamicsOverlay3d {
    /// Creates a no-op overlay when Rapier is disabled.
    ///
    /// # Errors
    ///
    /// Never fails without the Rapier feature.
    #[cfg(not(feature = "rapier"))]
    pub fn around_spawn(
        _spawn: [f32; 3],
        _character_radius: f32,
    ) -> Result<Self, DynamicsOverlayError3d> {
        Ok(Self {})
    }

    /// Creates a no-op overlay when Rapier is disabled.
    ///
    /// # Errors
    ///
    /// Never fails without the Rapier feature.
    #[cfg(not(feature = "rapier"))]
    pub fn around_spawn_with_solid_mesh(
        _spawn: [f32; 3],
        _character_radius: f32,
        _solid: &TriangleMesh3d,
    ) -> Result<Self, DynamicsOverlayError3d> {
        Ok(Self {})
    }

    /// Spawns a proxy floor, dynamic props, and a kinematic character proxy.
    ///
    /// Prefer [`Self::around_spawn_with_solid_mesh`] so props collide with map
    /// walls. This entry stays for headless smoke without a city mesh.
    ///
    /// # Errors
    ///
    /// Returns dynamics/mesh errors when Rapier setup fails.
    #[cfg(feature = "rapier")]
    pub fn around_spawn(
        spawn: [f32; 3],
        character_radius: f32,
    ) -> Result<Self, DynamicsOverlayError3d> {
        Self::build(spawn, character_radius, None)
    }

    /// Same as [`Self::around_spawn`], but inserts a budgeted fixed trimesh from
    /// the solid collision layer so props hit nearby walls/floors.
    ///
    /// # Errors
    ///
    /// Returns dynamics/mesh errors when Rapier setup fails.
    #[cfg(feature = "rapier")]
    pub fn around_spawn_with_solid_mesh(
        spawn: [f32; 3],
        character_radius: f32,
        solid: &TriangleMesh3d,
    ) -> Result<Self, DynamicsOverlayError3d> {
        Self::build(spawn, character_radius, Some(solid))
    }

    #[cfg(feature = "rapier")]
    fn build(
        spawn: [f32; 3],
        character_radius: f32,
        solid: Option<&TriangleMesh3d>,
    ) -> Result<Self, DynamicsOverlayError3d> {
        let mut world = RapierDynamicsWorld3d::new(DynamicsWorldConfig3d::earth_60hz())?;
        let floor_top = spawn[1] - character_radius;
        let mut visuals = Vec::new();
        let mut dynamic_props = Vec::new();
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
                    "dynamics overlay: solid map trimesh ({} / {} candidates within \
                     {:.0}m XZ, cap {}); character↔map contacts disabled",
                    slice.indices.len(),
                    slice.candidates,
                    MAP_TRIMESH_CAPTURE_RADIUS,
                    MAP_TRIMESH_MAX_FACES
                );
            } else {
                eprintln!(
                    "dynamics overlay: solid trimesh unavailable \
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
            dynamic_props.push(id);
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
        dynamic_props.push(ball);
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

        eprintln!(
            "dynamics overlay: {} props + kinematic character sphere \
             (two-way soft reaction; mesh character unchanged)",
            dynamic_props.len()
        );

        Ok(Self {
            inner: Some(OverlayInner {
                world,
                visuals,
                dynamic_props,
                character_proxy,
                cube: MeshPrimitive::cube(MESH_HALF)?,
                trigger_ids: HashMap::new(),
            }),
        })
    }

    /// Syncs the kinematic character proxy, advances Rapier, and returns a soft
    /// world-space displacement to apply to the mesh character (often zero).
    #[allow(clippy::unused_self, reason = "self is used under rapier")]
    #[must_use]
    pub fn step(&mut self, fixed_dt: f32, character_center: [f32; 3]) -> [f32; 3] {
        #[cfg(feature = "rapier")]
        {
            let Some(inner) = self.inner.as_mut() else {
                return [0.0, 0.0, 0.0];
            };
            if !(fixed_dt.is_finite()
                && fixed_dt > 0.0
                && character_center.iter().all(|channel| channel.is_finite()))
            {
                return [0.0, 0.0, 0.0];
            }

            let mut before = Vec::with_capacity(inner.dynamic_props.len());
            for &prop in &inner.dynamic_props {
                let _ = inner.world.wake_up(prop);
                before.push(inner.world.translation(prop).unwrap_or(character_center));
            }

            let _ = inner
                .world
                .set_next_kinematic_translation(inner.character_proxy, character_center);
            let _ = inner.world.step(Some(fixed_dt));

            let mut rx = 0.0_f32;
            let mut ry = 0.0_f32;
            let mut rz = 0.0_f32;
            for (index, &prop) in inner.dynamic_props.iter().enumerate() {
                let Some(after) = inner.world.translation(prop) else {
                    continue;
                };
                let prev = before[index];
                let moved = [
                    after[0] - prev[0],
                    after[1] - prev[1],
                    after[2] - prev[2],
                ];
                let move_len_sq = moved[0] * moved[0] + moved[1] * moved[1] + moved[2] * moved[2];
                if move_len_sq < 1.0e-10 {
                    continue;
                }
                let to_prop = [
                    prev[0] - character_center[0],
                    prev[1] - character_center[1],
                    prev[2] - character_center[2],
                ];
                let dist_sq =
                    to_prop[0] * to_prop[0] + to_prop[1] * to_prop[1] + to_prop[2] * to_prop[2];
                if dist_sq > 4.0 {
                    continue;
                }
                let outward =
                    moved[0] * to_prop[0] + moved[1] * to_prop[1] + moved[2] * to_prop[2];
                if outward <= 0.0 {
                    continue;
                }
                rx -= moved[0] * PROP_REACTION_SHARE;
                ry -= moved[1] * PROP_REACTION_SHARE * PROP_REACTION_Y_SCALE;
                rz -= moved[2] * PROP_REACTION_SHARE;
            }
            clamp_vec3(&mut rx, &mut ry, &mut rz, PROP_REACTION_MAX);
            [rx, ry, rz]
        }
        #[cfg(not(feature = "rapier"))]
        {
            let _ = (fixed_dt, character_center);
            [0.0, 0.0, 0.0]
        }
    }

    /// Draws prop meshes after the city/character passes (no-op without Rapier).
    ///
    /// # Errors
    ///
    /// Returns render errors when mesh upload/draw fails.
    pub fn draw(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
    ) -> Result<(), DynamicsOverlayError3d> {
        #[cfg(feature = "rapier")]
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
        #[cfg(not(feature = "rapier"))]
        {
            let _ = (frame, camera);
        }
        Ok(())
    }

    /// Returns whether the Rapier overlay is active.
    #[must_use]
    pub fn is_active(&self) -> bool {
        #[cfg(feature = "rapier")]
        {
            self.inner.is_some()
        }
        #[cfg(not(feature = "rapier"))]
        {
            false
        }
    }

    /// Registers an authored trigger as a fixed Rapier sensor sphere.
    ///
    /// Sensors overlap the kinematic character proxy (and props). Call hosts
    /// feed [`Self::trigger_overlap_pairs`] into Play's `TriggerOverlapTracker`.
    ///
    /// # Errors
    ///
    /// Returns when the overlay is inactive or the sensor insert fails.
    pub fn register_trigger_sphere(
        &mut self,
        center: [f32; 3],
        radius: f32,
        trigger_id: impl Into<String>,
    ) -> Result<(), DynamicsOverlayError3d> {
        #[cfg(feature = "rapier")]
        {
            let Some(inner) = self.inner.as_mut() else {
                return Err(DynamicsOverlayError3d::InactiveOverlay);
            };
            let body = inner.world.insert_trigger_sphere(center, radius)?;
            // Sensors see character + props; map is non-sensor so ignored by
            // collect_trigger_overlaps pairing rules.
            inner.world.set_collision_groups(
                body,
                CollisionGroups3d::new(GROUP_MAP, GROUP_CHAR | GROUP_PROP),
            )?;
            inner.trigger_ids.insert(body, trigger_id.into());
            Ok(())
        }
        #[cfg(not(feature = "rapier"))]
        {
            let _ = (center, radius, trigger_id);
            Err(DynamicsOverlayError3d::Inactive)
        }
    }

    /// Current sensor intersection pairs `(trigger_body, other_body)`.
    #[must_use]
    pub fn trigger_overlap_pairs(&self) -> Vec<(BodyId3d, BodyId3d)> {
        #[cfg(feature = "rapier")]
        {
            self.inner
                .as_ref()
                .map(|inner| inner.world.collect_trigger_overlaps())
                .unwrap_or_default()
        }
        #[cfg(not(feature = "rapier"))]
        {
            Vec::new()
        }
    }

    /// Semantic trigger id map for sensor bodies registered via
    /// [`Self::register_trigger_sphere`].
    #[must_use]
    pub fn trigger_ids(&self) -> &HashMap<BodyId3d, String> {
        #[cfg(feature = "rapier")]
        {
            static EMPTY: std::sync::OnceLock<HashMap<BodyId3d, String>> = std::sync::OnceLock::new();
            self.inner.as_ref().map_or_else(
                || EMPTY.get_or_init(HashMap::new),
                |inner| &inner.trigger_ids,
            )
        }
        #[cfg(not(feature = "rapier"))]
        {
            static EMPTY: std::sync::OnceLock<HashMap<BodyId3d, String>> = std::sync::OnceLock::new();
            EMPTY.get_or_init(HashMap::new)
        }
    }
}

/// Failure while constructing or drawing [`DynamicsOverlay3d`].
#[derive(Debug)]
pub enum DynamicsOverlayError3d {
    /// Rapier / dynamics backend failure.
    #[cfg(feature = "rapier")]
    Dynamics(DynamicsBackendError3d),
    /// Cube primitive construction failed.
    #[cfg(feature = "rapier")]
    Primitive(PrimitiveError),
    /// GPU mesh upload failed.
    #[cfg(feature = "rapier")]
    Upload(MeshUploadError),
    /// Prop batch draw failed.
    #[cfg(feature = "rapier")]
    Render(MeshRenderError),
    /// Overlay was not constructed (no active Rapier world).
    #[cfg(feature = "rapier")]
    InactiveOverlay,
    /// Stub variant so the enum is non-empty without Rapier.
    #[cfg(not(feature = "rapier"))]
    Inactive,
}

impl fmt::Display for DynamicsOverlayError3d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            #[cfg(feature = "rapier")]
            Self::Dynamics(ref error) => write!(formatter, "dynamics overlay: {error}"),
            #[cfg(feature = "rapier")]
            Self::Primitive(ref error) => write!(formatter, "dynamics overlay primitive: {error}"),
            #[cfg(feature = "rapier")]
            Self::Upload(ref error) => write!(formatter, "dynamics overlay upload: {error}"),
            #[cfg(feature = "rapier")]
            Self::Render(ref error) => write!(formatter, "dynamics overlay render: {error}"),
            #[cfg(feature = "rapier")]
            Self::InactiveOverlay => formatter.write_str("dynamics overlay is not active"),
            #[cfg(not(feature = "rapier"))]
            Self::Inactive => formatter.write_str("dynamics overlay inactive"),
        }
    }
}

impl Error for DynamicsOverlayError3d {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match *self {
            #[cfg(feature = "rapier")]
            Self::Dynamics(ref error) => Some(error),
            #[cfg(feature = "rapier")]
            Self::Primitive(ref error) => Some(error),
            #[cfg(feature = "rapier")]
            Self::Upload(ref error) => Some(error),
            #[cfg(feature = "rapier")]
            Self::Render(ref error) => Some(error),
            #[cfg(feature = "rapier")]
            Self::InactiveOverlay => None,
            #[cfg(not(feature = "rapier"))]
            Self::Inactive => None,
        }
    }
}

#[cfg(feature = "rapier")]
impl From<DynamicsBackendError3d> for DynamicsOverlayError3d {
    fn from(value: DynamicsBackendError3d) -> Self {
        Self::Dynamics(value)
    }
}

#[cfg(feature = "rapier")]
impl From<PrimitiveError> for DynamicsOverlayError3d {
    fn from(value: PrimitiveError) -> Self {
        Self::Primitive(value)
    }
}

#[cfg(feature = "rapier")]
impl From<MeshUploadError> for DynamicsOverlayError3d {
    fn from(value: MeshUploadError) -> Self {
        Self::Upload(value)
    }
}

#[cfg(feature = "rapier")]
impl From<MeshRenderError> for DynamicsOverlayError3d {
    fn from(value: MeshRenderError) -> Self {
        Self::Render(value)
    }
}

#[cfg(feature = "rapier")]
fn clamp_vec3(x: &mut f32, y: &mut f32, z: &mut f32, max_len: f32) {
    let len_sq = *x * *x + *y * *y + *z * *z;
    let max_sq = max_len * max_len;
    if len_sq > max_sq && len_sq > 0.0 {
        let scale = max_len / len_sq.sqrt();
        *x *= scale;
        *y *= scale;
        *z *= scale;
    }
}

#[cfg(feature = "rapier")]
struct LocalTrimeshSlice {
    vertices: Vec<[f32; 3]>,
    indices: Vec<[u32; 3]>,
    candidates: usize,
}

#[cfg(feature = "rapier")]
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

#[cfg(feature = "rapier")]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_or_active_step_returns_finite_reaction() {
        let mut overlay =
            DynamicsOverlay3d::around_spawn([0.0, 1.0, 0.0], 0.4).expect("construct");
        let reaction = overlay.step(1.0 / 60.0, [0.0, 1.0, 0.0]);
        assert!(reaction.iter().all(|v| v.is_finite()));
        #[cfg(not(feature = "rapier"))]
        {
            assert!(!overlay.is_active());
            assert_eq!(reaction, [0.0, 0.0, 0.0]);
        }
        #[cfg(feature = "rapier")]
        {
            assert!(overlay.is_active());
        }
    }

    #[test]
    #[cfg(feature = "rapier")]
    fn around_spawn_with_no_solid_stays_active() {
        // No solid mesh → proxy floor path inside build().
        let overlay =
            DynamicsOverlay3d::around_spawn([1.0, 2.0, 3.0], 0.35).expect("proxy floor");
        assert!(overlay.is_active());
        let reaction = {
            let mut overlay = overlay;
            overlay.step(1.0 / 60.0, [1.0, 2.0, 3.0])
        };
        assert!(reaction.iter().all(|v| v.is_finite()));
    }

    #[test]
    #[cfg(feature = "rapier")]
    fn register_trigger_sphere_on_active_overlay() {
        let mut overlay =
            DynamicsOverlay3d::around_spawn([0.0, 1.0, 0.0], 0.4).expect("construct");
        overlay
            .register_trigger_sphere([0.0, 1.0, 2.0], 1.0, "level.exit")
            .expect("sensor");
        assert!(overlay.trigger_ids().values().any(|id| id == "level.exit"));
    }
}
