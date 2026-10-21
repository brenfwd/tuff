use super::*;

impl Components {
    /// `state` with the properties this patch's `block_state` sets, as a
    /// placed block item applies them. Unknown or invalid ones are skipped.
    pub fn apply_block_state(
        &self,
        state: tuff_vanilla_data::BlockState,
    ) -> tuff_vanilla_data::BlockState {
        let Some(properties) = self
            .other
            .get_compound(DataComponentType::BlockState.name())
        else {
            return state;
        };
        properties.iter().fold(state, |state, (name, value)| {
            value
                .as_str()
                .and_then(|value| state.with(name, value))
                .unwrap_or(state)
        })
    }

    pub fn is_removed(&self, id: &str) -> bool {
        self.removed
            .binary_search_by(|removed| removed.as_str().cmp(id))
            .is_ok()
    }

    /// Reads a stored `components` compound. Typed components that fail to
    /// parse stay in `other` as they were.
    pub fn from_nbt(nbt: &Compound) -> Self {
        let mut components = Self::default();
        for (key, value) in nbt.iter() {
            if let Some(id) = key.strip_prefix(REMOVED_PREFIX) {
                components.removed.push(id.to_owned());
            } else if !components.parse_typed(key, value) {
                components.other.insert(key, value.clone());
            }
        }
        components.removed.sort();
        components.removed.dedup();
        components.compact();
        components
    }

    pub(super) fn parse_typed(&mut self, key: &str, value: &Tag) -> bool {
        match known_component(key) {
            Some(DataComponentType::Damage) => match as_u32(value) {
                Some(damage) => self.damage = Some(damage),
                None => return false,
            },
            Some(DataComponentType::MaxDamage) => match as_u32(value).filter(|max| *max > 0) {
                Some(max) => self.max_damage = Some(max),
                None => return false,
            },
            Some(DataComponentType::Unbreakable) => match value.as_compound() {
                Some(_) => self.unbreakable = true,
                None => return false,
            },
            Some(DataComponentType::CustomName) => match TextComponent::from_nbt(value) {
                Some(name) => self.custom_name = Some(Box::new(name)),
                None => return false,
            },
            Some(DataComponentType::Enchantments) => match Enchantments::from_nbt(value) {
                Some(enchantments) => self.enchantments = Some(enchantments),
                None => return false,
            },
            Some(DataComponentType::StoredEnchantments) => match Enchantments::from_nbt(value) {
                Some(enchantments) => self.stored_enchantments = Some(enchantments),
                None => return false,
            },
            Some(DataComponentType::PotionContents) => match PotionContents::from_nbt(value) {
                Some(contents) => self.potion_contents = Some(Box::new(contents)),
                None => return false,
            },
            Some(DataComponentType::RepairCost) => match as_u32(value) {
                Some(cost) => self.repair_cost = Some(cost),
                None => return false,
            },
            Some(DataComponentType::OminousBottleAmplifier) => match as_u32(value) {
                Some(amplifier) if amplifier <= 4 => {
                    self.ominous_bottle_amplifier = Some(amplifier)
                }
                _ => return false,
            },
            Some(DataComponentType::MapId) => match as_u32(value) {
                Some(id) => self.map_id = Some(id),
                None => return false,
            },
            Some(DataComponentType::Container) => match container_from_nbt(value) {
                Some(contents) => self.container = Some(contents),
                None => return false,
            },
            Some(DataComponentType::ChargedProjectiles) => match charged_from_nbt(value) {
                Some(projectiles) => self.charged_projectiles = Some(projectiles),
                None => return false,
            },
            Some(DataComponentType::BundleContents) => match BundleContents::from_nbt(value) {
                Some(contents) => self.bundle_contents = Some(Box::new(contents)),
                None => return false,
            },
            Some(DataComponentType::DyedColor) => match decor::dyed_color_from_nbt(value) {
                Some(rgb) => self.dyed_color = Some(rgb),
                None => return false,
            },
            Some(DataComponentType::BaseColor) => {
                match value.as_str().and_then(DyeColor::from_name) {
                    Some(color) => self.base_color = Some(color),
                    None => return false,
                }
            }
            Some(DataComponentType::BannerPatterns) => {
                match decor::banner_patterns_from_nbt(value) {
                    Some(layers) => self.banner_patterns = Some(layers),
                    None => return false,
                }
            }
            Some(DataComponentType::Trim) => match ArmorTrim::from_nbt(value) {
                Some(trim) => self.trim = Some(Box::new(trim)),
                None => return false,
            },
            Some(DataComponentType::Fireworks) => match Fireworks::from_nbt(value) {
                Some(fireworks) => self.fireworks = Some(Box::new(fireworks)),
                None => return false,
            },
            Some(DataComponentType::FireworkExplosion) => {
                match FireworkExplosion::from_nbt(value) {
                    Some(explosion) => self.firework_explosion = Some(Box::new(explosion)),
                    None => return false,
                }
            }
            Some(DataComponentType::WritableBookContent) => {
                match decor::writable_book_from_nbt(value) {
                    Some(pages) => self.writable_book = Some(pages),
                    None => return false,
                }
            }
            Some(DataComponentType::WrittenBookContent) => match WrittenBook::from_nbt(value) {
                Some(book) => self.written_book = Some(Box::new(book)),
                None => return false,
            },
            Some(DataComponentType::AttributeModifiers) => match gear::modifiers_from_nbt(value) {
                Some(modifiers) => self.attribute_modifiers = Some(modifiers),
                None => return false,
            },
            Some(DataComponentType::Tool) => match Tool::from_nbt(value) {
                Some(tool) => self.tool = Some(Box::new(tool)),
                None => return false,
            },
            Some(DataComponentType::Weapon) => match gear::weapon_from_nbt(value) {
                Some(weapon) => self.weapon = Some(weapon),
                None => return false,
            },
            Some(DataComponentType::SuspiciousStewEffects) => {
                match suspicious_stew_effects_from_nbt(value) {
                    Some(effects) => self.suspicious_stew_effects = Some(effects),
                    None => return false,
                }
            }
            _ => return false,
        }
        true
    }

    pub fn to_nbt(&self) -> Compound {
        let mut nbt = Compound::new();
        if let Some(damage) = self.damage {
            nbt.insert(DataComponentType::Damage.name(), damage as i32);
        }
        if let Some(max) = self.max_damage {
            nbt.insert(DataComponentType::MaxDamage.name(), max as i32);
        }
        if self.unbreakable {
            nbt.insert(DataComponentType::Unbreakable.name(), Compound::new());
        }
        if let Some(name) = &self.custom_name {
            nbt.insert(DataComponentType::CustomName.name(), name.to_nbt());
        }
        if let Some(enchantments) = &self.enchantments {
            nbt.insert(
                DataComponentType::Enchantments.name(),
                enchantments.to_nbt(),
            );
        }
        if let Some(enchantments) = &self.stored_enchantments {
            nbt.insert(
                DataComponentType::StoredEnchantments.name(),
                enchantments.to_nbt(),
            );
        }
        if let Some(contents) = &self.potion_contents {
            nbt.insert(DataComponentType::PotionContents.name(), contents.to_nbt());
        }
        if let Some(cost) = self.repair_cost {
            nbt.insert(DataComponentType::RepairCost.name(), cost as i32);
        }
        if let Some(amplifier) = self.ominous_bottle_amplifier {
            nbt.insert(
                DataComponentType::OminousBottleAmplifier.name(),
                amplifier as i32,
            );
        }
        if let Some(id) = self.map_id {
            nbt.insert(DataComponentType::MapId.name(), id as i32);
        }
        if let Some(contents) = &self.container {
            nbt.insert(
                DataComponentType::Container.name(),
                container_to_nbt(contents),
            );
        }
        if let Some(projectiles) = &self.charged_projectiles {
            nbt.insert(
                DataComponentType::ChargedProjectiles.name(),
                Tag::List(
                    projectiles
                        .iter()
                        .map(|stack| Tag::Compound(stack.to_nbt()))
                        .collect(),
                ),
            );
        }
        if let Some(contents) = &self.bundle_contents {
            nbt.insert(DataComponentType::BundleContents.name(), contents.to_nbt());
        }
        if let Some(rgb) = self.dyed_color {
            nbt.insert(DataComponentType::DyedColor.name(), rgb);
        }
        if let Some(color) = self.base_color {
            nbt.insert(DataComponentType::BaseColor.name(), color.name());
        }
        if let Some(layers) = &self.banner_patterns {
            nbt.insert(
                DataComponentType::BannerPatterns.name(),
                decor::banner_patterns_to_nbt(layers),
            );
        }
        if let Some(trim) = &self.trim {
            nbt.insert(DataComponentType::Trim.name(), trim.to_nbt());
        }
        if let Some(fireworks) = &self.fireworks {
            nbt.insert(DataComponentType::Fireworks.name(), fireworks.to_nbt());
        }
        if let Some(explosion) = &self.firework_explosion {
            nbt.insert(
                DataComponentType::FireworkExplosion.name(),
                explosion.to_nbt(),
            );
        }
        if let Some(pages) = &self.writable_book {
            nbt.insert(
                DataComponentType::WritableBookContent.name(),
                decor::writable_book_to_nbt(pages),
            );
        }
        if let Some(book) = &self.written_book {
            nbt.insert(DataComponentType::WrittenBookContent.name(), book.to_nbt());
        }
        if let Some(modifiers) = &self.attribute_modifiers {
            nbt.insert(
                DataComponentType::AttributeModifiers.name(),
                gear::modifiers_to_nbt(modifiers),
            );
        }
        if let Some(tool) = &self.tool {
            nbt.insert(DataComponentType::Tool.name(), tool.to_nbt());
        }
        if let Some(weapon) = &self.weapon {
            nbt.insert(
                DataComponentType::Weapon.name(),
                gear::weapon_to_nbt(weapon),
            );
        }
        if let Some(effects) = &self.suspicious_stew_effects {
            nbt.insert(
                DataComponentType::SuspiciousStewEffects.name(),
                suspicious_stew_effects_to_nbt(effects),
            );
        }
        for (key, value) in self.other.iter() {
            nbt.insert(key, value.clone());
        }
        for id in &self.removed {
            nbt.insert(format!("{REMOVED_PREFIX}{id}"), Compound::new());
        }
        nbt
    }

    /// The components a Slot carries, in the order they are written.
    /// Components without a wire encoding this server produces are left
    /// out; they still exist in storage.
    pub fn network_added(&self) -> Vec<NetworkComponent<'_>> {
        let mut added = Vec::new();
        if let Some(damage) = self.damage {
            added.push(NetworkComponent::Damage(damage));
        }
        if let Some(max) = self.max_damage {
            added.push(NetworkComponent::MaxDamage(max));
        }
        if self.unbreakable {
            added.push(NetworkComponent::Unbreakable);
        }
        if let Some(name) = &self.custom_name {
            added.push(NetworkComponent::CustomName(name));
        }
        if let Some(enchantments) = &self.enchantments {
            added.push(NetworkComponent::Enchantments(enchantments));
        }
        if let Some(enchantments) = &self.stored_enchantments {
            added.push(NetworkComponent::StoredEnchantments(enchantments));
        }
        if let Some(contents) = &self.potion_contents {
            added.push(NetworkComponent::PotionContents(contents));
        }
        if let Some(cost) = self.repair_cost {
            added.push(NetworkComponent::RepairCost(cost));
        }
        if let Some(amplifier) = self.ominous_bottle_amplifier {
            added.push(NetworkComponent::OminousBottleAmplifier(amplifier));
        }
        if let Some(id) = self.map_id {
            added.push(NetworkComponent::MapId(id));
        }
        if let Some(contents) = &self.container {
            added.push(NetworkComponent::Container(contents));
        }
        if let Some(projectiles) = &self.charged_projectiles {
            added.push(NetworkComponent::ChargedProjectiles(projectiles));
        }
        if let Some(contents) = &self.bundle_contents {
            added.push(NetworkComponent::BundleContents(contents));
        }
        if let Some(rgb) = self.dyed_color {
            added.push(NetworkComponent::DyedColor(rgb));
        }
        if let Some(color) = self.base_color {
            added.push(NetworkComponent::BaseColor(color));
        }
        if let Some(layers) = &self.banner_patterns
            && layers.iter().all(BannerLayer::is_sendable)
        {
            added.push(NetworkComponent::BannerPatterns(layers));
        }
        if let Some(trim) = &self.trim
            && trim.is_sendable()
        {
            added.push(NetworkComponent::Trim(trim));
        }
        if let Some(fireworks) = &self.fireworks {
            added.push(NetworkComponent::Fireworks(fireworks));
        }
        if let Some(explosion) = &self.firework_explosion {
            added.push(NetworkComponent::FireworkExplosion(explosion));
        }
        if let Some(pages) = &self.writable_book {
            added.push(NetworkComponent::WritableBook(pages));
        }
        if let Some(book) = &self.written_book {
            added.push(NetworkComponent::WrittenBook(book));
        }
        if let Some(modifiers) = &self.attribute_modifiers {
            added.push(NetworkComponent::AttributeModifiers(modifiers));
        }
        if let Some(tool) = &self.tool {
            added.push(NetworkComponent::Tool(tool));
        }
        if let Some(weapon) = self.weapon {
            added.push(NetworkComponent::Weapon(weapon));
        }
        if let Some(effects) = &self.suspicious_stew_effects {
            added.push(NetworkComponent::SuspiciousStewEffects(effects));
        }
        for (key, value) in self.other.iter() {
            let Some(kind) = known_component(key) else {
                continue;
            };
            if kind == DataComponentType::Lore {
                if let Some(lines) = value.as_list() {
                    added.push(NetworkComponent::Lore(lines));
                }
            } else if kind == DataComponentType::BlockState {
                if let Some(properties) = value.as_compound() {
                    added.push(NetworkComponent::BlockState(properties));
                }
            } else if kind == DataComponentType::PotDecorations {
                if let Some(sides) = pot_decorations_from_nbt(value) {
                    added.push(NetworkComponent::PotDecorations(Box::new(sides)));
                }
            } else if kind == DataComponentType::CustomData
                && let Some(view) = crate::item::identity::client_custom_data(value)
            {
                added.push(NetworkComponent::ClientData(view));
            } else if TEMPLATE_ON_WIRE.contains(&kind) {
                if let Some(stack) = value.as_compound().and_then(ItemStack::from_nbt) {
                    added.push(NetworkComponent::Template(kind, Box::new(stack)));
                }
            } else if NBT_ON_WIRE.contains(&kind) {
                added.push(NetworkComponent::Nbt(kind, value));
            } else if let Some(component) = display_component(kind, value) {
                added.push(component);
            } else if TYPED_NBT_ON_WIRE.contains(&kind)
                && let Some(tag) = value.as_compound()
                && let Some(id) = typed_entity_id(kind, tag)
            {
                added.push(NetworkComponent::TypedNbt(kind, id, tag));
            }
        }
        added
    }

    /// The removed default components a Slot carries.
    pub fn network_removed(&self) -> Vec<DataComponentType> {
        self.removed
            .iter()
            .filter_map(|id| known_component(id))
            .collect()
    }

    /// Stores a component received in a Slot. Returns false for a type this
    /// server cannot hold.
    pub fn set_network(&mut self, component: NetworkComponent<'_>) -> bool {
        match component {
            NetworkComponent::Damage(damage) => self.damage = Some(damage),
            NetworkComponent::MaxDamage(max) => self.max_damage = Some(max),
            NetworkComponent::Unbreakable => self.unbreakable = true,
            NetworkComponent::CustomName(name) => self.custom_name = Some(Box::new(name.clone())),
            NetworkComponent::Enchantments(enchantments) => {
                self.enchantments = Some(enchantments.clone())
            }
            NetworkComponent::StoredEnchantments(enchantments) => {
                self.stored_enchantments = Some(enchantments.clone())
            }
            NetworkComponent::PotionContents(contents) => {
                self.potion_contents = Some(Box::new(contents.clone()))
            }
            NetworkComponent::RepairCost(cost) => self.repair_cost = Some(cost),
            NetworkComponent::OminousBottleAmplifier(amplifier) => {
                self.ominous_bottle_amplifier = Some(amplifier)
            }
            NetworkComponent::MapId(id) => self.map_id = Some(id),
            NetworkComponent::Container(contents) => self.container = Some(contents.to_vec()),
            NetworkComponent::ChargedProjectiles(projectiles) => {
                self.charged_projectiles = Some(projectiles.to_vec())
            }
            NetworkComponent::BundleContents(contents) => {
                self.bundle_contents = Some(Box::new(contents.clone()))
            }
            NetworkComponent::DyedColor(rgb) => self.dyed_color = Some(rgb),
            NetworkComponent::BaseColor(color) => self.base_color = Some(color),
            NetworkComponent::BannerPatterns(layers) => {
                self.banner_patterns = Some(layers.to_vec())
            }
            NetworkComponent::Trim(trim) => self.trim = Some(Box::new(trim.clone())),
            NetworkComponent::Fireworks(fireworks) => {
                self.fireworks = Some(Box::new(fireworks.clone()))
            }
            NetworkComponent::FireworkExplosion(explosion) => {
                self.firework_explosion = Some(Box::new(explosion.clone()))
            }
            NetworkComponent::WritableBook(pages) => self.writable_book = Some(pages.to_vec()),
            NetworkComponent::WrittenBook(book) => self.written_book = Some(Box::new(book.clone())),
            NetworkComponent::AttributeModifiers(modifiers) => {
                self.attribute_modifiers = Some(modifiers.to_vec())
            }
            NetworkComponent::Tool(tool) => self.tool = Some(Box::new(tool.clone())),
            NetworkComponent::Weapon(weapon) => self.weapon = Some(weapon),
            NetworkComponent::SuspiciousStewEffects(effects) => {
                self.suspicious_stew_effects = Some(effects.to_vec())
            }
            NetworkComponent::Lore(lines) => {
                self.other
                    .insert(DataComponentType::Lore.name(), Tag::List(lines.to_vec()));
            }
            NetworkComponent::BlockState(properties) => {
                self.other.insert(
                    DataComponentType::BlockState.name(),
                    Tag::Compound(properties.clone()),
                );
            }
            NetworkComponent::PotDecorations(sides) => {
                self.other.insert(
                    DataComponentType::PotDecorations.name(),
                    pot_decorations_to_nbt(&sides),
                );
            }
            NetworkComponent::Nbt(kind, value) => {
                if !NBT_ON_WIRE.contains(&kind) {
                    return false;
                }
                self.other.insert(kind.name(), value.clone());
            }
            NetworkComponent::Template(kind, stack) => {
                if !TEMPLATE_ON_WIRE.contains(&kind) {
                    return false;
                }
                self.other
                    .insert(kind.name(), Tag::Compound(stack.to_nbt()));
            }
            NetworkComponent::TypedNbt(kind, _, tag) => {
                if !TYPED_NBT_ON_WIRE.contains(&kind) {
                    return false;
                }
                self.other.insert(kind.name(), Tag::Compound(tag.clone()));
            }
            NetworkComponent::MaxStackSize(size) => {
                if !(1..=MAX_COUNT as u32).contains(&size) {
                    return false;
                }
                self.other.insert(MAX_STACK_SIZE, size as i32);
            }
            NetworkComponent::Identifier(kind, id) => {
                if !matches!(
                    kind,
                    DataComponentType::ItemModel | DataComponentType::TooltipStyle
                ) {
                    return false;
                }
                self.other.insert(kind.name(), id.as_ref());
            }
            NetworkComponent::Rarity(index) => {
                let Some(name) = display::RARITIES.get(index as usize) else {
                    return false;
                };
                self.other.insert(DataComponentType::Rarity.name(), *name);
            }
            NetworkComponent::GlintOverride(glint) => {
                self.other
                    .insert(DataComponentType::EnchantmentGlintOverride.name(), glint);
            }
            NetworkComponent::CustomModelData(data) => {
                self.other
                    .insert(DataComponentType::CustomModelData.name(), data.to_nbt());
            }
            NetworkComponent::TooltipDisplay(display) => {
                self.other
                    .insert(DataComponentType::TooltipDisplay.name(), display.to_nbt());
            }
            NetworkComponent::ClientData(data) => {
                self.other
                    .insert(DataComponentType::CustomData.name(), Tag::Compound(data));
            }
            NetworkComponent::LodestoneTracker(tracker) => {
                self.other
                    .insert(DataComponentType::LodestoneTracker.name(), tracker.to_nbt());
            }
        }
        true
    }

    pub fn remove_network(&mut self, kind: DataComponentType) {
        let id = kind.name().to_owned();
        if let Err(at) = self.removed.binary_search(&id) {
            self.removed.insert(at, id);
        }
    }
}

impl NetworkComponent<'_> {
    pub fn kind(&self) -> DataComponentType {
        match self {
            Self::Damage(_) => DataComponentType::Damage,
            Self::MaxDamage(_) => DataComponentType::MaxDamage,
            Self::Unbreakable => DataComponentType::Unbreakable,
            Self::CustomName(_) => DataComponentType::CustomName,
            Self::Enchantments(_) => DataComponentType::Enchantments,
            Self::StoredEnchantments(_) => DataComponentType::StoredEnchantments,
            Self::PotionContents(_) => DataComponentType::PotionContents,
            Self::RepairCost(_) => DataComponentType::RepairCost,
            Self::OminousBottleAmplifier(_) => DataComponentType::OminousBottleAmplifier,
            Self::MapId(_) => DataComponentType::MapId,
            Self::Container(_) => DataComponentType::Container,
            Self::ChargedProjectiles(_) => DataComponentType::ChargedProjectiles,
            Self::BundleContents(_) => DataComponentType::BundleContents,
            Self::DyedColor(_) => DataComponentType::DyedColor,
            Self::BaseColor(_) => DataComponentType::BaseColor,
            Self::BannerPatterns(_) => DataComponentType::BannerPatterns,
            Self::Trim(_) => DataComponentType::Trim,
            Self::Fireworks(_) => DataComponentType::Fireworks,
            Self::FireworkExplosion(_) => DataComponentType::FireworkExplosion,
            Self::WritableBook(_) => DataComponentType::WritableBookContent,
            Self::WrittenBook(_) => DataComponentType::WrittenBookContent,
            Self::Lore(_) => DataComponentType::Lore,
            Self::AttributeModifiers(_) => DataComponentType::AttributeModifiers,
            Self::Tool(_) => DataComponentType::Tool,
            Self::Weapon(_) => DataComponentType::Weapon,
            Self::SuspiciousStewEffects(_) => DataComponentType::SuspiciousStewEffects,
            Self::BlockState(_) => DataComponentType::BlockState,
            Self::Nbt(kind, _)
            | Self::TypedNbt(kind, _, _)
            | Self::Template(kind, _)
            | Self::Identifier(kind, _) => *kind,
            Self::PotDecorations(_) => DataComponentType::PotDecorations,
            Self::MaxStackSize(_) => DataComponentType::MaxStackSize,
            Self::Rarity(_) => DataComponentType::Rarity,
            Self::GlintOverride(_) => DataComponentType::EnchantmentGlintOverride,
            Self::CustomModelData(_) => DataComponentType::CustomModelData,
            Self::TooltipDisplay(_) => DataComponentType::TooltipDisplay,
            Self::ClientData(_) => DataComponentType::CustomData,
            Self::LodestoneTracker(_) => DataComponentType::LodestoneTracker,
        }
    }

    /// The value in its serialized shape, as the client hashes it.
    pub fn canon(&self) -> Canon {
        match self {
            Self::Damage(v)
            | Self::MaxDamage(v)
            | Self::RepairCost(v)
            | Self::OminousBottleAmplifier(v)
            | Self::MapId(v) => Canon::Int(*v as i32),
            Self::Unbreakable => Canon::Map(Vec::new()),
            Self::CustomName(name) => name.canon(),
            Self::Enchantments(e) | Self::StoredEnchantments(e) => e.canon(),
            Self::PotionContents(contents) => contents.canon(),
            Self::ChargedProjectiles(projectiles) => {
                Canon::List(projectiles.iter().map(ItemStack::canon).collect())
            }
            Self::Template(_, stack) => stack.canon(),
            Self::Container(contents) => Canon::List(
                contents
                    .iter()
                    .enumerate()
                    .filter_map(|(slot, stack)| Some((slot, stack.as_ref()?)))
                    .map(|(slot, stack)| {
                        Canon::Map(vec![
                            ("slot".into(), Canon::Int(slot as i32)),
                            ("item".into(), stack.canon()),
                        ])
                    })
                    .collect(),
            ),
            Self::BundleContents(contents) => contents.canon(),
            Self::DyedColor(rgb) => Canon::Int(*rgb),
            Self::BaseColor(color) => Canon::str(color.name()),
            Self::BannerPatterns(layers) => decor::banner_patterns_canon(layers),
            Self::Trim(trim) => trim.canon(),
            Self::Fireworks(fireworks) => fireworks.canon(),
            Self::FireworkExplosion(explosion) => explosion.canon(),
            Self::WritableBook(pages) => decor::writable_book_canon(pages),
            Self::WrittenBook(book) => book.canon(),
            Self::Lore(lines) => Canon::List(
                lines
                    .iter()
                    .map(|line| {
                        TextComponent::from_nbt(line)
                            .map_or_else(|| Canon::from_nbt(line), |text| text.canon())
                    })
                    .collect(),
            ),
            Self::AttributeModifiers(modifiers) => gear::modifiers_canon(modifiers),
            Self::Tool(tool) => tool.canon(),
            Self::Weapon(weapon) => gear::weapon_canon(weapon),
            Self::SuspiciousStewEffects(effects) => {
                Canon::List(effects.iter().map(|e| e.canon()).collect())
            }
            Self::BlockState(properties) => Canon::from_nbt(&Tag::Compound((*properties).clone())),
            Self::Nbt(_, value) => Canon::from_nbt(value),
            Self::PotDecorations(sides) => Canon::Map(
                POT_SIDES
                    .iter()
                    .zip(sides.iter())
                    .filter_map(|(name, side)| Some(((*name).to_owned(), side.as_ref()?.canon())))
                    .collect(),
            ),
            Self::TypedNbt(_, _, tag) => Canon::from_nbt(&Tag::Compound((*tag).clone())),
            Self::MaxStackSize(size) => Canon::Int(*size as i32),
            Self::Identifier(_, id) => Canon::str(id.as_ref()),
            Self::Rarity(index) => Canon::str(display::RARITIES[*index as usize]),
            Self::GlintOverride(glint) => Canon::Bool(*glint),
            Self::CustomModelData(data) => data.canon(),
            Self::TooltipDisplay(display) => display.canon(),
            Self::ClientData(data) => Canon::from_nbt(&Tag::Compound(data.clone())),
            Self::LodestoneTracker(tracker) => tracker.canon(),
        }
    }

    pub fn hash(&self) -> i32 {
        self.canon().hash() as i32
    }
}
