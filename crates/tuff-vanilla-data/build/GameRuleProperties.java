import java.io.PrintWriter;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.stream.Collectors;

import com.mojang.brigadier.arguments.ArgumentType;
import com.mojang.brigadier.arguments.IntegerArgumentType;

import net.minecraft.SharedConstants;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.flag.FeatureFlags;
import net.minecraft.world.level.gamerules.GameRule;

/**
 * Dumps every game rule's type and default, which no data generator report
 * contains.
 *
 * Output lines:
 *   rule <id> bool <default> <features>
 *   rule <id> int <default> <min> <max> <features>
 *     features: the feature flags the rule needs, comma separated and
 *     sorted, or - when it needs none.
 */
public class GameRuleProperties {
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        try (PrintWriter out = new PrintWriter(Files.newBufferedWriter(Path.of(args[0])))) {
            for (GameRule<?> rule : BuiltInRegistries.GAME_RULE) {
                String id = BuiltInRegistries.GAME_RULE.getKey(rule).toString();
                Object defaultValue = rule.defaultValue();
                String features = FeatureFlags.REGISTRY.toNames(rule.requiredFeatures()).stream()
                    .map(Object::toString)
                    .sorted()
                    .collect(Collectors.joining(","));
                if (features.isEmpty()) {
                    features = "-";
                }
                ArgumentType<?> argument = rule.argument();
                if (defaultValue instanceof Boolean value) {
                    out.println("rule " + id + " bool " + value + " " + features);
                } else if (defaultValue instanceof Integer value && argument instanceof IntegerArgumentType range) {
                    out.println("rule " + id + " int " + value + " " + range.getMinimum() + " "
                        + range.getMaximum() + " " + features);
                } else {
                    throw new IllegalStateException("game rule " + id + " has an unsupported type");
                }
            }
        }
    }
}
