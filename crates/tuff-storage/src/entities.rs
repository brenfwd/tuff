//! Entity chunks (`entities/r.<x>.<z>.mca`): `{DataVersion, Position: [x, z],
//! Entities: [...]}`. Entities stay NBT here; the world turns them into live
//! entities by type (`world::entity::Registry`).

use tuff_vanilla_data::version::DATA_VERSION;

use crate::error::ChunkNbtError;
use tuff_nbt::{Compound, Tag};
use tuff_world_model::pos::ChunkPos;

pub fn decode(root: &Compound, expected: ChunkPos) -> Result<Vec<Compound>, ChunkNbtError> {
    decode_owned(root.clone(), expected)
}

/// [`decode`] taking the root by value, which saves copying its entities.
pub fn decode_owned(
    mut root: Compound,
    expected: ChunkPos,
) -> Result<Vec<Compound>, ChunkNbtError> {
    let version = root.get_int("DataVersion").unwrap_or(0);
    if version > DATA_VERSION {
        return Err(ChunkNbtError::NewerVersion(version));
    }
    crate::datafix::entity_chunk(&mut root)?;
    if let Some(&[x, z]) = root.get_int_array("Position")
        && ChunkPos::new(x, z) != expected
    {
        return Err(ChunkNbtError::WrongPosition(ChunkPos::new(x, z)));
    }
    Ok(match root.remove("Entities") {
        Some(Tag::List(entities)) => entities
            .into_iter()
            .filter_map(|entity| match entity {
                Tag::Compound(entity) => Some(entity),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    })
}

/// `None` when the chunk has no entities, which vanilla stores by clearing
/// the chunk from the entity region file.
pub fn encode(pos: ChunkPos, entities: &[Compound]) -> Option<Compound> {
    if entities.is_empty() {
        return None;
    }
    Some(
        Compound::new()
            .with("DataVersion", DATA_VERSION)
            .with("Entities", entities.to_vec())
            .with("Position", vec![pos.x, pos.z]),
    )
}
