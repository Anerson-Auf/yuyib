//! Declared render-pass ordering over a frame-local renderer boundary.

use std::{
    collections::HashSet,
    error::Error,
    fmt,
    sync::Arc,
    time::{Duration, Instant},
};

use crate::RenderFrame;

/// Standard high-level render phase ordering.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RenderPhase {
    /// Bounded publication of prepared GPU assets.
    AssetUpload,
    /// General compute work required by later phases.
    Compute,
    /// Shadow maps and other depth-only 3D preparation.
    Shadow3d,
    /// Opaque 3D geometry.
    Opaque3d,
    /// Sorted or weighted transparent 3D geometry.
    Transparent3d,
    /// Sprites, tilemaps, and other world-space 2D content.
    World2d,
    /// Post-processing over scene colour/depth.
    PostProcess,
    /// Native retained UI composition.
    NativeUi,
}

/// Stable graph-local resource name used for declared pass access.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RenderResourceId(Arc<str>);

impl RenderResourceId {
    /// Validates a portable non-empty graph resource name.
    ///
    /// Names accept ASCII alphanumerics plus `.`, `_`, `-`, `/`, and `:`.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty, oversized, or non-portable identifier.
    pub fn new(value: impl Into<Arc<str>>) -> Result<Self, RenderResourceIdError> {
        let value = value.into();
        if value.is_empty() {
            return Err(RenderResourceIdError::Empty);
        }
        if value.len() > 128 {
            return Err(RenderResourceIdError::TooLong);
        }
        if value.bytes().any(|byte| {
            !byte.is_ascii_alphanumeric() && !matches!(byte, b'.' | b'_' | b'-' | b'/' | b':')
        }) {
            return Err(RenderResourceIdError::InvalidCharacter);
        }
        Ok(Self(value))
    }

    /// Built-in presentation colour attachment.
    #[must_use]
    pub fn surface_color() -> Self {
        Self(Arc::from("frame:surface-color"))
    }

    /// Built-in frame-local depth attachment.
    #[must_use]
    pub fn depth() -> Self {
        Self(Arc::from("frame:depth"))
    }

    /// Returns the canonical graph resource name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Invalid graph resource identifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RenderResourceIdError {
    /// Empty names cannot produce useful diagnostics.
    Empty,
    /// Resource names are bounded to 128 bytes.
    TooLong,
    /// The name contains whitespace, control, Unicode, or unsupported syntax.
    InvalidCharacter,
}

impl fmt::Display for RenderResourceIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("render resource name is empty"),
            Self::TooLong => formatter.write_str("render resource name exceeds 128 bytes"),
            Self::InvalidCharacter => formatter.write_str("render resource name is not portable"),
        }
    }
}

impl Error for RenderResourceIdError {}

/// Stable identifier assigned to a registered pass.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RenderPassId(u32);

/// Declared pass metadata used for ordering and diagnostics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderPassDescriptor {
    /// Human-readable unique graph label.
    pub label: String,
    /// Standard phase used as the primary ordering boundary.
    pub phase: RenderPhase,
    /// Previously registered passes which must execute first.
    pub after: Vec<RenderPassId>,
    /// Resources sampled or otherwise read by this pass.
    pub reads: Vec<RenderResourceId>,
    /// Resources written by this pass. A resource may also appear in `reads`
    /// to declare read-modify-write behaviour.
    pub writes: Vec<RenderResourceId>,
}

impl RenderPassDescriptor {
    /// Creates a descriptor with no explicit dependencies or resource access.
    #[must_use]
    pub fn new(label: impl Into<String>, phase: RenderPhase) -> Self {
        Self {
            label: label.into(),
            phase,
            after: Vec::new(),
            reads: Vec::new(),
            writes: Vec::new(),
        }
    }

    /// Declares a previously registered pass dependency.
    #[must_use]
    pub fn after(mut self, pass: RenderPassId) -> Self {
        self.after.push(pass);
        self
    }

    /// Declares one read access.
    #[must_use]
    pub fn reads(mut self, resource: RenderResourceId) -> Self {
        self.reads.push(resource);
        self
    }

    /// Declares one write access.
    #[must_use]
    pub fn writes(mut self, resource: RenderResourceId) -> Self {
        self.writes.push(resource);
        self
    }
}

/// Type-erased structured failure returned by a graph pass.
pub type BoxedRenderPassError = Box<dyn Error + Send + Sync + 'static>;

type RenderPassCallback = Box<
    dyn for<'frame> FnMut(&mut RenderFrame<'frame>) -> Result<(), BoxedRenderPassError> + 'static,
>;

struct RegisteredPass {
    id: RenderPassId,
    registration_index: u32,
    descriptor: RenderPassDescriptor,
    callback: RenderPassCallback,
}

/// A validated, phase-ordered graph of frame-local rendering work.
#[derive(Default)]
pub struct RenderGraph {
    passes: Vec<RegisteredPass>,
}

impl RenderGraph {
    /// Creates an empty graph.
    #[must_use]
    pub const fn new() -> Self {
        Self { passes: Vec::new() }
    }

    /// Registers a fallible pass with declared dependencies and resources.
    ///
    /// # Errors
    ///
    /// Returns an error for duplicate/empty labels, unknown dependencies,
    /// dependency phase inversion, duplicate resources, or pass ID exhaustion.
    pub fn add_pass<PassError>(
        &mut self,
        descriptor: RenderPassDescriptor,
        mut callback: impl for<'frame> FnMut(&mut RenderFrame<'frame>) -> Result<(), PassError>
        + 'static,
    ) -> Result<RenderPassId, RenderGraphBuildError>
    where
        PassError: Error + Send + Sync + 'static,
    {
        self.validate_descriptor(&descriptor)?;
        let registration_index = u32::try_from(self.passes.len())
            .map_err(|_| RenderGraphBuildError::PassLimitExceeded)?;
        let id = RenderPassId(registration_index);
        self.passes.push(RegisteredPass {
            id,
            registration_index,
            descriptor,
            callback: Box::new(move |frame| {
                callback(frame).map_err(|error| Box::new(error) as BoxedRenderPassError)
            }),
        });
        self.sort_passes();
        Ok(id)
    }

    /// Registers an infallible pass.
    ///
    /// # Errors
    ///
    /// Returns the same descriptor validation errors as [`Self::add_pass`].
    pub fn add_infallible_pass(
        &mut self,
        descriptor: RenderPassDescriptor,
        mut callback: impl for<'frame> FnMut(&mut RenderFrame<'frame>) + 'static,
    ) -> Result<RenderPassId, RenderGraphBuildError> {
        self.add_pass(descriptor, move |frame| {
            callback(frame);
            Ok::<(), InfallibleRenderPassError>(())
        })
    }

    /// Executes every pass and records CPU submission time per pass.
    ///
    /// Execution stops at the first pass failure; the renderer still owns frame
    /// submission/presentation and decides whether that frame is discarded.
    ///
    /// # Errors
    ///
    /// Returns the failing pass identity, phase, label, and original source.
    pub fn execute(
        &mut self,
        frame: &mut RenderFrame<'_>,
    ) -> Result<RenderGraphExecution, RenderGraphExecutionError> {
        let mut timings = Vec::with_capacity(self.passes.len());
        for pass in &mut self.passes {
            let started = Instant::now();
            if let Err(source) = (pass.callback)(frame) {
                return Err(RenderGraphExecutionError {
                    pass: pass.id,
                    label: pass.descriptor.label.clone(),
                    phase: pass.descriptor.phase,
                    source,
                });
            }
            timings.push(RenderPassTiming {
                pass: pass.id,
                label: pass.descriptor.label.clone(),
                phase: pass.descriptor.phase,
                cpu_duration: started.elapsed(),
            });
        }
        Ok(RenderGraphExecution { timings })
    }

    /// Returns descriptors in their compiled execution order.
    #[must_use]
    pub fn passes(&self) -> impl ExactSizeIterator<Item = (RenderPassId, &RenderPassDescriptor)> {
        self.passes.iter().map(|pass| (pass.id, &pass.descriptor))
    }

    fn validate_descriptor(
        &self,
        descriptor: &RenderPassDescriptor,
    ) -> Result<(), RenderGraphBuildError> {
        if descriptor.label.trim().is_empty() {
            return Err(RenderGraphBuildError::EmptyLabel);
        }
        if self
            .passes
            .iter()
            .any(|pass| pass.descriptor.label == descriptor.label)
        {
            return Err(RenderGraphBuildError::DuplicateLabel);
        }
        ensure_unique_resources(&descriptor.reads)?;
        ensure_unique_resources(&descriptor.writes)?;
        let mut dependencies = HashSet::new();
        for dependency in &descriptor.after {
            if !dependencies.insert(*dependency) {
                return Err(RenderGraphBuildError::DuplicateDependency);
            }
            let dependency_phase = self
                .passes
                .iter()
                .find(|pass| pass.id == *dependency)
                .map(|pass| pass.descriptor.phase)
                .ok_or(RenderGraphBuildError::UnknownDependency)?;
            if dependency_phase > descriptor.phase {
                return Err(RenderGraphBuildError::DependencyPhaseInversion);
            }
        }
        Ok(())
    }

    fn sort_passes(&mut self) {
        self.passes
            .sort_by_key(|pass| (pass.descriptor.phase, pass.registration_index));
    }
}

fn ensure_unique_resources(resources: &[RenderResourceId]) -> Result<(), RenderGraphBuildError> {
    let mut unique = HashSet::new();
    if resources.iter().any(|resource| !unique.insert(resource)) {
        return Err(RenderGraphBuildError::DuplicateResourceAccess);
    }
    Ok(())
}

/// Invalid graph registration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RenderGraphBuildError {
    /// A pass label contains only whitespace.
    EmptyLabel,
    /// Pass labels must be unique within a graph.
    DuplicateLabel,
    /// A dependency does not refer to a registered pass.
    UnknownDependency,
    /// A dependency is in a later standard phase.
    DependencyPhaseInversion,
    /// The descriptor repeats the same dependency.
    DuplicateDependency,
    /// The same resource is repeated within reads or writes.
    DuplicateResourceAccess,
    /// More than `u32::MAX` passes were requested.
    PassLimitExceeded,
}

impl fmt::Display for RenderGraphBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyLabel => formatter.write_str("render pass label is empty"),
            Self::DuplicateLabel => formatter.write_str("render pass label is already registered"),
            Self::UnknownDependency => formatter.write_str("render pass dependency is unknown"),
            Self::DependencyPhaseInversion => {
                formatter.write_str("render pass depends on a later standard phase")
            }
            Self::DuplicateDependency => {
                formatter.write_str("render pass repeats the same dependency")
            }
            Self::DuplicateResourceAccess => {
                formatter.write_str("render pass repeats the same resource access")
            }
            Self::PassLimitExceeded => formatter.write_str("render graph pass limit exceeded"),
        }
    }
}

impl Error for RenderGraphBuildError {}

/// CPU-side timing for one successfully submitted pass.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderPassTiming {
    /// Stable pass identity.
    pub pass: RenderPassId,
    /// Diagnostic pass label.
    pub label: String,
    /// Standard phase.
    pub phase: RenderPhase,
    /// CPU time spent recording this pass. This is not GPU execution time.
    pub cpu_duration: Duration,
}

/// Successful execution diagnostics for one frame.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RenderGraphExecution {
    /// Pass timings in actual execution order.
    pub timings: Vec<RenderPassTiming>,
}

/// A graph pass failed while recording the current frame.
#[derive(Debug)]
pub struct RenderGraphExecutionError {
    /// Stable failing pass identity.
    pub pass: RenderPassId,
    /// Diagnostic pass label.
    pub label: String,
    /// Standard phase of the failing pass.
    pub phase: RenderPhase,
    source: BoxedRenderPassError,
}

impl fmt::Display for RenderGraphExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "render pass '{}' in {:?} failed: {}",
            self.label, self.phase, self.source
        )
    }
}

impl Error for RenderGraphExecutionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
}

#[derive(Debug)]
struct InfallibleRenderPassError;

impl fmt::Display for InfallibleRenderPassError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("infallible render pass failed")
    }
}

impl Error for InfallibleRenderPassError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phases_compile_in_standard_order_without_losing_stable_ids() {
        let mut graph = RenderGraph::new();
        let ui = graph
            .add_infallible_pass(
                RenderPassDescriptor::new("ui", RenderPhase::NativeUi)
                    .writes(RenderResourceId::surface_color()),
                |_| {},
            )
            .expect("valid UI pass");
        let opaque = graph
            .add_infallible_pass(
                RenderPassDescriptor::new("opaque", RenderPhase::Opaque3d)
                    .writes(RenderResourceId::surface_color())
                    .writes(RenderResourceId::depth()),
                |_| {},
            )
            .expect("valid opaque pass");

        let ordered: Vec<_> = graph.passes().map(|(id, pass)| (id, pass.phase)).collect();
        assert_eq!(
            ordered,
            [(opaque, RenderPhase::Opaque3d), (ui, RenderPhase::NativeUi)]
        );
    }

    #[test]
    fn dependency_cannot_point_back_from_an_earlier_phase() {
        let mut graph = RenderGraph::new();
        let ui = graph
            .add_infallible_pass(
                RenderPassDescriptor::new("ui", RenderPhase::NativeUi),
                |_| {},
            )
            .expect("valid pass");
        assert!(matches!(
            graph.add_infallible_pass(
                RenderPassDescriptor::new("opaque", RenderPhase::Opaque3d).after(ui),
                |_| {}
            ),
            Err(RenderGraphBuildError::DependencyPhaseInversion)
        ));
    }

    #[test]
    fn labels_dependencies_and_resources_are_validated() {
        let mut graph = RenderGraph::new();
        let pass = graph
            .add_infallible_pass(
                RenderPassDescriptor::new("world", RenderPhase::World2d),
                |_| {},
            )
            .expect("valid pass");
        assert!(matches!(
            graph.add_infallible_pass(
                RenderPassDescriptor::new("world", RenderPhase::World2d),
                |_| {}
            ),
            Err(RenderGraphBuildError::DuplicateLabel)
        ));
        assert!(matches!(
            graph.add_infallible_pass(
                RenderPassDescriptor::new("duplicate", RenderPhase::World2d)
                    .after(pass)
                    .after(pass),
                |_| {}
            ),
            Err(RenderGraphBuildError::DuplicateDependency)
        ));
        let surface = RenderResourceId::surface_color();
        assert!(matches!(
            graph.add_infallible_pass(
                RenderPassDescriptor::new("resources", RenderPhase::World2d)
                    .reads(surface.clone())
                    .reads(surface),
                |_| {}
            ),
            Err(RenderGraphBuildError::DuplicateResourceAccess)
        ));
    }
}
