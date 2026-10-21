import java.io.PrintWriter;
import java.nio.file.Files;
import java.nio.file.Path;

import net.minecraft.SharedConstants;
import net.minecraft.resources.RegistryDataLoader;
import net.minecraft.server.Bootstrap;

/**
 * Dumps the data-driven registries the server synchronizes to clients
 * during configuration (Registry Data, and the data registries whose tags
 * Update Tags carries), which no data generator report lists.
 *
 * Output lines, in the order the server sends them:
 *   synchronized <registry key>
 */
public class NetworkRegistries {
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        try (PrintWriter out = new PrintWriter(Files.newBufferedWriter(Path.of(args[0])))) {
            for (RegistryDataLoader.RegistryData<?> data : RegistryDataLoader.SYNCHRONIZED_REGISTRIES) {
                out.println("synchronized " + data.key().identifier());
            }
        }
    }
}
