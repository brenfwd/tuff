//! Advancements from the data pack (`advancement/`): the tree (parents,
//! children, roots and the layout the advancement screen draws), display
//! info, requirements and rewards, all typed and validated. Criterion
//! conditions are an open-ended mini-language per trigger and stay raw
//! JSON; every trigger the data names must be a vanilla trigger type.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde_json::{Map, Value};

use super::{json, registries};
use crate::input::Input;
use crate::names::str_literal;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Advancement {
    #[serde(default)]
    parent: Option<String>,
    #[serde(default)]
    display: Option<Display>,
    criteria: Map<String, Value>,
    #[serde(default)]
    requirements: Option<Vec<Vec<String>>>,
    #[serde(default)]
    rewards: Option<Rewards>,
    #[serde(default)]
    sends_telemetry_event: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Display {
    icon: Icon,
    title: Value,
    description: Value,
    #[serde(default)]
    background: Option<String>,
    #[serde(default)]
    frame: Option<String>,
    #[serde(default = "yes")]
    show_toast: bool,
    #[serde(default = "yes")]
    announce_to_chat: bool,
    #[serde(default)]
    hidden: bool,
}

fn yes() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Icon {
    id: String,
    #[serde(default)]
    count: Option<u8>,
    #[serde(default)]
    components: Option<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rewards {
    #[serde(default)]
    experience: i32,
    #[serde(default)]
    loot: Option<Value>,
    #[serde(default)]
    recipes: Vec<String>,
    #[serde(default)]
    function: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Criterion {
    trigger: String,
    #[serde(default)]
    conditions: Option<Value>,
}

const FRAMES: &[(&str, &str)] = &[
    ("task", "Frame::Task"),
    ("challenge", "Frame::Challenge"),
    ("goal", "Frame::Goal"),
];

fn qualified(name: &str) -> String {
    if name.contains(':') {
        name.to_owned()
    } else {
        format!("minecraft:{name}")
    }
}

/// Every `<namespace>/<dir>/<path>.json` data file as `namespace:path`.
fn data_ids<'a>(input: &'a Input, dir: &'a str) -> impl Iterator<Item = (String, &'a Value)> {
    input.data_json.iter().filter_map(move |(path, value)| {
        let (namespace, rest) = path.split_once('/')?;
        let name = rest.strip_prefix(dir)?.strip_prefix('/')?;
        let name = name.strip_suffix(".json")?;
        Some((format!("{namespace}:{name}"), value))
    })
}

struct Node {
    name: String,
    advancement: Advancement,
    parent: Option<usize>,
    children: Vec<usize>,
    root: usize,
    position: (f32, f32),
}

pub fn generate(input: &Input) -> Result<String> {
    let items = registries::variants(&input.registries["minecraft:item"])?;
    let triggers: BTreeSet<&str> = input.registries["minecraft:trigger_type"]
        .entries
        .keys()
        .map(String::as_str)
        .collect();
    let recipes: BTreeSet<String> = data_ids(input, "recipe").map(|(id, _)| id).collect();
    let loot_tables: BTreeSet<String> = data_ids(input, "loot_table").map(|(id, _)| id).collect();

    let mut nodes: Vec<Node> = Vec::new();
    for (name, value) in data_ids(input, "advancement") {
        let context = || format!("advancement {name}");
        let advancement: Advancement =
            serde_json::from_value(value.clone()).with_context(context)?;
        nodes.push(Node {
            name,
            advancement,
            parent: None,
            children: Vec::new(),
            root: 0,
            position: (0.0, 0.0),
        });
    }
    ensure!(!nodes.is_empty(), "no advancements found");
    nodes.sort_by(|a, b| a.name.cmp(&b.name));
    let index: BTreeMap<String, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, node)| (node.name.clone(), i))
        .collect();

    for i in 0..nodes.len() {
        let context = || format!("advancement {}", nodes[i].name);
        let advancement = &nodes[i].advancement;
        ensure!(
            !advancement.criteria.is_empty(),
            "{}: no criteria",
            context()
        );
        for (criterion, value) in &advancement.criteria {
            let criterion: Criterion = serde_json::from_value(value.clone())
                .with_context(|| format!("{}: criterion {criterion}", context()))?;
            ensure!(
                triggers.contains(qualified(&criterion.trigger).as_str()),
                "{}: unknown trigger {}",
                context(),
                criterion.trigger
            );
        }
        let names: BTreeSet<&str> = advancement.criteria.keys().map(String::as_str).collect();
        if let Some(requirements) = &advancement.requirements {
            let listed: BTreeSet<&str> =
                requirements.iter().flatten().map(String::as_str).collect();
            ensure!(
                listed == names,
                "{}: requirements do not match the criteria",
                context()
            );
            ensure!(
                requirements.iter().all(|group| !group.is_empty()),
                "{}: empty requirement group",
                context()
            );
        }
        let parent = match &advancement.parent {
            Some(parent) => Some(
                *index
                    .get(&qualified(parent))
                    .with_context(|| format!("{}: unknown parent {parent}", context()))?,
            ),
            None => None,
        };
        if let Some(display) = &advancement.display {
            ensure!(
                parent.is_none() == display.background.is_some(),
                "{}: only (and every) visible root has a background",
                context()
            );
            ensure!(
                items.contains_key(qualified(&display.icon.id).as_str()),
                "{}: unknown icon item {}",
                context(),
                display.icon.id
            );
            ensure!(
                display
                    .icon
                    .count
                    .is_none_or(|count| (1..=99).contains(&count)),
                "{}: icon count out of range",
                context()
            );
            if let Some(frame) = &display.frame {
                ensure!(
                    FRAMES.iter().any(|(name, _)| name == frame),
                    "{}: unknown frame {frame}",
                    context()
                );
            }
        }
        if let Some(rewards) = &advancement.rewards {
            for recipe in &rewards.recipes {
                ensure!(
                    recipes.contains(&qualified(recipe)),
                    "{}: rewards unknown recipe {recipe}",
                    context()
                );
            }
            for table in loot_list(rewards.loot.as_ref())? {
                ensure!(
                    loot_tables.contains(&table),
                    "{}: rewards unknown loot table {table}",
                    context()
                );
            }
        }
        nodes[i].parent = parent;
        if let Some(parent) = parent {
            nodes[parent].children.push(i);
        }
    }
    for i in 0..nodes.len() {
        let mut root = i;
        let mut steps = 0;
        while let Some(parent) = nodes[root].parent {
            root = parent;
            steps += 1;
            ensure!(
                steps <= nodes.len(),
                "advancement {} has a parent cycle",
                nodes[i].name
            );
        }
        nodes[i].root = root;
    }
    for root in 0..nodes.len() {
        if nodes[root].parent.is_none() && nodes[root].advancement.display.is_some() {
            for (node, position) in layout(&nodes, root) {
                nodes[node].position = position;
            }
        }
    }

    let mut out = String::new();
    writeln!(out, "pub static ALL: [Advancement; {}] = [", nodes.len())?;
    for node in &nodes {
        emit(&mut out, node, &items)?;
    }
    writeln!(out, "];")?;
    Ok(out)
}

fn loot_list(loot: Option<&Value>) -> Result<Vec<String>> {
    Ok(match loot {
        None => Vec::new(),
        Some(Value::String(table)) => {
            ensure!(!table.starts_with('#'), "loot table tags are not supported");
            vec![qualified(table)]
        }
        Some(Value::Array(tables)) => tables
            .iter()
            .map(|table| table.as_str().map(qualified).context("loot table id"))
            .collect::<Result<_>>()?,
        Some(other) => bail!("unexpected loot reward {other}"),
    })
}

fn emit(out: &mut String, node: &Node, items: &BTreeMap<&str, String>) -> Result<()> {
    let advancement = &node.advancement;
    let display = match &advancement.display {
        None => "None".to_owned(),
        Some(display) => {
            let frame = display.frame.as_deref().unwrap_or("task");
            let frame = FRAMES
                .iter()
                .find(|(name, _)| *name == frame)
                .map(|(_, variant)| *variant)
                .context("frame")?;
            format!(
                "Some(Display {{ icon: Icon {{ item: Item::{}, count: {}, components: {} }}, \
                 title: {}, description: {}, frame: {frame}, background: {}, show_toast: {}, \
                 announce_to_chat: {}, hidden: {} }})",
                items[qualified(&display.icon.id).as_str()],
                display.icon.count.unwrap_or(1),
                json::opt_literal(display.icon.components.as_ref()),
                json::literal(&display.title),
                json::literal(&display.description),
                match &display.background {
                    Some(background) => format!("Some({})", str_literal(&qualified(background))),
                    None => "None".to_owned(),
                },
                display.show_toast,
                display.announce_to_chat,
                display.hidden,
            )
        }
    };
    let criteria = advancement
        .criteria
        .iter()
        .map(|(name, value)| {
            let criterion: Criterion = serde_json::from_value(value.clone())?;
            Ok(format!(
                "Criterion {{ name: {}, trigger: {}, conditions: {} }}",
                str_literal(name),
                str_literal(&qualified(&criterion.trigger)),
                json::opt_literal(criterion.conditions.as_ref()),
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let requirements: Vec<Vec<String>> = match &advancement.requirements {
        Some(requirements) => requirements.clone(),
        None => advancement
            .criteria
            .keys()
            .map(|name| vec![name.clone()])
            .collect(),
    };
    let requirements = requirements
        .iter()
        .map(|group| {
            let names: Vec<String> = group.iter().map(|name| str_literal(name)).collect();
            format!("&[{}]", names.join(", "))
        })
        .collect::<Vec<_>>();
    let rewards = match &advancement.rewards {
        None => "Rewards::NONE".to_owned(),
        Some(rewards) => {
            let recipes: Vec<String> = rewards
                .recipes
                .iter()
                .map(|recipe| str_literal(&qualified(recipe)))
                .collect();
            let loot: Vec<String> = loot_list(rewards.loot.as_ref())?
                .iter()
                .map(|table| str_literal(table))
                .collect();
            format!(
                "Rewards {{ experience: {}, loot: &[{}], recipes: &[{}], function: {} }}",
                rewards.experience,
                loot.join(", "),
                recipes.join(", "),
                match &rewards.function {
                    Some(function) => format!("Some({})", str_literal(&qualified(function))),
                    None => "None".to_owned(),
                }
            )
        }
    };
    let children: Vec<String> = node.children.iter().map(|c| c.to_string()).collect();
    writeln!(
        out,
        "    Advancement {{ name: {}, parent: {}, root: {}, children: &[{}], display: {display}, \
         criteria: &[{}], requirements: &[{}], rewards: {rewards}, sends_telemetry_event: {}, \
         position: [{:?}, {:?}] }},",
        str_literal(&node.name),
        match node.parent {
            Some(parent) => format!("Some({parent})"),
            None => "None".to_owned(),
        },
        node.root,
        children.join(", "),
        criteria.join(", "),
        requirements.join(", "),
        advancement.sends_telemetry_event,
        node.position.0,
        node.position.1,
    )?;
    Ok(())
}

/// A displayed node as the layout sees it: invisible advancements are
/// skipped, their displayed descendants hanging from the nearest displayed
/// ancestor.
struct Place {
    node: usize,
    parent: Option<usize>,
    previous: Option<usize>,
    child_index: usize,
    children: Vec<usize>,
    ancestor: usize,
    thread: Option<usize>,
    x: i32,
    y: f32,
    modifier: f32,
    change: f32,
    shift: f32,
}

/// The advancement screen positions of a displayed root's tree: a tidy
/// tree drawing (Walker's algorithm with Buchheim's linear-time
/// refinements), depth along x and siblings stacked along y.
fn layout(nodes: &[Node], root: usize) -> Vec<(usize, (f32, f32))> {
    let mut places: Vec<Place> = Vec::new();
    build(nodes, &mut places, root, None, None, 1, 0);
    first_walk(&mut places, 0);
    let start = places[0].y;
    let min = second_walk(&mut places, 0, 0.0, 0, start);
    if min < 0.0 {
        third_walk(&mut places, 0, -min);
    }
    places
        .iter()
        .map(|place| (place.node, (place.x as f32, place.y)))
        .collect()
}

fn build(
    nodes: &[Node],
    places: &mut Vec<Place>,
    node: usize,
    parent: Option<usize>,
    previous: Option<usize>,
    child_index: usize,
    depth: i32,
) -> usize {
    let id = places.len();
    places.push(Place {
        node,
        parent,
        previous,
        child_index,
        children: Vec::new(),
        ancestor: id,
        thread: None,
        x: depth,
        y: -1.0,
        modifier: 0.0,
        change: 0.0,
        shift: 0.0,
    });
    let mut previous = None;
    for &child in &nodes[node].children {
        previous = add_child(nodes, places, id, child, previous);
    }
    id
}

fn add_child(
    nodes: &[Node],
    places: &mut Vec<Place>,
    place: usize,
    child: usize,
    mut previous: Option<usize>,
) -> Option<usize> {
    if nodes[child].advancement.display.is_some() {
        let index = places[place].children.len() + 1;
        let depth = places[place].x + 1;
        let id = build(nodes, places, child, Some(place), previous, index, depth);
        places[place].children.push(id);
        previous = Some(id);
    } else {
        for &grandchild in &nodes[child].children {
            previous = add_child(nodes, places, place, grandchild, previous);
        }
    }
    previous
}

fn first_walk(places: &mut [Place], v: usize) {
    if places[v].children.is_empty() {
        places[v].y = match places[v].previous {
            Some(previous) => places[previous].y + 1.0,
            None => 0.0,
        };
        return;
    }
    let mut default_ancestor: Option<usize> = None;
    for child in places[v].children.clone() {
        first_walk(places, child);
        default_ancestor = Some(apportion(places, child, default_ancestor.unwrap_or(child)));
    }
    execute_shifts(places, v);
    let children = &places[v].children;
    let midpoint = (places[children[0]].y + places[*children.last().unwrap()].y) / 2.0;
    match places[v].previous {
        Some(previous) => {
            places[v].y = places[previous].y + 1.0;
            places[v].modifier = places[v].y - midpoint;
        }
        None => places[v].y = midpoint,
    }
}

fn second_walk(places: &mut [Place], v: usize, mod_sum: f32, depth: i32, mut min: f32) -> f32 {
    places[v].y += mod_sum;
    places[v].x = depth;
    if places[v].y < min {
        min = places[v].y;
    }
    let modifier = places[v].modifier;
    for child in places[v].children.clone() {
        min = second_walk(places, child, mod_sum + modifier, depth + 1, min);
    }
    min
}

fn third_walk(places: &mut [Place], v: usize, offset: f32) {
    places[v].y += offset;
    for child in places[v].children.clone() {
        third_walk(places, child, offset);
    }
}

fn execute_shifts(places: &mut [Place], v: usize) {
    let mut shift = 0.0f32;
    let mut change = 0.0f32;
    for &child in places[v].children.clone().iter().rev() {
        places[child].y += shift;
        places[child].modifier += shift;
        change += places[child].change;
        shift += places[child].shift + change;
    }
}

fn previous_or_thread(places: &[Place], v: usize) -> Option<usize> {
    places[v]
        .thread
        .or_else(|| places[v].children.first().copied())
}

fn next_or_thread(places: &[Place], v: usize) -> Option<usize> {
    places[v]
        .thread
        .or_else(|| places[v].children.last().copied())
}

fn apportion(places: &mut [Place], v: usize, mut default_ancestor: usize) -> usize {
    let Some(previous) = places[v].previous else {
        return default_ancestor;
    };
    let parent = places[v]
        .parent
        .expect("a node with a sibling has a parent");
    let mut vir = v;
    let mut vor = v;
    let mut vil = previous;
    let mut vol = places[parent].children[0];
    let mut sir = places[v].modifier;
    let mut sor = places[v].modifier;
    let mut sil = places[vil].modifier;
    let mut sol = places[vol].modifier;
    while let (Some(next_vil), Some(next_vir)) =
        (next_or_thread(places, vil), previous_or_thread(places, vir))
    {
        vil = next_vil;
        vir = next_vir;
        vol = previous_or_thread(places, vol).expect("left contour continues");
        vor = next_or_thread(places, vor).expect("right contour continues");
        places[vor].ancestor = v;
        let shift = (places[vil].y + sil) - (places[vir].y + sir) + 1.0;
        if shift > 0.0 {
            let ancestor = ancestor_of(places, vil, v, default_ancestor);
            move_subtree(places, ancestor, v, shift);
            sir += shift;
            sor += shift;
        }
        sil += places[vil].modifier;
        sir += places[vir].modifier;
        sol += places[vol].modifier;
        sor += places[vor].modifier;
    }
    if let Some(next) = next_or_thread(places, vil)
        && next_or_thread(places, vor).is_none()
    {
        places[vor].thread = Some(next);
        places[vor].modifier += sil - sor;
    } else {
        if let Some(previous) = previous_or_thread(places, vir)
            && previous_or_thread(places, vol).is_none()
        {
            places[vol].thread = Some(previous);
            places[vol].modifier += sir - sol;
        }
        default_ancestor = v;
    }
    default_ancestor
}

fn move_subtree(places: &mut [Place], left: usize, right: usize, shift: f32) {
    let subtrees = places[right].child_index as f32 - places[left].child_index as f32;
    if subtrees != 0.0 {
        places[right].change -= shift / subtrees;
        places[left].change += shift / subtrees;
    }
    places[right].shift += shift;
    places[right].y += shift;
    places[right].modifier += shift;
}

fn ancestor_of(places: &[Place], v: usize, other: usize, default_ancestor: usize) -> usize {
    let ancestor = places[v].ancestor;
    let parent = places[other]
        .parent
        .expect("a node with a sibling has a parent");
    if places[parent].children.contains(&ancestor) {
        ancestor
    } else {
        default_ancestor
    }
}
