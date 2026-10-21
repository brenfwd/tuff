---
title: Pre-generating chunks
description: Generating terrain ahead of players.
group: Operating
source: README.md
---

`/tuff chunk generate <x> <z> <radius> circle|square [--force-regenerate]`
generates chunks in the source's dimension; run it with
`/execute in <dimension> run ...` to target another dimension. `x`, `z`
and `radius` are in blocks, and `radius` is 0 to 100000. A chunk is
included when any of its blocks is within the radius: for `circle` that
means a straight-line distance, for `square` the larger of the two axis
distances. Chunks beyond the 30,000,000-block world limit are skipped.

Chunks are generated in a spiral out from the centre, each one fully (the
same as a player loading it), then saved and unloaded. Chunks a player is
loading take priority. `tuff.pregen.parallel` sets how many chunks
generate at once (0 to 1024); `0`, the default, means twice the number of
worker threads, at least 4 and at most 128. `tuff.pregen.maxPriority`
(default `false`) raises that to eight times the worker threads (32 to
1024) for an otherwise idle server, at the cost of slower world ticks.

`--force-regenerate` first deletes everything in every chunk in the shape
(blocks, entities, block entities and points of interest), shown as a
"Removing" phase, then generates them again. Chunks just outside the shape
are left as they are, so a feature that crosses the edge may no longer
match on both sides.

`/tuff chunk status` reports progress, the oldest chunks still in flight
and what each is waiting on, and a summary of the last job.
`/tuff chunk cancel` stops the job after the chunks already in flight
finish. Only one job runs at a time. A chunk that has not finished after
300 seconds counts as failed, so a job always ends.

A player with the `tuff.command.chunk` permission (operator level 4 by
default) sees the job's progress on a boss bar: chunks done and total, the
percentage, the rate and an ETA.

With the `world-map` feature on, the panel's World map page can draw the
same shape, as a circle or a square, by dragging its centre or its handles
or by typing a radius. It shows the chunk count and starts the same job
with the Generate button; force-regenerate asks for confirmation first.
Starting or cancelling a job needs `map.generate` (level 4); `map.view`
(level 2) is enough to watch progress. New chunks appear on the map as
they are saved.
