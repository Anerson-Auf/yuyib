//! Machine-readable authoring metadata for Yuyib's foundation crates.
//!
//! This companion crate intentionally depends only on `yuyib-authoring`. The
//! descriptors identify current runtime capabilities without making the
//! editor UI or shipping builds dependencies of those capability crates.
//!
//! Component descriptors below describe their persisted *authoring
//! projection*, not a byte-for-byte serialization of the ECS component. In
//! particular, asset-backed assignments must persist an `AssetGuid`, never a
//! process-local runtime handle. The `Parent3d` projection similarly stores an
//! `EntityGuid`; materialization resolves that GUID to a runtime `Entity`.

#![forbid(unsafe_code)]

use std::num::NonZeroU32;

use yuyib_authoring::{
    AssetCoverageEvidence, AuthoringRegistry, CapabilityDescriptor, CapabilityId,
    ComponentDescriptor, ComponentSchemaId, CoverageStatus, FieldDescriptor, FieldKind,
    ImportSettingsDescriptor, ImportSettingsSchemaId, PluginId, RegistrationError,
    ScheduleId, SchemaVersion, SourceNavigation, SystemDescriptor, SystemId,
};

mod ids {
    pub const APPLICATION: &str = "yuyib.application";
    pub const GAME_LIFECYCLE: &str = "yuyib.game-lifecycle";
    pub const GAME_2D_SCENE: &str = "yuyib.game-2d-scene";
    pub const GAME_3D_SCENE: &str = "yuyib.game-3d-scene";
    pub const GLTF_IMPORT: &str = "yuyib.gltf-import";
    pub const GLTF_PREVIEW: &str = "yuyib.gltf-preview";
    // `yuyib.transform3d` is already the documented persisted-ID example in
    // the authoring contract. Related IDs follow the same type-name spelling.
    pub const TRANSFORM_3D: &str = "yuyib.transform3d";
    pub const LOCAL_TRANSFORM_3D: &str = "yuyib.local-transform3d";
    pub const LOCAL_MATRIX_TRANSFORM_3D: &str = "yuyib.local-matrix-transform3d";
    pub const WORLD_TRANSFORM_3D: &str = "yuyib.world-transform3d";
    pub const PARENT_3D: &str = "yuyib.parent3d";
    pub const MODEL_3D: &str = "yuyib.model3d";
    pub const DIRECTIONAL_LIGHT_3D: &str = "yuyib.directional-light3d";
    pub const RENDER_3D: &str = "yuyib.render3d";
    pub const COLLISION_3D: &str = "yuyib.collision3d";
    pub const SPRITE_2D: &str = "yuyib.sprite2d";
    pub const ANIMATED_SPRITE_2D: &str = "yuyib.animated-sprite2d";
    pub const TILE_MAP_2D: &str = "yuyib.tile-map2d";
    pub const KINEMATIC_SPRITE_CONTROLLER_2D: &str = "yuyib.kinematic-sprite-controller2d";
    pub const GAMEPLAY_INTERACTIONS: &str = "yuyib.gameplay-interactions";
    pub const CUSTOM_RENDER_PASSES: &str = "yuyib.custom-render-passes";

    pub const GLTF_IMPORT_SETTINGS: &str = "yuyib.gltf-import-settings";

    pub const HIERARCHY_PROPAGATION: &str = "yuyib.system.hierarchy-propagation-3d";
    pub const SPRITE_ANIMATION: &str = "yuyib.system.sprite-animation-2d";
    pub const TILE_MAP_ANIMATION: &str = "yuyib.system.tile-map-animation-2d";
    pub const KINEMATIC_SPRITE_CONTROLLER: &str = "yuyib.system.kinematic-sprite-controller-2d";

    pub const CALLER_DRIVEN_SCHEDULE: &str = "yuyib.schedule.caller-driven";
}

/// Registers authoring coverage for the current Yuyib foundation.
///
/// The registration is deterministic and deliberately contains no preview
/// adapter binary. `yuyib.gltf-import` / `yuyib.gltf-preview` are
/// [`CoverageStatus::Asset`]; hosts must call `yuyib_gltf_authoring::register`
/// to attach the production PreviewAdapter. Mesh/material/animation selection
/// and overlays remain future PreviewFeatures.
///
/// # Errors
///
/// Returns [`RegistrationError`] when the target registry already contains one
/// of these stable IDs or when a descriptor dependency is missing.
pub fn register_foundation(registry: &mut AuthoringRegistry) -> Result<(), RegistrationError> {
    register_capabilities(registry)?;
    register_component_schemas(registry)?;
    register_import_settings(registry)?;
    register_systems(registry)
}

#[allow(clippy::too_many_lines)]
fn register_capabilities(registry: &mut AuthoringRegistry) -> Result<(), RegistrationError> {
    for descriptor in [
        capability(
            ids::APPLICATION,
            "Application host",
            CoverageStatus::Unavailable,
            "yuyib.app",
            "crates/yuyib-app/src/lib.rs",
        ),
        capability(
            ids::GAME_LIFECYCLE,
            "Game lifecycle and schedules",
            CoverageStatus::Unavailable,
            "yuyib.game",
            "crates/yuyib-game/src/lib.rs",
        ),
        capability(
            ids::GAME_2D_SCENE,
            "Game2dScene",
            CoverageStatus::Unavailable,
            "yuyib.game-2d",
            "crates/yuyib-game-2d/src/scene.rs",
        ),
        capability(
            ids::GAME_3D_SCENE,
            "Game3dScene",
            CoverageStatus::Unavailable,
            "yuyib.render-3d",
            "crates/yuyib-render-3d/src/scene.rs",
        ),
        capability(
            ids::GLTF_IMPORT,
            "glTF import",
            CoverageStatus::Asset,
            "yuyib.gltf",
            "crates/yuyib-gltf/src/lib.rs",
        )
        .with_asset_evidence(gltf_asset_evidence()),
        capability(
            ids::GLTF_PREVIEW,
            "glTF authoring preview",
            CoverageStatus::Asset,
            "yuyib.gltf",
            "crates/yuyib-gltf-authoring/src/lib.rs",
        )
        .with_asset_evidence(gltf_asset_evidence()),
        capability(
            ids::TRANSFORM_3D,
            "Transform3d",
            CoverageStatus::Visual,
            "yuyib.game-3d",
            "crates/yuyib-game-3d/src/lib.rs",
        ),
        capability(
            ids::LOCAL_TRANSFORM_3D,
            "LocalTransform3d",
            CoverageStatus::Visual,
            "yuyib.game-3d",
            "crates/yuyib-game-3d/src/lib.rs",
        ),
        capability(
            ids::LOCAL_MATRIX_TRANSFORM_3D,
            "LocalMatrixTransform3d",
            CoverageStatus::Unavailable,
            "yuyib.game-3d",
            "crates/yuyib-game-3d/src/lib.rs",
        ),
        capability(
            ids::WORLD_TRANSFORM_3D,
            "WorldTransform3d",
            CoverageStatus::Unavailable,
            "yuyib.game-3d",
            "crates/yuyib-game-3d/src/lib.rs",
        ),
        capability(
            ids::PARENT_3D,
            "Parent3d relationship",
            CoverageStatus::Visual,
            "yuyib.game-3d",
            "crates/yuyib-game-3d/src/lib.rs",
        ),
        capability(
            ids::MODEL_3D,
            "Model3d",
            CoverageStatus::Visual,
            "yuyib.game-3d",
            "crates/yuyib-game-3d/src/lib.rs",
        ),
        capability(
            ids::DIRECTIONAL_LIGHT_3D,
            "DirectionalLight3d",
            CoverageStatus::Visual,
            "yuyib.game-3d",
            "crates/yuyib-game-3d/src/lib.rs",
        ),
        capability(
            ids::RENDER_3D,
            "Render3d (nodraw)",
            CoverageStatus::Visual,
            "yuyib.game-3d",
            "crates/yuyib-game-3d/src/lib.rs",
        ),
        capability(
            ids::COLLISION_3D,
            "Collision3d (nocollide)",
            CoverageStatus::Visual,
            "yuyib.game-3d",
            "crates/yuyib-game-3d/src/lib.rs",
        ),
        capability(
            ids::SPRITE_2D,
            "Sprite2d",
            CoverageStatus::Unavailable,
            "yuyib.game-2d",
            "crates/yuyib-game-2d/src/lib.rs",
        ),
        capability(
            ids::ANIMATED_SPRITE_2D,
            "AnimatedSprite2d",
            CoverageStatus::Unavailable,
            "yuyib.game-2d",
            "crates/yuyib-game-2d/src/lib.rs",
        ),
        capability(
            ids::TILE_MAP_2D,
            "TileMap2d",
            CoverageStatus::Unavailable,
            "yuyib.game-2d",
            "crates/yuyib-game-2d/src/lib.rs",
        ),
        capability(
            ids::KINEMATIC_SPRITE_CONTROLLER_2D,
            "KinematicSpriteController2d",
            CoverageStatus::Unavailable,
            "yuyib.game-2d",
            "crates/yuyib-game-2d/src/lib.rs",
        ),
        capability(
            ids::GAMEPLAY_INTERACTIONS,
            "Gameplay interactions",
            CoverageStatus::Visual,
            "yuyib.gameplay",
            "crates/yuyib-gameplay/src/lib.rs",
        ),
        capability(
            ids::CUSTOM_RENDER_PASSES,
            "Custom render graph passes",
            CoverageStatus::Unavailable,
            "yuyib.render",
            "crates/yuyib-render/src/graph.rs",
        ),
    ] {
        registry.register_capability(descriptor)?;
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn register_component_schemas(registry: &mut AuthoringRegistry) -> Result<(), RegistrationError> {
    for descriptor in [
        transform3d_component(ids::TRANSFORM_3D, ids::TRANSFORM_3D, true),
        transform3d_component(ids::LOCAL_TRANSFORM_3D, ids::LOCAL_TRANSFORM_3D, true),
        component(
            ids::LOCAL_MATRIX_TRANSFORM_3D,
            ids::LOCAL_MATRIX_TRANSFORM_3D,
        )
        .with_field(FieldDescriptor::new(
            "matrix",
            "Local matrix",
            FieldKind::Specialized {
                widget: "yuyib.widget.matrix4".to_owned(),
            },
        )),
        component(ids::WORLD_TRANSFORM_3D, ids::WORLD_TRANSFORM_3D).with_field(
            FieldDescriptor::new(
                "matrix",
                "World matrix",
                FieldKind::Specialized {
                    widget: "yuyib.widget.matrix4".to_owned(),
                },
            )
            .read_only(true),
        ),
        component(ids::PARENT_3D, ids::PARENT_3D).with_field(FieldDescriptor::new(
            "parent",
            "Parent",
            FieldKind::EntityReference,
        )),
        component(ids::MODEL_3D, ids::MODEL_3D)
            .with_field(FieldDescriptor::new(
                "model",
                "Model",
                FieldKind::AssetReference,
            ))
            .with_field(FieldDescriptor::new(
                "mesh",
                "Mesh",
                FieldKind::Specialized {
                    widget: "yuyib.widget.optional-u32".to_owned(),
                },
            ))
            .with_field(FieldDescriptor::new("visible", "Visible", FieldKind::Bool))
            .with_field(FieldDescriptor::new(
                "render_order",
                "Render order",
                FieldKind::I32,
            )),
        component(ids::DIRECTIONAL_LIGHT_3D, ids::DIRECTIONAL_LIGHT_3D)
            .with_field(
                FieldDescriptor::new("direction.x", "Direction X", FieldKind::F32).with_unit("axis"),
            )
            .with_field(
                FieldDescriptor::new("direction.y", "Direction Y", FieldKind::F32).with_unit("axis"),
            )
            .with_field(
                FieldDescriptor::new("direction.z", "Direction Z", FieldKind::F32).with_unit("axis"),
            )
            .with_field(
                FieldDescriptor::new("color.x", "Colour R", FieldKind::F32).with_unit("linear"),
            )
            .with_field(
                FieldDescriptor::new("color.y", "Colour G", FieldKind::F32).with_unit("linear"),
            )
            .with_field(
                FieldDescriptor::new("color.z", "Colour B", FieldKind::F32).with_unit("linear"),
            )
            .with_field(
                FieldDescriptor::new("illuminance_lux", "Illuminance", FieldKind::F32)
                    .with_unit("lux"),
            )
            .with_field(FieldDescriptor::new("enabled", "Enabled", FieldKind::Bool)),
        component(ids::RENDER_3D, ids::RENDER_3D).with_field(FieldDescriptor::new(
            "draw",
            "Draw (false = nodraw)",
            FieldKind::Bool,
        )),
        component(ids::COLLISION_3D, ids::COLLISION_3D)
            .with_field(FieldDescriptor::new(
                "enabled",
                "Enabled (false = nocollide)",
                FieldKind::Bool,
            ))
            .with_field(FieldDescriptor::new(
                "layer",
                "Layer tag",
                FieldKind::String,
            ))
            .with_field(FieldDescriptor::new(
                "collide_with",
                "Collide with tags (empty = all; include player for locomotion mesh)",
                FieldKind::String,
            )),
        component(ids::SPRITE_2D, ids::SPRITE_2D)
            .with_field(FieldDescriptor::new(
                "region",
                "Texture region",
                FieldKind::Specialized {
                    widget: "yuyib.widget.texture-region".to_owned(),
                },
            ))
            .with_field(play_field("position", "Position", FieldKind::Vec2, "units"))
            .with_field(play_field("size", "Size", FieldKind::Vec2, "units"))
            .with_field(play_field(
                "rotation_radians",
                "Rotation",
                FieldKind::F32,
                "radians",
            ))
            .with_field(FieldDescriptor::new("tint", "Tint", FieldKind::Color))
            .with_field(FieldDescriptor::new("layer", "Layer", FieldKind::I32)),
        component(ids::ANIMATED_SPRITE_2D, ids::ANIMATED_SPRITE_2D).with_field(
            FieldDescriptor::new(
                "animation",
                "Animation",
                FieldKind::Specialized {
                    widget: "yuyib.widget.sprite-animation".to_owned(),
                },
            ),
        ),
        component(ids::TILE_MAP_2D, ids::TILE_MAP_2D)
            .with_field(FieldDescriptor::new(
                "map",
                "Tile map",
                FieldKind::Specialized {
                    widget: "yuyib.widget.tile-map".to_owned(),
                },
            ))
            .with_field(play_field("position", "Position", FieldKind::Vec2, "units"))
            .with_field(FieldDescriptor::new("layer", "Layer", FieldKind::I32))
            .with_field(FieldDescriptor::new("visible", "Visible", FieldKind::Bool)),
        component(
            ids::KINEMATIC_SPRITE_CONTROLLER_2D,
            ids::KINEMATIC_SPRITE_CONTROLLER_2D,
        )
        .with_field(FieldDescriptor::new(
            "config",
            "Controller",
            FieldKind::Specialized {
                widget: "yuyib.widget.kinematic-controller2d".to_owned(),
            },
        )),
        component("yuyib.interactable", ids::GAMEPLAY_INTERACTIONS)
            .with_field(FieldDescriptor::new(
                "interaction",
                "Interaction",
                FieldKind::String,
            ))
            .with_field(FieldDescriptor::new(
                "enabled",
                "Enabled",
                FieldKind::Bool,
            ))
            .with_field(FieldDescriptor::new(
                "max_distance",
                "Max distance",
                FieldKind::F32,
            )),
        component("yuyib.trigger", ids::GAMEPLAY_INTERACTIONS)
            .with_field(FieldDescriptor::new(
                "trigger",
                "Trigger",
                FieldKind::String,
            ))
            .with_field(FieldDescriptor::new(
                "enabled",
                "Enabled",
                FieldKind::Bool,
            ))
            .with_field(FieldDescriptor::new(
                "radius",
                "Radius",
                FieldKind::F32,
            )),
    ] {
        registry.register_component(descriptor)?;
    }
    Ok(())
}

fn component(schema: &str, capability: &str) -> ComponentDescriptor {
    let source = if capability.contains("2d") || capability == ids::GAMEPLAY_INTERACTIONS {
        if capability == ids::GAMEPLAY_INTERACTIONS {
            "crates/yuyib-gameplay/src/lib.rs"
        } else {
            "crates/yuyib-game-2d/src/lib.rs"
        }
    } else {
        "crates/yuyib-game-3d/src/lib.rs"
    };
    ComponentDescriptor::new(
        component_id(schema),
        capability_id(capability),
        schema_version(1),
    )
    .with_runtime_source(SourceNavigation::file(source))
}

fn transform3d_component(
    schema: &str,
    capability: &str,
    play_whitelist: bool,
) -> ComponentDescriptor {
    let mut descriptor = component(schema, capability);
    for (path, title, unit) in [
        ("translation.x", "Translation X", "units"),
        ("translation.y", "Translation Y", "units"),
        ("translation.z", "Translation Z", "units"),
        ("rotation.x", "Rotation X", "quaternion"),
        ("rotation.y", "Rotation Y", "quaternion"),
        ("rotation.z", "Rotation Z", "quaternion"),
        ("rotation.w", "Rotation W", "quaternion"),
        ("scale.x", "Scale X", "ratio"),
        ("scale.y", "Scale Y", "ratio"),
        ("scale.z", "Scale Z", "ratio"),
    ] {
        let field = if play_whitelist {
            play_field(path, title, FieldKind::F32, unit)
        } else {
            FieldDescriptor::new(path, title, FieldKind::F32).with_unit(unit)
        };
        descriptor = descriptor.with_field(field);
    }
    descriptor
}

fn play_field(path: &str, title: &str, kind: FieldKind, unit: &str) -> FieldDescriptor {
    FieldDescriptor::new(path, title, kind)
        .with_unit(unit)
        .allow_apply_play_changes(true)
}

fn register_import_settings(registry: &mut AuthoringRegistry) -> Result<(), RegistrationError> {
    registry.register_import_settings(ImportSettingsDescriptor::new(
        import_settings_id(ids::GLTF_IMPORT_SETTINGS),
        capability_id(ids::GLTF_IMPORT),
        schema_version(1),
    ))
}

fn register_systems(registry: &mut AuthoringRegistry) -> Result<(), RegistrationError> {
    registry.register_system(
        system(
            ids::HIERARCHY_PROPAGATION,
            "yuyib.game-3d",
            ids::CALLER_DRIVEN_SCHEDULE,
            "crates/yuyib-game-3d/src/lib.rs",
            528,
        )
        .reading(component_id(ids::LOCAL_TRANSFORM_3D))
        .reading(component_id(ids::LOCAL_MATRIX_TRANSFORM_3D))
        .reading(component_id(ids::PARENT_3D))
        .writing(component_id(ids::WORLD_TRANSFORM_3D))
        .writing(component_id(ids::TRANSFORM_3D)),
    )?;
    registry.register_system(
        system(
            ids::SPRITE_ANIMATION,
            "yuyib.game-2d",
            ids::CALLER_DRIVEN_SCHEDULE,
            "crates/yuyib-game-2d/src/lib.rs",
            549,
        )
        .writing(component_id(ids::ANIMATED_SPRITE_2D))
        .writing(component_id(ids::SPRITE_2D)),
    )?;
    registry.register_system(
        system(
            ids::TILE_MAP_ANIMATION,
            "yuyib.game-2d",
            ids::CALLER_DRIVEN_SCHEDULE,
            "crates/yuyib-game-2d/src/lib.rs",
            1007,
        )
        .writing(component_id(ids::TILE_MAP_2D)),
    )?;
    registry.register_system(
        system(
            ids::KINEMATIC_SPRITE_CONTROLLER,
            "yuyib.game-2d",
            ids::CALLER_DRIVEN_SCHEDULE,
            "crates/yuyib-game-2d/src/lib.rs",
            1623,
        )
        .reading(component_id(ids::KINEMATIC_SPRITE_CONTROLLER_2D))
        .reading(component_id(ids::TILE_MAP_2D))
        .writing(component_id(ids::SPRITE_2D)),
    )
}

fn capability(
    id: &str,
    title: &str,
    coverage: CoverageStatus,
    owner: &str,
    documentation: &str,
) -> CapabilityDescriptor {
    let descriptor =
        CapabilityDescriptor::new(capability_id(id), title, coverage, plugin_id(owner))
            .with_documentation(documentation)
            .with_source(SourceNavigation::file(documentation));
    if coverage == CoverageStatus::Unavailable {
        let (reason, milestone) = (
            "Metadata exists, but no typed end-to-end authoring adapter is registered.",
            "typed-adapter-and-coverage-gate",
        );
        descriptor.unavailable(reason, milestone)
    } else {
        descriptor
    }
}

fn gltf_asset_evidence() -> AssetCoverageEvidence {
    AssetCoverageEvidence::new(
        import_settings_id(ids::GLTF_IMPORT_SETTINGS),
        capability_id(ids::GLTF_PREVIEW),
        [
            "gltf-flat-only-material",
            "gltf-unbound-material",
            "gltf-external-texture-uri",
            "gltf-skipped-primitives",
            "gltf-missing-uv-set",
        ],
    )
}

fn system(
    id: &str,
    owner: &str,
    schedule: &str,
    source_file: &str,
    source_line: u32,
) -> SystemDescriptor {
    SystemDescriptor::new(system_id(id), plugin_id(owner), schedule_id(schedule))
        .with_source(SourceNavigation::file(source_file).at(non_zero(source_line), None))
}

fn capability_id(value: &str) -> CapabilityId {
    CapabilityId::new(value).expect("foundation capability IDs are valid constants")
}

fn component_id(value: &str) -> ComponentSchemaId {
    ComponentSchemaId::new(value).expect("foundation component IDs are valid constants")
}

fn import_settings_id(value: &str) -> ImportSettingsSchemaId {
    ImportSettingsSchemaId::new(value).expect("foundation import-setting IDs are valid constants")
}

fn plugin_id(value: &str) -> PluginId {
    PluginId::new(value).expect("foundation plugin IDs are valid constants")
}

fn schedule_id(value: &str) -> ScheduleId {
    ScheduleId::new(value).expect("foundation schedule IDs are valid constants")
}

fn system_id(value: &str) -> SystemId {
    SystemId::new(value).expect("foundation system IDs are valid constants")
}

fn schema_version(value: u32) -> SchemaVersion {
    SchemaVersion::new(value).expect("foundation schema versions are non-zero constants")
}

fn non_zero(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).expect("foundation source lines are non-zero constants")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foundation_manifest_is_deterministic_and_machine_readable() {
        let mut first = AuthoringRegistry::new();
        let mut second = AuthoringRegistry::new();
        register_foundation(&mut first).expect("first foundation registry");
        register_foundation(&mut second).expect("second foundation registry");

        let first = first.coverage_manifest();
        let second = second.coverage_manifest();
        assert_eq!(first, second);
        assert!(
            first
                .capabilities
                .windows(2)
                .all(|pair| pair[0].id() < pair[1].id())
        );
        assert!(
            first
                .components
                .windows(2)
                .all(|pair| pair[0].id() < pair[1].id())
        );
        assert!(
            first
                .systems
                .windows(2)
                .all(|pair| pair[0].id() < pair[1].id())
        );

        let json = serde_json::to_value(first).expect("serialize coverage manifest");
        assert_eq!(json["capabilities"][0]["id"], "yuyib.animated-sprite2d");
    }

    #[test]
    fn duplicate_foundation_registration_is_a_hard_error() {
        let mut registry = AuthoringRegistry::new();
        register_foundation(&mut registry).expect("initial registration");

        assert!(matches!(
            register_foundation(&mut registry),
            Err(RegistrationError::Duplicate {
                kind: "capability",
                id,
            }) if id == ids::APPLICATION
        ));
    }

    #[test]
    fn descriptors_cannot_silently_create_missing_coverage() {
        let mut registry = AuthoringRegistry::new();
        let missing = capability_id("project.missing-capability");
        let descriptor = ComponentDescriptor::new(
            component_id("project.missing-component"),
            missing.clone(),
            schema_version(1),
        );

        assert_eq!(
            registry.register_component(descriptor),
            Err(RegistrationError::MissingCapability(missing))
        );
    }

    #[test]
    fn gltf_import_and_preview_are_asset_surfaces_awaiting_host_adapter() {
        let mut registry = AuthoringRegistry::new();
        register_foundation(&mut registry).expect("foundation registration");

        let preview = capability_id(ids::GLTF_PREVIEW);
        let import = capability_id(ids::GLTF_IMPORT);
        assert_eq!(
            registry
                .capability(&preview)
                .expect("glTF preview coverage")
                .surfaces(),
            &std::collections::BTreeSet::from([CoverageStatus::Asset])
        );
        assert_eq!(
            registry
                .capability(&import)
                .expect("glTF import coverage")
                .surfaces(),
            &std::collections::BTreeSet::from([CoverageStatus::Asset])
        );
        assert!(
            registry
                .capability(&import)
                .expect("import")
                .asset_evidence()
                .is_some()
        );
        // Adapter is registered by the Editor host via yuyib-gltf-authoring.
        assert!(registry.preview_descriptor(&preview).is_none());
        assert!(registry.preview_adapter(&preview).is_none());
        assert!(matches!(
            registry.validate_coverage_gate(),
            Err(yuyib_authoring::CoverageGateError::MissingPreviewAdapter { .. })
        ));
    }

    #[test]
    fn every_schema_references_registered_coverage() {
        let mut registry = AuthoringRegistry::new();
        register_foundation(&mut registry).expect("foundation registration");
        let manifest = registry.coverage_manifest();

        for component in &manifest.components {
            assert!(registry.capability(component.capability()).is_some());
        }
        for settings in &manifest.import_settings {
            assert!(registry.capability(settings.capability()).is_some());
        }
    }

    #[test]
    fn transform_fields_generate_inspector_and_explicit_play_whitelist() {
        let mut registry = AuthoringRegistry::new();
        register_foundation(&mut registry).expect("foundation registration");
        let transform = registry
            .component(&component_id(ids::TRANSFORM_3D))
            .expect("transform schema");
        assert_eq!(transform.fields().len(), 10);
        assert!(
            transform
                .fields()
                .iter()
                .all(FieldDescriptor::applies_play_changes)
        );
        let local = registry
            .component(&component_id(ids::LOCAL_TRANSFORM_3D))
            .expect("local transform schema");
        assert_eq!(local.fields().len(), 10);
        assert!(
            local
                .fields()
                .iter()
                .all(FieldDescriptor::applies_play_changes)
        );
        assert!(
            registry
                .capability(&capability_id(ids::TRANSFORM_3D))
                .expect("transform coverage")
                .surfaces()
                .contains(&CoverageStatus::Visual)
        );
        let parent = registry
            .component(&component_id(ids::PARENT_3D))
            .expect("parent schema");
        assert_eq!(parent.fields().len(), 1);
        assert_eq!(parent.fields()[0].path(), "parent");
        assert!(
            parent
                .fields()
                .iter()
                .all(|field| !field.applies_play_changes())
        );
        assert!(
            registry
                .capability(&capability_id(ids::PARENT_3D))
                .expect("parent coverage")
                .surfaces()
                .contains(&CoverageStatus::Visual)
        );
    }
}
