// SPDX-License-Identifier: MIT
// Startup content declarations. A hook on the plugin type, optional:
//
//     static void declare_content(tuff::Content &out);
//
// It adds custom items, entities, resource pack assets, recipes and
// advancements; every id must be in a namespace the owner granted with
// cap.content. Content is read once at startup. Custom blocks are not
// supported. Like worldgen, it runs on an instance with every import
// trapping, so it may depend on nothing but its own code.
#ifndef TUFF_CONTENT_HPP
#define TUFF_CONTENT_HPP

namespace tuff {

// A custom item: a vanilla base item with components on top.
class ItemDecl {
  public:
    ItemDecl(Str id, Str base) {
        d_ = {};
        detail::put_str(d_.id, id);
        detail::put_str(d_.base, base);
    }
    ItemDecl(const char *id, const char *base) : ItemDecl(str(id), str(base)) {}
    ItemDecl(ItemDecl &&other) : d_(other.d_) { other.d_ = {}; }
    ItemDecl(const ItemDecl &) = delete;
    ItemDecl &operator=(const ItemDecl &) = delete;
    ~ItemDecl() { raw::tuff_plugin_types_item_decl_free(&d_); }

    // The item_model the plugin's assets provide.
    ItemDecl &model(Str key) {
        if (d_.model.is_some) {
            raw::plugin_string_free(&d_.model.val);
        }
        d_.model.is_some = true;
        detail::put_str(d_.model.val, key);
        return *this;
    }
    // Data components as an SNBT compound keyed by component id.
    ItemDecl &components(Str snbt) {
        raw::plugin_string_free(&d_.components);
        detail::put_str(d_.components, snbt);
        return *this;
    }

    raw::tuff_plugin_types_item_decl_t release() {
        auto out = d_;
        d_ = {};
        return out;
    }

  private:
    raw::tuff_plugin_types_item_decl_t d_;
};

// A set of resource pack files under assets/<namespace>/.
class AssetDecl {
  public:
    AssetDecl(Str name, Str ns) {
        d_ = {};
        detail::put_str(d_.name, name);
        detail::put_str(d_.namespace_, ns);
    }
    AssetDecl(const char *name, const char *ns) : AssetDecl(str(name), str(ns)) {}
    AssetDecl(AssetDecl &&other) : d_(other.d_) { other.d_ = {}; }
    AssetDecl(const AssetDecl &) = delete;
    AssetDecl &operator=(const AssetDecl &) = delete;
    ~AssetDecl() { raw::tuff_plugin_types_asset_decl_free(&d_); }

    AssetDecl &file(Str path, const uint8_t *data, size_t len) {
        auto *grown = static_cast<raw::tuff_plugin_types_asset_file_t *>(realloc(
            d_.files.ptr, (d_.files.len + 1) * sizeof(raw::tuff_plugin_types_asset_file_t)));
        if (!grown) {
            return *this;
        }
        d_.files.ptr = grown;
        auto &slot = grown[d_.files.len++];
        slot = {};
        detail::put_str(slot.path, path);
        if (len) {
            slot.data.ptr = static_cast<uint8_t *>(malloc(len));
            if (slot.data.ptr) {
                memcpy(slot.data.ptr, data, len);
                slot.data.len = len;
            }
        }
        return *this;
    }
    AssetDecl &file(Str path, Str text) { return file(path, text.ptr, text.len); }

    raw::tuff_plugin_types_asset_decl_t release() {
        auto out = d_;
        d_ = {};
        return out;
    }

  private:
    raw::tuff_plugin_types_asset_decl_t d_;
};

// Everything declare_content adds.
class Content {
  public:
    explicit Content(raw::exports_tuff_plugin_content_content_decl_t *out) : out_(out) {
        memset(out_, 0, sizeof *out_);
    }

    void item(ItemDecl item) { push(out_->items.ptr, out_->items.len, item.release()); }
    void asset(AssetDecl asset) { push(out_->assets.ptr, out_->assets.len, asset.release()); }
    // A custom entity saved and sent as the vanilla stand-in type.
    void entity(Str id, Str stand_in) {
        raw::tuff_plugin_types_entity_decl_t e{};
        detail::put_str(e.id, id);
        detail::put_str(e.stand_in, stand_in);
        push(out_->entities.ptr, out_->entities.len, e);
    }
    // A crafting recipe as data pack JSON.
    void recipe(Str id, Str json) { data_file(out_->recipes.ptr, out_->recipes.len, id, json); }
    // An advancement as data pack JSON.
    void advancement(Str id, Str json) {
        data_file(out_->advancements.ptr, out_->advancements.len, id, json);
    }

  private:
    template <class T> static void push(T *&ptr, size_t &len, T value) {
        auto *grown = static_cast<T *>(realloc(ptr, (len + 1) * sizeof(T)));
        if (grown) {
            ptr = grown;
            grown[len++] = value;
        }
    }
    static void data_file(raw::tuff_plugin_types_data_file_t *&ptr, size_t &len, Str id, Str json) {
        raw::tuff_plugin_types_data_file_t f{};
        detail::put_str(f.id, id);
        detail::put_str(f.json, json);
        push(ptr, len, f);
    }

    raw::exports_tuff_plugin_content_content_decl_t *out_;
};

namespace detail {

template <class P> void declare_content(raw::exports_tuff_plugin_content_content_decl_t *ret) {
    Content out(ret);
    if constexpr (requires { P::declare_content(out); }) {
        init_once();
        P::declare_content(out);
    }
}

} // namespace detail

} // namespace tuff

#endif
