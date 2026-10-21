use super::*;
use tuff_vanilla_data::redstone::AnalogSource;

impl Reader for RegionActor {
    fn block(&self, pos: BlockPos) -> BlockState {
        if let Some(state) = self.sim.redstone.sent_changes.get(&pos) {
            return *state;
        }
        RegionActor::block(self, pos).unwrap_or(AIR)
    }

    fn comparator_output(&self, pos: BlockPos) -> u8 {
        self.raw_block_entity(pos)
            .and_then(|nbt| nbt.get_int("OutputSignal"))
            .map_or(0, |value| value.clamp(0, 15) as u8)
    }

    fn analog_output(&self, pos: BlockPos, state: BlockState, direction: Facing) -> u8 {
        if state.block() == Block::Crafter {
            return self.crafter_signal(pos);
        }
        if let Some(signal) = self
            .ents
            .feature_state
            .peek::<analog::RuntimeSignals>()
            .and_then(|signals| signals.0.get(&pos))
        {
            return *signal;
        }
        if let Some(signal) = self
            .block_behavior(state.block())
            .analog_output(self, pos, state, direction)
        {
            return signal;
        }
        match state.block().analog_source() {
            Some(AnalogSource::Container) => self
                .shared
                .redstone_behaviors
                .fill_signal(&self.container_slots_at(pos, state)),
            Some(AnalogSource::State) => state.state_analog_output(),
            Some(AnalogSource::Unsupported) => self.raw_block_entity(pos).map_or_else(
                || analog::state_signal(state),
                |nbt| analog::block_entity_signal(state, nbt, direction),
            ),
            None => 0,
        }
    }

    fn container_slots(&self, pos: BlockPos, state: BlockState) -> Vec<&Option<ItemStack>> {
        self.container_slots_at(pos, state)
    }

    fn minecarts_in(&self, area: [f64; 6]) -> Vec<redstone::CartReading<'_>> {
        self.minecart_readings(area)
    }

    fn item_frame_output(&self, pos: BlockPos, direction: Facing) -> Option<u8> {
        let cell = [
            f64::from(pos.x),
            f64::from(pos.y),
            f64::from(pos.z),
            f64::from(pos.x + 1),
            f64::from(pos.y + 1),
            f64::from(pos.z + 1),
        ];
        let mut frames = self.ents.entities.values().filter_map(|tracked| {
            let (facing, signal) = tracked.entity.comparator_frame()?;
            (facing == direction && intersects(tracked.entity.bounds(), cell)).then_some(signal)
        });
        let signal = frames.next()?;
        frames.next().is_none().then_some(signal)
    }

    fn open_count(&self, pos: BlockPos) -> usize {
        self.menus.viewers.get(&pos).map_or(0, Vec::len)
    }

    fn saved_nbt_at(&self, pos: BlockPos) -> Option<&tuff_nbt::Compound> {
        self.raw_block_entity(pos)
    }
}

impl RedstoneLevel for RegionActor {
    fn set_block(&mut self, pos: BlockPos, state: BlockState, flags: u32) -> bool {
        self.set_block_flags(pos, state, flags, UPDATE_LIMIT)
            .is_some_and(|old| old != state)
    }

    fn neighbor_changed(&mut self, pos: BlockPos, source: Block) {
        self.add_and_run(Update::Neighbor {
            pos,
            source,
            orientation: None,
        });
    }

    fn neighbor_changed_oriented(
        &mut self,
        pos: BlockPos,
        source: Block,
        orientation: Option<Orientation>,
    ) {
        self.add_and_run(Update::Neighbor {
            pos,
            source,
            orientation,
        });
    }

    fn update_neighbors_oriented(
        &mut self,
        pos: BlockPos,
        source: Block,
        skip: Option<Facing>,
        orientation: Option<Orientation>,
    ) {
        let orientation = match orientation {
            Some(orientation) => Some(orientation),
            None => self.initial_orientation(skip.map(Facing::opposite), None),
        };
        self.add_and_run(Update::multi(pos, source, skip, orientation));
    }

    fn redstone_experiments(&self) -> bool {
        self.shared
            .datapacks
            .has_feature(crate::world::redstone::REDSTONE_EXPERIMENTS)
    }

    fn random_orientation(&mut self) -> Orientation {
        Orientation::from_roll(self.rng.gen_range(0..Orientation::COUNT as u32))
    }

    fn incoming_orientation(&self) -> Option<Orientation> {
        self.sim.redstone.incoming
    }

    fn wire_orientation_debug(&mut self, pos: BlockPos, orientation: Orientation) {
        self.debug_wire_orientation(pos, orientation);
    }

    fn sky_brightness(&self, pos: BlockPos) -> Option<u8> {
        use tuff_world_model::light::LightLayer;
        let dimension = self.dimension();
        if !dimension.has_sky_light() {
            return None;
        }
        let column = self.columns.get(&ChunkPos::of_block(pos))?;
        let light = column.chunk.light().filter(|_| column.lit)?;
        let sky = light.value(dimension.shape, LightLayer::Sky, pos.x, pos.y, pos.z)?;
        Some(sky.saturating_sub(crate::world::sky_darken(&self.shared, dimension)))
    }

    fn sun_angle(&self) -> f32 {
        let clocks = self.shared.clocks.borrow();
        self.dimension().environment().sun_angle(|clock| {
            crate::world::clock::clock_id(clock).map_or(0, |id| clocks.get(id).total_ticks)
        })
    }

    fn update_neighbors_except(&mut self, pos: BlockPos, source: Block, skip: Option<Facing>) {
        self.update_neighbors_oriented(pos, source, skip, None);
    }

    fn shape_update(
        &mut self,
        direction: Facing,
        pos: BlockPos,
        neighbour_pos: BlockPos,
        neighbour: BlockState,
        flags: u32,
        limit: u32,
    ) {
        self.add_and_run(Update::Shape {
            direction,
            neighbour,
            pos,
            neighbour_pos,
            flags,
            limit,
        });
    }

    fn schedule_tick(&mut self, pos: BlockPos, block: Block, delay: u32, priority: TickPriority) {
        self.schedule_tick_with(pos, block, delay, priority as i32);
    }

    fn has_scheduled_tick(&self, pos: BlockPos, block: Block) -> bool {
        self.columns
            .get(&ChunkPos::of_block(pos))
            .is_some_and(|column| column.block_ticks.is_pending(pos, block))
    }

    fn will_tick_this_tick(&self, pos: BlockPos, block: Block) -> bool {
        self.sim.redstone.ticks_this_tick.contains(&(pos, block))
    }

    fn set_comparator_output(&mut self, pos: BlockPos, value: u8) {
        let Some(column) = self.columns.get_mut(&ChunkPos::of_block(pos)) else {
            return;
        };
        let Some(nbt) = column.meta.raw_block_entities.get_mut(&pos) else {
            return;
        };
        if nbt.get_int("OutputSignal") != Some(i32::from(value)) {
            nbt.insert("OutputSignal", i32::from(value));
            column.blocks_dirty = true;
        }
    }

    fn game_time(&self) -> u64 {
        self.now()
    }

    fn random_float(&mut self) -> f32 {
        self.rng.r#gen()
    }

    fn torch_toggles(&mut self) -> &mut Vec<(BlockPos, u64)> {
        &mut self.sim.redstone.torch_toggles
    }

    fn entities_touching(&self, bounds: [f64; 6], living: bool) -> usize {
        let foreign = self
            .sim
            .redstone
            .foreign
            .values()
            .flatten()
            .filter(|(b, alive)| (!living || *alive) && intersects(*b, bounds))
            .count();
        let entities = self
            .ents
            .entities
            .values()
            .filter(|tracked| !living || tracked.entity.living().is_some())
            .filter(|tracked| intersects(tracked.entity.bounds(), bounds))
            .count();
        let players = self
            .ents
            .presence
            .values()
            .filter(|presence| presence.is_physical())
            .filter(|presence| intersects(player_bounds(presence.position), bounds))
            .count();
        entities + players + foreign
    }

    fn arrow_touching(&self, bounds: [f64; 6]) -> bool {
        self.ents.entities.values().any(|tracked| {
            tracked.entity.presses_buttons() && intersects(tracked.entity.bounds(), bounds)
        })
    }

    fn minecarts_touching(&self, bounds: [f64; 6]) -> usize {
        self.ents
            .entities
            .values()
            .filter(|tracked| {
                tracked.entity.rides_rails() && intersects(tracked.entity.bounds(), bounds)
            })
            .count()
            + usize::from(
                self.sim
                    .redstone
                    .pass_boxes
                    .iter()
                    .any(|b| intersects(*b, bounds)),
            )
    }

    fn owned_block(&self, pos: BlockPos) -> Option<BlockState> {
        if self.owns(pos) {
            RegionActor::block(self, pos)
        } else {
            None
        }
    }

    fn reachable_block(&self, pos: BlockPos) -> Option<BlockState> {
        if self.pos.contains(ChunkPos::of_block(pos)) {
            RedstoneLevel::owned_block(self, pos)
        } else {
            RegionActor::block(self, pos)
                .map(|state| *self.sim.redstone.sent_changes.get(&pos).unwrap_or(&state))
        }
    }

    fn piston_set(
        &mut self,
        pos: BlockPos,
        state: BlockState,
        flags: u32,
        drops: bool,
        moving: Option<MovingBlock>,
        carry: Option<BlockPos>,
    ) {
        self.show_carried(pos, moving);
        if self.pos.contains(ChunkPos::of_block(pos)) {
            self.carry_out(pos, carry, Reader::block(self, pos), true);
            self.apply_piston_set(pos, state, flags, drops, moving);
        } else {
            let expected = Reader::block(self, pos);
            self.sim.redstone.sent_changes.insert(pos, state);
            match moving {
                Some(moving) => {
                    self.sim
                        .redstone
                        .remote_moving
                        .insert(pos, (moving, self.now()));
                }
                None => {
                    self.sim.redstone.remote_moving.remove(&pos);
                }
            }
            self.queue_remote(
                pos,
                BlockUpdate::Piston {
                    pos,
                    state,
                    flags,
                    drops,
                    moving,
                    tick: self.now(),
                    expected,
                    carry,
                },
            );
        }
    }

    fn within_border(&self, pos: BlockPos) -> bool {
        self.dimension().block_within_border(pos)
    }

    fn build_limits(&self) -> (i32, i32) {
        let shape = self.dimension().shape;
        (shape.min_y, shape.min_y + shape.height - 1)
    }

    fn block_event(&mut self, pos: BlockPos, block: Block, kind: u8, data: u8) {
        self.queue_block_event(pos, block, kind, data);
    }

    fn is_handling_tick(&self) -> bool {
        self.sim.redstone.handling_tick
    }

    fn moving_block(&self, pos: BlockPos) -> Option<MovingBlock> {
        if self.pos.contains(ChunkPos::of_block(pos)) {
            self.moving_at(pos)
        } else {
            self.remote_moving_at(pos)
        }
    }

    fn start_moving(&mut self, pos: BlockPos, moving: MovingBlock) {
        self.begin_move(pos, moving);
    }

    fn carries_block_entity(&self, state: BlockState, _pos: BlockPos, _direction: Facing) -> bool {
        self.may_carry(state)
    }

    fn finish_moving(&mut self, pos: BlockPos) {
        if self.pos.contains(ChunkPos::of_block(pos)) {
            self.finish_move(pos);
        } else {
            self.finish_remote_move(pos);
        }
    }

    fn destroy_block(&mut self, pos: BlockPos, drops: bool) {
        let Some(state) = RegionActor::block(self, pos).filter(|state| !is_air(*state)) else {
            return;
        };
        if self.owns(pos) {
            self.destroy_by_update(pos, state, drops, UPDATE_LIMIT);
        } else {
            self.break_remote(pos, state, None, drops);
        }
    }

    fn effect(&mut self, effect: Effect) {
        match effect {
            Effect::Sound {
                sound,
                pos,
                volume,
                pitch,
            } => self.play_sound(sound, pos, volume, pitch, None),
            Effect::LevelEvent { event, pos, data } => self.level_event(event, pos, data, false),
            Effect::CommandBlockPowered(pos, on) => self.command_block_powered(pos, on),
            Effect::CommandBlockTick(pos) => self.command_block_tick(pos),
            Effect::CommandBlockPlaced(pos) => self.command_block_placed(pos),
            Effect::TestBlockPower(pos, on) => self.test_block_power(pos, on),
            Effect::TestBlockReset(pos) => self.test_block_reset(pos),
            Effect::StructureBlockPower(pos, on) => self.structure_block_power(pos, on),
            Effect::CrafterTriggered(pos, on) => self.crafter_triggered(pos, on),
            Effect::CrafterDispense(pos) => {
                if let Some(dispensers) = self.shared.dispensers.clone() {
                    dispensers.fire(self, pos);
                }
            }
            Effect::WaterTick(pos) => {
                let fluid = tuff_vanilla_data::fluid::Fluid::Water;
                if let Some(rules) = fluid.rules() {
                    let delay = rules.tick_delay(self.sim.fluid_settings.fast_lava);
                    self.schedule_fluid_tick(pos, fluid, delay);
                }
            }
            Effect::PlayNote(pos) => self.play_note(pos),
            Effect::RingBell(pos) => self.ring_bell(pos, None),
            Effect::PrimeTnt(pos) => {
                if self.shared.rule_bool("tnt_explodes") {
                    self.spawn_primed_tnt(pos, None, None);
                    let left = placement::remains(Reader::block(self, pos));
                    RedstoneLevel::set_block(self, pos, left, redstone::UPDATE_ALL);
                }
            }
            Effect::Dispense(pos) => {
                if let Some(dispensers) = self.shared.dispensers.clone() {
                    dispensers.fire(self, pos);
                }
            }
            Effect::GameEvent(event, pos) => crate::world::region_api::RegionWorld::game_event(
                self,
                event,
                pos.center(),
                crate::world::game_event::Context::default(),
            ),
            Effect::DropResources(pos, state) => {
                if self.shared.rule_bool("block_drops") {
                    let saved = self.saved_block_entity(pos);
                    let stacks =
                        self.shared
                            .block_loot()
                            .destroyed(state, saved.as_ref(), &mut self.rng);
                    self.drop_items(pos, stacks);
                }
            }
        }
    }
}
