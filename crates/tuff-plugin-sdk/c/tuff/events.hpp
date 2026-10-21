// SPDX-License-Identifier: MIT
// Typed event handlers. While registering:
//
//     tuff::on<tuff::ev::Join>([](const tuff::Player &p) { ... });
//     tuff::listen<tuff::ev::BlockBreak>()
//         .in_dimension(tuff::OVERWORLD)
//         .block("minecraft:diamond_ore")
//         .run([](const auto &c) { ... });
//
// Each subscribes to its event kind and gets the case's record. The ev
// markers and the dimension_of/state_of/player_of accessors filters use are
// generated from the WIT (tuff_gen.hpp), like the Rust SDK's.
#ifndef TUFF_EVENTS_HPP
#define TUFF_EVENTS_HPP

namespace tuff {

namespace detail {
inline Registry<Function<void(const raw::tuff_event_t *)>> listeners;

inline void dispatch_event(const raw::tuff_event_t *ev) {
    for (Function<void(const raw::tuff_event_t *)> &listener : listeners) {
        listener(ev);
    }
}

inline Str unnamespaced(Str id) {
    static const char prefix[] = "minecraft:";
    size_t n = sizeof prefix - 1;
    if (id.len >= n && Str{id.ptr, n} == raw::tuff_str(prefix)) {
        return Str{id.ptr + n, id.len - n};
    }
    return id;
}
} // namespace detail

// Whether block state `state` (with or without properties) is of block
// `id`; `stone` means `minecraft:stone`.
inline bool same_block(Str state, Str id) {
    size_t name = 0;
    while (name < state.len && state.ptr[name] != '[') {
        name++;
    }
    return detail::unnamespaced(Str{state.ptr, name}) == detail::unnamespaced(id);
}

// A handler for events of case E being set up: add filters, then run().
template <class E> class Listen {
  public:
    using Payload = typename E::payload;

    // Only events for which `filter(payload)` holds.
    template <class F> Listen &when(F filter) {
        filter_ = [prev = move(filter_), next = Function<bool(const Payload &)>(move(filter))](
                      const Payload &p) { return (!prev || prev(p)) && next(p); };
        return *this;
    }
    // Only events in dimension `key`, such as tuff::OVERWORLD.
    Listen &in_dimension(const char *key) {
        return when(
            [key = String::copy(key)](const Payload &p) { return dimension_of(p) == key.view(); });
    }
    // Only events about block `id`, any properties.
    Listen &block(const char *id) {
        return when([id = String::copy(id)](const Payload &p) {
            return same_block(state_of(p), id.view());
        });
    }
    // Only events a player caused.
    Listen &by_players() {
        return when([](const Payload &p) { return player_of(p) != nullptr; });
    }
    // Subscribes and installs `handler(const Payload &)`; false when the
    // host refused (not registering).
    template <class F> bool run(F handler) {
        if (!raw::tuff_subscribe(E::kind)) {
            return false;
        }
        return detail::listeners.push(
            [filter = move(filter_), handler = Function<void(const Payload &)>(move(handler))](
                const raw::tuff_event_t *ev) {
                const Payload *p = E::get(ev);
                if (p && (!filter || filter(*p))) {
                    handler(*p);
                }
            });
    }

  private:
    Function<bool(const Payload &)> filter_;
};

template <class E> Listen<E> listen() { return Listen<E>(); }

// Runs `handler(const Payload &)` for every event of case E.
template <class E, class F> bool on(F handler) { return listen<E>().run(move(handler)); }

} // namespace tuff

#endif
