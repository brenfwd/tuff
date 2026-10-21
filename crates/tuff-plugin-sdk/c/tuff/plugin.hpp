// SPDX-License-Identifier: MIT
// TUFF_PLUGIN(Type, "id", "version", "description") writes the manifest
// and defines every guest export for plugin type Type, whose static hooks
// are all optional:
//
//     struct Hello {
//         static tuff::Status on_register();   // commands, events, settings
//         static void on_enable();
//         static void on_disable();
//         static void on_event(const auto *ev);   // after on<>/listen<>; read with
//         ev::Join::get(ev) static void on_timer(uint32_t token);          // tokens below
//         tuff::timer_base static void on_message(const tuff::raw::exports_tuff_plugin_guest_incoming_t &msg); static void
//         on_io(const tuff::IoEvent *ev);    // sockets and routes; http and files are awaited
//         static void declare_dimensions(tuff::Dimensions &out);   // tuff/worldgen.hpp
//         static tuff::Status generate_chunk(const tuff::ChunkRequest &, tuff::ChunkBuilder &);
//         static void declare_content(tuff::Content &out);   // tuff/content.hpp
//         static tuff::Status background(tuff::Str task, tuff::Str input, tuff::Bytes &out);  // co_await tuff::background(...)
//     };
//     TUFF_PLUGIN(Hello, "hello", "0.1.0", "Says hello");
//
// More manifest lines go in the typed macros below, before TUFF_PLUGIN:
//
//     TUFF_DEPENDS("economy >= 2.0.0, chat-format");   // also TUFF_SOFT_DEPENDS,
//     TUFF_CONFLICTS("other");  TUFF_LOAD_BEFORE("x");  // TUFF_CONFLICTS, TUFF_LOAD_BEFORE
//     TUFF_CAP_HTTP("discord.com/api/webhooks/", "Relays chat");
//     TUFF_CAP_FILES("1 MiB", "Keeps notes");
//     // also TUFF_CAP_CONTENT/TCP/UDP/WEBSOCKET/ROUTE/LISTEN_TCP/LISTEN_UDP/CROSS_PLUGIN(entry, reason)
// Use it in one source file of the plugin.
#ifndef TUFF_PLUGIN_HPP
#define TUFF_PLUGIN_HPP

namespace tuff::detail {

template <class P> bool on_register(Str *err) {
    raw::__wasm_call_ctors();
    raw::tuff_enter();
    if constexpr (requires { P::on_register(); }) {
        Status status = P::on_register();
        if (!status) {
            *err = status.take_error().release();
            return false;
        }
    }
    return true;
}

template <class P> void on_enable() {
    raw::tuff_enter();
    enabled();
    if constexpr (requires { P::on_enable(); }) {
        P::on_enable();
    }
}

template <class P> void on_disable() {
    raw::tuff_enter();
    disabled();
    if constexpr (requires { P::on_disable(); }) {
        P::on_disable();
    }
}

template <class P> void on_event(raw::tuff_event_t *ev) {
    raw::tuff_enter();
    dispatch_event(ev);
    if constexpr (requires { P::on_event(ev); }) {
        P::on_event(ev);
    }
    raw::exports_tuff_plugin_guest_event_free(ev);
}

template <class P> void on_timer(uint32_t token) {
    raw::tuff_enter();
    if (!timer(token)) {
        if constexpr (requires { P::on_timer(token); }) {
            P::on_timer(token);
        }
    }
}

template <class P> void on_message(raw::exports_tuff_plugin_guest_incoming_t *msg) {
    raw::tuff_enter();
    if (!messaging::detail::dispatch(msg)) {
        if constexpr (requires { P::on_message(*msg); }) {
            P::on_message(*msg);
        }
    }
    raw::exports_tuff_plugin_guest_incoming_free(msg);
}

template <class P> void on_io(IoEvent *ev) {
    raw::tuff_enter();
    if (ev->tag == TUFF_PLUGIN_CAPABILITY_TYPES_IO_EVENT_PLUGIN_CHANGE) {
        if constexpr (requires(const decltype(ev->val.plugin_change) &c) {
                          P::plugin_change(c.id, c.version, c.loaded);
                      }) {
            const auto &change = ev->val.plugin_change;
            P::plugin_change(change.id, change.version, change.loaded);
        }
    } else if (!io(ev) && !net::detail::dispatch(ev)) {
        if constexpr (requires { P::on_io(ev); }) {
            P::on_io(ev);
        }
    }
    raw::exports_tuff_plugin_capability_events_io_event_free(ev);
}

template <class P>
bool run_task(raw::plugin_string_t *task, raw::plugin_list_u8_t *input,
              raw::plugin_list_u8_t *ret, raw::plugin_string_t *err) {
    bool ok = false;
    if constexpr (requires(Bytes &out) { P::background(Str{}, Str{}, out); }) {
        init_once();
        Bytes out;
        Status status = P::background(*task, Str{input->ptr, input->len}, out);
        if (status) {
            *ret = *out.raw();
            *out.raw() = {nullptr, 0};
            ok = true;
        } else {
            *err = status.take_error().release();
        }
    } else {
        *err = String::copy("the plugin has no background tasks").release();
    }
    raw::plugin_string_free(task);
    raw::plugin_list_u8_free(input);
    return ok;
}

} // namespace tuff::detail

#define TUFF_DEPENDS(list) TUFF_DETAIL_MANIFEST("depends = " list)
#define TUFF_SOFT_DEPENDS(list) TUFF_DETAIL_MANIFEST("soft-depends = " list)
#define TUFF_CONFLICTS(list) TUFF_DETAIL_MANIFEST("conflicts = " list)
#define TUFF_LOAD_BEFORE(list) TUFF_DETAIL_MANIFEST("load-before = " list)
#define TUFF_CAP_HTTP(entry, reason) TUFF_DETAIL_MANIFEST("cap.http = " entry " -- " reason)
#define TUFF_CAP_WEBSOCKET(entry, reason) TUFF_DETAIL_MANIFEST("cap.websocket = " entry " -- " reason)
#define TUFF_CAP_TCP(entry, reason) TUFF_DETAIL_MANIFEST("cap.tcp = " entry " -- " reason)
#define TUFF_CAP_UDP(entry, reason) TUFF_DETAIL_MANIFEST("cap.udp = " entry " -- " reason)
#define TUFF_CAP_FILES(entry, reason) TUFF_DETAIL_MANIFEST("cap.files = " entry " -- " reason)
#define TUFF_CAP_ROUTE(entry, reason) TUFF_DETAIL_MANIFEST("cap.route = " entry " -- " reason)
#define TUFF_CAP_LISTEN_TCP(entry, reason) TUFF_DETAIL_MANIFEST("cap.listen-tcp = " entry " -- " reason)
#define TUFF_CAP_CROSS_PLUGIN(entry, reason) TUFF_DETAIL_MANIFEST("cap.cross-plugin = " entry " -- " reason)
#define TUFF_CAP_LISTEN_UDP(entry, reason) TUFF_DETAIL_MANIFEST("cap.listen-udp = " entry " -- " reason)
#define TUFF_CAP_CONTENT(entry, reason) TUFF_DETAIL_MANIFEST("cap.content = " entry " -- " reason)

#define TUFF_PLUGIN(Type, id, version, description)                                                \
    TUFF_DETAIL_MANIFEST("id = " id);                                                                     \
    TUFF_DETAIL_MANIFEST("version = " version);                                                           \
    TUFF_DETAIL_MANIFEST("description = " description);                                                   \
    TUFF_DETAIL_MANIFEST("api = 0");                                                                      \
    bool ::tuff::raw::exports_tuff_plugin_guest_register(::tuff::raw::plugin_string_t *err) {      \
        return ::tuff::detail::on_register<Type>(err);                                             \
    }                                                                                              \
    void ::tuff::raw::exports_tuff_plugin_guest_enable(void) {                                     \
        ::tuff::detail::on_enable<Type>();                                                         \
    }                                                                                              \
    void ::tuff::raw::exports_tuff_plugin_guest_disable(void) {                                    \
        ::tuff::detail::on_disable<Type>();                                                        \
    }                                                                                              \
    bool ::tuff::raw::exports_tuff_plugin_guest_run_command(                                       \
        ::tuff::raw::exports_tuff_plugin_guest_invocation_t *call, int32_t *ret,                   \
        ::tuff::raw::plugin_string_t *err) {                                                       \
        ::tuff::raw::tuff_enter();                                                                 \
        return ::tuff::raw::tuff_run_command(call, ret, err);                                      \
    }                                                                                              \
    void ::tuff::raw::exports_tuff_plugin_guest_suggest(                                           \
        ::tuff::raw::exports_tuff_plugin_guest_suggest_request_t *req,                             \
        ::tuff::raw::plugin_list_string_t *ret) {                                                  \
        ::tuff::raw::tuff_enter();                                                                 \
        ::tuff::raw::tuff_suggest(req, ret);                                                       \
    }                                                                                              \
    void ::tuff::raw::exports_tuff_plugin_guest_on_event(                                          \
        ::tuff::raw::exports_tuff_plugin_guest_event_t *ev) {                                      \
        ::tuff::detail::on_event<Type>(ev);                                                        \
    }                                                                                              \
    void ::tuff::raw::exports_tuff_plugin_guest_on_timer(uint32_t token) {                         \
        ::tuff::detail::on_timer<Type>(token);                                                     \
    }                                                                                              \
    void ::tuff::raw::exports_tuff_plugin_guest_on_message(                                        \
        ::tuff::raw::exports_tuff_plugin_guest_incoming_t *msg) {                                  \
        ::tuff::detail::on_message<Type>(msg);                                                     \
    }                                                                                              \
    void ::tuff::raw::exports_tuff_plugin_capability_events_on_io(::tuff::IoEvent *ev) {           \
        ::tuff::detail::on_io<Type>(ev);                                                           \
    }                                                                                              \
    void ::tuff::raw::exports_tuff_plugin_worldgen_declare_dimensions(                             \
        ::tuff::raw::exports_tuff_plugin_worldgen_list_dimension_decl_t *ret) {                    \
        ::tuff::detail::declare_dimensions<Type>(ret);                                             \
    }                                                                                              \
    bool ::tuff::raw::exports_tuff_plugin_worldgen_generate_chunk(                                 \
        ::tuff::raw::plugin_string_t *dimension, int64_t seed, int32_t chunk_x, int32_t chunk_z,   \
        int32_t min_y, int32_t height,                                                             \
        ::tuff::raw::exports_tuff_plugin_worldgen_generated_chunk_t *ret,                          \
        ::tuff::raw::plugin_string_t *err) {                                                       \
        return ::tuff::detail::generate_chunk<Type>(dimension, seed, chunk_x, chunk_z, min_y,      \
                                                    height, ret, err);                             \
    }                                                                                              \
    void ::tuff::raw::exports_tuff_plugin_content_declare_content(                                 \
        ::tuff::raw::exports_tuff_plugin_content_content_decl_t *ret) {                            \
        ::tuff::detail::declare_content<Type>(ret);                                                \
    }                                                                                              \
    bool ::tuff::raw::exports_tuff_plugin_background_tasks_run_task(                               \
        ::tuff::raw::plugin_string_t *task, ::tuff::raw::plugin_list_u8_t *input,                  \
        ::tuff::raw::plugin_list_u8_t *ret, ::tuff::raw::plugin_string_t *err) {                   \
        return ::tuff::detail::run_task<Type>(task, input, ret, err);                              \
    }                                                                                              \
    static_assert(true, "")

#endif
