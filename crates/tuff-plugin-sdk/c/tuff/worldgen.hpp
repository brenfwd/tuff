// SPDX-License-Identifier: MIT
// Dimensions and chunk generation. Hooks on the plugin type, both optional:
//
//     static void declare_dimensions(tuff::Dimensions &out);
//     static tuff::Status generate_chunk(const tuff::ChunkRequest &req, tuff::ChunkBuilder &chunk);
//
// declare_dimensions adds tuff::Dimension values (biomes, spawn, sea level);
// generate_chunk fills one column from req alone, deterministically, since
// the host runs it on instances with every import trapping.
#ifndef TUFF_WORLDGEN_HPP
#define TUFF_WORLDGEN_HPP

namespace tuff {

namespace detail {

inline void put_str(Str &dst, Str value) {
    dst = Str{nullptr, 0};
    if (value.len) {
        raw::plugin_string_dup_n(&dst, reinterpret_cast<const char *>(value.ptr), value.len);
    }
}

} // namespace detail

// A dimension the plugin adds at startup.
class Dimension {
  public:
    Dimension(Str key, Str dimension_type) {
        d_ = {};
        detail::put_str(d_.key, key);
        detail::put_str(d_.dimension_type, dimension_type);
    }
    Dimension(const char *key, const char *dimension_type)
        : Dimension(str(key), str(dimension_type)) {}
    Dimension(Dimension &&other) : d_(other.d_) { other.d_ = {}; }
    Dimension(const Dimension &) = delete;
    Dimension &operator=(const Dimension &) = delete;
    ~Dimension() { raw::exports_tuff_plugin_worldgen_dimension_decl_free(&d_); }

    // The dimension type as data pack JSON; without it the vanilla type
    // named by the constructor is used.
    Dimension &type_json(Str json) {
        raw::plugin_string_free(&d_.type_json);
        detail::put_str(d_.type_json, json);
        return *this;
    }
    Dimension &natural_spawning(bool on) {
        d_.natural_spawning = on;
        return *this;
    }
    Dimension &sea_level(int32_t y) {
        d_.sea_level = y;
        return *this;
    }
    Dimension &spawn(BlockPos at) {
        d_.spawn = at;
        return *this;
    }
    // A non-vanilla biome the generator uses, as data pack JSON.
    Dimension &biome(Str key, Str json) {
        auto *grown = static_cast<raw::tuff_plugin_types_biome_decl_t *>(
            realloc(d_.biomes.ptr, (d_.biomes.len + 1) * sizeof(raw::tuff_plugin_types_biome_decl_t)));
        if (!grown) {
            return *this;
        }
        d_.biomes.ptr = grown;
        auto &slot = grown[d_.biomes.len++];
        detail::put_str(slot.key, key);
        detail::put_str(slot.json, json);
        return *this;
    }

    raw::tuff_plugin_types_dimension_decl_t release() {
        auto out = d_;
        d_ = {};
        return out;
    }

  private:
    raw::tuff_plugin_types_dimension_decl_t d_;
};

// The dimensions declare_dimensions adds.
class Dimensions {
  public:
    explicit Dimensions(raw::exports_tuff_plugin_worldgen_list_dimension_decl_t *out) : out_(out) {
        out_->ptr = nullptr;
        out_->len = 0;
    }
    void add(Dimension dimension) {
        auto *grown = static_cast<raw::tuff_plugin_types_dimension_decl_t *>(
            realloc(out_->ptr, (out_->len + 1) * sizeof(raw::tuff_plugin_types_dimension_decl_t)));
        if (grown) {
            out_->ptr = grown;
            grown[out_->len++] = dimension.release();
        }
    }

  private:
    raw::exports_tuff_plugin_worldgen_list_dimension_decl_t *out_;
};

// What generate_chunk was asked for.
struct ChunkRequest {
    Str dimension;
    int64_t seed;
    int32_t chunk_x;
    int32_t chunk_z;
    int32_t min_y;
    int32_t height;
};

// One column of block states, `height` layers from min_y up. Every block
// starts as the first palette entry set or "minecraft:air".
class ChunkBuilder {
  public:
    explicit ChunkBuilder(const ChunkRequest &req) : req_(req) {
        size_t n = req.height > 0 ? static_cast<size_t>(req.height) * 256 : 0;
        blocks_ = static_cast<uint8_t *>(calloc(n ? n : 1, 1));
        len_ = blocks_ ? n : 0;
        palette_[0] = String::copy("minecraft:air");
        palette_len_ = 1;
    }
    ChunkBuilder(const ChunkBuilder &) = delete;
    ChunkBuilder &operator=(const ChunkBuilder &) = delete;
    ~ChunkBuilder() { free(blocks_); }

    // Sets the block at chunk-local x and z (0..15) and world y. Returns
    // false when the position is outside the column or the 256-state
    // palette is full.
    bool set(int x, int y, int z, Str state) {
        if (x < 0 || x > 15 || z < 0 || z > 15 || y < req_.min_y || y >= req_.min_y + req_.height) {
            return false;
        }
        int index = palette_index(state);
        if (index < 0) {
            return false;
        }
        blocks_[static_cast<size_t>(y - req_.min_y) * 256 + z * 16 + x] = static_cast<uint8_t>(index);
        return true;
    }
    bool set(int x, int y, int z, const char *state) { return set(x, y, z, str(state)); }

    // Fills y0 through y1 inclusive across the whole column.
    bool fill(int y0, int y1, Str state) {
        int index = palette_index(state);
        if (index < 0) {
            return false;
        }
        for (int y = y0 < req_.min_y ? req_.min_y : y0; y <= y1 && y < req_.min_y + req_.height;
             y++) {
            size_t base = static_cast<size_t>(y - req_.min_y) * 256;
            for (size_t i = 0; i < 256; i++) {
                blocks_[base + i] = static_cast<uint8_t>(index);
            }
        }
        return true;
    }
    bool fill(int y0, int y1, const char *state) { return fill(y0, y1, str(state)); }

    // The biome of the whole column.
    void biome(Str key) { biome_ = String::copy(key); }
    void biome(const char *key) { biome_ = String::copy(key); }

    // Hands the column to the host's result.
    void finish(raw::exports_tuff_plugin_worldgen_generated_chunk_t *out) {
        auto *palette = static_cast<Str *>(malloc(palette_len_ * sizeof(Str)));
        for (size_t i = 0; palette && i < palette_len_; i++) {
            palette[i] = palette_[i].release();
        }
        out->palette = {palette, palette ? palette_len_ : 0};
        out->blocks = {blocks_, len_};
        blocks_ = nullptr;
        len_ = 0;
        if (biome_.len() == 0) {
            biome_ = String::copy("minecraft:plains");
        }
        out->biome = biome_.release();
    }

  private:
    int palette_index(Str state) {
        for (size_t i = 0; i < palette_len_; i++) {
            if (palette_[i].view() == state) {
                return static_cast<int>(i);
            }
        }
        if (palette_len_ >= 256) {
            return -1;
        }
        palette_[palette_len_] = String::copy(state);
        return static_cast<int>(palette_len_++);
    }

    ChunkRequest req_;
    uint8_t *blocks_ = nullptr;
    size_t len_ = 0;
    String palette_[256];
    size_t palette_len_ = 0;
    String biome_;
};

namespace detail {

inline void init_once() {
    static bool ready = false;
    if (!ready) {
        raw::__wasm_call_ctors();
        ready = true;
    }
}

template <class P> void declare_dimensions(raw::exports_tuff_plugin_worldgen_list_dimension_decl_t *ret) {
    Dimensions out(ret);
    if constexpr (requires { P::declare_dimensions(out); }) {
        init_once();
        P::declare_dimensions(out);
    }
}

template <class P>
bool generate_chunk(Str *dimension, int64_t seed, int32_t chunk_x, int32_t chunk_z, int32_t min_y,
                    int32_t height, raw::exports_tuff_plugin_worldgen_generated_chunk_t *ret,
                    Str *err) {
    bool ok = false;
    if constexpr (requires(ChunkBuilder &c) {
                      P::generate_chunk(ChunkRequest{}, c);
                  }) {
        init_once();
        ChunkRequest req{*dimension, seed, chunk_x, chunk_z, min_y, height};
        ChunkBuilder chunk(req);
        Status status = P::generate_chunk(req, chunk);
        if (status) {
            chunk.finish(ret);
            ok = true;
        } else {
            *err = status.take_error().release();
        }
    } else {
        *err = String::copy("the plugin has no chunk generator").release();
    }
    raw::plugin_string_free(dimension);
    return ok;
}

} // namespace detail

} // namespace tuff

#endif
