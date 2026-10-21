#!/bin/sh
# Builds build/hello.wasm with the tuff C++ SDK (see ../../crates/tuff-plugin-sdk/c/build.sh
# for the toolchain: clang and wasm-ld; set CC, CXX and WASM_LD to choose).
set -eu
cd "$(dirname "$0")"
CXXFLAGS="${CXXFLAGS:--Werror=deprecated}" exec "../../crates/tuff-plugin-sdk/c/build.sh" build/hello.wasm hello.cpp
