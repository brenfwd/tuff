//! The session's side of advancements: firing trigger events with the
//! player's facts, granting and revoking criteria, rewards, chat
//! announcements and the Update Advancements packets.

use tuff_item::item::DupUnchecked;
use tuff_vanilla_data::registry::{CustomStat, EntityType};

use super::{PlayerSession, UNLOCK_WATCHED_SLOTS};
use crate::world::advancement::trigger::Event;
use crate::world::advancement::{Advancements, View};
use crate::world::messages::CoordinatorMessage;
use crate::world::predicate::{EntityFacts, Flags};
use tuff_item::item::{ItemStack, MintReason};

/// Window slots in the game's inventory order: hotbar, main inventory,
/// armor from the feet up, then the off hand. Two always-empty slots
/// (body and saddle) follow in the game's count.
const INVENTORY_ORDER: [usize; 41] = {
    let mut order = [0; 41];
    let mut i = 0;
    while i < 9 {
        order[i] = 36 + i;
        i += 1;
    }
    while i < 36 {
        order[i] = i;
        i += 1;
    }
    order[36] = 8;
    order[37] = 7;
    order[38] = 6;
    order[39] = 5;
    order[40] = 45;
    order
};
const ALWAYS_EMPTY_SLOTS: usize = 2;
/// A player sliding down a honey block's side falls faster than this.
const HONEY_SLIDE_SPEED: f64 = -0.08;
/// How far a vehicle's box is shrunk before testing it for lava.
const VEHICLE_LAVA_INSET: f64 = 0.001;

impl PlayerSession {
    /// The player as advancement predicates see it.
    pub fn player_facts(&self) -> EntityFacts {
        EntityFacts {
            kind: Some(EntityType::Player),
            position: self.pose.position,
            dimension: self.dimension().key.clone(),
            biome: self.surroundings.biome.map(str::to_owned),
            flags: Flags {
                on_fire: self.body.fire_ticks > 0,
                sneaking: self.pose.sneaking,
                sprinting: self.pose.sprinting,
                swimming: self.pose.swimming,
                on_ground: self.pose.on_ground,
                flying: self.mode.abilities.flying,
                baby: false,
                in_water: self.body.fall.in_water(),
                fall_flying: self.is_gliding(),
            },
            equipment: self.equipment(),
            vehicle: self.ride.riding.as_ref().map(|riding| {
                Box::new(EntityFacts {
                    kind: Some(riding.kind),
                    position: riding.position,
                    dimension: self.dimension().key.clone(),
                    ..EntityFacts::default()
                })
            }),
            components: Vec::new(),
            player: Some(crate::world::predicate::PlayerFacts {
                game_mode: self.mode.game_mode,
                level: self.body.experience.level,
                advancements: None,
                stats: None,
                input: None,
                food: Some((self.body.food.level, self.body.food.saturation)),
            }),
            tick_count: Some(self.tick as i64),
            movement: Some(self.sync.known_movement),
            fall_distance: Some(self.body.fall.distance),
            stepping_on: self.surroundings.stepping_on,
            looking_at: None,
            effects: self
                .body
                .effects
                .iter()
                .map(|e| {
                    (
                        e.effect,
                        i32::from(e.amplifier),
                        e.duration,
                        e.ambient,
                        e.show_particles,
                    )
                })
                .collect(),
            ..EntityFacts::default()
        }
    }

    /// Counts `event` towards every criterion it meets.
    pub fn fire(&mut self, event: Event<'_>) {
        let tree = self.progress.advancement_tree.clone();
        tree.fire(self, event);
    }

    /// After a data pack reload: moves the player onto `tree`, which
    /// carries its progress over.
    pub fn rebind_advancements(&mut self, tree: std::sync::Arc<dyn Advancements>) {
        let old = std::mem::replace(&mut self.progress.advancement_tree, tree);
        if std::sync::Arc::ptr_eq(&old, &self.progress.advancement_tree) {
            return;
        }
        let new = self.progress.advancement_tree.clone();
        new.rebind(self, &*old);
    }

    /// Adds recipes to the recipe book, tells the client, and counts each
    /// new one towards `recipe_unlocked` criteria.
    pub fn award_recipes<'a>(&mut self, recipes: impl IntoIterator<Item = &'a str>) {
        let set = self.shared.recipes();
        let (entries, learned) = self.progress.recipe_book.learn(&set, recipes);
        self.send_recipe_entries(entries);
        for recipe in learned {
            self.fire(Event::RecipeUnlocked(&recipe));
        }
    }

    /// Experience, loot, the reward function and the chat announcement of
    /// advancements completed since the last call.
    pub async fn apply_advancement_rewards(&mut self) {
        loop {
            let tree = self.progress.advancement_tree.clone();
            let rewards = tree.take_rewards(self);
            if rewards.is_empty() {
                return;
            }
            for reward in rewards {
                if reward.experience != 0 {
                    self.give_experience(reward.experience).await;
                }
                for table in &reward.loot {
                    let loot = self.shared.block_loot();
                    let facts = crate::world::drops::EntityFacts {
                        kind: Some(EntityType::Player),
                        on_fire: self.body.fire_ticks > 0,
                        score_holder: Some(self.profile.name.clone()),
                        player_name: Some(self.profile.name.clone()),
                        tables: Some(self.shared.loot_tables()),
                        origin: Some(crate::world::predicate::LocationFacts {
                            position: self.pose.position,
                            dimension: self.dimension().key.clone(),
                            biome: self.surroundings.biome.map(str::to_owned),
                            ..Default::default()
                        }),
                        ..Default::default()
                    };
                    let drops = loot.entity_drops(table, &facts, &mut self.rng);
                    for stack in drops {
                        let count = i32::from(stack.count());
                        self.give_items(stack, count).await;
                    }
                }
                if let Some(function) = &reward.function {
                    self.run_reward_function(function).await;
                }
                if let Some(message) = reward.announcement {
                    log::info!("{}", crate::world::message_plain(&message));
                    self.coordinator(CoordinatorMessage::Announce(message))
                        .await;
                }
            }
        }
    }

    /// Runs a data pack reward function as this player, silently and with
    /// gamemaster permission.
    pub async fn run_reward_function(&mut self, function: &str) {
        if self
            .shared
            .datapacks
            .contents()
            .function(function)
            .is_none()
        {
            return;
        }
        let mut source = self.command_source();
        source.permission = tuff_storage::admin::PermissionLevel::Gamemasters;
        source.silent = true;
        let input = format!("function {function}");
        crate::world::commands::run(crate::world::commands::Actor::Player(self), source, &input)
            .await;
    }

    /// Seen Advancements: the client opened a tab or closed the screen.
    pub fn seen_advancements(&mut self, tab: Option<String>) {
        let Some(tab) = tab else {
            return;
        };
        let tree = self.progress.advancement_tree.clone();
        tree.seen_tab(self, &tab);
    }

    /// Counts inventory slots that changed since the last scan towards
    /// `inventory_changed` criteria, one event per slot as the game fires
    /// it. The crafting result and grid are not inventory slots.
    pub fn scan_inventory(&mut self) {
        let slots = self.inventory.slots();
        if slots == self.progress.last_inventory.as_slice() {
            return;
        }
        let changed: Vec<Option<ItemStack>> = UNLOCK_WATCHED_SLOTS
            .filter(|slot| {
                slots[*slot]
                    != self
                        .progress
                        .last_inventory
                        .get(*slot)
                        .and_then(DupUnchecked::dup_unchecked)
            })
            .map(|slot| slots[slot].dup_unchecked())
            .collect();
        let mut inventory: Vec<Option<ItemStack>> = INVENTORY_ORDER
            .iter()
            .map(|&slot| slots[slot].dup_unchecked())
            .collect();
        inventory.extend(std::iter::repeat_with(|| None).take(ALWAYS_EMPTY_SLOTS));
        self.progress.last_inventory = slots
            .iter()
            .map(DupUnchecked::dup_unchecked)
            .collect::<Vec<_>>();
        for stack in &changed {
            self.fire(Event::InventoryChanged {
                changed: stack.as_ref(),
                inventory: &inventory,
            });
        }
    }

    /// A held item used on a block: placed-block and item-used-on-block
    /// criteria, tested with the stack as it was used.
    pub fn used_on_block(
        &mut self,
        off_hand: bool,
        item: tuff_vanilla_data::Item,
        placement: &crate::world::messages::Placement,
    ) {
        let stack = self
            .inventory
            .hand(off_hand)
            .filter(|stack| stack.item == item)
            .map(DupUnchecked::dup_unchecked)
            .unwrap_or_else(|| ItemStack::mint(item, 1, MintReason::Template));
        if let Some(placed) = &placement.placed {
            self.fire(Event::PlacedBlock {
                location: placed,
                item: &stack,
            });
        }
        self.fire(Event::ItemUsedOnBlock {
            location: &placement.used_on,
            item: &stack,
        });
    }

    /// Recipes crafted in a result slot, with the stacks they used.
    pub fn crafted_recipes(
        &mut self,
        crafted: &[(tuff_vanilla_data::Frozen<str>, Vec<ItemStack>)],
    ) {
        self.award_recipes(crafted.iter().map(|(recipe, _)| &**recipe));
        for (recipe, ingredients) in crafted {
            self.fire(Event::RecipeCrafted {
                recipe,
                ingredients,
            });
        }
    }

    /// An advancement event another actor reported.
    pub fn reported(&mut self, event: crate::world::messages::AdvancementEvent) {
        use crate::world::messages::AdvancementEvent;
        let dimension = self.dimension().key.clone();
        let here = |mut facts: EntityFacts| {
            if facts.dimension.is_empty() {
                facts.dimension = dimension.clone();
            }
            facts
        };
        match event {
            AdvancementEvent::BredAnimals {
                parent,
                partner,
                child,
            } => {
                let (parent, partner, child) = (here(parent), here(partner), child.map(here));
                if parent.kind != Some(EntityType::Frog) {
                    self.progress.stats.award_custom(CustomStat::AnimalsBred, 1);
                }
                self.fire(Event::BredAnimals {
                    parent: &parent,
                    partner: &partner,
                    child: child.as_ref(),
                });
            }
            AdvancementEvent::CuredZombieVillager { zombie, villager } => {
                let (zombie, villager) = (here(zombie), here(villager));
                self.fire(Event::CuredZombieVillager {
                    zombie: &zombie,
                    villager: &villager,
                });
            }
            AdvancementEvent::BrewedPotion(potion) => self.fire(Event::BrewedPotion(potion)),
            AdvancementEvent::RaidOmen => self.fire(Event::RaidOmen),
            AdvancementEvent::RaidWin => self.fire(Event::RaidWin),
            AdvancementEvent::LightningStrike {
                lightning,
                bystanders,
            } => {
                let lightning = here(lightning);
                let bystanders: Vec<EntityFacts> = bystanders.into_iter().map(here).collect();
                self.fire(Event::LightningStrike {
                    lightning: &lightning,
                    bystanders: &bystanders,
                });
            }
            AdvancementEvent::SummonedEntity(entity) => {
                let entity = here(entity);
                self.fire(Event::SummonedEntity(&entity));
            }
            AdvancementEvent::GeneratedContainerLoot(table) => {
                self.fire(Event::PlayerGeneratesContainerLoot(&table));
            }
            AdvancementEvent::TameAnimal(entity) => {
                let entity = here(entity);
                self.fire(Event::TameAnimal(&entity));
            }
            AdvancementEvent::ChanneledLightning { victims } => {
                let victims: Vec<EntityFacts> = victims.into_iter().map(here).collect();
                self.fire(Event::ChanneledLightning { victims: &victims });
            }
            AdvancementEvent::FishingRodHooked { entity, items } => {
                let entity = here(entity);
                let rod = [false, true]
                    .into_iter()
                    .filter_map(|off_hand| {
                        self.inventory
                            .hand(off_hand)
                            .map(DupUnchecked::dup_unchecked)
                    })
                    .find(|stack| stack.item == tuff_vanilla_data::Item::FishingRod);
                if let Some(rod) = rod {
                    self.fire(Event::FishingRodHooked {
                        rod: &rod,
                        entity: &entity,
                        items: &items,
                    });
                }
            }
            AdvancementEvent::GeneratedLoot(table) => {
                self.fire(Event::PlayerGeneratesContainerLoot(&table));
            }
            AdvancementEvent::HurtEntity {
                victim,
                damage,
                weapon,
                dealt,
                taken,
            } => {
                let victim = here(victim);
                self.hurt_entity_facts(&weapon, &victim, &damage, dealt, taken);
            }
            AdvancementEvent::InteractedWithEntity { item, entity } => {
                let entity = here(entity);
                self.fire(Event::PlayerInteractedWithEntity {
                    item: item.as_ref(),
                    entity: &entity,
                });
            }
            AdvancementEvent::UsedBlock {
                location,
                off_hand,
                default,
            } => {
                if default {
                    self.fire(Event::DefaultBlockUse {
                        location: &location,
                    });
                }
                let item = self
                    .inventory
                    .hand(off_hand)
                    .map(DupUnchecked::dup_unchecked)
                    .unwrap_or_else(ItemStack::empty);
                self.fire(Event::AnyBlockUse {
                    location: &location,
                    item: &item,
                });
            }
            AdvancementEvent::ConstructBeacon(level) => {
                self.fire(Event::ConstructBeacon { level });
            }
            AdvancementEvent::TargetHit { projectile, signal } => {
                let projectile = here(projectile);
                self.fire(Event::TargetHit {
                    projectile: &projectile,
                    signal,
                });
            }
            AdvancementEvent::KillMobNearSculkCatalyst(kill) => {
                let victim = here(kill.victim);
                self.fire(Event::KillMobNearSculkCatalyst {
                    victim: &victim,
                    killing_blow: &kill.killing_blow,
                });
            }
            AdvancementEvent::AvoidVibration => self.fire(Event::AvoidVibration),
            AdvancementEvent::CrafterRecipeCrafted {
                recipe,
                ingredients,
            } => {
                self.fire(Event::CrafterRecipeCrafted {
                    recipe: &recipe,
                    ingredients: &ingredients,
                });
            }
            AdvancementEvent::BeeNestDestroyed { block, item, bees } => {
                self.fire(Event::BeeNestDestroyed {
                    block,
                    item: item.as_ref(),
                    bees,
                });
            }
            AdvancementEvent::AllayDropItemOnBlock { item, location } => {
                self.fire(Event::AllayDropItemOnBlock {
                    location: &location,
                    item: &item,
                });
            }
            AdvancementEvent::ThrownItemPickedUpByEntity { item, entity } => {
                let entity = here(entity);
                self.fire(Event::ThrownItemPickedUpByEntity {
                    item: &item,
                    entity: &entity,
                });
            }
            AdvancementEvent::ThrownItemPickedUpByPlayer { item, thrower } => {
                let thrower = here(thrower);
                self.fire(Event::ThrownItemPickedUpByPlayer {
                    item: &item,
                    entity: &thrower,
                });
            }
            AdvancementEvent::ShearedEquipment { item, entity } => {
                let entity = here(entity);
                self.fire(Event::PlayerShearedEquipment {
                    item: &item,
                    entity: &entity,
                });
            }
        }
    }

    /// Changed dimension through a portal or teleport, from `from` at
    /// `position`: `changed_dimension`, and `nether_travel` on the way back
    /// to the overworld.
    pub fn changed_dimension(&mut self, from: &str, position: [f64; 3]) {
        use crate::world::dimension::{OVERWORLD, THE_NETHER};
        let to = self.dimension().key.clone();
        if from == to {
            return;
        }
        if from == OVERWORLD && to == THE_NETHER {
            self.progress.entered_nether = Some(position);
        }
        self.fire(Event::ChangedDimension { from, to: &to });
        if from == THE_NETHER
            && to == OVERWORLD
            && let Some(entered) = self.progress.entered_nether
        {
            self.fire(Event::NetherTravel { entered });
        }
        if to != THE_NETHER {
            self.progress.entered_nether = None;
        }
    }

    /// Damage that got past this player's immunities: `amount` before and
    /// `taken` after a shield.
    pub fn hurt_by_source(
        &mut self,
        source: &super::DamageSource,
        amount: f32,
        taken: f32,
        blocked: bool,
    ) {
        use crate::world::predicate::DamageFacts;
        let at = |kind| EntityFacts {
            kind: Some(kind),
            position: source.position.unwrap_or(self.pose.position),
            dimension: self.dimension().key.clone(),
            ..EntityFacts::default()
        };
        let attacker = source.attacker.as_ref().map(|a| at(a.kind));
        let damage = DamageFacts {
            kind: source.kind,
            direct: source.projectile.map(at).or_else(|| attacker.clone()),
            source: attacker,
        };
        self.fire(Event::EntityHurtPlayer {
            damage: &damage,
            dealt: amount,
            taken,
            blocked,
        });
    }

    /// This player was credited with a kill.
    pub fn killed_entity(&mut self, kill: crate::world::messages::Kill) {
        let mut victim = kill.victim;
        if victim.dimension.is_empty() {
            victim.dimension = self.dimension().key.clone();
        }
        self.fire(Event::PlayerKilledEntity {
            victim: &victim,
            killing_blow: &kill.killing_blow,
        });
        let by_arrow = kill.killing_blow.direct.as_ref().is_some_and(|direct| {
            matches!(
                direct.kind,
                Some(EntityType::Arrow | EntityType::SpectralArrow)
            )
        });
        if by_arrow {
            let weapon = kill
                .killing_blow
                .direct
                .as_ref()
                .and_then(|d| d.equipment.first().map(|(_, stack)| stack.dup_unchecked()));
            let arrow = kill.killing_blow.direct.as_ref().and_then(|d| d.entity_id);
            self.progress.arrow_kills.push((victim, weapon, arrow));
        }
    }

    /// Fires `killed_by_arrow` for the arrow kills of the last tick, as one
    /// volley per arrow.
    pub fn flush_arrow_kills(&mut self) {
        let pending = std::mem::take(&mut self.progress.arrow_kills);
        for (victims, weapon) in group_arrow_kills(pending) {
            self.fire(Event::KilledByArrow {
                victims: &victims,
                weapon: weapon.as_ref(),
            });
        }
    }

    /// This player's death to a melee `attack` as the attacking player's
    /// statistics and advancements see it.
    pub fn killed_by_player(
        &self,
        attack: &crate::world::messages::Attack,
    ) -> crate::world::messages::Kill {
        use crate::world::predicate::DamageFacts;
        use tuff_vanilla_data::item::EquipmentSlot;
        let attacker = EntityFacts {
            kind: Some(EntityType::Player),
            position: attack.attacker_position,
            dimension: self.dimension().key.clone(),
            equipment: attack
                .weapon
                .iter()
                .map(|weapon| (EquipmentSlot::Mainhand, weapon.dup_unchecked()))
                .collect(),
            ..EntityFacts::default()
        };
        crate::world::messages::Kill {
            kind: EntityType::Player,
            holder: self.profile.name.clone(),
            victim: self.player_facts(),
            killing_blow: DamageFacts {
                kind: &tuff_vanilla_data::damage_type::PLAYER_ATTACK,
                source: Some(attacker.clone()),
                direct: Some(attacker),
            },
        }
    }

    fn hurt_entity_facts(
        &mut self,
        weapon: &Option<tuff_item::item::ItemStack>,
        victim: &EntityFacts,
        damage: &crate::world::predicate::DamageFacts,
        dealt: f32,
        taken: f32,
    ) {
        use tuff_vanilla_data::item::EquipmentSlot;
        let mut attacker = self.player_facts();
        if let Some(weapon) = weapon {
            attacker
                .equipment
                .retain(|(slot, _)| *slot != EquipmentSlot::Mainhand);
            attacker
                .equipment
                .push((EquipmentSlot::Mainhand, weapon.dup_unchecked()));
        }
        let damage = crate::world::predicate::DamageFacts {
            kind: damage.kind,
            source: Some(attacker.clone()),
            direct: damage.direct.clone().or(Some(attacker)),
        };
        self.fire(Event::PlayerHurtEntity {
            entity: victim,
            damage: &damage,
            dealt,
            taken,
            blocked: false,
        });
    }

    /// This player's melee hit landed on an entity of type `kind`.
    pub fn hurt_entity(
        &mut self,
        weapon: &Option<tuff_item::item::ItemStack>,
        kind: EntityType,
        position: [f64; 3],
        dealt: f32,
        taken: f32,
    ) {
        use crate::world::predicate::DamageFacts;
        use tuff_vanilla_data::item::EquipmentSlot;
        let mut attacker = self.player_facts();
        if let Some(weapon) = weapon {
            attacker
                .equipment
                .retain(|(slot, _)| *slot != EquipmentSlot::Mainhand);
            attacker
                .equipment
                .push((EquipmentSlot::Mainhand, weapon.dup_unchecked()));
        }
        let entity = EntityFacts {
            kind: Some(kind),
            position,
            dimension: self.dimension().key.clone(),
            ..EntityFacts::default()
        };
        let damage = DamageFacts {
            kind: &tuff_vanilla_data::damage_type::PLAYER_ATTACK,
            source: Some(attacker.clone()),
            direct: Some(attacker),
        };
        self.fire(Event::PlayerHurtEntity {
            entity: &entity,
            damage: &damage,
            dealt,
            taken,
            blocked: false,
        });
    }

    /// An entity of type `killer` killed this player.
    pub fn killed_by(
        &mut self,
        killer: EntityType,
        kind: &'static tuff_vanilla_data::damage_type::DamageType,
        projectile: Option<EntityType>,
        position: Option<[f64; 3]>,
    ) {
        use crate::world::predicate::DamageFacts;
        let killer = EntityFacts {
            kind: Some(killer),
            position: position.unwrap_or(self.pose.position),
            dimension: self.dimension().key.clone(),
            ..EntityFacts::default()
        };
        let killing_blow = DamageFacts {
            kind,
            direct: Some(match projectile {
                Some(projectile) => EntityFacts {
                    kind: Some(projectile),
                    ..killer.clone()
                },
                None => killer.clone(),
            }),
            source: Some(killer.clone()),
        };
        self.fire(Event::EntityKilledPlayer {
            killer: &killer,
            killing_blow: &killing_blow,
        });
    }

    /// Trades taken from a merchant: `(offer, times)`.
    pub fn traded(
        &mut self,
        merchant: i32,
        offers: &[crate::world::trading::MerchantOffer],
        trades: &[(usize, u32)],
    ) {
        let (kind, position) = self.view.entities.visible_entity(merchant).map_or(
            (EntityType::Villager, self.pose.position),
            |(kind, _, position)| (kind, position),
        );
        let villager = EntityFacts {
            kind: Some(kind),
            position,
            dimension: self.dimension().key.clone(),
            ..EntityFacts::default()
        };
        for &(offer, times) in trades {
            let Some(offer) = offers.get(offer) else {
                continue;
            };
            for _ in 0..times {
                self.fire(Event::VillagerTrade {
                    villager: &villager,
                    item: &offer.sell,
                });
            }
        }
    }

    /// Potions taken out of a brewing stand's bottle slots.
    pub fn took_potions(&mut self, before: &[Option<ItemStack>], after: &[Option<ItemStack>]) {
        for (slot, taken) in before
            .iter()
            .enumerate()
            .take(crate::world::menu::brewing::INGREDIENT)
        {
            let Some(taken) = taken else {
                continue;
            };
            let left = after.get(slot).and_then(DupUnchecked::dup_unchecked);
            if left.as_ref().is_some_and(|stack| stack.item == taken.item) {
                continue;
            }
            if let Some(contents) = &taken.components.potion_contents {
                self.fire(Event::BrewedPotion(contents.potion.map(|p| p.name())));
            }
        }
    }

    /// The progress as selectors read it.
    pub fn advancement_view(&mut self) -> View {
        let tree = self.progress.advancement_tree.clone();
        tree.view(self)
    }

    /// Levitation, the end of a fall and the item in use, each tick.
    fn track_motion_triggers(&mut self) {
        use tuff_vanilla_data::registry::MobEffect;
        if self.body.effects.has(MobEffect::Levitation) {
            let (start, since) = *self
                .progress
                .levitation_start
                .get_or_insert((self.pose.position, self.tick));
            self.fire(Event::Levitation {
                start,
                duration: (self.tick - since) as i32,
            });
        } else {
            self.progress.levitation_start = None;
        }
        if self.body.fall.distance > 0.0 {
            if self.progress.fall_start.is_none() {
                self.progress.fall_start = Some(self.pose.position);
                if let Some((impact, cause)) = self.progress.explosion_impact.take()
                    && impact[1] <= self.pose.position[1]
                {
                    self.fire(Event::FallAfterExplosion {
                        start: impact,
                        cause: cause.as_ref(),
                    });
                }
            }
        } else if let Some(start) = self.progress.fall_start.take()
            && !self.is_dead()
        {
            self.fire(Event::FallFromHeight { start });
        }
        self.track_lava_ride();
        if let Some(state) = self.surroundings.contact.honey_side
            && !self.pose.on_ground
            && self.sync.known_movement[1] < HONEY_SLIDE_SPEED
            && self.tick.is_multiple_of(20)
        {
            self.fire(Event::SlideDownBlock(state));
        }
        if let Some(using) = &self.actions.using
            && let Some(stack) = self
                .inventory
                .hand(using.off_hand)
                .filter(|stack| stack.item == using.item)
                .map(DupUnchecked::dup_unchecked)
        {
            self.fire(Event::UsingItem(&stack));
        }
    }

    /// `ride_entity_in_lava`: every tick the vehicle is in lava (as the
    /// client's copy of the world has it), from where the ride entered it.
    fn track_lava_ride(&mut self) {
        let in_lava = self.ride.riding.as_ref().is_some_and(|riding| {
            let size = riding.kind.size();
            let half = f64::from(size.width) / 2.0;
            let [x, y, z] = riding.position;
            let bounds = [
                x - half,
                y,
                z - half,
                x + half,
                y + f64::from(size.height),
                z + half,
            ];
            let world = self.client_world();
            crate::world::contact::box_touches(
                bounds,
                VEHICLE_LAVA_INSET,
                tuff_vanilla_data::tag::fluid::key::LAVA,
                |pos| world.block_at(pos),
            )
        });
        if !in_lava {
            self.progress.lava_ride_start = None;
            return;
        }
        match self.progress.lava_ride_start {
            None => self.progress.lava_ride_start = Some(self.pose.position),
            Some(start) => self.fire(Event::RideEntityInLava { start }),
        }
    }

    pub async fn tick_advancements(&mut self) {
        self.scan_inventory();
        self.fire(Event::Tick);
        self.track_motion_triggers();
        if self.tick.is_multiple_of(20) {
            self.fire(Event::Location);
        }
        self.apply_advancement_rewards().await;
        let tree = self.progress.advancement_tree.clone();
        tree.flush(self, true);
    }
}

type ArrowKill = (EntityFacts, Option<ItemStack>, Option<i32>);

fn group_arrow_kills(mut pending: Vec<ArrowKill>) -> Vec<(Vec<EntityFacts>, Option<ItemStack>)> {
    let mut volleys = Vec::new();
    while !pending.is_empty() {
        let (weapon, arrow) = (pending[0].1.dup_unchecked(), pending[0].2);
        let (volley, rest): (Vec<_>, Vec<_>) = pending
            .into_iter()
            .partition(|(_, w, a)| *w == weapon && *a == arrow);
        pending = rest;
        volleys.push((volley.into_iter().map(|(v, _, _)| v).collect(), weapon));
    }
    volleys
}

#[cfg(test)]
mod arrow_volley_tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn separate_arrows_of_one_weapon_are_separate_volleys() {
        let kill = |arrow| (EntityFacts::default(), None, arrow);
        let volleys = group_arrow_kills(vec![kill(Some(1)), kill(Some(2)), kill(Some(1))]);
        let sizes: Vec<_> = volleys.iter().map(|(v, _)| v.len()).collect();
        assert_eq!(sizes, vec![2, 1]);
    }
}
