use serde_json::{Map, Value};

const FIXTURE: &str = include_str!("../../../../tests/fixtures/minimessage/adventure.json");

const STYLE_KEYS: &[&str] = &[
    "color",
    "bold",
    "italic",
    "underlined",
    "strikethrough",
    "obfuscated",
    "font",
    "insertion",
    "click_event",
    "hover_event",
    "shadow_color",
];

const KNOWN_DIFFERENCES: &[&str] = &[
    "MiniMessageParserTest.testGH5",
    "MiniMessageParserTest.testGH5Modified",
    "NbtTagTest.testParseBlockNbt",
];

fn normalize(v: &Value) -> Value {
    let mut runs = Vec::new();
    flatten(v, &Map::new(), &mut runs);
    Value::Array(runs)
}

const NAMED: &[(&str, &str)] = &[
    ("black", "#000000"),
    ("dark_blue", "#0000aa"),
    ("dark_green", "#00aa00"),
    ("dark_aqua", "#00aaaa"),
    ("dark_red", "#aa0000"),
    ("dark_purple", "#aa00aa"),
    ("gold", "#ffaa00"),
    ("gray", "#aaaaaa"),
    ("dark_gray", "#555555"),
    ("blue", "#5555ff"),
    ("green", "#55ff55"),
    ("aqua", "#55ffff"),
    ("red", "#ff5555"),
    ("light_purple", "#ff55ff"),
    ("yellow", "#ffff55"),
    ("white", "#ffffff"),
];

fn namespaced(v: &Value) -> Value {
    match v.as_str() {
        Some(s) if !s.contains(':') => Value::String(format!("minecraft:{s}")),
        _ => v.clone(),
    }
}

fn uuid_string(v: &Value) -> Value {
    match v.as_array() {
        Some(parts) if parts.len() == 4 => {
            let hex: String = parts
                .iter()
                .map(|p| format!("{:08x}", p.as_i64().unwrap_or_default() as u32))
                .collect();
            Value::String(format!(
                "{}-{}-{}-{}-{}",
                &hex[..8],
                &hex[8..12],
                &hex[12..16],
                &hex[16..20],
                &hex[20..]
            ))
        }
        _ => Value::String(v.as_str().unwrap_or_default().to_ascii_lowercase()),
    }
}

fn style_value(key: &str, v: &Value) -> Value {
    match (key, v) {
        ("color", Value::String(s)) => {
            let lower = s.to_ascii_lowercase();
            let hex = NAMED
                .iter()
                .find(|(name, _)| *name == lower)
                .map(|(_, hex)| *hex);
            Value::String(hex.map_or(lower, str::to_owned))
        }
        ("font", _) => namespaced(v),
        ("hover_event", Value::Object(o)) => Value::Object(
            o.iter()
                .map(|(k, v)| {
                    let v = match k.as_str() {
                        "value" | "name" => normalize(v),
                        "id" => namespaced(v),
                        "uuid" => uuid_string(v),
                        _ => v.clone(),
                    };
                    (k.clone(), v)
                })
                .collect(),
        ),
        _ => v.clone(),
    }
}

/// Folds encodings that mean the same component: defaults spelled out or
/// left out, bare ids against `minecraft:` ones, string shorthands.
fn content_value(key: &str, v: &Value) -> Option<Value> {
    match (key, v) {
        ("object", _) | ("hat", Value::Bool(true)) => None,
        ("interpret" | "plain", Value::Bool(false)) => None,
        ("atlas", _) if namespaced(v) == "minecraft:blocks" => None,
        ("sprite" | "atlas", _) => Some(namespaced(v)),
        ("separator", _) => Some(normalize(v)),
        ("player", Value::String(name)) => Some(serde_json::json!({ "name": name })),
        _ => Some(v.clone()),
    }
}

fn flatten(v: &Value, inherited: &Map<String, Value>, out: &mut Vec<Value>) {
    let owned;
    let obj = match v {
        Value::Object(o) => o,
        Value::String(s) => {
            owned = Map::from_iter([("text".to_owned(), Value::String(s.clone()))]);
            &owned
        }
        Value::Array(items) => {
            for item in items {
                flatten(item, inherited, out);
            }
            return;
        }
        other => panic!("unexpected component {other}"),
    };
    let mut style = inherited.clone();
    let mut content = Map::new();
    for (k, val) in obj {
        if STYLE_KEYS.contains(&k.as_str()) {
            style.insert(k.clone(), style_value(k, val));
        } else if k == "with" {
            let args = val.as_array().map(|a| a.iter().map(normalize).collect());
            content.insert(k.clone(), Value::Array(args.unwrap_or_default()));
        } else if k != "extra"
            && k != "type"
            && let Some(val) = content_value(k, val)
        {
            content.insert(k.clone(), val);
        }
    }
    let inherited = style;
    let style = Value::Object(inherited.clone());
    match content.get("text").and_then(Value::as_str) {
        Some("") if content.len() == 1 => {}
        Some(text) if content.len() == 1 => {
            if let Some(Value::Array(last)) = out.last_mut()
                && last[1] == style
                && let Some(Value::String(prev)) = last[0].get_mut("text")
            {
                prev.push_str(text);
            } else {
                out.push(Value::Array(vec![Value::Object(content), style]));
            }
        }
        _ => out.push(Value::Array(vec![Value::Object(content), style])),
    }
    if let Some(Value::Array(children)) = obj.get("extra") {
        for child in children {
            flatten(child, &inherited, out);
        }
    }
}

#[tuff_core::watchdog]
#[test]
fn parser_matches_adventure_minimessage() {
    let doc: Value = serde_json::from_str(FIXTURE).unwrap();
    let cases = doc["cases"].as_array().unwrap();
    let mut failures = Vec::new();
    let mut passed = 0;
    let mut compared = 0;
    for case in cases {
        let source = case["source"].as_str().unwrap();
        let Some(expected) = case.get("expected") else {
            continue;
        };
        compared += 1;
        let input = case["input"].as_str().unwrap();
        let actual: Value = serde_json::from_str(&super::parse(input).to_json()).unwrap();
        let (want, got) = (normalize(expected), normalize(&actual));
        let known = KNOWN_DIFFERENCES.contains(&source);
        match (want == got, known) {
            (true, false) => passed += 1,
            (false, true) => {}
            (true, true) => failures.push(format!(
                "{source}: listed as a known difference but now matches"
            )),
            (false, false) => failures.push(format!(
                "{source}\n  input: {input:?}\n  want: {want}\n  got:  {got}"
            )),
        }
    }
    assert!(
        failures.is_empty(),
        "{} mismatches ({passed}/{compared} match):\n{}",
        failures.len(),
        failures.join("\n")
    );
}
