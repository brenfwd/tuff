//! The server's command tree (`commands.json`), which is a single big nested
//! literal starting at [`crate::command::ROOT`].
//!
//! An argument's parser properties and a node's permission requirement are
//! kept as raw JSON text: both belong to their own open-ended mini-languages
//! (one schema per parser kind; a small requirement-predicate language)
//! that this crate does not model further.
//!
//! The report omits a redirect to the root (its path is empty), so a node
//! that has no children, is not executable, and names no redirect is taken
//! to redirect to the root, as `execute ... run` and `return run` do.

use std::fmt::Write;

use anyhow::{Context, Result};

use super::json;
use crate::input::{CommandNode, Input};
use crate::names::str_literal;

pub fn generate(input: &Input) -> Result<String> {
    let mut out = String::new();
    write!(out, "pub static ROOT: CommandNode = ")?;
    node(&mut out, "", &input.commands)?;
    writeln!(out, ";")?;
    Ok(out)
}

fn node(out: &mut String, name: &str, cmd_node: &CommandNode) -> Result<()> {
    let kind = match cmd_node.kind.as_str() {
        "root" => "CommandNodeKind::Root".to_owned(),
        "literal" => "CommandNodeKind::Literal".to_owned(),
        "argument" => format!(
            "CommandNodeKind::Argument {{ parser: {}, properties: {} }}",
            str_literal(
                cmd_node
                    .parser
                    .as_deref()
                    .context("argument node has no parser")?
            ),
            json::opt_literal(cmd_node.properties.as_ref()),
        ),
        other => anyhow::bail!("unknown command node type {other}"),
    };
    anyhow::ensure!(
        cmd_node.kind == "argument" || cmd_node.parser.is_none() && cmd_node.properties.is_none(),
        "{name}: only argument nodes have a parser"
    );
    let redirect = match &cmd_node.redirect {
        Some(path) => format!(
            "Some(&[{}])",
            path.iter()
                .map(|s| str_literal(s))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        None if cmd_node.kind != "root" && cmd_node.children.is_empty() && !cmd_node.executable => {
            "Some(&[])".to_owned()
        }
        None => "None".to_owned(),
    };
    write!(
        out,
        "CommandNode {{ name: {}, kind: {kind}, executable: {}, redirect: {redirect}, permissions: {}, children: &[",
        str_literal(name),
        cmd_node.executable,
        json::opt_literal(cmd_node.permissions.as_ref()),
    )?;
    for (name, child) in &cmd_node.children {
        node(out, name, child)?;
        out.push_str(", ");
    }
    out.push_str("] }");
    Ok(())
}
