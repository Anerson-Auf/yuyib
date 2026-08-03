//! Windowed M7 demo: Tiled farm objects + location stack + composer interior.
//!
//! Требует локальный пак `for_tests/2d/` (Tileset / Character / Objects).
//! WASD / стрелки — 4-dir idle/walk. `E` у двери — вход в дом / выход наружу.
//!
//! ```text
//! cargo run -p yuyib --example two_d_tiled_playable --features "app,two-d"
//! ```

use std::{cell::RefCell, error::Error, path::PathBuf, rc::Rc, time::Duration};

use yuyib::{
    animation::{AnimationSet, AnimationStateMachine},
    app::{Application, RenderLoop},
    assets::{Assets, ImportSource, ImporterRegistry},
    game_2d::{
        AnimatedSprite2d, CardinalClipPolicy2d, Game2dSceneConfig, KinematicSpriteController2d,
        Sprite2d, SpriteAnimator2d, SpriteFacing2d, TileCollision2d, TileMap2d, TileMapComposer2d,
        TileStamp2d, apply_cardinal_clips_2d,
    },
    image::{DecodePolicy, DecodedImage, decode_path},
    platform::WindowConfig,
    profile_2d::{
        CameraFollow2d, Game2dProfile, LocationFrame2d, LocationPortal2d, LocationPortalAction2d,
        LocationStack2d, PlayableLoop2d, PlayableLoopDesc2d,
    },
    render::ClearColor,
    render_2d::Camera2d,
    tiled::{
        BoundTiledMap2d, ImportedTiledMap, ImportedTiledObject2d, ImportedTiledObjectLayer2d,
        register_tiled_map_importer,
    },
    two_d::{
        PlaybackMode, SpriteAnimation, SpriteSheet, Texture, TextureHandle, TextureRegion,
        TextureSize,
    },
};

const MAP_JSON: &str = include_str!("fixtures/tiled_farm_spring.json");
const WORLD_TILE: f32 = 32.0;
const PLAYER_DRAW: [f32; 2] = [48.0, 48.0];
/// Half-extents for feet (`KinematicSpriteController2d::new` halves full size).
const PLAYER_HALF_EXTENTS: [f32; 2] = [10.0, 8.0];
/// Close shot: ~2 screen pixels per world unit (character reads clearly).
const CAMERA_PIXELS_PER_UNIT: f32 = 2.0;

const LOCATION_OUTDOOR: &str = "outdoor";
const LOCATION_HOUSE: &str = "house_interior";

/// Spring tileset locals: fixture used autotile fragments (holes / jagged corners).
/// Retarget to fully opaque fills before bind.
const LOCAL_GRASS_FRAGILE: u32 = 21; // GID 22 — 12 transparent corner texels
const LOCAL_GRASS_SOLID: u32 = 33; // GID 34 — flat opaque grass
const LOCAL_WALL_JAGGED: u32 = 165; // GID 166 — orange fill + stair-step green corners
const LOCAL_WALL_SOLID: u32 = 177; // GID 178 — flat opaque dirt border

const INTERIOR_GRID: [u32; 2] = [12, 10];

struct OutdoorCache {
    tile_map: TileMap2d,
    collision: TileCollision2d,
    object_layers: Vec<ImportedTiledObjectLayer2d>,
    tile_pixel_size: [u32; 2],
    world_tile_size: [f32; 2],
    house_region: TextureRegion,
    tree_region: TextureRegion,
}

struct Demo {
    _textures: Assets<Texture>,
    queued: Vec<(TextureHandle, DecodedImage)>,
    profile: Game2dProfile,
    playable: PlayableLoop2d,
    clips: CardinalClipPolicy2d,
    locations: LocationStack2d,
    outdoor: OutdoorCache,
    /// World position restored when exiting the house.
    outdoor_return: [f32; 2],
}

fn main() -> Result<(), yuyib::app::ApplicationError> {
    let mut demo = create_demo().unwrap_or_else(|error| {
        eprintln!("two_d_tiled_playable: {error}");
        std::process::exit(1);
    });
    for (handle, image) in demo.queued.drain(..) {
        demo.profile
            .queue_texture(handle, image)
            .expect("texture queue accepts farm pack");
    }
    let demo = Rc::new(RefCell::new(demo));
    let event_demo = Rc::clone(&demo);
    let update_demo = Rc::clone(&demo);
    let render_demo = Rc::clone(&demo);

    Application::new()
        .window(WindowConfig {
            title: "Yuyib — Tiled farm (WASD, E у двери)".to_owned(),
            width: 960,
            height: 540,
            resizable: true,
            ..Default::default()
        })
        .clear_color(ClearColor::linear(0.08, 0.12, 0.06, 1.0))
        .render_loop(RenderLoop::Continuous)
        .on_window_event(move |event, _context| {
            let mut demo = event_demo.borrow_mut();
            demo.playable.handle_window_event(event);
            if is_interact_press(event) {
                if let Err(error) = try_interact(&mut demo) {
                    eprintln!("location interact: {error}");
                }
            }
        })
        .on_frame(move |context| {
            let mut demo = update_demo.borrow_mut();
            let Demo {
                profile,
                playable,
                clips,
                ..
            } = &mut *demo;
            let actor = playable.actor();
            let axis = playable.movement().axis();
            apply_cardinal_clips_2d(profile.world_mut(), actor, [axis.x, axis.y], clips)
                .expect("actor owns animator");
            playable
                .step(profile, context.frame().delta)
                .expect("player starts in open grass");
        })
        .on_render(move |frame| {
            let mut demo = render_demo.borrow_mut();
            let Demo {
                profile, playable, ..
            } = &mut *demo;
            let _stats = playable
                .render(profile, frame)
                .expect("farm scene remains valid");
        })
        .run()
}

fn is_interact_press(event: &yuyib::platform::winit::event::WindowEvent) -> bool {
    use yuyib::platform::winit::{
        event::{ElementState, WindowEvent},
        keyboard::{KeyCode, PhysicalKey},
    };
    let WindowEvent::KeyboardInput { event, .. } = event else {
        return false;
    };
    matches!(
        (event.physical_key, event.state, event.repeat),
        (PhysicalKey::Code(KeyCode::KeyE), ElementState::Pressed, false)
    )
}

fn asset_root() -> Result<PathBuf, Box<dyn Error>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../for_tests");
    if !root.join("2d/Tileset/Tileset Spring.png").is_file() {
        return Err(format!(
            "missing for_tests/2d pack under {} — add Tileset/Character/Objects PNGs",
            root.display()
        )
        .into());
    }
    Ok(root)
}

fn load_png(
    textures: &mut Assets<Texture>,
    path: &std::path::Path,
    queued: &mut Vec<(TextureHandle, DecodedImage)>,
) -> Result<(TextureHandle, TextureSize), Box<dyn Error>> {
    let image = decode_path(path, DecodePolicy::default())?;
    let size = image.texture().size();
    let handle = textures.insert(image.texture().clone());
    queued.push((handle, image));
    Ok((handle, size))
}

fn sheet_animation(
    sheet: &SpriteSheet,
    row: u32,
    columns: u32,
    frame_count: u32,
    duration_ms: u64,
    playback: PlaybackMode,
) -> Result<SpriteAnimation, Box<dyn Error>> {
    let mut regions = Vec::with_capacity(frame_count as usize);
    for column in 0..frame_count {
        let index = row * columns + column;
        regions.push(
            sheet
                .region(usize::try_from(index).expect("frame index fits usize"))
                .ok_or_else(|| format!("missing sheet region {index}"))?,
        );
    }
    Ok(SpriteAnimation::from_regions(
        &regions,
        Duration::from_millis(duration_ms),
        playback,
    )?)
}

fn mark_solid(solid: &mut [bool], grid_w: u32, column: u32, row: u32) {
    let index = (row * grid_w + column) as usize;
    if let Some(flag) = solid.get_mut(index) {
        *flag = true;
    }
}

fn world_to_cell(center: [f32; 2]) -> [u32; 2] {
    [
        (center[0] / WORLD_TILE).floor().max(0.0) as u32,
        (center[1] / WORLD_TILE).floor().max(0.0) as u32,
    ]
}

fn object_world_rect(
    object: &ImportedTiledObject2d,
    tile_pixel_size: [u32; 2],
    world_tile_size: [f32; 2],
) -> ([f32; 2], [f32; 2]) {
    yuyib::tiled::world_from_tiled_px(
        object.position_px(),
        object.size_px(),
        tile_pixel_size,
        world_tile_size,
    )
}

fn portal_action_from_object(object: &ImportedTiledObject2d) -> LocationPortalAction2d {
    if object
        .property("action")
        .and_then(|value| value.as_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("exit"))
    {
        return LocationPortalAction2d::Exit;
    }
    match object.property("target").and_then(|value| value.as_str()) {
        Some(target) if !target.is_empty() => LocationPortalAction2d::Enter(target.to_owned()),
        _ => LocationPortalAction2d::Exit,
    }
}

fn portals_from_objects(
    layers: &[ImportedTiledObjectLayer2d],
    tile_pixel_size: [u32; 2],
    world_tile_size: [f32; 2],
) -> Vec<LocationPortal2d> {
    let mut portals = Vec::new();
    for layer in layers {
        for object in layer.objects() {
            if object.class() != "portal" {
                continue;
            }
            let (center, size) = object_world_rect(object, tile_pixel_size, world_tile_size);
            portals.push(LocationPortal2d::from_center_size(
                center,
                size,
                portal_action_from_object(object),
            ));
        }
    }
    portals
}

fn find_spawn(
    layers: &[ImportedTiledObjectLayer2d],
    tile_pixel_size: [u32; 2],
    world_tile_size: [f32; 2],
    fallback: [f32; 2],
) -> [f32; 2] {
    for layer in layers {
        for object in layer.objects() {
            if object.class() == "player_spawn" {
                let (center, _) = object_world_rect(object, tile_pixel_size, world_tile_size);
                return center;
            }
        }
    }
    fallback
}

fn spawn_outdoor_location(
    profile: &mut Game2dProfile,
    outdoor: &OutdoorCache,
) -> Result<LocationFrame2d, Box<dyn Error>> {
    let grid = outdoor.tile_map.grid();
    let mut solid = outdoor.collision.solid().to_vec();
    let mut entities = Vec::new();

    for layer in &outdoor.object_layers {
        for object in layer.objects() {
            let (center, _) =
                object_world_rect(object, outdoor.tile_pixel_size, outdoor.world_tile_size);
            match object.class() {
                "prop_house" => {
                    let [column, row] = world_to_cell(center);
                    mark_solid(&mut solid, grid[0], column, row);
                    entities.push(
                        profile
                            .world_mut()
                            .spawn(
                                Sprite2d::new(outdoor.house_region)
                                    .with_position(center)
                                    .with_size([72.0, 100.0])
                                    .with_layer(4),
                            )
                            .id(),
                    );
                }
                "prop_tree" => {
                    let [column, row] = world_to_cell(center);
                    mark_solid(&mut solid, grid[0], column, row);
                    entities.push(
                        profile
                            .world_mut()
                            .spawn(
                                Sprite2d::new(outdoor.tree_region)
                                    .with_position(center)
                                    .with_size([48.0, 72.0])
                                    .with_layer(5),
                            )
                            .id(),
                    );
                }
                _ => {}
            }
        }
    }

    let collision = TileCollision2d::new(grid, solid)?;
    let map_entity = profile
        .world_mut()
        .spawn((outdoor.tile_map.clone().with_layer(0), collision))
        .id();
    entities.insert(0, map_entity);

    let spawn = find_spawn(
        &outdoor.object_layers,
        outdoor.tile_pixel_size,
        outdoor.world_tile_size,
        [
            grid[0] as f32 * WORLD_TILE * 0.5,
            grid[1] as f32 * WORLD_TILE * 0.5,
        ],
    );
    let portals = portals_from_objects(
        &outdoor.object_layers,
        outdoor.tile_pixel_size,
        outdoor.world_tile_size,
    );

    Ok(LocationFrame2d {
        id: LOCATION_OUTDOOR.into(),
        entities,
        portals,
        spawn,
    })
}

fn build_interior_room(
    regions: Vec<TextureRegion>,
) -> Result<(TileMap2d, TileCollision2d, [f32; 2], LocationPortal2d), Box<dyn Error>> {
    let (map, collision) = TileMapComposer2d::new(INTERIOR_GRID, [WORLD_TILE, WORLD_TILE], regions)?
        .fill(Some(LOCAL_GRASS_SOLID))
        .fill_solid(false)
        .border(LOCAL_WALL_SOLID, true)?
        .stamp([1, INTERIOR_GRID[1] - 1, INTERIOR_GRID[0] - 2, 1], |_, _| {
            Some(TileStamp2d::filled(LOCAL_GRASS_SOLID, false))
        })?
        .build()?;
    let map_w = INTERIOR_GRID[0] as f32 * WORLD_TILE;
    let map_h = INTERIOR_GRID[1] as f32 * WORLD_TILE;
    let spawn = [map_w * 0.5, map_h * 0.5];
    let exit = LocationPortal2d::from_center_size(
        [map_w * 0.5, map_h - WORLD_TILE * 0.75],
        [WORLD_TILE * 2.0, WORLD_TILE],
        LocationPortalAction2d::Exit,
    );
    Ok((map, collision, spawn, exit))
}

fn set_actor_position(
    profile: &mut Game2dProfile,
    actor: yuyib::ecs::prelude::Entity,
    position: [f32; 2],
) -> Result<(), Box<dyn Error>> {
    let mut sprite = profile
        .world_mut()
        .get_mut::<Sprite2d>(actor)
        .ok_or("player sprite missing")?;
    sprite.position = position;
    Ok(())
}

fn try_interact(demo: &mut Demo) -> Result<(), Box<dyn Error>> {
    let actor = demo.playable.actor();
    let position = demo
        .profile
        .world()
        .get::<Sprite2d>(actor)
        .ok_or("player sprite missing")?
        .position;
    let Some(portal) = demo
        .locations
        .overlapping_portal(position, PLAYER_HALF_EXTENTS)
        .cloned()
    else {
        return Ok(());
    };

    match portal.action {
        LocationPortalAction2d::Enter(target) if target == LOCATION_HOUSE => {
            enter_house(demo, position)
        }
        LocationPortalAction2d::Enter(target) => {
            eprintln!("unknown portal target: {target}");
            Ok(())
        }
        LocationPortalAction2d::Exit => exit_house(demo),
    }
}

fn enter_house(demo: &mut Demo, from: [f32; 2]) -> Result<(), Box<dyn Error>> {
    demo.outdoor_return = from;
    let regions = demo.outdoor.tile_map.regions().to_vec();
    let (map, collision, spawn, exit) = build_interior_room(regions)?;
    let map_entity = demo
        .profile
        .world_mut()
        .spawn((map.with_layer(0), collision))
        .id();
    demo.locations.push(
        demo.profile.world_mut(),
        LocationFrame2d {
            id: LOCATION_HOUSE.into(),
            entities: vec![map_entity],
            portals: vec![exit],
            spawn,
        },
    );
    set_actor_position(&mut demo.profile, demo.playable.actor(), spawn)?;
    eprintln!("entered {LOCATION_HOUSE}");
    Ok(())
}

fn exit_house(demo: &mut Demo) -> Result<(), Box<dyn Error>> {
    let restored = demo.locations.pop(demo.profile.world_mut())?;
    if restored != LOCATION_OUTDOOR {
        return Err(format!("expected outdoor restore, got {restored}").into());
    }
    let frame = spawn_outdoor_location(&mut demo.profile, &demo.outdoor)?;
    demo.locations.replace_current(frame);
    set_actor_position(
        &mut demo.profile,
        demo.playable.actor(),
        demo.outdoor_return,
    )?;
    eprintln!("returned to {LOCATION_OUTDOOR}");
    Ok(())
}

fn create_demo() -> Result<Demo, Box<dyn Error>> {
    let root = asset_root()?;
    let mut textures = Assets::new();
    let mut queued = Vec::new();

    let (tileset_tex, _tileset_size) = load_png(
        &mut textures,
        &root.join("2d/Tileset/Tileset Spring.png"),
        &mut queued,
    )?;
    let (idle_tex, idle_size) =
        load_png(&mut textures, &root.join("2d/Character/Idle.png"), &mut queued)?;
    let (walk_tex, walk_size) =
        load_png(&mut textures, &root.join("2d/Character/Walk.png"), &mut queued)?;
    let (house_tex, house_size) =
        load_png(&mut textures, &root.join("2d/Objects/House.png"), &mut queued)?;
    let (tree_tex, tree_size) = load_png(
        &mut textures,
        &root.join("2d/Objects/Maple Tree.png"),
        &mut queued,
    )?;

    let mut registry = ImporterRegistry::<ImportedTiledMap>::default();
    register_tiled_map_importer(&mut registry)?;
    let mut imported = registry.import(ImportSource::new(
        "fixtures/tiled_farm_spring.json",
        MAP_JSON.as_bytes(),
    ))?;
    imported.asset.replace_local_tiles(&[
        (LOCAL_GRASS_FRAGILE, LOCAL_GRASS_SOLID),
        (LOCAL_WALL_JAGGED, LOCAL_WALL_SOLID),
    ]);
    let bound: BoundTiledMap2d = imported
        .asset
        .bind_texture_with_world_tile_size(tileset_tex, [WORLD_TILE, WORLD_TILE])?;
    let image_uri = bound.image_uri().to_owned();
    let visual_layer = bound.visual_layer().to_owned();
    let tile_pixel_size = bound.tile_pixel_size();
    let world_tile_size = bound.world_tile_size();
    let (tile_map, collision, object_layers) = bound.into_parts();
    let grid = tile_map.grid();

    let idle_cell = TextureSize::new(32, 32)?;
    let walk_cell = TextureSize::new(32, 32)?;
    let idle_sheet = SpriteSheet::from_grid(idle_tex, idle_size, idle_cell)?;
    let walk_sheet = SpriteSheet::from_grid(walk_tex, walk_size, walk_cell)?;

    let idle_down = sheet_animation(&idle_sheet, 0, 4, 4, 180, PlaybackMode::Loop)?;
    let idle_up = sheet_animation(&idle_sheet, 1, 4, 4, 180, PlaybackMode::Loop)?;
    let idle_side = sheet_animation(&idle_sheet, 2, 4, 4, 180, PlaybackMode::Loop)?;
    let walk_down = sheet_animation(&walk_sheet, 0, 6, 6, 90, PlaybackMode::Loop)?;
    let walk_up = sheet_animation(&walk_sheet, 1, 6, 6, 90, PlaybackMode::Loop)?;
    let walk_side = sheet_animation(&walk_sheet, 2, 6, 6, 90, PlaybackMode::Loop)?;

    let set = AnimationSet::new()
        .with("idle_down", idle_down.clone())
        .with("idle_up", idle_up)
        .with("idle_side", idle_side)
        .with("walk_down", walk_down)
        .with("walk_up", walk_up)
        .with("walk_side", walk_side);
    let machine = AnimationStateMachine::new("idle_down")?
        .with_clip("idle_up")?
        .with_clip("idle_side")?
        .with_clip("walk_down")?
        .with_clip("walk_up")?
        .with_clip("walk_side")?;

    let map_w = grid[0] as f32 * WORLD_TILE;
    let map_h = grid[1] as f32 * WORLD_TILE;
    let map_center = [map_w * 0.5, map_h * 0.5];

    let mut scene_config = Game2dSceneConfig::default();
    scene_config.camera = Camera2d::new(map_center, CAMERA_PIXELS_PER_UNIT);

    let mut profile = Game2dProfile::new(scene_config);

    let house_region = TextureRegion::new(
        house_tex,
        house_size,
        yuyib::two_d::PixelPoint { x: 148, y: 0 },
        TextureSize::new(72, 100)?,
    )?;
    let tree_region = TextureRegion::new(
        tree_tex,
        tree_size,
        yuyib::two_d::PixelPoint { x: 96, y: 0 },
        TextureSize::new(32, 48)?,
    )?;

    let outdoor = OutdoorCache {
        tile_map,
        collision,
        object_layers,
        tile_pixel_size,
        world_tile_size,
        house_region,
        tree_region,
    };

    let outdoor_frame = spawn_outdoor_location(&mut profile, &outdoor)?;
    let spawn = outdoor_frame.spawn;
    let locations = LocationStack2d::new(outdoor_frame);

    let first_frame = idle_sheet.region(0).ok_or("idle frame 0")?;
    let player = profile
        .world_mut()
        .spawn((
            Sprite2d::new(first_frame)
                .with_position(spawn)
                .with_size(PLAYER_DRAW)
                .with_layer(10),
            AnimatedSprite2d::new(idle_down),
            SpriteAnimator2d::new(set, machine)?,
            SpriteFacing2d::default(),
            KinematicSpriteController2d::new(
                [PLAYER_HALF_EXTENTS[0] * 2.0, PLAYER_HALF_EXTENTS[1] * 2.0],
                180.0,
            )?,
        ))
        .id();

    eprintln!(
        "tiled farm: tileset={image_uri} layer={visual_layer} grid={grid:?} ppu={CAMERA_PIXELS_PER_UNIT} player={player:?} (E = door)"
    );

    let playable = PlayableLoop2d::new(
        PlayableLoopDesc2d::new(player, 1_024)?.with_camera(CameraFollow2d::new()),
    );

    Ok(Demo {
        _textures: textures,
        queued,
        profile,
        playable,
        clips: CardinalClipPolicy2d::standard(),
        locations,
        outdoor,
        outdoor_return: spawn,
    })
}
