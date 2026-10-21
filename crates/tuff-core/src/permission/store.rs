//! The actor owning every player's and role's permission settings.
//! Changes are written to the server file area (both storage backends
//! keep one) before they are published, so a reply means the change is
//! saved.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde_json::{Map, Value, json};
use tokio::sync::{mpsc, oneshot, watch};

use super::{
    Attribute, Attributes, DEFAULT_ROLE, Grants, Nodes, PlayerGrants, Role, Subject, View,
    valid_pattern, valid_role,
};
use tuff_storage::admin::{format_uuid, parse_uuid_text};
use tuff_storage::config::ConfigStore;
use tuff_storage::migrate::{self, Step};

/// Where settings are kept in the server file area.
pub const FILE: &str = "permissions.json";
const MAX_FILE: usize = 4 << 20;
const MAX_PLAYERS: usize = 16_384;
const MAX_SETTINGS: usize = 512;
const MAX_ROLES: usize = 256;
const MAX_MEMBERSHIPS: usize = 32;
const QUEUE: usize = 64;

#[derive(Debug, PartialEq, Eq)]
pub enum SetError {
    /// Not a node id or `*` pattern.
    Invalid,
    /// Not a role name.
    InvalidRole,
    /// Not a valid value for that attribute.
    InvalidAttribute,
    /// No role has that name.
    UnknownRole,
    /// A role with that name exists.
    RoleExists,
    /// Every player is in the default role without joining it.
    DefaultRole,
    /// The player, role or store already has as many settings as allowed.
    Full,
    /// The settings could not be saved; nothing changed.
    Save(String),
    /// The actor is gone (the server is stopping).
    Stopped,
}

impl std::fmt::Display for SetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid => f.write_str("not a permission node or pattern"),
            Self::InvalidRole => {
                f.write_str("a role name is up to 32 characters of a-z, 0-9, _ and -")
            }
            Self::UnknownRole => f.write_str("no such role"),
            Self::InvalidAttribute => f.write_str(
                "attribute values are up to 512 bytes; name_color takes a colour name or #RRGGBB",
            ),
            Self::RoleExists => f.write_str("that role already exists"),
            Self::DefaultRole => f.write_str("every player is in the default role"),
            Self::Full => f.write_str("too many permission settings"),
            Self::Save(err) => write!(f, "cannot save permissions: {err}"),
            Self::Stopped => f.write_str("the server is stopping"),
        }
    }
}

/// One edit of the settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// Sets `uuid`'s own setting for `pattern`; `None` clears it.
    Player {
        uuid: u128,
        name: String,
        pattern: String,
        held: Option<bool>,
    },
    /// Sets a role's setting for `pattern`; `None` clears it.
    Role {
        role: String,
        pattern: String,
        held: Option<bool>,
    },
    CreateRole {
        role: String,
        priority: i32,
    },
    /// Deletes a role and every membership in it.
    DeleteRole {
        role: String,
    },
    Priority {
        role: String,
        priority: i32,
    },
    /// Sets one of a role's attributes; `None` clears it.
    Attribute {
        role: String,
        attribute: Attribute,
        value: Option<String>,
    },
    /// Adds `uuid` to `role` (`member`) or removes them.
    Member {
        uuid: u128,
        name: String,
        role: String,
        member: bool,
    },
}

/// Whose rights a change can alter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Affected {
    Players(Vec<u128>),
    Everyone,
}

impl Affected {
    pub fn includes(&self, uuid: u128) -> bool {
        match self {
            Self::Players(players) => players.contains(&uuid),
            Self::Everyone => true,
        }
    }
}

impl Change {
    /// Whose rights this change alters, given the settings before it.
    pub fn affected(&self, grants: &Grants) -> Affected {
        let role = match self {
            Self::Player { uuid, .. } | Self::Member { uuid, .. } => {
                return Affected::Players(vec![*uuid]);
            }
            Self::Role { role, .. }
            | Self::CreateRole { role, .. }
            | Self::DeleteRole { role }
            | Self::Priority { role, .. }
            | Self::Attribute { role, .. } => role,
        };
        if role == DEFAULT_ROLE {
            return Affected::Everyone;
        }
        Affected::Players(
            grants
                .players
                .iter()
                .filter(|(_, p)| p.roles.contains(role))
                .map(|(uuid, _)| *uuid)
                .collect(),
        )
    }
}

/// What an applied change did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    /// `false` when the settings already were that way.
    pub changed: bool,
    pub affected: Affected,
}

enum Request {
    #[cfg(test)]
    Panic,
    Apply {
        change: Change,
        reply: oneshot::Sender<Result<Applied, SetError>>,
    },
}

/// The permission store's handle: checks read the latest snapshot,
/// changes go through the actor.
#[derive(Clone)]
pub struct Permissions {
    tx: mpsc::Sender<Request>,
    view: watch::Receiver<View>,
}

impl Permissions {
    /// Starts the actor with the settings saved in `files`.
    pub async fn open(files: Option<Arc<dyn ConfigStore>>, nodes: Arc<Nodes>) -> Self {
        let Some(store) = files.clone() else {
            return Self::spawn(None, nodes, Grants::default());
        };
        match tokio::task::spawn_blocking(move || load(&*store)).await {
            Ok(Ok(grants)) => Self::spawn(files, nodes, grants),
            Ok(Err(err)) => {
                log::error!("{err}; permission changes will not be saved");
                Self::spawn(None, nodes, Grants::default())
            }
            Err(_) => Self::spawn(files, nodes, Grants::default()),
        }
    }

    /// Starts the actor with nothing saved and no persistence.
    pub fn start(nodes: Arc<Nodes>) -> Self {
        Self::spawn(None, nodes, Grants::default())
    }

    fn spawn(files: Option<Arc<dyn ConfigStore>>, nodes: Arc<Nodes>, grants: Grants) -> Self {
        let (view_tx, view) = watch::channel(View::new(nodes, Arc::new(grants)));
        let (tx, rx) = mpsc::channel(QUEUE);
        tuff_supervise::spawn("the permission store", restarting(rx, view_tx, files));
        Self { tx, view }
    }

    /// A handle nobody answers, with `nodes` and no settings, for a
    /// `Shared` built without a runtime.
    pub fn detached(nodes: Arc<Nodes>) -> Self {
        Self {
            tx: mpsc::channel(1).0,
            view: watch::channel(View::new(nodes, Arc::default())).1,
        }
    }

    pub fn view(&self) -> View {
        self.view.borrow().clone()
    }

    pub fn allows(&self, subject: Subject, node: &str) -> bool {
        self.view.borrow().allows(subject, node)
    }

    pub fn access(&self, subject: Subject) -> super::Access {
        self.view().access(subject)
    }

    /// Applies `change`, saved before it takes effect.
    pub async fn apply(&self, change: Change) -> Result<Applied, SetError> {
        let (reply, answer) = oneshot::channel();
        self.tx
            .send(Request::Apply { change, reply })
            .await
            .map_err(|_| SetError::Stopped)?;
        answer.await.map_err(|_| SetError::Stopped)?
    }

    /// Sets `uuid`'s explicit setting for `pattern` (`None` clears it),
    /// saved before it takes effect. `Ok(false)` when nothing changed.
    pub async fn set(
        &self,
        uuid: u128,
        name: &str,
        pattern: &str,
        held: Option<bool>,
    ) -> Result<bool, SetError> {
        self.apply(Change::Player {
            uuid,
            name: name.to_owned(),
            pattern: pattern.to_owned(),
            held,
        })
        .await
        .map(|applied| applied.changed)
    }
}

async fn restarting(
    mut rx: mpsc::Receiver<Request>,
    view: watch::Sender<View>,
    files: Option<Arc<dyn ConfigStore>>,
) {
    use tuff_supervise::{CatchUnwind, count_panic, panic_message};
    while let Err(panic) = CatchUnwind::new(Box::pin(run(&mut rx, &view, &files))).await {
        count_panic();
        log::error!(
            "the permission store panicked: {}; restarting it with its published settings",
            panic_message(&*panic)
        );
    }
}

async fn run(
    rx: &mut mpsc::Receiver<Request>,
    view: &watch::Sender<View>,
    files: &Option<Arc<dyn ConfigStore>>,
) {
    while let Some(request) = rx.recv().await {
        match request {
            #[cfg(test)]
            Request::Panic => panic!("test panic"),
            Request::Apply { change, reply } => {
                let current = view.borrow().clone();
                let affected = change.affected(current.grants());
                let outcome = match apply(current.grants(), change) {
                    Ok(Some(grants)) => match save(files, &grants).await {
                        Ok(()) => {
                            view.send_replace(current.with_grants(grants));
                            Ok(Applied {
                                changed: true,
                                affected,
                            })
                        }
                        Err(err) => Err(SetError::Save(err)),
                    },
                    Ok(None) => Ok(Applied {
                        changed: false,
                        affected,
                    }),
                    Err(err) => Err(err),
                };
                let _ = reply.send(outcome);
            }
        }
    }
}

/// Sets or clears `pattern` in `nodes`; `None` if that changes nothing.
fn edit_nodes(
    nodes: &BTreeMap<String, bool>,
    pattern: &str,
    held: Option<bool>,
) -> Result<Option<BTreeMap<String, bool>>, SetError> {
    if !valid_pattern(pattern) {
        return Err(SetError::Invalid);
    }
    let before = nodes.get(pattern).copied();
    if before == held {
        return Ok(None);
    }
    if before.is_none() && nodes.len() >= MAX_SETTINGS {
        return Err(SetError::Full);
    }
    let mut next = nodes.clone();
    match held {
        Some(held) => next.insert(pattern.to_owned(), held),
        None => next.remove(pattern),
    };
    Ok(Some(next))
}

fn known_role(grants: &Grants, role: &str) -> Result<(), SetError> {
    if !valid_role(role) {
        Err(SetError::InvalidRole)
    } else if grants.roles.contains_key(role) {
        Ok(())
    } else {
        Err(SetError::UnknownRole)
    }
}

/// The settings after `change`, or `None` if it changes nothing.
fn apply(grants: &Grants, change: Change) -> Result<Option<Grants>, SetError> {
    let mut next = grants.clone();
    match change {
        Change::Player {
            uuid,
            name,
            pattern,
            held,
        } => {
            let existing = grants.players.get(&uuid);
            let empty = BTreeMap::new();
            let Some(nodes) = edit_nodes(existing.map_or(&empty, |p| &p.nodes), &pattern, held)?
            else {
                return Ok(None);
            };
            if existing.is_none() && grants.players.len() >= MAX_PLAYERS {
                return Err(SetError::Full);
            }
            let player = next.players.entry(uuid).or_default();
            if !name.is_empty() {
                player.name = name;
            }
            player.nodes = nodes;
            if player.is_empty() {
                next.players.remove(&uuid);
            }
        }
        Change::Role {
            role,
            pattern,
            held,
        } => {
            known_role(grants, &role)?;
            let Some(nodes) = edit_nodes(&grants.roles[&role].nodes, &pattern, held)? else {
                return Ok(None);
            };
            next.roles.get_mut(&role).expect("checked").nodes = nodes;
        }
        Change::CreateRole { role, priority } => {
            if !valid_role(&role) {
                return Err(SetError::InvalidRole);
            }
            if grants.roles.contains_key(&role) {
                return Err(SetError::RoleExists);
            }
            if grants.roles.len() >= MAX_ROLES {
                return Err(SetError::Full);
            }
            next.roles.insert(
                role,
                Role {
                    priority,
                    ..Role::default()
                },
            );
        }
        Change::DeleteRole { role } => {
            known_role(grants, &role)?;
            next.roles.remove(&role);
            next.players.retain(|_, player| {
                player.roles.remove(&role);
                !player.is_empty()
            });
        }
        Change::Priority { role, priority } => {
            known_role(grants, &role)?;
            let entry = next.roles.get_mut(&role).expect("checked");
            if entry.priority == priority {
                return Ok(None);
            }
            entry.priority = priority;
        }
        Change::Attribute {
            role,
            attribute,
            value,
        } => {
            known_role(grants, &role)?;
            let value = value.filter(|v| !v.is_empty());
            if value.as_deref().is_some_and(|v| !attribute.valid(v)) {
                return Err(SetError::InvalidAttribute);
            }
            let slot = next
                .roles
                .get_mut(&role)
                .expect("checked")
                .attributes
                .slot_mut(attribute);
            if *slot == value {
                return Ok(None);
            }
            *slot = value;
        }
        Change::Member {
            uuid,
            name,
            role,
            member,
        } => {
            if role == DEFAULT_ROLE {
                return Err(SetError::DefaultRole);
            }
            if member {
                known_role(grants, &role)?;
            } else if !valid_role(&role) {
                return Err(SetError::InvalidRole);
            }
            let existing = grants.players.get(&uuid);
            if existing.is_some_and(|p| p.roles.contains(&role)) == member {
                return Ok(None);
            }
            if member {
                let joined = existing.map_or(0, |p| p.roles.len());
                if joined >= MAX_MEMBERSHIPS
                    || (existing.is_none() && grants.players.len() >= MAX_PLAYERS)
                {
                    return Err(SetError::Full);
                }
            }
            let player = next.players.entry(uuid).or_default();
            if !name.is_empty() {
                player.name = name;
            }
            if member {
                player.roles.insert(role);
            } else {
                player.roles.remove(&role);
            }
            if player.is_empty() {
                next.players.remove(&uuid);
            }
        }
    }
    Ok(Some(next))
}

async fn save(files: &Option<Arc<dyn ConfigStore>>, grants: &Grants) -> Result<(), String> {
    let Some(files) = files.clone() else {
        return Ok(());
    };
    let text = to_json(grants);
    match tokio::task::spawn_blocking(move || files.write_file(FILE, text.as_bytes())).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(err)) => Err(err.to_string()),
        Err(err) => Err(err.to_string()),
    }
}

fn nodes_json(nodes: &BTreeMap<String, bool>) -> Map<String, Value> {
    nodes
        .iter()
        .map(|(pattern, held)| (pattern.clone(), Value::Bool(*held)))
        .collect()
}

fn to_json(grants: &Grants) -> String {
    let mut players: Vec<(&u128, &PlayerGrants)> = grants.players.iter().collect();
    players.sort_by_key(|(uuid, _)| **uuid);
    let players: Vec<Value> = players
        .into_iter()
        .map(|(uuid, player)| {
            json!({
                "uuid": format_uuid(*uuid),
                "name": player.name,
                "roles": player.roles,
                "permissions": nodes_json(&player.nodes),
            })
        })
        .collect();
    let roles: Vec<Value> = grants
        .roles
        .iter()
        .map(|(name, role)| {
            let mut entry = json!({
                "name": name,
                "priority": role.priority,
                "permissions": nodes_json(&role.nodes),
            });
            for attribute in Attribute::ALL {
                if let Some(value) = role.attributes.get(attribute) {
                    entry[attribute.key()] = value.into();
                }
            }
            entry
        })
        .collect();
    serde_json::to_string_pretty(&json!({ "version": VERSION, "roles": roles, "players": players }))
        .unwrap_or_default()
}

fn nodes_from(entry: &Value) -> BTreeMap<String, bool> {
    entry
        .get("permissions")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(pattern, _)| valid_pattern(pattern))
        .filter_map(|(pattern, held)| Some((pattern.clone(), held.as_bool()?)))
        .take(MAX_SETTINGS)
        .collect()
}

fn attributes_from(entry: &Value) -> Attributes {
    let mut attributes = Attributes::default();
    for attribute in Attribute::ALL {
        *attributes.slot_mut(attribute) = entry
            .get(attribute.key())
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty() && attribute.valid(v))
            .map(str::to_owned);
    }
    attributes
}

/// The format version stamped into the file.
const VERSION: u32 = 1;
/// Upgrades from each older format, in order. None yet.
const MIGRATIONS: &[Step<Value>] = &[];

fn from_json(bytes: &[u8]) -> Result<Grants, migrate::Error> {
    let Ok(mut value) = serde_json::from_slice::<Value>(bytes) else {
        return Ok(Grants::default());
    };
    let found = value
        .get("version")
        .and_then(Value::as_u64)
        .map_or(VERSION, |v| u32::try_from(v).unwrap_or(u32::MAX));
    migrate::run(FILE, found, VERSION, MIGRATIONS, &mut value)?;
    Ok(grants_from(&value))
}

fn grants_from(value: &Value) -> Grants {
    let mut grants = Grants::default();
    for entry in value
        .get("roles")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if grants.roles.len() >= MAX_ROLES {
            break;
        }
        let Some(name) = entry.get("name").and_then(Value::as_str) else {
            continue;
        };
        if !valid_role(name) {
            continue;
        }
        let priority = entry
            .get("priority")
            .and_then(Value::as_i64)
            .and_then(|p| i32::try_from(p).ok())
            .unwrap_or_default();
        grants.roles.insert(
            name.to_owned(),
            Role {
                priority,
                nodes: nodes_from(entry),
                attributes: attributes_from(entry),
            },
        );
    }
    let Some(list) = value.get("players").and_then(Value::as_array) else {
        return grants;
    };
    for entry in list.iter().take(MAX_PLAYERS) {
        let Some(uuid) = entry
            .get("uuid")
            .and_then(Value::as_str)
            .and_then(parse_uuid_text)
        else {
            continue;
        };
        let name = entry
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let roles: BTreeSet<String> = entry
            .get("roles")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|g| *g != DEFAULT_ROLE && grants.roles.contains_key(*g))
            .take(MAX_MEMBERSHIPS)
            .map(str::to_owned)
            .collect();
        let player = PlayerGrants {
            name,
            nodes: nodes_from(entry),
            roles,
        };
        if !player.is_empty() {
            grants.players.insert(uuid, player);
        }
    }
    grants
}

/// The saved grants, or an error when the file must not be overwritten.
fn load(files: &dyn ConfigStore) -> Result<Grants, migrate::Error> {
    match files.read_file(FILE, MAX_FILE) {
        Ok(Some(bytes)) => from_json(&bytes),
        Ok(None) => Ok(Grants::default()),
        Err(err) => {
            log::warn!("cannot read {FILE}: {err}");
            Ok(Grants::default())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permission::{Decision, Node, NodeRegistry};
    use tuff_storage::admin::PermissionLevel;
    use tuff_storage::backend::WorldBackend;
    use tuff_storage::test_dir::TestDir;

    const ALEX: u128 = 0x1234;
    const SAM: u128 = 0x5678;

    fn nodes() -> Arc<Nodes> {
        let mut registry = NodeRegistry::new();
        registry
            .register("demo", Node::new("demo.vault.open", "x").op_bypass(false))
            .unwrap();
        registry.nodes()
    }

    fn alex(level: PermissionLevel) -> Subject {
        Subject::Player { uuid: ALEX, level }
    }

    fn alex_as(uuid: u128) -> Subject {
        Subject::Player {
            uuid,
            level: PermissionLevel::All,
        }
    }

    fn create(role: &str, priority: i32) -> Change {
        Change::CreateRole {
            role: role.into(),
            priority,
        }
    }

    fn role(role: &str, pattern: &str, held: Option<bool>) -> Change {
        Change::Role {
            role: role.into(),
            pattern: pattern.into(),
            held,
        }
    }

    fn member(uuid: u128, role: &str, member: bool) -> Change {
        Change::Member {
            uuid,
            name: "Alex".into(),
            role: role.into(),
            member,
        }
    }

    async fn changed(store: &Permissions, change: Change) -> Result<bool, SetError> {
        store.apply(change).await.map(|a| a.changed)
    }

    async fn round_trip(files: Arc<dyn ConfigStore>) {
        let store = Permissions::open(Some(files.clone()), nodes()).await;
        assert!(!store.allows(alex(PermissionLevel::Owners), "demo.vault.open"));
        assert_eq!(
            store.set(ALEX, "Alex", "demo.vault.open", Some(true)).await,
            Ok(true)
        );
        assert_eq!(
            store.set(ALEX, "Alex", "demo.vault.open", Some(true)).await,
            Ok(false)
        );
        assert_eq!(
            store.set(ALEX, "Alex", "tuff.*", Some(false)).await,
            Ok(true)
        );
        assert_eq!(
            store.set(ALEX, "Alex", "bad", Some(true)).await,
            Err(SetError::Invalid)
        );
        assert!(store.allows(alex(PermissionLevel::All), "demo.vault.open"));
        assert_eq!(changed(&store, create("builders", 3)).await, Ok(true));
        assert_eq!(
            changed(&store, create("builders", 1)).await,
            Err(SetError::RoleExists)
        );
        assert_eq!(
            changed(&store, role("builders", "minecraft.command.*", Some(true))).await,
            Ok(true)
        );
        assert_eq!(
            changed(&store, member(SAM, "builders", true)).await,
            Ok(true)
        );
        drop(store);

        let reopened = Permissions::open(Some(files.clone()), nodes()).await;
        let view = reopened.view();
        assert!(view.allows(alex(PermissionLevel::All), "demo.vault.open"));
        assert!(!view.allows(alex(PermissionLevel::Owners), "tuff.web.access"));
        assert_eq!(view.player(ALEX).unwrap().name, "Alex");
        let sam = Subject::Player {
            uuid: SAM,
            level: PermissionLevel::All,
        };
        assert!(view.allows(sam, "minecraft.command.gamemode"));
        assert_eq!(view.grants().roles["builders"].priority, 3);
        assert_eq!(
            reopened.set(ALEX, "Alex", "demo.vault.open", None).await,
            Ok(true)
        );
        assert_eq!(reopened.set(ALEX, "Alex", "tuff.*", None).await, Ok(true));
        assert!(reopened.view().player(ALEX).is_none());
        assert_eq!(
            changed(
                &reopened,
                Change::DeleteRole {
                    role: "builders".into()
                }
            )
            .await,
            Ok(true)
        );
        assert!(reopened.view().player(SAM).is_none());
        drop(reopened);
        let empty = Permissions::open(Some(files), nodes()).await;
        assert!(empty.view().grants().players.is_empty());
        assert!(empty.view().grants().roles.is_empty());
    }

    fn attribute(role: &str, attribute: Attribute, value: Option<&str>) -> Change {
        Change::Attribute {
            role: role.into(),
            attribute,
            value: value.map(str::to_owned),
        }
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn role_attributes_persist_and_decorate_names_by_priority() {
        use tuff_item::text::TextComponent;
        let dir = TestDir::new();
        let files: Arc<dyn ConfigStore> = Arc::new(
            tuff_storage::vanilla::config::VanillaConfig::new(dir.path()),
        );
        let store = Permissions::open(Some(files.clone()), nodes()).await;
        let steve = TextComponent::new("Steve");
        assert_eq!(store.view().decorate_name(ALEX, steve.clone()), steve);
        assert_eq!(
            changed(&store, attribute("nope", Attribute::Prefix, Some("x"))).await,
            Err(SetError::UnknownRole)
        );
        for change in [
            create(DEFAULT_ROLE, 0),
            create("admin", 5),
            member(ALEX, "admin", true),
            attribute(DEFAULT_ROLE, Attribute::Prefix, Some("<gray>[Member] ")),
            attribute(DEFAULT_ROLE, Attribute::NameColor, Some("grey")),
            attribute("admin", Attribute::Prefix, Some("<red>[Admin] ")),
            attribute("admin", Attribute::DisplayName, Some("<red>Admin")),
        ] {
            assert_eq!(changed(&store, change).await, Ok(true));
        }
        assert_eq!(
            changed(
                &store,
                attribute("admin", Attribute::Prefix, Some("<red>[Admin] "))
            )
            .await,
            Ok(false)
        );
        assert_eq!(
            changed(
                &store,
                attribute("admin", Attribute::NameColor, Some("nocolour"))
            )
            .await,
            Err(SetError::InvalidAttribute)
        );
        let long = "x".repeat(crate::permission::MAX_ATTRIBUTE_LEN + 1);
        assert_eq!(
            changed(&store, attribute("admin", Attribute::Suffix, Some(&long))).await,
            Err(SetError::InvalidAttribute)
        );
        let expect_alex = {
            let mut out = TextComponent::new("");
            out.extra = vec![
                TextComponent::new("[Admin] ").color("red"),
                TextComponent::new("Steve").color("gray"),
            ];
            out
        };
        assert_eq!(store.view().decorate_name(ALEX, steve.clone()), expect_alex);
        assert_eq!(
            store.view().decorate_name(SAM, steve.clone()).extra[0],
            TextComponent::new("[Member] ").color("gray")
        );
        drop(store);

        let reopened = Permissions::open(Some(files), nodes()).await;
        assert_eq!(
            reopened.view().decorate_name(ALEX, steve.clone()),
            expect_alex
        );
        let admin = &reopened.view().grants().roles["admin"].attributes.clone();
        assert_eq!(admin.display_name.as_deref(), Some("<red>Admin"));
        assert_eq!(
            changed(&reopened, attribute("admin", Attribute::Prefix, None)).await,
            Ok(true)
        );
        assert_eq!(
            reopened.view().decorate_name(ALEX, steve).extra[0],
            TextComponent::new("[Member] ").color("gray")
        );
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn settings_survive_a_restart_in_the_folder_backend() {
        let dir = TestDir::new();
        let files: Arc<dyn ConfigStore> = Arc::new(
            tuff_storage::vanilla::config::VanillaConfig::new(dir.path()),
        );
        round_trip(files).await;
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn settings_survive_a_restart_in_the_server_file() {
        let dir = TestDir::new();
        let backend =
            tuff_storage::sqlite::SqliteBackend::open(dir.path().join("world.mcserver")).unwrap();
        let files = backend.config().expect("the server file keeps the config");
        round_trip(files).await;
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn players_take_settings_from_their_roles_by_priority() {
        let store = Permissions::start(nodes());
        for change in [
            create("helpers", 1),
            create("mods", 10),
            create(DEFAULT_ROLE, 0),
            role("helpers", "demo.chat.*", Some(true)),
            role("helpers", "demo.vault.open", Some(true)),
            role("mods", "demo.vault.open", Some(false)),
            role(DEFAULT_ROLE, "demo.chat.mute", Some(false)),
            member(ALEX, "helpers", true),
        ] {
            assert_eq!(changed(&store, change).await, Ok(true));
        }
        let view = store.view();
        let player = alex(PermissionLevel::All);
        assert!(view.allows(player, "demo.vault.open"));
        assert!(view.allows(player, "demo.chat.mute"));
        assert_eq!(
            view.decide(player, "demo.chat.mute"),
            Decision::Role {
                role: "helpers".into(),
                pattern: "demo.chat.*".into(),
                held: true
            }
        );
        let stranger = Subject::Player {
            uuid: SAM,
            level: PermissionLevel::Owners,
        };
        assert!(!view.allows(stranger, "demo.chat.mute"));
        assert!(view.allows(stranger, "demo.chat.color"));

        assert_eq!(changed(&store, member(ALEX, "mods", true)).await, Ok(true));
        assert!(!store.allows(player, "demo.vault.open"));
        assert_eq!(
            changed(
                &store,
                Change::Priority {
                    role: "mods".into(),
                    priority: 0
                }
            )
            .await,
            Ok(true)
        );
        assert!(store.allows(player, "demo.vault.open"));
        assert_eq!(
            store
                .set(ALEX, "Alex", "demo.vault.open", Some(false))
                .await,
            Ok(true)
        );
        assert!(!store.allows(player, "demo.vault.open"));
        assert_eq!(
            store
                .view()
                .roles_of(ALEX)
                .iter()
                .map(|g| g.0)
                .collect::<Vec<_>>(),
            ["helpers", "default", "mods"]
        );
        assert_eq!(
            changed(&store, member(ALEX, DEFAULT_ROLE, true)).await,
            Err(SetError::DefaultRole)
        );
        assert_eq!(
            changed(&store, member(ALEX, "nobody", true)).await,
            Err(SetError::UnknownRole)
        );
        assert_eq!(
            changed(&store, create("Bad Name", 0)).await,
            Err(SetError::InvalidRole)
        );
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn changes_name_whose_rights_they_alter() {
        let store = Permissions::start(nodes());
        store.apply(create("mods", 1)).await.unwrap();
        store.apply(member(ALEX, "mods", true)).await.unwrap();
        let applied = store
            .apply(role("mods", "demo.a", Some(true)))
            .await
            .unwrap();
        assert_eq!(applied.affected, Affected::Players(vec![ALEX]));
        assert!(applied.affected.includes(ALEX));
        let applied = store.apply(create(DEFAULT_ROLE, 0)).await.unwrap();
        assert_eq!(applied.affected, Affected::Everyone);
        let applied = store.set(SAM, "Sam", "demo.a", Some(true)).await;
        assert_eq!(applied, Ok(true));
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn a_panic_restarts_the_store_keeping_published_settings() {
        let store = Permissions::start(nodes());
        assert_eq!(store.set(SAM, "Sam", "demo.a", Some(true)).await, Ok(true));
        store.tx.send(Request::Panic).await.unwrap();
        assert!(store.allows(alex_as(SAM), "demo.a"));
        assert_eq!(store.set(SAM, "Sam", "demo.b", Some(true)).await, Ok(true));
        assert!(store.allows(alex_as(SAM), "demo.b"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn malformed_files_load_what_they_can() {
        let text = br#"{"roles":[
            {"name":"mods","priority":5,"permissions":{"a.b":true,"Bad":true}},
            {"name":"Not Valid","permissions":{}},
            {"priority":1}
        ],"players":[
            {"uuid":"not a uuid","permissions":{"a.b":true}},
            {"uuid":"00000000-0000-0000-0000-000000001234","name":"Alex",
             "roles":["mods","gone","default"],
             "permissions":{"a.b":true,"Bad":true,"c.*":"yes","d.e":false}},
            {"uuid":"00000000-0000-0000-0000-000000005678","roles":["gone"]}
        ]}"#;
        let grants = from_json(text).unwrap();
        assert_eq!(grants.players.len(), 1);
        let alex = &grants.players[&ALEX];
        assert_eq!(alex.nodes.len(), 2);
        assert!(!alex.nodes["d.e"]);
        assert_eq!(alex.roles.iter().collect::<Vec<_>>(), ["mods"]);
        assert_eq!(grants.roles.len(), 1);
        assert_eq!(grants.roles["mods"].priority, 5);
        assert_eq!(grants.roles["mods"].nodes.len(), 1);
        assert!(from_json(b"[").unwrap().players.is_empty());
        assert!(from_json(b"{}").unwrap().players.is_empty());
        let unstamped = br#"{"players":[{"uuid":"00000000-0000-0000-0000-000000001234",
            "permissions":{"a.b":true}}]}"#;
        assert_eq!(from_json(unstamped).unwrap().players[&ALEX].nodes.len(), 1);
        assert!(matches!(
            from_json(br#"{"version":2,"players":[]}"#),
            Err(migrate::Error::Newer { found: 2, .. })
        ));
        let saved = to_json(&from_json(text).unwrap());
        assert!(saved.contains("\"version\": 1"), "{saved}");
        assert!(MIGRATIONS.is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn players_and_roles_cannot_hold_unbounded_settings() {
        let mut grants = Grants::default();
        let set = |pattern: &str, held| Change::Player {
            uuid: ALEX,
            name: "Alex".into(),
            pattern: pattern.into(),
            held,
        };
        for i in 0..MAX_SETTINGS {
            grants = apply(&grants, set(&format!("n.n{i}"), Some(true)))
                .unwrap()
                .unwrap();
        }
        assert_eq!(
            apply(&grants, set("n.more", Some(true))),
            Err(SetError::Full)
        );
        assert!(apply(&grants, set("n.n0", Some(false))).unwrap().is_some());
        for i in 0..MAX_ROLES {
            grants = apply(&grants, create(&format!("g{i}"), 0))
                .unwrap()
                .unwrap();
        }
        assert_eq!(apply(&grants, create("more", 0)), Err(SetError::Full));
        for i in 0..MAX_MEMBERSHIPS {
            grants = apply(&grants, member(ALEX, &format!("g{i}"), true))
                .unwrap()
                .unwrap();
        }
        assert_eq!(
            apply(&grants, member(ALEX, "g200", true)),
            Err(SetError::Full)
        );
    }
}
