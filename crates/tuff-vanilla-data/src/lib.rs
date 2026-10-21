//! Vanilla game data for the Minecraft version this crate was built against,
//! generated at build time from the official server jar's data generator
//! reports and built-in data pack.
//!
//! # Raw JSON escape hatches
//!
//! Most of Minecraft's data is a finite, closed set of shapes, which this
//! crate turns into ordinary Rust types. A few corners are instead an
//! open-ended mini-language that mods, plugins, and future versions can
//! extend with new kinds at any time: item/block/entity predicates, a
//! command's permission requirement, an argument parser's properties, and a
//! biome's `attributes` map. For those, this crate types the surrounding
//! shape (which kind of thing this is) and keeps that kind's own parameters
//! as compact JSON text rather than exhaustively modeling every variant.
//! Parse it with [`serde_json`] if you need a particular one; each escape
//! hatch's doc comment says which fields it applies to.

pub mod advancement;
pub mod attribute;
pub mod biome;
pub mod block;
pub mod block_entity;
pub mod block_transformer;
pub mod command;
pub mod damage_type;
pub mod data_registry;
pub mod dimension_type;
pub mod easing;
pub mod enchantment;
pub mod entity;
pub mod fluid;
pub mod frozen;
pub mod game_rule;
pub mod growth;
pub mod item;
pub mod light;
pub mod loot_table;
pub mod map;
pub mod potion;
pub mod provider;
pub mod recipe;
pub mod redstone;
pub mod registry;
pub mod structure;
pub mod sulfur_cube;
pub mod timeline;
pub mod trial_spawner;
pub mod variant;
pub mod version;
pub mod villager;
pub mod world;
pub mod worldgen;

pub mod packet {
    //! Packet IDs for each connection state and direction.
    include!(concat!(env!("OUT_DIR"), "/packets.rs"));
}

pub mod dragon {
    //! The ender dragon's code-only tables: phase ids and the graph of
    //! nodes it flies between.
    include!(concat!(env!("OUT_DIR"), "/dragon.rs"));
}

pub mod tag {
    //! Tags from the built-in data pack with nested tags already expanded,
    //! grouped into modules that mirror the registry path.

    /// A built-in tag: its registry's position in [`REGISTRIES`], its own
    /// position in that registry's `ALL`, its id and its built-in members.
    #[derive(Debug)]
    pub struct Key<T: 'static> {
        pub registry: usize,
        pub index: usize,
        pub name: &'static str,
        pub members: &'static [T],
    }

    impl<T> Clone for Key<T> {
        fn clone(&self) -> Self {
            *self
        }
    }

    impl<T> Copy for Key<T> {}

    impl<T> PartialEq for Key<T> {
        fn eq(&self, other: &Self) -> bool {
            (self.registry, self.index) == (other.registry, other.index)
        }
    }

    include!(concat!(env!("OUT_DIR"), "/tags.rs"));
}

pub mod lang {
    //! The `en_us` client language file: translation key -> format string.
    include!(concat!(env!("OUT_DIR"), "/lang.rs"));
}

pub use block::{BlockKind, BlockState, ShapeOffset, Support};
pub use frozen::Frozen;
pub use item::{Tool, ToolRule};
pub use registry::{Block, Item, Registry};

/// Binary-searches a table of `(resource location, value)` pairs sorted by
/// location. A `name` without a namespace is looked up in `minecraft`.
fn lookup<T: Copy>(table: &[(&str, T)], name: &str) -> Option<T> {
    let (namespace, path) = name.split_once(':').unwrap_or(("minecraft", name));
    table
        .binary_search_by(|(key, _)| {
            let wanted = namespace.bytes().chain(*b":").chain(path.bytes());
            key.bytes().cmp(wanted)
        })
        .ok()
        .map(|index| table[index].1)
}
