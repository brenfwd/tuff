import java.io.PrintWriter;
import java.lang.reflect.Field;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.TreeMap;

import net.minecraft.SharedConstants;
import net.minecraft.core.Holder;
import net.minecraft.core.Registry;
import net.minecraft.core.RegistryAccess;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.resources.ResourceKey;
import net.minecraft.server.Bootstrap;
import net.minecraft.server.packs.PackType;
import net.minecraft.server.packs.repository.ServerPacksSource;
import net.minecraft.server.packs.resources.MultiPackResourceManager;
import net.minecraft.sounds.SoundEvent;
import net.minecraft.tags.TagLoader;
import net.minecraft.world.entity.EntityType;
import net.minecraft.world.entity.ai.gossip.GossipType;
import net.minecraft.world.entity.ai.sensing.VillagerHostilesSensor;
import net.minecraft.world.entity.ai.village.poi.PoiType;
import net.minecraft.world.entity.npc.villager.VillagerData;
import net.minecraft.world.entity.npc.villager.VillagerProfession;
import net.minecraft.world.entity.npc.villager.VillagerType;
import net.minecraft.world.item.Item;
import net.minecraft.world.item.trading.TradeSet;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.state.BlockState;

/**
 * Dumps the villager data that lives in game code rather than in any
 * report.
 *
 * Output lines:
 *   poi <name> <max tickets> <valid range> <block state id>...
 *     Point of interest types and the block states that make them.
 *   profession <name> <held job site poi | -> <acquirable job site pois, comma separated | -> <work sound | -> <trade set per level 1..5 | ->...
 *   requested <profession> <item>...
 *     Items a profession picks up besides food.
 *   secondary <profession> <block>...
 *     Blocks a profession works at besides its job site.
 *   biome_type <biome> <villager type>
 *     The villager type born in a biome; other biomes give plains.
 *   default_type <villager type>
 *   xp_threshold <level> <experience>
 *     Experience at which each level starts.
 *   gossip <name> <weight> <max> <decay per day> <decay per transfer>
 *   hostile <entity type> <distance>
 *     Mobs villagers flee from when this close.
 */
public class VillagerProperties {
    private static Object constant(Class<?> owner, String name) throws Exception {
        Field field = owner.getDeclaredField(name);
        field.setAccessible(true);
        return field.get(null);
    }

    private static String sound(SoundEvent event) {
        if (event == null) {
            return "-";
        }
        return BuiltInRegistries.SOUND_EVENT.getKey(event).toString();
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

        List<Holder.Reference<PoiType>> pois = new ArrayList<>();
        BuiltInRegistries.POINT_OF_INTEREST_TYPE.listElements().forEach(pois::add);
        if (pois.isEmpty()) {
            throw new IllegalStateException("no point of interest types");
        }

        try (PrintWriter out = new PrintWriter(Files.newBufferedWriter(Path.of(args[0])))) {
            for (Holder.Reference<PoiType> poi : pois) {
                StringBuilder line = new StringBuilder("poi ")
                    .append(poi.key().identifier())
                    .append(' ').append(poi.value().maxTickets())
                    .append(' ').append(poi.value().validRange());
                List<Integer> ids = new ArrayList<>();
                for (BlockState state : poi.value().matchingStates()) {
                    ids.add(Block.getId(state));
                }
                ids.sort(null);
                for (int id : ids) {
                    line.append(' ').append(id);
                }
                out.println(line);
            }

            for (Holder.Reference<VillagerProfession> holder : BuiltInRegistries.VILLAGER_PROFESSION.listElements().toList()) {
                VillagerProfession profession = holder.value();
                String name = holder.key().identifier().toString();
                List<String> held = new ArrayList<>();
                List<String> acquirable = new ArrayList<>();
                for (Holder.Reference<PoiType> poi : pois) {
                    if (profession.heldJobSite().test(poi)) {
                        held.add(poi.key().identifier().toString());
                    }
                    if (profession.acquirableJobSite().test(poi)) {
                        acquirable.add(poi.key().identifier().toString());
                    }
                }
                if (held.size() > 1) {
                    throw new IllegalStateException(name + " holds more than one job site type");
                }
                StringBuilder line = new StringBuilder("profession ").append(name)
                    .append(' ').append(held.isEmpty() ? "-" : held.get(0))
                    .append(' ').append(acquirable.isEmpty() ? "-" : String.join(",", acquirable))
                    .append(' ').append(sound(profession.workSound()));
                for (int level = VillagerData.MIN_VILLAGER_LEVEL; level <= VillagerData.MAX_VILLAGER_LEVEL; level++) {
                    ResourceKey<TradeSet> set = profession.getTrades(level);
                    line.append(' ').append(set == null ? "-" : set.identifier().toString());
                }
                if (!profession.tradeSetsByLevel().keySet().intStream()
                        .allMatch(l -> l >= VillagerData.MIN_VILLAGER_LEVEL && l <= VillagerData.MAX_VILLAGER_LEVEL)) {
                    throw new IllegalStateException(name + " has trades outside the villager levels");
                }
                out.println(line);
                if (!profession.requestedItems().isEmpty()) {
                    StringBuilder items = new StringBuilder("requested ").append(name);
                    for (Item item : profession.requestedItems()) {
                        items.append(' ').append(BuiltInRegistries.ITEM.getKey(item));
                    }
                    out.println(items);
                }
                if (!profession.secondaryPoi().isEmpty()) {
                    StringBuilder blocks = new StringBuilder("secondary ").append(name);
                    for (Block block : profession.secondaryPoi()) {
                        blocks.append(' ').append(BuiltInRegistries.BLOCK.getKey(block));
                    }
                    out.println(blocks);
                }
            }

            @SuppressWarnings("unchecked")
            Map<ResourceKey<?>, ResourceKey<?>> byBiome = (Map<ResourceKey<?>, ResourceKey<?>>) constant(VillagerType.class, "BY_BIOME");
            Map<String, String> sorted = new TreeMap<>();
            byBiome.forEach((biome, type) -> sorted.put(biome.identifier().toString(), type.identifier().toString()));
            sorted.forEach((biome, type) -> out.println("biome_type " + biome + " " + type));
            out.println("default_type " + VillagerData.DEFAULT_TYPE.identifier());

            for (int level = VillagerData.MIN_VILLAGER_LEVEL; level <= VillagerData.MAX_VILLAGER_LEVEL; level++) {
                int xp = level < VillagerData.MAX_VILLAGER_LEVEL
                    ? VillagerData.getMinXpPerLevel(level)
                    : VillagerData.getMaxXpPerLevel(level - 1);
                out.println("xp_threshold " + level + " " + xp);
            }

            @SuppressWarnings("unchecked")
            Map<EntityType<?>, Float> hostiles = (Map<EntityType<?>, Float>) constant(VillagerHostilesSensor.class, "ACCEPTABLE_DISTANCE_FROM_HOSTILES");
            Map<String, Float> sortedHostiles = new TreeMap<>();
            hostiles.forEach((type, distance) -> sortedHostiles.put(BuiltInRegistries.ENTITY_TYPE.getKey(type).toString(), distance));
            if (sortedHostiles.isEmpty()) {
                throw new IllegalStateException("villagers fear nothing");
            }
            sortedHostiles.forEach((type, distance) -> out.println("hostile " + type + " " + distance));

            for (GossipType type : GossipType.values()) {
                out.println("gossip " + type.id + " " + type.weight + " " + type.max + " "
                    + type.decayPerDay + " " + type.decayPerTransfer);
            }
        }
    }
}
