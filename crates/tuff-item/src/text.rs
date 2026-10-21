//! Minecraft text components (https://minecraft.wiki/w/Text_component_format).

use serde::{Serialize, Serializer};
use serde_json::{Map, Value};

use crate::item::hash::Canon;
use tuff_nbt::{Compound, Tag};

/// A plain-text or translatable Minecraft text component with optional
/// styling and children. `text` is ignored when `translate` is set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextComponent {
    pub text: String,
    pub translate: Option<Box<str>>,
    pub with: Vec<TextComponent>,
    pub color: Option<Box<str>>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underlined: Option<bool>,
    pub strikethrough: Option<bool>,
    pub obfuscated: Option<bool>,
    /// Rarely used style and translation fields, boxed to keep components small.
    pub rare: Option<Box<Rare>>,
    pub extra: Vec<TextComponent>,
}

/// Fields most components never set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rare {
    /// Resource location of the font to render with.
    pub font: Option<Box<str>>,
    /// ARGB colour of the text shadow.
    pub shadow_color: Option<i32>,
    /// Shown instead of the key when a translation is missing.
    pub fallback: Option<Box<str>>,
    /// Contents resolved at send time or by the client, used instead of
    /// `text` and `translate` when set.
    pub special: Option<Box<Special>>,
    pub events: Events,
}

/// Component contents other than literal text and translations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Special {
    Selector {
        pattern: String,
        separator: Option<TextComponent>,
    },
    Score {
        name: String,
        objective: String,
    },
    Nbt {
        path: String,
        interpret: bool,
        plain: bool,
        separator: Option<TextComponent>,
        source: NbtSource,
    },
    Keybind(String),
    /// A sprite from an atlas (`object: atlas`).
    Atlas {
        atlas: Option<String>,
        sprite: String,
    },
    /// A player's head (`object: player`); the profile is kept as given.
    PlayerHead {
        profile: Value,
        hat: bool,
    },
}

/// Where an `nbt` component reads its data from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NbtSource {
    Block(String),
    Entity(String),
    Storage(String),
}

/// Limits on one resolution: how deep components nest and how many nbt
/// contents expand.
#[derive(Debug, Clone)]
pub struct ResolveBudget {
    pub depth_limit: usize,
    pub nbt_limit: usize,
    pub nbt_count: usize,
}

impl Default for ResolveBudget {
    fn default() -> Self {
        Self {
            depth_limit: 100,
            nbt_limit: 65536,
            nbt_count: 0,
        }
    }
}

/// World lookups needed to turn selector, score and nbt components into
/// plain components.
pub trait ComponentResolver {
    /// Display names of the entities a selector matches.
    fn selector(&self, pattern: &str) -> Vec<TextComponent>;
    /// The value of `objective` for the holder `name`, already formatted
    /// with the objective's number format. `None` when it has no score.
    fn score(&self, name: &str, objective: &str) -> Option<TextComponent>;
    /// Tags the path matches in the source.
    fn nbt(&self, source: &NbtSource, path: &str) -> Vec<Tag>;
    /// A tag as `/data` prints it: plain, or coloured the way the game
    /// colours it.
    fn nbt_text(&self, tag: &Tag, plain: bool) -> TextComponent;
}

/// Whether `color` is a named colour or `#RRGGBB`, as the game parses it.
pub fn is_valid_color(color: &str) -> bool {
    match color.strip_prefix('#') {
        Some(hex) => {
            !hex.is_empty() && hex.len() <= 6 && hex.bytes().all(|b| b.is_ascii_hexdigit())
        }
        None => NAMED_COLORS.contains(&color),
    }
}

const NAMED_COLORS: [&str; 16] = [
    "black",
    "dark_blue",
    "dark_green",
    "dark_aqua",
    "dark_red",
    "dark_purple",
    "gold",
    "gray",
    "dark_gray",
    "blue",
    "green",
    "aqua",
    "red",
    "light_purple",
    "yellow",
    "white",
];

/// What a component does when clicked, and the text shown while the
/// cursor is over it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Events {
    pub click: Option<ClickEvent>,
    pub hover: Option<Hover>,
    /// Text shift-clicking the component inserts into the chat box.
    pub insertion: Option<String>,
}

/// What the client shows while the cursor is over a component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hover {
    Text(TextComponent),
    /// An entity's tooltip: its name, type and UUID.
    Entity {
        kind: String,
        uuid: u128,
        name: Option<TextComponent>,
    },
    /// An item's tooltip: its id, count and component patch.
    Item {
        id: String,
        count: i32,
        components: Option<Value>,
    },
}

/// What clicking a component in chat does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClickEvent {
    /// Puts the command into the chat box without running it.
    SuggestCommand(String),
    RunCommand(String),
    OpenUrl(String),
    CopyToClipboard(String),
    /// Opens a dialog, given as a registry id or an inline definition.
    ShowDialog(Value),
    /// Turns a book to the given one-based page.
    ChangePage(i32),
    /// Sends a custom payload back to the server.
    Custom {
        id: String,
        payload: Option<Value>,
    },
}

impl ClickEvent {
    fn action_fields(&self) -> (&'static str, Vec<(&'static str, Value)>) {
        match self {
            Self::SuggestCommand(command) => (
                "suggest_command",
                vec![("command", command.as_str().into())],
            ),
            Self::RunCommand(command) => {
                ("run_command", vec![("command", command.as_str().into())])
            }
            Self::OpenUrl(url) => ("open_url", vec![("url", url.as_str().into())]),
            Self::CopyToClipboard(value) => {
                ("copy_to_clipboard", vec![("value", value.as_str().into())])
            }
            Self::ShowDialog(dialog) => ("show_dialog", vec![("dialog", dialog.clone())]),
            Self::ChangePage(page) => ("change_page", vec![("page", (*page).into())]),
            Self::Custom { id, payload } => {
                let mut fields = vec![("id", Value::from(id.as_str()))];
                if let Some(payload) = payload {
                    fields.push(("payload", payload.clone()));
                }
                ("custom", fields)
            }
        }
    }

    fn from_compound(map: &Compound) -> Option<Self> {
        let text = |key: &str| Some(map.get(key)?.as_str()?.to_owned());
        Some(match map.get("action")?.as_str()? {
            "suggest_command" => Self::SuggestCommand(text("command")?),
            "run_command" => Self::RunCommand(text("command")?),
            "open_url" => Self::OpenUrl(text("url")?),
            "copy_to_clipboard" => Self::CopyToClipboard(text("value")?),
            "show_dialog" => Self::ShowDialog(nbt_to_json(map.get("dialog")?)),
            "change_page" => match map.get("page")? {
                Tag::Int(page) if *page > 0 => Self::ChangePage(*page),
                _ => return None,
            },
            "custom" => Self::Custom {
                id: text("id")?,
                payload: map.get("payload").map(nbt_to_json),
            },
            _ => return None,
        })
    }
}

impl Hover {
    fn from_compound(map: &Compound) -> Option<Self> {
        Some(match map.get("action")?.as_str()? {
            "show_text" => Self::Text(TextComponent::from_nbt(map.get("value")?)?),
            "show_entity" => Self::Entity {
                kind: map.get("id")?.as_str()?.to_owned(),
                uuid: match map.get("uuid")? {
                    Tag::IntArray(ints) if ints.len() == 4 => ints
                        .iter()
                        .fold(0u128, |acc, i| (acc << 32) | u128::from(*i as u32)),
                    Tag::String(text) => tuff_storage::admin::parse_uuid_text(text)?,
                    _ => return None,
                },
                name: match map.get("name") {
                    Some(tag) => Some(TextComponent::from_nbt(tag)?),
                    None => None,
                },
            },
            "show_item" => Self::Item {
                id: map.get("id")?.as_str()?.to_owned(),
                count: match map.get("count") {
                    None => 1,
                    Some(Tag::Int(count)) if *count > 0 => *count,
                    _ => return None,
                },
                components: map.get("components").map(nbt_to_json),
            },
            _ => return None,
        })
    }
}

fn nbt_to_json(tag: &Tag) -> Value {
    match tag {
        Tag::Byte(v) => Value::from(*v),
        Tag::Short(v) => Value::from(*v),
        Tag::Int(v) => Value::from(*v),
        Tag::Long(v) => Value::from(*v),
        Tag::Float(v) => Value::from(f64::from(*v)),
        Tag::Double(v) => Value::from(*v),
        Tag::String(v) => Value::from(v.as_str()),
        Tag::List(items) => items.iter().map(nbt_to_json).collect(),
        Tag::Compound(map) => Value::Object(
            map.iter()
                .map(|(key, value)| (key.to_owned(), nbt_to_json(value)))
                .collect(),
        ),
        _ => Value::Null,
    }
}

fn json_canon(value: &Value) -> Canon {
    match value {
        Value::Null => Canon::Map(Vec::new()),
        Value::Bool(b) => Canon::Bool(*b),
        Value::Number(n) => match n.as_i64().and_then(|i| i32::try_from(i).ok()) {
            Some(i) => Canon::Int(i),
            None => Canon::Double(n.as_f64().unwrap_or_default()),
        },
        Value::String(s) => Canon::str(s.clone()),
        Value::Array(items) => Canon::List(items.iter().map(json_canon).collect()),
        Value::Object(map) => Canon::Map(
            map.iter()
                .map(|(key, value)| (key.clone(), json_canon(value)))
                .collect(),
        ),
    }
}

impl TextComponent {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Default::default()
        }
    }

    /// A component rendered client-side from the translation `key`, with
    /// `args` substituted for its placeholders.
    pub fn translate(
        key: impl Into<String>,
        args: impl IntoIterator<Item = TextComponent>,
    ) -> Self {
        Self {
            translate: Some(key.into().into_boxed_str()),
            with: args.into_iter().collect(),
            ..Default::default()
        }
    }

    /// Sets the color: a named color (e.g. `"gold"`) or `"#RRGGBB"`.
    pub fn color(mut self, color: impl Into<String>) -> Self {
        self.color = Some(color.into().into_boxed_str());
        self
    }

    pub fn bold(mut self, bold: bool) -> Self {
        self.bold = Some(bold);
        self
    }

    pub fn italic(mut self, italic: bool) -> Self {
        self.italic = Some(italic);
        self
    }

    pub fn underlined(mut self, underlined: bool) -> Self {
        self.underlined = Some(underlined);
        self
    }

    pub fn strikethrough(mut self, strikethrough: bool) -> Self {
        self.strikethrough = Some(strikethrough);
        self
    }

    pub fn obfuscated(mut self, obfuscated: bool) -> Self {
        self.obfuscated = Some(obfuscated);
        self
    }

    pub fn fallback(mut self, fallback: impl Into<String>) -> Self {
        self.rare_mut().fallback = Some(fallback.into().into_boxed_str());
        self
    }

    pub fn rare_mut(&mut self) -> &mut Rare {
        self.rare.get_or_insert_with(Default::default)
    }

    pub fn events(&self) -> Option<&Events> {
        self.rare.as_deref().map(|r| &r.events)
    }

    pub fn special_contents(&self) -> Option<&Special> {
        self.rare.as_deref()?.special.as_deref()
    }

    pub fn font(&self) -> Option<&str> {
        self.rare.as_deref()?.font.as_deref()
    }

    pub fn shadow_color(&self) -> Option<i32> {
        self.rare.as_deref()?.shadow_color
    }

    pub fn fallback_text(&self) -> Option<&str> {
        self.rare.as_deref()?.fallback.as_deref()
    }

    /// A component the server replaces with an entity list, score or NBT
    /// text through [`resolve`](Self::resolve).
    pub fn special(special: Special) -> Self {
        Self {
            rare: Some(Box::new(Rare {
                special: Some(Box::new(special)),
                ..Default::default()
            })),
            ..Default::default()
        }
    }

    pub fn click(mut self, event: ClickEvent) -> Self {
        self.rare_mut().events.click = Some(event);
        self
    }

    pub fn hover_text(mut self, text: TextComponent) -> Self {
        self.rare_mut().events.hover = Some(Hover::Text(text));
        self
    }

    /// Shows the entity of type `kind` (a registry id) with `uuid` and
    /// `name` on hover.
    pub fn hover_entity(
        mut self,
        kind: impl Into<String>,
        uuid: u128,
        name: Option<TextComponent>,
    ) -> Self {
        self.rare_mut().events.hover = Some(Hover::Entity {
            kind: kind.into(),
            uuid,
            name,
        });
        self
    }

    pub fn insertion(mut self, text: impl Into<String>) -> Self {
        self.rare_mut().events.insertion = Some(text.into());
        self
    }

    /// Green text in square brackets that copies itself when clicked, as
    /// the game shows seeds and coordinates.
    pub fn copy_on_click(text: impl Into<String>) -> Self {
        let text = text.into();
        let inner = Self::new(text.clone())
            .color("green")
            .click(ClickEvent::CopyToClipboard(text.clone()))
            .hover_text(Self::translate("chat.copy.click", []))
            .insertion(text);
        Self::translate("chat.square_brackets", [inner])
    }

    /// The text of the component and its children with formatting
    /// dropped, matching how a vanilla console renders it: `translate` keys
    /// are looked up against the client's `en_us` language file and `with`
    /// arguments are substituted into the resulting format string
    /// (themselves rendered the same way, recursively). A key missing from
    /// the table is used as the format string verbatim, same as vanilla
    /// falling back to the bare key.
    pub fn plain(&self) -> String {
        self.plain_with(&|_| None)
    }

    /// Like [`plain`](Self::plain), consulting `extra` before the vanilla
    /// table (feature language tables).
    pub fn plain_with<'a>(&self, extra: &dyn Fn(&str) -> Option<&'a str>) -> String {
        let mut out = String::new();
        self.write_plain(&mut out, extra);
        out
    }

    fn write_plain<'a>(&self, out: &mut String, extra: &dyn Fn(&str) -> Option<&'a str>) {
        match &self.translate {
            _ if self.special_contents().is_some() => {
                if let Some(Special::Keybind(key)) = self.special_contents() {
                    out.push_str(key);
                }
            }
            Some(key) => {
                let format = extra(key)
                    .or_else(|| tuff_vanilla_data::lang::lookup(key))
                    .or(self.fallback_text())
                    .unwrap_or(&**key);
                let args: Vec<String> = self.with.iter().map(|arg| arg.plain_with(extra)).collect();
                format_translation(format, &args, out);
            }
            None => out.push_str(&self.text),
        }
        for child in &self.extra {
            child.write_plain(out, extra);
        }
    }

    /// Appends a child component, which inherits this component's style.
    pub fn append(mut self, child: impl Into<TextComponent>) -> Self {
        self.extra.push(child.into());
        self
    }

    pub fn to_json(&self) -> String {
        self.to_value().to_string()
    }

    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        match &self.translate {
            _ if self.special_contents().is_some() => {
                if let Some(special) = self.special_contents() {
                    special.write_value(&mut map);
                }
            }
            Some(key) => {
                map.insert("translate".into(), (&**key).into());
                if let Some(fallback) = self.fallback_text() {
                    map.insert("fallback".into(), fallback.into());
                }
                if !self.with.is_empty() {
                    map.insert(
                        "with".into(),
                        self.with.iter().map(Self::to_value).collect(),
                    );
                }
            }
            None => {
                map.insert("text".into(), self.text.as_str().into());
            }
        }
        if let Some(color) = &self.color {
            map.insert("color".into(), (&**color).into());
        }
        if let Some(bold) = self.bold {
            map.insert("bold".into(), bold.into());
        }
        if let Some(italic) = self.italic {
            map.insert("italic".into(), italic.into());
        }
        if let Some(underlined) = self.underlined {
            map.insert("underlined".into(), underlined.into());
        }
        if let Some(strikethrough) = self.strikethrough {
            map.insert("strikethrough".into(), strikethrough.into());
        }
        if let Some(obfuscated) = self.obfuscated {
            map.insert("obfuscated".into(), obfuscated.into());
        }
        if let Some(font) = self.font() {
            map.insert("font".into(), font.into());
        }
        if let Some(shadow) = self.shadow_color() {
            map.insert("shadow_color".into(), shadow.into());
        }
        let events = self.rare.as_deref().map(|r| &r.events);
        if let Some(click) = events.and_then(|e| e.click.as_ref()) {
            let (action, fields) = click.action_fields();
            let mut event = Map::new();
            event.insert("action".into(), action.into());
            for (key, value) in fields {
                event.insert(key.into(), value);
            }
            map.insert("click_event".into(), Value::Object(event));
        }
        if let Some(hover) = events.and_then(|e| e.hover.as_ref()) {
            let mut event = Map::new();
            match hover {
                Hover::Text(text) => {
                    event.insert("action".into(), "show_text".into());
                    event.insert("value".into(), text.to_value());
                }
                Hover::Entity { kind, uuid, name } => {
                    event.insert("action".into(), "show_entity".into());
                    event.insert("id".into(), kind.as_str().into());
                    event.insert(
                        "uuid".into(),
                        tuff_storage::admin::format_uuid(*uuid).into(),
                    );
                    if let Some(name) = name {
                        event.insert("name".into(), name.to_value());
                    }
                }
                Hover::Item {
                    id,
                    count,
                    components,
                } => {
                    event.insert("action".into(), "show_item".into());
                    event.insert("id".into(), id.as_str().into());
                    if *count != 1 {
                        event.insert("count".into(), (*count).into());
                    }
                    if let Some(components) = components {
                        event.insert("components".into(), components.clone());
                    }
                }
            }
            map.insert("hover_event".into(), Value::Object(event));
        }
        if let Some(insertion) = events.and_then(|e| e.insertion.as_ref()) {
            map.insert("insertion".into(), insertion.as_str().into());
        }
        if !self.extra.is_empty() {
            map.insert(
                "extra".into(),
                self.extra.iter().map(Self::to_value).collect(),
            );
        }
        Value::Object(map)
    }

    /// A plain, unstyled, childless literal, which the game serializes as
    /// a bare string.
    pub fn is_plain(&self) -> bool {
        self.translate.is_none()
            && self.rare.is_none()
            && self.strikethrough.is_none()
            && self.obfuscated.is_none()
            && self.color.is_none()
            && self.bold.is_none()
            && self.italic.is_none()
            && self.underlined.is_none()
            && self.extra.is_empty()
    }

    /// The NBT form used by packets and stored components: a string when
    /// plain, otherwise a compound.
    pub fn to_nbt(&self) -> Tag {
        if self.is_plain() {
            return Tag::String(self.text.clone());
        }
        json_to_nbt(&self.to_value())
    }

    /// Reads a JSON text component, as data pack files store them.
    pub fn from_json_str(json: &str) -> Option<Self> {
        let value: Value = serde_json::from_str(json).ok()?;
        Self::from_nbt(&json_to_nbt(&value))
    }

    /// Reads a stored or received text component. `None` when it uses a
    /// field this type does not model, so the caller can keep the NBT.
    pub fn from_nbt(tag: &Tag) -> Option<Self> {
        match tag {
            Tag::String(text) => Some(Self::new(text.clone())),
            Tag::List(items) => {
                let (first, rest) = items.split_first()?;
                let mut component = Self::from_nbt(first)?;
                for sibling in rest {
                    component.extra.push(Self::from_nbt(sibling)?);
                }
                Some(component)
            }
            Tag::Compound(map) => {
                let mut component = Self::default();
                let mut text = None;
                let list = |tag: &Tag| {
                    tag.as_list()?
                        .iter()
                        .map(Self::from_nbt)
                        .collect::<Option<Vec<_>>>()
                };
                for (key, value) in map.iter() {
                    match key {
                        "text" => text = Some(value.as_str()?.to_owned()),
                        "translate" => component.translate = Some(value.as_str()?.into()),
                        "with" => component.with = list(value)?,
                        "extra" => component.extra = list(value)?,
                        "color" => {
                            let color = value.as_str()?;
                            if !is_valid_color(color) {
                                return None;
                            }
                            component.color = Some(color.into());
                        }
                        "strikethrough" => component.strikethrough = Some(value.as_bool()?),
                        "obfuscated" => component.obfuscated = Some(value.as_bool()?),
                        "font" => component.rare_mut().font = Some(value.as_str()?.into()),
                        "shadow_color" => component.rare_mut().shadow_color = Some(argb(value)?),
                        "fallback" => component.rare_mut().fallback = Some(value.as_str()?.into()),
                        "selector" | "score" | "keybind" | "object" | "atlas" | "sprite"
                        | "player" | "hat" | "nbt" | "interpret" | "plain" | "separator"
                        | "block" | "entity" | "storage" => {}
                        "click_event" => {
                            let click = ClickEvent::from_compound(value.as_compound()?)?;
                            component.rare_mut().events.click = Some(click);
                        }
                        "hover_event" => {
                            let hover = Hover::from_compound(value.as_compound()?)?;
                            component.rare_mut().events.hover = Some(hover);
                        }
                        "insertion" => {
                            component.rare_mut().events.insertion =
                                Some(value.as_str()?.to_owned());
                        }
                        "bold" => component.bold = Some(value.as_bool()?),
                        "italic" => component.italic = Some(value.as_bool()?),
                        "underlined" => component.underlined = Some(value.as_bool()?),
                        _ => return None,
                    }
                }
                let special = Special::from_compound(map)?.map(Box::new);
                if special.is_some() {
                    component.rare_mut().special = special;
                }
                match (text, &component.translate, &component.special_contents()) {
                    (None, None, Some(_)) => {}
                    (_, _, Some(_)) => return None,
                    (Some(text), None, None) if component.with.is_empty() => component.text = text,
                    (None, Some(_), None) => {}
                    _ => return None,
                }
                if component.extra.is_empty() && map.contains_key("extra") {
                    return None;
                }
                Some(component)
            }
            _ => None,
        }
    }

    /// The component in the shape the game's serializer gives it, for
    /// hashing.
    pub fn canon(&self) -> Canon {
        if self.is_plain() {
            return Canon::str(self.text.clone());
        }
        let mut entries = Vec::new();
        match &self.translate {
            Some(key) => {
                entries.push(("translate".to_owned(), Canon::str(key.clone())));
                if !self.with.is_empty() {
                    entries.push((
                        "with".to_owned(),
                        Canon::List(self.with.iter().map(Self::canon).collect()),
                    ));
                }
            }
            None => match self.special_contents() {
                Some(special) => special.write_canon(&mut entries),
                None => entries.push(("text".to_owned(), Canon::str(self.text.clone()))),
            },
        }
        if let Some(fallback) = self.fallback_text() {
            entries.push(("fallback".to_owned(), Canon::str(fallback.to_owned())));
        }
        if let Some(strikethrough) = self.strikethrough {
            entries.push(("strikethrough".to_owned(), Canon::Bool(strikethrough)));
        }
        if let Some(obfuscated) = self.obfuscated {
            entries.push(("obfuscated".to_owned(), Canon::Bool(obfuscated)));
        }
        if let Some(font) = self.font() {
            entries.push(("font".to_owned(), Canon::str(font.to_owned())));
        }
        if let Some(shadow) = self.shadow_color() {
            entries.push(("shadow_color".to_owned(), Canon::Int(shadow)));
        }
        if !self.extra.is_empty() {
            entries.push((
                "extra".to_owned(),
                Canon::List(self.extra.iter().map(Self::canon).collect()),
            ));
        }
        if let Some(color) = &self.color {
            entries.push(("color".to_owned(), Canon::str(color.to_string())));
        }
        if let Some(bold) = self.bold {
            entries.push(("bold".to_owned(), Canon::Bool(bold)));
        }
        if let Some(italic) = self.italic {
            entries.push(("italic".to_owned(), Canon::Bool(italic)));
        }
        if let Some(underlined) = self.underlined {
            entries.push(("underlined".to_owned(), Canon::Bool(underlined)));
        }
        let events = self.rare.as_deref().map(|r| &r.events);
        if let Some(click) = events.and_then(|e| e.click.as_ref()) {
            let (action, fields) = click.action_fields();
            let mut event = vec![("action".to_owned(), Canon::str(action.to_owned()))];
            event.extend(
                fields
                    .iter()
                    .map(|(key, value)| ((*key).to_owned(), json_canon(value))),
            );
            entries.push(("click_event".to_owned(), Canon::Map(event)));
        }
        if let Some(hover) = events.and_then(|e| e.hover.as_ref()) {
            let event = match hover {
                Hover::Text(text) => vec![
                    ("action".to_owned(), Canon::str("show_text".to_owned())),
                    ("value".to_owned(), text.canon()),
                ],
                Hover::Entity { kind, uuid, name } => {
                    let ints = match Tag::uuid(*uuid) {
                        Tag::IntArray(ints) => ints,
                        _ => Vec::new(),
                    };
                    let mut event = vec![
                        ("action".to_owned(), Canon::str("show_entity".to_owned())),
                        ("id".to_owned(), Canon::str(kind.clone())),
                        ("uuid".to_owned(), Canon::Ints(ints)),
                    ];
                    if let Some(name) = name {
                        event.push(("name".to_owned(), name.canon()));
                    }
                    event
                }
                Hover::Item {
                    id,
                    count,
                    components,
                } => {
                    let mut event = vec![
                        ("action".to_owned(), Canon::str("show_item".to_owned())),
                        ("id".to_owned(), Canon::str(id.clone())),
                    ];
                    if *count != 1 {
                        event.push(("count".to_owned(), Canon::Int(*count)));
                    }
                    if let Some(components) = components {
                        event.push(("components".to_owned(), json_canon(components)));
                    }
                    event
                }
            };
            entries.push(("hover_event".to_owned(), Canon::Map(event)));
        }
        if let Some(insertion) = events.and_then(|e| e.insertion.as_ref()) {
            entries.push(("insertion".to_owned(), Canon::str(insertion.clone())));
        }
        Canon::Map(entries)
    }
}

fn argb(tag: &Tag) -> Option<i32> {
    match tag {
        Tag::Int(value) => Some(*value),
        Tag::List(items) if items.len() == 4 => {
            let mut channels = [0u8; 4];
            for (slot, item) in channels.iter_mut().zip(items) {
                let value = match item {
                    Tag::Float(v) => *v,
                    Tag::Double(v) => *v as f32,
                    Tag::Int(v) => *v as f32,
                    _ => return None,
                };
                *slot = (value.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            }
            let [r, g, b, a] = channels;
            Some(i32::from_be_bytes([a, r, g, b]))
        }
        _ => None,
    }
}

impl Special {
    fn write_value(&self, map: &mut Map<String, Value>) {
        match self {
            Self::Selector { pattern, separator } => {
                map.insert("selector".into(), pattern.as_str().into());
                if let Some(separator) = separator {
                    map.insert("separator".into(), separator.to_value());
                }
            }
            Self::Score { name, objective } => {
                let mut score = Map::new();
                score.insert("name".into(), name.as_str().into());
                score.insert("objective".into(), objective.as_str().into());
                map.insert("score".into(), Value::Object(score));
            }
            Self::Nbt {
                path,
                interpret,
                plain,
                separator,
                source,
            } => {
                map.insert("nbt".into(), path.as_str().into());
                if *interpret {
                    map.insert("interpret".into(), true.into());
                }
                if *plain {
                    map.insert("plain".into(), true.into());
                }
                if let Some(separator) = separator {
                    map.insert("separator".into(), separator.to_value());
                }
                let (key, value) = source.parts();
                map.insert(key.into(), value.into());
            }
            Self::Keybind(key) => {
                map.insert("keybind".into(), key.as_str().into());
            }
            Self::Atlas { atlas, sprite } => {
                map.insert("object".into(), "atlas".into());
                if let Some(atlas) = atlas {
                    map.insert("atlas".into(), atlas.as_str().into());
                }
                map.insert("sprite".into(), sprite.as_str().into());
            }
            Self::PlayerHead { profile, hat } => {
                map.insert("object".into(), "player".into());
                map.insert("player".into(), profile.clone());
                if !*hat {
                    map.insert("hat".into(), false.into());
                }
            }
        }
    }

    fn write_canon(&self, entries: &mut Vec<(String, Canon)>) {
        match self {
            Self::Selector { pattern, separator } => {
                entries.push(("selector".to_owned(), Canon::str(pattern.clone())));
                if let Some(separator) = separator {
                    entries.push(("separator".to_owned(), separator.canon()));
                }
            }
            Self::Score { name, objective } => entries.push((
                "score".to_owned(),
                Canon::Map(vec![
                    ("name".to_owned(), Canon::str(name.clone())),
                    ("objective".to_owned(), Canon::str(objective.clone())),
                ]),
            )),
            Self::Nbt {
                path,
                interpret,
                plain,
                separator,
                source,
            } => {
                entries.push(("nbt".to_owned(), Canon::str(path.clone())));
                if *interpret {
                    entries.push(("interpret".to_owned(), Canon::Bool(true)));
                }
                if *plain {
                    entries.push(("plain".to_owned(), Canon::Bool(true)));
                }
                if let Some(separator) = separator {
                    entries.push(("separator".to_owned(), separator.canon()));
                }
                let (key, value) = source.parts();
                entries.push((key.to_owned(), Canon::str(value.to_owned())));
            }
            Self::Keybind(key) => entries.push(("keybind".to_owned(), Canon::str(key.clone()))),
            Self::Atlas { atlas, sprite } => {
                entries.push(("object".to_owned(), Canon::str("atlas")));
                if let Some(atlas) = atlas {
                    entries.push(("atlas".to_owned(), Canon::str(atlas.clone())));
                }
                entries.push(("sprite".to_owned(), Canon::str(sprite.clone())));
            }
            Self::PlayerHead { profile, hat } => {
                entries.push(("object".to_owned(), Canon::str("player")));
                entries.push(("player".to_owned(), json_canon(profile)));
                if !*hat {
                    entries.push(("hat".to_owned(), Canon::Bool(false)));
                }
            }
        }
    }

    /// `Ok(None)` when the compound carries no special contents, `None` when
    /// it carries malformed ones.
    fn from_compound(map: &Compound) -> Option<Option<Self>> {
        let separator = || match map.get("separator") {
            Some(tag) => TextComponent::from_nbt(tag).map(Some),
            None => Some(None),
        };
        let flag = |key: &str| match map.get(key) {
            Some(tag) => tag.as_bool(),
            None => Some(false),
        };
        if let Some(pattern) = map.get("selector") {
            return Some(Some(Self::Selector {
                pattern: pattern.as_str()?.to_owned(),
                separator: separator()?,
            }));
        }
        if let Some(score) = map.get("score") {
            let score = score.as_compound()?;
            return Some(Some(Self::Score {
                name: score.get("name")?.as_str()?.to_owned(),
                objective: score.get("objective")?.as_str()?.to_owned(),
            }));
        }
        if let Some(kind) = map.get("object") {
            return Some(Some(match kind.as_str()? {
                "atlas" => Self::Atlas {
                    atlas: match map.get("atlas") {
                        Some(atlas) => Some(atlas.as_str()?.to_owned()),
                        None => None,
                    },
                    sprite: map.get("sprite")?.as_str()?.to_owned(),
                },
                "player" => Self::PlayerHead {
                    profile: nbt_to_json(map.get("player")?),
                    hat: match map.get("hat") {
                        Some(hat) => hat.as_bool()?,
                        None => true,
                    },
                },
                _ => return None,
            }));
        }
        if let Some(key) = map.get("keybind") {
            return Some(Some(Self::Keybind(key.as_str()?.to_owned())));
        }
        if let Some(path) = map.get("nbt") {
            let source = ["block", "entity", "storage"]
                .into_iter()
                .find_map(|key| Some((key, map.get(key)?)))?;
            let value = source.1.as_str()?.to_owned();
            let source = match source.0 {
                "block" => NbtSource::Block(value),
                "entity" => NbtSource::Entity(value),
                _ => NbtSource::Storage(value),
            };
            let (interpret, plain) = (flag("interpret")?, flag("plain")?);
            if interpret && plain {
                return None;
            }
            return Some(Some(Self::Nbt {
                path: path.as_str()?.to_owned(),
                interpret,
                plain,
                separator: separator()?,
                source,
            }));
        }
        Some(None)
    }
}

impl NbtSource {
    fn parts(&self) -> (&'static str, &str) {
        match self {
            Self::Block(value) => ("block", value),
            Self::Entity(value) => ("entity", value),
            Self::Storage(value) => ("storage", value),
        }
    }
}

impl TextComponent {
    /// Replaces selector, score and nbt contents (here and in children and
    /// arguments) with the text `resolver` supplies, keeping styles.
    pub fn resolve(&self, resolver: &dyn ComponentResolver) -> TextComponent {
        self.resolve_limited(resolver, &mut ResolveBudget::default(), 0)
    }

    /// [`Self::resolve`] under `budget`: below the depth limit a component is
    /// copied unresolved, and past the nbt limit nbt contents resolve empty.
    pub fn resolve_limited(
        &self,
        resolver: &dyn ComponentResolver,
        budget: &mut ResolveBudget,
        depth: usize,
    ) -> TextComponent {
        if depth > budget.depth_limit {
            return self.clone();
        }
        let mut out = self.clone();
        out.with = self
            .with
            .iter()
            .map(|c| c.resolve_limited(resolver, budget, depth + 1))
            .collect();
        out.extra = self
            .extra
            .iter()
            .map(|c| c.resolve_limited(resolver, budget, depth + 1))
            .collect();
        let Some(special) = self.special_contents() else {
            return out;
        };
        let default_separator = || TextComponent::new(", ").color("gray");
        let join = |parts: Vec<TextComponent>,
                    separator: &Option<TextComponent>,
                    budget: &mut ResolveBudget| {
            let separator = separator
                .as_ref()
                .map(|s| s.resolve_limited(resolver, budget, depth + 1))
                .unwrap_or_else(default_separator);
            let mut joined = TextComponent::new("");
            for (i, part) in parts.into_iter().enumerate() {
                if i > 0 {
                    joined.extra.push(separator.clone());
                }
                joined.extra.push(part);
            }
            joined
        };
        let contents = match special {
            Special::Selector { pattern, separator } => {
                join(resolver.selector(pattern), separator, budget)
            }
            Special::Score { name, objective } => resolver
                .score(name, objective)
                .unwrap_or_else(|| TextComponent::new("")),
            Special::Nbt {
                path,
                interpret,
                plain,
                separator,
                source,
            } => {
                if budget.nbt_count > budget.nbt_limit {
                    return out.without_special();
                }
                budget.nbt_count += 1;
                let parts = resolver
                    .nbt(source, path)
                    .iter()
                    .map(|tag| {
                        if *interpret {
                            let parsed = tag
                                .as_str()
                                .and_then(Self::from_json_str)
                                .or_else(|| Self::from_nbt(tag));
                            parsed.map_or_else(
                                || TextComponent::new(""),
                                |c| c.resolve_limited(resolver, budget, depth + 1),
                            )
                        } else {
                            resolver.nbt_text(tag, *plain)
                        }
                    })
                    .collect();
                join(parts, separator, budget)
            }
            Special::Keybind(_) | Special::Atlas { .. } | Special::PlayerHead { .. } => return out,
        };
        let mut out = out.without_special();
        out.extra.insert(0, contents);
        out
    }

    fn without_special(mut self) -> TextComponent {
        if let Some(rare) = self.rare.as_deref_mut() {
            rare.special = None;
        }
        self.text = String::new();
        self
    }
}

impl Serialize for TextComponent {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_value().serialize(serializer)
    }
}

/// Substitutes `args` into a Java `Formatter`-style `format` string, the way
/// the game's translation system does: `%s` consumes the next argument in
/// order, `%1$s`/`%2$s`/... take a specific 1-indexed argument without
/// advancing the sequential counter, and `%%` is a literal percent sign. An
/// unrecognized `%`-sequence is copied through verbatim (this is what makes
/// falling back to an untranslated key, which typically has no placeholders
/// at all, safe: its literal `%` signs, if any, survive unchanged). A
/// sequential or positional reference past the end of `args` is dropped
/// rather than panicking.
fn format_translation(format: &str, args: &[String], out: &mut String) {
    let bytes = format.as_bytes();
    let mut i = 0;
    let mut seq = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'%' {
            let start = i;
            while i < bytes.len() && bytes[i] != b'%' {
                i += 1;
            }
            out.push_str(&format[start..i]);
            continue;
        }
        let percent = i;
        i += 1;
        match bytes.get(i) {
            Some(b'%') => {
                out.push('%');
                i += 1;
            }
            Some(b's') => {
                if let Some(arg) = args.get(seq) {
                    out.push_str(arg);
                }
                seq += 1;
                i += 1;
            }
            Some(b) if b.is_ascii_digit() => {
                let digits_start = i;
                while bytes.get(i).is_some_and(u8::is_ascii_digit) {
                    i += 1;
                }
                if bytes.get(i) == Some(&b'$') && bytes.get(i + 1) == Some(&b's') {
                    if let Ok(index) = format[digits_start..i].parse::<usize>()
                        && let Some(n) = index.checked_sub(1)
                        && let Some(arg) = args.get(n)
                    {
                        out.push_str(arg);
                    }
                    i += 2;
                } else {
                    out.push_str(&format[percent..i]);
                }
            }
            _ => out.push('%'),
        }
    }
}

fn json_to_nbt(value: &Value) -> Tag {
    match value {
        Value::Null => Tag::Compound(Compound::new()),
        Value::Bool(b) => Tag::from(*b),
        Value::Number(n) => match n.as_i64().and_then(|i| i32::try_from(i).ok()) {
            Some(i) => Tag::Int(i),
            None => Tag::Double(n.as_f64().unwrap_or_default()),
        },
        Value::String(s) => Tag::String(s.clone()),
        Value::Array(items) => Tag::List(items.iter().map(json_to_nbt).collect()),
        Value::Object(map) => Tag::Compound(
            map.iter()
                .map(|(key, value)| (key.clone(), json_to_nbt(value)))
                .collect(),
        ),
    }
}

impl From<&str> for TextComponent {
    fn from(text: &str) -> Self {
        Self::new(text)
    }
}

impl From<String> for TextComponent {
    fn from(text: String) -> Self {
        Self::new(text)
    }
}

#[cfg(test)]
#[path = "text_tests.rs"]
mod tests;
