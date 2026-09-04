//! Bounded Source 1 VMF parsing.
//!
//! This crate parses Source 1 Valve Map Format text into typed world, entity,
//! solid and side views while retaining unknown ordered properties and blocks.
//! It is not a Source 2 parser and does not read compiled BSP files.
//!
//! The accepted grammar has unquoted block names, quoted property keys and
//! values, nested braces, whitespace and line comments beginning with two
//! slashes outside quoted strings. Strings support escaped quote, backslash,
//! newline, carriage return and tab characters. Other bare tokens, block
//! comments and unknown escape sequences are rejected explicitly.

#![forbid(unsafe_code)]

use std::{error::Error, fmt};

/// Limits used before parsing potentially untrusted VMF text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VmfLimits {
    /// Maximum UTF-8 input byte length.
    pub max_input_bytes: usize,
    /// Maximum lexical tokens, including braces.
    pub max_tokens: usize,
    /// Maximum nested block depth.
    pub max_depth: usize,
    /// Maximum decoded bytes in one quoted string.
    pub max_string_bytes: usize,
    /// Maximum total parsed blocks.
    pub max_blocks: usize,
    /// Maximum total parsed key/value properties.
    pub max_properties: usize,
}

impl Default for VmfLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 64 * 1024 * 1024,
            max_tokens: 2_000_000,
            max_depth: 256,
            max_string_bytes: 1024 * 1024,
            max_blocks: 250_000,
            max_properties: 1_000_000,
        }
    }
}

/// Parses a Source 1 VMF document with default bounded limits.
///
/// # Errors
///
/// Returns `VmfParseError` for malformed syntax, unsupported lexical constructs
/// or a configured resource limit.
pub fn parse(input: &str) -> Result<VmfMap, VmfParseError> {
    parse_with_limits(input, VmfLimits::default())
}

/// Parses a Source 1 VMF document with explicit limits.
///
/// # Errors
///
/// Returns `VmfParseError` for malformed syntax, unsupported lexical constructs
/// or a configured resource limit.
pub fn parse_with_limits(input: &str, limits: VmfLimits) -> Result<VmfMap, VmfParseError> {
    parse_with_mode(input, limits, false)
}

/// Parses Valve `KeyValues` with the scalar flexibility used by real VMT files.
///
/// In addition to strict VMF syntax, this accepts quoted block names and bare
/// property keys/values such as `$basetexture`, `1` and `[1 1 1]` tokens. The
/// same byte, token, depth, block and property limits remain enforced. VMF
/// callers should keep using [`parse_with_limits`]; this compatibility entry
/// point exists for formats such as VMT that share `KeyValues` but not VMF's
/// narrower quoting convention.
///
/// # Errors
///
/// Returns [`VmfParseError`] for malformed nesting, strings or configured
/// resource limits.
pub fn parse_keyvalues_with_limits(
    input: &str,
    limits: VmfLimits,
) -> Result<VmfMap, VmfParseError> {
    parse_with_mode(input, limits, true)
}

fn parse_with_mode(
    input: &str,
    limits: VmfLimits,
    permissive_scalars: bool,
) -> Result<VmfMap, VmfParseError> {
    if input.len() > limits.max_input_bytes {
        return Err(VmfParseError::new(
            VmfParseErrorKind::LimitExceeded {
                limit: VmfLimit::InputBytes,
                maximum: limits.max_input_bytes,
            },
            1,
            1,
        ));
    }
    let tokens = Lexer::new(input, limits, permissive_scalars).tokenize()?;
    Parser::new(tokens, limits, permissive_scalars).parse_map()
}

/// Parsed Source 1 VMF map with typed views and preserved unrelated blocks.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VmfMap {
    world: Option<VmfEntity>,
    entities: Vec<VmfEntity>,
    other_blocks: Vec<VmfBlock>,
}

impl VmfMap {
    /// Returns the optional top-level world block.
    #[must_use]
    pub fn world(&self) -> Option<&VmfEntity> {
        self.world.as_ref()
    }

    /// Returns top-level entity blocks in document order.
    #[must_use]
    pub fn entities(&self) -> &[VmfEntity] {
        &self.entities
    }

    /// Returns all unrelated top-level blocks in document order.
    ///
    /// Examples include versioninfo, visgroups, cameras and cordons.
    #[must_use]
    pub fn other_blocks(&self) -> &[VmfBlock] {
        &self.other_blocks
    }
}

/// Generic VMF block preserved in source order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VmfBlock {
    name: String,
    properties: Vec<VmfProperty>,
    blocks: Vec<VmfBlock>,
}

impl VmfBlock {
    /// Returns the unquoted block name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns properties in source order, including unknown keys.
    #[must_use]
    pub fn properties(&self) -> &[VmfProperty] {
        &self.properties
    }

    /// Returns child blocks in source order, including unknown blocks.
    #[must_use]
    pub fn blocks(&self) -> &[VmfBlock] {
        &self.blocks
    }

    /// Returns the first matching property value.
    ///
    /// VMF permits repeated keys; callers needing every value should inspect
    /// properties instead of relying on this convenience accessor.
    #[must_use]
    pub fn property(&self, key: &str) -> Option<&str> {
        self.properties
            .iter()
            .find(|property| property.key == key)
            .map(VmfProperty::value)
    }
}

/// One quoted VMF key/value pair.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VmfProperty {
    key: String,
    value: String,
}

impl VmfProperty {
    /// Returns the decoded quoted property key.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Returns the decoded quoted property value.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }
}

/// Typed world or entity block retaining all generic content.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VmfEntity {
    block: VmfBlock,
    solids: Vec<VmfSolid>,
    other_blocks: Vec<VmfBlock>,
}

impl VmfEntity {
    /// Returns the full generic block, including all original properties.
    #[must_use]
    pub fn block(&self) -> &VmfBlock {
        &self.block
    }

    /// Returns the first classname property, if present.
    #[must_use]
    pub fn classname(&self) -> Option<&str> {
        self.block.property("classname")
    }

    /// Returns the first editor id property, if present.
    #[must_use]
    pub fn id(&self) -> Option<&str> {
        self.block.property("id")
    }

    /// Returns the original origin string, if present.
    ///
    /// Numeric conversion remains caller-controlled so source spelling is not
    /// silently normalized.
    #[must_use]
    pub fn origin(&self) -> Option<&str> {
        self.block.property("origin")
    }

    /// Returns brush solids in source order.
    #[must_use]
    pub fn solids(&self) -> &[VmfSolid] {
        &self.solids
    }

    /// Returns unknown or non-solid child blocks in source order.
    #[must_use]
    pub fn other_blocks(&self) -> &[VmfBlock] {
        &self.other_blocks
    }
}

/// Typed brush solid preserving original properties and non-side child blocks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VmfSolid {
    block: VmfBlock,
    sides: Vec<VmfSide>,
    other_blocks: Vec<VmfBlock>,
}

impl VmfSolid {
    /// Returns the full generic solid block.
    #[must_use]
    pub fn block(&self) -> &VmfBlock {
        &self.block
    }

    /// Returns the first editor id property, if present.
    #[must_use]
    pub fn id(&self) -> Option<&str> {
        self.block.property("id")
    }

    /// Returns sides in source order.
    #[must_use]
    pub fn sides(&self) -> &[VmfSide] {
        &self.sides
    }

    /// Returns unknown child blocks in source order.
    #[must_use]
    pub fn other_blocks(&self) -> &[VmfBlock] {
        &self.other_blocks
    }
}

/// Typed brush side preserving geometry-bearing VMF fields as original text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VmfSide {
    block: VmfBlock,
}

impl VmfSide {
    /// Returns the full generic side block.
    #[must_use]
    pub fn block(&self) -> &VmfBlock {
        &self.block
    }

    /// Returns the source plane equation string.
    #[must_use]
    pub fn plane(&self) -> Option<&str> {
        self.block.property("plane")
    }

    /// Returns the material path string.
    #[must_use]
    pub fn material(&self) -> Option<&str> {
        self.block.property("material")
    }

    /// Returns the U texture-axis string.
    #[must_use]
    pub fn uaxis(&self) -> Option<&str> {
        self.block.property("uaxis")
    }

    /// Returns the V texture-axis string.
    #[must_use]
    pub fn vaxis(&self) -> Option<&str> {
        self.block.property("vaxis")
    }

    /// Returns the lightmap scale string.
    #[must_use]
    pub fn lightmap_scale(&self) -> Option<&str> {
        self.block.property("lightmapscale")
    }
}

/// Location-aware VMF parse failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VmfParseError {
    kind: VmfParseErrorKind,
    line: usize,
    column: usize,
}

impl VmfParseError {
    fn new(kind: VmfParseErrorKind, line: usize, column: usize) -> Self {
        Self { kind, line, column }
    }

    /// Returns the structured failure kind.
    #[must_use]
    pub const fn kind(&self) -> &VmfParseErrorKind {
        &self.kind
    }

    /// Returns one-based source line.
    #[must_use]
    pub const fn line(&self) -> usize {
        self.line
    }

    /// Returns one-based UTF-8 scalar column.
    #[must_use]
    pub const fn column(&self) -> usize {
        self.column
    }
}

impl fmt::Display for VmfParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "VMF parse error at {}:{}: {}",
            self.line, self.column, self.kind
        )
    }
}

impl Error for VmfParseError {}

/// Structured reason for a VMF parse failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VmfParseErrorKind {
    /// A character cannot start any supported token.
    UnexpectedCharacter {
        /// Rejected character.
        character: char,
    },
    /// A quoted string reached end of input before its closing quote.
    UnclosedString,
    /// A string escape was not one of the supported escape forms.
    InvalidEscape {
        /// Rejected escaped character.
        character: char,
    },
    /// A block was not closed before end of input.
    UnclosedBlock {
        /// Name of the still-open block.
        block: String,
    },
    /// A token different from the grammar requirement was encountered.
    Expected {
        /// Human-readable expected token category.
        expected: &'static str,
    },
    /// A resource limit was exceeded.
    LimitExceeded {
        /// Limited resource.
        limit: VmfLimit,
        /// Configured maximum.
        maximum: usize,
    },
}

impl fmt::Display for VmfParseErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedCharacter { character } => {
                write!(formatter, "unexpected character {character:?}")
            }
            Self::UnclosedString => formatter.write_str("unclosed quoted string"),
            Self::InvalidEscape { character } => {
                write!(formatter, "unsupported string escape \\{character}")
            }
            Self::UnclosedBlock { block } => write!(formatter, "unclosed block {block}"),
            Self::Expected { expected } => write!(formatter, "expected {expected}"),
            Self::LimitExceeded { limit, maximum } => {
                write!(formatter, "{limit:?} exceeds configured limit {maximum}")
            }
        }
    }
}

/// A bounded resource tracked by `VmfLimits`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VmfLimit {
    /// Input UTF-8 bytes.
    InputBytes,
    /// Lexical tokens.
    Tokens,
    /// Nested blocks.
    Depth,
    /// Decoded string bytes.
    StringBytes,
    /// Parsed blocks.
    Blocks,
    /// Parsed properties.
    Properties,
}

#[derive(Clone, Debug)]
struct Token {
    kind: TokenKind,
    line: usize,
    column: usize,
}

#[derive(Clone, Debug)]
enum TokenKind {
    Identifier(String),
    String(String),
    OpenBrace,
    CloseBrace,
}

struct Lexer<'a> {
    source: &'a str,
    offset: usize,
    line: usize,
    column: usize,
    limits: VmfLimits,
    token_count: usize,
    permissive_scalars: bool,
}

impl<'a> Lexer<'a> {
    fn new(source: &'a str, limits: VmfLimits, permissive_scalars: bool) -> Self {
        Self {
            source,
            offset: 0,
            line: 1,
            column: 1,
            limits,
            token_count: 0,
            permissive_scalars,
        }
    }

    fn tokenize(mut self) -> Result<Vec<Token>, VmfParseError> {
        let mut tokens = Vec::new();
        while self.peek().is_some() {
            self.skip_ignored();
            let Some(character) = self.peek() else { break };
            let line = self.line;
            let column = self.column;
            let kind = match character {
                '{' => {
                    self.take();
                    TokenKind::OpenBrace
                }
                '}' => {
                    self.take();
                    TokenKind::CloseBrace
                }
                '"' => TokenKind::String(self.string()?),
                _ if self.permissive_scalars => TokenKind::Identifier(self.bare_scalar()),
                character if is_identifier_start(character) => {
                    TokenKind::Identifier(self.identifier())
                }
                character => {
                    return Err(self.error(VmfParseErrorKind::UnexpectedCharacter { character }));
                }
            };
            self.token_count += 1;
            if self.token_count > self.limits.max_tokens {
                return Err(self.error(VmfParseErrorKind::LimitExceeded {
                    limit: VmfLimit::Tokens,
                    maximum: self.limits.max_tokens,
                }));
            }
            tokens.push(Token { kind, line, column });
        }
        Ok(tokens)
    }

    fn skip_ignored(&mut self) {
        loop {
            while self.peek().is_some_and(char::is_whitespace) {
                self.take();
            }
            if self.source[self.offset..].starts_with("//") {
                while let Some(character) = self.take() {
                    if character == '\n' {
                        break;
                    }
                }
            } else {
                break;
            }
        }
    }

    fn string(&mut self) -> Result<String, VmfParseError> {
        self.take();
        let mut value = String::new();
        loop {
            let Some(character) = self.take() else {
                return Err(self.error(VmfParseErrorKind::UnclosedString));
            };
            match character {
                '"' => return Ok(value),
                '\\' => {
                    let Some(escaped) = self.take() else {
                        return Err(self.error(VmfParseErrorKind::UnclosedString));
                    };
                    match escaped {
                        '"' => value.push('"'),
                        '\\' => value.push('\\'),
                        'n' => value.push('\n'),
                        'r' => value.push('\r'),
                        't' => value.push('\t'),
                        character => {
                            return Err(self.error(VmfParseErrorKind::InvalidEscape { character }));
                        }
                    }
                }
                character => value.push(character),
            }
            if value.len() > self.limits.max_string_bytes {
                return Err(self.error(VmfParseErrorKind::LimitExceeded {
                    limit: VmfLimit::StringBytes,
                    maximum: self.limits.max_string_bytes,
                }));
            }
        }
    }

    fn identifier(&mut self) -> String {
        let start = self.offset;
        while self.peek().is_some_and(is_identifier_continue) {
            self.take();
        }
        self.source[start..self.offset].to_owned()
    }

    fn bare_scalar(&mut self) -> String {
        let start = self.offset;
        while self.peek().is_some_and(|character| {
            !character.is_whitespace() && !matches!(character, '{' | '}' | '"')
        }) {
            self.take();
        }
        self.source[start..self.offset].to_owned()
    }

    fn peek(&self) -> Option<char> {
        self.source[self.offset..].chars().next()
    }

    fn take(&mut self) -> Option<char> {
        let character = self.peek()?;
        self.offset += character.len_utf8();
        if character == '\n' {
            self.line += 1;
            self.column = 1;
        } else {
            self.column += 1;
        }
        Some(character)
    }

    fn error(&self, kind: VmfParseErrorKind) -> VmfParseError {
        VmfParseError::new(kind, self.line, self.column)
    }
}

fn is_identifier_start(character: char) -> bool {
    character.is_ascii_alphabetic() || character == '_'
}

fn is_identifier_continue(character: char) -> bool {
    is_identifier_start(character) || character.is_ascii_digit() || matches!(character, '-' | '.')
}

struct Parser {
    tokens: Vec<Token>,
    cursor: usize,
    limits: VmfLimits,
    blocks: usize,
    properties: usize,
    permissive_scalars: bool,
}

impl Parser {
    fn new(tokens: Vec<Token>, limits: VmfLimits, permissive_scalars: bool) -> Self {
        Self {
            tokens,
            cursor: 0,
            limits,
            blocks: 0,
            properties: 0,
            permissive_scalars,
        }
    }

    fn parse_map(&mut self) -> Result<VmfMap, VmfParseError> {
        let mut map = VmfMap::default();
        while self.current().is_some() {
            let block = self.block(1)?;
            match block.name.as_str() {
                "world" if map.world.is_none() => map.world = Some(entity_from_block(block)),
                "entity" => map.entities.push(entity_from_block(block)),
                _ => map.other_blocks.push(block),
            }
        }
        Ok(map)
    }

    fn block(&mut self, depth: usize) -> Result<VmfBlock, VmfParseError> {
        if depth > self.limits.max_depth {
            return Err(self.current_error(VmfParseErrorKind::LimitExceeded {
                limit: VmfLimit::Depth,
                maximum: self.limits.max_depth,
            }));
        }
        let name = self.take_block_name()?;
        let opening = self.take_open_brace()?;
        self.blocks += 1;
        if self.blocks > self.limits.max_blocks {
            return Err(self.at(
                &opening,
                VmfParseErrorKind::LimitExceeded {
                    limit: VmfLimit::Blocks,
                    maximum: self.limits.max_blocks,
                },
            ));
        }
        let mut properties = Vec::new();
        let mut blocks = Vec::new();
        loop {
            let Some(token) = self.current().cloned() else {
                return Err(self.at(&opening, VmfParseErrorKind::UnclosedBlock { block: name }));
            };
            match token.kind {
                TokenKind::CloseBrace => {
                    self.cursor += 1;
                    break;
                }
                TokenKind::String(ref key) if !self.permissive_scalars => {
                    self.cursor += 1;
                    let value = self.take_string()?;
                    self.properties += 1;
                    if self.properties > self.limits.max_properties {
                        return Err(self.at(
                            &token,
                            VmfParseErrorKind::LimitExceeded {
                                limit: VmfLimit::Properties,
                                maximum: self.limits.max_properties,
                            },
                        ));
                    }
                    properties.push(VmfProperty {
                        key: key.clone(),
                        value,
                    });
                }
                TokenKind::String(ref key) | TokenKind::Identifier(ref key)
                    if self.permissive_scalars && !self.next_is_open_brace() =>
                {
                    self.cursor += 1;
                    let value = self.take_scalar()?;
                    self.properties += 1;
                    if self.properties > self.limits.max_properties {
                        return Err(self.at(
                            &token,
                            VmfParseErrorKind::LimitExceeded {
                                limit: VmfLimit::Properties,
                                maximum: self.limits.max_properties,
                            },
                        ));
                    }
                    properties.push(VmfProperty {
                        key: key.clone(),
                        value,
                    });
                }
                TokenKind::String(_) if self.permissive_scalars => {
                    blocks.push(self.block(depth + 1)?);
                }
                TokenKind::Identifier(_) => blocks.push(self.block(depth + 1)?),
                TokenKind::String(_) => unreachable!("strict quoted property handled above"),
                TokenKind::OpenBrace => {
                    return Err(self.at(
                        &token,
                        VmfParseErrorKind::Expected {
                            expected: "block name or quoted property key",
                        },
                    ));
                }
            }
        }
        Ok(VmfBlock {
            name,
            properties,
            blocks,
        })
    }

    fn take_identifier(&mut self) -> Result<String, VmfParseError> {
        let token = self.take_token().ok_or_else(|| {
            self.current_error(VmfParseErrorKind::Expected {
                expected: "block name",
            })
        })?;
        match token.kind {
            TokenKind::Identifier(value) => Ok(value),
            _ => Err(self.at(
                &token,
                VmfParseErrorKind::Expected {
                    expected: "unquoted block name",
                },
            )),
        }
    }

    fn take_block_name(&mut self) -> Result<String, VmfParseError> {
        if !self.permissive_scalars {
            return self.take_identifier();
        }
        let token = self.take_token().ok_or_else(|| {
            self.current_error(VmfParseErrorKind::Expected {
                expected: "block name",
            })
        })?;
        match token.kind {
            TokenKind::Identifier(value) | TokenKind::String(value) => Ok(value),
            _ => Err(self.at(
                &token,
                VmfParseErrorKind::Expected {
                    expected: "block name",
                },
            )),
        }
    }

    fn take_open_brace(&mut self) -> Result<Token, VmfParseError> {
        let token = self.take_token().ok_or_else(|| {
            self.current_error(VmfParseErrorKind::Expected {
                expected: "opening brace",
            })
        })?;
        matches!(token.kind, TokenKind::OpenBrace)
            .then_some(token.clone())
            .ok_or_else(|| {
                self.at(
                    &token,
                    VmfParseErrorKind::Expected {
                        expected: "opening brace",
                    },
                )
            })
    }

    fn take_string(&mut self) -> Result<String, VmfParseError> {
        let token = self.take_token().ok_or_else(|| {
            self.current_error(VmfParseErrorKind::Expected {
                expected: "quoted property value",
            })
        })?;
        match token.kind {
            TokenKind::String(value) => Ok(value),
            _ => Err(self.at(
                &token,
                VmfParseErrorKind::Expected {
                    expected: "quoted property value",
                },
            )),
        }
    }

    fn take_scalar(&mut self) -> Result<String, VmfParseError> {
        if !self.permissive_scalars {
            return self.take_string();
        }
        let token = self.take_token().ok_or_else(|| {
            self.current_error(VmfParseErrorKind::Expected {
                expected: "property value",
            })
        })?;
        match token.kind {
            TokenKind::String(value) | TokenKind::Identifier(value) => Ok(value),
            _ => Err(self.at(
                &token,
                VmfParseErrorKind::Expected {
                    expected: "property value",
                },
            )),
        }
    }

    fn next_is_open_brace(&self) -> bool {
        self.tokens
            .get(self.cursor + 1)
            .is_some_and(|token| matches!(&token.kind, TokenKind::OpenBrace))
    }

    fn current(&self) -> Option<&Token> {
        self.tokens.get(self.cursor)
    }
    fn take_token(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.cursor)?.clone();
        self.cursor += 1;
        Some(token)
    }
    fn current_error(&self, kind: VmfParseErrorKind) -> VmfParseError {
        if let Some(token) = self.current() {
            self.at(token, kind)
        } else {
            VmfParseError::new(kind, 1, 1)
        }
    }
    #[allow(
        clippy::unused_self,
        reason = "Keeping location construction as a parser method makes error sites uniform."
    )]
    fn at(&self, token: &Token, kind: VmfParseErrorKind) -> VmfParseError {
        VmfParseError::new(kind, token.line, token.column)
    }
}

fn entity_from_block(block: VmfBlock) -> VmfEntity {
    let solids = block
        .blocks
        .iter()
        .filter(|block| block.name == "solid")
        .cloned()
        .map(solid_from_block)
        .collect();
    let other_blocks = block
        .blocks
        .iter()
        .filter(|block| block.name != "solid")
        .cloned()
        .collect();
    VmfEntity {
        block,
        solids,
        other_blocks,
    }
}

fn solid_from_block(block: VmfBlock) -> VmfSolid {
    let sides = block
        .blocks
        .iter()
        .filter(|block| block.name == "side")
        .cloned()
        .map(|block| VmfSide { block })
        .collect();
    let other_blocks = block
        .blocks
        .iter()
        .filter(|block| block.name != "side")
        .cloned()
        .collect();
    VmfSolid {
        block,
        sides,
        other_blocks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAP: &str = r#"
// Source 1 VMF fixture
world
{
  "id" "1"
  "classname" "worldspawn"
  solid
  {
    "id" "2"
    side
    {
      "id" "3"
      "plane" "(0 0 0) (0 1 0) (1 1 0)"
      "material" "BRICK/WALL"
      "uaxis" "[1 0 0 0] 0.25"
      "vaxis" "[0 -1 0 0] 0.25"
    }
    editor { "color" "220 30 220" }
  }
}
entity
{
  "id" "4"
  "classname" "prop_static"
  "origin" "1 2 3"
  "model" "models/a.vmdl"
}
"#;

    #[test]
    fn parses_world_entity_and_brush_geometry() {
        let map = parse(MAP).expect("valid VMF");
        let world = map.world().expect("world");
        assert_eq!(world.classname(), Some("worldspawn"));
        let side = &world.solids()[0].sides()[0];
        assert_eq!(side.material(), Some("BRICK/WALL"));
        assert_eq!(side.plane(), Some("(0 0 0) (0 1 0) (1 1 0)"));
        assert_eq!(map.entities()[0].origin(), Some("1 2 3"));
        assert_eq!(world.solids()[0].other_blocks()[0].name(), "editor");
    }

    #[test]
    fn comments_and_supported_escapes_are_unambiguous() {
        let map = parse("entity { \"classname\" \"info\\\\\\\"target // literal\" } // comment")
            .expect("valid");
        assert_eq!(
            map.entities()[0].classname(),
            Some("info\\\"target // literal")
        );
    }

    #[test]
    fn reports_budget_and_malformed_syntax() {
        let limits = VmfLimits {
            max_depth: 1,
            ..VmfLimits::default()
        };
        let depth = parse_with_limits("world { solid { } }", limits).expect_err("depth");
        assert!(matches!(
            depth.kind(),
            VmfParseErrorKind::LimitExceeded {
                limit: VmfLimit::Depth,
                ..
            }
        ));
        let unclosed = parse("world { \"id\" \"1\"").expect_err("unclosed");
        assert!(matches!(
            unclosed.kind(),
            VmfParseErrorKind::UnclosedBlock { .. }
        ));
        let invalid = parse("world { id \"1\" }").expect_err("unquoted key");
        assert!(matches!(invalid.kind(), VmfParseErrorKind::Expected { .. }));
    }
}
