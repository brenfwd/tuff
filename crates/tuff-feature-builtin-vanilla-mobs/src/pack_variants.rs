//! Mob variants and sound variants as the world's data packs leave them:
//! the built-in lists with pack-added entries after them (their network
//! ids) and pack overrides decoded in place. Registries are start-only, so
//! each world's lists are built once and kept.

use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};

use serde_json::Value;
use tuff_item::registries::{Entry, Registries};
use tuff_vanilla_data::registry::SoundEvent;
use tuff_vanilla_data::variant::{self, FarmVariant, SoundSet, SoundVariant, SpawnPriority};

type Cache<T> = Mutex<Vec<(&'static str, Weak<Registries>, &'static [T])>>;

fn leak<T>(items: Vec<T>) -> &'static [T] {
    Box::leak(items.into_boxed_slice())
}

fn leak_str(text: &str) -> &'static str {
    tuff_item::registries::intern(text)
}

fn cached<T: 'static>(
    cache: &'static OnceLock<Cache<T>>,
    key: &'static str,
    builtin: &'static [T],
    build: impl FnOnce(&Registries) -> Vec<T>,
) -> &'static [T] {
    let registries = tuff_item::registries::current();
    if !registries.get(key).is_some_and(|r| r.modified()) {
        return builtin;
    }
    let mut cache = cache
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    cache.retain(|(_, owner, _)| owner.strong_count() > 0);
    if let Some((_, _, list)) = cache
        .iter()
        .find(|(k, owner, _)| *k == key && std::ptr::eq(owner.as_ptr(), Arc::as_ptr(&registries)))
    {
        return list;
    }
    let list = leak(build(&registries));
    cache.push((key, Arc::downgrade(&registries), list));
    list
}

/// The names a holder set stands for: a `#tag`'s members, one id or a
/// list.
fn names(registry: &str, value: &Value) -> Vec<&'static str> {
    match value {
        Value::String(name) => match name.strip_prefix('#') {
            Some(tag) => tuff_datapack::tags::with(|tags| {
                tags.get(registry, tag)
                    .unwrap_or_default()
                    .iter()
                    .map(|id| leak_str(id))
                    .collect()
            }),
            None => vec![leak_str(&qualify(name))],
        },
        Value::Array(ids) => ids
            .iter()
            .filter_map(Value::as_str)
            .map(|id| leak_str(&qualify(id)))
            .collect(),
        _ => Vec::new(),
    }
}

fn qualify(id: &str) -> String {
    if id.contains(':') {
        id.to_owned()
    } else {
        format!("minecraft:{id}")
    }
}

/// A pack variant's spawn conditions.
pub fn spawn_conditions(json: &Value) -> Vec<SpawnPriority> {
    json["spawn_conditions"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|entry| {
            let condition = &entry["condition"];
            let kind = condition["type"].as_str().unwrap_or_default();
            let mut priority = SpawnPriority {
                priority: entry["priority"].as_i64().unwrap_or(0) as i32,
                biomes: None,
                structures: None,
                min_moon_brightness: None,
            };
            match kind.strip_prefix("minecraft:").unwrap_or(kind) {
                "biome" => {
                    priority.biomes = Some(leak(names(
                        "minecraft:worldgen/biome",
                        &condition["biomes"],
                    )))
                }
                "structure" => {
                    priority.structures = Some(leak(names(
                        "minecraft:worldgen/structure",
                        &condition["structures"],
                    )))
                }
                "moon_brightness" => {
                    priority.min_moon_brightness =
                        Some(condition["range"]["min"].as_f64().unwrap_or(0.0) as f32)
                }
                _ => {}
            }
            priority
        })
        .collect()
}

fn farm(key: &'static str, builtin: &'static [FarmVariant]) -> &'static [FarmVariant] {
    static CACHE: OnceLock<Cache<FarmVariant>> = OnceLock::new();
    cached(&CACHE, key, builtin, |registries| {
        let Some(frozen) = registries.get(key) else {
            return builtin.to_vec();
        };
        frozen
            .network_iter()
            .map(|(id, entry, hidden)| {
                let name = leak_str(id);
                let found = builtin.iter().find(|v| v.name == id);
                match (entry, found) {
                    _ if hidden => FarmVariant {
                        name,
                        spawn_conditions: &[],
                    },
                    (Entry::Builtin(_), Some(variant)) => *variant,
                    _ => FarmVariant {
                        name,
                        spawn_conditions: entry
                            .value()
                            .json
                            .as_deref()
                            .map_or(&[], |json| leak(spawn_conditions(json))),
                    },
                }
            })
            .collect()
    })
}

fn sound_set(json: &Value, fallback: &SoundSet) -> SoundSet {
    let event = |key: &str| json[key].as_str().and_then(SoundEvent::from_name);
    SoundSet {
        ambient: event("ambient_sound").unwrap_or(fallback.ambient),
        hurt: event("hurt_sound").unwrap_or(fallback.hurt),
        death: event("death_sound").unwrap_or(fallback.death),
        step: event("step_sound"),
        eat: event("eat_sound"),
        growl: event("growl_sound"),
        pant: event("pant_sound"),
        whine: event("whine_sound"),
        beg_for_food: event("beg_for_food_sound"),
        hiss: event("hiss_sound"),
        purr: event("purr_sound"),
        purreow: event("purreow_sound"),
        stray_ambient: event("stray_ambient_sound"),
    }
}

/// A pack sound variant, with the first built-in one's sounds where a
/// sound is not a known event.
pub fn sound_variant(name: &'static str, json: &Value, fallback: &SoundVariant) -> SoundVariant {
    match (json.get("adult_sounds"), json.get("baby_sounds")) {
        (Some(adult), baby) => SoundVariant {
            name,
            adult: sound_set(adult, &fallback.adult),
            baby: baby.map(|baby| sound_set(baby, fallback.sounds(true))),
        },
        _ => SoundVariant {
            name,
            adult: sound_set(json, &fallback.adult),
            baby: None,
        },
    }
}

fn sounds(key: &'static str, builtin: &'static [SoundVariant]) -> &'static [SoundVariant] {
    static CACHE: OnceLock<Cache<SoundVariant>> = OnceLock::new();
    cached(&CACHE, key, builtin, |registries| {
        let Some(frozen) = registries.get(key) else {
            return builtin.to_vec();
        };
        frozen
            .network_iter()
            .map(|(id, entry, _)| {
                let found = builtin.iter().find(|v| v.name == id);
                match (entry, found, entry.value().json.as_deref()) {
                    (Entry::Builtin(_), Some(variant), _) => *variant,
                    (_, _, Some(json)) => sound_variant(leak_str(id), json, &builtin[0]),
                    _ => SoundVariant {
                        name: leak_str(id),
                        ..builtin[0]
                    },
                }
            })
            .collect()
    })
}

macro_rules! lists {
    ($($farm:ident, $sounds:ident => $variant_key:literal, $sound_key:literal, $builtin:ident, $builtin_sounds:ident;)*) => {
        $(
            pub fn $farm() -> &'static [FarmVariant] {
                farm($variant_key, variant::$builtin)
            }
            pub fn $sounds() -> &'static [SoundVariant] {
                sounds($sound_key, variant::$builtin_sounds)
            }
        )*
    };
}

lists! {
    cow, cow_sounds => "minecraft:cow_variant", "minecraft:cow_sound_variant", COW, COW_SOUNDS;
    pig, pig_sounds => "minecraft:pig_variant", "minecraft:pig_sound_variant", PIG, PIG_SOUNDS;
    chicken, chicken_sounds => "minecraft:chicken_variant", "minecraft:chicken_sound_variant", CHICKEN, CHICKEN_SOUNDS;
    wolf, wolf_sounds => "minecraft:wolf_variant", "minecraft:wolf_sound_variant", WOLF, WOLF_SOUNDS;
    cat, cat_sounds => "minecraft:cat_variant", "minecraft:cat_sound_variant", CAT, CAT_SOUNDS;
}

pub fn frog() -> &'static [FarmVariant] {
    farm("minecraft:frog_variant", variant::FROG)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frozen(entries: &[(&str, &str, &str)]) -> Arc<Registries> {
        let mut loader = tuff_item::registries::Loader::default();
        for (key, id, json) in entries {
            loader.insert(
                tuff_vanilla_data::data_registry::DataRegistry::get(key).unwrap(),
                (*id).to_owned(),
                "p",
                json.as_bytes().to_vec(),
            );
        }
        loader.freeze().unwrap_or_else(|e| panic!("{e}"))
    }

    #[tuff_core::watchdog]
    #[test]
    fn pack_variants_follow_the_built_in_ones_and_spawn_where_they_say() {
        let registries = frozen(&[
            (
                "minecraft:cat_variant",
                "ex:spotted",
                r#"{"asset_id":"ex:entity/cat/spotted","baby_asset_id":"ex:entity/cat/spotted_baby","spawn_conditions":[{"priority":5,"condition":{"type":"minecraft:biome","biomes":"minecraft:desert"}}]}"#,
            ),
            (
                "minecraft:cat_sound_variant",
                "ex:quiet",
                r#"{"adult_sounds":{"ambient_sound":"minecraft:entity.cat.purr","hurt_sound":"minecraft:entity.cat.hurt","death_sound":"minecraft:entity.cat.death"},"baby_sounds":{"ambient_sound":"minecraft:entity.cat.purr","hurt_sound":"minecraft:entity.cat.hurt","death_sound":"minecraft:entity.cat.death"}}"#,
            ),
        ]);
        tuff_item::registries::sync_scope(registries, || {
            let cats = cat();
            assert_eq!(cats.len(), variant::CAT.len() + 1);
            assert_eq!(&cats[..variant::CAT.len()], variant::CAT);
            let spotted = cats.len() - 1;
            assert_eq!(cats[spotted].name, "ex:spotted");
            assert_eq!(variant::candidates(cats, "minecraft:desert"), [spotted]);
            assert!(!variant::candidates(cats, "minecraft:plains").contains(&spotted));
            let sounds = cat_sounds();
            let quiet = sounds.last().unwrap();
            assert_eq!(quiet.name, "ex:quiet");
            assert_eq!(quiet.adult.ambient, SoundEvent::EntityCatPurr);
        });
        assert_eq!(cat(), variant::CAT);
    }
}
