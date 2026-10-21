#!/bin/sh
# Builds a plugin from C or C++ sources against the tuff C SDK:
#
#   crates/tuff-plugin-sdk/c/build.sh out.wasm plugin.c [more.c|more.cpp ...]
#
# Freestanding clang (15+, wasm32 and C++20 coroutines) and wasm-ld (Debian/Ubuntu: apt install clang lld;
# macOS: brew install llvm lld), no libc or wasi-sdk: include/ and libc.c
# supply what the bindings and the SDK need. Override the tools with CC,
# CXX and WASM_LD. When WASM_LD is unset and wasm-ld is not on PATH, the
# wasm-ld shipped with the rustup toolchain is used. The result is a plain
# core module.
set -eu
[ $# -ge 2 ] || { echo "usage: $0 out.wasm source..." >&2; exit 2; }
SDK=$(cd "$(dirname "$0")" && pwd)
OUT=$1
shift
CC=${CC:-clang}
CXX=${CXX:-clang++}
if [ -z "${WASM_LD:-}" ]; then
    WASM_LD=wasm-ld
    if ! command -v wasm-ld >/dev/null 2>&1 && command -v rustc >/dev/null 2>&1; then
        HOST=$(rustc -vV | sed -n 's/^host: //p')
        RUSTUP_LD="$(rustc --print sysroot)/lib/rustlib/$HOST/bin/gcc-ld/wasm-ld"
        [ -x "$RUSTUP_LD" ] && WASM_LD=$RUSTUP_LD
    fi
fi
COMMON="--target=wasm32 -O2 -nostdlib -ffreestanding -fno-builtin -I$SDK/include -I$SDK/bindings -I$SDK"
OBJ=$(mktemp -d)
trap 'rm -rf "$OBJ"' EXIT
i=0
for src in "$SDK/tuff.c" "$SDK/libc.c" "$@"; do
    i=$((i + 1))
    case "$src" in
    *.cpp | *.cc) "$CXX" $COMMON -std=c++20 -fno-exceptions -fno-rtti ${CXXFLAGS:-} -Wall -Wextra -Werror -c "$src" -o "$OBJ/$i.o" ;;
    *) "$CC" $COMMON -std=c11 -Wall -Wextra -Werror -c "$src" -o "$OBJ/$i.o" ;;
    esac
done
"$CC" $COMMON -std=c11 -c "$SDK/bindings/plugin.c" -o "$OBJ/plugin.o"
mkdir -p "$(dirname "$OUT")"
$WASM_LD --no-entry --strip-debug -o "$OUT" "$OBJ"/*.o
echo "built $OUT"
