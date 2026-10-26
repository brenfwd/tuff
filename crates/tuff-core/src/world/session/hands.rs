//! Hands and held items: mining actions, block and bucket use requests, cooldowns, gliding, effects, hand changes, drops and experience.

use super::*;
use crate::protocol::play::{self, GameMode, PlayerActionStatus};
use rand::Rng;
use tokio::sync::oneshot;
use tuff_item::item::{BurnReason, ItemStack, LodestoneTracker, MintReason, StackCooldown};
use tuff_storage::admin::PermissionLevel;
use tuff_vanilla_data::damage_type;
use tuff_vanilla_data::item::EquipmentSlot;
use tuff_vanilla_data::registry::CustomStat;
use tuff_vanilla_data::registry::{Attribute, DataComponentType, MobEffect, SoundEvent};
use tuff_vanilla_data::{Block, Item};
use tuff_world_model::{BlockPos, Face};

use crate::world::effects::{self, ActiveEffect, EffectAction, EffectChange};
use crate::world::inventory::Inventory;

use crate::world::messages::{BreakMode, BucketUse, DestroyTick, HandEffect, Place, RegionRequest};
use crate::world::player::{self, Destroying};
use crate::world::{REACH, distance_sq, framed, packets};
use tuff_world_model::pos::ChunkPos;

/// Ticks between checks that a compass's lodestone still stands.
const LODESTONE_CHECK_INTERVAL: u64 = 10;
const CAN_BREAK: &str = "minecraft:can_break";
const CAN_PLACE_ON: &str = "minecraft:can_place_on";

impl PlayerSession {
    /// Using a writable or written book opens it on the client.
    pub fn open_book(&mut self, off_hand: bool) -> bool {
        let is_book = self
            .inventory
            .hand(off_hand)
            .is_some_and(|s| matches!(s.item(), Item::WritableBook | Item::WrittenBook));
        if is_book {
            self.send(&play::OpenBook { off_hand });
        }
        is_book
    }

    pub async fn player_action(
        &mut self,
        status: PlayerActionStatus,
        position: BlockPos,
        face: i8,
        sequence: i32,
    ) {
        let creative = self.mode.abilities.instabuild;
        let mined_ticks = self
            .actions
            .destroying
            .as_ref()
            .filter(|destroying| destroying.pos == position)
            .map(|destroying| destroying.ticks(self.tick));
        if matches!(
            status,
            PlayerActionStatus::StartDestroyBlock
                | PlayerActionStatus::StopDestroyBlock
                | PlayerActionStatus::AbortDestroyBlock
        ) {
            self.actions.destroying = None;
        }
        if status == PlayerActionStatus::StartDestroyBlock && self.holds_debug_stick(false) {
            if !self.is_dead() && distance_sq(self.eye(), position.center()) <= REACH * REACH {
                self.debug_stick_interact(false, position, false).await;
            }
            self.resend(position, sequence).await;
            return;
        }
        match status {
            PlayerActionStatus::StartDestroyBlock if creative => {
                let tool = self.inventory.hand(false);
                if tool.is_none_or(ItemStack::can_destroy_blocks_in_creative) {
                    self.break_block(position, sequence, BreakMode::Creative)
                        .await;
                } else {
                    self.resend(position, sequence).await;
                }
            }
            PlayerActionStatus::StartDestroyBlock => {
                if self.may_destroy(position) {
                    self.actions.destroying = Some(Destroying::new(position, face, self.tick));
                }
                let mode = BreakMode::Start {
                    eye: self.eye(),
                    miner: self.miner(),
                };
                self.break_block(position, sequence, mode).await;
            }
            PlayerActionStatus::StopDestroyBlock if !creative => match mined_ticks {
                Some(ticks) => {
                    let mode = BreakMode::Verified {
                        ticks,
                        eye: self.eye(),
                        miner: self.miner(),
                    };
                    self.break_block(position, sequence, mode).await;
                }
                None => self.resend(position, sequence).await,
            },
            PlayerActionStatus::ChangeDestroyDirection => {
                if let Some(destroying) = &mut self.actions.destroying {
                    destroying.set_face(face);
                }
                self.send(&play::BlockChangedAck(sequence));
            }
            PlayerActionStatus::ReleaseUseItem => {
                self.stop_using();
                self.send(&play::BlockChangedAck(sequence));
            }
            PlayerActionStatus::StopDestroyBlock => self.send(&play::BlockChangedAck(sequence)),
            PlayerActionStatus::Stab => self.send(&play::BlockChangedAck(sequence)),
            PlayerActionStatus::AbortDestroyBlock => {
                if let Some(handle) = self.region_of(position) {
                    handle
                        .request(RegionRequest::DestroyAborted {
                            player: self.conn.handle.id,
                            pos: position,
                        })
                        .await;
                }
                self.resend(position, sequence).await;
            }
            PlayerActionStatus::DropAllItems | PlayerActionStatus::DropItem => {
                if self.is_dead() || self.mode.game_mode == GameMode::Spectator {
                    return;
                }
                let count = if status == PlayerActionStatus::DropItem {
                    1
                } else {
                    u8::MAX
                };
                if let Some(stack) = self.inventory.take_from_hand(false, count) {
                    self.send_inventory();
                    self.progress.stats.award_custom(CustomStat::Drop, 1);
                    self.throw_item(stack).await;
                }
                self.actions.attack_reset_tick = self.tick;
            }
            PlayerActionStatus::SwapItemWithOffhand => {
                if self.mode.game_mode != GameMode::Spectator {
                    self.inventory.swap_hands();
                }
                self.send_inventory();
            }
        }
    }

    pub fn may_destroy(&self, position: BlockPos) -> bool {
        distance_sq(self.eye(), position.center()) <= REACH * REACH
            && !self.is_dead()
            && (self.mode.abilities.may_build || self.adventure_tool(false, CAN_BREAK))
            && !self.under_spawn_protection(position)
            && self.dimension().block_within_border(position)
    }

    fn adventure_tool(&self, off_hand: bool, component: &str) -> bool {
        self.mode.game_mode == GameMode::Adventure
            && self
                .inventory
                .hand(off_hand)
                .is_some_and(|stack| stack.components.other.get(component).is_some())
    }

    pub fn under_spawn_protection(&self, position: BlockPos) -> bool {
        self.shared.under_spawn_protection(
            self.dimension,
            position,
            self.mode.permission >= PermissionLevel::Gamemasters,
        )
    }

    pub async fn break_block(&mut self, position: BlockPos, sequence: i32, mode: BreakMode) {
        match self.region_of(position) {
            Some(handle) if self.may_destroy(position) => {
                let player = self.conn.handle.clone();
                let tool = self
                    .inventory
                    .hand(false)
                    .map(tuff_item::item::ItemSnapshot::of);
                let request = if self.mode.abilities.may_build {
                    RegionRequest::Break {
                        player,
                        pos: position,
                        tool,
                        mode,
                        sequence,
                    }
                } else {
                    RegionRequest::BreakRestricted {
                        player,
                        pos: position,
                        tool,
                        mode,
                        sequence,
                    }
                };
                handle.request(request).await;
            }
            _ => {
                if !self.is_dead() {
                    self.warn_spawn_protection(position);
                }
                self.resend(position, sequence).await
            }
        }
    }

    pub fn overlay_error(&mut self, key: &str, args: impl IntoIterator<Item = TextComponent>) {
        let content = TextComponent::translate(key, args).color("red");
        self.send(&play::SystemChat {
            content: &content,
            overlay: true,
        });
    }

    fn warn_spawn_protection(&mut self, position: BlockPos) {
        if self.under_spawn_protection(position) {
            let short = format!("{}, {}, {}", position.x, position.y, position.z);
            self.overlay_error("build.spawn_protection", [TextComponent::new(short)]);
        }
    }

    /// Sends the build limit message when `target` is outside the world's
    /// height range; returns whether it did.
    pub fn warn_build_limit(&mut self, target: BlockPos) -> bool {
        let shape = self.dimension().shape;
        let (min, max) = (shape.min_y, shape.max_y() - 1);
        let (key, limit) = if target.y > max {
            ("build.tooHigh", max)
        } else if target.y < min {
            ("build.tooLow", min)
        } else {
            return false;
        };
        self.overlay_error(key, [TextComponent::new(limit.to_string())]);
        true
    }

    /// Asks the owning region to resend a block, undoing the client's
    /// prediction, and acknowledges `sequence`.
    pub async fn resend(&mut self, pos: BlockPos, sequence: i32) {
        match self.region_of(pos) {
            Some(handle) => {
                handle
                    .request(RegionRequest::Resend {
                        player: self.conn.handle.clone(),
                        pos,
                        sequence,
                    })
                    .await;
            }
            None => self.send(&play::BlockChangedAck(sequence)),
        }
    }

    pub async fn use_item_on(
        &mut self,
        off_hand: bool,
        target: BlockPos,
        face: Face,
        cursor: [f32; 3],
        sequence: i32,
    ) {
        if self.holds_debug_stick(off_hand) {
            if !self.is_dead() && distance_sq(self.eye(), target.center()) <= REACH * REACH {
                self.debug_stick_interact(off_hand, target, true).await;
            }
            self.resend(target, sequence).await;
            return;
        }
        let held = self
            .inventory
            .hand(off_hand)
            .map(|stack| stack.item())
            .filter(|_| {
                self.mode.abilities.may_build || self.adventure_tool(off_hand, CAN_PLACE_ON)
            });
        let item = held.filter(|item| {
            item.block().is_some() && !self.shared.block_rules.places_on_water(*item)
        });
        let hands_empty =
            self.inventory.hand(false).is_none() && self.inventory.hand(true).is_none();
        let interact = !self.pose.sneaking || hands_empty;
        let region = self.region_of(target);
        if !self.is_dead() && distance_sq(self.eye(), target.center()) <= REACH * REACH {
            if self.warn_build_limit(target) {
                self.send_inventory();
                self.resend(target, sequence).await;
                return;
            }
            self.warn_spawn_protection(target);
        }
        match region {
            Some(region)
                if !self.is_dead()
                    && (held.is_some() || interact)
                    && self.sync.pending_teleport.is_none()
                    && distance_sq(self.eye(), target.center()) <= REACH * REACH
                    && !self.under_spawn_protection(target)
                    && self.dimension().block_within_border(target) =>
            {
                let mut place = self.place_request(
                    off_hand, target, face, cursor, sequence, held, item, interact,
                );
                place.offer = self.place_offer(&region, off_hand, target, held).await;
                if place.offer.pays() {
                    self.expect_answer(sequence);
                }
                self.send_place(&region, RegionRequest::Place(place)).await;
            }
            _ => {
                self.send_inventory();
                self.resend(target, sequence).await;
            }
        }
    }

    /// Hands a lily pad or frogspawn use in the air to the region around
    /// the player's eyes, which aims it at the water surface. Returns
    /// whether it did.
    pub async fn place_on_water(&mut self, off_hand: bool, sequence: i32) -> bool {
        let Some(item) = self
            .inventory
            .hand(off_hand)
            .map(|stack| stack.item())
            .filter(|item| self.shared.block_rules.places_on_water(*item))
        else {
            return false;
        };
        if self.is_dead() || !self.mode.abilities.may_build {
            return false;
        }
        let eye = self.eye();
        let target = BlockPos::containing(eye);
        let Some(region) = self.region_of(target) else {
            return false;
        };
        let mut place = self.place_request(
            off_hand,
            target,
            Face::Up,
            [0.5, 1.0, 0.5],
            sequence,
            Some(item),
            Some(item),
            false,
        );
        let slot = self.inventory.hand_slot(off_hand);
        if !self.mode.abilities.instabuild
            && let Some(one) = ItemStack::take_from(self.inventory.slot_mut(slot), 1)
        {
            place.offer = crate::world::messages::PlaceOffer::paying(false, slot, one);
        }
        self.send_place(&region, RegionRequest::PlaceOnWater(place))
            .await;
        true
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "independent inputs of one operation; a parameter struct would be built once"
    )]
    fn place_request(
        &self,
        off_hand: bool,
        target: BlockPos,
        face: Face,
        cursor: [f32; 3],
        sequence: i32,
        held: Option<Item>,
        item: Option<Item>,
        interact: bool,
    ) -> Place {
        Place {
            player: self.conn.handle.clone(),
            eye: self.eye(),
            target,
            face,
            cursor,
            yaw: self.pose.yaw,
            pitch: self.pose.pitch,
            sneaking: self.pose.sneaking,
            item,
            held,
            components: self
                .inventory
                .hand(off_hand)
                .map(|stack| tuff_item::item::ComponentsSnapshot::of(&stack.components))
                .unwrap_or_default(),
            held_count: self
                .inventory
                .hand(off_hand)
                .map_or(0, |stack| stack.count()),
            off_hand,
            off_hand_item: self
                .inventory
                .hand(true)
                .map(|stack| stack.item())
                .filter(|_| self.mode.abilities.may_build),
            sequence,
            interact,
            can_eat: self.can_eat(false),
            name: self.profile.name.clone(),
            uuid: self.profile.id,
            position: self.pose.position,
            off_hand_blocks: self
                .inventory
                .hand(true)
                .is_some_and(|stack| stack.item().blocks_attacks().is_some())
                && !self.pose.sneaking,
            may_build: self.mode.abilities.may_build,
            game_master: self.mode.game_mode == GameMode::Creative
                && self.mode.permission >= PermissionLevel::Gamemasters,
            context: None,
            offer: Default::default(),
            hand_slot: self.inventory.hand_slot(off_hand),
        }
    }

    /// Hands a fluid bucket's use to the region around the player's eyes,
    /// which casts the view ray. Returns whether it did; the region's answer
    /// acknowledges `sequence`.
    pub async fn use_bucket(&mut self, off_hand: bool, sequence: i32) -> bool {
        let Some(stack) = self.inventory.hand(off_hand) else {
            return false;
        };
        let item = stack.item();
        let egg = item.spawns_entity().is_some();
        let (fluid, mob) = match item.bucket() {
            Some(tuff_vanilla_data::item::Bucket::Fluid { fluid, mob }) => (fluid, mob),
            _ if egg => (None, None),
            _ => return false,
        };
        if self.is_dead() || !self.mode.abilities.may_build || self.on_cooldown(item) {
            return false;
        }
        let mob_data =
            mob.and_then(
                |_| match stack.components.other.get("minecraft:bucket_entity_data") {
                    Some(tuff_nbt::Tag::Compound(nbt)) => Some(nbt.clone()),
                    _ => None,
                },
            );
        let mob_components = if egg {
            Some(Box::new(stack.components.to_nbt()))
        } else {
            mob.map(|_| Box::new(stack.components.other.clone()))
        };
        let eye = self.eye();
        let Some(region) = self.region_of(BlockPos::new(
            eye[0].floor() as i32,
            eye[1].floor() as i32,
            eye[2].floor() as i32,
        )) else {
            return false;
        };
        let slot = self.inventory.hand_slot(off_hand);
        let pay = if !self.mode.abilities.instabuild {
            ItemStack::take_from(self.inventory.slot_mut(slot), 1)
                .map(|one| crate::world::messages::BucketPay::new(slot, one))
                .unwrap_or_default()
        } else {
            Default::default()
        };
        region
            .request(RegionRequest::UseBucket(BucketUse {
                player: self.conn.handle.clone(),
                position: self.pose.position,
                eye,
                look: crate::world::raycast::view_vector(self.pose.yaw, self.pose.pitch),
                reach: BLOCK_INTERACTION_RANGE,
                off_hand,
                item,
                fluid,
                mob,
                mob_data,
                mob_components,
                sneaking: self.pose.sneaking,
                creative: self.mode.game_mode == GameMode::Creative,
                sequence,
                hit: None,
                pay,
            }))
            .await;
        true
    }

    /// `Player.canEat`: invulnerable players and always-edible food ignore
    /// a full hunger bar.
    pub fn can_eat(&self, can_always_eat: bool) -> bool {
        self.mode.abilities.invulnerable || can_always_eat || self.body.food.needs_food()
    }

    pub fn cooldown_group(item: Item) -> &'static str {
        item.use_cooldown()
            .and_then(|cooldown| cooldown.group)
            .unwrap_or(item.name())
    }

    pub fn stack_cooldown_group(stack: &ItemStack) -> String {
        stack
            .use_cooldown()
            .and_then(|cooldown| cooldown.group)
            .unwrap_or_else(|| stack.item().name().to_owned())
    }

    pub fn stack_on_cooldown(&self, stack: &ItemStack) -> bool {
        self.actions
            .cooldowns
            .get(&Self::stack_cooldown_group(stack))
            .is_some_and(|end| *end > self.tick)
    }

    /// Starts a stack's use cooldown (see [`ItemStack::use_cooldown`]).
    pub fn add_stack_cooldown(&mut self, item: Item, cooldown: StackCooldown) {
        let group = cooldown.group.unwrap_or_else(|| item.name().to_owned());
        self.actions
            .cooldowns
            .insert(group.clone(), self.tick + u64::from(cooldown.ticks));
        self.send(&play::Cooldown {
            group: &group,
            ticks: cooldown.ticks as i32,
        });
    }

    pub fn on_cooldown(&self, item: Item) -> bool {
        self.actions
            .cooldowns
            .get(Self::cooldown_group(item))
            .is_some_and(|end| *end > self.tick)
    }

    pub fn add_cooldown(&mut self, item: Item, ticks: u32) {
        let group = Self::cooldown_group(item);
        self.actions
            .cooldowns
            .insert(group.to_owned(), self.tick + ticks as u64);
        self.send(&play::Cooldown {
            group,
            ticks: ticks as i32,
        });
    }

    /// Whether gliding is possible: not on the ground, not riding, not levitating,
    /// not creative-flying, and the chest slot carries a usable elytra.
    pub fn can_glide(&self) -> bool {
        player::Glide::can_glide(
            self.pose.on_ground,
            self.ride.riding.is_some(),
            self.body.effects.has(MobEffect::Levitation),
            self.mode.abilities.flying,
            self.inventory.equipped(EquipmentSlot::Chest),
        )
    }

    /// Player Command's "start fall flying" (jumping while falling with an
    /// elytra worn).
    pub async fn try_start_gliding(&mut self) {
        let can_glide = self.can_glide();
        if self
            .motion
            .glide
            .try_start(can_glide, self.body.fall.in_water())
        {
            self.motion.glide_velocity = self.sync.known_movement;
            self.motion.glide_speed =
                self.sync.known_movement[0].hypot(self.sync.known_movement[2]);
            self.publish(false).await;
        }
    }

    /// Advances one tick of elytra flight: the tracked velocity, an
    /// in-progress firework boost, the durability tick, and stopping on its
    /// own once [`Self::can_glide`] no longer holds.
    pub async fn tick_gliding(&mut self) {
        let can_glide = self.can_glide();
        if self.motion.glide.active {
            let gravity = self.attribute(Attribute::Gravity);
            self.motion.glide_velocity = player::glide_step(
                self.motion.glide_velocity,
                self.pose.yaw,
                self.pose.pitch,
                gravity,
            );
            if self.motion.firework_boost_ticks > 0 {
                self.motion.firework_boost_ticks -= 1;
                let look = crate::world::raycast::view_vector(self.pose.yaw, self.pose.pitch);
                self.motion.glide_velocity =
                    player::firework_boost(self.motion.glide_velocity, look);
            }
        }
        if self.motion.glide.tick(can_glide) {
            self.wear_slot(EquipmentSlot::Chest, 1).await;
        }
    }

    pub fn stop_using(&mut self) {
        self.actions.using = None;
    }

    /// Gives this player a status effect (instant ones act at once).
    pub async fn add_effect(&mut self, effect: ActiveEffect) {
        if let Some(action) = effects::instant_action(effect.effect, effect.amplifier, 1.0) {
            self.apply_effect_action(action).await;
            return;
        }
        let change = self.body.effects.add(effect);
        self.apply_effect_change(change).await;
    }

    pub async fn apply_effect_change(&mut self, change: Option<EffectChange>) {
        self.apply_effect_change_from(change, None).await;
    }

    pub async fn apply_effect_change_from(
        &mut self,
        change: Option<EffectChange>,
        source: Option<&crate::world::predicate::EntityFacts>,
    ) {
        let Some(change) = change else {
            return;
        };
        let blend = matches!(change, EffectChange::Added(_));
        match change {
            EffectChange::Added(effect) | EffectChange::Updated(effect) => {
                self.fire(crate::world::advancement::trigger::Event::EffectsChanged { source });
                if effect.effect == MobEffect::Absorption {
                    self.body.absorption = self
                        .body
                        .absorption
                        .max(effects::max_absorption(effect.amplifier));
                }
                self.send(&play::UpdateMobEffect {
                    entity_id: self.id,
                    effect: effect.effect.id() as i32,
                    amplifier: effect.amplifier as i32,
                    duration: effect.duration.clamp(-1, i32::MAX as i64) as i32,
                    flags: effect.flags() | if blend { play::effect_flags::BLEND } else { 0 },
                });
            }
            EffectChange::Removed(kind) => {
                if kind == MobEffect::Absorption {
                    self.body.absorption = 0.0;
                }
                self.send(&play::RemoveMobEffect {
                    entity_id: self.id,
                    effect: kind.id() as i32,
                });
                self.fire(crate::world::advancement::trigger::Event::EffectsChanged {
                    source: None,
                });
            }
        }
    }

    pub async fn apply_effect_action(&mut self, action: EffectAction) {
        match action {
            EffectAction::Heal(amount) => self.body.health.heal(amount, self.max_health()),
            EffectAction::Poison(amount) => {
                if self.body.health.value > 1.0 {
                    self.damage(amount, &damage_type::MAGIC, None).await;
                }
            }
            EffectAction::MagicDamage(amount) => {
                self.damage(amount, &damage_type::MAGIC, None).await;
            }
            EffectAction::WitherDamage(amount) => {
                self.damage(amount, &damage_type::WITHER, None).await;
            }
            EffectAction::Exhaust(amount) => self.exhaust(amount),
            EffectAction::Feed {
                nutrition,
                saturation,
            } => {
                let before = self.body.food.level;
                self.body.food.eat(nutrition, saturation);
                self.food_changed_hooks(before);
            }
        }
    }

    pub async fn tick_effects(&mut self) {
        if self.is_dead() {
            return;
        }
        let (actions, changes) = self
            .body
            .effects
            .tick(self.tick as i64, self.body.absorption);
        for action in actions {
            self.apply_effect_action(action).await;
        }
        for change in changes {
            self.apply_effect_change(Some(change)).await;
        }
        self.actions.cooldowns.retain(|_, end| *end > self.tick);
    }

    /// Compasses locked onto a lodestone drop their target once the lodestone
    /// is gone from a loaded chunk or the position leaves the world.
    pub async fn tick_lodestone_compasses(&mut self) {
        if !self.tick.is_multiple_of(LODESTONE_CHECK_INTERVAL) {
            return;
        }
        let key = self.dimension().key.clone();
        let mut checks = Vec::new();
        for (slot, stack) in self.inventory.slots().iter().enumerate() {
            let Some(stack) = stack.as_ref().filter(|s| s.item() == Item::Compass) else {
                continue;
            };
            let Some(tracker) = stack
                .components
                .other
                .get(DataComponentType::LodestoneTracker.name())
                .and_then(LodestoneTracker::from_nbt)
            else {
                continue;
            };
            if let (true, Some((dimension, [x, y, z]))) = (tracker.tracked, &tracker.target)
                && *dimension == key
            {
                checks.push((slot, BlockPos::new(*x, *y, *z)));
            }
        }
        for (slot, pos) in checks {
            let gone = if self.dimension().contains_block(pos) {
                let Some(region) = self.region_of(pos) else {
                    continue;
                };
                let (reply, answer) = oneshot::channel();
                region.request(RegionRequest::BlockAt { pos, reply }).await;
                let Ok(Some(state)) = answer.await else {
                    continue;
                };
                state.block() != Block::Lodestone
            } else {
                true
            };
            if !gone {
                continue;
            }
            let Some(stack) = self.inventory.slot_mut(slot).as_mut() else {
                continue;
            };
            stack.components.other.insert(
                DataComponentType::LodestoneTracker.name(),
                LodestoneTracker {
                    target: None,
                    tracked: true,
                }
                .to_nbt(),
            );
        }
    }

    /// Middle click on a block: brings its item to the hand, or gives it
    /// in creative, where `include_data` also copies the block entity.
    pub async fn pick_block(&mut self, position: BlockPos, include_data: bool) {
        let range = self.attribute(Attribute::BlockInteractionRange) + 1.0;
        if distance_sq_to_block(self.eye(), position) >= range * range {
            return;
        }
        let Some(region) = self.region_of(position) else {
            return;
        };
        let (reply, answer) = oneshot::channel();
        region
            .request(RegionRequest::BlockWithEntity {
                pos: position,
                reply,
            })
            .await;
        let Ok(Some((state, saved))) = answer.await else {
            return;
        };
        let include_data = include_data && self.mode.abilities.instabuild;
        let Some(stack) = picked_block_stack(state, saved.as_ref(), include_data) else {
            return;
        };
        self.try_pick(stack);
    }

    /// Middle click on an entity: its spawn egg or placing item.
    pub async fn pick_entity(&mut self, entity: i32, include_data: bool) {
        if let Some((position, handle)) = self.view.entities.visible_player(entity) {
            let id = handle.id;
            let range = self.attribute(Attribute::EntityInteractionRange) + 3.0;
            if include_data
                && self.mode.game_mode == GameMode::Creative
                && self.mode.permission >= PermissionLevel::Gamemasters
                && distance_sq(self.eye(), position) <= range * range
            {
                self.print_profile_of(id).await;
            }
            return;
        }
        let (visible, slack) = match self.view.entities.visible_entity(entity) {
            Some(visible) => (visible, 0.0),
            None => {
                let Some(owner) = (1..=tuff_vanilla_data::dragon::PARTS.len() as i32)
                    .filter_map(|offset| self.view.entities.visible_entity(entity - offset))
                    .find(|(kind, _, _)| {
                        *kind == tuff_vanilla_data::registry::EntityType::EnderDragon
                    })
                else {
                    return;
                };
                (owner, DRAGON_PART_REACH)
            }
        };
        let (kind, chunk, position) = visible;
        let range = self.attribute(Attribute::EntityInteractionRange) + 3.0 + slack;
        if distance_sq(self.eye(), position) > range * range {
            return;
        }
        if matches!(
            kind,
            tuff_vanilla_data::registry::EntityType::ItemFrame
                | tuff_vanilla_data::registry::EntityType::GlowItemFrame
        ) && let Some(framed) = self.framed_item(entity, chunk).await
        {
            self.try_pick(framed);
            return;
        }
        if let Some(item) = picked_entity_item(kind) {
            #[expect(
                clippy::disallowed_methods,
                reason = "item turning into another (pick_entity)"
            )]
            self.try_pick(ItemStack::mint(item, 1, MintReason::Transform));
        }
    }

    pub async fn query_block_entity_tag(&mut self, transaction: i32, position: BlockPos) {
        if self.mode.permission < PermissionLevel::Gamemasters {
            return;
        }
        let Some(region) = self.region_of(position) else {
            return;
        };
        let (reply, answer) = oneshot::channel();
        region
            .request(RegionRequest::BlockWithEntity {
                pos: position,
                reply,
            })
            .await;
        let Ok(Some((_, saved))) = answer.await else {
            return;
        };
        let tag = saved.map(tuff_nbt::Tag::Compound);
        self.send(&play::TagQuery {
            transaction_id: transaction,
            tag: tag.as_ref(),
        });
    }

    pub async fn query_entity_tag(&mut self, transaction: i32, entity: i32) {
        if self.mode.permission < PermissionLevel::Gamemasters {
            return;
        }
        let Some((_, chunk, _)) = self.view.entities.visible_entity(entity) else {
            return;
        };
        let Some(region) = self.region(chunk.region()).await else {
            return;
        };
        let (reply, answer) = oneshot::channel();
        region
            .request(RegionRequest::EntityData { id: entity, reply })
            .await;
        let Ok(Some(data)) = answer.await else {
            return;
        };
        let tag = tuff_nbt::Tag::Compound(data);
        self.send(&play::TagQuery {
            transaction_id: transaction,
            tag: Some(&tag),
        });
    }

    async fn print_profile_of(&mut self, player: i32) {
        let (reply, answer) = oneshot::channel();
        self.coordinator(CoordinatorMessage::ProfileOf { player, reply })
            .await;
        if let Ok(Some(profile)) = answer.await {
            let message = crate::profile_text::fetched_profile_message(&profile);
            self.system_message(&message);
        }
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "creative inventory, pick-block or creative copy (framed_item)"
    )]
    async fn framed_item(&mut self, entity: i32, chunk: ChunkPos) -> Option<ItemStack> {
        let region = self.region(chunk.region()).await?;
        let (reply, answer) = tokio::sync::oneshot::channel();
        region
            .request(RegionRequest::EntityData { id: entity, reply })
            .await;
        let data = answer.await.ok()??;
        data.get_compound("Item")
            .and_then(|nbt| ItemStack::mint_from_nbt(nbt, MintReason::Creative))
    }

    fn try_pick(&mut self, stack: ItemStack) {
        self.inventory.pick(stack, self.mode.abilities.instabuild);
        self.send(&play::SetHeldSlot(self.inventory.selected() as i32));
        self.send_inventory();
    }

    /// What using the stack in a hand costs it.
    pub async fn apply_hand_effect(&mut self, off_hand: bool, effect: HandEffect) {
        let creative = self.mode.abilities.instabuild;
        match effect {
            HandEffect::None => {}
            HandEffect::Consume(count) => {
                if !creative {
                    self.inventory.consume_from_hand(off_hand, count);
                }
            }
            HandEffect::Damage(amount) => self.hurt_hand(off_hand, amount).await,
            HandEffect::Emptied(result) => {
                if !creative {
                    let slot = if off_hand {
                        Inventory::OFFHAND_SLOT
                    } else {
                        Inventory::hotbar_slot(self.inventory.selected())
                    };
                    if let Some(old) = self.inventory.slot_mut(slot).take() {
                        old.burn(BurnReason::Transform);
                    }
                    #[expect(
                        clippy::disallowed_methods,
                        reason = "item turning into another (apply_hand_effect)"
                    )]
                    self.inventory.set_slot(
                        slot,
                        Some(ItemStack::mint(result, 1, MintReason::Transform)),
                    );
                }
            }
            HandEffect::Exchange(result) => {
                #[expect(
                    clippy::disallowed_methods,
                    reason = "item turning into another (apply_hand_effect)"
                )]
                self.apply_exchange(
                    off_hand,
                    creative,
                    ItemStack::mint(result, 1, MintReason::Transform),
                )
                .await;
            }
            HandEffect::ExchangeStack(result) => {
                self.apply_exchange(off_hand, creative, *result).await;
            }
            HandEffect::Split(result) => {
                let lone = !creative
                    && self
                        .inventory
                        .hand(off_hand)
                        .is_some_and(|h| h.count() == 1);
                if lone {
                    let slot = if off_hand {
                        Inventory::OFFHAND_SLOT
                    } else {
                        Inventory::hotbar_slot(self.inventory.selected())
                    };
                    self.inventory.set_slot(slot, Some(*result));
                } else {
                    if !creative {
                        self.inventory.consume_from_hand(off_hand, 1);
                    }
                    if let Some(extra) = self.inventory.add(*result) {
                        self.throw_item(extra).await;
                    }
                }
            }
            HandEffect::Spent(_) => {}
            HandEffect::Replace(stack) => {
                let slot = if off_hand {
                    Inventory::OFFHAND_SLOT
                } else {
                    Inventory::hotbar_slot(self.inventory.selected())
                };
                self.inventory.set_slot(slot, stack.map(|s| *s));
            }
        }
    }

    /// Shared body of [`HandEffect::Exchange`] and [`HandEffect::ExchangeStack`].
    pub async fn apply_exchange(&mut self, off_hand: bool, creative: bool, result: ItemStack) {
        if creative {
            let spare = match self.inventory.find_item(result.item()) {
                None => self.inventory.add(result),
                Some(_) => Some(result),
            };
            if let Some(spare) = spare {
                spare.burn(BurnReason::Creative);
            }
            return;
        }
        self.inventory.consume_from_hand(off_hand, 1);
        if self.inventory.hand(off_hand).is_none() {
            let slot = if off_hand {
                Inventory::OFFHAND_SLOT
            } else {
                Inventory::hotbar_slot(self.inventory.selected())
            };
            self.inventory.set_slot(slot, Some(result));
        } else if let Some(extra) = self.inventory.add(result) {
            self.throw_item(extra).await;
        }
    }

    pub async fn throw_item(&mut self, stack: ItemStack) {
        let (yaw, pitch) = (
            (self.pose.yaw as f64).to_radians(),
            (self.pose.pitch as f64).to_radians(),
        );
        let angle = self.rng.gen_range(0.0..std::f64::consts::TAU);
        let spread = self.rng.gen_range(0.0..0.02);
        let velocity = [
            -yaw.sin() * pitch.cos() * THROW_SPEED + angle.cos() * spread,
            -pitch.sin() * THROW_SPEED + 0.1 + self.rng.gen_range(-0.05..0.05),
            yaw.cos() * pitch.cos() * THROW_SPEED + angle.sin() * spread,
        ];
        let eye = self.eye();
        let position = [eye[0], eye[1] - 0.3, eye[2]];
        let region = ChunkPos::of_position(position).region();
        let handle = self.region(region).await;
        if let Some(handle) = &handle
            && !self.may_drop_into(handle, &stack, position)
        {
            self.keep_unspawned(stack);
            return;
        }
        let mut item = self.thrown_item(stack, position, velocity);
        let back = match handle {
            Some(handle) => match handle.try_request(RegionRequest::SpawnEntity(item)).await {
                Ok(()) => Vec::new(),
                Err(request) => match *request {
                    RegionRequest::SpawnEntity(mut item) => item.take_items(),
                    _ => Vec::new(),
                },
            },
            None => item.take_items(),
        };
        for stack in back {
            self.keep_unspawned(stack);
        }
    }

    /// Puts back a stack whose item entity could not be spawned.
    /// Whether items may be dropped into the region behind `to`: not
    /// while it or this player is quarantined, so nothing reaches state
    /// that will not be saved. An injected failure is a violation: the
    /// player and the region are quarantined and the player is kicked.
    fn may_drop_into(&mut self, to: &RegionHandle, stack: &ItemStack, position: [f64; 3]) -> bool {
        use crate::world::conservation::Transfer;
        self.may_transfer(
            Transfer::Drop,
            Some((to.dimension, to.pos)),
            BlockPos::containing(position),
            Some(stack),
        )
    }

    /// Whether a `transfer` between this player and the region `region`
    /// (or only this player) may run: not while a side is quarantined.
    /// An injected failure is a violation quarantining both sides and
    /// kicking the player. Refused, nothing moves.
    pub(crate) fn may_transfer(
        &mut self,
        transfer: crate::world::conservation::Transfer,
        region: Option<(DimensionId, RegionPos)>,
        pos: BlockPos,
        stack: Option<&ItemStack>,
    ) -> bool {
        use crate::world::conservation::Subject;
        let conservation = &self.shared.conservation;
        let region = region.map(|(dimension, pos)| (dimension, Subject::Region(dimension, pos)));
        if conservation.quarantined(Subject::Player(self.id))
            || region.is_some_and(|(_, region)| conservation.quarantined(region))
        {
            return false;
        }
        if !conservation.take_injected(transfer) {
            return true;
        }
        self.transfer_violation(
            transfer,
            region,
            pos,
            stack,
            "the transfer check was made to fail".to_owned(),
        );
        false
    }

    /// The release check of a `transfer` from this player's side: the
    /// items held `before` plus those `received` equal those held `after`
    /// plus those `sent` on. A mismatch is a violation; the moves already
    /// ran, so the player is quarantined and kicked and the answer is false.
    pub(crate) fn transfer_balanced(
        &mut self,
        transfer: crate::world::conservation::Transfer,
        before: u64,
        received: u64,
        after: u64,
        sent: u64,
    ) -> bool {
        if before + received == after + sent || !tuff_item::item::conservation::checks() {
            return true;
        }
        self.transfer_violation(
            transfer,
            None,
            BlockPos::containing(self.pose.position),
            None,
            format!("held {before} and received {received}, but holds {after} and sent {sent}"),
        );
        false
    }

    fn transfer_violation(
        &mut self,
        transfer: crate::world::conservation::Transfer,
        region: Option<(DimensionId, crate::world::conservation::Subject)>,
        pos: BlockPos,
        stack: Option<&ItemStack>,
        detail: String,
    ) {
        use crate::world::conservation::{ItemViolation, Subject};
        let violation = ItemViolation {
            operation: transfer.operation(),
            player: Some(self.id),
            pos,
            item: stack.map(tuff_item::item::ItemSnapshot::of),
            detail,
        };
        tuff_item::item::conservation::violation(
            violation.operation,
            format_args!("player {:?}: {}", self.id, violation.detail),
        );
        let dimension = region.map_or(self.dimension, |(dimension, _)| dimension);
        self.shared.services.item_violation(
            crate::world::services::ViolationCulprit::Player {
                uuid: self.profile.id,
                name: &self.profile.name,
            },
            dimension,
            &violation,
        );
        let conservation = &self.shared.conservation;
        if let Some((_, region)) = region {
            conservation.quarantine(region);
        }
        conservation.quarantine(Subject::Player(self.id));
        let kick = self
            .shared
            .config
            .get_bool(crate::config::ITEMS_CONSERVATION_KICK)
            .unwrap_or(true);
        if kick {
            self.kick(tuff_item::text::TextComponent::from(
                "An item operation was refused; reconnect to reload your inventory",
            ));
        }
    }

    fn keep_unspawned(&mut self, stack: ItemStack) {
        if let Some(lost) = self.inventory.add(stack) {
            log::warn!(
                "{} lost {lost:?}: no region took the drop",
                self.profile.name
            );
        }
        self.send_inventory();
    }

    /// A dropped item entity that remembers this player threw it.
    fn thrown_item(
        &mut self,
        stack: ItemStack,
        position: [f64; 3],
        velocity: [f64; 3],
    ) -> Box<dyn crate::world::entity::RegionEntity> {
        let mut item = crate::world::items::ItemEntity::new(
            self.shared.next_entity_id(),
            self.rng.r#gen(),
            stack,
            position,
            velocity,
            player::THROWN_PICKUP_DELAY,
        );
        item.base.extra.insert(
            crate::world::items::THROWER,
            tuff_nbt::Tag::uuid(self.profile.id),
        );
        Box::new(item)
    }

    pub async fn spawn_item(
        &mut self,
        stack: ItemStack,
        position: [f64; 3],
        velocity: [f64; 3],
        pickup_delay: u32,
    ) {
        let region = ChunkPos::of_position(position).region();
        let back = match self.region(region).await {
            Some(handle) if !self.may_drop_into(&handle, &stack, position) => Some(stack),
            Some(handle) => {
                let request = RegionRequest::SpawnItem {
                    stack,
                    position,
                    velocity,
                    pickup_delay,
                };
                match handle.try_request(request).await {
                    Ok(()) => None,
                    Err(request) => match *request {
                        RegionRequest::SpawnItem { stack, .. } => Some(stack),
                        _ => None,
                    },
                }
            }
            None => Some(stack),
        };
        if let Some(stack) = back {
            self.keep_unspawned(stack);
        }
    }

    /// Adds collected experience, letting a worn Mending item spend it on
    /// repairs first, then plays the level-up sound when due.
    pub async fn give_experience(&mut self, amount: i32) {
        if self.is_dead() {
            return;
        }
        let leftover = self.inventory.mend(amount, &mut self.rng);
        if leftover != amount {
            self.send_inventory();
        }
        if leftover == 0 {
            return;
        }
        self.body.score = self.body.score.saturating_add(leftover);
        if let Some(volume) = self.body.experience.give_points(leftover, self.tick) {
            let sound = packets([framed(&play::Sound {
                sound_event: SoundEvent::EntityPlayerLevelup.id(),
                category: play::sound_category::PLAYERS,
                position: self.pose.position,
                volume,
                pitch: 1.0,
                seed: self.rng.r#gen(),
            })]);
            self.broadcast_chunk(sound).await;
        }
    }

    /// Every active effect, for a client that just (re)entered a world.
    pub fn send_effects(&mut self) {
        let packets: Vec<_> = self
            .body
            .effects
            .iter()
            .map(|effect| play::UpdateMobEffect {
                entity_id: self.id,
                effect: effect.effect.id() as i32,
                amplifier: effect.amplifier as i32,
                duration: effect.duration.clamp(-1, i32::MAX as i64) as i32,
                flags: effect.flags(),
            })
            .collect();
        for packet in &packets {
            self.send(packet);
        }
    }

    pub fn send_experience(&mut self) {
        let current = (
            self.body.experience.progress,
            self.body.experience.level,
            self.body.experience.total,
        );
        if self.published.last_sent_experience == Some(current) {
            return;
        }
        self.published.last_sent_experience = Some(current);
        self.send(&play::SetExperience {
            progress: current.0,
            level: current.1,
            total: current.2,
        });
    }

    pub async fn tick_destroying(&mut self) {
        let Some(destroying) = self.actions.destroying else {
            return;
        };
        if self.is_dead() || self.mode.abilities.instabuild {
            self.actions.destroying = None;
            return;
        }
        let Some(handle) = self.region_of(destroying.pos) else {
            self.actions.destroying = None;
            return;
        };
        let (event, data) = destroying.event(self.tick);
        handle
            .request(RegionRequest::DestroyProgress(Box::new(DestroyTick {
                player: self.conn.handle.clone(),
                pos: destroying.pos,
                event,
                data,
                ticks: destroying.ticks(self.tick),
                tool: self
                    .inventory
                    .hand(false)
                    .map(tuff_item::item::ItemSnapshot::of),
                miner: self.miner(),
                eye: self.eye(),
            })))
            .await;
    }
}

/// How far a dragon part can sit from the dragon's own position.
const DRAGON_PART_REACH: f64 = 12.0;

/// Squared distance from `point` to the nearest point of the block's cube.
pub fn distance_sq_to_block(point: [f64; 3], block: BlockPos) -> f64 {
    let min = [block.x as f64, block.y as f64, block.z as f64];
    (0..3)
        .map(|axis| {
            let d = (min[axis] - point[axis])
                .max(point[axis] - (min[axis] + 1.0))
                .max(0.0);
            d * d
        })
        .sum()
}

/// What picking `state` gives: its pick item, with the components the
/// block always carries over (banner patterns, pot sherds, statue pose,
/// light level) and, when `include_data`, the block entity's data and
/// components.
#[expect(
    clippy::disallowed_methods,
    reason = "pick-block stack for a block, creative inventory (picked_block_stack)"
)]
pub fn picked_block_stack(
    state: tuff_vanilla_data::BlockState,
    saved: Option<&tuff_nbt::Compound>,
    include_data: bool,
) -> Option<ItemStack> {
    use crate::world::block_entity::raw;
    use tuff_nbt::Compound;
    use tuff_vanilla_data::registry::{BlockEntityType, DataComponentType};
    let item = state.pick_item()?;
    let kind = saved.and_then(raw::kind);
    let mut components = Compound::new();
    if let (Some(nbt), Some(kind)) = (saved, kind) {
        match kind {
            BlockEntityType::Banner | BlockEntityType::CopperGolemStatue => {
                components = raw::collect_components(kind, nbt);
            }
            BlockEntityType::DecoratedPot => {
                let sherds = DataComponentType::PotDecorations.name();
                if let Some(value) = raw::collect_components(kind, nbt).get(sherds) {
                    components.insert(sherds, value.clone());
                }
            }
            _ => {}
        }
    }
    let mut properties = Compound::new();
    if let Some(pick) = state.block().pick_state() {
        let copied: &[&str] = if kind == Some(BlockEntityType::CopperGolemStatue) {
            &["copper_golem_pose"]
        } else if pick.always || include_data {
            pick.properties
        } else {
            &[]
        };
        for name in copied {
            if let Some(value) = state.get(name) {
                properties.insert(*name, value);
            }
        }
        let default = pick
            .item_default
            .iter()
            .all(|(name, value)| properties.get_str(name) == Some(*value))
            && properties.len() == pick.item_default.len();
        if default {
            properties = Compound::new();
        }
    }
    if !properties.is_empty() {
        components.insert(DataComponentType::BlockState.name(), properties);
    }
    if let (Some(nbt), Some(kind), true) = (saved, kind, include_data) {
        for (key, value) in raw::picked_components(kind, nbt).iter() {
            components.insert(key, value.clone());
        }
    }
    Some(ItemStack::mint_with(
        item,
        1,
        &tuff_item::item::Components::from_nbt(&components),
        MintReason::Transform,
    ))
}

/// The item picking an entity of `kind` gives: a mob's spawn egg, a
/// boat's item, or the item that places the entity.
pub fn picked_entity_item(kind: tuff_vanilla_data::registry::EntityType) -> Option<Item> {
    use tuff_vanilla_data::registry::EntityType;
    if let Some(egg) = Item::from_name(&format!("{}_spawn_egg", kind.name())) {
        return Some(egg);
    }
    if let Some(boat) = Item::of_boat(kind) {
        return Some(boat);
    }
    match kind {
        EntityType::LeashKnot => Some(Item::Lead),
        EntityType::Minecart
        | EntityType::ChestMinecart
        | EntityType::FurnaceMinecart
        | EntityType::TntMinecart
        | EntityType::HopperMinecart
        | EntityType::CommandBlockMinecart
        | EntityType::SpawnerMinecart
        | EntityType::ArmorStand
        | EntityType::ItemFrame
        | EntityType::GlowItemFrame
        | EntityType::Painting
        | EntityType::EndCrystal => Item::from_name(kind.name()),
        _ => None,
    }
}

#[cfg(test)]
#[path = "hands_pick_tests.rs"]
mod pick_tests;
