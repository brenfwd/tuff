use super::*;

/// A mob type's own state and behaviour.
pub trait Species: Sized + Send + Debug + 'static {
    /// Fresh state for a new or loaded mob.
    fn create(rng: &mut StdRng) -> Self;

    /// Sets up the shared state: goals, path costs, ageable and animal
    /// parts.
    fn configure(core: &mut MobCore);

    fn load(&mut self, _core: &mut MobCore, _nbt: &mut Compound) {}

    /// Its value for an entity debug feed the shared state does not
    /// answer (bees, breezes, brains), asked only while someone
    /// subscribes.
    fn debug_value(
        &self,
        _core: &MobCore,
        _feed: tuff_vanilla_data::registry::DebugSubscription,
        _tick: u64,
    ) -> Option<tuff_core::protocol::play::DebugValue> {
        None
    }

    /// Its activity and memories for the brains feed, for mobs whose
    /// vanilla AI is activity driven.
    fn brain_state(&self, _core: &MobCore) -> crate::brain_debug::BrainState {
        crate::brain_debug::BrainState::default()
    }

    fn save(&self, _core: &MobCore, _out: &mut Compound) {}

    /// Entity data from index [`data::SPECIES`] on, full.
    fn metadata(&self, _core: &MobCore) -> Vec<(u8, MetadataValue)> {
        Vec::new()
    }

    fn sounds(&self, core: &MobCore) -> MobSounds;

    fn eating_sound(&self, _core: &MobCore) -> Option<SoundEvent> {
        None
    }

    fn ambient_sound_interval(&self) -> i32 {
        DEFAULT_AMBIENT_INTERVAL
    }

    /// Whether food can make it fall in love now (a turtle carrying eggs
    /// cannot).
    fn can_fall_in_love(&self, _core: &MobCore) -> bool {
        true
    }

    /// Whether it breeds with and is tempted by this stack.
    fn is_food(&self, _stack: &ItemStack) -> bool {
        false
    }

    /// Runs after the shared AI step.
    fn ai_step(&mut self, _core: &mut MobCore, _cx: &mut TickContext) {}

    /// Frozen in place: its goals and controls do not run (a ravager
    /// mid-bite or stunned).
    fn immobile(&self, _core: &MobCore) -> bool {
        false
    }

    /// Runs in the server AI step after navigation and before the move
    /// control.
    fn custom_ai_step(&mut self, _core: &mut MobCore, _cx: &mut TickContext) {}

    /// Runs at the start of every tick, before anything else moves it.
    fn tick(&mut self, _core: &mut MobCore, _cx: &mut TickContext) {}

    fn ate(&mut self, _core: &mut MobCore) {}

    /// A player's click, before the shared animal handling.
    fn interact(
        &mut self,
        _core: &mut MobCore,
        _interaction: &EntityInteraction,
        _cx: &mut TickContext,
    ) -> InteractOutcome {
        None
    }

    /// Shears it without a player (a dispenser); false if it has nothing
    /// to shear.
    fn shear(&mut self, _core: &mut MobCore, _cx: &mut TickContext) -> bool {
        false
    }

    /// A dispenser fires `stack` at it, before the shared equipping.
    fn dispensed_on(
        &mut self,
        _core: &mut MobCore,
        _stack: &ItemStack,
        _cx: &mut TickContext,
    ) -> Option<tuff_core::world::region_api::DispenseOutcome> {
        None
    }

    fn offspring(&self, _partner: &Self, rng: &mut StdRng) -> Self {
        Self::create(rng)
    }

    /// [`Self::offspring`] for a species whose babies read the world.
    fn offspring_in(&self, partner: &Self, cx: &mut TickContext) -> Self {
        self.offspring(partner, cx.rng)
    }

    /// Runs on a bred baby right after `offspring` sets its species, so it
    /// can react to its final rolled traits (a panda's gene-based
    /// attributes) the same way `finalize_spawn` does for a natural one.
    fn finalize_offspring(&mut self, _core: &mut MobCore) {}

    fn finalize_spawn(&mut self, _core: &mut MobCore, _cx: &mut TickContext) {}

    /// The stack an `Effect::TakeItem` it asked for took.
    fn took_item(&mut self, _core: &mut MobCore, stack: ItemStack) {
        stack.burn(tuff_item::item::BurnReason::Consumed);
    }

    /// Facts entity loot tables check.
    fn loot_facts(&self, _facts: &mut EntityFacts) {}

    /// Experience dropped when a player killed it: a monster's reward
    /// plus one to three per piece of equipment it would not always drop.
    fn experience(&self, core: &MobCore, rng: &mut StdRng) -> i32 {
        if core.xp_reward <= 0 {
            return 0;
        }
        core.xp_reward + core.equipment.experience_bonus(rng)
    }

    /// How many players can ride it; zero for a species nobody rides.
    fn seats(&self, _core: &MobCore) -> usize {
        0
    }

    /// Whether a player may get on now (a baby too small to ride).
    fn accepts_rider(&self, _core: &MobCore) -> bool {
        true
    }

    /// Whether its first rider's movement keys steer it right now (a
    /// tamed, saddled mount). See `RegionEntity::rideable_by_input`.
    fn drivable(&self, _core: &MobCore) -> bool {
        false
    }

    /// Whether this rider's input steers it (a pig answers only to a
    /// rider holding a carrot on a stick).
    fn controlled_by(&self, _core: &MobCore, _input: &RiderInput) -> bool {
        true
    }

    /// What a driving rider's input multiplies its movement speed by,
    /// called once a tick while it steers.
    fn ride_speed(
        &mut self,
        _core: &mut MobCore,
        _input: &RiderInput,
        _cx: &mut TickContext,
    ) -> f32 {
        1.0
    }

    /// The fastest its driving rider can make it go: its ridden movement
    /// speed at the highest boost and the horizontal speed a jump or dash
    /// adds.
    fn ridden_speed(&self, _core: &MobCore) -> Option<tuff_core::world::entity::RiddenSpeed> {
        None
    }

    /// Whether it flies where its driving rider steers it.
    fn flying_vehicle(&self, _core: &MobCore) -> bool {
        false
    }

    /// Whether the jump key makes it hop while ridden.
    fn hops_when_ridden(&self) -> bool {
        true
    }

    /// Whether the player behind `interaction` may shear its gear off:
    /// anything carrying no riders, by default.
    fn can_shear_equipment(&self, core: &MobCore, _interaction: &EntityInteraction) -> bool {
        core.base.riders.is_empty() && core.base.entity_riders.is_empty()
    }

    /// Whether a click with `stack` is not a ride but something else
    /// (feeding a pig).
    fn mount_refused(&self, _core: &MobCore, _stack: Option<&ItemStack>) -> bool {
        false
    }

    /// The chest of a mount's inventory, behind the saddle and body slots
    /// its screen shows from the equipment, with the menu it opens as.
    fn inventory(&self) -> Option<(&[Option<ItemStack>], tuff_core::world::menu::MenuKind)> {
        None
    }

    fn inventory_mut(&mut self) -> Option<&mut [Option<ItemStack>]> {
        None
    }

    /// Takes what it carries beyond its equipment (a mount's chest, an
    /// allay's or villager's inventory) before it is removed for good.
    fn take_items(&mut self) -> Vec<ItemStack> {
        self.inventory_mut()
            .map(|chest| chest.iter_mut().filter_map(Option::take).collect())
            .unwrap_or_default()
    }

    /// See `RegionEntity::container_opens_for`.
    fn inventory_opens_for(&self, _core: &MobCore, _player: i32, _sneaking: bool) -> bool {
        true
    }

    /// A rider spurred it on with a lure on a stick.
    fn boost(&mut self, _core: &mut MobCore, _roll: u32) -> bool {
        false
    }

    /// Scales a driving rider's raw forward/strafe input (each in
    /// -1.0..=1.0) into what `MobCore::strafe_full` actually applies --
    /// e.g. a horse halves backward movement and sideways strafing, as
    /// vanilla's own ridden mounts do. Only called when `drivable` is
    /// true. The identity scaling (used by nothing yet) is there for a
    /// future mount that wants raw 1:1 input.
    fn drive_input(&self, _core: &MobCore, input: RiderInput) -> (f32, f32) {
        (input.forward, input.strafe)
    }

    /// Damage of `amount` is about to be dealt to it, before any check
    /// (an axolotl deciding to play dead).
    fn incoming_damage(
        &mut self,
        _core: &mut MobCore,
        _damage: &Damage,
        _amount: f32,
        _cx: &mut TickContext,
    ) {
    }

    /// The air it holds when full, for the common breathing rule.
    fn max_air(&self) -> i16 {
        MAX_AIR_SUPPLY as i16
    }

    /// Out of water it breathes its air back all at once (dolphins).
    fn refills_air_at_once(&self) -> bool {
        false
    }

    /// Steers itself from its driving rider's `input` this tick in place
    /// of the common ground steering; false leaves it to that (a swimming
    /// mount sets its own speed and stroke).
    fn ride(&mut self, _core: &mut MobCore, _input: RiderInput, _cx: &mut TickContext) -> bool {
        false
    }

    /// Whether damage reaches it at all; a species may dodge or react
    /// instead (an enderman teleporting away from arrows).
    fn before_hurt(
        &mut self,
        _core: &mut MobCore,
        _damage: &Damage,
        _cx: &mut TickContext,
    ) -> bool {
        true
    }

    /// Damage it takes whatever its immunities, in place of the amount
    /// dealt (a ghast hit by a fireball a player sent back).
    fn damage_override(&self, _core: &MobCore, _damage: &Damage) -> Option<f32> {
        None
    }

    /// Takes the whole hit in place of its health (wolf armor wearing
    /// down); returns whether it did.
    fn absorb_hurt(
        &mut self,
        _core: &mut MobCore,
        _damage: &Damage,
        _amount: f32,
        _cx: &mut TickContext,
    ) -> bool {
        false
    }

    /// Scales the damage left after armor and effects (a witch's magic
    /// resistance).
    fn scale_damage(&self, _core: &MobCore, _damage: &Damage, amount: f32) -> f32 {
        amount
    }

    /// Runs after it took damage and survived.
    fn after_hurt(&mut self, _core: &mut MobCore, _damage: &Damage, _cx: &mut TickContext) {}

    /// Takes over a hit before the common hurt pipeline sees it:
    /// `Some(taken)` ends it there (a sulfur cube holding a block shrugs off
    /// a punch but still flies).
    fn intercept_hurt(
        &mut self,
        _core: &mut MobCore,
        _damage: &Damage,
        _amount: f32,
        _cx: &mut TickContext,
    ) -> Option<bool> {
        None
    }

    /// Pushes it in place of the common knockback; false leaves it to that.
    fn knockback(
        &mut self,
        _core: &mut MobCore,
        _push: &crate::Knockback,
        _damage: &Damage,
        _cx: &mut TickContext,
    ) -> bool {
        false
    }

    /// Runs when its body goes, after the death animation (a slime
    /// splits).
    fn remove_dead(&mut self, _core: &mut MobCore, _cx: &mut TickContext) {}

    /// Runs when it dies, before its loot drops.
    fn died(&mut self, _core: &mut MobCore, _damage: &Damage, _cx: &mut TickContext) {}

    /// A death it owes to something outside the hurt pipeline (a creaking
    /// whose heart a player broke), taken right after `tick`: it dies of
    /// that damage, with loot and kill credit, whatever its health.
    fn take_death(&mut self, _core: &mut MobCore) -> Option<Damage> {
        None
    }

    /// Runs each tick it lies dead in place of the common fade and poof:
    /// `Some(true)` removes it now, `Some(false)` keeps it, `None` leaves
    /// it to the common one.
    fn tick_death(&mut self, _core: &mut MobCore, _cx: &mut TickContext) -> Option<bool> {
        None
    }

    fn remove_when_far_away(&self) -> bool {
        true
    }

    /// A player-rules melee swing at `victim`; false if it cannot.
    fn melee_strike(&mut self, _core: &mut MobCore, _victim: u128, _cx: &mut TickContext) -> bool {
        false
    }

    /// A shulker box lid opened into it.
    fn shulker_box_pushed(&mut self, _core: &mut MobCore) {}

    /// How a lead tied to it behaves now, or `None` when it cannot be
    /// leashed (see [`crate::leash::default_profile`]).
    fn leash_profile(&self, core: &MobCore) -> Option<tuff_core::world::leash::LeashProfile> {
        crate::leash::mob_profile(core)
    }

    /// Whether a slack lead walks it toward the holder (not a flyer that
    /// drifts on its own, like an allay).
    fn follows_lead(&self) -> bool {
        true
    }

    /// The speed it follows a slack lead at.
    fn lead_speed(&self) -> f64 {
        1.0
    }

    /// Its lead went taut and pulled it (a horse stops eating).
    fn lead_pulled(&mut self, _core: &mut MobCore, _cx: &mut TickContext) {}

    /// Whether its own state keeps it (a bucketed fish, an enderman
    /// carrying a block), which also takes it out of the spawn caps.
    fn custom_persistence(&self, _core: &MobCore) -> bool {
        false
    }

    /// Overrides the common breathing rule (see [`Mob::breathe`]) for a
    /// type whose dumped `EntityType::mob().own_air_rules` is set. A type
    /// with that flag must return `Some` here before it is registered, or
    /// `registered_types_breathe_by_the_common_rules` fails the build.
    fn air_rules(&self) -> Option<OwnAirRules> {
        None
    }

    /// Whether water and lava currents push this mob. Swimmers that steer
    /// through water themselves return false.
    fn pushed_by_fluid(&self) -> bool {
        true
    }

    /// Whether a naturally spawned group sometimes holds babies.
    const RANDOM_BABIES: bool = true;

    /// The current value of a changed data index its `metadata` leaves
    /// out (living data it only sometimes has).
    fn changed_metadata(&self, _core: &MobCore, _index: u8) -> Option<MetadataValue> {
        None
    }

    /// A player took offer `request.offer` (merchants); true when it
    /// agreed.
    fn trade(
        &mut self,
        _core: &mut MobCore,
        _request: &crate::world::trading::TradeRequest,
        _cx: &mut TickContext,
    ) -> bool {
        false
    }

    fn is_merchant(&self) -> bool {
        false
    }

    fn stop_trading(&mut self, _core: &mut MobCore, _player: u128) {}

    /// Points of interest it has claimed.
    fn poi_claims(&self, _core: &MobCore) -> Vec<BlockPos> {
        Vec::new()
    }

    /// The bed it is sleeping in, if any (the head position it was told to
    /// sleep at).
    fn sleeping_in(&self, _core: &MobCore) -> Option<BlockPos> {
        None
    }

    /// A player woke it out of its bed by right-clicking it.
    fn wake_from_bed(&mut self, _core: &mut MobCore, _cx: &mut TickContext) {}

    /// Something another entity told it that the shared handling leaves
    /// alone (a villager courting it).
    fn heard(&mut self, _core: &mut MobCore, _notice: Notice) {}

    /// Whether it is a sheep with blue wool.
    fn blue_wool(&self) -> bool {
        false
    }

    /// Breeds with `partner`: the baby, if one comes.
    fn breed(
        mob: &mut Mob<Self>,
        partner: &mut Mob<Self>,
        cx: &mut TickContext,
    ) -> Option<Box<dyn RegionEntity>> {
        animal::breed(mob, partner, cx)
    }

    /// Breeds with a partner of any kind; by default only its own
    /// species, through [`Species::breed`].
    fn breed_with(
        mob: &mut Mob<Self>,
        partner: &mut dyn RegionEntity,
        cx: &mut TickContext,
    ) -> Option<Box<dyn RegionEntity>> {
        let partner = partner.downcast_mut::<Mob<Self>>()?;
        Self::breed(mob, partner, cx)
    }

    /// A boss health bar to show nearby players (see
    /// [`tuff_core::world::entity::RegionEntity::boss_bar`]); `None` by
    /// default, like every other mob.
    fn boss_bar(&self, _core: &MobCore) -> Option<tuff_core::world::entity::BossBarState> {
        None
    }

    /// How viewers' copies collide when it differs from the plain box.
    fn track_shape(&self, _core: &MobCore) -> Option<tuff_core::world::messages::Shape> {
        None
    }

    /// Hears about someone (villagers' gossip).
    fn reputation(
        &mut self,
        _core: &mut MobCore,
        _event: villager::Reputation,
        _source: u128,
        _cx: &mut TickContext,
    ) {
    }
}
