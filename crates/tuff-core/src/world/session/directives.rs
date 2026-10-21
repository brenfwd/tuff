//! Commands acting on this player, applied by its own actor so nothing
//! else ever touches its state.

use rand::Rng;
use tuff_vanilla_data::damage_type;
use tuff_vanilla_data::registry::SoundEvent;

use super::*;
use crate::protocol::play;
use crate::storage::player_data::RespawnSave;
use crate::world::commands::gamemode;
use crate::world::effects::{self, ActiveEffect};
use crate::world::inventory::Inventory;
use crate::world::messages::{
    AttributeAnswer, AttributeOp, DialogRef, Directive, DirectiveReply, DirectiveSource,
    PostEffectOp, SlotFill, Teleport, TitleAction,
};
use tuff_item::item::ItemStack;
use tuff_item::text::TextComponent;

#[derive(Clone, Copy)]
enum GameSlot {
    Window(usize),
    Cursor,
    Ender(usize),
}
use crate::world::player::Fall;
use crate::world::{EYE_HEIGHT, block_contact, distance_sq, framed, packets};

impl PlayerSession {
    pub fn reply(
        &self,
        value: i32,
        affected: bool,
        error: Option<TextComponent>,
    ) -> DirectiveReply {
        DirectiveReply {
            player: self.id,
            name: self.profile.name.clone(),
            value,
            affected,
            error,
            attribute: None,
            post_effects: Vec::new(),
        }
    }

    pub async fn apply_directive(
        &mut self,
        directive: Directive,
        source: DirectiveSource,
    ) -> DirectiveReply {
        match directive {
            Directive::Advancement {
                grant,
                tree,
                ids,
                criterion,
                show,
            } => {
                let current = self.progress.advancement_tree.clone();
                let ids: Vec<_> = ids
                    .into_iter()
                    .filter_map(|id| current.same_as(&*tree, id))
                    .collect();
                let count = current.set(self, grant, &ids, criterion.as_deref(), show);
                self.apply_advancement_rewards().await;
                self.reply(count, true, None)
            }
            Directive::Recipes { give, recipes } => {
                let set = self.shared.recipes();
                let all = || set.names();
                let count = if give {
                    let (entries, learned) = match &recipes {
                        Some(recipes) => self
                            .progress
                            .recipe_book
                            .learn(&set, recipes.iter().map(|r| &**r)),
                        None => self.progress.recipe_book.learn(&set, all()),
                    };
                    let count = entries.len() as i32;
                    self.send_recipe_entries(entries);
                    for recipe in learned {
                        self.fire(crate::world::advancement::trigger::Event::RecipeUnlocked(
                            &recipe,
                        ));
                    }
                    count
                } else {
                    let removed = match &recipes {
                        Some(recipes) => self
                            .progress
                            .recipe_book
                            .remove(&set, recipes.iter().map(|r| &**r)),
                        None => self.progress.recipe_book.remove(&set, all()),
                    };
                    if !removed.is_empty() {
                        self.send(&play::RecipeBookRemove(&removed));
                    }
                    removed.len() as i32
                };
                self.apply_advancement_rewards().await;
                self.reply(count, true, None)
            }
            Directive::GameMode(mode) => {
                let changed = self.set_game_mode(mode).await;
                if changed {
                    if source.player == Some(self.id) {
                        self.system_message(&gamemode::set_self(mode));
                    } else {
                        self.system_message(&gamemode::changed(mode));
                    }
                }
                self.reply(1, changed, None)
            }
            Directive::Give { stack, count } => {
                self.give_items(stack, count).await;
                self.reply(1, true, None)
            }
            Directive::Clear {
                predicate,
                max_count,
            } => {
                let removed = self.clear_items(predicate.as_ref(), max_count);
                self.reply(removed, removed > 0, None)
            }
            Directive::Spectate {
                target,
                target_name,
                untrackable,
            } => {
                if target == Some(self.id) {
                    let error = TextComponent::translate("commands.spectate.self", []);
                    return self.reply(0, true, Some(error));
                }
                if self.mode.game_mode != crate::protocol::play::GameMode::Spectator {
                    let error = TextComponent::translate(
                        "commands.spectate.not_spectator",
                        [TextComponent::new(self.profile.name.clone())],
                    );
                    return self.reply(0, true, Some(error));
                }
                if untrackable {
                    let error = TextComponent::translate(
                        "commands.spectate.cannot_spectate",
                        [target_name],
                    );
                    return self.reply(0, true, Some(error));
                }
                match target {
                    None => self.reset_camera(),
                    Some(target) => self.force_camera(target).await,
                }
                self.reply(1, true, None)
            }
            Directive::Swing {
                hand,
                kind,
                duration,
            } => {
                let swing = packets([framed(&play::SwingAnimation {
                    entity_id: self.id,
                    hand,
                    kind,
                    duration_ticks: duration,
                })]);
                self.broadcast_entity(swing, true).await;
                self.reply(1, true, None)
            }
            Directive::PostEffect { op, value } => {
                let effects = &mut self.mode.post_effects;
                let changed = match (op, value) {
                    (PostEffectOp::Add, Some(id)) if !effects.contains(&id) => {
                        effects.push(id);
                        true
                    }
                    (PostEffectOp::Remove, Some(id)) => {
                        let before = effects.len();
                        effects.retain(|effect| *effect != id);
                        effects.len() != before
                    }
                    (PostEffectOp::Clear, _) => {
                        let any = !effects.is_empty();
                        effects.clear();
                        any
                    }
                    (PostEffectOp::List, _) => {
                        let listed = effects.clone();
                        let mut reply = self.reply(listed.len() as i32, true, None);
                        reply.post_effects = listed;
                        return reply;
                    }
                    _ => false,
                };
                if changed {
                    let list = self.mode.post_effects.clone();
                    self.send(&play::PostEffects { effects: &list });
                }
                self.reply(i32::from(changed), changed, None)
            }
            Directive::ShowDialog { dialog } => {
                match dialog {
                    DialogRef::Registry(id) => self.send(&play::ShowRegistryDialog { id }),
                    DialogRef::Inline(dialog) => {
                        if crate::dialog::validate(&dialog).is_err() {
                            return self.reply(0, false, None);
                        }
                        self.send(&play::ShowDialog { dialog })
                    }
                }
                self.reply(1, true, None)
            }
            Directive::ClearDialog => {
                self.send(&play::ClearDialog);
                self.reply(1, true, None)
            }
            Directive::Kill => {
                if self.is_dead() {
                    return self.reply(0, false, None);
                }
                self.body.health.value = 0.0;
                self.die(&damage_type::GENERIC_KILL, None).await;
                let by = HurtBy {
                    kind: &damage_type::GENERIC_KILL,
                    attacker: None,
                    projectile: None,
                    direct: false,
                };
                let result = HurtResult {
                    landed: true,
                    ..HurtResult::default()
                };
                let hooks = self.shared.session_hooks.clone();
                for hook in hooks.iter() {
                    hook.player_hurt(self, by.kind, &result, true);
                }
                for hook in hooks.iter() {
                    hook.player_damaged(self, &by, &result, true);
                }
                self.reply(1, true, None)
            }
            Directive::SetVitals { health, food } => {
                if self.is_dead() {
                    return self.reply(0, false, None);
                }
                if let Some(health) = health {
                    self.body.health.value = health.clamp(0.0, self.max_health());
                }
                if let Some(food) = food {
                    self.body.food.level = food.clamp(0, player::MAX_FOOD);
                }
                self.reply(1, true, None)
            }
            Directive::Respawn => {
                let dead = self.is_dead();
                self.respawn().await;
                self.reply(1, dead, None)
            }
            Directive::Particle {
                dimension,
                particle,
                options,
                force,
                position,
                delta,
                speed,
                count,
            } => {
                let center = [
                    self.pose.position[0].floor() + 0.5,
                    self.pose.position[1].floor() + 0.5,
                    self.pose.position[2].floor() + 0.5,
                ];
                let range = if force { 512.0 } else { 32.0 };
                let sent =
                    dimension == self.dimension && distance_sq(center, position) < range * range;
                if sent {
                    self.send(&play::ParticlesWithOptions {
                        particle,
                        options: &options,
                        force,
                        position,
                        spread: delta,
                        speed,
                        count,
                    });
                }
                self.reply(i32::from(sent), sent, None)
            }
            Directive::SetSlots {
                ids,
                name: _,
                stack,
                mode,
            } => {
                let (selected, changed) = self.set_game_slots(&ids, &stack, mode);
                self.reply(changed, selected > 0, None)
            }
            Directive::ModifySlots {
                ids,
                name: _,
                functions,
            } => {
                let (selected, changed) = self.modify_game_slots(&ids, &functions);
                self.reply(changed, selected > 0, None)
            }
            Directive::Hurt {
                kind,
                amount,
                position,
                attacker,
                direct,
            } => {
                let source = DamageSource {
                    kind,
                    attacker,
                    projectile: None,
                    position,
                    weapon: None,
                    direct,
                };
                let landed = self.hurt(source, amount).await.landed;
                self.reply(i32::from(landed), landed, None)
            }
            Directive::Teleport(teleport) => {
                self.leave_vehicle().await;
                self.teleport_by_command(teleport).await;
                self.reply(1, true, None)
            }
            Directive::Experience {
                amount,
                levels,
                set,
            } => {
                let tick = self.tick;
                let ok = match (set, levels) {
                    (false, false) => {
                        if let Some(volume) = self.body.experience.give_points(amount, tick) {
                            self.level_up_sound(volume).await;
                        }
                        true
                    }
                    (false, true) => {
                        if let Some(volume) = self.body.experience.give_levels(amount, tick) {
                            self.level_up_sound(volume).await;
                        }
                        true
                    }
                    (true, false) => {
                        if amount >= self.body.experience.needed_for_next_level() {
                            false
                        } else {
                            self.body.experience.progress =
                                amount as f32 / self.body.experience.needed_for_next_level() as f32;
                            true
                        }
                    }
                    (true, true) => {
                        self.body.experience.level = amount.max(0);
                        if amount < 0 {
                            self.body.experience.progress = 0.0;
                            self.body.experience.total = 0;
                        }
                        true
                    }
                };
                self.send_experience();
                self.reply(
                    1,
                    ok,
                    (!ok).then(|| {
                        TextComponent::translate("commands.experience.set.points.invalid", [])
                    }),
                )
            }
            Directive::ExperienceQuery { levels } => {
                let value = if levels {
                    self.body.experience.level
                } else {
                    (self.body.experience.progress
                        * self.body.experience.needed_for_next_level() as f32)
                        .floor() as i32
                };
                self.reply(value, true, None)
            }
            Directive::EffectGive {
                effect,
                duration,
                amplifier,
                particles,
            } => {
                let mut active = ActiveEffect::timed(effect, i64::from(duration), amplifier);
                active.show_particles = particles;
                let applied = self.add_effect_reporting(active).await;
                self.reply(1, applied, None)
            }
            Directive::EffectClear(None) => {
                let changes = self.body.effects.clear();
                let affected = !changes.is_empty();
                for change in changes {
                    self.apply_effect_change(Some(change)).await;
                }
                self.reply(1, affected, None)
            }
            Directive::EffectClear(Some(effect)) => {
                let change = self.body.effects.remove(effect);
                let affected = change.is_some();
                self.apply_effect_change(change).await;
                self.reply(1, affected, None)
            }
            Directive::Enchant { enchantment, level } => self.enchant_hand(&enchantment, level),
            Directive::SpawnPoint {
                dimension,
                pos,
                yaw,
                pitch,
            } => {
                self.mode.respawn = Some(RespawnSave {
                    dimension: self.shared.dimension(dimension).key.clone(),
                    pos: [pos.x, pos.y, pos.z],
                    yaw,
                    pitch,
                    forced: true,
                });
                self.reply(1, true, None)
            }
            Directive::Title(action) => {
                match action {
                    TitleAction::Title(text) => self.send(&play::SetTitleText(&text)),
                    TitleAction::Subtitle(text) => self.send(&play::SetSubtitleText(&text)),
                    TitleAction::ActionBar(text) => self.send(&play::SetActionBarText(&text)),
                    TitleAction::Times {
                        fade_in,
                        stay,
                        fade_out,
                    } => self.send(&play::SetTitlesAnimation {
                        fade_in,
                        stay,
                        fade_out,
                    }),
                    TitleAction::Clear => self.send(&play::ClearTitles { reset: false }),
                    TitleAction::Reset => self.send(&play::ClearTitles { reset: true }),
                }
                self.reply(1, true, None)
            }
            Directive::Payload { channel, data } => {
                self.send(&play::CustomPayload {
                    channel: &channel,
                    data: &data,
                });
                self.reply(1, true, None)
            }
            Directive::Tellraw(text) => {
                self.system_message(&text);
                self.reply(1, true, None)
            }
            Directive::PlaySound {
                sound,
                category,
                position,
                volume,
                pitch,
                min_volume,
                seed,
            } => {
                let max_distance = f64::from(if volume > 1.0 { 16.0 * volume } else { 16.0 });
                let distance_sq = distance_sq(position, self.pose.position);
                let (position, volume) = if distance_sq > max_distance * max_distance {
                    if min_volume <= 0.0 {
                        return self.reply(0, false, None);
                    }
                    let distance = distance_sq.sqrt();
                    let mut near = self.pose.position;
                    for axis in 0..3 {
                        near[axis] += (position[axis] - self.pose.position[axis]) / distance * 2.0;
                    }
                    (near, min_volume)
                } else {
                    (position, volume)
                };
                self.send(&play::NamedSound {
                    sound: &sound,
                    category,
                    position,
                    volume,
                    pitch,
                    seed,
                });
                self.reply(1, true, None)
            }
            Directive::StopSound { category, sound } => {
                self.send(&play::StopSound {
                    category,
                    sound: sound.as_deref(),
                });
                self.reply(1, true, None)
            }
            Directive::Transfer { host, port } => {
                self.send(&play::Transfer { host: &host, port });
                self.reply(1, true, None)
            }
            Directive::Kick(reason) => {
                self.kick(reason);
                self.reply(1, true, None)
            }
            Directive::PrivateMessage(message) => {
                self.system_message(&message);
                self.reply(1, true, None)
            }
            Directive::Tag(edit) => {
                let changed = match edit {
                    crate::world::messages::TagEdit::Add(name) => self.record.edit_tag(true, &name),
                    crate::world::messages::TagEdit::Remove(name) => {
                        self.record.edit_tag(false, &name)
                    }
                };
                if changed {
                    self.report_state().await;
                }
                self.reply(i32::from(changed), changed, None)
            }
            Directive::Waypoint(edit) => {
                let changed = self.record.edit_waypoint_icon(&edit);
                if changed {
                    self.report_state().await;
                }
                self.reply(1, true, None)
            }
            Directive::Attribute(command) => {
                let current = self.attribute_value(command.attribute);
                let answer = self.body.attributes.apply(&command, |_, _| current);
                let edits = matches!(
                    command.op,
                    AttributeOp::SetBase(_)
                        | AttributeOp::ResetBase
                        | AttributeOp::AddModifier { .. }
                        | AttributeOp::RemoveModifier(_)
                );
                if edits && !matches!(answer, AttributeAnswer::NoAttribute) {
                    self.sync_attributes(false).await;
                }
                let mut reply = self.reply(1, true, None);
                reply.attribute = Some(answer);
                reply
            }
        }
    }

    pub async fn level_up_sound(&mut self, volume: f32) {
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

    /// Adds an effect, saying whether it took (a weaker copy does not).
    pub async fn add_effect_reporting(&mut self, effect: ActiveEffect) -> bool {
        if let Some(action) = effects::instant_action(effect.effect, effect.amplifier, 1.0) {
            self.apply_effect_action(action).await;
            return true;
        }
        let change = self.body.effects.add(effect);
        let applied = change.is_some();
        self.apply_effect_change(change).await;
        applied
    }

    /// The window slot a game slot id names for a player; `None` for
    /// slots a player has none of.
    fn game_slot(&self, id: i32) -> Option<GameSlot> {
        Some(GameSlot::Window(match id {
            0..=8 => Inventory::hotbar_slot(id as usize),
            9..=35 => id as usize,
            98 => Inventory::hotbar_slot(self.inventory.selected()),
            99 => Inventory::OFFHAND_SLOT,
            100..=103 => 8 - (id - 100) as usize,
            500..=503 => 1 + (id - 500) as usize,
            499 => return Some(GameSlot::Cursor),
            200..=226 => return Some(GameSlot::Ender((id - 200) as usize)),
            _ => return None,
        }))
    }

    /// `/item` on this player: how many of `ids` the player has, and how
    /// many of them changed.
    pub fn set_game_slots(&mut self, ids: &[i32], stack: &ItemStack, mode: SlotFill) -> (i32, i32) {
        let (mut selected, mut changed) = (0, 0);
        for id in ids {
            let Some(slot) = self.game_slot(*id) else {
                continue;
            };
            let value = match mode {
                SlotFill::Fill => Some(Some(stack.clone())),
                SlotFill::Replace if selected == 0 => Some(Some(stack.clone())),
                SlotFill::Override => Some((selected == 0).then(|| stack.clone())),
                SlotFill::Replace => None,
            };
            selected += 1;
            let Some(value) = value else {
                continue;
            };
            self.write_game_slot(slot, value.filter(|stack| stack.count > 0));
            changed += 1;
        }
        if changed > 0 {
            self.send_inventory();
        }
        (selected, changed)
    }

    fn read_game_slot(&self, slot: GameSlot) -> Option<ItemStack> {
        match slot {
            GameSlot::Window(slot) => self.inventory.slots().get(slot).cloned().flatten(),
            GameSlot::Cursor => self.inventory.carried().cloned(),
            GameSlot::Ender(index) => {
                let container = match &self.menus.menu {
                    Some(OpenMenu {
                        backing: MenuBacking::EnderChest(_),
                        container,
                        ..
                    }) => container,
                    _ => &self.menus.ender_items,
                };
                container.get(index).cloned().flatten()
            }
        }
    }

    /// `/item modify` on this player: how many of `ids` hold an item, and
    /// how many of them were rewritten.
    pub fn modify_game_slots(
        &mut self,
        ids: &[i32],
        functions: &[tuff_vanilla_data::loot_table::LootFunction],
    ) -> (i32, i32) {
        let (mut selected, mut changed) = (0, 0);
        let loot = self.shared.block_loot();
        let mut rng = rand::rngs::StdRng::from_entropy();
        for id in ids {
            let Some(slot) = self.game_slot(*id) else {
                continue;
            };
            let stack = self
                .read_game_slot(slot)
                .unwrap_or_else(|| ItemStack::new(tuff_vanilla_data::registry::Item::Air, 0));
            selected += 1;
            let result = loot.modify_item(functions, stack, &mut rng);
            self.write_game_slot(slot, Some(result).filter(|stack| stack.count > 0));
            changed += 1;
        }
        if changed > 0 {
            self.send_inventory();
        }
        (selected, changed)
    }

    fn write_game_slot(&mut self, slot: GameSlot, value: Option<ItemStack>) {
        match slot {
            GameSlot::Window(slot) => self.inventory.set_slot(slot, value),
            GameSlot::Cursor => self.inventory.set_carried(value),
            GameSlot::Ender(index) => {
                let container = match &mut self.menus.menu {
                    Some(OpenMenu {
                        backing: MenuBacking::EnderChest(_),
                        container,
                        ..
                    }) => container,
                    _ => &mut self.menus.ender_items,
                };
                if let Some(entry) = container.get_mut(index) {
                    *entry = value;
                }
            }
        }
    }

    /// `/give`: stacks up to the item's size, dropped at the player when
    /// the inventory is full.
    pub async fn give_items(&mut self, stack: ItemStack, count: i32) {
        let max = i32::from(stack.max_stack_size().max(1));
        let mut remaining = count;
        let mut added_any = false;
        while remaining > 0 {
            let size = remaining.min(max);
            remaining -= size;
            let part = stack.clone().with_count(size as u8);
            match self.inventory.add(part) {
                Some(leftover) => {
                    let eye = self.eye();
                    let position = [eye[0], eye[1] - 0.3, eye[2]];
                    self.spawn_item(leftover, position, [0.0; 3], 0).await;
                }
                None => added_any = true,
            }
        }
        if added_any {
            let eye = self.eye();
            let region = ChunkPos::of_position(eye).region();
            if let Some(handle) = self.region(region).await {
                handle
                    .request(RegionRequest::SpawnFakeItem {
                        stack: stack.clone().with_count(1),
                        position: [eye[0], eye[1] - 0.3, eye[2]],
                    })
                    .await;
            }
            let pitch = ((self.rng.r#gen::<f32>() - self.rng.r#gen::<f32>()) * 0.7 + 1.0) * 2.0;
            let sound = packets([framed(&play::Sound {
                sound_event: SoundEvent::EntityItemPickup.id(),
                category: play::sound_category::PLAYERS,
                position: self.pose.position,
                volume: 0.2,
                pitch,
                seed: self.rng.r#gen(),
            })]);
            self.broadcast_chunk(sound).await;
        }
        self.send_inventory();
    }

    /// `/clear`: removes (or with `max_count` 0 only counts) matching
    /// items across the inventory, crafting grid and cursor.
    pub fn clear_items(
        &mut self,
        predicate: Option<&crate::command::item::ItemPredicate>,
        max_count: i32,
    ) -> i32 {
        let counting = max_count == 0;
        let mut budget = if max_count < 0 { i32::MAX } else { max_count };
        let mut removed = 0;
        let matches = |stack: &ItemStack| predicate.is_none_or(|predicate| predicate.test(stack));
        let slots = self.inventory.slots().len();
        for slot in 0..slots {
            let Some(stack) = self.inventory.slots()[slot].clone() else {
                continue;
            };
            if !matches(&stack) {
                continue;
            }
            let count = i32::from(stack.count);
            if counting {
                removed += count;
                continue;
            }
            let take = count.min(budget);
            budget -= take;
            removed += take;
            let left = count - take;
            self.inventory.set_slot(
                slot,
                (left > 0).then(|| stack.clone().with_count(left as u8)),
            );
            if budget == 0 {
                break;
            }
        }
        if let Some(carried) = self.inventory.carried().cloned()
            && matches(&carried)
            && budget > 0
        {
            let count = i32::from(carried.count);
            if counting {
                removed += count;
            } else {
                let take = count.min(budget);
                removed += take;
                let left = count - take;
                self.inventory
                    .set_carried((left > 0).then(|| carried.with_count(left as u8)));
            }
        }
        if !counting {
            self.refresh_inventory_result();
            self.send_inventory();
        }
        removed
    }

    /// `/enchant`: puts an enchantment on the main-hand item if the item
    /// takes it and nothing on it conflicts.
    pub fn enchant_hand(&mut self, enchantment: &str, level: i32) -> DirectiveReply {
        let Some(stack) = self.inventory.hand(false).cloned() else {
            return self.reply(
                0,
                false,
                Some(TextComponent::translate(
                    "commands.enchant.failed.itemless",
                    [TextComponent::new(self.profile.name.clone())],
                )),
            );
        };
        let updated = match crate::world::messages::enchant_stack(&stack, enchantment, level) {
            Ok(updated) => updated,
            Err(crate::world::messages::EnchantAnswer::Incompatible(item)) => {
                return self.reply(
                    0,
                    false,
                    Some(TextComponent::translate(
                        "commands.enchant.failed.incompatible",
                        [TextComponent::new(item)],
                    )),
                );
            }
            Err(_) => return self.reply(0, false, None),
        };
        let selected = self.inventory.selected();
        let slot = Inventory::hotbar_slot(selected);
        self.inventory.set_slot(slot, Some(updated));
        self.send_inventory();
        self.reply(1, true, None)
    }

    /// `/tp`: moves the player, changing dimension through the respawn flow
    /// when needed.
    pub async fn teleport_by_command(&mut self, teleport: Teleport) {
        let mut position = teleport.position;
        for axis in 0..3 {
            if !position[axis].is_finite() {
                position[axis] = self.pose.position[axis];
            }
        }
        let [mut yaw, mut pitch] = match teleport.rotation {
            Some([yaw, pitch]) => [
                if teleport.relative_rotation[0] {
                    yaw + self.pose.yaw
                } else {
                    yaw
                },
                if teleport.relative_rotation[1] {
                    pitch + self.pose.pitch
                } else {
                    pitch
                },
            ],
            None => [self.pose.yaw, self.pose.pitch],
        };
        if let Some(look) = teleport.look_at {
            let mut from = position;
            if look.from == crate::command::args::Anchor::Eyes {
                from[1] += EYE_HEIGHT;
            }
            [yaw, pitch] = crate::command::coords::look_angles(from, look.point);
        }
        yaw = crate::command::coords::wrap_degrees(yaw);
        pitch = crate::command::coords::wrap_degrees(pitch);
        let key = self.shared.dimension(teleport.dimension).key.clone();
        if !self.teleport_decided_hooks(&key, position) {
            return;
        }
        self.body.fall = Fall::default();
        self.surroundings.in_lava = false;
        self.surroundings.contact = block_contact::Contact::default();
        self.surroundings.enchantment_modifiers.clear();
        self.surroundings.in_fire = None;
        self.teleported_hooks();
        self.pose.sprinting = false;
        if teleport.dimension != self.dimension {
            self.relocate(
                teleport.dimension,
                position,
                yaw,
                pitch,
                play::respawn::KEEP_ALL_DATA,
            )
            .await;
            return;
        }
        self.await_chunk_loaded(self.dimension, position).await;
        self.pose.yaw = yaw;
        self.pose.pitch = pitch;
        self.pose.on_ground = true;
        self.teleport(position);
        self.update_view().await;
        self.publish(true).await;
    }
}
