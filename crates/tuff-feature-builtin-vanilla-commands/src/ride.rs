//! `/ride <target> mount <vehicle>` and `/ride <target> dismount`. The
//! region holding the vehicle seats the passenger; a passenger in another
//! region is taken out of it and boarded from its saved form.

use tuff_core::command::CommandError;
use tuff_core::command::args::Value;
use tuff_core::command::selector::Selector;
use tuff_core::world::messages::{
    ManagerMessage, RegionHandle, RideAnswer, RideOp, RidePassenger, Teleport,
};
use tuff_item::text::TextComponent;

use super::targets::{self, Target};
use super::{Context, Executor, executor_future};

pub struct Ride;

fn selector<'a>(ctx: &'a Context<'_>, name: &str) -> Option<&'a Selector> {
    match ctx.arg(name) {
        Some(Value::Selector(selector)) => Some(selector),
        _ => None,
    }
}

async fn entity(ctx: &mut Context<'_>, name: &str) -> Result<Target, CommandError> {
    let Some(selector) = selector(ctx, name).cloned() else {
        return Err(CommandError::simple("argument.entity.notfound.entity"));
    };
    targets::select_entity(ctx, &selector).await
}

fn passenger_of(target: &Target) -> RidePassenger {
    match target {
        Target::Player(player) => RidePassenger::Player {
            handle: player.handle.clone(),
            uuid: player.uuid,
        },
        Target::Entity(entity) => RidePassenger::Entity(entity.id),
    }
}

pub(super) async fn regions(ctx: &Context<'_>) -> Vec<RegionHandle> {
    let (reply, regions) = tokio::sync::oneshot::channel();
    let _ = ctx
        .shared()
        .manager
        .send(ManagerMessage::Regions { reply })
        .await;
    regions.await.unwrap_or_default()
}

/// Asks every region in turn until one answers something other than
/// `NotHere`, with the region that answered.
async fn ask(
    ctx: &Context<'_>,
    mut op: impl FnMut() -> RideOp,
) -> Option<(RegionHandle, RideAnswer)> {
    for region in regions(ctx).await {
        let op = op();
        match region.ask(move |world| world.ride(op)).await {
            Some(RideAnswer::NotHere) | None => {}
            Some(answer) => return Some((region, answer)),
        }
    }
    None
}

pub(super) async fn vehicle_of_id(ctx: &Context<'_>, passenger: i32) -> Option<Target> {
    match ask(ctx, || RideOp::VehicleOf { passenger }).await {
        Some((_, RideAnswer::Vehicle(vehicle))) => Some(Target::Entity(*vehicle)),
        _ => None,
    }
}

pub(super) async fn player_passengers(ctx: &mut Context<'_>, vehicle: i32) -> Vec<Target> {
    let mut out = Vec::new();
    for player in super::targets::players(ctx).await {
        if vehicle_of_id(ctx, player.id)
            .await
            .is_some_and(|v| v.id() == vehicle)
        {
            out.push(Target::Player(player));
        }
    }
    out
}

async fn vehicle_of(ctx: &Context<'_>, target: &Target) -> Option<Target> {
    let passenger = target.id();
    match ask(ctx, || RideOp::VehicleOf { passenger }).await {
        Some((_, RideAnswer::Vehicle(vehicle))) => Some(Target::Entity(*vehicle)),
        _ => None,
    }
}

/// Takes the entity `passenger` out of whichever region holds it and
/// boards it on `vehicle` in `region`.
async fn board_from_afar(
    ctx: &Context<'_>,
    region: &RegionHandle,
    passenger: &Target,
    vehicle: &Target,
) -> Option<Target> {
    let teleport = Teleport {
        dimension: vehicle.dimension(),
        position: vehicle.position(),
        relative: [false; 3],
        rotation: None,
        relative_rotation: [false; 2],
        look_at: None,
    };
    let mut taken = None;
    for holder in regions(ctx).await {
        let (id, teleport) = (passenger.id(), teleport.clone());
        if let Some(Some(nbt)) = holder
            .ask(move |world| world.take_teleported_entity(id, &teleport))
            .await
        {
            taken = Some(nbt);
            break;
        }
    }
    let nbt = taken?;
    let op = RideOp::Board {
        nbt,
        vehicle: vehicle.id(),
    };
    match region.ask(move |world| world.ride(op)).await {
        Some(RideAnswer::Vehicle(vehicle)) => Some(Target::Entity(*vehicle)),
        _ => None,
    }
}

async fn mount(ctx: &mut Context<'_>, target: Target) -> Result<i32, CommandError> {
    let vehicle = entity(ctx, "vehicle").await?;
    if let Some(current) = vehicle_of(ctx, &target).await {
        return Err(CommandError::dynamic(
            "commands.ride.already_riding",
            [ctx.name_of(&target), ctx.name_of(&current)],
        ));
    }
    if matches!(vehicle, Target::Player(_)) {
        return Err(CommandError::simple(
            "commands.ride.mount.failure.cant_ride_players",
        ));
    }
    if target.id() == vehicle.id() {
        return Err(CommandError::simple("commands.ride.mount.failure.loop"));
    }
    if target.dimension() != vehicle.dimension() {
        return Err(CommandError::simple(
            "commands.ride.mount.failure.wrong_dimension",
        ));
    }
    let passenger = passenger_of(&target);
    let vehicle_id = vehicle.id();
    let answer = ask(ctx, || RideOp::Mount {
        passenger: passenger.clone(),
        vehicle: vehicle_id,
    })
    .await;
    let seated = match answer {
        Some((_, RideAnswer::Vehicle(seated))) => Some(Target::Entity(*seated)),
        Some((_, RideAnswer::Loop)) => {
            return Err(CommandError::simple("commands.ride.mount.failure.loop"));
        }
        Some((region, RideAnswer::PassengerElsewhere(_)))
            if !matches!(target, Target::Player(_)) =>
        {
            board_from_afar(ctx, &region, &target, &vehicle).await
        }
        _ => None,
    };
    let Some(seated) = seated else {
        return Err(CommandError::dynamic(
            "commands.ride.mount.failure.generic",
            [ctx.name_of(&target), ctx.name_of(&vehicle)],
        ));
    };
    let message = TextComponent::translate(
        "commands.ride.mount.success",
        [ctx.name_of(&target), ctx.name_of(&seated)],
    );
    ctx.success(message, true).await;
    Ok(1)
}

async fn dismount(ctx: &mut Context<'_>, target: Target) -> Result<i32, CommandError> {
    let passenger = passenger_of(&target);
    let answer = ask(ctx, || RideOp::Dismount {
        passenger: passenger.clone(),
    })
    .await;
    let Some((_, RideAnswer::Vehicle(vehicle))) = answer else {
        return Err(CommandError::dynamic(
            "commands.ride.not_riding",
            [ctx.name_of(&target)],
        ));
    };
    let message = TextComponent::translate(
        "commands.ride.dismount.success",
        [ctx.name_of(&target), ctx.name_of(&Target::Entity(*vehicle))],
    );
    ctx.success(message, true).await;
    Ok(1)
}

impl Executor for Ride {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let target = entity(ctx, "target").await?;
            if ctx.has("dismount") {
                dismount(ctx, target).await
            } else {
                mount(ctx, target).await
            }
        })
    }
}
