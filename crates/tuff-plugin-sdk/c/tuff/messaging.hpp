// SPDX-License-Identifier: MIT
// Messages between loaded plugins: a provider declares an interface and
// routes its methods, a caller requests (co_await) or notifies. Typed
// clients and serve helpers come from `tuff-plugin-sdk-gen interface <wit>
// --cpp <out.hpp>`; this header is the raw-bytes layer under them.
//
//     auto reply = co_await tuff::request(tuff::Target::any(), "acme:api/greet", key, payload);
//     if (!reply) { /* reply.error().tag, e.g. CallError::NO_PROVIDER */ }
//
// A missing provider is a cheap error, never a trap. A request is sent when
// it is awaited. Providers register in on_register (`messaging::provide`
// and `messaging::route`); each delivery is a fresh call, so a handler that
// answers later moves its Reply into a coroutine. Dropping a Reply refuses
// the request at once. Messages nobody routed (notifies, mostly) reach the
// plugin's optional `static void on_message(const tuff::messaging::Incoming &)`,
// and dependency changes reach `static void plugin_change(tuff::Str id,
// tuff::Str version, bool loaded)`.
#ifndef TUFF_MESSAGING_HPP
#define TUFF_MESSAGING_HPP

namespace tuff {

// Why a request or notify failed or was refused; `text` is the provider's
// own message for REFUSED.
struct CallError {
    static constexpr uint8_t NO_PROVIDER = TUFF_PLUGIN_CAPABILITY_TYPES_CALL_ERROR_NO_PROVIDER;
    static constexpr uint8_t AMBIGUOUS = TUFF_PLUGIN_CAPABILITY_TYPES_CALL_ERROR_AMBIGUOUS;
    static constexpr uint8_t NO_METHOD = TUFF_PLUGIN_CAPABILITY_TYPES_CALL_ERROR_NO_METHOD;
    static constexpr uint8_t TARGET_GONE = TUFF_PLUGIN_CAPABILITY_TYPES_CALL_ERROR_TARGET_GONE;
    static constexpr uint8_t FAULTED = TUFF_PLUGIN_CAPABILITY_TYPES_CALL_ERROR_FAULTED;
    static constexpr uint8_t TIMED_OUT = TUFF_PLUGIN_CAPABILITY_TYPES_CALL_ERROR_TIMED_OUT;
    static constexpr uint8_t BUSY = TUFF_PLUGIN_CAPABILITY_TYPES_CALL_ERROR_BUSY;
    static constexpr uint8_t TOO_LARGE = TUFF_PLUGIN_CAPABILITY_TYPES_CALL_ERROR_TOO_LARGE;
    static constexpr uint8_t REFUSED = TUFF_PLUGIN_CAPABILITY_TYPES_CALL_ERROR_REFUSED;

    uint8_t tag = REFUSED;
    String text;

    bool no_provider() const { return tag == NO_PROVIDER; }
    bool no_method() const { return tag == NO_METHOD; }
    bool timed_out() const { return tag == TIMED_OUT; }

    static CallError refused(const char *why) {
        CallError out;
        out.text = String::copy(why);
        return out;
    }

    // Copies `err` and frees what it owns.
    static CallError take(raw::tuff_plugin_capability_types_call_error_t &err) {
        CallError out;
        out.tag = err.tag;
        if (err.tag == REFUSED) {
            out.text = String::copy(err.val.refused);
        }
        raw::tuff_plugin_capability_types_call_error_free(&err);
        return out;
    }
};

// A failed call to a method whose WIT result has an error type: either the
// call failed (`call`) or the provider answered with its own error (`app`).
template <class E> struct Failure {
    bool is_app = false;
    CallError call;
    Option<E> app;

    Failure() = default;
    Failure(CallError error) : call(move(error)) {}
    static Failure from_app(E error) {
        Failure out;
        out.is_app = true;
        out.app = Option<E>(move(error));
        return out;
    }
};

// Which provider a message goes to.
class Target {
  public:
    // The sole provider of the interface (every provider, for a notify).
    static Target any() { return Target(); }
    // The provider with this plugin id.
    static Target plugin(Str id) {
        Target out;
        out.id_ = Option<String>(String::copy(id));
        return out;
    }
    static Target plugin(const char *id) { return plugin(raw::tuff_str(id)); }

    bool is_any() const { return !id_; }
    // Null for any().
    Str *raw_id(Str &storage) const {
        if (!id_) {
            return nullptr;
        }
        storage = id_->view();
        return &storage;
    }

  private:
    Option<String> id_;
};

using BytesOutcome = Outcome<Bytes, CallError>;

namespace detail {

// An awaitable request; the reply resumes the coroutine from on-io.
class RequestCall {
  public:
    RequestCall(Target target, Str interface, uint64_t method, Bytes payload, uint32_t ticks)
        : target_(move(target)), interface_(interface), method_(method),
          payload_(move(payload)), ticks_(ticks) {}

    bool await_ready() const noexcept { return false; }
    bool await_suspend(std::coroutine_handle<> task) {
        Str id;
        Str *maybe = target_.raw_id(id);
        raw::tuff_plugin_capability_types_handle_t handle = 0;
        raw::tuff_plugin_capability_types_call_error_t err;
        if (!raw::tuff_plugin_host_request(maybe, &interface_, method_, payload_.raw(), ticks_,
                                           &handle, &err)) {
            outcome_ = BytesOutcome(CallError::take(err));
            return false;
        }
        if (!pending_io.push({handle, TUFF_PLUGIN_CAPABILITY_TYPES_IO_EVENT_REPLY, task, &outcome_,
                              &fill})) {
            outcome_ = BytesOutcome(CallError::refused("out of memory"));
            return false;
        }
        return true;
    }
    BytesOutcome await_resume() { return move(outcome_); }

  private:
    static void fill(void *slot, IoEvent *event) {
        auto &done = event->val.reply.outcome;
        BytesOutcome &out = *static_cast<BytesOutcome *>(slot);
        if (done.is_err) {
            out = BytesOutcome(CallError::take(done.val.err));
            return;
        }
        out = BytesOutcome(Bytes::adopt(done.val.ok));
        done.val.ok = {nullptr, 0};
    }

    Target target_;
    Str interface_;
    uint64_t method_;
    Bytes payload_;
    uint32_t ticks_;
    BytesOutcome outcome_{CallError{}};
};

} // namespace detail

// Sends a request to the provider of `interface`; co_await gives the reply
// bytes or why there is none. `ticks` is the timeout (0 = the host default).
inline detail::RequestCall request(Target target, Str interface, uint64_t method, Bytes payload,
                                   uint32_t ticks = 0) {
    return detail::RequestCall(move(target), interface, method, move(payload), ticks);
}
inline detail::RequestCall request(Target target, const char *interface, uint64_t method,
                                   Bytes payload, uint32_t ticks = 0) {
    return detail::RequestCall(move(target), raw::tuff_str(interface), method, move(payload),
                               ticks);
}

// One-way send; gives how many deliveries were queued.
inline Outcome<uint32_t, CallError> notify(const Target &target, Str interface, uint64_t method,
                                           Bytes payload) {
    Str id;
    Str *maybe = target.raw_id(id);
    uint32_t queued = 0;
    raw::tuff_plugin_capability_types_call_error_t err;
    if (!raw::tuff_plugin_host_notify(maybe, &interface, method, payload.raw(), &queued, &err)) {
        return Outcome<uint32_t, CallError>(CallError::take(err));
    }
    return Outcome<uint32_t, CallError>(queued);
}

namespace messaging {

using Incoming = raw::exports_tuff_plugin_guest_incoming_t;

// The right to answer one request, once. Move it into a coroutine to answer
// after awaiting something. Dropping it unanswered refuses the request.
// For a notify (no request) the answers do nothing.
class Reply {
  public:
    Reply() = default;
    explicit Reply(uint64_t request) : request_(request), open_(true) {}
    Reply(Reply &&other) : request_(other.request_), open_(other.open_) { other.open_ = false; }
    Reply &operator=(Reply &&other) {
        if (this != &other) {
            finish_err("dropped");
            request_ = other.request_;
            open_ = other.open_;
            other.open_ = false;
        }
        return *this;
    }
    Reply(const Reply &) = delete;
    Reply &operator=(const Reply &) = delete;
    ~Reply() { finish_err("dropped"); }

    bool open() const { return open_; }

    void ok(const uint8_t *data, size_t len) {
        if (!open_) {
            return;
        }
        open_ = false;
        raw::tuff_plugin_host_result_list_u8_string_t out = {};
        out.is_err = false;
        out.val.ok = {const_cast<uint8_t *>(data), len};
        send(out);
    }
    void ok(const Bytes &payload) { ok(payload.data(), payload.len()); }
    void err(Str text) {
        if (!open_) {
            return;
        }
        open_ = false;
        raw::tuff_plugin_host_result_list_u8_string_t out = {};
        out.is_err = true;
        out.val.err = text;
        send(out);
    }
    void err(const char *text) { err(raw::tuff_str(text)); }

  private:
    void finish_err(const char *text) {
        if (open_) {
            err(text);
        }
    }
    void send(raw::tuff_plugin_host_result_list_u8_string_t &out) {
        HostError e;
        if (!raw::tuff_plugin_host_respond(request_, &out, &e)) {
            raw::tuff_plugin_types_host_error_free(&e);
        }
    }

    uint64_t request_ = 0;
    bool open_ = false;
};

// A Reply for a method returning R: encodes the value for you.
template <class R> class TypedReply {
  public:
    explicit TypedReply(Reply reply) : reply_(move(reply)) {}
    void ok(const R &value) {
        wire::Writer w;
        wire::encode(w, value);
        if (!w.ok()) {
            reply_.err("out of memory");
            return;
        }
        Bytes bytes = w.finish();
        reply_.ok(bytes);
    }
    void err(Str text) { reply_.err(text); }
    void err(const char *text) { reply_.err(text); }

  private:
    Reply reply_;
};

namespace detail {

struct Route {
    String interface;
    uint64_t key;
    Function<void(const Incoming &, Reply)> handler;
};

inline ::tuff::detail::Registry<Route> routes;

// Routes a delivery to its handler. False for a notify nobody routed, for
// the plugin's on_message; an unrouted request is refused here.
inline bool dispatch(Incoming *msg) {
    for (size_t i = 0; i < routes.size(); i++) {
        Route &route = routes[i];
        if (route.key == msg->method && route.interface.view() == msg->interface) {
            Reply reply = msg->request.is_some ? Reply(msg->request.val) : Reply();
            route.handler(*msg, move(reply));
            return true;
        }
    }
    if (msg->request.is_some) {
        Reply(msg->request.val).err("no such method");
        return true;
    }
    return false;
}

} // namespace detail

// Declares the interface and its method keys (register phase only).
inline Status provide(Str interface, const uint64_t *keys, size_t count) {
    raw::plugin_list_u64_t methods = {const_cast<uint64_t *>(keys), count};
    raw::tuff_plugin_types_register_error_t err;
    if (!raw::tuff_plugin_host_provide(&interface, &methods, &err)) {
        Status failed = Status::fail("the host refused the interface");
        raw::tuff_plugin_types_register_error_free(&err);
        return failed;
    }
    return Status();
}

// Runs `handler(msg, reply)` for a delivery of `interface` and `key`.
inline bool route(Str interface, uint64_t key, Function<void(const Incoming &, Reply)> handler) {
    return detail::routes.push({String::copy(interface), key, move(handler)});
}

} // namespace messaging
} // namespace tuff

#endif
