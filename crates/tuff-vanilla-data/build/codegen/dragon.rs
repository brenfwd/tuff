//! The ender dragon's phase ids and flight graph from
//! `DragonProperties.java`.

use std::fmt::Write;

use anyhow::{Context, Result, bail, ensure};

use crate::input::Input;
use crate::names::str_literal;

pub fn generate(input: &Input) -> Result<String> {
    let mut phases: Vec<String> = Vec::new();
    let mut adjacency: Vec<u32> = Vec::new();
    let mut parts: Vec<(String, f32, f32)> = Vec::new();
    for line in input.dragon_properties.lines() {
        let mut words = line.split(' ');
        match words.next() {
            Some("phase") => {
                let id: usize = words.next().context("phase id")?.parse()?;
                let name = words.next().context("phase name")?;
                ensure!(id == phases.len(), "phase ids are not consecutive: {line}");
                phases.push(name.to_owned());
            }
            Some("node_adjacency") => {
                let index: usize = words.next().context("node index")?.parse()?;
                let bits: u32 = words.next().context("node bits")?.parse()?;
                ensure!(
                    index == adjacency.len(),
                    "node indices are not consecutive: {line}"
                );
                adjacency.push(bits);
            }
            Some("part") => {
                let index: usize = words.next().context("part index")?.parse()?;
                let field = words.next().context("part field")?;
                let width: f32 = words.next().context("part width")?.parse()?;
                let height: f32 = words.next().context("part height")?.parse()?;
                ensure!(
                    index == parts.len(),
                    "part indices are not consecutive: {line}"
                );
                parts.push((field.to_owned(), width, height));
            }
            _ => bail!("unknown dragon properties line: {line}"),
        }
    }
    ensure!(!phases.is_empty(), "no dragon phases dumped");
    ensure!(!parts.is_empty(), "no dragon parts dumped");
    ensure!(
        adjacency.iter().all(|bits| (*bits >> adjacency.len()) == 0),
        "a node links past the last node"
    );
    let mut out = String::new();
    writeln!(out, "/// Phase names by id.")?;
    writeln!(
        out,
        "pub const PHASES: &[&str] = &[{}];",
        phases
            .iter()
            .map(|p| str_literal(p))
            .collect::<Vec<_>>()
            .join(", ")
    )?;
    writeln!(
        out,
        "/// Bit `j` of entry `i` links flight node `i` to node `j`."
    )?;
    write!(out, "pub const NODE_ADJACENCY: &[u32] = &[")?;
    super::array_body(&mut out, &adjacency);
    writeln!(out, "];")?;
    writeln!(
        out,
        "/// The hitbox parts as `(field, width, height)`, in the order clients\n\
         /// number them after the dragon's own entity id."
    )?;
    writeln!(out, "pub const PARTS: &[(&str, f32, f32)] = &[")?;
    for (field, width, height) in &parts {
        writeln!(out, "    ({}, {width:?}, {height:?}),", str_literal(field))?;
    }
    writeln!(out, "];")?;
    Ok(out)
}
