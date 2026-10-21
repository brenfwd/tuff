//! Every recipe in `data/minecraft/recipe/`. One Rust variant per registered
//! `minecraft:recipe_serializer`, matching the JSON fields that serializer
//! reads.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::enums::emit_value_enum;
use super::{json, registries, tags};

use crate::input::Input;
use crate::names::{self, shouty, str_literal};

const COOKING_TYPES: &[&str] = &[
    "minecraft:smelting",
    "minecraft:blasting",
    "minecraft:smoking",
    "minecraft:campfire_cooking",
];

/// Every JSON field each recipe type's generator reads. A field outside this
/// list fails the build, so a new field in a future version is noticed and
/// modeled instead of silently dropped.
const KNOWN_FIELDS: &[(&str, &[&str])] = &[
    (
        "minecraft:crafting_shaped",
        &[
            "category",
            "group",
            "key",
            "pattern",
            "result",
            "show_notification",
        ],
    ),
    (
        "minecraft:crafting_shapeless",
        &["category", "group", "ingredients", "result"],
    ),
    (
        "minecraft:smelting",
        &[
            "category",
            "group",
            "ingredient",
            "result",
            "experience",
            "cookingtime",
        ],
    ),
    (
        "minecraft:blasting",
        &[
            "category",
            "group",
            "ingredient",
            "result",
            "experience",
            "cookingtime",
        ],
    ),
    (
        "minecraft:smoking",
        &[
            "category",
            "group",
            "ingredient",
            "result",
            "experience",
            "cookingtime",
        ],
    ),
    (
        "minecraft:campfire_cooking",
        &[
            "category",
            "group",
            "ingredient",
            "result",
            "experience",
            "cookingtime",
        ],
    ),
    ("minecraft:stonecutting", &["ingredient", "result"]),
    ("minecraft:brewing", &["input", "reagent", "output"]),
    (
        "minecraft:smithing_transform",
        &["template", "base", "addition", "result"],
    ),
    (
        "minecraft:smithing_trim",
        &["template", "base", "addition", "pattern"],
    ),
    (
        "minecraft:crafting_transmute",
        &[
            "category",
            "group",
            "input",
            "material",
            "result",
            "add_material_count_to_result",
            "material_count",
        ],
    ),
    (
        "minecraft:crafting_dye",
        &["group", "dye", "target", "result"],
    ),
    (
        "minecraft:crafting_special_bannerduplicate",
        &["banner", "result"],
    ),
    (
        "minecraft:crafting_special_bookcloning",
        &["source", "material", "result"],
    ),
    ("minecraft:crafting_special_repairitem", &[]),
    (
        "minecraft:crafting_special_firework_rocket",
        &["fuel", "shell", "star", "result"],
    ),
    (
        "minecraft:crafting_special_firework_star",
        &["dye", "fuel", "trail", "twinkle", "shapes", "result"],
    ),
    (
        "minecraft:crafting_special_firework_star_fade",
        &["dye", "target", "result"],
    ),
    (
        "minecraft:crafting_special_mapextending",
        &["map", "material", "result"],
    ),
    (
        "minecraft:crafting_special_shielddecoration",
        &["banner", "target", "result"],
    ),
    (
        "minecraft:crafting_decorated_pot",
        &["back", "front", "left", "right", "result"],
    ),
    (
        "minecraft:crafting_imbue",
        &["source", "material", "result"],
    ),
];

pub fn generate(input: &Input) -> Result<String> {
    let items = Items::load(input)?;
    let trim_patterns = trim_pattern_names(input);
    let ctx = Ctx {
        items,
        trim_patterns,
        potions: registries::variants(&input.registries["minecraft:potion"])?,
        potion_tags: tags::resolved_registry_tags(input, "minecraft:potion")?,
    };

    let mut recipes = Vec::new();
    for (path, json) in &input.data_json {
        let Some((namespace, rest)) = path.split_once('/') else {
            continue;
        };
        let Some(name) = rest
            .strip_prefix("recipe/")
            .and_then(|r| r.strip_suffix(".json"))
        else {
            continue;
        };
        recipes.push((format!("{namespace}:{name}"), json));
    }
    recipes.sort_by(|(a, _), (b, _)| a.cmp(b));

    let mut crafting = BTreeSet::new();
    let mut cooking = BTreeSet::new();
    for (_, json) in &recipes {
        let (Some(kind), Some(category)) = (
            json.get("type").and_then(Value::as_str),
            json.get("category").and_then(Value::as_str),
        ) else {
            continue;
        };
        if COOKING_TYPES.contains(&kind) {
            &mut cooking
        } else {
            &mut crafting
        }
        .insert(category.to_owned());
    }

    let consts = names::unique(recipes.iter().map(|(name, _)| name.as_str()), shouty)?;
    let mut out = String::new();
    emit_value_enum(
        &mut out,
        "CraftingCategory",
        &crafting.into_iter().collect::<Vec<_>>(),
    )?;
    emit_value_enum(
        &mut out,
        "CookingCategory",
        &cooking.into_iter().collect::<Vec<_>>(),
    )?;
    for ((name, json), ident) in recipes.iter().zip(&consts) {
        let recipe = ctx
            .recipe_expr(json)
            .with_context(|| format!("recipe {name}"))?;
        writeln!(
            out,
            "pub static {ident}: RecipeEntry = RecipeEntry {{ name: Static({}), recipe: {recipe} }};",
            str_literal(name)
        )?;
    }
    writeln!(out, "pub const ALL: &[(&str, &RecipeEntry)] = &[")?;
    for ((name, _), ident) in recipes.iter().zip(&consts) {
        writeln!(out, "    ({}, &{ident}),", str_literal(name))?;
    }
    writeln!(out, "];")?;
    Ok(out)
}

struct Ctx<'a> {
    items: Items<'a>,
    /// Every `minecraft:trim_pattern` entry name; this is a data-driven
    /// registry (see [`crate::data_registry`]), so recipes reference it by
    /// name rather than a fixed enum.
    trim_patterns: Vec<String>,
    potions: BTreeMap<&'a str, String>,
    potion_tags: tags::Tags,
}

/// Entry names of the `minecraft:trim_pattern` data-driven registry.
fn trim_pattern_names(input: &Input) -> Vec<String> {
    input
        .data_files
        .iter()
        .filter_map(|file| {
            let (namespace, rest) = file.split_once('/')?;
            let name = rest.strip_prefix("trim_pattern/")?.strip_suffix(".json")?;
            Some(format!("{namespace}:{name}"))
        })
        .collect()
}

struct Items<'a> {
    variants: BTreeMap<&'a str, String>,
    tags: tags::Tags,
}

impl<'a> Items<'a> {
    fn load(input: &'a Input) -> Result<Self> {
        Ok(Self {
            variants: registries::variants(&input.registries["minecraft:item"])?,
            tags: tags::resolved_registry_tags(input, "minecraft:item")?,
        })
    }

    fn expr(&self, name: &str) -> Result<String> {
        let variant = self
            .variants
            .get(normalize(name).as_str())
            .with_context(|| format!("unknown item {name}"))?;
        Ok(format!("Item::{variant}"))
    }

    fn ingredient_expr(&self, value: &Value) -> Result<String> {
        match value {
            Value::String(name) => match name.strip_prefix('#') {
                Some(tag) => {
                    let members = self.any_of(self.tag_entries(tag)?.iter().map(String::as_str))?;
                    let members = members
                        .strip_prefix("Ingredient::AnyOf(")
                        .and_then(|rest| rest.strip_suffix(')'))
                        .expect("any_of output");
                    Ok(format!(
                        "Ingredient::Tag(Static({}), {members})",
                        str_literal(&normalize(tag))
                    ))
                }
                None => Ok(format!("Ingredient::Item({})", self.expr(name)?)),
            },
            Value::Array(names) => {
                let names = names
                    .iter()
                    .map(|v| v.as_str().context("expected an item name"));
                self.any_of(names.collect::<Result<Vec<_>>>()?.iter().copied())
            }
            _ => bail!("unexpected ingredient {value}"),
        }
    }

    fn tag_entries(&self, tag: &str) -> Result<&[String]> {
        self.tags
            .get(normalize(tag).as_str())
            .map(Vec::as_slice)
            .with_context(|| format!("unknown item tag #{tag}"))
    }

    fn any_of<'b>(&self, names: impl Iterator<Item = &'b str>) -> Result<String> {
        let exprs = names
            .map(|name| self.expr(name))
            .collect::<Result<Vec<_>>>()?;
        Ok(format!(
            "Ingredient::AnyOf(Static(&[{}]))",
            exprs.join(", ")
        ))
    }
}

fn normalize(name: &str) -> String {
    if name.contains(':') {
        name.to_owned()
    } else {
        format!("minecraft:{name}")
    }
}

impl Ctx<'_> {
    fn recipe_expr(&self, value: &Value) -> Result<String> {
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .context("recipe has no type")?;
        let (_, known) = KNOWN_FIELDS
            .iter()
            .find(|(k, _)| *k == kind)
            .with_context(|| format!("unknown recipe type {kind}"))?;
        for field in value.as_object().into_iter().flat_map(|o| o.keys()) {
            anyhow::ensure!(
                field == "type" || known.contains(&field.as_str()),
                "{kind} has an unhandled field `{field}`"
            );
        }
        let variant = match kind {
            "minecraft:crafting_shaped" => format!("CraftingShaped({})", self.shaped(value)?),
            "minecraft:crafting_shapeless" => {
                format!("CraftingShapeless({})", self.shapeless(value)?)
            }
            "minecraft:smelting" => format!("Smelting({})", self.cooking(value)?),
            "minecraft:blasting" => format!("Blasting({})", self.cooking(value)?),
            "minecraft:smoking" => format!("Smoking({})", self.cooking(value)?),
            "minecraft:campfire_cooking" => format!("CampfireCooking({})", self.cooking(value)?),
            "minecraft:stonecutting" => format!("Stonecutting({})", self.stonecutting(value)?),
            "minecraft:brewing" => format!("Brewing({})", self.brewing(value)?),
            "minecraft:smithing_transform" => {
                format!("SmithingTransform({})", self.smithing_transform(value)?)
            }
            "minecraft:smithing_trim" => format!("SmithingTrim({})", self.smithing_trim(value)?),
            "minecraft:crafting_transmute" => {
                format!("CraftingTransmute({})", self.transmute(value)?)
            }
            "minecraft:crafting_dye" => format!("CraftingDye({})", self.dye(value)?),
            "minecraft:crafting_special_bannerduplicate" => {
                format!(
                    "CraftingSpecialBannerDuplicate({})",
                    self.banner_duplicate(value)?
                )
            }
            "minecraft:crafting_special_bookcloning" => {
                format!(
                    "CraftingSpecialBookCloning({})",
                    self.source_material("BookCloningRecipe", value)?
                )
            }
            "minecraft:crafting_special_repairitem" => "CraftingSpecialRepairItem".to_owned(),
            "minecraft:crafting_special_firework_rocket" => {
                format!(
                    "CraftingSpecialFireworkRocket({})",
                    self.firework_rocket(value)?
                )
            }
            "minecraft:crafting_special_firework_star" => {
                format!(
                    "CraftingSpecialFireworkStar({})",
                    self.firework_star(value)?
                )
            }
            "minecraft:crafting_special_firework_star_fade" => {
                format!(
                    "CraftingSpecialFireworkStarFade({})",
                    self.firework_star_fade(value)?
                )
            }
            "minecraft:crafting_special_mapextending" => {
                format!(
                    "CraftingSpecialMapExtending({})",
                    self.map_extending(value)?
                )
            }
            "minecraft:crafting_special_shielddecoration" => {
                format!(
                    "CraftingSpecialShieldDecoration({})",
                    self.shield_decoration(value)?
                )
            }
            "minecraft:crafting_decorated_pot" => {
                format!("CraftingDecoratedPot({})", self.decorated_pot(value)?)
            }
            "minecraft:crafting_imbue" => format!(
                "CraftingImbue({})",
                self.source_material("ImbueRecipe", value)?
            ),
            other => bail!("unknown recipe type {other}"),
        };
        Ok(format!("Recipe::{variant}"))
    }

    fn str_field<'v>(&self, value: &'v Value, key: &str) -> Option<&'v str> {
        value.get(key).and_then(Value::as_str)
    }

    fn optional_ingredient_field(&self, value: &Value, key: &str) -> Result<String> {
        match value.get(key) {
            Some(_) => Ok(format!("Some({})", self.ingredient_field(value, key)?)),
            None => Ok("None".to_owned()),
        }
    }

    fn ingredient_field(&self, value: &Value, key: &str) -> Result<String> {
        self.items
            .ingredient_expr(value.get(key).with_context(|| format!("missing {key}"))?)
    }

    /// `Some(<Enum>::Variant)` for the recipe's `category`, or `None` when the
    /// JSON omits it (the game then uses `misc`).
    fn category_expr(&self, enum_name: &str, value: &Value) -> String {
        match self.str_field(value, "category") {
            Some(category) => format!("Some({enum_name}::{})", names::pascal(category)),
            None => "None".to_owned(),
        }
    }

    fn group_expr(&self, value: &Value) -> String {
        match self.str_field(value, "group") {
            Some(group) => format!("Some(Static({}))", str_literal(group)),
            None => "None".to_owned(),
        }
    }

    fn result_expr(&self, value: &Value) -> Result<String> {
        self.result_field(value, "result")
    }

    fn result_field(&self, value: &Value, key: &str) -> Result<String> {
        let result = value.get(key).with_context(|| format!("missing {key}"))?;
        for field in result.as_object().into_iter().flat_map(|o| o.keys()) {
            anyhow::ensure!(
                ["id", "count", "components"].contains(&field.as_str()),
                "{key} has an unhandled field `{field}`"
            );
        }
        let item = self
            .items
            .expr(self.str_field(result, "id").context("result has no id")?)?;
        let count = result.get("count").and_then(Value::as_u64).unwrap_or(1);
        let components = match result.get("components") {
            Some(components) => format!("Some(Static({}))", json::literal(components)),
            None => "None".to_owned(),
        };
        Ok(format!(
            "RecipeResult {{ item: {item}, count: {count}, components: {components} }}"
        ))
    }

    fn shaped(&self, value: &Value) -> Result<String> {
        let key = value
            .get("key")
            .and_then(Value::as_object)
            .context("missing key")?;
        let entries = key
            .iter()
            .map(|(k, v)| -> Result<String> {
                let mut chars = k.chars();
                let (Some(ch), None) = (chars.next(), chars.next()) else {
                    bail!("key symbol {k:?} is not a single character");
                };
                Ok(format!("({ch:?}, {})", self.items.ingredient_expr(v)?))
            })
            .collect::<Result<Vec<_>>>()?;
        let pattern = value
            .get("pattern")
            .and_then(Value::as_array)
            .context("missing pattern")?
            .iter()
            .map(|row| {
                row.as_str()
                    .map(str_literal)
                    .context("pattern row is not a string")
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(format!(
            "ShapedCraftingRecipe {{ category: {}, group: {}, key: Static(&[{}]), pattern: PatternRows::Static(&[{}]), result: {}, show_notification: {} }}",
            self.category_expr("CraftingCategory", value),
            self.group_expr(value),
            entries.join(", "),
            pattern.join(", "),
            self.result_expr(value)?,
            value
                .get("show_notification")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        ))
    }

    fn shapeless(&self, value: &Value) -> Result<String> {
        let ingredients = value
            .get("ingredients")
            .and_then(Value::as_array)
            .context("missing ingredients")?
            .iter()
            .map(|v| self.items.ingredient_expr(v))
            .collect::<Result<Vec<_>>>()?;
        Ok(format!(
            "ShapelessCraftingRecipe {{ category: {}, group: {}, ingredients: Static(&[{}]), result: {}, show_notification: {} }}",
            self.category_expr("CraftingCategory", value),
            self.group_expr(value),
            ingredients.join(", "),
            self.result_expr(value)?,
            self.show_notification(value),
        ))
    }

    fn cooking(&self, value: &Value) -> Result<String> {
        let experience = value
            .get("experience")
            .and_then(Value::as_f64)
            .unwrap_or(0.0) as f32;
        let cooking_time = value
            .get("cookingtime")
            .and_then(Value::as_u64)
            .unwrap_or(200);
        Ok(format!(
            "CookingRecipe {{ category: {}, group: {}, ingredient: {}, result: {}, experience: {experience:?}, cooking_time: {cooking_time}, show_notification: {} }}",
            self.category_expr("CookingCategory", value),
            self.group_expr(value),
            self.ingredient_field(value, "ingredient")?,
            self.result_expr(value)?,
            self.show_notification(value),
        ))
    }

    fn stonecutting(&self, value: &Value) -> Result<String> {
        Ok(format!(
            "StonecuttingRecipe {{ ingredient: {}, result: {}, show_notification: {} }}",
            self.ingredient_field(value, "ingredient")?,
            self.result_expr(value)?,
            self.show_notification(value),
        ))
    }

    fn brewing(&self, value: &Value) -> Result<String> {
        Ok(format!(
            "BrewingRecipe {{ input: {}, reagent: {}, output: {} }}",
            self.potion_ingredient(value.get("input").context("missing input")?)?,
            self.potion_ingredient(value.get("reagent").context("missing reagent")?)?,
            self.result_field(value, "output")?,
        ))
    }

    fn potion_ingredient(&self, value: &Value) -> Result<String> {
        for field in value.as_object().context("bad potion ingredient")?.keys() {
            anyhow::ensure!(
                ["item", "potion_contents"].contains(&field.as_str()),
                "potion ingredient has an unhandled field `{field}`"
            );
        }
        let potions = match value.get("potion_contents") {
            None => "None".to_owned(),
            Some(contents) => {
                let contents = contents.as_object().context("bad potion_contents")?;
                for field in contents.keys() {
                    anyhow::ensure!(
                        field == "potions",
                        "potion_contents predicate has an unhandled field `{field}`"
                    );
                }
                let mut names: Vec<String> = Vec::new();
                match contents.get("potions") {
                    Some(Value::String(name)) => match name.strip_prefix('#') {
                        Some(tag) => names.extend(
                            self.potion_tags
                                .get(normalize(tag).as_str())
                                .with_context(|| format!("unknown potion tag #{tag}"))?
                                .iter()
                                .cloned(),
                        ),
                        None => names.push(normalize(name)),
                    },
                    Some(Value::Array(list)) => {
                        for name in list {
                            names.push(normalize(name.as_str().context("bad potion name")?));
                        }
                    }
                    _ => bail!("potion_contents predicate without potions"),
                }
                let exprs = names
                    .iter()
                    .map(|name| {
                        self.potions
                            .get(name.as_str())
                            .map(|v| format!("Potion::{v}"))
                            .with_context(|| format!("unknown potion {name}"))
                    })
                    .collect::<Result<Vec<_>>>()?;
                format!("Some(Static(&[{}]))", exprs.join(", "))
            }
        };
        Ok(format!(
            "PotionIngredient {{ ingredient: {}, potions: {potions} }}",
            self.ingredient_field(value, "item")?
        ))
    }

    fn smithing_transform(&self, value: &Value) -> Result<String> {
        Ok(format!(
            "SmithingTransformRecipe {{ template: {}, base: {}, addition: {}, result: {}, show_notification: {} }}",
            self.optional_ingredient_field(value, "template")?,
            self.ingredient_field(value, "base")?,
            self.optional_ingredient_field(value, "addition")?,
            self.result_expr(value)?,
            self.show_notification(value),
        ))
    }

    fn smithing_trim(&self, value: &Value) -> Result<String> {
        let pattern = self
            .str_field(value, "pattern")
            .context("missing pattern")?;
        let pattern = normalize(pattern);
        anyhow::ensure!(
            self.trim_patterns.contains(&pattern),
            "unknown trim pattern {pattern}"
        );
        Ok(format!(
            "SmithingTrimRecipe {{ template: {}, base: {}, addition: {}, pattern: {}, show_notification: {} }}",
            self.ingredient_field(value, "template")?,
            self.ingredient_field(value, "base")?,
            self.ingredient_field(value, "addition")?,
            str_literal(&pattern),
            self.show_notification(value),
        ))
    }

    fn transmute(&self, value: &Value) -> Result<String> {
        let result = value.get("result").context("missing result")?;
        let result = if result.get("id").is_some() {
            format!("Some({})", self.result_expr(value)?)
        } else {
            "None".to_owned()
        };
        let raw = value.get("result").context("missing result")?;
        let keep_count = raw.get("count").and_then(Value::as_u64).unwrap_or(1);
        let keep_components = match raw.get("components") {
            Some(components) if raw.get("id").is_none() => {
                format!("Some(Static({}))", json::literal(components))
            }
            _ => "None".to_owned(),
        };
        let material_count = match value.get("material_count") {
            Some(count) => format!(
                "Some(CountRange {{ min: {}, max: {} }})",
                count
                    .get("min")
                    .and_then(Value::as_u64)
                    .context("material_count has no min")?,
                count
                    .get("max")
                    .and_then(Value::as_u64)
                    .context("material_count has no max")?,
            ),
            None => "None".to_owned(),
        };
        Ok(format!(
            "TransmuteRecipe {{ category: {}, group: {}, input: {}, material: {}, material_count: {material_count}, add_material_count_to_result: {}, result: {result}, keep_count: {keep_count}, keep_components: {keep_components}, show_notification: {} }}",
            self.category_expr("CraftingCategory", value),
            self.group_expr(value),
            self.ingredient_field(value, "input")?,
            self.ingredient_field(value, "material")?,
            value
                .get("add_material_count_to_result")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            self.show_notification(value),
        ))
    }

    fn dye(&self, value: &Value) -> Result<String> {
        Ok(format!(
            "DyeRecipe {{ category: {}, group: {}, dye: {}, target: {}, result: {}, show_notification: {} }}",
            self.category_expr("CraftingCategory", value),
            self.group_expr(value),
            self.ingredient_field(value, "dye")?,
            self.ingredient_field(value, "target")?,
            self.result_expr(value)?,
            self.show_notification(value),
        ))
    }

    fn banner_duplicate(&self, value: &Value) -> Result<String> {
        Ok(format!(
            "BannerDuplicateRecipe {{ banner: {}, result: {} }}",
            self.ingredient_field(value, "banner")?,
            self.result_expr(value)?,
        ))
    }

    fn firework_star_fade(&self, value: &Value) -> Result<String> {
        Ok(format!(
            "FireworkStarFadeRecipe {{ dye: {}, target: {}, result: {} }}",
            self.ingredient_field(value, "dye")?,
            self.ingredient_field(value, "target")?,
            self.result_expr(value)?,
        ))
    }

    fn shield_decoration(&self, value: &Value) -> Result<String> {
        Ok(format!(
            "ShieldDecorationRecipe {{ banner: {}, target: {}, result: {} }}",
            self.ingredient_field(value, "banner")?,
            self.ingredient_field(value, "target")?,
            self.result_expr(value)?,
        ))
    }

    fn source_material(&self, struct_name: &str, value: &Value) -> Result<String> {
        let extra = if struct_name == "BookCloningRecipe" {
            ", allowed_generations: CountRange { min: 0, max: 1 }".to_owned()
        } else {
            format!(
                ", category: {}, group: {}, show_notification: {}",
                self.category_expr("CraftingCategory", value),
                self.group_expr(value),
                self.show_notification(value)
            )
        };
        Ok(format!(
            "{struct_name} {{ source: {}, material: {}, result: {}{extra} }}",
            self.ingredient_field(value, "source")?,
            self.ingredient_field(value, "material")?,
            self.result_expr(value)?,
        ))
    }

    fn show_notification(&self, value: &Value) -> bool {
        value
            .get("show_notification")
            .and_then(Value::as_bool)
            .unwrap_or(true)
    }

    fn firework_rocket(&self, value: &Value) -> Result<String> {
        Ok(format!(
            "FireworkRocketRecipe {{ fuel: {}, shell: {}, star: {}, result: {} }}",
            self.ingredient_field(value, "fuel")?,
            self.ingredient_field(value, "shell")?,
            self.ingredient_field(value, "star")?,
            self.result_expr(value)?,
        ))
    }

    fn firework_star(&self, value: &Value) -> Result<String> {
        let shapes = value
            .get("shapes")
            .and_then(Value::as_object)
            .context("missing shapes")?;
        let shapes = shapes
            .iter()
            .map(|(name, ingredient)| -> Result<String> {
                Ok(format!(
                    "(Static({}), {})",
                    str_literal(name),
                    self.items.ingredient_expr(ingredient)?
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(format!(
            "FireworkStarRecipe {{ dye: {}, fuel: {}, trail: {}, twinkle: {}, shapes: Static(&[{}]), result: {} }}",
            self.ingredient_field(value, "dye")?,
            self.ingredient_field(value, "fuel")?,
            self.ingredient_field(value, "trail")?,
            self.ingredient_field(value, "twinkle")?,
            shapes.join(", "),
            self.result_expr(value)?,
        ))
    }

    fn map_extending(&self, value: &Value) -> Result<String> {
        Ok(format!(
            "MapExtendingRecipe {{ map: {}, material: {} }}",
            self.ingredient_field(value, "map")?,
            self.ingredient_field(value, "material")?,
        ))
    }

    fn decorated_pot(&self, value: &Value) -> Result<String> {
        Ok(format!(
            "DecoratedPotRecipe {{ back: {}, front: {}, left: {}, right: {}, result: {} }}",
            self.ingredient_field(value, "back")?,
            self.ingredient_field(value, "front")?,
            self.ingredient_field(value, "left")?,
            self.ingredient_field(value, "right")?,
            self.result_expr(value)?,
        ))
    }
}
