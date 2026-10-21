//! MiniMessage markup and legacy `&`/section-sign colour codes to text
//! components (https://docs.advntr.dev/minimessage/format.html).
//!
//! Lenient like the reference parser: unknown or malformed tags stay as
//! literal text, closing tags that match nothing are dropped and a closing
//! tag also closes every tag opened after its match.

use serde_json::{Value, json};

use tuff_item::text::{ClickEvent, Hover, NbtSource, Special, TextComponent};

/// Most tags open at once; deeper opening tags are dropped.
const MAX_DEPTH: usize = 64;
/// Most levels of markup nested inside tag arguments (hover text, lang
/// arguments, separators).
const MAX_ARG_NESTING: usize = 8;
/// Longest input parsed; the rest is ignored.
pub const MAX_INPUT: usize = 256 * 1024;

/// Parses MiniMessage markup.
pub fn parse(input: &str) -> TextComponent {
    parse_with(input, &|_| None)
}

/// Parses MiniMessage markup; `placeholder` supplies a component for a tag
/// name the language does not define (e.g. `<player>`), inserted unparsed.
pub fn parse_with(
    input: &str,
    placeholder: &dyn Fn(&str) -> Option<TextComponent>,
) -> TextComponent {
    Parser {
        placeholder,
        nesting: 0,
    }
    .parse(truncate(input))
}

/// Parses text that may mix MiniMessage tags with legacy `&` or
/// section-sign codes.
pub fn parse_mixed(input: &str) -> TextComponent {
    parse(&legacy_to_minimessage(truncate(input)))
}

/// Escapes `input` so MiniMessage shows it verbatim.
pub fn escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        if matches!(c, '<' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The text `input` shows, with every recognised tag removed.
pub fn strip_tags(input: &str) -> String {
    parse(input).plain()
}

fn truncate(input: &str) -> &str {
    if input.len() <= MAX_INPUT {
        return input;
    }
    let mut end = MAX_INPUT;
    while !input.is_char_boundary(end) {
        end -= 1;
    }
    &input[..end]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    fn hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.0, self.1, self.2)
    }

    fn parse_hex(hex: &str) -> Option<Self> {
        let hex = hex.strip_prefix('#')?;
        if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let v = u32::from_str_radix(hex, 16).ok()?;
        Some(Self((v >> 16) as u8, (v >> 8) as u8, v as u8))
    }

    fn lerp(t: f32, a: Self, b: Self) -> Self {
        let t = t.clamp(0.0, 1.0);
        let ch = |x: u8, y: u8| {
            let x = f32::from(x);
            (x + t * (f32::from(y) - x) + 0.5).floor() as u8
        };
        Self(ch(a.0, b.0), ch(a.1, b.1), ch(a.2, b.2))
    }

    fn hsv(h: f32, s: f32, v: f32) -> Self {
        let to = |x: f32| (x * 255.0) as u8;
        if s == 0.0 {
            return Self(to(v), to(v), to(v));
        }
        let h = h * 6.0;
        let i = h.floor() as i32;
        let f = h - i as f32;
        let p = v * (1.0 - s);
        let q = v * (1.0 - s * f);
        let t = v * (1.0 - s * (1.0 - f));
        let (r, g, b) = match i {
            0 => (v, t, p),
            1 => (q, v, p),
            2 => (p, v, t),
            3 => (p, q, v),
            4 => (t, p, v),
            _ => (v, p, q),
        };
        Self(to(r), to(g), to(b))
    }
}

const NAMED: [(&str, Rgb); 16] = [
    ("black", Rgb(0x00, 0x00, 0x00)),
    ("dark_blue", Rgb(0x00, 0x00, 0xAA)),
    ("dark_green", Rgb(0x00, 0xAA, 0x00)),
    ("dark_aqua", Rgb(0x00, 0xAA, 0xAA)),
    ("dark_red", Rgb(0xAA, 0x00, 0x00)),
    ("dark_purple", Rgb(0xAA, 0x00, 0xAA)),
    ("gold", Rgb(0xFF, 0xAA, 0x00)),
    ("gray", Rgb(0xAA, 0xAA, 0xAA)),
    ("dark_gray", Rgb(0x55, 0x55, 0x55)),
    ("blue", Rgb(0x55, 0x55, 0xFF)),
    ("green", Rgb(0x55, 0xFF, 0x55)),
    ("aqua", Rgb(0x55, 0xFF, 0xFF)),
    ("red", Rgb(0xFF, 0x55, 0x55)),
    ("light_purple", Rgb(0xFF, 0x55, 0xFF)),
    ("yellow", Rgb(0xFF, 0xFF, 0x55)),
    ("white", Rgb(0xFF, 0xFF, 0xFF)),
];

/// The component colour string for a MiniMessage colour (a named colour,
/// a `grey` spelling or `#RRGGBB`).
pub fn color(arg: &str) -> Option<String> {
    color_arg(arg).map(|(color, _)| color)
}

/// A colour argument: a named colour (with the `grey` spellings) or
/// `#RRGGBB`, as the component colour string and its RGB value.
fn color_arg(arg: &str) -> Option<(String, Rgb)> {
    let lower = arg.to_ascii_lowercase();
    if lower.starts_with('#') {
        let rgb = Rgb::parse_hex(&lower)?;
        return Some((rgb.hex(), rgb));
    }
    let name = match lower.as_str() {
        "grey" => "gray",
        "dark_grey" => "dark_gray",
        other => other,
    };
    NAMED
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(n, rgb)| ((*n).to_owned(), *rgb))
}

const PRIDE: &[(&str, &[u32])] = &[
    (
        "pride",
        &[0xE50000, 0xFF8D00, 0xFFEE00, 0x028121, 0x004CFF, 0x770088],
    ),
    (
        "progress",
        &[
            0xFFFFFF, 0xFFAFC7, 0x73D7EE, 0x613915, 0x000000, 0xE50000, 0xFF8D00, 0xFFEE00,
            0x028121, 0x004CFF, 0x770088,
        ],
    ),
    ("trans", &[0x5BCFFB, 0xF5ABB9, 0xFFFFFF, 0xF5ABB9, 0x5BCFFB]),
    ("bi", &[0xD60270, 0x9B4F96, 0x0038A8]),
    ("pan", &[0xFF1C8D, 0xFFD700, 0x1AB3FF]),
    ("nb", &[0xFCF431, 0xFCFCFC, 0x9D59D2, 0x282828]),
    (
        "lesbian",
        &[0xD62800, 0xFF9B56, 0xFFFFFF, 0xD462A6, 0xA40062],
    ),
    ("ace", &[0x000000, 0xA4A4A4, 0xFFFFFF, 0x810081]),
    (
        "agender",
        &[
            0x000000, 0xBABABA, 0xFFFFFF, 0xBAF484, 0xFFFFFF, 0xBABABA, 0x000000,
        ],
    ),
    ("demisexual", &[0x000000, 0xFFFFFF, 0x6E0071, 0xD3D3D3]),
    ("genderqueer", &[0xB57FDD, 0xFFFFFF, 0x49821E]),
    (
        "genderfluid",
        &[0xFE76A2, 0xFFFFFF, 0xBF12D7, 0x000000, 0x303CBE],
    ),
    ("intersex", &[0xFFD800, 0x7902AA, 0xFFD800]),
    ("aro", &[0x3BA740, 0xA8D47A, 0xFFFFFF, 0xABABAB, 0x000000]),
    (
        "baker",
        &[
            0xCD66FF, 0xFF6599, 0xFE0000, 0xFE9900, 0xFFFF01, 0x009900, 0x0099CB, 0x350099,
            0x990099,
        ],
    ),
    (
        "philly",
        &[
            0x000000, 0x784F17, 0xFE0000, 0xFD8C00, 0xFFE500, 0x119F0B, 0x0644B3, 0xC22EDC,
        ],
    ),
    (
        "queer",
        &[
            0x000000, 0x9AD9EB, 0x00A3E8, 0xB5E51D, 0xFFFFFF, 0xFFC90D, 0xFC6FA4, 0xFF0000,
        ],
    ),
    (
        "gay",
        &[
            0x078E70, 0x26CEAA, 0x98E8C1, 0xFFFFFF, 0x7BADE2, 0x5049CB, 0x3D1A78,
        ],
    ),
    (
        "bigender",
        &[
            0xC479A0, 0xECA6CB, 0xD5C7E8, 0xFFFFFF, 0xD5C7E8, 0x9AC7E8, 0x6C83CF,
        ],
    ),
    (
        "demiboy",
        &[
            0x7F7F7F, 0xC3C3C3, 0x9AD9EA, 0xFFFFFF, 0x9AD9EA, 0xC3C3C3, 0x7F7F7F,
        ],
    ),
    (
        "demigirl",
        &[
            0x7F7F7F, 0xC3C3C3, 0xFDADC8, 0xFFFFFF, 0xFDADC8, 0xC3C3C3, 0x7F7F7F,
        ],
    ),
];

fn rgb_of(v: u32) -> Rgb {
    Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decoration {
    Bold,
    Italic,
    Underlined,
    Strikethrough,
    Obfuscated,
}

fn decoration(name: &str) -> Option<Decoration> {
    Some(match name {
        "bold" | "b" => Decoration::Bold,
        "italic" | "em" | "i" => Decoration::Italic,
        "underlined" | "u" => Decoration::Underlined,
        "strikethrough" | "st" => Decoration::Strikethrough,
        "obfuscated" | "obf" => Decoration::Obfuscated,
        _ => return None,
    })
}

/// How a colour-changing tag picks each character's colour.
#[derive(Debug, Clone)]
enum Changer {
    Gradient { colors: Vec<Rgb>, phase: f64 },
    Rainbow { reversed: bool, phase: i32 },
}

/// A tag that wraps the content after it until closed.
#[derive(Debug, Clone)]
enum Wrap {
    Color(String),
    Decoration(Decoration, bool),
    Click(ClickEvent),
    Hover(Hover),
    Insert(String),
    Font(String),
    Shadow(i32),
    Changer(Changer),
}

#[derive(Debug)]
enum Node {
    Text(String),
    Insert(TextComponent),
    Wrap(Wrap, Vec<Node>),
}

struct Open {
    name: String,
    wrap: Wrap,
    children: Vec<Node>,
}

struct Parser<'a> {
    placeholder: &'a dyn Fn(&str) -> Option<TextComponent>,
    nesting: usize,
}

/// One `<...>` tag as written: its lower-cased name and raw arguments.
struct RawTag<'s> {
    close: bool,
    self_close: bool,
    name: String,
    args: Vec<String>,
    source: &'s str,
}

enum Scan {
    /// The tag ends just before this byte index.
    Tag(usize),
    /// A new `<` at this index starts the next candidate tag.
    Restart(usize),
    Unclosed,
}

/// Scans a tag starting at `start` (a `<`). A quote opens a string only
/// when the same quote character appears again later in the input.
fn scan_tag(input: &str, start: usize) -> Scan {
    let bytes = input.as_bytes();
    let mut i = start + 1;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(q) => {
                if b == b'\\' && i + 1 < bytes.len() && (bytes[i + 1] == b'\\' || bytes[i + 1] == q)
                {
                    i += 1;
                } else if b == q {
                    quote = None;
                }
            }
            None => match b {
                b'\'' | b'"' if bytes[i + 1..].contains(&b) => quote = Some(b),
                b'>' if i == start + 1 => return Scan::Unclosed,
                b'>' => return Scan::Tag(i + 1),
                b'\\' => return Scan::Unclosed,
                b'<' => return Scan::Restart(i),
                _ => {}
            },
        }
        i += 1;
    }
    Scan::Unclosed
}

fn split_args(inner: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut quoted_part = false;
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                if c == '\\' && matches!(chars.peek(), Some(&n) if n == q || n == '\\') {
                    current.push(chars.next().unwrap_or(c));
                } else if c == q {
                    quote = None;
                } else {
                    current.push(c);
                }
            }
            None => match c {
                ':' if !matches!(chars.clone().take(2).collect::<String>().as_str(), "//") => {
                    parts.push(std::mem::take(&mut current));
                    quoted_part = false;
                }
                '\'' | '"' if current.is_empty() && !quoted_part => {
                    quote = Some(c);
                    quoted_part = true;
                }
                _ => current.push(c),
            },
        }
    }
    parts.push(current);
    parts
}

fn valid_name(name: &str) -> bool {
    let body = name.strip_prefix(['!', '?', '#']).unwrap_or(name);
    !name.is_empty()
        && body
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

fn raw_tag(source: &str) -> Option<RawTag<'_>> {
    let mut inner = &source[1..source.len() - 1];
    let close = inner.starts_with('/');
    if close {
        inner = &inner[1..];
    }
    let self_close = inner.ends_with('/');
    if self_close {
        inner = &inner[..inner.len() - 1];
    }
    if inner.starts_with(['\'', '"']) {
        return None;
    }
    let mut args = split_args(inner);
    let name = args.remove(0).to_ascii_lowercase();
    if !valid_name(&name) {
        return None;
    }
    Some(RawTag {
        close,
        self_close,
        name,
        args,
        source,
    })
}

fn parse_bool(arg: &str) -> Option<bool> {
    match arg {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn parse_uuid(text: &str) -> Option<u128> {
    let hex: String = text.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 {
        return None;
    }
    u128::from_str_radix(&hex, 16).ok()
}

/// What a recognised opening tag does.
enum Action {
    Wrap(Wrap),
    Insert(TextComponent),
    Reset,
    Pre,
}

impl Parser<'_> {
    fn parse(&mut self, input: &str) -> TextComponent {
        let nodes = self.nodes(input);
        render_root(nodes)
    }

    fn nested(&mut self, input: &str) -> TextComponent {
        if self.nesting >= MAX_ARG_NESTING {
            return TextComponent::new(input);
        }
        self.nesting += 1;
        let out = self.parse(input);
        self.nesting -= 1;
        out
    }

    fn nodes(&mut self, input: &str) -> Vec<Node> {
        let mut root: Vec<Node> = Vec::new();
        let mut stack: Vec<Open> = Vec::new();
        let mut text = String::new();
        let mut i = 0;
        let bytes = input.as_bytes();

        fn push_node(root: &mut Vec<Node>, stack: &mut [Open], node: Node) {
            match stack.last_mut() {
                Some(open) => open.children.push(node),
                None => root.push(node),
            }
        }
        fn flush(root: &mut Vec<Node>, stack: &mut [Open], text: &mut String) {
            if !text.is_empty() {
                push_node(root, stack, Node::Text(std::mem::take(text)));
            }
        }
        fn close_to(root: &mut Vec<Node>, stack: &mut Vec<Open>, depth: usize) {
            while stack.len() > depth {
                let open = stack.pop().expect("non-empty");
                push_node(root, stack, Node::Wrap(open.wrap, open.children));
            }
        }

        while i < bytes.len() {
            let b = bytes[i];
            if b == b'\\' && matches!(bytes.get(i + 1), Some(b'<' | b'\\')) {
                text.push(bytes[i + 1] as char);
                i += 2;
                continue;
            }
            if b != b'<' {
                let c = input[i..].chars().next().expect("in bounds");
                text.push(c);
                i += c.len_utf8();
                continue;
            }
            let end = match scan_tag(input, i) {
                Scan::Tag(end) => end,
                Scan::Restart(next) => {
                    text.push_str(&input[i..next]);
                    i = next;
                    continue;
                }
                Scan::Unclosed => {
                    text.push('<');
                    i += 1;
                    continue;
                }
            };
            let source = &input[i..end];
            let Some(tag) = raw_tag(source) else {
                text.push_str(source);
                i = end;
                continue;
            };
            if tag.close {
                let base = tag.name.trim_start_matches('!');
                match stack.iter().rposition(|open| open.name == base) {
                    Some(depth) => {
                        flush(&mut root, &mut stack, &mut text);
                        close_to(&mut root, &mut stack, depth);
                    }
                    None if base == "reset" => {}
                    None => text.push_str(tag.source),
                }
                i = end;
                continue;
            }
            match self.action(&tag) {
                None => text.push_str(tag.source),
                Some(Action::Reset) => {
                    flush(&mut root, &mut stack, &mut text);
                    close_to(&mut root, &mut stack, 0);
                }
                Some(Action::Insert(component)) => {
                    flush(&mut root, &mut stack, &mut text);
                    push_node(&mut root, &mut stack, Node::Insert(component));
                }
                Some(Action::Pre) => {
                    let rest = &input[end..];
                    let stop = find_ci(rest, "</pre>");
                    text.push_str(&rest[..stop.unwrap_or(rest.len())]);
                    i = end + stop.map_or(rest.len(), |s| s + "</pre>".len());
                    continue;
                }
                Some(Action::Wrap(wrap)) => {
                    if tag.self_close || stack.len() >= MAX_DEPTH {
                        i = end;
                        continue;
                    }
                    flush(&mut root, &mut stack, &mut text);
                    stack.push(Open {
                        name: tag.name.trim_start_matches('!').to_owned(),
                        wrap,
                        children: Vec::new(),
                    });
                }
            }
            i = end;
        }
        flush(&mut root, &mut stack, &mut text);
        close_to(&mut root, &mut stack, 0);
        root
    }

    fn action(&mut self, tag: &RawTag) -> Option<Action> {
        let args = &tag.args;
        let name = tag.name.as_str();
        if let Some(base) = name.strip_prefix('!') {
            return match (decoration(base), base) {
                (Some(d), _) if args.is_empty() => Some(Action::Wrap(Wrap::Decoration(d, false))),
                (None, "shadow") if args.is_empty() => Some(Action::Wrap(Wrap::Shadow(0))),
                _ => None,
            };
        }
        if let Some(d) = decoration(name) {
            let on = match args.as_slice() {
                [] => true,
                [v] => parse_bool(v)?,
                _ => return None,
            };
            return Some(Action::Wrap(Wrap::Decoration(d, on)));
        }
        if args.is_empty()
            && let Some((color, _)) = color_arg(name)
        {
            return Some(Action::Wrap(Wrap::Color(color)));
        }
        Some(match name {
            "color" | "colour" | "c" => match args.as_slice() {
                [c] => Action::Wrap(Wrap::Color(color_arg(c)?.0)),
                _ => return None,
            },
            "reset" | "r" if args.is_empty() => Action::Reset,
            "pre" if args.is_empty() => Action::Pre,
            "newline" | "br" if args.is_empty() => Action::Insert(TextComponent::new("\n")),
            "click" => Action::Wrap(Wrap::Click(click(args)?)),
            "hover" => Action::Wrap(Wrap::Hover(self.hover(args)?)),
            "insert" => match args.as_slice() {
                [text] => Action::Wrap(Wrap::Insert(text.clone())),
                _ => return None,
            },
            "font" => match args.as_slice() {
                [font] => Action::Wrap(Wrap::Font(font.clone())),
                [ns, path] => Action::Wrap(Wrap::Font(format!("{ns}:{path}"))),
                _ => return None,
            },
            "shadow" => Action::Wrap(Wrap::Shadow(shadow(args)?)),
            "key" => match args.as_slice() {
                [key] => Action::Insert(TextComponent::special(Special::Keybind(key.clone()))),
                _ => return None,
            },
            "lang" | "tr" | "translate" => {
                let (key, rest) = args.split_first()?;
                let with = rest.iter().map(|a| self.nested(a)).collect::<Vec<_>>();
                Action::Insert(TextComponent::translate(key.clone(), with))
            }
            "lang_or" | "tr_or" | "translate_or" => {
                let [key, fallback, rest @ ..] = args.as_slice() else {
                    return None;
                };
                let with = rest.iter().map(|a| self.nested(a)).collect::<Vec<_>>();
                Action::Insert(
                    TextComponent::translate(key.clone(), with).fallback(fallback.clone()),
                )
            }
            "selector" | "sel" => {
                let (pattern, rest) = args.split_first()?;
                let separator = match rest {
                    [] => None,
                    [sep] => Some(self.nested(sep)),
                    _ => return None,
                };
                Action::Insert(TextComponent::special(Special::Selector {
                    pattern: pattern.clone(),
                    separator,
                }))
            }
            "score" => match args.as_slice() {
                [name, objective] => Action::Insert(TextComponent::special(Special::Score {
                    name: name.clone(),
                    objective: objective.clone(),
                })),
                _ => return None,
            },
            "nbt" | "data" => self.nbt(args)?,
            "sprite" => match args.as_slice() {
                [sprite] => Action::Insert(TextComponent::special(Special::Atlas {
                    atlas: None,
                    sprite: sprite.clone(),
                })),
                [atlas, sprite] => Action::Insert(TextComponent::special(Special::Atlas {
                    atlas: Some(atlas.clone()),
                    sprite: sprite.clone(),
                })),
                _ => return None,
            },
            "head" => {
                let Some((who, rest)) = args.split_first() else {
                    return Some(Action::Insert(TextComponent::special(
                        Special::PlayerHead {
                            profile: json!({}),
                            hat: true,
                        },
                    )));
                };
                let hat = match rest {
                    [] => match parse_bool(who) {
                        Some(hat) => {
                            return Some(Action::Insert(TextComponent::special(
                                Special::PlayerHead {
                                    profile: json!({}),
                                    hat,
                                },
                            )));
                        }
                        None => true,
                    },
                    [v] => parse_bool(v)?,
                    _ => return None,
                };
                let profile = match parse_uuid(who) {
                    Some(id) => json!({ "id": [
                        (id >> 96) as u32 as i32,
                        (id >> 64) as u32 as i32,
                        (id >> 32) as u32 as i32,
                        id as u32 as i32,
                    ] }),
                    None if who.contains('/') && valid_key(who) => {
                        let texture = if who.contains(':') {
                            who.clone()
                        } else {
                            format!("minecraft:{who}")
                        };
                        json!({ "texture": texture })
                    }
                    None => {
                        let name = who.trim();
                        if name.is_empty()
                            || name.chars().count() > 16
                            || name.chars().any(|c| c.is_whitespace() || c.is_control())
                        {
                            return None;
                        }
                        json!({ "name": name })
                    }
                };
                Action::Insert(TextComponent::special(Special::PlayerHead { profile, hat }))
            }
            "gradient" => Action::Wrap(Wrap::Changer(gradient(args)?)),
            "transition" => {
                let Changer::Gradient { colors, phase } = gradient(args)? else {
                    return None;
                };
                Action::Wrap(Wrap::Color(transition(&colors, phase).hex()))
            }
            "pride" => {
                let (flag, phase) = match args.as_slice() {
                    [] => ("pride", 0.0),
                    [a] => match a.parse::<f64>() {
                        Ok(p) => ("pride", p),
                        Err(_) => (a.as_str(), 0.0),
                    },
                    [a, p] => (a.as_str(), p.parse().ok()?),
                    _ => return None,
                };
                if !(-1.0..=1.0).contains(&phase) {
                    return None;
                }
                let (_, colors) = PRIDE.iter().find(|(n, _)| *n == flag)?;
                Action::Wrap(Wrap::Changer(phased(
                    colors.iter().copied().map(rgb_of).collect(),
                    phase,
                )))
            }
            "rainbow" => {
                let (reversed, phase) = match args.as_slice() {
                    [] => (false, 0),
                    [a] => {
                        let (reversed, rest) = match a.strip_prefix('!') {
                            Some(rest) => (true, rest),
                            None => (false, a.as_str()),
                        };
                        (
                            reversed,
                            if rest.is_empty() {
                                0
                            } else {
                                rest.parse().ok()?
                            },
                        )
                    }
                    _ => return None,
                };
                Action::Wrap(Wrap::Changer(Changer::Rainbow { reversed, phase }))
            }
            other => {
                if tag.args.is_empty()
                    && let Some(component) = (self.placeholder)(other)
                {
                    return Some(Action::Insert(component));
                }
                return None;
            }
        })
    }

    fn hover(&mut self, args: &[String]) -> Option<Hover> {
        let (action, rest) = args.split_first()?;
        Some(match (action.as_str(), rest) {
            ("show_text", [text]) => Hover::Text(self.nested(text)),
            ("show_item", [id, rest @ ..]) if rest.len() <= 2 => Hover::Item {
                id: id.clone(),
                count: match rest.first() {
                    Some(c) => c.parse().ok().filter(|c: &i32| *c > 0)?,
                    None => 1,
                },
                components: match rest.get(1) {
                    Some(raw) => Some(serde_json::from_str::<Value>(raw).ok()?),
                    None => None,
                },
            },
            ("show_entity", [kind, uuid, rest @ ..]) if rest.len() <= 1 => Hover::Entity {
                kind: kind.clone(),
                uuid: parse_uuid(uuid)?,
                name: rest.first().map(|n| self.nested(n)),
            },
            _ => return None,
        })
    }

    fn nbt(&mut self, args: &[String]) -> Option<Action> {
        let [kind, id, path, rest @ ..] = args else {
            return None;
        };
        let source = match kind.as_str() {
            "block" if valid_block_pos(id) => NbtSource::Block(id.clone()),
            "entity" => NbtSource::Entity(id.clone()),
            "storage" if valid_key(id) => NbtSource::Storage(id.clone()),
            _ => return None,
        };
        let mut separator = None;
        let mut interpret = false;
        for (index, arg) in rest.iter().enumerate() {
            match (index, arg.as_str()) {
                (_, "interpret") => interpret = true,
                (0, sep) => separator = Some(self.nested(sep)),
                _ => return None,
            }
        }
        Some(Action::Insert(TextComponent::special(Special::Nbt {
            path: path.clone(),
            interpret,
            plain: false,
            separator,
            source,
        })))
    }
}

/// A resource location: an optional `namespace:` and a path.
fn valid_key(key: &str) -> bool {
    let (namespace, path) = key.split_once(':').unwrap_or(("minecraft", key));
    let ok = |s: &str, extra: &[u8]| {
        s.bytes().all(|b| {
            b.is_ascii_lowercase()
                || b.is_ascii_digit()
                || matches!(b, b'_' | b'-' | b'.')
                || extra.contains(&b)
        })
    };
    !path.is_empty() && ok(namespace, &[]) && ok(path, b"/")
}

/// Three absolute, `~` relative or `^` local coordinates.
fn valid_block_pos(pos: &str) -> bool {
    let parts: Vec<&str> = pos.split(' ').collect();
    parts.len() == 3
        && parts.iter().all(|part| {
            let number = part
                .strip_prefix(['~', '^'])
                .map_or(Some(*part), |rest| (!rest.is_empty()).then_some(rest));
            number.is_none_or(|n| n.parse::<f64>().is_ok_and(f64::is_finite))
        })
}

fn find_ci(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .as_bytes()
        .windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
}

fn click(args: &[String]) -> Option<ClickEvent> {
    let [action, value] = args else {
        return None;
    };
    Some(match action.as_str() {
        "open_url" => ClickEvent::OpenUrl(value.clone()),
        "run_command" => ClickEvent::RunCommand(value.clone()),
        "suggest_command" => ClickEvent::SuggestCommand(value.clone()),
        "copy_to_clipboard" => ClickEvent::CopyToClipboard(value.clone()),
        "change_page" => ClickEvent::ChangePage(value.parse().ok().filter(|p: &i32| *p > 0)?),
        "show_dialog" => ClickEvent::ShowDialog(Value::from(value.as_str())),
        "custom" => ClickEvent::Custom {
            id: value.clone(),
            payload: None,
        },
        _ => return None,
    })
}

fn shadow(args: &[String]) -> Option<i32> {
    let (color, rest) = args.split_first()?;
    let alpha = match rest {
        [] => 0.25f32,
        [a] => a.parse().ok().filter(|a: &f32| (0.0..=1.0).contains(a))?,
        _ => return None,
    };
    let rgb = match color.strip_prefix('#') {
        Some(hex) if hex.len() == 8 => {
            let v = u32::from_str_radix(hex, 16).ok()?;
            return Some(v.rotate_right(8) as i32);
        }
        _ => color_arg(color)?.1,
    };
    let a = (alpha * 255.0) as u32;
    Some(((a << 24) | (u32::from(rgb.0) << 16) | (u32::from(rgb.1) << 8) | u32::from(rgb.2)) as i32)
}

fn gradient(args: &[String]) -> Option<Changer> {
    let mut colors = Vec::new();
    let mut phase = 0.0;
    for (index, arg) in args.iter().enumerate() {
        match color_arg(arg) {
            Some((_, rgb)) => colors.push(rgb),
            None if index + 1 == args.len() && index > 0 => {
                phase = arg
                    .parse::<f64>()
                    .ok()
                    .filter(|p| (-1.0..=1.0).contains(p))?;
            }
            None => return None,
        }
    }
    match colors.len() {
        0 => colors = vec![Rgb(0xFF, 0xFF, 0xFF), Rgb(0, 0, 0)],
        1 => return None,
        _ => {}
    }
    Some(phased(colors, phase))
}

fn phased(mut colors: Vec<Rgb>, mut phase: f64) -> Changer {
    if phase < 0.0 {
        colors.reverse();
        phase += 1.0;
    }
    Changer::Gradient { colors, phase }
}

fn transition(colors: &[Rgb], phase: f64) -> Rgb {
    let segments = (colors.len() - 1) as f64;
    let position = phase * segments;
    let low = (position.floor() as usize).min(colors.len() - 1);
    let high = (low + 1).min(colors.len() - 1);
    Rgb::lerp((position - low as f64) as f32, colors[low], colors[high])
}

impl Changer {
    fn colors(&self, size: usize) -> Vec<Rgb> {
        match self {
            Self::Gradient { colors, phase } => {
                let n = colors.len();
                let multiplier = if size <= 1 {
                    0.0
                } else {
                    (n - 1) as f64 / (size - 1) as f64
                };
                let phase = phase * (n - 1) as f64;
                (0..size)
                    .map(|index| {
                        let position = index as f64 * multiplier + phase;
                        let low_unclamped = position.floor();
                        let low = (low_unclamped as usize) % n;
                        let high = (position.ceil() as usize) % n;
                        Rgb::lerp((position - low_unclamped) as f32, colors[low], colors[high])
                    })
                    .collect()
            }
            Self::Rainbow { reversed, phase } => {
                let divided = f64::from(*phase) / 10.0;
                (0..size)
                    .map(|i| {
                        let index = if *reversed { size - 1 - i } else { i };
                        let hue = ((f64::from(index as f32 / size as f32) + divided) % 1.0) as f32;
                        Rgb::hsv(hue, 1.0, 1.0)
                    })
                    .collect()
            }
        }
    }
}

fn render_root(nodes: Vec<Node>) -> TextComponent {
    let mut children = render_all(nodes);
    if children.len() == 1 {
        return children.pop().expect("one child");
    }
    let mut root = TextComponent::new("");
    root.extra = children;
    root
}

fn render_all(nodes: Vec<Node>) -> Vec<TextComponent> {
    nodes.into_iter().map(render).collect()
}

fn render(node: Node) -> TextComponent {
    match node {
        Node::Text(text) => TextComponent::new(text),
        Node::Insert(component) => component,
        Node::Wrap(wrap, children) => {
            let mut children = render_all(children);
            let mut out = if children.len() == 1 && !conflicts(&wrap, &children[0]) {
                children.pop().expect("one child")
            } else {
                let mut out = TextComponent::new("");
                out.extra = children;
                out
            };
            match wrap {
                Wrap::Color(color) => out.color = Some(color.into_boxed_str()),
                Wrap::Decoration(d, on) => {
                    let slot = match d {
                        Decoration::Bold => &mut out.bold,
                        Decoration::Italic => &mut out.italic,
                        Decoration::Underlined => &mut out.underlined,
                        Decoration::Strikethrough => &mut out.strikethrough,
                        Decoration::Obfuscated => &mut out.obfuscated,
                    };
                    *slot = Some(on);
                }
                Wrap::Click(event) => out.rare_mut().events.click = Some(event),
                Wrap::Hover(hover) => out.rare_mut().events.hover = Some(hover),
                Wrap::Insert(text) => out.rare_mut().events.insertion = Some(text),
                Wrap::Font(font) => out.rare_mut().font = Some(font.into_boxed_str()),
                Wrap::Shadow(color) => out.rare_mut().shadow_color = Some(color),
                Wrap::Changer(changer) => {
                    let size = count_chars(&out);
                    let colors = changer.colors(size);
                    let mut index = 0;
                    out = recolor(out, &colors, &mut index, false);
                }
            }
            out
        }
    }
}

/// Whether applying `wrap` directly to `child` would override a style the
/// child sets itself, so the child must be wrapped instead.
fn conflicts(wrap: &Wrap, child: &TextComponent) -> bool {
    let events = child.events();
    match wrap {
        Wrap::Color(_) => child.color.is_some(),
        Wrap::Decoration(d, _) => match d {
            Decoration::Bold => child.bold.is_some(),
            Decoration::Italic => child.italic.is_some(),
            Decoration::Underlined => child.underlined.is_some(),
            Decoration::Strikethrough => child.strikethrough.is_some(),
            Decoration::Obfuscated => child.obfuscated.is_some(),
        },
        Wrap::Click(_) => events.is_some_and(|e| e.click.is_some()),
        Wrap::Hover(_) => events.is_some_and(|e| e.hover.is_some()),
        Wrap::Insert(_) => events.is_some_and(|e| e.insertion.is_some()),
        Wrap::Font(_) => child.font().is_some(),
        Wrap::Shadow(_) => child.shadow_color().is_some(),
        Wrap::Changer(_) => false,
    }
}

fn count_chars(component: &TextComponent) -> usize {
    let own = if component.translate.is_none() && component.special_contents().is_none() {
        component.text.chars().count()
    } else {
        1
    };
    own + component.extra.iter().map(count_chars).sum::<usize>()
}

/// Splits the text of `component` and its children into runs, one colour
/// per character from `colors`, leaving text under an explicit colour as is
/// while still advancing through the colours.
fn recolor(
    mut component: TextComponent,
    colors: &[Rgb],
    index: &mut usize,
    colored: bool,
) -> TextComponent {
    let colored = colored || component.color.is_some();
    let own_text = component.translate.is_none() && component.special_contents().is_none();
    let text = if own_text {
        std::mem::take(&mut component.text)
    } else {
        String::new()
    };
    let extra = std::mem::take(&mut component.extra);
    let mut runs: Vec<TextComponent> = Vec::new();
    if !own_text {
        if !colored {
            let color = colors.get(*index).copied().unwrap_or(Rgb(0xFF, 0xFF, 0xFF));
            component.color = Some(color.hex().into_boxed_str());
        }
        *index += 1;
    }
    if colored {
        *index += text.chars().count();
        if !text.is_empty() {
            runs.push(TextComponent::new(text));
        }
    } else {
        let mut current: Option<(Rgb, String)> = None;
        for c in text.chars() {
            let color = colors.get(*index).copied().unwrap_or(Rgb(0xFF, 0xFF, 0xFF));
            *index += 1;
            match &mut current {
                Some((rgb, run)) if *rgb == color => run.push(c),
                _ => {
                    if let Some((rgb, run)) = current.take() {
                        runs.push(TextComponent::new(run).color(rgb.hex()));
                    }
                    current = Some((color, c.to_string()));
                }
            }
        }
        if let Some((rgb, run)) = current {
            runs.push(TextComponent::new(run).color(rgb.hex()));
        }
    }
    let mut children = runs;
    children.extend(
        extra
            .into_iter()
            .map(|child| recolor(child, colors, index, colored)),
    );
    if children.len() == 1 && component.is_plain() && component.text.is_empty() {
        return children.pop().expect("one child");
    }
    component.extra = children;
    component
}

const LEGACY_COLORS: [&str; 16] = [
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

/// One legacy formatting code.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Legacy {
    Color(String),
    Decoration(Decoration),
    Reset,
}

/// Reads a legacy code at the start of `rest` (just after the prefix
/// character): `0`-`f`, `k`-`o`, `r`, `#RRGGBB`, or `x` followed by six
/// prefixed hex digits. Returns the code and how many bytes it used.
fn legacy_code(rest: &str, prefix: char) -> Option<(Legacy, usize)> {
    let c = rest.chars().next()?.to_ascii_lowercase();
    if let Some(digit) = c.to_digit(16) {
        return Some((Legacy::Color(LEGACY_COLORS[digit as usize].to_owned()), 1));
    }
    let code = match c {
        'k' => Legacy::Decoration(Decoration::Obfuscated),
        'l' => Legacy::Decoration(Decoration::Bold),
        'm' => Legacy::Decoration(Decoration::Strikethrough),
        'n' => Legacy::Decoration(Decoration::Underlined),
        'o' => Legacy::Decoration(Decoration::Italic),
        'r' => Legacy::Reset,
        '#' => {
            let hex = rest.get(1..7)?;
            let rgb = Rgb::parse_hex(&format!("#{hex}"))?;
            return Some((Legacy::Color(rgb.hex()), 7));
        }
        'x' => {
            let mut hex = String::new();
            let mut chars = rest[1..].chars();
            for _ in 0..6 {
                if chars.next()? != prefix {
                    return None;
                }
                hex.push(chars.next()?);
            }
            let rgb = Rgb::parse_hex(&format!("#{hex}"))?;
            return Some((Legacy::Color(rgb.hex()), 1 + 6 * (prefix.len_utf8() + 1)));
        }
        _ => return None,
    };
    Some((code, 1))
}

/// Parses legacy formatting codes introduced by `prefix` (usually `&` or
/// the section sign). A colour code clears earlier decorations, as the
/// game does.
pub fn from_legacy(input: &str, prefix: char) -> TextComponent {
    let input = truncate(input);
    let mut runs: Vec<TextComponent> = Vec::new();
    let mut style = TextComponent::default();
    let mut text = String::new();
    let mut rest = input;
    let flush = |runs: &mut Vec<TextComponent>, style: &TextComponent, text: &mut String| {
        if !text.is_empty() {
            let mut run = style.clone();
            run.text = std::mem::take(text);
            runs.push(run);
        }
    };
    while let Some(c) = rest.chars().next() {
        if c == prefix
            && let Some((code, used)) = legacy_code(&rest[c.len_utf8()..], prefix)
        {
            flush(&mut runs, &style, &mut text);
            match code {
                Legacy::Color(color) => style = TextComponent::default().color(color),
                Legacy::Reset => style = TextComponent::default(),
                Legacy::Decoration(d) => {
                    let slot = match d {
                        Decoration::Bold => &mut style.bold,
                        Decoration::Italic => &mut style.italic,
                        Decoration::Underlined => &mut style.underlined,
                        Decoration::Strikethrough => &mut style.strikethrough,
                        Decoration::Obfuscated => &mut style.obfuscated,
                    };
                    *slot = Some(true);
                }
            }
            rest = &rest[c.len_utf8() + used..];
            continue;
        }
        text.push(c);
        rest = &rest[c.len_utf8()..];
    }
    flush(&mut runs, &style, &mut text);
    if runs.len() == 1 {
        return runs.pop().expect("one run");
    }
    let mut root = TextComponent::new("");
    root.extra = runs;
    root
}

/// Rewrites legacy `&` and section-sign codes as MiniMessage tags. A
/// colour or reset code becomes `<reset>` plus the colour, matching how a
/// legacy colour clears formatting.
pub fn legacy_to_minimessage(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(c) = rest.chars().next() {
        if matches!(c, '&' | '\u{a7}')
            && let Some((code, used)) = legacy_code(&rest[c.len_utf8()..], c)
        {
            match code {
                Legacy::Color(color) => {
                    out.push_str("<reset><");
                    out.push_str(&color);
                    out.push('>');
                }
                Legacy::Reset => out.push_str("<reset>"),
                Legacy::Decoration(d) => out.push_str(match d {
                    Decoration::Bold => "<bold>",
                    Decoration::Italic => "<italic>",
                    Decoration::Underlined => "<underlined>",
                    Decoration::Strikethrough => "<strikethrough>",
                    Decoration::Obfuscated => "<obfuscated>",
                }),
            }
            rest = &rest[c.len_utf8() + used..];
            continue;
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

#[cfg(test)]
mod conformance;
#[cfg(test)]
mod tests;
