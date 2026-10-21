// SPDX-License-Identifier: MIT
// Typed settings, declared as constant-initialized statics:
//
//     static constinit tuff::Setting<tuff::String> greeting("greeting", "Hello", "What /hello
//     says"); TUFF_CHECK(greeting.declare());          // while registering tuff::String text =
//     greeting.get();      // the live value, else the default
//
// T is bool, int64_t or String; owners edit the setting as
// plugins.<id>.<key>.
#ifndef TUFF_CONFIG_HPP
#define TUFF_CONFIG_HPP

namespace tuff {

namespace detail {
template <class T> struct SettingTypes {
    using Default = T;
    using View = T;
};
template <> struct SettingTypes<String> {
    using Default = const char *;
    using View = Str;
};
} // namespace detail

using ConfigValue = raw::tuff_plugin_types_config_value_t;

template <class T> class Setting {
    static_assert(same_type<T, bool> || same_type<T, int64_t> || same_type<T, String>,
                  "a setting holds bool, int64_t or String");

  public:
    // How the default is written: const char * for a String setting.
    using Default = typename detail::SettingTypes<T>::Default;
    // What on_change hands over: Str for a String setting.
    using View = typename detail::SettingTypes<T>::View;

    constexpr Setting(const char *key, Default fallback, const char *description,
                      bool secret = false)
        : key_(key), default_(fallback), description_(description), secret_(secret) {}

    const char *key() const { return key_; }

    // Declares the setting; only while registering.
    bool declare() const {
        raw::tuff_plugin_types_config_decl_t decl;
        decl.key = raw::tuff_str(key_);
        decl.default_ = value_of(default_);
        decl.description = raw::tuff_str(description_);
        decl.secret = secret_;
        raw::tuff_plugin_host_register_error_t err;
        if (!raw::tuff_plugin_host_declare_config(&decl, &err)) {
            raw::tuff_plugin_host_register_error_free(&err);
            return false;
        }
        return true;
    }

    // The live value; the default when the host has none.
    T get() const {
        Str key = raw::tuff_str(key_);
        ConfigValue value;
        if (raw::tuff_plugin_host_config_get(&key, &value)) {
            if (value.tag == tag()) {
                return take(value);
            }
            raw::tuff_plugin_types_config_value_free(&value);
        }
        if constexpr (same_type<T, String>) {
            return String::copy(default_);
        } else {
            return default_;
        }
    }

    // Sets and saves the value, as an owner would.
    bool set(View value) const {
        Str key = raw::tuff_str(key_);
        ConfigValue v = value_of(value);
        HostError err = {};
        if (!raw::tuff_plugin_host_config_set(&key, &v, &err)) {
            (void)host_error_text(err);
            return false;
        }
        return true;
    }

    // Runs `handler(View)` with each new value; only while registering.
    template <class F> bool on_change(F handler) const {
        const char *key = key_;
        return listen<ev::ConfigChange>()
            .when(
                [key](const ev::ConfigChange::payload &c) { return raw::tuff_str_eq(c.key, key); })
            .run([handler =
                      Function<void(View)>(move(handler))](const ev::ConfigChange::payload &c) {
                if (c.value.tag == tag()) {
                    handler(view(c.value));
                }
            });
    }

  private:
    static constexpr uint8_t tag() {
        if constexpr (same_type<T, bool>) {
            return TUFF_PLUGIN_TYPES_CONFIG_VALUE_BOOLEAN;
        } else if constexpr (same_type<T, int64_t>) {
            return TUFF_PLUGIN_TYPES_CONFIG_VALUE_INTEGER;
        } else {
            return TUFF_PLUGIN_TYPES_CONFIG_VALUE_TEXT;
        }
    }

    template <class V> static ConfigValue value_of(V value) {
        ConfigValue v;
        v.tag = tag();
        if constexpr (same_type<T, bool>) {
            v.val.boolean = value;
        } else if constexpr (same_type<T, int64_t>) {
            v.val.integer = value;
        } else if constexpr (same_type<V, Str>) {
            v.val.text = value;
        } else {
            v.val.text = raw::tuff_str(value);
        }
        return v;
    }

    static View view(const ConfigValue &v) {
        if constexpr (same_type<T, bool>) {
            return v.val.boolean;
        } else if constexpr (same_type<T, int64_t>) {
            return v.val.integer;
        } else {
            return v.val.text;
        }
    }

    static T take(ConfigValue &v) {
        if constexpr (same_type<T, String>) {
            return String::adopt(v.val.text);
        } else {
            return view(v);
        }
    }

    const char *key_;
    Default default_;
    const char *description_;
    bool secret_;
};

} // namespace tuff

#endif
