//! Resolving selectors against the world: players come from the
//! coordinator's snapshots, items and orbs from every running region. A
//! command then acts on the targets by sending each player a directive and
//! gathering the replies (see [`super::feedback`]).

use tokio::sync::{mpsc, oneshot};
use tuff_vanilla_data::registry::EntityType;

use crate::command::CommandError;
use crate::command::args::Message;
use crate::command::selector::{Candidate, Selector, SelectorSource};
use crate::world::messages::{
    CoordinatorMessage, Directive, DirectiveReply, DirectiveSource, EntitySnapshot, ManagerMessage,
    PlayerEvent, PlayerSnapshot, RegionRequest,
};
use crate::world::{EYE_HEIGHT, PLAYER_HALF_WIDTH, PLAYER_HEIGHT};
use tuff_item::text::TextComponent;

use super::feedback::{self, Messages};
use super::{Context, SourceKind};

/// An entity a selector picked.
#[derive(Debug, Clone)]
pub enum Target {
    Player(PlayerSnapshot),
    Entity(EntitySnapshot),
}

impl Target {
    pub fn display_name(&self) -> TextComponent {
        match self {
            Self::Player(player) => TextComponent::new(player.name.clone()),
            Self::Entity(entity) => TextComponent::new(entity.name.clone()),
        }
    }

    /// The name command feedback shows: decorated by its team, with the
    /// entity on hover.
    pub fn feedback_name(&self, teams: &crate::world::scoreboard::Teams) -> TextComponent {
        match self {
            Self::Player(player) => teams.player_name(&player.name, player.uuid),
            Self::Entity(entity) => teams.owned_entity_name(
                entity.registered.as_deref().unwrap_or(entity.kind.name()),
                entity.uuid,
                TextComponent::new(entity.name.clone()),
                entity.owner_name.as_deref(),
            ),
        }
    }

    pub fn position(&self) -> [f64; 3] {
        match self {
            Self::Player(player) => player.state.position,
            Self::Entity(entity) => entity.position,
        }
    }

    pub fn dimension(&self) -> crate::world::dimension::DimensionId {
        match self {
            Self::Player(player) => player.state.dimension,
            Self::Entity(entity) => entity.dimension,
        }
    }

    pub fn rotation(&self) -> [f32; 2] {
        match self {
            Self::Player(player) => player.state.rotation,
            Self::Entity(_) => [0.0, 0.0],
        }
    }

    pub fn id(&self) -> i32 {
        match self {
            Self::Player(player) => player.id,
            Self::Entity(entity) => entity.id,
        }
    }
}

fn player_candidate(player: &PlayerSnapshot) -> Candidate {
    let [x, y, z] = player.state.position;
    Candidate {
        id: player.id,
        uuid: player.uuid,
        name: player.name.clone(),
        kind: EntityType::Player,
        dimension: player.state.dimension,
        position: player.state.position,
        rotation: player.state.rotation,
        bounds: [
            x - PLAYER_HALF_WIDTH,
            y,
            z - PLAYER_HALF_WIDTH,
            x + PLAYER_HALF_WIDTH,
            y + PLAYER_HEIGHT,
            z + PLAYER_HALF_WIDTH,
        ],
        alive: player.state.alive,
        level: Some(player.state.level),
        game_mode: Some(player.state.game_mode),
        tags: player.state.tags.clone(),
        advancements: player.state.advancements.clone(),
        registered: None,
        team: None,
        scores: Vec::new(),
        nbt: None,
        predicates: Vec::new(),
    }
}

fn entity_candidate(entity: &EntitySnapshot) -> Candidate {
    Candidate {
        id: entity.id,
        uuid: entity.uuid,
        name: entity.name.clone(),
        kind: entity.kind,
        dimension: entity.dimension,
        position: entity.position,
        rotation: [0.0, 0.0],
        bounds: entity.bounds,
        alive: true,
        level: None,
        game_mode: None,
        tags: entity.tags.clone(),
        advancements: None,
        registered: entity.registered.clone(),
        team: None,
        scores: Vec::new(),
        nbt: entity.nbt.clone(),
        predicates: Vec::new(),
    }
}

/// Everyone online, from the coordinator.
pub async fn players(ctx: &Context<'_>) -> Vec<PlayerSnapshot> {
    let (reply, answer) = oneshot::channel();
    ctx.coordinator(CoordinatorMessage::Players { reply }).await;
    answer.await.unwrap_or_default()
}

/// Every entity in every running region, players aside.
pub async fn entities(ctx: &Context<'_>) -> Vec<EntitySnapshot> {
    region_entities(ctx, false).await
}

async fn region_entities(ctx: &Context<'_>, nbt: bool) -> Vec<EntitySnapshot> {
    let (reply, answer) = oneshot::channel();
    if ctx
        .shared()
        .manager
        .send(ManagerMessage::Regions { reply })
        .await
        .is_err()
    {
        return Vec::new();
    }
    let regions = answer.await.unwrap_or_default();
    let mut found = Vec::new();
    for region in regions {
        let (reply, answer) = oneshot::channel();
        let request = if nbt {
            RegionRequest::EntitiesWithData { reply }
        } else {
            RegionRequest::Entities { reply }
        };
        if region.request(request).await
            && let Ok(mut entities) = answer.await
        {
            found.append(&mut entities);
        }
    }
    found
}

fn selector_source(ctx: &Context<'_>) -> SelectorSource {
    SelectorSource {
        position: ctx.source.position,
        dimension: ctx.source.dimension,
        entity: ctx.source.entity,
    }
}

/// Fills in what the selector tests beyond what snapshots carry: teams and
/// scores from the scoreboard, and players' saved forms.
async fn enrich(ctx: &mut Context<'_>, selector: &Selector, candidates: &mut [Candidate]) {
    if selector.needs_scoreboard() {
        let holders: Vec<String> = candidates.iter().map(Candidate::score_holder).collect();
        let objectives: Vec<String> = selector.scores.iter().map(|(o, _)| o.clone()).collect();
        let found = ctx
            .shared()
            .scoreboard
            .read(move |board| {
                holders
                    .iter()
                    .map(|holder| {
                        let team = board.team_of(holder).map(|team| team.name.clone());
                        let scores: Vec<(String, i32)> = objectives
                            .iter()
                            .filter_map(|o| Some((o.clone(), board.score(holder, o)?.value)))
                            .collect();
                        (team, scores)
                    })
                    .collect::<Vec<_>>()
            })
            .await
            .unwrap_or_default();
        for (candidate, (team, scores)) in candidates.iter_mut().zip(found) {
            candidate.team = team;
            candidate.scores = scores;
        }
    }
    if selector.needs_nbt() {
        let snapshots = players(ctx).await;
        for candidate in candidates.iter_mut().filter(|c| c.is_player()) {
            if let Some(own) = ctx.actor.own_selector_nbt(candidate.id) {
                candidate.nbt = Some(own);
            } else if let Some(player) = snapshots.iter().find(|p| p.id == candidate.id) {
                candidate.nbt = player.handle.selector_data().map(|form| (*form).clone());
            }
        }
    }
}

/// The players a selector picks; `Err` when none (the game's "no player
/// was found").
pub async fn select_players(
    ctx: &mut Context<'_>,
    selector: &Selector,
) -> Result<Vec<PlayerSnapshot>, CommandError> {
    let tagged = selector.with_tags(&ctx.shared().datapacks.contents().tags);
    let selector = tagged.as_ref().unwrap_or(selector);
    let snapshots = players(ctx).await;
    let mut candidates: Vec<Candidate> = snapshots.iter().map(player_candidate).collect();
    enrich(ctx, selector, &mut candidates).await;
    let source = selector_source(ctx);
    super::predicates::apply(ctx, selector, &source, &mut candidates, &snapshots).await;
    let chosen = selector.select(&source, &candidates, ctx.rng());
    let ids: Vec<i32> = chosen.iter().map(|c| c.id).collect();
    let found: Vec<PlayerSnapshot> = ids
        .iter()
        .filter_map(|id| snapshots.iter().find(|p| p.id == *id).cloned())
        .collect();
    if found.is_empty() {
        return Err(CommandError::simple("argument.entity.notfound.player"));
    }
    Ok(found)
}

/// Exactly one player, else the game's error for it.
pub async fn select_player(
    ctx: &mut Context<'_>,
    selector: &Selector,
) -> Result<PlayerSnapshot, CommandError> {
    let mut found = select_players(ctx, selector).await?;
    if found.len() != 1 {
        return Err(CommandError::simple("argument.entity.notfound.player"));
    }
    Ok(found.remove(0))
}

/// The entities a selector picks: players and, for `@e`-style selectors,
/// items and orbs.
pub async fn select_entities(
    ctx: &mut Context<'_>,
    selector: &Selector,
) -> Result<Vec<Target>, CommandError> {
    let tagged = selector.with_tags(&ctx.shared().datapacks.contents().tags);
    let selector = tagged.as_ref().unwrap_or(selector);
    let snapshots = players(ctx).await;
    let mut candidates: Vec<Candidate> = snapshots.iter().map(player_candidate).collect();
    let entities = if selector.includes_entities {
        region_entities(ctx, selector.needs_nbt()).await
    } else {
        Vec::new()
    };
    candidates.extend(entities.iter().map(entity_candidate));
    enrich(ctx, selector, &mut candidates).await;
    let source = selector_source(ctx);
    super::predicates::apply(ctx, selector, &source, &mut candidates, &snapshots).await;
    let chosen = selector.select(&source, &candidates, ctx.rng());
    let ids: Vec<(i32, bool)> = chosen.iter().map(|c| (c.id, c.is_player())).collect();
    let mut found = Vec::new();
    for (id, is_player) in ids {
        if is_player {
            if let Some(player) = snapshots.iter().find(|p| p.id == id) {
                found.push(Target::Player(player.clone()));
            }
        } else if let Some(entity) = entities.iter().find(|e| e.id == id) {
            found.push(Target::Entity(entity.clone()));
        }
    }
    if found.is_empty() {
        return Err(CommandError::simple("argument.entity.notfound.entity"));
    }
    Ok(found)
}

pub async fn select_entity(
    ctx: &mut Context<'_>,
    selector: &Selector,
) -> Result<Target, CommandError> {
    let mut found = select_entities(ctx, selector).await?;
    if found.len() != 1 {
        return Err(CommandError::simple("argument.entity.toomany"));
    }
    Ok(found.remove(0))
}

/// The source as a target, for commands that default to `@s`.
pub async fn source_player(ctx: &mut Context<'_>) -> Result<PlayerSnapshot, CommandError> {
    let SourceKind::Player { id, .. } = ctx.source.kind else {
        return Err(CommandError::simple("permissions.requires.player"));
    };
    players(ctx)
        .await
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| CommandError::simple("permissions.requires.player"))
}

/// Sends `directive` to every player in `targets` and hands the replies
/// to a collector that delivers `messages` as feedback. Players in other
/// sessions answer on their own time; the source's own player answers
/// right away.
async fn send_directives(
    ctx: &mut Context<'_>,
    targets: &[PlayerSnapshot],
    directive: impl Fn(&PlayerSnapshot) -> Directive,
) -> (mpsc::UnboundedReceiver<DirectiveReply>, usize) {
    let (reply, replies) = mpsc::unbounded_channel();
    let source = DirectiveSource {
        name: ctx.source.display_name(),
        player: ctx.source.player_id(),
    };
    let mut expected = 0;
    for target in targets {
        let directive = directive(target);
        if let Some(own) = ctx
            .actor
            .apply_self(target.id, directive.clone(), source.clone())
            .await
        {
            let _ = reply.send(own);
            expected += 1;
            continue;
        }
        if target.handle.send(PlayerEvent::Directive {
            directive,
            source: source.clone(),
            reply: reply.clone(),
        }) {
            expected += 1;
        }
    }
    drop(reply);
    (replies, expected)
}

/// Sends `directive` to every player in `targets` and returns their
/// replies, waiting for the other sessions to answer.
pub async fn dispatch_wait(
    ctx: &mut Context<'_>,
    targets: &[PlayerSnapshot],
    directive: impl Fn(&PlayerSnapshot) -> Directive,
) -> Vec<DirectiveReply> {
    let (mut replies, expected) = send_directives(ctx, targets, directive).await;
    feedback::gather(&mut replies, expected).await
}

/// [`dispatch`] that waits for the replies, delivers the feedback and
/// returns the total of the affected targets' values, so `execute store`
/// sees what the game's response tracker reports. A failure is the error.
pub async fn dispatch_result(
    ctx: &mut Context<'_>,
    targets: &[PlayerSnapshot],
    directive: impl Fn(&PlayerSnapshot) -> Directive,
    messages: Messages,
) -> Result<i32, CommandError> {
    let replies = dispatch_wait(ctx, targets, directive).await;
    let total = replies
        .iter()
        .filter(|reply| reply.affected)
        .map(|reply| reply.value)
        .sum();
    if messages.silent {
        return Ok(total);
    }
    match messages.resolve(&replies) {
        Ok(message) => {
            ctx.success(message, messages.broadcast).await;
            Ok(total)
        }
        Err(error) => Err(CommandError::text(error)),
    }
}

pub async fn dispatch(
    ctx: &mut Context<'_>,
    targets: &[PlayerSnapshot],
    directive: impl Fn(&PlayerSnapshot) -> Directive,
    messages: Messages,
) {
    let (replies, expected) = send_directives(ctx, targets, directive).await;
    let shared = ctx.shared().clone();
    let feedback = ctx.source.feedback_target();
    let name = ctx.source.display_name();
    let task = tokio::spawn(feedback::collect(
        shared, feedback, name, replies, expected, messages,
    ));
    ctx.actor.track(task);
}

/// A player's eye position, for anchors.
pub fn eye_of(target: &Target) -> [f64; 3] {
    let mut position = target.position();
    position[1] += match target {
        Target::Player(_) => EYE_HEIGHT,
        Target::Entity(entity) => crate::world::entity::eye_height(entity.kind, entity.bounds),
    };
    position
}

/// Resolves a message argument's selectors into names.
pub async fn resolve_message(
    ctx: &mut Context<'_>,
    message: &Message,
) -> Result<TextComponent, CommandError> {
    let mut out = String::new();
    let mut from = 0;
    for (start, end, selector) in &message.parts {
        out.push_str(&message.text[from..*start]);
        let names: Vec<String> = match select_entities(ctx, selector).await {
            Ok(found) => found.iter().map(|t| t.display_name().plain()).collect(),
            Err(err) if err.key() == Some("argument.entity.notfound.entity") => Vec::new(),
            Err(err) => return Err(err),
        };
        out.push_str(&names.join(", "));
        from = *end;
    }
    out.push_str(&message.text[from..]);
    Ok(TextComponent::new(out))
}
