use tuff_core::world::dimension::DimensionId;
use tuff_core::world::messages::{ManagerMessage, RegionRequest, Teleport, TimedTicket};
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

use super::Context;
use super::targets::Target;

/// `/tp` on entities other than players: their owning region is not known
/// here, so this broadcasts to every running region (like `/kill` does for
/// entities) and lets each pick out the ids it owns. Entities bound for
/// another dimension are taken out of their region and summoned from
/// their saved form at the destination. Returns how many actually moved.
pub(crate) async fn teleport_entities(
    ctx: &mut Context<'_>,
    targets: &[&Target],
    teleport: &Teleport,
) -> usize {
    if targets.is_empty() {
        return 0;
    }
    let (staying, leaving): (Vec<&Target>, Vec<&Target>) = targets
        .iter()
        .partition(|t| t.dimension() == teleport.dimension);
    let staying: Vec<i32> = staying.iter().map(|t| t.id()).collect();
    let leaving: Vec<i32> = leaving.iter().map(|t| t.id()).collect();
    let (reply, regions) = tokio::sync::oneshot::channel();
    let _ = ctx
        .shared()
        .manager
        .send(ManagerMessage::Regions { reply })
        .await;
    let mut moved = 0;
    let mut taken = Vec::new();
    let mut held: Vec<(Compound, [f64; 3])> = Vec::new();
    for region in regions.await.unwrap_or_default() {
        if !staying.is_empty() {
            let (ids, teleport) = (staying.clone(), teleport.clone());
            let (count, refused) = region
                .ask(move |world| {
                    let mut count = 0;
                    let mut refused = Vec::new();
                    for id in ids {
                        let origin = world.entity(id).map(|t| t.entity.base().position);
                        if world.teleport_entity(id, &teleport) {
                            count += 1;
                        } else if let (Some(origin), Some(nbt)) =
                            (origin, world.take_teleported_entity(id, &teleport))
                        {
                            refused.push((nbt, origin));
                        }
                    }
                    (count, refused)
                })
                .await
                .unwrap_or_default();
            moved += count;
            held.extend(refused);
        }
        if !leaving.is_empty() {
            let (ids, teleport) = (leaving.clone(), teleport.clone());
            taken.extend(
                region
                    .ask(move |world| {
                        ids.into_iter()
                            .filter_map(|id| world.take_teleported_entity(id, &teleport))
                            .collect::<Vec<_>>()
                    })
                    .await
                    .unwrap_or_default(),
            );
        }
    }
    for nbt in taken {
        if arrive(ctx, teleport, nbt).await {
            moved += 1;
        }
    }
    for (nbt, origin) in held {
        if arrive(ctx, teleport, nbt.clone()).await {
            moved += 1;
            continue;
        }
        let mut nbt = nbt;
        nbt.insert(
            "Pos",
            Tag::List(origin.iter().map(|v| Tag::Double(*v)).collect()),
        );
        restore(ctx, teleport.dimension, origin, nbt).await;
    }
    moved
}

/// Puts back an entity taken for a `/tp` that could not land, so it is
/// never lost.
async fn restore(ctx: &mut Context<'_>, dimension: DimensionId, origin: [f64; 3], nbt: Compound) {
    let Some(region) = ctx
        .actor
        .region(dimension, ChunkPos::of_position(origin).region())
        .await
    else {
        return;
    };
    let (reply, summoned) = tokio::sync::oneshot::channel();
    if region
        .request(RegionRequest::Summon {
            nbt,
            position: origin,
            fresh: false,
            reply,
        })
        .await
    {
        let _ = summoned.await;
    }
}

/// Summons an entity taken for a cross-dimension `/tp` at its new place.
async fn arrive(ctx: &mut Context<'_>, teleport: &Teleport, nbt: Compound) -> bool {
    let position = match nbt.get("Pos") {
        Some(Tag::List(pos)) => match pos.as_slice() {
            [Tag::Double(x), Tag::Double(y), Tag::Double(z)] => [*x, *y, *z],
            _ => teleport.position,
        },
        _ => teleport.position,
    };
    let region_pos = ChunkPos::of_position(position).region();
    let _ = ctx
        .shared()
        .manager
        .send(ManagerMessage::Hold {
            dimension: teleport.dimension,
            center: ChunkPos::of_position(position),
            ticket: TimedTicket::Portal,
        })
        .await;
    let Some(region) = ctx.actor.region(teleport.dimension, region_pos).await else {
        return false;
    };
    let (reply, ready) = tokio::sync::oneshot::channel();
    if !region
        .request(RegionRequest::AwaitChunk {
            pos: BlockPos::containing(position),
            reply,
        })
        .await
        || tokio::time::timeout(std::time::Duration::from_secs(30), ready)
            .await
            .is_err()
    {
        return false;
    }
    let (reply, summoned) = tokio::sync::oneshot::channel();
    region
        .request(RegionRequest::Summon {
            nbt,
            position,
            fresh: false,
            reply,
        })
        .await
        && summoned.await.unwrap_or(false)
}
