use super::*;

/// A container's item slots at a position, read-only (see
/// [`TickContext::container`]).
pub(super) type ContainerView<'a> = tuff_item::item::SlotsCow<'a>;

/// What examining a target chest resolves to: sampled on arrival for the
/// sound and animation, and again when the dwell ends for the transfer.
#[derive(Debug)]
pub(super) enum Outcome {
    Take {
        stack: tuff_item::item::ItemSnapshot,
    },
    TakeFail,
    Deposit {
        stack: tuff_item::item::ItemSnapshot,
    },
    DepositFail,
}

/// Horizontal/vertical radii of the box (65x17x65 total) a copper golem
/// searches for its next chest in.
pub(super) const SEARCH_HORIZONTAL: i32 = 32;
pub(super) const SEARCH_VERTICAL: i32 = 8;
/// How many distinct chests it remembers trying before giving up and
/// wandering.
pub(super) const MAX_VISITED: usize = 10;
/// Ticks spent examining a chest before acting (3 seconds).
pub(super) const DWELL_TICKS: i32 = 60;
/// Ticks spent wandering after giving up (7 seconds) before searching
/// again.
pub(super) const GIVEUP_TICKS: u64 = 140;
/// The most items taken from a copper chest in one visit.
pub(super) const TAKE_MAX: u8 = 16;
/// Squared distance within which the golem can reach into a chest.
pub(super) const REACH_SQ: f64 = 2.25;
pub(super) const TRANSPORT_SPEED: f64 = 1.0;
/// How many chests it remembers failing to reach.
pub(super) const MAX_UNREACHABLE: usize = 50;
/// How long visited and unreachable chests stay remembered.
pub(super) const MEMORY_TICKS: u64 = 6000;
/// How near a chest another mob has open makes it wait its turn.
pub(super) const QUEUE_DISTANCE: f64 = 3.0;
/// How far from the chest it may drift before giving up its turn.
pub(super) const CONTINUE_DISTANCE: f64 = 2.0;
pub(super) const LID_OPEN_TICK: i32 = 1;
pub(super) const SOUND_TICK: i32 = 9;

pub(super) fn block_center(pos: BlockPos) -> [f64; 3] {
    [pos.x as f64 + 0.5, pos.y as f64 + 0.5, pos.z as f64 + 0.5]
}

/// A copper golem's idle behaviour: shuttle items from the nearest
/// unvisited copper chest to the nearest unvisited wooden or trapped
/// chest that will take them. The container is sampled on arrival for
/// the animation and again when the dwell ends for what really moves.
#[derive(Debug, Default)]
pub(super) struct TransportItems {
    pub(super) target: Option<BlockPos>,
    pub(super) stage: Stage,
    /// Ticks spent at the chest since arriving.
    pub(super) elapsed: i32,
    pub(super) outcome: Option<Outcome>,
    pub(super) visited: Vec<BlockPos>,
    pub(super) unreachable: Vec<BlockPos>,
    pub(super) unreachable_until: u64,
    pub(super) cooldown_until: u64,
    /// The chest whose lid this golem holds open.
    pub(super) open: Option<BlockPos>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) enum Stage {
    #[default]
    Travelling,
    Queuing,
    Interacting,
}

impl TransportItems {
    pub(super) fn picking_up(mob: &MobCore) -> bool {
        mob.equipment.get(EquipmentSlot::Mainhand).is_none()
    }

    /// The nearest chest of the wanted kind within the search box that
    /// isn't already in `visited`, by block type only (contents are read
    /// once the golem arrives, see [`Outcome`]).
    pub(super) fn find_target(&self, mob: &MobCore, cx: &mut TickContext) -> Option<BlockPos> {
        let picking_up = Self::picking_up(mob);
        let origin = super::super::physics::block_pos(mob.base.position);
        let mut best: Option<(f64, BlockPos)> = None;
        for dx in -SEARCH_HORIZONTAL..=SEARCH_HORIZONTAL {
            for dz in -SEARCH_HORIZONTAL..=SEARCH_HORIZONTAL {
                for dy in -SEARCH_VERTICAL..=SEARCH_VERTICAL {
                    let pos = BlockPos::new(origin.x + dx, origin.y + dy, origin.z + dz);
                    if self.visited.contains(&pos) || self.unreachable.contains(&pos) {
                        continue;
                    }
                    let Some(state) = (cx.block)(pos) else {
                        continue;
                    };
                    if !Self::wanted(picking_up, state) {
                        continue;
                    }
                    if (cx.container_info)(pos).locked || Self::blocked(pos, cx) {
                        continue;
                    }
                    let center = block_center(pos);
                    let d = (0..3)
                        .map(|i| {
                            let diff = center[i] - mob.base.position[i];
                            diff * diff
                        })
                        .sum::<f64>();
                    if best.is_none_or(|(best_d, _)| d < best_d) {
                        best = Some((d, pos));
                    }
                }
            }
        }
        best.map(|(_, pos)| pos)
    }

    pub(super) fn wanted(picking_up: bool, state: BlockState) -> bool {
        if picking_up {
            tuff_datapack::tags::is(tags::key::COPPER_CHESTS, state.block())
        } else {
            matches!(state.block(), Block::Chest | Block::TrappedChest)
        }
    }

    /// A solid block on top of the chest keeps its lid shut.
    pub(super) fn blocked(pos: BlockPos, cx: &TickContext) -> bool {
        let above = BlockPos::new(pos.x, pos.y + 1, pos.z);
        (cx.block)(above).is_some_and(|state| state.is_redstone_conductor())
    }

    pub(super) fn give_up(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.drop_target(mob, cx);
        self.visited.clear();
        self.unreachable.clear();
        self.cooldown_until = cx.tick + GIVEUP_TICKS;
    }

    /// Lets go of the chest: the lid closes and the golem is idle again.
    pub(super) fn drop_target(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.close_lid(cx);
        mob.signal = STATE_IDLE;
        self.target = None;
        self.outcome = None;
        self.stage = Stage::Travelling;
        self.elapsed = 0;
    }

    pub(super) fn close_lid(&mut self, cx: &mut TickContext) {
        if let Some(pos) = self.open.take() {
            cx.effects.push(Effect::ContainerOpen { pos, open: false });
        }
    }

    /// Whether the golem, standing at `from` (the middle of its box), is
    /// within `distance` of the chest's shape.
    pub(super) fn within(mob: &MobCore, target: BlockPos, distance: f64, from: [f64; 3]) -> bool {
        let half = f64::from(mob.size.width) / 2.0;
        let height = f64::from(mob.size.height) / 2.0;
        let body = [
            from[0] - half,
            from[1] - height,
            from[2] - half,
            from[0] + half,
            from[1] + height,
            from[2] + half,
        ];
        let chest = [
            f64::from(target.x) - distance,
            f64::from(target.y) - 0.5,
            f64::from(target.z) - distance,
            f64::from(target.x) + 1.0 + distance,
            f64::from(target.y) + 0.875 + 0.5,
            f64::from(target.z) + 1.0 + distance,
        ];
        super::super::physics::intersects(body, chest)
    }

    pub(super) fn middle(mob: &MobCore, feet: [f64; 3]) -> [f64; 3] {
        [feet[0], feet[1] + f64::from(mob.size.height) / 2.0, feet[2]]
    }

    pub(super) fn sees_a_side(target: BlockPos, from: [f64; 3], cx: &TickContext) -> bool {
        let center = block_center(target);
        let level = |pos| (cx.block)(pos).unwrap_or(Block::Stone.default_state());
        [
            [1.0, 0.0, 0.0],
            [-1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, -1.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, -1.0],
        ]
        .iter()
        .any(|d| {
            let to = [
                center[0] + 0.5 * d[0],
                center[1] + 0.5 * d[1],
                center[2] + 0.5 * d[2],
            ];
            tuff_core::world::raycast::cast_colliders(from, to, &level)
                .is_some_and(|hit| hit.pos == target)
        })
    }

    /// Whether the way to `target` still leads to a spot it can be
    /// reached from: a path that ends out of reach or out of sight marks
    /// the chest unreachable.
    pub(super) fn reachable(&self, mob: &MobCore, target: BlockPos, cx: &TickContext) -> bool {
        let path = mob.navigation.path.as_ref();
        let feet = path.and_then(|p| p.end()).map_or(mob.base.position, |n| {
            [f64::from(n.x) + 0.5, f64::from(n.y), f64::from(n.z) + 0.5]
        });
        let from = Self::middle(mob, feet);
        let range = if path.is_some_and(|p| p.is_done()) {
            1.0
        } else {
            0.5
        };
        let reach = Self::within(mob, target, range, from);
        (path.is_none() && !reach) || (reach && Self::sees_a_side(target, from, cx))
    }

    pub(super) fn mark_unreachable(
        &mut self,
        mob: &mut MobCore,
        target: BlockPos,
        cx: &mut TickContext,
    ) {
        self.visited.retain(|p| *p != target);
        self.unreachable.push(target);
        self.unreachable_until = cx.tick + MEMORY_TICKS;
        if self.unreachable.len() > MAX_UNREACHABLE {
            self.give_up(mob, cx);
        } else {
            self.drop_target(mob, cx);
        }
    }

    /// Reads the container at `target` (a double chest as both halves) and
    /// decides what examining it will do.
    pub(super) fn resolve(mob: &MobCore, cx: &mut TickContext, target: BlockPos) -> Outcome {
        let items: ContainerView = (cx.container)(target).unwrap_or_default();
        if Self::picking_up(mob) {
            match items.iter().flatten().next() {
                Some(stack) => {
                    let stack = tuff_item::item::ItemSnapshot::of(stack)
                        .with_count(stack.count().min(TAKE_MAX));
                    Outcome::Take { stack }
                }
                None => Outcome::TakeFail,
            }
        } else {
            let held = mob.equipment.get(EquipmentSlot::Mainhand);
            let Some(held) = held else {
                return Outcome::DepositFail;
            };
            let empty = items.iter().all(Option::is_none);
            let matches = empty || items.iter().flatten().any(|s| s.item == held.item);
            if matches {
                Outcome::Deposit {
                    stack: tuff_item::item::ItemSnapshot::of(held),
                }
            } else {
                Outcome::DepositFail
            }
        }
    }

    pub(super) fn start_dwell(
        &mut self,
        mob: &mut MobCore,
        cx: &mut TickContext,
        target: BlockPos,
    ) {
        mob.navigation.stop();
        self.outcome = Some(Self::resolve(mob, cx, target));
        self.stage = Stage::Interacting;
        self.elapsed = 0;
    }

    /// The lid opens and the animation starts on the first tick at the
    /// chest, and its sound plays on the ninth.
    pub(super) fn interact(&mut self, mob: &mut MobCore, cx: &mut TickContext, target: BlockPos) {
        mob.navigation.stop();
        mob.look.set_look_at(block_center(target), 30.0, 30.0);
        self.elapsed += 1;
        let Some(outcome) = &self.outcome else {
            return;
        };
        let (state, sound) = match outcome {
            Outcome::Take { .. } => (STATE_GETTING_ITEM, SoundEvent::EntityCopperGolemNoItemGet),
            Outcome::TakeFail => (
                STATE_GETTING_NO_ITEM,
                SoundEvent::EntityCopperGolemNoItemNoGet,
            ),
            Outcome::Deposit { .. } => (STATE_DROPPING_ITEM, SoundEvent::EntityCopperGolemItemDrop),
            Outcome::DepositFail => (
                STATE_DROPPING_NO_ITEM,
                SoundEvent::EntityCopperGolemItemNoDrop,
            ),
        };
        if self.elapsed == LID_OPEN_TICK {
            cx.effects.push(Effect::ContainerOpen {
                pos: target,
                open: true,
            });
            self.open = Some(target);
            mob.signal = state;
        }
        if self.elapsed == SOUND_TICK {
            mob.sound(sound, 1.0, 1.0, cx);
        }
    }

    /// The end of the dwell: the container is read again and the golem
    /// takes or places what it can now.
    pub(super) fn finish(&mut self, mob: &mut MobCore, cx: &mut TickContext, target: BlockPos) {
        self.drop_target(mob, cx);
        match Self::resolve(mob, cx, target) {
            Outcome::Take { stack } => {
                cx.effects
                    .push(Effect::CopperGolemTake { pos: target, stack });
                self.visited.clear();
            }
            Outcome::Deposit { stack } => {
                let view = (cx.container)(target).unwrap_or_default();
                let placed = tuff_core::world::block_entity::golem_room(&view, &stack);
                if placed > 0
                    && let Some(given) = mob.equipment.take_some(EquipmentSlot::Mainhand, placed)
                {
                    cx.effects.push(Effect::CopperGolemDeposit {
                        pos: target,
                        stack: given,
                    });
                }
                if mob.equipment.get(EquipmentSlot::Mainhand).is_none() {
                    self.visited.clear();
                }
            }
            Outcome::TakeFail | Outcome::DepositFail => {}
        }
    }
}

impl Goal for TransportItems {
    fn flags(&self) -> Flags {
        Flags::MOVE
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        cx.tick >= self.cooldown_until && mob.leash.is_none()
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        cx.tick >= self.cooldown_until && mob.leash.is_none() && !mob.is_panicking()
    }

    fn stop(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        mob.navigation.stop();
        self.drop_target(mob, cx);
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        if cx.tick >= self.unreachable_until {
            self.unreachable.clear();
        }
        let Some(target) = self.target else {
            match self.find_target(mob, cx) {
                Some(pos) if self.visited.len() < MAX_VISITED => {
                    self.visited.push(pos);
                    self.target = Some(pos);
                }
                _ => self.give_up(mob, cx),
            }
            return;
        };
        let picking_up = Self::picking_up(mob);
        let still_wanted = (cx.block)(target).is_some_and(|state| Self::wanted(picking_up, state));
        if !still_wanted || Self::blocked(target, cx) {
            self.drop_target(mob, cx);
            return;
        }
        let others_open = |open_here: bool| {
            (cx.container_info)(target)
                .openers
                .saturating_sub(usize::from(open_here))
                > 0
        };
        let middle = Self::middle(mob, mob.base.position);
        match self.stage {
            Stage::Queuing => {
                if !others_open(false) {
                    self.stage = Stage::Travelling;
                    mob.move_to_position(block_center(target), TRANSPORT_SPEED, cx);
                }
            }
            Stage::Travelling => {
                if !self.reachable(mob, target, cx) {
                    self.mark_unreachable(mob, target, cx);
                } else if Self::within(mob, target, QUEUE_DISTANCE, middle) && others_open(false) {
                    mob.navigation.stop();
                    self.stage = Stage::Queuing;
                } else if mob.distance_sq(block_center(target)) <= REACH_SQ {
                    self.start_dwell(mob, cx, target);
                } else {
                    mob.move_to_position(block_center(target), TRANSPORT_SPEED, cx);
                }
            }
            Stage::Interacting => {
                if !Self::within(mob, target, CONTINUE_DISTANCE, middle) {
                    self.close_lid(cx);
                    mob.signal = STATE_IDLE;
                    self.outcome = None;
                    self.stage = Stage::Travelling;
                    self.elapsed = 0;
                    return;
                }
                self.interact(mob, cx, target);
                if self.elapsed >= DWELL_TICKS {
                    self.finish(mob, cx, target);
                }
            }
        }
    }
}
