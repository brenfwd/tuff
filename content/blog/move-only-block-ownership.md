---
title: Exactly-once ownership for blocks, players and entities
description: The second chapter after move-only item stacks, where tuff gives blocks, player data and entities the same rule, a resource that exists exactly once and changes hands only through named doors.
date: 2026-10-24
author: Brenden Forward
ai: "Written with Claude Fable 5.1 from the project's code and design notes, and reviewed by the author."
tags: [blocks, rust, design]
draft: true
---

The [first post](/blog/move-only-item-stacks) in this pair made an item stack a value that can be moved but not copied, created and destroyed only through doors that name their reason. That rule is about one resource. A Minecraft server has several more that can be duplicated: the blocks in the world, the data of each player, and the entities that live in regions or sit stored inside a hive or on a player's shoulder. This post applies the same idea to each of them, starting with the test that found the first block dupe.

A test of the simplest rail duper layout found a dupe in tuff. The layout is a sticky piston, a slime block in front of it and a rail on top of the slime block. The piston extends, the slime block moves one block east with the rail riding on it, and the test counts rails every tick. After the push there were two: one lying on the ground as an item at the rail's old place, one standing on the slime block at its new place.

The cause was in tuff. When a piston moves blocks, tuff first writes a moving piston at every destination, and those writes send shape updates to their neighbours; only afterwards does it write air at the places the blocks came from. In between, the rail still stands at its old place. The shape update from the moving piston written below it, the one carrying the piston's arm, reached that rail, and tuff's generic survival check on a shape update found the support gone and popped the rail. A few ticks later the moving block landed the same rail at its destination. A vanilla rail checks its support only when a neighbour changes; its shape update handles waterlogging and nothing else. That was a parity bug in tuff, fixed in one line in `crates/tuff-vanilla-common/src/placement/shape.rs`: the survival check on a shape update now skips rails. An audit of every support check in the decompiled 26.3 server against tuff's found the rail family to be the only one that lives in neighbour updates alone; repeaters and comparators pop on shape updates in both, and no other divergence turned up.

The dupe itself got a broader answer. A debug-only id assertion had caught it first, and the question it raised was why a block could be dropped by one path while another path was carrying it. The answer for blocks is the one items already had: a block leaves the world through a token that can be moved but not copied, and loot can be rolled only from that token. Once that was in place, the same question was asked of players and entities.

## Where dupes come from

Every dupe in this post is one resource existing in two places at once, or one of its transitions running twice.

| Root cause | Example | Addressed by |
| --- | --- | --- |
| The same drop happening twice | A block popped by one path while another carries it | Loot takes a `BrokenBlock` by value |
| A block both standing and moving | tuff's rail-on-slime dupe | Lift tokens and inert ghosts |
| A block and the item that paid for it | Placing the last item and dropping the stack at once | The item rides with the click; `place_from_item` |
| A block becoming an entity without leaving the world | Gravity blocks through End portals | Falling blocks and endermen hold a `HeldBlock` |
| A copy surviving a region hand-off | A piston reading a stale copy of a neighbouring region's block | Tokens travel inside the piston updates |
| Two sessions owning one player | A second login while the first session's saves are in flight; an offline edit racing a login | One `PlayerLease` per profile, handed on in request order |
| An old save landing after a newer one | An unawaited world-save barrier write after the logout save | Numbered writes; stale ones dropped |
| An entity both live and stored | A bee in a hive or a parrot on a shoulder kept as copyable NBT; one UUID alive in two regions | `StoredEntity` and `EntityClaim` |
| An entity and the item that paid for it | Placing an armor stand from the last item and dropping the stack at once | The item rides with the click; spent when the region answers |
| A resource deleted without a trace | `/kill` on a falling block; a session panicking before its final save | Drop guards, `take_held`, dirty leases |

The last row is the mirror image of a dupe. A resource that vanishes is a conservation bug too, and each design has to catch both directions.

## Blocks

### What was wrong

A block leaves the world in three ways: it breaks into drops, a piston carries it, or it becomes an entity (a falling block, primed TNT, the block an enderman picks up). It enters the world from an item, from a landing entity or from a piston. Before this work, every one of those paths read a `BlockState` from the world and acted on it: loot was rolled from a state, pistons carried a state, falling blocks and endermen held a state, and the item that paid for a placed block was consumed when the region's reply reached the session. Nothing connected the state a path read to the block's removal, so two paths could act on one block, and an entity holding a state could be killed, saved or teleported without the block going anywhere.

### What changed

The invariant is independent of the mechanism that moves the block: a block exists exactly once, as a block in the world, inside a moving piston, inside an entity, or as its drops. Everything below is a door through which a block passes from one of those states to the next, and each door is used at most once per block.

`crates/tuff-core/src/world/block_ledger.rs` defines two types. A `BrokenBlock` is a block taken out of the world to break: it holds the block state and the saved block entity data, and it is consumed by rolling its loot or by turning it into an entity. A `HeldBlock` is a block held out of the world by a moving piston or an entity: it lands, breaks or is burned exactly once. Neither derives `Clone`, both have private constructors, and both are `#[must_use]`. Three `compile_fail` doctests in the module pin this down: cloning either token, or building a `BrokenBlock` with a struct literal, does not compile. In debug builds each token carries an id and a trail of the operations applied to it, with the file and line of each.

A region takes a block out of the world through two doors in `crates/tuff-core/src/world/region/ownership.rs`. `claim(pos, Remains, ClaimReason) -> Option<BrokenBlock>` is for breaking, and it is atomic: it writes what remains in the block's place itself, with the usual update flags, and then returns the token, so holding a `BrokenBlock` always means the block is gone. `Remains` is `Air`, `Vanilla` (the fluid the block held, if any, else air), `State(s)` or `Fluid(s)`; `ClaimReason` is `Mined`, `Exploded`, `Washed`, `Popped`, `Crushed`, `Converted`, `Command` or `Entity`. `claim` returns `None` when the region does not own a block there, when only air is there, when the block has already left and nothing has been written since, or when a hook forbids the change, and in every one of those cases nothing may break and nothing may drop. The first version of `claim` left the block standing for the caller to write over; three callers depended on a later write somewhere else, so the atomic form replaced it. `take_up(pos, Remains, ClaimReason) -> Option<HeldBlock>` is the same door for an entity that will hold the block.

`lift(from) -> Option<HeldBlock>` is for pistons, and it is the one door that does not write anything, because the vacated place has to keep showing the old block until the piston's batch writes over it. `lift` records the position in a `vacated` set and returns the token. A vacated place is a ghost: a shape or neighbour update aimed at it is dropped in `run_action`, and a `claim` there finds nothing. Writing anything at the position takes it out of the set. At the end of the batch, `settle_vacated` overwrites every ghost still standing with air, because its block is already elsewhere; debug builds also panic with the positions.

The `BlockLoot` trait in `crates/tuff-core/src/world/drops.rs` used to take a `BlockState` and an optional block entity. Its `drops`, `drops_at`, `destroyed` and `exploded` methods now take a `BrokenBlock` by value, so a block's drops come at most once and a caller that reads a state from the world has no method to call. The other direction was closed too: `MintReason::BlockDrop`, the item-ledger reason for a block minted as its own item outside a loot table, is gone, and the only way a block becomes its own item is `BrokenBlock::into_item(self)`. What was left after removing `BlockDrop` turned out to be three things the game does on purpose: bone meal from a composter, a flower popping off a two-block flower that was bonemealed, and the plant taken out of a flower pot. They mint as `MintReason::Growth`. The [Minecraft Wiki's Bone Meal page](https://minecraft.wiki/w/Bone_Meal) describes the flower case as the item dropping "without destroying the original plant", and says a pink petals or wildflower block with four already on it "drops a pink petal as an item"; those stay as growth products.

tuff runs the world as region actors talking over channels, and a piston can move a block from one region into another. `BlockUpdate`, the message a region sends its neighbour about block changes, no longer derives `Clone`, and the `Piston` variant carries `lifted: Option<HeldBlock>`: when the sender owns the moved block's old place, the token travels inside the update. When the receiver owns the old place instead, it lifts the block ahead of the batch (`prelift`) and hands the token on with a `BlockUpdate::Lifted` message as the piston change at that place runs. Inside a region, the entry that tracks a moving block owns its `HeldBlock` until it lands. A moving block whose token has not arrived waits for it, up to `CARRY_WAIT` ticks; if no token comes, the moving block lands as air, because a block that never left its old place cannot appear at its new one. A token handed over for a move that never started breaks into its drops where it was going, and an update that cannot be delivered because the receiving region is gone burns the token it carries as `Orphaned`.

TNT leaves the world by becoming an entity, and until this change every path that lit it spawned the entity and removed the block without any token. `spawn_primed_tnt` now takes a `TntOrigin`: `Block(BrokenBlock)` for TNT taken out of the world, `Item` for TNT a dispenser or a hit used up, `Command` for a summon, and `Duplicated` for the door described below. Every block path claims first: fire, redstone power, a neighbouring region's update, a burning projectile, flint and steel, a dispenser lighting a block, mining unstable TNT, and the chain reaction inside an explosion. Fire needed a reorder: it used to kindle or douse the cell and then prime the TNT that stood there, and now it primes first, so the claim finds the TNT before the flames are written over it.

Falling blocks and endermen are the two entities that can turn back into a block or into an item, so they are the two that own a token. A `FallingBlock` owns an `Option<HeldBlock>`. Starting to fall goes through `take_up`, landing goes through `Effect::PlaceFallenBlock`, which carries the token to the region that writes it, breaking with a drop goes through `into_broken` and then `into_item`, and breaking without a drop burns the token as `Discarded`. Every entity exposes a `take_held` hook, and `remove_entity` burns what it returns as `Killed`, so `/kill` on a falling block destroys the block with a ledger entry. A hand-off that fails because the receiving region is unloaded burns the token as `Orphaned`; saving stores it and loading uses `HeldBlock::from_storage`; a hand-off between regions or dimensions moves the boxed entity by value, so the token travels with it. An enderman's block lives in `MobCore.carried_block` as an `Option<HeldBlock>`. Picking a block up is `Effect::TakeUpBlock`: the region calls `take_up` and hands the token to the mob, which normalises it to the block's default state, as the game does. Putting it down is `Effect::PlaceHeldBlock`; if the place is not air or the region does not own it, the token goes back to the mob. Death turns the token into a `BrokenBlock` and rolls its drops with the stand-in tool. `BrokenBlock::released`, the constructor that used to build a token from a picture of the state when one of these entities let go of its block, is gone.

A block that enters the world from an item has to spend that item exactly once, and the window in which it did not was real. A fake player that placed its last dirt block and then dropped the stack at once, before the region's reply, kept both the block and the item in 25 of 25 clicks in `tests/server/block_from_items.rs`. The fix moves the payment into the click. When a click would place a block that costs an item, the session takes the one item out of the hand and sends it inside the placement offer (`PlaceOffer::paying`). The region writes the block through `place_from_item`, the only door that writes a block from an item, and burns the stack as `Consumed` in the same call; when the click does not place, the offer is handed back to the player. A solid bucket, such as a powder snow bucket, turns into the empty bucket the same way, through a hand effect the region sends. After the fix the same test keeps both in 0 of 25 clicks, for dirt and for powder snow from a bucket. A bucket of water follows the same rule in both directions: in `tests/server/water_bucket.rs` a survival fake player right-clicks the ground with a water bucket, then picks the source back up with the empty bucket, and each click has to leave exactly one bucket of the right kind in the inventory and drop nothing.

Fluid and mob buckets had two faults of their own. A stack of three filled buckets lost two on use, where vanilla shrinks the stack by one and puts the empty bucket elsewhere in the inventory; and in the drop-the-stack race, 22 of 25 water trials and 22 of 25 fish trials kept the bucket after the fluid or the fish had been placed. A survival bucket use now moves the one bucket out of the hand into the request (`BucketUse.pay`). The region burns it when the fluid is poured or the mob released and hands the result bucket back as `HandEffect::Spent`, or hands the bucket back when the use is refused, and a stack keeps its other buckets. `tests/server/bucket_conservation.rs` checks the stack and runs the 25-trial race for lava, water and a fish.

### The doors Mojang keeps open

Vanilla has shipped duplication glitches and kept some of them. The [Minecraft Wiki's duplication tutorial](https://minecraft.wiki/w/Tutorials/Block_and_item_duplication) says primed TNT duplication "has been left in on purpose until a suitable replacement exists", documents piston-based carpet and rail dupers alongside it, and describes gravity-block duplication through End portals. [Paper disables all three](https://docs.papermc.io/paper/misc/paper-bug-fixes) and offers switches to restore them, `allow-piston-duplication` (which the Paper docs note "undesirably, also re-enables carpet and rail duping") and `allow-unsafe-end-portal-teleportation`.

tuff keeps three settings, on by default, and one more door, `block_ledger::duplicate(VanillaDupe, config, state)`:

| Setting | Default | Meaning |
| --- | --- | --- |
| `tuff.blocks.tntDuplication` | `true` | A TNT block a piston lifts may light where it stood, once, making one extra primed TNT |
| `tuff.blocks.carpetRailDuplication` | `true` | A carpet or rail a piston lifts may pop where it stood, once, making one extra drop |
| `tuff.blocks.gravityBlockPortalDuplication` | `true` | Vanilla's gravity block duplication through End portals |

The door is the only thing in the codebase that produces a token for a block that was not taken out of the world, and each use is counted in `totals().duplicated`. With a setting off, the glitch cannot happen. With it on, the extra block comes from the door, by name, while the real block still moves exactly once.

The gravity-block door is reproduced from vanilla's behaviour. When a falling block enters an End portal, vanilla ticks the removed original once more after the hop, so a copy lands or drops in the dimension it left. In tuff, a falling block that is about to settle while touching an End portal it is free to enter makes that copy through `duplicate_held`, a `HeldBlock` for a second falling block, and the original goes through the portal. One deviation: tuff makes the copy on the block's own tick while it touches the portal, and the sand has to land on the tick it first touches the portal; the original waits up to `PORTAL_WAIT_TICKS` (40) for the hop. The test drops sand into an End portal: with the setting on, two sand reach the End (one as the original, one as the copy, and `totals().duplicated` records the door); with it off, one.

The TNT and carpet-and-rail doors open for the layouts that duplicate in vanilla, and which layouts those are was measured rather than argued. The [mcdf wiki](https://mcdf.wiki.gg/wiki/Java_Edition:Item_Duplication_Methods) says that "From 1.12.2 to 26.4 Snapshot 5 (present) rails and carpet can be duped with a simple piston machine", with "slight differences between versions". `tests/world/vanilla_dupers.rs` is an opt-in harness (`TUFF_VANILLA_TESTS=1` and the EULA variable, never part of `cargo t`) that starts the real vanilla 26.3 server jar on an offline flat world, builds each layout from `tests/world/dupe_designs.rs` over the console, runs it and records the counts; eleven layouts take about 125 seconds. `tests/world/block_dupers.rs` then builds the same layouts on tuff with the settings on and off. The layouts were derived from the mechanism in the decompiled server and checked on the real one; the wiki's observer and minecart designs were not built.

The mechanism, verified on the server: vanilla's piston first destroys the blocks in its path that break when pushed, without updates; then writes every destination as a moving piston, in reverse order, with shape updates only; and only afterwards clears the old cells. A moved block therefore still stands in its old place while the destinations are written, and updates reach it. A coral fan whose support has just become a moving piston pops, and that pop updates its neighbours: a powered TNT beside it primes while the moving TNT still lands, a rail beside it pops while the moving rail still lands, and a carpet whose support was destroyed in the first phase pops on the fan's shape update.

| Layout | Vanilla 26.3 | tuff, setting on | tuff, setting off |
| --- | --- | --- | --- |
| `fan_rail` | 1 rail becomes 2 | 2 | 1 |
| `fan_tnt` | 1 TNT becomes 1 TNT and 1 primed TNT | same | no primed TNT |
| `carpet_pot` | 1 carpet becomes 2 | 2 | 1 |
| `rail_pot` | 1 rail becomes 2 | 2 | 1 |
| `fan_carpet` | 1 carpet stays 1 | 1 | 1 |
| `rail_on_slime` | 1 rail stays 1 | 1 | 1 |
| `carpet_on_slime` | 1 carpet stays 1 | 1 | 1 |
| `fan_tripwire_hook` | 1 hook stays 1 | 1 | 1 |
| `gravity_inside_portal` | 1 sand block on the End platform and 1 sand item that hopped through | same | 1 |
| `gravity_fall_into_portal` | 1 sand stays 1 | 1 | 1 |

The rail-on-slime layout that started this post measures 1 to 1 on vanilla, which confirms that its dupe was tuff's own.

Matching the mechanism needed one more change in tuff. A piston now lifts every block it will move before it writes any destination (`RedstoneLevel::lift_pushed`, with `settle_pushed` breaking anything lifted but never moved into its drops), so a block popped by an update in between is a ghost with its token already in flight, where before it was claimed as a real block and its moving copy landed empty. With `tntDuplication` or `carpetRailDuplication` on, ghosts react to updates, and only TNT, carpet and rail outputs can be minted through the door. With both off, ghosts are inert until they are gone, and the settings documentation says what that means: a contraption that relies on a moving observer, dust or note block reacting in its old place behaves differently from vanilla. Flying machines were not compared.

The plan had a fourth setting, for tripwire hook duplicators, which Paper addresses with `skip-tripwire-hook-placement-validation` because they still worked in vanilla 1.21.4 ([PaperMC issue #12114](https://github.com/PaperMC/Paper/issues/12114)). The mcdf wiki lists hook duplication as working "From 18w48a (a 1.14 snapshot) to 26.1 Pre-Release 1", the decompiled 26.3 server's hook state calculation returns early when the partner's update removed the hook, and the `fan_tripwire_hook` layout measures one hook before and after on the real server, so there is no setting. A design published on video for later versions was not built. The older string duplication through tripwire hooks ([MC-59471](https://bugs.mojang.com/browse/MC-59471)) was fixed in 24w33a.

### How it is enforced

The types do most of it: no `Clone`, private constructors, `#[must_use]`, and a `Drop` on `HeldBlock` that panics in debug builds with the token's trail when one is dropped without landing, breaking or being burned, and in release builds logs it and counts it as lost. A `BrokenBlock` may be dropped unused, which means "no drops", because many callers break a block without drops, a creative-mode break among them.

The scenarios live in `tests/world/block_dupers.rs`. Nine layouts each build a sticky piston facing east, powered by a redstone block under it: a rail riding a slime block, a carpet riding a slime block, a rail pushed off its support, a rail dragged sideways by a slime block, two linked rails on slime and stone, sand riding a slime block, sand whose support a piston moves away, powered TNT riding a slime block, and a powered rail pushed onto air beside a powered one. The piston is cycled twice, and after every tick the test tallies each kind of block it can reach: standing in the world, carried inside a moving piston, held by a falling block or an enderman, primed as a TNT entity, or lying as a dropped item. No kind may ever outnumber what was built, and once the piston settles each must add up to it exactly. Every layout runs inside a region and again with the piston at x=510, so the moved blocks cross into the region that starts at x=512. Further tests kill a falling block mid-fall (the sand is gone and the ledger shows a burn), save and reload a world with a block mid-fall (it lands once), save, reload and kill an enderman holding dirt (one drop), and drop sand into an End portal with the duplication setting on and off.

`tests/server/block_doors.rs` is a policy test: a text scan over every crate's sources for the constructors that make a token without taking a resource from anywhere (`BrokenBlock::simulated`, `HeldBlock::from_storage`, `StoredEntity::from_storage`, the `::test` constructors, `TntOrigin::Command` and `Duplicated`, the three `duplicate` functions, and any reintroduced `MintReason::BlockDrop`). It fails when one appears outside its allowlisted file, and it checks that an item-sourced block write goes through `place_from_item`.

### What it does not cover

Block states are pictures and remain copyable, as item snapshots are. The entry that tracks a moving block holds a copy of the moved state; the token is separate, and it is what lands. A bug that lands the wrong state passes the compiler. Loot tables are not checked: a token rolled through a mistaken table drops the wrong items once, which is a bug but not a dupe.

Placement is covered for blocks that cost one item from the hand and for fluid and mob buckets. A plugin whose `use_on_block` writes a block itself but does not report the click as `Placed` gets its block without spending the item.

A few constructors build tokens without a block in the world, by design: `BrokenBlock::simulated` serves `/loot ... mine`, `HeldBlock::from_storage` reloads a moving piston, a falling block or an enderman saved with its chunk, and the `::test` constructors serve tests. The policy test keeps each at its site. Release builds do not panic; nothing is quarantined, unlike the item checks.

## Players

### What was wrong

A player's data has two writers: the session, which loads it at login and saves it during play and at logout, and offline inventory edits, which commands and the audit log's rollback apply to a player who is not online. Nothing made those writers take turns. The three obvious races were a double login with items in flight, a session panicking before its final save, and an offline edit racing a login. All three were written as tests before the fix, in `tests/world/player_lease.rs`, and all three passed before the fix: the existing login chain already ordered them.

The real gap was elsewhere. A world save writes every online player's data through a barrier, and those writes were not awaited. One could land after the session's final logout save and overwrite it with the older snapshot, restoring an inventory the player had already changed. There is no deterministic reproduction of that race; the fix is unit-tested.

### What changed

`crates/tuff-core/src/world/player_lease.rs` defines `PlayerLease`, the right to read and write one profile's data, owned by one holder at a time. The coordinator keeps a `LeaseTable` with one lease per profile ever seen. Login and offline inventory edits call `request` in call order, and the lease travels from holder to holder through a oneshot channel when the holder returns it with `release()`, so returning it never touches the coordinator and two owners never overlap. The session's load and every save go through `lease.access()`.

`LeaseAccess::write` numbers each write and drops a stale one: a write from an older sequence of the same holder, or from a previous holder's generation, returns without running, so an older snapshot cannot land after a newer one. That closes the barrier-write reorder.

A lease dropped without `release()`, which happens when a session task panics outside the play loop's unwind guard, reaches the next holder marked dirty. The next login sees `was_dirty()`, loads the last completed save, logs the event and bumps `player_lease::dirty_returns()`.

### How it is enforced

`PlayerLease` is `#[must_use]`, cannot be cloned and has no public constructor other than `detached`. Its `Drop` hands the lease on dirty, and in debug builds panics unless the thread is already unwinding or the runtime is shutting down. The three scenario tests in `tests/world/player_lease.rs` run a second login against a first session's moves, a session dying before its final save (the next login holds the last save and `dirty_returns` has grown), and offline edits racing a login (every edit lands exactly once, offline or on the session). The write ordering is unit-tested in the module.

The lease costs nothing per tick or per packet. It is one mutex per profile, taken at login, at each save and at each offline edit.

### What it does not cover

`PlayerLease::detached` builds a lease outside any table for harness sessions that load and save without a coordinator, and dropping it is silent. It is the one lease nothing orders, and confining it is open work. A dirty lease is logged and counted; the player is not quarantined, because the quarantine from the item checks is keyed by live session id.

## Entities

### What was wrong

Live entities already moved by value between regions as a `Box<dyn RegionEntity>`, and a failed hand-off returned the box. Their identity did not. The claim on an entity's UUID was an entry in a global `Mutex<HashSet<u128>>`; `add_entity` ignored the result of claiming it, a hand-off into a region did not check it, `/summon` checked only its own region, and only loading a chunk rejected a duplicate. Two tests in `tests/world/cross_region.rs` written against that code, `the_same_uuid_cannot_live_in_two_regions` and `an_entity_handed_over_with_a_taken_uuid_is_dropped`, both failed: a second copy lived. Entities stored as data were plain, cloneable NBT, bees in hives and parrots on shoulders among them. And items that make an entity when used spent their item on the session's reply, the same window a placed block had before `place_from_item`: in `tests/server/entity_from_items.rs`, an armor stand, a boat, a pig spawn egg and an axolotl bucket each kept the item and made the entity in 25 of 25 clicks when the stack was dropped right after the click.

### What changed

`crates/tuff-core/src/world/session/paid_entities.rs` takes the one paying item out of the hand when the request is sent and spends it, or hands it back, when the region's answer arrives; payments still in flight go back to the inventory when the player leaves. It covers armor stands, boats, minecarts, item frames, paintings, end crystals, spawn eggs and mob buckets. A spawn egg used on an animal already moved the whole hand stack to the region, so it was outside the window.

`crates/tuff-core/src/world/entity_claim.rs` replaces the global set with `EntityUuids`, sharded sixteen ways by UUID, and `EntityClaim`, a move-only guard over one UUID. The live entity holds its claim in a `ClaimSlot`, a stored entity carries it, dropping it frees the UUID, and a hand-off moves it with the box, so no removal path has to remember a release and none can forget one. `add_entity` drops an entity whose UUID is already alive and burns its items; `/summon` checks the shared set.

`crates/tuff-core/src/world/stored_entity.rs` defines `StoredEntity`, an entity that is out of the world as data. It is move-only, produced only by `take_entity` on a live entity or by the `from_storage` load door, and it ends by `into_spawn_data`, `into_storage` or `burn(EntityBurn)`. Shoulders hold one per parrot, and a hive's occupants cross the hive's raw block entity data only through a typed `Occupant` that uses the storage doors; they are moved out of the list, never cloned. Mob buckets store a reduced subset of the mob's data with no UUID, and trial spawners hold only spawn templates and UUID references, so neither needed a holder.

### How it is enforced

`EntityClaim` and `StoredEntity` are `#[must_use]` and cannot be cloned. Dropping a `StoredEntity` unspent panics in debug builds; release builds carry no extra state beyond the data and the claim. `stored_entity::totals()` counts taken, loaded, spawned, stored and burned in debug builds and lost in every build. The policy test confines `StoredEntity::from_storage` to the session's persistence code and the hive. The two cross-region UUID tests pass, and the four entity-from-items tests keep item and entity together in 0 of 25 clicks.

### What it does not cover

No dupe was found for hives, which give a bee a fresh UUID on release, or for a parrot on a shoulder across a dimension change; those paths were converted for the invariant, not for a measured bug. Items are the item ledger's business once a mob is in a bucket, since the bucket stores no UUID.

## Across resources

Four things are shared. Each resource has a ledger with totals, a trail of operations in debug builds, and a drop guard that panics in debug builds and logs and counts in release. Each has a small set of fabricating doors, constructors that make a token without taking a resource from anywhere, and `tests/server/block_doors.rs` confines them to the files that need them, so a new one cannot slip in quietly. The vanilla duplication settings are decided against vanilla itself: every design is run on a real 26.3 server and on tuff and compared by count.

The fourth is how losses are counted in tests. A drop guard on a thread that is already unwinding cannot panic; it only counts, and the count went to a process-wide total that every test in a binary adds to, so a loss inside a tokio task that swallowed its panic failed some other test with no trace. Every `#[tuff_core::watchdog]` test now owns a loss scope: the macro builds the tokio runtime itself so that every worker and blocking thread enters the scope, and the test fails on the items its own threads lost, with a report for each. The scope found two real losses the old counter had hidden. A session that panicked while leaving dropped its whole inventory unaccounted; leaving now runs under the same unwind guard as entering and playing, the session's state is released explicitly and its lease goes to the next holder dirty. And entities that had arrived for a chunk still being generated when the server stopped were discarded, dropped items and mobs included; they are now written to the chunk's entity record so the next load restores them, under the test `chunk_lifecycle::an_item_arriving_for_a_generating_chunk_is_back_after_a_restart`.

## In the diff

The tokens in `crates/tuff-core/src/world/block_ledger.rs` have private fields and no `Clone`, and the module's doctests make the compiler prove it:

```rust
//! ```compile_fail
//! use tuff_core::world::block_ledger::HeldBlock;
//! fn copy(block: &HeldBlock) -> HeldBlock {
//!     block.clone()
//! }
//! ```

#[must_use = "a broken block is rolled into its drops, turned into an entity, or dropped for none"]
#[derive(Debug)]
pub struct BrokenBlock {
    state: BlockState,
    block_entity: Option<Compound>,
    trail: Trail,
}

#[must_use = "a held block must land, break or be burned"]
pub struct HeldBlock(Option<Held>);
```

The loot trait in `crates/tuff-core/src/world/drops.rs` stopped accepting a state read from the world:

```diff
     fn drops(
         &self,
-        state: BlockState,
+        block: BrokenBlock,
         tool: Option<AnyItem<'_>>,
-        block_entity: Option<&Compound>,
         rng: &mut StdRng,
     ) -> Vec<ItemStack>;
```

Popping a block that can no longer stay where it is, in `crates/tuff-core/src/world/region/redstone.rs`, first went through `claim` and still wrote the remains afterwards. With an atomic claim the write is part of taking the token, and the trailing write is gone:

```diff
-        let Some(broken) = self.claim(pos, crate::world::block_ledger::ClaimReason::Popped) else {
+        let Some(broken) = self.claim_limited(
+            pos,
+            crate::world::block_ledger::Remains::Vanilla,
+            crate::world::block_ledger::ClaimReason::Popped,
+            limit,
+        ) else {
             return;
         };
         self.break_event(pos, old, None);
@@
             let stacks = self.shared.block_loot().destroyed(broken, &mut self.rng);
             self.drop_items(pos, stacks);
         }
-        self.set_block_flags(pos, placement::remains(old), redstone::UPDATE_ALL, limit);
```

In `crates/tuff-core/src/world/region/pistons.rs`, a moving block that finishes its move lands the token its entry holds, and nothing if none came:

```rust
            moving.moved = self.landing(pos, &moving, held);
            if is_air(moving.moved) {
                RedstoneLevel::set_block(self, pos, AIR, ARRIVE);
                self.land_carried(pos, piston);
                RedstoneLevel::neighbor_changed(self, pos, Block::Air);
                return false;
            }
            let new = self.shape_from_neighbours(moving.moved, pos);
```

The two hooks in `crates/tuff-core/src/world/redstone.rs` that let a piston lift what it will move before it writes anything:

```rust
    /// The blocks at `from` are about to be moved by a piston: they stand on
    /// as ghosts until their moves start (vanilla leaves them in place while
    /// the destinations are written).
    fn lift_pushed(&mut self, _from: &[BlockPos]) {}
    /// The piston's moves all started: a block lifted but never moved goes
    /// back to its drops.
    fn settle_pushed(&mut self) {}
```

The rail parity fix in `crates/tuff-vanilla-common/src/placement/shape.rs`:

```diff
-    if can_survive(updated, pos, level) || block_support::defers_break(updated.block()) {
+    if can_survive(updated, pos, level)
+        || block_support::defers_break(updated.block())
+        || tuff_datapack::tags::is(tags::key::RAILS, updated.block())
+    {
```

The one door that writes a block from an item, in `crates/tuff-core/src/world/region/blocks.rs`:

```rust
    pub(crate) fn place_from_item(
        &mut self,
        pos: BlockPos,
        state: BlockState,
        paid: Option<tuff_item::item::ItemStack>,
    ) {
        self.put_block(pos, state);
        if let Some(paid) = paid {
            paid.burn(tuff_item::item::BurnReason::Consumed);
        }
    }
```

The player lease in `crates/tuff-core/src/world/player_lease.rs`, and the write that refuses to run when a later holder or a newer write owns the data:

```rust
#[must_use = "a lease must be returned with release()"]
pub struct PlayerLease {
    uuid: u128,
    gate: Arc<Gate>,
    generation: u64,
    dirty: bool,
    next: Option<oneshot::Sender<PlayerLease>>,
}

    pub fn write(&self, f: impl FnOnce()) -> bool {
        let mut state = self.gate.lock();
        if state.generation != self.generation || state.written > self.sequence {
            return false;
        }
        state.written = self.sequence;
        f();
        true
    }
```

## What it costs

One ordering changed for blocks. A claim writes the remains before the drops spawn, so a broken block's drops appear after its removal's updates; only the order of the item entity and the block updates differs.

The accounting is built to cost nothing in release builds. The per-operation counters of the block and item ledgers (lifted, landed, broken, burned, loaded, stored; minted, burned, loaded) are an `OpCounter`, a type that wraps an `AtomicU64` in debug builds and is zero-sized in release builds, because a counter bumped by every region thread on every operation would cost more than it tells. Only `lost` and `duplicated`, which are rare, stay as release counters. The debug trail is zero-sized in release. The moving-piston entry owns its held block instead of a second map, and `written_over`, which runs on every block write, returns at once unless a lift or a move is in flight. A release-build test pins the sizes: `Trail` and `OpCounter` are zero bytes, a `BrokenBlock` is no larger than a `BlockState` and an `Option<Compound>`, and a `HeldBlock` is at most twice a `BlockState`, because `BlockState` has no niche for the `Option` to use. The player lease is touched at login, saves and offline edits only.

The stress benchmark (`bench-stress-mixed-50`, one run of each head back to back) was taken after the first block ownership commit and before the rest of the work. It gave main at `ebf9a3b` 15.12 TPS with a 66.1 ms mean tick, 151.7 ms p99 and 2491 MiB resident, and `17e9feb` 16.00 TPS, 62.4 ms, 136.0 ms and 2459 MiB. Earlier runs of main on the same busy machine ranged from 13.5 to 15.1 TPS, so the difference is within run-to-run noise: no measurable regression.

A second benchmark was written for this work, `benches/tnt_cube.rs`: a cube of TNT on the corner where four regions meet, in a vanilla-generated overworld with seed 1 chosen so that none of the 16 regions around the corner is ocean, lit on its top face by a fake player with flint and steel, run until no TNT block and no primed TNT remain. At the default size of 100 blocks a side, 1,000,000 TNT at y 149 to 248 by the region corner near block -1536, -1536, main reached 762,039 primed TNT 15 ticks after ignition and then advanced about three ticks in twenty minutes, so that run was stopped.

A before-and-after comparison that includes the ownership steps has not been run yet.

The code cost is smaller than the item change was. The block work is nineteen commits; the first touched 28 files (1310 insertions, 168 deletions). The player lease is two commits touching 13 files (701 insertions, 47 deletions), 301 of the insertions the lease module and 322 its tests. The entity work is three commits.

## Prior art, and what is new

Paper's approach to blocks is the nearest. Its old [rails dupe patch](https://github.com/PaperMC/Paper-1.7/blob/master/Spigot-Server-Patches/0035-Temporary-fix-for-rails-dupe.patch) skipped the drop when the block was already air, and the [bug-fixes page](https://docs.papermc.io/paper/misc/paper-bug-fixes) explains that its restore switches exist because communities want the TNT behaviour back. The switch that restores TNT duplication restores carpet and rail duplication with it; Paper has one switch for the three piston cases.

The move-only resource model is the one the [item-stacks post](/blog/move-only-item-stacks) describes, with its sources: Move's resources, Austral's linear types, and the must-move discussion that explains why Rust needs a runtime drop guard to close the gap. The player lease is a lease in the ordinary distributed-systems sense: one holder at a time, the hand-over ordered by a queue, and a numbered write refused when an older holder sends it.

What tuff adds is the same invariant applied to every resource the server has, each enforced by a type: an item stack, a block, a player's data and an entity exist exactly once, every path that moves one goes through the same few doors and hands over the same token, and the item that pays for a placed block or a spawned entity travels with the click that spends it. The vanilla glitches are named, counted uses of one more door. That is why turning a switch off makes the glitch impossible, why the switch for TNT and the switch for carpets and rails are separate, and why each door opens only for the layouts that were measured on vanilla itself.
