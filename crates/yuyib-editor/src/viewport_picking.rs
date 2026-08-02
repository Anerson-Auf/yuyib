//! Viewport ray construction and proxy-cube intersection for scene picking.

use yuyib_game_3d::{Transform3d, WorldTransform3d};
use yuyib_render_3d::Camera3d;

/// Half extent of the editor preview proxy cube (`Model::cube` in `app.rs`).
pub const PROXY_CUBE_HALF_EXTENT: f32 = 0.7;

/// Selection id for the foundation smoke cube when no authored scene is materialized.
pub const FOUNDATION_CUBE_SELECTION: &str = "editor://foundation-cube";

const EPSILON: f32 = 1.0e-6;

/// A world-space pick ray with a normalized direction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewportRay {
    pub origin: [f32; 3],
    pub direction: [f32; 3],
}

/// Intersects a ray with the horizontal plane at `y`.
#[must_use]
pub fn intersect_horizontal_plane(ray: ViewportRay, y: f32) -> Option<[f32; 3]> {
    if !y.is_finite() || ray.direction[1].abs() <= EPSILON {
        return None;
    }
    let distance = (y - ray.origin[1]) / ray.direction[1];
    if !distance.is_finite() || distance < 0.0 {
        return None;
    }
    Some([
        ray.origin[0] + ray.direction[0] * distance,
        y,
        ray.origin[2] + ray.direction[2] * distance,
    ])
}

/// Builds a world-space ray through one viewport-local logical pixel.
///
/// `viewport_size` must match the physical draw region passed to
/// [`Camera3d::view_projection`]. Pointer coordinates use the same normalized
/// placement as logical viewport bounds (`x / logical_width`).
///
/// # Errors
///
/// Returns [`ViewportPickError`] when the viewport is empty or the camera
/// matrix cannot be inverted.
pub fn viewport_ray_from_pointer(
    camera: Camera3d,
    viewport_size: [u32; 2],
    local_x: f64,
    local_y: f64,
    logical_width: f64,
    logical_height: f64,
) -> Result<ViewportRay, ViewportPickError> {
    if viewport_size[0] == 0
        || viewport_size[1] == 0
        || logical_width <= 0.0
        || logical_height <= 0.0
    {
        return Err(ViewportPickError::EmptyViewport);
    }
    let ndc_x = (2.0 * local_x / logical_width - 1.0) as f32;
    let ndc_y = (1.0 - 2.0 * local_y / logical_height) as f32;
    // Validate the camera early so picking stays aligned with rendering.
    camera
        .view_projection(viewport_size)
        .map_err(ViewportPickError::Camera)?;
    let forward =
        normalize3(sub3(camera.target, camera.position)).ok_or(ViewportPickError::DegenerateRay)?;
    let side = normalize3(cross3(forward, camera.up)).ok_or(ViewportPickError::DegenerateRay)?;
    let up = cross3(side, forward);
    let aspect = viewport_size[0] as f32 / viewport_size[1] as f32;
    let tan_half = (camera.vertical_fov_radians * 0.5).tan();
    let direction = normalize3([
        side[0] * ndc_x * aspect * tan_half + up[0] * ndc_y * tan_half + forward[0],
        side[1] * ndc_x * aspect * tan_half + up[1] * ndc_y * tan_half + forward[1],
        side[2] * ndc_x * aspect * tan_half + up[2] * ndc_y * tan_half + forward[2],
    ])
    .ok_or(ViewportPickError::DegenerateRay)?;
    Ok(ViewportRay {
        origin: camera.position,
        direction,
    })
}

/// Returns the nearest positive ray distance to an axis-aligned proxy cube in
/// local model space, transformed by `model_matrix`.
#[must_use]
pub fn ray_hit_proxy_aabb(
    ray: ViewportRay,
    model_matrix: [f32; 16],
    half_extent: f32,
) -> Option<f32> {
    if !half_extent.is_finite() || half_extent <= 0.0 {
        return None;
    }
    let inverse = invert_affine(model_matrix)?;
    let local_origin = transform_point(inverse, ray.origin);
    let local_direction = normalize3(transform_vector(inverse, ray.direction))?;
    ray_local_aabb(local_origin, local_direction, half_extent)
}

/// Picks the closest target among `(selection_id, model_matrix)` pairs.
#[must_use]
pub fn pick_closest_proxy<'a>(
    ray: ViewportRay,
    targets: &'a [(&'a str, [f32; 16])],
    half_extent: f32,
) -> Option<&'a str> {
    let mut closest: Option<(&str, f32)> = None;
    for (id, matrix) in targets {
        let Some(distance) = ray_hit_proxy_aabb(ray, *matrix, half_extent) else {
            continue;
        };
        if closest.is_none_or(|(_, best)| distance < best) {
            closest = Some((*id, distance));
        }
    }
    closest.map(|(id, _)| id)
}

/// Resolves the model matrix used by preview rendering for one ECS entity.
#[must_use]
pub fn entity_model_matrix(
    world: &yuyib_ecs::prelude::World,
    entity: yuyib_ecs::bevy_ecs::entity::Entity,
) -> Option<[f32; 16]> {
    world
        .get::<WorldTransform3d>(entity)
        .map(|transform| transform.column_major())
        .or_else(|| {
            world
                .get::<Transform3d>(entity)
                .map(|transform| transform_matrix(*transform))
        })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewportPickError {
    EmptyViewport,
    DegenerateRay,
    Camera(yuyib_render_3d::MeshRenderError),
}

impl std::fmt::Display for ViewportPickError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyViewport => formatter.write_str("viewport dimensions must be positive"),
            Self::DegenerateRay => formatter.write_str("pick ray direction is degenerate"),
            Self::Camera(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for ViewportPickError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Camera(error) => Some(error),
            _ => None,
        }
    }
}

fn transform_matrix(transform: Transform3d) -> [f32; 16] {
    let [x, y, z, w] = transform.rotation;
    let xx = x * x;
    let yy = y * y;
    let zz = z * z;
    let xy = x * y;
    let xz = x * z;
    let yz = y * z;
    let wx = w * x;
    let wy = w * y;
    let wz = w * z;
    let rotation = [
        1.0 - 2.0 * (yy + zz),
        2.0 * (xy + wz),
        2.0 * (xz - wy),
        0.0,
        2.0 * (xy - wz),
        1.0 - 2.0 * (xx + zz),
        2.0 * (yz + wx),
        0.0,
        2.0 * (xz + wy),
        2.0 * (yz - wx),
        1.0 - 2.0 * (xx + yy),
        0.0,
        0.0,
        0.0,
        0.0,
        1.0,
    ];
    [
        rotation[0] * transform.scale[0],
        rotation[1] * transform.scale[0],
        rotation[2] * transform.scale[0],
        0.0,
        rotation[4] * transform.scale[1],
        rotation[5] * transform.scale[1],
        rotation[6] * transform.scale[1],
        0.0,
        rotation[8] * transform.scale[2],
        rotation[9] * transform.scale[2],
        rotation[10] * transform.scale[2],
        0.0,
        transform.translation[0],
        transform.translation[1],
        transform.translation[2],
        1.0,
    ]
}

fn ray_local_aabb(origin: [f32; 3], direction: [f32; 3], half_extent: f32) -> Option<f32> {
    let mut t_min = f32::NEG_INFINITY;
    let mut t_max = f32::INFINITY;
    for axis in 0..3 {
        let origin_axis = origin[axis];
        let direction_axis = direction[axis];
        let min_bound = -half_extent;
        let max_bound = half_extent;
        if direction_axis.abs() <= EPSILON {
            if origin_axis < min_bound || origin_axis > max_bound {
                return None;
            }
            continue;
        }
        let t1 = (min_bound - origin_axis) / direction_axis;
        let t2 = (max_bound - origin_axis) / direction_axis;
        let (near, far) = if t1 <= t2 { (t1, t2) } else { (t2, t1) };
        t_min = t_min.max(near);
        t_max = t_max.min(far);
        if t_min > t_max {
            return None;
        }
    }
    if t_max < 0.0 {
        return None;
    }
    Some(if t_min >= 0.0 { t_min } else { t_max })
}

fn transform_point(matrix: [f32; 16], point: [f32; 3]) -> [f32; 3] {
    let result = multiply_matrix4_vec4(matrix, [point[0], point[1], point[2], 1.0]);
    [result[0], result[1], result[2]]
}

fn transform_vector(matrix: [f32; 16], vector: [f32; 3]) -> [f32; 3] {
    let result = multiply_matrix4_vec4(matrix, [vector[0], vector[1], vector[2], 0.0]);
    [result[0], result[1], result[2]]
}

fn multiply_matrix4_vec4(matrix: [f32; 16], vector: [f32; 4]) -> [f32; 4] {
    [
        matrix[0] * vector[0]
            + matrix[4] * vector[1]
            + matrix[8] * vector[2]
            + matrix[12] * vector[3],
        matrix[1] * vector[0]
            + matrix[5] * vector[1]
            + matrix[9] * vector[2]
            + matrix[13] * vector[3],
        matrix[2] * vector[0]
            + matrix[6] * vector[1]
            + matrix[10] * vector[2]
            + matrix[14] * vector[3],
        matrix[3] * vector[0]
            + matrix[7] * vector[1]
            + matrix[11] * vector[2]
            + matrix[15] * vector[3],
    ]
}

fn invert_affine(matrix: [f32; 16]) -> Option<[f32; 16]> {
    let determinant = matrix[0] * (matrix[5] * matrix[10] - matrix[9] * matrix[6])
        - matrix[4] * (matrix[1] * matrix[10] - matrix[9] * matrix[2])
        + matrix[8] * (matrix[1] * matrix[6] - matrix[5] * matrix[2]);
    if !determinant.is_finite() || determinant.abs() <= EPSILON {
        return None;
    }
    let inverse = 1.0 / determinant;
    let inverse_linear = [
        (matrix[5] * matrix[10] - matrix[9] * matrix[6]) * inverse,
        (matrix[8] * matrix[6] - matrix[4] * matrix[10]) * inverse,
        (matrix[4] * matrix[9] - matrix[8] * matrix[5]) * inverse,
        (matrix[9] * matrix[2] - matrix[1] * matrix[10]) * inverse,
        (matrix[0] * matrix[10] - matrix[8] * matrix[2]) * inverse,
        (matrix[8] * matrix[1] - matrix[0] * matrix[9]) * inverse,
        (matrix[1] * matrix[6] - matrix[5] * matrix[2]) * inverse,
        (matrix[4] * matrix[2] - matrix[0] * matrix[6]) * inverse,
        (matrix[0] * matrix[5] - matrix[4] * matrix[1]) * inverse,
    ];
    let translation = [matrix[12], matrix[13], matrix[14]];
    Some([
        inverse_linear[0],
        inverse_linear[1],
        inverse_linear[2],
        0.0,
        inverse_linear[3],
        inverse_linear[4],
        inverse_linear[5],
        0.0,
        inverse_linear[6],
        inverse_linear[7],
        inverse_linear[8],
        0.0,
        -(inverse_linear[0] * translation[0]
            + inverse_linear[3] * translation[1]
            + inverse_linear[6] * translation[2]),
        -(inverse_linear[1] * translation[0]
            + inverse_linear[4] * translation[1]
            + inverse_linear[7] * translation[2]),
        -(inverse_linear[2] * translation[0]
            + inverse_linear[5] * translation[1]
            + inverse_linear[8] * translation[2]),
        1.0,
    ])
}

fn sub3(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
}

fn cross3(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [
        left[1] * right[2] - left[2] * right[1],
        left[2] * right[0] - left[0] * right[2],
        left[0] * right[1] - left[1] * right[0],
    ]
}

fn normalize3(vector: [f32; 3]) -> Option<[f32; 3]> {
    let length_squared = vector[0] * vector[0] + vector[1] * vector[1] + vector[2] * vector[2];
    if !length_squared.is_finite() || length_squared <= EPSILON {
        return None;
    }
    let inverse = length_squared.sqrt().recip();
    Some([
        vector[0] * inverse,
        vector[1] * inverse,
        vector[2] * inverse,
    ])
}

#[cfg(test)]
mod tests {
    use yuyib_ecs::prelude::World;
    use yuyib_render_3d::Camera3d;

    use super::*;

    fn orbit_camera() -> Camera3d {
        let yaw = 0.0_f32;
        let pitch: f32 = 0.2;
        let radius = 3.0;
        let target = [0.0, 0.0, 0.0];
        let (yaw_sin, yaw_cos) = yaw.sin_cos();
        let (pitch_sin, pitch_cos) = pitch.sin_cos();
        Camera3d::new(
            [
                target[0] + radius * yaw_sin * pitch_cos,
                target[1] + radius * pitch_sin,
                target[2] + radius * yaw_cos * pitch_cos,
            ],
            target,
            [0.0, 1.0, 0.0],
            std::f32::consts::FRAC_PI_3,
            0.1,
            1_000.0,
        )
    }

    #[test]
    fn viewport_center_ray_hits_origin_proxy_cube() {
        let camera = orbit_camera();
        let viewport = [800, 600];
        let ray =
            viewport_ray_from_pointer(camera, viewport, 400.0, 300.0, 800.0, 600.0).expect("ray");
        let matrix = Transform3d::default();
        let hit = ray_hit_proxy_aabb(ray, transform_matrix(matrix), PROXY_CUBE_HALF_EXTENT);
        assert!(hit.is_some(), "center click should hit the origin cube");
    }

    #[test]
    fn viewport_corner_ray_misses_origin_proxy_cube() {
        let camera = orbit_camera();
        let viewport = [800, 600];
        let ray = viewport_ray_from_pointer(camera, viewport, 4.0, 4.0, 800.0, 600.0).expect("ray");
        let matrix = Transform3d::default();
        assert!(
            ray_hit_proxy_aabb(ray, transform_matrix(matrix), PROXY_CUBE_HALF_EXTENT).is_none()
        );
    }

    #[test]
    fn pick_closest_prefers_nearest_materialized_proxy() {
        let camera = orbit_camera();
        let viewport = [800, 600];
        let ray =
            viewport_ray_from_pointer(camera, viewport, 400.0, 300.0, 800.0, 600.0).expect("ray");
        let near = transform_matrix(Transform3d::from_translation([0.0, 0.0, 0.0]));
        let far = transform_matrix(Transform3d::from_translation([0.0, 0.0, -12.0]));
        let targets = [("near", near), ("far", far)];
        let picked = pick_closest_proxy(ray, &targets, PROXY_CUBE_HALF_EXTENT);
        assert_eq!(picked, Some("near"));
    }

    #[test]
    fn entity_model_matrix_reads_transform3d() {
        let mut world = World::new();
        let entity = world
            .spawn(Transform3d::from_translation([2.0, 0.0, 0.0]))
            .id();
        let matrix = entity_model_matrix(&world, entity).expect("matrix");
        assert_eq!(&matrix[12..15], &[2.0, 0.0, 0.0]);
    }

    #[test]
    fn horizontal_plane_intersection_preserves_requested_height() {
        let ray = ViewportRay {
            origin: [0.0, 3.0, 0.0],
            direction: [0.0, -1.0, 0.0],
        };
        assert_eq!(intersect_horizontal_plane(ray, 1.5), Some([0.0, 1.5, 0.0]));
    }
}
