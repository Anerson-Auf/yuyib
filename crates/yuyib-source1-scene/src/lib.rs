//! Renderer-neutral Source 1 VMF entity metadata adapter for Yuyib ECS.
//!
//! [`spawn_entities`] converts selected [`yuyib_vmf::VmfMap`] world/entity
//! blocks into ECS metadata components. It preserves classname, Hammer editor
//! ID and ordered `KeyValues` while treating transformation as explicit imported
//! data rather than gameplay behavior. Valid VMF `origin` values produce a
//! [`yuyib_game_3d::LocalTransform3d`] and are then resolved through Yuyib's
//! existing hierarchy propagation.
//!
//! Source coordinates are converted as **`[x, y, z] -> [x, z, -y]`**, matching
//! the Source 1 brush compiler and Yuyib's right-handed `Y`-up 3D convention.
//! This crate does not parse Source 2, generate brush meshes, spawn props,
//! interpret outputs, select gameplay components, resolve models/materials, or
//! apply any Hammer/Source-specific gameplay magic.

#![forbid(unsafe_code)]

use std::{error::Error, fmt};

use yuyib_ecs::{bevy_ecs::entity::Entity, prelude::*};
use yuyib_game_3d::{LocalTransform3d, TransformHierarchyError, propagate_world_transforms};
use yuyib_vmf::{VmfEntity, VmfMap};

/// Source location of one spawned VMF entity metadata component.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Source1EntityLocation {
    /// The optional top-level VMF `world` block.
    World,
    /// One top-level VMF `entity` block in document order.
    Entity {
        /// Zero-based index in [`VmfMap::entities`].
        index: usize,
    },
}

/// One ordered VMF `KeyValues` entry retained in [`Source1Entity`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Source1KeyValue {
    key: String,
    value: String,
}

impl Source1KeyValue {
    /// Returns the decoded VMF property key.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Returns the decoded VMF property value.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }
}

/// ECS metadata copied from one Source 1 VMF world/entity block.
///
/// No property is interpreted beyond optional `classname`, `id` and `origin`.
/// Repeated VMF keys remain in source order inside [`Self::key_values`], so a
/// game-specific adapter can implement its own semantics without losing source
/// fidelity.
#[derive(Component, Clone, Debug, Eq, PartialEq)]
pub struct Source1Entity {
    location: Source1EntityLocation,
    classname: Option<String>,
    hammer_id: Option<String>,
    key_values: Vec<Source1KeyValue>,
}

impl Source1Entity {
    /// Returns whether this metadata came from the VMF world or an entity index.
    #[must_use]
    pub const fn location(&self) -> Source1EntityLocation {
        self.location
    }

    /// Returns the first VMF `classname`, if present.
    #[must_use]
    pub fn classname(&self) -> Option<&str> {
        self.classname.as_deref()
    }

    /// Returns the first VMF Hammer editor `id`, if present.
    #[must_use]
    pub fn hammer_id(&self) -> Option<&str> {
        self.hammer_id.as_deref()
    }

    /// Returns all ordered VMF properties, including repeated/unknown keys.
    #[must_use]
    pub fn key_values(&self) -> &[Source1KeyValue] {
        &self.key_values
    }

    /// Returns the first value for `key` in source order.
    ///
    /// VMF permits repeated keys; inspect [`Self::key_values`] when every value
    /// is meaningful to the consuming game.
    #[must_use]
    pub fn property(&self, key: &str) -> Option<&str> {
        self.key_values
            .iter()
            .find(|property| property.key == key)
            .map(Source1KeyValue::value)
    }
}

/// Selects which VMF blocks [`spawn_entities`] converts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Source1EntitySelection {
    /// Convert only the optional VMF `world` block.
    WorldOnly,
    /// Convert only top-level VMF entity blocks.
    EntitiesOnly,
    /// Convert the world first, then entities in document order.
    WorldAndEntities,
}

/// Policy for absent or invalid VMF `origin` properties.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Source1OriginPolicy {
    /// Abort before any ECS entity is spawned and report the source error.
    RequireValid,
    /// Spawn metadata only and omit local/world transforms for that source item.
    MetadataOnly,
}

/// Bounded conversion configuration for [`spawn_entities`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Source1SpawnOptions {
    /// VMF source blocks selected for conversion.
    pub selection: Source1EntitySelection,
    /// Maximum selected world/entity blocks.
    pub max_entities: usize,
    /// Whether missing/invalid `origin` is fatal or metadata-only.
    pub origin_policy: Source1OriginPolicy,
    /// Maximum bytes accepted in one `origin` value before numeric parsing.
    pub max_origin_bytes: usize,
}

impl Default for Source1SpawnOptions {
    fn default() -> Self {
        Self {
            selection: Source1EntitySelection::WorldAndEntities,
            max_entities: 100_000,
            origin_policy: Source1OriginPolicy::MetadataOnly,
            max_origin_bytes: 256,
        }
    }
}

/// A spawned ECS entity together with its VMF source location.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SpawnedSource1Entity {
    /// ECS entity created by [`spawn_entities`].
    pub entity: Entity,
    /// Original VMF source location.
    pub location: Source1EntityLocation,
    /// Whether a valid `origin` produced local/world transform components.
    pub has_transform: bool,
}

/// Result of successful bounded VMF entity conversion.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Source1SpawnReport {
    spawned: Vec<SpawnedSource1Entity>,
}

impl Source1SpawnReport {
    /// Returns spawned entities in deterministic selected-source order.
    #[must_use]
    pub fn spawned(&self) -> &[SpawnedSource1Entity] {
        &self.spawned
    }

    /// Returns how many source blocks were converted.
    #[must_use]
    pub fn len(&self) -> usize {
        self.spawned.len()
    }

    /// Returns whether selection contained no source blocks.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.spawned.is_empty()
    }
}

/// Failure while preparing or spawning Source 1 VMF entity metadata.
#[derive(Debug)]
pub enum Source1SpawnError {
    /// Selected VMF blocks exceed the configured bounded-work limit.
    TooManyEntities {
        /// Selected source count.
        actual: usize,
        /// Configured maximum.
        limit: usize,
    },
    /// A strict-origin conversion found no `origin` property.
    MissingOrigin {
        /// Source block missing `origin`.
        location: Source1EntityLocation,
    },
    /// An origin string exceeded its configured bounded parser size.
    OriginTooLong {
        /// Source block holding the value.
        location: Source1EntityLocation,
        /// Observed byte length.
        actual: usize,
        /// Configured maximum.
        limit: usize,
    },
    /// An origin string was not exactly three finite decimal coordinates.
    InvalidOrigin {
        /// Source block holding the malformed value.
        location: Source1EntityLocation,
    },
    /// Yuyib hierarchy propagation rejected a generated local transform.
    Hierarchy(TransformHierarchyError),
}

impl fmt::Display for Source1SpawnError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyEntities { actual, limit } => {
                write!(
                    formatter,
                    "selected Source 1 entity count {actual} exceeds limit {limit}"
                )
            }
            Self::MissingOrigin { location } => {
                write!(formatter, "Source 1 {location:?} has no origin property")
            }
            Self::OriginTooLong {
                location,
                actual,
                limit,
            } => write!(
                formatter,
                "Source 1 {location:?} origin is {actual} bytes; limit is {limit}"
            ),
            Self::InvalidOrigin { location } => {
                write!(
                    formatter,
                    "Source 1 {location:?} origin is not three finite numbers"
                )
            }
            Self::Hierarchy(source) => {
                write!(formatter, "Source 1 transform propagation failed: {source}")
            }
        }
    }
}

impl Error for Source1SpawnError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Hierarchy(source) => Some(source),
            Self::TooManyEntities { .. }
            | Self::MissingOrigin { .. }
            | Self::OriginTooLong { .. }
            | Self::InvalidOrigin { .. } => None,
        }
    }
}

struct PreparedEntity {
    metadata: Source1Entity,
    transform: Option<LocalTransform3d>,
}

/// Converts selected VMF metadata into ECS entities under explicit origin policy.
///
/// Input is fully prepared and validated before the first ECS entity is spawned.
/// Consequently [`Source1OriginPolicy::RequireValid`] does not create a partial
/// set when it encounters malformed source metadata. Under
/// [`Source1OriginPolicy::MetadataOnly`], missing/invalid origins simply omit
/// [`LocalTransform3d`], [`yuyib_game_3d::WorldTransform3d`] and legacy render
/// transform output for that source item.
///
/// Valid-origin entities are roots and call [`propagate_world_transforms`] only
/// after all selected metadata has been created. The resulting world/legacy
/// transforms therefore use the existing shared Yuyib hierarchy implementation.
///
/// # Errors
///
/// Returns [`Source1SpawnError`] for bounded selection/origin validation or
/// hierarchy propagation failure.
pub fn spawn_entities(
    world: &mut World,
    map: &VmfMap,
    options: Source1SpawnOptions,
) -> Result<Source1SpawnReport, Source1SpawnError> {
    let selected = select_entities(map, options.selection);
    if selected.len() > options.max_entities {
        return Err(Source1SpawnError::TooManyEntities {
            actual: selected.len(),
            limit: options.max_entities,
        });
    }
    let prepared = selected
        .iter()
        .map(|(location, entity)| prepare_entity(*location, entity, options))
        .collect::<Result<Vec<_>, _>>()?;

    let mut spawned = Vec::with_capacity(prepared.len());
    for ((location, _), prepared) in selected.into_iter().zip(prepared) {
        let has_transform = prepared.transform.is_some();
        let entity = if let Some(transform) = prepared.transform {
            world.spawn((prepared.metadata, transform)).id()
        } else {
            world.spawn(prepared.metadata).id()
        };
        spawned.push(SpawnedSource1Entity {
            entity,
            location,
            has_transform,
        });
    }
    if spawned.iter().any(|entry| entry.has_transform) {
        propagate_world_transforms(world).map_err(Source1SpawnError::Hierarchy)?;
    }
    Ok(Source1SpawnReport { spawned })
}

fn select_entities(
    map: &VmfMap,
    selection: Source1EntitySelection,
) -> Vec<(Source1EntityLocation, &VmfEntity)> {
    let mut selected = Vec::new();
    if matches!(
        selection,
        Source1EntitySelection::WorldOnly | Source1EntitySelection::WorldAndEntities
    ) && let Some(world) = map.world()
    {
        selected.push((Source1EntityLocation::World, world));
    }
    if matches!(
        selection,
        Source1EntitySelection::EntitiesOnly | Source1EntitySelection::WorldAndEntities
    ) {
        selected.extend(
            map.entities()
                .iter()
                .enumerate()
                .map(|(index, entity)| (Source1EntityLocation::Entity { index }, entity)),
        );
    }
    selected
}

fn prepare_entity(
    location: Source1EntityLocation,
    entity: &VmfEntity,
    options: Source1SpawnOptions,
) -> Result<PreparedEntity, Source1SpawnError> {
    let metadata = Source1Entity {
        location,
        classname: entity.classname().map(str::to_owned),
        hammer_id: entity.id().map(str::to_owned),
        key_values: entity
            .block()
            .properties()
            .iter()
            .map(|property| Source1KeyValue {
                key: property.key().to_owned(),
                value: property.value().to_owned(),
            })
            .collect(),
    };
    let transform = match entity.origin() {
        Some(origin) => match parse_origin(origin, location, options.max_origin_bytes) {
            Ok(position) => Some(LocalTransform3d::from_translation(position)),
            Err(_) if options.origin_policy == Source1OriginPolicy::MetadataOnly => None,
            Err(error) => return Err(error),
        },
        None if options.origin_policy == Source1OriginPolicy::MetadataOnly => None,
        None => return Err(Source1SpawnError::MissingOrigin { location }),
    };
    Ok(PreparedEntity {
        metadata,
        transform,
    })
}

fn parse_origin(
    source: &str,
    location: Source1EntityLocation,
    max_bytes: usize,
) -> Result<[f32; 3], Source1SpawnError> {
    if source.len() > max_bytes {
        return Err(Source1SpawnError::OriginTooLong {
            location,
            actual: source.len(),
            limit: max_bytes,
        });
    }
    let mut values = source.split_ascii_whitespace();
    let Some(x) = values.next().and_then(|value| value.parse::<f32>().ok()) else {
        return Err(Source1SpawnError::InvalidOrigin { location });
    };
    let Some(y) = values.next().and_then(|value| value.parse::<f32>().ok()) else {
        return Err(Source1SpawnError::InvalidOrigin { location });
    };
    let Some(z) = values.next().and_then(|value| value.parse::<f32>().ok()) else {
        return Err(Source1SpawnError::InvalidOrigin { location });
    };
    if values.next().is_some() || !x.is_finite() || !y.is_finite() || !z.is_finite() {
        return Err(Source1SpawnError::InvalidOrigin { location });
    }
    Ok([x, z, -y])
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // Test origins use exactly representable coordinates.
mod tests {
    use yuyib_game_3d::WorldTransform3d;
    use yuyib_vmf::parse;

    use super::*;

    const MAP: &str = r#"
world
{
    "id" "1"
    "classname" "worldspawn"
}
entity
{
    "id" "10"
    "classname" "info_target"
    "origin" "1 2 3"
    "message" "first"
    "message" "second"
}
entity
{
    "id" "11"
    "classname" "logic_relay"
}
"#;

    #[test]
    fn spawning_preserves_ordered_metadata_and_converts_origin() {
        let map = parse(MAP).expect("valid VMF");
        let mut world = World::new();
        let report = spawn_entities(&mut world, &map, Source1SpawnOptions::default())
            .expect("metadata-only missing origin is valid");
        assert_eq!(report.len(), 3);
        let target = report.spawned()[1];
        assert!(target.has_transform);
        let metadata = world
            .get::<Source1Entity>(target.entity)
            .expect("metadata component");
        assert_eq!(metadata.classname(), Some("info_target"));
        assert_eq!(metadata.hammer_id(), Some("10"));
        assert_eq!(
            metadata
                .key_values()
                .iter()
                .filter(|property| property.key() == "message")
                .map(Source1KeyValue::value)
                .collect::<Vec<_>>(),
            vec!["first", "second"]
        );
        assert_eq!(
            world
                .get::<WorldTransform3d>(target.entity)
                .expect("propagated transform")
                .translation(),
            [1.0, 3.0, -2.0]
        );
        assert!(!report.spawned()[0].has_transform);
    }

    #[test]
    fn strict_origin_policy_is_transactional() {
        let map = parse(MAP).expect("valid VMF");
        let mut world = World::new();
        let options = Source1SpawnOptions {
            origin_policy: Source1OriginPolicy::RequireValid,
            ..Source1SpawnOptions::default()
        };
        assert!(matches!(
            spawn_entities(&mut world, &map, options),
            Err(Source1SpawnError::MissingOrigin {
                location: Source1EntityLocation::World
            })
        ));
        assert_eq!(world.query::<&Source1Entity>().iter(&world).count(), 0);
    }

    #[test]
    fn malformed_origin_has_explicit_error_or_metadata_only_opt_out() {
        let map = parse(
            r#"entity
{
    "classname" "broken"
    "origin" "1 nope 3"
}"#,
        )
        .expect("syntax is valid");
        let mut strict_world = World::new();
        let strict = Source1SpawnOptions {
            selection: Source1EntitySelection::EntitiesOnly,
            origin_policy: Source1OriginPolicy::RequireValid,
            ..Source1SpawnOptions::default()
        };
        assert!(matches!(
            spawn_entities(&mut strict_world, &map, strict),
            Err(Source1SpawnError::InvalidOrigin { .. })
        ));
        let mut metadata_world = World::new();
        let report = spawn_entities(
            &mut metadata_world,
            &map,
            Source1SpawnOptions {
                selection: Source1EntitySelection::EntitiesOnly,
                ..Source1SpawnOptions::default()
            },
        )
        .expect("metadata-only opt-out");
        assert!(!report.spawned()[0].has_transform);
    }
}
