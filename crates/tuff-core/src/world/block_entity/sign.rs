//! Sign text, kept in the sign block entity's saved NBT: which side a
//! player faces, what may be edited, and the changes dyes, ink sacs,
//! honeycomb and the sign editor make.

use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::registry::BlockEntityType;
use tuff_vanilla_data::{BlockKind, BlockState};

use super::raw;
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;

pub const LINES: usize = 4;
/// The longest line, in UTF-16 units, the sign editor sends.
pub const MAX_LINE_LENGTH: usize = 384;
const IS_WAXED: &str = "is_waxed";
const MESSAGES: &str = "messages";
const FILTERED_MESSAGES: &str = "filtered_messages";
const STYLE_KEYS: [&str; 11] = [
    "color",
    "shadow_color",
    "font",
    "bold",
    "italic",
    "underlined",
    "strikethrough",
    "obfuscated",
    "insertion",
    "click_event",
    "hover_event",
];
const CLICK_ACTIONS: [&str; 3] = ["run_command", "show_dialog", "custom"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Front,
    Back,
}

impl Side {
    fn key(self) -> &'static str {
        match self {
            Self::Front => "front_text",
            Self::Back => "back_text",
        }
    }
}

pub fn is_sign(kind: BlockEntityType) -> bool {
    matches!(kind, BlockEntityType::Sign | BlockEntityType::HangingSign)
}

/// The angle the sign's front faces, in degrees.
fn y_rotation(state: BlockState) -> f32 {
    match state.block().kind() {
        BlockKind::WallSign | BlockKind::WallHangingSign => match state.facing() {
            Some(Facing::West) => 90.0,
            Some(Facing::North) => 180.0,
            Some(Facing::East) => 270.0,
            _ => 0.0,
        },
        _ => {
            let segment: f32 = state
                .get("rotation")
                .and_then(|r| r.parse().ok())
                .unwrap_or(0.0);
            segment * 22.5
        }
    }
}

/// The point of the sign whose direction from the player decides the side
/// it faces: the middle of a wall sign's board, else the block's centre.
fn center(state: BlockState) -> [f64; 3] {
    match (state.block().kind(), state.outline_boxes()) {
        (BlockKind::WallSign, [first, rest @ ..]) => {
            let mut bounds = *first;
            for b in rest {
                for axis in 0..3 {
                    bounds[axis] = bounds[axis].min(b[axis]);
                    bounds[axis + 3] = bounds[axis + 3].max(b[axis + 3]);
                }
            }
            [0, 1, 2].map(|axis| (bounds[axis] as f64 + bounds[axis + 3] as f64) / 2.0)
        }
        _ => [0.5; 3],
    }
}

fn wrap_degrees(degrees: f32) -> f32 {
    let mut wrapped = degrees % 360.0;
    if wrapped >= 180.0 {
        wrapped -= 360.0;
    }
    if wrapped < -180.0 {
        wrapped += 360.0;
    }
    wrapped
}

/// The side of the sign at `pos` a player standing at `position` faces.
pub fn side_facing(state: BlockState, pos: BlockPos, position: [f64; 3]) -> Side {
    let center = center(state);
    let dx = position[0] - (pos.x as f64 + center[0]);
    let dz = position[2] - (pos.z as f64 + center[2]);
    let player = (dz.atan2(dx) * 57.295_776_367_187_5) as f32 - 90.0;
    if wrap_degrees(player - y_rotation(state)).abs() <= 90.0 {
        Side::Front
    } else {
        Side::Back
    }
}

pub fn is_waxed(nbt: &Compound) -> bool {
    nbt.get_bool(IS_WAXED).unwrap_or(false)
}

/// Waxes the sign; false if it already was.
pub fn wax(nbt: &mut Compound) -> bool {
    if is_waxed(nbt) {
        return false;
    }
    nbt.insert(IS_WAXED, true);
    true
}

/// One side's text, set to the blank default if it is missing.
fn text_mut(kind: BlockEntityType, nbt: &mut Compound, side: Side) -> &mut Compound {
    if nbt.get_compound(side.key()).is_none() {
        let blank = raw::new(kind, BlockPos::default())
            .get(side.key())
            .cloned()
            .unwrap_or_else(|| Tag::Compound(Compound::new()));
        nbt.insert(side.key(), blank);
    }
    nbt.get_compound_mut(side.key())
        .expect("inserted above if missing")
}

fn messages(nbt: &Compound, side: Side) -> Vec<Tag> {
    nbt.get_compound(side.key())
        .and_then(|text| text.get_list(MESSAGES))
        .map(<[Tag]>::to_vec)
        .unwrap_or_default()
}

fn plain_text(tag: &Tag, out: &mut String) {
    match tag {
        Tag::String(text) => out.push_str(text),
        Tag::List(parts) => {
            for part in parts {
                plain_text(part, out);
            }
        }
        Tag::Compound(component) => {
            if let Some(text) = component.get_str("text") {
                out.push_str(text);
            } else if let Some(text) = component
                .get_str("fallback")
                .or_else(|| component.get_str("translate"))
            {
                out.push_str(text);
            }
            if let Some(Tag::List(extra)) = component.get("extra") {
                for part in extra {
                    plain_text(part, out);
                }
            }
        }
        _ => {}
    }
}

/// Whether any line of the side shows text.
pub fn has_message(nbt: &Compound, side: Side) -> bool {
    messages(nbt, side).iter().any(|line| {
        let mut text = String::new();
        plain_text(line, &mut text);
        !text.is_empty()
    })
}

fn is_plain_text(line: &Tag) -> bool {
    match line {
        Tag::String(_) => true,
        Tag::List(parts) => parts.first().is_some_and(is_plain_text),
        Tag::Compound(component) => match component.get_str("type") {
            Some(kind) => kind == "text",
            None => component.contains_key("text"),
        },
        _ => false,
    }
}

/// Whether the editor may change the side: every line is plain text.
pub fn is_editable(nbt: &Compound, side: Side) -> bool {
    messages(nbt, side).iter().all(is_plain_text)
}

fn line_style(line: &Tag) -> Option<&Compound> {
    match line {
        Tag::Compound(component) => Some(component),
        Tag::List(parts) => parts.first().and_then(line_style),
        _ => None,
    }
}

/// Whether a line of the side has a click action that runs something.
pub fn has_click_action(nbt: &Compound, side: Side) -> bool {
    messages(nbt, side).iter().any(|line| {
        line_style(line)
            .and_then(|style| style.get_compound("click_event"))
            .and_then(|event| event.get_str("action"))
            .is_some_and(|action| CLICK_ACTIONS.contains(&action))
    })
}

/// The `run_command` click event commands of the side's lines, in order.
pub fn click_commands(nbt: &Compound, side: Side) -> Vec<String> {
    messages(nbt, side)
        .iter()
        .filter_map(|line| {
            let event = line_style(line)?.get_compound("click_event")?;
            (event.get_str("action")? == "run_command")
                .then(|| event.get_str("command").map(str::to_owned))
                .flatten()
        })
        .collect()
}

/// A dialog a `show_dialog` click event opens.
#[derive(Debug, Clone, PartialEq)]
pub enum ClickDialog {
    Inline(Compound),
    Registry(String),
}

/// The dialogs of the side's `show_dialog` click events, in order.
pub fn click_dialogs(nbt: &Compound, side: Side) -> Vec<ClickDialog> {
    messages(nbt, side)
        .iter()
        .filter_map(|line| {
            let event = line_style(line)?.get_compound("click_event")?;
            if event.get_str("action")? != "show_dialog" {
                return None;
            }
            if let Some(id) = event.get_str("dialog") {
                return Some(ClickDialog::Registry(id.to_owned()));
            }
            event
                .get_compound("dialog")
                .cloned()
                .map(ClickDialog::Inline)
        })
        .collect()
}

/// Sets the side's text color (a dye color name); false if it already
/// had it.
pub fn set_color(kind: BlockEntityType, nbt: &mut Compound, side: Side, color: &str) -> bool {
    let text = text_mut(kind, nbt, side);
    if text.get_str("color").unwrap_or("black") == color {
        return false;
    }
    text.insert("color", color);
    true
}

/// Makes the side's text glow or not; false if it already did.
pub fn set_glowing(kind: BlockEntityType, nbt: &mut Compound, side: Side, glowing: bool) -> bool {
    let text = text_mut(kind, nbt, side);
    if text.get_bool("has_glowing_text").unwrap_or(false) == glowing {
        return false;
    }
    text.insert("has_glowing_text", glowing);
    true
}

/// Removes the game's formatting codes (a section sign and the character
/// after it) from an edited line.
pub fn strip_formatting(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{a7}'
            && chars.peek().is_some_and(
                |code| matches!(code.to_ascii_lowercase(), '0'..='9' | 'a'..='f' | 'k'..='o' | 'r'),
            )
        {
            chars.next();
            continue;
        }
        out.push(c);
    }
    out
}

/// Replaces the side's lines with what the editor sent, keeping each
/// line's style.
pub fn set_lines(kind: BlockEntityType, nbt: &mut Compound, side: Side, lines: &[String]) {
    let text = text_mut(kind, nbt, side);
    let old = text
        .get_list(MESSAGES)
        .map(<[Tag]>::to_vec)
        .unwrap_or_default();
    let mut messages: Vec<Tag> = (0..LINES)
        .map(|i| {
            old.get(i)
                .cloned()
                .unwrap_or_else(|| Tag::String(String::new()))
        })
        .collect();
    for (i, line) in lines.iter().take(LINES).enumerate() {
        let mut edited = Compound::new().with("text", line.as_str());
        if let Some(style) = line_style(&messages[i]) {
            for key in STYLE_KEYS {
                if let Some(value) = style.get(key) {
                    edited.insert(key, value.clone());
                }
            }
        }
        messages[i] = if edited.len() == 1 {
            Tag::String(line.clone())
        } else {
            Tag::Compound(edited)
        };
    }
    text.insert(MESSAGES, Tag::List(messages));
    text.remove(FILTERED_MESSAGES);
}

/// Stores what the text filter let through, kept only when it differs from
/// the lines themselves.
pub fn set_filtered_lines(
    kind: BlockEntityType,
    nbt: &mut Compound,
    side: Side,
    filtered: &[String],
) {
    let text = text_mut(kind, nbt, side);
    let raw: Vec<String> = text
        .get_list(MESSAGES)
        .map(|list| list.iter().map(plain_line).collect())
        .unwrap_or_default();
    let differs = (0..LINES).any(|i| {
        filtered.get(i).map(String::as_str).unwrap_or_default()
            != raw.get(i).map(String::as_str).unwrap_or_default()
    });
    if !differs {
        return;
    }
    let lines = (0..LINES)
        .map(|i| Tag::String(filtered.get(i).cloned().unwrap_or_default()))
        .collect();
    text.insert(FILTERED_MESSAGES, Tag::List(lines));
}

fn plain_line(tag: &Tag) -> String {
    match tag {
        Tag::String(s) => s.clone(),
        Tag::Compound(c) => c.get_str("text").unwrap_or_default().to_owned(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_vanilla_data::Block;

    fn sign() -> Compound {
        raw::new(BlockEntityType::Sign, BlockPos::new(0, 64, 0))
    }

    #[tuff_core::watchdog]
    #[test]
    fn registry_dialogs_come_from_string_references() {
        let event = Compound::new()
            .with("action", "show_dialog")
            .with("dialog", "minecraft:server_links");
        let line = Compound::new().with("click_event", Tag::Compound(event));
        let mut nbt = sign();
        let mut text = Compound::new();
        text.insert(MESSAGES, Tag::List(vec![Tag::Compound(line)]));
        nbt.insert("front_text", Tag::Compound(text));
        assert_eq!(
            click_dialogs(&nbt, Side::Front),
            vec![ClickDialog::Registry("minecraft:server_links".to_owned())]
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn inline_dialogs_come_from_show_dialog_events() {
        let dialog = Compound::new().with("type", "minecraft:notice");
        let event = Compound::new()
            .with("action", "show_dialog")
            .with("dialog", Tag::Compound(dialog.clone()));
        let line = Compound::new()
            .with("text", "hi")
            .with("click_event", Tag::Compound(event));
        let mut nbt = sign();
        let mut text = Compound::new();
        text.insert(
            MESSAGES,
            Tag::List(vec![Tag::Compound(line), Tag::String(String::new())]),
        );
        nbt.insert("front_text", Tag::Compound(text));
        assert_eq!(
            click_dialogs(&nbt, Side::Front),
            vec![ClickDialog::Inline(dialog)]
        );
        assert!(click_dialogs(&nbt, Side::Back).is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn players_face_the_front_from_where_the_sign_points() {
        let pos = BlockPos::new(0, 64, 0);
        let south = Block::OakSign
            .default_state()
            .with("rotation", "0")
            .unwrap();
        assert_eq!(side_facing(south, pos, [0.5, 64.0, 3.0]), Side::Front);
        assert_eq!(side_facing(south, pos, [0.5, 64.0, -3.0]), Side::Back);
        let east = south.with("rotation", "12").unwrap();
        assert_eq!(side_facing(east, pos, [3.0, 64.0, 0.5]), Side::Front);
        let wall = Block::OakWallSign
            .default_state()
            .with("facing", "north")
            .unwrap();
        assert_eq!(side_facing(wall, pos, [0.5, 64.0, -2.0]), Side::Front);
        assert_eq!(side_facing(wall, pos, [0.5, 64.0, 2.0]), Side::Back);
    }

    #[tuff_core::watchdog]
    #[test]
    fn new_signs_are_blank_editable_and_unwaxed() {
        let mut nbt = sign();
        assert!(!is_waxed(&nbt));
        assert!(!has_message(&nbt, Side::Front));
        assert!(is_editable(&nbt, Side::Front));
        assert!(wax(&mut nbt));
        assert!(!wax(&mut nbt));
        assert!(is_waxed(&nbt));
    }

    #[tuff_core::watchdog]
    #[test]
    fn edits_keep_line_styles_and_dyes_change_colors_once() {
        let kind = BlockEntityType::Sign;
        let mut nbt = sign();
        let styled = Compound::new().with("text", "old").with("color", "red");
        nbt.get_compound_mut("front_text")
            .unwrap()
            .get_list_mut(MESSAGES)
            .unwrap()[1] = Tag::Compound(styled);
        let lines = ["hi", "there", "", "x"].map(String::from);
        set_lines(kind, &mut nbt, Side::Front, &lines);
        let messages = messages(&nbt, Side::Front);
        assert_eq!(messages[0], Tag::from("hi"));
        assert_eq!(
            messages[1],
            Tag::Compound(Compound::new().with("text", "there").with("color", "red"))
        );
        assert!(has_message(&nbt, Side::Front));
        assert!(!has_message(&nbt, Side::Back));
        assert!(set_color(kind, &mut nbt, Side::Front, "blue"));
        assert!(!set_color(kind, &mut nbt, Side::Front, "blue"));
        assert!(!set_color(kind, &mut nbt, Side::Back, "black"));
        assert!(set_glowing(kind, &mut nbt, Side::Back, true));
        assert!(!set_glowing(kind, &mut nbt, Side::Back, true));
    }

    #[tuff_core::watchdog]
    #[test]
    fn translated_lines_are_not_editable_and_click_actions_are_found() {
        let mut nbt = sign();
        let front = nbt.get_compound_mut("front_text").unwrap();
        front.get_list_mut(MESSAGES).unwrap()[0] =
            Tag::Compound(Compound::new().with("translate", "block.minecraft.stone"));
        assert!(!is_editable(&nbt, Side::Front));
        assert!(has_message(&nbt, Side::Front));
        let mut nbt = sign();
        let front = nbt.get_compound_mut("front_text").unwrap();
        front.get_list_mut(MESSAGES).unwrap()[2] = Tag::Compound(
            Compound::new().with("text", "go").with(
                "click_event",
                Compound::new()
                    .with("action", "run_command")
                    .with("command", "/say hi"),
            ),
        );
        assert!(is_editable(&nbt, Side::Front));
        assert!(has_click_action(&nbt, Side::Front));
        assert!(!has_click_action(&nbt, Side::Back));
    }

    #[tuff_core::watchdog]
    #[test]
    fn formatting_codes_are_stripped() {
        assert_eq!(
            strip_formatting("\u{a7}cRed \u{a7}Lbold\u{a7}"),
            "Red bold\u{a7}"
        );
        assert_eq!(strip_formatting("\u{a7}zkeep"), "\u{a7}zkeep");
    }

    #[tuff_core::watchdog]
    #[test]
    fn filtered_messages_reach_clients_and_edits_clear_them() {
        let kind = BlockEntityType::Sign;
        let mut nbt = sign();
        let filtered: Vec<Tag> = ["***", "", "", ""].map(Tag::from).into();
        nbt.get_compound_mut("front_text")
            .unwrap()
            .insert(FILTERED_MESSAGES, Tag::List(filtered.clone()));
        let (_, _, sent) = raw::client_data(&nbt).unwrap();
        let front = sent.get_compound("front_text").unwrap();
        assert_eq!(front.get_list(FILTERED_MESSAGES), Some(filtered.as_slice()));
        set_lines(kind, &mut nbt, Side::Front, &["hi".to_string()]);
        let front = nbt.get_compound("front_text").unwrap();
        assert!(front.get(FILTERED_MESSAGES).is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn filtered_lines_are_stored_only_when_they_differ() {
        let kind = BlockEntityType::Sign;
        let mut nbt = sign();
        let lines: Vec<String> = ["bad word", "ok", "", ""].map(String::from).into();
        set_lines(kind, &mut nbt, Side::Front, &lines);
        set_filtered_lines(kind, &mut nbt, Side::Front, &lines);
        assert!(
            text_mut(kind, &mut nbt, Side::Front)
                .get(FILTERED_MESSAGES)
                .is_none()
        );
        let seen: Vec<String> = ["#### ####", "ok", "", ""].map(String::from).into();
        set_filtered_lines(kind, &mut nbt, Side::Front, &seen);
        let stored = text_mut(kind, &mut nbt, Side::Front)
            .get_list(FILTERED_MESSAGES)
            .map(<[Tag]>::to_vec)
            .unwrap();
        assert_eq!(stored[0], Tag::String("#### ####".into()));
        assert_eq!(stored.len(), 4);
    }
}
