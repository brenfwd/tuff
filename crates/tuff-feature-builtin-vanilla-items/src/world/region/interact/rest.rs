use super::*;

/// Beds, respawn anchors, doors and note blocks.
pub trait Rest: RegionWorld {
    /// A bed click: the bed's rule may destroy it (an explosion in the
    /// nether), else the player may set its spawn and sleep, which the
    /// session decides with what the region found.
    fn use_bed(&mut self, place: &Place, state: BlockState) -> Option<ActionOutcome> {
        let interacted = Some(ActionOutcome::Interacted {
            off_hand: place.off_hand,
        });
        let (part, facing) = (state.part()?, state.facing()?);
        let toward = if part == Part::Foot {
            facing
        } else {
            facing.opposite()
        };
        let other = self
            .block(placement::step(place.target, toward))
            .unwrap_or(AIR);
        if other.block() != state.block() || other.part() == Some(part) {
            return interacted;
        }
        let (pos, state) = if part == Part::Head {
            (place.target, state)
        } else {
            (placement::step(place.target, facing), other)
        };
        let bed = state.block().bed()?;
        let environment = self.dimension_info().environment();
        let rule = match bed.rule {
            BedRuleAttribute::Bed => environment.bed_rule,
            BedRuleAttribute::StrawBed => environment.straw_bed_rule,
        };
        if rule.destroy_on_use {
            if let Some(message) = rule.error_message.and_then(TextComponent::from_json_str) {
                place.player.send(PlayerEvent::Overlay(message));
            }
            if bed.explodes {
                self.put_block(pos, AIR);
                let foot = placement::step(pos, facing.opposite());
                if self.block(foot).is_some_and(|s| s.block() == state.block()) {
                    self.put_block(foot, AIR);
                }
                let blocks = if self.shared().rule_bool("block_explosion_drop_decay") {
                    BlockInteraction::DestroyWithDecay
                } else {
                    BlockInteraction::Destroy
                };
                self.explode(Explosion {
                    center: pos.center(),
                    radius: BED_EXPLOSION_POWER,
                    fire: true,
                    blocks,
                    damage_type: &tuff_vanilla_data::damage_type::BAD_RESPAWN_POINT,
                    igniter: None,
                    culprit: None,
                    dangerous_wither_skull: false,
                    spares_nether_portals: false,
                });
            } else {
                self.play_sound(SoundEvent::BlockStrawBedBreakLeave, pos, 1.0, 1.0, None);
                self.put_block(pos, AIR);
            }
            return interacted;
        }
        if state.occupied() == Some(true) {
            let foot = placement::step(pos, facing.opposite());
            if self.wake_bed_occupant(pos, foot) {
                return interacted;
            }
            place
                .player
                .send(PlayerEvent::Overlay(TextComponent::translate(
                    "block.minecraft.bed.occupied",
                    [],
                )));
            return interacted;
        }
        let reachable = |at: BlockPos| {
            let [x, y, z] = place.position;
            (x - (at.x as f64 + 0.5)).abs() <= 3.0
                && (y - at.y as f64).abs() <= 2.0
                && (z - (at.z as f64 + 0.5)).abs() <= 3.0
        };
        let problem = |key: &str| {
            place
                .player
                .send(PlayerEvent::Overlay(TextComponent::translate(key, [])));
            Some(ActionOutcome::Interacted {
                off_hand: place.off_hand,
            })
        };
        if !reachable(pos) && !reachable(placement::step(pos, facing.opposite())) {
            return problem("block.minecraft.bed.too_far_away");
        }
        let above = placement::step(pos, Facing::Up);
        let free = |at: BlockPos| !self.block(at).unwrap_or(AIR).is_suffocating();
        if !free(above) || !free(placement::step(above, facing.opposite())) {
            return problem("block.minecraft.bed.obstructed");
        }
        let dimension = self.dimension_info();
        let set_spawn =
            tuff_core::world::bed_condition(rule.can_set_spawn, self.shared(), dimension);
        let sleep = if tuff_core::world::bed_condition(rule.can_sleep, self.shared(), dimension) {
            let level = |p: BlockPos| self.block(p).unwrap_or(AIR);
            match interaction::sleep_height(pos, state, &level) {
                Some(height) => Ok(pos.y as f64 + height + 0.125),
                None => Err(None),
            }
        } else {
            Err(rule.error_message.and_then(TextComponent::from_json_str))
        };
        let bed = BedUse {
            dimension: self.dimension_id(),
            pos,
            rule: bed.rule,
            set_spawn,
            sleep,
            monsters_near: self.monsters_prevent_rest(pos),
        };
        let used = bed.clone();
        place.player.send(PlayerEvent::Run(
            SessionTask::new(move |session| Box::pin(session.use_bed(bed))).with_data(used),
        ));
        interacted
    }

    /// Whether a monster within 8 blocks sideways and 5 vertically of the bed
    /// keeps a player from sleeping. Angry neutral monsters count, calm ones
    /// do not, and monster-category mobs that are not monsters are skipped.
    fn monsters_prevent_rest(&self, pos: BlockPos) -> bool {
        use tuff_vanilla_data::entity::MobCategory;
        use tuff_vanilla_data::registry::EntityType;
        let [cx, cy, cz] = [pos.x as f64 + 0.5, pos.y as f64, pos.z as f64 + 0.5];
        self.entity_ids().into_iter().any(|id| {
            let Some(tracked) = self.entity(id) else {
                return false;
            };
            let base = tracked.entity.base();
            let [x, y, z] = base.position;
            if (x - cx).abs() > 8.0 || (y - cy).abs() > 5.0 || (z - cz).abs() > 8.0 {
                return false;
            }
            let kind = base.kind;
            if kind.category() != MobCategory::Monster
                || matches!(
                    kind,
                    EntityType::Ghast
                        | EntityType::Slime
                        | EntityType::MagmaCube
                        | EntityType::Phantom
                        | EntityType::Shulker
                        | EntityType::Hoglin
                        | EntityType::EnderDragon
                )
            {
                return false;
            }
            let living = tracked.entity.living();
            if living.as_ref().is_some_and(|l| !l.alive) {
                return false;
            }
            kind != EntityType::ZombifiedPiglin || living.is_some_and(|l| l.target.is_some())
        })
    }

    /// A respawn anchor click: glowstone in the clicking hand charges it
    /// (a main-hand click leaves it to glowstone in the off hand), else a
    /// charged anchor sets the spawn point where its dimension lets
    /// anchors work and explodes everywhere else.
    fn use_respawn_anchor(&mut self, place: &Place, state: BlockState) -> Option<ActionOutcome> {
        let pos = place.target;
        let charges = state.charges()?;
        let chargeable = charges < ANCHOR_MAX_CHARGES;
        if chargeable && place.held == Some(Item::Glowstone) {
            self.charge_anchor(pos, state);
            return Some(ActionOutcome::Used {
                off_hand: place.off_hand,
                item: Item::Glowstone,
                effect: HandEffect::Consume(1),
            });
        }
        if chargeable && !place.off_hand && place.off_hand_item == Some(Item::Glowstone) {
            return None;
        }
        if charges == 0 {
            return None;
        }
        let interacted = Some(ActionOutcome::Interacted {
            off_hand: place.off_hand,
        });
        if self.dimension_info().environment().respawn_anchor_works {
            let anchor = AnchorUse {
                dimension: self.dimension_id(),
                pos,
            };
            let used = anchor.clone();
            place.player.send(PlayerEvent::Run(
                SessionTask::new(move |session| Box::pin(session.use_anchor(anchor)))
                    .with_data(used),
            ));
            return interacted;
        }
        self.explode_anchor(pos);
        interacted
    }

    /// Adds one charge to the respawn anchor `state` at `pos`.
    fn charge_anchor(&mut self, pos: BlockPos, state: BlockState) -> bool {
        let Some(next) = state.charges().and_then(|c| state.with_charges(c + 1)) else {
            return false;
        };
        self.put_block(pos, next);
        self.play_sound(SoundEvent::BlockRespawnAnchorCharge, pos, 1.0, 1.0, None);
        true
    }

    /// Removes the respawn anchor at `pos` in an explosion that water
    /// beside it (which would flow into the hole) or above it damps at the
    /// anchor's own block.
    fn explode_anchor(&mut self, pos: BlockPos) {
        self.put_block(pos, AIR);
        let water = |p: BlockPos| {
            let fluid = self.block(p).unwrap_or(AIR).fluid_state();
            (!fluid.is_empty()
                && tuff_datapack::tags::is(
                    tuff_vanilla_data::tag::fluid::key::WATER,
                    fluid.fluid(),
                ))
            .then_some(fluid)
        };
        let flows_in = |p: BlockPos| {
            water(p).is_some_and(|fluid| {
                fluid.is_source()
                    || (fluid.amount() >= 2 && water(placement::step(p, Facing::Down)).is_none())
            })
        };
        let in_water = Facing::HORIZONTAL
            .iter()
            .any(|&side| flows_in(placement::step(pos, side)))
            || water(placement::step(pos, Facing::Up)).is_some();
        let blocks = if self.shared().rule_bool("block_explosion_drop_decay") {
            BlockInteraction::DestroyWithDecay
        } else {
            BlockInteraction::Destroy
        };
        let explosion = Explosion {
            center: pos.center(),
            radius: ANCHOR_EXPLOSION_POWER,
            fire: true,
            blocks,
            damage_type: &tuff_vanilla_data::damage_type::BAD_RESPAWN_POINT,
            igniter: None,
            culprit: None,
            dangerous_wither_skull: false,
            spares_nether_portals: false,
        };
        if in_water {
            self.explode_shielded(explosion, pos);
        } else {
            self.explode(explosion);
        }
    }

    /// A mob opens or shuts the door at `pos` on its way.
    fn use_door(&mut self, pos: BlockPos, open: bool) {
        let Some(state) = self.block(pos) else {
            return;
        };
        let Some(toggle) = state.block().toggle() else {
            return;
        };
        if state.open() == Some(open) {
            return;
        }
        let Some(next) = state.with_open(open) else {
            return;
        };
        self.put_block(pos, next);
        let sound = if open {
            toggle.open_sound
        } else {
            toggle.close_sound
        };
        let pitch = self.rng().gen_range(0.9..1.0);
        self.play_sound(sound, pos, 1.0, pitch, None);
    }

    fn occupy_bed(&mut self, pos: BlockPos, occupied: bool) {
        if let Some(state) = self.block(pos)
            && state.block().bed().is_some()
            && let Some(next) = state.with_occupied(occupied)
        {
            self.put_block(pos, next);
        }
    }

    /// A sleeper gets up: the bed is freed (a straw bed whose rule says so
    /// goes away) and the player stands up beside it.
    fn leave_bed(&mut self, player: PlayerHandle, pos: BlockPos, yaw: f32) {
        let state = self.block(pos).unwrap_or(AIR);
        let position = match (state.block().bed(), state.facing()) {
            (Some(bed), Some(facing)) => {
                self.occupy_bed(pos, false);
                let environment = self.dimension_info().environment();
                let rule = match bed.rule {
                    BedRuleAttribute::Bed => environment.bed_rule,
                    BedRuleAttribute::StrawBed => environment.straw_bed_rule,
                };
                if rule.destroy_on_leave {
                    self.play_sound(SoundEvent::BlockStrawBedBreakLeave, pos, 1.0, 1.0, None);
                    self.put_block(pos, AIR);
                }
                let level = |p: BlockPos| self.block(p).unwrap_or(AIR);
                interaction::stand_up_position(pos, facing, yaw, &level).unwrap_or([
                    pos.x as f64 + 0.5,
                    pos.y as f64 + 1.1,
                    pos.z as f64 + 0.5,
                ])
            }
            _ => [pos.x as f64 + 0.5, pos.y as f64 + 1.1, pos.z as f64 + 0.5],
        };
        let dimension = self.dimension_id();
        let yaw = interaction::yaw_toward(position, pos);
        player.send(PlayerEvent::Run(SessionTask::new(move |session| {
            Box::pin(session.left_bed(dimension, position, yaw))
        })));
    }

    /// Where a player respawns at `pos`: beside a bed whose rule lets it
    /// set spawns, around a charged respawn anchor where anchors work (a
    /// death respawn, `consume`, spends a charge unless the point is
    /// forced), or (for a forced spawn point) in the open at `pos`.
    fn respawn_point(
        &mut self,
        pos: BlockPos,
        yaw: f32,
        pitch: f32,
        forced: bool,
        consume: bool,
    ) -> Option<RespawnSpot> {
        let state = self.block(pos).unwrap_or(AIR);
        let level = |p: BlockPos| self.block(p).unwrap_or(AIR);
        let facing_block = |position: [f64; 3]| RespawnSpot {
            position,
            yaw: interaction::yaw_toward(position, pos),
            pitch: 0.0,
            anchor: false,
        };
        if state.block() == Block::RespawnAnchor
            && let Some(charges) = state.charges()
            && (forced || charges > 0)
            && self.dimension_info().environment().respawn_anchor_works
        {
            let spot =
                interaction::anchor_stand_up_position(pos, &level).map(|position| RespawnSpot {
                    anchor: true,
                    ..facing_block(position)
                });
            if !forced
                && consume
                && spot.is_some()
                && let Some(next) = state.with_charges(charges - 1)
            {
                self.put_block(pos, next);
            }
            return spot;
        }
        if let (Some(bed), Some(facing)) = (state.block().bed(), state.facing()) {
            let environment = self.dimension_info().environment();
            let rule = match bed.rule {
                BedRuleAttribute::Bed => environment.bed_rule,
                BedRuleAttribute::StrawBed => environment.straw_bed_rule,
            };
            if tuff_core::world::bed_condition(
                rule.can_set_spawn,
                self.shared(),
                self.dimension_info(),
            ) {
                return interaction::stand_up_position(pos, facing, yaw, &level).map(facing_block);
            }
        }
        if !forced {
            return None;
        }
        let above = placement::step(pos, Facing::Up);
        (state.is_possible_to_respawn_in() && level(above).is_possible_to_respawn_in()).then_some(
            RespawnSpot {
                position: [pos.x as f64 + 0.5, pos.y as f64 + 0.1, pos.z as f64 + 0.5],
                yaw,
                pitch,
                anchor: false,
            },
        )
    }

    /// A note block sounds its note, if nothing above muffles it.
    fn play_note(&mut self, pos: BlockPos) -> bool {
        let Some(state) = self.block(pos) else {
            return false;
        };
        let above = self.block(placement::step(pos, Facing::Up)).unwrap_or(AIR);
        let Some(instrument) = state.instrument() else {
            return false;
        };
        if !interaction::plays_note(state, above) {
            return false;
        }
        let pitch = if instrument.is_tunable() {
            interaction::note_pitch(state.note().unwrap_or(0))
        } else {
            1.0
        };
        let (position, seed) = (pos.center(), self.rng().r#gen());
        let sound = if instrument.has_custom_sound() {
            let Some(sound) = self.head_note_sound(placement::step(pos, Facing::Up)) else {
                return false;
            };
            framed(&play::NamedSound {
                sound: &sound,
                category: play::sound_category::RECORDS,
                position,
                volume: 3.0,
                pitch,
                seed,
            })
        } else {
            framed(&play::Sound {
                sound_event: instrument.sound().id(),
                category: play::sound_category::RECORDS,
                position,
                volume: 3.0,
                pitch,
                seed,
            })
        };
        self.block_event(pos, play::block_event::NOTE, 0);
        self.send_to_chunk(ChunkPos::of_block(pos), None, packets([sound]));
        true
    }

    /// The sound a head at `pos` gives the note block below it (a player
    /// head's `note_block_sound`), if any.
    fn head_note_sound(&self, pos: BlockPos) -> Option<String> {
        let nbt = self.raw_block_entity(pos)?;
        (block_entity::raw::kind(nbt)? == BlockEntityType::Skull)
            .then(|| nbt.get_str("note_block_sound").map(str::to_owned))?
    }
}

impl<T: RegionWorld + ?Sized> Rest for T {}
