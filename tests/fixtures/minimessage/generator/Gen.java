import com.google.gson.*;
import java.nio.file.*;
import net.kyori.adventure.text.minimessage.MiniMessage;
import net.kyori.adventure.text.serializer.gson.GsonComponentSerializer;
public class Gen {
  public static void main(String[] a) throws Exception {
    JsonArray out = new JsonArray();
    for (String f : new String[]{a[0], a[1]}) {
      for (JsonElement e : JsonParser.parseString(Files.readString(Path.of(f))).getAsJsonArray()) {
        JsonObject o = e.getAsJsonObject();
        JsonObject r = new JsonObject();
        r.add("source", o.get("source")); r.add("input", o.get("input"));
        try { r.add("expected", GsonComponentSerializer.gson().serializeToTree(MiniMessage.miniMessage().deserialize(o.get("input").getAsString()))); }
        catch (Exception ex) { r.addProperty("error", ex.getClass().getSimpleName()); }
        out.add(r);
      }
    }
    System.out.println(new GsonBuilder().setPrettyPrinting().disableHtmlEscaping().create().toJson(out));
  }
}
