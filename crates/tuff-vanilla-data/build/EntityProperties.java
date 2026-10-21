import java.io.PrintWriter;
import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.lang.reflect.Modifier;
import java.lang.reflect.ParameterizedType;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.BitSet;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.TreeMap;

import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.Registry;
import net.minecraft.core.RegistryAccess;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.network.syncher.EntityDataAccessor;
import net.minecraft.network.syncher.EntityDataSerializer;
import net.minecraft.network.syncher.EntityDataSerializers;
import net.minecraft.server.Bootstrap;
import net.minecraft.server.level.ServerEntity;
import net.minecraft.server.packs.PackType;
import net.minecraft.server.packs.repository.ServerPacksSource;
import net.minecraft.server.packs.resources.MultiPackResourceManager;
import net.minecraft.tags.TagLoader;
import net.minecraft.util.random.Weighted;
import net.minecraft.util.random.WeightedList;
import net.minecraft.world.attribute.EnvironmentAttributes;
import net.minecraft.world.entity.Avatar;
import net.minecraft.world.entity.Entity;
import net.minecraft.world.entity.EntityDimensions;
import net.minecraft.world.entity.EntityType;
import net.minecraft.world.entity.EntityTypes;
import net.minecraft.world.entity.EquipmentSlot;
import net.minecraft.world.entity.LivingEntity;
import net.minecraft.world.entity.Mob;
import net.minecraft.world.entity.MobCategory;
import net.minecraft.world.entity.Pose;
import net.minecraft.world.entity.SpawnPlacementType;
import net.minecraft.world.entity.SpawnPlacementTypes;
import net.minecraft.world.entity.SpawnPlacements;
import net.minecraft.world.entity.animal.sheep.SheepColorSpawnRules;
import net.minecraft.world.item.DyeColor;
import net.minecraft.world.item.Item;
import net.minecraft.world.level.BlockGetter;
import net.minecraft.world.level.biome.MobSpawnSettings;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.Blocks;
import net.minecraft.world.level.block.entity.BlockEntity;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.material.FluidState;
import net.minecraft.world.level.pathfinder.PathComputationType;
import net.minecraft.world.level.pathfinder.PathType;
import net.minecraft.world.level.pathfinder.WalkNodeEvaluator;

/**
 * Dumps per-entity-type data that lives in game code rather than in any
 * report, plus the block data mobs move, spawn and path over.
 *
 * Output lines:
 *   tracker <name> <value>
 *     Constants of the server's entity update pacing:
 *     position_tolerance (squared blocks) and forced_position_period (ticks).
 *   category <name> <max per chunk> <friendly 0/1> <persistent 0/1> <despawn distance> <no despawn distance>
 *   entity <id> <tracking range in chunks> <update interval ticks | never> <track deltas 0/1>
 *          <fire immune 0/1> <width> <height> <eye height> <category> <summonable 0/1>
 *          <spawns far from players 0/1> <allowed in peaceful 0/1> <spawn box scale>
 *          <default loot table | none>
 *   vehicle_attachment <id> <x> <y> <z>
 *     Where the type sits on what it rides, from its feet, when not zero.
 *   passenger_attachment <id> <x> <y> <z>
 *     Where its passengers sit, from its feet, unturned: one line per seat
 *     in seat order.
 *   baby <id> <width> <height> <eye height>
 *     Types whose babies have their own size rather than half the adult's.
 *   air <id> <method>,<method>...
 *     Types whose classes change how they breathe: the air supply and
 *     breathing methods they declare below the living entity.
 *   air_supply <ticks>
 *     The air supply every entity starts with and refills to by default.
 *   pose <name> <network id>
 *     Every entity pose.
 *   player_pose <pose> <width> <height> <eye height>
 *     The player's box and eyes in every pose.
 *   placement <id> <placement type> <heightmap>
 *     Where natural spawning puts the type: on_ground, in_water, in_lava or
 *     no_restrictions, on the named heightmap.
 *   serializer <id> <name>
 *     Every synched data serializer by network id, named after its field.
 *   data <id> <serializer id>...
 *     The type's synched data layout: one serializer id per data index.
 *   pathtype <name> <malus>
 *     Path types in network id order.
 *   movement <block> <friction> <speed factor> <jump factor> <fall distance reduction> <bounce restitution>
 *   path <block> <per-state value>...
 *     Per block state: the path type id, plus 32 when the state is
 *     pathfindable for land mobs and 64 when it is for water mobs.
 *   spawnable <block> <per-state value>...
 *     Per block state, the entity types that may spawn on top of it, by
 *     registry id: "a" for all, "n" for none, "+id.id.." for only those and
 *     "-id.id.." for all but those.
 *   creature_spawn_probability <default>
 *     The default of the biome attribute giving the chance of a creature
 *     group spawning with a new chunk; the natural spawn settings
 *     attribute is checked to default to no spawns.
 *   dangerous <block> <per-state value>...
 *     Per block state, the entity types to which standing in it is
 *     dangerous (natural spawning avoids it), encoded like spawnable.
 *   redstone <block> <per-state value>...
 *     Per block state: 1 if it conducts redstone (spawning never starts
 *     inside one), plus 2 if it is a signal source.
 *   sheep_colors <climate> <tree>
 *     The weighted color choice for naturally spawned sheep; a tree is a
 *     color name or "(" entries of weight:tree ")".
 */
public class EntityProperties {
    private static Object constant(Class<?> owner, String name) throws Exception {
        Field field = owner.getDeclaredField(name);
        field.setAccessible(true);
        return field.get(null);
    }

    /** A world holding one block state at the origin and air elsewhere. */
    private static final class One implements BlockGetter {
        private final BlockState state;

        One(BlockState state) {
            this.state = state;
        }

        @Override
        public BlockEntity getBlockEntity(BlockPos pos) {
            return null;
        }

        @Override
        public BlockState getBlockState(BlockPos pos) {
            return pos.equals(BlockPos.ZERO) ? state : Blocks.AIR.defaultBlockState();
        }

        @Override
        public FluidState getFluidState(BlockPos pos) {
            return getBlockState(pos).getFluidState();
        }

        @Override
        public int getHeight() {
            return 384;
        }

        @Override
        public int getMinY() {
            return -64;
        }
    }

    private static Map<EntityType<?>, Class<?>> entityClasses() throws Exception {
        Map<EntityType<?>, Class<?>> classes = new HashMap<>();
        for (Field field : EntityTypes.class.getDeclaredFields()) {
            if (!Modifier.isStatic(field.getModifiers()) || field.getType() != EntityType.class) {
                continue;
            }
            if (!(field.getGenericType() instanceof ParameterizedType generic)
                || !(generic.getActualTypeArguments()[0] instanceof Class<?> owner)) {
                throw new IllegalStateException("entity type field without a class: " + field);
            }
            classes.put((EntityType<?>) field.get(null), owner);
        }
        return classes;
    }

    private static EntityDimensions babyDimensions(Class<?> owner) throws Exception {
        for (Class<?> c = owner; c != null; c = c.getSuperclass()) {
            for (Field field : c.getDeclaredFields()) {
                if (Modifier.isStatic(field.getModifiers())
                    && field.getType() == EntityDimensions.class
                    && field.getName().equals("BABY_DIMENSIONS")) {
                    field.setAccessible(true);
                    return (EntityDimensions) field.get(null);
                }
            }
        }
        return null;
    }

    private static final String[] LIVING_AIR_METHODS = {
        "canBreatheUnderwater", "decreaseAirSupply", "increaseAirSupply", "shouldTakeDrowningDamage",
    };

    private static boolean declares(Class<?> owner, String name) {
        for (Method method : owner.getDeclaredMethods()) {
            if (method.getName().equals(name)) {
                return true;
            }
        }
        return false;
    }

    private static boolean isAirMethod(String name) {
        return name.contains("AirSupply") || name.contains("Breathe");
    }

    /** The breathing methods the type's classes declare below the base entity classes. */
    private static List<String> airMethods(Class<?> owner) {
        TreeMap<String, Boolean> names = new TreeMap<>();
        for (Class<?> c = owner; c != null && c != LivingEntity.class && c != Entity.class; c = c.getSuperclass()) {
            for (Method method : c.getDeclaredMethods()) {
                if (isAirMethod(method.getName()) && !method.isSynthetic()) {
                    names.put(method.getName(), true);
                }
            }
        }
        return new ArrayList<>(names.keySet());
    }

    private static void checkAirMethods() {
        for (String name : LIVING_AIR_METHODS) {
            if (!declares(LivingEntity.class, name)) {
                throw new IllegalStateException("living entities no longer declare " + name);
            }
        }
        if (!declares(Entity.class, "getMaxAirSupply")) {
            throw new IllegalStateException("entities no longer declare getMaxAirSupply");
        }
        for (Method method : Entity.class.getDeclaredMethods()) {
            if (isAirMethod(method.getName()) && !method.getName().equals("getMaxAirSupply")
                && !method.getName().equals("getAirSupply") && !method.getName().equals("setAirSupply")) {
                throw new IllegalStateException("unexpected entity air method " + method.getName());
            }
        }
    }

    private static List<Integer> dataLayout(Class<?> owner) throws Exception {
        TreeMap<Integer, Integer> layout = new TreeMap<>();
        for (Class<?> c = owner; c != null; c = c.getSuperclass()) {
            for (Field field : c.getDeclaredFields()) {
                if (!Modifier.isStatic(field.getModifiers()) || field.getType() != EntityDataAccessor.class) {
                    continue;
                }
                field.setAccessible(true);
                EntityDataAccessor<?> accessor = (EntityDataAccessor<?>) field.get(null);
                int serializer = EntityDataSerializers.getSerializedId(accessor.serializer());
                Integer previous = layout.put(accessor.id(), serializer);
                if (previous != null && previous != serializer) {
                    throw new IllegalStateException("data index " + accessor.id() + " defined twice in " + owner);
                }
            }
        }
        List<Integer> out = new ArrayList<>();
        int expected = 0;
        for (Map.Entry<Integer, Integer> entry : layout.entrySet()) {
            if (entry.getKey() != expected++) {
                throw new IllegalStateException("data indices of " + owner + " have a gap at " + entry.getKey());
            }
            out.add(entry.getValue());
        }
        return out;
    }

    private static String placementName(SpawnPlacementType type) throws Exception {
        for (Field field : SpawnPlacementTypes.class.getDeclaredFields()) {
            if (Modifier.isStatic(field.getModifiers()) && field.get(null) == type) {
                return field.getName().toLowerCase();
            }
        }
        throw new IllegalStateException("unknown spawn placement type " + type);
    }

    private static String colorTree(Object provider) throws Exception {
        Field[] captured = provider.getClass().getDeclaredFields();
        if (captured.length != 1) {
            throw new IllegalStateException("sheep color provider captures " + captured.length + " values");
        }
        captured[0].setAccessible(true);
        Object value = captured[0].get(provider);
        if (value instanceof DyeColor color) {
            return color.getSerializedName();
        }
        if (value instanceof WeightedList<?> list) {
            StringBuilder out = new StringBuilder("(");
            for (Weighted<?> entry : list.unwrap()) {
                if (out.length() > 1) {
                    out.append(' ');
                }
                out.append(entry.weight()).append(':').append(colorTree(entry.value()));
            }
            return out.append(')').toString();
        }
        throw new IllegalStateException("unknown sheep color provider capture " + value);
    }

    private static String idList(BitSet set, int size, boolean included) {
        StringBuilder ids = new StringBuilder();
        for (int i = 0; i < size; i++) {
            if (set.get(i) == included) {
                ids.append(ids.length() == 0 ? "" : ".").append(i);
            }
        }
        return ids.toString();
    }

    private static String typeSet(BitSet set, int size) {
        int count = set.cardinality();
        if (count == size) {
            return "a";
        }
        if (count == 0) {
            return "n";
        }
        if (count * 2 <= size) {
            return "+" + idList(set, size, true);
        }
        return "-" + idList(set, size, false);
    }

    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        List<Registry.PendingTags<?>> tags = TagLoader.loadTagsForExistingRegistries(
            new MultiPackResourceManager(
                PackType.SERVER_DATA, List.of(ServerPacksSource.createVanillaPackSource().fullResources())),
            RegistryAccess.fromRegistryOfRegistries(BuiltInRegistries.REGISTRY));
        if (tags.isEmpty()) {
            throw new IllegalStateException("no tags found in the vanilla data pack");
        }
        tags.forEach(Registry.PendingTags::apply);

        Map<EntityType<?>, Class<?>> classes = entityClasses();
        Method pathTypeOf = WalkNodeEvaluator.class.getDeclaredMethod("getPathTypeFromState", BlockGetter.class, BlockPos.class);
        pathTypeOf.setAccessible(true);
        Field spawnScale = EntityType.class.getDeclaredField("spawnDimensionsScale");
        spawnScale.setAccessible(true);
        List<EntityType<?>> types = new ArrayList<>();
        BuiltInRegistries.ENTITY_TYPE.forEach(types::add);

        try (PrintWriter out = new PrintWriter(Files.newBufferedWriter(Path.of(args[0])))) {
            out.println("tracker position_tolerance " + (double) constant(ServerEntity.class, "TOLERANCE_LEVEL_POSITION"));
            out.println("tracker forced_position_period " + (int) constant(ServerEntity.class, "FORCED_POS_UPDATE_PERIOD"));
            for (Field field : EntityDataSerializers.class.getDeclaredFields()) {
                if (Modifier.isStatic(field.getModifiers())
                    && EntityDataSerializer.class.isAssignableFrom(field.getType())) {
                    field.setAccessible(true);
                    EntityDataSerializer<?> serializer = (EntityDataSerializer<?>) field.get(null);
                    out.println("serializer " + EntityDataSerializers.getSerializedId(serializer)
                        + " " + field.getName().toLowerCase());
                }
            }
            checkAirMethods();
            out.println("air_supply " + (int) constant(Entity.class, "TOTAL_AIR_SUPPLY"));
            for (Pose pose : Pose.values()) {
                out.println("pose " + pose.getSerializedName() + " " + pose.id());
            }
            @SuppressWarnings("unchecked")
            Map<Pose, EntityDimensions> playerPoses = (Map<Pose, EntityDimensions>) constant(Avatar.class, "POSES");
            EntityDimensions standing = (EntityDimensions) constant(Avatar.class, "STANDING_DIMENSIONS");
            if (playerPoses.get(Pose.STANDING) != standing) {
                throw new IllegalStateException("the player's standing pose is not its default size");
            }
            for (Pose pose : Pose.values()) {
                EntityDimensions size = playerPoses.getOrDefault(pose, standing);
                out.println("player_pose " + pose.getSerializedName()
                    + " " + size.width() + " " + size.height() + " " + size.eyeHeight());
            }
            for (MobCategory category : MobCategory.values()) {
                out.println("category " + category.getName()
                    + " " + category.getMaxInstancesPerChunk()
                    + " " + (category.isFriendly() ? 1 : 0)
                    + " " + (category.isPersistent() ? 1 : 0)
                    + " " + category.getDespawnDistance()
                    + " " + category.getNoDespawnDistance());
            }
            for (EntityType<?> type : types) {
                String id = BuiltInRegistries.ENTITY_TYPE.getKey(type).toString();
                Class<?> owner = classes.get(type);
                if (owner == null) {
                    throw new IllegalStateException("no class for entity type " + id);
                }
                out.println("entity " + id
                    + " " + type.clientTrackingRange()
                    + " " + (type.hasUpdateInterval() ? Integer.toString(type.updateInterval()) : "never")
                    + " " + (type.trackDeltas() ? 1 : 0)
                    + " " + (type.fireImmune() ? 1 : 0)
                    + " " + type.getWidth()
                    + " " + type.getHeight()
                    + " " + type.getDimensions().eyeHeight()
                    + " " + type.getCategory().getName()
                    + " " + (type.canSummon() ? 1 : 0)
                    + " " + (type.canSpawnFarFromPlayer() ? 1 : 0)
                    + " " + (type.isAllowedInPeaceful() ? 1 : 0)
                    + " " + spawnScale.getFloat(type)
                    + " " + type.getDefaultLootTable().map(key -> key.identifier().toString()).orElse("none"));
                net.minecraft.world.phys.Vec3 seat = type.getDimensions().attachments()
                    .get(net.minecraft.world.entity.EntityAttachment.VEHICLE, 0, 0.0f);
                if (!seat.equals(net.minecraft.world.phys.Vec3.ZERO)) {
                    out.println("vehicle_attachment " + id + " " + seat.x + " " + seat.y + " " + seat.z);
                }
                for (int seatIndex = 0;; seatIndex++) {
                    net.minecraft.world.phys.Vec3 point = type.getDimensions().attachments()
                        .getNullable(net.minecraft.world.entity.EntityAttachment.PASSENGER, seatIndex, 0.0f);
                    if (point == null) {
                        break;
                    }
                    out.println("passenger_attachment " + id + " " + point.x + " " + point.y + " " + point.z);
                }
                EntityDimensions baby = babyDimensions(owner);
                if (baby != null) {
                    out.println("baby " + id + " " + baby.width() + " " + baby.height() + " " + baby.eyeHeight());
                }
                List<String> air = airMethods(owner);
                if (!air.isEmpty()) {
                    out.println("air " + id + " " + String.join(",", air));
                }
                out.println("placement " + id
                    + " " + placementName(SpawnPlacements.getPlacementType(type))
                    + " " + SpawnPlacements.getHeightmapType(type).getSerializedName());
                StringBuilder data = new StringBuilder("data " + id);
                for (int serializer : dataLayout(owner)) {
                    data.append(' ').append(serializer);
                }
                out.println(data);
            }
            for (PathType type : PathType.values()) {
                out.println("pathtype " + type.name().toLowerCase() + " " + type.getMalus());
            }
            for (Block block : BuiltInRegistries.BLOCK) {
                String name = BuiltInRegistries.BLOCK.getKey(block).toString();
                out.println("movement " + name + " " + block.getFriction() + " " + block.getSpeedFactor() + " " + block.getJumpFactor() + " " + block.getFallDistanceReduction() + " " + block.getBounceRestitution());
                StringBuilder path = new StringBuilder("path " + name);
                StringBuilder spawnable = new StringBuilder("spawnable " + name);
                StringBuilder dangerous = new StringBuilder("dangerous " + name);
                StringBuilder redstone = new StringBuilder("redstone " + name);
                for (BlockState state : block.getStateDefinition().getPossibleStates()) {
                    One level = new One(state);
                    PathType type = (PathType) pathTypeOf.invoke(null, level, BlockPos.ZERO);
                    int value = type.ordinal() + (state.isPathfindable(PathComputationType.LAND) ? 32 : 0) + (state.isPathfindable(PathComputationType.WATER) ? 64 : 0);
                    path.append(' ').append(value);
                    BitSet allowed = new BitSet();
                    for (int i = 0; i < types.size(); i++) {
                        if (state.isValidSpawn(level, BlockPos.ZERO, types.get(i))) {
                            allowed.set(i);
                        }
                    }
                    spawnable.append(' ').append(typeSet(allowed, types.size()));
                    BitSet hurts = new BitSet();
                    for (int i = 0; i < types.size(); i++) {
                        if (types.get(i).isBlockDangerous(state)) {
                            hurts.set(i);
                        }
                    }
                    dangerous.append(' ').append(typeSet(hurts, types.size()));
                    int bits = (state.isRedstoneConductor(level, BlockPos.ZERO) ? 1 : 0)
                        + (state.isSignalSource() ? 2 : 0);
                    redstone.append(' ').append(bits);
                }
                out.println(path);
                out.println(spawnable);
                out.println(dangerous);
                out.println(redstone);
            }
            if (!EnvironmentAttributes.NATURAL_MOB_SPAWNS.defaultValue().equals(MobSpawnSettings.EMPTY)) {
                throw new IllegalStateException("natural mob spawns no longer default to none");
            }
            out.println("creature_spawn_probability " + EnvironmentAttributes.CREATURE_WORLD_GEN_SPAWN_PROBABILITY.defaultValue());
            for (String climate : new String[] {"temperate", "warm", "cold"}) {
                Object configuration = constant(SheepColorSpawnRules.class, climate.toUpperCase() + "_SPAWN_CONFIGURATION");
                Method colors = configuration.getClass().getDeclaredMethod("colors");
                colors.setAccessible(true);
                out.println("sheep_colors " + climate + " " + colorTree(colors.invoke(configuration)));
            }
            @SuppressWarnings("unchecked")
            List<EquipmentSlot> armorOrder = (List<EquipmentSlot>) constant(Mob.class, "EQUIPMENT_POPULATION_ORDER");
            for (EquipmentSlot slot : armorOrder) {
                StringBuilder armor = new StringBuilder("spawn_armor " + slot.getName());
                for (int tier = 0; ; tier++) {
                    Item item = Mob.getEquipmentForSlot(slot, tier);
                    if (item == null) {
                        break;
                    }
                    armor.append(' ').append(BuiltInRegistries.ITEM.getKey(item));
                }
                out.println(armor);
            }
        }
    }
}
