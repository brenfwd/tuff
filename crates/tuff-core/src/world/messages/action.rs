use tuff_vanilla_data::{BlockState, Item};

use crate::world::placement;
use tuff_item::item::{Components, ItemStack};
use tuff_world_model::{BlockPos, Face};

use super::*;

/// The blocks a successful use on a block involved, as advancement
/// location conditions test them.
#[derive(Debug, Clone, PartialEq)]
pub struct Placement {
    /// The clicked block, after the use.
    pub used_on: crate::world::predicate::LocationFacts,
    /// The block the item placed, if it placed one.
    pub placed: Option<crate::world::predicate::LocationFacts>,
}

#[derive(Debug)]
pub enum ActionOutcome {
    Broken {
        state: BlockState,
        /// Durability the held tool loses for this block, before Unbreaking
        /// and creative exemptions, which the player applies.
        tool_damage: u32,
    },
    /// Mining started and the block needs more than one tick.
    Mining,
    Placed {
        off_hand: bool,
        item: Item,
        /// Where it was used, for advancements.
        placement: Option<Box<Placement>>,
    },
    /// The player ate from a block (cake).
    Ate { nutrition: i32, saturation: f32 },
    /// The block reacted to the click (a door, a lever, ...) without
    /// consuming the held item.
    Interacted { off_hand: bool },
    /// The held `item` was used on a block (a tool, honeycomb, a bucket)
    /// and costs the hand `effect`.
    Used {
        off_hand: bool,
        item: Item,
        effect: HandEffect,
    },
    /// The held `item` starts being used on the clicked block for `ticks`
    /// (a brush).
    StartUsing {
        off_hand: bool,
        item: Item,
        ticks: u32,
    },
    /// Items moved between the hotbar and a block: each `(hotbar index, new
    /// stack)` is set. A creative player's hand keeps its stack when
    /// `single` and the slot was emptied.
    HotbarSwapped {
        slots: Vec<(usize, Option<ItemStack>)>,
        single: bool,
    },
    /// Resend these block states so the client undoes its prediction.
    Rejected(Packets),
}

/// What using an item does to the stack in hand. Creative players keep
/// their stacks except where noted.
#[derive(Debug, PartialEq)]
pub enum HandEffect {
    None,
    Consume(u8),
    /// Durability loss, before Unbreaking.
    Damage(u32),
    /// The stack becomes `Item` (an emptied bucket); creative players keep
    /// theirs.
    Emptied(Item),
    /// One item becomes `Item` (a bucket filling): the hand
    /// holds it if the stack is used up, otherwise it goes to the
    /// inventory. Creative players keep the stack and gain the result if
    /// they have none.
    Exchange(Item),
    /// Like `Exchange`, but the result carries components (a fish or
    /// axolotl bucket with its captured entity's data).
    ExchangeStack(Box<ItemStack>),
    /// The hand's stack becomes exactly this (an armor stand swapping
    /// what it holds with the player).
    Replace(Option<Box<ItemStack>>),
    /// One of the hand's items is turned into this stack (a compass locking
    /// onto a lodestone): a lone item in a survival hand is replaced, else
    /// the result goes to the inventory (or the ground) and creative players
    /// keep the hand's stack.
    Split(Box<ItemStack>),
}

#[derive(Debug)]
pub struct Attack {
    pub attacker: PlayerHandle,
    pub attacker_name: String,
    pub attacker_uuid: u128,
    /// The attacker's main-hand stack: its enchantments act on the victim,
    /// it may disable a shield, and it is reported back if the hit lands.
    pub weapon: Option<ItemStack>,
    pub attacker_position: [f64; 3],
    pub attacker_yaw: f32,
    pub damage: f32,
    /// Knockback along the attacker's facing on top of the usual push.
    pub knockback: f32,
    pub kind: AttackKind,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AttackKind {
    /// The player the attacker aimed at.
    Direct {
        critical: bool,
        /// Enchantments added damage.
        magic: bool,
        full_strength: bool,
        sweep: Option<Sweep>,
        /// A mace's smash attack: the damage type is `mace_smash`, not
        /// `player_attack`, and the attacker's fall distance (for Wind
        /// Burst's requirement).
        smash: Option<f64>,
    },
    /// A player caught by the sweep of an attack on someone else.
    Sweep,
    /// A spear's extra knockback without damage: only the push lands.
    Knockback,
}

/// Using a bucket: the region casts the player's view ray itself, as the
/// game does, since the client only reports the click.
#[derive(Debug, Clone)]
pub struct BucketUse {
    pub player: PlayerHandle,
    /// Where the player stands, for the pickup sound.
    pub position: [f64; 3],
    pub eye: [f64; 3],
    pub look: [f64; 3],
    pub reach: f64,
    pub off_hand: bool,
    pub item: Item,
    /// The bucket's fluid block, or `None` for an empty bucket.
    pub fluid: Option<tuff_vanilla_data::Block>,
    /// The mob a fish or axolotl bucket places alongside the water, and
    /// its saved `minecraft:bucket_entity_data` (variant, age, ...), if any.
    pub mob: Option<tuff_vanilla_data::registry::EntityType>,
    pub mob_data: Option<tuff_nbt::Compound>,
    /// The bucket's components this server does not model, which the mob
    /// may take on (a sulfur cube's swallowed block).
    pub mob_components: Option<Box<tuff_nbt::Compound>>,
    pub sneaking: bool,
    /// The player is in creative mode (only creative players waterlog or
    /// drain barriers).
    pub creative: bool,
    pub sequence: i32,
    /// The ray's hit, once a region resolved it; a neighbour owning the
    /// target applies it.
    pub hit: Option<crate::world::raycast::Hit>,
}

/// One tick of a survival break, `ticks` after it started.
#[derive(Debug)]
pub struct DestroyTick {
    pub player: PlayerHandle,
    pub pos: BlockPos,
    pub event: i32,
    pub data: i32,
    pub ticks: u64,
    pub tool: Option<ItemStack>,
    pub miner: crate::world::player::Miner,
    pub eye: [f64; 3],
}

#[derive(Debug, Clone, Copy)]
pub enum BreakMode {
    /// Breaks without drops.
    Creative,
    /// A survival player started mining: breaks only if one tick of mining
    /// is enough, otherwise answers [`ActionOutcome::Mining`].
    Start {
        eye: [f64; 3],
        miner: crate::world::player::Miner,
    },
    /// A survival player finished mining.
    Finish,
    /// A client's stop-mining packet: like [`BreakMode::Finish`], but the
    /// block must have had at least 70% of its mining time `ticks` after
    /// the start.
    Verified {
        ticks: u64,
        eye: [f64; 3],
        miner: crate::world::player::Miner,
    },
}

#[derive(tuff_item::item::DupClone, Debug)]
pub struct Place {
    pub player: PlayerHandle,
    pub eye: [f64; 3],
    pub target: BlockPos,
    pub face: Face,
    /// Where on the target the player clicked, each axis in 0..=1.
    pub cursor: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub sneaking: bool,
    /// The held item, if it places a block.
    pub item: Option<Item>,
    /// The held item, whatever it is.
    pub held: Option<Item>,
    /// The held stack's components, for blocks placed with contents.
    pub components: Components,
    /// How many of the held item the hand holds.
    pub held_count: u8,
    pub off_hand: bool,
    /// What the off hand holds, whatever it is (a main-hand click on a
    /// respawn anchor leaves charging to glowstone there).
    pub off_hand_item: Option<Item>,
    pub sequence: i32,
    /// Whether the clicked block may react to the click first (not
    /// sneaking, as vanilla decides it).
    pub interact: bool,
    /// Whether the player can eat right now, for cake.
    pub can_eat: bool,
    pub name: String,
    pub uuid: u128,
    /// Where the player stands.
    pub position: [f64; 3],
    /// The off hand holds a shield, so a main-hand tool does not transform
    /// blocks (the player means to block).
    pub off_hand_blocks: bool,
    /// The nine hotbar stacks, for blocks that swap them.
    pub hotbar: Vec<Option<ItemStack>>,
    /// Which hotbar slot the main hand holds.
    pub selected: usize,
    /// Whether the player may change blocks (not in adventure or
    /// spectator mode).
    pub may_build: bool,
    /// A creative-mode operator, who may place blocks with operator-only
    /// block entity data.
    pub game_master: bool,
    /// Filled in by the target's region, which decides where the block
    /// goes; that may lie in a neighbouring region.
    pub context: Option<placement::Context>,
}
