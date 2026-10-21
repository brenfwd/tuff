use super::*;
use serde_json::json;

fn j(input: &str) -> Value {
    serde_json::from_str(&parse(input).to_json()).unwrap()
}

#[tuff_core::watchdog]
#[test]
fn plain_text_stays_plain() {
    assert_eq!(parse("hello"), TextComponent::new("hello"));
}

#[tuff_core::watchdog]
#[test]
fn colors_and_decorations_nest_and_close() {
    assert_eq!(
        j("<red>a<bold>b</bold>c</red>d"),
        json!({"text":"","extra":[
            {"text":"","color":"red","extra":[{"text":"a"},{"text":"b","bold":true},{"text":"c"}]},
            {"text":"d"}
        ]})
    );
    assert_eq!(j("<#ff00AA>x"), json!({"text":"x","color":"#FF00AA"}));
    assert_eq!(
        j("<color:dark_grey>x"),
        json!({"text":"x","color":"dark_gray"})
    );
    assert_eq!(j("<c:#123456>x"), json!({"text":"x","color":"#123456"}));
    assert_eq!(j("<!italic>x"), json!({"text":"x","italic":false}));
    assert_eq!(j("<b:false>x"), json!({"text":"x","bold":false}));
    assert_eq!(
        j("<st><u><obf><em>x"),
        json!({"text":"x","italic":true,"underlined":true,"strikethrough":true,"obfuscated":true})
    );
}

#[tuff_core::watchdog]
#[test]
fn closing_an_outer_tag_closes_inner_ones_and_reset_closes_all() {
    assert_eq!(
        j("<red><bold>a</red>b"),
        json!({"text":"","extra":[{"text":"a","color":"red","bold":true},{"text":"b"}]})
    );
    assert_eq!(
        j("<red><bold>a<reset>b"),
        json!({"text":"","extra":[{"text":"a","color":"red","bold":true},{"text":"b"}]})
    );
}

#[tuff_core::watchdog]
#[test]
fn unknown_tags_and_stray_closers() {
    assert_eq!(parse("<nope>x</nope>").plain(), "<nope>x</nope>");
    assert_eq!(parse("a</red>b").plain(), "a</red>b");
    assert_eq!(parse("a</reset>b").plain(), "ab");
    assert_eq!(parse("a < b > c").plain(), "a < b > c");
    assert_eq!(parse("<red").plain(), "<red");
    assert_eq!(parse("<<red>x").plain(), "<x");
    assert_eq!(parse("<color:nocolor>x").plain(), "<color:nocolor>x");
}

#[tuff_core::watchdog]
#[test]
fn escapes_and_pre() {
    assert_eq!(parse("\\<red>x").plain(), "<red>x");
    assert_eq!(parse("a\\\\b\\c").plain(), "a\\b\\c");
    assert_eq!(parse("<pre><red>x</pre>y").plain(), "<red>xy");
    let text = "<red> \\ weird";
    assert_eq!(parse(&escape(text)).plain(), text);
    assert_eq!(strip_tags("<red>a<b>b"), "ab");
}

#[tuff_core::watchdog]
#[test]
fn quoted_arguments_keep_colons_and_escaped_quotes() {
    let c = parse("<click:run_command:'/say a:b'>x");
    assert_eq!(
        c.events().unwrap().click,
        Some(ClickEvent::RunCommand("/say a:b".into()))
    );
    let c = parse("<insert:\"say \\\"hi\\\"\">x");
    assert_eq!(c.events().unwrap().insertion.as_deref(), Some("say \"hi\""));
    let c = parse("<hover:show_text:'<red>tip >'>x");
    assert_eq!(
        c.events().unwrap().hover,
        Some(Hover::Text(TextComponent::new("tip >").color("red")))
    );
}

#[tuff_core::watchdog]
#[test]
fn click_hover_and_insert_events() {
    let url = parse("<click:open_url:'https://example.com'>go");
    assert_eq!(
        url.events().unwrap().click,
        Some(ClickEvent::OpenUrl("https://example.com".into()))
    );
    let page = parse("<click:change_page:3>p");
    assert_eq!(
        page.events().unwrap().click,
        Some(ClickEvent::ChangePage(3))
    );
    let entity =
        parse("<hover:show_entity:minecraft:pig:00000000-0000-0000-0000-000000000001:Bob>e");
    assert!(
        entity.events().is_none(),
        "unquoted namespaced id splits into too many args"
    );
    let entity =
        parse("<hover:show_entity:'minecraft:pig':00000000-0000-0000-0000-000000000001:Bob>e");
    assert_eq!(
        entity.events().unwrap().hover,
        Some(Hover::Entity {
            kind: "minecraft:pig".into(),
            uuid: 1,
            name: Some(TextComponent::new("Bob"))
        })
    );
    let item = parse("<hover:show_item:'minecraft:stone':5>i");
    assert_eq!(
        item.events().unwrap().hover,
        Some(Hover::Item {
            id: "minecraft:stone".into(),
            count: 5,
            components: None
        })
    );
}

#[tuff_core::watchdog]
#[test]
fn inserting_tags() {
    assert_eq!(
        j("a<newline>b<br>c")
            .get("extra")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        5
    );
    assert_eq!(j("<key:key.jump>"), json!({"keybind":"key.jump"}));
    assert_eq!(
        j("<lang:block.minecraft.stone:'<red>x'>"),
        json!({"translate":"block.minecraft.stone","with":[{"text":"x","color":"red"}]})
    );
    assert_eq!(
        j("<tr_or:a.b:Fallback>"),
        json!({"translate":"a.b","fallback":"Fallback"})
    );
    assert_eq!(
        j("<score:@s:kills>"),
        json!({"score":{"name":"@s","objective":"kills"}})
    );
    assert_eq!(j("<sel:@a:', '>")["selector"], json!("@a"));
    assert_eq!(
        j("<nbt:storage:'a:b':path:interpret>")["interpret"],
        json!(true)
    );
    assert_eq!(j("<font:uniform>x"), json!({"text":"x","font":"uniform"}));
    assert_eq!(
        j("<font:minecraft:uniform>x"),
        json!({"text":"x","font":"minecraft:uniform"})
    );
    assert_eq!(
        parse("<head:Notch>").special_contents(),
        Some(&Special::PlayerHead {
            profile: json!({"name":"Notch"}),
            hat: true
        })
    );
    assert_eq!(
        parse("<sprite:'minecraft:blocks':'block/stone'>").special_contents(),
        Some(&Special::Atlas {
            atlas: Some("minecraft:blocks".into()),
            sprite: "block/stone".into()
        })
    );
}

#[tuff_core::watchdog]
#[test]
fn shadow_tag() {
    assert_eq!(
        parse("<shadow:red>x").shadow_color(),
        Some(0x3FFF5555_u32 as i32)
    );
    assert_eq!(
        parse("<shadow:#000000:1>x").shadow_color(),
        Some(0xFF000000_u32 as i32)
    );
    assert_eq!(parse("<!shadow>x").shadow_color(), Some(0));
}

fn colors_of(component: &TextComponent) -> Vec<(String, String)> {
    let mut out = Vec::new();
    fn walk(c: &TextComponent, inherited: &str, out: &mut Vec<(String, String)>) {
        let color = c.color.as_deref().unwrap_or(inherited);
        if !c.text.is_empty() {
            out.push((c.text.clone(), color.to_owned()));
        }
        for child in &c.extra {
            walk(child, color, out);
        }
    }
    walk(component, "", &mut out);
    out
}

#[tuff_core::watchdog]
#[test]
fn gradient_interpolates_per_character() {
    let runs = colors_of(&parse("<gradient:#000000:#ffffff>abc"));
    assert_eq!(
        runs,
        vec![
            ("a".into(), "#000000".into()),
            ("b".into(), "#808080".into()),
            ("c".into(), "#FFFFFF".into())
        ]
    );
    let runs = colors_of(&parse("<gradient:red:red>aaaa"));
    assert_eq!(runs, vec![("aaaa".into(), "#FF5555".into())]);
    let runs = colors_of(&parse("<gradient>ab"));
    assert_eq!(
        runs,
        vec![
            ("a".into(), "#FFFFFF".into()),
            ("b".into(), "#000000".into())
        ]
    );
    let reversed = colors_of(&parse("<gradient:#000000:#ffffff:-1>ab"));
    assert_eq!(reversed[0].1, "#FFFFFF");
    assert_eq!(parse("<gradient:red>x").plain(), "<gradient:red>x");
}

#[tuff_core::watchdog]
#[test]
fn gradient_spans_children_and_keeps_explicit_colors() {
    let runs = colors_of(&parse(
        "<gradient:#000000:#ffffff>a<bold>b</bold><blue>c</blue>d",
    ));
    assert_eq!(runs[0], ("a".into(), "#000000".into()));
    assert_eq!(runs[1], ("b".into(), "#555555".into()));
    assert_eq!(runs[2], ("c".into(), "blue".into()));
    assert_eq!(runs[3], ("d".into(), "#FFFFFF".into()));
}

#[tuff_core::watchdog]
#[test]
fn rainbow_transition_and_pride() {
    let runs = colors_of(&parse("<rainbow>ab"));
    assert_eq!(runs[0].1, "#FF0000");
    assert_eq!(runs[1].1, "#00FFFF");
    let runs = colors_of(&parse("<rainbow:!>ab"));
    assert_eq!(runs[0].1, "#00FFFF");
    assert_eq!(
        j("<transition:#000000:#ffffff:0.5>x"),
        json!({"text":"x","color":"#808080"})
    );
    assert_eq!(colors_of(&parse("<pride:trans>ab"))[0].1, "#5BCFFB");
    assert_eq!(parse("<pride:nope>x").plain(), "<pride:nope>x");
}

#[tuff_core::watchdog]
#[test]
fn placeholders_insert_unparsed_components() {
    let out = parse_with("<gold><player></gold>: <message>", &|name| match name {
        "player" => Some(TextComponent::new("Steve")),
        "message" => Some(TextComponent::new("<red>hi")),
        _ => None,
    });
    assert_eq!(out.plain(), "Steve: <red>hi");
}

#[tuff_core::watchdog]
#[test]
fn hostile_input_is_bounded() {
    let deep = "<red>".repeat(10_000) + "x";
    assert_eq!(parse(&deep).plain(), "x");
    let mut nested = String::from("x");
    for _ in 0..12 {
        nested = format!(
            "<hover:show_text:'{}'>y",
            nested.replace('\\', "\\\\").replace('\'', "\\'")
        );
    }
    parse(&nested);
    let huge = "a".repeat(MAX_INPUT * 2);
    assert_eq!(parse(&huge).text.len(), MAX_INPUT);
}

#[tuff_core::watchdog]
#[test]
fn legacy_codes() {
    assert_eq!(
        serde_json::from_str::<Value>(&from_legacy("&cRed &lbold&r plain", '&').to_json()).unwrap(),
        json!({"text":"","extra":[{"text":"Red ","color":"red"},{"text":"bold","color":"red","bold":true},{"text":" plain"}]})
    );
    assert_eq!(
        from_legacy("\u{a7}#ff0000x", '\u{a7}'),
        TextComponent::new("x").color("#FF0000")
    );
    assert_eq!(
        from_legacy("&x&1&2&3&4&5&6x", '&'),
        TextComponent::new("x").color("#123456")
    );
    assert_eq!(
        from_legacy("&lB&cR", '&').extra[1],
        TextComponent::new("R").color("red")
    );
    assert_eq!(from_legacy("a & b &z", '&').plain(), "a & b &z");
}

#[tuff_core::watchdog]
#[test]
fn mixed_legacy_and_minimessage() {
    assert_eq!(
        legacy_to_minimessage("&aHi &lthere"),
        "<reset><green>Hi <bold>there"
    );
    let out = parse_mixed("&aHi <bold>x");
    assert_eq!(colors_of(&out)[0], ("Hi ".into(), "green".into()));
}
