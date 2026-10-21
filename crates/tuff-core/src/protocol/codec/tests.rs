use super::*;
use tuff_item::item::BurnReason;
use tuff_item::item::MintReason;
use tuff_nbt::Compound;
use tuff_wire::Buffer;
use tuff_world_model::BlockPos;

fn reader(w: PacketWriter) -> PacketReader {
    let mut buf = Buffer::new();
    buf.extend(&w.finish()).unwrap();
    PacketReader::new(buf.try_split_packet().unwrap().unwrap())
}

#[tuff_core::watchdog]
#[test]
fn position_matches_wiki_example() {
    let pos = BlockPos::new(18357644, 831, -20882616);
    assert_eq!(
        pos.pack() as u64,
        0b0100011000000111011000110010110000010101101101001000001100111111
    );
    assert_eq!(BlockPos::unpack(pos.pack()), pos);
    let negative = BlockPos::new(-128, -64, -1);
    assert_eq!(BlockPos::unpack(negative.pack()), negative);
}

#[tuff_core::watchdog]
#[test]
fn lp_vec3_received_matches_the_wire() {
    for v in [
        [0.0, 0.0, 0.0],
        [1.0e-5, 0.0, -1.0e-5],
        [0.4, 0.4, -0.2],
        [-0.123456, 0.36, 0.987654],
        [-2.9, 1.0, 0.0],
        [12.5, -7.25, 3.0],
        [f64::NAN, 2.0, -1.5],
        [1.0e12, -1.0e12, 5.0],
        [1.7179869183e10, 0.0, 0.0],
    ] {
        let mut w = PacketWriter::new();
        w.write_lp_vec3(v);
        let decoded = reader(w).read_lp_vec3().unwrap();
        assert_eq!(lp_vec3_received(v), decoded, "{v:?}");
    }
}

#[tuff_core::watchdog]
#[test]
fn lp_vec3_roundtrips_within_quantization() {
    for v in [
        [0.0, 0.0, 0.0],
        [0.4, 0.4, -0.2],
        [-2.9, 1.0, 0.0],
        [12.5, -7.25, 3.0],
    ] {
        let mut w = PacketWriter::new();
        w.write_lp_vec3(v);
        let mut r = reader(w);
        let decoded = r.read_lp_vec3().unwrap();
        assert_eq!(r.remaining(), 0);
        let scale = v.iter().fold(1.0f64, |m, c| m.max(c.abs().ceil()));
        for (a, b) in v.iter().zip(decoded) {
            assert!((a - b).abs() <= scale / 16000.0, "{v:?} -> {decoded:?}");
        }
    }
}

#[tuff_core::watchdog]
#[test]
fn hashed_slot_rejects_absurd_component_counts() {
    let mut w = PacketWriter::new();
    w.write_bool(true);
    w.write_var_int(1);
    w.write_var_int(1);
    w.write_var_int(i32::MAX);
    assert!(reader(w).read_hashed_slot().is_err());
}

#[tuff_core::watchdog]
#[test]
fn creative_slot_accepts_plain_stacks_only() {
    let slot = |count: i32, item: i32, added: i32, removed: i32| {
        let mut w = PacketWriter::new();
        w.write_var_int(count);
        if count > 0 {
            w.write_var_int(item);
            w.write_var_int(added);
            w.write_var_int(removed);
        }
        let mut r = reader(w);
        let decoded = r.read_creative_slot().unwrap();
        (decoded, r.remaining())
    };
    let stone = Item::Stone.id() as i32;
    let sword = Item::DiamondSword.id() as i32;
    assert_eq!(slot(0, 0, 0, 0), (CreativeSlot::Empty, 0));
    assert_eq!(slot(-5, 0, 0, 0), (CreativeSlot::Empty, 0));
    assert_eq!(
        slot(64, stone, 0, 0),
        (
            CreativeSlot::Stack(ItemStack::mint(Item::Stone, 64, MintReason::Test)),
            0
        )
    );
    assert_eq!(slot(65, stone, 0, 0).0, CreativeSlot::Invalid);
    assert_eq!(slot(2, sword, 0, 0).0, CreativeSlot::Invalid);
    assert_eq!(slot(1, i32::MAX, 0, 0).0, CreativeSlot::Invalid);
    assert_eq!(slot(1, -1, 0, 0).0, CreativeSlot::Invalid);
    assert_eq!(slot(1, Item::Air.id() as i32, 0, 0).0, CreativeSlot::Empty);
}

fn damaged_named_pickaxe() -> ItemStack {
    let mut stack = ItemStack::mint(Item::DiamondPickaxe, 1, MintReason::Test);
    stack.set_damage(3);
    stack.components.custom_name = Some(Box::new(TextComponent::new("Sharp")));
    stack.components.enchantments = Some(Enchantments::new([
        ("minecraft:unbreaking".to_owned(), 3),
        ("minecraft:efficiency".to_owned(), 5),
    ]));
    stack.components.repair_cost = Some(2);
    stack
        .components
        .other
        .insert("minecraft:custom_data", Compound::new().with("k", 1));
    stack.components.other.insert(
        "minecraft:lore",
        Tag::List(vec![Tag::String("line".into())]),
    );
    stack.components.removed = vec!["minecraft:tool".into()];
    stack
}

fn unhex(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn bytes_reader(bytes: &[u8]) -> PacketReader {
    PacketReader::new(bytes::Bytes::copy_from_slice(bytes))
}

/// Writes a stack as a creative client does: a Slot whose added
/// component payloads are each prefixed with their byte length.
fn write_creative_slot(stack: &ItemStack) -> PacketWriter {
    let mut w = PacketWriter::new();
    w.write_var_int(stack.count() as i32);
    w.write_var_int(stack.item.id() as i32);
    let added = stack.components.network_added();
    let removed = stack.components.network_removed();
    w.write_var_int(added.len() as i32);
    w.write_var_int(removed.len() as i32);
    for component in &added {
        w.write_var_int(component.kind().id() as i32);
        let mut payload = PacketWriter::new();
        payload.write_component(component);
        w.write_prefixed_bytes(&payload.into_bytes());
    }
    for kind in removed {
        w.write_var_int(kind.id() as i32);
    }
    w
}

/// Stacks with the bytes the game's own codecs produce for them
/// (26.3): the clientbound Slot and the Set Creative Mode Slot payload
/// after the slot number. `exact` is false where the game's component
/// order is hash order, so only decoding is compared.
struct GameEncoded {
    stack: ItemStack,
    slot: &'static str,
    creative: &'static str,
    exact: bool,
}

fn game_encoded_stacks() -> Vec<GameEncoded> {
    let case = |stack: ItemStack, slot, creative, exact| GameEncoded {
        stack,
        slot,
        creative,
        exact,
    };
    let pickaxe = |f: fn(&mut ItemStack)| {
        let mut stack = ItemStack::mint(Item::DiamondPickaxe, 1, MintReason::Test);
        f(&mut stack);
        stack
    };
    let mut cases = vec![
        case(
            ItemStack::mint(Item::Stone, 64, MintReason::Test),
            "40010000",
            "40010000",
            true,
        ),
        case(
            pickaxe(|s| s.components.damage = Some(1)),
            "019c0801000301",
            "019c080100030101",
            true,
        ),
        case(
            pickaxe(|s| s.components.damage = Some(300)),
            "019c08010003ac02",
            "019c0801000302ac02",
            true,
        ),
        case(
            pickaxe(|s| s.components.unbreakable = true),
            "019c08010004",
            "019c0801000400",
            true,
        ),
        case(
            pickaxe(|s| {
                s.components.max_damage = Some(9);
                s.components.repair_cost = Some(2);
            }),
            "019c08020002091302",
            "019c080200020109130102",
            true,
        ),
    ];
    let mut book = ItemStack::mint(Item::EnchantedBook, 1, MintReason::Test);
    book.components.stored_enchantments =
        Some(Enchantments::new([("minecraft:mending".to_owned(), 1)]));
    cases.push(case(
        book,
        "01f30a01002d011701",
        "01f30a01002d03011701",
        true,
    ));
    let mut sword = ItemStack::mint(Item::DiamondSword, 1, MintReason::Test);
    sword.components.enchantments = Some(Enchantments::new([
        ("minecraft:sharpness".to_owned(), 5),
        ("minecraft:unbreaking".to_owned(), 3),
    ]));
    cases.push(case(
        sword,
        "019a0801000d0228032105",
        "019a0801000d050228032105",
        false,
    ));
    let mut strength = ItemStack::mint(Item::Potion, 1, MintReason::Test);
    strength.components.potion_contents = Some(Box::new(PotionContents {
        potion: Some(Potion::Strength),
        ..Default::default()
    }));
    cases.push(case(
        strength,
        "01f7090100350122000000",
        "01f709010035050122000000",
        true,
    ));
    let mut custom = ItemStack::mint(Item::Potion, 1, MintReason::Test);
    custom.components.potion_contents = Some(Box::new(PotionContents {
        potion: Some(Potion::Swiftness),
        custom_color: Some(0x123456),
        custom_effects: vec![PotionEffect {
            effect: MobEffect::Speed,
            details: EffectDetails {
                amplifier: 1,
                duration: 200,
                ambient: false,
                show_particles: true,
                show_icon: true,
                hidden_effect: None,
            },
        }],
        custom_name: Some("nm".into()),
    }));
    cases.push(case(
        custom,
        "01f709010035010d0100123456010001c8010001010001026e6d",
        "01f70901003514010d0100123456010001c8010001010001026e6d",
        true,
    ));
    cases.push(case(
        pickaxe(|s| {
            s.components.custom_name = Some(Box::new(TextComponent::new("Sharp")));
            s.components.other.insert(
                "minecraft:lore",
                Tag::List(vec![
                    Tag::String("line".into()),
                    Tag::Compound(Compound::new().with("color", "blue").with("text", "two")),
                ]),
            );
        }),
        "019c0802000608000553686172700b020800046c696e650a080005636f6c6f720004626c756508000474657874000374776f00",
        "019c080200060808000553686172700b24020800046c696e650a080005636f6c6f720004626c756508000474657874000374776f00",
        true,
    ));
    let mut named = ItemStack::mint(Item::Stone, 1, MintReason::Test);
    named
        .components
        .other
        .insert("minecraft:item_name", Tag::String("Named".into()));
    cases.push(case(
        named,
        "01010100090800054e616d6564",
        "0101010009080800054e616d6564",
        true,
    ));
    let mut data = ItemStack::mint(Item::Stone, 1, MintReason::Test);
    data.components
        .other
        .insert("minecraft:custom_data", Compound::new().with("k", 1));
    cases.push(case(
        data,
        "01010100000a0300016b0000000100",
        "01010100000a0a0300016b0000000100",
        true,
    ));
    let mut no_attributes = ItemStack::mint(Item::DiamondSword, 1, MintReason::Test);
    no_attributes.components.removed = vec!["minecraft:attribute_modifiers".into()];
    cases.push(case(no_attributes, "019a08000110", "019a08000110", true));
    cases.push(case(
        damaged_named_pickaxe(),
        "019c08060103030608000553686172700d02280308051302000a0300016b00000001000b010800046c696e651c",
        "019c080601030103060808000553686172700d050228030805130102000a0a0300016b00000001000b08010800046c696e651c",
        false,
    ));
    let mut stand = ItemStack::mint(Item::ArmorStand, 1, MintReason::Test);
    stand.components.other.insert(
        "minecraft:entity_data",
        Compound::new()
            .with("id", "minecraft:armor_stand")
            .with("Small", 1i8),
    );
    cases.push(case(
        stand,
        "01fd0a01003c050a010005536d616c6c0100",
        "01fd0a01003c0c050a010005536d616c6c0100",
        true,
    ));
    let mut bucket = ItemStack::mint(Item::AxolotlBucket, 1, MintReason::Test);
    bucket.components.other.insert(
        "minecraft:bucket_entity_data",
        Compound::new().with("Variant", 2),
    );
    cases.push(case(
        bucket,
        "01f30801003d0a03000756617269616e740000000200",
        "01f30801003d100a03000756617269616e740000000200",
        true,
    ));
    let mut chest = ItemStack::mint(Item::Chest, 1, MintReason::Test);
    chest.components.other.insert(
        "minecraft:block_entity_data",
        Compound::new()
            .with("id", "minecraft:chest")
            .with("CustomName", "\"Box\""),
    );
    cases.push(case(
        chest,
        "01940301003e010a08000a437573746f6d4e616d65000522426f782200",
        "01940301003e17010a08000a437573746f6d4e616d65000522426f782200",
        true,
    ));
    cases
}

#[tuff_core::watchdog]
#[test]
fn slot_encoding_matches_the_game() {
    for case in game_encoded_stacks().iter().filter(|case| case.exact) {
        let mut w = PacketWriter::new();
        w.write_slot(Some(case.stack.view()));
        assert_eq!(hex(&w.into_bytes()), case.slot, "{:?}", case.stack);
    }
    let mut w = PacketWriter::new();
    w.write_slot(None);
    assert_eq!(hex(&w.into_bytes()), "00");
}

#[tuff_core::watchdog]
#[test]
fn creative_slot_decoding_matches_the_game() {
    for case in game_encoded_stacks() {
        let mut r = bytes_reader(&unhex(case.creative));
        assert_eq!(
            r.read_creative_slot().unwrap(),
            CreativeSlot::Stack(case.stack.test_copy()),
            "{}",
            case.creative
        );
        assert_eq!(r.remaining(), 0, "{}", case.creative);
        let mut r = bytes_reader(&unhex(case.slot));
        let unprefixed = r.read_creative_slot();
        assert!(
            unprefixed.is_err() || r.remaining() != 0 || case.slot == case.creative,
            "{}",
            case.slot
        );
    }
    assert_eq!(
        bytes_reader(&unhex("00")).read_creative_slot().unwrap(),
        CreativeSlot::Empty
    );
}

#[tuff_core::watchdog]
#[test]
fn creative_slot_decodes_real_client_payloads() {
    for (payload, damage) in [("01a2080100030102", 2), ("01a0080100030108", 8)] {
        let mut r = bytes_reader(&unhex(payload));
        let CreativeSlot::Stack(stack) = r.read_creative_slot().unwrap() else {
            panic!("{payload}");
        };
        assert_eq!(r.remaining(), 0);
        assert_eq!(stack.count(), 1);
        assert_eq!(stack.components.damage, Some(damage));
        let mut expected = ItemStack::mint(stack.item, 1, MintReason::Test);
        expected.components.damage = Some(damage);
        assert_eq!(stack, expected);
    }
}

#[tuff_core::watchdog]
#[test]
fn slot_components_round_trip_through_the_creative_decoder() {
    let stack = damaged_named_pickaxe();
    let mut r = reader(write_creative_slot(&stack));
    assert_eq!(
        r.read_creative_slot().unwrap(),
        CreativeSlot::Stack(stack.test_copy())
    );
    assert_eq!(r.remaining(), 0);

    let mut potion = ItemStack::mint(Item::Potion, 1, MintReason::Test);
    potion.components.potion_contents = Some(Box::new(PotionContents {
        potion: Some(Potion::Swiftness),
        custom_color: Some(0x123456),
        custom_effects: vec![PotionEffect {
            effect: MobEffect::Speed,
            details: EffectDetails {
                amplifier: 1,
                duration: 200,
                hidden_effect: Some(Box::new(EffectDetails {
                    duration: 400,
                    ..Default::default()
                })),
                ..Default::default()
            },
        }],
        custom_name: Some("nm".into()),
    }));
    potion.components.unbreakable = true;
    potion.components.max_damage = Some(9);
    potion.components.stored_enchantments =
        Some(Enchantments::new([("minecraft:mending".to_owned(), 1)]));
    let mut r = reader(write_creative_slot(&potion));
    assert_eq!(r.read_creative_slot().unwrap(), CreativeSlot::Stack(potion));
    assert_eq!(r.remaining(), 0);
}

#[tuff_core::watchdog]
#[test]
fn suspicious_stew_effects_round_trip_on_the_wire() {
    let mut stew = ItemStack::mint(Item::SuspiciousStew, 1, MintReason::Test);
    stew.components.suspicious_stew_effects = Some(vec![
        SuspiciousEffect {
            effect: MobEffect::NightVision,
            duration: 100,
        },
        SuspiciousEffect {
            effect: MobEffect::Hunger,
            duration: 160,
        },
    ]);
    let mut r = reader(write_creative_slot(&stew));
    assert_eq!(r.read_creative_slot().unwrap(), CreativeSlot::Stack(stew));
    assert_eq!(r.remaining(), 0);
}

#[tuff_core::watchdog]
#[test]
fn bundle_contents_round_trip_on_the_wire_nested() {
    let mut inner = ItemStack::mint(Item::Bundle, 1, MintReason::Test);
    inner.edit_bundle(|c| c.try_insert(&mut ItemStack::mint(Item::Stone, 8, MintReason::Test)));
    let mut outer = ItemStack::mint(Item::Bundle, 1, MintReason::Test);
    outer.edit_bundle(|c| c.try_insert(&mut ItemStack::mint(Item::Dirt, 20, MintReason::Test)));
    outer.edit_bundle(|c| c.try_insert(&mut inner));
    let mut r = reader(write_creative_slot(&outer));
    assert_eq!(
        r.read_creative_slot().unwrap(),
        CreativeSlot::Stack(outer.test_copy())
    );
    assert_eq!(
        ItemStack::mint_from_nbt(&outer.to_nbt(), MintReason::Test).unwrap(),
        outer
    );
}

#[tuff_core::watchdog]
#[test]
fn ominous_bottle_amplifiers_round_trip_on_the_wire() {
    let mut bottle = ItemStack::mint(Item::OminousBottle, 1, MintReason::Test);
    bottle.set_ominous_bottle_amplifier(0);
    assert_eq!(bottle.components.ominous_bottle_amplifier, None);
    assert_eq!(bottle.ominous_bottle_amplifier(), Some(0));
    bottle.set_ominous_bottle_amplifier(3);
    let mut r = reader(write_creative_slot(&bottle));
    assert_eq!(
        r.read_creative_slot().unwrap(),
        CreativeSlot::Stack(bottle.test_copy())
    );
    assert_eq!(r.remaining(), 0);
    let saved = bottle.components.to_nbt();
    assert_eq!(saved.get_int("minecraft:ominous_bottle_amplifier"), Some(3));
    assert_eq!(Components::from_nbt(&saved), bottle.components);
}

#[tuff_core::watchdog]
#[test]
fn block_state_components_round_trip_on_the_wire() {
    let mut light = ItemStack::mint(Item::Light, 1, MintReason::Test);
    light
        .components
        .other
        .insert("minecraft:block_state", Compound::new().with("level", "7"));
    let bytes = write_creative_slot(&light).finish();
    assert!(bytes.windows(8).any(|w| w == b"\x05level\x017"));
    let mut r = reader(write_creative_slot(&light));
    assert_eq!(r.read_creative_slot().unwrap(), CreativeSlot::Stack(light));
    assert_eq!(r.remaining(), 0);
}

#[tuff_core::watchdog]
#[test]
fn a_sulfur_cube_bucket_sends_and_takes_back_what_it_holds() {
    let mut bucket = ItemStack::mint(Item::SulfurCubeBucket, 1, MintReason::Test);
    let held = ItemStack::mint(Item::Tnt, 1, MintReason::Test).to_nbt();
    bucket
        .components
        .other
        .insert("minecraft:sulfur_cube_content", held);
    let mut r = reader(write_creative_slot(&bucket));
    assert_eq!(r.read_creative_slot().unwrap(), CreativeSlot::Stack(bucket));
    assert_eq!(r.remaining(), 0);
}

#[tuff_core::watchdog]
#[test]
fn display_components_round_trip_on_the_wire() {
    use tuff_item::item::display::{ModelData, TooltipDisplay};
    let mut stack = ItemStack::mint(Item::Paper, 3, MintReason::Test);
    let other = &mut stack.components.other;
    other.insert("minecraft:item_model", "ex:ruby");
    other.insert("minecraft:tooltip_style", "ex:fancy");
    other.insert("minecraft:rarity", "epic");
    other.insert("minecraft:enchantment_glint_override", true);
    other.insert("minecraft:max_stack_size", 16);
    let data = ModelData {
        floats: vec![0.25],
        flags: vec![true, false],
        strings: vec!["lit".into()],
        colors: vec![0x00FF00],
    };
    other.insert("minecraft:custom_model_data", data.to_nbt());
    let display = TooltipDisplay {
        hide_tooltip: true,
        hidden: vec![DataComponentType::Lore],
    };
    other.insert("minecraft:tooltip_display", display.to_nbt());
    assert_eq!(stack.components.network_added().len(), 7);
    let bytes = write_creative_slot(&stack).finish();
    assert!(bytes.windows(8).any(|w| w == b"\x07ex:ruby"));
    let mut r = reader(write_creative_slot(&stack));
    assert_eq!(
        r.read_creative_slot().unwrap(),
        CreativeSlot::Stack(stack.test_copy())
    );
    assert_eq!(r.remaining(), 0);

    let mut w = PacketWriter::new();
    w.write_item_template(stack.view());
    assert_eq!(
        reader(w).read_item_template(0).unwrap(),
        Some(stack.test_copy())
    );

    let mut short = ItemStack::mint(Item::Paper, 1, MintReason::Test);
    short
        .components
        .other
        .insert("minecraft:item_model", "ruby");
    let mut r = reader(write_creative_slot(&short));
    let CreativeSlot::Stack(read) = r.read_creative_slot().unwrap() else {
        panic!("expected a stack");
    };
    assert_eq!(
        read.components.other.get_str("minecraft:item_model"),
        Some("minecraft:ruby")
    );

    let mut over = stack.test_copy();
    over.resize(17, MintReason::Test, BurnReason::Test);
    let mut r = reader(write_creative_slot(&over));
    assert_eq!(r.read_creative_slot().unwrap(), CreativeSlot::Invalid);

    let mut broken = ItemStack::mint(Item::Paper, 1, MintReason::Test);
    broken.components.other.insert("minecraft:rarity", "mythic");
    broken
        .components
        .other
        .insert("minecraft:item_model", "Not An Id");
    assert!(broken.components.network_added().is_empty());
}

#[tuff_core::watchdog]
#[test]
fn display_component_reads_are_bounded() {
    let payload = |kind: DataComponentType, body: &dyn Fn(&mut PacketWriter)| {
        let mut w = PacketWriter::new();
        body(&mut w);
        let mut r = reader(w);
        r.read_component(kind, &mut Components::default())
    };
    assert!(payload(DataComponentType::MaxStackSize, &|w| w.write_var_int(100)).is_err());
    assert!(payload(DataComponentType::Rarity, &|w| w.write_var_int(4)).is_err());
    assert!(
        payload(DataComponentType::CustomModelData, &|w| w
            .write_var_int(i32::MAX))
        .is_err()
    );
    assert!(
        payload(DataComponentType::TooltipDisplay, &|w| {
            w.write_bool(false);
            w.write_var_int(1);
            w.write_var_int(i32::MAX);
        })
        .is_err()
    );
    assert!(payload(DataComponentType::ItemModel, &|w| w.write_string("Bad Id")).is_err());
}

#[tuff_core::watchdog]
#[test]
fn charged_crossbows_round_trip_their_projectiles_as_templates() {
    let mut crossbow = ItemStack::mint(Item::Crossbow, 1, MintReason::Test);
    crossbow.components.charged_projectiles = Some(vec![
        ItemStack::mint(Item::Arrow, 1, MintReason::Test),
        ItemStack::mint(Item::FireworkRocket, 1, MintReason::Test),
    ]);
    let mut r = reader(write_creative_slot(&crossbow));
    assert_eq!(
        r.read_creative_slot().unwrap(),
        CreativeSlot::Stack(crossbow.test_copy())
    );
    assert_eq!(r.remaining(), 0);

    let mut w = PacketWriter::new();
    w.write_slot(Some(crossbow.view()));
    let mut r = reader(w);
    assert_eq!(r.read_var_int().unwrap(), 1);
    assert_eq!(r.read_var_int().unwrap(), Item::Crossbow.id() as i32);
    assert_eq!(r.read_var_int().unwrap(), 1);
    assert_eq!(r.read_var_int().unwrap(), 0);
    assert_eq!(
        r.read_var_int().unwrap(),
        DataComponentType::ChargedProjectiles.id() as i32
    );
    assert_eq!(r.read_var_int().unwrap(), 2);
    assert_eq!(r.read_var_int().unwrap(), Item::Arrow.id() as i32);
    assert_eq!(r.read_var_int().unwrap(), 1, "a template's count");
    assert_eq!(r.read_var_int().unwrap(), 0);
    assert_eq!(r.read_var_int().unwrap(), 0);
    assert_eq!(r.read_var_int().unwrap(), Item::FireworkRocket.id() as i32);

    let saved = ItemStack::mint_from_nbt(&crossbow.to_nbt(), MintReason::Test).unwrap();
    assert_eq!(saved, crossbow);
}

#[tuff_core::watchdog]
#[test]
fn pot_decorations_are_sent_as_optional_templates_per_side() {
    let mut pot = ItemStack::mint(Item::DecoratedPot, 1, MintReason::Test);
    pot.components.other.insert(
        DataComponentType::PotDecorations.name(),
        tuff_item::item::pot_decorations_to_nbt(&[
            Some(ItemStack::mint(
                Item::AnglerPotterySherd,
                1,
                MintReason::Test,
            )),
            None,
            Some(ItemStack::mint(Item::Brick, 1, MintReason::Test)),
            None,
        ]),
    );
    let mut r = reader(write_creative_slot(&pot));
    assert_eq!(
        r.read_creative_slot().unwrap(),
        CreativeSlot::Stack(pot.test_copy())
    );
    assert_eq!(r.remaining(), 0);

    let mut w = PacketWriter::new();
    w.write_slot(Some(pot.view()));
    let mut r = reader(w);
    for _ in 0..4 {
        r.read_var_int().unwrap();
    }
    assert_eq!(
        r.read_var_int().unwrap(),
        DataComponentType::PotDecorations.id() as i32
    );
    assert!(r.read_bool().unwrap());
    assert_eq!(
        r.read_var_int().unwrap(),
        Item::AnglerPotterySherd.id() as i32
    );
    assert_eq!(r.read_var_int().unwrap(), 1);
    assert_eq!(r.read_var_int().unwrap(), 0);
    assert_eq!(r.read_var_int().unwrap(), 0);
    assert!(!r.read_bool().unwrap());
    assert!(r.read_bool().unwrap());
    assert_eq!(r.read_var_int().unwrap(), Item::Brick.id() as i32);
    for _ in 0..3 {
        r.read_var_int().unwrap();
    }
    assert!(!r.read_bool().unwrap());
    assert_eq!(r.remaining(), 0);

    let legacy = Tag::List(vec!["minecraft:angler_pottery_sherd".into()]);
    let sides = tuff_item::item::pot_decorations_from_nbt(&legacy).unwrap();
    assert_eq!(sides[0].as_ref().unwrap().item, Item::AnglerPotterySherd);
    assert_eq!(sides[3].as_ref().unwrap().item, Item::Brick);
}

#[tuff_core::watchdog]
#[test]
fn inline_banner_and_trim_definitions_are_sent_in_full() {
    let mut banner = ItemStack::mint(Item::WhiteBanner, 1, MintReason::Test);
    banner.components.banner_patterns = Some(vec![BannerLayer {
        pattern: "tuff:custom".into(),
        inline: Some("tuff.key".into()),
        color: DyeColor::Red,
    }]);
    let mut w = PacketWriter::new();
    w.write_slot(Some(banner.view()));
    let bytes = w.finish();
    assert!(
        bytes
            .windows(22)
            .any(|w| w == b"\x00\x0btuff:custom\x08tuff.key")
    );

    let mut armor = ItemStack::mint(Item::DiamondChestplate, 1, MintReason::Test);
    let mut trim = ArmorTrim::named("", "minecraft:coast");
    trim.inline_material = Some(tuff_item::item::decor::InlineTrimMaterial {
        palette_id: "tuff:glow".into(),
        description: TextComponent::new("Glow"),
    });
    armor.components.trim = Some(Box::new(trim));
    let mut w = PacketWriter::new();
    w.write_slot(Some(armor.view()));
    let bytes = w.finish();
    assert!(bytes.windows(11).any(|w| w == b"\x00\x09tuff:glow"));
}

#[tuff_core::watchdog]
#[test]
fn inline_banner_and_trim_definitions_decode_from_creative_slots() {
    let mut banner = ItemStack::mint(Item::WhiteBanner, 1, MintReason::Test);
    banner.components.banner_patterns = Some(vec![BannerLayer {
        pattern: "tuff:custom".into(),
        inline: Some("tuff.key".into()),
        color: DyeColor::Red,
    }]);
    let mut armor = ItemStack::mint(Item::DiamondChestplate, 1, MintReason::Test);
    let mut trim = ArmorTrim::named("", "");
    trim.inline_material = Some(InlineTrimMaterial {
        palette_id: "tuff:glow".into(),
        description: TextComponent::new("Glow"),
    });
    trim.inline_pattern = Some(InlineTrimPattern {
        asset_id: "tuff:swirl".into(),
        description: TextComponent::new("Swirl"),
        decal: true,
    });
    armor.components.trim = Some(Box::new(trim));
    for stack in [banner, armor] {
        let mut r = reader(write_creative_slot(&stack));
        assert_eq!(r.read_creative_slot().unwrap(), CreativeSlot::Stack(stack));
        assert_eq!(r.remaining(), 0);
    }
}

#[tuff_core::watchdog]
#[test]
fn container_components_round_trip_and_bound_their_nesting() {
    let mut shulker = ItemStack::mint(Item::ShulkerBox, 1, MintReason::Test);
    shulker.components.container = Some(vec![
        None,
        Some(damaged_named_pickaxe()),
        Some(ItemStack::mint(Item::Stone, 64, MintReason::Test)),
    ]);
    let mut r = reader(write_creative_slot(&shulker));
    assert_eq!(
        r.read_creative_slot().unwrap(),
        CreativeSlot::Stack(shulker.test_copy())
    );
    assert_eq!(r.remaining(), 0);

    let mut w = PacketWriter::new();
    w.write_slot(Some(shulker.view()));
    let mut r = reader(w);
    assert_eq!(r.read_var_int().unwrap(), 1);
    assert_eq!(r.read_var_int().unwrap(), Item::ShulkerBox.id() as i32);
    assert_eq!(r.read_var_int().unwrap(), 1);
    assert_eq!(r.read_var_int().unwrap(), 0);
    assert_eq!(
        r.read_var_int().unwrap(),
        DataComponentType::Container.id() as i32
    );
    assert_eq!(
        r.read_var_int().unwrap(),
        3,
        "three entries up to the last stack"
    );
    assert!(
        !r.read_bool().unwrap(),
        "an empty slot is an absent optional"
    );
    assert!(r.read_bool().unwrap());
    assert_eq!(r.read_var_int().unwrap(), Item::DiamondPickaxe.id() as i32);
    assert_eq!(r.read_var_int().unwrap(), 1);

    let boxed = |layers: u32| {
        let mut nested = ItemStack::mint(Item::ShulkerBox, 1, MintReason::Test);
        for _ in 0..layers {
            let mut outer = ItemStack::mint(Item::ShulkerBox, 1, MintReason::Test);
            outer.components.container = Some(vec![Some(nested)]);
            nested = outer;
        }
        nested
    };
    let deepest = boxed(MAX_CONTAINER_DEPTH + 1);
    assert_eq!(
        reader(write_creative_slot(&deepest))
            .read_creative_slot()
            .unwrap(),
        CreativeSlot::Stack(deepest)
    );
    assert!(
        reader(write_creative_slot(&boxed(MAX_CONTAINER_DEPTH + 2)))
            .read_creative_slot()
            .is_err()
    );

    let mut w = PacketWriter::new();
    w.write_var_int(1);
    w.write_var_int(Item::ShulkerBox.id() as i32);
    w.write_var_int(1);
    w.write_var_int(0);
    w.write_var_int(DataComponentType::Container.id() as i32);
    let mut payload = PacketWriter::new();
    payload.write_var_int(tuff_item::item::MAX_CONTAINER_SLOTS as i32 + 1);
    w.write_prefixed_bytes(&payload.into_bytes());
    assert!(reader(w).read_creative_slot().is_err());
}

#[tuff_core::watchdog]
#[test]
fn typed_entity_data_without_a_known_id_stays_off_the_wire() {
    let mut stack = ItemStack::mint(Item::ArmorStand, 1, MintReason::Test);
    stack.components.other.insert(
        "minecraft:entity_data",
        Compound::new()
            .with("id", "minecraft:not_a_mob")
            .with("Small", 1i8),
    );
    let mut w = PacketWriter::new();
    w.write_slot(Some(stack.view()));
    assert_eq!(hex(&w.into_bytes()), "01fd0a0000");
    let mut stack = ItemStack::mint(Item::Chest, 1, MintReason::Test);
    stack.components.other.insert(
        "minecraft:block_entity_data",
        Compound::new().with("Lock", "x"),
    );
    let mut w = PacketWriter::new();
    w.write_slot(Some(stack.view()));
    assert_eq!(hex(&w.into_bytes()), "0194030000");
    let unknown_type = "01fd0a01003c0cff7f0a010005536d616c6c0100";
    assert!(
        bytes_reader(&unhex(unknown_type))
            .read_creative_slot()
            .is_err()
    );
}

#[tuff_core::watchdog]
#[test]
fn slot_wire_layout_matches_the_wiki() {
    let stack = damaged_named_pickaxe();
    let mut w = PacketWriter::new();
    w.write_slot(Some(stack.view()));
    let mut r = reader(w);
    assert_eq!(r.read_var_int().unwrap(), 1);
    assert_eq!(r.read_var_int().unwrap(), Item::DiamondPickaxe.id() as i32);
    assert_eq!(r.read_var_int().unwrap(), 6);
    assert_eq!(r.read_var_int().unwrap(), 1);
    assert_eq!(
        r.read_var_int().unwrap(),
        DataComponentType::Damage.id() as i32
    );
    assert_eq!(r.read_var_int().unwrap(), 3);
    assert_eq!(
        r.read_var_int().unwrap(),
        DataComponentType::CustomName.id() as i32
    );
    assert_eq!(r.read_nbt().unwrap(), Some(Tag::String("Sharp".into())));
    assert_eq!(
        r.read_var_int().unwrap(),
        DataComponentType::Enchantments.id() as i32
    );
    assert_eq!(r.read_var_int().unwrap(), 2);
    let efficiency = tuff_vanilla_data::data_registry::ENCHANTMENT
        .entries
        .binary_search(&"minecraft:efficiency")
        .unwrap() as i32;
    assert_eq!(r.read_var_int().unwrap(), efficiency);
    assert_eq!(r.read_var_int().unwrap(), 5);
}

#[tuff_core::watchdog]
#[test]
fn unknown_component_types_are_skipped_but_unsupported() {
    let mut w = PacketWriter::new();
    w.write_var_int(1);
    w.write_var_int(Item::Stone.id() as i32);
    w.write_var_int(2);
    w.write_var_int(0);
    w.write_var_int(DataComponentType::DebugStickState.id() as i32);
    w.write_prefixed_bytes(&[7, 8, 9]);
    w.write_var_int(DataComponentType::Damage.id() as i32);
    w.write_prefixed_bytes(&[4]);
    let mut r = reader(w);
    assert_eq!(r.read_creative_slot().unwrap(), CreativeSlot::Unsupported);
    assert_eq!(r.remaining(), 0);

    let mut w = PacketWriter::new();
    w.write_var_int(1);
    w.write_var_int(Item::Stone.id() as i32);
    w.write_var_int(0);
    w.write_var_int(1);
    w.write_var_int(DataComponentType::Tool.id() as i32);
    let mut expected = ItemStack::mint(Item::Stone, 1, MintReason::Test);
    expected.components.removed = vec!["minecraft:tool".into()];
    assert_eq!(
        reader(w).read_creative_slot().unwrap(),
        CreativeSlot::Stack(expected)
    );

    let mut w = PacketWriter::new();
    w.write_var_int(1);
    w.write_var_int(Item::Stone.id() as i32);
    w.write_var_int(i32::MAX);
    w.write_var_int(0);
    assert!(reader(w).read_creative_slot().is_err());
}

#[tuff_core::watchdog]
#[test]
fn creative_component_payload_lengths_are_checked() {
    let header = |added: i32| {
        let mut w = PacketWriter::new();
        w.write_var_int(1);
        w.write_var_int(Item::DiamondPickaxe.id() as i32);
        w.write_var_int(added);
        w.write_var_int(0);
        w.write_var_int(DataComponentType::Damage.id() as i32);
        w
    };
    let mut longer_than_packet = header(1);
    longer_than_packet.write_var_int(2);
    longer_than_packet.write_var_int(1);
    assert!(reader(longer_than_packet).read_creative_slot().is_err());
    let mut negative = header(1);
    negative.write_var_int(-1);
    assert!(reader(negative).read_creative_slot().is_err());
    let mut trailing_in_payload = header(1);
    trailing_in_payload.write_prefixed_bytes(&[1, 1]);
    assert!(reader(trailing_in_payload).read_creative_slot().is_err());
    let mut truncated_payload = header(1);
    truncated_payload.write_prefixed_bytes(&[]);
    assert!(reader(truncated_payload).read_creative_slot().is_err());
}

#[tuff_core::watchdog]
#[test]
fn hashed_slots_match_component_checksums() {
    let stack = damaged_named_pickaxe();
    let view = |added: Vec<(DataComponentType, i32)>, removed: Vec<DataComponentType>| HashedSlot {
        item: Item::DiamondPickaxe.id(),
        count: 1,
        added: added.into_iter().map(|(k, h)| (k.id(), h)).collect(),
        removed: removed.into_iter().map(|k| k.id()).collect(),
    };
    let exact = view(
        vec![
            (DataComponentType::Damage, -499649379),
            (DataComponentType::CustomName, -1209906280),
            (DataComponentType::Enchantments, -1704577720),
            (
                DataComponentType::RepairCost,
                NetworkComponent::RepairCost(2).hash(),
            ),
            (
                DataComponentType::CustomData,
                NetworkComponent::Nbt(
                    DataComponentType::CustomData,
                    stack.components.other.get("minecraft:custom_data").unwrap(),
                )
                .hash(),
            ),
            (
                DataComponentType::Lore,
                NetworkComponent::Lore(stack.components.other.get_list("minecraft:lore").unwrap())
                    .hash(),
            ),
        ],
        vec![DataComponentType::Tool],
    );
    assert!(HashedSlot::matches(Some(&exact), Some(stack.view())));
    let mut wrong_hash = exact.clone();
    wrong_hash.added[0].1 += 1;
    assert!(!HashedSlot::matches(Some(&wrong_hash), Some(stack.view())));
    let mut missing = exact.clone();
    missing.added.pop();
    assert!(!HashedSlot::matches(Some(&missing), Some(stack.view())));
    let mut no_removed = exact.clone();
    no_removed.removed.clear();
    assert!(!HashedSlot::matches(Some(&no_removed), Some(stack.view())));
    let mut count = exact.clone();
    count.count = 2;
    assert!(!HashedSlot::matches(Some(&count), Some(stack.view())));
    assert!(HashedSlot::matches(None, None));
    assert!(!HashedSlot::matches(Some(&exact), None));
    let plain = ItemStack::mint(Item::Stone, 3, MintReason::Test);
    let plain_view = HashedSlot {
        item: Item::Stone.id(),
        count: 3,
        added: Vec::new(),
        removed: Vec::new(),
    };
    assert!(HashedSlot::matches(Some(&plain_view), Some(plain.view())));
    assert!(!HashedSlot::matches(Some(&plain_view), Some(stack.view())));
}

/// Wire bytes and checksums the game produced for the sample gear
/// component values.
#[tuff_core::watchdog]
#[test]
fn gear_components_match_the_game() {
    use tuff_item::item::gear::samples;
    let (tool, default_tool, modifiers) = (
        samples::tool(),
        samples::default_tool(),
        samples::modifiers(),
    );
    let cases = [
        (
            NetworkComponent::Weapon(samples::weapon()),
            "0340200000",
            211448302,
        ),
        (
            NetworkComponent::Weapon(samples::default_weapon()),
            "0100000000",
            -982207288,
        ),
        (
            NetworkComponent::Tool(&tool),
            "0303010901408000000101001a6d696e6563726166743a6d696e6561626c652f7069636b61786500\
             0100028b01014170000000400000000300",
            1969995031,
        ),
        (
            NetworkComponent::Tool(&default_tool),
            "01001a6d696e6563726166743a6d696e6561626c652f7069636b617865014100000001013f8000000101",
            -994415279,
        ),
        (
            NetworkComponent::AttributeModifiers(&modifiers),
            "0303156d696e6563726166743a746573745f64616d6167654014000000000000000100010c637573\
             746f6d3a61726d6f723fd00000000000000200011a106d696e6563726166743a7370656564\
             79bfb999999999999a01040208000446617374",
            1522822305,
        ),
    ];
    for (component, wire, hash) in cases {
        let wire: String = wire.split_whitespace().collect();
        let mut w = PacketWriter::new();
        w.write_component(&component);
        assert_eq!(hex(&w.into_bytes()), wire, "{component:?}");
        assert_eq!(component.hash(), hash, "{component:?}");
        let mut components = Components::default();
        let mut r = bytes_reader(&unhex(&wire));
        assert!(r.read_component(component.kind(), &mut components).unwrap());
        assert_eq!(r.remaining(), 0);
        assert_eq!(components.network_added(), vec![component]);
    }
    let unknown_tag = "01001a6d696e6563726166743a6e6f5f737563685f7461675f6865726500003f8000000101";
    let mut components = Components::default();
    let decoded = bytes_reader(&unhex(unknown_tag))
        .read_component(DataComponentType::Tool, &mut components)
        .unwrap();
    assert!(!decoded, "a tag this server does not know is unsupported");
}

#[tuff_core::watchdog]
#[test]
fn bitset_trims_trailing_zero_words() {
    let mut w = PacketWriter::new();
    w.write_bitset(&[5, 0, 0]);
    let mut r = reader(w);
    assert_eq!(r.read_var_int().unwrap(), 1);
    assert_eq!(r.read_long().unwrap(), 5);
}
