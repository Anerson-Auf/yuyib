use std::{collections::BTreeSet, num::NonZeroU32};

use serde::{Deserialize, Serialize};

use crate::{ComponentSchemaId, PluginId, ScheduleId, SystemId};

/// Editor-only source location used for navigation, never stored in a scene.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceNavigation {
    /// Workspace-relative or absolute source file path.
    pub file: String,
    /// One-based source line, when known.
    pub line: Option<NonZeroU32>,
    /// One-based source column, when known.
    pub column: Option<NonZeroU32>,
}

impl SourceNavigation {
    /// Creates file-only source navigation metadata.
    #[must_use]
    pub fn file(file: impl Into<String>) -> Self {
        Self {
            file: file.into(),
            line: None,
            column: None,
        }
    }

    /// Adds a one-based line and optional one-based column.
    #[must_use]
    pub fn at(mut self, line: NonZeroU32, column: Option<NonZeroU32>) -> Self {
        self.line = Some(line);
        self.column = column;
        self
    }
}

/// Discoverability metadata for a global ECS system.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SystemDescriptor {
    id: SystemId,
    owner: PluginId,
    reads: BTreeSet<ComponentSchemaId>,
    writes: BTreeSet<ComponentSchemaId>,
    schedule: ScheduleId,
    source: Option<SourceNavigation>,
    documentation: Option<String>,
}

impl SystemDescriptor {
    /// Creates a system descriptor with no declared component access.
    #[must_use]
    pub fn new(id: SystemId, owner: PluginId, schedule: ScheduleId) -> Self {
        Self {
            id,
            owner,
            reads: BTreeSet::new(),
            writes: BTreeSet::new(),
            schedule,
            source: None,
            documentation: None,
        }
    }

    /// Declares a component read.
    #[must_use]
    pub fn reading(mut self, component: ComponentSchemaId) -> Self {
        if !self.writes.contains(&component) {
            self.reads.insert(component);
        }
        self
    }

    /// Declares a component write. A write supersedes a read declaration.
    #[must_use]
    pub fn writing(mut self, component: ComponentSchemaId) -> Self {
        self.reads.remove(&component);
        self.writes.insert(component);
        self
    }

    /// Adds editor-only source navigation metadata.
    #[must_use]
    pub fn with_source(mut self, source: SourceNavigation) -> Self {
        self.source = Some(source);
        self
    }

    /// Adds a documentation URI or project-relative path.
    #[must_use]
    pub fn with_documentation(mut self, documentation: impl Into<String>) -> Self {
        self.documentation = Some(documentation.into());
        self
    }

    /// Returns the stable system identifier.
    #[must_use]
    pub const fn id(&self) -> &SystemId {
        &self.id
    }

    /// Returns the owning plugin.
    #[must_use]
    pub const fn owner(&self) -> &PluginId {
        &self.owner
    }

    /// Returns component schemas read without declared mutation.
    #[must_use]
    pub const fn reads(&self) -> &BTreeSet<ComponentSchemaId> {
        &self.reads
    }

    /// Returns component schemas written by the system.
    #[must_use]
    pub const fn writes(&self) -> &BTreeSet<ComponentSchemaId> {
        &self.writes
    }

    /// Returns the schedule identifier.
    #[must_use]
    pub const fn schedule(&self) -> &ScheduleId {
        &self.schedule
    }

    /// Returns editor-only source navigation metadata.
    #[must_use]
    pub const fn source(&self) -> Option<&SourceNavigation> {
        self.source.as_ref()
    }

    /// Returns optional documentation.
    #[must_use]
    pub fn documentation(&self) -> Option<&str> {
        self.documentation.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use super::*;

    #[test]
    fn writes_supersede_reads_and_source_is_editor_metadata() {
        let transform = ComponentSchemaId::new("yuyib.transform3d").expect("id");
        let descriptor = SystemDescriptor::new(
            SystemId::new("yuyib.system.transform-propagation").expect("id"),
            PluginId::new("yuyib.game-3d").expect("id"),
            ScheduleId::new("yuyib.schedule.update").expect("id"),
        )
        .reading(transform.clone())
        .writing(transform.clone())
        .with_source(
            SourceNavigation::file("crates/yuyib-game-3d/src/transform.rs")
                .at(NonZeroU32::new(42).expect("non-zero"), None),
        );

        assert!(!descriptor.reads().contains(&transform));
        assert!(descriptor.writes().contains(&transform));
        assert_eq!(
            descriptor
                .source()
                .expect("source")
                .line
                .map(NonZeroU32::get),
            Some(42)
        );
    }
}
