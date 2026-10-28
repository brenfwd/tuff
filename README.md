<h1>
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/assets/logo-dark.svg">
    <img src="docs/assets/logo-light.svg" alt="" width="40" height="40" align="top">
  </picture>
  tuff
</h1>

A Minecraft Java Edition server written in Rust, for game version 26.3.
The vanilla game runs as a set of plugins on an inert core, and external
plugins are sandboxed WebAssembly modules.

The documentation lives at [tuff.bforward.me](https://tuff.bforward.me).
Start with the [introduction](https://tuff.bforward.me/docs/introduction/),
or read the same pages in [`docs/`](docs/introduction.md).

## Quick start

tuff publishes no prebuilt binaries. Clone the repository and run the
install script, which builds the server and installs it at
`./tuff-server/tuff`:

    git clone https://github.com/brenfwd/tuff.git tuff && cd tuff && scripts/install.sh

The script needs git, curl and a C compiler, and installs rustup and
[bun](https://bun.sh) for your user if they are missing. The build
downloads the official server jar from Mojang, so the script asks you to
accept the [Minecraft EULA](https://www.minecraft.net/eula) first (pass
`--accept-eula` for unattended installs).

Run `./tuff` from `tuff-server/`, add players with `whitelist add <name>`
on the console or set `white-list=false` in `server.properties`, then
join `localhost:25565` with a 26.3 client. For offline-mode accounts,
start it with `--offline` and set `server-ip=127.0.0.1`.

Current alpha: the release notes in
[Introduction](docs/introduction.md) list what changed in
v0.1.0-alpha.2 (2026-10-28), since v0.1.0-alpha.1. It supports worlds from 1.13 or newer, Windows is best
effort (use WSL or Docker), anti-cheat only logs by default, and the admin
panel needs a reverse proxy for remote access.

To build and run from the checkout instead:

    cargo build --release
    cargo run --release -- --offline -Dtuff.allowPublicOffline=true

See [Installing](docs/installing.md) and [Running](docs/running.md) for
requirements, flags and platform notes, and `CONTRIBUTING.md` for
development guidelines.

## License

The server is licensed under the [GNU Affero General Public License v3.0
only](LICENSE). The plugin SDK, SDK generator, plugin ABI WIT files and
example plugins are licensed under the [MIT License](examples/LICENSE-MIT),
so plugins you write may use any license. See [License](docs/license.md)
for details. Contributions require agreeing to the [Contributor License
Agreement](CLA.md).
