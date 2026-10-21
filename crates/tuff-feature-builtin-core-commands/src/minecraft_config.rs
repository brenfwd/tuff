//! `/tuff config minecraft.<key> [value|reset]`: reads and sets
//! server.properties through the same spec table and coordinator path as
//! the web panel.

use tuff_core::command::CommandError;
use tuff_core::panel_host::config::{change_note, check_value, properties};
use tuff_core::property_spec::{PROPERTIES, Property, PropertyKind, parse_property, property};
use tuff_core::world::commands::{Context, text};
use tuff_web_ui::audit::Entry;
use tuff_web_ui::config::REDACTED;

pub const PREFIX: &str = "minecraft.";

fn fail(message: impl Into<String>) -> CommandError {
    CommandError::text(text(message.into()))
}

pub fn names() -> impl Iterator<Item = String> {
    PROPERTIES.iter().map(|p| format!("{PREFIX}{}", p.name))
}

pub fn value_suggestions(name: &str) -> Vec<String> {
    let Some(spec) = name.strip_prefix(PREFIX).and_then(property) else {
        return Vec::new();
    };
    let mut out: Vec<String> = match spec.kind {
        PropertyKind::Bool => vec!["true".into(), "false".into()],
        PropertyKind::Choice(options) => options.iter().map(|o| (*o).to_owned()).collect(),
        _ => Vec::new(),
    };
    if !spec.secret && !spec.default.is_empty() && !out.iter().any(|o| o == spec.default) {
        out.push(spec.default.to_owned());
    }
    out.push("reset".into());
    out
}

pub fn describe(name: &str) -> Option<String> {
    let spec = property(name.strip_prefix(PREFIX)?)?;
    let kind = match spec.kind {
        PropertyKind::Bool => "bool",
        PropertyKind::Int { .. } => "int",
        PropertyKind::Text => "string",
        PropertyKind::Choice(_) => "choice",
    };
    Some(format!(
        "server.properties {kind}: {}",
        spec.effect.describe()
    ))
}

fn shown(spec: Option<&Property>, value: &str) -> String {
    if spec.is_some_and(|s| s.secret) {
        if value.is_empty() { "<unset>" } else { "<set>" }.to_owned()
    } else {
        value.to_owned()
    }
}

fn effect_note(spec: Option<&Property>) -> String {
    match spec.map(|s| s.effect) {
        Some(tuff_core::property_spec::Effect::Live) => String::new(),
        Some(effect) => format!(" ({})", effect.describe()),
        None => " (not a key this version defines; kept in the file)".to_owned(),
    }
}

pub async fn run(
    ctx: &mut Context<'_>,
    full: &str,
    raw: Option<String>,
) -> Result<i32, CommandError> {
    let name = full.strip_prefix(PREFIX).unwrap_or(full);
    let shared = ctx.shared().clone();
    let Some(stored) = properties(&shared, None).await else {
        return Err(fail("The world is not running"));
    };
    let spec = property(name);
    if spec.is_none() && !stored.contains_key(name) {
        return Err(fail(format!("Unknown server property '{name}'")));
    }
    let secret = spec.is_some_and(|s| s.secret);
    let Some(raw) = raw else {
        let default = spec.map_or("", |s| s.default);
        let current = stored.get(name).map_or(default, String::as_str);
        ctx.reply(text(format!(
            "{PREFIX}{name} = {} (default '{}'){}",
            shown(spec, current),
            shown(spec, default),
            match spec {
                Some(spec) => format!(" ({})", spec.effect.describe()),
                None => effect_note(spec),
            },
        )));
        return Ok(1);
    };
    let raw = raw.trim();
    let raw = if raw == "\"\"" { "" } else { raw };
    let wanted = if raw == "reset" {
        spec.map_or("", |s| s.default)
    } else {
        raw
    };
    let principal = crate::web::principal(ctx);
    let entry = || Entry::new(principal.name(), "properties.set").target(name);
    let audited = |text: &str| {
        if secret && !text.is_empty() {
            REDACTED.to_owned()
        } else {
            text.to_owned()
        }
    };
    let checked = match parse_property(spec, wanted) {
        Ok(value) => check_value(&shared, name, &value).await.map(|()| value),
        Err(message) => Err(message),
    };
    let value = match checked {
        Ok(value) => value,
        Err(message) => {
            shared
                .web
                .audit
                .record(entry().detail(audited(wanted)).outcome(message.clone()));
            return Err(fail(format!("Invalid value for {PREFIX}{name}: {message}")));
        }
    };
    let Some(now) = properties(&shared, Some((name.to_owned(), value.clone()))).await else {
        return Err(fail("The world is not running"));
    };
    shared.web.audit.record(entry().detail(audited(&value)));
    let now = now.get(name).cloned().unwrap_or(value);
    let note = match change_note(&shared, spec) {
        Some(note) => format!(". {note}"),
        None => effect_note(spec),
    };
    ctx.reply(text(format!(
        "Set {PREFIX}{name} to {}{note}",
        shown(spec, &now),
    )));
    Ok(1)
}
