//! Walks the command tree over typed input the way the game's dispatcher
//! does: literals must match a whole word, every argument child is tried,
//! the best partial parse wins, and redirects continue at their target.

use tuff_vanilla_data::command::{CommandNode, CommandNodeKind};

use super::args::{self, ParseContext, SuggestContext, Value};
use super::error::CommandError;
use super::reader::Reader;
use super::{Permits, Roots, allowed};

type NodeSuggest<'a> =
    dyn Fn(&[ParsedNode], &'static CommandNode, &str) -> Option<Vec<String>> + 'a;

#[derive(Debug, Clone)]
pub struct ParsedNode {
    pub node: &'static CommandNode,
    pub start: usize,
    pub end: usize,
    pub value: Option<Value>,
    /// The first node of a command reached through a redirect to the root
    /// (`execute ... run <command>`).
    pub chain_start: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Parsed {
    pub nodes: Vec<ParsedNode>,
    /// Where reading stopped.
    pub end: usize,
    pub errors: Vec<CommandError>,
    /// A redirect was followed with nothing after it.
    pub dangling: bool,
}

impl Parsed {
    /// The error the game reports for this parse, if it cannot execute:
    /// unread input, or a chain that ends before an executable node.
    pub fn error(&self, input: &str) -> Option<CommandError> {
        if self.end < input.len() {
            if self.errors.len() == 1 {
                return Some(self.errors[0].clone());
            }
            let key = if self.nodes.is_empty() {
                "command.unknown.command"
            } else {
                "command.unknown.argument"
            };
            return Some(CommandError::simple(key).at_cursor(self.end));
        }
        let executable = self
            .nodes
            .last()
            .is_some_and(|last| last.node.executable && !self.dangling);
        if !executable {
            return Some(CommandError::simple("command.unknown.command").at_cursor(self.end));
        }
        None
    }

    /// The parsed value of the argument named `name` within `nodes`.
    pub fn value<'a>(nodes: &'a [ParsedNode], name: &str) -> Option<&'a Value> {
        nodes
            .iter()
            .find(|n| {
                n.node.name == name && matches!(n.node.kind, CommandNodeKind::Argument { .. })
            })
            .and_then(|n| n.value.as_ref())
    }
}

fn usable(node: &CommandNode, access: &dyn Permits) -> bool {
    if !allowed(node, access) {
        return false;
    }
    match node.kind {
        CommandNodeKind::Argument { parser, .. } => args::SUPPORTED_PARSERS.contains(&parser),
        _ => true,
    }
}

/// The children worth trying: the literal matching the next word if there
/// is one, else every argument.
fn relevant(children: &'static [CommandNode], reader: &Reader<'_>) -> Vec<&'static CommandNode> {
    let word = reader.peek_word();
    if let Some(literal) = children
        .iter()
        .find(|c| matches!(c.kind, CommandNodeKind::Literal) && c.name == word)
    {
        return vec![literal];
    }
    children
        .iter()
        .filter(|c| matches!(c.kind, CommandNodeKind::Argument { .. }))
        .collect()
}

pub(super) struct Walker<'r> {
    pub roots: &'r Roots,
    pub access: &'r dyn Permits,
    pub context: ParseContext,
}

impl Walker<'_> {
    fn usable(&self, node: &CommandNode, at_root: bool) -> bool {
        if at_root {
            self.roots.root_allowed(node, self.access)
        } else {
            usable(node, self.access)
        }
    }

    pub fn parse(&self, input: &str) -> Parsed {
        let mut reader = Reader::new(input);
        self.parse_nodes(self.roots.children(), &mut reader, true)
    }

    fn parse_nodes(
        &self,
        children: &'static [CommandNode],
        reader: &mut Reader<'_>,
        at_root: bool,
    ) -> Parsed {
        let cursor = reader.cursor();
        let mut errors = Vec::new();
        let mut potentials: Vec<Parsed> = Vec::new();
        for child in relevant(children, reader) {
            if !self.usable(child, at_root) {
                continue;
            }
            let mut r = reader.clone();
            let start = r.cursor();
            let value = match child.kind {
                CommandNodeKind::Literal | CommandNodeKind::Root => {
                    let word = r.read_while(|c| c != ' ');
                    if word != child.name {
                        r.set_cursor(start);
                        errors.push(
                            CommandError::dynamic("argument.literal.incorrect", [child.name])
                                .at(&r),
                        );
                        continue;
                    }
                    None
                }
                CommandNodeKind::Argument { parser, properties } => {
                    match args::parse_argument_with(
                        parser,
                        properties,
                        &mut r,
                        self.context,
                        self.roots.resources(),
                    ) {
                        Ok(value) => Some(value),
                        Err(err) => {
                            errors.push(err);
                            continue;
                        }
                    }
                }
            };
            if r.can_read() && !r.peek_is(' ') {
                errors.push(CommandError::simple("command.expected.separator").at(&r));
                continue;
            }
            let parsed = ParsedNode {
                node: child,
                start,
                end: r.cursor(),
                value,
                chain_start: at_root,
            };
            let redirect = child.redirect;
            if r.can_read_n(if redirect.is_some() { 1 } else { 2 }) {
                r.skip();
                match redirect {
                    Some(path) => {
                        let (target, to_root) = match self.roots.resolve(path) {
                            Some(target) => target,
                            None => {
                                r.set_cursor(start);
                                errors.push(CommandError::simple("command.unknown.command").at(&r));
                                continue;
                            }
                        };
                        let mut sub = self.parse_nodes(target, &mut r, to_root);
                        let dangling = sub.nodes.is_empty() && !r.can_read();
                        let mut nodes = vec![parsed];
                        nodes.append(&mut sub.nodes);
                        reader.set_cursor(sub.end);
                        return Parsed {
                            nodes,
                            end: sub.end,
                            errors: sub.errors,
                            dangling: dangling || sub.dangling,
                        };
                    }
                    None => {
                        let mut sub = self.parse_nodes(child.children, &mut r, false);
                        let mut nodes = vec![parsed];
                        nodes.append(&mut sub.nodes);
                        potentials.push(Parsed {
                            nodes,
                            end: sub.end,
                            errors: sub.errors,
                            dangling: sub.dangling,
                        });
                    }
                }
            } else {
                potentials.push(Parsed {
                    nodes: vec![parsed],
                    end: r.cursor(),
                    errors: Vec::new(),
                    dangling: false,
                });
            }
        }
        if !potentials.is_empty() {
            let input_len = reader.input().len();
            potentials.sort_by(|a, b| {
                let complete = |p: &Parsed| p.end >= input_len;
                complete(b)
                    .cmp(&complete(a))
                    .then(a.errors.is_empty().cmp(&b.errors.is_empty()).reverse())
            });
            let best = potentials.swap_remove(0);
            reader.set_cursor(best.end);
            return best;
        }
        reader.set_cursor(cursor);
        Parsed {
            nodes: Vec::new(),
            end: cursor,
            errors,
            dangling: false,
        }
    }

    /// Completions at the end of `input`: `(start, matches)`, with `start`
    /// the byte offset the matches replace from.
    pub fn suggest(
        &self,
        input: &str,
        context: &SuggestContext,
        custom: &NodeSuggest<'_>,
    ) -> (usize, Vec<String>) {
        let parsed = self.parse(input);
        let cursor = input.len();
        let redirect = |path| self.roots.resolve(path).unwrap_or((&[][..], false));
        let (children, at_root, start, parent_index): (
            &'static [CommandNode],
            bool,
            usize,
            Option<usize>,
        ) = match parsed.nodes.last() {
            None => (self.roots.children(), true, 0, None),
            Some(last) if last.end < cursor => {
                let (children, at_root) = match last.node.redirect {
                    Some(path) => redirect(path),
                    None => (last.node.children, false),
                };
                (
                    children,
                    at_root,
                    last.end + 1,
                    Some(parsed.nodes.len() - 1),
                )
            }
            Some(_) => {
                let index = parsed
                    .nodes
                    .iter()
                    .position(|n| n.start <= cursor && cursor <= n.end)
                    .unwrap_or(parsed.nodes.len() - 1);
                let (children, at_root, parent_index) = match index.checked_sub(1) {
                    Some(previous) => {
                        let node = parsed.nodes[previous].node;
                        let (children, at_root) = match node.redirect {
                            Some(path) => redirect(path),
                            None => (node.children, false),
                        };
                        (children, at_root, Some(previous))
                    }
                    None => (self.roots.children(), true, None),
                };
                (children, at_root, parsed.nodes[index].start, parent_index)
            }
        };
        let start = start.min(cursor);
        let partial = &input[start..cursor];
        let path: &[ParsedNode] = match parent_index {
            Some(i) => &parsed.nodes[..=i],
            None => &[],
        };
        let mut matches = Vec::new();
        for child in children {
            if !self.usable(child, at_root) {
                continue;
            }
            match child.kind {
                CommandNodeKind::Literal | CommandNodeKind::Root => {
                    if child
                        .name
                        .to_lowercase()
                        .starts_with(&partial.to_lowercase())
                    {
                        matches.push(child.name.to_owned());
                    }
                }
                CommandNodeKind::Argument { parser, properties } => {
                    match custom(path, child, partial) {
                        Some(found) => matches.extend(found),
                        None => matches
                            .extend(args::suggest_argument(parser, properties, partial, context)),
                    }
                }
            }
        }
        matches.sort_by_key(|m| m.to_lowercase());
        matches.dedup();
        (start, matches)
    }
}
