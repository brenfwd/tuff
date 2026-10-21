use std::collections::BTreeSet;

use serde_json::Value;
use tuff_core::world::advancement::Id;
use tuff_item::item::{Components, ItemStack};
use tuff_item::registries::codecs::json_to_nbt;
use tuff_item::text::TextComponent;
use tuff_vanilla_data::Frozen;
use tuff_vanilla_data::advancement::{self as data, Advancement};

pub use tuff_vanilla_data::advancement::Frame;

/// A criterion: its advancement and its index in that advancement's
/// criteria.
pub type Key = (Id, u16);

/// The most parents an advancement may have; deeper ones are not loaded.
pub const MAX_DEPTH: usize = 256;

/// What the advancement screen shows of an advancement, decoded once.
#[derive(Debug, Clone)]
pub struct Display {
    pub title: TextComponent,
    pub description: TextComponent,
    pub icon: ItemStack,
    pub frame: Frame,
    /// Roots only: the texture behind the tab.
    pub background: Option<Frozen<str>>,
    pub show_toast: bool,
    pub announce_to_chat: bool,
    pub hidden: bool,
}

#[derive(Debug, Clone)]
pub struct Criterion {
    pub name: Frozen<str>,
    /// A trigger id the tree's trigger registry knows.
    pub trigger: Frozen<str>,
    /// The trigger's conditions, `Null` when there are none.
    pub conditions: Value,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rewards {
    pub experience: i32,
    pub loot: Box<[Frozen<str>]>,
    pub recipes: Box<[Frozen<str>]>,
    pub function: Option<Frozen<str>>,
}

/// An advancement as its data defines it, apart from where it sits in a
/// tree. Shared between the trees that hold it.
#[derive(Debug)]
pub struct Entry {
    pub name: Frozen<str>,
    pub parent_name: Option<Frozen<str>>,
    pub display: Option<Display>,
    /// Sorted by name.
    pub criteria: Box<[Criterion]>,
    /// Groups that must all be met; a group is met by any one of its
    /// criteria.
    pub requirements: Box<[Box<[Frozen<str>]>]>,
    /// `requirements` as criterion indices.
    groups: Box<[Box<[u16]>]>,
    pub rewards: Rewards,
    pub sends_telemetry_event: bool,
}

impl Entry {
    /// An advancement with its criteria sorted by name. `requirements`
    /// default to one group per criterion; given ones must name exactly the
    /// criteria.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        name: Frozen<str>,
        parent_name: Option<Frozen<str>>,
        display: Option<Display>,
        mut criteria: Vec<Criterion>,
        requirements: Option<Vec<Vec<Frozen<str>>>>,
        rewards: Rewards,
        sends_telemetry_event: bool,
    ) -> Result<Self, String> {
        if criteria.is_empty() {
            return Err("Advancement criteria cannot be empty".to_owned());
        }
        criteria.sort_by(|a, b| a.name.cmp(&b.name));
        if criteria.windows(2).any(|pair| pair[0].name == pair[1].name) {
            return Err("Duplicate criterion".to_owned());
        }
        if criteria.len() > usize::from(u16::MAX) {
            return Err("Too many criteria".to_owned());
        }
        let requirements = match requirements {
            Some(requirements) => {
                let listed: BTreeSet<&str> = requirements.iter().flatten().map(|n| &**n).collect();
                let names: BTreeSet<&str> = criteria.iter().map(|c| &*c.name).collect();
                if listed != names {
                    let missing: Vec<&str> = names.difference(&listed).copied().collect();
                    let unknown: Vec<&str> = listed.difference(&names).copied().collect();
                    return Err(format!(
                        "Advancement completion requirements did not exactly match specified \
                         criteria. Missing: {missing:?}. Unknown: {unknown:?}"
                    ));
                }
                requirements
            }
            None => criteria.iter().map(|c| vec![c.name.clone()]).collect(),
        };
        let index_of = |name: &str| {
            criteria
                .binary_search_by(|c| (*c.name).cmp(name))
                .ok()
                .map(|i| i as u16)
        };
        let groups = requirements
            .iter()
            .map(|group| group.iter().filter_map(|name| index_of(name)).collect())
            .collect();
        Ok(Self {
            name,
            parent_name,
            display,
            criteria: criteria.into_boxed_slice(),
            requirements: requirements
                .into_iter()
                .map(Vec::into_boxed_slice)
                .collect(),
            groups,
            rewards,
            sends_telemetry_event,
        })
    }

    pub fn groups(&self) -> &[Box<[u16]>] {
        &self.groups
    }

    pub fn from_static(advancement: &'static Advancement, all: &'static [Advancement]) -> Self {
        let criteria = advancement
            .criteria
            .iter()
            .map(|c| Criterion {
                name: Frozen::Static(c.name),
                trigger: Frozen::Static(c.trigger),
                conditions: c
                    .conditions
                    .and_then(|json| serde_json::from_str(json).ok())
                    .unwrap_or(Value::Null),
            })
            .collect::<Box<[Criterion]>>();
        let requirements: Box<[Box<[Frozen<str>]>]> = advancement
            .requirements
            .iter()
            .map(|group| group.iter().map(|name| Frozen::Static(*name)).collect())
            .collect();
        let groups = advancement
            .requirements
            .iter()
            .map(|group| {
                group
                    .iter()
                    .filter_map(|name| {
                        criteria
                            .iter()
                            .position(|c| &*c.name == *name)
                            .map(|i| i as u16)
                    })
                    .collect()
            })
            .collect();
        let rewards = &advancement.rewards;
        Self {
            name: Frozen::Static(advancement.name),
            parent_name: advancement
                .parent
                .map(|parent| Frozen::Static(all[parent as usize].name)),
            display: advancement.display.as_ref().map(|display| Display {
                title: TextComponent::from_json_str(display.title)
                    .unwrap_or_else(|| TextComponent::new(advancement.name)),
                description: TextComponent::from_json_str(display.description).unwrap_or_default(),
                icon: icon_stack(&display.icon),
                frame: display.frame,
                background: display.background.map(Frozen::Static),
                show_toast: display.show_toast,
                announce_to_chat: display.announce_to_chat,
                hidden: display.hidden,
            }),
            criteria,
            requirements,
            groups,
            rewards: Rewards {
                experience: rewards.experience,
                loot: rewards.loot.iter().map(|t| Frozen::Static(*t)).collect(),
                recipes: rewards.recipes.iter().map(|r| Frozen::Static(*r)).collect(),
                function: rewards.function.map(Frozen::Static),
            },
            sends_telemetry_event: advancement.sends_telemetry_event,
        }
    }
}

fn icon_stack(icon: &data::Icon) -> ItemStack {
    let components = icon
        .components
        .and_then(|json| serde_json::from_str::<Value>(json).ok())
        .and_then(|value| json_to_nbt(&value))
        .and_then(|tag| tag.as_compound().map(Components::from_nbt))
        .unwrap_or_default();
    ItemStack::new(icon.item, icon.count).with_components(components)
}
