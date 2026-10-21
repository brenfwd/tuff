// SPDX-License-Identifier: MIT
// The hello plugin, on the tuff C++ SDK (tuff.hpp). Build it with
// ./build.sh and load build/hello.wasm with /tuff plugin load.
//
// - /hello greets you and counts your visits (kept across restarts)
// - /hello later greets you again two seconds later
// - /hello reset forgets your visits (operators)
// - /hello help lists these
// - joining players get a welcome, diamond finds are announced and a tip
//   is broadcast every five minutes
// - owners change the greeting with the plugins.hello.greeting setting
#include "tuff.hpp"

using namespace tuff;

static constinit Setting<String> greeting("greeting", "Hello", "What /hello says");
static constinit StoredMap<uint32_t> visits("visits");

static Result greet(Call &call) {
    const Player *player = call.sender();
    if (!player) {
        return Result::fail("players only");
    }
    Option<uint32_t> n = visits.update(player->uuid, [](Option<uint32_t> old) {
        return old.value_or(0) + 1;
    });
    if (!n) {
        return Result::fail("the store refused");
    }
    call.reply(Text() << gold(greeting.get()) << ", " << aqua(player->name) << "! "
                      << gray(Text() << "(visit " << *n << ")"));
    return static_cast<int32_t>(*n);
}

static Task greet_later(Ctx ctx) {
    ctx.reply("See you in two seconds");
    co_await sleep(40);
    const Player *player = ctx.sender();
    Str name = player ? player->name : str("console");
    ctx.reply(Text() << gold(greeting.get()) << " again, " << aqua(name) << "!");
}

static Result reset(Call &call) {
    const Player *player = call.sender();
    if (!player) {
        return Result::fail("players only");
    }
    if (!visits.remove(player->uuid)) {
        return Result::fail("the store refused");
    }
    call.reply("Your visits are forgotten");
    return 1;
}

struct Hello {
    static Status on_register() {
        TUFF_CHECK(greeting.declare());
        TUFF_CHECK(greeting.on_change([](Str value) {
            broadcast(Text() << "The greeting is now " << gold(value));
        }));

        Tree tree("hello");
        Node root = tree.root();
        root.about("Greets you").executes(greet);
        root.literal("later").about("Greets you again in two seconds").executes_async(greet_later);
        root.literal("reset").about("Forgets your visits").require("hello.reset", 2).executes(reset);
        TUFF_CHECK(tree.help().register_command());

        TUFF_CHECK(on<ev::Join>([](const Player &player) {
            tell(player.uuid, Text() << "Welcome! Say hi with " << gold("/hello"));
        }));
        TUFF_CHECK(listen<ev::BlockBreak>()
                       .in_dimension(OVERWORLD)
                       .block("minecraft:diamond_ore")
                       .by_players()
                       .run([](const auto &change) {
                           broadcast(Text() << aqua(player_of(change)->name) << " found "
                                            << aqua("diamonds").bold() << "!");
                       }));
        every(20 * 60 * 5, [] { broadcast(Text() << green("Tip: ") << "say hi with /hello"); });
        return {};
    }
};

TUFF_PLUGIN(Hello, "hello", "0.1.0", "Greets players and counts their visits");
