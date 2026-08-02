//! Deterministic static navigation over imported triangle-map geometry.
//!
//! This module deliberately stops short of a full navmesh. It extracts
//! upward-facing walkable source triangles, connects nearly shared edges, and
//! provides bounded nearest-point and graph queries. It does not merge
//! polygons, smooth paths, update moving obstacles or perform agent-radius
//! erosion. Those layers can consume this stable low-level graph later.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    error::Error,
    fmt,
};

use yuyib_physics::{TriangleMesh3d, Vec3};

const DEFAULT_MAXIMUM_SOURCE_TRIANGLES: usize = 2_000_000;
const DEFAULT_MAXIMUM_WALKABLE_TRIANGLES: usize = 1_000_000;
const DEFAULT_MAXIMUM_ADJACENCIES: usize = 4_000_000;
const DEFAULT_MAXIMUM_NEIGHBORS: usize = 32;
const DEFAULT_MAXIMUM_EDGE_BUCKET_ENTRIES: usize = 256;
const DEFAULT_MAXIMUM_EDGE_CANDIDATE_TESTS: usize = 64_000_000;
const DEFAULT_MAXIMUM_SPATIAL_REFERENCES: usize = 8_000_000;
const DEFAULT_MAXIMUM_CELLS_PER_TRIANGLE: usize = 4_096;

/// Hard construction limits for a static walkable-surface snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WalkableSurfaceBuildLimits3d {
    /// Maximum source triangles inspected from the collision mesh.
    pub maximum_source_triangles: usize,
    /// Maximum triangles accepted as walkable.
    pub maximum_walkable_triangles: usize,
    /// Maximum undirected graph edges.
    pub maximum_adjacencies: usize,
    /// Maximum neighbors retained by one walkable triangle.
    pub maximum_neighbors_per_triangle: usize,
    /// Maximum edge references sharing one adjacency-grid bucket.
    pub maximum_edge_bucket_entries: usize,
    /// Maximum geometric edge-pair tests during adjacency construction.
    pub maximum_edge_candidate_tests: usize,
    /// Maximum triangle references stored by the nearest-point grid.
    pub maximum_spatial_references: usize,
    /// Maximum nearest-point grid cells covered by one triangle.
    pub maximum_cells_per_triangle: usize,
}

impl Default for WalkableSurfaceBuildLimits3d {
    fn default() -> Self {
        Self {
            maximum_source_triangles: DEFAULT_MAXIMUM_SOURCE_TRIANGLES,
            maximum_walkable_triangles: DEFAULT_MAXIMUM_WALKABLE_TRIANGLES,
            maximum_adjacencies: DEFAULT_MAXIMUM_ADJACENCIES,
            maximum_neighbors_per_triangle: DEFAULT_MAXIMUM_NEIGHBORS,
            maximum_edge_bucket_entries: DEFAULT_MAXIMUM_EDGE_BUCKET_ENTRIES,
            maximum_edge_candidate_tests: DEFAULT_MAXIMUM_EDGE_CANDIDATE_TESTS,
            maximum_spatial_references: DEFAULT_MAXIMUM_SPATIAL_REFERENCES,
            maximum_cells_per_triangle: DEFAULT_MAXIMUM_CELLS_PER_TRIANGLE,
        }
    }
}

/// Geometric policy and hard limits for static walkable extraction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WalkableSurfaceConfig3d {
    maximum_slope_radians: f32,
    maximum_step_height: f32,
    edge_tolerance: f32,
    spatial_cell_size: f32,
    limits: WalkableSurfaceBuildLimits3d,
}

impl WalkableSurfaceConfig3d {
    /// Creates a validated navigation policy.
    ///
    /// `maximum_slope_radians` is measured from world up and must be in
    /// `[0, π/2)`. Two projected edge endpoints may differ by at most
    /// `edge_tolerance` in XZ and `maximum_step_height` vertically.
    ///
    /// # Errors
    ///
    /// Returns [`WalkableSurfaceConfigError3d`] for non-finite or out-of-range
    /// geometry settings.
    pub fn new(
        maximum_slope_radians: f32,
        maximum_step_height: f32,
        edge_tolerance: f32,
        spatial_cell_size: f32,
    ) -> Result<Self, WalkableSurfaceConfigError3d> {
        if !maximum_slope_radians.is_finite()
            || !(0.0..std::f32::consts::FRAC_PI_2).contains(&maximum_slope_radians)
        {
            return Err(WalkableSurfaceConfigError3d::InvalidMaximumSlope);
        }
        if !maximum_step_height.is_finite() || maximum_step_height < 0.0 {
            return Err(WalkableSurfaceConfigError3d::InvalidStepHeight);
        }
        if !edge_tolerance.is_finite() || edge_tolerance <= 0.0 {
            return Err(WalkableSurfaceConfigError3d::InvalidEdgeTolerance);
        }
        if !spatial_cell_size.is_finite() || spatial_cell_size <= 0.0 {
            return Err(WalkableSurfaceConfigError3d::InvalidSpatialCellSize);
        }
        Ok(Self {
            maximum_slope_radians,
            maximum_step_height,
            edge_tolerance,
            spatial_cell_size,
            limits: WalkableSurfaceBuildLimits3d::default(),
        })
    }

    /// Replaces construction limits. They are checked before allocation/work.
    #[must_use]
    pub const fn with_limits(mut self, limits: WalkableSurfaceBuildLimits3d) -> Self {
        self.limits = limits;
        self
    }

    /// Returns the maximum authored slope from world up.
    #[must_use]
    pub const fn maximum_slope_radians(self) -> f32 {
        self.maximum_slope_radians
    }

    /// Returns the maximum vertical endpoint difference for adjacency.
    #[must_use]
    pub const fn maximum_step_height(self) -> f32 {
        self.maximum_step_height
    }

    /// Returns projected edge endpoint tolerance.
    #[must_use]
    pub const fn edge_tolerance(self) -> f32 {
        self.edge_tolerance
    }

    /// Returns nearest-point grid cell size.
    #[must_use]
    pub const fn spatial_cell_size(self) -> f32 {
        self.spatial_cell_size
    }

    /// Returns hard construction limits.
    #[must_use]
    pub const fn limits(self) -> WalkableSurfaceBuildLimits3d {
        self.limits
    }
}

impl Default for WalkableSurfaceConfig3d {
    fn default() -> Self {
        Self::new(50.0_f32.to_radians(), 0.45, 0.02, 4.0)
            .expect("built-in walkable-surface settings are valid")
    }
}

/// Invalid geometric configuration for walkable-surface extraction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WalkableSurfaceConfigError3d {
    /// Slope is non-finite, negative or at least 90 degrees.
    InvalidMaximumSlope,
    /// Step height is non-finite or negative.
    InvalidStepHeight,
    /// Edge tolerance is non-finite or not positive.
    InvalidEdgeTolerance,
    /// Spatial cell size is non-finite or not positive.
    InvalidSpatialCellSize,
    /// One hard construction limit is zero.
    ZeroLimit {
        /// Stable configuration field name.
        field: &'static str,
    },
    /// A limit cannot be represented by compact graph indices.
    LimitTooLarge {
        /// Stable configuration field name.
        field: &'static str,
        /// Requested value.
        actual: usize,
        /// Largest representable value.
        maximum: usize,
    },
}

impl fmt::Display for WalkableSurfaceConfigError3d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMaximumSlope => {
                formatter.write_str("maximum walkable slope must be finite and in [0, pi/2)")
            }
            Self::InvalidStepHeight => {
                formatter.write_str("maximum step height must be finite and non-negative")
            }
            Self::InvalidEdgeTolerance => {
                formatter.write_str("navigation edge tolerance must be finite and positive")
            }
            Self::InvalidSpatialCellSize => {
                formatter.write_str("navigation spatial cell size must be finite and positive")
            }
            Self::ZeroLimit { field } => {
                write!(formatter, "navigation limit {field} must be positive")
            }
            Self::LimitTooLarge {
                field,
                actual,
                maximum,
            } => write!(
                formatter,
                "navigation limit {field} is {actual}; compact graph maximum is {maximum}"
            ),
        }
    }
}

impl Error for WalkableSurfaceConfigError3d {}

/// Construction failure for a bounded static walkable surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WalkableSurfaceBuildError3d {
    /// Geometric policy or one hard limit is invalid.
    Config(WalkableSurfaceConfigError3d),
    /// Source collision geometry exceeds the inspection budget.
    SourceTriangleLimitExceeded {
        /// Configured maximum.
        maximum: usize,
        /// Source triangle count.
        actual: usize,
    },
    /// Slope filtering accepted more triangles than permitted.
    WalkableTriangleLimitExceeded {
        /// Configured maximum.
        maximum: usize,
    },
    /// A coordinate cannot be represented by the bounded i32 grid.
    CoordinateOutsideGrid {
        /// Source triangle associated with the coordinate.
        source_triangle: usize,
    },
    /// One large triangle spans too many nearest-point cells.
    TriangleCellLimitExceeded {
        /// Source triangle associated with the span.
        source_triangle: usize,
        /// Configured maximum.
        maximum: usize,
        /// Required cells.
        actual: usize,
    },
    /// Total nearest-point grid references exceed the configured bound.
    SpatialReferenceLimitExceeded {
        /// Configured maximum.
        maximum: usize,
    },
    /// A pathological edge bucket exceeds its local bound.
    EdgeBucketLimitExceeded {
        /// Configured maximum.
        maximum: usize,
    },
    /// Adjacency candidate work exceeds its global bound.
    EdgeCandidateLimitExceeded {
        /// Configured maximum.
        maximum: usize,
    },
    /// One triangle would exceed its neighbor bound.
    NeighborLimitExceeded {
        /// Walkable triangle index.
        triangle: u32,
        /// Configured maximum.
        maximum: usize,
    },
    /// The undirected graph exceeds its global edge bound.
    AdjacencyLimitExceeded {
        /// Configured maximum.
        maximum: usize,
    },
}

impl fmt::Display for WalkableSurfaceBuildError3d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(error) => write!(formatter, "invalid walkable-surface config: {error}"),
            Self::SourceTriangleLimitExceeded { maximum, actual } => write!(
                formatter,
                "collision mesh has {actual} triangles; navigation source limit is {maximum}"
            ),
            Self::WalkableTriangleLimitExceeded { maximum } => write!(
                formatter,
                "walkable triangle count exceeds configured maximum {maximum}"
            ),
            Self::CoordinateOutsideGrid { source_triangle } => write!(
                formatter,
                "source triangle {source_triangle} lies outside the navigation grid coordinate range"
            ),
            Self::TriangleCellLimitExceeded {
                source_triangle,
                maximum,
                actual,
            } => write!(
                formatter,
                "source triangle {source_triangle} spans {actual} grid cells; maximum is {maximum}"
            ),
            Self::SpatialReferenceLimitExceeded { maximum } => write!(
                formatter,
                "navigation spatial references exceed configured maximum {maximum}"
            ),
            Self::EdgeBucketLimitExceeded { maximum } => write!(
                formatter,
                "navigation edge bucket exceeds configured maximum {maximum}"
            ),
            Self::EdgeCandidateLimitExceeded { maximum } => write!(
                formatter,
                "navigation adjacency exceeds edge-candidate test maximum {maximum}"
            ),
            Self::NeighborLimitExceeded { triangle, maximum } => write!(
                formatter,
                "walkable triangle {triangle} exceeds neighbor maximum {maximum}"
            ),
            Self::AdjacencyLimitExceeded { maximum } => write!(
                formatter,
                "navigation graph exceeds adjacency maximum {maximum}"
            ),
        }
    }
}

impl Error for WalkableSurfaceBuildError3d {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Config(error) => Some(error),
            _ => None,
        }
    }
}

/// Construction telemetry for one immutable walkable surface.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WalkableSurfaceBuildStats3d {
    /// Source triangles inspected.
    pub source_triangles: usize,
    /// Downward or too-steep triangles rejected.
    pub rejected_by_slope: usize,
    /// Upward triangles retained.
    pub walkable_triangles: usize,
    /// Potential edge pairs tested geometrically.
    pub edge_candidates_tested: usize,
    /// Undirected adjacency links created.
    pub adjacencies: usize,
    /// Occupied nearest-point grid cells.
    pub spatial_cells: usize,
    /// Triangle references stored across those cells.
    pub spatial_references: usize,
}

/// One upward walkable source triangle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WalkableTriangle3d {
    source_triangle: usize,
    vertices: [Vec3; 3],
}

impl WalkableTriangle3d {
    /// Returns the triangle ordinal in [`TriangleMesh3d::triangles`].
    #[must_use]
    pub const fn source_triangle(self) -> usize {
        self.source_triangle
    }

    /// Returns immutable world-space triangle vertices.
    #[must_use]
    pub const fn vertices(self) -> [Vec3; 3] {
        self.vertices
    }

    /// Returns the normalized upward geometric normal.
    #[must_use]
    pub fn normal(self) -> Vec3 {
        normalized(cross(
            self.vertices[1] - self.vertices[0],
            self.vertices[2] - self.vertices[0],
        ))
    }

    /// Returns the arithmetic world-space centre.
    #[must_use]
    pub fn centre(self) -> Vec3 {
        (self.vertices[0] + self.vertices[1] + self.vertices[2]) * (1.0 / 3.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct GridCell3d {
    x: i32,
    z: i32,
}

/// Immutable walkable triangle graph and nearest-point acceleration grid.
#[derive(Clone, Debug)]
pub struct WalkableSurface3d {
    triangles: Vec<WalkableTriangle3d>,
    neighbor_offsets: Vec<u32>,
    neighbors: Vec<u32>,
    spatial: HashMap<GridCell3d, Vec<u32>>,
    spatial_cell_size: f32,
    stats: WalkableSurfaceBuildStats3d,
}

impl crate::StaticSceneCollider3d {
    /// Builds a bounded navigation snapshot from this imported static collider.
    ///
    /// The returned surface is independent of the collider and may be shared
    /// with navigation workers while collision queries continue.
    ///
    /// # Errors
    ///
    /// Returns [`WalkableSurfaceBuildError3d`] when geometry or configured
    /// construction work exceeds an explicit bound.
    pub fn build_walkable_surface(
        &self,
        config: WalkableSurfaceConfig3d,
    ) -> Result<WalkableSurface3d, WalkableSurfaceBuildError3d> {
        WalkableSurface3d::from_triangle_mesh(self.mesh(), config)
    }
}

impl WalkableSurface3d {
    /// Extracts upward triangles and builds deterministic static adjacency.
    ///
    /// For an imported scene collider, pass
    /// [`StaticSceneCollider3d::mesh`](crate::StaticSceneCollider3d::mesh)
    /// directly; navigation then retains its own immutable bounded snapshot.
    ///
    /// Adjacency requires both edge endpoints to match in XZ within
    /// `edge_tolerance` (forward or reversed) and in Y within
    /// `maximum_step_height`. T-junctions and partially overlapping edges are
    /// intentionally not inferred by this foundation.
    ///
    /// # Errors
    ///
    /// Returns [`WalkableSurfaceBuildError3d`] before any configured work or
    /// memory bound is exceeded.
    pub fn from_triangle_mesh(
        mesh: &TriangleMesh3d,
        config: WalkableSurfaceConfig3d,
    ) -> Result<Self, WalkableSurfaceBuildError3d> {
        validate_limits(config.limits).map_err(WalkableSurfaceBuildError3d::Config)?;
        if mesh.triangles().len() > config.limits.maximum_source_triangles {
            return Err(WalkableSurfaceBuildError3d::SourceTriangleLimitExceeded {
                maximum: config.limits.maximum_source_triangles,
                actual: mesh.triangles().len(),
            });
        }
        let slope_cosine = config.maximum_slope_radians.cos();
        let mut triangles = Vec::new();
        let mut stats = WalkableSurfaceBuildStats3d {
            source_triangles: mesh.triangles().len(),
            ..WalkableSurfaceBuildStats3d::default()
        };
        for (source_triangle, vertices) in mesh.triangles().iter().copied().enumerate() {
            let normal = normalized(cross(vertices[1] - vertices[0], vertices[2] - vertices[0]));
            if normal.y < slope_cosine {
                stats.rejected_by_slope += 1;
                continue;
            }
            if triangles.len() == config.limits.maximum_walkable_triangles {
                return Err(WalkableSurfaceBuildError3d::WalkableTriangleLimitExceeded {
                    maximum: config.limits.maximum_walkable_triangles,
                });
            }
            triangles.push(WalkableTriangle3d {
                source_triangle,
                vertices,
            });
        }
        stats.walkable_triangles = triangles.len();

        let adjacency = build_adjacency(&triangles, config, &mut stats)?;
        let (neighbor_offsets, neighbors) = compact_adjacency(adjacency);
        let spatial = build_spatial_grid(&triangles, config, &mut stats)?;
        Ok(Self {
            triangles,
            neighbor_offsets,
            neighbors,
            spatial,
            spatial_cell_size: config.spatial_cell_size,
            stats,
        })
    }

    /// Returns walkable triangles in ascending source-triangle order.
    #[must_use]
    pub fn triangles(&self) -> &[WalkableTriangle3d] {
        &self.triangles
    }

    /// Returns construction telemetry.
    #[must_use]
    pub const fn build_stats(&self) -> WalkableSurfaceBuildStats3d {
        self.stats
    }

    /// Returns whether no upward triangles were extracted.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.triangles.is_empty()
    }

    /// Returns neighbors in ascending walkable-triangle index order.
    ///
    /// # Errors
    ///
    /// Returns [`WalkableQueryError3d::MissingTriangle`] for an invalid index.
    pub fn neighbors(&self, triangle: u32) -> Result<&[u32], WalkableQueryError3d> {
        let index = usize_index(triangle);
        if index >= self.triangles.len() {
            return Err(WalkableQueryError3d::MissingTriangle { triangle });
        }
        let first = usize_index(self.neighbor_offsets[index]);
        let last = usize_index(self.neighbor_offsets[index + 1]);
        Ok(&self.neighbors[first..last])
    }

    /// Finds the nearest point on walkable geometry inside a finite radius.
    ///
    /// # Errors
    ///
    /// Returns [`WalkableQueryError3d`] for invalid input, unrepresentable grid
    /// range or a query work-limit violation.
    pub fn nearest_walkable_point(
        &self,
        point: Vec3,
        maximum_distance: f32,
        limits: NearestWalkableQueryLimits3d,
    ) -> Result<NearestWalkablePointResult3d, WalkableQueryError3d> {
        validate_nearest_query(point, maximum_distance, limits)?;
        let mut stats = NearestWalkablePointStats3d::default();
        let cells = query_cell_bounds(point, maximum_distance, self.spatial_cell_size)?;
        let cell_count =
            cell_range_count(cells).ok_or(WalkableQueryError3d::NearestCellLimitExceeded {
                maximum: limits.maximum_cells,
            })?;
        if cell_count > limits.maximum_cells {
            return Err(WalkableQueryError3d::NearestCellLimitExceeded {
                maximum: limits.maximum_cells,
            });
        }
        let mut seen = HashSet::new();
        let mut nearest: Option<NearestWalkablePoint3d> = None;
        for x in cells.minimum.x..=cells.maximum.x {
            for z in cells.minimum.z..=cells.maximum.z {
                stats.cells_visited += 1;
                let Some(candidates) = self.spatial.get(&GridCell3d { x, z }) else {
                    continue;
                };
                for triangle in candidates {
                    if seen.contains(triangle) {
                        continue;
                    }
                    if seen.len() == limits.maximum_candidates {
                        return Err(WalkableQueryError3d::NearestCandidateLimitExceeded {
                            maximum: limits.maximum_candidates,
                        });
                    }
                    seen.insert(*triangle);
                    stats.candidates_tested += 1;
                    let walkable = self.triangles[usize_index(*triangle)];
                    let projected = closest_point_on_triangle(point, walkable.vertices);
                    let distance_squared = (projected - point).length_squared();
                    if distance_squared > maximum_distance * maximum_distance {
                        continue;
                    }
                    let candidate = NearestWalkablePoint3d {
                        walkable_triangle: *triangle,
                        source_triangle: walkable.source_triangle,
                        point: projected,
                        distance_squared,
                    };
                    if nearest.as_ref().is_none_or(|current| {
                        distance_squared < current.distance_squared
                            || (distance_squared.total_cmp(&current.distance_squared)
                                == std::cmp::Ordering::Equal
                                && candidate.walkable_triangle < current.walkable_triangle)
                    }) {
                        nearest = Some(candidate);
                    }
                }
            }
        }
        Ok(NearestWalkablePointResult3d {
            point: nearest,
            stats,
        })
    }

    /// Tests graph reachability between two walkable triangle indices.
    ///
    /// # Errors
    ///
    /// Returns [`WalkableQueryError3d`] for invalid indices or if traversal
    /// would exceed `maximum_visited_triangles`.
    pub fn reachability(
        &self,
        start: u32,
        goal: u32,
        maximum_visited_triangles: usize,
    ) -> Result<WalkableReachabilityResult3d, WalkableQueryError3d> {
        if maximum_visited_triangles == 0 {
            return Err(WalkableQueryError3d::ZeroQueryLimit {
                field: "maximum_visited_triangles",
            });
        }
        self.validate_triangle(start)?;
        self.validate_triangle(goal)?;
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        visited.insert(start);
        queue.push_back(start);
        let mut stats = WalkableGraphQueryStats3d::default();
        while let Some(triangle) = queue.pop_front() {
            stats.visited_triangles += 1;
            if triangle == goal {
                return Ok(WalkableReachabilityResult3d {
                    reachable: true,
                    stats,
                });
            }
            for neighbor in self.neighbors(triangle)? {
                stats.edges_tested += 1;
                if visited.contains(neighbor) {
                    continue;
                }
                if visited.len() == maximum_visited_triangles {
                    return Err(WalkableQueryError3d::VisitedTriangleLimitExceeded {
                        maximum: maximum_visited_triangles,
                    });
                }
                visited.insert(*neighbor);
                queue.push_back(*neighbor);
            }
        }
        Ok(WalkableReachabilityResult3d {
            reachable: false,
            stats,
        })
    }

    /// Projects endpoints and searches a deterministic unsmoothed triangle path.
    ///
    /// Returned points contain the projected start/end and intermediate
    /// triangle centres. They are a coarse corridor/debug path, not a
    /// radius-aware or funnel-smoothed movement trajectory.
    ///
    /// # Errors
    ///
    /// Returns [`WalkableQueryError3d`] when input or an explicit nearest/BFS
    /// work limit is invalid or exceeded.
    pub fn find_path(
        &self,
        start: Vec3,
        goal: Vec3,
        maximum_projection_distance: f32,
        limits: WalkablePathQueryLimits3d,
    ) -> Result<WalkablePathQueryResult3d, WalkableQueryError3d> {
        validate_path_limits(limits)?;
        let start_result =
            self.nearest_walkable_point(start, maximum_projection_distance, limits.nearest)?;
        let Some(projected_start) = start_result.point else {
            return Ok(WalkablePathQueryResult3d {
                outcome: WalkablePathOutcome3d::StartNotFound,
                stats: WalkablePathQueryStats3d {
                    start_nearest: start_result.stats,
                    ..WalkablePathQueryStats3d::default()
                },
            });
        };
        let goal_result =
            self.nearest_walkable_point(goal, maximum_projection_distance, limits.nearest)?;
        let Some(projected_goal) = goal_result.point else {
            return Ok(WalkablePathQueryResult3d {
                outcome: WalkablePathOutcome3d::GoalNotFound,
                stats: WalkablePathQueryStats3d {
                    start_nearest: start_result.stats,
                    goal_nearest: goal_result.stats,
                    ..WalkablePathQueryStats3d::default()
                },
            });
        };
        let (path, graph) = self.triangle_path(
            projected_start.walkable_triangle,
            projected_goal.walkable_triangle,
            limits,
        )?;
        let Some(triangles) = path else {
            return Ok(WalkablePathQueryResult3d {
                outcome: WalkablePathOutcome3d::Unreachable,
                stats: WalkablePathQueryStats3d {
                    start_nearest: start_result.stats,
                    goal_nearest: goal_result.stats,
                    graph,
                },
            });
        };
        let mut points = Vec::with_capacity(triangles.len().saturating_add(1));
        points.push(projected_start.point);
        for triangle in triangles
            .iter()
            .copied()
            .skip(1)
            .take(triangles.len().saturating_sub(2))
        {
            points.push(self.triangles[usize_index(triangle)].centre());
        }
        points.push(projected_goal.point);
        Ok(WalkablePathQueryResult3d {
            outcome: WalkablePathOutcome3d::Path(WalkablePath3d { triangles, points }),
            stats: WalkablePathQueryStats3d {
                start_nearest: start_result.stats,
                goal_nearest: goal_result.stats,
                graph,
            },
        })
    }

    fn validate_triangle(&self, triangle: u32) -> Result<(), WalkableQueryError3d> {
        if usize_index(triangle) >= self.triangles.len() {
            Err(WalkableQueryError3d::MissingTriangle { triangle })
        } else {
            Ok(())
        }
    }

    fn triangle_path(
        &self,
        start: u32,
        goal: u32,
        limits: WalkablePathQueryLimits3d,
    ) -> Result<(Option<Vec<u32>>, WalkableGraphQueryStats3d), WalkableQueryError3d> {
        let mut predecessor = HashMap::new();
        let mut queue = VecDeque::new();
        predecessor.insert(start, start);
        queue.push_back(start);
        let mut stats = WalkableGraphQueryStats3d::default();
        while let Some(triangle) = queue.pop_front() {
            stats.visited_triangles += 1;
            if triangle == goal {
                let mut path = Vec::new();
                let mut current = goal;
                loop {
                    if path.len() == limits.maximum_path_triangles {
                        return Err(WalkableQueryError3d::PathTriangleLimitExceeded {
                            maximum: limits.maximum_path_triangles,
                        });
                    }
                    path.push(current);
                    if current == start {
                        break;
                    }
                    current = predecessor[&current];
                }
                path.reverse();
                return Ok((Some(path), stats));
            }
            for neighbor in self.neighbors(triangle)? {
                stats.edges_tested += 1;
                if predecessor.contains_key(neighbor) {
                    continue;
                }
                if predecessor.len() == limits.maximum_visited_triangles {
                    return Err(WalkableQueryError3d::VisitedTriangleLimitExceeded {
                        maximum: limits.maximum_visited_triangles,
                    });
                }
                predecessor.insert(*neighbor, triangle);
                queue.push_back(*neighbor);
            }
        }
        Ok((None, stats))
    }
}

/// Limits for one nearest walkable-point query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NearestWalkableQueryLimits3d {
    /// Maximum spatial cells inspected.
    pub maximum_cells: usize,
    /// Maximum unique walkable triangles tested exactly.
    pub maximum_candidates: usize,
}

impl NearestWalkableQueryLimits3d {
    /// Creates explicit nearest-point work limits.
    #[must_use]
    pub const fn new(maximum_cells: usize, maximum_candidates: usize) -> Self {
        Self {
            maximum_cells,
            maximum_candidates,
        }
    }
}

impl Default for NearestWalkableQueryLimits3d {
    fn default() -> Self {
        Self::new(4_096, 16_384)
    }
}

/// Combined nearest-point and graph limits for one path query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WalkablePathQueryLimits3d {
    /// Bounds for each endpoint projection.
    pub nearest: NearestWalkableQueryLimits3d,
    /// Maximum unique triangles discovered by BFS.
    pub maximum_visited_triangles: usize,
    /// Maximum triangles returned in the reconstructed path.
    pub maximum_path_triangles: usize,
}

impl Default for WalkablePathQueryLimits3d {
    fn default() -> Self {
        Self {
            nearest: NearestWalkableQueryLimits3d::default(),
            maximum_visited_triangles: 65_536,
            maximum_path_triangles: 4_096,
        }
    }
}

/// Nearest-point query telemetry.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NearestWalkablePointStats3d {
    /// Grid cells visited.
    pub cells_visited: usize,
    /// Unique triangle candidates tested exactly.
    pub candidates_tested: usize,
}

/// The nearest projected walkable point and its stable source identity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NearestWalkablePoint3d {
    /// Index in [`WalkableSurface3d::triangles`].
    pub walkable_triangle: u32,
    /// Original [`TriangleMesh3d`] triangle ordinal.
    pub source_triangle: usize,
    /// Closest point on the triangle.
    pub point: Vec3,
    /// Squared distance from the query point.
    pub distance_squared: f32,
}

/// Optional nearest point paired with bounded-work telemetry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NearestWalkablePointResult3d {
    /// Nearest point inside the requested radius, if one exists.
    pub point: Option<NearestWalkablePoint3d>,
    /// Work performed by the query.
    pub stats: NearestWalkablePointStats3d,
}

/// BFS work counters shared by reachability and path queries.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WalkableGraphQueryStats3d {
    /// Triangles removed from the deterministic BFS queue.
    pub visited_triangles: usize,
    /// Directed neighbor entries inspected.
    pub edges_tested: usize,
}

/// Reachability result with explicit traversal telemetry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WalkableReachabilityResult3d {
    /// Whether `goal` was reached.
    pub reachable: bool,
    /// Work performed before success or exhaustion.
    pub stats: WalkableGraphQueryStats3d,
}

/// One deterministic unsmoothed path through walkable triangles.
#[derive(Clone, Debug, PartialEq)]
pub struct WalkablePath3d {
    triangles: Vec<u32>,
    points: Vec<Vec3>,
}

impl WalkablePath3d {
    /// Returns the triangle corridor from start to goal.
    #[must_use]
    pub fn triangles(&self) -> &[u32] {
        &self.triangles
    }

    /// Returns projected endpoints and intermediate triangle centres.
    #[must_use]
    pub fn points(&self) -> &[Vec3] {
        &self.points
    }
}

/// Normal no-path outcomes are values rather than configuration failures.
#[derive(Clone, Debug, PartialEq)]
pub enum WalkablePathOutcome3d {
    /// Start could not be projected within the requested radius.
    StartNotFound,
    /// Goal could not be projected within the requested radius.
    GoalNotFound,
    /// Both endpoints projected, but their graph components are disconnected.
    Unreachable,
    /// Deterministic triangle corridor and coarse points.
    Path(WalkablePath3d),
}

/// Aggregated telemetry for endpoint projection and graph search.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WalkablePathQueryStats3d {
    /// Start projection work.
    pub start_nearest: NearestWalkablePointStats3d,
    /// Goal projection work.
    pub goal_nearest: NearestWalkablePointStats3d,
    /// BFS work.
    pub graph: WalkableGraphQueryStats3d,
}

/// Path outcome and all bounded-work counters.
#[derive(Clone, Debug, PartialEq)]
pub struct WalkablePathQueryResult3d {
    /// Path or normal no-path reason.
    pub outcome: WalkablePathOutcome3d,
    /// Work performed by all phases.
    pub stats: WalkablePathQueryStats3d,
}

/// Invalid input or an explicit query limit violation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WalkableQueryError3d {
    /// Point or distance is NaN/infinite, or distance is negative.
    InvalidNearestInput,
    /// One query limit is zero.
    ZeroQueryLimit {
        /// Stable limit field name.
        field: &'static str,
    },
    /// Query range cannot be represented by the bounded i32 grid.
    QueryOutsideGrid,
    /// Nearest-point cell range exceeds its explicit bound.
    NearestCellLimitExceeded {
        /// Configured maximum.
        maximum: usize,
    },
    /// Unique nearest candidates exceed their explicit bound.
    NearestCandidateLimitExceeded {
        /// Configured maximum.
        maximum: usize,
    },
    /// A walkable triangle index is invalid.
    MissingTriangle {
        /// Invalid index.
        triangle: u32,
    },
    /// BFS would discover more triangles than permitted.
    VisitedTriangleLimitExceeded {
        /// Configured maximum.
        maximum: usize,
    },
    /// Reconstructed output would exceed the path bound.
    PathTriangleLimitExceeded {
        /// Configured maximum.
        maximum: usize,
    },
}

impl fmt::Display for WalkableQueryError3d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidNearestInput => formatter.write_str(
                "walkable nearest point and maximum distance must be finite; distance cannot be negative",
            ),
            Self::ZeroQueryLimit { field } => write!(formatter, "walkable query limit {field} must be positive"),
            Self::QueryOutsideGrid => formatter.write_str("walkable query lies outside the grid coordinate range"),
            Self::NearestCellLimitExceeded { maximum } => write!(formatter, "walkable nearest query exceeds cell maximum {maximum}"),
            Self::NearestCandidateLimitExceeded { maximum } => write!(formatter, "walkable nearest query exceeds candidate maximum {maximum}"),
            Self::MissingTriangle { triangle } => write!(formatter, "walkable triangle {triangle} does not exist"),
            Self::VisitedTriangleLimitExceeded { maximum } => write!(formatter, "walkable graph query exceeds visited-triangle maximum {maximum}"),
            Self::PathTriangleLimitExceeded { maximum } => write!(formatter, "walkable path exceeds triangle maximum {maximum}"),
        }
    }
}

impl Error for WalkableQueryError3d {}

fn validate_limits(
    limits: WalkableSurfaceBuildLimits3d,
) -> Result<(), WalkableSurfaceConfigError3d> {
    for (field, value) in [
        ("maximum_source_triangles", limits.maximum_source_triangles),
        (
            "maximum_walkable_triangles",
            limits.maximum_walkable_triangles,
        ),
        ("maximum_adjacencies", limits.maximum_adjacencies),
        (
            "maximum_neighbors_per_triangle",
            limits.maximum_neighbors_per_triangle,
        ),
        (
            "maximum_edge_bucket_entries",
            limits.maximum_edge_bucket_entries,
        ),
        (
            "maximum_edge_candidate_tests",
            limits.maximum_edge_candidate_tests,
        ),
        (
            "maximum_spatial_references",
            limits.maximum_spatial_references,
        ),
        (
            "maximum_cells_per_triangle",
            limits.maximum_cells_per_triangle,
        ),
    ] {
        if value == 0 {
            return Err(WalkableSurfaceConfigError3d::ZeroLimit { field });
        }
    }
    let maximum_walkable = usize::try_from(u32::MAX / 3).unwrap_or(usize::MAX);
    if limits.maximum_walkable_triangles > maximum_walkable {
        return Err(WalkableSurfaceConfigError3d::LimitTooLarge {
            field: "maximum_walkable_triangles",
            actual: limits.maximum_walkable_triangles,
            maximum: maximum_walkable,
        });
    }
    let maximum_adjacencies = usize::try_from(u32::MAX / 2).unwrap_or(usize::MAX);
    if limits.maximum_adjacencies > maximum_adjacencies {
        return Err(WalkableSurfaceConfigError3d::LimitTooLarge {
            field: "maximum_adjacencies",
            actual: limits.maximum_adjacencies,
            maximum: maximum_adjacencies,
        });
    }
    Ok(())
}

fn build_adjacency(
    triangles: &[WalkableTriangle3d],
    config: WalkableSurfaceConfig3d,
    stats: &mut WalkableSurfaceBuildStats3d,
) -> Result<Vec<Vec<u32>>, WalkableSurfaceBuildError3d> {
    let mut adjacency = vec![Vec::new(); triangles.len()];
    let mut buckets: HashMap<GridCell3d, Vec<u32>> = HashMap::new();
    for (triangle_index, triangle) in triangles.iter().copied().enumerate() {
        let triangle_index = compact_index(triangle_index);
        for edge_index in 0_u32..3 {
            let (first, second) = triangle_edge(triangle, edge_index);
            let midpoint = (first + second) * 0.5;
            let cell = grid_cell(midpoint, config.edge_tolerance).map_err(|()| {
                WalkableSurfaceBuildError3d::CoordinateOutsideGrid {
                    source_triangle: triangle.source_triangle,
                }
            })?;
            for x_offset in -1..=1 {
                for z_offset in -1..=1 {
                    let Some(candidate_cell) = offset_cell(cell, x_offset, z_offset) else {
                        continue;
                    };
                    let Some(candidates) = buckets.get(&candidate_cell) else {
                        continue;
                    };
                    for candidate in candidates {
                        let candidate_triangle = *candidate / 3;
                        if candidate_triangle == triangle_index {
                            continue;
                        }
                        if stats.edge_candidates_tested
                            == config.limits.maximum_edge_candidate_tests
                        {
                            return Err(WalkableSurfaceBuildError3d::EdgeCandidateLimitExceeded {
                                maximum: config.limits.maximum_edge_candidate_tests,
                            });
                        }
                        stats.edge_candidates_tested += 1;
                        let candidate_edge = *candidate % 3;
                        let (other_first, other_second) = triangle_edge(
                            triangles[usize_index(candidate_triangle)],
                            candidate_edge,
                        );
                        if edges_are_adjacent(
                            first,
                            second,
                            other_first,
                            other_second,
                            config.edge_tolerance,
                            config.maximum_step_height,
                        ) {
                            add_adjacency(
                                &mut adjacency,
                                triangle_index,
                                candidate_triangle,
                                config.limits,
                                stats,
                            )?;
                        }
                    }
                }
            }
            let bucket = buckets.entry(cell).or_default();
            if bucket.len() == config.limits.maximum_edge_bucket_entries {
                return Err(WalkableSurfaceBuildError3d::EdgeBucketLimitExceeded {
                    maximum: config.limits.maximum_edge_bucket_entries,
                });
            }
            bucket.push(triangle_index * 3 + edge_index);
        }
    }
    for neighbors in &mut adjacency {
        neighbors.sort_unstable();
    }
    Ok(adjacency)
}

fn add_adjacency(
    adjacency: &mut [Vec<u32>],
    left: u32,
    right: u32,
    limits: WalkableSurfaceBuildLimits3d,
    stats: &mut WalkableSurfaceBuildStats3d,
) -> Result<(), WalkableSurfaceBuildError3d> {
    if adjacency[usize_index(left)].contains(&right) {
        return Ok(());
    }
    if stats.adjacencies == limits.maximum_adjacencies {
        return Err(WalkableSurfaceBuildError3d::AdjacencyLimitExceeded {
            maximum: limits.maximum_adjacencies,
        });
    }
    for triangle in [left, right] {
        if adjacency[usize_index(triangle)].len() == limits.maximum_neighbors_per_triangle {
            return Err(WalkableSurfaceBuildError3d::NeighborLimitExceeded {
                triangle,
                maximum: limits.maximum_neighbors_per_triangle,
            });
        }
    }
    adjacency[usize_index(left)].push(right);
    adjacency[usize_index(right)].push(left);
    stats.adjacencies += 1;
    Ok(())
}

fn compact_adjacency(adjacency: Vec<Vec<u32>>) -> (Vec<u32>, Vec<u32>) {
    let total = adjacency.iter().map(Vec::len).sum();
    let mut offsets = Vec::with_capacity(adjacency.len() + 1);
    let mut neighbors = Vec::with_capacity(total);
    offsets.push(0);
    for entries in adjacency {
        neighbors.extend(entries);
        offsets.push(compact_index(neighbors.len()));
    }
    (offsets, neighbors)
}

fn build_spatial_grid(
    triangles: &[WalkableTriangle3d],
    config: WalkableSurfaceConfig3d,
    stats: &mut WalkableSurfaceBuildStats3d,
) -> Result<HashMap<GridCell3d, Vec<u32>>, WalkableSurfaceBuildError3d> {
    let mut spatial: HashMap<GridCell3d, Vec<u32>> = HashMap::new();
    for (triangle_index, triangle) in triangles.iter().copied().enumerate() {
        let minimum = Vec3::new(
            triangle.vertices[0]
                .x
                .min(triangle.vertices[1].x)
                .min(triangle.vertices[2].x),
            0.0,
            triangle.vertices[0]
                .z
                .min(triangle.vertices[1].z)
                .min(triangle.vertices[2].z),
        );
        let maximum = Vec3::new(
            triangle.vertices[0]
                .x
                .max(triangle.vertices[1].x)
                .max(triangle.vertices[2].x),
            0.0,
            triangle.vertices[0]
                .z
                .max(triangle.vertices[1].z)
                .max(triangle.vertices[2].z),
        );
        let minimum = grid_cell(minimum, config.spatial_cell_size).map_err(|()| {
            WalkableSurfaceBuildError3d::CoordinateOutsideGrid {
                source_triangle: triangle.source_triangle,
            }
        })?;
        let maximum = grid_cell(maximum, config.spatial_cell_size).map_err(|()| {
            WalkableSurfaceBuildError3d::CoordinateOutsideGrid {
                source_triangle: triangle.source_triangle,
            }
        })?;
        let cells = cell_range_count(CellBounds { minimum, maximum }).ok_or(
            WalkableSurfaceBuildError3d::TriangleCellLimitExceeded {
                source_triangle: triangle.source_triangle,
                maximum: config.limits.maximum_cells_per_triangle,
                actual: usize::MAX,
            },
        )?;
        if cells > config.limits.maximum_cells_per_triangle {
            return Err(WalkableSurfaceBuildError3d::TriangleCellLimitExceeded {
                source_triangle: triangle.source_triangle,
                maximum: config.limits.maximum_cells_per_triangle,
                actual: cells,
            });
        }
        if stats.spatial_references.saturating_add(cells) > config.limits.maximum_spatial_references
        {
            return Err(WalkableSurfaceBuildError3d::SpatialReferenceLimitExceeded {
                maximum: config.limits.maximum_spatial_references,
            });
        }
        let triangle_index = compact_index(triangle_index);
        for x in minimum.x..=maximum.x {
            for z in minimum.z..=maximum.z {
                spatial
                    .entry(GridCell3d { x, z })
                    .or_default()
                    .push(triangle_index);
            }
        }
        stats.spatial_references += cells;
    }
    stats.spatial_cells = spatial.len();
    Ok(spatial)
}

#[derive(Clone, Copy)]
struct CellBounds {
    minimum: GridCell3d,
    maximum: GridCell3d,
}

fn query_cell_bounds(
    point: Vec3,
    radius: f32,
    cell_size: f32,
) -> Result<CellBounds, WalkableQueryError3d> {
    let minimum = grid_cell(
        Vec3::new(point.x - radius, 0.0, point.z - radius),
        cell_size,
    )
    .map_err(|()| WalkableQueryError3d::QueryOutsideGrid)?;
    let maximum = grid_cell(
        Vec3::new(point.x + radius, 0.0, point.z + radius),
        cell_size,
    )
    .map_err(|()| WalkableQueryError3d::QueryOutsideGrid)?;
    Ok(CellBounds { minimum, maximum })
}

fn cell_range_count(bounds: CellBounds) -> Option<usize> {
    let width = i64::from(bounds.maximum.x) - i64::from(bounds.minimum.x) + 1;
    let depth = i64::from(bounds.maximum.z) - i64::from(bounds.minimum.z) + 1;
    usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(depth).ok()?)
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "the floored f64 coordinate is range-checked against i32 immediately before conversion"
)]
fn grid_cell(point: Vec3, cell_size: f32) -> Result<GridCell3d, ()> {
    let coordinate = |value: f32| {
        let value = f64::from(value) / f64::from(cell_size);
        let value = value.floor();
        if value < f64::from(i32::MIN) || value > f64::from(i32::MAX) {
            Err(())
        } else {
            Ok(value as i32)
        }
    };
    Ok(GridCell3d {
        x: coordinate(point.x)?,
        z: coordinate(point.z)?,
    })
}

fn offset_cell(cell: GridCell3d, x: i32, z: i32) -> Option<GridCell3d> {
    Some(GridCell3d {
        x: cell.x.checked_add(x)?,
        z: cell.z.checked_add(z)?,
    })
}

fn triangle_edge(triangle: WalkableTriangle3d, edge: u32) -> (Vec3, Vec3) {
    match edge {
        0 => (triangle.vertices[0], triangle.vertices[1]),
        1 => (triangle.vertices[1], triangle.vertices[2]),
        _ => (triangle.vertices[2], triangle.vertices[0]),
    }
}

fn edges_are_adjacent(
    first: Vec3,
    second: Vec3,
    other_first: Vec3,
    other_second: Vec3,
    edge_tolerance: f32,
    maximum_step_height: f32,
) -> bool {
    endpoints_match(first, other_first, edge_tolerance, maximum_step_height)
        && endpoints_match(second, other_second, edge_tolerance, maximum_step_height)
        || endpoints_match(first, other_second, edge_tolerance, maximum_step_height)
            && endpoints_match(second, other_first, edge_tolerance, maximum_step_height)
}

fn endpoints_match(first: Vec3, second: Vec3, xz_tolerance: f32, y_tolerance: f32) -> bool {
    let dx = first.x - second.x;
    let dz = first.z - second.z;
    dx.mul_add(dx, dz * dz) <= xz_tolerance * xz_tolerance
        && (first.y - second.y).abs() <= y_tolerance
}

fn validate_nearest_query(
    point: Vec3,
    maximum_distance: f32,
    limits: NearestWalkableQueryLimits3d,
) -> Result<(), WalkableQueryError3d> {
    if !finite(point) || !maximum_distance.is_finite() || maximum_distance < 0.0 {
        return Err(WalkableQueryError3d::InvalidNearestInput);
    }
    for (field, value) in [
        ("maximum_cells", limits.maximum_cells),
        ("maximum_candidates", limits.maximum_candidates),
    ] {
        if value == 0 {
            return Err(WalkableQueryError3d::ZeroQueryLimit { field });
        }
    }
    Ok(())
}

fn validate_path_limits(limits: WalkablePathQueryLimits3d) -> Result<(), WalkableQueryError3d> {
    validate_nearest_query(Vec3::ZERO, 0.0, limits.nearest)?;
    for (field, value) in [
        (
            "maximum_visited_triangles",
            limits.maximum_visited_triangles,
        ),
        ("maximum_path_triangles", limits.maximum_path_triangles),
    ] {
        if value == 0 {
            return Err(WalkableQueryError3d::ZeroQueryLimit { field });
        }
    }
    Ok(())
}

fn finite(value: Vec3) -> bool {
    value.x.is_finite() && value.y.is_finite() && value.z.is_finite()
}

fn cross(left: Vec3, right: Vec3) -> Vec3 {
    Vec3::new(
        left.y * right.z - left.z * right.y,
        left.z * right.x - left.x * right.z,
        left.x * right.y - left.y * right.x,
    )
}

fn dot(left: Vec3, right: Vec3) -> f32 {
    left.x
        .mul_add(right.x, left.y.mul_add(right.y, left.z * right.z))
}

fn normalized(value: Vec3) -> Vec3 {
    let length_squared = value.length_squared();
    value * length_squared.sqrt().recip()
}

fn compact_index(index: usize) -> u32 {
    u32::try_from(index).expect("walkable triangle limits fit compact u32 indices")
}

fn usize_index(index: u32) -> usize {
    usize::try_from(index).expect("u32 fits every supported Rust target")
}

fn closest_point_on_triangle(point: Vec3, triangle: [Vec3; 3]) -> Vec3 {
    let [a, b, c] = triangle;
    let ab = b - a;
    let ac = c - a;
    let ap = point - a;
    let d1 = dot(ab, ap);
    let d2 = dot(ac, ap);
    if d1 <= 0.0 && d2 <= 0.0 {
        return a;
    }
    let bp = point - b;
    let d3 = dot(ab, bp);
    let d4 = dot(ac, bp);
    if d3 >= 0.0 && d4 <= d3 {
        return b;
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return a + ab * (d1 / (d1 - d3));
    }
    let cp = point - c;
    let d5 = dot(ab, cp);
    let d6 = dot(ac, cp);
    if d6 >= 0.0 && d5 <= d6 {
        return c;
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return a + ac * (d2 / (d2 - d6));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && d4 - d3 >= 0.0 && d5 - d6 >= 0.0 {
        let edge = c - b;
        return b + edge * ((d4 - d3) / ((d4 - d3) + (d5 - d6)));
    }
    let denominator = (va + vb + vc).recip();
    a + ab * (vb * denominator) + ac * (vc * denominator)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mesh(vertices: &[Vec3], indices: &[u32]) -> TriangleMesh3d {
        TriangleMesh3d::from_indexed(vertices, indices).expect("valid test triangle mesh")
    }

    fn config() -> WalkableSurfaceConfig3d {
        WalkableSurfaceConfig3d::new(45.0_f32.to_radians(), 0.25, 0.01, 1.0)
            .expect("valid test navigation config")
    }

    fn assert_vec3_close(actual: Vec3, expected: Vec3) {
        assert!((actual.x - expected.x).abs() <= 1.0e-6);
        assert!((actual.y - expected.y).abs() <= 1.0e-6);
        assert!((actual.z - expected.z).abs() <= 1.0e-6);
    }

    #[test]
    fn extraction_keeps_only_upward_triangles_within_slope() {
        let source = mesh(
            &[
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(0.0, 0.0, 1.0),
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::new(2.0, 0.0, 0.0),
                Vec3::new(3.0, 0.0, 0.0),
                Vec3::new(2.0, 1.0, 0.0),
            ],
            &[0, 1, 2, 3, 4, 5],
        );
        let surface = WalkableSurface3d::from_triangle_mesh(&source, config()).expect("bounded");
        assert_eq!(surface.triangles().len(), 1);
        assert_eq!(surface.triangles()[0].source_triangle(), 0);
        assert_eq!(surface.build_stats().rejected_by_slope, 1);
        assert!(surface.triangles()[0].normal().y > 0.99);
    }

    #[test]
    fn adjacency_accepts_small_step_and_rejects_large_step() {
        let vertices = [
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(0.0, 0.0, 1.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.0, 0.2, 1.0),
            Vec3::new(1.0, 0.2, 1.0),
            Vec3::new(1.0, 0.2, 0.0),
            Vec3::new(0.0, 1.0, 1.0),
            Vec3::new(1.0, 1.0, 1.0),
            Vec3::new(1.0, 1.0, 0.0),
        ];
        let source = mesh(&vertices, &[0, 1, 2, 3, 4, 5, 6, 7, 8]);
        let surface = WalkableSurface3d::from_triangle_mesh(&source, config()).expect("bounded");
        assert_eq!(surface.neighbors(0).expect("triangle 0"), &[1]);
        assert_eq!(surface.neighbors(1).expect("triangle 1"), &[0]);
        assert!(surface.neighbors(2).expect("triangle 2").is_empty());
        assert_eq!(surface.build_stats().adjacencies, 1);
    }

    #[test]
    fn nearest_point_uses_grid_and_stable_source_tie() {
        let source = mesh(
            &[
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(0.0, 0.0, 1.0),
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::new(10.0, 0.0, 0.0),
                Vec3::new(10.0, 0.0, 1.0),
                Vec3::new(11.0, 0.0, 0.0),
            ],
            &[0, 1, 2, 3, 4, 5],
        );
        let surface = WalkableSurface3d::from_triangle_mesh(&source, config()).expect("bounded");
        let nearest = surface
            .nearest_walkable_point(
                Vec3::new(0.2, 2.0, 0.2),
                3.0,
                NearestWalkableQueryLimits3d::new(64, 4),
            )
            .expect("bounded query");
        let point = nearest.point.expect("near first floor");
        assert_eq!(point.walkable_triangle, 0);
        assert_eq!(point.source_triangle, 0);
        assert_vec3_close(point.point, Vec3::new(0.2, 0.0, 0.2));
        assert_eq!(nearest.stats.candidates_tested, 1);
    }

    #[test]
    fn path_and_reachability_are_deterministic_and_report_limits() {
        let source = mesh(
            &[
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(0.0, 0.0, 1.0),
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::new(1.0, 0.0, 1.0),
                Vec3::new(2.0, 0.0, 0.0),
                Vec3::new(2.0, 0.0, 1.0),
            ],
            &[0, 1, 2, 2, 1, 3, 2, 3, 4, 4, 3, 5],
        );
        let surface = WalkableSurface3d::from_triangle_mesh(&source, config()).expect("bounded");
        assert!(
            surface
                .reachability(0, 3, 4)
                .expect("bounded BFS")
                .reachable
        );
        assert!(matches!(
            surface.reachability(0, 3, 2),
            Err(WalkableQueryError3d::VisitedTriangleLimitExceeded { maximum: 2 })
        ));

        let result = surface
            .find_path(
                Vec3::new(0.1, 0.2, 0.1),
                Vec3::new(1.9, 0.2, 0.9),
                1.0,
                WalkablePathQueryLimits3d {
                    nearest: NearestWalkableQueryLimits3d::new(16, 8),
                    maximum_visited_triangles: 8,
                    maximum_path_triangles: 8,
                },
            )
            .expect("bounded path");
        let WalkablePathOutcome3d::Path(path) = result.outcome else {
            panic!("connected floor should produce a path")
        };
        assert_eq!(path.triangles(), &[0, 1, 2, 3]);
        assert_vec3_close(path.points()[0], Vec3::new(0.1, 0.0, 0.1));
        assert_vec3_close(
            *path.points().last().expect("path has projected goal"),
            Vec3::new(1.9, 0.0, 0.9),
        );
        assert!(result.stats.graph.visited_triangles <= 4);
    }

    #[test]
    fn disconnected_components_return_normal_unreachable_outcome() {
        let (vertices, indices) = {
            let vertices = [
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(0.0, 0.0, 1.0),
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::new(10.0, 0.0, 0.0),
                Vec3::new(10.0, 0.0, 1.0),
                Vec3::new(11.0, 0.0, 0.0),
            ];
            (vertices, [0, 1, 2, 3, 4, 5])
        };
        let source = mesh(&vertices, &indices);
        let surface = WalkableSurface3d::from_triangle_mesh(&source, config()).expect("bounded");
        let result = surface
            .find_path(
                Vec3::new(0.1, 0.1, 0.1),
                Vec3::new(10.1, 0.1, 0.1),
                1.0,
                WalkablePathQueryLimits3d::default(),
            )
            .expect("normal no-path result");
        assert_eq!(result.outcome, WalkablePathOutcome3d::Unreachable);
    }

    #[test]
    fn build_and_query_limits_fail_explicitly() {
        let source = mesh(
            &[
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(0.0, 0.0, 1.0),
                Vec3::new(1.0, 0.0, 0.0),
            ],
            &[0, 1, 2],
        );
        let mut limits = WalkableSurfaceBuildLimits3d::default();
        limits.maximum_source_triangles = 0;
        assert!(matches!(
            WalkableSurface3d::from_triangle_mesh(&source, config().with_limits(limits)),
            Err(WalkableSurfaceBuildError3d::Config(
                WalkableSurfaceConfigError3d::ZeroLimit {
                    field: "maximum_source_triangles"
                }
            ))
        ));

        let surface = WalkableSurface3d::from_triangle_mesh(&source, config()).expect("bounded");
        assert!(matches!(
            surface.nearest_walkable_point(
                Vec3::ZERO,
                10.0,
                NearestWalkableQueryLimits3d::new(1, 1)
            ),
            Err(WalkableQueryError3d::NearestCellLimitExceeded { maximum: 1 })
        ));
    }
}
