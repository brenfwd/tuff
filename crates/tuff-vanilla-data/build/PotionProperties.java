import java.io.PrintWriter;
import java.nio.file.Files;
import java.nio.file.Path;

import net.minecraft.SharedConstants;
import net.minecraft.core.Holder;
import net.minecraft.core.particles.ColorParticleOption;
import net.minecraft.core.particles.ParticleOptions;
import net.minecraft.core.particles.ParticleTypes;
import net.minecraft.core.particles.SimpleParticleType;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.util.ARGB;
import net.minecraft.world.effect.MobEffect;
import net.minecraft.world.effect.MobEffectInstance;
import net.minecraft.world.item.alchemy.Potion;

/**
 * Dumps potion and mob effect data that lives in game code.
 *
 * Output lines:
 *   effect <mob effect> <category> <color> <instantaneous 0/1> <particle>
 *     The particle an instance shows: `color` for the tinted entity effect
 *     particle (alpha 255, or the ambient alpha when ambient), else the
 *     particle type of a plain particle.
 *   ambient_alpha <alpha>
 *   potion <potion> <name> <effect count>
 *   potion_effect <potion> <mob effect> <duration> <amplifier> <ambient 0/1> <visible 0/1> <show icon 0/1>
 */
public class PotionProperties {
    private static String particle(MobEffect effect, boolean ambient) throws Exception {
        Holder<MobEffect> holder = BuiltInRegistries.MOB_EFFECT.wrapAsHolder(effect);
        ParticleOptions options = effect.createParticleOptions(new MobEffectInstance(holder, 20, 0, ambient, true));
        if (options instanceof ColorParticleOption color) {
            if (color.getType() != ParticleTypes.ENTITY_EFFECT) {
                throw new IllegalStateException("unexpected color particle " + color.getType());
            }
            java.lang.reflect.Field field = ColorParticleOption.class.getDeclaredField("color");
            field.setAccessible(true);
            int argb = field.getInt(color);
            if ((argb & 0xFFFFFF) != (effect.getColor() & 0xFFFFFF)) {
                throw new IllegalStateException("particle color differs for " + effect);
            }
            return "color:" + ARGB.alpha(argb);
        }
        if (options instanceof SimpleParticleType simple) {
            return BuiltInRegistries.PARTICLE_TYPE.getKey(simple).toString();
        }
        throw new IllegalStateException("unexpected particle " + options);
    }

    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        try (PrintWriter out = new PrintWriter(Files.newBufferedWriter(Path.of(args[0])))) {
            Integer ambientAlpha = null;
            for (MobEffect effect : BuiltInRegistries.MOB_EFFECT) {
                String plain = particle(effect, false);
                String ambient = particle(effect, true);
                String shown;
                if (plain.startsWith("color:")) {
                    if (!plain.equals("color:255") || !ambient.startsWith("color:")) {
                        throw new IllegalStateException("unexpected alpha for " + effect);
                    }
                    int alpha = Integer.parseInt(ambient.substring(6));
                    if (ambientAlpha != null && ambientAlpha != alpha) {
                        throw new IllegalStateException("ambient alpha differs for " + effect);
                    }
                    ambientAlpha = alpha;
                    shown = "color";
                } else {
                    if (!plain.equals(ambient)) {
                        throw new IllegalStateException("ambient particle differs for " + effect);
                    }
                    shown = plain;
                }
                out.println("effect " + BuiltInRegistries.MOB_EFFECT.getKey(effect)
                    + " " + effect.getCategory().name().toLowerCase()
                    + " " + effect.getColor()
                    + " " + (effect.isInstantaneous() ? 1 : 0)
                    + " " + shown);
            }
            out.println("ambient_alpha " + ambientAlpha);
            for (Potion potion : BuiltInRegistries.POTION) {
                String name = BuiltInRegistries.POTION.getKey(potion).toString();
                out.println("potion " + name + " " + potion.name() + " " + potion.getEffects().size());
                for (MobEffectInstance instance : potion.getEffects()) {
                    out.println("potion_effect " + name
                        + " " + BuiltInRegistries.MOB_EFFECT.getKey(instance.getEffect().value())
                        + " " + instance.getDuration()
                        + " " + instance.getAmplifier()
                        + " " + (instance.isAmbient() ? 1 : 0)
                        + " " + (instance.isVisible() ? 1 : 0)
                        + " " + (instance.showIcon() ? 1 : 0));
                }
            }
        }
    }
}
