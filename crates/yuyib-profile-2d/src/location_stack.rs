//! Location stack for top-down map swaps (house interiors / sublocales).

use std::{error::Error, fmt};

use yuyib_ecs::prelude::{Entity, World};

/// Portal action when the player interacts while overlapping the trigger.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocationPortalAction2d {
    /// Enter a named location (host rebuilds contents).
    Enter(String),
    /// Pop back to the previous location.
    Exit,
}

/// Axis-aligned interact trigger in world space.
#[derive(Clone, Debug, PartialEq)]
pub struct LocationPortal2d {
    /// Inclusive-ish AABB minimum (world units).
    pub min: [f32; 2],
    /// Exclusive-ish AABB maximum (world units).
    pub max: [f32; 2],
    /// What happens on interact.
    pub action: LocationPortalAction2d,
}

impl LocationPortal2d {
    /// Creates a portal from centre + size (both finite, size non-negative).
    #[must_use]
    pub fn from_center_size(
        center: [f32; 2],
        size: [f32; 2],
        action: LocationPortalAction2d,
    ) -> Self {
        let half = [size[0].abs() * 0.5, size[1].abs() * 0.5];
        Self {
            min: [center[0] - half[0], center[1] - half[1]],
            max: [center[0] + half[0], center[1] + half[1]],
            action,
        }
    }

    /// Returns whether `point` lies inside the portal rectangle.
    #[must_use]
    pub fn contains_point(&self, point: [f32; 2]) -> bool {
        point[0] >= self.min[0]
            && point[0] <= self.max[0]
            && point[1] >= self.min[1]
            && point[1] <= self.max[1]
    }

    /// Returns whether a centre+half-extents AABB overlaps this portal.
    #[must_use]
    pub fn overlaps_aabb(&self, center: [f32; 2], half_extents: [f32; 2]) -> bool {
        let min = [center[0] - half_extents[0], center[1] - half_extents[1]];
        let max = [center[0] + half_extents[0], center[1] + half_extents[1]];
        min[0] <= self.max[0]
            && max[0] >= self.min[0]
            && min[1] <= self.max[1]
            && max[1] >= self.min[1]
    }
}

/// One active location membership + portals.
#[derive(Clone, Debug, PartialEq)]
pub struct LocationFrame2d {
    /// Host-defined location id (`"outdoor"`, `"house_interior"`, …).
    pub id: String,
    /// Entities that belong to this location (map + props). Player excluded.
    pub entities: Vec<Entity>,
    /// Interact triggers for this location.
    pub portals: Vec<LocationPortal2d>,
    /// Preferred player spawn when entering this location.
    pub spawn: [f32; 2],
}

/// Push/pop location frames on a shared ECS world without restarting the app.
///
/// Suspended frames keep their entity ids; those entities are **despawned** on
/// push and the previous frame's id is stacked so the host can rebuild on pop.
/// This keeps GPU/ECS simple for the first slice (no hide-component protocol).
#[derive(Clone, Debug, PartialEq)]
pub struct LocationStack2d {
    current: LocationFrame2d,
    /// Suspended location ids (oldest at front). Host rebuilds via id on pop.
    suspended: Vec<String>,
}

impl LocationStack2d {
    /// Starts with one active location.
    #[must_use]
    pub fn new(current: LocationFrame2d) -> Self {
        Self {
            current,
            suspended: Vec::new(),
        }
    }

    /// Active location frame.
    #[must_use]
    pub const fn current(&self) -> &LocationFrame2d {
        &self.current
    }

    /// Suspended location ids, oldest first.
    #[must_use]
    pub fn suspended_ids(&self) -> &[String] {
        &self.suspended
    }

    /// Depth of suspended locations (0 = only current).
    #[must_use]
    pub fn depth(&self) -> usize {
        self.suspended.len()
    }

    /// First overlapping portal for the player's collision AABB, if any.
    #[must_use]
    pub fn overlapping_portal(
        &self,
        player_center: [f32; 2],
        player_half_extents: [f32; 2],
    ) -> Option<&LocationPortal2d> {
        self.current
            .portals
            .iter()
            .find(|portal| portal.overlaps_aabb(player_center, player_half_extents))
    }

    /// Despawns the current location entities and activates `next`.
    ///
    /// The previous location id is pushed onto the suspended stack so
    /// [`Self::pop`] can ask the host to rebuild it.
    pub fn push(&mut self, world: &mut World, next: LocationFrame2d) {
        despawn_entities(world, &self.current.entities);
        self.suspended.push(self.current.id.clone());
        self.current = next;
    }

    /// Despawns the current location and returns the id that should be rebuilt.
    ///
    /// After a successful pop the stack's `current` is a placeholder empty frame
    /// with that id — the host must immediately replace it via
    /// [`Self::replace_current`] after spawning.
    ///
    /// # Errors
    ///
    /// Returns [`LocationStackError2d::EmptyStack`] when nothing is suspended.
    pub fn pop(&mut self, world: &mut World) -> Result<String, LocationStackError2d> {
        let previous_id = self
            .suspended
            .pop()
            .ok_or(LocationStackError2d::EmptyStack)?;
        despawn_entities(world, &self.current.entities);
        self.current = LocationFrame2d {
            id: previous_id.clone(),
            entities: Vec::new(),
            portals: Vec::new(),
            spawn: [0.0, 0.0],
        };
        Ok(previous_id)
    }

    /// Replaces the active frame after the host finished spawning it.
    pub fn replace_current(&mut self, frame: LocationFrame2d) {
        self.current = frame;
    }
}

fn despawn_entities(world: &mut World, entities: &[Entity]) {
    for entity in entities {
        let _ = world.despawn(*entity);
    }
}

/// Failure while driving [`LocationStack2d`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocationStackError2d {
    /// [`LocationStack2d::pop`] with an empty suspended stack.
    EmptyStack,
}

impl fmt::Display for LocationStackError2d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyStack => formatter.write_str("location stack is empty"),
        }
    }
}

impl Error for LocationStackError2d {}

#[cfg(test)]
mod tests {
    use super::{
        LocationFrame2d, LocationPortal2d, LocationPortalAction2d, LocationStack2d,
        LocationStackError2d,
    };
    use yuyib_ecs::prelude::World;

    #[test]
    fn push_pop_restores_id_and_despawns() {
        let mut world = World::new();
        let outdoor_map = world.spawn(()).id();
        let mut stack = LocationStack2d::new(LocationFrame2d {
            id: "outdoor".into(),
            entities: vec![outdoor_map],
            portals: vec![LocationPortal2d::from_center_size(
                [10.0, 10.0],
                [4.0, 4.0],
                LocationPortalAction2d::Enter("house".into()),
            )],
            spawn: [8.0, 8.0],
        });
        let interior = world.spawn(()).id();
        stack.push(
            &mut world,
            LocationFrame2d {
                id: "house".into(),
                entities: vec![interior],
                portals: vec![LocationPortal2d::from_center_size(
                    [2.0, 2.0],
                    [2.0, 2.0],
                    LocationPortalAction2d::Exit,
                )],
                spawn: [2.0, 2.0],
            },
        );
        assert_eq!(stack.current().id, "house");
        assert_eq!(stack.suspended_ids(), &["outdoor".to_owned()]);
        assert!(world.get_entity(outdoor_map).is_err());
        let restored = stack.pop(&mut world).expect("pop");
        assert_eq!(restored, "outdoor");
        assert!(world.get_entity(interior).is_err());
        assert_eq!(
            stack.pop(&mut world),
            Err(LocationStackError2d::EmptyStack)
        );
    }

    #[test]
    fn portal_overlap_detects_player() {
        let portal = LocationPortal2d::from_center_size(
            [0.0, 0.0],
            [10.0, 10.0],
            LocationPortalAction2d::Exit,
        );
        assert!(portal.overlaps_aabb([0.0, 0.0], [1.0, 1.0]));
        assert!(!portal.overlaps_aabb([20.0, 20.0], [1.0, 1.0]));
    }
}
