use super::*;

// ---------------------------------------------------------------------
// Assembly: turning a placed pumpkin head plus a body of blocks into a
// golem, the way nether portals turn a lit frame into a portal
// (`world::portal`) -- a pure function of a block lookup.
// ---------------------------------------------------------------------

/// A golem body a placed pumpkin completed: what to spawn, where, and
/// which blocks (the pumpkin included) it consumes.
pub struct Assembly {
    pub kind: EntityType,
    pub spawn: [f64; 3],
    pub consumed: Vec<BlockPos>,
    /// A copper golem's body block does not clear to air like the other
    /// golems' -- it becomes a copper chest, oriented like the pumpkin.
    pub replace: Option<(BlockPos, BlockState)>,
    /// A copper golem's starting oxidation stage, matching the block of
    /// copper it was built from.
    pub oxidation: u8,
}

fn is_head(state: Option<BlockState>) -> bool {
    state.is_some_and(|s| matches!(s.block(), Block::CarvedPumpkin | Block::JackOLantern))
}

fn is_block(lookup: &impl Fn(BlockPos) -> Option<BlockState>, pos: BlockPos, block: Block) -> bool {
    lookup(pos).is_some_and(|s| s.block() == block)
}

/// An iron golem: two iron blocks flanking a center column (either
/// horizontal axis), the column's bottom block on the ground and its top
/// block topped by the head.
fn try_iron_golem(
    head: BlockPos,
    lookup: &impl Fn(BlockPos) -> Option<BlockState>,
) -> Option<Assembly> {
    let center = BlockPos::new(head.x, head.y - 1, head.z);
    let bottom = BlockPos::new(center.x, center.y - 1, center.z);
    if !is_block(lookup, center, Block::IronBlock) || !is_block(lookup, bottom, Block::IronBlock) {
        return None;
    }
    for (dx, dz) in [(1, 0), (0, 1)] {
        let arm_a = BlockPos::new(center.x + dx, center.y, center.z + dz);
        let arm_b = BlockPos::new(center.x - dx, center.y, center.z - dz);
        if is_block(lookup, arm_a, Block::IronBlock) && is_block(lookup, arm_b, Block::IronBlock) {
            return Some(Assembly {
                kind: EntityType::IronGolem,
                spawn: [
                    center.x as f64 + 0.5,
                    bottom.y as f64,
                    center.z as f64 + 0.5,
                ],
                consumed: vec![head, center, bottom, arm_a, arm_b],
                replace: None,
                oxidation: 0,
            });
        }
    }
    None
}

/// A snow golem: two snow blocks stacked under the head.
fn try_snow_golem(
    head: BlockPos,
    lookup: &impl Fn(BlockPos) -> Option<BlockState>,
) -> Option<Assembly> {
    let top = BlockPos::new(head.x, head.y - 1, head.z);
    let bottom = BlockPos::new(top.x, top.y - 1, top.z);
    if is_block(lookup, top, Block::SnowBlock) && is_block(lookup, bottom, Block::SnowBlock) {
        Some(Assembly {
            kind: EntityType::SnowGolem,
            spawn: [top.x as f64 + 0.5, bottom.y as f64, top.z as f64 + 0.5],
            consumed: vec![head, top, bottom],
            replace: None,
            oxidation: 0,
        })
    } else {
        None
    }
}

/// The four unwaxed "block of copper" stages, in oxidation order; a waxed
/// block of copper also completes a golem (the wiki: "a waxed block of
/// copper can also be used"), but the golem and its chest come out
/// unwaxed regardless, so only the stage is read from it.
const COPPER_BLOCK_STAGES: [Block; 4] = [
    Block::CopperBlock,
    Block::ExposedCopper,
    Block::WeatheredCopper,
    Block::OxidizedCopper,
];
const WAXED_COPPER_BLOCK_STAGES: [Block; 4] = [
    Block::WaxedCopperBlock,
    Block::WaxedExposedCopper,
    Block::WaxedWeatheredCopper,
    Block::WaxedOxidizedCopper,
];
/// The copper chest each stage in [`COPPER_BLOCK_STAGES`] creates.
const COPPER_CHEST_STAGES: [Block; 4] = [
    Block::CopperChest,
    Block::ExposedCopperChest,
    Block::WeatheredCopperChest,
    Block::OxidizedCopperChest,
];

fn copper_block_stage(block: Block) -> Option<u8> {
    COPPER_BLOCK_STAGES
        .iter()
        .chain(&WAXED_COPPER_BLOCK_STAGES)
        .position(|b| *b == block)
        .map(|i| (i % 4) as u8)
}

/// A copper golem: a single block of copper (any oxidation stage, waxed or
/// not) under the head. The block becomes a copper chest at the matching
/// (always unwaxed) stage, facing the way the head does, joining an
/// adjacent same-facing copper chest exactly like a manually placed one
/// would (see [`placement::chest_state_for_facing`]).
fn try_copper_golem(
    head: BlockPos,
    head_state: BlockState,
    lookup: &impl Fn(BlockPos) -> Option<BlockState>,
) -> Option<Assembly> {
    let below = BlockPos::new(head.x, head.y - 1, head.z);
    let stage = copper_block_stage(lookup(below)?.block())?;
    let facing = head_state
        .facing()
        .unwrap_or(tuff_vanilla_data::block::Facing::North);
    let level = |p: BlockPos| lookup(p).unwrap_or(Block::Air.default_state());
    let chest = placement::chest_state_for_facing(
        COPPER_CHEST_STAGES[stage as usize],
        below,
        facing,
        &level,
    )?;
    Some(Assembly {
        kind: EntityType::CopperGolem,
        spawn: [head.x as f64 + 0.5, head.y as f64, head.z as f64 + 0.5],
        consumed: vec![head],
        replace: Some((below, chest)),
        oxidation: stage,
    })
}

/// Whether placing a pumpkin/jack o'lantern head at `head` completes an
/// iron, snow or copper golem's body, and if so, what to do about it.
/// Checked from wherever a block placement can complete the shape (see
/// `region/blocks.rs`).
pub fn try_assemble(
    head: BlockPos,
    lookup: impl Fn(BlockPos) -> Option<BlockState>,
) -> Option<Assembly> {
    let head_state = lookup(head)?;
    if !is_head(Some(head_state)) {
        return None;
    }
    try_iron_golem(head, &lookup)
        .or_else(|| try_snow_golem(head, &lookup))
        .or_else(|| try_copper_golem(head, head_state, &lookup))
}
