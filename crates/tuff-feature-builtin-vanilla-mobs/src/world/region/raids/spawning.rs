use super::*;

pub(super) fn find_spawn_pos(
    world: &mut dyn RegionWorld,
    raid: &mut Raid,
    tries: i32,
) -> Option<BlockPos> {
    let seconds = raid.cooldown / 20;
    let how_far = 0.22f32 * seconds as f32 - 0.24;
    let start = world.rng().r#gen::<f32>() * std::f32::consts::TAU;
    let mut elsewhere = Vec::new();
    for i in 0..tries {
        let angle = start + std::f32::consts::PI * i as f32 / 8.0;
        let spread = how_far.floor() as i32;
        let x = raid.center.x
            + (angle.cos() * 32.0 * how_far).floor() as i32
            + world.rng().gen_range(0..3) * spread;
        let z = raid.center.z
            + (angle.sin() * 32.0 * how_far).floor() as i32
            + world.rng().gen_range(0..3) * spread;
        if !world.owns(BlockPos::new(x, raid.center.y, z)) {
            elsewhere.push((x, z));
            continue;
        }
        if let Some(pos) = check_spawn_pos(world, raid.center, seconds, x, z) {
            return Some(pos);
        }
    }
    if !elsewhere.is_empty() && raid.remote_search.is_none() {
        ask_spawn_pos(world, raid, seconds, elsewhere);
    }
    None
}

/// Whether a wave may spawn in the column at `x`, `z`, judged by the region
/// that owns it: the spot to stand on.
pub(super) fn check_spawn_pos(
    world: &dyn RegionWorld,
    center: BlockPos,
    seconds: i32,
    x: i32,
    z: i32,
) -> Option<BlockPos> {
    let y = world.world_surface(x, z)?;
    if (y - center.y).abs() > 96 {
        return None;
    }
    let pos = BlockPos::new(x, y, z);
    if (is_village(world, pos) && seconds > SPAWN_IN_VILLAGE_SECONDS) || !world.owns(pos) {
        return None;
    }
    let loaded = [(-10, -10), (-10, 10), (10, -10), (10, 10)]
        .iter()
        .all(|(dx, dz)| world.block(BlockPos::new(x + dx, y, z + dz)).is_some());
    if !loaded {
        return None;
    }
    let ravager_fits = crate::spawning::placement_ok(EntityType::Ravager, pos, &|p| world.block(p));
    let on_snow = world
        .block(BlockPos::new(x, y - 1, z))
        .is_some_and(|s| s.block() == Block::Snow)
        && world
            .block(pos)
            .is_some_and(tuff_world_model::chunk::is_air);
    (ravager_fits || on_snow).then_some(pos)
}

pub(super) fn owner_of(x: i32, z: i32) -> RegionPos {
    ChunkPos::of_block(BlockPos::new(x, 0, z)).region()
}

/// Asks the region owning the first of `columns` whether a wave can spawn
/// in any of that region's candidates, not only the first one; the answer lands in `raid.wave_spawn` a tick or two later.
pub(super) fn ask_spawn_pos(
    world: &mut dyn RegionWorld,
    raid: &mut Raid,
    seconds: i32,
    mut columns: Vec<(i32, i32)>,
) {
    let owner = owner_of(columns[0].0, columns[0].1);
    columns.retain(|&(x, z)| owner_of(x, z) == owner);
    let Some(handle) = world.neighbor(owner) else {
        return;
    };
    let (tx, rx) = oneshot::channel();
    let center = raid.center;
    handle.internal(RegionInternal::Run(RegionTask::new(move |w| {
        let found = columns
            .iter()
            .find_map(|&(x, z)| check_spawn_pos(w, center, seconds, x, z));
        let _ = tx.send(found);
    })));
    raid.remote_search = Some(rx);
}

pub(super) fn poll_remote_search(raid: &mut Raid) {
    let Some(rx) = raid.remote_search.as_mut() else {
        return;
    };
    match rx.try_recv() {
        Ok(found) => {
            raid.remote_search = None;
            if raid.wave_spawn.is_none() {
                raid.wave_spawn = found;
            }
        }
        Err(oneshot::error::TryRecvError::Empty) => {}
        Err(oneshot::error::TryRecvError::Closed) => raid.remote_search = None,
    }
}

/// Spawns the wave in the neighbour that owns `pos`, which hands its
/// raiders back to this raid.
pub(super) fn spawn_group_remotely(world: &mut dyn RegionWorld, raid: &mut Raid, pos: BlockPos) {
    let Some(handle) = world.neighbor(owner_of(pos.x, pos.z)) else {
        raid.wave_spawn = None;
        return;
    };
    let home = world.region_pos();
    let difficulty = *world.shared().difficulty.borrow();
    let (id, center, omen, num_groups, spawned) = (
        raid.id,
        raid.center,
        raid.omen_level,
        raid.num_groups,
        raid.groups_spawned,
    );
    handle.internal(RegionInternal::Run(RegionTask::new(move |w| {
        let mut wave = Raid::new(id, center, difficulty, 0);
        wave.omen_level = omen;
        wave.num_groups = num_groups;
        wave.groups_spawned = spawned;
        spawn_group(w, &mut wave, pos);
        let Raid {
            members,
            leaders,
            total_health,
            ..
        } = wave;
        at_home(w, home, move |h| {
            with_raid(h, id, |raid, _| {
                raid.members.extend(members);
                raid.leaders.extend(leaders);
                raid.total_health = total_health;
                raid.pending_spawns = raid.pending_spawns.saturating_sub(1);
                raid.update_bar_progress();
            });
        });
    })));
    raid.total_health = 0.0;
    raid.wave_spawn = None;
    raid.groups_spawned += 1;
    raid.pending_spawns += 1;
    raid.spawn_deadline = world.tick() + REMOTE_QUERY_TICKS * 5;
}

pub(super) fn bonus_spawns(
    kind: EntityType,
    wave: i32,
    bonus_group: bool,
    difficulty: Difficulty,
    rng: &mut impl Rng,
) -> i32 {
    let easy = difficulty == Difficulty::Easy;
    let normal = difficulty == Difficulty::Normal;
    let bonus = match kind {
        EntityType::Vindicator | EntityType::Pillager => {
            if easy {
                rng.gen_range(0..2)
            } else if normal {
                1
            } else {
                2
            }
        }
        EntityType::Witch if !easy && wave > 2 && wave != 4 => 1,
        EntityType::Ravager if !easy && bonus_group => 1,
        _ => 0,
    };
    if bonus > 0 {
        rng.gen_range(0..=bonus)
    } else {
        0
    }
}

pub(super) fn spawn_group(world: &mut dyn RegionWorld, raid: &mut Raid, pos: BlockPos) {
    let wave = raid.groups_spawned + 1;
    raid.total_health = 0.0;
    let difficulty = *world.shared().difficulty.borrow();
    let bonus_group = raid.should_spawn_bonus_group();
    let mut leader_set = false;
    for (kind, counts) in WAVES {
        let index = if bonus_group { raid.num_groups } else { wave };
        let base = counts.get(index as usize).copied().unwrap_or(0);
        let count = base + bonus_spawns(kind, wave, bonus_group, difficulty, world.rng());
        let mut ravagers = 0;
        for _ in 0..count {
            let Some(vehicle) = spawn_raider(world, raid, kind, wave, pos, &mut leader_set) else {
                break;
            };
            if kind == EntityType::Ravager {
                let rider = ravager_rider(wave, ravagers);
                ravagers += 1;
                if let Some(rider) = rider {
                    let mut no_leader = true;
                    if let Some(passenger) =
                        spawn_raider(world, raid, rider, wave, pos, &mut no_leader)
                    {
                        world.ride(RideOp::Mount {
                            passenger: RidePassenger::Entity(passenger),
                            vehicle,
                        });
                    }
                }
            }
        }
    }
    raid.wave_spawn = None;
    raid.groups_spawned += 1;
    raid.update_bar_progress();
}

/// Who rides the `ravagers`-th ravager of `wave`: a pillager in the last
/// normal wave, an evoker then vindicators in the hard-only waves.
pub(super) fn ravager_rider(wave: i32, ravagers: i32) -> Option<EntityType> {
    if wave == num_groups(Difficulty::Normal) {
        Some(EntityType::Pillager)
    } else if wave >= num_groups(Difficulty::Hard) {
        Some(if ravagers == 0 {
            EntityType::Evoker
        } else {
            EntityType::Vindicator
        })
    } else {
        None
    }
}

/// Spawns one raider of the wave at `pos`; its entity id.
pub(super) fn spawn_raider(
    world: &mut dyn RegionWorld,
    raid: &mut Raid,
    kind: EntityType,
    wave: i32,
    pos: BlockPos,
    leader_set: &mut bool,
) -> Option<i32> {
    let at = [
        f64::from(pos.x) + 0.5,
        f64::from(pos.y) + 1.0,
        f64::from(pos.z) + 0.5,
    ];
    let id = world.shared().next_entity_id();
    let mut entity = world.shared().entity_types.create(kind, id, at, 0.0)?;
    let leader = {
        let core = raider::core_mut(entity.as_mut())?;
        let state = core.raider.as_mut()?;
        state.raid = Some(raid.id);
        state.wave = wave;
        state.can_join_raid = true;
        state.spawned_for_group = true;
        let leader = !*leader_set && state.can_be_leader;
        if leader {
            state.patrol_leader = true;
            state.patrolling = true;
        }
        leader
    };
    world.finalize_spawn(&mut entity, 0);
    let omen = raid.omen_level;
    let mut rng = rand::rngs::StdRng::seed_from_u64(world.rng().r#gen());
    let core = raider::core_mut(entity.as_mut())?;
    raider::apply_raid_buffs(core, wave, omen, &mut rng);
    if leader {
        raider::wear_banner(core);
        *leader_set = true;
    }
    if let Some(state) = core.raider.as_mut() {
        state.spawned_for_group = false;
    }
    core.base.on_ground = true;
    let uuid = core.base.uuid;
    let health = core.health;
    raid.members.insert(
        uuid,
        Member {
            wave,
            health,
            seen: world.tick(),
            position: at,
            no_action_time: 0,
            age: 0,
            outside: 0,
        },
    );
    raid.total_health += health;
    if leader {
        raid.leaders.insert(wave, uuid);
    }
    world.add_entity(entity);
    Some(id)
}
