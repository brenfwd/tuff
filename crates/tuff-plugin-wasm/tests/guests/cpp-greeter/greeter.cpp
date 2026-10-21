// The wgreet plugin in C++ on the tuff SDK (tuff.hpp): the same plugin as
// ../rust-wit-greeter.
#include "tuff.hpp"

using namespace tuff;

static Result greet(Str name, int32_t times) {
    for (int32_t i = 0; i < times; i++) {
        if (!reply(Text() << "Hello, " << name << "!")) {
            return Result::fail("reply refused");
        }
    }
    return times;
}

static void levels(const SuggestRequest &, Suggestions &out) {
    out.add("0").add("0.5").add("1");
}

static Result to_target(Call &call) {
    const Player *target = call.player("target");
    if (!target) {
        return Result::fail("target is not a player");
    }
    return greet(target->name, call.integer_or("times", 1));
}

struct Greeter {
    static Status on_register() {
        log(Level::Info, "wit-greeter (C++) registering");
        Tree tree("wgreet");
        Node root = tree.root();
        root.executes([](Call &call) -> Result {
            const Player *sender = call.sender();
            return sender ? greet(sender->name, 1) : Result::fail("players only");
        });
        root.argument("target", parser::player())
            .executes(to_target)
            .argument("times", parser::integer(1, 3))
            .executes(to_target);
        root.literal("sum")
            .argument("a", parser::integer())
            .argument("b", parser::integer())
            .executes([](Call &call) -> Result {
                int32_t a, b;
                if (!call.get("a", a) || !call.get("b", b)) {
                    return Result::fail("sum takes integers");
                }
                int32_t sum = static_cast<int32_t>(static_cast<uint32_t>(a) + static_cast<uint32_t>(b));
                call.reply(Text() << a << " + " << b << " = " << sum);
                return sum;
            });
        root.literal("volume")
            .permission("wgreet.volume")
            .argument("level", parser::number(0.0, 1.0))
            .suggests([](const SuggestRequest &r, Suggestions &o) { levels(r, o); })
            .executes([](Call &call) -> Result {
                double level;
                if (!call.get("level", level)) {
                    return Result::fail("level is not a double");
                }
                call.reply(Text() << "volume " << static_cast<int32_t>(level * 100.0) << "%");
                return 1;
            });
        TUFF_CHECK(tree.register_command());
        TUFF_CHECK(on<ev::Join>([](const Player &p) { broadcast(Text() << p.name << " joined"); }));
        TUFF_CHECK(on<ev::Leave>([](const Player &p) { broadcast(Text() << p.name << " left"); }));
        return {};
    }
};

TUFF_PLUGIN(Greeter, "wgreet", "1.0.0", "Greets players through a typed command tree (C++)");
