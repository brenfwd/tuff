import java.io.BufferedReader;
import java.io.IOException;
import java.io.InputStreamReader;
import java.io.OutputStream;
import java.lang.reflect.Method;
import java.net.InetAddress;
import java.net.Socket;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;
import java.util.Locale;
import java.util.function.Consumer;
import net.minecraft.client.KeyMapping;
import net.minecraft.client.Minecraft;
import net.minecraft.client.Screenshot;
import net.minecraft.client.gui.screens.ConnectScreen;
import net.minecraft.client.gui.screens.TitleScreen;
import net.minecraft.client.multiplayer.ServerData;
import net.minecraft.client.multiplayer.resolver.ServerAddress;
import net.minecraft.core.BlockPos;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.world.entity.Entity;
import net.minecraft.world.inventory.ContainerInput;
import net.minecraft.world.level.LightLayer;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.block.state.properties.Property;

public class TuffClientHook {
    private static OutputStream out;
    private static final List<long[]> waiters = new ArrayList<>();
    private static final List<Reply> waiterReplies = new ArrayList<>();
    private static boolean released;

    interface Reply {
        void ok(String payload);

        void err(String message);
    }

    interface Verb {
        void run(Minecraft mc, String[] args, Reply reply) throws Exception;
    }

    public static void main(String[] args) throws Exception {
        Thread watchdog = new Thread(() -> {
            try {
                while (System.in.read() != -1) {
                }
            } catch (Exception ignored) {
            }
            Runtime.getRuntime().halt(0);
        }, "tuff-stdin-watchdog");
        watchdog.setDaemon(true);
        watchdog.start();

        String mainClass = System.getProperty("tuff.harness.mainClass");
        if (mainClass == null) {
            System.err.println("tuff.harness.mainClass is not set");
            System.exit(2);
        }
        String port = System.getProperty("tuff.harness.port");
        if (port != null) {
            String token = System.getProperty("tuff.harness.token", "");
            Thread control = new Thread(() -> serve(Integer.parseInt(port), token), "tuff-harness-control");
            control.setDaemon(true);
            control.start();
            Thread ticker = new Thread(TuffClientHook::pollLoop, "tuff-harness-poll");
            ticker.setDaemon(true);
            ticker.start();
        }
        Method entry = Class.forName(mainClass).getMethod("main", String[].class);
        entry.invoke(null, (Object) args);
    }

    private static void serve(int port, String token) {
        try (Socket socket = new Socket(InetAddress.getLoopbackAddress(), port)) {
            socket.setTcpNoDelay(true);
            out = socket.getOutputStream();
            send("0 hello " + token);
            BufferedReader in = new BufferedReader(new InputStreamReader(socket.getInputStream(), StandardCharsets.UTF_8));
            String line;
            while ((line = in.readLine()) != null) {
                handle(line);
            }
        } catch (IOException e) {
            System.err.println("tuff harness control closed: " + e);
        }
    }

    private static synchronized void send(String line) {
        try {
            out.write((line.replace('\n', ' ').replace('\r', ' ') + "\n").getBytes(StandardCharsets.UTF_8));
            out.flush();
        } catch (IOException e) {
            System.err.println("tuff harness send failed: " + e);
        }
    }

    private static void handle(String line) {
        String[] parts = line.split(" ", 3);
        String id = parts[0];
        Reply reply = new Reply() {
            public void ok(String payload) {
                send(id + " ok " + payload);
            }

            public void err(String message) {
                send(id + " err " + message);
            }
        };
        if (parts.length < 2) {
            reply.err("malformed request");
            return;
        }
        String verb = parts[1];
        String rest = parts.length > 2 ? parts[2] : "";
        Minecraft mc = Minecraft.getInstance();
        if (mc == null) {
            reply.err("client not started");
            return;
        }
        mc.execute(() -> {
            try {
                dispatch(mc, verb, rest, reply);
            } catch (Throwable t) {
                reply.err(t.toString());
            }
        });
    }

    private static void pollLoop() {
        while (true) {
            try {
                Thread.sleep(20);
            } catch (InterruptedException e) {
                return;
            }
            Minecraft mc = Minecraft.getInstance();
            if (mc == null) {
                continue;
            }
            mc.execute(() -> {
                tickWaiters(mc);
                autoRelease(mc);
            });
        }
    }

    private static void autoRelease(Minecraft mc) {
        if (mc.player == null) {
            released = false;
        } else if (!released && mc.mouseHandler.isMouseGrabbed()) {
            mc.mouseHandler.releaseMouse();
            released = true;
        }
    }

    private static void tickWaiters(Minecraft mc) {
        if (waiters.isEmpty()) {
            return;
        }
        long now = mc.level == null ? -1 : mc.level.getGameTime();
        for (int i = waiters.size() - 1; i >= 0; i--) {
            if (now < 0) {
                waiters.remove(i);
                waiterReplies.remove(i).err("no level");
            } else if (now >= waiters.get(i)[0]) {
                waiters.remove(i);
                waiterReplies.remove(i).ok("tick=" + now);
            }
        }
    }

    private static Method agent(String name, Class<?>... types) throws Exception {
        Class<?> c;
        try {
            c = Class.forName("tuffagent.TuffAgent", true, ClassLoader.getSystemClassLoader());
        } catch (ClassNotFoundException e) {
            throw new IllegalStateException("agent not loaded");
        }
        return c.getMethod(name, types);
    }

    static final class Path {
        static Object eval(Minecraft mc, String expr) throws Exception {
            Object cur = null;
            boolean first = true;
            for (String seg : split(expr, '.')) {
                if (first) {
                    if (!seg.equals("mc")) {
                        throw new IllegalArgumentException("path must start with mc");
                    }
                    cur = mc;
                    first = false;
                } else {
                    cur = step(cur, seg);
                }
            }
            return cur;
        }

        @SuppressWarnings("unchecked")
        static void set(Minecraft mc, String path, String value) throws Exception {
            List<String> segs = split(path, '.');
            if (segs.size() < 2) {
                throw new IllegalArgumentException("path too short");
            }
            Object owner = eval(mc, String.join(".", segs.subList(0, segs.size() - 1)));
            java.lang.reflect.Field f = field(owner.getClass(), segs.get(segs.size() - 1));
            Object current = f.get(owner);
            if (current instanceof net.minecraft.client.OptionInstance<?> option) {
                Object old = option.get();
                ((net.minecraft.client.OptionInstance<Object>) option).set(convert(old.getClass(), value));
                return;
            }
            f.set(owner, convert(f.getType(), value));
        }

        static Object step(Object cur, String seg) throws Exception {
            if (cur == null) {
                throw new IllegalStateException("null before " + seg);
            }
            int open = seg.indexOf('(');
            if (open < 0) {
                return field(cur.getClass(), seg).get(cur);
            }
            String name = seg.substring(0, open);
            if (!seg.endsWith(")")) {
                throw new IllegalArgumentException("bad call " + seg);
            }
            String inner = seg.substring(open + 1, seg.length() - 1).trim();
            List<String> args = inner.isEmpty() ? List.of() : split(inner, ',');
            for (Class<?> c = cur.getClass(); c != null; c = c.getSuperclass()) {
                List<Method> all = new ArrayList<>(List.of(c.getDeclaredMethods()));
                all.addAll(List.of(c.getMethods()));
                for (Method m : all) {
                    if (!m.getName().equals(name) || m.getParameterCount() != args.size()) {
                        continue;
                    }
                    try {
                        Object[] values = new Object[args.size()];
                        for (int i = 0; i < values.length; i++) {
                            values[i] = convert(m.getParameterTypes()[i], args.get(i).trim());
                        }
                        m.setAccessible(true);
                        return m.invoke(cur, values);
                    } catch (IllegalArgumentException skip) {
                    }
                }
            }
            throw new NoSuchMethodException(name + "/" + args.size());
        }

        static java.lang.reflect.Field field(Class<?> start, String name) throws Exception {
            for (Class<?> c = start; c != null; c = c.getSuperclass()) {
                try {
                    java.lang.reflect.Field f = c.getDeclaredField(name);
                    f.setAccessible(true);
                    return f;
                } catch (NoSuchFieldException skip) {
                }
            }
            throw new NoSuchFieldException(name);
        }

        @SuppressWarnings({"unchecked", "rawtypes"})
        static Object convert(Class<?> type, String s) {
            if (type == int.class || type == Integer.class) {
                return Integer.parseInt(s);
            } else if (type == long.class || type == Long.class) {
                return Long.parseLong(s);
            } else if (type == float.class || type == Float.class) {
                return Float.parseFloat(s);
            } else if (type == double.class || type == Double.class) {
                return Double.parseDouble(s);
            } else if (type == boolean.class || type == Boolean.class) {
                if (!s.equals("true") && !s.equals("false")) {
                    throw new IllegalArgumentException("not a boolean");
                }
                return Boolean.parseBoolean(s);
            } else if (type == String.class) {
                return s.length() >= 2 && s.startsWith("\"") && s.endsWith("\"") ? s.substring(1, s.length() - 1) : s;
            } else if (type.isEnum()) {
                return Enum.valueOf((Class<Enum>) type, s);
            }
            throw new IllegalArgumentException("unsupported type " + type);
        }

        static List<String> split(String s, char sep) {
            List<String> out = new ArrayList<>();
            int depth = 0;
            boolean quoted = false;
            int start = 0;
            for (int i = 0; i < s.length(); i++) {
                char c = s.charAt(i);
                if (c == '"') {
                    quoted = !quoted;
                } else if (!quoted && c == '(') {
                    depth++;
                } else if (!quoted && c == ')') {
                    depth--;
                } else if (!quoted && depth == 0 && c == sep) {
                    out.add(s.substring(start, i));
                    start = i + 1;
                }
            }
            out.add(s.substring(start));
            return out;
        }
    }

    private static String clean(String value) {
        return value.replaceAll("\\s+", "_");
    }

    private static KeyMapping key(Minecraft mc, String name) {
        return switch (name) {
            case "forward" -> mc.options.keyUp;
            case "back" -> mc.options.keyDown;
            case "left" -> mc.options.keyLeft;
            case "right" -> mc.options.keyRight;
            case "jump" -> mc.options.keyJump;
            case "sneak" -> mc.options.keyShift;
            case "sprint" -> mc.options.keySprint;
            case "use" -> mc.options.keyUse;
            case "attack" -> mc.options.keyAttack;
            case "drop" -> mc.options.keyDrop;
            case "inventory" -> mc.options.keyInventory;
            default -> throw new IllegalArgumentException("unknown key " + name);
        };
    }

    private static void requirePlayer(Minecraft mc) {
        if (mc.player == null || mc.level == null) {
            throw new IllegalStateException("not in a world");
        }
    }

    private static BlockPos pos(String[] a) {
        return new BlockPos(Integer.parseInt(a[0]), Integer.parseInt(a[1]), Integer.parseInt(a[2]));
    }

    private static String blockString(BlockState state) {
        StringBuilder sb = new StringBuilder(BuiltInRegistries.BLOCK.getKey(state.getBlock()).toString());
        boolean first = true;
        for (Property<?> p : state.getProperties()) {
            sb.append(first ? "[" : ",");
            first = false;
            sb.append(p.getName()).append('=').append(valueName(state, p));
        }
        if (!first) {
            sb.append(']');
        }
        return sb.toString();
    }

    private static <T extends Comparable<T>> String valueName(BlockState state, Property<T> p) {
        return p.getName(state.getValue(p));
    }

    private static void dispatch(Minecraft mc, String verb, String rest, Reply reply) throws Exception {
        String[] a = rest.isEmpty() ? new String[0] : rest.split(" ");
        switch (verb) {
            case "state" -> {
                boolean inWorld = mc.player != null && mc.level != null;
                StringBuilder sb = new StringBuilder();
                sb.append("connected=").append(inWorld && mc.getConnection() != null);
                sb.append(" screen=").append(mc.gui.screen() == null ? "none" : mc.gui.screen().getClass().getSimpleName());
                sb.append(" title=").append(mc.gui.screen() == null ? "" : clean(mc.gui.screen().getTitle().getString()));
                if (inWorld) {
                    sb.append(String.format(Locale.ROOT, " x=%.3f y=%.3f z=%.3f yaw=%.2f pitch=%.2f",
                        mc.player.getX(), mc.player.getY(), mc.player.getZ(), mc.player.getYRot(), mc.player.getXRot()));
                    sb.append(" dim=").append(mc.level.dimension().identifier());
                    sb.append(" mode=").append(mc.gameMode.getPlayerMode().getName());
                    sb.append(String.format(Locale.ROOT, " health=%.1f", mc.player.getHealth()));
                    sb.append(" chunks=").append(mc.level.getChunkSource().getLoadedChunksCount());
                    sb.append(" entities=").append(mc.level.getEntityCount());
                    sb.append(" tick=").append(mc.level.getGameTime());
                    sb.append(" hardcore=").append(mc.level.getLevelData().isHardcore());
                }
                reply.ok(sb.toString());
            }
            case "block" -> {
                requirePlayer(mc);
                reply.ok("state=" + blockString(mc.level.getBlockState(pos(a))));
            }
            case "light" -> {
                requirePlayer(mc);
                BlockPos p = pos(a);
                reply.ok("block=" + mc.level.getBrightness(LightLayer.BLOCK, p) + " sky=" + mc.level.getBrightness(LightLayer.SKY, p));
            }
            case "entities" -> {
                requirePlayer(mc);
                double r = Double.parseDouble(a[0]);
                List<String> found = new ArrayList<>();
                for (Entity e : mc.level.entitiesForRendering()) {
                    if (e.distanceToSqr(mc.player) <= r * r) {
                        found.add(String.format(Locale.ROOT, "%d:%s:%.3f:%.3f:%.3f", e.getId(),
                            BuiltInRegistries.ENTITY_TYPE.getKey(e.getType()), e.getX(), e.getY(), e.getZ()));
                    }
                }
                reply.ok("list=" + String.join(";", found));
            }
            case "chat" -> {
                requirePlayer(mc);
                mc.getConnection().sendChat(rest);
                reply.ok("");
            }
            case "command" -> {
                requirePlayer(mc);
                mc.getConnection().sendCommand(rest.startsWith("/") ? rest.substring(1) : rest);
                reply.ok("");
            }
            case "look" -> {
                requirePlayer(mc);
                Path.eval(mc, "mc.player.setYRot(" + Float.parseFloat(a[0]) + ")");
                Path.eval(mc, "mc.player.setXRot(" + Float.parseFloat(a[1]) + ")");
                reply.ok("");
            }
            case "hotbar" -> {
                requirePlayer(mc);
                Path.eval(mc, "mc.player.getInventory().setSelectedSlot(" + (Integer.parseInt(a[0]) - 1) + ")");
                reply.ok("");
            }
            case "key" -> {
                KeyMapping k = key(mc, a[0]);
                switch (a[1]) {
                    case "down" -> k.setDown(true);
                    case "up" -> k.setDown(false);
                    default -> throw new IllegalArgumentException("expected down or up");
                }
                reply.ok("");
            }
            case "use" -> {
                requirePlayer(mc);
                KeyMapping.click(mc.options.keyUse.getDefaultKey());
                reply.ok("");
            }
            case "attack" -> {
                requirePlayer(mc);
                KeyMapping.click(mc.options.keyAttack.getDefaultKey());
                reply.ok("");
            }
            case "click" -> {
                requirePlayer(mc);
                ContainerInput mode = ContainerInput.valueOf(a[2].toUpperCase(Locale.ROOT));
                mc.gameMode.handleContainerInput(mc.player.containerMenu.containerId,
                    Integer.parseInt(a[0]), Integer.parseInt(a[1]), mode, mc.player);
                reply.ok("");
            }
            case "close" -> {
                requirePlayer(mc);
                mc.player.closeContainer();
                reply.ok("");
            }
            case "respawn" -> {
                requirePlayer(mc);
                mc.player.respawn();
                reply.ok("");
            }
            case "connect" -> {
                ServerData data = new ServerData("tuff test", a[0], ServerData.Type.OTHER);
                data.setResourcePackStatus(ServerData.ServerPackStatus.ENABLED);
                ConnectScreen.startConnecting(new TitleScreen(), mc, ServerAddress.parseString(a[0]), data, false, null);
                reply.ok("");
            }
            case "disconnect" -> {
                mc.disconnect(new TitleScreen(), false);
                reply.ok("");
            }
            case "screenshot" -> {
                String name = a[0].replaceAll("[^A-Za-z0-9_-]", "_") + ".png";
                Consumer<net.minecraft.network.chat.Component> done = c -> reply.ok("file=" + name);
                Screenshot.grab(mc.gameDirectory, name, mc.gameRenderer.mainRenderTarget(), 1, done);
            }
            case "wait_ticks" -> {
                requirePlayer(mc);
                waiters.add(new long[] {mc.level.getGameTime() + Long.parseLong(a[0])});
                waiterReplies.add(reply);
            }
            case "release_mouse" -> {
                mc.mouseHandler.releaseMouse();
                reply.ok("");
            }
            case "eval" -> reply.ok(String.valueOf(Path.eval(mc, rest)));
            case "set" -> {
                int sep = rest.indexOf(" = ");
                if (sep < 0) {
                    throw new IllegalArgumentException("expected <path> = <value>");
                }
                Path.set(mc, rest.substring(0, sep), rest.substring(sep + 3));
                reply.ok("");
            }
            case "hook" -> {
                agent("hook", String.class, String.class).invoke(null, a[0], a[1]);
                reply.ok("");
            }
            case "hook_hits" -> {
                Method hits = agent("hits", String.class, String.class, String.class);
                reply.ok("before=" + hits.invoke(null, a[0], a[1], "before") + " after=" + hits.invoke(null, a[0], a[1], "after")
                    + " thrown=" + hits.invoke(null, a[0], a[1], "thrown"));
            }
            case "hook_alter" -> {
                agent("alter", String.class, String.class, String.class).invoke(null, a[0], a[1], a[2]);
                reply.ok("");
            }
            case "quit" -> {
                reply.ok("");
                mc.stop();
            }
            default -> reply.err("unknown verb " + verb);
        }
    }
}
