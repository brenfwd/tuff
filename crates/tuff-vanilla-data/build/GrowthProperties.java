import java.io.PrintWriter;
import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.HashMap;
import java.util.HashSet;
import java.util.List;
import java.util.Map;
import java.util.Optional;
import java.util.Set;
import java.util.TreeSet;

import it.unimi.dsi.fastutil.objects.Object2IntMap;

import net.minecraft.SharedConstants;
import net.minecraft.core.Direction;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.resources.ResourceKey;
import net.minecraft.server.Bootstrap;
import net.minecraft.tags.TagKey;
import net.minecraft.util.random.Weighted;
import net.minecraft.util.random.WeightedList;
import net.minecraft.world.level.block.*;
import net.minecraft.world.level.block.grower.TreeGrower;
import net.minecraft.world.level.block.state.BlockState;

/**
 * Dumps what random ticks, growth, bone meal and fire need that no report
 * contains.
 *
 * Output lines:
 *   ticking <state id ranges>
 *     Block states that tick randomly.
 *   fluid_ticking <state id ranges>
 *     Block states whose fluid ticks randomly.
 *   lava_ignites <state id ranges>
 *     Block states lava sets fire to.
 *   tick <block> <kind>
 *     For every block with a randomly ticking state: what its random tick
 *     does, "-" when nothing. The kind is that of the nearest class in the
 *     block's hierarchy listed in TICK_KINDS; every class declaring a random
 *     tick must be listed there or in TICK_UNMODELED.
 *   bonemeal <block> <kind>
 *     For every block bone meal may act on, the same way with
 *     BONEMEAL_KINDS and BONEMEAL_UNMODELED.
 *   flammable <block> <ignite odds> <burn odds>
 *   grower <name> <shortest tree feature or ->
 *   grower_tree <name> <trees|mega|flower> <weight> <configured feature>
 *   sapling <block> <grower name>
 *   feature <block> <configured feature>
 *     Blocks that place a configured feature when grown (mushrooms, fungi,
 *     moss).
 *   stem <block> <fruit> <attached stem> <stem support tag> <fruit support tag>
 *   attached_stem <block> <stem> <fruit>
 *   growing_plant <head> <body> <growth direction> <grow per tick chance>
 *   weathering <block> <ageing group> <age> <chance modifier> <next block or ->
 *   dead <block> <dead block>
 *     Coral that dies out of water.
 *   fire <block> <damage>
 *     Fire blocks and the damage they deal to what stands in them.
 *   grown <block> <block>
 *     The two block tall plant bone meal grows short grass and ferns into.
 */
public class GrowthProperties {
    private record Kind(String name, Class<?>... classes) {}

    private static final Kind[] TICK_KINDS = {
        new Kind("beetroot", BeetrootBlock.class),
        new Kind("torchflower", TorchflowerCropBlock.class),
        new Kind("crop", CropBlock.class),
        new Kind("pitcher_crop", PitcherCropBlock.class),
        new Kind("nether_wart", NetherWartBlock.class),
        new Kind("farmland", FarmlandBlock.class),
        new Kind("stem", StemBlock.class),
        new Kind("sugar_cane", SugarCaneBlock.class),
        new Kind("cactus", CactusBlock.class),
        new Kind("bamboo", BambooStalkBlock.class),
        new Kind("bamboo_sapling", BambooSaplingBlock.class),
        new Kind("kelp", KelpBlock.class),
        new Kind("cave_vines", CaveVinesBlock.class),
        new Kind("nether_vines", TwistingVinesBlock.class, WeepingVinesBlock.class),
        new Kind("growing_plant_head", GrowingPlantHeadBlock.class),
        new Kind("vine", VineBlock.class),
        new Kind("sweet_berry_bush", SweetBerryBushBlock.class),
        new Kind("cocoa", CocoaBlock.class),
        new Kind("chorus_flower", ChorusFlowerBlock.class),
        new Kind("propagule", MangrovePropaguleBlock.class),
        new Kind("sapling", SaplingBlock.class),
        new Kind("mushroom", MushroomBlock.class),
        new Kind("spreading", SpreadingSnowyBlock.class),
        new Kind("nylium", NyliumBlock.class),
        new Kind("leaves", LeavesBlock.class),
        new Kind("ice", IceBlock.class),
        new Kind("snow_layer", SnowLayerBlock.class),
        new Kind("turtle_egg", TurtleEggBlock.class),
        new Kind("pointed_dripstone", PointedDripstoneBlock.class),
        new Kind("speleothem", SpeleothemBlock.class),
        new Kind("redstone_ore", RedStoneOreBlock.class),
        new Kind("eyeblossom", EyeblossomBlock.class),
        new Kind("dried_ghast", DriedGhastBlock.class),
        new Kind("nether_portal", NetherPortalBlock.class),
        new Kind("budding_amethyst", BuddingAmethystBlock.class),
        new Kind("liquid", LiquidBlock.class),
    };

    private static final Class<?>[] TICK_UNMODELED = {
        FlowerPotBlock.class,
    };

    private static final String[] TICK_BEHAVIOR = {"randomTick", "canGrowInto", "getGrowIntoState"};

    private static final Kind[] BONEMEAL_KINDS = {
        new Kind("beetroot", BeetrootBlock.class),
        new Kind("torchflower", TorchflowerCropBlock.class),
        new Kind("crop", CropBlock.class),
        new Kind("pitcher_crop", PitcherCropBlock.class),
        new Kind("stem", StemBlock.class),
        new Kind("propagule", MangrovePropaguleBlock.class),
        new Kind("sapling", SaplingBlock.class),
        new Kind("azalea", AzaleaBlock.class),
        new Kind("mushroom", MushroomBlock.class),
        new Kind("grass", GrassBlock.class),
        new Kind("tall_grass", TallGrassBlock.class),
        new Kind("tall_flower", TallFlowerBlock.class),
        new Kind("sweet_berry_bush", SweetBerryBushBlock.class),
        new Kind("cocoa", CocoaBlock.class),
        new Kind("cave_vines", CaveVinesBlock.class),
        new Kind("cave_vines_plant", CaveVinesPlantBlock.class),
        new Kind("kelp", KelpBlock.class),
        new Kind("nether_vines", TwistingVinesBlock.class, WeepingVinesBlock.class),
        new Kind("growing_plant_head", GrowingPlantHeadBlock.class),
        new Kind("growing_plant_body", GrowingPlantBodyBlock.class),
        new Kind("sea_pickle", SeaPickleBlock.class),
        new Kind("seagrass", SeagrassBlock.class),
        new Kind("bamboo", BambooStalkBlock.class),
        new Kind("bamboo_sapling", BambooSaplingBlock.class),
        new Kind("flower_bed", FlowerBedBlock.class),
        new Kind("feature_placer", BonemealableFeaturePlacerBlock.class),
        new Kind("rooted_dirt", RootedDirtBlock.class),
        new Kind("nylium", NyliumBlock.class),
        new Kind("netherrack", NetherrackBlock.class),
        new Kind("nether_fungus", NetherFungusBlock.class),
        new Kind("spreading_bush", BushBlock.class, FireflyBushBlock.class),
        new Kind("short_dry_grass", ShortDryGrassBlock.class),
        new Kind("tall_dry_grass", TallDryGrassBlock.class),
        new Kind("hanging_moss", HangingMossBlock.class),
        new Kind("shelf_mushroom", ShelfMushroomBlock.class),
        new Kind("big_dripleaf", BigDripleafBlock.class),
        new Kind("big_dripleaf_stem", BigDripleafStemBlock.class),
        new Kind("small_dripleaf", SmallDripleafBlock.class),
        new Kind("glow_lichen", GlowLichenBlock.class),
        new Kind("mossy_carpet", MossyCarpetBlock.class),
        new Kind("mangrove_leaves", MangroveLeavesBlock.class),
    };

    private static final Class<?>[] BONEMEAL_UNMODELED = {};

    private static final String[] BONEMEAL_BEHAVIOR = {
        "performBonemeal", "isValidBonemealTarget", "isBonemealSuccess", "getBonemealAgeIncrease",
        "getBlocksToGrowWhenBonemealed", "canGrowInto", "getGrowIntoState",
    };

    private static Object field(Class<?> owner, String name, Object target) throws Exception {
        Field field = owner.getDeclaredField(name);
        field.setAccessible(true);
        return field.get(target);
    }

    private static Object call(Class<?> owner, String name, Object target) throws Exception {
        Method method = owner.getDeclaredMethod(name);
        method.setAccessible(true);
        return method.invoke(target);
    }

    private static String name(Block block) {
        return BuiltInRegistries.BLOCK.getKey(block).toString();
    }

    private static String key(ResourceKey<?> key) {
        return key.identifier().toString();
    }

    private static String tag(TagKey<?> tag) {
        return "#" + tag.location().toString();
    }

    private static boolean declares(Class<?> type, String... methods) {
        for (Method method : type.getDeclaredMethods()) {
            if (Arrays.asList(methods).contains(method.getName())) {
                return true;
            }
        }
        return false;
    }

    /**
     * The kind of the nearest listed class; a class on the way that declares
     * a method of `behavior` and is neither listed nor unmodeled is recorded
     * as missing.
     */
    private static String kind(Block block, Map<Class<?>, String> kinds, Set<Class<?>> unmodeled,
            String[] behavior, Set<String> missing) {
        String kind = null;
        for (Class<?> type = block.getClass(); type != Block.class && type != null; type = type.getSuperclass()) {
            String listed = kinds.get(type);
            if (kind == null) {
                kind = listed;
            }
            if (listed != null || unmodeled.contains(type)) {
                continue;
            }
            if (kind == null && ChangeOverTimeBlock.class.isAssignableFrom(type) && declares(type, "randomTick")
                    && behavior == TICK_BEHAVIOR) {
                kind = "weathering";
                continue;
            }
            if (declares(type, behavior)) {
                missing.add(type.getName());
            }
        }
        return kind == null ? "-" : kind;
    }

    private static Map<Class<?>, String> kindMap(Kind[] kinds) {
        Map<Class<?>, String> map = new HashMap<>();
        for (Kind kind : kinds) {
            for (Class<?> type : kind.classes()) {
                if (map.put(type, kind.name()) != null) {
                    throw new IllegalStateException(type.getName() + " has two kinds");
                }
            }
        }
        return map;
    }

    private static String ranges(List<Integer> ids) {
        if (ids.isEmpty()) {
            return "-";
        }
        StringBuilder out = new StringBuilder();
        int start = ids.get(0);
        int end = start;
        for (int i = 1; i <= ids.size(); i++) {
            if (i < ids.size() && ids.get(i) == end + 1) {
                end = ids.get(i);
                continue;
            }
            if (out.length() > 0) {
                out.append(',');
            }
            out.append(start == end ? Integer.toString(start) : start + "-" + end);
            if (i < ids.size()) {
                start = ids.get(i);
                end = start;
            }
        }
        return out.toString();
    }

    private static void trees(PrintWriter out, String grower, String list, WeightedList<ResourceKey<?>> trees) {
        for (Weighted<ResourceKey<?>> tree : trees.unwrap()) {
            out.println("grower_tree " + grower + " " + list + " " + tree.weight() + " " + key(tree.value()));
        }
    }

    @SuppressWarnings("unchecked")
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        try (PrintWriter out = new PrintWriter(Files.newBufferedWriter(Path.of(args[0])))) {
            List<Integer> ticking = new ArrayList<>();
            List<Integer> fluidTicking = new ArrayList<>();
            List<Integer> lavaIgnites = new ArrayList<>();
            int count = Block.BLOCK_STATE_REGISTRY.size();
            for (int id = 0; id < count; id++) {
                BlockState state = Block.BLOCK_STATE_REGISTRY.byId(id);
                if (state.isRandomlyTicking()) {
                    ticking.add(id);
                }
                if (state.getFluidState().isRandomlyTicking()) {
                    fluidTicking.add(id);
                }
                if (state.ignitedByLava()) {
                    lavaIgnites.add(id);
                }
            }
            if (ticking.isEmpty() || fluidTicking.isEmpty()) {
                throw new IllegalStateException("no randomly ticking states");
            }
            out.println("ticking " + ranges(ticking));
            out.println("fluid_ticking " + ranges(fluidTicking));
            out.println("lava_ignites " + ranges(lavaIgnites));

            Map<Class<?>, String> tickKinds = kindMap(TICK_KINDS);
            Map<Class<?>, String> bonemealKinds = kindMap(BONEMEAL_KINDS);
            Set<Class<?>> tickUnmodeled = new HashSet<>(Arrays.asList(TICK_UNMODELED));
            Set<Class<?>> bonemealUnmodeled = new HashSet<>(Arrays.asList(BONEMEAL_UNMODELED));
            Set<String> missing = new TreeSet<>();
            Map<Class<?>, Integer> ageingGroups = new HashMap<>();

            for (Block block : BuiltInRegistries.BLOCK) {
                String name = name(block);
                boolean ticks = block.getStateDefinition().getPossibleStates().stream()
                    .anyMatch(BlockState::isRandomlyTicking);
                if (ticks) {
                    out.println("tick " + name + " "
                        + kind(block, tickKinds, tickUnmodeled, TICK_BEHAVIOR, missing));
                }
                if (block instanceof BonemealableBlock) {
                    out.println("bonemeal " + name + " "
                        + kind(block, bonemealKinds, bonemealUnmodeled, BONEMEAL_BEHAVIOR, missing));
                }
                if (block instanceof SaplingBlock) {
                    TreeGrower grower = (TreeGrower) field(SaplingBlock.class, "treeGrower", block);
                    out.println("sapling " + name + " " + field(TreeGrower.class, "name", grower));
                }
                if (block instanceof MushroomBlock) {
                    out.println("feature " + name + " "
                        + key((ResourceKey<?>) field(MushroomBlock.class, "feature", block)));
                }
                if (block instanceof NetherFungusBlock) {
                    out.println("feature " + name + " "
                        + key((ResourceKey<?>) field(NetherFungusBlock.class, "feature", block)));
                }
                if (block instanceof BonemealableFeaturePlacerBlock) {
                    out.println("feature " + name + " "
                        + key((ResourceKey<?>) field(BonemealableFeaturePlacerBlock.class, "feature", block)));
                }
                if (block instanceof StemBlock) {
                    out.println("stem " + name
                        + " " + key((ResourceKey<?>) field(StemBlock.class, "fruit", block))
                        + " " + key((ResourceKey<?>) field(StemBlock.class, "attachedStem", block))
                        + " " + tag((TagKey<?>) field(StemBlock.class, "stemSupportBlocks", block))
                        + " " + tag((TagKey<?>) field(StemBlock.class, "fruitSupportBlocks", block)));
                }
                if (block instanceof AttachedStemBlock) {
                    out.println("attached_stem " + name
                        + " " + key((ResourceKey<?>) field(AttachedStemBlock.class, "stem", block))
                        + " " + key((ResourceKey<?>) field(AttachedStemBlock.class, "fruit", block)));
                }
                if (block instanceof GrowingPlantHeadBlock head) {
                    Block body = (Block) call(GrowingPlantBlock.class, "getBodyBlock", head);
                    Direction direction = (Direction) field(GrowingPlantBlock.class, "growthDirection", head);
                    double chance = (double) field(GrowingPlantHeadBlock.class, "growPerTickProbability", head);
                    out.println("growing_plant " + name + " " + name(body) + " " + direction.getSerializedName()
                        + " " + chance);
                }
                if (block instanceof ChangeOverTimeBlock<?> ageing) {
                    Enum<?> age = ageing.getAge();
                    Optional<BlockState> next = ageing.getNext(block.defaultBlockState());
                    int group = ageingGroups.computeIfAbsent(age.getDeclaringClass(), c -> ageingGroups.size());
                    out.println("weathering " + name + " " + group
                        + " " + age.ordinal() + " " + ageing.getChanceModifier() + " "
                        + next.map(state -> name(state.getBlock())).orElse("-"));
                }
                if (block instanceof BaseFireBlock) {
                    out.println("fire " + name + " " + field(BaseFireBlock.class, "fireDamage", block));
                }
                if (block instanceof TallGrassBlock) {
                    Method grown = TallGrassBlock.class.getDeclaredMethod("getGrownBlock", BlockState.class);
                    grown.setAccessible(true);
                    out.println("grown " + name + " " + name((Block) grown.invoke(null, block.defaultBlockState())));
                }
                for (Class<?> type : List.of(CoralPlantBlock.class, CoralFanBlock.class, CoralWallFanBlock.class,
                        CoralBlock.class)) {
                    if (type.isInstance(block)) {
                        out.println("dead " + name + " " + name((Block) field(type, "deadBlock", block)));
                    }
                }
            }
            if (!missing.isEmpty()) {
                throw new IllegalStateException("classes with unclassified random tick or bone meal behaviour: "
                    + missing);
            }

            Field growersField = TreeGrower.class.getDeclaredField("GROWERS");
            growersField.setAccessible(true);
            Map<String, TreeGrower> all = (Map<String, TreeGrower>) growersField.get(null);
            for (TreeGrower grower : all.values()) {
                String growerName = (String) field(TreeGrower.class, "name", grower);
                ResourceKey<?> shortest = (ResourceKey<?>) field(TreeGrower.class, "shortestTreeType", grower);
                out.println("grower " + growerName + " " + (shortest == null ? "-" : key(shortest)));
                trees(out, growerName, "trees", (WeightedList<ResourceKey<?>>) field(TreeGrower.class, "trees", grower));
                trees(out, growerName, "mega",
                    (WeightedList<ResourceKey<?>>) field(TreeGrower.class, "megaTrees", grower));
                trees(out, growerName, "flower",
                    (WeightedList<ResourceKey<?>>) field(TreeGrower.class, "flowerTrees", grower));
            }

            FireBlock fire = (FireBlock) Blocks.FIRE;
            Object2IntMap<Block> ignite = (Object2IntMap<Block>) field(FireBlock.class, "igniteOdds", fire);
            Object2IntMap<Block> burn = (Object2IntMap<Block>) field(FireBlock.class, "burnOdds", fire);
            if (ignite.isEmpty()) {
                throw new IllegalStateException("no flammable blocks");
            }
            for (Block block : BuiltInRegistries.BLOCK) {
                int igniteOdds = ignite.getInt(block);
                int burnOdds = burn.getInt(block);
                if (igniteOdds != 0 || burnOdds != 0) {
                    out.println("flammable " + name(block) + " " + igniteOdds + " " + burnOdds);
                }
            }
        }
    }
}
