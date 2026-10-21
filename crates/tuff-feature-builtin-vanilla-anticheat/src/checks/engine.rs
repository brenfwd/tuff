//! Per-tick movement prediction. From the state the last move left the
//! client in (position, internal velocity, ground, jump delay, the block
//! that holds it back), each reported move is replayed tick by tick the
//! way the game's client moves a walking player: input scaling, jumping,
//! block friction, speed and jump factors, climbing, cobwebs, edge
//! back-off while sneaking, collision with the client's own blocks, solid
//! entities and step-up, bounces and gravity, levitation, wading through
//! water and lava with their currents, and elytra flight. Velocity the
//! server gave (knockback, blasts), firework boosts and pushes from
//! nearby entities are replayed at the tick boundary where the client
//! applied them. A move that one candidate explains within [`MATCH`] per
//! axis is accepted and that candidate's state carries on; otherwise the
//! move may not go farther, or rise higher, than the farthest candidate.
//! Swimming follows the client's swim state, its 0.6-tall pose and the
//! rise toward the look; crawling and forced crouching slow the input.
//! Bubble columns, riptides and other states the replay does not model
//! make the engine forget its state and re-seed from the next observed
//! move; so does a move under server velocity that no replay
//! places exactly but that stays within the replays' reach.

use tuff_core::protocol::play::{Movement, Serverbound};
use tuff_core::world::contact::WATER_CURRENT_SCALE;
use tuff_core::world::fluid_api::FluidBehaviors;
use tuff_core::world::physics::{self, Aabb, modified_friction};
use tuff_core::world::session::mirror::ChunkMirror;

use super::predict::Keys;
use super::velocity::Impulse;
use super::{Check, Facts, Fix, Flag};
use crate::trace::{Tags, Trace};
use tuff_core::world::session::{Shove, ShoveKind};

mod model;
use model::*;
pub use model::{Body, Prediction};

/// The prediction state of one player.

#[derive(Default)]
pub struct Engine {
    state: Option<State>,
    seeded: bool,
    /// Moves left in which an unexplained move re-seeds the state from
    /// what it showed, after a reset.
    uncertain: u8,
    keys: u8,
    /// Each tick the client ended since its last position.
    recorded: Vec<Ended>,
    rotation: Option<(f32, f32)>,
    ticking: bool,
    /// The seeded move started in a fluid, which may have lifted it out.
    seeded_wet: bool,
    /// Whether the server last had Dolphin's Grace on the player, and the
    /// tick that changed.
    dolphins: Option<bool>,
    dolphins_changed: Option<u64>,
    /// While the client may not have caught up with that change: the
    /// state the last move left under the other Dolphin's Grace.
    other_dolphins: Option<State>,
    /// When the last tick's jump was blocked outright (a ceiling right
    /// over the head), the state had the client not jumped, which ends at
    /// the same place.
    unjumped: Option<State>,
    /// The effects that shape a tick (slow falling, levitation, jump
    /// boost, speed) as last seen, and the tick they changed.
    effects: Option<Effects>,
    effects_changed: Option<u64>,
    /// What they were before that change.
    effects_before: Option<Effects>,
    /// The last tick the server had the player gliding.
    glided: Option<u64>,
    /// The last tick a firework pulled the player, as the server has it.
    boosted: Option<u64>,
    /// The first server velocity not yet replayed.
    consumed: u64,
    /// The first shove not yet replayed.
    shoves_consumed: u64,
    /// The spin since the last launch was replayed from its start.
    spun: bool,
}

/// Slow falling, Levitation and Jump Boost levels plus one, the Speed
/// amplifier and Weaving.
type Effects = (bool, u8, u8, Option<u8>, bool);

/// An entity the client sees near a position move.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Nearby {
    pub bounds: Aabb,
    /// The client collides with it (a boat, a shulker).
    pub solid: bool,
    /// How far the client's copy of the box may differ from the last known
    /// one (a shulker's peek, a boat or ghast still interpolating).
    pub fuzz: f64,
}

impl Nearby {
    /// A box whose client position cannot be pinned down touches the
    /// player's path.
    pub(crate) fn blurs(&self, from: [f64; 3], to: [f64; 3]) -> bool {
        if self.fuzz <= 0.0 || !self.solid {
            return false;
        }
        let body = [
            from[0].min(to[0]) - 0.3,
            from[1].min(to[1]),
            from[2].min(to[2]) - 0.3,
            from[0].max(to[0]) + 0.3,
            from[1].max(to[1]) + 1.8,
            from[2].max(to[2]) + 0.3,
        ];
        let f = self.fuzz;
        let b = self.bounds;
        physics::intersects(
            [b[0] - f, b[1] - f, b[2] - f, b[3] + f, b[4] + f, b[5] + f],
            body,
        )
    }

    /// Close enough to `to` that it may have pushed the player in ways
    /// the replay did not place.
    fn crowds(&self, to: [f64; 3]) -> bool {
        let centre = [
            (self.bounds[0] + self.bounds[3]) / 2.0,
            self.bounds[1],
            (self.bounds[2] + self.bounds[5]) / 2.0,
        ];
        (centre[0] - to[0]).abs() < CROWD_REACH
            && (centre[2] - to[2]).abs() < CROWD_REACH
            && (centre[1] - to[1]).abs() < 3.0
    }
}

/// Why the engine skips a move and re-seeds.
fn unpredictable(facts: &Facts, spun: bool, launching: bool) -> bool {
    facts.teleport_pending
        || facts.riding
        || facts.flying
        || facts.may_fly
        || facts.instabuild
        || facts.game_mode == tuff_core::protocol::play::GameMode::Spectator
        || (facts.riptide && !spun && !launching)
        || facts.shoved
        || !facts.terrain.is_some_and(|t| t.known)
}

/// The most each horizontal axis may be pushed by the entities touching
/// the body at `position`, from the side each lies on.
fn push_bounds(nearby: &[Nearby], position: [f64; 3]) -> Option<[[f64; 2]; 2]> {
    let reach = physics::inflate(bounds(position), [PUSH_MARGIN; 3]);
    let mut out = [[0.0; 2]; 2];
    let mut any = false;
    for entity in nearby
        .iter()
        .filter(|n| physics::intersects(n.bounds, reach))
        .take(MAX_PUSHERS)
    {
        any = true;
        for (slot, axis) in [(0, 0), (1, 2)] {
            let offset = (entity.bounds[axis] + entity.bounds[axis + 3]) / 2.0 - position[axis];
            if offset < PUSH_SIGN_MARGIN {
                out[slot][1] += PUSH_PER_ENTITY;
            }
            if offset > -PUSH_SIGN_MARGIN {
                out[slot][0] -= PUSH_PER_ENTITY;
            }
        }
    }
    any.then_some(out)
}

impl Engine {
    /// Follows the keys, rotations and tick ends the client reports.
    pub fn observe(&mut self, facts: &Facts, packet: &Serverbound) {
        match packet {
            Serverbound::PlayerInput(flags) => self.keys = *flags,
            Serverbound::Move(Movement {
                rotation: Some(rotation),
                ..
            }) => self.rotation = Some(*rotation),
            Serverbound::ClientTickEnd => {
                if self.recorded.len() >= RECORDED {
                    self.recorded.remove(0);
                }
                self.recorded.push(Ended {
                    flags: self.keys,
                    rotation: self.rotation,
                    sprint: facts.sprinting,
                    item: item_scale(facts),
                });
                self.ticking = true;
            }
            _ => {}
        }
    }

    /// The client placed a block under the feet the world has not
    /// confirmed, so the next moves may rest on it.
    pub fn world_ahead(&mut self) {
        self.uncertain = self.uncertain.max(WORLD_AHEAD_MOVES);
    }

    /// Moves the state along with a client a test world moved.
    #[cfg(test)]
    pub(crate) fn shift(&mut self, by: [f64; 3]) {
        if let Some(state) = self.state.as_mut() {
            for axis in 0..3 {
                state.position[axis] += by[axis];
            }
        }
    }

    /// Forgets the state; the next move seeds it.
    pub fn reset(&mut self) {
        self.state = None;
    }

    /// Judges a position move against the client's blocks.
    pub fn judge(
        &mut self,
        facts: &Facts,
        mirror: &ChunkMirror,
        fluids: &dyn FluidBehaviors,
        movement: &Movement,
        to: [f64; 3],
    ) -> Option<Prediction> {
        let ended = std::mem::take(&mut self.recorded);
        let solids: Vec<Aabb> = facts
            .nearby
            .iter()
            .filter(|n| n.solid)
            .map(|n| n.bounds)
            .collect();
        let world = World {
            mirror,
            fluids,
            solids: &solids,
        };
        let from = facts.position;
        if self.dolphins != Some(facts.dolphins_grace) {
            if self.dolphins.is_some() {
                self.dolphins_changed = Some(facts.tick);
            }
            self.dolphins = Some(facts.dolphins_grace);
        }
        let dolphins_unsure = self
            .dolphins_changed
            .is_some_and(|tick| facts.tick <= tick + DOLPHINS_CATCH_UP);
        let unjumped = std::mem::take(&mut self.unjumped).filter(|s| s.position == from);
        let other_dolphins = std::mem::take(&mut self.other_dolphins)
            .filter(|s| dolphins_unsure && s.position == from);
        let effects = (
            facts.slow_falling,
            facts.levitation_level,
            facts.jump_boost,
            facts.speed,
            facts.weaving,
        );
        if self.effects != Some(effects) {
            if self.effects.is_some() {
                self.effects_changed = Some(facts.tick);
                self.effects_before = self.effects;
            }
            self.effects = Some(effects);
        }
        let effects_unsure = self
            .effects_changed
            .is_some_and(|tick| facts.tick <= tick + STATE_CATCH_UP);
        if facts.gliding {
            self.glided = Some(facts.tick);
        }
        let glide_unsure = !facts.gliding
            && self
                .glided
                .is_some_and(|tick| facts.tick <= tick + STATE_CATCH_UP);
        let rules = Rules {
            body: facts.body,
            speed_amplifier: facts.speed,
            jump_boost: 0.1 * f32::from(facts.jump_boost),
            slow_falling: facts.slow_falling,
            dolphins_grace: facts.dolphins_grace,
            lava_current: LAVA_CURRENT_SCALE[usize::from(!facts.fast_lava)],
            levitation: facts.levitation_level.checked_sub(1),
            gliding: facts.gliding,
            snow_walker: facts.snow_walker,
            spinning: facts.riptide,
            weaving: facts.weaving,
        };
        let flipped = Rules {
            dolphins_grace: !rules.dolphins_grace,
            ..rules
        };
        let pending: Vec<Impulse> = facts
            .impulses
            .iter()
            .filter(|impulse| impulse.seq >= self.consumed)
            .copied()
            .collect();
        let shoves: Vec<(u64, Shove)> = facts
            .shoves
            .iter()
            .filter(|(seq, _)| *seq >= self.shoves_consumed)
            .copied()
            .collect();
        if !facts.riptide {
            self.spun = false;
        }
        let launching = shoves
            .iter()
            .any(|(_, shove)| matches!(shove.kind, ShoveKind::Riptide { .. }));
        let usable = !facts.confirming
            && !unpredictable(facts, self.spun, launching)
            && !world.unmodeled(from, to)
            && self.state.is_some_and(|s| s.position == from);
        if !usable {
            return self.reseed(facts, movement, from, to, &rules, &world);
        }
        let state = self.state.expect("checked usable");
        let rotation = movement
            .rotation
            .unwrap_or((facts.rotation[0], facts.rotation[1]));
        let using = item_scale(facts);
        let shift = self.keys & Keys::SNEAK != 0;
        let shift_changed = ended
            .last()
            .is_some_and(|ended| (ended.flags & Keys::SNEAK != 0) != shift);
        let base = Input {
            keys: Keys(self.keys & MOVEMENT_KEYS),
            jump: self.keys & JUMP != 0,
            sprint: facts.sprinting,
            slow: facts.sneaking,
            shift,
            item: using,
            yaw: rotation.0,
            pitch: rotation.1,
        };
        let unreported = ended.get(1..).unwrap_or_default();
        let skipped = unreported
            .len()
            .saturating_sub((MAX_IDLE + MAX_HIDDEN) as usize);
        let recorded = &unreported[skipped..];
        let looks: Vec<[f32; 2]> = std::iter::once(skipped)
            .map(|i| {
                ended
                    .get(i)
                    .and_then(|e| e.rotation)
                    .map_or(facts.rotation, |(yaw, pitch)| [yaw, pitch])
            })
            .chain(
                recorded
                    .iter()
                    .map(|e| e.rotation.map_or(facts.rotation, |(y, p)| [y, p])),
            )
            .collect();
        let ticks = recorded.len() as u32 + 1;
        let pushers = push_bounds(&facts.nearby, from);
        if facts.firework {
            self.boosted = Some(facts.tick);
        }
        let round_trip = u64::try_from(facts.latency_ms.max(0)).unwrap_or(0) * 2;
        let boost_window =
            (round_trip.div_ceil(50) + BOOST_CATCH_UP + BOOST_LIFETIME_SPREAD).min(STATE_CATCH_UP);
        let boosting = facts.gliding
            && self
                .boosted
                .is_some_and(|tick| facts.tick <= tick + boost_window);
        let boosts: Vec<Option<(u32, u32)>> = if boosting {
            std::iter::once(Some((0, ticks)))
                .chain(std::iter::once(None))
                .chain((1..ticks).map(|k| Some((k, ticks))))
                .chain((1..ticks).map(|k| Some((0, k))))
                .collect()
        } else {
            vec![None]
        };
        let (world_ref, looks_ref) = (&world, looks.as_slice());
        let replay = move |rules: Rules, plan: Plan| Replay {
            rules,
            world: world_ref,
            plan,
            looks: looks_ref,
        };
        let mut search = Search::new(to[1] - from[1], pushers.is_some() && recorded.is_empty());
        let mut plan = Plan {
            boost: boosts[0],
            ..Plan::default()
        };
        self.search(
            &mut search,
            state,
            &base,
            recorded,
            to,
            &replay(rules, plan),
        );
        if search.found.is_none()
            && let Some(alternative) = unjumped
        {
            self.search(
                &mut search,
                alternative,
                &base,
                recorded,
                to,
                &replay(rules, plan),
            );
        }
        let mut tags = Tags::empty();
        for &boost in &boosts[1..] {
            if search.found.is_some() {
                break;
            }
            plan = Plan {
                boost,
                ..Plan::default()
            };
            self.search(
                &mut search,
                state,
                &base,
                recorded,
                to,
                &replay(rules, plan),
            );
        }
        let mut applied = None;
        'impulses: for last in 0..pending.len() {
            if search.found.is_some() {
                break;
            }
            let change = Change::of(&pending[..=last]);
            for at in 0..ticks {
                plan = Plan {
                    boost: boosts[0],
                    impulse: Some((at, change)),
                    ..Plan::default()
                };
                self.search(
                    &mut search,
                    state,
                    &base,
                    recorded,
                    to,
                    &replay(rules, plan),
                );
                if search.found.is_some() {
                    applied = Some(pending[last].seq);
                    break 'impulses;
                }
            }
        }
        let mut shoved = None;
        'shoves: for entry in &shoves {
            if search.found.is_some() {
                break;
            }
            for at in 0..ticks {
                plan = Plan {
                    boost: boosts[0],
                    shove: Some((at, entry.1.kind)),
                    ..Plan::default()
                };
                self.search(
                    &mut search,
                    state,
                    &base,
                    recorded,
                    to,
                    &replay(rules, plan),
                );
                if search.found.is_some() {
                    shoved = Some(*entry);
                    break 'shoves;
                }
            }
        }
        if search.found.is_none() {
            plan = Plan {
                boost: boosts[0],
                ..Plan::default()
            };
        }
        if search.found.is_none() && (self.seeded || self.uncertain > 0) {
            let climbing = world.climbable(state.position);
            // A client still waiting for its terrain ends ticks without
            // moving, so a seed may also lead straight into this move.
            let frozen: &[Ended] = &[];
            'variants: for variant in seed_variants(&state, &rules, self.seeded_wet, climbing) {
                for ticks in [recorded, frozen] {
                    self.search(&mut search, variant, &base, ticks, to, &replay(rules, plan));
                    // Grounding an airborne guess covers a landing the seed
                    // missed, never a jump impulse taken from mid-air.
                    if variant.on_ground
                        && !state.on_ground
                        && search.found.is_some_and(|(_, marks, _)| marks.jumped)
                    {
                        search.found = None;
                        search.last = None;
                    }
                    if search.found.is_some() {
                        break 'variants;
                    }
                }
            }
        }
        if search.found.is_none() {
            // Only toward slower movement: a desync that would make the
            // client faster is a cheat's claim, not an explanation.
            let toggles = [
                (
                    Input {
                        sprint: false,
                        ..base
                    },
                    Tags::SPRINT_RESYNC,
                ),
                (
                    Input {
                        slow: !base.slow || !shift_changed,
                        ..base
                    },
                    Tags::SNEAK_RESYNC,
                ),
                (
                    Input {
                        item: USING_SCALE,
                        ..base
                    },
                    Tags::ITEM_RESYNC,
                ),
                (
                    Input {
                        shift: !base.shift,
                        ..base
                    },
                    Tags::SNEAK_RESYNC,
                ),
            ];
            for (input, tag) in toggles {
                if input == base {
                    continue;
                }
                self.search(
                    &mut search,
                    state,
                    &input,
                    recorded,
                    to,
                    &replay(rules, plan),
                );
                if search.found.is_some() {
                    tags = tags.with(tag, true);
                    break;
                }
            }
        }
        let mut last_rules = rules;
        if search.found.is_none()
            && let Some(other) = other_dolphins
        {
            self.search(
                &mut search,
                other,
                &base,
                recorded,
                to,
                &replay(flipped, plan),
            );
            last_rules = flipped;
        }
        let mut alternatives = Vec::new();
        if effects_unsure
            && let Some((slow_falling, levitation, jump_boost, speed, weaving)) =
                self.effects_before
        {
            alternatives.push(Rules {
                slow_falling,
                levitation: levitation.checked_sub(1),
                jump_boost: 0.1 * f32::from(jump_boost),
                speed_amplifier: speed,
                weaving,
                ..rules
            });
        }
        if glide_unsure {
            alternatives.push(Rules {
                gliding: true,
                ..rules
            });
        }
        for other in alternatives {
            if search.found.is_some() {
                break;
            }
            self.search(
                &mut search,
                state,
                &base,
                recorded,
                to,
                &replay(other, plan),
            );
            last_rules = other;
        }
        if search.found.is_none()
            && let (Some(bounds), Some(ends)) = (pushers, search.ends.take())
        {
            let mut marks = Marks::default();
            if let Some(end) = pushed_end(&ends, bounds, to, &rules, &world, &mut marks) {
                search.found = Some((end, marks, 0));
                tags = tags.with(Tags::ENTITY_PUSH, true);
            }
        }
        let delta = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
        let actual = delta[0].hypot(delta[2]);
        let mut prediction = Prediction {
            allowed: search.farthest(),
            actual,
            tried: search.tried,
            ..Prediction::default()
        };
        if let Some((end, marks, hidden)) = search.found {
            if let Some(seq) = applied {
                self.consumed = seq + 1;
            }
            if let Some((seq, shove)) = shoved {
                self.shoves_consumed = seq + 1;
                self.spun |= matches!(shove.kind, ShoveKind::Riptide { .. });
            }
            prediction.tags = tags
                .with(Tags::PREDICTED, true)
                .with(Tags::HIDDEN_TICKS, hidden > 0)
                .with(Tags::EDGE_BACKOFF, marks.edge)
                .with(Tags::STEP_UP, marks.step)
                .with(Tags::BOUNCE, marks.bounce)
                .with(Tags::CLIMB, marks.climb)
                .with(Tags::STUCK, marks.stuck)
                .with(Tags::FLUID, marks.fluid)
                .with(Tags::GLIDE, marks.glide)
                .with(Tags::SWIM, marks.swim)
                .with(Tags::JUMP_DELAY, marks.jump_delay)
                .with(Tags::PUSHED_OUT, marks.pushed_out)
                .with(Tags::IMPULSE, applied.is_some())
                .with(Tags::BOOST, plan.boost.is_some())
                .with(Tags::LEVITATION, last_rules.levitation.is_some())
                .with(Tags::SEEDED, self.seeded);
            self.state = Some(State {
                position: to,
                ..end
            });
            if let Some((before, input)) = search.last
                && input.jump
                && end.no_jump == NO_JUMP_DELAY
            {
                let held = Input {
                    jump: false,
                    ..input
                };
                let still = tick(before, &held, &last_rules, &world, &mut Marks::default());
                let error = [0, 1, 2]
                    .map(|a| (still.position[a] - end.position[a]).abs())
                    .into_iter()
                    .fold(0.0, f64::max);
                if error <= MATCH {
                    self.unjumped = Some(State {
                        position: to,
                        ..still
                    });
                }
            }
            if dolphins_unsure && let Some((before, input)) = search.last {
                let other = Rules {
                    dolphins_grace: !last_rules.dolphins_grace,
                    ..last_rules
                };
                let end = tick(before, &input, &other, &world, &mut Marks::default());
                self.other_dolphins = Some(State {
                    position: to,
                    ..end
                });
            }
            // A seed's guessed velocity and ground can match a move and
            // still be wrong, so its variants stay open a few moves more.
            self.uncertain = if self.seeded {
                self.uncertain.saturating_sub(1)
            } else {
                0
            };
            self.seeded = false;
            return Some(prediction);
        }
        if boosting || glide_unsure || effects_unsure {
            return self.reseed(facts, movement, from, to, &rules, &world);
        }
        let pushed =
            !pending.is_empty() || facts.push.horizontal > 0.0 || facts.push.launch.is_some();
        let crowd = facts
            .nearby
            .iter()
            .filter(|n| n.crowds(to) || n.blurs(from, to))
            .count();
        let uncertain = self.uncertain > 0;
        let slack = if uncertain { SEEDED_SLACK } else { 0.0 }
            + crowd.min(MAX_PUSHERS) as f64 * PUSH_PER_ENTITY;
        prediction.horizontal =
            (search.outside([delta[0], delta[2]]) - HORIZONTAL_TOLERANCE - slack).max(0.0);
        let held_up = !movement.on_ground
            && !uncertain
            && !pushed
            && !self.seeded
            && solids.is_empty()
            && search.highest.is_finite();
        prediction.vertical = if (delta[1] > 0.0 || held_up) && crowd == 0 {
            (delta[1] - search.highest - VERTICAL_TOLERANCE).max(0.0)
        } else {
            0.0
        };
        prediction.tags = Tags::empty()
            .with(Tags::SEEDED, self.seeded)
            .with(Tags::CROWDED, crowd > 0)
            .with(Tags::PUSH, pushed);
        if pushed && !prediction.excessive() {
            // Within what the server's velocity allows, but not replayed:
            // follow the client from what it showed.
            prediction.unplaced = !pending.is_empty();
            self.reseed(facts, movement, from, to, &rules, &world);
            return Some(prediction);
        }
        self.uncertain = self.uncertain.saturating_sub(1);
        self.state = Some(if uncertain && !prediction.excessive() {
            self.seed(
                Some(state),
                from,
                to,
                movement.on_ground,
                facts.sprinting,
                &rules,
                &world,
            )
        } else {
            let closest = search.closest.map_or(state, |(_, s)| s);
            carry(closest, from, to, movement.on_ground, &rules, &world)
        });
        self.seeded = false;
        Some(prediction)
    }

    /// Forgets the replay and seeds the state from the observed move.
    fn reseed(
        &mut self,
        facts: &Facts,
        movement: &Movement,
        from: [f64; 3],
        to: [f64; 3],
        rules: &Rules,
        world: &World,
    ) -> Option<Prediction> {
        let prior = if facts.confirming { None } else { self.state };
        self.state = Some(self.seed(
            prior,
            from,
            to,
            movement.on_ground,
            facts.sprinting,
            rules,
            world,
        ));
        self.seeded = true;
        self.spun = false;
        self.shoves_consumed = facts
            .shoves
            .last()
            .map_or(self.shoves_consumed, |(s, _)| s + 1);
        let fluid = world.immersion(from, Pose::Standing);
        self.seeded_wet = fluid.water > 0.0 || fluid.lava > 0.0;
        self.uncertain = SEED_MOVES;
        None
    }

    /// Replays the unreported ticks and the reported one: first with the
    /// keys the client reported for each tick, then with every input held
    /// through the last unreported ticks and every input with and without
    /// a jump for the reported one.
    fn search(
        &self,
        search: &mut Search,
        start: State,
        base: &Input,
        recorded: &[Ended],
        to: [f64; 3],
        replay: &Replay,
    ) {
        let as_input = |ended: Ended| {
            let (yaw, pitch) = ended.rotation.unwrap_or((base.yaw, base.pitch));
            Input {
                keys: Keys(ended.flags & MOVEMENT_KEYS),
                jump: ended.flags & JUMP != 0,
                shift: ended.flags & Keys::SNEAK != 0,
                slow: ended.flags & Keys::SNEAK != 0,
                yaw,
                pitch,
                sprint: ended.sprint,
                item: ended.item,
            }
        };
        let mut state = start;
        let mut marks = Marks::default();
        for (t, entry) in recorded.iter().enumerate() {
            state = replay.step(state, t as u32, &as_input(*entry), &mut marks);
        }
        if evaluate(
            search,
            start,
            state,
            marks,
            base,
            recorded.len() as u32,
            to,
            replay,
        ) {
            return;
        }
        if base.jump && start.no_jump <= 1 {
            // The client may still wait out the delay of a jump the state
            // does not show (one made before a re-seed): assume the least
            // delay that holds this jump back, so the next one is not.
            let mut state = State {
                no_jump: 2,
                ..start
            };
            let mut marks = Marks {
                jump_delay: true,
                ..Marks::default()
            };
            for (t, entry) in recorded.iter().enumerate() {
                state = replay.step(state, t as u32, &as_input(*entry), &mut marks);
            }
            if !marks.jumped
                && evaluate(
                    search,
                    start,
                    state,
                    marks,
                    base,
                    recorded.len() as u32,
                    to,
                    replay,
                )
            {
                return;
            }
        }
        let hidden = recorded.len().min(MAX_HIDDEN as usize);
        let idle = recorded.len() - hidden;
        let mut state = start;
        let mut marks = Marks::default();
        for (t, entry) in recorded[..idle].iter().enumerate() {
            state = replay.step(state, t as u32, &as_input(*entry), &mut marks);
        }
        let counts: &[u32] = if self.ticking {
            &[hidden as u32]
        } else {
            &[0, 1, 2]
        };
        let quiet = recorded.last().map_or(*base, |ended| as_input(*ended));
        let held = |keys: Keys| Input { keys, ..quiet };
        for &count in counts {
            let quiet_keys: &[Keys] = if count == 0 { &KEYS[..1] } else { &KEYS };
            for keys in quiet_keys {
                let Some((s, m)) = silent_ticks(
                    start,
                    state,
                    marks,
                    idle as u32,
                    count,
                    &held(*keys),
                    replay,
                ) else {
                    continue;
                };
                for last in KEYS {
                    for jump in [false, true] {
                        let input = Input {
                            keys: last,
                            jump,
                            ..*base
                        };
                        if evaluate(search, start, s, m, &input, count + idle as u32, to, replay) {
                            return;
                        }
                    }
                }
            }
        }
    }

    /// A state estimated from an observed move: its delta taken as the
    /// tick's motion, stopped on axes that touch a block, then carried
    /// through the end of the tick.
    #[allow(clippy::too_many_arguments)]
    fn seed(
        &self,
        prior: Option<State>,
        from: [f64; 3],
        to: [f64; 3],
        claimed_ground: bool,
        sprint: bool,
        rules: &Rules,
        world: &World,
    ) -> State {
        let delta = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
        let pose = prior.map_or(Pose::Standing, |p| p.pose);
        let a = bounds(from);
        let was_on_ground = prior.map_or_else(
            || !world.free([a[0], a[1] - 1.0e-3, a[2], a[3], a[1], a[5]]),
            |p| p.on_ground,
        );
        let arrived = world.immersion(to, pose);
        let in_water = arrived.water > 0.0;
        let eyes = in_water && world.eyes_in_water(to, pose);
        let swimming = sprint && in_water && (eyes || prior.is_some_and(|p| p.swimming));
        let desired = if swimming || rules.gliding {
            Pose::Prone
        } else if self.keys & Keys::SNEAK != 0 {
            Pose::Crouching
        } else {
            Pose::Standing
        };
        let end_pose = if !world.fits(to, Pose::Prone, Context::seeded(rules)) {
            pose
        } else if world.fits(to, desired, Context::seeded(rules)) {
            desired
        } else if world.fits(to, Pose::Crouching, Context::seeded(rules)) {
            Pose::Crouching
        } else {
            Pose::Prone
        };
        let b = body(to, end_pose);
        let fluid = world.immersion(from, pose);
        let mut velocity = delta;
        if fluid.water <= 0.0 {
            push_current(&mut velocity, &arrived.water_current, WATER_CURRENT_SCALE);
            push_current(&mut velocity, &arrived.lava_current, rules.lava_current);
        }
        let seeded = Context::seeded(rules);
        let floor = !world.free_in([b[0], b[1] - 1.0e-3, b[2], b[3], b[1], b[5]], b[1], seeded);
        let on_ground = claimed_ground && (delta[1] <= 1.0e-9 || floor);
        if on_ground {
            velocity[1] = 0.0;
        }
        if delta[1] > 0.0
            && !world.free_in([b[0], b[4], b[2], b[3], b[4] + 1.0e-3, b[5]], b[1], seeded)
        {
            velocity[1] = 0.0;
        }
        let mut walled = false;
        for (axis, lo, hi) in [(0, 0, 3), (2, 2, 5)] {
            let d = delta[axis];
            if d == 0.0 {
                continue;
            }
            let mut probe = b;
            if d > 0.0 {
                probe[lo] = b[hi];
                probe[hi] = b[hi] + CONTACT;
            } else {
                probe[hi] = b[lo];
                probe[lo] = b[lo] - CONTACT;
            }
            probe[1] += 1.0e-3;
            if !world.free_in(probe, b[1], seeded) {
                velocity[axis] = 0.0;
                walled = true;
            }
        }
        let in_snow = world.in_powder_snow(from, to, pose);
        if (walled || self.keys & JUMP != 0)
            && (world.climbable(to) || (in_snow && rules.snow_walker))
        {
            velocity[1] = CLIMB_SPEED;
        }
        let mut state = State {
            position: to,
            velocity,
            on_ground,
            horizontal_collision: false,
            supporting: if on_ground {
                physics::supporting_block(b, &|pos| world.block(pos))
            } else {
                None
            },
            no_jump: 0,
            fall: 0.0,
            stuck: None,
            in_snow: false,
            pose: end_pose,
            in_water,
            eyes,
            swimming,
            sprinting: sprint,
        };
        let start = State {
            on_ground: was_on_ground,
            supporting: prior.and_then(|p| p.supporting),
            position: from,
            ..state
        };
        let block_friction = if was_on_ground {
            modified_friction(
                world.movement_at(world.on_pos(&start, 0.500_001)).friction,
                rules.body.friction_modifier,
            )
        } else {
            1.0
        };
        let factor = f64::from(world.speed_factor(&state, rules.body.efficiency));
        let v = [
            state.velocity[0] * factor,
            state.velocity[1],
            state.velocity[2] * factor,
        ];
        let gravity = rules.gravity(v[1]);
        let falling = v[1] <= 0.0;
        state.velocity = if fluid.water > 0.0 {
            let (slow, _) = water_drag(rules, was_on_ground, sprint);
            let slow = f64::from(slow);
            sink(
                gravity,
                falling,
                sprint,
                [v[0] * slow, v[1] * WATER_VERTICAL_DRAG, v[2] * slow],
            )
        } else if fluid.lava > 0.0 {
            let mut v = if fluid.lava <= FLUID_JUMP_THRESHOLD {
                sink(
                    gravity,
                    falling,
                    sprint,
                    [v[0] * 0.5, v[1] * WATER_VERTICAL_DRAG, v[2] * 0.5],
                )
            } else {
                [v[0] * 0.5, v[1] * 0.5, v[2] * 0.5]
            };
            v[1] -= gravity / 4.0;
            v
        } else if rules.gliding {
            v
        } else {
            let air = modified_friction(0.91, rules.body.air_drag_modifier);
            let friction = f64::from(block_friction * air);
            let y = rules.pull(state.velocity[1]);
            [
                v[0] * friction,
                y * f64::from(modified_friction(0.98, rules.body.air_drag_modifier)),
                v[2] * friction,
            ]
        };
        state.stuck = world.stuck(from, to, pose, rules.weaving);
        state.in_snow = in_snow;
        state
    }
}

/// A one-tick move that the entities touching the player pushed: the
/// replay whose end is off only by a push they could give, ticked again
/// with that push added before the tick.
fn pushed_end(
    ends: &[(State, Input, State)],
    bounds: [[f64; 2]; 2],
    to: [f64; 3],
    rules: &Rules,
    world: &World,
    marks: &mut Marks,
) -> Option<State> {
    let within = |v: f64, range: [f64; 2]| v >= range[0] - MATCH && v <= range[1] + MATCH;
    let mut candidates: Vec<(f64, &(State, Input, State))> = ends
        .iter()
        .filter_map(|entry| {
            let end = entry.2.position;
            let r = [to[0] - end[0], to[2] - end[2]];
            (within(r[0], bounds[0]) && within(r[1], bounds[1])).then(|| (r[0].hypot(r[1]), entry))
        })
        .collect();
    candidates.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (_, (start, input, end)) in candidates {
        let r = [to[0] - end.position[0], to[2] - end.position[2]];
        let pushed = State {
            velocity: [
                start.velocity[0] + r[0],
                start.velocity[1],
                start.velocity[2] + r[1],
            ],
            ..*start
        };
        let mut tried = Marks::default();
        let end = tick(pushed, input, rules, world, &mut tried);
        let error = [0, 1, 2]
            .map(|a| (end.position[a] - to[a]).abs())
            .into_iter()
            .fold(0.0, f64::max);
        if error <= MATCH {
            *marks = tried;
            return Some(end);
        }
    }
    None
}

/// Replays `count` unreported ticks of `input` from tick `first`; each
/// must have stayed under the client's reporting threshold.
fn silent_ticks(
    start: State,
    mut state: State,
    mut marks: Marks,
    first: u32,
    count: u32,
    input: &Input,
    replay: &Replay,
) -> Option<(State, Marks)> {
    for n in 0..count {
        let before = state;
        state = replay.step(state, first + n, input, &mut marks);
        if input.shift && before.on_ground && !silent(&start, &state) {
            state.position = before.position;
            marks.edge = true;
        }
        if !silent(&start, &state) {
            return None;
        }
    }
    Some((state, marks))
}

/// Replays the reported tick `t` from `state`; true when it ends at `to`.
#[allow(clippy::too_many_arguments)]
fn evaluate(
    search: &mut Search,
    start: State,
    state: State,
    mut marks: Marks,
    input: &Input,
    t: u32,
    to: [f64; 3],
    replay: &Replay,
) -> bool {
    let prepared = replay.prepare(state, t);
    let end = tick(prepared, input, &replay.rules, replay.world, &mut marks);
    search.tried += 1;
    if t == 0
        && let Some(ends) = search.ends.as_mut()
    {
        ends.push((prepared, *input, end));
    }
    let d = [0, 1, 2].map(|a| end.position[a] - start.position[a]);
    if (d[1] - search.rise).abs() <= MATCH {
        search.level.push([d[0], d[2]]);
    }
    if !marks.jumped {
        search.walked.push([d[0], d[2]]);
    }
    search.highest = search.highest.max(d[1]);
    let error = [0, 1, 2]
        .map(|a| (end.position[a] - to[a]).abs())
        .into_iter()
        .fold(0.0, f64::max);
    if search.closest.is_none_or(|(e, _)| error < e) {
        search.closest = Some((error, end));
    }
    if error <= MATCH {
        search.found = Some((end, marks, t));
        search.last = Some((prepared, *input));
        true
    } else {
        false
    }
}

/// The closest replay's state carried to the reported position. A move
/// held up where the replay fell stood on something the client's blocks
/// do not show (an entity, a block placed a moment ago), so it stops
/// like a landing; one that fell lower than the replay keeps the fall it
/// showed. The horizontal momentum stays the replay's, so a move faster
/// than every replay cannot raise the next bound.
fn carry(
    closest: State,
    from: [f64; 3],
    to: [f64; 3],
    claimed_ground: bool,
    rules: &Rules,
    world: &World,
) -> State {
    let mut state = State {
        position: to,
        ..closest
    };
    let rise = to[1] - from[1];
    let replayed = closest.position[1] - from[1];
    let drag = f64::from(modified_friction(0.98, rules.body.air_drag_modifier));
    if rise > replayed + MATCH && rise <= 1.0e-9 {
        state.velocity[1] = -rules.gravity(0.0) * drag;
        state.on_ground = claimed_ground;
        state.supporting = if claimed_ground {
            physics::supporting_block(bounds(to), &|pos| world.block(pos))
        } else {
            None
        };
        state.fall = 0.0;
    } else if rise < replayed - MATCH {
        state.velocity[1] = (rise - rules.gravity(rise)) * drag;
        state.on_ground = false;
        state.supporting = None;
    }
    state
}

/// Other starts a seeded state may have had: the observed move hit
/// something vertically (a step, a ceiling, a landing) or horizontally,
/// or, when it started in a fluid, lifted the body out over a ledge, or,
/// on a climbable, climbed.
fn seed_variants(state: &State, rules: &Rules, wet: bool, climbing: bool) -> Vec<State> {
    let drag = f64::from(modified_friction(0.98, rules.body.air_drag_modifier));
    let collided = -rules.gravity(0.0) * drag;
    let mut variants = vec![
        State {
            velocity: [state.velocity[0], collided, state.velocity[2]],
            ..*state
        },
        State {
            velocity: [0.0, state.velocity[1], 0.0],
            ..*state
        },
        State {
            on_ground: !state.on_ground,
            ..*state
        },
        State {
            velocity: [state.velocity[0], collided, state.velocity[2]],
            on_ground: !state.on_ground,
            ..*state
        },
    ];
    if climbing {
        variants.push(State {
            velocity: [
                state.velocity[0],
                rules.pull(CLIMB_SPEED) * drag,
                state.velocity[2],
            ],
            ..*state
        });
    }
    if wet {
        variants.push(State {
            velocity: [state.velocity[0], FLUID_EXIT_LIFT, state.velocity[2]],
            ..*state
        });
        variants.push(State {
            swimming: !state.swimming,
            pose: if state.swimming {
                Pose::Standing
            } else {
                Pose::Prone
            },
            ..*state
        });
    }
    variants
}

/// An unreported tick stayed under the client's reporting threshold.
fn silent(start: &State, now: &State) -> bool {
    let d = [0, 1, 2].map(|a| now.position[a] - start.position[a]);
    length_sq(d) <= SILENT_SQ
}

/// Flags a move the engine cannot explain that goes farther or higher
/// than any replay. The setback is the last accepted position.
#[derive(Default)]
pub struct Predicted {
    trace: Option<Trace>,
}

impl Check for Predicted {
    fn trace(&self) -> Option<Trace> {
        self.trace
    }

    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        self.trace = None;
        if !matches!(packet, Serverbound::Move(m) if m.position.is_some()) {
            return None;
        }
        let prediction = facts.prediction?;
        self.trace = Trace::new(
            "predict",
            prediction.allowed,
            prediction.actual,
            prediction.tags,
        );
        if !prediction.excessive() {
            return None;
        }
        let excess = prediction.horizontal.max(prediction.vertical * 2.0);
        Some(Flag {
            check: "predict",
            weight: 1.0 + (excess * 10.0).min(3.0),
            fix: Fix::Teleport(facts.position),
        })
    }
}
