//! Renderer-neutral descriptions of WGSL shader programs.
//!
//! This crate keeps shader configuration separate from a GPU backend. It owns
//! WGSL text, describes the entry points a renderer should compile, and offers
//! small, explicitly documented prototype effects. It intentionally does not
//! parse, compile, reflect or hot-reload WGSL: only the selected renderer and
//! its target device can give those operations meaningful diagnostics.
//!
//! Use [`ShaderProgram::graphics`] or [`ShaderProgram::compute`] for explicit
//! WGSL. For a fast prototype, use [`ShaderPrototype::program`]. A future
//! renderer can consume the resulting [`ShaderProgram`] without forcing an
//! application to depend on WGPU types.

#![forbid(unsafe_code)]

use std::{error::Error, fmt, sync::Arc};

/// A human-readable shader label, source text, and no backend-specific state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShaderSource {
    label: Option<Arc<str>>,
    wgsl: Arc<str>,
}

impl ShaderSource {
    /// Creates a WGSL source object after cheap structural validation.
    ///
    /// This checks only that the source has non-whitespace content. It does
    /// not parse WGSL or contact a GPU; a renderer reports compile errors at
    /// pipeline creation time.
    ///
    /// # Errors
    ///
    /// Returns [`ShaderSourceError::EmptyWgsl`] when `wgsl` contains only
    /// whitespace.
    pub fn wgsl(wgsl: impl Into<Arc<str>>) -> Result<Self, ShaderSourceError> {
        let wgsl = wgsl.into();
        if wgsl.trim().is_empty() {
            return Err(ShaderSourceError::EmptyWgsl);
        }
        Ok(Self { label: None, wgsl })
    }

    /// Assigns a label used in backend diagnostics and graphics debuggers.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<Arc<str>>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Returns the optional diagnostic label.
    #[must_use]
    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    /// Returns the unchanged WGSL source text.
    #[must_use]
    pub fn wgsl_source(&self) -> &str {
        &self.wgsl
    }
}

/// A programmable pipeline stage supported by WGSL.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ShaderStage {
    /// Vertex processing for a graphics pipeline.
    Vertex,
    /// Fragment processing for a graphics pipeline.
    Fragment,
    /// Compute processing for a compute pipeline.
    Compute,
}

impl fmt::Display for ShaderStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Vertex => "vertex",
            Self::Fragment => "fragment",
            Self::Compute => "compute",
        })
    }
}

/// One named WGSL entry point that a renderer should compile for a stage.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct ShaderEntryPoint {
    stage: ShaderStage,
    name: Arc<str>,
}

impl ShaderEntryPoint {
    /// Creates an entry point after validating its portable ASCII identifier.
    ///
    /// The conservative identifier policy intentionally keeps configuration
    /// portable across diagnostics and asset pipelines. WGSL source itself is
    /// not restricted by this helper.
    ///
    /// # Errors
    ///
    /// Returns [`ShaderEntryPointError::InvalidName`] if `name` is not an
    /// ASCII identifier beginning with a letter or underscore.
    pub fn new(
        stage: ShaderStage,
        name: impl Into<Arc<str>>,
    ) -> Result<Self, ShaderEntryPointError> {
        let name = name.into();
        if !is_portable_identifier(&name) {
            return Err(ShaderEntryPointError::InvalidName { name });
        }
        Ok(Self { stage, name })
    }

    /// Returns the selected programmable stage.
    #[must_use]
    pub const fn stage(&self) -> ShaderStage {
        self.stage
    }

    /// Returns the WGSL function name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// A fully specified graphics shader pair.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphicsShader {
    vertex: ShaderEntryPoint,
    fragment: ShaderEntryPoint,
}

impl GraphicsShader {
    /// Creates a graphics entry-point pair.
    ///
    /// # Errors
    ///
    /// Returns [`ShaderProgramError::InvalidStage`] when entries do not use
    /// vertex and fragment stages respectively.
    pub fn new(
        vertex: ShaderEntryPoint,
        fragment: ShaderEntryPoint,
    ) -> Result<Self, ShaderProgramError> {
        if vertex.stage != ShaderStage::Vertex {
            return Err(ShaderProgramError::InvalidStage {
                slot: ShaderProgramSlot::Vertex,
                actual: vertex.stage,
            });
        }
        if fragment.stage != ShaderStage::Fragment {
            return Err(ShaderProgramError::InvalidStage {
                slot: ShaderProgramSlot::Fragment,
                actual: fragment.stage,
            });
        }
        Ok(Self { vertex, fragment })
    }

    /// Returns the vertex entry point.
    #[must_use]
    pub const fn vertex(&self) -> &ShaderEntryPoint {
        &self.vertex
    }

    /// Returns the fragment entry point.
    #[must_use]
    pub const fn fragment(&self) -> &ShaderEntryPoint {
        &self.fragment
    }
}

/// A validated shader program that a renderer may compile into a pipeline.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShaderProgram {
    source: ShaderSource,
    kind: ShaderProgramKind,
}

impl ShaderProgram {
    /// Combines WGSL with a vertex and fragment entry point.
    ///
    /// This validates configuration only; it does not verify the functions
    /// occur in the WGSL source or that their interfaces agree.
    ///
    /// # Errors
    ///
    /// Returns [`ShaderProgramError`] for invalid stage assignments.
    pub fn graphics(
        source: ShaderSource,
        vertex: ShaderEntryPoint,
        fragment: ShaderEntryPoint,
    ) -> Result<Self, ShaderProgramError> {
        Ok(Self {
            source,
            kind: ShaderProgramKind::Graphics(GraphicsShader::new(vertex, fragment)?),
        })
    }

    /// Combines WGSL with a compute entry point.
    ///
    /// # Errors
    ///
    /// Returns [`ShaderProgramError::InvalidStage`] if the entry is not a
    /// compute entry point.
    pub fn compute(
        source: ShaderSource,
        entry: ShaderEntryPoint,
    ) -> Result<Self, ShaderProgramError> {
        if entry.stage != ShaderStage::Compute {
            return Err(ShaderProgramError::InvalidStage {
                slot: ShaderProgramSlot::Compute,
                actual: entry.stage,
            });
        }
        Ok(Self {
            source,
            kind: ShaderProgramKind::Compute(entry),
        })
    }

    /// Returns the source to provide to a rendering backend.
    #[must_use]
    pub const fn source(&self) -> &ShaderSource {
        &self.source
    }

    /// Returns the selected program configuration.
    #[must_use]
    pub const fn kind(&self) -> &ShaderProgramKind {
        &self.kind
    }
}

/// The program shape selected for a [`ShaderProgram`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShaderProgramKind {
    /// A vertex/fragment graphics program.
    Graphics(GraphicsShader),
    /// A single compute entry point.
    Compute(ShaderEntryPoint),
}

/// A small, fixed shader effect for prototypes and smoke tests.
///
/// A prototype has an explicit [`ShaderInterface`] instead of hidden magic.
/// Its WGSL is valid as authored, but must still be compiled by the renderer
/// against the target GPU and its chosen render-target format.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ShaderPrototype {
    /// Passes vertex colour through to a fragment colour output.
    VertexColor,
}

impl ShaderPrototype {
    /// Builds a self-contained graphics program for this prototype.
    ///
    /// # Panics
    ///
    /// Panics only if Yuyib's own static prototype metadata becomes invalid,
    /// which is an implementation bug rather than an application input error.
    #[must_use]
    pub fn program(self) -> ShaderProgram {
        let source = ShaderSource::wgsl(self.wgsl())
            .expect("built-in prototype WGSL must never be empty")
            .with_label(self.label());
        let vertex = ShaderEntryPoint::new(ShaderStage::Vertex, "vs_main")
            .expect("built-in vertex name must be valid");
        let fragment = ShaderEntryPoint::new(ShaderStage::Fragment, "fs_main")
            .expect("built-in fragment name must be valid");
        ShaderProgram::graphics(source, vertex, fragment)
            .expect("built-in prototype stages must be valid")
    }

    /// Returns the vertex input and bind-group contract for this prototype.
    #[must_use]
    pub const fn interface(self) -> ShaderInterface {
        match self {
            Self::VertexColor => ShaderInterface {
                vertex_attributes: VERTEX_COLOR_ATTRIBUTES,
                bindings: &[],
            },
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::VertexColor => "yuyib.prototype.vertex-color",
        }
    }

    fn wgsl(self) -> &'static str {
        match self {
            Self::VertexColor => VERTEX_COLOR_WGSL,
        }
    }
}

/// Renderer-independent vertex and resource contract for a shader.
///
/// This is declared metadata, not reflection. For custom WGSL, a renderer or
/// application must supply its own matching pipeline layout until reflection is
/// deliberately added as a backend feature.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShaderInterface {
    vertex_attributes: &'static [VertexAttribute],
    bindings: &'static [ShaderBinding],
}

impl ShaderInterface {
    /// Returns required vertex attributes in ascending WGSL location order.
    #[must_use]
    pub const fn vertex_attributes(&self) -> &'static [VertexAttribute] {
        self.vertex_attributes
    }

    /// Returns declared bind-group requirements.
    #[must_use]
    pub const fn bindings(&self) -> &'static [ShaderBinding] {
        self.bindings
    }
}

/// One vertex attribute required by a shader interface.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct VertexAttribute {
    location: u32,
    format: VertexFormat,
}

impl VertexAttribute {
    /// Creates a vertex attribute descriptor.
    #[must_use]
    pub const fn new(location: u32, format: VertexFormat) -> Self {
        Self { location, format }
    }

    /// Returns the WGSL `@location` number.
    #[must_use]
    pub const fn location(self) -> u32 {
        self.location
    }

    /// Returns the portable vertex format.
    #[must_use]
    pub const fn format(self) -> VertexFormat {
        self.format
    }
}

/// Portable vertex input formats used by built-in prototype interfaces.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum VertexFormat {
    /// Two 32-bit floats.
    Float32x2,
    /// Three 32-bit floats.
    Float32x3,
    /// Four 32-bit floats.
    Float32x4,
}

/// A bind-group item declared by a built-in shader interface.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct ShaderBinding {
    group: u32,
    binding: u32,
    kind: ShaderBindingKind,
}

impl ShaderBinding {
    /// Creates a bind-group descriptor.
    #[must_use]
    pub const fn new(group: u32, binding: u32, kind: ShaderBindingKind) -> Self {
        Self {
            group,
            binding,
            kind,
        }
    }

    /// Returns the bind-group number.
    #[must_use]
    pub const fn group(self) -> u32 {
        self.group
    }

    /// Returns the binding number within the group.
    #[must_use]
    pub const fn binding(self) -> u32 {
        self.binding
    }

    /// Returns the resource kind.
    #[must_use]
    pub const fn kind(self) -> ShaderBindingKind {
        self.kind
    }
}

/// A portable resource category for [`ShaderBinding`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ShaderBindingKind {
    /// A read-only uniform buffer.
    UniformBuffer,
    /// A sampled 2D texture.
    Texture2d,
    /// A filtering sampler.
    FilteringSampler,
}

/// A source object had no WGSL text to give a renderer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShaderSourceError {
    /// Source text was empty or whitespace only.
    EmptyWgsl,
}

impl fmt::Display for ShaderSourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("WGSL source must not be empty")
    }
}
impl Error for ShaderSourceError {}

/// An entry point name cannot be represented by the portable configuration API.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShaderEntryPointError {
    /// The requested function name was not an ASCII identifier.
    InvalidName {
        /// The rejected name.
        name: Arc<str>,
    },
}

impl fmt::Display for ShaderEntryPointError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName { name } => {
                write!(formatter, "invalid shader entry point name `{name}`")
            }
        }
    }
}
impl Error for ShaderEntryPointError {}

/// A stage was supplied to the wrong [`ShaderProgram`] slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShaderProgramError {
    /// A graphics/compute constructor received an incompatible stage.
    InvalidStage {
        /// The fixed program slot that was configured.
        slot: ShaderProgramSlot,
        /// The entry point's actual stage.
        actual: ShaderStage,
    },
}

impl fmt::Display for ShaderProgramError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidStage { slot, actual } => {
                write!(
                    formatter,
                    "{slot} shader slot received a {actual} entry point"
                )
            }
        }
    }
}
impl Error for ShaderProgramError {}

/// A fixed slot in a [`ShaderProgram`] configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShaderProgramSlot {
    /// Vertex entry point slot.
    Vertex,
    /// Fragment entry point slot.
    Fragment,
    /// Compute entry point slot.
    Compute,
}

impl fmt::Display for ShaderProgramSlot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Vertex => "vertex",
            Self::Fragment => "fragment",
            Self::Compute => "compute",
        })
    }
}

const VERTEX_COLOR_ATTRIBUTES: &[VertexAttribute] = &[
    VertexAttribute::new(0, VertexFormat::Float32x3),
    VertexAttribute::new(1, VertexFormat::Float32x4),
];

const VERTEX_COLOR_WGSL: &str = r"
struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) color: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    output.position = vec4<f32>(input.position, 1.0);
    output.color = input.color;
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return input.color;
}
";

fn is_portable_identifier(name: &str) -> bool {
    let mut characters = name.bytes();
    let Some(first) = characters.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == b'_')
        && characters.all(|character| character.is_ascii_alphanumeric() || character == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_rejects_whitespace_only_wgsl() {
        assert_eq!(
            ShaderSource::wgsl(" \n\t "),
            Err(ShaderSourceError::EmptyWgsl)
        );
    }

    #[test]
    fn entry_points_require_portable_identifiers() {
        let error = ShaderEntryPoint::new(ShaderStage::Vertex, "not-a-name")
            .expect_err("hyphens are unsupported in portable names");
        assert!(matches!(error, ShaderEntryPointError::InvalidName { .. }));
    }

    #[test]
    fn graphics_program_enforces_its_stage_contract() {
        let source = ShaderSource::wgsl("@compute @workgroup_size(1) fn main() {}")
            .expect("non-empty source");
        let compute = ShaderEntryPoint::new(ShaderStage::Compute, "main").expect("valid name");
        let fragment = ShaderEntryPoint::new(ShaderStage::Fragment, "main").expect("valid name");
        let error = ShaderProgram::graphics(source, compute, fragment)
            .expect_err("compute cannot serve as a vertex entry");
        assert!(matches!(
            error,
            ShaderProgramError::InvalidStage {
                slot: ShaderProgramSlot::Vertex,
                ..
            }
        ));
    }

    #[test]
    fn prototype_has_explicit_program_and_interface() {
        let program = ShaderPrototype::VertexColor.program();
        assert!(program.source().wgsl_source().contains("fn vs_main"));
        assert!(matches!(program.kind(), ShaderProgramKind::Graphics(_)));
        assert_eq!(
            ShaderPrototype::VertexColor
                .interface()
                .vertex_attributes()
                .len(),
            2
        );
    }
}
