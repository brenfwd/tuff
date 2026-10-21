import java.io.PrintWriter;
import java.lang.reflect.InvocationHandler;
import java.lang.reflect.Method;
import java.lang.reflect.Proxy;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.TreeSet;

import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.Direction;
import net.minecraft.core.Holder;
import net.minecraft.core.Registry;
import net.minecraft.core.RegistryAccess;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.server.packs.PackType;
import net.minecraft.server.packs.repository.ServerPacksSource;
import net.minecraft.server.packs.resources.MultiPackResourceManager;
import net.minecraft.sounds.SoundEvent;
import net.minecraft.tags.TagLoader;
import net.minecraft.world.attribute.EnvironmentAttribute;
import net.minecraft.world.attribute.EnvironmentAttributeReader;
import net.minecraft.world.attribute.EnvironmentAttributes;
import net.minecraft.world.level.BlockGetter;
import net.minecraft.world.level.LevelAccessor;
import net.minecraft.world.level.LevelReader;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.Blocks;
import net.minecraft.world.level.block.IceBlock;
import net.minecraft.world.level.block.LiquidBlock;
import net.minecraft.world.level.block.LiquidBlockContainer;
import net.minecraft.world.level.block.entity.BlockEntity;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.material.FlowingFluid;
import net.minecraft.world.level.material.Fluid;
import net.minecraft.world.level.material.FluidState;
import net.minecraft.world.phys.AABB;
import net.minecraft.world.phys.shapes.Shapes;
import net.minecraft.world.phys.shapes.VoxelShape;

/**
 * Dumps what the fluid simulation needs that no report contains.
 *
 * Output lines:
 *   directions <direction names in ordinal order>
 *   fast_lava <environment attribute> <default>
 *   fluid <name> <source 0/1> <source fluid> <flowing fluid> <tick delay> <tick delay, fast lava>
 *         <drop off> <drop off, fast lava> <slope find distance> <slope find distance, fast lava>
 *         <overridden flowing fluid methods, comma separated, or ->
 *     One per flowing fluid kind, in registry order; the empty fluid has none.
 *   fluid_state <id> <fluid> <amount> <falling 0/1> <source 0/1> <own height> <block state id>
 *   replaceable <fluid state id> <same fluid above 0/1> <bits>
 *     Whether the fluid state gives way to another fluid: one 0/1 per fluid in
 *     registry order, times directions.
 *   faces N
 *   covers <row of N 0/1 chars>   (N rows: whether collision faces a and b together cover a face)
 *   state <id> <fluid state id> <ice 0/1> <liquid container 0/1> <liquid block 0/1> <can place mask>
 *         <collision face DOWN> ... <EAST>
 *     can place mask: bit i when a fluid of registry index i may be placed into it.
 *   place <state id> <fluid> <result state id, or - if refused> <scheduled tick fluid or -> <sounds or ->
 *     What placing the fluid's source into a liquid container does; sounds
 *     are sound,volume,pitch separated by ';'.
 */
public class FluidProperties {
    private static final class Level implements BlockGetter {
        private final Map<BlockPos, BlockState> states = new HashMap<>();

        Level with(BlockPos pos, BlockState state) {
            states.put(pos, state);
            return this;
        }

        @Override
        public BlockEntity getBlockEntity(BlockPos pos) {
            return null;
        }

        @Override
        public BlockState getBlockState(BlockPos pos) {
            return states.getOrDefault(pos, Blocks.AIR.defaultBlockState());
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

    private static EnvironmentAttributeReader attributes(boolean fastLava) {
        return (EnvironmentAttributeReader) Proxy.newProxyInstance(
            FluidProperties.class.getClassLoader(),
            new Class<?>[] {EnvironmentAttributeReader.class},
            (proxy, method, args) -> {
                if (method.getName().equals("getDimensionValue")) {
                    EnvironmentAttribute<?> attribute = (EnvironmentAttribute<?>) args[0];
                    if (attribute == EnvironmentAttributes.FAST_LAVA) {
                        return fastLava;
                    }
                    throw new IllegalStateException("fluid code reads unmodeled attribute " + attribute);
                }
                throw new IllegalStateException("fluid code calls unmodeled " + method);
            });
    }

    private static LevelReader reader(boolean fastLava) {
        EnvironmentAttributeReader attributes = attributes(fastLava);
        return (LevelReader) Proxy.newProxyInstance(
            FluidProperties.class.getClassLoader(),
            new Class<?>[] {LevelReader.class},
            (proxy, method, args) -> {
                if (method.getName().equals("environmentAttributes")) {
                    return attributes;
                }
                throw new IllegalStateException("fluid code calls unmodeled " + method);
            });
    }

    /** A level that records what placing a liquid into a block does. */
    private static final class Recorder implements InvocationHandler {
        final EnvironmentAttributeReader attributes;
        BlockState state;
        boolean changed;
        final List<String> ticks = new ArrayList<>();
        final List<String> sounds = new ArrayList<>();

        Recorder(BlockState state, boolean fastLava) {
            this.state = state;
            this.attributes = attributes(fastLava);
        }

        LevelAccessor level() {
            return (LevelAccessor) Proxy.newProxyInstance(
                FluidProperties.class.getClassLoader(), new Class<?>[] {LevelAccessor.class}, this);
        }

        @Override
        public Object invoke(Object proxy, Method method, Object[] args) {
            switch (method.getName()) {
                case "hashCode":
                    return System.identityHashCode(proxy);
                case "equals":
                    return proxy == args[0];
                case "toString":
                    return "liquid placement recorder";
                case "isClientSide":
                    return false;
                case "environmentAttributes":
                    return attributes;
                case "getBlockState":
                    return at(args[0]) ? state : Blocks.AIR.defaultBlockState();
                case "getFluidState":
                    return (at(args[0]) ? state : Blocks.AIR.defaultBlockState()).getFluidState();
                case "getBlockEntity":
                    return null;
                case "setBlock":
                case "setBlockAndUpdate":
                    if (!at(args[0])) {
                        throw new IllegalStateException("liquid placement changes another block");
                    }
                    state = (BlockState) args[1];
                    changed = true;
                    return true;
                case "scheduleTick":
                    if (!at(args[0])) {
                        throw new IllegalStateException("liquid placement schedules another block");
                    }
                    if (!(args[1] instanceof Fluid fluid)) {
                        throw new IllegalStateException("liquid placement schedules a block tick");
                    }
                    int delay = (Integer) args[2];
                    if (delay != fluid.getTickDelay(reader(attributes.getDimensionValue(EnvironmentAttributes.FAST_LAVA)))) {
                        throw new IllegalStateException("liquid placement schedules an unusual delay");
                    }
                    ticks.add(BuiltInRegistries.FLUID.getKey(fluid).toString());
                    return null;
                case "playSound":
                    sounds.add(sound(args));
                    return null;
                case "gameEvent":
                case "addParticle":
                    return null;
                default:
                    throw new IllegalStateException("liquid placement calls unmodeled " + method);
            }
        }

        private static boolean at(Object pos) {
            return BlockPos.ZERO.equals(pos);
        }

        private static String sound(Object[] args) {
            SoundEvent event = null;
            List<Float> floats = new ArrayList<>();
            for (Object arg : args) {
                if (arg instanceof SoundEvent sound) {
                    event = sound;
                } else if (arg instanceof Holder<?> holder && holder.value() instanceof SoundEvent sound) {
                    event = sound;
                } else if (arg instanceof Float value) {
                    floats.add(value);
                }
            }
            if (event == null || floats.size() != 2) {
                throw new IllegalStateException("unrecognised sound call");
            }
            return event.location() + "," + floats.get(0) + "," + floats.get(1);
        }
    }

    private static final String[] FLOWING_METHODS = methodNames(FlowingFluid.class);

    private static String[] methodNames(Class<?> type) {
        TreeSet<String> names = new TreeSet<>();
        for (Method method : type.getDeclaredMethods()) {
            names.add(method.getName());
        }
        return names.toArray(new String[0]);
    }

    private static String overrides(Fluid fluid) {
        TreeSet<String> found = new TreeSet<>();
        for (Class<?> type = fluid.getClass(); type != FlowingFluid.class; type = type.getSuperclass()) {
            for (Method method : type.getDeclaredMethods()) {
                for (String name : FLOWING_METHODS) {
                    if (name.equals(method.getName())) {
                        found.add(name);
                    }
                }
            }
        }
        return found.isEmpty() ? "-" : String.join(",", found);
    }

    private static int intCall(FlowingFluid fluid, String name, boolean fastLava) throws Exception {
        Method method = FlowingFluid.class.getDeclaredMethod(name, LevelReader.class);
        method.setAccessible(true);
        return (Integer) method.invoke(fluid, reader(fastLava));
    }

    private static String faceKey(VoxelShape shape, Direction direction) {
        if (shape == Shapes.block()) {
            return "block";
        }
        if (shape.isEmpty()) {
            return "empty";
        }
        StringBuilder key = new StringBuilder(direction.getAxis().name());
        for (AABB box : shape.toAabbs()) {
            key.append(' ').append(box.minX).append(',').append(box.minY).append(',').append(box.minZ)
                .append(',').append(box.maxX).append(',').append(box.maxY).append(',').append(box.maxZ);
        }
        return key.toString();
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

        List<Fluid> fluids = new ArrayList<>();
        BuiltInRegistries.FLUID.forEach(fluids::add);

        try (PrintWriter out = new PrintWriter(Files.newBufferedWriter(Path.of(args[0])))) {
            StringBuilder directions = new StringBuilder("directions");
            for (Direction direction : Direction.values()) {
                directions.append(' ').append(direction.getSerializedName());
            }
            out.println(directions);
            out.println("fast_lava " + BuiltInRegistries.ENVIRONMENT_ATTRIBUTE.getKey(EnvironmentAttributes.FAST_LAVA)
                + " " + EnvironmentAttributes.FAST_LAVA.defaultValue());

            for (Fluid fluid : fluids) {
                if (!(fluid instanceof FlowingFluid flowing)) {
                    if (!fluid.defaultFluidState().isEmpty()) {
                        throw new IllegalStateException("unmodeled fluid " + fluid);
                    }
                    continue;
                }
                String name = BuiltInRegistries.FLUID.getKey(fluid).toString();
                out.println("fluid " + name + " " + (fluid.defaultFluidState().isSource() ? 1 : 0) + " "
                    + BuiltInRegistries.FLUID.getKey(flowing.getSource()) + " "
                    + BuiltInRegistries.FLUID.getKey(flowing.getFlowing()) + " "
                    + fluid.getTickDelay(reader(false)) + " " + fluid.getTickDelay(reader(true)) + " "
                    + intCall(flowing, "getDropOff", false) + " " + intCall(flowing, "getDropOff", true) + " "
                    + intCall(flowing, "getSlopeFindDistance", false) + " "
                    + intCall(flowing, "getSlopeFindDistance", true) + " " + overrides(fluid));
            }

            int fluidStates = Fluid.FLUID_STATE_REGISTRY.size();
            for (int id = 0; id < fluidStates; id++) {
                FluidState state = Fluid.FLUID_STATE_REGISTRY.byId(id);
                boolean falling = state.hasProperty(FlowingFluid.FALLING) && state.getValue(FlowingFluid.FALLING);
                out.println("fluid_state " + id + " " + BuiltInRegistries.FLUID.getKey(state.getType()) + " "
                    + state.getAmount() + " " + (falling ? 1 : 0) + " " + (state.isSource() ? 1 : 0) + " "
                    + state.getOwnHeight() + " " + Block.BLOCK_STATE_REGISTRY.getId(state.createLegacyBlock()));
            }
            BlockPos above = BlockPos.ZERO.above();
            for (int id = 0; id < fluidStates; id++) {
                FluidState state = Fluid.FLUID_STATE_REGISTRY.byId(id);
                for (int same = 0; same < 2; same++) {
                    Level level = new Level().with(BlockPos.ZERO, state.createLegacyBlock());
                    if (same == 1) {
                        level.with(above, state.getType().defaultFluidState().createLegacyBlock());
                    }
                    StringBuilder bits = new StringBuilder();
                    for (Fluid other : fluids) {
                        for (Direction direction : Direction.values()) {
                            bits.append(state.canBeReplacedWith(level, BlockPos.ZERO, other, direction) ? '1' : '0');
                        }
                    }
                    out.println("replaceable " + id + " " + same + " " + bits);
                }
            }

            List<VoxelShape> faces = new ArrayList<>();
            Map<String, Integer> ids = new HashMap<>();
            faces.add(Shapes.empty());
            ids.put("empty", 0);
            faces.add(Shapes.block());
            ids.put("block", 1);
            StringBuilder states = new StringBuilder();
            StringBuilder places = new StringBuilder();
            int count = Block.BLOCK_STATE_REGISTRY.size();
            for (int id = 0; id < count; id++) {
                BlockState state = Block.BLOCK_STATE_REGISTRY.byId(id);
                Block block = state.getBlock();
                boolean container = block instanceof LiquidBlockContainer;
                int mask = 0;
                if (container) {
                    for (int i = 0; i < fluids.size(); i++) {
                        Fluid fluid = fluids.get(i);
                        if (fluid.defaultFluidState().isEmpty()) {
                            continue;
                        }
                        if (((LiquidBlockContainer) block).canPlaceLiquid(null, new Level().with(BlockPos.ZERO, state),
                                BlockPos.ZERO, state, fluid)) {
                            mask |= 1 << i;
                            if (!fluid.defaultFluidState().isSource()) {
                                throw new IllegalStateException("unmodeled flowing placement into " + state);
                            }
                            Recorder slow = new Recorder(state, false);
                            boolean placed = ((LiquidBlockContainer) block).placeLiquid(
                                slow.level(), BlockPos.ZERO, state, fluid.defaultFluidState());
                            Recorder fast = new Recorder(state, true);
                            ((LiquidBlockContainer) block).placeLiquid(
                                fast.level(), BlockPos.ZERO, state, fluid.defaultFluidState());
                            places.append("place ").append(id).append(' ')
                                .append(BuiltInRegistries.FLUID.getKey(fluid)).append(' ');
                            if (!placed || !slow.changed) {
                                places.append("- - -\n");
                                continue;
                            }
                            if (slow.ticks.size() > 1) {
                                throw new IllegalStateException("liquid placement schedules several ticks");
                            }
                            places.append(Block.BLOCK_STATE_REGISTRY.getId(slow.state)).append(' ')
                                .append(slow.ticks.isEmpty() ? "-" : slow.ticks.get(0)).append(' ')
                                .append(slow.sounds.isEmpty() ? "-" : String.join(";", slow.sounds)).append('\n');
                        }
                    }
                }
                VoxelShape collision = state.getCollisionShape(new Level().with(BlockPos.ZERO, state), BlockPos.ZERO);
                states.append("state ").append(id)
                    .append(' ').append(Fluid.FLUID_STATE_REGISTRY.getId(state.getFluidState()))
                    .append(' ').append(block instanceof IceBlock ? 1 : 0)
                    .append(' ').append(container ? 1 : 0)
                    .append(' ').append(block instanceof LiquidBlock ? 1 : 0)
                    .append(' ').append(mask);
                for (Direction direction : Direction.values()) {
                    VoxelShape face = collision.getFaceShape(direction);
                    String key = faceKey(face, direction);
                    Integer index = ids.get(key);
                    if (index == null) {
                        index = faces.size();
                        faces.add(face);
                        ids.put(key, index);
                    }
                    states.append(' ').append(index);
                }
                states.append('\n');
            }

            out.println("faces " + faces.size());
            for (VoxelShape a : faces) {
                StringBuilder row = new StringBuilder("covers ");
                for (VoxelShape b : faces) {
                    row.append(Shapes.faceShapeOccludes(a, b) ? '1' : '0');
                }
                out.println(row);
            }
            out.print(states);
            out.print(places);
        }
    }
}
