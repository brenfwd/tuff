use std::future::Future;
use std::pin::Pin;

use tuff_core::protocol::play::{self, GameMode, Movement, Serverbound};
use tuff_core::world::game_rules::{GameRules, RuleValue};
use tuff_core::world::messages::CoordinatorMessage;
use tuff_core::world::player::UsingItem;
use tuff_core::world::session::PlayerSession;
use tuff_core::world::session::hooks::{PacketInterceptor, Verdict};
use tuff_item::text::TextComponent;
use tuff_storage::admin::PermissionLevel;

use std::sync::Arc;

use tokio::sync::watch;
use tuff_core::world::physics;
use tuff_vanilla_data::Item;
use tuff_vanilla_data::item::EquipmentSlot;
use tuff_vanilla_data::registry::{Attribute, EntityType, MobEffect};
use tuff_world_model::BlockPos;

use crate::anchor::{self, Anchors};
use crate::checks::velocity::VelocityLedger;
use crate::checks::{self, Check, Engine, Facts, Fix, Flag, Placement, Terrain, Vehicle};
use crate::config::{self, Rules};
use crate::history::PositionHistory;
use crate::improbable::{self, Improbable};
use crate::permissions::{self, Exemptions};
use crate::service::{self, Board, Debugged, Reset};
use crate::trace::Trace;
use crate::violations::{self, AirClock, Ladder, Step};

const MAX_LADDERS: usize = 64;

/// Per-player state: the checks, their violation levels and the cached
/// settings.
struct Player {
    checks: Vec<Box<dyn Check>>,
    ladders: Vec<(&'static str, Ladder)>,
    alerted: Vec<(&'static str, u64)>,
    settings: Option<(u64, Arc<Rules>)>,
    link: Option<Link>,
    was_pending: bool,
    grace_until: Option<u64>,
    vehicle_setback: Option<VehicleSetback>,
    anchors: Anchors,
    velocity: VelocityLedger,
    engine: Engine,
    history: PositionHistory,
    improbable: Improbable,
    air: AirClock,
    /// Traced checks while staff debug this player: `Some(None)` is all.
    debug: Option<Option<&'static str>>,
    flag_trace: Option<Trace>,
}

/// A vehicle put back and not yet confirmed: the client answers a
/// Move Vehicle by sending the same position back, and every vehicle
/// move it sent before seeing the setback is stale.
struct VehicleSetback {
    vehicle: i32,
    position: [f64; 3],
    rotation: (f32, f32),
    check: &'static str,
    unconfirmed: u32,
    /// The session tick of the last vehicle move.
    heard: u64,
}

impl Default for Player {
    fn default() -> Self {
        Self {
            checks: checks::defaults(),
            ladders: Vec::new(),
            alerted: Vec::new(),
            settings: None,
            link: None,
            was_pending: false,
            grace_until: None,
            vehicle_setback: None,
            anchors: Anchors::default(),
            velocity: VelocityLedger::default(),
            engine: Engine::default(),
            history: PositionHistory::default(),
            improbable: Improbable::default(),
            air: AirClock::default(),
            debug: None,
            flag_trace: None,
        }
    }
}

/// This player's view of the world's board: their name for events and
/// the resets they have applied.
struct Link {
    name: Arc<str>,
    resets: Option<watch::Receiver<Arc<Vec<Reset>>>>,
    seen: u64,
    debugged: Option<watch::Receiver<Debugged>>,
}

impl Player {
    fn settings(&mut self, session: &PlayerSession) -> Arc<Rules> {
        let stale = self
            .settings
            .as_ref()
            .is_none_or(|(at, _)| session.tick.saturating_sub(*at) >= config::REFRESH_TICKS);
        if stale {
            let rules = Rules {
                settings: config::Settings::read(&session.shared.config),
                exempt: Exemptions::of(
                    &session.shared.permissions.view(),
                    session.profile.id,
                    session.mode.permission,
                ),
            };
            self.settings = Some((session.tick, Arc::new(rules)));
        }
        self.settings.as_ref().expect("just filled").1.clone()
    }

    /// Applies the resets staff made to this player's levels since the
    /// last packet.
    fn apply_resets(&mut self, session: &PlayerSession) {
        let link = self.link.get_or_insert_with(|| {
            let board = session.shared.services.get::<Board>();
            let mut resets = board.map(Board::resets);
            let seen = resets
                .as_mut()
                .and_then(|r| r.borrow_and_update().last().map(|r| r.serial))
                .unwrap_or(0);
            let mut debugged = board.map(Board::debugged);
            if let Some(receiver) = debugged.as_mut() {
                receiver.mark_changed();
            }
            Link {
                name: Arc::from(session.profile.name.as_str()),
                resets,
                seen,
                debugged,
            }
        });
        if let Some(receiver) = link.debugged.as_mut()
            && receiver.has_changed().unwrap_or(false)
        {
            let name = session.profile.name.to_ascii_lowercase();
            self.debug = receiver.borrow_and_update().get(&name).copied();
        }
        let Some(receiver) = link.resets.as_mut() else {
            return;
        };
        if !receiver.has_changed().unwrap_or(false) {
            return;
        }
        let resets = receiver.borrow_and_update().clone();
        let seen = link.seen;
        link.seen = resets.last().map_or(seen, |r| r.serial);
        for reset in resets.iter().filter(|r| r.serial > seen) {
            if reset.player != session.profile.id {
                continue;
            }
            match reset.check {
                Some(check) => self.ladders.retain(|(name, _)| *name != check),
                None => {
                    self.ladders.clear();
                    self.improbable = Improbable::default();
                }
            }
        }
    }

    /// Whether operators hear about this step: every kick, and a setback
    /// only once per check every [`ALERT_INTERVAL_TICKS`].
    fn should_alert(&mut self, check: &'static str, step: Step, tick: u64) -> bool {
        let slot = match self.alerted.iter().position(|(name, _)| *name == check) {
            Some(index) => &mut self.alerted[index].1,
            None => {
                if self.alerted.len() >= MAX_LADDERS {
                    self.alerted.remove(0);
                }
                self.alerted.push((check, tick));
                return true;
            }
        };
        if step == Step::Kick || tick.saturating_sub(*slot) >= ALERT_INTERVAL_TICKS {
            *slot = tick;
            true
        } else {
            false
        }
    }

    fn ladder(&mut self, check: &'static str) -> &mut Ladder {
        let index = match self.ladders.iter().position(|(name, _)| *name == check) {
            Some(index) => index,
            None => {
                if self.ladders.len() >= MAX_LADDERS {
                    self.ladders.remove(0);
                }
                self.ladders.push((check, Ladder::default()));
                self.ladders.len() - 1
            }
        };
        &mut self.ladders[index].1
    }
}

const ALERT_INTERVAL_TICKS: u64 = 20;
const ECHO_EPSILON: f64 = 1.0e-6;
/// Stale vehicle moves allowed after a setback, plus a round trip's worth.
const ECHO_PATIENCE: u32 = 10;
const RESEAT_EVERY: u32 = 20;
/// A driver's client sends a vehicle move every tick; one that went
/// quiet this long had stopped steering and never saw the setback.
const ECHO_SILENCE: u64 = 20;
/// Pushes the ledger does not see (pistons) exempt the movement checks
/// this long.
const PUSH_GRACE_TICKS: u64 = 10;
const TELEPORT_GRACE_TICKS: u64 = 60;
/// How far around a move entities are gathered for it.
const NEARBY_REACH: f64 = 3.0;
/// The ladder long airborne stretches without a clean descent climb.
const HOVER_CHECK: &str = "fly";

fn menu_distance(session: &PlayerSession) -> Option<f64> {
    let menu = session.menus.menu.as_ref()?;
    let eye = session.eye();
    menu.backing
        .positions()
        .iter()
        .map(|pos| {
            let near = |c: f64, b: i32| c.clamp(f64::from(b), f64::from(b) + 1.0);
            let dx = eye[0] - near(eye[0], pos.x);
            let dy = eye[1] - near(eye[1], pos.y);
            let dz = eye[2] - near(eye[2], pos.z);
            (dx * dx + dy * dy + dz * dz).sqrt()
        })
        .reduce(f64::max)
}

fn facts_of(
    session: &PlayerSession,
    packet: &Serverbound,
    grace: bool,
    confirming: bool,
    history: &PositionHistory,
    settings: &config::Settings,
) -> Facts {
    let step_height = f64::from(body_of(session).step_height);
    let terrain = match packet {
        Serverbound::Move(movement) => movement
            .position
            .filter(|p| p.iter().all(|c| c.is_finite()))
            .map(|new| {
                Terrain::probe(
                    session.client_world(),
                    session.pose.position,
                    new,
                    step_height,
                )
            }),
        _ => None,
    };
    let resting = match packet {
        Serverbound::Move(movement) if movement.position.is_none() && movement.on_ground => {
            let here = session.pose.position;
            here.iter()
                .all(|c| c.is_finite())
                .then(|| Terrain::probe(session.client_world(), here, here, step_height))
        }
        _ => None,
    };
    let recently_pushed = session
        .motion
        .pushed_at
        .is_some_and(|at| session.tick.saturating_sub(at) <= PUSH_GRACE_TICKS);
    let ledgered = session
        .motion
        .velocity_sent
        .recent
        .back()
        .is_some_and(|given| Some(given.tick) >= session.motion.pushed_at);
    let pushed =
        session.motion.glide.active || session.motion.spin_attack_ticks > 0 || recently_pushed;
    Facts {
        tick: session.tick,
        game_mode: session.mode.game_mode,
        instabuild: session.mode.abilities.instabuild,
        may_fly: session.mode.abilities.may_fly,
        sneaking: session.pose.sneaking,
        snow_walker: session
            .inventory
            .equipped(EquipmentSlot::Feet)
            .is_some_and(|stack| stack.item == Item::LeatherBoots),
        sprinting: session.pose.sprinting,
        food: session.food_level(),
        riding: session.ride.riding.is_some(),
        position: session.pose.position,
        on_ground: session.pose.on_ground,
        teleport_pending: session.sync.pending_teleport.is_some(),
        confirming,
        menu: session
            .menus
            .menu
            .as_ref()
            .map(|menu| (menu.window_id, menu.container.len())),
        now: crate::clock::now(session),
        latency_ms: session.net.latency_ms,
        tick_rate: session.shared.tick_state.borrow().rate,
        flying: session.mode.abilities.flying,
        fly_speed: session.mode.fly_speed,
        ceiling: f64::from(session.dimension().kind.min_y)
            + f64::from(session.dimension().kind.height),
        velocity: pushed,
        push: checks::velocity::Push::default(),
        impulses: Vec::new(),
        shoves: session.motion.shoves.numbered().collect(),
        gliding: session.motion.glide.active,
        firework: session.motion.firework_boost_ticks > 0,
        riptide: session.motion.spin_attack_ticks > 0,
        shoved: recently_pushed
            && !ledgered
            && session.motion.pushed_at != session.motion.piston_at,
        speed: session.body.effects.amplifier(MobEffect::Speed),
        jump_boost: session
            .body
            .effects
            .amplifier(MobEffect::JumpBoost)
            .map_or(0, |a| a.saturating_add(1)),
        levitation: session.body.effects.has(MobEffect::Levitation),
        levitation_level: session
            .body
            .effects
            .amplifier(MobEffect::Levitation)
            .map_or(0, |a| a.saturating_add(1)),
        slow_falling: session.body.effects.has(MobEffect::SlowFalling),
        weaving: session.body.effects.has(MobEffect::Weaving),
        dolphins_grace: session.body.effects.has(MobEffect::DolphinsGrace),
        fast_lava: tuff_vanilla_data::fluid::fast_lava(session.dimension().kind.name),
        terrain,
        resting,
        menu_distance: menu_distance(session),
        using: session.actions.using.is_some(),
        item_use: session.actions.using.map(|u| checks::ItemUse {
            item: u.item,
            elapsed: u.total - u.remaining,
            remaining: (u.total != UsingItem::HELD).then_some(u.remaining),
        }),
        may_edit_commands: session.mode.abilities.instabuild
            && session.mode.permission >= PermissionLevel::Gamemasters,
        grace,
        eye: session.eye(),
        target: match packet {
            Serverbound::PlayerAction { position, .. } => {
                session.client_world().block_at(*position)
            }
            _ => None,
        },
        reach: checks::Reach::probe(session, packet, history),
        placement: placement_of(session, packet),
        vehicle: Vehicle::probe(session, packet),
        click: checks::ClickBefore::probe(session, packet),
        rotation: [session.pose.yaw, session.pose.pitch],
        aim: checks::AimTarget::probe(session, packet, history, settings.aim),
        mining: checks::Mining::probe(session, packet, &settings.fast_break),
        echoes: 0,
        body: body_of(session),
        nearby: match packet {
            Serverbound::Move(Movement {
                position: Some(to), ..
            }) if to.iter().all(|c| c.is_finite()) => nearby(session, *to),
            _ => Vec::new(),
        },
        prediction: None,
        entity_id: session.id(),
    }
}

/// The movement attributes last synced to the client, without sprint.
fn body_of(session: &PlayerSession) -> checks::Body {
    let mut body = checks::Body::default();
    if session.published.last_synced_attributes.is_empty() {
        return body;
    }
    for (attribute, &value) in tuff_core::world::attributes::synced_attributes()
        .zip(&session.published.last_synced_attributes)
    {
        match attribute {
            Attribute::MovementSpeed => body.speed = value,
            Attribute::JumpStrength => body.jump_strength = value as f32,
            Attribute::Gravity => body.gravity = value,
            Attribute::StepHeight => body.step_height = value as f32,
            Attribute::SneakingSpeed => body.sneak_speed = value as f32,
            Attribute::MovementEfficiency => body.efficiency = value as f32,
            Attribute::WaterMovementEfficiency => body.water_efficiency = value as f32,
            Attribute::FrictionModifier => body.friction_modifier = value as f32,
            Attribute::AirDragModifier => body.air_drag_modifier = value as f32,
            Attribute::Bounciness => body.bounciness = value,
            _ => {}
        }
    }
    body.synced = true;
    body
}

/// Visible entities around a move to `to`: what may push the player, and
/// the boxes its client collides with.
fn nearby(session: &PlayerSession, to: [f64; 3]) -> Vec<checks::Nearby> {
    let from = session.pose.position;
    let area = [
        from[0].min(to[0]) - NEARBY_REACH,
        from[1].min(to[1]) - NEARBY_REACH,
        from[2].min(to[2]) - NEARBY_REACH,
        from[0].max(to[0]) + NEARBY_REACH,
        from[1].max(to[1]) + NEARBY_REACH,
        from[2].max(to[2]) + NEARBY_REACH,
    ];
    let tick = session.tick;
    let latency = u64::try_from(session.net.latency_ms)
        .unwrap_or(0)
        .div_ceil(50);
    let players = session
        .view
        .entities
        .visible_players()
        .map(|(_, position, _)| (EntityType::Player, position, 0.0, None));
    let others = session.view.entities.visible_colliders().map(|c| {
        let age = tick.saturating_sub(c.moved_at).saturating_sub(latency);
        let travelled = (0..3)
            .map(|i| (c.position[i] - c.previous[i]).abs())
            .fold(0.0, f64::max);
        let fuzz = travelled * lerp_remaining(age);
        (c.kind, c.position, fuzz, c.shape)
    });
    players
        .chain(others)
        .map(|(kind, position, moving, shape)| {
            let shape = shape.unwrap_or_default();
            let bounds = shape.offset.map_or_else(
                || checks::entity_bounds(kind, position),
                |o| {
                    [
                        position[0] + o[0],
                        position[1] + o[1],
                        position[2] + o[2],
                        position[0] + o[3],
                        position[1] + o[4],
                        position[2] + o[5],
                    ]
                },
            );
            let solid = checks::solid_entity(kind)
                && !shape.non_solid
                && (kind != EntityType::HappyGhast || from[1] >= bounds[4] - 1.0e-6);
            let fuzz = if solid { moving } else { 0.0 };
            checks::Nearby {
                bounds,
                solid,
                fuzz,
            }
        })
        .filter(|n| physics::intersects(n.bounds, area))
        .collect()
}

/// Share of a reported move the client's interpolated box still has to
/// cover after `age` ticks.
fn lerp_remaining(age: u64) -> f64 {
    let lerp = checks::LERP_TICKS.max(1);
    if age >= lerp {
        0.0
    } else {
        (lerp - age) as f64 / lerp as f64
    }
}

/// Adds a feeder's flag to the improbable score; true when it tipped over.
fn feed(score: &mut Improbable, facts: &Facts, flag: &Flag) -> bool {
    match improbable::weight_of(flag.check) {
        Some(weight) if flag.weight > 0.0 => score.feed(facts.tick, weight * flag.weight.min(2.0)),
        _ => false,
    }
}

/// States in which the player may hang in the air legitimately, so the
/// latest position is as good an anchor as any.
fn flying_like(session: &PlayerSession) -> bool {
    session.mode.abilities.flying
        || session.motion.glide.active
        || session.motion.spin_attack_ticks > 0
        || session.body.effects.has(MobEffect::Levitation)
}

fn placement_of(session: &PlayerSession, packet: &Serverbound) -> Option<Placement> {
    let Serverbound::UseItemOn { hand, .. } = packet else {
        return None;
    };
    let holding_block = session
        .inventory
        .hand(*hand == 1)
        .is_some_and(|stack| stack.item.block().is_some());
    Placement::probe(
        session.client_world(),
        session.eye(),
        [session.pose.yaw, session.pose.pitch],
        holding_block,
        packet,
    )
}

fn predict_placement(session: &mut PlayerSession, packet: &Serverbound) -> bool {
    let Serverbound::UseItemOn {
        hand,
        position,
        face,
        ..
    } = packet
    else {
        return false;
    };
    let Some(block) = session
        .inventory
        .hand(*hand == 1)
        .and_then(|stack| stack.item.block())
    else {
        return false;
    };
    let (nx, ny, nz) = face.normal();
    let cell = BlockPos::new(position.x + nx, position.y + ny, position.z + nz);
    if checks::placement::fills_body(session.pose.position, *face, *position)
        || !session
            .client_world()
            .block_at(cell)
            .is_some_and(|state| state.block() == tuff_vanilla_data::Block::Air)
    {
        return false;
    }
    session.predict_client_block(cell, block.default_state());
    true
}

pub struct ActionInterceptor;

impl ActionInterceptor {
    fn decide(session: &mut PlayerSession, packet: &Serverbound, echoes: u32) -> Verdict {
        let mut player = session.ext.remove::<Player>().unwrap_or_default();
        let verdict = Self::run(&mut player, session, packet, echoes);
        if matches!(verdict, Verdict::Allow) && predict_placement(session, packet) {
            player.engine.world_ahead();
        }
        *session.ext.entry::<Player>() = player;
        verdict
    }

    fn run(
        player: &mut Player,
        session: &mut PlayerSession,
        packet: &Serverbound,
        echoes: u32,
    ) -> Verdict {
        let mut settings = player.settings(session);
        if !movement_checked(
            &session.shared.game_rules.borrow(),
            session.motion.glide.active,
        ) {
            let mut rules = (*settings).clone();
            rules.exempt.checks.extend(MOVEMENT_CHECKS);
            settings = Arc::new(rules);
        }
        if !settings.enabled
            || settings.exempt.all
            || (settings.exempt_operators && session.mode.permission > PermissionLevel::All)
            || (settings.exempt_creative && session.mode.game_mode == GameMode::Creative)
            || (settings.exempt_spectators && session.mode.game_mode == GameMode::Spectator)
        {
            return Verdict::Allow;
        }
        if session.is_dead() && matches!(packet, Serverbound::Move(_)) {
            return Verdict::Allow;
        }
        player.apply_resets(session);
        let pending = session.sync.pending_teleport.is_some();
        let confirming = player.was_pending && !pending && matches!(packet, Serverbound::Move(_));
        if player.grace_until.is_none() || (player.was_pending && !pending) {
            player.grace_until = Some(session.tick + TELEPORT_GRACE_TICKS);
        }
        player.was_pending = pending;
        let grace = player
            .grace_until
            .is_some_and(|until| session.tick <= until);
        if let Serverbound::MoveVehicle { position, .. } = packet
            && let Some(verdict) = Self::await_echo(player, session, &settings, packet, *position)
        {
            return verdict;
        }
        player
            .anchors
            .observe(session.pose.position, session.dimension);
        if matches!(packet, Serverbound::Move(_) | Serverbound::ClientTickEnd) {
            player.history.record(
                session.tick,
                session.eye(),
                session
                    .view
                    .entities
                    .visible_players()
                    .map(|(id, position, _)| (id, position))
                    .chain(
                        session
                            .view
                            .entities
                            .visible_entities()
                            .map(|(id, _, _, position)| (id, position)),
                    ),
            );
        }
        let mut facts = facts_of(
            session,
            packet,
            grace,
            confirming,
            &player.history,
            &settings,
        );
        facts.echoes = echoes;
        let mut ignored = None;
        player
            .velocity
            .record(&session.motion.velocity_sent, session.net.latency_ms);
        facts.impulses = player.velocity.impulses();
        if facts.teleport_pending || facts.riding {
            player.velocity.excuse_all();
        } else if let Serverbound::Move(movement) = packet
            && let Some(to) = movement
                .position
                .filter(|p| p.iter().all(|c| c.is_finite()))
        {
            let free = !(facts.flying
                || facts.may_fly
                || facts.instabuild
                || facts.game_mode == GameMode::Spectator
                || session.motion.glide.active
                || session.motion.spin_attack_ticks > 0);
            let (push, flag) = player.velocity.settle(&facts, to, free);
            facts.push = push;
            ignored = flag;
        }
        player.engine.observe(&facts, packet);
        if let Serverbound::Move(movement) = packet
            && let Some(to) = movement
                .position
                .filter(|p| p.iter().all(|c| c.is_finite()))
        {
            facts.prediction = player.engine.judge(
                &facts,
                session.client_world(),
                &*session.shared.fluid_behaviors,
                movement,
                to,
            );
            if facts.prediction.as_ref().is_some_and(|p| p.unplaced) {
                player.velocity.unplaced();
            }
        }
        player
            .improbable
            .observe(facts.tick, facts.now, facts.tick_rate);
        let mut improbable = false;
        if let Some(flag) = ignored.filter(|flag| settings.runs(flag.check)) {
            improbable |= feed(&mut player.improbable, &facts, &flag);
            if !settings.feed_only(flag.check)
                && Self::violate(player, session, &settings, flag.check, flag.weight, packet)
                    == Some(Step::Kick)
            {
                return kick(&settings);
            }
        }
        if let Serverbound::Move(movement) = packet
            && let Some(to) = movement
                .position
                .filter(|p| p.iter().all(|c| c.is_finite()))
        {
            let air = player.air.observe(&facts, to, movement.on_ground);
            for (name, ladder) in &mut player.ladders {
                if let Some(info) = checks::info_of(name)
                    && violations::is_movement(name)
                {
                    ladder.on_move(facts.tick, air.frozen, settings.limits(info).decay);
                }
            }
            if air.hover
                && settings.runs(HOVER_CHECK)
                && !settings.feed_only(HOVER_CHECK)
                && Self::violate(
                    player,
                    session,
                    &settings,
                    HOVER_CHECK,
                    violations::HOVER_WEIGHT,
                    packet,
                ) == Some(Step::Kick)
            {
                return kick(&settings);
            }
        }
        let mut worst: Option<Flag> = None;
        let mut logged: Vec<Flag> = Vec::new();
        let debug = player.debug;
        let mut traces: Vec<Trace> = Vec::new();
        for check in &mut player.checks {
            let flag = check.examine(&facts, packet);
            if let Some(filter) = debug
                && let Some(trace) = check.trace()
                && filter.is_none_or(|name| name == trace.check)
            {
                traces.push(trace);
            }
            let weak = check.weak();
            if weak > 0.0 && flag.is_none() {
                improbable |= player.improbable.feed(facts.tick, weak);
            }
            let Some(flag) = flag else {
                continue;
            };
            if !settings.runs(flag.check) {
                continue;
            }
            improbable |= feed(&mut player.improbable, &facts, &flag);
            if settings.feed_only(flag.check) {
                continue;
            }
            if settings.log_only(flag.check) {
                if flag.weight > 0.0 {
                    logged.push(flag);
                }
                continue;
            }
            if flag.fix == Fix::ResyncUse {
                session.stop_using();
            } else if worst.as_ref().is_none_or(|w| flag.weight > w.weight) {
                worst = Some(flag);
            }
        }
        if !traces.is_empty() {
            emit_traces(session, &traces);
        }
        for flag in logged {
            player.flag_trace = trace_of(&player.checks, flag.check);
            Self::violate(player, session, &settings, flag.check, flag.weight, packet);
        }
        if improbable
            && settings.runs(improbable::CHECK)
            && Self::violate(player, session, &settings, improbable::CHECK, 1.0, packet)
                == Some(Step::Kick)
        {
            return kick(&settings);
        }
        let passes = worst.as_ref().is_none_or(|flag| {
            matches!(flag.fix, Fix::Correct(_) | Fix::Pass) || checks::info_of(flag.check).is_none()
        });
        if passes
            && let Serverbound::Move(movement) = packet
            && let Some(to) = movement.position
        {
            let safe = anchor::safe(facts.terrain, flying_like(session));
            player.anchors.passed(to, safe);
        }
        let Some(flag) = worst else {
            return Verdict::Allow;
        };
        player.flag_trace = trace_of(&player.checks, flag.check);
        let Some(step) = Self::violate(player, session, &settings, flag.check, flag.weight, packet)
        else {
            return Verdict::Allow;
        };
        match step {
            Step::Kick => kick(&settings),
            Step::Setback => match flag.fix {
                Fix::Cancel => Verdict::Cancel,
                Fix::Pass => Verdict::Allow,
                Fix::ResyncInventory => {
                    session.send_inventory();
                    session.send_menu_content();
                    Verdict::Cancel
                }
                Fix::Teleport(position) => {
                    let target = player.anchors.setback(position, flying_like(session));
                    let drop = session.pose.position[1] - target[1];
                    if settings.setback_fall_damage && drop > 0.0 {
                        session.body.fall.distance += drop;
                    }
                    session.teleport(target);
                    player.anchors.set_back_to(target);
                    Verdict::Cancel
                }
                Fix::Correct(replacement) => Verdict::Correct(replacement),
                Fix::ResyncUse => Verdict::Allow,
                Fix::MoveVehicle {
                    position,
                    yaw,
                    pitch,
                } => {
                    session.send(&play::MoveVehicle {
                        position,
                        yaw,
                        pitch,
                    });
                    player.vehicle_setback =
                        session.ride.riding.as_ref().map(|riding| VehicleSetback {
                            vehicle: riding.vehicle,
                            position,
                            rotation: (yaw, pitch),
                            check: flag.check,
                            unconfirmed: 0,
                            heard: session.tick,
                        });
                    Verdict::Cancel
                }
                Fix::ResetVehicle {
                    position,
                    yaw,
                    pitch,
                } => {
                    session.send(&play::MoveVehicle {
                        position,
                        yaw,
                        pitch,
                    });
                    Verdict::Cancel
                }
            },
        }
    }

    /// Adds a violation to `check`'s ladder, logging and alerting as
    /// configured; `None` for an unknown check.
    fn violate(
        player: &mut Player,
        session: &mut PlayerSession,
        settings: &Rules,
        check: &'static str,
        weight: f64,
        packet: &Serverbound,
    ) -> Option<Step> {
        let trace = player.flag_trace.take();
        let info = checks::info_of(check)?;
        let limits = settings.limits(info);
        let ladder = player.ladder(check);
        let step = if check == improbable::CHECK {
            ladder.add_relaxing(weight, session.tick, improbable::RELAX, limits.kick_above)
        } else if violations::is_movement(check) {
            ladder.add_movement(weight, session.tick, limits)
        } else {
            ladder.add(weight, session.tick, limits)
        };
        let counts = weight > 0.0 || step == Step::Kick;
        let logged = settings.log_only(check);
        if counts && let Some(board) = session.shared.services.get::<Board>() {
            let ladder = player.ladder(check);
            let level = if check == improbable::CHECK {
                ladder.level_relaxed(session.tick, improbable::RELAX)
            } else {
                ladder.level(session.tick, limits.decay)
            };
            board.publish(service::Violation {
                player: session.profile.id,
                name: player.link.as_ref().map_or_else(
                    || Arc::from(session.profile.name.as_str()),
                    |l| l.name.clone(),
                ),
                check,
                weight,
                level,
                outcome: match step {
                    Step::Kick => service::Outcome::Kicked,
                    Step::Setback if logged => service::Outcome::Logged,
                    Step::Setback => service::Outcome::SetBack,
                },
            });
        }
        if settings.alert && counts {
            match trace {
                Some(trace) => tuff_core::log::warn!(
                    "[anticheat] {} {:?}: {} (weight {}, {}) {}",
                    session.profile.name,
                    step,
                    check,
                    weight,
                    packet_name(packet),
                    trace,
                ),
                None => tuff_core::log::warn!(
                    "[anticheat] {} {:?}: {} (weight {}, {})",
                    session.profile.name,
                    step,
                    check,
                    weight,
                    packet_name(packet),
                ),
            }
        }
        if settings.alert_operators && counts && player.should_alert(check, step, session.tick) {
            let _ = session
                .shared
                .coordinator
                .try_send(CoordinatorMessage::OperatorAlert {
                    line: alert_line(&session.profile.name, step, check, logged),
                    node: permissions::ALERTS,
                    muted: session
                        .shared
                        .services
                        .get::<Board>()
                        .map(Board::muted)
                        .unwrap_or_default(),
                });
        }
        Some(step)
    }

    /// A vehicle move while a vehicle setback waits for its echo: the
    /// echo ends the wait, earlier moves are dropped, and a client that
    /// keeps moving past a round trip ignored the setback, so each of its
    /// moves counts against the check that set it back and its client is
    /// periodically re-seated on the vehicle where the server has it.
    fn await_echo(
        player: &mut Player,
        session: &mut PlayerSession,
        settings: &Rules,
        packet: &Serverbound,
        position: [f64; 3],
    ) -> Option<Verdict> {
        let setback = player.vehicle_setback.as_mut()?;
        let silent = session.tick.saturating_sub(setback.heard) > ECHO_SILENCE;
        setback.heard = session.tick;
        if silent
            || session
                .ride
                .riding
                .as_ref()
                .is_none_or(|riding| riding.vehicle != setback.vehicle)
        {
            player.vehicle_setback = None;
            return None;
        }
        if (0..3).all(|i| (position[i] - setback.position[i]).abs() < ECHO_EPSILON) {
            player.vehicle_setback = None;
            return Some(Verdict::Cancel);
        }
        setback.unconfirmed = setback.unconfirmed.saturating_add(1);
        let round_trip = u32::try_from(session.net.latency_ms).unwrap_or(0) * 2;
        let patience = ECHO_PATIENCE + round_trip.div_ceil(50);
        if setback.unconfirmed <= patience {
            return Some(Verdict::Cancel);
        }
        let (check, ignored) = (setback.check, setback.unconfirmed - patience);
        if ignored % RESEAT_EVERY == 1 {
            reseat(session, setback);
        }
        if !settings.runs(check) {
            return Some(Verdict::Cancel);
        }
        match Self::violate(player, session, settings, check, 1.0, packet) {
            Some(Step::Kick) => Some(kick(settings)),
            _ => Some(Verdict::Cancel),
        }
    }
}

/// The trace behind `check`'s flag on this packet: the one over its
/// allowance when two checks share the name.
fn trace_of(checks: &[Box<dyn Check>], check: &str) -> Option<Trace> {
    let mut traces = checks
        .iter()
        .filter_map(|c| c.trace())
        .filter(|t| t.check == check);
    let first = traces.next()?;
    if first.actual > first.allowed {
        return Some(first);
    }
    Some(traces.find(|t| t.actual > t.allowed).unwrap_or(first))
}

/// Logs and publishes the traces of a player staff are debugging.
fn emit_traces(session: &PlayerSession, traces: &[Trace]) {
    let board = session.shared.services.get::<Board>();
    let name: Arc<str> = Arc::from(session.profile.name.as_str());
    for trace in traces {
        tuff_core::log::info!("[anticheat debug] {} {}", session.profile.name, trace);
        if let Some(board) = board {
            board.publish_trace(service::TraceEvent {
                player: session.profile.id,
                name: name.clone(),
                trace: *trace,
            });
        }
    }
}

/// The kick, with the configured message: it never names the check.
fn kick(settings: &Rules) -> Verdict {
    Verdict::Kick(tuff_core::minimessage::parse(&settings.kick_message))
}

/// Shows the client itself getting off its vehicle, the vehicle where
/// the server has it, and itself getting back on, then puts the vehicle
/// back again: the server's own riders and vehicle never change.
fn reseat(session: &mut PlayerSession, setback: &VehicleSetback) {
    let Some(riding) = session.ride.riding.as_ref() else {
        return;
    };
    let riders = riding.riders.clone();
    let others: Vec<i32> = riders
        .iter()
        .copied()
        .filter(|id| *id != session.id)
        .collect();
    let (vehicle, position, (yaw, pitch)) = (setback.vehicle, setback.position, setback.rotation);
    session.send(&play::SetPassengers {
        vehicle,
        passengers: &others,
    });
    session.send(&play::EntityPositionSync {
        entity_id: vehicle,
        position,
        yaw,
        pitch,
        on_ground: false,
    });
    session.send(&play::SetPassengers {
        vehicle,
        passengers: &riders,
    });
    session.send(&play::MoveVehicle {
        position,
        yaw,
        pitch,
    });
}

/// The operator chat line, styled like other admin notices.
pub fn alert_line(player: &str, step: Step, check: &str, logged: bool) -> TextComponent {
    let action = match step {
        Step::Setback if logged => "flagged",
        Step::Setback => "set back",
        Step::Kick => "kicked",
    };
    TextComponent::translate(
        "chat.type.admin",
        [
            TextComponent::new("Anticheat"),
            TextComponent::new(format!("{player} {action} by the {check} check")),
        ],
    )
    .color("gray")
    .italic(true)
}

fn packet_name(packet: &Serverbound) -> String {
    let text = format!("{packet:?}");
    text.split(|c: char| !c.is_alphanumeric())
        .next()
        .unwrap_or_default()
        .to_owned()
}

impl PacketInterceptor for ActionInterceptor {
    fn intercept<'a>(
        &'a self,
        session: &'a mut PlayerSession,
        packet: &'a Serverbound,
    ) -> Pin<Box<dyn Future<Output = Verdict> + Send + 'a>> {
        Box::pin(async move {
            let echoes = match packet {
                Serverbound::Chat(text) => {
                    let shared = session.shared.clone();
                    match shared.services.get::<Board>() {
                        Some(board) => board.echoes(text, session.profile.id).await,
                        None => 0,
                    }
                }
                _ => 0,
            };
            let was_using = session.actions.using.is_some();
            let verdict = Self::decide(session, packet, echoes);
            if was_using && session.actions.using.is_none() {
                session.publish(false).await;
            }
            match verdict {
                Verdict::Kick(_) if session.fake.is_some() => Verdict::Cancel,
                verdict => verdict,
            }
        })
    }
}

const MOVEMENT_CHECKS: [&str; 5] = ["move_speed", "predict", "fly", "phase", "no_slow"];

/// Whether the world's movement game rules ask for movement validation:
/// the player rule always, the elytra rule too while gliding.
fn movement_checked(rules: &GameRules, gliding: bool) -> bool {
    let on = |name| rules.get(name) != Some(RuleValue::Bool(false));
    on("player_movement_check") && (!gliding || on("elytra_movement_check"))
}

#[cfg(test)]
mod rule_tests {
    use super::*;

    fn rules(player: bool, elytra: bool) -> GameRules {
        let mut rules = GameRules::default();
        rules
            .set("player_movement_check", RuleValue::Bool(player))
            .unwrap();
        rules
            .set("elytra_movement_check", RuleValue::Bool(elytra))
            .unwrap();
        rules
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_rules_gate_walking_and_gliding_separately() {
        assert!(movement_checked(&rules(true, true), true));
        assert!(!movement_checked(&rules(false, true), false));
        assert!(!movement_checked(&rules(false, true), true));
        assert!(movement_checked(&rules(true, false), false));
        assert!(!movement_checked(&rules(true, false), true));
    }
}

#[cfg(test)]
mod lerp_tests {
    use super::lerp_remaining;

    #[tuff_core::watchdog]
    #[test]
    fn collider_fuzz_shrinks_over_the_lerp_window() {
        assert_eq!(lerp_remaining(0), 1.0);
        assert!(lerp_remaining(1) < 1.0 && lerp_remaining(1) > lerp_remaining(2));
        assert_eq!(lerp_remaining(100), 0.0);
    }
}
