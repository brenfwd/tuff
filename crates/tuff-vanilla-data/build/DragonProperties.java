import java.io.InputStream;
import java.io.PrintWriter;
import java.lang.classfile.ClassFile;
import java.lang.classfile.ClassModel;
import java.lang.classfile.CodeElement;
import java.lang.classfile.CodeModel;
import java.lang.classfile.MethodModel;
import java.lang.classfile.Opcode;
import java.lang.classfile.instruction.ConstantInstruction;
import java.lang.classfile.instruction.FieldInstruction;
import java.lang.classfile.instruction.ArrayStoreInstruction;
import java.lang.classfile.instruction.InvokeInstruction;
import java.lang.classfile.instruction.NewReferenceArrayInstruction;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.TreeMap;

import net.minecraft.SharedConstants;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.entity.boss.enderdragon.EnderDragon;
import net.minecraft.world.entity.boss.enderdragon.phases.EnderDragonPhase;

/**
 * Dumps the ender dragon's code-only tables.
 *
 * Output lines:
 *   phase <id> <name>
 *     Every phase in id order, as saved and sent in the dragon's data.
 *   node_adjacency <index> <bits>
 *     The flight graph: bit j of node i's entry links node i to node j.
 *     Read from the constants the node setup stores, so a changed graph
 *     changes the output and a changed shape of that code fails here.
 *   part <index> <field> <width> <height>
 *     The dragon's hitbox parts in the order clients number them after the
 *     dragon's own id, with the sizes the constructor gives them.
 */
public class DragonProperties {
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        try (PrintWriter out = new PrintWriter(Files.newBufferedWriter(Path.of(args[0])))) {
            int count = EnderDragonPhase.getCount();
            if (count == 0) {
                throw new IllegalStateException("no dragon phases");
            }
            for (int id = 0; id < count; id++) {
                String name = EnderDragonPhase.getById(id).toString();
                int cut = name.indexOf(" (#");
                if (cut < 0) {
                    throw new IllegalStateException("unexpected phase name " + name);
                }
                out.println("phase " + id + " " + name.substring(0, cut));
            }
            ClassModel model = dragonClass();
            for (var entry : adjacency(model).entrySet()) {
                out.println("node_adjacency " + entry.getKey() + " " + entry.getValue());
            }
            for (String line : parts(model)) {
                out.println(line);
            }
        }
    }

    private static ClassModel dragonClass() throws Exception {
        String resource = EnderDragon.class.getName().replace('.', '/') + ".class";
        try (InputStream in = EnderDragon.class.getClassLoader().getResourceAsStream(resource)) {
            return ClassFile.of().parse(in.readAllBytes());
        }
    }

    /** Part sizes from each part's construction, then their array order. */
    private static List<String> parts(ClassModel model) {
        String part = "net/minecraft/world/entity/boss/enderdragon/EnderDragonPart";
        Map<String, float[]> sizes = new HashMap<>();
        List<String> order = new ArrayList<>();
        for (MethodModel method : model.methods()) {
            if (!method.methodName().stringValue().equals("<init>")) {
                continue;
            }
            List<Float> floats = new ArrayList<>();
            float[] built = null;
            boolean array = false;
            String pending = null;
            for (CodeElement element : method.code().orElseThrow()) {
                if (element instanceof ConstantInstruction constant) {
                    if (constant.constantValue() instanceof Float value) {
                        floats.add(value);
                    }
                } else if (element instanceof InvokeInstruction invoke
                    && invoke.owner().asInternalName().equals(part)
                    && invoke.name().stringValue().equals("<init>")) {
                    if (floats.size() < 2) {
                        throw new IllegalStateException("part built without two sizes");
                    }
                    built = new float[] {floats.get(floats.size() - 2), floats.get(floats.size() - 1)};
                    floats.clear();
                } else if (element instanceof FieldInstruction field
                    && field.opcode() == Opcode.PUTFIELD && built != null) {
                    sizes.put(field.name().stringValue(), built);
                    built = null;
                } else if (element instanceof NewReferenceArrayInstruction newArray
                    && newArray.componentType().asInternalName().equals(part)) {
                    array = true;
                } else if (array && element instanceof FieldInstruction field
                    && field.opcode() == Opcode.GETFIELD) {
                    pending = field.name().stringValue();
                } else if (array && element instanceof ArrayStoreInstruction store
                    && store.opcode() == Opcode.AASTORE) {
                    if (pending == null) {
                        throw new IllegalStateException("part array holds something else");
                    }
                    order.add(pending);
                    pending = null;
                } else if (array && element instanceof FieldInstruction field
                    && field.opcode() == Opcode.PUTFIELD) {
                    array = false;
                }
            }
        }
        if (order.isEmpty()) {
            throw new IllegalStateException("no dragon parts found");
        }
        List<String> lines = new ArrayList<>();
        for (int i = 0; i < order.size(); i++) {
            float[] size = sizes.get(order.get(i));
            if (size == null) {
                throw new IllegalStateException("no size for part " + order.get(i));
            }
            lines.add("part " + i + " " + order.get(i) + " " + size[0] + " " + size[1]);
        }
        return lines;
    }

    private static TreeMap<Integer, Integer> adjacency(ClassModel model) {
        TreeMap<Integer, Integer> table = new TreeMap<>();
        for (MethodModel method : model.methods()) {
            if (!method.methodName().stringValue().equals("findClosestNode")
                || !method.methodType().stringValue().equals("()I")) {
                continue;
            }
            CodeModel code = method.code().orElseThrow();
            boolean array = false;
            List<Integer> constants = new ArrayList<>();
            for (CodeElement element : code) {
                if (element instanceof FieldInstruction field
                    && field.opcode() == Opcode.GETFIELD
                    && field.name().stringValue().equals("nodeAdjacency")) {
                    array = true;
                    constants.clear();
                } else if (array && element instanceof ConstantInstruction constant
                    && constant.constantValue() instanceof Integer value) {
                    constants.add(value);
                } else if (array && element instanceof ArrayStoreInstruction store
                    && store.opcode() == Opcode.IASTORE) {
                    if (constants.size() != 2) {
                        throw new IllegalStateException("unexpected node adjacency store");
                    }
                    table.put(constants.get(0), constants.get(1));
                    array = false;
                } else if (array && element instanceof java.lang.classfile.Instruction) {
                    array = false;
                }
            }
        }
        if (table.size() != 24 || table.firstKey() != 0 || table.lastKey() != 23) {
            throw new IllegalStateException("found " + table.size() + " node adjacency entries");
        }
        return table;
    }
}
