---
title: "Move-only item stacks: item conservation in the type"
description: How the tuff server made Minecraft item stacks impossible to copy in Rust, backed by a drop guard, a ledger, two-phase handoffs and journaled saves.
date: 2026-10-22
author: Brenden Forward
ai: "Written with Claude Fable 5.1 from the project's code and design notes, and reviewed by the author."
tags: [items, rust, design]
---

Nearly every item dupe in Minecraft's history has the same shape. Some piece of code copies an item stack: a menu keeps a copy for rollback, a hopper reads a snapshot from a chunk that changed underneath it, a player disconnects halfway through a transfer, a donkey crosses a chunk border with its chest open. The copy and the original both survive, and a player finds the clicks that keep both.

Servers usually fix these one at a time. Each fix closes one path, and the next dupe arrives through another, because any line of code can still write `stack.clone()`.

tuff is a Minecraft server written in Rust, and Rust has a better tool for this. A type that does not implement `Clone` can only be moved, and after a move the sender no longer has it. In tuff, an item stack is now such a type. Items move, they are not copied, and they appear or disappear only through a few doors that name their reason.

## Where dupes come from

Community wikis such as the [Java Edition duplication methods](https://mcdf.wiki.gg/wiki/Java_Edition:Item_Duplication_Methods) list and the [2b2t duplication glitch](https://2b2t.miraheze.org/wiki/Duplication_glitch) archive document hundreds of dupes. Most fall into a handful of root causes:

| Root cause | Example | Addressed by |
| --- | --- | --- |
| Player data and chunks saved at different times | 2b2t's 11/11 drop-and-disconnect dupe | Journaled saves |
| Chunk unload or save races | Book and chunk dupes | Journaled saves |
| Entity, portal and dimension transfer | Donkey, llama and minecart portal dupes | Two-phase handoff; entities carry their inventories |
| A screen still pointing at an unloaded container | Shulker, mule and minecart dupes | Two-phase handoff |
| Two live references to one stack | Item shadowing, update suppression | Move-only stacks |
| The same drop happening twice in a tick | Piston and item frame, zombie conversion | Move-only stacks |
| Unvalidated counts or crafting math | Negative counts, crafting spam | Not covered: still needs validation |

The usual response is a targeted patch. Paper's old [rails dupe patch](https://github.com/PaperMC/Paper-1.7/blob/master/Spigot-Server-Patches/0035-Temporary-fix-for-rails-dupe.patch), for example, skips the drop when the block is already air. That closes one path and leaves the mechanism in place.

## The design

The idea is the one a ledger applies to money: value moves between accounts, and it is created or destroyed only by named entries.

### A stack you cannot copy

`ItemStack` no longer derives `Clone`, and its count is private. There is no setter. Counts change through `take(n)`, `split`, `merge` and `join`, which conserve the total: `merge` returns both stacks in its error if they do not fit together. The type is `#[must_use]`, so ignoring a stack you were handed is a compiler warning.

Code that only reads items gets something that cannot be turned back into a stack. An `ItemView` borrows a stack in place. An `ItemSnapshot` is a shared, read-only picture for data that has to travel: network packets, trade offers, advancement events, the weapon in a damage source, command arguments and audit-log rows. A snapshot describes an item; it holds none.

### Mint and burn

Items enter existence through `ItemStack::mint(item, count, MintReason)` and leave through `stack.burn(BurnReason)`. Both reasons are enums, so every creation and destruction in the codebase is searchable and says why it happens. `MintReason` has `Loot`, `BlockDrop`, `Spawn`, `EntityDrop`, `Crafting`, `Processing`, `Trade`, `Remainder`, `Transform`, `Creative`, `Command` and `Plugin`, plus `Network`, `Template` and `Test` for values that are pictures of items rather than items. `BurnReason` has `Consumed`, `CraftingInput`, `TradePayment`, `Broken`, `Destroyed`, `Despawned`, `Discarded`, `Transform` and a few more. World loading uses a third, narrow door, `ItemStack::from_storage`.

### The drop guard and the ledger

Rust's type system is affine, not linear: a value can always be dropped, leaked with `mem::forget`, or lost in a panic. For an item, a silent drop is a deletion bug, the mirror image of a dupe. So `ItemStack` implements `Drop`. A counted, non-empty stack dropped without being stored, moved or burned is reported to a ledger in `tuff_item::item::ledger`, which keeps totals of items minted, burned, loaded and lost, plus violations. Tests and debug builds treat a loss as a failure. Turning the guard on surfaced a series of places that had been deleting items quietly, among them projectile hits, jukeboxes, lecterns, decorated pots, campfires and villager pickups.

### Moving items between regions

tuff runs the world as region actors that talk over channels. A transfer between two actors now carries the stack itself in the message: the owner takes it out, sends it, and the receiver either stores it or sends it back. Code working from a stale snapshot cannot produce an item, because snapshots hold no items.

Container clicks are the hard case. The player's session works out a click on pictures of the slots, then settles it with real stacks. Its losses go into a pool sent with the transaction; the region checks its expected contents, adds its own losses to the pool, fills its gains from the pool all or nothing, and returns the rest. A refusal puts the pool back where it came from. A double chest split across two regions uses two phases, the familiar two-phase commit: each half takes its losses out when it prepares and fills its gains when the transaction resolves. A click that would create or destroy items is refused unless it is a trade, happens in creative mode, or only rearranges a bundle.

### Entities that carry inventories

Chested donkeys, chest minecarts, item frames, armor stands, allays and mobs holding gear have long been dupe vectors, because an open menu and an entity changing owner (chunk unload, dimension change, region border, death) disagree about who holds the items. In tuff, entities own their contents as stacks and travel as one value with them. They hand out views, not copies. Death, shearing and breaking an armor stand move items out. A player clicking an entity offers the hand stack itself; an entity that keeps an item takes it by move, and the region returns the remainder.

### Release checks that fail safe

The type system cannot see counts split across two actors, so runtime checks cover that gap, and they run in release builds too, behind `tuff.items.conservationChecks` (on by default). Container and entity transactions check an item census. Hopper and golem moves, drops and item-entity merges across a region border check that the count before plus what arrived equals the count after plus what left.

On a violation the server never panics. It refuses the operation if it has not run yet, logs it, writes a `tuff:item_conservation` row to the audit log, and quarantines the region and player involved. A quarantined region trades no items with players and is not saved until it unloads. A quarantined player is not saved, and by default is disconnected so the client resyncs (`tuff.items.conservationKick`). The last consistent state on disk is kept and the inconsistent one in memory is never written.

### Journaled saves

A partial save is a dupe of its own: if a player is saved holding items whose source chest was not, the items exist twice after a restart. Both storage backends keep an undo journal per save epoch, a `tuff-journal/` directory beside vanilla region files and journal tables inside the SQLite world file. Before a save first overwrites a key, the old contents go to the journal, and the epoch commits once every write has landed. Opening a world rolls back unfinished epochs, and offline tools recover the journal before touching a world. A region also applies the requests already queued before it takes its save snapshot, so a transfer cannot land on one side of a save and not the other.

WASM plugins follow the same rules. Ownership cannot cross the WASM boundary, so a plugin holds a stash, a handle to stacks the host keeps for it, and moves items with host calls such as `stash-split`, `stash-merge`, `give-stash` and `take-into-stash`. The Rust SDK wraps the handle in a move-only type.

## In the diff

The core change is three lines in `crates/tuff-item/src/item.rs`:

```diff
-#[derive(Debug, Clone, PartialEq)]
+#[derive(Debug, PartialEq)]
+#[must_use = "items must be stored, moved or burned"]
 pub struct ItemStack {
     pub item: Item,
-    pub count: u8,
+    count: u8,
     pub components: Components,
+    tally: ledger::Tally,
 }
```

Putting an item into a bundle used to copy it with a new count. In `crates/tuff-item/src/item/bundle.rs` it now takes from the source and merges:

```diff
-        let mut moved = stack.clone().with_count(amount);
+        let mut moved = stack.take(amount);
         if let Some(index) = existing {
             let held = self.items.remove(index);
-            moved.count = held.count.saturating_add(amount);
+            moved = match ItemStack::merge(held, moved) {
+                Ok(merged) => merged,
+                Err((held, moved)) => {
+                    self.items.insert(index, held);
+                    moved
+                }
+            };
```

The old `saturating_add` deserves a second look: on overflow it held the count at the cap and threw the rest away. `merge` cannot do that, because it hands back whatever did not fit.

When a tipped arrow's potion wears off, `crates/tuff-feature-builtin-vanilla-combat/src/projectile/arrow.rs` used to overwrite the old stack. Now the tipped stack is burned before the plain arrow is minted, and both entries say why:

```diff
-                    self.item = ItemStack::mint(Item::Arrow, 1, MintReason::EntityDrop);
+                    std::mem::replace(&mut self.item, ItemStack::empty())
+                        .burn(BurnReason::Transform);
+                    self.item = ItemStack::mint(Item::Arrow, 1, MintReason::Transform);
```

A runtime check site in `crates/tuff-core/src/world/region/item_merge.rs` handles two item entities merging across a region border. If the counts do not balance, the stack goes back and nothing is sent (one line elided):

```diff
+        if !self.may_transfer(Transfer::BorderMerge, pos, Some(handle.pos)) {
+            return;
+        }
+        let before = u64::from(own.stack.count());
+        let stack = own.stack.take(moved);
+        let (after, sent) = (u64::from(own.stack.count()), u64::from(stack.count()));
+        if sent != u64::from(moved)
+            || !self.transfer_balanced(Transfer::BorderMerge, pos, before, 0, after, sent)
+        {
+            if let Err(stack) = own.stack.join(stack) {
+                self.spawn_item_at(stack, at, [0.0; 3], pickup_delay);
+            }
+            return;
+        }
```

The claims are backed by `tests/world/item_fuzz.rs`, which runs seeded random menu clicks (including forged mints and stale clicks, which must be refused), hopper chains, piston pushes of chests and hoppers, and merges and handoffs across a region border, checking a per-item census and the ledger's lost count after every step. Failure-injection tests fail a check mid-transfer, inject an I/O error during a save, and simulate a crash between the player write and the chunk write, then assert that the total item count survives the restart.

## What it costs

Players can see one change. When a player clicks a block that takes an item (a lectern, jukebox, campfire, chiseled bookshelf, decorated pot, composter or shelf), the session moves the item out of the hand and offers it to the region. With a one-item hand, the hand is empty for one round trip until the region answers. On a local connection this is invisible; on a laggy one the slot may flicker.

Moves are free and views are references, so no slowdown was expected. Benchmarks of the server before and after the change, a village simulation and a mixed-player stress run, showed no measurable regression.

The code cost is real. The change touched 458 files, and a copy that used to be one `.clone()` now has to be written as a mint with a reason, or restructured as a move.

## What it does not cover

Items have no per-unit identity. The design conserves counts, not provenance, so it cannot say which diamond came from where.

The types catch copies, not arithmetic. A count bug, a logic error, or a double mint through a legitimate door with a plausible reason passes the compiler and the ledger alike; unvalidated counts and crafting math still need ordinary validation. Pictures of items are unchecked by design: `Template`, `Test` and `Network` values, snapshots, and stacks nested inside NBT. Some `Transform` mints remain where vanilla really turns one item into another, such as filling buckets and bottles, washing in a cauldron, locking a compass or signing a book; each pairs a burn with a mint.

## Prior art, and what is new

None of the techniques is new on its own. [Move](https://arxiv.org/html/2004.05106) resources can be moved but not copied, cannot be silently discarded, and can be created or destroyed only by their declaring module; the Resources paper proves that final supply equals initial plus created minus destroyed. [Austral](https://borretti.me/article/introducing-austral) makes discarding a linear value a compile error, and Niko Matsakis's [must move types](https://smallcultfollowing.com/babysteps/blog/2023/03/16/must-move-types/) proposal explains why closing that gap in Rust is hard, which is why tuff needs a runtime guard. [TigerBeetle](https://docs.tigerbeetle.com/concepts/debit-credit/) states the same invariant for money, and [Jepsen's bank test](https://cljdoc.org/d/jepsen/jepsen/0.1.13/api/jepsen.tests.bank) checks that balances always sum to the total. Two-phase commit and write-ahead logging are standard database practice.

Other games mostly react after the fact. Diablo II [scanned realms for duplicate item IDs](https://gamespy-archives.quaddicted.com/sites/www.planetdiablo.com/features/articles/dupeqa/index.html), RuneScape's [party hat dupe](https://classic.runescape.wiki/w/Great_Party_Hat_Dupe) could not be traced or rolled back, and New World [shut down trading](https://www.pcgamesn.com/new-world/dupe-exploit) in 2021. Roblox's [ProfileService](https://devforum.roblox.com/t/save-your-player-data-with-profileservice-datastore-module/667805) session locking is the nearest proactive pattern, and it targets the same crash-mid-trade class as Minecraft's save desync.

As far as we could find, tuff is the first Minecraft server to make item conservation a property of the item type. Every other server we inspected lets stacks be copied freely. [Valence](https://raw.githubusercontent.com/valence-rs/valence/main/crates/valence_protocol/src/item.rs) and [Pumpkin](https://raw.githubusercontent.com/Pumpkin-MC/Pumpkin/master/crates/pumpkin-data/src/item_stack/mod.rs), both in Rust, derive `Clone` on `ItemStack`. [Bukkit's ItemStack](https://hub.spigotmc.org/javadocs/spigot/org/bukkit/inventory/ItemStack.html), which Paper inherits, implements `Cloneable`. [Cuberite](https://forum.cuberite.org/thread-3325.html) developers describe items as copyable, and [Minestom](https://minestom.net/docs/feature/items) uses immutable stacks that are shared on purpose. The closest idea is [PocketMine's inventory transactions](https://apidoc.pmmp.io/d3/de1/classpocketmine_1_1inventory_1_1transaction_1_1_inventory_transaction.html), which require a client transaction's item balance to come to zero; that is a runtime check on one path. Otherwise, protection comes from plugins such as [DupeTrace](https://github.com/darkstarworks/DupeTrace), which tag items with IDs and remove copies afterwards.

What tuff adds is the combination: a Move-style resource model and a ledger-style supply invariant applied to a Minecraft server, ownership carried across region boundaries and inside entities, and checks that quarantine state instead of crashing.

## Closing

The change replaces a stream of individual dupe fixes with one rule the compiler enforces: an item stack is moved, never copied, and it is created or destroyed only through a door that names its reason. Where the compiler cannot see, the release checks refuse the operation, quarantine the state and keep the last complete save. The cost players notice is a hand that is empty for a moment after clicking a lectern.
