//! Per-item values from the default data components that drive gameplay:
//! food and consumables, use remainders and cooldowns, equipment, attribute
//! modifiers, shields, weapons, repair materials, enchantability, cooking
//! fuel and compostability. Each
//! component is emitted as a table of distinct values plus a per-item index,
//! since most items share a handful of values.

use std::collections::HashMap;
use std::fmt::Write;

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Map, Value};

use super::array_body;
use super::providers;
use super::registries::{self, sorted_entries};
use super::tags::resolved_registry_tags;
use crate::input::Input;
use crate::names::{pascal, str_literal};

const NONE: usize = u16::MAX as usize;
const FOOD_FIELDS: [&str; 3] = ["nutrition", "saturation", "can_always_eat"];
const CONSUMABLE_FIELDS: [&str; 5] = [
    "consume_seconds",
    "animation",
    "sound",
    "has_consume_particles",
    "on_consume_effects",
];
const EFFECT_FIELDS: [&str; 6] = [
    "id",
    "duration",
    "amplifier",
    "ambient",
    "show_particles",
    "show_icon",
];
const EQUIPPABLE_FIELDS: [&str; 11] = [
    "slot",
    "equip_sound",
    "asset_id",
    "camera_overlay",
    "allowed_entities",
    "dispensable",
    "swappable",
    "damage_on_hurt",
    "equip_on_interact",
    "can_be_sheared",
    "shearing_sound",
];
const MODIFIER_FIELDS: [&str; 6] = ["type", "id", "amount", "operation", "slot", "display"];
const BLOCKS_ATTACKS_FIELDS: [&str; 7] = [
    "block_delay_seconds",
    "disable_cooldown_scale",
    "damage_reductions",
    "item_damage",
    "bypassed_by",
    "block_sound",
    "disabled_sound",
];
const WEAPON_FIELDS: [&str; 2] = ["item_damage_per_attack", "disable_blocking_for_seconds"];
const PIERCING_WEAPON_FIELDS: [&str; 4] = ["deals_knockback", "dismounts", "sound", "hit_sound"];
const KINETIC_WEAPON_FIELDS: [&str; 9] = [
    "contact_cooldown_ticks",
    "delay_ticks",
    "dismount_conditions",
    "knockback_conditions",
    "damage_conditions",
    "forward_movement",
    "damage_multiplier",
    "sound",
    "hit_sound",
];
const KINETIC_CONDITION_FIELDS: [&str; 3] =
    ["max_duration_ticks", "min_speed", "min_relative_speed"];
const ATTACK_RANGE_FIELDS: [&str; 6] = [
    "min_reach",
    "max_reach",
    "min_creative_reach",
    "max_creative_reach",
    "hitbox_margin",
    "mob_factor",
];
const USE_ANIMATIONS: [&str; 11] = [
    "none",
    "eat",
    "drink",
    "block",
    "bow",
    "spear",
    "crossbow",
    "spyglass",
    "toot_horn",
    "brush",
    "bundle",
];
const EQUIPMENT_SLOTS: [&str; 8] = [
    "mainhand", "offhand", "feet", "legs", "chest", "head", "body", "saddle",
];
pub(super) const SLOT_GROUPS: [&str; 11] = [
    "any", "mainhand", "offhand", "hand", "feet", "legs", "chest", "head", "armor", "body",
    "saddle",
];
pub(super) const OPERATIONS: [&str; 3] =
    ["add_value", "add_multiplied_base", "add_multiplied_total"];

struct Table {
    name: &'static str,
    ty: &'static str,
    values: Vec<String>,
    ids: HashMap<String, usize>,
    index: Vec<usize>,
}

impl Table {
    fn new(name: &'static str, ty: &'static str) -> Self {
        Self {
            name,
            ty,
            values: Vec::new(),
            ids: HashMap::new(),
            index: Vec::new(),
        }
    }

    fn push(&mut self, value: Option<String>) -> Result<()> {
        let id = match value {
            None => NONE,
            Some(code) => {
                let next = self.values.len();
                let id = *self.ids.entry(code.clone()).or_insert(next);
                if id == next {
                    ensure!(id < NONE, "too many distinct {}", self.name);
                    self.values.push(code);
                }
                id
            }
        };
        self.index.push(id);
        Ok(())
    }

    fn emit(&self, out: &mut String) -> Result<()> {
        writeln!(
            out,
            "static {}_TABLE: [{}; {}] = [",
            self.name,
            self.ty,
            self.values.len()
        )?;
        for value in &self.values {
            writeln!(out, "    {value},")?;
        }
        writeln!(out, "];")?;
        write!(
            out,
            "static ITEM_{}: [u16; {}] = [",
            self.name,
            self.index.len()
        )?;
        array_body(out, self.index.iter().map(|i| *i as u16));
        writeln!(out, "];")?;
        Ok(())
    }
}

pub fn generate(input: &Input) -> Result<String> {
    let registry = &input.registries["minecraft:item"];
    let items = sorted_entries(registry)?;
    let item_variants = registries::variants(registry)?;
    let sounds = registries::variants(&input.registries["minecraft:sound_event"])?;
    let effects = registries::variants(&input.registries["minecraft:mob_effect"])?;
    let attributes = registries::variants(&input.registries["minecraft:attribute"])?;
    let item_tags = resolved_registry_tags(input, "minecraft:item")?;
    let effect_tags = resolved_registry_tags(input, "minecraft:mob_effect")?;
    let damage_tags = resolved_registry_tags(input, "minecraft:damage_type")?;

    let mut food = Table::new("FOOD", "Food");
    let mut consumable = Table::new("CONSUMABLE", "Consumable");
    let mut equippable = Table::new("EQUIPPABLE", "Equippable");
    let mut modifiers = Table::new("ATTRIBUTE_MODIFIERS", "&[AttributeModifier]");
    let mut blocks_attacks = Table::new("BLOCKS_ATTACKS", "BlocksAttacks");
    let mut weapon = Table::new("WEAPON", "Weapon");
    let mut piercing_weapon = Table::new("PIERCING_WEAPON", "PiercingWeapon");
    let mut attack_range = Table::new("ATTACK_RANGE", "AttackRange");
    let mut minimum_attack_charge = Vec::new();
    let mut use_effects = Vec::new();
    let mut item_damage_type = Vec::new();
    let mut repairable = Table::new("REPAIRABLE", "&[Item]");
    let mut cooldown = Table::new("USE_COOLDOWN", "UseCooldown");
    let mut cooking_fuel = Table::new("COOKING_FUEL", "CookingFuel");
    let mut brewing_fuel = Table::new("BREWING_FUEL", "BrewingFuel");
    let mut duration_scale = Vec::new();
    let mut entity_data = Table::new("ENTITY_DATA", "crate::registry::EntityType");
    let mut mob_visibility = Table::new("MOB_VISIBILITY", "MobVisibility");
    let entity_types = registries::variants(&input.registries["minecraft:entity_type"])?;
    let entity_tags = resolved_registry_tags(input, "minecraft:entity_type")?;
    let mut compostable = Vec::new();
    let mut remainder = Vec::new();
    let mut enchantable = Vec::new();
    let mut damage_resistant = Vec::new();
    let mut glider = Vec::new();
    let mut kinetic_weapon = Vec::new();
    let mut kinetic_data = Table::new("KINETIC_WEAPON_DATA", "KineticWeapon");
    let mut songs: Vec<(String, u8, u32)> = Vec::new();
    for (path, value) in &input.data_json {
        let Some((namespace, rest)) = path.split_once('/') else {
            continue;
        };
        let Some(name) = rest
            .strip_prefix("jukebox_song/")
            .and_then(|rest| rest.strip_suffix(".json"))
        else {
            continue;
        };
        let fields = fields(
            value,
            &[
                "comparator_output",
                "description",
                "length_in_seconds",
                "sound_event",
            ],
        )
        .with_context(|| format!("jukebox song {name}"))?;
        let output = fields["comparator_output"]
            .as_u64()
            .filter(|output| *output <= 15)
            .with_context(|| format!("jukebox song {name}'s comparator_output"))?;
        let seconds = fields["length_in_seconds"]
            .as_f64()
            .filter(|seconds| *seconds > 0.0)
            .with_context(|| format!("jukebox song {name}'s length_in_seconds"))?;
        songs.push((
            format!("{namespace}:{name}"),
            output as u8,
            (seconds * 20.0) as u32,
        ));
    }
    songs.sort();
    anyhow::ensure!(!songs.is_empty(), "no jukebox songs");
    let mut jukebox = Vec::new();
    let mut jukebox_song = Vec::new();
    let mut dye = Vec::new();
    let mut villager_food = Vec::new();
    let mut trim_material = Vec::new();
    let trim_materials: Vec<String> = input
        .data_files
        .iter()
        .filter_map(|file| {
            let (namespace, rest) = file.split_once('/')?;
            let name = rest.strip_prefix("trim_material/")?.strip_suffix(".json")?;
            Some(format!("{namespace}:{name}"))
        })
        .collect();

    let sound = |value: &Value| -> Result<String> {
        let name = value.as_str().context("sound is not a string")?;
        let variant = sounds
            .get(name)
            .with_context(|| format!("unknown sound event {name}"))?;
        Ok(format!("SoundEvent::{variant}"))
    };
    let item_list = |value: &Value| -> Result<String> {
        let mut names = Vec::new();
        list_entries(value, &item_tags, &mut names)?;
        let mut ids: Vec<(u32, &str)> = names
            .iter()
            .map(|name| {
                let id = registry
                    .entries
                    .get(name)
                    .with_context(|| format!("unknown item {name}"))?;
                Ok((id.protocol_id, name.as_str()))
            })
            .collect::<Result<_>>()?;
        ids.sort();
        ids.dedup();
        let variants: Vec<String> = ids
            .iter()
            .map(|(_, name)| format!("Item::{}", item_variants[name]))
            .collect();
        Ok(format!("&[{}]", variants.join(", ")))
    };

    for item in &items {
        let components = input
            .item_components
            .get(*item)
            .with_context(|| format!("no component report for {item}"))?;
        let context = |what: &str| format!("{item}'s {what}");

        entity_data.push(match components.get("minecraft:entity_data") {
            None => None,
            Some(value) => {
                let object = value.as_object().with_context(|| context("entity data"))?;
                ensure!(
                    object.len() == 1,
                    "{item}'s entity data has more than an id: {value}"
                );
                let id = object["id"]
                    .as_str()
                    .with_context(|| context("entity data id"))?;
                let variant = entity_types
                    .get(id)
                    .with_context(|| format!("{item}'s entity data names {id}"))?;
                Some(format!("crate::registry::EntityType::{variant}"))
            }
        })?;

        food.push(match components.get("minecraft:food") {
            None => None,
            Some(value) => {
                let fields = fields(value, &FOOD_FIELDS).with_context(|| context("food"))?;
                Some(format!(
                    "Food {{ nutrition: {}, saturation: {}, can_always_eat: {} }}",
                    integer(fields.get("nutrition"), None)?,
                    float(fields.get("saturation"), None)?,
                    boolean(fields.get("can_always_eat"), false)?,
                ))
            }
        })?;

        consumable.push(match components.get("minecraft:consumable") {
            None => None,
            Some(value) => {
                let fields =
                    fields(value, &CONSUMABLE_FIELDS).with_context(|| context("consumable"))?;
                let animation = match fields.get("animation") {
                    None => "eat".to_string(),
                    Some(value) => value.as_str().context("bad animation")?.to_string(),
                };
                ensure!(
                    USE_ANIMATIONS.contains(&animation.as_str()),
                    "{}: unknown use animation {animation}",
                    context("consumable")
                );
                let mut on_consume = Vec::new();
                for effect in fields
                    .get("on_consume_effects")
                    .and_then(Value::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or(&[])
                {
                    on_consume.push(
                        consume_effect(effect, &effects, &effect_tags, &sound)
                            .with_context(|| context("consume effect"))?,
                    );
                }
                Some(format!(
                    "Consumable {{ consume_seconds: {}, animation: UseAnimation::{}, sound: {}, \
                     has_consume_particles: {}, on_consume_effects: &[{}] }}",
                    float(fields.get("consume_seconds"), Some("1.6"))?,
                    pascal(&animation),
                    match fields.get("sound") {
                        Some(value) => sound(value)?,
                        None => "SoundEvent::EntityGenericEat".to_string(),
                    },
                    boolean(fields.get("has_consume_particles"), true)?,
                    on_consume.join(", "),
                ))
            }
        })?;

        remainder.push(match components.get("minecraft:use_remainder") {
            None => "None".to_string(),
            Some(value) => {
                let fields = fields(value, &["id"]).with_context(|| context("use_remainder"))?;
                let id = fields["id"].as_str().context("bad use_remainder id")?;
                let variant = item_variants
                    .get(id)
                    .with_context(|| format!("unknown item {id}"))?;
                format!("Some(Item::{variant})")
            }
        });

        cooldown.push(match components.get("minecraft:use_cooldown") {
            None => None,
            Some(value) => {
                let fields = fields(value, &["seconds", "cooldown_group"])
                    .with_context(|| context("use_cooldown"))?;
                let group = match fields.get("cooldown_group") {
                    None => "None".to_string(),
                    Some(value) => format!(
                        "Some({})",
                        str_literal(value.as_str().context("bad cooldown_group")?)
                    ),
                };
                Some(format!(
                    "UseCooldown {{ seconds: {}, group: {group} }}",
                    float(fields.get("seconds"), None)?
                ))
            }
        })?;

        equippable.push(match components.get("minecraft:equippable") {
            None => None,
            Some(value) => {
                let fields =
                    fields(value, &EQUIPPABLE_FIELDS).with_context(|| context("equippable"))?;
                let slot = fields["slot"].as_str().context("bad slot")?;
                ensure!(
                    EQUIPMENT_SLOTS.contains(&slot),
                    "unknown equipment slot {slot}"
                );
                let equip_sound = match fields.get("equip_sound") {
                    Some(value) => sound(value)?,
                    None => "SoundEvent::ItemArmorEquipGeneric".to_string(),
                };
                let asset = match fields.get("asset_id") {
                    None => "None".to_string(),
                    Some(value) => format!(
                        "Some({})",
                        str_literal(value.as_str().context("bad asset_id")?)
                    ),
                };
                let shearing_sound = match fields.get("shearing_sound") {
                    Some(value) => sound(value)?,
                    None => "SoundEvent::ItemShearsSnip".to_string(),
                };
                Some(format!(
                    "Equippable {{ slot: EquipmentSlot::{}, equip_sound: {equip_sound}, \
                     asset_id: {asset}, swappable: {}, damage_on_hurt: {}, equip_on_interact: {}, \
                     dispensable: {}, restricted: {}, can_be_sheared: {}, \
                     shearing_sound: {shearing_sound} }}",
                    pascal(slot),
                    boolean(fields.get("swappable"), true)?,
                    boolean(fields.get("damage_on_hurt"), true)?,
                    boolean(fields.get("equip_on_interact"), false)?,
                    boolean(fields.get("dispensable"), true)?,
                    fields.contains_key("allowed_entities"),
                    boolean(fields.get("can_be_sheared"), false)?,
                ))
            }
        })?;

        let mut modifier_code = Vec::new();
        for modifier in components
            .get("minecraft:attribute_modifiers")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
        {
            let fields = fields(modifier, &MODIFIER_FIELDS).with_context(|| context("modifier"))?;
            let attribute = fields["type"].as_str().context("bad attribute")?;
            let attribute = attributes
                .get(attribute)
                .with_context(|| format!("unknown attribute {attribute}"))?;
            let operation = fields["operation"].as_str().context("bad operation")?;
            ensure!(
                OPERATIONS.contains(&operation),
                "unknown operation {operation}"
            );
            let slot = match fields.get("slot") {
                None => "any".to_string(),
                Some(value) => value.as_str().context("bad slot")?.to_string(),
            };
            ensure!(
                SLOT_GROUPS.contains(&slot.as_str()),
                "unknown slot group {slot}"
            );
            modifier_code.push(format!(
                "AttributeModifier {{ attribute: Attribute::{attribute}, id: {}, amount: {}, \
                 operation: ModifierOperation::{}, slot: EquipmentSlotGroup::{} }}",
                str_literal(fields["id"].as_str().context("bad modifier id")?),
                double(fields.get("amount"))?,
                pascal(operation),
                pascal(&slot),
            ));
        }
        modifiers.push(Some(format!("&[{}]", modifier_code.join(", "))))?;

        blocks_attacks.push(match components.get("minecraft:blocks_attacks") {
            None => None,
            Some(value) => {
                let fields = fields(value, &BLOCKS_ATTACKS_FIELDS)
                    .with_context(|| context("blocks_attacks"))?;
                let reductions = match fields.get("damage_reductions") {
                    None => vec![
                        "DamageReduction { horizontal_blocking_angle: 90.0, types: None, base: 0.0, factor: 1.0 }"
                            .to_string(),
                    ],
                    Some(value) => value
                        .as_array()
                        .context("bad damage_reductions")?
                        .iter()
                        .map(|reduction| {
                            let f = self::fields(
                                reduction,
                                &["horizontal_blocking_angle", "type", "base", "factor"],
                            )?;
                            let types = match f.get("type") {
                                None => "None".to_string(),
                                Some(value) => {
                                    let mut names = Vec::new();
                                    list_entries(value, &damage_tags, &mut names)?;
                                    format!(
                                        "Some(&[{}])",
                                        names
                                            .iter()
                                            .map(|n| str_literal(n))
                                            .collect::<Vec<_>>()
                                            .join(", ")
                                    )
                                }
                            };
                            Ok(format!(
                                "DamageReduction {{ horizontal_blocking_angle: {}, types: {types}, \
                                 base: {}, factor: {} }}",
                                float(f.get("horizontal_blocking_angle"), Some("90.0"))?,
                                float(f.get("base"), None)?,
                                float(f.get("factor"), None)?,
                            ))
                        })
                        .collect::<Result<_>>()?,
                };
                let item_damage = match fields.get("item_damage") {
                    None => {
                        "ItemDamageFunction { threshold: 1.0, base: 0.0, factor: 1.0 }".to_string()
                    }
                    Some(value) => {
                        let f = self::fields(value, &["threshold", "base", "factor"])?;
                        format!(
                            "ItemDamageFunction {{ threshold: {}, base: {}, factor: {} }}",
                            float(f.get("threshold"), Some("1.0"))?,
                            float(f.get("base"), Some("0.0"))?,
                            float(f.get("factor"), Some("1.0"))?,
                        )
                    }
                };
                let bypassed_by = match fields.get("bypassed_by") {
                    None => "None".to_string(),
                    Some(value) => {
                        let tag = value.as_str().context("bad bypassed_by")?;
                        let tag = tag.strip_prefix('#').context("bypassed_by must be a tag")?;
                        format!("Some({})", str_literal(tag))
                    }
                };
                let optional_sound = |key: &str| -> Result<String> {
                    Ok(match fields.get(key) {
                        None => "None".to_string(),
                        Some(value) => format!("Some({})", sound(value)?),
                    })
                };
                Some(format!(
                    "BlocksAttacks {{ block_delay_seconds: {}, disable_cooldown_scale: {}, \
                     damage_reductions: &[{}], item_damage: {item_damage}, bypassed_by: {bypassed_by}, \
                     block_sound: {}, disabled_sound: {} }}",
                    float(fields.get("block_delay_seconds"), Some("0.0"))?,
                    float(fields.get("disable_cooldown_scale"), Some("1.0"))?,
                    reductions.join(", "),
                    optional_sound("block_sound")?,
                    optional_sound("disabled_sound")?,
                ))
            }
        })?;

        weapon.push(match components.get("minecraft:weapon") {
            None => None,
            Some(value) => {
                let fields = fields(value, &WEAPON_FIELDS).with_context(|| context("weapon"))?;
                Some(format!(
                    "Weapon {{ item_damage_per_attack: {}, disable_blocking_for_seconds: {} }}",
                    integer(fields.get("item_damage_per_attack"), Some(1))?,
                    float(fields.get("disable_blocking_for_seconds"), Some("0.0"))?,
                ))
            }
        })?;

        piercing_weapon.push(match components.get("minecraft:piercing_weapon") {
            None => None,
            Some(value) => {
                let fields = fields(value, &PIERCING_WEAPON_FIELDS)
                    .with_context(|| context("piercing_weapon"))?;
                let optional_sound = |key: &str| -> Result<String> {
                    Ok(match fields.get(key) {
                        None => "None".to_string(),
                        Some(value) => format!("Some({})", sound(value)?),
                    })
                };
                Some(format!(
                    "PiercingWeapon {{ deals_knockback: {}, dismounts: {}, sound: {}, hit_sound: {} }}",
                    boolean(fields.get("deals_knockback"), true)?,
                    boolean(fields.get("dismounts"), false)?,
                    optional_sound("sound")?,
                    optional_sound("hit_sound")?,
                ))
            }
        })?;

        attack_range.push(match components.get("minecraft:attack_range") {
            None => None,
            Some(value) => {
                let fields =
                    fields(value, &ATTACK_RANGE_FIELDS).with_context(|| context("attack_range"))?;
                Some(format!(
                    "AttackRange {{ min_reach: {}, max_reach: {}, min_creative_reach: {}, \
                     max_creative_reach: {}, hitbox_margin: {}, mob_factor: {} }}",
                    float(fields.get("min_reach"), Some("0.0"))?,
                    float(fields.get("max_reach"), Some("3.0"))?,
                    float(fields.get("min_creative_reach"), Some("0.0"))?,
                    float(fields.get("max_creative_reach"), Some("5.0"))?,
                    float(fields.get("hitbox_margin"), Some("0.3"))?,
                    float(fields.get("mob_factor"), Some("1.0"))?,
                ))
            }
        })?;

        minimum_attack_charge.push(match components.get("minecraft:minimum_attack_charge") {
            None => "0.0".to_string(),
            Some(value) => {
                let charge = value
                    .as_f64()
                    .with_context(|| context("minimum_attack_charge"))?;
                format!("{:?}", charge as f32)
            }
        });

        use_effects.push(match components.get("minecraft:use_effects") {
            None => "(false, true, 0.2_f32)".to_string(),
            Some(value) => format!(
                "({}, {}, {})",
                boolean(value.get("can_sprint"), false)?,
                boolean(value.get("interact_vibrations"), true)?,
                float(value.get("speed_multiplier"), Some("0.2"))?,
            ),
        });

        item_damage_type.push(match components.get("minecraft:damage_type") {
            None => "None".to_string(),
            Some(value) => {
                let name = value.as_str().context("bad damage_type")?;
                format!("Some({})", str_literal(name))
            }
        });

        repairable.push(match components.get("minecraft:repairable") {
            None => None,
            Some(value) => {
                let fields = fields(value, &["items"]).with_context(|| context("repairable"))?;
                Some(item_list(&fields["items"]).with_context(|| context("repairable"))?)
            }
        })?;

        enchantable.push(match components.get("minecraft:enchantable") {
            None => "0".to_string(),
            Some(value) => {
                let fields = fields(value, &["value"]).with_context(|| context("enchantable"))?;
                let value = integer(fields.get("value"), None)?;
                ensure!(
                    value > 0,
                    "{}: enchantability must be positive",
                    context("enchantable")
                );
                value.to_string()
            }
        });

        villager_food.push(match components.get("minecraft:villager_food") {
            None => "0".to_string(),
            Some(value) => {
                let object = value.as_object().context("bad villager_food")?;
                ensure!(
                    object.keys().all(|k| k == "nutrition"),
                    "{}: unknown field",
                    context("villager_food")
                );
                let nutrition = object["nutrition"]
                    .as_u64()
                    .filter(|n| (1..=255).contains(n))
                    .with_context(|| context("villager_food nutrition"))?;
                nutrition.to_string()
            }
        });

        dye.push(match components.get("minecraft:dye") {
            None => "None".to_string(),
            Some(value) => {
                let name = value.as_str().context("bad dye")?;
                format!("Some(DyeColor::{})", pascal(name))
            }
        });

        trim_material.push(match components.get("minecraft:provides_trim_material") {
            None => "None".to_string(),
            Some(value) => {
                let name = value.as_str().with_context(|| {
                    context("provides_trim_material must name a registry entry")
                })?;
                ensure!(
                    trim_materials.iter().any(|known| known == name),
                    "{}: unknown trim material {name}",
                    context("provides_trim_material")
                );
                format!("Some({})", str_literal(name))
            }
        });

        cooking_fuel.push(match components.get("minecraft:cooking_fuel") {
            None => None,
            Some(value) => {
                let fields = fields(value, &["burn_time", "speed_multiplier"])
                    .with_context(|| context("cooking_fuel"))?;
                Some(format!(
                    "CookingFuel {{ burn_time: {}, speed_multiplier: {} }}",
                    providers::int_reference(
                        input,
                        fields
                            .get("burn_time")
                            .context("cooking_fuel without burn_time")?
                    )
                    .with_context(|| context("cooking_fuel"))?,
                    providers::float_reference(
                        input,
                        fields
                            .get("speed_multiplier")
                            .context("cooking_fuel without speed_multiplier")?
                    )
                    .with_context(|| context("cooking_fuel"))?,
                ))
            }
        })?;

        brewing_fuel.push(match components.get("minecraft:brewing_fuel") {
            None => None,
            Some(value) => {
                let fields = fields(value, &["uses", "speed_multiplier"])
                    .with_context(|| context("brewing_fuel"))?;
                Some(format!(
                    "BrewingFuel {{ uses: {}, speed_multiplier: {} }}",
                    providers::int_reference(
                        input,
                        fields.get("uses").context("brewing_fuel without uses")?
                    )
                    .with_context(|| context("brewing_fuel"))?,
                    providers::float_reference(
                        input,
                        fields
                            .get("speed_multiplier")
                            .context("brewing_fuel without speed_multiplier")?
                    )
                    .with_context(|| context("brewing_fuel"))?,
                ))
            }
        })?;

        duration_scale.push(match components.get("minecraft:potion_duration_scale") {
            None => "1.0".to_string(),
            Some(value) => {
                let scale = value
                    .as_f64()
                    .with_context(|| context("potion_duration_scale"))?;
                format!("{:?}", scale as f32)
            }
        });

        compostable.push(match components.get("minecraft:compostable") {
            None => "None".to_string(),
            Some(value) => {
                let fields = fields(value, &["layers"]).with_context(|| context("compostable"))?;
                format!(
                    "Some({})",
                    providers::int_reference(
                        input,
                        fields.get("layers").context("compostable without layers")?
                    )
                    .with_context(|| context("compostable"))?
                )
            }
        });

        mob_visibility.push(match components.get("minecraft:mob_visibility") {
            None => None,
            Some(value) => {
                let fields = fields(value, &["targeting_entity_types", "visibility"])
                    .with_context(|| context("mob visibility"))?;
                let mut names = Vec::new();
                list_entries(&fields["targeting_entity_types"], &entity_tags, &mut names)?;
                names.sort();
                names.dedup();
                let kinds = names
                    .iter()
                    .map(|name| {
                        let variant = entity_types
                            .get(name.as_str())
                            .with_context(|| format!("{item}'s mob visibility names {name}"))?;
                        Ok(format!("crate::registry::EntityType::{variant}"))
                    })
                    .collect::<Result<Vec<_>>>()?;
                let visibility = fields["visibility"]
                    .as_f64()
                    .with_context(|| context("mob visibility"))?;
                Some(format!(
                    "MobVisibility {{ targeting: &[{}], visibility: {:?} }}",
                    kinds.join(", "),
                    visibility as f32
                ))
            }
        })?;

        damage_resistant.push(match components.get("minecraft:damage_resistant") {
            None => "None".to_string(),
            Some(value) => {
                let fields =
                    fields(value, &["types"]).with_context(|| context("damage_resistant"))?;
                let tag = fields["types"].as_str().context("bad damage_resistant")?;
                let tag = tag
                    .strip_prefix('#')
                    .context("damage_resistant must be a tag")?;
                format!("Some({})", str_literal(tag))
            }
        });

        glider.push(components.contains_key("minecraft:glider"));
        kinetic_weapon.push(components.contains_key("minecraft:kinetic_weapon"));
        kinetic_data.push(match components.get("minecraft:kinetic_weapon") {
            None => None,
            Some(value) => {
                let fields = fields(value, &KINETIC_WEAPON_FIELDS)
                    .with_context(|| context("kinetic_weapon"))?;
                let optional_sound = |key: &str| -> Result<String> {
                    Ok(match fields.get(key) {
                        None => "None".to_string(),
                        Some(value) => format!("Some({})", sound(value)?),
                    })
                };
                let condition = |key: &str| -> Result<String> {
                    Ok(match fields.get(key) {
                        None => "None".to_string(),
                        Some(value) => {
                            let c = self::fields(value, &KINETIC_CONDITION_FIELDS)
                                .with_context(|| context("kinetic_weapon condition"))?;
                            format!(
                                "Some(KineticCondition {{ max_duration_ticks: {}, min_speed: {}, min_relative_speed: {} }})",
                                integer(c.get("max_duration_ticks"), None)?,
                                float(c.get("min_speed"), Some("0.0"))?,
                                float(c.get("min_relative_speed"), Some("0.0"))?,
                            )
                        }
                    })
                };
                Some(format!(
                    "KineticWeapon {{ contact_cooldown_ticks: {}, delay_ticks: {}, dismount_conditions: {}, knockback_conditions: {}, damage_conditions: {}, forward_movement: {}, damage_multiplier: {}, sound: {}, hit_sound: {} }}",
                    integer(fields.get("contact_cooldown_ticks"), Some(10))?,
                    integer(fields.get("delay_ticks"), Some(0))?,
                    condition("dismount_conditions")?,
                    condition("knockback_conditions")?,
                    condition("damage_conditions")?,
                    float(fields.get("forward_movement"), Some("0.0"))?,
                    float(fields.get("damage_multiplier"), Some("1.0"))?,
                    optional_sound("sound")?,
                    optional_sound("hit_sound")?,
                ))
            }
        })?;

        jukebox_song.push(match components.get("minecraft:jukebox_playable") {
            None => 0u16,
            Some(value) => {
                let song = value
                    .as_str()
                    .with_context(|| context("jukebox_playable"))?;
                songs
                    .binary_search_by(|(name, _, _)| name.as_str().cmp(song))
                    .map_err(|_| anyhow::anyhow!("{item} plays unknown song {song}"))?
                    as u16
                    + 1
            }
        });
        jukebox.push(match components.get("minecraft:jukebox_playable") {
            None => 0,
            Some(value) => {
                let song = value
                    .as_str()
                    .with_context(|| context("jukebox_playable"))?;
                let index = songs
                    .binary_search_by(|(name, _, _)| name.as_str().cmp(song))
                    .map_err(|_| anyhow::anyhow!("{item} plays unknown song {song}"))?;
                songs[index].1
            }
        });
    }

    let mut out = String::new();
    emit_enum(&mut out, "UseAnimation", &USE_ANIMATIONS)?;
    emit_enum(&mut out, "EquipmentSlot", &EQUIPMENT_SLOTS)?;
    emit_enum(&mut out, "EquipmentSlotGroup", &SLOT_GROUPS)?;
    emit_enum(&mut out, "ModifierOperation", &OPERATIONS)?;
    for table in [
        &food,
        &consumable,
        &equippable,
        &modifiers,
        &blocks_attacks,
        &weapon,
        &piercing_weapon,
        &kinetic_data,
        &attack_range,
        &repairable,
        &cooldown,
        &cooking_fuel,
        &brewing_fuel,
        &entity_data,
        &mob_visibility,
    ] {
        table.emit(&mut out)?;
    }
    write!(
        out,
        "static ITEM_COMPOSTABLE: [Option<&crate::provider::IntProvider>; {}] = [",
        items.len()
    )?;
    array_body(&mut out, &compostable);
    writeln!(out, "];")?;
    write!(
        out,
        "static ITEM_USE_REMAINDER: [Option<Item>; {}] = [",
        items.len()
    )?;
    array_body(&mut out, &remainder);
    writeln!(out, "];")?;
    write!(out, "static ITEM_ENCHANTABLE: [u8; {}] = [", items.len())?;
    array_body(&mut out, &enchantable);
    writeln!(out, "];")?;
    write!(
        out,
        "static ITEM_DAMAGE_RESISTANT: [Option<&str>; {}] = [",
        items.len()
    )?;
    array_body(&mut out, &damage_resistant);
    writeln!(out, "];")?;
    write!(out, "static ITEM_VILLAGER_FOOD: [u8; {}] = [", items.len())?;
    array_body(&mut out, &villager_food);
    writeln!(out, "];")?;
    write!(out, "static ITEM_GLIDER: [bool; {}] = [", items.len())?;
    array_body(&mut out, &glider);
    writeln!(out, "];")?;
    write!(
        out,
        "static ITEM_KINETIC_WEAPON: [bool; {}] = [",
        items.len()
    )?;
    array_body(&mut out, &kinetic_weapon);
    writeln!(out, "];")?;
    write!(
        out,
        "static ITEM_DYE: [Option<DyeColor>; {}] = [",
        items.len()
    )?;
    array_body(&mut out, &dye);
    writeln!(out, "];")?;
    write!(
        out,
        "static ITEM_POTION_DURATION_SCALE: [f32; {}] = [",
        items.len()
    )?;
    array_body(&mut out, &duration_scale);
    writeln!(out, "];")?;
    write!(
        out,
        "static ITEM_TRIM_MATERIAL: [Option<&str>; {}] = [",
        items.len()
    )?;
    array_body(&mut out, &trim_material);
    writeln!(out, "];")?;
    write!(
        out,
        "static ITEM_MINIMUM_ATTACK_CHARGE: [f32; {}] = [",
        items.len()
    )?;
    array_body(&mut out, &minimum_attack_charge);
    writeln!(out, "];")?;
    write!(
        out,
        "static ITEM_USE_EFFECTS: [(bool, bool, f32); {}] = [",
        items.len()
    )?;
    array_body(&mut out, &use_effects);
    writeln!(out, "];")?;
    write!(out, "static ITEM_JUKEBOX_SONG: [u16; {}] = [", items.len())?;
    array_body(&mut out, &jukebox_song);
    writeln!(out, "];")?;
    write!(out, "static ITEM_JUKEBOX_OUTPUT: [u8; {}] = [", items.len())?;
    array_body(&mut out, &jukebox);
    writeln!(out, "];")?;
    write!(
        out,
        "static JUKEBOX_SONG_OUTPUT: [(&str, u8, u32); {}] = [",
        songs.len()
    )?;
    array_body(
        &mut out,
        songs
            .iter()
            .map(|(name, output, ticks)| format!("({}, {output}, {ticks})", str_literal(name))),
    );
    writeln!(out, "];")?;
    write!(
        out,
        "static ITEM_DAMAGE_TYPE: [Option<&str>; {}] = [",
        items.len()
    )?;
    array_body(&mut out, &item_damage_type);
    writeln!(out, "];")?;
    Ok(out)
}

fn emit_enum(out: &mut String, name: &str, values: &[&str]) -> Result<()> {
    writeln!(
        out,
        "#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]\npub enum {name} {{"
    )?;
    for value in values {
        writeln!(out, "    #[doc = \"`{value}`\"]\n    {},", pascal(value))?;
    }
    writeln!(out, "}}")?;
    writeln!(out, "impl {name} {{")?;
    write!(out, "    pub const ALL: [Self; {}] = [", values.len())?;
    array_body(out, values.iter().map(|v| format!("Self::{}", pascal(v))));
    writeln!(out, "    ];")?;
    write!(out, "    const NAMES: [&'static str; {}] = [", values.len())?;
    array_body(out, values.iter().map(|v| str_literal(v)));
    writeln!(out, "    ];")?;
    writeln!(
        out,
        "    pub const fn name(self) -> &'static str {{ Self::NAMES[self as usize] }}\n\
         \x20   pub fn from_name(name: &str) -> Option<Self> {{\n\
         \x20       Self::NAMES.iter().position(|n| *n == name).map(|i| Self::ALL[i])\n\
         \x20   }}\n}}"
    )?;
    Ok(())
}

fn consume_effect(
    value: &Value,
    effects: &std::collections::BTreeMap<&str, String>,
    effect_tags: &super::tags::Tags,
    sound: &dyn Fn(&Value) -> Result<String>,
) -> Result<String> {
    let object = value
        .as_object()
        .context("consume effect is not an object")?;
    let kind = object
        .get("type")
        .and_then(Value::as_str)
        .context("consume effect without type")?;
    Ok(match kind {
        "minecraft:apply_effects" => {
            let fields = fields(value, &["type", "effects", "probability"])?;
            let mut instances = Vec::new();
            for instance in fields
                .get("effects")
                .and_then(Value::as_array)
                .context("apply_effects without effects")?
            {
                let f = self::fields(instance, &EFFECT_FIELDS)?;
                let id = f["id"].as_str().context("bad effect id")?;
                let variant = effects
                    .get(id)
                    .with_context(|| format!("unknown mob effect {id}"))?;
                instances.push(format!(
                    "EffectInstance {{ effect: MobEffect::{variant}, duration: {}, amplifier: {}, \
                     ambient: {}, show_particles: {}, show_icon: {} }}",
                    integer_signed(f.get("duration"), Some(0))?,
                    integer(f.get("amplifier"), Some(0))?,
                    boolean(f.get("ambient"), false)?,
                    boolean(f.get("show_particles"), true)?,
                    boolean(f.get("show_icon"), true)?,
                ));
            }
            format!(
                "ConsumeEffect::ApplyEffects {{ effects: &[{}], probability: {} }}",
                instances.join(", "),
                float(fields.get("probability"), Some("1.0"))?
            )
        }
        "minecraft:remove_effects" => {
            let fields = fields(value, &["type", "effects"])?;
            let mut names = Vec::new();
            list_entries(&fields["effects"], effect_tags, &mut names)?;
            let variants: Vec<String> = names
                .iter()
                .map(|name| {
                    effects
                        .get(name.as_str())
                        .map(|v| format!("MobEffect::{v}"))
                        .with_context(|| format!("unknown mob effect {name}"))
                })
                .collect::<Result<_>>()?;
            format!("ConsumeEffect::RemoveEffects(&[{}])", variants.join(", "))
        }
        "minecraft:clear_all_effects" => {
            fields(value, &["type"])?;
            "ConsumeEffect::ClearAllEffects".to_string()
        }
        "minecraft:teleport_randomly" => {
            let fields = fields(value, &["type", "diameter", "directional_particles"])?;
            format!(
                "ConsumeEffect::TeleportRandomly {{ diameter: {}, directional_particles: {} }}",
                float(fields.get("diameter"), Some("16.0"))?,
                fields
                    .get("directional_particles")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true)
            )
        }
        "minecraft:play_sound" => {
            let fields = fields(value, &["type", "sound"])?;
            format!("ConsumeEffect::PlaySound({})", sound(&fields["sound"])?)
        }
        other => bail!("unknown consume effect {other}"),
    })
}

/// Resolves a registry entry list: one id, `#tag`, or an array of either.
fn list_entries(value: &Value, tags: &super::tags::Tags, out: &mut Vec<String>) -> Result<()> {
    match value {
        Value::String(entry) => match entry.strip_prefix('#') {
            Some(tag) => out.extend(
                tags.get(tag)
                    .with_context(|| format!("unknown tag {entry}"))?
                    .iter()
                    .cloned(),
            ),
            None => out.push(entry.clone()),
        },
        Value::Array(entries) => {
            for entry in entries {
                list_entries(entry, tags, out)?;
            }
        }
        _ => bail!("bad entry list {value}"),
    }
    Ok(())
}

fn fields<'v>(value: &'v Value, known: &[&str]) -> Result<&'v Map<String, Value>> {
    let object = value
        .as_object()
        .with_context(|| format!("expected an object, got {value}"))?;
    for key in object.keys() {
        ensure!(known.contains(&key.as_str()), "unknown field {key:?}");
    }
    Ok(object)
}

fn boolean(value: Option<&Value>, default: bool) -> Result<bool> {
    match value {
        None => Ok(default),
        Some(value) => value
            .as_bool()
            .with_context(|| format!("expected a boolean, got {value}")),
    }
}

fn integer(value: Option<&Value>, default: Option<u64>) -> Result<u64> {
    match (value, default) {
        (None, Some(default)) => Ok(default),
        (None, None) => bail!("missing integer"),
        (Some(value), _) => value
            .as_u64()
            .with_context(|| format!("expected a non-negative integer, got {value}")),
    }
}

fn integer_signed(value: Option<&Value>, default: Option<i64>) -> Result<i64> {
    match (value, default) {
        (None, Some(default)) => Ok(default),
        (None, None) => bail!("missing integer"),
        (Some(value), _) => value
            .as_i64()
            .with_context(|| format!("expected an integer, got {value}")),
    }
}

/// A single-precision float literal, as the game reads the value.
fn float(value: Option<&Value>, default: Option<&str>) -> Result<String> {
    match (value, default) {
        (None, Some(default)) => Ok(format!("{default}_f32")),
        (None, None) => bail!("missing number"),
        (Some(value), _) => {
            let value = value
                .as_f64()
                .with_context(|| format!("expected a number, got {value}"))?
                as f32;
            ensure!(value.is_finite(), "non-finite float");
            Ok(format!("{value:?}_f32"))
        }
    }
}

fn double(value: Option<&Value>) -> Result<String> {
    let value = value.context("missing number")?;
    let value = value
        .as_f64()
        .with_context(|| format!("expected a number, got {value}"))?;
    ensure!(value.is_finite(), "non-finite number");
    Ok(format!("{value:?}_f64"))
}
