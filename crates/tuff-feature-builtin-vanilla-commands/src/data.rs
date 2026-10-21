//! `/data`, `/tag` and the NBT stores of `/execute store`: block entity,
//! entity and command storage data read and edited through NBT paths.

use tokio::sync::oneshot;

use tuff_core::command::nbt_path::{self, NbtPath, pretty, pretty_component, too_deep};
use tuff_core::command::selector::Selector;
use tuff_core::command::{CommandError, Value};
use tuff_core::world::messages::{
    Directive, DirectiveReply, ManagerMessage, PlayerSnapshot, RegionRequest, TagEdit,
};
use tuff_item::text::{ClickEvent, TextComponent};
use tuff_nbt::{Compound, Tag};
use tuff_storage::admin::format_uuid;

use super::feedback::Messages;
use super::predicates;
use super::targets::{self, Target, resolve_message};
use super::world::{copy_blocks, in_world, paste_blocks, read_blocks};
use super::{Context, Executor, SourceKind, executor_future, text};

/// Something `/data` reads and writes: a compound with a name.
#[derive(Debug, Clone)]
pub enum Accessor {
    Block([i32; 3]),
    Entity(Box<Target>),
    Storage(String),
}

fn unknown_argument() -> CommandError {
    CommandError::simple("command.unknown.argument")
}

impl Accessor {
    /// The accessor an argument pair (`target`/`source`) names, by the
    /// kind of value it parsed to.
    pub async fn resolve(ctx: &mut Context<'_>, name: &str) -> Result<Self, CommandError> {
        let value = ctx.arg(name).cloned();
        Self::from_value(ctx, value).await
    }

    pub async fn from_value(
        ctx: &mut Context<'_>,
        value: Option<Value>,
    ) -> Result<Self, CommandError> {
        match value {
            Some(Value::Coordinates(coordinates)) => {
                let pos = coordinates.block(&ctx.source.pose());
                let dimension = ctx.source.dimension;
                if !in_world(pos) {
                    return Err(CommandError::simple("argument.pos.outofworld"));
                }
                let loaded = read_blocks(ctx, dimension, &[pos], false).await?;
                if loaded[0].is_none() {
                    return Err(CommandError::simple("argument.pos.unloaded"));
                }
                let copied = copy_blocks(ctx, dimension, &[pos]).await?;
                match copied.into_iter().next().flatten() {
                    Some((_, Some(_))) => Ok(Self::Block(pos)),
                    _ => Err(CommandError::simple("commands.data.block.invalid")),
                }
            }
            Some(Value::Selector(selector)) => Ok(Self::Entity(Box::new(
                targets::select_entity(ctx, &selector).await?,
            ))),
            Some(Value::Identifier(id)) => Ok(Self::Storage(id)),
            _ => Err(unknown_argument()),
        }
    }

    pub async fn read(&self, ctx: &mut Context<'_>) -> Result<Compound, CommandError> {
        match self {
            Self::Block(pos) => {
                let dimension = ctx.source.dimension;
                let copied = copy_blocks(ctx, dimension, &[*pos]).await?;
                match copied.into_iter().next().flatten() {
                    Some((_, Some(data))) => Ok(data),
                    _ => Err(CommandError::simple("commands.data.block.invalid")),
                }
            }
            Self::Entity(target) => match &**target {
                Target::Player(player) => Ok(player_data(ctx, player)),
                Target::Entity(entity) => {
                    let (_, mut data) = find_entity(ctx, entity.id)
                        .await
                        .ok_or_else(|| CommandError::simple("argument.entity.notfound.entity"))?;
                    data.remove("id");
                    Ok(data)
                }
            },
            Self::Storage(id) => Ok(ctx.shared().command_storage.get(id).await),
        }
    }

    pub async fn write(&self, ctx: &mut Context<'_>, data: Compound) -> Result<(), CommandError> {
        match self {
            Self::Block(pos) => {
                let dimension = ctx.source.dimension;
                let copied = copy_blocks(ctx, dimension, &[*pos]).await?;
                let Some((state, Some(_))) = copied.into_iter().next().flatten() else {
                    return Err(CommandError::simple("commands.data.block.invalid"));
                };
                paste_blocks(ctx, dimension, &[(*pos, state, Some(data))], true).await?;
                Ok(())
            }
            Self::Entity(target) => match &**target {
                Target::Player(_) => Err(CommandError::simple("commands.data.entity.invalid")),
                Target::Entity(entity) => {
                    let (region, old) = find_entity(ctx, entity.id)
                        .await
                        .ok_or_else(|| CommandError::simple("argument.entity.notfound.entity"))?;
                    let mut nbt = data;
                    if let Some(id) = old.get("id") {
                        nbt.insert("id", id.clone());
                    }
                    if let Some(uuid) = old.get("UUID") {
                        nbt.insert("UUID", uuid.clone());
                    }
                    let id = entity.id;
                    let _ = region.ask(move |world| world.replace_entity(id, nbt)).await;
                    Ok(())
                }
            },
            Self::Storage(id) => {
                ctx.shared().command_storage.set(id, data).await;
                Ok(())
            }
        }
    }

    fn modified(&self, ctx: &Context<'_>) -> TextComponent {
        match self {
            Self::Block([x, y, z]) => TextComponent::translate(
                "commands.data.block.modified",
                [text(x), text(y), text(z)],
            ),
            Self::Entity(target) => {
                TextComponent::translate("commands.data.entity.modified", [ctx.name_of(target)])
            }
            Self::Storage(id) => {
                TextComponent::translate("commands.data.storage.modified", [text(id)])
            }
        }
    }

    fn query(&self, data: &Tag, ctx: &Context<'_>) -> TextComponent {
        let data = pretty_component(data);
        match self {
            Self::Block([x, y, z]) => TextComponent::translate(
                "commands.data.block.query",
                [text(x), text(y), text(z), data],
            ),
            Self::Entity(target) => {
                TextComponent::translate("commands.data.entity.query", [ctx.name_of(target), data])
            }
            Self::Storage(id) => {
                TextComponent::translate("commands.data.storage.query", [text(id), data])
            }
        }
    }

    fn get(&self, path: &NbtPath, scale: f64, value: i32, ctx: &Context<'_>) -> TextComponent {
        let path = text(path.as_str());
        let scale = text(format!("{scale:.2}"));
        let value = text(value);
        match self {
            Self::Block([x, y, z]) => TextComponent::translate(
                "commands.data.block.get",
                [path, text(x), text(y), text(z), scale, value],
            ),
            Self::Entity(target) => TextComponent::translate(
                "commands.data.entity.get",
                [path, ctx.name_of(target), scale, value],
            ),
            Self::Storage(id) => TextComponent::translate(
                "commands.data.storage.get",
                [path, text(id), scale, value],
            ),
        }
    }
}

/// The saved form of a region entity and the region that owns it.
pub(super) async fn find_entity(
    ctx: &Context<'_>,
    id: i32,
) -> Option<(tuff_core::world::messages::RegionHandle, Compound)> {
    let (reply, regions) = oneshot::channel();
    ctx.shared()
        .manager
        .send(ManagerMessage::Regions { reply })
        .await
        .ok()?;
    for region in regions.await.unwrap_or_default() {
        let (reply, answer) = oneshot::channel();
        if region
            .request(RegionRequest::EntityData { id, reply })
            .await
            && let Ok(Some(data)) = answer.await
        {
            return Some((region, data));
        }
    }
    None
}

/// A player's saved form: exact for the command's own player, otherwise
/// what that player's session last published (a sketch from the snapshot
/// until it has published one).
pub(super) fn player_data(ctx: &Context<'_>, player: &PlayerSnapshot) -> Compound {
    if let Some(own) = ctx.actor.own_selector_nbt(player.id) {
        return own;
    }
    if let Some(form) = player.handle.selector_data() {
        return (*form).clone();
    }
    let [x, y, z] = player.state.position;
    let mut data = Compound::new()
        .with(
            "Pos",
            Tag::List(vec![Tag::Double(x), Tag::Double(y), Tag::Double(z)]),
        )
        .with(
            "Rotation",
            Tag::List(vec![
                Tag::Float(player.state.rotation[0]),
                Tag::Float(player.state.rotation[1]),
            ]),
        )
        .with("UUID", Tag::uuid(player.uuid));
    if !player.state.tags.is_empty() {
        data.insert(
            "Tags",
            Tag::List(
                player
                    .state
                    .tags
                    .iter()
                    .map(|t| Tag::from(t.as_str()))
                    .collect(),
            ),
        );
    }
    data
}

fn path_arg(ctx: &Context<'_>, name: &str) -> Result<NbtPath, CommandError> {
    match ctx.arg(name) {
        Some(Value::NbtPath(path)) => Ok(path.clone()),
        _ => Err(unknown_argument()),
    }
}

fn int_arg(ctx: &Context<'_>, name: &str) -> Option<i32> {
    match ctx.arg(name) {
        Some(Value::Int(value)) => Some(*value),
        _ => None,
    }
}

fn single(mut tags: Vec<Tag>) -> Result<Tag, CommandError> {
    let first = tags.remove(0);
    if tags.is_empty() {
        Ok(first)
    } else {
        Err(CommandError::simple("commands.data.get.multiple"))
    }
}

fn numeric(tag: &Tag) -> Option<f64> {
    Some(match tag {
        Tag::Byte(v) => f64::from(*v),
        Tag::Short(v) => f64::from(*v),
        Tag::Int(v) => f64::from(*v),
        Tag::Long(v) => *v as f64,
        Tag::Float(v) => f64::from(*v),
        Tag::Double(v) => *v,
        _ => return None,
    })
}

fn floor(value: f64) -> i32 {
    value.floor() as i32
}

fn compound_of(tag: Tag) -> Compound {
    match tag {
        Tag::Compound(compound) => compound,
        _ => Compound::new(),
    }
}

async fn get(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
    let accessor = Accessor::resolve(ctx, "target").await?;
    let data = accessor.read(ctx).await?;
    let Ok(path) = path_arg(ctx, "path") else {
        let message = accessor.query(&Tag::Compound(data), ctx);
        ctx.success(message, false).await;
        return Ok(1);
    };
    let found = single(path.get(&Tag::Compound(data))?)?;
    if let Some(Value::Double(scale)) = ctx.arg("scale").cloned() {
        let Some(value) = numeric(&found) else {
            return Err(CommandError::dynamic(
                "commands.data.get.invalid",
                [path.as_str().to_owned()],
            ));
        };
        let result = floor(value * scale);
        let message = accessor.get(&path, scale, result, ctx);
        ctx.success(message, false).await;
        return Ok(result);
    }
    let length = match &found {
        Tag::List(v) => v.len() as i32,
        Tag::ByteArray(v) => v.len() as i32,
        Tag::IntArray(v) => v.len() as i32,
        Tag::LongArray(v) => v.len() as i32,
        Tag::Compound(c) => c.len() as i32,
        Tag::String(s) => s.encode_utf16().count() as i32,
        other => floor(numeric(other).unwrap_or(0.0)),
    };
    let message = accessor.query(&found, ctx);
    ctx.success(message, false).await;
    Ok(length)
}

async fn merge(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
    let accessor = Accessor::resolve(ctx, "target").await?;
    let Some(Value::Nbt(nbt)) = ctx.arg("nbt").cloned() else {
        return Err(unknown_argument());
    };
    let old = accessor.read(ctx).await?;
    if too_deep(&nbt, 0) {
        return Err(CommandError::simple("arguments.nbtpath.too_deep"));
    }
    let mut result = old.clone();
    nbt_path::merge(&mut result, &compound_of(nbt));
    if old == result {
        return Err(CommandError::simple("commands.data.merge.failed"));
    }
    accessor.write(ctx, result).await?;
    let message = accessor.modified(ctx);
    ctx.success(message, true).await;
    Ok(1)
}

async fn remove(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
    let accessor = Accessor::resolve(ctx, "target").await?;
    let path = path_arg(ctx, "path")?;
    let mut root = Tag::Compound(accessor.read(ctx).await?);
    let count = path.remove(&mut root);
    if count == 0 {
        return Err(CommandError::simple("commands.data.merge.failed"));
    }
    accessor.write(ctx, compound_of(root)).await?;
    let message = accessor.modified(ctx);
    ctx.success(message, true).await;
    Ok(count as i32)
}

#[derive(Clone, Copy)]
enum Modify {
    Insert(i32),
    Set,
    Merge,
}

fn modification(ctx: &Context<'_>) -> Result<Modify, CommandError> {
    let literals = ctx.literals();
    let op = literals
        .iter()
        .copied()
        .find(|l| matches!(*l, "insert" | "prepend" | "append" | "set" | "merge"));
    Ok(match op {
        Some("insert") => Modify::Insert(int_arg(ctx, "index").ok_or_else(unknown_argument)?),
        Some("prepend") => Modify::Insert(0),
        Some("append") => Modify::Insert(-1),
        Some("set") => Modify::Set,
        Some("merge") => Modify::Merge,
        _ => return Err(unknown_argument()),
    })
}

fn utf16(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

fn offset(index: i32, length: usize) -> i64 {
    if index >= 0 {
        i64::from(index)
    } else {
        length as i64 + i64::from(index)
    }
}

fn substring(input: &str, start: i32, end: Option<i32>) -> Result<String, CommandError> {
    let units = utf16(input);
    let length = units.len();
    let from = offset(start, length);
    let to = end.map_or(length as i64, |end| offset(end, length));
    if from < 0 || to > length as i64 || from > to {
        return Err(CommandError::dynamic(
            "commands.data.modify.invalid_substring",
            [from.to_string(), to.to_string()],
        ));
    }
    Ok(String::from_utf16_lossy(&units[from as usize..to as usize]))
}

fn as_text(tag: &Tag) -> Result<String, CommandError> {
    match tag {
        Tag::String(value) => Ok(value.clone()),
        Tag::Byte(_)
        | Tag::Short(_)
        | Tag::Int(_)
        | Tag::Long(_)
        | Tag::Float(_)
        | Tag::Double(_) => Ok(pretty(tag)),
        other => Err(CommandError::dynamic(
            "commands.data.modify.expected_value",
            [pretty(other)],
        )),
    }
}

/// The number `compute` sets: the provider's value in the context the
/// command names (`default`, at a loaded block, or with an entity).
pub(crate) async fn compute_value(ctx: &mut Context<'_>, float: bool) -> Result<f64, CommandError> {
    let registry = if float {
        "minecraft:context_float_provider"
    } else {
        "minecraft:context_int_provider"
    };
    let provider = match ctx.arg("provider") {
        Some(Value::Nbt(inline)) => predicates::json_of(inline),
        Some(Value::Identifier(id)) => {
            return Err(CommandError::dynamic(
                "argument.resource_or_id.no_such_element",
                [id.clone(), registry.to_owned()],
            ));
        }
        _ => return Err(unknown_argument()),
    };
    let mut block = None;
    if let Some(Value::Coordinates(coordinates)) = ctx.arg("computePos").cloned() {
        let pos = coordinates.block(&ctx.source.pose());
        if !in_world(pos) {
            return Err(CommandError::simple("argument.pos.outofworld"));
        }
        let dimension = ctx.source.dimension;
        if read_blocks(ctx, dimension, &[pos], false).await?[0].is_none() {
            return Err(CommandError::simple("argument.pos.unloaded"));
        }
        block = Some(pos);
    }
    let mut target = None;
    if let Some(Value::Selector(selector)) = ctx.arg("computeTarget").cloned() {
        target = Some(match targets::select_entity(ctx, &selector).await? {
            targets::Target::Player(player) => player.id,
            targets::Target::Entity(entity) => entity.id,
        });
    }
    let provider = std::sync::Arc::new(provider);
    let probes = std::slice::from_ref(&provider);
    let mut subject = predicates::source_subject(ctx, probes).await;
    if let Some(id) = target {
        subject = predicates::with_target(ctx, subject, id, probes).await;
    }
    if let Some(pos) = block {
        subject = predicates::with_block(subject, pos);
    }
    predicates::compute(ctx, provider, float, subject)
        .await
        .ok_or_else(|| CommandError::simple("command.failed"))
}

async fn computed(ctx: &mut Context<'_>, float: bool) -> Result<Tag, CommandError> {
    let value = compute_value(ctx, float).await?;
    Ok(if float {
        Tag::Float(value as f32)
    } else {
        Tag::Int(value as i32)
    })
}

async fn modify(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
    let op = modification(ctx)?;
    let target = Accessor::resolve(ctx, "target").await?;
    let target_path = path_arg(ctx, "targetPath")?;
    let literals = ctx.literals();
    let strings = literals.contains(&"string");
    let sources: Vec<Tag> = if let Some(Value::Nbt(value)) = ctx.arg("value").cloned() {
        vec![value]
    } else if literals.contains(&"compute") {
        vec![computed(ctx, literals.contains(&"float")).await?]
    } else {
        let source = Accessor::resolve(ctx, "source").await?;
        let data = Tag::Compound(source.read(ctx).await?);
        let found = match path_arg(ctx, "sourcePath") {
            Ok(path) => path.get(&data)?,
            Err(_) => vec![data],
        };
        if strings {
            let start = int_arg(ctx, "start");
            let end = int_arg(ctx, "end");
            found
                .iter()
                .map(|tag| {
                    let value = as_text(tag)?;
                    let value = match (start, end) {
                        (Some(start), end) => substring(&value, start, end)?,
                        _ => value,
                    };
                    Ok(Tag::String(value))
                })
                .collect::<Result<_, CommandError>>()?
        } else {
            found
        }
    };
    let mut root = Tag::Compound(target.read(ctx).await?);
    let changed = match op {
        Modify::Insert(index) => target_path.insert(index, &mut root, &sources)?,
        Modify::Set => target_path.set(&mut root, sources.last().ok_or_else(unknown_argument)?)?,
        Modify::Merge => {
            let mut combined = Compound::new();
            for source in &sources {
                if too_deep(source, 0) {
                    return Err(CommandError::simple("arguments.nbtpath.too_deep"));
                }
                match source {
                    Tag::Compound(compound) => nbt_path::merge(&mut combined, compound),
                    other => {
                        return Err(CommandError::dynamic(
                            "commands.data.modify.expected_object",
                            [pretty(other)],
                        ));
                    }
                }
            }
            target_path.merge_into(&mut root, &combined)?
        }
    };
    if changed == 0 {
        return Err(CommandError::simple("commands.data.merge.failed"));
    }
    target.write(ctx, compound_of(root)).await?;
    let message = target.modified(ctx);
    ctx.success(message, true).await;
    Ok(changed as i32)
}

pub struct Data;

impl Executor for Data {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            match ctx.literals().get(1).copied() {
                Some("get") => get(ctx).await,
                Some("merge") => merge(ctx).await,
                Some("remove") => remove(ctx).await,
                Some("modify") => modify(ctx).await,
                _ => Err(CommandError::simple("command.unknown.command")),
            }
        })
    }
}

/// Numeric tag types `execute store` can write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumberKind {
    Byte,
    Short,
    Int,
    Long,
    Float,
    Double,
}

impl NumberKind {
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "byte" => Self::Byte,
            "short" => Self::Short,
            "int" => Self::Int,
            "long" => Self::Long,
            "float" => Self::Float,
            "double" => Self::Double,
            _ => return None,
        })
    }

    pub fn tag(self, value: f64) -> Tag {
        match self {
            Self::Byte => Tag::Byte(value as i32 as i8),
            Self::Short => Tag::Short(value as i32 as i16),
            Self::Int => Tag::Int(value as i32),
            Self::Long => Tag::Long(value as i64),
            Self::Float => Tag::Float(value as f32),
            Self::Double => Tag::Double(value),
        }
    }
}

/// Writes a number into the data an `execute store` names; any failure is
/// ignored, as the game does.
pub async fn store_number(
    ctx: &mut Context<'_>,
    accessor: &Accessor,
    path: &NbtPath,
    kind: NumberKind,
    scale: f64,
    value: i32,
) {
    let Ok(data) = accessor.read(ctx).await else {
        return;
    };
    let mut root = Tag::Compound(data);
    if path
        .set(&mut root, &kind.tag(f64::from(value) * scale))
        .is_err()
    {
        return;
    }
    let _ = accessor.write(ctx, compound_of(root)).await;
}

fn tag_name(ctx: &Context<'_>) -> Result<String, CommandError> {
    match ctx.arg("name") {
        Some(Value::Str(name)) => Ok(name.clone()),
        _ => Err(unknown_argument()),
    }
}

pub struct TagCommand;

fn tag_selector(ctx: &Context<'_>) -> Result<Selector, CommandError> {
    match ctx.arg("targets") {
        Some(Value::Selector(selector)) => Ok((**selector).clone()),
        _ => Err(unknown_argument()),
    }
}

async fn edit_entities(ctx: &mut Context<'_>, edit: TagEdit) -> Result<i32, CommandError> {
    let selector = tag_selector(ctx)?;
    let found = targets::select_entities(ctx, &selector).await?;
    let (adding, name) = match &edit {
        TagEdit::Add(name) => (true, name.clone()),
        TagEdit::Remove(name) => (false, name.clone()),
    };
    let entities: Vec<&Target> = found
        .iter()
        .filter(|t| matches!(t, Target::Entity(_)))
        .collect();
    let mut changed_entities: Vec<i32> = Vec::new();
    if !entities.is_empty() {
        let ids: Vec<i32> = entities.iter().map(|t| t.id()).collect();
        let (reply, regions) = oneshot::channel();
        let _ = ctx
            .shared()
            .manager
            .send(ManagerMessage::Regions { reply })
            .await;
        for region in regions.await.unwrap_or_default() {
            let (ids, edit) = (ids.clone(), edit.clone());
            let changed = region
                .ask(move |world| {
                    ids.into_iter()
                        .filter(|id| world.edit_entity_tags(*id, &edit))
                        .collect::<Vec<i32>>()
                })
                .await;
            changed_entities.extend(changed.unwrap_or_default());
        }
    }
    let players: Vec<PlayerSnapshot> = found
        .iter()
        .filter_map(|t| match t {
            Target::Player(player) => Some(player.clone()),
            Target::Entity(_) => None,
        })
        .collect();
    let replies = if players.is_empty() {
        Vec::new()
    } else {
        let directive_edit = edit.clone();
        targets::dispatch_wait(ctx, &players, move |_| {
            Directive::Tag(directive_edit.clone())
        })
        .await
    };
    let changed_players: Vec<&DirectiveReply> = replies
        .iter()
        .filter(|r| r.affected && r.value != 0)
        .collect();
    let total = changed_entities.len() + changed_players.len();
    let key = if adding { "add" } else { "remove" };
    if total == 0 {
        return Err(CommandError::simple(&format!("commands.tag.{key}.failed")));
    }
    let message = if total == 1 {
        let only = match (changed_entities.first(), changed_players.first()) {
            (Some(id), _) => entities
                .iter()
                .find(|t| t.id() == *id)
                .map_or_else(|| text(""), |t| ctx.name_of(t)),
            (None, Some(reply)) => text(&reply.name),
            (None, None) => text(""),
        };
        TextComponent::translate(
            format!("commands.tag.{key}.success.single"),
            [text(&name), only],
        )
    } else {
        TextComponent::translate(
            format!("commands.tag.{key}.success.multiple"),
            [text(&name), text(total)],
        )
    };
    ctx.success(message, true).await;
    Ok(total as i32)
}

fn list_message(
    single_empty: bool,
    entity_count: usize,
    tags: &[String],
    only: Option<TextComponent>,
) -> TextComponent {
    let mut sorted: Vec<&String> = tags.iter().collect();
    sorted.sort();
    sorted.dedup();
    if sorted.is_empty() {
        return match only {
            Some(name) if single_empty => {
                TextComponent::translate("commands.tag.list.single.empty", [name])
            }
            _ => TextComponent::translate("commands.tag.list.multiple.empty", [text(entity_count)]),
        };
    }
    let joined = sorted
        .iter()
        .map(|t| t.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    match only {
        Some(name) => TextComponent::translate(
            "commands.tag.list.single.success",
            [name, text(sorted.len()), text(joined)],
        ),
        None => TextComponent::translate(
            "commands.tag.list.multiple.success",
            [text(entity_count), text(sorted.len()), text(joined)],
        ),
    }
}

async fn list_tags(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
    let selector = tag_selector(ctx)?;
    let found = targets::select_entities(ctx, &selector).await?;
    let mut all: Vec<String> = Vec::new();
    for target in &found {
        match target {
            Target::Player(player) => all.extend(player.state.tags.iter().cloned()),
            Target::Entity(entity) => all.extend(entity.tags.iter().cloned()),
        }
    }
    let only = (found.len() == 1).then(|| ctx.name_of(&found[0]));
    let message = list_message(true, found.len(), &all, only);
    ctx.success(message, false).await;
    all.sort();
    all.dedup();
    Ok(all.len() as i32)
}

impl Executor for TagCommand {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            match ctx.literals().get(2).copied() {
                Some("add") => {
                    let name = tag_name(ctx)?;
                    edit_entities(ctx, TagEdit::Add(name)).await
                }
                Some("remove") => {
                    let name = tag_name(ctx)?;
                    edit_entities(ctx, TagEdit::Remove(name)).await
                }
                Some("list") => list_tags(ctx).await,
                _ => Err(CommandError::simple("command.unknown.command")),
            }
        })
    }
}

pub struct TeamMessage;

impl Executor for TeamMessage {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let Some(Value::Message(message)) = ctx.arg("message").cloned() else {
                return Err(unknown_argument());
            };
            let holder = match ctx.source.kind {
                SourceKind::Player { .. } => ctx.source.name.clone(),
                SourceKind::Console(_) => match ctx.source.entity {
                    Some(id) => targets::entities(ctx)
                        .await
                        .into_iter()
                        .find(|e| e.id == id)
                        .map(|e| format_uuid(e.uuid))
                        .ok_or_else(|| CommandError::simple("permissions.requires.entity"))?,
                    None => return Err(CommandError::simple("permissions.requires.entity")),
                },
            };
            let own_player = ctx.source.player_id();
            let players = targets::players(ctx).await;
            let names: Vec<String> = players.iter().map(|p| p.name.clone()).collect();
            let looked_up = holder.clone();
            let found = ctx
                .shared()
                .scoreboard
                .read(move |board| {
                    let team = board.team_of(&looked_up)?;
                    let members: Vec<bool> = names
                        .iter()
                        .map(|name| board.team_of(name).is_some_and(|t| t.name == team.name))
                        .collect();
                    Some((team.formatted_display_name(), members))
                })
                .await
                .flatten();
            let Some((team_name, members)) = found else {
                return Err(CommandError::simple("commands.teammsg.failed.noteam"));
            };
            let receivers: Vec<PlayerSnapshot> = players
                .into_iter()
                .zip(members)
                .filter(|(player, member)| *member || Some(player.id) == own_player)
                .map(|(player, _)| player)
                .collect();
            if receivers.is_empty() {
                return Ok(0);
            }
            let message = resolve_message(ctx, &message).await?;
            let team_name = team_name
                .click(ClickEvent::SuggestCommand("/teammsg ".into()))
                .hover_text(TextComponent::translate("chat.type.team.hover", []));
            let sender = ctx.source_display_name();
            let line = |key: &str| {
                TextComponent::translate(key, [team_name.clone(), sender.clone(), message.clone()])
            };
            let incoming = line("chat.type.team.text");
            let outgoing = line("chat.type.team.sent");
            let count = receivers.len() as i32;
            let messages =
                Messages::new(|_| TextComponent::new(""), |_, _| TextComponent::new("")).silent();
            targets::dispatch(
                ctx,
                &receivers,
                |player| {
                    Directive::PrivateMessage(if Some(player.id) == own_player {
                        outgoing.clone()
                    } else {
                        incoming.clone()
                    })
                },
                messages,
            )
            .await;
            Ok(count)
        })
    }
}
