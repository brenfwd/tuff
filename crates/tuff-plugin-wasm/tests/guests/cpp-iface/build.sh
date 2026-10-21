#!/bin/sh
# Builds build/iface.wasm with the C SDK's build script (see
# crates/tuff-plugin-sdk/c/build.sh for the toolchain it needs).
set -eu
cd "$(dirname "$0")"
CXXFLAGS="${CXXFLAGS:--Werror=deprecated}" exec ../../../../tuff-plugin-sdk/c/build.sh build/iface.wasm iface.cpp
