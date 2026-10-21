//! Chat commands: the tree sent to clients (the vanilla tree, filtered to
//! the commands this server registers and the player's permission level),
//! a parser over that tree, argument readers, selectors, and completions.
//! Executing a parsed command is the world's business (see
//! `world::commands`); this module never touches game state.

pub mod args;
pub mod block;
pub mod coords;
pub mod error;
pub mod item;
pub mod item_predicate;
pub mod nbt_path;
pub mod parse;
pub mod particle;
pub mod range;
pub mod reader;
pub mod selector;

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tuff_storage::admin::PermissionLevel;

use tuff_vanilla_data::command::ROOT;
pub use tuff_vanilla_data::command::{CommandNode, CommandNodeKind};
use tuff_vanilla_data::registry::CommandArgumentType;

use crate::protocol::play::{CommandNodeData, Commands, command_node};
use tuff_wire::PacketWriter;

pub use args::{ParseContext, Resources, SuggestContext, Value};
pub use error::CommandError;
pub use parse::{Parsed, ParsedNode};

type CustomSuggest<'a> = dyn Fn(&str, &[ParsedNode], &str) -> Option<Vec<String>> + 'a;

const ENTITY_SINGLE: u8 = 0x01;
const ENTITY_PLAYERS_ONLY: u8 = 0x02;
const RANGE_MIN: u8 = 0x01;
const RANGE_MAX: u8 = 0x02;
pub const ASK_SERVER: &str = "minecraft:ask_server";

/// What a command source may do: its operator level and the permission
/// nodes it holds (see `crate::permission`).
pub trait Permits {
    fn level(&self) -> PermissionLevel;
    fn has(&self, node: &str) -> bool;

    /// Whether the source may use the vanilla command `name`, which needs
    /// `required` without an explicit `minecraft.command.<name>` setting.
    fn command(&self, _name: &str, required: PermissionLevel) -> bool {
        required <= self.level()
    }
}

/// A bare level stands for a source with no player identity: it holds a
/// node only at owner level, like the console.
impl Permits for PermissionLevel {
    fn level(&self) -> PermissionLevel {
        *self
    }

    fn has(&self, _node: &str) -> bool {
        *self >= PermissionLevel::Owners
    }
}

impl<T: Permits + ?Sized> Permits for &T {
    fn level(&self) -> PermissionLevel {
        (**self).level()
    }

    fn has(&self, node: &str) -> bool {
        (**self).has(node)
    }

    fn command(&self, name: &str, required: PermissionLevel) -> bool {
        (**self).command(name, required)
    }
}

/// The requirement type of this server's own nodes: hold permission node
/// `node`.
pub const PERMISSION_REQUIREMENT: &str = "tuff:permission";
/// A requirement met when the source may use any child of the node.
pub const ANY_CHILD_REQUIREMENT: &str = "tuff:any_child";

/// The operator level a vanilla `minecraft:require` requirement asks
/// for; `None` for any other requirement.
fn required_level(json: &str) -> Option<PermissionLevel> {
    let value = serde_json::from_str::<serde_json::Value>(json).ok()?;
    let permission = &value["permission"];
    if value["type"] != "minecraft:require" || permission["type"] != "minecraft:command_level" {
        return None;
    }
    permission["level"]
        .as_str()
        .and_then(PermissionLevel::from_name)
}

/// Whether `access` meets the node's requirement. A requirement this
/// server does not understand is met by nobody.
pub fn allowed(node: &CommandNode, access: &dyn Permits) -> bool {
    let Some(json) = node.permissions else {
        return true;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return false;
    };
    match value["type"].as_str() {
        Some("minecraft:require") => {
            required_level(json).is_some_and(|required| required <= access.level())
        }
        Some(PERMISSION_REQUIREMENT) => value["node"].as_str().is_some_and(|n| access.has(n)),
        Some(ANY_CHILD_REQUIREMENT) => node.children.iter().any(|c| allowed(c, access)),
        _ => false,
    }
}

/// A top-level command this server handles.
#[derive(Debug, Clone, Copy)]
pub struct RootCommand {
    pub name: &'static str,
    pub node: &'static CommandNode,
    /// `(argument name, suggestion provider)` pairs for arguments whose
    /// completions come from a provider rather than the parser.
    pub providers: &'static [(&'static str, &'static str)],
}

impl RootCommand {
    /// A command whose tree this server defines itself.
    pub fn custom(
        name: &'static str,
        node: &'static CommandNode,
        providers: &'static [(&'static str, &'static str)],
    ) -> Self {
        Self {
            name,
            node,
            providers,
        }
    }

    /// The vanilla node for `name`.
    pub fn vanilla(name: &'static str, providers: &'static [(&'static str, &'static str)]) -> Self {
        Self {
            name,
            node: ROOT
                .child(name)
                .unwrap_or_else(|| panic!("no vanilla command {name}")),
            providers,
        }
    }
}

/// How a vanilla top-level command is gated: by the node of `command`
/// (an alias uses its target's), else at `level`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gate {
    pub command: &'static str,
    pub level: PermissionLevel,
}

/// The gates of the registered commands that use the vanilla tree.
fn gates(commands: &[RootCommand]) -> HashMap<&'static str, Gate> {
    commands
        .iter()
        .filter(|c| ROOT.child(c.name).is_some_and(|v| std::ptr::eq(v, c.node)))
        .map(|c| {
            let command = match c.node.redirect {
                Some([target]) if c.node.children.is_empty() => *target,
                _ => c.name,
            };
            let level = match c.node.permissions {
                None => Some(PermissionLevel::All),
                Some(json) => required_level(json),
            };
            (c.name, level.map(|level| Gate { command, level }))
        })
        .filter_map(|(name, gate)| Some((name, gate?)))
        .collect()
}

/// The command tree's root: the registered commands.
#[derive(Debug, Clone)]
pub struct Roots {
    commands: Vec<RootCommand>,
    gates: Arc<HashMap<&'static str, Gate>>,
    /// Leaked once per registry so parsed nodes can refer to them like any
    /// other tree node.
    children: &'static [CommandNode],
    resources: Resources,
}

impl Default for Roots {
    fn default() -> Self {
        Self::new([])
    }
}

impl Roots {
    pub fn new(commands: impl IntoIterator<Item = RootCommand>) -> Self {
        let commands: Vec<RootCommand> = commands.into_iter().collect();
        let children: Vec<CommandNode> = commands.iter().map(|c| *c.node).collect();
        Self {
            gates: Arc::new(gates(&commands)),
            commands,
            children: Box::leak(children.into_boxed_slice()),
            resources: Resources::default(),
        }
    }

    /// The same resource ids with `commands` as the registered commands.
    pub fn with_commands(&self, commands: impl IntoIterator<Item = RootCommand>) -> Self {
        Self {
            resources: self.resources.clone(),
            ..Self::new(commands)
        }
    }

    pub fn resources(&self) -> &Resources {
        &self.resources
    }

    /// Lets `resource` arguments of `registry` name `id`.
    pub fn add_resource(&mut self, registry: &str, id: impl Into<String>) {
        self.resources.add(registry, id);
    }

    /// Lets item arguments name the custom item `id`.
    pub fn add_custom_item(&mut self, id: impl Into<String>, input: item::ItemInput) {
        self.resources.add_custom_item(id, input);
    }

    pub fn children(&self) -> &'static [CommandNode] {
        self.children
    }

    pub fn commands(&self) -> &[RootCommand] {
        &self.commands
    }

    pub fn command(&self, name: &str) -> Option<&RootCommand> {
        self.commands.iter().find(|c| c.name == name)
    }

    /// How the vanilla command `name` is gated, if it is one.
    pub fn gate(&self, name: &str) -> Option<Gate> {
        self.gates.get(name).copied()
    }

    /// Every vanilla command's gate, by command name.
    pub fn gates(&self) -> impl Iterator<Item = (&'static str, Gate)> + '_ {
        self.gates.iter().map(|(name, gate)| (*name, *gate))
    }

    /// Whether `access` may use the top-level command `node`: a vanilla
    /// command by its `minecraft.command.<name>` node or level, any other
    /// by its requirement.
    pub fn root_allowed(&self, node: &CommandNode, access: &dyn Permits) -> bool {
        match self.gates.get(node.name) {
            Some(gate) => access.command(gate.command, gate.level),
            None => allowed(node, access),
        }
    }

    /// The children of a redirect target, and whether it is the root.
    pub fn resolve(&self, path: &[&str]) -> Option<(&'static [CommandNode], bool)> {
        let Some((first, rest)) = path.split_first() else {
            return Some((self.children, true));
        };
        let mut node = self.command(first)?.node;
        for name in rest {
            node = node.child(name)?;
        }
        Some((node.children, false))
    }

    fn provider(&self, path: &[ParsedNode], node: &CommandNode) -> Option<&'static str> {
        let first = path.first()?.node.name;
        let command = self.command(first)?;
        command
            .providers
            .iter()
            .find(|(name, _)| *name == node.name)
            .map(|(_, provider)| *provider)
    }

    /// Parses `input` (without the leading `/`) for a source with `access`.
    pub fn parse(&self, input: &str, access: impl Permits, context: ParseContext) -> Parsed {
        parse::Walker {
            roots: self,
            access: &access,
            context,
        }
        .parse(input)
    }

    /// Completions for `text` as typed (with the leading `/`), for a source
    /// with `access`. `custom` answers for arguments with a provider.
    pub fn suggest(
        &self,
        text: &str,
        access: impl Permits,
        context: &SuggestContext,
        custom: &CustomSuggest<'_>,
    ) -> Suggestions {
        let input = text.strip_prefix('/').unwrap_or(text);
        let offset = text.len() - input.len();
        let walker = parse::Walker {
            roots: self,
            access: &access,
            context: ParseContext {
                allow_selectors: context.allow_selectors,
            },
        };
        let hook = |path: &[ParsedNode], node: &'static CommandNode, partial: &str| {
            let provider = self.provider(path, node)?;
            custom(provider, path, partial)
        };
        let (start, matches) = walker.suggest(input, context, &hook);
        let start = offset + start;
        Suggestions {
            start: text[..start].encode_utf16().count(),
            length: text[start..].encode_utf16().count(),
            matches,
        }
    }

    /// The command tree for a player with `access`: the registered commands
    /// they may use, in vanilla's breadth-first node order.
    pub fn tree(&self, access: impl Permits) -> (Vec<CommandNodeData>, i32) {
        self.tree_for(access, None)
    }

    /// Like `tree`, leaving out game rules a world with `features` lacks.
    pub fn tree_for(
        &self,
        access: impl Permits,
        features: Option<&[String]>,
    ) -> (Vec<CommandNodeData>, i32) {
        let root = CommandNode {
            name: "",
            kind: CommandNodeKind::Root,
            executable: false,
            redirect: None,
            permissions: None,
            children: &[],
        };
        let mut nodes: Vec<Option<CommandNodeData>> = vec![Some(node_data(&root, None))];
        let mut index: HashMap<*const CommandNode, i32> = HashMap::new();
        let mut redirects = Vec::new();
        let mut queue: VecDeque<(&CommandNode, &'static str, usize)> = VecDeque::new();
        let mut root_children = Vec::new();
        for command in &self.commands {
            let child = command.node;
            if !self.root_allowed(child, &access) {
                continue;
            }
            let id = nodes.len() as i32;
            index.insert(child, id);
            root_children.push(id);
            nodes.push(Some(node_data(child, None)));
            queue.push_back((child, command.name, id as usize));
        }
        nodes[0].as_mut().expect("root").children = root_children;
        while let Some((node, command, own)) = queue.pop_front() {
            let mut children = Vec::new();
            for child in node.children {
                if !allowed(child, &access) {
                    continue;
                }
                if let (Some(features), "gamerule", CommandNodeKind::Literal) =
                    (features, command, &child.kind)
                    && tuff_vanilla_data::game_rule::GameRule::get(child.name)
                        .is_some_and(|rule| !rule.enabled_by(features))
                {
                    continue;
                }
                let parser = match child.kind {
                    CommandNodeKind::Argument {
                        parser: p,
                        properties,
                    } => {
                        if !args::SUPPORTED_PARSERS.contains(&p) {
                            continue;
                        }
                        match parser(p, properties) {
                            Some(parser) => Some(parser),
                            None => continue,
                        }
                    }
                    _ => None,
                };
                let id = nodes.len() as i32;
                index.insert(child, id);
                children.push(id);
                let mut data = node_data(child, parser);
                if matches!(child.kind, CommandNodeKind::Argument { .. }) {
                    let providers = self.command(command).map_or(&[][..], |c| c.providers);
                    data.suggestions = providers
                        .iter()
                        .find(|(name, _)| *name == child.name)
                        .map(|(_, provider)| *provider);
                }
                nodes.push(Some(data));
                queue.push_back((child, command, id as usize));
            }
            if let Some(path) = node.redirect {
                redirects.push((own, path));
            }
            nodes[own].as_mut().expect("queued nodes exist").children = children;
        }
        let mut nodes: Vec<CommandNodeData> = nodes.into_iter().flatten().collect();
        for (own, path) in redirects {
            nodes[own].redirect = match path.split_first() {
                None => Some(0),
                Some((first, rest)) => self.command(first).and_then(|c| {
                    let mut node = c.node;
                    for name in rest {
                        node = node.child(name)?;
                    }
                    index.get(&(node as *const _)).copied()
                }),
            };
            if nodes[own].redirect.is_none() {
                nodes[own].flags &= !command_node::REDIRECT;
            }
        }
        (nodes, 0)
    }

    /// Runs `f` with the Commands packet for a player with `access`.
    pub fn with_commands_packet<R>(
        &self,
        access: impl Permits,
        features: Option<&[String]>,
        extra: &crate::world::commands::ClientExtras,
        f: impl FnOnce(&Commands) -> R,
    ) -> R {
        let (mut nodes, root) = self.tree_for(&access, features);
        for name in &extra.roots {
            if self.command(name).is_some() || extra.trees.iter().any(|t| t.name() == *name) {
                continue;
            }
            let literal = nodes.len() as i32;
            nodes[root as usize].children.push(literal);
            push_greedy_literal(&mut nodes, name);
        }
        for tree in &extra.trees {
            if self.command(tree.name()).is_none() {
                push_client_tree(&mut nodes, root, tree, &access);
            }
        }
        let tuff = nodes[root as usize]
            .children
            .iter()
            .copied()
            .find(|id| nodes[*id as usize].name == Some("tuff"));
        if let Some(tuff) = tuff {
            for name in &extra.tuff {
                let literal = nodes.len() as i32;
                let exists = nodes[tuff as usize]
                    .children
                    .iter()
                    .any(|id| nodes[*id as usize].name == Some(name));
                if exists {
                    continue;
                }
                nodes[tuff as usize].children.push(literal);
                push_greedy_literal(&mut nodes, name);
            }
        }
        f(&Commands {
            nodes: &nodes,
            root,
        })
    }
}

/// Appends a feature's tree under `root`, leaving out nodes whose
/// permission `access` lacks.
fn push_client_tree(
    nodes: &mut Vec<CommandNodeData>,
    root: i32,
    tree: &crate::world::commands::ClientTree,
    access: &dyn Permits,
) {
    let allowed = |i: usize| {
        tree.nodes[i]
            .permission
            .as_deref()
            .is_none_or(|node| access.has(node))
    };
    if !allowed(0) {
        return;
    }
    let mut placed: Vec<Option<i32>> = vec![None; tree.nodes.len()];
    let mut queue = std::collections::VecDeque::from([0usize]);
    let first = nodes.len() as i32;
    placed[0] = Some(first);
    nodes[root as usize].children.push(first);
    while let Some(i) = queue.pop_front() {
        let node = &tree.nodes[i];
        let mut flags = if node.parser.is_some() {
            command_node::ARGUMENT
        } else {
            command_node::LITERAL
        };
        if node.executable {
            flags |= command_node::EXECUTABLE;
        }
        if node.redirect.is_some() {
            flags |= command_node::REDIRECT;
        }
        let parser = node
            .parser
            .as_ref()
            .and_then(|(name, props)| parser(name, props.as_deref()));
        let mut children = Vec::new();
        for &child in &node.children {
            let child = usize::from(child);
            if placed[child].is_none() && allowed(child) {
                let id = nodes.len() + queue.len() + 1;
                placed[child] = Some(id as i32);
                queue.push_back(child);
            }
            children.extend(placed[child]);
        }
        nodes.push(CommandNodeData {
            flags,
            children,
            redirect: None,
            name: Some(node.name),
            parser,
            suggestions: (node.suggest && node.parser.is_some()).then_some(ASK_SERVER),
        });
    }
    for (i, node) in tree.nodes.iter().enumerate() {
        let (Some(at), Some(target)) = (placed[i], node.redirect) else {
            continue;
        };
        let target = placed[usize::from(target)];
        let data = &mut nodes[at as usize];
        data.redirect = target;
        if target.is_none() {
            data.flags &= !command_node::REDIRECT;
        }
    }
}

/// Appends an executable literal `name` and its greedy `args` child.
fn push_greedy_literal(nodes: &mut Vec<CommandNodeData>, name: &'static str) {
    let literal = nodes.len() as i32;
    nodes.push(CommandNodeData {
        flags: command_node::LITERAL | command_node::EXECUTABLE,
        children: vec![literal + 1],
        redirect: None,
        name: Some(name),
        parser: None,
        suggestions: None,
    });
    let mut properties = PacketWriter::new();
    properties.write_var_int(2);
    nodes.push(CommandNodeData {
        flags: command_node::ARGUMENT | command_node::EXECUTABLE,
        children: Vec::new(),
        redirect: None,
        name: Some("args"),
        parser: Some((
            CommandArgumentType::from_name("brigadier:string")
                .expect("string parser")
                .id() as i32,
            properties.into_bytes().to_vec(),
        )),
        suggestions: None,
    });
}

/// An argument parser's network ID and encoded properties, or `None` if
/// this server cannot encode them.
fn parser(parser: &str, properties: Option<&str>) -> Option<(i32, Vec<u8>)> {
    let id = CommandArgumentType::from_name(parser)?.id() as i32;
    let value: serde_json::Value = match properties {
        Some(json) => serde_json::from_str(json).ok()?,
        None => serde_json::Value::Null,
    };
    let mut w = PacketWriter::new();
    match parser {
        "minecraft:entity" => {
            let mut flags = 0;
            if value["amount"] == "single" {
                flags |= ENTITY_SINGLE;
            }
            if value["type"] == "players" {
                flags |= ENTITY_PLAYERS_ONLY;
            }
            w.write_unsigned_byte(flags);
        }
        "minecraft:score_holder" => {
            w.write_unsigned_byte(u8::from(value["amount"] == "multiple"));
        }
        "brigadier:float" | "brigadier:double" | "brigadier:integer" | "brigadier:long" => {
            let bounds = [("min", RANGE_MIN), ("max", RANGE_MAX)]
                .map(|(key, flag)| value.get(key).map(|bound| (flag, bound)));
            let flags = bounds
                .iter()
                .flatten()
                .fold(0, |flags, (flag, _)| flags | flag);
            w.write_unsigned_byte(flags);
            for (_, bound) in bounds.into_iter().flatten() {
                match parser {
                    "brigadier:float" => w.write_float(bound.as_f64()? as f32),
                    "brigadier:double" => w.write_double(bound.as_f64()?),
                    "brigadier:integer" => w.write_int(i32::try_from(bound.as_i64()?).ok()?),
                    _ => w.write_long(bound.as_i64()?),
                }
            }
        }
        "brigadier:string" => w.write_var_int(match value["type"].as_str() {
            Some("word") => 0,
            Some("phrase") => 1,
            Some("greedy") => 2,
            _ => return None,
        }),
        "minecraft:time" => w.write_int(i32::try_from(value["min"].as_i64().unwrap_or(0)).ok()?),
        "minecraft:resource"
        | "minecraft:resource_key"
        | "minecraft:resource_or_tag"
        | "minecraft:resource_or_tag_key"
        | "minecraft:resource_selector" => w.write_string(value["registry"].as_str()?),
        _ if properties.is_some() => return None,
        _ => {}
    }
    Some((id, w.into_bytes().to_vec()))
}

fn node_data(node: &CommandNode, parser: Option<(i32, Vec<u8>)>) -> CommandNodeData {
    let kind = match node.kind {
        CommandNodeKind::Root => command_node::ROOT,
        CommandNodeKind::Literal => command_node::LITERAL,
        CommandNodeKind::Argument { .. } => command_node::ARGUMENT,
    };
    let mut flags = kind;
    if node.executable {
        flags |= command_node::EXECUTABLE;
    }
    if !allowed(node, &PermissionLevel::All) {
        flags |= command_node::RESTRICTED;
    }
    if node.redirect.is_some() {
        flags |= command_node::REDIRECT;
    }
    CommandNodeData {
        flags,
        children: Vec::new(),
        redirect: None,
        name: (kind != command_node::ROOT).then_some(node.name),
        parser,
        suggestions: None,
    }
}

/// Tab completions for the text before the cursor: where they start and
/// how much they replace, in UTF-16 code units.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestions {
    pub start: usize,
    pub length: usize,
    pub matches: Vec<String>,
}

/// Formats a float the way the game's messages do: at least one fraction
/// digit, and scientific notation outside `[1e-3, 1e7)`.
pub fn java_float(value: f32) -> String {
    java_number(f64::from(value), format!("{:e}", value.abs()))
}

pub fn java_double(value: f64) -> String {
    java_number(value, format!("{:e}", value.abs()))
}

fn java_number(value: f64, scientific: String) -> String {
    if !value.is_finite() {
        return match value {
            v if v.is_nan() => "NaN".into(),
            v if v > 0.0 => "Infinity".into(),
            _ => "-Infinity".into(),
        };
    }
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0.0"
        } else {
            "0.0"
        }
        .into();
    }
    let sign = if value < 0.0 { "-" } else { "" };
    let (mantissa, exponent) = scientific.split_once('e').expect("{:e} has an exponent");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let exponent: i32 = exponent.parse().expect("{:e} exponent is an integer");
    let magnitude = value.abs();
    if (1e-3..1e7).contains(&magnitude) {
        let (int, frac) = if exponent >= 0 {
            let split = exponent as usize + 1;
            let padded = format!("{digits:0<split$}");
            let (int, frac) = padded.split_at(split);
            (int.to_owned(), frac.to_owned())
        } else {
            let zeros = "0".repeat((-exponent - 1) as usize);
            ("0".to_owned(), format!("{zeros}{digits}"))
        };
        let frac = if frac.is_empty() {
            "0".to_owned()
        } else {
            frac
        };
        format!("{sign}{int}.{frac}")
    } else {
        let (first, rest) = digits.split_at(1);
        let rest = if rest.is_empty() { "0" } else { rest };
        format!("{sign}{first}.{rest}E{exponent}")
    }
}

/// `String.format("%f")`: six fraction digits.
pub fn java_format_f(value: f64) -> String {
    format!("{value:.6}")
}

/// `String.format("%.Nf")` of a double: the shortest digits that identify
/// the value, rounded half up to `digits` fraction digits.
pub fn java_fixed(value: f64, digits: usize) -> String {
    if !value.is_finite() {
        return java_double(value);
    }
    round_half_up(&value.to_string(), digits)
}

/// [`java_fixed`] of a float, which rounds the float's own shortest digits.
pub fn java_fixed_f32(value: f32, digits: usize) -> String {
    if !value.is_finite() {
        return java_float(value);
    }
    round_half_up(&value.to_string(), digits)
}

fn round_half_up(decimal: &str, digits: usize) -> String {
    let (sign, unsigned) = match decimal.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", decimal),
    };
    let (int, frac) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    let mut kept: Vec<u8> = int.bytes().chain(frac.bytes().take(digits)).collect();
    kept.extend(std::iter::repeat_n(b'0', digits.saturating_sub(frac.len())));
    if frac.as_bytes().get(digits).is_some_and(|d| *d >= b'5') {
        let mut carry = true;
        for digit in kept.iter_mut().rev() {
            if *digit == b'9' {
                *digit = b'0';
            } else {
                *digit += 1;
                carry = false;
                break;
            }
        }
        if carry {
            kept.insert(0, b'1');
        }
    }
    let split = kept.len() - digits;
    let int = std::str::from_utf8(&kept[..split]).expect("ascii digits");
    if digits == 0 {
        return format!("{sign}{int}");
    }
    let frac = std::str::from_utf8(&kept[split..]).expect("ascii digits");
    format!("{sign}{int}.{frac}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::encode_packet;
    use crate::protocol::play::GameMode;
    use tuff_wire::{Buffer, PacketReader};

    /// Every top-level command in the vanilla tree, registered or not:
    /// core cannot see which ones features register.
    fn every_vanilla_command() -> Roots {
        Roots::new(
            ROOT.children
                .iter()
                .map(|command| RootCommand::vanilla(command.name, &[]))
                .collect::<Vec<_>>(),
        )
    }

    #[tuff_core::watchdog]
    #[test]
    fn fixed_formats_round_the_shortest_digits_half_up() {
        assert_eq!(java_fixed(1.005, 2), "1.01");
        assert_eq!(java_fixed(0.125, 2), "0.13");
        assert_eq!(java_fixed(99.95, 1), "100.0");
        assert_eq!(java_fixed(59_999_968.0, 0), "59999968");
        assert_eq!(java_fixed(12.5, 0), "13");
        assert_eq!(java_fixed(-3.0, 1), "-3.0");
        assert_eq!(java_fixed(f64::INFINITY, 1), "Infinity");
        assert_eq!(java_fixed_f32(0.2, 2), "0.20");
        assert_eq!(java_fixed_f32(10.0, 2), "10.00");
    }

    fn roots() -> Roots {
        Roots::new([
            RootCommand::vanilla("gamemode", &[]),
            RootCommand::vanilla(
                "time",
                &[("timemarker", ASK_SERVER), ("timeline", ASK_SERVER)],
            ),
            RootCommand::vanilla("teleport", &[]),
            RootCommand::vanilla("tp", &[]),
            RootCommand::vanilla("execute", &[]),
            RootCommand::vanilla("give", &[]),
            RootCommand::vanilla("op", &[("targets", ASK_SERVER)]),
            RootCommand::vanilla("list", &[]),
        ])
    }

    fn names(nodes: &[CommandNodeData], children: &[i32]) -> Vec<&'static str> {
        children
            .iter()
            .filter_map(|c| nodes[*c as usize].name)
            .collect()
    }

    #[tuff_core::watchdog]
    #[test]
    fn gamerule_tree_omits_rules_the_world_features_lack() {
        let roots = Roots::default().with_commands([RootCommand::vanilla("gamerule", &[])]);
        let rules = |features: Option<&[String]>| {
            let (nodes, _) = roots.tree_for(PermissionLevel::Gamemasters, features);
            nodes
                .iter()
                .filter_map(|n| n.name)
                .any(|n| n == "max_minecart_speed")
        };
        let improved = [
            "minecraft:vanilla".to_owned(),
            "minecraft:minecart_improvements".to_owned(),
        ];
        assert!(rules(None));
        assert!(rules(Some(&improved)));
        assert!(!rules(Some(&improved[..1])));
    }

    #[tuff_core::watchdog]
    #[test]
    fn tree_holds_only_registered_commands_the_player_may_use() {
        let roots = roots();
        let (nodes, root) = roots.tree(PermissionLevel::All);
        assert_eq!(names(&nodes, &nodes[root as usize].children), ["list"]);

        let (nodes, root) = roots.tree(PermissionLevel::Gamemasters);
        let root = &nodes[root as usize];
        assert_eq!(root.flags, command_node::ROOT);
        assert_eq!(
            names(&nodes, &root.children),
            [
                "gamemode", "time", "teleport", "tp", "execute", "give", "list"
            ]
        );
        let gamemode = &nodes[root.children[0] as usize];
        assert_eq!(
            gamemode.flags,
            command_node::LITERAL | command_node::RESTRICTED
        );
        let mode = &nodes[gamemode.children[0] as usize];
        assert_eq!(mode.name, Some("gamemode"));
        assert_eq!(
            mode.flags,
            command_node::ARGUMENT | command_node::EXECUTABLE
        );
        let target = &nodes[mode.children[0] as usize];
        let entity_parser = CommandArgumentType::from_name("minecraft:entity").unwrap();
        assert_eq!(
            target.parser,
            Some((entity_parser.id() as i32, vec![ENTITY_PLAYERS_ONLY]))
        );
        let tp = &nodes[root.children[3] as usize];
        assert_eq!(tp.redirect, Some(root.children[2]));
        assert!(tp.flags & command_node::REDIRECT != 0);
        let execute = &nodes[root.children[4] as usize];
        let run = execute
            .children
            .iter()
            .find(|c| nodes[**c as usize].name == Some("run"))
            .unwrap();
        assert_eq!(nodes[*run as usize].redirect, Some(0));
        for (i, node) in nodes.iter().enumerate() {
            assert!(node.children.iter().all(|c| *c as usize > i));
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn every_vanilla_command_encodes_completely() {
        fn count(node: &CommandNode) -> usize {
            let supported = match node.kind {
                CommandNodeKind::Argument { parser, .. } => {
                    args::SUPPORTED_PARSERS.contains(&parser)
                }
                _ => true,
            };
            if !supported {
                return 0;
            }
            1 + node.children.iter().map(count).sum::<usize>()
        }
        let roots = every_vanilla_command();
        let (nodes, _) = roots.tree(PermissionLevel::Owners);
        let expected: usize = roots.commands().iter().map(|c| count(c.node)).sum();
        assert_eq!(nodes.len(), expected + 1);
        for command in roots.commands() {
            let usable = |node: &CommandNode| match node.kind {
                CommandNodeKind::Argument { parser, properties } => {
                    !args::SUPPORTED_PARSERS.contains(&parser)
                        || super::parser(parser, properties).is_some()
                }
                _ => true,
            };
            fn walk(node: &CommandNode, usable: &dyn Fn(&CommandNode) -> bool) {
                assert!(usable(node), "cannot encode {}", node.name);
                node.children.iter().for_each(|c| walk(c, usable));
            }
            walk(command.node, &usable);
        }
    }

    /// A decoded argument node: name, parser, properties as read back, and
    /// suggestion provider.
    type DecodedArgument = (String, &'static str, String, Option<String>);

    fn decode_commands_packet(roots: &Roots, level: PermissionLevel) -> Vec<DecodedArgument> {
        decode_with(roots, level, &Default::default())
    }

    fn decode_with(
        roots: &Roots,
        level: PermissionLevel,
        extra: &crate::world::commands::ClientExtras,
    ) -> Vec<DecodedArgument> {
        let packet = roots.with_commands_packet(level, None, extra, |p| encode_packet(p));
        let mut buf = Buffer::new();
        buf.extend(&packet.finish()).unwrap();
        let mut r = PacketReader::new(buf.try_split_packet().unwrap().unwrap());
        assert_eq!(
            r.read_var_int().unwrap(),
            tuff_vanilla_data::packet::play::Clientbound::Commands.id()
        );
        let count = r.read_var_int().unwrap();
        let mut arguments = Vec::new();
        for _ in 0..count {
            let flags = r.read_unsigned_byte().unwrap();
            for _ in 0..r.read_var_int().unwrap() {
                r.read_var_int().unwrap();
            }
            if flags & command_node::REDIRECT != 0 {
                r.read_var_int().unwrap();
            }
            if flags & 0x03 == command_node::ROOT {
                continue;
            }
            let name = r.read_string().unwrap();
            if flags & 0x03 == command_node::LITERAL {
                continue;
            }
            let parser = CommandArgumentType::from_id(r.read_var_int().unwrap() as u32)
                .unwrap()
                .name();
            let properties = match parser {
                "minecraft:entity" | "minecraft:score_holder" => {
                    r.read_unsigned_byte().unwrap().to_string()
                }
                "minecraft:time" => r.read_int().unwrap().to_string(),
                "minecraft:resource"
                | "minecraft:resource_or_tag"
                | "minecraft:resource_key"
                | "minecraft:resource_or_tag_key"
                | "minecraft:resource_selector" => r.read_string().unwrap(),
                "brigadier:string" => r.read_var_int().unwrap().to_string(),
                "brigadier:float" => {
                    let flags = r.read_unsigned_byte().unwrap();
                    let min = (flags & RANGE_MIN != 0).then(|| r.read_float().unwrap());
                    let max = (flags & RANGE_MAX != 0).then(|| r.read_float().unwrap());
                    format!("{min:?}..{max:?}")
                }
                "brigadier:integer" => {
                    let flags = r.read_unsigned_byte().unwrap();
                    let min = (flags & RANGE_MIN != 0).then(|| r.read_int().unwrap());
                    let max = (flags & RANGE_MAX != 0).then(|| r.read_int().unwrap());
                    format!("{min:?}..{max:?}")
                }
                "brigadier:double" => {
                    let flags = r.read_unsigned_byte().unwrap();
                    let min = (flags & RANGE_MIN != 0).then(|| r.read_double().unwrap());
                    let max = (flags & RANGE_MAX != 0).then(|| r.read_double().unwrap());
                    format!("{min:?}..{max:?}")
                }
                _ => String::new(),
            };
            let suggestions =
                (flags & command_node::SUGGESTIONS != 0).then(|| r.read_string().unwrap());
            arguments.push((name, parser, properties, suggestions));
        }
        assert_eq!(r.read_var_int().unwrap(), 0, "root index");
        assert_eq!(r.remaining(), 0);
        arguments
    }

    #[tuff_core::watchdog]
    #[test]
    fn feature_trees_reach_the_client_with_their_parsers_and_permissions() {
        use crate::world::commands::{ClientExtras, ClientNode, ClientTree};
        let node =
            |name, parser: Option<(&'static str, Option<&str>)>, children: Vec<u16>| ClientNode {
                name,
                parser: parser.map(|(n, p)| (n, p.map(str::to_owned))),
                children,
                executable: true,
                redirect: None,
                permission: None,
                suggest: false,
            };
        let mut secret = node("secret", None, vec![]);
        secret.permission = Some("plug.secret".into());
        let tree = ClientTree {
            nodes: vec![
                node("zzplug", None, vec![1, 3]),
                node(
                    "count",
                    Some(("brigadier:integer", Some(r#"{"min":1}"#))),
                    vec![2],
                ),
                ClientNode {
                    suggest: true,
                    ..node(
                        "who",
                        Some((
                            "minecraft:entity",
                            Some(r#"{"amount":"single","type":"players"}"#),
                        )),
                        vec![],
                    )
                },
                secret,
            ],
        };
        let extra = ClientExtras {
            roots: vec!["zzplug"],
            tuff: vec![],
            trees: vec![tree],
        };
        let found = decode_with(&roots(), PermissionLevel::Admins, &extra);
        let ours: Vec<_> = found
            .iter()
            .filter(|(name, ..)| matches!(name.as_str(), "count" | "who"))
            .map(|(n, p, props, _)| (n.as_str(), *p, props.as_str()))
            .collect();
        assert!(ours.contains(&("count", "brigadier:integer", "Some(1)..None")));
        assert!(ours.contains(&("who", "minecraft:entity", "3")));
        let asked = |name: &str| {
            found
                .iter()
                .find(|(n, ..)| n == name)
                .and_then(|(.., suggestions)| suggestions.clone())
        };
        assert_eq!(asked("who").as_deref(), Some(ASK_SERVER));
        assert_eq!(asked("count"), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn commands_packet_encodes_every_argument() {
        let arguments = decode_commands_packet(&roots(), PermissionLevel::Admins);
        let find = |name: &str| {
            arguments
                .iter()
                .filter(|(n, ..)| n == name)
                .map(|(_, parser, properties, suggestions)| {
                    (*parser, properties.as_str(), suggestions.as_deref())
                })
                .collect::<Vec<_>>()
        };
        assert!(find("target").contains(&(
            "minecraft:entity",
            ENTITY_PLAYERS_ONLY.to_string().as_str(),
            None
        )));
        let min = i32::MIN.to_string();
        assert!(find("time").contains(&("minecraft:time", min.as_str(), None)));
        assert_eq!(
            find("clock"),
            [("minecraft:resource", "minecraft:world_clock", None)]
        );
        assert_eq!(
            find("timeline"),
            [("minecraft:resource", "minecraft:timeline", Some(ASK_SERVER)); 2]
        );
        assert_eq!(
            find("timemarker"),
            [("minecraft:resource_location", "", Some(ASK_SERVER)); 2]
        );
        assert_eq!(
            find("count"),
            [("brigadier:integer", "Some(1)..None", None)]
        );
        assert!(
            find("targets")
                .iter()
                .any(|(parser, _, provider)| *parser == "minecraft:game_profile"
                    && *provider == Some(ASK_SERVER))
        );
        decode_commands_packet(&every_vanilla_command(), PermissionLevel::Owners);
    }

    fn parse(input: &str, level: PermissionLevel) -> Result<Parsed, String> {
        let parsed = roots().parse(
            input,
            level,
            ParseContext {
                allow_selectors: true,
            },
        );
        match parsed.error(input) {
            Some(err) => Err(format!("{}@{:?}", err.key().unwrap(), err.cursor)),
            None => Ok(parsed),
        }
    }

    fn failure(result: Result<Parsed, String>) -> String {
        result.expect_err("parse failed")
    }

    #[tuff_core::watchdog]
    #[test]
    fn parses_commands_and_reports_errors_with_positions() {
        let gm = PermissionLevel::Gamemasters;
        let parsed = parse("gamemode creative", gm).unwrap();
        assert_eq!(parsed.nodes.len(), 2);
        assert_eq!(
            Parsed::value(&parsed.nodes, "gamemode"),
            Some(&Value::GameMode(GameMode::Creative))
        );
        assert!(parse("gamemode creative @a", gm).is_ok());
        assert_eq!(failure(parse("", gm)), "command.unknown.command@Some(0)");
        assert_eq!(
            failure(parse("gamemode", gm)),
            "command.unknown.command@Some(8)"
        );
        assert_eq!(
            failure(parse("nope 1", gm)),
            "command.unknown.command@Some(0)"
        );
        assert_eq!(
            failure(parse("gamemode survival", PermissionLevel::Moderators)),
            "command.unknown.command@Some(0)"
        );
        assert_eq!(
            failure(parse("gamemode hardcore", gm)),
            "argument.gamemode.invalid@Some(17)"
        );
        assert_eq!(
            failure(parse("gamemode creative ", gm)),
            "command.unknown.argument@Some(17)"
        );
        assert_eq!(
            failure(parse("time add day", gm)),
            "parsing.float.expected@Some(9)"
        );
        assert_eq!(
            failure(parse("time add 5x", gm)),
            "argument.time.invalid_unit@Some(11)"
        );
        assert_eq!(
            failure(parse("time set 1 2", gm)),
            "command.unknown.argument@Some(11)"
        );
        assert_eq!(
            failure(parse("time rate 2x", gm)),
            "command.expected.separator@Some(11)"
        );
        assert_eq!(
            failure(parse("time query day twice", gm)),
            "command.unknown.argument@Some(15)"
        );
        assert_eq!(
            failure(parse("time spin", gm)),
            "command.unknown.argument@Some(5)"
        );
        assert!(
            parse("time set -5", gm).is_ok(),
            "a negative duration falls back to a marker"
        );
        assert!(parse("time set noon", gm).is_ok());
        assert!(parse("time of the_end query day repetition", gm).is_ok());
        assert_eq!(
            failure(parse("time of moon pause", gm)),
            "argument.resource.not_found@Some(8)"
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn follows_redirects_and_root_redirects() {
        let gm = PermissionLevel::Gamemasters;
        let tp = parse("tp 1 2 3", gm).unwrap();
        assert_eq!(
            tp.nodes.iter().map(|n| n.node.name).collect::<Vec<_>>(),
            ["tp", "location"]
        );
        let tp = parse("tp Alice Bob", gm).unwrap();
        assert_eq!(
            tp.nodes.iter().map(|n| n.node.name).collect::<Vec<_>>(),
            ["tp", "targets", "destination"]
        );
        let executed = parse("execute as @a at @s run give @s stone 2", gm).unwrap();
        let names: Vec<_> = executed.nodes.iter().map(|n| n.node.name).collect();
        assert_eq!(
            names,
            [
                "execute", "as", "targets", "at", "targets", "run", "give", "targets", "item",
                "count"
            ]
        );
        let chain: Vec<_> = executed
            .nodes
            .iter()
            .filter(|n| n.chain_start)
            .map(|n| n.node.name)
            .collect();
        assert_eq!(chain, ["execute", "give"]);
        assert_eq!(
            failure(parse("execute run", gm)),
            "command.unknown.command@Some(11)"
        );
        assert_eq!(
            failure(parse("execute run ", gm)),
            "command.unknown.command@Some(12)"
        );
        assert_eq!(
            failure(parse("execute as @a", gm)),
            "command.unknown.command@Some(13)"
        );
        assert!(parse("execute if block ~ ~ ~ stone", gm).is_ok());
        assert_eq!(
            failure(parse("execute run list", PermissionLevel::All)),
            "command.unknown.command@Some(0)"
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn suggests_literals_arguments_and_providers() {
        let roots = roots();
        let gm = PermissionLevel::Gamemasters;
        let context = SuggestContext {
            allow_selectors: true,
            player_names: vec!["Alice".into()],
            dimensions: vec!["minecraft:overworld".into()],
            ..Default::default()
        };
        let custom = |provider: &str, path: &[ParsedNode], partial: &str| -> Option<Vec<String>> {
            assert_eq!(provider, ASK_SERVER);
            let last = path.last()?.node.name;
            if last == "set" {
                return Some(
                    ["minecraft:noon", "minecraft:night"]
                        .into_iter()
                        .filter(|m| args::matches_resource(partial, m))
                        .map(str::to_owned)
                        .collect(),
                );
            }
            None
        };
        let suggest = |text: &str, level| roots.suggest(text, level, &context, &custom);
        assert_eq!(
            suggest("/", gm).matches,
            [
                "execute", "gamemode", "give", "list", "teleport", "time", "tp"
            ]
        );
        assert!(
            suggest("/", PermissionLevel::Admins)
                .matches
                .contains(&"op".to_string())
        );
        assert_eq!(suggest("/ga", gm).matches, ["gamemode"]);
        assert_eq!(suggest("/ga", gm).start, 1);
        assert_eq!(
            suggest("/gamemode ", gm).matches,
            ["adventure", "creative", "spectator", "survival"]
        );
        assert_eq!(
            suggest("/gamemode s", gm).matches,
            ["spectator", "survival"]
        );
        assert_eq!(
            suggest("/gamemode creative ", gm).matches,
            ["@a", "@p", "@r", "@s", "Alice"]
        );
        let markers = suggest("/time set n", gm);
        assert_eq!((markers.start, markers.length), (10, 1));
        assert_eq!(markers.matches, ["minecraft:night", "minecraft:noon"]);
        assert_eq!(suggest("/time set é", gm).length, 1);
        assert_eq!(
            suggest("/tp ", gm).matches,
            [
                "@a", "@e", "@n", "@p", "@r", "@s", "Alice", "~", "~ ~", "~ ~ ~"
            ]
        );
        assert_eq!(suggest("/execute run g", gm).matches, ["gamemode", "give"]);
        assert!(
            suggest("/gamemode ", PermissionLevel::All)
                .matches
                .is_empty()
        );
        assert_eq!(suggest("/gamemode creative", gm).matches, ["creative"]);
        assert_eq!(suggest("/gamemode creative", gm).start, 10);
    }

    #[tuff_core::watchdog]
    #[test]
    fn floats_format_like_the_game() {
        for (value, text) in [
            (2.0, "2.0"),
            (0.5, "0.5"),
            (1000.0, "1000.0"),
            (1e-5, "1.0E-5"),
            (0.001, "0.001"),
            (0.000_25, "2.5E-4"),
            (12345.678, "12345.678"),
            (1.5e7, "1.5E7"),
            (-3.25, "-3.25"),
            (0.1, "0.1"),
        ] {
            assert_eq!(java_float(value), text, "{value}");
        }
        assert_eq!(java_double(64.0), "64.0");
        assert_eq!(java_format_f(1.5), "1.500000");
    }
}
