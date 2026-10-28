//! `/spreadplayers <center> <spreadDistance> <maxRange> [under <maxHeight>]
//! <respectTeams> <targets>`: scatters targets over a square, at least
//! `spreadDistance` apart where it can, onto solid ground below the height.

use std::collections::{HashMap, HashSet};

use rand::{Rng, SeedableRng};
use tuff_core::command::args::Value;
use tuff_core::command::{CommandError, java_fixed, java_float};
use tuff_core::world::dimension::DimensionId;
use tuff_core::world::messages::{Directive, Teleport};
use tuff_core::world::scoreboard::Member;
use tuff_item::text::TextComponent;
use tuff_vanilla_data::BlockState;

use super::feedback::Messages;
use super::targets::{self, Target};
use super::{Context, Executor, executor_future, text, world};

const MAX_ITERATIONS: u32 = 10_000;

pub struct SpreadPlayers;

#[derive(Clone, Copy, Default)]
struct Spot {
    x: f64,
    z: f64,
}

impl Spot {
    fn dist(self, other: Spot) -> f64 {
        ((self.x - other.x).powi(2) + (self.z - other.z).powi(2)).sqrt()
    }

    fn randomize(&mut self, rng: &mut impl Rng, min: [f64; 2], max: [f64; 2]) {
        self.x = rng.gen_range(min[0]..=max[0]);
        self.z = rng.gen_range(min[1]..=max[1]);
    }

    fn clamp(&mut self, min: [f64; 2], max: [f64; 2]) -> bool {
        let (x, z) = (self.x.clamp(min[0], max[0]), self.z.clamp(min[1], max[1]));
        let changed = x != self.x || z != self.z;
        (self.x, self.z) = (x, z);
        changed
    }
}

/// Block columns read on demand, from the top of the world down.
struct Columns {
    dimension: DimensionId,
    min_y: i32,
    max_height: i32,
    columns: HashMap<(i32, i32), Vec<Option<BlockState>>>,
}

impl Columns {
    async fn load(&mut self, ctx: &mut Context<'_>, spot: Spot) -> Result<(), CommandError> {
        let key = (spot.x.floor() as i32, spot.z.floor() as i32);
        if self.columns.contains_key(&key) {
            return Ok(());
        }
        let top = self.max_height + 1;
        let positions: Vec<[i32; 3]> = (self.min_y..=top)
            .rev()
            .map(|y| [key.0, y, key.1])
            .collect();
        let states = world::read_blocks(ctx, self.dimension, &positions, true).await?;
        self.columns.insert(key, states);
        Ok(())
    }

    fn state(&self, x: i32, y: i32, z: i32) -> Option<BlockState> {
        let column = self.columns.get(&(x, z))?;
        let index = usize::try_from(self.max_height + 1 - y).ok()?;
        column.get(index).copied().flatten()
    }

    fn air(&self, x: i32, y: i32, z: i32) -> bool {
        self.state(x, y, z)
            .is_none_or(tuff_world_model::chunk::is_air)
    }

    fn spawn_y(&self, spot: Spot) -> i32 {
        let (x, z) = (spot.x.floor() as i32, spot.z.floor() as i32);
        let mut y = self.max_height + 1;
        let mut air_two_above = self.air(x, y, z);
        y -= 1;
        let mut air_above = self.air(x, y, z);
        while y > self.min_y {
            y -= 1;
            let current = self.air(x, y, z);
            if !current && air_above && air_two_above {
                return y + 1;
            }
            air_two_above = air_above;
            air_above = current;
        }
        self.max_height + 1
    }

    fn safe(&self, spot: Spot) -> bool {
        let (x, z) = (spot.x.floor() as i32, spot.z.floor() as i32);
        let y = self.spawn_y(spot) - 1;
        let Some(state) = self.state(x, y, z) else {
            return false;
        };
        y < self.max_height
            && !state.is_liquid()
            && tuff_datapack::tags::is(
                tuff_vanilla_data::tag::block::key::ENTITIES_CAN_TELEPORT_TO,
                state.block(),
            )
    }
}

fn team_of(ctx: &Context<'_>, target: &Target) -> Option<String> {
    match target {
        Target::Player(player) => ctx
            .shared()
            .scoreboard
            .teams()
            .team_of(Member::Player(&player.name))
            .map(|team| team.name.clone()),
        Target::Entity(_) => None,
    }
}

fn float(ctx: &Context<'_>, name: &str) -> f64 {
    match ctx.arg(name) {
        Some(Value::Float(v)) => f64::from(*v),
        _ => 0.0,
    }
}

impl Executor for SpreadPlayers {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let pose = ctx.source.pose();
            let center = match ctx.arg("center") {
                Some(Value::Coordinates(c)) => {
                    let p = c.position(&pose);
                    [p[0], p[2]]
                }
                _ => return Err(CommandError::simple("command.unknown.argument")),
            };
            let (spread, range) = (float(ctx, "spreadDistance"), float(ctx, "maxRange"));
            let respect_teams = matches!(ctx.arg("respectTeams"), Some(Value::Bool(true)));
            let dimension = ctx.source.dimension;
            let (min_y, top_y) = {
                let shape = &ctx.shared().dimension(dimension).shape;
                (shape.min_y, shape.max_y())
            };
            let max_height = match ctx.arg("maxHeight") {
                Some(Value::Int(h)) => *h,
                _ => top_y + 1,
            };
            if max_height < min_y {
                return Err(CommandError::text(TextComponent::translate(
                    "commands.spreadplayers.failed.invalid.height",
                    [text(max_height), text(min_y)],
                )));
            }
            let Some(Value::Selector(selector)) = ctx.arg("targets").cloned() else {
                return Err(CommandError::simple("argument.entity.notfound.entity"));
            };
            let found = targets::select_entities(ctx, &selector).await?;
            let teams: Vec<Option<String>> = found.iter().map(|t| team_of(ctx, t)).collect();
            let count = if respect_teams {
                teams.iter().collect::<HashSet<_>>().len()
            } else {
                found.len()
            };
            let min = [center[0] - range, center[1] - range];
            let max = [center[0] + range, center[1] + range];
            let seed: u64 = ctx.rng().r#gen();
            let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
            let mut spots = vec![Spot::default(); count];
            for spot in &mut spots {
                spot.randomize(&mut rng, min, max);
            }
            let mut columns = Columns {
                dimension,
                min_y,
                max_height,
                columns: HashMap::new(),
            };
            let mut min_distance = f64::MAX;
            let mut iterations = 0;
            let mut collisions = true;
            while iterations < MAX_ITERATIONS && collisions {
                collisions = false;
                min_distance = f64::MAX;
                for i in 0..spots.len() {
                    let mut neighbours = 0;
                    let mut away = Spot::default();
                    for j in 0..spots.len() {
                        if i == j {
                            continue;
                        }
                        let dist = spots[i].dist(spots[j]);
                        min_distance = min_distance.min(dist);
                        if dist < spread {
                            neighbours += 1;
                            away.x += spots[j].x - spots[i].x;
                            away.z += spots[j].z - spots[i].z;
                        }
                    }
                    if neighbours > 0 {
                        away.x /= f64::from(neighbours);
                        away.z /= f64::from(neighbours);
                        let length = (away.x * away.x + away.z * away.z).sqrt();
                        if length > 0.0 {
                            spots[i].x -= away.x / length;
                            spots[i].z -= away.z / length;
                        } else {
                            spots[i].randomize(&mut rng, min, max);
                        }
                        collisions = true;
                    }
                    if spots[i].clamp(min, max) {
                        collisions = true;
                    }
                }
                if !collisions {
                    for spot in &mut spots {
                        columns.load(ctx, *spot).await?;
                        if !columns.safe(*spot) {
                            spot.randomize(&mut rng, min, max);
                            collisions = true;
                        }
                    }
                }
                iterations += 1;
            }
            let describe = |distance: f64| {
                [
                    text(spots.len()),
                    text(java_float(center[0] as f32)),
                    text(java_float(center[1] as f32)),
                    text(java_fixed(distance, 2)),
                ]
            };
            if iterations >= MAX_ITERATIONS {
                let key = if respect_teams {
                    "commands.spreadplayers.failed.teams"
                } else {
                    "commands.spreadplayers.failed.entities"
                };
                let distance = if min_distance == f64::MAX {
                    0.0
                } else {
                    min_distance
                };
                return Err(CommandError::text(TextComponent::translate(
                    key,
                    describe(distance),
                )));
            }

            let mut assigned: HashMap<Option<String>, usize> = HashMap::new();
            let mut next = 0;
            let mut average = 0.0;
            let mut player_spots = HashMap::new();
            for (target, team) in found.iter().zip(&teams) {
                let index = if respect_teams {
                    *assigned.entry(team.clone()).or_insert_with(|| {
                        next += 1;
                        next - 1
                    })
                } else {
                    next += 1;
                    next - 1
                };
                let spot = spots[index];
                columns.load(ctx, spot).await?;
                let teleport = Teleport {
                    dimension,
                    position: [
                        spot.x.floor() + 0.5,
                        f64::from(columns.spawn_y(spot)),
                        spot.z.floor() + 0.5,
                    ],
                    relative: [false; 3],
                    rotation: None,
                    relative_rotation: [false; 2],
                    look_at: None,
                };
                match target {
                    Target::Player(player) => {
                        player_spots.insert(player.id, teleport);
                    }
                    Target::Entity(_) => {
                        super::entity_teleport::teleport_entities(ctx, &[target], &teleport).await;
                    }
                }
                average += spots
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i != index)
                    .map(|(_, other)| spot.dist(*other))
                    .fold(f64::MAX, f64::min);
            }
            if !player_spots.is_empty() {
                let snapshots: Vec<_> = found
                    .iter()
                    .filter_map(|t| match t {
                        Target::Player(p) => Some(p.clone()),
                        Target::Entity(_) => None,
                    })
                    .collect();
                targets::dispatch(
                    ctx,
                    &snapshots,
                    |player| Directive::Teleport(player_spots[&player.id].clone()),
                    Messages::new(|_| TextComponent::new(""), |_, _| TextComponent::new(""))
                        .silent(),
                )
                .await;
            }
            let distance = if found.len() < 2 {
                0.0
            } else {
                average / found.len() as f64
            };
            let key = if respect_teams {
                "commands.spreadplayers.success.teams"
            } else {
                "commands.spreadplayers.success.entities"
            };
            ctx.success(TextComponent::translate(key, describe(distance)), true)
                .await;
            Ok(spots.len() as i32)
        })
    }
}
