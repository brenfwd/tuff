import java.io.PrintWriter;
import java.lang.reflect.Method;
import java.lang.reflect.Proxy;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.TreeMap;

import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.HolderLookup;
import net.minecraft.core.Registry;
import net.minecraft.core.RegistryAccess;
import net.minecraft.core.component.DataComponentGetter;
import net.minecraft.core.component.DataComponentInitializers;
import net.minecraft.core.component.DataComponentType;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.nbt.CompoundTag;
import net.minecraft.server.Bootstrap;
import net.minecraft.server.packs.PackType;
import net.minecraft.server.packs.repository.ServerPacksSource;
import net.minecraft.server.packs.resources.MultiPackResourceManager;
import net.minecraft.tags.TagLoader;
import net.minecraft.world.Container;
import net.minecraft.world.item.Item;
import net.minecraft.world.item.SignApplicator;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.EntityBlock;
import net.minecraft.world.level.block.entity.BlockEntity;
import net.minecraft.world.level.block.entity.BlockEntityType;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.storage.ValueOutput;

/**
 * Dumps which blocks have block entities and what a new block entity of
 * each type holds.
 *
 * Output lines:
 *   block <block> <block entity type> <created 0/1>
 *     created: placing the block creates its block entity (0 when game
 *     code creates it some other way, e.g. moving pistons).
 *   default <block entity type> <SNBT>
 *     The saved data of a new block entity, without id and position; only
 *     for types some block creates.
 *   update <block entity type> <SNBT>
 *     The data clients receive for a new block entity.
 *   size <block entity type> <slots>
 *     For block entities that hold items.
 *   updates <block entity type>
 *     Clients are sent the block entity's data whenever its block changes.
 *   op_only <block entity type>
 *     Only operators in creative mode may place it with block entity data.
 *   keep <block> <old block>
 *     Replacing the old block with this one keeps its block entity.
 *   sign_applicator <item>
 *     Using the item on a sign changes its text or waxes it.
 *   implicit <block entity type> <data component>
 *     A component the block entity takes from the item that places it
 *     (and gives back to its drops), in the order it reads them.
 *   discard <block entity type> <key>
 *     A saved key holding one of those components, which is left out when
 *     the block entity's data is copied into an item.
 */
public class BlockEntityProperties {
    private static Set<DataComponentType<?>> implicit(BlockEntity entity) throws Exception {
        Set<DataComponentType<?>> seen = new LinkedHashSet<>();
        DataComponentGetter getter = new DataComponentGetter() {
            @Override
            public <T> T get(DataComponentType<? extends T> type) {
                seen.add(type);
                return null;
            }
        };
        Method apply = BlockEntity.class.getDeclaredMethod("applyImplicitComponents", DataComponentGetter.class);
        apply.setAccessible(true);
        apply.invoke(entity, getter);
        return seen;
    }

    private static List<String> discarded(BlockEntity entity) {
        List<String> keys = new ArrayList<>();
        ValueOutput output = (ValueOutput) Proxy.newProxyInstance(
            ValueOutput.class.getClassLoader(),
            new Class<?>[] {ValueOutput.class},
            (proxy, method, args) -> {
                if (!method.getName().equals("discard")) {
                    throw new IllegalStateException("block entity " + entity.getType()
                        + " writes while removing components: " + method.getName());
                }
                keys.add((String) args[0]);
                return null;
            });
        entity.removeComponentsFromTag(output);
        return keys;
    }

    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        List<Registry.PendingTags<?>> tags = TagLoader.loadTagsForExistingRegistries(
            new MultiPackResourceManager(
                PackType.SERVER_DATA, List.of(ServerPacksSource.createVanillaPackSource().fullResources())),
            RegistryAccess.fromRegistryOfRegistries(BuiltInRegistries.REGISTRY));
        if (tags.isEmpty()) {
            throw new IllegalStateException("no tags found in the vanilla data pack");
        }
        tags.forEach(Registry.PendingTags::apply);
        HolderLookup.Provider registries = VanillaRegistries.createWorldLookup();
        BuiltInRegistries.DATA_COMPONENT_INITIALIZERS.build(registries)
            .forEach(DataComponentInitializers.PendingComponents::apply);

        Map<String, String> blocks = new TreeMap<>();
        try (PrintWriter out = new PrintWriter(Files.newBufferedWriter(Path.of(args[0])))) {
            for (BlockEntityType<?> type : BuiltInRegistries.BLOCK_ENTITY_TYPE) {
                String name = BuiltInRegistries.BLOCK_ENTITY_TYPE.getKey(type).toString();
                String saved = null;
                String update = null;
                BlockEntity sample = null;
                for (Block block : BuiltInRegistries.BLOCK) {
                    BlockState state = block.defaultBlockState();
                    if (!type.isValid(state)) {
                        continue;
                    }
                    String blockName = BuiltInRegistries.BLOCK.getKey(block).toString();
                    String previous = blocks.put(blockName, name);
                    if (previous != null) {
                        throw new IllegalStateException(blockName + " is valid for " + previous + " and " + name);
                    }
                    if (!(block instanceof EntityBlock entityBlock)) {
                        throw new IllegalStateException(blockName + " has block entity " + name + " but makes none");
                    }
                    boolean created = true;
                    for (BlockState each : block.getStateDefinition().getPossibleStates()) {
                        if (!each.hasBlockEntity()) {
                            throw new IllegalStateException(each + " has no block entity");
                        }
                        BlockEntity made = entityBlock.newBlockEntity(BlockPos.ZERO, each);
                        if (made == null) {
                            created = false;
                        } else if (made.getType() != type) {
                            throw new IllegalStateException(each + " makes " + made.getType() + ", not " + name);
                        }
                    }
                    out.println("block " + blockName + " " + name + " " + (created ? 1 : 0));

                    BlockEntity entity = type.create(BlockPos.ZERO, state);
                    if (sample == null) {
                        sample = entity;
                    }
                    if (!created) {
                        continue;
                    }
                    String entitySaved = entity.saveWithoutMetadata(registries).toString();
                    String entityUpdate = entity.getUpdateTag(registries).toString();
                    if (saved == null) {
                        saved = entitySaved;
                        update = entityUpdate;
                    } else if (!saved.equals(entitySaved) || !update.equals(entityUpdate)) {
                        throw new IllegalStateException(name + " starts out differently for " + blockName);
                    }
                }
                if (sample == null) {
                    throw new IllegalStateException("no block has block entity " + name);
                }
                if (saved != null) {
                    out.println("default " + name + " " + saved);
                    out.println("update " + name + " " + update);
                }
                if (sample instanceof Container container) {
                    out.println("size " + name + " " + container.getContainerSize());
                }
                if (sample.getClass().getMethod("getUpdatePacket").getDeclaringClass() != BlockEntity.class) {
                    out.println("updates " + name);
                }
                if (type.onlyOpCanSetNbt()) {
                    out.println("op_only " + name);
                }
                for (DataComponentType<?> component : implicit(sample)) {
                    out.println("implicit " + name + " " + BuiltInRegistries.DATA_COMPONENT_TYPE.getKey(component));
                }
                for (String key : discarded(sample)) {
                    out.println("discard " + name + " " + key);
                }
            }
            for (Item item : BuiltInRegistries.ITEM) {
                if (item instanceof SignApplicator) {
                    out.println("sign_applicator " + BuiltInRegistries.ITEM.getKey(item));
                }
            }
            for (Block block : BuiltInRegistries.BLOCK) {
                String blockName = BuiltInRegistries.BLOCK.getKey(block).toString();
                if (block instanceof EntityBlock && !blocks.containsKey(blockName)) {
                    throw new IllegalStateException(blockName + " makes block entities of no known type");
                }
                if (!blocks.containsKey(blockName)) {
                    continue;
                }
                for (Block old : BuiltInRegistries.BLOCK) {
                    String oldName = BuiltInRegistries.BLOCK.getKey(old).toString();
                    if (old != block && blocks.containsKey(oldName)
                        && block.defaultBlockState().shouldChangedStateKeepBlockEntity(old.defaultBlockState())) {
                        out.println("keep " + blockName + " " + oldName);
                    }
                }
            }
        }
    }
}
