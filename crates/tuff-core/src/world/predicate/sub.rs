//! Entity and item sub-predicates that read slots, movement, live player
//! state, type-specific saved data and item components.

use serde_json::Value;
use tuff_vanilla_data::item::EquipmentSlot;
use tuff_vanilla_data::registry::{EntityType, StatType};

use super::{EntityFacts, bounds, bounds_sqr, flag, in_set, in_tag, item, nbt_matches, qualified};
use crate::world::stats::Stat;
use tuff_item::item::{ItemPicture, ItemSnapshot, ItemStack, Mode};
use tuff_nbt::{Compound, Tag};

fn equipment_slot_id(slot: EquipmentSlot) -> i32 {
    match slot {
        EquipmentSlot::Mainhand => 98,
        EquipmentSlot::Offhand => 99,
        EquipmentSlot::Feet => 100,
        EquipmentSlot::Legs => 101,
        EquipmentSlot::Chest => 102,
        EquipmentSlot::Head => 103,
        EquipmentSlot::Body => 105,
        EquipmentSlot::Saddle => 106,
    }
}

/// The slot ids a slot range name covers, as the game numbers them.
pub fn slot_range(name: &str) -> Option<Vec<i32>> {
    let range = |prefix: &str, offset: i32, size: i32| -> Option<Vec<i32>> {
        let rest = name.strip_prefix(prefix)?;
        if rest == "*" {
            return Some((offset..offset + size).collect());
        }
        let index: i32 = rest.parse().ok().filter(|i| (0..size).contains(i))?;
        (rest == index.to_string()).then(|| vec![offset + index])
    };
    Some(match name {
        "contents" => vec![0],
        "weapon" | "weapon.mainhand" => vec![98],
        "weapon.offhand" => vec![99],
        "weapon.*" => vec![98, 99],
        "armor.head" => vec![103],
        "armor.chest" => vec![102],
        "armor.legs" => vec![101],
        "armor.feet" => vec![100],
        "armor.body" => vec![105],
        "armor.*" => vec![103, 102, 101, 100, 105],
        "saddle" => vec![106],
        "horse.chest" | "player.cursor" => vec![499],
        _ => {
            return range("container.", 0, 54)
                .or_else(|| range("hotbar.", 0, 9))
                .or_else(|| range("inventory.", 9, 27))
                .or_else(|| range("enderchest.", 200, 27))
                .or_else(|| range("mob.inventory.", 300, 8))
                .or_else(|| range("horse.", 500, 15))
                .or_else(|| range("player.crafting.", 500, 4));
        }
    })
}

/// The stacks an entity holds by slot id: its equipment, and from its
/// saved form its inventory, ender chest or displayed item.
fn slot_items(entity: &EntityFacts) -> Vec<(i32, tuff_item::item::ItemSnapshot)> {
    let mut items: Vec<(i32, tuff_item::item::ItemSnapshot)> = entity
        .equipment
        .iter()
        .map(|(slot, stack)| (equipment_slot_id(*slot), stack.clone()))
        .collect();
    let template = |nbt: &Compound| ItemPicture::from_nbt(nbt).map(ItemSnapshot::from);
    let Some(saved) = &entity.nbt else {
        return items;
    };
    let stacks = |key: &str| {
        saved
            .get_list(key)
            .unwrap_or(&[])
            .iter()
            .filter_map(Tag::as_compound)
            .collect::<Vec<&Compound>>()
    };
    let player = entity.kind == Some(EntityType::Player);
    for (index, stack) in stacks("Inventory").into_iter().enumerate() {
        let slot = match (player, stack.get_byte("Slot")) {
            (true, Some(slot)) if (0..36).contains(&slot) => i32::from(slot),
            (true, _) => continue,
            (false, _) => 300 + index as i32,
        };
        items.extend(template(stack).map(|item| (slot, item)));
    }
    for stack in stacks("EnderItems") {
        if let Some(slot) = stack.get_byte("Slot") {
            items.extend(template(stack).map(|item| (200 + i32::from(slot), item)));
        }
    }
    if let Some(item) = saved.get_compound("Item").and_then(template) {
        items.push((0, item));
    }
    items
}

/// `slots`: every named range holds a stack matching its item predicate.
pub(super) fn slots(predicate: &Value, entity: &EntityFacts) -> bool {
    let Some(ranges) = predicate.as_object() else {
        return false;
    };
    let items = slot_items(entity);
    ranges.iter().all(|(name, test)| {
        slot_range(name).is_some_and(|ids| {
            items
                .iter()
                .any(|(slot, stack)| ids.contains(slot) && item(test, stack))
        })
    })
}

/// `movement`: velocity per second, speeds and fall distance.
pub(super) fn movement(predicate: &Value, entity: &EntityFacts) -> bool {
    let (Some(fields), Some(movement)) = (predicate.as_object(), entity.movement) else {
        return false;
    };
    let [x, y, z] = movement.map(|v| v * 20.0);
    fields.iter().all(|(key, bound)| match key.as_str() {
        "x" => bounds(bound, x),
        "y" => bounds(bound, y),
        "z" => bounds(bound, z),
        "speed" => bounds_sqr(bound, x * x + y * y + z * z),
        "horizontal_speed" => bounds_sqr(bound, x * x + z * z),
        "vertical_speed" => bounds(bound, y.abs()),
        "fall_distance" => entity
            .fall_distance
            .is_some_and(|distance| bounds(bound, distance)),
        _ => false,
    })
}

pub(super) fn periodic_tick(predicate: &Value, entity: &EntityFacts) -> bool {
    let period = predicate.as_i64().filter(|p| *p > 0);
    period
        .zip(entity.tick_count)
        .is_some_and(|(period, ticks)| ticks % period == 0)
}

fn saved_flag(entity: &EntityFacts, key: &str) -> Option<bool> {
    entity
        .nbt
        .as_ref()
        .map(|saved| saved.get_bool(key).unwrap_or(false))
}

/// `type_specific/...` predicates other than the player's, read from the
/// entity's saved form.
pub(super) fn type_specific(kind: &str, predicate: &Value, entity: &EntityFacts) -> bool {
    let fields = predicate.as_object().cloned().unwrap_or_default();
    let is = |kinds: &[EntityType]| entity.kind.is_some_and(|kind| kinds.contains(&kind));
    match kind {
        "sheep" => {
            is(&[EntityType::Sheep])
                && fields.iter().all(|(key, value)| {
                    key == "sheared" && flag(value) == saved_flag(entity, "Sheared")
                })
        }
        "cube_mob" => {
            is(&[EntityType::Slime, EntityType::MagmaCube])
                && fields.iter().all(|(key, value)| {
                    let size = entity
                        .nbt
                        .as_ref()
                        .map(|saved| saved.get_int("Size").unwrap_or(0) + 1);
                    key == "size" && size.is_some_and(|size| bounds(value, f64::from(size)))
                })
        }
        "raider" => {
            let raider = is(&[
                EntityType::Evoker,
                EntityType::Illusioner,
                EntityType::Pillager,
                EntityType::Ravager,
                EntityType::Vindicator,
                EntityType::Witch,
            ]);
            let Some(saved) = entity.nbt.as_ref().filter(|_| raider) else {
                return false;
            };
            let has_raid = saved.contains_key("RaidId");
            let captain = saved.get_bool("PatrolLeader").unwrap_or(false)
                && entity
                    .equipment
                    .iter()
                    .any(|(slot, stack)| *slot == EquipmentSlot::Head && is_ominous_banner(stack));
            let expect = |key: &str| fields.get(key).and_then(flag).unwrap_or(false);
            fields
                .keys()
                .all(|key| key == "has_raid" || key == "is_captain")
                && expect("has_raid") == has_raid
                && expect("is_captain") == captain
        }
        "fishing_hook" => {
            is(&[EntityType::FishingBobber])
                && fields.iter().all(|(key, value)| {
                    key == "in_open_water"
                        && flag(value).is_some()
                        && flag(value) == entity.open_water
                })
        }
        "lightning" => {
            is(&[EntityType::LightningBolt])
                && fields.iter().all(|(key, value)| match key.as_str() {
                    "blocks_set_on_fire" => entity
                        .blocks_set_on_fire
                        .is_some_and(|count| bounds(value, f64::from(count))),
                    "entity_struck" => entity
                        .entity_struck
                        .iter()
                        .any(|struck| super::entity(value, struck, entity.position)),
                    _ => false,
                })
        }
        _ => false,
    }
}

fn is_ominous_banner<M: Mode>(stack: &ItemStack<M>) -> bool {
    stack.item() == tuff_vanilla_data::Item::WhiteBanner
        && stack
            .components
            .to_nbt()
            .get("minecraft:item_name")
            .is_some_and(|name| mentions(name, "block.minecraft.ominous_banner"))
}

fn mentions(tag: &Tag, text: &str) -> bool {
    match tag {
        Tag::String(value) => value == text,
        Tag::Compound(compound) => compound.values().any(|tag| mentions(tag, text)),
        Tag::List(list) => list.iter().any(|tag| mentions(tag, text)),
        _ => false,
    }
}

/// The player fields `food`, `recipes`, `stats` and `input`; `None` for
/// any other field.
pub(super) fn player_field(field: &str, expected: &Value, entity: &EntityFacts) -> Option<bool> {
    let saved = entity.nbt.as_ref();
    let player = entity.player.as_ref();
    Some(match field {
        "food" => {
            let food = player.and_then(|player| player.food).or_else(|| {
                saved.map(|saved| {
                    (
                        saved.get_int("foodLevel").unwrap_or(20),
                        saved.get_float("foodSaturationLevel").unwrap_or(5.0),
                    )
                })
            });
            food.is_some_and(|(level, saturation)| {
                expected.as_object().is_some_and(|fields| {
                    fields.iter().all(|(key, bound)| match key.as_str() {
                        "level" => bounds(bound, f64::from(level)),
                        "saturation" => bounds(bound, f64::from(saturation)),
                        _ => false,
                    })
                })
            })
        }
        "recipes" => saved.is_some_and(|saved| {
            let known: Vec<&str> = saved
                .get_compound("recipeBook")
                .and_then(|book| book.get_list("recipes"))
                .unwrap_or(&[])
                .iter()
                .filter_map(Tag::as_str)
                .collect();
            expected.as_object().is_some_and(|recipes| {
                recipes.iter().all(|(id, wanted)| {
                    let id = qualified(id);
                    flag(wanted) == Some(known.contains(&id.as_str()))
                })
            })
        }),
        "stats" => player
            .and_then(|player| player.stats.as_ref())
            .is_some_and(|stats| {
                expected.as_array().is_some_and(|matchers| {
                    matchers.iter().all(|matcher| {
                        let stat = matcher["type"]
                            .as_str()
                            .and_then(|kind| StatType::from_name(&qualified(kind)))
                            .zip(matcher["stat"].as_str())
                            .and_then(|(kind, entry)| Stat::from_names(kind, &qualified(entry)));
                        stat.is_some_and(|stat| {
                            matcher
                                .get("value")
                                .is_none_or(|bound| bounds(bound, f64::from(stats.get(stat))))
                        })
                    })
                })
            }),
        "input" => player.and_then(|player| player.input).is_some_and(|input| {
            expected.as_object().is_some_and(|keys| {
                keys.iter().all(|(key, wanted)| {
                    let bit = match key.as_str() {
                        "forward" => 0x01,
                        "backward" => 0x02,
                        "left" => 0x04,
                        "right" => 0x08,
                        "jump" => 0x10,
                        "sneak" => 0x20,
                        "sprint" => 0x40,
                        _ => return false,
                    };
                    flag(wanted) == Some(input & bit != 0)
                })
            })
        }),
        _ => return None,
    })
}

/// A collection predicate (`contains`, `count`, `size`) over `elements`,
/// each tested with `test`.
fn collection_of<T>(predicate: &Value, elements: &[T], test: impl Fn(&Value, &T) -> bool) -> bool {
    let Some(fields) = predicate.as_object() else {
        return false;
    };
    fields.iter().all(|(key, value)| match key.as_str() {
        "contains" => value.as_array().is_some_and(|tests| {
            tests
                .iter()
                .all(|wanted| elements.iter().any(|element| test(wanted, element)))
        }),
        "count" => value.as_array().is_some_and(|entries| {
            entries.iter().all(|entry| {
                let matching = elements
                    .iter()
                    .filter(|element| test(&entry["test"], element))
                    .count();
                bounds(&entry["count"], matching as f64)
            })
        }),
        "size" => bounds(value, elements.len() as f64),
        _ => false,
    })
}

/// A collection predicate over stacks.
fn collection<M: Mode>(predicate: &Value, stacks: &[&ItemStack<M>]) -> bool {
    collection_of(predicate, stacks, |test, stack| item(test, *stack))
}

fn list_of(tag: Option<&Tag>) -> Vec<&Tag> {
    match tag {
        Some(Tag::List(items)) => items.iter().collect(),
        _ => Vec::new(),
    }
}

fn flag_tag(tag: Option<&Tag>) -> Option<bool> {
    match tag? {
        Tag::Byte(v) => Some(*v != 0),
        Tag::Int(v) => Some(*v != 0),
        _ => None,
    }
}

fn int_tag(tag: Option<&Tag>) -> Option<f64> {
    match tag? {
        Tag::Byte(v) => Some(f64::from(*v)),
        Tag::Short(v) => Some(f64::from(*v)),
        Tag::Int(v) => Some(f64::from(*v)),
        Tag::Long(v) => Some(*v as f64),
        Tag::Float(v) => Some(f64::from(*v)),
        Tag::Double(v) => Some(*v),
        _ => None,
    }
}

fn firework_explosion(predicate: &Value, tag: &Tag) -> bool {
    let (Some(fields), Some(explosion)) = (predicate.as_object(), tag.as_compound()) else {
        return false;
    };
    fields.iter().all(|(key, wanted)| match key.as_str() {
        "shape" => wanted
            .as_str()
            .is_some_and(|shape| explosion.get_str("shape").unwrap_or("small_ball") == shape),
        "has_twinkle" => {
            flag(wanted) == Some(flag_tag(explosion.get("has_twinkle")).unwrap_or(false))
        }
        "has_trail" => flag(wanted) == Some(flag_tag(explosion.get("has_trail")).unwrap_or(false)),
        _ => false,
    })
}

fn page_text(tag: &Tag) -> Option<&str> {
    match tag {
        Tag::String(text) => Some(text),
        Tag::Compound(page) => page.get_str("raw"),
        _ => None,
    }
}

fn page_matches(wanted: &Value, tag: &Tag) -> bool {
    let wanted = match wanted {
        Value::String(text) => text.as_str(),
        Value::Object(map) => map.get("text").and_then(Value::as_str).unwrap_or_default(),
        _ => return false,
    };
    page_text(tag).is_some_and(|text| text == wanted)
}

fn attribute_entry(predicate: &Value, tag: &Tag) -> bool {
    let (Some(fields), Some(entry)) = (predicate.as_object(), tag.as_compound()) else {
        return false;
    };
    fields.iter().all(|(key, wanted)| match key.as_str() {
        "attribute" => in_set(wanted, entry.get_str("type").unwrap_or_default(), |tag| {
            in_tag(
                "minecraft:attribute",
                tag,
                entry.get_str("type").unwrap_or_default(),
            )
        }),
        "id" => wanted.as_str().is_some_and(|id| {
            entry
                .get_str("id")
                .is_some_and(|actual| qualified(id) == qualified(actual))
        }),
        "amount" => int_tag(entry.get("amount")).is_some_and(|amount| bounds(wanted, amount)),
        "operation" => wanted.as_str() == entry.get_str("operation"),
        "slot" => wanted
            .as_str()
            .is_some_and(|slot| entry.get_str("slot").unwrap_or("any") == slot),
        _ => false,
    })
}

/// The value of component `id` on `stack`, its own or else the item's
/// fixed default.
fn component_tag<M: Mode>(stack: &ItemStack<M>, id: &str) -> Option<Tag> {
    stack.components.to_nbt().get(id).cloned().or_else(|| {
        (!stack.components.is_removed(id))
            .then(|| super::default_component(stack, id))
            .flatten()
    })
}

fn mob_effect_instance(
    predicate: &Value,
    effect: &str,
    amplifier: i64,
    duration: i64,
    ambient: bool,
    visible: bool,
) -> bool {
    predicate.as_object().is_some_and(|effects| {
        effects.iter().all(|(id, wanted)| {
            qualified(id) == effect
                && wanted.as_object().is_some_and(|fields| {
                    fields.iter().all(|(key, value)| match key.as_str() {
                        "amplifier" => bounds(value, amplifier as f64),
                        "duration" => bounds(value, duration as f64),
                        "ambient" => flag(value) == Some(ambient),
                        "visible" => flag(value) == Some(visible),
                        _ => false,
                    })
                })
        })
    })
}

fn potion_contents<M: Mode>(predicate: &Value, stack: &ItemStack<M>) -> bool {
    let Some(fields) = predicate.as_object() else {
        return false;
    };
    let contents = stack.components.potion_contents.as_deref();
    let base = contents.and_then(|c| c.potion);
    fields.iter().all(|(key, wanted)| match key.as_str() {
        "potions" => base.is_some_and(|potion| {
            in_set(wanted, potion.name(), |tag| {
                super::potion_in_tag(potion, tag)
            })
        }),
        "effects" => {
            let mut effects: Vec<(String, i64, i64, bool, bool)> = Vec::new();
            if let Some(potion) = base {
                for e in potion.effects() {
                    effects.push((
                        e.effect.name().to_owned(),
                        e.amplifier as i64,
                        e.duration,
                        e.ambient,
                        e.show_particles,
                    ));
                }
            }
            for e in contents.map_or(&[][..], |c| c.custom_effects.as_slice()) {
                effects.push((
                    e.effect.name().to_owned(),
                    i64::from(e.details.amplifier),
                    i64::from(e.details.duration),
                    e.details.ambient,
                    e.details.show_particles,
                ));
            }
            collection_of(wanted, &effects, |test, (id, amp, dur, amb, vis)| {
                mob_effect_instance(test, id, *amp, *dur, *amb, *vis)
            })
        }
        _ => false,
    })
}

/// Item component predicates beyond enchantments, damage and potions;
/// `None` for one handled elsewhere or not at all.
pub(super) fn item_component<M: Mode>(
    id: &str,
    predicate: &Value,
    stack: &ItemStack<M>,
) -> Option<bool> {
    let components = &stack.components;
    Some(match id {
        "minecraft:custom_data" => {
            let patch = components.to_nbt();
            let data = patch
                .get("minecraft:custom_data")
                .and_then(Tag::as_compound)
                .cloned()
                .unwrap_or_default();
            match predicate {
                Value::String(_) => nbt_matches(predicate, &data),
                Value::Object(_) => nbt_matches(&Value::String(predicate.to_string()), &data),
                _ => false,
            }
        }
        "minecraft:container" => {
            let stacks: Vec<&ItemStack<M>> = components
                .container
                .iter()
                .flatten()
                .flatten()
                .filter(|stack| stack.count() > 0)
                .collect();
            predicate
                .get("items")
                .is_none_or(|items| collection(items, &stacks))
        }
        "minecraft:bundle_contents" => {
            let stacks: Vec<&ItemStack<M>> = components
                .bundle_contents
                .iter()
                .flat_map(|bundle| bundle.items())
                .collect();
            predicate
                .get("items")
                .is_none_or(|items| collection(items, &stacks))
        }
        "minecraft:trim" => components.trim.as_ref().is_some_and(|trim| {
            predicate.get("material").is_none_or(|set| {
                in_set(set, &trim.material, |tag| {
                    in_tag("minecraft:trim_material", tag, &trim.material)
                })
            }) && predicate.get("pattern").is_none_or(|set| {
                in_set(set, &trim.pattern, |tag| {
                    in_tag("minecraft:trim_pattern", tag, &trim.pattern)
                })
            })
        }),
        "minecraft:jukebox_playable" => {
            let patch = components.to_nbt();
            let song = patch
                .get("minecraft:jukebox_playable")
                .and_then(|tag| match tag {
                    Tag::String(song) => Some(song.clone()),
                    Tag::Compound(playable) => playable.get_str("song").map(str::to_owned),
                    _ => None,
                });
            song.is_some_and(|song| {
                predicate.get("song").is_none_or(|set| {
                    in_set(set, &song, |tag| {
                        in_tag("minecraft:jukebox_song", tag, &song)
                    })
                })
            })
        }
        "minecraft:potion_contents" => potion_contents(predicate, stack),
        "minecraft:attribute_modifiers" => {
            let tag = component_tag(stack, id);
            let entries = match &tag {
                Some(Tag::Compound(map)) => list_of(map.get("modifiers")),
                other => list_of(other.as_ref()),
            };
            predicate.as_object().is_some_and(|fields| {
                fields.iter().all(|(key, value)| match key.as_str() {
                    "modifiers" => {
                        collection_of(value, &entries, |test, entry| attribute_entry(test, entry))
                    }
                    _ => false,
                })
            }) && tag.is_some()
        }
        "minecraft:fireworks" => component_tag(stack, id).is_some_and(|tag| {
            let Some(map) = tag.as_compound() else {
                return false;
            };
            let explosions = list_of(map.get("explosions"));
            predicate.as_object().is_some_and(|fields| {
                fields.iter().all(|(key, value)| match key.as_str() {
                    "explosions" => collection_of(value, &explosions, |test, tag| {
                        firework_explosion(test, tag)
                    }),
                    "flight_duration" => {
                        bounds(value, int_tag(map.get("flight_duration")).unwrap_or(1.0))
                    }
                    _ => false,
                })
            })
        }),
        "minecraft:firework_explosion" => {
            component_tag(stack, id).is_some_and(|tag| firework_explosion(predicate, &tag))
        }
        "minecraft:writable_book_content" => component_tag(stack, id).is_some_and(|tag| {
            let pages = list_of(tag.as_compound().and_then(|map| map.get("pages")));
            predicate.as_object().is_some_and(|fields| {
                fields.iter().all(|(key, value)| match key.as_str() {
                    "pages" => collection_of(value, &pages, |test, tag| page_matches(test, tag)),
                    _ => false,
                })
            })
        }),
        "minecraft:written_book_content" => component_tag(stack, id).is_some_and(|tag| {
            let Some(book) = tag.as_compound() else {
                return false;
            };
            let pages = list_of(book.get("pages"));
            predicate.as_object().is_some_and(|fields| {
                fields.iter().all(|(key, value)| match key.as_str() {
                    "pages" => collection_of(value, &pages, |test, tag| page_matches(test, tag)),
                    "author" => value.as_str() == book.get_str("author"),
                    "title" => {
                        let title = book.get("title").and_then(page_text);
                        value.as_str().is_some_and(|wanted| title == Some(wanted))
                    }
                    "generation" => bounds(value, int_tag(book.get("generation")).unwrap_or(0.0)),
                    "resolved" => {
                        flag(value) == Some(flag_tag(book.get("resolved")).unwrap_or(false))
                    }
                    _ => false,
                })
            })
        }),
        "minecraft:villager/variant" => component_tag(stack, id).is_some_and(|tag| {
            matches!(&tag, Tag::String(kind) if in_set(predicate, kind, |t| {
                in_tag("minecraft:villager_type", t, kind)
            }))
        }),
        other if other.starts_with("minecraft:") || other.contains(':') => {
            let present = component_tag(stack, other).is_some();
            return if present && predicate.as_object().is_some_and(|m| m.is_empty()) {
                Some(true)
            } else {
                None
            };
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;
    use tuff_vanilla_data::Item;
    use tuff_vanilla_data::registry::CustomStat;

    use super::super::{EntityFacts, LiveFacts, PlayerFacts, entity};
    use super::*;
    use crate::protocol::play::GameMode;
    use tuff_item::item::MintReason;

    fn test(predicate: Value, facts: &EntityFacts) -> bool {
        entity(&predicate, facts, facts.position)
    }

    #[tuff_core::watchdog]
    #[test]
    fn fishing_hook_and_lightning_fields() {
        let hook = EntityFacts {
            kind: Some(EntityType::FishingBobber),
            open_water: Some(true),
            ..Default::default()
        };
        assert!(test(
            json!({"type_specific/fishing_hook": {"in_open_water": true}}),
            &hook
        ));
        assert!(!test(
            json!({"type_specific/fishing_hook": {"in_open_water": false}}),
            &hook
        ));
        let bolt = EntityFacts {
            kind: Some(EntityType::LightningBolt),
            blocks_set_on_fire: Some(3),
            entity_struck: vec![EntityFacts {
                kind: Some(EntityType::Cow),
                ..Default::default()
            }],
            ..Default::default()
        };
        let lightning = |fields: Value| test(json!({"type_specific/lightning": fields}), &bolt);
        assert!(lightning(
            json!({"blocks_set_on_fire": {"min": 2, "max": 3}})
        ));
        assert!(!lightning(json!({"blocks_set_on_fire": 4})));
        assert!(lightning(json!({"entity_struck": {"type": "cow"}})));
        assert!(!lightning(json!({"entity_struck": {"type": "pig"}})));
    }

    #[tuff_core::watchdog]
    #[test]
    fn slots_cover_equipment_inventory_and_ender_chest() {
        let mut saved = Compound::new();
        let mut stone = tuff_item::item::ItemPicture::new(Item::Stone, 3).to_nbt();
        stone.insert("Slot", 4i8);
        let mut pearl = tuff_item::item::ItemPicture::new(Item::EnderPearl, 1).to_nbt();
        pearl.insert("Slot", 2i8);
        saved.insert("Inventory", Tag::List(vec![Tag::Compound(stone)]));
        saved.insert("EnderItems", Tag::List(vec![Tag::Compound(pearl)]));
        let facts = EntityFacts {
            kind: Some(EntityType::Player),
            equipment: vec![(
                EquipmentSlot::Head,
                tuff_item::item::ItemSnapshot::new(Item::IronHelmet, 1),
            )],
            nbt: Some(saved),
            ..EntityFacts::default()
        };
        assert!(test(
            json!({"slots": {"hotbar.4": {"items": "stone"}}}),
            &facts
        ));
        assert!(test(
            json!({"slots": {"container.*": {"items": "stone", "count": 3}}}),
            &facts
        ));
        assert!(!test(
            json!({"slots": {"inventory.*": {"items": "stone"}}}),
            &facts
        ));
        assert!(test(
            json!({"slots": {"enderchest.2": {"items": "ender_pearl"}}}),
            &facts
        ));
        assert!(test(
            json!({"slots": {"armor.*": {"items": "iron_helmet"}}}),
            &facts
        ));
        assert!(!test(json!({"slots": {"armor.feet": {}}}), &facts));
        assert!(!test(json!({"slots": {"nowhere.1": {}}}), &facts));
    }

    #[tuff_core::watchdog]
    #[test]
    fn movement_ticks_and_type_specific_data() {
        let mut sheep = EntityFacts {
            kind: Some(EntityType::Sheep),
            movement: Some([0.15, -0.2, 0.2]),
            fall_distance: Some(1.5),
            tick_count: Some(40),
            nbt: Some(Compound::new().with("Sheared", true)),
            ..EntityFacts::default()
        };
        assert!(test(
            json!({"movement": {"horizontal_speed": {"min": 4.9, "max": 5.1},
            "vertical_speed": 4.0, "fall_distance": {"min": 1}}}),
            &sheep
        ));
        assert!(!test(json!({"movement": {"y": {"min": 0}}}), &sheep));
        assert!(test(json!({"periodic_tick": 20}), &sheep));
        assert!(!test(json!({"periodic_tick": 30}), &sheep));
        assert!(test(
            json!({"type_specific/sheep": {"sheared": true}}),
            &sheep
        ));
        assert!(!test(json!({"type_specific/cube_mob": {}}), &sheep));
        sheep.kind = Some(EntityType::Slime);
        sheep.nbt = Some(Compound::new().with("Size", 3));
        assert!(test(json!({"type_specific/cube_mob": {"size": 4}}), &sheep));
        sheep.kind = Some(EntityType::Pillager);
        sheep.nbt = Some(Compound::new().with("RaidId", 1));
        assert!(test(
            json!({"type_specific/raider": {"has_raid": true}}),
            &sheep
        ));
        assert!(!test(
            json!({"type_specific/raider": {"is_captain": true, "has_raid": true}}),
            &sheep
        ));
    }

    #[tuff_core::watchdog]
    #[test]
    fn player_food_recipes_stats_and_input() {
        let mut stats = crate::world::stats::Stats::default();
        stats.award(Stat::custom(CustomStat::Jump), 12);
        let saved = Compound::new()
            .with("foodLevel", 7)
            .with("foodSaturationLevel", 1.5f32)
            .with(
                "recipeBook",
                Compound::new().with(
                    "recipes",
                    Tag::List(vec![Tag::String("minecraft:stick".into())]),
                ),
            );
        let facts = EntityFacts {
            kind: Some(EntityType::Player),
            nbt: Some(saved),
            player: Some(PlayerFacts {
                game_mode: GameMode::Survival,
                level: 0,
                advancements: None,
                stats: None,
                input: None,
                food: None,
            }),
            ..EntityFacts::default()
        }
        .with_live(&LiveFacts {
            sprinting: true,
            input: 0x01 | 0x40,
            stats: Arc::new(stats),
            ..LiveFacts::default()
        });
        let player = |fields: Value| test(json!({"type_specific/player": fields}), &facts);
        assert!(player(
            json!({"food": {"level": {"max": 7}, "saturation": 1.5}})
        ));
        assert!(player(
            json!({"recipes": {"stick": true, "minecraft:torch": false}})
        ));
        assert!(player(
            json!({"stats": [{"type": "custom", "stat": "jump", "value": {"min": 10}}]})
        ));
        assert!(!player(
            json!({"stats": [{"type": "minecraft:custom", "stat": "jump", "value": 3}]})
        ));
        assert!(player(
            json!({"input": {"forward": true, "sprint": true, "jump": false}})
        ));
        assert!(test(json!({"flags": {"is_sprinting": true}}), &facts));
    }

    #[tuff_core::watchdog]
    #[test]
    fn item_component_predicates() {
        let mut shulker = ItemStack::mint(Item::ShulkerBox, 1, MintReason::Test);
        shulker.components.container = Some(vec![
            Some(ItemStack::mint(Item::Diamond, 2, MintReason::Test)),
            None,
            Some(ItemStack::mint(Item::Diamond, 1, MintReason::Test)),
        ]);
        let check = |predicate: Value, stack: &ItemStack| item(&predicate, stack);
        assert!(check(
            json!({"predicates": {"container": {"items": {
            "contains": [{"items": "diamond"}], "count": [{"test": {"items": "diamond"}, "count": 2}],
            "size": 2}}}}),
            &shulker
        ));
        assert!(!check(
            json!({"predicates": {"container": {"items": {"contains": [{"items": "dirt"}]}}}}),
            &shulker
        ));
        let mut tagged = ItemStack::mint(Item::Stick, 1, MintReason::Test);
        tagged.components.other.insert(
            "minecraft:custom_data",
            Compound::new().with("magic", 1).with("other", 2),
        );
        assert!(check(
            json!({"predicates": {"custom_data": "{magic:1}"}}),
            &tagged
        ));
        assert!(!check(
            json!({"predicates": {"custom_data": "{magic:2}"}}),
            &tagged
        ));
    }
}
