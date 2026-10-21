//! Strict readers for the data-driven world generation JSON: every object
//! declares its fields, and an unknown one is an error.

use serde_json::{Map, Value};
use tuff_vanilla_data::{Block, BlockState};

use crate::surface::GenContext;
use crate::tags::BlockSet;

pub type Result<T> = std::result::Result<T, String>;

pub struct Obj<'v> {
    map: &'v Map<String, Value>,
    what: &'v str,
}

pub fn obj<'v>(value: &'v Value, fields: &[&str], what: &'v str) -> Result<Obj<'v>> {
    let map = value
        .as_object()
        .ok_or_else(|| format!("{what}: expected an object, got {value}"))?;
    for key in map.keys() {
        if key != "type" && !fields.contains(&key.as_str()) {
            return Err(format!("{what}: unknown field {key}"));
        }
    }
    Ok(Obj { map, what })
}

pub fn kind(value: &Value) -> Result<&str> {
    value
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("expected a typed object, got {value}"))
}

pub fn qualify(name: &str) -> String {
    if name.contains(':') {
        name.to_string()
    } else {
        format!("minecraft:{name}")
    }
}

impl<'v> Obj<'v> {
    pub fn get(&self, key: &str) -> Option<&'v Value> {
        self.map.get(key)
    }

    pub fn req(&self, key: &str) -> Result<&'v Value> {
        self.map
            .get(key)
            .ok_or_else(|| format!("{}: missing {key}", self.what))
    }

    pub fn f64(&self, key: &str) -> Result<f64> {
        self.req(key)?
            .as_f64()
            .ok_or_else(|| format!("{}: {key} is not a number", self.what))
    }

    pub fn f64_or(&self, key: &str, default: f64) -> Result<f64> {
        match self.get(key) {
            None => Ok(default),
            Some(_) => self.f64(key),
        }
    }

    pub fn f32(&self, key: &str) -> Result<f32> {
        Ok(self.f64(key)? as f32)
    }

    pub fn f32_or(&self, key: &str, default: f32) -> Result<f32> {
        Ok(self.f64_or(key, default as f64)? as f32)
    }

    pub fn i32(&self, key: &str) -> Result<i32> {
        self.req(key)?
            .as_i64()
            .map(|v| v as i32)
            .ok_or_else(|| format!("{}: {key} is not an integer", self.what))
    }

    pub fn i32_or(&self, key: &str, default: i32) -> Result<i32> {
        match self.get(key) {
            None => Ok(default),
            Some(_) => self.i32(key),
        }
    }

    pub fn bool(&self, key: &str) -> Result<bool> {
        self.req(key)?
            .as_bool()
            .ok_or_else(|| format!("{}: {key} is not a boolean", self.what))
    }

    pub fn bool_or(&self, key: &str, default: bool) -> Result<bool> {
        match self.get(key) {
            None => Ok(default),
            Some(_) => self.bool(key),
        }
    }

    pub fn str(&self, key: &str) -> Result<&'v str> {
        self.req(key)?
            .as_str()
            .ok_or_else(|| format!("{}: {key} is not a string", self.what))
    }

    pub fn list(&self, key: &str) -> Result<&'v [Value]> {
        self.req(key)?
            .as_array()
            .map(Vec::as_slice)
            .ok_or_else(|| format!("{}: {key} is not a list", self.what))
    }
}

/// A block state: a block name for its default state, or `{id or Name,
/// properties or Properties}`.
pub fn block_state(value: &Value) -> Result<BlockState> {
    if let Some(name) = value.as_str() {
        let block =
            Block::from_name(&qualify(name)).ok_or_else(|| format!("unknown block {name}"))?;
        return Ok(block.default_state());
    }
    let o = obj(
        value,
        &["id", "properties", "Name", "Properties"],
        "block state",
    )?;
    let name = o
        .get("id")
        .or_else(|| o.get("Name"))
        .and_then(Value::as_str)
        .ok_or("block state without id")?;
    let block = Block::from_name(&qualify(name)).ok_or_else(|| format!("unknown block {name}"))?;
    let mut state = block.default_state();
    if let Some(properties) = o.get("properties").or_else(|| o.get("Properties")) {
        for (key, value) in properties
            .as_object()
            .ok_or("properties is not an object")?
        {
            let value = value.as_str().ok_or("property values are strings")?;
            state = state
                .with(key, value)
                .ok_or_else(|| format!("{name} has no {key}={value}"))?;
        }
    }
    Ok(state)
}

pub fn block(value: &Value) -> Result<Block> {
    let name = value.as_str().ok_or("blocks are named")?;
    Block::from_name(&qualify(name)).ok_or_else(|| format!("unknown block {name}"))
}

/// A block list: a single name, a list of names, or `#tag` (its members
/// as built in).
pub fn blocks(value: &Value) -> Result<Vec<Block>> {
    match value {
        Value::String(name) if name.starts_with('#') => {
            let name = qualify(&name[1..]);
            tuff_vanilla_data::tag::block::ALL
                .iter()
                .find(|(tag, _)| *tag == name)
                .map(|(_, blocks)| blocks.to_vec())
                .ok_or_else(|| format!("unknown block tag {name}"))
        }
        Value::String(_) => Ok(vec![block(value)?]),
        Value::Array(list) => list.iter().map(block).collect(),
        _ => Err(format!("bad block list {value}")),
    }
}

/// [`blocks`] as a set, where a `#tag` is read from the tags in effect.
pub fn block_set(value: &Value) -> Result<BlockSet> {
    match value {
        Value::String(name) if name.starts_with('#') => tag_blocks(&name[1..]),
        _ => Ok(BlockSet::new(&blocks(value)?)),
    }
}

pub fn tag_blocks(name: &str) -> Result<BlockSet> {
    let name = qualify(name);
    BlockSet::tag(&name).ok_or_else(|| format!("unknown block tag {name}"))
}

pub fn vec3i(value: &Value) -> Result<[i32; 3]> {
    let list = value.as_array().ok_or("positions are lists")?;
    if list.len() != 3 {
        return Err("positions have three coordinates".into());
    }
    let mut out = [0; 3];
    for (i, v) in list.iter().enumerate() {
        out[i] = v.as_i64().ok_or("coordinates are integers")? as i32;
    }
    Ok(out)
}

pub fn anchor(value: &Value) -> Result<tuff_vanilla_data::worldgen::VerticalAnchor> {
    use tuff_vanilla_data::worldgen::VerticalAnchor;
    let map = value.as_object().ok_or("anchors are objects")?;
    let (key, v) = map.iter().next().ok_or("empty anchor")?;
    let v = v.as_i64().ok_or("anchor offsets are integers")? as i32;
    Ok(match key.as_str() {
        "absolute" => VerticalAnchor::Absolute(v),
        "above_bottom" => VerticalAnchor::AboveBottom(v),
        "below_top" => VerticalAnchor::BelowTop(v),
        "relative_to_sea_level" => VerticalAnchor::RelativeToSeaLevel(v),
        other => return Err(format!("unknown anchor {other}")),
    })
}

pub fn resolve(span: GenContext, value: &Value) -> Result<i32> {
    Ok(span.resolve(anchor(value)?))
}
