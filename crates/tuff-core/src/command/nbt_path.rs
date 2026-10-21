//! NBT paths (`Items[0].id`, `{a:1}`, `list[{x:1}]`, ...): parsing and the
//! reads and edits `/data` and `execute store` do through them.
//!
//! Matches are tracked as locations (a key or index per step from the root)
//! rather than references, so one path can read, create, set and remove in
//! the same tree without aliasing.

use tuff_item::text::TextComponent;
use tuff_nbt::{Compound, Tag};

use super::args::read_nbt_compound;
use super::error::CommandError;
use super::reader::Reader;
use super::selector::tag_matches;
use super::{java_double, java_float};

const MAX_DEPTH: usize = 512;

#[derive(Debug, Clone, PartialEq)]
enum Node {
    Child(String),
    Index(i32),
    MatchElement(Compound),
    AllElements,
    MatchObject(String, Compound),
    MatchRoot(Compound),
}

#[derive(Debug, Clone, PartialEq)]
enum Seg {
    Key(String),
    Index(usize),
}

type Loc = Vec<Seg>;

/// A parsed path with the text it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct NbtPath {
    text: String,
    nodes: Vec<Node>,
    ends: Vec<usize>,
}

fn invalid_node(reader: &Reader<'_>) -> CommandError {
    CommandError::simple("arguments.nbtpath.node.invalid").at(reader)
}

fn is_name_char(c: char) -> bool {
    !matches!(c, ' ' | '"' | '\'' | '[' | ']' | '.' | '{' | '}')
}

fn read_object_node(reader: &mut Reader<'_>, name: String) -> Result<Node, CommandError> {
    if name.is_empty() {
        return Err(invalid_node(reader));
    }
    if reader.peek_is('{') {
        let pattern = read_nbt_compound(reader)?;
        return Ok(Node::MatchObject(name, pattern));
    }
    Ok(Node::Child(name))
}

fn read_node(reader: &mut Reader<'_>, first: bool) -> Result<Node, CommandError> {
    match reader.peek() {
        Some('"' | '\'') => {
            let name = reader.read_quoted_string()?;
            read_object_node(reader, name)
        }
        Some('[') => {
            reader.skip();
            match reader.peek() {
                Some('{') => {
                    let pattern = read_nbt_compound(reader)?;
                    reader.expect(']')?;
                    Ok(Node::MatchElement(pattern))
                }
                Some(']') => {
                    reader.skip();
                    Ok(Node::AllElements)
                }
                _ => {
                    let index = reader.read_int()?;
                    reader.expect(']')?;
                    Ok(Node::Index(index))
                }
            }
        }
        Some('{') => {
            if !first {
                return Err(invalid_node(reader));
            }
            Ok(Node::MatchRoot(read_nbt_compound(reader)?))
        }
        _ => {
            let name = reader.read_while(is_name_char).to_owned();
            if name.is_empty() {
                return Err(invalid_node(reader));
            }
            read_object_node(reader, name)
        }
    }
}

impl NbtPath {
    pub fn parse(reader: &mut Reader<'_>) -> Result<Self, CommandError> {
        let start = reader.cursor();
        let mut nodes = Vec::new();
        let mut ends = Vec::new();
        while reader.can_read() && !reader.peek_is(' ') {
            nodes.push(read_node(reader, nodes.is_empty())?);
            ends.push(reader.cursor() - start);
            if let Some(next) = reader.peek()
                && !matches!(next, ' ' | '[' | '{')
            {
                reader.expect('.')?;
            }
        }
        if nodes.is_empty() {
            return Err(invalid_node(reader));
        }
        Ok(Self {
            text: reader.input()[start..reader.cursor()].to_owned(),
            nodes,
            ends,
        })
    }

    pub fn from_text(text: &str) -> Result<Self, CommandError> {
        Self::parse(&mut Reader::new(text))
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    fn not_found(&self, node: usize) -> CommandError {
        CommandError::dynamic(
            "arguments.nbtpath.nothing_found",
            [self.text[..self.ends[node]].to_owned()],
        )
    }

    fn walk(&self, root: &Tag, upto: usize) -> Result<Vec<Loc>, usize> {
        let mut locs: Vec<Loc> = vec![Vec::new()];
        for (index, node) in self.nodes[..upto].iter().enumerate() {
            locs = locs
                .iter()
                .flat_map(|loc| children(node, root, loc))
                .collect();
            if locs.is_empty() {
                return Err(index);
            }
        }
        Ok(locs)
    }

    /// Every tag the path matches; an error naming the first step that
    /// matched nothing.
    pub fn get(&self, root: &Tag) -> Result<Vec<Tag>, CommandError> {
        let locs = self
            .walk(root, self.nodes.len())
            .map_err(|node| self.not_found(node))?;
        Ok(locs.iter().filter_map(|loc| value_at(root, loc)).collect())
    }

    pub fn count_matching(&self, root: &Tag) -> usize {
        self.walk(root, self.nodes.len()).map_or(0, |l| l.len())
    }

    fn parents(&self, root: &mut Tag) -> Result<Vec<Loc>, CommandError> {
        let mut locs: Vec<Loc> = vec![Vec::new()];
        let last = self.nodes.len() - 1;
        for index in 0..last {
            let node = &self.nodes[index];
            let preferred = preferred_parent(&self.nodes[index + 1]);
            locs = locs
                .iter()
                .flat_map(|loc| children_or_create(node, root, loc, &|| preferred.clone()))
                .collect();
            if locs.is_empty() {
                return Err(self.not_found(index));
            }
        }
        Ok(locs)
    }

    /// Every tag the path matches, creating the missing ones from
    /// `create`.
    fn get_or_create(
        &self,
        root: &mut Tag,
        create: &dyn Fn() -> Tag,
    ) -> Result<Vec<Loc>, CommandError> {
        let parents = self.parents(root)?;
        let last = &self.nodes[self.nodes.len() - 1];
        Ok(parents
            .iter()
            .flat_map(|loc| children_or_create(last, root, loc, create))
            .collect())
    }

    /// Puts `value` at every match, creating the path as needed; how many
    /// places changed.
    pub fn set(&self, root: &mut Tag, value: &Tag) -> Result<usize, CommandError> {
        if too_deep(value, self.nodes.len()) {
            return Err(CommandError::simple("arguments.nbtpath.too_deep"));
        }
        let parents = self.parents(root)?;
        let last = &self.nodes[self.nodes.len() - 1];
        Ok(parents
            .iter()
            .map(|loc| match ref_mut_at(root, loc) {
                Some(parent) => set_tag(last, parent, value),
                None => 0,
            })
            .sum())
    }

    /// Inserts copies of `values` at `index` (negative counts from the
    /// end, `-1` appends) in every list the path finds or creates.
    pub fn insert(
        &self,
        index: i32,
        root: &mut Tag,
        values: &[Tag],
    ) -> Result<usize, CommandError> {
        for value in values {
            if too_deep(value, self.nodes.len()) {
                return Err(CommandError::simple("arguments.nbtpath.too_deep"));
            }
        }
        let locs = self.get_or_create(root, &|| Tag::List(Vec::new()))?;
        let mut modified = 0;
        for loc in locs {
            let Some(target) = ref_mut_at(root, &loc) else {
                let shown = value_at(root, &loc)
                    .map(|tag| pretty(&tag))
                    .unwrap_or_default();
                return Err(expected_list(&shown));
            };
            let Some(size) = collection_len(target) else {
                return Err(expected_list(&pretty(target)));
            };
            let mut at = if index < 0 {
                size as i64 + i64::from(index) + 1
            } else {
                i64::from(index)
            };
            let mut changed = false;
            for value in values {
                if at < 0 || at as usize > collection_len(target).unwrap_or(0) {
                    return Err(CommandError::dynamic(
                        "commands.data.modify.invalid_index",
                        [at.to_string()],
                    ));
                }
                if add_element(target, at as usize, value.clone()) {
                    at += 1;
                    changed = true;
                }
            }
            modified += usize::from(changed);
        }
        Ok(modified)
    }

    /// Merges `sources` into every compound the path finds or creates.
    pub fn merge_into(&self, root: &mut Tag, merged: &Compound) -> Result<usize, CommandError> {
        let locs = self.get_or_create(root, &|| Tag::Compound(Compound::new()))?;
        let mut changed = 0;
        for loc in locs {
            match ref_mut_at(root, &loc) {
                Some(Tag::Compound(target)) => {
                    let before = target.clone();
                    merge(target, merged);
                    changed += usize::from(before != *target);
                }
                Some(other) => {
                    return Err(CommandError::dynamic(
                        "commands.data.modify.expected_object",
                        [pretty(other)],
                    ));
                }
                None => {
                    let shown = value_at(root, &loc)
                        .map(|tag| pretty(&tag))
                        .unwrap_or_default();
                    return Err(CommandError::dynamic(
                        "commands.data.modify.expected_object",
                        [shown],
                    ));
                }
            }
        }
        Ok(changed)
    }

    /// Removes every match; how many went.
    pub fn remove(&self, root: &mut Tag) -> usize {
        let last = self.nodes.len() - 1;
        let Ok(parents) = self.walk(root, last) else {
            return 0;
        };
        let node = &self.nodes[last];
        parents
            .iter()
            .map(|loc| match ref_mut_at(root, loc) {
                Some(parent) => remove_tag(node, parent),
                None => 0,
            })
            .sum()
    }
}

fn expected_list(text: &str) -> CommandError {
    CommandError::dynamic("commands.data.modify.expected_list", [text.to_owned()])
}

fn preferred_parent(node: &Node) -> Tag {
    match node {
        Node::Child(_) | Node::MatchObject(..) | Node::MatchRoot(_) => {
            Tag::Compound(Compound::new())
        }
        Node::Index(_) | Node::AllElements | Node::MatchElement(_) => Tag::List(Vec::new()),
    }
}

/// Whether `tag` nests `MAX_DEPTH` levels deep when put `depth` levels
/// down.
pub fn too_deep(tag: &Tag, depth: usize) -> bool {
    if depth >= MAX_DEPTH {
        return true;
    }
    match tag {
        Tag::Compound(compound) => compound.values().any(|child| too_deep(child, depth + 1)),
        Tag::List(items) => items.iter().any(|item| too_deep(item, depth + 1)),
        _ => false,
    }
}

/// `target` with every key of `source` laid over it, compounds merging
/// recursively and everything else replaced.
pub fn merge(target: &mut Compound, source: &Compound) {
    for (key, value) in source.iter() {
        match (target.get_mut(key), value) {
            (Some(Tag::Compound(existing)), Tag::Compound(incoming)) => merge(existing, incoming),
            _ => {
                target.insert(key, value.clone());
            }
        }
    }
}

fn collection_len(tag: &Tag) -> Option<usize> {
    match tag {
        Tag::List(v) => Some(v.len()),
        Tag::ByteArray(v) => Some(v.len()),
        Tag::IntArray(v) => Some(v.len()),
        Tag::LongArray(v) => Some(v.len()),
        _ => None,
    }
}

fn element(tag: &Tag, index: usize) -> Option<Tag> {
    match tag {
        Tag::List(v) => v.get(index).cloned(),
        Tag::ByteArray(v) => v.get(index).map(|x| Tag::Byte(*x)),
        Tag::IntArray(v) => v.get(index).map(|x| Tag::Int(*x)),
        Tag::LongArray(v) => v.get(index).map(|x| Tag::Long(*x)),
        _ => None,
    }
}

fn whole_number(tag: &Tag) -> Option<i64> {
    match tag {
        Tag::Byte(v) => Some(i64::from(*v)),
        Tag::Short(v) => Some(i64::from(*v)),
        Tag::Int(v) => Some(i64::from(*v)),
        Tag::Long(v) => Some(*v),
        Tag::Float(v) => Some(f64::from(*v).floor() as i64),
        Tag::Double(v) => Some(v.floor() as i64),
        _ => None,
    }
}

fn set_element(parent: &mut Tag, index: usize, value: &Tag) -> bool {
    match parent {
        Tag::List(v) => {
            v[index] = value.clone();
            true
        }
        Tag::ByteArray(v) => whole_number(value).is_some_and(|n| {
            v[index] = n as i8;
            true
        }),
        Tag::IntArray(v) => whole_number(value).is_some_and(|n| {
            v[index] = n as i32;
            true
        }),
        Tag::LongArray(v) => whole_number(value).is_some_and(|n| {
            v[index] = n;
            true
        }),
        _ => false,
    }
}

fn add_element(parent: &mut Tag, index: usize, value: Tag) -> bool {
    match parent {
        Tag::List(v) => {
            v.insert(index, value);
            true
        }
        Tag::ByteArray(v) => whole_number(&value).is_some_and(|n| {
            v.insert(index, n as i8);
            true
        }),
        Tag::IntArray(v) => whole_number(&value).is_some_and(|n| {
            v.insert(index, n as i32);
            true
        }),
        Tag::LongArray(v) => whole_number(&value).is_some_and(|n| {
            v.insert(index, n);
            true
        }),
        _ => false,
    }
}

fn remove_element(parent: &mut Tag, index: usize) {
    match parent {
        Tag::List(v) => {
            v.remove(index);
        }
        Tag::ByteArray(v) => {
            v.remove(index);
        }
        Tag::IntArray(v) => {
            v.remove(index);
        }
        Tag::LongArray(v) => {
            v.remove(index);
        }
        _ => {}
    }
}

fn clear_collection(parent: &mut Tag) {
    match parent {
        Tag::List(v) => v.clear(),
        Tag::ByteArray(v) => v.clear(),
        Tag::IntArray(v) => v.clear(),
        Tag::LongArray(v) => v.clear(),
        _ => {}
    }
}

fn resolve_index(index: i32, size: usize) -> Option<usize> {
    let actual = if index < 0 {
        size as i64 + i64::from(index)
    } else {
        i64::from(index)
    };
    (0..size as i64)
        .contains(&actual)
        .then_some(actual as usize)
}

fn ref_at<'a>(root: &'a Tag, loc: &[Seg]) -> Option<&'a Tag> {
    let mut current = root;
    for seg in loc {
        current = match (seg, current) {
            (Seg::Key(key), Tag::Compound(c)) => c.get(key)?,
            (Seg::Index(i), Tag::List(v)) => v.get(*i)?,
            _ => return None,
        };
    }
    Some(current)
}

fn ref_mut_at<'a>(root: &'a mut Tag, loc: &[Seg]) -> Option<&'a mut Tag> {
    let mut current = root;
    for seg in loc {
        current = match (seg, current) {
            (Seg::Key(key), Tag::Compound(c)) => c.get_mut(key)?,
            (Seg::Index(i), Tag::List(v)) => v.get_mut(*i)?,
            _ => return None,
        };
    }
    Some(current)
}

fn value_at(root: &Tag, loc: &[Seg]) -> Option<Tag> {
    let Some((last, parents)) = loc.split_last() else {
        return Some(root.clone());
    };
    let parent = ref_at(root, parents)?;
    match last {
        Seg::Key(key) => parent.as_compound()?.get(key).cloned(),
        Seg::Index(index) => element(parent, *index),
    }
}

fn pushed(loc: &Loc, seg: Seg) -> Loc {
    let mut next = loc.clone();
    next.push(seg);
    next
}

fn pattern_matches(pattern: &Compound, tag: &Tag) -> bool {
    matches!(tag, Tag::Compound(actual) if super::selector::compound_matches(pattern, actual))
}

fn children(node: &Node, root: &Tag, loc: &Loc) -> Vec<Loc> {
    let Some(parent) = ref_at(root, loc) else {
        return Vec::new();
    };
    match node {
        Node::Child(name) => match parent {
            Tag::Compound(c) if c.contains_key(name) => vec![pushed(loc, Seg::Key(name.clone()))],
            _ => Vec::new(),
        },
        Node::Index(index) => collection_len(parent)
            .and_then(|size| resolve_index(*index, size))
            .map(|i| vec![pushed(loc, Seg::Index(i))])
            .unwrap_or_default(),
        Node::AllElements => (0..collection_len(parent).unwrap_or(0))
            .map(|i| pushed(loc, Seg::Index(i)))
            .collect(),
        Node::MatchElement(pattern) => match parent {
            Tag::List(items) => items
                .iter()
                .enumerate()
                .filter(|(_, item)| pattern_matches(pattern, item))
                .map(|(i, _)| pushed(loc, Seg::Index(i)))
                .collect(),
            _ => Vec::new(),
        },
        Node::MatchObject(name, pattern) => match parent {
            Tag::Compound(c) => match c.get(name) {
                Some(child) if tag_matches(&Tag::Compound(pattern.clone()), child) => {
                    vec![pushed(loc, Seg::Key(name.clone()))]
                }
                _ => Vec::new(),
            },
            _ => Vec::new(),
        },
        Node::MatchRoot(pattern) => {
            if pattern_matches(pattern, parent) {
                vec![loc.clone()]
            } else {
                Vec::new()
            }
        }
    }
}

fn children_or_create(
    node: &Node,
    root: &mut Tag,
    loc: &Loc,
    create: &dyn Fn() -> Tag,
) -> Vec<Loc> {
    let Some(parent) = ref_mut_at(root, loc) else {
        return Vec::new();
    };
    match node {
        Node::Child(name) => match parent {
            Tag::Compound(c) => {
                if !c.contains_key(name) {
                    c.insert(name.clone(), create());
                }
                vec![pushed(loc, Seg::Key(name.clone()))]
            }
            _ => Vec::new(),
        },
        Node::AllElements => {
            let Some(size) = collection_len(parent) else {
                return Vec::new();
            };
            if size == 0 {
                if add_element(parent, 0, create()) {
                    return vec![pushed(loc, Seg::Index(0))];
                }
                return Vec::new();
            }
            (0..size).map(|i| pushed(loc, Seg::Index(i))).collect()
        }
        Node::MatchElement(pattern) => {
            let Tag::List(items) = parent else {
                return Vec::new();
            };
            let found: Vec<Loc> = items
                .iter()
                .enumerate()
                .filter(|(_, item)| pattern_matches(pattern, item))
                .map(|(i, _)| pushed(loc, Seg::Index(i)))
                .collect();
            if !found.is_empty() {
                return found;
            }
            items.push(Tag::Compound(pattern.clone()));
            vec![pushed(loc, Seg::Index(items.len() - 1))]
        }
        Node::MatchObject(name, pattern) => {
            let Tag::Compound(c) = parent else {
                return Vec::new();
            };
            match c.get(name) {
                None => {
                    c.insert(name.clone(), Tag::Compound(pattern.clone()));
                    vec![pushed(loc, Seg::Key(name.clone()))]
                }
                Some(child) if tag_matches(&Tag::Compound(pattern.clone()), child) => {
                    vec![pushed(loc, Seg::Key(name.clone()))]
                }
                Some(_) => Vec::new(),
            }
        }
        Node::Index(_) | Node::MatchRoot(_) => {
            let root: &Tag = root;
            children(node, root, loc)
        }
    }
}

fn set_tag(node: &Node, parent: &mut Tag, value: &Tag) -> usize {
    match node {
        Node::Child(name) => match parent {
            Tag::Compound(c) => {
                let previous = c.insert(name.clone(), value.clone());
                usize::from(previous.as_ref() != Some(value))
            }
            _ => 0,
        },
        Node::Index(index) => {
            let Some(index) = collection_len(parent).and_then(|n| resolve_index(*index, n)) else {
                return 0;
            };
            if element(parent, index).as_ref() == Some(value) {
                return 0;
            }
            usize::from(set_element(parent, index, value))
        }
        Node::MatchElement(pattern) => {
            let Tag::List(items) = parent else {
                return 0;
            };
            if items.is_empty() {
                items.push(value.clone());
                return 1;
            }
            let mut changed = 0;
            for item in items.iter_mut() {
                if pattern_matches(pattern, item) && item != value {
                    *item = value.clone();
                    changed += 1;
                }
            }
            changed
        }
        Node::AllElements => {
            let Some(size) = collection_len(parent) else {
                return 0;
            };
            if size == 0 {
                add_element(parent, 0, value.clone());
                return 1;
            }
            let same = (0..size)
                .filter(|i| element(parent, *i).as_ref() == Some(value))
                .count();
            let changed = size - same;
            if changed == 0 {
                return 0;
            }
            clear_collection(parent);
            if !add_element(parent, 0, value.clone()) {
                return 0;
            }
            for i in 1..size {
                add_element(parent, i, value.clone());
            }
            changed
        }
        Node::MatchObject(name, pattern) => {
            let Tag::Compound(c) = parent else {
                return 0;
            };
            match c.get(name) {
                Some(current)
                    if tag_matches(&Tag::Compound(pattern.clone()), current)
                        && current != value =>
                {
                    c.insert(name.clone(), value.clone());
                    1
                }
                _ => 0,
            }
        }
        Node::MatchRoot(_) => 0,
    }
}

fn remove_tag(node: &Node, parent: &mut Tag) -> usize {
    match node {
        Node::Child(name) => match parent {
            Tag::Compound(c) => usize::from(c.remove(name).is_some()),
            _ => 0,
        },
        Node::Index(index) => match collection_len(parent).and_then(|n| resolve_index(*index, n)) {
            Some(i) => {
                remove_element(parent, i);
                1
            }
            None => 0,
        },
        Node::MatchElement(pattern) => {
            let Tag::List(items) = parent else {
                return 0;
            };
            let before = items.len();
            items.retain(|item| !pattern_matches(pattern, item));
            before - items.len()
        }
        Node::AllElements => match collection_len(parent) {
            Some(size) if size > 0 => {
                clear_collection(parent);
                size
            }
            _ => 0,
        },
        Node::MatchObject(name, pattern) => {
            let Tag::Compound(c) = parent else {
                return 0;
            };
            match c.get(name) {
                Some(current) if tag_matches(&Tag::Compound(pattern.clone()), current) => {
                    c.remove(name);
                    1
                }
                _ => 0,
            }
        }
        Node::MatchRoot(_) => 0,
    }
}

fn quoted(text: &str) -> String {
    let quote = if text.contains('"') && !text.contains('\'') {
        '\''
    } else {
        '"'
    };
    let mut out = String::new();
    out.push(quote);
    for c in text.chars() {
        if c == '\\' || c == quote {
            out.push('\\');
        }
        out.push(c);
    }
    out.push(quote);
    out
}

fn simple_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'))
}

const FOLD_AT: usize = 128;
const FOLD_DEPTH: usize = 64;

type Pieces = Vec<(String, Option<&'static str>)>;

fn piece(out: &mut Pieces, text: impl Into<String>, color: Option<&'static str>) {
    out.push((text.into(), color));
}

fn number(out: &mut Pieces, value: impl std::fmt::Display, suffix: &str) {
    piece(out, value.to_string(), Some("gold"));
    if !suffix.is_empty() {
        piece(out, suffix, Some("red"));
    }
}

/// A tag as `/data` prints it: one line, `key: value` pairs.
pub fn pretty(tag: &Tag) -> String {
    pieces(tag).into_iter().map(|(text, _)| text).collect()
}

/// The same text as [`pretty`], coloured the way the game colours it.
pub fn pretty_component(tag: &Tag) -> TextComponent {
    let mut root = TextComponent::new("");
    for (text, color) in pieces(tag) {
        let part = TextComponent::new(text);
        root = root.append(match color {
            Some(color) => part.color(color),
            None => part,
        });
    }
    root
}

fn pieces(tag: &Tag) -> Pieces {
    let mut out = Vec::new();
    write_pretty(tag, 0, &mut out);
    out
}

fn write_array<T>(
    out: &mut Pieces,
    prefix: &str,
    items: &[T],
    value: impl Fn(&T) -> (String, &'static str),
) {
    piece(out, "[", None);
    piece(out, prefix, Some("red"));
    piece(out, ";", None);
    for (i, item) in items.iter().take(FOLD_AT).enumerate() {
        piece(out, " ", None);
        let (text, suffix) = value(item);
        number(out, text, suffix);
        if i + 1 != items.len() {
            piece(out, ",", None);
        }
    }
    if items.len() > FOLD_AT {
        piece(out, "<...>", None);
    }
    piece(out, "]", None);
}

fn write_pretty(tag: &Tag, depth: usize, out: &mut Pieces) {
    match tag {
        Tag::Byte(v) => number(out, v, "b"),
        Tag::Short(v) => number(out, v, "s"),
        Tag::Int(v) => number(out, v, ""),
        Tag::Long(v) => number(out, v, "L"),
        Tag::Float(v) => number(out, java_float(*v), "f"),
        Tag::Double(v) => number(out, java_double(*v), "d"),
        Tag::String(s) => piece(out, quoted(s), Some("green")),
        Tag::ByteArray(v) => write_array(out, "B", v, |x| (x.to_string(), "b")),
        Tag::IntArray(v) => write_array(out, "I", v, |x| (x.to_string(), "")),
        Tag::LongArray(v) => write_array(out, "L", v, |x| (x.to_string(), "L")),
        Tag::List(items) => {
            if items.is_empty() {
                piece(out, "[]", None);
            } else if depth >= FOLD_DEPTH {
                piece(out, "[<...>]", None);
            } else {
                piece(out, "[", None);
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        piece(out, ", ", None);
                    }
                    if i >= FOLD_AT {
                        piece(out, "<...>", None);
                        break;
                    }
                    write_pretty(item, depth + 1, out);
                }
                piece(out, "]", None);
            }
        }
        Tag::Compound(compound) => {
            if compound.is_empty() {
                piece(out, "{}", None);
            } else if depth >= FOLD_DEPTH {
                piece(out, "{<...>}", None);
            } else {
                piece(out, "{", None);
                let len = compound.len();
                for (i, (key, value)) in compound.iter().enumerate() {
                    let name = if simple_key(key) {
                        key.to_string()
                    } else {
                        quoted(key)
                    };
                    piece(out, name, Some("aqua"));
                    piece(out, ": ", None);
                    write_pretty(value, depth + 1, out);
                    if i + 1 < len {
                        piece(out, ", ", None);
                    }
                }
                piece(out, "}", None);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(text: &str) -> NbtPath {
        NbtPath::from_text(text).unwrap()
    }

    fn root(snbt: &str) -> Tag {
        Tag::from_snbt(snbt).unwrap()
    }

    fn get(path_text: &str, snbt: &str) -> Vec<String> {
        path(path_text)
            .get(&root(snbt))
            .unwrap()
            .iter()
            .map(pretty)
            .collect()
    }

    #[tuff_core::watchdog]
    #[test]
    fn parsing_covers_every_node_kind() {
        assert_eq!(path("a.b[0]").nodes.len(), 3);
        assert_eq!(path("{a:1}.b").nodes.len(), 2);
        assert_eq!(path("a{x:1}").nodes.len(), 1);
        assert_eq!(path("\"a b\".c").nodes[0], Node::Child("a b".into()));
        assert_eq!(path("a[-1]").nodes[1], Node::Index(-1));
        assert_eq!(path("a[]").nodes[1], Node::AllElements);
        assert_eq!(path("a[{x:1}]").nodes.len(), 2);
        let err = |text: &str| {
            NbtPath::from_text(text)
                .unwrap_err()
                .key()
                .map(str::to_owned)
        };
        assert_eq!(
            err("a{b:1}{c:1}").as_deref(),
            Some("arguments.nbtpath.node.invalid")
        );
        assert_eq!(
            err("a.{b:1}").as_deref(),
            Some("arguments.nbtpath.node.invalid")
        );
        assert_eq!(err("").as_deref(), Some("arguments.nbtpath.node.invalid"));
        assert_eq!(
            err("a..b").as_deref(),
            Some("arguments.nbtpath.node.invalid")
        );
        assert!(err("a[").is_some());
        assert!(err("a[1").is_some());
    }

    #[tuff_core::watchdog]
    #[test]
    fn parsing_stops_at_a_space() {
        let mut reader = Reader::new("a.b rest");
        let parsed = NbtPath::parse(&mut reader).unwrap();
        assert_eq!(parsed.as_str(), "a.b");
        assert_eq!(reader.remaining(), " rest");
    }

    #[tuff_core::watchdog]
    #[test]
    fn get_follows_children_indices_and_filters() {
        let data = "{a:{b:[{x:1,y:2},{x:3,y:4}]},s:\"hi\",arr:[I;5,6,7]}";
        assert_eq!(get("a.b[1].y", data), ["4"]);
        assert_eq!(get("a.b[-1].x", data), ["3"]);
        assert_eq!(get("a.b[].x", data), ["1", "3"]);
        assert_eq!(get("a.b[{x:3}].y", data), ["4"]);
        assert_eq!(get("arr[1]", data), ["6"]);
        assert_eq!(get("arr[]", data), ["5", "6", "7"]);
        assert_eq!(get("a{b:[{x:1}]}.b[0].y", data), ["2"]);
        assert_eq!(get("{s:\"hi\"}.s", data), ["\"hi\""]);
        assert_eq!(get("s", data), ["\"hi\""]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn get_names_the_first_step_that_matches_nothing() {
        let data = root("{a:{b:1}}");
        let err = path("a.c.d").get(&data).unwrap_err();
        assert_eq!(err.key(), Some("arguments.nbtpath.nothing_found"));
        assert_eq!(err.message.with[0].plain(), "a.c");
        assert!(path("a.b[0]").get(&data).is_err());
        assert!(path("{a:2}").get(&data).is_err());
        assert_eq!(path("a.b").count_matching(&data), 1);
        assert_eq!(path("a.q").count_matching(&data), 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn set_creates_parents_and_counts_changes() {
        let mut data = root("{}");
        assert_eq!(path("a.b.c").set(&mut data, &Tag::Int(1)).unwrap(), 1);
        assert_eq!(pretty(&data), "{a: {b: {c: 1}}}");
        assert_eq!(path("a.b.c").set(&mut data, &Tag::Int(1)).unwrap(), 0);
        assert_eq!(path("a.b.c").set(&mut data, &Tag::Int(2)).unwrap(), 1);
        assert_eq!(path("l[0]").set(&mut data, &Tag::Int(5)).unwrap(), 0);
        assert_eq!(pretty(&data), "{a: {b: {c: 2}}, l: []}");
        assert_eq!(path("l[]").set(&mut data, &Tag::Int(5)).unwrap(), 1);
        assert_eq!(pretty(&data), "{a: {b: {c: 2}}, l: [5]}");
        path("l").set(&mut data, &root("[1,2,3]")).unwrap();
        assert_eq!(path("l[]").set(&mut data, &Tag::Int(9)).unwrap(), 3);
        assert_eq!(pretty(&data), "{a: {b: {c: 2}}, l: [9, 9, 9]}");
        assert_eq!(path("l[1]").set(&mut data, &Tag::Int(7)).unwrap(), 1);
        assert_eq!(path("l[-1]").set(&mut data, &Tag::Int(8)).unwrap(), 1);
        assert_eq!(pretty(&data), "{a: {b: {c: 2}}, l: [9, 7, 8]}");
    }

    #[tuff_core::watchdog]
    #[test]
    fn set_through_a_filter_creates_the_pattern() {
        let mut data = root("{}");
        path("l[{id:1}].n").set(&mut data, &Tag::Int(4)).unwrap();
        assert_eq!(pretty(&data), "{l: [{id: 1, n: 4}]}");
        path("l[{id:1}].n").set(&mut data, &Tag::Int(5)).unwrap();
        assert_eq!(pretty(&data), "{l: [{id: 1, n: 5}]}");
        assert_eq!(
            path("o{k:1}").set(&mut data, &root("{k:1,z:2}")).unwrap(),
            0
        );
        path("o").set(&mut data, &root("{k:1}")).unwrap();
        assert_eq!(
            path("o{k:1}").set(&mut data, &root("{k:1,z:2}")).unwrap(),
            1
        );
        assert_eq!(pretty(&data), "{l: [{id: 1, n: 5}], o: {k: 1, z: 2}}");
    }

    #[tuff_core::watchdog]
    #[test]
    fn set_on_arrays_converts_numbers() {
        let mut data = root("{arr:[B;1,2,3]}");
        assert_eq!(path("arr[1]").set(&mut data, &Tag::Int(9)).unwrap(), 1);
        assert_eq!(pretty(&data), "{arr: [B; 1b, 9b, 3b]}");
        assert_eq!(
            path("arr[1]")
                .set(&mut data, &Tag::String("x".into()))
                .unwrap(),
            0
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn set_refuses_data_nested_too_deep() {
        let mut deep = Tag::Int(0);
        for _ in 0..MAX_DEPTH {
            deep = Tag::List(vec![deep]);
        }
        let mut data = root("{}");
        let err = path("a").set(&mut data, &deep).unwrap_err();
        assert_eq!(err.key(), Some("arguments.nbtpath.too_deep"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn insert_places_values_in_lists() {
        let mut data = root("{l:[1,2]}");
        assert_eq!(path("l").insert(1, &mut data, &[Tag::Int(9)]).unwrap(), 1);
        assert_eq!(pretty(&data), "{l: [1, 9, 2]}");
        path("l")
            .insert(-1, &mut data, &[Tag::Int(7), Tag::Int(8)])
            .unwrap();
        assert_eq!(pretty(&data), "{l: [1, 9, 2, 7, 8]}");
        path("l").insert(0, &mut data, &[Tag::Int(0)]).unwrap();
        assert_eq!(pretty(&data), "{l: [0, 1, 9, 2, 7, 8]}");
        path("fresh").insert(0, &mut data, &[Tag::Int(1)]).unwrap();
        assert!(pretty(&data).ends_with("fresh: [1]}"));
        let err = path("l").insert(99, &mut data, &[Tag::Int(1)]).unwrap_err();
        assert_eq!(err.key(), Some("commands.data.modify.invalid_index"));
        let err = path("l[0]")
            .insert(0, &mut data, &[Tag::Int(1)])
            .unwrap_err();
        assert_eq!(err.key(), Some("commands.data.modify.expected_list"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn remove_deletes_every_match() {
        let mut data = root("{a:1,l:[{x:1},{x:2},{x:1}],m:[1,2,3]}");
        assert_eq!(path("a").remove(&mut data), 1);
        assert_eq!(path("a").remove(&mut data), 0);
        assert_eq!(path("l[{x:1}]").remove(&mut data), 2);
        assert_eq!(path("m[-1]").remove(&mut data), 1);
        assert_eq!(pretty(&data), "{l: [{x: 2}], m: [1, 2]}");
        assert_eq!(path("m[]").remove(&mut data), 2);
        assert_eq!(pretty(&data), "{l: [{x: 2}], m: []}");
        assert_eq!(path("l[].x").remove(&mut data), 1);
        assert_eq!(path("nothing.here").remove(&mut data), 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn merge_combines_compounds_recursively() {
        let mut data = root("{o:{a:1,n:{x:1}}}");
        let merged = match root("{b:2,n:{y:2},l:[1]}") {
            Tag::Compound(c) => c,
            _ => unreachable!(),
        };
        assert_eq!(path("o").merge_into(&mut data, &merged).unwrap(), 1);
        assert_eq!(pretty(&data), "{o: {a: 1, n: {x: 1, y: 2}, b: 2, l: [1]}}");
        assert_eq!(path("o").merge_into(&mut data, &merged).unwrap(), 0);
        let err = path("o.a").merge_into(&mut data, &merged).unwrap_err();
        assert_eq!(err.key(), Some("commands.data.modify.expected_object"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn pretty_component_colours_like_the_game() {
        let component = pretty_component(&root("{k:5b,s:\"x\"}"));
        assert_eq!(component.plain(), "{k: 5b, s: \"x\"}");
        let value = component.to_value();
        let colours: Vec<_> = value["extra"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| Some((c["text"].as_str()?, c["color"].as_str()?)))
            .collect();
        assert_eq!(
            colours,
            [
                ("k", "aqua"),
                ("5", "gold"),
                ("b", "red"),
                ("s", "aqua"),
                ("\"x\"", "green")
            ]
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn pretty_matches_the_game_text() {
        assert_eq!(
            pretty(&root(
                "{a:1b,b:2s,c:3,d:4L,e:1.5f,f:2.0d,s:\"x\",\"a b\":1}"
            )),
            "{a: 1b, b: 2s, c: 3, d: 4L, e: 1.5f, f: 2.0d, s: \"x\", \"a b\": 1}"
        );
        assert_eq!(pretty(&root("[I;1,2]")), "[I; 1, 2]");
        assert_eq!(pretty(&root("[B;1,2]")), "[B; 1b, 2b]");
        assert_eq!(pretty(&root("[L;1]")), "[L; 1L]");
        assert_eq!(pretty(&root("[]")), "[]");
        assert_eq!(pretty(&root("{}")), "{}");
        assert_eq!(
            pretty(&Tag::String("it's \"q\"".into())),
            "\"it's \\\"q\\\"\""
        );
    }
}
