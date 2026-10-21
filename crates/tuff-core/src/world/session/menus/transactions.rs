//! Settling clicks on menus whose slots the session does not own alone.
//!
//! A click on a block or entity container, or on a merchant's screen, is
//! worked out on pictures ([`Sim`]): template copies of the screen and
//! the inventory. It then settles by moving real stacks: every place on
//! the session's side that loses items gives them up into a pool, the
//! slots another actor owns put what they lose into that pool and take
//! what they gain out of it, and the session's side fills what it gains
//! from what comes back. Items appear or vanish only where the click has a
//! reason to make or use them up (a trade, creative mode), checked before
//! anything moves.

use super::*;
use crate::world::menu::settle::{self, Balance};
use tuff_item::item::{ItemSnapshot, MintReason};

/// A place on the session's side a click can change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    Slot(usize),
    Carried,
    /// A slot of the open screen this session owns (a merchant's payment).
    Screen(usize),
}

/// A click worked out on pictures: the screen and inventory as it leaves
/// them, and what it throws. Its stacks are templates, never items.
pub struct Sim {
    pub inventory: Inventory,
    pub container: Vec<Option<ItemStack>>,
    pub drops: Vec<ItemStack>,
}

impl Sim {
    pub fn of(inventory: &Inventory, container: &[Option<ItemStack>]) -> Self {
        Self {
            inventory: inventory.template_copy(),
            container: container.iter().map(template).collect(),
            drops: Vec::new(),
        }
    }

    fn place(&self, place: Place) -> Option<&ItemStack> {
        match place {
            Place::Slot(slot) => self.inventory.slots().get(slot)?.as_ref(),
            Place::Carried => self.inventory.carried(),
            Place::Screen(slot) => self.container.get(slot)?.as_ref(),
        }
    }

    /// Lets the pictures go.
    pub fn discard(self) {
        let stacks = self
            .inventory
            .into_stacks()
            .chain(self.container.into_iter().flatten())
            .chain(self.drops);
        for stack in stacks {
            stack.burn(BurnReason::Template);
        }
    }
}

/// Why the items a click makes come into existence and the items it uses
/// up leave it.
#[derive(Clone, Copy, Debug)]
pub struct Reasons {
    pub mint: MintReason,
    pub burn: BurnReason,
}

/// A template copy of a slot, for pictures that work like stacks.
pub fn template(slot: &Option<ItemStack>) -> Option<ItemStack> {
    slot.as_ref()
        .map(|stack| ItemStack::mint_like(stack, stack.count(), MintReason::Template))
}

/// Template stacks showing what `pictures` show, for a screen mirroring
/// slots another actor owns.
pub fn mirror_of(pictures: &[Option<ItemSnapshot>]) -> Vec<Option<ItemStack>> {
    pictures
        .iter()
        .map(|slot| {
            slot.as_ref()
                .map(|shown| ItemStack::mint_like(shown, shown.count(), MintReason::Template))
        })
        .collect()
}

/// Shows `shown` in a mirrored slot.
fn mirror_slot(slot: &mut Option<ItemStack>, shown: Option<&ItemSnapshot>) {
    let mirrored =
        shown.map(|shown| ItemStack::mint_like(shown, shown.count(), MintReason::Template));
    if let Some(old) = std::mem::replace(slot, mirrored) {
        old.burn(BurnReason::Template);
    }
}

/// The slots of a block or entity screen that changed between the mirror
/// and the click's pictures: (screen index, expected, new).
type ScreenChanges = Vec<(usize, Option<ItemSnapshot>, Option<ItemSnapshot>)>;

impl PlayerSession {
    /// Whether clicks on the open menu are worked out on pictures: its
    /// screen mirrors slots a region owns, or a merchant must agree first.
    pub(in crate::world::session) fn menu_mirrored(&self) -> bool {
        matches!(
            self.menus.menu.as_ref().map(|menu| &menu.backing),
            Some(MenuBacking::Blocks(_) | MenuBacking::Entity(_) | MenuBacking::Merchant(_))
        )
    }

    /// The places on the session's side a click on the open menu can
    /// change.
    fn local_places(&self) -> Vec<Place> {
        let mut places: Vec<Place> = (1..crate::world::inventory::SLOTS)
            .map(Place::Slot)
            .collect();
        places.push(Place::Carried);
        if let Some(menu) = &self.menus.menu
            && matches!(menu.backing, MenuBacking::Merchant(_))
        {
            places.push(Place::Screen(menu::merchant::PAYMENT_A));
            places.push(Place::Screen(menu::merchant::PAYMENT_B));
        }
        places
    }

    fn real(&self, place: Place) -> Option<&ItemStack> {
        match place {
            Place::Slot(slot) => self.inventory.slots().get(slot)?.as_ref(),
            Place::Carried => self.inventory.carried(),
            Place::Screen(slot) => self.menus.menu.as_ref()?.container.get(slot)?.as_ref(),
        }
    }

    fn real_mut(&mut self, place: Place) -> Option<&mut Option<ItemStack>> {
        match place {
            Place::Slot(slot) => Some(self.inventory.slot_mut(slot)),
            Place::Carried => Some(self.inventory.carried_mut()),
            Place::Screen(slot) => self.menus.menu.as_mut()?.container.get_mut(slot),
        }
    }

    /// The slots of the open block or entity screen the click changed.
    fn screen_changes(&self, sim: &Sim) -> ScreenChanges {
        let Some(menu) = &self.menus.menu else {
            return Vec::new();
        };
        let remote = match &menu.backing {
            MenuBacking::Blocks(parts) => 0..parts.iter().map(|(_, size)| size).sum(),
            MenuBacking::Entity(_) => 0..menu.container.len(),
            _ => 0..0,
        };
        remote
            .filter(|&index| menu.container.get(index) != sim.container.get(index))
            .map(|index| {
                (
                    index,
                    menu.container[index].as_ref().map(ItemSnapshot::of),
                    sim.container[index].as_ref().map(ItemSnapshot::of),
                )
            })
            .collect()
    }

    /// Checks that a click balances: what every side gains is what the
    /// others lose, unless it trades (`trade`), runs in creative mode, or
    /// only rearranges items inside containers such as bundles. Answers
    /// the reasons for what it makes and uses up, or the imbalance.
    fn plan(
        &self,
        sim: &Sim,
        screen: &ScreenChanges,
        trade: bool,
    ) -> Result<(Balance, Option<Reasons>), Balance> {
        let mut losses = Vec::new();
        let mut gains = Vec::new();
        let mut count = |before: Option<&ItemStack>, after: Option<&ItemStack>| {
            let (lose, gain) = settle::delta(before, after);
            if lose > 0
                && let Some(before) = before
            {
                losses.push(ItemSnapshot::of(before).with_count(lose));
            }
            gains.extend(gain);
        };
        for place in self.local_places() {
            count(self.real(place), sim.place(place));
        }
        for (_, before, after) in screen {
            count(before.as_deref(), after.as_deref());
        }
        gains.extend(sim.drops.iter().map(ItemSnapshot::of));
        let balance = Balance::of(losses.iter(), gains.iter());
        if balance.is_even() {
            return Ok((balance, None));
        }
        let reasons = if trade {
            Reasons {
                mint: MintReason::Trade,
                burn: BurnReason::TradePayment,
            }
        } else if self.mode.abilities.instabuild {
            Reasons {
                mint: MintReason::Creative,
                burn: BurnReason::Creative,
            }
        } else if balance.rearranges() {
            Reasons {
                mint: MintReason::Transform,
                burn: BurnReason::Transform,
            }
        } else {
            return Err(balance);
        };
        Ok((balance, Some(reasons)))
    }

    /// Takes what each place on the session's side loses into a pool,
    /// with where each stack came from, then adds what the click makes.
    fn take_losses(
        &mut self,
        sim: &Sim,
        balance: &Balance,
        reasons: Option<Reasons>,
    ) -> (Vec<ItemStack>, Vec<Option<Place>>) {
        let mut pool = Vec::new();
        let mut origins = Vec::new();
        for place in self.local_places() {
            let (lose, _) = settle::delta(self.real(place), sim.place(place));
            if let Some(stack) = self
                .real_mut(place)
                .and_then(|slot| ItemStack::take_from(slot, lose))
            {
                pool.push(stack);
                origins.push(Some(place));
            }
        }
        if let Some(reasons) = reasons {
            for stack in balance.mint_short(reasons.mint) {
                pool.push(stack);
                origins.push(None);
            }
        }
        (pool, origins)
    }

    /// Fills what the session's side gains from `pool`, shows the click's
    /// pictures in the screen slots the session does not own, and throws
    /// what the click dropped. What is left of the pool was used up for
    /// `reasons`, or goes back to the player.
    async fn fill_gains(&mut self, sim: Sim, mut pool: Vec<ItemStack>, reasons: Option<Reasons>) {
        let places = self.local_places();
        for &place in &places {
            let (_, gain) = settle::delta(self.real(place), sim.place(place));
            let Some(gain) = gain else {
                continue;
            };
            if let Some(slot) = self.real_mut(place) {
                let missing = settle::gain(slot, &gain, &mut pool);
                if missing > 0 {
                    log::warn!(
                        "{}: a click's settlement was {missing} short of {gain:?}",
                        self.profile.name
                    );
                }
            }
        }
        let Sim {
            inventory,
            mut container,
            drops,
        } = sim;
        let mut thrown = Vec::new();
        for dropped in &drops {
            thrown.extend(settle::take_out(&mut pool, dropped));
        }
        if let Some(menu) = self.menus.menu.as_mut() {
            for (index, shown) in container.iter_mut().enumerate() {
                if places.contains(&Place::Screen(index)) {
                    continue;
                }
                if let Some(slot) = menu.container.get_mut(index)
                    && let Some(old) = std::mem::replace(slot, shown.take())
                {
                    old.burn(BurnReason::Template);
                }
            }
        }
        Sim {
            inventory,
            container,
            drops,
        }
        .discard();
        match reasons {
            Some(reasons) => {
                for stack in pool {
                    stack.burn(reasons.burn);
                }
            }
            None => thrown.extend(
                pool.into_iter()
                    .filter_map(|stack| self.inventory.add(stack)),
            ),
        }
        for stack in thrown {
            self.throw_item(stack).await;
        }
    }

    /// Puts stacks no transaction will take back into the inventory,
    /// throwing what does not fit.
    async fn give_back(&mut self, pool: Vec<ItemStack>) {
        if pool.is_empty() {
            return;
        }
        let left: Vec<ItemStack> = pool
            .into_iter()
            .filter_map(|stack| self.inventory.add(stack))
            .collect();
        for stack in left {
            self.throw_item(stack).await;
        }
        self.send_inventory();
    }

    /// Puts a refused transaction's pool back where it came from; what the
    /// click made for it is used up again.
    async fn restore_pool(
        &mut self,
        origins: Vec<Option<Place>>,
        pool: Vec<ItemStack>,
        reasons: Option<Reasons>,
    ) {
        let mut loose = Vec::new();
        let mut origins = origins.into_iter();
        for stack in pool {
            match origins.next() {
                Some(Some(place)) => match self.real_mut(place) {
                    Some(slot @ None) => *slot = Some(stack),
                    Some(Some(held)) => loose.extend(held.join(stack).err()),
                    None => loose.push(stack),
                },
                Some(None) => match reasons {
                    Some(reasons) => stack.burn(reasons.burn),
                    None => loose.push(stack),
                },
                None => loose.push(stack),
            }
        }
        self.give_back(loose).await;
    }

    /// Settles a click worked out on pictures. Without changes to slots a
    /// region owns it settles at once; otherwise the changed slots go to
    /// their region as a transaction carrying the pool, settled when it
    /// answers.
    pub async fn settle_click(&mut self, sim: Sim, took_result: bool, resend: bool) {
        if self.menus.menu.is_none() {
            sim.discard();
            return;
        }
        let screen = self.screen_changes(&sim);
        if screen.is_empty() && !took_result {
            self.settle_local(sim, false, resend).await;
            return;
        }
        let (balance, reasons) = match self.plan(&sim, &screen, false) {
            Ok(plan) => plan,
            Err(balance) => {
                self.refuse_click(sim, balance);
                return;
            }
        };
        let (pool, origins) = self.take_losses(&sim, &balance, reasons);
        let player = self.conn.handle.clone();
        let menu = self.menus.menu.as_mut().expect("checked above");
        menu.pending = Some(Pending {
            origins,
            sim,
            reasons,
            resend,
            merchant: None,
        });
        let window_id = menu.window_id;
        let region = menu.region.clone();
        let (changes, entity) = match &menu.backing {
            MenuBacking::Entity(entity) => (Vec::new(), Some(*entity)),
            MenuBacking::Blocks(parts) => (block_changes(parts, &screen), None),
            _ => (Vec::new(), None),
        };
        if let Some(entity) = entity {
            let request = RegionRequest::EntityTransaction {
                player,
                window_id,
                entity,
                changes: screen
                    .into_iter()
                    .map(|(slot, expected, new)| EntitySlotChange {
                        slot,
                        expected,
                        new,
                    })
                    .collect(),
                pool,
            };
            self.send_transaction(&region, window_id, request).await;
            return;
        }
        let (far, near): (Vec<ContainerChange>, Vec<ContainerChange>) = match &menu.remote {
            Some((pos, _)) => changes.into_iter().partition(|c| c.pos == *pos),
            None => (Vec::new(), changes),
        };
        let remote = menu.remote.clone().filter(|_| !far.is_empty());
        let Some((_, remote)) = remote else {
            let request = RegionRequest::ContainerTransaction {
                player,
                window_id,
                changes: near,
                pool,
                took_result,
            };
            self.send_transaction(&region, window_id, request).await;
            return;
        };
        if near.is_empty() && !took_result {
            let request = RegionRequest::ContainerTransaction {
                player,
                window_id,
                changes: far,
                pool,
                took_result,
            };
            self.send_transaction(&remote, window_id, request).await;
            return;
        }
        self.menus.txn_counter += 1;
        let txn = self.menus.txn_counter;
        let menu = self.menus.menu.as_mut().expect("checked above");
        menu.split = Some(SplitTransaction {
            txn,
            near_voted: false,
            far_voted: false,
            prepared: true,
            contents: Vec::new(),
            pool,
            near_changes: near.clone(),
            far_changes: far.clone(),
            near_taken: Vec::new(),
            far_taken: Vec::new(),
        });
        for (region, changes) in [(region, near), (remote, far)] {
            region
                .request(RegionRequest::PrepareTransaction {
                    player: player.clone(),
                    window_id,
                    txn,
                    changes,
                })
                .await;
        }
    }

    /// Sends a transaction; if its region is gone, it is refused here and
    /// its pool comes straight back.
    async fn send_transaction(
        &mut self,
        region: &RegionHandle,
        window_id: i32,
        request: RegionRequest,
    ) {
        let Err(request) = region.try_request(request).await else {
            return;
        };
        let pool = match *request {
            RegionRequest::ContainerTransaction { pool, .. }
            | RegionRequest::EntityTransaction { pool, .. } => pool,
            _ => Vec::new(),
        };
        Box::pin(self.transaction_answered(window_id, false, Vec::new(), pool)).await;
    }

    /// Settles a click whose changes all fall on the session's side.
    /// Returns false when it was refused for not balancing.
    async fn settle_local(&mut self, sim: Sim, trade: bool, resend: bool) -> bool {
        let (balance, reasons) = match self.plan(&sim, &Vec::new(), trade) {
            Ok(plan) => plan,
            Err(balance) => {
                self.refuse_click(sim, balance);
                return false;
            }
        };
        let (pool, _) = self.take_losses(&sim, &balance, reasons);
        self.fill_gains(sim, pool, reasons).await;
        if resend {
            self.send_menu_content();
        }
        true
    }

    /// Drops a click that would make or destroy items with no reason to and
    /// shows the client the screen as it is.
    fn refuse_click(&mut self, sim: Sim, balance: Balance) {
        log::warn!(
            "{}: refused a click that would make or destroy items: {balance:?}",
            self.profile.name
        );
        sim.discard();
        if let Some(menu) = self.menus.menu.as_mut() {
            menu.drag = None;
        }
        self.send_menu_content();
    }

    /// Counts one region's vote on a two-region transaction; once both
    /// voted, commits it everywhere if both prepared, filling each
    /// region's gains from the pool, else hands each region back what it
    /// took out.
    pub async fn transaction_prepared(
        &mut self,
        window_id: i32,
        txn: u64,
        prepared: bool,
        contents: Vec<(BlockPos, Vec<Option<ItemSnapshot>>)>,
        taken: Vec<ItemStack>,
    ) {
        let split = self.menus.menu.as_mut().and_then(|menu| {
            let far_pos = menu.remote.as_ref().map(|(pos, _)| *pos);
            let window = menu.window_id;
            menu.split
                .as_mut()
                .filter(|split| window == window_id && split.txn == txn)
                .map(|split| (split, far_pos))
        });
        let Some((split, far_pos)) = split else {
            match self.abandoned_vote(txn, &contents) {
                Some(region) => self.resolve_share(region, txn, false, taken).await,
                None => self.give_back(taken).await,
            }
            return;
        };
        let from_far = contents.iter().any(|(pos, _)| Some(*pos) == far_pos);
        split.prepared &= prepared;
        split.contents.extend(contents);
        if from_far {
            split.far_voted = true;
            split.far_taken.extend(taken);
        } else {
            split.near_voted = true;
            split.near_taken.extend(taken);
        }
        if !(split.near_voted && split.far_voted) {
            return;
        }
        let menu = self.menus.menu.as_mut().expect("checked above");
        let SplitTransaction {
            prepared,
            contents,
            mut pool,
            near_changes,
            far_changes,
            near_taken,
            far_taken,
            ..
        } = menu.split.take().expect("checked above");
        let near = Some(menu.region.clone());
        let far = menu.remote.as_ref().map(|(_, region)| region.clone());
        if !prepared {
            self.resolve_share(near, txn, false, near_taken).await;
            self.resolve_share(far, txn, false, far_taken).await;
            self.transaction_answered(window_id, false, contents, pool)
                .await;
            return;
        }
        pool.extend(near_taken);
        pool.extend(far_taken);
        for (region, changes) in [(near, near_changes), (far, far_changes)] {
            let mut share = Vec::new();
            for change in &changes {
                let (_, gain) = settle::delta(change.expected.as_deref(), change.new.as_deref());
                if let Some(gain) = gain {
                    share.extend(settle::take_out(&mut pool, &gain));
                }
            }
            self.resolve_share(region, txn, true, share).await;
        }
        self.transaction_answered(window_id, true, Vec::new(), pool)
            .await;
    }

    /// Aborts a two-region transaction whose menu closed before both
    /// regions voted: the player's pool comes back, a region that voted
    /// gets back what it took out, and a region yet to vote is remembered
    /// so its late vote is aborted the same way.
    pub(in crate::world::session) async fn abandon_split(
        &mut self,
        split: SplitTransaction,
        near: &RegionHandle,
        remote: Option<&(BlockPos, RegionHandle)>,
    ) {
        let SplitTransaction {
            txn,
            near_voted,
            far_voted,
            pool,
            near_taken,
            far_taken,
            ..
        } = split;
        let far = remote.map(|(_, region)| region.clone());
        if near_voted {
            self.resolve_share(Some(near.clone()), txn, false, near_taken)
                .await;
        }
        if far_voted {
            self.resolve_share(far.clone(), txn, false, far_taken).await;
        }
        if !(near_voted && far_voted) {
            self.menus.abandoned.push(AbandonedSplit {
                txn,
                far_pos: remote.map(|(pos, _)| *pos),
                near: (!near_voted).then(|| near.clone()),
                far: if far_voted { None } else { far },
            });
        }
        self.give_back(pool).await;
    }

    /// The region a late vote on an abandoned transaction came from, if
    /// it is one; forgets the transaction once every region voted.
    pub(in crate::world::session) fn abandoned_vote(
        &mut self,
        txn: u64,
        contents: &[(BlockPos, Vec<Option<ItemSnapshot>>)],
    ) -> Option<Option<RegionHandle>> {
        let index = self.menus.abandoned.iter().position(|a| a.txn == txn)?;
        let abandoned = &mut self.menus.abandoned[index];
        let from_far = contents
            .iter()
            .any(|(pos, _)| Some(*pos) == abandoned.far_pos);
        let region = if from_far {
            abandoned.far.take()
        } else {
            abandoned.near.take()
        };
        if abandoned.near.is_none() && abandoned.far.is_none() {
            self.menus.abandoned.swap_remove(index);
        }
        Some(region)
    }

    /// Ends one region's share of a two-region transaction, handing it
    /// `items`; if the region is gone, they come back to the player.
    pub(in crate::world::session) async fn resolve_share(
        &mut self,
        region: Option<RegionHandle>,
        txn: u64,
        commit: bool,
        items: Vec<ItemStack>,
    ) {
        let request = RegionRequest::ResolveTransaction {
            player: self.id,
            txn,
            commit,
            items,
        };
        let refused = match region {
            Some(region) => region
                .try_request(request)
                .await
                .err()
                .map(|request| *request),
            None => Some(request),
        };
        if let Some(RegionRequest::ResolveTransaction { items, .. }) = refused {
            self.give_back(items).await;
        }
    }

    /// Tells the region a menu is no longer viewed.
    pub(super) async fn release_menu(
        &self,
        region: &RegionHandle,
        remote: Option<&(BlockPos, RegionHandle)>,
        mut positions: Vec<BlockPos>,
        entity: Option<i32>,
    ) {
        if let Some((pos, far)) = remote {
            positions.retain(|p| p != pos);
            far.request(RegionRequest::CloseContainer {
                player: self.id,
                positions: vec![*pos],
            })
            .await;
        }
        let request = match entity {
            Some(entity) => RegionRequest::CloseEntityContainer {
                player: self.id,
                entity,
            },
            None => RegionRequest::CloseContainer {
                player: self.id,
                positions,
            },
        };
        region.request(request).await;
    }

    pub async fn entity_transaction_answered(
        &mut self,
        window_id: i32,
        accepted: bool,
        contents: Vec<Option<ItemSnapshot>>,
        pool: Vec<ItemStack>,
    ) {
        if let Some(menu) = self.menus.menu.as_mut()
            && menu.window_id == window_id
            && menu.pending.is_some()
            && !accepted
            && contents.len() == menu.container.len()
        {
            for (slot, shown) in menu.container.iter_mut().zip(&contents) {
                mirror_slot(slot, shown.as_ref());
            }
        }
        self.transaction_answered(window_id, accepted, Vec::new(), pool)
            .await;
    }

    pub fn entity_slots_changed(&mut self, entity: i32, slots: Vec<(usize, Option<ItemSnapshot>)>) {
        let Some(menu) = self.menus.menu.as_mut() else {
            return;
        };
        if menu.backing != MenuBacking::Entity(entity) {
            return;
        }
        let window_id = menu.window_id;
        let mut updates = Vec::new();
        for (slot, item) in slots {
            if slot < menu.container.len() {
                mirror_slot(&mut menu.container[slot], item.as_ref());
                updates.push((menu.next_state_id(), menu.kind.wire_slot(slot) as i16, item));
            }
        }
        for (state_id, slot, item) in updates {
            self.send(&play::ContainerSetSlot {
                window_id,
                state_id,
                slot,
                item: item.as_ref().map(ItemSnapshot::view),
            });
        }
    }

    /// A transaction's answer: accepted, the pool fills what the session's
    /// side gains; refused, it goes back where it came from and the screen
    /// shows the region's `contents`.
    pub async fn transaction_answered(
        &mut self,
        window_id: i32,
        accepted: bool,
        contents: Vec<(BlockPos, Vec<Option<ItemSnapshot>>)>,
        pool: Vec<ItemStack>,
    ) {
        let pending = match self.menus.menu.as_mut() {
            Some(menu) if menu.window_id == window_id => menu.pending.take(),
            _ => None,
        };
        let Some(pending) = pending else {
            self.give_back(pool).await;
            return;
        };
        let Pending {
            origins,
            sim,
            reasons,
            resend,
            ..
        } = pending;
        if accepted {
            self.fill_gains(sim, pool, reasons).await;
            if resend {
                self.send_menu_content();
            }
        } else {
            sim.discard();
            self.restore_pool(origins, pool, reasons).await;
            if let Some(menu) = self.menus.menu.as_mut() {
                for (pos, items) in &contents {
                    if let Some(offset) = menu.backing.offset(*pos) {
                        for (slot, shown) in items.iter().enumerate() {
                            if let Some(entry) = menu.container.get_mut(offset + slot) {
                                mirror_slot(entry, shown.as_ref());
                            }
                        }
                    }
                }
                menu.queued.clear();
                menu.drag = None;
            }
            self.send_inventory();
            self.send_menu_content();
        }
        self.after_answer().await;
    }

    /// A merchant's answer to a trade: accepted, the click settles now
    /// (what it paid is used up, what it bought is made); refused, the
    /// screen and offers show as they were.
    pub async fn trade_answered(&mut self, window_id: i32, accepted: bool) {
        let pending = match self.menus.menu.as_mut() {
            Some(menu) if menu.window_id == window_id => menu.pending.take(),
            _ => None,
        };
        let Some(Pending {
            sim,
            resend,
            merchant,
            ..
        }) = pending
        else {
            return;
        };
        let settled = if accepted {
            self.settle_local(sim, true, resend).await
        } else {
            sim.discard();
            false
        };
        if !settled {
            if let (Some(offers), Some(menu)) = (merchant, self.menus.menu.as_mut()) {
                if let Some(Station::Merchant { view, .. }) = &mut menu.station {
                    view.offers = offers;
                }
                menu.queued.clear();
                menu.drag = None;
            }
            self.send_merchant_offers();
            self.send_inventory();
            self.send_menu_content();
        }
        self.after_answer().await;
    }

    /// After a transaction settled: a menu waiting to close closes, else
    /// the clicks held back run.
    async fn after_answer(&mut self) {
        if self
            .menus
            .menu
            .as_ref()
            .is_some_and(|m| m.closing.is_some())
        {
            self.finish_menu_close().await;
            return;
        }
        while let Some(click) = self.menus.menu.as_mut().and_then(|m| m.queued.pop_front()) {
            Box::pin(self.apply_menu_click(click)).await;
            if self.menus.menu.as_ref().is_none_or(|m| m.pending.is_some()) {
                break;
            }
        }
    }

    pub fn container_slots_changed(
        &mut self,
        pos: BlockPos,
        slots: Vec<(usize, Option<ItemSnapshot>)>,
    ) {
        let Some(menu) = self.menus.menu.as_mut() else {
            return;
        };
        let Some(offset) = menu.backing.offset(pos) else {
            return;
        };
        let window_id = menu.window_id;
        let mut updates = Vec::new();
        for (slot, item) in slots {
            let index = offset + slot;
            if index < menu.container.len() {
                mirror_slot(&mut menu.container[index], item.as_ref());
                updates.push((
                    menu.next_state_id(),
                    menu.kind.wire_slot(index) as i16,
                    item,
                ));
            }
        }
        for (state_id, slot, item) in updates {
            self.send(&play::ContainerSetSlot {
                window_id,
                state_id,
                slot,
                item: item.as_ref().map(ItemSnapshot::view),
            });
        }
    }

    pub fn container_data_changed(&mut self, pos: BlockPos, data: Vec<(usize, i32)>) {
        let Some(menu) = &self.menus.menu else {
            return;
        };
        if menu.backing.offset(pos) != Some(0) {
            return;
        }
        for (property, value) in data {
            self.send_menu_data(property as i16, value as i16);
        }
    }
}

/// Screen changes of a block menu, keyed by the block entity owning each
/// slot.
fn block_changes(parts: &[(BlockPos, usize)], screen: &ScreenChanges) -> Vec<ContainerChange> {
    screen
        .iter()
        .filter_map(|(index, expected, new)| {
            let mut offset = 0;
            for (pos, size) in parts {
                if *index < offset + size {
                    return Some(ContainerChange {
                        pos: *pos,
                        slot: index - offset,
                        expected: expected.clone(),
                        new: new.clone(),
                    });
                }
                offset += size;
            }
            None
        })
        .collect()
}
