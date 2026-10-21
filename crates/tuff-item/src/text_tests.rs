use super::*;

struct Fixed;
impl ComponentResolver for Fixed {
    fn selector(&self, _: &str) -> Vec<TextComponent> {
        vec![TextComponent::new("A"), TextComponent::new("B")]
    }
    fn score(&self, name: &str, objective: &str) -> Option<TextComponent> {
        (name == "x" && objective == "o").then(|| TextComponent::new("7"))
    }
    fn nbt(&self, _: &NbtSource, _: &str) -> Vec<Tag> {
        vec![Tag::String("{\"text\":\"hi\"}".into()), Tag::Int(3)]
    }
    fn nbt_text(&self, tag: &Tag, _: bool) -> TextComponent {
        TextComponent::new(format!("{tag:?}"))
    }
}

#[tuff_core::watchdog]
#[test]
fn special_contents_round_trip_and_resolve() {
    let selector = TextComponent::special(Special::Selector {
        pattern: "@a".into(),
        separator: None,
    });
    assert_eq!(
        TextComponent::from_nbt(&selector.to_nbt()),
        Some(selector.clone())
    );
    assert_eq!(selector.resolve(&Fixed).plain(), "A, B");
    let score = TextComponent::special(Special::Score {
        name: "x".into(),
        objective: "o".into(),
    });
    assert_eq!(
        TextComponent::from_nbt(&score.to_nbt()),
        Some(score.clone())
    );
    assert_eq!(score.resolve(&Fixed).plain(), "7");
    let nbt = TextComponent::special(Special::Nbt {
        path: "p".into(),
        interpret: true,
        plain: false,
        separator: Some(TextComponent::new("|")),
        source: NbtSource::Storage("a:b".into()),
    });
    assert_eq!(TextComponent::from_nbt(&nbt.to_nbt()), Some(nbt.clone()));
    assert_eq!(nbt.resolve(&Fixed).plain(), "hi|");
    let both = TextComponent::from_json_str(
        r#"{"nbt":"p","storage":"a:b","interpret":true,"plain":true}"#,
    );
    assert_eq!(both, None);
}

#[tuff_core::watchdog]
#[test]
fn resolve_stops_at_the_depth_and_nbt_limits() {
    let selector = TextComponent::special(Special::Selector {
        pattern: "@a".into(),
        separator: None,
    });
    let mut nested = selector.clone();
    for _ in 0..3 {
        let mut wrapper = TextComponent::new("");
        wrapper.extra.push(nested);
        nested = wrapper;
    }
    let mut shallow = ResolveBudget {
        depth_limit: 2,
        ..ResolveBudget::default()
    };
    assert_eq!(
        nested.resolve_limited(&Fixed, &mut shallow, 0).plain(),
        "",
        "the selector sits past the depth limit and is copied unresolved"
    );
    assert_eq!(nested.resolve(&Fixed).plain(), "A, B");

    let nbt = TextComponent::special(Special::Nbt {
        path: "p".into(),
        interpret: true,
        plain: false,
        separator: None,
        source: NbtSource::Storage("a:b".into()),
    });
    let mut spent = ResolveBudget {
        nbt_limit: 0,
        nbt_count: 1,
        ..ResolveBudget::default()
    };
    assert_eq!(nbt.resolve_limited(&Fixed, &mut spent, 0).plain(), "");
}

#[tuff_core::watchdog]
#[test]
fn extra_style_fields_fallback_and_colors() {
    let styled = TextComponent::new("x")
        .strikethrough(true)
        .obfuscated(false);
    assert_eq!(TextComponent::from_nbt(&styled.to_nbt()), Some(styled));
    let parsed = TextComponent::from_json_str(
        r##"{"text":"x","font":"a:b","shadow_color":-16777216,"color":"#ff0000"}"##,
    )
    .unwrap();
    assert_eq!(parsed.font(), Some("a:b"));
    assert_eq!(parsed.shadow_color(), Some(-16777216));
    assert!(parsed.to_json().contains("shadow_color"));
    let floats = TextComponent::from_json_str(r#"{"text":"x","shadow_color":[1.0,0.0,0.0,1.0]}"#);
    assert_eq!(floats.unwrap().shadow_color(), Some(0xFFFF0000u32 as i32));
    let missing = TextComponent::translate("no.such.key", []).fallback("Hello %%");
    assert_eq!(missing.plain(), "Hello %");
    assert_eq!(TextComponent::from_nbt(&missing.to_nbt()), Some(missing));
    assert!(is_valid_color("gold") && is_valid_color("#00ff7f"));
    assert!(!is_valid_color("#1234567") && !is_valid_color("mauve") && !is_valid_color("#"));
    assert_eq!(
        TextComponent::from_json_str(r#"{"text":"x","color":"mauve"}"#),
        None
    );
}

#[tuff_core::watchdog]
#[test]
fn plain_text_serializes_minimal() {
    assert_eq!(TextComponent::new("hi").to_json(), r#"{"text":"hi"}"#);
}

#[tuff_core::watchdog]
#[test]
fn styled_text_with_children() {
    let text = TextComponent::new("Hello, ")
        .color("gray")
        .append(TextComponent::new("Steve").color("gold").bold(true));
    assert_eq!(
        text.to_json(),
        r#"{"text":"Hello, ","color":"gray","extra":[{"text":"Steve","color":"gold","bold":true}]}"#
    );
}

#[tuff_core::watchdog]
#[test]
fn translatable_omits_text_and_converts_to_nbt() {
    let text = TextComponent::translate("death.attack.fall", [TextComponent::new("Steve")])
        .color("yellow");
    assert_eq!(
        text.to_json(),
        r#"{"translate":"death.attack.fall","with":[{"text":"Steve"}],"color":"yellow"}"#
    );
    let nbt = text.to_nbt();
    let compound = nbt.as_compound().unwrap();
    assert_eq!(
        compound.get("translate"),
        Some(&Tag::String("death.attack.fall".into()))
    );
    assert!(compound.get("text").is_none());
}

#[tuff_core::watchdog]
#[test]
fn plain_text_collapses_to_a_string_tag() {
    assert_eq!(TextComponent::new("hi").to_nbt(), Tag::String("hi".into()));
    let styled = TextComponent::new("hi").bold(true);
    assert!(styled.to_nbt().as_compound().is_some());
}

#[tuff_core::watchdog]
#[test]
fn nbt_round_trips_and_rejects_unmodeled_fields() {
    let styled = TextComponent::new("Hello, ")
        .color("gray")
        .italic(false)
        .append(TextComponent::new("Steve").bold(true));
    assert_eq!(TextComponent::from_nbt(&styled.to_nbt()), Some(styled));
    let translated = TextComponent::translate("a.b", [TextComponent::new("x")]);
    assert_eq!(
        TextComponent::from_nbt(&translated.to_nbt()),
        Some(translated)
    );
    assert_eq!(
        TextComponent::from_nbt(&Tag::String("s".into())),
        Some(TextComponent::new("s"))
    );
    let list = Tag::List(vec![Tag::String("a".into()), Tag::String("b".into())]);
    assert_eq!(
        TextComponent::from_nbt(&list),
        Some(TextComponent::new("a").append("b"))
    );
    let with_font = Tag::Compound(Compound::new().with("text", "x").with("font", "f"));
    assert_eq!(
        TextComponent::from_nbt(&with_font).and_then(|c| c.font().map(str::to_owned)),
        Some("f".to_owned())
    );
    let bad_color = Tag::Compound(Compound::new().with("text", "x").with("color", "nope"));
    assert_eq!(TextComponent::from_nbt(&bad_color), None);
    assert_eq!(TextComponent::from_nbt(&Tag::Int(1)), None);
}

#[tuff_core::watchdog]
#[test]
fn object_contents_round_trip_through_nbt() {
    let atlas = TextComponent::special(Special::Atlas {
        atlas: None,
        sprite: "item/stick".into(),
    });
    assert_eq!(TextComponent::from_nbt(&atlas.to_nbt()), Some(atlas));
    let head = TextComponent::special(Special::PlayerHead {
        profile: serde_json::json!({"name": "Steve"}),
        hat: false,
    });
    assert_eq!(TextComponent::from_nbt(&head.to_nbt()), Some(head));
}

#[tuff_core::watchdog]
#[test]
fn events_round_trip_through_nbt() {
    let events = [
        ClickEvent::RunCommand("/say hi".into()),
        ClickEvent::ChangePage(3),
        ClickEvent::ShowDialog(Value::from("minecraft:custom_options")),
        ClickEvent::Custom {
            id: "a:b".into(),
            payload: Some(Value::from("p")),
        },
    ];
    for click in events {
        let component = TextComponent::new("x")
            .click(click)
            .insertion("ins")
            .hover_text(TextComponent::new("h"));
        assert_eq!(
            TextComponent::from_nbt(&component.to_nbt()),
            Some(component)
        );
    }
    let item = TextComponent::new("x");
    let mut item = item;
    item.rare_mut().events.hover = Some(Hover::Item {
        id: "minecraft:stick".into(),
        count: 2,
        components: None,
    });
    assert_eq!(TextComponent::from_nbt(&item.to_nbt()), Some(item));
    let zero_page = Tag::Compound(
        Compound::new().with("text", "x").with(
            "click_event",
            Tag::Compound(
                Compound::new()
                    .with("action", "change_page")
                    .with("page", 0),
            ),
        ),
    );
    assert_eq!(TextComponent::from_nbt(&zero_page), None);
}

/// Expected hashes come from the game's own serializer.
#[tuff_core::watchdog]
#[test]
fn canonical_form_hashes_like_the_game() {
    let hash = |text: TextComponent| text.canon().hash() as i32;
    assert_eq!(hash(TextComponent::new("Sharp")), -1209906280);
    assert_eq!(hash(TextComponent::new("Sharp").bold(true)), 822952786);
    assert_eq!(
        hash(TextComponent::new("A").color("red").append("B")),
        -1442169671
    );
    assert_eq!(
        hash(TextComponent::translate(
            "item.minecraft.stone",
            [TextComponent::new("z")]
        )),
        1652333119
    );
    assert_eq!(
        hash(TextComponent::translate("item.minecraft.stone", [])),
        1287035347
    );
    assert_eq!(hash(TextComponent::new("N").italic(false)), 271656228);
}

#[tuff_core::watchdog]
#[test]
fn plain_translates_a_key_with_no_arguments() {
    assert_eq!(
        TextComponent::translate("argument.player.unknown", []).plain(),
        "That player does not exist"
    );
}

#[tuff_core::watchdog]
#[test]
fn plain_substitutes_sequential_and_positional_arguments() {
    assert_eq!(
        TextComponent::translate("multiplayer.player.joined", [TextComponent::new("Steve")])
            .plain(),
        "Steve joined the game"
    );
    assert_eq!(
        TextComponent::translate("death.attack.outOfWorld", [TextComponent::new("Steve")]).plain(),
        "Steve fell out of the world"
    );
}

#[tuff_core::watchdog]
#[test]
fn plain_recursively_translates_nested_arguments() {
    let message = TextComponent::translate(
        "chat.type.text",
        [
            TextComponent::new("Steve"),
            TextComponent::translate("item.minecraft.netherite_sword", []),
        ],
    );
    assert_eq!(message.plain(), "<Steve> Netherite Sword");
}

#[tuff_core::watchdog]
#[test]
fn plain_falls_back_to_the_bare_key_for_an_unknown_translation() {
    let message = TextComponent::translate("totally.unknown.key", [TextComponent::new("dropped")]);
    assert_eq!(message.plain(), "totally.unknown.key");
}

#[tuff_core::watchdog]
#[test]
fn format_translation_handles_percent_and_out_of_range_arguments() {
    let mut out = String::new();
    format_translation("100%% done: %s and %5$s", &["a".to_owned()], &mut out);
    assert_eq!(out, "100% done: a and ");
}

#[tuff_core::watchdog]
#[test]
fn advancement_message_translates_with_nested_title() {
    let title = TextComponent::translate("chat.square_brackets", [TextComponent::new("Stone Age")]);
    for (frame, verb) in [
        ("task", "has made the advancement"),
        ("goal", "has reached the goal"),
        ("challenge", "has completed the challenge"),
    ] {
        let message = TextComponent::translate(
            format!("chat.type.advancement.{frame}"),
            [TextComponent::new("Steve"), title.clone()],
        );
        assert_eq!(message.plain(), format!("Steve {verb} [Stone Age]"));
        assert_eq!(message.to_value()["with"].as_array().map(Vec::len), Some(2));
    }
}

#[tuff_core::watchdog]
#[test]
fn leave_and_death_messages_translate() {
    assert_eq!(
        TextComponent::translate("multiplayer.player.left", [TextComponent::new("Steve")]).plain(),
        "Steve left the game"
    );
    assert_eq!(
        TextComponent::translate("death.attack.fall", [TextComponent::new("Steve")]).plain(),
        "Steve hit the ground too hard"
    );
}
