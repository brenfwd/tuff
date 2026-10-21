import java.io.PrintWriter;
import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.lang.reflect.Proxy;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Arrays;
import java.util.HashMap;
import java.util.HashSet;
import java.util.List;
import java.util.Set;
import java.util.TreeSet;
import java.util.Map;

import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.Direction;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.core.Registry;
import net.minecraft.core.RegistryAccess;
import net.minecraft.core.component.DataComponentInitializers;
import net.minecraft.core.component.DataComponents;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.world.item.ItemStack;
import net.minecraft.world.item.component.BlockItemStateProperties;
import net.minecraft.world.level.LevelReader;
import net.minecraft.server.packs.PackType;
import net.minecraft.server.packs.repository.ServerPacksSource;
import net.minecraft.server.packs.resources.MultiPackResourceManager;
import net.minecraft.tags.TagLoader;
import net.minecraft.util.valueproviders.ConstantInt;
import net.minecraft.util.valueproviders.IntProvider;
import net.minecraft.util.valueproviders.UniformInt;
import net.minecraft.world.entity.EntityTypes;
import net.minecraft.world.item.BlockItem;
import net.minecraft.world.item.Item;
import net.minecraft.world.item.SolidBucketItem;
import net.minecraft.world.item.StandingAndWallBlockItem;
import net.minecraft.sounds.SoundEvent;
import net.minecraft.world.level.block.state.properties.BlockSetType;
import net.minecraft.world.level.block.state.properties.NoteBlockInstrument;
import net.minecraft.world.level.block.state.properties.WoodType;
import net.minecraft.world.level.material.Fluids;
import net.minecraft.world.level.BlockGetter;
import net.minecraft.world.level.block.*;
import net.minecraft.world.level.block.entity.BlockEntity;
import net.minecraft.world.level.block.piston.PistonBaseBlock;
import net.minecraft.world.level.block.piston.PistonHeadBlock;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.material.FluidState;
import net.minecraft.world.level.block.state.BlockBehaviour;
import net.minecraft.world.phys.AABB;
import net.minecraft.world.phys.Vec3;
import net.minecraft.world.phys.shapes.BooleanOp;
import net.minecraft.world.phys.shapes.Shapes;
import net.minecraft.world.phys.shapes.VoxelShape;

/**
 * Dumps block and block item data that no data generator report contains,
 * other than the light engine's (see LightProperties.java).
 *
 * Output lines:
 *   state <id> <requires correct tool 0/1> <destroy speed, -1 = unbreakable>
 *         <sturdy faces> <flags> <full faces> <break sound> <step sound> <place sound>
 *         <hit sound> <fall sound> <sound volume> <sound pitch>
 *         <collision boxes: "-" or minX,minY,minZ,maxX,maxY,maxZ;...>
 *         <outline boxes, the same way>
 *     Boxes of blocks with a per-position offset are listed unshifted.
 *   offset <name> <max horizontal offset> <max vertical offset, 0 if none>
 *          <collision shape shifted 0/1> <outline shape shifted 0/1>
 *     Blocks whose shapes the game shifts by a hash of the position.
 *   offset_sample <name> <x> <z> <offset x> <offset y> <offset z>
 *     The game's offset for that block at a few columns.
 *     sturdy faces: bit (direction * 3 + support), directions down, up,
 *       north, south, west, east; supports full, center, rigid.
 *     flags: 1 replaceable, 2 solid, 4 solid render, 8 full collision cube,
 *       16 liquid, then 32 << i: the bottom of the collision shape covers a
 *       wall's post (i = 0) or its north, east, south, west side (i = 1..4),
 *       then 1024 water may replace it, 2048 lava may replace it, 4096 it
 *       is dangerous for a player to stand in or on, 8192 it suffocates,
 *       16384 a player may respawn inside it.
 *     full faces: bit d when the face occlusion shape toward direction d is
 *       the full block, bit 6 + d when the collision shape covers the whole
 *       face toward d; directions as for sturdy faces. Bit 12 when the
 *       outline shape is the full block, bit 13 when the block has no dynamic
 *       shape and its collision shape is the full block (how structure
 *       templates sort their blocks).
 *   push <name> <push_pull|push|popped|immoveable|ignore_entity>
 *     How a piston moving the block treats it; blocks without a line are
 *     push_pull.
 *   base <name> <block>
 *     The block a dirt path or farmland turns into when covered.
 *   falling <name> <delay ticks>
 *     The block falls when unsupported (sand, gravel, anvils, ...); delay
 *     ticks is how long after placement/support loss the fall check runs.
 *   entity <name> <block entity type> <randomizable container 0/1>
 *     The block entity a block gets when placed.
 *   transform <id> <id rotated clockwise> <id mirrored left-right>
 *         <id mirrored front-back>
 *     Only for states that some rotation or mirror changes.
 *   block <name> <kind, or - for none> <loot table id, or - for none>
 *         <experience dropped when mined: -, c<n>, u<min>-<max>, or
 *          s<base>+<bound>+... for a base plus random(bound) terms>
 *         <explosion resistance> <note block instrument it gives>
 *   ground <plant name> <state id ranges, e.g. 1-5,9>
 *   item <item name> <block name> [<wall block name> <attachment direction>]
 *   instrument <name> <sound> <tunable 0/1> <works above a note block 0/1>
 *              <custom sound 0/1>
 *     One per note block instrument in property order.
 *   toggle <block> <open sound> <close sound> <opens by hand 0/1>
 *     Doors, trapdoors and fence gates.
 *   button <block> <ticks it stays pressed> <press sound> <release sound>
 *   pickup <block> <liquid|waterlogged|whole> <item taken> <sound or ->
 *     Blocks a bucket can take from.
 *   bed <block> <bed rule environment attribute> <explode|remove>
 *     What using the bed does where its rule destroys it.
 *   campfire <block> <damage to living entities inside it while lit>
 */
public class BlockProperties {
    private static final Set<String> UNCONSTRUCTIBLE_BLOCK_ENTITIES = Set.of("minecraft:vault");

    private static final Direction[] DIRECTIONS = {
        Direction.DOWN, Direction.UP, Direction.NORTH, Direction.SOUTH, Direction.WEST, Direction.EAST,
    };
    private static final SupportType[] SUPPORTS = {SupportType.FULL, SupportType.CENTER, SupportType.RIGID};

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

    private record Kind(String name, Class<?>... classes) {}

    /**
     * A block's kind is that of the nearest class in its hierarchy listed
     * here. Every class declaring one of BEHAVIOR must be listed here or in
     * UNMODELED, so a new override fails the build instead of being missed.
     */
    private static final Kind[] KINDS = {
        new Kind("double_plant", DoublePlantBlock.class, PitcherCropBlock.class, SmallDripleafBlock.class, TallSeagrassBlock.class),
        new Kind("leaf_litter", LeafLitterBlock.class),
        new Kind("flower_bed", FlowerBedBlock.class),
        new Kind("sea_pickle", SeaPickleBlock.class),
        new Kind("seagrass", SeagrassBlock.class),
        new Kind("mushroom", MushroomBlock.class),
        new Kind("propagule", MangrovePropaguleBlock.class),
        new Kind("plant", VegetationBlock.class, CropBlock.class, AttachedStemBlock.class),
        new Kind("wall_torch", WallTorchBlock.class, RedstoneWallTorchBlock.class),
        new Kind("torch", BaseTorchBlock.class),
        new Kind("carpet", CarpetBlock.class),
        new Kind("mossy_carpet", MossyCarpetBlock.class),
        new Kind("pressure_plate", BasePressurePlateBlock.class),
        new Kind("rail", BaseRailBlock.class),
        new Kind("grindstone", GrindstoneBlock.class),
        new Kind("face_attached", FaceAttachedHorizontalDirectionalBlock.class),
        new Kind("door", DoorBlock.class),
        new Kind("bed", AbstractBedBlock.class),
        new Kind("lantern", LanternBlock.class),
        new Kind("snow_layer", SnowLayerBlock.class),
        new Kind("cactus", CactusBlock.class),
        new Kind("sugar_cane", SugarCaneBlock.class),
        new Kind("bamboo", BambooStalkBlock.class, BambooSaplingBlock.class),
        new Kind("diode", DiodeBlock.class, RepeaterBlock.class, ComparatorBlock.class),
        new Kind("redstone_wire", RedstoneWireBlock.class),
        new Kind("candle", CandleBlock.class),
        new Kind("cake", CakeBlock.class, CandleCakeBlock.class),
        new Kind("coral_wall_fan", BaseCoralWallFanBlock.class, CoralWallFanBlock.class),
        new Kind("coral", BaseCoralPlantTypeBlock.class, CoralFanBlock.class, CoralPlantBlock.class),
        new Kind("amethyst_cluster", AmethystClusterBlock.class),
        new Kind("hanging_roots", HangingRootsBlock.class),
        new Kind("spore_blossom", SporeBlossomBlock.class),
        new Kind("cocoa", CocoaBlock.class),
        new Kind(
            "growing_plant",
            GrowingPlantBlock.class, GrowingPlantHeadBlock.class, GrowingPlantBodyBlock.class, KelpBlock.class),
        new Kind("standing_sign", StandingSignBlock.class, BannerBlock.class),
        new Kind("wall_sign", WallSignBlock.class, WallBannerBlock.class),
        new Kind("ceiling_hanging_sign", CeilingHangingSignBlock.class),
        new Kind("wall_hanging_sign", WallHangingSignBlock.class),
        new Kind("wall_skull", WallSkullBlock.class),
        new Kind("skull", SkullBlock.class),
        new Kind("wall_sturdy", LadderBlock.class, TripWireHookBlock.class, ShelfMushroomBlock.class),
        new Kind(
            "facing_opposite",
            AbstractFurnaceBlock.class, BeehiveBlock.class, ChiseledBookShelfBlock.class,
            CarvedPumpkinBlock.class, EndPortalFrameBlock.class, EnderChestBlock.class,
            GlazedTerracottaBlock.class, LecternBlock.class, LoomBlock.class, StonecutterBlock.class,
            VaultBlock.class, ShelfBlock.class, CopperGolemStatueBlock.class, DriedGhastBlock.class,
            ChestBlock.class, CopperChestBlock.class, BigDripleafBlock.class),
        new Kind("facing_player", CampfireBlock.class, CalibratedSculkSensorBlock.class, DecoratedPotBlock.class),
        new Kind("anvil", AnvilBlock.class),
        new Kind("facing_look_opposite", BarrelBlock.class, CommandBlock.class, DispenserBlock.class, PistonBaseBlock.class),
        new Kind("observer", ObserverBlock.class),
        new Kind("crafter", CrafterBlock.class),
        new Kind("facing_clicked", ShulkerBoxBlock.class, LightningRodBlock.class),
        new Kind("end_rod", EndRodBlock.class),
        new Kind("hopper", HopperBlock.class),
        new Kind("jigsaw", JigsawBlock.class),
        new Kind("stairs", StairBlock.class),
        new Kind("slab", SlabBlock.class),
        new Kind("trapdoor", TrapDoorBlock.class),
        new Kind("fence_gate", FenceGateBlock.class),
        new Kind("fence", FenceBlock.class),
        new Kind("bars", IronBarsBlock.class),
        new Kind("wall", WallBlock.class),
        new Kind("pillar", RotatedPillarBlock.class, ChainBlock.class, InfestedRotatedPillarBlock.class, CreakingHeartBlock.class),
        new Kind("leaves", LeavesBlock.class),
        new Kind("snowy", SnowyBlock.class),
        new Kind("huge_mushroom", HugeMushroomBlock.class),
        new Kind("turtle_egg", TurtleEggBlock.class),
        new Kind("vine", VineBlock.class),
        new Kind("multiface", MultifaceBlock.class),
        new Kind("note_block", NoteBlock.class),
        new Kind("path", PathBlock.class),
        new Kind("farmland", FarmlandBlock.class),
    };

    /** How each bed class goes when its rule destroys it on use. */
    private static final Map<Class<?>, String> BED_DESTRUCTION = Map.of(
        BedBlock.class, "explode",
        StrawBedBlock.class, "remove");

    private static String boxes(VoxelShape shape) {
        StringBuilder boxes = new StringBuilder();
        List<AABB> aabbs = shape.toAabbs();
        for (AABB box : aabbs) {
            if (boxes.length() > 0) {
                boxes.append(';');
            }
            boxes.append(box.minX).append(',').append(box.minY).append(',').append(box.minZ).append(',')
                .append(box.maxX).append(',').append(box.maxY).append(',').append(box.maxZ);
        }
        return aabbs.isEmpty() ? "-" : boxes.toString();
    }

    /** The column within a chunk whose offset differs most from the origin's. */
    private static BlockPos farthestOffset(BlockState state) {
        Vec3 origin = state.getOffset(BlockPos.ZERO);
        BlockPos best = BlockPos.ZERO;
        double far = -1.0;
        for (int x = 0; x < 16; x++) {
            for (int z = 0; z < 16; z++) {
                BlockPos pos = new BlockPos(x, 0, z);
                Vec3 d = state.getOffset(pos).subtract(origin);
                double away = Math.min(Math.abs(d.x), Math.abs(d.z));
                if (away > far) {
                    far = away;
                    best = pos;
                }
            }
        }
        if (far <= 0.0) {
            throw new IllegalStateException(state + " has an offset that never moves");
        }
        return best;
    }

    /** Whether a shape follows the offset between two positions; fails if it neither follows nor stays. */
    private static boolean moves(VoxelShape here, VoxelShape there, Vec3 by, BlockState state) {
        if (here.isEmpty() || sameBoxes(here, there)) {
            return false;
        }
        if (sameBoxes(here.move(by), there)) {
            return true;
        }
        throw new IllegalStateException(state + " has a shape that neither follows its offset nor stays");
    }

    private static boolean sameBoxes(VoxelShape a, VoxelShape b) {
        List<AABB> left = a.toAabbs();
        List<AABB> right = b.toAabbs();
        if (left.size() != right.size()) {
            return false;
        }
        for (int i = 0; i < left.size(); i++) {
            AABB l = left.get(i);
            AABB r = right.get(i);
            double[] d = {l.minX - r.minX, l.minY - r.minY, l.minZ - r.minZ, l.maxX - r.maxX, l.maxY - r.maxY, l.maxZ - r.maxZ};
            for (double v : d) {
                if (Math.abs(v) > 1.0e-9) {
                    return false;
                }
            }
        }
        return true;
    }

    private static String sound(SoundEvent sound) {
        return sound.location().toString();
    }

    private static Object field(Class<?> owner, String name, Object target) throws Exception {
        Field field = owner.getDeclaredField(name);
        field.setAccessible(true);
        return field.get(target);
    }

    private static final String[] BEHAVIOR = {"getStateForPlacement", "canSurvive", "updateShape", "canBeReplaced"};

    /**
     * Blocks whose experience drop is written inline in game code rather than
     * held in an IntProvider field, so it cannot be read reflectively. A block
     * class that overrides the break hook and is in neither group fails the
     * build so a new version cannot silently drop experience.
     */
    private static final Map<Class<?>, String> INLINE_EXPERIENCE = Map.of(
        RedStoneOreBlock.class, "u1-5",
        SculkSensorBlock.class, "c5",
        SculkShriekerBlock.class, "c5",
        SpawnerBlock.class, "s15+15+15",
        InfestedBlock.class, "-");
    private static final String BREAK_HOOK = "spawnAfterBreak";

    private static String formatExperience(IntProvider provider) {
        if (provider instanceof ConstantInt constant) {
            return "c" + constant.value();
        }
        if (provider instanceof UniformInt uniform) {
            return "u" + uniform.minInclusive() + "-" + uniform.maxInclusive();
        }
        throw new IllegalStateException("unsupported experience provider " + provider);
    }

    private static String experience(Block block, Set<String> unhandled) throws Exception {
        for (Class<?> type = block.getClass(); type != Block.class; type = type.getSuperclass()) {
            boolean overrides = false;
            for (Method method : type.getDeclaredMethods()) {
                if (method.getName().equals(BREAK_HOOK)) {
                    overrides = true;
                }
            }
            if (!overrides) {
                continue;
            }
            for (Field field : type.getDeclaredFields()) {
                if (IntProvider.class.isAssignableFrom(field.getType())) {
                    field.setAccessible(true);
                    return formatExperience((IntProvider) field.get(block));
                }
            }
            String inline = INLINE_EXPERIENCE.get(type);
            if (inline == null) {
                unhandled.add(type.getName());
                return "-";
            }
            return inline;
        }
        return "-";
    }

    /** Classes whose placement or survival overrides are deliberately not modeled. */
    private static final Class<?>[] UNMODELED = {
        AbstractSkullBlock.class, BarrierBlock.class, BaseFireBlock.class, BellBlock.class,
        BigDripleafStemBlock.class, BrushableBlock.class, BubbleColumnBlock.class, ChorusFlowerBlock.class,
        ChorusPlantBlock.class, ConcretePowderBlock.class, ConduitBlock.class, CoralBlock.class,
        EndGatewayBlock.class, EndPortalBlock.class, FallingBlock.class, FireBlock.class,
        FlowerPotBlock.class, FrogspawnBlock.class, HangingMossBlock.class, HeavyCoreBlock.class,
        LightBlock.class, LiquidBlock.class, MangroveRootsBlock.class, NetherPortalBlock.class,
        PistonHeadBlock.class, PotentSulfurBlock.class, RedstoneLampBlock.class,
        ScaffoldingBlock.class, SculkSensorBlock.class, SculkShriekerBlock.class, SignBlock.class,
        SoulFireBlock.class, SpeleothemBlock.class, TestBlock.class, TripWireBlock.class,
        WaterloggedTransparentBlock.class,
    };

    private static String kind(Block block, Map<Class<?>, String> kinds, Set<Class<?>> unmodeled, Set<String> missing) {
        String kind = null;
        for (Class<?> type = block.getClass(); type != Block.class; type = type.getSuperclass()) {
            String listed = kinds.get(type);
            if (kind == null) {
                kind = listed;
            }
            if (listed != null || unmodeled.contains(type)) {
                continue;
            }
            for (Method method : type.getDeclaredMethods()) {
                if (Arrays.asList(BEHAVIOR).contains(method.getName())) {
                    missing.add(type.getName());
                }
            }
        }
        return kind == null ? "-" : kind;
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
        BuiltInRegistries.DATA_COMPONENT_INITIALIZERS.build(VanillaRegistries.createWorldLookup())
            .forEach(DataComponentInitializers.PendingComponents::apply);

        VoxelShape post = Block.column(2.0, 0.0, 16.0);
        Map<Direction, VoxelShape> sides = Shapes.rotateHorizontal(Block.boxZ(2.0, 16.0, 0.0, 9.0));
        VoxelShape[] covers = {
            post, sides.get(Direction.NORTH), sides.get(Direction.EAST), sides.get(Direction.SOUTH), sides.get(Direction.WEST),
        };

        try (PrintWriter out = new PrintWriter(Files.newBufferedWriter(Path.of(args[0])))) {
            int count = Block.BLOCK_STATE_REGISTRY.size();
            Map<Block, Integer> offsetShapes = new HashMap<>();
            for (int id = 0; id < count; id++) {
                BlockState state = Block.BLOCK_STATE_REGISTRY.byId(id);
                One level = new One(state);
                BlockPos pos = BlockPos.ZERO;
                float destroySpeed = state.getDestroySpeed(level, pos);

                int sturdy = 0;
                for (int d = 0; d < DIRECTIONS.length; d++) {
                    for (int s = 0; s < SUPPORTS.length; s++) {
                        if (state.isFaceSturdy(level, pos, DIRECTIONS[d], SUPPORTS[s])) {
                            sturdy |= 1 << (d * 3 + s);
                        }
                    }
                }

                VoxelShape collision = state.getCollisionShape(level, pos);
                VoxelShape outline = state.getShape(level, pos);
                if (state.hasOffsetFunction()) {
                    Vec3 shift = state.getOffset(pos);
                    BlockPos probe = farthestOffset(state);
                    Vec3 there = state.getOffset(probe).subtract(shift);
                    boolean collisionMoves = moves(collision, state.getCollisionShape(level, probe), there, state);
                    boolean outlineMoves = moves(outline, state.getShape(level, probe), there, state);
                    if (collisionMoves) {
                        collision = collision.move(shift.reverse());
                    }
                    if (outlineMoves) {
                        outline = outline.move(shift.reverse());
                    }
                    int kinds = (collisionMoves ? 1 : 0) | (outlineMoves ? 2 : 0);
                    Integer seen = offsetShapes.putIfAbsent(state.getBlock(), kinds);
                    if (seen != null && seen != kinds) {
                        throw new IllegalStateException(state + " offsets its shapes unlike its other states");
                    }
                }
                int flags = (state.canBeReplaced() ? 1 : 0)
                    | (state.isSolid() ? 2 : 0)
                    | (state.isSolidRender() ? 4 : 0)
                    | (state.isCollisionShapeFullBlock(level, pos) ? 8 : 0)
                    | (state.liquid() ? 16 : 0);
                VoxelShape bottom = collision.getFaceShape(Direction.DOWN);
                for (int i = 0; i < covers.length; i++) {
                    if (!Shapes.joinIsNotEmpty(covers[i], bottom, BooleanOp.ONLY_FIRST)) {
                        flags |= 32 << i;
                    }
                }

                if (state.canBeReplaced(Fluids.WATER)) {
                    flags |= 1024;
                }
                if (state.canBeReplaced(Fluids.LAVA)) {
                    flags |= 2048;
                }
                if (EntityTypes.PLAYER.isBlockDangerous(state)) {
                    flags |= 4096;
                }
                if (state.isSuffocating(level, pos)) {
                    flags |= 8192;
                }
                if (state.getBlock().isPossibleToRespawnInThis(state)) {
                    flags |= 16384;
                }

                int fullFaces = 0;
                for (int d = 0; d < DIRECTIONS.length; d++) {
                    if (Block.isShapeFullBlock(state.getFaceOcclusionShape(DIRECTIONS[d]))) {
                        fullFaces |= 1 << d;
                    }
                    if (Block.isFaceFull(collision, DIRECTIONS[d])) {
                        fullFaces |= 1 << (6 + d);
                    }
                }
                if (Block.isShapeFullBlock(state.getShape(level, pos))) {
                    fullFaces |= 1 << 12;
                }
                if (!state.getBlock().hasDynamicShape()
                    && state.isCollisionShapeFullBlock(net.minecraft.world.level.EmptyBlockGetter.INSTANCE, BlockPos.ZERO)) {
                    fullFaces |= 1 << 13;
                }

                SoundType sound = state.getSoundType();
                out.println("state " + id + " " + (state.requiresCorrectToolForDrops() ? 1 : 0) + " " + destroySpeed
                    + " " + sturdy + " " + flags + " " + fullFaces + " " + sound(sound.getBreakSound()) + " " + sound(sound.getStepSound())
                    + " " + sound(sound.getPlaceSound()) + " " + sound(sound.getHitSound()) + " "
                    + sound(sound.getFallSound()) + " " + sound.getVolume() + " " + sound.getPitch() + " "
                    + boxes(collision) + " " + boxes(outline));
                int rotated = Block.BLOCK_STATE_REGISTRY.getId(state.rotate(Rotation.CLOCKWISE_90));
                int leftRight = Block.BLOCK_STATE_REGISTRY.getId(state.mirror(Mirror.LEFT_RIGHT));
                int frontBack = Block.BLOCK_STATE_REGISTRY.getId(state.mirror(Mirror.FRONT_BACK));
                if (rotated != id || leftRight != id || frontBack != id) {
                    out.println("transform " + id + " " + rotated + " " + leftRight + " " + frontBack);
                }
            }

            for (NoteBlockInstrument instrument : NoteBlockInstrument.values()) {
                out.println("instrument " + instrument.getSerializedName() + " "
                    + instrument.getSoundEvent().unwrapKey().orElseThrow().identifier() + " "
                    + (instrument.isTunable() ? 1 : 0) + " " + (instrument.worksAboveNoteBlock() ? 1 : 0) + " "
                    + (instrument.hasCustomSound() ? 1 : 0));
            }

            Method mayPlaceOn = VegetationBlock.class.getDeclaredMethod(
                "mayPlaceOn", BlockState.class, BlockGetter.class, BlockPos.class);
            mayPlaceOn.setAccessible(true);
            Method fallingDelay = FallingBlock.class.getDeclaredMethod("getDelayAfterPlace");
            fallingDelay.setAccessible(true);
            Field growthDirection = GrowingPlantBlock.class.getDeclaredField("growthDirection");
            growthDirection.setAccessible(true);
            Map<Class<?>, String> kinds = new HashMap<>();
            for (Kind kind : KINDS) {
                for (Class<?> type : kind.classes()) {
                    if (kinds.put(type, kind.name()) != null) {
                        throw new IllegalStateException(type.getName() + " has two kinds");
                    }
                }
            }
            Set<Class<?>> unmodeled = new HashSet<>(Arrays.asList(UNMODELED));
            Set<String> missing = new TreeSet<>();
            Set<String> unhandledExperience = new TreeSet<>();
            for (Block block : BuiltInRegistries.BLOCK) {
                String name = BuiltInRegistries.BLOCK.getKey(block).toString();
                String kind = kind(block, kinds, unmodeled, missing);
                if (block instanceof GrowingPlantBlock) {
                    kind += "_" + ((Direction) growthDirection.get(block)).getSerializedName();
                }
                String loot = block.getLootTable().map(key -> key.identifier().toString()).orElse("-");
                NoteBlockInstrument instrument = block.defaultBlockState().instrument();
                for (BlockState state : block.getStateDefinition().getPossibleStates()) {
                    if (state.instrument() != instrument) {
                        throw new IllegalStateException(name + " gives different instruments per state");
                    }
                }
                out.println("block " + name + " " + kind + " " + loot + " " + experience(block, unhandledExperience)
                    + " " + block.getExplosionResistance() + " " + instrument.getSerializedName());

                String push = block.defaultBlockState().getPistonPushReaction().name().toLowerCase(java.util.Locale.ROOT);
                for (BlockState state : block.getStateDefinition().getPossibleStates()) {
                    if (!state.getPistonPushReaction().name().toLowerCase(java.util.Locale.ROOT).equals(push)) {
                        throw new IllegalStateException(name + " gives different push reactions per state");
                    }
                }
                if (!push.equals("push_pull")) {
                    out.println("push " + name + " " + push);
                }

                for (var type : BuiltInRegistries.BLOCK_ENTITY_TYPE) {
                    if (!type.isValid(block.defaultBlockState())) {
                        continue;
                    }
                    String typeName = BuiltInRegistries.BLOCK_ENTITY_TYPE.getKey(type).toString();
                    boolean randomizable;
                    try {
                        randomizable = type.create(BlockPos.ZERO, block.defaultBlockState())
                            instanceof net.minecraft.world.RandomizableContainer;
                    } catch (Throwable e) {
                        // Needs bound item components to construct; none of
                        // these is a loot container.
                        if (!UNCONSTRUCTIBLE_BLOCK_ENTITIES.contains(typeName)) {
                            throw new IllegalStateException("cannot construct block entity " + typeName, e);
                        }
                        randomizable = false;
                    }
                    out.println("entity " + name + " " + typeName + " " + (randomizable ? 1 : 0));
                    break;
                }

                if (block instanceof FallingBlock fallingBlock) {
                    int delay = (int) fallingDelay.invoke(fallingBlock);
                    out.println("falling " + name + " " + delay);
                }

                if (block instanceof InfestedBlock infested) {
                    out.println("infested " + name + " " + BuiltInRegistries.BLOCK.getKey(infested.getHostBlock()));
                }

                if (block instanceof PathBlock) {
                    out.println("base " + name + " " + BuiltInRegistries.BLOCK.getKey((Block) field(PathBlock.class, "baseBlock", block)));
                } else if (block instanceof FarmlandBlock) {
                    out.println("base " + name + " " + BuiltInRegistries.BLOCK.getKey((Block) field(FarmlandBlock.class, "baseBlock", block)));
                }
                if (block.defaultBlockState().hasOffsetFunction() && offsetShapes.get(block) != 0) {
                    Method maxHorizontal = BlockBehaviour.class.getDeclaredMethod("getMaxHorizontalOffset");
                    maxHorizontal.setAccessible(true);
                    Method maxVertical = BlockBehaviour.class.getDeclaredMethod("getMaxVerticalOffset");
                    maxVertical.setAccessible(true);
                    boolean vertical = false;
                    for (int x = 0; x < 16 && !vertical; x++) {
                        for (int z = 0; z < 16 && !vertical; z++) {
                            vertical = block.defaultBlockState().getOffset(new BlockPos(x, 0, z)).y != 0.0;
                        }
                    }
                    int shifted = offsetShapes.get(block);
                    out.println("offset " + name + " " + maxHorizontal.invoke(block) + " "
                        + (vertical ? maxVertical.invoke(block) : 0.0f) + " " + (shifted & 1) + " " + (shifted >> 1));
                    int[][] samples = {{0, 0}, {1, 0}, {0, 1}, {5, 9}, {-3, 7}, {123456, -654321}, {-29999999, 29999999}};
                    for (int[] at : samples) {
                        Vec3 shift = block.defaultBlockState().getOffset(new BlockPos(at[0], 0, at[1]));
                        out.println("offset_sample " + name + " " + at[0] + " " + at[1] + " " + shift.x + " " + shift.y + " " + shift.z);
                    }
                }
                if (block instanceof CampfireBlock) {
                    out.println("campfire " + name + " " + field(CampfireBlock.class, "fireDamage", block));
                }

                if (block instanceof DoorBlock door) {
                    BlockSetType type = door.type();
                    out.println("toggle " + name + " " + sound(type.doorOpen()) + " " + sound(type.doorClose()) + " "
                        + (type.canOpenByHand() ? 1 : 0));
                } else if (block instanceof TrapDoorBlock) {
                    BlockSetType type = (BlockSetType) field(TrapDoorBlock.class, "type", block);
                    out.println("toggle " + name + " " + sound(type.trapdoorOpen()) + " " + sound(type.trapdoorClose())
                        + " " + (type.canOpenByHand() ? 1 : 0));
                } else if (block instanceof FenceGateBlock) {
                    WoodType type = (WoodType) field(FenceGateBlock.class, "type", block);
                    out.println("toggle " + name + " " + sound(type.fenceGateOpen()) + " " + sound(type.fenceGateClose())
                        + " 1");
                } else if (block instanceof ButtonBlock) {
                    BlockSetType type = (BlockSetType) field(ButtonBlock.class, "type", block);
                    out.println("button " + name + " " + field(ButtonBlock.class, "ticksToStayPressed", block) + " "
                        + sound(type.buttonClickOn()) + " " + sound(type.buttonClickOff()));
                }

                if (block instanceof BucketPickup pickup) {
                    String kind2;
                    Item taken = null;
                    if (block instanceof LiquidBlock) {
                        kind2 = "liquid";
                    } else if (block instanceof SimpleWaterloggedBlock) {
                        kind2 = "waterlogged";
                        taken = Fluids.WATER.getBucket();
                    } else {
                        kind2 = "whole";
                        for (Item item : BuiltInRegistries.ITEM) {
                            if (item instanceof SolidBucketItem bucket && bucket.getBlock() == block) {
                                taken = item;
                            }
                        }
                    }
                    if (taken == null) {
                        taken = block.defaultBlockState().getFluidState().getType().getBucket();
                    }
                    if (taken == null || taken == net.minecraft.world.item.Items.AIR) {
                        throw new IllegalStateException(name + " is a bucket pickup with no bucket");
                    }
                    out.println("pickup " + name + " " + kind2 + " " + BuiltInRegistries.ITEM.getKey(taken) + " "
                        + pickup.getPickupSound().map(BlockProperties::sound).orElse("-"));
                }

                if (block instanceof AbstractBedBlock) {
                    String destruction = BED_DESTRUCTION.get(block.getClass());
                    if (destruction == null) {
                        throw new IllegalStateException("unclassified bed " + block.getClass().getName());
                    }
                    Method attribute = AbstractBedBlock.class.getDeclaredMethod("getBedEnvironmentAttribute");
                    attribute.setAccessible(true);
                    Object key = BuiltInRegistries.ENVIRONMENT_ATTRIBUTE.getKey(
                        (net.minecraft.world.attribute.EnvironmentAttribute<?>) attribute.invoke(block));
                    out.println("bed " + name + " " + key + " " + destruction);
                }

                if (block instanceof VegetationBlock) {
                    StringBuilder ranges = new StringBuilder();
                    int start = -1;
                    for (int id = 0; id <= count; id++) {
                        boolean supports = false;
                        if (id < count) {
                            BlockState ground = Block.BLOCK_STATE_REGISTRY.byId(id);
                            supports = (Boolean) mayPlaceOn.invoke(block, ground, new One(ground), BlockPos.ZERO);
                        }
                        if (supports && start < 0) {
                            start = id;
                        } else if (!supports && start >= 0) {
                            if (ranges.length() > 0) {
                                ranges.append(',');
                            }
                            ranges.append(start);
                            if (id - 1 > start) {
                                ranges.append('-').append(id - 1);
                            }
                            start = -1;
                        }
                    }
                    out.println("ground " + name + " " + (ranges.length() == 0 ? "-" : ranges));
                }
            }

            if (!missing.isEmpty()) {
                throw new IllegalStateException(
                    "classes with unclassified placement or survival behavior: " + String.join(", ", missing));
            }
            if (!unhandledExperience.isEmpty()) {
                throw new IllegalStateException(
                    "classes overriding " + BREAK_HOOK + " with no readable experience: "
                        + String.join(", ", unhandledExperience));
            }

            RegistryAccess access = RegistryAccess.fromRegistryOfRegistries(BuiltInRegistries.REGISTRY);
            LevelReader pickLevel = (LevelReader) Proxy.newProxyInstance(
                LevelReader.class.getClassLoader(), new Class<?>[] {LevelReader.class},
                (proxy, method, margs) -> switch (method.getName()) {
                    case "registryAccess" -> access;
                    case "getBlockEntity" -> null;
                    default -> throw new UnsupportedOperationException("pick level: " + method);
                });
            for (Block block : BuiltInRegistries.BLOCK) {
                String name = BuiltInRegistries.BLOCK.getKey(block).toString();
                Item own = block.asItem();
                Set<String> copied = new TreeSet<>();
                boolean always = false;
                BlockItemStateProperties own_default = own.components().get(DataComponents.BLOCK_STATE);
                for (BlockState state : block.getStateDefinition().getPossibleStates()) {
                    ItemStack plain = state.getCloneItemStack(pickLevel, BlockPos.ZERO, false);
                    if (plain.getItem() != own) {
                        out.println("pick " + Block.getId(state) + " "
                            + (plain.isEmpty() ? "-" : BuiltInRegistries.ITEM.getKey(plain.getItem())));
                    }
                    if (plain.isEmpty()) {
                        continue;
                    }
                    BlockItemStateProperties prototype = plain.getItem().components().get(DataComponents.BLOCK_STATE);
                    BlockItemStateProperties full =
                        state.getCloneItemStack(pickLevel, BlockPos.ZERO, true).get(DataComponents.BLOCK_STATE);
                    if (full != null && !full.equals(prototype)) {
                        copied.addAll(full.properties().keySet());
                    }
                    BlockItemStateProperties bare = plain.get(DataComponents.BLOCK_STATE);
                    if (bare != null && !bare.equals(prototype)) {
                        always = true;
                    }
                }
                if (!copied.isEmpty() || (own_default != null && !own_default.isEmpty())) {
                    String prototype = "-";
                    if (own_default != null && !own_default.isEmpty()) {
                        List<String> pairs = new java.util.ArrayList<>();
                        new java.util.TreeMap<>(own_default.properties()).forEach((k, v) -> pairs.add(k + "=" + v));
                        prototype = String.join(",", pairs);
                    }
                    out.println("pick_state " + name + " " + (always ? 1 : 0) + " "
                        + (copied.isEmpty() ? "-" : String.join(",", copied)) + " " + prototype);
                }
            }

            Field wallBlock = StandingAndWallBlockItem.class.getDeclaredField("wallBlock");
            wallBlock.setAccessible(true);
            Field attachment = StandingAndWallBlockItem.class.getDeclaredField("attachmentDirection");
            attachment.setAccessible(true);
            for (Item item : BuiltInRegistries.ITEM) {
                if (!(item instanceof BlockItem blockItem)) {
                    continue;
                }
                String line = "item " + BuiltInRegistries.ITEM.getKey(item) + " "
                    + BuiltInRegistries.BLOCK.getKey(blockItem.getBlock());
                if (item instanceof StandingAndWallBlockItem) {
                    Block wall = (Block) wallBlock.get(item);
                    line += " " + BuiltInRegistries.BLOCK.getKey(wall) + " "
                        + ((Direction) attachment.get(item)).getSerializedName();
                }
                out.println(line);
            }
        }
    }
}
