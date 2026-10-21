//! Entities (in entity chunks, spawners, items and other entities) and
//! player files.

use super::{
    Fixer, block_entity, chunk, fix_block_pos, for_each_compound, get_bool, get_number, id_of,
    inline_block_pos, item, rename, rename_id, rename_with, text,
};
use tuff_nbt::{Compound, Tag};

const EFFECT_IDS: [&str; 33] = [
    "speed",
    "slowness",
    "haste",
    "mining_fatigue",
    "strength",
    "instant_health",
    "instant_damage",
    "jump_boost",
    "nausea",
    "regeneration",
    "resistance",
    "fire_resistance",
    "water_breathing",
    "invisibility",
    "blindness",
    "night_vision",
    "hunger",
    "weakness",
    "poison",
    "wither",
    "health_boost",
    "absorption",
    "saturation",
    "glowing",
    "levitation",
    "luck",
    "unluck",
    "slow_falling",
    "conduit_power",
    "dolphins_grace",
    "bad_omen",
    "hero_of_the_village",
    "darkness",
];
const CAT_VARIANTS: [&str; 11] = [
    "minecraft:tabby",
    "minecraft:black",
    "minecraft:red",
    "minecraft:siamese",
    "minecraft:british",
    "minecraft:calico",
    "minecraft:persian",
    "minecraft:ragdoll",
    "minecraft:white",
    "minecraft:jellie",
    "minecraft:all_black",
];
const FROG_VARIANTS: [&str; 3] = ["minecraft:temperate", "minecraft:warm", "minecraft:cold"];
const PATROLLING_MOBS: [&str; 6] = [
    "minecraft:witch",
    "minecraft:ravager",
    "minecraft:pillager",
    "minecraft:illusioner",
    "minecraft:evoker",
    "minecraft:vindicator",
];
const SADDLE_ITEM_MOBS: [&str; 8] = [
    "minecraft:horse",
    "minecraft:skeleton_horse",
    "minecraft:zombie_horse",
    "minecraft:donkey",
    "minecraft:mule",
    "minecraft:camel",
    "minecraft:llama",
    "minecraft:trader_llama",
];
const CHESTED_HORSES: [&str; 4] = [
    "minecraft:llama",
    "minecraft:trader_llama",
    "minecraft:mule",
    "minecraft:donkey",
];
const BLOCK_ATTACHED: [&str; 4] = [
    "minecraft:item_frame",
    "minecraft:glow_item_frame",
    "minecraft:painting",
    "minecraft:leash_knot",
];
/// `(entity, base before, base after)` of the follow range corrections.
const FOLLOW_RANGE_FIXES: [(&str, f64, f64); 6] = [
    ("minecraft:villager", 48.0, 16.0),
    ("minecraft:bee", 48.0, 16.0),
    ("minecraft:allay", 48.0, 16.0),
    ("minecraft:llama", 40.0, 16.0),
    ("minecraft:piglin_brute", 16.0, 12.0),
    ("minecraft:warden", 16.0, 24.0),
];
const DEFAULT_DROP_CHANCE: f32 = 0.085;
const ARMOR_SLOTS: [&str; 4] = ["feet", "legs", "chest", "head"];
const HAND_SLOTS: [&str; 2] = ["mainhand", "offhand"];
/// Single items an entity may hold, by key.
const ITEM_FIELDS: [&str; 12] = [
    "Item",
    "Trident",
    "item",
    "weapon",
    "FireworksItem",
    "SaddleItem",
    "ArmorItem",
    "DecorItem",
    "body_armor_item",
    "saddle",
    "Book",
    "item_stack",
];
const ITEM_LISTS: [&str; 5] = [
    "Items",
    "Inventory",
    "HandItems",
    "ArmorItems",
    "EnderItems",
];
const BLOCK_STATE_FIELDS: [&str; 5] = [
    "BlockState",
    "carriedBlockState",
    "block_state",
    "DisplayState",
    "inBlockState",
];
/// Known attribute modifier UUIDs (as four ints) and the ids that replaced
/// them.
const MODIFIER_IDS: [(u128, &str); 39] = [
    (
        0x736565d2_e1a7_403d_a3f8_1aeb3e302542,
        "minecraft:creative_mode_block_range",
    ),
    (
        0x98491ef6_97b1_4584_ae82_71a8cc85cf73,
        "minecraft:creative_mode_entity_range",
    ),
    (
        0x91aeaa56_376b_4498_935b_2f7f68070635,
        "minecraft:effect.speed",
    ),
    (
        0x7107de5e_7ce8_4030_940e_514c1f160890,
        "minecraft:effect.slowness",
    ),
    (
        0xaf8b6e3f_3328_4c0a_aa36_5ba2bb9dbef3,
        "minecraft:effect.haste",
    ),
    (
        0x55fced67_e92a_486e_9800_b47f202c4386,
        "minecraft:effect.mining_fatigue",
    ),
    (
        0x648d7064_6a60_4f59_8abe_c2c23a6dd7a9,
        "minecraft:effect.strength",
    ),
    (
        0xc0105bf3_aef8_46b0_9ebc_92943757ccbe,
        "minecraft:effect.jump_boost",
    ),
    (
        0x22653b89_116e_49dc_9b6b_9971489b5be5,
        "minecraft:effect.weakness",
    ),
    (
        0x5d6f0ba2_1186_46ac_b896_c61c5cee99cc,
        "minecraft:effect.health_boost",
    ),
    (
        0xeae29cf0_701e_4ed6_883a_96f798f3dab5,
        "minecraft:effect.absorption",
    ),
    (
        0x03c3c89d_7037_4b42_869f_b146bcb64d2e,
        "minecraft:effect.luck",
    ),
    (
        0xcc5af142_2bd2_4215_b636_2605aed11727,
        "minecraft:effect.unluck",
    ),
    (0x6555be74_63b3_41f1_a245_77833b3c2562, "minecraft:evil"),
    (
        0x1eaf83ff_7207_4596_b37a_d7a07b3ec4ce,
        "minecraft:powder_snow",
    ),
    (
        0x662a6b8d_da3e_4c1c_8813_96ea6097278d,
        "minecraft:sprinting",
    ),
    (
        0x020e0dfb_87ae_4653_9556_831010e291a0,
        "minecraft:attacking",
    ),
    (0x766bfa64_11f3_11ea_8d71_362b9e155667, "minecraft:baby"),
    (0x7e0292f2_9434_48d5_a29f_9583af7df27f, "minecraft:covered"),
    (
        0x9e362924_01de_4ddd_a2b2_d0f7a405a174,
        "minecraft:suffocating",
    ),
    (0x5cd17e52_a79a_43d3_a529_90fde04b181e, "minecraft:drinking"),
    (0xb9766b59_9566_4402_bc1f_2ee2a276d836, "minecraft:baby"),
    (
        0x49455a49_7ec5_45ba_b886_3b90b23a1718,
        "minecraft:attacking",
    ),
    (
        0x845db27c_c624_495f_8c9f_6020a9a58b6b,
        "minecraft:armor.boots",
    ),
    (
        0xd8499b04_0e66_4726_ab29_64469d734e0d,
        "minecraft:armor.leggings",
    ),
    (
        0x9f3d476d_c118_4544_8365_64846904b48e,
        "minecraft:armor.chestplate",
    ),
    (
        0x2ad3f246_fee1_4e67_b886_69fd380bb150,
        "minecraft:armor.helmet",
    ),
    (
        0xc1c72771_8b8e_ba4a_ace0_81a93c8928b2,
        "minecraft:armor.body",
    ),
    (
        0xb572ecd2_ac0c_4071_abde_9594af072a37,
        "minecraft:enchantment.fire_protection",
    ),
    (
        0x40a9968f_5c66_4e2f_b7f4_2ec2f4b3e450,
        "minecraft:enchantment.blast_protection",
    ),
    (
        0x07a65791_f64d_4e79_86c7_f83932f007ec,
        "minecraft:enchantment.respiration",
    ),
    (
        0x60b1b7db_fffd_4ad0_817c_d6c6a93d8a45,
        "minecraft:enchantment.aqua_affinity",
    ),
    (
        0x11dc269a_4476_46c0_aff3_9e17d7eb6801,
        "minecraft:enchantment.depth_strider",
    ),
    (
        0x87f46a96_686f_4796_b035_22e16ee9e038,
        "minecraft:enchantment.soul_speed",
    ),
    (
        0xb9716dbd_50df_4080_850e_70347d24e687,
        "minecraft:enchantment.soul_speed",
    ),
    (
        0x92437d00_c3a7_4f2e_8f6c_1f21585d5dd0,
        "minecraft:enchantment.swift_sneak",
    ),
    (
        0x5d3d087b_debe_4037_b53e_d84f3ff51f17,
        "minecraft:enchantment.sweeping_edge",
    ),
    (
        0x3ceb37c0_db62_46b5_bd02_785457b01d96,
        "minecraft:enchantment.efficiency",
    ),
    (
        0xcb3f55d3_645c_4f38_a497_9c13a33db5cf,
        "minecraft:base_attack_damage",
    ),
];
const BASE_ATTACK_SPEED: (u128, &str) = (
    0xfa233e1c_4180_4865_b01b_bcce9785aca3,
    "minecraft:base_attack_speed",
);
const MODIFIER_NAMES: [(&str, &str); 5] = [
    ("Random spawn bonus", "minecraft:random_spawn_bonus"),
    (
        "Random zombie-spawn bonus",
        "minecraft:zombie_random_spawn_bonus",
    ),
    ("Leader zombie bonus", "minecraft:leader_zombie_bonus"),
    (
        "Zombie reinforcement callee charge",
        "minecraft:reinforcement_callee_charge",
    ),
    (
        "Zombie reinforcement caller charge",
        "minecraft:reinforcement_caller_charge",
    ),
];
const ATTRIBUTE_PREFIXES: [&str; 4] = ["generic.", "horse.", "player.", "zombie."];
/// Player inventory list slots that 1.21.5 moved into `equipment`.
const PLAYER_EQUIPMENT_SLOTS: [(i32, &str); 5] = [
    (100, "feet"),
    (101, "legs"),
    (102, "chest"),
    (103, "head"),
    (-106, "offhand"),
];

/// Upgrades the entity at `key`, if present.
pub fn upgrade_field(compound: &mut Compound, key: &str, fixer: Fixer) {
    if let Some(Tag::Compound(entity)) = compound.get_mut(key) {
        upgrade(entity, fixer);
    }
}

pub fn upgrade(entity: &mut Compound, fixer: Fixer) {
    if !fixer.pending() {
        return;
    }
    super::pre2860::entity(entity, fixer);
    pre_nested(entity, fixer);
    upgrade_nested(entity, fixer);
    let id = id_of(entity);
    living(entity, fixer, &id);
    by_kind(entity, fixer, &id);
}

pub fn upgrade_player(player: &mut Compound, fixer: Fixer) {
    if !fixer.pending() {
        return;
    }
    super::pre2860::player(player, fixer);
    upgrade_nested(player, fixer);
    living(player, fixer, "minecraft:player");
    if fixer.due(4312, 0) {
        let mut equipment = Compound::new();
        if let Some(Tag::List(inventory)) = player.remove("Inventory") {
            let mut kept = Vec::with_capacity(inventory.len());
            for tag in inventory {
                let slot = tag
                    .as_compound()
                    .and_then(|item| get_number(item, "Slot"))
                    .map(|v| v as i32);
                match slot.and_then(|slot| PLAYER_EQUIPMENT_SLOTS.iter().find(|(s, _)| *s == slot))
                {
                    Some((_, name)) => {
                        if let Tag::Compound(mut item) = tag {
                            item.remove("Slot");
                            equipment.insert(*name, item);
                        }
                    }
                    None => kept.push(tag),
                }
            }
            player.insert("Inventory", kept);
        }
        player.insert("equipment", equipment);
    }
    if fixer.due(4314, 0) {
        let respawn = match (
            get_number(player, "SpawnX"),
            get_number(player, "SpawnY"),
            get_number(player, "SpawnZ"),
        ) {
            (Some(x), Some(y), Some(z)) => {
                let mut respawn = Compound::new().with("pos", vec![x as i32, y as i32, z as i32]);
                for (from, to) in [
                    ("SpawnAngle", "angle"),
                    ("SpawnDimension", "dimension"),
                    ("SpawnForced", "forced"),
                ] {
                    if let Some(value) = player.get(from) {
                        respawn.insert(to, value.clone());
                    }
                }
                Some(respawn)
            }
            _ => None,
        };
        if let Some(respawn) = respawn {
            for key in [
                "SpawnX",
                "SpawnY",
                "SpawnZ",
                "SpawnAngle",
                "SpawnDimension",
                "SpawnForced",
            ] {
                player.remove(key);
            }
            player.insert("respawn", respawn);
        }
        if let Some(Tag::Compound(pos)) = player.remove("enteredNetherPosition") {
            player.insert(
                "entered_nether_pos",
                vec![
                    Tag::Double(get_number(&pos, "x").unwrap_or(0.0)),
                    Tag::Double(get_number(&pos, "y").unwrap_or(0.0)),
                    Tag::Double(get_number(&pos, "z").unwrap_or(0.0)),
                ],
            );
        }
    }
    if fixer.due(4424, 0) {
        add_locator_bar_style(player);
    }
    if fixer.due(4548, 0)
        && let Some(respawn) = player.get_compound_mut("respawn")
    {
        if !matches!(respawn.get("dimension"), Some(Tag::String(_))) {
            respawn.insert("dimension", "minecraft:overworld");
        }
        let angle = respawn
            .remove("angle")
            .as_ref()
            .and_then(super::number)
            .unwrap_or(0.0);
        respawn.insert("yaw", angle as f32);
        respawn.insert("pitch", 0.0f32);
    }
    if fixer.due(4769, 0)
        && let Some(book) = player.get_compound_mut("recipeBook")
    {
        for key in ["recipes", "toBeDisplayed"] {
            if let Some(list) = book.get_list_mut(key) {
                for recipe in list {
                    if let Tag::String(name) = recipe
                        && let Some(new) = super::level::renamed_stonecutter_recipe(name)
                    {
                        *name = new;
                    }
                }
            }
        }
    }
}

/// Entity fixes that rewrite a held item in its legacy form, so they run
/// before the held items themselves are upgraded.
fn pre_nested(entity: &mut Compound, fixer: Fixer) {
    let id = id_of(entity);
    if fixer.due(3685, 0) && !entity.contains_key("item") {
        let kind = match id.as_str() {
            "minecraft:arrow" => Some(
                if entity
                    .get_str("Potion")
                    .is_none_or(|p| p == "minecraft:empty")
                {
                    "minecraft:arrow"
                } else {
                    "minecraft:tipped_arrow"
                },
            ),
            "minecraft:spectral_arrow" => Some("minecraft:spectral_arrow"),
            _ => None,
        };
        if let Some(kind) = kind {
            entity.insert("item", Compound::new().with("id", kind).with("Count", 1));
        }
    }
    if id == "minecraft:arrow" {
        if fixer.due(3322, 0) {
            for_each_compound(entity, "CustomPotionEffects", fix_effect_duration);
        }
        if fixer.due(3568, 0) {
            update_effect_list(entity, "CustomPotionEffects", "custom_potion_effects");
        }
    }
    if fixer.due(3818, 2) && id == "minecraft:arrow" {
        let potion = entity.remove("Potion");
        let effects = entity.remove("custom_potion_effects");
        let color = entity.remove("Color");
        if potion.is_some() || effects.is_some() || color.is_some() {
            let item = match entity.get_mut("item") {
                Some(Tag::Compound(item)) => item,
                _ => {
                    entity.insert("item", Compound::new());
                    entity.get_compound_mut("item").expect("inserted")
                }
            };
            if !item.contains_key("tag") {
                item.insert("tag", Compound::new());
            }
            let tag = item.get_compound_mut("tag").expect("inserted");
            for (value, key) in [
                (potion, "Potion"),
                (effects, "custom_potion_effects"),
                (color, "CustomPotionColor"),
            ] {
                if let Some(value) = value {
                    tag.insert(key, value);
                }
            }
        }
    }
}

fn empty_trade_cost(offer: &Compound) -> bool {
    let Some(buy_b) = offer.get_compound("buyB") else {
        return true;
    };
    let air = buy_b
        .get_str("id")
        .is_none_or(|id| super::namespaced(id) == "minecraft:air");
    let count = buy_b.get("count").and_then(super::number).unwrap_or(0.0);
    air || count == 0.0
}

fn upgrade_nested(entity: &mut Compound, fixer: Fixer) {
    let Some(inner) = fixer.nested() else {
        return;
    };
    for key in ITEM_FIELDS {
        item::upgrade_field(entity, key, inner);
    }
    for key in ITEM_LISTS {
        item::upgrade_list(entity, key, inner);
    }
    if let Some(equipment) = entity.get_compound_mut("equipment") {
        let slots: Vec<String> = equipment.keys().map(str::to_owned).collect();
        for slot in slots {
            item::upgrade_field(equipment, &slot, inner);
        }
    }
    if let Some(offers) = entity.get_compound_mut("Offers") {
        for_each_compound(offers, "Recipes", |offer| {
            for key in ["buy", "buyB", "sell"] {
                item::upgrade_field(offer, key, inner);
            }
            if fixer.due(3828, 0) && empty_trade_cost(offer) {
                offer.remove("buyB");
            }
        });
    }
    for_each_compound(entity, "Passengers", |passenger| upgrade(passenger, inner));
    for key in ["ShoulderEntityLeft", "ShoulderEntityRight"] {
        upgrade_field(entity, key, inner);
    }
    if let Some(vehicle) = entity.get_compound_mut("RootVehicle") {
        upgrade_field(vehicle, "Entity", inner);
    }
    if let Some(Tag::Compound(data)) = entity.get_mut("TileEntityData") {
        block_entity::upgrade(data, inner);
    }
    for key in BLOCK_STATE_FIELDS {
        if let Some(state) = entity.get_mut(key) {
            chunk::fix_block_state(state, fixer);
        }
    }
    if id_of(entity) == "minecraft:area_effect_cloud" {
        for key in ["Particle", "custom_particle"] {
            if let Some(Tag::Compound(particle)) = entity.get_mut(key) {
                item::upgrade_field(particle, "item", inner.at_least(3818));
                if let Some(state) = particle.get_mut("block_state") {
                    chunk::fix_block_state(state, fixer.at_least(3818));
                }
            }
        }
    }
}

/// Fixes every entity (and the player) may need.
fn living(entity: &mut Compound, fixer: Fixer, id: &str) {
    if fixer.due(3322, 0) {
        for key in ["Effects", "ActiveEffects", "CustomPotionEffects"] {
            for_each_compound(entity, key, fix_effect_duration);
        }
    }
    if fixer.due(3568, 0) {
        match id {
            "minecraft:mooshroom" => {
                let mut entry = Compound::new();
                if let Some(effect) = entity.get("EffectId").and_then(effect_id_name) {
                    entry.insert("id", effect);
                }
                if let Some(duration) = entity.get("EffectDuration") {
                    entry.insert("duration", duration.clone());
                }
                if !entry.is_empty() {
                    entity.insert("stew_effects", vec![entry]);
                }
                entity.remove("EffectId");
                entity.remove("EffectDuration");
            }
            "minecraft:arrow" => {
                update_effect_list(entity, "CustomPotionEffects", "custom_potion_effects")
            }
            "minecraft:area_effect_cloud" => update_effect_list(entity, "Effects", "effects"),
            _ => {}
        }
        update_effect_list(entity, "ActiveEffects", "active_effects");
    }
    if fixer.due(3813, 0) {
        rename_with(entity, "Leash", "leash", |pos| Some(fix_block_pos(pos)));
        let fields: &[(&str, &str)] = match id {
            "minecraft:bee" => &[("HivePos", "hive_pos"), ("FlowerPos", "flower_pos")],
            "minecraft:end_crystal" => &[("BeamTarget", "beam_target")],
            "minecraft:wandering_trader" => &[("WanderTarget", "wander_target")],
            _ if PATROLLING_MOBS.contains(&id) => &[("PatrolTarget", "patrol_target")],
            _ => &[],
        };
        for (from, to) in fields {
            rename_with(entity, from, to, |pos| Some(fix_block_pos(pos)));
        }
    }
    if fixer.due(3814, 0) {
        for_each_compound(entity, "Attributes", |attribute| {
            rename_id(attribute, "Name", item::JUMP_STRENGTH_RENAMES)
        });
    }
    if fixer.due(3812, 0) && id == "minecraft:wolf" {
        fix_wolf_health(entity);
    }
    if fixer.due(3945, 0)
        && let Some(Tag::List(attributes)) = entity.remove("Attributes")
    {
        let fixed: Vec<Tag> = attributes
            .into_iter()
            .map(|attribute| match attribute {
                Tag::Compound(attribute) => Tag::Compound(fix_attribute(attribute)),
                other => other,
            })
            .collect();
        entity.insert("attributes", fixed);
    }
    if fixer.due(4055, 0) {
        rename_attribute_ids(entity, strip_attribute_prefix);
    }
    if fixer.due(4187, 0)
        && let Some(&(_, before, after)) = FOLLOW_RANGE_FIXES.iter().find(|(e, _, _)| *e == id)
    {
        for_each_compound(entity, "attributes", |attribute| {
            if attribute.get_str("id").map(super::namespaced).as_deref()
                == Some("minecraft:follow_range")
            {
                let base = get_number(attribute, "base").unwrap_or(0.0);
                if base == before {
                    attribute.insert("base", after);
                }
            }
        });
    }
    text::upgrade_field(entity, "CustomName", fixer);
    if fixer.due(4293, 0) && id != "minecraft:player" {
        fix_drop_chances(entity);
    }
    if fixer.due(4300, 0) {
        fix_saddle(entity, id);
    }
    if fixer.due(4301, 0) && id != "minecraft:player" {
        fix_equipment_format(entity);
    }
    if fixer.due(4303, 0) {
        rename_with(entity, "FallDistance", "fall_distance", |distance| {
            Some(Tag::Double(f64::from(
                super::number(&distance).unwrap_or(0.0) as f32,
            )))
        });
    }
    if fixer.due(4314, 0) {
        inline_block_pos(
            entity,
            "SleepingX",
            "SleepingY",
            "SleepingZ",
            "sleeping_pos",
        );
    }
    if fixer.due(4424, 0) && id != "minecraft:player" {
        add_locator_bar_style(entity);
    }
    if fixer.due(4888, 0) {
        rename_attribute_ids(entity, |name| {
            (name == "minecraft:nameplate_distance").then(|| "minecraft:name_tag_distance".into())
        });
    }
}

fn by_kind(entity: &mut Compound, fixer: Fixer, id: &str) {
    if matches!(id, "minecraft:warden" | "minecraft:allay") {
        super::rename_vibration_game_event(entity, fixer);
    }
    if fixer.due(3086, 0) && id == "minecraft:cat" {
        int_variant(entity, "CatType", |v| {
            CAT_VARIANTS.get(v).copied().unwrap_or("minecraft:tabby")
        });
    }
    if fixer.due(3087, 0) && id == "minecraft:frog" {
        let variant = entity.get("Variant").and_then(super::number);
        if let Some(variant) = variant
            && let Some(name) = usize::try_from(variant as i32)
                .ok()
                .and_then(|i| FROG_VARIANTS.get(i))
        {
            entity.insert("variant", *name);
        }
    }
    if fixer.due(3090, 0) && id == "minecraft:painting" {
        rename(entity, "Motive", "variant");
        rename(entity, "Facing", "facing");
    }
    if fixer.due(3093, 0) && id == "minecraft:goat" {
        entity.insert("HasLeftHorn", true);
        entity.insert("HasRightHorn", true);
    }
    if fixer.due(3097, 0)
        && id == "minecraft:cat"
        && entity.get_str("variant") == Some("minecraft:british")
    {
        entity.insert("variant", "minecraft:british_shorthair");
    }
    if fixer.due(3683, 0) && id == "minecraft:tnt" {
        let mut state = Tag::Compound(Compound::new().with("Name", "minecraft:tnt"));
        chunk::fix_block_state(&mut state, fixer);
        entity.insert("block_state", state);
        if let Some(fuse) = entity.get("Fuse").cloned() {
            entity.insert("fuse", fuse);
        }
    }
    if fixer.due(3808, 0) && id == "minecraft:horse" {
        move_body_armor(entity, "ArmorItem", true);
    }
    if fixer.due(3808, 1) && id == "minecraft:llama" {
        move_body_armor(entity, "DecorItem", false);
    }
    if fixer.due(3808, 2) && id == "minecraft:trader_llama" {
        move_body_armor(entity, "DecorItem", false);
    }
    if fixer.due(3809, 0) && CHESTED_HORSES.contains(&id) {
        for_each_compound(entity, "Items", |item| {
            let slot = get_number(item, "Slot").map_or(2, |v| v as i32);
            item.insert("Slot", (slot - 2) as i8);
        });
    }
    if fixer.due(3818, 6) && id == "minecraft:area_effect_cloud" {
        let color = entity.remove("Color");
        let effects = entity.remove("effects");
        let potion = entity.remove("Potion");
        if color.is_some() || effects.is_some() || potion.is_some() {
            let mut contents = Compound::new();
            for (value, key) in [
                (color, "custom_color"),
                (effects, "custom_effects"),
                (potion, "potion"),
            ] {
                if let Some(value) = value {
                    contents.insert(key, value);
                }
            }
            entity.insert("potion_contents", contents);
        }
    }
    if fixer.due(4067, 0) && matches!(id, "minecraft:boat" | "minecraft:chest_boat") {
        let chest = id == "minecraft:chest_boat";
        let wood = match entity.remove("Type") {
            Some(Tag::String(wood)) => wood,
            _ => String::new(),
        };
        entity.insert("id", boat_id(&wood, chest));
    }
    if fixer.due(4081, 0) && id == "minecraft:salmon" && entity.get_str("type") != Some("large") {
        entity.insert("type", "medium");
    }
    if fixer.due(4173, 0) && id == "minecraft:tnt_minecart" {
        rename(entity, "TNTFuse", "fuse");
    }
    if fixer.due(4296, 0) && id == "minecraft:area_effect_cloud" {
        entity.insert("potion_duration_scale", 0.25f32);
    }
    if fixer.due(4306, 0) && id == "minecraft:potion" {
        let lingering = entity
            .get_compound("Item")
            .is_some_and(|item| id_of(item) == "minecraft:lingering_potion");
        entity.insert(
            "id",
            if lingering {
                "minecraft:lingering_potion"
            } else {
                "minecraft:splash_potion"
            },
        );
    }
    if fixer.due(4314, 0) {
        match id {
            "minecraft:vex" => {
                rename(entity, "LifeTicks", "life_ticks");
                inline_block_pos(entity, "BoundX", "BoundY", "BoundZ", "bound_pos");
            }
            "minecraft:phantom" => {
                rename(entity, "Size", "size");
                inline_block_pos(entity, "AX", "AY", "AZ", "anchor_pos");
            }
            "minecraft:turtle" => {
                for key in ["TravelPosX", "TravelPosY", "TravelPosZ"] {
                    entity.remove(key);
                }
                inline_block_pos(entity, "HomePosX", "HomePosY", "HomePosZ", "home_pos");
                rename(entity, "HasEgg", "has_egg");
            }
            _ if BLOCK_ATTACHED.contains(&id) => {
                inline_block_pos(entity, "TileX", "TileY", "TileZ", "block_pos");
            }
            _ => {}
        }
    }
    if fixer.due(4535, 0)
        && id == "minecraft:copper_golem"
        && let Some(state) = entity.get("weather_state").and_then(super::number)
    {
        let name = match state as i32 {
            1 => "exposed",
            2 => "weathered",
            3 => "oxidized",
            _ => "unaffected",
        };
        entity.insert("weather_state", name);
    }
    if fixer.due(4763, 0) && matches!(id, "minecraft:villager" | "minecraft:zombie_villager") {
        entity.insert("VillagerDataFinalized", true);
    }
}

fn int_variant(entity: &mut Compound, key: &str, name: impl Fn(usize) -> &'static str) {
    if let Some(value) = entity.get(key).and_then(super::number) {
        let index = usize::try_from(value as i32).unwrap_or(0);
        entity.insert("variant", name(index));
    } else if let Some(value) = entity.get(key).cloned() {
        entity.insert("variant", value);
    }
}

fn boat_id(wood: &str, chest: bool) -> String {
    let wood = match wood {
        "spruce" | "birch" | "jungle" | "acacia" | "cherry" | "dark_oak" | "mangrove" => wood,
        "bamboo" => {
            return if chest {
                "minecraft:bamboo_chest_raft".into()
            } else {
                "minecraft:bamboo_raft".into()
            };
        }
        _ => "oak",
    };
    if chest {
        format!("minecraft:{wood}_chest_boat")
    } else {
        format!("minecraft:{wood}_boat")
    }
}

fn move_body_armor(entity: &mut Compound, key: &str, clear_armor_items: bool) {
    let Some(armor) = entity.remove(key) else {
        return;
    };
    if clear_armor_items {
        if let Some(items) = entity.get_list_mut("ArmorItems")
            && let Some(slot) = items.get_mut(2)
        {
            *slot = Tag::Compound(Compound::new());
        }
        if let Some(chances) = entity.get_list_mut("ArmorDropChances")
            && let Some(slot) = chances.get_mut(2)
        {
            *slot = Tag::Float(DEFAULT_DROP_CHANCE);
        }
    }
    entity.insert("body_armor_item", armor);
    entity.insert("body_armor_drop_chance", 2.0f32);
}

fn fix_wolf_health(entity: &mut Compound) {
    let mut adjusted = false;
    for_each_compound(entity, "Attributes", |attribute| {
        if attribute.get_str("Name").map(super::namespaced).as_deref()
            == Some("minecraft:generic.max_health")
            && get_number(attribute, "Base") == Some(20.0)
        {
            attribute.insert("Base", 40.0f64);
            adjusted = true;
        }
    });
    if adjusted {
        let health = get_number(entity, "Health").unwrap_or(0.0) as f32;
        entity.insert("Health", health * 2.0);
    }
}

fn fix_attribute(mut attribute: Compound) -> Compound {
    rename(&mut attribute, "Name", "id");
    rename(&mut attribute, "Base", "base");
    if let Some(Tag::List(modifiers)) = attribute.remove("Modifiers") {
        let converted: Vec<Tag> = modifiers
            .into_iter()
            .map(|modifier| match modifier {
                Tag::Compound(modifier) => Tag::Compound(convert_entity_modifier(modifier)),
                other => other,
            })
            .collect();
        attribute.insert("modifiers", fix_modifier_ids(converted));
    }
    attribute
}

fn convert_entity_modifier(mut modifier: Compound) -> Compound {
    rename(&mut modifier, "UUID", "uuid");
    rename(&mut modifier, "Name", "name");
    rename(&mut modifier, "Amount", "amount");
    rename_with(&mut modifier, "Operation", "operation", |operation| {
        Some(Tag::from(
            match super::number(&operation).map(|v| v as i32) {
                Some(0) => "add_value",
                Some(1) => "add_multiplied_base",
                Some(2) => "add_multiplied_total",
                _ => "invalid",
            },
        ))
    });
    modifier
}

/// Gives attribute modifiers string ids in place of UUIDs and names,
/// merging the name-keyed ones that shared an id.
pub(crate) fn fix_modifier_ids(modifiers: Vec<Tag>) -> Vec<Tag> {
    let mut out: Vec<(String, Compound)> = Vec::new();
    for modifier in modifiers {
        let Tag::Compound(mut modifier) = modifier else {
            continue;
        };
        let uuid = modifier.get("uuid").and_then(Tag::as_uuid);
        let name = modifier.get_str("name").unwrap_or("").to_owned();
        let from_uuid = uuid.and_then(|uuid| {
            MODIFIER_IDS
                .iter()
                .chain(std::iter::once(&BASE_ATTACK_SPEED))
                .find(|(known, _)| *known == uuid)
                .map(|(_, id)| *id)
        });
        let from_name = MODIFIER_NAMES
            .iter()
            .find(|(known, _)| *known == name)
            .map(|(_, id)| *id);
        modifier.remove("uuid");
        modifier.remove("name");
        let id = match (from_uuid, from_name) {
            (Some(id), _) => id.to_owned(),
            (None, Some(id)) => {
                if let Some((_, existing)) = out.iter_mut().find(|(known, _)| known == id) {
                    let total = get_number(existing, "amount").unwrap_or(0.0)
                        + get_number(&modifier, "amount").unwrap_or(0.0);
                    existing.insert("amount", total);
                    continue;
                }
                id.to_owned()
            }
            (None, None) => match uuid {
                Some(uuid) => format!("minecraft:{}", hyphenated(uuid)),
                None => "minecraft:unknown".to_owned(),
            },
        };
        modifier.insert("id", id.as_str());
        match out.iter_mut().find(|(known, _)| *known == id) {
            Some((_, existing)) => *existing = modifier,
            None => out.push((id, modifier)),
        }
    }
    out.into_iter().map(|(_, m)| Tag::Compound(m)).collect()
}

fn hyphenated(uuid: u128) -> String {
    let hex = format!("{uuid:032x}");
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

pub(crate) fn strip_attribute_prefix(id: &str) -> Option<String> {
    let full = super::namespaced(id);
    ATTRIBUTE_PREFIXES.iter().find_map(|prefix| {
        full.strip_prefix("minecraft:")
            .and_then(|rest| rest.strip_prefix(prefix))
            .map(|rest| format!("minecraft:{rest}"))
    })
}

fn rename_attribute_ids(entity: &mut Compound, rename: impl Fn(&str) -> Option<String>) {
    for_each_compound(entity, "attributes", |attribute| {
        if let Some(Tag::String(id)) = attribute.get_mut("id")
            && let Some(new) = rename(id)
        {
            *id = new;
        }
    });
}

fn fix_drop_chances(entity: &mut Compound) {
    let chances = |entity: &mut Compound, key: &str| -> Vec<f32> {
        match entity.remove(key) {
            Some(Tag::List(list)) => list
                .iter()
                .map(|t| super::number(t).map_or(DEFAULT_DROP_CHANCE, |v| v as f32))
                .collect(),
            _ => Vec::new(),
        }
    };
    let armor = chances(entity, "ArmorDropChances");
    let hands = chances(entity, "HandDropChances");
    let body = entity
        .remove("body_armor_drop_chance")
        .as_ref()
        .and_then(super::number)
        .map_or(DEFAULT_DROP_CHANCE, |v| v as f32);
    let mut slots = Compound::new();
    for (names, values) in [(&ARMOR_SLOTS[..], &armor), (&HAND_SLOTS[..], &hands)] {
        for (name, chance) in names.iter().zip(values.iter()) {
            if *chance != DEFAULT_DROP_CHANCE {
                slots.insert(*name, *chance);
            }
        }
    }
    if body != DEFAULT_DROP_CHANCE {
        slots.insert("body", body);
    }
    if !slots.is_empty() {
        entity.insert("drop_chances", slots);
    }
}

fn fix_saddle(entity: &mut Compound, id: &str) {
    let saddled = if SADDLE_ITEM_MOBS.contains(&id) {
        if !entity.contains_key("SaddleItem") {
            return;
        }
        rename(entity, "SaddleItem", "saddle");
        true
    } else if matches!(id, "minecraft:pig" | "minecraft:strider") {
        let has = get_bool(entity, "Saddle").unwrap_or(false);
        entity.remove("Saddle");
        if has {
            entity.insert(
                "saddle",
                Compound::new()
                    .with("id", "minecraft:saddle")
                    .with("count", 1),
            );
        }
        has
    } else {
        false
    };
    if saddled {
        if !entity.contains_key("drop_chances") {
            entity.insert("drop_chances", Compound::new());
        }
        if let Some(chances) = entity.get_compound_mut("drop_chances") {
            chances.insert("saddle", 2.0f32);
        }
    }
}

fn fix_equipment_format(entity: &mut Compound) {
    let list = |entity: &mut Compound, key: &str| -> Vec<Tag> {
        match entity.remove(key) {
            Some(Tag::List(list)) => list,
            _ => Vec::new(),
        }
    };
    let armor = list(entity, "ArmorItems");
    let hands = list(entity, "HandItems");
    let body = entity.remove("body_armor_item");
    let saddle = entity.remove("saddle");
    let mut equipment = Compound::new();
    let mut put = |slot: &str, item: Option<Tag>| {
        if let Some(Tag::Compound(item)) = item
            && item.contains_key("id")
        {
            equipment.insert(slot, item);
        }
    };
    put("mainhand", hands.first().cloned());
    put("offhand", hands.get(1).cloned());
    for (i, slot) in ARMOR_SLOTS.iter().enumerate() {
        put(slot, armor.get(i).cloned());
    }
    put("body", body);
    put("saddle", saddle);
    if !equipment.is_empty() {
        entity.insert("equipment", equipment);
    }
}

fn add_locator_bar_style(entity: &mut Compound) {
    if let Some(icon) = entity.get_compound_mut("locator_bar_icon") {
        icon.insert("style", "minecraft:default");
    }
}

pub(crate) fn fix_effect_duration(effect: &mut Compound) {
    let duration = get_number(effect, "Duration").map_or(-1, |v| v as i32);
    if let Some(factor) = effect.get_compound_mut("FactorCalculationData") {
        let timestamp = factor
            .remove("effect_changed_timestamp")
            .as_ref()
            .and_then(super::number)
            .map_or(-1, |v| v as i32);
        factor.insert("ticks_active", timestamp - duration);
    }
}

fn effect_id_name(tag: &Tag) -> Option<String> {
    let id = super::number(tag)? as i32;
    let index = usize::try_from(id.checked_sub(1)?).ok()?;
    EFFECT_IDS
        .get(index)
        .map(|name| format!("minecraft:{name}"))
}

/// Replaces a numeric effect id field with the named one under a new key.
pub(crate) fn update_effect_id(compound: &mut Compound, from: &str, to: &str) {
    if let Some(old) = compound.remove(from)
        && let Some(name) = effect_id_name(&old)
    {
        compound.insert(to, name);
    }
}

fn update_effect_instance(effect: &mut Compound) {
    update_effect_id(effect, "Id", "id");
    for (from, to) in [
        ("Ambient", "ambient"),
        ("Amplifier", "amplifier"),
        ("Duration", "duration"),
        ("ShowParticles", "show_particles"),
        ("ShowIcon", "show_icon"),
    ] {
        rename(effect, from, to);
    }
    if let Some(Tag::Compound(mut hidden)) = effect.remove("HiddenEffect") {
        update_effect_instance(&mut hidden);
        effect.insert("hidden_effect", hidden);
    }
}

pub(crate) fn update_effect_list(compound: &mut Compound, from: &str, to: &str) {
    if let Some(Tag::List(list)) = compound.remove(from) {
        let fixed: Vec<Tag> = list
            .into_iter()
            .map(|effect| match effect {
                Tag::Compound(mut effect) => {
                    update_effect_instance(&mut effect);
                    Tag::Compound(effect)
                }
                other => other,
            })
            .collect();
        compound.insert(to, fixed);
    }
}

pub(crate) fn stew_effect_entry(entry: &mut Compound) {
    update_effect_id(entry, "EffectId", "id");
    rename(entry, "EffectDuration", "duration");
}

pub(crate) fn tropical_pattern(packed: i32) -> &'static str {
    match packed & 0xffff {
        1 => "flopper",
        256 => "sunstreak",
        257 => "stripey",
        512 => "snooper",
        513 => "glitter",
        768 => "dasher",
        769 => "blockfish",
        1024 => "brinely",
        1025 => "betty",
        1280 => "spotty",
        1281 => "clayfish",
        _ => "kob",
    }
}
