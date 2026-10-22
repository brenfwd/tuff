---
title: Installing
description: Requirements, build cost, cross-building, Docker and updating.
group: Getting started
source: docs/installing.md
---

Clone the repository and run the install script from the checkout:

    git clone https://github.com/brenfwd/tuff.git tuff && cd tuff && scripts/install.sh

The script builds the server and installs the binary at
`./tuff-server/tuff`. Run it from that directory (for offline-mode
accounts add `--offline` and `server-ip=127.0.0.1`, or see "Offline mode
and proxies"), set `white-list=false` in `server.properties` or add
players with `whitelist add <name>` on the console, then join
`localhost:25565` with a 26.3 client.

## The Minecraft EULA

The build downloads the official Minecraft server jar from Mojang and
generates game data from it, so the script asks you to accept the
[Minecraft EULA](https://www.minecraft.net/eula) before it starts
building. Answer `y` at the prompt, or pass `--accept-eula` (or set
`TUFF_ACCEPT_MOJANG_EULA=1`) for unattended installs. Without a terminal
to prompt on and without one of those, the script stops.

## Requirements

The script needs git, curl and a C compiler (`cc`, `gcc` or `clang`) and
checks for them first. It runs on bash 4 or newer on Linux and macOS. If
rustup or [bun](https://bun.sh) is missing, it installs them for your
user only (no root needed); rustup then installs the toolchain pinned in
`rust-toolchain.toml`.

## Build cost

A release build on a 16-vCPU machine, with the Mojang download already
cached, took about 6.5 minutes, peaked at about 1.3 GiB of memory in one
process and left about 1.2 GiB in `target/`. A first build on a fresh
machine is slower and uses more disk. On a host with little memory, limit
parallel jobs with `--jobs 2` (or 4). Pass `--clean` to delete `target/`
after installing; the next update then rebuilds from scratch.

Pass `--opt` to build with the `release-opt` profile (fat LTO, one codegen
unit). The server runs faster, but the build takes several times longer
and needs more memory, so leave it off on small hosts.

Other flags: `--prefix DIR` installs somewhere other than
`./tuff-server`, `--no-update` skips the `git pull`, `--dry-run` prints
the steps without running them, and `--help` lists everything.

## Building on another machine

You can run the install script on a stronger machine and copy the binary
to the server host, as long as both have the same target triple: the same
CPU architecture, operating system and C library. Check it on both with:

    rustc -vV | grep '^host:'

For example, `x86_64-unknown-linux-gnu` on both. With a `gnu` triple the
build machine's glibc must not be newer than the host's, or the binary
will not start there. If the host's glibc is older, build a musl static
binary instead.

### Cross-building

Another way to get a binary for a different machine, for example a
Raspberry Pi or an ARM VPS, is to cross-build on an x86_64 Linux host:

    scripts/install.sh --accept-eula --target aarch64-unknown-linux-musl

With a `--target` that differs from the host, the script runs
`rustup target add` and builds with `cargo zigbuild` (it installs
`cargo-zigbuild` user-locally if missing). `zig` must already be on your
`PATH`. `--opt` and `--jobs` work as usual. The `musl` triple gives a static
binary with no glibc dependency (`file` reports it as statically linked).
The output is
`target/<triple>/release/tuff`, installed to the prefix. The aarch64 musl
target has not been tested yet. The no-redistribution rule below applies
unchanged.

The binary lands in `--prefix DIR` if you passed one (default
`./tuff-server/tuff`). If you build with cargo directly, it is
`target/release/tuff`, or `target/release-opt/tuff` with `--opt`. Copy it
to the host with `scp` or `rsync`.

You still may not redistribute the binary. It embeds data generated from
Mojang's server jar, so you may only run it on machines that you (or your
organisation) operate for your own server. Redistributing means giving or
making the binary available to anyone else in any form: uploading it to a
website, release page or file share, publishing a container image that
contains it, including it in a modpack, hosting panel or installer, or
handing it to another server operator. Each operator builds their own
copy; the install script does this and asks them to accept the
[Minecraft EULA](https://www.minecraft.net/eula). This is a plain
description of how the project works, not legal advice. The same rule is
why Docker images stay off public registries.

## Updating

Run `scripts/install.sh` again. It pulls the latest commits with
`git pull --ff-only`, rebuilds and replaces the binary. Stop the server
before replacing a running binary.

## Docker

The `Dockerfile` runs the same script inside the image build. Build the
image locally and never push it to a public registry, since it contains
data generated from Mojang's server jar:

    docker build --build-arg ACCEPT_MOJANG_EULA=1 -t tuff .
    docker run -p 25565:25565 -v tuff-data:/data tuff

The build fails without `ACCEPT_MOJANG_EULA=1`. Add
`--build-arg JOBS=4` to limit parallel jobs, and `--build-arg OPT=1` to
build with the `release-opt` profile, like `--opt` above. The world and
settings live in the `tuff-data` volume, mounted at `/data`. To update, pull the
repository and build the image again.

## Windows

Windows is best effort. Run the install script inside WSL, or use the
Docker image.


## Building

The install script wraps the steps below; use them directly when working
on tuff. The build needs the Rust toolchain pinned in
`rust-toolchain.toml` (the nightly `nightly-2026-09-11`, with rustfmt,
clippy and the `wasm32-unknown-unknown` target; rustup installs it on
first use) and [bun](https://bun.sh), which builds the web admin panel
and the example plugins. The build looks for bun in `BUN`, then on
`PATH`, then at `$HOME/.bun/bin/bun`, and runs
`bun install --frozen-lockfile`; tested with bun 1.3.14.

    cargo build --release

The first build downloads the vanilla server jar and a Java runtime from
Mojang to generate game data. Set `VANILLA_JAVA` to use your own Java
instead of the downloaded one. Both are cached under `target/vanilla` (or
`VANILLA_CACHE_DIR`), and later builds need no network while that cache is
complete; if parts of it are deleted, the next build downloads them again.
