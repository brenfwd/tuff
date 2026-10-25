use super::*;

const SAVE_BARRIER_HOLD: Duration = Duration::from_secs(3);

impl PlayerSession {
    pub fn defaults(&self) -> PlayerState {
        let spawn = self.shared.spawn();
        PlayerState {
            dimension: self
                .shared
                .dimension(self.shared.spawn_dimension())
                .key
                .clone(),
            position: [
                spawn.pos.x as f64 + 0.5,
                spawn.pos.y as f64,
                spawn.pos.z as f64 + 0.5,
            ],
            yaw: spawn.yaw,
            pitch: spawn.pitch,
            on_ground: true,
            fall_distance: 0.0,
            fire: 0,
            air: MAX_AIR_SUPPLY as i16,
            ticks_frozen: 0,
            score: 0,
            portal_cooldown: 0,
            seen_credits: false,
            warden_tracker: Default::default(),
            health: player::MAX_HEALTH,
            food: FoodSave {
                level: player::MAX_FOOD,
                saturation: Food::default().saturation,
                exhaustion: 0.0,
                timer: 0,
            },
            selected: 0,
            slots: Vec::new(),
            ender_items: Vec::new(),
            game_mode: self.shared.live.default_game_mode(),
            previous_game_mode: None,
            flying: false,
            recipe_book: RecipeBookSave::default(),
            experience: ExperienceSave::default(),
            respawn: None,
            last_death: None,
            entered_nether: None,
            post_effects: Vec::new(),
            shoulders: [None, None],
            effects: ActiveEffects::default(),
            attributes: Default::default(),
        }
    }

    pub async fn load(&mut self) -> std::io::Result<()> {
        let backend = self.shared.backend.clone();
        let uuid = self.profile.id;
        let saved = self.shared.player_saved.clone();
        let fake = self.fake.clone();
        let (raw, stats, extra) = if fake.is_some() {
            (None, None, Vec::new())
        } else {
            tokio::task::spawn_blocking(move || {
                let docs = backend.documents();
                let stats = docs
                    .read(tuff_storage::document::Document::Stats(uuid))
                    .unwrap_or_else(|err| {
                        log::error!("failed to load statistics: {err}");
                        None
                    });
                let extra: Vec<_> = saved
                    .iter()
                    .map(|owner| {
                        let document = owner.document(uuid);
                        let root = docs.read(document).unwrap_or_else(|err| {
                            log::error!("failed to load {document:?}: {err}");
                            None
                        });
                        (owner.clone(), root)
                    })
                    .collect();
                player_data::load(docs, uuid).map(|raw| (raw, stats, extra))
            })
            .await
            .map_err(std::io::Error::other)??
        };
        let mut stats = stats
            .map(|root| crate::world::stats::Stats::from_nbt(&root))
            .unwrap_or_default();
        stats.mark_all_dirty();
        *self.progress.stats = stats;
        for (owner, root) in extra {
            owner.load(self, root);
        }
        let defaults = self.defaults();
        let new_player = raw.is_none();
        let (record, mut state) = PlayerRecord::decode(raw.unwrap_or_default(), &defaults);
        if new_player {
            state.position = self.world_spawn_position().await;
        }
        if let Some(spec) = &fake {
            if let Some(key) = &spec.dimension {
                state.dimension = key.clone();
            }
            if let Some(position) = spec.position {
                state.position = position;
            }
            if let Some((yaw, pitch)) = spec.rotation {
                state.yaw = yaw;
                state.pitch = pitch;
            }
            if let Some(mode) = spec.game_mode {
                state.game_mode = mode;
            }
        }
        match self.shared.dimensions.by_key(&state.dimension) {
            Some(dimension) => self.dimension = dimension.id,
            None => {
                self.dimension = self.shared.spawn_dimension();
                state.position = defaults.position;
            }
        }
        self.record = record;
        self.pose.position = state.position;
        self.pose.yaw = state.yaw;
        self.pose.pitch = state.pitch;
        self.pose.on_ground = state.on_ground;
        self.body.fall = Fall::new(state.fall_distance);
        self.body.fire_ticks = state.fire.into();
        self.body.air = state.air.into();
        self.body.ticks_frozen = state.ticks_frozen;
        self.body.score = state.score;
        self.actions.portal_cooldown = state.portal_cooldown.max(0) as u32;
        self.progress.seen_credits = state.seen_credits;
        self.conn.handle.set_seen_credits(state.seen_credits);
        self.progress.warden_tracker = state.warden_tracker;
        self.body.health = Health::default();
        self.body.health.value = state.health.max(0.0);
        self.body.food = Food {
            level: state.food.level.clamp(0, player::MAX_FOOD),
            saturation: state.food.saturation,
            exhaustion: state.food.exhaustion,
            tick_timer: state.food.timer.max(0) as u32,
        };
        self.inventory = Inventory::default();
        for (slot, stack) in state.slots {
            self.inventory.set_slot(slot, Some(stack));
        }
        self.inventory.set_selected(state.selected);
        self.menus.ender_items = (0..ENDER_CHEST_SLOTS).map(|_| None).collect();
        for (slot, stack) in state.ender_items {
            self.menus.ender_items[slot] = Some(stack);
        }
        let forced = self
            .shared
            .force_game_mode
            .load(std::sync::atomic::Ordering::Relaxed);
        let (mode, flying) = if forced {
            (self.shared.live.default_game_mode(), false)
        } else {
            (state.game_mode, state.flying)
        };
        self.mode.game_mode = mode;
        self.mode.previous_game_mode = state.previous_game_mode;
        self.mode.abilities = Abilities::new(mode, flying);
        self.progress.recipe_book = RecipeBook::load(&state.recipe_book, &self.shared.recipes());
        let xp = state.experience;
        let seed = if xp.seed == 0 {
            self.rng.r#gen()
        } else {
            xp.seed
        };
        self.body.experience = Experience::new(xp.level, xp.progress, xp.total, seed);
        self.mode.respawn = state.respawn;
        self.mode.last_death = state
            .last_death
            .map(|(dimension, [x, y, z])| (dimension, BlockPos::new(x, y, z)));
        self.progress.entered_nether = state.entered_nether;
        self.mode.post_effects = state.post_effects;
        self.body.shoulders.entities = state
            .shoulders
            .map(|s| s.map(crate::world::stored_entity::StoredEntity::from_storage));
        self.body.effects = state.effects;
        self.body.attributes = state.attributes;
        self.progress.last_inventory = tuff_item::item::snapshot_slots(self.inventory.slots());
        Ok(())
    }

    pub fn state(&self) -> PlayerState<&ItemStack> {
        PlayerState {
            dimension: self.dimension().key.clone(),
            position: self.pose.position,
            yaw: self.pose.yaw,
            pitch: self.pose.pitch,
            on_ground: self.pose.on_ground,
            fall_distance: self.body.fall.distance,
            fire: self.body.fire_ticks.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
            air: self.body.air.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
            ticks_frozen: self.body.ticks_frozen,
            score: self.body.score,
            portal_cooldown: self.actions.portal_cooldown.min(i32::MAX as u32) as i32,
            seen_credits: self.progress.seen_credits,
            warden_tracker: self.progress.warden_tracker,
            health: self.body.health.value,
            food: FoodSave {
                level: self.body.food.level,
                saturation: self.body.food.saturation,
                exhaustion: self.body.food.exhaustion,
                timer: self.body.food.tick_timer as i32,
            },
            selected: self.inventory.selected(),
            slots: self
                .inventory
                .slots()
                .iter()
                .enumerate()
                .filter_map(|(slot, stack)| stack.as_ref().map(|s| (slot, s)))
                .collect(),
            ender_items: self
                .ender_items()
                .iter()
                .enumerate()
                .filter_map(|(slot, stack)| stack.as_ref().map(|s| (slot, s)))
                .collect(),
            game_mode: self.mode.game_mode,
            previous_game_mode: self.mode.previous_game_mode,
            flying: self.mode.abilities.flying,
            recipe_book: self.progress.recipe_book.save(),
            experience: ExperienceSave {
                level: self.body.experience.level,
                progress: self.body.experience.progress,
                total: self.body.experience.total,
                seed: self.body.experience.seed,
            },
            respawn: self.mode.respawn.clone(),
            last_death: self
                .mode
                .last_death
                .as_ref()
                .map(|(dimension, pos)| (dimension.clone(), [pos.x, pos.y, pos.z])),
            entered_nether: self.progress.entered_nether,
            post_effects: self.mode.post_effects.clone(),
            shoulders: self.body.shoulders.entities.each_ref().map(|s| {
                s.as_ref()
                    .map(crate::world::stored_entity::StoredEntity::snapshot)
            }),
            effects: self.body.effects.clone(),
            attributes: self.body.attributes.clone(),
        }
    }

    /// The saved form as selectors test it: no `id`, plus the held stack
    /// as `SelectedItem`.
    pub fn selector_nbt(&self) -> tuff_nbt::Compound {
        let mut root = self.record.encode(self.profile.id, &self.state());
        let selected = root.get_int("SelectedItemSlot");
        let held = root.get_list("Inventory").and_then(|items| {
            items.iter().find_map(|item| {
                let item = item.as_compound()?;
                (selected.is_some() && item.get_byte("Slot").map(i32::from) == selected).then(
                    || {
                        let mut stack = item.clone();
                        stack.remove("Slot");
                        stack
                    },
                )
            })
        });
        if let Some(held) = held {
            root.insert("SelectedItem", held);
        }
        root
    }

    pub fn save_task(&self) -> tokio::task::JoinHandle<()> {
        self.save_task_in(None)
    }

    /// Saves the player as part of the world save `ticket` belongs to (or
    /// the one open now). A player quarantined after a refused item
    /// operation keeps its last consistent save.
    pub fn save_task_in(
        &self,
        ticket: Option<tuff_storage::journal::EpochTicket>,
    ) -> tokio::task::JoinHandle<()> {
        let ticket = ticket.or_else(|| {
            self.shared
                .backend
                .save_journal()
                .map(|journal| journal.ticket_now())
        });
        let quarantined = self
            .shared
            .conservation
            .quarantined(crate::world::conservation::Subject::Player(self.id));
        if self.fake.is_some() || quarantined {
            if quarantined {
                log::warn!(
                    "not saving {}: quarantined after a refused item operation",
                    self.profile.name
                );
            }
            if let Some(ticket) = ticket {
                ticket.landed();
            }
            return tokio::spawn(async {});
        }
        let backend = self.shared.backend.clone();
        let uuid = self.profile.id;
        let root = self.record.encode(uuid, &self.state());
        let stats = self.progress.stats.to_nbt();
        let extra: Vec<_> = self
            .shared
            .player_saved
            .iter()
            .filter_map(|owner| Some((owner.document(uuid), owner.save(self)?)))
            .collect();
        let name = self.profile.name.clone();
        tokio::task::spawn_blocking(move || {
            let failed = || {
                if let Some(ticket) = &ticket {
                    ticket.fail();
                }
            };
            let write = || {
                for (document, root) in extra {
                    if let Err(err) = backend.documents().write(document, root) {
                        log::error!("failed to save {document:?} for {name}: {err}");
                        failed();
                    }
                }
                if let Err(err) = player_data::save(backend.documents(), uuid, root) {
                    log::error!("failed to save player data for {name}: {err}");
                    failed();
                }
                let document = tuff_storage::document::Document::Stats(uuid);
                if let Err(err) = backend.documents().write(document, stats) {
                    log::error!("failed to save statistics for {name}: {err}");
                    failed();
                }
            };
            match &ticket {
                Some(ticket) => ticket.scope(write),
                None => write(),
            }
            if let Some(ticket) = ticket {
                ticket.landed();
            }
        })
    }

    /// Snapshots the player for a world save, then holds still until the
    /// regions have snapshotted (or a bound passes) so no container
    /// transaction can land between the two.
    pub(crate) async fn save_at_barrier(&mut self, barrier: crate::world::messages::SaveBarrier) {
        let saving = self.save_task_in(barrier.ticket);
        let _ = barrier.snapshotted.send(());
        let _ = tokio::time::timeout(SAVE_BARRIER_HOLD, barrier.release).await;
        if let Some(written) = barrier.written {
            tokio::spawn(async move {
                let _ = saving.await;
                let _ = written.send(());
            });
        }
    }

    /// Takes a world save that waited for a transaction, once none is
    /// pending.
    pub async fn settle_save_barrier(&mut self) {
        if !self.inventory_busy()
            && let Some(barrier) = self.menus.save_barrier.take()
        {
            self.save_at_barrier(barrier).await;
        }
    }

    pub async fn save(&self) {
        let _ = self.save_task().await;
    }
}
