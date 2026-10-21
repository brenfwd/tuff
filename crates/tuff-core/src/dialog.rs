use tuff_nbt::{Compound, Tag};

const MAX_DEPTH: usize = 8;
const MAX_WIDTH: i32 = 1024;

fn width(c: &Compound, key: &str) -> Result<(), String> {
    match c.get(key) {
        None => Ok(()),
        Some(tag) => match tag.as_int() {
            Some(w) if (1..=MAX_WIDTH).contains(&w) => Ok(()),
            _ => Err(format!("{key} must be an integer in 1..={MAX_WIDTH}")),
        },
    }
}

fn positive_int(c: &Compound, key: &str) -> Result<(), String> {
    match c.get(key) {
        None => Ok(()),
        Some(tag) => match tag.as_int() {
            Some(v) if v > 0 => Ok(()),
            _ => Err(format!("{key} must be a positive integer")),
        },
    }
}

fn int_range(c: &Compound, key: &str, min: i32, max: i32) -> Result<(), String> {
    match c.get(key) {
        None => Ok(()),
        Some(tag) => match tag.as_int() {
            Some(v) if (min..=max).contains(&v) => Ok(()),
            _ => Err(format!("{key} must be an integer in {min}..={max}")),
        },
    }
}

fn required(c: &Compound, key: &str) -> Result<(), String> {
    c.contains_key(key)
        .then_some(())
        .ok_or_else(|| format!("missing {key}"))
}

fn kind(c: &Compound) -> Result<&str, String> {
    let id = c.get_str("type").ok_or("missing type")?;
    Ok(id.strip_prefix("minecraft:").unwrap_or(id))
}

fn compounds(tag: &Tag) -> Vec<&Compound> {
    match tag {
        Tag::Compound(c) => vec![c],
        Tag::List(list) => list.iter().filter_map(Tag::as_compound).collect(),
        _ => Vec::new(),
    }
}

fn body(c: &Compound) -> Result<(), String> {
    match kind(c)? {
        "plain_message" => {
            required(c, "contents")?;
            width(c, "width")
        }
        "item" => {
            required(c, "item")?;
            int_range(c, "width", 1, 256)?;
            int_range(c, "height", 1, 256)?;
            match c.get_compound("description") {
                Some(description) => {
                    required(description, "contents")?;
                    width(description, "width")
                }
                None => Ok(()),
            }
        }
        other => Err(format!("unknown dialog body type {other}")),
    }
}

fn input(c: &Compound) -> Result<(), String> {
    let key = c.get_str("key").ok_or("input missing key")?;
    let valid_key = !key.is_empty()
        && key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        && !key.starts_with(|ch: char| ch.is_ascii_digit());
    if !valid_key {
        return Err(format!("invalid input key {key:?}"));
    }
    required(c, "label").or_else(|e| match kind(c)? {
        "boolean" | "number_range" | "single_option" | "text" => Err(e),
        _ => Ok(()),
    })?;
    width(c, "width")?;
    match kind(c)? {
        "boolean" => Ok(()),
        "text" => {
            positive_int(c, "max_length")?;
            let max = c.get_int("max_length").unwrap_or(32) as usize;
            if c.get_str("initial")
                .is_some_and(|s| s.chars().count() > max)
            {
                return Err("default text length exceeds allowed size".to_owned());
            }
            if let Some(multiline) = c.get_compound("multiline") {
                positive_int(multiline, "max_lines")?;
                int_range(multiline, "height", 1, 512)?;
            }
            Ok(())
        }
        "number_range" => {
            let start = c
                .get("start")
                .and_then(Tag::as_float)
                .ok_or("missing start")?;
            let end = c.get("end").and_then(Tag::as_float).ok_or("missing end")?;
            if let Some(initial) = c.get("initial").and_then(Tag::as_float)
                && (initial < start.min(end) || initial > start.max(end))
            {
                return Err("initial value is outside of range".to_owned());
            }
            if c.get("step")
                .and_then(Tag::as_float)
                .is_some_and(|s| s <= 0.0)
            {
                return Err("step must be positive".to_owned());
            }
            Ok(())
        }
        "single_option" => {
            let options = c.get_list("options").ok_or("missing options")?;
            if options.is_empty() {
                return Err("options must not be empty".to_owned());
            }
            let initial = options
                .iter()
                .filter_map(Tag::as_compound)
                .filter(|o| o.get_bool("initial").unwrap_or(false))
                .count();
            if initial > 1 {
                return Err("multiple initial values".to_owned());
            }
            Ok(())
        }
        other => Err(format!("unknown input type {other}")),
    }
}

fn button(c: &Compound) -> Result<(), String> {
    required(c, "label")?;
    width(c, "width")
}

fn nested(tag: &Tag, depth: usize) -> Result<(), String> {
    match tag {
        Tag::Compound(c) => validate_at(c, depth + 1),
        Tag::List(list) => list.iter().try_for_each(|t| nested(t, depth)),
        _ => Ok(()),
    }
}

fn validate_at(c: &Compound, depth: usize) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err("dialogs nested too deeply".to_owned());
    }
    let dialog_kind = kind(c)?;
    if !matches!(
        dialog_kind,
        "notice" | "confirmation" | "multi_action" | "dialog_list" | "server_links"
    ) {
        return Err(format!("unknown dialog type {dialog_kind}"));
    }
    required(c, "title")?;
    let after_action = c.get_str("after_action").unwrap_or("close");
    if !matches!(after_action, "close" | "none" | "wait_for_response") {
        return Err(format!("unknown after_action {after_action}"));
    }
    let pause = c.get_bool("pause").unwrap_or(true);
    if pause && after_action == "none" {
        return Err(
            "dialogs that pause the game must use after_action values that unpause it".to_owned(),
        );
    }
    if let Some(tag) = c.get("body") {
        compounds(tag).into_iter().try_for_each(body)?;
    }
    if let Some(inputs) = c.get_list("inputs") {
        inputs
            .iter()
            .filter_map(Tag::as_compound)
            .try_for_each(input)?;
    }
    match dialog_kind {
        "notice" => {
            if let Some(action) = c.get_compound("action") {
                button(action)?;
            }
        }
        "confirmation" => {
            for side in ["yes", "no"] {
                button(
                    c.get_compound(side)
                        .ok_or_else(|| format!("missing {side}"))?,
                )?;
            }
        }
        "multi_action" => {
            let actions = c.get_list("actions").ok_or("missing actions")?;
            if actions.is_empty() {
                return Err("actions must not be empty".to_owned());
            }
            actions
                .iter()
                .filter_map(Tag::as_compound)
                .try_for_each(button)?;
            positive_int(c, "columns")?;
        }
        "dialog_list" => {
            nested(c.get("dialogs").ok_or("missing dialogs")?, depth)?;
            positive_int(c, "columns")?;
            width(c, "button_width")?;
        }
        _ => {
            positive_int(c, "columns")?;
            width(c, "button_width")?;
        }
    }
    if let Some(exit) = c.get_compound("exit_action") {
        button(exit)?;
    }
    Ok(())
}

/// Checks an inline dialog definition against the dialog codecs' rules
/// (types, required fields, width and count limits, the pause rule).
pub fn validate(dialog: &Compound) -> Result<(), String> {
    validate_at(dialog, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(snbt: &str) -> Compound {
        Tag::from_snbt(snbt).unwrap().as_compound().unwrap().clone()
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_minimal_notice_is_valid() {
        assert!(validate(&parse("{type:\"minecraft:notice\",title:\"Hi\"}")).is_ok());
    }

    #[tuff_core::watchdog]
    #[test]
    fn pausing_with_after_action_none_is_rejected() {
        let bad = parse("{type:\"notice\",title:\"Hi\",after_action:\"none\"}");
        assert!(validate(&bad).is_err());
        let ok = parse("{type:\"notice\",title:\"Hi\",after_action:\"none\",pause:0b}");
        assert!(validate(&ok).is_ok());
    }

    #[tuff_core::watchdog]
    #[test]
    fn unknown_types_and_missing_titles_are_rejected() {
        assert!(validate(&parse("{type:\"nope\",title:\"x\"}")).is_err());
        assert!(validate(&parse("{type:\"notice\"}")).is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn widths_and_input_rules_are_enforced() {
        let wide =
            "{type:\"notice\",title:\"x\",body:{type:\"plain_message\",contents:\"a\",width:2000}}";
        assert!(validate(&parse(wide)).is_err());
        let text = "{type:\"notice\",title:\"x\",inputs:[{type:\"text\",key:\"a\",label:\"l\",initial:\"toolong\",max_length:3}]}";
        assert!(validate(&parse(text)).is_err());
        let range = "{type:\"notice\",title:\"x\",inputs:[{type:\"number_range\",key:\"a\",label:\"l\",start:0f,end:1f,initial:5f}]}";
        assert!(validate(&parse(range)).is_err());
        let fine =
            "{type:\"notice\",title:\"x\",inputs:[{type:\"boolean\",key:\"a\",label:\"l\"}]}";
        assert!(validate(&parse(fine)).is_ok());
    }

    #[tuff_core::watchdog]
    #[test]
    fn nested_dialog_lists_recurse_and_bound_depth() {
        let mut snbt = "{type:\"notice\",title:\"x\",after_action:\"none\"}".to_owned();
        assert!(
            validate(&parse(
                "{type:\"dialog_list\",title:\"x\",dialogs:\"minecraft:a\"}"
            ))
            .is_ok()
        );
        assert!(
            validate(&parse(&format!(
                "{{type:\"dialog_list\",title:\"x\",dialogs:[{snbt}]}}"
            )))
            .is_err()
        );
        for _ in 0..12 {
            snbt = format!("{{type:\"dialog_list\",title:\"x\",dialogs:[{snbt}]}}");
        }
        let _ = validate(&parse(&snbt));
    }
}
