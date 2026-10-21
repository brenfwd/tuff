//! The data pack's advancements (`advancement/`), sorted by name: the tree
//! (parent, root and children as indices into [`ALL`]), display info,
//! criteria, requirements and rewards, plus where the advancement screen
//! draws each displayed node.
//!
//! Criterion conditions are raw JSON: each trigger kind has its own
//! open-ended condition language, evaluated by whoever registers the
//! trigger.

use crate::registry::Item;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Advancement {
    pub name: &'static str,
    pub parent: Option<u16>,
    pub root: u16,
    /// Sorted by name.
    pub children: &'static [u16],
    pub display: Option<Display>,
    /// Sorted by name.
    pub criteria: &'static [Criterion],
    /// Groups that must all be met; a group is met by any one of its
    /// criteria.
    pub requirements: &'static [&'static [&'static str]],
    pub rewards: Rewards,
    pub sends_telemetry_event: bool,
    /// `(x, y)` on the advancement screen; zero for nodes not displayed.
    pub position: [f32; 2],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Display {
    pub icon: Icon,
    /// A text component as JSON.
    pub title: &'static str,
    /// A text component as JSON.
    pub description: &'static str,
    pub frame: Frame,
    /// Roots only: the texture behind the tab.
    pub background: Option<&'static str>,
    pub show_toast: bool,
    pub announce_to_chat: bool,
    pub hidden: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Icon {
    pub item: Item,
    pub count: u8,
    /// The stack's component patch as JSON.
    pub components: Option<&'static str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Frame {
    Task,
    Challenge,
    Goal,
}

impl Frame {
    /// The protocol id.
    pub fn id(self) -> u8 {
        match self {
            Self::Task => 0,
            Self::Challenge => 1,
            Self::Goal => 2,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::Challenge => "challenge",
            Self::Goal => "goal",
        }
    }

    /// The chat color of announcements and names.
    pub fn color(self) -> &'static str {
        match self {
            Self::Challenge => "dark_purple",
            Self::Task | Self::Goal => "green",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Criterion {
    pub name: &'static str,
    /// A `minecraft:trigger_type` id.
    pub trigger: &'static str,
    /// The trigger's conditions as JSON.
    pub conditions: Option<&'static str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rewards {
    pub experience: i32,
    pub loot: &'static [&'static str],
    pub recipes: &'static [&'static str],
    pub function: Option<&'static str>,
}

impl Rewards {
    pub const NONE: Self = Self {
        experience: 0,
        loot: &[],
        recipes: &[],
        function: None,
    };
}

impl Advancement {
    pub fn get(name: &str) -> Option<&'static Advancement> {
        index(name).map(|i| &ALL[i])
    }

    pub fn index(&self) -> usize {
        index(self.name).expect("every advancement is in ALL")
    }
}

/// The position of `name` in [`ALL`]; a name without a namespace is looked
/// up in `minecraft`.
pub fn index(name: &str) -> Option<usize> {
    let (namespace, path) = name.split_once(':').unwrap_or(("minecraft", name));
    ALL.binary_search_by(|advancement| {
        let wanted = namespace.bytes().chain(*b":").chain(path.bytes());
        advancement.name.bytes().cmp(wanted)
    })
    .ok()
}

include!(concat!(env!("OUT_DIR"), "/advancements.rs"));
