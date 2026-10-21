// SPDX-License-Identifier: MIT
// The tuff plugin SDK for C++20: a header-only layer over tuff.h for
// freestanding builds (no exceptions, RTTI or standard library). It mirrors
// the Rust SDK's high-level surface:
//
// - TUFF_PLUGIN(Type, id, version, description) exports a plugin type whose
//   static on_register/on_enable/on_disable/on_event/on_timer/on_message
//   hooks are all optional (tuff/plugin.hpp)
// - command trees from Tree/Node with lambda handlers, about texts, an
//   automatic help subcommand and declared permissions; executes_async runs
//   a coroutine handler that answers through its Ctx after co_await
// - on<ev::Join>(...) and listen<ev::BlockBreak>().in_dimension(...)
//   event handlers (tuff/events.hpp), with markers generated from the WIT
// - Setting<T> typed settings (tuff/config.hpp), Stored/StoredMap typed
//   storage (tuff/store.hpp)
// - Task/Async coroutines, sleep, after and every on a single-threaded
//   executor driven by the host's timer callbacks (tuff/task.hpp)
// - co_await tuff::http::get(url) and tuff::files::read/write/list/stat/...
//   with the outcome of each capability call (tuff/io.hpp)
// - co_await request(...), notify, serve tables and the wire codec for typed
//   plugin-to-plugin interfaces (tuff/messaging.hpp, tuff/wire.hpp)
// - World/EntityRef handles, tuff::acting and teleport_player over the
//   host's world calls (tuff/world.hpp)
// - Menu/MenuItem, BlockRule, ChangeRule, rules::set_team, online:: player state and
//   effects, dimensions::create/discard/list, acting::damage/hit/holds
//   (tuff/players.hpp)
// - colored text: Text() << gold("Hello") << ", " << aqua(name).bold()
#ifndef TUFF_HPP
#define TUFF_HPP

#include "tuff/base.hpp"
#include "tuff_gen.hpp"

namespace tuff {

using Player = raw::tuff_player_t;
using BlockPos = raw::tuff_block_pos_t;
using Vec3 = raw::tuff_vec3_t;
using Parser = raw::tuff_parser_t;

inline constexpr const char *OVERWORLD = "minecraft:overworld";
inline constexpr const char *NETHER = "minecraft:the_nether";
inline constexpr const char *END = "minecraft:the_end";

// Text built into a fixed buffer; output past TUFF_TEXT_MAX is dropped and
// truncated() turns true.
class Text {
  public:
    Text() {
        t_.len = 0;
        t_.truncated = false;
    }
    Text &operator<<(const char *s) {
        raw::tuff_put(&t_, s);
        return *this;
    }
    Text &operator<<(Str s) {
        raw::tuff_put_str(&t_, s);
        return *this;
    }
    Text &operator<<(const String &s) { return *this << s.view(); }
    Text &operator<<(int v) {
        raw::tuff_put_int(&t_, v);
        return *this;
    }
    Text &operator<<(long long v) {
        raw::tuff_put_int(&t_, v);
        return *this;
    }
    Text &operator<<(unsigned v) {
        raw::tuff_put_int(&t_, static_cast<int64_t>(v));
        return *this;
    }
    Text &operator<<(unsigned long long v) {
        raw::tuff_put_int(&t_, static_cast<int64_t>(v));
        return *this;
    }
    Text &operator<<(double v) {
        raw::tuff_put_double(&t_, v, 3);
        return *this;
    }
    Text &operator<<(Uuid u) {
        raw::tuff_put_uuid(&t_, u);
        return *this;
    }
    // Color code, style codes, the text and a reset; plain text when the
    // span has neither (the Rust SDK renders spans the same way).
    Text &operator<<(const Span &span) {
        if (!span.color_code() && !has_style(span)) {
            return *this << span.text();
        }
        if (span.color_code()) {
            const char code[] = {span.color_code(), 0};
            *this << TUFF_SECTION << code;
        }
        for (Style st : styles_in_order) {
            if (span.has(st)) {
                const char code[] = {static_cast<char>(st), 0};
                *this << TUFF_SECTION << code;
            }
        }
        return *this << span.text() << TUFF_SECTION "r";
    }
    const raw::tuff_text_t *raw() const { return &t_; }
    bool truncated() const { return t_.truncated; }
    Str view() const { return raw::tuff_text_str(&t_); }

  private:
    static bool has_style(const Span &span) {
        for (Style st : styles_in_order) {
            if (span.has(st)) {
                return true;
            }
        }
        return false;
    }

    raw::tuff_text_t t_;
};

// To the player the current callback acts for; false if refused.
inline bool reply(const char *text) { return raw::tuff_reply(text); }
inline bool reply(const Text &text) { return raw::tuff_reply_text(text.raw()); }
inline bool broadcast(const char *text) { return raw::tuff_broadcast(text); }
inline bool broadcast(const Text &text) { return raw::tuff_broadcast_text(text.raw()); }
// To an online player by uuid, from any callback.
inline bool tell(Uuid player, const char *text) { return raw::tuff_tell(player, text); }
inline bool tell(Uuid player, const Text &text) { return raw::tuff_tell_text(player, text.raw()); }
enum class Level : uint8_t {
    Trace = TUFF_PLUGIN_TYPES_LOG_LEVEL_TRACE,
    Debug = TUFF_PLUGIN_TYPES_LOG_LEVEL_DEBUG,
    Info = TUFF_PLUGIN_TYPES_LOG_LEVEL_INFO,
    Warn = TUFF_PLUGIN_TYPES_LOG_LEVEL_WARN,
    Error = TUFF_PLUGIN_TYPES_LOG_LEVEL_ERROR,
};
inline void log(Level level, const char *text) { raw::tuff_log(static_cast<uint8_t>(level), text); }
inline void log(Level level, const Text &text) {
    raw::tuff_log_str(static_cast<uint8_t>(level), text.view());
}

// Answers one player (or the console) from the callback it was made in or
// any later one: a reply while that callback runs, then a tell; the
// console's messages go to the server log.
class Replier {
  public:
    static Replier console() { return Replier(false, Uuid{0, 0}); }
    static Replier to(Uuid player) { return Replier(true, player); }
    static Replier to(const Player *player) { return player ? to(player->uuid) : console(); }

    bool is_console() const { return !player_; }
    Uuid player() const { return to_; }
    bool in_callback() const { return callback_ == raw::tuff_callback(); }
    bool reply(Str text) const {
        if (!player_) {
            raw::tuff_log_str(TUFF_PLUGIN_TYPES_LOG_LEVEL_INFO, text);
            return true;
        }
        return in_callback() ? raw::tuff_reply_str(text) : raw::tuff_tell_str(to_, text);
    }
    bool reply(const char *text) const { return reply(raw::tuff_str(text)); }
    bool reply(const Text &text) const { return reply(text.view()); }

  private:
    Replier(bool player, Uuid to) : player_(player), to_(to), callback_(raw::tuff_callback()) {}

    bool player_;
    Uuid to_;
    uint64_t callback_;
};

// What a handler returns: a value, or Result::fail("message").
struct Result {
    Result(int32_t v) : ok(true), value(v), error(nullptr) {}
    static Result fail(const char *message) {
        Result r(0);
        r.ok = false;
        r.error = message;
        return r;
    }
    bool ok;
    int32_t value;
    const char *error;
};

// One run of a command, borrowed for the handler's callback.
class Call {
  public:
    explicit Call(const raw::tuff_call_t *call)
        : c_(call), replier_(Replier::to(call ? raw::tuff_sender(call) : nullptr)) {}
    const raw::tuff_call_t *raw() const { return c_; }
    uint32_t handler() const { return c_->handler; }
    // The player who ran it, nullptr for the console.
    const Player *sender() const { return raw::tuff_sender(c_); }
    bool has(const char *name) const { return raw::tuff_arg(c_, name) != nullptr; }
    bool get(const char *name, int32_t &out) const { return raw::tuff_arg_int(c_, name, &out); }
    bool get(const char *name, double &out) const { return raw::tuff_arg_double(c_, name, &out); }
    bool get(const char *name, bool &out) const { return raw::tuff_arg_bool(c_, name, &out); }
    bool get(const char *name, Str &out) const { return raw::tuff_arg_text(c_, name, &out); }
    bool get(const char *name, BlockPos &out) const {
        return raw::tuff_arg_block_pos(c_, name, &out);
    }
    bool get(const char *name, Vec3 &out) const { return raw::tuff_arg_vec3(c_, name, &out); }
    const Player *player(const char *name) const { return raw::tuff_arg_player(c_, name); }
    int32_t integer_or(const char *name, int32_t fallback) const {
        get(name, fallback);
        return fallback;
    }
    // Answers the sender (see Replier).
    const Replier &replier() const { return replier_; }
    template <class T> bool reply(const T &text) const { return replier_.reply(text); }

  protected:
    const raw::tuff_call_t *c_;
    Replier replier_;
};

// A command run an async handler owns, so it outlives the callback.
class Ctx : public Call {
  public:
    explicit Ctx(raw::tuff_call_t *owned) : Call(owned) {}
    Ctx(Ctx &&other) : Call(other) { other.c_ = nullptr; }
    Ctx(const Ctx &) = delete;
    ~Ctx() { raw::tuff_call_free(const_cast<raw::tuff_call_t *>(c_)); }
};

using Handler = Result (*)(Call &);

namespace detail {
inline bool trampoline(const raw::tuff_call_t *call, int32_t *out, Str *err, void *data) {
    Call c(call);
    Result r = (*static_cast<Function<Result(Call &)> *>(data))(c);
    if (!r.ok) {
        return raw::tuff_fail(err, r.error ? r.error : "failed");
    }
    *out = r.value;
    return true;
}
} // namespace detail

using SuggestRequest = raw::tuff_suggest_request_t;

class Suggestions {
  public:
    explicit Suggestions(raw::tuff_suggestions_t *out) : out_(out) {}
    Suggestions &add(Str text) {
        char buf[TUFF_TEXT_MAX];
        size_t n = text.len < sizeof buf - 1 ? text.len : sizeof buf - 1;
        for (size_t i = 0; i < n; i++) {
            buf[i] = text.ptr[i];
        }
        buf[n] = 0;
        raw::tuff_suggest_add(out_, buf);
        return *this;
    }
    Suggestions &add(const char *text) {
        raw::tuff_suggest_add(out_, text);
        return *this;
    }

  private:
    raw::tuff_suggestions_t *out_;
};

namespace detail {
template <class F>
void suggest_thunk(const raw::tuff_suggest_request_t *req, raw::tuff_suggestions_t *out) {
    Suggestions s(out);
    F()(*req, s);
}
} // namespace detail

namespace parser {
inline Parser boolean() { return raw::tuff_parser_boolean(); }
inline Parser integer() { return raw::tuff_parser_integer(); }
inline Parser integer(int32_t min, int32_t max) { return raw::tuff_parser_integer_in(min, max); }
inline Parser number() { return raw::tuff_parser_double(); }
inline Parser number(double min, double max) { return raw::tuff_parser_double_in(min, max); }
inline Parser word() { return raw::tuff_parser_word(); }
inline Parser phrase() { return raw::tuff_parser_phrase(); }
inline Parser greedy() { return raw::tuff_parser_greedy(); }
inline Parser block_pos() { return raw::tuff_parser_block_pos(); }
inline Parser vec3() { return raw::tuff_parser_vec3(); }
inline Parser player() { return raw::tuff_parser_player(); }
inline Parser players() { return raw::tuff_parser_players(); }
inline Parser block_state() { return raw::tuff_parser_block_state(); }
inline Parser item() { return raw::tuff_parser_item(); }
inline Parser message() { return raw::tuff_parser_message(); }
} // namespace parser

// A node of a Tree being built.
class Node {
  public:
    Node(raw::tuff_tree_t *tree, int index) : t_(tree), i_(index) {}
    int index() const { return i_; }
    Node literal(const char *name) const { return Node(t_, raw::tuff_literal(t_, i_, name)); }
    Node argument(const char *name, Parser p) const {
        return Node(t_, raw::tuff_argument(t_, i_, name, p));
    }
    // Runs `handler` (Result(Call &), any callable) when the input ends here.
    template <class F> const Node &executes(F handler) const {
        void *memory = malloc(sizeof(Function<Result(Call &)>));
        if (memory) {
            auto *h = new (memory) Function<Result(Call &)>(move(handler));
            raw::tuff_executes_data(t_, i_, detail::trampoline, h);
        }
        return *this;
    }
    // Starts coroutine `handler` (taking a Ctx) when the input ends here;
    // the command counts as run (result 1) once it first waits.
    template <class F> const Node &executes_async(F handler) const {
        return executes([handler = move(handler)](Call &call) -> Result {
            raw::tuff_call_t *owned = raw::tuff_take_call(call.raw());
            if (!owned) {
                return Result::fail("out of memory");
            }
            handler(Ctx(owned));
            return 1;
        });
    }
    // Binds the arguments into `A` first: A provides
    // `static const char *bind(const Call &, A &)`, returning null on success
    // or the message the sender sees. `handler` is Result(Call &, A &).
    template <class A, class F> const Node &executes_with(F handler) const {
        return executes([handler = move(handler)](Call &call) -> Result {
            A args{};
            if (const char *err = A::bind(call, args)) {
                return Result::fail(err);
            }
            return handler(call, args);
        });
    }
    // executes_with for a coroutine: `handler` is Task(Ctx, A).
    template <class A, class F> const Node &executes_async_with(F handler) const {
        return executes([handler = move(handler)](Call &call) -> Result {
            A args{};
            if (const char *err = A::bind(call, args)) {
                return Result::fail(err);
            }
            raw::tuff_call_t *owned = raw::tuff_take_call(call.raw());
            if (!owned) {
                return Result::fail("out of memory");
            }
            handler(Ctx(owned), move(args));
            return 1;
        });
    }
    const Node &permission(const char *node) const {
        raw::tuff_permission(t_, i_, node);
        return *this;
    }
    // permission, and declares `node` at `level` (0 every player, 1 to 4
    // operators) when the tree is registered.
    const Node &require(const char *node, uint8_t level) const {
        raw::tuff_requires(t_, i_, node, level);
        return *this;
    }
    // What the command does, for Tree::help.
    const Node &about(const char *text) const {
        raw::tuff_about(t_, i_, text);
        return *this;
    }
    // Completions from a captureless lambda void(const SuggestRequest &, Suggestions &).
    template <class F> const Node &suggests(F) const {
        static_assert(__is_empty(F), "suggest handlers cannot capture");
        raw::tuff_suggests(t_, i_, detail::suggest_thunk<F>);
        return *this;
    }
    const Node &redirect(const Node &target) const {
        raw::tuff_redirect(t_, i_, target.i_);
        return *this;
    }

  private:
    raw::tuff_tree_t *t_;
    int i_;
};

// A command tree; registering hands it to the host and frees it.
class Tree {
  public:
    explicit Tree(const char *root) : t_(raw::tuff_tree_new(root)) {}
    ~Tree() { raw::tuff_tree_free(t_); }
    Tree(const Tree &) = delete;
    Tree &operator=(const Tree &) = delete;
    Node root() const { return Node(t_, 0); }
    // Adds a `help` subcommand listing what the sender may run.
    Tree &help() {
        raw::tuff_help(t_);
        return *this;
    }
    bool register_command(Str *err) {
        raw::tuff_tree_t *t = t_;
        t_ = nullptr;
        return raw::tuff_register_command(t, err);
    }
    bool register_tuff_subcommand(Str *err) {
        raw::tuff_tree_t *t = t_;
        t_ = nullptr;
        return raw::tuff_register_tuff_subcommand(t, err);
    }
    Status register_command() { return status(register_command(&err_)); }
    Status register_tuff_subcommand() { return status(register_tuff_subcommand(&err_)); }

  private:
    Status status(bool ok) { return ok ? Status() : Status::fail(String::adopt(err_)); }

    raw::tuff_tree_t *t_;
    Str err_ = {nullptr, 0};
};

} // namespace tuff

#include "tuff/task.hpp"
#include "tuff/io.hpp"
#include "tuff/wire.hpp"
#include "tuff/messaging.hpp"
#include "tuff/world.hpp"
#include "tuff/sockets.hpp"
#include "tuff/events.hpp"
#include "tuff/players.hpp"
#include "tuff/config.hpp"
#include "tuff/store.hpp"
#include "tuff/worldgen.hpp"
#include "tuff/content.hpp"
#include "tuff/plugin.hpp"

#ifndef TUFF_RAW_H
#include "tuff/unraw.hpp"
#endif

#endif
