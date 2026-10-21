//! Block arguments: a block with properties (`oak_stairs[facing=east]`),
//! and block predicates that also accept tags (`#logs[axis=y]`).

use tuff_vanilla_data::{Block, BlockState};

use tuff_nbt::Compound;

use super::args::{read_identifier, read_nbt_compound};
use super::error::CommandError;
use super::reader::Reader;

/// A block to place: its state plus which properties were named, so a
/// clone or fill can tell an explicit `[lit=false]` from a default.
#[derive(Debug, Clone, PartialEq)]
pub struct BlockInput {
    pub state: BlockState,
    pub properties: Vec<&'static str>,
    pub nbt: Option<Compound>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BlockPredicate {
    Block(BlockInput),
    /// A block tag; `blocks` are the built-in pack's members until
    /// [`BlockPredicate::resolved`] reads the tags in effect.
    Tag {
        name: String,
        blocks: tuff_vanilla_data::Frozen<[Block]>,
        properties: Vec<(String, String)>,
        nbt: Option<Compound>,
    },
}

impl BlockPredicate {
    /// This predicate with its tag's members read from `tags` (data packs
    /// may change them); an unknown tag is an error.
    pub fn resolved(mut self, tags: &tuff_datapack::tags::TagSet) -> Result<Self, CommandError> {
        if let Self::Tag { name, blocks, .. } = &mut self {
            let members = tags.typed::<Block>(name).ok_or_else(|| {
                CommandError::dynamic("arguments.block.tag.unknown", [name.clone()])
            })?;
            *blocks = members.into();
        }
        Ok(self)
    }

    /// Whether `state` matches, ignoring block entity data.
    pub fn test(&self, state: BlockState) -> bool {
        match self {
            Self::Block(input) => {
                input.state.block() == state.block()
                    && input
                        .properties
                        .iter()
                        .all(|name| state.get(name) == input.state.get(name))
            }
            Self::Tag {
                blocks, properties, ..
            } => {
                blocks.contains(&state.block())
                    && properties
                        .iter()
                        .all(|(name, value)| state.get(name) == Some(value.as_str()))
            }
        }
    }
}

fn has_property(block: Block, name: &str) -> bool {
    block.default_state().properties().any(|(n, _)| n == name)
}

fn read_block(reader: &mut Reader<'_>) -> Result<(Block, String), CommandError> {
    let start = reader.cursor();
    let id = read_identifier(reader)?;
    match Block::from_name(&id) {
        Some(block) => Ok((block, id)),
        None => {
            reader.set_cursor(start);
            Err(CommandError::dynamic("argument.block.id.invalid", [id]).at(reader))
        }
    }
}

fn read_properties(
    reader: &mut Reader<'_>,
    block: Block,
    id: &str,
) -> Result<(BlockState, Vec<&'static str>), CommandError> {
    reader.skip();
    reader.skip_whitespace();
    let mut state = block.default_state();
    let mut names: Vec<&'static str> = Vec::new();
    while reader.can_read() && !reader.peek_is(']') {
        reader.skip_whitespace();
        let key = reader.read_string()?;
        let Some((name, _)) = block.default_state().properties().find(|(n, _)| *n == key) else {
            return Err(
                CommandError::dynamic("argument.block.property.unknown", [id, &key]).at(reader),
            );
        };
        if names.contains(&name) {
            return Err(
                CommandError::dynamic("argument.block.property.duplicate", [&key, id]).at(reader),
            );
        }
        reader.skip_whitespace();
        if !reader.peek_is('=') {
            return Err(
                CommandError::dynamic("argument.block.property.novalue", [&key, id]).at(reader),
            );
        }
        reader.skip();
        reader.skip_whitespace();
        let value_start = reader.cursor();
        let value = reader.read_string()?;
        match state.with(name, &value) {
            Some(next) => state = next,
            None => {
                reader.set_cursor(value_start);
                return Err(CommandError::dynamic(
                    "argument.block.property.invalid",
                    [id, &value, &key],
                )
                .at(reader));
            }
        }
        names.push(name);
        reader.skip_whitespace();
        if !reader.can_read() {
            break;
        }
        if reader.peek_is(',') {
            reader.skip();
        } else if !reader.peek_is(']') {
            return Err(CommandError::simple("argument.block.property.unclosed").at(reader));
        }
    }
    if reader.peek_is(']') {
        reader.skip();
        return Ok((state, names));
    }
    Err(CommandError::simple("argument.block.property.unclosed").at(reader))
}

fn read_vague_properties(
    reader: &mut Reader<'_>,
    id: &str,
) -> Result<Vec<(String, String)>, CommandError> {
    reader.skip();
    reader.skip_whitespace();
    let mut properties: Vec<(String, String)> = Vec::new();
    while reader.can_read() && !reader.peek_is(']') {
        reader.skip_whitespace();
        let key = reader.read_string()?;
        if properties.iter().any(|(k, _)| *k == key) {
            return Err(
                CommandError::dynamic("argument.block.property.duplicate", [&key, id]).at(reader),
            );
        }
        reader.skip_whitespace();
        if !reader.peek_is('=') {
            return Err(
                CommandError::dynamic("argument.block.property.novalue", [&key, id]).at(reader),
            );
        }
        reader.skip();
        reader.skip_whitespace();
        let value = reader.read_string()?;
        properties.push((key, value));
        reader.skip_whitespace();
        if !reader.can_read() {
            break;
        }
        if reader.peek_is(',') {
            reader.skip();
        } else if !reader.peek_is(']') {
            return Err(CommandError::simple("argument.block.property.unclosed").at(reader));
        }
    }
    if reader.peek_is(']') {
        reader.skip();
        return Ok(properties);
    }
    Err(CommandError::simple("argument.block.property.unclosed").at(reader))
}

fn read_optional_nbt(reader: &mut Reader<'_>) -> Result<Option<Compound>, CommandError> {
    if reader.peek_is('{') {
        return read_nbt_compound(reader).map(Some);
    }
    Ok(None)
}

/// `block_state`: a block and optional properties and NBT.
pub fn parse_block_state(reader: &mut Reader<'_>) -> Result<BlockInput, CommandError> {
    if reader.peek_is('#') {
        return Err(CommandError::simple("argument.block.tag.disallowed").at(reader));
    }
    let (block, id) = read_block(reader)?;
    let (state, properties) = if reader.peek_is('[') {
        read_properties(reader, block, &id)?
    } else {
        (block.default_state(), Vec::new())
    };
    let nbt = read_optional_nbt(reader)?;
    Ok(BlockInput {
        state,
        properties,
        nbt,
    })
}

/// `block_predicate`: a block or `#tag`, with optional properties and NBT.
pub fn parse_block_predicate(reader: &mut Reader<'_>) -> Result<BlockPredicate, CommandError> {
    if reader.peek_is('#') {
        reader.skip();
        let id = read_identifier(reader)?;
        let (namespace, path) = id.split_once(':').unwrap_or(("minecraft", &id));
        let name = format!("{namespace}:{path}");
        let blocks: tuff_vanilla_data::Frozen<[Block]> = tuff_vanilla_data::tag::block::ALL
            .iter()
            .find(|(tag, _)| *tag == name)
            .map_or_else(Default::default, |(_, blocks)| {
                tuff_vanilla_data::Frozen::Static(*blocks)
            });
        let properties = if reader.peek_is('[') {
            read_vague_properties(reader, &name)?
        } else {
            Vec::new()
        };
        let nbt = read_optional_nbt(reader)?;
        return Ok(BlockPredicate::Tag {
            name,
            blocks,
            properties,
            nbt,
        });
    }
    parse_block_state(reader).map(BlockPredicate::Block)
}

/// Whether the named property exists on the block, for suggestions.
pub fn property_names(block: Block) -> impl Iterator<Item = &'static str> {
    block.default_state().properties().map(|(name, _)| name)
}

pub fn is_property(block: Block, name: &str) -> bool {
    has_property(block, name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(input: &str) -> Result<BlockInput, String> {
        parse_block_state(&mut Reader::new(input)).map_err(|e| e.key().unwrap().to_owned())
    }

    #[tuff_core::watchdog]
    #[test]
    fn parses_blocks_with_properties() {
        let stairs = state("oak_stairs[facing=east,half=top]").unwrap();
        assert_eq!(stairs.state.get("facing"), Some("east"));
        assert_eq!(stairs.state.get("half"), Some("top"));
        assert_eq!(stairs.properties, ["facing", "half"]);
        assert_eq!(
            state("minecraft:stone").unwrap().state,
            Block::Stone.default_state()
        );
        let nbt = state("chest{Items:[]}").unwrap();
        assert!(nbt.nbt.is_some());
        assert_eq!(state("nope"), Err("argument.block.id.invalid".into()));
        assert_eq!(
            state("stone[x=1]"),
            Err("argument.block.property.unknown".into())
        );
        assert_eq!(
            state("oak_stairs[facing=up]"),
            Err("argument.block.property.invalid".into())
        );
        assert_eq!(
            state("oak_stairs[facing=east,facing=west]"),
            Err("argument.block.property.duplicate".into())
        );
        assert_eq!(
            state("oak_stairs[facing]"),
            Err("argument.block.property.novalue".into())
        );
        assert_eq!(
            state("oak_stairs[facing=east"),
            Err("argument.block.property.unclosed".into())
        );
        assert_eq!(state("#logs"), Err("argument.block.tag.disallowed".into()));
    }

    #[tuff_core::watchdog]
    #[test]
    fn predicates_match_blocks_and_tags() {
        let predicate = |input: &str| parse_block_predicate(&mut Reader::new(input)).unwrap();
        let stairs = Block::OakStairs
            .state_with([("facing", "east"), ("half", "top")])
            .unwrap();
        assert!(predicate("oak_stairs").test(stairs));
        assert!(predicate("oak_stairs[facing=east]").test(stairs));
        assert!(!predicate("oak_stairs[facing=west]").test(stairs));
        assert!(!predicate("stone").test(stairs));
        assert!(predicate("#stairs").test(stairs));
        assert!(predicate("#stairs[half=top]").test(stairs));
        assert!(!predicate("#stairs[half=bottom]").test(stairs));
        assert!(!predicate("#logs").test(stairs));
        let pack_tag = predicate("#nope");
        assert!(
            !pack_tag.test(stairs),
            "a tag only packs may define matches nothing yet"
        );
        assert_eq!(
            pack_tag
                .resolved(&tuff_datapack::tags::TagSet::vanilla())
                .unwrap_err()
                .key(),
            Some("arguments.block.tag.unknown")
        );
        assert!(
            predicate("#stairs")
                .resolved(&tuff_datapack::tags::TagSet::vanilla())
                .unwrap()
                .test(stairs)
        );
    }
}
