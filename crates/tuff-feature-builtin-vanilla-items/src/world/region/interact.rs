//! Right-click reactions of blocks and of items used on blocks, and the
//! scheduled block ticks (button releases) they start. The rules are pure
//! functions in [`tuff_core::world::interaction`]; this applies them.

use crate::world::session::item_use::ItemUseExt as _;
use rand::Rng;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::block_transformer::{DropStrategy, TransformParticle, TransformType};
use tuff_vanilla_data::fluid::Fluid;
use tuff_vanilla_data::registry::{BlockEntityType, DataComponentType, SoundEvent};
use tuff_vanilla_data::tag::item as item_tags;
use tuff_vanilla_data::{Block, BlockKind, BlockState, Item};

use crate::world::interaction::{self, Pour};
use crate::world::items;
use crate::world::region::shelves::Shelves as _;
use tuff_core::protocol::play;
use tuff_core::world::block_entity::{self, sign};
use tuff_core::world::explosion::{BlockInteraction, Explosion};
use tuff_core::world::messages::{
    ActionOutcome, AnchorUse, BedUse, BucketUse, HandEffect, Igniter, Place, PlayerEvent,
    PlayerHandle, PlayerId, RegionInternal, RegionRequest, RespawnSpot, SessionTask,
};
use tuff_item::item::{ItemStack, LodestoneTracker, MintReason};
use tuff_item::text::TextComponent;
use tuff_vanilla_data::block::{BedRuleAttribute, Part};
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;

/// Where a thrown eye of ender flies, as an eye-target task carries it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EyeTarget {
    pub off_hand: bool,
    pub target: Option<BlockPos>,
}

/// The explosion a bed makes where it cannot be used.
const BED_EXPLOSION_POWER: f32 = 5.0;
/// The explosion a charged respawn anchor makes when used where anchors do
/// not work, whatever its charge.
const ANCHOR_EXPLOSION_POWER: f32 = 5.0;
/// A respawn anchor's most charges.
const ANCHOR_MAX_CHARGES: u8 = 4;
use tuff_core::world::game_event::{Context, GameEvent, Source};
use tuff_core::world::region_api::{BlockAction, RegionTask, RegionWorld};
use tuff_core::world::{framed, packets};
use tuff_vanilla_common::placement;
use tuff_world_model::pos::ChunkPos;

/// The dragon egg teleport particle level event.
const DRAGON_EGG_TELEPORT: i32 = 2010;
const AIR: BlockState = Block::Air.default_state();
use tuff_core::world::raycast::{self, FluidMode};

/// Block and item interactions over a region: what a click does to a
/// block, tools, buckets, beds, respawn anchors and note blocks.
pub trait Interact: RegionWorld + Rest {
    /// The block's own reaction to a click with any item, after containers
    /// and stations: doors, trapdoors, gates, buttons, levers, note blocks
    /// and bells. `None` lets the held item act.
    fn use_block(&mut self, place: &Place, state: BlockState) -> Option<ActionOutcome> {
        let pos = place.target;
        let except = Some(place.player.id);
        let interacted = ActionOutcome::Interacted {
            off_hand: place.off_hand,
        };
        let block = state.block();
        if block.block_entity().is_some_and(sign::is_sign) {
            return self.use_sign(place, state);
        }
        if block == Block::FlowerPot || potted_content(block).is_some() {
            return Some(self.use_flower_pot(place, state));
        }
        if block == Block::Light {
            if place.game_master {
                let level: u8 = state.get("level")?.parse().ok()?;
                let next = state.with("level", &((level + 1) % 16).to_string())?;
                self.set_block_flags(pos, next, tuff_core::world::redstone::UPDATE_CLIENTS);
            }
            return Some(interacted);
        }
        if block == Block::SweetBerryBush {
            return self.harvest_sweet_berry_bush(pos, state, place.off_hand);
        }
        if matches!(block, Block::CaveVines | Block::CaveVinesPlant) {
            return self.harvest_cave_vines(pos, state, place.off_hand);
        }
        if block.name().ends_with("copper_golem_statue") {
            let axe = place
                .held
                .is_some_and(|item| tuff_datapack::tags::is(item_tags::key::AXES, item));
            return if axe {
                None
            } else {
                self.cycle_statue_pose(place, state)
            };
        }
        if let Some((next, sound)) = interaction::toggle(state, placement::from_yaw(place.yaw)) {
            self.set_block_flags(pos, next, tuff_core::world::redstone::UPDATE_CLIENTS);
            let pitch = self.rng().gen_range(0.9..1.0);
            self.play_sound(sound, pos, 1.0, pitch, except);
            return Some(interacted);
        }
        let adjusts = matches!(
            block,
            Block::RedstoneWire | Block::Repeater | Block::Comparator | Block::DaylightDetector
        );
        if (place.may_build || !adjusts) && self.use_redstone(pos, state) {
            return Some(interacted);
        }
        if block.kind() == BlockKind::NoteBlock {
            let on_top = place.face == tuff_world_model::Face::Up
                && place.held.is_some_and(|item| {
                    tuff_datapack::tags::is(item_tags::key::NOTEBLOCK_TOP_INSTRUMENTS, item)
                });
            if on_top {
                return None;
            }
            let next = interaction::next_note(state)?;
            self.put_block(pos, next);
            place.player.send(PlayerEvent::AwardStat(
                tuff_vanilla_data::registry::CustomStat::TuneNoteblock,
                1,
            ));
            if self.play_note(pos) {
                self.game_event(
                    tuff_core::world::game_event::GameEvent::NoteBlockPlay,
                    pos.center(),
                    tuff_core::world::game_event::Context::default(),
                );
            }
            return Some(interacted);
        }
        if block.bed().is_some() {
            return self.use_bed(place, state);
        }
        if block == Block::RespawnAnchor {
            return self.use_respawn_anchor(place, state);
        }
        if block == Block::DecoratedPot {
            return self.use_decorated_pot(place);
        }
        if block == Block::ChiseledBookshelf {
            return self.use_chiseled_bookshelf(place, state);
        }
        if tuff_vanilla_data::tag::block::WOODEN_SHELVES.contains(&block) {
            return self.use_shelf(place, state);
        }
        if block == Block::Jukebox {
            return self.use_jukebox(place, state);
        }
        if block == Block::Composter {
            return self.use_composter(place, state);
        }
        if block == Block::DragonEgg {
            self.teleport_dragon_egg(pos, state);
            return Some(interacted);
        }
        if matches!(
            block,
            Block::Cauldron
                | Block::WaterCauldron
                | Block::LavaCauldron
                | Block::PowderSnowCauldron
        ) {
            return self.use_cauldron(place, state);
        }
        if let Some(outcome) = self.extinguish_candle(place, state) {
            return Some(outcome);
        }
        if block == Block::Bell {
            let face = placement::facing(place.face);
            if !interaction::bell_struck(state, face, place.cursor[1] as f64) {
                return None;
            }
            self.ring_bell(pos, Some(face));
            place
                .player
                .send(tuff_core::world::messages::PlayerEvent::AwardStat(
                    tuff_vanilla_data::registry::CustomStat::BellRing,
                    1,
                ));
            return Some(interacted);
        }
        None
    }

    /// A click on a decorated pot: one item of the held stack goes in if
    /// the pot is empty or holds the same item below a full stack; anything
    /// else wobbles it in refusal.
    fn use_decorated_pot(&mut self, place: &Place) -> Option<ActionOutcome> {
        use tuff_item::item::{ItemStack, MintReason};
        let pos = place.target;
        let interacted = ActionOutcome::Interacted {
            off_hand: place.off_hand,
        };
        let inserted = place.held.and_then(|item| {
            let held = ItemStack::mint_with(item, 1, &place.components, MintReason::Transform);
            let stored = self
                .raw_block_entity(pos)?
                .get_compound("item")
                .and_then(ItemStack::picture_from_nbt);
            let old = stored.as_ref().map(tuff_item::item::ItemSnapshot::of);
            let next = match stored {
                None => held,
                Some(stored) => {
                    if stored.count() >= stored.item.max_stack_size() {
                        held.burn(tuff_item::item::BurnReason::Template);
                        return None;
                    }
                    match ItemStack::merge(stored, held) {
                        Ok(next) => next,
                        Err((_, held)) => {
                            held.burn(tuff_item::item::BurnReason::Template);
                            return None;
                        }
                    }
                }
            };
            let ratio = f32::from(next.count()) / next.item.max_stack_size() as f32;
            self.raw_block_entity_mut(pos)?
                .insert("item", next.to_nbt());
            self.report_container_edit(
                tuff_core::world::region_api::ContainerHolder::Block(pos),
                &[(0, old.as_deref(), Some(&next))],
            );
            tuff_item::item::conservation::release(next);
            Some((item, ratio))
        });
        let Some((item, ratio)) = inserted else {
            self.block_event(pos, play::block_event::DECORATED_POT_WOBBLE, 1);
            self.play_sound(SoundEvent::BlockDecoratedPotInsertFail, pos, 1.0, 1.0, None);
            return Some(interacted);
        };
        self.block_event(pos, play::block_event::DECORATED_POT_WOBBLE, 0);
        let plume = framed(&play::Particles {
            particle: tuff_vanilla_data::registry::ParticleType::DustPlume.id(),
            position: [
                f64::from(pos.x) + 0.5,
                f64::from(pos.y) + 1.2,
                f64::from(pos.z) + 0.5,
            ],
            spread: [0.0; 3],
            speed: 0.0,
            count: 7,
            alternative: false,
        });
        self.send_to_chunk(ChunkPos::of_block(pos), None, packets([plume]));
        self.play_sound(
            SoundEvent::BlockDecoratedPotInsert,
            pos,
            1.0,
            0.7 + 0.5 * ratio,
            None,
        );
        Some(ActionOutcome::Used {
            off_hand: place.off_hand,
            item,
            effect: HandEffect::Consume(1),
        })
    }

    /// A composter click: a compostable item goes in below level 7 (and is
    /// refused but still counts as used at 7); otherwise a ripe composter
    /// gives up its bone meal.
    fn use_composter(&mut self, place: &Place, state: BlockState) -> Option<ActionOutcome> {
        use tuff_core::world::item_port::{PortAccess, Porter};
        use tuff_vanilla_common::composter;
        let level = composter::level(state)?;
        let port = self.shared().item_ports.get(state.block())?.clone();
        let at = PortAccess {
            pos: place.target,
            state,
            face: None,
            by: Porter::Player(Some(place.player.id)),
        };
        let held = place
            .held
            .map(|item| {
                tuff_item::item::ItemStack::mint_with(
                    item,
                    1,
                    &place.components,
                    tuff_item::item::MintReason::Template,
                )
            })
            .and_then(|stack| {
                if composter::compostable(&stack).is_some() {
                    Some(stack)
                } else {
                    stack.burn(tuff_item::item::BurnReason::Template);
                    None
                }
            });
        if let Some(stack) = held {
            let item = stack.item;
            let accepted = if level < composter::READY {
                port.offer(self.as_dyn(), &at, stack)
                    .map_err(|refused| refused.burn(tuff_item::item::BurnReason::Template))
                    .is_ok()
            } else {
                stack.burn(tuff_item::item::BurnReason::Template);
                false
            };
            if level >= composter::READY {
                if level > composter::READY {
                    return None;
                }
                port.take(self.as_dyn(), &at);
                return Some(ActionOutcome::Interacted {
                    off_hand: place.off_hand,
                });
            }
            if !accepted {
                return Some(ActionOutcome::Interacted {
                    off_hand: place.off_hand,
                });
            }
            return Some(ActionOutcome::Used {
                off_hand: place.off_hand,
                item,
                effect: HandEffect::Consume(1),
            });
        }
        if level == composter::READY {
            port.take(self.as_dyn(), &at);
            return Some(ActionOutcome::Interacted {
                off_hand: place.off_hand,
            });
        }
        None
    }

    /// The dragon egg hops to a random nearby spot with air above solid
    /// ground inside the border and build height, if one turns up in 1000
    /// tries.
    fn teleport_dragon_egg(&mut self, pos: BlockPos, state: BlockState) {
        let (min_y, height) = {
            let dimension = self.dimension();
            (dimension.min_y, dimension.height as i32)
        };
        for _ in 0..1000 {
            let rng = self.rng();
            let offset = [
                rng.gen_range(0..16) - rng.gen_range(0..16),
                rng.gen_range(0..8) - rng.gen_range(0..8),
                rng.gen_range(0..16) - rng.gen_range(0..16),
            ];
            let test = BlockPos::new(pos.x + offset[0], pos.y + offset[1], pos.z + offset[2]);
            if test.y < min_y
                || test.y >= min_y + height
                || !self.dimension_info().block_within_border(test)
            {
                continue;
            }
            let air = |at: BlockPos| self.block(at).is_some_and(tuff_world_model::chunk::is_air);
            if !air(test) || air(BlockPos::new(test.x, test.y - 1, test.z)) {
                continue;
            }
            let packed = ((offset[0] + 16) & 255) << 16
                | ((offset[1] + 8) & 255) << 8
                | ((offset[2] + 16) & 255);
            self.broadcast_level_event(DRAGON_EGG_TELEPORT, pos, packed);
            self.put_block(test, state);
            self.put_block(pos, AIR);
            return;
        }
    }

    /// An empty hand on a lit candle (or on the candle of a candle cake,
    /// above the cake) blows it out.
    fn extinguish_candle(&mut self, place: &Place, state: BlockState) -> Option<ActionOutcome> {
        let block = state.block();
        let cake = match block.kind() {
            BlockKind::Candle => false,
            BlockKind::Cake if block != Block::Cake => true,
            _ => return None,
        };
        if place.held.is_some()
            || !place.may_build
            || state.lit() != Some(true)
            || (cake && place.cursor[1] <= 0.5)
        {
            return None;
        }
        let pos = place.target;
        let unlit = state.with_lit(false)?;
        self.put_block(pos, unlit);
        self.play_sound(SoundEvent::BlockCandleExtinguish, pos, 1.0, 1.0, None);
        self.game_event(
            GameEvent::BlockChange,
            pos.center(),
            Context {
                source: Some(Source::player(place.player.id)),
                affected: None,
            },
        );
        Some(ActionOutcome::Interacted {
            off_hand: place.off_hand,
        })
    }

    fn cycle_statue_pose(&mut self, place: &Place, state: BlockState) -> Option<ActionOutcome> {
        let pos = place.target;
        let next = match state.get("copper_golem_pose")? {
            "standing" => "sitting",
            "sitting" => "running",
            "running" => "star",
            _ => "standing",
        };
        let next = state.with("copper_golem_pose", next)?;
        self.play_sound(
            SoundEvent::EntityCopperGolemBecomeStatue,
            pos,
            1.0,
            1.0,
            None,
        );
        self.put_block(pos, next);
        self.game_event(
            GameEvent::BlockChange,
            pos.center(),
            Context {
                source: Some(Source::player(place.player.id)),
                affected: Some(next),
            },
        );
        Some(ActionOutcome::Interacted {
            off_hand: place.off_hand,
        })
    }

    /// Lowers a layered cauldron's fill by one level, emptying it at zero.
    fn lower_cauldron(&mut self, pos: BlockPos, state: BlockState) {
        let next = match state.level() {
            Some(level) if level > 1 => state.with_level(level - 1).unwrap_or(state),
            _ => Block::Cauldron.default_state(),
        };
        self.put_block(pos, next);
        self.game_event(
            GameEvent::BlockChange,
            pos.center(),
            Context {
                source: None,
                affected: Some(next),
            },
        );
    }

    /// An item used on a cauldron: buckets and bottles fill and empty it,
    /// and water washes banners, dyed shulker boxes and dyed armor.
    fn use_cauldron(&mut self, place: &Place, state: BlockState) -> Option<ActionOutcome> {
        use tuff_vanilla_data::registry::CustomStat;
        let item = place.held?;
        let pos = place.target;
        let block = state.block();
        let used = |effect| {
            Some(ActionOutcome::Used {
                off_hand: place.off_hand,
                item,
                effect,
            })
        };
        let full = state.level() == Some(3);
        let under_water = {
            let fluid = self
                .block(BlockPos::new(pos.x, pos.y + 1, pos.z))
                .unwrap_or(AIR)
                .fluid_state();
            !fluid.is_empty()
                && tuff_datapack::tags::is(tuff_vanilla_data::tag::fluid::key::WATER, fluid.fluid())
        };
        let fill = |this: &mut Self, new: BlockState, sound: SoundEvent| {
            this.put_block(pos, new);
            this.play_sound(sound, pos, 1.0, 1.0, None);
            this.game_event(GameEvent::FluidPlace, pos.center(), Context::default());
            place
                .player
                .send(PlayerEvent::AwardStat(CustomStat::FillCauldron, 1));
        };
        let take = |this: &mut Self, sound: SoundEvent| {
            this.put_block(pos, Block::Cauldron.default_state());
            this.play_sound(sound, pos, 1.0, 1.0, None);
            this.game_event(GameEvent::FluidPickup, pos.center(), Context::default());
            place
                .player
                .send(PlayerEvent::AwardStat(CustomStat::UseCauldron, 1));
        };
        let water_bottle = || {
            place
                .components
                .potion_contents
                .as_deref()
                .is_some_and(|contents| {
                    contents.potion == Some(tuff_vanilla_data::registry::Potion::Water)
                })
        };
        let layered = |block: Block, level: u8| block.default_state().with_level(level);
        match item {
            Item::LavaBucket | Item::WaterBucket | Item::PowderSnowBucket => {
                let (new, sound) = match item {
                    Item::LavaBucket => (
                        Block::LavaCauldron.default_state(),
                        SoundEvent::ItemBucketEmptyLava,
                    ),
                    Item::WaterBucket => (
                        layered(Block::WaterCauldron, 3)?,
                        SoundEvent::ItemBucketEmpty,
                    ),
                    _ => (
                        layered(Block::PowderSnowCauldron, 3)?,
                        SoundEvent::ItemBucketEmptyPowderSnow,
                    ),
                };
                if item != Item::WaterBucket && under_water {
                    return Some(ActionOutcome::Interacted {
                        off_hand: place.off_hand,
                    });
                }
                fill(self, new, sound);
                used(HandEffect::Emptied(Item::Bucket))
            }
            Item::Bucket => {
                let (filled, sound) = match block {
                    Block::WaterCauldron if full => (Item::WaterBucket, SoundEvent::ItemBucketFill),
                    Block::PowderSnowCauldron if full => {
                        (Item::PowderSnowBucket, SoundEvent::ItemBucketFillPowderSnow)
                    }
                    Block::LavaCauldron => (Item::LavaBucket, SoundEvent::ItemBucketFillLava),
                    _ => return None,
                };
                take(self, sound);
                used(HandEffect::Exchange(filled))
            }
            Item::Potion if water_bottle() => match block {
                Block::Cauldron => {
                    fill(
                        self,
                        layered(Block::WaterCauldron, 1)?,
                        SoundEvent::ItemBottleEmpty,
                    );
                    used(HandEffect::Exchange(Item::GlassBottle))
                }
                Block::WaterCauldron if !full => {
                    let next = state.with_level(state.level()? + 1)?;
                    fill(self, next, SoundEvent::ItemBottleEmpty);
                    used(HandEffect::Exchange(Item::GlassBottle))
                }
                _ => None,
            },
            Item::GlassBottle if block == Block::WaterCauldron => {
                self.lower_cauldron(pos, state);
                self.play_sound(SoundEvent::ItemBottleFill, pos, 1.0, 1.0, None);
                self.game_event(GameEvent::FluidPickup, pos.center(), Context::default());
                place
                    .player
                    .send(PlayerEvent::AwardStat(CustomStat::UseCauldron, 1));
                let mut water = ItemStack::mint(Item::Potion, 1, MintReason::Transform);
                water.components.potion_contents =
                    Some(Box::new(tuff_item::item::PotionContents {
                        potion: Some(tuff_vanilla_data::registry::Potion::Water),
                        ..Default::default()
                    }));
                used(HandEffect::ExchangeStack(Box::new(water)))
            }
            _ if block != Block::WaterCauldron => None,
            _ if tuff_datapack::tags::is(item_tags::key::BANNERS, item) => {
                let mut layers = place.components.banner_patterns.clone()?;
                layers.pop()?;
                let mut cleaned =
                    ItemStack::mint_with(item, 1, &place.components, MintReason::Transform);
                cleaned.components.banner_patterns = Some(layers);
                self.lower_cauldron(pos, state);
                place
                    .player
                    .send(PlayerEvent::AwardStat(CustomStat::CleanBanner, 1));
                used(HandEffect::ExchangeStack(Box::new(cleaned)))
            }
            _ if item != Item::ShulkerBox
                && tuff_datapack::tags::is(item_tags::key::SHULKER_BOXES, item) =>
            {
                let cleaned = ItemStack::mint_with(
                    Item::ShulkerBox,
                    1,
                    &place.components,
                    MintReason::Transform,
                );
                self.lower_cauldron(pos, state);
                place
                    .player
                    .send(PlayerEvent::AwardStat(CustomStat::CleanShulkerBox, 1));
                used(HandEffect::ExchangeStack(Box::new(cleaned)))
            }
            _ if tuff_datapack::tags::is(item_tags::key::CAULDRON_CAN_REMOVE_DYE, item) => {
                place.components.dyed_color?;
                let mut cleaned = ItemStack::mint_with(
                    item,
                    place.held_count,
                    &place.components,
                    MintReason::Transform,
                );
                cleaned.components.dyed_color = None;
                self.lower_cauldron(pos, state);
                place
                    .player
                    .send(PlayerEvent::AwardStat(CustomStat::CleanArmor, 1));
                used(HandEffect::Replace(Some(Box::new(cleaned))))
            }
            _ => None,
        }
    }

    /// A jukebox click: a record in hand goes in and starts playing, an
    /// empty or non-record hand takes the record back out.
    fn use_jukebox(&mut self, place: &Place, state: BlockState) -> Option<ActionOutcome> {
        use tuff_core::world::item_port::{PortAccess, Porter};
        use tuff_item::item::{ItemStack, MintReason};
        let pos = place.target;
        let port = self.shared().item_ports.get(state.block())?.clone();
        let at = PortAccess {
            pos,
            state,
            face: None,
            by: Porter::Player(Some(place.player.id)),
        };
        if state.get("has_record") == Some("true") {
            if let Some(record) = port.take(self.as_dyn(), &at) {
                let rng = self.rng();
                let mut offset = || f64::from(rng.r#gen::<f32>() * 0.7 - 0.35);
                let (dx, dz) = (offset(), offset());
                let position = [
                    f64::from(pos.x) + 0.5 + dx,
                    f64::from(pos.y) + 1.01,
                    f64::from(pos.z) + 0.5 + dz,
                ];
                let rng = self.rng();
                let velocity = [
                    rng.r#gen::<f64>() * 0.2 - 0.1,
                    0.2,
                    rng.r#gen::<f64>() * 0.2 - 0.1,
                ];
                self.spawn_item_at(record, position, velocity, 10);
            }
            return Some(ActionOutcome::Interacted {
                off_hand: place.off_hand,
            });
        }
        let item = place.held?;
        let held = ItemStack::mint_with(item, 1, &place.components, MintReason::Transform);
        if let Err(refused) = port.offer(self.as_dyn(), &at, held) {
            refused.burn(tuff_item::item::BurnReason::Transform);
            return None;
        }
        place.player.send(PlayerEvent::AwardStat(
            tuff_vanilla_data::registry::CustomStat::PlayRecord,
            1,
        ));
        Some(ActionOutcome::Used {
            off_hand: place.off_hand,
            item,
            effect: HandEffect::Consume(1),
        })
    }

    /// A sweet berry bush click: past the sapling stage (age 2 or 3) it
    /// drops berries from the `harvest/sweet_berry_bush` loot table and
    /// resets to age 1, matching the jar's bush growth stages (0 sapling,
    /// 1-2 growing, 3 fully grown -- age 2 already carries berries).
    /// Below that it falls through to placement.
    fn use_flower_pot(&mut self, place: &Place, state: BlockState) -> ActionOutcome {
        let pos = place.target;
        let interacted = ActionOutcome::Interacted {
            off_hand: place.off_hand,
        };
        let held_pot = place
            .held
            .and_then(Item::block)
            .and_then(potted_block_for)
            .map(Block::default_state);
        match (potted_content(state.block()), held_pot) {
            (None, Some(potted)) => {
                self.put_block(pos, potted);
                self.game_event(GameEvent::BlockChange, pos.center(), Context::default());
                place.player.send(PlayerEvent::AwardStat(
                    tuff_vanilla_data::registry::CustomStat::PotFlower,
                    1,
                ));
                ActionOutcome::Used {
                    off_hand: place.off_hand,
                    item: place.held.unwrap_or(Item::Air),
                    effect: HandEffect::Consume(1),
                }
            }
            (Some(_), Some(_)) | (None, None) => interacted,
            (Some(content), None) => {
                if let Some(item) = content.item() {
                    place.player.send(PlayerEvent::ItemGranted {
                        stack: ItemStack::mint(item, 1, MintReason::BlockDrop),
                        picked_up: false,
                    });
                }
                self.put_block(pos, Block::FlowerPot.default_state());
                self.game_event(GameEvent::BlockChange, pos.center(), Context::default());
                interacted
            }
        }
    }

    fn harvest_cave_vines(
        &mut self,
        pos: BlockPos,
        state: BlockState,
        off_hand: bool,
    ) -> Option<ActionOutcome> {
        if state.berries() != Some(true) {
            return None;
        }
        let loot = self.shared().block_loot();
        let drops = loot.table("minecraft:harvest/cave_vine", state, None, self.rng());
        self.drop_items(pos, drops);
        let pitch = 0.8 + self.rng().gen_range(0.0..0.4);
        self.play_sound(SoundEvent::BlockCaveVinesPickBerries, pos, 1.0, pitch, None);
        let picked = state.with_berries(false).unwrap_or(state);
        self.set_block_flags(pos, picked, tuff_core::world::redstone::UPDATE_CLIENTS);
        self.game_event(GameEvent::BlockChange, pos.center(), Context::default());
        Some(ActionOutcome::Interacted { off_hand })
    }

    fn harvest_sweet_berry_bush(
        &mut self,
        pos: BlockPos,
        state: BlockState,
        off_hand: bool,
    ) -> Option<ActionOutcome> {
        if state.age().unwrap_or(0) <= 1 {
            return None;
        }
        let loot = self.shared().block_loot();
        let drops = loot.table(
            "minecraft:harvest/sweet_berry_bush",
            state,
            None,
            self.rng(),
        );
        self.drop_items(pos, drops);
        let pitch = 0.8 + self.rng().gen_range(0.0..0.4);
        self.play_sound(
            SoundEvent::BlockSweetBerryBushPickBerries,
            pos,
            1.0,
            pitch,
            None,
        );
        self.set_block_flags(
            pos,
            state.with_age(1).unwrap_or(state),
            tuff_core::world::redstone::UPDATE_CLIENTS,
        );
        Some(ActionOutcome::Interacted { off_hand })
    }

    /// Items acting on the clicked block before any placement: lighting,
    /// priming TNT, honeycomb, and tools transforming blocks.
    fn use_tool_on(&mut self, place: &Place, state: BlockState) -> Option<ActionOutcome> {
        let item = place.held?;
        let pos = place.target;
        let except = Some(place.player.id);
        let used = |effect| ActionOutcome::Used {
            off_hand: place.off_hand,
            item,
            effect,
        };
        let igniter = matches!(item, Item::FlintAndSteel | Item::FireCharge);
        let ignite_cost = if item == Item::FlintAndSteel {
            HandEffect::Damage(1)
        } else {
            HandEffect::Consume(1)
        };
        if igniter && state.block() == Block::Tnt {
            if !self.shared().rule_bool("tnt_explodes") {
                place.player.send(PlayerEvent::Overlay(
                    tuff_item::text::TextComponent::translate("block.minecraft.tnt.disabled", []),
                ));
                return None;
            }
            self.put_block(pos, AIR);
            let igniter = Igniter {
                id: place.player.id,
                uuid: place.uuid,
                name: place.name.clone(),
            };
            self.spawn_primed_tnt(pos, None, Some(igniter));
            return Some(used(ignite_cost));
        }
        if igniter && interaction::can_light(state) {
            let lit = state.with_lit(true)?;
            if !self.allows_block_action(place.player.id, pos, lit, BlockAction::Placed) {
                return Some(ActionOutcome::Rejected(self.corrections(&[pos])));
            }
            let (sound, pitch) = self.fire_sound(item);
            let except = (item == Item::FlintAndSteel).then_some(place.player.id);
            self.play_sound(sound, pos, 1.0, pitch, except);
            self.put_block(pos, lit);
            return Some(used(ignite_cost));
        }
        if tuff_datapack::tags::is(item_tags::key::AXES, item)
            && state.block() == Block::CopperGolemStatue
        {
            let facing = state.facing().unwrap_or(Facing::North);
            let yaw = match facing {
                Facing::South => 0.0,
                Facing::West => 90.0,
                Facing::North => 180.0,
                Facing::East => 270.0,
                _ => 0.0,
            };
            let id = self.shared().next_entity_id();
            let spawn = [pos.x as f64 + 0.5, pos.y as f64, pos.z as f64 + 0.5];
            if let Some(entity) = self.shared().entity_types.create(
                tuff_vanilla_data::registry::EntityType::CopperGolem,
                id,
                spawn,
                yaw,
            ) {
                self.put_block(pos, AIR);
                self.play_sound(SoundEvent::EntityCopperGolemSpawn, pos, 1.0, 1.0, None);
                self.add_entity(entity);
                return Some(used(HandEffect::Damage(1)));
            }
        }
        if item == Item::Potion
            && place.face != tuff_world_model::Face::Down
            && place
                .components
                .potion_contents
                .as_deref()
                .is_some_and(|c| c.potion == Some(tuff_vanilla_data::registry::Potion::Water))
            && tuff_datapack::tags::is(
                tuff_vanilla_data::tag::block::key::CONVERTIBLE_TO_MUD,
                state.block(),
            )
        {
            let mud = Block::Mud.default_state();
            if !self.allows_block_action(place.player.id, pos, mud, BlockAction::Placed) {
                return Some(ActionOutcome::Rejected(self.corrections(&[pos])));
            }
            self.play_sound(SoundEvent::EntityGenericSplash, pos, 1.0, 1.0, None);
            let splashes: Vec<_> = (0..5)
                .map(|_| {
                    let x = f64::from(pos.x) + self.rng().r#gen::<f64>();
                    let z = f64::from(pos.z) + self.rng().r#gen::<f64>();
                    framed(&play::Particles {
                        particle: tuff_vanilla_data::registry::ParticleType::Splash.id(),
                        position: [x, f64::from(pos.y + 1), z],
                        spread: [0.0; 3],
                        speed: 1.0,
                        count: 1,
                        alternative: false,
                    })
                })
                .collect();
            self.send_to_chunk(ChunkPos::of_block(pos), None, packets(splashes));
            self.play_sound(SoundEvent::ItemBottleEmpty, pos, 1.0, 1.0, None);
            self.game_event(GameEvent::FluidPlace, pos.center(), Context::default());
            self.put_block(pos, mud);
            return Some(used(HandEffect::Exchange(Item::GlassBottle)));
        }
        if item == Item::Honeycomb
            && let Some(waxed) = interaction::waxed(state)
        {
            self.put_block(pos, waxed);
            self.level_event_except(play::level_event::PARTICLES_WAX_ON, pos, except);
            self.play_sound(SoundEvent::ItemHoneycombWaxOn, pos, 1.0, 1.0, except);
            if placement::is_chest(state.block())
                && let Some(partner) = placement::chest_partner(state, pos)
            {
                self.level_event_except(play::level_event::PARTICLES_WAX_ON, partner, except);
                self.play_sound(SoundEvent::ItemHoneycombWaxOn, pos, 1.0, 1.0, except);
            }
            return Some(used(HandEffect::Consume(1)));
        }
        if item == Item::Compass && state.block() == Block::Lodestone {
            self.play_sound(SoundEvent::ItemLodestoneCompassLock, pos, 1.0, 1.0, None);
            let tracker =
                LodestoneTracker::locked(&self.dimension_info().key, [pos.x, pos.y, pos.z]);
            let mut locked =
                ItemStack::mint_with(Item::Compass, 1, &place.components, MintReason::Transform);
            locked
                .components
                .other
                .insert(DataComponentType::LodestoneTracker.name(), tracker.to_nbt());
            return Some(used(HandEffect::Split(Box::new(locked))));
        }
        if item == Item::Shears
            && state
                .block()
                .growing_plant()
                .is_some_and(|plant| plant.head == state.block())
            && let Some(max) = state
                .get("age")
                .and_then(|age| age.parse::<u8>().ok())
                .filter(|age| *age < 25)
                .and_then(|_| state.with("age", "25"))
        {
            self.play_sound(SoundEvent::BlockGrowingPlantCrop, pos, 1.0, 1.0, None);
            self.put_block(pos, max);
            self.game_event(
                GameEvent::BlockChange,
                pos.center(),
                Context {
                    source: Some(Source::player(place.player.id)),
                    affected: Some(max),
                },
            );
            return Some(used(HandEffect::Damage(1)));
        }
        if item == Item::Shears && state.block() == Block::Pumpkin {
            let face = placement::facing(place.face);
            let carved_facing = if face.is_horizontal() {
                face
            } else {
                placement::from_yaw(place.yaw).opposite()
            };
            let new = interaction::with_properties_of(Block::CarvedPumpkin, state)
                .with_facing(carved_facing)?;
            self.put_block(pos, new);
            self.play_sound(SoundEvent::BlockPumpkinCarve, pos, 1.0, 1.0, None);
            let loot = self.shared().block_loot();
            let drops = loot.table("minecraft:carve/pumpkin", state, Some(item), self.rng());
            let (sx, _, sz) = carved_facing.step();
            let (sx, sz) = (f64::from(sx), f64::from(sz));
            for stack in drops {
                let position = [
                    pos.x as f64 + 0.5 + sx * 0.65,
                    pos.y as f64 + 0.1,
                    pos.z as f64 + 0.5 + sz * 0.65,
                ];
                let velocity = [
                    0.05 * sx + self.rng().gen_range(0.0..0.02),
                    0.05,
                    0.05 * sz + self.rng().gen_range(0.0..0.02),
                ];
                self.spawn_item_at(stack, position, velocity, items::BLOCK_DROP_PICKUP_DELAY);
            }
            self.game_event(GameEvent::Shear, pos.center(), Context::default());
            return Some(used(HandEffect::Damage(1)));
        }
        if !place.off_hand && place.off_hand_blocks {
            return None;
        }
        let face = placement::facing(place.face);
        let (transform, new) = {
            let level = |p: BlockPos| self.block(p).unwrap_or(AIR);
            interaction::tool_transform(item, face, pos, &level)?
        };
        if let Some(table) = transform.loot
            && self.shared().rule_bool("block_drops")
        {
            let loot = self.shared().block_loot();
            for stack in loot.table(table, state, Some(item), self.rng()) {
                match transform.drop_strategy {
                    DropStrategy::ClickedFace => self.pop_from_face(pos, face, stack),
                    DropStrategy::FromMiddle => self.drop_items(pos, vec![stack]),
                }
            }
        }
        self.put_block(pos, new);
        if let Some(sound) = transform.sound {
            self.play_sound(sound, pos, 1.0, 1.0, except);
        }
        let particle = match transform.particle {
            TransformParticle::None => None,
            TransformParticle::Scrape => Some(play::level_event::PARTICLES_SCRAPE),
            TransformParticle::WaxOn => Some(play::level_event::PARTICLES_WAX_ON),
            TransformParticle::WaxOff => Some(play::level_event::PARTICLES_WAX_OFF),
        };
        if let Some(event) = particle {
            self.level_event_except(event, pos, except);
            if transform.transform_type == TransformType::CopperChest
                && let Some(partner) = placement::chest_partner(state, pos)
            {
                self.level_event_except(event, partner, except);
            }
        }
        let effect = if item.max_stack_size() > 1 {
            HandEffect::Consume(u8::from(transform.consume_on_use))
        } else {
            HandEffect::Damage(transform.item_damage_per_use)
        };
        Some(used(effect))
    }

    /// An eye of ender thrown by a player not looking at an end portal
    /// frame: the stronghold search runs on rayon (the rings may need
    /// computing) and answers the player directly.
    fn locate_eye(
        &mut self,
        player: PlayerHandle,
        eye: [f64; 3],
        look: [f64; 3],
        reach: f64,
        from: BlockPos,
        off_hand: bool,
    ) {
        let to = [0, 1, 2].map(|i| eye[i] + look[i] * reach);
        let level = |p: BlockPos| self.block(p).unwrap_or(AIR);
        if let Some(hit) = raycast::cast(eye, to, FluidMode::None, &level)
            && level(hit.pos).block() == Block::EndPortalFrame
        {
            return;
        }
        let generator = self
            .shared()
            .dimension(self.dimension_id())
            .generator
            .clone();
        rayon::spawn(move || {
            let target = generator.nearest_ring_structure(
                tuff_vanilla_data::tag::worldgen::structure::EYE_OF_ENDER_LOCATED,
                from,
            );
            let task = SessionTask::new(move |session| {
                Box::pin(async move {
                    if let Some(target) = target {
                        session.throw_eye(off_hand, target).await;
                    }
                })
            })
            .with_data(EyeTarget { off_hand, target });
            player.send(PlayerEvent::Run(task));
        });
    }

    /// Fills or empties a bucket at what the player looks at, forwarding
    /// to the neighbour that owns the target.
    fn use_bucket(&mut self, mut request: BucketUse) {
        let reject = |region: &Self, request: &BucketUse, at: &[BlockPos]| {
            request.player.send(PlayerEvent::ActionResult {
                sequence: request.sequence,
                outcome: ActionOutcome::Rejected(region.corrections(at)),
            });
        };
        let releases_mob = request.fluid.is_none() && request.mob.is_some();
        let fluids = if request.fluid.is_none() && !releases_mob {
            FluidMode::SourceOnly
        } else {
            FluidMode::None
        };
        let hit = match request.hit {
            Some(hit) => hit,
            None => {
                let to = [0, 1, 2].map(|i| request.eye[i] + request.look[i] * request.reach);
                let level = |p: BlockPos| self.block(p).unwrap_or(AIR);
                match raycast::cast(request.eye, to, fluids, &level) {
                    Some(hit) => hit,
                    None => return reject(self, &request, &[]),
                }
            }
        };
        let evaporates = self.dimension_info().environment().water_evaporates;
        let target = match request.fluid {
            None if releases_mob => Some((placement::step(hit.pos, hit.face), None)),
            None => Some((hit.pos, None)),
            Some(fluid) => {
                let level = |p: BlockPos| self.block(p).unwrap_or(AIR);
                interaction::pour_target(
                    &hit,
                    fluid,
                    request.sneaking,
                    request.creative,
                    evaporates,
                    &level,
                )
                .map(|(pos, pour)| (pos, Some(pour)))
            }
        };
        let Some((pos, pour)) = target else {
            return reject(self, &request, &[hit.pos]);
        };
        let chunk = ChunkPos::of_block(pos);
        if !self.owns_chunk(chunk) {
            request.hit = Some(hit);
            match self.neighbor(chunk.region()) {
                Some(neighbor)
                    if neighbor.internal(RegionInternal::Run(RegionTask::new({
                        let request = request.clone();
                        move |world| Interact::use_bucket(world, request)
                    }))) => {}
                _ => reject(self, &request, &[pos]),
            }
            return;
        }
        if !self.owns(pos) || !self.dimension_info().contains_block(pos) {
            return reject(self, &request, &[pos]);
        }
        let except = Some(request.player.id);
        let effect = match (request.fluid, pour) {
            (None, _) if releases_mob => {
                let kind = request.mob.expect("checked above");
                self.release_bucket_mob(
                    kind,
                    pos,
                    request.mob_data.clone(),
                    request.mob_components.as_deref(),
                    except,
                );
                HandEffect::Emptied(Item::Bucket)
            }
            (None, _) => {
                let Some((taken, sound)) = self.take_fluid_as(pos, request.creative) else {
                    return reject(self, &request, &[pos]);
                };
                if let Some(sound) = sound {
                    let packet = packets([framed(&play::Sound {
                        sound_event: sound.id(),
                        category: play::sound_category::PLAYERS,
                        position: request.position,
                        volume: 1.0,
                        pitch: 1.0,
                        seed: self.rng().r#gen(),
                    })]);
                    self.send_to_chunk(ChunkPos::of_position(request.position), except, packet);
                }
                self.game_event(
                    GameEvent::FluidPickup,
                    pos.center(),
                    Context::from(Source::player(request.player.id).sneaking(request.sneaking)),
                );
                HandEffect::Exchange(taken)
            }
            (Some(fluid), Some(pour)) => {
                if !self.allows_block_action(
                    request.player.id,
                    pos,
                    fluid.default_state(),
                    BlockAction::Placed,
                ) {
                    return reject(self, &request, &[pos]);
                }
                self.pour_fluid(
                    pos,
                    fluid,
                    pour,
                    request.mob,
                    request.mob_data.clone(),
                    except,
                );
                if !matches!(pour, Pour::Evaporate) {
                    self.game_event(
                        GameEvent::FluidPlace,
                        pos.center(),
                        Context::from(Source::player(request.player.id).sneaking(request.sneaking)),
                    );
                }
                HandEffect::Emptied(Item::Bucket)
            }
            (Some(_), None) => return reject(self, &request, &[pos]),
        };
        request.player.send(PlayerEvent::ActionResult {
            sequence: request.sequence,
            outcome: ActionOutcome::Used {
                off_hand: request.off_hand,
                item: request.item,
                effect,
            },
        });
    }

    /// Takes the fluid (or the waterlogging) out of the block at `pos`:
    /// the filled bucket and the sound a player hears.
    fn take_fluid(&mut self, pos: BlockPos) -> Option<(Item, Option<SoundEvent>)> {
        self.take_fluid_as(pos, false)
    }

    /// [`Self::take_fluid`] for a player's bucket, `creative` or not.
    fn take_fluid_as(
        &mut self,
        pos: BlockPos,
        creative: bool,
    ) -> Option<(Item, Option<SoundEvent>)> {
        let state = self.block(pos).unwrap_or(AIR);
        let (left, taken, sound) = interaction::player_bucket_pickup(state, creative)?;
        self.put_block(pos, left);
        if left.block() == state.block() {
            let level = |p: BlockPos| self.block(p).unwrap_or(AIR);
            if !placement::can_survive(left, pos, &level) {
                self.destroy_block(pos);
            }
        }
        Some((taken, sound))
    }

    /// Pours a bucket of `fluid` at `pos` as `pour` says, then lets out the
    /// mob it held, if any.
    fn pour_fluid(
        &mut self,
        pos: BlockPos,
        fluid: Block,
        pour: Pour,
        mob: Option<tuff_vanilla_data::registry::EntityType>,
        mob_data: Option<tuff_nbt::Compound>,
        except: Option<PlayerId>,
    ) {
        let chunk = ChunkPos::of_block(pos);
        let lava = tuff_vanilla_data::registry::Fluid::from_name(fluid.name())
            .is_some_and(|f| tuff_datapack::tags::is(tuff_vanilla_data::tag::fluid::key::LAVA, f));
        let empty_sound = if lava {
            SoundEvent::ItemBucketEmptyLava
        } else {
            SoundEvent::ItemBucketEmpty
        };
        match pour {
            Pour::Evaporate => {
                let pitch = 2.6 + (self.rng().r#gen::<f32>() - self.rng().r#gen::<f32>()) * 0.8;
                self.play_sound(SoundEvent::BlockFireExtinguish, pos, 0.5, pitch, except);
                let packet = packets([framed(&play::Particles {
                    particle: tuff_vanilla_data::registry::ParticleType::LargeSmoke.id(),
                    position: [pos.x as f64, pos.y as f64, pos.z as f64],
                    spread: [1.0; 3],
                    speed: 0.0,
                    count: 8,
                    alternative: true,
                })]);
                self.send_to_chunk(chunk, None, packet);
            }
            Pour::Contain => {
                let state = self.block(pos).unwrap_or(AIR);
                if let Some(placement) = state
                    .place_liquid(Fluid::Water)
                    .or_else(|| interaction::barrier_placement(state))
                {
                    self.apply_liquid_placement(pos, placement);
                }
                self.play_sound(empty_sound, pos, 1.0, 1.0, except);
            }
            Pour::Replace { break_old } => {
                if break_old {
                    self.destroy_block(pos);
                }
                self.put_block(pos, fluid.default_state());
                self.play_sound(empty_sound, pos, 1.0, 1.0, except);
            }
        }
        if !matches!(pour, Pour::Evaporate)
            && let Some(kind) = mob
        {
            self.spawn_bucket_mob(kind, pos.center(), mob_data, None);
        }
    }

    /// Lets out the mob of a bucket that holds no fluid (a sulfur cube),
    /// standing in the block at `pos`.
    fn release_bucket_mob(
        &mut self,
        kind: tuff_vanilla_data::registry::EntityType,
        pos: BlockPos,
        mob_data: Option<tuff_nbt::Compound>,
        components: Option<&tuff_nbt::Compound>,
        except: Option<PlayerId>,
    ) {
        let packet = packets([framed(&play::Sound {
            sound_event: SoundEvent::ItemBucketEmptySulfurCube.id(),
            category: play::sound_category::NEUTRAL,
            position: pos.center(),
            volume: 1.0,
            pitch: 1.0,
            seed: self.rng().r#gen(),
        })]);
        self.send_to_chunk(ChunkPos::of_block(pos), except, packet);
        let feet = [
            f64::from(pos.x) + 0.5,
            f64::from(pos.y),
            f64::from(pos.z) + 0.5,
        ];
        self.spawn_bucket_mob(kind, feet, mob_data, components);
        self.game_event(
            tuff_core::world::game_event::GameEvent::EntityPlace,
            feet,
            tuff_core::world::game_event::Context::default(),
        );
    }

    /// Places the fish or axolotl a mob bucket holds where its water just
    /// went, right after the fluid, as vanilla does.
    fn spawn_bucket_mob(
        &mut self,
        kind: tuff_vanilla_data::registry::EntityType,
        position: [f64; 3],
        data: Option<tuff_nbt::Compound>,
        components: Option<&tuff_nbt::Compound>,
    ) {
        let base = tuff_core::world::entity::EntityBase::new(
            kind,
            self.shared().next_entity_id(),
            self.rng().r#gen(),
            position,
            [0.0; 3],
        );
        let types = &self.shared().entity_types;
        if let Some(entity) = types.from_bucket(base, data, components) {
            self.add_entity(entity);
        }
    }

    /// Breaks the block at `pos` as the world does, with particles and
    /// drops.
    fn destroy_block(&mut self, pos: BlockPos) {
        let Some(old) = self.block(pos) else {
            return;
        };
        if is_air(old) {
            return;
        }
        let saved = self.saved_block_entity(pos);
        self.put_block(pos, placement::remains(old));
        self.break_event(pos, old, None);
        if self.shared().rule_bool("block_drops") {
            let loot = self.shared().block_loot();
            let stacks = loot.drops(old, None, saved.as_ref(), self.rng());
            self.drop_items(pos, stacks);
        }
        self.release_infestation(pos, old, None);
    }

    fn level_event_except(&self, event: i32, pos: BlockPos, except: Option<PlayerId>) {
        let packet = packets([framed(&play::LevelEvent {
            event,
            position: pos,
            data: 0,
            global: false,
        })]);
        self.send_to_chunk(ChunkPos::of_block(pos), except, packet);
    }

    /// Drops an item out of a block face, as loot from an interaction is.
    fn pop_from_face(&mut self, pos: BlockPos, face: Facing, stack: ItemStack) {
        const HALF_WIDTH: f64 = 0.125;
        const HALF_HEIGHT: f64 = 0.125;
        let (sx, sy, sz) = face.step();
        let spread = |step: i32, half: f64, rng: &mut rand::rngs::StdRng| {
            if step == 0 {
                rng.gen_range(-0.25..0.25)
            } else {
                step as f64 * (0.5 + half)
            }
        };
        let position = [
            pos.x as f64 + 0.5 + spread(sx, HALF_WIDTH, self.rng()),
            pos.y as f64 + 0.5 + spread(sy, HALF_HEIGHT, self.rng()) - HALF_HEIGHT,
            pos.z as f64 + 0.5 + spread(sz, HALF_WIDTH, self.rng()),
        ];
        let velocity = |step: i32, rng: &mut rand::rngs::StdRng| {
            if step == 0 {
                rng.gen_range(-0.1..0.1)
            } else {
                step as f64 * 0.1
            }
        };
        let velocity = [
            velocity(sx, self.rng()),
            if sy == 0 {
                self.rng().gen_range(0.0..0.1)
            } else {
                sy as f64 * 0.1 + 0.1
            },
            velocity(sz, self.rng()),
        ];
        self.spawn_item_at(stack, position, velocity, items::BLOCK_DROP_PICKUP_DELAY);
    }
}

impl<T: RegionWorld + ?Sized> Interact for T {}

mod rest;
pub use rest::Rest;

/// Vanilla block and item interactions as region hooks.
pub struct InteractionHooks;

impl tuff_core::world::region_api::RegionHooks for InteractionHooks {
    fn use_block(
        &self,
        world: &mut dyn RegionWorld,
        place: &Place,
        state: BlockState,
    ) -> Option<ActionOutcome> {
        Interact::use_block(world, place, state)
    }

    fn use_item_on(
        &self,
        world: &mut dyn RegionWorld,
        place: &Place,
        state: BlockState,
    ) -> Option<ActionOutcome> {
        if place.held == Some(Item::Brush) {
            return Some(ActionOutcome::StartUsing {
                off_hand: place.off_hand,
                item: Item::Brush,
                ticks: tuff_vanilla_common::brushable::BRUSH_USE_TICKS,
            });
        }
        Interact::use_tool_on(world, place, state)
    }

    fn attack_block(&self, world: &mut dyn RegionWorld, pos: BlockPos, state: BlockState) {
        if state.block() == Block::DragonEgg {
            Interact::teleport_dragon_egg(world, pos, state);
        }
    }

    fn play_note(&self, world: &mut dyn RegionWorld, pos: BlockPos) {
        Rest::play_note(world, pos);
    }

    fn use_door(&self, world: &mut dyn RegionWorld, pos: BlockPos, open: bool) {
        Rest::use_door(world, pos, open);
    }

    fn occupy_bed(&self, world: &mut dyn RegionWorld, pos: BlockPos, occupied: bool) {
        Rest::occupy_bed(world, pos, occupied);
    }

    fn leave_bed(
        &self,
        world: &mut dyn RegionWorld,
        player: &PlayerHandle,
        pos: BlockPos,
        yaw: f32,
    ) {
        Rest::leave_bed(world, player.clone(), pos, yaw);
    }

    fn respawn_point(
        &self,
        world: &mut dyn RegionWorld,
        query: &tuff_core::world::region_api::RespawnQuery,
    ) -> Option<Option<RespawnSpot>> {
        Some(Rest::respawn_point(
            world,
            query.pos,
            query.yaw,
            query.pitch,
            query.forced,
            query.consume,
        ))
    }

    fn dispense(
        &self,
        world: &mut dyn RegionWorld,
        pos: BlockPos,
        facing: Facing,
        _origin: [f64; 3],
        stack: &ItemStack,
    ) -> Option<tuff_core::world::region_api::DispenseOutcome> {
        super::dispense::dispense(world, pos, facing, stack)
    }

    fn use_bucket(&self, world: &mut dyn RegionWorld, request: &BucketUse) {
        if request.item.spawns_entity().is_some() {
            return;
        }
        Interact::use_bucket(world, request.clone());
    }
}

/// The request that throws an eye of ender from `eye` along `look`: unless
/// it looks at an end portal frame within `reach`, the nearest stronghold
/// is looked up off the actor and the player answered with
/// [`PlayerEvent::EyeTarget`].
pub fn locate_eye_request(
    player: PlayerHandle,
    eye: [f64; 3],
    look: [f64; 3],
    reach: f64,
    from: BlockPos,
    off_hand: bool,
) -> RegionRequest {
    RegionRequest::Run(RegionTask::new(move |world| {
        Interact::locate_eye(world, player, eye, look, reach, from, off_hand)
    }))
}

/// The block that a `potted_*` flower pot variant holds.
fn potted_content(block: Block) -> Option<Block> {
    let name = block.name().strip_prefix("minecraft:potted_")?;
    Block::from_name(&format!("minecraft:{name}"))
}

/// The flower pot variant holding `content`.
fn potted_block_for(content: Block) -> Option<Block> {
    let name = content.name().strip_prefix("minecraft:")?;
    Block::from_name(&format!("minecraft:potted_{name}"))
}
