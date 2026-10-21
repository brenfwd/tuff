// SPDX-License-Identifier: MIT
//! Typed command trees. Build a tree with [`command`], [`literal`] and
//! [`argument`], attach handlers with [`Node::executes`] (or
//! [`Node::with`] for a struct of arguments declared with [`crate::args!`])
//! and register it; [`crate::plugin!`] routes each invocation to its
//! handler, so no handler ids or argument lookups are written by hand:
//!
//! ```ignore
//! use tuff_plugin_sdk::command::{argument, command, literal, parser};
//!
//! tuff_plugin_sdk::args! {
//!     struct Sum { a: i32, b: i32 }
//! }
//!
//! command("calc")
//!     .executes(|ctx| ctx.reply("usage: /calc sum <a> <b>").map(|()| 0))
//!     .then(literal("sum").with(|ctx, Sum { a, b }| {
//!         ctx.reply(format!("{a} + {b} = {}", a + b))?;
//!         Ok(a + b)
//!     }))
//!     .then(argument("level", parser::double_in(0.0..=1.0)).suggests(|_| vec!["0.5".into()]))
//!     .register()?;
//! ```
//!
//! Handlers run inside the plugin's `run-command` callback; one that needs
//! to wait (a capability call, [`crate::sleep`]) uses
//! [`Node::executes_async`] and answers through its [`Ctx`], whose
//! [`Replier`] keeps reaching the sender after the callback returned.

use std::cell::RefCell;
use std::future::Future;
use std::ops::{Bound, RangeBounds};
use std::rc::Rc;

use crate::bindings::tuff::plugin::host;
use crate::reply::Replier;
use crate::text::Styled;
use crate::types::{
    Arg, ArgValue, BlockPos, CommandNode, CommandTree, DoubleRange, IntRange, Invocation, NodeKind,
    Parser, Player, RegisterError, SuggestRequest, TextKind, Vec3,
};

/// What a handler returns: the command's result value, or a message the
/// sender sees as the failure.
pub type Outcome = Result<i32, String>;

type Handler = Rc<dyn Fn(Ctx) -> Outcome>;
type Suggester = Rc<dyn Fn(&Suggest) -> Vec<String>>;

/// The handler id of commands registered with [`crate::register_command`],
/// which reach [`crate::Plugin::command`] with their line.
pub(crate) const LINE_HANDLER: u32 = u32::MAX;

/// The arguments the host parsed for an invocation, by node name.
#[derive(Clone, Debug, Default)]
pub struct Args(Vec<Arg>);

impl Args {
    pub fn new(args: Vec<Arg>) -> Self {
        Self(args)
    }

    /// The raw value of argument `name`.
    pub fn value(&self, name: &str) -> Option<&ArgValue> {
        self.0.iter().find(|a| a.name == name).map(|a| &a.value)
    }

    /// Argument `name` as `T`; an `Option<T>` is `None` when the argument
    /// was not given.
    pub fn get<T: ArgType>(&self, name: &str) -> Result<T, String> {
        T::extract(self, name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arg> {
        self.0.iter()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// One run of a command, handed to its handler.
pub struct Ctx {
    /// The handler id the host matched.
    pub handler: u32,
    /// Who ran it; `None` for the console.
    pub sender: Option<Player>,
    pub args: Args,
    /// The command as run, without its slash, after alias rewriting.
    pub line: String,
    replier: Replier,
}

impl Ctx {
    pub fn new(call: Invocation) -> Self {
        let replier = Replier::to(call.sender.as_ref().map(|p| p.uuid));
        Self {
            handler: call.handler,
            sender: call.sender,
            args: Args(call.args),
            line: call.line,
            replier,
        }
    }

    /// Argument `name` as `T`.
    pub fn get<T: ArgType>(&self, name: &str) -> Result<T, String> {
        self.args.get(name)
    }

    /// The player who ran the command, or the error "players only".
    pub fn player(&self) -> Result<&Player, String> {
        self.sender
            .as_ref()
            .ok_or_else(|| "players only".to_owned())
    }

    /// Messages the sender, also after an `await` (see [`Replier`]).
    pub fn reply(&self, text: impl AsRef<str>) -> Result<(), crate::HostError> {
        self.replier.reply(text.as_ref())
    }

    /// A handle that messages the sender, to move into other tasks.
    pub fn replier(&self) -> Replier {
        self.replier.clone()
    }
}

/// A tab-completion request for an argument whose node
/// [`suggests`](Node::suggests).
pub struct Suggest {
    /// The argument node being completed.
    pub node: String,
    /// What the player has typed of it.
    pub partial: String,
    pub sender: Option<Player>,
    /// The arguments parsed before it.
    pub args: Args,
    /// The command line typed so far, without its slash.
    pub line: String,
}

/// A Rust type an argument parses to: its default parser and how to read
/// it back from the host's parsed value.
pub trait ArgType: Sized {
    /// Whether the argument may be left out (`Option<T>`).
    const OPTIONAL: bool = false;

    fn parser() -> Parser;

    fn from_value(value: &ArgValue) -> Option<Self>;

    fn extract(args: &Args, name: &str) -> Result<Self, String> {
        let value = args
            .value(name)
            .ok_or_else(|| format!("missing argument {name}"))?;
        Self::from_value(value).ok_or_else(|| format!("argument {name} has the wrong type"))
    }
}

impl<T: ArgType> ArgType for Option<T> {
    const OPTIONAL: bool = true;

    fn parser() -> Parser {
        T::parser()
    }

    fn from_value(value: &ArgValue) -> Option<Self> {
        T::from_value(value).map(Some)
    }

    fn extract(args: &Args, name: &str) -> Result<Self, String> {
        match args.value(name) {
            None => Ok(None),
            Some(_) => T::extract(args, name).map(Some),
        }
    }
}

impl ArgType for bool {
    fn parser() -> Parser {
        Parser::Boolean
    }

    fn from_value(value: &ArgValue) -> Option<Self> {
        match value {
            ArgValue::Boolean(b) => Some(*b),
            _ => None,
        }
    }
}

impl ArgType for i32 {
    fn parser() -> Parser {
        parser::integer()
    }

    fn from_value(value: &ArgValue) -> Option<Self> {
        match value {
            ArgValue::Integer(i) => Some(*i),
            _ => None,
        }
    }
}

impl ArgType for f64 {
    fn parser() -> Parser {
        parser::double()
    }

    fn from_value(value: &ArgValue) -> Option<Self> {
        match value {
            ArgValue::Double(d) => Some(*d),
            ArgValue::Integer(i) => Some(f64::from(*i)),
            _ => None,
        }
    }
}

/// A word or a quoted phrase; any text-like value reads as a `String`.
impl ArgType for String {
    fn parser() -> Parser {
        Parser::Text(TextKind::Phrase)
    }

    fn from_value(value: &ArgValue) -> Option<Self> {
        match value {
            ArgValue::Text(t)
            | ArgValue::BlockState(t)
            | ArgValue::Item(t)
            | ArgValue::Message(t) => Some(t.clone()),
            _ => None,
        }
    }
}

impl ArgType for BlockPos {
    fn parser() -> Parser {
        Parser::BlockPos
    }

    fn from_value(value: &ArgValue) -> Option<Self> {
        match value {
            ArgValue::BlockPos(p) => Some(*p),
            _ => None,
        }
    }
}

impl ArgType for Vec3 {
    fn parser() -> Parser {
        Parser::Vec3
    }

    fn from_value(value: &ArgValue) -> Option<Self> {
        match value {
            ArgValue::Vec3(v) => Some(*v),
            _ => None,
        }
    }
}

impl ArgType for Player {
    fn parser() -> Parser {
        Parser::Player
    }

    fn from_value(value: &ArgValue) -> Option<Self> {
        match value {
            ArgValue::Player(p) => Some(p.clone()),
            _ => None,
        }
    }
}

/// A player selector: every player it matched.
impl ArgType for Vec<Player> {
    fn parser() -> Parser {
        Parser::Players
    }

    fn from_value(value: &ArgValue) -> Option<Self> {
        match value {
            ArgValue::Players(p) => Some(p.clone()),
            ArgValue::Player(p) => Some(vec![p.clone()]),
            _ => None,
        }
    }
}

macro_rules! text_args {
    ($($(#[$meta:meta])* $name:ident => $parser:expr, $variant:ident;)*) => {$(
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Eq)]
        pub struct $name(pub String);

        impl std::ops::Deref for $name {
            type Target = str;

            fn deref(&self) -> &str {
                &self.0
            }
        }

        impl ArgType for $name {
            fn parser() -> Parser {
                $parser
            }

            fn from_value(value: &ArgValue) -> Option<Self> {
                match value {
                    ArgValue::$variant(t) => Some(Self(t.clone())),
                    _ => None,
                }
            }
        }
    )*};
}

text_args! {
    /// One unquoted word.
    Word => Parser::Text(TextKind::Word), Text;
    /// The rest of the line.
    Greedy => Parser::Text(TextKind::Greedy), Text;
    /// The rest of the line as a chat message (selectors resolved).
    Message => Parser::Message, Message;
    /// A block state such as `minecraft:oak_stairs[facing=east]`.
    BlockState => Parser::BlockState, BlockState;
    /// An item id.
    Item => Parser::Item, Item;
}

/// Parsers for [`argument`] nodes.
pub mod parser {
    use super::*;

    pub fn boolean() -> Parser {
        Parser::Boolean
    }

    pub fn integer() -> Parser {
        Parser::Integer(IntRange {
            min: None,
            max: None,
        })
    }

    /// An integer in `range`, such as `1..=3` or `0..`.
    pub fn integer_in(range: impl RangeBounds<i32>) -> Parser {
        let min = match range.start_bound() {
            Bound::Included(&v) => Some(v),
            Bound::Excluded(&v) => Some(v.saturating_add(1)),
            Bound::Unbounded => None,
        };
        let max = match range.end_bound() {
            Bound::Included(&v) => Some(v),
            Bound::Excluded(&v) => Some(v.saturating_sub(1)),
            Bound::Unbounded => None,
        };
        Parser::Integer(IntRange { min, max })
    }

    pub fn double() -> Parser {
        Parser::Double(DoubleRange {
            min: None,
            max: None,
        })
    }

    /// A double in `range`; an excluded bound is taken as included.
    pub fn double_in(range: impl RangeBounds<f64>) -> Parser {
        let bound = |b: Bound<&f64>| match b {
            Bound::Included(&v) | Bound::Excluded(&v) => Some(v),
            Bound::Unbounded => None,
        };
        Parser::Double(DoubleRange {
            min: bound(range.start_bound()),
            max: bound(range.end_bound()),
        })
    }

    pub fn word() -> Parser {
        Parser::Text(TextKind::Word)
    }

    pub fn phrase() -> Parser {
        Parser::Text(TextKind::Phrase)
    }

    pub fn greedy() -> Parser {
        Parser::Text(TextKind::Greedy)
    }

    pub fn block_pos() -> Parser {
        Parser::BlockPos
    }

    pub fn vec3() -> Parser {
        Parser::Vec3
    }

    pub fn player() -> Parser {
        Parser::Player
    }

    pub fn players() -> Parser {
        Parser::Players
    }

    pub fn block_state() -> Parser {
        Parser::BlockState
    }

    pub fn item() -> Parser {
        Parser::Item
    }

    pub fn message() -> Parser {
        Parser::Message
    }
}

/// One argument of a [`FromArgs`] struct.
pub struct ArgSpec {
    pub name: &'static str,
    pub parser: Parser,
    pub optional: bool,
}

/// A struct of arguments read from an invocation, declared with
/// [`crate::args!`]: its fields become a chain of argument nodes in order.
pub trait FromArgs: Sized {
    fn spec() -> Vec<ArgSpec>;

    fn from_args(args: &Args) -> Result<Self, String>;
}

/// A command tree node being built.
pub struct Node {
    name: String,
    kind: NodeKind,
    children: Vec<Node>,
    handler: Option<Handler>,
    permission: Option<String>,
    suggester: Option<Suggester>,
    label: Option<String>,
    redirect: Option<String>,
    about: Option<String>,
    help: bool,
    level: Option<u8>,
}

/// While registering: makes `/name ...` run as `target ...`, one of this
/// plugin's commands or `tuff <subcommand>`.
pub fn alias(name: &str, target: &str) -> Result<(), RegisterError> {
    host::register_alias(name, target)
}

/// A command's root node: `/name`.
pub fn command(name: impl Into<String>) -> Node {
    literal(name)
}

/// A fixed word.
pub fn literal(name: impl Into<String>) -> Node {
    Node {
        name: name.into(),
        kind: NodeKind::Literal,
        children: Vec::new(),
        handler: None,
        permission: None,
        suggester: None,
        label: None,
        redirect: None,
        about: None,
        help: false,
        level: None,
    }
}

/// An argument named `name`, parsed by `parser` (see [`parser`]).
pub fn argument(name: impl Into<String>, parser: Parser) -> Node {
    Node {
        kind: NodeKind::Argument(parser),
        ..literal(name)
    }
}

/// An argument parsed as `T` by its default parser.
pub fn arg<T: ArgType>(name: impl Into<String>) -> Node {
    argument(name, T::parser())
}

impl Node {
    pub fn then(mut self, child: Node) -> Self {
        self.children.push(child);
        self
    }

    /// Runs `handler` when the input ends at this node.
    pub fn executes(mut self, handler: impl Fn(Ctx) -> Outcome + 'static) -> Self {
        self.handler = Some(Rc::new(handler));
        self
    }

    /// Spawns `handler`'s future when the input ends at this node; the
    /// command counts as run (result 1) once the future first waits.
    pub fn executes_async<F>(self, handler: impl Fn(Ctx) -> F + 'static) -> Self
    where
        F: Future<Output = ()> + 'static,
    {
        self.executes(move |ctx| {
            crate::spawn(handler(ctx));
            Ok(1)
        })
    }

    /// Appends `A`'s fields as a chain of argument nodes ending in
    /// `handler`, which gets them parsed. Every node from which only
    /// optional fields remain also runs it, this node included.
    pub fn with<A: FromArgs>(mut self, handler: impl Fn(Ctx, A) -> Outcome + 'static) -> Self {
        let handler: Handler = Rc::new(move |ctx: Ctx| {
            let args = A::from_args(&ctx.args)?;
            handler(ctx, args)
        });
        let spec = A::spec();
        let mut chain: Option<Node> = None;
        let mut rest_optional = true;
        for field in spec.into_iter().rev() {
            let mut node = argument(field.name, field.parser);
            if let Some(next) = chain.take() {
                node = node.then(next);
            }
            if rest_optional {
                node.handler = Some(handler.clone());
            }
            rest_optional &= field.optional;
            chain = Some(node);
        }
        if rest_optional {
            self.handler = Some(handler);
        }
        if let Some(chain) = chain {
            self.children.push(chain);
        }
        self
    }

    /// Only senders holding permission node `node` see and use this node.
    pub fn permission(mut self, node: impl Into<String>) -> Self {
        self.permission = Some(node.into());
        self
    }

    /// Like [`permission`](Node::permission), and declares `node` when
    /// the tree is registered: `level` 0 gives it to every player, 1 to 4
    /// to operators of that level or above.
    pub fn requires(mut self, node: impl Into<String>, level: u8) -> Self {
        self.permission = Some(node.into());
        self.level = Some(level);
        self
    }

    /// Answers tab-completion for this argument; the SDK keeps only the
    /// answers that start with what was typed (ignoring case).
    pub fn suggests(mut self, suggester: impl Fn(&Suggest) -> Vec<String> + 'static) -> Self {
        self.suggester = Some(Rc::new(suggester));
        self
    }

    /// Names this node for [`Node::redirect`].
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Continues parsing at the node [`label`](Node::label)led `label`, as
    /// `/execute run` does; a redirecting node has no children.
    pub fn redirect(mut self, label: impl Into<String>) -> Self {
        self.redirect = Some(label.into());
        self
    }

    /// What this node's command does, for [`Node::help`].
    pub fn about(mut self, text: impl Into<String>) -> Self {
        self.about = Some(text.into());
        self
    }

    /// Adds a `help` subcommand that lists every form of the command the
    /// sender may run, with its [`about`](Node::about) text; the root
    /// runs it too when it has no handler of its own.
    pub fn help(mut self) -> Self {
        self.help = true;
        self
    }

    /// Registers the tree as command `/name`.
    pub fn register(self) -> Result<(), RegisterError> {
        self.declare_permissions()?;
        let tree = self.install(false).map_err(RegisterError::Invalid)?;
        host::register_command(&tree)
    }

    /// Registers the tree as `/tuff name`; handlers see the line as
    /// `tuff name ...`.
    pub fn register_tuff_subcommand(self) -> Result<(), RegisterError> {
        self.declare_permissions()?;
        let tree = self.install(true).map_err(RegisterError::Invalid)?;
        host::register_tuff_subcommand(&tree)
    }

    fn declare_permissions(&self) -> Result<(), RegisterError> {
        if let (Some(node), Some(level)) = (&self.permission, self.level) {
            host::declare_permission(node, level)?;
        }
        self.children.iter().try_for_each(Node::declare_permissions)
    }

    /// Flattens the tree into the WIT form and installs its handlers and
    /// suggesters for [`dispatch`] and [`suggest`]; what registering sends.
    pub fn install(mut self, tuff: bool) -> Result<CommandTree, String> {
        if std::mem::take(&mut self.help) {
            self = with_help(self, tuff);
        }
        let root = self.name.clone();
        let mut flat = Flat::default();
        flat.add(self);
        let mut nodes = Vec::with_capacity(flat.nodes.len());
        let base = REGISTRY.with(|r| r.borrow().handlers.len()) as u32;
        let mut handlers = Vec::new();
        let mut suggesters = Vec::new();
        for pending in flat.nodes {
            let redirect = match &pending.redirect {
                Some(label) => Some(
                    flat.labels
                        .iter()
                        .find(|(l, _)| l == label)
                        .map(|&(_, i)| i)
                        .ok_or_else(|| format!("no node is labelled {label}"))?,
                ),
                None => None,
            };
            let executes = pending.handler.map(|h| {
                handlers.push(h);
                base + handlers.len() as u32 - 1
            });
            let suggest = pending.suggester.is_some();
            if let Some(s) = pending.suggester {
                suggesters.push(Suggestion {
                    root: root.clone(),
                    tuff,
                    node: pending.name.clone(),
                    suggester: s,
                });
            }
            nodes.push(CommandNode {
                name: pending.name,
                kind: pending.kind,
                children: pending.children,
                executes,
                redirect,
                permission: pending.permission,
                suggest,
            });
        }
        REGISTRY.with(|r| {
            let mut r = r.borrow_mut();
            r.handlers.extend(handlers);
            r.suggesters.extend(suggesters);
        });
        Ok(CommandTree { nodes })
    }
}

/// One line of a command's help: its usage, what it does and the
/// permission it needs.
struct HelpLine {
    text: String,
    permission: Option<String>,
}

fn help_lines(
    node: &Node,
    path: &mut Vec<String>,
    permission: Option<&str>,
    out: &mut Vec<HelpLine>,
) {
    path.push(match node.kind {
        NodeKind::Literal => node.name.clone(),
        NodeKind::Argument(_) => format!("<{}>", node.name),
    });
    let permission = node.permission.as_deref().or(permission);
    if node.handler.is_some() {
        let usage = format!("/{}", path.join(" ")).gold();
        out.push(HelpLine {
            text: match &node.about {
                Some(about) => crate::text!(usage, " - ", about),
                None => usage.to_string(),
            },
            permission: permission.map(str::to_owned),
        });
    }
    for child in &node.children {
        help_lines(child, path, permission, out);
    }
    path.pop();
}

fn with_help(mut root: Node, tuff: bool) -> Node {
    let mut lines = Vec::new();
    let mut path = if tuff {
        vec!["tuff".to_owned()]
    } else {
        Vec::new()
    };
    help_lines(&root, &mut path, None, &mut lines);
    let handler: Handler = Rc::new(move |ctx: Ctx| {
        let mut shown = 0;
        for line in &lines {
            let allowed = match &line.permission {
                Some(node) => host::has_permission(node) != Some(false),
                None => true,
            };
            if allowed {
                ctx.reply(&line.text)?;
                shown += 1;
            }
        }
        Ok(shown)
    });
    let mut help = literal("help").about("Lists these commands");
    help.handler = Some(handler.clone());
    if root.handler.is_none() {
        root.handler = Some(handler);
    }
    root.then(help)
}

struct Pending {
    name: String,
    kind: NodeKind,
    children: Vec<u16>,
    handler: Option<Handler>,
    permission: Option<String>,
    suggester: Option<Suggester>,
    redirect: Option<String>,
}

#[derive(Default)]
struct Flat {
    nodes: Vec<Pending>,
    labels: Vec<(String, u16)>,
}

impl Flat {
    /// Adds `node` and its subtree in preorder; returns its index.
    fn add(&mut self, node: Node) -> u16 {
        let index = self.nodes.len() as u16;
        if let Some(label) = node.label {
            self.labels.push((label, index));
        }
        self.nodes.push(Pending {
            name: node.name,
            kind: node.kind,
            children: Vec::new(),
            handler: node.handler,
            permission: node.permission,
            suggester: node.suggester,
            redirect: node.redirect,
        });
        let children: Vec<u16> = node.children.into_iter().map(|c| self.add(c)).collect();
        self.nodes[usize::from(index)].children = children;
        index
    }
}

struct Suggestion {
    root: String,
    tuff: bool,
    node: String,
    suggester: Suggester,
}

#[derive(Default)]
struct Registry {
    handlers: Vec<Handler>,
    suggesters: Vec<Suggestion>,
}

thread_local! {
    static REGISTRY: RefCell<Registry> = RefCell::new(Registry::default());
}

/// Whether `call` is for a command registered with
/// [`crate::register_command`].
#[doc(hidden)]
pub fn is_line(call: &Invocation) -> bool {
    call.handler == LINE_HANDLER
}

/// Runs the handler installed for `call.handler`, or hands the invocation
/// back when it has none (commands registered by line).
#[doc(hidden)]
pub fn dispatch(call: Invocation) -> Result<Outcome, Box<Invocation>> {
    let handler = REGISTRY.with(|r| r.borrow().handlers.get(call.handler as usize).cloned());
    match handler {
        Some(handler) => Ok(handler(Ctx::new(call))),
        None => Err(Box::new(call)),
    }
}

/// The installed suggester's answers for `req`, filtered by what was
/// typed, or the request back when no suggester claims its node.
#[doc(hidden)]
pub fn suggest(req: SuggestRequest) -> Result<Vec<String>, Box<SuggestRequest>> {
    let mut words = req.line.split_whitespace();
    let first = words.next().unwrap_or("");
    let second = words.next().unwrap_or("");
    let suggester = REGISTRY.with(|r| {
        let r = r.borrow();
        let mut candidates = r.suggesters.iter().filter(|s| s.node == req.node);
        let fallback = candidates.clone().next();
        candidates
            .find(|s| match s.tuff {
                true => first == "tuff" && second == s.root,
                false => first == s.root,
            })
            .or(fallback)
            .map(|s| s.suggester.clone())
    });
    let Some(suggester) = suggester else {
        return Err(Box::new(req));
    };
    let request = Suggest {
        node: req.node,
        partial: req.partial,
        sender: req.sender,
        args: Args(req.args),
        line: req.line,
    };
    let typed = request.partial.to_lowercase();
    let mut found = suggester(&request);
    found.retain(|s| s.to_lowercase().starts_with(&typed));
    Ok(found)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;
    use crate::types::Uuid;

    fn steve() -> Player {
        Player {
            uuid: Uuid::from_u128(7),
            name: "Steve".into(),
        }
    }

    fn call(handler: u32, args: Vec<(&str, ArgValue)>) -> Invocation {
        Invocation {
            handler,
            sender: Some(steve()),
            args: args
                .into_iter()
                .map(|(name, value)| Arg {
                    name: name.into(),
                    value,
                })
                .collect(),
            line: "test".into(),
        }
    }

    fn run(call: Invocation) -> Outcome {
        dispatch(call).unwrap_or_else(|call| panic!("no handler {}", call.handler))
    }

    crate::args! {
        struct Greet {
            target: Player,
            times: Option<i32> = parser::integer_in(1..=3),
        }
    }

    crate::args! {
        struct Optional {
            first: Option<Word>,
            second: Option<f64>,
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn trees_flatten_in_preorder_with_handlers_numbered_in_order() {
        let tree = command("wgreet")
            .executes(|_| Ok(10))
            .then(
                arg::<Player>("target")
                    .executes(|_| Ok(11))
                    .then(argument("times", parser::integer_in(1..=3)).executes(|_| Ok(12))),
            )
            .then(literal("sum").then(arg::<i32>("a").then(
                arg::<i32>("b").executes(|ctx| Ok(ctx.get::<i32>("a")? + ctx.get::<i32>("b")?)),
            )))
            .then(
                literal("volume").permission("wgreet.volume").then(
                    argument("level", parser::double_in(0.0..=1.0))
                        .suggests(|_| Vec::new())
                        .executes(|_| Ok(14)),
                ),
            )
            .install(false)
            .unwrap();
        let names: Vec<&str> = tree.nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "wgreet", "target", "times", "sum", "a", "b", "volume", "level"
            ]
        );
        assert_eq!(tree.nodes[0].children, [1, 3, 6]);
        assert_eq!(tree.nodes[1].children, [2]);
        assert_eq!(tree.nodes[4].children, [5]);
        let executes: Vec<Option<u32>> = tree.nodes.iter().map(|n| n.executes).collect();
        assert_eq!(
            executes,
            [
                Some(0),
                Some(1),
                Some(2),
                None,
                None,
                Some(3),
                None,
                Some(4)
            ]
        );
        assert_eq!(
            tree.nodes[2].kind,
            NodeKind::Argument(Parser::Integer(IntRange {
                min: Some(1),
                max: Some(3)
            }))
        );
        assert_eq!(tree.nodes[6].permission.as_deref(), Some("wgreet.volume"));
        assert!(tree.nodes[7].suggest && !tree.nodes[6].suggest);

        assert_eq!(run(call(0, vec![])), Ok(10));
        assert_eq!(run(call(2, vec![])), Ok(12));
        let sum = vec![("a", ArgValue::Integer(-40)), ("b", ArgValue::Integer(2))];
        assert_eq!(run(call(3, sum)), Ok(-38));
        assert_eq!(
            run(call(3, vec![("a", ArgValue::Integer(1))])),
            Err("missing argument b".into())
        );
        let wrong = vec![("a", ArgValue::Integer(1)), ("b", ArgValue::Double(2.0))];
        assert_eq!(
            run(call(3, wrong)),
            Err("argument b has the wrong type".into())
        );
        assert!(dispatch(call(5, vec![])).is_err());
        assert!(dispatch(call(LINE_HANDLER, vec![])).is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn second_trees_continue_the_handler_numbering() {
        let first = command("a").executes(|_| Ok(1)).install(false).unwrap();
        let second = command("b").executes(|_| Ok(2)).install(false).unwrap();
        assert_eq!(first.nodes[0].executes, Some(0));
        assert_eq!(second.nodes[0].executes, Some(1));
        assert_eq!(run(call(1, vec![])), Ok(2));
    }

    #[tuff_core::watchdog]
    #[test]
    fn args_structs_become_argument_chains() {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let log = seen.clone();
        let tree = command("greet")
            .with(move |ctx, Greet { target, times }| {
                log.borrow_mut().push((target.name.clone(), times));
                Ok(times.unwrap_or(1) + i32::from(ctx.sender.is_some()))
            })
            .install(false)
            .unwrap();
        assert_eq!(tree.nodes.len(), 3);
        assert_eq!(tree.nodes[0].executes, None);
        assert_eq!(tree.nodes[1].kind, NodeKind::Argument(Parser::Player));
        assert_eq!(
            tree.nodes[2].kind,
            NodeKind::Argument(parser::integer_in(1..=3))
        );
        let (one, two) = (tree.nodes[1].executes, tree.nodes[2].executes);
        assert!(one.is_some() && two.is_some() && one != two);

        let target = ("target", ArgValue::Player(steve()));
        assert_eq!(run(call(one.unwrap(), vec![target.clone()])), Ok(2));
        let times = ("times", ArgValue::Integer(3));
        assert_eq!(run(call(two.unwrap(), vec![target, times])), Ok(4));
        assert_eq!(
            run(call(one.unwrap(), vec![])),
            Err("missing argument target".into())
        );
        assert_eq!(
            *seen.borrow(),
            [("Steve".to_owned(), None), ("Steve".to_owned(), Some(3))]
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn all_optional_args_also_run_at_the_parent() {
        let tree = literal("opt")
            .with(|_, Optional { first, second }| {
                Ok(first.map_or(0, |w| w.len() as i32) + second.map_or(0, |d| d as i32))
            })
            .install(false)
            .unwrap();
        assert!(tree.nodes.iter().all(|n| n.executes.is_some()));
        assert_eq!(tree.nodes[1].kind, NodeKind::Argument(parser::word()));
        assert_eq!(run(call(tree.nodes[0].executes.unwrap(), vec![])), Ok(0));
        let args = vec![
            ("first", ArgValue::Text("abc".into())),
            ("second", ArgValue::Double(2.5)),
        ];
        assert_eq!(run(call(tree.nodes[2].executes.unwrap(), args)), Ok(5));
    }

    #[tuff_core::watchdog]
    #[test]
    fn redirects_resolve_labels() {
        let tree = command("r")
            .then(literal("loop").label("top").executes(|_| Ok(1)))
            .then(literal("again").redirect("top"))
            .install(false)
            .unwrap();
        assert_eq!(tree.nodes[2].redirect, Some(1));
        let missing = command("r").then(literal("x").redirect("nowhere"));
        assert!(missing.install(false).unwrap_err().contains("nowhere"));
    }

    fn request(node: &str, partial: &str, line: &str) -> SuggestRequest {
        SuggestRequest {
            node: node.into(),
            partial: partial.into(),
            sender: None,
            args: Vec::new(),
            line: line.into(),
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn suggestions_filter_by_prefix_and_pick_the_tree_by_line() {
        let colours = |_: &Suggest| vec!["Red".to_owned(), "green".to_owned(), "rose".to_owned()];
        command("paint")
            .then(arg::<Word>("colour").suggests(colours).executes(|_| Ok(1)))
            .install(false)
            .unwrap();
        command("dye")
            .then(
                arg::<Word>("colour")
                    .suggests(|s| vec![format!("{}-dye", s.partial)])
                    .executes(|_| Ok(1)),
            )
            .install(true)
            .unwrap();
        assert_eq!(
            suggest(request("colour", "r", "paint r")).unwrap(),
            ["Red", "rose"]
        );
        assert_eq!(
            suggest(request("colour", "b", "tuff dye b")).unwrap(),
            ["b-dye"]
        );
        assert!(suggest(request("shade", "", "paint ")).is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn text_types_read_their_variants() {
        let args = Args::new(vec![
            Arg {
                name: "s".into(),
                value: ArgValue::BlockState("minecraft:stone".into()),
            },
            Arg {
                name: "p".into(),
                value: ArgValue::Player(steve()),
            },
            Arg {
                name: "i".into(),
                value: ArgValue::Integer(4),
            },
        ]);
        assert_eq!(&*args.get::<BlockState>("s").unwrap(), "minecraft:stone");
        assert_eq!(args.get::<String>("s").unwrap(), "minecraft:stone");
        assert!(args.get::<Item>("s").is_err());
        assert_eq!(args.get::<Vec<Player>>("p").unwrap(), [steve()]);
        assert_eq!(args.get::<f64>("i").unwrap(), 4.0);
        assert_eq!(args.get::<Option<bool>>("missing").unwrap(), None);
        assert!(args.get::<Option<bool>>("i").is_err());
    }
}
