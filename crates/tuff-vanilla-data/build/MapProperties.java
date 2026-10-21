import java.io.PrintWriter;
import java.nio.file.Files;
import java.nio.file.Path;

import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.EmptyBlockGetter;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.material.MapColor;
import net.minecraft.world.level.saveddata.maps.MapDecorationType;

/**
 * Dumps map colours and decoration types.
 *
 * Output lines:
 *   color <id> <rgb>
 *     Every base map colour; a shown pixel is the id times four plus a
 *     brightness (0 low, 1 normal, 2 high, 3 lowest).
 *   states <first state id> <last state id> <color id>
 *     The map colour of consecutive block states, merged into runs.
 *   decoration <name> <asset id> <show on item frame 0/1> <track count 0/1>
 */
public class MapProperties {
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        try (PrintWriter out = new PrintWriter(Files.newBufferedWriter(Path.of(args[0])))) {
            for (int i = 0; i < 64; i++) {
                MapColor color = MapColor.byId(i);
                if (color.id == i) {
                    out.println("color " + i + " " + color.col);
                }
            }
            int count = Block.BLOCK_STATE_REGISTRY.size();
            int first = 0;
            int current = -1;
            for (int id = 0; id < count; id++) {
                BlockState state = Block.BLOCK_STATE_REGISTRY.byId(id);
                int color = state.getMapColor(EmptyBlockGetter.INSTANCE, BlockPos.ZERO).id;
                if (color != current) {
                    if (current >= 0) {
                        out.println("states " + first + " " + (id - 1) + " " + current);
                    }
                    first = id;
                    current = color;
                }
            }
            out.println("states " + first + " " + (count - 1) + " " + current);
            for (MapDecorationType type : BuiltInRegistries.MAP_DECORATION_TYPE) {
                out.println("decoration " + BuiltInRegistries.MAP_DECORATION_TYPE.getKey(type)
                    + " " + type.assetId()
                    + " " + (type.showOnItemFrame() ? 1 : 0)
                    + " " + (type.trackCount() ? 1 : 0));
            }
        }
    }
}
