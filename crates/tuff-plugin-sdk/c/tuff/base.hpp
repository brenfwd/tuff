// SPDX-License-Identifier: MIT
// Small freestanding building blocks for the C++ SDK (there is no standard
// library): Option, an owned String, a move-only Function, registries and
// the Status a plugin's registration returns.
#ifndef TUFF_BASE_HPP
#define TUFF_BASE_HPP

#include <new>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#ifdef TUFF_H
#error "C++ plugins include tuff.hpp (and tuff/raw.h for the raw layer), not tuff.h"
#endif

// The C binding layer lives in tuff::raw so that tuff.hpp does not expose
// it; tuff/raw.h brings it into scope for code that needs it.
namespace tuff::raw {
#include "tuff.h"
extern "C" void __wasm_call_ctors(void);
} // namespace tuff::raw

namespace tuff {

using Str = raw::plugin_string_t;
using Uuid = raw::tuff_uuid_t;

template <class A, class B> inline constexpr bool same_type = false;
template <class A> inline constexpr bool same_type<A, A> = true;

template <class T> struct plain_of {
    using type = T;
};
template <class T> struct plain_of<T &> {
    using type = T;
};
template <class T> struct plain_of<T &&> {
    using type = T;
};

template <class T> constexpr T &&move(T &value) noexcept { return static_cast<T &&>(value); }

inline Str str(const char *text) { return raw::tuff_str(text); }

inline bool operator==(Str a, Str b) {
    if (a.len != b.len) {
        return false;
    }
    for (size_t i = 0; i < a.len; i++) {
        if (a.ptr[i] != b.ptr[i]) {
            return false;
        }
    }
    return true;
}

// A value or nothing.
template <class T> class Option {
  public:
    Option() : some_(false) {}
    Option(T value) : some_(true) { new (&value_) T(move(value)); }
    Option(Option &&other) : some_(other.some_) {
        if (some_) {
            new (&value_) T(move(other.value_));
        }
    }
    Option(const Option &other) : some_(other.some_) {
        if (some_) {
            new (&value_) T(other.value_);
        }
    }
    Option &operator=(Option other) {
        clear();
        if (other.some_) {
            new (&value_) T(move(other.value_));
            some_ = true;
        }
        return *this;
    }
    ~Option() { clear(); }

    explicit operator bool() const { return some_; }
    T &operator*() { return value_; }
    const T &operator*() const { return value_; }
    T *operator->() { return &value_; }
    const T *operator->() const { return &value_; }
    T value_or(T fallback) const { return some_ ? value_ : fallback; }

  private:
    void clear() {
        if (some_) {
            value_.~T();
            some_ = false;
        }
    }

    union {
        T value_;
    };
    bool some_;
};

// Owned UTF-8 text.
class String {
  public:
    String() : s_{nullptr, 0} {}
    // Takes ownership of `owned` (freed with plugin_string_free).
    static String adopt(Str owned) {
        String out;
        out.s_ = owned;
        return out;
    }
    static String copy(Str view) {
        String out;
        if (view.len) {
            raw::plugin_string_dup_n(&out.s_, reinterpret_cast<const char *>(view.ptr), view.len);
        }
        return out;
    }
    static String copy(const char *text) { return copy(raw::tuff_str(text)); }
    String(String &&other) : s_(other.s_) { other.s_ = Str{nullptr, 0}; }
    String &operator=(String &&other) {
        if (this != &other) {
            drop();
            s_ = other.s_;
            other.s_ = Str{nullptr, 0};
        }
        return *this;
    }
    String(const String &other) : String(copy(other.s_)) {}
    ~String() { drop(); }

    Str view() const { return s_; }
    size_t len() const { return s_.len; }
    bool operator==(const char *text) const { return raw::tuff_str_eq(s_, text); }
    // Hands the string to a caller that frees it (an export's error).
    Str release() {
        Str out = s_;
        s_ = Str{nullptr, 0};
        return out;
    }

  private:
    void drop() {
        if (s_.ptr) {
            raw::plugin_string_free(&s_);
        }
    }

    Str s_;
};

// A callable of signature R(A...), owning what it captures. Move-only.
template <class Sig> class Function;

template <class R, class... A> class Function<R(A...)> {
  public:
    Function() = default;
    template <class F>
        requires(!same_type<typename plain_of<F>::type, Function>)
    Function(F f) {
        using Callable = typename plain_of<F>::type;
        void *memory = malloc(sizeof(Callable));
        if (!memory) {
            return;
        }
        obj_ = new (memory) Callable(move(f));
        call_ = [](void *obj, A... args) -> R {
            return (*static_cast<Callable *>(obj))(static_cast<A &&>(args)...);
        };
        drop_ = [](void *obj) {
            static_cast<Callable *>(obj)->~Callable();
            free(obj);
        };
    }
    Function(Function &&other) : obj_(other.obj_), call_(other.call_), drop_(other.drop_) {
        other.obj_ = nullptr;
    }
    Function &operator=(Function &&other) {
        if (this != &other) {
            reset();
            obj_ = other.obj_;
            call_ = other.call_;
            drop_ = other.drop_;
            other.obj_ = nullptr;
        }
        return *this;
    }
    Function(const Function &) = delete;
    Function &operator=(const Function &) = delete;
    ~Function() { reset(); }

    explicit operator bool() const { return obj_ != nullptr; }
    R operator()(A... args) const { return call_(obj_, static_cast<A &&>(args)...); }

  private:
    void reset() {
        if (obj_) {
            drop_(obj_);
            obj_ = nullptr;
        }
    }

    void *obj_ = nullptr;
    R (*call_)(void *, A...) = nullptr;
    void (*drop_)(void *) = nullptr;
};

namespace detail {

// A growable list for registries that live as long as the plugin: it has
// no destructor, so a static one needs no constructor or destructor run.
template <class T> class Registry {
  public:
    constexpr Registry() = default;
    bool push(T value) {
        if (len_ == cap_) {
            size_t cap = cap_ ? cap_ * 2 : 8;
            T *grown = static_cast<T *>(malloc(cap * sizeof(T)));
            if (!grown) {
                return false;
            }
            for (size_t i = 0; i < len_; i++) {
                new (&grown[i]) T(move(items_[i]));
                items_[i].~T();
            }
            free(items_);
            items_ = grown;
            cap_ = cap;
        }
        new (&items_[len_++]) T(move(value));
        return true;
    }
    // Moves the last item into slot `at`.
    void swap_remove(size_t at) {
        if (at + 1 != len_) {
            items_[at] = move(items_[len_ - 1]);
        }
        items_[--len_].~T();
    }
    void clear() {
        while (len_) {
            items_[--len_].~T();
        }
    }
    size_t size() const { return len_; }
    T &operator[](size_t at) { return items_[at]; }
    T *begin() { return items_; }
    T *end() { return items_ + len_; }

  private:
    T *items_ = nullptr;
    size_t len_ = 0;
    size_t cap_ = 0;
};

} // namespace detail

// What registration returns: success, or why it failed.
class Status {
  public:
    Status() = default;
    static Status fail(const char *why) { return fail(String::copy(why)); }
    static Status fail(String why) {
        Status s;
        s.failed_ = true;
        s.error_ = move(why);
        return s;
    }
    explicit operator bool() const { return !failed_; }
    Str error() const { return error_.view(); }
    String take_error() { return move(error_); }

  private:
    bool failed_ = false;
    String error_;
};

using HostError = raw::tuff_plugin_host_host_error_t;

// The reason text of a failed host call, freeing what the error owns.
inline String host_error_text(HostError &err) {
    constexpr uint8_t invalid = 6;
    if (err.tag == invalid) {
        return String::adopt(err.val.invalid);
    }
    return String::copy("the host refused the call");
}

inline Status check(bool ok, const char *what) { return ok ? Status() : Status::fail(what); }
inline Status check(Status status, const char *) { return status; }

} // namespace tuff

// Returns the failure from the enclosing function when `expr` (a bool or
// a Status) failed, naming the expression.
#define TUFF_CHECK(expr)                                                                           \
    do {                                                                                           \
        ::tuff::Status tuff_status_ = ::tuff::check((expr), #expr " failed");                      \
        if (!tuff_status_) {                                                                       \
            return tuff_status_;                                                                   \
        }                                                                                          \
    } while (0)

#endif
