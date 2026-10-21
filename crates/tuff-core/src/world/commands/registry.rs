//! The commands this server runs: each top-level literal with its tree
//! node and executor. Features register entries here; the parser and the
//! Commands packet follow whatever is registered.

use std::collections::HashMap;
use std::sync::Arc;

use crate::command::{CommandNode, RootCommand, Roots};

use super::{Executor, Source};
use crate::command::CommandError;
use crate::world::Shared;
use tuff_item::text::TextComponent;

#[derive(Clone, Default)]
pub struct Registry {
    roots: Roots,
    executors: HashMap<&'static str, Arc<dyn Executor>>,
    /// Alias name to the command its vanilla node redirects to.
    aliases: HashMap<&'static str, &'static str>,
    tuff_subcommands: Vec<TuffSubcommand>,
    line_claims: Vec<Arc<dyn LineClaim>>,
    client_roots: ClientRoots,
    text_resolver: Option<Arc<dyn TextResolver>>,
}

pub type TextFuture<'a> = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<TextComponent, CommandError>> + Send + 'a>,
>;

/// Expands the selector, score and nbt contents of a text component
/// against the command source.
pub trait TextResolver: Send + Sync {
    fn resolve<'a>(
        &'a self,
        ctx: &'a mut super::Context<'_>,
        component: &'a TextComponent,
    ) -> TextFuture<'a>;
}

/// What a claimed command line produced.
#[derive(Debug)]
pub struct LineOutcome {
    pub replies: Vec<String>,
    pub result: Result<i32, String>,
}

pub type LineFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Option<LineOutcome>> + Send + 'a>>;

/// Runs command lines the dispatcher's tree does not know, such as runtime
/// feature commands; `None` leaves the line to the dispatcher.
pub trait LineClaim: Send + Sync {
    fn run<'a>(
        &'a self,
        source: &'a Source,
        shared: &'a Arc<Shared>,
        line: &'a str,
    ) -> LineFuture<'a>;

    /// Completions for the last word of `text` (no slash) when the claim
    /// owns the command; `None` leaves it to the dispatcher.
    fn suggest<'a>(
        &'a self,
        _source: &'a Source,
        _shared: &'a Arc<Shared>,
        _text: &'a str,
    ) -> SuggestFuture<'a> {
        Box::pin(std::future::ready(None))
    }
}

pub type SuggestFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Option<Vec<String>>> + Send + 'a>>;

/// What runtime plugins add to the client's command tree after startup:
/// root literals and `/tuff` subcommand literals, each taking a greedy
/// string. Sessions watch it to resend the tree; the dispatcher's own tree
/// never sees them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClientExtras {
    pub roots: Vec<&'static str>,
    pub tuff: Vec<&'static str>,
    /// Whole command trees features registered; they replace a greedy root
    /// of the same name.
    pub trees: Vec<ClientTree>,
}

/// A feature's command as the client's tree shows it, flat with child
/// indices. Node 0 is the root literal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientTree {
    pub nodes: Vec<ClientNode>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientNode {
    pub name: &'static str,
    /// The parser's name and JSON properties; none for a literal.
    pub parser: Option<(&'static str, Option<String>)>,
    pub children: Vec<u16>,
    pub executable: bool,
    pub redirect: Option<u16>,
    /// A permission node the player must hold to see the node.
    pub permission: Option<String>,
    /// Whether the feature answers tab-completion for this argument.
    pub suggest: bool,
}

impl ClientTree {
    pub fn name(&self) -> &'static str {
        self.nodes.first().map_or("", |node| node.name)
    }
}

/// Interns a node name for the life of the process.
pub fn intern_name(name: &str) -> &'static str {
    intern([name.to_owned()])[0]
}

#[derive(Clone)]
pub struct ClientRoots(Arc<tokio::sync::watch::Sender<Arc<ClientExtras>>>);

impl Default for ClientRoots {
    fn default() -> Self {
        Self(Arc::new(tokio::sync::watch::channel(Arc::default()).0))
    }
}

fn intern(names: impl IntoIterator<Item = String>) -> Vec<&'static str> {
    static INTERNED: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());
    let mut interned = INTERNED.lock().unwrap_or_else(|e| e.into_inner());
    let mut names: Vec<&'static str> = names
        .into_iter()
        .map(|name| match interned.iter().find(|known| **known == name) {
            Some(known) => *known,
            None => {
                let leaked: &'static str = Box::leak(name.into_boxed_str());
                interned.push(leaked);
                leaked
            }
        })
        .collect();
    names.sort_unstable();
    names
}

impl ClientRoots {
    /// Replaces the set. Names are interned for the life of the process,
    /// bounded by the distinct names features ever claim.
    pub fn set(
        &self,
        roots: impl IntoIterator<Item = String>,
        tuff: impl IntoIterator<Item = String>,
        trees: Vec<ClientTree>,
    ) {
        let next = ClientExtras {
            roots: intern(roots),
            tuff: intern(tuff),
            trees,
        };
        self.0.send_if_modified(|current| {
            let changed = **current != next;
            if changed {
                *current = Arc::new(next);
            }
            changed
        });
    }

    pub fn get(&self) -> Arc<ClientExtras> {
        self.0.borrow().clone()
    }

    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<Arc<ClientExtras>> {
        self.0.subscribe()
    }
}

/// A `/tuff` subcommand a feature adds. Its node carries its own permission
/// requirement; the feature's executor runs the whole `/tuff <name> ...`
/// line and checks its own permission nodes.
#[derive(Clone)]
pub struct TuffSubcommand {
    pub node: &'static CommandNode,
    /// `(argument name, suggestion provider)` pairs for its arguments.
    pub providers: &'static [(&'static str, &'static str)],
    pub executor: Arc<dyn Executor>,
}

impl Registry {
    pub fn new() -> Self {
        Self {
            roots: Roots::default(),
            executors: HashMap::new(),
            aliases: HashMap::new(),
            tuff_subcommands: Vec::new(),
            line_claims: Vec::new(),
            client_roots: ClientRoots::default(),
            text_resolver: None,
        }
    }

    pub fn set_text_resolver(&mut self, resolver: Arc<dyn TextResolver>) {
        self.text_resolver = Some(resolver);
    }

    pub fn text_resolver(&self) -> Option<&Arc<dyn TextResolver>> {
        self.text_resolver.as_ref()
    }

    pub fn client_roots(&self) -> &ClientRoots {
        &self.client_roots
    }

    /// Adds a `/tuff` subcommand; the feature that owns `/tuff` merges these
    /// into its tree once every feature's content is registered.
    pub fn add_tuff_subcommand(&mut self, subcommand: TuffSubcommand) {
        self.tuff_subcommands
            .retain(|known| known.node.name != subcommand.node.name);
        self.tuff_subcommands.push(subcommand);
    }

    pub fn add_line_claim(&mut self, claim: Arc<dyn LineClaim>) {
        self.line_claims.push(claim);
    }

    pub fn line_claims(&self) -> &[Arc<dyn LineClaim>] {
        &self.line_claims
    }

    pub fn tuff_subcommands(&self) -> &[TuffSubcommand] {
        &self.tuff_subcommands
    }

    /// Registers a command: its tree node (for parsing and the client's
    /// tree) and what runs it.
    pub fn register(&mut self, command: RootCommand, executor: impl Executor + 'static) {
        let mut commands: Vec<RootCommand> = self.roots.commands().to_vec();
        commands.retain(|c| c.name != command.name);
        commands.push(command);
        self.roots = self.roots.with_commands(commands);
        self.executors.insert(command.name, Arc::new(executor));
    }

    /// Registers a command by its name in the vanilla command tree.
    pub fn vanilla(
        &mut self,
        name: &'static str,
        providers: &'static [(&'static str, &'static str)],
        executor: impl Executor + 'static,
    ) {
        self.register(RootCommand::vanilla(name, providers), executor);
    }

    /// Registers an alias whose vanilla node redirects to an already
    /// registered command (`tp` for `teleport`).
    pub fn register_alias(&mut self, name: &'static str) {
        let command = RootCommand::vanilla(name, &[]);
        let target = match command.node.redirect {
            Some([target]) => *target,
            _ => panic!("{name} does not redirect to a top-level command"),
        };
        assert!(
            self.executors.contains_key(target),
            "{name} aliases unregistered {target}"
        );
        let mut commands: Vec<RootCommand> = self.roots.commands().to_vec();
        commands.retain(|c| c.name != name);
        commands.push(command);
        self.roots = self.roots.with_commands(commands);
        self.aliases.insert(name, target);
    }

    /// Lets `resource` arguments of `registry` (`minecraft:entity_type`)
    /// name `id`, which only this server knows.
    pub fn add_resource(&mut self, registry: &str, id: impl Into<String>) {
        self.roots.add_resource(registry, id);
    }

    /// Lets item arguments (`/give`, `/clear`, ...) name the custom item
    /// `id`, which parses as `input`.
    pub fn add_custom_item(
        &mut self,
        id: impl Into<String>,
        input: crate::command::item::ItemInput,
    ) {
        self.roots.add_custom_item(id, input);
    }

    pub fn roots(&self) -> &Roots {
        &self.roots
    }

    /// What runs the command named `name`, following aliases.
    pub fn executor(&self, name: &str) -> Option<&dyn Executor> {
        let name = self.aliases.get(name).copied().unwrap_or(name);
        self.executors.get(name).map(|e| &**e)
    }
}

/// Completions a line claim offers for the word being typed at the end of
/// `text` (with or without the leading `/`), ready to send.
pub async fn claimed_suggestions(
    shared: &Arc<Shared>,
    source: &Source,
    text: &str,
) -> Option<crate::command::Suggestions> {
    let line = text.strip_prefix('/').unwrap_or(text);
    let offset = text.len() - line.len();
    let space = line.rfind(' ')?;
    for claim in shared.commands.line_claims() {
        let Some(mut matches) = claim.suggest(source, shared, line).await else {
            continue;
        };
        let partial = line[space + 1..].to_lowercase();
        matches.retain(|m| m.to_lowercase().starts_with(&partial));
        matches.sort_by_key(|m| m.to_lowercase());
        matches.dedup();
        matches.truncate(100);
        let start = offset + space + 1;
        return Some(crate::command::Suggestions {
            start: text[..start].encode_utf16().count(),
            length: text[start..].encode_utf16().count(),
            matches,
        });
    }
    None
}
