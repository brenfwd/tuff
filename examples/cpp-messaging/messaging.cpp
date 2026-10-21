// SPDX-License-Identifier: MIT
// Calls the optional greeter plugin through its typed interface, in C++ (the
// counterpart of ../rust-messaging-client). The interface header is generated
// from the provider's WIT:
//
//     cargo run -p tuff-plugin-sdk-gen -- interface <greeter wit dir> --cpp greeter_api.hpp
//
// - /cppgreeted greet <name> asks the greeter to greet <name>, and falls back
//   to a plain greeting when the greeter is not installed
// - /cppgreeted answer repeats the last answer
// - /cppgreeted last reports the last `counted` notification the greeter sent
// - /cppgreeted greeter reports the last plugin change for the greeter
#include "tuff.hpp"

#include "greeter_api.hpp"

using namespace tuff;

namespace {

char answer_text[256];
size_t answer_len;
char last_name[64];
size_t last_name_len;
uint32_t last_total;
int greeter_state = -1;

void keep(char *buffer, size_t capacity, size_t &len, Str text) {
    len = text.len < capacity ? text.len : capacity;
    memcpy(buffer, text.ptr, len);
}

void say(Ctx &ctx, Str line) {
    keep(answer_text, sizeof(answer_text), answer_len, line);
    ctx.reply(line);
}

Task greet_through_greeter(Ctx ctx, String name) {
    auto reply = co_await greet::Greet::any().greet(move(name));
    if (reply) {
        say(ctx, reply->view());
    } else if (reply.error().is_app) {
        say(ctx, str("Say who to greet"));
    } else if (reply.error().call.no_provider()) {
        say(ctx, str("The greeter plugin is not installed, so: hello there"));
    } else {
        say(ctx, str("The greeter failed"));
    }
}

Result show_answer(Call &call) {
    call.reply(Str{reinterpret_cast<uint8_t *>(answer_text), answer_len});
    return 1;
}

Result show_last(Call &call) {
    if (last_name_len == 0) {
        call.reply("Nobody greeted yet");
    } else {
        call.reply(Text() << "Last greeted " << Str{reinterpret_cast<uint8_t *>(last_name), last_name_len}
                          << " (" << static_cast<int>(last_total) << ")");
    }
    return 1;
}

Result show_greeter(Call &call) {
    call.reply(greeter_state == 1   ? "greeter is loaded"
               : greeter_state == 0 ? "greeter is gone"
                                    : "greeter state unknown");
    return 1;
}

} // namespace

struct Counts {
    static void counted(Str, String name, uint32_t total) {
        keep(last_name, sizeof(last_name), last_name_len, name.view());
        last_total = total;
    }
};

struct Client {
    static Status on_register() {
        TUFF_CHECK(counted::serve<Counts>());
        Tree tree("cppgreeted");
        Node root = tree.root();
        root.literal("greet")
            .argument("name", parser::word())
            .executes_async([](Ctx ctx) {
                Str name;
                ctx.get("name", name);
                greet_through_greeter(move(ctx), String::copy(name));
            });
        root.literal("answer").executes(show_answer);
        root.literal("last").executes(show_last);
        root.literal("greeter").executes(show_greeter);
        return tree.register_command();
    }

    static void plugin_change(Str id, Str, bool loaded) {
        if (id == str("greeter")) {
            greeter_state = loaded ? 1 : 0;
        }
    }
};

TUFF_SOFT_DEPENDS("greeter");
TUFF_CAP_CROSS_PLUGIN("all", "Calls the greeter plugin");
TUFF_PLUGIN(Client, "cpp-messaging", "0.1.0", "Calls the optional greeter plugin");
