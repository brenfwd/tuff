//! The `en_us` client language file (`assets/minecraft/lang/en_us.json`),
//! embedded as a sorted lookup table. Console output resolves `translate`
//! keys against this the same way a vanilla client would, since the server
//! itself never receives a resource pack to translate with.

use std::fmt::Write;

use anyhow::Result;

use crate::input::Input;
use crate::names::str_literal;

pub fn generate(input: &Input) -> Result<String> {
    let mut out = String::new();
    writeln!(out, "pub static EN_US: &[(&str, &str)] = &[")?;
    // `input.lang` is a `BTreeMap`, so entries are already sorted by key,
    // which is what the binary search in `lookup` requires.
    for (key, value) in &input.lang {
        writeln!(out, "    ({}, {}),", str_literal(key), str_literal(value))?;
    }
    writeln!(out, "];")?;
    writeln!(
        out,
        "\npub fn lookup(key: &str) -> Option<&'static str> {{
    EN_US
        .binary_search_by_key(&key, |(k, _)| *k)
        .ok()
        .map(|index| EN_US[index].1)
}}"
    )?;
    Ok(out)
}
