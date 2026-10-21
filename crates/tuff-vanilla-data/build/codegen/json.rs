//! Emits arbitrary JSON as Rust string literals. Used for the handful of
//! places where Minecraft's data format is an open-ended mini-language (item
//! predicates, permission requirements, argument-parser properties, ...): the
//! surrounding shape is typed, but a payload like that is kept as compact
//! JSON text rather than exhaustively modeled.

use serde_json::Value;

use crate::names::str_literal;

/// `value` as a Rust string literal holding compact JSON text.
pub fn literal(value: &Value) -> String {
    str_literal(&value.to_string())
}

/// `Some(...)`/`None` for an optional JSON payload.
pub fn opt_literal(value: Option<&Value>) -> String {
    match value {
        Some(value) => format!("Some({})", literal(value)),
        None => "None".to_owned(),
    }
}

/// Splits `{"type": "...", ...rest}` into its type name and the remaining
/// fields, which callers emit as a [`literal`]. `None` for `rest` when it
/// would be an empty object, so a parameterless kind emits `None` instead of
/// an unhelpful `Some("{}")`.
pub fn kind_and_rest(value: &Value) -> anyhow::Result<(String, Option<Value>)> {
    let mut object = value
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("expected an object, got {value}"))?;
    let kind = object
        .remove("type")
        .and_then(|v| v.as_str().map(str::to_owned))
        .ok_or_else(|| anyhow::anyhow!("object has no \"type\": {value}"))?;
    Ok((kind, (!object.is_empty()).then_some(Value::Object(object))))
}
