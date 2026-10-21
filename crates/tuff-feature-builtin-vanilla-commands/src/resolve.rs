//! Expands selector, score and nbt contents of a command's text component
//! before it is sent, under the depth and count limits the game applies.

use std::collections::{HashMap, HashSet};

use tuff_core::command::CommandError;
use tuff_core::command::args::{ScoreHolderInput, Value};
use tuff_core::command::coords::Coordinates;
use tuff_core::command::nbt_path::NbtPath;
use tuff_core::command::reader::Reader;
use tuff_core::command::selector::Selector;
use tuff_item::text::{ComponentResolver, NbtSource, ResolveBudget, Special, TextComponent};
use tuff_nbt::Tag;

use super::data::Accessor;
use super::scoreboard;
use super::targets;
use super::{Context, text};

type NbtKey = (&'static str, String, String);

#[derive(Default)]
struct Fetched {
    selectors: HashMap<String, Vec<TextComponent>>,
    scores: HashMap<(String, String), Option<TextComponent>>,
    nbt: HashMap<NbtKey, Vec<Tag>>,
}

fn nbt_key(source: &NbtSource, path: &str) -> NbtKey {
    match source {
        NbtSource::Block(v) => ("block", v.clone(), path.to_owned()),
        NbtSource::Entity(v) => ("entity", v.clone(), path.to_owned()),
        NbtSource::Storage(v) => ("storage", v.clone(), path.to_owned()),
    }
}

impl ComponentResolver for Fetched {
    fn selector(&self, pattern: &str) -> Vec<TextComponent> {
        self.selectors.get(pattern).cloned().unwrap_or_default()
    }

    fn score(&self, name: &str, objective: &str) -> Option<TextComponent> {
        self.scores
            .get(&(name.to_owned(), objective.to_owned()))
            .cloned()
            .flatten()
    }

    fn nbt(&self, source: &NbtSource, path: &str) -> Vec<Tag> {
        self.nbt
            .get(&nbt_key(source, path))
            .cloned()
            .unwrap_or_default()
    }

    fn nbt_text(&self, tag: &Tag, plain: bool) -> TextComponent {
        if plain {
            TextComponent::new(tuff_core::command::nbt_path::pretty(tag))
        } else {
            tuff_core::command::nbt_path::pretty_component(tag)
        }
    }
}

fn collect<'a>(component: &'a TextComponent, out: &mut Vec<&'a Special>) {
    if let Some(special) = component.special_contents() {
        out.push(special);
        if let Special::Selector {
            separator: Some(separator),
            ..
        }
        | Special::Nbt {
            separator: Some(separator),
            ..
        } = special
        {
            collect(separator, out);
        }
    }
    component.with.iter().for_each(|c| collect(c, out));
    component.extra.iter().for_each(|c| collect(c, out));
}

fn parse_selector(pattern: &str) -> Option<Selector> {
    Selector::parse(&mut Reader::new(pattern), true).ok()
}

async fn fetch_selector(ctx: &mut Context<'_>, pattern: &str) -> Vec<TextComponent> {
    let Some(selector) = parse_selector(pattern) else {
        return Vec::new();
    };
    targets::select_entities(ctx, &selector)
        .await
        .map(|found| found.iter().map(|t| ctx.name_of(t)).collect())
        .unwrap_or_default()
}

async fn fetch_score(
    ctx: &mut Context<'_>,
    name: &str,
    objective: &str,
) -> Result<Option<TextComponent>, CommandError> {
    let holder = if name == "*" {
        match scoreboard::source_holder(ctx).await {
            Ok(holder) => holder,
            Err(_) => tuff_core::world::scoreboard::Holder::named(name),
        }
    } else if name.starts_with('@') {
        let Some(selector) = parse_selector(name) else {
            return Ok(None);
        };
        let input = ScoreHolderInput::Selector(Box::new(selector));
        match scoreboard::resolve_holders(ctx, input, false).await {
            Ok(mut holders) if holders.len() == 1 => holders.remove(0),
            Ok(_) => return Err(CommandError::simple("argument.entity.toomany")),
            Err(_) => return Ok(None),
        }
    } else {
        tuff_core::world::scoreboard::Holder::named(name)
    };
    let (holder_name, objective) = (holder.name, objective.to_owned());
    let value = ctx
        .shared()
        .scoreboard
        .read(move |board| board.score(&holder_name, &objective).map(|s| s.value))
        .await
        .flatten();
    Ok(value.map(text))
}

async fn fetch_nbt(ctx: &mut Context<'_>, source: &NbtSource, path: &str) -> Vec<Tag> {
    let value = match source {
        NbtSource::Block(pos) => Coordinates::parse_block_pos(&mut Reader::new(pos))
            .ok()
            .map(Value::Coordinates),
        NbtSource::Entity(pattern) => parse_selector(pattern).map(|s| Value::Selector(Box::new(s))),
        NbtSource::Storage(id) => Some(Value::Identifier(if id.contains(':') {
            id.clone()
        } else {
            format!("minecraft:{id}")
        })),
    };
    let Ok(path) = NbtPath::parse(&mut Reader::new(path)) else {
        return Vec::new();
    };
    let Ok(accessor) = Accessor::from_value(ctx, value).await else {
        return Vec::new();
    };
    let Ok(data) = accessor.read(ctx).await else {
        return Vec::new();
    };
    path.get(&Tag::Compound(data)).unwrap_or_default()
}

/// `component` with its selector, score and nbt contents replaced by
/// what they currently name.
pub async fn resolve(
    ctx: &mut Context<'_>,
    component: &TextComponent,
) -> Result<TextComponent, CommandError> {
    let budget = ResolveBudget::default();
    let mut fetched = Fetched::default();
    let mut seen: HashSet<String> = HashSet::new();
    let mut pending = vec![component.clone()];
    for _ in 0..=budget.depth_limit {
        if pending.is_empty() {
            break;
        }
        let batch = std::mem::take(&mut pending);
        let mut specials = Vec::new();
        batch.iter().for_each(|c| collect(c, &mut specials));
        for special in specials {
            match special {
                Special::Selector { pattern, .. } => {
                    if seen.insert(format!("s:{pattern}")) {
                        let names = fetch_selector(ctx, pattern).await;
                        fetched.selectors.insert(pattern.clone(), names);
                    }
                }
                Special::Score { name, objective } => {
                    if seen.insert(format!("c:{name}:{objective}")) {
                        let score = fetch_score(ctx, name, objective).await?;
                        fetched
                            .scores
                            .insert((name.clone(), objective.clone()), score);
                    }
                }
                Special::Nbt {
                    path,
                    interpret,
                    source,
                    ..
                } => {
                    let key = nbt_key(source, path);
                    if seen.insert(format!("n:{key:?}")) {
                        let tags = fetch_nbt(ctx, source, path).await;
                        if *interpret {
                            pending.extend(tags.iter().filter_map(|tag| {
                                tag.as_str()
                                    .and_then(TextComponent::from_json_str)
                                    .or_else(|| TextComponent::from_nbt(tag))
                            }));
                        }
                        fetched.nbt.insert(key, tags);
                    }
                }
                _ => {}
            }
        }
    }
    Ok(component.resolve_limited(&fetched, &mut budget.clone(), 0))
}
