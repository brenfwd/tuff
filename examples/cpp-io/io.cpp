// SPDX-License-Identifier: MIT
// A plugin on the C++ SDK's awaitable capability calls and world handles.
//
// - /io get <url> fetches a URL and reports the status and body size
// - /io save <text> writes notes.txt, reads it back and lists the store
// - /io upper <text> upper-cases text with a background task, off the tick thread
// - /io probe gives bread, nudges the player and reads a block
#include "tuff.hpp"

using namespace tuff;

static Task fetch(Ctx ctx, String url) {
    auto reply = co_await http::get(url.view()).timeout(5000);
    if (!reply) {
        ctx.reply(Text() << red("fetch failed: ") << "denied " << (int)reply.error().tag);
        co_return;
    }
    ctx.reply(Text() << "status " << (int)reply->status << ", " << (unsigned long long)reply->body.len()
                     << " bytes: " << reply->body.view());
}

static Task save(Ctx ctx, String text) {
    auto wrote = co_await files::write("notes.txt", text.view());
    if (!wrote) {
        ctx.reply(Text() << red("write failed: ") << (int)wrote.error().tag);
        co_return;
    }
    auto data = co_await files::read("notes.txt");
    if (!data) {
        ctx.reply(Text() << red("read failed: ") << (int)data.error().tag);
        co_return;
    }
    ctx.reply(Text() << "read back: " << data->data.view());
    auto listing = co_await files::list("");
    if (listing) {
        ctx.reply(Text() << (unsigned long long)listing->entries.size() << " file(s)");
    }
    auto missing = co_await files::read("missing.txt");
    ctx.reply(Text() << "missing: " << (missing.error().not_found() ? "not found" : "other"));
}

static Task upper(Ctx ctx, String text) {
    auto result = co_await background("upper", text.view());
    if (!result) {
        ctx.reply(Text() << red("background failed: ") << (int)result.error().kind);
        co_return;
    }
    ctx.reply(Text() << "upper: " << result->view());
}

static net::Socket echo_socket;

static void echo(Ctx ctx, uint16_t port) {
    net::Handlers h;
    h.connected = [](const Denied *refused) {
        if (!refused) {
            echo_socket.send("ping");
        }
    };
    h.data = [ctx = move(ctx)](Str bytes) mutable {
        ctx.reply(Text() << "echo: " << bytes);
        echo_socket.close();
    };
    auto socket = net::tcp_connect(raw::tuff_str("127.0.0.1"), port, false, move(h));
    if (socket) {
        echo_socket = *socket;
    }
}

static Result probe(Call &call) {
    auto dimension = acting::world();
    if (!dimension) {
        return Result::fail("players only");
    }
    call.reply(Text() << "in " << dimension->key());
    auto held = acting::has_permission("io.use");
    call.reply(Text() << "io.use: " << (held && *held ? "yes" : "no"));
    auto gave = acting::give("minecraft:bread", 3);
    call.reply(Text() << "give: " << (gave ? "ok" : "refused"));
    auto moved = acting::teleport({1.5, 70, 2.5});
    call.reply(Text() << "teleport: " << (moved ? "ok" : "refused"));
    auto block = dimension->block({0, 64, 0});
    call.reply(Text() << "block: " << (block ? "read" : "unavailable"));
    return 1;
}

struct Io {
    // Runs on a separate instance where every host call traps.
    static Status background(Str task, Str input, Bytes &out) {
        if (!(task == raw::tuff_str("upper"))) {
            return Status::fail("unknown task");
        }
        uint8_t *copy = static_cast<uint8_t *>(malloc(input.len ? input.len : 1));
        for (size_t i = 0; i < input.len; i++) {
            uint8_t c = input.ptr[i];
            copy[i] = (c >= 'a' && c <= 'z') ? c - 32 : c;
        }
        out = Bytes::adopt({copy, input.len});
        return {};
    }

    static Status on_register() {
        Tree tree("io");
        Node root = tree.root();
        root.literal("get")
            .argument("url", parser::greedy())
            .executes_async([](Ctx ctx) {
                Str url;
                ctx.get("url", url);
                fetch(move(ctx), String::copy(url));
            });
        root.literal("save")
            .argument("text", parser::greedy())
            .executes_async([](Ctx ctx) {
                Str text;
                ctx.get("text", text);
                save(move(ctx), String::copy(text));
            });
        root.literal("upper")
            .argument("text", parser::greedy())
            .executes_async([](Ctx ctx) {
                Str text;
                ctx.get("text", text);
                upper(move(ctx), String::copy(text));
            });
        root.literal("echo")
            .argument("port", parser::integer())
            .executes_async([](Ctx ctx) {
                int32_t port = 0;
                ctx.get("port", port);
                echo(move(ctx), static_cast<uint16_t>(port));
            });
        root.literal("probe").executes(probe);
        TUFF_CHECK(tree.help().register_command());
        return {};
    }
};

TUFF_CAP_HTTP("http://127.0.0.1", "Fetches pages for /io get");
TUFF_CAP_TCP("127.0.0.1", "Echo test for /io echo");
TUFF_CAP_FILES("1 MiB", "Keeps notes for /io save");
TUFF_PLUGIN(Io, "io", "0.1.0", "Fetches pages and keeps notes");
