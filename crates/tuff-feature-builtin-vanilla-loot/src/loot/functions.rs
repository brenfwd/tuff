use super::*;

use tuff_item::item::display::TooltipDisplay;
use tuff_vanilla_data::registry::DataComponentType;

use tuff_datapack::loot::{Predicates, parse_item_modifier};

fn patch(stack: &mut ItemStack, edit: impl FnOnce(&mut Compound)) {
    let mut nbt = stack.components.to_nbt();
    edit(&mut nbt);
    stack.components = Components::from_nbt(&nbt);
}

fn take_list(nbt: &Compound, key: &str) -> Vec<Tag> {
    nbt.get(key)
        .and_then(Tag::as_list)
        .map(<[Tag]>::to_vec)
        .unwrap_or_default()
}

/// A list operation: `mode` (`replace_all` by default), with `offset` and
/// `size` for the modes that take them.
const MAX_LORE_LINES: usize = 256;
const MAX_WRITABLE_PAGES: usize = 100;

fn list_operation(original: Vec<Tag>, replacement: Vec<Tag>, op: &Value) -> Vec<Tag> {
    let offset = op["offset"].as_u64().unwrap_or(0) as usize;
    match op["mode"]
        .as_str()
        .map(|m| m.trim_start_matches("minecraft:"))
    {
        Some("append") => [original, replacement].concat(),
        Some("insert") if offset <= original.len() => {
            let mut out = original;
            out.splice(offset..offset, replacement);
            out
        }
        Some("replace_section") if offset <= original.len() => {
            let size = op["size"]
                .as_u64()
                .map_or(replacement.len(), |size| size as usize);
            let resume = (offset + size).min(original.len());
            let mut out = original;
            out.splice(offset..resume, replacement);
            out
        }
        Some("insert" | "replace_section") => original,
        _ => replacement,
    }
}

/// A stand-alone list operation's values and the operation: either a
/// plain list or `{values, mode, ...}`.
fn stand_alone(value: &Value) -> (Vec<Value>, Value) {
    match value {
        Value::Array(values) => (values.clone(), Value::Null),
        Value::Object(map) => (
            map.get("values")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            value.clone(),
        ),
        _ => (Vec::new(), Value::Null),
    }
}

fn merge(into: &mut Compound, from: &Compound) {
    for (key, value) in from.iter() {
        match (into.get_mut(key), value) {
            (Some(Tag::Compound(existing)), Tag::Compound(incoming)) => merge(existing, incoming),
            _ => {
                into.insert(key, value.clone());
            }
        }
    }
}

fn block_entity_target(target: &str) -> bool {
    target.trim_start_matches("minecraft:") == "block_entity"
}

fn path_get<'a>(nbt: &'a Compound, path: &str) -> Option<&'a Tag> {
    let mut parts = path.split('.');
    let mut tag = nbt.get(parts.next()?)?;
    for part in parts {
        tag = tag.as_compound()?.get(part)?;
    }
    Some(tag)
}

fn path_set(nbt: &mut Compound, path: &str, op: &str, value: Tag) {
    let mut parts: Vec<&str> = path.split('.').collect();
    let Some(last) = parts.pop() else { return };
    let mut at = nbt;
    for part in parts {
        if !matches!(at.get(part), Some(Tag::Compound(_))) {
            at.insert(part, Compound::new());
        }
        let Some(next) = at.get_compound_mut(part) else {
            return;
        };
        at = next;
    }
    match (op, at.get_mut(last), value) {
        ("merge", Some(Tag::Compound(existing)), Tag::Compound(incoming)) => {
            merge(existing, &incoming);
        }
        ("append", Some(Tag::List(existing)), value) => existing.push(value),
        ("append", None, value) => {
            at.insert(last, Tag::List(vec![value]));
        }
        (_, _, value) => {
            at.insert(last, value);
        }
    }
}

fn parse_entries(entries: &Value) -> Option<tuff_vanilla_data::loot_table::LootTable> {
    let table = serde_json::json!({"pools": [{"rolls": 1, "entries": entries}]});
    tuff_datapack::loot::parse(
        table.to_string().as_bytes(),
        &tuff_datapack::tags::current(),
        &Predicates::vanilla(),
    )
    .ok()
}

fn modifier_functions(modifier: &Value) -> Vec<LootFunction> {
    parse_item_modifier(modifier, &Predicates::vanilla()).unwrap_or_default()
}

pub(super) fn item_stacks(tags: &[Tag]) -> Vec<ItemStack> {
    tags.iter()
        .filter_map(|tag| ItemStack::mint_from_nbt(tag.as_compound()?, MintReason::Loot))
        .collect()
}

fn contents_component(params: &Value) -> Option<&str> {
    match params["component"]
        .as_str()?
        .trim_start_matches("minecraft:")
    {
        "container" => Some("minecraft:container"),
        "bundle_contents" => Some("minecraft:bundle_contents"),
        "charged_projectiles" => Some("minecraft:charged_projectiles"),
        _ => None,
    }
}

fn stacks_to_component(component: &str, stacks: &[ItemStack]) -> Tag {
    let items = stacks.iter().map(|stack| Tag::Compound(stack.to_nbt()));
    if component == "minecraft:container" {
        Tag::List(
            stacks
                .iter()
                .enumerate()
                .map(|(slot, stack)| {
                    Tag::Compound(
                        Compound::new()
                            .with("slot", slot as i32)
                            .with("item", stack.to_nbt()),
                    )
                })
                .collect(),
        )
    } else {
        Tag::List(items.collect())
    }
}

pub(super) fn component_stacks(component: &str, nbt: &Compound) -> Vec<ItemStack> {
    let list = take_list(nbt, component);
    if component == "minecraft:container" {
        let items: Vec<Tag> = list
            .iter()
            .filter_map(|entry| entry.as_compound()?.get("item").cloned())
            .collect();
        item_stacks(&items)
    } else {
        item_stacks(&list)
    }
}

fn custom_model_data(stack: &mut ItemStack, params: &Value, context: &Context, rng: &mut impl Rng) {
    let _ = context;
    patch(stack, |nbt| {
        let mut data = nbt
            .get_compound("minecraft:custom_model_data")
            .cloned()
            .unwrap_or_default();
        for key in ["floats", "flags", "strings", "colors"] {
            if params.get(key).is_none() {
                continue;
            }
            let (values, op) = stand_alone(&params[key]);
            let replacement: Vec<Tag> = values
                .iter()
                .filter_map(|value| match key {
                    "floats" => json_number(value, rng, None).map(Tag::Float),
                    "flags" => value.as_bool().map(|b| Tag::Byte(i8::from(b))),
                    "strings" => value.as_str().map(|s| Tag::String(s.to_owned())),
                    _ => json_int(value, rng, None)
                        .or_else(|| rgb_color(value))
                        .map(|c| Tag::Int(c as i32)),
                })
                .collect();
            let original = take_list(&data, key);
            data.insert(key, Tag::List(list_operation(original, replacement, &op)));
        }
        nbt.insert("minecraft:custom_model_data", data);
    });
}

fn rgb_color(value: &Value) -> Option<i64> {
    let [r, g, b] = value.as_array()?.as_slice() else {
        return None;
    };
    let channel = |v: &Value| (v.as_f64().unwrap_or(0.0).clamp(0.0, 1.0) * 255.0) as i64;
    Some((channel(r) << 16) | (channel(g) << 8) | channel(b))
}

fn set_attributes(stack: &mut ItemStack, params: &Value, rng: &mut impl Rng) {
    let replace = params["replace"].as_bool().unwrap_or(true);
    patch(stack, |nbt| {
        let mut list = if replace {
            Vec::new()
        } else {
            take_list(nbt, "minecraft:attribute_modifiers")
        };
        for modifier in params["modifiers"].as_array().into_iter().flatten() {
            let Some(amount) = json_number(&modifier["amount"], rng, None) else {
                continue;
            };
            let slot = match &modifier["slot"] {
                Value::Array(slots) if !slots.is_empty() => &slots[rng.gen_range(0..slots.len())],
                other => other,
            };
            list.push(Tag::Compound(
                Compound::new()
                    .with("type", modifier["attribute"].as_str().unwrap_or_default())
                    .with("id", modifier["id"].as_str().unwrap_or_default())
                    .with("amount", f64::from(amount))
                    .with(
                        "operation",
                        modifier["operation"].as_str().unwrap_or_default(),
                    )
                    .with("slot", slot.as_str().unwrap_or("any")),
            ));
        }
        nbt.insert("minecraft:attribute_modifiers", Tag::List(list));
    });
}

fn set_instrument(stack: &mut ItemStack, params: &Value, rng: &mut impl Rng) {
    let Some(options) = params["options"].as_str() else {
        return;
    };
    let options = options.strip_prefix('#').unwrap_or(options);
    let tag = if options.contains(':') {
        options.to_owned()
    } else {
        format!("minecraft:{options}")
    };
    let members = tuff_datapack::tags::with(|tags| {
        tags.get("minecraft:instrument", &tag)
            .map(<[String]>::to_vec)
    });
    if let Some(members) = members.filter(|m| !m.is_empty()) {
        let chosen = members[rng.gen_range(0..members.len())].clone();
        patch(stack, |nbt| {
            nbt.insert("minecraft:instrument", chosen);
        });
    }
}

fn filtered(stack: ItemStack, params: &Value, context: &Context, rng: &mut impl Rng) -> ItemStack {
    let mut probe = None;
    if stack.components.map_id.is_none() && crate::exploration::pending(&stack).is_some() {
        let mut charted = tuff_item::item::ItemSnapshot::of(&stack);
        charted.edit_components(|components| components.map_id = Some(0));
        probe = Some(charted);
    }
    let tested = probe.as_deref().unwrap_or(&stack);
    let branch = if match_tool(&params["item_filter"], Some(tested)) {
        "on_pass"
    } else {
        "on_fail"
    };
    let Some(modifier) = params.get(branch) else {
        return stack;
    };
    let mut stacks = [stack];
    modify_stacks(&modifier_functions(modifier), &mut stacks, context, rng);
    let [stack] = stacks;
    stack
}

/// Applies the item modifier functions that need more than the stack's
/// components. Returns the stack unchanged for any other function.
pub(super) fn apply(
    mut stack: ItemStack,
    kind: &str,
    params: &Value,
    context: &Context,
    rng: &mut impl Rng,
) -> ItemStack {
    match kind {
        "minecraft:fill_player_head" => {
            if stack.item == Item::PlayerHead
                && let Some(name) = context.entity.and_then(|e| e.player_name.clone())
            {
                patch(&mut stack, |nbt| {
                    nbt.insert("minecraft:profile", Compound::new().with("name", name));
                });
            }
        }
        "minecraft:discard" => stack = ItemStack::empty(),
        "minecraft:filtered" => stack = filtered(stack, params, context, rng),
        "minecraft:set_instrument" => set_instrument(&mut stack, params, rng),
        "minecraft:set_attributes" => set_attributes(&mut stack, params, rng),
        "minecraft:set_custom_model_data" => custom_model_data(&mut stack, params, context, rng),
        "minecraft:set_custom_data" => {
            if let Some(Tag::Compound(tag)) = params.get("tag").and_then(json_to_nbt) {
                patch(&mut stack, |nbt| {
                    let mut data = nbt
                        .get_compound("minecraft:custom_data")
                        .cloned()
                        .unwrap_or_default();
                    merge(&mut data, &tag);
                    nbt.insert("minecraft:custom_data", data);
                });
            }
        }
        "minecraft:copy_state" => {
            let block = params["block"].as_str().map(|name| {
                if name.contains(':') {
                    name.to_owned()
                } else {
                    format!("minecraft:{name}")
                }
            });
            if block.as_deref() == Some(context.state.block().name()) {
                let copied: Vec<(&str, &str)> = params["properties"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .filter_map(|name| context.state.get(name).map(|value| (name, value)))
                    .collect();
                if !copied.is_empty() {
                    patch(&mut stack, |nbt| {
                        let mut state = nbt
                            .get_compound("minecraft:block_state")
                            .cloned()
                            .unwrap_or_default();
                        for (name, value) in copied {
                            state.insert(name, Tag::String(value.into()));
                        }
                        nbt.insert("minecraft:block_state", state);
                    });
                }
            }
        }
        "minecraft:copy_custom_data" => {
            let provider = &params["source"];
            let source: Option<std::borrow::Cow<'_, Compound>> = match provider.as_str() {
                Some(target) => block_entity_target(target)
                    .then_some(context.block_entity)
                    .flatten()
                    .map(std::borrow::Cow::Borrowed),
                None => match provider["type"].as_str() {
                    Some("minecraft:storage" | "storage") => provider["source"]
                        .as_str()
                        .and_then(|id| {
                            let id = if id.contains(':') {
                                id.to_owned()
                            } else {
                                format!("minecraft:{id}")
                            };
                            context.world.and_then(|world| world.storage(&id))
                        })
                        .map(std::borrow::Cow::Owned),
                    Some("minecraft:context" | "context") | None => provider["target"]
                        .as_str()
                        .filter(|target| block_entity_target(target))
                        .and(context.block_entity)
                        .map(std::borrow::Cow::Borrowed),
                    _ => None,
                },
            };
            if let Some(source) = source.as_deref() {
                patch(&mut stack, |nbt| {
                    let mut data = nbt
                        .get_compound("minecraft:custom_data")
                        .cloned()
                        .unwrap_or_default();
                    for op in params["ops"].as_array().into_iter().flatten() {
                        let (Some(from), Some(to)) = (op["source"].as_str(), op["target"].as_str())
                        else {
                            continue;
                        };
                        if let Some(value) = path_get(source, from) {
                            let mode = op["op"].as_str().unwrap_or("replace");
                            path_set(&mut data, to, mode, value.clone());
                        }
                    }
                    nbt.insert("minecraft:custom_data", data);
                });
            }
        }
        "minecraft:toggle_tooltips" => {
            let toggles: Vec<(DataComponentType, bool)> = params["toggles"]
                .as_object()
                .into_iter()
                .flatten()
                .filter_map(|(id, shown)| {
                    let id = if id.contains(':') {
                        id.clone()
                    } else {
                        format!("minecraft:{id}")
                    };
                    Some((DataComponentType::from_name(&id)?, shown.as_bool()?))
                })
                .collect();
            patch(&mut stack, |nbt| {
                let mut display = nbt
                    .get("minecraft:tooltip_display")
                    .and_then(TooltipDisplay::from_nbt)
                    .unwrap_or_default();
                for (kind, shown) in toggles {
                    display.hidden.retain(|hidden| *hidden != kind);
                    if !shown {
                        display.hidden.push(kind);
                    }
                }
                nbt.insert("minecraft:tooltip_display", display.to_nbt());
            });
        }
        "minecraft:set_lore" => {
            let lines: Vec<Tag> = params["lore"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(json_to_nbt)
                .collect();
            patch(&mut stack, |nbt| {
                let original = take_list(nbt, "minecraft:lore");
                let mut lore = list_operation(original, lines, params);
                lore.truncate(MAX_LORE_LINES);
                nbt.insert("minecraft:lore", Tag::List(lore));
            });
        }
        "minecraft:set_written_book_pages" | "minecraft:set_writable_book_pages" => {
            let written = kind == "minecraft:set_written_book_pages";
            let key = if written {
                "minecraft:written_book_content"
            } else {
                "minecraft:writable_book_content"
            };
            let pages: Vec<Tag> = params["pages"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(json_to_nbt)
                .collect();
            patch(&mut stack, |nbt| {
                let mut content = nbt.get_compound(key).cloned().unwrap_or_default();
                if written {
                    if content.get("title").is_none() {
                        content.insert("title", Tag::String("".into()));
                    }
                    if content.get("author").is_none() {
                        content.insert("author", Tag::String("".into()));
                    }
                }
                let original = take_list(&content, "pages");
                let mut all = list_operation(original, pages, params);
                if !written {
                    all.truncate(MAX_WRITABLE_PAGES);
                }
                content.insert("pages", Tag::List(all));
                nbt.insert(key, content);
            });
        }
        "minecraft:set_banner_pattern" => {
            let layers: Vec<Tag> = params["patterns"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(json_to_nbt)
                .collect();
            let append = params["append"].as_bool() == Some(true);
            patch(&mut stack, |nbt| {
                let mut all = if append {
                    take_list(nbt, "minecraft:banner_patterns")
                } else {
                    Vec::new()
                };
                all.extend(layers);
                nbt.insert("minecraft:banner_patterns", Tag::List(all));
            });
        }
        "minecraft:set_fireworks" => patch(&mut stack, |nbt| {
            let mut fireworks = nbt
                .get_compound("minecraft:fireworks")
                .cloned()
                .unwrap_or_default();
            if let Some(explosions) = params.get("explosions") {
                let (values, op) = stand_alone(explosions);
                let replacement = values.iter().filter_map(json_to_nbt).collect();
                let original = take_list(&fireworks, "explosions");
                fireworks.insert(
                    "explosions",
                    Tag::List(list_operation(original, replacement, &op)),
                );
            }
            if let Some(duration) = params["flight_duration"].as_i64() {
                fireworks.insert("flight_duration", Tag::Byte(duration.clamp(0, 255) as i8));
            }
            nbt.insert("minecraft:fireworks", fireworks);
        }),
        "minecraft:set_firework_explosion" => patch(&mut stack, |nbt| {
            let mut explosion = nbt
                .get_compound("minecraft:firework_explosion")
                .cloned()
                .unwrap_or_default();
            for key in ["shape", "colors", "fade_colors", "has_trail", "has_twinkle"] {
                if let Some(value) = params.get(key).and_then(json_to_nbt) {
                    explosion.insert(key, value);
                }
            }
            nbt.insert("minecraft:firework_explosion", explosion);
        }),
        "minecraft:set_book_cover" => patch(&mut stack, |nbt| {
            let mut book = nbt
                .get_compound("minecraft:written_book_content")
                .cloned()
                .unwrap_or_default();
            if let Some(title) = params.get("title") {
                let title = match title {
                    Value::String(raw) => Compound::new().with("raw", raw.as_str()),
                    other => json_to_nbt(other)
                        .and_then(|t| t.as_compound().cloned())
                        .unwrap_or_default(),
                };
                book.insert("title", title);
            }
            if let Some(author) = params["author"].as_str() {
                book.insert("author", author);
            }
            if let Some(generation) = params["generation"].as_i64() {
                book.insert("generation", generation.clamp(0, 3) as i32);
            }
            nbt.insert("minecraft:written_book_content", book);
        }),
        "minecraft:set_loot_table" => {
            if let Some(name) = params["name"].as_str() {
                let seed = params["seed"].as_i64().unwrap_or(0);
                patch(&mut stack, |nbt| {
                    let mut loot = Compound::new().with("loot_table", name);
                    if seed != 0 {
                        loot.insert("seed", seed);
                    }
                    nbt.insert("minecraft:container_loot", loot);
                });
            }
        }
        "minecraft:copy_name" => {
            if params["source"].as_str() == Some("block_entity")
                && let Some(name) = context.block_entity.and_then(|nbt| nbt.get("CustomName"))
            {
                let name = name.clone();
                patch(&mut stack, |nbt| {
                    nbt.insert("minecraft:custom_name", name);
                });
            }
        }
        "minecraft:set_contents" => {
            if let (Some(component), Some(table)) = (
                contents_component(params),
                parse_entries(&params["entries"]),
            ) {
                let mut stacks = Vec::new();
                roll_table(&table, context, rng, &mut stacks);
                stacks.retain(|s| s.count() > 0);
                patch(&mut stack, |nbt| {
                    nbt.insert(component, stacks_to_component(component, &stacks));
                });
            }
        }
        "minecraft:modify_contents" => {
            if let Some(component) = contents_component(params) {
                let functions = modifier_functions(&params["modifier"]);
                let mut nbt = stack.components.to_nbt();
                let mut stacks = component_stacks(component, &nbt);
                modify_stacks(&functions, &mut stacks, context, rng);
                stacks.retain(|s| s.count() > 0);
                nbt.insert(component, stacks_to_component(component, &stacks));
                stack.components = Components::from_nbt(&nbt);
            }
        }
        _ => {}
    }
    stack
}
