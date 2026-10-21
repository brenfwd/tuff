package tuffagent;

import java.lang.classfile.ClassFile;
import java.lang.classfile.ClassModel;
import java.lang.classfile.ClassTransform;
import java.lang.classfile.CodeBuilder;
import java.lang.classfile.CodeElement;
import java.lang.classfile.CodeTransform;
import java.lang.classfile.MethodModel;
import java.lang.classfile.Label;
import java.lang.classfile.MethodTransform;
import java.lang.classfile.TypeKind;
import java.lang.classfile.instruction.LocalVariable;
import java.lang.classfile.instruction.LocalVariableType;
import java.lang.classfile.instruction.ReturnInstruction;
import java.lang.constant.ClassDesc;
import java.lang.constant.ConstantDescs;
import java.lang.constant.MethodTypeDesc;
import java.lang.instrument.ClassFileTransformer;
import java.lang.instrument.Instrumentation;
import java.security.ProtectionDomain;
import java.util.Map;
import java.util.Set;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.atomic.AtomicLong;

public class TuffAgent {
    private static final ClassDesc SELF = ClassDesc.of("tuffagent.TuffAgent");
    private static final MethodTypeDesc HIT = MethodTypeDesc.of(ConstantDescs.CD_void, ConstantDescs.CD_String);
    private static final Map<String, Set<String>> targets = new ConcurrentHashMap<>();
    private static final Map<String, String> alters = new ConcurrentHashMap<>();
    private static final Map<String, AtomicLong> hits = new ConcurrentHashMap<>();
    private static Instrumentation inst;

    public static void premain(String args, Instrumentation instrumentation) {
        inst = instrumentation;
        dropHostInput("net.minecraft.client.KeyboardHandler", "keyPress", "charTyped", "textInput", "textEditing");
        dropHostInput("net.minecraft.client.MouseHandler", "onButton", "onMove", "onScroll", "onDrop", "grabMouse");
        instrumentation.addTransformer(new Hooker(), true);
    }

    private static void dropHostInput(String className, String... methods) {
        for (String method : methods) {
            targets.computeIfAbsent(className, k -> ConcurrentHashMap.newKeySet()).add(method);
            alters.put(className + "." + method, "skip");
        }
    }

    public static void hit(String key) {
        hits.computeIfAbsent(key, k -> new AtomicLong()).incrementAndGet();
    }

    public static void hook(String className, String method) throws Exception {
        if (inst == null) {
            throw new IllegalStateException("agent not loaded");
        }
        targets.computeIfAbsent(className, k -> ConcurrentHashMap.newKeySet()).add(method);
        Class<?> target = Class.forName(className, false, ClassLoader.getSystemClassLoader());
        inst.retransformClasses(target);
    }

    public static void alter(String className, String method, String spec) throws Exception {
        alters.put(className + "." + method, spec);
        hook(className, method);
    }

    public static long hits(String className, String method, String phase) {
        AtomicLong n = hits.get(className + "." + method + ":" + phase);
        return n == null ? 0 : n.get();
    }

    private static final class Hooker implements ClassFileTransformer {
        @Override
        public byte[] transform(ClassLoader loader, String internalName, Class<?> redefined, ProtectionDomain domain, byte[] bytes) {
            if (internalName == null) {
                return null;
            }
            String className = internalName.replace('/', '.');
            Set<String> wanted = targets.get(className);
            if (wanted == null) {
                return null;
            }
            try {
                ClassFile cf = ClassFile.of();
                ClassModel model = cf.parse(bytes);
                ClassTransform transform = (builder, element) -> {
                    if (element instanceof MethodModel mm
                        && mm.code().isPresent()
                        && wanted.contains(mm.methodName().stringValue())) {
                        String base = className + "." + mm.methodName().stringValue();
                        builder.transformMethod(mm, MethodTransform.transformingCode(probes(base, mm, alters.get(base))));
                    } else {
                        builder.with(element);
                    }
                };
                return cf.transformClass(model, transform);
            } catch (Throwable t) {
                System.err.println("tuff agent could not transform " + className + ": " + t);
                return null;
            }
        }
    }

    private static CodeTransform probes(String base, MethodModel mm, String spec) {
        MethodTypeDesc type = mm.methodTypeSymbol();
        boolean skip = "skip".equals(spec);
        String returnValue = spec != null && spec.startsWith("return=") ? spec.substring(7) : null;
        int argIndex = -1;
        String argValue = null;
        if (spec != null && spec.startsWith("arg")) {
            int eq = spec.indexOf('=');
            argIndex = Integer.parseInt(spec.substring(3, eq));
            argValue = spec.substring(eq + 1);
        }
        final int alterArg = argIndex;
        final String alterArgValue = argValue;
        return new CodeTransform() {
            private Label start;
            private Label end;
            private Label handler;

            @Override
            public void atStart(CodeBuilder builder) {
                emit(builder, base + ":before");
                if (skip) {
                    TypeKind rk = TypeKind.from(type.returnType());
                    if (rk != TypeKind.VOID) {
                        constant(builder, rk, rk == TypeKind.REFERENCE ? "null" : "0");
                    }
                    builder.return_(rk);
                }
                if (alterArg >= 0 && alterArg < type.parameterCount()) {
                    int slot = mm.flags().has(java.lang.reflect.AccessFlag.STATIC) ? 0 : 1;
                    for (int i = 0; i < alterArg; i++) {
                        slot += TypeKind.from(type.parameterType(i)).slotSize();
                    }
                    TypeKind pk = TypeKind.from(type.parameterType(alterArg));
                    constant(builder, pk, alterArgValue);
                    builder.storeLocal(pk.asLoadable(), slot);
                }
                start = builder.newBoundLabel();
                end = builder.newLabel();
                handler = builder.newLabel();
            }

            @Override
            public void accept(CodeBuilder builder, CodeElement element) {
                if (element instanceof LocalVariable || element instanceof LocalVariableType) {
                    return;
                }
                if (element instanceof ReturnInstruction) {
                    TypeKind rk = TypeKind.from(type.returnType());
                    emit(builder, base + ":after");
                    if (returnValue != null && rk != TypeKind.VOID) {
                        if (rk.slotSize() == 2) {
                            builder.pop2();
                        } else {
                            builder.pop();
                        }
                        constant(builder, rk, returnValue);
                        builder.return_(rk);
                        return;
                    }
                }
                builder.with(element);
            }

            @Override
            public void atEnd(CodeBuilder builder) {
                builder.labelBinding(end);
                builder.labelBinding(handler);
                emit(builder, base + ":thrown");
                builder.athrow();
                builder.exceptionCatchAll(start, end, handler);
            }
        };
    }

    private static void constant(CodeBuilder builder, TypeKind kind, String value) {
        switch (kind) {
            case REFERENCE -> builder.aconst_null();
            case LONG -> builder.loadConstant(Long.parseLong(value));
            case FLOAT -> builder.loadConstant(Float.parseFloat(value));
            case DOUBLE -> builder.loadConstant(Double.parseDouble(value));
            default -> builder.loadConstant(value.equals("true") ? 1 : value.equals("false") ? 0 : Integer.parseInt(value));
        }
    }

    private static void emit(CodeBuilder builder, String key) {
        builder.ldc(key);
        builder.invokestatic(SELF, "hit", HIT);
    }
}
