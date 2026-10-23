use super::*;

/// What viewers, the coordinator, scoreboards and the client were last
/// told about this player, so only changes go out.
pub struct Published {
    pub(super) selector_hot_until: u64,
    pub last_sent_experience: Option<(f32, i32, i32)>,
    /// What the coordinator was last told about this player.
    pub last_state: Option<crate::world::messages::PlayerState>,
    pub last_sent_status: Option<(f32, i32, bool)>,
    /// The vitals scoreboard criteria last saw (health, food, air, armor,
    /// experience, level).
    pub last_vitals: [Option<i32>; 6],
    pub seq: u64,
    pub last_broadcast: ([f64; 3], f32, f32),
    pub last_flags: (
        bool,
        bool,
        bool,
        bool,
        i8,
        f32,
        Option<BlockPos>,
        u64,
        Pose,
        bool,
    ),
    /// The air supply others last saw.
    pub last_air: i32,
    pub last_ticks_frozen: i32,
    pub last_score: i32,
    pub last_stuck_arrows: i32,
    /// The equipment as others last saw it, by [`EQUIPMENT_SLOTS`].
    pub last_equipment: [Option<tuff_item::item::ItemSnapshot>; 6],
    pub last_feet: Option<tuff_item::item::ItemSnapshot>,
    /// The synced attribute values last sent to this player, by
    /// [`attributes::synced_attributes`] order.
    pub last_synced_attributes: Vec<f64>,
    /// The explosion knockback resistance and creative flight regions
    /// last heard of.
    pub last_blast_state: (f64, bool),
    pub last_looks: crate::world::messages::PlayerLooks,
    /// The region relaying this player's updates to viewers.
    pub published_home: Option<RegionPos>,
    /// Where the player stood when it last published, the start of the
    /// stretch it swept through since.
    pub last_position: Option<[f64; 3]>,
}

impl Default for Published {
    fn default() -> Self {
        Self {
            selector_hot_until: 0,
            last_sent_experience: None,
            last_state: None,
            last_sent_status: None,
            last_vitals: [None; 6],
            seq: 0,
            last_broadcast: ([0.0; 3], 0.0, 0.0),
            last_flags: (
                false,
                false,
                false,
                false,
                0,
                0.0,
                None,
                0,
                Pose::Standing,
                false,
            ),
            last_air: MAX_AIR_SUPPLY,
            last_ticks_frozen: 0,
            last_score: 0,
            last_stuck_arrows: 0,
            last_equipment: Default::default(),
            last_feet: None,
            last_synced_attributes: Vec::new(),
            last_blast_state: (0.0, false),
            last_looks: crate::world::messages::PlayerLooks::default(),
            published_home: None,
            last_position: None,
        }
    }
}

impl PlayerSession {
    /// Entity data that changes with the player's state. Effect particles
    /// go along when they are not the default or `effects_changed`.
    pub fn flags_metadata(&self, effects_changed: bool) -> Vec<(u8, MetadataValue)> {
        let mut flags = 0;
        if self.body.fire_ticks > 0 {
            flags |= metadata::FLAG_ON_FIRE;
        }
        if self.pose.sneaking {
            flags |= metadata::FLAG_SNEAKING;
        }
        if self.pose.sprinting {
            flags |= metadata::FLAG_SPRINTING;
        }
        if self.pose.swimming {
            flags |= metadata::FLAG_SWIMMING;
        }
        if self.body.effects.has(MobEffect::Invisibility) {
            flags |= metadata::FLAG_INVISIBLE;
        }
        if self.body.effects.has(MobEffect::Glowing) {
            flags |= metadata::FLAG_GLOWING;
        }
        if self.motion.glide.active {
            flags |= metadata::FLAG_FALL_FLYING;
        }
        let (particles, ambient) = self.body.effects.particles();
        let pose = i32::from(self.current_pose().id());
        let mut values = vec![
            (metadata::ENTITY_FLAGS, MetadataValue::Byte(flags)),
            (metadata::POSE, MetadataValue::Pose(pose)),
            (
                metadata::LIVING_FLAGS,
                MetadataValue::Byte(self.living_flags()),
            ),
            (
                metadata::ABSORPTION,
                MetadataValue::Float(self.body.absorption),
            ),
            (
                metadata::SLEEPING_POS,
                MetadataValue::OptionalPosition(self.sleep.sleeping.map(|sleep| sleep.pos)),
            ),
        ];
        if effects_changed || !particles.is_empty() || ambient {
            values.push((
                metadata::EFFECT_PARTICLES,
                MetadataValue::Particles(particles),
            ));
            values.push((metadata::EFFECT_AMBIENCE, MetadataValue::Boolean(ambient)));
        }
        values
    }

    /// A fingerprint of what effects show on this player: particles,
    /// invisibility and glowing.
    pub fn effect_look(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.body.effects.particles().hash(&mut hasher);
        self.body
            .effects
            .has(MobEffect::Invisibility)
            .hash(&mut hasher);
        self.body.effects.has(MobEffect::Glowing).hash(&mut hasher);
        hasher.finish()
    }

    pub fn living_flags(&self) -> i8 {
        let spin = if self.motion.spin_attack_ticks > 0 {
            metadata::LIVING_FLAG_SPIN_ATTACK
        } else {
            0
        };
        spin | match self.actions.using {
            None => 0,
            Some(using) => {
                metadata::LIVING_FLAG_HAND_ACTIVE
                    | if using.off_hand {
                        metadata::LIVING_FLAG_OFF_HAND
                    } else {
                        0
                    }
            }
        }
    }

    /// The model options the client chose (skin layers, main arm), as
    /// entity data differing from the defaults. The client's own player
    /// starts from defaults too, so it needs these as much as viewers do.
    pub fn options_metadata(&self) -> Vec<(u8, MetadataValue)> {
        let mut values = Vec::new();
        if self.info.main_hand == 0 {
            values.push((metadata::MAIN_ARM, MetadataValue::HumanoidArm(0)));
        }
        if self.info.skin_parts != 0 {
            values.push((
                metadata::SKIN_PARTS,
                MetadataValue::Byte(self.info.skin_parts as i8),
            ));
        }
        values
    }

    pub fn send_options_metadata(&mut self) {
        let values = self.options_metadata();
        if !values.is_empty() {
            self.send(&play::SetEntityData {
                entity_id: self.id,
                values: &values,
            });
        }
    }

    pub fn spawn_packets(&self) -> Packets {
        let mut values = self.flags_metadata(false);
        if self.body.score != 0 {
            values.push((metadata::SCORE, MetadataValue::VarInt(self.body.score)));
        }
        if self.body.air != MAX_AIR_SUPPLY {
            values.push((metadata::AIR_SUPPLY, MetadataValue::VarInt(self.body.air)));
        }
        if self.body.ticks_frozen > 0 {
            values.push((
                metadata::TICKS_FROZEN,
                MetadataValue::VarInt(self.body.ticks_frozen),
            ));
        }
        values.push((
            metadata::HEALTH,
            MetadataValue::Float(self.body.health.value),
        ));
        if self.body.stuck_arrows.count > 0 {
            values.push((
                metadata::ARROW_COUNT,
                MetadataValue::VarInt(self.body.stuck_arrows.count),
            ));
        }
        values.extend(self.options_metadata());
        values.extend(self.shoulder_spawn_metadata());
        packets(
            [
                framed(&play::AddEntity {
                    entity_id: self.id,
                    uuid: self.profile.id,
                    kind: EntityType::Player.id() as i32,
                    position: self.pose.position,
                    velocity: [0.0; 3],
                    pitch: self.pose.pitch,
                    yaw: self.pose.yaw,
                    head_yaw: self.pose.yaw,
                    data: 0,
                }),
                framed(&play::SetEntityData {
                    entity_id: self.id,
                    values: &values,
                }),
                framed(&play::SetEquipment {
                    entity_id: self.id,
                    items: &EQUIPMENT_SLOTS.map(|slot| {
                        (
                            equipment_slot_id(slot),
                            self.inventory.equipped(slot).map(ItemStack::any),
                        )
                    }),
                }),
                framed(&play::RotateHead {
                    entity_id: self.id,
                    head_yaw: self.pose.yaw,
                }),
            ]
            .into_iter()
            .chain(self.ride.riding.as_ref().map(|riding| {
                framed(&play::SetPassengers {
                    vehicle: riding.vehicle,
                    passengers: &riding.riders,
                })
            })),
        )
    }

    /// What mobs see of this player.
    pub fn looks(&self) -> crate::world::messages::PlayerLooks {
        let armor = [
            EquipmentSlot::Feet,
            EquipmentSlot::Legs,
            EquipmentSlot::Chest,
            EquipmentSlot::Head,
        ]
        .map(|slot| self.inventory.equipped(slot).map(|s| s.item()));
        crate::world::messages::PlayerLooks {
            yaw: self.pose.yaw,
            pitch: self.pose.pitch,
            eye_height: self.eye()[1] - self.pose.position[1],
            height: self.height(),
            held: [
                self.inventory.hand(false).map(|s| s.item()),
                self.inventory.hand(true).map(|s| s.item()),
            ],
            creative: self.mode.game_mode == GameMode::Creative,
            sneaking: self.pose.sneaking,
            invisible: self.body.effects.has(MobEffect::Invisibility),
            invulnerable: self.mode.abilities.invulnerable,
            armor,
            spin_attack: self.motion.spin_attack_ticks > 0,
            effects: crate::world::messages::PlayerLooks::effect_bits(
                self.body.effects.iter().map(|e| e.effect),
            ),
            movement: self.sync.known_movement,
            sleeping: self.sleep.sleeping.map(|_| self.sleep.sleep_counter),
            warden_warning: self.progress.warden_tracker.warning_level as u8,
            warden_cooldown: self.progress.warden_tracker.on_cooldown(),
            last_hurt_by: self.body.combat.defend_target(self.tick),
            last_hurt_mob: self.body.combat.victim(),
            perchable: self.perchable(),
            sprinting: self.pose.sprinting,
        }
    }

    /// Tells the regions this player is in or near about its current state,
    /// which they relay to everyone who can see it. Changed entity data also
    /// goes to the player's own client (its bed and pose, absorption).
    /// `force` sends even when nothing visible changed.
    pub async fn publish(&mut self, force: bool) {
        let mut update = Vec::new();
        let (position, yaw, pitch) = self.published.last_broadcast;
        let moved =
            position != self.pose.position || yaw != self.pose.yaw || pitch != self.pose.pitch;
        let mut movement = Vec::new();
        let mut head = None;
        let mut relative = !force;
        if moved || force {
            self.follow_entities();
            self.published.last_broadcast = (self.pose.position, self.pose.yaw, self.pose.pitch);
            update.push(framed(&play::EntityPositionSync {
                entity_id: self.id,
                position: self.pose.position,
                yaw: self.pose.yaw,
                pitch: self.pose.pitch,
                on_ground: self.pose.on_ground,
            }));
            head = Some(framed(&play::RotateHead {
                entity_id: self.id,
                head_yaw: self.pose.yaw,
            }));
            let delta = crate::world::movement_sync::encode_delta(position, self.pose.position);
            let (old_yaw, new_yaw) = (
                crate::world::movement_sync::pack_degrees(yaw),
                crate::world::movement_sync::pack_degrees(self.pose.yaw),
            );
            let (old_pitch, new_pitch) = (
                crate::world::movement_sync::pack_degrees(pitch),
                crate::world::movement_sync::pack_degrees(self.pose.pitch),
            );
            let turned = old_yaw != new_yaw || old_pitch != new_pitch;
            let shifted = delta.is_some_and(|d| d != [0; 3]);
            match delta {
                Some(delta) if shifted && turned => {
                    movement.push(framed(&play::MoveEntityPosRot {
                        entity_id: self.id,
                        delta,
                        yaw: new_yaw,
                        pitch: new_pitch,
                        on_ground: self.pose.on_ground,
                    }))
                }
                Some(delta) if shifted => movement.push(framed(&play::MoveEntityPos {
                    entity_id: self.id,
                    delta,
                    on_ground: self.pose.on_ground,
                })),
                Some(_) if turned => movement.push(framed(&play::MoveEntityRot {
                    entity_id: self.id,
                    yaw: new_yaw,
                    pitch: new_pitch,
                    on_ground: self.pose.on_ground,
                })),
                Some(_) => {}
                None => relative = false,
            }
            if old_yaw != new_yaw {
                movement.push(framed(&play::RotateHead {
                    entity_id: self.id,
                    head_yaw: self.pose.yaw,
                }));
            }
        }
        let leading = update.len();
        let flags = (
            self.pose.sneaking,
            self.pose.sprinting,
            self.body.fire_ticks > 0,
            self.is_dead(),
            self.living_flags(),
            self.body.absorption,
            self.sleep.sleeping.map(|sleep| sleep.pos),
            self.effect_look(),
            self.current_pose(),
            self.motion.glide.active,
        );
        let flags_changed = self.published.last_flags != flags;
        if flags_changed || force {
            let effects_changed = self.published.last_flags.7 != flags.7;
            self.published.last_flags = flags;
            let values = self.flags_metadata(effects_changed);
            let data = framed(&play::SetEntityData {
                entity_id: self.id,
                values: &values,
            });
            if flags_changed {
                self.send_raw(data.clone());
            }
            update.push(data);
        }
        if self.body.air != self.published.last_air {
            self.published.last_air = self.body.air;
            let data = framed(&play::SetEntityData {
                entity_id: self.id,
                values: &[(metadata::AIR_SUPPLY, MetadataValue::VarInt(self.body.air))],
            });
            self.send_raw(data.clone());
            update.push(data);
        }
        if self.body.ticks_frozen != self.published.last_ticks_frozen {
            self.published.last_ticks_frozen = self.body.ticks_frozen;
            let data = framed(&play::SetEntityData {
                entity_id: self.id,
                values: &[(
                    metadata::TICKS_FROZEN,
                    MetadataValue::VarInt(self.body.ticks_frozen),
                )],
            });
            self.send_raw(data.clone());
            update.push(data);
        }
        if let Some(values) = self.shoulder_metadata() {
            let data = framed(&play::SetEntityData {
                entity_id: self.id,
                values: &values,
            });
            self.send_raw(data.clone());
            update.push(data);
        }
        if self.body.score != self.published.last_score {
            self.published.last_score = self.body.score;
            let data = framed(&play::SetEntityData {
                entity_id: self.id,
                values: &[(metadata::SCORE, MetadataValue::VarInt(self.body.score))],
            });
            self.send_raw(data.clone());
            update.push(data);
        }
        if self.body.stuck_arrows.count != self.published.last_stuck_arrows {
            self.published.last_stuck_arrows = self.body.stuck_arrows.count;
            let data = framed(&play::SetEntityData {
                entity_id: self.id,
                values: &[(
                    metadata::ARROW_COUNT,
                    MetadataValue::VarInt(self.body.stuck_arrows.count),
                )],
            });
            self.send_raw(data.clone());
            update.push(data);
        }
        let mut blast_state = self.published.last_blast_state;
        blast_state.1 = self.mode.abilities.flying && self.mode.abilities.instabuild;
        let equipment_changed = EQUIPMENT_SLOTS
            .into_iter()
            .zip(&self.published.last_equipment)
            .any(|(slot, last)| {
                !tuff_item::item::same_slot(self.inventory.equipped(slot), last.as_deref())
            });
        if equipment_changed || force {
            let equipment = self.equipment_snapshot();
            blast_state.0 = self.attribute(Attribute::ExplosionKnockbackResistance);
            let items: Vec<(u8, Option<tuff_item::item::AnyItem<'_>>)> = EQUIPMENT_SLOTS
                .into_iter()
                .zip(&equipment)
                .map(|(slot, stack)| {
                    (
                        equipment_slot_id(slot),
                        stack.as_ref().map(tuff_item::item::ItemSnapshot::any),
                    )
                })
                .collect();
            update.push(framed(&play::SetEquipment {
                entity_id: self.id,
                items: &items,
            }));
            let spectator = self.mode.game_mode == GameMode::Spectator;
            let sounds: Vec<SoundEvent> = EQUIPMENT_SLOTS
                .into_iter()
                .zip(&self.published.last_equipment)
                .zip(&equipment)
                .filter_map(|((slot, old), new)| {
                    let new = new.as_ref()?;
                    // Only an actual equip/unequip/item-type change plays the
                    // sound; a component-only mutation of the item already
                    // worn (armor durability, enchantments, ...) must not,
                    // or every hit that damages worn armor would replay it.
                    let same = old.as_ref().is_some_and(|old| old.item() == new.item());
                    let equippable = new.equippable()?;
                    (equippable.slot == slot && !same && !spectator)
                        .then_some(equippable.equip_sound)
                })
                .collect();
            for _ in &sounds {
                self.emit_game_event(
                    crate::world::game_event::GameEvent::Equip,
                    self.pose.position,
                )
                .await;
            }
            for sound in sounds {
                let packet = play::Sound {
                    sound_event: sound.id(),
                    category: play::sound_category::PLAYERS,
                    position: self.pose.position,
                    volume: 1.0,
                    pitch: 1.0,
                    seed: self.rng.r#gen(),
                };
                update.push(framed(&packet));
                self.send(&packet);
            }
            self.published.last_equipment = equipment;
        }
        self.sync_attributes(force).await;
        let targets: HashSet<RegionPos> = regions_near(self.pose.position, PRESENCE_MARGIN)
            .into_iter()
            .chain([self.chunk().region()])
            .collect();
        let looks = self.looks();
        if update.is_empty()
            && head.is_none()
            && targets == self.view.presence
            && blast_state == self.published.last_blast_state
            && looks == self.published.last_looks
        {
            return;
        }
        self.published.last_blast_state = blast_state;
        self.published.last_looks = looks;
        self.published.seq += 1;
        let grounded = self.pose.on_ground && !self.is_dead() && self.ride.riding.is_none();
        let feet_now = self.inventory.equipped(EquipmentSlot::Feet);
        if !tuff_item::item::same_slot(feet_now, self.published.last_feet.as_deref()) {
            self.published.last_feet = feet_now.map(tuff_item::item::ItemSnapshot::of);
        }
        let feet = self.published.last_feet.clone();
        let moves = relative.then(|| {
            movement
                .into_iter()
                .chain(update[leading..].iter().cloned())
                .collect::<Vec<_>>()
                .into()
        });
        let message = PlayerUpdate {
            handle: self.conn.handle.clone(),
            uuid: self.profile.id,
            name: self.profile.name.clone(),
            seq: self.published.seq,
            position: self.pose.position,
            chunk: self.chunk(),
            alive: !self.is_dead(),
            spectator: self.mode.game_mode == GameMode::Spectator,
            gamemaster: self.mode.permission >= PermissionLevel::Gamemasters,
            explosion_knockback_resistance: blast_state.0,
            creative_flying: blast_state.1,
            grounded,
            feet,
            landed: self.body.fall.take_landed(),
            looks,
            spawn: self.spawn_packets(),
            update: update.into(),
            swept_from: self.published.last_position.unwrap_or(self.pose.position),
            head,
            moves,
        };
        let previous = std::mem::take(&mut self.view.presence);
        let mut relays = targets;
        relays.extend(self.published.published_home);
        for region in &relays {
            if let Some(handle) = self.region(*region).await {
                handle.request(RegionRequest::Player(message.clone())).await;
            }
        }
        let dimension = self.dimension;
        for region in previous.difference(&relays) {
            if let Some(handle) = self.view.regions.get(&(dimension, *region)) {
                handle
                    .request(RegionRequest::PresenceGone { player: self.id })
                    .await;
            }
        }
        self.view.presence = relays;
        self.published.last_position = Some(self.pose.position);
        self.published.published_home = Some(self.chunk().region());
    }

    /// Sends this player the attributes the client needs synced to render
    /// itself correctly (armor icons, health, movement, ...) whenever one
    /// changed (equipment, enchantments or effects), or unconditionally
    /// when `force` (login, respawn).
    pub async fn sync_attributes(&mut self, force: bool) {
        let worn = self.worn();
        let effects = self.effect_levels();
        let modifiers = attributes::Modifiers {
            equipment: &worn,
            effects: &effects,
        };
        let frost = if self.body.ticks_frozen > 0 && self.pose.on_ground {
            crate::world::freezing::SLOWDOWN
                * crate::world::freezing::percent(self.body.ticks_frozen)
        } else {
            0.0
        };
        let custom = |attribute: Attribute| {
            let mut extras = self.body.attributes.extras(attribute);
            extras.extend(
                self.surroundings
                    .enchantment_modifiers
                    .iter()
                    .filter(|(a, ..)| *a == attribute)
                    .map(|(_, id, amount, operation)| (id.as_str(), *amount, *operation)),
            );
            modifiers.value_from(attribute, self.body.attributes.base(attribute), &extras)
        };
        let mut values: Vec<f64> = attributes::synced_attributes()
            .map(|attribute| match attribute {
                Attribute::MovementSpeed => (custom(attribute) + frost).max(0.0),
                _ => custom(attribute),
            })
            .collect();
        values.push(f64::from(u8::from(self.pose.sprinting)));
        if !force && values == self.published.last_synced_attributes {
            return;
        }
        let sprint = [SPRINTING_MODIFIER];
        let properties: Vec<play::AttributeProperty> = attributes::synced_attributes()
            .zip(&values)
            .map(|(attribute, &base)| play::AttributeProperty {
                attribute,
                base,
                modifiers: if attribute == Attribute::MovementSpeed && self.pose.sprinting {
                    &sprint
                } else {
                    &[]
                },
            })
            .collect();
        self.send(&play::UpdateAttributes {
            entity_id: self.id,
            properties: &properties,
        });
        self.published.last_synced_attributes = values;
    }

    pub async fn broadcast_entity(&mut self, packets: Packets, include_self: bool) {
        if include_self {
            self.send_all(packets.iter().cloned());
        }
        let chunk = self.chunk();
        if let Some(region) = self.region(chunk.region()).await {
            region
                .request(RegionRequest::Broadcast {
                    chunk,
                    entity: Some(self.id),
                    packets,
                    except: Some(self.id),
                })
                .await;
        }
    }

    pub async fn broadcast_chunk(&mut self, packets: Packets) {
        self.broadcast_chunk_from(packets, None).await;
    }

    /// Sounds the client already plays for its own player (as a sound
    /// played "by" that player): everyone around hears them but the player.
    pub async fn broadcast_chunk_to_others(&mut self, packets: Packets) {
        self.broadcast_chunk_from(packets, Some(self.id)).await;
    }

    pub(super) async fn broadcast_chunk_from(&mut self, packets: Packets, except: Option<i32>) {
        let chunk = self.chunk();
        if let Some(region) = self.region(chunk.region()).await {
            region
                .request(RegionRequest::Broadcast {
                    chunk,
                    entity: None,
                    packets,
                    except,
                })
                .await;
        }
    }

    /// Publishes the saved form for other actors' selectors: every tick
    /// while one has read it within [`SELECTOR_INTEREST_TICKS`], else every
    /// [`SELECTOR_IDLE_PERIOD`] ticks (staggered by id).
    pub(super) fn publish_selector_data(&mut self) {
        if self.conn.handle.take_selector_interest() {
            self.published.selector_hot_until = self.tick + SELECTOR_INTEREST_TICKS;
        }
        let due = self.tick < self.published.selector_hot_until
            || (self.tick + self.id as u64).is_multiple_of(SELECTOR_IDLE_PERIOD)
            || !self.conn.handle.has_selector_data();
        if due {
            self.conn.handle.publish_selector_data(self.selector_nbt());
            self.conn.handle.publish_live_facts(self.live_facts());
        }
    }

    /// Tells the coordinator where this player is and how it stands, for
    /// selectors and commands, whenever that changed.
    pub async fn report_state(&mut self) {
        let state = crate::world::messages::PlayerState {
            dimension: self.dimension,
            position: self.pose.position,
            looks: self.looks(),
            rotation: [self.pose.yaw, self.pose.pitch],
            game_mode: self.mode.game_mode,
            level: self.body.experience.level,
            alive: !self.is_dead(),
            sleeping: self.sleep.sleeping.is_some(),
            deep_sleeping: self.sleep.sleeping.is_some()
                && self.sleep.sleep_counter >= DEEP_SLEEP_TICKS,
            time_since_rest: self
                .progress
                .stats
                .get(crate::world::stats::Stat::custom(CustomStat::TimeSinceRest)),
            advancements: Some(self.advancement_view()),
            tags: self.record.tags(),
            waypoint_transmit_range: self.attribute_value(Attribute::WaypointTransmitRange),
            waypoint_receive_range: self.attribute_value(Attribute::WaypointReceiveRange),
            waypoint_icon: self.record.waypoint_icon(),
            view_distance: self.effective_view_distance(),
            health: self.body.health.value,
            food: self.body.food.level,
            experience_total: self.body.experience.total,
            effects: self
                .body
                .effects
                .iter()
                .map(|active| (active.effect, active.amplifier))
                .collect(),
        };
        if self.published.last_state.as_ref() == Some(&state) {
            return;
        }
        self.published.last_state = Some(state.clone());
        self.coordinator(CoordinatorMessage::PlayerState {
            player: self.id,
            state,
        })
        .await;
    }
}
