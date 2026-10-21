#!/usr/bin/env bash
# Builds tuff from source on this machine and installs the binary.
# Usage: install.sh [--accept-eula] [--prefix DIR] [--jobs N] [--clean]
#                   [--opt] [--target TRIPLE] [--no-update] [--dry-run] [--help]
# The build downloads the official Minecraft server jar and generates data from
# it, so the Minecraft EULA must be accepted first. Release binaries are not
# distributed.
set -euo pipefail

PREFIX=./tuff-server
JOBS=${CARGO_BUILD_JOBS:-}
CLEAN=0
DRY=0
PROFILE=release
UPDATE=1
TARGET=
ACCEPT=0
[ "${TUFF_ACCEPT_MOJANG_EULA:-}" = 1 ] && ACCEPT=1

usage() {
    sed -n '2,8p' "$0" | sed 's/^# \{0,1\}//'
    cat <<'TXT'

Flags:
  --accept-eula   accept https://www.minecraft.net/eula (or TUFF_ACCEPT_MOJANG_EULA=1)
  --prefix DIR    install directory (default ./tuff-server)
  --jobs N        cargo build jobs (use 2-4 on hosts with little RAM)
  --clean         delete target/ after installing
  --opt           build with the release-opt profile (fat LTO, one codegen
                  unit): faster server, much slower build and more RAM
  --target TRIPLE cross-build for another target (needs zig and cargo-zigbuild),
                  e.g. aarch64-unknown-linux-musl for a static ARM binary
  --no-update     skip git pull when run inside a git checkout
  --dry-run       print the steps without installing or building
TXT
}

die() { echo "install.sh: $*" >&2; exit 1; }

while [ $# -gt 0 ]; do
    case "$1" in
        --accept-eula) ACCEPT=1 ;;
        --prefix) [ $# -ge 2 ] || die "--prefix needs a value"; PREFIX=$2; shift ;;
        --prefix=*) PREFIX=${1#*=} ;;
        --jobs) [ $# -ge 2 ] || die "--jobs needs a value"; JOBS=$2; shift ;;
        --jobs=*) JOBS=${1#*=} ;;
        --clean) CLEAN=1 ;;
        --opt) PROFILE=release-opt ;;
        --target) [ $# -ge 2 ] || die "--target needs a value"; TARGET=$2; shift ;;
        --target=*) TARGET=${1#*=} ;;
        --no-update) UPDATE=0 ;;
        --dry-run) DRY=1 ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; die "unknown argument: $1" ;;
    esac
    shift
done

if [ -n "$JOBS" ]; then
    case "$JOBS" in ''|*[!0-9]*|0) die "--jobs must be a positive integer" ;; esac
fi

step() { echo "==> $*"; }
run() {
    if [ "$DRY" = 1 ]; then echo "[dry-run] $*"; else "$@"; fi
}

cat <<'TXT'
tuff builds from source on this machine. Expect roughly 2 to 20 GiB of disk during
the build (almost none after --clean), 2 GiB of RAM per job
(use --jobs 2 or 4 on smaller hosts) and 6 to 40 minutes depending on cores.
TXT

if [ "$ACCEPT" != 1 ]; then
    if [ "$DRY" = 1 ] || [ ! -t 0 ]; then
        die "the Minecraft EULA (https://www.minecraft.net/eula) must be accepted: pass --accept-eula or set TUFF_ACCEPT_MOJANG_EULA=1"
    fi
    printf 'The build downloads the official Minecraft server jar from Mojang.\nDo you accept the Minecraft EULA (https://www.minecraft.net/eula)? [y/N] '
    read -r answer
    case "$answer" in y|Y|yes|YES) ACCEPT=1 ;; *) die "EULA not accepted" ;; esac
fi
export TUFF_ACCEPT_MOJANG_EULA=1
step "EULA accepted"

case "$PREFIX" in /*) ;; *) PREFIX=$PWD/${PREFIX#./} ;; esac
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
[ -f "$ROOT/Cargo.toml" ] && [ -f "$ROOT/rust-toolchain.toml" ] || die "run this from a tuff source checkout"

step "checking prerequisites"
missing=()
for tool in git curl; do
    command -v "$tool" >/dev/null 2>&1 || missing+=("$tool")
done
command -v cc >/dev/null 2>&1 || command -v gcc >/dev/null 2>&1 || command -v clang >/dev/null 2>&1 || missing+=("a C compiler (cc)")
if [ "${#missing[@]}" -gt 0 ]; then
    echo "missing: ${missing[*]}" >&2
    if [ "$DRY" = 1 ]; then echo "[dry-run] would stop here"; else
        die "install them with your package manager (apt install build-essential git curl, or xcode-select --install)"
    fi
fi

cd "$ROOT"
if [ "$UPDATE" = 1 ] && [ -d .git ]; then
    step "updating source"
    run git pull --ff-only
fi

export PATH="$HOME/.cargo/bin:$HOME/.bun/bin:$PATH"
if ! command -v rustup >/dev/null 2>&1; then
    step "installing rustup (user-local)"
    if [ "$DRY" = 1 ]; then
        echo "[dry-run] curl https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain none"
    else
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain none
    fi
fi
HOST=$(rustc -vV 2>/dev/null | sed -n 's/^host: //p' || true)
step "installing the pinned toolchain from rust-toolchain.toml"
run rustup show active-toolchain || run rustup toolchain install

if ! command -v bun >/dev/null 2>&1 && [ -z "${BUN:-}" ]; then
    step "installing bun (user-local, for the example plugin panel)"
    if [ "$DRY" = 1 ]; then
        echo "[dry-run] curl https://bun.sh/install | bash"
    else
        curl -fsSL https://bun.sh/install | bash
    fi
fi

if [ -n "$JOBS" ]; then export CARGO_BUILD_JOBS=$JOBS; fi
OUT=$ROOT/target/$PROFILE
if [ -n "$TARGET" ] && [ "$TARGET" != "$HOST" ]; then
    step "cross-building for $TARGET ($PROFILE)"
    if ! command -v zig >/dev/null 2>&1; then
        echo "zig not found on PATH; cross-building needs it (https://ziglang.org/download/ or your package manager)" >&2
        if [ "$DRY" = 1 ]; then echo "[dry-run] would stop here"; else exit 1; fi
    fi
    run rustup target add "$TARGET"
    if ! command -v cargo-zigbuild >/dev/null 2>&1; then
        step "installing cargo-zigbuild (user-local)"
        run cargo install --locked cargo-zigbuild
    fi
    run cargo zigbuild --profile "$PROFILE" --locked --target "$TARGET" --bin tuff
    OUT=$ROOT/target/$TARGET/$PROFILE
else
    step "building ($PROFILE)"
    run cargo build --profile "$PROFILE" --locked --bin tuff
fi

step "installing to $PREFIX"
run mkdir -p "$PREFIX"
run install -m 755 "$OUT/tuff" "$PREFIX/tuff"

if [ "$CLEAN" = 1 ]; then
    step "removing target/"
    run rm -rf "$ROOT/target"
fi
step "done: run $PREFIX/tuff"
