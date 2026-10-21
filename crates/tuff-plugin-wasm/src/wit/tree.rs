//! Command trees a plugin registers: validated when registered, and
//! walked host-side to find the handler and parse the arguments of a
//! command line. Player arguments resolve against the roster the caller
//! supplies, and the same tree is flattened for the client's Commands
//! packet by [`client_tree`].

use super::bindings::types::{
    Arg, ArgValue, BlockPos, CommandNode, CommandTree, NodeKind, Parser, Player, TextKind, Vec3,
};

/// Most nodes in one tree.
pub const MAX_NODES: usize = 256;
/// Longest node name.
pub const MAX_NAME: usize = 64;
/// Longest permission node.
pub const MAX_PERMISSION: usize = 128;

/// The tree's root name, its command.
pub fn name(tree: &CommandTree) -> &str {
    tree.nodes.first().map_or("", |node| node.name.as_str())
}

fn greedy(node: &CommandNode) -> bool {
    matches!(
        node.kind,
        NodeKind::Argument(Parser::Text(TextKind::Greedy) | Parser::Message)
    )
}

/// Checks that `tree` is a tree rooted at a literal: every node but the
/// root has one parent, every node is reachable, names are sensible, no
/// sibling names repeat, ranges are not empty and every branch ends in a
/// handler or a redirect.
pub fn validate(tree: &CommandTree) -> Result<(), String> {
    let nodes = &tree.nodes;
    if nodes.is_empty() || nodes.len() > MAX_NODES {
        return Err(format!("a tree has 1 to {MAX_NODES} nodes"));
    }
    if nodes[0].kind != NodeKind::Literal {
        return Err("node 0 must be a literal".into());
    }
    let mut parents = vec![0u32; nodes.len()];
    for (i, node) in nodes.iter().enumerate() {
        if node.name.is_empty()
            || node.name.len() > MAX_NAME
            || node.name.chars().any(char::is_whitespace)
        {
            return Err(format!("node {i}: bad name {:?}", node.name));
        }
        if let Some(p) = &node.permission
            && (p.is_empty() || p.len() > MAX_PERMISSION)
        {
            return Err(format!("node {i}: bad permission"));
        }
        match &node.kind {
            NodeKind::Argument(Parser::Integer(range)) => {
                if let (Some(min), Some(max)) = (range.min, range.max)
                    && min > max
                {
                    return Err(format!("node {i}: empty range"));
                }
            }
            NodeKind::Argument(Parser::Double(range)) => {
                let (min, max) = (range.min, range.max);
                if min.is_some_and(|v| !v.is_finite()) || max.is_some_and(|v| !v.is_finite()) {
                    return Err(format!("node {i}: range is not finite"));
                }
                if let (Some(min), Some(max)) = (min, max)
                    && min > max
                {
                    return Err(format!("node {i}: empty range"));
                }
            }
            _ => {}
        }
        if node.suggest && node.kind == NodeKind::Literal {
            return Err(format!("node {i}: literals take no suggestions"));
        }
        if greedy(node) && !node.children.is_empty() {
            return Err(format!("node {i}: a greedy argument has no children"));
        }
        if let Some(target) = node.redirect {
            if usize::from(target) >= nodes.len() {
                return Err(format!("node {i}: redirect out of range"));
            }
            if !node.children.is_empty() {
                return Err(format!("node {i}: a redirect has no children"));
            }
        } else if node.children.is_empty() && node.executes.is_none() {
            return Err(format!("node {i}: dead end"));
        }
        let mut names = Vec::with_capacity(node.children.len());
        for &child in &node.children {
            let child = usize::from(child);
            if child == 0 || child >= nodes.len() {
                return Err(format!("node {i}: child {child} out of range"));
            }
            parents[child] += 1;
            if names.contains(&&nodes[child].name) {
                return Err(format!("node {i}: duplicate child {}", nodes[child].name));
            }
            names.push(&nodes[child].name);
        }
    }
    if let Some(i) = parents.iter().skip(1).position(|&p| p != 1) {
        return Err(format!("node {}: must have exactly one parent", i + 1));
    }
    let mut seen = vec![false; nodes.len()];
    let mut stack = vec![0usize];
    while let Some(i) = stack.pop() {
        if std::mem::replace(&mut seen[i], true) {
            continue;
        }
        stack.extend(nodes[i].children.iter().map(|&c| usize::from(c)));
    }
    if let Some(i) = seen.iter().position(|s| !s) {
        return Err(format!("node {i}: not reachable from the root"));
    }
    Ok(())
}

/// What a walk needs from the caller.
pub struct Context<'a> {
    /// Whether the sender holds a permission node.
    pub permitted: &'a dyn Fn(&str) -> bool,
    /// The sender's position, for relative coordinates.
    pub origin: Option<[f64; 3]>,
    /// The sender, for `@s`.
    pub sender: Option<Player>,
    /// Online players and where they stand, asked for only when a player
    /// argument is parsed.
    pub online: &'a dyn Fn() -> Vec<(Player, Option<[f64; 3]>)>,
}

/// A command line matched against a tree.
#[derive(Clone, Debug, PartialEq)]
pub struct Matched {
    pub handler: u32,
    pub args: Vec<Arg>,
}

struct Walk<'a> {
    tree: &'a CommandTree,
    cx: &'a Context<'a>,
    line: &'a str,
    /// How far into the line the walk got, and why it stopped there.
    furthest: (usize, String),
    steps: usize,
}

/// Matches `line`, whose first word is the tree's root, against `tree`:
/// the handler of the node the line ends at and the parsed arguments.
pub fn walk(tree: &CommandTree, line: &str, cx: &Context<'_>) -> Result<Matched, String> {
    let (root, rest) = split_word(line);
    if root != name(tree) {
        return Err(format!("{root} is not this command"));
    }
    if let Some(node) = &tree.nodes[0].permission
        && !(cx.permitted)(node)
    {
        return Err("You do not have permission to use this command".into());
    }
    let mut walk = Walk {
        tree,
        cx,
        line,
        furthest: (0, String::new()),
        steps: 0,
    };
    let mut args = Vec::new();
    match walk.descend(0, rest, &mut args) {
        Some(handler) => Ok(Matched { handler, args }),
        None => {
            let (at, why) = walk.furthest;
            let shown = line.get(..at).unwrap_or(line);
            let shown = shown.get(shown.len().saturating_sub(32)..).unwrap_or(shown);
            Err(format!("{why}: ...{shown}<--[HERE]"))
        }
    }
}

/// An argument node that takes guest suggestions at the end of a line,
/// with the arguments parsed before it.
#[derive(Clone, Debug, PartialEq)]
pub struct SuggestAt {
    pub node: String,
    pub args: Vec<Arg>,
    pub partial: String,
}

/// Where the guest is asked for completions when `line` (whose first word
/// is the tree's root) is typed up to the cursor: each suggesting argument
/// node a parse of everything before the last word can reach.
pub fn suggest_targets(tree: &CommandTree, line: &str, cx: &Context<'_>) -> Vec<SuggestAt> {
    let Some(space) = line.rfind(' ') else {
        return Vec::new();
    };
    let (prefix, partial) = (&line[..space], &line[space + 1..]);
    let (root, rest) = split_word(prefix);
    if root != name(tree) {
        return Vec::new();
    }
    if let Some(node) = &tree.nodes[0].permission
        && !(cx.permitted)(node)
    {
        return Vec::new();
    }
    let mut walk = Walk {
        tree,
        cx,
        line,
        furthest: (0, String::new()),
        steps: 0,
    };
    let mut reached = Vec::new();
    walk.reach(0, rest, &mut Vec::new(), &mut reached);
    let mut out = Vec::new();
    for (index, args) in reached {
        let node = &tree.nodes[index];
        let source = node.redirect.map_or(index, usize::from);
        for &child in &tree.nodes[source].children {
            let child = &tree.nodes[usize::from(child)];
            let permitted = child
                .permission
                .as_deref()
                .is_none_or(|p| (cx.permitted)(p));
            if child.suggest && matches!(child.kind, NodeKind::Argument(_)) && permitted {
                out.push(SuggestAt {
                    node: child.name.clone(),
                    args: args.clone(),
                    partial: partial.to_owned(),
                });
            }
        }
    }
    out
}

fn split_word(input: &str) -> (&str, &str) {
    match input.find(' ') {
        Some(end) => (&input[..end], &input[end..]),
        None => (input, ""),
    }
}

impl Walk<'_> {
    fn fail(&mut self, rest: &str, why: &str) {
        let at = self.line.len() - rest.len();
        if at >= self.furthest.0 {
            self.furthest = (at, why.to_owned());
        }
    }

    fn reach(
        &mut self,
        index: usize,
        rest: &str,
        args: &mut Vec<Arg>,
        out: &mut Vec<(usize, Vec<Arg>)>,
    ) {
        self.steps += 1;
        if self.steps > MAX_NODES * 4 || out.len() >= MAX_NODES {
            return;
        }
        if rest.is_empty() {
            out.push((index, args.clone()));
            return;
        }
        let Some(input) = rest.strip_prefix(' ') else {
            return;
        };
        let source = self.tree.nodes[index].redirect.map_or(index, usize::from);
        for &child in &self.tree.nodes[source].children {
            let child = usize::from(child);
            let node = &self.tree.nodes[child];
            if let Some(permission) = &node.permission
                && !(self.cx.permitted)(permission)
            {
                continue;
            }
            match &node.kind {
                NodeKind::Literal => {
                    let (word, after) = split_word(input);
                    if word == node.name {
                        self.reach(child, after, args, out);
                    }
                }
                NodeKind::Argument(parser) => {
                    if let Ok((value, after)) = parse(parser, input, self.cx) {
                        args.push(Arg {
                            name: node.name.clone(),
                            value,
                        });
                        self.reach(child, after, args, out);
                        args.pop();
                    }
                }
            }
        }
    }

    fn descend(&mut self, index: usize, rest: &str, args: &mut Vec<Arg>) -> Option<u32> {
        self.steps += 1;
        if self.steps > MAX_NODES * 4 {
            self.fail(rest, "Command too complex");
            return None;
        }
        let node = &self.tree.nodes[index];
        if rest.is_empty() {
            if node.executes.is_none() {
                self.fail(rest, "Unknown or incomplete command");
            }
            return node.executes;
        }
        let Some(input) = rest.strip_prefix(' ') else {
            self.fail(rest, "Expected whitespace to end one argument");
            return None;
        };
        let source = node.redirect.map_or(index, usize::from);
        let children = &self.tree.nodes[source].children;
        let ordered = children
            .iter()
            .filter(|&&c| self.tree.nodes[usize::from(c)].kind == NodeKind::Literal)
            .chain(
                children
                    .iter()
                    .filter(|&&c| self.tree.nodes[usize::from(c)].kind != NodeKind::Literal),
            );
        for &child in ordered {
            let child = usize::from(child);
            let node = &self.tree.nodes[child];
            if let Some(permission) = &node.permission
                && !(self.cx.permitted)(permission)
            {
                continue;
            }
            match &node.kind {
                NodeKind::Literal => {
                    let (word, after) = split_word(input);
                    if word == node.name
                        && let Some(handler) = self.descend(child, after, args)
                    {
                        return Some(handler);
                    }
                }
                NodeKind::Argument(parser) => match parse(parser, input, self.cx) {
                    Ok((value, after)) => {
                        args.push(Arg {
                            name: node.name.clone(),
                            value,
                        });
                        if let Some(handler) = self.descend(child, after, args) {
                            return Some(handler);
                        }
                        args.pop();
                    }
                    Err(why) => self.fail(input, &why),
                },
            }
        }
        let at = self.line.len() - input.len();
        if at > self.furthest.0 || self.furthest.1.is_empty() {
            self.furthest = (at, "Incorrect argument for command".to_owned());
        }
        None
    }
}

fn word_of(input: &str) -> Result<(&str, &str), String> {
    let (word, rest) = split_word(input);
    if word.is_empty() {
        return Err("Expected a value".into());
    }
    Ok((word, rest))
}

fn quoted(input: &str) -> Result<(String, &str), String> {
    let mut chars = input.char_indices();
    let Some((_, quote)) = chars.next() else {
        return Err("Expected a value".into());
    };
    let mut out = String::new();
    let mut escaped = false;
    for (at, c) in chars {
        if escaped {
            if c != quote && c != '\\' {
                return Err(format!("Invalid escape sequence \\{c} in quoted string"));
            }
            out.push(c);
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == quote {
            let rest = &input[at + c.len_utf8()..];
            if !rest.is_empty() && !rest.starts_with(' ') {
                return Err("Expected whitespace to end one argument".into());
            }
            return Ok((out, rest));
        } else {
            out.push(c);
        }
    }
    Err("Unclosed quoted string".into())
}

fn coordinate(word: &str, origin: Option<f64>) -> Result<f64, String> {
    let value = |text: &str| -> Result<f64, String> {
        text.parse::<f64>()
            .ok()
            .filter(|v| v.is_finite())
            .ok_or_else(|| format!("Invalid coordinate {word}"))
    };
    match word.strip_prefix('~') {
        Some(offset) => {
            let origin = origin.ok_or("Relative coordinates need a position")?;
            Ok(origin
                + if offset.is_empty() {
                    0.0
                } else {
                    value(offset)?
                })
        }
        None if word.starts_with('^') => Err("Local coordinates are not supported".into()),
        None => value(word),
    }
}

fn coordinates(
    input: &str,
    origin: Option<[f64; 3]>,
) -> Result<([f64; 3], [&str; 3], &str), String> {
    let (x, rest) = word_of(input)?;
    let (y, rest) = word_of(rest.strip_prefix(' ').ok_or("Incomplete position")?)?;
    let (z, rest) = word_of(rest.strip_prefix(' ').ok_or("Incomplete position")?)?;
    let words = [x, y, z];
    let mut out = [0.0; 3];
    for axis in 0..3 {
        out[axis] = coordinate(words[axis], origin.map(|o| o[axis]))?;
    }
    Ok((out, words, rest))
}

/// Parses one argument from the start of `input`: its value and the input
/// after it.
pub fn parse<'a>(
    parser: &Parser,
    input: &'a str,
    cx: &Context<'_>,
) -> Result<(ArgValue, &'a str), String> {
    let origin = cx.origin;
    Ok(match parser {
        Parser::Boolean => {
            let (word, rest) = word_of(input)?;
            let value = match word {
                "true" => true,
                "false" => false,
                _ => return Err(format!("Invalid boolean {word}")),
            };
            (ArgValue::Boolean(value), rest)
        }
        Parser::Integer(range) => {
            let (word, rest) = word_of(input)?;
            let value: i32 = word
                .parse()
                .map_err(|_| format!("Invalid integer {word}"))?;
            if range.min.is_some_and(|min| value < min) || range.max.is_some_and(|max| value > max)
            {
                return Err(format!("Integer {value} is out of range"));
            }
            (ArgValue::Integer(value), rest)
        }
        Parser::Double(range) => {
            let (word, rest) = word_of(input)?;
            let value = word
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite())
                .ok_or_else(|| format!("Invalid double {word}"))?;
            if range.min.is_some_and(|min| value < min) || range.max.is_some_and(|max| value > max)
            {
                return Err(format!("Double {value} is out of range"));
            }
            (ArgValue::Double(value), rest)
        }
        Parser::Text(TextKind::Word) => {
            let (word, rest) = word_of(input)?;
            (ArgValue::Text(word.to_owned()), rest)
        }
        Parser::Text(TextKind::Phrase) if input.starts_with(['"', '\'']) => {
            let (text, rest) = quoted(input)?;
            (ArgValue::Text(text), rest)
        }
        Parser::Text(TextKind::Phrase) => {
            let (word, rest) = word_of(input)?;
            (ArgValue::Text(word.to_owned()), rest)
        }
        Parser::Text(TextKind::Greedy) | Parser::Message if input.is_empty() => {
            return Err("Expected a value".into());
        }
        Parser::Text(TextKind::Greedy) => (ArgValue::Text(input.to_owned()), ""),
        Parser::Message => (ArgValue::Message(input.to_owned()), ""),
        Parser::BlockPos => {
            let block_origin = origin.map(|o| o.map(f64::floor));
            let (at, words, rest) = coordinates(input, block_origin)?;
            let mut pos = [0i32; 3];
            for axis in 0..3 {
                if !words[axis].starts_with('~') && words[axis].contains('.') {
                    return Err(format!("Invalid integer {}", words[axis]));
                }
                let v = at[axis].floor();
                if v < f64::from(i32::MIN) || v > f64::from(i32::MAX) {
                    return Err("Position out of range".into());
                }
                pos[axis] = v as i32;
            }
            let [x, y, z] = pos;
            (ArgValue::BlockPos(BlockPos { x, y, z }), rest)
        }
        Parser::Vec3 => {
            let ([x, y, z], _, rest) = coordinates(input, origin)?;
            (ArgValue::Vec3(Vec3 { x, y, z }), rest)
        }
        Parser::BlockState => {
            let (word, rest) = word_of(input)?;
            let state = crate::registry::normalize(word)
                .ok_or_else(|| format!("Unknown block state {word}"))?;
            (ArgValue::BlockState(state), rest)
        }
        Parser::Item => {
            let (word, rest) = word_of(input)?;
            let item = tuff_vanilla_data::Item::from_name(word)
                .or_else(|| tuff_vanilla_data::Item::from_name(&format!("minecraft:{word}")))
                .ok_or_else(|| format!("Unknown item {word}"))?;
            (ArgValue::Item(item.name().to_owned()), rest)
        }
        Parser::Player | Parser::Players => {
            let (word, rest) = word_of(input)?;
            let many = matches!(parser, Parser::Players);
            let found: Vec<Player> = match word {
                "@s" => cx.sender.clone().into_iter().collect(),
                "@a" if many => online(cx).into_iter().map(|(p, _)| p).collect(),
                "@p" => nearest(cx).into_iter().collect(),
                "@r" => random(cx).into_iter().collect(),
                "@a" | "@e" => {
                    return Err(format!("Selector {word} is not supported here"));
                }
                name if name.starts_with('@') => return Err(format!("Unknown selector {name}")),
                name => online(cx)
                    .into_iter()
                    .map(|(p, _)| p)
                    .filter(|p| p.name.eq_ignore_ascii_case(name))
                    .collect(),
            };
            if found.is_empty() {
                return Err("No player was found".into());
            }
            if many {
                (ArgValue::Players(found), rest)
            } else {
                (
                    ArgValue::Player(found.into_iter().next().expect("one")),
                    rest,
                )
            }
        }
    })
}

fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    a.iter().zip(b).map(|(a, b)| (a - b) * (a - b)).sum()
}

fn online(cx: &Context<'_>) -> Vec<(Player, Option<[f64; 3]>)> {
    (cx.online)()
}

/// The online player nearest the sender's position; the first one when
/// there is no position to measure from.
fn nearest(cx: &Context<'_>) -> Option<Player> {
    let online = online(cx);
    let Some(origin) = cx.origin else {
        return online.into_iter().next().map(|(p, _)| p);
    };
    online
        .into_iter()
        .filter_map(|(player, at)| Some((distance(origin, at?), player)))
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, player)| player)
}

/// A random online player.
fn random(cx: &Context<'_>) -> Option<Player> {
    let mut online = online(cx);
    if online.is_empty() {
        return None;
    }
    let pick = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos() as usize)
        % online.len();
    Some(online.swap_remove(pick).0)
}

/// Parser, as the client's Commands packet names it, with its encoded
/// properties.
pub fn client_parser(parser: &Parser) -> (&'static str, Option<String>) {
    fn bound<T: std::fmt::Display>(min: Option<T>, max: Option<T>) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(min) = min {
            parts.push(format!("\"min\":{min}"));
        }
        if let Some(max) = max {
            parts.push(format!("\"max\":{max}"));
        }
        (!parts.is_empty()).then(|| format!("{{{}}}", parts.join(",")))
    }
    let string = |kind: &str| Some(format!("{{\"type\":\"{kind}\"}}"));
    match parser {
        Parser::Boolean => ("brigadier:bool", None),
        Parser::Integer(r) => ("brigadier:integer", bound(r.min, r.max)),
        Parser::Double(r) => ("brigadier:double", bound(r.min, r.max)),
        Parser::Text(TextKind::Word) => ("brigadier:string", string("word")),
        Parser::Text(TextKind::Phrase) => ("brigadier:string", string("phrase")),
        Parser::Text(TextKind::Greedy) => ("brigadier:string", string("greedy")),
        Parser::BlockPos => ("minecraft:block_pos", None),
        Parser::Vec3 => ("minecraft:vec3", None),
        Parser::Player => (
            "minecraft:entity",
            Some(r#"{"amount":"single","type":"players"}"#.into()),
        ),
        Parser::Players => (
            "minecraft:entity",
            Some(r#"{"amount":"multiple","type":"players"}"#.into()),
        ),
        Parser::BlockState => ("minecraft:block_state", None),
        Parser::Item => ("minecraft:item_stack", None),
        Parser::Message => ("minecraft:message", None),
    }
}
