import java.io.PrintWriter;
import java.lang.reflect.Field;
import java.lang.reflect.InvocationTargetException;
import java.lang.reflect.Method;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Arrays;
import java.util.HashMap;
import java.util.HashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.TreeSet;

import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.dispenser.DefaultDispenseItemBehavior;
import net.minecraft.core.dispenser.DispenseItemBehavior;
import net.minecraft.core.dispenser.ProjectileDispenseBehavior;
import net.minecraft.world.item.Item;
import net.minecraft.core.Direction;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.sounds.SoundEvent;
import net.minecraft.world.level.BlockGetter;
import net.minecraft.world.level.Level;
import net.minecraft.world.level.block.*;
import net.minecraft.world.level.block.entity.BlockEntity;
import net.minecraft.world.level.block.piston.MovingPistonBlock;
import net.minecraft.world.level.block.piston.PistonBaseBlock;
import net.minecraft.world.level.block.piston.PistonHeadBlock;
import net.minecraft.world.level.block.state.BlockBehaviour;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.block.state.properties.BlockSetType;
import net.minecraft.world.level.material.FluidState;

/**
 * Dumps what the redstone simulation needs to know about blocks.
 *
 * Output lines:
 *   state <id> <flags> [<weak> <strong>]
 *     flags: 1 conducts redstone, 2 is a signal source, 4 has an analog
 *       output. For signal sources, weak and strong are the signals the
 *       state gives, alone in an empty world, toward each direction (down,
 *       up, north, south, west, east) as six hex digits.
 *   component <block> <component>
 *     The block's redstone behaviour, from the nearest class in its
 *     hierarchy listed in COMPONENTS.
 *   analog <block> <state|container|unsupported>
 *     Where the block's comparator output comes from: its state alone
 *     (the values follow as analog_state lines), the contents of its
 *     container, or something not modeled.
 *   analog_state <id> <value>
 *     A state-driven comparator output, for nonzero values only.
 *   plate <block> <everything|mobs|weighted> <max weight> <pressed ticks>
 *         <click on sound> <click off sound>
 *   arrow_button <block>
 *     A button arrows can press.
 *   dispense <item> <default|projectile|special>
 *     Items with their own dispenser behaviour: the plain drop, a
 *     projectile, or item-specific code.
 */
public class RedstoneProperties {
    private static final Direction[] DIRECTIONS = {
        Direction.DOWN, Direction.UP, Direction.NORTH, Direction.SOUTH, Direction.WEST, Direction.EAST,
    };

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

    private record Component(String name, Class<?>... classes) {}

    private static final Component[] COMPONENTS = {
        new Component("wire", RedstoneWireBlock.class),
        new Component("wall_torch", RedstoneWallTorchBlock.class),
        new Component("torch", RedstoneTorchBlock.class),
        new Component("repeater", RepeaterBlock.class),
        new Component("comparator", ComparatorBlock.class),
        new Component("observer", ObserverBlock.class),
        new Component("lever", LeverBlock.class),
        new Component("button", ButtonBlock.class),
        new Component("pressure_plate", PressurePlateBlock.class, WeightedPressurePlateBlock.class),
        new Component("power_block", PoweredBlock.class),
        new Component("lamp", RedstoneLampBlock.class),
        new Component("door", DoorBlock.class),
        new Component("trapdoor", TrapDoorBlock.class),
        new Component("fence_gate", FenceGateBlock.class),
        new Component("note_block", NoteBlock.class),
        new Component("tnt", TntBlock.class),
        new Component("hopper", HopperBlock.class),
        new Component("bell", BellBlock.class),
        new Component("dropper", DropperBlock.class),
        new Component("dispenser", DispenserBlock.class),
        new Component("daylight_detector", DaylightDetectorBlock.class),
        new Component("target", TargetBlock.class),
    };

    /** Classes whose redstone hooks are deliberately not modeled yet. */
    private static final Class<?>[] UNMODELED = {
        AbstractSkullBlock.class, BaseRailBlock.class, BigDripleafBlock.class, CommandBlock.class,
        CopperBulbBlock.class, CrafterBlock.class, DetectorRailBlock.class, FrostedIceBlock.class,
        LecternBlock.class, LightningRodBlock.class, LiquidBlock.class, PistonBaseBlock.class,
        PistonHeadBlock.class, MovingPistonBlock.class, SculkSensorBlock.class, CalibratedSculkSensorBlock.class,
        ShelfBlock.class, SpongeBlock.class, StructureBlock.class, TestBlock.class, TrappedChestBlock.class,
        TripWireBlock.class, TripWireHookBlock.class, PoweredRailBlock.class, RailBlock.class,
        SculkShriekerBlock.class, JigsawBlock.class, JukeboxBlock.class,
    };

    private static final String[] HOOKS = {"neighborChanged", "getSignal", "getDirectSignal", "isSignalSource"};

    /** Comparator outputs computed from something other than the state. */
    private static final Map<Class<?>, String> DYNAMIC_ANALOG = Map.ofEntries(
        Map.entry(AbstractFurnaceBlock.class, "container"),
        Map.entry(BarrelBlock.class, "container"),
        Map.entry(BrewingStandBlock.class, "container"),
        Map.entry(ChestBlock.class, "container"),
        Map.entry(DispenserBlock.class, "container"),
        Map.entry(HopperBlock.class, "container"),
        Map.entry(ShulkerBoxBlock.class, "container"),
        Map.entry(ChiseledBookShelfBlock.class, "unsupported"),
        Map.entry(CommandBlock.class, "unsupported"),
        Map.entry(CopperBulbBlock.class, "unsupported"),
        Map.entry(CrafterBlock.class, "unsupported"),
        Map.entry(CreakingHeartBlock.class, "unsupported"),
        Map.entry(DecoratedPotBlock.class, "unsupported"),
        Map.entry(DetectorRailBlock.class, "unsupported"),
        Map.entry(JukeboxBlock.class, "unsupported"),
        Map.entry(LecternBlock.class, "unsupported"),
        Map.entry(SculkSensorBlock.class, "unsupported"),
        Map.entry(ShelfBlock.class, "unsupported"));

    private static String sound(SoundEvent sound) {
        return sound.location().toString();
    }

    private static Object field(Class<?> owner, String name, Object target) throws Exception {
        Field field = owner.getDeclaredField(name);
        field.setAccessible(true);
        return field.get(target);
    }

    private static Class<?> declaring(Block block, String method) {
        for (Class<?> type = block.getClass(); type != null; type = type.getSuperclass()) {
            for (Method declared : type.getDeclaredMethods()) {
                if (declared.getName().equals(method)) {
                    return type;
                }
            }
        }
        return null;
    }

    private static String nibbles(int[] values) {
        StringBuilder out = new StringBuilder();
        for (int value : values) {
            out.append(Integer.toHexString(value));
        }
        return out.toString();
    }

    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();

        Map<Class<?>, String> components = new HashMap<>();
        for (Component component : COMPONENTS) {
            for (Class<?> type : component.classes()) {
                if (components.put(type, component.name()) != null) {
                    throw new IllegalStateException(type.getName() + " has two components");
                }
            }
        }
        Set<Class<?>> unmodeled = new HashSet<>(Arrays.asList(UNMODELED));
        Set<String> missing = new TreeSet<>();
        Method analog = BlockBehaviour.class.getDeclaredMethod(
            "getAnalogOutputSignal", BlockState.class, Level.class, BlockPos.class, Direction.class);
        analog.setAccessible(true);
        Method pressedTime = BasePressurePlateBlock.class.getDeclaredMethod("getPressedTime");
        pressedTime.setAccessible(true);

        try (PrintWriter out = new PrintWriter(Files.newBufferedWriter(Path.of(args[0])))) {
            int count = Block.BLOCK_STATE_REGISTRY.size();
            for (int id = 0; id < count; id++) {
                BlockState state = Block.BLOCK_STATE_REGISTRY.byId(id);
                One level = new One(state);
                int flags = (state.isRedstoneConductor(level, BlockPos.ZERO) ? 1 : 0)
                    | (state.isSignalSource() ? 2 : 0)
                    | (state.hasAnalogOutputSignal() ? 4 : 0);
                String line = "state " + id + " " + flags;
                if (state.isSignalSource()) {
                    int[] weak = new int[6];
                    int[] strong = new int[6];
                    for (int d = 0; d < DIRECTIONS.length; d++) {
                        weak[d] = state.getSignal(level, BlockPos.ZERO, DIRECTIONS[d]);
                        strong[d] = state.getDirectSignal(level, BlockPos.ZERO, DIRECTIONS[d]);
                    }
                    line += " " + nibbles(weak) + " " + nibbles(strong);
                }
                out.println(line);
            }

            for (Block block : BuiltInRegistries.BLOCK) {
                String name = BuiltInRegistries.BLOCK.getKey(block).toString();
                String component = null;
                for (Class<?> type = block.getClass(); type != Block.class; type = type.getSuperclass()) {
                    String listed = components.get(type);
                    if (component == null) {
                        component = listed;
                    }
                    if (listed != null || component != null || unmodeled.contains(type)) {
                        continue;
                    }
                    for (Method method : type.getDeclaredMethods()) {
                        if (Arrays.asList(HOOKS).contains(method.getName())) {
                            missing.add(type.getName());
                        }
                    }
                }
                if (component != null) {
                    out.println("component " + name + " " + component);
                }

                BlockState defaultState = block.defaultBlockState();
                if (defaultState.hasAnalogOutputSignal()) {
                    Class<?> source = declaring(block, "getAnalogOutputSignal");
                    String kind = DYNAMIC_ANALOG.get(source);
                    if (kind == null) {
                        StringBuilder values = new StringBuilder();
                        for (BlockState state : block.getStateDefinition().getPossibleStates()) {
                            int value;
                            try {
                                value = (Integer) analog.invoke(block, state, null, BlockPos.ZERO, Direction.NORTH);
                            } catch (InvocationTargetException e) {
                                throw new IllegalStateException(
                                    name + " reads the world for its comparator output; classify "
                                        + source.getName() + " in DYNAMIC_ANALOG", e);
                            }
                            if (value != 0) {
                                values.append("analog_state ").append(Block.BLOCK_STATE_REGISTRY.getId(state))
                                    .append(' ').append(value).append('\n');
                            }
                        }
                        out.println("analog " + name + " state");
                        out.print(values);
                    } else {
                        out.println("analog " + name + " " + kind);
                    }
                }

                if (block instanceof BasePressurePlateBlock plate) {
                    BlockSetType type = (BlockSetType) field(BasePressurePlateBlock.class, "type", block);
                    String sensitivity;
                    int maxWeight = 0;
                    if (block instanceof WeightedPressurePlateBlock) {
                        sensitivity = "weighted";
                        maxWeight = (Integer) field(WeightedPressurePlateBlock.class, "maxWeight", block);
                    } else {
                        sensitivity = switch (type.pressurePlateSensitivity()) {
                            case EVERYTHING -> "everything";
                            case MOBS -> "mobs";
                        };
                    }
                    out.println("plate " + name + " " + sensitivity + " " + maxWeight + " "
                        + pressedTime.invoke(plate) + " " + sound(type.pressurePlateClickOn()) + " "
                        + sound(type.pressurePlateClickOff()));
                }
                if (block instanceof ButtonBlock) {
                    BlockSetType type = (BlockSetType) field(ButtonBlock.class, "type", block);
                    if (type.canButtonBeActivatedByArrows()) {
                        out.println("arrow_button " + name);
                    }
                }
            }

            for (Map.Entry<Item, DispenseItemBehavior> entry : DispenserBlock.DISPENSER_REGISTRY.entrySet()) {
                DispenseItemBehavior behavior = entry.getValue();
                String kind;
                if (behavior.getClass() == DefaultDispenseItemBehavior.class) {
                    kind = "default";
                } else if (behavior instanceof ProjectileDispenseBehavior) {
                    kind = "projectile";
                } else {
                    kind = "special";
                }
                out.println("dispense " + BuiltInRegistries.ITEM.getKey(entry.getKey()) + " " + kind);
            }
        }
        if (!missing.isEmpty()) {
            throw new IllegalStateException(
                "classes with unclassified redstone behavior: " + String.join(", ", missing));
        }
    }
}
