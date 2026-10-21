import java.io.InputStream;
import java.io.InputStreamReader;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Enumeration;
import java.util.jar.JarEntry;
import java.util.jar.JarFile;

import com.google.gson.Gson;
import com.google.gson.GsonBuilder;
import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import com.google.gson.JsonParser;

/**
 * Writes a data pack that stops vanilla world generation after one stage, so
 * the vanilla server's own chunks can serve as per-stage references:
 *
 *   noise    - no material rules (surface, bedrock, ore veins), carvers or features
 *   surface  - no carvers or features
 *   carvers  - no features
 *
 * Arguments: the server jar, the output directory, the mode.
 */
public class WorldgenDatapack {
    public static void main(String[] args) throws Exception {
        String mode = args[2];
        Path out = Path.of(args[1]);
        Gson gson = new GsonBuilder().setPrettyPrinting().create();
        Files.createDirectories(out);
        try (JarFile jar = new JarFile(args[0])) {
            int format;
            try (InputStream in = jar.getInputStream(jar.getEntry("version.json"))) {
                format = JsonParser.parseReader(new InputStreamReader(in, StandardCharsets.UTF_8)).getAsJsonObject()
                    .getAsJsonObject("pack_version").get("data_major").getAsInt();
            }
            Files.writeString(out.resolve("pack.mcmeta"), "{\"pack\": {\"description\": \"worldgen stage " + mode
                + "\", \"min_format\": " + format + ", \"max_format\": " + format + "}}");
            Enumeration<JarEntry> entries = jar.entries();
            while (entries.hasMoreElements()) {
                JarEntry entry = entries.nextElement();
                String name = entry.getName();
                boolean biome = name.startsWith("data/minecraft/worldgen/biome/") && name.endsWith(".json");
                boolean settings = name.startsWith("data/minecraft/worldgen/noise_settings/") && name.endsWith(".json");
                if (!biome && !(settings && mode.equals("noise"))) {
                    continue;
                }
                JsonObject json;
                try (InputStream in = jar.getInputStream(entry)) {
                    json = JsonParser.parseReader(new InputStreamReader(in, StandardCharsets.UTF_8)).getAsJsonObject();
                }
                if (biome) {
                    json.add("features", new JsonArray());
                    if (!mode.equals("carvers")) {
                        json.add("carvers", new JsonArray());
                    }
                } else {
                    JsonObject empty = new JsonObject();
                    empty.addProperty("type", "minecraft:sequence");
                    empty.add("sequence", new JsonArray());
                    json.add("material_rule", empty);
                }
                Path target = out.resolve(name);
                Files.createDirectories(target.getParent());
                Files.writeString(target, gson.toJson(json));
            }
        }
    }
}
