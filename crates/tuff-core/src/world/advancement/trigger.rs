//! Advancement triggers: what happened to a player ([`Event`]) and, per
//! trigger id, how a criterion's conditions are tested against it. The
//! registry is keyed by trigger id so a feature can add its own trigger
//! (firing [`Event::Custom`]) next to the vanilla ones.

use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;
use tuff_vanilla_data::registry::TriggerType;
use tuff_vanilla_data::{BlockState, Item};

use crate::world::predicate::{self, Context, DamageFacts, EntityFacts, LocationFacts};
use tuff_item::item::ItemStack;

/// Something a player did or had happen that criteria may count.
#[derive(Debug, Clone, Copy)]
pub enum Event<'a> {
    /// A slot of the player's inventory changed to `changed` (possibly
    /// empty). `inventory` is every inventory slot, equipment included.
    InventoryChanged {
        changed: Option<&'a ItemStack>,
        inventory: &'a [Option<tuff_item::item::ItemView<'a>>],
    },
    RecipeUnlocked(&'a str),
    RecipeCrafted {
        recipe: &'a str,
        ingredients: &'a [ItemStack],
    },
    ConsumeItem(&'a ItemStack),
    /// A block placed by the player, the item it came from.
    PlacedBlock {
        location: &'a LocationFacts,
        item: &'a ItemStack,
    },
    ItemUsedOnBlock {
        location: &'a LocationFacts,
        item: &'a ItemStack,
    },
    PlayerKilledEntity {
        victim: &'a EntityFacts,
        killing_blow: &'a DamageFacts,
    },
    EntityKilledPlayer {
        killer: &'a EntityFacts,
        killing_blow: &'a DamageFacts,
    },
    BredAnimals {
        parent: &'a EntityFacts,
        partner: &'a EntityFacts,
        child: Option<&'a EntityFacts>,
    },
    ChangedDimension {
        from: &'a str,
        to: &'a str,
    },
    EnterBlock(BlockState),
    /// Once a second: where the player is.
    Location,
    SleptInBed,
    Tick,
    /// The player set off a raid with a Raid Omen.
    RaidOmen,
    /// The player helped win a raid.
    RaidWin,
    EnchantedItem {
        item: &'a ItemStack,
        levels: i32,
    },
    BrewedPotion(Option<&'a str>),
    FilledBucket(&'a ItemStack),
    VillagerTrade {
        villager: &'a EntityFacts,
        item: &'a ItemStack,
    },
    /// An eye of ender thrown `distance_sqr` (horizontal) from its target.
    UsedEnderEye {
        distance_sqr: f64,
    },
    CuredZombieVillager {
        zombie: &'a EntityFacts,
        villager: &'a EntityFacts,
    },
    PlayerGeneratesContainerLoot(&'a str),
    ShotCrossbow(&'a ItemStack),
    TameAnimal(&'a EntityFacts),
    /// A bobber reeled in `items` (none when it held an entity).
    FishingRodHooked {
        rod: &'a ItemStack,
        entity: &'a EntityFacts,
        items: &'a [tuff_item::item::ItemSnapshot],
    },
    SummonedEntity(&'a EntityFacts),
    UsedTotem(&'a ItemStack),
    PlayerInteractedWithEntity {
        item: Option<&'a ItemStack>,
        entity: &'a EntityFacts,
    },
    PlayerShearedEquipment {
        item: &'a ItemStack,
        entity: &'a EntityFacts,
    },
    ItemDurabilityChanged {
        item: &'a ItemStack,
        delta: i32,
    },
    EntityHurtPlayer {
        damage: &'a DamageFacts,
        dealt: f32,
        taken: f32,
        blocked: bool,
    },
    PlayerHurtEntity {
        entity: &'a EntityFacts,
        damage: &'a DamageFacts,
        dealt: f32,
        taken: f32,
        blocked: bool,
    },
    /// Back in the overworld from the nether: where the player entered it.
    NetherTravel {
        entered: [f64; 3],
    },
    StartedRiding,
    EffectsChanged {
        source: Option<&'a EntityFacts>,
    },
    /// A lightning strike landed near the player.
    LightningStrike {
        lightning: &'a EntityFacts,
        bystanders: &'a [EntityFacts],
    },
    /// A trident's lightning struck these entities.
    ChanneledLightning {
        victims: &'a [EntityFacts],
    },
    /// Arrows from `weapon` killed these entities.
    KilledByArrow {
        victims: &'a [EntityFacts],
        weapon: Option<&'a ItemStack>,
    },
    /// Each tick while levitating: where the levitation began and how
    /// many ticks ago.
    Levitation {
        start: [f64; 3],
        duration: i32,
    },
    /// A fall ended; it began at `start`.
    FallFromHeight {
        start: [f64; 3],
    },
    /// Each tick an item is being used.
    UsingItem(&'a ItemStack),
    /// A right click on a block did something, holding `item` after the
    /// use; `location` is the clicked block as it is now.
    AnyBlockUse {
        location: &'a LocationFacts,
        item: &'a ItemStack,
    },
    /// An empty-handed style use of a block (opening, toggling) that the
    /// held item did not take over.
    DefaultBlockUse {
        location: &'a LocationFacts,
    },
    /// A beacon nearby came on with this many pyramid levels.
    ConstructBeacon {
        level: i32,
    },
    /// The player's projectile struck a target block, which gave `signal`.
    TargetHit {
        projectile: &'a EntityFacts,
        signal: i32,
    },
    KillMobNearSculkCatalyst {
        victim: &'a EntityFacts,
        killing_blow: &'a DamageFacts,
    },
    /// Sneaking kept a sculk sensor or warden from hearing the player.
    AvoidVibration,
    /// A crafter near the player crafted `recipe`.
    CrafterRecipeCrafted {
        recipe: &'a str,
        ingredients: &'a [ItemStack],
    },
    /// The player broke a bee nest or hive holding `bees` bees.
    BeeNestDestroyed {
        block: BlockState,
        item: Option<&'a ItemStack>,
        bees: i32,
    },
    /// Thrown by an explosion at `start`, the player began to fall.
    FallAfterExplosion {
        start: [f64; 3],
        cause: Option<&'a EntityFacts>,
    },
    /// Each tick riding a vehicle in lava since it entered at `start`.
    RideEntityInLava {
        start: [f64; 3],
    },
    /// Sliding down the side of this block.
    SlideDownBlock(BlockState),
    /// An allay dropped `item` onto a block for the player.
    AllayDropItemOnBlock {
        location: &'a LocationFacts,
        item: &'a ItemStack,
    },
    /// `entity` picked up an item the player threw.
    ThrownItemPickedUpByEntity {
        item: &'a ItemStack,
        entity: &'a EntityFacts,
    },
    /// The player picked up an item `entity` threw.
    ThrownItemPickedUpByPlayer {
        item: &'a ItemStack,
        entity: &'a EntityFacts,
    },
    /// A spear charge hit `count` living entities.
    SpearMobs {
        count: u32,
    },
    /// A trigger registered by a feature, with whatever its conditions need.
    Custom {
        trigger: &'a str,
        data: &'a (dyn Any + Send + Sync),
    },
}

impl Event<'_> {
    /// The trigger id this event fires.
    pub fn trigger(&self) -> &str {
        let kind = match self {
            Self::InventoryChanged { .. } => TriggerType::InventoryChanged,
            Self::RecipeUnlocked(_) => TriggerType::RecipeUnlocked,
            Self::RecipeCrafted { .. } => TriggerType::RecipeCrafted,
            Self::ConsumeItem(_) => TriggerType::ConsumeItem,
            Self::PlacedBlock { .. } => TriggerType::PlacedBlock,
            Self::ItemUsedOnBlock { .. } => TriggerType::ItemUsedOnBlock,
            Self::PlayerKilledEntity { .. } => TriggerType::PlayerKilledEntity,
            Self::EntityKilledPlayer { .. } => TriggerType::EntityKilledPlayer,
            Self::BredAnimals { .. } => TriggerType::BredAnimals,
            Self::ChangedDimension { .. } => TriggerType::ChangedDimension,
            Self::EnterBlock(_) => TriggerType::EnterBlock,
            Self::Location => TriggerType::Location,
            Self::SleptInBed => TriggerType::SleptInBed,
            Self::Tick => TriggerType::Tick,
            Self::RaidOmen => TriggerType::VoluntaryExile,
            Self::RaidWin => TriggerType::HeroOfTheVillage,
            Self::EnchantedItem { .. } => TriggerType::EnchantedItem,
            Self::BrewedPotion(_) => TriggerType::BrewedPotion,
            Self::FilledBucket(_) => TriggerType::FilledBucket,
            Self::VillagerTrade { .. } => TriggerType::VillagerTrade,
            Self::UsedEnderEye { .. } => TriggerType::UsedEnderEye,
            Self::CuredZombieVillager { .. } => TriggerType::CuredZombieVillager,
            Self::PlayerGeneratesContainerLoot(_) => TriggerType::PlayerGeneratesContainerLoot,
            Self::ShotCrossbow(_) => TriggerType::ShotCrossbow,
            Self::TameAnimal(_) => TriggerType::TameAnimal,
            Self::FishingRodHooked { .. } => TriggerType::FishingRodHooked,
            Self::SummonedEntity(_) => TriggerType::SummonedEntity,
            Self::UsedTotem(_) => TriggerType::UsedTotem,
            Self::PlayerInteractedWithEntity { .. } => TriggerType::PlayerInteractedWithEntity,
            Self::PlayerShearedEquipment { .. } => TriggerType::PlayerShearedEquipment,
            Self::ItemDurabilityChanged { .. } => TriggerType::ItemDurabilityChanged,
            Self::EntityHurtPlayer { .. } => TriggerType::EntityHurtPlayer,
            Self::PlayerHurtEntity { .. } => TriggerType::PlayerHurtEntity,
            Self::NetherTravel { .. } => TriggerType::NetherTravel,
            Self::StartedRiding => TriggerType::StartedRiding,
            Self::EffectsChanged { .. } => TriggerType::EffectsChanged,
            Self::LightningStrike { .. } => TriggerType::LightningStrike,
            Self::ChanneledLightning { .. } => TriggerType::ChanneledLightning,
            Self::KilledByArrow { .. } => TriggerType::KilledByArrow,
            Self::Levitation { .. } => TriggerType::Levitation,
            Self::FallFromHeight { .. } => TriggerType::FallFromHeight,
            Self::UsingItem(_) => TriggerType::UsingItem,
            Self::AnyBlockUse { .. } => TriggerType::AnyBlockUse,
            Self::DefaultBlockUse { .. } => TriggerType::DefaultBlockUse,
            Self::ConstructBeacon { .. } => TriggerType::ConstructBeacon,
            Self::TargetHit { .. } => TriggerType::TargetHit,
            Self::KillMobNearSculkCatalyst { .. } => TriggerType::KillMobNearSculkCatalyst,
            Self::AvoidVibration => TriggerType::AvoidVibration,
            Self::CrafterRecipeCrafted { .. } => TriggerType::CrafterRecipeCrafted,
            Self::BeeNestDestroyed { .. } => TriggerType::BeeNestDestroyed,
            Self::FallAfterExplosion { .. } => TriggerType::FallAfterExplosion,
            Self::RideEntityInLava { .. } => TriggerType::RideEntityInLava,
            Self::SlideDownBlock(_) => TriggerType::SlideDownBlock,
            Self::AllayDropItemOnBlock { .. } => TriggerType::AllayDropItemOnBlock,
            Self::ThrownItemPickedUpByEntity { .. } => TriggerType::ThrownItemPickedUpByEntity,
            Self::ThrownItemPickedUpByPlayer { .. } => TriggerType::ThrownItemPickedUpByPlayer,
            Self::SpearMobs { .. } => TriggerType::SpearMobs,
            Self::Custom { trigger, .. } => return trigger,
        };
        kind.name()
    }

    /// The item an inventory change is about, for indexing criteria.
    pub fn changed_item(&self) -> Option<Item> {
        match self {
            Self::InventoryChanged { changed, .. } => changed.map(|stack| stack.item),
            _ => None,
        }
    }
}

/// Tests one criterion's conditions (`Null` when it has none) against an
/// event, given the player it happened to. The generic `player` condition
/// is tested before this is called.
pub type Handler = Arc<dyn Fn(&Value, &Event<'_>, &EntityFacts) -> bool + Send + Sync>;

/// Trigger ids to their handlers.
pub struct Registry {
    handlers: HashMap<String, Handler>,
}

impl Registry {
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
        }
    }

    pub fn register(&mut self, trigger: impl Into<String>, handler: Handler) {
        self.handlers.insert(trigger.into(), handler);
    }

    pub fn get(&self, trigger: &str) -> Option<&Handler> {
        self.handlers.get(trigger)
    }

    /// Whether `conditions` match `event` for `player`.
    pub fn matches(
        &self,
        trigger: &str,
        conditions: &Value,
        event: &Event<'_>,
        player: &EntityFacts,
    ) -> bool {
        let Some(handler) = self.handlers.get(trigger) else {
            return false;
        };
        if let Some(condition) = conditions.get("player") {
            let context = Context {
                this: Some(player),
                target: None,
                origin: player.position,
                location: None,
                tool: None,
                enchantment_level: None,
            };
            if !predicate::condition(condition, &context) {
                return false;
            }
        }
        handler(conditions, event, player)
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}
