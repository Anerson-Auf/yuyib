//! TMX / TSX XML → shared [`RawMap`] / [`RawTileset`] front-end.

use roxmltree::{Document, Node};

use super::{
    RawLayer, RawMap, RawProperty, RawTile, TiledImportError, objects, objects::RawObject,
    strip_utf8_bom,
};
use crate::tileset::RawTileset;

/// True when `bytes` look like a Tiled XML document (after BOM strip).
#[must_use]
pub(crate) fn looks_like_xml(bytes: &[u8]) -> bool {
    let bytes = trim_ascii_start(strip_utf8_bom(bytes));
    bytes.starts_with(b"<?xml")
        || bytes.starts_with(b"<map")
        || bytes.starts_with(b"<tileset")
}

/// Parses a TMX map document into the shared raw map model.
pub(crate) fn parse_tmx_map(bytes: &[u8]) -> Result<RawMap, TiledImportError> {
    let document = parse_document(bytes)?;
    let root = document.root_element();
    if root.tag_name().name() != "map" {
        return Err(TiledImportError::Unsupported(
            "tmx root element must be map",
        ));
    }
    if attr_bool(root, "infinite").unwrap_or(false) {
        return Err(TiledImportError::Unsupported("infinite tmx maps"));
    }

    let mut tilesets = Vec::new();
    let mut layers = Vec::new();
    for child in root.children().filter(Node::is_element) {
        match child.tag_name().name() {
            "tileset" => tilesets.push(parse_tileset_element(child, true)?),
            "layer" => layers.push(parse_tile_layer(child)?),
            "objectgroup" => layers.push(parse_object_group(child)?),
            "imagelayer" | "group" => {
                return Err(TiledImportError::Unsupported(
                    "tmx imagelayer/group are not supported in this slice",
                ));
            }
            _ => {}
        }
    }

    Ok(RawMap {
        map_type: Some("map".into()),
        orientation: attr_string(root, "orientation"),
        renderorder: attr_string(root, "renderorder"),
        infinite: Some(false),
        width: require_u32(root, "width")?,
        height: require_u32(root, "height")?,
        tilewidth: require_u32(root, "tilewidth")?,
        tileheight: require_u32(root, "tileheight")?,
        tilesets,
        layers,
    })
}

/// Parses a TSX (or embedded tileset body) XML document.
pub(crate) fn parse_tsx_tileset(bytes: &[u8]) -> Result<RawTileset, TiledImportError> {
    let document = parse_document(bytes)?;
    let root = document.root_element();
    if root.tag_name().name() != "tileset" {
        return Err(TiledImportError::Unsupported(
            "tsx root element must be tileset",
        ));
    }
    let mut tileset = parse_tileset_element(root, false)?;
    tileset.tileset_type = Some("tileset".into());
    Ok(tileset)
}

fn parse_document(bytes: &[u8]) -> Result<Document<'_>, TiledImportError> {
    let text = std::str::from_utf8(strip_utf8_bom(bytes))?;
    Document::parse(text).map_err(TiledImportError::Xml)
}

fn parse_tileset_element(node: Node<'_, '_>, allow_source: bool) -> Result<RawTileset, TiledImportError> {
    let source = attr_string(node, "source");
    if let Some(source) = source {
        if !allow_source {
            return Err(TiledImportError::Unsupported(
                "nested external tileset source is not supported",
            ));
        }
        return Ok(RawTileset {
            firstgid: Some(require_u32(node, "firstgid")?),
            name: attr_string(node, "name"),
            image: None,
            imagewidth: None,
            imageheight: None,
            tilewidth: None,
            tileheight: None,
            tilecount: None,
            columns: None,
            margin: None,
            spacing: None,
            source: Some(source),
            tiles: None,
            tileset_type: None,
        });
    }

    let mut image = None;
    let mut imagewidth = None;
    let mut imageheight = None;
    let mut tiles = Vec::new();
    for child in node.children().filter(Node::is_element) {
        match child.tag_name().name() {
            "image" => {
                image = attr_string(child, "source");
                imagewidth = attr_u32(child, "width")?;
                imageheight = attr_u32(child, "height")?;
            }
            "tile" => tiles.push(parse_tile(child)?),
            "wangsets" | "terraintypes" | "grid" | "transformations" | "tileoffset" => {}
            _ => {}
        }
    }

    Ok(RawTileset {
        firstgid: attr_u32(node, "firstgid")?,
        name: attr_string(node, "name"),
        image,
        imagewidth,
        imageheight,
        tilewidth: attr_u32(node, "tilewidth")?,
        tileheight: attr_u32(node, "tileheight")?,
        tilecount: attr_u32(node, "tilecount")?,
        columns: attr_u32(node, "columns")?,
        margin: attr_u32(node, "margin")?,
        spacing: attr_u32(node, "spacing")?,
        source: None,
        tiles: (!tiles.is_empty()).then_some(tiles),
        tileset_type: None,
    })
}

fn parse_tile(node: Node<'_, '_>) -> Result<RawTile, TiledImportError> {
    let mut animation = None;
    for child in node.children().filter(Node::is_element) {
        if child.tag_name().name() == "animation" {
            let mut frames = Vec::new();
            for frame_node in child.children().filter(Node::is_element) {
                if frame_node.tag_name().name() != "frame" {
                    continue;
                }
                frames.push(super::RawTileAnimFrame {
                    tileid: require_u32(frame_node, "tileid")?,
                    duration: require_u32(frame_node, "duration")?,
                });
            }
            animation = Some(frames);
        }
    }
    Ok(RawTile {
        id: require_u32(node, "id")?,
        properties: parse_properties_child(node)?,
        animation,
    })
}

fn parse_tile_layer(node: Node<'_, '_>) -> Result<RawLayer, TiledImportError> {
    let data_node = node
        .children()
        .find(|child| child.is_element() && child.tag_name().name() == "data")
        .ok_or(TiledImportError::Unsupported(
            "tmx layer requires a data element",
        ))?;
    let data = parse_layer_data(data_node)?;
    Ok(RawLayer {
        layer_type: "tilelayer".into(),
        name: attr_string(node, "name").unwrap_or_default(),
        width: attr_u32(node, "width")?,
        height: attr_u32(node, "height")?,
        data: Some(data),
        visible: attr_bool(node, "visible"),
        parallaxx: attr_f32(node, "parallaxx")?.unwrap_or(1.0),
        parallaxy: attr_f32(node, "parallaxy")?.unwrap_or(1.0),
        objects: None,
    })
}

fn parse_layer_data(node: Node<'_, '_>) -> Result<Vec<u32>, TiledImportError> {
    let encoding = attr_string(node, "encoding").unwrap_or_default();
    let compression = attr_string(node, "compression").unwrap_or_default();
    if encoding.is_empty() {
        if !compression.is_empty() {
            return Err(TiledImportError::Unsupported(
                "compressed tmx XML tile lists are not supported",
            ));
        }
        let mut data = Vec::new();
        for child in node.children().filter(Node::is_element) {
            if child.tag_name().name() != "tile" {
                continue;
            }
            data.push(attr_u32(child, "gid")?.unwrap_or(0));
        }
        return Ok(data);
    }
    if encoding == "csv" {
        if !compression.is_empty() {
            return Err(TiledImportError::Unsupported(
                "compressed tmx csv layer data is not supported",
            ));
        }
        return parse_csv_gids(node.text().unwrap_or(""));
    }
    if encoding == "base64" {
        return parse_base64_gids(node.text().unwrap_or(""), compression.as_str());
    }
    Err(TiledImportError::Unsupported(
        "only csv, base64, or XML tile tmx layer data is supported in this slice",
    ))
}

fn parse_csv_gids(text: &str) -> Result<Vec<u32>, TiledImportError> {
    let mut data = Vec::new();
    for part in text.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let gid = part.parse::<u32>().map_err(|_| {
            TiledImportError::Unsupported("invalid csv gid in tmx layer data")
        })?;
        data.push(gid);
    }
    Ok(data)
}

fn parse_base64_gids(text: &str, compression: &str) -> Result<Vec<u32>, TiledImportError> {
    use std::io::Read;

    let trimmed: String = text.split_whitespace().collect();
    let decoded = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, trimmed)
        .map_err(|_| TiledImportError::Unsupported("invalid base64 tmx layer data"))?;
    let bytes = match compression {
        "" => decoded,
        "zlib" => {
            let mut decoder = flate2::read::ZlibDecoder::new(decoded.as_slice());
            let mut out = Vec::new();
            decoder
                .read_to_end(&mut out)
                .map_err(|_| TiledImportError::Unsupported("invalid zlib tmx layer data"))?;
            out
        }
        "gzip" => {
            let mut decoder = flate2::read::GzDecoder::new(decoded.as_slice());
            let mut out = Vec::new();
            decoder
                .read_to_end(&mut out)
                .map_err(|_| TiledImportError::Unsupported("invalid gzip tmx layer data"))?;
            out
        }
        "zstd" => {
            let mut decoder = ruzstd::decoding::StreamingDecoder::new(decoded.as_slice())
                .map_err(|_| TiledImportError::Unsupported("invalid zstd tmx layer header"))?;
            let mut out = Vec::new();
            decoder
                .read_to_end(&mut out)
                .map_err(|_| TiledImportError::Unsupported("invalid zstd tmx layer data"))?;
            out
        }
        _ => {
            return Err(TiledImportError::Unsupported(
                "only zlib/gzip/zstd tmx compression is supported in this slice",
            ));
        }
    };
    if !bytes.len().is_multiple_of(4) {
        return Err(TiledImportError::Unsupported(
            "base64 tmx layer byte length must be a multiple of 4",
        ));
    }
    let mut data = Vec::with_capacity(bytes.len() / 4);
    for chunk in bytes.chunks_exact(4) {
        data.push(u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    Ok(data)
}

fn parse_object_group(node: Node<'_, '_>) -> Result<RawLayer, TiledImportError> {
    let mut objects = Vec::new();
    for child in node.children().filter(Node::is_element) {
        if child.tag_name().name() == "object" {
            objects.push(parse_object(child)?);
        }
    }
    Ok(RawLayer {
        layer_type: "objectgroup".into(),
        name: attr_string(node, "name").unwrap_or_default(),
        width: None,
        height: None,
        data: None,
        visible: attr_bool(node, "visible"),
        parallaxx: 1.0,
        parallaxy: 1.0,
        objects: Some(objects),
    })
}

fn parse_object(node: Node<'_, '_>) -> Result<RawObject, TiledImportError> {
    let mut point = false;
    let mut ellipse = false;
    let mut polygon = None;
    let mut polyline = None;
    let mut text = None;
    for child in node.children().filter(Node::is_element) {
        match child.tag_name().name() {
            "point" => point = true,
            "ellipse" => ellipse = true,
            "polygon" => {
                let points_text = attr_string(child, "points").unwrap_or_default();
                polygon = Some(objects::parse_tmx_points(&points_text)?);
            }
            "polyline" => polyline = Some(serde_json::Value::Bool(true)),
            "text" => text = Some(serde_json::Value::Bool(true)),
            "properties" => {}
            _ => {}
        }
    }
    Ok(RawObject {
        name: attr_string(node, "name").unwrap_or_default(),
        type_name: attr_string(node, "type").unwrap_or_default(),
        class: attr_string(node, "class").unwrap_or_default(),
        x: attr_f32(node, "x")?.unwrap_or(0.0),
        y: attr_f32(node, "y")?.unwrap_or(0.0),
        width: attr_f32(node, "width")?.unwrap_or(0.0),
        height: attr_f32(node, "height")?.unwrap_or(0.0),
        point,
        ellipse,
        polygon,
        polyline,
        gid: attr_u32(node, "gid")?,
        template: attr_string(node, "template"),
        text,
        properties: parse_properties_child(node)?,
    })
}

fn parse_properties_child(node: Node<'_, '_>) -> Result<Option<Vec<RawProperty>>, TiledImportError> {
    let Some(properties_node) = node
        .children()
        .find(|child| child.is_element() && child.tag_name().name() == "properties")
    else {
        return Ok(None);
    };
    let mut properties = Vec::new();
    for child in properties_node.children().filter(Node::is_element) {
        if child.tag_name().name() != "property" {
            continue;
        }
        properties.push(parse_property(child)?);
    }
    Ok(Some(properties))
}

fn parse_property(node: Node<'_, '_>) -> Result<RawProperty, TiledImportError> {
    let name = attr_string(node, "name").ok_or(TiledImportError::Unsupported(
        "tmx property name is required",
    ))?;
    let type_name = attr_string(node, "type").unwrap_or_else(|| "string".into());
    let raw_value = attr_string(node, "value").or_else(|| {
        node.text()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    });
    let value = match type_name.as_str() {
        "bool" => {
            let text = raw_value.as_deref().unwrap_or("false");
            serde_json::Value::Bool(parse_bool_text(text).unwrap_or(false))
        }
        "int" | "float" => {
            let text = raw_value.as_deref().ok_or(TiledImportError::Unsupported(
                "tmx numeric property requires value",
            ))?;
            let number: f64 = text.parse().map_err(|_| {
                TiledImportError::Unsupported("invalid tmx numeric property")
            })?;
            serde_json::Number::from_f64(number)
                .map(serde_json::Value::Number)
                .ok_or(TiledImportError::Unsupported(
                    "tmx numeric property must be finite",
                ))?
        }
        "string" | "" => serde_json::Value::String(raw_value.unwrap_or_default()),
        _ => {
            return Err(TiledImportError::Unsupported(
                "tmx property types support bool/string/int/float only",
            ));
        }
    };
    Ok(RawProperty { name, value })
}

fn attr_string(node: Node<'_, '_>, name: &str) -> Option<String> {
    node.attribute(name).map(str::to_owned)
}

fn attr_u32(node: Node<'_, '_>, name: &str) -> Result<Option<u32>, TiledImportError> {
    let Some(text) = node.attribute(name) else {
        return Ok(None);
    };
    text.parse::<u32>()
        .map(Some)
        .map_err(|_| TiledImportError::Unsupported("invalid tmx u32 attribute"))
}

fn require_u32(node: Node<'_, '_>, name: &str) -> Result<u32, TiledImportError> {
    attr_u32(node, name)?.ok_or(TiledImportError::Unsupported(
        "required tmx u32 attribute missing",
    ))
}

fn attr_f32(node: Node<'_, '_>, name: &str) -> Result<Option<f32>, TiledImportError> {
    let Some(text) = node.attribute(name) else {
        return Ok(None);
    };
    text.parse::<f32>()
        .map(Some)
        .map_err(|_| TiledImportError::Unsupported("invalid tmx f32 attribute"))
}

fn attr_bool(node: Node<'_, '_>, name: &str) -> Option<bool> {
    node.attribute(name).and_then(parse_bool_text)
}

fn parse_bool_text(text: &str) -> Option<bool> {
    match text {
        "1" | "true" | "True" => Some(true),
        "0" | "false" | "False" => Some(false),
        _ => None,
    }
}

fn trim_ascii_start(bytes: &[u8]) -> &[u8] {
    let mut index = 0;
    while index < bytes.len() && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    &bytes[index..]
}
