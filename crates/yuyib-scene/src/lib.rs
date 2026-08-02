//! Explicit conversion of imported glTF scenes into Yuyib ECS entities.
//!
//! This crate keeps yuyib-gltf renderer- and ECS-neutral. [`spawn_scene`] is an
//! opt-in adapter that copies an [`yuyib_gltf::ImportedAsset`] into a Bevy ECS
//! world, retaining selected roots, node-local transforms and
//! parent/child links. It never flattens a hierarchy into world transforms.
//!
//! The current yuyib-game-3d crate has no generic hierarchy or camera
//! component. This adapter therefore provides small, explicit scene components
//! instead of pretending those runtime systems already exist. A future
//! transform-propagation system can consume [`yuyib_game_3d::Parent3d`] and
//! [`yuyib_game_3d::LocalTransform3d`] while preserving imported local data.

#![forbid(unsafe_code)]

use std::{collections::HashMap, error::Error, fmt};

use yuyib_assets::Assets;
use yuyib_ecs::{
    bevy_ecs::entity::Entity,
    prelude::{Component, World},
};
use yuyib_game_3d::{
    LocalMatrixTransform3d, LocalTransform3d, Model3d, TransformHierarchyError,
    propagate_world_transforms, set_parent_3d,
};
use yuyib_gltf::{
    CameraIndex, CameraProjection, DirectionalLightIndex, ImportedAsset, ImportedDirectionalLight,
    ImportedScene, LocalTransform, NodeIndex, SceneIndex,
};
use yuyib_model::{Model, ModelHandle};

/// Selects exactly one source root scene to spawn.
///
/// glTF can contain multiple scenes. Selecting one is mandatory to avoid
/// implicitly combining separate authoring worlds. [`SceneSelection::Default`] uses the
/// document's declared default scene and fails when it is absent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SceneSelection {
    /// Select the glTF document's declared default scene.
    Default,
    /// Select one zero-based source scene explicitly.
    Index(SceneIndex),
}

/// Spawns one selected glTF root scene into ECS.
///
/// The model is cloned into models once and all spawned mesh nodes reference
/// the returned typed handle. Only nodes reachable from the selected roots are
/// created. Every selected source node maps to exactly one entity; sharing a
/// source node through more than one selected parent is rejected instead of
/// duplicating or silently choosing a parent.
///
/// # Errors
///
/// Returns [`SceneSpawnError`] when the selection is absent/invalid or when
/// source hierarchy references cannot be represented unambiguously.
pub fn spawn_scene(
    world: &mut World,
    models: &mut Assets<Model>,
    asset: &ImportedAsset,
    selection: SceneSelection,
) -> Result<SpawnedScene, SceneSpawnError> {
    let model = models.insert(asset.model.clone());
    spawn_scene_with_model(world, model, asset, selection)
}

/// Spawns one selected glTF root scene using an existing [`ModelHandle`].
///
/// Editor rematerialize / same-project reopen can reuse a stable handle so the
/// GPU model residency cache stays warm instead of re-uploading every open.
///
/// # Errors
///
/// Returns [`SceneSpawnError`] when the selection is absent/invalid or when
/// source hierarchy references cannot be represented unambiguously.
pub fn spawn_scene_with_model(
    world: &mut World,
    model: ModelHandle,
    asset: &ImportedAsset,
    selection: SceneSelection,
) -> Result<SpawnedScene, SceneSpawnError> {
    let source_scene = select_scene(&asset.scene, selection)?;
    let selected_nodes = collect_selected_nodes(&asset.scene, source_scene)?;
    let mut entities = HashMap::new();

    for (node_index, _) in &selected_nodes {
        let node = get_node(&asset.scene, *node_index)?;
        let entity = world
            .spawn(SceneNode {
                source: *node_index,
            })
            .id();
        match node.local_transform() {
            LocalTransform::Trs {
                translation,
                rotation,
                scale,
            } => {
                world.entity_mut(entity).insert(
                    LocalTransform3d::IDENTITY
                        .with_translation(translation)
                        .with_rotation(rotation)
                        .with_scale(scale),
                );
            }
            LocalTransform::Matrix { column_major } => {
                world
                    .entity_mut(entity)
                    .insert(LocalMatrixTransform3d::new(column_major));
            }
        }
        if let Some(mesh) = node.mesh() {
            world
                .entity_mut(entity)
                .insert(Model3d::new(model).with_mesh(mesh));
        }
        if let Some(source_camera) = node.camera() {
            let imported_camera = asset.scene.cameras().get(source_camera.get()).ok_or(
                SceneSpawnError::InvalidCameraReference {
                    node: *node_index,
                    camera: source_camera,
                },
            )?;
            world.entity_mut(entity).insert(SceneCamera {
                source: source_camera,
                projection: *imported_camera.projection(),
            });
        }
        if let Some(source_light) = node.directional_light() {
            let imported_light = asset
                .scene
                .directional_lights()
                .get(source_light.get())
                .ok_or(SceneSpawnError::InvalidLightReference {
                    node: *node_index,
                    light: source_light,
                })?;
            world
                .entity_mut(entity)
                .insert(SceneDirectionalLight::from_imported(
                    source_light,
                    imported_light,
                ));
        }
        entities.insert(*node_index, entity);
    }

    for (node_index, parent) in &selected_nodes {
        let entity = entity_for(&entities, *node_index)?;
        if let Some(parent) = parent {
            let parent_entity = entity_for(&entities, *parent)?;
            set_parent_3d(world, entity, parent_entity).map_err(SceneSpawnError::Hierarchy)?;
        }
    }
    propagate_world_transforms(world).map_err(SceneSpawnError::Hierarchy)?;

    let roots = source_scene
        .roots()
        .iter()
        .map(|node| entity_for(&entities, *node))
        .collect::<Result<Vec<_>, SceneSpawnError>>()?;
    Ok(SpawnedScene {
        model,
        roots,
        entities: sorted_entities(entities),
    })
}

/// A source glTF node index attached to every spawned entity.
#[derive(Component, Clone, Copy, Debug, Eq, PartialEq)]
pub struct SceneNode {
    /// Stable node index in the imported glTF document.
    pub source: NodeIndex,
}

/// Camera metadata attached to a source camera node.
///
/// This is not a render camera selection component. The application must
/// explicitly choose a camera and translate its projection to a renderer.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct SceneCamera {
    source: CameraIndex,
    projection: CameraProjection,
}

impl SceneCamera {
    /// Returns the source camera index.
    #[must_use]
    pub const fn source(&self) -> CameraIndex {
        self.source
    }

    /// Returns the unmodified source projection.
    #[must_use]
    pub const fn projection(&self) -> &CameraProjection {
        &self.projection
    }
}

/// Directional-light metadata attached to a source light node.
///
/// It intentionally does not attach `DirectionalLight3d`: that component needs
/// a world-space direction, while the current hierarchy runtime has no
/// transform-propagation system. Converting it here would make parent rotation
/// silently stop affecting the light.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct SceneDirectionalLight {
    source: DirectionalLightIndex,
    color: [f32; 3],
    illuminance_lux: f32,
}

impl SceneDirectionalLight {
    fn from_imported(source: DirectionalLightIndex, light: &ImportedDirectionalLight) -> Self {
        Self {
            source,
            color: light.color(),
            illuminance_lux: light.illuminance_lux(),
        }
    }

    /// Returns the source directional-light index.
    #[must_use]
    pub const fn source(&self) -> DirectionalLightIndex {
        self.source
    }

    /// Returns the source linear RGB multiplier.
    #[must_use]
    pub const fn color(&self) -> [f32; 3] {
        self.color
    }

    /// Returns source illuminance in lux.
    #[must_use]
    pub const fn illuminance_lux(&self) -> f32 {
        self.illuminance_lux
    }
}

/// Stable result of one [`spawn_scene`] call.
#[derive(Clone, Debug, PartialEq)]
pub struct SpawnedScene {
    model: ModelHandle,
    roots: Vec<Entity>,
    entities: Vec<(NodeIndex, Entity)>,
}

impl SpawnedScene {
    /// Returns the inserted model asset handle shared by mesh nodes.
    #[must_use]
    pub const fn model(&self) -> ModelHandle {
        self.model
    }

    /// Returns selected root entities in source order.
    #[must_use]
    pub fn roots(&self) -> &[Entity] {
        &self.roots
    }

    /// Resolves one selected source node to its stable spawned entity.
    #[must_use]
    pub fn entity(&self, source: NodeIndex) -> Option<Entity> {
        self.entities
            .iter()
            .find_map(|(candidate, entity)| (*candidate == source).then_some(*entity))
    }

    /// Returns source-node/entity mappings in ascending source node order.
    pub fn entities(&self) -> impl Iterator<Item = (NodeIndex, Entity)> + '_ {
        self.entities.iter().copied()
    }
}

fn select_scene(
    scene: &ImportedScene,
    selection: SceneSelection,
) -> Result<&yuyib_gltf::ImportedRootScene, SceneSpawnError> {
    let index = match selection {
        SceneSelection::Default => scene
            .default_scene()
            .ok_or(SceneSpawnError::NoDefaultScene)?,
        SceneSelection::Index(index) => index,
    };
    scene
        .scenes()
        .get(index.get())
        .ok_or(SceneSpawnError::UnknownScene { scene: index })
}

fn collect_selected_nodes(
    scene: &ImportedScene,
    source_scene: &yuyib_gltf::ImportedRootScene,
) -> Result<Vec<(NodeIndex, Option<NodeIndex>)>, SceneSpawnError> {
    let mut selected = Vec::new();
    let mut parents = HashMap::<NodeIndex, Option<NodeIndex>>::new();
    let mut stack: Vec<(NodeIndex, Option<NodeIndex>)> = source_scene
        .roots()
        .iter()
        .rev()
        .map(|root| (*root, None))
        .collect();
    while let Some((node, parent)) = stack.pop() {
        if parents.insert(node, parent).is_some() {
            return Err(SceneSpawnError::SharedNode { node });
        }
        let source = get_node(scene, node)?;
        selected.push((node, parent));
        stack.extend(
            source
                .children()
                .iter()
                .rev()
                .map(|child| (*child, Some(node))),
        );
    }
    Ok(selected)
}

fn get_node(
    scene: &ImportedScene,
    node: NodeIndex,
) -> Result<&yuyib_gltf::ImportedNode, SceneSpawnError> {
    scene
        .nodes()
        .get(node.get())
        .ok_or(SceneSpawnError::InvalidNodeReference { node })
}

fn entity_for(
    entities: &HashMap<NodeIndex, Entity>,
    node: NodeIndex,
) -> Result<Entity, SceneSpawnError> {
    entities
        .get(&node)
        .copied()
        .ok_or(SceneSpawnError::InvalidNodeReference { node })
}

fn sorted_entities(entities: HashMap<NodeIndex, Entity>) -> Vec<(NodeIndex, Entity)> {
    let mut sorted: Vec<_> = entities.into_iter().collect();
    sorted.sort_by_key(|(node, _)| node.get());
    sorted
}

/// A selected imported scene could not be represented unambiguously in ECS.
#[derive(Debug)]
pub enum SceneSpawnError {
    /// The caller selected Default but the asset did not declare one.
    NoDefaultScene,
    /// The requested source scene does not exist.
    UnknownScene {
        /// Requested source scene index.
        scene: SceneIndex,
    },
    /// A root or child node index was absent from the imported node table.
    InvalidNodeReference {
        /// Invalid source node index.
        node: NodeIndex,
    },
    /// One selected source node appeared through more than one parent path.
    SharedNode {
        /// Ambiguous source node.
        node: NodeIndex,
    },
    /// A node referenced an absent imported camera.
    InvalidCameraReference {
        /// Node containing the invalid reference.
        node: NodeIndex,
        /// Absent camera index.
        camera: CameraIndex,
    },
    /// A node referenced an absent imported directional light.
    InvalidLightReference {
        /// Node containing the invalid reference.
        node: NodeIndex,
        /// Absent directional-light index.
        light: DirectionalLightIndex,
    },
    /// The shared 3D hierarchy system rejected the spawned graph.
    Hierarchy(TransformHierarchyError),
}

impl fmt::Display for SceneSpawnError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDefaultScene => formatter.write_str("imported asset has no default scene"),
            Self::UnknownScene { scene } => {
                write!(formatter, "source scene {} does not exist", scene.get())
            }
            Self::InvalidNodeReference { node } => {
                write!(formatter, "source node {} does not exist", node.get())
            }
            Self::SharedNode { node } => write!(
                formatter,
                "source node {} is reachable through multiple selected parents",
                node.get()
            ),
            Self::InvalidCameraReference { node, camera } => write!(
                formatter,
                "source node {} references absent camera {}",
                node.get(),
                camera.get()
            ),
            Self::InvalidLightReference { node, light } => write!(
                formatter,
                "source node {} references absent directional light {}",
                node.get(),
                light.get()
            ),
            Self::Hierarchy(source) => {
                write!(formatter, "3D hierarchy propagation failed: {source}")
            }
        }
    }
}

impl Error for SceneSpawnError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Hierarchy(source) => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "Fixture values validate exact source metadata copying."
)]
mod tests {
    use base64::Engine as _;
    use yuyib_gltf::{ImportOptions, import_scene_bytes_with_base_path};

    use super::*;

    fn fixture() -> ImportedAsset {
        let mut binary = Vec::new();
        binary.extend([0_u16, 1, 2].into_iter().flat_map(u16::to_le_bytes));
        binary.extend([0_u8; 2]);
        for position in [[0.0_f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
            binary.extend(position.into_iter().flat_map(f32::to_le_bytes));
        }
        let encoded = base64::engine::general_purpose::STANDARD.encode(binary);
        let json = [
            r#"{"asset":{"version":"2.0"},"buffers":[{"uri":"data:application/octet-stream;base64,"#,
            &encoded,
            r#"","byteLength":44}],"bufferViews":[{"buffer":0,"byteOffset":0,"byteLength":6},{"buffer":0,"byteOffset":8,"byteLength":36}],"accessors":[{"bufferView":0,"componentType":5123,"count":3,"type":"SCALAR"},{"bufferView":1,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}],"meshes":[{"primitives":[{"attributes":{"POSITION":1},"indices":0}]}],"cameras":[{"type":"perspective","perspective":{"yfov":1,"znear":0.1}}],"nodes":[{"mesh":0,"children":[1],"translation":[1,2,3]},{"camera":0,"scale":[2,2,2]}],"scenes":[{"nodes":[0]}],"scene":0}"#,
        ]
        .concat();
        import_scene_bytes_with_base_path(json.as_bytes(), ".", ImportOptions::default())
            .expect("valid fixture")
    }

    fn matrix_fixture() -> ImportedAsset {
        let mut binary = Vec::new();
        binary.extend([0_u16, 1, 2].into_iter().flat_map(u16::to_le_bytes));
        binary.extend([0_u8; 2]);
        for position in [[0.0_f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
            binary.extend(position.into_iter().flat_map(f32::to_le_bytes));
        }
        let encoded = base64::engine::general_purpose::STANDARD.encode(binary);
        let json = [
            r#"{"asset":{"version":"2.0"},"buffers":[{"uri":"data:application/octet-stream;base64,"#,
            &encoded,
            r#"","byteLength":44}],"bufferViews":[{"buffer":0,"byteOffset":0,"byteLength":6},{"buffer":0,"byteOffset":8,"byteLength":36}],"accessors":[{"bufferView":0,"componentType":5123,"count":3,"type":"SCALAR"},{"bufferView":1,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}],"meshes":[{"primitives":[{"attributes":{"POSITION":1},"indices":0}]}],"nodes":[{"mesh":0,"matrix":[1,0,0,0,0.5,1,0,0,0,0,1,0,2,3,4,1]}],"scenes":[{"nodes":[0]}],"scene":0}"#,
        ]
        .concat();
        import_scene_bytes_with_base_path(json.as_bytes(), ".", ImportOptions::default())
            .expect("valid affine matrix fixture")
    }

    #[test]
    fn spawns_local_hierarchy_with_stable_source_mapping() {
        let asset = fixture();
        let mut world = World::new();
        let mut models = Assets::new();
        let spawned = spawn_scene(&mut world, &mut models, &asset, SceneSelection::Default)
            .expect("default scene is valid");
        let parent = spawned.entity(NodeIndex::new(0)).expect("parent spawned");
        let child = spawned.entity(NodeIndex::new(1)).expect("child spawned");
        assert_eq!(spawned.roots(), [parent]);
        assert_eq!(
            world
                .get::<LocalTransform3d>(parent)
                .expect("parent transform")
                .translation,
            [1.0, 2.0, 3.0]
        );
        assert_eq!(
            world
                .get::<yuyib_game_3d::Parent3d>(child)
                .expect("parent link")
                .entity(),
            parent
        );
        assert!(world.get::<Model3d>(parent).is_some());
        assert!(world.get::<Model3d>(child).is_none());
        assert_eq!(
            world.get::<SceneCamera>(child).expect("camera").source(),
            CameraIndex::new(0)
        );
        assert!(models.get(spawned.model()).is_some());
    }

    #[test]
    fn explicit_invalid_scene_selection_is_rejected() {
        let asset = fixture();
        let error = spawn_scene(
            &mut World::new(),
            &mut Assets::new(),
            &asset,
            SceneSelection::Index(SceneIndex::new(9)),
        )
        .expect_err("unknown source scene");
        assert!(matches!(error, SceneSpawnError::UnknownScene { .. }));
    }

    #[test]
    fn spawns_matrix_nodes_without_lossy_trs_decomposition() {
        let asset = matrix_fixture();
        let mut world = World::new();
        let mut models = Assets::new();
        let spawned = spawn_scene(&mut world, &mut models, &asset, SceneSelection::Default)
            .expect("matrix scene is representable");
        let entity = spawned
            .entity(NodeIndex::new(0))
            .expect("matrix node spawned");
        let matrix = world
            .get::<LocalMatrixTransform3d>(entity)
            .expect("exact local matrix component");
        assert_eq!(matrix.column_major()[4], 0.5);
        let world_transform = world
            .get::<yuyib_game_3d::WorldTransform3d>(entity)
            .expect("exact propagated matrix");
        assert_eq!(world_transform.column_major(), matrix.column_major());
        assert!(
            world_transform.as_trs().is_none(),
            "shear is not approximated"
        );
        assert!(world.get::<LocalTransform3d>(entity).is_none());
    }
}
