//! Command and chat filters plugins declare while registering.

use crate::wit::bindings::types as wit;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineAction {
    Cancel(Option<String>),
    Rewrite(String),
}

impl LineAction {
    pub fn is_rewrite(&self) -> bool {
        matches!(self, Self::Rewrite(_))
    }
}

const PRIVILEGED_ROOTS: &[&str] = &[
    "tuff",
    "op",
    "deop",
    "stop",
    "reload",
    "whitelist",
    "ban",
    "ban-ip",
    "pardon",
    "pardon-ip",
    "banlist",
    "save-all",
    "save-on",
    "save-off",
    "setidletimeout",
    "perf",
    "debug",
    "transfer",
];

/// Whether a rewrite target names a command tree a plugin must never
/// reach by rewriting (grant management and server administration).
pub fn is_privileged_target(line: &str) -> bool {
    let root = line
        .trim_start()
        .trim_start_matches('/')
        .split_whitespace()
        .next()
        .unwrap_or("");
    let root = root.strip_prefix("minecraft:").unwrap_or(root);
    PRIVILEGED_ROOTS
        .iter()
        .any(|r| root.eq_ignore_ascii_case(r))
}

impl From<wit::LineAction> for LineAction {
    fn from(action: wit::LineAction) -> Self {
        match action {
            wit::LineAction::Cancel(notice) => Self::Cancel(notice),
            wit::LineAction::Rewrite(text) => Self::Rewrite(text),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandFilter {
    pub prefix: String,
    pub action: LineAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatFilter {
    pub prefix: Option<String>,
    pub contains: Option<String>,
    pub action: LineAction,
}

/// What the first matching filter does to a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    Cancel(Option<String>),
    Replace(String),
}

fn starts_with_ignore_case(text: &str, prefix: &str) -> bool {
    text.get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

pub fn command_outcome(filters: &[CommandFilter], line: &str) -> Outcome {
    let line = line.trim_start();
    for filter in filters {
        let prefix = filter.prefix.trim();
        if prefix.is_empty() || !starts_with_ignore_case(line, prefix) {
            continue;
        }
        let rest = &line[prefix.len()..];
        if !rest.is_empty() && !rest.starts_with(' ') {
            continue;
        }
        return match &filter.action {
            LineAction::Cancel(notice) => Outcome::Cancel(notice.clone()),
            LineAction::Rewrite(with) => {
                let replaced = format!("{with}{rest}");
                if is_privileged_target(&replaced) {
                    Outcome::Cancel(Some("A plugin may not rewrite into that command.".into()))
                } else {
                    Outcome::Replace(replaced)
                }
            }
        };
    }
    Outcome::Pass
}

pub fn chat_outcome(filters: &[ChatFilter], message: &str) -> Outcome {
    let lower = message.to_ascii_lowercase();
    for filter in filters {
        let prefix_ok = filter
            .prefix
            .as_deref()
            .is_none_or(|prefix| starts_with_ignore_case(message, prefix));
        let contains_ok = filter
            .contains
            .as_deref()
            .is_none_or(|needle| lower.contains(&needle.to_ascii_lowercase()));
        if !(prefix_ok && contains_ok) {
            continue;
        }
        return match &filter.action {
            LineAction::Cancel(notice) => Outcome::Cancel(notice.clone()),
            LineAction::Rewrite(with) => Outcome::Replace(with.clone()),
        };
    }
    Outcome::Pass
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(prefix: &str, action: LineAction) -> CommandFilter {
        CommandFilter {
            prefix: prefix.into(),
            action,
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn command_prefix_matches_on_word_borders_and_rewrites() {
        let filters = [
            command("gamemode", LineAction::Cancel(Some("no".into()))),
            command("tp", LineAction::Rewrite("teleport".into())),
        ];
        assert_eq!(
            command_outcome(&filters, "GameMode creative"),
            Outcome::Cancel(Some("no".into()))
        );
        assert_eq!(
            command_outcome(&filters, "tp a b"),
            Outcome::Replace("teleport a b".into())
        );
        assert_eq!(command_outcome(&filters, "tpall"), Outcome::Pass);
        assert_eq!(command_outcome(&filters, "say hi"), Outcome::Pass);
    }

    #[tuff_core::watchdog]
    #[test]
    fn rewrites_into_privileged_commands_are_refused() {
        for target in ["tuff plugin grant x", "/op", "minecraft:stop", "TUFF"] {
            let filters = [command("list", LineAction::Rewrite(target.into()))];
            assert!(matches!(
                command_outcome(&filters, "list"),
                Outcome::Cancel(Some(_))
            ));
        }
        let filters = [command("list", LineAction::Rewrite("who".into()))];
        assert_eq!(
            command_outcome(&filters, "list"),
            Outcome::Replace("who".into())
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn chat_filters_need_every_condition() {
        let filters = [ChatFilter {
            prefix: Some("!".into()),
            contains: Some("SPAM".into()),
            action: LineAction::Cancel(None),
        }];
        assert_eq!(chat_outcome(&filters, "!buy spam"), Outcome::Cancel(None));
        assert_eq!(chat_outcome(&filters, "buy spam"), Outcome::Pass);
        assert_eq!(chat_outcome(&filters, "!hello"), Outcome::Pass);
    }
}
