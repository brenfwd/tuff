#!/bin/sh
# Builds build/wit-greeter.wasm with the C SDK's build script (see
# crates/tuff-plugin-sdk/c/build.sh for the toolchain it needs).
set -eu
cd "$(dirname "$0")"
exec ../../../../tuff-plugin-sdk/c/build.sh build/wit-greeter.wasm greeter.c
