import java.io.PrintWriter;
import java.lang.reflect.Field;
import java.nio.file.Files;
import java.nio.file.Path;

import net.minecraft.SharedConstants;
import net.minecraft.core.Holder;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.effect.MobEffect;
import net.minecraft.util.StringRepresentable;
import net.minecraft.world.entity.EntityType;
import net.minecraft.world.entity.EquipmentSlotGroup;
import net.minecraft.world.entity.LivingEntity;
import net.minecraft.world.entity.ai.attributes.Attribute;
import net.minecraft.world.entity.ai.attributes.AttributeModifier;
import net.minecraft.world.entity.ai.attributes.AttributeSupplier;
import net.minecraft.world.entity.ai.attributes.DefaultAttributes;
import net.minecraft.world.entity.ai.attributes.RangedAttribute;
import net.minecraft.world.item.component.ItemAttributeModifiers;

/**
 * Dumps attribute data that lives in game code rather than in any report.
 *
 * Output lines:
 *   attribute <id> <default> <min> <max>
 *   base <entity type> <attribute> <value>
 *     The attributes a living entity type has, with their base values.
 *   effect <mob effect> <attribute> <modifier id> <amount at amplifier 0> <operation>
 *   network <enum> <name> <id>
 *     Network ids of the enums attribute modifier components use:
 *     slot_group, operation and display.
 */
public class AttributeProperties {
    private static void network(PrintWriter out, String kind, Enum<?>[] values) throws Exception {
        for (Enum<?> value : values) {
            Field id = value.getDeclaringClass().getDeclaredField("id");
            id.setAccessible(true);
            out.println("network " + kind + " " + ((StringRepresentable) value).getSerializedName() + " " + id.getInt(value));
        }
    }

    @SuppressWarnings("unchecked")
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        try (PrintWriter out = new PrintWriter(Files.newBufferedWriter(Path.of(args[0])))) {
            for (Attribute attribute : BuiltInRegistries.ATTRIBUTE) {
                if (!(attribute instanceof RangedAttribute ranged)) {
                    throw new IllegalStateException("attribute is not ranged: " + BuiltInRegistries.ATTRIBUTE.getKey(attribute));
                }
                out.println("attribute " + BuiltInRegistries.ATTRIBUTE.getKey(attribute)
                    + " " + ranged.getDefaultValue()
                    + " " + ranged.getMinValue()
                    + " " + ranged.getMaxValue());
            }
            for (EntityType<?> type : BuiltInRegistries.ENTITY_TYPE) {
                if (!DefaultAttributes.hasSupplier(type)) {
                    continue;
                }
                AttributeSupplier supplier = DefaultAttributes.getSupplier((EntityType<? extends LivingEntity>) type);
                BuiltInRegistries.ATTRIBUTE.listElements().forEach(holder -> {
                    if (supplier.hasAttribute(holder)) {
                        out.println("base " + BuiltInRegistries.ENTITY_TYPE.getKey(type)
                            + " " + holder.key().identifier()
                            + " " + supplier.getBaseValue(holder));
                    }
                });
            }
            network(out, "slot_group", EquipmentSlotGroup.values());
            network(out, "operation", AttributeModifier.Operation.values());
            network(out, "display", ItemAttributeModifiers.Display.Type.values());
            for (MobEffect effect : BuiltInRegistries.MOB_EFFECT) {
                String name = BuiltInRegistries.MOB_EFFECT.getKey(effect).toString();
                effect.createModifiers(0, (Holder<Attribute> attribute, AttributeModifier modifier) ->
                    out.println("effect " + name
                        + " " + attribute.unwrapKey().orElseThrow().identifier()
                        + " " + modifier.id()
                        + " " + modifier.amount()
                        + " " + modifier.operation().getSerializedName()));
            }
        }
    }
}
