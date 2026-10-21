//! Permission nodes: namespaced rights (`tuff.command.config`,
//! `myfeature.home.set`) that core and features register with a
//! description, a default and whether operators bypass them. Settings are
//! per player and per named role, owned by one actor (see [`store`]) that
//! persists them in the server file area; checks read an immutable
//! [`View`], so they never wait.
//!
//! A player's node resolves in this order, the first answer winning:
//!
//! 1. the player's own most specific setting (`a.b.c`, then `a.b.*`,
//!    `a.*`, `*`);
//! 2. their roles, highest priority first (ties by name), each answering
//!    with its most specific setting; every player is in the
//!    [`DEFAULT_ROLE`] when it exists;
//! 3. operator bypass, when the node allows it and the player is an
//!    operator at or above its level;
//! 4. the node's default.
//!
//! The console holds every node. Vanilla commands are gated by
//! `minecraft.command.<name>` nodes resolved the same way, with the
//! command's own level standing in for steps 3 and 4.

pub mod store;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use crate::command::Permits;
use tuff_item::load::{LoadError, LoadErrorKind};
use tuff_item::registry::Registry;
use tuff_storage::admin::PermissionLevel;
use tuff_web_ui::auth::{
    WEB_ACCESS, WEB_BACKUPS_MANAGE, WEB_BACKUPS_READ, WEB_BACKUPS_RESTORE, WEB_CONFIG_READ,
    WEB_CONFIG_WRITE, WEB_CONSOLE_RUN, WEB_DATAPACKS_MANAGE, WEB_DATAPACKS_READ, WEB_LOGOUT_ALL,
    WEB_LOGS_READ, WEB_METRICS_READ, WEB_PANEL_ADMIN, WEB_PERMISSIONS, WEB_PLAYERS_MODERATE,
    WEB_PLAYERS_READ, WEB_RESOURCEPACKS_MANAGE, WEB_RESOURCEPACKS_READ, WEB_SQL, WEB_SQL_WRITE,
    WEB_TIMINGS,
};

pub use store::{Affected, Applied, Change, Permissions, SetError};

/// The longest node id or pattern accepted.
pub const MAX_NODE_LEN: usize = 128;
/// The longest role name accepted.
pub const MAX_ROLE_LEN: usize = 32;
/// The role every player is in, when it exists.
pub const DEFAULT_ROLE: &str = "default";
/// The node prefix gating vanilla commands: `minecraft.command.<name>`.
pub const COMMAND_PREFIX: &str = "minecraft.command.";

/// Receive the server's tick time samples (the client's tick chart).
pub const DEBUG_TICK_TIME: &str = "tuff.debug.tick_time";
/// Receive the world debug feeds (POIs, village sections, neighbor
/// updates, game events) the client asks for.
pub const DEBUG_SUBSCRIPTIONS: &str = "tuff.debug.subscriptions";

/// A registered right.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub id: String,
    pub description: String,
    /// Whether players hold it without any grant.
    pub default: bool,
    /// Whether operators at `op_level` or above hold it without a grant.
    pub op_bypass: bool,
    pub op_level: PermissionLevel,
}

impl Node {
    /// A node nobody holds by default and owner-level operators bypass.
    pub fn new(id: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            description: description.into(),
            default: false,
            op_bypass: true,
            op_level: PermissionLevel::Owners,
        }
    }

    /// Whether every player holds it unless denied.
    pub fn by_default(mut self, held: bool) -> Self {
        self.default = held;
        self
    }

    /// Whether operators hold it without a grant; `false` makes even
    /// owners need an explicit grant.
    pub fn op_bypass(mut self, bypass: bool) -> Self {
        self.op_bypass = bypass;
        self
    }

    /// The lowest operator level that bypasses it.
    pub fn op_level(mut self, level: PermissionLevel) -> Self {
        self.op_level = level.max(PermissionLevel::Moderators);
        self
    }

    /// What a node nobody registered resolves as: owners only.
    fn unregistered(id: &str) -> Self {
        Self::new(id, "")
    }

    /// Whether a subject at `level` without an explicit setting holds it.
    pub fn fallback(&self, level: PermissionLevel) -> bool {
        self.default || (self.op_bypass && level > PermissionLevel::All && level >= self.op_level)
    }
}

/// The nodes core itself checks outside any feature.
fn core_nodes() -> Vec<Node> {
    vec![
        Node::new(
            DEBUG_TICK_TIME,
            "Receive the server's tick time samples for the client's tick chart, as operators do",
        )
        .op_level(PermissionLevel::Moderators),
        Node::new(
            DEBUG_SUBSCRIPTIONS,
            "Receive the world debug feeds the client asks for, as operators do",
        )
        .op_level(PermissionLevel::Moderators),
        Node::new(
            WEB_ACCESS,
            "Use the web admin panel: /tuff web login and logout, and keep a session \
             (operators at tuff.web.minOpLevel or above have it unless denied)",
        ),
        Node::new(WEB_LOGOUT_ALL, "End every web admin panel session"),
        Node::new(WEB_METRICS_READ, "Read the web panel dashboard and metrics"),
        Node::new(WEB_LOGS_READ, "Read the server log in the web panel")
            .op_level(tuff_web_ui::auth::node_level(WEB_LOGS_READ)),
        Node::new(
            WEB_CONSOLE_RUN,
            "Run commands from the web panel console, at the user's own level",
        )
        .op_level(tuff_web_ui::auth::node_level(WEB_CONSOLE_RUN)),
        Node::new(
            WEB_TIMINGS,
            "Record, list and export timings reports from the web panel",
        ),
        Node::new(
            WEB_PERMISSIONS,
            "Manage permission settings and roles from the web panel",
        ),
        Node::new(
            WEB_PANEL_ADMIN,
            "Administer the web panel: sessions and the audit log",
        ),
        Node::new(
            WEB_BACKUPS_READ,
            "List and download world backups and follow backup jobs in the web panel",
        ),
        Node::new(
            WEB_BACKUPS_MANAGE,
            "Create, cancel, upload and delete world backups in the web panel",
        ),
        Node::new(
            WEB_BACKUPS_RESTORE,
            "Stage a world backup to replace the world at the next start, and shut the server down for it",
        ),
        Node::new(
            WEB_DATAPACKS_READ,
            "List and download the world's data packs in the web panel",
        ),
        Node::new(
            WEB_DATAPACKS_MANAGE,
            "Upload, enable, disable, remove and reload data packs in the web panel",
        ),
        Node::new(
            WEB_RESOURCEPACKS_READ,
            "List and download hosted resource packs and see players' pack status in the web panel",
        ),
        Node::new(
            WEB_RESOURCEPACKS_MANAGE,
            "Upload, configure, push, pop and delete resource packs in the web panel",
        ),
        Node::new(
            WEB_PLAYERS_READ,
            "See online players, known players and the operator, whitelist and ban lists in the web panel",
        )
        .op_level(tuff_web_ui::auth::node_level(WEB_PLAYERS_READ)),
        Node::new(
            WEB_PLAYERS_MODERATE,
            "Kick, ban, pardon, op, whitelist, message and move players from the web panel \
             (each action still needs its command's level)",
        )
        .op_level(tuff_web_ui::auth::node_level(WEB_PLAYERS_MODERATE)),
        Node::new(
            WEB_CONFIG_READ,
            "See server settings, server.properties and game rules in the web panel (secret values stay hidden)",
        ),
        Node::new(
            WEB_CONFIG_WRITE,
            "Change server settings, server.properties and game rules from the web panel",
        ),
        Node::new(
            WEB_SQL,
            "Run read-only SQL queries against a SQLite world from the web panel",
        ),
        Node::new(
            WEB_SQL_WRITE,
            "Change a SQLite world with SQL from the web panel (also needs tuff.web.sql.allowWrite)",
        ),
    ]
}

/// Whether `id` names a node: at least two dot-separated segments of
/// `a-z`, `0-9`, `_` and `-`.
pub fn valid_id(id: &str) -> bool {
    id.len() <= MAX_NODE_LEN && id.split('.').count() >= 2 && id.split('.').all(valid_segment)
}

fn valid_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// Whether `pattern` may be granted: a node id, `*`, or a prefix of
/// segments ending in `.*`.
pub fn valid_pattern(pattern: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    match pattern.strip_suffix(".*") {
        Some(prefix) => pattern.len() <= MAX_NODE_LEN && prefix.split('.').all(valid_segment),
        None => valid_id(pattern),
    }
}

/// Whether `name` may name a role: one segment of `a-z`, `0-9`, `_` and
/// `-`.
pub fn valid_role(name: &str) -> bool {
    name.len() <= MAX_ROLE_LEN && valid_segment(name)
}

/// The node gating the vanilla command `name`.
pub fn command_node(name: &str) -> String {
    format!("{COMMAND_PREFIX}{name}")
}

/// Where features register their nodes; core's own are in from the start.
pub struct NodeRegistry {
    inner: Registry<Node>,
}

impl NodeRegistry {
    pub fn new() -> Self {
        let mut inner = Registry::new("permission_nodes");
        for node in core_nodes() {
            inner
                .register("core", node.id.clone(), node)
                .expect("core nodes are distinct");
        }
        Self { inner }
    }

    /// Adds `node`; a malformed id or one already registered fails the
    /// load naming both features.
    pub fn register(&mut self, feature: &'static str, node: Node) -> Result<(), LoadError> {
        if !valid_id(&node.id) {
            return Err(LoadError {
                feature,
                kind: LoadErrorKind::Other(format!(
                    "permission node '{}' must be namespaced segments of a-z, 0-9, _ and -",
                    node.id
                )),
            });
        }
        self.inner.register(feature, node.id.clone(), node)
    }

    /// Adds a `minecraft.command.<name>` node for every vanilla command
    /// in `roots` no feature registered itself: held by default for
    /// commands anyone may use, else by operators at the command's level.
    pub fn register_commands(&mut self, roots: &crate::command::Roots) {
        let mut gates: Vec<_> = roots
            .gates()
            .filter(|(name, gate)| *name == gate.command)
            .collect();
        gates.sort_by_key(|(name, _)| *name);
        for (name, gate) in gates {
            let id = command_node(name);
            if self.inner.registered_by(&id).is_some() {
                continue;
            }
            let node = Node::new(id.clone(), format!("Use /{name}"))
                .by_default(gate.level == PermissionLevel::All)
                .op_level(gate.level);
            let _ = self.inner.register("core", id, node);
        }
    }

    pub fn registered_by(&self, id: &str) -> Option<&'static str> {
        self.inner.registered_by(id)
    }

    pub fn freeze(&mut self) {
        self.inner.freeze();
    }

    /// The registered nodes as checks read them.
    pub fn nodes(&self) -> Arc<Nodes> {
        Arc::new(Nodes {
            by_id: self
                .inner
                .iter()
                .map(|(id, node)| (id.to_owned(), (**node).clone()))
                .collect(),
        })
    }
}

impl Default for NodeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Every registered node, by id.
#[derive(Clone, Debug)]
pub struct Nodes {
    by_id: BTreeMap<String, Node>,
}

impl Default for Nodes {
    /// Core's nodes only.
    fn default() -> Self {
        Self {
            by_id: core_nodes()
                .into_iter()
                .map(|n| (n.id.clone(), n))
                .collect(),
        }
    }
}

impl Nodes {
    pub fn get(&self, id: &str) -> Option<&Node> {
        self.by_id.get(id)
    }

    pub fn contains(&self, id: &str) -> bool {
        self.by_id.contains_key(id)
    }

    /// Every node in id order.
    pub fn iter(&self) -> impl Iterator<Item = &Node> {
        self.by_id.values()
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    fn fallback(&self, id: &str, level: PermissionLevel) -> bool {
        match self.by_id.get(id) {
            Some(node) => node.fallback(level),
            None => Node::unregistered(id).fallback(level),
        }
    }
}

/// Who a check is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Subject {
    /// The console: every node.
    Server,
    /// A player (or a web panel user) with their operator level.
    Player { uuid: u128, level: PermissionLevel },
    /// A source with a level but no identity (command blocks, functions).
    Anonymous(PermissionLevel),
}

impl Subject {
    pub fn level(self) -> PermissionLevel {
        match self {
            Self::Server => PermissionLevel::Owners,
            Self::Player { level, .. } | Self::Anonymous(level) => level,
        }
    }
}

/// One player's explicit settings and role memberships.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlayerGrants {
    /// The name they had when last changed, for listings.
    pub name: String,
    /// Node or pattern to held (`true`) or denied (`false`).
    pub nodes: BTreeMap<String, bool>,
    pub roles: BTreeSet<String>,
}

impl PlayerGrants {
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty() && self.roles.is_empty()
    }
}

/// A named set of settings players can be members of.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Role {
    /// Higher priority roles answer first.
    pub priority: i32,
    pub nodes: BTreeMap<String, bool>,
    pub attributes: Attributes,
}

/// The longest attribute value accepted, in bytes.
pub const MAX_ATTRIBUTE_LEN: usize = 512;

/// How a role decorates its members' names; text values are MiniMessage.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Attributes {
    /// Shown before a member's name in chat and the player list.
    pub prefix: Option<String>,
    /// Shown after a member's name in chat and the player list.
    pub suffix: Option<String>,
    /// A named colour or `#RRGGBB` for a member's name.
    pub name_color: Option<String>,
    /// How the role itself is shown in listings.
    pub display_name: Option<String>,
}

/// One of a role's [`Attributes`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Attribute {
    Prefix,
    Suffix,
    NameColor,
    DisplayName,
}

impl Attribute {
    pub const ALL: [Self; 4] = [
        Self::Prefix,
        Self::Suffix,
        Self::NameColor,
        Self::DisplayName,
    ];

    /// The attribute's key in commands, the API and the store.
    pub fn key(self) -> &'static str {
        match self {
            Self::Prefix => "prefix",
            Self::Suffix => "suffix",
            Self::NameColor => "name_color",
            Self::DisplayName => "display_name",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.key() == key)
    }

    /// Whether `value` is acceptable for this attribute.
    pub fn valid(self, value: &str) -> bool {
        value.len() <= MAX_ATTRIBUTE_LEN
            && match self {
                Self::NameColor => crate::minimessage::color(value).is_some(),
                _ => true,
            }
    }
}

impl Attributes {
    pub fn get(&self, attribute: Attribute) -> Option<&str> {
        self.slot(attribute).as_deref()
    }

    pub fn slot_mut(&mut self, attribute: Attribute) -> &mut Option<String> {
        match attribute {
            Attribute::Prefix => &mut self.prefix,
            Attribute::Suffix => &mut self.suffix,
            Attribute::NameColor => &mut self.name_color,
            Attribute::DisplayName => &mut self.display_name,
        }
    }

    fn slot(&self, attribute: Attribute) -> &Option<String> {
        match attribute {
            Attribute::Prefix => &self.prefix,
            Attribute::Suffix => &self.suffix,
            Attribute::NameColor => &self.name_color,
            Attribute::DisplayName => &self.display_name,
        }
    }
}

/// Every player's and role's settings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Grants {
    pub players: HashMap<u128, PlayerGrants>,
    pub roles: BTreeMap<String, Role>,
}

/// An explicit setting that decides a node: the player's own, or one of
/// their roles'.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Setting<'a> {
    pub role: Option<&'a str>,
    pub pattern: &'a str,
    pub held: bool,
}

/// The most specific of `nodes` covering `node`.
fn most_specific<'a>(nodes: &'a BTreeMap<String, bool>, node: &str) -> Option<(&'a str, bool)> {
    if let Some((pattern, held)) = nodes.get_key_value(node) {
        return Some((pattern, *held));
    }
    let mut prefix = node;
    while let Some((head, _)) = prefix.rsplit_once('.') {
        prefix = head;
        if let Some((pattern, held)) = nodes.get_key_value(&format!("{prefix}.*")) {
            return Some((pattern, *held));
        }
    }
    nodes
        .get_key_value("*")
        .map(|(pattern, held)| (pattern.as_str(), *held))
}

/// Why a subject holds a node or not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Server,
    Explicit {
        pattern: String,
        held: bool,
    },
    Role {
        role: String,
        pattern: String,
        held: bool,
    },
    OpBypass(PermissionLevel),
    Default(bool),
}

impl Decision {
    pub fn held(&self) -> bool {
        match self {
            Self::Server | Self::OpBypass(_) => true,
            Self::Explicit { held, .. } | Self::Role { held, .. } => *held,
            Self::Default(held) => *held,
        }
    }
}

/// A snapshot of the nodes and grants, cheap to clone.
#[derive(Clone, Debug, Default)]
pub struct View {
    nodes: Arc<Nodes>,
    grants: Arc<Grants>,
}

impl View {
    pub fn new(nodes: Arc<Nodes>, grants: Arc<Grants>) -> Self {
        Self { nodes, grants }
    }

    pub fn nodes(&self) -> &Nodes {
        &self.nodes
    }

    /// The same nodes with `grants`.
    pub fn with_grants(&self, grants: Grants) -> View {
        Self::new(self.nodes.clone(), Arc::new(grants))
    }

    pub fn grants(&self) -> &Grants {
        &self.grants
    }

    pub fn player(&self, uuid: u128) -> Option<&PlayerGrants> {
        self.grants.players.get(&uuid)
    }

    /// The most specific explicit setting `uuid` itself has for `node`,
    /// and the pattern it came from.
    pub fn explicit(&self, uuid: u128, node: &str) -> Option<(&str, bool)> {
        most_specific(&self.grants.players.get(&uuid)?.nodes, node)
    }

    /// The roles `uuid` takes settings from, in the order they answer:
    /// highest priority first, ties by name, the default role included.
    pub fn roles_of(&self, uuid: u128) -> Vec<(&str, &Role)> {
        let member = self.grants.players.get(&uuid).map(|p| &p.roles);
        let mut roles: Vec<(&str, &Role)> = self
            .grants
            .roles
            .iter()
            .filter(|(name, _)| {
                name.as_str() == DEFAULT_ROLE || member.is_some_and(|m| m.contains(*name))
            })
            .map(|(name, role)| (name.as_str(), role))
            .collect();
        roles.sort_by(|a, b| b.1.priority.cmp(&a.1.priority).then(a.0.cmp(b.0)));
        roles
    }

    /// The explicit setting that decides `node` for `uuid`: their own,
    /// else the first of their roles with one.
    pub fn setting(&self, uuid: u128, node: &str) -> Option<Setting<'_>> {
        if let Some((pattern, held)) = self.explicit(uuid, node) {
            return Some(Setting {
                role: None,
                pattern,
                held,
            });
        }
        if self.grants.roles.is_empty() {
            return None;
        }
        self.roles_of(uuid).into_iter().find_map(|(name, role)| {
            most_specific(&role.nodes, node).map(|(pattern, held)| Setting {
                role: Some(name),
                pattern,
                held,
            })
        })
    }

    pub fn decide(&self, subject: Subject, node: &str) -> Decision {
        let level = match subject {
            Subject::Server => return Decision::Server,
            Subject::Player { uuid, level } => {
                if let Some(setting) = self.setting(uuid, node) {
                    let pattern = setting.pattern.to_owned();
                    let held = setting.held;
                    return match setting.role {
                        None => Decision::Explicit { pattern, held },
                        Some(role) => Decision::Role {
                            role: role.to_owned(),
                            pattern,
                            held,
                        },
                    };
                }
                level
            }
            Subject::Anonymous(level) => level,
        };
        let registered = self.nodes.get(node);
        let unregistered;
        let def = match registered {
            Some(def) => def,
            None => {
                unregistered = Node::unregistered(node);
                &unregistered
            }
        };
        if def.default {
            Decision::Default(true)
        } else if def.fallback(level) {
            Decision::OpBypass(def.op_level)
        } else {
            Decision::Default(false)
        }
    }

    /// The highest priority value of `attribute` among `uuid`'s roles.
    pub fn attribute_of(&self, uuid: u128, attribute: Attribute) -> Option<&str> {
        self.roles_of(uuid)
            .into_iter()
            .find_map(|(_, role)| role.attributes.get(attribute))
    }

    /// `name` decorated with the prefix, suffix and name colour of
    /// `uuid`'s roles, each from the highest priority role that sets it.
    pub fn decorate_name(
        &self,
        uuid: u128,
        name: tuff_item::text::TextComponent,
    ) -> tuff_item::text::TextComponent {
        use tuff_item::text::TextComponent;
        if self.grants.roles.is_empty() {
            return name;
        }
        let prefix = self.attribute_of(uuid, Attribute::Prefix);
        let suffix = self.attribute_of(uuid, Attribute::Suffix);
        let color = self
            .attribute_of(uuid, Attribute::NameColor)
            .and_then(crate::minimessage::color);
        if prefix.is_none() && suffix.is_none() && color.is_none() {
            return name;
        }
        let mut name = name;
        if let Some(color) = color
            && name.color.is_none()
        {
            name.color = Some(color.into_boxed_str());
        }
        let mut out = TextComponent::new("");
        if let Some(prefix) = prefix {
            out.extra.push(crate::minimessage::parse(prefix));
        }
        out.extra.push(name);
        if let Some(suffix) = suffix {
            out.extra.push(crate::minimessage::parse(suffix));
        }
        out
    }

    /// Whether `subject` holds `node`.
    pub fn allows(&self, subject: Subject, node: &str) -> bool {
        match subject {
            Subject::Server => true,
            Subject::Player { uuid, level } => match self.setting(uuid, node) {
                Some(setting) => setting.held,
                None => self.nodes.fallback(node, level),
            },
            Subject::Anonymous(level) => self.nodes.fallback(node, level),
        }
    }

    pub fn access(&self, subject: Subject) -> Access {
        Access {
            view: self.clone(),
            subject,
        }
    }
}

/// Has every online player whose rights `affected` names resend their
/// command tree.
pub async fn notify(
    coordinator: &tokio::sync::mpsc::Sender<crate::world::messages::CoordinatorMessage>,
    affected: &store::Affected,
) {
    if matches!(affected, store::Affected::Players(players) if players.is_empty()) {
        return;
    }
    let names = crate::world::messages::CoordinatorMessage::RoleNames(affected.clone());
    if coordinator.send(names).await.is_err() {
        return;
    }
    let (reply, players) = tokio::sync::oneshot::channel();
    let request = crate::world::messages::CoordinatorMessage::Players { reply };
    if coordinator.send(request).await.is_err() {
        return;
    }
    for online in players.await.unwrap_or_default() {
        if affected.includes(online.uuid) {
            online
                .handle
                .send(crate::world::messages::PlayerEvent::PermissionsChanged);
        }
    }
}

/// A subject's rights for the command tree and parser.
#[derive(Clone, Debug)]
pub struct Access {
    view: View,
    subject: Subject,
}

impl Access {
    pub fn subject(&self) -> Subject {
        self.subject
    }
}

impl Permits for Access {
    fn level(&self) -> PermissionLevel {
        self.subject.level()
    }

    fn has(&self, node: &str) -> bool {
        self.view.allows(self.subject, node)
    }

    fn command(&self, name: &str, required: PermissionLevel) -> bool {
        match self.subject {
            Subject::Server => true,
            Subject::Player { uuid, level } => self
                .view
                .setting(uuid, &command_node(name))
                .map_or(required <= level, |s| s.held),
            Subject::Anonymous(level) => required <= level,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALEX: u128 = 7;

    fn view(nodes: Vec<Node>, grants: &[(&str, bool)]) -> View {
        let mut registry = NodeRegistry::new();
        for node in nodes {
            registry.register("test", node).unwrap();
        }
        let mut player = PlayerGrants {
            name: "Alex".into(),
            ..Default::default()
        };
        for (pattern, held) in grants {
            player.nodes.insert((*pattern).to_owned(), *held);
        }
        let mut all = Grants::default();
        all.players.insert(ALEX, player);
        View::new(registry.nodes(), Arc::new(all))
    }

    fn alex(level: PermissionLevel) -> Subject {
        Subject::Player { uuid: ALEX, level }
    }

    #[tuff_core::watchdog]
    #[test]
    fn operators_get_every_bypassable_node() {
        let view = view(
            vec![
                Node::new("demo.home.set", "Set a home"),
                Node::new("demo.mod.kick", "Kick").op_level(PermissionLevel::Gamemasters),
            ],
            &[],
        );
        assert!(view.allows(alex(PermissionLevel::Owners), "demo.home.set"));
        assert!(view.allows(alex(PermissionLevel::Owners), "demo.never.registered"));
        assert!(view.allows(alex(PermissionLevel::Gamemasters), "demo.mod.kick"));
        assert!(!view.allows(alex(PermissionLevel::Gamemasters), "demo.home.set"));
        assert!(!view.allows(alex(PermissionLevel::All), "demo.home.set"));
        assert!(!view.allows(alex(PermissionLevel::All), "demo.mod.kick"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_node_without_op_bypass_needs_a_grant_even_for_owners() {
        let secret = Node::new("demo.vault.open", "Open the vault").op_bypass(false);
        let denied = view(vec![secret.clone()], &[]);
        assert!(!denied.allows(alex(PermissionLevel::Owners), "demo.vault.open"));
        assert_eq!(
            denied.decide(alex(PermissionLevel::Owners), "demo.vault.open"),
            Decision::Default(false)
        );
        assert!(denied.allows(Subject::Server, "demo.vault.open"));
        let granted = view(vec![secret], &[("demo.vault.open", true)]);
        assert!(granted.allows(alex(PermissionLevel::All), "demo.vault.open"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_most_specific_setting_wins_over_defaults_and_bypass() {
        let view = view(
            vec![
                Node::new("demo.chat.color", "Colors").by_default(true),
                Node::new("demo.chat.mute", "Mute"),
            ],
            &[("demo.*", true), ("demo.chat.color", false), ("*", false)],
        );
        let player = alex(PermissionLevel::Owners);
        assert!(!view.allows(player, "demo.chat.color"));
        assert!(view.allows(player, "demo.chat.mute"));
        assert_eq!(
            view.decide(player, "demo.chat.mute"),
            Decision::Explicit {
                pattern: "demo.*".into(),
                held: true
            }
        );
        assert!(!view.allows(player, "other.thing"));
        assert!(view.allows(alex(PermissionLevel::All), "demo.chat.mute"));
        let stranger = Subject::Player {
            uuid: 99,
            level: PermissionLevel::All,
        };
        assert!(view.allows(stranger, "demo.chat.color"));
        assert!(!view.allows(stranger, "demo.chat.mute"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn anonymous_sources_use_level_and_defaults_only() {
        let view = view(vec![Node::new("demo.a.b", "x")], &[]);
        assert!(!view.allows(Subject::Anonymous(PermissionLevel::Gamemasters), "demo.a.b"));
        assert!(view.allows(Subject::Anonymous(PermissionLevel::Owners), "demo.a.b"));
        assert!(!PermissionLevel::Gamemasters.has("demo.a.b"));
        assert!(PermissionLevel::Owners.has("demo.a.b"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn ids_and_patterns_are_validated() {
        assert!(valid_id("tuff.command.config"));
        assert!(valid_id("my_plugin.logout-all"));
        assert!(!valid_id("single"));
        assert!(!valid_id("Upper.case"));
        assert!(!valid_id("a..b"));
        assert!(!valid_id("a.b*"));
        assert!(!valid_id(&format!("a.{}", "b".repeat(MAX_NODE_LEN))));
        assert!(valid_pattern("*"));
        assert!(valid_pattern("tuff.*"));
        assert!(valid_pattern("tuff.web.*"));
        assert!(!valid_pattern("tuff.*.x"));
        assert!(!valid_pattern(".*"));
        let mut registry = NodeRegistry::new();
        assert!(registry.register("p", Node::new("bad", "")).is_err());
        registry.register("p", Node::new("p.ok", "")).unwrap();
        let conflict = registry.register("q", Node::new("p.ok", "")).unwrap_err();
        assert!(conflict.to_string().contains('p'), "{conflict}");
        assert!(registry.nodes().contains(WEB_ACCESS));
        assert!(valid_role("mods"));
        assert!(valid_role("build-team_2"));
        assert!(!valid_role("a.b"));
        assert!(!valid_role(""));
        assert!(!valid_role(&"g".repeat(MAX_ROLE_LEN + 1)));
    }

    fn vanilla_roots() -> crate::command::Roots {
        use crate::command::RootCommand;
        crate::command::Roots::new(
            ["gamemode", "teleport", "tp", "help", "datapack"]
                .map(|name| RootCommand::vanilla(name, &[])),
        )
    }

    fn listed(roots: &crate::command::Roots, access: &Access) -> Vec<&'static str> {
        let (nodes, root) = roots.tree(access);
        nodes[root as usize]
            .children
            .iter()
            .filter_map(|c| nodes[*c as usize].name)
            .collect()
    }

    fn parses(roots: &crate::command::Roots, access: &Access, line: &str) -> bool {
        let context = crate::command::ParseContext {
            allow_selectors: true,
        };
        roots.parse(line, access, context).error(line).is_none()
    }

    #[tuff_core::watchdog]
    #[test]
    fn vanilla_commands_keep_their_level_until_a_setting_decides() {
        let roots = vanilla_roots();
        let mut registry = NodeRegistry::new();
        registry.register_commands(&roots);
        let nodes = registry.nodes();
        let gamemode = nodes.get("minecraft.command.gamemode").unwrap();
        assert_eq!(gamemode.op_level, PermissionLevel::Gamemasters);
        assert!(!gamemode.default && gamemode.op_bypass);
        assert!(nodes.get("minecraft.command.help").unwrap().default);
        assert!(nodes.contains("minecraft.command.teleport"));
        assert!(!nodes.contains("minecraft.command.tp"));

        let empty = View::new(nodes.clone(), Arc::default());
        let op = empty.access(alex(PermissionLevel::Gamemasters));
        assert_eq!(
            listed(&roots, &op),
            ["gamemode", "teleport", "tp", "help", "datapack"]
        );
        assert!(parses(&roots, &op, "gamemode creative"));
        let player = empty.access(alex(PermissionLevel::All));
        assert_eq!(listed(&roots, &player), ["help"]);
        assert!(!parses(&roots, &player, "gamemode creative"));
        let block = empty.access(Subject::Anonymous(PermissionLevel::Gamemasters));
        assert!(parses(&roots, &block, "tp 0 0 0"));

        let granted = view(
            vec![],
            &[
                ("minecraft.command.gamemode", true),
                ("minecraft.command.teleport", true),
                ("minecraft.command.help", false),
            ],
        )
        .access(alex(PermissionLevel::All));
        assert_eq!(listed(&roots, &granted), ["gamemode", "teleport", "tp"]);
        assert!(parses(&roots, &granted, "gamemode creative"));
        assert!(parses(&roots, &granted, "tp 0 0 0"));
        assert!(!parses(&roots, &granted, "help"));

        let denied =
            view(vec![], &[("minecraft.command.*", false)]).access(alex(PermissionLevel::Owners));
        assert!(listed(&roots, &denied).is_empty());
        assert!(!parses(&roots, &denied, "gamemode creative"));

        let datapack = view(vec![], &[("minecraft.command.datapack", true)]);
        let helper = datapack.access(alex(PermissionLevel::All));
        assert!(parses(&roots, &helper, "datapack list"));
        assert!(!parses(&roots, &helper, "datapack create x y"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn role_settings_rank_between_player_settings_and_bypass() {
        let mut grants = Grants::default();
        let mut builders = Role {
            priority: 5,
            ..Role::default()
        };
        builders
            .nodes
            .insert("minecraft.command.gamemode".into(), true);
        builders.nodes.insert("demo.*".into(), false);
        grants.roles.insert("builders".into(), builders);
        let mut everyone = Role::default();
        everyone.nodes.insert("demo.chat.color".into(), true);
        grants.roles.insert(DEFAULT_ROLE.into(), everyone);
        let mut player = PlayerGrants::default();
        player.roles.insert("builders".into());
        player.nodes.insert("demo.home.set".into(), true);
        grants.players.insert(ALEX, player);
        let view = View::new(NodeRegistry::new().nodes(), Arc::new(grants));

        let owner = alex(PermissionLevel::Owners);
        assert!(view.allows(owner, "demo.home.set"));
        assert!(!view.allows(owner, "demo.chat.color"));
        assert!(view.allows(owner, "tuff.web.access"));
        let stranger = Subject::Player {
            uuid: 99,
            level: PermissionLevel::All,
        };
        assert!(view.allows(stranger, "demo.chat.color"));
        assert_eq!(
            view.decide(stranger, "demo.chat.color"),
            Decision::Role {
                role: DEFAULT_ROLE.into(),
                pattern: "demo.chat.color".into(),
                held: true
            }
        );
        let roots = vanilla_roots();
        let member = view.access(alex(PermissionLevel::All));
        assert!(parses(&roots, &member, "gamemode creative"));
    }
}
