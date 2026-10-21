// SPDX-License-Identifier: MIT
#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code, unused_imports))]
#![deny(deprecated)]
//! Bed Wars as a runtime plugin: a lobby dimension, arenas in their own
//! dimensions, teams with beds, generators and the `/bedwars` command.
//! See README.md for how to play and set it up. The game lives in
//! [`game`], free of host calls; this file binds it to the SDK.

pub mod bot;
pub mod economy;
pub mod game;
pub mod map;
pub mod shop;

use std::cell::RefCell;

use tuff_plugin_sdk::Chunk;
use tuff_plugin_sdk::command::alias as register_alias;
use tuff_plugin_sdk::prelude::*;
use tuff_plugin_sdk::types::{GameMode, Rotation, WorldError};

use crate::bot::Difficulty;
use crate::game::{BotCall, Game, Host, Id, MenuSlot, Settings, SpawnFailure, Who};
use crate::map::{Map, Pos};

/// The lobby dimension's key.
pub const LOBBY: &str = "bedwars:bedwars";
/// How many arena dimensions the plugin declares.
pub const ARENAS: usize = 4;
pub const LOBBY_SPAWN: [i32; 3] = [0, 65, 0];
const SKY: &str = "bedwars:sky";
const VOID: &str = "minecraft:the_void";
const ADMIN: &str = "bedwars.admin";
/// Adding practice bots; every player holds it unless an operator takes it.
const BOTS: &str = "bedwars.bots";
use game::{SHOPKEEPER_KIND, SHOPKEEPER_TAG};

const SKY_JSON: &str = r##"{
    "ambient_light": 0.0,
    "attributes": {
        "minecraft:gameplay/bed_rule": {
            "can_set_spawn": "never",
            "can_sleep": "never",
            "error_message": {"translate": "block.minecraft.bed.no_sleep"}
        },
        "minecraft:gameplay/respawn_anchor_works": false,
        "minecraft:visual/sky_color": "#78a7ff",
        "minecraft:visual/fog_color": "#c0d8ff"
    },
    "coordinate_scale": 1.0,
    "has_ceiling": false,
    "has_ender_dragon_fight": false,
    "has_fixed_time": true,
    "has_skylight": true,
    "height": 256,
    "infiniburn": "#minecraft:infiniburn_overworld",
    "logical_height": 256,
    "min_y": 0,
    "monster_spawn_block_light_limit": 0,
    "monster_spawn_light_level": 0,
    "skybox": "overworld"
}"##;

pub fn arena_dimension(number: usize) -> String {
    format!("bedwars:arena_{number}")
}

static MAPS: Setting<String> = Setting::new(
    "maps",
    "",
    "Extra arena maps as JSON (a list of maps); see the plugin README",
);
static MIN_PLAYERS: Setting<i64> = Setting::new(
    "minPlayers",
    2,
    "Queued players needed to start the countdown",
);
static COUNTDOWN: Setting<i64> = Setting::new(
    "countdownSeconds",
    20,
    "Seconds from enough players to the start",
);
static RESPAWN: Setting<i64> = Setting::new("respawnSeconds", 5, "Seconds dead players wait");
static LOBBY_ON_JOIN: Setting<bool> = Setting::new(
    "lobbyOnJoin",
    false,
    "Send players to the bedwars lobby dimension when they join the server",
);

/// Arena numbers and their maps, one `number map` per line.
static ARENA_LIST: Stored<String> = Stored::new("arenas");
/// A player's state from before they joined a game.
static SAVED: StoredMap<String> = StoredMap::new("saved");
/// Arenas holding a game's leftovers (cleared before they are built).
static DIRTY: StoredMap<bool> = StoredMap::new("dirty");
/// Arenas that were built at least once.
static USED: StoredMap<bool> = StoredMap::new("used");

thread_local! {
    static GAME: RefCell<Game> = RefCell::new(Game::default());
    /// The block rule guarding each running arena's box.
    static RULES: RefCell<std::collections::BTreeMap<usize, u32>> = RefCell::default();
}

fn with_game<T>(f: impl FnOnce(&mut Game, &mut dyn Host) -> T) -> T {
    let (out, calls) = GAME.with(|g| {
        let mut game = g.borrow_mut();
        let out = f(&mut game, &mut Sdk);
        (out, std::mem::take(&mut game.bot_calls))
    });
    for call in calls {
        send_bot_call(call);
    }
    out
}

fn bots_loaded() -> bool {
    tuff_plugin_sdk::messaging::plugin_loaded("bots").is_some()
}

/// Sends one request to the bots plugin; a spawn's answer goes back to
/// the game.
fn send_bot_call(call: BotCall) {
    use tuff_bots_api::control::Control;
    use tuff_plugin_sdk::wire::Error;
    match call {
        BotCall::Spawn(n, spec) => tuff_plugin_sdk::exec::spawn(async move {
            let result = Control::any().spawn(spec).await.map_err(|e| match e {
                Error::Call(CallError::NoProvider) => SpawnFailure::NoProvider,
                other => SpawnFailure::Refused(format!("{other:?}")),
            });
            if let Err(SpawnFailure::Refused(why)) = &result {
                log(
                    Level::Warn,
                    &format!("the bots plugin refused a bot: {why}"),
                );
            }
            with_game(|game, host| game.bot_spawned(host, n, result));
        }),
        BotCall::Remove(bot) => tuff_plugin_sdk::exec::spawn(async move {
            let _ = Control::any().remove(bot).await;
        }),
        BotCall::Process(bot, process) => tuff_plugin_sdk::exec::spawn(async move {
            let _ = Control::any().set_process(bot, process).await;
        }),
    }
}

/// Hears what the bots plugin says about Bedwars' bots.
struct BotEvents;

impl tuff_bots_api::events::Serve for BotEvents {
    fn spawned(&mut self, _from: &str, _bot: u32) {}
    fn died(&mut self, _from: &str, _bot: u32, _cause: String) {}
    fn respawned(&mut self, _from: &str, _bot: u32) {}
    fn process_done(&mut self, _from: &str, bot: u32) {
        with_game(|game, _| game.bot_walk_ended(bot));
    }
    fn process_failed(&mut self, _from: &str, bot: u32, _reason: String) {
        with_game(|game, _| game.bot_walk_ended(bot));
    }
    fn removed(&mut self, _from: &str, bot: u32) {
        with_game(|game, host| game.bot_removed(host, bot));
    }
}

fn uuid(id: Id) -> Uuid {
    Uuid::from_u128(id)
}

fn player(id: Id) -> Player {
    Player {
        uuid: uuid(id),
        name: String::new(),
    }
}

fn hex(id: Id) -> String {
    format!("{id:032x}")
}

fn who(player: &Player) -> Who {
    Who::new(player.uuid.as_u128(), &player.name)
}

fn settings() -> Settings {
    let seconds = |s: &Setting<i64>| s.get().clamp(0, 3600) as u64 * economy::SECOND;
    Settings {
        min_players: MIN_PLAYERS.get().clamp(2, 64) as usize,
        countdown: seconds(&COUNTDOWN),
        respawn: seconds(&RESPAWN),
        ..Settings::default()
    }
}

fn maps() -> Vec<Map> {
    map::all(&MAPS.get()).unwrap_or_else(|err| {
        log(
            Level::Warn,
            &format!("ignoring plugins.bedwars.maps: {err}"),
        );
        map::builtin()
    })
}

fn arena_list() -> Vec<(usize, String)> {
    let text = ARENA_LIST.get().unwrap_or_else(|| "1 classic".to_owned());
    text.lines()
        .filter_map(|line| {
            let (n, map) = line.split_once(' ')?;
            let n: usize = n.parse().ok()?;
            (1..=ARENAS).contains(&n).then(|| (n, map.to_owned()))
        })
        .collect()
}

fn save_arena_list(game: &Game) {
    let text: Vec<String> = game
        .arenas
        .iter()
        .map(|a| format!("{} {}", a.number, a.map.name))
        .collect();
    if let Err(err) = ARENA_LIST.set(&text.join("\n")) {
        log(Level::Warn, &format!("saving the arena list: {err}"));
    }
}

/// Whether the owner granted the arena dimensions.
fn dimension_ready(key: &str) -> bool {
    match World::new(key).block(pos(0, 0, 0)) {
        Err(WorldError::UnknownDimension) => {
            log(
                Level::Warn,
                &format!(
                    "{key} does not exist: grant it with `tuff plugin grant bedwars \
                     cap.dimension bedwars:*` and restart"
                ),
            );
            false
        }
        _ => true,
    }
}

/// Sets up arena `number` with `map` (clearing what an earlier game or map
/// left there) and remembers it.
fn set_up(game: &mut Game, number: usize, map: Map) -> Result<(), String> {
    let dimension = arena_dimension(number);
    if !dimension_ready(&dimension) {
        return Err(format!("{dimension} does not exist; see the server log"));
    }
    let dirty = DIRTY.get(number).unwrap_or(false) || USED.get(number).unwrap_or(false);
    game.add_arena(number, &dimension, map, dirty)?;
    let _ = DIRTY.set(number, &true);
    save_arena_list(game);
    Ok(())
}

fn enable_game() {
    let found = maps();
    with_game(|game, _| {
        *game = Game::new(settings());
        game.bots_present = bots_loaded();
        for (number, name) in arena_list() {
            let Some(map) = found.iter().find(|m| m.name == name) else {
                log(Level::Warn, &format!("arena {number}: no map named {name}"));
                continue;
            };
            let dimension = arena_dimension(number);
            if dimension_ready(&dimension) {
                let dirty = DIRTY.get(number).unwrap_or(false);
                if let Err(err) = game.add_arena(number, &dimension, map.clone(), dirty) {
                    log(Level::Warn, &err);
                }
            }
        }
    });
}

/// Item entities around `at` holding `item`, with their stack counts.
fn items_around(dimension: &str, at: [f64; 3], item: &str) -> Vec<(EntityRef, u32)> {
    let world = World::new(dimension);
    let lo = vec3(at[0] - 1.5, at[1] - 1.0, at[2] - 1.5);
    let hi = vec3(at[0] + 1.5, at[1] + 2.0, at[2] + 1.5);
    let Ok(found) = world.entities(lo, hi) else {
        return Vec::new();
    };
    let quoted = format!("\"{item}\"");
    found
        .iter()
        .filter(|e| e.kind == "minecraft:item")
        .filter_map(|e| {
            let entity = EntityRef::from(e);
            let data = entity.data().ok()??;
            data.contains(&quoted).then(|| (entity, stack_count(&data)))
        })
        .collect()
}

/// The `count` of an item entity's SNBT, 1 when missing.
pub fn stack_count(snbt: &str) -> u32 {
    let Some(start) = snbt.find("count:") else {
        return 1;
    };
    let digits: String = snbt[start + 6..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().unwrap_or(1)
}

fn sweep_now(dimension: &str, at: Pos, item: &str) {
    let center = [at[0] as f64 + 0.5, at[1] as f64, at[2] as f64 + 0.5];
    for (entity, _) in items_around(dimension, center, item) {
        let _ = entity.remove();
    }
}

/// The SDK as the game's [`Host`].
struct Sdk;

impl Host for Sdk {
    fn tell(&mut self, to: Id, text: &str) {
        let _ = player(to).tell(text);
    }

    fn set_block(&mut self, dimension: &str, at: Pos, state: &str) {
        let _ = World::new(dimension).set_block_quietly(pos(at[0], at[1], at[2]), state);
    }

    fn teleport(&mut self, who: Id, dimension: &str, at: [f64; 3], yaw: f32) {
        let rotation = Rotation { yaw, pitch: 0.0 };
        let _ = player(who).teleport(
            &World::new(dimension),
            vec3(at[0], at[1], at[2]),
            Some(rotation),
        );
    }

    fn spawn_item(&mut self, dimension: &str, at: [f64; 3], item: &str) {
        let snbt = format!("{{Item:{{id:\"{item}\",count:1}},PickupDelay:10s}}");
        let _ =
            World::new(dimension).spawn("minecraft:item", vec3(at[0], at[1], at[2]), Some(&snbt));
    }

    fn items_near(&mut self, dimension: &str, at: [f64; 3], item: &str) -> u32 {
        items_around(dimension, at, item)
            .iter()
            .map(|(_, n)| n)
            .sum()
    }

    fn sweep(&mut self, dimension: &str, at: Pos, item: &str) {
        sweep_now(dimension, at, item);
        let (dimension, item) = (dimension.to_owned(), item.to_owned());
        after(2, move || sweep_now(&dimension, at, &item));
    }

    fn give_back(&mut self, item: &str) {
        let _ = acting::give(item, 1);
    }

    fn save_state(&mut self, who: Id) {
        let Some(at) = acting::location() else {
            return;
        };
        let (yaw, pitch) = at.rotation.map_or((0.0, 0.0), |r| (r.yaw, r.pitch));
        let mut text = format!(
            "{} {} {} {} {} {}",
            at.dimension, at.pos.x, at.pos.y, at.pos.z, yaw, pitch
        );
        if let Some(state) = tuff_plugin_sdk::players::state(&hex(who)) {
            let mode = match state.game_mode {
                GameMode::Survival => "survival",
                GameMode::Creative => "creative",
                GameMode::Adventure => "adventure",
                GameMode::Spectator => "spectator",
            };
            text.push_str(&format!(
                "\n{mode} {} {} {} {}",
                state.health, state.food, state.xp_level, state.xp_points
            ));
        }
        for (slot, stack) in self.carried() {
            text.push_str(&format!("\n{slot} {stack}"));
        }
        let _ = SAVED.set(uuid(who), &text);
    }

    fn restore_state(&mut self, who: Id) {
        self.keep_inventory(who, false);
        let Some(text) = SAVED.get(uuid(who)) else {
            return;
        };
        let _ = SAVED.remove(uuid(who));
        let Some(place) = Saved::parse(&text) else {
            return;
        };
        let rotation = Rotation {
            yaw: place.yaw,
            pitch: place.pitch,
        };
        let _ = player(who).teleport(
            &World::new(&place.dimension),
            vec3(place.at[0], place.at[1], place.at[2]),
            Some(rotation),
        );
        let id = hex(who);
        let _ = player(who).clear_inventory();
        for (slot, stack) in &place.slots {
            let _ = player(who).set_slot(*slot, Some(stack));
        }
        if let Some(v) = place.vitals {
            let mode = match v.mode.as_str() {
                "creative" => GameMode::Creative,
                "adventure" => GameMode::Adventure,
                "spectator" => GameMode::Spectator,
                _ => GameMode::Survival,
            };
            let _ = tuff_plugin_sdk::players::set_game_mode(&id, mode);
            let _ = tuff_plugin_sdk::players::set_vitals(&id, Some(v.health), Some(v.food));
            let _ = tuff_plugin_sdk::players::set_xp(&id, v.level, v.points);
        }
    }

    fn open_menu(&mut self, who: Id, menu: &str, rows: u8, title: &str, items: &[MenuSlot]) {
        let items: Vec<_> = items
            .iter()
            .map(|s| {
                let mut item = tuff_plugin_sdk::menu::item(s.slot, &s.item, s.count).named(&s.name);
                for line in &s.lore {
                    item = item.lore(line);
                }
                if s.glint {
                    item = item.components("{\"minecraft:enchantment_glint_override\":1b}");
                }
                item
            })
            .collect();
        let _ = tuff_plugin_sdk::menu::open(&hex(who), menu, rows, title, &items);
    }

    fn carried(&mut self) -> Vec<(u8, String)> {
        acting::inventory_snbt().unwrap_or_default()
    }

    fn take(&mut self, who: Id, item: &str, count: u32) {
        let _ = player(who).take(item, count, true);
    }

    fn give(&mut self, who: Id, item: &str, count: u32) {
        let _ = player(who).give(item, count);
    }

    fn set_slot(&mut self, who: Id, slot: u8, stack: Option<&str>) {
        let _ = player(who).set_slot(slot, stack);
    }

    fn clear(&mut self, who: Id) {
        let _ = player(who).clear_inventory();
    }

    fn keep_inventory(&mut self, who: Id, keep: bool) {
        let _ = player(who).set_keep_inventory(keep.then_some(true));
    }

    fn effect(&mut self, who: Id, effect: &str, ticks: i32, amplifier: u8) {
        let _ = tuff_plugin_sdk::players::add_effect(&hex(who), effect, ticks, amplifier, false);
    }

    fn position(&mut self, who: Id) -> Option<[f64; 3]> {
        let at = tuff_plugin_sdk::players::location(&hex(who))?;
        Some([at.pos.x, at.pos.y, at.pos.z])
    }

    fn gaze(&mut self, who: Id) -> Option<(String, [f64; 3], [f64; 3])> {
        let at = tuff_plugin_sdk::players::location(&hex(who))?;
        let rotation = at.rotation?;
        let (yaw, pitch) = (
            f64::from(rotation.yaw).to_radians(),
            f64::from(rotation.pitch).to_radians(),
        );
        let dir = [
            -yaw.sin() * pitch.cos(),
            -pitch.sin(),
            yaw.cos() * pitch.cos(),
        ];
        let eye = [at.pos.x, at.pos.y + at.eye_height, at.pos.z];
        Some((at.dimension, eye, dir))
    }

    fn launch(&mut self, dimension: &str, kind: &str, at: [f64; 3], snbt: &str) -> Option<Id> {
        let entity = World::new(dimension)
            .spawn(kind, vec3(at[0], at[1], at[2]), Some(snbt))
            .ok()?;
        Some(entity.uuid().as_u128())
    }

    fn entity_at(&mut self, entity: Id) -> Option<[f64; 3]> {
        let data = EntityRef(uuid(entity)).data().ok()??;
        let start = data.find("Pos:[")? + 5;
        let mut parts = data[start..].split(']').next()?.split(',');
        let mut next = || {
            parts
                .next()?
                .trim()
                .trim_end_matches('d')
                .parse::<f64>()
                .ok()
        };
        Some([next()?, next()?, next()?])
    }

    fn block_at(&mut self, dimension: &str, at: Pos) -> Option<String> {
        World::new(dimension)
            .block(pos(at[0], at[1], at[2]))
            .ok()
            .map(|state| state.split('[').next().unwrap_or(&state).to_owned())
    }

    fn player_named(&mut self, name: &str) -> Option<Id> {
        tuff_plugin_sdk::players::online()
            .into_iter()
            .find(|p| p.name == name)
            .map(|p| p.uuid.as_u128())
    }

    fn remove_entity(&mut self, entity: Id) {
        let _ = EntityRef(uuid(entity)).remove();
    }

    fn clear_shopkeepers(&mut self, dimension: &str, bounds: [Pos; 2]) {
        let [lo, hi] = bounds;
        let lo = vec3(f64::from(lo[0]), f64::from(lo[1]), f64::from(lo[2]));
        let hi = vec3(
            f64::from(hi[0] + 1),
            f64::from(hi[1] + 1),
            f64::from(hi[2] + 1),
        );
        let Ok(found) = World::new(dimension).entities(lo, hi) else {
            return;
        };
        let tag = format!("\"{SHOPKEEPER_TAG}\"");
        for e in found.iter().filter(|e| e.kind == SHOPKEEPER_KIND) {
            let entity = EntityRef::from(e);
            if entity
                .data()
                .ok()
                .flatten()
                .is_some_and(|d| d.contains(&tag))
            {
                let _ = entity.remove();
            }
        }
    }

    fn protect(&mut self, arena: usize, dimension: &str, bounds: [Pos; 2], beds: &[String]) {
        self.unprotect(arena);
        let [lo, hi] = bounds;
        let rule = tuff_plugin_sdk::rules::BlockRule::protect(
            dimension,
            (lo[0], lo[1], lo[2]),
            (hi[0], hi[1], hi[2]),
        )
        .breakable(beds.iter().map(String::as_str))
        .blast_proof(game::BLAST_PROOF);
        match tuff_plugin_sdk::rules::add_block_rule(&rule) {
            Ok(id) => {
                RULES.with(|r| r.borrow_mut().insert(arena, id));
            }
            Err(err) => log(Level::Warn, &format!("arena {arena} is unguarded: {err}")),
        }
    }

    fn unprotect(&mut self, arena: usize) {
        if let Some(id) = RULES.with(|r| r.borrow_mut().remove(&arena)) {
            tuff_plugin_sdk::rules::remove_block_rule(id);
        }
    }

    fn title(&mut self, who: Id, title: &str, subtitle: &str) {
        let _ = player(who).show_title(&Title::new(title).subtitle(subtitle).times(5, 40, 10));
    }

    fn action_bar(&mut self, who: Id, text: &str) {
        let _ = player(who).action_bar(text);
    }

    fn sidebar(&mut self, lines: Option<(&str, &[String])>) {
        match lines {
            Some((title, lines)) => {
                let _ = Sidebar::new(title)
                    .lines(lines.iter().map(Span::from))
                    .show();
            }
            None => {
                let _ = Sidebar::clear();
            }
        }
    }

    fn spectate(&mut self, who: Id, on: bool) {
        let id = hex(who);
        if on {
            let _ = tuff_plugin_sdk::players::respawn(&id);
            let _ = tuff_plugin_sdk::players::set_game_mode(&id, GameMode::Spectator);
        } else {
            let _ = tuff_plugin_sdk::players::set_game_mode(&id, GameMode::Survival);
            let _ = tuff_plugin_sdk::players::set_vitals(&id, Some(20.0), Some(20));
        }
    }

    fn mark_dirty(&mut self, arena: usize, dirty: bool) {
        let _ = match dirty {
            true => DIRTY.set(arena, &true),
            false => DIRTY.remove(arena).and_then(|()| USED.set(arena, &true)),
        };
    }
}

/// Where a player was before a game, as saved.
#[derive(Debug, PartialEq)]
pub struct Saved {
    pub dimension: String,
    pub at: [f64; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub vitals: Option<Vitals>,
    pub slots: Vec<(u8, String)>,
}

/// The survival state saved with a place.
#[derive(Debug, PartialEq)]
pub struct Vitals {
    pub mode: String,
    pub health: f32,
    pub food: i32,
    pub level: i32,
    pub points: i32,
}

impl Vitals {
    fn parse(line: &str) -> Option<Vitals> {
        let mut words = line.split(' ');
        let mode = words.next()?.to_owned();
        let health = words.next()?.parse().ok()?;
        let food = words.next()?.parse().ok()?;
        let level = words.next()?.parse().ok()?;
        let points = words.next()?.parse().ok()?;
        Some(Vitals {
            mode,
            health,
            food,
            level,
            points,
        })
    }
}

impl Saved {
    pub fn parse(text: &str) -> Option<Saved> {
        let mut words = text.lines().next()?.split(' ');
        let dimension = words.next()?.to_owned();
        let mut num = || words.next()?.parse::<f64>().ok();
        let at = [num()?, num()?, num()?];
        let (yaw, pitch) = (num()? as f32, num()? as f32);
        let mut lines = text.lines().skip(1).peekable();
        let vitals = lines
            .next_if(|l| l.split(' ').count() == 5)
            .and_then(Vitals::parse);
        let slots = lines
            .filter_map(|l| {
                let (slot, stack) = l.split_once(' ')?;
                Some((slot.parse().ok()?, stack.to_owned()))
            })
            .collect();
        Some(Saved {
            dimension,
            at,
            yaw,
            pitch,
            vitals,
            slots,
        })
    }
}

fn arena_arg() -> Node {
    argument("arena", parser::integer_in(1..=ARENAS as i32))
}

type Node = tuff_plugin_sdk::command::Node;

fn number(ctx: &Ctx) -> Result<Option<usize>, String> {
    Ok(ctx.get::<Option<i32>>("arena")?.map(|n| n as usize))
}

/// The arena an admin command names, or the sender's.
fn target(ctx: &Ctx) -> Result<usize, String> {
    if let Some(n) = number(ctx)? {
        return Ok(n);
    }
    let id = ctx.player()?.uuid.as_u128();
    with_game(|game, _| game.arena_of(id).map(|a| a.number))
        .ok_or_else(|| "Name an arena".to_owned())
}

fn answer(ctx: &Ctx, result: Result<String, String>) -> Outcome {
    let line = result?;
    ctx.reply(text!(line.as_str().yellow()))?;
    Ok(1)
}

fn join(ctx: Ctx) -> Outcome {
    let player = who(ctx.player()?);
    let n = number(&ctx)?;
    let result = with_game(|game, host| game.join(host, player, n));
    answer(&ctx, result)
}

fn leave(ctx: Ctx) -> Outcome {
    let id = ctx.player()?.uuid.as_u128();
    let result = with_game(|game, host| game.leave(host, id, true));
    answer(&ctx, result)
}

fn list(ctx: Ctx) -> Outcome {
    let lines = with_game(|game, _| game.list());
    if lines.is_empty() {
        ctx.reply("No arenas; an admin adds one with /bedwars create <map>")?;
    }
    for line in lines {
        ctx.reply(line)?;
    }
    Ok(1)
}

fn status(ctx: Ctx) -> Outcome {
    let n = target(&ctx)?;
    for line in with_game(|game, _| game.status(n))? {
        ctx.reply(line)?;
    }
    Ok(1)
}

fn show_maps(ctx: Ctx) -> Outcome {
    for map in maps() {
        let teams: Vec<String> = map
            .teams
            .iter()
            .map(|t| text!(t.name.as_str().color(t.chat_color())))
            .collect();
        ctx.reply(text!(
            map.name.as_str().white(),
            format!(" {}x{}: ", map.teams.len(), map.team_size).gray(),
            teams.join(", ")
        ))?;
    }
    Ok(1)
}

fn show_shop(ctx: Ctx) -> Outcome {
    let category: Option<String> = ctx.get("category")?;
    let Some(category) = category else {
        let names: Vec<&str> = shop::Category::ALL.iter().map(|c| c.name()).collect();
        ctx.reply(text!(
            "Shop: ".gold(),
            names.join(", "),
            " (/bedwars shop <category>)".gray()
        ))?;
        let upgrades: Vec<&str> = shop::UPGRADES.iter().map(|u| u.name).collect();
        ctx.reply(text!("Team upgrades: ".gold(), upgrades.join(", ")))?;
        return Ok(1);
    };
    let category = shop::Category::of(&category).ok_or("No such category")?;
    for offer in shop::OFFERS.iter().filter(|o| o.category == category) {
        let price = offer.price;
        ctx.reply(text!(
            offer.name.white(),
            format!(" x{} ", offer.count).gray(),
            format!("{} {}", price.amount, price.resource.name()).color(price.resource.color())
        ))?;
    }
    Ok(1)
}

fn lobby(ctx: Ctx) -> Outcome {
    let player = ctx.player()?;
    if with_game(|game, _| game.arena_of(player.uuid.as_u128()).is_some()) {
        return Err("Leave your game first with /bedwars leave".into());
    }
    let [x, y, z] = LOBBY_SPAWN;
    player
        .teleport(
            &World::new(LOBBY),
            vec3(x as f64 + 0.5, y as f64, z as f64 + 0.5),
            None,
        )
        .map_err(|e| format!("The lobby is not available: {e:?}"))?;
    Ok(1)
}

fn start(ctx: Ctx) -> Outcome {
    let n = target(&ctx)?;
    let result = with_game(|game, host| game.start(host, n));
    answer(&ctx, result)
}

fn stop(ctx: Ctx) -> Outcome {
    let n = target(&ctx)?;
    let result = with_game(|game, host| game.stop(host, n));
    answer(&ctx, result)
}

fn difficulty(ctx: &Ctx) -> Result<Difficulty, String> {
    match ctx.get::<Option<String>>("difficulty")? {
        None => Ok(Difficulty::Normal),
        Some(name) => Difficulty::parse(&name).ok_or_else(|| format!("Unknown difficulty {name}")),
    }
}

fn bot_add(ctx: Ctx) -> Outcome {
    let n = number(&ctx)?;
    let level = difficulty(&ctx)?;
    let result = with_game(|game, _| {
        game.bots_present = bots_loaded();
        game.add_bot(n, level)
    });
    answer(&ctx, result)
}

fn bot_fill(ctx: Ctx) -> Outcome {
    let n = target(&ctx)?;
    let level = difficulty(&ctx)?;
    let result = with_game(|game, _| {
        game.bots_present = bots_loaded();
        game.fill_bots(n, level)
    })
    .map(|added| format!("Adding {added} bots to arena {n}"));
    answer(&ctx, result)
}

fn bot_remove(ctx: Ctx) -> Outcome {
    let n = target(&ctx)?;
    let result = with_game(|game, host| game.remove_bots(host, n))
        .map(|gone| format!("Removed {gone} bots from arena {n}"));
    answer(&ctx, result)
}

fn bot_command() -> Node {
    let levels = |_: &tuff_plugin_sdk::command::Suggest| {
        Difficulty::ALL
            .iter()
            .map(|d| d.name().to_owned())
            .collect::<Vec<_>>()
    };
    let level = || argument("difficulty", parser::word()).suggests(levels);
    literal("bot")
        .about("Adds or removes practice bots")
        .requires(BOTS, 0)
        .then(
            literal("add")
                .about("Queues a bot in arena <arena> (easy, normal or hard)")
                .executes(bot_add)
                .then(
                    arena_arg()
                        .executes(bot_add)
                        .then(level().executes(bot_add)),
                ),
        )
        .then(
            literal("fill")
                .about("Fills an arena's free places with bots")
                .executes(bot_fill)
                .then(
                    arena_arg()
                        .executes(bot_fill)
                        .then(level().executes(bot_fill)),
                ),
        )
        .then(
            literal("remove")
                .about("Takes every bot out of an arena")
                .executes(bot_remove)
                .then(arena_arg().executes(bot_remove)),
        )
}

fn create(ctx: Ctx) -> Outcome {
    let name: String = ctx.get("map")?;
    let map = maps()
        .into_iter()
        .find(|m| m.name == name)
        .ok_or_else(|| format!("No map named {name}; see /bedwars maps"))?;
    let result = with_game(|game, _| {
        let n = match number(&ctx)? {
            Some(n) => n,
            None => (1..=ARENAS)
                .find(|n| game.arena(*n).is_none())
                .ok_or("Every arena is in use; remove one first")?,
        };
        set_up(game, n, map)?;
        Ok(format!(
            "Arena {n} is being built with {name}; join with /bedwars join {n}"
        ))
    });
    answer(&ctx, result)
}

fn remove(ctx: Ctx) -> Outcome {
    let n = number(&ctx)?.ok_or("Name an arena")?;
    let result = with_game(|game, host| {
        game.remove_arena(host, n)?;
        save_arena_list(game);
        Ok(format!("Removed arena {n}"))
    });
    answer(&ctx, result)
}

fn register_commands() -> Result<(), String> {
    let map_names = |_: &tuff_plugin_sdk::command::Suggest| {
        maps().into_iter().map(|m| m.name).collect::<Vec<_>>()
    };
    let categories = |_: &tuff_plugin_sdk::command::Suggest| {
        shop::Category::ALL
            .iter()
            .map(|c| c.name().to_owned())
            .collect::<Vec<_>>()
    };
    command("bedwars")
        .about("Bed Wars")
        .then(
            literal("join")
                .about("Joins the fullest open arena, or arena <arena>")
                .executes(join)
                .then(arena_arg().executes(join)),
        )
        .then(
            literal("leave")
                .about("Leaves your queue or game")
                .executes(leave),
        )
        .then(literal("list").about("Lists the arenas").executes(list))
        .then(
            literal("status")
                .about("Shows teams, beds and the timeline of a game")
                .executes(status)
                .then(arena_arg().executes(status)),
        )
        .then(literal("maps").about("Lists the maps").executes(show_maps))
        .then(
            literal("shop")
                .about("Lists the shop's offers and prices")
                .executes(show_shop)
                .then(
                    argument("category", parser::word())
                        .suggests(categories)
                        .executes(show_shop),
                ),
        )
        .then(
            literal("lobby")
                .about("Goes to the Bed Wars lobby")
                .executes(lobby),
        )
        .then(
            literal("start")
                .about("Starts a waiting arena now")
                .requires(ADMIN, 2)
                .executes(start)
                .then(arena_arg().executes(start)),
        )
        .then(
            literal("stop")
                .about("Stops a game with no winner")
                .permission(ADMIN)
                .executes(stop)
                .then(arena_arg().executes(stop)),
        )
        .then(
            literal("create")
                .about("Builds a map in a free arena, or arena <arena>")
                .permission(ADMIN)
                .then(
                    argument("map", parser::word())
                        .suggests(map_names)
                        .executes(create)
                        .then(arena_arg().executes(create)),
                ),
        )
        .then(bot_command())
        .then(
            literal("remove")
                .about("Ends an arena's game and drops the arena")
                .permission(ADMIN)
                .then(arena_arg().executes(remove)),
        )
        .help()
        .register()?;
    if register_alias("bw", "bedwars").is_err() {
        log(Level::Warn, "could not claim /bw");
    }
    Ok(())
}

fn register_events() -> Result<(), String> {
    on::<ev::Join>(|player| {
        let id = player.uuid.as_u128();
        if with_game(|game, _| game.arena_of(id).is_some()) {
            return;
        }
        if SAVED.get(player.uuid).is_some() {
            Sdk.restore_state(id);
            let _ = player.tell(text!(
                "You were put back where you were before your game.".yellow()
            ));
        } else if LOBBY_ON_JOIN.get() {
            let [x, y, z] = LOBBY_SPAWN;
            let _ = player.teleport(
                &World::new(LOBBY),
                vec3(x as f64 + 0.5, y as f64, z as f64 + 0.5),
                None,
            );
        }
    })?;
    on::<ev::Leave>(|player| {
        let id = player.uuid.as_u128();
        with_game(|game, host| game.disconnected(host, id));
    })?;
    on::<ev::BlockBreak>(|change| {
        let p = change.pos;
        let who = change.player.as_ref().map(who);
        with_game(|game, host| {
            game.broken(
                host,
                who.as_ref(),
                &change.dimension,
                [p.x, p.y, p.z],
                &change.state,
            )
        });
    })?;
    on::<ev::BlockPlace>(|change| {
        let p = change.pos;
        let who = change.player.as_ref().map(who);
        with_game(|game, host| {
            game.placed(
                host,
                who.as_ref(),
                &change.dimension,
                [p.x, p.y, p.z],
                &change.state,
            )
        });
    })?;
    tuff_plugin_sdk::events::intercept_click(
        tuff_plugin_sdk::ClickKind::Air,
        game::FIRE_CHARGE,
        "",
        "",
    )?;
    tuff_plugin_sdk::events::intercept_click(tuff_plugin_sdk::ClickKind::Air, game::EGG, "", "")?;
    on::<ev::UseItem>(|used| {
        let who = who(&used.player);
        let rotation = [used.rotation.yaw, used.rotation.pitch];
        with_game(|game, host| game.used(host, &who, &used.dimension, &used.item, rotation));
    })?;
    on::<ev::PlayerDeath>(|death| {
        let id = death.player.uuid.as_u128();
        let credit = tuff_plugin_sdk::rules::acting_damage()
            .filter(|d| d.credit_is_player)
            .and_then(|d| d.credit)
            .map(|u| u.as_u128());
        with_game(|game, host| game.died_by(host, id, credit));
    })?;
    on::<ev::ItemPickup>(|picked| {
        let id = picked.player.uuid.as_u128();
        with_game(|game, host| game.bot_picked_up(host, id));
    })?;
    tuff_plugin_sdk::menu::intercept_entity_interact(Some(SHOPKEEPER_KIND), Some(SHOPKEEPER_TAG))?;
    tuff_plugin_sdk::combat::attack()
        .victim_kind(SHOPKEEPER_KIND)
        .victim_tag(SHOPKEEPER_TAG)
        .cancel()?;
    on::<ev::EntityInteract>(|hit| {
        if hit.kind == SHOPKEEPER_KIND && hit.cancelled {
            let id = hit.player.uuid.as_u128();
            with_game(|game, host| game.open_shop(host, id, shop::Category::Blocks));
        }
    })?;
    on::<ev::InventoryOpen>(|window| {
        if window.container == "chest" {
            let id = window.player.uuid.as_u128();
            with_game(|game, host| game.container_opened(host, id));
        }
    })?;
    on::<ev::MenuClick>(|click| {
        use tuff_plugin_sdk::menu::MenuClickKind::{Left, Right, ShiftLeft};
        if !matches!(click.click, Left | Right | ShiftLeft) {
            return;
        }
        let id = click.player.uuid.as_u128();
        with_game(|game, host| game.menu_click(host, id, &click.menu, click.slot));
    })?;
    Ok(())
}

struct Bedwars;

impl Plugin for Bedwars {
    fn register() -> Result<(), String> {
        MAPS.declare()?;
        MIN_PLAYERS.declare()?;
        COUNTDOWN.declare()?;
        RESPAWN.declare()?;
        LOBBY_ON_JOIN.declare()?;
        for setting in [&MIN_PLAYERS, &COUNTDOWN, &RESPAWN] {
            setting.on_change(|_| with_game(|game, _| game.settings = settings()))?;
        }
        MAPS.on_change(|json| {
            if let Err(err) = map::all(&json) {
                log(Level::Warn, &format!("plugins.bedwars.maps: {err}"));
            }
        })?;
        tuff_bots_api::events::serve(BotEvents).map_err(|e| format!("{e:?}"))?;
        register_commands()?;
        register_events()?;
        every(1, || with_game(|game, host| game.tick(host)));
        Ok(())
    }

    fn enable() {
        enable_game();
    }

    fn disable() {
        with_game(|game, host| game.shutdown(host));
    }

    fn plugin_change(id: &str, _version: &str, loaded: bool) {
        if id == "bots" {
            with_game(|game, host| match loaded {
                true => game.bots_present = true,
                false => game.bots_unloaded(host),
            });
        }
    }

    fn worldgen(world: &mut Worldgen) {
        let decl = |key: String, spawn: [i32; 3]| {
            DimensionDef::new(key)
                .dimension_type_json(SKY, SKY_JSON)
                .spawn(spawn[0], spawn[1], spawn[2])
        };
        world.dimension(decl(LOBBY.into(), LOBBY_SPAWN), |a| {
            Ok(Chunk {
                palette: LOBBY_PALETTE.iter().map(|s| (*s).to_owned()).collect(),
                blocks: lobby_chunk(a.chunk_x, a.chunk_z, a.min_y, a.height),
                biome: VOID.into(),
            })
        });
        for n in 1..=ARENAS {
            world.dimension(decl(arena_dimension(n), [0, 110, 0]), |a| {
                Ok(Chunk {
                    palette: LOBBY_PALETTE.iter().map(|s| (*s).to_owned()).collect(),
                    blocks: vec![0; a.height.max(0) as usize * 256],
                    biome: VOID.into(),
                })
            });
        }
    }
}

const LOBBY_PALETTE: [&str; 3] = [
    "minecraft:air",
    "minecraft:smooth_stone",
    "minecraft:red_concrete",
];
const LOBBY_RADIUS: i32 = 12;

/// The lobby: a smooth stone platform with a red rim under the spawn.
pub fn lobby_chunk(chunk_x: i32, chunk_z: i32, min_y: i32, height: i32) -> Vec<u8> {
    let mut blocks = vec![0u8; height.max(0) as usize * 256];
    let y = LOBBY_SPAWN[1] - 1 - min_y;
    if !(0..height).contains(&y) {
        return blocks;
    }
    for lz in 0..16 {
        for lx in 0..16 {
            let (x, z) = (chunk_x * 16 + lx, chunk_z * 16 + lz);
            let edge = x.abs().max(z.abs());
            if edge <= LOBBY_RADIUS {
                let kind = if edge == LOBBY_RADIUS { 2 } else { 1 };
                blocks[(y * 256 + lz * 16 + lx) as usize] = kind;
            }
        }
    }
    blocks
}

#[cfg(target_arch = "wasm32")]
plugin!(Bedwars {
    id: "bedwars",
    version: "0.1.0",
    description: "Bed Wars: teams, beds, generators and arenas in their own dimensions",
    spawn_dimension: "bedwars:bedwars",
    soft_depends: "bots",
    requests: [
        "cap.dimension = bedwars:* -- the lobby and arena dimensions",
        "cap.world-write = all -- builds arenas and spawns items",
        "cap.player-write = all -- teleports players, sets modes and kits",
        "cap.player-read = all -- lists players for games and kits",
        "cap.cross-plugin = all -- asks the bots plugin for practice bots",
    ],
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_places_and_stack_counts_parse() {
        let saved = Saved::parse(
            "minecraft:overworld 1.5 64 -3.25 90 -10\nadventure 7.5 18 3 12\n0 {id:\"minecraft:stone\",count:3}",
        );
        assert_eq!(
            saved,
            Some(Saved {
                dimension: "minecraft:overworld".into(),
                at: [1.5, 64.0, -3.25],
                yaw: 90.0,
                pitch: -10.0,
                vitals: Some(Vitals {
                    mode: "adventure".into(),
                    health: 7.5,
                    food: 18,
                    level: 3,
                    points: 12,
                }),
                slots: vec![(0, "{id:\"minecraft:stone\",count:3}".into())],
            })
        );
        assert_eq!(Saved::parse("minecraft:overworld 1 2"), None);
        assert_eq!(
            stack_count("{Item:{count:17,id:\"minecraft:iron_ingot\"},PickupDelay:0s}"),
            17
        );
        assert_eq!(stack_count("{Item:{id:\"minecraft:iron_ingot\"}}"), 1);
    }

    #[test]
    fn the_lobby_is_a_platform_under_the_spawn() {
        let chunk = lobby_chunk(0, 0, 0, 256);
        let at = |x: usize, z: usize| chunk[64 * 256 + z * 16 + x];
        assert_eq!(at(0, 0), 1);
        assert_eq!(at(12, 3), 2);
        assert_eq!(at(13, 3), 0);
        assert_eq!(chunk.iter().filter(|b| **b != 0).count(), 13 * 13);
        assert!(lobby_chunk(2, 0, 0, 256).iter().all(|b| *b == 0));
        assert_eq!(arena_dimension(3), "bedwars:arena_3");
    }
}
