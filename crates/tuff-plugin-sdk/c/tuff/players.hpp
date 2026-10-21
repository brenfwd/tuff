// SPDX-License-Identifier: MIT
// Menus, block rules, teams, players and runtime dimensions over the host's
// calls, like the Rust SDK's menu, rules, players and dimensions modules.
//
//     tuff::Menu menu("shop", 3, "Shop");
//     menu.item(13, "minecraft:diamond", 1).name("Buy");
//     menu.open(player.uuid);
//     tuff::on<tuff::ev::MenuClick>([](const auto &c) { ... });
//
//     for (const tuff::PlayerInfo &p : tuff::online::all()) { ... }
//     tuff::online::set_game_mode(uuid, tuff::GameMode::Creative);
#ifndef TUFF_PLAYERS_HPP
#define TUFF_PLAYERS_HPP

namespace tuff {

enum class MenuError : uint8_t {
    Invalid = TUFF_PLUGIN_TYPES_MENU_ERROR_INVALID,
    Offline = TUFF_PLUGIN_TYPES_MENU_ERROR_OFFLINE,
    OverBudget = TUFF_PLUGIN_TYPES_MENU_ERROR_OVER_BUDGET,
    NotGranted = TUFF_PLUGIN_TYPES_MENU_ERROR_NOT_GRANTED,
};

enum class GameMode : uint8_t {
    Survival = TUFF_PLUGIN_TYPES_GAME_MODE_SURVIVAL,
    Creative = TUFF_PLUGIN_TYPES_GAME_MODE_CREATIVE,
    Adventure = TUFF_PLUGIN_TYPES_GAME_MODE_ADVENTURE,
    Spectator = TUFF_PLUGIN_TYPES_GAME_MODE_SPECTATOR,
};

enum class DimensionError : uint8_t {
    UnknownDimension = TUFF_PLUGIN_TYPES_WORLD_ERROR_UNKNOWN_DIMENSION,
    OutsideWorld = TUFF_PLUGIN_TYPES_WORLD_ERROR_OUTSIDE_WORLD,
    InvalidState = TUFF_PLUGIN_TYPES_WORLD_ERROR_INVALID_STATE,
    OverBudget = TUFF_PLUGIN_TYPES_WORLD_ERROR_OVER_BUDGET,
    Unavailable = TUFF_PLUGIN_TYPES_WORLD_ERROR_UNAVAILABLE,
    NotGranted = TUFF_PLUGIN_TYPES_WORLD_ERROR_NOT_GRANTED,
};

using ChunkArea = raw::tuff_plugin_types_chunk_area_t;

// An item stack in a menu slot.
class MenuItem {
  public:
    MenuItem(uint8_t slot, Str item, uint8_t count)
        : slot_(slot), item_(String::copy(item)), count_(count) {}
    MenuItem(uint8_t slot, const char *item, uint8_t count)
        : MenuItem(slot, raw::tuff_str(item), count) {}

    // Plain text shown without the default italics.
    MenuItem &name(Str text) {
        name_ = Option<String>(String::copy(text));
        return *this;
    }
    MenuItem &name(const char *text) { return name(raw::tuff_str(text)); }
    MenuItem &lore(Str line) {
        lore_.push(String::copy(line));
        return *this;
    }
    MenuItem &lore(const char *line) { return lore(raw::tuff_str(line)); }
    // Item components as SNBT.
    MenuItem &components(Str snbt) {
        components_ = Option<String>(String::copy(snbt));
        return *this;
    }

    uint8_t slot() const { return slot_; }

    // A raw item borrowing this one; valid while this item and `lore` live.
    raw::tuff_plugin_host_menu_item_t to_raw(detail::Vec<Str> &lore) const {
        raw::tuff_plugin_host_menu_item_t out = {};
        out.slot = slot_;
        out.item = item_.view();
        out.count = count_;
        if (name_) {
            out.name.is_some = true;
            out.name.val = name_->view();
        }
        for (const String &line : lore_) {
            lore.push(line.view());
        }
        out.lore.ptr = lore.begin();
        out.lore.len = lore.size();
        if (components_) {
            out.components.is_some = true;
            out.components.val = components_->view();
        }
        return out;
    }

  private:
    uint8_t slot_;
    String item_;
    uint8_t count_;
    Option<String> name_;
    detail::Vec<String> lore_;
    Option<String> components_;
};

namespace detail {

inline Outcome<Unit, MenuError> menu_unit(bool ok, raw::tuff_plugin_host_menu_error_t e) {
    return ok ? Outcome<Unit, MenuError>(Unit{}) : Outcome<Unit, MenuError>(static_cast<MenuError>(e));
}

} // namespace detail

// A chest-style menu of 1 to 6 rows, shown to a player with open().
class Menu {
  public:
    Menu(Str id, uint8_t rows, Str title)
        : id_(String::copy(id)), rows_(rows), title_(String::copy(title)) {}
    Menu(const char *id, uint8_t rows, const char *title)
        : Menu(raw::tuff_str(id), rows, raw::tuff_str(title)) {}

    MenuItem &item(uint8_t slot, Str item, uint8_t count) {
        items_.push(MenuItem(slot, item, count));
        return items_[items_.size() - 1];
    }
    MenuItem &item(uint8_t slot, const char *item_id, uint8_t count) {
        return item(slot, raw::tuff_str(item_id), count);
    }

    Outcome<Unit, MenuError> open(Uuid player) const {
        detail::Vec<detail::Vec<Str>> lores;
        detail::Vec<raw::tuff_plugin_host_menu_item_t> raws;
        for (size_t i = 0; i < items_.size(); i++) {
            lores.push(detail::Vec<Str>());
        }
        size_t at = 0;
        for (const MenuItem &it : items_) {
            raws.push(it.to_raw(lores[at++]));
        }
        raw::tuff_plugin_host_list_menu_item_t list = {raws.begin(), raws.size()};
        raw::tuff_plugin_host_menu_error_t e = 0;
        Str id = id_.view();
        Str title = title_.view();
        return detail::menu_unit(raw::tuff_plugin_host_open_menu(&player, &id, rows_, &title, &list, &e), e);
    }

  private:
    String id_;
    uint8_t rows_;
    String title_;
    detail::Vec<MenuItem> items_;
};

namespace menu {

// Sets one slot of the menu the player has open.
inline Outcome<Unit, MenuError> update_slot(Uuid player, MenuItem &item) {
    detail::Vec<Str> lore;
    raw::tuff_plugin_host_menu_item_t raw_item = item.to_raw(lore);
    raw::tuff_plugin_host_menu_error_t e = 0;
    return detail::menu_unit(
        raw::tuff_plugin_host_update_menu_slot(&player, item.slot(), &raw_item, &e), e);
}

inline Outcome<Unit, MenuError> clear_slot(Uuid player, uint8_t slot) {
    raw::tuff_plugin_host_menu_error_t e = 0;
    return detail::menu_unit(
        raw::tuff_plugin_host_update_menu_slot(&player, slot, nullptr, &e), e);
}

inline void close(Uuid player) { raw::tuff_plugin_host_close_menu(&player); }

} // namespace menu

// Which actions a box of blocks allows; build with the setters, then add().
class BlockRule {
  public:
    BlockRule(Str dimension, BlockPos min, BlockPos max)
        : dimension_(String::copy(dimension)), min_(min), max_(max) {}
    BlockRule(const char *dimension, BlockPos min, BlockPos max)
        : BlockRule(raw::tuff_str(dimension), min, max) {}

    BlockRule &place(bool allowed) {
        place_ = allowed;
        return *this;
    }
    BlockRule &break_placed(bool allowed) {
        break_placed_ = allowed;
        return *this;
    }
    BlockRule &explode_placed(bool allowed) {
        explode_placed_ = allowed;
        return *this;
    }
    // Cancels every explosion centred inside the box whole.
    BlockRule &cancel_explosions(bool cancelled) {
        cancel_explosions_ = cancelled;
        return *this;
    }
    // A block id or tag that may be broken even if not placed.
    BlockRule &breakable(Str block) {
        breakable_.push(String::copy(block));
        return *this;
    }
    BlockRule &breakable(const char *block) { return breakable(raw::tuff_str(block)); }
    // A block id or tag that explosions leave alone.
    BlockRule &blast_proof(Str block) {
        blast_proof_.push(String::copy(block));
        return *this;
    }
    BlockRule &blast_proof(const char *block) { return blast_proof(raw::tuff_str(block)); }

    // The rule's id, or the host's refusal text.
    Outcome<uint32_t, String> add() const {
        detail::Vec<Str> breakable;
        detail::Vec<Str> blast;
        for (const String &s : breakable_) {
            breakable.push(s.view());
        }
        for (const String &s : blast_proof_) {
            blast.push(s.view());
        }
        raw::tuff_plugin_host_block_rule_t rule = {};
        rule.dimension = dimension_.view();
        rule.min = min_;
        rule.max = max_;
        rule.place = place_;
        rule.break_placed = break_placed_;
        rule.breakable = {breakable.begin(), breakable.size()};
        rule.explode_placed = explode_placed_;
        rule.blast_proof = {blast.begin(), blast.size()};
        rule.cancel_explosions = cancel_explosions_;
        uint32_t id = 0;
        HostError err = {};
        if (!raw::tuff_plugin_host_add_block_rule(&rule, &id, &err)) {
            return Outcome<uint32_t, String>(host_error_text(err));
        }
        return Outcome<uint32_t, String>(id);
    }

  private:
    String dimension_;
    BlockPos min_;
    BlockPos max_;
    bool place_ = false;
    bool break_placed_ = false;
    bool explode_placed_ = false;
    bool cancel_explosions_ = false;
    detail::Vec<String> breakable_;
    detail::Vec<String> blast_proof_;
};

// Which changes no player makes (fire, liquids, entities) a box allows; all
// are stopped until allowed with the setters, then add().
class ChangeRule {
  public:
    ChangeRule(Str dimension, BlockPos min, BlockPos max)
        : dimension_(String::copy(dimension)), min_(min), max_(max) {}
    ChangeRule(const char *dimension, BlockPos min, BlockPos max)
        : ChangeRule(raw::tuff_str(dimension), min, max) {}

    ChangeRule &fire_spread(bool allowed) {
        fire_spread_ = allowed;
        return *this;
    }
    ChangeRule &fire_burn(bool allowed) {
        fire_burn_ = allowed;
        return *this;
    }
    ChangeRule &liquid_flow(bool allowed) {
        liquid_flow_ = allowed;
        return *this;
    }
    ChangeRule &entity_grief(bool allowed) {
        entity_grief_ = allowed;
        return *this;
    }

    // The rule's id (rules::remove takes it), or the host's refusal text.
    Outcome<uint32_t, String> add() const {
        raw::tuff_plugin_host_change_rule_t rule = {};
        rule.dimension = dimension_.view();
        rule.min = min_;
        rule.max = max_;
        rule.fire_spread = fire_spread_;
        rule.fire_burn = fire_burn_;
        rule.liquid_flow = liquid_flow_;
        rule.entity_grief = entity_grief_;
        uint32_t id = 0;
        HostError err = {};
        if (!raw::tuff_plugin_host_add_change_rule(&rule, &id, &err)) {
            return Outcome<uint32_t, String>(host_error_text(err));
        }
        return Outcome<uint32_t, String>(id);
    }

  private:
    String dimension_;
    BlockPos min_;
    BlockPos max_;
    bool fire_spread_ = false;
    bool fire_burn_ = false;
    bool liquid_flow_ = false;
    bool entity_grief_ = false;
};

namespace rules {

// False when `id` is not one of this plugin's rules.
inline bool remove(uint32_t id) { return raw::tuff_plugin_host_remove_block_rule(id); }

// Puts a player in one of this plugin's teams; teammates cannot hurt each
// other. Gives the host's refusal text on failure.
inline Outcome<Unit, String> set_team(Uuid player, Str team) {
    HostError err = {};
    if (!raw::tuff_plugin_host_set_team(&player, &team, &err)) {
        return Outcome<Unit, String>(host_error_text(err));
    }
    return Outcome<Unit, String>(Unit{});
}
inline Outcome<Unit, String> set_team(Uuid player, const char *team) {
    return set_team(player, raw::tuff_str(team));
}
inline Outcome<Unit, String> clear_team(Uuid player) {
    HostError err = {};
    if (!raw::tuff_plugin_host_set_team(&player, nullptr, &err)) {
        return Outcome<Unit, String>(host_error_text(err));
    }
    return Outcome<Unit, String>(Unit{});
}

} // namespace rules

// What hurt the player of the player-hurt or player-death event.
struct DamageInfo {
    Option<Uuid> attacker;
    Option<String> attacker_kind;
    Option<String> projectile;
    Option<Uuid> credit;
    bool credit_is_player = false;
};

// What a projectile of the projectile-hit event struck.
struct HitInfo {
    Option<Uuid> owner;
    Option<BlockPos> block;
    Option<Uuid> target;
};

namespace detail {

inline Option<Uuid> uuid_of(const raw::tuff_plugin_types_option_uuid_t &o) {
    return o.is_some ? Option<Uuid>(o.val) : Option<Uuid>();
}

inline Option<String> string_of(const raw::plugin_option_string_t &o) {
    return o.is_some ? Option<String>(String::copy(o.val)) : Option<String>();
}

} // namespace detail

namespace acting {

inline Option<DamageInfo> damage() {
    raw::tuff_plugin_host_damage_source_t d = {};
    if (!raw::tuff_plugin_host_acting_damage(&d)) {
        return {};
    }
    DamageInfo out;
    out.attacker = detail::uuid_of(d.attacker);
    out.attacker_kind = detail::string_of(d.attacker_kind);
    out.projectile = detail::string_of(d.projectile);
    out.credit = detail::uuid_of(d.credit);
    out.credit_is_player = d.credit_is_player;
    raw::tuff_plugin_host_damage_source_free(&d);
    return Option<DamageInfo>(move(out));
}

inline Option<HitInfo> hit() {
    raw::tuff_plugin_host_hit_t h = {};
    if (!raw::tuff_plugin_host_acting_hit(&h)) {
        return {};
    }
    HitInfo out;
    out.owner = detail::uuid_of(h.owner);
    if (h.block.is_some) {
        out.block = Option<BlockPos>(h.block.val);
    }
    out.target = detail::uuid_of(h.target);
    return Option<HitInfo>(move(out));
}

// Whether the acting player's inventory is known and holds `item`.
inline Option<bool> holds(Str item) {
    raw::tuff_plugin_host_inventory_t inv = {};
    if (!raw::tuff_plugin_host_acting_inventory(&inv)) {
        return {};
    }
    bool found = inv.held.is_some && inv.held.val.item == item;
    for (size_t i = 0; i < inv.slots.len && !found; i++) {
        found = inv.slots.ptr[i].stack.item == item;
    }
    raw::tuff_plugin_host_inventory_free(&inv);
    return Option<bool>(found);
}
inline Option<bool> holds(const char *item) { return holds(raw::tuff_str(item)); }

} // namespace acting

struct PlayerInfo {
    Uuid uuid;
    String name;
};

struct Effect {
    String id;
    uint8_t amplifier;
};

struct PlayerState {
    String dimension;
    Vec3 pos;
    GameMode game_mode;
    bool alive;
    float health;
    int32_t food;
    int32_t xp_level;
    int32_t xp_points;
    detail::Vec<Effect> effects;
};

namespace online {

inline detail::Vec<PlayerInfo> all() {
    raw::tuff_plugin_host_list_player_t list = {};
    raw::tuff_plugin_host_online_players(&list);
    detail::Vec<PlayerInfo> out;
    for (size_t i = 0; i < list.len; i++) {
        out.push(PlayerInfo{list.ptr[i].uuid, String::copy(list.ptr[i].name)});
    }
    raw::tuff_plugin_host_list_player_free(&list);
    return out;
}

// None when the player is not online.
inline Option<PlayerState> state(Uuid player) {
    raw::tuff_plugin_host_player_state_t s = {};
    if (!raw::tuff_plugin_host_player_info(&player, &s)) {
        return {};
    }
    PlayerState out{String::copy(s.location.dimension),
                    s.location.pos,
                    static_cast<GameMode>(s.game_mode),
                    s.alive,
                    s.health,
                    s.food,
                    s.xp_level,
                    s.xp_points,
                    {}};
    for (size_t i = 0; i < s.effects.len; i++) {
        out.effects.push(Effect{String::copy(s.effects.ptr[i].effect), s.effects.ptr[i].amplifier});
    }
    raw::tuff_plugin_host_player_state_free(&s);
    return Option<PlayerState>(move(out));
}

// The player's dimension and position; none when they are not online.
inline Option<World> world_of(Uuid player) {
    raw::tuff_plugin_host_location_t loc;
    if (!raw::tuff_plugin_host_player_location(&player, &loc)) {
        return {};
    }
    World w(loc.dimension);
    raw::tuff_plugin_host_location_free(&loc);
    return Option<World>(move(w));
}

inline Option<Vec3> position(Uuid player) {
    raw::tuff_plugin_host_location_t loc;
    if (!raw::tuff_plugin_host_player_location(&player, &loc)) {
        return {};
    }
    Vec3 pos = loc.pos;
    raw::tuff_plugin_host_location_free(&loc);
    return Option<Vec3>(pos);
}

inline WorldOutcome<Unit> set_game_mode(Uuid player, GameMode mode) {
    raw::tuff_plugin_host_world_error_t e = {};
    return detail::unit(
        raw::tuff_plugin_host_set_game_mode(&player, static_cast<uint8_t>(mode), &e), e);
}

// Health and food are optional; pass a negative value to leave one as is.
inline WorldOutcome<Unit> set_vitals(Uuid player, float health, int32_t food) {
    raw::tuff_plugin_host_world_error_t e = {};
    float *h = health >= 0 ? &health : nullptr;
    int32_t *f = food >= 0 ? &food : nullptr;
    return detail::unit(raw::tuff_plugin_host_set_vitals(&player, h, f, &e), e);
}

inline WorldOutcome<Unit> set_xp(Uuid player, int32_t level, int32_t points) {
    raw::tuff_plugin_host_world_error_t e = {};
    return detail::unit(raw::tuff_plugin_host_set_xp(&player, level, points, &e), e);
}

inline WorldOutcome<Unit> add_effect(Uuid player, Str effect, int32_t ticks, uint8_t amplifier = 0,
                                     bool particles = true) {
    raw::tuff_plugin_host_world_error_t e = {};
    return detail::unit(
        raw::tuff_plugin_host_add_effect(&player, &effect, ticks, amplifier, particles, &e), e);
}
inline WorldOutcome<Unit> add_effect(Uuid player, const char *effect, int32_t ticks,
                                     uint8_t amplifier = 0, bool particles = true) {
    return add_effect(player, raw::tuff_str(effect), ticks, amplifier, particles);
}

inline WorldOutcome<Unit> clear_effects(Uuid player) {
    raw::tuff_plugin_host_world_error_t e = {};
    return detail::unit(raw::tuff_plugin_host_clear_effects(&player, nullptr, &e), e);
}
inline WorldOutcome<Unit> clear_effect(Uuid player, Str effect) {
    raw::tuff_plugin_host_world_error_t e = {};
    return detail::unit(raw::tuff_plugin_host_clear_effects(&player, &effect, &e), e);
}

inline WorldOutcome<Unit> kill(Uuid player) {
    raw::tuff_plugin_host_world_error_t e = {};
    return detail::unit(raw::tuff_plugin_host_kill_player(&player, &e), e);
}

inline WorldOutcome<Unit> respawn(Uuid player) {
    raw::tuff_plugin_host_world_error_t e = {};
    return detail::unit(raw::tuff_plugin_host_respawn_player(&player, &e), e);
}

inline WorldOutcome<Unit> set_respawn_point(Uuid player, const World &world, BlockPos pos) {
    raw::tuff_plugin_host_world_error_t e = {};
    Str dim = world.key();
    return detail::unit(
        raw::tuff_plugin_host_set_respawn_point(&player, &dim, &pos, nullptr, &e), e);
}

inline WorldOutcome<Unit> clear_inventory(Uuid player) {
    raw::tuff_plugin_host_world_error_t e = {};
    return detail::unit(raw::tuff_plugin_host_clear_inventory(&player, &e), e);
}

// Replaces one slot in menu numbering with a stack given as SNBT.
inline WorldOutcome<Unit> set_slot(Uuid player, uint8_t slot, Str stack_snbt) {
    raw::tuff_plugin_host_world_error_t e = {};
    return detail::unit(raw::tuff_plugin_host_set_slot(&player, slot, &stack_snbt, &e), e);
}
inline WorldOutcome<Unit> clear_slot(Uuid player, uint8_t slot) {
    raw::tuff_plugin_host_world_error_t e = {};
    return detail::unit(raw::tuff_plugin_host_set_slot(&player, slot, nullptr, &e), e);
}

} // namespace online

namespace dimensions {

inline Outcome<Unit, DimensionError> from(bool ok, raw::tuff_plugin_host_dimension_error_t e) {
    return ok ? Outcome<Unit, DimensionError>(Unit{})
              : Outcome<Unit, DimensionError>(static_cast<DimensionError>(e.tag));
}

// Makes dimension `key` of the dimension type of `base`, empty.
inline Outcome<Unit, DimensionError> create(Str key, Str base) {
    raw::tuff_plugin_host_dimension_error_t e = {};
    return from(raw::tuff_plugin_host_create_dimension(&key, &base, nullptr, &e), e);
}
inline Outcome<Unit, DimensionError> create(const char *key, const char *base) {
    return create(raw::tuff_str(key), raw::tuff_str(base));
}
// Makes dimension `key` as a copy of the chunks of `area` in `base`.
inline Outcome<Unit, DimensionError> create_copy(Str key, Str base, ChunkArea area) {
    raw::tuff_plugin_host_dimension_error_t e = {};
    return from(raw::tuff_plugin_host_create_dimension(&key, &base, &area, &e), e);
}

inline Outcome<Unit, DimensionError> discard(Str key) {
    raw::tuff_plugin_host_dimension_error_t e = {};
    return from(raw::tuff_plugin_host_discard_dimension(&key, &e), e);
}
inline Outcome<Unit, DimensionError> discard(const char *key) { return discard(raw::tuff_str(key)); }

// The keys of the dimensions this plugin made and has not discarded.
inline detail::Vec<String> list() {
    raw::plugin_list_string_t keys = {};
    raw::tuff_plugin_host_runtime_dimensions(&keys);
    detail::Vec<String> out;
    for (size_t i = 0; i < keys.len; i++) {
        out.push(String::copy(keys.ptr[i]));
    }
    raw::plugin_list_string_free(&keys);
    return out;
}

} // namespace dimensions

} // namespace tuff

#endif
