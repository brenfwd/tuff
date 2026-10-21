// SPDX-License-Identifier: MIT
/* The C SDK: see tuff.h. */
#include <stdlib.h>
#include <string.h>

#include "tuff.h"

#define WEAK __attribute__((weak))

/* ---- strings and text ---- */

plugin_string_t tuff_str(const char *text) {
    plugin_string_t s = {(uint8_t *)text, strlen(text)};
    return s;
}

static bool same(plugin_string_t a, const char *b, size_t n) {
    if (a.len != n) {
        return false;
    }
    for (size_t i = 0; i < n; i++) {
        if (a.ptr[i] != (uint8_t)b[i]) {
            return false;
        }
    }
    return true;
}

bool tuff_str_eq(plugin_string_t a, const char *b) {
    return same(a, b, strlen(b));
}

void tuff_text_clear(tuff_text_t *text) {
    text->len = 0;
    text->truncated = false;
}

static void put_n(tuff_text_t *text, const uint8_t *s, size_t n) {
    for (size_t i = 0; i < n; i++) {
        if (text->len >= TUFF_TEXT_MAX) {
            text->truncated = true;
            return;
        }
        text->buf[text->len++] = (char)s[i];
    }
}

void tuff_put(tuff_text_t *text, const char *s) {
    put_n(text, (const uint8_t *)s, strlen(s));
}

void tuff_put_str(tuff_text_t *text, plugin_string_t s) {
    put_n(text, s.ptr, s.len);
}

static void put_unsigned(tuff_text_t *text, uint64_t u) {
    char digits[24];
    size_t n = 0;
    do {
        digits[n++] = (char)('0' + u % 10);
        u /= 10;
    } while (u);
    while (n) {
        put_n(text, (const uint8_t *)&digits[--n], 1);
    }
}

void tuff_put_int(tuff_text_t *text, int64_t value) {
    if (value < 0) {
        tuff_put(text, "-");
        put_unsigned(text, (uint64_t)0 - (uint64_t)value);
    } else {
        put_unsigned(text, (uint64_t)value);
    }
}

void tuff_put_double(tuff_text_t *text, double value, unsigned decimals) {
    if (value != value) {
        tuff_put(text, "NaN");
        return;
    }
    if (value < 0) {
        tuff_put(text, "-");
        value = -value;
    }
    if (value > 1e18) {
        tuff_put(text, "inf");
        return;
    }
    if (decimals > 9) {
        decimals = 9;
    }
    uint64_t scale = 1;
    for (unsigned i = 0; i < decimals; i++) {
        scale *= 10;
    }
    uint64_t whole = (uint64_t)value;
    uint64_t frac = (uint64_t)((value - (double)whole) * (double)scale + 0.5);
    if (frac >= scale) {
        whole++;
        frac -= scale;
    }
    put_unsigned(text, whole);
    while (decimals && frac % 10 == 0) {
        frac /= 10;
        decimals--;
    }
    if (decimals) {
        char digits[10];
        for (unsigned i = decimals; i > 0; i--) {
            digits[i - 1] = (char)('0' + frac % 10);
            frac /= 10;
        }
        tuff_put(text, ".");
        put_n(text, (const uint8_t *)digits, decimals);
    }
}

void tuff_put_uuid(tuff_text_t *text, tuff_uuid_t uuid) {
    static const char hex[] = "0123456789abcdef";
    uint64_t halves[2] = {uuid.high, uuid.low};
    for (int h = 0; h < 2; h++) {
        for (int shift = 60; shift >= 0; shift -= 4) {
            put_n(text, (const uint8_t *)&hex[(halves[h] >> shift) & 15], 1);
        }
    }
}

plugin_string_t tuff_text_str(const tuff_text_t *text) {
    plugin_string_t s = {(uint8_t *)text->buf, text->len};
    return s;
}

/* ---- messages ---- */

static bool sent(bool ok, tuff_plugin_host_host_error_t *err) {
    if (!ok && err->tag == TUFF_PLUGIN_TYPES_HOST_ERROR_INVALID) {
        plugin_string_free(&err->val.invalid);
    }
    return ok;
}

void tuff_log(tuff_plugin_types_log_level_t level, const char *text) {
    tuff_log_str(level, tuff_str(text));
}

void tuff_log_str(tuff_plugin_types_log_level_t level, plugin_string_t text) {
    tuff_plugin_host_log(level, &text);
}

bool tuff_reply_str(plugin_string_t s) {
    tuff_plugin_host_host_error_t err;
    return sent(tuff_plugin_host_reply(&s, &err), &err);
}

bool tuff_reply(const char *text) {
    return tuff_reply_str(tuff_str(text));
}

bool tuff_reply_text(const tuff_text_t *text) {
    return tuff_reply_str(tuff_text_str(text));
}

bool tuff_broadcast_str(plugin_string_t s) {
    tuff_plugin_host_host_error_t err;
    return sent(tuff_plugin_host_broadcast(&s, &err), &err);
}

bool tuff_broadcast(const char *text) {
    return tuff_broadcast_str(tuff_str(text));
}

bool tuff_broadcast_text(const tuff_text_t *text) {
    return tuff_broadcast_str(tuff_text_str(text));
}

bool tuff_tell_str(tuff_uuid_t player, plugin_string_t s) {
    tuff_plugin_host_host_error_t err;
    return sent(tuff_plugin_host_tell(&player, &s, &err), &err);
}

bool tuff_tell(tuff_uuid_t player, const char *text) {
    return tuff_tell_str(player, tuff_str(text));
}

bool tuff_tell_text(tuff_uuid_t player, const tuff_text_t *text) {
    return tuff_tell_str(player, tuff_text_str(text));
}

/* ---- registration ---- */

static bool accepted(bool ok, tuff_plugin_host_register_error_t *refusal) {
    if (!ok) {
        tuff_plugin_host_register_error_free(refusal);
    }
    return ok;
}

bool tuff_subscribe(tuff_event_kinds_t kinds) {
    tuff_plugin_host_register_error_t refusal;
    return accepted(tuff_plugin_host_subscribe(kinds, &refusal), &refusal);
}

bool tuff_declare_permission(const char *node, uint8_t level) {
    plugin_string_t s = tuff_str(node);
    tuff_plugin_host_register_error_t refusal;
    return accepted(tuff_plugin_host_declare_permission(&s, level, &refusal), &refusal);
}

bool tuff_schedule(uint64_t delay, uint32_t token) {
    tuff_plugin_host_host_error_t err;
    return sent(tuff_plugin_host_schedule(delay, token, &err), &err);
}

/* ---- command trees ---- */

typedef struct handler {
    tuff_handler_fn plain;
    tuff_handler_data_fn with_data;
    void *data;
} handler_t;

typedef struct suggester {
    char *root;
    char *node;
    bool tuff;
    tuff_suggest_fn fn;
} suggester_t;

static handler_t *handlers;
static size_t handler_count;
static suggester_t *suggesters;
static size_t suggester_count;

typedef struct pending {
    char *name;
    tuff_plugin_types_node_kind_t kind;
    int parent;
    handler_t handler;
    char *permission;
    tuff_suggest_fn suggest;
    int redirect;
    char *about;
    bool declare;
    uint8_t level;
} pending_t;

struct tuff_tree {
    pending_t nodes[TUFF_MAX_NODES];
    size_t count;
    bool failed;
    bool help;
};

static char *copy(const char *s) {
    size_t n = strlen(s) + 1;
    char *out = malloc(n);
    if (out) {
        memcpy(out, s, n);
    }
    return out;
}

static int add(tuff_tree_t *tree, int parent, const char *name, tuff_plugin_types_node_kind_t kind) {
    if (!tree || tree->count >= TUFF_MAX_NODES || (tree->count > 0 && (parent < 0 || (size_t)parent >= tree->count))) {
        if (tree) {
            tree->failed = true;
        }
        return -1;
    }
    pending_t *node = &tree->nodes[tree->count];
    memset(node, 0, sizeof *node);
    node->name = copy(name);
    node->kind = kind;
    node->parent = parent;
    node->redirect = -1;
    if (!node->name) {
        tree->failed = true;
        return -1;
    }
    return (int)tree->count++;
}

static pending_t *at(tuff_tree_t *tree, int node) {
    if (!tree || node < 0 || (size_t)node >= tree->count) {
        if (tree) {
            tree->failed = true;
        }
        return NULL;
    }
    return &tree->nodes[node];
}

tuff_tree_t *tuff_tree_new(const char *root) {
    tuff_tree_t *tree = malloc(sizeof *tree);
    if (!tree) {
        return NULL;
    }
    tree->count = 0;
    tree->failed = false;
    tree->help = false;
    tuff_plugin_types_node_kind_t literal = {.tag = TUFF_PLUGIN_TYPES_NODE_KIND_LITERAL};
    add(tree, -1, root, literal);
    return tree;
}

int tuff_literal(tuff_tree_t *tree, int parent, const char *name) {
    tuff_plugin_types_node_kind_t literal = {.tag = TUFF_PLUGIN_TYPES_NODE_KIND_LITERAL};
    return add(tree, parent, name, literal);
}

int tuff_argument(tuff_tree_t *tree, int parent, const char *name, tuff_parser_t parser) {
    tuff_plugin_types_node_kind_t argument = {.tag = TUFF_PLUGIN_TYPES_NODE_KIND_ARGUMENT};
    argument.val.argument = parser;
    return add(tree, parent, name, argument);
}

void tuff_executes(tuff_tree_t *tree, int node, tuff_handler_fn handler) {
    pending_t *n = at(tree, node);
    if (n) {
        n->handler.plain = handler;
        n->handler.with_data = NULL;
    }
}

void tuff_executes_data(tuff_tree_t *tree, int node, tuff_handler_data_fn handler, void *data) {
    pending_t *n = at(tree, node);
    if (n) {
        n->handler.plain = NULL;
        n->handler.with_data = handler;
        n->handler.data = data;
    }
}

void tuff_permission(tuff_tree_t *tree, int node, const char *permission) {
    pending_t *n = at(tree, node);
    if (n) {
        free(n->permission);
        n->permission = copy(permission);
        tree->failed |= !n->permission;
    }
}

void tuff_requires(tuff_tree_t *tree, int node, const char *permission, uint8_t level) {
    tuff_permission(tree, node, permission);
    pending_t *n = at(tree, node);
    if (n) {
        n->declare = true;
        n->level = level;
    }
}

void tuff_about(tuff_tree_t *tree, int node, const char *text) {
    pending_t *n = at(tree, node);
    if (n) {
        free(n->about);
        n->about = copy(text);
        tree->failed |= !n->about;
    }
}

void tuff_help(tuff_tree_t *tree) {
    if (tree) {
        tree->help = true;
    }
}

void tuff_suggests(tuff_tree_t *tree, int node, tuff_suggest_fn suggester) {
    pending_t *n = at(tree, node);
    if (n) {
        n->suggest = suggester;
    }
}

void tuff_redirect(tuff_tree_t *tree, int node, int target) {
    pending_t *n = at(tree, node);
    if (n && at(tree, target)) {
        n->redirect = target;
    }
}

void tuff_tree_free(tuff_tree_t *tree) {
    if (!tree) {
        return;
    }
    for (size_t i = 0; i < tree->count; i++) {
        free(tree->nodes[i].name);
        free(tree->nodes[i].permission);
        free(tree->nodes[i].about);
    }
    free(tree);
}

/* ---- help ---- */

typedef struct help_line {
    char *text;
    char *permission;
} help_line_t;

typedef struct help {
    help_line_t *lines;
    size_t count;
} help_t;

static char *text_copy(const tuff_text_t *t) {
    char *out = malloc(t->len + 1);
    if (out) {
        memcpy(out, t->buf, t->len);
        out[t->len] = 0;
    }
    return out;
}

static void put_path(tuff_text_t *t, tuff_tree_t *tree, int node) {
    pending_t *p = &tree->nodes[node];
    if (p->parent >= 0) {
        put_path(t, tree, p->parent);
        tuff_put(t, " ");
    }
    bool argument = p->kind.tag == TUFF_PLUGIN_TYPES_NODE_KIND_ARGUMENT;
    tuff_put(t, argument ? "<" : "");
    tuff_put(t, p->name);
    tuff_put(t, argument ? ">" : "");
}

static const char *permission_of(tuff_tree_t *tree, int node) {
    for (; node >= 0; node = tree->nodes[node].parent) {
        if (tree->nodes[node].permission) {
            return tree->nodes[node].permission;
        }
    }
    return NULL;
}

static bool run_help(const tuff_call_t *call, int32_t *result, plugin_string_t *err, void *data) {
    help_t *help = data;
    bool console = tuff_sender(call) == NULL;
    int32_t shown = 0;
    for (size_t i = 0; i < help->count; i++) {
        help_line_t *line = &help->lines[i];
        if (line->permission) {
            plugin_string_t node = tuff_str(line->permission);
            bool held;
            if (tuff_plugin_host_has_permission(&node, &held) && !held) {
                continue;
            }
        }
        if (console) {
            tuff_log(TUFF_PLUGIN_TYPES_LOG_LEVEL_INFO, line->text);
        } else if (!tuff_reply(line->text)) {
            return tuff_fail(err, "reply refused");
        }
        shown++;
    }
    *result = shown;
    return true;
}

/* Adds the help subcommand: one line per node with a handler, in node
   order, as "<gold>/path<reset> - about". */
static bool add_help(tuff_tree_t *tree, bool tuff) {
    help_t *help = calloc(1, sizeof *help);
    help->lines = help ? calloc(tree->count, sizeof *help->lines) : NULL;
    if (!help || !help->lines) {
        free(help);
        return false;
    }
    for (size_t i = 0; i < tree->count; i++) {
        pending_t *p = &tree->nodes[i];
        if (!p->handler.plain && !p->handler.with_data) {
            continue;
        }
        tuff_text_t t = {.len = 0};
        tuff_put(&t, TUFF_SECTION "6/");
        tuff_put(&t, tuff ? "tuff " : "");
        put_path(&t, tree, (int)i);
        tuff_put(&t, TUFF_SECTION "r");
        if (p->about) {
            tuff_put(&t, " - ");
            tuff_put(&t, p->about);
        }
        const char *permission = permission_of(tree, (int)i);
        help_line_t *line = &help->lines[help->count++];
        line->text = text_copy(&t);
        line->permission = permission ? copy(permission) : NULL;
    }
    int node = tuff_literal(tree, 0, "help");
    tuff_about(tree, node, "Lists these commands");
    tuff_executes_data(tree, node, run_help, help);
    if (!tree->nodes[0].handler.plain && !tree->nodes[0].handler.with_data) {
        tuff_executes_data(tree, 0, run_help, help);
    }
    return node >= 0;
}

static bool refuse(plugin_string_t *err, const char *why) {
    if (err) {
        plugin_string_dup(err, why);
    }
    return false;
}

static bool grow(void **items, size_t count, size_t extra, size_t size) {
    void *grown = realloc(*items, (count + extra) * size);
    if (!grown) {
        return false;
    }
    *items = grown;
    return true;
}

static bool install(tuff_tree_t *tree, bool tuff, plugin_string_t *err) {
    if (!tree) {
        return refuse(err, "out of memory");
    }
    if (tree->help && !add_help(tree, tuff)) {
        tree->failed = true;
    }
    if (tree->failed) {
        tuff_tree_free(tree);
        return refuse(err, "the tree was built with a bad node");
    }
    for (size_t i = 0; i < tree->count; i++) {
        pending_t *p = &tree->nodes[i];
        if (p->declare) {
            plugin_string_t node = tuff_str(p->permission);
            tuff_plugin_host_register_error_t refusal;
            if (!tuff_plugin_host_declare_permission(&node, p->level, &refusal)) {
                tuff_plugin_host_register_error_free(&refusal);
                tuff_tree_free(tree);
                return refuse(err, "the host refused a permission");
            }
        }
    }
    size_t count = tree->count;
    size_t handled = 0;
    size_t suggesting = 0;
    for (size_t i = 0; i < count; i++) {
        handled += tree->nodes[i].handler.plain || tree->nodes[i].handler.with_data;
        suggesting += tree->nodes[i].suggest != NULL;
    }
    tuff_plugin_types_command_node_t *nodes = calloc(count, sizeof *nodes);
    uint16_t *kids = calloc(count, sizeof *kids);
    if (!nodes || !kids || !grow((void **)&handlers, handler_count, handled, sizeof *handlers) ||
        !grow((void **)&suggesters, suggester_count, suggesting, sizeof *suggesters)) {
        free(nodes);
        free(kids);
        tuff_tree_free(tree);
        return refuse(err, "out of memory");
    }
    size_t next_kid = 0;
    size_t handler_id = handler_count;
    for (size_t i = 0; i < count; i++) {
        pending_t *p = &tree->nodes[i];
        tuff_plugin_types_command_node_t *n = &nodes[i];
        n->name = tuff_str(p->name);
        n->kind = p->kind;
        n->children.ptr = &kids[next_kid];
        for (size_t j = i + 1; j < count; j++) {
            if (tree->nodes[j].parent == (int)i) {
                kids[next_kid++] = (uint16_t)j;
            }
        }
        n->children.len = (size_t)(&kids[next_kid] - n->children.ptr);
        if (p->handler.plain || p->handler.with_data) {
            n->executes.is_some = true;
            n->executes.val = (uint32_t)handler_id;
            handlers[handler_id++] = p->handler;
        }
        if (p->redirect >= 0) {
            n->redirect.is_some = true;
            n->redirect.val = (uint16_t)p->redirect;
        }
        if (p->permission) {
            n->permission.is_some = true;
            n->permission.val = tuff_str(p->permission);
        }
        n->suggest = p->suggest != NULL;
    }
    tuff_plugin_types_command_tree_t wit = {{nodes, count}};
    tuff_plugin_host_register_error_t refusal;
    bool ok = tuff ? tuff_plugin_host_register_tuff_subcommand(&wit, &refusal)
                   : tuff_plugin_host_register_command(&wit, &refusal);
    free(nodes);
    free(kids);
    if (!ok) {
        tuff_plugin_host_register_error_free(&refusal);
        tuff_tree_free(tree);
        return refuse(err, "the host refused the command tree");
    }
    handler_count = handler_id;
    for (size_t i = 0; i < count; i++) {
        pending_t *p = &tree->nodes[i];
        if (p->suggest) {
            suggester_t *s = &suggesters[suggester_count++];
            s->root = copy(tree->nodes[0].name);
            s->node = copy(p->name);
            s->tuff = tuff;
            s->fn = p->suggest;
        }
    }
    tuff_tree_free(tree);
    return true;
}

bool tuff_register_command(tuff_tree_t *tree, plugin_string_t *err) {
    return install(tree, false, err);
}

bool tuff_register_tuff_subcommand(tuff_tree_t *tree, plugin_string_t *err) {
    return install(tree, true, err);
}

static tuff_parser_t parser(uint8_t tag) {
    tuff_parser_t p;
    memset(&p, 0, sizeof p);
    p.tag = tag;
    return p;
}

tuff_parser_t tuff_parser_boolean(void) {
    return parser(TUFF_PLUGIN_TYPES_PARSER_BOOLEAN);
}

tuff_parser_t tuff_parser_integer(void) {
    return parser(TUFF_PLUGIN_TYPES_PARSER_INTEGER);
}

tuff_parser_t tuff_parser_integer_in(int32_t min, int32_t max) {
    tuff_parser_t p = parser(TUFF_PLUGIN_TYPES_PARSER_INTEGER);
    p.val.integer.min.is_some = true;
    p.val.integer.min.val = min;
    p.val.integer.max.is_some = true;
    p.val.integer.max.val = max;
    return p;
}

tuff_parser_t tuff_parser_double(void) {
    return parser(TUFF_PLUGIN_TYPES_PARSER_DOUBLE);
}

tuff_parser_t tuff_parser_double_in(double min, double max) {
    tuff_parser_t p = parser(TUFF_PLUGIN_TYPES_PARSER_DOUBLE);
    p.val.double_.min.is_some = true;
    p.val.double_.min.val = min;
    p.val.double_.max.is_some = true;
    p.val.double_.max.val = max;
    return p;
}

static tuff_parser_t text(uint8_t kind) {
    tuff_parser_t p = parser(TUFF_PLUGIN_TYPES_PARSER_TEXT);
    p.val.text = kind;
    return p;
}

tuff_parser_t tuff_parser_word(void) {
    return text(TUFF_PLUGIN_TYPES_TEXT_KIND_WORD);
}

tuff_parser_t tuff_parser_phrase(void) {
    return text(TUFF_PLUGIN_TYPES_TEXT_KIND_PHRASE);
}

tuff_parser_t tuff_parser_greedy(void) {
    return text(TUFF_PLUGIN_TYPES_TEXT_KIND_GREEDY);
}

tuff_parser_t tuff_parser_block_pos(void) {
    return parser(TUFF_PLUGIN_TYPES_PARSER_BLOCK_POS);
}

tuff_parser_t tuff_parser_vec3(void) {
    return parser(TUFF_PLUGIN_TYPES_PARSER_VEC3);
}

tuff_parser_t tuff_parser_player(void) {
    return parser(TUFF_PLUGIN_TYPES_PARSER_PLAYER);
}

tuff_parser_t tuff_parser_players(void) {
    return parser(TUFF_PLUGIN_TYPES_PARSER_PLAYERS);
}

tuff_parser_t tuff_parser_block_state(void) {
    return parser(TUFF_PLUGIN_TYPES_PARSER_BLOCK_STATE);
}

tuff_parser_t tuff_parser_item(void) {
    return parser(TUFF_PLUGIN_TYPES_PARSER_ITEM);
}

tuff_parser_t tuff_parser_message(void) {
    return parser(TUFF_PLUGIN_TYPES_PARSER_MESSAGE);
}

/* ---- invocations ---- */

bool tuff_fail(plugin_string_t *err, const char *message) {
    plugin_string_dup(err, message);
    return false;
}

const tuff_player_t *tuff_sender(const tuff_call_t *call) {
    return call->sender.is_some ? &call->sender.val : NULL;
}

static const tuff_value_t *find(const tuff_plugin_types_list_arg_t *args, const char *name) {
    size_t n = strlen(name);
    for (size_t i = 0; i < args->len; i++) {
        if (same(args->ptr[i].name, name, n)) {
            return &args->ptr[i].value;
        }
    }
    return NULL;
}

const tuff_value_t *tuff_arg(const tuff_call_t *call, const char *name) {
    return find(&call->args, name);
}

bool tuff_arg_bool(const tuff_call_t *call, const char *name, bool *out) {
    const tuff_value_t *v = tuff_arg(call, name);
    if (!v || v->tag != TUFF_PLUGIN_TYPES_ARG_VALUE_BOOLEAN) {
        return false;
    }
    *out = v->val.boolean;
    return true;
}

bool tuff_arg_int(const tuff_call_t *call, const char *name, int32_t *out) {
    const tuff_value_t *v = tuff_arg(call, name);
    if (!v || v->tag != TUFF_PLUGIN_TYPES_ARG_VALUE_INTEGER) {
        return false;
    }
    *out = v->val.integer;
    return true;
}

bool tuff_arg_double(const tuff_call_t *call, const char *name, double *out) {
    const tuff_value_t *v = tuff_arg(call, name);
    if (v && v->tag == TUFF_PLUGIN_TYPES_ARG_VALUE_DOUBLE) {
        *out = v->val.double_;
        return true;
    }
    if (v && v->tag == TUFF_PLUGIN_TYPES_ARG_VALUE_INTEGER) {
        *out = (double)v->val.integer;
        return true;
    }
    return false;
}

bool tuff_arg_text(const tuff_call_t *call, const char *name, plugin_string_t *out) {
    const tuff_value_t *v = tuff_arg(call, name);
    if (!v) {
        return false;
    }
    switch (v->tag) {
    case TUFF_PLUGIN_TYPES_ARG_VALUE_TEXT:
        *out = v->val.text;
        return true;
    case TUFF_PLUGIN_TYPES_ARG_VALUE_BLOCK_STATE:
        *out = v->val.block_state;
        return true;
    case TUFF_PLUGIN_TYPES_ARG_VALUE_ITEM:
        *out = v->val.item;
        return true;
    case TUFF_PLUGIN_TYPES_ARG_VALUE_MESSAGE:
        *out = v->val.message;
        return true;
    default:
        return false;
    }
}

bool tuff_arg_block_pos(const tuff_call_t *call, const char *name, tuff_block_pos_t *out) {
    const tuff_value_t *v = tuff_arg(call, name);
    if (!v || v->tag != TUFF_PLUGIN_TYPES_ARG_VALUE_BLOCK_POS) {
        return false;
    }
    *out = v->val.block_pos;
    return true;
}

bool tuff_arg_vec3(const tuff_call_t *call, const char *name, tuff_vec3_t *out) {
    const tuff_value_t *v = tuff_arg(call, name);
    if (!v || v->tag != TUFF_PLUGIN_TYPES_ARG_VALUE_VEC3) {
        return false;
    }
    *out = v->val.vec3;
    return true;
}

const tuff_player_t *tuff_arg_player(const tuff_call_t *call, const char *name) {
    const tuff_value_t *v = tuff_arg(call, name);
    return v && v->tag == TUFF_PLUGIN_TYPES_ARG_VALUE_PLAYER ? &v->val.player : NULL;
}

/* ---- suggestions ---- */

struct tuff_suggestions {
    plugin_string_t partial;
    plugin_list_string_t *out;
};

static uint8_t lower(uint8_t c) {
    return c >= 'A' && c <= 'Z' ? (uint8_t)(c + 32) : c;
}

void tuff_suggest_add(tuff_suggestions_t *out, const char *text) {
    size_t n = strlen(text);
    if (n < out->partial.len) {
        return;
    }
    for (size_t i = 0; i < out->partial.len; i++) {
        if (lower((uint8_t)text[i]) != lower(out->partial.ptr[i])) {
            return;
        }
    }
    plugin_list_string_t *list = out->out;
    if (!grow((void **)&list->ptr, list->len, 1, sizeof *list->ptr)) {
        return;
    }
    plugin_string_dup_n(&list->ptr[list->len++], text, n);
}

static plugin_string_t word(plugin_string_t line, size_t *at) {
    while (*at < line.len && line.ptr[*at] == ' ') {
        (*at)++;
    }
    size_t start = *at;
    while (*at < line.len && line.ptr[*at] != ' ') {
        (*at)++;
    }
    plugin_string_t w = {line.ptr + start, *at - start};
    return w;
}

/* ---- dispatch ---- */

static uint64_t callback;
static bool call_taken;

void tuff_enter(void) {
    callback++;
}

uint64_t tuff_callback(void) {
    return callback;
}

tuff_call_t *tuff_take_call(const tuff_call_t *call) {
    tuff_call_t *owned = malloc(sizeof *owned);
    if (owned) {
        *owned = *call;
        call_taken = true;
    }
    return owned;
}

void tuff_call_free(tuff_call_t *call) {
    if (call) {
        exports_tuff_plugin_guest_invocation_free(call);
        free(call);
    }
}

bool tuff_run_command(tuff_call_t *call, int32_t *ret, plugin_string_t *err) {
    bool ok;
    call_taken = false;
    if (call->handler < handler_count) {
        handler_t *h = &handlers[call->handler];
        ok = h->plain ? h->plain(call, ret, err) : h->with_data(call, ret, err, h->data);
    } else {
        ok = tuff_fail(err, "unknown handler");
    }
    if (!call_taken) {
        exports_tuff_plugin_guest_invocation_free(call);
    }
    call_taken = false;
    return ok;
}

void tuff_suggest(tuff_suggest_request_t *req, plugin_list_string_t *ret) {
    ret->ptr = NULL;
    ret->len = 0;
    size_t at = 0;
    plugin_string_t first = word(req->line, &at);
    plugin_string_t second = word(req->line, &at);
    suggester_t *chosen = NULL;
    for (size_t i = 0; i < suggester_count; i++) {
        suggester_t *s = &suggesters[i];
        if (!tuff_str_eq(req->node, s->node)) {
            continue;
        }
        bool matches = s->tuff ? tuff_str_eq(first, "tuff") && tuff_str_eq(second, s->root)
                               : tuff_str_eq(first, s->root);
        if (matches || !chosen) {
            chosen = s;
        }
        if (matches) {
            break;
        }
    }
    if (chosen) {
        tuff_suggestions_t out = {req->partial, ret};
        chosen->fn(req, &out);
    }
    exports_tuff_plugin_guest_suggest_request_free(req);
}

/* ---- default exports ---- */

WEAK bool exports_tuff_plugin_guest_register(plugin_string_t *err) {
    (void)err;
    return true;
}

WEAK void exports_tuff_plugin_guest_enable(void) {}

WEAK void exports_tuff_plugin_guest_disable(void) {}

WEAK bool exports_tuff_plugin_guest_run_command(exports_tuff_plugin_guest_invocation_t *call,
                                                int32_t *ret, plugin_string_t *err) {
    tuff_enter();
    return tuff_run_command(call, ret, err);
}

WEAK void exports_tuff_plugin_guest_suggest(exports_tuff_plugin_guest_suggest_request_t *req,
                                            plugin_list_string_t *ret) {
    tuff_enter();
    tuff_suggest(req, ret);
}

WEAK void exports_tuff_plugin_guest_on_event(exports_tuff_plugin_guest_event_t *ev) {
    exports_tuff_plugin_guest_event_free(ev);
}

WEAK void exports_tuff_plugin_guest_on_timer(uint32_t token) {
    (void)token;
}

WEAK void exports_tuff_plugin_guest_on_message(exports_tuff_plugin_guest_incoming_t *msg) {
    exports_tuff_plugin_guest_incoming_free(msg);
}

WEAK void exports_tuff_plugin_capability_events_on_io(
    exports_tuff_plugin_capability_events_io_event_t *event) {
    exports_tuff_plugin_capability_events_io_event_free(event);
}

WEAK void exports_tuff_plugin_worldgen_declare_dimensions(
    exports_tuff_plugin_worldgen_list_dimension_decl_t *ret) {
    ret->ptr = NULL;
    ret->len = 0;
}

WEAK bool exports_tuff_plugin_worldgen_generate_chunk(
    plugin_string_t *dimension, int64_t seed, int32_t chunk_x, int32_t chunk_z, int32_t min_y,
    int32_t height, exports_tuff_plugin_worldgen_generated_chunk_t *ret, plugin_string_t *err) {
    (void)seed;
    (void)chunk_x;
    (void)chunk_z;
    (void)min_y;
    (void)height;
    (void)ret;
    plugin_string_free(dimension);
    plugin_string_dup(err, "the plugin has no chunk generator");
    return false;
}

WEAK void exports_tuff_plugin_content_declare_content(
    exports_tuff_plugin_content_content_decl_t *ret) {
    memset(ret, 0, sizeof *ret);
}

WEAK bool exports_tuff_plugin_background_tasks_run_task(plugin_string_t *task,
                                                         plugin_list_u8_t *input,
                                                         plugin_list_u8_t *ret,
                                                         plugin_string_t *err) {
    (void)ret;
    plugin_string_free(task);
    plugin_list_u8_free(input);
    plugin_string_dup(err, "the plugin has no background tasks");
    return false;
}
