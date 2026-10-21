//! Villagers: professions, points of interest, types by biome, levels,
//! gossip kinds, and the data pack's trades and trade sets.

use crate::BlockState;
use crate::registry::{
    Block, EntityType, Item, PointOfInterestType, SoundEvent, VillagerProfession, VillagerType,
};

/// How many villagers may claim a point of interest, and how close
/// (in blocks) one must be to use it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoiInfo {
    pub max_tickets: u32,
    pub valid_range: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct Profession {
    /// The job site type a villager of this profession keeps.
    pub job_site: Option<PointOfInterestType>,
    /// The job site types a villager of this profession may claim.
    pub acquirable_job_sites: &'static [PointOfInterestType],
    pub work_sound: Option<SoundEvent>,
    /// The trade set unlocked at each level, 1 to 5.
    pub trade_sets: [Option<&'static TradeSet>; 5],
    /// Items it picks up besides food.
    pub requested_items: &'static [Item],
    /// Blocks it works at besides its job site (a farmer's farmland).
    pub secondary_blocks: &'static [Block],
}

/// What a trade asks for: an item, how many, and for potions which one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TradeCost {
    pub item: Item,
    pub count: i64,
    /// A number provider rolled for the count instead, as raw JSON.
    pub count_provider: Option<&'static str>,
    pub potion: Option<&'static str>,
}

/// A `minecraft:villager_trade` entry.
#[derive(Clone, Copy, Debug)]
pub struct Trade {
    pub name: &'static str,
    pub wants: TradeCost,
    pub additional_wants: Option<TradeCost>,
    pub gives: Item,
    pub gives_count: i64,
    pub max_uses: i64,
    pub xp: i64,
    pub reputation_discount: f32,
    /// The villager types that offer it; `None` for every type.
    pub merchant_types: Option<&'static [VillagerType]>,
    /// The loot item functions applied to the result, as raw JSON (one
    /// function or a list).
    pub modifier: Option<&'static str>,
    /// Stored enchantments that double the extra cost the modifier adds.
    pub double_price_enchantments: Option<&'static [&'static str]>,
    /// The enchantment tag those come from.
    pub double_price_tag: Option<&'static str>,
}

/// A `minecraft:trade_set` entry: how many offers to draw from which trades.
#[derive(Clone, Copy, Debug)]
pub struct TradeSet {
    pub name: &'static str,
    /// The `villager_trade` tag its trades come from, which data packs may
    /// change.
    pub tag: Option<&'static str>,
    /// In tag order, which is the order the draws index, as the built-in
    /// tag has them.
    pub trades: &'static [&'static Trade],
    pub amount: i64,
    pub allow_duplicates: bool,
    pub random_sequence: Option<&'static str>,
}

/// A kind of gossip villagers share about others.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GossipKind {
    pub name: &'static str,
    pub weight: i32,
    pub max: i32,
    pub decay_per_day: i32,
    pub decay_per_transfer: i32,
}

include!(concat!(env!("OUT_DIR"), "/villagers.rs"));

impl PointOfInterestType {
    pub fn info(self) -> PoiInfo {
        POI_INFO
            .iter()
            .find(|(kind, _)| *kind == self)
            .map(|(_, info)| *info)
            .expect("every point of interest type is dumped")
    }

    /// Every block state that makes a point of interest, by state id,
    /// sorted.
    pub fn states() -> &'static [(u32, Self)] {
        STATE_POIS
    }

    /// The point of interest a block state makes, if any.
    pub fn of_state(state: BlockState) -> Option<Self> {
        STATE_POIS
            .binary_search_by_key(&{ state.id() }, |(id, _)| *id)
            .ok()
            .map(|index| STATE_POIS[index].1)
    }
}

impl VillagerProfession {
    pub fn info(self) -> &'static Profession {
        PROFESSIONS
            .iter()
            .find(|(kind, _)| *kind == self)
            .map(|(_, info)| info)
            .expect("every profession is dumped")
    }
}

impl VillagerType {
    /// The type of villager born in `biome`.
    pub fn for_biome(biome: &str) -> Self {
        BIOME_TYPES
            .binary_search_by(|(name, _)| (*name).cmp(biome))
            .map_or(DEFAULT_TYPE, |index| BIOME_TYPES[index].1)
    }
}

/// How close a mob of type `kind` must come for villagers to panic, if
/// they fear it at all.
pub fn hostile_distance(kind: EntityType) -> Option<f32> {
    HOSTILE_DISTANCES
        .iter()
        .find(|(k, _)| *k == kind)
        .map(|(_, d)| *d)
}

impl GossipKind {
    pub fn get(name: &str) -> Option<&'static GossipKind> {
        GOSSIP_KINDS.iter().find(|kind| kind.name == name)
    }
}

impl Trade {
    pub fn get(name: &str) -> Option<&'static Trade> {
        crate::lookup(TRADES, name)
    }
}

impl TradeSet {
    pub fn get(name: &str) -> Option<&'static TradeSet> {
        crate::lookup(TRADE_SETS, name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_lightning_rod_is_a_lightning_rod_point() {
        for &rod in crate::tag::block::LIGHTNING_RODS {
            assert_eq!(
                PointOfInterestType::of_state(rod.default_state()),
                Some(PointOfInterestType::LightningRod),
                "{}",
                rod.name()
            );
        }
    }

    #[test]
    fn professions_hold_their_job_sites_and_trade_sets() {
        let farmer = VillagerProfession::Farmer.info();
        assert_eq!(farmer.job_site, Some(PointOfInterestType::Farmer));
        assert_eq!(farmer.acquirable_job_sites, &[PointOfInterestType::Farmer]);
        assert_eq!(
            farmer.work_sound,
            Some(SoundEvent::EntityVillagerWorkFarmer)
        );
        assert_eq!(
            farmer.trade_sets[0].unwrap().name,
            "minecraft:farmer/level_1"
        );
        assert!(farmer.requested_items.contains(&Item::WheatSeeds));
        assert_eq!(farmer.secondary_blocks, &[Block::Farmland]);
        let none = VillagerProfession::None.info();
        assert_eq!(none.job_site, None);
        assert!(
            none.acquirable_job_sites
                .contains(&PointOfInterestType::Librarian)
        );
        assert!(
            !none
                .acquirable_job_sites
                .contains(&PointOfInterestType::Home)
        );
        assert!(
            VillagerProfession::Nitwit
                .info()
                .acquirable_job_sites
                .is_empty()
        );
        assert!(
            VillagerProfession::Nitwit
                .info()
                .trade_sets
                .iter()
                .all(Option::is_none)
        );
    }

    #[test]
    fn points_of_interest_come_from_block_states() {
        assert_eq!(
            PointOfInterestType::of_state(Block::Composter.default_state()),
            Some(PointOfInterestType::Farmer)
        );
        assert_eq!(
            PointOfInterestType::of_state(Block::RedBed.default_state()),
            None,
            "the default bed state is the foot"
        );
        let head = Block::RedBed.default_state().with("part", "head").unwrap();
        assert_eq!(
            PointOfInterestType::of_state(head),
            Some(PointOfInterestType::Home)
        );
        assert_eq!(
            PointOfInterestType::of_state(Block::Stone.default_state()),
            None
        );
        assert_eq!(PointOfInterestType::Meeting.info().max_tickets, 32);
        assert_eq!(PointOfInterestType::Meeting.info().valid_range, 6);
    }

    #[test]
    fn trades_and_sets_come_from_the_data_pack() {
        let set = TradeSet::get("minecraft:farmer/level_1").unwrap();
        assert_eq!(set.amount, 2);
        assert!(!set.allow_duplicates);
        assert_eq!(set.trades[0].name, "minecraft:farmer/1/wheat_emerald");
        let wheat = Trade::get("minecraft:farmer/1/wheat_emerald").unwrap();
        assert_eq!(wheat.wants.item, Item::Wheat);
        assert_eq!(wheat.wants.count, 20);
        assert_eq!(wheat.gives, Item::Emerald);
        assert_eq!(wheat.max_uses, 16);
        assert_eq!(wheat.xp, 2);
        assert_eq!(wheat.reputation_discount, 0.05);
        let bottle = Trade::get("minecraft:wandering_trader/water_bottle_emerald").unwrap();
        assert_eq!(bottle.wants.potion, Some("minecraft:water"));
        let book = Trade::get("minecraft:librarian/1/emerald_and_book_enchanted_book").unwrap();
        assert!(book.modifier.is_some());
        assert!(book.double_price_enchantments.is_some());
        assert_eq!(book.additional_wants.unwrap().item, Item::Book);
    }

    #[test]
    fn villagers_follow_their_schedule() {
        use crate::registry::Activity;
        use crate::world::Environment;
        let overworld = Environment::of("minecraft:overworld");
        let at = |ticks: i64, baby: bool| overworld.villager_activity(|_| ticks, baby);
        assert_eq!(at(3000, false), Activity::Work);
        assert_eq!(at(9500, false), Activity::Meet);
        assert_eq!(at(13000, false), Activity::Rest);
        assert_eq!(at(24000 + 3000, false), Activity::Work);
        assert_eq!(
            at(5, false),
            Activity::Rest,
            "before the first keyframe of a day"
        );
        assert_eq!(at(3000, true), Activity::Play);
        let nether = Environment::of("minecraft:the_nether");
        assert_eq!(
            nether.villager_activity(|_| 3000, false),
            Activity::Work,
            "the schedule is universal"
        );
        assert_eq!(hostile_distance(EntityType::Zombie), Some(8.0));
        assert_eq!(hostile_distance(EntityType::Pillager), Some(15.0));
        assert_eq!(hostile_distance(EntityType::Skeleton), None);
    }

    #[test]
    fn villager_types_follow_biomes() {
        assert_eq!(
            VillagerType::for_biome("minecraft:desert"),
            VillagerType::Desert
        );
        assert_eq!(
            VillagerType::for_biome("minecraft:snowy_plains"),
            VillagerType::Snow
        );
        assert_eq!(
            VillagerType::for_biome("minecraft:forest"),
            VillagerType::Plains
        );
        assert_eq!(LEVEL_XP, [0, 10, 70, 150, 250]);
        assert_eq!(GossipKind::get("trading").unwrap().max, 25);
    }
}
