# Bed Wars

A Hypixel-style Bed Wars game as a tuff runtime plugin (WebAssembly, Rust
on `crates/tuff-plugin-sdk`). Plugin id `bedwars`.

Teams start on islands in the void, each with a bed and a forge that drops
iron and gold. Diamond generators sit between the islands and emerald
generators in the middle. A player whose bed stands respawns after dying;
once the bed is broken, the next death is a final kill. The last team with
players left wins.

## Build and install

    rustup target add wasm32-unknown-unknown
    cargo build --release --target wasm32-unknown-unknown \
        --manifest-path examples/rust-bedwars/Cargo.toml

Copy `examples/rust-bedwars/target/wasm32-unknown-unknown/release/bedwars.wasm`
into the server's `plugins/` directory.

The plugin adds five dimensions: the lobby `bedwars:bedwars` and the arenas
`bedwars:arena_1` to `bedwars:arena_4`. Adding a dimension needs an owner
grant, and dimensions are only registered when the world starts, so grant
it before starting (the server file must be the SQLite one, which keeps
grants):

    tuff plugin grant bedwars cap.dimension 'bedwars:*'

(offline, before the server starts; add `--server_file <file>` and
`--plugins <dir>` when they are not the defaults; `all-requested` in place
of the entry grants everything the manifest asks for)

or in game as an owner, then restart:

    /tuff plugin grant bedwars cap.dimension bedwars:*

`tuff plugin grants bedwars` (or `/tuff plugin grants bedwars`) shows what
is requested and granted; `revoke` takes it back. Without the grant the
plugin still loads, but there are no arenas and the log says which command
grants them.

Then `/tuff plugin load bedwars.wasm` (or restart). Arena 1 is built with
the `classic` map on first start.

## Playing

| command | who | what |
|---|---|---|
| `/bedwars join [arena]` | everyone | queue for the fullest open arena, or that one |
| `/bedwars leave` | everyone | leave the queue or forfeit your game |
| `/bedwars list` | everyone | arenas, their state and players |
| `/bedwars status [arena]` | everyone | teams, beds and the next timeline event |
| `/bedwars maps` | everyone | the maps and their teams |
| `/bedwars shop [category]` | everyone | the shop's offers and prices |
| `/bedwars lobby` | everyone | go to the Bed Wars lobby |
| `/bedwars start [arena]` | `bedwars.admin` | start a waiting arena now (2+ players) |
| `/bedwars stop [arena]` | `bedwars.admin` | end a game with no winner |
| `/bedwars create <map> [arena]` | `bedwars.admin` | build a map in a free arena (or replace an empty one) |
| `/bedwars remove <arena>` | `bedwars.admin` | end the arena's game and drop it |
| `/bedwars bot add [arena] [difficulty]` | `bedwars.bots` (everyone) | queue a bot (easy, normal or hard) |
| `/bedwars bot fill [arena] [difficulty]` | `bedwars.bots` (everyone) | fill an arena's free places with bots |
| `/bedwars bot remove [arena]` | `bedwars.bots` (everyone) | take every bot out of an arena |

`/bw` is the same command. `bedwars.admin` is held by operators of level
2 and up, or by an explicit permission grant.

A game starts by itself 20 seconds after enough players (`minPlayers`)
queue, 10 seconds once the arena is full. Players are split evenly over
the teams in join order; teams nobody plays lose their bed at the start.

Rules:

- Only blocks placed by players can be broken. Map blocks broken anyway
  come back and their drops are removed.
- Breaking your own bed puts it back. Breaking an enemy bed destroys it:
  that team no longer respawns.
- Blocks cannot be placed outside the arena's box, above its build limit,
  or within the protection radius of a spawn or generator; the block is
  given back.
- Dead players with a bed respawn at their island after `respawnSeconds`.
  Without a bed the death is final and they go back where they were before
  joining. Leaving or disconnecting forfeits.
- Timeline: Diamond II at 6:00, Emerald II at 12:00, Diamond III at 18:00,
  Emerald III at 24:00, all beds destroyed at 30:00, Sudden Death at
  40:00 and a draw at 50:00.
- Forges drop iron every 1.25 s (cap 48) and gold every 7.5 s (cap 12);
  diamonds every 30/23/12 s (cap 4) and emeralds every 65/50/35 s (cap 2)
  by tier.

After a game the winners are announced, everyone is put back where they
were before joining, and the arena is rebuilt: placed blocks are cleared
and the map, beds and waiting platform come back.
A waiting arena whose platform or a bed vanished (for example after
`/tuff chunk generate --force-regenerate`) rebuilds itself within a
second.

## Bots

Practice bots need the optional `bots` plugin (`examples/rust-bots`):
Bedwars soft-depends on it and asks it for bots over plugin messaging
(grant Bedwars `cap.cross-plugin`). Without it, Bedwars plays as usual and
`/bedwars bot ...` answers "Bots need the bots plugin". Anyone may add bots
up to an arena's player limit (`bedwars.bots`, given to every player by
default); bot names start with `BOT_`.

A bot is a real (fake) player: it joins the queue, takes a team and its
kit, shows in the player list, dies, respawns, is eliminated, gets and
gives kill loot and spends what it picks up at the shop (Bedwars buys for
it against its real inventory) exactly as a player does. When its game
ends, it is removed, or the bots plugin unloads, it leaves like a player.

Its brain (`src/bot/`) is a goal selector like a mob's: fight an enemy in
range, run back to a threatened bed, go for the nearest enemy bed once
bold enough, wait at the forge while saving up, otherwise hold the home
island. Each decision to walk is sent to the bots plugin as a `goto`
process, only when it changes. Difficulty only takes away: easy, normal
and hard bots differ in reaction time (12, 7 and 4 ticks), aim error (14,
7 and 2.5 degrees), sprinting, how far they look for fights and when they
start rushing beds (90, 45 and 15 s).

Not there yet: bots do not swing at players, bridge or use bows and
fireballs until the bots plugin has melee, place and use-item processes.

## Settings

Owners edit these like any other server setting:

| key | default | meaning |
|---|---|---|
| `plugins.bedwars.maps` | empty | extra maps as JSON (below) |
| `plugins.bedwars.minPlayers` | 2 | queued players that start the countdown |
| `plugins.bedwars.countdownSeconds` | 20 | countdown length |
| `plugins.bedwars.respawnSeconds` | 5 | respawn wait |
| `plugins.bedwars.lobbyOnJoin` | false | send players to the lobby dimension when they join the server |

Which map each arena uses is kept in the plugin's store and changed with
`/bedwars create` and `/bedwars remove`.

## Maps

`classic` (four teams of one) and `doubles` (four teams of two) are built
in. More maps go into `plugins.bedwars.maps` as a JSON list (or
`{"maps": [...]}`); a map with a built-in's name replaces it:

```json
[{
  "name": "duel",
  "team_size": 1,
  "teams": [
    {"name": "Red", "color": "red", "spawn": [0, 65, -10], "yaw": 0,
     "bed": [0, 65, -8], "bed_facing": "south", "forge": [0, 65, -12]},
    {"name": "Blue", "color": "blue", "spawn": [0, 65, 10], "yaw": 180,
     "bed": [0, 65, 8], "bed_facing": "north", "forge": [0, 65, 12]}
  ],
  "diamonds": [[10, 65, 0]],
  "emeralds": [[0, 65, 0]],
  "waiting": [0, 90, 0],
  "bounds": [[-24, 0, -24], [24, 120, 24]],
  "build_limit": 90,
  "protect": 2,
  "fills": [{"from": [-3, 64, -14], "to": [3, 64, 14], "block": "minecraft:oak_planks"}]
}]
```

- `teams`: 2 to 8, `team_size` 1 to 4. `color` is a dye color (`red`,
  `lime`, `light_blue`...); the bed takes it. `bed` is the foot; the head
  is one block towards `bed_facing`. `spawn` is where members stand,
  `forge` where iron and gold appear.
- `waiting` is where queued players stand; a glass platform is built
  under it and removed when the game starts.
- `bounds` is the arena box (min and max corners); everything must be
  inside it, and players cannot build outside it or above `build_limit`.
- `fills` are the map's blocks, boxes filled in order (at most 262144
  blocks in all). Generators and spawns get no blocks of their own, so
  give them floors.

A bad map is logged and ignored; the others still load.

## Vanilla dimensions and the spawn dimension

The plugin claims the lobby (`bedwars:bedwars`) as the spawn dimension in
its manifest (`spawn_dimension`), so once `cap.dimension` is granted, new
players and players without a bed spawn in the lobby. At the first start
with the claim the world spawn moves to the lobby's spawn point.

- An operator can pick another spawn dimension with
  `--config tuff.world.spawnDimension minecraft:overworld` (any dimension
  key); it wins over the claim.
- To run a server whose only world is Bed Wars, switch the vanilla
  dimensions off:
  `--config tuff.world.dimensions.overworld.enabled false` (and
  `.nether.enabled`, `.end.enabled`). Switched-off dimensions are not
  loaded, ticked or sent, portals into them do nothing, and their saved
  data stays for when they are switched back on. A world left with no
  dimension at all (the grant missing, say) refuses to start.
- Set `plugins.bedwars.lobbyOnJoin` to `true` to also send players who
  already have a position elsewhere to the lobby when they join.

## Chests, shop and items

- Every island has a team chest and an ender chest beside its bed. The team
  chest opens only for its team's living members; an island nobody played on
  stays locked, and once a team is out for good (bed gone, every member dead)
  anyone may open it. The ender chest is a per-player stash kept for the game
  only; the real ender chest is never shown or changed. Both are plugin menus:
  click a stack to take it, and the hopper deposits your iron, gold, diamonds
  and emeralds. Players cannot place chests in an arena.
- Right-clicking a shopkeeper in a game opens the item shop (a chest menu);
  payment is taken from the inventory and a death moves the victim's
  resources to the killing enemy while armor and permanent tools stay.
  Sharpness and protection enchant the buyer's gear at once and a
  teammate's when they respawn; haste, the heal pool and traps run every two
  seconds. Shop data is in `src/shop.rs`, the trading rules in
  `src/game/trade.rs`.
- Joining a game saves where a player was, what they carried, their health,
  food, experience and game mode; leaving or the game ending puts all of it
  back.
- Titles, the action bar and a sidebar show game state; a dead player
  spectates until the respawn timer runs out.
- Fireballs, bridge eggs and TNT that lights itself are in
  `src/game/explosives.rs`; box rules keep explosions from breaking the map.

## What is not there yet

- Final kills do not name the killer.
- Arenas are four fixed dimensions rebuilt in place; one dimension per
  game needs runtime dimensions.
