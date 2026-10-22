//! Block rules, plugin teams, uses of an item in the air, projectile hits
//! and damage attribution, end to end through a WAT plugin.

use super::*;
use crate::world::entity::{Culprit, ProjectileLanding};
use crate::world::messages::{BreakMode, RegionRequest};
use crate::world::region_api::{BlockAction, PlayerBlockAction, RegionHooks, RegionTask};
use crate::world::session::{DamageSource, HurtBy, HurtResult};
use tuff_plugin_wasm::guard;
use tuff_plugin_wasm::wit::bindings::types::{BlockRule, ChangeRule, Uuid as WitUuid};
use tuff_vanilla_data::registry::EntityType;
use tuff_vanilla_data::{BlockState, damage_type};
use tuff_world_model::BlockPos;
use tuff_world_model::pos::RegionPos;

/// Prints `n` when the option at `disc` holds a u64 at `low`, else `-1`.
fn opt_low(disc: u32, low: u32) -> String {
    format!(
        "(if (i32.load8_u (i32.const {disc}))
           (then (call $put_int (i32.wrap_i64 (i64.load (i32.const {low})))))
           (else (call $put_int (i32.const -1))))
         (call $put_char (i32.const 32))"
    )
}

/// Cancels egg throws, adds a rule with `/rule`, teams 77 and 88 with
/// `/squad`, and broadcasts the use, hit and hurt events it hears.
fn arena() -> Vec<u8> {
    arena_placing(true)
}

fn arena_placing(place: bool) -> Vec<u8> {
    arena_with(place, false)
}

fn arena_with(place: bool, cancel_explosions: bool) -> Vec<u8> {
    let mut data = Data::new();
    let filter = ClickFilter {
        kind: ClickKind::Air,
        item: Some("minecraft:egg".into()),
        tag: None,
        block: None,
        scope: super::everywhere(),
    };
    let kinds = EventKinds::PROJECTILE_HIT | EventKinds::PLAYER_HURT | EventKinds::PLAYER_DEATH;
    let more = data.call("intercept-click", &[filter.to_value()])
        + &data.call("subscribe", &[kinds.to_value()]);
    let register = register(&mut data, &["rule", "squad"], &more);
    let rule = BlockRule {
        dimension: "overworld".into(),
        min: WitPos { x: 0, y: 100, z: 0 },
        max: WitPos { x: 9, y: 110, z: 9 },
        place,
        break_placed: true,
        breakable: vec!["red_bed".into()],
        explode_placed: true,
        blast_proof: vec!["glass".into()],
        cancel_explosions,
    };
    let add = data.call("add-block-rule", &[rule.to_value()]);
    let player = |low: u64| WitUuid { high: 0, low }.to_value().into_owned();
    let red = || WitValue::Variant(1, Some(Box::new(s("red"))));
    let team =
        data.call("set-team", &[player(77), red()]) + &data.call("set-team", &[player(88), red()]);
    let hit = data.call("acting-hit", &[]);
    let damage = data.call("acting-damage", &[]);
    let body = format!(
        r#"{register}
        (func {run} (param $handler i32) (param i32 i64 i64 i32 i32 i32 i32 i32 i32) (result i32)
          (if (i32.eqz (local.get $handler))
            (then
              {add}
              (if (i32.load8_u (i32.const {RET})) (then (return {failed})))
              (return {id})))
          {team}
          {zero})
        (func {event} (param $ev i32) (local $case i32)
          (local.set $case (i32.load8_u (local.get $ev)))
          (call $clear)
          (if (i32.eq (local.get $case) (i32.const 18))
            (then
              (call $put (i32.load offset=32 (local.get $ev)) (i32.load offset=36 (local.get $ev)))))
          (if (i32.eq (local.get $case) (i32.const 19))
            (then
              {hit}
              (call $put (i32.load offset=24 (local.get $ev)) (i32.load offset=28 (local.get $ev)))
              (call $put_char (i32.const 32))
              {owner}
              (if (i32.load8_u (i32.const {block}))
                (then (call $put_int (i32.load (i32.const {block_y}))))
                (else (call $put_int (i32.const -1))))
              (call $put_char (i32.const 32))
              {target}))
          (if (i32.ge_u (local.get $case) (i32.const 10))
            (then (if (i32.le_u (local.get $case) (i32.const 11))
              (then
                {damage}
                (call $put (i32.load offset=32 (local.get $ev)) (i32.load offset=36 (local.get $ev)))
                (call $put_char (i32.const 32))
                {attacker}
                {credit}
                (call $put_int (i32.load8_u (i32.const {credit_player})))))))
          {SHOUT})
        {data}"#,
        run = export("run-command").replace(
            " (param i32 i32 i64 i64 i32 i32 i32 i32 i32 i32) (result i32)",
            ""
        ),
        event = export("on-event").replace(" (param i32)", ""),
        failed = ok_code("(i32.const -1)"),
        id = ok_code(&format!("(i32.load (i32.const {}))", RET + 4)),
        zero = ok_code("(i32.const 0)"),
        owner = opt_low(RET + 8, RET + 24),
        block = RET + 32,
        block_y = RET + 40,
        target = opt_low(RET + 48, RET + 64),
        attacker = opt_low(RET + 8, RET + 24),
        credit = opt_low(RET + 56, RET + 72),
        credit_player = RET + 80,
        data = data.wat(),
    );
    module(&plain("arena"), &body)
}

fn action(pos: BlockPos, state: BlockState, action: BlockAction) -> PlayerBlockAction {
    PlayerBlockAction {
        player: 1000,
        pos,
        state,
        action,
    }
}

async fn allows(region: &crate::world::messages::RegionHandle, action: PlayerBlockAction) -> bool {
    region
        .ask(move |world| guard::Blocks.allows_block_action(world, &action))
        .await
        .unwrap()
}

async fn placed(region: &crate::world::messages::RegionHandle, action: PlayerBlockAction) {
    region
        .ask(move |world| guard::Blocks.player_block_action(world, &action))
        .await
        .unwrap();
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn block_rules_cancel_breaks_and_spare_blocks_from_explosions() {
    let dir = TestDir::new();
    install(dir.path(), "arena.wasm", &arena());
    let world = start(dir.path()).await;
    let region = world.region(RegionPos::new(0, 0)).await;
    let stone: BlockState = "minecraft:stone".parse().unwrap();
    let wool: BlockState = "minecraft:white_wool".parse().unwrap();
    let glass: BlockState = "minecraft:glass".parse().unwrap();
    let map = BlockPos::new(1, 100, 1);
    let built = BlockPos::new(3, 100, 3);
    let pane = BlockPos::new(3, 101, 3);
    let outside = BlockPos::new(12, 100, 12);
    let (player, mut events) = crate::world::messages::PlayerHandle::new(1000);
    subscribe(
        &region,
        &player,
        &mut events,
        &[tuff_world_model::pos::ChunkPos::new(0, 0)],
    )
    .await;
    place_blocks(
        &world,
        &region,
        [(map, stone), (built, wool), (pane, glass), (outside, stone)],
    )
    .await;
    assert!(allows(&region, action(map, stone, BlockAction::Broke)).await);

    let steve = || Player::new(77, "Steve");
    let added = host(&world).command(steve(), "rule".into()).await.unwrap();
    assert_eq!(added.result, Ok(1), "the first rule's id");
    placed(&region, action(built, wool, BlockAction::Placed)).await;
    placed(&region, action(pane, glass, BlockAction::Placed)).await;

    assert!(!allows(&region, action(map, stone, BlockAction::Broke)).await);
    assert!(allows(&region, action(built, wool, BlockAction::Broke)).await);
    assert!(allows(&region, action(outside, stone, BlockAction::Broke)).await);
    assert!(allows(&region, action(map, stone, BlockAction::Placed)).await);
    let bed: BlockState = "minecraft:red_bed".parse().unwrap();
    assert!(allows(&region, action(map, bed, BlockAction::Broke)).await);

    let smash = |pos| RegionRequest::Break {
        player: player.clone(),
        pos,
        tool: None,
        mode: BreakMode::Creative,
        sequence: 1,
    };
    region.request(smash(map)).await;
    assert_eq!(block_at(&region, map).await, Some(stone), "the map stays");

    region
        .request(RegionRequest::Run(RegionTask::new(|world| {
            world.explode(crate::world::explosion::Explosion {
                dangerous_wither_skull: false,
                spares_nether_portals: false,
                center: [3.5, 100.5, 3.5],
                radius: 3.0,
                fire: false,
                blocks: crate::world::explosion::BlockInteraction::Destroy,
                damage_type: &damage_type::EXPLOSION,
                igniter: None,
                culprit: None,
            });
        })))
        .await;
    world.ticks(2).await;
    assert_eq!(block_at(&region, map).await, Some(stone));
    assert_eq!(block_at(&region, pane).await, Some(glass), "blast-proof");
    assert_eq!(
        block_at(&region, built).await.map(|s| s.block()),
        Some(tuff_vanilla_data::Block::Air),
        "a block placed after the rule explodes"
    );

    console(&world, "tuff plugin unload arena").await;
    assert!(allows(&region, action(map, stone, BlockAction::Broke)).await);
    region.request(smash(map)).await;
    assert_eq!(
        block_at(&region, map).await.map(|s| s.block()),
        Some(tuff_vanilla_data::Block::Air)
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelling_rules_stop_whole_explosions_centred_inside() {
    let dir = TestDir::new();
    install(dir.path(), "arena.wasm", &arena_with(true, true));
    let world = start(dir.path()).await;
    let region = world.region(RegionPos::new(0, 0)).await;
    let wool: BlockState = "minecraft:white_wool".parse().unwrap();
    let inside = BlockPos::new(3, 100, 3);
    let outside = BlockPos::new(12, 100, 12);
    let (player, mut events) = crate::world::messages::PlayerHandle::new(1000);
    subscribe(
        &region,
        &player,
        &mut events,
        &[tuff_world_model::pos::ChunkPos::new(0, 0)],
    )
    .await;
    place_blocks(&world, &region, [(inside, wool), (outside, wool)]).await;
    host(&world)
        .command(Player::new(77, "Steve"), "rule".into())
        .await
        .unwrap();
    placed(&region, action(inside, wool, BlockAction::Placed)).await;

    let blast = |center: [f64; 3]| {
        RegionRequest::Run(RegionTask::new(move |world| {
            world.explode(crate::world::explosion::Explosion {
                dangerous_wither_skull: false,
                spares_nether_portals: false,
                center,
                radius: 3.0,
                fire: false,
                blocks: crate::world::explosion::BlockInteraction::Destroy,
                damage_type: &damage_type::EXPLOSION,
                igniter: None,
                culprit: None,
            });
        }))
    };
    region.request(blast([3.5, 100.5, 3.5])).await;
    world.ticks(2).await;
    assert_eq!(
        block_at(&region, inside).await,
        Some(wool),
        "even a placed block survives a cancelled blast"
    );

    region.request(blast([12.5, 100.5, 12.5])).await;
    world.ticks(2).await;
    assert_eq!(
        block_at(&region, outside).await.map(|s| s.block()),
        Some(tuff_vanilla_data::Block::Air),
        "blasts outside the box still go off"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn block_rules_refuse_bucket_pours_and_relight_with_a_resync() {
    use crate::world::messages::{ActionOutcome, BucketUse, PlayerEvent};
    use tuff_vanilla_data::{Block, Item};

    let dir = TestDir::new();
    install(dir.path(), "arena.wasm", &arena_placing(false));
    let world = start(dir.path()).await;
    let region = world.region(RegionPos::new(0, 0)).await;
    let stone: BlockState = "minecraft:stone".parse().unwrap();
    let floor = BlockPos::new(1, 100, 1);
    let pour_at = BlockPos::new(1, 101, 1);
    let (player, mut events) = crate::world::messages::PlayerHandle::new(1000);
    subscribe(
        &region,
        &player,
        &mut events,
        &[tuff_world_model::pos::ChunkPos::new(0, 0)],
    )
    .await;
    place_blocks(&world, &region, [(floor, stone)]).await;
    let host = host(&world);
    host.command(Player::new(77, "Steve"), "rule".into())
        .await
        .unwrap();

    let eye = [1.5, 103.0, 1.5];
    let pour = |sequence| {
        RegionRequest::UseBucket(BucketUse {
            player: player.clone(),
            position: [eye[0], eye[1] - 1.62, eye[2]],
            eye,
            look: crate::world::raycast::view_vector(0.0, 90.0),
            reach: 4.5,
            off_hand: false,
            item: Item::WaterBucket,
            fluid: Some(Block::Water),
            mob: None,
            mob_data: None,
            mob_components: None,
            sneaking: false,
            creative: false,
            sequence,
            hit: None,
        })
    };
    region.request(pour(1)).await;
    let denied = expect(&mut events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: 1,
            outcome,
        } => Some(outcome),
        _ => None,
    })
    .await;
    assert!(matches!(denied, ActionOutcome::Rejected(_)), "{denied:?}");
    assert_eq!(
        block_at(&region, pour_at).await.map(|s| s.block()),
        Some(Block::Air)
    );

    console(&world, "tuff plugin unload arena").await;
    region.request(pour(2)).await;
    let allowed = expect(&mut events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: 2,
            outcome,
        } => Some(outcome),
        _ => None,
    })
    .await;
    assert!(matches!(allowed, ActionOutcome::Used { .. }), "{allowed:?}");
    assert_eq!(
        block_at(&region, pour_at).await.map(|s| s.block()),
        Some(Block::Water)
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn egg_throws_in_the_air_are_cancelled_and_reported() {
    use crate::world::inventory::Inventory;
    use tuff_item::item::{ItemStack, MintReason};
    use tuff_vanilla_data::Item;

    let dir = TestDir::new();
    install(dir.path(), "arena.wasm", &arena());
    let world = start(dir.path()).await;
    let mut client = Client::new(&world);
    let slot = Inventory::hotbar_slot(client.session.inventory.selected());
    let throw = |off_hand| Serverbound::UseItem {
        off_hand,
        sequence: 9,
    };
    *client.session.inventory.slot_mut(slot) =
        Some(ItemStack::mint(Item::Egg, 4, MintReason::Test));
    assert_eq!(
        guard::Uses
            .intercept(&mut client.session, &throw(false))
            .await,
        Verdict::Cancel
    );
    assert_eq!(announced(&world).await, ["minecraft:egg"]);
    assert_eq!(
        guard::Uses
            .intercept(&mut client.session, &throw(true))
            .await,
        Verdict::Allow,
        "only the main hand"
    );
    *client.session.inventory.slot_mut(slot) =
        Some(ItemStack::mint(Item::Snowball, 4, MintReason::Test));
    assert_eq!(
        guard::Uses
            .intercept(&mut client.session, &throw(false))
            .await,
        Verdict::Allow
    );
    assert_eq!(announced(&world).await.len(), 1);

    console(&world, "tuff plugin unload arena").await;
    *client.session.inventory.slot_mut(slot) =
        Some(ItemStack::mint(Item::Egg, 4, MintReason::Test));
    assert_eq!(
        guard::Uses
            .intercept(&mut client.session, &throw(false))
            .await,
        Verdict::Allow
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn projectile_hits_report_their_owner_and_what_they_struck() {
    let dir = TestDir::new();
    install(dir.path(), "arena.wasm", &arena());
    let world = start(dir.path()).await;
    let region = world.region(RegionPos::new(0, 0)).await;
    let landing = |block: Option<BlockPos>| ProjectileLanding {
        id: 4000,
        uuid: 5,
        kind: EntityType::Egg,
        position: [2.5, 101.0, 2.5],
        owner: Some(Culprit::player(1, 77, "Steve")),
        block,
        target: None,
    };
    for block in [Some(BlockPos::new(2, 100, 2)), None] {
        let landing = landing(block);
        region
            .ask(move |world| guard::Blocks.projectile_landed(world, &landing))
            .await
            .unwrap();
    }
    assert_eq!(
        announced(&world).await,
        [
            "minecraft:egg 77 100 -1 ".to_owned(),
            "minecraft:egg 77 -1 -1 ".to_owned()
        ]
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn thrown_snowballs_land_on_blocks_where_plugins_hear_them() {
    let dir = TestDir::new();
    install(dir.path(), "arena.wasm", &arena());
    let world = start(dir.path()).await;
    let region = world.region(RegionPos::new(0, 0)).await;
    let floor = BlockPos::new(5, 100, 5);
    let (player, mut events) = crate::world::messages::PlayerHandle::new(1000);
    subscribe(
        &region,
        &player,
        &mut events,
        &[tuff_world_model::pos::ChunkPos::new(0, 0)],
    )
    .await;
    place_blocks(
        &world,
        &region,
        [(floor, "minecraft:stone".parse().unwrap())],
    )
    .await;
    let (reply, summoned) = tokio::sync::oneshot::channel();
    let motion = tuff_nbt::Tag::List(vec![
        tuff_nbt::Tag::Double(0.0),
        tuff_nbt::Tag::Double(-0.5),
        tuff_nbt::Tag::Double(0.0),
    ]);
    region
        .request(RegionRequest::Summon {
            nbt: tuff_nbt::Compound::new()
                .with("id", "minecraft:snowball")
                .with("Motion", motion),
            position: [5.5, 103.0, 5.5],
            fresh: false,
            reply,
        })
        .await;
    assert!(summoned.await.unwrap());
    let mut heard = Vec::new();
    for _ in 0..40 {
        world.ticks(1).await;
        heard = announced(&world).await;
        if !heard.is_empty() {
            break;
        }
    }
    assert_eq!(heard, ["minecraft:snowball -1 100 -1 ".to_owned()]);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn teammates_cannot_hurt_each_other_and_kills_are_credited() {
    let dir = TestDir::new();
    install(dir.path(), "arena.wasm", &arena());
    let world = start(dir.path()).await;
    let mut client = Client::new(&world);
    let by = |uuid: u128| DamageSource {
        kind: &damage_type::PLAYER_ATTACK,
        attacker: Some(Culprit::player(5, uuid, "Alex")),
        projectile: None,
        position: None,
        weapon: None,
        direct: true,
    };
    assert!(guard::Sessions.allows_player_hurt(&client.session, &by(88)));
    let teamed = host(&world)
        .command(Player::new(77, "Steve"), "squad".into())
        .await
        .unwrap();
    assert_eq!(teamed.result, Ok(0));
    host(&world).sync().await;
    assert!(!guard::Sessions.allows_player_hurt(&client.session, &by(88)));
    assert!(guard::Sessions.allows_player_hurt(&client.session, &by(99)));
    assert!(
        guard::Sessions.allows_player_hurt(&client.session, &by(77)),
        "a player may still hurt themself"
    );

    let result = HurtResult {
        landed: true,
        health_lost: 2.0,
        ..HurtResult::default()
    };
    let hit = HurtBy {
        kind: &damage_type::PLAYER_ATTACK,
        attacker: Some(Culprit::player(5, 99, "Alex")),
        projectile: None,
        direct: true,
    };
    let void = HurtBy {
        kind: &damage_type::OUT_OF_WORLD,
        attacker: None,
        projectile: None,
        direct: false,
    };
    client.session.tick = 1000;
    guard::Sessions.player_damaged(&mut client.session, &hit, &result, false);
    client.session.tick = 1080;
    guard::Sessions.player_damaged(&mut client.session, &void, &result, true);
    client.session.tick = 1300;
    guard::Sessions.player_damaged(&mut client.session, &void, &result, true);
    assert_eq!(
        announced(&world).await,
        [
            "minecraft:player_attack 99 99 1".to_owned(),
            "minecraft:out_of_world -1 99 1".to_owned(),
            "minecraft:out_of_world -1 -1 0".to_owned(),
        ]
    );

    console(&world, "tuff plugin unload arena").await;
    assert!(guard::Sessions.allows_player_hurt(&client.session, &by(88)));
    world.shutdown().await;
}

/// Adds a change rule with `/freeze`; `allow` sets fire-spread, fire-burn,
/// liquid-flow and entity-grief in that order.
fn freezer(allow: [bool; 4]) -> Vec<u8> {
    let mut data = Data::new();
    let register = register(&mut data, &["freeze"], "");
    let rule = ChangeRule {
        dimension: "overworld".into(),
        min: WitPos { x: 0, y: 100, z: 0 },
        max: WitPos { x: 9, y: 110, z: 9 },
        fire_spread: allow[0],
        fire_burn: allow[1],
        liquid_flow: allow[2],
        entity_grief: allow[3],
    };
    let add = data.call("add-change-rule", &[rule.to_value()]);
    let body = format!(
        r#"{register}
        (func {run} (param $handler i32) (param i32 i64 i64 i32 i32 i32 i32 i32 i32) (result i32)
          {add}
          (if (i32.load8_u (i32.const {RET})) (then (return {failed})))
          (return {id}))
        {data}"#,
        run = export("run-command").replace(
            " (param i32 i32 i64 i64 i32 i32 i32 i32 i32 i32) (result i32)",
            ""
        ),
        failed = ok_code("(i32.const -1)"),
        id = ok_code(&format!("(i32.load (i32.const {}))", RET + 4)),
        data = data.wat(),
    );
    module(&plain("freezer"), &body)
}

async fn caused_change(
    region: &crate::world::messages::RegionHandle,
    cause: crate::world::region_api::BlockCause,
    pos: BlockPos,
    state: BlockState,
) {
    region
        .ask(move |world| world.caused(cause, |world| world.set_block_flags(pos, state, 3)))
        .await
        .unwrap();
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn change_rules_stop_fire_liquids_and_entities_inside_the_box() {
    use crate::world::region_api::BlockCause;
    use tuff_vanilla_data::Block;
    let dir = TestDir::new();
    install(dir.path(), "freezer.wasm", &freezer([false; 4]));
    let world = start(dir.path()).await;
    let region = world.region(RegionPos::new(0, 0)).await;
    let state = |name: &str| -> BlockState { format!("minecraft:{name}").parse().unwrap() };
    let planks = state("oak_planks");
    let air = state("air");
    let inside = BlockPos::new(2, 100, 2);
    let outside = BlockPos::new(12, 100, 12);
    let (player, mut events) = crate::world::messages::PlayerHandle::new(1000);
    subscribe(
        &region,
        &player,
        &mut events,
        &[tuff_world_model::pos::ChunkPos::new(0, 0)],
    )
    .await;
    place_blocks(&world, &region, [(inside, planks), (outside, planks)]).await;
    let fire = BlockCause::Block(Block::Fire);
    let water = BlockCause::Block(Block::Water);
    let enderman = BlockCause::Entity {
        kind: EntityType::Enderman,
        uuid: 5,
    };

    let added = host(&world)
        .command(Player::new(77, "Steve"), "freeze".into())
        .await
        .unwrap();
    assert_eq!(added.result, Ok(1), "the first rule's id");

    caused_change(&region, fire, inside, air).await;
    assert_eq!(block_at(&region, inside).await, Some(planks), "fire burn");
    caused_change(&region, enderman, inside, air).await;
    assert_eq!(
        block_at(&region, inside).await,
        Some(planks),
        "entity grief"
    );
    caused_change(&region, water, inside, state("water")).await;
    assert_eq!(block_at(&region, inside).await, Some(planks), "liquid flow");
    caused_change(&region, BlockCause::World, inside, state("stone")).await;
    assert_eq!(
        block_at(&region, inside).await,
        Some(state("stone")),
        "world edits pass"
    );

    caused_change(&region, fire, outside, air).await;
    assert_eq!(
        block_at(&region, outside).await,
        Some(air),
        "outside the box"
    );

    console(&world, "tuff plugin unload freezer").await;
    caused_change(&region, enderman, inside, air).await;
    assert_eq!(
        block_at(&region, inside).await,
        Some(air),
        "rules go with the plugin"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn change_rule_flags_allow_each_kind_on_its_own() {
    use crate::world::region_api::BlockCause;
    use tuff_vanilla_data::Block;
    let dir = TestDir::new();
    install(
        dir.path(),
        "freezer.wasm",
        &freezer([false, true, true, true]),
    );
    let world = start(dir.path()).await;
    let region = world.region(RegionPos::new(0, 0)).await;
    let planks: BlockState = "minecraft:oak_planks".parse().unwrap();
    let air: BlockState = "minecraft:air".parse().unwrap();
    let fire: BlockState = "minecraft:fire".parse().unwrap();
    let pos = BlockPos::new(2, 100, 2);
    let next = BlockPos::new(3, 100, 2);
    let (player, mut events) = crate::world::messages::PlayerHandle::new(1000);
    subscribe(
        &region,
        &player,
        &mut events,
        &[tuff_world_model::pos::ChunkPos::new(0, 0)],
    )
    .await;
    place_blocks(&world, &region, [(pos, planks)]).await;
    host(&world)
        .command(Player::new(77, "Steve"), "freeze".into())
        .await
        .unwrap();
    let source = BlockCause::Block(Block::Fire);
    caused_change(&region, source, next, fire).await;
    assert_eq!(
        block_at(&region, next).await,
        Some(air),
        "fire cannot spread in"
    );
    caused_change(&region, source, pos, air).await;
    assert_eq!(
        block_at(&region, pos).await,
        Some(air),
        "burning is allowed"
    );
    world.shutdown().await;
}
