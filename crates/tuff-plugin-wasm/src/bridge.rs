//! The fixed hooks that carry player traffic to the host: registered once
//! at startup, inert while no runtime plugin claims anything, so loading
//! and unloading plugins never touches the server's own hook lists.

use std::future::Future;
use std::pin::Pin;

use tuff_core::protocol::play::{self, PlayerActionStatus, Serverbound};
use tuff_core::world::commands::text;
use tuff_core::world::entity::RegionEntity;
use tuff_core::world::game_event::{Emission, GameEvent};
use tuff_core::world::inventory::Inventory;
use tuff_core::world::region_api::{BlockAction, PlayerBlockAction, RegionHooks, RegionWorld};
use tuff_core::world::session::PlayerSession;
use tuff_core::world::session::hooks::{HookFuture, PacketInterceptor, SessionHooks, Verdict};
use tuff_world_model::Face;

use std::sync::Arc;

use tuff_core::world::Shared;
use tuff_core::world::commands::registry::{LineClaim, LineFuture, LineOutcome, SuggestFuture};
use tuff_core::world::commands::{Source, SourceKind};

use crate::host::WasmHost;
use crate::line_filters::Outcome;
use crate::sandbox::{Items, Player, Position, STANDING_EYE_HEIGHT, uuid};
use crate::wit::bindings::types::{
    BlockChange, BlockPos, ChatMessage, Click, ClickKind, Direction, Entity, Event, EventKinds,
    Vec3,
};

/// After the anti-cheat's chat checks, beside the audit log.
pub const ORDER: i32 = 10_100;

pub(crate) fn player(session: &PlayerSession) -> Player {
    Player {
        uuid: session.profile.id,
        name: session.profile.name.clone(),
        level: Some(session.mode.permission),
        position: Some(Position {
            dimension: session.shared.dimensions.get(session.dimension).key.clone(),
            at: session.pose.position,
            look: Some([session.pose.yaw, session.pose.pitch]),
            eye_height: if session.pose.sneaking {
                1.27
            } else {
                STANDING_EYE_HEIGHT
            },
        }),
        items: Some(items(session)),
    }
}

fn items(session: &PlayerSession) -> Items {
    let slots = session.inventory.slots();
    let named =
        |stack: &tuff_item::item::ItemStack| (stack.item().name().to_owned(), stack.count());
    Items {
        held: slots[Inventory::hotbar_slot(session.inventory.selected())]
            .as_ref()
            .map(named),
        slots: slots
            .iter()
            .enumerate()
            .filter_map(|(slot, stack)| {
                let (item, count) = named(stack.as_ref()?);
                Some((slot as u8, item, count))
            })
            .collect(),
        stacks: slots
            .iter()
            .enumerate()
            .filter_map(|(slot, stack)| {
                Some((
                    slot as u8,
                    tuff_item::item::ItemSnapshot::of(stack.as_ref()?),
                ))
            })
            .collect(),
    }
}

/// Runs commands plugins own and hands chat to plugins that listen.
pub struct Commands;

impl PacketInterceptor for Commands {
    fn intercept<'a>(
        &'a self,
        session: &'a mut PlayerSession,
        packet: &'a Serverbound,
    ) -> Pin<Box<dyn Future<Output = Verdict> + Send + 'a>> {
        let (Serverbound::ChatCommand(_) | Serverbound::Chat(_)) = packet else {
            return Box::pin(async { Verdict::Allow });
        };
        Box::pin(async move {
            let shared = session.shared.clone();
            let Some(host) = shared.services.get::<WasmHost>() else {
                return Verdict::Allow;
            };
            match packet {
                Serverbound::ChatCommand(line) => {
                    let mut rewritten = None;
                    match host.command_outcome(line) {
                        Outcome::Pass => {}
                        Outcome::Cancel(notice) => {
                            if let Some(notice) = notice {
                                session.system_message(&text(notice));
                            }
                            return Verdict::Cancel;
                        }
                        Outcome::Replace(with) => rewritten = Some(with),
                    }
                    let line = rewritten.as_ref().unwrap_or(line);
                    if !host.claims(line) {
                        return match rewritten {
                            Some(with) => {
                                Verdict::Correct(Box::new(Serverbound::ChatCommand(with)))
                            }
                            None => Verdict::Allow,
                        };
                    }
                    let Some(outcome) = host.command(player(session), line.clone()).await else {
                        return Verdict::Allow;
                    };
                    for reply in outcome.replies {
                        session.system_message(&text(reply));
                    }
                    if let Err(err) = outcome.result {
                        session.system_message(&text(err).color("red"));
                    }
                    Verdict::Cancel
                }
                Serverbound::Chat(message) => {
                    let mut rewritten = None;
                    match host.chat_outcome(message) {
                        Outcome::Pass => {}
                        Outcome::Cancel(notice) => {
                            if let Some(notice) = notice {
                                session.system_message(&text(notice));
                            }
                            return Verdict::Cancel;
                        }
                        Outcome::Replace(with) => rewritten = Some(with),
                    }
                    if host.listens(EventKinds::CHAT) {
                        let who = player(session);
                        let chat = ChatMessage {
                            player: who.to_wit(),
                            message: rewritten.clone().unwrap_or_else(|| message.clone()),
                        };
                        host.event(Event::Chat(chat), Some(who));
                    }
                    match rewritten {
                        Some(with) => Verdict::Correct(Box::new(Serverbound::Chat(with))),
                        None => Verdict::Allow,
                    }
                }
                _ => Verdict::Allow,
            }
        })
    }
}

/// Runs plugin commands for the console and command blocks, which never
/// pass through the packet hook.
pub struct Lines;

impl LineClaim for Lines {
    fn suggest<'a>(
        &'a self,
        source: &'a Source,
        shared: &'a Arc<Shared>,
        text: &'a str,
    ) -> SuggestFuture<'a> {
        Box::pin(async move {
            let host = shared.services.get::<WasmHost>()?;
            host.suggest(source_player(source, shared), text.to_owned())
                .await
        })
    }

    fn run<'a>(
        &'a self,
        source: &'a Source,
        shared: &'a Arc<Shared>,
        line: &'a str,
    ) -> LineFuture<'a> {
        Box::pin(async move {
            let host = shared.services.get::<WasmHost>()?;
            if !host.claims(line) {
                return None;
            }
            let who = source_player(source, shared);
            let outcome = host.command(who, line.to_owned()).await?;
            Some(LineOutcome {
                replies: outcome.replies,
                result: outcome.result,
            })
        })
    }
}

fn source_player(source: &Source, shared: &Shared) -> Player {
    let uuid = match &source.kind {
        SourceKind::Player { uuid, .. } => *uuid,
        SourceKind::Console(_) => 0,
    };
    Player {
        uuid,
        name: source.name.clone(),
        level: Some(source.permission),
        position: Some(Position {
            dimension: shared.dimensions.get(source.dimension).key.clone(),
            at: source.position,
            look: Some(source.rotation),
            eye_height: STANDING_EYE_HEIGHT,
        }),
        items: None,
    }
}

/// Joins and leaves.
pub struct Sessions;

impl SessionHooks for Sessions {
    fn joined<'a>(&'a self, session: &'a mut PlayerSession) -> HookFuture<'a> {
        Box::pin(async move {
            if let Some(host) = session.shared.services.get::<WasmHost>()
                && host.listens(EventKinds::JOIN)
            {
                let player = player(session);
                host.event(Event::Join(player.to_wit()), Some(player));
            }
        })
    }

    fn leaving<'a>(&'a self, session: &'a mut PlayerSession) -> HookFuture<'a> {
        Box::pin(async move {
            let shared = session.shared.clone();
            if let Some(host) = shared.services.get::<WasmHost>() {
                host.left(player(session)).await;
            }
        })
    }
}

/// Block breaks and placements, from every region.
pub struct Blocks;

impl RegionHooks for Blocks {
    fn player_block_action(&self, world: &mut dyn RegionWorld, action: &PlayerBlockAction) {
        let kind = match action.action {
            BlockAction::Broke => EventKinds::BLOCK_BREAK,
            BlockAction::Placed => EventKinds::BLOCK_PLACE,
            BlockAction::Opened => return,
        };
        let shared = world.shared().clone();
        let Some(host) = shared.services.get::<WasmHost>() else {
            return;
        };
        if !host.listens(kind) {
            return;
        }
        let dimension = world.dimension_info().key.clone();
        let player = world
            .seen_player_details()
            .into_iter()
            .find(|seen| seen.handle.id == action.player)
            .map(|seen| Player {
                uuid: seen.uuid,
                name: seen.name,
                level: None,
                position: Some(Position {
                    dimension: dimension.clone(),
                    at: seen.position,
                    look: None,
                    eye_height: STANDING_EYE_HEIGHT,
                }),
                items: None,
            });
        let change = BlockChange {
            player: player.as_ref().map(Player::to_wit),
            pos: BlockPos {
                x: action.pos.x,
                y: action.pos.y,
                z: action.pos.z,
            },
            dimension,
            state: action.state.to_string(),
        };
        let event = match kind {
            EventKinds::BLOCK_BREAK => Event::BlockBreak(change),
            _ => Event::BlockPlace(change),
        };
        host.event(event, player);
    }
}

pub(crate) fn entity_of(world: &dyn RegionWorld, entity: &dyn RegionEntity) -> Entity {
    let base = entity.base();
    let [x, y, z] = base.position;
    Entity {
        uuid: uuid(base.uuid),
        kind: base.kind.name().to_owned(),
        pos: Vec3 { x, y, z },
        dimension: world.dimension_info().key.clone(),
    }
}

/// The entity a hit or use on `id` is reported for, its tags and its custom
/// id: an interaction part resolves to the custom entity it rides.
pub(crate) fn target_of(
    world: &dyn RegionWorld,
    id: i32,
) -> Option<(Entity, Vec<String>, Option<String>)> {
    let owner = world.entity(tuff_core::custom::entities::reported_entity(world, id))?;
    let tags = owner.entity.base().tags.clone();
    let custom = tuff_core::custom::entity_id(&tags).map(str::to_owned);
    Some((entity_of(world, &*owner.entity), tags, custom))
}

/// Spawns, deaths and damage of non-player entities, from every region.
pub struct Entities;

impl RegionHooks for Entities {
    fn entity_spawned(&self, world: &mut dyn RegionWorld, entity: &dyn RegionEntity) {
        let shared = world.shared().clone();
        let Some(host) = shared.services.get::<WasmHost>() else {
            return;
        };
        if host.listens(EventKinds::ENTITY_SPAWN) {
            host.event(Event::EntitySpawn(entity_of(world, entity)), None);
        }
    }

    fn game_event(&self, world: &mut dyn RegionWorld, emission: &Emission) {
        let kind = match emission.event {
            GameEvent::EntityDie => EventKinds::ENTITY_DEATH,
            GameEvent::EntityDamage => EventKinds::ENTITY_DAMAGE,
            _ => return,
        };
        let shared = world.shared().clone();
        let Some(host) = shared.services.get::<WasmHost>() else {
            return;
        };
        if !host.listens(kind) {
            return;
        }
        let Some(source) = emission.context.source else {
            return;
        };
        let Some(tracked) = world.entity(source.entity) else {
            return;
        };
        let entity = entity_of(world, &*tracked.entity);
        let event = match kind {
            EventKinds::ENTITY_DEATH => Event::EntityDeath(entity),
            _ => Event::EntityDamage(entity),
        };
        host.event(event, None);
    }
}

fn direction(face: Face) -> Direction {
    match face {
        Face::Down => Direction::Down,
        Face::Up => Direction::Up,
        Face::North => Direction::North,
        Face::South => Direction::South,
        Face::West => Direction::West,
        Face::East => Direction::East,
    }
}

/// Cancels and reports the clicks plugins filter for (`intercept_click`).
pub struct Clicks;

impl PacketInterceptor for Clicks {
    fn intercept<'a>(
        &'a self,
        session: &'a mut PlayerSession,
        packet: &'a Serverbound,
    ) -> Pin<Box<dyn Future<Output = Verdict> + Send + 'a>> {
        Box::pin(async move {
            let (kind, position, face, sequence) = match packet {
                Serverbound::PlayerAction {
                    status: PlayerActionStatus::StartDestroyBlock,
                    position,
                    face,
                    sequence,
                } => (
                    ClickKind::Left,
                    *position,
                    Face::from_id(i32::from(*face)).unwrap_or(Face::Up),
                    *sequence,
                ),
                Serverbound::UseItemOn {
                    hand: 0,
                    position,
                    face,
                    sequence,
                    ..
                } => (ClickKind::Right, *position, *face, *sequence),
                _ => return Verdict::Allow,
            };
            let shared = session.shared.clone();
            let Some(host) = shared.services.get::<WasmHost>() else {
                return Verdict::Allow;
            };
            let wanted = match kind {
                ClickKind::Left => EventKinds::LEFT_CLICK,
                ClickKind::Right | ClickKind::Air => EventKinds::RIGHT_CLICK,
            };
            if !host.listens(wanted) {
                return Verdict::Allow;
            }
            let held = session.inventory.slots()
                [Inventory::hotbar_slot(session.inventory.selected())]
            .as_ref()
            .map(tuff_item::item::ItemSnapshot::of);
            let Some(held) = held else {
                return Verdict::Allow;
            };
            let blocks = host.matching_clicks(
                kind,
                held.item().name(),
                |tag| {
                    held.components
                        .other
                        .get("minecraft:custom_data")
                        .and_then(|data| data.as_compound())
                        .is_some_and(|data| data.get(tag).is_some())
                },
                |scope| scope.applies_to_player(session),
            );
            if blocks.is_empty() {
                return Verdict::Allow;
            }
            let dimension_id = session.dimension;
            let state = crate::world::read(shared.clone(), dimension_id, vec![position])
                .await
                .pop()
                .flatten();
            let matched = blocks.iter().any(|wanted| match wanted {
                None => true,
                Some(wanted) => state
                    .is_some_and(|state| crate::portal::same_block(state.block().name(), wanted)),
            });
            if !matched {
                return Verdict::Allow;
            }
            let who = player(session);
            let click = Click {
                player: who.to_wit(),
                pos: BlockPos {
                    x: position.x,
                    y: position.y,
                    z: position.z,
                },
                dimension: session.shared.dimensions.get(session.dimension).key.clone(),
                face: direction(face),
                state: state.map(|state| state.to_string()).unwrap_or_default(),
            };
            let event = match kind {
                ClickKind::Left => Event::LeftClick(click),
                ClickKind::Right | ClickKind::Air => Event::RightClick(click),
            };
            host.event(event, Some(who));
            if let Some(state) = state {
                session.send(&play::BlockUpdate {
                    position,
                    state: state.id(),
                });
            }
            session.send(&play::BlockChangedAck(sequence));
            Verdict::Cancel
        })
    }
}

pub const LOGIN_TIMEOUT_MS: i64 = 2000;

const UNANSWERED: &str = "Login could not be verified, try again later";

/// Asks plugins that listen for logins whether to refuse one, off the
/// hot path: nothing is sent while no plugin subscribed to `login`.
pub struct LoginBridge;

impl tuff_core::world::login_gate::LoginGates for LoginBridge {
    fn open(&self) -> Box<dyn tuff_core::world::login_gate::LoginGate> {
        Box::new(LoginBridge)
    }
}

impl tuff_core::world::login_gate::LoginGate for LoginBridge {
    fn admit(
        &mut self,
        attempt: &tuff_core::world::login_gate::Attempt<'_>,
    ) -> Result<(), tuff_item::text::TextComponent> {
        let Some(host) = attempt.shared.services.get::<WasmHost>() else {
            return Ok(());
        };
        if !host.listens(EventKinds::LOGIN) {
            return Ok(());
        }
        let config = &attempt.shared.config;
        let timeout = config
            .get_int(crate::LOGIN_TIMEOUT_KEY)
            .unwrap_or(LOGIN_TIMEOUT_MS)
            .clamp(1, 10_000) as u64;
        let unanswered = config
            .get_bool(crate::LOGIN_FAIL_CLOSED_KEY)
            .unwrap_or(false)
            .then(|| UNANSWERED.to_owned());
        let who = Player {
            uuid: attempt.user.uuid,
            name: attempt.user.name.clone(),
            level: Some(attempt.permission),
            position: None,
            items: None,
        };
        match host.decide_login(
            who,
            attempt.address.to_string(),
            std::time::Duration::from_millis(timeout),
            unanswered,
        ) {
            Some(reason) => Err(tuff_item::text::TextComponent::new(reason)),
            None => Ok(()),
        }
    }
}
