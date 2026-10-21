use super::*;

impl PlayerSession {
    pub fn dimension(&self) -> &Dimension {
        self.shared.dimension(self.dimension)
    }

    pub fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    pub fn id(&self) -> PlayerId {
        self.id
    }

    pub fn rng(&mut self) -> &mut StdRng {
        &mut self.rng
    }

    /// Reads the player's saved data as joining does.
    pub async fn load_saved(&mut self) {
        self.load().await.expect("player data loads");
    }

    pub fn tick_count(&self) -> u64 {
        self.tick
    }

    /// Moves the player to another dimension as a portal would, without
    /// the regions: only what advancements see.
    pub fn travel_for_test(&mut self, dimension: DimensionId, position: [f64; 3]) {
        let from = (self.dimension().key.clone(), self.pose.position);
        self.dimension = dimension;
        self.pose.position = position;
        self.changed_dimension(&from.0, from.1);
    }

    pub fn knows_recipe(&self, recipe: &str) -> bool {
        self.progress.recipe_book.contains(recipe)
    }

    pub fn inventory(&self) -> &Inventory {
        &self.inventory
    }

    pub fn inventory_mut(&mut self) -> &mut Inventory {
        &mut self.inventory
    }

    pub fn experience(&self) -> &Experience {
        &self.body.experience
    }

    pub fn position(&self) -> [f64; 3] {
        self.pose.position
    }

    pub fn dimension_id(&self) -> DimensionId {
        self.dimension
    }

    pub fn respawn_point_save(&self) -> Option<&RespawnSave> {
        self.mode.respawn.as_ref()
    }

    /// Lets this player see a region's entity as reported by `track`.
    pub fn see_entity(&mut self, track: crate::world::messages::Track) {
        self.view.subscribed.insert(track.chunk);
        self.view.center = track.chunk;
        let mut out = Vec::new();
        let tick = self.tick;
        let mut entities = std::mem::take(&mut self.view.entities);
        entities.track(track, &self.area(), tick, false, &mut out);
        self.view.entities = entities;
    }

    pub fn health(&self) -> f32 {
        self.body.health.value
    }

    pub fn effect(&self, effect: MobEffect) -> Option<&crate::world::effects::ActiveEffect> {
        self.body.effects.get(effect)
    }

    /// Holds up the item in a hand as if for `ticks` already.
    pub fn hold_up(&mut self, off_hand: bool, ticks: u32) {
        if let Some(stack) = self.inventory.hand(off_hand) {
            let mut using = UsingItem::new(off_hand, stack.item, UsingItem::HELD);
            using.remaining -= ticks;
            self.actions.using = Some(using);
        }
    }

    pub fn is_using(&self) -> bool {
        self.actions.using.is_some()
    }

    pub fn is_gliding(&self) -> bool {
        self.motion.glide.active
    }

    pub fn item_on_cooldown(&self, item: Item) -> bool {
        self.on_cooldown(item)
    }

    pub fn is_sleeping(&self) -> bool {
        self.sleep.sleeping.is_some()
    }

    pub async fn handle_packet(&mut self, packet: Serverbound) {
        self.on_packet(packet).await;
    }

    /// Puts the player somewhere without telling anyone.
    pub fn stand_at(&mut self, position: [f64; 3], on_ground: bool) {
        self.pose.position = position;
        self.pose.on_ground = on_ground;
    }

    /// Lets this player see another one standing at `position`.
    pub fn see_player(&mut self, player: &PlayerHandle, position: [f64; 3]) {
        let chunk = ChunkPos::of_position(position);
        self.view.subscribed.insert(chunk);
        self.view.center = chunk;
        let track = crate::world::messages::Track {
            kind: EntityType::Player,
            entity: player.id,
            dimension: self.dimension,
            chunk,
            seq: 1,
            position,
            range: EntityType::Player.tracking().range_chunks as i32 * 16,
            spawn: packets([]),
            update: packets([]),
            moves: None,
            head: None,
            player: Some(player.clone()),
            spectator: false,
            shape: None,
        };
        let mut out = Vec::new();
        let tick = self.tick;
        let mut entities = std::mem::take(&mut self.view.entities);
        entities.track(track, &self.area(), tick, false, &mut out);
        self.view.entities = entities;
    }

    /// Lets `ticks` game ticks pass without running them.
    pub fn skip_ticks(&mut self, ticks: u64) {
        self.tick += ticks;
        self.view.mirror.set_clock(self.tick);
    }

    /// Runs one game tick.
    pub async fn run_tick(&mut self) {
        self.tick += 1;
        self.view.mirror.set_clock(self.tick);
        self.on_tick().await;
    }

    /// Leaves the player too hungry to heal.
    pub fn make_hungry(&mut self) {
        self.body.food.level = 10;
        self.body.food.saturation = 0.0;
    }

    /// Sets the hunger bar to an exact level (and zero saturation), so a
    /// test can control exactly how many nutrition points reach full.
    pub fn set_food_level(&mut self, level: i32) {
        self.body.food.level = level;
        self.body.food.saturation = 0.0;
    }

    pub fn food_level(&self) -> i32 {
        self.body.food.level
    }

    pub fn effect_duration(&self, effect: MobEffect) -> Option<i64> {
        self.body.effects.get(effect).map(|active| active.duration)
    }

    pub fn effect_amplifier(&self, effect: MobEffect) -> Option<u8> {
        self.body.effects.amplifier(effect)
    }

    pub fn fire_ticks(&self) -> i32 {
        self.body.fire_ticks
    }

    pub fn is_sprinting(&self) -> bool {
        self.pose.sprinting
    }

    pub fn set_sprinting(&mut self, sprinting: bool) {
        self.pose.sprinting = sprinting;
    }

    pub fn air(&self) -> i32 {
        self.body.air
    }

    pub fn pose(&self) -> Pose {
        self.current_pose()
    }

    pub async fn give_effect(&mut self, effect: crate::world::effects::ActiveEffect) {
        self.add_effect(effect).await;
    }
}
