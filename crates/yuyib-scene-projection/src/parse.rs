//! Parse entity projection `.rs` files into structured edits.

use serde_json::{Map, Value};

use crate::ENTITY_PROJECTION_SCHEMA;

/// One component block from `yuyib_entity!`.
#[derive(Clone, Debug, PartialEq)]
pub struct ParsedComponent {
    /// Stable schema id (`yuyib.transform3d`).
    pub schema: String,
    /// Schema version from `@ N`.
    pub version: u32,
    /// When true, `payload` came from a `raw { … }` JSON block.
    pub raw: bool,
    /// Component JSON payload.
    pub payload: Value,
}

/// Parsed entity projection file.
#[derive(Clone, Debug, PartialEq)]
pub struct ParsedEntityProjection {
    /// Scene GUID from the header.
    pub scene_guid: String,
    /// Entity GUID from the header.
    pub entity_guid: String,
    /// Optional display name.
    pub name: Option<String>,
    /// Components in file order.
    pub components: Vec<ParsedComponent>,
}

/// Parses one entity projection source file.
///
/// # Errors
///
/// Returns when the header/schema/`yuyib_entity!` body is malformed.
pub fn parse_entity_file(source: &str) -> Result<ParsedEntityProjection, String> {
    let mut scene_guid = None;
    let mut entity_guid = None;
    let mut saw_schema = false;
    for line in source.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("//!") {
            let rest = rest.trim();
            if rest == ENTITY_PROJECTION_SCHEMA {
                saw_schema = true;
                continue;
            }
            if let Some(value) = header_string(rest, "scene_guid") {
                scene_guid = Some(value);
                continue;
            }
            if let Some(value) = header_string(rest, "entity_guid") {
                entity_guid = Some(value);
            }
        }
    }
    if !saw_schema {
        return Err(format!(
            "missing `//! {ENTITY_PROJECTION_SCHEMA}` header"
        ));
    }
    let scene_guid = scene_guid.ok_or_else(|| "missing `//! scene_guid`".to_owned())?;
    let entity_guid = entity_guid.ok_or_else(|| "missing `//! entity_guid`".to_owned())?;

    let body = extract_entity_macro_body(source)?;
    let (name, components_src) = split_name_and_components(&body)?;
    let components = parse_components_block(&components_src)?;
    Ok(ParsedEntityProjection {
        scene_guid,
        entity_guid,
        name,
        components,
    })
}

fn header_string(line: &str, key: &str) -> Option<String> {
    let prefix = format!("{key} = ");
    let rest = line.strip_prefix(&prefix)?.trim();
    parse_quoted(rest).ok()
}

fn extract_entity_macro_body(source: &str) -> Result<String, String> {
    let start = source
        .find("yuyib_entity!")
        .ok_or_else(|| "missing `yuyib_entity!` block".to_owned())?;
    let after = &source[start + "yuyib_entity!".len()..];
    let brace = after
        .find('{')
        .ok_or_else(|| "`yuyib_entity!` is missing `{`".to_owned())?;
    let body_start = &after[brace + 1..];
    let end = find_matching_brace(body_start)
        .ok_or_else(|| "`yuyib_entity!` is missing closing `}`".to_owned())?;
    Ok(body_start[..end].to_owned())
}

fn find_matching_brace(input: &str) -> Option<usize> {
    let mut depth = 0_i32;
    let mut in_string = false;
    let mut escape = false;
    for (index, ch) in input.char_indices() {
        if in_string {
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                if depth == 0 {
                    return Some(index);
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    None
}

fn split_name_and_components(body: &str) -> Result<(Option<String>, String), String> {
    let mut cursor = Skip::new(body);
    cursor.skip_ws_and_comments();
    if cursor.consume_ident("name") {
        cursor.skip_ws_and_comments();
        cursor.expect(':')?;
        cursor.skip_ws_and_comments();
        let name = cursor.parse_string()?;
        cursor.skip_ws_and_comments();
        let _ = cursor.consume(',');
        cursor.skip_ws_and_comments();
        if !cursor.consume_ident("components") {
            return Err("expected `components:` after `name`".to_owned());
        }
        cursor.skip_ws_and_comments();
        cursor.expect(':')?;
        cursor.skip_ws_and_comments();
        let block = cursor.parse_brace_block()?;
        Ok((Some(name).filter(|value| !value.is_empty()), block))
    } else if cursor.consume_ident("components") {
        cursor.skip_ws_and_comments();
        cursor.expect(':')?;
        cursor.skip_ws_and_comments();
        let block = cursor.parse_brace_block()?;
        Ok((None, block))
    } else {
        Err("expected `name:` or `components:` in yuyib_entity!".to_owned())
    }
}

fn parse_components_block(block: &str) -> Result<Vec<ParsedComponent>, String> {
    let mut cursor = Skip::new(block);
    let mut components = Vec::new();
    loop {
        cursor.skip_ws_and_comments();
        if cursor.is_empty() {
            break;
        }
        let schema = cursor.parse_string()?;
        cursor.skip_ws_and_comments();
        cursor.expect('@')?;
        cursor.skip_ws_and_comments();
        let version = cursor.parse_u32()?;
        cursor.skip_ws_and_comments();
        cursor.expect(':')?;
        cursor.skip_ws_and_comments();
        let raw = cursor.consume_ident("raw");
        cursor.skip_ws_and_comments();
        let inner = cursor.parse_brace_block()?;
        let payload = if raw {
            parse_json_object_like(&inner)?
        } else {
            parse_typed_object(&inner)?
        };
        components.push(ParsedComponent {
            schema,
            version,
            raw,
            payload,
        });
        cursor.skip_ws_and_comments();
        let _ = cursor.consume(',');
    }
    Ok(components)
}

fn parse_typed_object(src: &str) -> Result<Value, String> {
    let mut cursor = Skip::new(src);
    let mut map = Map::new();
    loop {
        cursor.skip_ws_and_comments();
        if cursor.is_empty() {
            break;
        }
        let key = cursor.parse_ident_or_string()?;
        cursor.skip_ws_and_comments();
        cursor.expect(':')?;
        cursor.skip_ws_and_comments();
        let value = cursor.parse_value()?;
        map.insert(key, value);
        cursor.skip_ws_and_comments();
        let _ = cursor.consume(',');
    }
    Ok(Value::Object(map))
}

fn parse_json_object_like(src: &str) -> Result<Value, String> {
    let trimmed = src.trim();
    let candidate = if trimmed.starts_with('{') {
        trimmed.to_owned()
    } else {
        format!("{{{trimmed}}}")
    };
    serde_json::from_str(&candidate).map_err(|error| format!("raw JSON: {error}"))
}

fn parse_quoted(input: &str) -> Result<String, String> {
    let mut cursor = Skip::new(input);
    cursor.parse_string()
}

struct Skip<'a> {
    src: &'a str,
    index: usize,
}

impl<'a> Skip<'a> {
    fn new(src: &'a str) -> Self {
        Self { src, index: 0 }
    }

    fn rest(&self) -> &'a str {
        &self.src[self.index..]
    }

    fn is_empty(&mut self) -> bool {
        self.skip_ws_and_comments();
        self.index >= self.src.len()
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.index += ch.len_utf8();
        Some(ch)
    }

    fn skip_ws_and_comments(&mut self) {
        loop {
            while self.peek().is_some_and(char::is_whitespace) {
                self.bump();
            }
            if self.rest().starts_with("//") {
                while let Some(ch) = self.bump() {
                    if ch == '\n' {
                        break;
                    }
                }
                continue;
            }
            break;
        }
    }

    fn consume(&mut self, expected: char) -> bool {
        self.skip_ws_and_comments();
        if self.peek() == Some(expected) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, expected: char) -> Result<(), String> {
        if self.consume(expected) {
            Ok(())
        } else {
            Err(format!(
                "expected `{expected}`, found {:?}",
                self.peek().unwrap_or('\0')
            ))
        }
    }

    fn consume_ident(&mut self, ident: &str) -> bool {
        self.skip_ws_and_comments();
        if self.rest().starts_with(ident) {
            let after = &self.rest()[ident.len()..];
            if after
                .chars()
                .next()
                .is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_')
            {
                return false;
            }
            self.index += ident.len();
            true
        } else {
            false
        }
    }

    fn parse_ident_or_string(&mut self) -> Result<String, String> {
        self.skip_ws_and_comments();
        if self.peek() == Some('"') {
            return self.parse_string();
        }
        let rest = self.rest();
        let end = rest
            .char_indices()
            .find(|(_, ch)| !(ch.is_ascii_alphanumeric() || *ch == '_'))
            .map(|(index, _)| index)
            .unwrap_or(rest.len());
        if end == 0 {
            return Err("expected identifier".to_owned());
        }
        let ident = rest[..end].to_owned();
        self.index += end;
        Ok(ident)
    }

    fn parse_string(&mut self) -> Result<String, String> {
        self.skip_ws_and_comments();
        if self.bump() != Some('"') {
            return Err("expected string".to_owned());
        }
        let mut out = String::new();
        loop {
            match self.bump() {
                None => return Err("unterminated string".to_owned()),
                Some('"') => return Ok(out),
                Some('\\') => match self.bump() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('r') => out.push('\r'),
                    Some('\\') => out.push('\\'),
                    Some('"') => out.push('"'),
                    Some(other) => out.push(other),
                    None => return Err("unterminated escape".to_owned()),
                },
                Some(ch) => out.push(ch),
            }
        }
    }

    fn parse_u32(&mut self) -> Result<u32, String> {
        self.skip_ws_and_comments();
        let rest = self.rest();
        let end = rest
            .char_indices()
            .find(|(_, ch)| !ch.is_ascii_digit())
            .map(|(index, _)| index)
            .unwrap_or(rest.len());
        if end == 0 {
            return Err("expected integer".to_owned());
        }
        let number = rest[..end]
            .parse::<u32>()
            .map_err(|error| error.to_string())?;
        self.index += end;
        Ok(number)
    }

    fn parse_brace_block(&mut self) -> Result<String, String> {
        self.skip_ws_and_comments();
        self.expect('{')?;
        let start = self.index;
        let relative = find_matching_brace(self.rest())
            .ok_or_else(|| "missing closing `}`".to_owned())?;
        let body = self.src[start..start + relative].to_owned();
        self.index = start + relative + 1;
        Ok(body)
    }

    fn parse_value(&mut self) -> Result<Value, String> {
        self.skip_ws_and_comments();
        match self.peek() {
            Some('"') => Ok(Value::String(self.parse_string()?)),
            Some('[') => self.parse_array(),
            Some('{') => {
                let body = self.parse_brace_block()?;
                parse_typed_object(&body)
            }
            Some('n') if self.rest().starts_with("null") => {
                self.index += 4;
                Ok(Value::Null)
            }
            Some('t') if self.rest().starts_with("true") => {
                self.index += 4;
                Ok(Value::Bool(true))
            }
            Some('f') if self.rest().starts_with("false") => {
                self.index += 5;
                Ok(Value::Bool(false))
            }
            Some(ch) if ch == '-' || ch.is_ascii_digit() => self.parse_number(),
            other => Err(format!("unexpected value start {other:?}")),
        }
    }

    fn parse_array(&mut self) -> Result<Value, String> {
        self.expect('[')?;
        let mut items = Vec::new();
        loop {
            self.skip_ws_and_comments();
            if self.consume(']') {
                break;
            }
            items.push(self.parse_value()?);
            self.skip_ws_and_comments();
            if self.consume(',') {
                continue;
            }
            self.expect(']')?;
            break;
        }
        Ok(Value::Array(items))
    }

    fn parse_number(&mut self) -> Result<Value, String> {
        self.skip_ws_and_comments();
        let rest = self.rest();
        let end = rest
            .char_indices()
            .find(|(_, ch)| !(ch.is_ascii_digit() || matches!(ch, '-' | '+' | '.' | 'e' | 'E')))
            .map(|(index, _)| index)
            .unwrap_or(rest.len());
        if end == 0 {
            return Err("expected number".to_owned());
        }
        let text = &rest[..end];
        self.index += end;
        if let Ok(int) = text.parse::<i64>() {
            return Ok(Value::Number(int.into()));
        }
        let float: f64 = text
            .parse()
            .map_err(|error| format!("invalid number `{text}`: {error}"))?;
        serde_json::Number::from_f64(float)
            .map(Value::Number)
            .ok_or_else(|| format!("non-finite number `{text}`"))
    }
}
