//! Text components: JSON strings became NBT structures, legacy hover events
//! were replaced, stringly flags became booleans, and hover and click events
//! were renamed with their fields.

use serde_json::Value;

use super::Fixer;
use tuff_nbt::{Compound, Tag};

/// Deepest JSON text component converted to NBT; anything deeper stays a
/// plain string.
pub const MAX_JSON_DEPTH: usize = 64;
const MAX_TREE_DEPTH: usize = 128;
const FLAGS: [&str; 5] = [
    "bold",
    "italic",
    "underlined",
    "strikethrough",
    "obfuscated",
];

/// Upgrades one text component in place.
pub fn upgrade(tag: &mut Tag, fixer: Fixer) {
    if fixer.due(4290, 0)
        && let Tag::String(json) = tag
    {
        *tag = unflatten(json);
    }
    let legacy = fixer.due(4291, 0);
    let events = fixer.due(4292, 0);
    if legacy || events {
        fix_tree(tag, legacy, events, 0);
    }
    if let Some(items) = fixer.nested() {
        fix_hover_items(tag, items.at_least(4290), 0);
    }
}

/// Upgrades the items shown by `show_item` hover events. Text was a JSON
/// string before 4290, out of reach of item fixes until then.
fn fix_hover_items(tag: &mut Tag, items: Fixer, depth: usize) {
    if depth > MAX_TREE_DEPTH {
        return;
    }
    let component = match tag {
        Tag::List(list) => {
            for element in list {
                fix_hover_items(element, items, depth + 1);
            }
            return;
        }
        Tag::Compound(component) => component,
        _ => return,
    };
    if let Some(Tag::List(extra)) = component.get_mut("extra") {
        for element in extra {
            fix_hover_items(element, items, depth + 1);
        }
    }
    if let Some(separator) = component.get_mut("separator") {
        fix_hover_items(separator, items, depth + 1);
    }
    if let Some(Tag::Compound(hover)) = component.get_mut("hover_event") {
        match hover.get_str("action") {
            Some("show_item") => super::item::upgrade(hover, items),
            Some("show_text") => {
                if let Some(value) = hover.get_mut("value") {
                    fix_hover_items(value, items, depth + 1);
                }
            }
            Some("show_entity") => {
                if let Some(name) = hover.get_mut("name") {
                    fix_hover_items(name, items, depth + 1);
                }
            }
            _ => {}
        }
    }
    if let Some(Tag::Compound(hover)) = component.get_mut("hoverEvent") {
        let action = hover.get_str("action").map(str::to_owned);
        match (action.as_deref(), hover.get_mut("contents")) {
            (Some("show_item"), Some(Tag::Compound(item))) => super::item::upgrade(item, items),
            (Some("show_text"), Some(contents)) => fix_hover_items(contents, items, depth + 1),
            (Some("show_entity"), Some(Tag::Compound(entity))) => {
                if let Some(name) = entity.get_mut("name") {
                    fix_hover_items(name, items, depth + 1);
                }
            }
            _ => {}
        }
    }
}

/// Upgrades the component at `key`, if present.
pub fn upgrade_field(compound: &mut Compound, key: &str, fixer: Fixer) {
    if let Some(tag) = compound.get_mut(key) {
        upgrade(tag, fixer);
    }
}

/// Upgrades every component of the list at `key`.
pub fn upgrade_list(compound: &mut Compound, key: &str, fixer: Fixer) {
    if let Some(list) = compound.get_list_mut(key) {
        for tag in list {
            upgrade(tag, fixer);
        }
    }
}

/// A JSON text component as NBT, or the text itself when it is not JSON.
pub fn unflatten(json: &str) -> Tag {
    let parsed = serde_json::from_str::<Value>(json)
        .ok()
        .or_else(|| Lenient::parse(json));
    match parsed {
        None | Some(Value::Null) => Tag::String(json.to_owned()),
        Some(value) => json_to_nbt(&value, 0).unwrap_or_else(|| Tag::String(json.to_owned())),
    }
}

/// A parser for the relaxed JSON dialect the original text component fixer
/// accepts: comments, single-quoted and unquoted strings, `=`, `=>` and `;`
/// separators, and omitted array elements.
struct Lenient<'a> {
    src: &'a [u8],
    pos: usize,
}

impl<'a> Lenient<'a> {
    fn parse(json: &'a str) -> Option<Value> {
        let mut parser = Lenient {
            src: json.as_bytes(),
            pos: 0,
        };
        let value = parser.value(0)?;
        parser.skip();
        (parser.pos == parser.src.len()).then_some(value)
    }

    fn skip(&mut self) {
        while let Some(&c) = self.src.get(self.pos) {
            match c {
                b' ' | b'\t' | b'\r' | b'\n' => self.pos += 1,
                b'#' => self.line_comment(1),
                b'/' if self.src.get(self.pos + 1) == Some(&b'/') => self.line_comment(2),
                b'/' if self.src.get(self.pos + 1) == Some(&b'*') => {
                    let rest = &self.src[self.pos + 2..];
                    match rest.windows(2).position(|w| w == b"*/") {
                        Some(end) => self.pos += end + 4,
                        None => self.pos = self.src.len() + 1,
                    }
                }
                _ => return,
            }
        }
    }

    fn line_comment(&mut self, prefix: usize) {
        self.pos += prefix;
        while let Some(&c) = self.src.get(self.pos) {
            self.pos += 1;
            if c == b'\n' {
                break;
            }
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip();
        self.src.get(self.pos).copied()
    }

    fn value(&mut self, depth: usize) -> Option<Value> {
        if depth > MAX_JSON_DEPTH {
            return None;
        }
        match self.peek()? {
            b'{' => {
                self.pos += 1;
                self.object(depth)
            }
            b'[' => {
                self.pos += 1;
                self.array(depth)
            }
            b'"' | b'\'' => self.quoted().map(Value::String),
            b',' | b';' | b']' | b'}' | b':' | b'=' => None,
            _ => Some(self.literal(true)?),
        }
    }

    fn object(&mut self, depth: usize) -> Option<Value> {
        let mut map = serde_json::Map::new();
        if self.peek()? == b'}' {
            self.pos += 1;
            return Some(Value::Object(map));
        }
        loop {
            let key = match self.peek()? {
                b'"' | b'\'' => self.quoted()?,
                b',' | b';' | b']' | b'}' | b':' | b'=' | b'{' | b'[' => return None,
                _ => match self.literal(false)? {
                    Value::String(key) => key,
                    other => other.to_string(),
                },
            };
            match self.peek()? {
                b':' => self.pos += 1,
                b'=' => {
                    self.pos += 1;
                    if self.src.get(self.pos) == Some(&b'>') {
                        self.pos += 1;
                    }
                }
                _ => return None,
            }
            let value = self.value(depth + 1)?;
            map.insert(key, value);
            match self.peek()? {
                b',' | b';' => self.pos += 1,
                b'}' => {
                    self.pos += 1;
                    return Some(Value::Object(map));
                }
                _ => return None,
            }
        }
    }

    fn array(&mut self, depth: usize) -> Option<Value> {
        let mut items = Vec::new();
        loop {
            match self.peek()? {
                b']' => {
                    self.pos += 1;
                    if !items.is_empty() {
                        items.push(Value::Null);
                    }
                    return Some(Value::Array(items));
                }
                b',' | b';' => {
                    self.pos += 1;
                    items.push(Value::Null);
                    continue;
                }
                _ => {}
            }
            items.push(self.value(depth + 1)?);
            match self.peek()? {
                b',' | b';' => self.pos += 1,
                b']' => {
                    self.pos += 1;
                    return Some(Value::Array(items));
                }
                _ => return None,
            }
        }
    }

    fn quoted(&mut self) -> Option<String> {
        let quote = self.src[self.pos];
        self.pos += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let c = *self.src.get(self.pos)?;
            self.pos += 1;
            if c == quote {
                return String::from_utf8(out).ok();
            }
            if c != b'\\' {
                out.push(c);
                continue;
            }
            let escape = *self.src.get(self.pos)?;
            self.pos += 1;
            let decoded = match escape {
                b'b' => '\u{8}',
                b'f' => '\u{c}',
                b'n' => '\n',
                b'r' => '\r',
                b't' => '\t',
                b'u' => {
                    let digits = std::str::from_utf8(self.src.get(self.pos..self.pos + 4)?).ok()?;
                    self.pos += 4;
                    char::from_u32(u32::from_str_radix(digits, 16).ok()?).unwrap_or('\u{fffd}')
                }
                b'\n' | b'\'' | b'"' | b'\\' | b'/' => escape as char,
                _ => return None,
            };
            let mut buf = [0u8; 4];
            out.extend_from_slice(decoded.encode_utf8(&mut buf).as_bytes());
        }
    }

    fn literal(&mut self, typed: bool) -> Option<Value> {
        let start = self.pos;
        while let Some(&c) = self.src.get(self.pos) {
            if matches!(
                c,
                b' ' | b'\t'
                    | b'\r'
                    | b'\n'
                    | b'/'
                    | b'\\'
                    | b';'
                    | b'#'
                    | b'='
                    | b':'
                    | b'{'
                    | b'}'
                    | b'['
                    | b']'
                    | b','
            ) {
                break;
            }
            self.pos += 1;
        }
        let text = std::str::from_utf8(&self.src[start..self.pos]).ok()?;
        if text.is_empty() {
            return None;
        }
        if typed {
            if text.eq_ignore_ascii_case("true") {
                return Some(Value::Bool(true));
            }
            if text.eq_ignore_ascii_case("false") {
                return Some(Value::Bool(false));
            }
            if text.eq_ignore_ascii_case("null") {
                return Some(Value::Null);
            }
            if let Ok(n) = text.parse::<f64>()
                && let Some(n) = serde_json::Number::from_f64(n)
            {
                return Some(Value::Number(n));
            }
        }
        Some(Value::String(text.to_owned()))
    }
}

fn json_to_nbt(value: &Value, depth: usize) -> Option<Tag> {
    if depth > MAX_JSON_DEPTH {
        return None;
    }
    Some(match value {
        Value::Null => return None,
        Value::Bool(b) => Tag::from(*b),
        Value::Number(n) => Tag::Double(n.as_f64().unwrap_or(0.0)),
        Value::String(s) => Tag::String(s.clone()),
        Value::Array(items) => {
            let mut list = Vec::with_capacity(items.len());
            for item in items {
                if !item.is_null() {
                    list.push(json_to_nbt(item, depth + 1)?);
                }
            }
            Tag::List(list)
        }
        Value::Object(map) => {
            let mut compound = Compound::with_capacity(map.len());
            for (key, item) in map {
                if !item.is_null() {
                    compound.insert(key.as_str(), json_to_nbt(item, depth + 1)?);
                }
            }
            Tag::Compound(compound)
        }
    })
}

fn fix_tree(tag: &mut Tag, legacy: bool, events: bool, depth: usize) {
    if depth > MAX_TREE_DEPTH {
        return;
    }
    match tag {
        Tag::List(list) => {
            for element in list {
                fix_tree(element, legacy, events, depth + 1);
            }
        }
        Tag::Compound(component) => fix_component(component, legacy, events, depth),
        _ => {}
    }
}

fn fix_component(component: &mut Compound, legacy: bool, events: bool, depth: usize) {
    if let Some(Tag::List(extra)) = component.get_mut("extra") {
        for element in extra {
            fix_tree(element, legacy, events, depth + 1);
        }
    }
    if let Some(separator) = component.get_mut("separator") {
        fix_tree(separator, legacy, events, depth + 1);
    }
    if legacy {
        fix_legacy_hover(component);
        for flag in FLAGS {
            if let Some(Tag::String(text)) = component.get(flag) {
                let value = text.eq_ignore_ascii_case("true");
                component.insert(flag, value);
            }
        }
    }
    if let Some(Tag::Compound(hover)) = component.get_mut("hoverEvent")
        && hover.get_str("action") == Some("show_entity")
        && let Some(Tag::Compound(contents)) = hover.get_mut("contents")
        && let Some(name) = contents.get_mut("name")
    {
        fix_tree(name, legacy, events, depth + 1);
    }
    if let Some(Tag::Compound(hover)) = component.get_mut("hoverEvent")
        && hover.get_str("action") == Some("show_text")
        && let Some(contents) = hover.get_mut("contents")
    {
        fix_tree(contents, legacy, events, depth + 1);
    }
    if events {
        if let Some(Tag::Compound(hover)) = component.remove("hoverEvent") {
            component.insert("hover_event", fix_hover_event(hover));
        }
        if let Some(Tag::Compound(click)) = component.remove("clickEvent")
            && let Some(click) = fix_click_event(click)
        {
            component.insert("click_event", click);
        }
    }
}

fn fix_legacy_hover(component: &mut Compound) {
    let Some(Tag::Compound(hover)) = component.get_mut("hoverEvent") else {
        return;
    };
    if !hover.contains_key("value") {
        return;
    }
    if hover.get_str("action") == Some("show_text") {
        super::rename(hover, "value", "contents");
        return;
    }
    let text = format!(
        "Legacy hoverEvent: {}",
        stable_json(&Tag::Compound(hover.clone()))
    );
    *hover = Compound::new()
        .with("action", "show_text")
        .with("contents", Compound::new().with("text", text));
}

fn fix_hover_event(mut hover: Compound) -> Compound {
    match hover.get_str("action") {
        Some("show_text") => super::rename(&mut hover, "contents", "value"),
        Some("show_item") => {
            if let Some(Tag::Compound(contents)) = hover.remove("contents") {
                for key in ["id", "count", "components"] {
                    if let Some(value) = contents.get(key) {
                        hover.insert(key, value.clone());
                    }
                }
            }
        }
        Some("show_entity") => {
            if let Some(Tag::Compound(contents)) = hover.remove("contents") {
                for (from, to) in [("id", "uuid"), ("type", "id"), ("name", "name")] {
                    if let Some(value) = contents.get(from) {
                        hover.insert(to, value.clone());
                    }
                }
            }
        }
        _ => {}
    }
    hover
}

fn fix_click_event(mut click: Compound) -> Option<Compound> {
    let value = click.get_str("value").unwrap_or("").to_owned();
    match click.get_str("action").unwrap_or("") {
        "open_url" => {
            if !valid_url(&value) {
                return None;
            }
            super::rename(&mut click, "value", "url");
        }
        "open_file" => super::rename(&mut click, "value", "path"),
        "run_command" | "suggest_command" => {
            if !valid_chat(&value) {
                return None;
            }
            super::rename(&mut click, "value", "command");
        }
        "change_page" => {
            let page = match click.get("value") {
                Some(Tag::String(text)) => text.parse::<i32>().ok(),
                Some(other) => super::number(other).map(|v| v as i32),
                None => None,
            }?;
            click.remove("value");
            click.insert("page", page.max(1));
        }
        _ => {}
    }
    Some(click)
}

fn valid_url(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once(':') else {
        return false;
    };
    let scheme = scheme.to_ascii_lowercase();
    (scheme == "http" || scheme == "https")
        && !rest.contains(|c: char| c.is_whitespace() || c == '"' || c == '<' || c == '>')
}

fn valid_chat(text: &str) -> bool {
    !text
        .chars()
        .any(|c| c == '\u{a7}' || c < ' ' || c == '\u{7f}')
}

/// A tag as JSON with sorted object keys.
pub(crate) fn stable_json(tag: &Tag) -> String {
    serde_json::to_string(&to_json(tag, 0)).unwrap_or_default()
}

fn to_json(tag: &Tag, depth: usize) -> Value {
    if depth > MAX_TREE_DEPTH {
        return Value::Null;
    }
    match tag {
        Tag::Compound(compound) => {
            let mut keys: Vec<&str> = compound.keys().collect();
            keys.sort_unstable();
            Value::Object(
                keys.into_iter()
                    .map(|key| {
                        (
                            key.to_owned(),
                            to_json(compound.get(key).unwrap(), depth + 1),
                        )
                    })
                    .collect(),
            )
        }
        Tag::List(items) => Value::Array(items.iter().map(|t| to_json(t, depth + 1)).collect()),
        Tag::String(text) => Value::from(text.as_str()),
        Tag::Byte(v) => Value::from(*v),
        Tag::Short(v) => Value::from(*v),
        Tag::Int(v) => Value::from(*v),
        Tag::Long(v) => Value::from(*v),
        Tag::Float(v) => Value::from(*v),
        Tag::Double(v) => Value::from(*v),
        Tag::ByteArray(v) => Value::from(v.clone()),
        Tag::IntArray(v) => Value::from(v.clone()),
        Tag::LongArray(v) => Value::from(v.clone()),
    }
}

/// The `translate` key of a component that is still a JSON string.
pub(crate) fn json_translation_key(tag: &Tag) -> Option<String> {
    let Tag::String(json) = tag else {
        return None;
    };
    match serde_json::from_str::<Value>(json).ok()? {
        Value::Object(map) => map.get("translate")?.as_str().map(str::to_owned),
        _ => None,
    }
}

/// The `translate` key of a component in its NBT form with no other
/// content (a "plain" translation).
pub(crate) fn plain_translation_key(tag: &Tag) -> Option<&str> {
    match tag {
        Tag::Compound(component) if component.len() == 1 => component.get_str("translate"),
        _ => None,
    }
}

/// A plain text component as a JSON string, as pre-NBT data held it.
pub(crate) fn json_text(text: &str) -> String {
    serde_json::to_string(&serde_json::json!({ "text": text })).unwrap_or_default()
}

pub(crate) fn json_translate(key: &str) -> String {
    serde_json::to_string(&serde_json::json!({ "translate": key })).unwrap_or_default()
}
