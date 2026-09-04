//! Bounded Source 1 VMT material text parsing.
//!
//! VMT uses the same quoted KeyValues and nested-block grammar as VMF. This
//! crate reuses the bounded lexer/parser from yuyib-vmf, then provides a
//! material-specific typed view. It supports Source 1 shader names and the
//! typed material properties needed by ordinary, terrain and Water shaders,
//! including the common AnimatedTexture and TextureScroll proxies. Unknown
//! properties and child blocks remain visible unchanged.
//! No VTF decoding, filesystem resolution, GPU material emission, Source 2 or
//! complete VMT directive support is claimed.

#![forbid(unsafe_code)]
#![allow(
    clippy::doc_markdown,
    reason = "The compact public names in this material layer are intentionally plain."
)]

use std::{error::Error, fmt};

use yuyib_vmf::{
    VmfBlock, VmfLimits, VmfParseError, VmfParseErrorKind, VmfProperty, parse_keyvalues_with_limits,
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
    let map = parse_keyvalues_with_limits(input, limits.into())
        .map_err(|error| VmtParseError::from_vmf(&error))?;
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
#[derive(Clone, Debug, PartialEq)]
pub struct VmtMaterial {
    root: VmfBlock,
    translucent: Option<bool>,
    alpha_test: Option<bool>,
    above_water: Option<bool>,
    refract: Option<bool>,
    reflect_amount: Option<f32>,
    refract_amount: Option<f32>,
    fog_enable: Option<bool>,
    fog_color: Option<[u8; 3]>,
    fog_start: Option<f32>,
    fog_end: Option<f32>,
    proxies: Vec<VmtProxy>,
}

impl VmtMaterial {
    fn from_block(root: VmfBlock) -> Result<Self, VmtParseError> {
        let translucent = optional_bool(&root, "$translucent")?;
        let alpha_test = optional_bool(&root, "$alphatest")?;
        let above_water = optional_bool(&root, "$abovewater")?;
        let refract = optional_bool(&root, "$refract")?;
        let reflect_amount = optional_f32(&root, "$reflectamount")?;
        let refract_amount = optional_f32(&root, "$refractamount")?;
        let fog_enable = optional_bool(&root, "$fogenable")?;
        let fog_color = optional_rgb8(&root, "$fogcolor")?;
        let fog_start = optional_f32(&root, "$fogstart")?;
        let fog_end = optional_f32(&root, "$fogend")?;
        let proxies = parse_proxies(&root)?;
        Ok(Self {
            root,
            translucent,
            alpha_test,
            above_water,
            refract,
            reflect_amount,
            refract_amount,
            fog_enable,
            fog_color,
            fog_start,
            fog_end,
            proxies,
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

    /// Returns the optional secondary base texture path exactly as authored.
    ///
    /// Source terrain shaders such as `WorldVertexTransition` blend this
    /// texture with [`Self::base_texture`] using displacement vertex alpha.
    #[must_use]
    pub fn base_texture2(&self) -> Option<&str> {
        self.root.property("$basetexture2")
    }

    /// Returns the optional normal-map texture path exactly as authored.
    #[must_use]
    pub fn bump_map(&self) -> Option<&str> {
        self.root.property("$bumpmap")
    }

    /// Returns the optional Water normal-map path exactly as authored.
    #[must_use]
    pub fn normal_map(&self) -> Option<&str> {
        self.root.property("$normalmap")
    }

    /// Returns the optional material rendered below a Water surface.
    #[must_use]
    pub fn bottom_material(&self) -> Option<&str> {
        self.root.property("$bottommaterial")
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

    /// Returns whether the authored Water material represents its above-water side.
    #[must_use]
    pub const fn above_water(&self) -> Option<bool> {
        self.above_water
    }

    /// Returns whether Water refraction is enabled.
    #[must_use]
    pub const fn refract(&self) -> Option<bool> {
        self.refract
    }

    /// Returns the authored Water reflection strength.
    #[must_use]
    pub const fn reflect_amount(&self) -> Option<f32> {
        self.reflect_amount
    }

    /// Returns the authored Water refraction strength.
    #[must_use]
    pub const fn refract_amount(&self) -> Option<f32> {
        self.refract_amount
    }

    /// Returns whether distance fog is enabled for the Water material.
    #[must_use]
    pub const fn fog_enabled(&self) -> Option<bool> {
        self.fog_enable
    }

    /// Returns the authored Water fog colour as 8-bit RGB.
    #[must_use]
    pub const fn fog_color(&self) -> Option<[u8; 3]> {
        self.fog_color
    }

    /// Returns the Water fog start distance.
    #[must_use]
    pub const fn fog_start(&self) -> Option<f32> {
        self.fog_start
    }

    /// Returns the Water fog end distance.
    #[must_use]
    pub const fn fog_end(&self) -> Option<f32> {
        self.fog_end
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

    /// Returns typed proxy blocks in authored order.
    ///
    /// Unsupported proxy blocks remain available as [`VmtProxy::Unknown`],
    /// while the original `Proxies` block is also retained by [`Self::blocks`].
    #[must_use]
    pub fn proxies(&self) -> &[VmtProxy] {
        &self.proxies
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

/// One typed child of a VMT `Proxies` block.
#[derive(Clone, Debug, PartialEq)]
pub enum VmtProxy {
    /// Advances a multi-frame VTF at the authored rate.
    AnimatedTexture(VmtAnimatedTextureProxy),
    /// Scrolls one texture transform over time.
    TextureScroll(VmtTextureScrollProxy),
    /// Proxy type outside the typed subset, preserved losslessly.
    Unknown(VmtBlock),
}

/// Typed Source `AnimatedTexture` material proxy.
#[derive(Clone, Debug, PartialEq)]
pub struct VmtAnimatedTextureProxy {
    texture_variable: Option<String>,
    frame_variable: Option<String>,
    frame_rate: Option<f32>,
}

impl VmtAnimatedTextureProxy {
    /// Returns the texture variable advanced by the proxy.
    #[must_use]
    pub fn texture_variable(&self) -> Option<&str> {
        self.texture_variable.as_deref()
    }

    /// Returns the material variable receiving the current frame number.
    #[must_use]
    pub fn frame_variable(&self) -> Option<&str> {
        self.frame_variable.as_deref()
    }

    /// Returns the authored animation rate in frames per second.
    #[must_use]
    pub const fn frame_rate(&self) -> Option<f32> {
        self.frame_rate
    }
}

/// Typed Source `TextureScroll` material proxy.
#[derive(Clone, Debug, PartialEq)]
pub struct VmtTextureScrollProxy {
    texture_scroll_variable: Option<String>,
    rate: Option<f32>,
    angle_degrees: Option<f32>,
    texture_scale: Option<f32>,
}

impl VmtTextureScrollProxy {
    /// Returns the texture-transform variable updated by the proxy.
    #[must_use]
    pub fn texture_scroll_variable(&self) -> Option<&str> {
        self.texture_scroll_variable.as_deref()
    }

    /// Returns the authored scrolling speed.
    #[must_use]
    pub const fn rate(&self) -> Option<f32> {
        self.rate
    }

    /// Returns the authored scrolling direction in degrees.
    #[must_use]
    pub const fn angle_degrees(&self) -> Option<f32> {
        self.angle_degrees
    }

    /// Returns the authored texture-coordinate scale.
    #[must_use]
    pub const fn texture_scale(&self) -> Option<f32> {
        self.texture_scale
    }
}

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

fn optional_f32(root: &VmfBlock, key: &str) -> Result<Option<f32>, VmtParseError> {
    root.property(key)
        .map(|value| parse_f32(key, value))
        .transpose()
}

fn parse_f32(key: &str, value: &str) -> Result<f32, VmtParseError> {
    match value.parse::<f32>() {
        Ok(number) if number.is_finite() => Ok(number),
        _ => Err(VmtParseError::new(
            VmtParseErrorKind::InvalidNumber {
                key: key.to_owned(),
                value: value.to_owned(),
            },
            1,
            1,
        )),
    }
}

fn optional_rgb8(root: &VmfBlock, key: &str) -> Result<Option<[u8; 3]>, VmtParseError> {
    let Some(value) = root.property(key) else {
        return Ok(None);
    };
    let components = value
        .trim()
        .trim_start_matches('{')
        .trim_end_matches('}')
        .split_ascii_whitespace()
        .map(str::parse::<u8>)
        .collect::<Result<Vec<_>, _>>();
    let Ok(components) = components else {
        return Err(invalid_color(key, value));
    };
    let [red, green, blue] = components.as_slice() else {
        return Err(invalid_color(key, value));
    };
    Ok(Some([*red, *green, *blue]))
}

fn invalid_color(key: &str, value: &str) -> VmtParseError {
    VmtParseError::new(
        VmtParseErrorKind::InvalidColor {
            key: key.to_owned(),
            value: value.to_owned(),
        },
        1,
        1,
    )
}

fn parse_proxies(root: &VmfBlock) -> Result<Vec<VmtProxy>, VmtParseError> {
    let mut proxies = Vec::new();
    for proxies_block in root
        .blocks()
        .iter()
        .filter(|block| block.name().eq_ignore_ascii_case("Proxies"))
    {
        for block in proxies_block.blocks() {
            if block.name().eq_ignore_ascii_case("AnimatedTexture") {
                proxies.push(VmtProxy::AnimatedTexture(VmtAnimatedTextureProxy {
                    texture_variable: block.property("animatedTextureVar").map(str::to_owned),
                    frame_variable: block
                        .property("animatedTextureFrameNumVar")
                        .map(str::to_owned),
                    frame_rate: optional_f32(block, "animatedTextureFrameRate")?,
                }));
            } else if block.name().eq_ignore_ascii_case("TextureScroll") {
                proxies.push(VmtProxy::TextureScroll(VmtTextureScrollProxy {
                    texture_scroll_variable: block.property("textureScrollVar").map(str::to_owned),
                    rate: optional_f32(block, "textureScrollRate")?,
                    angle_degrees: optional_f32(block, "textureScrollAngle")?,
                    texture_scale: optional_f32(block, "textureScale")?,
                }));
            } else {
                proxies.push(VmtProxy::Unknown(block.clone()));
            }
        }
    }
    Ok(proxies)
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
    /// A supported numeric property was not a finite Source scalar.
    InvalidNumber {
        /// Property key.
        key: String,
        /// Rejected source value.
        value: String,
    },
    /// A supported colour property was not three 8-bit RGB components.
    InvalidColor {
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
    fn accepts_real_vmt_scalar_and_quoted_block_variants() {
        let material = parse(
            r#""LightmappedGeneric"
            {
                $basetexture "mirrorsedge/white_concrete"
                "$normalmapalphaenvmapmask" 1
                $detail detail\noise_detail_01
                $detailblendfactor 0.1
                "Proxies" { "AnimatedTexture" { "animatedtextureframerate" 0 } }
            }"#,
        )
        .expect("real Valve KeyValues variants");
        assert_eq!(material.shader(), "LightmappedGeneric");
        assert_eq!(material.base_texture(), Some("mirrorsedge/white_concrete"));
        assert_eq!(
            material.block().property("$detail"),
            Some("detail\\noise_detail_01")
        );
        assert_eq!(material.blocks()[0].name(), "Proxies");
    }

    #[test]
    fn source_keys_ignore_case_and_quoted_paths_keep_backslashes() {
        let material = parse(
            r#"LightmappedGeneric {
                "$baseTexture" "antmaps\medieval\wood_roof2"
                "$SurfaceProp" "wood"
            }"#,
        )
        .expect("ordinary Source VMT");

        assert_eq!(
            material.base_texture(),
            Some("antmaps\\medieval\\wood_roof2")
        );
        assert_eq!(material.surface_prop(), Some("wood"));
    }

    #[test]
    fn exposes_world_vertex_transition_second_base_texture() {
        let material = parse(
            r#"WorldVertexTransition {
                "$basetexture" "terrain/grass"
                "$basetexture2" "terrain/rock"
            }"#,
        )
        .expect("world vertex transition material");

        assert_eq!(material.base_texture(), Some("terrain/grass"));
        assert_eq!(material.base_texture2(), Some("terrain/rock"));
    }

    #[test]
    fn exposes_typed_water_properties_and_animation_proxies() {
        let material = parse(
            r#"Water
            {
                $surfaceprop Water
                $envmap "env_cubemap"
                $normalmap "alex_water/alex_water_8_n"
                $bottommaterial "alex_water/alex_water_8_bottom"
                $abovewater 1
                $reflecttexture "_rt_waterreflection"
                $reflectamount 0.09
                $refracttexture "_rt_waterrefraction"
                $refract 1
                $refractamount 0.09
                $fogenable 1
                $fogcolor "{27 38 41}"
                $fogstart 150
                $fogend 304
                %compilewater 1
                Proxies
                {
                    AnimatedTexture
                    {
                        animatedTextureVar "$normalmap"
                        animatedTextureFrameNumVar "$bumpframe"
                        animatedTextureFrameRate 30
                    }
                    TextureScroll
                    {
                        texturescrollvar "$bumptransform"
                        texturescrollrate 0.07
                        texturescrollangle 0
                        texturescale 0.35
                    }
                }
            }"#,
        )
        .expect("user Water material");

        assert!(material.shader().eq_ignore_ascii_case("Water"));
        assert_eq!(material.normal_map(), Some("alex_water/alex_water_8_n"));
        assert_eq!(
            material.bottom_material(),
            Some("alex_water/alex_water_8_bottom")
        );
        assert_eq!(material.above_water(), Some(true));
        assert_eq!(material.reflect_amount(), Some(0.09));
        assert_eq!(material.refract(), Some(true));
        assert_eq!(material.refract_amount(), Some(0.09));
        assert_eq!(material.fog_enabled(), Some(true));
        assert_eq!(material.fog_color(), Some([27, 38, 41]));
        assert_eq!(material.fog_start(), Some(150.0));
        assert_eq!(material.fog_end(), Some(304.0));
        assert_eq!(material.block().property("$envmap"), Some("env_cubemap"));
        assert_eq!(
            material.block().property("$reflecttexture"),
            Some("_rt_waterreflection")
        );
        assert_eq!(material.proxies().len(), 2);

        let VmtProxy::AnimatedTexture(animated) = &material.proxies()[0] else {
            panic!("first proxy must be AnimatedTexture");
        };
        assert_eq!(animated.texture_variable(), Some("$normalmap"));
        assert_eq!(animated.frame_variable(), Some("$bumpframe"));
        assert_eq!(animated.frame_rate(), Some(30.0));

        let VmtProxy::TextureScroll(scroll) = &material.proxies()[1] else {
            panic!("second proxy must be TextureScroll");
        };
        assert_eq!(scroll.texture_scroll_variable(), Some("$bumptransform"));
        assert_eq!(scroll.rate(), Some(0.07));
        assert_eq!(scroll.angle_degrees(), Some(0.0));
        assert_eq!(scroll.texture_scale(), Some(0.35));
    }

    #[test]
    fn water_proxy_keys_ignore_case_and_unknown_proxy_blocks_survive() {
        let material = parse(
            r#"wAtEr {
                "$FoGeNaBlE" "0"
                "$FoGcOlOr" "{1 2 3}"
                "pRoXiEs" {
                    "aNiMaTeDtExTuRe" { "ANIMATEDTEXTUREFRAMERATE" "12.5" }
                    "Sine" { "resultVar" "$alpha" }
                }
            }"#,
        )
        .expect("case-insensitive Water material");

        assert_eq!(material.fog_enabled(), Some(false));
        assert_eq!(material.fog_color(), Some([1, 2, 3]));
        assert!(matches!(
            &material.proxies()[0],
            VmtProxy::AnimatedTexture(proxy) if proxy.frame_rate() == Some(12.5)
        ));
        assert!(matches!(
            &material.proxies()[1],
            VmtProxy::Unknown(block) if block.name() == "Sine"
        ));
        assert_eq!(
            material.blocks()[0].blocks()[1].property("resultvar"),
            Some("$alpha")
        );
    }

    #[test]
    fn malformed_typed_water_scalars_are_rejected() {
        assert!(matches!(
            parse(r#"Water { "$reflectamount" "many" }"#),
            Err(VmtParseError {
                kind: VmtParseErrorKind::InvalidNumber { .. },
                ..
            })
        ));
        assert!(matches!(
            parse(r#"Water { "$fogcolor" "{27 38}" }"#),
            Err(VmtParseError {
                kind: VmtParseErrorKind::InvalidColor { .. },
                ..
            })
        ));
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
