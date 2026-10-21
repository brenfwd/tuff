//! Advancements: the tree every player shares ([`Tree`], built from the
//! compiled-in data and feature definitions, and rebuilt with data pack
//! advancements on each reload) and each player's progress ([`Progress`],
//! kept in the session's extensions). Criteria are met by
//! [`trigger::Event`]s the session fires; requirements are an AND of ORs
//! over criteria; what the client has been shown is tracked so Update
//! Advancements goes out incrementally, as the game sends it.

pub mod entry;
mod layout;
mod parse;
mod seam;
mod session;

pub use seam::SavedProgress;
pub use session::{SessionAdvancements, State};

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ops::Deref;
use std::sync::Arc;

use super::trigger::Event;
use entry::{Display, Entry, Key, MAX_DEPTH};
use serde_json::Value;
use tuff_core::world::advancement::trigger::Registry;
use tuff_core::world::advancement::{Definition, Id};
use tuff_core::world::predicate::EntityFacts;
use tuff_datapack::tags::TagSet;
use tuff_item::text::TextComponent;
use tuff_nbt::{Compound, Tag};
use tuff_vanilla_data::advancement::Advancement;
use tuff_vanilla_data::registry::TriggerType;
use tuff_vanilla_data::{Frozen, Item};

type ProgressList = Vec<(Id, Vec<(Frozen<str>, Option<i64>)>)>;

/// An advancement in a tree.
#[derive(Debug, Clone)]
pub struct Node {
    entry: Arc<Entry>,
    pub parent: Option<Id>,
    pub root: Id,
    /// Sorted by name.
    pub children: Box<[Id]>,
    /// `(x, y)` on the advancement screen; zero for nodes not displayed.
    pub position: [f32; 2],
}

impl Node {
    pub fn entry(&self) -> &Arc<Entry> {
        &self.entry
    }
}

impl Deref for Node {
    type Target = Entry;

    fn deref(&self) -> &Entry {
        &self.entry
    }
}

/// Every advancement with its criteria indexed by trigger.
pub struct Tree {
    pub nodes: Box<[Node]>,
    by_trigger: HashMap<Frozen<str>, Vec<Key>>,
    /// Inventory criteria that only look for one of a set of items.
    inventory_by_item: HashMap<Item, Vec<Key>>,
    /// Every other inventory criterion.
    inventory_other: Vec<Key>,
    pub triggers: Arc<Registry>,
    function_runner: Option<Arc<FunctionRunner>>,
}

impl std::fmt::Debug for Tree {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tree")
            .field("advancements", &self.nodes.len())
            .finish_non_exhaustive()
    }
}

/// Runs a reward function (`rewards.function`) for the player with this
/// UUID. Data pack functions do not exist here, so this is a hook.
pub type FunctionRunner = Box<dyn Fn(&str, u128) + Send + Sync>;

/// The definitions of a compiled-in tree (sorted by name, indices into
/// itself).
pub fn definitions_of(base: &'static [Advancement]) -> impl Iterator<Item = Definition> {
    base.iter().map(|a| Definition {
        name: a.name,
        parent: a.parent.map(|p| base[p as usize].name),
        display: a.display,
        criteria: a.criteria,
        requirements: a.requirements,
        rewards: a.rewards,
        sends_telemetry_event: a.sends_telemetry_event,
        position: a.position,
    })
}

/// `base` (sorted by name, indices into itself) plus `extra`, sorted by
/// name with parent, root and child indices recomputed. An extra
/// definition replaces a base one of the same name. Leaked, since a tree
/// hands out `&'static` advancements; built once per server.
pub fn merge(
    base: &'static [Advancement],
    extra: impl IntoIterator<Item = Definition>,
) -> Result<&'static [Advancement], String> {
    let mut by_name: std::collections::BTreeMap<&'static str, Definition> =
        definitions_of(base).map(|d| (d.name, d)).collect();
    for definition in extra {
        by_name.insert(definition.name, definition);
    }
    if by_name.len() > Id::MAX as usize {
        return Err("too many advancements".to_owned());
    }
    let definitions: Vec<Definition> = by_name.into_values().collect();
    let index = |name: &str| {
        definitions
            .binary_search_by(|d| d.name.cmp(name))
            .ok()
            .map(|i| i as Id)
    };
    let mut parents = Vec::with_capacity(definitions.len());
    let mut children: Vec<Vec<Id>> = vec![Vec::new(); definitions.len()];
    for (id, definition) in definitions.iter().enumerate() {
        let parent = match definition.parent {
            Some(name) => Some(index(name).ok_or_else(|| {
                format!("advancement {} has unknown parent {name}", definition.name)
            })?),
            None => None,
        };
        if let Some(parent) = parent {
            children[parent as usize].push(id as Id);
        }
        parents.push(parent);
    }
    let mut advancements = Vec::with_capacity(definitions.len());
    for (id, definition) in definitions.iter().enumerate() {
        let mut root = id as Id;
        let mut steps = 0;
        while let Some(parent) = parents[root as usize] {
            root = parent;
            steps += 1;
            if steps > definitions.len() {
                return Err(format!(
                    "advancement {} has a parent cycle",
                    definition.name
                ));
            }
        }
        advancements.push(Advancement {
            name: definition.name,
            parent: parents[id],
            root,
            children: Box::leak(std::mem::take(&mut children[id]).into_boxed_slice()),
            display: definition.display,
            criteria: definition.criteria,
            requirements: definition.requirements,
            rewards: definition.rewards,
            sends_telemetry_event: definition.sends_telemetry_event,
            position: definition.position,
        });
    }
    Ok(Box::leak(advancements.into_boxed_slice()))
}

/// Where an entry's parent chain leads while placing entries in a tree.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reach {
    Unknown,
    Visiting,
    Root(usize, usize),
    Lost,
}

/// `entries` (sorted by name, unique) as tree nodes. An advancement whose
/// parent is missing, part of a cycle or more than [`MAX_DEPTH`] deep is
/// left out with its descendants, as the game leaves out advancements it
/// cannot attach. Positions are copied from `base` for trees it holds
/// unchanged, and laid out afresh otherwise.
pub fn place(entries: Vec<Arc<Entry>>, base: Option<&Tree>) -> Vec<Node> {
    let index = |name: &str| entries.binary_search_by(|e| (*e.name).cmp(name)).ok();
    let mut reach = vec![Reach::Unknown; entries.len()];
    for start in 0..entries.len() {
        let mut path = Vec::new();
        let mut at = start;
        // The root and depth of the last node on the path, if it has one.
        let mut next = loop {
            match reach[at] {
                Reach::Root(root, depth) => break Some((root, depth + 1)),
                Reach::Lost | Reach::Visiting => break None,
                Reach::Unknown => {}
            }
            reach[at] = Reach::Visiting;
            path.push(at);
            match &entries[at].parent_name {
                None => break Some((at, 0)),
                Some(parent) => match index(parent) {
                    Some(parent) => at = parent,
                    None => break None,
                },
            }
        };
        for &node in path.iter().rev() {
            reach[node] = match next {
                Some((root, depth)) if depth <= MAX_DEPTH => {
                    next = Some((root, depth + 1));
                    Reach::Root(root, depth)
                }
                _ => {
                    next = None;
                    Reach::Lost
                }
            };
        }
    }
    let lost: Vec<&str> = reach
        .iter()
        .zip(&entries)
        .filter(|(reach, _)| **reach == Reach::Lost)
        .map(|(_, entry)| &*entry.name)
        .collect();
    if !lost.is_empty() {
        log::error!("Couldn't load advancements: {lost:?}");
    }
    let kept: Vec<usize> = (0..entries.len())
        .filter(|&i| reach[i] != Reach::Lost)
        .collect();
    let new_id = |old: usize| kept.binary_search(&old).ok().map(|i| i as Id);
    let mut nodes: Vec<Node> = kept
        .iter()
        .map(|&old| {
            let entry = &entries[old];
            let Reach::Root(root, _) = reach[old] else {
                unreachable!("kept entries reach a root")
            };
            Node {
                entry: entry.clone(),
                parent: entry
                    .parent_name
                    .as_deref()
                    .and_then(&index)
                    .and_then(new_id),
                root: new_id(root).unwrap_or(0),
                children: Box::new([]),
                position: [0.0, 0.0],
            }
        })
        .collect();
    let mut children: Vec<Vec<Id>> = vec![Vec::new(); nodes.len()];
    for (id, node) in nodes.iter().enumerate() {
        if let Some(parent) = node.parent {
            children[parent as usize].push(id as Id);
        }
    }
    for (node, children) in nodes.iter_mut().zip(children) {
        node.children = children.into_boxed_slice();
    }
    position(&mut nodes, base);
    nodes
}

/// Lays out every displayed root's tree, copying `base`'s positions for a
/// tree that is the same there.
fn position(nodes: &mut [Node], base: Option<&Tree>) {
    let mut members: HashMap<Id, Vec<Id>> = HashMap::new();
    for (id, node) in nodes.iter().enumerate() {
        members.entry(node.root).or_default().push(id as Id);
    }
    let base_sizes: HashMap<Id, usize> = base.map_or_else(HashMap::new, |base| {
        let mut sizes = HashMap::new();
        for node in base.nodes.iter() {
            *sizes.entry(node.root).or_default() += 1;
        }
        sizes
    });
    for (root, members) in members {
        if nodes[root as usize].display.is_none() {
            continue;
        }
        let same = base.and_then(|base| {
            let base_root = base.id(&nodes[root as usize].name)?;
            (base_sizes.get(&base_root) == Some(&members.len())).then_some(())?;
            members
                .iter()
                .map(|&id| {
                    let node = &nodes[id as usize];
                    let old = base.id(&node.name)?;
                    let old = &base.nodes[old as usize];
                    (old.root == base_root && Arc::ptr_eq(&old.entry, &node.entry))
                        .then_some((id, old.position))
                })
                .collect::<Option<Vec<_>>>()
        });
        let positions = match same {
            Some(positions) => positions,
            None => {
                let children: Vec<&[Id]> = nodes.iter().map(|n| &*n.children).collect();
                let displayed: Vec<bool> = nodes.iter().map(|n| n.display.is_some()).collect();
                layout::layout(root, &|id: Id| children[id as usize], &|id: Id| {
                    displayed[id as usize]
                })
            }
        };
        for (id, at) in positions {
            nodes[id as usize].position = at;
        }
    }
}

impl Tree {
    pub fn new(advancements: &'static [Advancement], triggers: Registry) -> Self {
        let nodes = advancements
            .iter()
            .map(|advancement| Node {
                entry: Arc::new(Entry::from_static(advancement, advancements)),
                parent: advancement.parent,
                root: advancement.root,
                children: advancement.children.into(),
                position: advancement.position,
            })
            .collect();
        Self::index(nodes, Arc::new(triggers), None, &TagSet::vanilla())
    }

    fn index(
        nodes: Box<[Node]>,
        triggers: Arc<Registry>,
        function_runner: Option<Arc<FunctionRunner>>,
        tags: &TagSet,
    ) -> Self {
        let mut tree = Self {
            nodes,
            by_trigger: HashMap::new(),
            inventory_by_item: HashMap::new(),
            inventory_other: Vec::new(),
            triggers,
            function_runner,
        };
        let inventory = TriggerType::InventoryChanged.name();
        for (id, node) in tree.nodes.iter().enumerate() {
            for (index, criterion) in node.criteria.iter().enumerate() {
                let key = (id as Id, index as u16);
                if &*criterion.trigger == inventory {
                    match single_item_set(&criterion.conditions, tags) {
                        Some(items) => {
                            for item in items {
                                tree.inventory_by_item.entry(item).or_default().push(key);
                            }
                        }
                        None => tree.inventory_other.push(key),
                    }
                } else {
                    tree.by_trigger
                        .entry(criterion.trigger.clone())
                        .or_default()
                        .push(key);
                }
            }
        }
        tree
    }

    /// This tree with the advancements data packs define laid over it by
    /// name: `Some` replaces (or adds) one, `None` removes it (a file that
    /// did not load). Trees left unchanged keep their layout.
    pub fn overlay(&self, changes: impl IntoIterator<Item = (String, Option<Entry>)>) -> Self {
        self.overlay_with(changes, &TagSet::vanilla())
    }

    /// [`Self::overlay`], indexing item criteria by `tags`.
    pub fn overlay_with(
        &self,
        changes: impl IntoIterator<Item = (String, Option<Entry>)>,
        tags: &TagSet,
    ) -> Self {
        let mut entries: BTreeMap<Frozen<str>, Arc<Entry>> = self
            .nodes
            .iter()
            .map(|node| (node.name.clone(), node.entry.clone()))
            .collect();
        for (name, entry) in changes {
            entries.remove(name.as_str());
            if let Some(entry) = entry {
                entries.insert(entry.name.clone(), Arc::new(entry));
            }
        }
        let mut entries: Vec<Arc<Entry>> = entries.into_values().collect();
        if entries.len() > Id::MAX as usize {
            log::error!(
                "Too many advancements ({}), leaving out the last ones",
                entries.len()
            );
            entries.truncate(Id::MAX as usize);
        }
        let nodes = place(entries, Some(self));
        Self::index(
            nodes.into_boxed_slice(),
            self.triggers.clone(),
            self.function_runner.clone(),
            tags,
        )
    }

    pub fn set_function_runner(&mut self, runner: FunctionRunner) {
        self.function_runner = Some(Arc::new(runner));
    }

    /// Runs a reward function through the hook, if one is set.
    pub fn run_function(&self, function: &str, player: u128) {
        if let Some(runner) = &self.function_runner {
            runner(function, player);
        }
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn get(&self, id: Id) -> &Node {
        &self.nodes[id as usize]
    }

    pub fn id(&self, name: &str) -> Option<Id> {
        let qualified;
        let name = if name.contains(':') {
            name
        } else {
            qualified = format!("minecraft:{name}");
            &qualified
        };
        self.nodes
            .binary_search_by(|node| (*node.name).cmp(name))
            .ok()
            .map(|i| i as Id)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> + '_ {
        self.nodes.iter().map(|node| &*node.name)
    }

    pub fn display(&self, id: Id) -> Option<&Display> {
        self.get(id).display.as_ref()
    }

    pub fn criterion_index(&self, id: Id, name: &str) -> Option<u16> {
        self.get(id)
            .criteria
            .binary_search_by(|c| (*c.name).cmp(name))
            .ok()
            .map(|i| i as u16)
    }

    /// The advancement's name as chat shows it: the title in brackets in
    /// the frame's color, with the title and description on hover.
    pub fn chat_name(&self, id: Id) -> TextComponent {
        let advancement = self.get(id);
        let Some(display) = &advancement.display else {
            return TextComponent::new(advancement.name.to_string());
        };
        let color = display.frame.color();
        let tooltip = display
            .title
            .clone()
            .color(color)
            .append(TextComponent::new("\n"))
            .append(display.description.clone());
        let title = display.title.clone().hover_text(tooltip);
        TextComponent::translate("chat.square_brackets", [title]).color(color)
    }

    /// The criteria an event may meet.
    fn candidates<'a>(&'a self, event: &Event<'_>) -> Box<dyn Iterator<Item = Key> + 'a> {
        let trigger = event.trigger();
        if trigger == TriggerType::InventoryChanged.name() {
            let by_item = event
                .changed_item()
                .and_then(|item| self.inventory_by_item.get(&item))
                .map_or(&[][..], Vec::as_slice);
            return Box::new(by_item.iter().chain(&self.inventory_other).copied());
        }
        Box::new(
            self.by_trigger
                .get(trigger)
                .map_or(&[][..], Vec::as_slice)
                .iter()
                .copied(),
        )
    }

    /// Children in display order.
    fn children(&self, id: Id) -> &[Id] {
        &self.get(id).children
    }

    /// The id `other`'s advancement `id` has here, by name.
    pub fn same_as(&self, other: &Tree, id: Id) -> Option<Id> {
        if std::ptr::eq(self, other) {
            return Some(id);
        }
        self.id(&other.nodes.get(id as usize)?.name)
    }
}

/// The items an inventory criterion needs when that is all it tests: one
/// item predicate with nothing but `items`.
fn single_item_set(conditions: &Value, tags: &TagSet) -> Option<Vec<Item>> {
    let fields = conditions.as_object()?;
    if fields.keys().any(|key| key != "items") {
        return None;
    }
    let [predicate] = fields.get("items")?.as_array()?.as_slice() else {
        return None;
    };
    let predicate = predicate.as_object()?;
    if predicate.len() != 1 {
        return None;
    }
    let names: Vec<&str> = match predicate.get("items")? {
        Value::String(name) => vec![name.as_str()],
        Value::Array(names) => names.iter().filter_map(Value::as_str).collect(),
        _ => return None,
    };
    let mut items = Vec::new();
    for name in names {
        match name.strip_prefix('#') {
            Some(tag) => {
                let tag = if tag.contains(':') {
                    tag.to_owned()
                } else {
                    format!("minecraft:{tag}")
                };
                items.extend(tags.typed::<Item>(&tag)?);
            }
            None => items.push(Item::from_name(name)?),
        }
    }
    Some(items)
}

/// What granting a criterion did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Granted {
    /// The criterion was not met before.
    pub granted: bool,
    /// The advancement became done.
    pub completed: bool,
}

/// One Update Advancements packet's worth of changes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Update {
    pub reset: bool,
    pub added: Vec<Id>,
    pub removed: Vec<Id>,
    /// Criterion names with the time each was met (epoch milliseconds).
    pub progress: ProgressList,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Rule {
    Show,
    Hide,
    NoChange,
}

/// How far up the tree a node looks for a shown or hidden ancestor.
const VISIBILITY_DEPTH: usize = 2;

/// One player's advancement progress and what its client has been told.
#[derive(Debug, Clone, Default)]
pub struct Progress {
    /// When each criterion was met (epoch milliseconds), by advancement;
    /// missing advancements have no criterion met.
    obtained: HashMap<Id, Box<[Option<i64>]>>,
    visible: HashSet<Id>,
    changed: BTreeSet<Id>,
    roots_to_update: BTreeSet<Id>,
    sent_once: bool,
    selected_tab: Option<Id>,
    /// Bumped whenever a criterion is met or un-met.
    version: u64,
}

impl Progress {
    pub fn new() -> Self {
        Self::default()
    }

    /// Progress read from a saved document. Unknown advancements and
    /// criteria are dropped, as the game drops them.
    pub fn load(tree: &Tree, root: &Compound) -> Self {
        let mut progress = Self::new();
        for (name, entry) in root.iter() {
            let (Some(id), Some(entry)) = (tree.id(name), entry.as_compound()) else {
                continue;
            };
            let Some(criteria) = entry.get_compound("criteria") else {
                continue;
            };
            for (criterion, date) in criteria.iter() {
                let (Some(index), Tag::String(date)) = (tree.criterion_index(id, criterion), date)
                else {
                    continue;
                };
                if let Some(seconds) = tuff_storage::admin::parse_file_date(date) {
                    progress.slots(tree, id)[index as usize] = Some(seconds * 1000);
                }
            }
            progress.changed.insert(id);
            progress.roots_to_update.insert(tree.get(id).root);
        }
        progress
    }

    /// This progress on `new` after a reload replaced `old`: criteria are
    /// matched by advancement and criterion name (the rest are dropped, as
    /// saving and loading again would drop them), and the client's tree is
    /// to be reset and sent whole again.
    pub fn rebind(&self, old: &Tree, new: &Tree) -> Self {
        let mut progress = Self::new();
        progress.version = self.version + 1;
        for (&id, slots) in &self.obtained {
            let Some(target) = new.same_as(old, id) else {
                continue;
            };
            let mut any = false;
            for (criterion, time) in old.get(id).criteria.iter().zip(slots.iter()) {
                let (Some(time), Some(index)) =
                    (time, new.criterion_index(target, &criterion.name))
                else {
                    continue;
                };
                progress.slots(new, target)[index as usize] = Some(*time);
                any = true;
            }
            if any {
                progress.changed.insert(target);
                progress.roots_to_update.insert(new.get(target).root);
            }
        }
        progress
    }

    /// The saved document: advancements with any criterion met.
    pub fn save(&self, tree: &Tree) -> Compound {
        let mut ids: Vec<Id> = self
            .obtained
            .iter()
            .filter(|(_, slots)| slots.iter().any(Option::is_some))
            .map(|(id, _)| *id)
            .collect();
        ids.sort_by_key(|id| (self.first_obtained(*id), *id));
        let mut root = Compound::new();
        for id in ids {
            let advancement = tree.get(id);
            let slots = &self.obtained[&id];
            let mut criteria = Compound::new();
            for (criterion, time) in advancement.criteria.iter().zip(slots.iter()) {
                if let Some(time) = time {
                    criteria.insert(
                        &*criterion.name,
                        Tag::String(tuff_storage::admin::format_file_date(time.div_euclid(1000))),
                    );
                }
            }
            let mut entry = Compound::new();
            entry.insert("criteria", Tag::Compound(criteria));
            entry.insert("done", Tag::Byte(i8::from(self.is_done(tree, id))));
            root.insert(&*advancement.name, Tag::Compound(entry));
        }
        root.insert(
            "DataVersion",
            Tag::Int(tuff_vanilla_data::version::DATA_VERSION),
        );
        root
    }

    fn first_obtained(&self, id: Id) -> Option<i64> {
        self.obtained
            .get(&id)
            .and_then(|slots| slots.iter().flatten().min().copied())
    }

    fn slots(&mut self, tree: &Tree, id: Id) -> &mut [Option<i64>] {
        self.obtained
            .entry(id)
            .or_insert_with(|| vec![None; tree.get(id).criteria.len()].into_boxed_slice())
    }

    pub fn criterion_done(&self, id: Id, criterion: u16) -> bool {
        self.obtained
            .get(&id)
            .and_then(|slots| slots.get(criterion as usize))
            .is_some_and(Option::is_some)
    }

    pub fn is_done(&self, tree: &Tree, id: Id) -> bool {
        let groups = tree.get(id).groups();
        !groups.is_empty()
            && groups
                .iter()
                .all(|group| group.iter().any(|c| self.criterion_done(id, *c)))
    }

    /// Advancements with at least one criterion met.
    pub fn started(&self) -> impl Iterator<Item = Id> + '_ {
        self.obtained
            .iter()
            .filter(|(_, slots)| slots.iter().any(Option::is_some))
            .map(|(id, _)| *id)
    }

    /// Meets a criterion at `now` (epoch milliseconds).
    pub fn grant(&mut self, tree: &Tree, id: Id, criterion: u16, now: i64) -> Granted {
        let was_done = self.is_done(tree, id);
        let slot = &mut self.slots(tree, id)[criterion as usize];
        let granted = slot.is_none();
        if granted {
            *slot = Some(now);
            self.changed.insert(id);
            self.version += 1;
        }
        let completed = !was_done && self.is_done(tree, id);
        if completed {
            self.roots_to_update.insert(tree.get(id).root);
        }
        Granted { granted, completed }
    }

    /// Un-meets a criterion; true if it was met.
    pub fn revoke(&mut self, tree: &Tree, id: Id, criterion: u16) -> bool {
        let was_done = self.is_done(tree, id);
        let Some(slot) = self
            .obtained
            .get_mut(&id)
            .and_then(|slots| slots.get_mut(criterion as usize))
        else {
            return false;
        };
        let revoked = slot.take().is_some();
        if revoked {
            self.changed.insert(id);
            self.version += 1;
        }
        if was_done && !self.is_done(tree, id) {
            self.roots_to_update.insert(tree.get(id).root);
        }
        revoked
    }

    /// The criteria `event` meets that are not met yet, of advancements
    /// not done yet. `player` is only asked for when some criterion is a
    /// candidate.
    pub fn matching(
        &self,
        tree: &Tree,
        event: &Event<'_>,
        player: impl FnOnce() -> EntityFacts,
    ) -> Vec<Key> {
        let candidates: Vec<Key> = tree
            .candidates(event)
            .filter(|&(id, criterion)| {
                !self.criterion_done(id, criterion) && !self.is_done(tree, id)
            })
            .collect();
        if candidates.is_empty() {
            return candidates;
        }
        let player = player();
        candidates
            .into_iter()
            .filter(|&(id, criterion)| {
                let criterion = &tree.get(id).criteria[criterion as usize];
                tree.triggers
                    .matches(&criterion.trigger, &criterion.conditions, event, &player)
            })
            .collect()
    }

    /// The packet the client needs now, if any: the first call always
    /// resets the client's tree.
    pub fn flush(&mut self, tree: &Tree) -> Option<Update> {
        let first = !self.sent_once;
        self.sent_once = true;
        if !first && self.roots_to_update.is_empty() && self.changed.is_empty() {
            return None;
        }
        let mut added = Vec::new();
        let mut removed = Vec::new();
        for root in std::mem::take(&mut self.roots_to_update) {
            let mut rules = vec![Rule::NoChange; VISIBILITY_DEPTH + 1];
            self.update_visibility(tree, root, &mut rules, &mut added, &mut removed);
        }
        let progress = std::mem::take(&mut self.changed)
            .into_iter()
            .filter(|id| self.visible.contains(id))
            .map(|id| {
                let names = tree.get(id).criteria.iter().enumerate().map(|(i, c)| {
                    let time = self.obtained.get(&id).and_then(|slots| slots[i]);
                    (c.name.clone(), time)
                });
                (id, names.collect())
            })
            .collect::<Vec<_>>();
        if progress.is_empty() && added.is_empty() && removed.is_empty() {
            return None;
        }
        added.reverse();
        Some(Update {
            reset: first,
            added,
            removed,
            progress,
        })
    }

    /// Walks a tree from its root deciding what the client sees: done
    /// advancements and their ancestors, and anything within two levels
    /// below a shown one unless a hidden one is closer. Returns whether
    /// the node or a descendant is done.
    fn update_visibility(
        &mut self,
        tree: &Tree,
        id: Id,
        rules: &mut Vec<Rule>,
        added: &mut Vec<Id>,
        removed: &mut Vec<Id>,
    ) -> bool {
        let done = self.is_done(tree, id);
        let rule = match &tree.get(id).display {
            None => Rule::Hide,
            Some(_) if done => Rule::Show,
            Some(display) if display.hidden => Rule::Hide,
            Some(_) => Rule::NoChange,
        };
        let mut any_done = done;
        rules.push(rule);
        for &child in tree.children(id) {
            any_done |= self.update_visibility(tree, child, rules, added, removed);
        }
        let visible = any_done
            || rules
                .iter()
                .rev()
                .take(VISIBILITY_DEPTH + 1)
                .find(|rule| **rule != Rule::NoChange)
                .is_some_and(|rule| *rule == Rule::Show);
        rules.pop();
        if visible {
            if self.visible.insert(id) {
                added.push(id);
                self.changed.insert(id);
            }
        } else if self.visible.remove(&id) {
            removed.push(id);
        }
        any_done
    }

    /// The client opened a tab: remembers it when it is a displayed root,
    /// returning the tab to tell the client about when that changed.
    pub fn select_tab(&mut self, tree: &Tree, id: Option<Id>) -> Option<Option<Id>> {
        let tab = id.filter(|id| {
            let advancement = tree.get(*id);
            advancement.parent.is_none() && advancement.display.is_some()
        });
        if tab == self.selected_tab {
            return None;
        }
        self.selected_tab = tab;
        Some(tab)
    }

    /// Changes whenever any criterion is met or un-met.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Whether the client currently sees this advancement.
    pub fn is_visible(&self, id: Id) -> bool {
        self.visible.contains(&id)
    }
}

#[cfg(test)]
mod merge_tests {
    use super::*;
    use tuff_vanilla_data::advancement as data;

    fn definition(name: &'static str, parent: Option<&'static str>) -> Definition {
        Definition {
            name,
            parent,
            display: None,
            criteria: &[],
            requirements: &[],
            rewards: data::Rewards::NONE,
            sends_telemetry_event: false,
            position: [0.0, 0.0],
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn merged_feature_advancements_form_their_own_tree() {
        let merged = merge(
            &data::ALL,
            [
                definition("zz:child", Some("aa:root")),
                definition("aa:root", None),
            ],
        )
        .unwrap();
        let tree = Tree::new(merged, Registry::new());
        let root = tree.id("aa:root").unwrap();
        let child = tree.id("zz:child").unwrap();
        assert_eq!(root, 0, "sorted by name");
        assert_eq!(tree.get(child).parent, Some(root));
        assert_eq!(tree.get(child).root, root);
        assert_eq!(&*tree.get(root).children, &[child]);
        let husbandry = tree.id("minecraft:husbandry/plant_seed").unwrap();
        let husbandry_root = tree.id("minecraft:husbandry/root").unwrap();
        assert_eq!(tree.get(husbandry).root, husbandry_root);
    }

    #[tuff_core::watchdog]
    #[test]
    fn an_unknown_parent_or_a_cycle_is_refused() {
        assert!(merge(&data::ALL, [definition("a:b", Some("a:missing"))]).is_err());
        assert!(
            merge(
                &[],
                [
                    definition("a:x", Some("a:y")),
                    definition("a:y", Some("a:x"))
                ]
            )
            .is_err()
        );
    }
}
