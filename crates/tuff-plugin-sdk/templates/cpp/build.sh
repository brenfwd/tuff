#!/bin/sh
# Builds build/__ID__.wasm with the tuff C++ SDK (see __SDK__/c/build.sh
# for the toolchain: clang and wasm-ld; set CC, CXX and WASM_LD to choose).
set -eu
cd "$(dirname "$0")"
CXXFLAGS="${CXXFLAGS:--Werror=deprecated}" exec "__SDK__/c/build.sh" build/__ID__.wasm __ID__.cpp
