//! The generic registration primitives every domain registry (commands,
//! chunk generators, growth behaviors, species, biomes, ...) is built
//! from. `tuff_core::registry::Registries` is the bag of them a feature
//! registers into.
//!
//! Two shapes cover everything a feature registers:
//!
//! - [`Registry<T>`]: a name-keyed table with many entries (commands,
//!   biomes, loot tables, species). Duplicate keys are a conflict
//!   unless the caller uses `replace`; `freeze` fixes iteration order
//!   (this is what defines a synchronized registry's network ids) and
//!   stops accepting further writes.
//! - [`HookSlot<T>`]: exactly one entry, no key. Used where at most one
//!   feature should ever own a piece of behavior (the session's attack
//!   resolution, item-use dispatch, ...) -- see
//!   `tuff_core::world::session::hooks`. A second `fill` without `replace`
//!   is the same conflict error `Registry::register` gives.
//!
//! Most of the domain registries a feature actually touches
//! (`Registry<Species>`, `Registry<Biome>`, ...) are owned by whichever
//! crate defines their value type, not by [`Registries`] here -- see
//! [`AnyRegistries`]. `Registries` only names the registries core's
//! own mechanism must read directly.

use std::any::{Any, TypeId};
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Arc;

use crate::load::{LoadError, LoadErrorKind};

type FrozenEntries<T> = Arc<[(String, Arc<T>)]>;

/// A name-keyed registry of plugin-contributed entries of type `T`.
/// Entries are handed out as `Arc<T>` so a frozen snapshot can be
/// cloned cheaply and shared across the actor system the way today's
/// baked `&'static` tables are.
pub struct Registry<T> {
    name: &'static str,
    entries: BTreeMap<String, Entry<T>>,
    frozen: Option<FrozenEntries<T>>,
}

struct Entry<T> {
    value: Arc<T>,
    registered_by: &'static str,
}

impl<T> Registry<T> {
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            entries: BTreeMap::new(),
            frozen: None,
        }
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Register a new entry. Errors if `key` is already taken -- use
    /// [`replace`](Self::replace) for an explicit override -- or if
    /// this registry is already frozen (registration only happens
    /// during `Types`/`Content`/`Resolve`, never after).
    pub fn register(
        &mut self,
        feature: &'static str,
        key: impl Into<String>,
        value: T,
    ) -> Result<(), LoadError> {
        if self.frozen.is_some() {
            return Err(LoadError {
                feature,
                kind: LoadErrorKind::Other(format!(
                    "{} is frozen; registration is closed",
                    self.name
                )),
            });
        }
        let key = key.into();
        if let Some(existing) = self.entries.get(&key) {
            return Err(LoadError {
                feature,
                kind: LoadErrorKind::Conflict {
                    registry: self.name,
                    key,
                    first_registered_by: existing.registered_by,
                },
            });
        }
        self.entries.insert(
            key,
            Entry {
                value: Arc::new(value),
                registered_by: feature,
            },
        );
        Ok(())
    }

    /// Explicit override: replaces whatever is registered at `key`, or
    /// inserts it fresh if nothing was. Never conflicts. This is the
    /// only sanctioned way to shadow another feature's (or vanilla's)
    /// entry; shadowing through `register` is a startup error instead.
    pub fn replace(
        &mut self,
        feature: &'static str,
        key: impl Into<String>,
        value: T,
    ) -> Result<(), LoadError> {
        if self.frozen.is_some() {
            return Err(LoadError {
                feature,
                kind: LoadErrorKind::Other(format!(
                    "{} is frozen; registration is closed",
                    self.name
                )),
            });
        }
        self.entries.insert(
            key.into(),
            Entry {
                value: Arc::new(value),
                registered_by: feature,
            },
        );
        Ok(())
    }

    /// A `Resolve`-phase read of an already-registered entry. Returns
    /// `None` rather than erroring: only the caller knows how to
    /// describe where its own reference came from, so it is the
    /// caller's job to turn a `None` into a
    /// `LoadErrorKind::DanglingReference`.
    /// The feature that registered `key`.
    pub fn registered_by(&self, key: &str) -> Option<&'static str> {
        self.entries.get(key).map(|e| e.registered_by)
    }

    pub fn get(&self, key: &str) -> Option<&Arc<T>> {
        self.entries.get(key).map(|e| &e.value)
    }

    pub fn contains(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// `Freeze`-phase: fix iteration order (sorted by key -- for a
    /// synchronized registry, this order *is* the network id
    /// assignment) and stop accepting further registrations.
    /// Idempotent; safe to call from multiple places during freeze.
    pub fn freeze(&mut self) -> Arc<[(String, Arc<T>)]> {
        if let Some(f) = &self.frozen {
            return f.clone();
        }
        let snapshot: Arc<[(String, Arc<T>)]> = self
            .entries
            .iter()
            .map(|(k, e)| (k.clone(), e.value.clone()))
            .collect();
        self.frozen = Some(snapshot.clone());
        snapshot
    }

    pub fn is_frozen(&self) -> bool {
        self.frozen.is_some()
    }

    /// Every entry by key, in key order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Arc<T>)> {
        self.entries.iter().map(|(k, e)| (k.as_str(), &e.value))
    }
}

/// Exactly one entry, no key -- for behavior only one feature should
/// ever own (the session's attack resolution, item-use dispatch, ...).
/// See `tuff_core::world::session::hooks`.
pub struct HookSlot<T: ?Sized> {
    name: &'static str,
    value: Option<Arc<T>>,
    registered_by: Option<&'static str>,
    frozen: bool,
}

impl<T: ?Sized> HookSlot<T> {
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            value: None,
            registered_by: None,
            frozen: false,
        }
    }

    pub fn fill(&mut self, feature: &'static str, value: Arc<T>) -> Result<(), LoadError> {
        if self.frozen {
            return Err(LoadError {
                feature,
                kind: LoadErrorKind::Other(format!(
                    "hook slot {} is frozen; registration is closed",
                    self.name
                )),
            });
        }
        if let Some(first) = self.registered_by {
            return Err(LoadError {
                feature,
                kind: LoadErrorKind::Conflict {
                    registry: self.name,
                    key: self.name.to_string(),
                    first_registered_by: first,
                },
            });
        }
        self.value = Some(value);
        self.registered_by = Some(feature);
        Ok(())
    }

    pub fn replace(&mut self, feature: &'static str, value: Arc<T>) -> Result<(), LoadError> {
        if self.frozen {
            return Err(LoadError {
                feature,
                kind: LoadErrorKind::Other(format!(
                    "hook slot {} is frozen; registration is closed",
                    self.name
                )),
            });
        }
        self.value = Some(value);
        self.registered_by = Some(feature);
        Ok(())
    }

    pub fn get(&self) -> Option<&Arc<T>> {
        self.value.as_ref()
    }

    pub fn freeze(&mut self) {
        self.frozen = true;
    }
}

// --- per-block tables ---

/// A value per block, looked up by block id: `Vec<Option<Arc<T>>>`
/// indexed by `Block`, one claimant per block.
pub struct BlockTable<T: ?Sized> {
    name: &'static str,
    slots: Vec<Option<(&'static str, Arc<T>)>>,
    frozen: bool,
}

impl<T: ?Sized> Clone for BlockTable<T> {
    fn clone(&self) -> Self {
        Self {
            name: self.name,
            slots: self.slots.clone(),
            frozen: self.frozen,
        }
    }
}

impl<T: ?Sized> BlockTable<T> {
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            slots: Vec::new(),
            frozen: false,
        }
    }

    pub fn register(
        &mut self,
        feature: &'static str,
        block: tuff_vanilla_data::Block,
        value: Arc<T>,
    ) -> Result<(), LoadError> {
        if self.frozen {
            return Err(LoadError {
                feature,
                kind: LoadErrorKind::Other(format!(
                    "block table {} is frozen; registration is closed",
                    self.name
                )),
            });
        }
        let index = block as usize;
        if self.slots.len() <= index {
            self.slots.resize(index + 1, None);
        }
        if let Some((first, _)) = &self.slots[index] {
            return Err(LoadError {
                feature,
                kind: LoadErrorKind::Conflict {
                    registry: self.name,
                    key: format!("{block:?}"),
                    first_registered_by: first,
                },
            });
        }
        self.slots[index] = Some((feature, value));
        Ok(())
    }

    pub fn get(&self, block: tuff_vanilla_data::Block) -> Option<&Arc<T>> {
        self.slots
            .get(block as usize)
            .and_then(Option::as_ref)
            .map(|(_, value)| value)
    }

    pub fn freeze(&mut self) {
        self.frozen = true;
    }
}

/// A `TypeId`-keyed bag of `Registry<T>` for any `T`, so a feature whose
/// registry value type is *not* one of core's own concrete types
/// (`Species` lives in `vanilla-mobs`, not core) still gets identical register/replace/freeze/conflict
/// semantics through [`Registry`], without core needing to name that
/// type. Core stays inert; this is what lets it stay generic instead
/// of accumulating a `use tuff_feature_builtin_vanilla_mobs::Species` it must never have.
///
/// A feature that wants an `ext` registry declares its own accessor
/// (e.g. `tuff_feature_builtin_vanilla_mobs::species_registry(registries: &mut Registries)
/// -> &mut Registry<Species>`) built on [`AnyRegistries::registry`];
/// core never needs to know it exists.
#[derive(Default)]
pub struct AnyRegistries {
    map: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
}

impl AnyRegistries {
    /// Get (creating on first use) the `Registry<T>` for this type.
    /// `name` is only used the first time; later calls ignore it (the
    /// registry already has the name it was created with).
    pub fn registry<T: 'static + Send + Sync>(&mut self, name: &'static str) -> &mut Registry<T> {
        self.map
            .entry(TypeId::of::<T>())
            .or_insert_with(|| Box::new(Registry::<T>::new(name)))
            .downcast_mut::<Registry<T>>()
            .expect("TypeId maps 1:1 to T; a collision here is a bug in AnyRegistries itself")
    }

    /// Same idea as [`registry`](Self::registry) for a single-owner
    /// hook slot instead of a many-entry registry.
    pub fn hook_slot<T: ?Sized + 'static + Send + Sync>(
        &mut self,
        name: &'static str,
    ) -> &mut HookSlot<T> {
        self.map
            .entry(TypeId::of::<HookSlot<T>>())
            .or_insert_with(|| Box::new(HookSlot::<T>::new(name)))
            .downcast_mut::<HookSlot<T>>()
            .expect("TypeId maps 1:1 to T; a collision here is a bug in AnyRegistries itself")
    }
}
