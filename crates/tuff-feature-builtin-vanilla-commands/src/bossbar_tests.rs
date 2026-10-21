use super::*;
use tokio::sync::mpsc::UnboundedReceiver;
use tuff_wire::PacketReader;

fn viewer(id: PlayerId, uuid: u128) -> (Viewer, UnboundedReceiver<PlayerEvent>) {
    let (handle, events) = PlayerHandle::new(id);
    (
        Viewer {
            id,
            uuid,
            name: format!("player{id}"),
            handle,
        },
        events,
    )
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_panic_restarts_the_actor_keeping_its_bars() {
    let (_clock, clock_rx) = tokio::sync::watch::channel(0u64);
    let mut shared = tuff_test_support::bare_shared();
    shared.clock = clock_rx;
    let bars = BossBars::start(Arc::new(shared));
    let made = bars
        .with(|bars| bars.create("a:b", 1, TextComponent::new("x")).is_some())
        .await;
    assert_eq!(made, Some(true));
    assert!(bars.with(|_| panic!("test panic")).await.is_none());
    assert_eq!(
        bars.with(|bars| bars.ids()).await,
        Some(vec!["a:b".to_owned()])
    );
}

#[derive(Debug, PartialEq)]
enum Seen {
    Add {
        title: String,
        progress: f32,
        color: i32,
        overlay: i32,
        flags: u8,
    },
    Remove,
    Progress(f32),
    Title(String),
    Style(i32, i32),
    Flags(u8),
}

fn read_title(r: &mut PacketReader) -> String {
    let tag = r.read_nbt().unwrap().unwrap();
    TextComponent::from_nbt(&tag).unwrap().plain()
}

fn seen(events: &mut UnboundedReceiver<PlayerEvent>) -> Vec<(u128, Seen)> {
    let mut out = Vec::new();
    while let Ok(event) = events.try_recv() {
        let PlayerEvent::Packets(frames) = event else {
            continue;
        };
        for frame in frames.iter() {
            let mut r = PacketReader::new(frame.clone());
            assert!(r.read_var_int().unwrap() > 0);
            assert_eq!(r.read_var_int().unwrap(), 0, "uncompressed frame");
            assert_eq!(
                r.read_var_int().unwrap(),
                tuff_vanilla_data::packet::play::Clientbound::BossEvent.id()
            );
            let uuid = r.read_uuid().unwrap();
            let action = match r.read_var_int().unwrap() {
                0 => Seen::Add {
                    title: read_title(&mut r),
                    progress: r.read_float().unwrap(),
                    color: r.read_var_int().unwrap(),
                    overlay: r.read_var_int().unwrap(),
                    flags: r.read_unsigned_byte().unwrap(),
                },
                1 => Seen::Remove,
                2 => Seen::Progress(r.read_float().unwrap()),
                3 => Seen::Title(read_title(&mut r)),
                4 => Seen::Style(r.read_var_int().unwrap(), r.read_var_int().unwrap()),
                5 => Seen::Flags(r.read_unsigned_byte().unwrap()),
                other => panic!("unknown boss action {other}"),
            };
            out.push((uuid, action));
        }
    }
    out
}

fn add(title: &str, progress: f32, color: i32, overlay: i32, flags: u8) -> Seen {
    Seen::Add {
        title: title.into(),
        progress,
        color,
        overlay,
        flags,
    }
}

#[tuff_core::watchdog]
#[test]
fn a_bar_shows_its_changes_only_to_its_online_players_while_visible() {
    let mut bars = Bars::default();
    bars.create("minecraft:fight", 7, TextComponent::new("Fight"))
        .unwrap();
    assert!(
        bars.create("minecraft:fight", 8, TextComponent::new("Again"))
            .is_none()
    );
    let (alex, mut alex_events) = viewer(1, 11);
    let (sam, mut sam_events) = viewer(2, 22);
    let bar = bars.get_mut("minecraft:fight").unwrap();
    assert!(bar.set_players(vec![alex.clone()]));
    assert!(!bar.set_players(vec![alex.clone()]));
    assert_eq!(seen(&mut alex_events), [(7, add("Fight", 0.0, 6, 0, 0))]);

    bar.set_value(50);
    assert!(bar.set_color(Color::Red));
    assert!(!bar.set_color(Color::Red));
    assert!(bar.set_overlay(Overlay::Notched10));
    assert!(bar.set_name(TextComponent::new("Boss")));
    bar.set_flags(true, false, true);
    assert_eq!(
        seen(&mut alex_events),
        [
            (7, Seen::Progress(0.5)),
            (7, Seen::Style(2, 0)),
            (7, Seen::Style(2, 2)),
            (7, Seen::Title("Boss".into())),
            (7, Seen::Flags(5)),
        ]
    );

    bar.set_max(25);
    bar.set_max(20);
    assert_eq!(seen(&mut alex_events), [(7, Seen::Progress(1.0))]);
    bar.set_visible(false);
    bar.set_value(10);
    assert_eq!(seen(&mut alex_events), [(7, Seen::Remove)]);
    bar.set_visible(true);
    assert_eq!(seen(&mut alex_events), [(7, add("Boss", 0.5, 2, 2, 5))]);

    assert!(bar.set_players(vec![sam.clone()]));
    assert_eq!(seen(&mut alex_events), [(7, Seen::Remove)]);
    assert_eq!(seen(&mut sam_events), [(7, add("Boss", 0.5, 2, 2, 5))]);
    assert_eq!(bar.players().iter().copied().collect::<Vec<_>>(), [22]);

    bars.remove("minecraft:fight").unwrap();
    assert_eq!(seen(&mut sam_events), [(7, Seen::Remove)]);
    assert!(bars.is_empty());
}

#[tuff_core::watchdog]
#[test]
fn players_who_leave_see_the_bar_again_when_they_return() {
    let mut bars = Bars::default();
    bars.create("minecraft:a", 1, TextComponent::new("A"));
    bars.create("minecraft:b", 2, TextComponent::new("B"));
    let (alex, mut events) = viewer(1, 11);
    bars.get_mut("minecraft:a")
        .unwrap()
        .set_players(vec![alex.clone()]);
    seen(&mut events);
    bars.disconnect(1);
    assert!(bars.get("minecraft:a").unwrap().online().is_empty());
    assert!(seen(&mut events).is_empty());

    let (back, mut back_events) = viewer(5, 11);
    bars.connect(back.clone());
    bars.connect(back);
    assert_eq!(seen(&mut back_events), [(1, add("A", 0.0, 6, 0, 0))]);
    assert_eq!(bars.get("minecraft:a").unwrap().online().len(), 1);
}

#[tuff_core::watchdog]
#[test]
fn progress_divides_as_floats_and_keeps_the_games_clamp() {
    let mut bar = Bar::new("minecraft:x".into(), 1, TextComponent::new("X"));
    bar.set_value(1);
    bar.set_max(3);
    assert_eq!(bar.progress(), 1.0 / 3.0);
    bar.set_value(9);
    assert_eq!(bar.progress(), 1.0);
    bar.set_max(-3);
    assert_eq!(bar.progress(), 0.0);
    bar.set_value(0);
    bar.set_max(0);
    assert!(bar.progress().is_nan());
}

#[tuff_core::watchdog]
#[test]
fn bars_save_in_the_game_format_with_defaults_left_out() {
    let mut bars = Bars::default();
    bars.create("minecraft:plain", 1, TextComponent::new("Plain"));
    bars.create("custom:fancy", 2, TextComponent::new("Fancy").color("gold"));
    let fancy = bars.get_mut("custom:fancy").unwrap();
    fancy.set_max(40);
    fancy.set_value(10);
    fancy.set_color(Color::Purple);
    fancy.set_overlay(Overlay::Notched20);
    fancy.set_visible(false);
    fancy.set_flags(true, true, false);
    let (alex, _events) = viewer(1, 0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10);
    fancy.set_players(vec![alex]);

    let nbt = bars.to_nbt();
    let plain = nbt.get_compound("minecraft:plain").unwrap();
    assert_eq!(plain.get_str("Name"), Some("Plain"));
    assert_eq!(plain.get_bool("Visible"), Some(true));
    assert_eq!(plain.len(), 2);
    let fancy = nbt.get_compound("custom:fancy").unwrap();
    assert!(fancy.get_compound("Name").is_some());
    assert!(!fancy.contains_key("Visible"));
    assert_eq!(fancy.get_int("Value"), Some(10));
    assert_eq!(fancy.get_int("Max"), Some(40));
    assert_eq!(fancy.get_str("Color"), Some("purple"));
    assert_eq!(fancy.get_str("Overlay"), Some("notched_20"));
    assert_eq!(fancy.get_bool("DarkenScreen"), Some(true));
    assert_eq!(fancy.get_bool("PlayBossMusic"), Some(true));
    assert!(!fancy.contains_key("CreateWorldFog"));
    assert_eq!(
        fancy.get_list("Players").unwrap()[0].as_int_array(),
        Some(&[0x0102_0304, 0x0506_0708, 0x090a_0b0c, 0x0d0e_0f10][..])
    );

    let mut rng = StdRng::seed_from_u64(1);
    let read = Bars::from_nbt(&nbt, &mut rng);
    let fancy = read.get("custom:fancy").unwrap();
    assert_eq!(fancy.name(), &TextComponent::new("Fancy").color("gold"));
    assert_eq!((fancy.value(), fancy.max()), (10, 40));
    assert_eq!(fancy.progress(), 0.25);
    assert!(!fancy.visible());
    assert_eq!(fancy.color(), Color::Purple);
    assert_eq!(fancy.overlay(), Overlay::Notched20);
    assert_eq!(fancy.flags(), 3);
    assert_eq!(fancy.players().len(), 1);
    assert!(fancy.online().is_empty());
    assert_eq!(fancy.uuid() >> 76 & 0xF, 4);
    assert_eq!(read.to_nbt(), nbt);
}

#[tuff_core::watchdog]
#[test]
fn unreadable_bars_are_dropped_and_unmodelled_names_kept() {
    let odd_name = Tag::Compound(
        Compound::new()
            .with("text", "Odd")
            .with("strikethrough", Tag::Byte(1)),
    );
    let data = Compound::new()
        .with(
            "minecraft:nameless",
            Compound::new().with("Visible", Tag::Byte(1)),
        )
        .with(
            "minecraft:bad_color",
            Compound::new().with("Name", "x").with("Color", "beige"),
        )
        .with(
            "minecraft:odd",
            Compound::new().with("Name", odd_name.clone()),
        );
    let read = Bars::from_nbt(&data, &mut StdRng::seed_from_u64(2));
    assert_eq!(read.ids(), ["minecraft:odd"]);
    let odd = read.get("minecraft:odd").unwrap();
    assert_eq!(odd.name().plain(), "Odd");
    assert!(!odd.visible());
    assert_eq!(odd.to_nbt().get("Name"), Some(&odd_name));
}

#[tuff_core::watchdog]
#[test]
fn feedback_names_carry_the_bar_color_and_id() {
    let mut bar = Bar::new("minecraft:x".into(), 1, TextComponent::new("X"));
    bar.set_color(Color::Pink);
    let shown = bar.display_name();
    assert_eq!(shown.plain(), "[X]");
    assert_eq!(shown.color.as_deref(), Some("red"));
    let events = shown.events().unwrap();
    assert_eq!(events.insertion.as_deref(), Some("minecraft:x"));
}
