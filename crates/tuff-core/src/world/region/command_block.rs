//! Command blocks: powering, the impulse/repeating/chain modes, conditions,
//! success counts, the editor screen and its packet.

use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::registry::BlockEntityType;
use tuff_vanilla_data::{Block, BlockState};

use super::RegionActor;
use crate::protocol::play;
use crate::world::commands::block::{self, BlockCommand};
use crate::world::messages::{
    CommandBlockEdit, CommandMode, CommandRemote, Place, PlayerEvent, PlayerHandle, RegionInternal,
    Resume,
};
use crate::world::placement::step;
use crate::world::redstone::{TickPriority, UPDATE_CLIENTS, UPDATE_LIMIT};
use crate::world::{framed, packets};
use tuff_item::text::TextComponent;
use tuff_nbt::Compound;
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

enum Condition {
    Met(bool),
    /// Conditional on a block a neighbour region owns.
    Remote(BlockPos),
}

enum Performed {
    Blocked,
    Done,
    Started,
}

fn flag(nbt: &Compound, key: &str) -> bool {
    nbt.get_bool(key).unwrap_or(false)
}

impl RegionActor {
    fn command_block(&self, pos: BlockPos) -> Option<(BlockState, CommandMode, &Compound)> {
        let state = self.block(pos)?;
        let mode = CommandMode::of_block(state.block())?;
        Some((state, mode, self.raw_block_entity(pos)?))
    }

    fn set_command_flag(&mut self, pos: BlockPos, key: &str, on: bool) {
        if let Some(nbt) = self.raw_block_entity_mut(pos) {
            nbt.insert(key, on);
        }
    }

    pub(super) fn command_block_powered(&mut self, pos: BlockPos, signal: bool) {
        let Some((state, mode, nbt)) = self.command_block(pos) else {
            return;
        };
        if flag(nbt, "powered") == signal {
            return;
        }
        let auto = flag(nbt, "auto");
        self.set_command_flag(pos, "powered", signal);
        if !signal || auto || mode == CommandMode::Sequence {
            return;
        }
        if self.mark_condition_met(pos, Resume::Schedule).is_some() {
            self.schedule_tick_with(pos, state.block(), 1, TickPriority::Normal as i32);
        }
    }

    /// Whether the block at `pos` may run: unconditional, or the block
    /// behind it succeeded. `known` is a success count already in hand for
    /// a block (the one before it in a chain).
    fn condition(&self, pos: BlockPos, known: Option<(BlockPos, i32)>) -> Condition {
        let Some((state, _, _)) = self.command_block(pos) else {
            return Condition::Met(false);
        };
        if state.get("conditional") != Some("true") {
            return Condition::Met(true);
        }
        let back = step(pos, state.facing().unwrap_or(Facing::North).opposite());
        if let Some((at, count)) = known
            && at == back
        {
            return Condition::Met(count > 0);
        }
        if !self.pos.contains(ChunkPos::of_block(back)) {
            return Condition::Remote(back);
        }
        Condition::Met(
            self.block(back)
                .is_some_and(|s| CommandMode::of_block(s.block()).is_some())
                && self
                    .raw_block_entity(back)
                    .and_then(|nbt| nbt.get_int("SuccessCount"))
                    .is_some_and(|count| count > 0),
        )
    }

    /// Records whether the block at `pos` may run; `None` when that waits
    /// on a neighbour's answer, which then goes on as `resume` says.
    fn mark_condition_met(&mut self, pos: BlockPos, resume: Resume) -> Option<bool> {
        let met = match self.condition(pos, None) {
            Condition::Met(met) => met,
            Condition::Remote(back) => {
                self.set_command_flag(pos, "conditionMet", false);
                if self.ask_success(pos, back, resume) {
                    return None;
                }
                false
            }
        };
        self.set_command_flag(pos, "conditionMet", met);
        Some(met)
    }

    fn ask_success(&mut self, pos: BlockPos, back: BlockPos, resume: Resume) -> bool {
        let Some(owner) = self
            .border
            .neighbors
            .get(&ChunkPos::of_block(back).region())
        else {
            return false;
        };
        owner.internal(RegionInternal::CommandBlock(CommandRemote::AskSuccess {
            pos,
            back,
            reply: self.pos,
            resume,
        }));
        true
    }

    pub(super) fn command_block_remote(&mut self, remote: CommandRemote) {
        match remote {
            CommandRemote::Chain {
                from,
                direction,
                budget,
                back,
            } => self.command_chain_from(from, direction, budget, back.map(|count| (from, count))),
            CommandRemote::AskSuccess {
                pos,
                back,
                reply,
                resume,
            } => {
                let count = self
                    .command_block(back)
                    .map(|(_, _, nbt)| nbt.get_int("SuccessCount").unwrap_or(0));
                if let Some(handle) = self.border.neighbors.get(&reply) {
                    handle.internal(RegionInternal::CommandBlock(CommandRemote::Success {
                        pos,
                        back,
                        count,
                        resume,
                    }));
                }
            }
            CommandRemote::Success {
                pos,
                back,
                count,
                resume,
            } => {
                let Some((state, mode, nbt)) = self.command_block(pos) else {
                    return;
                };
                let active = flag(nbt, "powered") || flag(nbt, "auto");
                let met = match self.condition(pos, count.map(|count| (back, count))) {
                    Condition::Met(met) => met,
                    Condition::Remote(_) => false,
                };
                self.set_command_flag(pos, "conditionMet", met);
                match resume {
                    Resume::Record => {}
                    Resume::Schedule => {
                        self.schedule_tick_with(pos, state.block(), 1, TickPriority::Normal as i32)
                    }
                    Resume::Chain { budget } => {
                        if mode == CommandMode::Sequence
                            && active
                            && self.chain_block_step(pos, budget, met)
                        {
                            let direction = state.facing().unwrap_or(Facing::North);
                            let known = self.success_of(pos);
                            self.command_chain_from(pos, direction, budget, known);
                        }
                    }
                }
            }
        }
    }

    fn success_of(&self, pos: BlockPos) -> Option<(BlockPos, i32)> {
        let count = self.raw_block_entity(pos)?.get_int("SuccessCount")?;
        Some((pos, count))
    }

    pub(super) fn command_block_tick(&mut self, pos: BlockPos) {
        let Some((state, mode, nbt)) = self.command_block(pos) else {
            return;
        };
        let command_set = nbt.get_str("Command").is_some_and(|c| !c.is_empty());
        let met = flag(nbt, "conditionMet");
        let conditional = state.get("conditional") == Some("true");
        let schedule = flag(nbt, "powered") || flag(nbt, "auto");
        match mode {
            CommandMode::Auto => {
                self.mark_condition_met(pos, Resume::Record);
                if met {
                    self.execute_command_block(pos, command_set);
                } else if conditional {
                    self.set_success_count(pos, 0);
                }
                if schedule {
                    self.schedule_tick_with(pos, state.block(), 1, TickPriority::Normal as i32);
                }
            }
            CommandMode::Redstone => {
                if met {
                    self.execute_command_block(pos, command_set);
                } else if conditional {
                    self.set_success_count(pos, 0);
                }
            }
            CommandMode::Sequence => {}
        }
    }

    fn set_success_count(&mut self, pos: BlockPos, count: i32) {
        if let Some(nbt) = self.raw_block_entity_mut(pos) {
            nbt.insert("SuccessCount", count);
        }
    }

    fn sequence_budget(&self) -> u32 {
        self.shared.rule_int("max_command_sequence_length").max(0) as u32
    }

    fn execute_command_block(&mut self, pos: BlockPos, command_set: bool) {
        if self.sim.redstone.command_blocks_pending.contains_key(&pos) {
            return;
        }
        let budget = self.sequence_budget();
        if command_set {
            if let Performed::Started = self.perform_command(pos, budget) {
                return;
            }
        } else {
            self.set_success_count(pos, 0);
        }
        self.command_chain(pos, budget);
    }

    fn perform_command(&mut self, pos: BlockPos, budget: u32) -> Performed {
        let Some((state, _, nbt)) = self.command_block(pos) else {
            return Performed::Blocked;
        };
        let last = nbt.get_long("LastExecution").unwrap_or(-1);
        if last == self.sched.tick as i64 {
            return Performed::Blocked;
        }
        let command = nbt.get_str("Command").unwrap_or("").to_owned();
        let track = nbt.get_bool("TrackOutput").unwrap_or(true);
        let update_last = nbt.get_bool("UpdateLastExecution").unwrap_or(true);
        let name = nbt
            .get("CustomName")
            .and_then(TextComponent::from_nbt)
            .unwrap_or_else(|| TextComponent::new("@"));
        let yaw = match state.facing() {
            Some(Facing::West) => 90.0,
            Some(Facing::North) => 180.0,
            Some(Facing::East) => 270.0,
            _ => 0.0,
        };
        if command.eq_ignore_ascii_case("Searge") {
            if let Some(nbt) = self.raw_block_entity_mut(pos) {
                nbt.insert("LastOutput", TextComponent::new("#itzlipofutzli").to_nbt());
                nbt.insert("SuccessCount", 1i32);
            }
            return Performed::Done;
        }
        self.set_success_count(pos, 0);
        let enabled = self.shared.rule_bool("command_blocks_work") && !command.is_empty();
        let now = self.sched.tick as i64;
        if let Some(nbt) = self.raw_block_entity_mut(pos) {
            if update_last {
                nbt.insert("LastExecution", now);
            } else {
                nbt.remove("LastExecution");
            }
            if track && enabled {
                nbt.remove("LastOutput");
            }
        }
        if !enabled {
            return Performed::Done;
        }
        self.sim.redstone.command_blocks_pending.insert(pos, budget);
        let request = BlockCommand {
            dimension: self.dimension,
            position: pos.center(),
            yaw,
            name,
            command,
            track_output: track,
        };
        let shared = self.shared.clone();
        let handle = self.handle.clone();
        tokio::spawn(async move {
            let outcome = block::run(shared, request).await;
            handle
                .request(crate::world::messages::RegionRequest::CommandBlockDone {
                    pos,
                    success: outcome.success,
                    output: outcome.output,
                })
                .await;
        });
        Performed::Started
    }

    pub(super) fn command_block_done(
        &mut self,
        pos: BlockPos,
        success: u32,
        output: Option<TextComponent>,
    ) {
        let Some(budget) = self.sim.redstone.command_blocks_pending.remove(&pos) else {
            return;
        };
        if self.command_block(pos).is_none() {
            return;
        }
        if let Some(nbt) = self.raw_block_entity_mut(pos) {
            nbt.insert("SuccessCount", success as i32);
            if nbt.get_bool("TrackOutput").unwrap_or(true)
                && let Some(output) = output
            {
                nbt.insert("LastOutput", output.to_nbt());
            }
        }
        self.command_chain(pos, budget);
    }

    fn command_chain(&mut self, start: BlockPos, budget: u32) {
        let Some((state, _, _)) = self.command_block(start) else {
            return;
        };
        let direction = state.facing().unwrap_or(Facing::North);
        let known = self.success_of(start);
        self.command_chain_from(start, direction, budget, known);
    }

    /// Runs the chain on from the block at `pos` towards `direction`,
    /// handing it to the neighbour that owns the next block.
    fn command_chain_from(
        &mut self,
        mut pos: BlockPos,
        mut direction: Facing,
        mut budget: u32,
        mut known: Option<(BlockPos, i32)>,
    ) {
        while budget > 0 {
            let next = step(pos, direction);
            if !self.pos.contains(ChunkPos::of_block(next)) {
                if let Some(owner) = self
                    .border
                    .neighbors
                    .get(&ChunkPos::of_block(next).region())
                {
                    owner.internal(RegionInternal::CommandBlock(CommandRemote::Chain {
                        from: pos,
                        direction,
                        budget,
                        back: known.filter(|(at, _)| *at == pos).map(|(_, count)| count),
                    }));
                }
                return;
            }
            budget -= 1;
            pos = next;
            let Some(state) = self.block(pos) else {
                return;
            };
            if state.block() != Block::ChainCommandBlock || !self.owns(pos) {
                return;
            }
            let Some((_, mode, nbt)) = self.command_block(pos) else {
                return;
            };
            if mode != CommandMode::Sequence {
                return;
            }
            if flag(nbt, "powered") || flag(nbt, "auto") {
                let met = match self.condition(pos, known) {
                    Condition::Met(met) => met,
                    Condition::Remote(back) => {
                        self.set_command_flag(pos, "conditionMet", false);
                        if self.ask_success(pos, back, Resume::Chain { budget }) {
                            return;
                        }
                        false
                    }
                };
                self.set_command_flag(pos, "conditionMet", met);
                if !self.chain_block_step(pos, budget, met) {
                    return;
                }
            }
            direction = state.facing().unwrap_or(Facing::North);
            known = self.success_of(pos);
        }
    }

    /// Runs an active chain block whose condition is `met`; false when the
    /// chain stops here for now.
    fn chain_block_step(&mut self, pos: BlockPos, budget: u32, met: bool) -> bool {
        if !met {
            if self
                .block(pos)
                .is_some_and(|state| state.get("conditional") == Some("true"))
            {
                self.set_success_count(pos, 0);
            }
            return true;
        }
        if self.sim.redstone.command_blocks_pending.contains_key(&pos) {
            return false;
        }
        matches!(self.perform_command(pos, budget), Performed::Done)
    }

    pub(super) fn command_block_placed(&mut self, pos: BlockPos) {
        let rule = self.shared.rule_bool("send_command_feedback");
        let fresh = self
            .command_block(pos)
            .is_some_and(|(_, _, nbt)| nbt.get_str("Command").is_none_or(str::is_empty));
        if fresh && let Some(nbt) = self.raw_block_entity_mut(pos) {
            nbt.insert("TrackOutput", rule);
        }
    }

    /// Opens the editor of the command block at `place.target` for a
    /// creative operator, by sending the block entity's saved data.
    pub(super) fn open_command_block(&mut self, place: &Place) -> bool {
        let pos = place.target;
        if !place.game_master || self.command_block(pos).is_none() {
            return false;
        }
        let Some(nbt) = self.raw_block_entity(pos) else {
            return false;
        };
        let mut data = Compound::new();
        for (key, value) in nbt.iter() {
            if !["id", "x", "y", "z", "keepPacked"].contains(&key) {
                data.insert(key, value.clone());
            }
        }
        place.player.send(PlayerEvent::ChunkPackets {
            dimension: self.dimension,
            pos: tuff_world_model::pos::ChunkPos::of_block(pos),
            packets: packets([framed(&play::BlockEntityData {
                position: pos,
                kind: BlockEntityType::CommandBlock.id(),
                nbt: &data,
            })]),
        });
        true
    }

    pub(super) fn edit_command_block(
        &mut self,
        player: &PlayerHandle,
        pos: BlockPos,
        edit: CommandBlockEdit,
    ) {
        let Some((state, old_mode, nbt)) = self.command_block(pos) else {
            return;
        };
        if !self.owns(pos) {
            return;
        }
        let was_auto = flag(nbt, "auto");
        let powered = flag(nbt, "powered");
        let facing = state.facing().unwrap_or(Facing::North);
        let next = edit
            .mode
            .block()
            .default_state()
            .with_facing(facing)
            .and_then(|s| {
                s.with(
                    "conditional",
                    if edit.conditional { "true" } else { "false" },
                )
            });
        if let Some(next) = next
            && next != state
        {
            self.set_block_flags(pos, next, UPDATE_CLIENTS, UPDATE_LIMIT);
        }
        if let Some(nbt) = self.raw_block_entity_mut(pos) {
            nbt.insert("Command", edit.command.as_str());
            nbt.insert("SuccessCount", 0i32);
            nbt.insert("TrackOutput", edit.track_output);
            if !edit.track_output {
                nbt.remove("LastOutput");
            }
            nbt.insert("auto", edit.automatic);
        }
        let block = edit.mode.block();
        let start = !was_auto && edit.automatic && !powered && edit.mode != CommandMode::Sequence;
        let repeat =
            old_mode != edit.mode && edit.mode == CommandMode::Auto && (powered || edit.automatic);
        for _ in 0..(start as u8 + repeat as u8) {
            if self.mark_condition_met(pos, Resume::Schedule).is_some() {
                self.schedule_tick_with(pos, block, 1, TickPriority::Normal as i32);
            }
        }
        if !edit.command.is_empty() {
            player.send(PlayerEvent::Message(TextComponent::translate(
                "advMode.setCommand.success",
                [TextComponent::new(edit.command)],
            )));
        }
    }
}
