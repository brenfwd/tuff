import java.io.PrintWriter;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.List;

import com.google.gson.JsonElement;
import com.mojang.serialization.JsonOps;

import net.minecraft.SharedConstants;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.attribute.EnvironmentAttribute;
import net.minecraft.world.attribute.EnvironmentAttributeMap;
import net.minecraft.world.attribute.EnvironmentAttributes;
import net.minecraft.world.attribute.WeatherAttributes;
import net.minecraft.world.attribute.modifier.FloatModifier;
import net.minecraft.world.attribute.modifier.FloatWithAlpha;
import net.minecraft.world.clock.ClockTimeMarkers;

/**
 * Dumps world rules that live in game code rather than in the data pack.
 *
 * Output lines:
 *   default <environment attribute> <default value as JSON>
 *     The value an attribute has where no dimension, biome or timeline
 *     sets it.
 *   weather <rain|thunder> <environment attribute> <value> <alpha>
 *     Weather blends the attribute toward value by alpha, scaled by the
 *     rain or thunder level.
 *   wake_up <clock time marker>
 *     The marker the day clock jumps to when everyone has slept.
 *   explosion_particle <particle type> <scaling> <speed> <weight>
 *     The particles blocks give off in an explosion, as a weighted list.
 *   dungeon_mob <entity type>
 *     The mobs a monster room spawner picks from, uniformly, in order
 *     (repeats weight an entry).
 *   dungeon_loot <loot table>
 *     The loot table of monster room chests.
 *   decoration_step <name>
 *     The decoration steps features and structures are placed in, in order.
 *   ruined_portal <normal|giant> <template>
 *     The templates a ruined portal picks from, in order.
 *   mineshaft <type> <log> <planks> <fence>
 *     The wood of each mineshaft type, in the game's type order.
 *   mineshaft_loot <loot table>
 *     The loot table of mineshaft chest minecarts.
 *   shape_check <block>
 *     Blocks structure pieces mark for a shape update after placing them.
 *   nether_fossil <template>
 *     The templates a nether fossil picks from, in order.
 *   blackstone_replace <block> <replacement>
 *     The blocks nether ruined portals swap for blackstone variants.
 *   structure_loot <use> <loot table>
 *     The loot tables hand-built structure pieces fill containers from.
 *   structure_template <use> <template>
 *     The templates a template-built structure picks from, per use, in
 *     order.
 *   structure_marker <use> <marker> <loot table>
 *     The loot table a structure's data marker fills the chest below with.
 *   igloo <template> <pivot x y z> <offset x y z>
 *     The rotation pivot and the offset from the start of each igloo part.
 */
public class WorldProperties {
    private static <T> String json(EnvironmentAttribute<T> attribute) {
        JsonElement value = attribute.valueCodec()
            .encodeStart(JsonOps.INSTANCE, attribute.defaultValue())
            .getOrThrow();
        return value.toString();
    }

    private static String weather(String name, EnvironmentAttributeMap map) {
        EnvironmentAttributeMap.Entry<Float, ?> entry = map.get(EnvironmentAttributes.SKY_LIGHT_LEVEL);
        if (entry == null) {
            throw new IllegalStateException(name + " does not change the sky light level");
        }
        if (entry.modifier() != FloatModifier.ALPHA_BLEND || !(entry.argument() instanceof FloatWithAlpha blend)) {
            throw new IllegalStateException(name + " changes the sky light level with an unknown modifier");
        }
        return "weather " + name + " " + BuiltInRegistries.ENVIRONMENT_ATTRIBUTE.getKey(EnvironmentAttributes.SKY_LIGHT_LEVEL)
            + " " + blend.value() + " " + blend.alpha();
    }

    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        try (PrintWriter out = new PrintWriter(Files.newBufferedWriter(Path.of(args[0])))) {
            List<EnvironmentAttribute<?>> defaults = List.of(
                EnvironmentAttributes.BED_RULE,
                EnvironmentAttributes.STRAW_BED_RULE,
                EnvironmentAttributes.WATER_EVAPORATES,
                EnvironmentAttributes.SKY_LIGHT_LEVEL,
                EnvironmentAttributes.MONSTERS_BURN,
                EnvironmentAttributes.SURFACE_SLIME_SPAWN_CHANCE,
                EnvironmentAttributes.TURTLE_EGG_HATCH_CHANCE,
                EnvironmentAttributes.PIGLINS_ZOMBIFY,
                EnvironmentAttributes.VILLAGER_ACTIVITY,
                EnvironmentAttributes.BABY_VILLAGER_ACTIVITY,
                EnvironmentAttributes.RESPAWN_ANCHOR_WORKS,
                EnvironmentAttributes.CAN_START_RAID,
                EnvironmentAttributes.SUN_ANGLE);
            for (EnvironmentAttribute<?> attribute : defaults) {
                out.println("default " + BuiltInRegistries.ENVIRONMENT_ATTRIBUTE.getKey(attribute) + " " + json(attribute));
            }
            out.println(weather("rain", WeatherAttributes.RAIN));
            out.println(weather("thunder", WeatherAttributes.THUNDER));
            out.println("wake_up " + ClockTimeMarkers.WAKE_UP_FROM_SLEEP.identifier());
            java.lang.reflect.Field particles = net.minecraft.world.level.Level.class
                .getDeclaredField("DEFAULT_EXPLOSION_BLOCK_PARTICLES");
            particles.setAccessible(true);
            var list = (net.minecraft.util.random.WeightedList<net.minecraft.core.particles.ExplosionParticleInfo>)
                particles.get(null);
            for (var weighted : list.unwrap()) {
                var info = weighted.value();
                out.println("explosion_particle " + BuiltInRegistries.PARTICLE_TYPE.getKey(info.particle().getType())
                    + " " + info.scaling() + " " + info.speed() + " " + weighted.weight());
            }
            java.lang.reflect.Field mobs = net.minecraft.world.level.levelgen.feature.MonsterRoomFeature.class
                .getDeclaredField("MOBS");
            mobs.setAccessible(true);
            for (var type : (net.minecraft.world.entity.EntityType<?>[]) mobs.get(null)) {
                out.println("dungeon_mob " + BuiltInRegistries.ENTITY_TYPE.getKey(type));
            }
            out.println("dungeon_loot "
                + net.minecraft.world.level.storage.loot.BuiltInLootTables.SIMPLE_DUNGEON.identifier());
            for (var step : net.minecraft.world.level.levelgen.GenerationStep.Decoration.values()) {
                out.println("decoration_step " + step.getSerializedName());
            }
            Class<?> portal = net.minecraft.world.level.levelgen.structure.structures.RuinedPortalStructure.class;
            for (String[] kind : new String[][] {{"normal", "STRUCTURE_LOCATION_PORTALS"}, {"giant", "STRUCTURE_LOCATION_GIANT_PORTALS"}}) {
                java.lang.reflect.Field templates = portal.getDeclaredField(kind[1]);
                templates.setAccessible(true);
                for (String template : (String[]) templates.get(null)) {
                    out.println("ruined_portal " + kind[0] + " " + net.minecraft.resources.Identifier.withDefaultNamespace(template));
                }
            }
            for (var type : net.minecraft.world.level.levelgen.structure.structures.MineshaftStructure.Type.values()) {
                out.println("mineshaft " + type.getSerializedName() + " "
                    + BuiltInRegistries.BLOCK.getKey(type.getWoodState().getBlock()) + " "
                    + BuiltInRegistries.BLOCK.getKey(type.getPlanksState().getBlock()) + " "
                    + BuiltInRegistries.BLOCK.getKey(type.getFenceState().getBlock()));
            }
            out.println("mineshaft_loot "
                + net.minecraft.world.level.storage.loot.BuiltInLootTables.ABANDONED_MINESHAFT.identifier());
            java.lang.reflect.Field shapeCheck = net.minecraft.world.level.levelgen.structure.StructurePiece.class
                .getDeclaredField("SHAPE_CHECK_BLOCKS");
            shapeCheck.setAccessible(true);
            for (var block : (java.util.Set<net.minecraft.world.level.block.Block>) shapeCheck.get(null)) {
                out.println("shape_check " + BuiltInRegistries.BLOCK.getKey(block));
            }
            java.lang.reflect.Field fossils = net.minecraft.world.level.levelgen.structure.structures.NetherFossilPieces.class
                .getDeclaredField("FOSSILS");
            fossils.setAccessible(true);
            for (var template : (net.minecraft.resources.Identifier[]) fossils.get(null)) {
                out.println("nether_fossil " + template);
            }
            java.lang.reflect.Field replacements = net.minecraft.world.level.levelgen.structure.templatesystem.BlackstoneReplaceProcessor.class
                .getDeclaredField("replacements");
            replacements.setAccessible(true);
            var map = (java.util.Map<net.minecraft.world.level.block.Block, net.minecraft.world.level.block.Block>)
                replacements.get(net.minecraft.world.level.levelgen.structure.templatesystem.BlackstoneReplaceProcessor.INSTANCE);
            for (var entry : new java.util.TreeMap<>(map.entrySet().stream().collect(java.util.stream.Collectors.toMap(
                    e -> BuiltInRegistries.BLOCK.getKey(e.getKey()).toString(),
                    e -> BuiltInRegistries.BLOCK.getKey(e.getValue()).toString()))).entrySet()) {
                out.println("blackstone_replace " + entry.getKey() + " " + entry.getValue());
            }
            String[][] structureLoot = {
                {"fortress", "NETHER_BRIDGE"},
                {"stronghold_library", "STRONGHOLD_LIBRARY"},
                {"stronghold_crossing", "STRONGHOLD_CROSSING"},
                {"stronghold_corridor", "STRONGHOLD_CORRIDOR"},
                {"desert_pyramid", "DESERT_PYRAMID"},
                {"jungle_temple", "JUNGLE_TEMPLE"},
                {"jungle_temple_dispenser", "JUNGLE_TEMPLE_DISPENSER"},
                {"igloo", "IGLOO_CHEST"},
                {"woodland_mansion", "WOODLAND_MANSION"},
                {"underwater_ruin_small", "UNDERWATER_RUIN_SMALL"},
                {"underwater_ruin_big", "UNDERWATER_RUIN_BIG"},
                {"buried_treasure", "BURIED_TREASURE"},
                {"shipwreck_map", "SHIPWRECK_MAP"},
                {"shipwreck_supply", "SHIPWRECK_SUPPLY"},
                {"shipwreck_treasure", "SHIPWRECK_TREASURE"},
                {"end_city", "END_CITY_TREASURE"},
                {"desert_pyramid_archaeology", "DESERT_PYRAMID_ARCHAEOLOGY"},
                {"ocean_ruin_warm_archaeology", "OCEAN_RUIN_WARM_ARCHAEOLOGY"},
                {"ocean_ruin_cold_archaeology", "OCEAN_RUIN_COLD_ARCHAEOLOGY"},
            };
            for (String[] loot : structureLoot) {
                var key = (net.minecraft.resources.ResourceKey<?>) net.minecraft.world.level.storage.loot.BuiltInLootTables.class
                    .getField(loot[1]).get(null);
                out.println("structure_loot " + loot[0] + " " + key.identifier());
            }
            String[][] templateLists = {
                {"ShipwreckPieces", "shipwreck_beached", "STRUCTURE_LOCATION_BEACHED"},
                {"ShipwreckPieces", "shipwreck_ocean", "STRUCTURE_LOCATION_OCEAN"},
                {"OceanRuinPieces", "ocean_ruin_warm", "WARM_RUINS"},
                {"OceanRuinPieces", "ocean_ruin_big_warm", "BIG_WARM_RUINS"},
                {"OceanRuinPieces", "ocean_ruin_brick", "RUINS_BRICK"},
                {"OceanRuinPieces", "ocean_ruin_cracked", "RUINS_CRACKED"},
                {"OceanRuinPieces", "ocean_ruin_mossy", "RUINS_MOSSY"},
                {"OceanRuinPieces", "ocean_ruin_big_brick", "BIG_RUINS_BRICK"},
                {"OceanRuinPieces", "ocean_ruin_big_cracked", "BIG_RUINS_CRACKED"},
                {"OceanRuinPieces", "ocean_ruin_big_mossy", "BIG_RUINS_MOSSY"},
                {"IglooPieces", "igloo_top", "STRUCTURE_LOCATION_IGLOO"},
                {"IglooPieces", "igloo_middle", "STRUCTURE_LOCATION_LADDER"},
                {"IglooPieces", "igloo_bottom", "STRUCTURE_LOCATION_LABORATORY"},
            };
            for (String[] templateList : templateLists) {
                java.lang.reflect.Field field = Class.forName("net.minecraft.world.level.levelgen.structure.structures." + templateList[0])
                    .getDeclaredField(templateList[2]);
                field.setAccessible(true);
                Object value = field.get(null);
                var templates = value instanceof net.minecraft.resources.Identifier single
                    ? new net.minecraft.resources.Identifier[] {single}
                    : (net.minecraft.resources.Identifier[]) value;
                for (var template : templates) {
                    out.println("structure_template " + templateList[1] + " " + template);
                }
            }
            java.lang.reflect.Field markers = net.minecraft.world.level.levelgen.structure.structures.ShipwreckPieces.class
                .getDeclaredField("MARKERS_TO_LOOT");
            markers.setAccessible(true);
            var markerMap = new java.util.TreeMap<String, net.minecraft.resources.ResourceKey<?>>(
                (java.util.Map<String, net.minecraft.resources.ResourceKey<?>>) markers.get(null));
            for (var entry : markerMap.entrySet()) {
                out.println("structure_marker shipwreck " + entry.getKey() + " " + entry.getValue().identifier());
            }
            Class<?> igloo = net.minecraft.world.level.levelgen.structure.structures.IglooPieces.class;
            java.lang.reflect.Field pivots = igloo.getDeclaredField("PIVOTS");
            java.lang.reflect.Field offsets = igloo.getDeclaredField("OFFSETS");
            pivots.setAccessible(true);
            offsets.setAccessible(true);
            var pivotMap = (java.util.Map<net.minecraft.resources.Identifier, net.minecraft.core.BlockPos>) pivots.get(null);
            var offsetMap = (java.util.Map<net.minecraft.resources.Identifier, net.minecraft.core.BlockPos>) offsets.get(null);
            for (var entry : new java.util.TreeMap<>(pivotMap).entrySet()) {
                var pivot = entry.getValue();
                var offset = offsetMap.get(entry.getKey());
                out.println("igloo " + entry.getKey() + " " + pivot.getX() + " " + pivot.getY() + " " + pivot.getZ()
                    + " " + offset.getX() + " " + offset.getY() + " " + offset.getZ());
            }
        }
    }
}
