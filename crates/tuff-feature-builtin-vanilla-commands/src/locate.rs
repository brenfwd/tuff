//! `/locate`: the nearest structure or biome. The search runs as a rayon
//! job and a detached task delivers its feedback, so no actor waits on it.

use std::panic::{AssertUnwindSafe, catch_unwind};

use tokio::sync::oneshot;

use tuff_vanilla_data::registry::PointOfInterestType;

use tuff_core::command::{CommandError, Value};
use tuff_datapack::tags::TagSet;
use tuff_item::text::{ClickEvent, TextComponent};
use tuff_world_model::BlockPos;

use super::{Context, Executor, executor_future, feedback, text};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Structure,
    Biome,
}

impl Target {
    pub fn name(self) -> &'static str {
        match self {
            Self::Structure => "structure",
            Self::Biome => "biome",
        }
    }
}

/// What a `/locate` argument names: the ids of a tag's members in `tags`,
/// or the one id; `None` when it names nothing.
pub fn members(target: Target, name: &str, tag: bool, tags: &TagSet) -> Option<Vec<&'static str>> {
    if tag {
        let registry = match target {
            Target::Structure => "minecraft:worldgen/structure",
            Target::Biome => "minecraft:worldgen/biome",
        };
        return tags.get(registry, name).map(|ids| {
            ids.iter()
                .filter_map(|id| members(target, id, false, tags))
                .flatten()
                .collect()
        });
    }
    match target {
        Target::Structure => tuff_vanilla_data::structure::STRUCTURES
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(n, _)| vec![*n])
            .or_else(|| {
                tuff_item::registries::current()
                    .exists("minecraft:worldgen/structure", name)
                    .filter(|exists| *exists)
                    .map(|_| vec![tuff_item::registries::intern(name)])
            }),
        Target::Biome => tuff_world_model::chunk::biome_id(name)
            .map(|id| vec![tuff_vanilla_data::data_registry::WORLDGEN_BIOME.entries[id as usize]]),
    }
}

/// Like [`members`], also resolving custom biomes by name.
pub fn biome_members(
    biomes: &tuff_world_model::biomes::Biomes,
    name: &str,
    tag: bool,
    tags: &TagSet,
) -> Option<Vec<&'static str>> {
    members(Target::Biome, name, tag, tags).or_else(|| {
        if tag {
            return None;
        }
        let id = biomes.id(name)?;
        Some(vec![biomes.name(id)])
    })
}

/// The point of interest types a `/locate poi` argument names.
fn poi_members(name: &str, tag: bool, tags: &TagSet) -> Option<Vec<PointOfInterestType>> {
    if tag {
        return tags.typed::<PointOfInterestType>(name);
    }
    PointOfInterestType::from_name(name).map(|kind| vec![kind])
}

/// A `resource_or_tag` argument naming a tag that is not loaded.
pub(crate) fn unknown_tag(name: &str, registry: &str) -> CommandError {
    CommandError::dynamic(
        "argument.resource_tag.not_found",
        [name.to_owned(), registry.to_owned()],
    )
}

/// The closest point of interest of `kinds` within 256 blocks, from the
/// points of interest of loaded chunks in every region the search reaches.
async fn closest_poi(
    shared: &tuff_core::world::Shared,
    dimension: tuff_core::world::dimension::DimensionId,
    kinds: Vec<PointOfInterestType>,
    from: BlockPos,
) -> Option<(BlockPos, PointOfInterestType)> {
    const RADIUS: i32 = 256;
    let (reply, answer) = oneshot::channel();
    shared
        .manager
        .send(tuff_core::world::messages::ManagerMessage::Regions { reply })
        .await
        .ok()?;
    let regions = answer.await.unwrap_or_default();
    let low = tuff_world_model::pos::ChunkPos::of_block(BlockPos::new(
        from.x - RADIUS,
        0,
        from.z - RADIUS,
    ))
    .region();
    let high = tuff_world_model::pos::ChunkPos::of_block(BlockPos::new(
        from.x + RADIUS,
        0,
        from.z + RADIUS,
    ))
    .region();
    let mut best: Option<(i64, BlockPos, PointOfInterestType)> = None;
    for region in regions {
        let pos = region.pos;
        if region.dimension != dimension
            || pos.x < low.x
            || pos.x > high.x
            || pos.z < low.z
            || pos.z > high.z
        {
            continue;
        }
        let (reply, answer) = oneshot::channel();
        let asked = region
            .request(tuff_core::world::messages::RegionRequest::Run(
                tuff_core::world::region_api::RegionTask::new({
                    let kinds = kinds.clone();
                    move |world| {
                        let _ = reply.send(nearest_poi(world, &kinds, from, RADIUS));
                    }
                }),
            ))
            .await;
        if !asked {
            continue;
        }
        if let Ok(Some((at, kind))) = answer.await {
            let d = |a: i32, b: i32| i64::from(a - b).pow(2);
            let distance = d(at.x, from.x) + d(at.y, from.y) + d(at.z, from.z);
            if best.is_none_or(|(b, ..)| distance < b) {
                best = Some((distance, at, kind));
            }
        }
    }
    best.map(|(_, at, kind)| (at, kind))
}

/// The closest point of interest of `kinds` within `radius` blocks of
/// `from` among the region's loaded chunks.
fn nearest_poi(
    world: &dyn tuff_core::world::region_api::RegionWorld,
    kinds: &[PointOfInterestType],
    from: BlockPos,
    radius: i32,
) -> Option<(BlockPos, PointOfInterestType)> {
    world
        .pois()
        .closest_first(
            from,
            radius,
            tuff_core::world::poi::Occupancy::Any,
            |kind| kinds.contains(&kind),
        )
        .into_iter()
        .next()
}

fn floor_pos(position: [f64; 3]) -> BlockPos {
    BlockPos::new(
        position[0].floor() as i32,
        position[1].floor() as i32,
        position[2].floor() as i32,
    )
}

/// The feedback distance: horizontal for structures, in 3D for biomes,
/// in the game's float math.
pub fn distance(target: Target, from: BlockPos, to: BlockPos) -> i32 {
    match target {
        Target::Structure => {
            let (dx, dz) = (to.x.wrapping_sub(from.x), to.z.wrapping_sub(from.z));
            let squared = dx.wrapping_mul(dx).wrapping_add(dz.wrapping_mul(dz));
            ((squared as f32).sqrt()).floor() as i32
        }
        Target::Biome => {
            let (dx, dy, dz) = (
                f64::from(from.x - to.x),
                f64::from(from.y - to.y),
                f64::from(from.z - to.z),
            );
            (((dx * dx + dy * dy + dz * dz) as f32).sqrt()).floor() as i32
        }
    }
}

/// The success message: the name, green clickable coordinates and the
/// distance.
fn found_message(target: Target, name: &str, from: BlockPos, pos: BlockPos) -> TextComponent {
    found_message_as(target, target.name(), name, from, pos)
}

/// The success message under `commands.locate.<kind>.success`, measured
/// and shown the way `target` is.
fn found_message_as(
    target: Target,
    kind: &str,
    name: &str,
    from: BlockPos,
    pos: BlockPos,
) -> TextComponent {
    let y = match target {
        Target::Structure => "~".to_owned(),
        Target::Biome => pos.y.to_string(),
    };
    let coordinates = TextComponent::translate(
        "chat.square_brackets",
        [TextComponent::translate(
            "chat.coordinates",
            [text(pos.x), text(&y), text(pos.z)],
        )],
    )
    .color("green")
    .click(ClickEvent::SuggestCommand(format!(
        "/tp @s {} {y} {}",
        pos.x, pos.z
    )))
    .hover_text(TextComponent::translate("chat.coordinates.tooltip", []));
    TextComponent::translate(
        format!("commands.locate.{kind}.success"),
        [text(name), coordinates, text(distance(target, from, pos))],
    )
}

/// `/locate poi`: the closest point of interest of loaded chunks within
/// 256 blocks, shown like a structure (no height, horizontal distance).
async fn locate_poi(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
    let Some(Value::ResourceOrTag { name, tag }) = ctx.arg("poi").cloned() else {
        return Err(CommandError::simple("command.unknown.argument"));
    };
    let printable = if tag {
        format!("#{name}")
    } else {
        name.clone()
    };
    let not_found = || {
        CommandError::text(TextComponent::translate(
            "commands.locate.poi.not_found",
            [text(&printable)],
        ))
    };
    let tags = ctx.shared().datapacks.contents().tags.clone();
    let kinds = match poi_members(&name, tag, &tags) {
        Some(kinds) => kinds,
        None if tag => return Err(unknown_tag(&name, "minecraft:point_of_interest_type")),
        None => return Err(not_found()),
    };
    let from = floor_pos(ctx.source.position);
    let (pos, kind) = closest_poi(ctx.shared(), ctx.source.dimension, kinds, from)
        .await
        .ok_or_else(not_found)?;
    let shown = if tag {
        format!("{printable} ({})", kind.name())
    } else {
        printable.clone()
    };
    let message = found_message_as(Target::Structure, "poi", &shown, from, pos);
    ctx.success(message, false).await;
    Ok(distance(Target::Structure, from, pos))
}

pub struct Locate;

impl Executor for Locate {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            if ctx.has("poi") {
                return locate_poi(ctx).await;
            }
            let target = if ctx.has("structure") {
                Target::Structure
            } else {
                Target::Biome
            };
            let Some(Value::ResourceOrTag { name, tag }) = ctx.arg(target.name()).cloned() else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            let printable = if tag {
                format!("#{name}")
            } else {
                name.clone()
            };
            let tags = ctx.shared().datapacks.contents().tags.clone();
            let members = match target {
                Target::Biome => biome_members(ctx.shared().dimensions.biomes(), &name, tag, &tags),
                Target::Structure => tuff_item::registries::sync_scope(
                    ctx.shared().datapacks.contents().registries.clone(),
                    || members(target, &name, tag, &tags),
                ),
            };
            if members.is_none() && tag && target == Target::Biome {
                return Err(unknown_tag(&name, "minecraft:worldgen/biome"));
            }
            let Some(members) = members else {
                let key = match target {
                    Target::Structure => "commands.locate.structure.invalid",
                    Target::Biome => "commands.locate.biome.not_found",
                };
                return Err(CommandError::text(TextComponent::translate(
                    key,
                    [text(&printable)],
                )));
            };
            let from = floor_pos(ctx.source.position);
            let dimension = ctx.shared().dimension(ctx.source.dimension);
            let (generator, shape) = (dimension.generator.clone(), dimension.shape);
            let (reply, found) = oneshot::channel();
            rayon::spawn(move || {
                let result = catch_unwind(AssertUnwindSafe(|| {
                    tuff_datapack::tags::sync_scope(tags, || match target {
                        Target::Structure => generator.locate_structure(&members, from, shape),
                        Target::Biome => generator.locate_biome(&members, from, shape),
                    })
                }));
                if let Ok(result) = result {
                    let _ = reply.send(result);
                }
            });
            let shared = ctx.shared().clone();
            let source = ctx.source.feedback_target();
            let source_name = ctx.source.display_name();
            let task = tokio::spawn(async move {
                match found.await.ok().flatten() {
                    Some((pos, id)) => {
                        let name = if tag {
                            format!("{printable} ({id})")
                        } else {
                            printable
                        };
                        let message = found_message(target, &name, from, pos);
                        feedback::success(&shared, &source, source_name, message, false).await;
                    }
                    None => feedback::fail(
                        &source,
                        TextComponent::translate(
                            format!("commands.locate.{}.not_found", target.name()),
                            [text(printable)],
                        ),
                    ),
                }
            });
            ctx.actor.track(task);
            Ok(1)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn tag_arguments_read_the_loaded_tags() {
        use tuff_datapack::tags::{TagEntry, vanilla_builders};
        let vanilla = TagSet::vanilla();
        let villages = members(Target::Structure, "minecraft:village", true, &vanilla).unwrap();
        assert!(villages.contains(&"minecraft:village_plains"));
        assert_eq!(members(Target::Biome, "a:flat", true, &vanilla), None);
        let mut builders = vanilla_builders();
        builders
            .entry("minecraft:worldgen/biome")
            .or_default()
            .insert(
                "a:flat".to_owned(),
                vec![
                    TagEntry {
                        id: "minecraft:plains".to_owned(),
                        tag: false,
                        required: true,
                    },
                    TagEntry {
                        id: "minecraft:desert".to_owned(),
                        tag: false,
                        required: true,
                    },
                ],
            );
        let pack = TagSet::resolve_all(&builders).0;
        assert_eq!(
            members(Target::Biome, "a:flat", true, &pack),
            Some(vec!["minecraft:plains", "minecraft:desert"])
        );
        assert_eq!(poi_members("a:none", true, &pack), None);
        assert!(
            poi_members("minecraft:village", true, &pack)
                .is_some_and(|kinds| kinds.contains(&PointOfInterestType::Home))
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn distances_follow_the_game() {
        let from = BlockPos::new(0, 70, 0);
        assert_eq!(
            distance(Target::Structure, from, BlockPos::new(300, 0, 400)),
            500
        );
        assert_eq!(distance(Target::Biome, from, BlockPos::new(3, 74, 0)), 5);
    }

    #[tuff_core::watchdog]
    #[test]
    fn coordinates_are_green_and_suggest_a_teleport() {
        let message = found_message(
            Target::Structure,
            "minecraft:monument",
            BlockPos::new(0, 64, 0),
            BlockPos::new(1216, 0, -1216),
        );
        let json = message.to_json();
        assert!(json.contains(r#""translate":"commands.locate.structure.success""#));
        assert!(json.contains(r#""color":"green""#));
        assert!(json.contains(
            r#""click_event":{"action":"suggest_command","command":"/tp @s 1216 ~ -1216"}"#
        ));
        assert!(json.contains(r#""hover_event":{"action":"show_text","value":{"translate":"chat.coordinates.tooltip"}}"#));
        assert_eq!(message.with[2].text, "1719");
    }
}
