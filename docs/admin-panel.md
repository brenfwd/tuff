# Web admin panel

The panel is served under `/admin` on the HTTP server and is off by
default. To log in:

1. Run `/tuff config tuff.web.enabled true`.
2. Run `/tuff web login`. It prints a single-use 16-digit code
   (`XXXX-XXXX-XXXX-XXXX`), valid for 120 seconds, and the panel URL.
3. Open the URL and enter the code.

Code exchanges are rate limited per address and overall. `/tuff web
logout` ends your own panel sessions and `/tuff web logout-all` ends
everyone's.

Scripts use API tokens instead of a session.
`/tuff web token create <name> <level> [expiry]` prints a token of the
form `tuff_<id>_<secret>` once; only its hash is stored. The name is 1 to
64 letters, digits, `_`, `-` or `.`, a token cannot outrank its creator,
and the expiry is `never` or a count with `m`, `h`, `d`, `w` or `y`, like
`30d`, up to ten years. Send it as `Authorization: Bearer <token>`; such
requests need no CSRF header. `/tuff web token list` shows tokens and
`/tuff web token revoke <id or name>` deletes one. The Access page in the
panel does the same. Logins, logouts, token changes and refused tokens go
to the panel's audit log.

The built-in pages are Access, World, Config, Audit, Players, Logs,
Metrics, Timings, Backups, Datapacks, Resource packs and Anticheat. Each
area has its own permission node (for example `tuff.web.config.write`,
`tuff.web.backups.restore`), held by operators at level 4 by default.

| Key | Default | Effect |
|-----|---------|--------|
| `tuff.web.enabled` | `false` | Serve the panel |
| `tuff.web.trustOfflineNames` | `false` | With `online-mode=false` player names are unauthenticated, so by default players get no panel login codes, tokens or panel sessions (the console still can); set true to accept that anyone can claim an operator's name |
| `tuff.web.allowRemote` | `false` | Accept clients other than loopback; refused (with an error in the log) unless `tuff.web.tlsTerminated` or `tuff.web.acknowledgePlaintext` is also true |
| `tuff.web.tlsTerminated` | `false` | Declare that a TLS proxy terminates HTTPS in front of the panel |
| `tuff.web.acknowledgePlaintext` | `false` | Accept serving the panel to remote clients over plain HTTP |
| `tuff.web.hostnames` | empty | Extra host names the panel answers to, comma separated |
| `tuff.web.publicUrl` | empty | Base URL shown in login messages (empty or an `http(s)` URL) |
| `tuff.web.trustForwarded` | `false` | Take the client address and host from `X-Forwarded-For`/`-Host` |
| `tuff.web.minOpLevel` | `4` | Lowest operator level (1 to 4) that may log in; the panel refuses a raise that would lock you out |

Behind a reverse proxy that rewrites the `Host` header, set
`tuff.web.publicUrl` to the proxy's public address (and
`tuff.web.trustForwarded` if the proxy sets `X-Forwarded-Host`/`-For`), or
plugin pages will not load. A request whose host is not allowed is
answered with 421. While `publicUrl` is empty and `trustForwarded` is
false, the server logs one warning per process the first time an allowed
panel request names a host (in `Host` or `X-Forwarded-Host`) that is
neither a bound address nor in `tuff.web.hostnames`.

Plugins can add pages and JSON APIs to the panel. The example plugin (the
`tuff-feature-builtin-example` feature) adds a Gold page that shows, live, how many gold
blocks have been placed in the world, with a button to reset the count.
Pages are React apps built with the Vite helper `panelPage()` in
`crates/tuff-web-ui/app/sdk/vite.ts`; `crates/tuff-feature-builtin-example/README.md` shows
how to write one. A plugin page is gated by a permission scope that
defaults to operator level 4; a plugin can set a lower level, as the world
map does for `map.view`.
