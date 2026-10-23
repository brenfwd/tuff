use super::*;

pub(super) fn condition(condition: &LootCondition, context: &Context, rng: &mut impl Rng) -> bool {
    let mut value = condition
        .params
        .as_deref()
        .and_then(|p| serde_json::from_str::<Value>(p).ok())
        .unwrap_or_else(|| Value::Object(Default::default()));
    if let Some(object) = value.as_object_mut() {
        object.insert("type".into(), condition.kind.to_string().into());
    }
    json_condition(&value, context, rng)
}

/// Conditions that depend on context this server never provides (enchanted
/// tools, block entity data) resolve as vanilla would for a player without
/// enchantments breaking the block directly.
pub(super) fn json_condition(condition: &Value, context: &Context, rng: &mut impl Rng) -> bool {
    match condition["type"].as_str().unwrap_or_default() {
        "minecraft:survives_explosion" => survives(context.explosion_radius, rng),
        "minecraft:random_chance" => chance(&condition["chance"], rng),
        "minecraft:table_bonus" => table_bonus(condition, context.tool, rng),
        "minecraft:match_block" => match_block(condition, context.state, context.block_entity),
        "minecraft:match_tool" => match_tool(&condition["predicate"], context.tool),
        "minecraft:inverted" => !json_condition(&condition["term"], context, rng),
        "minecraft:killed_by_player" => context.entity.is_some_and(|e| e.killed_by_player),
        "minecraft:entity_properties" => context
            .entity
            .is_some_and(|facts| entity_properties(condition, facts, context)),
        "minecraft:damage_source_properties" => context
            .entity
            .is_some_and(|facts| damage_source_properties(&condition["predicate"], facts)),
        "minecraft:random_chance_with_enchanted_bonus" => {
            random_chance_with_enchanted_bonus(condition, context, rng)
        }
        "minecraft:location_check" => {
            let offset = ["offsetX", "offsetY", "offsetZ"]
                .map(|key| condition.get(key).and_then(Value::as_i64).unwrap_or(0));
            offset == [0, 0, 0]
                && context.location.is_some_and(|at| {
                    condition
                        .get("predicate")
                        .is_none_or(|p| tuff_core::world::predicate::location(p, at))
                })
        }
        "minecraft:int_value_check" => {
            json_int(&condition["value"], rng, context.scores().as_ref())
                .is_some_and(|value| int_in_range(&condition["test"], value))
        }
        "minecraft:float_value_check" => {
            json_number(&condition["value"], rng, context.scores().as_ref())
                .is_some_and(|value| float_in_range(&condition["test"], f64::from(value)))
        }
        "minecraft:weather_check"
        | "minecraft:time_check"
        | "minecraft:entity_scores"
        | "minecraft:environment_attribute_check" => world_condition(condition, context),
        "minecraft:any_of" => terms(condition)
            .iter()
            .any(|term| json_condition(term, context, rng)),
        "minecraft:all_of" => terms(condition)
            .iter()
            .all(|term| json_condition(term, context, rng)),
        _ => false,
    }
}

pub(super) fn world_condition(condition: &Value, context: &Context) -> bool {
    let this = context
        .entity
        .map(|facts| tuff_core::world::predicate::EntityFacts {
            kind: facts.kind,
            holder: facts.score_holder.clone(),
            ..Default::default()
        });
    let core = tuff_core::world::predicate::Context {
        this: this.as_ref(),
        target: None,
        origin: context.location.map_or([0.0; 3], |at| at.position),
        location: context.location,
        tool: context.tool,
        enchantment_level: None,
    };
    tuff_core::world::predicate::condition_in(condition, &core, context.world)
}

/// The `damage_source_properties` fields entity loot tables use; any
/// other field never matches.
pub(super) fn damage_source_properties(predicate: &Value, facts: &EntityFacts) -> bool {
    let (Some(fields), Some(kind)) = (predicate.as_object(), facts.damage_kind) else {
        return false;
    };
    fields.iter().all(|(key, value)| match key.as_str() {
        "tags" => value.as_array().into_iter().flatten().all(|tag| {
            let id = tag["id"].as_str().unwrap_or_default();
            let expected = tag["expected"].as_bool().unwrap_or(true);
            in_tag("minecraft:damage_type", id, kind.name) == expected
        }),
        "is_direct" => value.as_bool() == Some(facts.damage_direct),
        "source_entity" | "direct_entity" => facts.attacker_kind.is_some_and(|kind| {
            value["type"].as_str().is_none_or(|ty| {
                in_list(&Value::from(ty), kind.name(), |tag| {
                    in_tag("minecraft:entity_type", tag, kind.name())
                })
            })
        }),
        _ => false,
    })
}

/// The `entity_properties` predicate fields entity loot tables use; any
/// other field never matches.
pub(super) fn entity_properties(condition: &Value, facts: &EntityFacts, context: &Context) -> bool {
    let Some(predicate) = condition["predicate"].as_object() else {
        return false;
    };
    match condition["entity"].as_str() {
        Some("this") => predicate.iter().all(|(key, value)| match key.as_str() {
            "minecraft:flags" => value.as_object().is_some_and(|flags| {
                flags.iter().all(|(flag, expected)| match flag.as_str() {
                    "is_on_fire" => expected.as_bool() == Some(facts.on_fire),
                    _ => false,
                })
            }),
            "minecraft:components" => value.as_object().is_some_and(|components| {
                components.iter().all(|(id, expected)| {
                    facts
                        .components
                        .iter()
                        .any(|(key, actual)| key == id && expected.as_str() == Some(actual))
                })
            }),
            "minecraft:type_specific/sheep" => value.as_object().is_some_and(|fields| {
                fields.iter().all(|(field, expected)| match field.as_str() {
                    "sheared" => facts.sheared.is_some() && expected.as_bool() == facts.sheared,
                    _ => false,
                })
            }),
            "minecraft:type_specific/fishing_hook" => value.as_object().is_some_and(|fields| {
                fields.iter().all(|(field, expected)| match field.as_str() {
                    "in_open_water" => {
                        facts.open_water.is_some() && expected.as_bool() == facts.open_water
                    }
                    _ => false,
                })
            }),
            "minecraft:type_specific/raider" => value.as_object().is_some_and(|fields| {
                facts.raider.is_some_and(|(has_raid, captain)| {
                    let field = |name: &str| fields.get(name).and_then(|v| v.as_bool());
                    field("has_raid").unwrap_or(false) == has_raid
                        && field("is_captain").unwrap_or(false) == captain
                })
            }),
            "minecraft:type_specific/cube_mob" => value.as_object().is_some_and(|fields| {
                fields.iter().all(|(field, expected)| match field.as_str() {
                    "size" => facts
                        .cube_size
                        .is_some_and(|size| int_in_range(expected, i64::from(size))),
                    _ => false,
                })
            }),
            "minecraft:type" | "minecraft:entity_type" => facts.kind.is_some_and(|kind| {
                in_list(value, kind.name(), |tag| {
                    in_tag("minecraft:entity_type", tag, kind.name())
                })
            }),
            _ => delegated_entity(key, value, facts, context, false),
        }),
        Some("direct_attacker") | Some("attacker") => predicate
            .iter()
            .all(|(key, value)| delegated_entity(key, value, facts, context, true)),
        _ => false,
    }
}

/// One entity predicate field checked by the shared predicate evaluator
/// over the facts loot knows of the looted entity or of its attacker.
pub(super) fn delegated_entity(
    key: &str,
    value: &Value,
    facts: &EntityFacts,
    context: &Context,
    attacker: bool,
) -> bool {
    use tuff_core::world::predicate::Flags;
    let mut core = tuff_core::world::predicate::EntityFacts::default();
    if attacker {
        core.kind = facts.attacker_kind;
        if let Some(weapon) = facts.attacker_weapon.clone() {
            core.equipment
                .push((tuff_vanilla_data::item::EquipmentSlot::Mainhand, weapon));
        }
    } else {
        core.kind = facts.kind;
        core.flags = Flags {
            on_fire: facts.on_fire,
            ..Flags::default()
        };
        core.holder = facts.score_holder.clone();
    }
    if let Some(at) = context.location {
        core.position = at.position;
        core.dimension = at.dimension.clone();
        core.biome = at.biome.clone();
    }
    let predicate = Value::Object(std::iter::once((key.to_string(), value.clone())).collect());
    tuff_core::world::predicate::entity_in(
        &predicate,
        &core,
        context.location.map_or([0.0; 3], |at| at.position),
        context.world,
    )
}

/// Whether `value` meets an exact or `{min, max}` integer bound.
pub(super) fn int_in_range(bound: &Value, value: i64) -> bool {
    if let Some(exact) = bound.as_i64() {
        return value == exact;
    }
    let Some(range) = bound.as_object() else {
        return false;
    };
    range.keys().all(|k| k == "min" || k == "max")
        && range
            .get("min")
            .and_then(Value::as_i64)
            .is_none_or(|min| value >= min)
        && range
            .get("max")
            .and_then(Value::as_i64)
            .is_none_or(|max| value <= max)
}

pub(super) fn float_in_range(bound: &Value, value: f64) -> bool {
    if let Some(exact) = bound.as_f64() {
        return value == exact;
    }
    let Some(range) = bound.as_object() else {
        return false;
    };
    range.keys().all(|k| k == "min" || k == "max")
        && range
            .get("min")
            .and_then(Value::as_f64)
            .is_none_or(|min| value >= min)
        && range
            .get("max")
            .and_then(Value::as_f64)
            .is_none_or(|max| value <= max)
}

/// The enchantment level on `tool` for `enchantment`'s id or `0` if
/// there is no tool (an empty hand still gives its formula/chances a
/// level of 0, matching a tool context param that is always present).
pub(super) fn tool_enchantment_level(condition: &Value, tool: Option<AnyItem<'_>>) -> i32 {
    let Some(id) = condition["enchantment"].as_str() else {
        return 0;
    };
    tool.map_or(0, |tool| {
        tuff_item::with_item!(tool, |stack| stack.enchantment_level(id))
    })
}

/// `minecraft:table_bonus`: an enchantment-level-indexed chance, clamped
/// to the last entry for a level past the array (as the game caps it).
pub(super) fn table_bonus(
    condition: &Value,
    tool: Option<AnyItem<'_>>,
    rng: &mut impl Rng,
) -> bool {
    let Some(chances) = condition["chances"].as_array() else {
        return false;
    };
    if chances.is_empty() {
        return false;
    }
    let level = tool_enchantment_level(condition, tool).max(0) as usize;
    chance(&chances[level.min(chances.len() - 1)], rng)
}

/// `minecraft:random_chance_with_enchanted_bonus`: the attacker's
/// enchantment level (0 with no weapon or none of it) picks between the
/// unenchanted chance and the level-based enchanted one.
pub(super) fn random_chance_with_enchanted_bonus(
    condition: &Value,
    context: &Context,
    rng: &mut impl Rng,
) -> bool {
    let weapon = context.entity.and_then(|e| e.attacker_weapon.as_deref());
    let level = condition["enchantment"]
        .as_str()
        .map_or(0, |id| weapon.map_or(0, |w| w.enchantment_level(id)));
    let probability = if level > 0 {
        tuff_core::world::attributes::level_based(&condition["enchanted_chance"], level)
            .unwrap_or(0.0)
    } else {
        condition["unenchanted_chance"].as_f64().unwrap_or(0.0) as f32
    };
    rng.r#gen::<f32>() < probability
}

/// A number provider given as JSON: a plain number or a `constant`,
/// `uniform` or `binomial` provider.
pub(super) fn json_number(
    value: &Value,
    rng: &mut impl Rng,
    scores: Option<&Scores>,
) -> Option<f32> {
    if let Some(number) = value.as_f64() {
        return Some(number as f32);
    }
    let kind = value["type"].as_str().unwrap_or("minecraft:uniform");
    match kind {
        "minecraft:constant" => value["value"].as_f64().map(|v| v as f32),
        "minecraft:uniform" => {
            let min = json_number(&value["min"], rng, scores)?;
            let max = json_number(&value["max"], rng, scores)?;
            Some(if max <= min {
                min
            } else {
                rng.gen_range(min..max)
            })
        }
        "minecraft:binomial" => {
            let n = json_number(&value["n"], rng, scores)? as i32;
            let p = json_number(&value["p"], rng, scores)? as f64;
            Some((0..n.max(0)).filter(|_| rng.r#gen::<f64>() < p).count() as f32)
        }
        "minecraft:score" => {
            let scores = scores?;
            let objective = value["score"].as_str()?;
            match scores.score(&value["target"], objective) {
                Some(score) => Some(score as f32),
                None => match value.get("fallback") {
                    Some(fallback) => json_number(fallback, rng, Some(scores)),
                    None => Some(0.0),
                },
            }
        }
        _ => None,
    }
}

/// The score holders and level a `score` number provider reads.
pub struct Scores<'a> {
    pub(super) world: &'a dyn PredicateWorld,
    pub(super) this: Option<&'a str>,
}

impl Scores<'_> {
    /// Resolves a scoreboard name provider (`this`, a `context` target or a
    /// `fixed` name) and reads that holder's score.
    pub(super) fn score(&self, target: &Value, objective: &str) -> Option<i32> {
        let holder = match target {
            Value::String(name) if name == "this" => self.this,
            Value::Object(object) => match object.get("type").and_then(Value::as_str) {
                Some("minecraft:fixed") => object.get("name").and_then(Value::as_str),
                Some("minecraft:context") if object.get("target") == Some(&Value::from("this")) => {
                    self.this
                }
                _ => None,
            },
            _ => None,
        }?;
        self.world.score(holder, objective)
    }
}

impl Context<'_> {
    pub(super) fn scores(&self) -> Option<Scores<'_>> {
        Some(Scores {
            world: self.world?,
            this: self.entity.and_then(|e| e.score_holder.as_deref()),
        })
    }
}

/// An int number provider given as JSON: like [`json_number`], except a
/// `uniform` range is drawn as a whole number with both ends included.
pub(super) fn json_int(value: &Value, rng: &mut impl Rng, scores: Option<&Scores>) -> Option<i64> {
    if let Some(number) = value.as_f64() {
        return Some(number.floor() as i64);
    }
    match value["type"].as_str().unwrap_or("minecraft:uniform") {
        "minecraft:uniform" => {
            let min = json_int(&value["min"], rng, scores)?;
            let max = json_int(&value["max"], rng, scores)?;
            Some(if max <= min {
                min
            } else {
                rng.gen_range(min..=max)
            })
        }
        _ => json_number(value, rng, scores).map(|v| v.floor() as i64),
    }
}

/// `minecraft:limit_count`: clamps `count` into an int range whose bounds
/// are number providers (a bare number fixes both).
pub(super) fn limit_count(limit: &Value, count: i64, rng: &mut impl Rng) -> i64 {
    if let Some(exact) = limit.as_i64() {
        return exact;
    }
    let mut count = count;
    if let Some(min) = limit.get("min").and_then(|v| json_int(v, rng, None)) {
        count = count.max(min);
    }
    if let Some(max) = limit.get("max").and_then(|v| json_int(v, rng, None)) {
        count = count.min(max);
    }
    count
}

/// `minecraft:apply_bonus`: Fortune's block-loot count bonus, by formula.
/// A tool with no matching enchantment runs at level 0 (still meaningful
/// for `binomial_with_bonus_count`'s `extra` rounds).
pub(super) fn apply_bonus(params: &Value, count: i64, level: i32, rng: &mut impl Rng) -> i64 {
    let level = level.max(0);
    match params["formula"].as_str().unwrap_or_default() {
        "minecraft:uniform_bonus_count" => {
            let multiplier = params["parameters"]["bonusMultiplier"]
                .as_i64()
                .unwrap_or(0);
            let bound = (multiplier * i64::from(level)).max(0);
            count + rng.gen_range(0..=bound)
        }
        "minecraft:binomial_with_bonus_count" => {
            let extra = params["parameters"]["extra"].as_i64().unwrap_or(0).max(0);
            let probability = params["parameters"]["probability"].as_f64().unwrap_or(0.0);
            let rounds = i64::from(level) + extra;
            count
                + (0..rounds)
                    .filter(|_| rng.r#gen::<f64>() < probability)
                    .count() as i64
        }
        "minecraft:ore_drops" => {
            if level > 0 {
                let bonus = (rng.gen_range(0..(level + 2)) - 1).max(0);
                count * i64::from(bonus + 1)
            } else {
                count
            }
        }
        _ => count,
    }
}

pub(super) fn chance(value: &Value, rng: &mut impl Rng) -> bool {
    json_number(value, rng, None).is_some_and(|c| rng.r#gen::<f64>() < f64::from(c))
}

pub(super) fn terms(condition: &Value) -> &[Value] {
    condition["terms"].as_array().map_or(&[], Vec::as_slice)
}

pub(super) fn match_block(
    condition: &Value,
    state: BlockState,
    block_entity: Option<&Compound>,
) -> bool {
    let Some(fields) = condition.as_object() else {
        return false;
    };
    let predicate: serde_json::Map<String, Value> = fields
        .iter()
        .filter(|(key, _)| matches!(key.as_str(), "blocks" | "state" | "nbt"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    tuff_core::world::predicate::block_with(&Value::Object(predicate), state, block_entity)
}

pub(super) fn match_tool<'a>(
    predicate: &Value,
    tool: Option<impl Into<tuff_item::item::AnyItem<'a>>>,
) -> bool {
    tool.is_some_and(|tool| tuff_core::world::predicate::item(predicate, tool))
}

/// Whether `name` is in a registry entry list: one id, `#tag`, or an array
/// of ids.
pub(super) fn in_list(list: &Value, name: &str, in_tag: impl Fn(&str) -> bool) -> bool {
    match list {
        Value::String(entry) => match entry.strip_prefix('#') {
            Some(tag) => in_tag(tag),
            None => entry == name,
        },
        Value::Array(entries) => entries.iter().any(|e| e.as_str() == Some(name)),
        _ => false,
    }
}

/// Whether `#tag` of `registry` holds `id` in the tags in effect.
pub(super) fn in_tag(registry: &str, tag: &str, id: &str) -> bool {
    let tag = if tag.contains(':') {
        std::borrow::Cow::Borrowed(tag)
    } else {
        std::borrow::Cow::Owned(format!("minecraft:{tag}"))
    };
    tuff_datapack::tags::with(|tags| tags.contains(registry, &tag, id))
}

pub(super) fn pool_rolls(pool: &LootPool, luck: f32, rng: &mut impl Rng) -> u32 {
    let bonus = (number_float(&pool.bonus_rolls, rng) * f64::from(luck)).floor() as i64;
    (i64::from(number(&pool.rolls, rng)) + bonus).max(0) as u32
}

pub(super) fn number_float(provider: &NumberProvider, rng: &mut impl Rng) -> f64 {
    match *provider {
        NumberProvider::Constant(value) => value,
        NumberProvider::Uniform { min, max } if max >= min => rng.gen_range(min..=max),
        NumberProvider::Binomial { n, p } => (0..n.max(0.0) as u32)
            .filter(|_| rng.r#gen::<f64>() < p)
            .count() as f64,
        _ => 0.0,
    }
}

pub(super) fn number(provider: &NumberProvider, rng: &mut impl Rng) -> u32 {
    match *provider {
        NumberProvider::Constant(value) => value.max(0.0) as u32,
        NumberProvider::Uniform { min, max } if max >= min => rng
            .gen_range(min.floor() as i64..=max.floor() as i64)
            .max(0) as u32,
        NumberProvider::Binomial { n, p } => (0..n.max(0.0) as u32)
            .filter(|_| rng.r#gen::<f64>() < p)
            .count() as u32,
        _ => 0,
    }
}

/// The data pack's furnace recipe that smelts `item`.
pub(super) fn smelting_recipe(
    item: Item,
) -> Option<&'static tuff_vanilla_data::recipe::CookingRecipe> {
    tuff_vanilla_data::recipe::ALL
        .iter()
        .find_map(|(_, entry)| match &entry.recipe {
            tuff_vanilla_data::recipe::Recipe::Smelting(recipe)
                if recipe.ingredient.matches(item) =>
            {
                Some(recipe)
            }
            _ => None,
        })
}
