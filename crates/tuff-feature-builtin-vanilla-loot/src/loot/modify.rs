use super::*;

/// Runs item modifier `functions` over one stack, as a command does:
/// no block, entity or tool in the context. The result is limited to the
/// item's stack size.
pub fn modify_item(functions: &[LootFunction], stack: ItemStack, rng: &mut impl Rng) -> ItemStack {
    let context = Context {
        tables: LootTables::builtin(),
        visiting: RefCell::default(),
        state: Block::Air.default_state(),
        entity: None,
        tool: None,
        explosion_radius: None,
        block_entity: None,
        location: None,
        world: None,
    };
    let mut stack = stack;
    for function in functions {
        let params = function
            .params
            .as_deref()
            .and_then(|p| serde_json::from_str::<Value>(p).ok())
            .unwrap_or_else(|| Value::Object(Default::default()));
        if !params
            .get("condition")
            .is_none_or(|c| json_condition(c, &context, rng))
        {
            continue;
        }
        match &*function.kind {
            "minecraft:set_count" => {
                if let Some(value) = json_int(&params["count"], rng, context.scores().as_ref()) {
                    let count = if params["add"].as_bool() == Some(true) {
                        i64::from(stack.count) + value
                    } else {
                        value
                    };
                    stack.count = count.clamp(0, 255) as u8;
                }
            }
            "minecraft:limit_count" => {
                let count = limit_count(&params["limit"], i64::from(stack.count), rng);
                stack.count = count.clamp(0, 255) as u8;
            }
            "minecraft:set_item" => {
                if let Some(item) = params["item"].as_str().and_then(Item::from_name) {
                    stack = ItemStack::new(item, stack.count).with_components(stack.components);
                }
            }
            "minecraft:furnace_smelt" => {
                if let Some(recipe) = smelting_recipe(stack.item) {
                    let input = if params["use_input_count"].as_bool() == Some(false) {
                        1
                    } else {
                        u64::from(stack.count)
                    };
                    let count = (input * recipe.result.count).min(255) as u8;
                    stack =
                        ItemStack::new(recipe.result.item, count).with_components(stack.components);
                }
            }
            "minecraft:set_name" => {
                if let Some(name) = params.get("name").and_then(json_to_nbt) {
                    let target = format!(
                        "minecraft:{}",
                        params["target"].as_str().unwrap_or("custom_name")
                    );
                    let mut patch = stack.components.to_nbt();
                    patch.insert(target, name);
                    stack.components = Components::from_nbt(&patch);
                }
            }
            kind => stack = modify_stack(stack, kind, &params, &context, rng),
        }
    }
    stack.count = stack.count.min(stack.max_stack_size());
    stack
}
