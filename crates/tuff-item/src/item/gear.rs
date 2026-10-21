//! Typed forms of the components that make an item gear: attribute
//! modifiers, tool rules and weapon stats
//! (https://minecraft.wiki/w/Data_component_format), and views that answer
//! the same questions for a stack's own value or its item's default.

use tuff_vanilla_data::item::{EquipmentSlotGroup, ModifierOperation, Weapon};
use tuff_vanilla_data::registry::{Attribute, Block};

use crate::item::hash::Canon;
use crate::text::TextComponent;
use tuff_nbt::{Compound, Tag};

pub const MAX_MODIFIERS: usize = 256;
pub const MAX_TOOL_RULES: usize = 256;
const DEFAULT_MINING_SPEED: f32 = 1.0;
const DEFAULT_DAMAGE_PER_BLOCK: u32 = 1;
const DEFAULT_ITEM_DAMAGE_PER_ATTACK: u64 = 1;

fn number(tag: &Tag) -> Option<f64> {
    match *tag {
        Tag::Byte(v) => Some(v.into()),
        Tag::Short(v) => Some(v.into()),
        Tag::Int(v) => Some(v.into()),
        Tag::Long(v) => Some(v as f64),
        Tag::Float(v) => Some(v.into()),
        Tag::Double(v) => Some(v),
        _ => None,
    }
}

fn boolean(tag: &Tag) -> Option<bool> {
    number(tag).map(|v| v != 0.0)
}

fn non_negative_int(tag: &Tag) -> Option<u32> {
    let value = number(tag)?;
    (value >= 0.0 && value <= i32::MAX as f64).then_some(value as u32)
}

/// A resource location with the default namespace filled in.
fn location(id: &str) -> String {
    if id.contains(':') {
        id.to_owned()
    } else {
        format!("minecraft:{id}")
    }
}

/// How an attribute modifier shows in the tooltip.
#[derive(Debug, Clone, PartialEq)]
pub enum ModifierDisplay {
    Default,
    Hidden,
    /// A text component, as stored.
    Override(Tag),
}

impl ModifierDisplay {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Hidden => "hidden",
            Self::Override(_) => "override",
        }
    }

    /// The display type's network id.
    pub fn network_id(&self) -> u32 {
        tuff_vanilla_data::attribute::MODIFIER_DISPLAYS
            .iter()
            .position(|name| *name == self.name())
            .expect("display types are generated") as u32
    }

    /// The display type a network id names; `None` for `override`, whose
    /// text follows.
    pub fn from_network_id(id: u32) -> Option<Option<Self>> {
        match *tuff_vanilla_data::attribute::MODIFIER_DISPLAYS.get(id as usize)? {
            "default" => Some(Some(Self::Default)),
            "hidden" => Some(Some(Self::Hidden)),
            "override" => Some(None),
            _ => None,
        }
    }

    fn from_nbt(tag: &Tag) -> Option<Self> {
        let map = tag.as_compound()?;
        Some(match map.get_str("type")? {
            "default" => Self::Default,
            "hidden" => Self::Hidden,
            "override" => Self::Override(map.get("value")?.clone()),
            _ => return None,
        })
    }

    fn to_nbt(&self) -> Tag {
        let mut map = Compound::new().with("type", self.name());
        if let Self::Override(text) = self {
            map.insert("value", text.clone());
        }
        Tag::Compound(map)
    }

    fn canon(&self) -> Canon {
        let mut entries = vec![("type".to_owned(), Canon::str(self.name()))];
        if let Self::Override(text) = self {
            let text = TextComponent::from_nbt(text)
                .map_or_else(|| Canon::from_nbt(text), |text| text.canon());
            entries.push(("value".to_owned(), text));
        }
        Canon::Map(entries)
    }
}

/// One entry of a stack's `attribute_modifiers` component.
#[derive(Debug, Clone, PartialEq)]
pub struct AttributeModifier {
    pub attribute: Attribute,
    pub id: String,
    pub amount: f64,
    pub operation: ModifierOperation,
    pub slot: EquipmentSlotGroup,
    pub display: ModifierDisplay,
}

impl AttributeModifier {
    fn from_nbt(tag: &Tag) -> Option<Self> {
        let map = tag.as_compound()?;
        Some(Self {
            attribute: Attribute::from_name(&location(map.get_str("type")?))?,
            id: location(map.get_str("id")?),
            amount: number(map.get("amount")?)?,
            operation: ModifierOperation::from_name(map.get_str("operation")?)?,
            slot: match map.get("slot") {
                None => EquipmentSlotGroup::Any,
                Some(slot) => EquipmentSlotGroup::from_name(slot.as_str()?)?,
            },
            display: match map.get("display") {
                None => ModifierDisplay::Default,
                Some(display) => ModifierDisplay::from_nbt(display)?,
            },
        })
    }

    fn to_nbt(&self) -> Tag {
        let mut map = Compound::new()
            .with("type", self.attribute.name())
            .with("id", self.id.as_str())
            .with("amount", self.amount)
            .with("operation", self.operation.name());
        if self.slot != EquipmentSlotGroup::Any {
            map.insert("slot", self.slot.name());
        }
        if self.display != ModifierDisplay::Default {
            map.insert("display", self.display.to_nbt());
        }
        Tag::Compound(map)
    }

    fn canon(&self) -> Canon {
        let mut entries = vec![
            ("type".to_owned(), Canon::str(self.attribute.name())),
            ("id".to_owned(), Canon::str(self.id.clone())),
            ("amount".to_owned(), Canon::Double(self.amount)),
            ("operation".to_owned(), Canon::str(self.operation.name())),
        ];
        if self.slot != EquipmentSlotGroup::Any {
            entries.push(("slot".to_owned(), Canon::str(self.slot.name())));
        }
        if self.display != ModifierDisplay::Default {
            entries.push(("display".to_owned(), self.display.canon()));
        }
        Canon::Map(entries)
    }
}

pub fn modifiers_from_nbt(tag: &Tag) -> Option<Vec<AttributeModifier>> {
    let entries = tag.as_list()?;
    if entries.len() > MAX_MODIFIERS {
        return None;
    }
    entries.iter().map(AttributeModifier::from_nbt).collect()
}

pub fn modifiers_to_nbt(modifiers: &[AttributeModifier]) -> Tag {
    Tag::List(modifiers.iter().map(AttributeModifier::to_nbt).collect())
}

pub fn modifiers_canon(modifiers: &[AttributeModifier]) -> Canon {
    Canon::List(modifiers.iter().map(AttributeModifier::canon).collect())
}

/// The blocks a tool rule covers: a block tag or a list of blocks.
#[derive(Debug, Clone, PartialEq)]
pub enum BlockSet {
    Tag {
        /// The tag's id, without `#`.
        name: String,
        key: tuff_vanilla_data::tag::Key<Block>,
    },
    Blocks(Vec<Block>),
}

impl BlockSet {
    pub fn tag(name: &str) -> Option<Self> {
        let name = location(name);
        let index = tuff_vanilla_data::tag::block::ALL
            .iter()
            .position(|(tag, _)| *tag == name)?;
        let (static_name, members) = tuff_vanilla_data::tag::block::ALL[index];
        let key = tuff_vanilla_data::tag::Key {
            registry: tuff_vanilla_data::tag::block::key::LOGS.registry,
            index,
            name: static_name,
            members,
        };
        Some(Self::Tag { name, key })
    }

    /// Whether the set holds `block`, a tag's members as the tags in
    /// effect have them.
    pub fn contains(&self, block: Block) -> bool {
        match self {
            Self::Tag { key, .. } => tuff_datapack::tags::is(*key, block),
            Self::Blocks(blocks) => blocks.contains(&block),
        }
    }

    fn from_nbt(tag: &Tag) -> Option<Self> {
        match tag {
            Tag::String(id) => match id.strip_prefix('#') {
                Some(name) => Self::tag(name),
                None => Some(Self::Blocks(vec![Block::from_name(&location(id))?])),
            },
            Tag::List(ids) => ids
                .iter()
                .map(|id| Block::from_name(&location(id.as_str()?)))
                .collect::<Option<Vec<_>>>()
                .map(Self::Blocks),
            _ => None,
        }
    }

    fn to_nbt(&self) -> Tag {
        match self {
            Self::Tag { name, .. } => Tag::String(format!("#{name}")),
            Self::Blocks(blocks) if blocks.len() == 1 => Tag::String(blocks[0].name().to_owned()),
            Self::Blocks(blocks) => Tag::List(
                blocks
                    .iter()
                    .map(|block| Tag::String(block.name().to_owned()))
                    .collect(),
            ),
        }
    }

    fn canon(&self) -> Canon {
        match self {
            Self::Tag { name, .. } => Canon::str(format!("#{name}")),
            Self::Blocks(blocks) if blocks.len() == 1 => Canon::str(blocks[0].name()),
            Self::Blocks(blocks) => Canon::List(
                blocks
                    .iter()
                    .map(|block| Canon::str(block.name()))
                    .collect(),
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolRule {
    pub blocks: BlockSet,
    pub speed: Option<f32>,
    pub correct_for_drops: Option<bool>,
}

/// A stack's own `tool` component.
#[derive(Debug, Clone, PartialEq)]
pub struct Tool {
    pub rules: Vec<ToolRule>,
    pub default_mining_speed: f32,
    pub damage_per_block: u32,
    pub can_destroy_blocks_in_creative: bool,
}

impl Tool {
    pub fn from_nbt(tag: &Tag) -> Option<Self> {
        let map = tag.as_compound()?;
        let rules = map.get_list("rules")?;
        if rules.len() > MAX_TOOL_RULES {
            return None;
        }
        let rules = rules
            .iter()
            .map(|rule| {
                let rule = rule.as_compound()?;
                Some(ToolRule {
                    blocks: BlockSet::from_nbt(rule.get("blocks")?)?,
                    speed: match rule.get("speed") {
                        None => None,
                        Some(speed) => Some(number(speed).filter(|s| *s > 0.0)? as f32),
                    },
                    correct_for_drops: match rule.get("correct_for_drops") {
                        None => None,
                        Some(correct) => Some(boolean(correct)?),
                    },
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            rules,
            default_mining_speed: match map.get("default_mining_speed") {
                None => DEFAULT_MINING_SPEED,
                Some(speed) => number(speed)? as f32,
            },
            damage_per_block: match map.get("damage_per_block") {
                None => DEFAULT_DAMAGE_PER_BLOCK,
                Some(damage) => non_negative_int(damage)?,
            },
            can_destroy_blocks_in_creative: match map.get("can_destroy_blocks_in_creative") {
                None => true,
                Some(can) => boolean(can)?,
            },
        })
    }

    pub fn to_nbt(&self) -> Tag {
        let rules = self
            .rules
            .iter()
            .map(|rule| {
                let mut map = Compound::new().with("blocks", rule.blocks.to_nbt());
                if let Some(speed) = rule.speed {
                    map.insert("speed", speed);
                }
                if let Some(correct) = rule.correct_for_drops {
                    map.insert("correct_for_drops", correct);
                }
                Tag::Compound(map)
            })
            .collect();
        let mut map = Compound::new().with("rules", Tag::List(rules));
        if self.default_mining_speed != DEFAULT_MINING_SPEED {
            map.insert("default_mining_speed", self.default_mining_speed);
        }
        if self.damage_per_block != DEFAULT_DAMAGE_PER_BLOCK {
            map.insert("damage_per_block", self.damage_per_block as i32);
        }
        if !self.can_destroy_blocks_in_creative {
            map.insert("can_destroy_blocks_in_creative", false);
        }
        Tag::Compound(map)
    }

    pub fn canon(&self) -> Canon {
        let rules = self
            .rules
            .iter()
            .map(|rule| {
                let mut entries = vec![("blocks".to_owned(), rule.blocks.canon())];
                if let Some(speed) = rule.speed {
                    entries.push(("speed".to_owned(), Canon::Float(speed)));
                }
                if let Some(correct) = rule.correct_for_drops {
                    entries.push(("correct_for_drops".to_owned(), Canon::Bool(correct)));
                }
                Canon::Map(entries)
            })
            .collect();
        let mut entries = vec![("rules".to_owned(), Canon::List(rules))];
        if self.default_mining_speed != DEFAULT_MINING_SPEED {
            entries.push((
                "default_mining_speed".to_owned(),
                Canon::Float(self.default_mining_speed),
            ));
        }
        if self.damage_per_block != DEFAULT_DAMAGE_PER_BLOCK {
            entries.push((
                "damage_per_block".to_owned(),
                Canon::Int(self.damage_per_block as i32),
            ));
        }
        if !self.can_destroy_blocks_in_creative {
            entries.push((
                "can_destroy_blocks_in_creative".to_owned(),
                Canon::Bool(false),
            ));
        }
        Canon::Map(entries)
    }
}

/// Whether a built-in tool rule covers `block`, reading its tag in the tags
/// in effect.
fn default_rule_matches(rule: &tuff_vanilla_data::ToolRule, block: Block) -> bool {
    match rule.tag {
        Some(key) => tuff_datapack::tags::is(key, block),
        None => rule.matches(block),
    }
}

/// A stack's tool, from its own component or its item's default.
#[derive(Debug, Clone, Copy)]
pub enum ToolRef<'a> {
    Default(&'static tuff_vanilla_data::Tool),
    Own(&'a Tool),
}

impl ToolRef<'_> {
    /// The speed of the first rule matching `block` that sets one.
    pub fn mining_speed(self, block: Block) -> f32 {
        match self {
            Self::Default(tool) => tool
                .rules
                .iter()
                .filter(|rule| default_rule_matches(rule, block))
                .find_map(|rule| rule.speed)
                .unwrap_or(tool.default_mining_speed),
            Self::Own(tool) => tool
                .rules
                .iter()
                .filter(|rule| rule.blocks.contains(block))
                .find_map(|rule| rule.speed)
                .unwrap_or(tool.default_mining_speed),
        }
    }

    /// The verdict of the first rule matching `block` that has one; false
    /// if no rule does.
    pub fn is_correct_for_drops(self, block: Block) -> bool {
        match self {
            Self::Default(tool) => tool
                .rules
                .iter()
                .filter(|rule| default_rule_matches(rule, block))
                .find_map(|rule| rule.correct_for_drops)
                .unwrap_or(false),
            Self::Own(tool) => tool
                .rules
                .iter()
                .filter(|rule| rule.blocks.contains(block))
                .find_map(|rule| rule.correct_for_drops)
                .unwrap_or(false),
        }
    }

    pub fn damage_per_block(self) -> u32 {
        match self {
            Self::Default(tool) => tool.damage_per_block,
            Self::Own(tool) => tool.damage_per_block,
        }
    }

    pub fn can_destroy_blocks_in_creative(self) -> bool {
        match self {
            Self::Default(tool) => tool.can_destroy_blocks_in_creative,
            Self::Own(tool) => tool.can_destroy_blocks_in_creative,
        }
    }
}

pub fn weapon_from_nbt(tag: &Tag) -> Option<Weapon> {
    let map = tag.as_compound()?;
    Some(Weapon {
        item_damage_per_attack: match map.get("item_damage_per_attack") {
            None => DEFAULT_ITEM_DAMAGE_PER_ATTACK,
            Some(damage) => non_negative_int(damage)?.into(),
        },
        disable_blocking_for_seconds: match map.get("disable_blocking_for_seconds") {
            None => 0.0,
            Some(seconds) => number(seconds).filter(|s| *s >= 0.0)? as f32,
        },
    })
}

pub fn weapon_to_nbt(weapon: &Weapon) -> Tag {
    let mut map = Compound::new();
    if weapon.item_damage_per_attack != DEFAULT_ITEM_DAMAGE_PER_ATTACK {
        map.insert(
            "item_damage_per_attack",
            weapon.item_damage_per_attack as i32,
        );
    }
    if weapon.disable_blocking_for_seconds != 0.0 {
        map.insert(
            "disable_blocking_for_seconds",
            weapon.disable_blocking_for_seconds,
        );
    }
    Tag::Compound(map)
}

pub fn weapon_canon(weapon: &Weapon) -> Canon {
    let mut entries = Vec::new();
    if weapon.item_damage_per_attack != DEFAULT_ITEM_DAMAGE_PER_ATTACK {
        entries.push((
            "item_damage_per_attack".to_owned(),
            Canon::Int(weapon.item_damage_per_attack as i32),
        ));
    }
    if weapon.disable_blocking_for_seconds != 0.0 {
        entries.push((
            "disable_blocking_for_seconds".to_owned(),
            Canon::Float(weapon.disable_blocking_for_seconds),
        ));
    }
    Canon::Map(entries)
}

/// The values whose encodings the tests compare with the game's.
#[doc(hidden)]
pub mod samples {
    use super::*;

    pub fn weapon() -> Weapon {
        Weapon {
            item_damage_per_attack: 3,
            disable_blocking_for_seconds: 2.5,
        }
    }

    pub fn default_weapon() -> Weapon {
        Weapon {
            item_damage_per_attack: 1,
            disable_blocking_for_seconds: 0.0,
        }
    }

    pub fn tool() -> Tool {
        Tool {
            rules: vec![
                ToolRule {
                    blocks: BlockSet::Blocks(vec![Block::Stone, Block::Dirt]),
                    speed: Some(4.0),
                    correct_for_drops: Some(true),
                },
                ToolRule {
                    blocks: BlockSet::tag("minecraft:mineable/pickaxe").unwrap(),
                    speed: None,
                    correct_for_drops: Some(false),
                },
                ToolRule {
                    blocks: BlockSet::Blocks(vec![Block::Cobweb]),
                    speed: Some(15.0),
                    correct_for_drops: None,
                },
            ],
            default_mining_speed: 2.0,
            damage_per_block: 3,
            can_destroy_blocks_in_creative: false,
        }
    }

    pub fn default_tool() -> Tool {
        Tool {
            rules: vec![ToolRule {
                blocks: BlockSet::tag("mineable/pickaxe").unwrap(),
                speed: Some(8.0),
                correct_for_drops: Some(true),
            }],
            default_mining_speed: 1.0,
            damage_per_block: 1,
            can_destroy_blocks_in_creative: true,
        }
    }

    pub fn modifiers() -> Vec<AttributeModifier> {
        vec![
            AttributeModifier {
                attribute: Attribute::AttackDamage,
                id: "minecraft:test_damage".into(),
                amount: 5.0,
                operation: ModifierOperation::AddValue,
                slot: EquipmentSlotGroup::Mainhand,
                display: ModifierDisplay::Default,
            },
            AttributeModifier {
                attribute: Attribute::Armor,
                id: "custom:armor".into(),
                amount: 0.25,
                operation: ModifierOperation::AddMultipliedTotal,
                slot: EquipmentSlotGroup::Any,
                display: ModifierDisplay::Hidden,
            },
            AttributeModifier {
                attribute: Attribute::MovementSpeed,
                id: "minecraft:speedy".into(),
                amount: -0.1,
                operation: ModifierOperation::AddMultipliedBase,
                slot: EquipmentSlotGroup::Feet,
                display: ModifierDisplay::Override(Tag::String("Fast".into())),
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The built-in tags with `extra` added to the block tag `tag`.
    fn with_block(tag: &str, extra: &str) -> std::sync::Arc<tuff_datapack::tags::TagSet> {
        use tuff_datapack::tags::{TagEntry, TagSet, vanilla_builders};
        let mut builders = vanilla_builders();
        builders
            .get_mut("minecraft:block")
            .unwrap()
            .get_mut(tag)
            .unwrap()
            .push(TagEntry {
                id: extra.to_owned(),
                tag: false,
                required: true,
            });
        std::sync::Arc::new(TagSet::resolve_all(&builders).0)
    }

    #[tuff_core::watchdog]
    #[test]
    fn tool_rules_read_block_tags_in_effect() {
        let pickaxe = crate::item::ItemStack::new(tuff_vanilla_data::Item::WoodenPickaxe, 1);
        let tool = pickaxe.tool().unwrap();
        assert_eq!(tool.mining_speed(Block::Dirt), 1.0);
        assert!(tool.is_correct_for_drops(Block::Stone));
        let dirt = with_block("minecraft:mineable/pickaxe", "minecraft:dirt");
        assert_eq!(
            tuff_datapack::tags::sync_scope(dirt, || tool.mining_speed(Block::Dirt)),
            2.0
        );
        let stone = with_block("minecraft:incorrect_for_wooden_tool", "minecraft:stone");
        assert!(!tuff_datapack::tags::sync_scope(stone, || {
            tool.is_correct_for_drops(Block::Stone)
        }));
        let own = Tool::from_nbt(&snbt(
            "{rules:[{blocks:\"#minecraft:mineable/pickaxe\",speed:7.0f,correct_for_drops:1b}]}",
        ))
        .unwrap();
        let glass = with_block("minecraft:mineable/pickaxe", "minecraft:glass");
        assert_eq!(ToolRef::Own(&own).mining_speed(Block::Glass), 1.0);
        assert_eq!(
            tuff_datapack::tags::sync_scope(glass, || ToolRef::Own(&own)
                .mining_speed(Block::Glass)),
            7.0
        );
    }

    /// The stored forms the game wrote for the sample values.
    const WEAPON_SNBT: &str = "{disable_blocking_for_seconds:2.5f,item_damage_per_attack:3}";
    const TOOL_SNBT: &str = "{can_destroy_blocks_in_creative:0b,damage_per_block:3,\
        default_mining_speed:2.0f,rules:[{blocks:[\"minecraft:stone\",\"minecraft:dirt\"],\
        correct_for_drops:1b,speed:4.0f},{blocks:\"#minecraft:mineable/pickaxe\",\
        correct_for_drops:0b},{blocks:\"minecraft:cobweb\",speed:15.0f}]}";
    const MODIFIERS_SNBT: &str = "[{amount:5.0d,id:\"minecraft:test_damage\",\
        operation:\"add_value\",slot:\"mainhand\",type:\"minecraft:attack_damage\"},\
        {amount:0.25d,display:{type:\"hidden\"},id:\"custom:armor\",\
        operation:\"add_multiplied_total\",type:\"minecraft:armor\"},\
        {amount:-0.1d,display:{type:\"override\",value:\"Fast\"},id:\"minecraft:speedy\",\
        operation:\"add_multiplied_base\",slot:\"feet\",type:\"minecraft:movement_speed\"}]";

    fn snbt(text: &str) -> Tag {
        Tag::from_snbt(text).unwrap()
    }

    #[tuff_core::watchdog]
    #[test]
    fn stored_forms_read_the_games_and_round_trip() {
        assert_eq!(weapon_from_nbt(&snbt(WEAPON_SNBT)), Some(samples::weapon()));
        assert_eq!(
            weapon_from_nbt(&snbt("{}")),
            Some(samples::default_weapon())
        );
        assert_eq!(
            weapon_to_nbt(&samples::default_weapon()),
            Tag::Compound(Compound::new())
        );
        assert_eq!(Tool::from_nbt(&snbt(TOOL_SNBT)), Some(samples::tool()));
        assert_eq!(
            modifiers_from_nbt(&snbt(MODIFIERS_SNBT)),
            Some(samples::modifiers())
        );
        let tool = samples::tool();
        assert_eq!(Tool::from_nbt(&tool.to_nbt()), Some(tool));
        let modifiers = samples::modifiers();
        assert_eq!(
            modifiers_from_nbt(&modifiers_to_nbt(&modifiers)),
            Some(modifiers)
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn stored_forms_are_read_leniently_but_rejected_when_invalid() {
        let short = snbt("[{amount:2,id:\"boost\",operation:\"add_value\",type:\"armor\"}]");
        let parsed = modifiers_from_nbt(&short).unwrap();
        assert_eq!(parsed[0].id, "minecraft:boost");
        assert_eq!(parsed[0].attribute, Attribute::Armor);
        assert_eq!(parsed[0].amount, 2.0);
        assert_eq!(parsed[0].slot, EquipmentSlotGroup::Any);
        for bad in [
            "[{amount:1,id:\"x\",operation:\"add_value\",type:\"minecraft:nope\"}]",
            "[{amount:1,id:\"x\",operation:\"double\",type:\"armor\"}]",
            "[{id:\"x\",operation:\"add_value\",type:\"armor\"}]",
            "{modifiers:[]}",
        ] {
            assert_eq!(modifiers_from_nbt(&snbt(bad)), None, "{bad}");
        }
        for bad in [
            "{rules:[{blocks:\"#minecraft:no_such_tag\"}]}",
            "{rules:[{blocks:\"minecraft:no_such_block\"}]}",
            "{rules:[{blocks:\"stone\",speed:0.0f}]}",
            "{rules:[],damage_per_block:-1}",
            "{default_mining_speed:2.0f}",
        ] {
            assert_eq!(Tool::from_nbt(&snbt(bad)), None, "{bad}");
        }
        assert_eq!(
            weapon_from_nbt(&snbt("{disable_blocking_for_seconds:-1.0f}")),
            None
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn own_tools_answer_like_default_ones() {
        let tool = samples::tool();
        let own = ToolRef::Own(&tool);
        assert_eq!(own.mining_speed(Block::Stone), 4.0);
        assert!(own.is_correct_for_drops(Block::Stone));
        assert!(!own.is_correct_for_drops(Block::Obsidian));
        assert_eq!(
            own.mining_speed(Block::Obsidian),
            2.0,
            "the tag rule has no speed"
        );
        assert_eq!(own.mining_speed(Block::Cobweb), 15.0);
        assert!(!own.is_correct_for_drops(Block::Cobweb));
        assert_eq!(own.damage_per_block(), 3);
        assert!(!own.can_destroy_blocks_in_creative());
        let pickaxe = ToolRef::Default(tuff_vanilla_data::Item::DiamondPickaxe.tool().unwrap());
        assert_eq!(pickaxe.mining_speed(Block::Stone), 8.0);
        assert!(pickaxe.is_correct_for_drops(Block::Obsidian));
    }
}
