/* wit-greeter in C, on the tuff C SDK (crates/plugin-sdk/c): the same
   plugin as ../rust-wit-greeter. */
#include "tuff.h"

TUFF_MANIFEST("id = wgreet");
TUFF_MANIFEST("version = 1.0.0");
TUFF_MANIFEST("api = 0");
TUFF_MANIFEST("description = Greets players through a typed command tree (C)");

static bool greet(plugin_string_t name, int32_t times, int32_t *result, plugin_string_t *err) {
    for (int32_t i = 0; i < times; i++) {
        tuff_text_t t = {.len = 0};
        tuff_put(&t, "Hello, ");
        tuff_put_str(&t, name);
        tuff_put(&t, "!");
        if (!tuff_reply_text(&t)) {
            return tuff_fail(err, "reply refused");
        }
    }
    *result = times;
    return true;
}

static bool greet_sender(const tuff_call_t *call, int32_t *result, plugin_string_t *err) {
    const tuff_player_t *sender = tuff_sender(call);
    return sender ? greet(sender->name, 1, result, err) : tuff_fail(err, "players only");
}

static bool greet_target(const tuff_call_t *call, int32_t *result, plugin_string_t *err) {
    const tuff_player_t *target = tuff_arg_player(call, "target");
    int32_t times = 1;
    if (!target) {
        return tuff_fail(err, "target is not a player");
    }
    if (tuff_arg(call, "times") && !tuff_arg_int(call, "times", &times)) {
        return tuff_fail(err, "times is not an integer");
    }
    return greet(target->name, times, result, err);
}

static bool sum(const tuff_call_t *call, int32_t *result, plugin_string_t *err) {
    int32_t a, b;
    if (!tuff_arg_int(call, "a", &a) || !tuff_arg_int(call, "b", &b)) {
        return tuff_fail(err, "sum takes integers");
    }
    int32_t total = (int32_t)((uint32_t)a + (uint32_t)b);
    tuff_text_t t = {.len = 0};
    tuff_put_int(&t, a);
    tuff_put(&t, " + ");
    tuff_put_int(&t, b);
    tuff_put(&t, " = ");
    tuff_put_int(&t, total);
    *result = total;
    return tuff_reply_text(&t) || tuff_fail(err, "reply refused");
}

static bool volume(const tuff_call_t *call, int32_t *result, plugin_string_t *err) {
    double level;
    if (!tuff_arg_double(call, "level", &level)) {
        return tuff_fail(err, "level is not a double");
    }
    tuff_text_t t = {.len = 0};
    tuff_put(&t, "volume ");
    tuff_put_int(&t, (int32_t)(level * 100.0));
    tuff_put(&t, "%");
    *result = 1;
    return tuff_reply_text(&t) || tuff_fail(err, "reply refused");
}

static void levels(const tuff_suggest_request_t *req, tuff_suggestions_t *out) {
    (void)req;
    tuff_suggest_add(out, "0");
    tuff_suggest_add(out, "0.5");
    tuff_suggest_add(out, "1");
}

bool exports_tuff_plugin_guest_register(plugin_string_t *err) {
    tuff_log(TUFF_PLUGIN_TYPES_LOG_LEVEL_INFO, "wit-greeter (C) registering");
    tuff_tree_t *t = tuff_tree_new("wgreet");
    tuff_executes(t, 0, greet_sender);
    int target = tuff_argument(t, 0, "target", tuff_parser_player());
    tuff_executes(t, target, greet_target);
    tuff_executes(t, tuff_argument(t, target, "times", tuff_parser_integer_in(1, 3)), greet_target);
    int a = tuff_argument(t, tuff_literal(t, 0, "sum"), "a", tuff_parser_integer());
    tuff_executes(t, tuff_argument(t, a, "b", tuff_parser_integer()), sum);
    int vol = tuff_literal(t, 0, "volume");
    tuff_permission(t, vol, "wgreet.volume");
    int level = tuff_argument(t, vol, "level", tuff_parser_double_in(0.0, 1.0));
    tuff_suggests(t, level, levels);
    tuff_executes(t, level, volume);
    if (!tuff_register_command(t, err)) {
        return false;
    }
    return tuff_subscribe(TUFF_PLUGIN_TYPES_EVENT_KINDS_JOIN | TUFF_PLUGIN_TYPES_EVENT_KINDS_LEAVE) ||
           tuff_fail(err, "subscribe refused");
}

void exports_tuff_plugin_guest_on_event(tuff_event_t *ev) {
    tuff_text_t t = {.len = 0};
    if (ev->tag == TUFF_PLUGIN_TYPES_EVENT_JOIN) {
        tuff_put_str(&t, ev->val.join.name);
        tuff_put(&t, " joined");
    } else if (ev->tag == TUFF_PLUGIN_TYPES_EVENT_LEAVE) {
        tuff_put_str(&t, ev->val.leave.name);
        tuff_put(&t, " left");
    }
    if (t.len) {
        tuff_broadcast_text(&t);
    }
    exports_tuff_plugin_guest_event_free(ev);
}
