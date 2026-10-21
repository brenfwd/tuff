import java.io.PrintWriter;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import java.util.Locale;
import java.util.zip.CRC32;

import net.minecraft.SharedConstants;
import net.minecraft.core.HolderLookup;
import net.minecraft.core.registries.Registries;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.util.RandomSource;
import net.minecraft.world.level.levelgen.LegacyRandomSource;
import net.minecraft.world.level.levelgen.NoiseGeneratorSettings;
import net.minecraft.world.level.levelgen.Noises;
import net.minecraft.world.level.levelgen.PositionalRandomFactory;
import net.minecraft.world.level.levelgen.RandomState;
import net.minecraft.world.level.levelgen.WorldgenRandom;
import net.minecraft.world.level.levelgen.XoroshiroRandomSource;
import net.minecraft.world.level.levelgen.densityfunction.DensityBuffer;
import net.minecraft.world.level.levelgen.densityfunction.DensityFunction;
import net.minecraft.world.level.levelgen.densityfunction.DensityVolume;
import net.minecraft.world.level.levelgen.densityfunction.SamplerContext;
import net.minecraft.world.level.levelgen.synth.Noise;
import net.minecraft.world.level.levelgen.synth.PerlinNoise;
import net.minecraft.world.level.levelgen.synth.SimplexNoise;

/**
 * Writes reference values computed by the game's own world generation code,
 * for the bit-exactness tests of this server's reimplementation.
 *
 * Output lines are "key value..." with floats and doubles as the hex of their
 * raw bits, so the tests compare exact values.
 */
public class WorldgenReference {
    private static PrintWriter out;

    private static String f(float v) {
        return Integer.toHexString(Float.floatToRawIntBits(v));
    }

    private static String d(double v) {
        return Long.toHexString(Double.doubleToRawLongBits(v));
    }

    private static void line(String key, Object... values) {
        StringBuilder sb = new StringBuilder(key);
        for (Object value : values) {
            sb.append(' ').append(value);
        }
        out.println(sb);
    }

    private static void randomSequence(String key, RandomSource random) {
        List<Object> values = new ArrayList<>();
        values.add(random.nextInt());
        values.add(random.nextInt(10));
        values.add(random.nextInt(16));
        values.add(random.nextInt(1000000007));
        values.add(random.nextLong());
        values.add(random.nextBoolean());
        values.add(f(random.nextFloat()));
        values.add(d(random.nextDouble()));
        values.add(d(random.nextGaussian()));
        values.add(d(random.nextGaussian()));
        values.add(random.nextInt());
        line(key, values.toArray());
    }

    private static String crc(DensityBuffer buffer, int size) {
        CRC32 crc = new CRC32();
        for (int i = 0; i < size; i++) {
            int bits = Float.floatToRawIntBits(buffer.get(i));
            crc.update(bits & 255);
            crc.update((bits >>> 8) & 255);
            crc.update((bits >>> 16) & 255);
            crc.update((bits >>> 24) & 255);
        }
        return Long.toHexString(crc.getValue());
    }

    private static void randoms() {
        randomSequence("legacy", new LegacyRandomSource(12345L));
        randomSequence("xoroshiro", new XoroshiroRandomSource(12345L));
        randomSequence("xoroshiro_fork", new XoroshiroRandomSource(-99L).fork());
        PositionalRandomFactory positional = new XoroshiroRandomSource(7L).forkPositional();
        randomSequence("xoroshiro_at", positional.at(1, -2, 300));
        randomSequence("xoroshiro_hash", positional.fromHashOf("minecraft:test"));
        randomSequence("xoroshiro_seed", positional.fromSeed(424242L));
        PositionalRandomFactory legacyPositional = new LegacyRandomSource(7L).forkPositional();
        randomSequence("legacy_at", legacyPositional.at(1, -2, 300));
        randomSequence("legacy_hash", legacyPositional.fromHashOf("minecraft:test"));
        WorldgenRandom decoration = new WorldgenRandom(new XoroshiroRandomSource(0L));
        long seed = decoration.setDecorationSeed(1L, -160, 32);
        line("decoration_seed", seed);
        decoration.setFeatureSeed(seed, 3, 6);
        randomSequence("feature_seed", decoration);
        WorldgenRandom large = new WorldgenRandom(new LegacyRandomSource(0L));
        large.setLargeFeatureSeed(1L, 5, -9);
        randomSequence("large_feature_seed", large);
        large.setLargeFeatureWithSalt(1L, 5, -9, 10387312);
        randomSequence("large_feature_salt", large);
    }

    private static void noises(HolderLookup.Provider lookup) {
        PerlinNoise perlin = new PerlinNoise(new XoroshiroRandomSource(42L));
        double[][] points = {{0, 0, 0}, {0.5, 1.25, -7.75}, {1234.567, -64.0, 98765.4321}, {-3e7, 17.0, 2e7}};
        for (int i = 0; i < points.length; i++) {
            double[] p = points[i];
            line("perlin_" + i, f(perlin.get(p[0], p[1], p[2])), f(perlin.get(p[0], p[2])));
        }
        SimplexNoise simplex = new SimplexNoise(new XoroshiroRandomSource(42L));
        for (int i = 0; i < points.length; i++) {
            double[] p = points[i];
            line("simplex_" + i, f(simplex.get(p[0], p[2])));
        }
        RandomState state = RandomState.create(lookup.lookupOrThrow(Registries.NOISE), 1L,
            lookup.lookupOrThrow(Registries.NOISE_SETTINGS).getOrThrow(NoiseGeneratorSettings.OVERWORLD).value());
        Noise continents = state.getOrCreateNoise(Noises.CONTINENTALNESS);
        Noise surface = state.getOrCreateNoise(Noises.SURFACE);
        for (int i = 0; i < points.length; i++) {
            double[] p = points[i];
            line("continentalness_" + i, f(continents.get(p[0], p[1], p[2])), f(surface.get(p[0], p[1], p[2])));
        }
        DensityVolume volume = new DensityVolume(5, 7, 3, -37, -64, 1022, 4, 8, 4);
        DensityBuffer buffer = DensityBuffer.createUnpooled(volume.size());
        buffer.fill(0.0f);
        continents.addToVolume(buffer, volume, 0.25, 0.0, 1.0f);
        line("continentalness_volume", crc(buffer, volume.size()), f(buffer.get(0)), f(buffer.get(volume.size() - 1)));
    }

    private static RandomState state(HolderLookup.Provider lookup, net.minecraft.resources.ResourceKey<NoiseGeneratorSettings> key, long seed) {
        return RandomState.create(lookup.lookupOrThrow(Registries.NOISE), seed,
            lookup.lookupOrThrow(Registries.NOISE_SETTINGS).getOrThrow(key).value());
    }

    private static void densities(HolderLookup.Provider lookup) {
        String[] dims = {"overworld", "nether", "end"};
        List<net.minecraft.resources.ResourceKey<NoiseGeneratorSettings>> keys = List.of(
            NoiseGeneratorSettings.OVERWORLD, NoiseGeneratorSettings.NETHER, NoiseGeneratorSettings.END);
        int[][] points = {{0, 0, 0}, {5, 70, -3}, {-1000, 40, 2003}, {123456, -60, -98765}, {8, 100, 8}, {1, 2, 3}};
        for (int k = 0; k < dims.length; k++) {
            NoiseGeneratorSettings settings = lookup.lookupOrThrow(Registries.NOISE_SETTINGS).getOrThrow(keys.get(k)).value();
            RandomState state = state(lookup, keys.get(k), 1L);
            var router = settings.noiseRouter();
            DensityFunction[] functions = {router.temperature(), router.vegetation(), router.continents(), router.erosion(),
                router.depth(), router.ridges(), router.chunkSurfaceLevel(), router.finalDensity()};
            for (int i = 0; i < points.length; i++) {
                int[] p = points[i];
                List<Object> values = new ArrayList<>();
                for (DensityFunction function : functions) {
                    values.add(f(state.sampleBlockValueUncached(function, p[0], p[1], p[2])));
                }
                line("point_" + dims[k] + "_" + i, values.toArray());
            }
            int minY = settings.noiseSettings().minY();
            int height = settings.noiseSettings().height();
            int[][] chunks = {{0, 0}, {10, 10}, {-7, 13}};
            for (int[] chunk : chunks) {
                DensityVolume volume = new DensityVolume(16, height, 16, chunk[0] * 16, minY, chunk[1] * 16);
                SamplerContext context = SamplerContext.builder().enableCaches().build();
                var samplers = state.samplersWithContext(context);
                DensityBuffer buffer = DensityBuffer.createUnpooled(volume.size());
                List<Object> values = new ArrayList<>();
                for (DensityFunction function : functions) {
                    samplers.get(function).sampleVolume(buffer, volume);
                    values.add(crc(buffer, volume.size()));
                }
                line("volume_" + dims[k] + "_" + chunk[0] + "_" + chunk[1], values.toArray());
            }
        }
    }

    private static void rings() {
        var resources = new net.minecraft.server.packs.resources.MultiPackResourceManager(
            net.minecraft.server.packs.PackType.SERVER_DATA,
            List.of(net.minecraft.server.packs.repository.ServerPacksSource.createVanillaPackSource().fullResources()));
        var layers = net.minecraft.server.RegistryLayer.createRegistryAccess();
        var staticTags = net.minecraft.tags.TagLoader.loadTagsForExistingRegistries(
            resources, layers.getLayer(net.minecraft.server.RegistryLayer.STATIC));
        var context = net.minecraft.tags.TagLoader.buildUpdatedLookups(
            layers.getAccessForLoading(net.minecraft.server.RegistryLayer.WORLD), staticTags);
        HolderLookup.Provider lookup = net.minecraft.resources.RegistryDataLoader.load(
            resources, context, net.minecraft.resources.RegistryDataLoader.WORLD_REGISTRIES, Runnable::run).join();
        var params = lookup.lookupOrThrow(Registries.MULTI_NOISE_BIOME_SOURCE_PARAMETER_LIST)
            .getOrThrow(net.minecraft.world.level.biome.MultiNoiseBiomeSourceParameterLists.OVERWORLD);
        var source = net.minecraft.world.level.biome.MultiNoiseBiomeSource.createFromPreset(params);
        RandomState state = state(lookup, NoiseGeneratorSettings.OVERWORLD, 1L);
        var sets = lookup.lookupOrThrow(Registries.STRUCTURE_SET);
        var structures = net.minecraft.world.level.chunk.ChunkGeneratorStructureState.createForNormal(
            state, 1L, net.minecraft.world.level.ChunkPos.ZERO, source, sets);
        var placement = (net.minecraft.world.level.levelgen.structure.placement.ConcentricRingsStructurePlacement)
            sets.getOrThrow(net.minecraft.world.level.levelgen.structure.BuiltinStructureSets.STRONGHOLDS).value().placement();
        List<Object> values = new ArrayList<>();
        for (var pos : structures.getRingPositionsFor(placement)) {
            values.add(pos.x() + "," + pos.z());
        }
        line("stronghold_rings", values.toArray());
    }

    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        HolderLookup.Provider lookup = VanillaRegistries.createWorldLookup();
        try (PrintWriter writer = new PrintWriter(Files.newBufferedWriter(Path.of(args[0])))) {
            out = writer;
            randoms();
            noises(lookup);
            densities(lookup);
            rings();
        }
    }
}
