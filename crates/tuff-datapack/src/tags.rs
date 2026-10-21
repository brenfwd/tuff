//! Tag files (`tags/<registry>/<name>.json`): entries merged across packs
//! in priority order (`replace` drops what lower packs listed), then
//! resolved with nested tags expanded, in first-seen order without
//! duplicates. The built-in pack's files come from the compiled-in data.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, OnceLock};

use bytes::Bytes;
use serde_json::Value;
use tuff_vanilla_data::Registry;
use tuff_vanilla_data::data_registry::DataRegistry;
use tuff_vanilla_data::registry::{
    Block, EntityType, Fluid, GameEvent, Item, PointOfInterestType, Potion,
};
use tuff_vanilla_data::tag::Key;

/// One `values` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagEntry {
    pub id: String,
    pub tag: bool,
    pub required: bool,
}

impl TagEntry {
    fn parse(value: &Value) -> Result<Self, String> {
        let (text, required) = match value {
            Value::String(text) => (text.as_str(), true),
            Value::Object(entry) => {
                let text = entry
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or("tag entry without an id")?;
                let required = match entry.get("required") {
                    None => true,
                    Some(Value::Bool(required)) => *required,
                    Some(_) => return Err("tag entry required is not a boolean".to_owned()),
                };
                (text, required)
            }
            _ => return Err("tag entry is neither a string nor an object".to_owned()),
        };
        let (tag, id) = match text.strip_prefix('#') {
            Some(id) => (true, id),
            None => (false, text),
        };
        let id = crate::identifier(id).ok_or_else(|| format!("invalid tag entry {text:?}"))?;
        Ok(Self { id, tag, required })
    }
}

/// A tag file's entries and whether it replaces lower packs'.
pub fn parse(bytes: &[u8]) -> Result<(bool, Vec<TagEntry>), String> {
    let root: Value = serde_json::from_slice(bytes).map_err(|err| err.to_string())?;
    let root = root.as_object().ok_or("tag file is not an object")?;
    let replace = match root.get("replace") {
        None => false,
        Some(Value::Bool(replace)) => *replace,
        Some(_) => return Err("replace is not a boolean".to_owned()),
    };
    let values = root
        .get("values")
        .and_then(Value::as_array)
        .ok_or("tag file without values")?;
    let entries = values
        .iter()
        .map(TagEntry::parse)
        .collect::<Result<_, _>>()?;
    Ok((replace, entries))
}

/// Resolves every tag in `builders` against the elements `exists` knows.
/// A tag missing a required element or tag (including one in a cycle) is
/// dropped, with the reason in the returned errors.
pub fn resolve(
    builders: &BTreeMap<String, Vec<TagEntry>>,
    exists: impl Fn(&str) -> bool,
) -> (BTreeMap<String, Vec<String>>, Vec<String>) {
    #[derive(Clone)]
    enum State {
        Visiting,
        Done(Option<Vec<String>>),
    }
    fn visit(
        id: &str,
        builders: &BTreeMap<String, Vec<TagEntry>>,
        exists: &dyn Fn(&str) -> bool,
        states: &mut HashMap<String, State>,
        errors: &mut Vec<String>,
    ) -> Option<Vec<String>> {
        match states.get(id) {
            Some(State::Done(done)) => return done.clone(),
            Some(State::Visiting) => return None,
            None => {}
        }
        let entries = builders.get(id)?;
        states.insert(id.to_owned(), State::Visiting);
        let mut values: Vec<String> = Vec::new();
        let mut missing = Vec::new();
        let push = |values: &mut Vec<String>, value: &str| {
            if !values.iter().any(|v| v == value) {
                values.push(value.to_owned());
            }
        };
        for entry in entries {
            if entry.tag {
                match visit(&entry.id, builders, exists, states, errors) {
                    Some(nested) => {
                        for value in &nested {
                            push(&mut values, value);
                        }
                    }
                    None if entry.required => missing.push(format!("#{}", entry.id)),
                    None => {}
                }
            } else if exists(&entry.id) {
                push(&mut values, &entry.id);
            } else if entry.required {
                missing.push(entry.id.clone());
            }
        }
        let result = if missing.is_empty() {
            Some(values)
        } else {
            errors.push(format!(
                "Couldn't load tag {id} as it is missing following references: {}",
                missing.join(", ")
            ));
            None
        };
        states.insert(id.to_owned(), State::Done(result.clone()));
        result
    }
    let mut states = HashMap::new();
    let mut errors = Vec::new();
    let mut tags = BTreeMap::new();
    for id in builders.keys() {
        if let Some(values) = visit(id, builders, &exists, &mut states, &mut errors) {
            tags.insert(id.clone(), values);
        }
    }
    (tags, errors)
}

/// Tag files of one registry being merged, by tag id.
pub type Builders = BTreeMap<String, Vec<TagEntry>>;

/// Every registry's tag files as the built-in pack ships them.
pub fn vanilla_builders() -> BTreeMap<&'static str, Builders> {
    tuff_vanilla_data::tag::raw::ALL
        .iter()
        .map(|(registry, tags)| {
            let builders = tags
                .iter()
                .map(|(tag, entries)| {
                    let entries = entries
                        .iter()
                        .map(|entry| match entry.strip_prefix('#') {
                            Some(id) => TagEntry {
                                id: id.to_owned(),
                                tag: true,
                                required: true,
                            },
                            None => TagEntry {
                                id: (*entry).to_owned(),
                                tag: false,
                                required: true,
                            },
                        })
                        .collect();
                    ((*tag).to_owned(), entries)
                })
                .collect();
            (*registry, builders)
        })
        .collect()
}

/// Built-in registries whose tags clients are sent, in the game's order.
const NETWORK_TYPED: [&str; 7] = [
    Block::KEY,
    EntityType::KEY,
    Fluid::KEY,
    GameEvent::KEY,
    Item::KEY,
    PointOfInterestType::KEY,
    Potion::KEY,
];

fn typed_exists<T: Registry>(id: &str) -> bool {
    T::from_name(id).is_some()
}

/// Whether `id` names an element of `registry`; `None` for a registry
/// whose elements this server cannot list.
pub fn element_exists(registry: &str, id: &str) -> Option<bool> {
    Some(match registry {
        Block::KEY => typed_exists::<Block>(id),
        EntityType::KEY => typed_exists::<EntityType>(id),
        Fluid::KEY => typed_exists::<Fluid>(id),
        GameEvent::KEY => typed_exists::<GameEvent>(id),
        Item::KEY => typed_exists::<Item>(id),
        PointOfInterestType::KEY => typed_exists::<PointOfInterestType>(id),
        Potion::KEY => typed_exists::<Potion>(id),
        "minecraft:villager_trade" => tuff_vanilla_data::villager::Trade::get(id).is_some(),
        _ => match DataRegistry::get(registry) {
            Some(data) if !data.entries.is_empty() => data.entries.binary_search(&id).is_ok(),
            _ => return None,
        },
    })
}

/// The registry a tag directory below `tags/` belongs to, with the rest
/// of the path: the longest known registry path wins.
pub fn split_registry(path: &str) -> Option<(&'static str, &str)> {
    static PATHS: OnceLock<Vec<(&'static str, &'static str)>> = OnceLock::new();
    let paths = PATHS.get_or_init(|| {
        let mut keys: Vec<&'static str> = tuff_vanilla_data::tag::raw::ALL
            .iter()
            .map(|(key, _)| *key)
            .chain(NETWORK_TYPED)
            .chain(
                tuff_vanilla_data::data_registry::ALL
                    .iter()
                    .map(|registry| registry.key),
            )
            .collect();
        keys.sort_unstable();
        keys.dedup();
        let mut paths: Vec<(&'static str, &'static str)> = keys
            .into_iter()
            .map(|key| (key.strip_prefix("minecraft:").unwrap_or(key), key))
            .collect();
        paths.sort_by_key(|(path, _)| std::cmp::Reverse(path.len()));
        paths
    });
    paths.iter().find_map(|(prefix, key)| {
        path.strip_prefix(prefix)
            .and_then(|rest| rest.strip_prefix('/'))
            .filter(|rest| !rest.is_empty())
            .map(|rest| (*key, rest))
    })
}

/// The members of the built-in tags of one registry, by [`Key::index`]:
/// a bitset over element ids for a built-in registry, sorted ids for a
/// data-driven one.
#[derive(Debug, Default)]
struct Indexed {
    stride: usize,
    bits: Box<[u64]>,
    names: Box<[Box<[Box<str>]>]>,
}

impl Indexed {
    fn build(registry: &str, tags: Option<&BTreeMap<String, Vec<String>>>) -> Self {
        let built_in = tuff_vanilla_data::tag::raw::ALL
            .iter()
            .find(|(key, _)| *key == registry)
            .map_or(&[][..], |(_, tags)| *tags);
        let members = |tag: &str| {
            tags.and_then(|tags| tags.get(tag))
                .map_or(&[][..], Vec::as_slice)
        };
        match typed_len(registry) {
            Some(len) => {
                let stride = len.div_ceil(64);
                let mut bits = vec![0u64; stride * built_in.len()].into_boxed_slice();
                for (index, (tag, _)) in built_in.iter().enumerate() {
                    for id in members(tag) {
                        if let Some(element) = typed_id(registry, id) {
                            bits[index * stride + element / 64] |= 1 << (element % 64);
                        }
                    }
                }
                Self {
                    stride,
                    bits,
                    names: Box::default(),
                }
            }
            None => Self {
                stride: 0,
                bits: Box::default(),
                names: built_in
                    .iter()
                    .map(|(tag, _)| {
                        let mut ids: Vec<Box<str>> =
                            members(tag).iter().map(|id| id.as_str().into()).collect();
                        ids.sort_unstable();
                        ids.into_boxed_slice()
                    })
                    .collect(),
            },
        }
    }

    #[inline]
    fn has_id(&self, index: usize, element: usize) -> bool {
        self.bits
            .get(index * self.stride + element / 64)
            .is_some_and(|word| word & (1 << (element % 64)) != 0)
    }

    fn has_name(&self, index: usize, id: &str) -> bool {
        self.names
            .get(index)
            .is_some_and(|ids| ids.binary_search_by(|e| (**e).cmp(id)).is_ok())
    }
}

fn typed_len(registry: &str) -> Option<usize> {
    Some(match registry {
        Block::KEY => Block::ALL.len(),
        EntityType::KEY => EntityType::ALL.len(),
        Fluid::KEY => Fluid::ALL.len(),
        GameEvent::KEY => GameEvent::ALL.len(),
        Item::KEY => Item::ALL.len(),
        PointOfInterestType::KEY => PointOfInterestType::ALL.len(),
        Potion::KEY => Potion::ALL.len(),
        _ => return None,
    })
}

fn typed_id(registry: &str, id: &str) -> Option<usize> {
    let id = match registry {
        Block::KEY => Block::from_name(id)?.id(),
        EntityType::KEY => EntityType::from_name(id)?.id(),
        Fluid::KEY => Fluid::from_name(id)?.id(),
        GameEvent::KEY => GameEvent::from_name(id)?.id(),
        Item::KEY => Item::from_name(id)?.id(),
        PointOfInterestType::KEY => PointOfInterestType::from_name(id)?.id(),
        Potion::KEY => Potion::from_name(id)?.id(),
        _ => return None,
    };
    Some(id as usize)
}

/// The resolved tags of every registry.
#[derive(Debug, Default)]
pub struct TagSet {
    registries: BTreeMap<&'static str, BTreeMap<String, Vec<String>>>,
    /// The built-in tags' members, by [`Key::registry`].
    indexed: Box<[Indexed]>,
    fingerprint: u64,
    state_flags: OnceLock<Box<[u8]>>,
}

/// [`TagSet::state_flags`] bit: the state's block is in `blocks_motion_in_heightmap`.
pub const FLAG_BLOCKS_MOTION: u8 = 32;
/// [`TagSet::state_flags`] bit: the state's block is in `blocks_motion_in_heightmap_no_leaves`.
pub const FLAG_BLOCKS_MOTION_NO_LEAVES: u8 = 64;

impl TagSet {
    /// The built-in pack's tags alone.
    pub fn vanilla() -> Arc<TagSet> {
        static SET: OnceLock<Arc<TagSet>> = OnceLock::new();
        SET.get_or_init(|| {
            let (set, errors) = Self::resolve_all(&vanilla_builders());
            debug_assert!(errors.is_empty(), "{errors:?}");
            Arc::new(set)
        })
        .clone()
    }

    /// Resolves merged tag files; a tag missing a required element is
    /// left out, with the reason among the returned errors.
    pub fn resolve_all(builders: &BTreeMap<&'static str, Builders>) -> (Self, Vec<String>) {
        Self::resolve_all_with(builders, &|_, _| None)
    }

    /// [`Self::resolve_all`], asking `exists` first whether an element is
    /// in its registry (`None` falls back to the built-in registries).
    pub fn resolve_all_with(
        builders: &BTreeMap<&'static str, Builders>,
        exists: &dyn Fn(&str, &str) -> Option<bool>,
    ) -> (Self, Vec<String>) {
        let mut errors = Vec::new();
        let mut registries = BTreeMap::new();
        for (registry, files) in builders {
            let (tags, mut failed) = resolve(files, |id| {
                exists(registry, id)
                    .or_else(|| element_exists(registry, id))
                    .unwrap_or(true)
            });
            errors.append(&mut failed);
            registries.insert(*registry, tags);
        }
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        registries.hash(&mut hasher);
        let indexed = tuff_vanilla_data::tag::REGISTRIES
            .iter()
            .map(|registry| Indexed::build(registry, registries.get(registry)))
            .collect();
        (
            Self {
                registries,
                indexed,
                fingerprint: hasher.finish(),
                state_flags: OnceLock::new(),
            },
            errors,
        )
    }

    /// Per vanilla state id, the state's `BlockState::flags` plus the
    /// [`FLAG_BLOCKS_MOTION`] and [`FLAG_BLOCKS_MOTION_NO_LEAVES`] bits of
    /// this tag set, built on first use. Index with `BlockState::id`.
    pub fn state_flags(&self) -> &[u8] {
        self.state_flags.get_or_init(|| {
            use tuff_vanilla_data::tag::block::key;
            let motion: Vec<u8> = tuff_vanilla_data::Block::ALL
                .iter()
                .map(|&block| {
                    let motion = u8::from(self.is(key::BLOCKS_MOTION_IN_HEIGHTMAP, block));
                    let no_leaves =
                        u8::from(self.is(key::BLOCKS_MOTION_IN_HEIGHTMAP_NO_LEAVES, block));
                    (motion * FLAG_BLOCKS_MOTION) | (no_leaves * FLAG_BLOCKS_MOTION_NO_LEAVES)
                })
                .collect();
            tuff_vanilla_data::BlockState::flag_table()
                .iter()
                .enumerate()
                .map(|(id, &flags)| {
                    let block = tuff_vanilla_data::BlockState::from_id(id as u32)
                        .expect("state id in range")
                        .block();
                    flags | motion[block as usize]
                })
                .collect()
        })
    }

    /// Whether the built-in tag `key` holds `value` here.
    #[inline]
    #[track_caller]
    pub fn is<T: Registry>(&self, key: Key<T>, value: T) -> bool {
        self.indexed
            .get(key.registry)
            .is_some_and(|indexed| indexed.has_id(key.index, value.id() as usize))
    }

    /// Whether the built-in tag `key` of a data-driven registry holds `id`.
    #[track_caller]
    pub fn has(&self, key: Key<&'static str>, id: &str) -> bool {
        self.indexed
            .get(key.registry)
            .is_some_and(|indexed| indexed.has_name(key.index, id))
    }

    /// Whether `#tag` of `registry` holds `id`; false for a tag that does
    /// not exist.
    pub fn contains(&self, registry: &str, tag: &str, id: &str) -> bool {
        self.get(registry, tag)
            .is_some_and(|members| members.iter().any(|member| member == id))
    }

    /// Changes whenever any tag's members do.
    pub fn fingerprint(&self) -> u64 {
        self.fingerprint
    }

    /// The members of `#tag` in `registry`, in order.
    pub fn get(&self, registry: &str, tag: &str) -> Option<&[String]> {
        self.registries.get(registry)?.get(tag).map(Vec::as_slice)
    }

    /// Every tag id of `registry`, sorted.
    pub fn names(&self, registry: &str) -> impl Iterator<Item = &str> {
        self.registries
            .get(registry)
            .into_iter()
            .flat_map(|tags| tags.keys().map(String::as_str))
    }

    /// The members of a built-in registry's tag, typed.
    pub fn typed<T: Registry>(&self, tag: &str) -> Option<Vec<T>> {
        self.get(T::KEY, tag)
            .map(|members| members.iter().filter_map(|id| T::from_name(id)).collect())
    }

    /// The Update Tags body (configuration and play share it): the
    /// built-in registries clients know, then every synchronized data
    /// registry with tags, by id.
    pub fn network_body(&self) -> Bytes {
        self.network_body_with(&|key, id| {
            DataRegistry::get(key)?
                .entries
                .binary_search(&id)
                .ok()
                .map(|i| i as u32)
        })
    }

    /// [`Self::network_body`] with data registry ids numbered by
    /// `network_id(registry, id)`, for registries data packs changed.
    pub fn network_body_with(&self, network_id: &dyn Fn(&str, &str) -> Option<u32>) -> Bytes {
        let mut written = 0;
        let mut body = BodyWriter::default();
        for key in NETWORK_TYPED {
            if let Some(tags) = self.registries.get(key) {
                write_tags(&mut body, key, tags, |id| match key {
                    Block::KEY => Block::from_name(id).map(Block::id),
                    EntityType::KEY => EntityType::from_name(id).map(EntityType::id),
                    Fluid::KEY => Fluid::from_name(id).map(Fluid::id),
                    GameEvent::KEY => GameEvent::from_name(id).map(GameEvent::id),
                    Item::KEY => Item::from_name(id).map(Item::id),
                    PointOfInterestType::KEY => {
                        PointOfInterestType::from_name(id).map(PointOfInterestType::id)
                    }
                    _ => Potion::from_name(id).map(Potion::id),
                });
                written += 1;
            }
        }
        for (key, tags) in &self.registries {
            let Some(data) = DataRegistry::get(key).filter(|data| data.synchronized) else {
                continue;
            };
            if tags.is_empty() {
                continue;
            }
            write_tags(&mut body, key, tags, |id| network_id(data.key, id));
            written += 1;
        }
        let mut w = BodyWriter::default();
        w.write_var_int(written);
        w.write_bytes(&body.0);
        Bytes::from(w.0)
    }
}

tokio::task_local! {
    static CURRENT: RefCell<Arc<TagSet>>;
}

/// Runs `future` with `tags` as the tags in effect for every check made
/// while it is polled (the actor's own reloadable snapshot).
pub fn scope<F: Future>(tags: Arc<TagSet>, future: F) -> impl Future<Output = F::Output> {
    CURRENT.scope(RefCell::new(tags), future)
}

/// [`scope`] for synchronous code.
pub fn sync_scope<R>(tags: Arc<TagSet>, f: impl FnOnce() -> R) -> R {
    CURRENT.sync_scope(RefCell::new(tags), f)
}

/// Swaps the tags of the enclosing [`scope`] for `tags` if they differ.
pub fn refresh(tags: &Arc<TagSet>) {
    let _ = CURRENT.try_with(|current| {
        if !Arc::ptr_eq(&current.borrow(), tags) {
            *current.borrow_mut() = tags.clone();
        }
    });
}

/// Runs `f` over the tags in effect: the enclosing [`scope`]'s, else the
/// built-in ones.
#[track_caller]
pub fn with<R>(f: impl FnOnce(&TagSet) -> R) -> R {
    let at = std::panic::Location::caller();
    let mut f = Some(f);
    if let Ok(result) = CURRENT.try_with(|current| (f.take().expect("unused"))(&current.borrow())) {
        return result;
    }
    missing_scope(at);
    (f.take().expect("unused"))(&TagSet::vanilla())
}

/// The tags in effect, as [`with`] sees them.
#[track_caller]
pub fn current() -> Arc<TagSet> {
    let at = std::panic::Location::caller();
    CURRENT
        .try_with(|current| current.borrow().clone())
        .unwrap_or_else(|_| {
            missing_scope(at);
            TagSet::vanilla()
        })
}

/// Whether the built-in tag `key` holds `value` in the tags in effect.
#[inline]
#[track_caller]
pub fn is<T: Registry + PartialEq>(key: Key<T>, value: T) -> bool {
    let at = std::panic::Location::caller();
    CURRENT
        .try_with(|current| current.borrow().is(key, value))
        .unwrap_or_else(|_| {
            missing_scope(at);
            built_in().is(key, value)
        })
}

/// [`is`] for a tag of a data-driven registry.
#[track_caller]
pub fn has(key: Key<&'static str>, id: &str) -> bool {
    let at = std::panic::Location::caller();
    CURRENT
        .try_with(|current| current.borrow().has(key, id))
        .unwrap_or_else(|_| {
            missing_scope(at);
            built_in().has(key, id)
        })
}

fn missing_scope(at: &std::panic::Location<'static>) {
    if ALLOW_UNSCOPED.try_with(|_| ()).is_ok() {
        return;
    }
    if cfg!(debug_assertions) && STRICT.get() {
        panic!(
            "tag check at {at} ran outside a tags scope; capture the Arc<TagSet> and enter tags::scope or sync_scope"
        );
    }
    static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        log::warn!("tag check at {at} ran outside a tags scope, using built-in tags");
    }
}

thread_local! {
    static STRICT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Marks the calling thread as a server pool thread, where a tag check
/// outside a scope is a bug (debug builds panic). Other threads, such as
/// unit-test threads, fall back to the built-in tags.
pub fn require_scope_on_this_thread() {
    STRICT.set(true);
}

tokio::task_local! {
    static ALLOW_UNSCOPED: ();
}

/// Runs `f` where the built-in tags are wanted on purpose (no data packs).
pub fn built_in_scope<R>(f: impl FnOnce() -> R) -> R {
    ALLOW_UNSCOPED.sync_scope((), || sync_scope(TagSet::vanilla(), f))
}

fn built_in() -> &'static TagSet {
    static SET: OnceLock<Arc<TagSet>> = OnceLock::new();
    SET.get_or_init(TagSet::vanilla)
}

/// The built-in tag of `registry` named `name` as a typed key, for data
/// that names tags (`#minecraft:logs`) and checks them later.
pub fn key<T>(
    registry: &str,
    members: &'static [(&'static str, &'static [T])],
    name: &str,
) -> Option<Key<T>> {
    let registry = tuff_vanilla_data::tag::REGISTRIES
        .iter()
        .position(|r| *r == registry)?;
    let index = members.iter().position(|(tag, _)| *tag == name)?;
    let (name, members) = members[index];
    Some(Key {
        registry,
        index,
        name,
        members,
    })
}

/// The built-in tags with `(registry, tag, id)` added, as a pack adding
/// those members would load them (for tests).
pub fn vanilla_with(additions: &[(&str, &str, &str)]) -> Arc<TagSet> {
    let mut builders = vanilla_builders();
    for (registry, tag, id) in additions {
        builders
            .get_mut(*registry)
            .and_then(|tags| tags.get_mut(*tag))
            .unwrap_or_else(|| panic!("no built-in tag {registry} {tag}"))
            .push(TagEntry {
                id: (*id).to_owned(),
                tag: false,
                required: true,
            });
    }
    let (set, errors) = TagSet::resolve_all(&builders);
    assert!(errors.is_empty(), "{errors:?}");
    Arc::new(set)
}

/// Where code running outside any [`scope`] (a pool job, a pipeline
/// shard) reads the tags in effect: the newest data pack load, or with no
/// packs attached the enclosing scope's (else the built-in) tags. Jobs
/// take one snapshot when they are handed out and run under
/// [`sync_scope`], so a reload reaches the next job, never one halfway.
/// What a feed reads the tags in effect from.
pub trait HasTags {
    fn tag_set(&self) -> Arc<TagSet>;
}

type Source = Arc<dyn Fn() -> Arc<TagSet> + Send + Sync>;

#[derive(Clone, Default)]
pub struct TagFeed(Option<Source>);

impl TagFeed {
    pub fn new<T: HasTags + Send + Sync + 'static>(
        contents: tokio::sync::watch::Receiver<Arc<T>>,
    ) -> Self {
        Self(Some(Arc::new(move || contents.borrow().tag_set())))
    }

    pub fn current(&self) -> Arc<TagSet> {
        match &self.0 {
            Some(source) => source(),
            None => CURRENT
                .try_with(|current| current.borrow().clone())
                .unwrap_or_else(|_| TagSet::vanilla()),
        }
    }
}

#[derive(Default)]
struct BodyWriter(Vec<u8>);

impl BodyWriter {
    fn write_var_int(&mut self, value: i32) {
        let mut value = value as u32;
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            if value == 0 {
                self.0.push(byte);
                return;
            }
            self.0.push(byte | 0x80);
        }
    }

    fn write_string(&mut self, value: &str) {
        self.write_var_int(value.len() as i32);
        self.write_bytes(value.as_bytes());
    }

    fn write_bytes(&mut self, value: &[u8]) {
        self.0.extend_from_slice(value);
    }
}

fn write_tags(
    w: &mut BodyWriter,
    key: &str,
    tags: &BTreeMap<String, Vec<String>>,
    id_of: impl Fn(&str) -> Option<u32>,
) {
    w.write_string(key);
    w.write_var_int(tags.len() as i32);
    for (name, members) in tags {
        let ids: Vec<u32> = members.iter().filter_map(|id| id_of(id)).collect();
        w.write_string(name);
        w.write_var_int(ids.len() as i32);
        for id in ids {
            w.write_var_int(id as i32);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builders(files: &[(&str, &str)]) -> BTreeMap<String, Vec<TagEntry>> {
        files
            .iter()
            .map(|(id, json)| (id.to_string(), parse(json.as_bytes()).unwrap().1))
            .collect()
    }

    #[tuff_core::watchdog]
    #[test]
    fn nested_tags_expand_in_order_without_duplicates() {
        let tags = builders(&[
            (
                "a:outer",
                r##"{"values":["a:x","#a:inner","a:x",{"id":"a:gone","required":false}]}"##,
            ),
            ("a:inner", r#"{"values":["y","a:z"]}"#),
        ]);
        let (resolved, errors) = resolve(&tags, |id| id != "a:gone");
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(resolved["a:outer"], ["a:x", "minecraft:y", "a:z"]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn missing_required_references_and_cycles_drop_the_tag() {
        let tags = builders(&[
            ("a:broken", r#"{"values":["a:missing"]}"#),
            ("a:uses_broken", r##"{"values":["#a:broken"]}"##),
            (
                "a:optional",
                r##"{"values":[{"id":"#a:broken","required":false},"a:ok"]}"##,
            ),
            ("a:loop1", r##"{"values":["#a:loop2"]}"##),
            ("a:loop2", r##"{"values":["#a:loop1"]}"##),
        ]);
        let (resolved, errors) = resolve(&tags, |id| id == "a:ok");
        assert_eq!(resolved.keys().collect::<Vec<_>>(), ["a:optional"]);
        assert_eq!(resolved["a:optional"], ["a:ok"]);
        assert_eq!(errors.len(), 4, "{errors:?}");
    }

    #[tuff_core::watchdog]
    #[test]
    fn malformed_tag_files_are_errors() {
        assert!(parse(b"{}").is_err());
        assert!(parse(br#"{"values":[1]}"#).is_err());
        assert!(parse(br#"{"values":["Bad Id"]}"#).is_err());
        assert!(parse(br#"{"replace":"yes","values":[]}"#).is_err());
        assert_eq!(
            parse(br#"{"replace":true,"values":[]}"#).unwrap(),
            (true, vec![])
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn vanilla_tags_match_the_compiled_in_flattened_ones() {
        let set = TagSet::vanilla();
        for (tag, items) in tuff_vanilla_data::tag::item::ALL {
            assert_eq!(set.typed::<Item>(tag).as_deref(), Some(*items), "{tag}");
        }
        for (tag, blocks) in tuff_vanilla_data::tag::block::ALL {
            assert_eq!(set.typed::<Block>(tag).as_deref(), Some(*blocks), "{tag}");
        }
        for (tag, biomes) in tuff_vanilla_data::tag::worldgen::biome::ALL {
            assert_eq!(
                set.get("minecraft:worldgen/biome", tag),
                Some(&biomes.iter().map(|b| b.to_string()).collect::<Vec<_>>()[..])
            );
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn tag_directories_map_to_the_longest_registry() {
        assert_eq!(
            split_registry("item/logs"),
            Some(("minecraft:item", "logs"))
        );
        assert_eq!(
            split_registry("worldgen/biome/is_ocean"),
            Some(("minecraft:worldgen/biome", "is_ocean"))
        );
        assert_eq!(split_registry("nonsense/x"), None);
        assert_eq!(split_registry("item/"), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn built_in_keys_index_the_resolved_members() {
        let set = TagSet::vanilla();
        for (index, (name, members)) in tuff_vanilla_data::tag::block::ALL.iter().enumerate() {
            let key = Key {
                registry: tuff_vanilla_data::tag::block::key::LOGS.registry,
                index,
                name,
                members,
            };
            for block in <Block as Registry>::ALL.iter().copied() {
                assert_eq!(set.is(key, block), members.contains(&block), "{name}");
            }
        }
        for (name, members) in tuff_vanilla_data::tag::worldgen::biome::ALL {
            let index = tuff_vanilla_data::tag::worldgen::biome::ALL
                .iter()
                .position(|(n, _)| n == name)
                .unwrap();
            let key = Key {
                registry: tuff_vanilla_data::tag::worldgen::biome::key::IS_OCEAN.registry,
                index,
                name,
                members,
            };
            for biome in tuff_vanilla_data::data_registry::WORLDGEN_BIOME.entries {
                assert_eq!(set.has(key, biome), members.contains(biome), "{name}");
            }
        }
        assert_eq!(
            tuff_vanilla_data::tag::REGISTRIES[tuff_vanilla_data::tag::item::key::LOGS.registry],
            "minecraft:item"
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_pool_thread_outside_a_scope_fails_and_a_scoped_one_sees_the_pack() {
        use tuff_vanilla_data::tag::block::key::LOGS;
        let pack = vanilla_with(&[(
            "minecraft:block",
            "minecraft:logs_that_burn",
            "minecraft:stone",
        )]);
        let unscoped = std::thread::spawn(|| {
            require_scope_on_this_thread();
            is(LOGS, Block::Stone)
        })
        .join();
        assert_eq!(unscoped.is_err(), cfg!(debug_assertions));
        assert!(
            !std::thread::spawn(|| is(LOGS, Block::Stone))
                .join()
                .unwrap()
        );
        let scoped = std::thread::spawn(move || sync_scope(pack, || is(LOGS, Block::Stone)));
        assert!(scoped.join().unwrap());
        let built_in = std::thread::spawn(|| built_in_scope(|| is(LOGS, Block::Stone)));
        assert!(!built_in.join().unwrap());
    }

    #[tuff_core::watchdog]
    #[test]
    fn checks_read_the_scoped_set_else_the_built_in_one() {
        use tuff_vanilla_data::tag::block::key::LOGS;
        let mut builders = vanilla_builders();
        builders
            .get_mut("minecraft:block")
            .unwrap()
            .get_mut("minecraft:logs_that_burn")
            .unwrap()
            .push(TagEntry {
                id: "minecraft:stone".into(),
                tag: false,
                required: true,
            });
        let pack = Arc::new(TagSet::resolve_all(&builders).0);
        assert!(!is(LOGS, Block::Stone));
        assert!(sync_scope(pack.clone(), || is(LOGS, Block::Stone)));
        assert!(sync_scope(TagSet::vanilla(), || {
            let before = is(LOGS, Block::Stone);
            refresh(&pack);
            (!before, is(LOGS, Block::Stone), current().fingerprint())
                == (true, true, pack.fingerprint())
        }));
        assert!(!is(LOGS, Block::Stone));
        assert!(with(|set| set.contains(
            "minecraft:block",
            "minecraft:logs",
            "minecraft:oak_log"
        )));
    }

    #[tuff_core::watchdog]
    #[test]
    fn an_added_member_reaches_tags_that_nest_it() {
        let mut builders = vanilla_builders();
        builders
            .get_mut("minecraft:item")
            .unwrap()
            .get_mut("minecraft:logs_that_burn")
            .unwrap()
            .push(
                parse(br#"{"values":["minecraft:stone"]}"#)
                    .unwrap()
                    .1
                    .remove(0),
            );
        let (set, errors) = TagSet::resolve_all(&builders);
        assert!(errors.is_empty(), "{errors:?}");
        assert!(
            set.typed::<Item>("minecraft:logs")
                .unwrap()
                .contains(&Item::Stone)
        );
        assert_ne!(set.fingerprint(), TagSet::vanilla().fingerprint());
    }

    #[tuff_core::watchdog]
    #[test]
    fn state_flags_follow_the_tags_of_their_own_set() {
        use tuff_vanilla_data::BlockState;
        use tuff_vanilla_data::tag::block::key;

        let check = |set: &TagSet| {
            let table = set.state_flags();
            assert_eq!(table.len(), BlockState::COUNT);
            for id in 0..BlockState::COUNT as u32 {
                let state = BlockState::from_id(id).unwrap();
                let block = state.block();
                let mut expected = state.flags();
                if set.is(key::BLOCKS_MOTION_IN_HEIGHTMAP, block) {
                    expected |= FLAG_BLOCKS_MOTION;
                }
                if set.is(key::BLOCKS_MOTION_IN_HEIGHTMAP_NO_LEAVES, block) {
                    expected |= FLAG_BLOCKS_MOTION_NO_LEAVES;
                }
                assert_eq!(table[id as usize], expected, "{state:?}");
            }
        };
        let vanilla = TagSet::vanilla();
        check(&vanilla);
        assert_ne!(
            vanilla.state_flags()[Block::Stone.default_state().id() as usize] & FLAG_BLOCKS_MOTION,
            0
        );

        let mut builders = vanilla_builders();
        builders
            .get_mut("minecraft:block")
            .unwrap()
            .get_mut("minecraft:blocks_motion_in_heightmap")
            .map(|entries| {
                entries.clear();
                entries.push(TagEntry {
                    id: "minecraft:dirt".into(),
                    tag: false,
                    required: true,
                });
            })
            .expect("tag exists");
        let pack = TagSet::resolve_all(&builders).0;
        check(&pack);
        let table = pack.state_flags();
        assert_eq!(
            table[Block::Stone.default_state().id() as usize] & FLAG_BLOCKS_MOTION,
            0
        );
        assert_ne!(
            table[Block::Dirt.default_state().id() as usize] & FLAG_BLOCKS_MOTION,
            0
        );
    }
}
