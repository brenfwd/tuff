use super::*;

impl RegionActor {
    /// Lets the block at `pos` react to its neighbour in `direction`
    /// becoming `neighbour`, replacing or destroying it as the reaction
    /// says.
    pub(in crate::world::region) fn run_shape_update(
        &mut self,
        direction: Facing,
        pos: BlockPos,
        neighbour_pos: BlockPos,
        neighbour: BlockState,
        flags: u32,
        limit: u32,
    ) {
        let chunk = ChunkPos::of_block(pos);
        if !self.pos.contains(chunk) {
            self.queue_remote(
                pos,
                BlockUpdate::Shape {
                    pos,
                    direction,
                    neighbour,
                    flags,
                    limit,
                    cause: self.sim.cause,
                },
            );
            return;
        }
        let _ = neighbour_pos;
        let Some(old) = RegionActor::block(self, pos) else {
            return;
        };
        if flags & UPDATE_SKIP_SHAPE_UPDATE_ON_WIRE != 0 && old.block() == Block::RedstoneWire {
            return;
        }
        if is_air(old) {
            return;
        }
        self.fluid_shape_changed(pos, neighbour);
        let Some(old) = RegionActor::block(self, pos) else {
            return;
        };
        self.shape_tick(pos, old, direction, neighbour);
        let Some(new) = self.shape_for(pos, old, direction, neighbour) else {
            return;
        };
        let new = if !is_air(new)
            && !self
                .shared
                .growth_behaviors
                .clone()
                .light_allows(self, new, pos)
        {
            AIR
        } else {
            new
        };
        if new == old {
            return;
        }
        if is_air(new) {
            self.destroy_by_update(pos, old, flags & UPDATE_SUPPRESS_DROPS == 0, limit);
        } else {
            self.set_block_flags(pos, new, flags & !UPDATE_SUPPRESS_DROPS, limit);
        }
    }

    /// The shape the block at `pos` takes for its neighbour in `direction`
    /// becoming `neighbour`, or `None` when the rule read an unloaded block
    /// and cannot be trusted.
    pub(in crate::world::region) fn shape_for(
        &mut self,
        pos: BlockPos,
        old: BlockState,
        direction: Facing,
        neighbour: BlockState,
    ) -> Option<BlockState> {
        if (old.block().component().is_some() || self.block_behavior(old.block()).is_active())
            && self.redstone_ready(pos)
        {
            if let Some(shaped) = self
                .block_behavior(old.block())
                .update_shape(self, pos, old, direction, neighbour)
            {
                return Some(shaped);
            }
            let old = RegionActor::block(self, pos)?;
            return self.placement_shape(pos, old, direction, neighbour);
        }
        self.placement_shape(pos, old, direction, neighbour)
    }

    /// The placement rules' reaction of `old` at `pos` to its neighbour in
    /// `direction` becoming `neighbour`, scheduling the block tick the rules
    /// ask for.
    pub(in crate::world::region) fn placement_shape(
        &mut self,
        pos: BlockPos,
        old: BlockState,
        direction: Facing,
        neighbour: BlockState,
    ) -> Option<BlockState> {
        let (new, tick) = self.placement_reaction(pos, old, direction, neighbour)?;
        if let Some(delay) = tick {
            RedstoneLevel::schedule_tick(self, pos, old.block(), delay, TickPriority::Normal);
        }
        Some(new)
    }

    pub(in crate::world::region) fn placement_reaction(
        &self,
        pos: BlockPos,
        old: BlockState,
        direction: Facing,
        neighbour: BlockState,
    ) -> Option<(BlockState, Option<u32>)> {
        let rules = self.shared.block_rules.clone();
        if rules.holds_shape_while_ticking(old, direction)
            && self.has_scheduled_tick(pos, old.block())
        {
            return Some((old, None));
        }
        let source = step(pos, direction);
        let unknown = std::cell::Cell::new(false);
        let level = |p: BlockPos| {
            if p == source {
                neighbour
            } else {
                RegionActor::block(self, p).unwrap_or_else(|| {
                    unknown.set(true);
                    AIR
                })
            }
        };
        let min_y = self.dimension().shape.min_y;
        let new = self
            .shared
            .region_hooks
            .iter()
            .find_map(|hook| hook.update_shape(old, pos, direction, neighbour, &level, min_y))
            .unwrap_or_else(|| rules.update_shape(old, pos, direction, neighbour, &level));
        let tick = rules.shape_tick(old, pos, direction, &level);
        (!unknown.get()).then_some((new, tick))
    }

    /// Runs the block tick of a block the placement rules schedule ticks
    /// for; false when the block is not one of them.
    pub(in crate::world::region) fn placement_tick(
        &mut self,
        pos: BlockPos,
        state: BlockState,
    ) -> bool {
        let rules = self.shared.block_rules.clone();
        rules.scheduled_tick(self, pos, state)
    }
}
