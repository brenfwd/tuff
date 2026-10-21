use super::*;

impl RegionActor {
    /// Lets the clicked block react to a right click, as vanilla's block
    /// `useWithoutItem` does. `None` means the click falls through to
    /// placing the held item.
    pub(crate) fn interact_block(
        &mut self,
        place: &Place,
        state: BlockState,
    ) -> Option<ActionOutcome> {
        let pos = place.target;
        let opens = ContainerKind::of_block(state.block()).is_some_and(|k| k.menu().is_some());
        if opens || state.block() == Block::EnderChest {
            self.player_block_action(place.player.id, pos, state, BlockAction::Opened);
            self.open_container(place, state);
            return Some(ActionOutcome::Interacted {
                off_hand: place.off_hand,
            });
        }
        if self.open_command_block(place) {
            return Some(ActionOutcome::Interacted {
                off_hand: place.off_hand,
            });
        }
        if place.game_master
            && matches!(state.block(), Block::Jigsaw | Block::TestBlock)
            && self.raw_block_entity(pos).is_some()
        {
            return Some(ActionOutcome::Interacted {
                off_hand: place.off_hand,
            });
        }
        if state.block() == Block::Beacon {
            let data = crate::world::beacon::menu_data(self.raw_block_entity(pos));
            place.player.send(PlayerEvent::MenuOpen(MenuOpen {
                dimension: self.dimension,
                region: self.handle.clone(),
                kind: MenuKind::Beacon,
                title: TextComponent::translate("container.beacon", []),
                source: MenuSource::Station {
                    pos,
                    bookcases: 0,
                    data,
                },
                stat: menu_stat(state.block()),
            }));
            return Some(ActionOutcome::Interacted {
                off_hand: place.off_hand,
            });
        }
        let station = match state.block() {
            Block::CraftingTable => Some((MenuKind::Crafting, "container.crafting")),
            Block::Stonecutter => Some((MenuKind::Stonecutter, "container.stonecutter")),
            Block::SmithingTable => Some((MenuKind::Smithing, "container.upgrade")),
            Block::Anvil | Block::ChippedAnvil | Block::DamagedAnvil => {
                Some((MenuKind::Anvil, "container.repair"))
            }
            Block::CartographyTable => Some((MenuKind::Cartography, "container.cartography_table")),
            Block::Loom => Some((MenuKind::Loom, "container.loom")),
            Block::Grindstone => Some((MenuKind::Grindstone, "container.grindstone_title")),
            Block::EnchantingTable => Some((MenuKind::Enchantment, "container.enchant")),
            _ => None,
        };
        if let Some((kind, title)) = station {
            let bookcases = if kind == MenuKind::Enchantment {
                self.enchanting_bookcases(pos)
            } else {
                0
            };
            place.player.send(PlayerEvent::MenuOpen(MenuOpen {
                dimension: self.dimension,
                region: self.handle.clone(),
                kind,
                title: TextComponent::translate(title, []),
                source: MenuSource::Station {
                    pos,
                    bookcases,
                    data: Vec::new(),
                },
                stat: menu_stat(state.block()),
            }));
            return Some(ActionOutcome::Interacted {
                off_hand: place.off_hand,
            });
        }
        if state.block() == Block::Cake
            && state.get("bites") == Some("0")
            && let Some(cake) = place.held.and_then(candle_cake)
        {
            self.put_block(pos, cake.default_state());
            self.play_sound(SoundEvent::BlockCakeAddCandle, pos, 1.0, 1.0, None);
            return Some(ActionOutcome::Placed {
                off_hand: place.off_hand,
                item: place.held?,
                placement: None,
            });
        }
        match state.block().kind() {
            BlockKind::Cake if place.can_eat => {
                if state.block() != Block::Cake
                    && let Some(outcome) = self.use_block(place, state)
                {
                    return Some(outcome);
                }
                place.player.send(PlayerEvent::AwardStat(
                    tuff_vanilla_data::registry::CustomStat::EatCakeSlice,
                    1,
                ));
                let bites: i32 = state.get("bites").and_then(|b| b.parse().ok()).unwrap_or(0);
                if state.block() == Block::Cake {
                    if bites < 6 {
                        let next = state.with("bites", &(bites + 1).to_string())?;
                        self.put_block(pos, next);
                    } else {
                        self.put_block(pos, AIR);
                    }
                } else {
                    let cake = Block::Cake.default_state().with("bites", "1")?;
                    self.put_block(pos, cake);
                    let candle = self
                        .shared
                        .block_loot()
                        .drops(state, None, None, &mut self.rng);
                    self.drop_items(pos, candle);
                }
                Some(ActionOutcome::Ate {
                    nutrition: 2,
                    saturation: 0.4,
                })
            }
            _ => self.use_block(place, state),
        }
    }

    /// Uses the held item on the clicked block, as vanilla's item `useOn`
    /// does for items that are not blocks: lighting fires and portals, and
    /// filling end portal frames. `None` falls through to placement.
    pub(crate) fn use_item_on(
        &mut self,
        place: &Place,
        state: BlockState,
    ) -> Option<ActionOutcome> {
        let item = place.held?;
        let pos = place.target;
        let outcome = ActionOutcome::Placed {
            off_hand: place.off_hand,
            item,
            placement: None,
        };
        if ContainerKind::of_block(state.block()) == Some(ContainerKind::Campfire) {
            if self
                .shared
                .recipes()
                .can_cook(ContainerKind::Campfire, item)
            {
                let stack = ItemStack::new(item, 1).with_components(place.components.clone());
                if !self.place_campfire_food(pos, stack) {
                    return None;
                }
                place.player.send(PlayerEvent::AwardStat(
                    tuff_vanilla_data::registry::CustomStat::InteractWithCampfire,
                    1,
                ));
                return Some(outcome);
            }
            if tuff_datapack::tags::is(tuff_vanilla_data::tag::item::key::DOUSES_CAMPFIRES, item)
                && state.get("lit") == Some("true")
            {
                self.level_event(play::level_event::SOUND_EXTINGUISH_FIRE, pos, 0, false);
                self.put_block(pos, state.with("lit", "false")?);
                return Some(outcome);
            }
        }
        if let Some(outcome) = self.use_item_hooks(place, state) {
            return Some(outcome);
        }
        match item {
            Item::BoneMeal => {
                let behaviours = self.shared.growth_behaviors.clone();
                let face = placement::facing(place.face);
                let grown = behaviours.bonemeal(self, pos, face)?;
                self.level_event(crate::world::growth::BONEMEAL_EVENT, grown, 15, false);
                Some(outcome)
            }
            _ => None,
        }
    }

    /// Places `state` for the block item `stack` at `pos` without a
    /// player (a dispenser): refused where a player stands in the way.
    pub(crate) fn place_item_block(
        &mut self,
        pos: BlockPos,
        state: BlockState,
        stack: &ItemStack,
    ) -> bool {
        if !self.owns(pos) || !self.dimension().contains_block(pos) {
            return false;
        }
        let state = self.redstone_placement(pos, state);
        let blocked = self.ents.presence.values().any(|p| {
            let [x, y, z] = p.position;
            p.is_physical()
                && placement::obstructs(
                    state,
                    pos,
                    [
                        x - PLAYER_HALF_WIDTH,
                        y,
                        z - PLAYER_HALF_WIDTH,
                        x + PLAYER_HALF_WIDTH,
                        y + PLAYER_HEIGHT,
                        z + PLAYER_HALF_WIDTH,
                    ],
                )
        });
        if blocked || self.entities_obstruct(state, pos, None) {
            return false;
        }
        self.put_block(pos, state);
        self.apply_placed_components(pos, &stack.components, false);
        if let Some((at, companion)) = self.shared.block_rules.companions(state, pos) {
            self.put_block(at, companion);
        }
        self.redstone_placed(pos, state);
        let (_, volume, _) = state.place_sound();
        let packet = stand_in_place_sound(state, pos, self.rng.r#gen());
        self.send_sound_near(pos.center(), (volume + 1.0) / 2.0, None, packet);
        true
    }

    pub(crate) fn fire_sound(&mut self, item: Item) -> (SoundEvent, f32) {
        match item {
            Item::FlintAndSteel => (
                SoundEvent::ItemFlintandsteelUse,
                self.rng.r#gen::<f32>() * 0.4 + 0.8,
            ),
            _ => (
                SoundEvent::ItemFirechargeUse,
                (self.rng.r#gen::<f32>() - self.rng.r#gen::<f32>()) * 0.2 + 1.0,
            ),
        }
    }

    /// Puts one of `stack` on the campfire at `pos`; false if it has no
    /// campfire recipe or no free slot.
    pub(crate) fn place_campfire_food(&mut self, pos: BlockPos, stack: ItemStack) -> bool {
        let recipes = self.shared.recipes();
        if !self.ensure_block_entity(pos, ContainerKind::Campfire) {
            return false;
        }
        let placed = {
            let entity = self.block_entity_mut(pos).expect("ensured above");
            let mut items = std::mem::take(&mut entity.items);
            let campfire = entity.campfire_mut().expect("a campfire");
            let placed = recipes.place_food(&mut items, campfire, &stack);
            entity.items = items;
            placed
        };
        if placed.is_none() {
            return false;
        }
        self.send_block_entity_data(pos);
        true
    }

    /// Sends a block entity's client data (a campfire's food) to the
    /// chunk's viewers, and drops the cached chunk packet that lacks it.
    pub(crate) fn send_block_entity_data(&mut self, pos: BlockPos) {
        self.flush_batched_chunk(ChunkPos::of_block(pos));
        if let Some(column) = self.columns.get_mut(&ChunkPos::of_block(pos)) {
            column.version += 1;
            column.packet = None;
        }
        let Some((kind, nbt)) = self.client_block_entity(pos) else {
            return;
        };
        let packet = packets([framed(&play::BlockEntityData {
            position: pos,
            kind: kind.id(),
            nbt: &nbt,
        })]);
        self.send_chunk_packets(ChunkPos::of_block(pos), None, packet);
    }

    /// What clients receive of the block entity at `pos`, if anything.
    pub(crate) fn client_block_entity(&self, pos: BlockPos) -> Option<(BlockEntityType, Compound)> {
        if let Some(entity) = self.block_entity(pos) {
            return Some((entity.kind.entity_type(), entity.client_data()?));
        }
        let (_, kind, data) = block_entity::raw::client_data(self.raw_block_entity(pos)?)?;
        Some((kind, data))
    }

    /// Sends the data of block entities whose blocks changed, as the game
    /// does after the block changes themselves.
    pub(crate) fn flush_block_entity_updates(&mut self) {
        for pos in std::mem::take(&mut self.block_entity_updates) {
            if self
                .block_entity_kind(pos)
                .is_some_and(BlockEntityType::sends_updates)
            {
                self.send_block_entity_data(pos);
            }
        }
    }

    pub(crate) fn play_sound(
        &self,
        sound: SoundEvent,
        pos: BlockPos,
        volume: f32,
        pitch: f32,
        except: Option<PlayerId>,
    ) {
        let seed = rand::random();
        let packet = packets([framed(&play::Sound {
            sound_event: sound.id(),
            category: play::sound_category::BLOCKS,
            position: pos.center(),
            volume,
            pitch,
            seed,
        })]);
        self.send_sound_near(pos.center(), volume, except, packet);
    }

    pub(crate) fn anvil_used(&mut self, pos: BlockPos, damage: bool) {
        let Some(state) = self.block(pos) else {
            return;
        };
        if !tuff_datapack::tags::is(tuff_vanilla_data::tag::block::key::ANVIL, state.block()) {
            return;
        }
        if !damage {
            self.level_event(ANVIL_USED, pos, 0, false);
            return;
        }
        match self.shared.block_rules.anvil_damaged(state) {
            Some(next) => {
                self.set_block(pos, next);
                self.level_event(ANVIL_USED, pos, 0, false);
            }
            None => {
                self.destroy_block(pos, false);
                self.level_event(ANVIL_BROKEN, pos, 0, false);
            }
        }
    }

    pub(crate) fn grindstone_used(&mut self, pos: BlockPos, experience: i32) {
        if experience > 0 {
            self.spawn_experience(pos.center(), experience);
        }
        self.level_event(GRINDSTONE_USED, pos, 0, false);
    }

    pub(crate) fn level_event(&self, event: i32, pos: BlockPos, data: i32, global: bool) {
        let packet = packets([framed(&play::LevelEvent {
            event,
            position: pos,
            data,
            global,
        })]);
        if global {
            self.send_chunk_packets(ChunkPos::of_block(pos), None, packet);
        } else {
            self.send_near(pos.center(), LEVEL_EVENT_RANGE, None, packet);
        }
    }
}
