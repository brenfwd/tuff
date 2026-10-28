// SPDX-License-Identifier: MIT
// Fluent handles over the host's world calls, like the Rust SDK's world
// module: a World per dimension, EntityRef for a loaded entity, acting::
// for whoever the current callback acts for.
//
//     tuff::World overworld = tuff::World::overworld();
//     auto block = overworld.block({0, 64, 0});
//     if (block && *block == "minecraft:air") {
//         overworld.set_block({0, 64, 0}, "minecraft:stone");
//     }
//     tuff::acting::give("minecraft:bread", 3);
//
// Every call gives an Outcome with the host's WorldError on failure. World
// reads and writes are bounded per call by the host.
#ifndef TUFF_WORLD_HPP
#define TUFF_WORLD_HPP

namespace tuff {

using Rotation = raw::tuff_plugin_types_rotation_t;

enum class WorldError : uint8_t {
    UnknownDimension = TUFF_PLUGIN_TYPES_WORLD_ERROR_UNKNOWN_DIMENSION,
    OutsideWorld = TUFF_PLUGIN_TYPES_WORLD_ERROR_OUTSIDE_WORLD,
    InvalidState = TUFF_PLUGIN_TYPES_WORLD_ERROR_INVALID_STATE,
    OverBudget = TUFF_PLUGIN_TYPES_WORLD_ERROR_OVER_BUDGET,
    Unavailable = TUFF_PLUGIN_TYPES_WORLD_ERROR_UNAVAILABLE,
};

template <class T> using WorldOutcome = Outcome<T, WorldError>;

namespace detail {

inline WorldError world_error(const raw::tuff_plugin_host_world_error_t &e) {
    return static_cast<WorldError>(e.tag);
}

inline WorldOutcome<Unit> unit(bool ok, const raw::tuff_plugin_host_world_error_t &e) {
    return ok ? WorldOutcome<Unit>(Unit{}) : WorldOutcome<Unit>(world_error(e));
}

} // namespace detail

// A loaded non-player entity, by uuid.
struct EntityRef {
    Uuid id;

    Uuid uuid() const { return id; }

    WorldOutcome<Unit> remove() const {
        raw::tuff_plugin_host_world_error_t e = {};
        Uuid u = id;
        return detail::unit(raw::tuff_plugin_host_remove_entity(&u, &e), e);
    }

    // The entity's data as SNBT; none when it is gone.
    WorldOutcome<Option<String>> data() const {
        raw::tuff_plugin_host_world_error_t e = {};
        raw::plugin_option_string_t out = {};
        Uuid u = id;
        if (!raw::tuff_plugin_host_entity_data(&u, &out, &e)) {
            return WorldOutcome<Option<String>>(detail::world_error(e));
        }
        Option<String> value;
        if (out.is_some) {
            value = Option<String>(String::adopt(out.val));
        }
        return WorldOutcome<Option<String>>(move(value));
    }

    // Merges SNBT into the entity's data.
    WorldOutcome<Unit> merge(Str snbt) const {
        raw::tuff_plugin_host_world_error_t e = {};
        Uuid u = id;
        return detail::unit(raw::tuff_plugin_host_entity_merge(&u, &snbt, &e), e);
    }
    WorldOutcome<Unit> merge(const char *snbt) const { return merge(raw::tuff_str(snbt)); }

    WorldOutcome<Unit> rename(Str name) const {
        raw::tuff_plugin_host_world_error_t e = {};
        Uuid u = id;
        return detail::unit(raw::tuff_plugin_host_entity_set_name(&u, &name, &e), e);
    }
    WorldOutcome<Unit> rename(const char *name) const { return rename(raw::tuff_str(name)); }
    // Clears the custom name.
    WorldOutcome<Unit> clear_name() const {
        raw::tuff_plugin_host_world_error_t e = {};
        Uuid u = id;
        return detail::unit(raw::tuff_plugin_host_entity_set_name(&u, nullptr, &e), e);
    }

    inline WorldOutcome<Unit> teleport(const class World &world, Vec3 to) const;
};

// A non-player entity found in a box.
struct EntityInfo {
    Uuid uuid;
    String kind;
    Vec3 pos;
    String dimension;

    EntityRef ref() const { return EntityRef{uuid}; }
};

// One dimension, by key.
class World {
  public:
    explicit World(Str key) : key_(String::copy(key)) {}
    explicit World(const char *key) : key_(String::copy(key)) {}
    static World overworld() { return World(OVERWORLD); }
    static World nether() { return World(NETHER); }
    static World end() { return World(END); }

    Str key() const { return key_.view(); }

    // The block state at `at`, such as minecraft:oak_log[axis=y].
    WorldOutcome<String> block(BlockPos at) const {
        raw::tuff_plugin_host_world_error_t e = {};
        Str dim = key_.view();
        Str out = {nullptr, 0};
        if (!raw::tuff_plugin_host_block_get(&dim, &at, &out, &e)) {
            return WorldOutcome<String>(detail::world_error(e));
        }
        return WorldOutcome<String>(String::adopt(out));
    }

    // Sets the block at `at`, updating its neighbours.
    WorldOutcome<Unit> set_block(BlockPos at, Str state) const { return set(at, state, 0); }
    WorldOutcome<Unit> set_block(BlockPos at, const char *state) const {
        return set(at, raw::tuff_str(state), 0);
    }
    // Sets the block at `at` without updating its neighbours.
    WorldOutcome<Unit> set_block_quietly(BlockPos at, Str state) const {
        return set(at, state, TUFF_PLUGIN_TYPES_SET_FLAGS_NO_UPDATES);
    }
    WorldOutcome<Unit> set_block_quietly(BlockPos at, const char *state) const {
        return set_block_quietly(at, raw::tuff_str(state));
    }

    // The loaded non-player entities inside the box from `a` to `b`.
    WorldOutcome<detail::Vec<EntityInfo>> entities(Vec3 a, Vec3 b) const {
        raw::tuff_plugin_host_world_error_t e = {};
        Str dim = key_.view();
        raw::tuff_plugin_host_list_entity_t list = {};
        if (!raw::tuff_plugin_host_entities_in_box(&dim, &a, &b, &list, &e)) {
            return WorldOutcome<detail::Vec<EntityInfo>>(detail::world_error(e));
        }
        detail::Vec<EntityInfo> out;
        for (size_t i = 0; i < list.len; i++) {
            const raw::tuff_plugin_host_entity_t &en = list.ptr[i];
            out.push(
                EntityInfo{en.uuid, String::copy(en.kind), en.pos, String::copy(en.dimension)});
        }
        raw::tuff_plugin_host_list_entity_free(&list);
        return WorldOutcome<detail::Vec<EntityInfo>>(move(out));
    }

    // Spawns an entity of type `kind` (such as minecraft:pig).
    WorldOutcome<EntityRef> spawn(Str kind, Vec3 at) const { return spawn_with(kind, at, nullptr); }
    WorldOutcome<EntityRef> spawn(const char *kind, Vec3 at) const {
        return spawn_with(raw::tuff_str(kind), at, nullptr);
    }
    // With SNBT data.
    WorldOutcome<EntityRef> spawn(Str kind, Vec3 at, Str snbt) const {
        return spawn_with(kind, at, &snbt);
    }

    // Spawns the registered custom entity `id`, facing `yaw`.
    WorldOutcome<EntityRef> spawn_custom(Str id, Vec3 at, float yaw) const {
        raw::tuff_plugin_host_world_error_t e = {};
        Str dim = key_.view();
        Uuid out = {0, 0};
        if (!raw::tuff_plugin_host_spawn_custom_entity(&dim, &id, &at, yaw, &out, &e)) {
            return WorldOutcome<EntityRef>(detail::world_error(e));
        }
        return WorldOutcome<EntityRef>(EntityRef{out});
    }

  private:
    WorldOutcome<Unit> set(BlockPos at, Str state, raw::tuff_plugin_host_set_flags_t flags) const {
        raw::tuff_plugin_host_world_error_t e = {};
        Str dim = key_.view();
        return detail::unit(raw::tuff_plugin_host_block_set(&dim, &at, &state, flags, &e), e);
    }

    WorldOutcome<EntityRef> spawn_with(Str kind, Vec3 at, Str *snbt) const {
        raw::tuff_plugin_host_world_error_t e = {};
        Str dim = key_.view();
        Uuid out = {0, 0};
        if (!raw::tuff_plugin_host_spawn_entity(&dim, &kind, &at, snbt, &out, &e)) {
            return WorldOutcome<EntityRef>(detail::world_error(e));
        }
        return WorldOutcome<EntityRef>(EntityRef{out});
    }

    String key_;
};

inline WorldOutcome<Unit> EntityRef::teleport(const World &world, Vec3 to) const {
    raw::tuff_plugin_host_world_error_t e = {};
    Uuid u = id;
    Str dim = world.key();
    return detail::unit(raw::tuff_plugin_host_teleport_entity(&u, &dim, &to, nullptr, &e), e);
}

// Moves an online player to a dimension and position.
inline WorldOutcome<Unit> teleport_player(Uuid player, const World &world, Vec3 to) {
    raw::tuff_plugin_host_world_error_t e = {};
    Str dim = world.key();
    return detail::unit(raw::tuff_plugin_host_teleport_player(&player, &dim, &to, nullptr, &e), e);
}
inline WorldOutcome<Unit> teleport_player(Uuid player, const World &world, Vec3 to,
                                          Rotation rotation) {
    raw::tuff_plugin_host_world_error_t e = {};
    Str dim = world.key();
    return detail::unit(raw::tuff_plugin_host_teleport_player(&player, &dim, &to, &rotation, &e),
                        e);
}

// The player the current callback acts for (a command's sender, an
// event's player).
namespace acting {

// The player's dimension, from their location; none when no player acts.
inline Option<World> world() {
    raw::tuff_plugin_host_location_t loc;
    if (!raw::tuff_plugin_host_acting_location(&loc)) {
        return {};
    }
    World w(loc.dimension);
    raw::tuff_plugin_host_location_free(&loc);
    return Option<World>(move(w));
}

// Whether they hold permission `node`; none when no player acts.
inline Option<bool> has_permission(Str node) {
    bool held = false;
    if (!raw::tuff_plugin_host_has_permission(&node, &held)) {
        return {};
    }
    return Option<bool>(held);
}
inline Option<bool> has_permission(const char *node) { return has_permission(raw::tuff_str(node)); }

// Gives them `count` of `item`; what does not fit drops at their feet.
inline WorldOutcome<Unit> give(Str item, uint32_t count) {
    raw::tuff_plugin_host_world_error_t e = {};
    return detail::unit(raw::tuff_plugin_host_give_item(&item, count, nullptr, nullptr, &e), e);
}
inline WorldOutcome<Unit> give(const char *item, uint32_t count) {
    return give(raw::tuff_str(item), count);
}

// Moves them within their dimension.
inline WorldOutcome<Unit> teleport(Vec3 to) {
    raw::tuff_plugin_host_world_error_t e = {};
    return detail::unit(raw::tuff_plugin_host_teleport_self(&to, nullptr, &e), e);
}
inline WorldOutcome<Unit> teleport(Vec3 to, Rotation rotation) {
    raw::tuff_plugin_host_world_error_t e = {};
    return detail::unit(raw::tuff_plugin_host_teleport_self(&to, &rotation, &e), e);
}

} // namespace acting

} // namespace tuff

#endif
