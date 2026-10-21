//! Block entities this server models: block containers, the furnace family
//! and campfires. They are chunk data, stored in the chunk's
//! `block_entities` list with the game's schema; block entities of other
//! kinds pass through as raw NBT.

pub mod analog;
pub mod editors;
pub mod raw;
pub mod sign;

use std::collections::{BTreeMap, HashMap};

use rand::Rng;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::registry::BlockEntityType;
use tuff_vanilla_data::{Block, Item, tag};

use crate::world::crafting::RecipeSet;
use crate::world::menu::MenuKind;
use tuff_item::item::bundle::fits_inside_container_item;
use tuff_item::item::{Components, ItemStack};
use tuff_item::text::TextComponent;
use tuff_nbt::Compound;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::Chunk;
use tuff_world_model::pos::ChunkPos;

pub const HOPPER_COOLDOWN: i32 = 8;
const NO_COOLDOWN: i32 = -1;
const ITEMS: &str = "Items";
const CUSTOM_NAME: &str = "CustomName";
const TRANSFER_COOLDOWN: &str = "TransferCooldown";
const KEEP_PACKED: &str = "keepPacked";
const POSITION_KEYS: [&str; 4] = ["id", "x", "y", "z"];
const FURNACE_KEYS: [&str; 6] = [
    "cooking_time_spent",
    "cooking_total_time",
    "lit_time_remaining",
    "lit_total_time",
    "speed_multiplier",
    "RecipesUsed",
];
const CAMPFIRE_KEYS: [&str; 2] = ["CookingTimes", "CookingTotalTimes"];
const BREWING_KEYS: [&str; 5] = [
    "BrewTime",
    "total_brew_time",
    "Fuel",
    "total_fuel",
    "speed_multiplier",
];
pub const CAMPFIRE_SLOTS: usize = 4;
pub const CRAFTER_SLOTS: usize = 9;
const CRAFTER_KEYS: [&str; 3] = ["disabled_slots", "crafting_ticks_remaining", "triggered"];

pub mod brewing {
    /// The three bottle slots come first.
    pub const BOTTLES: usize = 3;
    pub const INGREDIENT: usize = 3;
    pub const FUEL: usize = 4;
    pub const SIZE: usize = 5;
    /// Container data slots, in the game's order.
    pub const DATA_BREW_TIME: usize = 0;
    pub const DATA_FUEL: usize = 1;
    pub const DATA_COUNT: usize = 4;
}

pub mod furnace {
    pub const INPUT: usize = 0;
    pub const FUEL: usize = 1;
    pub const RESULT: usize = 2;
    pub const SIZE: usize = 3;
    /// Container data slots, in the game's order.
    pub const DATA_LIT_TIME: usize = 0;
    pub const DATA_LIT_DURATION: usize = 1;
    pub const DATA_COOKING_PROGRESS: usize = 2;
    pub const DATA_COOKING_TOTAL: usize = 3;
    pub const DATA_COUNT: usize = 4;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContainerKind {
    Chest,
    TrappedChest,
    Barrel,
    ShulkerBox,
    Hopper,
    Dropper,
    Dispenser,
    Furnace,
    BlastFurnace,
    Smoker,
    Campfire,
    BrewingStand,
    Crafter,
}

impl ContainerKind {
    pub const COUNT: usize = 13;

    pub fn of_block(block: Block) -> Option<Self> {
        Some(match block {
            Block::Chest => Self::Chest,
            Block::TrappedChest => Self::TrappedChest,
            Block::Barrel => Self::Barrel,
            Block::Hopper => Self::Hopper,
            Block::Dropper => Self::Dropper,
            Block::Dispenser => Self::Dispenser,
            Block::Furnace => Self::Furnace,
            Block::BlastFurnace => Self::BlastFurnace,
            Block::Smoker => Self::Smoker,
            Block::Campfire | Block::SoulCampfire => Self::Campfire,
            Block::BrewingStand => Self::BrewingStand,
            Block::Crafter => Self::Crafter,
            _ if tuff_datapack::tags::is(tag::block::key::COPPER_CHESTS, block) => Self::Chest,
            _ if tuff_datapack::tags::is(tag::block::key::SHULKER_BOXES, block) => Self::ShulkerBox,
            _ => return None,
        })
    }

    pub fn entity_type(self) -> BlockEntityType {
        match self {
            Self::Chest => BlockEntityType::Chest,
            Self::TrappedChest => BlockEntityType::TrappedChest,
            Self::Barrel => BlockEntityType::Barrel,
            Self::ShulkerBox => BlockEntityType::ShulkerBox,
            Self::Hopper => BlockEntityType::Hopper,
            Self::Dropper => BlockEntityType::Dropper,
            Self::Dispenser => BlockEntityType::Dispenser,
            Self::Furnace => BlockEntityType::Furnace,
            Self::BlastFurnace => BlockEntityType::BlastFurnace,
            Self::Smoker => BlockEntityType::Smoker,
            Self::Campfire => BlockEntityType::Campfire,
            Self::BrewingStand => BlockEntityType::BrewingStand,
            Self::Crafter => BlockEntityType::Crafter,
        }
    }

    pub fn of_entity_type(kind: BlockEntityType) -> Option<Self> {
        Some(match kind {
            BlockEntityType::Chest => Self::Chest,
            BlockEntityType::TrappedChest => Self::TrappedChest,
            BlockEntityType::Barrel => Self::Barrel,
            BlockEntityType::ShulkerBox => Self::ShulkerBox,
            BlockEntityType::Hopper => Self::Hopper,
            BlockEntityType::Dropper => Self::Dropper,
            BlockEntityType::Dispenser => Self::Dispenser,
            BlockEntityType::Furnace => Self::Furnace,
            BlockEntityType::BlastFurnace => Self::BlastFurnace,
            BlockEntityType::Smoker => Self::Smoker,
            BlockEntityType::Campfire => Self::Campfire,
            BlockEntityType::BrewingStand => Self::BrewingStand,
            BlockEntityType::Crafter => Self::Crafter,
            _ => return None,
        })
    }

    pub fn size(self) -> usize {
        match self {
            Self::Chest | Self::TrappedChest | Self::Barrel | Self::ShulkerBox => 27,
            Self::Hopper => 5,
            Self::Dropper | Self::Dispenser | Self::Crafter => 9,
            Self::Furnace | Self::BlastFurnace | Self::Smoker => furnace::SIZE,
            Self::Campfire => CAMPFIRE_SLOTS,
            Self::BrewingStand => brewing::SIZE,
        }
    }

    /// The menu a right click opens; campfires have none.
    pub fn menu(self) -> Option<MenuKind> {
        Some(match self {
            Self::Chest | Self::TrappedChest | Self::Barrel => MenuKind::Generic { rows: 3 },
            Self::ShulkerBox => MenuKind::ShulkerBox,
            Self::Hopper => MenuKind::Hopper,
            Self::Dropper | Self::Dispenser => MenuKind::Generic3x3,
            Self::Furnace => MenuKind::Furnace,
            Self::BlastFurnace => MenuKind::BlastFurnace,
            Self::Smoker => MenuKind::Smoker,
            Self::BrewingStand => MenuKind::BrewingStand,
            Self::Crafter => MenuKind::Crafter,
            Self::Campfire => return None,
        })
    }

    /// The translation key of the menu title.
    pub fn title(self) -> &'static str {
        match self {
            Self::Chest | Self::TrappedChest => "container.chest",
            Self::Barrel => "container.barrel",
            Self::ShulkerBox => "container.shulkerBox",
            Self::Hopper => "container.hopper",
            Self::Dropper => "container.dropper",
            Self::Dispenser => "container.dispenser",
            Self::Furnace => "container.furnace",
            Self::BlastFurnace => "container.blast_furnace",
            Self::Smoker => "container.smoker",
            Self::BrewingStand => "container.brewing",
            Self::Crafter => "container.crafter",
            Self::Campfire => "",
        }
    }

    /// Whether two adjacent blocks of this kind join into one menu.
    pub fn pairs(self) -> bool {
        matches!(self, Self::Chest | Self::TrappedChest)
    }

    /// Whether hoppers and droppers can move items in and out.
    pub fn is_container(self) -> bool {
        self != Self::Campfire
    }

    pub fn is_furnace(self) -> bool {
        matches!(self, Self::Furnace | Self::BlastFurnace | Self::Smoker)
    }

    /// The recipe property set naming what this block cooks.
    pub fn cooking_set(self) -> Option<&'static str> {
        Some(match self {
            Self::Furnace => "furnace_input",
            Self::BlastFurnace => "blast_furnace_input",
            Self::Smoker => "smoker_input",
            Self::Campfire => "campfire_input",
            _ => return None,
        })
    }

    /// Whether the block entity acts every tick.
    pub fn ticks(self) -> bool {
        matches!(
            self,
            Self::Hopper | Self::Campfire | Self::BrewingStand | Self::Crafter
        ) || self.is_furnace()
    }

    /// Whether an automated transfer may put `stack` in `slot` of a
    /// container holding `items`, reached through `face`.
    pub fn can_insert(
        self,
        recipes: &RecipeSet,
        items: &[Option<ItemStack>],
        slot: usize,
        stack: &ItemStack,
        _face: Option<Facing>,
        disabled: u16,
    ) -> bool {
        match self {
            Self::Crafter => crafter_accepts(items, slot, disabled),
            Self::ShulkerBox => fits_inside_container_item(stack),
            Self::BrewingStand => match slot {
                brewing::FUEL => recipes.is_brewing_fuel(stack),
                brewing::INGREDIENT => recipes.is_brewing_reagent(stack),
                _ => recipes.is_potion_input(stack) && items[slot].is_none(),
            },
            _ if self.is_furnace() => match slot {
                furnace::RESULT => false,
                furnace::FUEL => {
                    let bucket_in = items[furnace::FUEL]
                        .as_ref()
                        .is_some_and(|fuel| fuel.item == Item::Bucket);
                    stack.item.cooking_fuel().is_some()
                        || (stack.item == Item::Bucket && !bucket_in)
                }
                _ => true,
            },
            _ => true,
        }
    }

    pub fn can_extract(self, slot: usize, stack: &ItemStack, face: Option<Facing>) -> bool {
        if self == Self::BrewingStand && slot == brewing::INGREDIENT {
            return stack.item == Item::GlassBottle;
        }
        if self.is_furnace() && face == Some(Facing::Down) && slot == furnace::FUEL {
            return tuff_datapack::tags::is(
                tag::item::key::FURNACE_FUEL_BOTTOM_TAKEABLE,
                stack.item,
            );
        }
        true
    }

    /// The slots an automated transfer sees through `face`, in the order
    /// it tries them.
    pub fn slots_for_face(self, face: Option<Facing>) -> Vec<usize> {
        if self == Self::BrewingStand {
            return match face {
                Some(Facing::Up) => vec![brewing::INGREDIENT],
                Some(Facing::Down) => vec![0, 1, 2, brewing::INGREDIENT],
                Some(_) => vec![0, 1, 2, brewing::FUEL],
                None => (0..brewing::SIZE).collect(),
            };
        }
        if self.is_furnace() {
            return match face {
                Some(Facing::Down) => vec![furnace::RESULT, furnace::FUEL],
                Some(Facing::Up) => vec![furnace::INPUT],
                Some(_) => vec![furnace::FUEL],
                None => (0..furnace::SIZE).collect(),
            };
        }
        (0..self.size()).collect()
    }

    fn saves_empty_items(self) -> bool {
        self != Self::ShulkerBox
    }
}

/// Whether a crafter takes an automated item in `slot`: enabled, not full,
/// and no later slot that is empty or holds a smaller stack of the same
/// item takes it first.
fn crafter_accepts(items: &[Option<ItemStack>], slot: usize, disabled: u16) -> bool {
    if disabled & (1 << slot) != 0 {
        return false;
    }
    let Some(current) = &items[slot] else {
        return true;
    };
    if current.count() >= current.max_stack_size() {
        return false;
    }
    !(slot + 1..CRAFTER_SLOTS).any(|other| {
        disabled & (1 << other) == 0
            && match &items[other] {
                None => true,
                Some(stack) => stack.count() < current.count() && stack.stacks_with(current),
            }
    })
}

/// A crafter's saved state beyond its items.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Crafter {
    /// One bit per slot the player switched off.
    pub disabled: u16,
    pub crafting_ticks: i32,
    pub triggered: bool,
}

impl Crafter {
    /// The container data slots: nine slot states, then the trigger.
    pub fn data(&self) -> Vec<i32> {
        let mut data: Vec<i32> = (0..CRAFTER_SLOTS)
            .map(|slot| i32::from(self.disabled & (1 << slot) != 0))
            .collect();
        data.push(i32::from(self.triggered));
        data
    }
}

/// A furnace's progress, saved with the game's keys.
#[derive(Debug, Clone, PartialEq)]
pub struct Furnace {
    pub lit_time_remaining: i32,
    pub lit_total_time: i32,
    pub cooking_timer: i32,
    pub cooking_total_time: i32,
    /// The last fuel's speed multiplier, which scales recipe times.
    pub speed_multiplier: f32,
    /// Recipe id to how many times it cooked since experience was last
    /// collected.
    pub recipes_used: BTreeMap<String, i32>,
    /// A ticker's recipe cache (an index into its own table); never saved.
    pub last_recipe: Option<u32>,
}

impl Default for Furnace {
    fn default() -> Self {
        Self {
            lit_time_remaining: 0,
            lit_total_time: 0,
            cooking_timer: 0,
            cooking_total_time: 0,
            speed_multiplier: 1.0,
            recipes_used: BTreeMap::new(),
            last_recipe: None,
        }
    }
}

impl Furnace {
    pub fn is_lit(&self) -> bool {
        self.lit_time_remaining > 0
    }

    /// The Set Container Property values, in the game's order.
    pub fn data(&self) -> [i32; furnace::DATA_COUNT] {
        [
            self.lit_time_remaining,
            self.lit_total_time,
            self.cooking_timer,
            self.cooking_total_time,
        ]
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Campfire {
    pub cooking_progress: [i32; CAMPFIRE_SLOTS],
    pub cooking_time: [i32; CAMPFIRE_SLOTS],
}

/// Per-kind state beyond the item slots.
#[derive(Debug, Clone, PartialEq)]
pub enum State {
    Plain,
    Furnace(Furnace),
    Campfire(Campfire),
    Brewing(Brewing),
    Crafter(Crafter),
}

impl State {
    fn for_kind(kind: ContainerKind) -> Self {
        match kind {
            _ if kind.is_furnace() => Self::Furnace(Furnace::default()),
            ContainerKind::Campfire => Self::Campfire(Campfire::default()),
            ContainerKind::BrewingStand => Self::Brewing(Default::default()),
            ContainerKind::Crafter => Self::Crafter(Crafter::default()),
            _ => Self::Plain,
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct BlockEntity {
    pub kind: ContainerKind,
    pub items: Vec<Option<ItemStack>>,
    pub custom_name: Option<TextComponent>,
    /// Hoppers: ticks until the next transfer, `-1` when idle.
    pub transfer_cooldown: i32,
    pub state: State,
    /// Fields this server does not model (locks, loot tables, ...), kept
    /// as stored.
    pub extra: Compound,
}

impl BlockEntity {
    pub fn new(kind: ContainerKind) -> Self {
        Self {
            kind,
            items: (0..kind.size()).map(|_| None).collect(),
            custom_name: None,
            transfer_cooldown: NO_COOLDOWN,
            state: State::for_kind(kind),
            extra: Compound::new(),
        }
    }

    pub fn furnace(&self) -> Option<&Furnace> {
        match &self.state {
            State::Furnace(furnace) => Some(furnace),
            _ => None,
        }
    }

    pub fn furnace_mut(&mut self) -> Option<&mut Furnace> {
        match &mut self.state {
            State::Furnace(furnace) => Some(furnace),
            _ => None,
        }
    }

    pub fn brewing_mut(&mut self) -> Option<&mut Brewing> {
        match &mut self.state {
            State::Brewing(brewing) => Some(brewing),
            _ => None,
        }
    }

    pub fn crafter(&self) -> Option<&Crafter> {
        match &self.state {
            State::Crafter(crafter) => Some(crafter),
            _ => None,
        }
    }

    pub fn crafter_mut(&mut self) -> Option<&mut Crafter> {
        match &mut self.state {
            State::Crafter(crafter) => Some(crafter),
            _ => None,
        }
    }

    /// What a comparator reads from a crafter: its filled or disabled slots.
    pub fn crafter_signal(&self) -> u8 {
        let disabled = self.crafter().map_or(0, |crafter| crafter.disabled);
        (0..CRAFTER_SLOTS)
            .filter(|slot| self.items[*slot].is_some() || disabled & (1 << slot) != 0)
            .count() as u8
    }

    pub fn campfire_mut(&mut self) -> Option<&mut Campfire> {
        match &mut self.state {
            State::Campfire(campfire) => Some(campfire),
            _ => None,
        }
    }

    /// The container data slots a menu over this block entity shows.
    pub fn data(&self) -> Vec<i32> {
        match &self.state {
            State::Furnace(furnace) => furnace.data().to_vec(),
            State::Brewing(brewing) => brewing.data().to_vec(),
            State::Crafter(crafter) => crafter.data(),
            State::Plain | State::Campfire(_) => Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.items.iter().all(Option::is_none)
    }

    pub fn is_full(&self) -> bool {
        self.items.iter().all(|slot| {
            slot.as_ref()
                .is_some_and(|s| s.count() >= s.max_stack_size())
        })
    }

    /// The NBT clients need to render this block entity (a campfire's
    /// food), if any.
    pub fn client_data(&self) -> Option<Compound> {
        if self.kind != ContainerKind::Campfire {
            return None;
        }
        Some(Compound::new().with(ITEMS, encode_items(&self.items)))
    }

    /// Reads a stored block entity, or `None` for a kind this server does
    /// not model (whose NBT is kept as is).
    pub fn decode(nbt: &Compound) -> Option<(BlockPos, Self)> {
        let kind = BlockEntityType::from_name(nbt.get_str("id")?)
            .and_then(ContainerKind::of_entity_type)?;
        let pos = BlockPos::new(nbt.get_int("x")?, nbt.get_int("y")?, nbt.get_int("z")?);
        let mut entity = Self::new(kind);
        entity.items = decode_items(nbt.get_list(ITEMS).unwrap_or(&[]), kind.size());
        entity.custom_name = nbt.get(CUSTOM_NAME).and_then(TextComponent::from_nbt);
        entity.transfer_cooldown = nbt.get_int(TRANSFER_COOLDOWN).unwrap_or(NO_COOLDOWN);
        let reagent = (kind == ContainerKind::BrewingStand)
            .then(|| {
                entity.items[brewing::INGREDIENT]
                    .as_ref()
                    .map(|stack| stack.item)
            })
            .flatten();
        match &mut entity.state {
            State::Furnace(furnace) => {
                furnace.cooking_timer = nbt.get_int("cooking_time_spent").unwrap_or(0);
                furnace.cooking_total_time = nbt.get_int("cooking_total_time").unwrap_or(0);
                furnace.lit_time_remaining = nbt.get_int("lit_time_remaining").unwrap_or(0);
                furnace.lit_total_time = nbt.get_int("lit_total_time").unwrap_or(0);
                furnace.speed_multiplier = nbt.get_float("speed_multiplier").unwrap_or(1.0);
                if let Some(used) = nbt.get_compound("RecipesUsed") {
                    for (recipe, count) in used.iter() {
                        if let Some(count) = count.as_int() {
                            furnace.recipes_used.insert(recipe.to_owned(), count);
                        }
                    }
                }
            }
            State::Campfire(campfire) => {
                let read = |key: &str, into: &mut [i32; CAMPFIRE_SLOTS]| {
                    for (slot, value) in nbt
                        .get_int_array(key)
                        .unwrap_or(&[])
                        .iter()
                        .take(CAMPFIRE_SLOTS)
                        .enumerate()
                    {
                        into[slot] = *value;
                    }
                };
                read("CookingTimes", &mut campfire.cooking_progress);
                read("CookingTotalTimes", &mut campfire.cooking_time);
            }
            State::Brewing(brewing) => {
                brewing.brew_time = nbt.get_int("BrewTime").unwrap_or(0);
                brewing.total_brew_time = nbt.get_int("total_brew_time").unwrap_or(BREW_TIME);
                brewing.fuel = nbt.get_int("Fuel").unwrap_or(0);
                brewing.total_fuel = nbt.get_int("total_fuel").unwrap_or(DEFAULT_FUEL_USES);
                brewing.speed_multiplier = nbt.get_float("speed_multiplier").unwrap_or(1.0);
                if brewing.brew_time > 0 {
                    brewing.ingredient = reagent;
                }
            }
            State::Crafter(crafter) => {
                crafter.crafting_ticks = nbt.get_int("crafting_ticks_remaining").unwrap_or(0);
                crafter.triggered = nbt.get_int("triggered").unwrap_or(0) == 1;
                for slot in nbt.get_int_array("disabled_slots").unwrap_or(&[]) {
                    if (0..CRAFTER_SLOTS as i32).contains(slot)
                        && entity.items[*slot as usize].is_none()
                    {
                        crafter.disabled |= 1 << slot;
                    }
                }
            }
            State::Plain => {}
        }
        for (key, value) in nbt.iter() {
            let modeled = POSITION_KEYS.contains(&key)
                || key == ITEMS
                || key == KEEP_PACKED
                || (key == CUSTOM_NAME && entity.custom_name.is_some())
                || (key == TRANSFER_COOLDOWN && kind == ContainerKind::Hopper)
                || (kind.is_furnace() && FURNACE_KEYS.contains(&key))
                || (kind == ContainerKind::Campfire && CAMPFIRE_KEYS.contains(&key))
                || (kind == ContainerKind::BrewingStand && BREWING_KEYS.contains(&key))
                || (kind == ContainerKind::Crafter && CRAFTER_KEYS.contains(&key));
            if !modeled {
                entity.extra.insert(key, value.clone());
            }
        }
        Some((pos, entity))
    }

    pub fn encode(&self, pos: BlockPos) -> Compound {
        let mut nbt = Compound::new()
            .with("id", self.kind.entity_type().name())
            .with("x", pos.x)
            .with("y", pos.y)
            .with("z", pos.z)
            .with(KEEP_PACKED, false);
        let items = encode_items(&self.items);
        if !items.is_empty() || self.kind.saves_empty_items() {
            nbt.insert(ITEMS, items);
        }
        if let Some(name) = &self.custom_name {
            nbt.insert(CUSTOM_NAME, name.to_nbt());
        }
        if self.kind == ContainerKind::Hopper {
            nbt.insert(TRANSFER_COOLDOWN, self.transfer_cooldown);
        }
        match &self.state {
            State::Furnace(furnace) => {
                nbt.insert("cooking_time_spent", furnace.cooking_timer);
                nbt.insert("cooking_total_time", furnace.cooking_total_time);
                nbt.insert("lit_time_remaining", furnace.lit_time_remaining);
                nbt.insert("lit_total_time", furnace.lit_total_time);
                nbt.insert("speed_multiplier", furnace.speed_multiplier);
                let mut used = Compound::new();
                for (recipe, count) in &furnace.recipes_used {
                    used.insert(recipe.as_str(), *count);
                }
                nbt.insert("RecipesUsed", used);
            }
            State::Campfire(campfire) => {
                nbt.insert("CookingTimes", campfire.cooking_progress.to_vec());
                nbt.insert("CookingTotalTimes", campfire.cooking_time.to_vec());
            }
            State::Brewing(brewing) => {
                nbt.insert("BrewTime", brewing.brew_time);
                nbt.insert("total_brew_time", brewing.total_brew_time);
                nbt.insert("Fuel", brewing.fuel);
                nbt.insert("total_fuel", brewing.total_fuel);
                nbt.insert("speed_multiplier", brewing.speed_multiplier);
            }
            State::Crafter(crafter) => {
                nbt.insert("crafting_ticks_remaining", crafter.crafting_ticks);
                let disabled: Vec<i32> = (0..CRAFTER_SLOTS as i32)
                    .filter(|slot| crafter.disabled & (1 << slot) != 0)
                    .collect();
                nbt.insert("disabled_slots", disabled);
                nbt.insert("triggered", i32::from(crafter.triggered));
            }
            State::Plain => {}
        }
        for (key, value) in self.extra.iter() {
            nbt.insert(key, value.clone());
        }
        nbt
    }
}

/// What a save writes for a chunk's block entities, taken on the owning
/// thread: the modeled ones are encoded in place (their stacks are read,
/// never copied) and stored with the unmodeled ones.
pub trait SavePicture {
    fn save_picture(&self) -> Self;
}

impl SavePicture for tuff_world_model::chunk_meta::ChunkMeta<BlockEntity> {
    fn save_picture(&self) -> Self {
        let mut raw_block_entities = self.raw_block_entities.clone();
        raw_block_entities.extend(
            self.block_entities
                .iter()
                .map(|(pos, entity)| (*pos, entity.encode(*pos))),
        );
        Self {
            status: self.status.clone(),
            extra: self.extra.clone(),
            relight: self.relight,
            block_entities: Default::default(),
            raw_block_entities,
            below_zero_retrogen: self.below_zero_retrogen,
        }
    }
}

/// `size` slots filled from saved `Items` entries; entries with a slot out
/// of range or an unreadable stack are dropped.
pub fn decode_items(entries: &[tuff_nbt::Tag], size: usize) -> Vec<Option<ItemStack>> {
    let mut items: Vec<Option<ItemStack>> = (0..size).map(|_| None).collect();
    for entry in entries {
        let Some(entry) = entry.as_compound() else {
            continue;
        };
        let slot = entry.get_byte("Slot").unwrap_or(-1);
        if !(0..size as i64).contains(&i64::from(slot)) {
            continue;
        }
        let mut stack = entry.clone();
        stack.remove("Slot");
        items[slot as usize] = ItemStack::from_storage(&stack);
    }
    items
}

/// Saved `Items` entries: the non-empty stacks, each tagged with its slot.
pub fn encode_items(items: &[Option<ItemStack>]) -> Vec<Compound> {
    items
        .iter()
        .enumerate()
        .filter_map(|(slot, stack)| {
            let mut entry = Compound::new().with("Slot", slot as i8);
            for (key, value) in stack.as_ref()?.to_nbt() {
                entry.insert(key, value);
            }
            Some(entry)
        })
        .collect()
}

/// The `size` slots of the saved `Items` of `nbt`.
pub fn read_items(nbt: &Compound, size: usize) -> Vec<Option<ItemStack>> {
    decode_items(nbt.get_list(ITEMS).unwrap_or(&[]), size)
}

/// Saves `items` as the `Items` of `nbt`.
pub fn write_items(nbt: &mut Compound, items: &[Option<ItemStack>]) {
    nbt.insert(
        ITEMS,
        tuff_nbt::Tag::List(
            encode_items(items)
                .into_iter()
                .map(tuff_nbt::Tag::Compound)
                .collect(),
        ),
    );
}

/// Puts `stack` into `items` the way a copper golem does: front to back,
/// topping up stacks of the same item with room until the first empty
/// slot, which takes everything left. Returns what is left (`None` when
/// all went in)
/// and whether the walk reached an empty slot.
/// How many of `stack` [`golem_place`] would put into `items`.
pub fn golem_room<'a>(
    items: impl IntoIterator<Item = &'a Option<ItemStack>>,
    stack: &ItemStack,
) -> u8 {
    let mut room = 0u8;
    for item in items {
        match item {
            None => return stack.count(),
            Some(existing) if existing.stacks_with(stack) => {
                room =
                    room.saturating_add(existing.max_stack_size().saturating_sub(existing.count()));
                if room >= stack.count() {
                    return stack.count();
                }
            }
            Some(_) => {}
        }
    }
    room
}

pub fn golem_place(
    items: &mut [Option<ItemStack>],
    stack: ItemStack,
    changed: &mut Vec<usize>,
) -> (Option<ItemStack>, bool) {
    let mut held = stack;
    for (slot, item) in items.iter_mut().enumerate() {
        match item {
            None => {
                *item = Some(held);
                changed.push(slot);
                return (None, true);
            }
            Some(existing) if existing.stacks_with(&held) => {
                let max = existing.max_stack_size();
                if existing.count() >= max {
                    continue;
                }
                let room = max - existing.count();
                existing.transfer(&mut held, room);
                changed.push(slot);
                if held.count() == 0 {
                    return (None, false);
                }
            }
            Some(_) => {}
        }
    }
    (Some(held), false)
}

/// Puts `stack` into `items` (of a `kind` container reached through
/// `face`) the way hoppers and droppers do: the first matching stack with
/// room, else the first empty slot, in slot order. Returns what is left and
/// the slots that changed.
pub fn insert(
    recipes: &RecipeSet,
    kind: ContainerKind,
    items: &mut [Option<ItemStack>],
    face: Option<Facing>,
    mut stack: ItemStack,
    changed: &mut Vec<usize>,
    disabled: u16,
) -> Option<ItemStack> {
    for slot in kind.slots_for_face(face) {
        if stack.count() == 0 {
            break;
        }
        if !kind.can_insert(recipes, items, slot, &stack, face, disabled) {
            continue;
        }
        match &mut items[slot] {
            None => {
                let count = stack.count();
                items[slot] = Some(stack.take(count));
                changed.push(slot);
            }
            Some(current)
                if current.stacks_with(&stack) && current.count() <= current.max_stack_size() =>
            {
                let moved = stack
                    .count()
                    .min(stack.max_stack_size().saturating_sub(current.count()));
                if moved > 0 {
                    current.transfer(&mut stack, moved);
                    changed.push(slot);
                }
            }
            Some(_) => {}
        }
    }
    (stack.count() > 0).then_some(stack)
}

/// How many of `count` of `stack` [`insert`] would put into `items`,
/// reading them only. Exact for containers whose slots accept items
/// regardless of the others (chests).
pub fn insert_room(
    recipes: &RecipeSet,
    kind: ContainerKind,
    items: &[Option<ItemStack>],
    face: Option<Facing>,
    stack: &ItemStack,
    count: u8,
    disabled: u16,
) -> u8 {
    let mut left = count;
    for slot in kind.slots_for_face(face) {
        if left == 0 {
            break;
        }
        if !kind.can_insert(recipes, items, slot, stack, face, disabled) {
            continue;
        }
        match &items[slot] {
            None => left = 0,
            Some(current)
                if current.stacks_with(stack) && current.count() <= current.max_stack_size() =>
            {
                left -= left.min(stack.max_stack_size().saturating_sub(current.count()));
            }
            Some(_) => {}
        }
    }
    count - left
}

/// Whether every slot of `items` reachable through `face` is full.
pub fn is_full(kind: ContainerKind, items: &[Option<ItemStack>], face: Option<Facing>) -> bool {
    kind.slots_for_face(face).into_iter().all(|slot| {
        items[slot]
            .as_ref()
            .is_some_and(|s| s.count() >= s.max_stack_size())
    })
}

/// Moves one item from `from` into `into`, as a hopper pushing or pulling
/// does: the first slot whose single item the target accepts. Returns the
/// changed slots of `from` and `into`.
pub fn transfer_one(
    recipes: &RecipeSet,
    from_kind: ContainerKind,
    from: &mut [Option<ItemStack>],
    from_face: Option<Facing>,
    into_kind: ContainerKind,
    into: &mut [Option<ItemStack>],
    into_face: Option<Facing>,
) -> Option<(usize, Vec<usize>)> {
    for slot in from_kind.slots_for_face(from_face) {
        let Some(stack) = from[slot].as_ref() else {
            continue;
        };
        if !from_kind.can_extract(slot, stack, from_face) {
            continue;
        }
        let Some(one) = ItemStack::take_from(&mut from[slot], 1) else {
            continue;
        };
        let mut changed = Vec::new();
        match insert(recipes, into_kind, into, into_face, one, &mut changed, 0) {
            None => return Some((slot, changed)),
            Some(back) => ItemStack::put_back(&mut from[slot], back),
        }
    }
    None
}

/// How a container's contents scatter when its block breaks: each stack
/// split into pieces of 10-30 items at a random spot inside the block with
/// a small random velocity, as the game does.
pub fn scatter(
    pos: BlockPos,
    items: impl IntoIterator<Item = ItemStack>,
    rng: &mut impl Rng,
) -> Vec<(ItemStack, [f64; 3], [f64; 3])> {
    const ITEM_WIDTH: f64 = 0.25;
    const DEVIATION: f64 = 0.114_850_001_711_398_36;
    let mut drops = Vec::new();
    for mut stack in items {
        let position = [
            pos.x as f64 + rng.r#gen::<f64>() * (1.0 - ITEM_WIDTH) + ITEM_WIDTH / 2.0,
            pos.y as f64 + rng.r#gen::<f64>() * (1.0 - ITEM_WIDTH),
            pos.z as f64 + rng.r#gen::<f64>() * (1.0 - ITEM_WIDTH) + ITEM_WIDTH / 2.0,
        ];
        while stack.count() > 0 {
            let piece = (rng.gen_range(0..21) + 10).min(stack.count() as i32) as u8;
            let piece = stack.take(piece);
            let mut triangle =
                |mode: f64| mode + DEVIATION * (rng.r#gen::<f64>() - rng.r#gen::<f64>());
            let velocity = [triangle(0.0), triangle(0.2), triangle(0.0)];
            drops.push((piece, position, velocity));
        }
    }
    drops
}

/// Takes out the items to drop when a container block breaks: its
/// contents, or for shulker boxes nothing (they keep their contents in
/// the item).
pub fn spilled(entity: &mut BlockEntity) -> Vec<ItemStack> {
    if entity.kind == ContainerKind::ShulkerBox {
        return Vec::new();
    }
    entity.items.iter_mut().filter_map(Option::take).collect()
}

/// Splits a chunk's stored `block_entities` into the modeled ones and the
/// rest, both keyed by position. Entries without a position are dropped, as
/// the game cannot place them either.
pub fn split(
    list: impl IntoIterator<Item = Compound>,
) -> (HashMap<BlockPos, BlockEntity>, HashMap<BlockPos, Compound>) {
    let mut typed = HashMap::new();
    let mut raw = HashMap::new();
    for nbt in list {
        match BlockEntity::decode(&nbt) {
            Some((pos, entity)) => {
                raw.remove(&pos);
                typed.insert(pos, entity);
            }
            None => {
                if let Some(pos) = raw::position(&nbt) {
                    typed.remove(&pos);
                    raw.insert(pos, nbt);
                }
            }
        }
    }
    (typed, raw)
}

/// Drops stored block entities whose block is gone or of another type,
/// and gives every block that should have a block entity but lacks one
/// (as in chunks saved without one) a new one.
pub fn fill_missing(
    chunk: &Chunk,
    chunk_pos: ChunkPos,
    typed: &mut HashMap<BlockPos, BlockEntity>,
    raw: &mut HashMap<BlockPos, Compound>,
) {
    let block_type = |pos: &BlockPos| {
        if ChunkPos::of_block(*pos) != chunk_pos {
            return None;
        }
        chunk.get(pos.x, pos.y, pos.z).block().block_entity()
    };
    typed.retain(|pos, entity| block_type(pos) == Some(entity.kind.entity_type()));
    raw.retain(|pos, nbt| block_type(pos).is_some_and(|kind| raw::kind(nbt) == Some(kind)));
    for (x, y, z, state) in chunk.block_entity_blocks() {
        let pos = BlockPos::new(chunk_pos.x * 16 + x, y, chunk_pos.z * 16 + z);
        if typed.contains_key(&pos) || raw.contains_key(&pos) {
            continue;
        }
        let Some(kind) = state.block().block_entity() else {
            continue;
        };
        match create(kind, pos, &Components::default()) {
            Ok(entity) => {
                typed.insert(pos, entity);
            }
            Err(nbt) => {
                raw.insert(pos, nbt);
            }
        }
    }
}

/// A new block entity of `kind` at `pos` that took the components of the
/// item placing it: modeled (`Ok`) or as saved NBT (`Err`).
pub fn create(
    kind: BlockEntityType,
    pos: BlockPos,
    components: &Components,
) -> Result<BlockEntity, Compound> {
    let mut nbt = raw::new(kind, pos);
    raw::apply_components(kind, &mut nbt, components);
    match BlockEntity::decode(&nbt) {
        Some((_, entity)) => Ok(entity),
        None => Err(nbt),
    }
}

/// Ticks one brew takes at speed 1.
pub const BREW_TIME: i32 = 400;
/// What a stand saved without totals assumes.
pub const DEFAULT_FUEL_USES: i32 = 20;

/// A brewing stand's progress, saved with the game's keys.
#[derive(Debug, Clone, PartialEq)]
pub struct Brewing {
    pub brew_time: i32,
    pub total_brew_time: i32,
    pub fuel: i32,
    pub total_fuel: i32,
    pub speed_multiplier: f32,
    /// The reagent the running brew started with; not saved (a loaded
    /// stand takes whatever is in its reagent slot).
    pub ingredient: Option<Item>,
    /// The bottle slots last reflected in the block state.
    pub bottles: Option<[bool; 3]>,
}

impl Default for Brewing {
    fn default() -> Self {
        Self {
            brew_time: 0,
            total_brew_time: 0,
            fuel: 0,
            total_fuel: 0,
            speed_multiplier: 1.0,
            ingredient: None,
            bottles: None,
        }
    }
}

impl Brewing {
    /// The Set Container Property values, in the game's order.
    pub fn data(&self) -> [i32; brewing::DATA_COUNT] {
        [
            self.brew_time,
            self.fuel,
            self.total_brew_time,
            self.total_fuel,
        ]
    }
}

#[derive(Default)]
pub struct BrewTick {
    /// Whether the block entity must be saved.
    pub changed: bool,
    /// A brew finished: the stand plays its sound.
    pub brewed: bool,
    /// The `has_bottle_*` states to set, when they change.
    pub bottles: Option<[bool; 3]>,
    /// Remainders that did not fit back in their slot.
    pub drops: Vec<ItemStack>,
}

/// What a [`BlockEntityTicker`] sees of one block entity for one tick.
pub struct TickView<'a> {
    pub pos: BlockPos,
    pub state: tuff_vanilla_data::BlockState,
    pub kind: ContainerKind,
    pub items: &'a mut [Option<ItemStack>],
    pub data: &'a mut State,
    pub recipes: &'a RecipeSet,
    pub rng: &'a mut rand::rngs::StdRng,
    pub game_time: i64,
}

/// What a tick asks the region to apply afterwards.
#[derive(Default)]
pub struct TickOutcome {
    pub set_state: Option<tuff_vanilla_data::BlockState>,
    pub drops: Vec<ItemStack>,
    /// A level event (id, data) played at the block.
    pub level_event: Option<(i32, i32)>,
    /// Resend the block entity to clients tracking it.
    pub send_data: bool,
}

/// Ticks the block entities of one [`ContainerKind`].
pub trait BlockEntityTicker: Send + Sync {
    fn tick(&self, view: &mut TickView) -> TickOutcome;
}

/// Tickers by container kind. A kind without one keeps the built-in tick.
#[derive(Default, Clone)]
pub struct BlockEntityTickers {
    table: [Option<std::sync::Arc<dyn BlockEntityTicker>>; ContainerKind::COUNT],
    owners: [Option<&'static str>; ContainerKind::COUNT],
    frozen: bool,
}

impl BlockEntityTickers {
    pub fn register(
        &mut self,
        feature: &'static str,
        kind: ContainerKind,
        ticker: std::sync::Arc<dyn BlockEntityTicker>,
    ) -> Result<(), tuff_item::load::LoadError> {
        use tuff_item::load::{LoadError, LoadErrorKind};
        let slot = kind as usize;
        if self.frozen {
            return Err(LoadError {
                feature,
                kind: LoadErrorKind::Other(
                    "block_entity_tickers is frozen; registration is closed".into(),
                ),
            });
        }
        if let Some(first) = self.owners[slot] {
            return Err(LoadError {
                feature,
                kind: LoadErrorKind::Conflict {
                    registry: "block_entity_tickers",
                    key: format!("{kind:?}"),
                    first_registered_by: first,
                },
            });
        }
        self.table[slot] = Some(ticker);
        self.owners[slot] = Some(feature);
        Ok(())
    }

    pub fn freeze(&mut self) {
        self.frozen = true;
    }

    pub fn get(&self, kind: ContainerKind) -> Option<&std::sync::Arc<dyn BlockEntityTicker>> {
        self.table[kind as usize].as_ref()
    }
}

/// One furnace tick's outcome.
#[derive(Default)]
pub struct FurnaceTick {
    /// Whether the block entity must be saved.
    pub changed: bool,
    /// The `lit` state to set, when it flips.
    pub lit: Option<bool>,
    /// Fuel remainders that did not fit in the fuel slot.
    pub drops: Vec<ItemStack>,
}

#[cfg(test)]
#[path = "block_entity_tests.rs"]
mod tests;
