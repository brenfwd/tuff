import java.io.PrintWriter;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;

import net.minecraft.SharedConstants;
import net.minecraft.core.Direction;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.phys.AABB;
import net.minecraft.world.phys.shapes.Shapes;
import net.minecraft.world.phys.shapes.VoxelShape;

/**
 * Dumps the block state properties the light engine reads.
 *
 * Output lines:
 *   directions <direction names in ordinal order>
 *   faces N
 *   occludes <row of N 0/1 chars>      (N rows: whether faces a and b together cover a full face)
 *   state <id> <emission> <dampening> <empty shape 0/1> <face DOWN> ... <face EAST>
 * Face ids index the occludes matrix; empty-shape states have only empty faces.
 */
public class LightProperties {
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();

        List<VoxelShape> faces = new ArrayList<>();
        Map<String, Integer> ids = new HashMap<>();
        faces.add(Shapes.empty());
        ids.put("empty", 0);
        faces.add(Shapes.block());
        ids.put("block", 1);

        StringBuilder states = new StringBuilder();
        int count = Block.BLOCK_STATE_REGISTRY.size();
        for (int id = 0; id < count; id++) {
            BlockState state = Block.BLOCK_STATE_REGISTRY.byId(id);
            states.append("state ").append(id)
                .append(' ').append(state.getLightEmission())
                .append(' ').append(state.getLightDampening());
            boolean emptyShape = !state.canOcclude() || !state.useShapeForLightOcclusion();
            states.append(' ').append(emptyShape ? 1 : 0);
            for (Direction direction : Direction.values()) {
                VoxelShape shape = emptyShape ? Shapes.empty() : state.getFaceOcclusionShape(direction);
                String key = key(shape, direction);
                Integer face = ids.get(key);
                if (face == null) {
                    face = faces.size();
                    faces.add(shape);
                    ids.put(key, face);
                }
                states.append(' ').append(face);
            }
            states.append('\n');
        }

        try (PrintWriter out = new PrintWriter(Files.newBufferedWriter(Path.of(args[0])))) {
            StringBuilder directions = new StringBuilder("directions");
            for (Direction direction : Direction.values()) {
                directions.append(' ').append(direction.getSerializedName());
            }
            out.println(directions);
            out.println("faces " + faces.size());
            for (VoxelShape a : faces) {
                StringBuilder row = new StringBuilder("occludes ");
                for (VoxelShape b : faces) {
                    row.append(Shapes.faceShapeOccludes(a, b) ? '1' : '0');
                }
                out.println(row);
            }
            out.print(states);
        }
    }

    private static String key(VoxelShape shape, Direction direction) {
        if (shape == Shapes.block()) {
            return "block";
        }
        if (shape.isEmpty()) {
            return "empty";
        }
        StringBuilder key = new StringBuilder(direction.name());
        for (AABB box : shape.toAabbs()) {
            key.append(' ').append(box.minX).append(',').append(box.minY).append(',').append(box.minZ)
                .append(',').append(box.maxX).append(',').append(box.maxY).append(',').append(box.maxZ);
        }
        return key.toString();
    }
}
