// SPDX-License-Identifier: MIT
/* The tuff plugin SDK for C (and, through tuff.hpp, C++): a layer over the
   bindings wit-bindgen generates from the plugin WIT (bindings/plugin.h).

   - TUFF_MANIFEST("key = value") lines write the tuff-plugin custom section
     from source, no post-link step needed.
   - Command trees are built node by node with function-pointer handlers;
     the SDK's run-command export dispatches to them and reads the
     arguments back by name (tuff_arg_int, tuff_arg_player, ...).
   - Every guest export has a weak default in tuff.c, so a plugin defines
     only the exports it needs (register, on_event, ...).
   - Text is built in fixed buffers (tuff_text_t) without a printf.

   Build with build.sh (freestanding clang + wasm-ld, no wasi-libc). */
#ifndef TUFF_H
#define TUFF_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "plugin.h"

#ifdef __cplusplus
extern "C" {
#endif

/* One line of the tuff-plugin manifest section, such as
   TUFF_MANIFEST("id = greeter"). Lines are joined in source order; the
   text must not contain a double quote or a backslash. */
#define TUFF_DETAIL_MANIFEST(line)                                           \
    __asm__(".section \".custom_section.tuff-plugin\",\"\",@\n"              \
            ".ascii \"" line "\\n\"\n"                                       \
            ".text\n")
#ifndef __cplusplus
#define TUFF_MANIFEST(line) TUFF_DETAIL_MANIFEST(line)
#endif

typedef tuff_plugin_types_invocation_t tuff_call_t;
typedef tuff_plugin_types_suggest_request_t tuff_suggest_request_t;
typedef tuff_plugin_types_arg_value_t tuff_value_t;
typedef tuff_plugin_types_player_t tuff_player_t;
typedef tuff_plugin_types_uuid_t tuff_uuid_t;
typedef tuff_plugin_types_parser_t tuff_parser_t;
typedef tuff_plugin_types_block_pos_t tuff_block_pos_t;
typedef tuff_plugin_types_vec3_t tuff_vec3_t;
typedef tuff_plugin_types_event_t tuff_event_t;
typedef tuff_plugin_types_event_kinds_t tuff_event_kinds_t;

/* ---- strings and text ---- */

/* A borrowed view of a C string (not copied, never freed). */
plugin_string_t tuff_str(const char *text);
bool tuff_str_eq(plugin_string_t a, const char *b);

/* The section sign (UTF-8) that starts a chat formatting code, such as
   TUFF_SECTION "6" for gold; TUFF_SECTION "r" resets. */
#define TUFF_SECTION "\xc2\xa7"

#define TUFF_TEXT_MAX 1024

/* Text built into a fixed buffer; output past TUFF_TEXT_MAX is dropped and
   sets `truncated`. */
typedef struct tuff_text {
    char buf[TUFF_TEXT_MAX];
    size_t len;
    bool truncated;
} tuff_text_t;

void tuff_text_clear(tuff_text_t *text);
void tuff_put(tuff_text_t *text, const char *s);
void tuff_put_str(tuff_text_t *text, plugin_string_t s);
void tuff_put_int(tuff_text_t *text, int64_t value);
/* `decimals` digits after the point, trailing zeros dropped. */
void tuff_put_double(tuff_text_t *text, double value, unsigned decimals);
void tuff_put_uuid(tuff_text_t *text, tuff_uuid_t uuid);
plugin_string_t tuff_text_str(const tuff_text_t *text);

/* ---- messages ---- */

void tuff_log(tuff_plugin_types_log_level_t level, const char *text);
/* To the player the current callback acts for; false if refused. */
bool tuff_reply(const char *text);
bool tuff_reply_text(const tuff_text_t *text);
bool tuff_broadcast(const char *text);
bool tuff_broadcast_text(const tuff_text_t *text);
/* To an online player by uuid, from any callback. */
bool tuff_tell(tuff_uuid_t player, const char *text);
bool tuff_tell_text(tuff_uuid_t player, const tuff_text_t *text);
/* The same with borrowed strings. */
void tuff_log_str(tuff_plugin_types_log_level_t level, plugin_string_t text);
bool tuff_reply_str(plugin_string_t text);
bool tuff_broadcast_str(plugin_string_t text);
bool tuff_tell_str(tuff_uuid_t player, plugin_string_t text);

/* ---- registration (only while registering) ---- */

bool tuff_subscribe(tuff_event_kinds_t kinds);
bool tuff_declare_permission(const char *node, uint8_t level);
/* Calls on_timer(token) after `delay` ticks. */
bool tuff_schedule(uint64_t delay, uint32_t token);

/* ---- command trees ---- */

/* A handler: set *result and return true, or return tuff_fail(err, ...). */
typedef bool (*tuff_handler_fn)(const tuff_call_t *call, int32_t *result, plugin_string_t *err);
typedef bool (*tuff_handler_data_fn)(const tuff_call_t *call, int32_t *result,
                                     plugin_string_t *err, void *data);

typedef struct tuff_suggestions tuff_suggestions_t;
/* Adds completions with tuff_suggest_add; only those starting with what
   the player typed (ignoring ASCII case) are kept. */
typedef void (*tuff_suggest_fn)(const tuff_suggest_request_t *req, tuff_suggestions_t *out);
void tuff_suggest_add(tuff_suggestions_t *out, const char *text);

#define TUFF_MAX_NODES 256

typedef struct tuff_tree tuff_tree_t;

/* A tree whose node 0 is the literal `root`, the command's name. */
tuff_tree_t *tuff_tree_new(const char *root);
/* Children of `parent`, numbered in creation order; -1 when the tree is
   full or the parent is not a node (registering then fails). */
int tuff_literal(tuff_tree_t *tree, int parent, const char *name);
int tuff_argument(tuff_tree_t *tree, int parent, const char *name, tuff_parser_t parser);
void tuff_executes(tuff_tree_t *tree, int node, tuff_handler_fn handler);
void tuff_executes_data(tuff_tree_t *tree, int node, tuff_handler_data_fn handler, void *data);
void tuff_permission(tuff_tree_t *tree, int node, const char *permission);
/* tuff_permission, and declares `permission` when the tree is registered:
   level 0 gives it to every player, 1 to 4 to operators of that level. */
void tuff_requires(tuff_tree_t *tree, int node, const char *permission, uint8_t level);
/* What the node's command does, for tuff_help. */
void tuff_about(tuff_tree_t *tree, int node, const char *text);
/* When registered, adds a `help` subcommand listing every node with a
   handler (as "/path <arg> - about") that the sender may run; the root runs
   it too when it has no handler. */
void tuff_help(tuff_tree_t *tree);
void tuff_suggests(tuff_tree_t *tree, int node, tuff_suggest_fn suggester);
/* Continue parsing at `target`, as /execute run does. */
void tuff_redirect(tuff_tree_t *tree, int node, int target);
/* Registers /root and frees the tree; on failure `err` (if not NULL) gets
   the reason, to free with plugin_string_free. Handler ids are given in
   node order. */
bool tuff_register_command(tuff_tree_t *tree, plugin_string_t *err);
/* Registers /tuff root; handlers see the line as "tuff root ...". */
bool tuff_register_tuff_subcommand(tuff_tree_t *tree, plugin_string_t *err);
/* Frees a tree that was not registered. */
void tuff_tree_free(tuff_tree_t *tree);

tuff_parser_t tuff_parser_boolean(void);
tuff_parser_t tuff_parser_integer(void);
tuff_parser_t tuff_parser_integer_in(int32_t min, int32_t max);
tuff_parser_t tuff_parser_double(void);
tuff_parser_t tuff_parser_double_in(double min, double max);
tuff_parser_t tuff_parser_word(void);
tuff_parser_t tuff_parser_phrase(void);
tuff_parser_t tuff_parser_greedy(void);
tuff_parser_t tuff_parser_block_pos(void);
tuff_parser_t tuff_parser_vec3(void);
tuff_parser_t tuff_parser_player(void);
tuff_parser_t tuff_parser_players(void);
tuff_parser_t tuff_parser_block_state(void);
tuff_parser_t tuff_parser_item(void);
tuff_parser_t tuff_parser_message(void);

/* ---- invocations ---- */

/* Copies `message` into *err and returns false, for handlers. */
bool tuff_fail(plugin_string_t *err, const char *message);
/* The player who ran the command, NULL for the console. */
const tuff_player_t *tuff_sender(const tuff_call_t *call);
const tuff_value_t *tuff_arg(const tuff_call_t *call, const char *name);
/* False when the argument is missing or of another kind. */
bool tuff_arg_bool(const tuff_call_t *call, const char *name, bool *out);
bool tuff_arg_int(const tuff_call_t *call, const char *name, int32_t *out);
/* Also reads integers. */
bool tuff_arg_double(const tuff_call_t *call, const char *name, double *out);
/* Text, block state, item and message arguments. */
bool tuff_arg_text(const tuff_call_t *call, const char *name, plugin_string_t *out);
bool tuff_arg_block_pos(const tuff_call_t *call, const char *name, tuff_block_pos_t *out);
bool tuff_arg_vec3(const tuff_call_t *call, const char *name, tuff_vec3_t *out);
const tuff_player_t *tuff_arg_player(const tuff_call_t *call, const char *name);

/* ---- dispatch (for layers such as tuff.hpp) ---- */

/* Marks the start of a guest callback; the default exports call it. */
void tuff_enter(void);
/* Which callback is running: changes with every tuff_enter. */
uint64_t tuff_callback(void);
/* Runs the handler for `call`, then frees it unless a handler took it. */
bool tuff_run_command(tuff_call_t *call, int32_t *ret, plugin_string_t *err);
/* Answers a suggestion request from the installed suggesters, then frees it. */
void tuff_suggest(tuff_suggest_request_t *req, plugin_list_string_t *ret);
/* From a handler: keeps the invocation past the callback (for an async
   handler). Free it with tuff_call_free. */
tuff_call_t *tuff_take_call(const tuff_call_t *call);
void tuff_call_free(tuff_call_t *call);

#ifdef __cplusplus
}
#endif

#endif
