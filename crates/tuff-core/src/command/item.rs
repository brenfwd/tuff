//! Item arguments: an item with components (`diamond_sword[damage=5]`) and
//! item predicates that also accept tags and component tests.

use tuff_vanilla_data::Item;
use tuff_vanilla_data::registry::DataComponentType;

use tuff_item::item::{Components, ItemStack, MintReason};
use tuff_nbt::{Compound, Tag};

use super::args::{Resources, read_identifier, read_nbt_value};
use super::error::CommandError;
use super::item_predicate::{self, ComponentPredicate};
use super::range::{self, Bounds};
use super::reader::Reader;

#[derive(Debug, Clone, PartialEq)]
pub struct ItemInput {
    pub item: Item,
    pub components: Components,
}

impl ItemInput {
    pub fn stack(&self, count: u8) -> ItemStack {
        ItemStack::mint(self.item, count, MintReason::Command)
            .with_components(self.components.clone())
    }
}

/// One test of an item predicate.
#[derive(Debug, Clone, PartialEq)]
pub enum ItemTest {
    Item(Item),
    /// An item tag by name, with the built-in pack's members until
    /// [`ItemPredicate::resolved`] reads the tags in effect.
    Tag(String, tuff_vanilla_data::Frozen<[Item]>),
    /// The component is set on the stack.
    HasComponent(String),
    /// The component's stored value equals this NBT.
    ComponentEquals(String, Tag),
    Count(Bounds<i32>),
    Damage(Bounds<i32>),
    Component(ComponentPredicate),
    /// The stack is the custom item with this id.
    Custom(String),
}

/// A conjunction of clauses (`,`), each a disjunction (`|`) of tests that
/// are optionally negated.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemPredicate {
    pub clauses: Vec<Vec<(ItemTest, bool)>>,
}

impl ItemPredicate {
    /// This predicate with its tags' members read from `tags` (data packs
    /// may change them); an unknown tag is an error.
    pub fn resolved(mut self, tags: &tuff_datapack::tags::TagSet) -> Result<Self, CommandError> {
        for (test, _) in self.clauses.iter_mut().flatten() {
            if let ItemTest::Tag(name, items) = test {
                let members = tags.typed::<Item>(name).ok_or_else(|| {
                    CommandError::dynamic("arguments.item.tag.unknown", [name.clone()])
                })?;
                *items = members.into();
            }
        }
        Ok(self)
    }

    pub fn test(&self, stack: &ItemStack) -> bool {
        self.clauses.iter().all(|tests| {
            tests
                .iter()
                .any(|(test, negated)| test.test(stack) != *negated)
        })
    }
}

impl ItemTest {
    fn test(&self, stack: &ItemStack) -> bool {
        match self {
            Self::Item(item) => stack.item == *item,
            Self::Tag(_, items) => items.contains(&stack.item),
            Self::HasComponent(id) => stack.components.to_nbt().contains_key(id),
            Self::ComponentEquals(id, value) => stack.components.to_nbt().get(id) == Some(value),
            Self::Count(range) => range.contains(i32::from(stack.count())),
            Self::Damage(range) => range.contains(stack.damage() as i32),
            Self::Component(predicate) => predicate.test(stack),
            Self::Custom(id) => crate::custom::item_id(stack) == Some(id.as_str()),
        }
    }
}

/// A vanilla item, or (as `Err`) a custom item `resources` knows.
fn read_any_item<'r>(
    reader: &mut Reader<'_>,
    resources: &'r Resources,
) -> Result<Result<Item, (String, &'r ItemInput)>, CommandError> {
    let start = reader.cursor();
    let id = read_identifier(reader)?;
    if let Some(item) = Item::from_name(&id) {
        return Ok(Ok(item));
    }
    match resources.custom_item(&id) {
        Some(input) => Ok(Err((id, input))),
        None => {
            reader.set_cursor(start);
            Err(CommandError::dynamic("argument.item.id.invalid", [id]).at(reader))
        }
    }
}

fn read_component_type(reader: &mut Reader<'_>) -> Result<String, CommandError> {
    if !reader.can_read() {
        return Err(CommandError::simple("arguments.item.component.expected").at(reader));
    }
    let start = reader.cursor();
    let id = read_identifier(reader)?;
    let (namespace, path) = id.split_once(':').unwrap_or(("minecraft", &id));
    let name = format!("{namespace}:{path}");
    if DataComponentType::from_name(&name).is_none() {
        reader.set_cursor(start);
        return Err(CommandError::dynamic("arguments.item.component.unknown", [name]).at(reader));
    }
    Ok(name)
}

/// `item_stack`: an item and optional `[component=value,!removed]`.
pub fn parse_item_stack(reader: &mut Reader<'_>) -> Result<ItemInput, CommandError> {
    parse_item_stack_with(reader, &Resources::default())
}

/// [`parse_item_stack`], also accepting the custom items in `resources`;
/// listed components then override the custom item's own.
pub fn parse_item_stack_with(
    reader: &mut Reader<'_>,
    resources: &Resources,
) -> Result<ItemInput, CommandError> {
    let (item, mut nbt) = match read_any_item(reader, resources)? {
        Ok(item) => (item, Compound::new()),
        Err((_, input)) => (input.item, input.components.to_nbt()),
    };
    if reader.peek_is('[') {
        reader.skip();
        let mut seen: Vec<String> = Vec::new();
        while reader.can_read() && !reader.peek_is(']') {
            reader.skip_whitespace();
            if reader.peek_is('!') {
                reader.skip();
                let id = read_component_type(reader)?;
                if seen.contains(&id) {
                    return Err(CommandError::dynamic(
                        "arguments.item.component.repeated",
                        [id],
                    ));
                }
                nbt.remove(&id);
                nbt.insert(format!("!{id}"), Compound::new());
                seen.push(id);
                reader.skip_whitespace();
            } else {
                let id = read_component_type(reader)?;
                if seen.contains(&id) {
                    return Err(CommandError::dynamic(
                        "arguments.item.component.repeated",
                        [id],
                    ));
                }
                reader.skip_whitespace();
                reader.expect('=')?;
                reader.skip_whitespace();
                let value_start = reader.cursor();
                let value = read_nbt_value(reader)?;
                let mut probe = Compound::new();
                probe.insert(id.clone(), value.clone());
                let parsed = Components::from_nbt(&probe);
                if parsed.other.contains_key(&id)
                    && DataComponentType::from_name(&id).is_some_and(known)
                {
                    reader.set_cursor(value_start);
                    return Err(CommandError::dynamic(
                        "arguments.item.component.malformed",
                        [id, "unexpected value".to_owned()],
                    )
                    .at(reader));
                }
                nbt.remove(&format!("!{id}"));
                nbt.insert(id.clone(), value);
                seen.push(id);
                reader.skip_whitespace();
            }
            if !reader.peek_is(',') {
                break;
            }
            reader.skip();
            reader.skip_whitespace();
            if !reader.can_read() {
                return Err(CommandError::simple("arguments.item.component.expected").at(reader));
            }
        }
        reader.expect(']')?;
    }
    Ok(ItemInput {
        item,
        components: Components::from_nbt(&nbt),
    })
}

/// Component types this server decodes; a value it could not decode for
/// one of these is malformed rather than merely unmodelled.
fn known(kind: DataComponentType) -> bool {
    matches!(
        kind,
        DataComponentType::Damage
            | DataComponentType::MaxDamage
            | DataComponentType::Unbreakable
            | DataComponentType::CustomName
            | DataComponentType::Enchantments
            | DataComponentType::StoredEnchantments
            | DataComponentType::PotionContents
            | DataComponentType::RepairCost
            | DataComponentType::OminousBottleAmplifier
            | DataComponentType::MapId
            | DataComponentType::Container
            | DataComponentType::DyedColor
            | DataComponentType::BaseColor
            | DataComponentType::BannerPatterns
            | DataComponentType::Trim
            | DataComponentType::Fireworks
            | DataComponentType::FireworkExplosion
            | DataComponentType::WritableBookContent
            | DataComponentType::WrittenBookContent
            | DataComponentType::AttributeModifiers
            | DataComponentType::Tool
            | DataComponentType::Weapon
    )
}

/// `item_predicate`: `item|#tag[test,...]|...`.
pub fn parse_item_predicate(reader: &mut Reader<'_>) -> Result<ItemPredicate, CommandError> {
    parse_item_predicate_with(reader, &Resources::default())
}

/// [`parse_item_predicate`], also accepting the custom items in
/// `resources`.
pub fn parse_item_predicate_with(
    reader: &mut Reader<'_>,
    resources: &Resources,
) -> Result<ItemPredicate, CommandError> {
    let mut clauses: Vec<Vec<(ItemTest, bool)>> = Vec::new();
    {
        let mut tests: Vec<(ItemTest, bool)> = Vec::new();
        if reader.peek_is('#') {
            reader.skip();
            let id = read_identifier(reader)?;
            let (namespace, path) = id.split_once(':').unwrap_or(("minecraft", &id));
            let name = format!("{namespace}:{path}");
            let items: tuff_vanilla_data::Frozen<[Item]> = tuff_vanilla_data::tag::item::ALL
                .iter()
                .find(|(tag, _)| *tag == name)
                .map_or_else(Default::default, |(_, items)| {
                    tuff_vanilla_data::Frozen::Static(*items)
                });
            tests.push((ItemTest::Tag(name, items), false));
        } else if reader.peek_is('*') {
            reader.skip();
        } else {
            match read_any_item(reader, resources)? {
                Ok(item) => tests.push((ItemTest::Item(item), false)),
                Err((id, _)) => tests.push((ItemTest::Custom(id), false)),
            }
        }
        if !tests.is_empty() {
            clauses.push(std::mem::take(&mut tests));
        }
        if reader.peek_is('[') {
            reader.skip();
            reader.skip_whitespace();
            let mut clause: Vec<(ItemTest, bool)> = Vec::new();
            while reader.can_read() && !reader.peek_is(']') {
                let negated = if reader.peek_is('!') {
                    reader.skip();
                    reader.skip_whitespace();
                    true
                } else {
                    false
                };
                let start = reader.cursor();
                let id = read_identifier(reader)?;
                let (namespace, path) = id.split_once(':').unwrap_or(("minecraft", &id));
                let name = format!("{namespace}:{path}");
                reader.skip_whitespace();
                let test = if reader.peek_is('~') {
                    reader.skip();
                    reader.skip_whitespace();
                    let numeric = reader
                        .peek()
                        .is_some_and(|c| c.is_ascii_digit() || c == '-' || c == '.');
                    if name == "minecraft:count" || (name == "minecraft:damage" && numeric) {
                        let range = range::parse_ints(reader)?;
                        if name == "minecraft:count" {
                            ItemTest::Count(range)
                        } else {
                            ItemTest::Damage(range)
                        }
                    } else if item_predicate::NAMES.contains(&name.as_str()) {
                        let value = read_nbt_value(reader)?;
                        match ComponentPredicate::from_nbt(&name, &value) {
                            Ok(predicate) => ItemTest::Component(predicate),
                            Err(message) => {
                                reader.set_cursor(start);
                                return Err(CommandError::dynamic(
                                    "arguments.item.predicate.malformed",
                                    [name, message],
                                )
                                .at(reader));
                            }
                        }
                    } else {
                        reader.set_cursor(start);
                        return Err(CommandError::dynamic(
                            "arguments.item.predicate.unknown",
                            [name],
                        )
                        .at(reader));
                    }
                } else if name == "minecraft:count" {
                    if reader.peek_is('=') {
                        reader.skip();
                        reader.skip_whitespace();
                        let value_start = reader.cursor();
                        let value = read_nbt_value(reader)?;
                        match item_predicate::bounds(&value) {
                            Some(range) => ItemTest::Count(range),
                            None => {
                                reader.set_cursor(value_start);
                                return Err(CommandError::dynamic(
                                    "arguments.item.predicate.malformed",
                                    [name, "invalid range".to_owned()],
                                )
                                .at(reader));
                            }
                        }
                    } else {
                        ItemTest::Count(Bounds::ANY)
                    }
                } else {
                    if DataComponentType::from_name(&name).is_none() {
                        reader.set_cursor(start);
                        return Err(CommandError::dynamic(
                            "arguments.item.component.unknown",
                            [name],
                        )
                        .at(reader));
                    }
                    if reader.peek_is('=') {
                        reader.skip();
                        reader.skip_whitespace();
                        ItemTest::ComponentEquals(name, read_nbt_value(reader)?)
                    } else {
                        ItemTest::HasComponent(name)
                    }
                };
                clause.push((test, negated));
                reader.skip_whitespace();
                if reader.peek_is('|') {
                    reader.skip();
                    reader.skip_whitespace();
                } else if reader.peek_is(',') {
                    reader.skip();
                    reader.skip_whitespace();
                    clauses.push(std::mem::take(&mut clause));
                } else if !reader.peek_is(']') {
                    return Err(CommandError::dynamic("parsing.expected", ["]"]).at(reader));
                }
            }
            reader.expect(']')?;
            if !clause.is_empty() {
                clauses.push(clause);
            }
        }
        Ok(ItemPredicate { clauses })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack(input: &str) -> Result<ItemInput, String> {
        parse_item_stack(&mut Reader::new(input)).map_err(|e| e.key().unwrap().to_owned())
    }

    #[tuff_core::watchdog]
    #[test]
    fn parses_items_with_components() {
        let plain = stack("minecraft:stone").unwrap();
        assert_eq!(plain.item, Item::Stone);
        assert!(plain.components.is_empty());
        let sword =
            stack("diamond_sword[damage=5,enchantments={\"minecraft:sharpness\":3},!max_damage]")
                .unwrap();
        assert_eq!(sword.components.damage, Some(5));
        assert_eq!(
            sword
                .components
                .enchantments
                .as_ref()
                .unwrap()
                .level("minecraft:sharpness"),
            3
        );
        assert_eq!(sword.components.removed, ["minecraft:max_damage"]);
        let named = stack("stone[custom_name=\"Rock\"]").unwrap();
        assert_eq!(
            named.components.custom_name.as_ref().unwrap().plain(),
            "Rock"
        );
        assert_eq!(stack("nope"), Err("argument.item.id.invalid".into()));
        assert_eq!(
            stack("stone[nope=1]"),
            Err("arguments.item.component.unknown".into())
        );
        assert_eq!(
            stack("stone[damage=1,damage=2]"),
            Err("arguments.item.component.repeated".into())
        );
        assert_eq!(
            stack("stone[damage=x]"),
            Err("arguments.item.component.malformed".into())
        );
        assert_eq!(
            stack("stone[damage=1,"),
            Err("arguments.item.component.expected".into())
        );
        assert_eq!(stack("stone[damage 1]"), Err("parsing.expected".into()));
        let club = stack(
            "stick[weapon={disable_blocking_for_seconds:5},attribute_modifiers=[{type:\"attack_damage\",\
             id:\"club\",amount:4,operation:\"add_value\",slot:\"mainhand\"}]]",
        )
        .unwrap();
        assert_eq!(
            club.components.weapon.unwrap().disable_blocking_for_seconds,
            5.0
        );
        assert_eq!(
            club.components.attribute_modifiers.as_ref().unwrap()[0].amount,
            4.0
        );
        assert_eq!(
            stack("stick[tool={rules:[{blocks:\"#no_such_tag\"}]}]"),
            Err("arguments.item.component.malformed".into())
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn predicates_match_stacks() {
        let predicate = |input: &str| parse_item_predicate(&mut Reader::new(input)).unwrap();
        let stone = ItemStack::mint(Item::Stone, 5, MintReason::Test);
        let cobblestone = ItemStack::mint(Item::Cobblestone, 1, MintReason::Test);
        let mut sword = ItemStack::mint(Item::DiamondSword, 1, MintReason::Test);
        sword.set_damage(10);
        assert!(predicate("stone").test(&stone));
        assert!(!predicate("stone").test(&sword));
        assert!(predicate("#stone_crafting_materials").test(&cobblestone));
        assert!(!predicate("#stone_crafting_materials").test(&stone));
        assert!(predicate("*").test(&sword));
        assert!(predicate("*[count~5|damage=10]").test(&sword));
        assert!(predicate("*[count~5|damage=10]").test(&stone));
        assert!(!predicate("*[count~9|damage=10]").test(&stone));
        assert!(!predicate("*[count~5,damage=10]").test(&stone));
        assert!(predicate("*[damage]").test(&sword));
        assert!(!predicate("*[damage]").test(&stone));
        assert!(predicate("*[!damage]").test(&stone));
        assert!(predicate("*[damage=10]").test(&sword));
        assert!(!predicate("*[damage=9]").test(&sword));
        assert!(predicate("*[count~5]").test(&stone));
        assert!(!predicate("*[count~..4]").test(&stone));
        assert!(predicate("*[damage~5..15]").test(&sword));
        let err = |input: &str| {
            parse_item_predicate(&mut Reader::new(input))
                .unwrap_err()
                .key()
                .unwrap()
                .to_owned()
        };
        assert_eq!(
            predicate("#nope")
                .resolved(&tuff_datapack::tags::TagSet::vanilla())
                .unwrap_err()
                .key(),
            Some("arguments.item.tag.unknown")
        );
        assert!(
            predicate("#stone_crafting_materials")
                .resolved(&tuff_datapack::tags::TagSet::vanilla())
                .unwrap()
                .test(&cobblestone)
        );
        assert_eq!(err("*[nope~1]"), "arguments.item.predicate.unknown");
        assert_eq!(err("*[nope]"), "arguments.item.component.unknown");
    }

    #[tuff_core::watchdog]
    #[test]
    fn typed_sub_predicates_match_stacks() {
        let predicate = |input: &str| parse_item_predicate(&mut Reader::new(input)).unwrap();
        let err = |input: &str| {
            parse_item_predicate(&mut Reader::new(input))
                .unwrap_err()
                .key()
                .unwrap()
                .to_owned()
        };
        let enchanted = ItemInput::clone(
            &parse_item_stack(&mut Reader::new(
                "diamond_sword[enchantments={\"minecraft:sharpness\":3},damage=10,max_damage=100,\
                 custom_data={a:{b:1},c:2}]",
            ))
            .unwrap(),
        )
        .stack(4);
        let plain = ItemStack::mint(Item::Stone, 1, MintReason::Test);
        assert!(
            predicate("*[enchantments~[{enchantments:\"sharpness\",levels:{min:2}}]]")
                .test(&enchanted)
        );
        assert!(!predicate("*[enchantments~[{levels:{min:4}}]]").test(&enchanted));
        assert!(!predicate("*[enchantments~[{}]]").test(&plain));
        assert!(predicate("*[damage~{damage:10,durability:{max:90}}]").test(&enchanted));
        assert!(!predicate("*[damage~{damage:{min:11}}]").test(&enchanted));
        assert!(predicate("*[custom_data~{a:{b:1}}]").test(&enchanted));
        assert!(!predicate("*[custom_data~{a:{b:2}}]").test(&enchanted));
        assert!(predicate("*[count=4]").test(&enchanted));
        assert!(predicate("*[count={min:2,max:5}]").test(&enchanted));
        assert!(!predicate("*[count=5]").test(&enchanted));
        assert!(predicate("*[count]").test(&plain));
        assert_eq!(
            err("*[enchantments~5]"),
            "arguments.item.predicate.malformed"
        );
        assert_eq!(err("*[nope~{}]"), "arguments.item.predicate.unknown");
    }
}
