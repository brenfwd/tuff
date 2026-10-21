//! Allays: given an item, they gather matching dropped items and deliver
//! them to the player who gave it, or to a note block they heard play
//! lately. They dance while a jukebox plays nearby and split in two when
//! handed an amethyst shard mid-dance.
//!
//! The brain runs in `Species::tick` (liked player, liked note block and
//! the pickup cooldown are allay state goals cannot see); idle flying and
//! looking around are ordinary goals that wait while it runs an errand.
//! Known gaps: dropped items are matched by item only (potion contents are
//! not visible on item summaries), and the allay_drop_item_on_block
//! advancement trigger is not fired yet.

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::item::EquipmentSlot;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};
use tuff_vanilla_data::{Block, Item, tag};

use super::control::MoveStyle;
use super::goal::GoalSelector;
use super::goals::{Float, LookAtPlayer, Panic, RandomLookAround, RandomStroll};
use super::navigation::NavKind;
use super::{Damage, Mob, MobCore, MobSounds, Species};
use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::entity::{
    Effect, EntityInteraction, InteractOutcome, TickContext, take, take_number,
};
use tuff_core::world::game_event::{self, Emission, GameEvent};
use tuff_core::world::messages::HandEffect;
use tuff_core::world::region_api::RegionWorld;
use tuff_item::item::ItemStack;
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;

const DANCING: u8 = 16;
const CAN_DUPLICATE: u8 = 17;

const ITEM_SPEED: f64 = 1.75;
const DEPOSIT_SPEED: f64 = 2.25;
const PANIC_SPEED: f64 = 2.5;
const WANTED_ITEM_RANGE: f64 = 32.0;
const LIKED_PLAYER_RANGE: f64 = 64.0;
const NOTE_BLOCK_RANGE: i64 = 1024;
const NOTE_BLOCK_MEMORY: i32 = 600;
const PICKUP_COOLDOWN: i32 = 60;
const THROW_DISTANCE: f64 = 3.0;
const FOLLOW_CLOSE_ENOUGH: i32 = 4;
const FOLLOW_TOO_FAR: f64 = 16.0;
const DEPOSIT_CLOSE_ENOUGH: i32 = 3;
const THROW_VELOCITY: [f64; 3] = [0.2, 0.3, 0.2];
const ITEM_PICKUP_DELAY: u32 = 10;
const DUPLICATION_COOLDOWN: i64 = 6000;
const JUKEBOX_RANGE: i32 = 10;
const HEART_EVENT: i8 = 18;
const THROW_PITCHES: [f32; 16] = [
    0.5625, 0.625, 0.75, 0.9375, 1.0, 1.0, 1.125, 1.25, 1.5, 1.875, 2.0, 2.25, 2.5, 3.0, 3.75, 4.0,
];
/// While it runs an errand the idle stroll waits.
const BUSY: i32 = 1;

const MEMORIES: &str = "memories";
const LIKED_PLAYER: &str = "minecraft:liked_player";
const LIKED_NOTEBLOCK: &str = "minecraft:liked_noteblock";
const LIKED_NOTEBLOCK_COOLDOWN: &str = "minecraft:liked_noteblock_cooldown_ticks";
const ITEM_PICKUP_COOLDOWN: &str = "minecraft:item_pickup_cooldown_ticks";

/// A note block it heard, by dimension key and position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LikedNoteBlock {
    pub dimension: String,
    pub pos: BlockPos,
}

/// Where it delivers what it gathered.
#[derive(Debug, Clone, Copy)]
struct Deposit {
    /// The point it throws at: the liked player's eyes or the middle of
    /// the block above the note block.
    point: [f64; 3],
    entity: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Errand {
    Fetch,
    Deliver,
    Follow,
}

#[derive(Debug, Default)]
pub struct Allay {
    inventory: Option<ItemStack>,
    liked_player: Option<u128>,
    liked_noteblock: Option<LikedNoteBlock>,
    noteblock_cooldown: Option<i32>,
    pickup_cooldown: Option<i32>,
    jukebox: Option<BlockPos>,
    dancing: bool,
    duplication_cooldown: i64,
    errand: Option<Errand>,
    /// The block a path was last asked for, so moving targets re-path.
    heading: Option<BlockPos>,
    /// Picked-up stacks it could not keep, dropped on its next tick.
    rejected: Vec<ItemStack>,
    other_memories: Compound,
    listener: Option<Compound>,
}

fn count_down(cooldown: &mut Option<i32>) {
    match cooldown {
        Some(ticks) if *ticks > 0 => *ticks -= 1,
        _ => *cooldown = None,
    }
}

fn distance_sq(a: [f64; 3], b: [f64; 3]) -> f64 {
    tuff_core::world::distance_sq(a, b)
}

fn center(pos: BlockPos) -> [f64; 3] {
    [
        f64::from(pos.x) + 0.5,
        f64::from(pos.y) + 0.5,
        f64::from(pos.z) + 0.5,
    ]
}

fn block_is(cx: &TickContext, pos: BlockPos, block: Block) -> bool {
    (cx.block)(pos).is_some_and(|s| s.block() == block)
}

fn has_potion_mismatch(a: &ItemStack, b: &ItemStack) -> bool {
    a.components.potion_contents != b.components.potion_contents
}

impl Allay {
    pub fn liked_player(&self) -> Option<u128> {
        self.liked_player
    }

    pub fn liked_noteblock(&self) -> Option<&LikedNoteBlock> {
        self.liked_noteblock.as_ref()
    }

    pub fn is_dancing(&self) -> bool {
        self.dancing
    }

    pub fn inventory(&self) -> Option<&ItemStack> {
        self.inventory.as_ref()
    }

    fn can_duplicate(&self) -> bool {
        self.duplication_cooldown == 0
    }

    fn set_duplication_cooldown(&mut self, core: &mut MobCore, ticks: i64) {
        let could = self.can_duplicate();
        self.duplication_cooldown = ticks.max(0);
        if could != self.can_duplicate() {
            core.changed(CAN_DUPLICATE);
        }
    }

    fn set_dancing(&mut self, core: &mut MobCore, dancing: bool) {
        if core.no_ai || (dancing && core.is_panicking()) || self.dancing == dancing {
            return;
        }
        self.dancing = dancing;
        core.changed(DANCING);
    }

    /// A jukebox at `pos` started or stopped playing within earshot.
    pub fn jukebox_playing(&mut self, core: &mut MobCore, pos: BlockPos, playing: bool) {
        if playing {
            if !self.dancing {
                self.jukebox = Some(pos);
                self.set_dancing(core, true);
            }
        } else if self.jukebox.is_none_or(|at| at == pos) {
            self.jukebox = None;
            self.set_dancing(core, false);
        }
    }

    /// Whether a vibration from `pos` may reach it: anything while it likes
    /// no note block, else only that note block.
    pub fn can_hear(&self, core: &MobCore, dimension: &str, pos: BlockPos) -> bool {
        if core.no_ai {
            return false;
        }
        match &self.liked_noteblock {
            None => true,
            Some(liked) => liked.pos == pos && self.noteblock_within_range(core, liked, dimension),
        }
    }

    /// A note block played at `pos` and the vibration reached it.
    pub fn heard_note_block(&mut self, dimension: &str, pos: BlockPos) {
        let heard = LikedNoteBlock {
            dimension: dimension.to_owned(),
            pos,
        };
        match &self.liked_noteblock {
            None => {
                self.liked_noteblock = Some(heard);
                self.noteblock_cooldown = Some(NOTE_BLOCK_MEMORY);
            }
            Some(liked) if *liked == heard => self.noteblock_cooldown = Some(NOTE_BLOCK_MEMORY),
            Some(_) => {}
        }
    }

    fn noteblock_within_range(
        &self,
        core: &MobCore,
        liked: &LikedNoteBlock,
        dimension: &str,
    ) -> bool {
        let here = game_event::floor_pos(core.base.position);
        let close = |a: i32, b: i32| (i64::from(a) - i64::from(b)).abs() <= NOTE_BLOCK_RANGE;
        liked.dimension == dimension
            && close(liked.pos.x, here.x)
            && close(liked.pos.y, here.y)
            && close(liked.pos.z, here.z)
    }

    /// Where it would deliver now; forgets a note block it should no
    /// longer go to.
    fn deposit(&mut self, core: &MobCore, cx: &TickContext) -> Option<Deposit> {
        if let Some(liked) = self.liked_noteblock.clone() {
            let dimension = &cx.shared.dimension(cx.dimension).key;
            if self.noteblock_within_range(core, &liked, dimension)
                && block_is(cx, liked.pos, Block::NoteBlock)
                && self.noteblock_cooldown.is_some()
            {
                return Some(Deposit {
                    point: center(BlockPos::new(liked.pos.x, liked.pos.y + 1, liked.pos.z)),
                    entity: false,
                });
            }
            self.liked_noteblock = None;
        }
        let liked = self.liked_player?;
        let player = cx.players.iter().find(|p| p.uuid == liked)?;
        (distance_sq(player.position, core.base.position) < LIKED_PLAYER_RANGE * LIKED_PLAYER_RANGE)
            .then(|| Deposit {
                point: player.eye(),
                entity: true,
            })
    }

    fn holding(&self, core: &MobCore) -> bool {
        core.equipment.main_hand().is_some()
    }

    /// Room left for `item` in its one slot, if it wants it at all.
    fn room_for(&self, core: &MobCore, item: Item) -> Option<u8> {
        let held = core.equipment.main_hand()?;
        if held.item != item {
            return None;
        }
        match &self.inventory {
            None => Some(held.max_stack_size()),
            Some(stack) if stack.item == item => {
                Some(stack.max_stack_size().saturating_sub(stack.count())).filter(|n| *n > 0)
            }
            Some(_) => None,
        }
    }

    fn can_pick_up(&self, core: &MobCore, cx: &TickContext) -> bool {
        self.pickup_cooldown.is_none() && self.holding(core) && cx.shared.rule_bool("mob_griefing")
    }

    /// The nearest dropped item it wants and can see.
    fn wanted_item(&self, core: &MobCore, cx: &TickContext) -> Option<(i32, [f64; 3])> {
        if !cx.shared.rule_bool("mob_griefing") {
            return None;
        }
        let area = super::physics::inflate(core.bounds(), [32.0, 16.0, 32.0]);
        let mut items: Vec<_> = cx
            .entities
            .iter()
            .filter(|e| {
                e.item.is_some_and(|(item, count, _)| {
                    count > 0 && self.room_for(core, item).is_some()
                }) && super::physics::intersects(e.bounds, area)
            })
            .map(|e| {
                (
                    distance_sq(e.position, core.base.position),
                    e.id,
                    e.position,
                )
            })
            .collect();
        items.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        items.into_iter().find_map(|(d, id, at)| {
            let eye = [at[0], at[1] + 0.2125, at[2]];
            (d < WANTED_ITEM_RANGE * WANTED_ITEM_RANGE
                && !core.blind
                && core.can_see_point(eye, cx))
            .then_some((id, at))
        })
    }

    /// Takes the wanted items it touches.
    fn pick_up(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if !self.can_pick_up(core, cx) {
            return;
        }
        let reach = super::physics::inflate(core.bounds(), [1.0, 1.0, 1.0]);
        let mut taken: Option<(Item, u8)> = None;
        for e in cx.entities {
            let Some((item, count, true)) = e.item else {
                continue;
            };
            if count == 0 || !super::physics::intersects(e.bounds, reach) {
                continue;
            }
            let Some(mut room) = self.room_for(core, item) else {
                continue;
            };
            if let Some((kind, already)) = taken {
                if kind != item {
                    continue;
                }
                room = room.saturating_sub(already);
            }
            let amount = count.min(room);
            if amount == 0 {
                continue;
            }
            taken = Some((item, taken.map_or(0, |(_, n)| n) + amount));
            cx.effects.push(Effect::TakeItem {
                item: e.id,
                count: amount,
            });
        }
    }

    fn throw(&mut self, core: &MobCore, deposit: Deposit, cx: &mut TickContext) {
        let Some(stack) = self.inventory.as_mut() else {
            return;
        };
        let one = stack.take(1);
        if stack.count() == 0 {
            self.inventory = None;
        }
        let aim = [deposit.point[0], deposit.point[1] + 1.0, deposit.point[2]];
        if let (true, Some(player)) = (deposit.entity, self.liked_player) {
            cx.effects.push(Effect::Advancement {
                player,
                event: Box::new(
                    tuff_core::world::messages::AdvancementEvent::AllayDropItemOnBlock {
                        item: tuff_item::item::ItemSnapshot::of(&one),
                        location: tuff_core::world::predicate::LocationFacts {
                            position: [deposit.point[0], deposit.point[1] - 2.1, deposit.point[2]],
                            ..Default::default()
                        },
                    },
                ),
            });
        } else if let Some(player) = self.liked_player {
            cx.effects.push(Effect::Advancement {
                player,
                event: Box::new(
                    tuff_core::world::messages::AdvancementEvent::AllayDropItemOnBlock {
                        item: tuff_item::item::ItemSnapshot::of(&one),
                        location: tuff_core::world::predicate::LocationFacts {
                            position: [deposit.point[0], deposit.point[1] - 1.0, deposit.point[2]],
                            ..Default::default()
                        },
                    },
                ),
            });
        }
        throw_item(core, one, aim, THROW_VELOCITY, 0.2, cx);
        if cx.tick.is_multiple_of(7) && cx.rng.r#gen::<f64>() < 0.9 {
            let pitch = THROW_PITCHES[cx.rng.gen_range(0..THROW_PITCHES.len())];
            core.sound(SoundEvent::EntityAllayItemThrown, 1.0, pitch, cx);
        }
    }

    fn walk(
        &mut self,
        core: &mut MobCore,
        point: [f64; 3],
        reach: i32,
        speed: f64,
        cx: &mut TickContext,
    ) {
        core.look.set_look_at(point, 45.0, 90.0);
        let goal = game_event::floor_pos(point);
        if self.heading == Some(goal) && !core.navigation.is_done() {
            return;
        }
        self.heading = Some(goal);
        core.move_to_entity_reach(point, reach, speed, cx);
    }

    fn arrived(core: &MobCore, point: [f64; 3], reach: i32) -> bool {
        let here = game_event::floor_pos(core.base.position);
        let there = game_event::floor_pos(point);
        (here.x - there.x).abs() + (here.y - there.y).abs() + (here.z - there.z).abs() <= reach
    }

    fn brain(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        count_down(&mut self.noteblock_cooldown);
        count_down(&mut self.pickup_cooldown);
        if core.is_panicking() {
            self.set_dancing(core, false);
            self.errand = None;
            core.signal = 0;
            return;
        }
        let deposit = self.deposit(core, cx);
        let wanted = self
            .can_pick_up(core, cx)
            .then(|| self.wanted_item(core, cx))
            .flatten();
        if let Some(deposit) = deposit.filter(|_| self.inventory.is_some())
            && distance_sq(deposit.point, core.eye_position()) < THROW_DISTANCE * THROW_DISTANCE
        {
            self.throw(core, deposit, cx);
            self.pickup_cooldown = Some(PICKUP_COOLDOWN);
        }
        let errand = if wanted.is_some() && self.pickup_cooldown.is_none() {
            Some(Errand::Fetch)
        } else if deposit.is_some() && self.inventory.is_some() {
            Some(Errand::Deliver)
        } else if let Some(deposit) = deposit
            && wanted.is_none()
            && (self.errand == Some(Errand::Follow)
                || distance_sq(core.base.position, deposit.point)
                    >= FOLLOW_TOO_FAR * FOLLOW_TOO_FAR)
        {
            Some(Errand::Follow)
        } else {
            None
        };
        if errand != self.errand {
            self.heading = None;
            if errand.is_none() {
                core.navigation.stop();
            }
        }
        self.errand = errand;
        match errand {
            Some(Errand::Fetch) => {
                let (_, at) = wanted.expect("fetching a wanted item");
                self.walk(core, at, 0, ITEM_SPEED, cx);
            }
            Some(Errand::Deliver) => {
                let deposit = deposit.expect("delivering to a deposit");
                self.walk(core, deposit.point, DEPOSIT_CLOSE_ENOUGH, DEPOSIT_SPEED, cx);
            }
            Some(Errand::Follow) => {
                let deposit = deposit.expect("following a deposit");
                if Self::arrived(core, deposit.point, FOLLOW_CLOSE_ENOUGH) {
                    self.errand = None;
                    self.heading = None;
                    core.navigation.stop();
                } else {
                    let reach = if deposit.entity {
                        FOLLOW_CLOSE_ENOUGH
                    } else {
                        1
                    };
                    self.walk(core, deposit.point, reach, DEPOSIT_SPEED, cx);
                }
            }
            None => {}
        }
        core.signal = if self.errand.is_some() { BUSY } else { 0 };
    }

    fn should_stop_dancing(&self, core: &MobCore, cx: &TickContext) -> bool {
        let Some(pos) = self.jukebox else {
            return true;
        };
        let range = f64::from(JUKEBOX_RANGE);
        distance_sq(center(pos), core.base.position) >= range * range
            || !block_is(cx, pos, Block::Jukebox)
    }

    fn duplicate(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let id = cx.shared.next_entity_id();
        let Some(mut twin) =
            cx.shared
                .entity_types
                .create(EntityType::Allay, id, core.base.position, core.base.yaw)
        else {
            return;
        };
        if let Some(mob) = twin.downcast_mut::<Mob<Allay>>() {
            mob.core.persistence_required = true;
            mob.species
                .set_duplication_cooldown(&mut mob.core, DUPLICATION_COOLDOWN);
        }
        self.set_duplication_cooldown(core, DUPLICATION_COOLDOWN);
        cx.effects.push(Effect::Spawn(twin));
    }
}

/// Throws `stack` from its hand toward `aim`, as mobs hand items over.
fn throw_item(
    core: &MobCore,
    stack: ItemStack,
    aim: [f64; 3],
    speed: [f64; 3],
    hand_below_eye: f64,
    cx: &mut TickContext,
) {
    let [x, y, z] = core.base.position;
    let (dx, dy, dz) = (aim[0] - x, aim[1] - y, aim[2] - z);
    let length = (dx * dx + dy * dy + dz * dz).sqrt();
    let velocity = if length < 1.0e-4 {
        [0.0; 3]
    } else {
        [
            dx / length * speed[0],
            dy / length * speed[1],
            dz / length * speed[2],
        ]
    };
    cx.effects.push(Effect::DropItem {
        stack,
        position: [x, core.eye_position()[1] - hand_below_eye, z],
        velocity,
        pickup_delay: ITEM_PICKUP_DELAY,
    });
}

fn memory(value: impl Into<Tag>) -> Compound {
    Compound::new().with("value", value)
}

fn memory_value<'a>(memories: &'a Compound, key: &str) -> Option<&'a Tag> {
    memories.get(key)?.as_compound()?.get("value")
}

impl Species for Allay {
    fn take_items(&mut self) -> Vec<ItemStack> {
        self.inventory.take().into_iter().collect()
    }

    fn create(_rng: &mut StdRng) -> Self {
        Self::default()
    }

    fn configure(core: &mut MobCore) {
        core.flying = true;
        core.navigation.kind = NavKind::Fly;
        core.navigation.can_float = true;
        core.move_style = MoveStyle::Flying {
            hovers: true,
            max_turn: 20.0,
        };
        let mut goals = GoalSelector::default();
        goals.add(0, Float);
        goals.add(1, Panic::new(PANIC_SPEED));
        goals.add(
            2,
            RandomStroll::air_and_water(1.0, 30).unless(|core, _| core.signal == BUSY),
        );
        goals.add(3, LookAtPlayer::new(6.0));
        goals.add(4, RandomLookAround::default());
        core.goals = goals;
    }

    fn load(&mut self, core: &mut MobCore, nbt: &mut Compound) {
        if let Some(list) = take(nbt, "Inventory", |t| t.as_list().map(<[Tag]>::to_vec)) {
            self.inventory = list
                .iter()
                .find_map(|t| ItemStack::from_storage(t.as_compound()?));
        }
        let cooldown = take_number(nbt, "DuplicationCooldown").map_or(0, |v| v as i64);
        self.set_duplication_cooldown(core, cooldown);
        self.listener = take(nbt, "listener", |t| t.as_compound().cloned());
        let Some(brain) = take(nbt, "Brain", |t| t.as_compound().cloned()) else {
            return;
        };
        let mut memories = brain.get_compound(MEMORIES).cloned().unwrap_or_default();
        self.liked_player = memory_value(&memories, LIKED_PLAYER).and_then(Tag::as_uuid);
        self.liked_noteblock = memory_value(&memories, LIKED_NOTEBLOCK)
            .and_then(Tag::as_compound)
            .and_then(|global| {
                let pos = global.get("pos")?.as_int_array()?;
                let &[x, y, z] = pos else {
                    return None;
                };
                Some(LikedNoteBlock {
                    dimension: global.get_str("dimension")?.to_owned(),
                    pos: BlockPos::new(x, y, z),
                })
            });
        let ticks = |memories: &Compound, key: &str| {
            memory_value(memories, key)
                .and_then(tuff_core::world::entity::number)
                .map(|v| v as i32)
        };
        self.noteblock_cooldown = ticks(&memories, LIKED_NOTEBLOCK_COOLDOWN);
        self.pickup_cooldown = ticks(&memories, ITEM_PICKUP_COOLDOWN);
        for key in [
            LIKED_PLAYER,
            LIKED_NOTEBLOCK,
            LIKED_NOTEBLOCK_COOLDOWN,
            ITEM_PICKUP_COOLDOWN,
        ] {
            memories.remove(key);
        }
        self.other_memories = memories;
    }

    fn save(&self, _core: &MobCore, out: &mut Compound) {
        out.insert(
            "Inventory",
            Tag::List(
                self.inventory
                    .iter()
                    .map(|stack| Tag::Compound(stack.to_nbt()))
                    .collect(),
            ),
        );
        out.insert("DuplicationCooldown", self.duplication_cooldown);
        if let Some(listener) = &self.listener {
            out.insert("listener", listener.clone());
        }
        let mut memories = self.other_memories.clone();
        if let Some(uuid) = self.liked_player {
            memories.insert(LIKED_PLAYER, memory(Tag::uuid(uuid)));
        }
        if let Some(liked) = &self.liked_noteblock {
            let pos = Tag::IntArray(vec![liked.pos.x, liked.pos.y, liked.pos.z]);
            memories.insert(
                LIKED_NOTEBLOCK,
                memory(
                    Compound::new()
                        .with("dimension", liked.dimension.as_str())
                        .with("pos", pos),
                ),
            );
        }
        if let Some(ticks) = self.noteblock_cooldown {
            memories.insert(LIKED_NOTEBLOCK_COOLDOWN, memory(ticks));
        }
        if let Some(ticks) = self.pickup_cooldown {
            memories.insert(ITEM_PICKUP_COOLDOWN, memory(ticks));
        }
        out.insert("Brain", Compound::new().with(MEMORIES, memories));
    }

    fn metadata(&self, _core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![
            (DANCING, MetadataValue::Boolean(self.dancing)),
            (CAN_DUPLICATE, MetadataValue::Boolean(self.can_duplicate())),
        ]
    }

    fn sounds(&self, core: &MobCore) -> MobSounds {
        MobSounds {
            ambient: Some(if self.holding(core) {
                SoundEvent::EntityAllayAmbientWithItem
            } else {
                SoundEvent::EntityAllayAmbientWithoutItem
            }),
            hurt: Some(SoundEvent::EntityAllayHurt),
            death: Some(SoundEvent::EntityAllayDeath),
            step: None,
            volume: 0.4,
        }
    }

    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        for stack in std::mem::take(&mut self.rejected) {
            throw_item(core, stack, core.base.position, [0.3; 3], 0.3, cx);
        }
        if core.is_alive() && !core.no_ai {
            self.brain(core, cx);
        }
    }

    fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.pick_up(core, cx);
        if core.tick_count.is_multiple_of(10) {
            core.heal(1.0);
        }
        if self.dancing && core.tick_count.is_multiple_of(20) && self.should_stop_dancing(core, cx)
        {
            self.set_dancing(core, false);
            self.jukebox = None;
        }
        if self.duplication_cooldown > 0 {
            self.set_duplication_cooldown(core, self.duplication_cooldown - 1);
        }
    }

    fn interact(
        &mut self,
        core: &mut MobCore,
        interaction: &EntityInteraction,
        cx: &mut TickContext,
    ) -> InteractOutcome {
        let stack = interaction.stack.as_ref();
        if let Some(stack) = stack
            && self.dancing
            && self.can_duplicate()
            && tuff_datapack::tags::is(tag::item::key::DUPLICATES_ALLAYS, stack.item)
        {
            self.duplicate(core, cx);
            core.entity_event(HEART_EVENT, cx);
            core.sound(SoundEvent::BlockAmethystBlockChime, 2.0, 1.0, cx);
            return Some(HandEffect::Consume(1));
        }
        if let Some(stack) = stack
            && stack.item == Item::NameTag
            && stack.components.custom_name.is_some()
        {
            return None;
        }
        match (core.equipment.main_hand().is_some(), stack) {
            (false, Some(stack)) => {
                core.equipment
                    .set(EquipmentSlot::Mainhand, Some(stack.dup_count_unchecked(1)));
                core.sound(SoundEvent::EntityAllayItemGiven, 2.0, 1.0, cx);
                self.liked_player = Some(interaction.player_uuid);
                Some(HandEffect::Consume(1))
            }
            (true, None) if !interaction.off_hand => {
                let held = core.equipment.take(EquipmentSlot::Mainhand)?;
                core.sound(SoundEvent::EntityAllayItemTaken, 2.0, 1.0, cx);
                core.swing(cx);
                if let Some(kept) = self.inventory.take() {
                    throw_item(core, kept, core.base.position, [0.3; 3], 0.3, cx);
                }
                self.liked_player = None;
                Some(HandEffect::Replace(Some(Box::new(held))))
            }
            _ => None,
        }
    }

    fn took_item(&mut self, core: &mut MobCore, stack: ItemStack) {
        let fits = core
            .equipment
            .main_hand()
            .is_some_and(|held| held.item == stack.item && !has_potion_mismatch(held, &stack));
        match &mut self.inventory {
            None if fits => self.inventory = Some(stack),
            Some(kept) if fits && kept.stacks_with(&stack) => {
                let room = kept.max_stack_size().saturating_sub(kept.count());
                let mut stack = stack;
                kept.transfer(&mut stack, room);
                if stack.count() > 0 {
                    self.rejected.push(stack);
                }
            }
            _ => self.rejected.push(stack),
        }
    }

    fn before_hurt(&mut self, _core: &mut MobCore, damage: &Damage, _cx: &mut TickContext) -> bool {
        damage
            .player
            .is_none_or(|(_, uuid)| Some(uuid) != self.liked_player)
    }

    fn died(&mut self, core: &mut MobCore, _damage: &Damage, cx: &mut TickContext) {
        if let Some(stack) = self.inventory.take() {
            core.drop_item(stack, 0.0, cx);
        }
        if core
            .equipment
            .main_hand()
            .is_some_and(|held| !super::equipment::prevents_drop(held))
            && let Some(held) = core.equipment.take(EquipmentSlot::Mainhand)
        {
            core.drop_item(held, 0.0, cx);
        }
    }

    fn remove_when_far_away(&self) -> bool {
        false
    }
}

fn allay_ids(world: &dyn RegionWorld) -> Vec<i32> {
    world.entity_ids_of(&[EntityType::Allay])
}

fn allay_mut(world: &mut dyn RegionWorld, id: i32) -> Option<&mut Mob<Allay>> {
    world.entity_mut(id)?.entity.downcast_mut::<Mob<Allay>>()
}

/// Jukebox starts and stops reach allays within ten blocks of their eyes,
/// at once and through walls (they are not vibrations).
pub(crate) fn hear_jukebox(world: &mut dyn RegionWorld, e: &Emission) {
    let playing = match e.event {
        GameEvent::JukeboxPlay => true,
        GameEvent::JukeboxStopPlay => false,
        _ => return,
    };
    let origin = e.block_pos();
    for id in allay_ids(world) {
        let Some(mob) = allay_mut(world, id) else {
            continue;
        };
        let eye = game_event::floor_pos(mob.core.eye_position());
        let (dx, dy, dz) = (origin.x - eye.x, origin.y - eye.y, origin.z - eye.z);
        if dx * dx + dy * dy + dz * dz > JUKEBOX_RANGE * JUKEBOX_RANGE {
            continue;
        }
        mob.species.jukebox_playing(&mut mob.core, origin, playing);
    }
}

/// Whether the allay `id` takes a vibration from `pos`.
pub(crate) fn can_hear(world: &dyn RegionWorld, id: i32, pos: BlockPos) -> bool {
    let dimension = &world.dimension_info().key;
    world
        .entity(id)
        .and_then(|t| t.entity.downcast_ref::<Mob<Allay>>())
        .is_some_and(|mob| mob.species.can_hear(&mob.core, dimension, pos))
}

/// A vibration of `event` from `pos` reached the allay `id`.
pub(crate) fn heard(world: &mut dyn RegionWorld, id: i32, event: GameEvent, pos: BlockPos) {
    if event != GameEvent::NoteBlockPlay {
        return;
    }
    let dimension = world.dimension_info().key.clone();
    if let Some(mob) = allay_mut(world, id) {
        mob.species.heard_note_block(&dimension, pos);
    }
}

/// Registers the allay's loader. Allays never spawn naturally; they come
/// from outposts and mansions.
pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Allay, super::load::<Allay>);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::with_context;
    use tuff_item::item::MintReason;

    fn interaction(player: u128, item: Option<Item>, off_hand: bool) -> EntityInteraction {
        EntityInteraction {
            player: 1,
            player_uuid: player,
            position: [0.0; 3],
            location: [0.0; 3],
            stack: item.map(|item| ItemStack::mint(item, 5, MintReason::Test)),
            off_hand,
            creative: false,
            sneaking: false,
            riding: false,
            hero: None,
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn allay_data_indices_match_the_dumped_layout() {
        let layout = EntityType::Allay.data_layout();
        assert_eq!(layout.len(), CAN_DUPLICATE as usize + 1);
        for index in [DANCING, CAN_DUPLICATE] {
            assert_eq!(
                layout[index as usize],
                MetadataValue::Boolean(false).type_id() as u8
            );
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn giving_an_item_makes_it_like_the_player_and_taking_it_back_forgets_them() {
        with_context(|core, cx| {
            Allay::configure(core);
            let mut allay = Allay::create(cx.rng);
            let effect = allay.interact(core, &interaction(9, Some(Item::Cobblestone), false), cx);
            assert!(matches!(effect, Some(HandEffect::Consume(1))));
            assert_eq!(
                core.equipment.main_hand().map(|s| (s.item, s.count())),
                Some((Item::Cobblestone, 1))
            );
            assert_eq!(allay.liked_player(), Some(9));
            assert!(
                allay
                    .interact(core, &interaction(9, Some(Item::Dirt), false), cx)
                    .is_none(),
                "a full hand takes nothing more"
            );
            assert!(
                allay
                    .interact(core, &interaction(9, None, true), cx)
                    .is_none(),
                "only the main hand takes the item back"
            );
            allay.inventory = Some(ItemStack::mint(Item::Cobblestone, 3, MintReason::Test));
            let effect = allay.interact(core, &interaction(9, None, false), cx);
            assert!(matches!(
                effect,
                Some(HandEffect::Replace(Some(ref stack))) if stack.item == Item::Cobblestone
            ));
            assert!(core.equipment.main_hand().is_none());
            assert_eq!(allay.liked_player(), None);
            assert!(allay.inventory().is_none());
            assert!(
                cx.effects.iter().any(|e| matches!(
                    e,
                    Effect::DropItem { stack, .. } if stack.item == Item::Cobblestone && stack.count() == 3
                )),
                "what it gathered falls out"
            );
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_liked_player_cannot_hurt_it() {
        with_context(|core, cx| {
            let mut allay = Allay {
                liked_player: Some(9),
                ..Allay::default()
            };
            let mut damage = Damage::of(&tuff_vanilla_data::damage_type::PLAYER_ATTACK);
            damage.player = Some((1, 9));
            assert!(!allay.before_hurt(core, &damage, cx));
            damage.player = Some((2, 10));
            assert!(allay.before_hurt(core, &damage, cx));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn it_likes_the_first_note_block_it_hears_and_then_only_that_one() {
        with_context(|core, _cx| {
            let mut allay = Allay::default();
            let first = BlockPos::new(1, 2, 3);
            let other = BlockPos::new(4, 2, 3);
            assert!(allay.can_hear(core, "minecraft:overworld", other));
            allay.heard_note_block("minecraft:overworld", first);
            allay.noteblock_cooldown = Some(5);
            allay.heard_note_block("minecraft:overworld", other);
            assert_eq!(allay.liked_noteblock().map(|n| n.pos), Some(first));
            assert_eq!(allay.noteblock_cooldown, Some(5));
            assert!(allay.can_hear(core, "minecraft:overworld", first));
            assert!(!allay.can_hear(core, "minecraft:overworld", other));
            assert!(!allay.can_hear(core, "minecraft:the_nether", first));
            allay.heard_note_block("minecraft:overworld", first);
            assert_eq!(allay.noteblock_cooldown, Some(NOTE_BLOCK_MEMORY));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn cooldowns_count_down_to_zero_then_lapse() {
        let mut cooldown = Some(1);
        count_down(&mut cooldown);
        assert_eq!(cooldown, Some(0));
        count_down(&mut cooldown);
        assert_eq!(cooldown, None);
    }
}
