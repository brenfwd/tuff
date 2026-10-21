// SPDX-License-Identifier: MIT
// Typed values in the plugin's persistent store:
//
//     static constinit tuff::StoredMap<uint32_t> visits("visits");
//     tuff::Option<uint32_t> n = visits.update(player.uuid, [](tuff::Option<uint32_t> old) {
//         return old.value_or(0) + 1;
//     });
//
// Values are text, as the Rust SDK stores them: numbers in decimal,
// true/false, strings as UTF-8 and uuids as 32 hex digits. A map entry's
// key is <prefix>/<key>.
#ifndef TUFF_STORE_HPP
#define TUFF_STORE_HPP

namespace tuff {

namespace detail {

inline bool digits(Str s, size_t from, uint64_t &out) {
    if (from >= s.len) {
        return false;
    }
    out = 0;
    for (size_t i = from; i < s.len; i++) {
        uint8_t c = s.ptr[i];
        if (c < '0' || c > '9' || out > (UINT64_MAX - (c - '0')) / 10) {
            return false;
        }
        out = out * 10 + (c - '0');
    }
    return true;
}

inline int hex(uint8_t c) {
    if (c >= '0' && c <= '9') {
        return c - '0';
    }
    if (c >= 'a' && c <= 'f') {
        return c - 'a' + 10;
    }
    if (c >= 'A' && c <= 'F') {
        return c - 'A' + 10;
    }
    return -1;
}

} // namespace detail

// How a T is stored: encode into a Text, decode from bytes.
template <class T> struct Codec;

template <class T>
    requires(same_type<T, uint8_t> || same_type<T, uint16_t> || same_type<T, uint32_t> ||
             same_type<T, uint64_t>)
struct Codec<T> {
    static void encode(Text &out, T v) { out << static_cast<unsigned long long>(v); }
    static Option<T> decode(Str s) {
        uint64_t v;
        size_t from = s.len && s.ptr[0] == '+' ? 1 : 0;
        if (!detail::digits(s, from, v) || v > static_cast<T>(~T(0))) {
            return {};
        }
        return static_cast<T>(v);
    }
};

template <class T>
    requires(same_type<T, int8_t> || same_type<T, int16_t> || same_type<T, int32_t> ||
             same_type<T, int64_t>)
struct Codec<T> {
    static void encode(Text &out, T v) { out << static_cast<long long>(v); }
    static Option<T> decode(Str s) {
        bool negative = s.len && s.ptr[0] == '-';
        size_t from = s.len && (s.ptr[0] == '-' || s.ptr[0] == '+') ? 1 : 0;
        uint64_t v;
        uint64_t max = static_cast<uint64_t>(static_cast<T>(~0ull >> (65 - 8 * sizeof(T))));
        if (!detail::digits(s, from, v) || v > max + (negative ? 1 : 0)) {
            return {};
        }
        return negative ? static_cast<T>(0 - v) : static_cast<T>(v);
    }
};

template <> struct Codec<bool> {
    static void encode(Text &out, bool v) { out << (v ? "true" : "false"); }
    static Option<bool> decode(Str s) {
        if (s == raw::tuff_str("true")) {
            return true;
        }
        if (s == raw::tuff_str("false")) {
            return false;
        }
        return {};
    }
};

template <> struct Codec<String> {
    static void encode(Text &out, const String &v) { out << v; }
    static Option<String> decode(Str s) { return String::copy(s); }
};

template <> struct Codec<Uuid> {
    static void encode(Text &out, Uuid v) { out << v; }
    static Option<Uuid> decode(Str s) {
        if (s.len != 32) {
            return {};
        }
        uint64_t halves[2] = {0, 0};
        for (size_t i = 0; i < 32; i++) {
            int d = detail::hex(s.ptr[i]);
            if (d < 0) {
                return {};
            }
            halves[i / 16] = halves[i / 16] << 4 | static_cast<uint64_t>(d);
        }
        return Uuid{halves[0], halves[1]};
    }
};

namespace detail {

template <class T> Option<T> load(Str key) {
    raw::plugin_list_u8_t bytes;
    if (!raw::tuff_plugin_host_storage_get(&key, &bytes)) {
        return {};
    }
    Option<T> value = Codec<T>::decode(Str{bytes.ptr, bytes.len});
    raw::plugin_list_u8_free(&bytes);
    return value;
}

template <class T> bool save(Str key, const T *value) {
    Text encoded;
    raw::plugin_list_u8_t bytes = {nullptr, 0};
    if (value) {
        Codec<T>::encode(encoded, *value);
        Str s = encoded.view();
        bytes = raw::plugin_list_u8_t{s.ptr, s.len};
    }
    HostError err = {};
    if (!raw::tuff_plugin_host_storage_set(&key, value ? &bytes : nullptr, &err)) {
        (void)host_error_text(err);
        return false;
    }
    return true;
}

} // namespace detail

// One stored value.
template <class T> class Stored {
  public:
    constexpr explicit Stored(const char *key) : key_(key) {}
    Option<T> get() const { return detail::load<T>(raw::tuff_str(key_)); }
    bool set(const T &value) const { return detail::save(raw::tuff_str(key_), &value); }
    bool remove() const { return detail::save<T>(raw::tuff_str(key_), nullptr); }
    // Stores f(old value) and gives it back; nothing when refused.
    template <class F> Option<T> update(F f) const {
        T value = f(get());
        if (!set(value)) {
            return {};
        }
        return value;
    }

  private:
    const char *key_;
};

// Stored values under one prefix, by key: a player's uuid or a name.
template <class T> class StoredMap {
  public:
    constexpr explicit StoredMap(const char *prefix) : prefix_(prefix) {}
    template <class K> Text key(const K &key) const {
        Text out;
        out << prefix_ << "/" << key;
        return out;
    }
    template <class K> Option<T> get(const K &k) const { return detail::load<T>(key(k).view()); }
    template <class K> bool set(const K &k, const T &value) const {
        return detail::save(key(k).view(), &value);
    }
    template <class K> bool remove(const K &k) const {
        return detail::save<T>(key(k).view(), nullptr);
    }
    // Stores f(old value) under `k` and gives it back; nothing when refused.
    template <class K, class F> Option<T> update(const K &k, F f) const {
        Text at = key(k);
        T value = f(detail::load<T>(at.view()));
        if (!detail::save(at.view(), &value)) {
            return {};
        }
        return value;
    }

  private:
    const char *prefix_;
};

} // namespace tuff

#endif
