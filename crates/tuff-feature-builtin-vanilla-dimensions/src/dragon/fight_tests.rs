use super::*;

#[tuff_core::watchdog]
#[test]
fn a_respawned_dragon_is_summoned_for_every_boss_bar_player() {
    let (one, mut one_events) = PlayerHandle::new(1);
    let (two, mut two_events) = PlayerHandle::new(2);
    let boss = BossBar {
        uuid: 1,
        name: TextComponent::translate("entity.minecraft.ender_dragon", []),
        progress: 1.0,
        visible: true,
        players: HashMap::from([(1, one), (2, two)]),
    };
    let dragon = EntityFacts {
        kind: Some(EntityType::EnderDragon),
        ..EntityFacts::default()
    };
    boss.notify_summoned(&dragon);
    for events in [&mut one_events, &mut two_events] {
        assert!(matches!(
            events.try_recv(),
            Ok(PlayerEvent::Advancement(event))
                if matches!(*event, AdvancementEvent::SummonedEntity(_))
        ));
    }
}

#[tuff_core::watchdog]
#[test]
fn saved_data_round_trips_with_the_game_keys() {
    let mut data = FightData {
        needs_state_scanning: false,
        dragon_killed: true,
        previously_killed: true,
        dragon_uuid: Some(0x1234),
        exit_portal: Some(BlockPos::new(0, 64, 0)),
        ..Default::default()
    };
    data.init_gateways(1);
    data.respawn_stage = Some(RespawnStage::SummoningPillars);
    data.respawn_crystals = vec![5, 6];
    data.extra.insert("future_key", 3);
    let saved = data.save();
    assert_eq!(saved.get_str("respawn_stage"), Some("summoning_pillars"));
    assert_eq!(
        saved.get("exit_portal_location"),
        Some(&Tag::IntArray(vec![0, 64, 0]))
    );
    assert_eq!(FightData::load(saved), data);
}

#[tuff_core::watchdog]
#[test]
fn gateways_are_twenty_shuffled_slots_on_a_ring() {
    let mut data = FightData::default();
    data.init_gateways(42);
    let mut sorted = data.gateways.clone();
    sorted.sort();
    assert_eq!(sorted, (0..20).collect::<Vec<_>>());
    assert_ne!(data.gateways, sorted);
    for index in 0..20 {
        let pos = gateway_position(index);
        let distance = f64::from(pos.x * pos.x + pos.z * pos.z).sqrt();
        assert!((distance - 96.0).abs() < 1.5, "{pos:?}");
        assert_eq!(pos.y, 75);
    }
}

#[tuff_core::watchdog]
#[test]
fn the_podium_has_a_portal_only_when_active_and_a_torched_pillar() {
    let origin = BlockPos::new(0, 64, 0);
    let active = podium_blocks(origin, true);
    let last = |blocks: &[(BlockPos, BlockState, bool)], pos: BlockPos| {
        blocks
            .iter()
            .rev()
            .find(|(p, _, _)| *p == pos)
            .map(|(_, s, _)| s.block())
    };
    assert_eq!(
        last(&active, BlockPos::new(1, 64, 1)),
        Some(Block::EndPortal)
    );
    assert_eq!(last(&active, BlockPos::new(0, 67, 0)), Some(Block::Bedrock));
    assert_eq!(last(&active, BlockPos::new(3, 64, 0)), Some(Block::Bedrock));
    assert_eq!(
        last(&active, BlockPos::new(0, 66, -1)),
        Some(Block::WallTorch)
    );
    let inactive = podium_blocks(origin, false);
    assert_eq!(last(&inactive, BlockPos::new(1, 64, 1)), Some(Block::Air));
    let portals = active
        .iter()
        .filter(|(p, _, _)| last(&active, *p) == Some(Block::EndPortal))
        .count();
    assert_eq!(portals, 20);
}

#[tuff_core::watchdog]
#[test]
fn countdowns_fire_on_their_period_and_expired_ones_at_once() {
    let mut every_three = Countdown::new(3);
    let fired: Vec<bool> = (0..7).map(|_| every_three.step()).collect();
    assert_eq!(fired, [false, false, true, false, false, true, false]);
    let mut eager = Countdown::expired(3);
    assert!(eager.step());
    assert!(!eager.step());
    eager.rewind();
    assert!(!eager.step());
}

#[tuff_core::watchdog]
#[test]
fn the_summoning_timeline_is_pinned() {
    use respawn::{SummoningCue as C, summoning_cue};
    let cues = |stage, until: i32| -> Vec<(i32, C)> {
        (0..=until)
            .map(|t| (t, summoning_cue(stage, t, 10)))
            .filter(|(_, c)| *c != C::Wait)
            .collect()
    };
    assert_eq!(summoning_cue(RespawnStage::Start, 0, 10), C::BeginBeaming);
    let mut prep = vec![(0, C::Roar), (50, C::Roar), (51, C::Roar), (52, C::Roar)];
    prep.extend((95..100).map(|t| (t, C::Roar)));
    prep.extend((100..=101).map(|t| (t, C::Advance(RespawnStage::SummoningPillars))));
    assert_eq!(cues(RespawnStage::PreparingToSummonPillars, 101), prep);
    let mut pillars = Vec::new();
    for i in 0..10 {
        pillars.push((i * 40, C::AimAtSpike(i as usize)));
        pillars.push((i * 40 + 39, C::RebuildSpike(i as usize)));
    }
    pillars.push((400, C::Advance(RespawnStage::SummoningDragon)));
    pillars.push((440, C::Advance(RespawnStage::SummoningDragon)));
    assert_eq!(cues(RespawnStage::SummoningPillars, 479), pillars);
    let mut awaken = vec![(0, C::AimAtSummonPoint)];
    awaken.extend((1..5).map(|t| (t, C::Roar)));
    awaken.extend((80..100).map(|t| (t, C::Roar)));
    awaken.push((100, C::Finish));
    assert_eq!(cues(RespawnStage::SummoningDragon, 100), awaken);
    assert_eq!(cues(RespawnStage::End, 200), []);
}
