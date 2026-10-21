//! Plugin chest menus and entity interaction filters.

use super::*;
use crate::protocol::play::Serverbound;
use crate::world::session::hooks::SessionTick;
use tuff_plugin_wasm::menus::{MenuClicks, MenuOp, MenuSessions};
use tuff_plugin_wasm::sandbox::uuid;
use tuff_plugin_wasm::wit::bindings::types::{EntityFilter, MenuItem};
use tuff_vanilla_data::packet::play::Clientbound;
use tuff_vanilla_data::registry::EntityType;

const WINDOW: i32 = tuff_plugin_wasm::menus::WINDOW;

fn item(slot: u8, id: &str) -> MenuItem {
    MenuItem {
        slot,
        item: id.into(),
        count: 1,
        name: Some("Wool".into()),
        lore: vec!["costs 4 iron".into()],
        components: Some("{\"minecraft:enchantment_glint_override\":1b}".into()),
    }
}

fn list(items: Vec<MenuItem>) -> WitValue<'static> {
    items.to_value().into_owned()
}

/// `menu` returns 1000 times a refused row count, plus 100 times a refused
/// item, plus 10 times a refused slot, plus the valid menu's result (0 for
/// ok).
///
/// The listener shouts one line per event it hears.
fn shopkeeper() -> Vec<u8> {
    let mut data = Data::new();
    let kinds = EventKinds::MENU_CLICK | EventKinds::MENU_CLOSE;
    let subscribe = data.call("subscribe", &[kinds.to_value()]);
    let filter = EntityFilter {
        kind: Some("villager".into()),
        tag: None,
        scope: super::everywhere(),
    };
    let intercept = data.call("intercept-entity-interact", &[filter.to_value()]);
    let register = register(&mut data, &["menu"], &format!("{subscribe} {intercept}"));
    let who = || uuid(77).to_value().into_owned();
    let open = |data: &mut Data, rows: u8, items: Vec<MenuItem>| {
        data.call(
            "open-menu",
            &[who(), s("shop"), WitValue::U8(rows), s("Shop"), list(items)],
        )
    };
    let bad_rows = open(&mut data, 7, vec![item(0, "minecraft:white_wool")]);
    let bad_item = open(&mut data, 3, vec![item(0, "minecraft:nope")]);
    let bad_slot = open(&mut data, 1, vec![item(9, "minecraft:white_wool")]);
    let good = open(&mut data, 3, vec![item(4, "minecraft:white_wool")]);
    let load = format!("(i32.load8_u (i32.const {RET}))");
    let code = ok_code(
        "(i32.add (i32.add (i32.mul (local.get $a) (i32.const 1000)) (i32.mul (local.get $b) (i32.const 100))) (i32.add (i32.mul (local.get $c) (i32.const 10)) (local.get $d)))",
    );
    let click = text(&mut data, "click ");
    let close = text(&mut data, "close ");
    let interact = text(&mut data, "interact ");
    let body = format!(
        "{register}
         (func {} (local $a i32) (local $b i32) (local $c i32) (local $d i32)
           {bad_rows} (local.set $a {load})
           {bad_item} (local.set $b {load})
           {bad_slot} (local.set $c {load})
           {good} (local.set $d {load})
           {code})
         (func {}
           (call $clear)
           (if (i32.eq (i32.load8_u (local.get 0)) (i32.const 15))
             (then (call $put {click}) (call $put_int (i32.load8_u offset=40 (local.get 0))) {SHOUT}))
           (if (i32.eq (i32.load8_u (local.get 0)) (i32.const 16))
             (then (call $put {close}) (call $put (i32.load offset=32 (local.get 0)) (i32.load offset=36 (local.get 0))) {SHOUT}))
           (if (i32.eq (i32.load8_u (local.get 0)) (i32.const 17))
             (then (call $put {interact}) (call $put (i32.load offset=48 (local.get 0)) (i32.load offset=52 (local.get 0))) {SHOUT}))
           )
         {}",
        export("run-command"),
        export("on-event"),
        data.wat()
    );
    module(&plain("shopkeeper"), &body)
}

/// `probe` returns 100 times open-menu's refusal, 10 times
/// update-menu-slot's and refuse-login's variant (all 0 when they succeed).
fn menu_prober(manifest: &str) -> Vec<u8> {
    let mut data = Data::new();
    let register = register(&mut data, &["probe"], "");
    let who = || uuid(77).to_value().into_owned();
    let open = data.call(
        "open-menu",
        &[
            who(),
            s("shop"),
            WitValue::U8(1),
            s("Shop"),
            list(vec![item(0, "minecraft:white_wool")]),
        ],
    );
    let update = data.call(
        "update-menu-slot",
        &[
            who(),
            WitValue::U8(1),
            WitValue::Variant(
                1,
                Some(Box::new(
                    item(1, "minecraft:white_wool").to_value().into_owned(),
                )),
            ),
        ],
    );
    let refuse = data.call("refuse-login", &[s("no")]);
    let refusal = |offset: u32| {
        format!(
            "(select (i32.const 0) (i32.load8_u offset={offset} (i32.const {RET})) (i32.eqz (i32.load8_u (i32.const {RET}))))"
        )
    };
    let code = ok_code(
        "(i32.add (i32.add (i32.mul (local.get $a) (i32.const 100)) (i32.mul (local.get $b) (i32.const 10))) (local.get $c))",
    );
    let body = format!(
        "{register}
         (func {} (local $a i32) (local $b i32) (local $c i32)
           {open} (local.set $a {})
           {update} (local.set $b {})
           {refuse} (local.set $c {})
           {code})
         {}",
        export("run-command"),
        refusal(1),
        refusal(1),
        refusal(4),
        data.wat()
    );
    module(manifest, &body)
}

async fn probe(dir: &std::path::Path) -> i64 {
    let world = start(dir).await;
    let outcome = host(&world)
        .command(Player::new(77, "Steve"), "probe".into())
        .await
        .expect("the plugin owns /probe");
    world.shutdown().await;
    outcome.result.expect("probe runs").into()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn menu_calls_need_the_player_write_grant_but_refuse_login_stays_open() {
    let denied_dir = TestDir::new();
    install(
        denied_dir.path(),
        "probe.wasm",
        &menu_prober(&format!("{}\ncap.world-write = all", plain("probe"))),
    );
    assert_eq!(probe(denied_dir.path()).await, 331);

    let granted_dir = TestDir::new();
    install(
        granted_dir.path(),
        "probe.wasm",
        &menu_prober(&plain("probe")),
    );
    assert_eq!(probe(granted_dir.path()).await, 1);
}

fn ids(frames: &[bytes::Bytes]) -> Vec<i32> {
    frames
        .iter()
        .map(|frame| framed_payload(frame).read_var_int().unwrap())
        .collect()
}

fn drain(client: &mut Client) -> Vec<bytes::Bytes> {
    let mut frames = Vec::new();
    while let Ok(frame) = client.frames.try_recv() {
        frames.push(frame);
    }
    frames
}

fn click(slot: i16, window_id: i32) -> Serverbound {
    Serverbound::ContainerClick {
        window_id,
        state_id: 0,
        slot,
        button: 0,
        mode: 0,
        changed: Vec::new(),
        carried: None,
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_plugin_menu_shows_cancels_every_click_and_reports_them() {
    let dir = TestDir::new();
    install(dir.path(), "shop.wasm", &shopkeeper());
    let world = start(dir.path()).await;
    let mut client = Client::new(&world);
    let outcome = host(&world)
        .command(Player::new(77, "Steve"), "menu".into())
        .await
        .expect("the plugin owns /menu");
    assert_eq!(outcome.result, Ok(1110));
    assert_eq!(host(&world).menus().open_menu(77), Some(("shop".into(), 3)));

    drain(&mut client);
    MenuSessions
        .tick(&mut client.session, SessionTick::Use)
        .await;
    let shown = ids(&drain(&mut client));
    assert_eq!(
        shown,
        [
            Clientbound::OpenScreen.id(),
            Clientbound::ContainerSetContent.id()
        ]
    );
    MenuSessions
        .tick(&mut client.session, SessionTick::Use)
        .await;
    assert!(drain(&mut client).is_empty(), "nothing changed");

    assert_eq!(
        MenuClicks
            .intercept(&mut client.session, &click(4, WINDOW))
            .await,
        Verdict::Cancel
    );
    let answered = ids(&drain(&mut client));
    assert!(answered.contains(&Clientbound::ContainerSetContent.id()));
    assert_eq!(count(&world, "click 4").await, 1);
    MenuClicks
        .intercept(&mut client.session, &click(40, WINDOW))
        .await;
    assert_eq!(
        count(&world, "click 40").await,
        0,
        "a player slot is not reported"
    );
    assert_eq!(
        MenuClicks
            .intercept(&mut client.session, &click(4, 0))
            .await,
        Verdict::Allow,
        "other windows are the server's"
    );

    host(&world).menus().apply(MenuOp::Set {
        player: 77,
        slot: 5,
        stack: None,
    });
    drain(&mut client);
    MenuSessions
        .tick(&mut client.session, SessionTick::Use)
        .await;
    assert_eq!(
        ids(&drain(&mut client)),
        [Clientbound::ContainerSetSlot.id()]
    );

    assert_eq!(
        MenuClicks
            .intercept(&mut client.session, &Serverbound::ContainerClose(WINDOW))
            .await,
        Verdict::Cancel
    );
    assert_eq!(count(&world, "close shop").await, 1);
    assert_eq!(host(&world).menus().open_menu(77), None);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_plugin_can_close_its_menu_and_the_player_hears_of_it() {
    let dir = TestDir::new();
    install(dir.path(), "shop.wasm", &shopkeeper());
    let world = start(dir.path()).await;
    let mut client = Client::new(&world);
    host(&world)
        .command(Player::new(77, "Steve"), "menu".into())
        .await
        .unwrap();
    MenuSessions
        .tick(&mut client.session, SessionTick::Use)
        .await;
    drain(&mut client);
    host(&world).menus().apply(MenuOp::Close { player: 77 });
    MenuSessions
        .tick(&mut client.session, SessionTick::Use)
        .await;
    assert_eq!(ids(&drain(&mut client)), [Clientbound::ContainerClose.id()]);
    assert_eq!(count(&world, "close shop").await, 1);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn filtered_entity_interactions_are_cancelled_and_reported() {
    use crate::world::messages::{PlayerEvent, PlayerHandle};
    use tuff_world_model::pos::ChunkPos;

    let dir = TestDir::new();
    install(dir.path(), "shop.wasm", &shopkeeper());
    let world = start(dir.path()).await;
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(999);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    let spawn = |kind| {
        summon(
            &region,
            kind,
            Compound::new().with("NoAI", true),
            [7.5, 80.0, 7.5],
        )
    };
    spawn(EntityType::Villager).await;
    spawn(EntityType::Cow).await;
    world.ticks(1).await;
    let mut client = Client::new(&world);
    let mut seen = Vec::new();
    for kind in [EntityType::Villager, EntityType::Cow] {
        let track = expect(&mut events, track_of(kind)).await;
        seen.push(track.entity);
        client.session.see_entity(track);
    }
    let interact = |entity, off_hand| Serverbound::Interact {
        entity,
        off_hand,
        location: [0.0; 3],
        sneaking: false,
    };
    assert_eq!(
        MenuClicks
            .intercept(&mut client.session, &interact(seen[0], false))
            .await,
        Verdict::Cancel
    );
    assert_eq!(
        MenuClicks
            .intercept(&mut client.session, &interact(seen[0], true))
            .await,
        Verdict::Cancel,
        "the off hand is cancelled too"
    );
    assert_eq!(
        MenuClicks
            .intercept(&mut client.session, &interact(seen[1], false))
            .await,
        Verdict::Allow
    );
    let heard = announced(&world).await;
    assert_eq!(
        heard
            .iter()
            .filter(|line| line.starts_with("interact minecraft:villager"))
            .count(),
        1,
        "{heard:?}"
    );
    assert_eq!(
        heard
            .iter()
            .filter(|line| line.starts_with("interact minecraft:cow"))
            .count(),
        1,
        "{heard:?}"
    );
    world.shutdown().await;
}
