use super::*;
use tuff_item::item::MintReason;
use tuff_world_model::BlockPos;

fn read_text_nbt(reader: &mut PacketReader) -> Result<Option<TextComponent>> {
    Ok(reader
        .read_nbt()?
        .and_then(|tag| TextComponent::from_nbt(&tag)))
}

impl ReadExt for PacketReader {
    fn read_position(&mut self) -> Result<BlockPos> {
        Ok(BlockPos::unpack(self.read_long()?))
    }

    fn read_lp_vec3(&mut self) -> Result<[f64; 3]> {
        let first = self.read_unsigned_byte()? as u64;
        if first == 0 {
            return Ok([0.0; 3]);
        }
        let second = self.read_unsigned_byte()? as u64;
        let rest = self.read_int()? as u32 as u64;
        let packed = (rest << 16) | (second << 8) | first;
        let mut scale = first & 3;
        if first & 4 != 0 {
            scale |= (self.read_var_int()? as u32 as u64) << 2;
        }
        let unpack = |v: u64| ((v & 0x7FFF) as f64).min(32766.0) * 2.0 / 32766.0 - 1.0;
        let scale = scale as f64;
        Ok([
            unpack(packed >> 3) * scale,
            unpack(packed >> 18) * scale,
            unpack(packed >> 33) * scale,
        ])
    }

    fn read_hashed_slot(&mut self) -> Result<Option<HashedSlot>> {
        if !self.read_bool()? {
            return Ok(None);
        }
        let item = self.read_var_int()? as u32;
        let count = self.read_var_int()?;
        let added_len = bounded_len(self.read_var_int()?)?;
        let mut added = Vec::with_capacity(added_len);
        for _ in 0..added_len {
            let kind = self.read_var_int()? as u32;
            let hash = self.read_int()?;
            added.push((kind, hash));
        }
        let removed_len = bounded_len(self.read_var_int()?)?;
        let mut removed = Vec::with_capacity(removed_len);
        for _ in 0..removed_len {
            removed.push(self.read_var_int()? as u32);
        }
        Ok(Some(HashedSlot {
            item,
            count,
            added,
            removed,
        }))
    }

    fn read_creative_slot(&mut self) -> Result<CreativeSlot> {
        let count = self.read_var_int()?;
        if count <= 0 {
            return Ok(CreativeSlot::Empty);
        }
        let item = self.read_var_int()?;
        let added = self.read_var_int()?;
        let removed = self.read_var_int()?;
        if !(0..=MAX_SLOT_COMPONENTS).contains(&added)
            || !(0..=MAX_SLOT_COMPONENTS).contains(&removed)
        {
            return Err(BufferError::InvalidEnum(added.max(removed)));
        }
        let mut components = Components::default();
        let mut supported = true;
        for _ in 0..added {
            let kind = self.read_var_int()?;
            let len = self.read_var_int()?;
            let len = usize::try_from(len)
                .ok()
                .filter(|len| *len <= self.remaining())
                .ok_or(BufferError::InvalidArrayLength(len))?;
            let mut payload = PacketReader::new(self.read_bytes(len)?);
            let Some(kind) = u32::try_from(kind)
                .ok()
                .and_then(DataComponentType::from_id)
            else {
                supported = false;
                continue;
            };
            if !payload.read_component(kind, &mut components)? {
                supported = false;
            } else if payload.remaining() != 0 {
                return Err(BufferError::InvalidArrayLength(len as i32));
            }
        }
        for _ in 0..removed {
            let kind = self.read_var_int()?;
            let Some(kind) = u32::try_from(kind)
                .ok()
                .and_then(DataComponentType::from_id)
            else {
                return Ok(CreativeSlot::Unsupported);
            };
            components.remove_network(kind);
        }
        let item = u32::try_from(item).ok().and_then(Item::from_id);
        let shown = match item {
            Some(Item::Air) => return Ok(CreativeSlot::Empty),
            Some(item) => tuff_item::item::ItemSnapshot::describe(item, 1, components),
            None => return Ok(CreativeSlot::Invalid),
        };
        if !supported {
            return Ok(CreativeSlot::Unsupported);
        }
        if count > shown.max_stack_size() as i32 {
            return Ok(CreativeSlot::Invalid);
        }
        Ok(CreativeSlot::Stack(shown.with_count(count as u8)))
    }

    fn read_component(
        &mut self,
        kind: DataComponentType,
        components: &mut Components,
    ) -> Result<bool> {
        self.read_component_at(kind, components, 0)
    }

    fn read_container(&mut self, depth: u32) -> Result<Option<Vec<Option<ItemStack>>>> {
        if depth > MAX_CONTAINER_DEPTH {
            return Err(BufferError::InvalidEnum(depth as i32));
        }
        let len = self.read_var_int()?;
        if !(0..=tuff_item::item::MAX_CONTAINER_SLOTS as i32).contains(&len) {
            return Err(BufferError::InvalidArrayLength(len));
        }
        let mut contents = Vec::with_capacity(len as usize);
        for _ in 0..len {
            if !self.read_bool()? {
                contents.push(None);
                continue;
            }
            let Some(stack) = self.read_item_template(depth)? else {
                return Ok(None);
            };
            contents.push(Some(stack));
        }
        Ok(Some(contents))
    }

    fn read_bundle(&mut self, depth: u32) -> Result<Option<BundleContents>> {
        if depth > MAX_CONTAINER_DEPTH {
            return Err(BufferError::InvalidEnum(depth as i32));
        }
        let len = self.read_var_int()?;
        if !(0..=tuff_item::item::MAX_CONTAINER_SLOTS as i32).contains(&len) {
            return Err(BufferError::InvalidArrayLength(len));
        }
        let mut items = Vec::with_capacity(len as usize);
        for _ in 0..len {
            let Some(stack) = self.read_item_template(depth)? else {
                return Ok(None);
            };
            items.push(stack);
        }
        Ok(Some(BundleContents::from_items(items)))
    }

    fn read_charged_projectiles(&mut self, depth: u32) -> Result<Option<Vec<ItemStack>>> {
        if depth > MAX_CONTAINER_DEPTH {
            return Err(BufferError::InvalidEnum(depth as i32));
        }
        let len = self.read_var_int()?;
        if !(0..=tuff_item::item::MAX_CHARGED_PROJECTILES as i32).contains(&len) {
            return Err(BufferError::InvalidArrayLength(len));
        }
        let mut projectiles = Vec::with_capacity(len as usize);
        for _ in 0..len {
            let Some(stack) = self.read_item_template(depth)? else {
                return Ok(None);
            };
            projectiles.push(stack);
        }
        Ok(Some(projectiles))
    }

    fn read_item_template(&mut self, depth: u32) -> Result<Option<ItemStack>> {
        let item = self.read_var_int()?;
        let item = u32::try_from(item)
            .ok()
            .and_then(Item::from_id)
            .ok_or(BufferError::InvalidEnum(item))?;
        let count = self.read_var_int()?;
        let added = self.read_var_int()?;
        let removed = self.read_var_int()?;
        if !(0..=MAX_SLOT_COMPONENTS).contains(&added)
            || !(0..=MAX_SLOT_COMPONENTS).contains(&removed)
        {
            return Err(BufferError::InvalidEnum(added.max(removed)));
        }
        let mut components = Components::default();
        let mut supported = true;
        for _ in 0..added {
            let kind = self.read_var_int()?;
            let kind = u32::try_from(kind)
                .ok()
                .and_then(DataComponentType::from_id)
                .ok_or(BufferError::InvalidEnum(kind))?;
            supported &= self.read_component_at(kind, &mut components, depth + 1)?;
        }
        for _ in 0..removed {
            let kind = self.read_var_int()?;
            let kind = u32::try_from(kind)
                .ok()
                .and_then(DataComponentType::from_id)
                .ok_or(BufferError::InvalidEnum(kind))?;
            components.remove_network(kind);
        }
        if item == Item::Air || !supported {
            return Ok(None);
        }
        let stack = ItemStack::mint(item, 1, MintReason::Network).with_components(components);
        if !(1..=stack.max_stack_size() as i32).contains(&count) {
            return Err(BufferError::InvalidEnum(count));
        }
        Ok(Some(ItemStack::mint_like(
            &stack,
            count as u8,
            MintReason::Network,
        )))
    }

    fn read_component_at(
        &mut self,
        kind: DataComponentType,
        components: &mut Components,
        depth: u32,
    ) -> Result<bool> {
        let non_negative = |r: &mut Self| {
            let value = r.read_var_int()?;
            u32::try_from(value).map_err(|_| BufferError::InvalidEnum(value))
        };
        let text = |r: &mut Self| {
            let tag = r.read_nbt()?.ok_or(BufferError::InvalidEnum(0))?;
            Ok::<_, BufferError>((TextComponent::from_nbt(&tag), tag))
        };
        let enchantments = |r: &mut Self| {
            let len = r.read_var_int()?;
            if !(0..=MAX_ENCHANTMENTS).contains(&len) {
                return Err(BufferError::InvalidEnum(len));
            }
            let mut entries = Vec::with_capacity(len as usize);
            for _ in 0..len {
                let index = r.read_var_int()?;
                let level = r.read_var_int()?;
                let id = u32::try_from(index)
                    .ok()
                    .and_then(Enchantments::from_network_index)
                    .ok_or(BufferError::InvalidEnum(index))?;
                if !(1..=MAX_ENCHANTMENT_LEVEL).contains(&level) {
                    return Err(BufferError::InvalidEnum(level));
                }
                entries.push((id, level));
            }
            Ok(Enchantments::new(entries))
        };
        let component = match kind {
            DataComponentType::Damage => NetworkComponent::Damage(non_negative(self)?),
            DataComponentType::MaxDamage => {
                let max = non_negative(self)?;
                if max == 0 {
                    return Err(BufferError::InvalidEnum(0));
                }
                NetworkComponent::MaxDamage(max)
            }
            DataComponentType::RepairCost => NetworkComponent::RepairCost(non_negative(self)?),
            DataComponentType::OminousBottleAmplifier => match non_negative(self)? {
                amplifier @ 0..=4 => NetworkComponent::OminousBottleAmplifier(amplifier),
                other => return Err(BufferError::InvalidEnum(other as i32)),
            },
            DataComponentType::MapId => NetworkComponent::MapId(non_negative(self)?),
            DataComponentType::Unbreakable => NetworkComponent::Unbreakable,
            DataComponentType::MaxStackSize => match non_negative(self)? {
                size @ 1..=99 => NetworkComponent::MaxStackSize(size),
                other => return Err(BufferError::InvalidEnum(other as i32)),
            },
            DataComponentType::ItemModel | DataComponentType::TooltipStyle => {
                let id = self.read_string_with_max(MAX_IDENTIFIER_BYTES)?;
                let Some(full) = tuff_item::item::display::full_identifier(&id) else {
                    return Err(BufferError::InvalidEnum(0));
                };
                let full = full.into_owned();
                return Ok(components.set_network(NetworkComponent::Identifier(
                    kind,
                    std::borrow::Cow::Owned(full),
                )));
            }
            DataComponentType::Rarity => match non_negative(self)? {
                index if (index as usize) < tuff_item::item::display::RARITIES.len() => {
                    NetworkComponent::Rarity(index)
                }
                other => return Err(BufferError::InvalidEnum(other as i32)),
            },
            DataComponentType::EnchantmentGlintOverride => {
                NetworkComponent::GlintOverride(self.read_bool()?)
            }
            DataComponentType::CustomModelData => {
                let len = |r: &mut Self| {
                    let len = r.read_var_int()?;
                    usize::try_from(len)
                        .ok()
                        .filter(|len| *len <= tuff_item::item::display::MAX_DISPLAY_ENTRIES)
                        .ok_or(BufferError::InvalidArrayLength(len))
                };
                let mut data = tuff_item::item::display::ModelData::default();
                for _ in 0..len(self)? {
                    data.floats.push(self.read_float()?);
                }
                for _ in 0..len(self)? {
                    data.flags.push(self.read_bool()?);
                }
                for _ in 0..len(self)? {
                    data.strings.push(self.read_string()?);
                }
                for _ in 0..len(self)? {
                    data.colors.push(self.read_int()?);
                }
                NetworkComponent::CustomModelData(data)
            }
            DataComponentType::TooltipDisplay => {
                let hide_tooltip = self.read_bool()?;
                let len = self.read_var_int()?;
                if !(0..=tuff_item::item::display::MAX_DISPLAY_ENTRIES as i32).contains(&len) {
                    return Err(BufferError::InvalidArrayLength(len));
                }
                let mut hidden = Vec::with_capacity(len as usize);
                for _ in 0..len {
                    let id = self.read_var_int()?;
                    let kind = u32::try_from(id)
                        .ok()
                        .and_then(DataComponentType::from_id)
                        .ok_or(BufferError::InvalidEnum(id))?;
                    if !hidden.contains(&kind) {
                        hidden.push(kind);
                    }
                }
                NetworkComponent::TooltipDisplay(tuff_item::item::display::TooltipDisplay {
                    hide_tooltip,
                    hidden,
                })
            }
            DataComponentType::LodestoneTracker => {
                let target = if self.read_bool()? {
                    let dimension = self.read_string()?;
                    let pos = self.read_position()?;
                    Some((dimension, [pos.x, pos.y, pos.z]))
                } else {
                    None
                };
                let tracked = self.read_bool()?;
                NetworkComponent::LodestoneTracker(tuff_item::item::LodestoneTracker {
                    target,
                    tracked,
                })
            }
            DataComponentType::CustomName => match text(self)? {
                (Some(name), _) => {
                    components.custom_name = Some(Box::new(name));
                    return Ok(true);
                }
                (None, tag) => {
                    components.other.insert(kind.name(), tag);
                    return Ok(true);
                }
            },
            DataComponentType::Enchantments => {
                components.enchantments = Some(enchantments(self)?);
                return Ok(true);
            }
            DataComponentType::StoredEnchantments => {
                components.stored_enchantments = Some(enchantments(self)?);
                return Ok(true);
            }
            DataComponentType::PotionContents => {
                let potion = if self.read_bool()? {
                    let id = self.read_var_int()?;
                    Some(
                        u32::try_from(id)
                            .ok()
                            .and_then(Potion::from_id)
                            .ok_or(BufferError::InvalidEnum(id))?,
                    )
                } else {
                    None
                };
                let custom_color = if self.read_bool()? {
                    Some(self.read_int()?)
                } else {
                    None
                };
                let len = self.read_var_int()?;
                if !(0..=MAX_POTION_EFFECTS).contains(&len) {
                    return Err(BufferError::InvalidEnum(len));
                }
                let mut custom_effects = Vec::with_capacity(len as usize);
                for _ in 0..len {
                    let id = self.read_var_int()?;
                    let effect = u32::try_from(id)
                        .ok()
                        .and_then(MobEffect::from_id)
                        .ok_or(BufferError::InvalidEnum(id))?;
                    let details = self.read_effect_details(0)?;
                    custom_effects.push(PotionEffect { effect, details });
                }
                let custom_name = if self.read_bool()? {
                    Some(self.read_string()?)
                } else {
                    None
                };
                components.potion_contents = Some(Box::new(PotionContents {
                    potion,
                    custom_color,
                    custom_effects,
                    custom_name,
                }));
                return Ok(true);
            }
            DataComponentType::Container => {
                let Some(contents) = self.read_container(depth)? else {
                    return Ok(false);
                };
                components.set_network_container(contents);
                return Ok(true);
            }
            DataComponentType::SulfurCubeContent => {
                let Some(stack) = self.read_item_template(depth)? else {
                    return Ok(false);
                };
                return Ok(
                    components.set_network(NetworkComponent::Template(kind, Box::new(stack)))
                );
            }
            DataComponentType::ChargedProjectiles => {
                let Some(projectiles) = self.read_charged_projectiles(depth)? else {
                    return Ok(false);
                };
                components.set_network_projectiles(projectiles);
                return Ok(true);
            }
            DataComponentType::BundleContents => {
                let Some(contents) = self.read_bundle(depth)? else {
                    return Ok(false);
                };
                components.set_network_bundle(contents);
                return Ok(true);
            }
            DataComponentType::Lore => {
                let len = self.read_var_int()?;
                if !(0..=MAX_LORE_LINES).contains(&len) {
                    return Err(BufferError::InvalidEnum(len));
                }
                let mut lines = Vec::with_capacity(len as usize);
                for _ in 0..len {
                    lines.push(self.read_nbt()?.ok_or(BufferError::InvalidEnum(0))?);
                }
                return Ok(components.set_network(NetworkComponent::Lore(&lines)));
            }
            DataComponentType::DyedColor => NetworkComponent::DyedColor(self.read_int()?),
            DataComponentType::BaseColor => {
                let id = self.read_var_int()?;
                let color = u32::try_from(id)
                    .ok()
                    .and_then(DyeColor::from_id)
                    .ok_or(BufferError::InvalidEnum(id))?;
                NetworkComponent::BaseColor(color)
            }
            DataComponentType::BannerPatterns => {
                let len = self.read_var_int()?;
                if !(0..=MAX_BANNER_LAYERS as i32).contains(&len) {
                    return Err(BufferError::InvalidEnum(len));
                }
                let mut layers = Vec::with_capacity(len as usize);
                for _ in 0..len {
                    let pattern = self.read_var_int()?;
                    let (pattern, inline) = if pattern == 0 {
                        let asset = self.read_string()?;
                        let key = self.read_string()?;
                        (asset, Some(key))
                    } else {
                        let Some(pattern) = usize::try_from(pattern)
                            .ok()
                            .and_then(|id| id.checked_sub(1))
                            .and_then(|index| BANNER_PATTERN.entries.get(index))
                        else {
                            return Ok(false);
                        };
                        ((*pattern).to_owned(), None)
                    };
                    let color = self.read_var_int()?;
                    let color = u32::try_from(color)
                        .ok()
                        .and_then(DyeColor::from_id)
                        .ok_or(BufferError::InvalidEnum(color))?;
                    layers.push(BannerLayer {
                        pattern,
                        inline,
                        color,
                    });
                }
                return Ok(components.set_network(NetworkComponent::BannerPatterns(&layers)));
            }
            DataComponentType::Trim => {
                let mut trim = ArmorTrim::named("", "");
                let id = self.read_var_int()?;
                if id == 0 {
                    let palette_id = self.read_string()?;
                    let Some(description) = read_text_nbt(self)? else {
                        return Ok(false);
                    };
                    trim.inline_material = Some(InlineTrimMaterial {
                        palette_id,
                        description,
                    });
                } else {
                    let Some(name) = usize::try_from(id)
                        .ok()
                        .and_then(|id| id.checked_sub(1))
                        .and_then(|index| TRIM_MATERIAL.entries.get(index))
                    else {
                        return Ok(false);
                    };
                    trim.material = (*name).to_owned();
                }
                let id = self.read_var_int()?;
                if id == 0 {
                    let asset_id = self.read_string()?;
                    let Some(description) = read_text_nbt(self)? else {
                        return Ok(false);
                    };
                    let decal = self.read_bool()?;
                    trim.inline_pattern = Some(InlineTrimPattern {
                        asset_id,
                        description,
                        decal,
                    });
                } else {
                    let Some(name) = usize::try_from(id)
                        .ok()
                        .and_then(|id| id.checked_sub(1))
                        .and_then(|index| TRIM_PATTERN.entries.get(index))
                    else {
                        return Ok(false);
                    };
                    trim.pattern = (*name).to_owned();
                }
                components.trim = Some(Box::new(trim));
                return Ok(true);
            }
            DataComponentType::Fireworks => {
                let flight_duration = self.read_var_int()?;
                let flight_duration = u8::try_from(flight_duration)
                    .map_err(|_| BufferError::InvalidEnum(flight_duration))?;
                let len = self.read_var_int()?;
                if !(0..=MAX_EXPLOSIONS as i32).contains(&len) {
                    return Err(BufferError::InvalidEnum(len));
                }
                let mut explosions = Vec::with_capacity(len as usize);
                for _ in 0..len {
                    explosions.push(self.read_explosion()?);
                }
                components.fireworks = Some(Box::new(Fireworks {
                    flight_duration,
                    explosions,
                }));
                return Ok(true);
            }
            DataComponentType::FireworkExplosion => {
                components.firework_explosion = Some(Box::new(self.read_explosion()?));
                return Ok(true);
            }
            DataComponentType::WritableBookContent => {
                let len = self.read_var_int()?;
                if !(0..=MAX_BOOK_PAGES as i32).contains(&len) {
                    return Err(BufferError::InvalidEnum(len));
                }
                let mut pages = Vec::with_capacity(len as usize);
                for _ in 0..len {
                    pages.push(self.read_filterable_string(MAX_PAGE_CHARS)?);
                }
                components.writable_book = Some(pages);
                return Ok(true);
            }
            DataComponentType::WrittenBookContent => {
                let title = self.read_filterable_string(MAX_TITLE_CHARS)?;
                let author = self.read_string()?;
                let generation = self.read_var_int()?;
                let len = self.read_var_int()?;
                if !(0..=MAX_BOOK_PAGES as i32).contains(&len) {
                    return Err(BufferError::InvalidEnum(len));
                }
                let mut pages = Vec::with_capacity(len as usize);
                for _ in 0..len {
                    let raw = self.read_nbt()?.ok_or(BufferError::InvalidEnum(0))?;
                    let filtered = if self.read_bool()? {
                        Some(self.read_nbt()?.ok_or(BufferError::InvalidEnum(0))?)
                    } else {
                        None
                    };
                    pages.push(Filterable { raw, filtered });
                }
                let resolved = self.read_bool()?;
                components.written_book = Some(Box::new(WrittenBook {
                    title,
                    author,
                    generation,
                    pages,
                    resolved,
                }));
                return Ok(true);
            }
            DataComponentType::AttributeModifiers => {
                let len = self.read_var_int()?;
                if !(0..=MAX_MODIFIERS as i32).contains(&len) {
                    return Err(BufferError::InvalidEnum(len));
                }
                let mut modifiers = Vec::with_capacity(len as usize);
                for _ in 0..len {
                    let attribute = self.read_var_int()?;
                    let attribute = u32::try_from(attribute)
                        .ok()
                        .and_then(Attribute::from_id)
                        .ok_or(BufferError::InvalidEnum(attribute))?;
                    let id = self.read_string_with_max(MAX_IDENTIFIER_BYTES)?;
                    let amount = self.read_double()?;
                    let operation = self.read_var_int()?;
                    let operation = u32::try_from(operation)
                        .ok()
                        .and_then(ModifierOperation::from_network_id)
                        .ok_or(BufferError::InvalidEnum(operation))?;
                    let slot = self.read_var_int()?;
                    let slot = u32::try_from(slot)
                        .ok()
                        .and_then(EquipmentSlotGroup::from_network_id)
                        .ok_or(BufferError::InvalidEnum(slot))?;
                    let display = self.read_var_int()?;
                    let display = match u32::try_from(display)
                        .ok()
                        .and_then(ModifierDisplay::from_network_id)
                    {
                        Some(Some(display)) => display,
                        Some(None) => ModifierDisplay::Override(
                            self.read_nbt()?.ok_or(BufferError::InvalidEnum(0))?,
                        ),
                        None => return Err(BufferError::InvalidEnum(display)),
                    };
                    modifiers.push(AttributeModifier {
                        attribute,
                        id,
                        amount,
                        operation,
                        slot,
                        display,
                    });
                }
                components.attribute_modifiers = Some(modifiers);
                return Ok(true);
            }
            DataComponentType::Tool => {
                let len = self.read_var_int()?;
                if !(0..=MAX_TOOL_RULES as i32).contains(&len) {
                    return Err(BufferError::InvalidEnum(len));
                }
                let mut rules = Vec::with_capacity(len as usize);
                let mut known = true;
                for _ in 0..len {
                    let size = self.read_var_int()?;
                    let blocks = if size == 0 {
                        let name = self.read_string_with_max(MAX_IDENTIFIER_BYTES)?;
                        BlockSet::tag(&name)
                    } else {
                        let count = size - 1;
                        if !(0..=Block::ALL.len() as i32).contains(&count) {
                            return Err(BufferError::InvalidEnum(size));
                        }
                        let mut blocks = Vec::with_capacity(count as usize);
                        for _ in 0..count {
                            let id = self.read_var_int()?;
                            blocks.push(
                                u32::try_from(id)
                                    .ok()
                                    .and_then(Block::from_id)
                                    .ok_or(BufferError::InvalidEnum(id))?,
                            );
                        }
                        Some(BlockSet::Blocks(blocks))
                    };
                    let speed = if self.read_bool()? {
                        Some(self.read_float()?)
                    } else {
                        None
                    };
                    let correct_for_drops = if self.read_bool()? {
                        Some(self.read_bool()?)
                    } else {
                        None
                    };
                    match blocks {
                        Some(blocks) => rules.push(ToolRule {
                            blocks,
                            speed,
                            correct_for_drops,
                        }),
                        None => known = false,
                    }
                }
                let default_mining_speed = self.read_float()?;
                let damage_per_block = non_negative(self)?;
                let can_destroy_blocks_in_creative = self.read_bool()?;
                if !known {
                    return Ok(false);
                }
                components.tool = Some(Box::new(Tool {
                    rules,
                    default_mining_speed,
                    damage_per_block,
                    can_destroy_blocks_in_creative,
                }));
                return Ok(true);
            }
            DataComponentType::Weapon => {
                let item_damage_per_attack = non_negative(self)?.into();
                let disable_blocking_for_seconds = self.read_float()?;
                components.weapon = Some(Weapon {
                    item_damage_per_attack,
                    disable_blocking_for_seconds,
                });
                return Ok(true);
            }
            DataComponentType::SuspiciousStewEffects => {
                let len = self.read_var_int()?;
                if !(0..=MAX_SUSPICIOUS_STEW_EFFECTS).contains(&len) {
                    return Err(BufferError::InvalidEnum(len));
                }
                let mut effects = Vec::with_capacity(len as usize);
                for _ in 0..len {
                    let id = self.read_var_int()?;
                    let effect = u32::try_from(id)
                        .ok()
                        .and_then(MobEffect::from_id)
                        .ok_or(BufferError::InvalidEnum(id))?;
                    let duration = self.read_var_int()?;
                    effects.push(SuspiciousEffect { effect, duration });
                }
                components.suspicious_stew_effects = Some(effects);
                return Ok(true);
            }
            DataComponentType::BlockState => {
                let len = self.read_var_int()?;
                if !(0..=MAX_BLOCK_STATE_PROPERTIES).contains(&len) {
                    return Err(BufferError::InvalidEnum(len));
                }
                let mut properties = Compound::with_capacity(len as usize);
                for _ in 0..len {
                    let key = self.read_string()?;
                    let value = self.read_string()?;
                    properties.insert(key, value);
                }
                return Ok(components.set_network(NetworkComponent::BlockState(&properties)));
            }
            DataComponentType::PotDecorations => {
                let mut sides: tuff_item::item::PotSides = Default::default();
                let mut supported = true;
                for side in &mut sides {
                    if self.read_bool()? {
                        *side = self.read_item_template(depth)?;
                        supported &= side.is_some();
                    }
                }
                return Ok(supported
                    && components.set_network(NetworkComponent::PotDecorations(Box::new(sides))));
            }
            DataComponentType::CustomData
            | DataComponentType::ItemName
            | DataComponentType::BucketEntityData => {
                let tag = self.read_nbt()?.ok_or(BufferError::InvalidEnum(0))?;
                return Ok(components.set_network(NetworkComponent::Nbt(kind, &tag)));
            }
            DataComponentType::EntityData | DataComponentType::BlockEntityData => {
                let id = self.read_var_int()?;
                let name = u32::try_from(id)
                    .ok()
                    .and_then(|id| typed_entity_name(kind, id))
                    .ok_or(BufferError::InvalidEnum(id))?;
                let data = match self.read_nbt()? {
                    Some(Tag::Compound(data)) => data,
                    _ => return Err(BufferError::InvalidEnum(0)),
                };
                let mut tag = Compound::with_capacity(data.len() + 1).with("id", name);
                for (key, value) in data.iter().filter(|(key, _)| *key != "id") {
                    tag.insert(key, value.clone());
                }
                return Ok(
                    components.set_network(NetworkComponent::TypedNbt(kind, id as u32, &tag))
                );
            }
            _ => return Ok(false),
        };
        Ok(components.set_network(component))
    }

    fn read_explosion(&mut self) -> Result<FireworkExplosion> {
        let shape = self.read_var_int()?;
        let shape = u32::try_from(shape)
            .ok()
            .and_then(FireworkShape::from_id)
            .ok_or(BufferError::InvalidEnum(shape))?;
        let mut lists = [Vec::new(), Vec::new()];
        for colors in &mut lists {
            let len = self.read_var_int()?;
            if !(0..=MAX_EXPLOSIONS as i32).contains(&len) {
                return Err(BufferError::InvalidEnum(len));
            }
            for _ in 0..len {
                colors.push(self.read_int()?);
            }
        }
        let [colors, fade_colors] = lists;
        Ok(FireworkExplosion {
            shape,
            colors,
            fade_colors,
            has_trail: self.read_bool()?,
            has_twinkle: self.read_bool()?,
        })
    }

    fn read_filterable_string(&mut self, max_chars: usize) -> Result<Filterable<String>> {
        let raw = self.read_string_chars(max_chars)?;
        let filtered = if self.read_bool()? {
            Some(self.read_string_chars(max_chars)?)
        } else {
            None
        };
        Ok(Filterable { raw, filtered })
    }

    fn read_effect_details(&mut self, depth: u32) -> Result<EffectDetails> {
        let amplifier = self.read_var_int()?;
        let amplifier = u8::try_from(amplifier).map_err(|_| BufferError::InvalidEnum(amplifier))?;
        let duration = self.read_var_int()?;
        let ambient = self.read_bool()?;
        let show_particles = self.read_bool()?;
        let show_icon = self.read_bool()?;
        let hidden_effect = if self.read_bool()? {
            if depth >= MAX_HIDDEN_EFFECT_DEPTH {
                return Err(BufferError::InvalidEnum(depth as i32));
            }
            Some(Box::new(self.read_effect_details(depth + 1)?))
        } else {
            None
        };
        Ok(EffectDetails {
            amplifier,
            duration,
            ambient,
            show_particles,
            show_icon,
            hidden_effect,
        })
    }
}

fn bounded_len(len: i32) -> Result<usize> {
    if !(0..=MAX_HASHED_COMPONENTS as i32).contains(&len) {
        return Err(BufferError::InvalidEnum(len));
    }
    Ok(len as usize)
}
