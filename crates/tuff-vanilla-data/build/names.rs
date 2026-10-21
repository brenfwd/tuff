//! Conversions from resource locations and property names to Rust identifiers.

use std::collections::BTreeMap;

use anyhow::{Result, bail};

const KEYWORDS: &[&str] = &[
    "abstract", "as", "async", "await", "become", "box", "break", "const", "continue", "crate",
    "do", "dyn", "else", "enum", "extern", "false", "final", "fn", "for", "gen", "if", "impl",
    "in", "let", "loop", "macro", "match", "mod", "move", "mut", "override", "priv", "pub", "ref",
    "return", "static", "struct", "super", "trait", "true", "try", "type", "typeof", "unsafe",
    "unsized", "use", "virtual", "where", "while", "yield",
];

/// Drops the default `minecraft:` namespace but keeps any other one, so
/// `brigadier:bool` stays distinct from `minecraft:bool`.
pub fn strip_namespace(name: &str) -> &str {
    name.strip_prefix("minecraft:").unwrap_or(name)
}

fn words(name: &str) -> impl Iterator<Item = &str> {
    strip_namespace(name)
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
}

pub fn pascal(name: &str) -> String {
    let ident: String = words(name)
        .map(|word| {
            let mut chars = word.chars();
            let first = chars.next().map(|c| c.to_ascii_uppercase());
            first.into_iter().chain(chars).collect::<String>()
        })
        .collect();
    if ident.starts_with(|c: char| c.is_ascii_digit()) {
        format!("_{ident}")
    } else {
        ident
    }
}

pub fn snake(name: &str) -> String {
    let ident = words(name)
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join("_");
    if KEYWORDS.contains(&ident.as_str()) {
        format!("r#{ident}")
    } else if ident.starts_with(|c: char| c.is_ascii_digit()) {
        format!("_{ident}")
    } else {
        ident
    }
}

pub fn shouty(name: &str) -> String {
    let ident = words(name)
        .map(str::to_ascii_uppercase)
        .collect::<Vec<_>>()
        .join("_");
    if ident.starts_with(|c: char| c.is_ascii_digit()) {
        format!("_{ident}")
    } else {
        ident
    }
}

/// Maps each name to an identifier with `convert`, failing the build if two
/// names collide so that a new game version cannot silently shadow an entry.
pub fn unique<'a>(
    names: impl IntoIterator<Item = &'a str>,
    convert: fn(&str) -> String,
) -> Result<Vec<String>> {
    let mut seen = BTreeMap::new();
    let mut idents = Vec::new();
    for name in names {
        let ident = convert(name);
        if let Some(other) = seen.insert(ident.clone(), name) {
            bail!("`{other}` and `{name}` both map to the identifier `{ident}`");
        }
        idents.push(ident);
    }
    Ok(idents)
}

pub fn str_literal(value: &str) -> String {
    format!("{value:?}")
}
