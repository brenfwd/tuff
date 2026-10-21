use super::*;

/// Open menus, container locks and ticking block entities.
#[derive(Default)]
pub(super) struct Menus {
    /// Players with a menu open over the container (or ender chest) at
    /// each position; their number drives lids and sounds.
    pub(super) viewers: HashMap<BlockPos, Vec<PlayerHandle>>,
    /// Mobs holding the container at each position open, by entity id;
    /// they count towards the lid like viewers.
    pub(super) mob_openers: HashMap<BlockPos, Vec<i32>>,
    /// Container slots locked by the first phase of a two-region
    /// transaction, by position, with the player and transaction id.
    pub(super) container_locks: HashMap<BlockPos, (PlayerId, u64)>,
    /// The changes of prepared two-region transactions, by player and id.
    pub(super) prepared: HashMap<(PlayerId, u64), Vec<ContainerChange>>,
    /// Contents of containers broken while a prepared share still touched
    /// them, spilled once that transaction resolves.
    pub(super) orphans: Orphans,
    /// Players with a menu open over an entity's container, by entity id.
    pub(super) entity_viewers: HashMap<i32, Vec<PlayerHandle>>,
    /// The player each sign's editor is open for, who alone may change it.
    pub(super) sign_editors: HashMap<BlockPos, PlayerHandle>,
    /// Block entities that act every tick (hoppers), so ticking never
    /// scans chunks.
    pub(super) ticking: HashSet<BlockPos>,
    /// When each hopper last ticked, for the hopper-to-hopper cooldown rule.
    pub(super) hopper_ticks: HashMap<BlockPos, u64>,
}

impl Menus {
    pub(super) fn pending_share(&self, pos: BlockPos) -> Option<(PlayerId, u64)> {
        self.prepared
            .iter()
            .find(|(_, changes)| changes.iter().any(|change| change.pos == pos))
            .map(|(key, _)| *key)
    }

    pub(super) fn remove_entity_viewer(&mut self, entity: i32, player: PlayerId) {
        if let Some(viewers) = self.entity_viewers.get_mut(&entity) {
            viewers.retain(|v| v.id != player);
            if viewers.is_empty() {
                self.entity_viewers.remove(&entity);
            }
        }
    }
}
