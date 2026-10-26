use tuff_core::world::messages::{Place, PlayerHandle};
use tuff_vanilla_data::Item;
use tuff_world_model::BlockPos;

/// Uses `item` on the top of `target`, standing south of it looking down.
pub fn place(player: &PlayerHandle, target: BlockPos, item: Item, sequence: i32) -> Place {
    Place {
        player: player.clone(),
        eye: [
            target.x as f64 + 0.5,
            target.y as f64 + 2.62,
            target.z as f64 + 1.5,
        ],
        target,
        face: tuff_world_model::Face::Up,
        cursor: [0.5, 1.0, 0.5],
        yaw: 0.0,
        pitch: 60.0,
        sneaking: false,
        item: item.block().map(|_| item),
        held: Some(item),
        components: Default::default(),
        held_count: 1,
        off_hand: false,
        off_hand_item: None,
        sequence,
        interact: true,
        can_eat: true,
        name: "Tester".to_owned(),
        uuid: u128::from(player.id as u32),
        position: [
            target.x as f64 + 0.5,
            target.y as f64 + 1.0,
            target.z as f64 + 1.5,
        ],
        off_hand_blocks: false,
        may_build: true,
        game_master: false,
        context: None,
        offer: Default::default(),
        hand_slot: tuff_core::world::inventory::Inventory::hotbar_slot(0),
    }
}

/// `place` with one of its held item moved into the click, as a session
/// does for blocks that keep one (a campfire, a pot, a composter).
pub fn offering(mut place: Place) -> Place {
    if let Some(item) = place.held {
        let minted = tuff_core::world::messages::PlaceOffer::new(
            false,
            vec![(
                place.hand_slot,
                tuff_item::item::ItemStack::mint_with(
                    item,
                    1,
                    &place.components,
                    tuff_item::item::MintReason::Test,
                ),
            )],
        );
        place.offer = minted;
    }
    place
}
