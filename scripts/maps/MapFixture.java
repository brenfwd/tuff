import java.nio.file.Path;

import com.mojang.serialization.DataResult;
import net.minecraft.SharedConstants;
import net.minecraft.nbt.CompoundTag;
import net.minecraft.nbt.ListTag;
import net.minecraft.nbt.NbtIo;
import net.minecraft.nbt.NbtOps;
import net.minecraft.nbt.Tag;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.saveddata.maps.MapItemSavedData;

/**
 * Writes a map saved-data file through the game's own codec: a sparse
 * compound is decoded and encoded again, so the output is exactly what the
 * game would save for that map. Argument 0 is the output path.
 */
public class MapFixture {
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        CompoundTag in = new CompoundTag();
        in.putString("dimension", "minecraft:the_nether");
        in.putInt("xCenter", -192);
        in.putInt("zCenter", 320);
        in.putByte("scale", (byte) 2);
        byte[] colors = new byte[16384];
        for (int i = 0; i < colors.length; i++) {
            colors[i] = (byte) (i * 7 + (i >> 7));
        }
        in.putByteArray("colors", colors);
        in.putBoolean("locked", true);
        in.putBoolean("unlimitedTracking", true);
        ListTag banners = new ListTag();
        CompoundTag named = new CompoundTag();
        named.putIntArray("pos", new int[] {-190, 70, 322});
        named.putString("color", "red");
        named.putString("name", "Home");
        banners.add(named);
        CompoundTag plain = new CompoundTag();
        plain.putIntArray("pos", new int[] {-150, 64, 300});
        banners.add(plain);
        in.put("banners", banners);
        ListTag frames = new ListTag();
        CompoundTag frame = new CompoundTag();
        frame.putIntArray("pos", new int[] {-180, 65, 330});
        frame.putInt("rotation", 90);
        frame.putInt("entity_id", 41);
        frames.add(frame);
        in.put("frames", frames);

        DataResult<MapItemSavedData> decoded = MapItemSavedData.CODEC.parse(NbtOps.INSTANCE, in);
        MapItemSavedData data = decoded.getOrThrow();
        Tag encoded = MapItemSavedData.CODEC.encodeStart(NbtOps.INSTANCE, data).getOrThrow();
        CompoundTag root = new CompoundTag();
        root.put("data", encoded);
        root.putInt("DataVersion", SharedConstants.getCurrentVersion().dataVersion().version());
        NbtIo.writeCompressed(root, Path.of(args[0]));
    }
}
