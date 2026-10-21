// Conformance fixture: the wgreet plugin on the raw C++ binding layer
// (hand-written exports, no TUFF_PLUGIN). Examples use the high-level SDK;
// this file intentionally uses the raw layer to keep it covered, so it
// includes tuff/raw.h in place of tuff.hpp.
#include "tuff/raw.h"

TUFF_MANIFEST("id = wgreet");
TUFF_MANIFEST("version = 1.0.0");
TUFF_MANIFEST("api = 0");
TUFF_MANIFEST("description = Greets players through a typed command tree (C++)");

using tuff::Call;
using tuff::Result;
using tuff::Text;

static Result greet(tuff::Str name, int32_t times) {
    for (int32_t i = 0; i < times; i++) {
        if (!tuff::reply(Text() << "Hello, " << name << "!")) {
            return Result::fail("reply refused");
        }
    }
    return times;
}

extern "C" bool exports_tuff_plugin_guest_register(plugin_string_t *err) {
    tuff_log(TUFF_PLUGIN_TYPES_LOG_LEVEL_INFO, "wit-greeter (C++) registering");
    tuff::Tree tree("wgreet");
    auto root = tree.root();
    root.executes([](Call &call) -> Result {
        const tuff::Player *sender = call.sender();
        return sender ? greet(sender->name, 1) : Result::fail("players only");
    });
    auto to_target = [](Call &call) -> Result {
        const tuff::Player *target = call.player("target");
        if (!target) {
            return Result::fail("target is not a player");
        }
        return greet(target->name, call.integer_or("times", 1));
    };
    root.argument("target", tuff::parser::player())
        .executes(to_target)
        .argument("times", tuff::parser::integer(1, 3))
        .executes(to_target);
    root.literal("sum")
        .argument("a", tuff::parser::integer())
        .argument("b", tuff::parser::integer())
        .executes([](Call &call) -> Result {
            int32_t a, b;
            if (!call.get("a", a) || !call.get("b", b)) {
                return Result::fail("sum takes integers");
            }
            int32_t sum = (int32_t)((uint32_t)a + (uint32_t)b);
            tuff::reply(Text() << a << " + " << b << " = " << sum);
            return sum;
        });
    root.literal("volume")
        .permission("wgreet.volume")
        .argument("level", tuff::parser::number(0.0, 1.0))
        .suggests([](const tuff::SuggestRequest &, tuff::Suggestions &out) {
            out.add("0").add("0.5").add("1");
        })
        .executes([](Call &call) -> Result {
            double level;
            if (!call.get("level", level)) {
                return Result::fail("level is not a double");
            }
            tuff::reply(Text() << "volume " << (int32_t)(level * 100.0) << "%");
            return 1;
        });
    if (!tree.register_command(err)) {
        return false;
    }
    return tuff_subscribe(TUFF_PLUGIN_TYPES_EVENT_KINDS_JOIN | TUFF_PLUGIN_TYPES_EVENT_KINDS_LEAVE) ||
           tuff_fail(err, "subscribe refused");
}

extern "C" void exports_tuff_plugin_guest_on_event(tuff_event_t *ev) {
    if (ev->tag == TUFF_PLUGIN_TYPES_EVENT_JOIN) {
        tuff::broadcast(Text() << ev->val.join.name << " joined");
    } else if (ev->tag == TUFF_PLUGIN_TYPES_EVENT_LEAVE) {
        tuff::broadcast(Text() << ev->val.leave.name << " left");
    }
    exports_tuff_plugin_guest_event_free(ev);
}
