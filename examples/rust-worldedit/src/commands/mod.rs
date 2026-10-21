// SPDX-License-Identifier: MIT
mod biome;
mod clip;
pub mod entity;
mod general;
mod generation;
pub mod navigation;
pub mod region;
mod schem;
pub mod select;
pub mod utility;

use crate::session::{owner, say};

pub struct Ctx<'a> {
    pub id: u128,
    pub args: Vec<&'a str>,
    pub flags: Vec<char>,
}

impl Ctx<'_> {
    pub fn flag(&self, flag: char) -> bool {
        self.flags.contains(&flag)
    }
}

pub struct Command {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub usage: &'static str,
    pub summary: &'static str,
    pub node: &'static str,
    pub flags: &'static str,
    pub run: fn(&Ctx) -> i32,
}

pub const BASE_NODE: &str = "worldedit.use";

pub fn table() -> Vec<&'static Command> {
    [
        general::COMMANDS,
        generation::COMMANDS,
        region::COMMANDS,
        select::COMMANDS,
        schem::COMMANDS,
        clip::COMMANDS,
        biome::COMMANDS,
        entity::COMMANDS,
        utility::COMMANDS,
        navigation::COMMANDS,
        crate::brush::COMMANDS,
    ]
    .into_iter()
    .flatten()
    .collect()
}

pub fn find(name: &str) -> Option<&'static Command> {
    let name = name.trim_start_matches('/');
    table()
        .into_iter()
        .find(|c| c.name == name || c.aliases.contains(&name))
}

pub fn usage(command: &Command) -> String {
    format!("Usage: //{} {}", command.name, command.usage)
        .trim_end()
        .to_owned()
}

fn is_flag(word: &str) -> bool {
    word.len() > 1 && word.starts_with('-') && word[1..].chars().all(|c| c.is_ascii_alphabetic())
}

pub fn dispatch(line: &str) -> i32 {
    let mut words = line.split_whitespace().skip(2);
    let Some(name) = words.next() else {
        return general::overview();
    };
    let Some(command) = find(name) else {
        return say(format!(
            "Unknown WorldEdit command {name}. Use //help for the list"
        ));
    };
    if tuff_plugin_sdk::world::acting::has_permission(BASE_NODE) != Some(true)
        || tuff_plugin_sdk::world::acting::has_permission(command.node) != Some(true)
    {
        return say("You do not have permission to use this WorldEdit command");
    }
    let mut flags = Vec::new();
    let mut args = Vec::new();
    for word in words {
        if is_flag(word) {
            for flag in word[1..].chars() {
                if !command.flags.contains(flag) {
                    return say(format!("Unknown flag -{flag}. {}", usage(command)));
                }
                flags.push(flag);
            }
        } else {
            args.push(word);
        }
    }
    let Some(id) = owner() else {
        return say("WorldEdit needs a player");
    };
    let result = (command.run)(&Ctx { id, args, flags });
    crate::cui::sync(id);
    result
}

pub fn register() -> Result<(), String> {
    use tuff_plugin_sdk::command::{alias, argument, command, parser};
    command("we")
        .executes(|ctx| Ok(dispatch(&ctx.line)))
        .then(argument("args", parser::greedy()).executes(|ctx| Ok(dispatch(&ctx.line))))
        .register_tuff_subcommand()
        .map_err(|e| format!("{e:?}"))?;
    tuff_plugin_sdk::world::declare_permission(BASE_NODE, 2);
    tuff_plugin_sdk::world::declare_permission("worldedit.history.undo.other", 3);
    for command in table() {
        tuff_plugin_sdk::world::declare_permission(command.node, 2);
        let target = format!("tuff we {}", command.name);
        for name in std::iter::once(&command.name).chain(command.aliases) {
            let alias_name = match navigation::is_navigation(command.node) {
                true => name.to_string(),
                false => format!("/{name}"),
            };
            let _ = alias(&alias_name, &target);
        }
    }
    Ok(())
}
