use tuff_core::protocol::codec::{CreativeSlot, HashedSlot};
use tuff_core::protocol::play::{GameMode, Serverbound};
use tuff_core::world::menu::MenuKind;
use tuff_core::world::session::PlayerSession;
use tuff_item::item::ItemStack;
use tuff_nbt::Tag;
use tuff_vanilla_data::Item;
use tuff_vanilla_data::registry::DataComponentType;

use super::{Check, Facts, Flag};

const DUPE: &str = "dupe";
const STACK_COUNT: &str = "stack_count";
const ITEM_DATA: &str = "item_data";
const INVENTORY: &str = "inventory";

const MAX_COUNT: i32 = 99;
/// Deepest NBT nesting a stack may carry, counted from each component.
pub const MAX_NBT_DEPTH: usize = 64;
/// Most NBT a stack and the stacks inside it may carry, estimated.
pub const MAX_NBT_BYTES: usize = 256 * 1024;
const TAG_OVERHEAD: usize = 4;

/// The server's side of a Click Container, taken before the session
/// applies it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClickBefore {
    /// Slots in the clicked window.
    pub window_len: usize,
    /// Item totals over the slots the click claims to change and the
    /// cursor.
    pub totals: Vec<(Item, u32)>,
    /// The claim can be held against the totals: the client saw the
    /// latest state, no earlier click waits in the queue, no result slot
    /// or bundle is involved and the player cannot create items.
    pub comparable: bool,
}

impl ClickBefore {
    pub fn probe(session: &PlayerSession, packet: &Serverbound) -> Option<Self> {
        let Serverbound::ContainerClick {
            window_id,
            state_id,
            slot,
            changed,
            ..
        } = packet
        else {
            return None;
        };
        let (kind, container, current_state, settled) = match &session.menus.menu {
            Some(menu) if menu.window_id == *window_id => (
                menu.kind,
                menu.container.as_slice(),
                menu.state_id,
                menu.queued.is_empty() && menu.closing.is_none(),
            ),
            None if *window_id == 0 => (
                MenuKind::Player,
                &[][..],
                session.inventory.state_id(),
                true,
            ),
            _ => return None,
        };
        let slots = session.inventory.slots();
        let stack_at = |index: usize| -> Option<&ItemStack> {
            if index < container.len() {
                return container[index].as_ref();
            }
            slots.get(kind.inventory_slot(index)?)?.as_ref()
        };
        let window_len = kind.len();
        let mut totals = Vec::new();
        let mut bundle = false;
        let mut seen: Vec<usize> = Vec::with_capacity(changed.len());
        let claimed = changed
            .iter()
            .filter_map(|(index, _)| usize::try_from(*index).ok())
            .filter(|index| *index < window_len);
        for index in claimed {
            if seen.contains(&index) {
                continue;
            }
            seen.push(index);
            if let Some(stack) = stack_at(index) {
                bundle |= stack.is_bundle();
                add(&mut totals, stack.item, u32::from(stack.count));
            }
        }
        if let Some(stack) = session.inventory.carried() {
            bundle |= stack.is_bundle();
            add(&mut totals, stack.item, u32::from(stack.count));
        }
        let result = kind.result_slot();
        let on_result = result
            .is_some_and(|result| usize::try_from(*slot) == Ok(result) || seen.contains(&result));
        let comparable = settled
            && *state_id == current_state
            && !on_result
            && !bundle
            && !session.mode.abilities.instabuild
            && session.mode.game_mode != GameMode::Spectator;
        Some(Self {
            window_len,
            totals,
            comparable,
        })
    }
}

fn add(totals: &mut Vec<(Item, u32)>, item: Item, count: u32) {
    match totals.iter_mut().find(|(known, _)| *known == item) {
        Some((_, total)) => *total += count,
        None => totals.push((item, count)),
    }
}

/// Why a stack could not have come from a vanilla client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tamper {
    /// No items, or more than the stack limit, here or inside it.
    Count,
    /// Both damageable and stackable.
    DamageableStack,
    /// A bundle whose weight cannot be counted.
    BundleWeight,
    NbtDepth,
    NbtSize,
}

/// Checks a client-made stack, its contents and their NBT.
pub fn tampered(stack: tuff_item::item::ItemView<'_>) -> Option<Tamper> {
    let mut bytes = 0;
    walk(stack, &mut bytes).err()
}

fn walk(stack: tuff_item::item::ItemView<'_>, bytes: &mut usize) -> Result<(), Tamper> {
    let max = stack.max_stack_size();
    if stack.count == 0 || stack.count > max {
        return Err(Tamper::Count);
    }
    if max > 1 && stack.max_damage().is_some() {
        return Err(Tamper::DamageableStack);
    }
    let components = &stack.components;
    for tag in components.other.values() {
        measure(tag, 1, bytes)?;
    }
    if let Some(bundle) = &components.bundle_contents {
        if bundle.weight().is_none() {
            return Err(Tamper::BundleWeight);
        }
        for inner in bundle.items() {
            walk(inner.view(), bytes)?;
        }
    }
    for inner in components.container.iter().flatten().flatten() {
        walk(inner.view(), bytes)?;
    }
    for inner in components.charged_projectiles.iter().flatten() {
        walk(inner.view(), bytes)?;
    }
    Ok(())
}

fn measure(tag: &Tag, depth: usize, bytes: &mut usize) -> Result<(), Tamper> {
    if depth > MAX_NBT_DEPTH {
        return Err(Tamper::NbtDepth);
    }
    *bytes += TAG_OVERHEAD
        + match tag {
            Tag::Byte(_) => 1,
            Tag::Short(_) => 2,
            Tag::Int(_) | Tag::Float(_) => 4,
            Tag::Long(_) | Tag::Double(_) => 8,
            Tag::ByteArray(values) => values.len(),
            Tag::String(text) => text.len(),
            Tag::IntArray(values) => values.len() * 4,
            Tag::LongArray(values) => values.len() * 8,
            Tag::List(_) | Tag::Compound(_) => 0,
        };
    if *bytes > MAX_NBT_BYTES {
        return Err(Tamper::NbtSize);
    }
    match tag {
        Tag::List(values) => {
            for value in values {
                measure(value, depth + 1, bytes)?;
            }
        }
        Tag::Compound(entries) => {
            for (key, value) in entries.iter() {
                *bytes += key.len();
                measure(value, depth + 1, bytes)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn hashed_limit(slot: &HashedSlot) -> Option<i32> {
    let item = Item::from_id(slot.item)?;
    let patched = DataComponentType::MaxStackSize.id();
    let custom =
        slot.added.iter().any(|(kind, _)| *kind == patched) || slot.removed.contains(&patched);
    Some(if custom {
        MAX_COUNT
    } else {
        i32::from(item.max_stack_size())
    })
}

fn impossible(slot: &HashedSlot) -> bool {
    hashed_limit(slot).is_none_or(|limit| !(1..=limit).contains(&slot.count))
}

/// Stack counts and item data a client cannot have made, and clicks that
/// claim more items than the touched slots and the cursor held.
pub struct ItemIntegrity;

impl ItemIntegrity {
    fn creative(stack: &CreativeSlot) -> Option<Flag> {
        match stack {
            CreativeSlot::Invalid => Flag::resync(STACK_COUNT, 5.0),
            CreativeSlot::Stack(stack) => match tampered(stack.view())? {
                Tamper::Count => Flag::resync(STACK_COUNT, 5.0),
                Tamper::NbtDepth | Tamper::NbtSize => Flag::resync(ITEM_DATA, 10.0),
                Tamper::DamageableStack | Tamper::BundleWeight => Flag::resync(ITEM_DATA, 5.0),
            },
            CreativeSlot::Empty | CreativeSlot::Unsupported => None,
        }
    }

    fn click(
        facts: &Facts,
        changed: &[(i16, Option<HashedSlot>)],
        carried: Option<&HashedSlot>,
    ) -> Option<Flag> {
        let claims = changed
            .iter()
            .filter_map(|(_, slot)| slot.as_ref())
            .chain(carried);
        if claims.clone().any(impossible) {
            return Flag::resync(STACK_COUNT, 5.0);
        }
        let before = facts.click.as_ref()?;
        let mut indices: Vec<i16> = changed.iter().map(|(index, _)| *index).collect();
        indices.sort_unstable();
        let repeated = indices.windows(2).any(|pair| pair[0] == pair[1]);
        let outside = indices.iter().any(|index| {
            usize::try_from(*index)
                .ok()
                .is_none_or(|i| i >= before.window_len)
        });
        if repeated || outside {
            return Flag::resync(INVENTORY, 2.0);
        }
        if !before.comparable {
            return None;
        }
        let mut after = Vec::new();
        for slot in claims {
            let item = Item::from_id(slot.item)?;
            if item.name().ends_with("bundle") {
                return None;
            }
            add(&mut after, item, slot.count as u32);
        }
        let created: u32 = after
            .iter()
            .map(|(item, count)| {
                let had = before
                    .totals
                    .iter()
                    .find(|(known, _)| known == item)
                    .map_or(0, |(_, total)| *total);
                count.saturating_sub(had)
            })
            .sum();
        if created > 0 {
            return Flag::resync(DUPE, 4.0);
        }
        None
    }
}

impl Check for ItemIntegrity {
    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        match packet {
            Serverbound::SetCreativeModeSlot { stack, .. } => Self::creative(stack),
            Serverbound::ContainerClick {
                changed, carried, ..
            } => Self::click(facts, changed, carried.as_ref()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::facts;
    use super::*;
    use tuff_nbt::Compound;

    fn creative(stack: CreativeSlot) -> Serverbound {
        Serverbound::SetCreativeModeSlot { slot: 36, stack }
    }

    fn flag_of(facts: &Facts, packet: &Serverbound) -> Option<&'static str> {
        ItemIntegrity.examine(facts, packet).map(|flag| flag.check)
    }

    fn with_data(item: Item, data: Tag) -> ItemStack {
        let mut stack = ItemStack::new(item, 1);
        stack.components.other.insert("minecraft:custom_data", data);
        stack
    }

    fn nested(depth: usize) -> Tag {
        (0..depth).fold(Tag::Compound(Compound::new()), |inner, _| {
            Tag::Compound(Compound::new().with("a", inner))
        })
    }

    #[tuff_core::watchdog]
    #[test]
    fn plain_and_reasonable_creative_stacks_pass() {
        let f = facts();
        let stone = ItemStack::new(Item::Stone, 64);
        assert_eq!(flag_of(&f, &creative(CreativeSlot::Stack(stone))), None);
        assert_eq!(flag_of(&f, &creative(CreativeSlot::Empty)), None);
        assert_eq!(flag_of(&f, &creative(CreativeSlot::Unsupported)), None);
        let data = with_data(Item::Stick, nested(MAX_NBT_DEPTH - 1));
        assert_eq!(tampered(data.view()), None);
        let mut shulker = ItemStack::new(Item::ShulkerBox, 1);
        shulker.components.container = Some(vec![Some(ItemStack::new(Item::Dirt, 64)), None]);
        assert_eq!(tampered(shulker.view()), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn overstacked_creative_stacks_are_flagged_at_any_depth() {
        let f = facts();
        assert_eq!(
            flag_of(&f, &creative(CreativeSlot::Invalid)),
            Some(STACK_COUNT)
        );
        let swords = ItemStack::new(Item::DiamondSword, 2);
        assert_eq!(tampered(swords.view()), Some(Tamper::Count));
        let mut shulker = ItemStack::new(Item::ShulkerBox, 1);
        shulker.components.container = Some(vec![None, Some(ItemStack::new(Item::Dirt, 65))]);
        assert_eq!(
            flag_of(&f, &creative(CreativeSlot::Stack(shulker))),
            Some(STACK_COUNT)
        );
        let mut crossbow = ItemStack::new(Item::Crossbow, 1);
        crossbow.components.charged_projectiles = Some(vec![ItemStack::new(Item::Arrow, 0)]);
        assert_eq!(tampered(crossbow.view()), Some(Tamper::Count));
    }

    #[tuff_core::watchdog]
    #[test]
    fn stackable_tools_are_tampered() {
        let mut sword = ItemStack::new(Item::DiamondSword, 1);
        sword
            .components
            .other
            .insert("minecraft:max_stack_size", Tag::Int(16));
        assert_eq!(tampered(sword.view()), Some(Tamper::DamageableStack));
        assert_eq!(
            flag_of(&facts(), &creative(CreativeSlot::Stack(sword))),
            Some(ITEM_DATA)
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn deep_or_huge_nbt_is_flagged() {
        let deep = with_data(Item::Stick, nested(MAX_NBT_DEPTH + 1));
        assert_eq!(tampered(deep.view()), Some(Tamper::NbtDepth));
        let wide = Tag::List(vec![Tag::String("x".repeat(1024)); 300]);
        assert_eq!(
            tampered(with_data(Item::Stick, wide).view()),
            Some(Tamper::NbtSize)
        );
        let small = Tag::List(vec![Tag::String("x".repeat(1024)); 100]);
        let mut shulker = ItemStack::new(Item::ShulkerBox, 1);
        shulker.components.container = Some(vec![Some(with_data(Item::Stick, small.clone())); 3]);
        assert_eq!(tampered(shulker.view()), Some(Tamper::NbtSize));
        assert_eq!(tampered(with_data(Item::Stick, small).view()), None);
        assert_eq!(
            flag_of(
                &facts(),
                &creative(CreativeSlot::Stack(with_data(Item::Stick, nested(500))))
            ),
            Some(ITEM_DATA)
        );
    }

    fn hashed(item: Item, count: i32) -> HashedSlot {
        HashedSlot {
            item: item.id(),
            count,
            added: Vec::new(),
            removed: Vec::new(),
        }
    }

    fn click(changed: Vec<(i16, Option<HashedSlot>)>, carried: Option<HashedSlot>) -> Serverbound {
        Serverbound::ContainerClick {
            window_id: 0,
            state_id: 0,
            slot: 36,
            button: 0,
            mode: 0,
            changed,
            carried,
        }
    }

    fn before(totals: Vec<(Item, u32)>, comparable: bool) -> Facts {
        let mut f = facts();
        f.click = Some(ClickBefore {
            window_len: 46,
            totals,
            comparable,
        });
        f
    }

    #[tuff_core::watchdog]
    #[test]
    fn impossible_claimed_stacks_are_flagged() {
        let f = before(Vec::new(), false);
        for bad in [
            hashed(Item::Stone, 65),
            hashed(Item::Stone, 0),
            hashed(Item::EnderPearl, 17),
            HashedSlot {
                item: u32::MAX,
                ..hashed(Item::Stone, 1)
            },
        ] {
            assert_eq!(
                flag_of(&f, &click(vec![(36, Some(bad.clone()))], None)),
                Some(STACK_COUNT)
            );
            assert_eq!(
                flag_of(&f, &click(Vec::new(), Some(bad))),
                Some(STACK_COUNT)
            );
        }
        let mut custom = hashed(Item::EnderPearl, 64);
        custom.added.push((DataComponentType::MaxStackSize.id(), 7));
        assert_eq!(flag_of(&f, &click(vec![(36, Some(custom))], None)), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn repeated_or_foreign_changed_slots_are_flagged() {
        let f = before(Vec::new(), false);
        let twice = click(vec![(36, None), (36, None)], None);
        assert_eq!(flag_of(&f, &twice), Some(INVENTORY));
        let outside = click(vec![(46, None)], None);
        assert_eq!(flag_of(&f, &outside), Some(INVENTORY));
        let negative = click(vec![(-1, None)], None);
        assert_eq!(flag_of(&f, &negative), Some(INVENTORY));
        assert_eq!(
            flag_of(&f, &click(vec![(36, None), (37, None)], None)),
            None
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn claiming_more_items_than_were_there_is_a_dupe() {
        let f = before(vec![(Item::Stone, 10)], true);
        let honest = click(vec![(36, None)], Some(hashed(Item::Stone, 10)));
        assert_eq!(flag_of(&f, &honest), None);
        let split = click(
            vec![(36, Some(hashed(Item::Stone, 5)))],
            Some(hashed(Item::Stone, 5)),
        );
        assert_eq!(flag_of(&f, &split), None);
        let doubled = click(
            vec![(36, Some(hashed(Item::Stone, 10)))],
            Some(hashed(Item::Stone, 10)),
        );
        assert_eq!(flag_of(&f, &doubled), Some(DUPE));
        let conjured = click(vec![(37, Some(hashed(Item::Diamond, 1)))], None);
        assert_eq!(flag_of(&f, &conjured), Some(DUPE));
        let stale = before(vec![(Item::Stone, 10)], false);
        assert_eq!(flag_of(&stale, &doubled), None);
        let bundle = click(vec![(37, Some(hashed(Item::Bundle, 1)))], None);
        assert_eq!(flag_of(&f, &bundle), None);
    }
}
