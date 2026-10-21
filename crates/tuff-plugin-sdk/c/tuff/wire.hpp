// SPDX-License-Identifier: MIT
// The binary codec of plugin interface payloads, the same wire format as the
// Rust SDK's `wire` module: little-endian fixed-width ints and floats, bool
// as one byte (0 or 1), char as a u32 scalar, string and list as a LEB128
// u32 length plus items, option/result/variant/enum as a LEB128 discriminant
// plus payload, record and tuple fields in order, flags as a u64 bitset.
//
// Payloads come from other plugins, so a Reader never trusts input: every
// length is checked against the remaining bytes before anything is
// allocated, and a bad payload makes decode return false.
//
// Generated interface headers (`tuff-plugin-sdk-gen interface <wit> --cpp
// <out.hpp>`) define `encode(wire::Writer &, const T &)` and
// `bool decode(wire::Reader &, T &)` for every type of the interface.
#ifndef TUFF_WIRE_HPP
#define TUFF_WIRE_HPP

namespace tuff {

template <class T> using List = detail::Vec<T>;

// WIT result<T, E>; `ok` or `err` holds the value.
template <class T, class E> struct WireResult {
    bool is_err = false;
    Option<T> ok;
    Option<E> err;

    static WireResult make_ok(T value) {
        WireResult out;
        out.ok = Option<T>(move(value));
        return out;
    }
    static WireResult make_err(E error) {
        WireResult out;
        out.is_err = true;
        out.err = Option<E>(move(error));
        return out;
    }
};

// WIT tuple of 1 to 4 elements.
template <class... T> struct Tuple;
template <class A> struct Tuple<A> {
    A f0{};
};
template <class A, class B> struct Tuple<A, B> {
    A f0{};
    B f1{};
};
template <class A, class B, class C> struct Tuple<A, B, C> {
    A f0{};
    B f1{};
    C f2{};
};
template <class A, class B, class C, class D> struct Tuple<A, B, C, D> {
    A f0{};
    B f1{};
    C f2{};
    D f3{};
};

namespace wire {

// Zero-sized list elements are capped to this many per list.
inline constexpr size_t MAX_ZERO_SIZED_ITEMS = 1 << 16;

// A growable output buffer; an allocation failure makes ok() false.
class Writer {
  public:
    Writer() = default;
    Writer(const Writer &) = delete;
    Writer &operator=(const Writer &) = delete;
    ~Writer() { free(buf_); }

    bool ok() const { return !failed_; }
    size_t size() const { return len_; }

    void bytes(const void *data, size_t n) {
        if (failed_ || n == 0) {
            return;
        }
        if (len_ + n > cap_) {
            size_t cap = cap_ ? cap_ : 64;
            while (cap < len_ + n) {
                cap *= 2;
            }
            uint8_t *grown = static_cast<uint8_t *>(malloc(cap));
            if (!grown) {
                failed_ = true;
                return;
            }
            if (len_) {
                memcpy(grown, buf_, len_);
            }
            free(buf_);
            buf_ = grown;
            cap_ = cap;
        }
        memcpy(buf_ + len_, data, n);
        len_ += n;
    }

    void leb(uint32_t value) {
        for (;;) {
            uint8_t byte = value & 0x7f;
            value >>= 7;
            if (value == 0) {
                bytes(&byte, 1);
                return;
            }
            byte |= 0x80;
            bytes(&byte, 1);
        }
    }

    // The encoded bytes (empty after a failure; check ok()).
    Bytes finish() {
        if (failed_ || len_ == 0) {
            return Bytes();
        }
        Bytes out = Bytes::adopt(raw::plugin_list_u8_t{buf_, len_});
        buf_ = nullptr;
        len_ = cap_ = 0;
        return out;
    }

  private:
    uint8_t *buf_ = nullptr;
    size_t len_ = 0;
    size_t cap_ = 0;
    bool failed_ = false;
};

// A cursor over untrusted input.
class Reader {
  public:
    Reader(const uint8_t *data, size_t len) : p_(data), n_(len) {}
    explicit Reader(Str text) : p_(text.ptr), n_(text.len) {}

    size_t remaining() const { return n_; }
    bool finished() const { return n_ == 0; }

    bool take(void *out, size_t n) {
        if (n_ < n) {
            return false;
        }
        if (n) {
            memcpy(out, p_, n);
        }
        p_ += n;
        n_ -= n;
        return true;
    }

    // A view of the next `n` bytes, or null.
    const uint8_t *span(size_t n) {
        if (n_ < n) {
            return nullptr;
        }
        const uint8_t *at = p_;
        p_ += n;
        n_ -= n;
        return at;
    }

    bool leb(uint32_t &out) {
        uint32_t value = 0;
        for (int index = 0; index < 5; index++) {
            uint8_t byte;
            if (!take(&byte, 1)) {
                return false;
            }
            uint32_t bits = byte & 0x7f;
            if (index == 4 && bits > 0x0f) {
                return false;
            }
            value |= bits << (7 * index);
            if ((byte & 0x80) == 0) {
                out = value;
                return true;
            }
        }
        return false;
    }

    // A list length that the remaining input can hold at `min_item` bytes each.
    bool len(size_t &out, size_t min_item) {
        uint32_t n;
        if (!leb(n)) {
            return false;
        }
        if (min_item == 0 ? n > MAX_ZERO_SIZED_ITEMS : n > n_ / min_item) {
            return false;
        }
        out = n;
        return true;
    }

    // A discriminant below `count`.
    bool case_of(uint32_t &out, uint32_t count) {
        uint32_t n;
        if (!leb(n) || n >= count) {
            return false;
        }
        out = n;
        return true;
    }

  private:
    const uint8_t *p_;
    size_t n_;
};

// True when `data` is well-formed UTF-8 (no overlong forms or surrogates).
inline bool valid_utf8(const uint8_t *data, size_t len) {
    size_t i = 0;
    while (i < len) {
        uint8_t b = data[i];
        size_t extra;
        uint32_t scalar;
        uint32_t least;
        if (b < 0x80) {
            i++;
            continue;
        } else if ((b & 0xe0) == 0xc0) {
            extra = 1;
            scalar = b & 0x1f;
            least = 0x80;
        } else if ((b & 0xf0) == 0xe0) {
            extra = 2;
            scalar = b & 0x0f;
            least = 0x800;
        } else if ((b & 0xf8) == 0xf0) {
            extra = 3;
            scalar = b & 0x07;
            least = 0x10000;
        } else {
            return false;
        }
        if (len - i <= extra) {
            return false;
        }
        for (size_t k = 1; k <= extra; k++) {
            if ((data[i + k] & 0xc0) != 0x80) {
                return false;
            }
            scalar = (scalar << 6) | (data[i + k] & 0x3f);
        }
        if (scalar < least || scalar > 0x10ffff || (scalar >= 0xd800 && scalar <= 0xdfff)) {
            return false;
        }
        i += extra + 1;
    }
    return true;
}

// The fewest bytes any encoding of T takes; each generated type specializes it.
template <class T> struct MinSize;

#define TUFF_WIRE_FIXED(T)                                                                         \
    template <> struct MinSize<T> {                                                                \
        static constexpr size_t value = sizeof(T);                                                 \
    };                                                                                             \
    inline void encode(Writer &w, T v) { w.bytes(&v, sizeof(T)); }                                 \
    inline bool decode(Reader &r, T &out) { return r.take(&out, sizeof(T)); }

TUFF_WIRE_FIXED(uint8_t)
TUFF_WIRE_FIXED(int8_t)
TUFF_WIRE_FIXED(uint16_t)
TUFF_WIRE_FIXED(int16_t)
TUFF_WIRE_FIXED(uint32_t)
TUFF_WIRE_FIXED(int32_t)
TUFF_WIRE_FIXED(uint64_t)
TUFF_WIRE_FIXED(int64_t)
TUFF_WIRE_FIXED(float)
TUFF_WIRE_FIXED(double)
#undef TUFF_WIRE_FIXED

template <> struct MinSize<bool> {
    static constexpr size_t value = 1;
};
inline void encode(Writer &w, bool v) {
    uint8_t byte = v ? 1 : 0;
    w.bytes(&byte, 1);
}
inline bool decode(Reader &r, bool &out) {
    uint8_t byte;
    if (!r.take(&byte, 1) || byte > 1) {
        return false;
    }
    out = byte == 1;
    return true;
}

template <> struct MinSize<char32_t> {
    static constexpr size_t value = 4;
};
inline void encode(Writer &w, char32_t v) {
    uint32_t scalar = v;
    w.bytes(&scalar, 4);
}
inline bool decode(Reader &r, char32_t &out) {
    uint32_t scalar;
    if (!r.take(&scalar, 4) || scalar > 0x10ffff || (scalar >= 0xd800 && scalar <= 0xdfff)) {
        return false;
    }
    out = static_cast<char32_t>(scalar);
    return true;
}

// The WIT `_` of a result with no ok or error type: zero bytes.
template <> struct MinSize<Unit> {
    static constexpr size_t value = 0;
};
inline void encode(Writer &, const Unit &) {}
inline bool decode(Reader &, Unit &) { return true; }

template <> struct MinSize<String> {
    static constexpr size_t value = 1;
};
inline void encode(Writer &w, const String &v) {
    w.leb(static_cast<uint32_t>(v.len()));
    w.bytes(v.view().ptr, v.len());
}
inline bool decode(Reader &r, String &out) {
    size_t len;
    if (!r.len(len, 1)) {
        return false;
    }
    const uint8_t *at = r.span(len);
    if (!at || !valid_utf8(at, len)) {
        return false;
    }
    out = String::copy(Str{const_cast<uint8_t *>(at), len});
    return true;
}

template <class T> struct MinSize<List<T>> {
    static constexpr size_t value = 1;
};
template <class T> struct MinSize<Option<T>> {
    static constexpr size_t value = 1;
};

template <class T> void encode(Writer &w, const List<T> &v);
template <class T> bool decode(Reader &r, List<T> &out);
template <class T> void encode(Writer &w, const Option<T> &v);
template <class T> bool decode(Reader &r, Option<T> &out);
template <class T, class E> void encode(Writer &w, const WireResult<T, E> &v);
template <class T, class E> bool decode(Reader &r, WireResult<T, E> &out);
template <class A> void encode(Writer &w, const Tuple<A> &v);
template <class A> bool decode(Reader &r, Tuple<A> &out);
template <class A, class B> void encode(Writer &w, const Tuple<A, B> &v);
template <class A, class B> bool decode(Reader &r, Tuple<A, B> &out);
template <class A, class B, class C> void encode(Writer &w, const Tuple<A, B, C> &v);
template <class A, class B, class C> bool decode(Reader &r, Tuple<A, B, C> &out);
template <class A, class B, class C, class D> void encode(Writer &w, const Tuple<A, B, C, D> &v);
template <class A, class B, class C, class D> bool decode(Reader &r, Tuple<A, B, C, D> &out);

template <class T> void encode(Writer &w, const List<T> &v) {
    w.leb(static_cast<uint32_t>(v.size()));
    for (const T &item : v) {
        encode(w, item);
    }
}
template <class T> bool decode(Reader &r, List<T> &out) {
    size_t len;
    if (!r.len(len, MinSize<T>::value)) {
        return false;
    }
    List<T> items;
    for (size_t i = 0; i < len; i++) {
        T item{};
        if (!decode(r, item) || !items.push(move(item))) {
            return false;
        }
    }
    out = move(items);
    return true;
}

template <class T> void encode(Writer &w, const Option<T> &v) {
    if (v) {
        w.leb(1);
        encode(w, *v);
    } else {
        w.leb(0);
    }
}
template <class T> bool decode(Reader &r, Option<T> &out) {
    uint32_t tag;
    if (!r.case_of(tag, 2)) {
        return false;
    }
    if (tag == 0) {
        out = Option<T>();
        return true;
    }
    T value{};
    if (!decode(r, value)) {
        return false;
    }
    out = Option<T>(move(value));
    return true;
}


template <class T, class E> struct MinSize<WireResult<T, E>> {
    static constexpr size_t value = 1;
};
template <class A> struct MinSize<Tuple<A>> {
    static constexpr size_t value = MinSize<A>::value;
};
template <class A, class B> struct MinSize<Tuple<A, B>> {
    static constexpr size_t value = MinSize<A>::value + MinSize<B>::value;
};
template <class A, class B, class C> struct MinSize<Tuple<A, B, C>> {
    static constexpr size_t value = MinSize<A>::value + MinSize<B>::value + MinSize<C>::value;
};
template <class A, class B, class C, class D> struct MinSize<Tuple<A, B, C, D>> {
    static constexpr size_t value =
        MinSize<A>::value + MinSize<B>::value + MinSize<C>::value + MinSize<D>::value;
};

template <class T, class E> void encode(Writer &w, const WireResult<T, E> &v) {
    if (v.is_err) {
        w.leb(1);
        if (v.err) {
            encode(w, *v.err);
        }
    } else {
        w.leb(0);
        if (v.ok) {
            encode(w, *v.ok);
        }
    }
}
template <class T, class E> bool decode(Reader &r, WireResult<T, E> &out) {
    uint32_t tag;
    if (!r.case_of(tag, 2)) {
        return false;
    }
    if (tag == 0) {
        T value{};
        if (!decode(r, value)) {
            return false;
        }
        out = WireResult<T, E>::make_ok(move(value));
    } else {
        E value{};
        if (!decode(r, value)) {
            return false;
        }
        out = WireResult<T, E>::make_err(move(value));
    }
    return true;
}

template <class A> void encode(Writer &w, const Tuple<A> &v) { encode(w, v.f0); }
template <class A> bool decode(Reader &r, Tuple<A> &out) { return decode(r, out.f0); }
template <class A, class B> void encode(Writer &w, const Tuple<A, B> &v) {
    encode(w, v.f0);
    encode(w, v.f1);
}
template <class A, class B> bool decode(Reader &r, Tuple<A, B> &out) {
    return decode(r, out.f0) && decode(r, out.f1);
}
template <class A, class B, class C> void encode(Writer &w, const Tuple<A, B, C> &v) {
    encode(w, v.f0);
    encode(w, v.f1);
    encode(w, v.f2);
}
template <class A, class B, class C> bool decode(Reader &r, Tuple<A, B, C> &out) {
    return decode(r, out.f0) && decode(r, out.f1) && decode(r, out.f2);
}
template <class A, class B, class C, class D> void encode(Writer &w, const Tuple<A, B, C, D> &v) {
    encode(w, v.f0);
    encode(w, v.f1);
    encode(w, v.f2);
    encode(w, v.f3);
}
template <class A, class B, class C, class D> bool decode(Reader &r, Tuple<A, B, C, D> &out) {
    return decode(r, out.f0) && decode(r, out.f1) && decode(r, out.f2) && decode(r, out.f3);
}

// Encodes one value; check `w.ok()` before using the bytes.
template <class T> Bytes encoded(const T &value) {
    Writer w;
    encode(w, value);
    return w.finish();
}

// Decodes one value and rejects trailing bytes.
template <class T> bool decode_all(const uint8_t *data, size_t len, T &out) {
    Reader r(data, len);
    return decode(r, out) && r.finished();
}

} // namespace wire
} // namespace tuff

#endif
