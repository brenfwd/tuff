fn parse_with(parser: &str, props: Option<&str>, text: &str) -> Result<Value, String> {
    parse_argument(
        parser,
        props,
        &mut Reader::new(text),
        ParseContext {
            allow_selectors: true,
        },
    )
    .map_err(|e| e.key().unwrap().to_owned())
}

#[tuff_core::watchdog]
#[test]
fn resource_selector_matches_wildcards_against_the_registry() {
    let props = Some(r#"{"registry":"minecraft:enchantment"}"#);
    let Ok(Value::Resources(all)) = parse_with("minecraft:resource_selector", props, "minecraft:*")
    else {
        panic!("no match");
    };
    assert!(all.len() > 10 && all.iter().all(|id| id.starts_with("minecraft:")));
    let Ok(Value::Resources(one)) = parse_with("minecraft:resource_selector", props, "sharp?ess")
    else {
        panic!("no match");
    };
    assert_eq!(one, ["minecraft:sharpness"]);
    assert_eq!(
        parse_with("minecraft:resource_selector", props, "zzz*").unwrap_err(),
        "argument.resource_selector.not_found"
    );
}

#[tuff_core::watchdog]
#[test]
fn loot_table_argument_takes_an_id_or_an_inline_table() {
    assert!(matches!(
        parse_with(
            "minecraft:loot_table",
            None,
            "minecraft:chests/simple_dungeon"
        ),
        Ok(Value::Identifier(_))
    ));
    assert!(matches!(
        parse_with("minecraft:loot_table", None, "{pools:[]}"),
        Ok(Value::Nbt(_))
    ));
    assert!(SUPPORTED_PARSERS.contains(&"minecraft:loot_table"));
}

#[tuff_core::watchdog]
#[test]
fn dialog_argument_takes_an_id_or_an_inline_definition() {
    assert!(matches!(
        parse_with("minecraft:dialog", None, "minecraft:server_links"),
        Ok(Value::Identifier(_))
    ));
    assert!(matches!(
        parse_with("minecraft:dialog", None, "{type:\"minecraft:notice\"}"),
        Ok(Value::Nbt(_))
    ));
    assert!(SUPPORTED_PARSERS.contains(&"minecraft:dialog"));
}

#[tuff_core::watchdog]
#[test]
fn slot_parsers_reject_ranges_only_for_single() {
    let parse = |p: &str, t: &str| {
        parse_argument(
            p,
            None,
            &mut Reader::new(t),
            ParseContext {
                allow_selectors: true,
            },
        )
        .map_err(|e| e.key().unwrap().to_owned())
    };
    assert!(parse("minecraft:slot", "container.5").is_ok());
    assert_eq!(
        parse("minecraft:slot", "container.*").unwrap_err(),
        "slot.only_single_allowed"
    );
    assert!(parse("minecraft:slots", "container.*").is_ok());
    assert_eq!(parse("minecraft:slot", "nope").unwrap_err(), "slot.unknown");
}

#[tuff_core::watchdog]
#[test]
fn style_argument_validates_fields() {
    let parse = |t: &str| {
        parse_argument(
            "minecraft:style",
            None,
            &mut Reader::new(t),
            ParseContext {
                allow_selectors: true,
            },
        )
        .map(|_| ())
        .map_err(|e| e.key().unwrap().to_owned())
    };
    assert!(parse("{color:\"red\",bold:true}").is_ok());
    assert!(parse("{color:\"#ff00aa\"}").is_ok());
    assert_eq!(
        parse("{color:\"nope\"}").unwrap_err(),
        "argument.style.invalid"
    );
    assert_eq!(
        parse("{bold:\"yes\"}").unwrap_err(),
        "argument.style.invalid"
    );
}
use super::*;

fn parse(parser: &str, props: Option<&str>, input: &str) -> Result<Value, String> {
    let mut reader = Reader::new(input);
    parse_argument(
        parser,
        props,
        &mut reader,
        ParseContext {
            allow_selectors: true,
        },
    )
    .map_err(|e| e.key().unwrap().to_owned())
}

#[tuff_core::watchdog]
#[test]
fn reads_primitive_arguments_with_bounds() {
    assert_eq!(
        parse("brigadier:integer", Some(r#"{"min":1}"#), "5"),
        Ok(Value::Int(5))
    );
    assert_eq!(
        parse("brigadier:integer", Some(r#"{"min":1}"#), "0"),
        Err("argument.integer.low".into())
    );
    assert_eq!(
        parse("brigadier:integer", Some(r#"{"max":3}"#), "4"),
        Err("argument.integer.big".into())
    );
    assert_eq!(
        parse("brigadier:integer", None, "x"),
        Err("parsing.int.expected".into())
    );
    assert_eq!(
        parse("brigadier:float", Some(r#"{"min":0.0,"max":2.0}"#), "2.5"),
        Err("argument.float.big".into())
    );
    assert_eq!(
        parse("brigadier:double", None, "1.5"),
        Ok(Value::Double(1.5))
    );
    assert_eq!(
        parse("brigadier:long", None, "9999999999"),
        Ok(Value::Long(9_999_999_999))
    );
    assert_eq!(parse("brigadier:bool", None, "true"), Ok(Value::Bool(true)));
    assert_eq!(
        parse("brigadier:string", Some(r#"{"type":"word"}"#), "ab cd"),
        Ok(Value::Str("ab".into()))
    );
    assert_eq!(
        parse("brigadier:string", Some(r#"{"type":"greedy"}"#), "ab cd"),
        Ok(Value::Str("ab cd".into()))
    );
    assert_eq!(
        parse("brigadier:string", Some(r#"{"type":"phrase"}"#), "\"a b\""),
        Ok(Value::Str("a b".into()))
    );
}

#[tuff_core::watchdog]
#[test]
fn reads_game_arguments() {
    assert_eq!(
        parse("minecraft:gamemode", None, "creative"),
        Ok(Value::GameMode(GameMode::Creative))
    );
    assert_eq!(
        parse("minecraft:gamemode", None, "1"),
        Err("argument.gamemode.invalid".into())
    );
    assert_eq!(
        parse("minecraft:time", Some(r#"{"min":0}"#), "1d"),
        Ok(Value::Time(24_000))
    );
    assert_eq!(
        parse("minecraft:time", Some(r#"{"min":0}"#), "0.5"),
        Ok(Value::Time(1))
    );
    assert_eq!(
        parse("minecraft:time", Some(r#"{"min":0}"#), "-1"),
        Err("argument.time.tick_count_too_low".into())
    );
    assert_eq!(
        parse("minecraft:time", None, "5x"),
        Err("argument.time.invalid_unit".into())
    );
    assert_eq!(
        parse(
            "minecraft:resource",
            Some(r#"{"registry":"minecraft:mob_effect"}"#),
            "speed"
        ),
        Ok(Value::Resource("minecraft:speed".into()))
    );
    assert_eq!(
        parse(
            "minecraft:resource",
            Some(r#"{"registry":"minecraft:mob_effect"}"#),
            "fast"
        ),
        Err("argument.resource.not_found".into())
    );
    assert_eq!(
        parse(
            "minecraft:resource",
            Some(r#"{"registry":"minecraft:enchantment"}"#),
            "minecraft:sharpness"
        ),
        Ok(Value::Resource("minecraft:sharpness".into()))
    );
    assert_eq!(
        parse(
            "minecraft:resource_or_tag",
            Some(r#"{"registry":"minecraft:worldgen/biome"}"#),
            "#is_forest"
        ),
        Ok(Value::ResourceOrTag {
            name: "minecraft:is_forest".into(),
            tag: true
        })
    );
    assert_eq!(
        parse(
            "minecraft:resource_or_tag_key",
            Some(r#"{"registry":"minecraft:worldgen/structure"}"#),
            "#no_such_tag"
        ),
        Ok(Value::ResourceOrTag {
            name: "minecraft:no_such_tag".into(),
            tag: true
        }),
        "tag keys are only resolved when the command runs"
    );
    assert_eq!(
        parse("minecraft:template_rotation", None, "clockwise_90"),
        Ok(Value::TemplateRotation(1))
    );
    assert_eq!(
        parse("minecraft:template_rotation", None, "180"),
        Ok(Value::TemplateRotation(2))
    );
    assert_eq!(
        parse("minecraft:template_rotation", None, "sideways"),
        Err("argument.enum.invalid".into())
    );
    assert_eq!(
        parse("minecraft:template_mirror", None, "front_back"),
        Ok(Value::TemplateMirror(2))
    );
    assert_eq!(
        parse("minecraft:feature", None, "minecraft:oak"),
        Ok(Value::Identifier("minecraft:oak".into()))
    );
    assert!(matches!(
        parse("minecraft:feature", None, "{type:\"minecraft:no_op\"}"),
        Ok(Value::Nbt(_))
    ));
    assert_eq!(
        parse("minecraft:resource_location", None, "a:b/c"),
        Ok(Value::Identifier("a:b/c".into()))
    );
    assert_eq!(
        parse("minecraft:resource_location", None, "A"),
        Ok(Value::Identifier("minecraft:".into()))
    );
    assert_eq!(
        parse("minecraft:resource_location", None, "a:b:c"),
        Err("argument.id.invalid".into())
    );
    assert_eq!(
        parse("minecraft:swizzle", None, "xz"),
        Ok(Value::Swizzle([true, false, true]))
    );
    assert_eq!(
        parse("minecraft:swizzle", None, "xx"),
        Err("arguments.swizzle.invalid".into())
    );
    assert_eq!(
        parse("minecraft:entity_anchor", None, "eyes"),
        Ok(Value::Anchor(Anchor::Eyes))
    );
    assert_eq!(
        parse("minecraft:entity_anchor", None, "head"),
        Err("argument.anchor.invalid".into())
    );
    assert_eq!(
        parse("minecraft:hex_color", None, "ff0000"),
        Ok(Value::HexColor(-65536))
    );
    assert_eq!(
        parse("minecraft:hex_color", None, "f00"),
        Ok(Value::HexColor(-65536))
    );
    assert_eq!(
        parse("minecraft:hex_color", None, "zz"),
        Err("argument.hexcolor.invalid".into())
    );
    assert_eq!(
        parse(
            "minecraft:uuid",
            None,
            "069a79f4-44e9-4726-a5be-fca90e38aaf5"
        ),
        Ok(Value::Uuid(0x069a79f444e94726a5befca90e38aaf5))
    );
    assert_eq!(
        parse("minecraft:uuid", None, "nope"),
        Err("argument.uuid.invalid".into())
    );
    assert_eq!(
        parse("minecraft:heightmap", None, "world_surface"),
        Ok(Value::Heightmap("world_surface"))
    );
    assert!(matches!(
        parse("minecraft:int_range", None, "1..3"),
        Ok(Value::IntRange(_))
    ));
    assert!(matches!(
        parse("minecraft:nbt_compound_tag", None, "{a:1b}"),
        Ok(Value::Nbt(Tag::Compound(_)))
    ));
    assert_eq!(
        parse("minecraft:nbt_compound_tag", None, "[1]"),
        Err("argument.nbt.expected.compound".into())
    );
    assert_eq!(
        parse("minecraft:nbt_tag", None, "{"),
        Err("argument.nbt.expected.value".into())
    );
    let component = parse(
        "minecraft:component",
        None,
        r#"{"text":"hi","color":"red"}"#,
    )
    .unwrap();
    assert_eq!(
        component,
        Value::Component(TextComponent::new("hi").color("red"))
    );
    assert_eq!(
        parse("minecraft:component", None, "\"plain\""),
        Ok(Value::Component(TextComponent::new("plain")))
    );
    assert_eq!(
        parse("minecraft:component", None, "{"),
        Err("argument.component.invalid".into())
    );
}

#[tuff_core::watchdog]
#[test]
fn reads_entities_profiles_and_messages() {
    let single = Some(r#"{"amount":"single","type":"players"}"#);
    assert!(matches!(
        parse("minecraft:entity", single, "@p"),
        Ok(Value::Selector(_))
    ));
    assert_eq!(
        parse("minecraft:entity", single, "@a"),
        Err("argument.player.toomany".into())
    );
    assert_eq!(
        parse("minecraft:entity", single, "@e[limit=1]"),
        Err("argument.player.entities".into())
    );
    assert!(matches!(
        parse("minecraft:entity", single, "@s"),
        Ok(Value::Selector(_))
    ));
    let entities = Some(r#"{"amount":"single","type":"entities"}"#);
    assert_eq!(
        parse("minecraft:entity", entities, "@e"),
        Err("argument.entity.toomany".into())
    );
    assert_eq!(
        parse("minecraft:game_profile", None, "@e"),
        Err("argument.player.entities".into())
    );
    assert_eq!(
        parse("minecraft:game_profile", None, "Notch rest"),
        Ok(Value::Profile(ProfileSelector::Name("Notch".into())))
    );
    let message = parse("minecraft:message", None, "hi @a and @p!").unwrap();
    let Value::Message(message) = message else {
        panic!()
    };
    assert_eq!(message.text, "hi @a and @p!");
    assert_eq!(message.parts.len(), 2);
    assert_eq!((message.parts[0].0, message.parts[0].1), (3, 5));
    let email = parse("minecraft:message", None, "mail me@example.com").unwrap();
    let Value::Message(email) = email else {
        panic!()
    };
    assert_eq!(
        email.parts.len(),
        1,
        "the game reads @e out of an address too"
    );
    let bare = parse("minecraft:message", None, "a @ b @x c").unwrap();
    let Value::Message(bare) = bare else { panic!() };
    assert!(bare.parts.is_empty());
    let mut reader = Reader::new("@a[distance=..5] hi");
    let no_selectors = parse_argument(
        "minecraft:message",
        None,
        &mut reader,
        ParseContext {
            allow_selectors: false,
        },
    )
    .unwrap();
    assert!(matches!(no_selectors, Value::Message(m) if m.parts.is_empty()));
}

#[tuff_core::watchdog]
#[test]
fn suggests_names_resources_and_coordinates() {
    let context = SuggestContext {
        allow_selectors: true,
        player_names: vec!["Alice".into(), "Bob".into()],
        dimensions: vec!["minecraft:overworld".into(), "minecraft:the_nether".into()],
        ..Default::default()
    };
    assert_eq!(
        suggest_argument(
            "minecraft:entity",
            Some(r#"{"type":"players"}"#),
            "",
            &context
        ),
        ["Alice", "Bob", "@p", "@a", "@r", "@s"]
    );
    assert_eq!(
        suggest_argument("minecraft:entity", None, "@e", &context),
        ["@e"]
    );
    assert_eq!(
        suggest_argument("minecraft:entity", None, "al", &context),
        ["Alice"]
    );
    assert_eq!(
        suggest_argument("minecraft:dimension", None, "neth", &context),
        ["minecraft:the_nether"]
    );
    assert_eq!(
        suggest_argument("minecraft:gamemode", None, "s", &context),
        ["survival", "spectator"]
    );
    assert_eq!(
        suggest_argument(
            "minecraft:resource",
            Some(r#"{"registry":"minecraft:mob_effect"}"#),
            "night",
            &context
        ),
        ["minecraft:night_vision"]
    );
    assert!(
        suggest_argument("minecraft:block_state", None, "diamond_o", &context)
            .contains(&"minecraft:diamond_ore".to_string())
    );
    assert!(
        suggest_argument("minecraft:block_predicate", None, "#log", &context)
            .contains(&"#minecraft:logs".to_string())
    );
    assert_eq!(
        suggest_argument("minecraft:vec3", None, "", &context),
        ["~", "~ ~", "~ ~ ~"]
    );
    assert_eq!(
        suggest_argument("minecraft:vec3", None, "1", &context),
        ["1 ~", "1 ~ ~"]
    );
    assert_eq!(
        suggest_argument("minecraft:vec3", None, "1 2", &context),
        ["1 2 ~"]
    );
    assert_eq!(
        suggest_argument("minecraft:block_pos", None, "^", &context),
        ["^ ^", "^ ^ ^"]
    );
    assert_eq!(
        suggest_argument("minecraft:rotation", None, "", &context),
        ["~", "~ ~"]
    );
    assert_eq!(
        suggest_argument("minecraft:time", None, "5", &context),
        ["5d", "5s", "5t"]
    );
    assert_eq!(
        suggest_argument("brigadier:bool", None, "t", &context),
        ["true"]
    );
    assert!(matches_sub_str("vis", "night_vision"));
    assert!(!matches_sub_str("ision", "night_vision"));
}
