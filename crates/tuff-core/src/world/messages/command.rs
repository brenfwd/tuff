use tuff_vanilla_data::BlockState;

use tuff_item::text::TextComponent;
use tuff_nbt::Compound;
use tuff_world_model::BlockPos;

use super::*;

/// What `/tag` does to one entity's scoreboard tags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagEdit {
    Add(String),
    Remove(String),
}

/// What `/waypoint modify` does to a player's locator bar icon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaypointEdit {
    Color(Option<[u8; 3]>),
    Style(Option<String>),
}

/// What `/effect` does to a non-player entity.
#[derive(Debug, Clone)]
pub enum EffectCommand {
    Give(crate::world::effects::ActiveEffect),
    Clear(Option<tuff_vanilla_data::registry::MobEffect>),
}

/// What `/attribute` asks of one living entity about one attribute.
#[derive(Debug, Clone)]
pub struct AttributeCommand {
    pub attribute: tuff_vanilla_data::registry::Attribute,
    pub op: AttributeOp,
}

#[derive(Debug, Clone)]
pub enum AttributeOp {
    Value,
    Base,
    SetBase(f64),
    ResetBase,
    AddModifier {
        id: String,
        amount: f64,
        operation: tuff_vanilla_data::item::ModifierOperation,
    },
    RemoveModifier(String),
    ModifierValue(String),
}

/// The outcome of an [`AttributeCommand`]; the errors are the game's.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AttributeAnswer {
    Number(f64),
    Done,
    NotLiving,
    NoAttribute,
    NoModifier,
    DuplicateModifier,
}

/// A block read or write over a set of positions, applied once the
/// chunks are loaded.
#[derive(Debug)]
pub enum BlockOp {
    /// The biome names at `positions`; `None` where the chunk is not
    /// loaded and `load` is false.
    Biomes {
        positions: Vec<BlockPos>,
        load: bool,
    },
    /// The states at `positions`; `None` where the chunk is not loaded
    /// and `load` is false.
    Get {
        positions: Vec<BlockPos>,
        load: bool,
    },
    /// Sets each position in order. `destroy` breaks the old block with
    /// its drops first, `keep` only fills air, `strict` skips neighbour
    /// updates. `quiet` replaces block entities without their side effects
    /// (no spilling), and `data` is loaded into each new block entity.
    Set {
        changes: Vec<(BlockPos, BlockState)>,
        destroy: bool,
        keep: bool,
        strict: bool,
        quiet: bool,
        data: Option<Compound>,
    },
    /// Each position's state and saved block entity, loading chunks.
    Copy { positions: Vec<BlockPos> },
    /// Sets each position in order like `Set` but without any block
    /// entity side effects (replaced containers do not spill, jukeboxes do
    /// not pop their record), then gives each block that comes with saved
    /// block entity data that data, moved to its new position.
    Paste {
        changes: Vec<(BlockPos, BlockState, Option<Compound>)>,
        strict: bool,
    },
    /// Test blocks for the game test runner: `trigger` triggers each one
    /// as a signal would (a start block powers its neighbours), otherwise
    /// answers whether each was triggered since its last reset (as
    /// `Triggered`), resetting the triggered ones when `reset`.
    TestBlocks {
        positions: Vec<BlockPos>,
        trigger: bool,
        reset: bool,
    },
    /// The test instance blocks this region's loaded points of interest
    /// hold whose x and z lie within `radius` of `center` (as `Positions`).
    TestInstances { center: BlockPos, radius: i32 },
    /// The pending block ticks inside the box from `min` to `max`.
    CopyTicks { min: BlockPos, max: BlockPos },
    /// Schedules each tick in order, keeping its delay and priority.
    PasteTicks { ticks: Vec<CopiedTick> },
    /// Replaces the item of the item port block at `pos` (a jukebox), as
    /// `/item` does; `Set(1)` when the port took it, else `Set(0)`.
    ReplacePortItem {
        pos: BlockPos,
        stack: Option<tuff_item::item::ItemStack>,
    },
    /// Biomes of the cells whose corner lies in the box from `min` to
    /// `max`, restricted to cells whose biome is in `filter`. Without
    /// `apply` it only counts the chunks of the box this region owns that
    /// are not loaded (answered as `Set`); the region never loads chunks
    /// for it.
    FillBiome {
        min: BlockPos,
        max: BlockPos,
        biome: u16,
        filter: Option<Vec<u16>>,
        apply: bool,
    },
}

/// A pending block tick lifted out of the world, due `delay` ticks from
/// now; `order` ranks it among the ticks copied with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopiedTick {
    pub pos: BlockPos,
    pub block: tuff_vanilla_data::Block,
    pub delay: u32,
    pub priority: i32,
    pub order: i64,
}

#[derive(Debug)]
pub enum BlockOpResult {
    States(Vec<Option<BlockState>>),
    Biomes(Vec<Option<&'static str>>),
    /// `None` where the position is not this region's.
    Copied(Vec<Option<(BlockState, Option<Compound>)>>),
    /// How many blocks changed.
    Set(usize),
    Ticks(Vec<CopiedTick>),
    /// `None` where the position is not this region's.
    Triggered(Vec<Option<bool>>),
    Positions(Vec<BlockPos>),
}

/// Where a command's feedback goes.
#[derive(Debug, Clone)]
pub enum FeedbackTarget {
    Player(PlayerHandle),
    Console,
    /// The console run through the server API, which collects the lines.
    Channel(tokio::sync::mpsc::UnboundedSender<TextComponent>),
    /// A command block's tracked output, collected as lines; success only
    /// shows when command feedback is on.
    CommandBlock(tokio::sync::mpsc::UnboundedSender<TextComponent>),
    /// Nowhere: output is dropped and operators are not told.
    Silent,
}

impl FeedbackTarget {
    pub fn send(&self, message: TextComponent) {
        match self {
            Self::Player(handle) => {
                handle.send(PlayerEvent::Message(message));
            }
            Self::Console => log::info!("{}", message.plain()),
            Self::Channel(sender) | Self::CommandBlock(sender) => {
                let _ = sender.send(message);
            }
            Self::Silent => {}
        }
    }

    pub fn player_id(&self) -> Option<PlayerId> {
        match self {
            Self::Player(handle) => Some(handle.id),
            Self::Console | Self::Channel(_) | Self::CommandBlock(_) | Self::Silent => None,
        }
    }

    pub fn is_command_block(&self) -> bool {
        matches!(self, Self::CommandBlock(_))
    }
}

/// A sweep: the damage each nearby entity takes before enchantments and
/// cooldown, and the cooldown fraction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sweep {
    pub damage: f32,
    pub strength: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandMode {
    Sequence,
    Auto,
    Redstone,
}

impl CommandMode {
    pub fn of_block(block: tuff_vanilla_data::Block) -> Option<Self> {
        match block {
            tuff_vanilla_data::Block::CommandBlock => Some(Self::Redstone),
            tuff_vanilla_data::Block::RepeatingCommandBlock => Some(Self::Auto),
            tuff_vanilla_data::Block::ChainCommandBlock => Some(Self::Sequence),
            _ => None,
        }
    }

    pub fn from_id(id: i32) -> Self {
        match id {
            0 => Self::Sequence,
            1 => Self::Auto,
            _ => Self::Redstone,
        }
    }

    pub fn block(self) -> tuff_vanilla_data::Block {
        match self {
            Self::Sequence => tuff_vanilla_data::Block::ChainCommandBlock,
            Self::Auto => tuff_vanilla_data::Block::RepeatingCommandBlock,
            Self::Redstone => tuff_vanilla_data::Block::CommandBlock,
        }
    }
}

/// Command block work that crosses into a neighbour region.
#[derive(Debug, Clone)]
pub enum CommandRemote {
    /// A chain goes on past `from` (the sender's last block) towards
    /// `direction` with `budget` steps left. `back` is the success count
    /// of the block at `from`, which a conditional block behind it reads.
    Chain {
        from: BlockPos,
        direction: tuff_vanilla_data::block::Facing,
        budget: u32,
        back: Option<i32>,
    },
    /// The command block at `pos` in region `reply` is conditional on the
    /// block at `back`, which the receiver owns.
    AskSuccess {
        pos: BlockPos,
        back: BlockPos,
        reply: tuff_world_model::pos::RegionPos,
        resume: Resume,
    },
    /// The answer: `back`'s success count, `None` when it is no command
    /// block.
    Success {
        pos: BlockPos,
        back: BlockPos,
        count: Option<i32>,
        resume: Resume,
    },
}

/// What a command block waiting on a neighbour's success count does once
/// it has the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resume {
    /// Only records whether its condition is met.
    Record,
    /// Records it and schedules its tick (it was just powered or edited).
    Schedule,
    /// Records it and runs its step of a chain with `budget` steps left.
    Chain { budget: u32 },
}

/// What the editor screen sent.
#[derive(Debug, Clone)]
pub struct CommandBlockEdit {
    pub command: String,
    pub mode: CommandMode,
    pub track_output: bool,
    pub conditional: bool,
    pub automatic: bool,
}
