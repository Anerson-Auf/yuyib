//! Axis-constrained viewport gizmo math.

use crate::viewport_picking::ViewportRay;

const EPSILON: f32 = 1.0e-6;

/// A world-space principal gizmo axis.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GizmoAxis {
    X,
    Y,
    Z,
}

impl GizmoAxis {
    /// Returns this axis as a unit world-space vector.
    #[must_use]
    pub const fn as_vec3(self) -> [f32; 3] {
        match self {
            Self::X => [1.0, 0.0, 0.0],
            Self::Y => [0.0, 1.0, 0.0],
            Self::Z => [0.0, 0.0, 1.0],
        }
    }

    /// Returns the conventional RGB color for this axis.
    #[must_use]
    pub const fn color_rgb(self) -> [f32; 3] {
        match self {
            Self::X => [0.95, 0.2, 0.2],
            Self::Y => [0.25, 0.85, 0.3],
            Self::Z => [0.25, 0.45, 0.95],
        }
    }
}

/// The transform operation performed by a viewport gizmo.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GizmoToolKind {
    Move,
    Rotate,
    Scale,
}

/// Picks the nearest principal-axis handle represented by a thin AABB.
#[must_use]
pub fn pick_axis_handle(
    ray: ViewportRay,
    origin: [f32; 3],
    axis_length: f32,
    handle_radius: f32,
) -> Option<GizmoAxis> {
    if !axis_length.is_finite()
        || !handle_radius.is_finite()
        || axis_length <= 0.0
        || handle_radius <= 0.0
    {
        return None;
    }
    [GizmoAxis::X, GizmoAxis::Y, GizmoAxis::Z]
        .into_iter()
        .filter_map(|axis| {
            axis_handle_hit(ray, origin, axis, axis_length, handle_radius).map(|t| (axis, t))
        })
        .min_by(|(_, left), (_, right)| left.total_cmp(right))
        .map(|(axis, _)| axis)
}

/// Result of picking a Unity-style move/scale gizmo (shaft, tip, or free-move centre).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GizmoPick {
    pub axis: GizmoAxis,
    /// `true` when constrained to `axis`; `false` for the free-plane centre handle.
    pub axis_constrained: bool,
}

/// Picks arrow shafts/tips first, then an optional free-move centre sphere.
///
/// Returns `None` when the ray misses every handle — callers must not fall back to
/// unconstrained drag (that is what made raw LMB feel buggy).
#[must_use]
pub fn pick_arrow_gizmo(
    ray: ViewportRay,
    origin: [f32; 3],
    axis_length: f32,
    shaft_radius: f32,
    tip_radius: f32,
    centre_radius: Option<f32>,
) -> Option<GizmoPick> {
    if !axis_length.is_finite()
        || !shaft_radius.is_finite()
        || !tip_radius.is_finite()
        || axis_length <= 0.0
        || shaft_radius <= 0.0
        || tip_radius <= 0.0
    {
        return None;
    }

    let mut best: Option<(GizmoAxis, f32)> = None;
    for axis in [GizmoAxis::X, GizmoAxis::Y, GizmoAxis::Z] {
        let tip = [
            origin[0] + axis.as_vec3()[0] * axis_length,
            origin[1] + axis.as_vec3()[1] * axis_length,
            origin[2] + axis.as_vec3()[2] * axis_length,
        ];
        if let Some(t) = ray_sphere(ray, tip, tip_radius) {
            best = match best {
                Some((_, previous)) if previous <= t => best,
                _ => Some((axis, t)),
            };
        }
        if let Some(t) = axis_handle_hit(ray, origin, axis, axis_length, shaft_radius) {
            best = match best {
                Some((_, previous)) if previous <= t => best,
                _ => Some((axis, t)),
            };
        }
    }
    if let Some((axis, _)) = best {
        return Some(GizmoPick {
            axis,
            axis_constrained: true,
        });
    }

    let Some(radius) = centre_radius.filter(|value| value.is_finite() && *value > 0.0) else {
        return None;
    };
    ray_sphere(ray, origin, radius).map(|_| GizmoPick {
        axis: GizmoAxis::Y,
        axis_constrained: false,
    })
}

/// Picks the nearest rotation ring (circle in the plane perpendicular to an axis).
#[must_use]
pub fn pick_rotation_gizmo(
    ray: ViewportRay,
    origin: [f32; 3],
    radius: f32,
    tube_radius: f32,
) -> Option<GizmoPick> {
    if !radius.is_finite() || !tube_radius.is_finite() || radius <= 0.0 || tube_radius <= 0.0 {
        return None;
    }
    [GizmoAxis::X, GizmoAxis::Y, GizmoAxis::Z]
        .into_iter()
        .filter_map(|axis| {
            rotation_ring_hit(ray, origin, axis, radius, tube_radius).map(|t| (axis, t))
        })
        .min_by(|(_, left), (_, right)| left.total_cmp(right))
        .map(|(axis, _)| GizmoPick {
            axis,
            axis_constrained: true,
        })
}

fn rotation_ring_hit(
    ray: ViewportRay,
    origin: [f32; 3],
    axis: GizmoAxis,
    radius: f32,
    tube_radius: f32,
) -> Option<f32> {
    let normal = axis.as_vec3();
    let denom = dot3(ray.direction, normal);
    if denom.abs() <= EPSILON {
        return None;
    }
    let t = dot3(sub3(origin, ray.origin), normal) / denom;
    if !t.is_finite() || t < 0.0 {
        return None;
    }
    let hit = [
        ray.origin[0] + ray.direction[0] * t,
        ray.origin[1] + ray.direction[1] * t,
        ray.origin[2] + ray.direction[2] * t,
    ];
    let radial = sub3(hit, origin);
    let dist = (radial[0] * radial[0] + radial[1] * radial[1] + radial[2] * radial[2]).sqrt();
    ((dist - radius).abs() <= tube_radius * 2.8).then_some(t)
}

/// Finds the signed axis distance of the point on the ray nearest the axis.
#[must_use]
pub fn axis_parameter(ray: ViewportRay, origin: [f32; 3], axis: GizmoAxis) -> Option<f32> {
    let direction = ray.direction;
    let axis_vector = axis.as_vec3();
    let offset = sub3(ray.origin, origin);
    let ray_axis_dot = dot3(direction, axis_vector);
    let denominator = dot3(direction, direction) - ray_axis_dot * ray_axis_dot;
    if !denominator.is_finite() || denominator.abs() <= EPSILON {
        return None;
    }
    let parameter =
        (dot3(axis_vector, offset) - ray_axis_dot * dot3(direction, offset)) / denominator;
    parameter.is_finite().then_some(parameter)
}

/// Applies an axis-angle delta in local space (`start * delta`).
#[must_use]
pub fn rotate_quat(start: [f32; 4], axis: GizmoAxis, delta_radians: f32) -> [f32; 4] {
    let half = delta_radians * 0.5;
    let [x, y, z] = axis.as_vec3();
    let delta = [x * half.sin(), y * half.sin(), z * half.sin(), half.cos()];
    normalize_quat(multiply_quat(start, delta)).unwrap_or(start)
}

/// Adds `delta` to one scale axis, clamping the result away from zero.
#[must_use]
pub fn apply_axis_scale(mut start: [f32; 3], axis: GizmoAxis, delta: f32) -> [f32; 3] {
    let index = match axis {
        GizmoAxis::X => 0,
        GizmoAxis::Y => 1,
        GizmoAxis::Z => 2,
    };
    let sign = if start[index].is_sign_negative() {
        -1.0
    } else {
        1.0
    };
    start[index] = (start[index] + delta).abs().max(0.001) * sign;
    start
}

fn axis_handle_hit(
    ray: ViewportRay,
    origin: [f32; 3],
    axis: GizmoAxis,
    length: f32,
    radius: f32,
) -> Option<f32> {
    let minimum = [origin[0] - radius, origin[1] - radius, origin[2] - radius];
    let mut maximum = [origin[0] + radius, origin[1] + radius, origin[2] + radius];
    match axis {
        GizmoAxis::X => maximum[0] = origin[0] + length,
        GizmoAxis::Y => maximum[1] = origin[1] + length,
        GizmoAxis::Z => maximum[2] = origin[2] + length,
    }
    ray_aabb(ray, minimum, maximum)
}

fn ray_sphere(ray: ViewportRay, centre: [f32; 3], radius: f32) -> Option<f32> {
    let offset = sub3(ray.origin, centre);
    let direction = ray.direction;
    let a = dot3(direction, direction);
    if !a.is_finite() || a <= EPSILON {
        return None;
    }
    let b = 2.0 * dot3(offset, direction);
    let c = dot3(offset, offset) - radius * radius;
    let discriminant = b * b - 4.0 * a * c;
    if !discriminant.is_finite() || discriminant < 0.0 {
        return None;
    }
    let root = discriminant.sqrt();
    let t0 = (-b - root) / (2.0 * a);
    let t1 = (-b + root) / (2.0 * a);
    [t0, t1]
        .into_iter()
        .filter(|t| t.is_finite() && *t >= 0.0)
        .min_by(|left, right| left.total_cmp(right))
}

fn ray_aabb(ray: ViewportRay, minimum: [f32; 3], maximum: [f32; 3]) -> Option<f32> {
    let mut near = f32::NEG_INFINITY;
    let mut far = f32::INFINITY;
    for index in 0..3 {
        let direction = ray.direction[index];
        if direction.abs() <= EPSILON {
            if ray.origin[index] < minimum[index] || ray.origin[index] > maximum[index] {
                return None;
            }
            continue;
        }
        let first = (minimum[index] - ray.origin[index]) / direction;
        let second = (maximum[index] - ray.origin[index]) / direction;
        near = near.max(first.min(second));
        far = far.min(first.max(second));
        if near > far {
            return None;
        }
    }
    (far >= 0.0).then_some(near.max(0.0))
}

fn multiply_quat(left: [f32; 4], right: [f32; 4]) -> [f32; 4] {
    [
        left[3] * right[0] + left[0] * right[3] + left[1] * right[2] - left[2] * right[1],
        left[3] * right[1] - left[0] * right[2] + left[1] * right[3] + left[2] * right[0],
        left[3] * right[2] + left[0] * right[1] - left[1] * right[0] + left[2] * right[3],
        left[3] * right[3] - left[0] * right[0] - left[1] * right[1] - left[2] * right[2],
    ]
}

fn normalize_quat(quaternion: [f32; 4]) -> Option<[f32; 4]> {
    let length_squared = quaternion.iter().map(|value| value * value).sum::<f32>();
    (length_squared.is_finite() && length_squared > EPSILON).then(|| {
        let inverse = length_squared.sqrt().recip();
        quaternion.map(|value| value * inverse)
    })
}

fn dot3(left: [f32; 3], right: [f32; 3]) -> f32 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

fn sub3(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_nearest_axis_handle() {
        let ray = ViewportRay {
            origin: [2.0, 0.0, 0.0],
            direction: [-1.0, 0.0, 0.0],
        };
        assert_eq!(
            pick_axis_handle(ray, [0.0, 0.0, 0.0], 1.0, 0.05),
            Some(GizmoAxis::X)
        );
    }

    #[test]
    fn arrow_gizmo_prefers_tip_and_ignores_empty_space() {
        let tip_ray = ViewportRay {
            origin: [1.2, 0.0, 0.0],
            direction: [-1.0, 0.0, 0.0],
        };
        assert_eq!(
            pick_arrow_gizmo(tip_ray, [0.0, 0.0, 0.0], 1.0, 0.05, 0.12, Some(0.1)),
            Some(GizmoPick {
                axis: GizmoAxis::X,
                axis_constrained: true,
            })
        );

        let miss = ViewportRay {
            origin: [0.0, 5.0, 0.0],
            direction: [0.0, 0.0, -1.0],
        };
        assert_eq!(
            pick_arrow_gizmo(miss, [0.0, 0.0, 0.0], 1.0, 0.05, 0.12, Some(0.1)),
            None
        );
    }

    #[test]
    fn rotation_ring_picks_matching_axis() {
        let ray = ViewportRay {
            origin: [1.0, 2.0, 0.0],
            direction: [0.0, -1.0, 0.0],
        };
        assert_eq!(
            pick_rotation_gizmo(ray, [0.0, 0.0, 0.0], 1.0, 0.15),
            Some(GizmoPick {
                axis: GizmoAxis::Y,
                axis_constrained: true,
            })
        );
    }

    #[test]
    fn rotates_identity_around_y() {
        let rotation = rotate_quat([0.0, 0.0, 0.0, 1.0], GizmoAxis::Y, std::f32::consts::PI);
        assert!((rotation[1].abs() - 1.0).abs() < 1.0e-5);
        assert!(rotation[3].abs() < 1.0e-5);
    }

    #[test]
    fn scales_only_requested_axis_and_never_reaches_zero() {
        assert_eq!(
            apply_axis_scale([1.0, 2.0, 3.0], GizmoAxis::Y, 0.5),
            [1.0, 2.5, 3.0]
        );
        assert!(apply_axis_scale([1.0, 1.0, 1.0], GizmoAxis::X, -5.0)[0] > 0.0);
    }
}
