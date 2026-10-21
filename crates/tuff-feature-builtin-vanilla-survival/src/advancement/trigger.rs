use std::sync::Arc;

use serde_json::Value;
pub use tuff_core::world::advancement::trigger::{Event, Handler, Registry};
use tuff_core::world::predicate::{
    self, Context, DamageFacts, EntityFacts, LocationFacts, bounds, bounds_sqr, in_set,
};
use tuff_item::item::ItemStack;
use tuff_vanilla_data::BlockState;
use tuff_vanilla_data::registry::TriggerType;

/// Every vanilla trigger type. Those whose event this server never fires
/// still parse and hold progress; see [`NEVER_FIRED`].
pub fn builtin_registry() -> Registry {
    let mut registry = Registry::new();
    for kind in TriggerType::ALL {
        registry.register(kind.name(), builtin(kind));
    }
    registry
}

pub fn builtin_handler(kind: TriggerType) -> Handler {
    builtin(kind)
}

/// Vanilla triggers no system here fires yet: criteria using them are
/// loaded and can be granted by command, but never met in play.
pub const NEVER_FIRED: &[TriggerType] = &[TriggerType::Impossible];

fn handler(f: fn(&Value, &Event<'_>, &EntityFacts) -> bool) -> Handler {
    Arc::new(f)
}

/// An optional item predicate field.
fn item_field(conditions: &Value, key: &str, stack: &ItemStack) -> bool {
    conditions
        .get(key)
        .is_none_or(|predicate| predicate::item(predicate, stack))
}

/// An optional entity condition field, tested with the entity as `this`
/// and the player's position as the origin.
fn entity_field(conditions: &Value, key: &str, entity: &EntityFacts, player: &EntityFacts) -> bool {
    conditions.get(key).is_none_or(|condition| {
        predicate::condition(
            condition,
            &Context {
                this: Some(entity),
                target: None,
                origin: player.position,
                location: None,
                tool: None,
                enchantment_level: None,
            },
        )
    })
}

fn optional_entity_field(
    conditions: &Value,
    key: &str,
    entity: Option<&EntityFacts>,
    player: &EntityFacts,
) -> bool {
    match (conditions.get(key), entity) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(_), Some(entity)) => entity_field(conditions, key, entity, player),
    }
}

/// The `bystander` condition field: absent always matches; present, at
/// least one of `bystanders` must satisfy it.
/// Each `victims` predicate must be met by a victim; with `distinct` a victim
/// satisfies at most one predicate.
fn victims_field(
    conditions: &Value,
    victims: &[EntityFacts],
    player: &EntityFacts,
    distinct: bool,
) -> bool {
    let Some(wanted) = conditions.get("victims").and_then(Value::as_array) else {
        return true;
    };
    let mut used = vec![false; victims.len()];
    wanted.iter().all(|condition| {
        let found = victims.iter().enumerate().position(|(i, victim)| {
            !(distinct && used[i])
                && predicate::condition(
                    condition,
                    &Context {
                        this: Some(victim),
                        target: None,
                        origin: player.position,
                        location: None,
                        tool: None,
                        enchantment_level: None,
                    },
                )
        });
        found.is_some_and(|i| {
            used[i] = true;
            true
        })
    })
}

fn bystander_field(conditions: &Value, bystanders: &[EntityFacts], player: &EntityFacts) -> bool {
    if conditions.get("bystander").is_none() {
        return true;
    }
    bystanders
        .iter()
        .any(|entity| entity_field(conditions, "bystander", entity, player))
}

fn location_field(
    conditions: &Value,
    location: &LocationFacts,
    item: Option<&ItemStack>,
    player: &EntityFacts,
) -> bool {
    conditions.get("location").is_none_or(|condition| {
        predicate::condition(
            condition,
            &Context {
                this: Some(player),
                target: None,
                origin: location.position,
                location: Some(location),
                tool: item,
                enchantment_level: None,
            },
        )
    })
}

/// The `blocks` and `state` fields of a block trigger, a bare block
/// predicate beside the trigger's other fields.
fn block_fields(conditions: &Value, state: BlockState) -> bool {
    let Some(fields) = conditions.as_object() else {
        return true;
    };
    let predicate: serde_json::Map<String, Value> = fields
        .iter()
        .filter(|(key, _)| matches!(key.as_str(), "blocks" | "state"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    predicate.is_empty() || predicate::block(&Value::Object(predicate), state)
}

/// A distance trigger's `start_position` and `distance` from `start` to
/// where the player is now.
fn travelled(conditions: &Value, start: [f64; 3], player: &EntityFacts) -> bool {
    let from = LocationFacts {
        position: start,
        dimension: player.dimension.clone(),
        ..LocationFacts::default()
    };
    conditions
        .get("start_position")
        .is_none_or(|p| predicate::location(p, &from))
        && conditions
            .get("distance")
            .is_none_or(|p| predicate::distance(p, start, player.position))
}

fn picked_up(
    conditions: &Value,
    item: &ItemStack,
    entity: &EntityFacts,
    player: &EntityFacts,
) -> bool {
    item_field(conditions, "item", item) && entity_field(conditions, "entity", entity, player)
}

fn id_field(conditions: &Value, key: &str, id: &str) -> bool {
    conditions
        .get(key)
        .is_none_or(|expected| in_set(expected, id, |_| false))
}

fn loot_tables(conditions: &Value, table: &str) -> bool {
    conditions.get("loot_tables").is_some_and(|set| {
        in_set(set, table, |tag| {
            tuff_datapack::tags::with(|tags| tags.contains("minecraft:loot_table", tag, table))
        })
    })
}

fn slots_match(slots: &Value, inventory: &[Option<ItemStack>]) -> bool {
    let Some(fields) = slots.as_object() else {
        return false;
    };
    let mut full = 0u32;
    let mut empty = 0u32;
    let mut occupied = 0u32;
    for slot in inventory {
        match slot {
            None => empty += 1,
            Some(stack) => {
                occupied += 1;
                if stack.count >= stack.max_stack_size() {
                    full += 1;
                }
            }
        }
    }
    fields.iter().all(|(key, bound)| match key.as_str() {
        "occupied" => bounds(bound, f64::from(occupied)),
        "full" => bounds(bound, f64::from(full)),
        "empty" => bounds(bound, f64::from(empty)),
        _ => false,
    })
}

fn inventory_changed(conditions: &Value, event: &Event<'_>, _: &EntityFacts) -> bool {
    let Event::InventoryChanged { changed, inventory } = event else {
        return false;
    };
    if let Some(slots) = conditions.get("slots")
        && !slots_match(slots, inventory)
    {
        return false;
    }
    let items = conditions
        .get("items")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    match items {
        [] => true,
        [only] => changed.is_some_and(|stack| predicate::item(only, stack)),
        _ => {
            let mut remaining: Vec<&Value> = items.iter().collect();
            for stack in inventory.iter().flatten() {
                remaining.retain(|predicate| !predicate::item(predicate, stack));
                if remaining.is_empty() {
                    break;
                }
            }
            remaining.is_empty()
        }
    }
}

fn recipe_crafted(conditions: &Value, event: &Event<'_>, _: &EntityFacts) -> bool {
    let Event::RecipeCrafted {
        recipe,
        ingredients,
    } = event
    else {
        return false;
    };
    if !id_field(conditions, "recipes", recipe) {
        return false;
    }
    let mut remaining: Vec<&ItemStack> = ingredients.iter().collect();
    conditions
        .get("ingredients")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .all(|wanted| {
            match remaining
                .iter()
                .position(|stack| predicate::item(wanted, stack))
            {
                Some(found) => {
                    remaining.swap_remove(found);
                    true
                }
                None => false,
            }
        })
}

fn killed(
    conditions: &Value,
    entity: &EntityFacts,
    blow: &DamageFacts,
    player: &EntityFacts,
) -> bool {
    conditions
        .get("killing_blow")
        .is_none_or(|p| predicate::damage_source(p, blow, player.position))
        && entity_field(conditions, "entity", entity, player)
}

fn builtin(kind: TriggerType) -> Handler {
    match kind {
        TriggerType::Impossible => handler(|_, _, _| false),
        TriggerType::InventoryChanged => handler(inventory_changed),
        TriggerType::RecipeUnlocked => handler(
            |c, event, _| matches!(event, Event::RecipeUnlocked(recipe) if id_field(c, "recipes", recipe)),
        ),
        TriggerType::RecipeCrafted => handler(recipe_crafted),
        TriggerType::CrafterRecipeCrafted => handler(|c, event, player| {
            let Event::CrafterRecipeCrafted {
                recipe,
                ingredients,
            } = event
            else {
                return false;
            };
            recipe_crafted(
                c,
                &Event::RecipeCrafted {
                    recipe,
                    ingredients,
                },
                player,
            )
        }),
        TriggerType::ConsumeItem => handler(
            |c, event, _| matches!(event, Event::ConsumeItem(item) if item_field(c, "item", item)),
        ),
        TriggerType::PlacedBlock => handler(
            |c, event, player| matches!(event, Event::PlacedBlock { location, item } if location_field(c, location, Some(item), player)),
        ),
        TriggerType::ItemUsedOnBlock => handler(
            |c, event, player| matches!(event, Event::ItemUsedOnBlock { location, item } if location_field(c, location, Some(item), player)),
        ),
        TriggerType::AllayDropItemOnBlock => handler(
            |c, event, player| matches!(event, Event::AllayDropItemOnBlock { location, item } if location_field(c, location, Some(item), player)),
        ),
        TriggerType::AnyBlockUse => handler(
            |c, event, player| matches!(event, Event::AnyBlockUse { location, item } if location_field(c, location, Some(item), player)),
        ),
        TriggerType::DefaultBlockUse => handler(
            |c, event, player| matches!(event, Event::DefaultBlockUse { location } if location_field(c, location, None, player)),
        ),
        TriggerType::PlayerKilledEntity => handler(|c, event, player| {
            matches!(event, Event::PlayerKilledEntity { victim, killing_blow }
                if killed(c, victim, killing_blow, player))
        }),
        TriggerType::EntityKilledPlayer => handler(|c, event, player| {
            matches!(event, Event::EntityKilledPlayer { killer, killing_blow }
                if killed(c, killer, killing_blow, player))
        }),
        TriggerType::BredAnimals => handler(|c, event, player| {
            let Event::BredAnimals {
                parent,
                partner,
                child,
            } = event
            else {
                return false;
            };
            optional_entity_field(c, "child", *child, player)
                && ((entity_field(c, "parent", parent, player)
                    && entity_field(c, "partner", partner, player))
                    || (entity_field(c, "parent", partner, player)
                        && entity_field(c, "partner", parent, player)))
        }),
        TriggerType::ChangedDimension => handler(|c, event, _| {
            matches!(event, Event::ChangedDimension { from, to }
                if id_field(c, "from", from) && id_field(c, "to", to))
        }),
        TriggerType::EnterBlock => handler(|c, event, _| {
            let Event::EnterBlock(state) = event else {
                return false;
            };
            // The condition is a bare block predicate (`blocks`/`state`
            // fields), not wrapped under a "block" key.
            matches!(c, Value::Null) || predicate::block(c, *state)
        }),
        TriggerType::Location => handler(|_, event, _| matches!(event, Event::Location)),
        TriggerType::SleptInBed => handler(|_, event, _| matches!(event, Event::SleptInBed)),
        TriggerType::Tick => handler(|_, event, _| matches!(event, Event::Tick)),
        TriggerType::VoluntaryExile => handler(|_, event, _| matches!(event, Event::RaidOmen)),
        TriggerType::HeroOfTheVillage => handler(|_, event, _| matches!(event, Event::RaidWin)),
        TriggerType::EnchantedItem => handler(|c, event, _| {
            matches!(event, Event::EnchantedItem { item, levels }
                if item_field(c, "item", item)
                    && c.get("levels").is_none_or(|b| bounds(b, f64::from(*levels))))
        }),
        TriggerType::BrewedPotion => handler(|c, event, _| {
            let Event::BrewedPotion(potion) = event else {
                return false;
            };
            match (c.get("potion"), potion) {
                (None, _) => true,
                (Some(expected), Some(potion)) => in_set(expected, potion, |_| false),
                (Some(_), None) => false,
            }
        }),
        TriggerType::FilledBucket => handler(
            |c, event, _| matches!(event, Event::FilledBucket(item) if item_field(c, "item", item)),
        ),
        TriggerType::VillagerTrade => handler(|c, event, player| {
            matches!(event, Event::VillagerTrade { villager, item }
                if item_field(c, "item", item) && entity_field(c, "villager", villager, player))
        }),
        TriggerType::UsedEnderEye => handler(|c, event, _| {
            matches!(event, Event::UsedEnderEye { distance_sqr }
                if c.get("distance").is_none_or(|b| bounds_sqr(b, *distance_sqr)))
        }),
        TriggerType::CuredZombieVillager => handler(|c, event, player| {
            matches!(event, Event::CuredZombieVillager { zombie, villager }
                if entity_field(c, "zombie", zombie, player)
                    && entity_field(c, "villager", villager, player))
        }),
        TriggerType::ShotCrossbow => handler(
            |c, event, _| matches!(event, Event::ShotCrossbow(item) if item_field(c, "item", item)),
        ),
        TriggerType::TameAnimal => handler(
            |c, event, player| matches!(event, Event::TameAnimal(entity) if entity_field(c, "entity", entity, player)),
        ),
        TriggerType::SummonedEntity => handler(
            |c, event, player| matches!(event, Event::SummonedEntity(entity) if entity_field(c, "entity", entity, player)),
        ),
        TriggerType::UsedTotem => handler(
            |c, event, _| matches!(event, Event::UsedTotem(item) if item_field(c, "item", item)),
        ),
        TriggerType::ItemDurabilityChanged => handler(|c, event, _| {
            let Event::ItemDurabilityChanged { item, delta } = event else {
                return false;
            };
            let durability = item
                .max_damage()
                .map_or(0.0, |max| f64::from(max) - f64::from(item.damage()));
            item_field(c, "item", item)
                && c.get("durability").is_none_or(|b| bounds(b, durability))
                && c.get("delta").is_none_or(|b| bounds(b, f64::from(*delta)))
        }),
        TriggerType::EntityHurtPlayer => handler(|c, event, player| {
            matches!(event, Event::EntityHurtPlayer { damage, dealt, taken, blocked }
                if c.get("damage").is_none_or(|p| predicate::damage(p, damage, *dealt, *taken, *blocked, player.position)))
        }),
        TriggerType::PlayerHurtEntity => handler(|c, event, player| {
            matches!(event, Event::PlayerHurtEntity { entity, damage, dealt, taken, blocked }
                if c.get("damage").is_none_or(|p| predicate::damage(p, damage, *dealt, *taken, *blocked, player.position))
                    && entity_field(c, "entity", entity, player))
        }),
        TriggerType::PlayerInteractedWithEntity => handler(|c, event, player| {
            matches!(event, Event::PlayerInteractedWithEntity { item, entity }
                if c.get("item").is_none_or(|p| item.is_some_and(|stack| predicate::item(p, stack)))
                    && entity_field(c, "entity", entity, player))
        }),
        TriggerType::PlayerShearedEquipment => handler(|c, event, player| {
            matches!(event, Event::PlayerShearedEquipment { item, entity }
                if item_field(c, "item", item) && entity_field(c, "entity", entity, player))
        }),
        TriggerType::NetherTravel => handler(|c, event, player| {
            let Event::NetherTravel { entered } = event else {
                return false;
            };
            let start = LocationFacts {
                position: *entered,
                dimension: "minecraft:overworld".into(),
                ..LocationFacts::default()
            };
            c.get("start_position")
                .is_none_or(|p| predicate::location(p, &start))
                && c.get("distance")
                    .is_none_or(|p| predicate::distance(p, *entered, player.position))
        }),
        TriggerType::StartedRiding => handler(|_, event, _| matches!(event, Event::StartedRiding)),
        TriggerType::EffectsChanged => handler(|c, event, player| {
            let Event::EffectsChanged { source } = event else {
                return false;
            };
            c.get("effects")
                .is_none_or(|p| predicate::effects(p, &player.effects))
                && optional_entity_field(c, "source", *source, player)
        }),
        TriggerType::LightningStrike => handler(|c, event, player| {
            let Event::LightningStrike {
                lightning,
                bystanders,
            } = event
            else {
                return false;
            };
            entity_field(c, "lightning", lightning, player)
                && bystander_field(c, bystanders, player)
        }),
        TriggerType::ChanneledLightning => handler(|c, event, player| {
            matches!(event, Event::ChanneledLightning { victims }
                if victims_field(c, victims, player, false))
        }),
        TriggerType::KilledByArrow => handler(|c, event, player| {
            let Event::KilledByArrow { victims, weapon } = event else {
                return false;
            };
            if !victims_field(c, victims, player, true) {
                return false;
            }
            if let Some(expected) = c.get("fired_from_weapon")
                && !weapon.is_some_and(|w| predicate::item(expected, w))
            {
                return false;
            }
            c.get("unique_entity_types").is_none_or(|bounds| {
                let mut kinds = Vec::new();
                for victim in victims.iter() {
                    if !kinds.contains(&victim.kind) {
                        kinds.push(victim.kind);
                    }
                }
                predicate::bounds(bounds, kinds.len() as f64)
            })
        }),
        TriggerType::PlayerGeneratesContainerLoot => handler(
            |c, event, _| matches!(event, Event::PlayerGeneratesContainerLoot(table) if loot_tables(c, table)),
        ),
        TriggerType::Levitation => handler(|c, event, player| {
            matches!(event, Event::Levitation { start, duration }
                if c.get("distance").is_none_or(|p| predicate::distance(p, *start, player.position))
                    && c.get("duration").is_none_or(|b| bounds(b, f64::from(*duration))))
        }),
        TriggerType::FallFromHeight => handler(|c, event, player| {
            let Event::FallFromHeight { start } = event else {
                return false;
            };
            travelled(c, *start, player)
        }),
        TriggerType::UsingItem => handler(
            |c, event, _| matches!(event, Event::UsingItem(item) if item_field(c, "item", item)),
        ),
        TriggerType::ConstructBeacon => handler(|c, event, _| {
            matches!(event, Event::ConstructBeacon { level }
                if c.get("level").is_none_or(|b| bounds(b, f64::from(*level))))
        }),
        TriggerType::TargetHit => handler(|c, event, player| {
            matches!(event, Event::TargetHit { projectile, signal }
                if c.get("signal_strength").is_none_or(|b| bounds(b, f64::from(*signal)))
                    && entity_field(c, "projectile", projectile, player))
        }),
        TriggerType::KillMobNearSculkCatalyst => handler(|c, event, player| {
            matches!(event, Event::KillMobNearSculkCatalyst { victim, killing_blow }
                if killed(c, victim, killing_blow, player))
        }),
        TriggerType::AvoidVibration => {
            handler(|_, event, _| matches!(event, Event::AvoidVibration))
        }
        TriggerType::BeeNestDestroyed => handler(|c, event, _| {
            let Event::BeeNestDestroyed { block, item, bees } = event else {
                return false;
            };
            block_fields(c, *block)
                && c.get("item")
                    .is_none_or(|p| item.is_some_and(|stack| predicate::item(p, stack)))
                && c.get("num_bees_inside")
                    .is_none_or(|b| bounds(b, f64::from(*bees)))
        }),
        TriggerType::SlideDownBlock => handler(
            |c, event, _| matches!(event, Event::SlideDownBlock(state) if block_fields(c, *state)),
        ),
        TriggerType::FallAfterExplosion => handler(|c, event, player| {
            let Event::FallAfterExplosion { start, cause } = event else {
                return false;
            };
            travelled(c, *start, player) && optional_entity_field(c, "cause", *cause, player)
        }),
        TriggerType::RideEntityInLava => handler(
            |c, event, player| matches!(event, Event::RideEntityInLava { start } if travelled(c, *start, player)),
        ),
        TriggerType::ThrownItemPickedUpByEntity => handler(|c, event, player| {
            matches!(event, Event::ThrownItemPickedUpByEntity { item, entity }
                if picked_up(c, item, entity, player))
        }),
        TriggerType::ThrownItemPickedUpByPlayer => handler(|c, event, player| {
            matches!(event, Event::ThrownItemPickedUpByPlayer { item, entity }
                if picked_up(c, item, entity, player))
        }),
        TriggerType::SpearMobs => handler(|c, event, _| {
            matches!(event, Event::SpearMobs { count }
                if c.get("count").and_then(Value::as_u64).is_none_or(|n| u64::from(*count) >= n))
        }),
        TriggerType::FishingRodHooked => handler(|c, event, player| {
            let Event::FishingRodHooked { rod, entity, items } = event else {
                return false;
            };
            item_field(c, "rod", rod)
                && entity_field(c, "entity", entity, player)
                && c.get("item").is_none_or(|predicate| {
                    items.iter().any(|stack| predicate::item(predicate, stack))
                })
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tuff_vanilla_data::Item;

    fn player() -> EntityFacts {
        EntityFacts {
            kind: Some(tuff_vanilla_data::registry::EntityType::Player),
            dimension: "minecraft:overworld".into(),
            ..EntityFacts::default()
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn every_vanilla_trigger_type_is_registered() {
        let registry = builtin_registry();
        for kind in TriggerType::ALL {
            assert!(registry.get(kind.name()).is_some(), "{}", kind.name());
        }
        for advancement in &tuff_vanilla_data::advancement::ALL {
            for criterion in advancement.criteria {
                assert!(
                    registry.get(criterion.trigger).is_some(),
                    "{}",
                    criterion.trigger
                );
            }
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn feeding_tnt_to_a_grown_sulfur_cube_meets_uh_oh() {
        let advancement = tuff_vanilla_data::advancement::ALL
            .iter()
            .find(|a| a.name == "minecraft:husbandry/uh_oh")
            .expect("the advancement");
        let criterion = advancement
            .criteria
            .iter()
            .find(|c| c.name == "give_tnt_directly")
            .expect("the direct criterion");
        let conditions: Value = serde_json::from_str(criterion.conditions.unwrap()).unwrap();
        let registry = builtin_registry();
        let cube = |baby| EntityFacts {
            kind: Some(tuff_vanilla_data::registry::EntityType::SulfurCube),
            flags: predicate::Flags {
                baby,
                ..Default::default()
            },
            ..player()
        };
        let tnt = ItemStack::new(Item::Tnt, 1);
        let fed = |entity: &EntityFacts| {
            registry.matches(
                criterion.trigger,
                &conditions,
                &Event::PlayerInteractedWithEntity {
                    item: Some(&tnt),
                    entity,
                },
                &player(),
            )
        };
        assert!(fed(&cube(false)));
        assert!(!fed(&cube(true)), "a baby cube does not count");
    }

    #[tuff_core::watchdog]
    #[test]
    fn reeling_in_a_cod_meets_fishy_business() {
        let advancement = tuff_vanilla_data::advancement::ALL
            .iter()
            .find(|a| a.name == "minecraft:husbandry/fishy_business")
            .expect("the advancement");
        let criterion = advancement
            .criteria
            .iter()
            .find(|c| c.name == "cod")
            .expect("the cod criterion");
        let conditions: Value = serde_json::from_str(criterion.conditions.unwrap()).unwrap();
        let registry = builtin_registry();
        let rod = ItemStack::new(Item::FishingRod, 1);
        let hook = EntityFacts {
            kind: Some(tuff_vanilla_data::registry::EntityType::FishingBobber),
            ..player()
        };
        let reeled = |items: &[ItemStack]| {
            registry.matches(
                criterion.trigger,
                &conditions,
                &Event::FishingRodHooked {
                    rod: &rod,
                    entity: &hook,
                    items,
                },
                &player(),
            )
        };
        assert!(reeled(&[ItemStack::new(Item::Cod, 1)]));
        assert!(!reeled(&[ItemStack::new(Item::Salmon, 1)]));
        assert!(!reeled(&[]));
    }

    #[tuff_core::watchdog]
    #[test]
    fn shearing_wolf_armor_meets_remove_wolf_armor() {
        let advancement = tuff_vanilla_data::advancement::ALL
            .iter()
            .find(|a| a.name == "minecraft:husbandry/remove_wolf_armor")
            .expect("the advancement");
        let criterion = &advancement.criteria[0];
        let conditions: Value = criterion
            .conditions
            .map_or(Value::Null, |c| serde_json::from_str(c).unwrap());
        let registry = builtin_registry();
        let wolf = EntityFacts {
            kind: Some(tuff_vanilla_data::registry::EntityType::Wolf),
            ..player()
        };
        let armor = ItemStack::new(Item::WolfArmor, 1);
        let event = Event::PlayerShearedEquipment {
            item: &armor,
            entity: &wolf,
        };
        assert!(registry.matches(criterion.trigger, &conditions, &event, &player()));
        let saddle = ItemStack::new(Item::Saddle, 1);
        let other = Event::PlayerShearedEquipment {
            item: &saddle,
            entity: &wolf,
        };
        assert!(!registry.matches(criterion.trigger, &conditions, &other, &player()));
    }

    #[tuff_core::watchdog]
    #[test]
    fn inventory_changed_tests_the_changed_item_or_the_whole_inventory() {
        let registry = builtin_registry();
        let trigger = TriggerType::InventoryChanged.name();
        let iron = ItemStack::new(Item::IronIngot, 1);
        let inventory = vec![Some(iron.clone()), None];
        let event = Event::InventoryChanged {
            changed: Some(&iron),
            inventory: &inventory,
        };
        let one = json!({"items": [{"items": "minecraft:iron_ingot"}]});
        assert!(registry.matches(trigger, &one, &event, &player()));
        let two = json!({"items": [{"items": "minecraft:iron_ingot"}, {"items": "minecraft:gold_ingot"}]});
        assert!(!registry.matches(trigger, &two, &event, &player()));
        let slots = json!({"slots": {"occupied": {"min": 2}}});
        assert!(!registry.matches(trigger, &slots, &event, &player()));
        let nether = json!({"player": {"type": "minecraft:entity_properties", "entity": "this",
            "predicate": {"minecraft:location": {"dimension": "minecraft:the_nether"}}}});
        assert!(!registry.matches(trigger, &nether, &event, &player()));
    }

    #[tuff_core::watchdog]
    #[test]
    fn killed_by_arrow_needs_each_victim_and_the_weapon() {
        use tuff_vanilla_data::registry::EntityType;
        let registry = builtin_registry();
        let trigger = TriggerType::KilledByArrow.name();
        let of = |kind| EntityFacts {
            kind: Some(kind),
            ..EntityFacts::default()
        };
        let phantom = json!({"type": "minecraft:entity_properties", "entity": "this",
            "predicate": {"minecraft:entity_type": "minecraft:phantom"}});
        let two = json!({"victims": [phantom.clone(), phantom.clone()],
            "fired_from_weapon": {"items": "minecraft:crossbow"}});
        let crossbow = ItemStack::new(Item::Crossbow, 1);
        let bow = ItemStack::new(Item::Bow, 1);
        let both = [of(EntityType::Phantom), of(EntityType::Phantom)];
        let event =
            |victims: &'static [EntityFacts], weapon| Event::KilledByArrow { victims, weapon };
        let both: &'static [EntityFacts] = Box::leak(Box::new(both));
        assert!(registry.matches(trigger, &two, &event(both, Some(&crossbow)), &player()));
        assert!(!registry.matches(trigger, &two, &event(both, Some(&bow)), &player()));
        assert!(!registry.matches(
            trigger,
            &two,
            &event(&both[..1], Some(&crossbow)),
            &player()
        ));
        let unique = json!({"unique_entity_types": 2});
        assert!(!registry.matches(trigger, &unique, &event(both, None), &player()));
        let mixed: &'static [EntityFacts] =
            Box::leak(Box::new([of(EntityType::Phantom), of(EntityType::Zombie)]));
        assert!(registry.matches(trigger, &unique, &event(mixed, None), &player()));
    }

    #[tuff_core::watchdog]
    #[test]
    fn channeled_lightning_lets_one_victim_satisfy_several_predicates() {
        use tuff_vanilla_data::registry::EntityType;
        let registry = builtin_registry();
        let villager = json!({"type": "minecraft:entity_properties", "entity": "this",
            "predicate": {"minecraft:entity_type": "minecraft:villager"}});
        let conditions = json!({"victims": [villager.clone(), villager]});
        let one = vec![EntityFacts {
            kind: Some(EntityType::Villager),
            ..EntityFacts::default()
        }];
        assert!(registry.matches(
            TriggerType::ChanneledLightning.name(),
            &conditions,
            &Event::ChanneledLightning { victims: &one },
            &player()
        ));
    }

    #[tuff_core::watchdog]
    #[test]
    fn channeled_lightning_matches_a_struck_victim() {
        use tuff_vanilla_data::registry::EntityType;
        let registry = builtin_registry();
        let trigger = TriggerType::ChanneledLightning.name();
        let villager = json!({"victims": [{"type": "minecraft:entity_properties", "entity": "this",
            "predicate": {"minecraft:entity_type": "minecraft:villager"}}]});
        let of = |kind| {
            vec![EntityFacts {
                kind: Some(kind),
                ..EntityFacts::default()
            }]
        };
        let struck = of(EntityType::Villager);
        let missed = of(EntityType::Cow);
        assert!(registry.matches(
            trigger,
            &villager,
            &Event::ChanneledLightning { victims: &struck },
            &player()
        ));
        assert!(!registry.matches(
            trigger,
            &villager,
            &Event::ChanneledLightning { victims: &missed },
            &player()
        ));
    }

    #[tuff_core::watchdog]
    #[test]
    fn generated_container_loot_matches_its_table() {
        let registry = builtin_registry();
        let trigger = TriggerType::PlayerGeneratesContainerLoot.name();
        let event = Event::PlayerGeneratesContainerLoot("minecraft:chests/simple_dungeon");
        let table = |id| json!({"loot_tables": id});
        assert!(registry.matches(
            trigger,
            &table("minecraft:chests/simple_dungeon"),
            &event,
            &player()
        ));
        assert!(!registry.matches(
            trigger,
            &table("minecraft:chests/igloo_chest"),
            &event,
            &player()
        ));
        assert!(registry.matches(
            trigger,
            &json!({"loot_tables": ["minecraft:chests/igloo_chest", "chests/simple_dungeon"]}),
            &event,
            &player()
        ));
        assert!(!registry.matches(trigger, &json!({}), &event, &player()));
        assert!(!registry.matches(
            trigger,
            &json!({"loot_table": "minecraft:chests/igloo_chest"}),
            &event,
            &player()
        ));
    }

    #[tuff_core::watchdog]
    #[test]
    fn block_uses_test_the_block_after_the_use_and_the_held_item() {
        let registry = builtin_registry();
        let lever = LocationFacts {
            block: "minecraft:lever[powered=true]".parse().ok(),
            ..LocationFacts::default()
        };
        let powered = json!({"location": {"type": "minecraft:block_state_property",
            "block": "minecraft:lever", "properties": {"powered": "true"}}});
        let unpowered = json!({"location": {"type": "minecraft:block_state_property",
            "block": "minecraft:lever", "properties": {"powered": "false"}}});
        let default = TriggerType::DefaultBlockUse.name();
        let event = Event::DefaultBlockUse { location: &lever };
        assert!(registry.matches(default, &powered, &event, &player()));
        assert!(!registry.matches(default, &unpowered, &event, &player()));
        let stick = ItemStack::new(Item::Stick, 1);
        let holding = json!({"location": {"type": "minecraft:match_tool",
            "predicate": {"items": "minecraft:stick"}}});
        let any = TriggerType::AnyBlockUse.name();
        let event = Event::AnyBlockUse {
            location: &lever,
            item: &stick,
        };
        assert!(registry.matches(any, &holding, &event, &player()));
        assert!(!registry.matches(default, &holding, &event, &player()));
    }

    #[tuff_core::watchdog]
    #[test]
    fn region_reports_test_their_numbers_and_blocks() {
        use tuff_vanilla_data::registry::EntityType;
        let registry = builtin_registry();
        let beacon = TriggerType::ConstructBeacon.name();
        let full = json!({"level": 4});
        assert!(registry.matches(
            beacon,
            &full,
            &Event::ConstructBeacon { level: 4 },
            &player()
        ));
        assert!(!registry.matches(
            beacon,
            &full,
            &Event::ConstructBeacon { level: 3 },
            &player()
        ));

        let far = EntityFacts {
            kind: Some(EntityType::Arrow),
            position: [40.0, 0.0, 0.0],
            ..EntityFacts::default()
        };
        let near = EntityFacts {
            position: [10.0, 0.0, 0.0],
            ..far.clone()
        };
        let bullseye = json!({"signal_strength": 15, "projectile": {"type": "minecraft:entity_properties",
            "entity": "this", "predicate": {"minecraft:distance": {"horizontal": {"min": 30.0}}}}});
        let target = TriggerType::TargetHit.name();
        let hit = |projectile, signal| Event::TargetHit { projectile, signal };
        assert!(registry.matches(target, &bullseye, &hit(&far, 15), &player()));
        assert!(!registry.matches(target, &bullseye, &hit(&far, 14), &player()));
        assert!(!registry.matches(target, &bullseye, &hit(&near, 15), &player()));

        let nest = TriggerType::BeeNestDestroyed.name();
        let silk = json!({"blocks": "minecraft:bee_nest", "num_bees_inside": 3,
            "item": {"items": "minecraft:shears"}});
        let shears = ItemStack::new(Item::Shears, 1);
        let destroyed = |block: tuff_vanilla_data::Block, bees| Event::BeeNestDestroyed {
            block: block.default_state(),
            item: Some(&shears),
            bees,
        };
        assert!(registry.matches(
            nest,
            &silk,
            &destroyed(tuff_vanilla_data::Block::BeeNest, 3),
            &player()
        ));
        assert!(!registry.matches(
            nest,
            &silk,
            &destroyed(tuff_vanilla_data::Block::BeeNest, 2),
            &player()
        ));
        assert!(!registry.matches(
            nest,
            &silk,
            &destroyed(tuff_vanilla_data::Block::Beehive, 3),
            &player()
        ));

        let slide = TriggerType::SlideDownBlock.name();
        let honey = json!({"blocks": "minecraft:honey_block"});
        let on = |block: tuff_vanilla_data::Block| Event::SlideDownBlock(block.default_state());
        assert!(registry.matches(
            slide,
            &honey,
            &on(tuff_vanilla_data::Block::HoneyBlock),
            &player()
        ));
        assert!(!registry.matches(
            slide,
            &honey,
            &on(tuff_vanilla_data::Block::SlimeBlock),
            &player()
        ));

        let crafter = TriggerType::CrafterRecipeCrafted.name();
        let recipe = json!({"recipes": "minecraft:crafter"});
        let crafted = |recipe| Event::CrafterRecipeCrafted {
            recipe,
            ingredients: &[],
        };
        assert!(registry.matches(crafter, &recipe, &crafted("minecraft:crafter"), &player()));
        assert!(!registry.matches(crafter, &recipe, &crafted("minecraft:dropper"), &player()));
        assert!(
            !registry.matches(
                TriggerType::RecipeCrafted.name(),
                &recipe,
                &crafted("minecraft:crafter"),
                &player()
            ),
            "a crafter's craft is not the player's"
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn distance_triggers_measure_from_where_they_started() {
        use tuff_vanilla_data::registry::EntityType;
        let registry = builtin_registry();
        let mut high = player();
        high.position = [0.0, 108.0, 0.0];
        let wind = EntityFacts {
            kind: Some(EntityType::WindCharge),
            ..EntityFacts::default()
        };
        let breeze = EntityFacts {
            kind: Some(EntityType::BreezeWindCharge),
            ..EntityFacts::default()
        };
        let rockets = json!({"cause": {"type": "minecraft:entity_properties", "entity": "this",
            "predicate": {"minecraft:entity_type": "minecraft:wind_charge"}}, "distance": {"y": {"min": 7.0}}});
        let fall = TriggerType::FallAfterExplosion.name();
        let thrown = |cause| Event::FallAfterExplosion {
            start: [0.0, 100.0, 0.0],
            cause,
        };
        assert!(registry.matches(fall, &rockets, &thrown(Some(&wind)), &high));
        assert!(!registry.matches(fall, &rockets, &thrown(Some(&breeze)), &high));
        assert!(!registry.matches(fall, &rockets, &thrown(None), &high));
        let mut low = player();
        low.position = [0.0, 103.0, 0.0];
        assert!(!registry.matches(fall, &rockets, &thrown(Some(&wind)), &low));

        let lava = TriggerType::RideEntityInLava.name();
        let far = json!({"distance": {"horizontal": {"min": 50.0}}});
        let mut moved = player();
        moved.position = [36.0, 0.0, 36.0];
        let ride = Event::RideEntityInLava {
            start: [0.0, 0.0, 0.0],
        };
        assert!(registry.matches(lava, &far, &ride, &moved));
        assert!(!registry.matches(lava, &far, &ride, &player()));
    }

    #[tuff_core::watchdog]
    #[test]
    fn allay_drop_tests_the_item_and_location_and_not_item_use() {
        let registry = builtin_registry();
        let gold = ItemStack::new(Item::GoldIngot, 1);
        let location = LocationFacts::default();
        let name = TriggerType::AllayDropItemOnBlock.name();
        let conditions = json!({"item": {"items": "minecraft:gold_ingot"}});
        let dropped = Event::AllayDropItemOnBlock {
            location: &location,
            item: &gold,
        };
        assert!(registry.matches(name, &json!({}), &dropped, &player()));
        let used = Event::ItemUsedOnBlock {
            location: &location,
            item: &gold,
        };
        assert!(!registry.matches(name, &conditions, &used, &player()));
    }

    #[tuff_core::watchdog]
    #[test]
    fn picked_up_and_sheared_items_test_the_item_and_the_other_entity() {
        use tuff_vanilla_data::registry::EntityType;
        let registry = builtin_registry();
        let piglin = EntityFacts {
            kind: Some(EntityType::Piglin),
            ..EntityFacts::default()
        };
        let gold = ItemStack::new(Item::GoldIngot, 1);
        let iron = ItemStack::new(Item::IronIngot, 1);
        let distract = json!({"item": {"items": "minecraft:gold_ingot"}, "entity": {"type":
            "minecraft:entity_properties", "entity": "this", "predicate": {"minecraft:entity_type": "minecraft:piglin"}}});
        let by_entity = TriggerType::ThrownItemPickedUpByEntity.name();
        let picked = |item| Event::ThrownItemPickedUpByEntity {
            item,
            entity: &piglin,
        };
        assert!(registry.matches(by_entity, &distract, &picked(&gold), &player()));
        assert!(!registry.matches(by_entity, &distract, &picked(&iron), &player()));
        assert!(!registry.matches(
            TriggerType::ThrownItemPickedUpByPlayer.name(),
            &distract,
            &picked(&gold),
            &player()
        ));
        let sheared = TriggerType::PlayerShearedEquipment.name();
        let event = Event::PlayerShearedEquipment {
            item: &gold,
            entity: &piglin,
        };
        assert!(registry.matches(sheared, &distract, &event, &player()));
    }

    #[tuff_core::watchdog]
    #[test]
    fn bred_animals_accepts_either_parent_order() {
        let registry = builtin_registry();
        let kind = |kind| EntityFacts {
            kind: Some(kind),
            ..EntityFacts::default()
        };
        use tuff_vanilla_data::registry::EntityType;
        let (cow, pig) = (kind(EntityType::Cow), kind(EntityType::Pig));
        let conditions = json!({"parent": {"type": "minecraft:entity_properties", "entity": "this",
            "predicate": {"minecraft:entity_type": "minecraft:pig"}}});
        let event = Event::BredAnimals {
            parent: &cow,
            partner: &pig,
            child: None,
        };
        assert!(registry.matches(
            TriggerType::BredAnimals.name(),
            &conditions,
            &event,
            &player()
        ));
        let child = json!({"child": {"type": "minecraft:entity_properties", "entity": "this",
            "predicate": {"minecraft:entity_type": "minecraft:pig"}}});
        assert!(!registry.matches(TriggerType::BredAnimals.name(), &child, &event, &player()));
    }

    #[tuff_core::watchdog]
    #[test]
    fn spear_mobs_needs_the_stabbed_count() {
        let registry = builtin_registry();
        let at_least_two = json!({"count": 2});
        let fires = |count| {
            registry.matches(
                TriggerType::SpearMobs.name(),
                &at_least_two,
                &Event::SpearMobs { count },
                &player(),
            )
        };
        assert!(!fires(1));
        assert!(fires(2));
        assert!(fires(5));
    }

    fn read_keys(kind: TriggerType) -> &'static [&'static str] {
        const PICKED_UP: &[&str] = &["item", "entity"];
        const TRAVELLED: &[&str] = &["start_position", "distance"];
        const KILLED: &[&str] = &["killing_blow", "entity"];
        match kind {
            TriggerType::Impossible
            | TriggerType::Location
            | TriggerType::SleptInBed
            | TriggerType::Tick
            | TriggerType::VoluntaryExile
            | TriggerType::HeroOfTheVillage
            | TriggerType::StartedRiding
            | TriggerType::AvoidVibration => &[],
            TriggerType::SpearMobs => &["count"],
            TriggerType::InventoryChanged => &["slots", "items"],
            TriggerType::RecipeUnlocked => &["recipes"],
            TriggerType::RecipeCrafted | TriggerType::CrafterRecipeCrafted => {
                &["recipes", "ingredients"]
            }
            TriggerType::FishingRodHooked => &["rod", "entity", "item"],
            TriggerType::ConsumeItem
            | TriggerType::FilledBucket
            | TriggerType::ShotCrossbow
            | TriggerType::UsedTotem
            | TriggerType::UsingItem => &["item"],
            TriggerType::PlacedBlock
            | TriggerType::ItemUsedOnBlock
            | TriggerType::AllayDropItemOnBlock
            | TriggerType::AnyBlockUse
            | TriggerType::DefaultBlockUse => &["location"],
            TriggerType::PlayerKilledEntity
            | TriggerType::EntityKilledPlayer
            | TriggerType::KillMobNearSculkCatalyst => KILLED,
            TriggerType::BredAnimals => &["child", "parent", "partner"],
            TriggerType::ChangedDimension => &["from", "to"],
            TriggerType::EnterBlock | TriggerType::SlideDownBlock => &["blocks", "state"],
            TriggerType::EnchantedItem => &["item", "levels"],
            TriggerType::BrewedPotion => &["potion"],
            TriggerType::VillagerTrade => &["item", "villager"],
            TriggerType::UsedEnderEye => &["distance"],
            TriggerType::CuredZombieVillager => &["zombie", "villager"],
            TriggerType::TameAnimal | TriggerType::SummonedEntity => &["entity"],
            TriggerType::ItemDurabilityChanged => &["item", "durability", "delta"],
            TriggerType::EntityHurtPlayer => &["damage"],
            TriggerType::PlayerHurtEntity => &["damage", "entity"],
            TriggerType::PlayerInteractedWithEntity | TriggerType::PlayerShearedEquipment => {
                &["item", "entity"]
            }
            TriggerType::NetherTravel
            | TriggerType::FallFromHeight
            | TriggerType::RideEntityInLava => TRAVELLED,
            TriggerType::EffectsChanged => &["effects", "source"],
            TriggerType::LightningStrike => &["lightning", "bystander"],
            TriggerType::ChanneledLightning => &["victims"],
            TriggerType::KilledByArrow => &["victims", "fired_from_weapon", "unique_entity_types"],
            TriggerType::PlayerGeneratesContainerLoot => &["loot_tables"],
            TriggerType::Levitation => &["distance", "duration"],
            TriggerType::ConstructBeacon => &["level"],
            TriggerType::TargetHit => &["signal_strength", "projectile"],
            TriggerType::BeeNestDestroyed => &["blocks", "state", "item", "num_bees_inside"],
            TriggerType::FallAfterExplosion => &["start_position", "distance", "cause"],
            TriggerType::ThrownItemPickedUpByEntity | TriggerType::ThrownItemPickedUpByPlayer => {
                PICKED_UP
            }
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn every_jar_condition_key_is_read_by_its_handler() {
        let mut unread = std::collections::BTreeSet::new();
        for advancement in &tuff_vanilla_data::advancement::ALL {
            for criterion in advancement.criteria {
                let Some(kind) = TriggerType::ALL
                    .iter()
                    .find(|k| k.name() == criterion.trigger)
                else {
                    panic!("unknown trigger {}", criterion.trigger);
                };
                if NEVER_FIRED.contains(kind) {
                    continue;
                }
                let Some(raw) = criterion.conditions else {
                    continue;
                };
                let conditions: Value = serde_json::from_str(raw).unwrap();
                let Some(map) = conditions.as_object() else {
                    continue;
                };
                for key in map.keys() {
                    if key != "player" && !read_keys(*kind).contains(&key.as_str()) {
                        unread.insert(format!("{}.{key}", criterion.trigger));
                    }
                }
            }
        }
        assert!(
            unread.is_empty(),
            "condition keys no handler reads: {unread:?}"
        );
    }
}
