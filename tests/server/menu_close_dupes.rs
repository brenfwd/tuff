//! A client that opens a menu and never sends the close packet (a hacked
//! client's "close without packet") keeps clicking the stale window after
//! its container was broken, left behind, pushed away or emptied by someone
//! else. Every diamond given stays somewhere: nothing is made, nothing lost.

use crate::common;

use common::{TempDir, TestServer};
use std::time::Duration;
use tuff::Server;
use tuff_bot::{Bot, BotConfig};
use tuff_core::protocol::play::Serverbound;
use tuff_item::item::conservation::LossScope;
use tuff_vanilla_data::packet::play::Clientbound as Id;
use tuff_vanilla_data::registry::EntityType;
use tuff_wire::PacketReader;
use tuff_world_model::{BlockPos, Face};

const ITEM: &str = "diamond";
const HOTBAR_DIAMONDS: i64 = 3;
const SLOT_ZERO: i64 = 5;
const SLOT_ONE: i64 = 7;
const GIVEN: i64 = HOTBAR_DIAMONDS + SLOT_ZERO + SLOT_ONE;
const CONTAINER: [i32; 3] = [4, 150, 0];

#[derive(Clone, Copy, Debug)]
enum Kind {
    Chest,
    Shulker,
    Hopper,
    Furnace,
    Crafting,
    Anvil,
    Smithing,
    Grindstone,
    Loom,
    Barrel,
    Dropper,
    Minecart,
    ChestBoat,
    Donkey,
    Villager,
}

impl Kind {
    fn block(self) -> Option<&'static str> {
        Some(match self {
            Kind::Chest => "chest",
            Kind::Shulker => "shulker_box",
            Kind::Hopper => "hopper",
            Kind::Furnace => "furnace",
            Kind::Crafting => "crafting_table",
            Kind::Anvil => "anvil",
            Kind::Smithing => "smithing_table",
            Kind::Grindstone => "grindstone",
            Kind::Loom => "loom",
            Kind::Barrel => "barrel",
            Kind::Dropper => "dropper",
            Kind::Minecart | Kind::ChestBoat | Kind::Donkey | Kind::Villager => return None,
        })
    }

    fn station(self) -> bool {
        matches!(
            self,
            Kind::Crafting | Kind::Anvil | Kind::Smithing | Kind::Grindstone | Kind::Loom
        )
    }

    fn entity(self) -> Option<(&'static str, EntityType)> {
        match self {
            Kind::Minecart => Some(("chest_minecart", EntityType::ChestMinecart)),
            Kind::ChestBoat => Some(("oak_chest_boat", EntityType::OakChestBoat)),
            Kind::Donkey => Some(("donkey", EntityType::Donkey)),
            Kind::Villager => Some(("villager", EntityType::Villager)),
            _ => None,
        }
    }

    /// Slots of the container part of the window, and the window slot of
    /// the first hotbar slot.
    fn layout(self) -> (i16, i16) {
        let container = match self {
            Kind::Chest | Kind::Shulker | Kind::Barrel | Kind::Minecart | Kind::ChestBoat => 27,
            Kind::Donkey => 17,
            Kind::Dropper => 9,
            Kind::Hopper => 5,
            Kind::Furnace | Kind::Anvil | Kind::Grindstone | Kind::Villager => 3,
            Kind::Crafting => 10,
            Kind::Smithing | Kind::Loom => 4,
        };
        (container, container + 27)
    }

    /// The first window slot that holds a given item.
    fn first_item_slot(self) -> i16 {
        if matches!(self, Kind::Donkey) { 2 } else { 0 }
    }

    /// Window slots the diamonds in the hotbar are put into, for the
    /// stations whose inputs the client fills.
    fn inputs(self) -> [i16; 2] {
        match self {
            Kind::Crafting => [1, 2],
            Kind::Anvil | Kind::Grindstone | Kind::Loom => [0, 1],
            _ => [1, 2],
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Event {
    Live,
    Closed,
    Break,
    Away,
    Nether,
    Disconnect,
    SecondPlayer,
    Piston,
    Carried,
    Unloaded,
    RiddenAway,
}

async fn start() -> TestServer {
    let instance = TempDir::new();
    let mut features = tuff::vanilla_features::all();
    features.retain(|feature| !feature.id().contains("anticheat"));
    let server = Server::builder()
        .features(features)
        .instance_dir(instance.path().to_path_buf())
        .binds(vec!["127.0.0.1:0".into()])
        .online_mode(false)
        .whitelist(false)
        .view_distance(2)
        .seed(1)
        .build();
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let addr = handle.local_addrs().await.unwrap()[0];
    TestServer {
        handle,
        addr,
        task,
        _instance: instance,
    }
}

async fn say(server: &TestServer, line: &str) -> String {
    server
        .handle
        .run_command(line)
        .await
        .unwrap()
        .iter()
        .map(|c| c.plain())
        .collect::<Vec<_>>()
        .join("\n")
}

async fn until<F: std::future::Future<Output = bool>>(what: &str, mut check: impl FnMut() -> F) {
    tokio::time::timeout(Duration::from_secs(60), async {
        while !check().await {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting until {what}"));
}

/// Diamonds in the stacks of an NBT dump: every compound with the item's
/// id holds its own `count` (one when absent).
/// The hyphenated form of an `[I; a, b, c, d]` UUID dump. The census reads and
/// kills an item entity by this id: two `limit=1` selectors can pick different
/// entities when one appears or merges between the commands, which counted one
/// stack twice and killed another unseen.
fn uuid_of(text: &str) -> Option<String> {
    let (_, ints) = text.split_once("[I;")?;
    let ints: Vec<u32> = ints
        .trim_end_matches(|c: char| c == ']' || c.is_whitespace())
        .split(',')
        .map(|n| n.trim().parse::<i32>().map(|n| n as u32))
        .collect::<Result<_, _>>()
        .ok()?;
    let [a, b, c, d] = ints[..] else { return None };
    Some(format!(
        "{a:08x}-{:04x}-{:04x}-{:04x}-{:04x}{d:08x}",
        b >> 16,
        b & 0xffff,
        c >> 16,
        c & 0xffff
    ))
}

fn stacks_of(text: &str, item: &str) -> i64 {
    let needle = format!("id: \"minecraft:{item}\"");
    let bytes = text.as_bytes();
    let mut total = 0;
    let mut from = 0;
    while let Some(found) = text[from..].find(&needle) {
        let at = from + found;
        from = at + needle.len();
        let mut depth = 0;
        let mut start = None;
        for i in (0..at).rev() {
            match bytes[i] {
                b'}' => depth += 1,
                b'{' if depth == 0 => {
                    start = Some(i);
                    break;
                }
                b'{' => depth -= 1,
                _ => {}
            }
        }
        let Some(start) = start else { continue };
        let mut depth = 0;
        let mut end = bytes.len();
        for (i, byte) in bytes.iter().enumerate().skip(start) {
            match byte {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = i;
                        break;
                    }
                }
                _ => {}
            }
        }
        let body = &text[start + 1..end];
        let mut depth = 0;
        let mut count = 1;
        for (i, byte) in body.bytes().enumerate() {
            match byte {
                b'{' | b'[' => depth += 1,
                b'}' | b']' => depth -= 1,
                _ if depth == 0 && body[i..].starts_with("count: ") => {
                    let digits: String = body[i + 7..]
                        .chars()
                        .take_while(char::is_ascii_digit)
                        .collect();
                    count = digits.parse().unwrap_or(1);
                    break;
                }
                _ => {}
            }
        }
        total += count;
    }
    total
}

fn carried(said: &str) -> i64 {
    said.strip_prefix("Found ")
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|count| count.parse().ok())
        .unwrap_or(0)
}

struct Rig {
    server: TestServer,
    opener: Option<Bot>,
    /// Item entities already swept up by earlier censuses.
    swept: i64,
}

impl Rig {
    async fn new() -> Self {
        let server = start().await;
        let opener = join(&server, "Opener").await;
        let teleports = opener.state().teleports;
        say(&server, "tp Opener 0.5 150 0.5").await;
        opener
            .wait_for(|state| state.teleports > teleports)
            .await
            .unwrap();
        opener.wait_loaded(2).await.unwrap();
        until("the floor is filled", || async {
            say(&server, "fill -10 149 -10 10 149 10 stone")
                .await
                .contains("Successfully")
        })
        .await;
        for line in [
            "fill -10 150 -10 10 160 10 air replace air",
            "gamemode survival Opener",
            "gamerule natural_health_regeneration false",
        ] {
            say(&server, line).await;
        }
        Rig {
            server,
            opener: Some(opener),
            swept: 0,
        }
    }

    fn bot(&self) -> &Bot {
        self.opener.as_ref().unwrap()
    }

    async fn say(&self, line: &str) -> String {
        say(&self.server, line).await
    }

    async fn reset(&mut self, kind: Kind) {
        let [x, y, z] = CONTAINER;
        for line in [
            "kill @e[type=item]".to_owned(),
            "kill @e[type=chest_minecart]".to_owned(),
            "kill @e[type=oak_chest_boat]".to_owned(),
            "kill @e[type=donkey]".to_owned(),
            "kill @e[type=villager]".to_owned(),
            "clear Opener".to_owned(),
            "clear Taker".to_owned(),
            format!("setblock {x} {y} {z} air"),
            format!("setblock {} {y} {z} air", x + 1),
            format!("setblock {} {y} {z} air", x + 2),
            format!("setblock {} {y} {z} air", x + 3),
            "tp Opener 0.5 150 0.5".to_owned(),
        ] {
            self.say(&line).await;
        }
        self.swept = 0;
        self.settle().await;
        let hotbar = format!("item replace entity Opener hotbar.0 with {ITEM} {HOTBAR_DIAMONDS}");
        self.say(&hotbar).await;
        if let Some(block) = kind.block() {
            self.say(&format!("setblock {x} {y} {z} {block}")).await;
            if kind.station() {
                self.say(&format!(
                    "item replace entity Opener hotbar.1 with {ITEM} {}",
                    SLOT_ZERO + SLOT_ONE
                ))
                .await;
            } else {
                for (slot, count) in [(0, SLOT_ZERO), (1, SLOT_ONE)] {
                    let line = format!(
                        "item replace block {x} {y} {z} container.{slot} with {ITEM} {count}"
                    );
                    self.say(&line).await;
                }
            }
        } else if matches!(kind, Kind::Villager) {
            self.say(&format!(
                "item replace entity Opener hotbar.1 with {ITEM} {}",
                SLOT_ZERO + SLOT_ONE
            ))
            .await;
            self.say(&format!(
                "summon villager {}.5 {y} {z}.5 {{NoAI:1b,VillagerData:{{profession:\"minecraft:farmer\",level:1,type:\"minecraft:plains\"}},Offers:{{Recipes:[{{buy:{{id:\"minecraft:{ITEM}\",count:{SLOT_ZERO}}},sell:{{id:\"minecraft:emerald\",count:1}},maxUses:99}},{{buy:{{id:\"minecraft:{ITEM}\",count:{SLOT_ONE}}},sell:{{id:\"minecraft:gold_ingot\",count:1}},maxUses:99}}]}}}}",
                x - 3
            ))
            .await;
        } else if let Some((name, _)) = kind.entity() {
            let at = if matches!(kind, Kind::Donkey) {
                x - 3
            } else {
                x
            };
            let said = self.say(&format!(
                "summon {name} {}.5 {y} {z}.5 {{Owner:[I;1,2,3,4],Tame:1b,ChestedHorse:1b,Items:[{{Slot:0b,id:\"minecraft:{ITEM}\",count:{SLOT_ZERO}}},{{Slot:1b,id:\"minecraft:{ITEM}\",count:{SLOT_ONE}}}]}}",
                at
            ))
            .await;
            let _ = said;
        }
    }

    async fn settle(&self) {
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    /// Every diamond anywhere: inventories, container blocks, container
    /// entities and the item entities lying about (swept as they are read).
    async fn total(&mut self, kind: Kind, players: &[&str]) -> i64 {
        let mut total = 0;
        for player in players {
            total += carried(&self.say(&format!("clear {player} {ITEM} 0")).await);
        }
        let [x, y, z] = CONTAINER;
        if kind.block().is_some() {
            total += stacks_of(
                &self.say(&format!("data get block {x} {y} {z}")).await,
                ITEM,
            );
        }
        if let Some((name, _)) = kind.entity().filter(|_| !matches!(kind, Kind::Villager)) {
            total += stacks_of(
                &self
                    .say(&format!("data get entity @e[type={name},limit=1]"))
                    .await,
                ITEM,
            );
        }
        for _ in 0..200 {
            let found = self.say("data get entity @e[type=item,limit=1] UUID").await;
            if found.contains("No entity was found") {
                break;
            }
            let Some(uuid) = uuid_of(&found) else {
                continue;
            };
            let dump = self.say(&format!("data get entity {uuid} Item")).await;
            let killed = self.say(&format!("kill {uuid}")).await;
            if killed.contains("Killed") {
                self.swept += stacks_of(&dump, ITEM);
            }
        }
        total + self.swept
    }

    async fn stable_total(&mut self, kind: Kind, players: &[&str]) -> i64 {
        let mut last = -1;
        for _ in 0..20 {
            self.settle().await;
            let now = self.total(kind, players).await;
            if now == last {
                return now;
            }
            last = now;
        }
        last
    }
}

async fn join(server: &TestServer, name: &str) -> Bot {
    let bot = Bot::connect(
        server.addr,
        BotConfig {
            view_distance: 2,
            keep: vec![Id::ContainerSetContent.id(), Id::ContainerSetSlot.id()],
            ..BotConfig::new(name)
        },
    )
    .await
    .unwrap();
    bot.wait_for(|state| state.chunks > 0).await.unwrap();
    bot
}

fn pos() -> BlockPos {
    BlockPos::new(CONTAINER[0], CONTAINER[1], CONTAINER[2])
}

async fn open(bot: &Bot, kind: Kind) -> i32 {
    let before = bot.state().windows_opened;
    if let Some((_, entity)) = kind.entity() {
        let id = loop {
            let found = bot
                .state()
                .entities
                .iter()
                .find(|(_, seen)| seen.kind == Some(entity))
                .map(|(id, _)| *id);
            if let Some(id) = found {
                break id;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        };
        bot.send(Serverbound::Interact {
            entity: id,
            off_hand: false,
            location: [0.0; 3],
            sneaking: !matches!(kind, Kind::Villager),
        });
    } else {
        bot.send(Serverbound::UseItemOn {
            hand: 0,
            position: pos(),
            face: Face::Up,
            cursor: [0.5; 3],
            sequence: 1,
        });
    }
    tokio::time::timeout(
        Duration::from_secs(30),
        bot.wait_for(|state| state.windows_opened > before && state.window.is_some()),
    )
    .await
    .unwrap_or_else(|_| panic!("{kind:?} did not open for {}", bot.name()))
    .unwrap();
    bot.state().window.unwrap()
}

/// Mounts the donkey and opens its inventory with the inventory key, as a
/// riding client does.
async fn open_ridden(rig: &Rig) -> i32 {
    let bot = rig.bot();
    until("the donkey is seen", || async {
        bot.state()
            .entities
            .values()
            .any(|seen| seen.kind == Some(EntityType::Donkey))
    })
    .await;
    rig.say("ride Opener mount @e[type=donkey,limit=1]").await;
    until("the player rides the donkey", || async {
        rig.say("execute as Opener on vehicle if entity @s[type=donkey]")
            .await
            .contains("passed")
    })
    .await;
    let before = bot.state().windows_opened;
    bot.send(Serverbound::PlayerCommand(5));
    tokio::time::timeout(
        Duration::from_secs(30),
        bot.wait_for(|state| state.windows_opened > before && state.window.is_some()),
    )
    .await
    .expect("the ridden donkey's inventory opens")
    .unwrap();
    bot.state().window.unwrap()
}

/// The state id the server last sent for the window, as a client tracks it.
fn state_id(bot: &Bot, window: i32) -> i32 {
    bot.with_state(|state| {
        state
            .kept_order
            .iter()
            .rev()
            .find_map(|(_, body)| {
                let mut r = PacketReader::new(body.clone());
                (r.read_var_int().ok()? == window)
                    .then(|| r.read_var_int().ok())
                    .flatten()
            })
            .unwrap_or(0)
    })
}

fn click(bot: &Bot, window_id: i32, slot: i16, button: i8, mode: i32) -> Serverbound {
    Serverbound::ContainerClick {
        window_id,
        state_id: state_id(bot, window_id),
        slot,
        button,
        mode,
        changed: Vec::new(),
        carried: None,
    }
}

/// Fills the first two inputs of a station from the hotbar, as a client
/// does: pick the stack up, put it down.
fn fill_station(bot: &Bot, window: i32, kind: Kind) {
    let (_, hotbar) = kind.layout();
    let [first, second] = kind.inputs();
    bot.send(click(bot, window, hotbar, 0, 0));
    bot.send(click(bot, window, first, 0, 0));
    bot.send(click(bot, window, hotbar + 1, 0, 0));
    bot.send(click(bot, window, second, 0, 0));
}

/// What a client that never closed the window sends afterwards.
fn stale_clicks(bot: &Bot, window: i32, kind: Kind) {
    let (container, hotbar) = kind.layout();
    for slot in [0, 1, 2, container - 1] {
        for (button, mode) in [
            (0, 0),
            (1, 0),
            (0, 1),
            (0, 2),
            (1, 2),
            (0, 4),
            (1, 4),
            (0, 6),
        ] {
            bot.send(click(bot, window, slot, button, mode));
        }
    }
    for slot in [hotbar, hotbar + 1, container] {
        for (button, mode) in [(0, 0), (0, 1), (0, 4), (1, 4)] {
            bot.send(click(bot, window, slot, button, mode));
        }
    }
    bot.send(click(bot, window, -999, 0, 0));
}

async fn game_time(rig: &Rig) -> i64 {
    rig.say("time query gametime")
        .await
        .chars()
        .filter(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .unwrap_or(0)
}

async fn scenario(kind: Kind, event: Event) {
    let mut rig = Rig::new().await;
    rig.reset(kind).await;
    let mut players = vec!["Opener"];
    let window = if matches!(event, Event::RiddenAway) {
        open_ridden(&rig).await
    } else {
        open(rig.bot(), kind).await
    };
    if kind.station() {
        fill_station(rig.bot(), window, kind);
    }
    if matches!(kind, Kind::Villager) {
        rig.bot().send(Serverbound::SelectTrade(0));
        rig.settle().await;
    }
    let [x, y, z] = CONTAINER;
    let mut taker = None;
    match event {
        Event::Live => {
            rig.bot()
                .send(click(rig.bot(), window, kind.first_item_slot(), 0, 1));
            rig.settle().await;
        }
        Event::Closed => {
            rig.bot().send(Serverbound::ContainerClose(window));
            rig.settle().await;
        }
        Event::Break => {
            if kind.block().is_some() {
                rig.say(&format!("setblock {x} {y} {z} air destroy")).await;
            } else {
                let name = kind.entity().expect("an entity container").0;
                rig.say(&format!("kill @e[type={name}]")).await;
            }
        }
        Event::Away => {
            rig.say("tp Opener 1500.5 150 1500.5").await;
            rig.settle().await;
        }
        Event::Nether => {
            rig.say("execute in minecraft:the_nether run tp Opener 0.5 100 0.5")
                .await;
            rig.settle().await;
        }
        Event::Disconnect => {
            let old = rig.opener.take().unwrap();
            drop(old);
            until("the player has left", || async {
                rig.say("data get entity Opener Health")
                    .await
                    .contains("No entity")
            })
            .await;
            rig.opener = Some(join(&rig.server, "Opener").await);
            rig.say("gamemode survival Opener").await;
        }
        Event::SecondPlayer => {
            let bot = join(&rig.server, "Taker").await;
            rig.say("gamemode survival Taker").await;
            let teleports = bot.state().teleports;
            rig.say("tp Taker 1.5 150 1.5").await;
            bot.wait_for(|state| state.teleports > teleports)
                .await
                .unwrap();
            rig.settle().await;
            let theirs = open(&bot, kind).await;
            let (container, _) = kind.layout();
            for slot in 0..container.min(3) {
                bot.send(click(&bot, theirs, slot, 0, 1));
            }
            rig.settle().await;
            taker = Some((bot, theirs));
            players = vec!["Opener", "Taker"];
        }
        Event::Unloaded => {
            rig.say("tp Opener 1500.5 150 1500.5").await;
            let before = game_time(&rig).await;
            rig.say("tick sprint 400").await;
            until("the sprint is over", || async {
                game_time(&rig).await >= before + 400
            })
            .await;
            rig.settle().await;
            until("the chunk is unloaded", || async {
                !rig.say("execute if loaded 4 150 0")
                    .await
                    .contains("Test passed")
            })
            .await;
        }
        Event::Carried => {
            rig.say("tp @e[type=donkey] 1.5 150 30.5").await;
            rig.settle().await;
        }
        Event::RiddenAway => {
            rig.bot().send(Serverbound::PlayerInput(0x20));
            rig.settle().await;
            rig.bot().send(Serverbound::PlayerInput(0));
            until("the player has dismounted", || async {
                !rig.say("execute as Opener on vehicle if entity @s[type=donkey]")
                    .await
                    .contains("passed")
            })
            .await;
            rig.say("tp Opener 64.5 150 0.5").await;
            rig.settle().await;
        }
        Event::Piston => {
            for line in [
                format!("setblock {} {y} {z} piston[facing=west]", x + 1),
                format!("setblock {} {y} {z} redstone_block", x + 2),
            ] {
                rig.say(&line).await;
            }
        }
    }
    let stays = matches!(event, Event::Live | Event::Closed | Event::SecondPlayer)
        || (matches!(event, Event::Piston) && (!kind.station() || matches!(kind, Kind::Anvil)));
    if matches!(
        event,
        Event::Break
            | Event::Piston
            | Event::Away
            | Event::Nether
            | Event::Carried
            | Event::Unloaded
            | Event::RiddenAway
    ) {
        let watcher = rig.bot();
        let budget = Duration::from_secs(if stays { 10 } else { 60 });
        let shut = tokio::time::timeout(budget, watcher.wait_for(|state| state.window.is_none()))
            .await
            .is_ok();
        assert_eq!(
            shut, !stays,
            "{kind:?} / {event:?}: the server closes the window"
        );
    }
    if !matches!(event, Event::Live) {
        stale_clicks(rig.bot(), window, kind);
    }
    rig.settle().await;
    if let Some((bot, theirs)) = &taker {
        stale_clicks(bot, *theirs, kind);
    }
    match event {
        Event::Away | Event::Nether | Event::Unloaded | Event::RiddenAway => {
            rig.say("execute in minecraft:overworld run tp Opener 0.5 150 0.5")
                .await;
        }
        _ => {}
    }
    if matches!(event, Event::Live) && !kind.station() {
        rig.settle().await;
        let held = carried(&rig.say("clear Opener diamond 0").await);
        assert!(held > HOTBAR_DIAMONDS, "{kind:?}: the clicks moved nothing");
    }
    rig.bot().send(Serverbound::ContainerClose(window));
    rig.bot().send(Serverbound::ContainerClose(0));
    if let Some((bot, theirs)) = &taker {
        bot.send(Serverbound::ContainerClose(*theirs));
        bot.send(Serverbound::ContainerClose(0));
    }
    let total = rig.stable_total(kind, &players).await;
    assert_eq!(
        total, GIVEN,
        "{kind:?} / {event:?}: {GIVEN} diamonds were given"
    );
    let lost = LossScope::current().expect("scoped by the watchdog").lost();
    assert_eq!(lost, 0, "{kind:?} / {event:?}: items were lost");
    rig.server.handle.shutdown().await.unwrap();
    rig.server.task.await.unwrap().unwrap();
}

macro_rules! scenarios {
    ($($name:ident: $kind:expr, $event:expr;)*) => {$(
        #[tuff_core::watchdog]
        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn $name() {
            scenario($kind, $event).await;
        }
    )*};
}

scenarios! {
    chest_clicks_while_open: Kind::Chest, Event::Live;
    minecart_clicks_while_open: Kind::Minecart, Event::Live;
    chest_closed_properly: Kind::Chest, Event::Closed;
    anvil_closed_properly: Kind::Anvil, Event::Closed;
    chest_broken_while_open: Kind::Chest, Event::Break;
    chest_left_far_behind: Kind::Chest, Event::Away;
    chest_left_for_the_nether: Kind::Chest, Event::Nether;
    chest_taken_by_a_second_player: Kind::Chest, Event::SecondPlayer;
    chest_beside_a_piston: Kind::Chest, Event::Piston;
    shulker_broken_while_open: Kind::Shulker, Event::Break;
    shulker_taken_by_a_second_player: Kind::Shulker, Event::SecondPlayer;
    hopper_broken_while_open: Kind::Hopper, Event::Break;
    furnace_broken_while_open: Kind::Furnace, Event::Break;
    crafting_table_broken_while_open: Kind::Crafting, Event::Break;
    crafting_table_pushed_while_open: Kind::Crafting, Event::Piston;
    anvil_broken_while_open: Kind::Anvil, Event::Break;
    anvil_pushed_while_open: Kind::Anvil, Event::Piston;
    smithing_table_broken_while_open: Kind::Smithing, Event::Break;
    grindstone_broken_while_open: Kind::Grindstone, Event::Break;
    loom_broken_while_open: Kind::Loom, Event::Break;
    barrel_broken_while_open: Kind::Barrel, Event::Break;
    dropper_broken_while_open: Kind::Dropper, Event::Break;
    minecart_killed_while_open: Kind::Minecart, Event::Break;
    minecart_left_far_behind: Kind::Minecart, Event::Away;
    chest_boat_killed_while_open: Kind::ChestBoat, Event::Break;
    chest_disconnected_while_open: Kind::Chest, Event::Disconnect;
    chest_unloaded_while_open: Kind::Chest, Event::Unloaded;
    furnace_unloaded_while_open: Kind::Furnace, Event::Unloaded;
    minecart_unloaded_while_open: Kind::Minecart, Event::Unloaded;
    donkey_unloaded_while_open: Kind::Donkey, Event::Unloaded;
    donkey_clicks_while_open: Kind::Donkey, Event::Live;
    donkey_closed_properly: Kind::Donkey, Event::Closed;
    donkey_killed_while_open: Kind::Donkey, Event::Break;
    donkey_left_far_behind: Kind::Donkey, Event::Away;
    donkey_left_for_the_nether: Kind::Donkey, Event::Nether;
    donkey_led_far_away: Kind::Donkey, Event::Carried;
    donkey_disconnected_while_open: Kind::Donkey, Event::Disconnect;
    donkey_taken_by_a_second_player: Kind::Donkey, Event::SecondPlayer;
    donkey_ridden_then_dismounted_away: Kind::Donkey, Event::RiddenAway;
    villager_killed_while_trading: Kind::Villager, Event::Break;
}

/// Picks a trade, picks another one (the result is replaced), takes one
/// result and closes: only the trade taken changes the counts.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn merchant_result_replaced_by_another_trade() {
    let mut rig = Rig::new().await;
    rig.reset(Kind::Villager).await;
    let window = open(rig.bot(), Kind::Villager).await;
    let (_, hotbar) = Kind::Villager.layout();
    for trade in [0, 1, 0, 1] {
        rig.bot().send(Serverbound::SelectTrade(trade));
        rig.settle().await;
    }
    rig.bot().send(click(rig.bot(), window, 2, 0, 0));
    rig.settle().await;
    rig.bot().send(click(rig.bot(), window, hotbar + 8, 0, 0));
    rig.settle().await;
    rig.bot().send(Serverbound::ContainerClose(window));
    let total = rig.stable_total(Kind::Villager, &["Opener"]).await;
    assert_eq!(
        total,
        GIVEN - SLOT_ONE,
        "one trade paid {SLOT_ONE} diamonds"
    );
    for (item, made) in [("gold_ingot", 1), ("emerald", 0)] {
        let held = carried(&rig.say(&format!("clear Opener {item} 0")).await);
        assert_eq!(held, made, "{item} made by the trades");
    }
    let lost = LossScope::current().expect("scoped by the watchdog").lost();
    assert_eq!(lost, 0, "merchant: items were lost");
    rig.server.handle.shutdown().await.unwrap();
    rig.server.task.await.unwrap().unwrap();
}

struct Station {
    block: &'static str,
    container: i16,
    gives: &'static [(&'static str, &'static str)],
    inputs: &'static [i16],
    buttons: &'static [i32],
    withdraw: i16,
    /// The first given item is an empty map, used first to draw one.
    draws_map: bool,
}

/// Fills a station with items that make a result, changes the result
/// (buttons, taking an input away) and closes: nothing may be lost.
async fn result_replaced(station: Station) {
    let rig = Rig::new().await;
    let [x, y, z] = CONTAINER;
    for (n, (_, stack)) in station.gives.iter().enumerate() {
        rig.say(&format!(
            "item replace entity Opener hotbar.{n} with {stack}"
        ))
        .await;
    }
    if station.draws_map {
        rig.bot().send(Serverbound::UseItem {
            off_hand: false,
            sequence: 2,
        });
        until("the map is drawn", || async {
            carried(&rig.say("clear Opener filled_map 0").await) == 1
        })
        .await;
    }
    rig.say(&format!("setblock {x} {y} {z} {}", station.block))
        .await;
    rig.settle().await;
    let window = open(rig.bot(), Kind::Crafting).await;
    let hotbar = station.container + 27;
    for (n, slot) in station.inputs.iter().enumerate() {
        rig.bot()
            .send(click(rig.bot(), window, hotbar + n as i16, 0, 0));
        rig.bot().send(click(rig.bot(), window, *slot, 0, 0));
    }
    for button in station.buttons {
        rig.bot().send(Serverbound::ContainerButtonClick {
            window_id: window,
            button: *button,
        });
        rig.settle().await;
    }
    rig.bot()
        .send(click(rig.bot(), window, station.withdraw, 0, 0));
    rig.bot().send(click(rig.bot(), window, hotbar + 8, 0, 0));
    rig.settle().await;
    rig.bot().send(Serverbound::ContainerClose(window));
    rig.settle().await;
    let lost = LossScope::current().expect("scoped by the watchdog").lost();
    assert_eq!(lost, 0, "{}: items were lost", station.block);
    rig.server.handle.shutdown().await.unwrap();
    rig.server.task.await.unwrap().unwrap();
}

macro_rules! result_scenarios {
    ($($name:ident: $station:expr;)*) => {$(
        #[tuff_core::watchdog]
        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn $name() {
            result_replaced($station).await;
        }
    )*};
}

result_scenarios! {
    crafting_result_dropped_with_its_input: Station {
        block: "crafting_table", container: 10,
        gives: &[("oak_log", "oak_log 1")], inputs: &[1], buttons: &[], withdraw: 1, draws_map: false,
    };
    stonecutter_result_dropped_with_its_input: Station {
        block: "stonecutter", container: 2,
        gives: &[("stone", "stone 1")], inputs: &[0], buttons: &[0], withdraw: 0, draws_map: false,
    };
    stonecutter_result_replaced_by_another_choice: Station {
        block: "stonecutter", container: 2,
        gives: &[("stone", "stone 1")], inputs: &[0], buttons: &[0, 1], withdraw: 0, draws_map: false,
    };
    smithing_result_dropped_with_its_addition: Station {
        block: "smithing_table", container: 4,
        gives: &[
            ("t", "netherite_upgrade_smithing_template 1"),
            ("p", "diamond_pickaxe 1"),
            ("i", "netherite_ingot 1"),
        ],
        inputs: &[0, 1, 2], buttons: &[], withdraw: 2, draws_map: false,
    };
    loom_result_replaced_by_another_pattern: Station {
        block: "loom", container: 4,
        gives: &[("b", "white_banner 1"), ("d", "red_dye 1")],
        inputs: &[0, 1], buttons: &[0, 1], withdraw: 1, draws_map: false,
    };
    grindstone_result_dropped_with_its_input: Station {
        block: "grindstone", container: 3,
        gives: &[("s", "iron_sword[enchantments={\"minecraft:sharpness\":3}] 1")],
        inputs: &[0], buttons: &[], withdraw: 0, draws_map: false,
    };
    cartography_result_replaced_by_another_input: Station {
        block: "cartography_table", container: 3,
        gives: &[("m", "map 1"), ("p", "paper 1"), ("g", "glass_pane 1")],
        inputs: &[0, 1, 1], buttons: &[], withdraw: 1, draws_map: true,
    };
}
