//! Editor viewport transform gizmo (Unity-style move / rotate / scale).
//!
//! Drawn as an immediate unlit depth-cleared pass after the scene PBR draw —
//! never as gameplay [`Model3d`] entities. Shafts are cylinders, tips are cones,
//! rotate handles are tori — not scaled cubes.

use yuyib_ecs::{bevy_ecs::entity::Entity, prelude::World};
use yuyib_game_3d::{Model3d, Transform3d, WorldTransform3d};
use yuyib_model::MeshPrimitive;
use yuyib_render::{RenderFrame, Renderer};
use yuyib_render_3d::{Camera3d, GpuMesh, MeshRenderError, MeshRenderer3d};

use crate::{
    bridge::ViewportTool,
    viewport_gizmo::{GizmoAxis, GizmoPick, GizmoToolKind, pick_arrow_gizmo, pick_rotation_gizmo},
    viewport_picking::ViewportRay,
};

/// Active gizmo configuration (no ECS entities).
#[derive(Clone, Copy, Debug)]
pub struct GizmoState {
    pub tool: ViewportTool,
    pub origin: [f32; 3],
    pub layout: GizmoLayout,
}

#[derive(Clone, Copy, Debug)]
pub struct GizmoLayout {
    #[allow(dead_code)]
    pub unit: f32,
    pub shaft_length: f32,
    pub shaft_radius: f32,
    pub tip_length: f32,
    pub tip_radius: f32,
    pub tip_offset: f32,
    pub ring_radius: f32,
    pub ring_tube: f32,
}

impl GizmoLayout {
    #[must_use]
    pub fn from_camera_distance(distance: f32) -> Self {
        // Screen-ish size: shrink with distance, but never below `lo` so close-up
        // orbit still leaves pickable rotate rings / move arrows.
        let unit = if distance.is_finite() && distance > 0.0 {
            let lo = 0.65_f32;
            let hi = (distance * 0.45).max(lo);
            (distance * 0.12).clamp(lo, hi)
        } else {
            1.0
        };
        Self {
            unit,
            shaft_length: 0.70 * unit,
            shaft_radius: 0.038 * unit,
            tip_length: 0.32 * unit,
            tip_radius: 0.11 * unit,
            tip_offset: 1.0 * unit,
            ring_radius: 1.45 * unit,
            ring_tube: 0.032 * unit,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum GizmoMeshKind {
    Shaft,
    Tip,
    Ring,
    Box,
}

/// One unlit instance in the post-scene gizmo pass.
#[derive(Clone, Copy, Debug)]
pub struct GizmoDrawPart {
    mesh: GizmoMeshKind,
    pub model_matrix: [f32; 16],
    pub color: [f32; 4],
}

/// GPU resources for the editor gizmo unlit pass (session-long).
pub struct GizmoUnlitPass {
    renderer: MeshRenderer3d,
    shaft: GpuMesh,
    tip: GpuMesh,
    ring: GpuMesh,
    cube: GpuMesh,
}

impl GizmoUnlitPass {
    /// Uploads unit shaft / tip / ring / cube meshes once.
    ///
    /// # Errors
    ///
    /// Returns mesh construction or GPU upload failures.
    pub fn new(renderer: &Renderer) -> Result<Self, Box<dyn std::error::Error>> {
        let mesh_renderer = MeshRenderer3d::new(renderer);
        let shaft = mesh_renderer.upload_mesh(renderer, &cylinder_along_y(1.0, 1.0, 20)?)?;
        let tip = mesh_renderer.upload_mesh(renderer, &cone_along_y(1.0, 1.0, 20)?)?;
        // Tube radius relative to major=1 — keep thin so large rings stay readable.
        let ring = mesh_renderer.upload_mesh(renderer, &torus_xy(1.0, 0.028, 96, 10)?)?;
        let cube = mesh_renderer.upload_mesh(renderer, &unit_cube()?)?;
        Ok(Self {
            renderer: mesh_renderer,
            shaft,
            tip,
            ring,
            cube,
        })
    }

    /// Draws all parts after the scene in one depth-cleared batch pass.
    ///
    /// # Errors
    ///
    /// Returns mesh render errors for invalid camera/matrices.
    pub fn draw(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        parts: &[GizmoDrawPart],
    ) -> Result<(), MeshRenderError> {
        if parts.is_empty() {
            return Ok(());
        }
        let draws: Vec<(&GpuMesh, [f32; 16], [f32; 4])> = parts
            .iter()
            .map(|part| {
                let mesh = match part.mesh {
                    GizmoMeshKind::Shaft => &self.shaft,
                    GizmoMeshKind::Tip => &self.tip,
                    GizmoMeshKind::Ring => &self.ring,
                    GizmoMeshKind::Box => &self.cube,
                };
                (mesh, part.model_matrix, part.color)
            })
            .collect();
        self.renderer
            .draw_batch_depth_clear_double_sided(frame, camera, &draws)
            .map(|_| ())
    }
}

/// Builds gizmo state for the active tool, or `None` for Select.
#[must_use]
pub fn make(tool: ViewportTool, origin: [f32; 3], layout: GizmoLayout) -> Option<GizmoState> {
    match tool {
        ViewportTool::Select => None,
        ViewportTool::Move | ViewportTool::Rotate | ViewportTool::Scale => Some(GizmoState {
            tool,
            origin,
            layout,
        }),
    }
}

/// CPU draw list for the unlit pass.
#[must_use]
pub fn draw_parts(state: GizmoState) -> Vec<GizmoDrawPart> {
    match state.tool {
        ViewportTool::Select => Vec::new(),
        ViewportTool::Move => arrow_parts(state.origin, state.layout),
        ViewportTool::Scale => scale_parts(state.origin, state.layout),
        ViewportTool::Rotate => ring_parts(state.origin, state.layout),
    }
}

/// Wireframe AABB as 12 thin shafts (unit cylinder along +Y from each edge start).
#[must_use]
pub fn bounds_box_parts(minimum: [f32; 3], maximum: [f32; 3]) -> Vec<GizmoDrawPart> {
    let dx = (maximum[0] - minimum[0]).abs();
    let dy = (maximum[1] - minimum[1]).abs();
    let dz = (maximum[2] - minimum[2]).abs();
    if !dx.is_finite() || !dy.is_finite() || !dz.is_finite() {
        return Vec::new();
    }
    let diag = (dx * dx + dy * dy + dz * dz).sqrt().max(1.0e-3);
    let thickness = (diag * 0.004).clamp(0.0015, 0.06);
    let color = [0.28, 0.82, 1.0, 0.92];

    let x0 = minimum[0].min(maximum[0]);
    let y0 = minimum[1].min(maximum[1]);
    let z0 = minimum[2].min(maximum[2]);
    let x1 = minimum[0].max(maximum[0]);
    let y1 = minimum[1].max(maximum[1]);
    let z1 = minimum[2].max(maximum[2]);

    let corners = [
        [x0, y0, z0],
        [x1, y0, z0],
        [x1, y0, z1],
        [x0, y0, z1],
        [x0, y1, z0],
        [x1, y1, z0],
        [x1, y1, z1],
        [x0, y1, z1],
    ];
    let edges = [
        (0, 1),
        (1, 2),
        (2, 3),
        (3, 0),
        (4, 5),
        (5, 6),
        (6, 7),
        (7, 4),
        (0, 4),
        (1, 5),
        (2, 6),
        (3, 7),
    ];

    let mut parts = Vec::with_capacity(12);
    for (from, to) in edges {
        let a = corners[from];
        let b = corners[to];
        let delta = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let length_sq = delta[0] * delta[0] + delta[1] * delta[1] + delta[2] * delta[2];
        if length_sq < 1.0e-12 {
            continue;
        }
        let length = length_sq.sqrt();
        let Some(rot) = rotation_y_to_dir(delta) else {
            continue;
        };
        parts.push(GizmoDrawPart {
            mesh: GizmoMeshKind::Shaft,
            model_matrix: compose(a, rot, [thickness, length, thickness]),
            color,
        });
    }
    parts
}

/// Hard cap on shafts drawn by [`vertex_normal_parts`] / preview overlay.
pub const NORMAL_OVERLAY_MAX_SHAFTS: usize = 1_024;

/// Visible shaft length from aggregate bounds radius (scales with the asset).
#[must_use]
pub fn normal_shaft_length_for_radius(radius: f32) -> f32 {
    let radius = if radius.is_finite() && radius > 0.0 {
        radius
    } else {
        1.0
    };
    (radius * 0.22).clamp(0.2, (radius * 0.55).max(0.2))
}

/// Debug shafts for vertex normals (local → world via column-major model matrix).
///
/// Samples evenly when `positions.len()` exceeds `max_count` (capped at
/// [`NORMAL_OVERLAY_MAX_SHAFTS`]). Each sample draws a shaft + tip.
#[must_use]
pub fn vertex_normal_parts(
    positions: &[[f32; 3]],
    normals: &[[f32; 3]],
    model_matrix: [f32; 16],
    length: f32,
    max_count: usize,
) -> Vec<GizmoDrawPart> {
    if positions.len() != normals.len() || positions.is_empty() || !length.is_finite() || length <= 0.0
    {
        return Vec::new();
    }
    let budget = max_count.min(NORMAL_OVERLAY_MAX_SHAFTS).max(1);
    let stride = positions.len().div_ceil(budget).max(1);
    let thickness = (length * 0.12).clamp(0.02, length * 0.22);
    let tip_len = (length * 0.28).clamp(0.04, length * 0.4);
    let tip_r = (thickness * 2.2).max(thickness + 0.01);
    let color = [1.0, 0.35, 0.95, 1.0];
    let mut parts = Vec::with_capacity(budget.saturating_mul(2));
    let mut index = 0;
    while index < positions.len() && parts.len() / 2 < budget {
        let local_n = normals[index];
        let Some(world_n) = transform_direction(model_matrix, local_n) else {
            index = index.saturating_add(stride);
            continue;
        };
        let origin = transform_point(model_matrix, positions[index]);
        let Some(rot) = rotation_y_to_dir(world_n) else {
            index = index.saturating_add(stride);
            continue;
        };
        let tip_origin = [
            origin[0] + world_n[0] * length,
            origin[1] + world_n[1] * length,
            origin[2] + world_n[2] * length,
        ];
        parts.push(GizmoDrawPart {
            mesh: GizmoMeshKind::Shaft,
            model_matrix: compose(origin, rot, [thickness, length, thickness]),
            color,
        });
        parts.push(GizmoDrawPart {
            mesh: GizmoMeshKind::Tip,
            model_matrix: compose(tip_origin, rot, [tip_r, tip_len, tip_r]),
            color,
        });
        index = index.saturating_add(stride);
    }
    parts
}

/// Face-normal shafts when vertex normals are missing (triangle centroids).
#[must_use]
pub fn face_normal_parts(
    positions: &[[f32; 3]],
    indices: &[u32],
    model_matrix: [f32; 16],
    length: f32,
    max_count: usize,
) -> Vec<GizmoDrawPart> {
    if positions.is_empty() || indices.len() < 3 || !length.is_finite() || length <= 0.0 {
        return Vec::new();
    }
    let tri_count = indices.len() / 3;
    if tri_count == 0 {
        return Vec::new();
    }
    let budget = max_count.min(NORMAL_OVERLAY_MAX_SHAFTS).max(1);
    let stride = tri_count.div_ceil(budget).max(1);
    let mut local_positions = Vec::with_capacity(budget);
    let mut local_normals = Vec::with_capacity(budget);
    let mut tri = 0;
    while tri < tri_count && local_positions.len() < budget {
        let base = tri * 3;
        let Some(ia) = indices.get(base).copied() else {
            break;
        };
        let Some(ib) = indices.get(base + 1).copied() else {
            break;
        };
        let Some(ic) = indices.get(base + 2).copied() else {
            break;
        };
        let Some(a) = positions.get(ia as usize).copied() else {
            tri = tri.saturating_add(stride);
            continue;
        };
        let Some(b) = positions.get(ib as usize).copied() else {
            tri = tri.saturating_add(stride);
            continue;
        };
        let Some(c) = positions.get(ic as usize).copied() else {
            tri = tri.saturating_add(stride);
            continue;
        };
        let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let n = cross3(ab, ac);
        if normalize3(n).is_none() {
            tri = tri.saturating_add(stride);
            continue;
        }
        local_positions.push([
            (a[0] + b[0] + c[0]) / 3.0,
            (a[1] + b[1] + c[1]) / 3.0,
            (a[2] + b[2] + c[2]) / 3.0,
        ]);
        local_normals.push(n);
        tri = tri.saturating_add(stride);
    }
    vertex_normal_parts(&local_positions, &local_normals, model_matrix, length, budget)
}

/// Collects world-space normal shafts for every visible [`Model3d`] in `world`.
#[must_use]
pub fn model_normal_overlay_parts(
    world: &World,
    models: &yuyib_assets::Assets<yuyib_model::Model>,
    length: f32,
) -> Vec<GizmoDrawPart> {
    let mut remaining = NORMAL_OVERLAY_MAX_SHAFTS;
    let mut parts = Vec::new();
    for entity in world.iter_entities() {
        if remaining == 0 {
            break;
        }
        let Some(model3d) = entity.get::<Model3d>() else {
            continue;
        };
        if !model3d.visible {
            continue;
        }
        let Some(model_matrix) = entity_model_matrix(entity) else {
            continue;
        };
        let Some(model) = models.get(model3d.model) else {
            continue;
        };
        let mesh_indices: Vec<usize> = match model3d.mesh {
            Some(index) => vec![index],
            None => (0..model.meshes().len()).collect(),
        };
        for mesh_index in mesh_indices {
            let Some(mesh) = model.meshes().get(mesh_index) else {
                continue;
            };
            for primitive in mesh.primitives() {
                if remaining == 0 {
                    break;
                }
                let chunk = if let Some(normals) = primitive.normals() {
                    vertex_normal_parts(
                        primitive.positions(),
                        normals,
                        model_matrix,
                        length,
                        remaining,
                    )
                } else {
                    face_normal_parts(
                        primitive.positions(),
                        primitive.indices(),
                        model_matrix,
                        length,
                        remaining,
                    )
                };
                // Each sample may contribute shaft+tip (2 parts).
                let samples = chunk.len() / 2;
                remaining = remaining.saturating_sub(samples.max(1));
                parts.extend(chunk);
            }
        }
    }
    parts
}

fn entity_model_matrix(entity: yuyib_ecs::bevy_ecs::world::EntityRef<'_>) -> Option<[f32; 16]> {
    if let Some(world_xf) = entity.get::<WorldTransform3d>() {
        return Some(world_xf.column_major());
    }
    if let Some(local) = entity.get::<yuyib_game_3d::LocalMatrixTransform3d>() {
        return Some(local.column_major());
    }
    None
}

/// Directional-light aim cone: beam shaft + open cone along world light direction.
#[must_use]
pub fn light_direction_parts(
    origin: [f32; 3],
    direction: [f32; 3],
    unit: f32,
) -> Vec<GizmoDrawPart> {
    let Some(forward) = normalize3(direction) else {
        return Vec::new();
    };
    let Some(rot_forward) = rotation_y_to_dir(forward) else {
        return Vec::new();
    };
    let back = [-forward[0], -forward[1], -forward[2]];
    let Some(rot_back) = rotation_y_to_dir(back) else {
        return Vec::new();
    };
    let color = [1.0, 0.82, 0.18, 0.95];
    let shaft_len = 1.35 * unit;
    let shaft_r = 0.028 * unit;
    let tip_len = 0.38 * unit;
    let tip_r = 0.12 * unit;
    let beam = 1.55 * unit;
    let cone_r = 0.58 * unit;

    let tip_base = [
        origin[0] + forward[0] * shaft_len,
        origin[1] + forward[1] * shaft_len,
        origin[2] + forward[2] * shaft_len,
    ];
    let cone_base = [
        origin[0] + forward[0] * beam,
        origin[1] + forward[1] * beam,
        origin[2] + forward[2] * beam,
    ];

    vec![
        GizmoDrawPart {
            mesh: GizmoMeshKind::Shaft,
            model_matrix: compose(origin, rot_forward, [shaft_r, shaft_len, shaft_r]),
            color,
        },
        GizmoDrawPart {
            mesh: GizmoMeshKind::Tip,
            model_matrix: compose(tip_base, rot_forward, [tip_r, tip_len, tip_r]),
            color,
        },
        // Apex toward the light origin, base opens along travel — reads as a cone.
        GizmoDrawPart {
            mesh: GizmoMeshKind::Tip,
            model_matrix: compose(cone_base, rot_back, [cone_r, beam, cone_r]),
            color: [1.0, 0.78, 0.12, 0.55],
        },
    ]
}

/// Picks a handle. `origin` must match the world-space draw origin.
#[must_use]
pub fn pick(
    tool: ViewportTool,
    ray: ViewportRay,
    origin: [f32; 3],
    layout: GizmoLayout,
) -> Option<GizmoPick> {
    let kind = tool_kind(tool)?;
    match kind {
        GizmoToolKind::Rotate => {
            // Visual tube is thin; keep a wider pick radius so rings stay usable.
            pick_rotation_gizmo(
                ray,
                origin,
                layout.ring_radius,
                (layout.ring_tube * 8.0).max(0.09 * layout.ring_radius),
            )
        }
        GizmoToolKind::Move | GizmoToolKind::Scale => pick_arrow_gizmo(
            ray,
            origin,
            layout.tip_offset,
            layout.shaft_radius * 4.0,
            layout.tip_radius * 2.5,
            None,
        ),
    }
}

#[must_use]
pub fn tool_kind(tool: ViewportTool) -> Option<GizmoToolKind> {
    match tool {
        ViewportTool::Move => Some(GizmoToolKind::Move),
        ViewportTool::Rotate => Some(GizmoToolKind::Rotate),
        ViewportTool::Scale => Some(GizmoToolKind::Scale),
        ViewportTool::Select => None,
    }
}

/// World translation of a materialized entity (prefers [`WorldTransform3d`]).
#[must_use]
pub fn entity_world_translation(world: &World, entity: Entity) -> Option<[f32; 3]> {
    if let Some(transform) = world.get::<WorldTransform3d>(entity) {
        let m = transform.column_major();
        return Some([m[12], m[13], m[14]]);
    }
    world
        .get::<Transform3d>(entity)
        .map(|transform| transform.translation)
}

fn arrow_parts(origin: [f32; 3], layout: GizmoLayout) -> Vec<GizmoDrawPart> {
    let mut parts = Vec::with_capacity(6);
    for axis in [GizmoAxis::X, GizmoAxis::Y, GizmoAxis::Z] {
        let color = axis_color(axis);
        parts.push(GizmoDrawPart {
            mesh: GizmoMeshKind::Shaft,
            model_matrix: shaft_matrix(origin, axis, layout),
            color,
        });
        parts.push(GizmoDrawPart {
            mesh: GizmoMeshKind::Tip,
            model_matrix: tip_matrix(origin, axis, layout),
            color,
        });
    }
    parts
}

fn scale_parts(origin: [f32; 3], layout: GizmoLayout) -> Vec<GizmoDrawPart> {
    let mut parts = Vec::with_capacity(6);
    for axis in [GizmoAxis::X, GizmoAxis::Y, GizmoAxis::Z] {
        let color = axis_color(axis);
        parts.push(GizmoDrawPart {
            mesh: GizmoMeshKind::Shaft,
            model_matrix: shaft_matrix(origin, axis, layout),
            color,
        });
        let direction = axis.as_vec3();
        let center = [
            origin[0] + direction[0] * layout.tip_offset,
            origin[1] + direction[1] * layout.tip_offset,
            origin[2] + direction[2] * layout.tip_offset,
        ];
        let s = layout.tip_radius * 1.6;
        parts.push(GizmoDrawPart {
            mesh: GizmoMeshKind::Box,
            model_matrix: trs_matrix(center, [s, s, s]),
            color,
        });
    }
    parts
}

fn ring_parts(origin: [f32; 3], layout: GizmoLayout) -> Vec<GizmoDrawPart> {
    [GizmoAxis::X, GizmoAxis::Y, GizmoAxis::Z]
        .into_iter()
        .map(|axis| GizmoDrawPart {
            mesh: GizmoMeshKind::Ring,
            model_matrix: ring_matrix(origin, axis, layout),
            color: axis_color(axis),
        })
        .collect()
}

/// Unit cylinder along +Y from y=0..1, radius 1 → scale (r, length, r).
fn shaft_matrix(origin: [f32; 3], axis: GizmoAxis, layout: GizmoLayout) -> [f32; 16] {
    let rot = rotation_y_to_axis(axis);
    let scale = [
        layout.shaft_radius,
        layout.shaft_length,
        layout.shaft_radius,
    ];
    // Base of cylinder at origin; extends along axis.
    compose(origin, rot, scale)
}

/// Unit cone along +Y from y=0 (base) to y=1 (apex), radius 1 at base.
fn tip_matrix(origin: [f32; 3], axis: GizmoAxis, layout: GizmoLayout) -> [f32; 16] {
    let rot = rotation_y_to_axis(axis);
    let direction = axis.as_vec3();
    let base = [
        origin[0] + direction[0] * layout.shaft_length,
        origin[1] + direction[1] * layout.shaft_length,
        origin[2] + direction[2] * layout.shaft_length,
    ];
    let scale = [layout.tip_radius, layout.tip_length, layout.tip_radius];
    compose(base, rot, scale)
}

/// Unit torus in XY (around Z), major radius 1.
fn ring_matrix(origin: [f32; 3], axis: GizmoAxis, layout: GizmoLayout) -> [f32; 16] {
    let rot = rotation_z_to_axis(axis);
    // Mesh tube is ~0.028 at major=1. Scale XY by ring radius; scale Z so the
    // baked tube tracks `ring_tube` instead of fattening with the major radius.
    let tube_mesh = 0.028_f32;
    let z = (layout.ring_tube / tube_mesh).max(0.15);
    let scale = [layout.ring_radius, layout.ring_radius, z];
    compose(origin, rot, scale)
}

/// Columns: rotated scaled basis + translation (column-major).
fn compose(translation: [f32; 3], rot: [[f32; 3]; 3], scale: [f32; 3]) -> [f32; 16] {
    let c0 = [
        rot[0][0] * scale[0],
        rot[1][0] * scale[0],
        rot[2][0] * scale[0],
    ];
    let c1 = [
        rot[0][1] * scale[1],
        rot[1][1] * scale[1],
        rot[2][1] * scale[1],
    ];
    let c2 = [
        rot[0][2] * scale[2],
        rot[1][2] * scale[2],
        rot[2][2] * scale[2],
    ];
    [
        c0[0], c0[1], c0[2], 0.0, c1[0], c1[1], c1[2], 0.0, c2[0], c2[1], c2[2], 0.0,
        translation[0], translation[1], translation[2], 1.0,
    ]
}

fn trs_matrix(translation: [f32; 3], scale: [f32; 3]) -> [f32; 16] {
    [
        scale[0],
        0.0,
        0.0,
        0.0,
        0.0,
        scale[1],
        0.0,
        0.0,
        0.0,
        0.0,
        scale[2],
        0.0,
        translation[0],
        translation[1],
        translation[2],
        1.0,
    ]
}

/// Rotation matrix (rows) mapping +Y to the gizmo axis.
fn rotation_y_to_axis(axis: GizmoAxis) -> [[f32; 3]; 3] {
    match axis {
        GizmoAxis::Y => [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        // Y → X: rotate -90° around Z
        GizmoAxis::X => [[0.0, 1.0, 0.0], [-1.0, 0.0, 0.0], [0.0, 0.0, 1.0]],
        // Y → Z: rotate +90° around X
        GizmoAxis::Z => [[1.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, 1.0, 0.0]],
    }
}

/// Orthonormal rows mapping local +Y onto a unit world direction.
fn rotation_y_to_dir(dir: [f32; 3]) -> Option<[[f32; 3]; 3]> {
    let y = normalize3(dir)?;
    let helper = if y[1].abs() < 0.9 {
        [0.0, 1.0, 0.0]
    } else {
        [1.0, 0.0, 0.0]
    };
    let x = normalize3(cross3(helper, y))?;
    let z = cross3(y, x);
    Some([[x[0], y[0], z[0]], [x[1], y[1], z[1]], [x[2], y[2], z[2]]])
}

fn transform_point(m: [f32; 16], p: [f32; 3]) -> [f32; 3] {
    [
        m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12],
        m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13],
        m[2] * p[0] + m[6] * p[1] + m[10] * p[2] + m[14],
    ]
}

fn transform_direction(m: [f32; 16], d: [f32; 3]) -> Option<[f32; 3]> {
    normalize3([
        m[0] * d[0] + m[4] * d[1] + m[8] * d[2],
        m[1] * d[0] + m[5] * d[1] + m[9] * d[2],
        m[2] * d[0] + m[6] * d[1] + m[10] * d[2],
    ])
}

fn normalize3(v: [f32; 3]) -> Option<[f32; 3]> {
    let len_sq = v[0] * v[0] + v[1] * v[1] + v[2] * v[2];
    if !len_sq.is_finite() || len_sq < 1.0e-12 {
        return None;
    }
    let inv = 1.0 / len_sq.sqrt();
    Some([v[0] * inv, v[1] * inv, v[2] * inv])
}

fn cross3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// Rotation mapping torus +Z normal to the gizmo axis (ring lies in plane ⊥ axis).
fn rotation_z_to_axis(axis: GizmoAxis) -> [[f32; 3]; 3] {
    match axis {
        GizmoAxis::Z => [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        // Z → X: rotate +90° around Y
        GizmoAxis::X => [[0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [-1.0, 0.0, 0.0]],
        // Z → Y: rotate -90° around X
        GizmoAxis::Y => [[1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, -1.0, 0.0]],
    }
}

fn axis_color(axis: GizmoAxis) -> [f32; 4] {
    let rgb = axis.color_rgb();
    [rgb[0], rgb[1], rgb[2], 1.0]
}

fn cylinder_along_y(
    radius: f32,
    height: f32,
    segments: usize,
) -> Result<MeshPrimitive, Box<dyn std::error::Error>> {
    let segments = segments.max(3);
    let mut positions = Vec::with_capacity((segments + 1) * 2);
    let mut indices = Vec::with_capacity(segments * 6);
    for i in 0..=segments {
        let angle = (i as f32 / segments as f32) * std::f32::consts::TAU;
        let (s, c) = angle.sin_cos();
        let x = c * radius;
        let z = s * radius;
        positions.push([x, 0.0, z]);
        positions.push([x, height, z]);
    }
    for i in 0..segments {
        let i0 = (i * 2) as u32;
        let i1 = i0 + 1;
        let i2 = i0 + 2;
        let i3 = i0 + 3;
        indices.extend_from_slice(&[i0, i2, i1, i1, i2, i3]);
    }
    Ok(MeshPrimitive::new(positions, indices)?)
}

fn cone_along_y(
    radius: f32,
    height: f32,
    segments: usize,
) -> Result<MeshPrimitive, Box<dyn std::error::Error>> {
    let segments = segments.max(3);
    // Apex + base ring + base center (closed arrow tip).
    let mut positions = Vec::with_capacity(segments + 2);
    let mut indices = Vec::with_capacity(segments * 6);
    let apex = 0_u32;
    let base_center = 1_u32;
    positions.push([0.0, height, 0.0]);
    positions.push([0.0, 0.0, 0.0]);
    for i in 0..segments {
        let angle = (i as f32 / segments as f32) * std::f32::consts::TAU;
        let (s, c) = angle.sin_cos();
        positions.push([c * radius, 0.0, s * radius]);
    }
    for i in 0..segments {
        let a = 2 + i as u32;
        let b = 2 + ((i + 1) % segments) as u32;
        // Side (outward).
        indices.extend_from_slice(&[apex, a, b]);
        // Base disk (facing -Y).
        indices.extend_from_slice(&[base_center, b, a]);
    }
    Ok(MeshPrimitive::new(positions, indices)?)
}

fn torus_xy(
    major: f32,
    minor: f32,
    segments_major: usize,
    segments_minor: usize,
) -> Result<MeshPrimitive, Box<dyn std::error::Error>> {
    let segments_major = segments_major.max(8);
    let segments_minor = segments_minor.max(4);
    let mut positions =
        Vec::with_capacity(segments_major * segments_minor);
    let mut indices = Vec::with_capacity(segments_major * segments_minor * 6);
    for i in 0..segments_major {
        let u = (i as f32 / segments_major as f32) * std::f32::consts::TAU;
        let (su, cu) = u.sin_cos();
        for j in 0..segments_minor {
            let v = (j as f32 / segments_minor as f32) * std::f32::consts::TAU;
            let (sv, cv) = v.sin_cos();
            let r = major + minor * cv;
            positions.push([r * cu, r * su, minor * sv]);
        }
    }
    for i in 0..segments_major {
        let i0 = i * segments_minor;
        let i1 = ((i + 1) % segments_major) * segments_minor;
        for j in 0..segments_minor {
            let j1 = (j + 1) % segments_minor;
            let a = (i0 + j) as u32;
            let b = (i1 + j) as u32;
            let c = (i1 + j1) as u32;
            let d = (i0 + j1) as u32;
            indices.extend_from_slice(&[a, b, d, b, c, d]);
        }
    }
    Ok(MeshPrimitive::new(positions, indices)?)
}

fn unit_cube() -> Result<MeshPrimitive, Box<dyn std::error::Error>> {
    // Half-extent 0.5 → full size 1 after scale.
    let h = 0.5_f32;
    let positions = vec![
        [-h, -h, -h],
        [h, -h, -h],
        [h, h, -h],
        [-h, h, -h],
        [-h, -h, h],
        [h, -h, h],
        [h, h, h],
        [-h, h, h],
    ];
    let indices = vec![
        0, 1, 2, 0, 2, 3, // -Z
        4, 6, 5, 4, 7, 6, // +Z
        0, 4, 5, 0, 5, 1, // -Y
        2, 6, 7, 2, 7, 3, // +Y
        0, 3, 7, 0, 7, 4, // -X
        1, 5, 6, 1, 6, 2, // +X
    ];
    Ok(MeshPrimitive::new(positions, indices)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_clamp_never_panics_for_tiny_distance() {
        for distance in [0.01, 0.1, 0.5, 0.7, 1.0, 10.0, 100.0, 1000.0] {
            let layout = GizmoLayout::from_camera_distance(distance);
            assert!(layout.unit.is_finite() && layout.unit > 0.0);
            assert!(layout.shaft_radius < layout.shaft_length);
        }
    }

    #[test]
    fn procedural_meshes_build() {
        assert!(cylinder_along_y(1.0, 1.0, 8).is_ok());
        assert!(cone_along_y(1.0, 1.0, 8).is_ok());
        assert!(torus_xy(1.0, 0.05, 16, 6).is_ok());
        assert!(unit_cube().is_ok());
    }

    #[test]
    fn move_draw_list_has_three_axes() {
        let layout = GizmoLayout::from_camera_distance(5.0);
        let parts = draw_parts(GizmoState {
            tool: ViewportTool::Move,
            origin: [0.0; 3],
            layout,
        });
        assert_eq!(parts.len(), 6);
        assert!(parts.iter().any(|p| matches!(p.mesh, GizmoMeshKind::Tip)));
    }

    #[test]
    fn rotate_draw_list_has_three_tori() {
        let layout = GizmoLayout::from_camera_distance(5.0);
        let parts = draw_parts(GizmoState {
            tool: ViewportTool::Rotate,
            origin: [1.0, 2.0, 3.0],
            layout,
        });
        assert_eq!(parts.len(), 3);
        assert!(parts.iter().all(|p| matches!(p.mesh, GizmoMeshKind::Ring)));
    }

    #[test]
    fn axis_matrices_point_along_distinct_axes() {
        let layout = GizmoLayout::from_camera_distance(5.0);
        let origin = [0.0, 0.0, 0.0];
        let x = shaft_matrix(origin, GizmoAxis::X, layout);
        let y = shaft_matrix(origin, GizmoAxis::Y, layout);
        let z = shaft_matrix(origin, GizmoAxis::Z, layout);
        // Local +Y maps to column 1 of the model matrix.
        let tip = |m: [f32; 16]| [m[4], m[5], m[6]];
        assert!(tip(x)[0].abs() > tip(x)[1].abs() && tip(x)[0].abs() > tip(x)[2].abs());
        assert!(tip(y)[1].abs() > tip(y)[0].abs() && tip(y)[1].abs() > tip(y)[2].abs());
        assert!(tip(z)[2].abs() > tip(z)[0].abs() && tip(z)[2].abs() > tip(z)[1].abs());
    }

    #[test]
    fn bounds_box_has_twelve_edge_shafts() {
        let parts = bounds_box_parts([-1.0, -2.0, -3.0], [1.0, 2.0, 3.0]);
        assert_eq!(parts.len(), 12);
        assert!(parts.iter().all(|part| matches!(part.mesh, GizmoMeshKind::Shaft)));
    }

    #[test]
    fn bounds_box_ignores_degenerate_aabb() {
        assert!(bounds_box_parts([0.0, 0.0, 0.0], [0.0, 0.0, 0.0]).is_empty());
    }

    #[test]
    fn vertex_normal_parts_samples_and_caps() {
        let identity = [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ];
        let positions: Vec<[f32; 3]> = (0..64).map(|i| [i as f32, 0.0, 0.0]).collect();
        let normals = vec![[0.0, 1.0, 0.0]; 64];
        let parts = vertex_normal_parts(&positions, &normals, identity, 0.1, 16);
        assert_eq!(parts.len(), 32);
        assert!(parts.iter().any(|part| matches!(part.mesh, GizmoMeshKind::Shaft)));
        assert!(parts.iter().any(|part| matches!(part.mesh, GizmoMeshKind::Tip)));
    }

    #[test]
    fn vertex_normal_parts_requires_matching_streams() {
        let identity = [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ];
        assert!(vertex_normal_parts(&[[0.0; 3]], &[], identity, 0.1, 8).is_empty());
        assert!(vertex_normal_parts(&[[0.0; 3]], &[[0.0, 1.0, 0.0]], identity, 0.0, 8).is_empty());
    }
}
