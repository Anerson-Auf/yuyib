//! Bounded Source 1 VMT material text parsing.
//!
//! VMT uses the same quoted KeyValues and nested-block grammar as VMF. This
//! crate reuses the bounded lexer/parser from yuyib-vmf, then provides a
//! material-specific typed view. It supports Source 1 shader names and the
//! initial property subset: basetexture, bumpmap, translucent, alphatest and
//! surfaceprop. Unknown properties and child blocks remain visible unchanged.
//! No VTF decoding, filesystem resolution, GPU material emission, Source 2 or
//! complete VMT directive support is claimed.

#![forbid(unsafe_code)]
#![allow(
    clippy::doc_markdown,
    reason = "The compact public names in this material layer are intentionally plain."
)]

use std::{error::Error, fmt};

use yuyib_vmf::{
    VmfBlock, VmfLimits, VmfParseError, VmfParseErrorKind, VmfProperty, parse_with_limits,
};

/// Bounds applied before parsing VMT text.
///
/// These map one-to-one onto the reusable bounded KeyValues parser limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VmtLimits {
    /// Maximum input bytes.
    pub max_input_bytes: usize,
    /// Maximum lexical tokens.
    pub max_tokens: usize,
    /// Maximum nested block depth.
    pub max_depth: usize,
    /// Maximum decoded string bytes.
    pub max_string_bytes: usize,
    /// Maximum blocks.
    pub max_blocks: usize,
    /// Maximum properties.
    pub max_properties: usize,
}

impl Default for VmtLimits {
    fn default() -> Self {
        let limits = VmfLimits::default();
        Self {
            max_input_bytes: limits.max_input_bytes,
            max_tokens: limits.max_tokens,
            max_depth: limits.max_depth,
            max_string_bytes: limits.max_string_bytes,
            max_blocks: limits.max_blocks,
            max_properties: limits.max_properties,
        }
    }
}

impl From<VmtLimits> for VmfLimits {
    fn from(limits: VmtLimits) -> Self {
        Self {
            max_input_bytes: limits.max_input_bytes,
            max_tokens: limits.max_tokens,
            max_depth: limits.max_depth,
            max_string_bytes: limits.max_string_bytes,
            max_blocks: limits.max_blocks,
            max_properties: limits.max_properties,
        }
    }
}

/// Parses one Source 1 VMT material with default limits.
///
/// # Errors
///
/// Returns VmtParseError for bounded KeyValues syntax failures or an invalid
/// material root/property in the supported subset.
pub fn parse(input: &str) -> Result<VmtMaterial, VmtParseError> {
    parse_with_vmt_limits(input, VmtLimits::default())
}

/// Parses one Source 1 VMT material with explicit limits.
///
/// # Errors
///
/// Returns VmtParseError for bounded KeyValues syntax failures or an invalid
/// material root/property in the supported subset.
pub fn parse_with_vmt_limits(input: &str, limits: VmtLimits) -> Result<VmtMaterial, VmtParseError> {
    let map =
        parse_with_limits(input, limits.into()).map_err(|error| VmtParseError::from_vmf(&error))?;
    let mut roots = map.other_blocks().iter();
    let Some(root) = roots.next() else {
        return Err(VmtParseError::new(VmtParseErrorKind::MissingShader, 1, 1));
    };
    if roots.next().is_some() || map.world().is_some() || !map.entities().is_empty() {
        return Err(VmtParseError::new(
            VmtParseErrorKind::MultipleRootBlocks,
            1,
            1,
        ));
    }
    VmtMaterial::from_block(root.clone())
}

/// Typed view of one Source 1 VMT material root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VmtMaterial {
    root: VmfBlock,
    translucent: Option<bool>,
    alpha_test: Option<bool>,
}

impl VmtMaterial {
    fn from_block(root: VmfBlock) -> Result<Self, VmtParseError> {
        let translucent = optional_bool(&root, "$translucent")?;
        let alpha_test = optional_bool(&root, "$alphatest")?;
        Ok(Self {
            root,
            translucent,
            alpha_test,
        })
    }

    /// Returns the Source 1 shader name, such as LightmappedGeneric.
    #[must_use]
    pub fn shader(&self) -> &str {
        self.root.name()
    }

    /// Returns the optional base texture path exactly as authored.
    #[must_use]
    pub fn base_texture(&self) -> Option<&str> {
        self.root.property("$basetexture")
    }

    /// Returns the optional normal-map texture path exactly as authored.
    #[must_use]
    pub fn bump_map(&self) -> Option<&str> {
        self.root.property("$bumpmap")
    }

    /// Returns optional parsed translucent mode.
    ///
    /// Only exact Source-style 0 and 1 values are accepted by this initial
    /// subset; malformed values produce VmtParseError during parsing.
    #[must_use]
    pub const fn translucent(&self) -> Option<bool> {
        self.translucent
    }

    /// Returns optional parsed alpha-test mode.
    #[must_use]
    pub const fn alpha_test(&self) -> Option<bool> {
        self.alpha_test
    }

    /// Returns optional Source surface property name.
    #[must_use]
    pub fn surface_prop(&self) -> Option<&str> {
        self.root.property("$surfaceprop")
    }

    /// Returns every root property, including unsupported ones, in source order.
    #[must_use]
    pub fn properties(&self) -> &[VmtProperty] {
        self.root.properties()
    }

    /// Returns every nested KeyValues block in source order.
    #[must_use]
    pub fn blocks(&self) -> &[VmtBlock] {
        self.root.blocks()
    }

    /// Returns the generic root block for advanced, unsupported directives.
    #[must_use]
    pub fn block(&self) -> &VmtBlock {
        &self.root
    }
}

/// Generic VMT KeyValues block preserved from the parser.
pub type VmtBlock = VmfBlock;
/// Generic VMT KeyValues property preserved from the parser.
pub type VmtProperty = VmfProperty;

fn optional_bool(root: &VmfBlock, key: &str) -> Result<Option<bool>, VmtParseError> {
    let Some(value) = root.property(key) else {
        return Ok(None);
    };
    match value {
        "0" => Ok(Some(false)),
        "1" => Ok(Some(true)),
        _ => Err(VmtParseError::new(
            VmtParseErrorKind::InvalidBoolean {
                key: key.to_owned(),
                value: value.to_owned(),
            },
            1,
            1,
        )),
    }
}

/// Location-rich Source 1 VMT parsing failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VmtParseError {
    kind: VmtParseErrorKind,
    line: usize,
    column: usize,
}

impl VmtParseError {
    fn new(kind: VmtParseErrorKind, line: usize, column: usize) -> Self {
        Self { kind, line, column }
    }
    fn from_vmf(error: &VmfParseError) -> Self {
        Self::new(
            VmtParseErrorKind::KeyValues(error.kind().clone()),
            error.line(),
            error.column(),
        )
    }
    /// Returns structured failure kind.
    #[must_use]
    pub const fn kind(&self) -> &VmtParseErrorKind {
        &self.kind
    }
    /// Returns one-based source line.
    #[must_use]
    pub const fn line(&self) -> usize {
        self.line
    }
    /// Returns one-based source column.
    #[must_use]
    pub const fn column(&self) -> usize {
        self.column
    }
}

impl fmt::Display for VmtParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "VMT parse error at {}:{}: {:?}",
            self.line, self.column, self.kind
        )
    }
}
impl Error for VmtParseError {}

/// Structured VMT parsing failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VmtParseErrorKind {
    /// Reusable KeyValues parser rejected text or a configured bound.
    KeyValues(VmfParseErrorKind),
    /// Input did not contain a shader root block.
    MissingShader,
    /// Input contained more than one top-level material root.
    MultipleRootBlocks,
    /// A supported boolean property was not exact 0 or 1.
    InvalidBoolean {
        /// Property key.
        key: String,
        /// Rejected source value.
        value: String,
    },
}

#[cfg(test)]
mod tests {
    use yuyib_vmf::VmfLimit;

    use super::*;

    #[test]
    fn parses_generic_and_vertex_lit_subset() {
        let material = parse(
            r#"LightmappedGeneric {
                "$basetexture" "brick/wall"
                "$bumpmap" "brick/wall_normal"
                "$translucent" "1"
                "$surfaceprop" "brick"
                Proxies { AnimatedTexture { "animatedtexturevar" "$basetexture" } }
            }"#,
        )
        .expect("valid VMT");
        assert_eq!(material.shader(), "LightmappedGeneric");
        assert_eq!(material.base_texture(), Some("brick/wall"));
        assert_eq!(material.bump_map(), Some("brick/wall_normal"));
        assert_eq!(material.translucent(), Some(true));
        assert_eq!(material.surface_prop(), Some("brick"));
        assert_eq!(material.blocks()[0].name(), "Proxies");
        assert_eq!(material.properties().len(), 4);
    }

    #[test]
    fn comments_and_escapes_follow_supported_keyvalues_contract() {
        let material = parse(
            "VertexLitGeneric { // ignored\n \"$basetexture\" \"models/\\\\\\\"quoted\" \"$alphatest\" \"0\" }",
        ).expect("valid escapes");
        assert_eq!(material.shader(), "VertexLitGeneric");
        assert_eq!(material.base_texture(), Some("models/\\\"quoted"));
        assert_eq!(material.alpha_test(), Some(false));
    }

    #[test]
    fn malformed_and_budgeted_input_is_rejected() {
        assert!(matches!(
            parse("LightmappedGeneric {"),
            Err(VmtParseError { .. })
        ));
        assert!(matches!(
            parse(r#"LightmappedGeneric { "$translucent" "yes" }"#),
            Err(VmtParseError {
                kind: VmtParseErrorKind::InvalidBoolean { .. },
                ..
            })
        ));
        let error = parse_with_vmt_limits(
            r#"LightmappedGeneric { "$basetexture" "brick/wall" }"#,
            VmtLimits {
                max_tokens: 1,
                ..VmtLimits::default()
            },
        )
        .expect_err("token budget");
        assert!(matches!(
            error.kind(),
            VmtParseErrorKind::KeyValues(VmfParseErrorKind::LimitExceeded {
                limit: VmfLimit::Tokens,
                ..
            })
        ));
    }
}
