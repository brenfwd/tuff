# Built-in features

The vanilla game is 16 built-in features loaded on an inert core: `worldgen`,
`redstone`, `items`, `maps`, `recipes`, `loot`, `blocks`, `combat`,
`players`, `entities`, `mobs`, `survival`, `dimensions`, `tuff-feature-builtin-core-commands`,
`tuff-feature-builtin-vanilla-commands` and `anticheat`. They load by default. Leave one out
(for example on a minigame server) with
`tuff.vanilla.<name>.enabled=false`, where `<name>` is the feature id
without `vanilla-` (`tuff-feature-builtin-core-commands` keeps its full name). If a feature that
stays needs one you left out, the server refuses to start and names both.
The loaded set is logged at start-up.

Optional built-in features change vanilla behaviour and are off by default:

| Id | Effect |
|----|--------|
| `tuff-feature-builtin-moving-block-entities` | Pistons move blocks with block entities (chests, furnaces and others) and their data |
| `world-map` | A live, zoomable world map page in the web panel, with chunk pre-generation (permissions `map.view`, level 2, and `map.generate`, level 4) |
| `tuff-feature-builtin-example` | The built-in feature API example (`crates/tuff-feature-builtin-example`): `/gold`, extra recipes, a custom item, block and entity, and a Gold panel page |
| `tuff-feature-builtin-chunk-progress` | A "Loading Chunks" boss bar showing each player how many chunks in view have been sent |
| `audit-log` | A world audit log in its own SQLite file beside the world (`<world>.audit.sqlite`, or `tuff.features.audit-log.database`): block breaks and placements, explosions, chat, commands and sessions, with `/tuff audit lookup`, `rollback`, `restore`, `purge` and `status`. Retention is set by `tuff.features.audit-log.retention` and old entries are purged in the background |

Turn one on with `--config tuff.features.<id>.enabled true` or by setting
`tuff.features.<id>.enabled=true` in the saved settings (`tuff.properties`
for a folder world, the server file for sqlite); `--config` wins. A
feature's own settings live under `tuff.features.<id>.*`. Feature and
vanilla feature switches are read only at start-up.


## Custom content pack

The server builds a resource pack from the custom items, blocks and assets
that plugins register, and every client must accept it: a player who
declines it, or whose download fails, is disconnected. When no plugin
registers any assets there is no pack, and clients are not asked for one.

Clients download the pack from the HTTP server, so its address must be
reachable from their machines. Fix `tuff.http.port` and set
`tuff.http.resourcePathBaseUrl` to a URL players can reach, or set
`tuff.http.detectPublicIp` to `true` (default `false`), which learns the
server's public address with a STUN request over UDP to
`stun.cloudflare.com` and `stun.l.google.com`. Both apply without a
restart, and the log warns, naming both keys, when a pack URL is one
clients cannot reach. `tuff.customContent.prompt` sets the text shown with
the offer. `tuff.resourcePacks.enabled` turns off other plugin packs but
not this one.
