use super::*;
use tuff_world_model::BlockPos;

fn write_component_patch<M: Mode>(w: &mut PacketWriter, stack: ItemView<'_, M>) {
    let added = stack.components.network_added();
    let removed = stack.components.network_removed();
    w.write_var_int(added.len() as i32);
    w.write_var_int(removed.len() as i32);
    for component in &added {
        w.write_var_int(component.kind().id() as i32);
        w.write_component(component);
    }
    for kind in removed {
        w.write_var_int(kind.id() as i32);
    }
}

impl WriteExt for PacketWriter {
    fn write_position(&mut self, pos: BlockPos) {
        self.write_long(pos.pack());
    }

    fn write_angle(&mut self, degrees: f32) {
        self.write_unsigned_byte(((degrees / 360.0 * 256.0).floor() as i32) as u8);
    }

    fn write_lp_vec3(&mut self, v: [f64; 3]) {
        let v = v.map(|c| {
            if c.is_finite() {
                c.clamp(-LP_VEC3_ABS_MAX, LP_VEC3_ABS_MAX)
            } else {
                0.0
            }
        });
        let max = v.iter().fold(0.0f64, |m, c| m.max(c.abs()));
        if max < 3.051944088384301e-5 {
            self.write_unsigned_byte(0);
            return;
        }
        let scale = max.ceil() as u64;
        let pack = |c: f64| ((c / scale as f64 * 0.5 + 0.5) * 32766.0).round() as u64 & 0x7FFF;
        let continuation = scale > 3;
        let low_scale = if continuation { scale & 3 } else { scale };
        let packed = low_scale
            | if continuation { 4 } else { 0 }
            | (pack(v[0]) << 3)
            | (pack(v[1]) << 18)
            | (pack(v[2]) << 33);
        self.write_unsigned_byte(packed as u8);
        self.write_unsigned_byte((packed >> 8) as u8);
        self.write_int((packed >> 16) as u32 as i32);
        if continuation {
            self.write_var_int((scale >> 2) as i32);
        }
    }

    fn write_slot<M: Mode>(&mut self, stack: Option<ItemView<'_, M>>) {
        let Some(stack) = stack else {
            self.write_var_int(0);
            return;
        };
        self.write_var_int(stack.count() as i32);
        self.write_var_int(stack.item().id() as i32);
        write_component_patch(self, stack);
    }

    fn write_any_slot(&mut self, stack: Option<tuff_item::item::AnyItem<'_>>) {
        match stack {
            None => self.write_slot::<tuff_item::item::Counted>(None),
            Some(stack) => {
                tuff_item::with_item!(stack, |stack| self.write_slot(Some(stack.view())))
            }
        }
    }

    fn write_item_template<M: Mode>(&mut self, stack: ItemView<'_, M>) {
        self.write_var_int(stack.item().id() as i32);
        self.write_var_int(stack.count() as i32);
        write_component_patch(self, stack);
    }

    fn write_component<M: Mode>(&mut self, component: &NetworkComponent<'_, M>) {
        let nbt = |w: &mut Self, tag: &Tag| {
            w.write_nbt(tag)
                .expect("stored NBT is decoded within the depth limit");
        };
        match component {
            NetworkComponent::Damage(v)
            | NetworkComponent::MaxDamage(v)
            | NetworkComponent::RepairCost(v)
            | NetworkComponent::OminousBottleAmplifier(v)
            | NetworkComponent::MapId(v)
            | NetworkComponent::MaxStackSize(v)
            | NetworkComponent::Rarity(v) => self.write_var_int(*v as i32),
            NetworkComponent::Identifier(_, id) => self.write_string(id),
            NetworkComponent::GlintOverride(glint) => self.write_bool(*glint),
            NetworkComponent::CustomModelData(data) => {
                self.write_var_int(data.floats.len() as i32);
                for value in &data.floats {
                    self.write_float(*value);
                }
                self.write_var_int(data.flags.len() as i32);
                for flag in &data.flags {
                    self.write_bool(*flag);
                }
                self.write_var_int(data.strings.len() as i32);
                for string in &data.strings {
                    self.write_string(string);
                }
                self.write_var_int(data.colors.len() as i32);
                for color in &data.colors {
                    self.write_int(*color);
                }
            }
            NetworkComponent::TooltipDisplay(display) => {
                self.write_bool(display.hide_tooltip);
                self.write_var_int(display.hidden.len() as i32);
                for kind in &display.hidden {
                    self.write_var_int(kind.id() as i32);
                }
            }
            NetworkComponent::ClientData(data) => nbt(self, &Tag::Compound(data.clone())),
            NetworkComponent::LodestoneTracker(tracker) => {
                self.write_bool(tracker.target.is_some());
                if let Some((dimension, [x, y, z])) = &tracker.target {
                    self.write_string(dimension);
                    self.write_position(BlockPos::new(*x, *y, *z));
                }
                self.write_bool(tracker.tracked);
            }
            NetworkComponent::Template(_, stack) => self.write_item_template(stack.view()),
            NetworkComponent::Unbreakable => {}
            NetworkComponent::CustomName(name) => self.write_text(name),
            NetworkComponent::Enchantments(e) | NetworkComponent::StoredEnchantments(e) => {
                let entries: Vec<(u32, i32)> = e.network_entries().collect();
                self.write_var_int(entries.len() as i32);
                for (index, level) in entries {
                    self.write_var_int(index as i32);
                    self.write_var_int(level);
                }
            }
            NetworkComponent::PotionContents(contents) => {
                self.write_bool(contents.potion.is_some());
                if let Some(potion) = contents.potion {
                    self.write_var_int(potion.id() as i32);
                }
                self.write_bool(contents.custom_color.is_some());
                if let Some(color) = contents.custom_color {
                    self.write_int(color);
                }
                self.write_var_int(contents.custom_effects.len() as i32);
                for effect in &contents.custom_effects {
                    self.write_var_int(effect.effect.id() as i32);
                    self.write_effect_details(&effect.details);
                }
                self.write_bool(contents.custom_name.is_some());
                if let Some(name) = &contents.custom_name {
                    self.write_string(name);
                }
            }
            NetworkComponent::Lore(lines) => {
                self.write_var_int(lines.len() as i32);
                for line in *lines {
                    nbt(self, line);
                }
            }
            NetworkComponent::Container(contents) => {
                self.write_var_int(contents.len() as i32);
                for stack in *contents {
                    self.write_bool(stack.is_some());
                    if let Some(stack) = stack {
                        self.write_item_template(stack.view());
                    }
                }
            }
            NetworkComponent::ChargedProjectiles(projectiles) => {
                self.write_var_int(projectiles.len() as i32);
                for stack in *projectiles {
                    self.write_item_template(stack.view());
                }
            }
            NetworkComponent::BundleContents(contents) => {
                self.write_var_int(contents.len() as i32);
                for stack in contents.items() {
                    self.write_item_template(stack.view());
                }
            }
            NetworkComponent::DyedColor(rgb) => self.write_int(*rgb),
            NetworkComponent::BaseColor(color) => self.write_var_int(color.id() as i32),
            NetworkComponent::BannerPatterns(layers) => {
                self.write_var_int(layers.len() as i32);
                for layer in *layers {
                    match (&layer.inline, layer.network_id()) {
                        (Some(key), _) => {
                            self.write_var_int(0);
                            self.write_string(&layer.pattern);
                            self.write_string(key);
                        }
                        (None, id) => {
                            let id = id.expect("checked before listing");
                            self.write_var_int(id as i32 + 1);
                        }
                    }
                    self.write_var_int(layer.color.id() as i32);
                }
            }
            NetworkComponent::Trim(trim) => {
                let (material, pattern) = trim.network_ids();
                match (&trim.inline_material, material) {
                    (Some(inline), _) => {
                        self.write_var_int(0);
                        self.write_string(&inline.palette_id);
                        self.write_text(&inline.description);
                    }
                    (None, id) => {
                        self.write_var_int(id.expect("checked before listing") as i32 + 1)
                    }
                }
                match (&trim.inline_pattern, pattern) {
                    (Some(inline), _) => {
                        self.write_var_int(0);
                        self.write_string(&inline.asset_id);
                        self.write_text(&inline.description);
                        self.write_bool(inline.decal);
                    }
                    (None, id) => {
                        self.write_var_int(id.expect("checked before listing") as i32 + 1)
                    }
                }
            }
            NetworkComponent::Fireworks(fireworks) => {
                self.write_var_int(fireworks.flight_duration as i32);
                self.write_var_int(fireworks.explosions.len() as i32);
                for explosion in &fireworks.explosions {
                    self.write_explosion(explosion);
                }
            }
            NetworkComponent::FireworkExplosion(explosion) => self.write_explosion(explosion),
            NetworkComponent::WritableBook(pages) => {
                self.write_var_int(pages.len() as i32);
                for page in *pages {
                    self.write_filterable_string(page);
                }
            }
            NetworkComponent::WrittenBook(book) => {
                self.write_filterable_string(&book.title);
                self.write_string(&book.author);
                self.write_var_int(book.generation);
                self.write_var_int(book.pages.len() as i32);
                for page in &book.pages {
                    nbt(self, &page.raw);
                    self.write_bool(page.filtered.is_some());
                    if let Some(filtered) = &page.filtered {
                        nbt(self, filtered);
                    }
                }
                self.write_bool(book.resolved);
            }
            NetworkComponent::AttributeModifiers(modifiers) => {
                self.write_var_int(modifiers.len() as i32);
                for modifier in *modifiers {
                    self.write_var_int(modifier.attribute.id() as i32);
                    self.write_string(&modifier.id);
                    self.write_double(modifier.amount);
                    self.write_var_int(modifier.operation.network_id() as i32);
                    self.write_var_int(modifier.slot.network_id() as i32);
                    self.write_var_int(modifier.display.network_id() as i32);
                    if let ModifierDisplay::Override(text) = &modifier.display {
                        nbt(self, text);
                    }
                }
            }
            NetworkComponent::Tool(tool) => {
                self.write_var_int(tool.rules.len() as i32);
                for rule in &tool.rules {
                    match &rule.blocks {
                        BlockSet::Tag { name, .. } => {
                            self.write_var_int(0);
                            self.write_string(name);
                        }
                        BlockSet::Blocks(blocks) => {
                            self.write_var_int(blocks.len() as i32 + 1);
                            for block in blocks {
                                self.write_var_int(block.id() as i32);
                            }
                        }
                    }
                    self.write_bool(rule.speed.is_some());
                    if let Some(speed) = rule.speed {
                        self.write_float(speed);
                    }
                    self.write_bool(rule.correct_for_drops.is_some());
                    if let Some(correct) = rule.correct_for_drops {
                        self.write_bool(correct);
                    }
                }
                self.write_float(tool.default_mining_speed);
                self.write_var_int(tool.damage_per_block as i32);
                self.write_bool(tool.can_destroy_blocks_in_creative);
            }
            NetworkComponent::Weapon(weapon) => {
                self.write_var_int(weapon.item_damage_per_attack as i32);
                self.write_float(weapon.disable_blocking_for_seconds);
            }
            NetworkComponent::SuspiciousStewEffects(effects) => {
                self.write_var_int(effects.len() as i32);
                for effect in *effects {
                    self.write_var_int(effect.effect.id() as i32);
                    self.write_var_int(effect.duration);
                }
            }
            NetworkComponent::BlockState(properties) => {
                let pairs: Vec<_> = properties
                    .iter()
                    .filter_map(|(key, value)| Some((key, value.as_str()?)))
                    .collect();
                self.write_var_int(pairs.len() as i32);
                for (key, value) in pairs {
                    self.write_string(key);
                    self.write_string(value);
                }
            }
            NetworkComponent::PotDecorations(sides) => {
                for side in sides.iter() {
                    self.write_bool(side.is_some());
                    if let Some(stack) = side {
                        self.write_item_template(stack.view());
                    }
                }
            }
            NetworkComponent::Nbt(_, value) => nbt(self, value),
            NetworkComponent::TypedNbt(_, id, tag) => {
                self.write_var_int(*id as i32);
                let mut data = Compound::with_capacity(tag.len());
                for (key, value) in tag.iter().filter(|(key, _)| *key != "id") {
                    data.insert(key, value.clone());
                }
                nbt(self, &Tag::Compound(data));
            }
        }
    }

    fn write_text(&mut self, text: &TextComponent) {
        self.write_nbt(&text.to_nbt())
            .expect("text components are shallow enough to encode");
    }

    fn write_effect_details(&mut self, details: &EffectDetails) {
        self.write_var_int(details.amplifier as i32);
        self.write_var_int(details.duration);
        self.write_bool(details.ambient);
        self.write_bool(details.show_particles);
        self.write_bool(details.show_icon);
        self.write_bool(details.hidden_effect.is_some());
        if let Some(hidden) = &details.hidden_effect {
            self.write_effect_details(hidden);
        }
    }

    fn write_bitset(&mut self, words: &[u64]) {
        let len = words.iter().rposition(|w| *w != 0).map_or(0, |i| i + 1);
        self.write_var_int(len as i32);
        for word in &words[..len] {
            self.write_long(*word as i64);
        }
    }

    fn write_explosion(&mut self, explosion: &FireworkExplosion) {
        self.write_var_int(explosion.shape.id() as i32);
        for colors in [&explosion.colors, &explosion.fade_colors] {
            self.write_var_int(colors.len() as i32);
            for color in colors {
                self.write_int(*color);
            }
        }
        self.write_bool(explosion.has_trail);
        self.write_bool(explosion.has_twinkle);
    }

    fn write_filterable_string(&mut self, value: &Filterable<String>) {
        self.write_string(&value.raw);
        self.write_bool(value.filtered.is_some());
        if let Some(filtered) = &value.filtered {
            self.write_string(filtered);
        }
    }

    fn write_id_list(&mut self, ids: &[u32]) {
        self.write_var_int(ids.len() as i32 + 1);
        for id in ids {
            self.write_var_int(*id as i32);
        }
    }
}
