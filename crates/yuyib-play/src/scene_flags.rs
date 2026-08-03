//! Materialize authored `yuyib.render3d` / `yuyib.collision3d` (nodraw / nocollide).

use yuyib_authoring::SceneDocument;
use yuyib_ecs::prelude::{Entity, World};
use yuyib_game_3d::{CollisionFlags3d, Model3d, RenderFlags3d};

use crate::play_log::play_log;

/// Hides the Player placeholder mesh in Play.
///
/// Authored `Player` ships a `builtin:cube` for editor placement. Leaving that
/// mesh extracted makes a **large near-camera cube appear whenever chase/FPS
/// yaw points at the player AABB** (frustum enter/leave — not near-plane
/// flicker). Collision uses the static map mesh / capsule, not this visual, so
/// the component is removed entirely rather than only flagged nodraw.
pub fn hide_player_visual(world: &mut World, player: Entity) {
    world.entity_mut(player).insert(RenderFlags3d::NODRAW);
    world.entity_mut(player).remove::<Model3d>();
    play_log(
        "yuyib-play: Player Model3d removed (placeholder cube — yaw would frustum-show it)",
    );
}

/// Applies render/collision flags from the scene document onto materialized entities.
pub fn materialize_render_collision_flags(
    document: &SceneDocument,
    world: &mut World,
    entities: &std::collections::BTreeMap<yuyib_authoring::EntityGuid, Entity>,
) {
    let mut nodraw = 0_usize;
    let mut nocollide = 0_usize;
    for (guid, &entity) in entities {
        let Some(record) = document
            .entities
            .iter()
            .find(|candidate| candidate.guid == *guid)
        else {
            continue;
        };

        if let Some(component) = record
            .components
            .iter()
            .find(|component| component.schema().as_str() == "yuyib.render3d")
        {
            let draw = component
                .payload()
                .get("draw")
                .and_then(|value| value.as_bool())
                .unwrap_or(true);
            world.entity_mut(entity).insert(RenderFlags3d::new(draw));
            if !draw {
                nodraw += 1;
                if let Some(mut model) = world.get_mut::<Model3d>(entity) {
                    *model = model.clone().with_visible(false);
                }
                play_log(format!(
                    "yuyib-play: nodraw on `{}`",
                    record.name.as_deref().unwrap_or("<unnamed>")
                ));
            }
        }

        if let Some(component) = record
            .components
            .iter()
            .find(|component| component.schema().as_str() == "yuyib.collision3d")
        {
            let Some(flags) = collision_from_payload(component.payload()) else {
                play_log(format!(
                    "yuyib-play: skip invalid yuyib.collision3d on `{}`",
                    record.name.as_deref().unwrap_or("<unnamed>")
                ));
                continue;
            };
            if !flags.contributes_to_player_mesh() {
                nocollide += 1;
                play_log(format!(
                    "yuyib-play: nocollide (vs player mesh) on `{}` layer=`{}`",
                    record.name.as_deref().unwrap_or("<unnamed>"),
                    flags.layer
                ));
            }
            world.entity_mut(entity).insert(flags);
        }
    }
    if nodraw > 0 || nocollide > 0 {
        play_log(format!(
            "yuyib-play: scene flags nodraw={nodraw} nocollide_vs_player={nocollide}"
        ));
    }
}

fn collision_from_payload(payload: &serde_json::Value) -> Option<CollisionFlags3d> {
    let enabled = payload
        .get("enabled")
        .and_then(|value| value.as_bool())
        .unwrap_or(true);
    let layer = payload
        .get("layer")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim()
        .to_owned();
    let collide_with = parse_tag_list(payload.get("collide_with"));
    Some(CollisionFlags3d {
        enabled,
        collide_with,
        layer,
    })
}

fn parse_tag_list(value: Option<&serde_json::Value>) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    if let Some(text) = value.as_str() {
        return text
            .split([',', ';', ' '])
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(str::to_owned)
            .collect();
    }
    if let Some(items) = value.as_array() {
        return items
            .iter()
            .filter_map(|item| item.as_str())
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(str::to_owned)
            .collect();
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use std::f32::consts::TAU;

    use yuyib_assets::Assets;
    use yuyib_game_3d::{
        ClipDepthRange3d, Frustum3d, ModelBoundsRegistry3d, ModelDraw, Transform3d,
        extract_models_with_lod_3d, filter_extracted_models_by_frustum_3d,
        register_computed_model_bounds_3d,
    };
    use yuyib_model::Model;

    use super::*;

    /// prj2-ish layout: Player + NoDrawSolid + GhostProp + ExitVolume cubes.
    const PLAYER: [f32; 3] = [5.3, 1.59, 20.3];
    const NODRAW: [f32; 3] = [-1.32, 1.0, 19.5];
    const GHOST: [f32; 3] = [2.0, 0.7, 21.63];
    const EXIT: [f32; 3] = [4.5, 1.0, 19.5];
    const CUBE_HALF: f32 = 0.7;

    fn translation_of(draw: &ModelDraw) -> [f32; 3] {
        [draw.model_matrix[12], draw.model_matrix[13], draw.model_matrix[14]]
    }

    fn near_translation(draw: &ModelDraw, expected: [f32; 3], eps: f32) -> bool {
        let t = translation_of(draw);
        (t[0] - expected[0]).abs() <= eps
            && (t[1] - expected[1]).abs() <= eps
            && (t[2] - expected[2]).abs() <= eps
    }

    /// Same ZeroToOne perspective × look-at convention as `Camera3d::view_projection`.
    fn chase_view_projection(
        eye: [f32; 3],
        target: [f32; 3],
        near: f32,
        far: f32,
        fov_y: f32,
        aspect: f32,
    ) -> [f32; 16] {
        let forward = {
            let d = [
                target[0] - eye[0],
                target[1] - eye[1],
                target[2] - eye[2],
            ];
            let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            [d[0] / len, d[1] / len, d[2] / len]
        };
        let up = [0.0_f32, 1.0, 0.0];
        let side = {
            let c = [
                forward[1] * up[2] - forward[2] * up[1],
                forward[2] * up[0] - forward[0] * up[2],
                forward[0] * up[1] - forward[1] * up[0],
            ];
            let len = (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt();
            [c[0] / len, c[1] / len, c[2] / len]
        };
        let actual_up = [
            side[1] * forward[2] - side[2] * forward[1],
            side[2] * forward[0] - side[0] * forward[2],
            side[0] * forward[1] - side[1] * forward[0],
        ];
        let view = [
            side[0],
            actual_up[0],
            -forward[0],
            0.0,
            side[1],
            actual_up[1],
            -forward[1],
            0.0,
            side[2],
            actual_up[2],
            -forward[2],
            0.0,
            -(side[0] * eye[0] + side[1] * eye[1] + side[2] * eye[2]),
            -(actual_up[0] * eye[0] + actual_up[1] * eye[1] + actual_up[2] * eye[2]),
            forward[0] * eye[0] + forward[1] * eye[1] + forward[2] * eye[2],
            1.0,
        ];
        let focal = 1.0 / (fov_y * 0.5).tan();
        let projection = [
            focal / aspect,
            0.0,
            0.0,
            0.0,
            0.0,
            focal,
            0.0,
            0.0,
            0.0,
            0.0,
            far / (near - far),
            -1.0,
            0.0,
            0.0,
            (near * far) / (near - far),
            0.0,
        ];
        multiply4(projection, view)
    }

    fn multiply4(left: [f32; 16], right: [f32; 16]) -> [f32; 16] {
        let mut out = [0.0_f32; 16];
        for col in 0..4 {
            for row in 0..4 {
                out[col * 4 + row] = (0..4)
                    .map(|k| left[k * 4 + row] * right[col * 4 + k])
                    .sum();
            }
        }
        out
    }

    fn orbit_eye(focus: [f32; 3], yaw: f32, pitch: f32, distance: f32) -> [f32; 3] {
        let (yaw_sin, yaw_cos) = yaw.sin_cos();
        let (pitch_sin, pitch_cos) = pitch.sin_cos();
        [
            focus[0] + distance * yaw_sin * pitch_cos,
            focus[1] + distance * pitch_sin,
            focus[2] + distance * yaw_cos * pitch_cos,
        ]
    }

    fn visible_scene_draws(
        world: &mut World,
        models: &Assets<Model>,
        eye: [f32; 3],
        target: [f32; 3],
    ) -> Vec<ModelDraw> {
        let extracted = extract_models_with_lod_3d(world, eye).expect("lod");
        let (scene, _overlay) = extracted.partition_overlay();
        let mut bounds = ModelBoundsRegistry3d::new();
        for batch in scene.batches() {
            register_computed_model_bounds_3d(&mut bounds, models, batch.model()).expect("bounds");
        }
        let vp = chase_view_projection(eye, target, 0.05, 500.0, 1.0, 16.0 / 9.0);
        let frustum = Frustum3d::from_clip_matrix(vp, ClipDepthRange3d::ZeroToOne).expect("frustum");
        filter_extracted_models_by_frustum_3d(&scene, &frustum, &bounds)
            .expect("cull")
            .visible()
            .batches()
            .iter()
            .flat_map(|batch| batch.draws().iter().copied())
            .collect()
    }

    #[test]
    fn without_hide_player_cube_enters_frustum_on_some_yaw() {
        let mut models = Assets::new();
        let cube = models.insert(Model::cube(CUBE_HALF).expect("cube"));
        let mut world = World::new();
        let player = world
            .spawn((
                Model3d::new(cube),
                Transform3d::from_translation(PLAYER),
            ))
            .id();
        world.spawn((
            Model3d::new(cube),
            Transform3d::from_translation(GHOST),
        ));

        // Camera pressed at NoDrawSolid, looking around — the Player placeholder
        // is ~6 m away. It must enter the frustum only when yaw faces it (the
        // "huge cube appears from yaw" class), not stay glued to look-at.
        let mut saw_player = false;
        let mut missed_player = false;
        for step in 0..36 {
            let yaw = step as f32 * (TAU / 36.0);
            let eye = orbit_eye(NODRAW, yaw, 0.12, 1.5);
            let draws = visible_scene_draws(&mut world, &models, eye, NODRAW);
            let hit = draws.iter().any(|d| near_translation(d, PLAYER, 0.05));
            saw_player |= hit;
            missed_player |= !hit;
        }
        assert!(
            saw_player && missed_player,
            "regression fixture: visible Player cube must appear for some yaw and leave for others \
             when orbiting near NoDrawSolid; player={player:?}"
        );
    }

    #[test]
    fn hide_player_removes_model_so_yaw_never_shows_player_cube() {
        let mut models = Assets::new();
        let cube = models.insert(Model::cube(CUBE_HALF).expect("cube"));
        let mut world = World::new();
        let player = world
            .spawn((
                Model3d::new(cube),
                Transform3d::from_translation(PLAYER),
            ))
            .id();
        world.spawn((
            Model3d::new(cube),
            Transform3d::from_translation(GHOST),
        ));
        hide_player_visual(&mut world, player);
        assert!(world.get::<Model3d>(player).is_none());

        for step in 0..36 {
            let yaw = step as f32 * (TAU / 36.0);
            // Pressed against NoDrawSolid: orbit around that contact point.
            let focus = NODRAW;
            let eye = orbit_eye(focus, yaw, 0.2, 1.8);
            let draws = visible_scene_draws(&mut world, &models, eye, focus);
            assert!(
                !draws.iter().any(|d| near_translation(d, PLAYER, 0.05)),
                "Player cube must never frustum-appear after hide (yaw={yaw})"
            );
        }
    }

    #[test]
    fn nodraw_solid_never_appears_for_any_yaw_when_flags_applied() {
        let mut models = Assets::new();
        let cube = models.insert(Model::cube(CUBE_HALF).expect("cube"));
        let mut world = World::new();
        let player = world
            .spawn((
                Model3d::new(cube),
                Transform3d::from_translation(PLAYER),
            ))
            .id();
        world.spawn((
            Model3d::new(cube),
            RenderFlags3d::NODRAW,
            Transform3d::from_translation(NODRAW),
        ));
        world.spawn((
            Model3d::new(cube),
            Transform3d::from_translation(GHOST),
        ));
        world.spawn((
            Model3d::new(cube).with_overlay(true),
            Transform3d::from_translation(EXIT),
        ));
        hide_player_visual(&mut world, player);

        // Without flags this solid would fill the view when yaw points at it.
        let mut would_have_seen_without_flags = false;
        {
            let mut probe = World::new();
            probe.spawn((
                Model3d::new(cube),
                Transform3d::from_translation(NODRAW),
            ));
            for step in 0..24 {
                let yaw = step as f32 * (TAU / 24.0);
                let eye = orbit_eye(NODRAW, yaw, 0.15, 1.6);
                let draws = visible_scene_draws(&mut probe, &models, eye, NODRAW);
                if draws.iter().any(|d| near_translation(d, NODRAW, 0.05)) {
                    would_have_seen_without_flags = true;
                    break;
                }
            }
        }
        assert!(
            would_have_seen_without_flags,
            "fixture sanity: a visible cube at NoDrawSolid is frustum-shown for some yaw"
        );

        for step in 0..36 {
            let yaw = step as f32 * (TAU / 36.0);
            let eye = orbit_eye(NODRAW, yaw, 0.15, 1.6);
            let draws = visible_scene_draws(&mut world, &models, eye, NODRAW);
            assert!(
                !draws.iter().any(|d| near_translation(d, NODRAW, 0.05)),
                "NoDrawSolid must never be in scene draws (yaw={yaw})"
            );
            assert!(
                !draws.iter().any(|d| near_translation(d, PLAYER, 0.05)),
                "Player must stay hidden (yaw={yaw})"
            );
            // ExitVolume is overlay — partition keeps it out of scene frustum list.
            assert!(
                !draws.iter().any(|d| near_translation(d, EXIT, 0.05)),
                "overlay ExitVolume must not be camera-frustum scene draw"
            );
        }
    }

    #[test]
    fn ghost_prop_stable_when_looking_directly_at_it() {
        let mut models = Assets::new();
        let cube = models.insert(Model::cube(CUBE_HALF).expect("cube"));
        let mut world = World::new();
        world.spawn((
            Model3d::new(cube),
            Transform3d::from_translation(GHOST),
        ));
        // Eye 3 m south of GhostProp, looking at it; small yaw jitter must not drop it.
        let base_eye = [GHOST[0], GHOST[1] + 0.4, GHOST[2] - 3.0];
        for step in -6..=6 {
            let yaw = step as f32 * 0.04;
            let (s, c) = yaw.sin_cos();
            let eye = [
                base_eye[0] + s * 0.2,
                base_eye[1],
                base_eye[2] + (1.0 - c) * 0.2,
            ];
            let draws = visible_scene_draws(&mut world, &models, eye, GHOST);
            assert!(
                draws.iter().any(|d| near_translation(d, GHOST, 0.05)),
                "GhostProp must stay visible while looking at it (yaw_jitter={yaw})"
            );
        }
    }

    #[test]
    fn collision_extract_keeps_nodraw_solid() {
        let mut models = Assets::new();
        let cube = models.insert(Model::cube(CUBE_HALF).expect("cube"));
        let mut world = World::new();
        world.spawn((
            Model3d::new(cube),
            RenderFlags3d::NODRAW,
            Transform3d::from_translation(NODRAW),
        ));
        let drawn = extract_models_with_lod_3d(&mut world, PLAYER)
            .expect("lod")
            .model_count();
        assert_eq!(drawn, 0);
        let collision = yuyib_game_3d::extract_models_for_static_collision(&mut world);
        assert_eq!(collision.model_count(), 1);
        assert!(
            near_translation(&collision.batches()[0].draws()[0], NODRAW, 0.05),
            "nodraw ≠ nocollide"
        );
    }

    #[test]
    fn model_refresh_must_reapply_nodraw_or_cube_returns() {
        // Models the editor bug: Model3d payload visible:true is re-inserted on
        // refresh while yuyib.render3d.draw=false must keep the entity out of
        // extract — otherwise NoDrawSolid frustum-pops under yaw again.
        let mut models = Assets::new();
        let cube = models.insert(Model::cube(0.7).expect("cube"));
        let mut world = World::new();
        let entity = world
            .spawn((
                Model3d::new(cube).with_visible(false),
                RenderFlags3d::NODRAW,
                Transform3d::from_translation(NODRAW),
            ))
            .id();
        assert_eq!(
            extract_models_with_lod_3d(&mut world, PLAYER)
                .expect("lod")
                .model_count(),
            0
        );
        // Simulate refresh_entity_model writing authored visible:true without flags.
        world
            .entity_mut(entity)
            .insert(Model3d::new(cube).with_visible(true));
        // RenderFlags still present → must stay hidden.
        assert_eq!(
            extract_models_with_lod_3d(&mut world, PLAYER)
                .expect("lod")
                .model_count(),
            0,
            "NODRAW must win over Model3d.visible=true after refresh"
        );
        // If flags were cleared (broken ensure path), the cube returns.
        world.entity_mut(entity).remove::<RenderFlags3d>();
        assert_eq!(
            extract_models_with_lod_3d(&mut world, PLAYER)
                .expect("lod")
                .model_count(),
            1,
            "fixture: missing RenderFlags after visible refresh re-shows the cube"
        );
    }
}
