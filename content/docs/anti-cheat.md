---
title: Anti-cheat and anti-xray
description: Packet checks, enforcement settings and anti-xray.
group: Operating
source: docs/anti-cheat.md
---

The anti-cheat plugin checks player packets: movement, reach, rates, item
and inventory integrity, and vehicles. It runs by default, but only logs:
checks count violations and alert, and never set a player back or kick
unless `tuff.anticheat.enforce` is `true`. With enforcement on, a failed
check sets the action back, and a player whose violations pass a check's
kick limit is kicked only when `tuff.anticheat.kickThreshold` is above 0.

| Key | Default | Effect |
|-----|---------|--------|
| `tuff.anticheat.enabled` | `true` | Run the checks |
| `tuff.anticheat.enforce` | `false` | `false`: checks run, count and alert but never set back or kick. `true` enables setbacks (and kicks with `kickThreshold`) |
| `tuff.anticheat.exemptOperators` | `true` | Skip operators |
| `tuff.anticheat.exemptCreative` | `false` | Skip players in creative mode |
| `tuff.anticheat.exemptSpectators` | `false` | Skip spectators |
| `tuff.anticheat.alert` | `true` | Log violations, setbacks and kicks to the console |
| `tuff.anticheat.alertOperators` | `true` | Tell online players with `tuff.anticheat.alerts` in chat, at most once per check and player per second |
| `tuff.anticheat.kickThreshold` | `0` | Multiplier on every kick limit; `0` never kicks and only sets back, `1.0` is the designed limit, larger is more lenient |
| `tuff.anticheat.setbackFallDamage` | `true` | A setback drop counts as falling |
| `tuff.anticheat.kickMessage` | "Kicked for suspicious activity" | The kick text |

Each check also has `tuff.anticheat.check.<name>.enabled`, `.kickAbove`
(its kick limit, multiplied by `kickThreshold`), `.decay` and `.action`
(`setback` or `log`), for example `tuff.anticheat.check.reach.kickAbove`;
`/tuff config` completes the names. The vehicle checks are `vehicle_fly`
(a boat rising or hovering without water or support under it),
`vehicle_speed` (boats and ridden mobs), `vehicle_phase` (a vehicle moving
through blocks), `vehicle_timer` (vehicle moves sent too often) and
`vehicle_control` (moves for a vehicle the player does not steer). Each
answers with a setback and can kick like any other check, except
`vehicle_control`, which only resends the vehicle's position and in
practice never reaches its kick limit. The `/anticheat` command and the
panel's Anticheat page show flags and kicks.


## Anti-xray

Anti-xray sends chunks with valuable blocks nobody can see replaced, and
sends the true blocks once they are exposed or mined towards. It is off by
default; turn it on with `/tuff config tuff.antiXray.enabled true`.

| Key | Default | Effect |
|-----|---------|--------|
| `tuff.antiXray.mode` | `hide` | `hide` sends hidden blocks as a nearby stone-like block; `fill` sends every enclosed block as a random hidden one |
| `tuff.antiXray.maxY` | `64` | Highest y at which blocks are hidden |
| `tuff.antiXray.updateRadius` | `2` | Blocks (1 to 4) around a change whose true blocks are resent |
| `tuff.antiXray.<dimension>.hiddenBlocks` | ores and similar | Blocks to hide; empty turns anti-xray off in that dimension |
| `tuff.antiXray.<dimension>.replacementBlocks` | stone-like blocks | Blocks sent in their place |

`<dimension>` is `overworld`, `the_nether` or `the_end`; other dimensions
use the overworld's lists.
