// SPDX-License-Identifier: MIT
// WebSocket, TCP and UDP sockets and listeners. Results arrive as io events,
// which the plugin runtime routes to the callbacks given at connect time:
//
//     tuff::net::Handlers h;
//     h.connected = [](const tuff::Denied *refused) { ... };
//     h.text = [](tuff::Str message) { ... };
//     h.closed = [](const tuff::net::Closed &c) { ... };
//     auto ws = tuff::net::ws_connect("wss://example.org/feed", move(h));
//     if (ws) ws->send_text("hello");
#ifndef TUFF_SOCKETS_HPP
#define TUFF_SOCKETS_HPP

namespace tuff::net {

// How a socket ended; `error` is null for an orderly close.
struct Closed {
    const Denied *error = nullptr;
    bool has_code = false;
    uint16_t code = 0;
    Str reason{nullptr, 0};
};

// Callbacks of one socket or listener; set the ones that matter.
struct Handlers {
    // A connect finished: null on success, the refusal otherwise.
    Function<void(const Denied *)> connected;
    // A WebSocket text frame.
    Function<void(Str)> text;
    // A WebSocket binary frame or TCP bytes.
    Function<void(Str)> data;
    // A UDP packet: peer as `ip:port`, then the bytes.
    Function<void(Str, Str)> datagram;
    Function<void(const Closed &)> closed;
    // A listener is bound: the port.
    Function<void(uint16_t)> listening;
    // A listener took a connection (peer, socket); give the socket its own
    // handlers with watch().
    Function<void(Str, uint32_t)> accepted;
};

namespace detail {

struct Entry {
    uint32_t handle;
    Handlers handlers;
};

inline tuff::detail::Registry<Entry> entries;

inline Entry *find(uint32_t handle) {
    for (Entry &e : entries) {
        if (e.handle == handle) {
            return &e;
        }
    }
    return nullptr;
}

inline void forget(uint32_t handle) {
    for (size_t i = 0; i < entries.size(); i++) {
        if (entries[i].handle == handle) {
            entries.swap_remove(i);
            return;
        }
    }
}

// Routes a socket event to its callbacks; false for other events.
inline bool dispatch(IoEvent *ev) {
    using namespace raw;
    switch (ev->tag) {
    case TUFF_PLUGIN_CAPABILITY_TYPES_IO_EVENT_CONNECTED: {
        auto &c = ev->val.connected;
        Entry *e = find(c.socket);
        if (!e) {
            return true;
        }
        if (c.outcome.is_err) {
            Denied d = Denied::from(c.outcome.val.err);
            if (e->handlers.connected) {
                e->handlers.connected(&d);
            }
            forget(c.socket);
        } else if (e->handlers.connected) {
            e->handlers.connected(nullptr);
        }
        return true;
    }
    case TUFF_PLUGIN_CAPABILITY_TYPES_IO_EVENT_WS_MESSAGE: {
        auto &m = ev->val.ws_message;
        Entry *e = find(m.socket);
        if (!e) {
            return true;
        }
        if (m.message.tag == TUFF_PLUGIN_CAPABILITY_TYPES_WS_MESSAGE_TEXT) {
            if (e->handlers.text) {
                e->handlers.text(m.message.val.text);
            }
        } else if (e->handlers.data) {
            e->handlers.data(Str{m.message.val.binary.ptr, m.message.val.binary.len});
        }
        return true;
    }
    case TUFF_PLUGIN_CAPABILITY_TYPES_IO_EVENT_DATA: {
        auto &d = ev->val.data;
        Entry *e = find(d.socket);
        if (e && e->handlers.data) {
            e->handlers.data(Str{d.data.ptr, d.data.len});
        }
        return true;
    }
    case TUFF_PLUGIN_CAPABILITY_TYPES_IO_EVENT_DATAGRAM: {
        auto &d = ev->val.datagram;
        Entry *e = find(d.socket);
        if (e && e->handlers.datagram) {
            e->handlers.datagram(d.peer, Str{d.data.ptr, d.data.len});
        }
        return true;
    }
    case TUFF_PLUGIN_CAPABILITY_TYPES_IO_EVENT_CLOSED: {
        auto &c = ev->val.closed;
        Entry *e = find(c.socket);
        if (!e) {
            return true;
        }
        Closed closed;
        Denied error;
        if (c.error.is_some) {
            error = Denied::from(c.error.val);
            closed.error = &error;
        }
        closed.has_code = c.code.is_some;
        closed.code = c.code.val;
        closed.reason = c.reason;
        if (e->handlers.closed) {
            e->handlers.closed(closed);
        }
        forget(c.socket);
        return true;
    }
    case TUFF_PLUGIN_CAPABILITY_TYPES_IO_EVENT_LISTENING: {
        auto &l = ev->val.listening;
        Entry *e = find(l.listener);
        if (e && e->handlers.listening) {
            e->handlers.listening(l.port);
        }
        return true;
    }
    case TUFF_PLUGIN_CAPABILITY_TYPES_IO_EVENT_ACCEPTED: {
        auto &a = ev->val.accepted;
        Entry *e = find(a.listener);
        if (e && e->handlers.accepted) {
            e->handlers.accepted(a.peer, a.socket);
        } else {
            raw::tuff_plugin_net_close(a.socket);
        }
        return true;
    }
    default:
        return false;
    }
}

} // namespace detail

using Result = Outcome<Unit, Denied>;

// A socket or listener the host holds for the plugin.
class Socket {
  public:
    Socket() = default;
    explicit Socket(uint32_t handle) : handle_(handle) {}
    uint32_t handle() const { return handle_; }

    // Sends on a TCP or connected UDP socket; refused while the send queue
    // is full.
    Result send(const void *data, size_t len) const {
        raw::plugin_list_u8_t list = {static_cast<uint8_t *>(const_cast<void *>(data)), len};
        raw::tuff_plugin_net_denied_t err;
        if (!raw::tuff_plugin_net_send(handle_, &list, &err)) {
            return fail(err);
        }
        return Result(Unit{});
    }
    Result send(Str bytes) const { return send(bytes.ptr, bytes.len); }
    Result send(const char *text) const { return send(raw::tuff_str(text)); }

    Result send_text(Str text) const {
        raw::tuff_plugin_net_ws_message_t m = {};
        m.tag = TUFF_PLUGIN_CAPABILITY_TYPES_WS_MESSAGE_TEXT;
        m.val.text = text;
        return ws(m);
    }
    Result send_text(const char *text) const { return send_text(raw::tuff_str(text)); }
    Result send_binary(const void *data, size_t len) const {
        raw::tuff_plugin_net_ws_message_t m = {};
        m.tag = TUFF_PLUGIN_CAPABILITY_TYPES_WS_MESSAGE_BINARY;
        m.val.binary = {static_cast<uint8_t *>(const_cast<void *>(data)), len};
        return ws(m);
    }

    // Replies from a UDP listener to a peer it heard from (`ip:port`).
    Result reply_to(Str peer, const void *data, size_t len) const {
        raw::plugin_list_u8_t list = {static_cast<uint8_t *>(const_cast<void *>(data)), len};
        raw::tuff_plugin_net_denied_t err;
        if (!raw::tuff_plugin_net_reply_to(handle_, &peer, &list, &err)) {
            return fail(err);
        }
        return Result(Unit{});
    }

    // Closes it and drops its callbacks.
    void close() const {
        raw::tuff_plugin_net_close(handle_);
        detail::forget(handle_);
    }

  private:
    Result ws(raw::tuff_plugin_net_ws_message_t &m) const {
        raw::tuff_plugin_net_denied_t err;
        if (!raw::tuff_plugin_net_ws_send(handle_, &m, &err)) {
            return fail(err);
        }
        return Result(Unit{});
    }
    static Result fail(raw::tuff_plugin_net_denied_t &err) {
        Result r(Denied::from(err));
        raw::tuff_plugin_capability_types_denied_free(&err);
        return r;
    }

    uint32_t handle_ = 0;
};

namespace detail {

inline Outcome<Socket, Denied> started(bool ok, uint32_t handle,
                                       raw::tuff_plugin_net_denied_t &err, Handlers &&h) {
    if (!ok) {
        Outcome<Socket, Denied> r(Denied::from(err));
        raw::tuff_plugin_capability_types_denied_free(&err);
        return r;
    }
    if (!entries.push(Entry{handle, move(h)})) {
        raw::tuff_plugin_net_close(handle);
        return Outcome<Socket, Denied>(Denied{});
    }
    return Outcome<Socket, Denied>(Socket(handle));
}

} // namespace detail

// Routes events of an accepted (or otherwise known) socket to `h`.
inline bool watch(Socket socket, Handlers &&h) {
    return detail::entries.push(detail::Entry{socket.handle(), move(h)});
}

inline Outcome<Socket, Denied> ws_connect(Str url, Handlers &&h) {
    raw::tuff_plugin_net_list_header_t headers = {nullptr, 0};
    raw::plugin_list_string_t protocols = {nullptr, 0};
    uint32_t handle = 0;
    raw::tuff_plugin_net_denied_t err;
    bool ok = raw::tuff_plugin_net_ws_connect(&url, &headers, &protocols, &handle, &err);
    return detail::started(ok, handle, err, move(h));
}
inline Outcome<Socket, Denied> ws_connect(const char *url, Handlers &&h) {
    return ws_connect(raw::tuff_str(url), move(h));
}

inline Outcome<Socket, Denied> tcp_connect(Str host, uint16_t port, bool tls, Handlers &&h) {
    uint32_t handle = 0;
    raw::tuff_plugin_net_denied_t err;
    bool ok = raw::tuff_plugin_net_tcp_connect(&host, port, tls, &handle, &err);
    return detail::started(ok, handle, err, move(h));
}

inline Outcome<Socket, Denied> udp_connect(Str host, uint16_t port, Handlers &&h) {
    uint32_t handle = 0;
    raw::tuff_plugin_net_denied_t err;
    bool ok = raw::tuff_plugin_net_udp_connect(&host, port, &handle, &err);
    return detail::started(ok, handle, err, move(h));
}

namespace detail {
inline Outcome<Socket, Denied> listen(uint8_t kind, uint16_t port, Handlers &&h) {
    uint32_t handle = 0;
    raw::tuff_plugin_net_denied_t err;
    bool ok = raw::tuff_plugin_net_listen(kind, nullptr, port, &handle, &err);
    return started(ok, handle, err, move(h));
}
} // namespace detail

// Listens on every local address; port 0 picks one (see Handlers::listening).
inline Outcome<Socket, Denied> listen_tcp(uint16_t port, Handlers &&h) {
    return detail::listen(TUFF_PLUGIN_CAPABILITY_TYPES_CAPABILITY_KIND_LISTEN_TCP, port, move(h));
}
inline Outcome<Socket, Denied> listen_udp(uint16_t port, Handlers &&h) {
    return detail::listen(TUFF_PLUGIN_CAPABILITY_TYPES_CAPABILITY_KIND_LISTEN_UDP, port, move(h));
}

} // namespace tuff::net

#endif
