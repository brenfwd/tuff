use super::*;

/// Something an entity's tick or an action on it does beyond itself,
/// applied by the region afterwards.
#[derive(Debug)]
pub enum Effect {
    /// Packets for players who can see the entity.
    EntityPackets(Packets),
    /// Items the entity holds changed (an item frame's item, an armor
    /// stand's equipment), for features observing containers; `player` did
    /// it, else the entity itself.
    HeldItems {
        player: Option<crate::world::messages::PlayerId>,
        slots: Vec<crate::world::region_api::SlotEdit>,
    },
    /// Packets for everyone viewing the entity's chunk (sounds, particles).
    ChunkPackets(Packets),
    /// Packets for every player within `range` of `center`, including those
    /// in neighbouring regions or not viewing the entity's chunk (a mob's
    /// voice).
    NearbyPackets {
        center: [f64; 3],
        range: f64,
        packets: Packets,
    },
    SetBlock {
        pos: BlockPos,
        state: BlockState,
    },
    /// Runs a command as a command block at the entity's position; its
    /// `SuccessCount` (and `LastOutput` when `track_output`) land in the
    /// entity's unknown saved fields once it finishes.
    RunCommand {
        command: String,
        name: String,
        position: [f64; 3],
        track_output: bool,
    },
    /// A player opened or broke a container entity: piglins that see it
    /// turn on it.
    DisturbGuarded {
        player: i32,
    },
    /// Boxes the entity occupied along its move this tick, before the one
    /// it ends in: blocks that sense entities react to each.
    PassedThrough(Vec<[f64; 6]>),
    /// The entity landed on a block after a fall.
    Landed(crate::world::region_api::Landing),
    /// A falling block landed: places it with the full neighbour and shape
    /// cascade a normal placement gets, so a block resting on it reacts and
    /// the block that used to hold it up (now air) keeps a stack falling.
    PlaceFallenBlock {
        pos: BlockPos,
        state: BlockState,
        data: Option<Compound>,
    },
    /// A game event at `position`, caused by the entity `source`.
    GameEvent {
        event: crate::world::game_event::GameEvent,
        position: [f64; 3],
        source: Option<i32>,
    },
    /// Lightning struck the block at `pos` (the one under the bolt).
    LightningStrike {
        pos: BlockPos,
    },
    /// A projectile landed on a block or an entity: the `projectile_land`
    /// game event, and region hooks hear of it.
    ProjectileLanded(Box<ProjectileLanding>),
    /// A projectile struck a block, which may react to it.
    ProjectileHitBlock {
        hit: crate::world::raycast::Hit,
        impact: Box<ProjectileImpact>,
    },
    /// Breaks a block as the world does, with its particles and sound.
    DestroyBlock {
        pos: BlockPos,
        drop: bool,
    },
    DropItem {
        stack: ItemStack,
        position: [f64; 3],
        velocity: [f64; 3],
        /// Ticks before it can be picked up.
        pickup_delay: u32,
    },
    /// An item that lives twice as long before despawning (a wither's
    /// nether star).
    DropExtendedItem {
        stack: ItemStack,
        position: [f64; 3],
        velocity: [f64; 3],
        pickup_delay: u32,
    },
    Experience {
        position: [f64; 3],
        amount: i32,
    },
    /// A mob died worth `amount` experience: a sculk catalyst may take it,
    /// else orbs drop if a player killed it.
    MobExperience {
        position: [f64; 3],
        amount: i32,
        player_killed: bool,
        /// The player who last hurt it and the kill as it saw it, for the
        /// advancement a catalyst taking the experience awards.
        killer: Option<(
            crate::world::messages::PlayerId,
            Box<crate::world::messages::Kill>,
        )>,
    },
    Spawn(Box<dyn RegionEntity>),
    /// Breeds with the entity `partner`, which gets told as well.
    Breed {
        partner: i32,
    },
    /// Hurts a player or another entity, by id.
    Hurt {
        target: i32,
        hit: Box<EntityHit>,
    },
    /// The entity is crammed among `counted` (entity ids) and `players`:
    /// it takes cramming damage unless enough of those entities died
    /// earlier this tick to bring the count to `max` or under.
    Crammed {
        counted: Vec<i32>,
        players: i32,
        max: i32,
    },
    /// An explosion at the entity's doing.
    Explode(Explosion),
    /// Pushes a player or another entity, by id, whether or not it is
    /// hurt.
    Push {
        target: i32,
        push: [f64; 3],
    },
    /// A shulker's opening shell pushes what is in `area` (not itself)
    /// `amount` blocks along `direction`, stopping at blocks.
    ShellPush {
        area: [f64; 6],
        direction: tuff_vanilla_data::block::Facing,
        amount: f64,
    },
    /// A contact push (a cart or boat shoving a neighbour). A player's
    /// client applies it to itself, so the server sends it nothing and
    /// only records the impulse for the movement checks.
    Nudge {
        target: i32,
        push: [f64; 3],
    },
    /// Takes up to `count` from the dropped item `item` (a mob picking
    /// it up). The mob already has it; the region only removes it, and
    /// later mobs in the same tick see what is left.
    TakeItem {
        item: i32,
        count: u8,
    },
    /// A copper golem takes up to `stack.count` of `stack` (matched with
    /// its components) from the container at `pos`. The golem's hand is
    /// filled through `took_item` with what the container actually gave.
    CopperGolemTake {
        pos: BlockPos,
        stack: tuff_item::item::ItemSnapshot,
    },
    /// Runs a feature's own work on the region.
    Run(crate::world::region_api::RegionTask),
    /// A copper golem deposited `stack` (already cleared from its own held
    /// item optimistically) into the container at `pos`.
    CopperGolemDeposit {
        pos: BlockPos,
        stack: ItemStack,
    },
    /// The acting mob opens or closes the container at `pos` (both halves
    /// of a double chest): the lid and sounds follow, and players and
    /// other mobs see it in use.
    ContainerOpen {
        pos: BlockPos,
        open: bool,
    },
    /// Calls mobs of `kinds` standing in `area` in the neighbouring
    /// regions to turn on `attacker` (this region's own are told with
    /// `Notify`).
    AlertNeighbours {
        kinds: &'static [EntityType],
        area: [f64; 6],
        attacker: i32,
    },
    /// Runs `run` on each neighbouring region `area` reaches into (a
    /// burst near a border hurting what the neighbour holds).
    InNeighbours {
        area: [f64; 6],
        run: NeighbourTask,
    },
    /// Tells another entity something.
    Notify {
        target: i32,
        notice: Notice,
    },
    /// `passenger` gets on `vehicle`, both entities of the region.
    Mount {
        passenger: i32,
        vehicle: i32,
    },
    /// The entity gets off what it rides.
    StopRiding,
    /// A spear stab knocks `target` (a player or an entity of the region)
    /// off what it rides.
    StabDismount {
        target: i32,
    },
    /// A rider is off: it stands at `position` now.
    Dismount {
        rider: Rider,
        position: [f64; 3],
    },
    /// A player killed this entity (of type `kind`): it gets the credit.
    /// Counts towards the advancements of the player with this UUID, if
    /// the region sees it.
    Advancement {
        player: u128,
        event: Box<crate::world::messages::AdvancementEvent>,
    },
    KillCredit {
        player: i32,
        kill: Box<crate::world::messages::Kill>,
    },
    /// The entity died: features observing deaths are told, with who to
    /// blame.
    Died {
        player: Option<u128>,
        attacker: Option<EntityType>,
        damage: &'static str,
    },
    /// A tamed animal died: its owner, wherever they are, is told how.
    PetDied {
        owner: u128,
        victim: TextComponent,
        kind: &'static DamageType,
        killer: Option<TextComponent>,
        killer_player: Option<u128>,
    },
    /// Gives a player potion effects.
    Potion {
        target: i32,
        hit: Box<crate::world::effects::PotionHit>,
    },
    /// Gives a region entity lasting effects, by id.
    MobEffects {
        target: i32,
        effects: Vec<crate::world::effects::ActiveEffect>,
    },
    /// Opens or shuts a door (both halves), with its sound.
    UseDoor {
        pos: BlockPos,
        open: bool,
    },
    /// Rings the bell at `pos`, as a villager calling the village.
    RingBell {
        pos: BlockPos,
    },
    /// Marks a bed taken or free.
    OccupyBed {
        pos: BlockPos,
        occupied: bool,
    },
    /// Tells a player something (a merchant's screen), by entity id.
    TellPlayer {
        player: i32,
        event: Box<crate::world::messages::PlayerEvent>,
    },
    /// Moves a player (an ender pearl's thrower) to `position`.
    TeleportPlayer {
        player: i32,
        /// The player's session, when the region may not see it.
        handle: Option<crate::world::messages::PlayerHandle>,
        position: [f64; 3],
        fall_damage: f32,
    },
    /// Removes the entity outright, without a death animation or drops (a
    /// fish or axolotl captured in a bucket).
    Discard,
    /// The entity (a tamed parrot) touches its owner, by entity id, and
    /// asks to ride the owner's shoulder; on acceptance it leaves the
    /// world, saved into the player.
    SitOnShoulder {
        player: i32,
    },
}

/// What one entity tells another through the region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notice {
    /// It bred and is a parent now.
    Bred,
    /// A mob of its kind was hurt by `attacker`: help it.
    Alert { attacker: i32 },
    /// A mob of its kind was hurt by a player under universal anger: it
    /// forgets its target and turns on every player.
    UniversalAnger { attacker: u128 },
    /// Whoever made it is done with it: it goes away.
    Discard,
    /// Its owner reels in (a fishing bobber).
    Reel,
    /// A villager wants to breed with this one.
    Courted { by: i32 },
    /// A lightning strike charged it (a creeper).
    Charged,
    /// Lightning struck it (a cushion falls apart).
    Struck,
    /// Splashed with water (an axolotl gets its moisture back).
    Rehydrate,
    /// A hit it asked to be told about hurt its victim.
    HitLanded,
    /// A vibration reached it, from `source` (-1 for none) at a block.
    Heard { at: [i32; 3], source: i32 },
    /// A bell rang within hearing.
    BellRung,
    /// A mob spawning beside it took it as its mount (a chicken jockey).
    Jockey,
    /// A fellow mob is retreating from `from`: get away from it too.
    Retreat { from: i32 },
    /// A hit it asked to hear about was stopped by `defender`'s shield.
    ShieldBlocked { defender: i32 },
    /// Another mob handed it an item (an iron golem's poppy).
    Gifted(Item),
    /// A player died in this dimension with the `forgive_dead_players`
    /// rule on; `nearby` when it was within the death's forgiving box.
    PlayerDied { player: u128, nearby: bool },
    /// An evoker's wololo spell turns a sheep's wool red.
    Wololo,
}
