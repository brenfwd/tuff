use std::time::Instant;

use tuff_core::protocol::play::{GameMode, PlayerActionStatus, Serverbound};
use tuff_core::world::physics::Aabb;
use tuff_core::world::session::PlayerSession;
use tuff_core::world::session::mirror::ChunkMirror;
use tuff_vanilla_data::registry::{Attribute, EntityType};
use tuff_world_model::BlockPos;

use super::sight;
use super::{Check, Facts, Fix, Flag};
use crate::history::{self, PositionHistory};

const CREATIVE_BLOCK_BONUS: f64 = 0.5;
const CREATIVE_ENTITY_BONUS: f64 = 2.0;
const DRAGON_PARTS: i32 = 8;
const DRAGON_SLACK: f64 = 12.0;
const BLOCK_SLACK: f64 = 0.5;
const ENTITY_SLACK: f64 = 0.3;
const INTERPOLATION_TICKS: f64 = 3.0;
const TARGET_STEP: f64 = 0.2;
const MAX_LAG_TICKS: f64 = 10.0;
const STRIKE_DECAY: f64 = 0.02;
const STRIKE_TIGHTEN: f64 = 0.25;
const MIN_SLACK_SCALE: f64 = 0.5;
const LAG_WINDOW: u64 = 20;
const LAG_RATIO: f64 = 0.7;
const SIGHT_PAIRS: usize = 3;

/// What the player aims an attack, interaction, break or placement at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Aim {
    Entity,
    /// An ender dragon or one of its parts, whose hitboxes sit far from the
    /// position the client is told.
    Dragon,
    Block,
}

/// The geometry of one targeting packet, measured on the client's world.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reach {
    pub aim: Aim,
    /// From the eyes to the nearest point of the target's real shape.
    pub distance: f64,
    /// The player's interaction range for this target.
    pub range: f64,
    /// Every sampled line from the eyes to the target passes through a
    /// block; `None` when part of a line lies in an unsent chunk.
    pub occluded: Option<bool>,
}

impl Reach {
    pub fn probe(
        session: &PlayerSession,
        packet: &Serverbound,
        history: &PositionHistory,
    ) -> Option<Reach> {
        let eye = session.eye();
        let creative = session.mode.game_mode == GameMode::Creative;
        let world = session.client_world();
        match packet {
            Serverbound::Attack(entity) => {
                let weapon = session
                    .inventory
                    .hand(false)
                    .and_then(|stack| stack.item.attack_range())
                    .map(|range| {
                        let (_, max) = range.reach(creative);
                        f64::from(max) + f64::from(range.hitbox_margin)
                    });
                let range = entity_range(session, creative).max(weapon.unwrap_or(0.0));
                Self::entity(session, world, history, *entity, None, range)
            }
            Serverbound::Interact {
                entity, location, ..
            } => {
                let range = entity_range(session, creative);
                Self::entity(session, world, history, *entity, Some(*location), range)
            }
            Serverbound::UseItemOn {
                position, cursor, ..
            } => {
                let hit = [
                    f64::from(position.x) + f64::from(cursor[0]),
                    f64::from(position.y) + f64::from(cursor[1]),
                    f64::from(position.z) + f64::from(cursor[2]),
                ];
                Self::block(
                    session,
                    world,
                    eye,
                    *position,
                    (Some(hit), None),
                    creative,
                    true,
                )
            }
            Serverbound::PlayerAction {
                status,
                position,
                face,
                ..
            } => {
                let sight = match status {
                    PlayerActionStatus::StartDestroyBlock => true,
                    PlayerActionStatus::StopDestroyBlock => false,
                    _ => return None,
                };
                Self::block(
                    session,
                    world,
                    eye,
                    *position,
                    (None, Some(*face)),
                    creative,
                    sight,
                )
            }
            _ => None,
        }
    }

    fn entity(
        session: &PlayerSession,
        world: &ChunkMirror,
        history: &PositionHistory,
        entity: i32,
        location: Option<[f64; 3]>,
        range: f64,
    ) -> Option<Reach> {
        let eye = session.eye();
        let (kind, position) = match session.view.entities.visible_player(entity) {
            Some((position, _)) => (EntityType::Player, position),
            None => match session.view.entities.visible_entity(entity) {
                Some((kind, _, position)) => (kind, position),
                None => {
                    let owner = (1..=DRAGON_PARTS)
                        .filter_map(|offset| session.view.entities.visible_entity(entity - offset))
                        .find(|(kind, _, _)| *kind == EntityType::EnderDragon)?;
                    (owner.0, owner.2)
                }
            },
        };
        let size = kind.size();
        let half = f64::from(size.width) / 2.0;
        let bounds = |position: [f64; 3]| {
            [
                position[0] - half,
                position[1],
                position[2] - half,
                position[0] + half,
                position[1] + f64::from(size.height),
                position[2] + half,
            ]
        };
        let window = history::window(session.net.latency_ms);
        let eyes: Vec<[f64; 3]> = std::iter::once(eye)
            .chain(history.eyes(session.tick, window))
            .collect();
        let targets: Vec<[f64; 3]> = std::iter::once(position)
            .chain(history.target(entity, session.tick, window))
            .collect();
        let mut pairs: Vec<(f64, [f64; 3], [f64; 3])> = eyes
            .iter()
            .flat_map(|e| {
                targets
                    .iter()
                    .map(move |t| (sight::distance(*e, bounds(*t)), *e, *t))
            })
            .collect();
        let current = pairs[0];
        pairs.sort_by(|a, b| a.0.total_cmp(&b.0));
        let distance = pairs[0].0;
        if kind == EntityType::EnderDragon {
            return Some(Reach {
                aim: Aim::Dragon,
                distance,
                range,
                occluded: Some(false),
            });
        }
        let mut occluded = Some(true);
        for (_, eye, position) in pairs.iter().take(SIGHT_PAIRS).chain([&current]) {
            let b = bounds(*position);
            let mut samples = vec![
                sight::nearest(*eye, b),
                centre(b),
                [
                    position[0],
                    position[1] + f64::from(size.eye_height),
                    position[2],
                ],
            ];
            if let Some(location) = location {
                samples.push([
                    position[0] + location[0],
                    position[1] + location[1],
                    position[2] + location[2],
                ]);
            }
            match sight::all_blocked(world, *eye, &samples, None) {
                Some(false) => {
                    occluded = Some(false);
                    break;
                }
                None => occluded = None,
                Some(true) => {}
            }
        }
        Some(Reach {
            aim: Aim::Entity,
            distance,
            range,
            occluded,
        })
    }

    fn block(
        session: &PlayerSession,
        world: &ChunkMirror,
        eye: [f64; 3],
        position: BlockPos,
        (hit, face): (Option<[f64; 3]>, Option<i8>),
        creative: bool,
        check_sight: bool,
    ) -> Option<Reach> {
        let state = world.block_at(position)?;
        let boxes = sight::outline(state, position);
        let distance = boxes
            .iter()
            .map(|b| sight::distance(eye, *b))
            .fold(f64::INFINITY, f64::min);
        let occluded = if !check_sight {
            Some(false)
        } else {
            let samples: Vec<[f64; 3]> = match (hit, face) {
                (Some(hit), _) => vec![hit],
                (None, Some(face)) if (0..6).contains(&face) => boxes
                    .iter()
                    .flat_map(|b| sight::face_points(*b, face))
                    .collect(),
                _ => boxes
                    .iter()
                    .flat_map(|b| [sight::nearest(eye, *b), centre(*b)])
                    .collect(),
            };
            sight::all_blocked(world, eye, &samples, Some(position))
        };
        let mut range = session.attribute(Attribute::BlockInteractionRange);
        if creative {
            range += CREATIVE_BLOCK_BONUS;
        }
        Some(Reach {
            aim: Aim::Block,
            distance,
            range,
            occluded,
        })
    }
}

fn entity_range(session: &PlayerSession, creative: bool) -> f64 {
    let range = session.attribute(Attribute::EntityInteractionRange);
    if creative {
        range + CREATIVE_ENTITY_BONUS
    } else {
        range
    }
}

fn centre(b: Aabb) -> [f64; 3] {
    [
        (b[0] + b[3]) / 2.0,
        (b[1] + b[4]) / 2.0,
        (b[2] + b[5]) / 2.0,
    ]
}

/// Hitting or using something farther than the player's interaction range
/// allows, or through a wall. Entity targets get slack for how far they
/// may have moved while the client showed an older position; repeat
/// offenders lose part of that slack, and nothing is flagged while the
/// server runs far behind its tick rate.
#[derive(Default)]
pub struct ReachCheck {
    strikes: f64,
    last_tick: u64,
    window: Option<(u64, Instant)>,
    lagging: bool,
}

impl ReachCheck {
    fn observe_lag(&mut self, facts: &Facts) {
        let (start, at) = *self.window.get_or_insert((facts.tick, facts.now));
        let ticks = facts.tick.saturating_sub(start);
        if ticks < LAG_WINDOW {
            let expected = facts.now.duration_since(at).as_secs_f64()
                * f64::from(facts.tick_rate.clamp(1.0, 20.0));
            if expected > 2.0 * LAG_WINDOW as f64 {
                self.lagging = true;
                self.window = Some((facts.tick, facts.now));
            }
            return;
        }
        let expected = facts.now.duration_since(at).as_secs_f64()
            * f64::from(facts.tick_rate.clamp(1.0, 20.0));
        self.lagging = expected > 0.0 && (ticks as f64) < LAG_RATIO * expected;
        self.window = Some((facts.tick, facts.now));
    }

    fn slack(&self, facts: &Facts, aim: Aim) -> f64 {
        let scale = (1.0 / (1.0 + STRIKE_TIGHTEN * self.strikes)).max(MIN_SLACK_SCALE);
        match aim {
            Aim::Block => BLOCK_SLACK * scale,
            Aim::Dragon => DRAGON_SLACK,
            Aim::Entity => {
                let ticks = (f64::from(facts.latency_ms.max(0)) / 50.0 + INTERPOLATION_TICKS)
                    .min(MAX_LAG_TICKS);
                (ENTITY_SLACK + TARGET_STEP * ticks) * scale
            }
        }
    }
}

impl Check for ReachCheck {
    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        let reach = facts.reach?;
        let elapsed = facts.tick.saturating_sub(self.last_tick) as f64;
        self.last_tick = facts.tick;
        self.strikes = (self.strikes - elapsed * STRIKE_DECAY).max(0.0);
        self.observe_lag(facts);
        if self.lagging
            || facts.teleport_pending
            || facts.game_mode == GameMode::Spectator
            || !matches!(
                packet,
                Serverbound::Attack(_)
                    | Serverbound::Interact { .. }
                    | Serverbound::UseItemOn { .. }
                    | Serverbound::PlayerAction { .. }
            )
        {
            return None;
        }
        let excess = reach.distance - reach.range - self.slack(facts, reach.aim);
        if excess > 0.0 {
            self.strikes += 1.0;
            return Flag::cancel("reach", (1.0 + excess).min(5.0));
        }
        if reach.occluded == Some(true) {
            self.strikes += 1.0;
            if let Serverbound::PlayerAction {
                position, sequence, ..
            } = packet
            {
                return Some(Flag {
                    check: "sight",
                    weight: 1.0,
                    fix: Fix::Correct(Box::new(Serverbound::PlayerAction {
                        status: PlayerActionStatus::AbortDestroyBlock,
                        position: *position,
                        face: 0,
                        sequence: *sequence,
                    })),
                });
            }
            return Flag::cancel("sight", 1.0);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::super::facts;
    use super::*;

    fn with(aim: Aim, distance: f64, range: f64, occluded: bool) -> Facts {
        let mut f = facts();
        f.reach = Some(Reach {
            aim,
            distance,
            range,
            occluded: Some(occluded),
        });
        f
    }

    #[tuff_core::watchdog]
    #[test]
    fn entity_reach_allows_the_interpolation_window_only() {
        let mut check = ReachCheck::default();
        let attack = Serverbound::Attack(1);
        assert_eq!(
            check.examine(&with(Aim::Entity, 3.8, 3.0, false), &attack),
            None
        );
        let flag = check.examine(&with(Aim::Entity, 4.5, 3.0, false), &attack);
        assert_eq!(flag.unwrap().check, "reach");
        let mut lagged = with(Aim::Entity, 4.5, 3.0, false);
        lagged.latency_ms = 300;
        assert_eq!(ReachCheck::default().examine(&lagged, &attack), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn repeat_offenders_lose_slack() {
        let mut check = ReachCheck::default();
        let attack = Serverbound::Attack(1);
        let mut f = with(Aim::Entity, 3.85, 3.0, false);
        assert_eq!(check.examine(&f, &attack), None);
        check.strikes = 4.0;
        assert!(check.examine(&f, &attack).is_some());
        f.tick = 10_000;
        assert_eq!(check.examine(&f, &attack), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn blocks_use_a_tight_limit_and_dragons_a_wide_one() {
        let place = Serverbound::PlayerAction {
            status: PlayerActionStatus::StartDestroyBlock,
            position: BlockPos::new(0, 0, 0),
            face: 1,
            sequence: 0,
        };
        assert_eq!(
            ReachCheck::default().examine(&with(Aim::Block, 4.9, 4.5, false), &place),
            None
        );
        assert!(
            ReachCheck::default()
                .examine(&with(Aim::Block, 5.2, 4.5, false), &place)
                .is_some()
        );
        assert_eq!(
            ReachCheck::default().examine(
                &with(Aim::Dragon, 12.0, 3.0, false),
                &Serverbound::Attack(1)
            ),
            None
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn walls_and_lag_are_handled() {
        let attack = Serverbound::Attack(1);
        let flag = ReachCheck::default().examine(&with(Aim::Entity, 2.0, 3.0, true), &attack);
        assert_eq!(flag.unwrap().check, "sight");
        let mut check = ReachCheck::default();
        let mut f = with(Aim::Entity, 9.0, 3.0, false);
        let start = f.now;
        check.examine(&f, &attack);
        f.now = start + std::time::Duration::from_secs(5);
        f.tick = 30;
        assert_eq!(check.examine(&f, &attack), None);
    }
}
