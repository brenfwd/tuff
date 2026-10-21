// SPDX-License-Identifier: MIT
// The raw binding layer: tuff_* functions, generated binding types and
// constants, and the exports_tuff_plugin_guest_* entry points. tuff.hpp
// keeps them in tuff::raw and drops the binding constants, so only
// conformance fixtures that exercise the raw layer on purpose include this,
// in place of tuff.hpp. A C++ definition of a guest export must be declared
// extern "C".
#ifndef TUFF_RAW_H
#define TUFF_RAW_H
#ifdef __cplusplus
#ifdef TUFF_HPP
#error "include tuff/raw.h in place of tuff.hpp, not after it"
#endif
#include "../tuff.hpp"
using namespace tuff::raw;
#define TUFF_MANIFEST(line) TUFF_DETAIL_MANIFEST(line)
#else
#include "../tuff.h"
#endif
#endif
