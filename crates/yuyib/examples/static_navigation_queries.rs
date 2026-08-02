//! Headless high-level navigation over a static ECS scene collider.
//!
//! Run from the workspace root:
//!
//! ```text
//! cargo run -p yuyib --example static_navigation_queries
//! ```

use std::{error::Error, f32::consts::FRAC_PI_3};

use yuyib::{
    assets::Assets,
    ecs::prelude::World,
    game_3d::{
        Model3d, NearestWalkableQueryLimits3d, Transform3d, WalkablePathOutcome3d,
        WalkablePathQueryLimits3d, WalkableSurface3d, WalkableSurfaceConfig3d,
        build_static_scene_collider_3d,
    },
    model::Model,
    physics::Vec3,
};

fn main() -> Result<(), Box<dyn Error>> {
    let mut models = Assets::new();
    let platform = models.insert(Model::cube(0.5)?);
    let mut world = World::new();

    // The first two platforms share an edge. The third is a disconnected island.
    for x in [0.0, 1.0, 4.0] {
        world.spawn((
            Model3d::new(platform),
            Transform3d::from_translation([x, 0.0, 0.0]),
        ));
    }

    let collider = build_static_scene_collider_3d(&mut world, &models)?;
    let config = WalkableSurfaceConfig3d::new(FRAC_PI_3, 0.3, 0.001, 1.0)?;

    // High-level path for an imported/static ECS scene.
    let surface = collider.build_walkable_surface(config)?;
    // Equivalent low-level path for procedural/importer-owned triangle geometry.
    let direct_surface = WalkableSurface3d::from_triangle_mesh(collider.mesh(), config)?;
    assert_eq!(surface.build_stats(), direct_surface.build_stats());

    let build = surface.build_stats();
    println!(
        "build: source={} walkable={} rejected_slope={} adjacencies={} cells={} references={}",
        build.source_triangles,
        build.walkable_triangles,
        build.rejected_by_slope,
        build.adjacencies,
        build.spatial_cells,
        build.spatial_references,
    );

    let nearest_limits = NearestWalkableQueryLimits3d::new(64, 64);
    let start_query =
        surface.nearest_walkable_point(Vec3::new(-0.25, 1.0, 0.0), 1.0, nearest_limits)?;
    let start = start_query
        .point
        .ok_or("start did not project onto the platforms")?;
    println!(
        "nearest: triangle={} source={} point={:?}; cells={} candidates={}",
        start.walkable_triangle,
        start.source_triangle,
        start.point,
        start_query.stats.cells_visited,
        start_query.stats.candidates_tested,
    );

    let connected_goal = surface
        .nearest_walkable_point(Vec3::new(1.25, 1.0, 0.0), 1.0, nearest_limits)?
        .point
        .ok_or("connected goal did not project")?;
    let reachable = surface.reachability(
        start.walkable_triangle,
        connected_goal.walkable_triangle,
        64,
    )?;
    println!(
        "reachability: reachable={} visited={} edges={}",
        reachable.reachable, reachable.stats.visited_triangles, reachable.stats.edges_tested,
    );

    let path_limits = WalkablePathQueryLimits3d {
        nearest: nearest_limits,
        maximum_visited_triangles: 64,
        maximum_path_triangles: 16,
    };
    let path = surface.find_path(
        Vec3::new(-0.25, 1.0, 0.0),
        Vec3::new(1.25, 1.0, 0.0),
        1.0,
        path_limits,
    )?;
    let path_stats = path.stats.graph;
    match path.outcome {
        WalkablePathOutcome3d::Path(path) => println!(
            "path: triangles={:?} points={:?}; visited={} edges={}",
            path.triangles(),
            path.points(),
            path_stats.visited_triangles,
            path_stats.edges_tested,
        ),
        outcome => return Err(format!("expected a connected path, got {outcome:?}").into()),
    }

    let disconnected = surface.find_path(
        Vec3::new(-0.25, 1.0, 0.0),
        Vec3::new(4.0, 1.0, 0.0),
        1.0,
        path_limits,
    )?;
    match disconnected.outcome {
        WalkablePathOutcome3d::Unreachable => println!(
            "no path: Unreachable; visited={} edges={}",
            disconnected.stats.graph.visited_triangles, disconnected.stats.graph.edges_tested,
        ),
        outcome => return Err(format!("expected typed Unreachable, got {outcome:?}").into()),
    }

    Ok(())
}
