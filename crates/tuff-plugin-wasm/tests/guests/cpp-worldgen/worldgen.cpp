#include "tuff.hpp"

using namespace tuff;

struct Sky {
    static void declare_dimensions(Dimensions &out) {
        Dimension sky("example:sky", "minecraft:overworld");
        sky.sea_level(5).natural_spawning(false).spawn(BlockPos{1, 70, 2});
        sky.biome(str("example:cloud"), str("{}"));
        out.add(move(sky));
    }

    static Status generate_chunk(const ChunkRequest &req, ChunkBuilder &chunk) {
        if (req.chunk_x == 99) {
            return Status::fail("refused");
        }
        chunk.fill(req.min_y, req.min_y, "minecraft:bedrock");
        chunk.set(3, req.min_y + 1, 4, "minecraft:stone");
        chunk.set(0, req.min_y + 2, 0, req.seed % 2 ? "minecraft:dirt" : "minecraft:sand");
        chunk.biome("example:cloud");
        return Status();
    }
};

TUFF_PLUGIN(Sky, "sky", "0.1.0", "worldgen probe");
