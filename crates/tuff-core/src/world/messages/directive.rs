use std::net::IpAddr;
use std::sync::Arc;

use tuff_vanilla_data::registry::{EntityType, MobEffect};

use crate::command::item::ItemPredicate;
use crate::protocol::play::GameMode;
use crate::world::dimension::DimensionId;
use tuff_item::item::ItemStack;
use tuff_item::text::TextComponent;
use tuff_nbt::Compound;
use tuff_storage::admin::NameAndId;
use tuff_storage::admin::PermissionLevel;
use tuff_world_model::BlockPos;

use super::*;

/// Who issued a directive, for messages that name the sender.
#[derive(Debug, Clone)]
pub struct DirectiveSource {
    pub name: TextComponent,
    pub player: Option<PlayerId>,
}

/// How `/item` spreads a stack over the slots it selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotFill {
    /// Only the first slot gets it.
    Replace,
    /// Every slot gets a copy.
    Fill,
    /// The first slot gets it and the others are emptied.
    Override,
}

/// What a command does to one player.
#[derive(Clone, Debug)]
pub enum Directive {
    GameMode(GameMode),
    /// `/recipe give|take`: `None` means every recipe.
    Recipes {
        give: bool,
        recipes: Option<Vec<tuff_vanilla_data::Frozen<str>>>,
    },
    /// `/advancement`: meets (or un-meets) every criterion of each
    /// advancement, or just `criterion` of the only one. `show: false`
    /// sends the resulting update without toasts.
    Advancement {
        grant: bool,
        /// The tree `ids` index; a session on another tree (a reload in
        /// between) matches them by name.
        tree: Arc<dyn crate::world::advancement::Advancements>,
        ids: Vec<crate::world::advancement::Id>,
        criterion: Option<String>,
        show: bool,
    },
    /// Gives `count` items of `stack`, split into stacks and dropped when
    /// the inventory is full.
    Give {
        stack: tuff_item::item::ItemSnapshot,
        count: i32,
    },
    /// Removes matching items, up to `max_count` (`-1` for all; `0` only
    /// counts).
    Clear {
        predicate: Option<ItemPredicate>,
        max_count: i32,
    },
    Kill,
    /// `/dialog show`: opens a built-in dialog by its registry id, or an
    /// inline definition.
    ShowDialog {
        dialog: DialogRef,
    },
    /// `/dialog clear`.
    ClearDialog,
    /// `/posteffect`: `value` is the effect id for add and remove.
    PostEffect {
        op: PostEffectOp,
        value: Option<String>,
    },
    /// `/swing`: plays a swing animation of the player for everyone
    /// watching, themselves included.
    Swing {
        hand: i32,
        kind: tuff_item::item::SwingKind,
        duration: i32,
    },
    /// `/spectate`: points a spectator's camera at an entity, or back
    /// at themselves with `None`.
    Spectate {
        target: Option<i32>,
        target_name: TextComponent,
        untrackable: bool,
    },
    /// A feature sets health (above zero) and food level directly.
    SetVitals {
        health: Option<f32>,
        food: Option<i32>,
    },
    /// A feature revives a dead player at their respawn point.
    Respawn,
    /// `/particle`: sends the particle when this player is close enough
    /// (512 blocks forced, else 32) in the same dimension.
    Particle {
        dimension: DimensionId,
        particle: u32,
        options: std::sync::Arc<[u8]>,
        force: bool,
        position: [f64; 3],
        delta: [f32; 3],
        speed: f32,
        count: i32,
    },
    /// `/item replace|fill|override`: puts `stack` into the player's slots
    /// (game slot ids); the reply is how many slots changed, with the
    /// game's error when none matched or none changed.
    SetSlots {
        ids: Vec<i32>,
        name: String,
        stack: tuff_item::item::ItemSnapshot,
        mode: SlotFill,
    },
    /// `/item modify`: runs item modifier `functions` over the player's
    /// non-empty slots among `ids`; the reply is how many slots changed.
    ModifySlots {
        ids: Vec<i32>,
        name: String,
        functions: std::sync::Arc<Vec<tuff_vanilla_data::loot_table::LootFunction>>,
    },
    /// `/damage`: hurts the player, as coming from `position` if given.
    Hurt {
        kind: &'static tuff_vanilla_data::damage_type::DamageType,
        amount: f32,
        position: Option<[f64; 3]>,
        /// Who is blamed (`/damage ... by ... from`), for death messages.
        attacker: Option<crate::world::entity::Culprit>,
        /// The attacker dealt it itself.
        direct: bool,
    },
    Teleport(Teleport),
    Experience {
        amount: i32,
        levels: bool,
        set: bool,
    },
    ExperienceQuery {
        levels: bool,
    },
    EffectGive {
        effect: MobEffect,
        duration: i32,
        amplifier: u8,
        particles: bool,
    },
    EffectClear(Option<MobEffect>),
    Enchant {
        enchantment: String,
        level: i32,
    },
    SpawnPoint {
        dimension: DimensionId,
        pos: BlockPos,
        yaw: f32,
        pitch: f32,
    },
    Title(TitleAction),
    /// A custom payload packet for the client.
    Payload {
        channel: String,
        data: Vec<u8>,
    },
    Tellraw(TextComponent),
    PlaySound {
        sound: String,
        category: i32,
        position: [f64; 3],
        volume: f32,
        pitch: f32,
        min_volume: f32,
        seed: i64,
    },
    StopSound {
        category: Option<i32>,
        sound: Option<String>,
    },
    Kick(TextComponent),
    /// `/transfer`: send the player to another server.
    Transfer {
        host: String,
        port: i32,
    },
    /// A `/msg` for this player, already formatted as the incoming line.
    PrivateMessage(TextComponent),
    /// `/tag add` or `/tag remove`.
    Tag(TagEdit),
    /// `/waypoint modify`.
    Waypoint(WaypointEdit),
    /// `/attribute`.
    Attribute(AttributeCommand),
}

#[derive(Debug, Clone)]
pub struct Teleport {
    pub dimension: DimensionId,
    pub position: [f64; 3],
    /// Axes given as `~` offsets, which the player's own movement is
    /// kept on.
    pub relative: [bool; 3],
    /// Yaw and pitch, or `None` to keep the player's.
    pub rotation: Option<[f32; 2]>,
    pub relative_rotation: [bool; 2],
    /// A point to face after arriving.
    pub look_at: Option<LookAt>,
}

/// A point a teleported entity turns to face, from its feet or its eyes
/// (the command source's anchor).
#[derive(Debug, Clone, Copy)]
pub struct LookAt {
    pub point: [f64; 3],
    pub from: crate::command::args::Anchor,
}

#[derive(Debug, Clone)]
pub enum TitleAction {
    Title(TextComponent),
    Subtitle(TextComponent),
    ActionBar(TextComponent),
    Times {
        fade_in: i32,
        stay: i32,
        fade_out: i32,
    },
    Clear,
    Reset,
}

/// The outcome of a directive on one player, for the command's feedback.
#[derive(Debug, Clone)]
pub struct DirectiveReply {
    pub player: PlayerId,
    pub name: String,
    /// The value the game's feedback counts (items cleared, 1 for done).
    pub value: i32,
    /// Whether the player counts as affected.
    pub affected: bool,
    /// The error the game would show when this was the only target.
    pub error: Option<TextComponent>,
    /// What an `/attribute` directive found.
    pub attribute: Option<AttributeAnswer>,
    /// What a `/posteffect list` found.
    pub post_effects: Vec<String>,
}

/// A player as commands see it, kept current by the coordinator.
#[derive(Debug, Clone)]
pub struct PlayerSnapshot {
    pub id: PlayerId,
    pub uuid: u128,
    pub name: String,
    pub handle: PlayerHandle,
    pub address: IpAddr,
    pub permission: PermissionLevel,
    pub state: PlayerState,
    /// A fake player a feature drives.
    pub fake: bool,
}

/// The web panel's view of who is online, who is known, and the
/// operator, whitelist and ban lists.
#[derive(Debug, Clone, Default)]
pub struct Roster {
    pub online: Vec<RosterEntry>,
    /// Players who logged in or were looked up since the server started.
    pub known: Vec<NameAndId>,
    pub lists: tuff_storage::admin::UserLists,
    pub whitelist: bool,
    pub enforce_whitelist: bool,
}

/// One online player as the web panel shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct RosterEntry {
    pub uuid: u128,
    pub name: String,
    pub address: IpAddr,
    pub permission: PermissionLevel,
    pub latency: i32,
    pub brand: Option<String>,
    pub game_mode: GameMode,
    pub dimension: Option<DimensionId>,
    pub position: Option<[f64; 3]>,
}

/// What a session reports about itself whenever it changes.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerState {
    pub dimension: DimensionId,
    pub position: [f64; 3],
    /// What mobs see of the player; regions read it from the player index.
    pub looks: PlayerLooks,
    pub rotation: [f32; 2],
    pub game_mode: GameMode,
    pub level: i32,
    pub alive: bool,
    /// In a bed, and for how long: `deep` once asleep long enough to count
    /// towards skipping the night.
    pub sleeping: bool,
    pub deep_sleeping: bool,
    /// The `minecraft:time_since_rest` custom statistic, for phantoms'
    /// insomnia-driven natural spawning.
    pub time_since_rest: i32,
    /// Advancement progress, for the `advancements` selector option.
    pub advancements: Option<crate::world::advancement::View>,
    /// The scoreboard tags `/tag` keeps.
    pub tags: Vec<String>,
    /// The locator bar attributes and the chunk radius the client tracks.
    pub waypoint_transmit_range: f64,
    pub waypoint_receive_range: f64,
    pub waypoint_icon: crate::world::waypoints::Icon,
    pub view_distance: i32,
    pub health: f32,
    pub food: i32,
    /// Total experience points.
    pub experience_total: i32,
    /// Active effects: the effect and its amplifier.
    pub effects: Vec<(MobEffect, u8)>,
}

/// An item or experience orb as selectors see it.
#[derive(Debug, Clone)]
pub struct EntitySnapshot {
    pub id: i32,
    pub uuid: u128,
    pub kind: EntityType,
    pub name: String,
    pub dimension: DimensionId,
    pub position: [f64; 3],
    pub bounds: [f64; 6],
    /// The experience an orb holds (value times count); 0 for items.
    pub experience: i32,
    /// The saved id of a feature type sent to clients as `kind`.
    pub registered: Option<String>,
    /// The online owner of a tamed animal, whose team it takes.
    pub owner_name: Option<String>,
    /// The scoreboard tags `/tag` keeps.
    pub tags: Vec<String>,
    /// The saved form, without `id`, when asked for.
    pub nbt: Option<Compound>,
}

/// How `/enchant` fared on one entity other than a player.
#[derive(Debug, Clone, PartialEq)]
pub enum EnchantAnswer {
    Done,
    /// Nothing in the main hand; the entity's name is added by the caller.
    Itemless,
    /// The item (its id) cannot take the enchantment.
    Incompatible(String),
    /// Not a living entity with hands.
    NotLiving,
}

/// The enchantments of `stack` with `enchantment` set to `level`
/// (removed at 0), or the reason it cannot be.
pub fn enchant_stack(
    stack: &ItemStack,
    enchantment: &str,
    level: i32,
) -> Result<Option<tuff_item::item::Enchantments>, EnchantAnswer> {
    let registry = tuff_item::enchantment::current();
    let Some(definition) = registry.get(enchantment) else {
        return Err(EnchantAnswer::NotLiving);
    };
    let existing = stack.components.enchantments.clone().unwrap_or_default();
    let compatible = definition.supports(stack.item())
        && existing.entries().iter().all(|(id, _)| {
            registry.get(id).is_none_or(|other| {
                id == enchantment
                    || tuff_item::enchantment::compatible((id, other), (enchantment, definition))
            })
        });
    if !compatible {
        return Err(EnchantAnswer::Incompatible(stack.item().name().to_owned()));
    }
    let mut entries: Vec<(String, i32)> = existing
        .entries()
        .iter()
        .filter(|(id, _)| id != enchantment)
        .cloned()
        .collect();
    if level > 0 {
        entries.push((enchantment.to_owned(), level));
    }
    Ok((!entries.is_empty()).then(|| tuff_item::item::Enchantments::new(entries)))
}

/// The dialog a `/dialog show` opens.
#[derive(Debug, Clone)]
pub enum DialogRef {
    Registry(i32),
    Inline(tuff_nbt::Compound),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostEffectOp {
    Add,
    Remove,
    Clear,
    List,
}
