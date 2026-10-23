//! Villagers in regions: trading, levels, restocking, gossip and saving,
//! driven through the region's channels.

use super::*;
use crate::world::trading::TradeRequest;
use tuff_feature_builtin_vanilla_mobs::merchant_events::{Opened, Updated};
use tuff_vanilla_data::registry::{CustomStat, EntityType};

pub(super) const PLAYER_UUID: u128 = 77;

/// A farmer at level 1 with one offer: 20 wheat for an emerald, twice,
/// worth ten experience (enough for level 2). Without a job site it would
/// soon lose its profession, so trading tests turn its AI off.
pub(super) fn farmer() -> Compound {
    let offer = Compound::new()
        .with(
            "buy",
            Compound::new()
                .with("id", "minecraft:wheat")
                .with("count", 20),
        )
        .with(
            "sell",
            Compound::new()
                .with("id", "minecraft:emerald")
                .with("count", 1),
        )
        .with("maxUses", 2)
        .with("xp", 10)
        .with("priceMultiplier", 0.05f32);
    Compound::new()
        .with(
            "VillagerData",
            Compound::new()
                .with("type", "minecraft:plains")
                .with("profession", "minecraft:farmer")
                .with("level", 1),
        )
        .with("Offers", Compound::new().with("Recipes", vec![offer]))
}

async fn trade(
    region: &RegionHandle,
    player: &PlayerHandle,
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    villager: i32,
    trades: Vec<(usize, u32)>,
) -> bool {
    region
        .request(RegionRequest::Trade {
            player: player.clone(),
            window_id: 5,
            request: TradeRequest {
                merchant: villager,
                player: PLAYER_UUID,
                trades,
            },
        })
        .await;
    expect(events, |event| match event {
        PlayerEvent::TradeAnswered {
            window_id: 5,
            accepted,
        } => Some(accepted),
        _ => None,
    })
    .await
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn villagers_trade_gain_levels_and_new_offers() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(4, 11)).await;
    let position = [7.5, SKY as f64 + 1.0, 7.5];
    region
        .request(player_update(&viewer, [9.5, SKY as f64 + 1.0, 7.5], None))
        .await;
    summon(
        &region,
        EntityType::Villager,
        farmer().with("NoAI", true),
        position,
    )
    .await;
    world.ticks(2).await;
    let villager = mobs(&region, EntityType::Villager).await[0].id;

    region
        .request(RegionRequest::InteractEntity {
            player: viewer.clone(),
            target: villager,
            interaction: interaction(&viewer, None),
        })
        .await;
    let stat = expect(&mut events, |event| match event {
        PlayerEvent::AwardStat(stat, amount) => Some((stat, amount)),
        _ => None,
    })
    .await;
    assert_eq!(stat, (CustomStat::TalkedToVillager, 1));
    let view = expect(&mut events, |event| match event {
        PlayerEvent::Run(task) => task.data::<Opened>().map(|opened| opened.view.clone()),
        _ => None,
    })
    .await;
    assert_eq!(view.merchant, villager);
    assert_eq!((view.level, view.xp, view.offers.len()), (1, 0, 1));
    assert!(view.regular && view.can_restock);

    assert!(
        !trade(&region, &viewer, &mut events, villager, vec![(0, 3)]).await,
        "only two uses"
    );
    assert!(
        !trade(&region, &viewer, &mut events, villager, vec![(4, 1)]).await,
        "no such offer"
    );
    assert!(trade(&region, &viewer, &mut events, villager, vec![(0, 1)]).await);
    let update = expect(&mut events, |event| match event {
        PlayerEvent::Run(task) => task.data::<Updated>().map(|update| update.0.clone()),
        _ => None,
    })
    .await;
    assert_eq!(update.level, 2, "ten experience reaches level 2");
    assert_eq!(update.xp, 10);
    assert_eq!(update.offers[0].uses, 1);
    assert!(update.offers.len() > 1, "level 2 brings new offers");
    let orbs = wait_for(&world, &region, 40, |found| {
        found.iter().any(|s| s.kind == EntityType::ExperienceOrb)
    })
    .await;
    assert!(orbs.iter().any(|s| s.kind == EntityType::ExperienceOrb));
    world.ticks(2).await;
    let heard = expect(&mut events, |event| match event {
        PlayerEvent::Run(task) => task.data::<Updated>().map(|update| update.0.clone()),
        _ => None,
    })
    .await;
    assert_eq!(
        heard.offers[0].uses, 1,
        "the trade's gossip resends the offers"
    );
    assert!(!trade(&region, &viewer, &mut events, villager, vec![(0, 2)]).await);
    region
        .request(RegionRequest::StopTrading {
            merchant: villager,
            player: PLAYER_UUID,
        })
        .await;
    assert!(
        !trade(&region, &viewer, &mut events, villager, vec![(0, 1)]).await,
        "the screen is closed"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn babies_and_jobless_villagers_do_not_trade() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(4, 11)).await;
    summon(
        &region,
        EntityType::Villager,
        farmer().with("Age", -24000),
        [6.5, SKY as f64 + 1.0, 7.5],
    )
    .await;
    summon(
        &region,
        EntityType::Villager,
        Compound::new(),
        [9.5, SKY as f64 + 1.0, 7.5],
    )
    .await;
    world.ticks(2).await;
    let villagers = mobs(&region, EntityType::Villager).await;
    for villager in &villagers {
        region
            .request(RegionRequest::InteractEntity {
                player: viewer.clone(),
                target: villager.id,
                interaction: interaction(&viewer, Some(Item::Stick)),
            })
            .await;
        expect(&mut events, |event| match event {
            PlayerEvent::EntityInteracted { .. } => Some(()),
            PlayerEvent::Run(task) if task.data::<Opened>().is_some() => {
                panic!("a baby or jobless villager traded")
            }
            _ => None,
        })
        .await;
    }
    world.shutdown().await;
}

mod screen {
    //! The merchant screen on a real session, trading with a real villager.

    use super::session_with_outbound;
    use super::*;
    use crate::protocol::play::Serverbound;
    use crate::world::inventory::Inventory;
    use crate::world::menu::merchant;
    use crate::world::stats::Stat;

    fn click(window_id: i32, slot: i16, mode: i32) -> Serverbound {
        Serverbound::ContainerClick {
            window_id,
            state_id: 0,
            slot,
            button: 0,
            mode,
            changed: Vec::new(),
            carried: None,
        }
    }

    fn sent(outbound: &mut mpsc::UnboundedReceiver<bytes::Bytes>, id: i32) -> bool {
        let mut found = false;
        while let Ok(frame) = outbound.try_recv() {
            let mut r = framed_payload(&frame);
            found |= r.read_var_int().unwrap() == id;
        }
        found
    }

    #[tuff_core::watchdog]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn players_trade_through_the_merchant_screen() {
        use tuff_vanilla_data::packet::play::Clientbound;

        let dir = TestDir::new();
        let world = TestWorld::start(dir.path());
        let chunk = ChunkPos::new(0, 0);
        let region = world.region(chunk.region()).await;
        let (viewer, mut events) = PlayerHandle::new(999);
        subscribe(&region, &viewer, &mut events, &[chunk]).await;
        place_blocks(&world, &region, pen(4, 11)).await;
        summon(
            &region,
            EntityType::Villager,
            farmer().with("NoAI", true),
            [7.5, SKY as f64 + 1.0, 7.5],
        )
        .await;
        world.ticks(1).await;
        let track = expect(&mut events, super::track_of(EntityType::Villager)).await;
        let villager = track.entity;

        let (mut player, mut outbound) = session_with_outbound(&world, PLAYER_UUID as i32);
        player.stand_at([9.5, SKY as f64 + 1.0, 7.5], true);
        player.see_entity(track);
        player.inventory_mut().set_slot(
            Inventory::hotbar_slot(0),
            Some(ItemStack::mint(Item::Wheat, 40, MintReason::Test)),
        );
        player
            .handle_packet(Serverbound::Interact {
                entity: villager,
                off_hand: false,
                location: [0.0; 3],
                sneaking: false,
            })
            .await;
        assert!(
            player
                .handle_events_until(
                    |e| matches!(e, PlayerEvent::Run(task) if task.data::<Opened>().is_some())
                )
                .await
        );
        assert!(sent(&mut outbound, Clientbound::MerchantOffers.id()));
        assert_eq!(player.stat(Stat::custom(CustomStat::TalkedToVillager)), 1);
        let window = player.window_id().expect("the screen is open");

        player.handle_packet(Serverbound::SelectTrade(0)).await;
        let slots = player
            .menu_container()
            .unwrap()
            .iter()
            .map(|s| s.as_ref().map(tuff_item::item::ItemStack::test_copy))
            .collect::<Vec<_>>();
        assert_eq!(
            slots[merchant::PAYMENT_A],
            Some(ItemStack::mint(Item::Wheat, 40, MintReason::Test))
        );
        assert_eq!(
            slots[merchant::RESULT],
            Some(ItemStack::mint(Item::Emerald, 1, MintReason::Test))
        );
        assert_eq!(player.inventory().hand(false), None, "the wheat moved over");

        player
            .handle_packet(click(window, merchant::RESULT as i16, 0))
            .await;
        assert!(
            player
                .handle_events_until(|e| matches!(
                    e,
                    PlayerEvent::TradeAnswered { accepted: true, .. }
                ))
                .await
        );
        assert_eq!(
            player.inventory().carried(),
            Some(&ItemStack::mint(Item::Emerald, 1, MintReason::Test)),
            "the trade settles once the merchant agrees"
        );
        let slots = player
            .menu_container()
            .unwrap()
            .iter()
            .map(|s| s.as_ref().map(tuff_item::item::ItemStack::test_copy))
            .collect::<Vec<_>>();
        assert_eq!(
            slots[merchant::PAYMENT_A],
            Some(ItemStack::mint(Item::Wheat, 20, MintReason::Test))
        );
        assert_eq!(
            slots[merchant::RESULT],
            Some(ItemStack::mint(Item::Emerald, 1, MintReason::Test))
        );
        assert_eq!(player.stat(Stat::custom(CustomStat::TradedWithVillager)), 1);
        assert_eq!(
            player.stat(Stat::item(
                tuff_vanilla_data::registry::StatType::Crafted,
                Item::Emerald
            )),
            1
        );

        player
            .handle_packet(click(window, merchant::RESULT as i16, 1))
            .await;
        assert!(
            player
                .handle_events_until(|e| matches!(
                    e,
                    PlayerEvent::TradeAnswered { accepted: true, .. }
                ))
                .await
        );
        let slots = player
            .menu_container()
            .unwrap()
            .iter()
            .map(|s| s.as_ref().map(tuff_item::item::ItemStack::test_copy))
            .collect::<Vec<_>>();
        assert_eq!(slots[merchant::PAYMENT_A], None, "all the wheat is spent");
        assert_eq!(slots[merchant::RESULT], None);
        assert_eq!(player.stat(Stat::custom(CustomStat::TradedWithVillager)), 2);

        player
            .handle_packet(Serverbound::ContainerClose(window))
            .await;
        assert_eq!(player.window_id(), None);
        let emeralds: u32 = player
            .inventory()
            .slots()
            .iter()
            .flatten()
            .filter(|s| s.item() == Item::Emerald)
            .map(|s| s.count() as u32)
            .sum();
        assert_eq!(emeralds, 2);
        world.shutdown().await;
    }

    #[tuff_core::watchdog]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn refused_trades_give_the_payment_back() {
        let dir = TestDir::new();
        let world = TestWorld::start(dir.path());
        let chunk = ChunkPos::new(0, 0);
        let region = world.region(chunk.region()).await;
        let (viewer, mut events) = PlayerHandle::new(999);
        subscribe(&region, &viewer, &mut events, &[chunk]).await;
        place_blocks(&world, &region, pen(4, 11)).await;
        summon(
            &region,
            EntityType::Villager,
            farmer().with("NoAI", true),
            [7.5, SKY as f64 + 1.0, 7.5],
        )
        .await;
        world.ticks(1).await;
        let track = expect(&mut events, super::track_of(EntityType::Villager)).await;
        let villager = track.entity;
        let (mut player, _outbound) = session_with_outbound(&world, PLAYER_UUID as i32);
        player.stand_at([9.5, SKY as f64 + 1.0, 7.5], true);
        player.see_entity(track);
        player.inventory_mut().set_slot(
            Inventory::hotbar_slot(0),
            Some(ItemStack::mint(Item::Wheat, 20, MintReason::Test)),
        );
        player
            .handle_packet(Serverbound::Interact {
                entity: villager,
                off_hand: false,
                location: [0.0; 3],
                sneaking: false,
            })
            .await;
        assert!(
            player
                .handle_events_until(
                    |e| matches!(e, PlayerEvent::Run(task) if task.data::<Opened>().is_some())
                )
                .await
        );
        let window = player.window_id().unwrap();
        player.handle_packet(Serverbound::SelectTrade(0)).await;
        region
            .request(RegionRequest::StopTrading {
                merchant: villager,
                player: PLAYER_UUID,
            })
            .await;
        player
            .handle_packet(click(window, merchant::RESULT as i16, 0))
            .await;
        assert!(
            player
                .handle_events_until(|e| matches!(
                    e,
                    PlayerEvent::TradeAnswered {
                        accepted: false,
                        ..
                    }
                ))
                .await
        );
        assert_eq!(player.inventory().carried(), None);
        let slots = player
            .menu_container()
            .unwrap()
            .iter()
            .map(|s| s.as_ref().map(tuff_item::item::ItemStack::test_copy))
            .collect::<Vec<_>>();
        assert_eq!(
            slots[merchant::PAYMENT_A],
            Some(ItemStack::mint(Item::Wheat, 20, MintReason::Test))
        );
        assert_eq!(
            slots[merchant::RESULT],
            Some(ItemStack::mint(Item::Emerald, 1, MintReason::Test))
        );
        world.shutdown().await;
    }
}

mod behaviour {
    //! What villagers do on their own: jobs, beds, fleeing, doors and
    //! babies.

    use super::wait_for;
    use super::*;

    async fn data(region: &RegionHandle, id: i32) -> Compound {
        let (reply, answer) = oneshot::channel();
        region
            .request(RegionRequest::EntityData { id, reply })
            .await;
        answer.await.unwrap().expect("the villager is there")
    }

    fn profession(nbt: &Compound) -> String {
        nbt.get_compound("VillagerData")
            .and_then(|d| d.get_str("profession"))
            .unwrap_or_default()
            .to_owned()
    }

    fn memory(nbt: &Compound, key: &str) -> Option<BlockPos> {
        let value = nbt
            .get_compound("Brain")?
            .get_compound("memories")?
            .get_compound(key)?
            .get_compound("value")?;
        let [x, y, z] = value.get_int_array("pos")? else {
            return None;
        };
        Some(BlockPos::new(*x, *y, *z))
    }

    fn set_time(world: &TestWorld, ticks: i64) {
        let overworld = crate::world::clock::clock_id("minecraft:overworld").unwrap();
        world
            .clocks
            .send_modify(|clocks| clocks.get_mut(overworld).set(ticks));
    }

    fn jobless() -> Compound {
        Compound::new().with(
            "VillagerData",
            Compound::new()
                .with("type", "minecraft:plains")
                .with("profession", "minecraft:none")
                .with("level", 1),
        )
    }

    /// Ticks until `check` holds for the villager's saved data.
    async fn wait_for_data(
        world: &TestWorld,
        region: &RegionHandle,
        id: i32,
        ticks: u64,
        mut check: impl FnMut(&Compound) -> bool,
    ) -> Compound {
        for _ in 0..ticks {
            let nbt = data(region, id).await;
            if check(&nbt) {
                return nbt;
            }
            world.ticks(1).await;
        }
        panic!(
            "never happened; the villager is {:#?}",
            data(region, id).await
        );
    }

    #[tuff_core::watchdog]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn jobless_villagers_take_a_workstation_and_lose_it_with_the_block() {
        let dir = TestDir::new();
        let world = TestWorld::start(dir.path());
        let chunk = ChunkPos::new(0, 0);
        let region = world.region(chunk.region()).await;
        let (viewer, mut events) = PlayerHandle::new(1000);
        subscribe(&region, &viewer, &mut events, &[chunk]).await;
        set_time(&world, 3000);
        let composter = BlockPos::new(11, SKY + 1, 7);
        let mut blocks = pen(4, 11);
        blocks.push((composter, Block::Composter.default_state()));
        place_blocks(&world, &region, blocks).await;
        summon(
            &region,
            EntityType::Villager,
            jobless(),
            [5.5, SKY as f64 + 1.0, 7.5],
        )
        .await;
        world.ticks(1).await;
        let id = super::mobs(&region, EntityType::Villager).await[0].id;
        let farmer = wait_for_data(&world, &region, id, 600, |nbt| {
            profession(nbt) == "minecraft:farmer"
        })
        .await;
        assert_eq!(memory(&farmer, "minecraft:job_site"), Some(composter));
        assert!(
            farmer.get_compound("Offers").is_none(),
            "offers come when first needed"
        );

        place_blocks(&world, &region, [(composter, Block::Air.default_state())]).await;
        let fired = wait_for_data(&world, &region, id, 100, |nbt| {
            profession(nbt) == "minecraft:none"
        })
        .await;
        assert_eq!(memory(&fired, "minecraft:job_site"), None);
        world.shutdown().await;
    }

    #[tuff_core::watchdog]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn villagers_claim_a_bed_and_sleep_in_it_at_night() {
        let dir = TestDir::new();
        let world = TestWorld::start(dir.path());
        let chunk = ChunkPos::new(0, 0);
        let region = world.region(chunk.region()).await;
        let (viewer, mut events) = PlayerHandle::new(1000);
        subscribe(&region, &viewer, &mut events, &[chunk]).await;
        set_time(&world, 13000);
        let head = BlockPos::new(10, SKY + 1, 7);
        let foot = BlockPos::new(10, SKY + 1, 8);
        let bed = Block::RedBed
            .default_state()
            .with("facing", "north")
            .unwrap();
        let mut blocks = pen(4, 11);
        blocks.push((head, bed.with("part", "head").unwrap()));
        blocks.push((foot, bed.with("part", "foot").unwrap()));
        place_blocks(&world, &region, blocks).await;
        summon(
            &region,
            EntityType::Villager,
            jobless(),
            [5.5, SKY as f64 + 1.0, 5.5],
        )
        .await;
        world.ticks(1).await;
        let id = super::mobs(&region, EntityType::Villager).await[0].id;
        let asleep = wait_for_data(&world, &region, id, 800, |nbt| {
            memory(nbt, "minecraft:last_slept").is_some()
                || nbt
                    .get_compound("Brain")
                    .and_then(|b| b.get_compound("memories"))
                    .is_some_and(|m| m.contains_key("minecraft:last_slept"))
        })
        .await;
        assert_eq!(memory(&asleep, "minecraft:home"), Some(head));
        let occupied =
            |state: Option<tuff_vanilla_data::BlockState>| state.and_then(|s| s.occupied());
        assert_eq!(
            occupied(block_at(&region, head).await),
            Some(true),
            "the bed is taken"
        );
        let position = super::mobs(&region, EntityType::Villager).await[0].position;
        assert!(
            (position[0] - 10.5).abs() < 0.01 && (position[2] - 7.5).abs() < 0.01,
            "lies in the bed: {position:?}"
        );

        set_time(&world, 3000);
        wait_for_data(&world, &region, id, 40, |nbt| {
            nbt.get_compound("Brain")
                .and_then(|b| b.get_compound("memories"))
                .is_some_and(|m| m.contains_key("minecraft:last_woken"))
        })
        .await;
        world.ticks(1).await;
        assert_eq!(occupied(block_at(&region, head).await), Some(false));
        world.shutdown().await;
    }

    /// Right-clicking a bed a villager sleeps in wakes and ejects it
    /// instead of showing "this bed is occupied" (that message is for a
    /// bed a player is actually asleep in).
    #[tuff_core::watchdog]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn right_clicking_a_villagers_bed_wakes_it_instead_of_the_occupied_message() {
        let dir = TestDir::new();
        let world = TestWorld::start(dir.path());
        let chunk = ChunkPos::new(0, 0);
        let region = world.region(chunk.region()).await;
        let (viewer, mut events) = PlayerHandle::new(1000);
        subscribe(&region, &viewer, &mut events, &[chunk]).await;
        set_time(&world, 13000);
        let head = BlockPos::new(10, SKY + 1, 7);
        let foot = BlockPos::new(10, SKY + 1, 8);
        let bed = Block::RedBed
            .default_state()
            .with("facing", "north")
            .unwrap();
        let mut blocks = pen(4, 11);
        blocks.push((head, bed.with("part", "head").unwrap()));
        blocks.push((foot, bed.with("part", "foot").unwrap()));
        place_blocks(&world, &region, blocks).await;
        summon(
            &region,
            EntityType::Villager,
            jobless(),
            [5.5, SKY as f64 + 1.0, 5.5],
        )
        .await;
        world.ticks(1).await;
        let id = super::mobs(&region, EntityType::Villager).await[0].id;
        wait_for_data(&world, &region, id, 800, |nbt| {
            memory(nbt, "minecraft:last_slept").is_some()
                || nbt
                    .get_compound("Brain")
                    .and_then(|b| b.get_compound("memories"))
                    .is_some_and(|m| m.contains_key("minecraft:last_slept"))
        })
        .await;
        let occupied =
            |state: Option<tuff_vanilla_data::BlockState>| state.and_then(|s| s.occupied());
        assert_eq!(occupied(block_at(&region, head).await), Some(true));

        let mut click = place(&viewer, head, Item::Stick, 1);
        click.item = None;
        click.held = None;
        let outcome = expect(&mut events, |event| match event {
            PlayerEvent::Overlay(message) => {
                assert!(
                    !message.plain().contains("occupied"),
                    "a mob occupant must not show the occupied message: {message:?}"
                );
                None
            }
            PlayerEvent::ActionResult {
                sequence: 1,
                outcome,
            } => Some(outcome),
            _ => None,
        });
        region.request(RegionRequest::Place(click)).await;
        assert!(matches!(outcome.await, ActionOutcome::Interacted { .. }));

        assert_eq!(
            occupied(block_at(&region, head).await),
            Some(false),
            "the villager is evicted so the bed frees up"
        );
        let woken = wait_for_data(&world, &region, id, 40, |nbt| {
            nbt.get_compound("Brain")
                .and_then(|b| b.get_compound("memories"))
                .is_some_and(|m| m.contains_key("minecraft:last_woken"))
        })
        .await;
        assert_eq!(profession(&woken), "minecraft:none");
        world.shutdown().await;
    }

    /// A villager saved asleep (its chunk unloaded at night) comes back
    /// asleep in that bed, so the bed it left marked occupied still has an
    /// occupant a right-click can wake.
    #[tuff_core::watchdog]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_villager_reloaded_asleep_keeps_its_bed_and_can_be_woken() {
        let dir = TestDir::new();
        let world = TestWorld::start(dir.path());
        let chunk = ChunkPos::new(0, 0);
        let region = world.region(chunk.region()).await;
        let (viewer, mut events) = PlayerHandle::new(1000);
        subscribe(&region, &viewer, &mut events, &[chunk]).await;
        set_time(&world, 13000);
        let head = BlockPos::new(10, SKY + 1, 7);
        let foot = BlockPos::new(10, SKY + 1, 8);
        let bed = Block::RedBed
            .default_state()
            .with("facing", "north")
            .unwrap()
            .with("occupied", "true")
            .unwrap();
        let mut blocks = pen(4, 11);
        blocks.push((head, bed.with("part", "head").unwrap()));
        blocks.push((foot, bed.with("part", "foot").unwrap()));
        place_blocks(&world, &region, blocks).await;
        summon(
            &region,
            EntityType::Villager,
            jobless()
                .with("SleepingX", head.x)
                .with("SleepingY", head.y)
                .with("SleepingZ", head.z),
            [10.5, SKY as f64 + 1.6875, 7.5],
        )
        .await;
        world.ticks(2).await;
        let id = super::mobs(&region, EntityType::Villager).await[0].id;
        let saved = data(&region, id).await;
        assert_eq!(
            (
                saved.get_int("SleepingX"),
                saved.get_int("SleepingY"),
                saved.get_int("SleepingZ")
            ),
            (Some(head.x), Some(head.y), Some(head.z)),
            "the sleeping position survives a save"
        );

        let mut click = place(&viewer, head, Item::Stick, 1);
        click.item = None;
        click.held = None;
        let outcome = expect(&mut events, |event| match event {
            PlayerEvent::Overlay(message) => {
                assert!(
                    !message.plain().contains("occupied"),
                    "the reloaded sleeper is woken, not reported: {message:?}"
                );
                None
            }
            PlayerEvent::ActionResult {
                sequence: 1,
                outcome,
            } => Some(outcome),
            _ => None,
        });
        region.request(RegionRequest::Place(click)).await;
        assert!(matches!(outcome.await, ActionOutcome::Interacted { .. }));
        assert_eq!(
            block_at(&region, head).await.and_then(|s| s.occupied()),
            Some(false)
        );
        world.shutdown().await;
    }

    /// A player asleep in a bed is left undisturbed by another player's
    /// right-click: still the "occupied" message, no eviction. The region
    /// keeps no per-bed record of which player sleeps where (only the
    /// block's occupied flag, flipped the same way a real session does it
    /// via `RegionRequest::OccupyBed`), so this also covers any occupant
    /// that is not a tracked mob.
    #[tuff_core::watchdog]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn right_clicking_a_players_bed_still_shows_the_occupied_message() {
        let dir = TestDir::new();
        let world = TestWorld::start(dir.path());
        let chunk = ChunkPos::new(0, 0);
        let region = world.region(chunk.region()).await;
        let (viewer, mut events) = PlayerHandle::new(1000);
        subscribe(&region, &viewer, &mut events, &[chunk]).await;
        let head = BlockPos::new(10, SKY + 1, 7);
        let foot = BlockPos::new(10, SKY + 1, 8);
        let bed = Block::RedBed
            .default_state()
            .with("facing", "north")
            .unwrap();
        place_blocks(
            &world,
            &region,
            vec![
                (head, bed.with("part", "head").unwrap()),
                (foot, bed.with("part", "foot").unwrap()),
            ],
        )
        .await;
        region.request(RegionRequest::OccupyBed { pos: head }).await;
        let occupied =
            |state: Option<tuff_vanilla_data::BlockState>| state.and_then(|s| s.occupied());
        assert_eq!(occupied(block_at(&region, head).await), Some(true));

        let mut click = place(&viewer, head, Item::Stick, 1);
        click.item = None;
        click.held = None;
        region.request(RegionRequest::Place(click)).await;
        let message = expect(&mut events, |event| match event {
            PlayerEvent::Overlay(message) => Some(message),
            _ => None,
        })
        .await;
        assert!(
            message.plain().contains("occupied"),
            "expected occupied, got {message:?}"
        );
        assert_eq!(
            occupied(block_at(&region, head).await),
            Some(true),
            "the sleeper is left undisturbed"
        );
        world.shutdown().await;
    }

    #[tuff_core::watchdog]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn villagers_flee_zombies() {
        let dir = TestDir::new();
        let world = TestWorld::start(dir.path());
        let chunk = ChunkPos::new(0, 0);
        let region = world.region(chunk.region()).await;
        let (viewer, mut events) = PlayerHandle::new(1000);
        subscribe(&region, &viewer, &mut events, &[chunk]).await;
        place_blocks(&world, &region, pen(1, 14)).await;
        let zombie = [3.5, SKY as f64 + 1.0, 3.5];
        summon(
            &region,
            EntityType::Zombie,
            Compound::new()
                .with("NoAI", true)
                .with("PersistenceRequired", true),
            zombie,
        )
        .await;
        summon(
            &region,
            EntityType::Villager,
            jobless(),
            [6.5, SKY as f64 + 1.0, 6.5],
        )
        .await;
        let far = wait_for(&world, &region, 400, |found| {
            found.iter().any(|s| {
                s.kind == EntityType::Villager
                    && crate::world::distance_sq(s.position, zombie) > 8.0 * 8.0
            })
        })
        .await;
        assert!(!far.is_empty());
        world.shutdown().await;
    }

    #[tuff_core::watchdog]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn villagers_ignore_zombies_behind_walls() {
        let dir = TestDir::new();
        let world = TestWorld::start(dir.path());
        let chunk = ChunkPos::new(0, 0);
        let region = world.region(chunk.region()).await;
        let (viewer, mut events) = PlayerHandle::new(1000);
        subscribe(&region, &viewer, &mut events, &[chunk]).await;
        set_time(&world, 3000);
        let mut blocks = pen(1, 14);
        let cell = (1..=9)
            .map(|z| (9, z))
            .chain((6..=8).map(|x| (x, 9)))
            .flat_map(|(x, z)| (1..=3).map(move |y| BlockPos::new(x, SKY + y, z)));
        let wall: Vec<BlockPos> = (1..=14)
            .flat_map(|z| (1..=3).map(move |y| BlockPos::new(5, SKY + y, z)))
            .chain(cell)
            .collect();
        blocks.extend(wall.iter().map(|p| (*p, Block::Stone.default_state())));
        place_blocks(&world, &region, blocks).await;
        let zombie = [3.5, SKY as f64 + 1.0, 3.5];
        summon(
            &region,
            EntityType::Zombie,
            Compound::new()
                .with("NoAI", true)
                .with("PersistenceRequired", true),
            zombie,
        )
        .await;
        summon(
            &region,
            EntityType::Villager,
            jobless(),
            [6.5, SKY as f64 + 1.0, 6.5],
        )
        .await;
        for _ in 0..200 {
            world.ticks(1).await;
            let found = mobs(&region, EntityType::Villager).await;
            assert!(
                found
                    .iter()
                    .all(|s| crate::world::distance_sq(s.position, zombie) < 8.0 * 8.0),
                "villager fled a zombie it cannot see"
            );
        }
        place_blocks(
            &world,
            &region,
            wall.iter().map(|p| (*p, Block::Air.default_state())),
        )
        .await;
        let far = wait_for(&world, &region, 400, |found| {
            found.iter().any(|s| {
                s.kind == EntityType::Villager
                    && crate::world::distance_sq(s.position, zombie) > 8.0 * 8.0
            })
        })
        .await;
        assert!(!far.is_empty());
        world.shutdown().await;
    }

    #[tuff_core::watchdog]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn villagers_walk_up_to_cats() {
        let dir = TestDir::new();
        let world = TestWorld::start(dir.path());
        let chunk = ChunkPos::new(0, 0);
        let region = world.region(chunk.region()).await;
        let (viewer, mut events) = PlayerHandle::new(1000);
        subscribe(&region, &viewer, &mut events, &[chunk]).await;
        set_time(&world, 3000);
        place_blocks(&world, &region, pen(1, 14)).await;
        let cat = [3.5, SKY as f64 + 1.0, 3.5];
        summon(
            &region,
            EntityType::Cat,
            Compound::new()
                .with("NoAI", true)
                .with("PersistenceRequired", true),
            cat,
        )
        .await;
        summon(
            &region,
            EntityType::Villager,
            jobless(),
            [6.5, SKY as f64 + 1.0, 6.5],
        )
        .await;
        let near = wait_for(&world, &region, 3000, |found| {
            found.iter().any(|s| {
                s.kind == EntityType::Villager
                    && crate::world::distance_sq(s.position, cat) < 3.0 * 3.0
            })
        })
        .await;
        assert!(!near.is_empty());
        world.shutdown().await;
    }

    #[tuff_core::watchdog]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn villagers_open_doors_on_their_way_and_shut_them_behind() {
        let dir = TestDir::new();
        let world = TestWorld::start(dir.path());
        let chunk = ChunkPos::new(0, 0);
        let region = world.region(chunk.region()).await;
        let (viewer, mut events) = PlayerHandle::new(1000);
        subscribe(&region, &viewer, &mut events, &[chunk]).await;
        set_time(&world, 3000);
        let door = BlockPos::new(8, SKY + 1, 7);
        let oak = Block::OakDoor
            .default_state()
            .with("facing", "east")
            .unwrap();
        let mut blocks = pen(2, 13);
        for z in 2..=13 {
            for y in 1..=3 {
                blocks.push((BlockPos::new(8, SKY + y, z), Block::Stone.default_state()));
            }
        }
        blocks.push((door, oak.with("half", "lower").unwrap()));
        blocks.push((
            BlockPos::new(8, SKY + 2, 7),
            oak.with("half", "upper").unwrap(),
        ));
        let composter = BlockPos::new(12, SKY + 1, 7);
        blocks.push((composter, Block::Composter.default_state()));
        place_blocks(&world, &region, blocks).await;
        summon(
            &region,
            EntityType::Villager,
            jobless(),
            [4.5, SKY as f64 + 1.0, 7.5],
        )
        .await;
        world.ticks(1).await;
        let id = super::mobs(&region, EntityType::Villager).await[0].id;
        let mut opened = false;
        for _ in 0..800 {
            let state = block_at(&region, door).await;
            opened |= state.and_then(|s| s.open()) == Some(true);
            let villager = super::mobs(&region, EntityType::Villager).await;
            let through = villager[0].position[0] > 9.5;
            if through && opened && state.and_then(|s| s.open()) == Some(false) {
                let nbt = data(&region, id).await;
                assert_eq!(profession(&nbt), "minecraft:farmer");
                world.shutdown().await;
                return;
            }
            world.ticks(1).await;
        }
        panic!(
            "never went through and shut the door; opened: {opened}, villager {:#?}",
            data(&region, id).await
        );
    }

    #[tuff_core::watchdog]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn fed_villagers_with_a_free_bed_have_a_baby() {
        let dir = TestDir::new();
        let world = TestWorld::start(dir.path());
        let chunk = ChunkPos::new(0, 0);
        let region = world.region(chunk.region()).await;
        let (viewer, mut events) = PlayerHandle::new(1000);
        subscribe(&region, &viewer, &mut events, &[chunk]).await;
        set_time(&world, 11500);
        let bed = Block::BlueBed
            .default_state()
            .with("facing", "north")
            .unwrap();
        let beds = [4, 6, 8];
        let mut blocks = pen(2, 13);
        for x in beds {
            blocks.push((
                BlockPos::new(x, SKY + 1, 12),
                bed.with("part", "head").unwrap(),
            ));
            blocks.push((
                BlockPos::new(x, SKY + 1, 13),
                bed.with("part", "foot").unwrap(),
            ));
        }
        place_blocks(&world, &region, blocks).await;
        let fed = || {
            jobless().with(
                "Inventory",
                vec![tuff_item::item::ItemPicture::new(Item::Bread, 3).to_nbt()],
            )
        };
        for x in [6.5, 8.5] {
            summon(
                &region,
                EntityType::Villager,
                fed(),
                [x, SKY as f64 + 1.0, 6.5],
            )
            .await;
        }
        let found = wait_for(&world, &region, 1200, |found| {
            found
                .iter()
                .filter(|s| s.kind == EntityType::Villager)
                .count()
                == 3
        })
        .await;
        let baby = found
            .iter()
            .filter(|s| s.kind == EntityType::Villager)
            .max_by_key(|s| s.id)
            .unwrap();
        let nbt = data(&region, baby.id).await;
        assert!(nbt.get_int("Age").unwrap() < 0, "a baby");
        let home = memory(&nbt, "minecraft:home").expect("the baby has a bed");
        assert!(beds.contains(&home.x) && home.z == 12);
        world.shutdown().await;
    }

    #[tuff_core::watchdog]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn villagers_pick_up_food_nearby() {
        let dir = TestDir::new();
        let world = TestWorld::start(dir.path());
        let chunk = ChunkPos::new(0, 0);
        let region = world.region(chunk.region()).await;
        let (viewer, mut events) = PlayerHandle::new(1000);
        subscribe(&region, &viewer, &mut events, &[chunk]).await;
        set_time(&world, 11500);
        place_blocks(&world, &region, pen(4, 11)).await;
        summon(
            &region,
            EntityType::Villager,
            jobless().with("CanPickUpLoot", true),
            [5.5, SKY as f64 + 1.0, 7.5],
        )
        .await;
        summon(
            &region,
            EntityType::Item,
            Compound::new()
                .with(
                    "Item",
                    tuff_item::item::ItemPicture::new(Item::Bread, 3).to_nbt(),
                )
                .with("PickupDelay", 0i16),
            [8.5, SKY as f64 + 1.0, 7.5],
        )
        .await;
        world.ticks(1).await;
        let id = super::mobs(&region, EntityType::Villager).await[0].id;
        let fed = wait_for_data(&world, &region, id, 400, |nbt| {
            nbt.get_list("Inventory")
                .is_some_and(|items| !items.is_empty())
        })
        .await;
        let item = fed.get_list("Inventory").unwrap()[0]
            .as_compound()
            .unwrap()
            .clone();
        assert_eq!(item.get_str("id"), Some("minecraft:bread"));
        assert_eq!(item.get_int("count"), Some(3));
        let left = super::mobs(&region, EntityType::Item).await;
        assert!(left.is_empty(), "the bread was taken");
        world.shutdown().await;
    }

    #[tuff_core::watchdog]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn villagers_restock_at_their_workstation() {
        let dir = TestDir::new();
        let world = TestWorld::start(dir.path());
        let chunk = ChunkPos::new(0, 0);
        let region = world.region(chunk.region()).await;
        let (viewer, mut events) = PlayerHandle::new(1000);
        subscribe(&region, &viewer, &mut events, &[chunk]).await;
        set_time(&world, 3000);
        let composter = BlockPos::new(8, SKY + 1, 8);
        let mut blocks = pen(4, 11);
        blocks.push((composter, Block::Composter.default_state()));
        place_blocks(&world, &region, blocks).await;
        let mut nbt = farmer().with("Xp", 5);
        let offers = nbt.get_compound_mut("Offers").unwrap();
        let recipe = offers.get_list_mut("Recipes").unwrap()[0]
            .as_compound_mut()
            .unwrap();
        recipe.insert("uses", 2);
        summon(
            &region,
            EntityType::Villager,
            nbt,
            [8.5, SKY as f64 + 1.0, 7.5],
        )
        .await;
        world.ticks(1).await;
        let id = super::mobs(&region, EntityType::Villager).await[0].id;
        let restocked = wait_for_data(&world, &region, id, 1500, |nbt| {
            nbt.get_compound("Offers")
                .and_then(|o| o.get_list("Recipes"))
                .and_then(|r| r[0].as_compound())
                .is_some_and(|r| r.get_int("uses").is_none())
        })
        .await;
        assert_eq!(restocked.get_int("RestocksToday"), Some(1));
        let recipe = restocked
            .get_compound("Offers")
            .unwrap()
            .get_list("Recipes")
            .unwrap()[0]
            .as_compound()
            .unwrap()
            .clone();
        assert_eq!(
            recipe.get_int("demand"),
            Some(2),
            "both uses count as demand"
        );
        world.shutdown().await;
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_villager_holds_up_what_it_sells_to_a_player_holding_what_it_buys() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let overworld = crate::world::clock::clock_id("minecraft:overworld").unwrap();
    world
        .clocks
        .send_modify(|clocks| clocks.get_mut(overworld).set(3000));
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(4, 11)).await;
    let y = SKY as f64 + 1.0;
    summon(
        &region,
        EntityType::Villager,
        farmer().with("Xp", 1),
        [6.5, y, 7.5],
    )
    .await;
    world.ticks(2).await;
    let id = mobs(&region, EntityType::Villager).await[0].id;
    let shown = |region: RegionHandle| async move {
        let (reply, answer) = oneshot::channel();
        let at = BlockPos::new(6, SKY, 7);
        region
            .request(RegionRequest::WhenLoaded {
                min: at,
                max: at,
                run: crate::world::region_api::RegionTask::new(move |world| {
                    let held = world.entity(id).and_then(|tracked| {
                        tracked
                            .entity
                            .equipment()
                            .into_iter()
                            .find(|(slot, _)| {
                                *slot == tuff_core::protocol::play::equipment_slot::MAIN_HAND
                            })?
                            .1
                            .map(|stack| stack.item())
                    });
                    let _ = reply.send(held);
                }),
            })
            .await;
        answer.await.unwrap()
    };
    region
        .request(player_update(&viewer, [8.5, y, 7.5], Some(Item::Stick)))
        .await;
    world.ticks(5).await;
    assert_ne!(
        shown(region.clone()).await,
        Some(Item::Emerald),
        "a stick buys nothing"
    );
    region
        .request(player_update(&viewer, [8.5, y, 7.5], Some(Item::Wheat)))
        .await;
    world.ticks(5).await;
    assert_eq!(
        shown(region.clone()).await,
        Some(Item::Emerald),
        "an emerald is held up"
    );
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::EntityData { id, reply })
        .await;
    let saved = answer.await.unwrap().expect("villager");
    assert!(
        saved
            .get_compound("equipment")
            .and_then(|e| e.get_compound("mainhand"))
            .is_none(),
        "what it shows is a picture, never its equipment"
    );
    region
        .request(player_update(&viewer, [8.5, y, 7.5], Some(Item::Stick)))
        .await;
    world.ticks(5).await;
    assert_ne!(
        shown(region.clone()).await,
        Some(Item::Emerald),
        "put away again"
    );
    world.shutdown().await;
}

mod zombies {
    //! Zombies turning villagers, and curing zombie villagers.

    use super::*;
    use crate::world::entity::EntityAttack;
    use crate::world::messages::HandEffect;
    use crate::world::player::Difficulty;
    use crate::{interact, mobs, wait_for};

    async fn data(region: &RegionHandle, id: i32) -> Compound {
        let (reply, answer) = oneshot::channel();
        region
            .request(RegionRequest::EntityData { id, reply })
            .await;
        answer.await.unwrap().expect("the entity is there")
    }

    fn night(world: &TestWorld) {
        let overworld = crate::world::clock::clock_id("minecraft:overworld").unwrap();
        world
            .clocks
            .send_modify(|clocks| clocks.get_mut(overworld).set(18000));
    }

    #[tuff_core::watchdog]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn zombies_turn_villagers_into_zombie_villagers_on_hard() {
        let dir = TestDir::new();
        let world = TestWorld::start(dir.path());
        world.difficulty.send_replace(Difficulty::Hard);
        night(&world);
        let chunk = ChunkPos::new(0, 0);
        let region = world.region(chunk.region()).await;
        let (viewer, mut events) = PlayerHandle::new(1000);
        subscribe(&region, &viewer, &mut events, &[chunk]).await;
        place_blocks(&world, &region, pen(4, 11)).await;
        summon(
            &region,
            EntityType::Villager,
            farmer().with("NoAI", true).with("Xp", 3),
            [7.5, SKY as f64 + 1.0, 7.5],
        )
        .await;
        summon(
            &region,
            EntityType::Zombie,
            Compound::new().with("PersistenceRequired", true),
            [9.5, SKY as f64 + 1.0, 7.5],
        )
        .await;
        let found = wait_for(&world, &region, 1200, |found| {
            found.iter().any(|s| s.kind == EntityType::ZombieVillager)
                && !found.iter().any(|s| s.kind == EntityType::Villager)
        })
        .await;
        let zombie = found
            .iter()
            .find(|s| s.kind == EntityType::ZombieVillager)
            .unwrap();
        let nbt = data(&region, zombie.id).await;
        assert_eq!(
            nbt.get_compound("VillagerData")
                .and_then(|d| d.get_str("profession")),
            Some("minecraft:farmer")
        );
        assert_eq!(nbt.get_int("Xp"), Some(3));
        let offers = nbt
            .get_compound("Offers")
            .unwrap()
            .get_list("Recipes")
            .unwrap();
        assert_eq!(offers.len(), 1, "the trades are kept");
        world.shutdown().await;
    }

    #[tuff_core::watchdog]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn weakened_zombie_villagers_take_a_golden_apple_and_are_cured() {
        let dir = TestDir::new();
        let world = TestWorld::start(dir.path());
        night(&world);
        let chunk = ChunkPos::new(0, 0);
        let region = world.region(chunk.region()).await;
        let (viewer, mut events) = PlayerHandle::new(1000);
        subscribe(&region, &viewer, &mut events, &[chunk]).await;
        place_blocks(&world, &region, pen(4, 11)).await;
        let at = [7.5, SKY as f64 + 1.0, 7.5];
        summon(
            &region,
            EntityType::ZombieVillager,
            Compound::new()
                .with("NoAI", true)
                .with("PersistenceRequired", true),
            at,
        )
        .await;
        world.ticks(1).await;
        let zombie = mobs(&region, EntityType::ZombieVillager).await[0].id;
        let effect = interact(&region, &viewer, &mut events, zombie, Item::GoldenApple).await;
        assert!(
            matches!(effect, HandEffect::None),
            "without weakness the apple is refused"
        );

        let potion = tuff_item::item::ItemPicture::new(Item::SplashPotion, 1)
            .to_nbt()
            .with(
                "components",
                Compound::new().with(
                    "minecraft:potion_contents",
                    Compound::new().with("potion", "minecraft:weakness"),
                ),
            );
        summon(
            &region,
            EntityType::SplashPotion,
            Compound::new().with("Item", potion).with(
                "Motion",
                Tag::List(vec![Tag::Double(0.0), Tag::Double(-0.5), Tag::Double(0.0)]),
            ),
            [at[0], at[1] + 3.0, at[2]],
        )
        .await;
        let mut cured = false;
        for _ in 0..40 {
            world.ticks(1).await;
            if matches!(
                interact(&region, &viewer, &mut events, zombie, Item::GoldenApple).await,
                HandEffect::Consume(1)
            ) {
                cured = true;
                break;
            }
        }
        assert!(cured, "the weakened zombie villager took the apple");
        let nbt = data(&region, zombie).await;
        assert!(nbt.get_int("ConversionTime").unwrap() >= 3600);
        assert_eq!(nbt.get_uuid("ConversionPlayer"), Some(PLAYER_UUID));
        let effects = effect_ids(&nbt);
        assert!(
            effects.contains(&"minecraft:strength".to_owned()),
            "{effects:?}"
        );
        assert!(
            !effects.contains(&"minecraft:weakness".to_owned()),
            "{effects:?}"
        );
        world.shutdown().await;
    }

    #[tuff_core::watchdog]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn cured_zombie_villagers_are_villagers_who_like_their_curer() {
        let dir = TestDir::new();
        let world = TestWorld::start(dir.path());
        night(&world);
        let chunk = ChunkPos::new(0, 0);
        let region = world.region(chunk.region()).await;
        let (viewer, mut events) = PlayerHandle::new(1000);
        subscribe(&region, &viewer, &mut events, &[chunk]).await;
        place_blocks(&world, &region, pen(4, 11)).await;
        let offers = farmer().get_compound("Offers").unwrap().clone();
        summon(
            &region,
            EntityType::ZombieVillager,
            Compound::new()
                .with("NoAI", true)
                .with("PersistenceRequired", true)
                .with("ConversionTime", 5)
                .with("ConversionPlayer", Tag::uuid(PLAYER_UUID))
                .with(
                    "VillagerData",
                    Compound::new()
                        .with("type", "minecraft:taiga")
                        .with("profession", "minecraft:farmer")
                        .with("level", 2),
                )
                .with("Offers", offers)
                .with("Xp", 12),
            [7.5, SKY as f64 + 1.0, 7.5],
        )
        .await;
        let found = wait_for(&world, &region, 60, |found| {
            found.iter().any(|s| s.kind == EntityType::Villager)
                && !found.iter().any(|s| s.kind == EntityType::ZombieVillager)
        })
        .await;
        let villager = found
            .iter()
            .find(|s| s.kind == EntityType::Villager)
            .unwrap();
        let nbt = data(&region, villager.id).await;
        assert_eq!(
            nbt.get_compound("VillagerData")
                .and_then(|d| d.get_str("type")),
            Some("minecraft:taiga")
        );
        assert!(
            effect_ids(&nbt).contains(&"minecraft:nausea".to_owned()),
            "a cured villager is nauseous"
        );
        assert_eq!(nbt.get_int("Xp"), Some(12));
        let gossips = nbt.get_list("Gossips").unwrap();
        let about = |kind: &str| {
            gossips.iter().filter_map(Tag::as_compound).find(|g| {
                g.get_uuid("Target") == Some(PLAYER_UUID) && g.get_str("Type") == Some(kind)
            })
        };
        assert_eq!(
            about("major_positive").and_then(|g| g.get_int("Value")),
            Some(20)
        );
        assert_eq!(
            about("minor_positive").and_then(|g| g.get_int("Value")),
            Some(25)
        );
        world.shutdown().await;
    }

    #[tuff_core::watchdog]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn zombie_villagers_take_damage_from_players() {
        let dir = TestDir::new();
        let world = TestWorld::start(dir.path());
        let chunk = ChunkPos::new(0, 0);
        let region = world.region(chunk.region()).await;
        let (viewer, mut events) = PlayerHandle::new(1000);
        subscribe(&region, &viewer, &mut events, &[chunk]).await;
        place_blocks(&world, &region, pen(2, 13)).await;
        summon(
            &region,
            EntityType::ZombieVillager,
            Compound::new()
                .with("NoAI", true)
                .with("PersistenceRequired", true),
            [7.5, SKY as f64 + 1.0, 7.5],
        )
        .await;
        world.ticks(1).await;
        let zombie = mobs(&region, EntityType::ZombieVillager).await[0].id;
        let before = data(&region, zombie).await.get_float("Health").unwrap();
        region
            .request(RegionRequest::AttackEntity {
                attacker: viewer.clone(),
                target: zombie,
                attack: EntityAttack {
                    attacker: viewer.id,
                    attacker_uuid: PLAYER_UUID,
                    attacker_position: [7.5, SKY as f64 + 1.0, 8.5],
                    attacker_yaw: 0.0,
                    attacker_pitch: 0.0,
                    damage: 5.0,
                    knockback: 0.0,
                    weapon: None,
                    critical: false,
                    magic: false,
                    sweep: false,
                    part: None,
                },
                sweep: None,
            })
            .await;
        world.ticks(1).await;
        let after = data(&region, zombie).await.get_float("Health").unwrap();
        assert!(after < before, "the hit should have dealt damage");
        world.shutdown().await;
    }

    async fn infected_zombie_villager(world: &TestWorld, region: &RegionHandle) -> i32 {
        world.difficulty.send_replace(Difficulty::Hard);
        night(world);
        summon(
            region,
            EntityType::Villager,
            farmer(),
            [7.5, SKY as f64 + 1.0, 7.5],
        )
        .await;
        summon(
            region,
            EntityType::Zombie,
            Compound::new().with("PersistenceRequired", true),
            [9.5, SKY as f64 + 1.0, 7.5],
        )
        .await;
        let found = wait_for(world, region, 1200, |found| {
            found.iter().any(|s| s.kind == EntityType::ZombieVillager)
                && !found.iter().any(|s| s.kind == EntityType::Villager)
        })
        .await;
        found
            .iter()
            .find(|s| s.kind == EntityType::ZombieVillager)
            .unwrap()
            .id
    }

    #[tuff_core::watchdog]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn infected_zombie_villagers_chase_and_hit_players_they_see() {
        use crate::world::messages::PlayerLooks;
        use crate::{player_at, ticking_expect};

        let dir = TestDir::new();
        let world = TestWorld::start(dir.path());
        let chunk = ChunkPos::new(0, 0);
        let region = world.region(chunk.region()).await;
        let (player, mut events) = PlayerHandle::new(1000);
        subscribe(&region, &player, &mut events, &[chunk]).await;
        place_blocks(&world, &region, pen(4, 11)).await;
        // The zombie that did the infecting is still around too, so wait
        // specifically for a hit credited to the zombie villager it left
        // behind rather than the first hit from either of them.
        let id = infected_zombie_villager(&world, &region).await;
        let position = [10.5, SKY as f64 + 1.0, 10.5];
        region
            .request(player_at(&player, position, PlayerLooks::default()))
            .await;
        let hit = ticking_expect(&world, &mut events, 600, |event| match event {
            PlayerEvent::EntityHurt(hit) if hit.attacker.as_ref().is_some_and(|a| a.id == id) => {
                Some(*hit)
            }
            _ => None,
        })
        .await;
        assert_eq!(hit.kind.name, "minecraft:mob_attack");
        let attacker = hit.attacker.expect("the zombie villager is to blame");
        assert_eq!(attacker.kind, EntityType::ZombieVillager);
        let zombie = mobs(&region, EntityType::ZombieVillager)
            .await
            .into_iter()
            .find(|s| s.id == id)
            .expect("the zombie villager is still there");
        assert!(
            crate::world::distance_sq(zombie.position, position) < 3.0 * 3.0,
            "it walked over: {:?}",
            zombie.position
        );
        world.shutdown().await;
    }

    #[tuff_core::watchdog]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn infected_zombie_villagers_burn_in_the_sun() {
        let dir = TestDir::new();
        let world = TestWorld::start(dir.path());
        let chunk = ChunkPos::new(0, 0);
        let region = world.region(chunk.region()).await;
        let (viewer, mut events) = PlayerHandle::new(1000);
        subscribe(&region, &viewer, &mut events, &[chunk]).await;
        place_blocks(&world, &region, pen(4, 11)).await;
        let _id = infected_zombie_villager(&world, &region).await;
        assert_eq!(
            mobs(&region, EntityType::ZombieVillager).await.len(),
            1,
            "it should have spawned"
        );
        world.clocks.send_modify(|clocks| {
            clocks
                .get_mut(crate::world::clock::clock_id("minecraft:overworld").unwrap())
                .set(6000)
        });
        wait_for(&world, &region, 1200, |found| {
            found.iter().all(|s| s.kind != EntityType::ZombieVillager)
        })
        .await;
        world.shutdown().await;
    }

    #[tuff_core::watchdog]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn infected_zombie_villagers_take_damage_from_players() {
        let dir = TestDir::new();
        let world = TestWorld::start(dir.path());
        let chunk = ChunkPos::new(0, 0);
        let region = world.region(chunk.region()).await;
        let (viewer, mut events) = PlayerHandle::new(1000);
        subscribe(&region, &viewer, &mut events, &[chunk]).await;
        place_blocks(&world, &region, pen(4, 11)).await;
        let id = infected_zombie_villager(&world, &region).await;
        let before = data(&region, id).await.get_float("Health").unwrap();
        region
            .request(RegionRequest::AttackEntity {
                attacker: viewer.clone(),
                target: id,
                attack: EntityAttack {
                    attacker: viewer.id,
                    attacker_uuid: PLAYER_UUID,
                    attacker_position: [7.5, SKY as f64 + 1.0, 8.5],
                    attacker_yaw: 0.0,
                    attacker_pitch: 0.0,
                    damage: 5.0,
                    knockback: 0.0,
                    weapon: None,
                    critical: false,
                    magic: false,
                    sweep: false,
                    part: None,
                },
                sweep: None,
            })
            .await;
        world.ticks(1).await;
        let after = data(&region, id).await.get_float("Health").unwrap();
        assert!(after < before, "the hit should have dealt damage");
        world.shutdown().await;
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pack_trade_set_changes_a_farmers_first_offers() {
    let dir = TestDir::new();
    let current = tuff_vanilla_data::version::DATA_PACK_VERSION.major;
    let files = [
        (
            "pack.mcmeta".to_owned(),
            format!(
                r#"{{"pack":{{"description":"trades","min_format":{current},"max_format":{current}}}}}"#
            ),
        ),
        (
            "data/example/villager_trade/golden_carrots.json".to_owned(),
            r#"{"wants":{"id":"minecraft:emerald","count":3},
                "gives":{"id":"minecraft:golden_carrot","count":2},"max_uses":7,"xp":9}"#
                .to_owned(),
        ),
        (
            "data/minecraft/trade_set/farmer/level_1.json".to_owned(),
            r#"{"trades":["example:golden_carrots"],"amount":2}"#.to_owned(),
        ),
    ];
    for (path, contents) in &files {
        let path = dir.path().join("datapacks/trades").join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(4, 11)).await;
    region
        .request(player_update(&viewer, [9.5, SKY as f64 + 1.0, 7.5], None))
        .await;
    let mut villager = farmer().with("NoAI", true);
    villager.remove("Offers");
    summon(
        &region,
        EntityType::Villager,
        villager,
        [7.5, SKY as f64 + 1.0, 7.5],
    )
    .await;
    world.ticks(2).await;
    let villager = mobs(&region, EntityType::Villager).await[0].id;
    region
        .request(RegionRequest::InteractEntity {
            player: viewer.clone(),
            target: villager,
            interaction: interaction(&viewer, None),
        })
        .await;
    let view = expect(&mut events, |event| match event {
        PlayerEvent::Run(task) => task.data::<Opened>().map(|opened| opened.view.clone()),
        _ => None,
    })
    .await;
    assert_eq!(view.offers.len(), 1, "the pack's set has one trade");
    let offer = &view.offers[0];
    assert_eq!(offer.sell.item(), tuff_vanilla_data::Item::GoldenCarrot);
    assert_eq!(offer.sell.count(), 2);
    assert_eq!(
        (offer.buy.item, offer.buy.count),
        (tuff_vanilla_data::Item::Emerald, 3)
    );
    assert_eq!((offer.max_uses, offer.xp), (7, 9));
}
