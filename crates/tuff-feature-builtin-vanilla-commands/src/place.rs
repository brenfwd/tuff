//! `/place`: features, structures, jigsaw assemblies and templates. The
//! thing to place is built off the actors, then placed chunk by chunk by
//! the region that owns each chunk, so a placement spanning regions is
//! applied by each of them.

use std::sync::Arc;

use tokio::sync::oneshot;

use tuff_core::command::args::matches_resource;
use tuff_core::command::{CommandError, ParsedNode, Value};
use tuff_core::world::commands::predicates::json_of;
use tuff_core::world::dimension::DimensionId;
use tuff_core::world::features::{FeatureRef, PlaceRequest, PlannedStructure, TemplateRequest};
use tuff_core::world::place_work::Work;
use tuff_core::world::redstone::{
    UPDATE_ALL, UPDATE_CLIENTS, UPDATE_NEIGHBORS, UPDATE_SKIP_BLOCK_ENTITY_SIDE_EFFECTS,
};
use tuff_item::text::TextComponent;
use tuff_nbt::Tag;
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

use super::world::{block_pos_arg, read_blocks};
use super::{Context, Executor, SuggestInput, executor_future, text};

const FEATURE_REGISTRY: &str = "minecraft:worldgen/feature";

fn unloaded() -> CommandError {
    CommandError::simple("argument.pos.unloaded")
}

fn position(ctx: &Context<'_>, name: &str) -> Result<BlockPos, CommandError> {
    if ctx.has(name) {
        let [x, y, z] = block_pos_arg(ctx, name)?;
        return Ok(BlockPos::new(x, y, z));
    }
    let at = ctx.source.position;
    Ok(BlockPos::new(
        at[0].floor() as i32,
        at[1].floor() as i32,
        at[2].floor() as i32,
    ))
}

fn chunk_rect(min: ChunkPos, max: ChunkPos) -> impl Iterator<Item = ChunkPos> {
    (min.z..=max.z).flat_map(move |z| (min.x..=max.x).map(move |x| ChunkPos::new(x, z)))
}

fn inflated(range: (ChunkPos, ChunkPos)) -> (ChunkPos, ChunkPos) {
    (
        ChunkPos::new(range.0.x - 1, range.0.z - 1),
        ChunkPos::new(range.1.x + 1, range.1.z + 1),
    )
}

fn union(a: (ChunkPos, ChunkPos), b: (ChunkPos, ChunkPos)) -> (ChunkPos, ChunkPos) {
    (
        ChunkPos::new(a.0.x.min(b.0.x), a.0.z.min(b.0.z)),
        ChunkPos::new(a.1.x.max(b.1.x), a.1.z.max(b.1.z)),
    )
}

async fn ensure_loaded(
    ctx: &mut Context<'_>,
    range: (ChunkPos, ChunkPos),
) -> Result<(), CommandError> {
    let probes: Vec<[i32; 3]> = chunk_rect(range.0, range.1)
        .map(|chunk| [chunk.x << 4, 0, chunk.z << 4])
        .collect();
    let dimension = ctx.source.dimension;
    let states = read_blocks(ctx, dimension, &probes, false).await?;
    if states.iter().any(Option::is_none) {
        return Err(unloaded());
    }
    Ok(())
}

/// Places `work` in the window around each chunk of `chunks` on the region
/// that owns it; the unloaded error if a window never formed.
async fn place_chunks(
    ctx: &mut Context<'_>,
    chunks: &[ChunkPos],
    work: &Arc<Work>,
    flags: u32,
) -> Result<usize, CommandError> {
    let dimension: DimensionId = ctx.source.dimension;
    let mut targets = Vec::with_capacity(chunks.len());
    for &chunk in chunks {
        if let Some(region) = ctx.actor.region(dimension, chunk.region()).await {
            targets.push((chunk, region));
        }
    }
    tuff_core::world::place_work::place_chunks(&targets, work, flags)
        .await
        .ok_or_else(unloaded)
}

fn coordinates(pos: BlockPos) -> [TextComponent; 3] {
    [text(pos.x), text(pos.y), text(pos.z)]
}

fn success_with(key: &str, lead: Option<TextComponent>, pos: BlockPos) -> TextComponent {
    let mut args: Vec<TextComponent> = lead.into_iter().collect();
    args.extend(coordinates(pos));
    TextComponent::translate(key, args)
}

async fn place_feature(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
    let pos = position(ctx, "pos")?;
    let (feature, name) = match ctx.arg("feature") {
        Some(Value::Identifier(id)) => {
            let known = tuff_core::command::args::registry_entries(FEATURE_REGISTRY)
                .is_some_and(|entries| entries.binary_search(&id.as_str()).is_ok());
            if !known {
                return Err(CommandError::dynamic(
                    "argument.resource_or_id.no_such_element",
                    [id.clone(), FEATURE_REGISTRY.to_owned()],
                ));
            }
            (FeatureRef::Named(id.clone()), Some(id.clone()))
        }
        Some(Value::Nbt(tag @ Tag::Compound(_))) => (FeatureRef::Inline(json_of(tag)), None),
        _ => return Err(CommandError::simple("command.unknown.argument")),
    };
    let center = ChunkPos::of_block(pos);
    ensure_loaded(
        ctx,
        (
            ChunkPos::new(center.x - 1, center.z - 1),
            ChunkPos::new(center.x + 1, center.z + 1),
        ),
    )
    .await?;
    let work = Arc::new(Work::Request(PlaceRequest::Feature { feature, pos }));
    if place_chunks(ctx, &[center], &work, UPDATE_ALL).await? == 0 {
        return Err(CommandError::simple("commands.place.feature.failed"));
    }
    let message = match name {
        Some(name) => success_with("commands.place.feature.success", Some(text(name)), pos),
        None => success_with("commands.place.feature.success.inline", None, pos),
    };
    ctx.success(message, true).await;
    Ok(1)
}

async fn plan_off_actor<T: Send + 'static>(
    ctx: &Context<'_>,
    build: impl FnOnce() -> Option<T> + Send + 'static,
) -> Option<T> {
    let tags = ctx.shared().datapacks.contents().tags.clone();
    let (reply, answer) = oneshot::channel();
    rayon::spawn(move || {
        let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tuff_datapack::tags::sync_scope(tags, build)
        }));
        if let Ok(built) = built {
            let _ = reply.send(built);
        }
    });
    answer.await.ok().flatten()
}

async fn place_plan(
    ctx: &mut Context<'_>,
    plan: Arc<dyn PlannedStructure>,
    require_loaded: bool,
) -> Result<(), CommandError> {
    let range = plan.chunks();
    if require_loaded {
        ensure_loaded(ctx, inflated(range)).await?;
    }
    let chunks: Vec<ChunkPos> = chunk_rect(range.0, range.1).collect();
    place_chunks(ctx, &chunks, &Arc::new(Work::Plan(plan)), UPDATE_CLIENTS).await?;
    Ok(())
}

async fn place_jigsaw(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
    let (Some(Value::Resource(pool)), Some(Value::Identifier(target)), Some(Value::Int(depth))) =
        (ctx.arg("pool"), ctx.arg("target"), ctx.arg("max_depth"))
    else {
        return Err(CommandError::simple("command.unknown.argument"));
    };
    let (pool, target, depth) = (pool.clone(), target.clone(), *depth);
    let pos = position(ctx, "position")?;
    let chunk = ChunkPos::of_block(pos);
    ensure_loaded(ctx, inflated((chunk, chunk))).await?;
    let dimension = ctx.shared().dimension(ctx.source.dimension);
    let (generator, shape) = (dimension.generator.clone(), dimension.shape);
    let plan = plan_off_actor(ctx, move || {
        generator.plan_jigsaw(&pool, &target, depth, pos, shape, false)
    })
    .await
    .ok_or_else(|| CommandError::simple("commands.place.jigsaw.failed"))?;
    place_plan(ctx, plan, false).await?;
    ctx.success(
        success_with("commands.place.jigsaw.success", None, pos),
        true,
    )
    .await;
    Ok(1)
}

async fn place_structure(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
    let Some(Value::Resource(structure)) = ctx.arg("structure") else {
        return Err(CommandError::simple("command.unknown.argument"));
    };
    let structure = structure.clone();
    let pos = position(ctx, "pos")?;
    let chunk = ChunkPos::of_block(pos);
    let dimension = ctx.shared().dimension(ctx.source.dimension);
    let (generator, shape) = (dimension.generator.clone(), dimension.shape);
    let id = structure.clone();
    let plan = plan_off_actor(ctx, move || {
        generator.plan_structure(&structure, chunk, shape)
    })
    .await
    .ok_or_else(|| CommandError::simple("commands.place.structure.failed"))?;
    place_plan(ctx, plan, true).await?;
    ctx.success(
        success_with("commands.place.structure.success", Some(text(id)), pos),
        true,
    )
    .await;
    Ok(1)
}

async fn place_template(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
    let Some(Value::Identifier(name)) = ctx.arg("template") else {
        return Err(CommandError::simple("command.unknown.argument"));
    };
    let name = name.clone();
    let pos = position(ctx, "pos")?;
    let rotation = match ctx.arg("rotation") {
        Some(Value::TemplateRotation(turns)) => *turns,
        _ => 0,
    };
    let mirror = match ctx.arg("mirror") {
        Some(Value::TemplateMirror(mirror)) => *mirror,
        _ => 0,
    };
    let integrity = match ctx.arg("integrity") {
        Some(Value::Float(integrity)) => *integrity,
        _ => 1.0,
    };
    let seed = match ctx.arg("seed") {
        Some(Value::Int(seed)) => i64::from(*seed),
        _ => 0,
    };
    let request = TemplateRequest {
        name: name.clone(),
        pos,
        rotation,
        mirror,
        integrity,
        seed,
        strict: ctx.has("strict"),
        ignore_entities: false,
    };
    place_template_request(ctx, request).await?;
    ctx.success(
        success_with("commands.place.template.success", Some(text(name)), pos),
        true,
    )
    .await;
    Ok(1)
}

/// Places the template `request` names as `/place template` does, loading
/// the chunks it covers.
pub async fn place_template_request(
    ctx: &mut Context<'_>,
    request: TemplateRequest,
) -> Result<(), CommandError> {
    let name = request.name.clone();
    let host =
        ctx.shared().services.feature_host().ok_or_else(|| {
            CommandError::dynamic("commands.place.template.invalid", [name.clone()])
        })?;
    let extent = host
        .template_extent(&request)
        .ok_or_else(|| CommandError::dynamic("commands.place.template.invalid", [name.clone()]))?;
    ensure_loaded(ctx, inflated(union(extent.declared, extent.placed))).await?;
    let flags = if request.strict {
        UPDATE_CLIENTS | UPDATE_SKIP_BLOCK_ENTITY_SIDE_EFFECTS
    } else {
        UPDATE_CLIENTS | UPDATE_NEIGHBORS
    };
    let chunks: Vec<ChunkPos> = chunk_rect(extent.placed.0, extent.placed.1).collect();
    let work = Arc::new(Work::Request(PlaceRequest::Template(request)));
    if place_chunks(ctx, &chunks, &work, flags).await? == 0 {
        return Err(CommandError::simple("commands.place.template.failed"));
    }
    Ok(())
}

pub struct Place;

impl Executor for Place {
    fn suggest(
        &self,
        path: &[ParsedNode],
        partial: &str,
        _input: &SuggestInput,
    ) -> Option<Vec<String>> {
        if path.last().map(|node| node.node.name) != Some("template") {
            return None;
        }
        let partial = partial.to_lowercase();
        Some(
            tuff_vanilla_data::structure::template_names()
                .filter(|name| matches_resource(&partial, name))
                .map(str::to_owned)
                .collect(),
        )
    }

    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            match ctx.literals().get(1).copied() {
                Some("feature") => place_feature(ctx).await,
                Some("jigsaw") => place_jigsaw(ctx).await,
                Some("structure") => place_structure(ctx).await,
                Some("template") => place_template(ctx).await,
                _ => Err(CommandError::simple("command.unknown.command")),
            }
        })
    }
}
