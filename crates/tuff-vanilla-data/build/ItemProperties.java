import java.io.PrintWriter;
import java.nio.file.Files;
import java.nio.file.Path;

import net.minecraft.SharedConstants;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.item.BannerItem;
import net.minecraft.world.item.BucketItem;
import net.minecraft.world.item.HoneycombItem;
import net.minecraft.world.item.MobBucketItem;
import net.minecraft.world.item.SolidBucketItem;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.material.Fluids;
import net.minecraft.sounds.SoundEvent;
import net.minecraft.world.item.DyeColor;
import net.minecraft.world.item.Item;
import net.minecraft.world.item.ItemStackTemplate;
import net.minecraft.world.item.component.FireworkExplosion;

/**
 * Dumps item data that lives in game code rather than in any data generator
 * report or default data component.
 *
 * Output lines:
 *   remainder <item> <remainder item> <count>
 *     The item left in the crafting grid when this item is used up.
 *   dye <name> <id> <texture diffuse color> <firework color>
 *     One per dye color in ID order; colors are opaque ARGB integers.
 *   banner <item> <dye name>
 *     The base color of a banner item.
 *   firework_shape <name> <id>
 *     One per firework explosion shape in ID order.
 *   wax <block> <waxed block>
 *     What honeycomb turns a block into.
 *   bucket <item> <fluid block, or - when empty> <holds a mob 0/1> <entity type, or - when not a mob bucket>
 *   solid_bucket <item> <block> <place sound>
 *   stat_type <stat type> <registry>
 *     The registry whose entries a statistic type counts.
 *   boat <item> <entity type>
 *     The boat, chest boat or raft an item places.
 */
public class ItemProperties {
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        try (PrintWriter out = new PrintWriter(Files.newBufferedWriter(Path.of(args[0])))) {
            for (Item item : BuiltInRegistries.ITEM) {
                ItemStackTemplate remainder = item.getCraftingRemainder();
                if (remainder == null) {
                    continue;
                }
                if (!remainder.components().isEmpty()) {
                    throw new IllegalStateException(
                        BuiltInRegistries.ITEM.getKey(item) + " has a crafting remainder with components");
                }
                out.println("remainder " + BuiltInRegistries.ITEM.getKey(item) + " "
                    + BuiltInRegistries.ITEM.getKey(remainder.item().value()) + " " + remainder.count());
            }
            for (DyeColor color : DyeColor.values()) {
                out.println("dye " + color.getName() + " " + color.getId() + " "
                    + color.getTextureDiffuseColor() + " " + color.getFireworkColor());
            }
            for (Item item : BuiltInRegistries.ITEM) {
                if (item instanceof BannerItem banner) {
                    out.println("banner " + BuiltInRegistries.ITEM.getKey(item) + " " + banner.getColor().getName());
                }
            }
            for (FireworkExplosion.Shape shape : FireworkExplosion.Shape.values()) {
                out.println("firework_shape " + shape.getSerializedName() + " " + shape.getId());
            }
            for (var entry : HoneycombItem.WAXABLES.get().entrySet()) {
                out.println("wax " + BuiltInRegistries.BLOCK.getKey(entry.getKey()) + " "
                    + BuiltInRegistries.BLOCK.getKey(entry.getValue()));
            }
            for (net.minecraft.stats.StatType<?> type : BuiltInRegistries.STAT_TYPE) {
                out.println("stat_type " + BuiltInRegistries.STAT_TYPE.getKey(type) + " "
                    + type.getRegistry().key().identifier());
            }
            java.lang.reflect.Field boatType = net.minecraft.world.item.BoatItem.class.getDeclaredField("entityType");
            boatType.setAccessible(true);
            for (Item item : BuiltInRegistries.ITEM) {
                if (item instanceof net.minecraft.world.item.BoatItem boat) {
                    out.println("boat " + BuiltInRegistries.ITEM.getKey(item) + " "
                        + BuiltInRegistries.ENTITY_TYPE.getKey((net.minecraft.world.entity.EntityType<?>) boatType.get(boat)));
                }
            }
            java.lang.reflect.Field placeSound = SolidBucketItem.class.getDeclaredField("placeSound");
            placeSound.setAccessible(true);
            java.lang.reflect.Field mobBucketType = MobBucketItem.class.getDeclaredField("type");
            mobBucketType.setAccessible(true);
            for (Item item : BuiltInRegistries.ITEM) {
                String name = BuiltInRegistries.ITEM.getKey(item).toString();
                if (item instanceof BucketItem bucket) {
                    String fluid = bucket.getContent() == Fluids.EMPTY
                        ? "-"
                        : BuiltInRegistries.BLOCK.getKey(
                            bucket.getContent().defaultFluidState().createLegacyBlock().getBlock()).toString();
                    String mobType = "-";
                    if (item instanceof MobBucketItem mobBucket) {
                        mobType = BuiltInRegistries.ENTITY_TYPE.getKey(
                            (net.minecraft.world.entity.EntityType<?>) mobBucketType.get(mobBucket)).toString();
                    }
                    out.println("bucket " + name + " " + fluid + " " + (item instanceof MobBucketItem ? 1 : 0)
                        + " " + mobType);
                } else if (item instanceof SolidBucketItem bucket) {
                    SoundEvent sound = (SoundEvent) placeSound.get(bucket);
                    out.println("solid_bucket " + name + " " + BuiltInRegistries.BLOCK.getKey(bucket.getBlock()) + " "
                        + sound.location());
                }
            }
        }
    }
}
