//! Recipe files (`recipe/<name>.json`) as the compiled-in recipe shapes,
//! checked the way the game's recipe codecs check them. Item tags resolve
//! against the loaded [`TagSet`]. What a file parses to is owned by the
//! [`RecipeEntry`], so it is freed with the last recipe set using it.

use serde_json::{Map, Value};
use tuff_vanilla_data::data_registry::DataRegistry;
use tuff_vanilla_data::recipe::{
    BannerDuplicateRecipe, BookCloningRecipe, BrewingRecipe, CookingCategory, CookingRecipe,
    CountRange, CraftingCategory, DecoratedPotRecipe, DyeRecipe, FireworkRocketRecipe,
    FireworkStarFadeRecipe, FireworkStarRecipe, ImbueRecipe, Ingredient, MapExtendingRecipe,
    PatternRows, PotionIngredient, Recipe, RecipeEntry, RecipeResult, ShapedCraftingRecipe,
    ShapelessCraftingRecipe, ShieldDecorationRecipe, SmithingTransformRecipe, SmithingTrimRecipe,
    StonecuttingRecipe, TransmuteRecipe,
};
use tuff_vanilla_data::registry::{Item, Potion};
use tuff_vanilla_data::{Frozen, Registry};

use super::tags::TagSet;

const MAX_GRID: usize = 3;
const MAX_RESULT_COUNT: u64 = 99;

/// The recipe `id` a pack file holds.
/// The members of item tag `tag` an ingredient takes (air left out).
pub fn tag_members(tags: &TagSet, tag: &str) -> Option<Vec<Item>> {
    Some(
        tags.typed::<Item>(tag)?
            .into_iter()
            .filter(|item| *item != Item::Air)
            .collect(),
    )
}

pub fn parse(id: &str, bytes: &[u8], tags: &TagSet) -> Result<RecipeEntry, String> {
    let value: Value = serde_json::from_slice(bytes).map_err(|err| err.to_string())?;
    Ok(RecipeEntry {
        name: Frozen::from(id.to_owned()),
        recipe: Parser { tags }.recipe(&value)?,
    })
}

/// One ingredient as a recipe file spells it: an item id, a `#tag` or a
/// list of item ids.
pub fn ingredient(value: &Value, tags: &TagSet) -> Result<Ingredient, String> {
    Parser { tags }.ingredient(value)
}

fn text(text: &str) -> Frozen<str> {
    Frozen::from(text.to_owned())
}

fn normalize(id: &str) -> String {
    if id.contains(':') {
        id.to_owned()
    } else {
        format!("minecraft:{id}")
    }
}

struct Parser<'a> {
    tags: &'a TagSet,
}

type Object = Map<String, Value>;

fn object(value: &Value) -> Result<&Object, String> {
    value
        .as_object()
        .ok_or_else(|| format!("not an object: {value}"))
}

fn field<'v>(object: &'v Object, key: &str) -> Result<&'v Value, String> {
    object.get(key).ok_or_else(|| format!("No key {key}"))
}

fn string<'v>(object: &'v Object, key: &str) -> Result<&'v str, String> {
    field(object, key)?
        .as_str()
        .ok_or_else(|| format!("{key} is not a string"))
}

fn optional_bool(object: &Object, key: &str, default: bool) -> Result<bool, String> {
    match object.get(key) {
        None => Ok(default),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(format!("{key} is not a boolean")),
    }
}

fn allowed_generations(object: &Object) -> Result<CountRange, String> {
    const LIMIT: u64 = 2;
    let bound = |value: &Value| {
        value
            .as_u64()
            .filter(|bound| *bound <= LIMIT)
            .ok_or_else(|| format!("allowed_generations must be within 0..={LIMIT}"))
    };
    let (min, max) = match object.get("allowed_generations") {
        None => return Ok(CountRange { min: 0, max: 1 }),
        Some(Value::Object(range)) => (
            range.get("min").map(bound).transpose()?.unwrap_or(0),
            range.get("max").map(bound).transpose()?.unwrap_or(LIMIT),
        ),
        Some(exact) => (bound(exact)?, bound(exact)?),
    };
    if min > max {
        return Err(format!("Invalid allowed_generations range {min}..{max}"));
    }
    Ok(CountRange { min, max })
}

fn optional_str<'v>(object: &'v Object, key: &str) -> Result<Option<&'v str>, String> {
    match object.get(key) {
        None => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(_) => Err(format!("{key} is not a string")),
    }
}

fn group(object: &Object) -> Result<Option<Frozen<str>>, String> {
    Ok(optional_str(object, "group")?
        .filter(|group| !group.is_empty())
        .map(text))
}

fn crafting_category(object: &Object) -> Result<Option<CraftingCategory>, String> {
    match optional_str(object, "category")? {
        None | Some("misc") => Ok(None),
        Some(name) => CraftingCategory::from_name(name)
            .map(Some)
            .ok_or_else(|| format!("Unknown crafting category {name}")),
    }
}

fn cooking_category(object: &Object) -> Result<Option<CookingCategory>, String> {
    match optional_str(object, "category")? {
        None | Some("misc") => Ok(None),
        Some(name) => CookingCategory::from_name(name)
            .map(Some)
            .ok_or_else(|| format!("Unknown cooking category {name}")),
    }
}

impl Parser<'_> {
    fn recipe(&self, value: &Value) -> Result<Recipe, String> {
        let object = object(value)?;
        let kind = normalize(string(object, "type")?);
        Ok(match kind.as_str() {
            "minecraft:crafting_shaped" => Recipe::CraftingShaped(self.shaped(object)?),
            "minecraft:crafting_shapeless" => Recipe::CraftingShapeless(self.shapeless(object)?),
            "minecraft:smelting" => Recipe::Smelting(self.cooking(object)?),
            "minecraft:blasting" => Recipe::Blasting(self.cooking(object)?),
            "minecraft:smoking" => Recipe::Smoking(self.cooking(object)?),
            "minecraft:campfire_cooking" => Recipe::CampfireCooking(self.cooking(object)?),
            "minecraft:stonecutting" => Recipe::Stonecutting(StonecuttingRecipe {
                ingredient: self.ingredient_field(object, "ingredient")?,
                result: self.result(object, "result")?,
                show_notification: optional_bool(object, "show_notification", true)?,
            }),
            "minecraft:brewing" => Recipe::Brewing(BrewingRecipe {
                input: self.potion_ingredient(field(object, "input")?)?,
                reagent: self.potion_ingredient(field(object, "reagent")?)?,
                output: self.result(object, "output")?,
            }),
            "minecraft:smithing_transform" => Recipe::SmithingTransform(SmithingTransformRecipe {
                template: self.optional_ingredient_field(object, "template")?,
                base: self.ingredient_field(object, "base")?,
                addition: self.optional_ingredient_field(object, "addition")?,
                result: self.result(object, "result")?,
                show_notification: optional_bool(object, "show_notification", true)?,
            }),
            "minecraft:smithing_trim" => Recipe::SmithingTrim(SmithingTrimRecipe {
                template: self.ingredient_field(object, "template")?,
                base: self.ingredient_field(object, "base")?,
                addition: self.ingredient_field(object, "addition")?,
                pattern: trim_pattern(string(object, "pattern")?)?,
                show_notification: optional_bool(object, "show_notification", true)?,
            }),
            "minecraft:crafting_transmute" => Recipe::CraftingTransmute(self.transmute(object)?),
            "minecraft:crafting_dye" => Recipe::CraftingDye(DyeRecipe {
                category: crafting_category(object)?,
                group: group(object)?,
                dye: self.ingredient_field(object, "dye")?,
                target: self.ingredient_field(object, "target")?,
                result: self.result(object, "result")?,
                show_notification: optional_bool(object, "show_notification", true)?,
            }),
            "minecraft:crafting_special_bannerduplicate" => {
                Recipe::CraftingSpecialBannerDuplicate(BannerDuplicateRecipe {
                    banner: self.ingredient_field(object, "banner")?,
                    result: self.result(object, "result")?,
                })
            }
            "minecraft:crafting_special_bookcloning" => {
                Recipe::CraftingSpecialBookCloning(BookCloningRecipe {
                    source: self.ingredient_field(object, "source")?,
                    material: self.ingredient_field(object, "material")?,
                    allowed_generations: allowed_generations(object)?,
                    result: self.result(object, "result")?,
                })
            }
            "minecraft:crafting_special_repairitem" => Recipe::CraftingSpecialRepairItem,
            "minecraft:crafting_special_firework_rocket" => {
                Recipe::CraftingSpecialFireworkRocket(FireworkRocketRecipe {
                    fuel: self.ingredient_field(object, "fuel")?,
                    shell: self.ingredient_field(object, "shell")?,
                    star: self.ingredient_field(object, "star")?,
                    result: self.result(object, "result")?,
                })
            }
            "minecraft:crafting_special_firework_star" => {
                Recipe::CraftingSpecialFireworkStar(self.firework_star(object)?)
            }
            "minecraft:crafting_special_firework_star_fade" => {
                Recipe::CraftingSpecialFireworkStarFade(FireworkStarFadeRecipe {
                    dye: self.ingredient_field(object, "dye")?,
                    target: self.ingredient_field(object, "target")?,
                    result: self.result(object, "result")?,
                })
            }
            "minecraft:crafting_special_mapextending" => {
                Recipe::CraftingSpecialMapExtending(MapExtendingRecipe {
                    map: self.ingredient_field(object, "map")?,
                    material: self.ingredient_field(object, "material")?,
                })
            }
            "minecraft:crafting_special_shielddecoration" => {
                Recipe::CraftingSpecialShieldDecoration(ShieldDecorationRecipe {
                    banner: self.ingredient_field(object, "banner")?,
                    target: self.ingredient_field(object, "target")?,
                    result: self.result(object, "result")?,
                })
            }
            "minecraft:crafting_decorated_pot" => {
                Recipe::CraftingDecoratedPot(DecoratedPotRecipe {
                    back: self.ingredient_field(object, "back")?,
                    front: self.ingredient_field(object, "front")?,
                    left: self.ingredient_field(object, "left")?,
                    right: self.ingredient_field(object, "right")?,
                    result: self.result(object, "result")?,
                })
            }
            "minecraft:crafting_imbue" => Recipe::CraftingImbue(ImbueRecipe {
                category: crafting_category(object)?,
                group: group(object)?,
                source: self.ingredient_field(object, "source")?,
                material: self.ingredient_field(object, "material")?,
                result: self.result(object, "result")?,
                show_notification: optional_bool(object, "show_notification", true)?,
            }),
            _ => return Err(format!("Unknown recipe type {kind}")),
        })
    }

    fn item(&self, name: &str) -> Result<Item, String> {
        match Item::from_name(&normalize(name)) {
            Some(Item::Air) => Err("Item must not be minecraft:air".to_owned()),
            Some(item) => Ok(item),
            None => Err(format!("Unknown item {name}")),
        }
    }

    fn ingredient(&self, value: &Value) -> Result<Ingredient, String> {
        let items = match value {
            Value::String(text) => match text.strip_prefix('#') {
                Some(tag) => {
                    let tag = normalize(tag);
                    let items = tag_members(self.tags, &tag)
                        .ok_or_else(|| format!("Missing tag #{tag}"))?;
                    if items.is_empty() {
                        return Err("Ingredient must not be empty".to_owned());
                    }
                    return Ok(Ingredient::Tag(tag.into(), items.into()));
                }
                None => return self.item(text).map(Ingredient::Item),
            },
            Value::Array(list) => list
                .iter()
                .map(|entry| {
                    entry
                        .as_str()
                        .ok_or_else(|| format!("Not an item id: {entry}"))
                        .and_then(|name| self.item(name))
                })
                .collect::<Result<Vec<_>, _>>()?,
            _ => return Err(format!("Not an ingredient: {value}")),
        };
        if items.is_empty() {
            return Err("Ingredient must not be empty".to_owned());
        }
        Ok(Ingredient::AnyOf(items.into()))
    }

    fn ingredient_field(&self, object: &Object, key: &str) -> Result<Ingredient, String> {
        self.ingredient(field(object, key)?)
            .map_err(|err| format!("{key}: {err}"))
    }

    fn optional_ingredient_field(
        &self,
        object: &Object,
        key: &str,
    ) -> Result<Option<Ingredient>, String> {
        match object.get(key) {
            None => Ok(None),
            Some(value) => self
                .ingredient(value)
                .map(Some)
                .map_err(|err| format!("{key}: {err}")),
        }
    }

    fn result(&self, object: &Object, key: &str) -> Result<RecipeResult, String> {
        let value = field(object, key)?;
        let (id, count, components) = match value {
            Value::String(id) => (id.as_str(), 1, None),
            Value::Object(result) => {
                let count = match result.get("count") {
                    None => 1,
                    Some(count) => count
                        .as_u64()
                        .filter(|count| (1..=MAX_RESULT_COUNT).contains(count))
                        .ok_or_else(|| {
                            format!("Value {count} outside of range [1:{MAX_RESULT_COUNT}]")
                        })?,
                };
                let components = match result.get("components") {
                    None => None,
                    Some(components @ Value::Object(_)) => Some(text(&components.to_string())),
                    Some(_) => return Err("components is not an object".to_owned()),
                };
                (string(result, "id")?, count, components)
            }
            _ => return Err(format!("{key}: not an item stack")),
        };
        Ok(RecipeResult {
            item: self.item(id)?,
            count,
            components,
        })
    }

    fn shaped(&self, object: &Object) -> Result<ShapedCraftingRecipe, String> {
        let rows: Vec<&str> = field(object, "pattern")?
            .as_array()
            .ok_or("pattern is not a list")?
            .iter()
            .map(|row| row.as_str().ok_or("pattern row is not a string"))
            .collect::<Result<_, _>>()?;
        if rows.len() > MAX_GRID {
            return Err("Invalid pattern: too many rows, 3 is maximum".to_owned());
        }
        if rows.is_empty() {
            return Err("Invalid pattern: empty pattern not allowed".to_owned());
        }
        let width = rows[0].chars().count();
        for row in &rows {
            let length = row.chars().count();
            if length > MAX_GRID {
                return Err("Invalid pattern: too many columns, 3 is maximum".to_owned());
            }
            if length != width {
                return Err("Invalid pattern: each row must be the same width".to_owned());
            }
        }
        let mut key: Vec<(char, Ingredient)> = Vec::new();
        for (symbol, ingredient) in self::object(field(object, "key")?)? {
            let mut chars = symbol.chars();
            let (Some(symbol), None) = (chars.next(), chars.next()) else {
                return Err(format!(
                    "Invalid key entry: '{symbol}' is an invalid symbol (must be 1 character only)."
                ));
            };
            if symbol == ' ' {
                return Err("Invalid key entry: ' ' is a reserved symbol.".to_owned());
            }
            key.push((symbol, self.ingredient(ingredient)?));
        }
        for row in &rows {
            for symbol in row.chars().filter(|c| *c != ' ') {
                if !key.iter().any(|(s, _)| *s == symbol) {
                    return Err(format!(
                        "Pattern references symbol '{symbol}' but it's not defined in the key"
                    ));
                }
            }
        }
        let unused: String = key
            .iter()
            .map(|(symbol, _)| *symbol)
            .filter(|symbol| !rows.iter().any(|row| row.contains(*symbol)))
            .collect();
        if !unused.is_empty() {
            return Err(format!(
                "Key defines symbols that aren't used in pattern: [{unused}]"
            ));
        }
        let (key, rows) = ascii_symbols(key, &rows);
        Ok(ShapedCraftingRecipe {
            category: crafting_category(object)?,
            group: group(object)?,
            key: key.into(),
            pattern: PatternRows::from(rows),
            result: self.result(object, "result")?,
            show_notification: optional_bool(object, "show_notification", true)?,
        })
    }

    fn shapeless(&self, object: &Object) -> Result<ShapelessCraftingRecipe, String> {
        let ingredients: Vec<Ingredient> = field(object, "ingredients")?
            .as_array()
            .ok_or("ingredients is not a list")?
            .iter()
            .map(|value| self.ingredient(value))
            .collect::<Result<_, _>>()?;
        if !(1..=MAX_GRID * MAX_GRID).contains(&ingredients.len()) {
            return Err(format!(
                "List must have between 1 and 9 elements, but has {}",
                ingredients.len()
            ));
        }
        Ok(ShapelessCraftingRecipe {
            category: crafting_category(object)?,
            group: group(object)?,
            ingredients: ingredients.into(),
            result: self.result(object, "result")?,
            show_notification: optional_bool(object, "show_notification", true)?,
        })
    }

    fn cooking(&self, object: &Object) -> Result<CookingRecipe, String> {
        let experience = match object.get("experience") {
            None => 0.0,
            Some(value) => value.as_f64().ok_or("experience is not a number")? as f32,
        };
        let cooking_time = field(object, "cookingtime")?
            .as_i64()
            .and_then(|time| i32::try_from(time).ok())
            .ok_or("cookingtime is not an int")?;
        Ok(CookingRecipe {
            category: cooking_category(object)?,
            group: group(object)?,
            ingredient: self.ingredient_field(object, "ingredient")?,
            result: self.result(object, "result")?,
            experience,
            cooking_time: cooking_time.max(1) as u64,
            show_notification: optional_bool(object, "show_notification", true)?,
        })
    }

    fn potion_ingredient(&self, value: &Value) -> Result<PotionIngredient, String> {
        let object = object(value)?;
        let potions = match object.get("potion_contents") {
            None => None,
            Some(contents) => {
                let names: Vec<String> = match field(self::object(contents)?, "potions")? {
                    Value::String(text) => match text.strip_prefix('#') {
                        Some(tag) => self
                            .tags
                            .get(Potion::KEY, &normalize(tag))
                            .ok_or_else(|| format!("Missing tag #{tag}"))?
                            .to_vec(),
                        None => vec![normalize(text)],
                    },
                    Value::Array(list) => list
                        .iter()
                        .map(|name| name.as_str().map(normalize).ok_or("bad potion id"))
                        .collect::<Result<_, _>>()?,
                    _ => return Err("potions is not a potion list".to_owned()),
                };
                let potions = names
                    .iter()
                    .map(|name| {
                        Potion::from_name(name).ok_or_else(|| format!("Unknown potion {name}"))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Some(potions.into())
            }
        };
        Ok(PotionIngredient {
            ingredient: self.ingredient_field(object, "item")?,
            potions,
        })
    }

    fn transmute(&self, object: &Object) -> Result<TransmuteRecipe, String> {
        let (result, keep_count, keep_components) = match field(object, "result")? {
            Value::Object(result) if !result.contains_key("id") => {
                let count = match result.get("count") {
                    None => 1,
                    Some(count) => count
                        .as_u64()
                        .filter(|count| (1..=MAX_RESULT_COUNT).contains(count))
                        .ok_or_else(|| {
                            format!("Value {count} outside of range [1:{MAX_RESULT_COUNT}]")
                        })?,
                };
                let components = match result.get("components") {
                    None => None,
                    Some(components @ Value::Object(_)) => Some(text(&components.to_string())),
                    Some(_) => return Err("components is not an object".to_owned()),
                };
                (None, count, components)
            }
            _ => (Some(self.result(object, "result")?), 1, None),
        };
        let material_count = match object.get("material_count") {
            None => None,
            Some(count) => {
                let count = self::object(count)?;
                let bound = |key| {
                    field(count, key)?
                        .as_u64()
                        .ok_or_else(|| format!("material_count {key} is not a count"))
                };
                let (min, max) = (bound("min")?, bound("max")?);
                if min == 0 || min > max {
                    return Err(format!("Invalid material count range {min}..{max}"));
                }
                Some(CountRange { min, max })
            }
        };
        Ok(TransmuteRecipe {
            category: crafting_category(object)?,
            group: group(object)?,
            input: self.ingredient_field(object, "input")?,
            material: self.ingredient_field(object, "material")?,
            material_count,
            add_material_count_to_result: optional_bool(
                object,
                "add_material_count_to_result",
                false,
            )?,
            result,
            keep_count,
            keep_components,
            show_notification: optional_bool(object, "show_notification", true)?,
        })
    }

    fn firework_star(&self, object: &Object) -> Result<FireworkStarRecipe, String> {
        let shapes = self::object(field(object, "shapes")?)?
            .iter()
            .map(|(name, ingredient)| Ok((text(name), self.ingredient(ingredient)?)))
            .collect::<Result<Vec<_>, String>>()?;
        Ok(FireworkStarRecipe {
            dye: self.ingredient_field(object, "dye")?,
            fuel: self.ingredient_field(object, "fuel")?,
            trail: self.ingredient_field(object, "trail")?,
            twinkle: self.ingredient_field(object, "twinkle")?,
            shapes: shapes.into(),
            result: self.result(object, "result")?,
        })
    }
}

fn trim_pattern(name: &str) -> Result<&'static str, String> {
    let name = normalize(name);
    DataRegistry::get("minecraft:trim_pattern")
        .and_then(|registry| {
            let index = registry.entries.binary_search(&name.as_str()).ok()?;
            Some(registry.entries[index])
        })
        .or_else(|| {
            tuff_vanilla_data::recipe::ALL
                .iter()
                .find_map(|(_, entry)| match &entry.recipe {
                    Recipe::SmithingTrim(trim) if trim.pattern == name => Some(trim.pattern),
                    _ => None,
                })
        })
        .ok_or_else(|| format!("Unknown trim pattern {name}"))
}

/// The key and rows with every symbol an ASCII byte, since grids are
/// matched byte by byte.
fn ascii_symbols(
    key: Vec<(char, Ingredient)>,
    rows: &[&str],
) -> (Vec<(char, Ingredient)>, Vec<String>) {
    if key.iter().all(|(symbol, _)| symbol.is_ascii()) {
        return (key, rows.iter().map(|row| (*row).to_owned()).collect());
    }
    let mut free = ('!'..='~').filter(|c| !key.iter().any(|(symbol, _)| symbol == c));
    let mapping: Vec<(char, char)> = key
        .iter()
        .map(|(symbol, _)| {
            if symbol.is_ascii() {
                (*symbol, *symbol)
            } else {
                (*symbol, free.next().expect("at most nine symbols are used"))
            }
        })
        .collect();
    let swap = |c: char| {
        mapping
            .iter()
            .find(|(from, _)| *from == c)
            .map_or(c, |(_, to)| *to)
    };
    let key = key
        .into_iter()
        .map(|(symbol, ingredient)| (swap(symbol), ingredient))
        .collect();
    let rows = rows
        .iter()
        .map(|row| row.chars().map(swap).collect())
        .collect();
    (key, rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recipe(json: &str) -> Result<Recipe, String> {
        let value: Value = serde_json::from_str(json).unwrap();
        Parser {
            tags: &TagSet::vanilla(),
        }
        .recipe(&value)
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_shaped_recipe_reads_its_key_pattern_and_result() {
        let Recipe::CraftingShaped(shaped) = recipe(
            r##"{"type":"minecraft:crafting_shaped","category":"building","pattern":["#\u00e9","# "],
                "key":{"#":"minecraft:stone","\u00e9":"#minecraft:planks"},
                "result":{"id":"minecraft:diamond","count":3}}"##,
        )
        .unwrap() else {
            panic!("not shaped");
        };
        assert_eq!(shaped.category, Some(CraftingCategory::Building));
        assert_eq!(shaped.result.item, Item::Diamond);
        assert_eq!(shaped.result.count, 3);
        assert!(shaped.pattern.iter().all(|row| row.is_ascii()));
        let symbol = shaped.pattern[0].chars().nth(1).unwrap();
        assert!(shaped.ingredient(symbol).unwrap().matches(Item::OakPlanks));
        assert!(shaped.ingredient('#').unwrap().matches(Item::Stone));
        assert!(shaped.show_notification);
    }

    #[tuff_core::watchdog]
    #[test]
    fn dye_and_imbue_recipes_read_category_and_group() {
        let Recipe::CraftingDye(dye) = recipe(
            r#"{"type":"minecraft:crafting_dye","category":"equipment","group":"g",
                "dye":"minecraft:red_dye","target":"minecraft:stone",
                "result":{"id":"minecraft:diamond"}}"#,
        )
        .unwrap() else {
            panic!("not dye");
        };
        assert_eq!(dye.category, Some(CraftingCategory::Equipment));
        let Recipe::CraftingImbue(imbue) = recipe(
            r#"{"type":"minecraft:crafting_imbue","category":"redstone","group":"h",
                "source":"minecraft:stone","material":"minecraft:stone",
                "result":{"id":"minecraft:diamond"}}"#,
        )
        .unwrap() else {
            panic!("not imbue");
        };
        assert_eq!(imbue.category, Some(CraftingCategory::Redstone));
        assert_eq!(imbue.group.as_deref(), Some("h"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn show_notification_is_read_for_every_recipe_kind() {
        let shapeless = |extra: &str| {
            recipe(&format!(
                r#"{{"type":"minecraft:crafting_shapeless","ingredients":["minecraft:stone"],
                "result":{{"id":"minecraft:diamond"}}{extra}}}"#
            ))
            .unwrap()
        };
        let Recipe::CraftingShapeless(on) = shapeless("") else {
            panic!("not shapeless");
        };
        let Recipe::CraftingShapeless(off) = shapeless(r#","show_notification":false"#) else {
            panic!("not shapeless");
        };
        assert!(on.show_notification);
        assert!(!off.show_notification);
        let Recipe::Stonecutting(cut) = recipe(
            r#"{"type":"minecraft:stonecutting","ingredient":"minecraft:stone",
            "result":{"id":"minecraft:stone_slab"},"show_notification":false}"#,
        )
        .unwrap() else {
            panic!("not stonecutting");
        };
        assert!(!cut.show_notification);
    }

    #[tuff_core::watchdog]
    #[test]
    fn book_cloning_reads_allowed_generations() {
        let cloning = |extra: &str| {
            recipe(&format!(
                r#"{{"type":"minecraft:crafting_special_bookcloning","source":"minecraft:written_book",
                "material":"minecraft:writable_book","result":{{"id":"minecraft:written_book"}}{extra}}}"#
            ))
        };
        let range = |recipe: Result<Recipe, String>| match recipe.unwrap() {
            Recipe::CraftingSpecialBookCloning(r) => {
                (r.allowed_generations.min, r.allowed_generations.max)
            }
            _ => panic!("not book cloning"),
        };
        assert_eq!(range(cloning("")), (0, 1));
        assert_eq!(
            range(cloning(r#","allowed_generations":{"max":2}"#)),
            (0, 2)
        );
        assert_eq!(range(cloning(r#","allowed_generations":1"#)), (1, 1));
        assert!(cloning(r#","allowed_generations":{"max":3}"#).is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn malformed_recipes_are_rejected() {
        let bad = [
            r###"{"type":"minecraft:nope"}"###,
            r###"{"type":"minecraft:crafting_shaped","pattern":["XXXX"],"key":{"X":"stone"},"result":"stone"}"###,
            r###"{"type":"minecraft:crafting_shaped","pattern":["X","XX"],"key":{"X":"stone"},"result":"stone"}"###,
            r###"{"type":"minecraft:crafting_shaped","pattern":["X"],"key":{"X":"stone","x":"dirt"},"result":"stone"}"###,
            r###"{"type":"minecraft:crafting_shaped","pattern":["Xx"],"key":{"X":"stone"},"result":"stone"}"###,
            r###"{"type":"minecraft:crafting_shapeless","ingredients":[],"result":"stone"}"###,
            r###"{"type":"minecraft:crafting_shapeless","ingredients":["air"],"result":"stone"}"###,
            r###"{"type":"minecraft:crafting_shapeless","ingredients":["#minecraft:nope"],"result":"stone"}"###,
            r###"{"type":"minecraft:crafting_shapeless","ingredients":["stone"],"result":{"id":"stone","count":100}}"###,
            r###"{"type":"minecraft:smelting","ingredient":"stone","result":"stone"}"###,
            r###"{"type":"minecraft:smithing_trim","template":"stone","base":"stone","addition":"stone","pattern":"nope"}"###,
        ];
        for json in bad {
            assert!(recipe(json).is_err(), "{json}");
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn smithing_transform_template_and_addition_are_optional() {
        let Recipe::SmithingTransform(bare) = recipe(
            r###"{"type":"minecraft:smithing_transform","base":"stone","result":{"id":"dirt"}}"###,
        )
        .unwrap() else {
            panic!("not smithing");
        };
        assert!(bare.template.is_none() && bare.addition.is_none());
        let Recipe::SmithingTransform(full) = recipe(
            r###"{"type":"minecraft:smithing_transform","template":"stone","base":"stone","addition":"stone","result":{"id":"dirt"}}"###,
        )
        .unwrap() else {
            panic!("not smithing");
        };
        assert!(full.template.is_some() && full.addition.is_some());
        assert!(
            recipe(
                r###"{"type":"minecraft:smithing_transform","template":"nope","base":"stone","result":{"id":"dirt"}}"###,
            )
            .is_err()
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_transmute_result_without_id_keeps_count_and_components() {
        let Recipe::CraftingTransmute(transmute) = recipe(
            r##"{"type":"minecraft:crafting_transmute","input":"stone","material":"dirt",
                "result":{"count":3,"components":{"minecraft:custom_name":"\"x\""}}}"##,
        )
        .unwrap() else {
            panic!("not transmute");
        };
        assert!(transmute.result.is_none());
        assert_eq!(transmute.keep_count, 3);
        assert!(transmute.keep_components.is_some());
        assert!(
            recipe(
                r##"{"type":"minecraft:crafting_transmute","input":"stone","material":"dirt","result":{"count":100}}"##,
            )
            .is_err()
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn every_kind_the_game_ships_parses() {
        let good = [
            r###"{"type":"smelting","ingredient":"cobblestone","result":{"id":"stone"},"experience":0.1,"cookingtime":200}"###,
            r###"{"type":"minecraft:stonecutting","ingredient":"stone","result":{"id":"stone_slab","count":2}}"###,
            r###"{"type":"minecraft:brewing","input":{"item":"minecraft:potion","potion_contents":{"potions":"minecraft:water"}},"reagent":{"item":"nether_wart"},"output":{"id":"minecraft:potion"}}"###,
            r###"{"type":"minecraft:smithing_trim","template":"stone","base":"#minecraft:trimmable_armor","addition":"#minecraft:trim_materials","pattern":"minecraft:coast"}"###,
            r###"{"type":"minecraft:crafting_transmute","input":"#minecraft:shulker_boxes","material":"blue_dye","result":{"id":"blue_shulker_box"}}"###,
            r###"{"type":"minecraft:crafting_special_repairitem"}"###,
        ];
        for json in good {
            recipe(json).unwrap_or_else(|err| panic!("{json}: {err}"));
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_parsed_recipe_owns_its_data() {
        let tags = TagSet::vanilla();
        let bytes = br##"{"type":"minecraft:crafting_shapeless","group":"g",
            "ingredients":["#minecraft:planks"],"result":{"id":"dirt","components":{}}}"##;
        let entry = parse("a:x", bytes, &tags).unwrap();
        assert_eq!(entry.name, "a:x");
        assert!(entry.name.as_static().is_none());
        let Recipe::CraftingShapeless(shapeless) = &entry.recipe else {
            panic!("not shapeless");
        };
        let Ingredient::Tag(_, Frozen::Shared(items)) = &shapeless.ingredients[0] else {
            panic!("a pack ingredient is not runtime data");
        };
        let weak = std::sync::Arc::downgrade(items);
        assert!(
            shapeless
                .group
                .as_ref()
                .is_some_and(|g| g.as_static().is_none())
        );
        drop(entry);
        assert!(weak.upgrade().is_none());
    }
}
