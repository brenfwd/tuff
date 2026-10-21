use tuff_item::item::ItemSnapshot;

/// What a region or another player saw happen that a player's advancement
/// triggers count, with the facts their conditions test.
#[derive(Debug)]
pub enum AdvancementEvent {
    /// Two animals the player fed bred.
    BredAnimals {
        parent: crate::world::predicate::EntityFacts,
        partner: crate::world::predicate::EntityFacts,
        child: Option<crate::world::predicate::EntityFacts>,
    },
    /// A zombie villager the player cured turned back into a villager.
    CuredZombieVillager {
        zombie: crate::world::predicate::EntityFacts,
        villager: crate::world::predicate::EntityFacts,
    },
    /// A potion the player put in a brewing stand was taken out brewed.
    BrewedPotion(Option<&'static str>),
    /// A lightning strike landed near the player.
    LightningStrike {
        lightning: crate::world::predicate::EntityFacts,
        bystanders: Vec<crate::world::predicate::EntityFacts>,
    },
    /// An entity was summoned near the player (a golem or wither built
    /// from blocks).
    SummonedEntity(crate::world::predicate::EntityFacts),
    /// The player opened a container that rolled this loot table.
    GeneratedContainerLoot(String),
    /// The player tamed this animal.
    TameAnimal(crate::world::predicate::EntityFacts),
    /// The player's right click on this entity did something, holding
    /// `item` as it was before the use.
    InteractedWithEntity {
        item: Option<ItemSnapshot>,
        entity: crate::world::predicate::EntityFacts,
    },
    /// The player reeled in a catch (or a hooked entity, with no items).
    FishingRodHooked {
        entity: crate::world::predicate::EntityFacts,
        items: Vec<tuff_item::item::ItemSnapshot>,
    },
    /// The player opened a container whose loot table `0` just rolled.
    GeneratedLoot(String),
    /// The player's Raid Omen set off (or strengthened) a raid.
    RaidOmen,
    /// A raid the player fought in was won.
    RaidWin,
    /// The player's Channeling trident called lightning down on these.
    ChanneledLightning {
        victims: Vec<crate::world::predicate::EntityFacts>,
    },
    /// The player's melee hit, sweep or projectile hurt this entity.
    HurtEntity {
        victim: crate::world::predicate::EntityFacts,
        damage: crate::world::predicate::DamageFacts,
        weapon: Option<ItemSnapshot>,
        dealt: f32,
        taken: f32,
    },
    /// The player's right click on the block at `location` (as it is
    /// after the use) did something; `default` when the block reacted on
    /// its own rather than to the held item.
    UsedBlock {
        location: crate::world::predicate::LocationFacts,
        off_hand: bool,
        default: bool,
    },
    /// A beacon near the player came on with this many levels.
    ConstructBeacon(i32),
    /// The player's projectile struck a target block, giving `signal`.
    TargetHit {
        projectile: crate::world::predicate::EntityFacts,
        signal: i32,
    },
    /// A mob the player killed fed a sculk catalyst.
    KillMobNearSculkCatalyst(Kill),
    /// Sneaking kept a listener from hearing the player.
    AvoidVibration,
    /// A crafter near the player crafted `recipe` from `ingredients`.
    CrafterRecipeCrafted {
        recipe: tuff_vanilla_data::Frozen<str>,
        ingredients: Vec<tuff_item::item::ItemStack>,
    },
    /// The player broke a bee nest or hive holding `bees` bees with `item`.
    BeeNestDestroyed {
        block: tuff_vanilla_data::BlockState,
        item: Option<ItemSnapshot>,
        bees: i32,
    },
    /// An allay the player likes dropped `item` onto the block at `location`;
    /// the region fills the location in from its position.
    AllayDropItemOnBlock {
        item: ItemSnapshot,
        location: crate::world::predicate::LocationFacts,
    },
    /// `entity` picked up `item`, which the player threw.
    ThrownItemPickedUpByEntity {
        item: ItemSnapshot,
        entity: crate::world::predicate::EntityFacts,
    },
    /// The player picked up `item`, which `thrower` threw.
    ThrownItemPickedUpByPlayer {
        item: ItemSnapshot,
        thrower: crate::world::predicate::EntityFacts,
    },
    /// The player sheared `item` off `entity`.
    ShearedEquipment {
        item: ItemSnapshot,
        entity: crate::world::predicate::EntityFacts,
    },
}

/// A kill credited to a player: statistics count `kind`, advancements
/// test the victim and the killing blow.
#[derive(Clone, Debug)]
pub struct Kill {
    pub kind: tuff_vanilla_data::registry::EntityType,
    /// The victim's scoreboard name: a player's name, else its UUID.
    pub holder: String,
    pub victim: crate::world::predicate::EntityFacts,
    pub killing_blow: crate::world::predicate::DamageFacts,
}
