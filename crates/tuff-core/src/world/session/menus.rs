//! Container menus: opening, clicks, transactions with regions, crafting stations, trading and the recipe book.

use super::*;
use crate::protocol::codec::{CreativeSlot, HashedSlot};
use crate::protocol::play::{self, GameMode};
use crate::storage::player_data::ENDER_CHEST_SLOTS;
use crate::world::inventory::Inventory;
use crate::world::menu::{
    self, AnvilHook, CartographyHook, Click, FurnaceHook, GridHook, GrindstoneHook, LoomHook,
    MenuKind, MerchantHook, ResultHook, SmithingHook, StonecutterHook, anvil, cartography,
    enchantment, grindstone, loom, merchant, smithing, smithing_result, stonecutter,
};
use crate::world::messages::{
    ContainerChange, EntitySlotChange, MenuOpen, MenuSource, RegionHandle, RegionRequest,
};
use crate::world::recipe_book::Entries;
use crate::world::trading::{MerchantOffer, MerchantView, TradeRequest};
use crate::world::{distance_sq, framed, packets};
use rand::Rng;
use std::collections::VecDeque;
use tuff_item::item::BurnReason;
use tuff_item::item::ItemStack;
use tuff_item::item::enchanting::TableOffers;
use tuff_vanilla_data::Block;
use tuff_vanilla_data::registry::CustomStat;
use tuff_vanilla_data::registry::SoundEvent;
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

const ANVIL_DAMAGE_CHANCE: f32 = 0.12;

impl PlayerSession {
    pub async fn set_creative_slot(&mut self, slot: i16, stack: CreativeSlot) {
        if self.mode.game_mode != GameMode::Creative {
            return;
        }
        let stack = match stack {
            CreativeSlot::Empty => None,
            #[expect(
                clippy::disallowed_methods,
                reason = "creative inventory, pick-block or creative copy (set_creative_slot)"
            )]
            CreativeSlot::Stack(shown) => Some(ItemStack::mint_like(
                &shown,
                shown.count(),
                tuff_item::item::MintReason::Creative,
            )),
            CreativeSlot::Unsupported | CreativeSlot::Invalid => {
                self.send_inventory();
                return;
            }
        };
        let window_slot = (MIN_WINDOW_SLOT..=MAX_WINDOW_SLOT)
            .contains(&slot)
            .then_some(slot as usize);
        let stack = self.restore_private_data(window_slot, stack);
        if let Some(slot) = window_slot {
            if let Some(old) = std::mem::replace(self.inventory.slot_mut(slot), stack) {
                old.burn(BurnReason::Creative);
            }
            self.refresh_inventory_result();
        } else if let Some(stack) = stack.filter(|_| slot < 0)
            && self.menus.creative_drops < CREATIVE_DROP_LIMIT
        {
            self.menus.creative_drops += CREATIVE_DROP_COST;
            self.throw_item(stack).await;
        }
    }

    /// Update Recipes and the whole recipe book, as the game sends after
    /// Login and Respawn.
    pub fn send_recipe_book(&mut self) {
        let recipes = self.shared.recipes();
        let stonecutter = recipes.stonecutter_list();
        self.send(&play::UpdateRecipes {
            property_sets: recipes.property_sets(),
            stonecutter: &stonecutter,
        });
        self.send(&play::RecipeBookSettings(
            self.progress.recipe_book.settings,
        ));
        let entries = self.progress.recipe_book.initial_entries(&recipes);
        self.send(&play::RecipeBookAdd {
            entries: &entries,
            replace: true,
        });
    }

    /// After a data pack reload: the advancement tree is replaced (its
    /// update follows with the next flush), then Update Tags, Update
    /// Recipes and the recipe book again, as the game sends every player.
    pub fn resend_reloaded_data(&mut self) {
        let contents = self.shared.datapacks.contents();
        if contents.generation == self.progress.datapack_generation {
            return;
        }
        self.progress.datapack_generation = contents.generation;
        tuff_datapack::tags::refresh(&contents.tags);
        self.rebind_advancements(self.shared.advancements_in(&contents));
        self.send(&play::UpdateTags(contents.tags_packet.clone()));
        self.send_recipe_book();
        self.refresh_inventory_result();
    }

    pub fn send_recipe_entries(&mut self, entries: Entries<'_>) {
        if !entries.is_empty() {
            self.send(&play::RecipeBookAdd {
                entries: &entries,
                replace: false,
            });
        }
    }

    /// Recomputes what the inventory's 2x2 grid makes and shows it.
    pub fn refresh_inventory_result(&mut self) {
        let result = limited_grid_result(
            &self.shared,
            &self.progress.recipe_book,
            self.inventory.craft_grid(),
            2,
            2,
        );
        if self.inventory.slots()[0] != result {
            if let Some(old) = std::mem::replace(self.inventory.slot_mut(0), result) {
                old.burn(BurnReason::Preview);
            }
            let state_id = self.inventory.next_state_id();
            let packet = framed(&play::ContainerSetSlot {
                window_id: 0,
                state_id,
                slot: 0,
                item: self.inventory.slots()[0].as_ref().map(ItemStack::any),
            });
            self.send_raw(packet);
        } else if let Some(result) = result {
            result.burn(BurnReason::Preview);
        }
    }

    /// The window id clicks and recipe placements must name: the open
    /// menu's, or 0 for the inventory.
    pub fn current_window(&self) -> i32 {
        self.menus.menu.as_ref().map_or(0, |menu| menu.window_id)
    }

    pub fn send_menu_slot(&mut self, slot: usize) {
        let Some(menu) = self.menus.menu.as_mut() else {
            return;
        };
        let state_id = menu.next_state_id();
        let window_id = menu.window_id;
        let wire = menu.kind.wire_slot(slot);
        let packet = framed(&play::ContainerSetSlot {
            window_id,
            state_id,
            slot: wire as i16,
            item: menu.container[slot].as_ref().map(ItemStack::any),
        });
        self.send_raw(packet);
    }

    pub fn send_menu_data(&mut self, property: i16, value: i16) {
        if let Some(menu) = &self.menus.menu {
            let window_id = menu.window_id;
            self.send(&play::ContainerSetData {
                window_id,
                property,
                value,
            });
        }
    }

    /// Brings a station menu's result (and the stonecutter's recipe list)
    /// up to date with its inputs after they changed.
    pub fn refresh_station(&mut self) {
        let Some(menu) = self.menus.menu.as_mut() else {
            return;
        };
        match &mut menu.station {
            Some(Station::Crafting) => {
                let (range, width, height) = MenuKind::Crafting.grid().expect("grid");
                let result = limited_grid_result(
                    &self.shared,
                    &self.progress.recipe_book,
                    &menu.container[range],
                    width,
                    height,
                );
                if menu.container[0] != result {
                    menu::show_preview(&mut menu.container[0], result);
                    self.send_menu_slot(0);
                } else if let Some(result) = result {
                    result.burn(BurnReason::Preview);
                }
            }
            Some(Station::Stonecutter {
                input,
                options,
                selected,
            }) => {
                let item = menu.container[stonecutter::INPUT]
                    .as_ref()
                    .map(|s| s.item());
                if item != *input {
                    *input = item;
                    *options = item
                        .map(|item| self.shared.recipes().stonecutter_options(item))
                        .unwrap_or_default();
                    *selected = None;
                    let cleared = menu.container[stonecutter::RESULT].is_some();
                    menu::show_preview(&mut menu.container[stonecutter::RESULT], None);
                    self.send_menu_data(0, -1);
                    if cleared {
                        self.send_menu_slot(stonecutter::RESULT);
                    }
                }
            }
            Some(Station::Merchant { view, hint, active }) => {
                let (offer, result) =
                    menu::merchant_sell_item(&menu.container, &view.offers, *hint);
                *active = offer;
                if menu.container[merchant::RESULT] != result {
                    menu::show_preview(&mut menu.container[merchant::RESULT], result);
                    self.send_menu_slot(merchant::RESULT);
                } else if let Some(result) = result {
                    result.burn(BurnReason::Preview);
                }
            }
            Some(Station::Enchantment {
                bookcases,
                seed,
                offers,
            }) => {
                let fresh = menu.container[enchantment::ITEM]
                    .as_ref()
                    .map_or(TableOffers::NONE, |stack| {
                        tuff_item::item::enchanting::table_offers(stack, *seed, *bookcases)
                    });
                let before = std::mem::replace(offers, fresh);
                if before != fresh {
                    self.send_enchantment_data(Some(before));
                }
            }
            Some(Station::Smithing { error }) => {
                let result = smithing_result(&self.shared.recipes(), &menu.container);
                let changed = menu.container[smithing::RESULT] != result;
                let filled = [smithing::TEMPLATE, smithing::BASE, smithing::ADDITION]
                    .iter()
                    .all(|slot| menu.container[*slot].is_some());
                let has_error = filled && result.is_none();
                let error_changed = *error != has_error;
                *error = has_error;
                if changed {
                    menu::show_preview(&mut menu.container[smithing::RESULT], result);
                    self.send_menu_slot(smithing::RESULT);
                } else if let Some(result) = result {
                    result.burn(BurnReason::Preview);
                }
                if error_changed {
                    self.send_menu_data(0, has_error as i16);
                }
            }
            Some(Station::Anvil { name, outcome }) => {
                let mut fresh = self.shared.station_rules.anvil_result(
                    menu.container[anvil::INPUT].as_ref(),
                    menu.container[anvil::ADDITION].as_ref(),
                    name.as_deref(),
                    self.mode.abilities.instabuild,
                );
                let slot_changed = menu.container[anvil::RESULT] != fresh.result;
                let cost_changed = outcome.cost != fresh.cost;
                if let Some(old) =
                    std::mem::replace(&mut menu.container[anvil::RESULT], fresh.result.take())
                {
                    old.burn(BurnReason::Preview);
                }
                let cost = fresh.cost;
                *outcome = fresh;
                if slot_changed {
                    self.send_menu_slot(anvil::RESULT);
                }
                if cost_changed {
                    self.send_menu_data(0, cost as i16);
                }
            }
            Some(Station::Beacon { .. }) => {}
            Some(Station::Grindstone) => {
                let result = self.shared.station_rules.grindstone_result(
                    menu.container[grindstone::INPUT].as_ref(),
                    menu.container[grindstone::ADDITIONAL].as_ref(),
                );
                if menu.container[grindstone::RESULT] != result {
                    menu::show_preview(&mut menu.container[grindstone::RESULT], result);
                    self.send_menu_slot(grindstone::RESULT);
                } else if let Some(result) = result {
                    result.burn(BurnReason::Preview);
                }
            }
            Some(Station::Loom { patterns, selected }) => {
                let fresh = self.shared.station_rules.loom_refresh(
                    patterns,
                    *selected,
                    menu.container[loom::BANNER].as_ref(),
                    menu.container[loom::DYE].as_ref(),
                    menu.container[loom::PATTERN].as_ref(),
                );
                *patterns = fresh.patterns;
                let index_changed = *selected != fresh.selected;
                *selected = fresh.selected;
                let result_changed = menu.container[loom::RESULT] != fresh.result;
                menu::show_preview(&mut menu.container[loom::RESULT], fresh.result);
                if result_changed {
                    self.send_menu_slot(loom::RESULT);
                }
                if index_changed {
                    self.send_menu_data(0, fresh.selected as i16);
                }
            }
            Some(Station::Cartography { inputs, stale }) => {
                let slots = [cartography::MAP, cartography::ADDITIONAL];
                let changed = slots.iter().zip(inputs.iter()).any(|(&slot, seen)| {
                    !tuff_item::item::same_slot(menu.container[slot].as_ref(), seen.as_deref())
                });
                if changed {
                    **inputs = slots.map(|slot| {
                        menu.container[slot]
                            .as_ref()
                            .map(tuff_item::item::ItemSnapshot::of)
                    });
                    *stale = inputs.iter().all(Option::is_some);
                    if let Some(old) = menu.container[cartography::RESULT].take() {
                        old.burn(tuff_item::item::BurnReason::Preview);
                        self.send_menu_slot(cartography::RESULT);
                    }
                }
            }
            None => {}
        }
    }

    /// Sends the enchanting table's data slots that differ from `last`
    /// (all of them when `None`).
    fn send_enchantment_data(&mut self, last: Option<TableOffers>) {
        let Some(Station::Enchantment { seed, offers, .. }) = self
            .menus
            .menu
            .as_ref()
            .and_then(|menu| menu.station.as_ref())
        else {
            return;
        };
        let (seed, now) = (*seed, *offers);
        let mut values = Vec::with_capacity(10);
        for slot in 0..3 {
            values.push((slot, now.costs[slot], last.map(|l| l.costs[slot])));
        }
        values.push((3, seed, None));
        for slot in 0..3 {
            values.push((4 + slot, now.clue[slot], last.map(|l| l.clue[slot])));
        }
        for slot in 0..3 {
            values.push((
                7 + slot,
                now.level_clue[slot],
                last.map(|l| l.level_clue[slot]),
            ));
        }
        for (property, value, previous) in values {
            if previous != Some(value) {
                self.send_menu_data(property as i16, value as i16);
            }
        }
    }

    /// Container Button Click on an enchanting table: buys one of the
    /// three offers.
    async fn enchant_with_button(&mut self, button: i32) {
        let free = self.mode.abilities.instabuild;
        let level = self.body.experience.level;
        let Some(menu) = self.menus.menu.as_mut() else {
            return;
        };
        let pos = menu.backing.positions()[0];
        let Some(Station::Enchantment { seed, offers, .. }) = &mut menu.station else {
            return;
        };
        let Some(slot) = usize::try_from(button).ok().filter(|b| *b < 3) else {
            return;
        };
        let price = slot as i32 + 1;
        let cost = offers.costs[slot];
        let lapis = menu.container[enchantment::LAPIS]
            .as_ref()
            .map_or(0, |s| s.count() as i32);
        let Some(stack) = menu.container[enchantment::ITEM].as_ref() else {
            return;
        };
        if (lapis < price && !free) || cost <= 0 || !(free || (level >= price && level >= cost)) {
            return;
        }
        let chosen = tuff_item::item::enchanting::table_enchantments(stack, *seed, slot, cost);
        if chosen.is_empty() {
            return;
        }
        let Some(stack) = menu.container[enchantment::ITEM].take() else {
            return;
        };
        let enchanted = tuff_item::item::enchanting::enchant(stack, &chosen);
        let item_for_trigger = tuff_item::item::ItemSnapshot::of(&enchanted);
        menu.container[enchantment::ITEM] = Some(enchanted);
        if !free && let Some(lapis_stack) = menu.container[enchantment::LAPIS].as_mut() {
            lapis_stack.shrink(price as u8, BurnReason::CraftingInput);
            if lapis_stack.count() == 0 {
                menu.container[enchantment::LAPIS] = None;
            }
        }
        if !free {
            self.body.experience.give_levels(-price, self.tick);
            self.send_experience();
        }
        let fresh_seed = self.rng.r#gen::<i32>();
        self.body.experience.seed = fresh_seed;
        if let Some(Station::Enchantment { seed, .. }) = self
            .menus
            .menu
            .as_mut()
            .and_then(|menu| menu.station.as_mut())
        {
            *seed = fresh_seed;
        }
        self.progress.stats.award_custom(CustomStat::EnchantItem, 1);
        self.fire(crate::world::advancement::trigger::Event::EnchantedItem {
            item: item_for_trigger.any(),
            levels: price,
        });
        self.send_menu_slot(enchantment::ITEM);
        self.send_menu_slot(enchantment::LAPIS);
        self.send_menu_data(3, fresh_seed as i16);
        self.refresh_station();
        self.enchant_sound(pos).await;
    }

    async fn enchant_sound(&mut self, pos: BlockPos) {
        let packet = framed(&play::Sound {
            sound_event: SoundEvent::BlockEnchantmentTableUse.id(),
            category: play::sound_category::BLOCKS,
            position: pos.center(),
            volume: 1.0,
            pitch: self.rng.r#gen::<f32>() * 0.1 + 0.9,
            seed: self.rng.r#gen(),
        });
        if let Some(region) = self.region_of(pos) {
            region
                .request(RegionRequest::Broadcast {
                    chunk: ChunkPos::of_block(pos),
                    entity: None,
                    packets: packets([packet]),
                    except: None,
                })
                .await;
        }
    }

    /// Rename Item: the anvil's name field.
    pub fn rename_anvil_item(&mut self, new_name: String) {
        let new_name: String = new_name
            .chars()
            .filter(|&c| c != '\u{00A7}' && !c.is_control())
            .collect();
        if !crate::world::anvil::valid_name(&new_name) {
            return;
        }
        let Some(Station::Anvil { name, .. }) = self
            .menus
            .menu
            .as_mut()
            .and_then(|menu| menu.station.as_mut())
        else {
            return;
        };
        if name.as_deref() == Some(new_name.as_str()) {
            return;
        }
        *name = Some(new_name);
        self.refresh_station();
    }

    /// Container Button Click: the stonecutter's recipe choice.
    pub async fn menu_button(&mut self, window_id: i32, button: i32) {
        if self.mode.game_mode == GameMode::Spectator {
            return;
        }
        let Some(menu) = self.menus.menu.as_mut() else {
            return;
        };
        if menu.window_id != window_id || menu.closing.is_some() {
            return;
        }
        if menu.kind == MenuKind::Lectern {
            if let MenuBacking::Blocks(parts) = &menu.backing
                && let Some((pos, _)) = parts.first()
            {
                let request = RegionRequest::LecternButton {
                    player: self.conn.handle.clone(),
                    pos: *pos,
                    button,
                    may_build: self.mode.abilities.may_build,
                };
                let region = menu.region.clone();
                region.request(request).await;
            }
            return;
        }
        if menu.kind == MenuKind::Enchantment {
            self.enchant_with_button(button).await;
            return;
        }
        if let Some(Station::Loom { patterns, selected }) = &mut menu.station {
            let Some(index) = usize::try_from(button)
                .ok()
                .filter(|index| *index < patterns.len())
            else {
                return;
            };
            *selected = button;
            let woven = match (
                menu.container[loom::BANNER].as_ref(),
                menu.container[loom::DYE].as_ref(),
            ) {
                (Some(banner), Some(dye)) => {
                    self.shared
                        .station_rules
                        .loom_weave(banner, dye, patterns[index])
                }
                _ => None,
            };
            menu::show_preview(&mut menu.container[loom::RESULT], woven);
            self.send_menu_slot(loom::RESULT);
            self.send_menu_data(0, button as i16);
            return;
        }
        let Some(Station::Stonecutter {
            options, selected, ..
        }) = &mut menu.station
        else {
            return;
        };
        let Some(index) = usize::try_from(button).ok().filter(|b| *b < options.len()) else {
            return;
        };
        if *selected == Some(index) {
            return;
        }
        *selected = Some(index);
        let cut = self
            .shared
            .recipes()
            .stonecutter_recipe(options[index])
            .map(|(_, result)| result);
        menu::show_preview(&mut menu.container[stonecutter::RESULT], cut);
        self.send_menu_slot(stonecutter::RESULT);
        self.send_menu_data(0, button as i16);
    }

    /// Set Slot State: a crafter slot switched on or off.
    pub async fn crafter_slot_state(&mut self, window_id: i32, slot: i32, enabled: bool) {
        if self.mode.game_mode == GameMode::Spectator {
            return;
        }
        let Some(menu) = self.menus.menu.as_ref() else {
            return;
        };
        if menu.window_id != window_id || menu.closing.is_some() || menu.kind != MenuKind::Crafter {
            return;
        }
        let (MenuBacking::Blocks(parts), Ok(slot)) = (&menu.backing, usize::try_from(slot)) else {
            return;
        };
        let Some((pos, _)) = parts.first() else {
            return;
        };
        let request = RegionRequest::CrafterSlotState {
            pos: *pos,
            slot,
            enabled,
        };
        menu.region.clone().request(request).await;
    }

    /// An anvil result was taken: the levels are charged, and the anvil
    /// may wear down.
    async fn anvil_taken(&mut self, pos: BlockPos, cost: i32) {
        let free = self.mode.abilities.instabuild;
        if !free {
            self.body.experience.give_levels(-cost, self.tick);
            self.send_experience();
        }
        let damage = !free && self.rng.r#gen::<f32>() < ANVIL_DAMAGE_CHANCE;
        let region = self.menus.menu.as_ref().map(|menu| menu.region.clone());
        if let Some(region) = region.or_else(|| self.region_of(pos)) {
            region
                .request(RegionRequest::AnvilUsed { pos, damage })
                .await;
        }
    }

    /// A grindstone result was taken: its experience drops and it is heard.
    async fn grindstone_taken(&mut self, pos: BlockPos, experience: i32) {
        let experience = self
            .shared
            .station_rules
            .grindstone_award(experience, &mut self.rng);
        let region = self.menus.menu.as_ref().map(|menu| menu.region.clone());
        if let Some(region) = region.or_else(|| self.region_of(pos)) {
            region
                .request(RegionRequest::GrindstoneUsed { pos, experience })
                .await;
        }
    }

    /// The sound of a station being used, heard by everyone nearby.
    pub async fn station_sound(&mut self, kind: MenuKind, pos: BlockPos) {
        let packet = match kind {
            MenuKind::Stonecutter => framed(&play::Sound {
                sound_event: SoundEvent::UiStonecutterTakeResult.id(),
                category: play::sound_category::BLOCKS,
                position: pos.center(),
                volume: 1.0,
                pitch: 1.0,
                seed: self.rng.r#gen(),
            }),
            MenuKind::Loom => framed(&play::Sound {
                sound_event: SoundEvent::UiLoomTakeResult.id(),
                category: play::sound_category::BLOCKS,
                position: pos.center(),
                volume: 1.0,
                pitch: 1.0,
                seed: self.rng.r#gen(),
            }),
            MenuKind::Cartography => framed(&play::Sound {
                sound_event: SoundEvent::UiCartographyTableTakeResult.id(),
                category: play::sound_category::BLOCKS,
                position: pos.center(),
                volume: 1.0,
                pitch: 1.0,
                seed: self.rng.r#gen(),
            }),
            MenuKind::Smithing => framed(&play::LevelEvent {
                event: SMITHING_TABLE_USED,
                position: pos,
                data: 0,
                global: false,
            }),
            _ => return,
        };
        if let Some(region) = self.region_of(pos) {
            region
                .request(RegionRequest::Broadcast {
                    chunk: ChunkPos::of_block(pos),
                    entity: None,
                    packets: packets([packet]),
                    except: None,
                })
                .await;
        }
    }

    pub fn send_inventory(&mut self) {
        let state_id = self.inventory.next_state_id();
        let packet = play::ContainerSetContent {
            window_id: 0,
            state_id,
            slots: self.inventory.slots().into(),
            carried: self.inventory.carried().map(ItemStack::any),
        };
        let bytes = framed(&packet);
        self.send_raw(bytes);
    }

    /// The container section of the open menu.
    pub fn menu_container(&self) -> Option<&[Option<ItemStack>]> {
        self.menus
            .menu
            .as_ref()
            .map(|menu| menu.container.as_slice())
    }

    pub fn menu_kind(&self) -> Option<MenuKind> {
        self.menus.menu.as_ref().map(|menu| menu.kind)
    }

    pub fn window_id(&self) -> Option<i32> {
        self.menus.menu.as_ref().map(|menu| menu.window_id)
    }

    /// The player's ender chest contents, wherever they currently live.
    pub fn ender_items(&self) -> &[Option<ItemStack>] {
        match &self.menus.menu {
            Some(OpenMenu {
                backing: MenuBacking::EnderChest(_),
                container,
                ..
            }) => container,
            _ => &self.menus.ender_items,
        }
    }

    pub async fn open_menu(&mut self, open: MenuOpen) {
        let positions = match &open.source {
            MenuSource::Blocks { parts, .. } => parts.iter().map(|(pos, _)| *pos).collect(),
            MenuSource::EnderChest { pos } | MenuSource::Station { pos, .. } => vec![*pos],
            MenuSource::Entity { .. } => Vec::new(),
        };
        let entity = match &open.source {
            MenuSource::Entity { entity, .. } => Some(*entity),
            _ => None,
        };
        let remote = match &open.source {
            MenuSource::Blocks { remote, .. } => remote.clone(),
            _ => None,
        };
        let open_source_bookcases = match &open.source {
            MenuSource::Station { bookcases, .. } => Some(*bookcases),
            _ => None,
        };
        let menu_type = open.kind.menu_type();
        let mount = match (open.kind, entity) {
            (MenuKind::Mount { columns, .. }, Some(entity)) => Some((columns, entity)),
            _ => None,
        };
        if menu_type.is_none() && mount.is_none() {
            return;
        }
        if open.dimension != self.dimension || self.is_dead() || self.disconnected {
            self.release_menu(&open.region, remote.as_ref(), positions, entity)
                .await;
            return;
        }
        if !self.allows_menu_open_hooks(open.kind) {
            self.release_menu(&open.region, remote.as_ref(), positions, entity)
                .await;
            return;
        }
        if let Some(stat) = open.stat {
            self.progress.stats.award_custom(stat, 1);
        }
        if self.menus.menu.is_some() {
            self.close_menu(true).await;
            if let Some(menu) = self.menus.menu.as_mut() {
                menu.pending = None;
            }
            self.finish_menu_close().await;
        }
        let mut data = Vec::new();
        let (container, backing) = match open.source {
            MenuSource::Blocks {
                parts,
                contents,
                data: initial,
                ..
            } => {
                data = initial;
                (mirror_of(&contents), MenuBacking::Blocks(parts))
            }
            MenuSource::EnderChest { pos } => (
                std::mem::replace(
                    &mut self.menus.ender_items,
                    (0..ENDER_CHEST_SLOTS).map(|_| None).collect(),
                ),
                MenuBacking::EnderChest(pos),
            ),
            MenuSource::Station {
                pos, data: initial, ..
            } => {
                data = initial;
                (
                    (0..open.kind.container_size()).map(|_| None).collect(),
                    MenuBacking::Station(pos),
                )
            }
            MenuSource::Entity { entity, contents } => {
                (mirror_of(&contents), MenuBacking::Entity(entity))
            }
        };
        let bookcases = match &open_source_bookcases {
            Some(bookcases) => *bookcases,
            None => 0,
        };
        let station = match open.kind {
            MenuKind::Crafting => Some(Station::Crafting),
            MenuKind::Beacon => Some(Station::Beacon {
                levels: data.first().copied().unwrap_or(0),
            }),
            MenuKind::Enchantment => Some(Station::Enchantment {
                bookcases,
                seed: self.body.experience.seed,
                offers: tuff_item::item::enchanting::TableOffers::NONE,
            }),
            MenuKind::Stonecutter => Some(Station::Stonecutter {
                input: None,
                options: Vec::new(),
                selected: None,
            }),
            MenuKind::Smithing => Some(Station::Smithing { error: false }),
            MenuKind::Anvil => Some(Station::Anvil {
                name: None,
                outcome: crate::world::anvil::AnvilOutcome {
                    result: None,
                    cost: 0,
                    repair_count: 0,
                    only_renaming: false,
                },
            }),
            MenuKind::Grindstone => Some(Station::Grindstone),
            MenuKind::Loom => Some(Station::Loom {
                patterns: &[],
                selected: 0,
            }),
            MenuKind::Cartography => Some(Station::Cartography {
                inputs: Box::new([None, None]),
                stale: false,
            }),
            _ => None,
        };
        if container.len() != open.kind.container_size() {
            self.release_menu(&open.region, remote.as_ref(), positions, entity)
                .await;
            if let MenuBacking::EnderChest(_) = backing {
                self.menus.ender_items = container;
            }
            return;
        }
        self.menus.window_counter = self.menus.window_counter % 100 + 1;
        let window_id = self.menus.window_counter;
        self.inventory.reset_drag();
        match (menu_type, mount) {
            (Some(menu_type), _) => self.send(&play::OpenScreen {
                window_id,
                menu_type: menu_type.id(),
                title: &open.title,
            }),
            (None, Some((columns, entity_id))) => self.send(&play::MountScreenOpen {
                window_id,
                columns: columns.into(),
                entity_id,
            }),
            (None, None) => unreachable!("checked above"),
        }
        let anchors = backing
            .positions()
            .into_iter()
            .map(|pos| {
                (
                    pos,
                    self.client_world().block_at(pos).map(|state| state.block()),
                )
            })
            .collect();
        self.menus.menu = Some(OpenMenu {
            window_id,
            kind: open.kind,
            title: open.title.clone(),
            state_id: 0,
            container,
            drag: None,
            region: open.region,
            backing,
            station,
            pending: None,
            remote: remote.clone(),
            split: None,
            queued: VecDeque::new(),
            anchors,
            closing: None,
        });
        self.send_menu_content();
        for (property, value) in data.into_iter().enumerate() {
            self.send_menu_data(property as i16, value as i16);
        }
        if open.kind == MenuKind::Enchantment {
            self.send_enchantment_data(None);
        }
        self.menu_opened_hooks(open.kind);
        if let Some((pos, region)) = remote {
            region
                .request(RegionRequest::RefreshContainer {
                    player: self.conn.handle.clone(),
                    pos,
                })
                .await;
        }
    }

    /// Set Container Content for the open menu: its container section,
    /// the player's main inventory and hotbar, and the cursor.
    pub fn send_menu_content(&mut self) {
        let Some(menu) = self.menus.menu.as_mut() else {
            return;
        };
        let state_id = menu.next_state_id();
        let window_id = menu.window_id;
        let mut slots: Vec<Option<tuff_item::item::AnyItem<'_>>> = menu
            .container
            .iter()
            .map(|slot| slot.as_ref().map(ItemStack::any))
            .collect();
        for slot in 0..menu.kind.player_section() {
            let index = menu
                .kind
                .inventory_slot(menu.container.len() + slot)
                .expect("player section slot");
            slots.push(self.inventory.slots()[index].as_ref().map(ItemStack::any));
        }
        if menu.kind == MenuKind::Crafter {
            let preview = slots.remove(menu::crafter::PREVIEW);
            slots.push(preview);
        }
        let packet = play::ContainerSetContent {
            window_id,
            state_id,
            slots: slots.as_slice().into(),
            carried: self.inventory.carried().map(ItemStack::any),
        };
        let bytes = framed(&packet);
        self.send_raw(bytes);
    }

    /// Shows the open menu's window again after the client shut it.
    pub fn reopen_menu(&mut self) {
        let Some(menu) = self.menus.menu.as_ref() else {
            return;
        };
        let window_id = menu.window_id;
        let title = menu.title.clone();
        match (menu.kind, &menu.backing) {
            (MenuKind::Mount { columns, .. }, MenuBacking::Entity(entity)) => {
                self.send(&play::MountScreenOpen {
                    window_id,
                    columns: columns.into(),
                    entity_id: *entity,
                });
            }
            (kind, _) => {
                if let Some(menu_type) = kind.menu_type() {
                    self.send(&play::OpenScreen {
                        window_id,
                        menu_type: menu_type.id(),
                        title: &title,
                    });
                }
            }
        }
        self.send_menu_content();
        self.send_merchant_offers();
    }

    /// Shuts the client's window (unless it shut it itself) and, once no
    /// transaction is in flight, tears the menu down.
    pub async fn close_menu(&mut self, tell_client: bool) {
        let Some(menu) = self.menus.menu.as_mut() else {
            return;
        };
        if menu.closing.is_some() {
            return;
        }
        if tell_client {
            let window_id = menu.window_id;
            self.send(&play::ContainerClose(window_id));
        }
        let menu = self.menus.menu.as_mut().unwrap();
        menu.closing = Some(self.tick);
        menu.queued.clear();
        if menu.pending.is_none() {
            self.finish_menu_close().await;
        }
    }

    pub async fn finish_menu_close(&mut self) {
        let closing = self
            .menus
            .menu
            .as_ref()
            .is_some_and(|menu| menu.closing.is_some() && menu.pending.is_none());
        if !closing {
            return;
        }
        let mut menu = self.menus.menu.take().unwrap();
        if let Some(split) = menu.split.take() {
            self.abandon_split(split, &menu.region, menu.remote.as_ref())
                .await;
        }
        self.menu_closed_hooks(menu.kind);
        let positions = menu.backing.positions();
        let mut returned = Vec::new();
        match menu.backing {
            MenuBacking::EnderChest(_) => self.menus.ender_items = menu.container,
            MenuBacking::Station(_) | MenuBacking::Merchant(_) => {
                let result = menu.kind.result_slot();
                for (slot, stack) in menu.container.iter_mut().enumerate() {
                    if result != Some(slot) {
                        returned.extend(stack.take());
                    }
                }
            }
            MenuBacking::Blocks(_) | MenuBacking::Entity(_) => {
                for stack in menu.container.iter_mut().filter_map(Option::take) {
                    stack.burn(BurnReason::Preview);
                }
            }
        }
        if let MenuBacking::Merchant(merchant) = menu.backing {
            self.stop_trading(merchant).await;
        }
        if menu.backing.viewed() {
            let entity = match menu.backing {
                MenuBacking::Entity(entity) => Some(entity),
                _ => None,
            };
            self.release_menu(&menu.region, menu.remote.as_ref(), positions, entity)
                .await;
        }
        returned.extend(self.inventory.carried_mut().take());
        for stack in returned {
            let leftover = if self.disconnected {
                Some(stack)
            } else {
                self.inventory.place_back(stack)
            };
            if let Some(stack) = leftover {
                self.throw_item(stack).await;
            }
        }
        self.send_inventory();
    }

    /// Closes the menu when the player is too far from its blocks, as the
    /// game's `stillValid` does each tick.
    pub async fn tick_menu(&mut self) {
        let Some(menu) = &self.menus.menu else {
            return;
        };
        if let Some(since) = menu.closing {
            if self.tick.saturating_sub(since) > MENU_CLOSE_TIMEOUT_TICKS {
                self.menus.menu.as_mut().unwrap().pending = None;
                self.finish_menu_close().await;
            }
            return;
        }
        let learned: Vec<(usize, Block)> = menu
            .anchors
            .iter()
            .enumerate()
            .filter(|(_, (_, opened))| opened.is_none())
            .filter_map(|(i, (pos, _))| Some((i, self.client_world().block_at(*pos)?.block())))
            .collect();
        if !learned.is_empty() {
            let menu = self.menus.menu.as_mut().unwrap();
            for (i, block) in learned {
                menu.anchors[i].1 = Some(block);
            }
        }
        let menu = self.menus.menu.as_ref().unwrap();
        let range = BLOCK_INTERACTION_RANGE + MENU_RANGE_BUFFER;
        let eye = self.eye();
        let anchored = menu.anchors.iter().all(|(pos, opened)| {
            let Some(opened) = opened else { return true };
            self.client_world()
                .block_at(*pos)
                .is_none_or(|state| same_menu_block(*opened, state.block()))
        });
        let valid = anchored
            && match menu.backing {
                MenuBacking::Merchant(merchant) | MenuBacking::Entity(merchant) => self
                    .view
                    .entities
                    .visible_entity(merchant)
                    .is_some_and(|(_, _, [x, y, z])| {
                        let reach = ENTITY_INTERACTION_RANGE + MENU_RANGE_BUFFER + 1.0;
                        distance_sq(eye, [x, y + 1.0, z]) < reach * reach
                    }),
                _ => menu
                    .backing
                    .positions()
                    .iter()
                    .all(|pos| block_distance_sq(*pos, eye) < range * range),
            };
        if !valid || self.is_dead() {
            self.close_menu(true).await;
        }
    }

    /// Select Bundle Item: toggles which stack a bundle in the given
    /// window slot hands out next.
    pub fn select_bundle_item(&mut self, slot: i32, index: i32) {
        let Ok(slot) = usize::try_from(slot) else {
            return;
        };
        let slot = self
            .menus
            .menu
            .as_ref()
            .map_or(slot, |m| m.kind.internal_slot(slot));
        let (kind, container_len) = match &self.menus.menu {
            Some(menu) => (menu.kind, menu.container.len()),
            None => (MenuKind::Player, 0),
        };
        let stack = if slot < container_len {
            self.menus
                .menu
                .as_mut()
                .and_then(|menu| menu.container[slot].as_mut())
        } else {
            let Some(index) = kind.inventory_slot(slot) else {
                return;
            };
            self.inventory.slot_mut(index).as_mut()
        };
        if let Some(contents) = stack.and_then(|s| s.components.bundle_contents.as_mut()) {
            contents.toggle_selected(index);
        }
    }

    pub async fn menu_click(&mut self, window_id: i32, click: QueuedClick) {
        let (dead, spectator) = (self.is_dead(), self.mode.game_mode == GameMode::Spectator);
        let Some(menu) = self.menus.menu.as_mut() else {
            return;
        };
        if menu.window_id != window_id || menu.closing.is_some() {
            return;
        }
        if dead || spectator || menu.kind == MenuKind::Lectern {
            self.send_menu_content();
            return;
        }
        let mut click = click;
        let kind = menu.kind;
        let internal = |slot: i16| {
            usize::try_from(slot).map_or(slot, |s| {
                kind.internal_slot(s).min(i16::MAX as usize) as i16
            })
        };
        click.slot = internal(click.slot);
        for (slot, _) in &mut click.changed {
            *slot = internal(*slot);
        }
        if menu.pending.is_some() {
            menu.queued.push_back(click);
            return;
        }
        self.apply_menu_click(click).await;
    }

    pub async fn apply_menu_click(&mut self, click: QueuedClick) {
        let creative = self.mode.abilities.instabuild;
        let mirrored = self.menu_mirrored();
        let Some(menu) = self.menus.menu.as_mut() else {
            return;
        };
        let resync = click.state_id != menu.state_id;
        let mut sim = mirrored.then(|| Sim::of(&self.inventory, &menu.container));
        let result_before = menu
            .kind
            .result_slot()
            .and_then(|slot| menu.container.get(slot)?.as_ref())
            .map(|stack| stack.item());
        let held_before = result_before.map(|item| Self::count_in(&self.inventory, item));
        let action = Click {
            slot: click.slot,
            button: click.button,
            mode: click.mode,
        };
        let mut station = menu.station.take();
        let mut grid_hook = GridHook::new(menu.kind, self.shared.recipes())
            .limited_to(crafting_gate(&self.shared, &self.progress.recipe_book));
        let mut stonecutter_hook = StonecutterHook::new(
            match &station {
                Some(Station::Stonecutter {
                    options, selected, ..
                }) => selected.map(|index| options[index]),
                _ => None,
            },
            self.shared.recipes(),
        );
        let mut smithing_hook = SmithingHook::new(self.shared.recipes());
        let mut furnace_hook = FurnaceHook::default();
        let instabuild = self.mode.abilities.instabuild;
        let mut anvil_hook = match &station {
            Some(Station::Anvil { outcome, .. }) => AnvilHook::new(
                outcome.cost,
                outcome.repair_count,
                outcome.only_renaming,
                instabuild || self.body.experience.level >= outcome.cost,
            ),
            _ => AnvilHook::new(0, 0, false, false),
        };
        let mut cartography_hook = CartographyHook::default();
        let mut grindstone_hook = GrindstoneHook::new(self.shared.station_rules.clone());
        let mut loom_hook = LoomHook::default();
        let which = match &station {
            Some(Station::Crafting) => 1,
            Some(Station::Stonecutter { .. }) => 2,
            Some(Station::Smithing { .. }) => 3,
            Some(Station::Merchant { .. }) => 4,
            Some(Station::Cartography { .. }) => 5,
            Some(Station::Anvil { .. }) => 6,
            Some(Station::Grindstone) => 7,
            Some(Station::Loom { .. }) => 8,
            Some(Station::Enchantment { .. } | Station::Beacon { .. }) => 0,
            None => 0,
        };
        let offers_before = match &station {
            Some(Station::Merchant { view, .. }) => Some(view.offers.clone()),
            _ => None,
        };
        let mut merchant_hook = match &mut station {
            Some(Station::Merchant { view, hint, active }) => Some(MerchantHook {
                offers: &mut view.offers,
                active: *active,
                hint: *hint,
                trades: Vec::new(),
            }),
            _ => None,
        };
        let hook: &mut dyn ResultHook = match (which, merchant_hook.as_mut()) {
            (1, _) => &mut grid_hook,
            (2, _) => &mut stonecutter_hook,
            (3, _) => &mut smithing_hook,
            (4, Some(hook)) => hook,
            (5, _) => &mut cartography_hook,
            (6, _) => &mut anvil_hook,
            (7, _) => &mut grindstone_hook,
            (8, _) => &mut loom_hook,
            _ if menu.kind.is_furnace() => &mut furnace_hook,
            _ => &mut menu::NoResult,
        };
        let (container, inventory) = match sim.as_mut() {
            Some(sim) => (&mut sim.container, &mut sim.inventory),
            None => (&mut menu.container, &mut self.inventory),
        };
        let drops = menu::click(
            menu.kind,
            container,
            inventory,
            &mut menu.drag,
            action,
            menu::ClickEnv {
                creative,
                hook,
                recipes: &self.shared.recipes(),
            },
        );
        let trades = match merchant_hook {
            Some(hook) => {
                let (active, trades) = (hook.active, hook.trades);
                if let Some(Station::Merchant { active: slot, .. }) = &mut station {
                    *slot = active;
                }
                trades
            }
            None => Vec::new(),
        };
        menu.station = station;
        let used_station = (stonecutter_hook.took
            || smithing_hook.took
            || cartography_hook.took
            || loom_hook.took)
            .then(|| (menu.kind, menu.backing.positions()[0]));
        let anvil_used = anvil_hook
            .took
            .then(|| (menu.backing.positions()[0], anvil_hook.cost));
        let grindstone_used = grindstone_hook
            .took
            .then(|| (menu.backing.positions()[0], grindstone_hook.experience));
        let took_result = furnace_hook.took;
        let traded = !trades.is_empty();
        let brewed = match &sim {
            Some(sim) if menu.kind == MenuKind::BrewingStand => {
                crate::world::session::advancements::potions_taken(&menu.container, &sim.container)
            }
            _ => Vec::new(),
        };
        let mut local_drops = Vec::new();
        match sim.as_mut() {
            Some(sim) => {
                sim.drops = drops;
                for sound in sim.inventory.take_bundle_sounds() {
                    self.inventory.note_bundle_sound(sound);
                }
            }
            None => local_drops = drops,
        }
        let crafted = (grid_hook.took
            || stonecutter_hook.took
            || smithing_hook.took
            || took_result
            || traded)
            .then_some(result_before)
            .flatten();
        if let (Some(item), Some(before)) = (crafted, held_before) {
            let (after, dropped) = match &sim {
                Some(sim) => (Self::count_in(&sim.inventory, item), &sim.drops),
                None => (Self::count_in(&self.inventory, item), &local_drops),
            };
            let dropped: i32 = dropped
                .iter()
                .filter(|stack| stack.item() == item)
                .map(|stack| i32::from(stack.count()))
                .sum();
            self.count_crafted(item, after - before + dropped);
        }
        for recipes in [
            &grid_hook.crafted,
            &stonecutter_hook.crafted,
            &smithing_hook.crafted,
        ] {
            self.crafted_recipes(recipes);
        }
        for potion in brewed {
            self.fire(crate::world::advancement::trigger::Event::BrewedPotion(
                potion,
            ));
        }

        self.refresh_station();
        self.flush_bundle_sounds().await;
        let menu = self.menus.menu.as_ref().unwrap();
        let (container, inventory) = match &sim {
            Some(sim) => (&sim.container, &sim.inventory),
            None => (&menu.container, &self.inventory),
        };
        let in_sync = click.changed.iter().all(|(slot, view)| {
            let value = usize::try_from(*slot).ok().and_then(|slot| {
                if slot < container.len() {
                    container[slot].as_ref()
                } else {
                    menu.kind
                        .inventory_slot(slot)
                        .and_then(|index| inventory.slots()[index].as_ref())
                }
            });
            HashedSlot::matches(view.as_ref(), value.map(ItemStack::view))
        }) && HashedSlot::matches(
            click.carried.as_ref(),
            inventory.carried().map(ItemStack::view),
        );
        let resend = !in_sync || resync;
        if resend && sim.is_none() {
            self.send_menu_content();
        }
        if let Some((pos, cost)) = anvil_used {
            self.anvil_taken(pos, cost).await;
        }
        if let Some((pos, experience)) = grindstone_used {
            self.grindstone_taken(pos, experience).await;
        }
        if let Some((kind, pos)) = used_station {
            self.station_sound(kind, pos).await;
        }
        let Some(sim) = sim else {
            for stack in local_drops {
                self.throw_item(stack).await;
            }
            return;
        };
        if let (true, Some(offers)) = (traded, offers_before) {
            self.submit_trade(sim, offers, trades, resend).await;
            return;
        }
        self.settle_click(sim, took_result, resend).await;
    }

    /// Asks the merchant to agree to the trades a click made; the click
    /// settles once it does. Until then the screen and inventory stay as
    /// they were, with the offers as they were kept to restore on refusal.
    pub async fn submit_trade(
        &mut self,
        sim: Sim,
        offers_before: Vec<MerchantOffer>,
        trades: Vec<(usize, u32)>,
        resend: bool,
    ) {
        let times: u32 = trades.iter().map(|(_, n)| n).sum();
        self.progress
            .stats
            .award_custom(CustomStat::TradedWithVillager, times as i32);
        let merchant = match self.menus.menu.as_ref().map(|m| &m.backing) {
            Some(MenuBacking::Merchant(merchant)) => Some(*merchant),
            _ => None,
        };
        if let Some(merchant) = merchant {
            self.traded(merchant, &offers_before, &trades);
        }
        let Some(menu) = self.menus.menu.as_mut() else {
            sim.discard();
            return;
        };
        let MenuBacking::Merchant(merchant) = menu.backing else {
            sim.discard();
            return;
        };
        menu.pending = Some(Pending {
            origins: Vec::new(),
            sim,
            reasons: None,
            resend,
            merchant: Some(offers_before),
        });
        let window_id = menu.window_id;
        let region = match self.view.entities.visible_entity(merchant) {
            Some((_, chunk, _)) => self.region(chunk.region()).await,
            None => None,
        };
        let request = TradeRequest {
            merchant,
            player: self.profile.id,
            trades,
        };
        let sent = match region {
            Some(region) => {
                region
                    .request(RegionRequest::Trade {
                        player: self.conn.handle.clone(),
                        window_id,
                        request,
                    })
                    .await
            }
            None => false,
        };
        if !sent {
            Box::pin(self.trade_answered(window_id, false)).await;
        }
    }

    /// A merchant's screen opens: an empty payment section, then its
    /// offers.
    pub async fn open_merchant(
        &mut self,
        title: tuff_item::text::TextComponent,
        view: MerchantView,
    ) {
        let merchant = view.merchant;
        let region = match self.view.entities.visible_entity(merchant) {
            Some((_, chunk, _)) => self.region(chunk.region()).await,
            None => None,
        };
        let Some(region) = region.filter(|_| !self.is_dead() && !self.disconnected) else {
            self.stop_trading(merchant).await;
            return;
        };
        if self.menus.menu.is_some() {
            self.close_menu(true).await;
            if let Some(menu) = self.menus.menu.as_mut() {
                menu.pending = None;
            }
            self.finish_menu_close().await;
        }
        self.menus.window_counter = self.menus.window_counter % 100 + 1;
        let window_id = self.menus.window_counter;
        self.inventory.reset_drag();
        let menu_type = MenuKind::Merchant.menu_type().expect("merchant screen");
        self.send(&play::OpenScreen {
            window_id,
            menu_type: menu_type.id(),
            title: &title,
        });
        self.menus.menu = Some(OpenMenu {
            window_id,
            kind: MenuKind::Merchant,
            title: title.clone(),
            state_id: 0,
            container: (0..MenuKind::Merchant.container_size())
                .map(|_| None)
                .collect(),
            drag: None,
            region,
            backing: MenuBacking::Merchant(merchant),
            station: Some(Station::Merchant {
                view,
                hint: 0,
                active: None,
            }),
            pending: None,
            remote: None,
            split: None,
            queued: VecDeque::new(),
            closing: None,
            anchors: Vec::new(),
        });
        self.send_menu_content();
        self.send_merchant_offers();
    }
}

pub(crate) mod deferred;
pub use deferred::Deferred;
mod state;
pub use state::*;
mod transactions;
pub use transactions::{Place, Reasons, Sim, mirror_of};
