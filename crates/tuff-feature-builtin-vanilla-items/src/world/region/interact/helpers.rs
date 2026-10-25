use super::*;

/// The request that throws an eye of ender from `eye` along `look`: unless
/// it looks at an end portal frame within `reach`, the nearest stronghold
/// is looked up off the actor and the player answered with
/// [`PlayerEvent::EyeTarget`].
pub fn locate_eye_request(
    player: PlayerHandle,
    eye: [f64; 3],
    look: [f64; 3],
    reach: f64,
    from: BlockPos,
    off_hand: bool,
) -> RegionRequest {
    RegionRequest::Run(RegionTask::new(move |world| {
        Interact::locate_eye(world, player, eye, look, reach, from, off_hand)
    }))
}

/// The block that a `potted_*` flower pot variant holds.
pub(super) fn potted_content(block: Block) -> Option<Block> {
    let name = block.name().strip_prefix("minecraft:potted_")?;
    Block::from_name(&format!("minecraft:{name}"))
}

/// The flower pot variant holding `content`.
pub(super) fn potted_block_for(content: Block) -> Option<Block> {
    let name = content.name().strip_prefix("minecraft:")?;
    Block::from_name(&format!("minecraft:potted_{name}"))
}
